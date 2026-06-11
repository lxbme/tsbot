# Phase 1 设计：薄端到端切片（可点播 + 常驻）

**日期**: 2026-06-11
**所属**: 业务逻辑 roadmap 的 Phase 1（见 `2026-06-11-business-logic-roadmap.md`）。

## 目标

频道内 `!play <url|path>` 入队 → 顺序播放 → `!skip` / `!stop` / `!queue`，**无权限**（任何人可用）。机器人**常驻**并在断线后**自动重连**。打通"指令 → 解析 → 队列 → 播放"整条链。

### 成功标准（人工验收）
- 频道发 `!play <本地文件>` 能听到声音；`!play <YouTube/直连流 URL>` 同样能播。
- `!skip` 切下一首；`!stop` 清空队列并停止但机器人**保持在频道**；`!queue` 回复当前+待播列表。
- 解析失败（如无效 URL）`!play` 立即回错误、不入队。
- 断网后机器人按退避自动重连，重连后队列保留。

### 非目标（仍按 roadmap）
权限系统、私聊/poke 指令、Web 面板、元数据标题、关键词搜索、队列持久化、多服务器。

## 音源与解析（已定）
- yt-dlp 走 `-g` 解析直连 URL；三类音源（yt-dlp 站点 / 直连流·电台 / 本地文件）统一交给 `ffmpeg -i <input>` 解码。`audio::spawn_ffmpeg` 本就是 `-i <string>`，URL/路径通用，**无需改动**。
- 解析在**入队时**发生：成功才入队并回复"已添加"，失败立即回错误。

## 新增子包与依赖

| 子包 | 职责 | 依赖 |
|------|------|------|
| `crates/source` | 把用户参数解析为可播条目 | tokio(process)、anyhow |
| `crates/player` | 队列 + 播放引擎，实现 `OpusSource` | source、tsbot-audio、ts-connection、tokio、anyhow |
| `crates/commands` | 指令解析 + 处理器 | player、source、ts-connection、tokio、anyhow |

依赖方向单向无环：`tsbot`(bin) → {commands, player, source} → {tsbot-audio, ts-connection, tsbot-config}；其中 player → source，commands → player + source。

## 接口设计

### source
```rust
/// 一个可交给 ffmpeg 播放的条目。
pub struct Resolved {
    pub input: String, // ffmpeg `-i` 的参数：本地路径 / 直连或解析后的媒体 URL
    pub label: String, // 队列展示用；Phase 1 = 原始请求串，标题留 Phase 2
}

/// 解析用户参数：
/// - 本地存在的路径 → input=路径
/// - http(s) → 跑 `yt-dlp -g <url>` 取直连 URL；yt-dlp 失败则回退用原始 URL（直连流/电台）
/// - 其它 → 错误
pub async fn resolve(arg: &str) -> anyhow::Result<Resolved>;
```

### player
```rust
/// 控制命令，经 control 通道从指令处理器发往 player。
pub enum Control { Play(Resolved), Skip, Stop }

/// 指令处理器持有的句柄（克隆其内部 control_tx）。
#[derive(Clone)]
pub struct PlayerHandle { /* control_tx: mpsc::Sender<Control> */ }
impl PlayerHandle {
    pub fn play(&self, r: Resolved);
    pub fn skip(&self);
    pub fn stop(&self);
}

/// 供 `!queue` 只读的播放快照。
#[derive(Clone, Default)]
pub struct Snapshot {
    pub now_playing: Option<String>, // 当前曲目 label
    pub upcoming: Vec<String>,       // 待播 label 列表
}

/// 队列播放引擎；driver 持 `&mut Player` 调 next_frame。
pub struct Player { /* control_rx, 当前解码管线, VecDeque<Resolved>, Arc<Mutex<Snapshot>> */ }

impl Player {
    /// 返回 player 与配套句柄、只读快照。
    pub fn new() -> (Player, PlayerHandle, std::sync::Arc<std::sync::Mutex<Snapshot>>);
}

impl ts_connection::OpusSource for Player {
    async fn next_frame(&mut self) -> anyhow::Result<Option<Vec<u8>>>;
}
```
`next_frame` 行为：① 用 `try_recv` 排空 control（Play 入队尾、Skip 丢弃当前、Stop 清空队列+丢弃当前），并更新快照；② 若当前无曲目且队列非空，弹出下一条、`audio::spawn_ffmpeg(&resolved.input)` 起解码，更新快照；③ 若有当前曲目，从其 `PcmFrameReader` 取一帧编码返回 `Some(bytes)`，曲目读到 EOF 则丢弃并前进；④ 若当前无曲目且队列空（空闲），返回 **`None`（空闲，非结束）**。

