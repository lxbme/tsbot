use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use rand::seq::SliceRandom;
use tokio::process::{Child, ChildStdout};
use tokio::sync::mpsc;

use source::Resolved;
use tsbot_audio::{spawn_ffmpeg, OpusMusicEncoder, PcmFrameReader};
use ts_connection::OpusSource;

/// 每帧 20ms。
const FRAME_MS: u64 = 20;
/// 每隔多少帧把进度写入快照（25 帧 ≈ 0.5s）。
const SNAPSHOT_EVERY: u64 = 25;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum LoopMode {
    #[default]
    Off,
    Track,
    Queue,
}

/// 控制命令，经 control 通道从指令处理器发往 player。
pub enum Control {
    Play(Resolved),
    Skip,
    Stop,
    Pause,
    Resume,
    SetVolume(u8),
    SetLoop(LoopMode),
    Remove(usize),
    Clear,
    Shuffle,
}

/// 指令处理器持有的句柄。
#[derive(Clone)]
pub struct PlayerHandle {
    tx: mpsc::Sender<Control>,
}

impl PlayerHandle {
    pub fn play(&self, r: Resolved) { let _ = self.tx.try_send(Control::Play(r)); }
    pub fn skip(&self) { let _ = self.tx.try_send(Control::Skip); }
    pub fn stop(&self) { let _ = self.tx.try_send(Control::Stop); }
    pub fn pause(&self) { let _ = self.tx.try_send(Control::Pause); }
    pub fn resume(&self) { let _ = self.tx.try_send(Control::Resume); }
    pub fn set_volume(&self, v: u8) { let _ = self.tx.try_send(Control::SetVolume(v)); }
    pub fn set_loop(&self, m: LoopMode) { let _ = self.tx.try_send(Control::SetLoop(m)); }
    pub fn remove(&self, n: usize) { let _ = self.tx.try_send(Control::Remove(n)); }
    pub fn clear(&self) { let _ = self.tx.try_send(Control::Clear); }
    pub fn shuffle(&self) { let _ = self.tx.try_send(Control::Shuffle); }
}

#[derive(Clone)]
pub struct NowPlaying {
    pub title: String,
    pub elapsed: Duration,
    pub duration: Option<Duration>,
    pub request: String,
}

#[derive(Clone)]
pub struct QueueItem {
    pub title: String,
    pub duration: Option<Duration>,
    pub request: String,
}

/// 供查询命令只读的播放快照。
#[derive(Clone, Default)]
pub struct Snapshot {
    pub now_playing: Option<NowPlaying>,
    pub upcoming: Vec<QueueItem>,
    pub volume: u8,
    pub loop_mode: LoopMode,
    pub paused: bool,
}

/// 正在播放的曲目。
struct Current {
    _child: Child,
    reader: PcmFrameReader<ChildStdout>,
    resolved: Resolved,
    played_frames: u64,
}

pub struct Player {
    rx: mpsc::Receiver<Control>,
    queue: VecDeque<Resolved>,
    current: Option<Current>,
    encoder: OpusMusicEncoder,
    snapshot: Arc<Mutex<Snapshot>>,
    volume: u8,
    paused: bool,
    loop_mode: LoopMode,
    frames_since_snapshot: u64,
}

impl Player {
    pub fn new() -> Result<(Player, PlayerHandle, Arc<Mutex<Snapshot>>)> {
        let (tx, rx) = mpsc::channel(32);
        let snapshot = Arc::new(Mutex::new(Snapshot { volume: 100, ..Default::default() }));
        let player = Player {
            rx,
            queue: VecDeque::new(),
            current: None,
            encoder: OpusMusicEncoder::new()?,
            snapshot: snapshot.clone(),
            volume: 100,
            paused: false,
            loop_mode: LoopMode::Off,
            frames_since_snapshot: 0,
        };
        Ok((player, PlayerHandle { tx }, snapshot))
    }

    fn drain_control(&mut self) {
        while let Ok(c) = self.rx.try_recv() {
            match c {
                Control::Play(r) => self.queue.push_back(r),
                Control::Skip => self.current = None,
                Control::Stop => {
                    self.queue.clear();
                    self.current = None;
                }
                Control::Pause => self.paused = true,
                Control::Resume => self.paused = false,
                Control::SetVolume(v) => self.volume = v.min(100),
                Control::SetLoop(m) => self.loop_mode = m,
                Control::Remove(n) => {
                    if n >= 1 && n <= self.queue.len() {
                        self.queue.remove(n - 1);
                    }
                }
                Control::Clear => self.queue.clear(),
                Control::Shuffle => {
                    self.queue.make_contiguous().shuffle(&mut rand::rng());
                }
            }
        }
        self.update_snapshot();
    }

    fn update_snapshot(&self) {
        let mut s = self.snapshot.lock().unwrap();
        s.now_playing = self.current.as_ref().map(|c| NowPlaying {
            title: c.resolved.title.clone(),
            elapsed: Duration::from_millis(c.played_frames * FRAME_MS),
            duration: c.resolved.duration,
            request: c.resolved.request.clone(),
        });
        s.upcoming = self
            .queue
            .iter()
            .map(|r| QueueItem { title: r.title.clone(), duration: r.duration, request: r.request.clone() })
            .collect();
        s.volume = self.volume;
        s.loop_mode = self.loop_mode;
        s.paused = self.paused;
    }

