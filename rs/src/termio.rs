//! crossterm 装配 + TUI 事件循环。
//!
//! 职责：raw mode / alt-screen / 鼠标捕获 / 事件读取 / 键位+滚轮映射 / resize /
//! 文件变更热重载（stat 轮询）/ 文本拖选与复制（selection）/ cleanup。

use std::io::{self, IsTerminal, Stdout};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crossterm::clipboard::CopyToClipboard;
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use crossterm::style::{Attribute, Colored};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::execute;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Terminal;

use crate::ansi_lines;
use crate::content;
use crate::doc::Doc;
use crate::highlight::Highlighter;
use crate::lang::{self, Mode};
use crate::links;
use crate::markdown;
use crate::mermaid;
use crate::selection;
use crate::viewport::Viewport;

type Term = Terminal<CrosstermBackend<Stdout>>;

/// 左右边距（字符数）。
const H_MARGIN: u16 = 1;

const FOOTER: &str = "q quit  ↑↓/jk/scroll  space/pgdn  g/G top/bottom  Ctrl+C quit";
const FOOTER_BACK: &str = "  ⌫back";

/// 状态栏消息存活时间。
const STATUS_TTL: Duration = Duration::from_millis(1500);

/// 拖选边缘自动滚动的最小间隔。
const AUTO_SCROLL_INTERVAL: Duration = Duration::from_millis(120);

/// TUI 交互状态:选区 + 拖拽 + 状态栏消息。
struct UiState {
    sel: Option<selection::Selection>,
    /// 鼠标左键按下中(Down 后 Up 前)。
    dragging: bool,
    /// 按下后指针是否移动过(纯点击不算拖拽,不触发边缘自动滚动)。
    moved: bool,
    /// 最近一次指针位置(屏幕坐标 列,行)。
    pointer: Option<(u16, u16)>,
    /// 上次边缘自动滚动时刻(None = 从未,首次立即触发)。
    last_autoscroll: Option<Instant>,
    /// 状态栏消息 + 产生时刻。
    status: Option<(String, Instant)>,
}

impl UiState {
    fn new() -> Self {
        UiState {
            sel: None,
            dragging: false,
            moved: false,
            pointer: None,
            last_autoscroll: None,
            status: None,
        }
    }

    /// 未过期的状态栏消息。
    fn status_text(&self) -> Option<&str> {
        self.status.as_ref().and_then(|(msg, at)| {
            if at.elapsed() < STATUS_TTL {
                Some(msg.as_str())
            } else {
                None
            }
        })
    }
}

/// 判定 stdout 是否为 TTY。
pub fn is_tty() -> bool {
    io::stdout().is_terminal()
}

/// 构建 markdown skin(对齐 vue-tui 默认主题,见 GAP.md G1/G2/G6):
///   - 标题:粗体(去 termimad 默认下划线);h1/h2 青、h3/h4 蓝、h5/h6 无色
///   - H1 左对齐(termimad 默认居中,vue-tui 为左对齐)
///   - 表格圆角边框(vue-tui 用 ╭┬╮;termimad 默认方角 ┌┬┐)
fn build_skin() -> termimad::MadSkin {
    use crossterm::style::Color;

    let mut skin = termimad::MadSkin::default();
    for (i, h) in skin.headers.iter_mut().enumerate() {
        h.compound_style
            .object_style
            .attributes
            .unset(Attribute::Underlined);
        h.compound_style
            .object_style
            .attributes
            .set(Attribute::Bold);
        match i {
            0 | 1 => h.compound_style.set_fg(Color::Cyan), // cyanBright
            2 | 3 => h.compound_style.set_fg(Color::Blue), // blueBright
            _ => {}
        }
    }
    skin.headers[0].align = termimad::Alignment::Left;
    skin.table_border_chars = termimad::ROUNDED_TABLE_BORDER_CHARS;
    skin
}

