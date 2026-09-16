//! mermaid 图表渲染（mermansi）。
//!
//! mermansi 是纯 Rust mermaid 终端渲染器，支持 28 种图类型，
//! 输出带 ANSI truecolor 的 ASCII/Unicode 文本。
//! max_width 适配终端宽度，图不溢出。
//!
//! 配色（DECISIONS D17，与 markdown skin 同源设计）：
//! mermansi 固定输出为浅色终端设计的 light 主题（文字近黑、结构暗灰），
//! 暗色终端上不可读；且形状节点 flowchart / class / pie / mindmap / journey
//! 等 fallback 路径完全无色。上游未暴露主题 API，故在 dlook 侧做两层修正：
//!   1. remap_ansi：把 mermansi 已着色的 SGR 重映射到 dlook 调色板
//!      （色值常量集合确定，按 SGR 参数串精确查表替换）
//!   2. colorize_plain：无色输出按盒绘/箭头字符启发式补色
//! 调色板与正文同源：文字=默认前景（同正文），边框/框架=青（同 h1/h2 色系），
//! 连线/生命线/坐标轴=蓝（同链接色系），箭头=琥珀，图表系列=truecolor。
//! 主题按终端背景亮度选择（termio 启动期 OSC 11/COLORFGBG 检测，默认暗色）。

use std::sync::atomic::{AtomicBool, Ordering};

use mermansi::{render_source, ColorMode, MermansiOptions, OutputMode};

/// 终端背景是否为暗色（启动期由 termio 检测后写入；未检测时默认暗色，
/// 与 D4 代码高亮固定暗色主题的立场一致）。
static DARK_BG: AtomicBool = AtomicBool::new(true);

/// 记录终端背景检测结果（TUI 启动期调用一次，须在首次渲染前）。
pub fn set_dark_bg(dark: bool) {
    DARK_BG.store(dark, Ordering::Relaxed);
}

fn is_dark_bg() -> bool {
    DARK_BG.load(Ordering::Relaxed)
}

/// 盒绘字符集合，用于识别图的最后一行（截掉 mermansi 追加的边列表）。
/// 去掉 mermansi 在图后追加的边列表（如 "A --> B"），只保留盒绘图。
/// 边列表行含 ASCII "-->"（图本身用 Unicode 盒绘字符 ─▶，不会误伤）。
fn strip_edge_legend(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    // 从末尾往前找：去掉所有"边列表行"（含 --> / --- / ==> 等ASCII边标记）
    // 以及它们前面的空行
    let mut cut = lines.len();
    let mut saw_edge = false;
    for i in (0..lines.len()).rev() {
        let trimmed = lines[i].trim();
        if trimmed.is_empty() {
            // 空行：如果在边列表区域，也去掉
            if saw_edge {
                cut = i;
                continue;
            } else {
                break;
            }
        }
        if is_edge_line(trimmed) {
            saw_edge = true;
            cut = i;
        } else if saw_edge {
            // 遇到非边列表行，停止
            break;
        }
    }
    if cut < lines.len() {
        lines[..cut].join("\n") + "\n"
    } else {
        text.to_string()
    }
}

/// 判断一行是否为 mermansi 追加的边列表行（ASCII 边标记）。
fn is_edge_line(s: &str) -> bool {
    s.contains("-->") || s.contains("---") || s.contains("==>") || s.contains("-.->")
}

/// mermansi 的 canvas cell 上限（max_width × max_height ≤ 此值）。
const MAX_CANVAS_CELLS: usize = 250_000;

/// 根据宽度动态计算 max_height，确保 canvas cells 不超限。
/// 宽终端时自动降低高度上限；窄终端时仍允许较高的图（上限 2000）。
fn safe_max_height(width: usize) -> usize {
    (MAX_CANVAS_CELLS / width.max(1)).min(2000)
}

/// 渲染 mermaid 源码 → 带 truecolor ANSI 的终端文本（TUI 模式用）。
/// 解析失败时返回 Err（调用方降级为单色源码显示）。
/// 输出已按终端背景重映射到 dlook 调色板；无色输出会启发式补色。
pub fn render_mermaid_to_ansi(src: &str, width: u16) -> Result<String, String> {
    let max_w = width as usize;
    let opts = MermansiOptions::unicode()
        .with_color(ColorMode::TrueColor) // truecolor ANSI
        .with_output_mode(OutputMode::Concise) // 只输出预览，不含语义 JSON
        .with_max_width(max_w)
        .with_max_height(safe_max_height(max_w));

    let dark = is_dark_bg();
    render_source(src, &opts)
        .map(|text| {
            let text = strip_edge_legend(&text);
            let text = remap_ansi(&text, dark);
            if text.contains("\x1b[") {
                text
            } else {
                colorize_plain(&text, dark)
            }
        })
        .map_err(|e| e.to_string())
}

