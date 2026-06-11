# 业务逻辑 Roadmap

**日期**: 2026-06-11
**性质**: 多子项目的分解与排序路线图。每个阶段后续各自走 brainstorm → spec → plan → 实现 的独立循环；本文件只定范围、子包划分、阶段顺序与每阶段需解决的关键架构决策。

## 背景与现状

地基（Phase 0）已完成并合并入 `main`：Cargo workspace 四子包——`tsbot-config`、`tsbot-audio`（分帧+Opus 编码）、`ts_connection`（连接/identity/`OpusSource` trait/`stream_audio` driver）、`tsbot`(bin)。当前 bin 用 `FileOpusSource` 实现 `OpusSource`，连上服务器播放一个本地文件即退出。

`OpusSource` trait 与 `stream_audio` 正是为业务逻辑预留的接缝：播放器（player）将实现 `OpusSource`，直接插入 `stream_audio` 驱动。

## 控制面与音源（讨论结论）

- **控制面**：① 频道聊天指令（主）② 私聊 / poke 指令。（原 ③ Web/HTTP 控制面板已取消。）不做启动自动播放。
- **音源**：① yt-dlp 站点（YouTube 等）② 直连流 / 电台 URL ③ 本地文件/目录。**不做关键词搜索**（按 URL/路径点播）。
- **权限**：角色 + 每指令权限位，用户（TS3 uid）→ 角色映射，与 TS3 服务器组解耦。
- **起点形态**：薄的端到端切片优先，逐层加厚。

## 新增子包

| 子包 | 职责 | 引入阶段 |
|------|------|----------|
| `crates/source` | 把一个请求（url/path）解析为可解码输入 + 元数据（标题/时长）。封装 yt-dlp / 直连流 / 本地三种解析；泛化现有 `spawn_ffmpeg` | Phase 1 |
| `crates/player` | 队列 + 播放引擎，**实现 `ts_connection::OpusSource`**。管理当前曲目与待播队列，处理 skip/stop/pause/volume/loop | Phase 1 |
| `crates/commands` | 指令解析 + 路由 + 权限门禁；监听 TS3 事件流，把文本消息/poke 转为指令 | Phase 1 |
| `crates/perms` | 角色与每指令权限位、用户→角色映射，在指令分发层做门禁 | Phase 3 |
| ~~`crates/web`~~ | ~~HTTP API + 最小控制面板~~ | Phase 5（已取消） |

依赖方向保持单向无环：bin → {business crates} → {audio, ts-connection, config}；business crates 之间，player/commands 依赖 source，commands 依赖 player + perms。

## 阶段顺序

### Phase 0 ✅ 已完成
地基：workspace 四子包 + 文件播放 MVP。

### Phase 1 — 薄端到端切片（里程碑：可点播 + 常驻）
**交付**：频道内 `!play <url>`（yt-dlp / 直连流 / 本地）入队 → 顺序播放 → `!skip` / `!stop` / `!queue`。**无权限**（任何人可用）。机器人**常驻**，含**断线重连**。

**范围**：source / player / commands 三个最小版本，打通"指令 → 解析 → 队列 → 播放"整条链。

**关键架构决策（本阶段必须解决）**：
1. **事件路由**：当前 `stream_audio` 仅把事件流用于保活/断线检测。本阶段要让驱动循环**同时把文本消息事件路由给指令处理器**。倾向把 `stream_audio` 演进为更通用的 `run` 驱动，或在其事件臂上加一个事件回调/输出通道。
2. **player 与指令并发共享**：player 既是 `stream_audio` 的 `OpusSource`（被拉帧），又被指令处理器写入（入队/跳过/停止）。倾向用 `mpsc` 控制通道——指令处理器发送 `Control::{Play,Skip,Stop}`，player 在帧间消费控制消息——以此解耦，避免对 `con` 的多重借用。
3. **source 泛化**：把现有 `spawn_ffmpeg` 泛化为接受 yt-dlp 子进程管道 / 直连 URL（ffmpeg 直拉）/ 本地路径三类输入，产出统一的 PCM 流交给 player 的编码段。
4. **断线重连**：MVP 现为断线即 `Err` 退出。本阶段补一个轻量重连——检测到断线后按退避重连、保留/恢复队列状态，使机器人可常驻。

### Phase 2 — 播放控制加厚（拆为 2A / 2B）
brainstorm 时因范围扩大拆分为两个各自聚焦的子项目：

- **Phase 2A — 播放控制加厚**（spec: `2026-06-11-phase2a-design.md`）：`!help`、`!pause`/`!resume`、`!volume`(0-100)、`!loop`(off/track/queue)、`!nowplaying`(带进度)、`!queue`/`!remove`/`!clear`/`!skip`/`!stop`、`!shuffle`、批量 `!play`、元数据(标题/时长)。回复用 BBCode 轻美化。围绕当前队列/播放器的内聚单元。
- **Phase 2B — 播放列表**：命名歌单的本地持久化（文件格式）+ 创建/保存/列出/查看/加载播放。独立存储子系统，2A 之后单独走 spec→plan。

**范围**：source 增加元数据提取（yt-dlp/ffprobe 取标题、时长）；player 队列模型加厚（音量、循环、移除）。

### Phase 3 — 权限系统
**交付**：`crates/perms`——config 定义角色（如 admin/dj/guest）与每指令权限位，用户（TS3 uid）→ 角色映射；在指令分发层做门禁。点播/查看可开放，stop/clear/volume 等管理操作转为受控。

**关键决策**：权限检查插在指令分发层，与解析解耦；默认角色与未知用户的处理策略。

### Phase 4 — 私聊 / poke 指令面
**交付**：扩展事件监听处理私聊文本消息与 poke，复用同一套指令解析器 + 权限门禁，作为频道指令之外的第二输入通道。

### ~~Phase 5 — Web/HTTP 控制面板~~（已取消，2026-06-11）
原计划：axum HTTP API + 最小网页面板，鉴权接 perms。**已从 roadmap 移除**，暂不实现。

## 明确不做（范围之外）
- 关键词搜索（按 URL/路径点播）
- 启动自动播放
- 队列重启持久化
- 多服务器 / 多实例

## 进度（2026-06-11）
Phase **0 / 1 / 2A / 2B / 3 / 4 均已完成并合并入 `main`**。Phase 5（Web 面板）**已取消**。

roadmap 范围内功能至此完成。后续如有新需求，再各自走 brainstorm → spec → plan → 实现。
