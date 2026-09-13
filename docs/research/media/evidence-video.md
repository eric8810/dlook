# 视频播放可行性调研（evidence-video）

- 调研日期：2026-09-13
- 范围：视频解码与终端播放。**音频输出、网页预览不在本文范围**（由其他调研负责）。
- 方法：全部结论基于上网核实的官方文档 / 源码 / 发行说明 / issue。证据分级标注：【实测】= 有公开测量数字；【官方】= 官方文档/源码/维护者声明；【社区】= issue/项目实践，无严格测量；【估算】= 本文算术推演，未实测。
- 关联文件：`docs/research/media/experiments/`（test-video.mp4、test-60s.mp4 可用于后续实测）。

---

## 一、当前结论

**判断：dlook 做视频播放可行，但不应在二进制里引入 FFmpeg（破坏单文件静态特性，且 Rust 生态没有可用的纯 Rust H.264/H.265 解码器）。推荐「运行时可选」两层路线：**

1. **增强路线（系统装有 mpv 时）：委托 mpv。**
   spawn `mpv --vo=kitty --really-quiet --no-terminal --vo-kitty-alt-screen=no --vo-kitty-config-clear=no --input-ipc-server=<唯一临时socket>`，协议选择复用 dlook 已有 picker 结论（kitty 优先，其次 sixel，用 `mpv --vo=help` 探测编译产物）；播放/暂停/seek/音量/进度全部走官方 JSON IPC。理由：
   - `--vo=kitty`（kitty graphics protocol）与 `--vo=sixel` 均为 **mpv 官方 VO**（vo=kitty 自 v0.36.0/2023-07 起，无条件编译；vo=sixel 需构建时 libsixel）；
   - 解码、丢帧策略、音画同步、音频输出全部由 mpv 承担——音频/A-V 同步问题**整体消失**；
   - dlook 二进制保持纯 Rust 静态，mpv 是可选外部命令，缺席时降级到路线 2。
   - 已知最大的坑（均有源码证据，见 §4.4）：mpv 退出/reconfigure 时发 `\033_Ga=d;` **删除终端内全部 kitty 图像**（会连带清掉 dlook 自己的画面，退出后需触发全量重绘）；alt-screen 默认开（必须显式关）；输出与 dlook 重排交叠会破图。

2. **内置降级路线（无 mpv 时）：ffmpeg-sidecar 子进程解码 + timg 式渲染管线。**
   `ffmpeg-sidecar`（纯 Rust crate，MIT，spawn ffmpeg CLI 读 rawvideo 帧，2026-05 仍在发版）作运行时可选依赖；渲染采用 timg 验证过的架构：每帧 **PNG 压缩**（kitty `f=100`）+ base64 分块、动画 **双 ID 轮换**、后台线程编码 + 独立写线程**绝对时间锚定 + 丢帧**。ffmpeg 同样是外部可选命令，缺席时退到半块字符静帧/低帧率。

**不推荐**：`ffmpeg-next` / `ffmpeg-the-third`（链接系统 FFmpeg 库，破坏静态单文件；`build/static` feature 源码编译 FFmpeg 极重）；纯 Rust 解码（H.264/H.265 无可用实现，rav1d 仅 AV1 且主要暴露 C API）；chafa（官方不支持视频输入）。

### 性能上限速览

| 路线 | 640×360@24fps | 1080p@24fps | 条件 |
|---|---|---|---|
| kitty 裸 RGB24+base64（mpv vo=kitty 默认） | ≈22 MB/s pty 流量【估算】，可跑【实测旁证】 | ≈200 MB/s【估算】；mpv 开发者机器实测修 CPU 瓶颈后可达 24fps【实测】，Intel UHD 620 掉帧【实测】 | 本机或高带宽 |
| kitty PNG 压缩帧（timg 路线） | ≈2–10 MB/s【估算：单帧 PNG 0.1–0.4MB，内容相关】 | 可行但 CPU 编码吃紧【社区：timg 用线程池并行编码】 | 通用，SSH 下最稳 |
| kitty `t=s` 共享内存（`--vo-kitty-use-shm`） | 「比转义序列快得多」【官方文档】，无公开数字 | 同左 | 仅本机、支持的终端更少 |
| sixel 逐帧全量重发 | 无公开 fps 数字【社区：xterm 明显慢，mpv 手册佐证】 | 不乐观 | 终端宿主差异大 |
| 半块/字符（tct、chafa 类） | 低帧率、低画质定位 | — | 兜底 |

