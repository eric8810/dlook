# dlook 媒体能力研究：音乐 / 视频 / 网页预览

## 问题与用途

用户要让 dlook（终端文档预览器）支持音乐播放、视频播放、嵌入式网页预览，且键盘/鼠标交互要舒服流畅。结论用于后续立项与实现决策（对应 DECISIONS 的下一个决策项），不直接产出代码。

**硬约束**（来自项目现状）：单文件纯 Rust 静态二进制（v0.4.0 为 9.21MB）、即时启动、D12「全功能优先于体积但仅限纯 Rust 依赖」、已有图片渲染基建（ratatui-image 11：kitty/sixel/iTerm2 + halfblocks 回退、ImageCtx 后台加载、SlicedProtocol 滚动、LinkSpan/Selection 内容坐标、200ms poll 事件循环 + dirty 计数重排）。

## 当前结论与建议

总体判断：**三类媒体能力全部可行，但各有清晰的能力边界与代价**；推荐「音频原生、视频委托、网页分层」的混合架构，全部交互收进统一的「媒体形态」键位表。

### 1. 音乐（音频原生，纯 Rust）

- **栈**：`rodio 0.22.2`（Player API：play/pause/`try_seek()`/set_volume/set_speed，无需自研引擎）+ symphonia 解码（rodio 默认全家桶）。**不要**直接用 cpal / libpulse-binding。
- **代价**：二进制 +1.52MB（默认格式集 flac/mp3/m4a/vorbis/wav；只留 mp3+flac+wav 则 +0.6MB）；**Linux 上新增 `libasound.so.2` 链接时依赖**（alsa-sys 非可选、非 dlopen）——macOS/Windows 无额外代价。README「零运行时依赖」表述需改为「零运行时环境（无 VM/解释器），Linux 音频需系统 ALSA 库」。
- **格式边界**：FLAC/MP3/Vorbis/WAV=Excellent，AAC-LC/ALAC(m4a)=Great；**Opus/HE-AAC/WavPack 不支持**（纯 Rust 无现成实现）。Opus 等 mpv 委托路径兜底（见下）。
- **风险**：rodio 正在引擎重写（0.22 已破坏 API `Sink`→`Player`），**锁 0.22.x 逐步跟**；rodio 0.22.2 锁 cpal 0.17.3（Linux 仅 ALSA 后端，PipeWire 走 ALSA 兼容层——本机 PipeWire 环境实测正常输出）；cpal 无设备热插拔通知（可接受的边界）；symphonia MPL-2.0（文件级弱 copyleft，静态链接合规，需保留其源码可得性声明）。
- **为什么不委托 mpv 做音频**：音乐是「文档类型」级功能——`dlook music.mp3` 应像 `dlook README.md` 一样开箱即用；且 dlook 自己掌握进度条/选区/链接 UI 才能保证交互统一。

### 2. 视频（委托 mpv，运行时可选；零二进制代价）

