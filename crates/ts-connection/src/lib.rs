use std::time::Duration;

use anyhow::{bail, Result};
use futures::prelude::*;
use tsproto_packets::packets::{AudioData, CodecType, OutAudio, OutPacket};

pub use tsclientlib::{Connection, DisconnectOptions, Identity, StreamItem};

pub mod identity;

/// 建立连接所需的设置，由 bin 从配置映射而来。
pub struct ConnectSettings {
    pub address: String,
    pub password: Option<String>,
    pub channel: Option<String>,
    pub name: String,
    pub identity: Identity,
}

/// 用 ConnectSettings 构建 ConnectOptions 并发起连接。
pub fn connect(settings: ConnectSettings) -> Result<Connection> {
    let mut cfg = Connection::build(settings.address)
        .identity(settings.identity)
        .name(settings.name);
    if let Some(pw) = settings.password {
        cfg = cfg.password(pw);
    }
    if let Some(ch) = settings.channel {
        cfg = cfg.channel(ch);
    }
    Ok(cfg.connect()?)
}

/// 等待 BookEvents，确认连接就绪。
pub async fn wait_until_ready(con: &mut Connection) -> Result<()> {
    let r = con
        .events()
        .try_filter(|e| future::ready(matches!(e, StreamItem::BookEvents(_))))
        .next()
        .await;
    if let Some(r) = r {
        r?;
    }
    Ok(())
}

/// 音频帧来源：被 `stream_audio` 按 20ms 拉取。
#[allow(async_fn_in_trait)]
pub trait OpusSource {
    /// 返回下一帧已编码 opus 字节；None 表示流结束。
    async fn next_frame(&mut self) -> Result<Option<Vec<u8>>>;
}

/// 包一个 C2S OpusMusic 音频包。
fn opus_music_packet(data: &[u8]) -> OutPacket {
    OutAudio::new(&AudioData::C2S { id: 0, codec: CodecType::OpusMusic, data })
}

/// 驱动连接：轮询事件保活 + 20ms 节奏 + 拉帧发送，
/// 直到 source 返回 None（正常结束，发送停止包）或断线（返回 Err）。
pub async fn stream_audio<S: OpusSource>(con: &mut Connection, source: &mut S) -> Result<()> {
    let mut interval = tokio::time::interval(Duration::from_millis(20));
    loop {
        let events = con.events().try_for_each(|_| future::ready(Ok(())));
        tokio::select! {
            _ = interval.tick() => {}
            r = events => { r?; bail!("Disconnected"); }
        }

        match source.next_frame().await? {
            Some(data) => con.send_audio(opus_music_packet(&data))?,
            None => break, // 流结束
        }
    }
    // 发空音频包表示停止说话
    let _ = con.send_audio(opus_music_packet(&[]));
    Ok(())
}