24fps 达标的现实条件（综合证据）：**本机** + **kitty/WezTerm/Ghostty/Konsole 等 kitty 协议终端** + **≤720p** 用裸 base64 可行；更高分辨率或远程 SSH 场景需要 PNG 压缩（或 shm）。sixel 只适合「能动就行」的档位。

---

## 二、子问题 1：现有终端视频播放方案

### 2.1 timg（hzeller/timg，C++，GPL-2）

**机制**【官方源码，`src/` 目录】：
- 解码：`video-source.cc` 用 **FFmpeg 库**（AVFormatContext/AVCodecContext/SwsContext，链接 libav*，非子进程），swscale 缩放到「终端 framebuffer」尺寸，`SendFrames()` 按 `1/fps` 节奏把帧推给渲染回调。
- 渲染四选一：`kitty-canvas` / `sixel-canvas` / `iterm2-canvas` / `unicode-block-canvas`（半块/四分块），`-p<h|q|k|i|s>` 或自动探测。
- **kitty 路线的关键选择**（`kitty-canvas.cc`）：每帧 **PNG 编码**（`f=100`，自研 PNG 编码器 `timg-png.cc`，压缩级别可调 `--compress`）→ base64 → 4096 字节分块传输；`q=2` 不回显；**动画帧只用两个 ID 轮换**（源码注释：部分终端如 wezterm 按 ID 缓存 GPU 纹理，ID 用太多会「 overwhelm some terminals」）；tmux 下自动 `set -p allow-passthrough` 并用 unicode placeholder + `U=1,c,r` 包裹。
- **节奏控制**（`buffered-write-sequencer.h`）：解码/编码变延迟由**独立写线程 + 队列**吸收；帧时间以**动画第一帧为绝对锚点**（第 N 帧 = N/fps），避免累积漂移；允许跳帧（`allow_frame_skipping`），控制序列（如光标操作）永不跳过。PNG/base64 编码在 ThreadPool 里与写输出**并行**。
- fps：**无官方公布实测数字**。架构上以丢帧换节奏，慢终端表现为跳帧而非卡死。
- 【对 dlook 的直接启示】这套「后台并行编码 + 绝对时间锚定 + 丢帧」写序器，正是 dlook「后台线程 + dirty 计数重排」模式在视频上的正确扩展；双 ID 轮换可移植到 dlook 的 kitty 流式更新，避免 wezterm 纹理无限堆积。

### 2.2 mpv

四个终端向 VO 全部为**官方支持**【官方仓库 `video/out/vo.c` + `DOCS/man/vo.rst`】，状态如下：

| VO | 机制 | 编译条件 | 官方定位/限制 |
|---|---|---|---|
| `tct` | 真彩色半块字符（可选 256 色、plain 空格模式） | 无条件编译 | 「可能需要 `--profile=sw-fast`」；输出与其他终端输出**不同步**易破图，建议 `--really-quiet`；`--vo-tct-buffering=line|frame` 调批写 |
| `caca` | libcaca 彩色 ASCII | 需构建时 libcaca（`HAVE_CACA`） | 官方原话「**This driver is a joke**」 |
| `sixel` | libsixel 编码 | 需构建时 libsixel（`HAVE_SIXEL`） | 见下 |
| `kitty` | kitty graphics protocol | **无条件编译**（无第三方依赖，POSIX shm 可选） | 见下 |

