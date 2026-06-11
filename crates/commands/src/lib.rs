use std::sync::{Arc, Mutex};
use std::time::Duration;

use perms::Permissions;
use player::{LoopMode, PlayerHandle, Snapshot};
use playlist::{Playlist, PlaylistItem, Store};
use tokio::sync::mpsc;
use ts_connection::ChatMessage;

/// 解析后的指令。Volume/Loop/Remove 携带原始参数，由 run 校验并给出回复。
pub enum Command {
    Play(Vec<String>),
    Skip,
    Stop,
    Pause,
    Resume,
    NowPlaying,
    Queue,
    Clear,
    Shuffle,
    Help,
    Whoami,
    Volume(Option<String>),
    Loop(Option<String>),
    Remove(Option<String>),
    Playlist(Vec<String>),
}

const HELP_TEXT: &str = "可用命令：\n\
!play <url/路径> [更多…] 点播(支持空格批量)\n\
!pause / !resume 暂停/继续\n\
!skip 跳过  !stop 停止并清空\n\
!volume [0-100] 音量  !loop [off|track|queue] 循环\n\
!nowplaying 当前曲目  !queue 队列\n\
!remove <编号> 移除待播  !clear 清空待播  !shuffle 打乱\n\
!help 帮助\n\
!playlist <save|create|add|remove|list|view|play|delete> 歌单\n\
!whoami 查看你的 uid";

/// 去掉 TeamSpeak 自动给 URL 套的 BBCode 包裹（[URL]X[/URL] / [URL=X]label[/URL]）。
fn strip_url_bbcode(s: &str) -> String {
    let t = s.trim();
    let lower = t.to_ascii_lowercase();
    if !lower.starts_with("[url") || !lower.ends_with("[/url]") {
        return t.to_string();
    }
    let Some(open_end) = t.find(']') else {
        return t.to_string();
    };
    let open_tag = &t[..open_end];
    if let Some(eq) = open_tag.find('=') {
        return open_tag[eq + 1..].trim().to_string();
    }
    t[open_end + 1..t.len() - 6].trim().to_string()
}

fn opt(arg: &str) -> Option<String> {
    if arg.is_empty() { None } else { Some(arg.to_string()) }
}

/// 解析一行文本；非指令返回 None。
pub fn parse(text: &str) -> Option<Command> {
    let rest = text.trim().strip_prefix('!')?;
    let mut it = rest.splitn(2, char::is_whitespace);
    let cmd = it.next()?.to_ascii_lowercase();
    let arg = it.next().map(str::trim).unwrap_or("");
    match cmd.as_str() {
        "play" if !arg.is_empty() => {
            let items: Vec<String> = arg.split_whitespace().map(strip_url_bbcode).collect();
            if items.is_empty() { None } else { Some(Command::Play(items)) }
        }
        "skip" => Some(Command::Skip),
        "stop" => Some(Command::Stop),
        "pause" => Some(Command::Pause),
        "resume" => Some(Command::Resume),
        "nowplaying" => Some(Command::NowPlaying),
        "queue" => Some(Command::Queue),
        "clear" => Some(Command::Clear),
        "shuffle" => Some(Command::Shuffle),
        "help" => Some(Command::Help),
        "whoami" => Some(Command::Whoami),
        "volume" => Some(Command::Volume(opt(arg))),
        "loop" => Some(Command::Loop(opt(arg))),
        "remove" => Some(Command::Remove(opt(arg))),
        "playlist" => Some(Command::Playlist(arg.split_whitespace().map(|s| s.to_string()).collect())),
        _ => None,
    }
}

