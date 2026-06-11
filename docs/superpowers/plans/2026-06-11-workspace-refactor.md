# Workspace 子包化 + TOML 配置 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把单 crate MVP 重构为 Cargo workspace 四子包（config / audio / ts-connection / tsbot-bin），把发送循环抽成 `ts-connection` 的 `stream_audio` driver + `OpusSource` trait，并将业务参数从 CLI 迁移到 `--config` 指定的 TOML 文件，运行行为保持与 MVP 一致。

**Architecture:** 自底向上渐进迁移，每个任务结束时 `cargo build --workspace` 与 `cargo test --workspace` 必须全绿。依赖单向无环：`tsbot`(bin) → {tsbot-config, tsbot-audio, ts-connection}；三库互不依赖。版本由根 `[workspace.dependencies]` 统一。

**Tech Stack:** Rust 2021 / Cargo workspace、tsclientlib 0.2、tsproto-packets 0.1、audiopus 0.3.0-rc.0、tokio、serde+toml、clap、anyhow、tracing。

**前置条件:** 当前在 `main` 分支、单 crate 状态（`src/*.rs` + 根 `Cargo.toml`），7 个单测通过，ffmpeg 已装。

---

## File Structure（最终态）

```
Cargo.toml                          # [workspace] + [workspace.dependencies]（根，无 [package]）
.gitignore                          # 追加 /config.toml
config.example.toml                 # 配置模板（入库）
crates/
  config/
    Cargo.toml                      # tsbot-config
    src/lib.rs                      # Config/Server/Bot/Playback + load() + 2 tests
  audio/
    Cargo.toml                      # tsbot-audio
    src/lib.rs                      # mod + re-export
    src/frame.rs                    # FRAME_SAMPLES, PcmFrameReader, spawn_ffmpeg + 3 tests
    src/encode.rs                   # OpusMusicEncoder + 1 test
  ts-connection/
    Cargo.toml                      # tsbot-ts-connection, [lib] name="ts_connection"
    src/lib.rs                      # connect/wait_until_ready/OpusSource/stream_audio + re-exports
    src/identity.rs                 # load_or_create + 1 test
  tsbot/
    Cargo.toml                      # tsbot (bin)
    src/main.rs                     # --config 解析 + 接线 + FileOpusSource + 1 test
```

迁移顺序：Task1 建 workspace 并把现有 bin 整体搬到 `crates/tsbot` → Task2 抽 audio → Task3 抽 ts-connection 并改写 main 发送路径 → Task4 抽 config 库 → Task5 把 main 配置切到 TOML → Task6 全量校验 + 文档。

---

## Task 1: 建立 workspace 骨架，bin 整体迁入 crates/tsbot

**Files:**
- Create: `Cargo.toml`（根，改为 workspace）
- Create: `crates/tsbot/Cargo.toml`
- Move: `src/*.rs` → `crates/tsbot/src/*.rs`

- [ ] **Step 1: 移动源码到 crates/tsbot/src**

```bash
mkdir -p crates/tsbot/src
git mv src/audio_source.rs crates/tsbot/src/audio_source.rs
git mv src/config.rs       crates/tsbot/src/config.rs
git mv src/identity_store.rs crates/tsbot/src/identity_store.rs
git mv src/opus_enc.rs     crates/tsbot/src/opus_enc.rs
git mv src/main.rs         crates/tsbot/src/main.rs
rmdir src 2>/dev/null || true
```

- [ ] **Step 2: 根 Cargo.toml 改为 workspace**

把根 `Cargo.toml` 整个替换为：

```toml
[workspace]
resolver = "2"
members = ["crates/tsbot"]

[workspace.dependencies]
tsclientlib = "0.2"
tsproto-packets = "0.1"
audiopus = "0.3.0-rc.0"
tokio = "1"
futures = "0.3"
clap = { version = "4", features = ["derive"] }
toml = "0.8"
serde = { version = "1", features = ["derive"] }
anyhow = "1"
tracing = "0.1"
tracing-subscriber = "0.3"
tempfile = "3"
```