**vo=sixel**【官方源码 `vo_sixel.c`（627 行）+ 手册】：
- RGB24 → libsixel；默认**静态 xterm256 调色板**（`--vo-sixel-fixedpal=yes`），动态调色板带**场景切换检测**（直方图色彩数变化超阈值才重建调色板，`--vo-sixel-threshold`），缓冲整帧后 POSIX `write()` 原子写出（`--vo-sixel-buffered`）。
- 限制（手册明说）：sixel 输出与 mpv 其他终端输出**不同步** → 破图，`--really-quiet` 推荐；**xterm 默认不开 sixel**（需 `xterm -ti 340`），且 xterm 默认**不显示超过 1000×1000** 的图像；终端像素尺寸探测因各终端 padding 报告差异而**易错**（`--vo-sixel-pad-x/y` 修正）；动态调色板在 xterm 上**慢**。
- 测试过的终端：mlterm、xterm。

**vo=kitty**【官方源码 `vo_kitty.c` + v0.36.0 发行说明 + 提交历史】：
- 由 Mia Herkt 于 **2022-12-21** 提交（commit 874e28f，"Introduce modern sixel alternative"），随 **v0.36.0（2023-07-23）** 首次发布；此后持续维护（2026-09 仍有修复提交）。**不是社区 PR/fork，是官方主干**。
- 实现：RGB24 → base64 → `a=T,f=24,q=2,m=1` 4096 字节分块**每帧全量重发**——**无压缩、无 placement 重放**；`C=1` 不动光标，先 `ESC[y;xH` 定位；自带 **SIGWINCH** 处理（resize 时 reconfig）；`--vo-kitty-use-shm`（`t=s` POSIX 共享内存，路径 base64 后传给终端）绕开转义序列传输；tmux/screen DCS passthrough（`--vo-kitty-auto-multiplexer-passthrough`）；位置/尺寸参数 `--vo-kitty-left/top/rows/cols/width/height` 可把画面嵌入指定区域。
- 实测数据：
  - 【实测】issue #13285（2024-01，mpv 0.37.0）：kitty/wezterm/konsole 下高分辨率（全屏）**大量掉帧**，低分辨率明显改善；环境 Intel UHD 620 / Arch。
  - 【实测】commit fa9c2a3（2025-02，Safari77→kasper93）："make kitty vo ten times faster by avoiding strlen"：修前 base64 模式播 1080p24 视频 **2 fps**（55% CPU 耗在 strlen/字符串拼接），修后 **24 fps**。→ 结论：该路径的瓶颈在**发送端 CPU（base64+分块拼装）**而非协议本身；修好后 1080p24 在开发者机器可跑满。
  - 手册定位："You may need to use `--profile=sw-fast`"（软解缩放要开快速档）。

**mpv JSON IPC**【官方文档 `DOCS/man/ipc.rst`】：
- `--input-ipc-server=<unix socket|命名管道>`（或 `--input-ipc-client=<fd>`）；协议 = 每行一个 JSON `{"command":["名",参数...]}` + `\n`，回复 `{"error":"success","data":...}`，可选 `request_id` 关联、`async` 异步命令。
- 控制面完备：`set_property pause/volume/speed`、`seek <秒> [absolute|relative]`、`get_property time-pos/duration/percent-pos/eosd`…；`observe_property` 订阅属性变更（进度条轮询/推送）、事件（`end-file`、`idle` 等）可驱动 UI。
- 官方警告：**无鉴权、无加密、`run` 命令可执行任意系统命令**，定位是本机控制（dlook 的 socket 必须放 `$XDG_RUNTIME_DIR`/`/tmp` 且带随机名，绝不能固定公开路径长期驻留）。
- Rust 生态：`mpvipc` crate（GPL-3.0，~1.4k 行，2026-02 更新，控制已存在实例）存在但小众；协议足够简单，dlook 自写客户端（serde + UnixStream）更干净且规避 GPL 传染。

### 2.3 chafa

【官方 README + 手册】**chafa 不能播视频**：输入只支持图片含动图（GIF 等动画图像格式），构建依赖（freetype/libjpeg/librsvg/libtiff/libwebp）中**没有 FFmpeg**，也无视频容器/编解码支持。输出倒是齐全（iterm/kitty/sixels/symbols 四种）。
- 动画相关能力：`--animate`、`--duration`、`--speed`（倍速或指定 fps）、`--watch`（**监视单文件变化即重绘**——这是社区「ffmpeg 逐帧写文件 + chafa --watch」式外部喂帧黑科技的基础，本质低帧率）。
- 「用 chafa 放视频」= 社区把 ffmpeg 抽帧管道接到 chafa 的做法，定位是 ASCII 艺术，画质/帧率都非目标。对 dlook 无增量价值（dlook 已有半块回退）。