- **主路线**：检测到 `mpv` 时 spawn `mpv --vo=kitty --really-quiet --no-terminal --vo-kitty-alt-screen=no --vo-kitty-config-clear=no --input-ipc-server=<随机临时socket>`；控制全走官方 JSON IPC（pause/seek/volume/observe_property 事件流）。`--vo=kitty` 是 **mpv 官方 VO**（v0.36.0 起无条件编译），`--vo=sixel` 亦官方（需构建带 libsixel，生态已归档但发行版普遍有）。解码、丢帧、音画同步、音频输出**整体外包**——视频的音频问题顺带消失（含 Opus）。
- **帧率预期（条件性结论，勿当承诺）**：高配本机 kitty 终端上 720p@24fps 量级可达（mpv 开发者实测修 CPU 瓶颈后 1080p 达 24fps）；核显（UHD 620）高分掉帧证据在案，SSH 网络抖动另计。kitty 作者官方表态：协议不为此设计（整帧重画低效）。本机实测：vo=kitty 30 帧输出 1.48s/4.85MB（≈20fps 含启动开销）；vo=sixel 生成上限 ~400fps@640×360、8.5MB/s——生成端永不瓶颈，瓶颈在终端解析。**P2 立项前用 `experiments/test-60s.mp4` 在目标终端矩阵（kitty/ghostty/wezterm/SSH）实测 720p/1080p 一次再定帧率承诺**。
- **已知坑（源码级 + 复核实验 E6 证据）**：mpv **启动 reconfigure 与退出时各发一次** `\033_Ga=d` 删除终端**全部** kitty 图像（dlook 画面连带被清）——全量重绘的触发点必须含「mpv 启动完成后」与「退出后」两处；必须显式关 alt-screen；播放期间 dlook 不做全屏重排（帧序列与重排交叠会破图）。
- **集成形态（复核后升级，关键新事实）**：mpv vo=kitty 支持 **`--vo-kitty-left/top/rows/cols/width/height` 区域几何选项**（本机 `--list-options` 核实且被接受）——「共屏」形态从纯假设变为有官方选项支撑：
  - **MVP-A 共屏（首选候选）**：`--vo-kitty-top=<header行>` `--vo-kitty-rows=<body行数>` 把视频限制在 body 区域，dlook 底部 2 行媒体栏常驻（click-to-seek/滚轮热区等鼠标交互有宿主）；dlook 播放期只重绘自己的 chrome 行、不动视频区。**前置验证**（一条原型实验，需真实 kitty 终端，本环境无图形终端无法完成）：两进程输出在 tty 层独立 write 交错是否破图。
  - **MVP-B 全屏（保底）**：mpv 全屏画帧、dlook 暂停自身重绘但保留键盘焦点（`--no-terminal` 使 mpv 不抢输入），媒体键位翻译成 IPC；`Esc/q` 停止 → 全量重绘。若共屏原型失败则采用此形态，鼠标交互让位键盘。
  - **P2**（自研区域渲染）：`ffmpeg-sidecar`（纯 Rust spawn ffmpeg CLI 读 rawvideo）+ timg 验证过的管线（每帧 PNG 压缩 `f=100` + 双 ID 轮换 + 绝对时间锚定丢帧 + 后台编码线程），画面进 dlook 自己的 viewport 区域。工程量大，且同样依赖运行时 ffmpeg——仅当 mpv 共屏/全屏形态都不可接受时立项。
- **降级链**：mpv 播放 → ffmpeg 首帧静图 + 时长/分辨率元信息行（`ffmpeg -i -frames:v 1` 一次截图走现有图片管线）→ 纯文本文件信息行。
- **不推荐**：ffmpeg-next/ffmpeg-the-third（链接系统 FFmpeg，破坏静态性）；libmpv 嵌入（链接 C 库破坏静态性，render API 需 GL surface，与终端场景不匹配——spawn CLI + IPC 是正确形态）；GStreamer 委托（无 kitty/sixel 终端 sink，视频只能开 X 窗口；音频无优于 mpv 之处）；纯 Rust 解码（H.264/H.265 无可用实现，rav1d 仅 AV1）；chafa（官方无视频输入）。

### 3. 网页预览（三层）

**协议层否定结论已查证**：kitty/iTerm2/sixel 全部只是位图通道，不存在网页/交互嵌入能力；「终端里的网页」都是客户端渲染成像素（awrit，已归档）或字符（browsh/w3m）。