- [ ] **Step 3: 写 crates/tsbot/Cargo.toml**

```toml
[package]
name = "tsbot"
version = "0.1.0"
edition = "2021"

[dependencies]
tsclientlib = { workspace = true }
tsproto-packets = { workspace = true }
audiopus = { workspace = true }
tokio = { workspace = true, features = ["macros", "rt-multi-thread", "process", "io-util", "time", "signal", "sync"] }
futures = { workspace = true }
clap = { workspace = true }
toml = { workspace = true }
anyhow = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }

[dev-dependencies]
tempfile = { workspace = true }
```

- [ ] **Step 4: 构建 + 全测试**

Run: `cargo build --workspace && cargo test --workspace 2>&1 | tail -6`
Expected: 编译成功；7 个测试全部 PASS（audio_source 3、opus_enc 1、identity_store 1、config 2）。

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor: 建立 workspace，bin 迁入 crates/tsbot"
```

---

## Task 2: 抽出 crates/audio 子包

**Files:**
- Create: `crates/audio/Cargo.toml`, `crates/audio/src/lib.rs`, `crates/audio/src/frame.rs`, `crates/audio/src/encode.rs`
- Delete: `crates/tsbot/src/audio_source.rs`, `crates/tsbot/src/opus_enc.rs`
- Modify: `Cargo.toml`（members）、`crates/tsbot/Cargo.toml`、`crates/tsbot/src/main.rs`

- [ ] **Step 1: 写 crates/audio/Cargo.toml**

```toml
[package]
name = "tsbot-audio"
version = "0.1.0"
edition = "2021"

[dependencies]
audiopus = { workspace = true }
tokio = { workspace = true, features = ["io-util", "process", "macros", "rt"] }
anyhow = { workspace = true }
```

- [ ] **Step 2: 写 crates/audio/src/frame.rs**（由 audio_source.rs 迁移，内容不变）

```rust
use std::process::Stdio;

use anyhow::Result;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};

/// 一帧 = 48000 / 50 = 960 个 f32 样本（48kHz 单声道 20ms）。
pub const FRAME_SAMPLES: usize = 960;
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