/// 指令 → 权限键名。
fn command_name(cmd: &Command) -> &'static str {
    match cmd {
        Command::Play(_) => "play",
        Command::Skip => "skip",
        Command::Stop => "stop",
        Command::Pause => "pause",
        Command::Resume => "resume",
        Command::NowPlaying => "nowplaying",
        Command::Queue => "queue",
        Command::Clear => "clear",
        Command::Shuffle => "shuffle",
        Command::Help => "help",
        Command::Whoami => "whoami",
        Command::Volume(_) => "volume",
        Command::Loop(_) => "loop",
        Command::Remove(_) => "remove",
        Command::Playlist(_) => "playlist",
    }
}

/// Duration → "m:ss"。
fn fmt_dur(d: Duration) -> String {
    let s = d.as_secs();
    // 用全角冒号，避免 TeamSpeak 把 ":0X" 等渲染成表情符号
    format!("{}：{:02}", s / 60, s % 60)
}

/// 可选时长：None 显示 LIVE。
fn fmt_opt_dur(d: Option<Duration>) -> String {
    match d {
        Some(d) => fmt_dur(d),
        None => "LIVE".to_string(),
    }
}

fn loop_name(m: LoopMode) -> &'static str {
    match m {
        LoopMode::Off => "关",
        LoopMode::Track => "单曲",
        LoopMode::Queue => "队列",
    }
}

/// `!nowplaying` 回复。
pub fn format_nowplaying(s: &Snapshot) -> String {
    match &s.now_playing {
        None => "当前没有播放".to_string(),
        Some(np) => {
            let prog = match np.duration {
                Some(d) => format!("{} / {}", fmt_dur(np.elapsed), fmt_dur(d)),
                None => format!("{} / LIVE", fmt_dur(np.elapsed)),
            };
            let pause = if s.paused { " [已暂停]" } else { "" };
            format!(
                "[b]{}[/b]  {}  ♪音量{} 循环{}{}",
                np.title, prog, s.volume, loop_name(s.loop_mode), pause
            )
        }
    }
}

/// `!queue` 回复。
pub fn format_queue(s: &Snapshot) -> String {
    let mut out = match &s.now_playing {
        Some(np) => format!("▶ [b]{}[/b] ({})", np.title, fmt_opt_dur(np.duration)),
        None => "当前没有播放".to_string(),
    };
    if s.upcoming.is_empty() {
        out.push_str("\n队列为空");
    } else {
        for (i, q) in s.upcoming.iter().enumerate() {
            out.push_str(&format!("\n{}. {} ({})", i + 1, q.title, fmt_opt_dur(q.duration)));
        }
    }
    out
}

/// 指令处理循环：读 chat_rx → parse → 执行 → 经 reply_tx 回复。不接触 con。
pub async fn run(
    mut chat_rx: mpsc::Receiver<ChatMessage>,
    handle: PlayerHandle,
    snapshot: Arc<Mutex<Snapshot>>,
    store: Store,
    perms: Permissions,
    reply_tx: mpsc::Sender<String>,
) {
    while let Some(msg) = chat_rx.recv().await {
        let Some(cmd) = parse(&msg.text) else { continue };
        let name = command_name(&cmd);
        if name != "help" && name != "whoami" && !perms.allows(&msg.invoker_uid, name) {
            let _ = reply_tx
                .send(format!("⛔ 无权限：{name}（你的角色：{}）", perms.role_of(&msg.invoker_uid)))
                .await;
            continue;
        }
        let reply = handle_command(cmd, &handle, &snapshot, &store, &msg.invoker_uid).await;
        let _ = reply_tx.send(reply).await;
    }
}

