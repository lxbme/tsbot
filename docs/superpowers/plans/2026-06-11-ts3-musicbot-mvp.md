# TS3 Musicbot MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让一个 Rust 程序以 TS3 客户端身份连上指定服务器，把一个本地音频文件解码、编码成 Opus 推送到频道，让同频道真人能听到，播完干净退出。

**Architecture:** 单二进制、`#[tokio::main]` 单任务内联驱动（不 spawn，规避 `Connection` 的 `!Send`）。ffmpeg 子进程把文件解成 48kHz/mono/f32le PCM，一个 20ms 定时器每 tick 读 960 样本一帧 → audiopus 编码 Opus → `con.send_audio()`。事件流与发送用 `tokio::select!` 协调借用。

**Tech Stack:** `tsclientlib` 0.2（TS3 协议）、`tsproto-packets`（音频包类型）、`audiopus`（Opus 编码）、`tokio`（运行时/子进程/定时器）、`clap`（CLI）、`toml`（identity 持久化）、`anyhow`/`tracing`。

**外部前置条件:** 系统已装 `ffmpeg`（已确认 `/usr/bin/ffmpeg`）。一个可连的 TS3 服务器（用于 Task 7 人工验证）。audiopus 链接 libopus；若构建报缺 opus，装系统 `libopus`/`opus-dev` 或开启 audiopus bundled feature。

---

## File Structure

| 文件 | 职责 |
|------|------|
| `Cargo.toml` | 依赖与包元数据 |
| `src/main.rs` | 入口：解析参数、连接、驱动播放循环、断开 |
| `src/config.rs` | `Args`（clap）CLI 参数定义与解析 |
| `src/identity_store.rs` | identity 的 `load_or_create`（toml 持久化） |
| `src/audio_source.rs` | `PcmFrameReader`：从任意 `AsyncRead` 产出定长 f32 帧 + 启动 ffmpeg 的辅助函数 |
| `src/opus_enc.rs` | `OpusMusicEncoder`：把一帧 f32 编码成 Opus 字节 |

纯逻辑单元（`audio_source`、`opus_enc`、`identity_store`、`config`）走 TDD 单元测试；连接与发送循环（`main.rs`）无法脱离真实服务器单测，作为 Task 7 的人工/集成验证。

---

## Task 1: 项目脚手架

**Files:**
- Create: `Cargo.toml`
- Create: `src/main.rs`

- [ ] **Step 1: 写 Cargo.toml**

```toml
[package]
name = "tsbot"
version = "0.1.0"
edition = "2021"

[dependencies]
tsclientlib = "0.2"
tsproto-packets = "0.2"
audiopus = "0.3.0-rc.0"
tokio = { version = "1", features = ["macros", "rt-multi-thread", "process", "io-util", "time", "signal", "sync"] }
futures = "0.3"
clap = { version = "4", features = ["derive"] }
toml = "0.8"
anyhow = "1"
tracing = "0.1"
tracing-subscriber = "0.3"

[dev-dependencies]
tempfile = "3"
```

> 注：若 `cargo build` 因 `tsproto-packets`/`audiopus` 版本与 `tsclientlib 0.2` 解析冲突，运行 `cargo tree -i tsproto-packets` 查看 tsclientlib 锁定的版本并对齐到同一版本号。

- [ ] **Step 2: 写最小 main.rs**

```rust
fn main() {
    println!("tsbot");
}
```

- [ ] **Step 3: 构建验证**

Run: `cargo build`
Expected: 编译成功（首次会拉取并编译 tsclientlib，耗时较长）。

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock src/main.rs
git commit -m "chore: 项目脚手架与依赖"
```

---

## Task 2: CLI 参数（config 模块）

**Files:**
- Create: `src/config.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: 写失败测试**

在 `src/config.rs`：

```rust
use clap::Parser;

#[derive(Parser, Debug)]
#[command(about = "TS3 musicbot MVP")]
pub struct Args {
    /// TS3 服务器地址 (host 或 host:port)
    #[arg(short, long)]
    pub address: String,
    /// 要播放的本地音频文件路径
    #[arg(short, long)]
    pub file: String,
    /// 服务器密码（可选）
    #[arg(short, long)]
    pub password: Option<String>,
    /// 连接后切入的频道名/路径（可选）
    #[arg(short, long)]
    pub channel: Option<String>,
    /// identity 持久化文件路径
    #[arg(short, long, default_value = "identity.toml")]
    pub identity: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_required_and_optional_args() {
        let args = Args::try_parse_from([
            "tsbot", "--address", "ts.example.com:9987", "--file", "song.mp3",
            "--password", "secret", "--channel", "Lobby",
        ])
        .unwrap();
        assert_eq!(args.address, "ts.example.com:9987");
        assert_eq!(args.file, "song.mp3");
        assert_eq!(args.password.as_deref(), Some("secret"));
        assert_eq!(args.channel.as_deref(), Some("Lobby"));
        assert_eq!(args.identity, "identity.toml");
    }

    #[test]
    fn missing_required_args_fails() {
        let res = Args::try_parse_from(["tsbot", "--address", "x"]);
        assert!(res.is_err());
    }
}
```

在 `src/main.rs` 顶部加 `mod config;` 使其参与编译：

```rust
mod config;

fn main() {
    println!("tsbot");
}
```

- [ ] **Step 2: 运行测试，确认失败/通过**

Run: `cargo test config::`
Expected: 测试通过（clap derive 已实现行为）。若编译错误（如未加 `mod config;`），先修正使其编译并通过。