/// 运行 TUI 循环，返回退出码。
///
/// `file_path` 用于文件变更监听（热重载）；`mode`/`syntax_token` 由扩展名决定，
/// 文件类型不会因内容修改而变化，故在加载时一次性确定。
pub fn run(
    file_path: &str,
    mode: Mode,
    syntax_token: Option<&'static str>,
    initial_content: &str,
) -> i32 {
    // 强制启用 ANSI 颜色输出（忽略 NO_COLOR 环境变量）。
    Colored::set_ansi_color_disabled(false);

    let mut stdout = io::stdout();
    let _ = execute!(stdout, EnterAlternateScreen, crossterm::event::EnableMouseCapture);
    let _ = enable_raw_mode();

    let mut terminal = match Terminal::new(CrosstermBackend::new(stdout)) {
        Ok(t) => t,
        Err(_) => return 1,
    };

    let highlighter = Highlighter::new();
    let skin = build_skin();

    let (w, _h) = current_size(&terminal);
    let content_w = content_width(w);
    let (lines, doc_links) =
        build_lines(initial_content, mode, syntax_token, content_w, &highlighter, &skin);
    let mut doc = Doc::new(lines, mode, content_w, doc_links);

    let exit_code = event_loop(
        &mut terminal,
        &mut doc,
        file_path,
        mode,
        syntax_token,
        &highlighter,
        &skin,
    );

    // Cleanup
    let _ = disable_raw_mode();
    let _ = execute!(
        terminal.backend_mut(),
        crossterm::event::DisableMouseCapture,
        LeaveAlternateScreen
    );

    exit_code
}

