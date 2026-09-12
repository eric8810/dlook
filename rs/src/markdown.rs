//! markdown 渲染：minimad/termimad 排版 + code fence 拦截分流。
//!
//! 流程：
//!   1. 预扫描 raw markdown，识别围栏代码块（``` 或 ~~~）
//!   2. 将文档拆分为 prose 段落 + code block 段落
//!   3. prose → termimad FmtText(Display 产出 ANSI) → ansi-to-tui → Vec<Line>
//!   4. code block:
//!      - lang="mermaid" → mermansi 渲染图(ANSI) → ansi-to-tui
//!      - lang=其它       → syntect 高亮(ANSI)   → ansi-to-tui
//!      - 无 lang         → 纯文本

use std::fmt::Write;

use ratatui::style::{Color as RColor, Modifier, Style};
use ratatui::text::{Line, Span};
use termimad::{FmtText, MadSkin};

use crate::ansi_lines;
use crate::highlight::Highlighter;
use crate::links::{self, LinkSpan};
use crate::mermaid;

/// 将 markdown 源码渲染为 ratatui Vec<Line>,同时收集行内链接的可点击区域。
pub fn markdown_to_lines(
    md: &str,
    width: u16,
    skin: &MadSkin,
    hl: &Highlighter,
) -> (Vec<Line<'static>>, Vec<LinkSpan>) {
    let segments = split_at_fences(md);
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut links: Vec<LinkSpan> = Vec::new();

    for seg in segments {
        match seg {
            Segment::Prose(text) => {
                // 任务列表 checkbox 预处理(仅 prose,代码块内不处理)
                let text = render_task_checkboxes(&text);
                let ansi = render_prose_to_ansi(&text, width, skin);
                let lines = ansi_lines::to_lines(&ansi, width);
                // 先补表格圆角外框(会插入行),再做链接样式化——
                // 这样 LinkSpan.line 基于最终行集,表格后移的链接行号正确。
                let base = out.len();
                let (styled, mut seg_links) = style_links(frame_tables(lines));
                for l in &mut seg_links {
                    l.line += base;
                }
                out.extend(styled);
                links.append(&mut seg_links);
            }
            Segment::Code { lang, code } => {
                out.extend(render_code_fence(&code, lang.as_deref(), width, hl));
            }
        }
    }
    (out, links)
}

/// 一个文档段：prose 或 code block。
enum Segment {
    Prose(String),
    Code { lang: Option<String>, code: String },
}

/// 预扫描 markdown，按围栏代码块拆分。
fn split_at_fences(md: &str) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut prose_buf = String::new();
    let mut lines = md.lines().peekable();

    while let Some(line) = lines.next() {
        if let Some((fence_char, info)) = parse_fence_open(line) {
            // Flush prose
            if !prose_buf.is_empty() {
                prose_buf.pop(); // 去掉末尾多余 \n
                segments.push(Segment::Prose(std::mem::take(&mut prose_buf)));
            }
            // Accumulate code until closing fence
            let mut code = String::new();
            let close_marker: String = (0..3).map(|_| fence_char).collect();
            for code_line in lines.by_ref() {
                if is_fence_close(code_line, &close_marker) {
                    break;
                }
                code.push_str(code_line);
                code.push('\n');
            }
            let lang = info.trim().split_whitespace().next().map(|s| s.to_string());
            segments.push(Segment::Code { lang, code });
        } else {
            prose_buf.push_str(line);
            prose_buf.push('\n');
        }
    }

    // Flush remaining prose
    if !prose_buf.is_empty() {
        prose_buf.pop();
        segments.push(Segment::Prose(prose_buf));
    }

    segments
}

