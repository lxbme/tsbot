# Phase 2A 设计：播放控制加厚

**日期**: 2026-06-11
**所属**: 业务逻辑 roadmap 的 Phase 2，拆为 **2A（播放控制加厚，本文档）** 与 **2B（播放列表，后续单独 spec）**。

## 目标

在 Phase 1（点播/队列/常驻）基础上，加厚播放控制与用户体验：暂停/继续、音量、循环、当前曲目（带进度）、移除/清空、打乱、批量点播、`!help`，以及曲目**元数据（标题/时长）**。强调良好的频道交互体验。

### 非目标（明确不做）
- 播放列表（命名歌单持久化）→ Phase 2B
- 权限（Phase 3）、私聊/poke（Phase 4）、Web（Phase 5）
- 关键词搜索、队列重启持久化、多服务器
- 命令短别名（本期不做）

## 命令集与回复风格

回复发到机器人所在频道，**BBCode 轻美化 + 简洁**：关键信息（曲名等）用 `[b]…[/b]`，每个动作一行短确认。查询类（queue/nowplaying/help）与错误必回；动作类也回简短确认。

| 命令 | 参数 | 语义 | 回复示例 |
|------|------|------|----------|
| `!play` | `<url/路径> [更多…]` | 解析并入队，**支持空格分隔批量** | 单个：`已添加 [b]<标题>[/b]（3:32）`；批量：`已添加 3 首，1 个失败` |
| `!pause` | — | 暂停（保持连接与当前曲目） | `已暂停` |
| `!resume` | — | 继续 | `已继续` |
| `!volume` | `[0-100]` | 设音量；无参显示当前 | `音量：[b]70[/b]` |
| `!loop` | `[off\|track\|queue]` | 设循环模式；无参显示当前 | `循环：[b]单曲[/b]` |
| `!nowplaying` | — | 当前曲目 + 进度 + 状态 | `[b]<标题>[/b]  1:23 / 4:05  ♪音量70 循环关` |
| `!queue` | — | 当前 + 待播（标题、时长） | `▶ [b]<当前>[/b] (4:05)` 换行 `1. <下一首> (3:10)` … |
| `!skip` | — | 跳过当前 | `已跳过` |
| `!remove` | `<n>` | 删除待播第 n 首（1 起，对应 !queue 编号） | `已移除 [b]<标题>[/b]` / `队列里没有第 n 首` |
| `!clear` | — | 清空**待播**队列，保留当前继续播放 | `已清空待播队列（N 首）` |
| `!stop` | — | 停当前 + 清空（Phase 1 行为不变） | `已停止` |
| `!shuffle` | — | 打乱待播队列（不动当前） | `已打乱 N 首` |
| `!help` | — | 列出所有命令及用法 | 多行命令清单 |

未识别命令 / 参数非法 → 简短错误回复（如 `音量需在 0-100 之间`）。

## 子包改动

### source —— 元数据
`Resolved` 由 `{ input, label }` 改为：
```rust
pub struct Resolved {
    pub input: String,            // ffmpeg -i 的输入（路径/直连URL）
    pub title: String,            // 展示标题
    pub duration: Option<Duration>, // 总时长；直播/未知为 None
}
```
- **yt-dlp**：一次调用取 标题 + 时长 + 直连 URL（`--print` 模板组合，具体格式在实现计划中核验；解析失败回退：title=原URL、duration=None、input=原URL）。
- **本地文件**：`ffprobe` 取时长与 `format_tags=title`；无标题标签则用文件名（不含目录）。失败则 title=路径、duration=None。
- **直连流**：title=URL，duration=None。

### player —— 控制状态与播放引擎
新增/扩展状态（均为 `Player` 字段，跨曲目持久）：
- `volume: u8`（0-100，默认 100）
- `paused: bool`（默认 false）
- `loop_mode: LoopMode { Off, Track, Queue }`（默认 Off）
- 当前曲目改为保存完整 `Resolved`（用于循环重播/重入队）+ 已播帧计数（算 elapsed）

控制枚举扩展：
```rust
pub enum Control {
    Play(Resolved), Skip, Stop,
    Pause, Resume,
    SetVolume(u8), SetLoop(LoopMode),
    Remove(usize), Clear, Shuffle,
}
```

`next_frame` 行为变更：
1. drain control（应用上述各操作，更新快照）。
2. 若 `paused` → 返回 `None`（空闲，**不丢弃当前 reader**）。
3. 取帧逻辑同 Phase 1；取到帧后**按音量缩放**（每个 f32 样本 × `volume as f32 / 100.0`）再编码返回。每帧累加 elapsed 帧计数。
4. 曲目结束（reader 返回 None）：
   - `Track` → 用当前 `Resolved` 重新 `spawn_ffmpeg` 重播（elapsed 归零）。
   - `Queue` → 把当前 `Resolved` 压回队列尾，再前进取下一首。
   - `Off` → 丢弃当前，前进。