/// 渲染 mermaid 源码 → 纯文本（无 ANSI 颜色），用于非 TTY 管道输出。
/// 解析失败时返回 Err（调用方降级为原始源码）。
pub fn render_mermaid_to_plain(src: &str) -> Result<String, String> {
    let max_w = 120;
    let opts = MermansiOptions::unicode()
        .with_color(ColorMode::Plain) // 无颜色，纯文本
        .with_output_mode(OutputMode::Concise)
        .with_max_width(max_w)
        .with_max_height(safe_max_height(max_w));

    render_source(src, &opts)
        .map(|text| strip_edge_legend(&text))
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// 调色板：mermansi 输出 SGR 重映射（有色路径）
// ---------------------------------------------------------------------------
//
// mermansi TrueColor 输出的 SGR 参数串是确定的常量集合，来源两处：
//   - merman-ascii light 主题（矩形 flowchart / sequence / state / xychart 等）
//   - mermansi AnsiRole（lanes flowchart / xychart 等，本身已偏暗色友好）
// 映射目标（与 markdown skin 同源，见文件头注释）：
//   文字类 → SGR 39 默认前景；结构青 #22d3ee(dark)/#0891b2(light)；
//   连线蓝 #60a5fa(dark)/#2563eb(light)；箭头琥珀 #ffc457(dark)/#b45309(light)；
//   图表系列 dark 用 400 系（亮），light 保持上游 600 系。

/// SGR 参数串（ESC[ 与 m 之间）→ 替换后的完整 SGR。键须按输入常量原文。
fn remap_table(dark: bool) -> &'static [(&'static str, &'static str)] {
    const DARK: &[(&str, &str)] = &[
        // merman-ascii light 主题
        ("38;2;39;39;42", "\x1b[39m"),          // 文字/边标签 → 默认前景
        ("38;2;113;113;122", "\x1b[38;2;96;165;250m"), // 连线/生命线/轴/次级 → 蓝
        ("38;2;161;161;170", "\x1b[38;2;34;211;238m"), // 边框/分组/框架 → 青
        ("38;2;82;82;91", "\x1b[38;2;255;196;87m"),    // 箭头 → 琥珀
        ("38;2;37;99;235", "\x1b[38;2;96;165;250m"),   // 激活条/系列0 → 蓝
        ("38;2;22;163;74", "\x1b[38;2;74;222;128m"),   // 系列 green → 400 系
        ("38;2;220;38;38", "\x1b[38;2;248;113;113m"),  // 系列 red
        ("38;2;147;51;234", "\x1b[38;2;192;132;252m"), // 系列 purple
        ("38;2;234;88;12", "\x1b[38;2;251;146;60m"),   // 系列 orange
        ("38;2;8;145;178", "\x1b[38;2;34;211;238m"),   // 系列 cyan → 青
        ("38;2;79;70;229", "\x1b[38;2;129;140;248m"),  // 系列 indigo
        ("38;2;190;18;60", "\x1b[38;2;251;113;133m"),  // 系列 rose
        // mermansi AnsiRole
        ("38;2;56;189;173", "\x1b[38;2;34;211;238m"),  // 节点边框 → 青
        ("38;2;230;237;243", "\x1b[39m"),              // 节点文字 → 默认前景
        ("38;2;88;166;255", "\x1b[38;2;96;165;250m"),  // 连线 → 蓝
        ("38;2;213;128;255", "\x1b[39m"),              // 边标签 → 默认前景
        ("1;38;2;255;255;255", "\x1b[1;39m"),          // 标题 → 粗体默认前景
    ];
    const LIGHT: &[(&str, &str)] = &[
        // merman-ascii light 主题（系列 600 系在浅底可读，保持）
        ("38;2;39;39;42", "\x1b[39m"),
        ("38;2;113;113;122", "\x1b[38;2;37;99;235m"),
        ("38;2;161;161;170", "\x1b[38;2;8;145;178m"),
        ("38;2;82;82;91", "\x1b[38;2;180;83;9m"),
        // mermansi AnsiRole（400 系在浅底偏淡，压到 600 系）
        ("38;2;56;189;173", "\x1b[38;2;8;145;178m"),
        ("38;2;230;237;243", "\x1b[39m"),
        ("38;2;88;166;255", "\x1b[38;2;37;99;235m"),
        ("38;2;255;196;87", "\x1b[38;2;180;83;9m"),
        ("38;2;213;128;255", "\x1b[39m"),
        ("1;38;2;255;255;255", "\x1b[1;39m"),
    ];
    if dark { DARK } else { LIGHT }
}

