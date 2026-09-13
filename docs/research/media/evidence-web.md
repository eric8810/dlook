# 网页预览可行性调研（evidence-web）

- 调研日期：2026-09-13
- 范围：终端里的网页/HTML 预览（markdown 链接指向的网页、本地 HTML 文件）。**音频/视频预览不在本文范围**（见 `evidence-video.md`）。
- 方法：结论基于上网核实的官方文档 / 源码 / 发行信息，辅以本机实验（Arch Linux + Chromium 152.0.7977.82、Rust 1.x，2026-09-13）。证据分级：【实测】= 本机跑过且记录了数字或输出；【官方】= 官方文档/源码/维护者声明；【包证据】= 从发行包内容直接核实；【社区】= issue/项目实践；【推断】= 逻辑推演，未实测。

---

## 一、当前结论

**判断：终端协议层不存在任何「网页/交互内容嵌入」能力（kitty graphics protocol、iTerm2 inline images、sixel 全部只是位图传输通道，已查证官方规范）。因此 dlook 的网页预览只有两条真实实现路径：文本渲染（进程内，纯 Rust）与图像快照（外部浏览器进程，可选增强）。推荐分三层实现，优先级：**

1. **L1 纯文本渲染层（P0，核心层）**：`ureq` 抓 HTML（沿用图片先例 10s/16MB/重定向上限）→ `html2text 0.17` 的 `from_read_rich()` 渲染成 `Vec<TaggedLine<Vec<RichAnnotation>>>` → 映射为 dlook 的 `Vec<Line>` + LinkSpan。关键发现（本机实测）：`RichAnnotation` 是**类型化注解**（`Link(url)`、`Strong`、`Emphasis`、`Code`、`Preformat`、`Image(src)`、`Colour`），与 dlook 现有模型一一对应——链接点击/历史栈/`Image(src)` 走既有图片管线**全部免费复用**。无 JS、无 CSS 布局保真是其能力边界。
2. **L3 外部打开兜底（P1，与 L1 同期做）**：`xdg-open`/`open` 委托系统浏览器，永远可用，覆盖 L1/L2 都无能为力的场景（需要交互/WebGL/登录的页面）。实现成本≈0。
3. **L2 图像快照层（P2，运行时可选增强）**：检测到 `chromium`/`chrome-headless-shell` 时，`--headless --screenshot --window-size=W,H --virtual-time-budget=N` 生成 PNG → 直接进 dlook **已有的 ratatui-image 管线**（kitty/sixel/iTerm2/halfblocks 自动降级）。本机实测：example.com 全程 1.0s、HN 1.8s、GitHub 2.5–4.0s；`--window-size=1280,30000` 在 Chromium 152 下可出整页图（30000×1280 PNG 185KB）。缺席浏览器/纯文本终端时自动回落 L1。它是「所见即所渲染」的视觉真值，但静态不可点（链接导航仍靠 L1 文本层承载）。

**明确不推荐**：把 browsh / w3m / awrit 作为 dlook 的内置依赖（Firefox/独立浏览器引擎是重运行时依赖，违反 dlook 哲学；awrit 已归档）；自研或引入 JS 引擎（servo embedding 路线不存在成品，成本失控）。w3m 一类更适合作为 aerc 式的**用户可配置外部 filter** 留个口子（P3 可选）。

**HTML→markdown 中转路径（htmd 0.5.5）实测可行但不作为主路径**：结构转换质量很好，但会把 `<style>`/`<script>` 内容当正文泄漏（kitty 文档页开头 2 行 CSS/JS 泄漏），且相对 URL 不解析；而 html2text 无泄漏、注解类型化，直接对齐 `Vec<Line>` 模型。markdown 路线仅在「想让网页复用 termimad 全套排版」时有吸引力，需要额外的 DOM 预清洗，性价比低于 L1 直渲染。

### 分层速览

