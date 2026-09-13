# 渲染能力差距对照:vue-tui(Node 版)vs Rust 版(dlook)

> 记录 2026-09-03 的调研结论:vue-tui(@simon_he/vue-tui 1.1.3)在**渲染**上具备哪些能力、
> 本项目 Node 版实际用到了哪些、Rust 版(dlook)差了哪些、反超了哪些。
> 配套决策清单见 [DECISIONS.md](DECISIONS.md)。
>
> **实施状态(2026-09-03)**:G1–G7、G10 的补齐决策已全部实施并通过测试
> (E2E A–L 96 项、单元测试 15 项全过);G8(图片)、G9(主题配置化)按决策不做。
> 详见 [DECISIONS.md](DECISIONS.md) 决策记录。
>
> **更新(2026-09-13)**:G8 图片已实施(DECISIONS D15,翻案 D9)——kitty/sixel/iTerm2
> 图形协议 + halfblocks 回退 + 远程/data: 源 + 直开图片文件;G9 维持不做。

研究方法:读 vue-tui 的 `.d.ts` 类型声明与编译产物、Node 版 `src/`、Rust 版 `rs/src/`;
并用 pty 分别运行 `preview`(Node)与 `dlook`(Rust)渲染同一份 markdown,抓取原始 ANSI 序列实测对比。

---

## 1. 实测证据(同一份 md,80×24 pty 抓屏)

| 元素 | Node `preview`(vue-tui) | Rust `dlook` |
|---|---|---|
| H1/H2 | 粗体 + **青色** `38;2;95;253;255` | 仅粗体 `SGR 1`,无色,**H1 居中** |
| H3/H4 | 粗体 + **蓝色** `38;2;104;113;255` | 仅粗体,无色,左对齐 |
| 链接 | 只显示 label,蓝色 + 下划线,OSC 8 可点击 | 原样文本 `[link text](https://example.com)` |
| 引用 | dim,`│` 前缀,支持嵌套 | `▐` 前缀(gray 256 色) |
| 代码块(md 内) | 单色(黄亮) | syntect 真彩高亮 |

## 2. 能力对照总表

| 能力 | vue-tui 库 | Node 版实际 | Rust dlook | 判定 |
|---|---|---|---|---|
| 标题分级上色 | ✅ 默认主题即带色 | ✅ | ❌ 全部仅粗体 | **Rust 缺(G1)** |
| 标题对齐 | 左对齐 | ✅ | H1 居中(termimad 默认) | 差异(G2) |
| 链接渲染 | ✅ label 蓝色+下划线 + OSC 8 超链接 | ✅ | ❌ 原样文本 | **Rust 缺(G3)** |
| md 内代码块高亮 | ❌ 库本身单色 | ❌ | ✅ syntect | **Rust 反超** |
| mermaid | ⚠️ 需 optional peer,强制无色,仅简单 flowchart | ❌ 未接入 | ✅ mermansi 28 种图 + truecolor | **Rust 反超** |
| 代码语言覆盖 | — | ✅ shiki 40 语言 | ⚠️ syntect 默认集,缺十余种 | **Rust 缺(G4)** |
| 高亮主题 | — | github-dark 固定 | base16-ocean.dark 固定 | 平手 |
| markdown 主题覆盖 | ✅ `theme` prop(hex 真彩) | ❌ 未传 | ❌ 无配置 | Rust 缺(G9,产品层均未用) |
| 表格 | ✅ 对齐 + 圆角边框 `╭┬╮` | ✅ | ✅ 对齐 + 方角 `┌┬┐`(有圆角 preset) | 功能平手,样式差异(G6) |
| 任务列表 `- [x]` | ✅ checkbox | ✅ | ❌ 原样文本 | **Rust 缺(G5)** |
| 删除线 | ⚠️ 降级为 dim | ⚠️ | ✅ 真 CrossedOut | Rust 略优 |
| 数学公式 | ⚠️ optional katex → Unicode 近似(仅行内) | ❌ 未装 katex | ❌ | 平手(产品层均无,G7) |
| 图片 | ⚠️ kitty/iTerm2 图形协议 + resolver | ❌ 未用 | ✅ kitty/sixel/iTerm2 + halfblocks 回退(D15) | **Rust 反超(G8)** |
| 文本拖选+复制 | ✅ 拖选(反显/自动滚动)+ 松开即 OSC 52 复制 | ❌ 未启用(app 未传 `selection`,实测拖选零输出) | ❌ 无实现;原生选择需 Shift 绕过 | **两版都缺(G10)** |

