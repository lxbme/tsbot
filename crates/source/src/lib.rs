use std::path::Path;
use std::process::Stdio;

use anyhow::{bail, Context, Result};
use tokio::process::Command;

/// 一个可交给 ffmpeg 播放的条目。
pub struct Resolved {
    /// ffmpeg `-i` 的参数：本地路径 / 直连或解析后的媒体 URL。
    pub input: String,
    /// 队列展示用；Phase 1 = 原始请求串。
    pub label: String,
}

/// 解析用户参数为可播条目。
/// - 本地存在文件 → 直接用其路径
/// - http(s) → `yt-dlp -g` 取直连 URL；失败则回退用原始 URL（直连流/电台）
/// - 其它 → 错误
pub async fn resolve(arg: &str) -> Result<Resolved> {
    if Path::new(arg).is_file() {
        return Ok(Resolved { input: arg.to_string(), label: arg.to_string() });
    }
    if arg.starts_with("http://") || arg.starts_with("https://") {
        let input = match ytdlp_direct_url(arg).await {
            Ok(url) => url,
            Err(_) => arg.to_string(), // 回退：当作直连流
        };
        return Ok(Resolved { input, label: arg.to_string() });
    }
    bail!("无法识别的音源: {arg}（需为存在的本地文件或 http(s) URL）")
}

async fn ytdlp_direct_url(url: &str) -> Result<String> {
    let out = Command::new("yt-dlp")
        .args(["-g", "-f", "bestaudio/best", url])
        .stdin(Stdio::null())
        .output()
        .await
        .context("启动 yt-dlp 失败")?;
    if !out.status.success() {
        bail!("yt-dlp 解析失败: {}", String::from_utf8_lossy(&out.stderr));
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .context("yt-dlp 未返回 URL")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resolves_existing_local_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("song.mp3");
        std::fs::write(&path, b"x").unwrap();
        let p = path.to_str().unwrap();
        let r = resolve(p).await.unwrap();
        assert_eq!(r.input, p);
        assert_eq!(r.label, p);
    }

    #[tokio::test]
    async fn rejects_non_file_non_url() {
        let err = resolve("not a url or file").await;
        assert!(err.is_err());
    }
}
