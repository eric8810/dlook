# 音频播放栈调研（evidence-audio）

> 调研日期：2026-09-13。方法：官方仓库/crates.io/docs.rs 查证 + 本地实测（rustc 1.98.1，dlook 同款 release profile 构建）。区分【官方】声明与【社区】评价。本文件只覆盖音频；视频/网页另文。

---

## 一、当前结论

**推荐 dlook 采用 rodio 0.22.2（默认特性，即 symphonia 解码全家桶）**，理由与代价如下：

| 维度 | 结论 |
|---|---|
| 体积增量 | **+1.52MB**（默认 flac/mp3/mp4+vorbis/wav 全集，实测）；只要 mp3+flac+wav 则 **+0.6MB**。9.2MB → ~10.7MB（+16%），符合 D12 全功能优先 |
| 平台覆盖 | Linux（ALSA 动态链接）/ macOS（CoreAudio）/ Windows（WASAPI），均为 cpal 官方支持 |
| 运行时依赖代价 | Linux 上新增 `libasound.so.2` 链接时依赖（非 dlopen，无法关闭）；macOS/Windows 无额外运行时依赖 |
| 格式边界 | FLAC/MP3/Vorbis/WAV/PCM = Excellent；AAC-LC/ALAC(M4A)/MP1/MP2 = Great；**Opus、HE-AAC、WavPack 不支持**（Opus 仅 master 开发中，未发版） |
| 播放控制 API | rodio 0.22 `Player` 原生提供 play/pause/`try_seek()`/set_volume/set_speed，无需自研播放器 |
| 许可 | rodio MIT/Apache-2.0；**symphonia MPL-2.0**（文件级弱 copyleft，静态链接进 MIT 二进制可行，但需遵守 MPL 对应源文件公开义务；替代解码器 claxon/hound/lewton/minimp3 为 MIT/Apache） |
| 主要风险 | ① rodio 正在引擎重写（#901 未完成），0.22 已破坏 API（`Sink`→`Player`），升级需 pin 版本逐步跟；② rodio 0.22.2 锁定 cpal 0.17.3（Linux 仅 ALSA 后端），纯 Rust PulseAudio 路径要等 rodio 下个版本（cpal 0.18）；③ cpal 无设备热插拔/切换通知 API |

**不推荐**：直接用 cpal（丢失 Player 的暂停/seek/音量/重采样/混音，dlook 无低延迟需求）、libpulse-binding（C 动态链接，违背纯 Rust 哲学）、单独用纯 Rust `pulseaudio` crate（无 ALSA 回退、无 macOS/Windows）、sndio（OpenBSD 小众，cpal 无此后端）。

---

## 二、子问题 1：纯 Rust 音频输出栈

### 2.1 rodio / cpal 后端覆盖【官方】

- **rodio 0.22.2**（2026-03-05 发布，crates.io 总下载 1107 万、近期 257 万）：默认特性 `playback, recording, flac, mp3, mp4, vorbis, wav, dither`，解码**默认全走 symphonia**（claxon/minimp3/lewton/hound 降级为可选替代）。来源：crates.io API、官方 README。
- **cpal 后端矩阵**（官方 README，cpal 0.18.2 = 2026-08-16）：

| 平台 | 默认后端 | 可选后端 |
|---|---|---|
| Linux/BSD | ALSA | JACK、PipeWire、PulseAudio |
| macOS | CoreAudio | JACK |
| Windows | WASAPI | ASIO、JACK |
| Android/iOS | AAudio / CoreAudio | — |
| WASM | Web Audio API | Audio Worklet |

- **版本错位（重要）**：已发布的 rodio 0.22.2 依赖 **cpal 0.17.3 + symphonia 0.5.5**（本地 `cargo tree` 实测）。cpal 0.17.3 **没有** pulseaudio/pipewire 特性（crates.io 特性表只有 asio/jack/audioworklet/wasm-bindgen）→ 即当前 rodio 发布版在 Linux 只走 ALSA。rodio **master**（未发布）才默认启用 `pulseaudio` 特性并宣称运行时顺序 **PipeWire > PulseAudio > ALSA**（官方 README "Dependencies (Linux only)"）。

### 2.2 ALSA 依赖形态：链接时依赖，非 dlopen【官方+实测】

