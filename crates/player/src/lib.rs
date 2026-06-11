use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use tokio::process::{Child, ChildStdout};
use tokio::sync::mpsc;

use source::Resolved;
use tsbot_audio::{spawn_ffmpeg, OpusMusicEncoder, PcmFrameReader};
use ts_connection::OpusSource;

/// 控制命令，经 control 通道从指令处理器发往 player。
pub enum Control {
    Play(Resolved),
    Skip,
    Stop,
}

/// 指令处理器持有的句柄（克隆其内部 control_tx）。
#[derive(Clone)]
pub struct PlayerHandle {
    tx: mpsc::Sender<Control>,
}

impl PlayerHandle {
    pub fn play(&self, r: Resolved) {
        let _ = self.tx.try_send(Control::Play(r));
    }
    pub fn skip(&self) {
        let _ = self.tx.try_send(Control::Skip);
    }
    pub fn stop(&self) {
        let _ = self.tx.try_send(Control::Stop);
    }
}

/// 供 `!queue` 只读的播放快照。
#[derive(Clone, Default)]
pub struct Snapshot {
    pub now_playing: Option<String>,
    pub upcoming: Vec<String>,
}

/// 正在播放的曲目：ffmpeg 子进程 + 帧读取器 + 展示名。
struct Current {
    _child: Child, // 持有以保活；drop 时 kill_on_drop 终止 ffmpeg
    reader: PcmFrameReader<ChildStdout>,
    label: String,
}

/// 队列播放引擎；driver 持 `&mut Player` 调 next_frame。
pub struct Player {
    rx: mpsc::Receiver<Control>,
    queue: VecDeque<Resolved>,
    current: Option<Current>,
    encoder: OpusMusicEncoder,
    snapshot: Arc<Mutex<Snapshot>>,
}

impl Player {
    /// 返回 player、配套句柄、只读快照。
    pub fn new() -> Result<(Player, PlayerHandle, Arc<Mutex<Snapshot>>)> {
        let (tx, rx) = mpsc::channel(32);
        let snapshot = Arc::new(Mutex::new(Snapshot::default()));
        let player = Player {
            rx,
            queue: VecDeque::new(),
            current: None,
            encoder: OpusMusicEncoder::new()?,
            snapshot: snapshot.clone(),
        };
        Ok((player, PlayerHandle { tx }, snapshot))
    }

    /// 排空 control 通道，更新队列与快照。同步、不 await（丢弃 Current 即杀 ffmpeg）。
    fn drain_control(&mut self) {
        while let Ok(c) = self.rx.try_recv() {
            match c {
                Control::Play(r) => self.queue.push_back(r),
                Control::Skip => self.current = None,
                Control::Stop => {
                    self.queue.clear();
                    self.current = None;
                }
            }
        }
        self.update_snapshot();
    }

    fn update_snapshot(&self) {
        let mut s = self.snapshot.lock().unwrap();
        s.now_playing = self.current.as_ref().map(|c| c.label.clone());
        s.upcoming = self.queue.iter().map(|r| r.label.clone()).collect();
    }
}

impl OpusSource for Player {
    async fn next_frame(&mut self) -> Result<Option<Vec<u8>>> {
        self.drain_control();
        loop {
            // 无当前曲目则尝试起播下一条
            if self.current.is_none() {
                match self.queue.pop_front() {
                    Some(r) => match spawn_ffmpeg(&r.input) {
                        Ok((child, stdout)) => {
                            self.current = Some(Current {
                                _child: child,
                                reader: PcmFrameReader::new(stdout),
                                label: r.label,
                            });
                            self.update_snapshot();
                        }
                        Err(e) => {
                            tracing::warn!(%e, "spawn ffmpeg 失败，跳过该曲");
                            self.update_snapshot();
                            continue;
                        }
                    },
                    None => return Ok(None), // 空闲：本 tick 无帧
                }
            }
            // 有当前曲目：取一帧
            let frame_opt = self.current.as_mut().unwrap().reader.next_frame().await?;
            match frame_opt {
                Some(frame) => return Ok(Some(self.encoder.encode(&frame)?.to_vec())),
                None => {
                    // 当前曲目结束，前进
                    self.current = None;
                    self.update_snapshot();
                    continue;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn res(label: &str) -> Resolved {
        Resolved { input: format!("/nonexistent/{label}"), label: label.to_string() }
    }

    #[tokio::test]
    async fn idle_player_yields_none() {
        let (mut player, _handle, _snap) = Player::new().unwrap();
        assert!(player.next_frame().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn play_enqueues_and_updates_snapshot() {
        let (mut player, handle, snap) = Player::new().unwrap();
        handle.play(res("A"));
        handle.play(res("B"));
        player.drain_control();
        let s = snap.lock().unwrap();
        assert_eq!(s.upcoming, vec!["A".to_string(), "B".to_string()]);
        assert!(s.now_playing.is_none());
    }

    #[tokio::test]
    async fn stop_clears_queue() {
        let (mut player, handle, snap) = Player::new().unwrap();
        handle.play(res("A"));
        player.drain_control();
        handle.stop();
        player.drain_control();
        assert!(snap.lock().unwrap().upcoming.is_empty());
    }
}