### 2.4 VLC / mplayer

- **VLC**【官方仓库 master `modules/video_output/`】：唯一终端向输出是 `caca.c`（libcaca ASCII，可选构建模块）；**没有 aa**（aalib 已不在 master）、**没有 sixel/kitty 输出**。即 VLC 的终端路径=字符艺术，与 2026 年的图形协议生态脱节，无参考价值。
- **mplayer**：`-vo aa`（aalib）/`-vo caca` 是历史悠久的字符方案；mplayer 本体长期低维护。本文未单独上网复核其当前构建状态（对结论无影响，仅完整性记录）。mpv 即 mplayer 系的现代后继，终端图形能力已全面覆盖 mplayer。

---

## 三、子问题 2：kitty / sixel 协议的视频级帧率可行性

### 3.1 官方态度（kovidgoyal，kitty 作者）

【官方，kitty issue #2947（2020-09-01）】原话要点：
- 「我对为 kitty 开发视频播放器不感兴趣。视频通常还带声音。你们应该让 mpv 这类项目把 kitty graphics protocol 当后端，就像它们已有 ASCII 后端一样。」（→ 后来 mpv 真的这么做了，v0.36.0。）
- 「**真要高效地显示视频，需要支持部分帧更新的协议；整帧重画相当低效。**」——这是对 kitty 协议做视频的官方定性：**协议没有部分帧/delta 更新机制**，视频只能整帧重传。

【官方，kitty graphics protocol 文档】把 mpv 列为协议应用（"A video player that can play videos in the terminal"）；实现该协议的终端：kitty、**Ghostty、WezTerm、Konsole、iTerm2（部分）、Warp、wayst、st(patch)、xterm.js**。连续帧类应用先例：**desktui**（VNC 客户端，逐像素远程桌面）、**awrit**（Chromium 内嵌渲染）。**kitty 没有自带 video demo**，kovidgoyal 从未演示过协议放视频（他明确拒绝了 #2947）。

### 3.2 传输开销量级

【估算，本文算术】一帧 640×360：
- kitty `f=24`（RGB24）：640×360×3 = 675 KB → base64 ×4/3 ≈ **900 KB/帧**；24fps ≈ **22 MB/s** pty 流量。720p≈2.7MB/帧→78MB/s；1080p≈8.3MB/帧→**199 MB/s**。
- kitty `f=100`（PNG）：摄影内容单帧约 0.1–0.4 MB（内容相关）→ 24fps ≈ **2.4–9.6 MB/s**，比裸 base64 低一个量级；代价是发送端 CPU 编码（timg 用线程池并行 + 可调压缩级别摊平）。
- sixel：编码后大小强依赖内容与调色板；**逐帧全量重发**（无缓存复用机制）。无可靠公开 fps 数字。

【实测旁证】mpv vo=kitty 的 2fps→24fps 数据（§2.2）说明：发送端 base64/拼装 CPU 与 pty 写出是第一瓶颈，终端侧解码/上纹理是第二瓶颈（UHD 620 高分辨率掉帧）。两者都修好时 1080p24 可跑（开发者机器）。

### 3.3 24fps 可达性结论（按证据强度）

- **本机 kitty 协议终端 + ≤720p + 裸 base64：可达**【实测旁证 + timg 生产可用性】。
- **1080p：边缘**——mpv 开发者机器 base64 模式实测 24fps【实测】；核显机器掉帧【实测】。`--vo-kitty-use-shm` 官方称「快得多」但支持的终端更少且**仅本机**【官方文档】。
- **SSH/远程：pty 带宽主导**，应走 PNG 压缩帧（timg 的默认策略正是如此）【源码证据 + 算术】。
- **sixel：无达标证据**。定性证据：xterm 动态调色板慢/1000×1000 上限/默认关闭【官方手册】；FFmpeg-SIXEL 历史 demo 仅用 16 色寄存器【社区】；libsixel 维护状态恶化（见 §5.4 注）。定位为「能动就行」。