    /// 起播一首（失败则记录并保持 current=None）。
    fn start_track(&mut self, r: Resolved) {
        match spawn_ffmpeg(&r.input) {
            Ok((child, stdout)) => {
                self.current = Some(Current {
                    _child: child,
                    reader: PcmFrameReader::new(stdout),
                    resolved: r,
                    played_frames: 0,
                });
            }
            Err(e) => {
                tracing::warn!(%e, title = %r.title, "spawn ffmpeg 失败，跳过");
            }
        }
        self.update_snapshot();
    }

    /// 当前曲目结束，按循环模式处理。
    fn on_track_end(&mut self) {
        let finished = self.current.take();
        match self.loop_mode {
            LoopMode::Track => {
                if let Some(c) = finished {
                    self.start_track(c.resolved);
                }
            }
            LoopMode::Queue => {
                if let Some(c) = finished {
                    self.queue.push_back(c.resolved);
                }
            }
            LoopMode::Off => {}
        }
        self.update_snapshot();
    }
}

impl OpusSource for Player {
    async fn next_frame(&mut self) -> Result<Option<Vec<u8>>> {
        self.drain_control();
        if self.paused {
            return Ok(None);
        }
        loop {
            if self.current.is_none() {
                match self.queue.pop_front() {
                    Some(r) => self.start_track(r),
                    None => return Ok(None),
                }
                if self.current.is_none() {
                    continue;
                }
            }
            let frame_opt = self.current.as_mut().unwrap().reader.next_frame().await?;
            match frame_opt {
                Some(frame) => {
                    {
                        let cur = self.current.as_mut().unwrap();
                        cur.played_frames += 1;
                    }
                    let data = if self.volume >= 100 {
                        self.encoder.encode(&frame)?.to_vec()
                    } else {
                        let gain = self.volume as f32 / 100.0;
                        let mut scaled = frame;
                        for s in scaled.iter_mut() {
                            *s *= gain;
                        }
                        self.encoder.encode(&scaled)?.to_vec()
                    };
                    self.frames_since_snapshot += 1;
                    if self.frames_since_snapshot >= SNAPSHOT_EVERY {
                        self.frames_since_snapshot = 0;
                        self.update_snapshot();
                    }
                    return Ok(Some(data));
                }
                None => {
                    self.on_track_end();
                    continue;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn res(title: &str) -> Resolved {
        Resolved { input: format!("/nonexistent/{title}"), title: title.to_string(), duration: None, request: title.to_string() }
    }

    #[tokio::test]
    async fn idle_player_yields_none() {
        let (mut player, _h, _s) = Player::new().unwrap();
        assert!(player.next_frame().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn play_enqueues_and_snapshot_titles() {
        let (mut player, handle, snap) = Player::new().unwrap();
        handle.play(res("A"));
        handle.play(res("B"));
        player.drain_control();
        let s = snap.lock().unwrap();
        let titles: Vec<_> = s.upcoming.iter().map(|q| q.title.clone()).collect();
        assert_eq!(titles, vec!["A".to_string(), "B".to_string()]);
        assert_eq!(s.volume, 100);
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

    #[tokio::test]
    async fn volume_clamped_and_reflected() {
        let (mut player, handle, snap) = Player::new().unwrap();
        handle.set_volume(150);
        player.drain_control();
        assert_eq!(snap.lock().unwrap().volume, 100);
        handle.set_volume(40);
        player.drain_control();
        assert_eq!(snap.lock().unwrap().volume, 40);
    }

    #[tokio::test]
    async fn loop_and_pause_reflected() {
        let (mut player, handle, snap) = Player::new().unwrap();
        handle.set_loop(LoopMode::Queue);
        handle.pause();
        player.drain_control();
        {
            let s = snap.lock().unwrap();
            assert_eq!(s.loop_mode, LoopMode::Queue);
            assert!(s.paused);
        }
        handle.play(res("A"));
        player.drain_control();
        assert!(player.next_frame().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn remove_by_index() {
        let (mut player, handle, snap) = Player::new().unwrap();
        for t in ["A", "B", "C"] {
            handle.play(res(t));
        }
        player.drain_control();
        handle.remove(2);
        player.drain_control();
        let titles: Vec<_> = snap.lock().unwrap().upcoming.iter().map(|q| q.title.clone()).collect();
        assert_eq!(titles, vec!["A".to_string(), "C".to_string()]);
        handle.remove(9);
        player.drain_control();
        assert_eq!(snap.lock().unwrap().upcoming.len(), 2);
    }

    #[tokio::test]
    async fn clear_empties_queue() {
        let (mut player, handle, snap) = Player::new().unwrap();
        handle.play(res("A"));
        handle.play(res("B"));
        player.drain_control();
        handle.clear();
        player.drain_control();
        assert!(snap.lock().unwrap().upcoming.is_empty());
    }

    #[tokio::test]
    async fn shuffle_preserves_set() {
        let (mut player, handle, snap) = Player::new().unwrap();
        for t in ["A", "B", "C", "D", "E"] {
            handle.play(res(t));
        }
        player.drain_control();
        handle.shuffle();
        player.drain_control();
        let mut titles: Vec<_> = snap.lock().unwrap().upcoming.iter().map(|q| q.title.clone()).collect();
        titles.sort();
        assert_eq!(titles, vec!["A", "B", "C", "D", "E"]);
    }
}