- [ ] **Step 3: Commit**

```bash
git add src/config.rs src/main.rs
git commit -m "feat: CLI 参数解析"
```

---

## Task 3: identity 持久化（identity_store 模块）

**Files:**
- Create: `src/identity_store.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: 写失败测试**

在 `src/identity_store.rs`：

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

在 `src/main.rs` 加 `mod identity_store;`。

- [ ] **Step 2: 运行测试，确认通过**

Run: `cargo test identity_store::`
Expected: PASS（生成文件、二次加载内容一致）。

- [ ] **Step 3: Commit**

```bash
git add src/identity_store.rs src/main.rs
git commit -m "feat: identity 持久化 load_or_create"
```

---

## Task 4: PCM 分帧（audio_source 模块）

**Files:**
- Create: `src/audio_source.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: 写失败测试**

在 `src/audio_source.rs`：

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

在 `src/main.rs` 加 `mod audio_source;`。

- [ ] **Step 2: 运行测试，确认失败再通过**

Run: `cargo test audio_source::`
Expected: 三个测试全部 PASS。若先红，按实现修正分帧/补零逻辑直至通过。

- [ ] **Step 3: Commit**

```bash
git add src/audio_source.rs src/main.rs
git commit -m "feat: PCM 分帧 PcmFrameReader 与 ffmpeg 启动"
```

---

## Task 5: Opus 编码（opus_enc 模块）

**Files:**
- Create: `src/opus_enc.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: 写失败测试**

在 `src/opus_enc.rs`：

```rust
use anyhow::Result;
use audiopus::coder::Encoder;
use audiopus::{Application, Channels, SampleRate};

use crate::audio_source::FRAME_SAMPLES;

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

在 `src/main.rs` 加 `mod opus_enc;`。

- [ ] **Step 2: 运行测试，确认通过**

Run: `cargo test opus_enc::`
Expected: PASS（静音帧编码出非空 Opus 字节）。若链接报缺 libopus，参见计划顶部前置条件说明。

- [ ] **Step 3: Commit**

```bash
git add src/opus_enc.rs src/main.rs
git commit -m "feat: Opus 音乐档编码 OpusMusicEncoder"
```

---

## Task 6: 连接与播放主循环（main.rs）

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: 实现完整 main**

替换 `src/main.rs` 全文为：

```rust
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
```

- [ ] **Step 2: 构建**

Run: `cargo build`
Expected: 编译成功。若 `OutAudio::new`/`AudioData`/`send_audio` 签名报错，运行 `cargo doc -p tsproto-packets --open` 与 `cargo doc -p tsclientlib --open` 核对实际签名并对齐（本计划基于 0.2 版本示例编写）。

- [ ] **Step 3: Commit**

```bash
git add src/main.rs
git commit -m "feat: 连接 TS3 并推流播放本地文件"
```

---

## Task 7: 端到端人工验证

**Files:** 无（运行验证）

> 这是 MVP 的成功判定，需对接真实 TS3 服务器，由人耳确认。

- [ ] **Step 1: 准备一个测试音频文件**

确保有一个本地音频文件，例如 `~/test.mp3`（任意可被 ffmpeg 解码的音频）。

- [ ] **Step 2: 运行机器人连接服务器**

Run（替换为真实参数）：
```bash
RUST_LOG=info cargo run -- \
  --address ts.example.com:9987 \
  --file ~/test.mp3 \
  --channel Lobby
```
Expected 日志：出现 `connected, start streaming ...`，进程持续运行直到文件播完。

- [ ] **Step 3: 用真人 TS3 客户端在同频道收听**

用普通 TeamSpeak3 客户端连同一服务器、进同一频道（如 `Lobby`）。
Expected 观察：
- 机器人 `tsbot` 出现在该频道的客户端列表中。
- 能**听到** `test.mp3` 的声音，连续、不明显卡顿。
- 文件播完后机器人自动从频道消失、进程退出码为 0。

- [ ] **Step 4: 记录结果**

若三项全部满足 → MVP 验证通过。若失败，按现象排查：
- 听不到声音但机器人在线 → 检查 `CodecType`/帧大小/服务器是否允许该客户端发言权限。
- 声音卡顿 → 检查 20ms 定时器是否被 ffmpeg 读阻塞（确认 ffmpeg 比实时快产出）。
- 连不上 → 检查地址/密码/identity level（服务器可能要求更高 identity 等级，日志会报 `IdentityLevel`）。

---

## Self-Review 记录

- **Spec 覆盖**：连接(Task6)、identity 持久化(Task3)、ffmpeg→PCM(Task4)、20ms/Opus 编码发送(Task5+6)、CLI 传参(Task2)、播完退出(Task6)、人工"听得到"判定(Task7)——逐条对应，无遗漏。非目标（队列/指令/yt-dlp/重连）未引入。
- **占位符扫描**：无 TBD/TODO；每个代码步骤含完整代码与确切命令。
- **类型一致性**：`FRAME_SAMPLES` 定义于 audio_source，被 opus_enc 与 main 引用一致；`PcmFrameReader::next_frame`、`OpusMusicEncoder::encode`、`spawn_ffmpeg`、`load_or_create`、`Args` 字段在各任务间签名一致。`AudioData::C2S { id, codec, data }` 与 `OutAudio::new` 用法与 tsclientlib 0.2 示例一致。
- **风险提示**：`tsproto-packets`/`audiopus` 版本对齐、libopus 链接、identity 等级要求——已在计划顶部与 Task6/7 注明核对方式。