| 层 | 依赖形态 | 体验上限 | 降级链 |
|---|---|---|---|
| L1 文本渲染 | 纯 Rust crate（html2text + html5ever 系），静态编译进二进制 | 无 JS 页面的结构化阅读：标题/链接可点/表格/代码块/图片 alt，图片本体还能走现有图形协议 | SSH/纯文本终端下表现一致 |
| L2 图像快照 | 运行时可选外部进程（系统 chromium） | JS/CSS 完整保真的「网页快照」，像素级真实 | 无浏览器/无图形协议 → L1；halfblocks 兜底 |
| L3 外部打开 | 无（调用系统 opener） | 完整交互网页 | 永远可用 |

---

## 二、子问题 1：终端里的网页渲染光谱

### 2.1 browsh：真实浏览器引擎 → 文本块

【官方】brow.sh 与 GitHub README：

- **架构**：headless Firefox + 必需的 Web Extension（在页面里注入 JS，把页面改造成 browsh 可消费的形态）+ Go 编写的「interfacer」（websocket server），把渲染结果以文本/颜色块实时推给 TTY 客户端或 HTML 客户端。三层组件缺一不可。
- **能力**：官方声明「renders anything that a modern browser can: HTML5, CSS3, JS, video and even WebGL」——因为内核就是 Firefox（JS/CSS 全支持，视频/WebGL 是把画面栅格化为色块）。
- **部署成本**：发行版是 ~11MB 静态二进制，但**唯一硬依赖是本机已装 Firefox 57+**；Docker 镜像 ~230MB。对 dlook 而言这是「用户必须另装一个浏览器」级别的运行时依赖。
- **活跃度**：19,035 stars；最新 release **v1.8.3（2024-01-29）**，仓库最后 push 2025-07-11，官方自述「currently maintained and funded by one person」，SSH demo 与浏览器版服务「Temporarily offline」。单人维护、低频发版。LGPL v2.1。

【推断】结论：架构参考价值高（「浏览器引擎 → 终端像素/字符」的鼻祖），但不适合作为 dlook 依赖：Firefox 是重依赖且 browsh 是常驻浏览器而非单页渲染器。

### 2.2 w3m / lynx / links2：自研 HTML 解析 + 文本排版，外加 X11 贴图

【官方+包证据】三类经典文本浏览器都是**自带 HTML 解析器和文本布局引擎的独立进程**，不是可嵌入的库；均无 JS（w3m、lynx）。links2 手册页无 JS 选项（其源码有实验性 JS 但发行版普遍不启用）【官方：Ubuntu links2(1) 手册页只字未提 JS；社区共识为实验性/默认禁用】。links2 还有 `-g` 图形模式（X/svgalib/fb 驱动，直接开 X 窗口），`-dump` 把排版后文本打到 stdout——aerc 就是用这个形态消费它们的。

**w3mimgdisplay 的图片内嵌原理（重点查证）**【官方：tats/w3m 镜像源码 `w3mimg/x11/x11_w3mimg.c`】：它**不是终端协议**。程序流程：打开 X display → 取得 `xi->parent`（终端模拟器的 X 窗口）→ `XCreateGC(display, parent, ...)` 创建 GC → 用 `XCopyArea` 把图像 Pixmap **直接画在终端模拟器的 X11 窗口表面上**，与终端自己画的字符叠加。推论（与众所周知的行为一致）：滚动/重绘时终端不知道图像存在会被字符覆盖出残迹、只工作在 X11（Wayland 原生/tmux/无 DISPLAY 均失效）、逐 terminal 适配脆弱。这是「绕过终端、直接往宿主窗口贴图」的上古 hack，与 kitty/sixel 协议是两个时代的东西。

【官方：ArchWiki w3m】现代 w3m 已内置图形协议选择：`inline_img_protocol 4` = kitty graphics protocol，`3` = iTerm2 协议（0 = 传统 w3mimgdisplay）。即连 w3m 自己都在向终端图形协议迁移。

### 2.3 headless Chromium 截图 → 终端图形协议（L2 的技术核）

【官方】Chromium headless 现状（developer.chrome.com + chromium 源码 README）：