/// 检测围栏开始行，返回 (围栏字符, info 字符串)。
/// 支持 ``` 和 ~~~（3+ 个相同字符），允许最多 3 个前导空格。
fn parse_fence_open(line: &str) -> Option<(char, String)> {
    let trimmed = line.trim_start();
    let leading_spaces = line.len() - trimmed.len();
    if leading_spaces > 3 {
        return None;
    }

    let first = trimmed.chars().next()?;
    if first != '`' && first != '~' {
        return None;
    }

    let fence_str: String = trimmed.chars().take_while(|&c| c == first).collect();
    if fence_str.len() < 3 {
        return None;
    }

    let info = trimmed[fence_str.len()..].to_string();
    // 反引号围栏的 info 中不能有反引号
    if first == '`' && info.contains('`') {
        return None;
    }

    Some((first, info))
}

/// 检测围栏结束行。
fn is_fence_close(line: &str, close_marker: &str) -> bool {
    let trimmed = line.trim_start();
    let leading_spaces = line.len() - trimmed.len();
    if leading_spaces > 3 {
        return false;
    }
    // 结束行只有围栏字符（可能有尾部空格）
    if !trimmed.starts_with(close_marker) {
        return false;
    }
    let rest = &trimmed[close_marker.len()..];
    // 允许更多围栏字符（如 ```` 关闭 ```）
    if !rest.chars().all(|c| c == close_marker.chars().next().unwrap()) {
        return rest.trim().is_empty();
    }
    true
}

/// 用 termimad 渲染 prose 段 → ANSI 字符串。
fn render_prose_to_ansi(prose: &str, width: u16, skin: &MadSkin) -> String {
    let fmt = FmtText::from(skin, prose, Some(width as usize));
    let mut buf = String::new();
    let _ = write!(buf, "{}", fmt);
    buf
}

/// 渲染围栏代码块 → Vec<Line>。
fn render_code_fence(
    code: &str,
    lang: Option<&str>,
    width: u16,
    hl: &Highlighter,
) -> Vec<Line<'static>> {
    match lang {
        Some("mermaid") => {
            match mermaid::render_mermaid_to_ansi(code, width) {
                Ok(ansi) => ansi_lines::to_lines_untruncated(&ansi),
                Err(_) => {
                    // 降级：当普通代码显示
                    let ansi = hl.highlight_to_ansi(code, None);
                    ansi_lines::to_lines(&ansi, width)
                }
            }
        }
        Some(lang) => {
            // 归一化代码块语言名 → syntect 可识别的 token
            let token = normalize_fence_lang(lang);
            let ansi = hl.highlight_to_ansi(code, token.as_deref());
            ansi_lines::to_lines(&ansi, width)
        }
        None => {
            let ansi = hl.highlight_to_ansi(code, None);
            ansi_lines::to_lines(&ansi, width)
        }
    }
}

/// 将代码块的语言标识（可能带大小写/别名）归一化为 syntect 可识别的 token。
/// two-face 全量语法集下,大部分语言名可直接命中(DECISIONS D4),
/// 仍保留常见别名;其余原样返回,交由 Highlighter::find_syntax 按扩展名/名称匹配。
fn normalize_fence_lang(lang: &str) -> Option<String> {
    let lower = lang.trim().to_lowercase();
    if lower.is_empty() {
        return None;
    }
    let token = match lower.as_str() {
        "typescript" | "ts" | "tsx" | "mts" | "cts" => "ts",
        "javascript" | "js" | "jsx" | "mjs" | "cjs" => "js",
        "python" | "py" | "pyi" => "py",
        "rust" | "rs" => "rs",
        "golang" => "go",
        "c++" | "cpp" | "cc" | "cxx" | "hpp" | "hxx" => "cpp",
        "csharp" | "cs" => "cs",
        "shell" | "sh" | "bash" | "zsh" | "fish" => "sh",
        "yaml" | "yml" => "yaml",
        "markdown" | "md" => "md",
        "powershell" | "pwsh" => "ps1",
        _ => return Some(lower),
    };
    Some(token.to_string())
}

// ---------------------------------------------------------------------------
// 任务列表 checkbox(DECISIONS D6,GAP G5)
// ---------------------------------------------------------------------------

