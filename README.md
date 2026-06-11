# tsbot

一个用 Rust 编写的 TeamSpeak3 音乐机器人。以完整客户端身份接入 TS3 服务器，通过频道聊天指令点播，把音频实时编码为 Opus 推送到频道。

## 功能

- 频道聊天指令控制（仅响应频道消息，BBCode 轻美化回复）
- 音源：yt-dlp 支持的站点（YouTube 等）、直连流 / 网络电台、本地文件；支持空格分隔**批量点播**
- 播放队列 + 控制：暂停/继续、音量(0-100)、循环(off/track/queue)、跳过、移除、清空、打乱
- 当前曲目元数据（标题/时长）与播放进度
- **播放列表**：命名歌单本地 TOML 持久化，保存当前队列 / 增删 / 列出 / 查看 / 加载
- 常驻运行，断线自动重连；`Ctrl+C` 优雅断开退出
- 配置走 TOML 文件

尚未实现（见 `docs/superpowers/specs/2026-06-11-business-logic-roadmap.md`）：权限系统、私聊/poke 指令、Web 控制面板、关键词搜索。

## 依赖

运行前需在 `PATH` 中具备：

- **ffmpeg** —— 音频解码/转码
- **yt-dlp** —— 解析 yt-dlp 站点 URL（仅点播此类 URL 时需要）

构建需要 Rust 工具链（edition 2021；本项目用 1.94 验证）。`audiopus` 会通过 cmake 自带构建 libopus，无需系统 opus 开发包。

```bash
# Fedora 示例
sudo dnf install ffmpeg yt-dlp cmake
```

## 配置

复制模板并填入你的服务器信息：

```bash
cp config.example.toml config.toml
```

`config.toml` 字段：

```toml
[server]
address  = "ts.example.com:9987"   # TS3 服务器地址 host 或 host:port
password = "secret"                # 可选；无密码服务器删除此行
channel  = "Lobby"                 # 可选；不指定则留在默认频道

[bot]
name          = "tsbot"            # 机器人在服务器上的显示名
identity_path = "identity.toml"    # 客户端身份文件；首次运行自动生成并复用

[playlist]
dir = "playlists"                  # 可选；歌单 TOML 存放目录（默认 playlists）
```

`config.toml`、`identity.toml`、`playlists/` 已在 `.gitignore` 中，不会入库。

## 构建与运行

```bash
# 构建整个 workspace
cargo build --workspace

# 运行（默认读取 ./config.toml，可用 --config 指定其它路径）
cargo run -p tsbot -- --config config.toml

# 看日志
RUST_LOG=info cargo run -p tsbot -- --config config.toml
```

机器人连上后会停在频道空闲待命，靠指令驱动播放。`Ctrl+C` 退出。

## 频道指令

在机器人所在的 TS3 频道里发送（仅响应频道消息）：

### 播放与队列
| 指令 | 说明 |
|------|------|
| `!play <url/路径> [更多…]` | 解析并加入队列；支持空格分隔批量；成功/失败会回复 |
| `!pause` / `!resume` | 暂停 / 继续 |
| `!skip` | 跳过当前曲目 |
| `!stop` | 停止并清空队列（机器人保持在频道） |
| `!volume [0-100]` | 设置/查看音量 |
| `!loop [off\|track\|queue]` | 设置/查看循环模式 |
| `!nowplaying` | 当前曲目 + 进度 + 状态 |
| `!queue` | 当前播放与待播列表（标题、时长） |
| `!remove <编号>` | 移除待播第 n 首 |
| `!clear` | 清空待播队列（保留当前曲目） |
| `!shuffle` | 打乱待播队列 |
| `!help` | 列出所有命令 |

### 播放列表
| 指令 | 说明 |
|------|------|
| `!playlist save <名>` | 把当前队列存为歌单 |
| `!playlist create <名>` | 创建空歌单 |
| `!playlist add <名> <url>` | 向歌单追加一条 |
| `!playlist remove <名> <编号>` | 删除歌单中第 n 条 |
| `!playlist list` | 列出所有歌单 |
| `!playlist view <名>` | 查看歌单内容 |
| `!playlist play <名>` | 加载歌单（重解析后追加到队列尾） |
| `!playlist delete <名>` | 删除歌单 |

示例：

```
!play https://www.youtube.com/watch?v=xxxxxxxxxxx https://stream.example.com/radio.mp3
!volume 70
!loop queue
!nowplaying
!playlist save chill
!playlist play chill
```

## 注意事项

- **发言权限**：机器人所在频道需具备发言权（talk power）。否则音频包被服务器静默丢弃，听不到声音也不会报错。
- **首次连接可能较慢**：服务器若要求更高的 identity 安全等级，库会在后台计算并升级身份（首次可能静默等待数秒到数分钟），算好后写入 `identity.toml` 复用，之后启动很快。服务器要求等级 > 20 时会连接失败。
- **听不到声音排查**：先用 `ffmpeg -i <你的输入> -f f32le -` 确认 ffmpeg 能解码该输入。
- yt-dlp `-g` 解析出的直连 URL 可能有时效（如 YouTube），排队很久后才播的曲目偶尔会失效。

## 项目结构

Cargo workspace，子包职责清晰、单向依赖：

```
crates/
  config/         TOML 配置加载
  audio/          PCM 分帧 + Opus 编码（无 TS 依赖）
  ts-connection/  tsclientlib 封装：连接/identity/收发/常驻驱动 run_persistent
  source/         音源解析（本地 / yt-dlp / 直连流），含标题/时长元数据
  playlist/       歌单存储：TOML 文件格式 + 名字校验（叶子 crate）
  player/         播放队列引擎，实现 OpusSource（音量/暂停/循环/进度）
  commands/       指令解析与处理（含 !playlist）
  tsbot/          可执行入口，接线各子包
docs/superpowers/ 设计文档、实现计划、roadmap
```

依赖方向（单向无环）：`tsbot` 依赖全部；`player` → {source, audio, ts-connection}；`commands` → {player, source, ts-connection, playlist}；`config`、`audio`、`ts-connection`、`source`、`playlist` 互不依赖业务层。

设计与路线图见 `docs/superpowers/`。