| 层 | 优先级 | 做法 | 代价 | 上限 |
|---|---|---|---|---|
| **L1 文本渲染** | P0 | ureq 抓 HTML（沿用 10s/16MB/重定向≤5 先例）→ `html2text 0.17 from_read_rich()` 的类型化注解（Link/Strong/Code/**Image(src)**）→ 映射 `Vec<Line>` + LinkSpan | 纯 Rust 静态，链接点击/历史栈/图片管线全免费复用 | 无 JS 页面的结构化阅读；`Image(src)` 注解直连图形协议内联图 |
| **L3 外部打开** | P1 | xdg-open/open 兜底（已有） | ≈0 | 完整交互页面 |
| **L2 图像快照** | P2 | 检测到 chromium 时 `--headless --screenshot --window-size=W,H --virtual-time-budget=N` → PNG → 现有图片管线；P2 时 A/B：CLI 截图（简单）vs `headless_chrome` crate CDP `captureBeyondViewport`（正规整页机制） | 运行时可选；缺席自动回落 L1 | JS/CSS 像素级保真的静态长截图（本机实测 1.0–4.0s；30000px 实测可行，保守承诺 16384px） |

- **安全先例（aerc 0.22）**：HTML 渲染不拉任何子资源（html2text 天然合规，CSS/字体永不取），页面内 `<img>` 仅显式打开后按图片管线限额拉取；防追踪像素。
- **HTML→markdown 中转（htmd）不作主路径**：实测泄漏 `<style>/<script>` 正文；html2text 无泄漏且注解类型化。
- **不内置** browsh（需 Firefox、单人维护、停滞）/awrit（归档）。

### 4. 交互设计（媒体形态统一键位/鼠标）

**模式模型**：M1 全屏媒体模式（`dlook music.mp3` / 点击媒体文件——媒体即文档）与 M2 mini 媒体栏（markdown 阅读中点开媒体链接——阅读优先，不推返回栈不换视图）。

**键位表（复核修订：Space 语义按 M1/M2 拆分，消除与 pager 翻页的矛盾）**；依据 mpv/musikcube/termusic/cmus/ncspot 惯例，全部核验过官方文档/源码：

| 键 | M1 全屏媒体 | M2 mini 栏（阅读中） | 惯例依据 |
|---|---|---|---|
| `Space` | **播放/暂停**（压倒性惯例；翻页让位给 PgUp/PgDn） | **保持翻页**（阅读场景不动摇；暂停走 `p`/`Shift+Space`/点击媒体栏） | mpv/musikcube/termusic 用 Space 暂停；ncspot Space=加队列是反面教材 |
| `p` / `Shift+Space` | 播放/暂停 | 播放/暂停 | cmus `c` 暂停的同位替代 |
| `←`/`→` | seek ±5s（`Shift+←/→` ±1s，`,`/`.` ±1min） | 同左 | mpv 标准键位；与 j/k 滚动零冲突 |
| `j`/`k`/`↑`/`↓` | **保持滚动**（M1 里滚动媒体信息/歌词） | 保持滚动 | pager 基因全程不丢 |
| `-`/`+`、`m` | 音量 ±5%、静音 | 同左 | ncspot/cmus/termusic/mpv 共识 |
| `<`/`>`、`0` | 上/下一首、回曲首 | 同左 | mpv+ncspot |
| `[`/`]`、`t` | 倍速、已播/剩余切换（P2） | 同左 | mpv / cmus |
| `Esc` | 链式：清选区 → 停止退媒体 → 退出 | 链式：清选区 → 停止播放撤栏 → 原有 Esc 行为 | 与现有 Esc 链同构 |
| `⌫`/`Alt+←` | 保持返回语义 | 保持返回语义 | dlook 导航核心不动 |
| `?` | 帮助视图（收编长尾键位、防 footer 爆炸） | 同左 | 全体播放器惯例 |
| 媒体键 | PlayPause/TrackNext… | 同左 | crossterm `KeyCode::Media` 需 Kitty 键盘协议，渐进增强；OS 硬件媒体键走 MPRIS（P3+，souvlaki 纯 Rust） |

**鼠标**：进度条行 click-to-seek（ncspot 公式 `x/width × duration`，命中测试比 LinkSpan 简单——固定 UI 行）+ 拖动 scrubbing（120ms 节流预览、松开提交，沿用 AUTO_SCROLL_INTERVAL 先例）；**滚轮按光标区域分语义**（进度条行=seek ±5s、音量热区=音量、文档区=滚动保持现状——cmus/ncspot 双先例）；点击信息行=播放暂停；中键=返回（w3m 先例）；文档区拖选/复制完全保持。**视频形态例外**：MVP-A 共屏形态下此表全部适用；若共屏原型失败退到 MVP-B 全屏形态，则播放期间鼠标无对象（dlook 无自绘行）、仅键盘经 IPC 生效——这是 MVP-B 的已知体验折损，鼠标交互规范的最终定稿以共屏原型裁决为准（见视频节前置验证）。

**状态栏**：底部 2 行媒体栏（行 1 `━` 进度条 + `01:23 / 04:56 (13%)`；行 2 ▶/▮▮ 图标 + 曲名 + 右端 `▣ 80%` 音量热区），挂现有 200ms poll 刷新；footer 分层（pager 不变 / M1 整体替换为媒体键位 / M2 只追加 `♪ p ⏯`——注意 M2 提示的是 `p` 而非 `space`，与上表一致），操作回显复用 1.5s TTL 状态消息。歌词跟随（P2）：LRC 时间戳 + 200ms 轮询查行滚动到锚点，手动滚动暂停跟随 3s。

**SSH 边界声明（复核补充）**：dlook 经 SSH 在远端运行时，rodio/cpal 输出到**远端** ALSA（声音从服务器出，不从用户耳机）；mpv 音频同理。产品行为：检测 `SSH_CONNECTION/SSH_TTY` 时状态栏提示「audio plays on remote host」，用户仍可继续播放（合法场景：远程桌面音箱/跳板机上外接声卡）。远程 URL 音频（`https://…/x.mp3`）**支持**——与图片的远程 URL 语义一致，复用 ureq 获取管线，symphonia `MediaSourceStream` 支持流式 Read。

### 5. 分期建议

| 期 | 内容 | 前置任务 | 依据 |
|---|---|---|---|
| MVP | 音频原生（rodio）+ M1/M2 媒体栏 + 全套键位/鼠标 + 网页 L1/L3 | rodio 0.22 锁版确认；M2 Space 语义已裁决（本表键位表）；实测 html2text 对 `<meta charset=gbk>` 等非 UTF-8 页面的解码行为（乱码则评估 encoding_rs 预转换，S 档） | 三个方向里体验最完整、依赖最干净 |
| P2 | 视频 mpv 委托（MVP-A 共屏优先、MVP-B 全屏保底）+ 降级链 + 网页 L2 快照 | ① 真实终端跑共屏原型（Hyprland+foot 已具备 V 场景自动化通道：`--vo-kitty-top/rows` + dlook 只重绘 chrome 行，grim 截图断言不破图）② 目标终端矩阵实测 720p/1080p 帧率后定承诺 | 零二进制代价；几何选项已核实存在；视觉验证通道已由 E11/E12 证明可行 |
| P3 | ffmpeg-sidecar 区域渲染、倍速/歌词、Opus（mpv 音频委托兜底）、`?` 帮助视图、MPRIS（souvlaki）、readability 正文提取（L1 降噪） | 按需 | 边际收益递减 |

### 6. 验证策略（E2E 优先；本节为立项后测试设计的依据）

**原则**：媒体功能的主验证路径全部是「真进程 + 真终端 + 真时间流逝」的 E2E（沿用现有 PtySession 套件模式）；unit test 只退守纯函数（时间码格式化、URL 解析、WAV 头解析）。三类「不可观测」的媒体量各有一个**已实验验证的可观测代理**：

| 不可观测 | 可观测代理（实验编号见 experiments/README.md） |
|---|---|
| 声音（听不到） | **E9**：null sink + monitor 录制 → RMS > 阈值 + Goertzel 频点能量（证明真实样本到达输出设备，全程零外放）；辅以 `pactl list sink-inputs` 出现客户端条目 |
| kitty/sixel 视频画面（pyte/tmux 不渲染图形协议） | **E11**：真实终端（foot/kitty）+ `grim` 窗口截图 + MD5 比对——播放中帧帧不同、IPC 暂停后完全冻结、恢复后又变、seek 后变化，四态可断言；**E12**：同一次会话内录屏 + 录音联合断言 |
| 控制链（键位操作是否真的作用到播放器） | **E10**：测试进程作为 mpv IPC **第二客户端**旁观——dlook 按 Space → 旁观者 `get_property pause==true` + 收到 `property-change` 事件（零 mock）；spawn 参数经 `/proc/<pid>/cmdline` 断言 |
| 播放真的在跑（无图形终端时） | 媒体栏时间码随墙钟推进（两次采样差 ≥ 间隔−容差）、暂停后冻结 ≥1s、seek 后立即跳变——TUI 状态即播放器状态机的投影 |
| 画面内容正确性（非仅「在动」） | 截图交视觉模型/人工复核（E11 已验证可读出时间码 `00:00:01.700` 与色条位移）；关键场景（首帧、seek 目标帧）可存为基线图做像素比对 |

**图形会话能力登记**（本机已核实，见 experiments/README.md 环境登记）：Hyprland 合成器、`grim` 截图、`wtype` 注入按键、`hyprctl clients -j` 取窗口几何、`magick` 图像处理、**foot 1.28（支持 sixel）**、PipeWire 音频链路。grim 采集上限 ≈16.6fps——足够状态判定，不做逐帧校验。

**场景矩阵**（编号接现有套件；O/P/Q 为 pty+pyte，V 为图形会话视觉套件，T37+ 为 tmux）：

| 场景 | 覆盖 |
|---|---|
| **O 音频** | M1 打开（媒体栏+footer 替换）、时间码推进/暂停冻结/seek 跳变、音量显示、Esc 链式退出、M2 mini 栏（Space 仍翻页、p 暂停）、远程 URL 音频、SSH 提示、无音频设备降级（headless CI 本身即用例）、E9 探针全量断言 |
| **P 视频（无图形终端部分）** | mpv spawn 参数（cmdline 断言）、`_G`/sixel 字节流存在、E10 双客户端控制链（Space/seek/音量）、mpv 退出后全量重绘+无僵尸进程、**降级链每级**：无 mpv→ffmpeg 首帧静图（halfblock 断言）→无 ffmpeg→信息行；fake-bin 伪 mpv（python 最小 IPC 实现，供无 mpv 的 CI）+真 mpv 扩展套件（本机有） |
| **V 视频/音频（图形会话视觉套件，本机可跑）** | E11 四态断言（播放中变化/暂停冻结/恢复/seek）、E12 音视频联合录制、dlook 自身图片渲染的视觉复核（0.4.0 已在 foot 实测 sixel 像素级渲染）、视频与 dlook UI 共屏不破图（P2 前置原型）、关键帧基线比对 |
| **Q 网页** | L1 渲染（标题/链接/表格）、链接点击导航+⌫ 返回、`<img>` 走图片管线、**安全 canary**（页面埋 img/css 子资源 URL，断言 server 日志里 dlook 从未请求）、GBK 页面无乱码（MVP 前置任务的 E2E 化）、chromium 缺席回落 L1、4xx/超时状态提示；L2 快照可加视觉复核（网页截图内容正确性） |
| **R 交互专项** | click-to-seek（SGR 点击进度条 x 列 → 时间码 ≈ x/width×duration，mpv 场景加 IPC 双重断言）、拖动 scrubbing（down→drag→up 节流+提交）、滚轮分区三断言（进度条行=seek/音量热区=音量/文档区=滚动）、footer token ≤8 |

**fake-bin 的定位**：仅用于 CI 可移植性（仓库已有 fake-bin/xdg-open 先例）；真依赖存在时必跑真路径（本机 mpv/ffmpeg/chromium/PipeWire/foot 全齐）。

**分层执行策略**：
1. **CI 层**（无图形/无音频设备）：O/P/Q/R 的断言 + E9 探针（null sink 不需真实声卡）+ E10 旁观客户端——可全自动。
2. **图形会话层**（本机 Hyprland，可自动）：V 场景——真实终端截图断言 + 音视频联合录制，本层已用 E11/E12 证明可自动化。
3. **人工/视觉复核层**：画面美感、共屏布局观感、真 SSH 链路体验——截图存档供视觉模型或人工判读（E11 证明可行）。

**已知边界（诚实声明）**：grim 截图不含终端图形协议的合成结果在某些合成器/终端组合下可能不可靠（本机 Hyprland+foot 已验证可靠）；逐帧帧率测量需外部摄像头或终端自身统计，截图法只能测 ≥ 截图周期的状态；SSH 真链路带宽影响仍需人工。

## 覆盖范围

已探索：终端视频输出全景（timg/mpv/chafa/VLC 源码级）、kitty 协议帧率实测数据、纯 Rust 音视频解码版图、rodio/cpal/symphonia 生态与版本陷阱（含本机构建实测）、网页渲染光谱（browsh/w3m/awrit/aerc/chromium headless/html2text/htmd 本机实测）、终端协议嵌入能力的否定性查证、五款播放器键位与鼠标的官方文档/源码核验、mpv JSON IPC 本机验证。

未覆盖/待检验：非 UTF-8 网页字符集（GBK）处理（已上浮为 MVP 前置任务）；html2text 布局表格噪音的 Decorator 压制；Kitty 协议媒体键在 foot/Ghostty/tmux 的实际到达率；mpv 终端 VO 下 `term-status-msg` 能否替代 dlook 媒体栏；ffmpeg-sidecar 端到端吞吐（1080p 软解→PNG→base64）；muskl+静态 alsa-lib 可行性；macOS/Windows 实测。

## 关键推理

1. **音频原生 vs 委托的裁决**：音乐播放是文件类型功能（打开 .mp3），单文件开箱即用是 dlook 核心卖点 → 原生 rodio；视频播放受终端协议物理限制（kitty 作者明言协议不为视频设计），注定是「增强体验」而非「基础能力」→ 委托 mpv 零代价获得专业级解码/同步/输出。两者代价对称地落在各自合理的一侧（音频付出 1.5MB 体积 + ALSA 链接；视频付出外部进程依赖）。
2. **协议层否定结论**排除了所有「终端原生嵌入网页」的想象空间，把网页预览收敛到「文本渲染（进程内）/图像快照（外部浏览器）/外部打开」三条真实路径；html2text 的类型化注解与 dlook 现有 Vec<Line>+LinkSpan 模型一一对应，是罕见的架构级巧合红利。
3. **交互冲突消解**（Space=翻页 vs Space=暂停）按模式拆分：M1 全屏媒体态 Space=暂停（翻页仍有专用键 PgUp/PgDn），M2 阅读态 Space 保持翻页、暂停走 `p`/`Shift+Space`/点击媒体栏——pager 能力在任何形态下零损失（复核 B1 裁决）。
4. **实验数据支撑**（本机）：mpv vo=kitty/sixel 输出验证、JSON IPC 全命令验证、chromium 整页截图 0.45–0.7s、ffmpeg 解码 8000fps（瓶颈在终端解析而非解码——自研管线无性能收益，只有 UI 掌控收益）。

支撑材料：[evidence-video.md](evidence-video.md) / [evidence-audio.md](evidence-audio.md) / [evidence-web.md](evidence-web.md) / [evidence-ux.md](evidence-ux.md) / [experiments/README.md](experiments/README.md)（含可复现实验记录）。

## 关键未知（影响下一步决策）

1. **mpv 播放期间 dlook 共屏的终端侧验证**：`--vo-kitty-left/top/rows/cols` 区域几何选项已核实存在且被接受（`mpv --vo=kitty --list-options`，几何参数实际运行有帧输出）；剩余未知是**真实 kitty 终端**上 mpv 帧序列与 dlook 局部重绘（仅 chrome 行）交错是否破图、`term-status-msg` 是否够用——一条集成原型即可裁决 MVP-A/B 形态（本环境无图形终端，无法完成，已列为 P2 前置任务）。
2. **rodio 0.23 API 走向**：引擎重写未完，升级窗口期决定锁版本策略（补充：rodio 0.22 的流打开 API 已是 `DeviceSinkBuilder::open_default_sink()` → `Player::connect_new(mixer)`，与旧版 `OutputStream::try_default` 完全不同——0.22 这一代 API 已剧烈变动过一次。MVP 实现应对音频播放层做薄封装，隔离 rodio API 面，降低 0.23 升级时的改动半径）。
3. **PipeWire 纯环境**（无 pipewire-alsa）的 ALSA-only 表现：决定是否提前上 cpal 0.18/rodio master。
4. **html2text 相对 URL/字符集**的预处理工作量：影响 L1 的完成度估算。
5. **目标终端矩阵的 720p/1080p 实测帧率**：视频帧率承诺的证据缺口（P2 前置任务②）。

## 复用条件

- 调研日期 2026-09-13；mpv v0.41.0、rodio 0.22.2、cpal 0.18.2、symphonia 0.5.5（rodio 0.22.2 实际携带；上游最新 0.6.1）、html2text 0.17、Chromium 152、ratatui-image 11（dlook v0.4.0 集成形态）。
- 版本升级（尤其 rodio/cpal 系）或 kitty 协议演进（部分帧更新提案）时，视频/音频结论需重查；键位惯例部分稳定。

## 复核结果（独立复核,2026-09-13）

**总判断：需要补充调查——三条主路线的技术裁决维持成立,但 4 个定向缺口需在立项前补齐（其中 2 个属交互完成条件,1 个属能力边界声明,1 个属表述强度）。不需要重新研究。**

### A. 抽查核实（结论维持的主张）

| 主张 | 核查方式 | 结果 |
|---|---|---|
| `--vo=kitty` 为 mpv 官方 VO（v0.36.0 起） | mpv v0.36.0 release notes 原文「vo_kitty: introduce modern sixel alternative」+ 本机 `--vo=help` | ✅ 成立 |
| rodio 0.22.2 → cpal ^0.17（Linux 仅 ALSA 后端） | crates.io API dependencies + 本机 cargo tree | ✅ 成立 |
| cpal `alsa` 依赖非可选（libasound 链接时依赖） | cpal master(0.19-dev) Cargo.toml Linux 段 `alsa = "0.12.1"` 无 optional + 本机 ldd | ✅ 成立（连 0.19-dev 依然如此） |
| 体积 +1.52MB | `/tmp/audio-size` 构建物实测 291,760→1,815,512 B,与报告数字逐字节一致 | ✅ 成立 |
| symphonia MPL-2.0、无 Opus | crates.io 0.6.1（上游最新版,license=MPL-2.0;特性表无 opus/he-aac/wavpack）;rodio 0.22.2 实际携带 0.5.5（本地 registry 核验,同样 MPL-2.0、无 opus/he-aac/wavpack） | ✅ 成立 |
| html2text 0.17 类型化注解 | docs.rs `render::RichAnnotation`：`Link(String)`/`Image(String)`/`Strong`/`Code`/`Preformat` 等 | ✅ 成立 |
| **`--no-terminal` + vo=kitty 仍输出画面**（原关键未知 1 的前提） | 复核实验 E6：与不关终端时输出几乎逐字节一致（629 个 `_G` 帧）,IPC pause/seek 正常 | ✅ **由假设变为已证实** |

推理链审查:音频原生/视频委托的对称代价裁决、协议层否定结论（位图通道）、网页三层优先级、M1 下 Space 覆盖（PgUp/PgDn+j/k 保留翻页滚动）——均成立。

### B. 决定性问题（立项前需补）

1. **M2 mini 栏下 Space 语义自相矛盾（交互规范缺口）**。evidence-ux §3.0 定义 M2「pager 键位保持 + 媒体栏鼠标热区」（用户在读文档）,但键位表与 footer 设计（「M2 只追加 ♪ space ⏯」）又声明「媒体形态下 Space 覆盖翻页」——M2 也是媒体形态。若 M2 下 Space=暂停,读文档的核心场景丢翻页,与 M2 设计动机（保住 pager 基因）直接冲突;若=翻页,则与 footer 提示矛盾。**影响**:MVP 交互规范不完备,两种读法实现结果不同。**裁决成本低**（设计决策）:建议 M2 下 Space 保持翻页、暂停走 `p`/点击媒体栏/Shift+Space,并把键位表按 M1/M2 拆开写。
2. **视频 mpv 全屏形态下鼠标交互无定义、媒体栏可见性未裁决（结构性缺口）**。交互交付（click-to-seek/scrubbing/滚轮分区）全部以「dlook 自绘媒体栏」为宿主;视频 MVP 形态里画面由 mpv 全屏绘制、dlook 冻结重排（坑 3 对策）,进度条行不存在——键盘经 IPC 有方案,**鼠标在视频形态无对象**。README 关键未知 1 已承认需要集成原型,但「全套键位/鼠标交互体验」这一完成条件在 P2 存在缺口。**区分证据**:一条原型实验——mpv `--vo-kitty-left/top/rows/cols` 区域定位 + dlook 仅重绘底部 2 行媒体栏,验证帧序列与局部重绘交错是否破图（dlook 输出与 mpv 输出在 tty 层是独立 write,序列各自完整即可行,未实测）。应作为 P2 前置任务。
3. **SSH 场景下音频/视频行为未定义（边界遗漏）**。网页部分讨论了 SSH（evidence-web §5.2）,但音频完全未提:dlook 经 SSH 在远端运行时,rodio/cpal 输出到**远端** ALSA,声音从服务器出而非用户耳机;mpv 音频同理。这不是技术错误而是产品边界缺失,与「优秀交互体验」目标相关。**补法**:检测 `SSH_CONNECTION/SSH_TTY` 时提示音频将从远端播放（或降级为元信息行）;写入 README 边界声明。相关:远程 URL 音频（https://…/x.mp3）是否支持也未声明（dlook 图片已支持远程 URL,存在一致性预期;symphonia `MediaSourceStream` 支持流式 Read,技术免费）。
4. **「≤720p@24fps 可达」表述强于证据（实验外推越界）**。支撑为 mpv 开发者机器单点实测（fa9c2a3 修后 1080p24）+ 本机 E1——但 E1 自身吞吐 ≈20fps（30 帧/1.48s,360p,pty 无人渲染,见复核 E7）,未达 24fps;核显（UHD 620）高分掉帧证据也在案。**影响**:若立项书把「流畅 720p」当承诺,核显/SSH 场景可能落空。**补法**:表述降级为条件性结论（「高配本机可达,需终端矩阵实测后承诺」）,P2 前用 `test-60s.mp4` 在真实 kitty 终端实测 720p/1080p 一次。

### C. 新事实（复核实验 E6 补充）

即使 `--vo-kitty-config-clear=no`,mpv **启动** reconfig 时也发 `_Ga=d` 清终端全部 kitty 图像（头+尾各一次）,比原报告「退出时清」更早。MVP 全屏形态影响小（视频盖住一切,退出重绘恢复）;M2（文档+mini 栏共存）与 P2 区域形态下,文档内嵌图片在 mpv 启动瞬间消失、播放期间空缺——对策仍是全量重绘,但触发点应含「mpv 启动完成后」而非仅退出后。

### D. 已确认不阻塞的后续问题

- **GStreamer 委托为何不可行未明写**:GStreamer 无 kitty/sixel 终端 sink（视频输出只能开 X 窗口）,作视频委托不可行;作音频委托无优于 mpv 之处。建议在「不推荐」清单补一句,防评审质疑。
- **libmpv 嵌入**（libmpv-rs,链接 C 库）:破坏静态性且 render API 需 GL surface,与终端场景不匹配——spawn CLI + IPC 是正确形态,建议补一句明示排除。
- **L2 实现选型**:CLI `--screenshot` 的 30000px window-size 是 hack（研究自己也建议保守 16384）;`headless_chrome` crate 走 CDP `captureBeyondViewport` 是正规整页机制,P2 时值得 A/B（均为运行时可选依赖,不影响裁决）。
- **readability 类正文提取**（纯 Rust）:可作为 L1 对布局表格/导航噪音的降噪增强（P3）,evidence-web 只讨论了 Decorator 压制,未提此选项。
- **MPRIS**（souvlaki,纯 Rust）:OS 硬件媒体键的 Linux 通路,研究建议单独立项,合理;建议在边界声明提一句「OS 媒体键走 MPRIS,P3+」。
- 研究自列的未覆盖项（GBK 字符集、musl 静态 alsa、macOS/Windows、媒体键到达率、ffmpeg-sidecar 端到端）维持原判:不阻塞立项,按里程碑补。

### E. 复核结论

音频原生（rodio 0.22.2 锁版）、视频委托（mpv CLI + JSON IPC）、网页三层（L1 html2text / L2 可选 chromium / L3 外部打开）三条路线裁决**全部维持**;MVP/P2/P3 分期结构维持。补齐 B1-B4（约 1 个设计决策 + 1 条原型实验 + 2 处边界声明 + 1 处措辞降级）后,答案满足「三条路线技术方案与边界可信、交互设计有依据」的完成条件。

### F. 复核后处理记录（主 agent,同日）

- **B1 已裁决并修入正文**：M1 Space=播放暂停、M2 Space 保持翻页（暂停=`p`/`Shift+Space`/点击媒体栏），键位表按 M1/M2 双列改写，M2 footer 提示同步改为 `♪ p ⏯`。
- **B2 已升级并修入正文**：复核期间核实 mpv vo=kitty **存在 `--vo-kitty-left/top/rows/cols/width/height` 区域几何选项**（`--list-options` 确认，几何参数实际运行有帧输出）——集成形态改为 MVP-A 共屏（首选候选，几何选项限定视频区+媒体栏常驻，鼠标交互有宿主）/ MVP-B 全屏（保底，鼠标无对象为已知折损）；共屏终端侧原型与帧率矩阵实测列为 P2 前置任务（本环境无图形终端，无法完成）。D 节非阻塞项（GStreamer/libmpv 排除理由、L2 CDP A/B、readability、MPRIS）均已补入正文。
- **B3 已修入正文**：交互节新增 SSH 边界声明（远端 ALSA 提示、远程 URL 音频支持声明）。
- **B4 已修入正文**：视频节帧率表述降级为条件性结论，P2 前置任务含目标终端矩阵实测。
- **C 节新事实已修入正文**：视频节「已知坑」更新为启动+退出两处 `_Ga=d` 全量重绘触发点。
- 结论：B1/B3/B4 关闭，B2 关闭至「P2 前置任务」粒度（与复核 E 节判断一致——原型属实现期验证，不阻塞立项）。研究满足完成条件。