- Chrome 132 起 `--headless=old` 已从 Chrome 二进制移除；旧 headless 功能独立为 **`chrome-headless-shell`**（Chrome for Testing 下载），新 `--headless` 与有头模式统一代码。
- CLI flag 语义（源码 `headless/app/headless_shell_switches.cc` 原文）：
  - `--screenshot`：「Save a screenshot of the loaded page」——**截取的是 window-size 视口**，不是自动整页（官方博客原话：full page screenshots "things are a tad more involved"，需 CDP `captureBeyondViewport`/Puppeteer `fullPage`）；
  - `--window-size=W,H`：设初始窗口尺寸，即截图视口尺寸；
  - `--virtual-time-budget=<ms>`：官方注释——虚拟时间在网络请求未完成时不前进，请求完成后定时器快进，直到预算耗尽才认为页面 ready。**这是等待 JS 渲染完成的确定性机制**；
  - `--timeout=<ms>`：硬停机；`--hide-scrollbars`：隐藏滚动条。
- 【社区】Lighthouse 定义 `MAX_SCREENSHOT_HEIGHT = 16384`（设备像素），超出需多段截图拼接（issue #11121 实测截图在 16300+px 处截断，即 16384 是那一带 CDP 截图的真实上限）。

【实测】本机（Chromium 152.0.7977.82，`--no-sandbox --disable-gpu`）：

| 实验 | 参数 | 耗时 | 产物 |
|---|---|---|---|
| example.com | `--window-size=1280,2000 --virtual-time-budget=5000` | 1.03s | 1280×2000 PNG 23KB |
| example.com | `--window-size=1280,16384` | 1.37s | 1280×16384 PNG 107KB |
| example.com | `--window-size=1280,30000` | 2.60s | **1280×30000 PNG 正常产出**（185KB） |
| news.ycombinator.com | `--window-size=1280,8000 --virtual-time-budget=8000` | 1.79s | 260KB |
| github.com/ratatui/ratatui | `--window-size=1280,4000 --virtual-time-budget=8000` | 4.02s | 747KB |
| 同上（**无** virtual-time-budget） | 同尺寸 | 2.53s | 466KB |

要点：① 单页截图 1–4s 量级，与 dlook 现有图片后台加载的交互模式兼容（占位→就绪）；② 当前 Chromium 的 `--window-size` 高度不被 clamp（30000px 出图），16384 上限属于旧 CDP 时代/Lighthouse 保守值；③ 对比有/无 `--virtual-time-budget`：4.0s/747KB vs 2.5s/466KB——预算确实换来更多 JS 内容完成渲染；④ 内容真实性由体积差异佐证（近空白的 example.com 23KB vs GitHub 747KB）。

**可行性结论**【推断】：L2 = `chromium --headless --screenshot --window-size=<内容宽>,<限高> --virtual-time-budget=<5-10s> --timeout=<15s> URL` → PNG → dlook 已有 `ratatui-image` 管线（picker 探测 kitty/sixel/iTerm2，halfblocks 兜底）→ 长图按 `MAX_IMG_ROWS` 类逻辑分页/切片展示。dlook 无需写任何图形代码，只是把一个 PNG 喂进现有管线。

### 2.4 其他「网页→图像进终端」项目

- 【官方】**awrit**（kitty 官方 graphics protocol 文档收录）：Electron/Chromium 常驻进程把页面帧持续经 kitty 协议投进终端，鼠标键盘双向交互（kitty v0.31+ 体验最佳）。证明「真实网页以图像形式活在终端里」可行。但仓库 **2026-04-25 归档**，作者明言无暇维护、安全债上升，推荐 macOS 用户转 cmux。
- 【官方】**cmux**（awrit 精神续作）：走的是完全不同的路——基于 Ghostty 的 macOS 终端应用，**原生分栏内嵌浏览器**（split pane + scriptable API），而非任何终端协议。这是「协议层做不了交互网页」的最有力旁证：做终端的人最后选择了终端应用原生 UI。
- 【官方】**monolith**（Rust，crates.io 有包）：把任意网页打包成单文件 HTML（CSS/图片/JS 全部 data URL 内嵌），供浏览器离线呈现。**它是保存器不是渲染器**——对 dlook 的价值至多是「把网页存成 .html 再用 L1/L2 预览」的伴生工具（可作为 P3 特性：save-link-as），不是预览路径本身。SingleFile 是其浏览器扩展等价物。
- 【社区】playwright/CDP 截图 + sixel 显示的散装组合（puppeteer-webshot-cli 等）存在，但没有形成知名 CLI 成品；「URL→终端图像」这条链上知名成品只有交互式的 awrit（已死）和文本式的 browsh。dlook 的 L2 是把标准零件拼起来，无先发竞品。