/// 文件的 stat 指纹(mtime + size),用于热重载轮询;不可读时返回 None。
fn file_stamp(path: &str) -> Option<(std::time::SystemTime, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

fn current_size(terminal: &Term) -> (u16, u16) {
    terminal
        .size()
        .map(|r| (r.width, r.height))
        .unwrap_or((80, 24))
}

/// 内容区宽度 = 终端宽度 - 左右边距。
fn content_width(term_w: u16) -> u16 {
    term_w.saturating_sub(H_MARGIN * 2)
}

/// 导航状态:当前文件 + 返回栈(D14)。
/// 点击本地链接 → push 当前 (path, top) 并跳转;⌫/Alt+← → pop 并恢复。
struct Nav {
    path: String,
    mode: Mode,
    syntax_token: Option<&'static str>,
    history: Vec<HistoryEntry>,
}

struct HistoryEntry {
    path: String,
    top: usize,
}

/// 鼠标事件的结果:点击命中链接时携带原始目标,交事件循环解析。
enum MouseAction {
    None,
    OpenLink(String),
}

#[allow(clippy::too_many_arguments)]
fn event_loop(
    terminal: &mut Term,
    doc: &mut Doc,
    file_path: &str,
    mode: Mode,
    syntax_token: Option<&'static str>,
    hl: &Highlighter,
    skin: &termimad::MadSkin,
) -> i32 {
    let mut nav = Nav {
        path: file_path.to_string(),
        mode,
        syntax_token,
        history: Vec::new(),
    };
    // 当前文件内容（用于 resize / 文件变更时重排）
    let mut content: String = content::reload_content(&nav.path).unwrap_or_default();
    let mut last_stamp = file_stamp(&nav.path);
    let mut ui = UiState::new();
    // 外部打开器(xdg-open 等)子进程句柄,非阻塞收割避免僵尸
    let mut children: Vec<Child> = Vec::new();

    loop {
        children.retain_mut(|c| c.try_wait().map(|r| r.is_none()).unwrap_or(false));

        let _ = terminal.draw(|f| {
            render_frame(
                f,
                doc,
                &nav.path,
                ui.sel,
                ui.status_text(),
                !nav.history.is_empty(),
            )
        });

        // 用 poll 非阻塞检查终端事件(200ms 超时兼作热重载轮询周期)
        if event::poll(Duration::from_millis(200)).unwrap_or(false) {
            match event::read() {
                Ok(Event::Key(k)) => {
                    if k.kind != KeyEventKind::Press {
                        continue;
                    }
                    // 选区相关键优先于滚动/退出映射(DECISIONS D11):
                    //   Esc:有选区 → 清除;无选区 → 退出
                    //   y / Enter:有非空选区 → 手动复制
                    if k.code == KeyCode::Esc && ui.sel.is_some() {
                        ui.sel = None;
                        continue;
                    }
                    if (k.code == KeyCode::Char('y') || k.code == KeyCode::Enter)
                        && k.modifiers.is_empty()
                        && ui.sel.is_some_and(|s| !s.is_empty())
                    {
                        if let Some(sel) = ui.sel {
                            copy_selection(terminal, doc, &sel, &mut ui.status);
                        }
                        ui.sel = None;
                        continue;
                    }
                    match map_key(k) {
                        Action::Quit(code) => return code,
                        Action::Back => {
                            go_back(terminal, doc, &mut nav, &mut content, &mut ui, hl, skin);
                        }
                        Action::Scroll(delta) => {
                            let body_h = body_height(terminal);
                            doc.scroll(delta, body_h);
                        }
                        Action::Page(delta) => {
                            let body_h = body_height(terminal);
                            let page = body_h as isize;
                            doc.scroll(delta * page, body_h);
                        }
                        Action::Top => doc.set_top(0, body_height(terminal)),
                        Action::Bottom => doc.set_top(usize::MAX, body_height(terminal)),
                        Action::None => {}
                    }
                }
                Ok(Event::Mouse(m)) => {
                    if let MouseAction::OpenLink(raw) =
                        handle_mouse(terminal, doc, &mut ui, m)
                    {
                        open_link(
                            terminal,
                            doc,
                            &mut nav,
                            &mut content,
                            &mut ui,
                            &mut children,
                            &raw,
                            hl,
                            skin,
                        );
                    }
                }
                Ok(Event::Resize(w, h)) => {
                    let body_h = (h as usize).saturating_sub(2);
                    let cw = content_width(w);
                    // 重排后行结构变化,内容坐标失效 → 清除选区
                    ui.sel = None;
                    let (new_lines, new_links) =
                        build_lines(&content, nav.mode, nav.syntax_token, cw, hl, skin);
                    doc.replace_lines(new_lines, new_links, cw, body_h);
                }
                Ok(_) => {}
                Err(_) => return 1,
            }
        }

        // 拖选中指针停在视口边缘 → 持续自动滚动并延伸选区(DECISIONS D11 ②)。
        // 纯点击(未移动)不算拖拽,不触发——否则点击视口首/末行会平移内容,
        // 把点击错位成跨行选区复制。
        if ui.dragging && ui.moved {
            edge_autoscroll(terminal, doc, &mut ui);
        }

        // 热重载:stat 轮询(mtime+size 变化即重排),跟随当前导航文件。
        // 替代 notify 依赖(省 ~150KB 体积);事件循环本就以 ~200ms 轮询,
        // 检测延迟同量级。
        let stamp = file_stamp(&nav.path);
        if stamp != last_stamp {
            last_stamp = stamp;
            // 文件被修改：重新读取并重排
            if let Some(new_content) = content::reload_content(&nav.path) {
                content = new_content;
                ui.sel = None; // 行结构变化,选区坐标失效
                let (w, _h) = current_size(terminal);
                let cw = content_width(w);
                let body_h = body_height(terminal);
                let (new_lines, new_links) =
                    build_lines(&content, nav.mode, nav.syntax_token, cw, hl, skin);
                doc.replace_lines(new_lines, new_links, cw, body_h);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 链接导航(D14)
// ---------------------------------------------------------------------------

/// 解析点击的链接目标并执行:本地文件 → 内部跳转;外部 URL → 系统打开器;
/// 锚点/无效目标 → 状态栏提示。
#[allow(clippy::too_many_arguments)]
fn open_link(
    terminal: &mut Term,
    doc: &mut Doc,
    nav: &mut Nav,
    content: &mut String,
    ui: &mut UiState,
    children: &mut Vec<Child>,
    raw: &str,
    hl: &Highlighter,
    skin: &termimad::MadSkin,
) {
    let base_dir = Path::new(&nav.path)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    match links::classify(&base_dir, raw) {
        links::Target::Local(path) => {
            navigate(terminal, doc, nav, content, ui, &path, hl, skin);
        }
        links::Target::External(url) => {
            if open_external(&url, children) {
                ui.status = Some((format!("opened externally: {url}"), Instant::now()));
            } else {
                ui.status = Some((
                    format!("no opener found (xdg-open/open) for: {url}"),
                    Instant::now(),
                ));
            }
        }
        links::Target::Anchor => {
            ui.status = Some(("anchor links unsupported".to_string(), Instant::now()));
        }
        links::Target::Invalid => {
            ui.status = Some((format!("invalid link: {raw}"), Instant::now()));
        }
    }
}

/// 跳转到本地文件:校验可读 → push 历史 → 重排渲染。失败仅状态栏提示。
#[allow(clippy::too_many_arguments)]
fn navigate(
    terminal: &mut Term,
    doc: &mut Doc,
    nav: &mut Nav,
    content: &mut String,
    ui: &mut UiState,
    target: &Path,
    hl: &Highlighter,
    skin: &termimad::MadSkin,
) {
    match content::read_for_navigate(target) {
        Err(e) => {
            let p = target.display();
            let msg = match e {
                content::OpenError::NotFound => format!("link target not found: {p}"),
                content::OpenError::IsDir => format!("link target is a directory: {p}"),
                content::OpenError::Binary => format!("link target is binary: {p}"),
                content::OpenError::Unreadable => format!("link target unreadable: {p}"),
            };
            ui.status = Some((msg, Instant::now()));
        }
        Ok(text) => {
            nav.history.push(HistoryEntry {
                path: nav.path.clone(),
                top: doc.top,
            });
            load_doc(terminal, doc, nav, content, ui, &target.to_string_lossy(), text, hl, skin);
        }
    }
}

/// 返回上一文件(恢复滚动位置);目标已不可读时仅提示,栈不弹。
#[allow(clippy::too_many_arguments)]
fn go_back(
    terminal: &mut Term,
    doc: &mut Doc,
    nav: &mut Nav,
    content: &mut String,
    ui: &mut UiState,
    hl: &Highlighter,
    skin: &termimad::MadSkin,
) {
    let Some(entry) = nav.history.last() else {
        return;
    };
    let (path, top) = (entry.path.clone(), entry.top);
    match content::read_for_navigate(Path::new(&path)) {
        Err(_) => {
            ui.status =
                Some((format!("cannot go back, unreadable: {path}"), Instant::now()));
        }
        Ok(text) => {
            nav.history.pop();
            load_doc(terminal, doc, nav, content, ui, &path, text, hl, skin);
            // 恢复跳转前的滚动位置
            doc.set_top(top, body_height(terminal));
        }
    }
}

/// 按新文件装载 Doc(路径/模式/内容/渲染全部切换)。
#[allow(clippy::too_many_arguments)]
fn load_doc(
    terminal: &mut Term,
    doc: &mut Doc,
    nav: &mut Nav,
    content: &mut String,
    ui: &mut UiState,
    path: &str,
    text: String,
    hl: &Highlighter,
    skin: &termimad::MadSkin,
) {
    nav.path = path.to_string();
    (nav.mode, nav.syntax_token) = lang::detect_mode_lang(path);
    *content = text;
    ui.sel = None; // 换文档,旧选区坐标失效
    let (w, _h) = current_size(terminal);
    let cw = content_width(w);
    let (lines, new_links) = build_lines(content, nav.mode, nav.syntax_token, cw, hl, skin);
    *doc = Doc::new(lines, nav.mode, cw, new_links);
}

/// 用系统打开器打开外部 URL(Linux xdg-open / macOS open / Windows start)。
/// spawn 不等待;句柄留在 children 由事件循环非阻塞收割。
fn open_external(url: &str, children: &mut Vec<Child>) -> bool {
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = Command::new("open");
        c.arg(url);
        c
    } else if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", "start", "", url]);
        c
    } else {
        let mut c = Command::new("xdg-open");
        c.arg(url);
        c
    };
    match cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => {
            children.push(child);
            true
        }
        Err(_) => false,
    }
}

fn body_height(terminal: &Term) -> usize {
    terminal
        .size()
        .map(|r| r.height as usize)
        .unwrap_or(24)
        .saturating_sub(2)
}

enum Action {
    Quit(i32),
    Scroll(isize),
    Page(isize),
    Top,
    Bottom,
    Back,
    None,
}

fn map_key(k: KeyEvent) -> Action {
    // Ctrl+C → 退出码 130
    if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
        return Action::Quit(130);
    }
    if k.code == KeyCode::Char('\u{0003}') {
        return Action::Quit(130);
    }

    match k.code {
        KeyCode::Char('q') => Action::Quit(0),
        KeyCode::Esc => Action::Quit(0),
        KeyCode::Char('j') => Action::Scroll(1),
        KeyCode::Down => Action::Scroll(1),
        KeyCode::Char('k') => Action::Scroll(-1),
        KeyCode::Up => Action::Scroll(-1),
        KeyCode::Char(' ') => Action::Page(1),
        KeyCode::PageDown => Action::Page(1),
        KeyCode::PageUp => Action::Page(-1),
        KeyCode::Home => Action::Top,
        KeyCode::Char('g') => Action::Top,
        KeyCode::End => Action::Bottom,
        KeyCode::Char('G') => Action::Bottom,
        // 返回上一文件(D14)
        KeyCode::Backspace => Action::Back,
        KeyCode::Left if k.modifiers.contains(KeyModifiers::ALT) => Action::Back,
        _ => Action::None,
    }
}

// ---------------------------------------------------------------------------
// 鼠标拖选(DECISIONS D11)
// ---------------------------------------------------------------------------

/// 鼠标事件分发:滚轮滚动 + 左键拖选状态机 + 点击链接命中(D14)。
fn handle_mouse(
    terminal: &mut Term,
    doc: &mut Doc,
    ui: &mut UiState,
    m: MouseEvent,
) -> MouseAction {
    let body_h = body_height(terminal);
    let (_, rows) = current_size(terminal);

    match m.kind {
        MouseEventKind::ScrollUp => doc.scroll(-1, body_h),
        MouseEventKind::ScrollDown => doc.scroll(1, body_h),

        // 左键按下:定锚(Shift+点击 → 扩展已有选区)
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some(p) = to_content_point(doc, m.column, m.row, rows) {
                if m.modifiers.contains(KeyModifiers::SHIFT) {
                    match &mut ui.sel {
                        Some(sel) => sel.focus = p,
                        None => ui.sel = Some(selection::Selection::new(p)),
                    }
                } else {
                    ui.sel = Some(selection::Selection::new(p));
                }
                ui.dragging = true;
                ui.moved = false;
                ui.last_autoscroll = None;
                ui.pointer = Some((m.column, m.row));
            }
        }

        // 拖动:移动焦点;指针停在视口边缘时自动滚动
        MouseEventKind::Drag(MouseButton::Left) if ui.dragging => {
            ui.moved = true;
            ui.pointer = Some((m.column, m.row));
            if let Some(p) = to_content_point(doc, m.column, m.row, rows) {
                if let Some(sel) = &mut ui.sel {
                    sel.focus = p;
                }
            }
            edge_autoscroll(terminal, doc, ui);
        }

        // 松开:非空选区 → 自动复制(autoCopy);空选(点击)→ 命中测试链接
        MouseEventKind::Up(MouseButton::Left) if ui.dragging => {
            ui.dragging = false;
            if let Some(sel) = ui.sel {
                if !sel.is_empty() {
                    copy_selection(terminal, doc, &sel, &mut ui.status);
                    return MouseAction::None;
                }
            }
            ui.sel = None;
            return match hit_test_link(doc, m.column, m.row, rows) {
                Some(target) => MouseAction::OpenLink(target),
                None => MouseAction::None,
            };
        }

        _ => {}
    }
    MouseAction::None
}

