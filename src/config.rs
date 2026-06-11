use clap::Parser;

#[derive(Parser, Debug)]
#[command(about = "TS3 musicbot MVP")]
pub struct Args {
    /// TS3 服务器地址 (host 或 host:port)
    #[arg(short, long)]
    pub address: String,
    /// 要播放的本地音频文件路径
    #[arg(short, long)]
    pub file: String,
    /// 服务器密码（可选）
    #[arg(short, long)]
    pub password: Option<String>,
    /// 连接后切入的频道名/路径（可选）
    #[arg(short, long)]
    pub channel: Option<String>,
    /// identity 持久化文件路径
    #[arg(short, long, default_value = "identity.toml")]
    pub identity: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_required_and_optional_args() {
        let args = Args::try_parse_from([
            "tsbot", "--address", "ts.example.com:9987", "--file", "song.mp3",
            "--password", "secret", "--channel", "Lobby",
        ])
        .unwrap();
        assert_eq!(args.address, "ts.example.com:9987");
        assert_eq!(args.file, "song.mp3");
        assert_eq!(args.password.as_deref(), Some("secret"));
        assert_eq!(args.channel.as_deref(), Some("Lobby"));
        assert_eq!(args.identity, "identity.toml");
    }

    #[test]
    fn missing_required_args_fails() {
        let res = Args::try_parse_from(["tsbot", "--address", "x"]);
        assert!(res.is_err());
    }
}