---

## 四、子问题 3：Rust 侧视频解码路径

### 4.1 绑定系统 FFmpeg（破坏静态特性）

- **`ffmpeg-next`**（zmwangx，crates.io 7.0M 下载）：v9.0.0（2026-08-05 发布，活跃）。FFmpeg 4+ 安全封装，基于 `ffmpeg-sys-next`，**默认经 pkg-config 链接系统 libav\* 动态库**；`build` feature（`static` + `build-lib-*`）可让 cargo 构建期源码编译 FFmpeg（含各外部编解码库），构建重、交叉编译痛苦。**与 dlook「单文件静态、纯 Rust 依赖」直接冲突**。
- **`ffmpeg-the-third`**（shssoichiro fork）：v6.0.0+ffmpeg-9.0（2026-08-09 发布），FFmpeg 5–9，同样 sys 绑定形态。结论相同。
- `video-rs`、`gstreamer` 等同理（链接系统 C 库）。

### 4.2 spawn ffmpeg 子进程（保持静态特性）——推荐

- **`ffmpeg-sidecar`**（nathanbabcock，MIT，2023-02 首发，v2.5.2 / 2026-05-30 发布，1.8M 下载）：纯 Rust ~3k 行，**无原生链接**。`FfmpegCommand::new().input(file).rawvideo().spawn()?.iter()?` → 阻塞迭代器吐 `frame.data`（raw RGB 像素）+ 从 stderr 解析进度/元数据/警告。支持命名管道多输出；有社区 async 版（async-ffmpeg-sidecar）。
- 注意：**default feature `download_ffmpeg` 会在构建/运行时自动下载 ffmpeg 二进制**（ureq/tar/xz2/zip 依赖）——dlook 必须 `default-features = false`，改为运行时探测系统 `ffmpeg`。
- 形态定位：**ffmpeg 变成「运行时可选外部命令」**，与「mpv 可选」同一模式；dlook 二进制仍是纯 Rust 静态。缺点：多一跳进程 + stdout 管道吞吐（rawvideo 640×360 RGB ≈ 675KB/帧 ≈ 16MB/s@24fps，管道开销可忽略但值得实测）；无音频帧路径（本文范围外）。

### 4.3 纯 Rust 视频解码现状

- **H.264 / H.265：不存在可用的纯 Rust 解码器**【lib.rs/multimedia/video 索引 + 生态检索】。生态里只有绑定类（openh264 = Cisco C 库绑定，可 vendor 编入但仅 baseline/main 且无 demux/音频；x264/openh264 均偏编码）与解析类（h264-reader 等，不做完整解码）。rav1e 是**编码器**，与解码无关。
- **AV1：`rav1d`**（memorysafety/rav1d，dav1d 的 Rust 移植）活跃（8.3k commits），stable Rust 可编译 x86_64/aarch64（x86 需 nasm，arm/riscv64 需 nightly）；**当前主要暴露 C API**（作为 libdav1d 的 drop-in 替换），原生 Rust API 仍是计划（issue #1252）；另有 `rav1d-safe` 早期包装。即便可用也只覆盖 AV1，主流 H.264/H.265 视频鞭长莫及。
- 结论：**「纯 Rust 解码」路线在当前生态下不成立**，不用评估。

### 4.4 音画同步 / 帧节奏（架构参考）

- **timg 模式**（§2.1，最直接可抄）：解码线程 → ThreadPool 并行做 PNG+base64 编码（std::future 占位）→ **BufferedWriteSequencer** 独立写线程按「第 N 帧必须在动画起点 + N/fps 时刻前写完」的**绝对时间锚**输出，来不及就**整帧丢弃**（不排队堆积），控制序列例外。
- **映射到 dlook**：现有「后台线程 + `dirty` 计数 → 事件循环重排」（`rs/src/images.rs`）天然是 latest-wins 单槽；视频只需加一层节流——解码线程按视频 pts 取「当前应显示的帧」，渲染循环只画最新帧，慢了跳帧。SlicededImage/占位行模式可继续承担首帧定位与滚动。
- kitty 静态图的「一次性传输 + placement 重放」优化对视频**无意义**（每帧像素都变）；视频的正确姿势是 timg 的「PNG 压缩 + 双 ID 轮换 + 全量重发」。