/// 点击命中测试:屏幕坐标 → 内容坐标 → 查 doc.links。
/// 落在 body 区且该行真实存在(不钳制到末行)才算命中。
fn hit_test_link(doc: &Doc, col: u16, row: u16, rows: u16) -> Option<String> {
    if row == 0 || row + 1 >= rows {
        return None; // header / footer / 越界
    }
    let line = doc.top + (row - 1) as usize;
    if line >= doc.lines.len() {
        return None; // 文档末尾下方的空行,不是链接
    }
    let col = col.saturating_sub(H_MARGIN) as usize;
    doc.links
        .iter()
        .find(|l| l.line == line && col >= l.start && col < l.end)
        .map(|l| l.target.clone())
}

/// 屏幕坐标(列,行)→ 内容坐标。行必须落在 body 区(跳过 header/footer),
/// 行索引钳制到文档末尾,列换算掉 H_MARGIN。
fn to_content_point(
    doc: &Doc,
    col: u16,
    row: u16,
    rows: u16,
) -> Option<selection::SelPoint> {
    // body:第 1 行 ..= rows-2(第 0 行 header,第 rows-1 行 footer)
    if row == 0 || row + 1 >= rows {
        return None;
    }
    let line = doc.top + row.saturating_sub(1) as usize;
    let line = line.min(doc.lines.len().saturating_sub(1));
    Some(selection::SelPoint {
        line,
        col: col.saturating_sub(H_MARGIN) as usize,
    })
}