async fn handle_command(cmd: Command, handle: &PlayerHandle, snapshot: &Arc<Mutex<Snapshot>>, store: &Store, uid: &str) -> String {
    match cmd {
        Command::Play(items) => {
            let single = items.len() == 1;
            let mut added = 0usize;
            let mut failed = 0usize;
            let mut last: Option<(String, Option<Duration>)> = None;
            for it in &items {
                match source::resolve(it).await {
                    Ok(r) => {
                        last = Some((r.title.clone(), r.duration));
                        handle.play(r);
                        added += 1;
                    }
                    Err(_) => failed += 1,
                }
            }
            if added == 0 {
                format!("解析失败，未添加（{failed} 个）")
            } else if single {
                let (t, d) = last.unwrap();
                match d {
                    Some(d) => format!("已添加 [b]{t}[/b]（{}）", fmt_dur(d)),
                    None => format!("已添加 [b]{t}[/b]"),
                }
            } else if failed > 0 {
                format!("已添加 {added} 首，{failed} 个失败")
            } else {
                format!("已添加 {added} 首")
            }
        }
        Command::Skip => { handle.skip(); "已跳过".to_string() }
        Command::Stop => { handle.stop(); "已停止".to_string() }
        Command::Pause => { handle.pause(); "已暂停".to_string() }
        Command::Resume => { handle.resume(); "已继续".to_string() }
        Command::Shuffle => {
            let n = snapshot.lock().unwrap().upcoming.len();
            handle.shuffle();
            format!("已打乱 {n} 首")
        }
        Command::Clear => {
            let n = snapshot.lock().unwrap().upcoming.len();
            handle.clear();
            format!("已清空待播队列（{n} 首）")
        }
        Command::NowPlaying => format_nowplaying(&snapshot.lock().unwrap()),
        Command::Queue => format_queue(&snapshot.lock().unwrap()),
        Command::Help => HELP_TEXT.to_string(),
        Command::Volume(None) => {
            let v = snapshot.lock().unwrap().volume;
            format!("音量：[b]{v}[/b]")
        }
        Command::Volume(Some(s)) => match s.parse::<u8>() {
            Ok(v) if v <= 100 => {
                handle.set_volume(v);
                format!("音量设为 [b]{v}[/b]")
            }
            _ => "音量需在 0-100 之间".to_string(),
        },
        Command::Loop(None) => {
            let m = snapshot.lock().unwrap().loop_mode;
            format!("循环：[b]{}[/b]", loop_name(m))
        }
        Command::Loop(Some(s)) => match s.to_ascii_lowercase().as_str() {
            "off" => { handle.set_loop(LoopMode::Off); "循环：[b]关[/b]".to_string() }
            "track" => { handle.set_loop(LoopMode::Track); "循环：[b]单曲[/b]".to_string() }
            "queue" => { handle.set_loop(LoopMode::Queue); "循环：[b]队列[/b]".to_string() }
            _ => "用法：!loop off|track|queue".to_string(),
        },
        Command::Remove(None) => "用法：!remove <编号>".to_string(),
        Command::Remove(Some(s)) => match s.parse::<usize>() {
            Ok(n) if n >= 1 => {
                let title = {
                    let snap = snapshot.lock().unwrap();
                    snap.upcoming.get(n - 1).map(|q| q.title.clone())
                };
                match title {
                    Some(t) => { handle.remove(n); format!("已移除 [b]{t}[/b]") }
                    None => format!("队列里没有第 {n} 首"),
                }
            }
            _ => "编号需为正整数".to_string(),
        },
        Command::Playlist(tokens) => handle_playlist(&tokens, store, handle, snapshot).await,
        Command::Whoami => format!("你的 uid：[b]{uid}[/b]"),
    }
}

fn playlist_from_snapshot(name: &str, s: &Snapshot) -> Playlist {
    let mut items = Vec::new();
    if let Some(np) = &s.now_playing {
        items.push(PlaylistItem { request: np.request.clone(), title: np.title.clone() });
    }
    for q in &s.upcoming {
        items.push(PlaylistItem { request: q.request.clone(), title: q.title.clone() });
    }
    Playlist { name: name.to_string(), items }
}