---

## 3. 差距明细(Rust 缺失项)

### G1 标题分级上色(差距最大、改动最小)

- **Rust 现状**:[termio.rs](rs/src/termio.rs) `build_skin()` 只对 headers 设 Bold(去掉 termimad 默认的下划线),未设任何前景色。
- **vue-tui 行为**:默认主题 h1/h2 = bold + `cyanBright`,h3/h4 = bold + `blueBright`,h5/h6 = 纯 bold
  (编译产物 `block-source-Crro3mxi.js#L1401-L1439`;项目未传 `theme` → 默认生效,见上文实测 SGR)。
- **根因**:termimad 的 `MadSkin.headers` 每级是独立 `LineStyle`,支持 `set_fg`,但 dlook 没配。
- **修复点**:`build_skin()` 中对 `headers[0..=1]` 设青、`headers[2..=3]` 设蓝。约 15 行。

### G2 H1 对齐

- termimad 默认 `headers[0].align = Center`(termimad skin.rs `default()`);vue-tui 全部左对齐(实测)。
- 修复点:`skin.headers[0].align = Alignment::Left`,一行。

### G3 链接渲染

- **Rust 现状**:minimad **没有 `[label](url)` 行内链接语法**,整个中括号表达式按普通文本输出(实测)。
- **vue-tui 行为**:`sanitizeMarkdownLink` → link 主题样式(蓝 + 下划线)→ stdout 渲染器输出 **OSC 8**
  (`\x1B]8;;href\x07`,cli.js#L9877),可点击。
- **阻碍**:ratatui 0.30.2 **无 hyperlink 支持**(已查 registry 源码,`hyperlink` 零命中)。
- **修复点**:[markdown.rs](rs/src/markdown.rs) `render_prose_to_ansi` 管道自行解析行内链接 → 样式化 label。
  完整 OSC 8 需手工嵌序列或升级等 ratatui 支持(见 DECISIONS D3)。

### G4 代码语言覆盖

- **Node 版**:shiki 静态打包 40 种语言([highlight.ts](src/highlight.ts)、[lang.ts](src/lang.ts)),
  含 TS/TSX/Vue/Svelte/TOML/INI/GraphQL/Dockerfile/PowerShell/SCSS/Less/Swift/Kotlin/Dart。
- **Rust 版**:syntect `default-fancy` 默认语法集不含上述语言([lang.rs](rs/src/lang.rs) 注释已列明):
  `ts/tsx` 回退到 `js`,其余十余种扩展名 → 无色纯文本;md 代码块同样受限
  ([markdown.rs](rs/src/markdown.rs) `normalize_fence_lang`)。
- **修复方案**:引入 `two-face` crate(预打包完整 Sublime 语法集)或 syntect `extra_syntaxes`
  打包 `.sublime-syntax` 文件;代价是二进制增大(幅度待实测)。

### G5 任务列表 checkbox

- vue-tui 经 stream-markdown-parser 的 task-checkbox 插件渲染 `[x]`/`[ ]`(block-source L614-617);
  minimad 无此概念 → dlook 原样输出 `- [x] todo`。
- 修复点:prose 预处理把 `- [x] `/`- [ ] ` 替换为 `☑ `/`☐ `。

### G6 表格边框风格(纯样式)

- vue-tui 圆角 `╭┬╮ ├┼┤ ╰┴╯`;termimad 默认 `STANDARD`(方角),另有现成的
  `ROUNDED_TABLE_BORDER_CHARS` preset 可一键切换。两版均有 per-cell 对齐。

### G7 数学公式(库能力,两版产品层都没有)

- vue-tui 路径:optional peer `katex ^0.16.47`,首次遇 `$...$` 惰性动态 import;
  仅**行内**公式(≤160 字符,命令白名单 = 符号表 + `frac`/`sqrt`),katex `output:"mathml"`
  → 提取 Unicode 文本;`$$...$$` 块级公式无专门处理,原样直出;未装 katex → 原样黄亮文本。
- Node 版未安装 katex → 实际不可用。

### G8 图片(vue-tui 库能力,两版产品层原本都没有;**dlook 已反超,见 DECISIONS D15**)

- vue-tui 路径:`imageRenderer` resolver 把 src 解析成 base64 → 终端支持时走
  **kitty graphics / iTerm2 inline image 协议**;`data:` URL(png/jpeg/gif/webp)可内联;
  sixel 能探测但 markdown 路径无编码器;不支持时降级为 alt 文本(带 href 可点击)。
  Node 版没传 resolver → 只有 data: URL 理论可用。
- **dlook(2026-09-13,D15)**:ratatui-image 协议栈 kitty/sixel/iTerm2 + halfblocks 回退;
  markdown 独立段落图片 + 本地/http(s)/data: 源 + `dlook photo.png` 直开;
  滚动视口部分可见(sliced);行内图片降级链接(与 vue-tui 的 alt-text 降级一致)。

### G9 markdown 主题覆盖(库能力,产品层未用)

- vue-tui:`theme` prop 逐元素覆盖(支持 hex 真彩);dlook 无对应配置。低优先级。

### G10 文本拖选与复制(库能力完整,**产品层两版都没有**)

**vue-tui 库能力**(`createTerminalApp({ selection: true })` 启用,`dist/selection/terminal-selection.d.ts` + cli.js):

- 鼠标拖选:`linear`(行内连续)与 `block`(列选)两种模式;高亮样式默认 `{ inverse: true }` 反显。
- 拖到视口边缘自动滚动扩展选择(`autoScrollSelectionAt`);虚拟滚动组件经
  `SelectionTextProvider`(`canHandle`/`pointForCell`/`getText`/`getVisibleSpans`)把屏幕坐标映射回内容坐标,
  滚出视口的选区仍能取到文本。
- 复制:默认 `autoCopy: true` + `copyOnMouseUp: true` → **松开鼠标即复制**;走 `ClipboardApi` →
  **OSC 52**(cli.js 内联 `src/runtime/osc52.ts`,默认上限 100 KB,**SSH 远程可用**);
  读剪贴板回退 `wl-paste`/`xclip`/`xsel`。`Escape` 清除选择。
- 组件级 `selectable` 标记可选区域:`TVirtualMarkdown` 默认 true,`CodeView` 显式 false
  ([CodeView.vue:243](src/CodeView.vue#L243))→ 即使启用,code 模式也不可选。

**Node 版现状:未启用(实测确认)**。装配处 `selectionConfig = options.selection ?? false`
(cli.js `create-terminal-app`),[terminal.ts:37](src/terminal.ts#L37) 没传 `selection` → 关闭;
pty 发送 SGR 拖选序列(press/drag/release)后**零输出**(无反显、无 OSC 52)。

**共同行为**:两版都开启鼠标捕获(vue-tui `enableMouse` / dlook `EnableMouseCapture`)→
终端**原生**拖选被应用吞掉,只能 Shift+拖动绕过。

**Rust 实现路径**:crossterm 0.29 自带 `osc52` feature(`clipboard::CopyToClipboard`,支持
Clipboard/Primary,内部 base64);事件侧 `MouseEventKind::Down/Drag/Up(Left)` 可做状态机;
渲染侧 ratatui 对选中 span 加 `REVERSED` modifier。详见 DECISIONS D11。

---

## 4. Rust 反超项(勿误判为全差)

| 能力 | Rust(dlook) | vue-tui / Node 版 |
|---|---|---|
| md 内代码块 | ✅ fence 拦截 + syntect 真彩高亮 | 库本身单色(codeBlock 黄亮),Node 版同 |
| mermaid | ✅ mermansi:**28 种图**、Unicode + **truecolor**、宽度自适应 | TMermaidText 路径:需 optional peer `beautiful-mermaid`(项目未装);**强制 `colorMode:"none"` 且输出再剥全部 ANSI → 无色**;默认门禁 `isSimpleMermaidFlowchartSource` **只渲染"简单 flowchart"**;beautiful-mermaid 仅 6 种图(flowchart/state-v2/sequence/class/ER/xychart);Node 版 src 零 mermaid 引用 |
| 删除线 | ✅ `~~x~~` → CrossedOut 属性 | 降级为 dim |
| 体积/运行时 | 6 MB 静态二进制 | 110 MB Node SEA |

---

## 附:vue-tui 渲染能力速查(库层面,1.1.3)

**markdown 默认主题**(`TuiMarkdownTheme`,Style 支持 fg/bg hex、bold/dim/italic/underline/inverse/href):

| 元素 | 默认样式 |
|---|---|
| h1 / h2 | bold + cyanBright |
| h3 / h4 | bold + blueBright |
| h5 / h6 | bold |
| strong / emphasis | bold / italic |
| strikethrough | dim(Style 无删除线属性,降级) |
| inlineCode | yellowBright |
| link | blueBright + underline + href |
| blockquote | dim,前缀 `│ `(支持嵌套) |
| listMarker | cyanBright + bold |
| codeBlock | yellowBright(整块单色,无 token 高亮) |
| thematicBreak | dim,整行 `─` |
| html | dim(当纯文本,白名单 `customHtmlTags` 可放行) |

**解析器**(stream-markdown-parser 1.1.9,markdown-it 系,`html:true, linkify:true, typographer:true`
+ sub/sup/ins/task-checkbox/footnote/math/containers 插件):6 级标题、表格(per-cell 对齐 + 圆角边框)、
任务列表、嵌套列表/引用、hardbreak、脚注(仅原样直出)、sub/sup/ins(无样式)。

**其它组件**:TLinkifyText(CJK 感知 linkify)、TLink(点击/hover/visited)、
TAgentTerminalGraphic(kitty/iTerm2/sixel 图片,应用自备 PNG)、TVideo(experimental,ffmpeg/yt-dlp)、
T3DViewport(仅 Bun + bun-webgpu)。全局 TuiTheme 令牌与 markdown 主题是**两套独立系统**。

**选择与剪贴板**(`selection` 配置,默认关):linear/block 拖选、反显高亮、视口边缘自动滚动、
`SelectionTextProvider` 虚拟滚动坐标映射、`autoCopy`+`copyOnMouseUp`(默认 true,松开即复制)、
Escape 清除;复制走 OSC 52(≤100 KB,SSH 可用),读取回退 wl-paste/xclip/xsel。

**关键证据文件**:
`node_modules/@simon_he/vue-tui/dist/vue/markdown/theme.d.ts`、`dist/block-source-Crro3mxi.js`(默认主题/数学/图片)、
`dist/cli.js#L9877`(OSC 8)、`dist/vue/components/TMermaidText.d.ts`、`dist/mermaid-BNhM1jAZ.js`(beautiful-mermaid 桥)、
`dist/selection/terminal-selection.d.ts` + `dist/cli.js`(selectionConfig / dispatchWithSelection / osc52)。