---

## 三、子问题 2：纯 Rust 的 HTML→结构化文本渲染

### 3.1 crate 生态定位（版本/活跃度核实于 crates.io API，2026-09-13）

| crate | 版本 | 最近更新 | 定位 |
|---|---|---|---|
| html5ever | 0.40.0 | 2026-09-11 | Servo 出品的浏览器级 HTML5 解析器（解析→DOM，**不含渲染**） |
| lol_html | 3.0.1 | 2026-07-29 | Cloudflare 流式 HTML **改写器**（CSS 选择器 API），不是渲染器 |
| scraper | 0.27.0 | 2026-05-11 | html5ever DOM + CSS 选择器**查询**，不渲染 |
| **html2text** | **0.17.1** | **2026-04-19** | **唯一的「HTML→宽度约束文本」渲染器**，正是 dlook 需要的东西 |
| ammonia | 4.1.4 | 2026-07-22 | HTML 白名单清洗（安全场景备用） |

即：解析/查询/改写生态齐全，**渲染只有 html2text 一家**（其本身基于 html5ever + markup5ever_rcdom，纯 Rust）。

### 3.2 html2text 输出质量【实测】

本机建最小工程（`from_read_rich(html, 80)`，逐行取 `TaggedLine` 的 `TaggedLineElement::Str(TaggedString{s, tag})`，tag 即 `Vec<RichAnnotation>`）：

- **结构**：`<h1>`→`# 标题`、列表→`* 项`、表格→box-drawing 对齐表（`┬─┼─┴`）、`<pre>`→原样、`<blockquote>`→`> 引用`、图片→alt 文本行。
- **注解（可编程消费，非 ANSI 字符串）**：`Link("https://…")`、`Strong`、`Emphasis`、`Code`、`Preformat(bool)`、`Image("pic.png")`、`Colour`/`BgColour`（css feature）、`FragmentStart(name)`（HTML 锚点零宽标记，可做页内跳转）。HN 真实页：每行标题/用户/评论链接的 href 全部拿到。
- **无 style/script 泄漏**：kitty 文档页（furo 主题，内联 CSS+GA 脚本）输出干净，与 htmd 形成对照（见 3.4）。
- **已知坑（实测确认）**：① 相对 href 不解析（`user?id=x`、`../conf/`），需 dlook 用 base URL + url crate 归一化；② 布局表格页（HN）会把布局表格也渲染成带边框表格——可读但有视觉噪音；③ `<style>` 内容虽不泄漏，但 `display:none` 的隐藏内容默认仍会输出（需开 css feature 才尊重）。

【官方】html2text 自带：`html2term` 交互式终端查看器示例（验证了「渲染成 TUI」这条路本身就是它的设计用途）；`css` feature 支持基础选择器（class/element/hash/child 组合器）与 `color`/`background-color`/`display:none`/`white-space` 属性；`from_read_coloured()` 可注入自定义 colour_map 产 ANSI 字符串（可走 dlook 的 ansi-to-tui 现成管线，但会丢类型化 Link 信息，故推荐 from_read_rich 直映射）。

### 3.3 与 dlook 模型的对齐【推断】

`RichAnnotation` → dlook：`Link(u)`→LinkSpan（内容坐标已天然具备）；`Strong/Emphasis`→Span 样式；`Code/Preformat`→现有代码样式/高亮管线；**`Image(src)`→现有 images.rs 注册表（后台加载 + 图形协议渲染 + 占位/失败行）**——即 HTML 预览里图片可以直接用 dlook 已有的整套图片逻辑，HTML 与 markdown 预览共享同一渲染底座。`FragmentStart`→页内锚点跳转（与 markdown 标题锚点机制同构）。