async fn handle_playlist(
    tokens: &[String],
    store: &Store,
    handle: &PlayerHandle,
    snapshot: &Arc<Mutex<Snapshot>>,
) -> String {
    let sub = tokens.first().map(|s| s.as_str()).unwrap_or("");
    match sub {
        "list" => match store.list() {
            Ok(names) if names.is_empty() => "还没有歌单".to_string(),
            Ok(names) => format!("歌单：{}", names.join("、")),
            Err(e) => format!("出错：{e}"),
        },
        "save" => {
            let Some(name) = tokens.get(1) else { return "用法：!playlist save <名>".to_string() };
            let p = {
                let s = snapshot.lock().unwrap();
                playlist_from_snapshot(name, &s)
            };
            let n = p.items.len();
            match store.save(&p) {
                Ok(()) => format!("已保存歌单 [b]{name}[/b]（{n} 首）"),
                Err(e) => format!("保存失败：{e}"),
            }
        }
        "create" => {
            let Some(name) = tokens.get(1) else { return "用法：!playlist create <名>".to_string() };
            if store.load(name).is_ok() {
                return format!("歌单 {name} 已存在");
            }
            let p = Playlist { name: name.to_string(), items: Vec::new() };
            match store.save(&p) {
                Ok(()) => format!("已创建空歌单 [b]{name}[/b]"),
                Err(e) => format!("创建失败：{e}"),
            }
        }
        "delete" => {
            let Some(name) = tokens.get(1) else { return "用法：!playlist delete <名>".to_string() };
            match store.delete(name) {
                Ok(()) => format!("已删除歌单 [b]{name}[/b]"),
                Err(e) => e.to_string(),
            }
        }
        "view" => {
            let Some(name) = tokens.get(1) else { return "用法：!playlist view <名>".to_string() };
            match store.load(name) {
                Ok(p) if p.items.is_empty() => format!("歌单 [b]{name}[/b] 为空"),
                Ok(p) => {
                    let mut out = format!("歌单 [b]{name}[/b]（{} 首）", p.items.len());
                    for (i, it) in p.items.iter().enumerate() {
                        out.push_str(&format!("\n{}. {}", i + 1, it.title));
                    }
                    out
                }
                Err(e) => e.to_string(),
            }
        }
        "remove" => {
            let (Some(name), Some(ns)) = (tokens.get(1), tokens.get(2)) else {
                return "用法：!playlist remove <名> <编号>".to_string();
            };
            let Ok(n) = ns.parse::<usize>() else { return "编号需为正整数".to_string() };
            let mut p = match store.load(name) {
                Ok(p) => p,
                Err(e) => return e.to_string(),
            };
            if n < 1 || n > p.items.len() {
                return format!("歌单里没有第 {n} 首");
            }
            let removed = p.items.remove(n - 1);
            match store.save(&p) {
                Ok(()) => format!("已从 {name} 移除 [b]{}[/b]", removed.title),
                Err(e) => format!("保存失败：{e}"),
            }
        }
        "add" => {
            let (Some(name), Some(raw)) = (tokens.get(1), tokens.get(2)) else {
                return "用法：!playlist add <名> <url>".to_string();
            };
            let url = strip_url_bbcode(raw);
            let title = match source::resolve(&url).await {
                Ok(r) => r.title,
                Err(e) => return format!("解析失败：{e}"),
            };
            let mut p = store
                .load(name)
                .unwrap_or_else(|_| Playlist { name: name.to_string(), items: Vec::new() });
            p.items.push(PlaylistItem { request: url, title: title.clone() });
            match store.save(&p) {
                Ok(()) => format!("已加入 [b]{title}[/b] → 歌单 {name}"),
                Err(e) => format!("保存失败：{e}"),
            }
        }
        "play" => {
            let Some(name) = tokens.get(1) else { return "用法：!playlist play <名>".to_string() };
            let p = match store.load(name) {
                Ok(p) => p,
                Err(e) => return e.to_string(),
            };
            let mut ok = 0usize;
            let mut fail = 0usize;
            for it in &p.items {
                match source::resolve(&it.request).await {
                    Ok(r) => {
                        handle.play(r);
                        ok += 1;
                    }
                    Err(_) => fail += 1,
                }
            }
            if fail > 0 {
                format!("已加载歌单 [b]{name}[/b]（成功 {ok}，失败 {fail}）")
            } else {
                format!("已加载歌单 [b]{name}[/b]（{ok} 首）")
            }
        }
        _ => "用法：!playlist <save|create|add|remove|list|view|play|delete>".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use player::{NowPlaying, Player, QueueItem};

    #[test]
    fn parse_routes_all_commands() {
        assert!(matches!(parse("!play a b c"), Some(Command::Play(v)) if v == vec!["a","b","c"]));
        assert!(matches!(parse("!play [URL]https://x[/URL]"), Some(Command::Play(v)) if v == vec!["https://x"]));
        assert!(matches!(parse("!skip"), Some(Command::Skip)));
        assert!(matches!(parse("!STOP"), Some(Command::Stop)));
        assert!(matches!(parse("!pause"), Some(Command::Pause)));
        assert!(matches!(parse("!resume"), Some(Command::Resume)));
        assert!(matches!(parse("!nowplaying"), Some(Command::NowPlaying)));
        assert!(matches!(parse("!queue"), Some(Command::Queue)));
        assert!(matches!(parse("!clear"), Some(Command::Clear)));
        assert!(matches!(parse("!shuffle"), Some(Command::Shuffle)));
        assert!(matches!(parse("!help"), Some(Command::Help)));
        assert!(matches!(parse("!volume 50"), Some(Command::Volume(Some(s))) if s == "50"));
        assert!(matches!(parse("!volume"), Some(Command::Volume(None))));
        assert!(matches!(parse("!loop track"), Some(Command::Loop(Some(s))) if s == "track"));
        assert!(matches!(parse("!remove 2"), Some(Command::Remove(Some(s))) if s == "2"));
        assert!(parse("!unknown").is_none());
        assert!(parse("hello").is_none());
    }

    #[test]
    fn fmt_dur_formats_mss() {
        assert_eq!(fmt_dur(Duration::from_secs(83)), "1：23");
        assert_eq!(fmt_dur(Duration::from_secs(5)), "0：05");
        assert_eq!(fmt_opt_dur(None), "LIVE");
    }

    #[test]
    fn format_nowplaying_and_queue() {
        let mut s = Snapshot { volume: 70, ..Default::default() };
        assert!(format_nowplaying(&s).contains("没有播放"));
        s.now_playing = Some(NowPlaying {
            title: "Song".into(),
            elapsed: Duration::from_secs(83),
            duration: Some(Duration::from_secs(245)),
            request: "req".into(),
        });
        s.upcoming = vec![QueueItem { title: "Next".into(), duration: Some(Duration::from_secs(190)), request: "req2".into() }];
        let np = format_nowplaying(&s);
        assert!(np.contains("[b]Song[/b]"));
        assert!(np.contains("1：23 / 4：05"));
        assert!(np.contains("音量70"));
        let q = format_queue(&s);
        assert!(q.contains("▶ [b]Song[/b] (4：05)"));
        assert!(q.contains("1. Next (3：10)"));
    }

    #[tokio::test]
    async fn run_volume_loop_remove_replies() {
        let (player, handle, snap) = Player::new().unwrap();
        drop(player);
        let (chat_tx, chat_rx) = mpsc::channel(8);
        let (reply_tx, mut reply_rx) = mpsc::channel(8);
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().to_path_buf());
        let join = tokio::spawn(run(chat_rx, handle, snap, store, perms::Permissions::default(), reply_tx));

        let send = |t: &str| chat_tx.send(ChatMessage { text: t.into(), invoker_id: ts_connection::ClientId(1), invoker_uid: "u1".into() });

        send("!volume 500").await.unwrap();
        assert!(reply_rx.recv().await.unwrap().contains("0-100"));
        send("!volume 60").await.unwrap();
        assert!(reply_rx.recv().await.unwrap().contains("60"));
        send("!loop bogus").await.unwrap();
        assert!(reply_rx.recv().await.unwrap().contains("用法"));
        send("!remove 5").await.unwrap();
        assert!(reply_rx.recv().await.unwrap().contains("没有第 5 首"));
        send("!help").await.unwrap();
        assert!(reply_rx.recv().await.unwrap().contains("!play"));

        drop(chat_tx);
        let _ = join.await;
    }

    #[test]
    fn command_name_maps() {
        assert_eq!(command_name(&Command::Play(vec![])), "play");
        assert_eq!(command_name(&Command::Whoami), "whoami");
        assert_eq!(command_name(&Command::Playlist(vec![])), "playlist");
        assert_eq!(command_name(&Command::Stop), "stop");
    }

    #[tokio::test]
    async fn run_enforces_permissions() {
        use std::collections::HashMap;
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().to_path_buf());
        let (player, handle, snap) = player::Player::new().unwrap();
        drop(player);
        let mut roles = HashMap::new();
        roles.insert("guest".to_string(), vec!["queue".to_string()]);
        let perms = Permissions { default_role: "guest".to_string(), roles, users: HashMap::new() };
        let (chat_tx, chat_rx) = mpsc::channel(8);
        let (reply_tx, mut reply_rx) = mpsc::channel(8);
        let join = tokio::spawn(run(chat_rx, handle, snap, store, perms, reply_tx));
        let send = |t: &str| chat_tx.send(ChatMessage {
            text: t.into(),
            invoker_id: ts_connection::ClientId(1),
            invoker_uid: "u1".into(),
        });

        send("!stop").await.unwrap();
        assert!(reply_rx.recv().await.unwrap().contains("无权限"));
        send("!queue").await.unwrap();
        assert!(reply_rx.recv().await.unwrap().contains("没有播放"));
        send("!help").await.unwrap();
        assert!(reply_rx.recv().await.unwrap().contains("!play"));
        send("!whoami").await.unwrap();
        assert!(reply_rx.recv().await.unwrap().contains("u1"));

        drop(chat_tx);
        let _ = join.await;
    }

    fn toks(s: &str) -> Vec<String> {
        s.split_whitespace().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parse_playlist_tokens() {
        assert!(matches!(parse("!playlist save mix"), Some(Command::Playlist(v)) if v == vec!["save","mix"]));
        assert!(matches!(parse("!playlist"), Some(Command::Playlist(v)) if v.is_empty()));
    }

    #[tokio::test]
    async fn playlist_save_view_remove_delete() {
        use player::{NowPlaying, Player, QueueItem};
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().to_path_buf());
        let (player, handle, snap) = Player::new().unwrap();
        drop(player);
        {
            let mut s = snap.lock().unwrap();
            s.now_playing = Some(NowPlaying {
                title: "Cur".into(),
                elapsed: Duration::ZERO,
                duration: None,
                request: "req-cur".into(),
            });
            s.upcoming = vec![QueueItem { title: "Up".into(), duration: None, request: "req-up".into() }];
        }
        assert!(handle_playlist(&toks("save mix"), &store, &handle, &snap).await.contains("已保存"));
        assert!(handle_playlist(&toks("list"), &store, &handle, &snap).await.contains("mix"));
        let v = handle_playlist(&toks("view mix"), &store, &handle, &snap).await;
        assert!(v.contains("1. Cur") && v.contains("2. Up"));
        assert!(handle_playlist(&toks("remove mix 1"), &store, &handle, &snap).await.contains("Cur"));
        assert!(handle_playlist(&toks("view mix"), &store, &handle, &snap).await.contains("1. Up"));
        assert!(handle_playlist(&toks("delete mix"), &store, &handle, &snap).await.contains("已删除"));
        assert!(handle_playlist(&toks("list"), &store, &handle, &snap).await.contains("还没有"));
        assert!(handle_playlist(&toks("save a/b"), &store, &handle, &snap).await.contains("失败"));
    }
}
