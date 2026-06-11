use player::Snapshot;

/// 解析后的指令。
pub enum Command {
    Play(String),
    Skip,
    Stop,
    Queue,
}

/// 解析一行文本；非指令（不以 ! 开头或未知/缺参）返回 None。
pub fn parse(text: &str) -> Option<Command> {
    let rest = text.trim().strip_prefix('!')?;
    let mut it = rest.splitn(2, char::is_whitespace);
    let cmd = it.next()?.to_ascii_lowercase();
    let arg = it.next().map(str::trim).unwrap_or("");
    match cmd.as_str() {
        "play" if !arg.is_empty() => Some(Command::Play(arg.to_string())),
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
    fn format_queue_empty_and_filled() {
        let empty = Snapshot::default();
        assert!(format_queue(&empty).contains("没有播放"));
        let filled = Snapshot { now_playing: Some("A".into()), upcoming: vec!["B".into()] };
        let s = format_queue(&filled);
        assert!(s.contains("正在播放: A"));
        assert!(s.contains("1. B"));
    }
}