- cpal 在 Linux 上经 `alsa` crate → `alsa-sys`（`links = "alsa"`，pkg-config）**链接时**依赖 libasound；构建需 `libasound2-dev`，运行需 `libasound.so.2`。cpal master（0.19-dev）Cargo.toml 中 Linux 段 `alsa = "0.12.1"` 仍为**非可选**，官方 README 明言"ALSA is needed even when using JACK, PipeWire, or PulseAudio"。
- 实测：`ldd rodio-full` 显示 `libasound.so.2`（连同 libgcc_s/libm/libc）；本机无 libpulse 开发头也能编译 rodio 0.22.2（因其只有 ALSA 后端）。
- 含义：**引入音频后，dlook 在 Linux 不再是"零共享库依赖"**（仍为单文件，但多一个 libasound）。静态方案（musl + 自编译 alsa-lib 静态库）存在理论可能，未验证，非发行版默认路径。

### 2.3 PipeWire 支持状况【官方】

- cpal 0.18.2 提供 `pipewire` 特性，但需 `libpipewire-0.3-dev` + `libdbus-1-dev`（C 库，动态链接），rodio master 也只将其列为可选。**默认不启用**。
- 现实路径：纯 Rust `pulseaudio` crate（colinmarc/pulseaudio-rs，原生实现 PulseAudio 协议，无 C 依赖）是 rodio master 的默认选择（cpal 0.18 特性 `pulseaudio = ["dep:pulseaudio", ...]`）；PipeWire 桌面通过 pipewire-pulse 兼容服务被覆盖。
- 背景动机【社区】：rodio #883（Alacritty 维护者 chrisduerr，2026-05）：ALSA 直连出现"音量极小、应用不出现在混音器、cpal beep 示例默认报错"，而 pulseaudio/pipewire 特性正常；促成 PR #888 默认启用纯 Rust pulseaudio。

### 2.4 社区评价的坑

| 坑 | 证据 | 状态 |
|---|---|---|
| 设备热插拔/默认设备切换无通知 API | cpal #1332（feature request，2026-09 关为重复）；v0.19 设计目标 #1220 亦无此项 | 长期空白，应用层自行轮询/重建流 |
| ALSA 直连异常（音量/混音器不可见） | rodio #883【社区】 | rodio master 已转向 pulseaudio 默认 |
| 高采样率 FLAC 缓冲 underrun/overrun（96k/192k） | rodio #827（2026-01 关闭） | 有 #856 limit-buffersize、#843 高质量重采样等修复，44.1k/48k 常规场景未见集中投诉 |
| 引擎重写进行中、API 不稳 | rodio #901 跟踪 issue（~7700 行重构：废除 spans、引入 rubato 重采样、ConstSource/FixedSource）；README 顶部警告"rewriting core engine, under documented" | 0.22 已落地 `Sink`→`Player` 破坏性变更（UPGRADE.md）；阶段 3-6 未完 |
| MSRV 滚动 | rodio README：滚动 6 个月 MSRV；master `rust-version = 1.95` | 需关注工具链 |

### 2.5 直接用 cpal 的成本收益

- 收益：设备/主机枚举、按稳定 ID 查设备、流配置（采样率/缓冲）精细控制、回调线程自主（cpal 0.18 还加了 play/pause/时钟查询、realtime 特性）。
- 成本：自行实现环形缓冲、重采样（rubato）、混音、暂停语义、音量、seek——rodio `Player` + 后台线程已全部提供且文档声明"混合无并发数限制"。
- dlook 是文档预览器内嵌的单音轨播放，无低延迟/多路混音诉求 → **不值得**。

### 2.6 更轻的选择（均不推荐）

- `libpulse-binding`：C 绑定，动态链接 libpulse（ncspot 默认后端即此，见 ldd 实测）→ 违背静态/纯 Rust 哲学。
- 纯 Rust `pulseaudio` crate 单独直用：丢 ALSA 回退与跨平台 → 无净收益（它已被 cpal 0.18 集成为默认路径）。
- sndio：OpenBSD 默认（cmus 有此输出插件）；Rust 侧无成熟 cpal 后端 → 排除。
- rodio 支持 `default-features = false` 无播放构建（README "Minimal build"，可只留 symphonia 解码）→ **建议 dlook CI 用此形态编译测试解码逻辑，免音频设备**。

### 2.7 体积实测（dlook 同款 profile：opt-level=z、fat LTO、codegen-units=1、strip、panic=abort）