5. 队列空且无当前 → `None`（空闲）。

`Remove(n)`：移除待播第 n 首（1 基）；越界则忽略（处理器侧据快照给错误回复）。
`Shuffle`：用 `rand` 打乱待播 `VecDeque`，不动当前。引入 `rand` 依赖。
进度：player 每帧累加帧数；**约每 0.5s（25 帧）把 elapsed 写入快照**一次，限制锁频。

`Snapshot` 扩展：
```rust
pub struct NowPlaying { pub title: String, pub elapsed: Duration, pub duration: Option<Duration> }
pub struct QueueItem { pub title: String, pub duration: Option<Duration> }
pub struct Snapshot {
    pub now_playing: Option<NowPlaying>,
    pub upcoming: Vec<QueueItem>,
    pub volume: u8,
    pub loop_mode: LoopMode,
    pub paused: bool,
}
```

### commands —— 新命令与反馈
- `parse` 扩展支持全部新命令：`Command` 增加 `Pause/Resume/Volume(Option<u8>)/Loop(Option<LoopMode>)/NowPlaying/Remove(usize)/Clear/Shuffle/Help`；`Play(Vec<String>)`（批量，多个 token）。
- `parse` 仍对每个 play token 调用 `strip_url_bbcode`。
- **反馈策略（不加新通道）**：沿用 control 通道 + 只读快照。处理器对需要反馈细节的命令**先读快照**：
  - `Remove(n)`：读快照验证第 n 首存在并取标题；存在则发 `Remove(n)` 回复标题，否则回错误。
  - `Volume`：校验 0-100，发 `SetVolume`，回显值；无参读快照显示当前。
  - `Loop`：解析模式，发 `SetLoop`，回显；无参读快照显示当前。
  - `NowPlaying`/`Queue`：读快照格式化（含进度、时长）。
  - `Pause/Resume/Skip/Stop/Clear/Shuffle`：发对应 control，回简短确认（数量等可读快照）。
  - 极小竞态（读快照与 player 应用之间状态可能变）对聊天机器人可接受。
- 时长格式化辅助：`Duration → "m:ss"`；进度行 `elapsed / duration`（duration 为 None 时显示 `LIVE`）。

### ts-connection
无结构改动（回复内容带 BBCode 文本即可，已有 `send_channel_text`）。

## 数据流（相对 Phase 1 的增量）
不变：driver 转发频道文本 → 处理器 → control 通道 → player（OpusSource）。增量仅在 player 内部状态机变厚、Snapshot 变丰富、处理器读快照做反馈。

## 边界与已知限制
- **批量 `!play`**：按空白切 token，逐个 strip BBCode + resolve，成功入队、汇总失败数；**含空格的本地路径会被切碎**（批量面向 URL），后续可加引号支持——本期接受此限制。
- **暂停直连流**：暂停期间 ffmpeg 因管道背压阻塞；resume 后直播源从"现场"继续（非续播），文件则自然续播——可接受。
- **进度对直播**：duration 为 None，nowplaying 只显示 elapsed 或 `LIVE`。

## 错误处理
- 解析失败 / 越界 / 非法音量 → 友好错误回复，不影响其它播放。
- 单曲 spawn/解码失败 → player 记日志跳过，前进（Phase 1 已有）。
- 统一 anyhow + tracing。

## 测试策略
- `commands::parse`：全部新命令含边界（volume 越界/非数字、loop 合法/非法参数、remove 数字、批量多 token、help）。`strip_url_bbcode` 对批量每个 token 生效。时长/进度格式化纯函数测试。
- `player`：音量系数缩放、pause→None 且保留 current、loop=track 重播、loop=queue 重入队尾、shuffle 改变顺序不动 current、remove 索引（含越界）、clear 保留 current 清待播、elapsed 累计、Snapshot 各字段更新。
- `source`：本地 ffprobe 元数据（造带/不带标签的测试文件）、回退路径。yt-dlp 分支人工/集成。
- driver/真实收发/进度观感/BBCode 渲染：人工验收。

### 人工验收
频道依次验证：批量 `!play a b c`、`!pause`/`!resume`、`!volume 50`、`!loop track`/`queue`、`!nowplaying`(进度走动)、`!queue`(标题时长)、`!remove 2`、`!clear`、`!shuffle`、`!help`，BBCode 正确渲染。

## 待实现核验
- yt-dlp 一次取 标题+时长+直连URL 的 `--print` 模板与输出顺序。
- ffprobe 取 duration + title 标签的命令与解析。
- TS 频道消息对 `[b]` 等 BBCode 的渲染（确认机器人发的消息也被渲染）。
- `rand` crate 加入 player 依赖。