---

## 五、子问题 4：委托 mpv 的架构

### 5.1 可行性

官方组件齐备：`--vo=kitty`（或 sixel）渲染 + `--input-ipc-server` JSON IPC 控制 + `--vo-kitty-left/top/rows/cols` 区域定位 + `--really-quiet` 静默 + `--profile=sw-fast`。dlook 侧只需：spawn、连 socket、observe_property 进度、转发按键（IPC `keypress`）。

### 5.2 现成先例

- **「TUI 内嵌 mpv + IPC 控制」几乎无公开先例**【GitHub 仓库搜索 `mpv input-ipc-server tui` ≈ 0 结果】。GUI 前端（Celluloid/SMPlayer 等）走 libmpv 而非 IPC。CLI 控制器有 mpvc / mpvipc（控制独立 mpv 实例，非嵌入）。
- 最接近的先例是 **hunter**（rabite0/hunter，Rust 文件管理器）：预览栏用**独立工具 hunter-media + GStreamer** 播视频/音频（配置项 `media_autostart/media_mute/graphics_mode=kitty|sixel|unicode`），sixel/kitty 输出画进预览区。但项目 **2022-09 后停更**，且走的是 GStreamer 非 mpv【社区，README】。
- 结论：该架构**没有经过大规模验证的模板**，dlook 需要自己踩坑——但组件级证据（VO、IPC、定位参数）全部官方齐备，风险集中在集成层（见 §5.3）。

### 5.3 已证实的坑（及对策）

1. **退出/reconfig 清空所有 kitty 图像**【官方源码 vo_kitty.c】：`uninit()` 与每次 `reconfig()` 都发 `\033_Ga=d;\033\\`（无参 a=d = 删除**全部**图像，不只 mpv 的）→ mpv 退出或 resize 时 dlook 自己的文档图像/画面一并消失。对策：dlook 监听 mpv 退出（进程 wait + IPC `end-file`/`idle` 事件）后**主动触发一次全量重绘**；resize 场景接受画面闪断。
2. **alt-screen 冲突**【官方文档】：vo=kitty/sixel 默认 `alt-screen=yes`、`config-clear=yes`，会切备用屏并清屏，与 ratatui 的 alt screen 互相打架。对策：显式 `--vo-kitty-alt-screen=no --vo-kitty-config-clear=no`（sixel 同名）。
3. **输出交叠破图**【官方文档，sixel/tct 小节通用警告 + vo_kitty 直接 `write()` 到 stdout】：mpv 的帧转义序列与 dlook 的重排输出会交错产生破图。对策：播放期间冻结/让出该矩形区域（mpv 用 `--vo-kitty-left/top/rows/cols` 定位在预留框内），dlook 不再向该区刷字符；并保持 `--really-quiet`。
4. **键盘抢占**：mpv 默认读终端输入。对策：`--no-terminal`（关闭终端输入/状态行）+ dlook 捕获按键后经 IPC `keypress <name>` 转发。⚠ `--no-terminal` 下 VO 是否仍正常写 stdout **未实测**（见关键未知）。
5. **resize**：mpv vo_kitty 自带 SIGWINCH 重配置【源码】；但 dlook 布局变化（预览框移动）需要同步 mpv 几何——IPC 运行时热改 `vo-kitty-*` 选项是否生效**未验证**；稳妥做法是 kill + 重启 mpv 进程（秒级，可接受）。
6. **tmux/screen**：`--vo-kitty-auto-multiplexer-passthrough=yes`；tmux 需 ≥3.3 且 `allow-passthrough`【官方文档 + timg 源码同款处理】。
7. **mpv 缺席/能力未知**：启动前 `mpv --vo=help` 探测（kitty 无条件编译可依赖；sixel 未必编入）；缺席 → 路线 2（ffmpeg-sidecar）→ 再缺席 → 半块静帧。
8. **生命周期**：socket 放 `$XDG_RUNTIME_DIR`（或 /tmp）+ 随机名；Drop 时先 IPC `quit`、超时 kill；删 socket；播放页切换/退出必须清理，否则残留进程占着 tty 输出。
9. **安全**：IPC 无鉴权且 `run` 可执行任意命令【官方警告】——绝不能用固定可猜路径长驻。