/// 拖选中指针停在视口顶/底边缘 → 自动滚动一行并把焦点延伸到新的可见边界行。
/// 节流:AUTO_SCROLL_INTERVAL 内不重复滚动。
fn edge_autoscroll(terminal: &mut Term, doc: &mut Doc, ui: &mut UiState) {
    let Some((col, row)) = ui.pointer else { return };
    let (_, rows) = current_size(terminal);
    let body_h = body_height(terminal);
    // 视口顶边缘 = body 第一行(row 1);底边缘 = body 最后一行(rows-2)
    let at_top = row == 1;
    let at_bottom = row + 2 == rows;
    if !at_top && !at_bottom {
        return;
    }
    if ui
        .last_autoscroll
        .is_some_and(|t| t.elapsed() < AUTO_SCROLL_INTERVAL)
    {
        return;
    }
    ui.last_autoscroll = Some(Instant::now());

    let delta: isize = if at_top { -1 } else { 1 };
    let before_top = doc.top;
    doc.scroll(delta, body_h);
    if doc.top == before_top {
        return; // 已到文档顶/底,无法继续滚动
    }
    if let Some(sel) = &mut ui.sel {
        let line = if at_top {
            doc.top
        } else {
            doc.top + body_h.saturating_sub(1)
        };
        sel.focus = selection::SelPoint {
            line,
            col: col.saturating_sub(H_MARGIN) as usize,
        };
    }
}