### 3.4 HTML→markdown 路径（对照实验）【实测】

htmd 0.5.5（turndown.js 风格）：

- 结构转换质量**好**：标题/粗斜体/链接/表格/代码块/引用/图片 全部正确产出 markdown；HN 页产出干净的有序列表+行内链接，比 html2text 的边框表格噪音小。
- 但喂 kitty 文档页（真实复杂页面）时：**`<style>`/`<script>` 内容作为正文泄漏**（输出开头 2 行 CSS/JS 混入），相对链接同样不解析，布局空链接（如投票箭头 `[](vote?...)`）产生空链接噪音。必须先用 scraper/ammonia 类工具剥 `script/style/nav` 才可用。
- html2md 0.2.17 亦存在（活跃度尚可），定位类似。

结论：markdown 中转会「先降级成纯文本、再让 termimad 重新排版」，多一跳且要自担清洗；from_read_rich 一跳到位且信息保真更高。markdown 路线仅在想 100% 复用 termimad 排版细节（主题表格样式等）时才有意义，不推荐作主路径。

### 3.5 已有 TUI 应用的 HTML 渲染先例

【包证据】**aerc 0.22.0**（从 Arch extra 包直接抽取核实，2026-09 构建）：

- 默认配置 `text/html=! html`：`!` 前缀表示「需要 TTY 的 filter」，指向内置 filter 脚本 `html`——脚本内容是 **`w3m -I UTF-8 -O UTF-8 -T text/html -s -graph -o display_link=true …`**：stdout 接管道时 `-cols 100 -dump`，接 TTY 时交互模式。同目录还有 `html-unsafe`。
- 配置注释里给出替代方案：`#text/html=pandoc -f html -t plain | colorize`、`#text/html=! w3m -T text/html -I UTF-8`——filter 体系 = `sh -c` 管道、按 MIME 类型首个匹配生效、用户可完全替换。
- **安全设计（对 dlook 最有价值的先例）**：默认 filter 里，HTML 渲染**默认禁网**——优先 `unshare --map-root-user --net`（网络命名空间隔离），退化用 socksify 指向死地址、再退化用无效 http 代理；`no_cache=true -o use_cookie=false`；仅当脚本名为 `html-unsafe` 才放行网络。动机即「渲染 HTML 邮件防追踪像素/phone-home」。
- 官网对功能的表述：**「Render HTML emails with an interactive terminal web browser」**。

即：成熟 TUI 应用的选择是「外部进程文本浏览器 + 严格禁网 + 可配置 filter」，而非进程内渲染引擎。dlook 的 L1（进程内纯 Rust）在能力上对标 w3m 路线，但保住了单文件静态卖点；aerc 的「渲染时禁网、显式 opt-in 才联网」应作为 dlook 远程 HTML 预览的安全蓝本。

---

## 四、子问题 3：终端协议层的「嵌入」能力核查

**结论（否定性结论已查证成立）：kitty graphics protocol、iTerm2 inline images、sixel 均为纯位图通道，不存在任何形式的网页/HTML/交互内容嵌入能力；三家的规范全文无此类机制，roadmap 亦无。**

