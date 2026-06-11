use std::path::Path;

use anyhow::Result;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub server: Server,
    pub bot: Bot,
    pub playback: Playback,
}

#[derive(Debug, Deserialize)]
pub struct Server {
    pub address: String,
    pub password: Option<String>,
    pub channel: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Bot {
    pub name: String,
    pub identity_path: String,
}

#[derive(Debug, Deserialize)]
pub struct Playback {
    pub file: String,
}

/// 读取并解析 TOML 配置文件。
pub fn load(path: &Path) -> Result<Config> {
    let s = std::fs::read_to_string(path)?;
    Ok(toml::from_str(&s)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_tmp(body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, body).unwrap();
        (dir, path)
    }

    #[test]
    fn loads_full_config() {
        let (_dir, path) = write_tmp(
            r#"
[server]
address = "ts.example.com:9987"
password = "secret"
channel = "Lobby"

[bot]
name = "tsbot"
identity_path = "identity.toml"

[playback]
file = "test.mp3"
"#,
        );
        let c = load(&path).unwrap();
        assert_eq!(c.server.address, "ts.example.com:9987");
        assert_eq!(c.server.password.as_deref(), Some("secret"));
        assert_eq!(c.server.channel.as_deref(), Some("Lobby"));
        assert_eq!(c.bot.name, "tsbot");
        assert_eq!(c.bot.identity_path, "identity.toml");
        assert_eq!(c.playback.file, "test.mp3");
    }

    #[test]
    fn loads_config_without_optionals() {
        let (_dir, path) = write_tmp(
            r#"
[server]
address = "localhost"

[bot]
name = "tsbot"
identity_path = "identity.toml"

[playback]
file = "a.mp3"
"#,
        );
        let c = load(&path).unwrap();
        assert!(c.server.password.is_none());
        assert!(c.server.channel.is_none());
        assert_eq!(c.server.address, "localhost");
    }
}