### 5.4 附注：libsixel 维护状态（影响 sixel 生态判断）

【官方仓库】原作 saitoha 2020-01 失联；社区 fork `libsixel/libsixel` 又于 **2025-02-12 归档**（只读）。Rust 侧 dlook 已用 `icy_sixel`（纯 Rust）不受影响，但这说明 sixel 生态整体在收缩，视频路线押注 sixel 的风险高于 kitty。

---

## 六、关键未知（待实测/待确认）

1. **`mpv --no-terminal` + `--vo=kitty` 是否正常输出画面**（§5.3-4 对策的前提）。低成本实测：一条命令即可验证。
2. **IPC 运行时热改 `vo-kitty-*` 几何选项是否生效**（决定 resize 用热改还是重启进程）。
3. **`--vo-kitty-use-shm`（`t=s`）在 WezTerm/Ghostty/Konsole 的支持面**：mpv 手册只说「支持的终端更少」，具体名单无官方数据。
4. **sixel 视频帧率无公开数字**：xterm/mlterm/foot/WezTerm 各宿主差异大，若 dlook 要支持 sixel 视频需自测（可用 `experiments/test-60s.mp4`）。
5. **ffmpeg-sidecar 实测吞吐**：1080p 软解 → rawvideo 管道 → PNG 编码 → base64 的端到端帧率未测（估算 720p 可行，1080p 吃紧）。
6. **timg 无官方 fps 数字**；timg 在慢终端上的表现依赖其跳帧机制（表现为丢帧不卡死）——结论为架构性判断而非实测。
7. **pty 在高吞吐下的行为**：199MB/s（1080p 裸 base64）在真实 pty + kitty 的端到端表现仅有 mpv 开发者机器单点数据；不同终端（WezTerm/Ghostty）终端侧解码上纹理成本未测。
8. mplayer `-vo aa/caca` 当前构建状态未复核（低优先级，不影响结论）。

---

## 附：证据源清单（检索于 2026-09-13）

- timg：github.com/hzeller/timg（README、`src/kitty-canvas.cc`、`src/buffered-write-sequencer.h`、`src/video-source.h`）
- mpv：仓库 `video/out/vo_kitty.c`、`video/out/vo_sixel.c`、`video/out/vo.c`（编译守卫）、`DOCS/man/vo.rst`、`DOCS/man/ipc.rst`；release v0.36.0（2023-07-23，"vo_kitty: introduce modern sixel alternative"）；commit fa9c2a3（1080p24: 2fps→24fps 实测）；commit 874e28f（2022-12-21 引入）；issue #13285（高分掉帧实测）、#9605（kitty backend 请求）、#16299
- kitty：sw.kovidgoyal.net/kitty/graphics-protocol/（应用与实现终端列表）；issue #2947（kovidgoyal 拒绝视频播放器 + 部分帧更新论断）
- chafa：github.com/hpjansson/chafa README + hpjansson.org/chafa/man/
- VLC：github.com/videolan/vlc `modules/video_output/`（master 仅 caca.c）
- Rust：crates.io API（ffmpeg-next 9.0.0、ffmpeg-the-third 6.0.0+ffmpeg-9.0、ffmpeg-sidecar 2.5.2、mpvipc 1.3.1、openh264）；lib.rs/multimedia/video；github.com/memorysafety/rav1d README；github.com/nathanbabcock/ffmpeg-sidecar README
- libsixel：github.com/libsixel/libsixel（归档公告 + README：原作者失联、FFmpeg-SIXEL/RetroArch 先例）
- hunter：github.com/rabite0/hunter README（hunter-media/GStreamer、2022-09 停更）
