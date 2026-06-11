# Phase 4 设计：私聊 / poke 指令面

**日期**: 2026-06-11
**所属**: 业务逻辑 roadmap 的 Phase 4。

## 目标

把现有指令处理（频道）扩展到**私聊文本**与 **poke** 两个私密入口。三种入口共用同一套解析 + 权限，仅输入来源与回复通道不同。回复回到消息来源对应的通道。私聊/poke 不要求机器人与发起者同频道——可在服务器任意位置控制机器人。

### 非目标
Web(Phase 5)；发送 poke（poke 回复一律走私信）；按入口禁用的开关（权限已按指令控制，YAGNI）；私聊群发。

## 已核验（tsclientlib）
- 三种入口都产出 `events::Event::Message { target: MessageTarget, invoker: Invoker, message: String }`：
  - 频道 → `MessageTarget::Channel`
  - 私聊 → `MessageTarget::Client(ClientId)`
  - poke → `MessageTarget::Poke(ClientId)`（data.rs:136）
  - 服务器广播 → `MessageTarget::Server`
- 发送：`send_channel_text`（已有，target=Channel）；私信用 c2s `OutSendTextMessageMessage` target=`TextMessageTargetMode::Client` + `target_client_id=Some(id)`，经 `OutCommandExt::send`。

## 模型：来源 → 回复目标

| 入口 target | 处理 | 回复目标 |
|------|------|------|
| `Channel` | 转发为指令 | 频道（`send_channel_text`，现状） |
| `Client(_)`（私聊） | 转发为指令 | 私信回**发起者**（`Client(invoker.id)`） |
| `Poke(_)` | 转发为指令 | 私信回**发起者**（`Client(invoker.id)`） |
| `Server`（广播） | **忽略** | — |

注意：私聊/poke 的 `target` 里携带的是机器人自身 id（消息发给机器人），回复目标取 **invoker.id**（发起者）。

## ts-connection 改动

```rust
/// 回复目标。
#[derive(Clone, Copy)]
pub enum ReplyDest {
    Channel,
    Client(ClientId),
}

/// 收到的指令消息，driver 转发给指令处理器。
pub struct ChatMessage {
    pub text: String,
    pub invoker_id: ClientId,
    pub invoker_uid: String,
    pub reply_to: ReplyDest,   // 该消息应回到哪
}

/// 一条待发回复。
pub struct Reply {
    pub dest: ReplyDest,
    pub text: String,
}

/// 从入口 target 计算回复目标；Server 广播返回 None（不处理）。纯函数，可单测。
pub fn reply_dest_for(target: &MessageTarget, invoker_id: ClientId) -> Option<ReplyDest>;
```

`reply_dest_for` 逻辑：
- `Channel` → `Some(ReplyDest::Channel)`
- `Client(_)` → `Some(ReplyDest::Client(invoker_id))`
- `Poke(_)` → `Some(ReplyDest::Client(invoker_id))`
- `Server` → `None`

**driver 事件处理**：对每个 `Event::Message { target, invoker, message }`，若 `invoker.id != own_id` 且 `reply_dest_for(target, invoker.id)` 为 `Some(dest)`，构造 `ChatMessage { text, invoker_id: invoker.id, invoker_uid, reply_to: dest }` 送入 `chat_tx`。（移除原先只匹配 `Channel` 的写法。）

**回复管道**：`run`/`run_persistent` 的 `reply_rx` 由 `Receiver<String>` 改为 **`Receiver<Reply>`**。driver drain 时按 `dest` 路由：
- `ReplyDest::Channel` → `send_channel_text(con, &text)`（现有）。
- `ReplyDest::Client(id)` → 新增 `send_private_text(con, id, &text)`（c2s 文本 target=Client + target_client_id=Some(id)，`.send(con)`）。

## commands 改动
- `run` 的 `reply_tx` 改为 `Sender<Reply>`。每条消息处理后构造 `Reply { dest: msg.reply_to, text }` 发送（权限拒绝回复同样用 `msg.reply_to`）。
- 依赖 ts_connection 的 `ReplyDest`/`Reply`（已 re-export 或直接路径）。
- 解析、权限门禁、handle_command 逻辑不变（与入口无关）。

## bin 改动
- reply 通道改为 `mpsc::channel::<Reply>(32)`。其余不变。

## 权限：跨入口统一
权限按 uid 判定，与入口无关——私聊/poke 指令同样过 `perms.allows`，`help`/`whoami` 同样豁免。无需改 perms 或 config。私聊里 `!whoami` 取 uid 尤其方便（不在频道暴露）。

## 错误处理
私信发送失败 → driver 记 warn，不影响其它。统一 anyhow + tracing。

## 测试策略
- **ts-connection**：`reply_dest_for` 纯函数——Channel→Channel、Client→Client(invoker)、Poke→Client(invoker)、Server→None。（构造 `MessageTarget` 各变体；`ClientId` 由 re-export 构造。）
- **commands**：`run` 测试改用 `Sender<Reply>`，断言 `reply.text`（受限指令被拒、放行指令通过、whoami 回 uid 等沿用 Phase 3）；新增一例：来源 `reply_to: ReplyDest::Client(ClientId(7))` 的消息，断言回复 `reply.dest` 为 `Client(7)`（回到私信）。
- driver 真实三路收发/私信渲染 → 人工验收。

## 人工验收
1. 频道发 `!queue` → 频道回复（现状不变）。
2. 私聊机器人 `!nowplaying` → 私信收到回复。
3. poke 机器人带文本 `!whoami` → 私信收到 uid。
4. 权限：未授权用户私聊 `!stop` → 私信收到"⛔ 无权限"。

## 待实现核验
- `MessageTarget` 各变体可达（tsclientlib re-export `MessageTarget`，含 `Poke`）。
- 私信发送的 c2s 构造（target=Client + target_client_id）与 `send_channel_text` 同源、可用。
- `Reply`/`ReplyDest` 跨 crate 传递（ts-connection 定义，commands/bin 使用）；`ReplyDest: Copy`。
