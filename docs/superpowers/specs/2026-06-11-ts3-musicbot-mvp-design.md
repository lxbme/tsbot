# TS3 Musicbot MVP — 设计文档

**日期**: 2026-06-11
**目标**: 验证"接入 TS3 服务器 + 推送音频发声"这条完整链路。仅做路径验证，不追求架构完备。

## 目标与非目标

### 目标（MVP 成功标准）
- 机器人以完整客户端身份连上指定 TS3 服务器，出现在频道中。
- 真人客户端在同频道能**听到**一个本地音频文件的播放、不卡顿。
- 文件播完后机器人干净退出（退出码 0）。

### 非目标（本阶段明确不做）
yt-dlp / 在线音源、播放队列、文本指令、跳过/音量、断线重连、多文件、配置文件。
这些待 MVP 跑通后再迭代。

## 运行形态

单二进制 Rust 程序，单线程 `tokio` 运行时配合 `LocalSet`（因为 tsclientlib 的 `Connection` 非 `Send`，官方示例同样如此）。

通过 CLI 参数传入配置，**不使用配置文件**。播完即退出，**不常驻等指令**。

## 模块划分

| 模块 | 职责 | 主要依赖 |
|------|------|----------|
| `config` | 解析 CLI 参数 | `clap` |
| `audio_source` | 启动 ffmpeg 子进程，从其 stdout 按 20ms 帧读取 PCM | `tokio::process` |
| `bot` / `main` | 建立连接、identity 管理、定时器驱动编码与发送 | `tsclientlib`, `audiopus` |

### CLI 参数
- `--address <addr>`（必填）：TS3 服务器地址（host 或 host:port）。
- `--file <path>`（必填）：要播放的本地音频文件。
- `--password <pw>`（可选）：服务器密码。
- `--channel <name|id>`（可选）：连接后切入的频道。
- `--identity <path>`（可选，默认 `identity.toml`）：identity 持久化路径。

## 数据流

1. 加载 identity：若 identity 文件存在则读取，否则生成新 identity 并写入文件（复用以避免每次都是新身份）。
2. 用 tsclientlib 构建连接：`Connection::build(address)` + 可选密码，`connect()` 并等待连接就绪。
3. 可选：连接成功后切入 `--channel` 指定的频道。
4. 启动 ffmpeg 子进程解码音频为裸 PCM：
   ```
   ffmpeg -hide_banner -i <file> -ac 1 -ar 48000 -f f32le -
   ```
   输出 = 48kHz / 单声道 / f32 little-endian PCM，写到 stdout。
5. 创建 `audiopus::coder::Encoder`：`SampleRate::Hz48000`、`Channels::Mono`、`Application::Audio`。
6. 20ms `tokio::time::interval` 定时器，每个 tick：
   - 从 ffmpeg stdout 读取 960 个 f32 样本（一个 20ms 帧）。
   - `encoder.encode_float(&frame, &mut opus_buf)` 得到 Opus 字节。
   - `OutAudio::new(&AudioData::C2S { id: 0, codec: CodecType::OpusMusic, data: &opus_buf[..len] })`。
   - `connection.send_audio(packet)`。
7. ffmpeg stdout 读到 EOF（文件播完）后，发送一个空 `data` 的停止音频包，断开连接并退出。

## 关键技术决策

- **编码参数**：48kHz / 单声道 / 20ms 帧（= 48000/50 = 960 样本）。使用 `Application::Audio` + `CodecType::OpusMusic`（音乐场景，比示例用的 Voip/OpusVoice 音质更适配）。
- **节奏控制**：用 20ms 定时器 pace 发送，保证实时性，避免一次性灌满导致卡顿或被服务器限流。
- **identity 持久化**：首次生成、写文件，之后复用同一身份。
- **不引入 SDL2**：官方示例用 SDL2 抓麦克风，本 MVP 用 ffmpeg 子进程提供 PCM 替代，省去一个 C 依赖与音频设备要求（适合服务端常驻场景）。

## 错误处理（MVP 级别）

- 连接失败 / ffmpeg 启动失败 → 打印错误、以非 0 退出码退出。
- 单帧编码失败 → 记日志、跳过该帧，不中断整体播放。
- 错误传播用 `anyhow`，日志用 `tracing`。

## 依赖清单

- `tsclientlib`（0.2.x，默认含 `audio` feature）
- `audiopus`（随 tsclientlib，提供 Opus 编码）
- `tsproto-packets`（提供 `AudioData` / `CodecType` / `OutAudio` / `OutPacket`）
- `tokio`（current-thread + `LocalSet`，`process`/`time`/`io` feature）
- `clap`（CLI 解析）
- `anyhow`（错误处理）
- `tracing` + `tracing-subscriber`（日志）

外部运行时依赖：系统已安装 `ffmpeg`（已确认存在于 `/usr/bin/ffmpeg`）。

## 参考实现

- ReSpeak/tsclientlib `examples/audio.rs` 与 `examples/audio_utils/audio_to_ts.rs`：编码与发送链路的权威参照（注意我们用 ffmpeg PCM 替换其 SDL 麦克风捕获部分）。
- BojanoN/tsmusicbot：tsclientlib + ffmpeg + youtube-dl 的现成 Rust 音乐机器人，可作整体结构参照。
