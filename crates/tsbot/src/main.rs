mod config;

use std::path::Path;

use anyhow::Result;
use clap::Parser;
use futures::prelude::*;
use tokio::io::AsyncRead;
use ts_connection::{ConnectSettings, DisconnectOptions, OpusSource};
use tsbot_audio::{spawn_ffmpeg, OpusMusicEncoder, PcmFrameReader};

use config::Args;

/// 把 ffmpeg 解出的 PCM 经分帧 + Opus 编码，作为 stream_audio 的拉取源。
struct FileOpusSource<R> {
    reader: PcmFrameReader<R>,
    encoder: OpusMusicEncoder,
}

impl<R> FileOpusSource<R> {
    fn new(reader: PcmFrameReader<R>, encoder: OpusMusicEncoder) -> Self {
        Self { reader, encoder }
    }
}

impl<R: AsyncRead + Unpin> OpusSource for FileOpusSource<R> {
    async fn next_frame(&mut self) -> Result<Option<Vec<u8>>> {
        match self.reader.next_frame().await? {
            Some(frame) => Ok(Some(self.encoder.encode(&frame)?.to_vec())),
            None => Ok(None),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    // 1. identity（生成或复用）
    let identity = ts_connection::identity::load_or_create(Path::new(&args.identity))?;

    // 2. 连接并等待就绪
    let settings = ConnectSettings {
        address: args.address.clone(),
        password: args.password.clone(),
        channel: args.channel.clone(),
        name: "tsbot".to_string(),
        identity,
    };
    let mut con = ts_connection::connect(settings)?;
    ts_connection::wait_until_ready(&mut con).await?;
    tracing::info!("connected, start streaming {}", args.file);

    // 3. ffmpeg 源 + 编码器 → FileOpusSource
    let (mut child, stdout) = spawn_ffmpeg(&args.file)?;
    let mut source = FileOpusSource::new(PcmFrameReader::new(stdout), OpusMusicEncoder::new()?);

    // 4. 驱动发送，ctrl_c 可中断
    tokio::select! {
        r = ts_connection::stream_audio(&mut con, &mut source) => r?,
        _ = tokio::signal::ctrl_c() => {}
    }

    // 5. 清理并断开
    let _ = child.kill().await;
    con.disconnect(DisconnectOptions::new())?;
    con.events().for_each(|_| future::ready(())).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tsbot_audio::FRAME_SAMPLES;

    #[tokio::test]
    async fn file_source_yields_opus_then_none() {
        // 一帧静音的 PCM（960 f32 = 3840 字节）
        let pcm = vec![0u8; FRAME_SAMPLES * 4];
        let reader = PcmFrameReader::new(Cursor::new(pcm));
        let mut src = FileOpusSource::new(reader, OpusMusicEncoder::new().unwrap());

        let first = src.next_frame().await.unwrap();
        assert!(first.is_some());
        assert!(!first.unwrap().is_empty());
        assert!(src.next_frame().await.unwrap().is_none());
    }
}