/// 复制选区文本到系统剪贴板(OSC 52;写入失败静默降级为状态栏提示)。
fn copy_selection(
    terminal: &mut Term,
    doc: &Doc,
    sel: &selection::Selection,
    status: &mut Option<(String, Instant)>,
) {
    let text = selection::text(&doc.lines, sel);
    let chars = text.chars().count();
    let result = execute!(
        terminal.backend_mut(),
        CopyToClipboard::to_clipboard_from(text.as_str())
    );
    *status = Some(match result {
        Ok(()) => (format!("copied {chars} chars (OSC 52)"), Instant::now()),
        Err(_) => (
            "clipboard unsupported (OSC 52 write failed)".to_string(),
            Instant::now(),
        ),
    });
}

fn render_frame(
    f: &mut ratatui::Frame,
    doc: &Doc,
    file_name: &str,
    sel: Option<selection::Selection>,
    status: Option<&str>,
    can_back: bool,
) {
    let area = f.area();
    let chunks = Layout::vertical([
        Constraint::Length(1), // header
        Constraint::Min(0),    // body
        Constraint::Length(1), // footer
    ])
    .split(area);

    // Header (bold, 左边距 1 字符)
    let header = Paragraph::new(format!(" {file_name}"))
        .style(Style::default().add_modifier(Modifier::BOLD));
    f.render_widget(header, chunks[0]);

    // Body — 左右各留 H_MARGIN 字符边距
    let body_area = Rect::new(
        chunks[1].x + H_MARGIN,
        chunks[1].y,
        chunks[1].width.saturating_sub(H_MARGIN * 2),
        chunks[1].height,
    );
    let viewport = Viewport {
        lines: &doc.lines,
        top: doc.top,
        selection: sel,
    };
    f.render_widget(viewport, body_area);

    // Footer (dim, 左边距 1 字符);有状态消息时优先显示(复制结果等)。
    // 有导航历史时追加 ⌫back 提示(D14)。
    let mut footer_text = format!(" {}", status.unwrap_or(FOOTER));
    if can_back && status.is_none() {
        footer_text.push_str(FOOTER_BACK);
    }
    let footer = Paragraph::new(Line::from(vec![Span::styled(
        footer_text,
        Style::default().add_modifier(Modifier::DIM),
    )]))
    .alignment(Alignment::Left);
    f.render_widget(footer, chunks[2]);
}

/// 根据 mode + content 生成 Vec<Line> 与行内链接区域。
fn build_lines(
    content: &str,
    mode: Mode,
    syntax_token: Option<&str>,
    width: u16,
    hl: &Highlighter,
    skin: &termimad::MadSkin,
) -> (Vec<Line<'static>>, Vec<crate::links::LinkSpan>) {
    match mode {
        Mode::Markdown => markdown::markdown_to_lines(content, width, skin, hl),
        Mode::Code => {
            let ansi = hl.highlight_to_ansi(content, syntax_token);
            (ansi_lines::to_lines(&ansi, width), Vec::new())
        }
        Mode::Mermaid => match mermaid::render_mermaid_to_ansi(content, width) {
            Ok(ansi) => (ansi_lines::to_lines_untruncated(&ansi), Vec::new()),
            Err(_) => (ansi_lines::to_lines(content, width), Vec::new()),
        },
    }
}