| 构建物 | 大小（字节） | 相对基线增量 |
|---|---|---|
| hello world 基线 | 291,760 | — |
| rodio 0.22.2 默认（flac/mp3/mp4+vorbis/wav） | 1,815,512 | **+1.52MB** |
| rodio playback+mp3+flac+wav | 908,920 | **+617KB** |
| rodio playback+mp3+flac+wav+vorbis+mp4 | 1,815,624 | ≈默认（recording/dither 被 LTO 裁剪） |

- 依赖树规模：正常依赖共 53 个 crate（rodio→cpal 0.17.3、symphonia 0.5.5、dasp_sample、num-rational、thiserror、alsa、libc…）。
- 结论：格式集合是体积主变量；**即使全集也仅 +1.5MB**。

---

## 三、子问题 2：symphonia 解码

### 3.1 格式覆盖与成熟度【官方状态表】

symphonia 0.6.1（2026-08-13 发布，0.5.4 之后约 3 年的首个大版本；总下载 1279 万）。官方 README 状态表（Excellent=过全部合规测试；Great=可用于多数应用）：

| 类别 | Excellent | Great | Good | 不支持/未完成 |
|---|---|---|---|---|
| Demuxer | WAV | OGG、ISO/MP4、AIFF | MKV/WebM、CAF | — |
| Codec | FLAC、MP3、Vorbis、PCM | AAC-LC、ALAC、MP1、MP2 | ADPCM | **Opus（开发中）、HE-AAC、WavPack** |
| 元数据 | Vorbis comment（FLAC/OGG）"Perfect" | ID3v1/v2、MP4、RIFF | — | CAF/MKV 元数据（termusic 实测标"No"） |

- **Opus 边界查证**：master README 列 Opus 状态"-"（in work），引用的 `symphonia-codec-opus` crate **在 crates.io 不存在（404）**；0.6.1 特性表无 `opus`。→ `.opus`/ogg-opus 当前无法用 symphonia 解码。termusic 的做法是 `rusty-soundtouch` 之外的 `rusty-libopus` 特性：`symphonia-adapter-libopus` crate 桥接 **C libopus**（需系统库，非纯 Rust）。
- Gapless：FLAC/MP3/Vorbis/WAV/PCM/ALAC/ADPCM 官方标注支持（demuxer+decoder 双方支持才生效）。
- 0.6 起 `opt-simd`（SSE/AVX/NEON，rustfft）进入默认特性。

### 3.2 生产使用方【crates.io 反向依赖】

- **rodio**（0.22.2 默认解码器，1107 万下载）
- **librespot-playback 0.8**（Spotify 协议实现，symphonia 为**非可选**依赖，特性 ogg/vorbis/mp3/flac——ncspot/spotifyd 生态的解码核心）
- **termusic**（rusty 后端直接用 symphonia 0.6 特性 aac/mp3/isomp4/alac/flac/mkv/wav/aiff）
- **fundsp**（音频处理库，files 特性）
- 成熟度综合：发布节奏慢（3 年一版）但被上述生产项目锁定使用；fuzz 测试与 DoS 防护为官方目标。

### 3.3 与 rodio 的标准用法

```rust
// rodio 0.22 官方文档示例（新 API）
let handle = rodio::DeviceSinkBuilder::open_default_sink()?;
let player = rodio::Player::connect_new(&handle.mixer());
player.append(rodio::Decoder::try_from(File::open("music.ogg")?)?);
```

- `Player` 提供：`play()`/`pause()`/`try_seek(Duration)`（阻塞 0-5ms，支持与否取决于源；symphonia 路径支持）/`set_volume`/`set_speed`（变速同时变调——pitch 保持需 soundtouch 类方案，termusic 即如此）。
- 两种集成深度：① 直接 `rodio::Decoder`（最省事）；② termusic 式：自持 symphonia `FormatReader` 解码、经 rodio Mixer 播放（获得对 seek/gapless/进度的完全控制）。dlook 起步建议①。

### 3.4 流式播放内存占用【官方+源码】

- symphonia 是拉取式流解码：`MediaSourceStream` 默认缓冲 **64KB**（源码 `Default for MediaSourceStreamOptions`：`buffer_len: 64 * 1024`；约束为 2 的幂且 >32KB），大文件/HTTP 流（termusic 配 stream-download 边下边解）内存恒定，不整载文件。
- rodio 后台线程从 Source 拉取并混音送 cpal 缓冲 → 常规曲目峰值内存为数百 KB 量级（64KB 读缓冲 + 解码器状态 + packet + cpal 缓冲）。

---

## 四、子问题 3：参照项目

### 4.1 ncspot 1.4.0（Spotify TUI）