/// 把 prose 段落里列表项行首的 `[x]`/`[X]`/`[ ]` 替换为 ☑ / ☐。
/// minimad 无 checkbox 语法,不预处理会原样渲染方括号文本。
fn render_task_checkboxes(md: &str) -> String {
    let mut out = String::with_capacity(md.len() + 8);
    for line in md.split_inclusive('\n') {
        out.push_str(&replace_task_marker(line));
    }
    out
}

/// 单行处理:识别「(缩进)(列表标记) [x]/[X]/[ ] (空格或行尾)」并替换。
/// 非任务列表行原样返回。
fn replace_task_marker(line: &str) -> String {
    let indent = line.len() - line.trim_start_matches(' ').len();
    let rest = &line[indent..];
    let after_marker = rest
        .strip_prefix("- ")
        .or_else(|| rest.strip_prefix("* "))
        .or_else(|| rest.strip_prefix("+ "))
        .or_else(|| strip_ordered_marker(rest));
    let Some(after_marker) = after_marker else {
        return line.to_string();
    };
    let Some((sym, tail)) = checkbox_of(after_marker) else {
        return line.to_string();
    };
    let body_at = line.len() - after_marker.len();
    let mut out = String::with_capacity(line.len() + 2);
    out.push_str(&line[..body_at]);
    out.push_str(sym);
    out.push_str(tail);
    out
}

/// 识别有序列表标记 `N. ` / `N) `,返回标记后的剩余文本。
fn strip_ordered_marker(s: &str) -> Option<&str> {
    let n = s.chars().take_while(|c| c.is_ascii_digit()).count();
    if n == 0 {
        return None;
    }
    let tail = &s[n..];
    tail.strip_prefix(". ").or_else(|| tail.strip_prefix(") "))
}

