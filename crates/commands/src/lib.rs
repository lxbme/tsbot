use std::sync::{Arc, Mutex};
use std::time::Duration;

use player::{LoopMode, PlayerHandle, Snapshot};
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
    Volume(Option<String>),
    Loop(Option<String>),
    Remove(Option<String>),
}

const HELP_TEXT: &str = "可用命令：\n\
!play <url/路径> [更多…] 点播(支持空格批量)\n\
!pause / !resume 暂停/继续\n\
!skip 跳过  !stop 停止并清空\n\
!volume [0-100] 音量  !loop [off|track|queue] 循环\n\
!nowplaying 当前曲目  !queue 队列\n\
!remove <编号> 移除待播  !clear 清空待播  !shuffle 打乱\n\
!help 帮助";

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
        "volume" => Some(Command::Volume(opt(arg))),
        "loop" => Some(Command::Loop(opt(arg))),
        "remove" => Some(Command::Remove(opt(arg))),
        _ => None,
    }
}

/// Duration → "m:ss"。
fn fmt_dur(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
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
    reply_tx: mpsc::Sender<String>,
) {
    while let Some(msg) = chat_rx.recv().await {
        let Some(cmd) = parse(&msg.text) else { continue };
        let reply = handle_command(cmd, &handle, &snapshot).await;
        let _ = reply_tx.send(reply).await;
    }
}

async fn handle_command(cmd: Command, handle: &PlayerHandle, snapshot: &Arc<Mutex<Snapshot>>) -> String {
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
        assert_eq!(fmt_dur(Duration::from_secs(83)), "1:23");
        assert_eq!(fmt_dur(Duration::from_secs(5)), "0:05");
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
        assert!(np.contains("1:23 / 4:05"));
        assert!(np.contains("音量70"));
        let q = format_queue(&s);
        assert!(q.contains("▶ [b]Song[/b] (4:05)"));
        assert!(q.contains("1. Next (3:10)"));
    }

    #[tokio::test]
    async fn run_volume_loop_remove_replies() {
        let (player, handle, snap) = Player::new().unwrap();
        drop(player);
        let (chat_tx, chat_rx) = mpsc::channel(8);
        let (reply_tx, mut reply_rx) = mpsc::channel(8);
        let join = tokio::spawn(run(chat_rx, handle, snap, reply_tx));

        let send = |t: &str| chat_tx.send(ChatMessage { text: t.into(), invoker_id: ts_connection::ClientId(1) });

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
}