- **架构修正**：任务假设"ncspot = rodio + librespot"已过时。ncspot 1.4.0 Cargo.toml：UI = cursive 0.21（默认 **crossterm_backend**）；音频 = librespot-playback 0.8，默认后端 **pulseaudio_backend**（libpulse-binding，C 动态链接），`rodio_backend` 只是可选特性（另有 alsa/portaudio 后端）。release profile：lto=true + codegen-units=1。
- **体积/依赖形态（实测）**：官方 linux-x86_64 tar.gz 7.9MB；解包后单二进制 **20.8MB（未 strip）**；ldd：libpulse-simple/libpulse/libssl/libcrypto/libdbus/libz/libbrotlienc + glibc 族。官方 users.md 运行时依赖：dbus、libssl、libpulse（或 portaudio）、libxcb、libncurses（ncurses 后端时）。
- 含义：librespot+reqwest+tokio+cursive 全家桶 ≈ 21MB —— 与 dlook 场景不同，仅作"Rust TUI 播放器可以多大"的参照上限。

### 4.2 termusic 0.13.2（本地音乐 TUI，最贴近 dlook 场景）

- **架构**（workspace：lib/playback/server/tui）：**client-server** —— TUI（tuirealm 4.1，基于 ratatui）与 termusic-server（tonic gRPC + rusqlite bundled）分离。实测 TUI 二进制**不含音频栈**（ldd 仅 libc 族），音频在 server。
- **rusty 后端（默认）= 纯 Rust**：rodio 0.22（`default-features=false, features=["playback"]` 仅用其播放）+ symphonia 0.6（自选特性自管解码）；可选 GStreamer/MPV 后端；Opus 需 `rusty-libopus`（C libopus）；pitch 保持变速需 `rusty-soundtouch`（bundled C++，链 libstdc++）。
- **格式表【官方】**：容器 MP4/M4A、MP3、OGG、FLAC、ADTS、WAV/AIFF、CAF、MKV/WebM 全支持；codec：AAC-LC ✓、HE-AAC ✗、MP1/2/3 ✓、FLAC ✓、Vorbis ✓、**Opus 仅开 C 特性后 ✓**、ADPCM ✓、PCM ✓。元数据：CAF/MKV 不读。
- **功能**：歌词（内嵌、偏移调整 F/B 键、T 循环显示）、gapless 开关（Ctrl+g）、播客 RSS、YouTube/网易云等下载、tag 编辑、专辑封面（kitty/iTerm2 默认，可选 sixel/ueberzug）、MPRIS（souvlaki + zbus，纯 Rust）。
- **体积（实测官方发布物）**：linux-x86_64 tar.xz 12.75MB（含双二进制）；解包：**termusic(TUI) 24.4MB、termusic-server 18.8MB（均已 strip）**。注意官方 release 用 `all-backends,rusty-soundtouch,cover` 构建（release.yml），server 因此链接 libasound/libmpv(带 ffmpeg 全家)/libgstreamer/libstdc++；**纯 rusty 后端的体积会显著更小**。
- 构建：MSRV 1.90、edition 2024、lto=true。

### 4.3 cmus 2.12 / musikcube（C/C++，交互参照）

- cmus：输出插件 PulseAudio/ALSA/OSS/JACK/sndio/CoreAudio/libao/WaveOut；输入含 Opus、ffmpeg 全家、mod 等；gapless、ReplayGain、CUE；2.12 版"UI refresh：更新状态行、更多可配置项与**进度条**"。依赖 ncurses。
- musikcube：ffmpeg 解码（libopenmpt/libgme）；输出 ALSA/PulseAudio/CoreAudio；sqlite 媒体库；内建 websocket/http 流服务器；pdcurses/ncurses。

### 4.4 交互参照矩阵（默认键位，均出自官方文档/源码）

