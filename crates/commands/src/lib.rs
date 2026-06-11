use std::sync::{Arc, Mutex};

use player::{PlayerHandle, Snapshot};
use tokio::sync::mpsc;
use ts_connection::ChatMessage;

/// 解析后的指令。
pub enum Command {
    Play(String),
    Skip,
    Stop,
    Queue,
}

/// 去掉 TeamSpeak 自动给 URL 套的 BBCode 包裹，取出其中的 URL：
/// `[URL]X[/URL]` → `X`；`[URL=X]label[/URL]` → `X`（取 target）。
/// 非该形态则原样返回（去首尾空白）。大小写不敏感。
fn strip_url_bbcode(s: &str) -> String {
    let t = s.trim();
    let lower = t.to_ascii_lowercase();
    if !lower.starts_with("[url") || !lower.ends_with("[/url]") {
        return t.to_string();
    }
    let Some(open_end) = t.find(']') else {
        return t.to_string();
    };
    let open_tag = &t[..open_end]; // "[URL" 或 "[URL=https://x"
    if let Some(eq) = open_tag.find('=') {
        return open_tag[eq + 1..].trim().to_string();
    }
    // 取开标签与闭标签之间的内容（闭标签 "[/url]" 为 6 个 ASCII 字节）
    t[open_end + 1..t.len() - 6].trim().to_string()
}

/// 解析一行文本；非指令（不以 ! 开头或未知/缺参）返回 None。
pub fn parse(text: &str) -> Option<Command> {
    let rest = text.trim().strip_prefix('!')?;
    let mut it = rest.splitn(2, char::is_whitespace);
    let cmd = it.next()?.to_ascii_lowercase();
    let arg = it.next().map(str::trim).unwrap_or("");
    match cmd.as_str() {
        "play" if !arg.is_empty() => Some(Command::Play(strip_url_bbcode(arg))),
        "skip" => Some(Command::Skip),
        "stop" => Some(Command::Stop),
        "queue" => Some(Command::Queue),
        _ => None,
    }
}

/// 把快照格式化为 `!queue` 的回复文本。
pub fn format_queue(s: &Snapshot) -> String {
    let mut out = match &s.now_playing {
        Some(np) => format!("正在播放: {np}"),
        None => "当前没有播放".to_string(),
    };
    if s.upcoming.is_empty() {
        out.push_str("\n队列为空");
    } else {
        out.push_str("\n队列:");
        for (i, label) in s.upcoming.iter().enumerate() {
            out.push_str(&format!("\n  {}. {}", i + 1, label));
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
        let reply = match cmd {
            Command::Play(arg) => match source::resolve(&arg).await {
                Ok(r) => {
                    let label = r.title.clone();
                    handle.play(r);
                    format!("已添加: {label}")
                }
                Err(e) => format!("解析失败: {e}"),
            },
            Command::Skip => { handle.skip(); "已跳过".to_string() }
            Command::Stop => { handle.stop(); "已停止并清空队列".to_string() }
            Command::Queue => {
                let snap = snapshot.lock().unwrap().clone();
                format_queue(&snap)
            }
        };
        let _ = reply_tx.send(reply).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_play_with_arg() {
        assert!(matches!(parse("!play http://x"), Some(Command::Play(a)) if a == "http://x"));
    }

    #[test]
    fn parse_play_multiword_arg_trimmed() {
        assert!(matches!(parse("  !play  a b  "), Some(Command::Play(a)) if a == "a b"));
    }

    #[test]
    fn parse_play_without_arg_is_none() {
        assert!(parse("!play").is_none());
    }

    #[test]
    fn parse_skip_stop_queue_case_insensitive() {
        assert!(matches!(parse("!skip"), Some(Command::Skip)));
        assert!(matches!(parse("!STOP"), Some(Command::Stop)));
        assert!(matches!(parse("!Queue"), Some(Command::Queue)));
    }

    #[test]
    fn parse_non_command_is_none() {
        assert!(parse("hello world").is_none());
        assert!(parse("!unknown").is_none());
    }

    #[test]
    fn parse_play_strips_ts_url_bbcode() {
        // TeamSpeak 把粘贴的 URL 自动包成 [URL]...[/URL]
        let url = "https://www.youtube.com/watch?v=GQ15U4NJftA";
        assert!(matches!(
            parse(&format!("!play [URL]{url}[/URL]")),
            Some(Command::Play(a)) if a == url
        ));
        // 带标签形式 [URL=target]label[/URL] → 取 target
        assert!(matches!(
            parse("!play [URL=https://a.com/s.mp3]我的电台[/URL]"),
            Some(Command::Play(a)) if a == "https://a.com/s.mp3"
        ));
        // 大小写不敏感
        assert!(matches!(
            parse(&format!("!play [url]{url}[/url]")),
            Some(Command::Play(a)) if a == url
        ));
    }

    #[test]
    fn strip_url_bbcode_passes_through_plain() {
        assert_eq!(strip_url_bbcode("https://x.com"), "https://x.com");
        assert_eq!(strip_url_bbcode("/home/me/a.mp3"), "/home/me/a.mp3");
    }

    #[test]
    fn format_queue_empty_and_filled() {
        let empty = Snapshot::default();
        assert!(format_queue(&empty).contains("没有播放"));
        let filled = Snapshot { now_playing: Some("A".into()), upcoming: vec!["B".into()] };
        let s = format_queue(&filled);
        assert!(s.contains("正在播放: A"));
        assert!(s.contains("1. B"));
    }

    #[tokio::test]
    async fn run_dispatches_skip_and_queue() {
        use player::Player;

        let (player, handle, snap) = Player::new().unwrap();
        drop(player); // 本测试只验证句柄与回复，不驱动 player
        let (chat_tx, chat_rx) = mpsc::channel(8);
        let (reply_tx, mut reply_rx) = mpsc::channel(8);
        let join = tokio::spawn(run(chat_rx, handle, snap, reply_tx));

        chat_tx.send(ChatMessage { text: "!queue".into(), invoker_id: ts_connection::ClientId(1) }).await.unwrap();
        let r = reply_rx.recv().await.unwrap();
        assert!(r.contains("没有播放"));

        chat_tx.send(ChatMessage { text: "!skip".into(), invoker_id: ts_connection::ClientId(1) }).await.unwrap();
        assert_eq!(reply_rx.recv().await.unwrap(), "已跳过");

        drop(chat_tx);
        let _ = join.await;
    }
}