- 【官方】**kitty graphics protocol**（sw.kovidgoyal.net/kitty/graphics-protocol/）：开篇目标即「render **arbitrary pixel (raster) graphics**」；设计原则第一条「Should not require terminal emulators to understand image formats」——客户端自己栅格化，终端只收像素（`f=100` PNG / `f=24` RGB / `f=32` RGBA，base64 载荷）。能力边界：像素定位、文字上下混合、alpha、随文字滚动、unicode 占位符、本机共享内存优化。规范中**唯一与「网页」相关的内容**是应用列表——awrit（Chromium→像素）与 mpv（视频→像素），都是**客户端渲染成位图**再进协议。协议不执行、不解释、不嵌入任何内容。
- 【官方】**iTerm2 inline images protocol**（iterm2.com/documentation-images.html）：OSC 1337 `File=` 序列传 base64 文件内容（内联图像，含动态 GIF）+ 文件传输；iTerm2 同时实现 kitty 协议。无网页概念。
- 【官方】**wezterm**（wezterm.org/imgcat.html）：实现 iTerm2 协议 + kitty 协议，文档仅涉及图像显示。无网页概念。
- 【官方】**sixel**（libsixel README）：DEC 1980 年代「printer and terminal imaging」图像格式，转义序列形态的栅格位图；动画靠 GIF 逐帧流（FFmpeg-SIXEL 视频同理，逐帧位图）。无网页概念。
- 【官方】**ghostty**：VT 文档列出的是常规控制序列 + kitty graphics/sixel 图像支持；无任何 web 嵌入序列。（旁证：cmux 作为「Ghostty-based 终端」要内嵌浏览器时，选择了原生 split pane UI 而非协议。）

「在终端里呈现网页」的全部现存形态＝① 客户端把网页渲染成**像素**（awrit、mpv 类比）；② 渲染成**字符/颜色块**（browsh、w3m）；③ 终端应用**原生 UI** 内嵌（cmux）。dlook 属于①+② 的组合，不存在第四条「协议级嵌入」的捷径。

---

## 五、子问题 4：降级与安全

### 5.1 远程 fetch 安全考量

- **dlook 现有先例**（rs/src/images.rs）：`ureq`（rustls）+ 整体 10s 超时 + 重定向≤5 + 远程 16MB 上限 + 空响应拒绝 + 解码后尺寸上限。L1/L2 的 HTML 抓取应沿用同款参数（HTML 16MB 足够；L2 的 URL 只传给 chromium，不做代理）。
- **追踪/phone-home（HTML 场景特有的新风险）**：渲染远程 HTML ≠ 只抓那一个 URL——`<img>`、CSS、字体、GA 脚本都会形成后续请求。aerc 把这当成必须默认阻断的威胁（`unshare --net` 级别隔离）。dlook 的 L1 天然安全：**html2text 不发任何子资源请求**（纯解析，无网络）；子资源（img src）是否拉取由 dlook 的图片注册表决定——建议默认「已显式打开的页面内图片可拉取（用户已表达意图），页面 CSS/字体等永不拉取」。L2 的 chromium 是另一回事：建议快照时加 `--disable-remote-fonts` 类最小化策略可后续实验，核心底线是**超时+尺寸上限+无交互**。
- **SSRF**：dlook 是本地单用户查看器，点击链接才抓取，风险形态≈本机浏览器；重点是 ①重定向可能指向内网地址（可接受，与浏览器同级）②若未来有 daemon/共享场景需重审（目前不适用）。资源耗尽靠 10s/16MB 硬顶。
- **解析面**：html5ever 是 Servo 出品、以健壮著称的解析器，被 scraper/ammonia/html2text 共用；L1 只解析不执行，无 JS 注入面。若未来嵌入用户 HTML 片段到自有文档（而非仅预览），再引入 ammonia。

### 5.2 SSH / 纯文本终端下的表现

- **L1 文本层**：终端协议无关，SSH 下与本地零差异；纯 16 色终端退化为无色/单色文本（html2text 的结构符号 `#`/`*`/`>` 本身承载语义）。
- **L2 快照层**：走 ratatui-image 现有 picker——SSH 直连时协议探测同样生效（本地终端支持 kitty/sixel 则可用；tmux 内图形协议会被吞，需探测降级）；无协议终端用 halfblocks 半块渲染，永远有产出，只是分辨率降为「字符画」级别。静态图不可点击：**链接导航必须仍由 L1 的文本行承载**（或对快照+文本做并列布局：文本是地图、图像是实景）。
- **L3**：SSH 场景下 `xdg-open` 在远端执行会在服务器侧打开（无意义）——应识别 SSH 环境提示用户在本地打开（OSC 8 超链接点击在多数现代终端里由**本地终端**打开 URL，这正是 SSH 场景的正确兜底：dlook 渲染的链接走 OSC 8，用户点击即在本机浏览器打开）。

