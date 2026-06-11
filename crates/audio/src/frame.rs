use std::process::Stdio;

use anyhow::Result;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};

/// 声道数：立体声（保留音乐的立体声宽度，避免单声道下混抵消高频）。
pub const CHANNELS: usize = 2;
/// 每声道每帧样本数 = 48000 / 50 = 960（48kHz、20ms）。
const SAMPLES_PER_CHANNEL: usize = 960;
/// 每帧交错样本总数（L,R,L,R… 交错）= 1920。
pub const FRAME_SAMPLES: usize = SAMPLES_PER_CHANNEL * CHANNELS;
const FRAME_BYTES: usize = FRAME_SAMPLES * 4; // f32le = 4 字节/样本

/// 从任意字节流按固定 20ms 帧读取 f32 样本。最后一帧不足时用静音(0.0)补齐。
pub struct PcmFrameReader<R> {
    inner: R,
    done: bool,
}

impl<R: AsyncRead + Unpin> PcmFrameReader<R> {
    pub fn new(inner: R) -> Self {
        Self { inner, done: false }
    }

    /// 返回下一帧；输入耗尽后返回 `Ok(None)`。
    pub async fn next_frame(&mut self) -> Result<Option<[f32; FRAME_SAMPLES]>> {
        if self.done {
            return Ok(None);
        }
        let mut buf = [0u8; FRAME_BYTES];
        let mut filled = 0;
        while filled < FRAME_BYTES {
            let n = self.inner.read(&mut buf[filled..]).await?;
            if n == 0 {
                break; // EOF
            }
            filled += n;
        }
        if filled == 0 {
            self.done = true;
            return Ok(None);
        }
        if filled < FRAME_BYTES {
            self.done = true; // 这是最后一帧（已补零）
        }
        let mut frame = [0f32; FRAME_SAMPLES];
        for (i, chunk) in buf[..filled].chunks_exact(4).enumerate() {
            frame[i] = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        Ok(Some(frame))
    }
}

/// 启动 ffmpeg 把输入解码为 48kHz/立体声/f32le 裸 PCM（L,R 交错），返回子进程与其 stdout。
/// 输入可为本地路径或 URL（ffmpeg `-i` 通用）。子进程在 drop 时被杀。
pub fn spawn_ffmpeg(input: &str) -> Result<(Child, tokio::process::ChildStdout)> {
    let mut child = Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "error",
            "-i", input,
            "-ac", "2", "-ar", "48000", "-f", "f32le", "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let stdout = child.stdout.take().expect("stdout piped");
    Ok((child, stdout))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn bytes_of(samples: &[f32]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    #[tokio::test]
    async fn yields_full_frames_then_none() {
        let input = bytes_of(&vec![1.0f32; FRAME_SAMPLES * 2]);
        let mut r = PcmFrameReader::new(Cursor::new(input));

        let f1 = r.next_frame().await.unwrap().unwrap();
        assert!(f1.iter().all(|&s| s == 1.0));
        let f2 = r.next_frame().await.unwrap().unwrap();
        assert!(f2.iter().all(|&s| s == 1.0));
        assert!(r.next_frame().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn pads_final_partial_frame_with_silence() {
        // 只有 1 个样本，不足一帧
        let input = bytes_of(&[0.5f32]);
        let mut r = PcmFrameReader::new(Cursor::new(input));

        let f = r.next_frame().await.unwrap().unwrap();
        assert_eq!(f[0], 0.5);
        assert!(f[1..].iter().all(|&s| s == 0.0));
        assert!(r.next_frame().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn empty_input_yields_none() {
        let mut r = PcmFrameReader::new(Cursor::new(Vec::new()));
        assert!(r.next_frame().await.unwrap().is_none());
    }
}