| 操作 | cmus【cmus.txt】 | ncspot【users.md】 | musikcube【wiki】 | termusic【key.rs Default】 |
|---|---|---|---|---|
| 播放/暂停切换 | `c`（暂停）；`x` 播放/重播；`v` 停止 | `Shift+P`；`Enter` 播放选中 | `Space`（列表）；`Ctrl+P` 全局；`Ctrl+X` 停止 | `Space` |
| 上一首 / 下一首 | `z` / `b`（`Z`/`B` 专辑级） | `<` / `>` | `j` / `l` | `N` / `n` |
| seek | `←`/`→` ∓5s；`h`/`l` ∓5s；`,`/`.` ∓1m | `B`/`F` ∓1s；`Shift+B/F` ∓10s | `u`/`o` ∓10s | `b` / `f` |
| 音量 | `+`/`=` +10%、`-` -10%；`[`/`]`、`{`/`}` 单声道 ±1% | `-`/`+` ±1%；`[`/`]` ±5% | `i`/`k` ±5%；`m` 静音 | `+`/`=` 与 `-`/`_` |
| 循环/随机 | `r` / `s`（`^R` 单曲循环） | `R` / `Z` | `.` / `,` | `m`（模式循环）/ `r` |
| 列表导航 | `j/k`、`g/G`、PageUp/Down | 同（Vim 风格，README 宣称） | ↑↓/PageUp/Home/End | `j/k/h/l`、`g/G` |
| 帮助 / 退出 | `:help` / `q` | `?` / `Q` | `?` / `Ctrl+D` | `Ctrl+h` / `q` |

- 惯例归纳：
  - **暂停**：`Space` 是最主流（musikcube/termusic）；cmus 因 Space 被"标记/展开"占用而用 `c`；ncspot 因 Space 是"加入队列"而用 `Shift+P`。
  - **切曲**：字母派（`n`/`p`、`N`/`n`、`j`/`l`、`z`/`b`）与符号派（`<`/`>`）并存，无统一标准。
  - **seek**：`←`/`→`（cmus/musikcube）最直观；`h/l`、`f/b` 是 Vim 变体。
  - **音量**：`-`/`+` 近乎统一（musikcube 的 `i/k` 是例外）。

### 4.5 鼠标支持现状

| 项目 | 滚轮 | 点击 | 进度条交互 |
|---|---|---|---|
| cmus | 列表上下行；标题栏滚轮=切换视图；**进度条上滚轮=seek ±5s；右键滚轮(进度条)=音量 ±1%** | 点击选中项=激活 | **点击进度条=暂停**（`mlb_click_bar`→player-pause）【cmus.txt 官方键位表】 |
| ncspot | 滚动列表 ✓（cursive） | 点击列表项播放 ✓（#1638/#1642 修复过冻结 bug）；2026-01 新增"禁用鼠标"选项 #1308 | 无 click-to-seek |
| musikcube | 滚轮滚动 ✓（2021 年 #401 加入，#383 降输入延迟） | 点击选择 ✓ | 无 click-to-seek |
| termusic | 滚轮滚动 ✓（#455 tmux 问题已修） | **点击操作是 open feature request**（#456，2025-03；#718 关为重复） | 无 |

- **关键结论**：四个参照项目**均无 click-to-seek 进度条**；唯一进度条鼠标交互是 cmus 的"点击=暂停"。滚轮调音量也仅在 cmus 有（右键滚轮限定进度条区域）。dlook 若实现 click-to-seek 属超出业界现状的新设计（技术上 crossterm 完全可行）。
- 进度条视觉形态：termusic = tui-realm-stdlib **Gauge（ratatui Gauge）**：圆角边框、主题前景/背景色、居中标题"Status | Volume | Speed | Gapless"、label 显示时间（progress.rs 源码）；cmus 2.12 = 状态行内进度条（可配置）；musikcube 有频谱可视化视图（`v` 键）。TUI 通用形态即 ratatui Gauge / `[██░░░░]` 块条 + 两侧 elapsed/total 时间戳。

### 4.6 dlook 键位冲突分析（Space 翻页 vs 播放暂停）

dlook 现有：`q`/`Esc` 退出、`j/k`/↑↓ 滚动、`Space`/PageDown 翻页、`g/G` 首尾、`⌫` 返回、`y`/`Enter` 复制选区；鼠标滚轮滚动、点击链接、拖选。业界处理 Space 冲突的三种模式：

1. **上下文覆盖**（musikcube/termusic 隐含模式）：音频播放上下文中 Space=暂停。dlook 进入音频预览态时可采用——代价是同一键在不同文档类型下语义不同，需状态栏明示。
2. **让位**（cmus 模式）：暂停用 `c`，Space 保留原语义（cmus 的 Space=标记/展开）。dlook 可保留 Space=翻页、用 `c`（或 `x`）暂停/播放——`c`/`x` 目前未占用。
3. **修饰键**（ncspot 模式）：`Shift+P` 等。终端 Shift+字母 输入依赖终端配置（大小写字符可行），dlook 现有键位已用 `G`（Shift+g）证明可行，但记忆成本高。