---

## 六、关键未知（后续实验待办）

1. **html2text 的字符集处理**：`<meta charset>` 非 UTF-8（GBK/Shift-JIS）页面是否正确解码？未实测；必要时在 dlook 侧用 `encoding_rs` 预转换（成本待估）。
2. **布局表格噪音的治理**：HN 类页面的边框表格渲染是否可通过 html2text 配置（`config` API / 自定义 TextDecorator）或预处理（`display:none`、表格深度阈值）压噪？需要原型实验。
3. **`Image(src)` 注解 → dlook 图片注册表的实际接线**：src 相对路径解析、base URL 传播、与现有 `MAX_IMG_ROWS`/分页逻辑的交互，需原型验证。
4. **L2 的 window-size 策略**：宽度取内容区像素宽（cols×cell 宽）可保证清晰度；高度上限建议保守取 16384（Lighthouse 实测的上代 CDP 截断线；本机 152 已到 30000 但不排除老版本 chrome-headless-shell 差异），超长页需决定「clamp 还是分段截图」。内存/耗时在高分辨率整页下的实测未做（只测了 1280 宽）。
5. **chromium 变体探测**：`chromium`/`google-chrome`/`chrome-headless-shell`/`firefox`（Firefox 无等价 CLI 截图路径，只有 `--screenshot`？未查证——若要支持 Firefox 需另查）的优先序与版本下限（`--virtual-time-budget` 在新/旧 headless 的行为差异）。
6. **OSC 8 点击在 dlook 目标终端矩阵中的实际可用性**（kitty/wezterm/ghostty/iTerm2/SSH 场景），与现有 LinkSpan 点击的关系（终端原生点击 vs dlook 自绘点击区）。
7. **htmd 的 DOM 预清洗成本**（若最终仍想要 markdown 路径做 A/B 对比）：scraper 删 `script/style/nav` 后的输出质量未测。

---

## 附：证据源清单

- browsh：brow.sh（官方站）；github.com/browsh-org/browsh（README + API：v1.8.3 2024-01、pushed 2025-07-11、19,035 stars、单人维护声明）
- w3m/w3mimgdisplay： tats/w3m 源码 `w3mimg/x11/x11_w3mimg.c`（XCreateGC/XCopyArea 到 parent window）；ArchWiki「w3m」（inline_img_protocol 3/4）；links2(1) Ubuntu 手册页
- headless chromium：developer.chrome.com/docs/chromium/headless、/blog/chrome-headless-shell；chromium.googlesource headless/README.md；源码 headless_shell_switches.cc（80.0.3987.114 镜像，flag 语义注释）；Lighthouse issue #11121（MAX_SCREENSHOT_HEIGHT 16384）；本机实验 6 组（表格见 §2.3）
- awrit / cmux：github.com/chase/awrit（归档声明 + README）、manaflow-ai/cmux README
- monolith：github.com/Y2Z/monolith README
- crate 生态：crates.io API（html2text 0.17.1 / html5ever 0.40.0 / lol_html 3.0.1 / scraper 0.27.0 / ammonia 4.1.4 / htmd 0.5.5 / html2md 0.2.17，2026-09-13 查询）；docs.rs html2text（from_read_rich 签名）；本机实测输出样例见 §3.2/3.4
- aerc：aerc-mail.org（官方功能声明）；Arch extra aerc-0.22.0-1 包内文件（`usr/lib/aerc/filters/html`、`aerc.conf` filters/openers 段）——包证据
- 终端协议：sw.kovidgoyal.net/kitty/graphics-protocol/（规范全文）；iterm2.com/documentation-images.html；wezterm.org/imgcat.html；libsixel README；ghostty.org/docs/vt*
- dlook 侧现状：rs/Cargo.toml（ratatui-image 11 / ureq 2 / termimad 0.35 / ansi-to-tui 8）；rs/src/images.rs（10s/16MB/重定向 5/解码上限先例）；ratatui-image README（协议探测 + halfblocks 回退）
