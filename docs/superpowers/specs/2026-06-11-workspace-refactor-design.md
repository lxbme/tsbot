# 地基子包化 + TOML 配置 — 设计文档

**日期**: 2026-06-11
**目标**: 把现有单 crate 的 MVP 地基重构为 Cargo workspace 四子包，并把业务参数从 CLI 迁移到 TOML 配置文件，为后续业务逻辑（播放队列、yt-dlp 多音源、聊天指令）开发做准备。**运行行为与现 MVP 一致**（连上服务器→播放一个文件→退出）。

## 目标与非目标

### 目标
- 重构为 workspace 四子包，依赖方向单向无环，各子包可独立理解与测试。
- 业务参数（服务器地址/密码/频道/identity 路径/bot 名/播放文件）改由 TOML 配置文件提供；仅保留 `--config <path>` 一个 CLI 参数。
- 把发送循环（`select!` + 借用 dance）抽进 `ts-connection`，成为 `stream_audio` driver + `OpusSource` trait。
- 现有 7 个单元测试随模块迁移到对应子包并保持通过。

### 非目标（本次不做）
播放队列、yt-dlp/多音源、聊天指令、多服务器、断线重连。仅重构 + 配置迁移，不新增业务功能。

## Workspace 布局

```
tsbot/
├── Cargo.toml              # [workspace] + [workspace.dependencies]
├── config.example.toml     # 配置模板（入库）
├── config.toml             # 实际配置（gitignore）
├── .gitignore              # 追加 /config.toml
├── crates/
│   ├── config/             # 包名 tsbot-config   （import: tsbot_config）
│   ├── audio/              # 包名 tsbot-audio    （import: tsbot_audio）
│   ├── ts-connection/      # 包名 tsbot-ts-connection，[lib] name = "ts_connection"（import: ts_connection）
│   └── tsbot/              # 包名 tsbot（bin）
└── docs/
```

**依赖方向（单向无环）**：`tsbot`(bin) → `tsbot-config`、`tsbot-audio`、`tsbot-ts-connection`。三个库 crate 互不依赖。版本通过根 `[workspace.dependencies]` 统一声明，各子包用 `dep = { workspace = true }` 引用。

## 配置：定位与 Schema

**定位**：唯一 CLI 参数 `--config <path>`（clap），默认 `./config.toml`。

**Schema**（`config.example.toml`）：
```toml
[server]
address  = "ts.example.com:9987"
password = "secret"     # 可选；无密码服务器删除此行
channel  = "Lobby"      # 可选；不指定则留在默认频道

[bot]
name          = "tsbot"
identity_path = "identity.toml"

[playback]
file = "test.mp3"       # 连上后播放此文件，播完退出（MVP 行为）
```

**tsbot-config 公开 API**（纯数据，仅依赖 serde/toml/anyhow，不依赖任何 TS/audio 类型）：
```rust
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
pub fn load(path: &Path) -> anyhow::Result<Config>;
```

## tsbot-audio 公开 API

由现有 `audio_source.rs` + `opus_enc.rs` 原样迁移，逻辑不变。纯音频管线，无 TS 依赖，依赖 tokio/audiopus/anyhow。
```rust
pub const FRAME_SAMPLES: usize;                 // 960
pub struct PcmFrameReader<R> { ... }            // new(R) + async next_frame() -> Result<Option<[f32; FRAME_SAMPLES]>>
pub fn spawn_ffmpeg(file: &str) -> Result<(tokio::process::Child, impl AsyncRead + Unpin)>;
pub struct OpusMusicEncoder { ... }             // new() + encode(&[f32; FRAME_SAMPLES]) -> Result<&[u8]>
```
模块组织：crate 内分 `frame`（PcmFrameReader + spawn_ffmpeg + FRAME_SAMPLES）与 `encode`（OpusMusicEncoder）两个模块，在 `lib.rs` re-export。

## tsbot-ts-connection 公开 API

封装 tsclientlib 连接生命周期、identity、以及发送驱动。依赖 tsclientlib/tsproto-packets/tokio/futures/toml/anyhow。
```rust
// re-export，使 bin 无需直接依赖 tsclientlib
pub use tsclientlib::{Connection, DisconnectOptions, Identity, StreamItem};

pub mod identity {
    /// 存在则读取，否则生成并持久化（由 identity_store.rs 迁移而来）
    pub fn load_or_create(path: &Path) -> anyhow::Result<Identity>;
}

/// 建立连接所需的设置，由 bin 从 Config 映射而来。
pub struct ConnectSettings {
    pub address: String,
    pub password: Option<String>,
    pub channel: Option<String>,
    pub name: String,
    pub identity: Identity,
}

/// 用 ConnectSettings 构建 ConnectOptions 并 connect。
pub fn connect(settings: ConnectSettings) -> anyhow::Result<Connection>;

/// 等待 BookEvents，确认连接就绪。
pub async fn wait_until_ready(con: &mut Connection) -> anyhow::Result<()>;

/// 音频帧来源：被 stream_audio 按 20ms 拉取。
pub trait OpusSource {
    /// 返回下一帧已编码 opus 字节；None 表示流结束。
    async fn next_frame(&mut self) -> anyhow::Result<Option<Vec<u8>>>;
}

/// 驱动连接：轮询事件保活 + 20ms 节奏 + 拉帧发送，
/// 直到 source 返回 None（正常结束，发送停止包）或断线（返回 Err）。
pub async fn stream_audio<S: OpusSource>(con: &mut Connection, source: &mut S) -> anyhow::Result<()>;
```

