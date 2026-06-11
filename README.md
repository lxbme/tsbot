# tsbot

一个用 Rust 编写的 TeamSpeak3 音乐机器人。以完整客户端身份接入 TS3 服务器，通过频道聊天指令点播，把音频实时编码为 Opus 推送到频道。

## 功能（当前：Phase 1）

- 频道聊天指令点播：`!play` / `!skip` / `!stop` / `!queue`
- 音源：yt-dlp 支持的站点（YouTube 等）、直连流 / 网络电台、本地文件
- 顺序播放队列
- 常驻运行，断线自动重连
- 配置走 TOML 文件

尚未实现（见 `docs/superpowers/specs/2026-06-11-business-logic-roadmap.md`）：权限系统、私聊/poke 指令、Web 控制面板、元数据标题、关键词搜索。

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
```

`config.toml` 与 `identity.toml` 已在 `.gitignore` 中，不会入库。

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

| 指令 | 说明 |
|------|------|
| `!play <url 或 本地路径>` | 解析并加入队列；成功/失败会在频道回复 |
| `!skip` | 跳过当前曲目，播放下一首 |
| `!stop` | 停止并清空队列（机器人保持在频道） |
| `!queue` | 查看当前播放与待播列表 |

示例：

```
!play https://www.youtube.com/watch?v=xxxxxxxxxxx
!play https://stream.example.com/radio.mp3
!play /home/me/music/song.mp3
!queue
!skip
!stop
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
  source/         音源解析（本地 / yt-dlp / 直连流）
  player/         播放队列引擎，实现 OpusSource
  commands/       指令解析与处理
  tsbot/          可执行入口，接线各子包
docs/superpowers/ 设计文档、实现计划、roadmap
```

依赖方向（单向无环）：`tsbot` 依赖全部；`player` → {source, audio, ts-connection}；`commands` → {player, source, ts-connection}；`config`、`audio`、`ts-connection`、`source` 互不依赖业务层。

设计与路线图见 `docs/superpowers/`。