/// 识别 `[x] `/`[X] `/`[ ] `(后跟空格)或行尾单独的 `[x]`,
/// 返回 (替换符号含尾随空格, 剩余文本)。
fn checkbox_of(s: &str) -> Option<(&'static str, &str)> {
    for (prefix, sym) in [("[x] ", "☑ "), ("[X] ", "☑ "), ("[ ] ", "☐ ")] {
        if let Some(rest) = s.strip_prefix(prefix) {
            return Some((sym, rest));
        }
    }
    // 行尾/无内容形式(允许紧跟换行)
    for (whole, sym) in [("[x]\n", "☑\n"), ("[X]\n", "☑\n"), ("[ ]\n", "☐\n")] {
        if s == whole {
            return Some((sym, ""));
        }
    }
    for (whole, sym) in [("[x]", "☑"), ("[X]", "☑"), ("[ ]", "☐")] {
        if s == whole {
            return Some((sym, ""));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// 行内链接样式化(DECISIONS D3 / B1,GAP G3)
// ---------------------------------------------------------------------------

const LINK_LABEL_FG: RColor = RColor::LightBlue; // blueBright
const LINK_URL_FG: RColor = RColor::DarkGray;

fn link_label_style() -> Style {
    Style::default()
        .fg(LINK_LABEL_FG)
        .add_modifier(Modifier::UNDERLINED)
}

fn link_url_style() -> Style {
    Style::default().fg(LINK_URL_FG)
}

/// 对渲染后的行做行内链接样式化 + 收集可点击区域:
/// `[label](url)` → `label`(亮蓝+下划线)+ `" (url)"`(暗灰);
/// 本地文件链接(label 无 scheme/非锚点)额外加 `↗` 标记,可点击区域
/// 覆盖 label 与 `(url)` 后缀(整段渲染结果,与浏览器一致)。
/// 仅处理无样式的 span(行内代码/加粗等已有样式的片段跳过),
/// 且要求整个链接落在同一个 span 内(termimad 对未识别语法的输出即如此)。
fn style_links(lines: Vec<Line<'static>>) -> (Vec<Line<'static>>, Vec<LinkSpan>) {
    let mut all_links: Vec<LinkSpan> = Vec::new();
    let lines = lines
        .into_iter()
        .enumerate()
        .map(|(li, line)| {
            if line
                .spans
                .iter()
                .any(|s| is_plain(s.style) && contains_link(&s.content))
            {
                let mut spans: Vec<Span<'static>> = Vec::new();
                let mut col = 0usize; // 已产出的字符列(行内)
                for span in line.spans {
                    let (sub, sub_links) = style_span_links(span);
                    for (s0, s1, target) in sub_links {
                        all_links.push(LinkSpan {
                            line: li,
                            start: col + s0,
                            end: col + s1,
                            target,
                        });
                    }
                    col += sub.iter().map(|s| s.content.chars().count()).sum::<usize>();
                    spans.extend(sub);
                }
                Line::default().spans(spans).style(line.style)
            } else {
                line
            }
        })
        .collect();
    (lines, all_links)
}

/// span 无有效前景/背景/修饰样式。
/// termimad 经 ansi-to-tui 转换后,普通文本的 fg/bg 常为 `Some(Reset)`(SGR 39/49),
/// 并带全量属性清除位(not_bold 等 sub_modifier);这些都不构成视觉样式,视为无样式。
fn is_plain(style: Style) -> bool {
    let fg_plain = matches!(style.fg, None | Some(RColor::Reset));
    let bg_plain = matches!(style.bg, None | Some(RColor::Reset));
    let uc_plain = matches!(style.underline_color, None | Some(RColor::Reset));
    fg_plain && bg_plain && uc_plain && style.add_modifier.is_empty()
}

/// 展开一个 span 内的所有链接。
/// 返回 (新 span 列表, 链接区域列表);区域坐标相对 span 产出的首字符。
fn style_span_links(span: Span<'static>) -> (Vec<Span<'static>>, Vec<(usize, usize, String)>) {
    if !is_plain(span.style) || !contains_link(&span.content) {
        return (vec![span], Vec::new());
    }
    let s: &str = &span.content;
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut hits: Vec<(usize, usize, String)> = Vec::new();
    let mut pos = 0usize; // 原文本扫描位置
    let mut col = 0usize; // 产出文本当前列
    while let Some((start, label, url, end)) = find_link(s, pos) {
        if start > pos {
            let lead = s[pos..start].to_string();
            col += lead.chars().count();
            out.push(Span::raw(lead));
        }
        // 本地文件链接加 ↗ 标记(D14)
        let label_text = if links::is_local_target(url) {
            format!("{label}↗")
        } else {
            label.to_string()
        };
        let url_text = format!(" ({url})");
        let l_start = col;
        let label_len = label_text.chars().count();
        let url_len = url_text.chars().count();
        out.push(Span::styled(label_text, link_label_style()));
        col += label_len;
        out.push(Span::styled(url_text, link_url_style()));
        col += url_len;
        hits.push((l_start, col, url.to_string()));
        pos = end;
    }
    if pos < s.len() {
        out.push(Span::raw(s[pos..].to_string()));
    }
    if out.is_empty() {
        out.push(span);
    }
    (out, hits)
}

fn contains_link(s: &str) -> bool {
    find_link(s, 0).is_some()
}

// ---------------------------------------------------------------------------
// 表格圆角外框(DECISIONS D7,GAP G6)
// ---------------------------------------------------------------------------

/// 给表格补上下圆角边框。
/// termimad 的 FmtText 路径把 TableRule 固定为 Other 位置,只画行分隔线
/// (├─┼─┤),不画外框;这里按分隔线的几何,在表格块首尾插入
/// `╭─┬─╮` / `╰─┴─╯`(样式取分隔线),对齐 vue-tui 的圆角表格观感。
fn frame_tables(lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::with_capacity(lines.len() + 4);
    let mut i = 0usize;
    while i < lines.len() {
        if !line_text(&lines[i]).starts_with('│') {
            out.push(lines[i].clone());
            i += 1;
            continue;
        }
        // 表格块起点(表头行):向后吞掉连续的 行/分隔线
        let mut j = i;
        let mut rule: Option<(String, Style)> = None;
        while j < lines.len() {
            let t = line_text(&lines[j]);
            if t.starts_with('│') {
                j += 1;
            } else if t.starts_with('├') {
                if rule.is_none() {
                    rule = Some((t, first_span_style(&lines[j])));
                }
                j += 1;
            } else {
                break;
            }
        }
        match rule {
            Some((rt, st)) => {
                if let Some(top) = border_line(&rt, st, true) {
                    out.push(top);
                }
                out.extend(lines[i..j].iter().cloned());
                if let Some(bottom) = border_line(&rt, st, false) {
                    out.push(bottom);
                }
            }
            None => out.extend(lines[i..j].iter().cloned()),
        }
        i = j;
    }
    out
}

/// 一行的纯文本(spans 拼接)。
fn line_text(line: &Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// 第一个非空 span 的样式(边框样式参考)。
fn first_span_style(line: &Line<'_>) -> Style {
    line.spans
        .iter()
        .find(|s| !s.content.is_empty())
        .map(|s| s.style)
        .unwrap_or_default()
}

/// 按分隔线几何生成顶/底边框行。
/// 映射:├→╭/╰,┼→┬/┴,┤→╮/╯,─ 保持。
fn border_line(rule_text: &str, style: Style, top: bool) -> Option<Line<'static>> {
    if rule_text.is_empty() {
        return None;
    }
    let mapped: String = rule_text
        .chars()
        .map(|c| match c {
            '├' => {
                if top {
                    '╭'
                } else {
                    '╰'
                }
            }
            '┼' => {
                if top {
                    '┬'
                } else {
                    '┴'
                }
            }
            '┤' => {
                if top {
                    '╮'
                } else {
                    '╯'
                }
            }
            other => other,
        })
        .collect();
    if mapped == rule_text {
        return None; // 不是可识别的分隔线
    }
    Some(Line::default().spans(vec![Span::styled(mapped, style)]))
}

/// 在 `s[from..]` 查找行内链接 `[label](url)`,
/// 返回 `(匹配起点, label, url, 匹配终点)`。
/// 语法(简化):label 不含未转义 `[`/`]`;url 两种形式——
/// 裸形式(不含空白与 `)`)或 `<>` 包裹(允许空白,CommonMark 风格);忽略图片 `![...]`。
fn find_link(s: &str, from: usize) -> Option<(usize, &str, &str, usize)> {
    let b = s.as_bytes();
    let mut i = from;
    while i < b.len() {
        if b[i] != b'[' {
            i += 1;
            continue;
        }
        // 排除图片语法 ![alt](url)
        if i > 0 && b[i - 1] == b'!' {
            i += 1;
            continue;
        }
        // 找配对的 ]
        let mut j = i + 1;
        let mut depth = 1usize;
        while j < b.len() {
            match b[j] {
                b'\\' => j += 1,
                b'[' => depth += 1,
                b']' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            j += 1;
        }
        if j >= b.len() || b[j] != b']' || j == i + 1 {
            // 无配对 ] 或 label 为空
            i += 1;
            continue;
        }
        // 紧跟 (url)
        if j + 1 >= b.len() || b[j + 1] != b'(' {
            i = j + 1;
            continue;
        }
        let mut k = j + 2;
        // <> 包裹形式:扫到配对 >,url 为其内文本(允许空白)
        if k < b.len() && b[k] == b'<' {
            k += 1;
            let url_start = k;
            while k < b.len() && b[k] != b'>' {
                k += 1;
            }
            if k < b.len() && b[k] == b'>' && k + 1 < b.len() && b[k + 1] == b')' && k > url_start
            {
                let label = &s[i + 1..j];
                let url = &s[url_start..k];
                return Some((i, label, url, k + 2));
            }
            i = j + 1;
            continue;
        }
        // 裸形式:不含空白与 )
        while k < b.len() && b[k] != b')' && !b[k].is_ascii_whitespace() {
            if b[k] == b'\\' {
                k += 1;
            }
            k += 1;
        }
        if k < b.len() && b[k] == b')' && k > j + 2 {
            let label = &s[i + 1..j];
            let url = &s[j + 2..k];
            if !url.is_empty() {
                return Some((i, label, url, k + 1));
            }
        }
        i = j + 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- find_link(DECISIONS D3)----

    #[test]
    fn find_link_basic() {
        let s = "see [example link](https://example.com/path) here";
        let (start, label, url, end) = find_link(s, 0).unwrap();
        assert_eq!(start, 4);
        assert_eq!(label, "example link");
        assert_eq!(url, "https://example.com/path");
        assert_eq!(&s[end..], " here");
    }

    #[test]
    fn find_link_ignores_images_and_bad_syntax() {
        // 图片语法不匹配
        assert!(find_link("![alt](img.png)", 0).is_none());
        // 未闭合
        assert!(find_link("[unclosed](https://x", 0).is_none());
        // 空 label
        assert!(find_link("[](https://x)", 0).is_none());
        // 空 url
        assert!(find_link("[a]()", 0).is_none());
        // url 含空格 → 不匹配
        assert!(find_link("[a](https://x y)", 0).is_none());
    }

    #[test]
    fn find_link_nested_brackets_in_label() {
        let (_, label, url, _) = find_link("[a [b] c](https://x)", 0).unwrap();
        assert_eq!(label, "a [b] c");
        assert_eq!(url, "https://x");
    }

    #[test]
    fn find_link_angle_bracket_url_with_spaces() {
        let s = "see [doc](<my file.md>) now";
        let (start, label, url, end) = find_link(s, 0).unwrap();
        assert_eq!(start, 4);
        assert_eq!(label, "doc");
        assert_eq!(url, "my file.md");
        assert_eq!(&s[end..], " now");
        // 未闭合 <> → 不匹配
        assert!(find_link("[a](<unclosed.md)", 0).is_none());
        // 空 <> → 不匹配
        assert!(find_link("[a](<>)", 0).is_none());
    }

    // ---- 任务列表 checkbox(DECISIONS D6)----

    #[test]
    fn task_checkbox_unordered() {
        assert_eq!(replace_task_marker("- [x] done"), "- ☑ done");
        assert_eq!(replace_task_marker("- [X] done"), "- ☑ done");
        assert_eq!(replace_task_marker("* [ ] open"), "* ☐ open");
        assert_eq!(replace_task_marker("+ [x] done\n"), "+ ☑ done\n");
    }

    #[test]
    fn task_checkbox_ordered_and_nested() {
        assert_eq!(replace_task_marker("3. [ ] step"), "3. ☐ step");
        assert_eq!(replace_task_marker("  - [x] nested"), "  - ☑ nested");
    }

    #[test]
    fn task_checkbox_ignores_non_task() {
        assert_eq!(replace_task_marker("- [x]nospace"), "- [x]nospace");
        assert_eq!(replace_task_marker("- plain item"), "- plain item");
        assert_eq!(replace_task_marker("text [x] inline"), "text [x] inline");
        assert_eq!(replace_task_marker("1) not checkbox [x]"), "1) not checkbox [x]");
    }

    // ---- 表格圆角外框(DECISIONS D7)----

    #[test]
    fn border_line_maps_rule_chars() {
        let st = Style::default();
        let top = border_line("├─────┼─────┤", st, true).unwrap();
        let bot = border_line("├─────┼─────┤", st, false).unwrap();
        assert_eq!(line_text(&top), "╭─────┬─────╮");
        assert_eq!(line_text(&bot), "╰─────┴─────╯");
        // 不可识别输入 → None
        assert!(border_line("plain text", st, true).is_none());
    }

    #[test]
    fn frame_tables_wraps_table_block() {
        let mk = |t: &str| Line::default().spans(vec![Span::raw(t.to_string())]);
        let lines = vec![
            mk("para before"),
            mk("│col a│col b│"),
            mk("├─────┼─────┤"),
            mk("│1    │2    │"),
            mk("para after"),
        ];
        let out = frame_tables(lines);
        let texts: Vec<String> = out.iter().map(line_text).collect();
        assert_eq!(
            texts,
            vec![
                "para before",
                "╭─────┬─────╮",
                "│col a│col b│",
                "├─────┼─────┤",
                "│1    │2    │",
                "╰─────┴─────╯",
                "para after",
            ]
        );
    }

    // ---- 链接样式化端到端(D3/D14)----

    #[test]
    fn style_links_renders_label_and_url() {
        let lines = vec![Line::default().spans(vec![Span::raw(
            "see [docs](https://example.com) end",
        )])];
        let (out, links) = style_links(lines);
        assert_eq!(out.len(), 1);
        // 外部链接也有可点击区域(点击 → 系统打开器),只是不加 ↗ 标记
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "https://example.com");
        assert_eq!(links[0].start, 4);
        assert_eq!(
            links[0].end,
            4 + "docs (https://example.com)".chars().count()
        );
        let spans = &out[0].spans;
        // before / label / (url) / after 四段
        assert_eq!(spans.len(), 4);
        assert_eq!(spans[0].content, "see ");
        assert_eq!(spans[1].content, "docs");
        assert_eq!(spans[2].content, " (https://example.com)");
        assert_eq!(spans[3].content, " end");
        assert!(spans[1].style.add_modifier.contains(Modifier::UNDERLINED));
        assert_eq!(spans[1].style.fg, Some(RColor::LightBlue));
        assert_eq!(spans[2].style.fg, Some(RColor::DarkGray));
    }

    #[test]
    fn style_links_local_gets_marker_and_span() {
        let lines = vec![Line::default().spans(vec![Span::raw(
            "see [guide](./guide.md) end",
        )])];
        let (out, links) = style_links(lines);
        assert_eq!(links.len(), 1);
        let l = &links[0];
        assert_eq!(l.target, "./guide.md");
        assert_eq!(l.start, 4); // "see " 之后
        // 可点击区域覆盖 label↗ + " (./guide.md)"
        assert_eq!(l.end, 4 + "guide↗ (./guide.md)".chars().count());
        let text: String = out[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "see guide↗ (./guide.md) end");
    }

    #[test]
    fn style_links_multiple_and_position_offset() {
        // 第二个链接的列坐标要加上前文产出的字符数
        let lines = vec![Line::default().spans(vec![Span::raw(
            "[a](a.md) then [b](b.md)",
        )])];
        let (_, links) = style_links(lines);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].start, 0);
        assert_eq!(links[0].target, "a.md");
        // "a↗ (a.md) then " 长度后开始
        let offset = "a↗ (a.md) then ".chars().count();
        assert_eq!(links[1].start, offset);
        assert_eq!(links[1].target, "b.md");
    }

    #[test]
    fn markdown_to_lines_collects_links_across_table_and_fence() {
        let md = "# T\n\nsee [docs](./sub.md) here\n\n| a | b |\n|---|---|\n| [cell](c.md) | 2 |\n\nafter [two](x.md)\n\n```text\n[fenced](y.md)\n```\n";
        let skin = MadSkin::default();
        let hl = Highlighter::new();
        let (lines, links) = markdown_to_lines(md, 60, &skin, &hl);

        // prose 链接 3 个(docs/cell/two),code fence 内不算
        assert_eq!(links.len(), 3);
        let targets: Vec<&str> = links.iter().map(|l| l.target.as_str()).collect();
        assert_eq!(targets, vec!["./sub.md", "c.md", "x.md"]);

        // 每个链接的 line 都应指向含对应 label↗ 的渲染行
        // (表格块会插入圆角边框行,frame_tables 之后 style_links 的行号必须正确)
        for (l, label) in links.iter().zip(["docs", "cell", "two"]) {
            let line_text: String = lines[l.line]
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect();
            assert!(
                line_text.contains(&format!("{label}↗")),
                "line {} should contain {label}↗, got: {line_text}",
                l.line
            );
        }
        // fence 行不是链接
        let fence_line = lines
            .iter()
            .position(|ln| {
                let t: String = ln.spans.iter().map(|s| s.content.as_ref()).collect();
                t.contains("[fenced](y.md)")
            })
            .unwrap();
        assert!(links.iter().all(|l| l.line != fence_line));
    }
}