### ts-connection（演进）
```rust
/// 收到的频道文本消息，driver 转发给指令处理器。
pub struct ChatMessage {
    pub text: String,
    pub invoker_id: ClientId, // 用于过滤机器人自身消息
}

/// 驱动单条连接，直到断线返回 Err：
/// - 20ms 拉帧发送（source.next_frame；None=本 tick 不发，循环继续）
/// - 轮询事件：StreamItem::MessageEvent(InMessage::TextMessage) 且 target=Channel
///   且 invoker≠自身 → 构造 ChatMessage 送入 chat_tx
/// - drain reply_rx：对每条文本用 book 的 send_textmessage 发回机器人所在频道
pub async fn run<S: OpusSource>(
    con: &mut Connection,
    source: &mut S,
    chat_tx: &tokio::sync::mpsc::Sender<ChatMessage>,
    reply_rx: &mut tokio::sync::mpsc::Receiver<String>,
) -> Result<()>;

/// 常驻：connect → wait_until_ready → run；断线后指数退避重连，
/// 直到 shutdown future 触发。source/channels 跨重连复用。
pub async fn run_persistent<S: OpusSource>(
    settings: ConnectSettings,
    source: &mut S,
    chat_tx: tokio::sync::mpsc::Sender<ChatMessage>,
    reply_rx: tokio::sync::mpsc::Receiver<String>,
    shutdown: impl std::future::Future<Output = ()>,
) -> Result<()>;
```
`OpusSource::next_frame` 的 `None` 语义由 Phase 0 的"流结束"改为"**本 tick 空闲、无帧可发**"；driver 据此继续循环而非结束。旧 `stream_audio` 由 `run`/`run_persistent` 取代。`ClientId` 由 tsclientlib re-export。`ConnectSettings` 需 `#[derive(Clone)]`（其 `identity: Identity` 已是 Clone），以便 `run_persistent` 每次重连用克隆重建。

### commands
```rust
pub enum Command { Play(String), Skip, Stop, Queue }

/// 解析一行文本；非指令（不以 ! 开头或未知）返回 None。
pub fn parse(text: &str) -> Option<Command>;

/// 指令处理循环：读 chat_rx → parse → 执行 → 经 reply_tx 回复。
/// 不接触 con，可独立 spawn。
pub async fn run(
    mut chat_rx: tokio::sync::mpsc::Receiver<ts_connection::ChatMessage>,
    handle: player::PlayerHandle,
    snapshot: std::sync::Arc<std::sync::Mutex<player::Snapshot>>,
    reply_tx: tokio::sync::mpsc::Sender<String>,
);
```
处理：`Play(arg)` → `source::resolve(arg).await` → Ok 则 `handle.play(r)` 回"已添加: {label}"，Err 回"解析失败: {e}"；`Skip` → `handle.skip()` 回执；`Stop` → `handle.stop()` 回执；`Queue` → 读 snapshot 格式化回复。

### bin（tsbot）接线
```rust
// config.load（去掉 [playback] 段）
// identity + ConnectSettings 同前
let (mut player, handle, snapshot) = player::Player::new();
let (chat_tx, chat_rx) = mpsc::channel(32);
let (reply_tx, reply_rx) = mpsc::channel(32);

// 指令处理器：独立任务（不碰 con）
tokio::spawn(commands::run(chat_rx, handle, snapshot, reply_tx));

// 常驻驱动 + ctrl_c 关闭
let shutdown = async { let _ = tokio::signal::ctrl_c().await; };
ts_connection::run_persistent(settings, &mut player, chat_tx, reply_rx, shutdown).await?;
```

## 配置变更
删除 `[playback]` 段（不再启动即播单文件）。`config.example.toml` 相应更新；机器人连上后空闲待命，靠指令驱动。`[server]`/`[bot]` 不变。

## 错误处理
- `source::resolve` 失败 → 经回复告知用户，不影响其它播放。
- 单曲 `spawn_ffmpeg`/解码失败 → player 记日志、丢弃该曲、前进到下一条，不中断循环。
- 断线 → `run` 返回 Err，`run_persistent` 退避重连。
- 统一 `anyhow::Result` + `tracing` 日志。

## 测试策略
- `commands::parse`：单测各指令（`!play x`、`!skip`、`!stop`、`!queue`、非指令、大小写/空白）。
- `source::resolve`：单测本地路径分类（存在/不存在）；http 分支对 yt-dlp 调用做边界（可注入命令或在缺 yt-dlp 时跳过）。
- `player`：单测队列状态机——构造 Player，通过 control 通道发 Play/Skip/Stop，断言 Snapshot 变化；用注入的假输入源（如 `Cursor` 经 PcmFrameReader）验证 next_frame 在空闲返回 None、有曲目返回 Some。
- `run`/`run_persistent`/真实收发/重连：依赖服务器，人工验收覆盖。

## 待实现时核验的 tsclientlib 细节
1. `InMessage::TextMessage` 的字段：取出消息文本、target 模式、invoker `ClientId`。
2. 向机器人所在频道发文本：从 `con.get_state()` book 取频道对象调 `send_textmessage(text)` 再 `.send(&mut con)`（参考 `examples/sync.rs:65`）。
3. 机器人自身 `ClientId`：从 book 状态获取，用于过滤自身消息。
4. `Identity` 实现 `Clone`（重连重建 ConnectSettings 需要）。
