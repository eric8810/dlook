# dlook (Rust 版) 设计文档

> `look`/`preview`(现统一更名 **dlook**)的 Rust 重写版。目标:把单文件二进制从 **110MB(Node SEA)降到 ~4–6MB**,
> 同时保持现有 E2E 验收契约,并**新增两项 TS 版没有的能力**:
> ① mermaid 图表在终端渲染成 ASCII/Unicode 图;② markdown 围栏代码块语法上色。
> 技术栈:Rust + [ratatui](https://ratatui.rs)(diff 渲染)+ [crossterm](https://crates.io/crates/crossterm)(终端 I/O)
> + [syntect](https://crates.io/crates/syntect)(代码高亮)+ [termimad](https://crates.io/crates/termimad)/[minimad](https://crates.io/crates/minimad)(markdown)
> + [merman-core](https://crates.io/crates/merman-core)/[mermansi](https://crates.io/crates/mermansi)(mermaid→终端图)+ [ansi-to-tui](https://crates.io/crates/ansi-to-tui)(ANSI→ratatui)。

---

## 1. 背景与动机

现有 TS 版(见 [DESIGN.md](DESIGN.md))打包为 Node SEA,成品 110MB。体积构成(DESIGN §19.8):

| 组成 | 体积 |
|---|---|
| Node 运行时(`.text` 段) | ~105 MB |
| JS bundle(vue-tui + shiki + vue) | 3.5 MB |

**瓶颈是 Node 运行时本身,不是业务代码**。`strip` 已做到极限,DESIGN §19.8 结论:"进一步削减需更换运行时"。Rust 版直接去掉运行时这个大头,目标 4–6MB(约 20 倍削减)。

附带收益:
- 架构上去掉 vue-tui 黑盒,**两套 body 组件合一**(§2);
- **新增 mermaid 图渲染 + markdown 代码块上色** —— TS 版 markdown 代码块是单色的(DESIGN §11/§19.3),且不支持 mermaid;Rust 版用纯 Rust mermaid 栈 + code fence 拦截实现;
- 现有 pty+pyte E2E 套件 binary-agnostic,可直接复用(§11)。

---

## 2. 总体架构:三种内容收敛到一个 `Vec<Line>`

### 2.1 核心模型

Rust 版掌控全链路,**三种内容源都产出同一个类型**,body 渲染/滚动/按键完全共用一套:

```rust
use ratatui::text::Line;          // 一行 = 若干带样式 Span

struct Doc {
    lines: Vec<Line<'static>>,    // 整篇文档渲染后的带样式行(已按宽度换行/截断)
    top: usize,                   // 滚动位置
    mode: Mode,
    width: u16,                   // 当前排版宽度(= 终端列数)
}

enum Mode { Markdown, Code, Mermaid }
```

### 2.2 三种内容源的生成路径(关键)

```
┌─ markdown 文件(.md)─────────────────────────────────────────────┐
│  minimad 解析 → 遍历 Line:                                        │
│    Line::CodeFence (lang="mermaid") → mermansi 渲染成图(带ANSI) ─┐ │
│    Line::CodeFence (lang=其它)     → syntect 高亮   (带ANSI) ──┐│ │
│    Line::Normal/TableRow/...       → termimad 排版            │││
│                                                              │││
│  三者都 → ansi-to-tui → Vec<Line> ◄──────────────────────────┘││
└────────────────────────────────────────────────────────────────┘│
┌─ 代码文件(.ts/.rs/...)─── syntect 全文高亮(带ANSI) ──┐        │
│                                                        │        │
└────────────────────────────────────────────────────────┘        │
┌─ mermaid 文件(.mmd/.mermaid)── mermansi 渲染(带ANSI) ──┐      │
│                                                          │      │
└──────────────────────────────────────────────────────────┘      │
                                                                   │
                          都收敛到 ◄───────────────────────────────┘
                          Vec<ratatui::Line<'static>>
                                   │
                          Viewport widget: lines[top .. top+body_h]
```

**统一转换枢纽 = `ansi-to-tui`**:mermansi 和 syntect 都能输出带 ANSI SGR(truecolor `\x1b[38;2;r;g;bm`)的字符串,`ansi-to-tui` 把它解析成 ratatui `Text`(含 `Line`/`Span`/`Style`)。于是三类内容、markdown 内的图/代码块/正文,全部归一为 `Vec<Line>`。

### 2.3 屏幕布局(与 TS 版一致)

```
┌─ row 0 ─── header(文件名,bold)──────────────────┐
│                                                     │
│  Viewport widget  (h = rows - 2)   lines[top..]     │
│                                                     │
└─ row rows-1 ─ footer(按键提示,dim)──────────────┘
body_h = rows - 2
```

### 2.4 运行装配

```
parse_args → load_content(二进制检测 + 模式判定) → 非 TTY 直出
  → 按 mode 生成 Doc.lines:
       Markdown → markdown.rs(minimad 解析 + code fence 拦截分流 + termimad/mermansi/syntect)
       Code     → highlight.rs(syntect 全文高亮)
       Mermaid  → mermaid.rs(mermansi 渲染整图)
  → 进入 TUI 循环:enable_raw_mode + alt-screen + hide-cursor
  → 每帧 terminal.draw(header / Viewport / footer)
  → 读 crossterm 事件 → 映射键位 → 改 Doc.top / quit
  → resize → terminal.resize + 按 new_width 重排 Doc.lines
  → 退出:restore terminal + exit(code)
```

---

## 3. 依赖选择与理由

| 依赖 | 版本 | 用途 | 预估体积(strip 后) | 选择理由 |
|---|---|---|---|---|
| **ratatui** | 0.29 | 双缓冲 diff 渲染、`Line`/`Span`/`Style`/`Buffer`、widget 体系 | ~0.8–1.2 MB | diff 渲染免闪烁;`Line<'static>` 作统一行模型 |
| **crossterm** | 0.28 | raw mode、alt-screen、按键事件、resize 事件、终端尺寸 | ~0.1 MB | 跨平台;替我们解析按键 escape 序列(↑↓/PageUp/Home…) |
| **syntect** | 5.2 | 代码语法高亮(truecolor) | ~1.5–2.5 MB | Rust 版 shiki;Sublime/TextMate 语法 |
| **termimad** + **minimad** | 0.35 / 0.16 | markdown 排版 + 结构化解析 | ~0.3–0.4 MB | minimad 暴露 `Line::CodeFence` + `code_fence_lang()` 用于拦截;termimad 排版正文/表格 |
| **merman-core** + **mermansi** | 0.8.0-alpha.3 | mermaid 解析 + 终端 ASCII/Unicode 渲染 | ~0.5–1.5 MB(待实测) | **纯 Rust**,28 种图,`max_width` 适配终端,ANSI 彩色,无浏览器 |
| **ansi-to-tui** | 最新 | ANSI SGR → ratatui `Text` | ~极小 | 统一转换枢纽:mermansi/syntect 的 ANSI 输出 → `Vec<Line>` |
| args | — | argv 解析 | 0 | **手写**(与 TS 版一致,省 clap ~0.2MB) |
| Viewport | — | 自定义 ratatui widget | ~0 | 自写(§5.6) |

### 3.1 syntect:纯 Rust 引擎

syntect 默认用 `onig`(oniguruma,C 引擎)。改为纯 Rust 引擎:

```toml
[dependencies]
syntect = { version = "5.2", default-features = false, features = [
    "default-fancy",      # 纯 Rust 引擎(fancy-regex),替代默认 oniguruma
    "default-syntaxes",   # 内置默认语法集(~130 语言)
    "default-themes",
    "parsing",
    "metadata",           # find_syntax_by_token 需要
]}
```

好处:无 C 依赖,`cargo build --release` 直出单文件;体积更小;交叉编译/musl 友好。
> 注:具体 feature 名以 syntect 5.2 实际为准,实现时核对。

### 3.2 mermaid:mermansi(纯 Rust,无浏览器)

```toml
mermansi = "0.8.0-alpha.3"     # 终端渲染器(依赖 merman-core 解析器)
```

API 极简:
```rust
use mermansi::{render_source, MermansiOptions};
let opts = MermansiOptions::unicode()
    .with_color(ColorMode::Ansi)        // ANSI 彩色
    .with_max_width(width as usize);    // 图按终端宽度排版 —— pager 关键
let ascii_art: String = render_source(mermaid_text, &opts)?;  // 带 ANSI 的字符串
```

- 支持 28 种图(flowchart/sequence/state/class/er/gantt/pie/timeline/journey/gitgraph/mindmap/quadrant/xychart/sankey 等)
- `max_width`/`max_height`:图自适应终端宽度,不溢出
- `ColorMode::Ansi`:输出带 truecolor SGR,经 ansi-to-tui 还原为彩色 `Line`
- 确定性输出保证
- ⚠️ **alpha 版**(0.8.0-alpha.3),项目活跃。风险见 §10。

### 3.3 统一转换:ansi-to-tui

```rust
use ansi_to_tui::IntoText as _;
let text: ratatui::text::Text = ansi_string.as_bytes().into_text()?;
let lines: Vec<Line> = text.lines;     // 带 truecolor Style 的 ratatui Line
```

支持 named/indexed/truecolor(24-bit RGB)三种颜色 + bold/italic/underline。
malformed 序列被忽略,可直接喂真实终端输出。

> **待核对**:ansi-to-tui 与 ratatui 0.29 的类型兼容(文档引用 `ratatui_core`,实现时确认版本对齐;必要时锁定配套版本)。

---

## 4. 项目结构

```
doc-preview/                # 现有 TS 版保持不动,Rust 版在同目录新增 rs/
  rs/
    Cargo.toml
    src/
      main.rs               # 入口:argv + 装配 + TUI 循环 + 退出码
      args.rs               # argv 解析 + --help/--version(手写)
      content.rs            # 读文件 + 二进制检测 + 模式判定(Markdown/Code/Mermaid)
      lang.rs               # 扩展名 → 语言 ID / 模式(.mmd→Mermaid)
      highlight.rs          # syntect 初始化 + code → ANSI → Vec<Line>
      mermaid.rs            # mermansi 渲染 mermaid → ANSI → Vec<Line>
      markdown.rs           # minimad 解析 + code fence 拦截分流 + termimad 排版
      ansi_lines.rs         # ansi-to-tui 封装:ANSI String → Vec<Line>(统一枢纽)
      doc.rs                # Doc 模型 + 滚动数学(clamp/max_top)
      viewport.rs           # 自定义 ratatui widget:渲染 lines[top..]
      termio.rs             # crossterm 装配:raw/alt-screen/cleanup + 事件读取
      style_map.rs          # termimad CompoundStyle → ratatui Style(正文片段)
```

模块对照(TS → Rust):

| TS 模块 | Rust 模块 | 说明 |
|---|---|---|
| [args.ts](../src/args.ts) | `args.rs` | 手写,1:1 搬 |
| [content.ts](../src/content.ts) | `content.rs` | `std::fs` + 8KB NUL 检测 + **三模式** |
| [lang.ts](../src/lang.ts) | `lang.rs` | `match` 表 + `.mmd/.mermaid` |
| [highlight.ts](../src/highlight.ts) | `highlight.rs` | shiki → syntect(经 ansi-to-tui) |
| `TVirtualMarkdown` | `markdown.rs` | **minimad 解析 + code fence 拦截** + termimad 排版 |
| (TS 无) | `mermaid.rs` | **新增**:mermaid → 终端图 |
| (TS 无) | `ansi_lines.rs` | **新增**:统一 ANSI→Line 枢纽 |
| [CodeView.vue](../src/CodeView.vue) + [PreviewApp.vue](../src/PreviewApp.vue) body | `viewport.rs` + `doc.rs` | **两套合一**:统一虚拟视口 |
| [terminal.ts](../src/terminal.ts) | `main.rs` + `termio.rs` | 装配 + TUI 循环 |
| vue-tui 运行时 | ratatui + crossterm | diff 渲染 + 终端 I/O |

---

## 5. 关键模块设计

### 5.1 `content.rs` —— 四模式判定

> 2026-09-13(D15)追加 `Mode::Image`:图片扩展名(png/jpg/jpeg/gif/webp/bmp/ico/tiff)
> 跳过二进制检测与文本读取,字节由 [images.rs](rs/src/images.rs) 的加载线程按需读取;
> 实现细节见 [DECISIONS.md](DECISIONS.md) D15。

```rust
pub enum Mode { Markdown, Code, Mermaid, Image }

pub struct Loaded {
    pub file_name: String,
    pub content: String,
    pub mode: Mode,
    pub lang: Option<&'static str>,   // syntect 语言 ID(仅 Code 模式)
}

const BINARY_SAMPLE: usize = 8192;

pub fn load_content(path: &str) -> Result<Loaded, LoadError> {
    let meta = fs::metadata(path).map_err(|_| LoadError::NotFound(path.into()))?;
    if meta.is_dir() { return Err(LoadError::IsDirectory(path.into())); }
    let bytes = fs::read(path).map_err(|_| LoadError::Unreadable(path.into()))?;
    let sample = &bytes[..bytes.len().min(BINARY_SAMPLE)];
    if sample.contains(&0) { return Err(LoadError::Binary(path.into())); }
    let content = String::from_utf8_lossy(&bytes).into_owned();
    let (mode, lang) = detect_mode_lang(path);   // 见下
    Ok(Loaded { file_name: path.into(), content, mode, lang })
}
```

模式判定(`lang.rs`):
- `.md`/`.markdown` → `Mode::Markdown`
- `.mmd`/`.mermaid` → `Mode::Mermaid`
- 图片扩展名 → `Mode::Image`(D15)
- 其它 → `Mode::Code` + `detect_lang(path)`(扩展名→syntect 语言 ID,1:1 搬 [lang.ts](../src/lang.ts))

### 5.2 `highlight.rs` —— syntect code → ANSI → `Vec<Line>`

```rust
use syntect::easy::HighlightLines;

pub struct Highlighter { ss: SyntaxSet, theme: Theme }

impl Highlighter {
    /// 全文高亮(独立代码文件用)。返回带 ANSI truecolor 的字符串。
    pub fn highlight_to_ansi(&self, code: &str, lang: Option<&str>) -> String {
        let syntax = lang.and_then(|l| self.ss.find_syntax_by_token(l));
        let mut out = String::new();
        match syntax {
            Some(s) => {
                let mut h = HighlightLines::new(s, &self.theme);
                for line in code.lines() {
                    let regions = h.highlight_line(line, &self.ss).unwrap_or_default();
                    // syntect 自带:拼接成 \x1b[38;2;r;g;bm...\x1b[0m 的 ANSI 串
                    out.push_str(&syntect::util::as_24_bit_terminal_escaped(&regions, false));
                    out.push('\n');
                }
            }
            None => out.push_str(code),   // 未知语言:无色纯文本
        }
        out
    }
}
```

> tab 展开为 2 空格(与 TS 版 `expandTabs` 一致),在拼 ANSI 前对每行源码预处理。
> 长行截断(`less -S`)在 `ansi_lines` 转 `Line` 后按 cell 宽度截(§5.5)。

### 5.3 `mermaid.rs` —— mermaid → ASCII/Unicode 图 → `Vec<Line>`

```rust
use mermansi::{render_source, MermansiOptions, ColorMode};

pub fn render_mermaid_to_ansi(src: &str, width: u16) -> Result<String, MermaidError> {
    let opts = MermansiOptions::unicode()
        .with_color(ColorMode::Ansi)           // 彩色输出(经 ansi-to-tui 还原)
        .with_max_width(width as usize);       // 按终端宽度排版,不溢出
    render_source(src, &opts)
        .map_err(|e| MermaidError::Render(e.to_string()))
}
```

- 独立 `.mmd` 文件:整文件内容交给 `render_mermaid_to_ansi`。
- markdown 内 ```mermaid 块:收集围栏内源码,同上。
- 解析失败(`MermaidError`):降级为把源码当普通代码块单色显示 + 一行错误提示(不崩)。

### 5.4 `markdown.rs` —— minimad 解析 + code fence 拦截(核心新增)

这是与 TS 版最大区别:**markdown 里的围栏代码块不再单色**,而是按语言分流渲染。

```rust
use minimad::{Text, Line};

pub fn markdown_to_lines(md: &str, width: u16, skin: &MadSkin,
                         hl: &Highlighter) -> Vec<Line<'static>> {
    let text = Text::from(md);          // minimad 解析成 Vec<Line>
    let mut out: Vec<Line> = vec![];
    let mut fence: Option<FenceAcc> = None;   // 正在累积的 code block

    for line in text.lines {
        match (&fence, &line) {
            // 命中围栏代码块:累积源码(不立即输出)
            (None, Line::CodeFence(_)) => {
                let lang = line.code_fence_lang().map(str::to_string);
                fence = Some(FenceAcc::new(lang));
            }
            // 围栏结束:按 lang 分流渲染
            (Some(acc), Line::CodeFence(_)) if acc.is_closing(&line) => {
                out.extend(render_code_fence(acc, width, hl));   // 见下
                fence = None;
            }
            // 围栏内正文:累积
            (Some(acc), _) => acc.push_source_line(&line),
            // 普通行:termimad 排版成 ratatui Line
            (None, other) => out.extend(termimad_line_to_ratatui(other, skin, width)),
        }
    }
    out
}

fn render_code_fence(acc: FenceAcc, width: u16, hl: &Highlighter) -> Vec<Line<'static>> {
    let src = acc.source.join("\n");
    match acc.lang.as_deref() {
        Some("mermaid") => {
            // mermaid → mermansi(带 ANSI)→ ansi-to-tui → Vec<Line>
            match mermaid::render_mermaid_to_ansi(&src, width) {
                Ok(ansi) => ansi_lines::to_lines(&ansi),
                Err(_)   => ansi_lines::to_lines(&src),  // 降级单色
            }
        }
        Some(lang) => {
            // 代码块 → syntect 高亮(带 ANSI)→ ansi-to-tui → Vec<Line>
            let ansi = hl.highlight_to_ansi(&src, Some(lang));
            ansi_lines::to_lines(&ansi)
        }
        None => ansi_lines::to_lines(&src),   // 无 lang:单色
    }
}
```

- `termimad_line_to_ratatui`:用 termimad 把单行(`Normal`/`TableRow`/`TableRule`/`HorizontalRule`)排版,`FmtLine` 的 `CompoundStyle`(crossterm ContentStyle)经 `style_map` 转 ratatui `Style`,产出 `Line`(复用 termimad 表格列宽 + 段落换行)。
- **skin 配置**:标题段落设 bold,使输出含 `\x1b[1m`,通过 E2E H1 断言(§11)。
- **待核对**:minimad `Line::CodeFence` 的行级表示 —— fence 开始/结束行的判定、内容行是否也是 `CodeFence` 变体。实现时用 `code_fence_lang()` 区分开始行(lang=Some)与内容/结束行,实测边界。

### 5.5 `ansi_lines.rs` —— 统一转换枢纽

```rust
use ansi_to_tui::IntoText as _;

pub fn to_lines(ansi: &str) -> Vec<Line<'static>> {
    let text = ansi.as_bytes().into_text().unwrap_or_default();  // ANSI→ratatui Text
    text.lines.into_iter().map(|l| truncate_line(l, WIDTH)).collect()
}
```

- `truncate_line`:按 cell 宽度截断到视口宽度(`less -S` 风格,与 TS 版一致)。
- mermaid 图本身已由 mermansi 的 `max_width` 适配宽度,通常无需再截;代码行才需截断。
- 三种来源(mermaid/syntect/降级纯文本)都经此函数 → `Vec<Line>`。

### 5.6 `viewport.rs` —— 统一虚拟视口 widget

```rust
use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget, text::Line};

