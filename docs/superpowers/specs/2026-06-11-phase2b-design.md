# Phase 2B 设计：播放列表

**日期**: 2026-06-11
**所属**: 业务逻辑 roadmap Phase 2 的 2B（2A 播放控制加厚已完成）。

## 目标

命名歌单的本地持久化：从当前队列一键保存，或增量增删；列出、查看、加载（追加播放）、删除。歌单以 TOML 文件保存在可配置目录，人可手编。

### 非目标
权限(Phase 3)、私聊/poke(Phase 4)、Web(Phase 5)、跨实例同步、自动保存正在播放进度。

## 关键前提：Resolved 需保存原始请求

加载歌单时要"重新解析以拿新鲜直连 URL"，因此歌单存的是**原始请求**（用户输入的 URL/路径）。但当前 `source::Resolved` 只存解析后的 `input`（如 yt-dlp 产出的会过期的 googlevideo 直连 URL），未保留原始请求。若直接保存当前队列会存成一堆过期直连 URL。

**修正**：`source::Resolved` 增加 `request: String`（`resolve` 中 = 传入的 `arg`）；`player` 的 `NowPlaying`/`QueueItem` 携带 `request`（来自 `Current`/队列项的 `resolved.request`），使"从队列保存歌单"能取到 原始请求 + 标题。

## 新子包 crates/playlist（tsbot-playlist）

纯存储与文件格式，独立可测。依赖 serde/toml/anyhow。

```rust
#[derive(Clone, Serialize, Deserialize)]
pub struct PlaylistItem { pub request: String, pub title: String }

#[derive(Clone, Serialize, Deserialize)]
pub struct Playlist { pub name: String, pub items: Vec<PlaylistItem> }

pub struct Store { dir: PathBuf }

impl Store {
    pub fn new(dir: PathBuf) -> Self;
    /// 写 <dir>/<name>.toml（目录不存在则创建）。
    pub fn save(&self, p: &Playlist) -> anyhow::Result<()>;
    pub fn load(&self, name: &str) -> anyhow::Result<Playlist>;
    /// 目录下所有 *.toml 的歌单名（按名排序）。
    pub fn list(&self) -> anyhow::Result<Vec<String>>;
    pub fn delete(&self, name: &str) -> anyhow::Result<()>;
}

/// 名字校验：非空，且每个字符为 字母数字(含 Unicode) / '-' / '_'。
/// 拒绝含 '/'、'.'、空格、'..' 等（防路径穿越，安全关键）。
pub fn valid_name(name: &str) -> bool;
```
`save`/`load`/`delete` 内部先 `valid_name` 校验，非法名返回错误（绝不据用户输入拼出越界路径）。文件名 = `<dir>/<name>.toml`。

## 命令：`!playlist <子命令>` 命名空间

| 命令 | 作用 | 网络 |
|------|------|------|
| `!playlist save <名>` | 当前队列（正在放 + 待播）存为歌单（覆盖同名） | 否 |
| `!playlist create <名>` | 建空歌单 | 否 |
| `!playlist add <名> <url>` | 解析取标题后追加一条（存 请求+标题） | 是 |
| `!playlist remove <名> <n>` | 删第 n 条（1 基，对应 view 编号） | 否 |
| `!playlist list` | 列出所有歌单名 | 否 |
| `!playlist view <名>` | 显示歌单内容（编号 + 标题） | 否 |
| `!playlist play <名>` | 逐条重解析后**追加到队列尾** | 是 |
| `!playlist delete <名>` | 删除歌单文件 | 否 |
| `!playlist`（空/未知子命令） | 用法提示 | 否 |

回复沿用 BBCode 轻美化（`[b]…[/b]`）。错误（非法名/歌单不存在/编号越界/缺参）→ 友好回复。

## 各子包改动

- **source**：`Resolved` 加 `request: String`，`resolve` 置 `request = arg.to_string()`。
- **player**：`NowPlaying`/`QueueItem` 各加 `request: String`（`update_snapshot` 从 `resolved.request`/队列项填充）。其余不变。
- **commands**：
  - `Command::Playlist(Vec<String>)`：`!playlist` 之后按空白切的 token（首个为子命令）。
  - `parse`：`"playlist" => Some(Command::Playlist(tokens))`。
  - 新增 `async fn handle_playlist(tokens, store, handle, snapshot) -> String` 分发子命令。
  - `run` 签名**增加 `store: playlist::Store`** 参数：`run(chat_rx, handle, snapshot, store, reply_tx)`。
  - 依赖新增 `tsbot-playlist`。
- **config**：新增
  ```toml
  [playlist]
  dir = "playlists"   # 默认；歌单 TOML 存放目录
  ```
  对应 `Config` 增 `pub playlist: Playlist { pub dir: String }`，默认 "playlists"（用 serde default，使旧配置无 `[playlist]` 段也能加载）。
- **bin**：`let store = playlist::Store::new(config.playlist.dir.into());` 传入 `commands::run`。

## 子命令行为细节

- **save**：读快照，把 `now_playing`(若有) + `upcoming` 依序转为 `PlaylistItem{request,title}`，组成 `Playlist` 存盘（同名覆盖）。空队列也允许保存（空歌单）。回 `已保存歌单 [b]名[/b]（N 首）`。
- **create**：存一个空 `Playlist`（同名已存在则报错，避免误覆盖）。
- **add**：`source::resolve(url)` 取 title，`load` 现有歌单（不存在则新建）、追加 `{request:url, title}`、`save`。回 `已加入 [b]title[/b] → 歌单 名`。
- **remove n**：`load`，校验 1≤n≤len，移除第 n-1 项，`save`。回移除的标题或"歌单里没有第 n 首"。
- **list**：`store.list()`，回名字列表或"还没有歌单"。
- **view**：`load`，编号列出各项标题。
- **play**：`load`，逐条 `source::resolve(item.request)` → `handle.play`，统计成功/失败，**追加**到当前队列。回 `已加载歌单 [b]名[/b]（成功 X，失败 M）`。大歌单逐条解析耗时，期间命令循环阻塞——本期接受，靠数量反馈。
- **delete**：`store.delete(name)`，回确认或"歌单不存在"。

## 错误处理
统一 anyhow + 友好频道回复；非法名/缺参/越界/不存在均不 panic。Store 操作失败（IO/解析）回简短错误。

## 测试策略
- **tsbot-playlist**：`valid_name`（合法/非法：含 `/`、`..`、空、空格）；`Store` save→load 往返、list（排序）、delete、load 不存在报错——全部 tempdir，无网络。
- **commands**：
  - playlist 子命令解析（`parse("!playlist save x")` → `Playlist(["save","x"])`）。
  - `handle_playlist` 的非网络分支用 tempdir Store + 注入快照测：save（从快照生成歌单文件）、create、list、view、remove、delete、非法名/越界/缺参的错误回复。
  - `add`/`play`（联网 resolve）→ 人工验收。
- **人工验收**：`!play` 攒队列 → `!playlist save mylist` → `!playlist list` → `!playlist view mylist` → 清空后 `!playlist play mylist` 追加播放 → `!playlist add`/`remove` → `!playlist delete`。检查 `playlists/mylist.toml` 内容可读。

## 待实现核验
- serde + toml 对 `Playlist`/`PlaylistItem` 的序列化（`Vec<struct>` 在 toml 中为 `[[items]]` 数组表，确认往返）。
- `config` 的 `[playlist]` 用 `#[serde(default)]` 保证旧配置兼容。
- `commands::run` 新增 `Store` 参数后 bin 接线更新。
