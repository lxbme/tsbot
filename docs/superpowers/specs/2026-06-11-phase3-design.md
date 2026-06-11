# Phase 3 设计：权限系统

**日期**: 2026-06-11
**所属**: 业务逻辑 roadmap 的 Phase 3。

## 目标

基于**角色 + 单指令权限**的访问控制，与 TS3 服务器组解耦：每个用户（按稳定 uid）映射到一个角色，角色列出允许的指令名（`*` = 全部）。未映射的陌生用户归入可配置的默认角色（建议 guest：只读 + 点歌）。提供 `!whoami` 供管理员获取自身 uid 以填入配置。

### 设计原则：opt-in（兼容现状）
配置中**没有 `[permissions]` 段时，权限关闭，一切照旧全开**；一旦配置了角色，才开始强制。现有部署无需改配置即可继续运行。

### 非目标
私聊/poke(Phase 4)、Web(Phase 5)、权限热重载（改权限需重启）、映射 TS3 服务器组、按频道区分权限。

## 用户身份

权限按 TS3 **稳定 uid**（`Invoker.uid`，客户端身份的持久标识）判定，而非会话级的 `ClientId`。`ChatMessage` 当前只带 `invoker_id: ClientId`，需新增 `invoker_uid: String`；ts-connection 的 driver 在转发频道消息时从 `Invoker.uid` 填充（实现时核验 `Uid`/`UidBuf` → `String` 的转换方式）。

## 新子包 crates/perms（tsbot-perms）

纯策略，仅依赖 serde，可独立单测。

```rust
use std::collections::HashMap;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Permissions {
    #[serde(default = "default_role")]
    pub default_role: String,            // 陌生用户的角色，默认 "guest"
    #[serde(default)]
    pub roles: HashMap<String, Vec<String>>,  // 角色名 → 允许的指令名（含 "*" 表示全部）
    #[serde(default)]
    pub users: HashMap<String, String>,  // uid → 角色名
}

fn default_role() -> String { "guest".to_string() }

impl Permissions {
    /// 是否允许某 uid 执行某指令。
    /// - roles 为空（未配置权限）→ 一律 true（权限关闭，opt-in）。
    /// - 否则：取 users[uid] 否则 default_role；该角色指令集含 "*" 或该指令名 → true。
    pub fn allows(&self, uid: &str, command: &str) -> bool;

    /// uid 当前归属的角色名（用于回复提示）。
    pub fn role_of(&self, uid: &str) -> &str;
}
```

`allows` 逻辑：
1. `self.roles.is_empty()` → `true`（权限关闭）。
2. `role = self.users.get(uid).map(String::as_str).unwrap_or(&self.default_role)`。
3. `cmds = self.roles.get(role)`；`None` → `false`（角色未定义即无权限）。
4. `cmds.iter().any(|c| c == "*" || c == command)`。

`role_of`：`users.get(uid).unwrap_or(&self.default_role)`。

## config `[permissions]` 段

`Config` 增 `#[serde(default)] pub permissions: perms::Permissions`（config 依赖 tsbot-perms）。无该段 → `Permissions::default()`（roles 空 → 权限关闭，兼容旧配置）。

`config.example.toml` 附注释示例：
```toml
[permissions]
default_role = "guest"          # 陌生用户角色；删除整个 [permissions] 段则关闭权限

[permissions.roles]
admin = ["*"]
dj    = ["play", "skip", "pause", "resume", "volume", "loop", "shuffle", "remove", "clear", "stop", "playlist"]
guest = ["play", "queue", "nowplaying"]

[permissions.users]
# "你的uid哈希（用 !whoami 获取）" = "admin"
```

## commands 改动

- **新增 `Command::Whoami`**；`parse` 增 `"whoami" => Some(Command::Whoami)`。
- **`command_name(&Command) -> &'static str`**：把每个变体映射到权限键：
  Play→"play"、Pause→"pause"、Resume→"resume"、Skip→"skip"、Stop→"stop"、Volume→"volume"、Loop→"loop"、NowPlaying→"nowplaying"、Queue→"queue"、Remove→"remove"、Clear→"clear"、Shuffle→"shuffle"、Help→"help"、Whoami→"whoami"、Playlist→"playlist"。
- **永远放行集合**：`help`、`whoami`（发现性 + 引导，不受权限限制）。
- **`run` 门禁**：签名增加 `perms: Permissions` 参数。每条消息 parse 出 `cmd` 后：
  ```
  let name = command_name(&cmd);
  if name != "help" && name != "whoami" && !perms.allows(&msg.invoker_uid, name) {
      reply: "⛔ 无权限：{name}（你的角色：{role}）"   // role = perms.role_of(uid)
      continue;
  }
  reply = handle_command(cmd, &handle, &snapshot, &store, &msg.invoker_uid).await;
  ```
- **`handle_command` 增加 `uid: &str` 参数**；新增 `Command::Whoami => format!("你的 uid：[b]{uid}[/b]")`。
- `playlist` 整个作单一权限键（其所有子命令同权限）。

## bin 改动
`config.permissions` 传入 `commands::run(chat_rx, handle, snapshot, store, perms, reply_tx)`。`Permissions` 移动进 run（无需 clone）。

## 各子包改动汇总
- **perms**（新）：`Permissions` + `allows` + `role_of` + `default_role`。
- **ts-connection**：`ChatMessage` 加 `invoker_uid: String`；driver 从 `Invoker.uid` 填充。
- **config**：加 `permissions` 字段（serde default）+ 依赖 perms；`config.example.toml` 加示例段。
- **commands**：`Command::Whoami`、parse、`command_name`、`run` 门禁与 `perms` 参数、`handle_command` 加 `uid`；依赖 perms。
- **bin**：传 perms。

## 错误处理
被拒 → 友好频道回复，不执行、不影响其它用户。统一 anyhow + tracing（无新错误类型）。

## 测试策略
- **perms**：`allows`——roles 空时全 true（权限关闭）；admin `["*"]` 任意指令 true；guest 限定集内 true、集外 false；未知 uid 走 default_role；default_role 指向未定义角色时 false；`role_of` 正确。
- **commands**：`command_name` 每个变体映射；`run` 门禁——注入一个 guest-only `Permissions`，断言受限指令（如 stop）对陌生 uid 被拒回复、放行指令（play）通过、`help`/`whoami` 永远通过、`whoami` 回复含 uid。沿用注入快照/临时 Store 的既有测试手法。
- 真实 uid/收发/重启生效 → 人工验收。

## 人工验收
1. 不配 `[permissions]` 跑 → 一切照旧（权限关闭）。
2. 配 guest/admin，`!whoami` 取 uid → 填 admin → 重启。
3. 管理员可用全部命令；另一未列出用户只能 play/queue/nowplaying，`!stop`/`!volume` 被拒并提示角色。

## 待实现核验
- `Invoker.uid` 的类型与 → `String` 转换（ts-bookkeeping `Uid`/`UidBuf`，可能 `.to_string()` 或取其内部字段）。
- config 依赖 perms、commands 依赖 perms 后依赖图仍单向无环（perms 仅依赖 serde；config→perms；commands→perms；bin→全部）。
- `Permissions` 需 `Clone`（若 bin 需要在别处也用；否则移动即可——按移动设计，Clone 仍加上以备用）。