pub struct Viewport<'a> { pub lines: &'a [Line<'a>], pub top: usize }

impl Widget for Viewport<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        for y in 0..area.height {
            let li = self.top + y as usize;
            let row = area.y + y;
            match self.lines.get(li) {
                Some(line) => buf.set_line(area.x, row, line, area.width),
                None       => buf.set_string(area.x, row, &spaces(area.width), Style::default()),
            };
        }
    }
}
```

ratatui 自动 diff,只 flush 变化 cell → 无闪烁。md/code/mermaid 共用。

### 5.7 `doc.rs` —— 滚动数学(1:1 复刻 E2E B 用例)

```rust
impl Doc {
    pub fn max_top(&self, body_h: usize) -> usize {
        self.lines.len().saturating_sub(body_h)
    }
    pub fn set_top(&mut self, t: usize, body_h: usize) {
        self.top = t.min(self.max_top(body_h)).max(0);   // clamp,等价 TS clamp()
    }
    pub fn scroll(&mut self, delta: isize, body_h: usize) {
        self.set_top((self.top as isize + delta).max(0) as usize, body_h);
    }
}
```

### 5.8 `termio.rs` —— crossterm 装配 + 事件循环

```rust
pub fn run(mut doc: Doc, file_name: &str) -> io::Result<()> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, Hide)?;
    let mut term = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    let body_h = || -> usize { (term.size()?.height as usize).saturating_sub(2) };
    loop {
        term.draw(|f| render_frame(f, &doc, file_name))?;     // header / Viewport / footer
        if let Event::Key(k) = event::read()? {
            if k.kind != KeyEventKind::Press { continue; }    // unix 只处理 Press
            match map_key(k) {
                Action::Quit(code) => { cleanup(&mut term)?; return exit_code(code); }
                Action::Scroll(d)  => doc.scroll(d, body_h()),
                Action::Page(d)    => doc.scroll(d * body_h() as isize, body_h()),
                Action::Top        => doc.set_top(0, body_h()),
                Action::Bottom     => doc.set_top(usize::MAX, body_h()),
            }
        } else if let Event::Resize(w, h) = event::read()? {
            term.resize(Rect::new(0,0,w,h))?;
            doc.relayout(w);           // 按新宽度重排(md 换行 / code 截断 / mermaid 重渲)
        }
    }
}
```

> crossterm unix 默认只发 `KeyEventKind::Press`,需过滤(否则 Release/Repeat 双触发)。
> Ctrl+C 在 raw mode 下不触发 SIGINT,而是 `KeyEvent{code:Char('c'), modifiers:CONTROL}` → `Action::Quit(130)`。
> **mermaid 重排成本**:resize 时 mermaid 图需用新 `max_width` 重渲。大图重渲可能耗时,可加节流(防抖 resize 事件)。

---

## 6. 交互与滚动契约(1:1 复刻 [run_acceptance.py](../test/e2e/run_acceptance.py) B 用例)

`body_h = rows - 2`。80×24 → body_h=22(与 E2E 的 `+22` 一致)。

| 键 | Action | 效果 | E2E 对应 |
|---|---|---|---|
| `q` / `Esc` | Quit(0) | 退出码 0 | C1/C2 |
| `Ctrl+C` | Quit(130) | 退出码 130 | C3 |
| `j` / `↓` | Scroll(+1) | top+1 | B2/B7 |
| `k` / `↑` | Scroll(-1) | top-1 | B3/B8 |
| `Space` / `PageDown` | Page(+body_h) | top+22 | B4/B5 |
| `PageUp` | Page(-body_h) | top-22 | B6 |
| `Home` / `g` | Top | top=0 | B9/B11 |
| `End` / `G` | Bottom | top=max_top | B10/B12 |

所有操作后 `clamp(top, 0, max_top)`,与 TS `clamp()` 等价。

---

## 7. 体积优化

### 7.1 Cargo profile

```toml
[profile.release]
opt-level = "z"       # 体积优先
lto = "fat"
codegen-units = 1
strip = true
panic = "abort"
```

### 7.2 体积预估(strip 后)

| 配置 | 预估 |
|---|---|
| ratatui + crossterm + syntect(默认语法集) + termimad + **mermansi/merman-core** + ansi-to-tui | ~4–6 MB |
| syntect 自建子集(只含 40 种语言) + 同上 | ~3–5 MB |

> merman-core 是完整 mermaid parser(28 种图),其实际体积待实测(估 0.5–1.5MB)。
> 若要更小:syntect 自建语法子集(§7.3);mermansi 无法裁剪图类型(整包)。

### 7.3 对照

| | TS(Node SEA) | Rust |
|---|---|---|
| 成品体积 | 110 MB | ~4–6 MB |
| 运行时 | Node ~105MB | 无(静态链接) |
| 业务代码 | 3.5 MB(JS bundle) | 含进二进制 |
| 构建步骤 | vite build + SEA + postject + strip | `cargo build --release` |
| C 依赖 | 无 | 无(fancy-regex 纯 Rust) |
| **mermaid** | ❌ | ✅ 28 种图 |
| **md 代码块上色** | ❌(单色) | ✅ syntect |

---

## 8. 退出码与错误处理(1:1 对齐 [args.ts](../src/args.ts) / [content.ts](../src/content.ts))

| 场景 | 退出码 | 输出 |
|---|---|---|
| `q` / `Esc` | 0 | restore terminal |
| `Ctrl+C` | 130 | restore terminal |
| 无参数 | 2 | help → stderr |
| 参数 >1 | 2 | error + help → stderr |
| `--help`/`-h` | 0 | help → stdout |
| `--version`/`-V` | 0 | `dlook 0.1.0` → stdout |
| 文件不存在 / 不可读 / 二进制 / 目录 | 1 | `error: ...` → stderr |
| 非 TTY(管道) | 0 | 原始内容 → stdout(无 TUI/无 truecolor) |

非 TTY 检测:`std::io::IsTerminal`(`stdout().is_terminal()`)。直接 write 原始 content,exit 0 —— 满足 F1/F2(管道无 truecolor)。
**mermaid 解析失败**不改变退出码:降级为单色源码显示 + 行内错误提示(用户仍能看内容)。

---

## 9. 测试策略

### 9.1 复用现有 E2E(binary-agnostic)

[test/e2e/run_acceptance.py](../test/e2e/run_acceptance.py) 只跑 `BIN` 命令,Rust 版无需改测试:

```bash
cargo build --release
BIN=./rs/target/release/dlook python3 test/e2e/run_acceptance.py
```

63 项断言(A–H)直接套用。重点关注:

| 场景 | 复刻要点 | 风险 |
|---|---|---|
| **B 滚动** | `large.txt` 纯文本 code 模式,1 行=1 视觉行,滚动数学精确 | 低(§5.7 已对齐) |
| **C 退出** | q/Esc→0、Ctrl+C→130、alt-screen 恢复 | 低(crossterm 原生) |
| **D resize** | 放大/缩小后 header/footer 定位正确 | 低(ratatui 自动重排) |
| **E 错误码** | 无参→2、不存在/二进制/目录→1 | 低 |
| **F 非 TTY** | 管道直出原始内容、无 truecolor | 低 |
| **G 大文件** | 2000 行可滚、启动 <3s | 低(Rust 启动远快于 Node) |
| **H markdown** | 标题 bold、列表/表格/代码块/链接可见 | **中**(见下) |

**H 用例风险**:termimad 排版 ≠ TVirtualMarkdown。H 多为 "contains" + 标题加粗 SGR。
应对:配 `MadSkin` 让标题 bold(输出含 `\x1b[1m`),表格/列表/链接 termimad 均支持。
**新增注意**:H4 原断言"代码块单色无 truecolor"—— Rust 版代码块**已上色**,该断言需调整(见 §9.2)。

### 9.2 新增验收用例(mermaid + codeblock 上色)

在 `test/fixtures/` 增补夹具 + E2E 场景(扩展 [run_acceptance.py](../test/e2e/run_acceptance.py)):

| # | 用例 | 夹具 | 断言 |
|---|---|---|---|
| I1 | markdown 内 mermaid 块渲染成图 | `mermaid.md`(含 ```mermaid flowchart) | 屏幕含盒绘字符(`─`/`│`/`┌` 等),非源码 `flowchart TD` |
| I2 | 独立 .mmd 文件渲染成图 | `sample.mmd` | 同 I1 |
| I3 | markdown 内代码块上色 | `codeblock.md`(含 ```ts 块) | raw 含 truecolor `\x1b[38;2;` |
| I4 | mermaid 解析失败降级 | `bad.mmd`(语法错误) | 不崩;显示源码;exit 0 |

> **H4 断言调整**:原 `H4 fenced code block 单色无 truecolor` 与 I3 冲突。
> 方案:把 H4 的"无 truecolor"改为"代码块可见"(contains 代码文本);truecolor 断言移到 I3。
> 这是 Rust 版能力增强带来的预期调整,需同步改 [run_acceptance.py](../test/e2e/run_acceptance.py) 的 H4。

### 9.3 单元测试(L0)

- `doc.rs`:滚动 clamp/max_top(纯函数)
- `lang.rs`:扩展名→模式/语言映射(含 `.mmd`→Mermaid)
- `content.rs`:二进制检测(用 [binary.bin](../test/fixtures/binary.bin))
- `mermaid.rs`:`render_mermaid_to_ansi` 不 panic、非空、含盒绘字符
- `markdown.rs`:`code_fence_lang` 分流正确(mermaid→盒绘,ts→truecolor,无 lang→单色)

---

## 10. 风险与取舍

| 风险/取舍 | 说明 | 应对 |
|---|---|---|
| **mermansi alpha 版** | 0.8.0-alpha.3,API 可能变;某些图类型渲染质量待验 | 先验证常用图(flowchart/sequence/state);降级路径:mermaid 解析失败→单色源码;锁定版本 |
| **minimad code fence 行级表示** | `CodeFence` 的开始/结束/内容行边界需实测 | §5.4 用 `code_fence_lang()` 区分;实现时写单元测试覆盖边界 |
| md 排版保真度 | termimad ≠ TVirtualMarkdown,表格/间距细节不同 | H 用例多为 contains+bold;配 skin,必要时微调 |
| 高亮配色 | shiki github-dark vs syntect 主题,token 颜色不一致 | E2E 只查 truecolor **存在性**,不查色值 → 不影响验收 |
| mermaid 图宽度/高度 | 大图超出视口 | `max_width` 适配;纵向超长靠 pager 滚动(天然支持);resize 重渲加节流 |
| ansi-to-tui 兼容 | 文档引用 `ratatui_core`,与 ratatui 0.29 对齐待确认 | 实现时锁定配套版本;必要时手动 ANSI→Style(有限 SGR 子集) |
| syntect 默认集偏大 | 含 ~130 语言 | 先用默认;要更小再自建子集 |
| 启动速度 | syntect + merman-core 首次加载 | 仍远快于 Node+V8;G2 <3s 轻松 |
| 宽字符 | tab/East Asian Width | tab 展开 2 空格(同 TS);宽字符初版近似,后续接 `unicode-width` |

---

## 11. 实现路线(建议顺序)

1. **脚手架**:`rs/Cargo.toml` + 依赖 + `main.rs` 空循环(进 alt-screen 再退出)
2. **`args.rs` + `content.rs` + `lang.rs`**:argv + 文件读取 + 二进制检测 + 三模式判定 + 非 TTY 直出 + 退出码(跑通 E/F 用例)
3. **`highlight.rs` + `ansi_lines.rs` + `viewport.rs` + `doc.rs`**:code 模式高亮 + 统一转换 + 视口 + 滚动(跑通 A2/A3/A4/B/G)
4. **`markdown.rs`(无 code fence 拦截) + skin**:md 正文/表格/列表渲染(跑通 A1/H 正文部分)
5. **`markdown.rs` code fence 拦截**:```ts 块→syntect 上色(跑通 I3 + 调整 H4)
6. **`mermaid.rs` + `.mmd` 模式**:mermaid 渲染成图(跑通 I1/I2/I4)
7. **`termio.rs` 打磨**:resize(含 mermaid 重渲节流)、Ctrl+C=130、alt-screen 恢复(跑通 C/D)
8. **体积优化**:profile + (可选)syntect 自建子集(§7)
9. **全量 E2E**:`BIN=./rs/target/release/dlook python3 test/e2e/run_acceptance.py` → 全 PASS

每步对应一组 E2E 用例,渐进验收。

---

## 12. 与 TS 版差异对照

| 维度 | TS 版 | Rust 版 |
|---|---|---|
| 运行时 | Node 18+ | 无(静态二进制) |
| TUI 框架 | vue-tui(Vue 3 + stdout renderer) | ratatui(diff buffer) |
| 终端 I/O | vue-tui cli driver | crossterm |
| 高亮 | shiki(@shikijs/core + JS regex) | syntect(fancy-regex,Sublime 语法) |
| markdown | TVirtualMarkdown(黑盒) | minimad 解析 + **code fence 拦截** + termimad 排版 |
| **mermaid** | ❌ | ✅ mermansi(28 种图,纯 Rust) |
| **md 代码块** | ❌ 单色 | ✅ syntect 上色 |
| body 组件 | 两套(TVirtualMarkdown / CodeView) | **一套**(统一 Viewport widget) |
| 统一转换 | — | ansi-to-tui(ANSI→ratatui Line) |
| 滚动控制 | v-model:scrollTop 跨组件 | 单一 `Doc.top` |
| 打包 | vite build + SEA + postject + strip | `cargo build --release` |
| 成品体积 | 110 MB | ~4–6 MB |
| C 依赖 | 无 | 无 |

---

## 附:实现核对结果(已验证)

实现过程中已逐一核对以下点(对应 DESIGN-rust.md §10 风险):

- [x] syntect 5.3 feature `default-fancy` —— 纯 Rust fancy-regex 引擎,无 C 依赖 ✅
- [x] ansi-to-tui 8.0.1 与 ratatui 0.30 兼容(均用 `ratatui-core`)✅
- [x] **NO_COLOR 环境变量**:crossterm 0.29 的 `Colored::ansi_color_disabled()` 检测 `NO_COLOR` env,
      若设置则抑制所有颜色输出(产出空 `\x1b[;m`)。**实现中在 TUI 启动时强制 `set_ansi_color_disabled(false)`**
      (termio.rs),确保 truecolor 在任何环境下输出。
- [x] **syntect 默认语法集不含 TypeScript/Vue/Svelte/GraphQL/Docker/PowerShell/Dart/Swift/Kotlin/Toml/INI/SCSS/Less**。
      lang.rs 将 ts/tsx 回退到 `js` 扩展名;markdown.rs `normalize_fence_lang` 把代码块语言名归一化到
      syntect 可识别的 token。其余无语法的扩展名 → 无色纯文本。
- [x] mermansi `ColorMode::TrueColor` + `OutputMode::Concise` —— 输出 truecolor SGR ✅
- [x] merman-core 体积:成品 5.7MB(含全部依赖)✅
- [x] termimad `FmtText` Display 产出 ANSI(crossterm `StyledContent`),经 ansi-to-tui 转 ratatui Line ✅
- [x] ratatui `set_line` 正确应用 span 样式(line.style.patch(span.style))✅
- [x] crossterm `KeyEventKind::Press` 过滤在 pty 下正常(只处理 Press)✅
- [x] 主题用 `base16-ocean.dark`(syntect 默认主题集),token 输出 truecolor ✅

### 实际使用的 crate 版本

| crate | 版本 |
|---|---|
| ratatui | 0.30.2(经 ratatui-core) |
| crossterm | 0.29.0 |
| syntect | 5.3.0(`default-fancy`) |
| termimad | 0.35.1 |
| minimad | 0.16.0 |
| mermansi | 0.1.6(依赖 merman-core 0.8.0-alpha.3) |
| ansi-to-tui | 8.0.1 |

### 实际体积

| | TS(Node SEA) | Rust |
|---|---|---|
| 成品 | 109.9 MB | **5.7 MB** |
| 倍率 | — | **19x 更小** |

### E2E 验收结果

`BIN=./rs/target/release/dlook python3 test/e2e/run_acceptance.py`
→ **PASS=70 FAIL=0**(A–H 原 63 项 + I1–I4 新增 7 项)