/// 单遍扫描 ANSI 文本，把已知 SGR 参数串替换为调色板 SGR；
/// 未知序列（含 OSC/CSI 非 SGR）原样保留，不产生链式二次替换。
fn remap_ansi(text: &str, dark: bool) -> String {
    let table = remap_table(dark);
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            // CSI: ESC [ 参数(0x30-0x3F) 中间(0x20-0x2F) 最终(0x40-0x7E)
            if i + 1 < bytes.len() && bytes[i + 1] == b'[' {
                let mut j = i + 2;
                while j < bytes.len() && (0x30..=0x3f).contains(&bytes[j]) {
                    j += 1;
                }
                let params_end = j;
                while j < bytes.len() && (0x20..=0x2f).contains(&bytes[j]) {
                    j += 1;
                }
                if j < bytes.len() && (0x40..=0x7e).contains(&bytes[j]) {
                    let params = &text[i + 2..params_end];
                    let final_byte = bytes[j] as char;
                    let mapped = (final_byte == 'm')
                        .then(|| table.iter().find(|(k, _)| *k == params).map(|(_, v)| *v));
                    match mapped.flatten() {
                        Some(v) => {
                            out.push_str(v);
                            i = j + 1;
                            continue;
                        }
                        None => {
                            out.push_str(&text[i..=j]);
                            i = j + 1;
                            continue;
                        }
                    }
                }
            }
        }
        // 非转义起点：原样拷贝本字节（ASCII/UTF-8 均按字节透传安全）
        let ch_len = utf8_len(bytes[i]);
        let end = (i + ch_len).min(bytes.len());
        out.push_str(&text[i..end]);
        i = end;
    }
    out
}

/// UTF-8 首字节 → 字符长度。
fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else if b >> 3 == 0b11110 {
        4
    } else {
        1 // 无效首字节按 1 透传
    }
}

// ---------------------------------------------------------------------------
// 调色板：无色输出补色（fallback 路径：形状节点 flowchart / class / pie /
// mindmap / journey，mermansi 输出完全无 ANSI）
// ---------------------------------------------------------------------------

/// 结构色（盒绘字符：节点边框、连线、交点）。
const fn structure_sgr(dark: bool) -> &'static str {
    if dark {
        "\x1b[38;2;34;211;238m" // 青 #22d3ee
    } else {
        "\x1b[38;2;8;145;178m" // cyan-600 #0891b2
    }
}

/// 箭头色。
const fn arrow_sgr(dark: bool) -> &'static str {
    if dark {
        "\x1b[38;2;255;196;87m" // 琥珀 #ffc457
    } else {
        "\x1b[38;2;180;83;9m" // amber-700 #b45309
    }
}

/// 盒绘字符（U+2500–U+257F：─│┌┐└┘├┤┬┴┼═║╔╗…含粗线变体）。
fn is_box_char(ch: char) -> bool {
    ('\u{2500}'..='\u{257f}').contains(&ch)
}

/// 实心箭头字符（▲▼◀▶◄► 等）。
fn is_arrow_char(ch: char) -> bool {
    matches!(
        ch,
        '\u{25b2}' | '\u{25b6}' | '\u{25ba}' | '\u{25bc}' | '\u{25c0}' | '\u{25c4}'
    )
}

enum CellKind {
    Box,
    Arrow,
    Text,
}

fn cell_kind(ch: char) -> CellKind {
    if is_box_char(ch) {
        CellKind::Box
    } else if is_arrow_char(ch) {
        CellKind::Arrow
    } else {
        CellKind::Text
    }
}

/// 对无 ANSI 的纯文本图做启发式补色：连续盒绘字符 → 结构色，
/// 箭头字符 → 箭头色，文字保持默认前景（正文同源）。
/// 仅当文本不含任何转义序列时调用（调用方已保证）。
fn colorize_plain(text: &str, dark: bool) -> String {
    fn flush(pending: &mut String, kind: Option<&CellKind>, dark: bool, out: &mut String) {
        if pending.is_empty() {
            return;
        }
        match kind {
            None | Some(CellKind::Text) => out.push_str(pending),
            Some(CellKind::Box) => {
                out.push_str(structure_sgr(dark));
                out.push_str(pending);
                out.push_str("\x1b[0m");
            }
            Some(CellKind::Arrow) => {
                out.push_str(arrow_sgr(dark));
                out.push_str(pending);
                out.push_str("\x1b[0m");
            }
        }
        pending.clear();
    }

    let mut out = String::with_capacity(text.len() + 64);
    let mut pending = String::new();
    let mut pending_kind: Option<CellKind> = None;
    for ch in text.chars() {
        if ch == '\n' || ch == '\r' {
            // 换行不进任何着色 run，先冲刷再透传
            flush(&mut pending, pending_kind.as_ref(), dark, &mut out);
            pending_kind = None;
            out.push(ch);
            continue;
        }
        let kind = cell_kind(ch);
        if !same_kind(pending_kind.as_ref(), &kind) {
            flush(&mut pending, pending_kind.as_ref(), dark, &mut out);
            pending_kind = Some(kind);
        }
        pending.push(ch);
    }
    flush(&mut pending, pending_kind.as_ref(), dark, &mut out);
    out
}