/// 启动 ffmpeg 把文件解码为 48kHz/单声道/f32le 裸 PCM，返回子进程与其 stdout。
pub fn spawn_ffmpeg(file: &str) -> Result<(Child, impl AsyncRead + Unpin)> {
    let mut child = Command::new("ffmpeg")
        .args([
            "-hide_banner", "-loglevel", "error",
            "-i", file,
            "-ac", "1", "-ar", "48000", "-f", "f32le", "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
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
```

- [ ] **Step 3: 写 crates/audio/src/encode.rs**（由 opus_enc.rs 迁移，import 改为 `crate::frame`）

```rust
use anyhow::Result;
use audiopus::coder::Encoder;
use audiopus::{Application, Channels, SampleRate};

use crate::frame::FRAME_SAMPLES;

/// 把一帧 48kHz 单声道 f32 PCM 编码为 Opus（音乐档）字节。
pub struct OpusMusicEncoder {
    enc: Encoder,
    buf: Vec<u8>,
}

impl OpusMusicEncoder {
    pub fn new() -> Result<Self> {
        let enc = Encoder::new(SampleRate::Hz48000, Channels::Mono, Application::Audio)?;
        Ok(Self { enc, buf: vec![0u8; 4000] })
    }

    /// 编码恰好 `FRAME_SAMPLES` 个样本，返回编码后的字节切片。
    pub fn encode(&mut self, frame: &[f32; FRAME_SAMPLES]) -> Result<&[u8]> {
        let len = self.enc.encode_float(frame, &mut self.buf)?;
        Ok(&self.buf[..len])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_silence_to_nonempty_opus() {
        let mut enc = OpusMusicEncoder::new().unwrap();
        let frame = [0f32; FRAME_SAMPLES];
        let out = enc.encode(&frame).unwrap();
        assert!(!out.is_empty());
    }
}
```

- [ ] **Step 4: 写 crates/audio/src/lib.rs**

```rust
mod encode;
mod frame;

pub use encode::OpusMusicEncoder;
pub use frame::{spawn_ffmpeg, PcmFrameReader, FRAME_SAMPLES};
```

- [ ] **Step 5: 删除旧文件并更新 workspace members**

```bash
git rm crates/tsbot/src/audio_source.rs crates/tsbot/src/opus_enc.rs
```

把根 `Cargo.toml` 的 members 改为：

```toml
members = ["crates/audio", "crates/tsbot"]
```

- [ ] **Step 6: 更新 crates/tsbot/Cargo.toml**（去掉 audiopus，加 tsbot-audio）

把 `[dependencies]` 中的 `audiopus = { workspace = true }` 这一行替换为：

```toml
tsbot-audio = { path = "../audio" }
```

- [ ] **Step 7: 更新 crates/tsbot/src/main.rs 的模块声明与 import**

把文件顶部的：

```rust
mod audio_source;
mod config;
mod identity_store;
mod opus_enc;
```

替换为：

```rust
mod config;
mod identity_store;
```

并把：

```rust
use audio_source::{spawn_ffmpeg, PcmFrameReader};
use config::Args;
use opus_enc::OpusMusicEncoder;
```

替换为：

```rust
use config::Args;
use tsbot_audio::{spawn_ffmpeg, OpusMusicEncoder, PcmFrameReader};
```

（main.rs 其余代码不变。）

- [ ] **Step 8: 构建 + 全测试**

Run: `cargo build --workspace && cargo test --workspace 2>&1 | tail -8`
Expected: 编译成功；测试 PASS：tsbot-audio 4（frame 3 + encode 1）、tsbot 3（config 2 + identity 1）。共 7。

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "refactor: 抽出 tsbot-audio 子包"
```

---

## Task 3: 抽出 crates/ts-connection 并改写 main 发送路径

**Files:**
- Create: `crates/ts-connection/Cargo.toml`, `crates/ts-connection/src/lib.rs`, `crates/ts-connection/src/identity.rs`
- Delete: `crates/tsbot/src/identity_store.rs`
- Modify: `Cargo.toml`（members）、`crates/tsbot/Cargo.toml`、`crates/tsbot/src/main.rs`

- [ ] **Step 1: 写 crates/ts-connection/Cargo.toml**

```toml
[package]
name = "tsbot-ts-connection"
version = "0.1.0"
edition = "2021"

[lib]
name = "ts_connection"

[dependencies]
tsclientlib = { workspace = true }
tsproto-packets = { workspace = true }
tokio = { workspace = true, features = ["time", "macros"] }
futures = { workspace = true }
toml = { workspace = true }
anyhow = { workspace = true }

[dev-dependencies]
tempfile = { workspace = true }
```

- [ ] **Step 2: 写 crates/ts-connection/src/identity.rs**（由 identity_store.rs 迁移，内容不变）

```rust
use std::path::Path;

use anyhow::Result;
use tsclientlib::Identity;

/// 若 `path` 存在则读取复用，否则生成新 identity 并写入。
pub fn load_or_create(path: &Path) -> Result<Identity> {
    if path.exists() {
        let s = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&s)?)
    } else {
        let id = Identity::create();
        std::fs::write(path, toml::to_string(&id)?)?;
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_then_reuses_same_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.toml");

        // 第一次：文件不存在 -> 生成并落盘
        let first = load_or_create(&path).unwrap();
        assert!(path.exists());

        // 第二次：从文件加载 -> 与第一次内容一致
        let second = load_or_create(&path).unwrap();
        assert_eq!(
            toml::to_string(&first).unwrap(),
            toml::to_string(&second).unwrap()
        );
    }
}
```

- [ ] **Step 3: 写 crates/ts-connection/src/lib.rs**

```rust
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
```

- [ ] **Step 4: 删除旧 identity_store 并更新 members**

```bash
git rm crates/tsbot/src/identity_store.rs
```

根 `Cargo.toml` members 改为：

```toml
members = ["crates/audio", "crates/ts-connection", "crates/tsbot"]
```

- [ ] **Step 5: 更新 crates/tsbot/Cargo.toml**

把 `[dependencies]` 替换为（去掉 tsclientlib/tsproto-packets/toml；tokio 精简为 bin 真正需要的 feature；加 ts-connection）：

```toml
[dependencies]
tsbot-audio = { path = "../audio" }
tsbot-ts-connection = { path = "../ts-connection" }
tokio = { workspace = true, features = ["macros", "rt-multi-thread", "signal", "io-util"] }
futures = { workspace = true }
clap = { workspace = true }
anyhow = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
```

（`[dev-dependencies] tempfile` 暂时保留不动——本任务后它在 bin 中已无用处，属无害的未使用 dev-dep，将在 Task 5 删除 `[dev-dependencies]` 时一并清理。）

- [ ] **Step 6: 改写 crates/tsbot/src/main.rs**（用 ts-connection 替换 connect/wait/发送循环，新增 FileOpusSource 适配器）

把整个 `crates/tsbot/src/main.rs` 替换为：

```rust
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
```

注意：`con.events()`、`con.disconnect()` 通过 ts-connection re-export 的 `Connection`/`DisconnectOptions` 类型可用，bin 不再直接依赖 tsclientlib。

- [ ] **Step 7: 构建 + 全测试**

Run: `cargo build --workspace && cargo test --workspace 2>&1 | tail -12`
Expected: 编译成功（仅 ts-connection 的 `connect`/`wait_until_ready`/`stream_audio` 因无 server 不被测试覆盖，但应无警告）。测试 PASS：tsbot-audio 4、ts-connection 1（identity）、tsbot 3（config 2 + FileOpusSource 1）。共 8。

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "refactor: 抽出 ts-connection（含 stream_audio driver + OpusSource），改写 main 发送路径"
```

---

## Task 4: 抽出 crates/config 子包

**Files:**
- Create: `crates/config/Cargo.toml`, `crates/config/src/lib.rs`
- Modify: `Cargo.toml`（members）

- [ ] **Step 1: 写 crates/config/Cargo.toml**

```toml
[package]
name = "tsbot-config"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = { workspace = true }
toml = { workspace = true }
anyhow = { workspace = true }

[dev-dependencies]
tempfile = { workspace = true }
```

- [ ] **Step 2: 写 crates/config/src/lib.rs（先写实现 + 测试，TDD 一次到位）**

```rust
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
```

- [ ] **Step 3: 更新 workspace members**

根 `Cargo.toml` members 改为：

```toml
members = ["crates/config", "crates/audio", "crates/ts-connection", "crates/tsbot"]
```

- [ ] **Step 4: 构建 + 测试该包**

Run: `cargo test -p tsbot-config 2>&1 | tail -8`
Expected: 2 个测试 PASS（loads_full_config、loads_config_without_optionals）。

- [ ] **Step 5: 全 workspace 测试**

Run: `cargo test --workspace 2>&1 | tail -12`
Expected: 全绿。当前总数 10（tsbot-audio 4、ts-connection 1、tsbot 3、tsbot-config 2）。注意此时 config 库尚未被 bin 使用——这是预期中间态，下个任务接线。

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "refactor: 抽出 tsbot-config 子包（Config + load）"
```

---

## Task 5: main 切换到 TOML 配置，弃用业务 CLI 参数

**Files:**
- Delete: `crates/tsbot/src/config.rs`
- Modify: `crates/tsbot/Cargo.toml`、`crates/tsbot/src/main.rs`
- Create: `config.example.toml`
- Modify: `.gitignore`

- [ ] **Step 1: 删除旧 clap 业务 Args 模块**

```bash
git rm crates/tsbot/src/config.rs
```

- [ ] **Step 2: 更新 crates/tsbot/Cargo.toml**（加 tsbot-config；移除不再需要的 tempfile dev-dep）

把 `[dependencies]` 顶部加入：

```toml
tsbot-config = { path = "../config" }
```

并删除整个 `[dev-dependencies]` 段（bin 不再有依赖 tempfile 的测试；FileOpusSource 测试只用 Cursor）。最终 `crates/tsbot/Cargo.toml`：

```toml
[package]
name = "tsbot"
version = "0.1.0"
edition = "2021"

[dependencies]
tsbot-config = { path = "../config" }
tsbot-audio = { path = "../audio" }
tsbot-ts-connection = { path = "../ts-connection" }
tokio = { workspace = true, features = ["macros", "rt-multi-thread", "signal", "io-util"] }
futures = { workspace = true }
clap = { workspace = true }
anyhow = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
```

- [ ] **Step 3: 改写 main.rs 的参数与接线**

把 `crates/tsbot/src/main.rs` 中——

顶部 import 段（`mod config;` 起到 `use config::Args;` 止）替换为：

```rust
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Parser;
use futures::prelude::*;
use tokio::io::AsyncRead;
use ts_connection::{ConnectSettings, DisconnectOptions, OpusSource};
use tsbot_audio::{spawn_ffmpeg, OpusMusicEncoder, PcmFrameReader};

/// 命令行参数：仅保留配置文件路径。
#[derive(Parser, Debug)]
#[command(about = "TS3 musicbot")]
struct Args {
    /// TOML 配置文件路径
    #[arg(short, long, default_value = "config.toml")]
    config: PathBuf,
}
```

`#[tokio::main] async fn main()` 函数体从开头到 `tracing::info!(...)` 之间替换为：

```rust
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    let config = tsbot_config::load(&args.config)?;

    // 1. identity（生成或复用）
    let identity = ts_connection::identity::load_or_create(Path::new(&config.bot.identity_path))?;

    // 2. 连接并等待就绪
    let settings = ConnectSettings {
        address: config.server.address,
        password: config.server.password,
        channel: config.server.channel,
        name: config.bot.name,
        identity,
    };
    let mut con = ts_connection::connect(settings)?;
    ts_connection::wait_until_ready(&mut con).await?;
    tracing::info!("connected, start streaming {}", config.playback.file);

    // 3. ffmpeg 源 + 编码器 → FileOpusSource
    let (mut child, stdout) = spawn_ffmpeg(&config.playback.file)?;
    let mut source = FileOpusSource::new(PcmFrameReader::new(stdout), OpusMusicEncoder::new()?);
```

其余部分（`FileOpusSource` struct/impl、`select!` 驱动、清理断开、`#[cfg(test)]` 模块）保持不变。

> 实施提示：最稳妥的做法是直接整体重写 `main.rs`——保留 Task 3 Step 6 中的 `FileOpusSource` 定义、`select!` 块、清理段、测试模块原样，只把"参数解析 + 配置读取 + 设置映射"三处按上面替换。完成后 `mod config;` 已删除，不再引用 `config::Args`。

- [ ] **Step 4: 写 config.example.toml**

```toml
[server]
address  = "ts.example.com:9987"
password = "secret"     # 可选；无密码服务器删除此行
channel  = "Lobby"      # 可选；不指定则留在默认频道

[bot]
name          = "tsbot"
identity_path = "identity.toml"

[playback]
file = "test.mp3"       # 连上后播放此文件，播完退出
```

- [ ] **Step 5: 更新 .gitignore**

在 `.gitignore` 追加一行（保留已有 `/target`、`/identity.toml`、`test.mp3`）：

```
/config.toml
```

- [ ] **Step 6: 构建 + 全测试**

Run: `cargo build --workspace && cargo test --workspace 2>&1 | tail -12`
Expected: 编译成功。测试 PASS 共 8：tsbot-audio 4、ts-connection 1、tsbot-config 2、tsbot 1（FileOpusSource）。旧的 2 个 clap 业务参数测试已随 config.rs 删除。

- [ ] **Step 7: 验证 --config 行为（不连真实服务器）**

Run: `cargo run -p tsbot -- --config /nonexistent.toml 2>&1 | head -3`
Expected: 程序读取配置文件失败并报错退出（anyhow 错误信息含找不到文件），证明 `--config` 路径已生效、不再要求业务参数。

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "refactor: main 改用 TOML 配置文件，弃用业务 CLI 参数"
```

---

## Task 6: 全量校验与运行文档

**Files:** 无代码改动（校验 + 文档）

- [ ] **Step 1: 全 workspace 构建/测试/lint**

Run: `cargo build --workspace 2>&1 | tail -5 && cargo test --workspace 2>&1 | tail -12 && cargo clippy --workspace 2>&1 | tail -20`
Expected: 构建无错误无警告；8 个测试全 PASS；clippy 无 lint（若 clippy 未安装则跳过该段）。

- [ ] **Step 2: 确认依赖方向无环**

Run: `cargo tree -p tsbot --depth 1 2>&1 | head -20`
Expected: tsbot 依赖 tsbot-config、tsbot-audio、tsbot-ts-connection；三个库不互相出现在彼此依赖中（人工确认 audio/config 不依赖 ts-connection 等）。

- [ ] **Step 3: 确认工作树干净、无配置/身份泄漏入库**

Run: `git status --porcelain && git ls-files | grep -E 'config.toml|identity.toml' || echo "无敏感文件入库"`
Expected: 工作树干净；`config.toml`/`identity.toml` 未被跟踪（仅 `config.example.toml` 入库）。

- [ ] **Step 4: 人工端到端验收说明（交付给用户执行）**

```bash
cp config.example.toml config.toml
# 编辑 config.toml 填入真实服务器地址/密码/频道与本地音频文件
cargo run -p tsbot -- --config config.toml
```
预期：机器人出现在频道、真人听得到、播完退出码 0。注意事项同前：ffmpeg 在 PATH、首次 identity 等级提升可能静默等待、机器人需有频道发言权限。

- [ ] **Step 5: Commit（若有文档/README 更新；无更改则跳过）**

```bash
git add -A
git commit -m "chore: workspace 重构全量校验" --allow-empty
```

---

## Self-Review 记录

- **Spec 覆盖**：四子包布局(Task1-5)、`--config`+TOML schema(Task5 + config.example.toml)、tsbot-config API(Task4)、tsbot-audio 迁移(Task2)、ts-connection connect/wait/OpusSource/stream_audio/identity(Task3)、FileOpusSource 留 bin + ctrl_c 留 bin(Task3/5)、依赖方向无环(Task6 Step2)、测试迁移分布(各任务 + Task6)、gitignore/example(Task5)、人工验收(Task6)——逐条对应。
- **占位符扫描**：无 TBD/TODO；每个代码步骤含完整代码与确切命令。
- **类型一致性**：`OpusSource::next_frame() -> Result<Option<Vec<u8>>>`、`FileOpusSource::new`、`ConnectSettings{address,password,channel,name,identity}`、`stream_audio<S: OpusSource>(&mut Connection, &mut S)`、`tsbot_config::load(&Path) -> Result<Config>`、`Config{server,bot,playback}` 字段名在 Task3/4/5 间一致；import 名 `ts_connection`（由 `[lib] name` 提供）、`tsbot_audio`、`tsbot_config` 全程一致。
- **风险点**：`#[allow(async_fn_in_trait)]` 已加于 OpusSource，避免公开 trait 的 async-fn 警告；tokio 各子包 feature 已按实际用途裁剪（audio: io-util/process/macros/rt；ts-connection: time/macros；bin: macros/rt-multi-thread/signal/io-util）。
- **中间态说明**：Task4 后 config 库短暂未被 bin 使用，Task5 接线——已在 Task4 Step5 标注为预期。
```