dlook 未占用且无冲突的候选：`←`/`→`（seek ±5s，Shift+←/→ 或 `,`/`.` ±1m）、`-`/`+`（音量 ±5%）、`n`/`p`（下一/上一曲）、`m`（静音）、`c` 或 `x`（播放/暂停，若不让 Space 覆盖）。`Enter` 已被"复制选区"占用，不宜再当"播放"（cmus 用 Enter=播放选中，dlook 不适用）。鼠标：滚轮维持滚动；进度条区域可考虑 cmus 式"点击=暂停/滚轮=seek"保守方案，click-to-seek 作为增强项。

---

## 五、关键未知 / 待检验假设

1. **rodio 0.23 发布时点与 API 稳定性**：#901 重写未完（阶段 3-6 未勾选），README 自称"under documented"。dlook 应 pin 0.22.x 并在升级窗口重新评估；假设"Player API 形态已稳定"**未验证**。
2. **PipeWire 纯环境表现**：rodio 0.22.2（cpal 0.17.3）只有 ALSA 后端，依赖 pipewire-alsa 兼容层；在无 pipewire-alsa/pipewire-pulse 的环境（极少数）可能无声。是否提前切 cpal 0.18 git 依赖获得纯 Rust PulseAudio——**未实测**（网络与社区证据支持纯 Rust 路径更稳，#883）。
3. **Opus 支持路线**：symphonia 上游无 ETA；备选 `symphonia-adapter-libopus`（C libopus）会破坏纯 Rust。产品决策：首发明确声明不支持 .opus，还是加可选特性。
4. **ALSA 静态链接**：musl + 自编译 alsa-lib 理论可行但**未验证**；默认 glibc 构建下 libasound.so.2 是硬运行时依赖，与"零运行时依赖"卖点的关系需产品层明确表述（建议表述为"单文件二进制，Linux 需 libasound，macOS/Windows 无额外依赖"）。
5. **高采样率内容**（96k/192k FLAC）underrun 修复的完备性未复测（#827/#856）。
6. **macOS/Windows 实测缺失**：cpal 官方声明覆盖三平台，但本调研无法在本地验证 CoreAudio/WASAPI 行为（含设备切换、蓝牙）。
7. **click-to-seek 无业界先例**：若 dlook 做，交互需自验证（crossterm 可上报点击列坐标，映射到百分比技术上无障碍）。
8. **MPRIS/媒体键**：termusic/ncspot 均将 MPRIS 与媒体键（tuirealm `Key::Media`）视为标配（souvlaki+zbus 纯 Rust 可达）；dlook 是否纳入超出本次范围，建议单独立项。

---

## 附：本地实测环境与命令

- 环境：Linux x86_64，rustc 1.98.1，alsa dev 1.2.16（pkg-config），本机亦装有 libpulse dev。
- 体积测试：`/tmp/audio-size/{base,rodio-slim,rodio-mid,rodio-full}`，统一 profile `opt-level="z" / lto="fat" / codegen-units=1 / strip=true / panic="abort"`（复制自 `rs/Cargo.toml`）。
- 发布物实测：ncspot v1.4.0 linux-x86_64（解包 20,769,584 B 未 strip）；termusic v0.13.2 x86_64-linux（termusic 24,385,184 B / server 18,767,584 B，已 strip，all-backends 构建）。

## 附：主要来源

- rodio：github.com/RustAudio/rodio（README、Cargo.toml、#883、#901、UPGRADE.md）、docs.rs/rodio/0.22.2、crates.io API
- cpal：github.com/RustAudio/cpal（README 平台矩阵、Cargo.toml）、crates.io（0.17.3/0.18.2 特性）、#1220、#1332
- symphonia：github.com/pdeljanov/Symphonia（README 状态表）、crates.io API（0.6.1 特性表、reverse_dependencies）、symphonia-core 源码（MSS 默认 64KB）
- ncspot：github.com/hrkfdn/ncspot（README、Cargo.toml、doc/users.md、#1308/#1638、release assets）
- termusic：github.com/tramhao/termusic（README、workspace Cargo.toml、lib/src/config/v1/key.rs、tui/src/ui/components/progress.rs、release.yml、#455/#456/#718、release assets）
- cmus：cmus.github.io、Doc/cmus.txt（KEYBINDINGS 默认表）、Doc/cmus-tutorial.txt
- musikcube：github.com/clangen/musikcube（README、wiki/user-guide、#401/#383）