/// 两个 CellKind 是否同类（None 视作 Text，首个字符直接开新 run）。
fn same_kind(a: Option<&CellKind>, b: &CellKind) -> bool {
    match a {
        None => false,
        Some(a) => std::mem::discriminant(a) == std::mem::discriminant(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remap_dark_replaces_merman_ascii_light_theme() {
        let src = "\x1b[38;2;39;39;42mText\x1b[0m\x1b[38;2;113;113;122m│\x1b[0m\x1b[38;2;161;161;170m┌\x1b[0m\x1b[38;2;82;82;91m▼\x1b[0m";
        let out = remap_ansi(src, true);
        assert!(out.contains("\x1b[39mText\x1b[0m"));
        assert!(out.contains("\x1b[38;2;96;165;250m│\x1b[0m"));
        assert!(out.contains("\x1b[38;2;34;211;238m┌\x1b[0m"));
        assert!(out.contains("\x1b[38;2;255;196;87m▼\x1b[0m"));
        assert!(!out.contains("39;39;42"));
    }

    #[test]
    fn remap_dark_maps_series_to_bright_palette() {
        let out = remap_ansi("\x1b[38;2;220;38;38m###\x1b[0m", true);
        assert_eq!(out, "\x1b[38;2;248;113;113m###\x1b[0m");
    }

    #[test]
    fn remap_light_uses_600_palette_and_keeps_series() {
        let out = remap_ansi("\x1b[38;2;161;161;170m┌\x1b[0m\x1b[38;2;220;38;38m#\x1b[0m", false);
        assert!(out.contains("\x1b[38;2;8;145;178m┌\x1b[0m"));
        // 系列 600 系保持原样
        assert!(out.contains("\x1b[38;2;220;38;38m#\x1b[0m"));
    }

    #[test]
    fn remap_keeps_unknown_sequences_verbatim() {
        let src = "\x1b[38;2;1;2;3m?\x1b[0m\x1b[?25l";
        assert_eq!(remap_ansi(src, true), src);
    }

    #[test]
    fn remap_maps_mermansi_title_to_bold_default() {
        let out = remap_ansi("\x1b[1;38;2;255;255;255mT\x1b[0m", true);
        assert_eq!(out, "\x1b[1;39mT\x1b[0m");
    }

    #[test]
    fn colorize_colors_box_runs_and_arrows_only() {
        let src = "  ┌────┐\n  │ A  │\n    ▼\nplain text\n";
        let out = colorize_plain(src, true);
        assert!(out.contains("\x1b[38;2;34;211;238m┌────┐\x1b[0m"));
        assert!(out.contains("\x1b[38;2;34;211;238m│\x1b[0m"));
        // 文字与箭头行不混入盒绘色
        assert!(out.contains(" A  "));
        assert!(out.contains("\x1b[38;2;255;196;87m▼\x1b[0m"));
        assert!(out.contains("plain text\n"));
    }

    #[test]
    fn colorize_light_uses_cyan600_amber700() {
        let out = colorize_plain("┌─┐\n▼\n", false);
        assert!(out.contains("\x1b[38;2;8;145;178m┌─┐\x1b[0m"));
        assert!(out.contains("\x1b[38;2;180;83;9m▼\x1b[0m"));
    }

    #[test]
    fn render_ansi_uses_dark_palette_by_default() {
        // 默认（未检测）按暗色处理：有色路径不再出现近黑文字色
        let src = "flowchart TD\n    A[Alpha] --> B[Beta]\n";
        let out = render_mermaid_to_ansi(src, 80).expect("render ok");
        assert!(out.contains("\x1b[38;2;34;211;238m"), "cyan border expected");
        assert!(!out.contains("39;39;42"), "light-theme text color must be gone");
    }

    #[test]
    fn render_ansi_colorizes_fallback_shapes() {
        // 形状节点走 mermansi fallback（原本无色）→ 盒绘补色
        let src = "flowchart TD\n    A([Start]) --> B{Decide}\n";
        let out = render_mermaid_to_ansi(src, 80).expect("render ok");
        assert!(out.contains("\x1b[38;2;34;211;238m"), "box run colored");
    }
}
