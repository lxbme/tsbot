use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tokio::process::Command;

/// 一个可交给 ffmpeg 播放的条目。
pub struct Resolved {
    pub input: String,
    pub title: String,
    pub duration: Option<Duration>,
}

/// 解析用户参数为可播条目（含元数据）。
pub async fn resolve(arg: &str) -> Result<Resolved> {
    if Path::new(arg).is_file() {
        let (title, duration) = ffprobe_meta(arg).await;
        return Ok(Resolved { input: arg.to_string(), title, duration });
    }
    if arg.starts_with("http://") || arg.starts_with("https://") {
        return match ytdlp_meta(arg).await {
            Ok(r) => Ok(r),
            Err(_) => Ok(Resolved { input: arg.to_string(), title: arg.to_string(), duration: None }),
        };
    }
    bail!("无法识别的音源: {arg}（需为存在的本地文件或 http(s) URL）")
}

/// yt-dlp 一次取 标题 / 时长 / 直连URL。
async fn ytdlp_meta(url: &str) -> Result<Resolved> {
    let out = Command::new("yt-dlp")
        .args([
            "-f", "bestaudio/best",
            "--print", "%(title)s",
            "--print", "%(duration)s",
            "--print", "%(url)s",
            url,
        ])
        .stdin(Stdio::null())
        .output()
        .await
        .context("启动 yt-dlp 失败")?;
    if !out.status.success() {
        bail!("yt-dlp 解析失败: {}", String::from_utf8_lossy(&out.stderr));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    let title = lines.next().unwrap_or("").trim().to_string();
    let dur = lines.next().unwrap_or("").trim();
    let input = lines.next().unwrap_or("").trim().to_string();
    if input.is_empty() {
        bail!("yt-dlp 未返回直连 URL");
    }
    let duration = dur.parse::<u64>().ok().map(Duration::from_secs);
    let title = if title.is_empty() { url.to_string() } else { title };
    Ok(Resolved { input, title, duration })
}

/// ffprobe 取本地文件 时长 + 标题标签；失败回退文件名。
async fn ffprobe_meta(path: &str) -> (String, Option<Duration>) {
    let filename = Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(path)
        .to_string();
    let out = Command::new("ffprobe")
        .args([
            "-v", "quiet",
            "-show_entries", "format=duration:format_tags=title",
            "-of", "default=noprint_wrappers=1",
            path,
        ])
        .stdin(Stdio::null())
        .output()
        .await;
    let Ok(out) = out else { return (filename, None) };
    if !out.status.success() {
        return (filename, None);
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut title: Option<String> = None;
    let mut duration: Option<Duration> = None;
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("duration=") {
            duration = v.trim().parse::<f64>().ok().map(Duration::from_secs_f64);
        } else if let Some(v) = line.strip_prefix("TAG:title=") {
            let v = v.trim();
            if !v.is_empty() {
                title = Some(v.to_string());
            }
        }
    }
    (title.unwrap_or(filename), duration)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resolves_local_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("song.mp3");
        let p = path.to_str().unwrap();
        let made = std::process::Command::new("ffmpeg")
            .args([
                "-hide_banner", "-loglevel", "error",
                "-f", "lavfi", "-i", "sine=frequency=440:duration=1",
                "-metadata", "title=My Tune", "-c:a", "libmp3lame", p, "-y",
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if made {
            let r = resolve(p).await.unwrap();
            assert_eq!(r.input, p);
            assert_eq!(r.title, "My Tune");
            assert!(r.duration.is_some());
        } else {
            std::fs::write(&path, b"x").unwrap();
            let r = resolve(p).await.unwrap();
            assert_eq!(r.title, "song.mp3");
            assert!(r.duration.is_none());
        }
    }

    #[tokio::test]
    async fn rejects_non_file_non_url() {
        assert!(resolve("not a url or file").await.is_err());
    }
}
