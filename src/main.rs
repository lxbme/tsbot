mod audio_source;
mod config;
mod identity_store;
mod opus_enc;

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Result};
use clap::Parser;
use futures::prelude::*;
use tsclientlib::{Connection, DisconnectOptions, StreamItem};
use tsproto_packets::packets::{AudioData, CodecType, OutAudio};

use audio_source::{spawn_ffmpeg, PcmFrameReader};
use config::Args;
use opus_enc::OpusMusicEncoder;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    // 1. identity（生成或复用）
    let identity = identity_store::load_or_create(Path::new(&args.identity))?;

    // 2. 构建连接配置
    let mut cfg = Connection::build(args.address.clone())
        .identity(identity)
        .name("tsbot".to_string());
    if let Some(pw) = &args.password {
        cfg = cfg.password(pw.clone());
    }
    if let Some(ch) = &args.channel {
        cfg = cfg.channel(ch.clone());
    }

    // 3. 连接并等待 book 就绪
    let mut con = cfg.connect()?;
    let r = con
        .events()
        .try_filter(|e| future::ready(matches!(e, StreamItem::BookEvents(_))))
        .next()
        .await;
    if let Some(r) = r {
        r?;
    }
    tracing::info!("connected, start streaming {}", args.file);

    // 4. 启动 ffmpeg + 编码器 + 20ms 定时器
    let (mut child, stdout) = spawn_ffmpeg(&args.file)?;
    let mut reader = PcmFrameReader::new(stdout);
    let mut encoder = OpusMusicEncoder::new()?;
    let mut interval = tokio::time::interval(Duration::from_millis(20));

    // 5. 播放循环：每 20ms 发一帧
    loop {
        let events = con.events().try_for_each(|_| future::ready(Ok(())));
        tokio::select! {
            _ = interval.tick() => {}
            _ = tokio::signal::ctrl_c() => break,
            r = events => { r?; bail!("Disconnected"); }
        }

        match reader.next_frame().await? {
            Some(frame) => {
                let data = encoder.encode(&frame)?;
                let packet = OutAudio::new(&AudioData::C2S {
                    id: 0,
                    codec: CodecType::OpusMusic,
                    data,
                });
                con.send_audio(packet)?;
            }
            None => break, // 文件播完
        }
    }

    // 6. 发空音频包表示停止说话，断开
    let stop = OutAudio::new(&AudioData::C2S { id: 0, codec: CodecType::OpusMusic, data: &[] });
    let _ = con.send_audio(stop);
    let _ = child.kill().await;
    con.disconnect(DisconnectOptions::new())?;
    con.events().for_each(|_| future::ready(())).await;
    Ok(())
}