**内部**（非公开）：`opus_music_packet(data: &[u8]) -> OutPacket`（包 `AudioData::C2S { id: 0, codec: CodecType::OpusMusic, data }`），供 `stream_audio` 发送帧与停止包使用。

**stream_audio 行为**（把原 main 循环搬入，去掉 ctrl_c 分支）：
```rust
let mut interval = interval(Duration::from_millis(20));
loop {
    let events = con.events().try_for_each(|_| future::ready(Ok(())));
    tokio::select! {
        _ = interval.tick() => {}
        r = events => { r?; bail!("Disconnected"); }
    }
    match source.next_frame().await? {
        Some(data) => con.send_audio(opus_music_packet(&data))?,
        None => break,
    }
}
let _ = con.send_audio(opus_music_packet(&[])); // 停止包
Ok(())
```
借用 dance（`events` future 在 select 选中 tick 分支后被 drop，释放 `con` 借用，随后 `send_audio`）封装在此函数内，bin 不再看到。

## tsbot (bin) 行为

```rust
// clap 仅解析 --config（默认 ./config.toml）
let args = Args::parse();
let config = tsbot_config::load(&args.config)?;

let identity = ts_connection::identity::load_or_create(Path::new(&config.bot.identity_path))?;
let settings = ConnectSettings {
    address: config.server.address,
    password: config.server.password,
    channel: config.server.channel,
    name: config.bot.name,
    identity,
};
let mut con = ts_connection::connect(settings)?;
ts_connection::wait_until_ready(&mut con).await?;

let (mut child, stdout) = tsbot_audio::spawn_ffmpeg(&config.playback.file)?;
let mut source = FileOpusSource::new(PcmFrameReader::new(stdout), OpusMusicEncoder::new()?);

tokio::select! {
    r = ts_connection::stream_audio(&mut con, &mut source) => r?,
    _ = tokio::signal::ctrl_c() => {}   // 停止信号属 app 层策略，留在 bin
}

let _ = child.kill().await;
con.disconnect(DisconnectOptions::new())?;
con.events().for_each(|_| future::ready(())).await;
```

**FileOpusSource**（适配器，定义在 bin 内 —— 它同时依赖 audio 类型与 ts-connection 的 trait，只有 bin 能同时见到二者，故放此处以保持库间不互相依赖；将来业务 crate 出现后可上移）：
```rust
struct FileOpusSource<R> { reader: PcmFrameReader<R>, encoder: OpusMusicEncoder }
impl<R: AsyncRead + Unpin> OpusSource for FileOpusSource<R> {
    async fn next_frame(&mut self) -> Result<Option<Vec<u8>>> {
        match self.reader.next_frame().await? {
            Some(frame) => Ok(Some(self.encoder.encode(&frame)?.to_vec())),
            None => Ok(None),
        }
    }
}
```
`to_vec()`：为规避 async-fn-in-trait 返回借用 encoder 内部 buffer 的生命周期复杂度，每帧返回 owned `Vec<u8>`（约 50 次/秒小分配，开销可忽略）。

## 错误处理

各 crate 统一 `anyhow::Result` + `?`（与现状一致；库 crate 暂不引入 thiserror，YAGNI）。`tracing` 日志保留在 bin 的 `main` 初始化。

## 测试与迁移策略

**迁移方式**：搬移代码而非重写。逐子包建立后 `cargo test --workspace` 全绿。

**测试分布**：
- `tsbot-audio`：迁移现有 4 个测试（PcmFrameReader 3 + OpusMusicEncoder 1）。
- `tsbot-ts-connection`：迁移 `identity::load_or_create` 的 1 个测试（tempdir 往返）。
- `tsbot-config`：**新增** 1 个测试——写一个完整 TOML 到 tempfile，`load` 后断言所有字段（含 optional 有值/缺省两种）。
- `tsbot`(bin)：**新增** 1 个测试——`FileOpusSource` 适配器：用 `Cursor`/slice 喂 PCM 字节，断言 `next_frame` 先产出非空 opus 字节、耗尽后产出 `None`。
- `stream_audio` 与 `connect`/`wait_until_ready` 依赖真实 `Connection`，无法脱机单测，由人工端到端验证覆盖（同 MVP）。

**人工验收**（与现状等价）：
```bash
cp config.example.toml config.toml   # 填入真实服务器与文件
cargo run -p tsbot -- --config config.toml
```
预期：bot 出现在频道、真人听得到、播完退出码 0。

**gitignore**：追加 `/config.toml`；保留 `/identity.toml`、`/target`、`test.mp3`。提交 `config.example.toml`。

## 风险

- **async fn in trait**：Rust 1.94 已稳定支持（1.75+）。`OpusSource::next_frame` 返回 owned `Vec<u8>` 规避借用生命周期问题，无需 `async-trait` 宏。
- **版本一致性**：workspace 化后所有 crate 共享 `[workspace.dependencies]`，避免子包间版本漂移；`tsproto-packets` 锁 `0.1`、`audiopus` 锁 `0.3.0-rc.0`（与 MVP 一致）。
- **trait 形状前置**：在业务逻辑落地前定下 `OpusSource` 拉取契约；拉取模型对队列/跳过/暂停均可在 source 内部消化，driver 大概率无需改动（已评估为低风险）。
