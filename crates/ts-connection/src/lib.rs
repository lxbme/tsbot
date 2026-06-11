use std::future::Future;
use std::time::Duration;

use anyhow::{bail, Result};
use futures::prelude::*;
use tokio::sync::mpsc;
use tsclientlib::events::Event;
use tsclientlib::messages::c2s;
use tsclientlib::{MessageTarget, OutCommandExt, TextMessageTargetMode};
use tsproto_packets::packets::{AudioData, CodecType, OutAudio, OutPacket};

pub use tsclientlib::{ClientId, Connection, DisconnectOptions, Identity, StreamItem};

pub mod identity;

/// 建立连接所需的设置，由 bin 从配置映射而来。
#[derive(Clone)]
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

/// 音频帧来源：被 driver 按 20ms 拉取。
#[allow(async_fn_in_trait)]
pub trait OpusSource {
    /// 返回下一帧已编码 opus 字节；`None` 表示本 tick 没有帧（空闲）；
    /// driver 应继续循环，而不是结束。
    async fn next_frame(&mut self) -> Result<Option<Vec<u8>>>;
}

/// 包一个 C2S OpusMusic 音频包。
fn opus_music_packet(data: &[u8]) -> OutPacket {
    OutAudio::new(&AudioData::C2S { id: 0, codec: CodecType::OpusMusic, data })
}

/// 收到的频道文本消息，driver 转发给指令处理器。
pub struct ChatMessage {
    pub text: String,
    pub invoker_id: ClientId,
}

/// 向机器人所在频道发送一条文本。
fn send_channel_text(con: &mut Connection, text: &str) -> Result<()> {
    let cmd = c2s::OutSendTextMessageMessage::new(&mut std::iter::once(c2s::OutSendTextMessagePart {
        target: TextMessageTargetMode::Channel,
        target_client_id: None,
        message: text.into(),
    }));
    cmd.send(con)?;
    Ok(())
}

/// 驱动单条连接直到断线返回 Err。
pub async fn run<S: OpusSource>(
    con: &mut Connection,
    source: &mut S,
    chat_tx: &mpsc::Sender<ChatMessage>,
    reply_rx: &mut mpsc::Receiver<String>,
) -> Result<()> {
    let own_id = con.get_state()?.own_client;
    let mut interval = tokio::time::interval(Duration::from_millis(20));
    loop {
        while let Ok(text) = reply_rx.try_recv() {
            if let Err(e) = send_channel_text(con, &text) {
                tracing::warn!(%e, "发送回复失败");
            }
        }
        let events = con.events().try_for_each(|item| {
            if let StreamItem::BookEvents(evs) = &item {
                for e in evs {
                    if let Event::Message { target: MessageTarget::Channel, invoker, message } = e {
                        if invoker.id != own_id {
                            tracing::debug!(%message, "收到频道消息");
                            let _ = chat_tx.try_send(ChatMessage {
                                text: message.clone(),
                                invoker_id: invoker.id,
                            });
                        }
                    }
                }
            }
            future::ready(Ok(()))
        });
        tokio::select! {
            _ = interval.tick() => {}
            r = events => { r?; bail!("Disconnected"); }
        }
        if let Some(data) = source.next_frame().await? {
            con.send_audio(opus_music_packet(&data))?;
        }
    }
}

/// 向服务器发送干净的断开，并排空事件直到连接关闭（套超时兜底，防服务器失联时卡死）。
async fn graceful_disconnect(con: &mut Connection) {
    if let Err(e) = con.disconnect(DisconnectOptions::new()) {
        tracing::warn!(%e, "disconnect 失败");
        return;
    }
    let drain = con.events().for_each(|_| future::ready(()));
    if tokio::time::timeout(Duration::from_secs(5), drain).await.is_err() {
        tracing::warn!("断开排空超时，强制退出");
    }
}

/// 常驻：connect → wait_until_ready → run；断线后指数退避重连，直到 shutdown。
/// shutdown 触发时主动 disconnect 优雅退出。
pub async fn run_persistent<S: OpusSource>(
    settings: ConnectSettings,
    source: &mut S,
    chat_tx: mpsc::Sender<ChatMessage>,
    mut reply_rx: mpsc::Receiver<String>,
    shutdown: impl Future<Output = ()>,
) -> Result<()> {
    tokio::pin!(shutdown);
    let mut backoff = Duration::from_secs(1);
    loop {
        // 建立连接（同步）
        let mut con = match connect(settings.clone()) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(%e, ?backoff, "连接失败，准备重连");
                tokio::select! {
                    _ = &mut shutdown => return Ok(()),
                    _ = tokio::time::sleep(backoff) => {}
                }
                backoff = (backoff * 2).min(Duration::from_secs(30));
                continue;
            }
        };

        // 等待就绪，与 shutdown 赛跑
        tokio::select! {
            _ = &mut shutdown => { tracing::info!("shutdown"); graceful_disconnect(&mut con).await; return Ok(()); }
            r = wait_until_ready(&mut con) => {
                if let Err(e) = r {
                    tracing::warn!(%e, ?backoff, "等待就绪失败，准备重连");
                    tokio::select! {
                        _ = &mut shutdown => { graceful_disconnect(&mut con).await; return Ok(()); }
                        _ = tokio::time::sleep(backoff) => {}
                    }
                    backoff = (backoff * 2).min(Duration::from_secs(30));
                    continue;
                }
            }
        }
        tracing::info!("connected");
        backoff = Duration::from_secs(1);

        // 驱动连接，与 shutdown 赛跑
        tokio::select! {
            _ = &mut shutdown => { tracing::info!("shutdown"); graceful_disconnect(&mut con).await; return Ok(()); }
            r = run(&mut con, source, &chat_tx, &mut reply_rx) => {
                if let Err(e) = r {
                    tracing::warn!(%e, ?backoff, "连接断开，准备重连");
                }
            }
        }

        // 非 shutdown 的断开：退避后重连
        tokio::select! {
            _ = &mut shutdown => return Ok(()),
            _ = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}
