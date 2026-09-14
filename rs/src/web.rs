//! 网页文本预览(DECISIONS D16,研究 L1 层):抓取 HTML → 类型化注解 → dlook 行模型。
//!
//! 设计要点(研究依据见 docs/research/media/evidence-web.md):
//!   - `render()` 阻塞(网络 IO),调用方放后台线程;沿用图片先例的限额:
//!     超时 10s、响应上限 16MB、重定向 ≤5、UA `dlook/<version>`(rustls,ureq)。
//!   - 渲染路径:`html2text::from_read_rich()` 的 `RichAnnotation`
//!     (Link/Strong/Emphasis/Code/Preformat/Image) → `Vec<Line>` + `LinkSpan`,
//!     复用 dlook 既有链接样式化/点击跳转/历史栈。
//!   - 安全(aerc 先例):**不拉取任何子资源**——`<img>`/CSS/JS 只作为注解出现,
//!     图片本体不进图片管线(防追踪像素;后续若接线需显式限额,见研究 §L1)。
//!   - 相对链接按 base(最终 URL,含重定向后)解析为绝对 URL 存入 LinkSpan.target。
//!   - 本地 .html/.htm 文件:读文件,base = 该文件的 `file://` URL(绝对化)。
//!
//! 已知边界(MVP):
//!   - `javascript:`/`data:` 目标丢弃(不建 LinkSpan);页面自带配色(Colour/BgColour
//!     注解)忽略,终端主题优先;`<img>` 无 alt 时 html2text 不产出任何注解,
//!     该图在行模型里消失(dlook 无从得知 src,故不请求也不占位)。
//!   - `_skin` 暂未参与样式:网页行样式用本文件内常量与 markdown 观感对齐。
//!     TODO(style): 与 markdown.rs 的 LINK_LABEL_FG/LINK_URL_FG 重复,后续提取公共常量。
//!   - 字符集:非 UTF-8 由本文件末尾内置 GB18030 表解码(**临时**方案,~72KB 生成数据 =
//!     71.7KB UTF-8 串 + 2.5KB 四字节区间表;数据取自 WHATWG gb18030 表,已逐序列对
//!     encoding_rs 校验)。`encoding_rs` 已在依赖图中(merman→lol_html 与 rodio→symphonia
//!     间接引入,`cargo tree -i encoding_rs` 可验证 → 零新增下载/编译),但**体积非零**:
//!     实测(opt-level=z + lto=fat + strip)直接调用 `GB18030::decode` 最多 +147KiB
//!     (经 `Encoding::for_label` 动态派发 +176KiB),与本内置表同量级。主 agent 批准写入
//!     Cargo.toml 后应删除 `GBK_ROWS`/`GB18030_4BYTE_RANGES` 并改用
//!     `encoding_rs::GB18030::decode`(顺带修好 Big5/Shift_JIS/EUC-KR 的乱码)。
//!     其余字符集(Big5/Shift_JIS/KOI8-R…)当前按 UTF-8 lossy 兜底:不 panic,可能乱码。

use std::io::Read;
use std::path::Path;
use std::time::Duration;

use html2text::render::{RichAnnotation, TaggedLine};
use ratatui::style::{Color as RColor, Modifier, Style};
use ratatui::text::{Line, Span};
use termimad::MadSkin;

use crate::links::LinkSpan;

/// 来源大小上限(与 images.rs 的 `MAX_REMOTE_BYTES` 同值)。
const MAX_BYTES: u64 = 16 * 1024 * 1024;
/// 抓取超时(与 images.rs `FETCH_TIMEOUT` 同值)。
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
/// 重定向跳数上限。
const MAX_REDIRECTS: u32 = 5;
/// UA:便于站点区分(研究 §L1)。
const USER_AGENT: &str = concat!("dlook/", env!("CARGO_PKG_VERSION"));
/// `<meta charset>` 嗅探窗口(文档头)。
const META_SNIFF_BYTES: usize = 4096;
/// `<title>` 提取窗口(必须出现在 head 里)。
const TITLE_SCAN_BYTES: usize = 64 * 1024;

// ---- 链接/代码观感(与 markdown.rs 一致;TODO(style) 提取公共常量)----
const LINK_LABEL_FG: RColor = RColor::LightBlue;
const LINK_URL_FG: RColor = RColor::DarkGray;
const INLINE_CODE_FG: RColor = RColor::LightCyan;
const IMAGE_FG: RColor = RColor::DarkGray;

/// 渲染结果:行模型 + 可点击链接 + 页面元信息。
pub struct WebDoc {
    /// 页面标题(`<title>`,缺失时回退为 URL/文件名)。
    pub title: String,
    /// 最终 URL(重定向后);本地文件为 file:// URL。
    pub final_url: String,
    pub lines: Vec<Line<'static>>,
    pub links: Vec<LinkSpan>,
}

/// 抓取 + 渲染一个网页(阻塞;失败返回可读错误文案,交状态栏展示)。
/// `source` 为 http(s) URL 或本地 .html/.htm 路径。
pub fn render(source: &str, width: u16, _skin: &MadSkin) -> Result<WebDoc, String> {
    let src = source.trim();
    if src.is_empty() {
        return Err("empty source".into());
    }
    let got = if is_web_url(src) {
        fetch_url(src)?
    } else {
        read_local(src)?
    };
    let text = decode_bytes(&got.bytes, got.charset.as_deref());
    if text.trim().is_empty() {
        return Err("empty page".into());
    }
    let title = page_title(&text).unwrap_or_else(|| fallback_title(&got.final_url));
    // html2text 按 width 折行正文;pre/表格等超宽行随后由 layout() 兜底折行,保证 ≤ width。
    let rich = html2text::from_read_rich(text.as_bytes(), width.max(1) as usize)
        .map_err(|e| format!("render failed: {e}"))?;
    let rows: Vec<Vec<Piece>> = rich
        .iter()
        .map(|line| pieces_from_line(line, &got.final_url))
        .collect();
    let (lines, links) = layout(rows, width.max(1) as usize);
    Ok(WebDoc {
        title,
        final_url: got.final_url,
        lines,
        links,
    })
}

/// 是否网页 URL(http/https)。
pub fn is_web_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// 本地 html 文件的 file:// URL(base 解析用)。
pub fn file_url(path: &Path) -> String {
    format!("file://{}", path.display())
}

// ---------------------------------------------------------------------------
// 抓取 / 读取
// ---------------------------------------------------------------------------

/// 抓取或读取结果。
struct Fetched {
    bytes: Vec<u8>,
    /// `Content-Type; charset=` 声明的字符集(本地文件为 None → 靠 `<meta>` 嗅探)。
    charset: Option<String>,
    /// 最终 URL(重定向后);同时作为相对链接的 base。
    final_url: String,
}

/// http(s) 抓取(rustls;跟随重定向;16MB 上限;UA= dlook/<version>)。
fn fetch_url(url: &str) -> Result<Fetched, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(FETCH_TIMEOUT)
        .redirects(MAX_REDIRECTS)
        .user_agent(USER_AGENT)
        .build();
    let resp = match agent.get(url).call() {
        Ok(r) => r,
        // ureq 把 4xx/5xx 作 Err(Status) 返回;文案与项目状态栏风格一致("http 404")。
        Err(ureq::Error::Status(code, _)) => return Err(format!("http {code}")),
        Err(e) => return Err(format!("fetch failed: {e}")),
    };
    let status = resp.status();
    if !(200..300).contains(&status) {
        return Err(format!("http {status}"));
    }
    let ctype = resp.header("content-type").unwrap_or("").to_string();
    let mime = ctype
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if !mime.is_empty() && !is_html_mime(&mime) {
        return Err(format!("not a web page: {mime}"));
    }
    if let Some(len) = resp
        .header("content-length")
        .and_then(|v| v.trim().parse::<u64>().ok())
    {
        if len > MAX_BYTES {
            return Err(too_large_error());
        }
    }
    let charset = charset_param(&ctype);
    let final_url = resp.get_url().to_string();
    let mut bytes = Vec::with_capacity(64 * 1024);
    resp.into_reader()
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("read failed: {e}"))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(too_large_error());
    }
    if bytes.is_empty() {
        return Err("empty response".into());
    }
    Ok(Fetched {
        bytes,
        charset,
        final_url,
    })
}

/// 本地 `.html/.htm` 读取(上限同样 16MB;base = 文件的绝对 file:// URL)。
fn read_local(path_str: &str) -> Result<Fetched, String> {
    let path = Path::new(path_str);
    let meta = std::fs::metadata(path).map_err(|_| format!("not found: {path_str}"))?;
    if meta.is_dir() {
        return Err(format!("is a directory: {path_str}"));
    }
    if !has_html_ext(path_str) {
        return Err(format!("not a web page: {path_str}"));
    }
    if meta.len() > MAX_BYTES {
        return Err(too_large_error());
    }
    let bytes = std::fs::read(path).map_err(|_| format!("unreadable: {path_str}"))?;
    if bytes.is_empty() {
        return Err(format!("empty file: {path_str}"));
    }
    // 绝对化:相对链接按 base 解析后必须是绝对 file://,点击时 links::classify 才能
    // 得到正确的本地路径(Local)。
    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    Ok(Fetched {
        bytes,
        charset: None,
        final_url: file_url(&abs),
    })
}

/// 是否可当网页渲染的 MIME(text/* 与 xhtml/xml;空头按 HTML 处理)。
fn is_html_mime(mime: &str) -> bool {
    mime.starts_with("text/")
        || mime == "application/xhtml+xml"
        || mime == "application/xml"
        || mime == "application/xhtml"
}

fn has_html_ext(path: &str) -> bool {
    let base = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    match base.rfind('.') {
        Some(dot) => matches!(&base[dot + 1..], "html" | "htm"),
        None => false,
    }
}

fn too_large_error() -> String {
    format!("too large (>{}MB)", MAX_BYTES / 1024 / 1024)
}

/// 从 `Content-Type` 提取 `charset=`(去掉引号)。
fn charset_param(ctype: &str) -> Option<String> {
    let lower = ctype.to_ascii_lowercase();
    let idx = lower.find("charset")? + "charset".len();
    let rest = &ctype[idx..];
    let rest = rest.trim_start().strip_prefix('=')?.trim_start();
    let val: String = rest
        .trim_start_matches(['"', '\''])
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect();
    if val.is_empty() {
        None
    } else {
        Some(val)
    }
}

// ---------------------------------------------------------------------------
// 字符集:声明 → 嗅探 → 解码
// ---------------------------------------------------------------------------

/// 字节 → String。优先级:UTF-8/UTF-16 BOM → `Content-Type; charset=` →
/// `<meta charset>` 嗅探 → UTF-8(lossy)。任何情况下不 panic。
fn decode_bytes(bytes: &[u8], declared: Option<&str>) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return decode_utf16(&bytes[2..], true);
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return decode_utf16(&bytes[2..], false);
    }
    let label = declared
        .map(normalize_label)
        .filter(|s| !s.is_empty())
        .or_else(|| sniff_charset(bytes))
        .unwrap_or_else(|| "utf-8".to_string());
    match label.as_str() {
        "utf-8" | "utf8" | "us-ascii" | "ascii" | "unicode-1-1-utf-8" => {
            String::from_utf8_lossy(bytes).into_owned()
        }
        "gb2312" | "gbk" | "gb18030" | "gb-2312" | "x-gbk" | "cp936" | "ms936" | "csgb2312" => {
            decode_gb18030(bytes)
        }
        "iso-8859-1" | "iso8859-1" | "latin1" | "latin-1" | "l1" | "cp819" | "ascii-8bit" => {
            decode_latin1(bytes)
        }
        "windows-1252" | "cp1252" | "x-cp1252" => decode_cp1252(bytes),
        "utf-16" => decode_utf16(bytes, true),
        "utf-16le" => decode_utf16(bytes, true),
        "utf-16be" => decode_utf16(bytes, false),
        // 未识别/未实现的字符集(Big5/Shift_JIS/EUC-KR…):lossy 兜底,不 panic。
        // TODO(dep): encoding_rs 落地后这里全部交给 Encoding::for_label。
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// 字符集标签规范化:小写、去引号与空白。
fn normalize_label(raw: &str) -> String {
    raw.trim()
        .trim_matches(['"', '\''])
        .trim()
        .to_ascii_lowercase()
}

/// 在文档头里嗅探 `<meta charset=…>` / `<meta http-equiv … content="…charset=…">`。
fn sniff_charset(bytes: &[u8]) -> Option<String> {
    let head = &bytes[..bytes.len().min(META_SNIFF_BYTES)];
    let text = String::from_utf8_lossy(head);
    let lower = text.to_ascii_lowercase();
    let mut from = 0usize;
    while let Some(pos) = lower[from..].find("<meta") {
        let start = from + pos;
        let end = match lower[start..].find('>') {
            Some(e) => start + e,
            None => break,
        };
        let tag = &lower[start..end];
        if let Some(cs) = charset_param(tag) {
            return Some(cs);
        }
        from = end;
    }
    None
}

fn decode_latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

/// windows-1252 = latin1 + 0x80–0x9F 的 27 个可见字符(cp1252 私有区)。
fn decode_cp1252(bytes: &[u8]) -> String {
    const HIGH: [char; 32] = [
        '\u{20AC}', '\u{0081}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}',
        '\u{2021}', '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{008D}',
        '\u{017D}', '\u{008F}', '\u{0090}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}',
        '\u{2022}', '\u{2013}', '\u{2014}', '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}',
        '\u{0153}', '\u{009D}', '\u{017E}', '\u{0178}',
    ];
    bytes
        .iter()
        .map(|&b| match b {
            0x80..=0x9F => HIGH[(b - 0x80) as usize],
            _ => b as char,
        })
        .collect()
}

/// UTF-16 → String(BOM 已在外层剥掉;孤立代理 → U+FFFD)。
fn decode_utf16(bytes: &[u8], little_endian: bool) -> String {
    let unit = |i: usize| -> u16 {
        let (a, b) = (bytes[i], bytes[i + 1]);
        if little_endian {
            u16::from_le_bytes([a, b])
        } else {
            u16::from_be_bytes([a, b])
        }
    };
    let mut out = String::with_capacity(bytes.len() / 2);
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        let u = unit(i);
        i += 2;
        match u {
            0xD800..=0xDBFF if i + 1 < bytes.len() => {
                let lo = unit(i);
                if (0xDC00..=0xDFFF).contains(&lo) {
                    i += 2;
                    let cp = 0x10000u32 + ((u as u32 - 0xD800) << 10) + (lo as u32 - 0xDC00);
                    out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                } else {
                    out.push('\u{FFFD}');
                }
            }
            0xD800..=0xDFFF => out.push('\u{FFFD}'),
            _ => out.push(char::from_u32(u as u32).unwrap_or('\u{FFFD}')),
        }
    }
    if bytes.len() % 2 == 1 {
        out.push('\u{FFFD}');
    }
    out
}

// ---------------------------------------------------------------------------
// 标题
// ---------------------------------------------------------------------------

/// `<title>…</title>`(大小写不敏感、跨行、实体解码);缺失返回 None。
fn page_title(html: &str) -> Option<String> {
    let head = &html[..floor_char_boundary(html, html.len().min(TITLE_SCAN_BYTES))];
    let lower = head.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let open_end = start + lower[start..].find('>')? + 1;
    let close = lower[open_end..].find("</title")? + open_end;
    let raw = &head[open_end..close];
    let text = decode_entities(raw)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// 标题缺失时的回退:URL/文件路径的最后一段(去掉 query/fragment)。
fn fallback_title(url: &str) -> String {
    let no_suffix = url.split(['?', '#']).next().unwrap_or(url);
    let trimmed = no_suffix.trim_end_matches('/');
    let last = trimmed.rsplit('/').next().unwrap_or(trimmed);
    if last.is_empty() {
        no_suffix.to_string()
    } else {
        last.to_string()
    }
}

/// 最小 HTML 实体解码(标题用;正文实体由 html2text/html5ever 处理)。
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < s.len() {
        if bytes[i] != b'&' {
            let ch = s[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        let rest = &s[i..];
        let semi = match rest.find(';') {
            Some(p) if p <= 10 => p,
            _ => {
                out.push('&');
                i += 1;
                continue;
            }
        };
        let ent = &rest[1..semi];
        let decoded = match ent.to_ascii_lowercase().as_str() {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            _ => ent
                .strip_prefix('#')
                .and_then(|n| match n.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => n.parse::<u32>().ok(),
                })
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                i += semi + 1;
            }
            None => {
                out.push('&');
                i += 1;
            }
        }
    }
    out
}

/// 就近的 char 边界(避免在多字节字符中间切片 panic)。
fn floor_char_boundary(s: &str, mut idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

// ---------------------------------------------------------------------------
// 渲染:html2text 注解 → 行模型 + 链接区
// ---------------------------------------------------------------------------

/// 渲染中间结构:一个「片段」(同一样式、同一链接目标的连续文本)。
///
/// 为什么不把注解直接映射为 `Span`(任务要求消融实验 A,实测结论):
/// ① 链接区坐标必须在**按 width 折行之后**的最终行集上计算:追加的 ` (url)` 后缀会把
///    行挤宽(必须折行),而折行后 label 与后缀可能落在不同行;② 一旦只保留渲染后的
///    文本,label 起点就无从得知——消融版(直出 Span + 按 `(url)` 后缀做字符串扫描)
///    实测把 `a link (url)` 的链接区起点定在 `link`(丢了 `a `),
///    `every_line_fits_width` 与 `local_html_renders_title_paragraphs_and_absolute_link`
///    双双失败(见任务报告「消融实验」)。保留 Piece 即保留「链接来源 + 样式」,
///    折行时按来源记账,坐标不靠反解。
#[derive(Debug, Clone)]
struct Piece {
    text: String,
    style: Style,
    /// 可点击目标(已解析为绝对 URL);None = 普通文本。
    link: Option<String>,
}

/// 一行 html2text 输出 → 片段序列(链接绝对化、图片占位、样式映射)。
fn pieces_from_line(line: &TaggedLine<Vec<RichAnnotation>>, base_url: &str) -> Vec<Piece> {
    let mut out: Vec<Piece> = Vec::new();
    for ts in line.tagged_strings() {
        if ts.s.is_empty() {
            continue;
        }
        let tags = ts.tag.as_slice();
        // 图片:只出占位文本,**绝不请求**,也不建 LinkSpan(防追踪像素)。
        if tags
            .iter()
            .any(|t| matches!(t, RichAnnotation::Image(_)))
        {
            let alt = ts.s.trim();
            let text = if alt.is_empty() {
                "🖼".to_string()
            } else {
                format!("🖼 {alt}")
            };
            push_piece(&mut out, text, image_style(), None);
            continue;
        }
        let style = text_style(tags);
        match tags.iter().find_map(|t| match t {
            RichAnnotation::Link(u) => Some(u.as_str()),
            _ => None,
        }) {
            Some(raw) => {
                let target = resolve_url(base_url, raw);
                if is_unsafe_target(&target) {
                    // javascript:/data: 等:只留文本,不可点
                    push_piece(&mut out, ts.s.clone(), style, None);
                    continue;
                }
                let is_bare_url = ts.s.trim() == target;
                push_piece(&mut out, ts.s.clone(), link_label_style(), Some(target.clone()));
                if !is_bare_url {
                    // 与 markdown 观感一致:label 后跟暗灰 " (url)"(同属可点区域)
                    push_piece(
                        &mut out,
                        format!(" ({target})"),
                        link_url_style(),
                        Some(target),
                    );
                }
            }
            None => push_piece(&mut out, ts.s.clone(), style, None),
        }
    }
    out
}

fn push_piece(out: &mut Vec<Piece>, text: String, style: Style, link: Option<String>) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = out.last_mut() {
        if last.style == style && last.link == link {
            last.text.push_str(&text);
            return;
        }
    }
    out.push(Piece { text, style, link });
}

fn link_label_style() -> Style {
    Style::default()
        .fg(LINK_LABEL_FG)
        .add_modifier(Modifier::UNDERLINED)
}

fn link_url_style() -> Style {
    Style::default().fg(LINK_URL_FG)
}

fn image_style() -> Style {
    Style::default()
        .fg(IMAGE_FG)
        .add_modifier(Modifier::DIM)
}

/// 注解 → 样式。页面自带配色(Colour/BgColour)忽略:终端主题优先。
fn text_style(tags: &[RichAnnotation]) -> Style {
    let mut style = Style::default();
    for t in tags {
        style = match t {
            RichAnnotation::Strong => style.add_modifier(Modifier::BOLD),
            RichAnnotation::Emphasis => style.add_modifier(Modifier::ITALIC),
            RichAnnotation::Strikeout => style.add_modifier(Modifier::CROSSED_OUT),
            RichAnnotation::Code => style.fg(INLINE_CODE_FG),
            // 代码块/预格式:单一 dim 样式(MVP 不做语法高亮)
            RichAnnotation::Preformat(_) => style.add_modifier(Modifier::DIM),
            _ => style,
        };
    }
    style
}

/// `javascript:`/`data:`/`vbscript:` 目标不建链接(交浏览器打开它们无意义且有风险)。
fn is_unsafe_target(url: &str) -> bool {
    let lower = url.trim_start().to_ascii_lowercase();
    lower.starts_with("javascript:")
        || lower.starts_with("data:")
        || lower.starts_with("vbscript:")
}

/// 按 base(最终 URL)把相对链接解析为绝对 URL(RFC 3986 常用子集:
/// scheme/authority/path/query/fragment;不依赖 url crate)。
fn resolve_url(base: &str, rel: &str) -> String {
    let rel = rel.trim();
    if rel.is_empty() {
        return base.to_string();
    }
    if has_scheme(rel) {
        return rel.to_string();
    }
    let base_main = base.split('#').next().unwrap_or(base);
    if let Some(frag) = rel.strip_prefix('#') {
        return format!("{base_main}#{frag}");
    }
    if let Some(rest) = rel.strip_prefix("//") {
        let scheme = scheme_of(base).unwrap_or("http");
        return format!("{scheme}://{rest}");
    }
    let base_no_query = base_main.split('?').next().unwrap_or(base_main);
    if let Some(q) = rel.strip_prefix('?') {
        return format!("{base_no_query}?{q}");
    }
    let origin = origin_of(base_main);
    let path = path_of(base_main);
    if rel.starts_with('/') {
        return format!("{origin}{}", remove_dot_segments(rel));
    }
    let dir = match path.rfind('/') {
        Some(i) => &path[..=i],
        None => "/",
    };
    let joined = remove_dot_segments(&format!("{dir}{rel}"));
    format!("{origin}{joined}")
}

/// `scheme:` 前缀判定(首字符字母,scheme 体 [A-Za-z0-9+.-],后跟 `:`)。
fn has_scheme(s: &str) -> bool {
    let b = s.as_bytes();
    let Some(colon) = b.iter().position(|&c| c == b':') else {
        return false;
    };
    colon > 0
        && b[0].is_ascii_alphabetic()
        && b[1..colon]
            .iter()
            .all(|&c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.'))
}

fn scheme_of(url: &str) -> Option<&str> {
    let idx = url.find("://")?;
    Some(&url[..idx])
}

/// `scheme://authority`(file:///a/b → `file://`)。
fn origin_of(url: &str) -> &str {
    match url.find("://") {
        Some(i) => match url[i + 3..].find('/') {
            Some(slash) => &url[..i + 3 + slash],
            None => url,
        },
        None => "",
    }
}

/// origin 之后的路径部分(无则 `/`)。
fn path_of(url: &str) -> &str {
    let origin = origin_of(url);
    let rest = &url[origin.len()..];
    if rest.starts_with('/') {
        rest
    } else {
        "/"
    }
}

/// 折叠 `.`/`..`(保留绝对路径与结尾斜杠语义)。
fn remove_dot_segments(path: &str) -> String {
    let absolute = path.starts_with('/');
    let trailing = path.ends_with('/') || path.ends_with("/.") || path.ends_with("/..");
    let mut segs: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                segs.pop();
            }
            s => segs.push(s),
        }
    }
    let mut out = String::new();
    if absolute {
        out.push('/');
    }
    out.push_str(&segs.join("/"));
    if trailing && !out.ends_with('/') {
        out.push('/');
    }
    out
}

// ---------------------------------------------------------------------------
// 铺行:按 width 折行 + 计算链接列(字符列,与 markdown.rs 的 LinkSpan 语义一致)
// ---------------------------------------------------------------------------

/// 一行 html2text 输出在铺行过程中的累积状态。
#[derive(Default)]
struct RowBuf {
    spans: Vec<Span<'static>>,
    /// 行内链接区(字符列);收行时落成 `LinkSpan`。
    row_links: Vec<(usize, usize, String)>,
    /// cell 宽度:决定折行(中文/全角占 2 列)。
    col_cells: usize,
    /// 字符列:决定 LinkSpan 坐标(与 markdown.rs 的 LinkSpan 语义一致:字符列)。
    col_chars: usize,
    /// 行内已出现、尚未落行的空白。折行时丢弃(与 html2text 自身的折行一致:
    /// 折行处的空白是分隔符,不进行尾/列首);行末放得下则保留(表格补白/pre 缩进)。
    pending: Option<Piece>,
}

impl RowBuf {
    fn pending_cells(&self) -> usize {
        self.pending
            .as_ref()
            .map_or(0, |p| p.text.chars().map(char_cells).sum())
    }

    /// 落一段文本(调用方保证不会超出 width;`push_text` 负责超长词的硬切)。
    fn emit(&mut self, text: &str, style: Style, link: Option<&str>) {
        if text.is_empty() {
            return;
        }
        let start = self.col_chars;
        self.col_cells += text.chars().map(char_cells).sum::<usize>();
        self.col_chars += text.chars().count();
        self.spans.push(Span::styled(text.to_string(), style));
        if let Some(target) = link {
            match self.row_links.last_mut() {
                Some((_, end, prev)) if prev == target && *end == start => *end = self.col_chars,
                _ => {
                    self.row_links
                        .push((start, self.col_chars, target.to_string()))
                }
            }
        }
    }

    /// 落一段可能超宽文本(超长「词」/行首缩进):按 cell 硬切,保证每行 ≤ width。
    fn push_text(
        &mut self,
        text: &str,
        style: Style,
        link: Option<&str>,
        width: usize,
        lines: &mut Vec<Line<'static>>,
        all_links: &mut Vec<LinkSpan>,
    ) {
        let mut rest: &str = text;
        while !rest.is_empty() {
            if self.col_cells >= width {
                self.flush(lines, all_links, width);
            }
            // 宽字符边界:整字放不下就收行(行内已有内容,不会死循环);
            // 行内为空时仍取该字符,保证推进(width 小于单字宽度的退化情形)。
            let first = rest.chars().next().map_or(0, char_cells);
            if self.col_cells > 0 && self.col_cells + first > width {
                self.flush(lines, all_links, width);
                continue;
            }
            let (chunk, tail) = split_cells(rest, width - self.col_cells);
            if chunk.is_empty() {
                break; // 兜底:不应发生(split_cells 至少取 1 字符)
            }
            self.emit(chunk, style, link);
            rest = tail;
        }
    }

    /// 收行:行号 = 当前 lines 长度(LinkSpan.line 语义与 markdown 一致:最终行集索引)。
    /// 行末的待落空白:放得下才保留(避免任何情况下超出 width)。
    fn flush(
        &mut self,
        lines: &mut Vec<Line<'static>>,
        all_links: &mut Vec<LinkSpan>,
        width: usize,
    ) {
        if let Some(ws) = self.pending.take() {
            if self.col_cells + cells_of(&ws.text) <= width {
                self.emit(&ws.text, ws.style, ws.link.as_deref());
            }
        }
        let line = lines.len();
        for (start, end, target) in self.row_links.drain(..) {
            if end > start {
                all_links.push(LinkSpan {
                    line,
                    start,
                    end,
                    target,
                });
            }
        }
        lines.push(Line::default().spans(std::mem::take(&mut self.spans)));
        self.col_cells = 0;
        self.col_chars = 0;
    }

    /// 折行:同 `flush`,但待落空白随折行丢弃(折行处空白 = 分隔符)。
    fn wrap(
        &mut self,
        width: usize,
        lines: &mut Vec<Line<'static>>,
        all_links: &mut Vec<LinkSpan>,
    ) {
        self.pending = None;
        self.flush(lines, all_links, width);
    }
}

/// 把一个 html2text 输出行(片段序列)铺成若干 doc 行 + 链接区。
///
/// 折行以**整词**为单位(与 html2text 自身的折行策略一致):只有单个「词」
/// (无空白的连续文本,如超长 URL、CJK 段落)本身超过 width 时才按 cell 硬切。
/// 这样追加的 ` (url)` 后缀把行挤宽时,不会把 `here.` 切成 `he`+`re.`。
/// 每个 html2text 行独立处理:保留段落/块级结构,只在超宽时继续折行。
fn layout(rows: Vec<Vec<Piece>>, width: usize) -> (Vec<Line<'static>>, Vec<LinkSpan>) {
    let width = width.max(1);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut all_links: Vec<LinkSpan> = Vec::new();
    for pieces in rows {
        let mut rb = RowBuf::default();
        for p in pieces {
            let style = p.style;
            let link = p.link.as_deref();
            let mut rest: &str = &p.text;
            while !rest.is_empty() {
                let (atom, tail) = split_atom(rest);
                rest = tail;
                if atom.starts_with(is_break_space) {
                    if rb.col_cells == 0 {
                        // 行首空白(缩进/表格补白):照原样落行
                        rb.push_text(atom, style, link, width, &mut lines, &mut all_links);
                    } else {
                        match rb.pending.as_mut() {
                            Some(prev) => prev.text.push_str(atom),
                            None => {
                                rb.pending = Some(Piece {
                                    text: atom.to_string(),
                                    style,
                                    link: p.link.clone(),
                                })
                            }
                        }
                    }
                    continue;
                }
                // 整词:先看「待落空白 + 词」能否整体放下,放不下则提前收行。
                let word_cells = cells_of(atom);
                if rb.col_cells > 0 && rb.col_cells + rb.pending_cells() + word_cells > width {
                    rb.wrap(width, &mut lines, &mut all_links);
                }
                if let Some(ws) = rb.pending.take() {
                    rb.emit(&ws.text, ws.style, ws.link.as_deref());
                }
                rb.push_text(atom, style, link, width, &mut lines, &mut all_links);
            }
        }
        rb.flush(&mut lines, &mut all_links, width);
    }
    (lines, all_links)
}

/// 取行首「原子」:一段空白或一个词(无空白的连续文本)。
/// 折行以词为单位,故词内部不做断点(超长词由 `push_text` 兜底硬切)。
fn split_atom(s: &str) -> (&str, &str) {
    let ws = s.starts_with(is_break_space);
    let mut end = s.len();
    for (i, ch) in s.char_indices() {
        if is_break_space(ch) != ws {
            end = i;
            break;
        }
    }
    s.split_at(end)
}

/// 可作折行断点的空白。U+00A0(NBSP,`&nbsp;`)按语义**不可**断行,故不算断点。
fn is_break_space(c: char) -> bool {
    c.is_whitespace() && c != '\u{a0}'
}

/// 字符串的 cell 宽度(中文/全角/emoji 占 2 列)。
fn cells_of(s: &str) -> usize {
    s.chars().map(char_cells).sum()
}

/// 取不超过 `room` cell 宽度的前缀(至少 1 个字符,避免零宽字符造成死循环)。
fn split_cells(s: &str, room: usize) -> (&str, &str) {
    let mut width = 0usize;
    let mut cut = 0usize;
    for (i, ch) in s.char_indices() {
        let cw = char_cells(ch);
        if cut > 0 && width + cw > room {
            break;
        }
        width += cw;
        cut = i + ch.len_utf8();
        if width >= room {
            break;
        }
    }
    s.split_at(cut)
}

/// 字符显示宽度近似(East Asian Wide/Fullwidth 与 emoji = 2,组合记号 = 0)。
///
/// ansi_lines.rs 目前按「1 字符 = 1 cell」近似;网页里中文/全角很常见,这里保留
/// 宽字符判定,否则 CJK 行会渲染超宽。TODO(style): 项目统一后换成 unicode-width。
fn char_cells(c: char) -> usize {
    match c as u32 {
        0x0300..=0x036F | 0x200B..=0x200F | 0xFE00..=0xFE0F | 0xFE20..=0xFE2F => 0,
        0x1100..=0x115F
        | 0x2E80..=0x303E
        | 0x3041..=0x33FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xA000..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE6F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1F64F
        | 0x1F680..=0x1F6FF
        | 0x1F900..=0x1F9FF
        | 0x20000..=0x3FFFD => 2,
        _ => 1,
    }
}

/// 整行 cell 宽度(单测断言用)。
#[cfg(test)]
fn line_cells(line: &Line<'_>) -> usize {
    line.spans.iter().map(|s| s.content.chars().map(char_cells).sum::<usize>()).sum()
}

// ---------------------------------------------------------------------------
// GB18030 解码(临时自实现,见文件头 TODO(dep))
// ---------------------------------------------------------------------------

/// GBK/GB18030 解码:双字节平面走 `GBK_ROWS`,四字节平面走线性区间表,
/// 非法字节/序列输出 U+FFFD 并按字节重新同步(不 panic、不丢后续文本)。
fn decode_gb18030(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b < 0x80 {
            out.push(b as char);
            i += 1;
            continue;
        }
        if b == 0x80 {
            out.push('\u{20AC}'); // GBK:0x80 = €
            i += 1;
            continue;
        }
        if b == 0xFF {
            out.push('\u{FFFD}');
            i += 1;
            continue;
        }
        let mut consumed = 1usize;
        let mut decoded: Option<char> = None;
        if let Some(&b2) = bytes.get(i + 1) {
            if (0x40..=0x7E).contains(&b2) || (0x80..=0xFE).contains(&b2) {
                let idx = (b2 as usize) - 0x40 - usize::from(b2 > 0x7F);
                decoded = gbk_lookup(b, idx);
                consumed = 2;
            } else if (0x30..=0x39).contains(&b2) {
                if let (Some(&b3), Some(&b4)) = (bytes.get(i + 2), bytes.get(i + 3)) {
                    if (0x81..=0xFE).contains(&b3) && (0x30..=0x39).contains(&b4) {
                        let p = (u32::from(b) - 0x81) * 10 * 1260
                            + (u32::from(b2) - 0x30) * 1260
                            + (u32::from(b3) - 0x81) * 10
                            + (u32::from(b4) - 0x30);
                        decoded = gb18030_4byte_lookup(p);
                        consumed = 4;
                    }
                }
            }
        }
        out.push(decoded.unwrap_or('\u{FFFD}'));
        i += consumed;
    }
    out
}

/// 双字节查表(lead ∈ 0x81..=0xFE,`idx` = trail 在行内的下标 0..190)。
fn gbk_lookup(lead: u8, trail_idx: usize) -> Option<char> {
    let row = GBK_ROWS[(lead - 0x81) as usize];
    if row.is_empty() {
        return None;
    }
    match row.chars().nth(trail_idx) {
        Some('~') | None => None,
        Some(c) => Some(c),
    }
}

/// 四字节查表:二分定位 `p` 所属区间,`p` 落在区间外(未分配指针)→ None。
fn gb18030_4byte_lookup(p: u32) -> Option<char> {
    let mut lo = 0usize;
    let mut hi = GB18030_4BYTE_RANGES.len();
    let mut found: Option<(u32, u32, u32)> = None;
    while lo < hi {
        let mid = (lo + hi) / 2;
        let (ps, cs, len) = GB18030_4BYTE_RANGES[mid];
        if p < ps {
            hi = mid;
        } else {
            found = Some((ps, cs, len));
            lo = mid + 1;
        }
    }
    let (ps, cs, len) = found?;
    if p >= ps + len {
        return None;
    }
    char::from_u32(cs + (p - ps))
}

/// GBK/GB18030 双字节平面(lead 0x81–0xFE × trail 190 个),按 `lead - 0x81` 索引。
///
/// 每行 190 个字符(trail 0x40–0x7E + 0x80–0xFE)。GB18030 为该平面全部 23,940 个码位
/// 都定义了映射(规范里 GBK 未分配的码位落到 PUA),故现表没有 `~` 占位;`~` 仍按
/// 「未分配 → U+FFFD」处理,以防后续改表。生成数据已逐序列对 encoding_rs 0.8 校验:
/// 23,940 个双字节序列 + 1,087,996 个四字节指针,0 差异(见 `gb18030_table_shape_is_intact`)。
/// 见文件头 TODO:主 agent 批准 `encoding_rs` 依赖后应整体删除本表。
#[rustfmt::skip]
const GBK_ROWS: [&str; 126] = [
    "丂丄丅丆丏丒丗丟丠両丣並丩丮丯丱丳丵丷丼乀乁乂乄乆乊乑乕乗乚乛乢乣乤乥乧乨乪乫乬乭乮乯乲乴乵乶乷乸乹乺乻乼乽乿亀亁亂亃亄亅亇亊亐亖亗亙亜亝亞亣亪亯亰亱亴亶亷亸亹亼亽亾仈仌仏仐仒仚仛仜仠仢仦仧仩仭仮仯仱仴仸仹仺仼仾伀伂伃伄伅伆伇伈伋伌伒伓伔伕伖伜伝伡伣伨伩伬伭伮伱伳伵伷伹伻伾伿佀佁佂佄佅佇佈佉佊佋佌佒佔佖佡佢佦佨佪佫佭佮佱佲併佷佸佹佺佽侀侁侂侅來侇侊侌侎侐侒侓侕侖侘侙侚侜侞侟価侢",
    "侤侫侭侰侱侲侳侴侶侷侸侹侺侻侼侽侾俀俁係俆俇俈俉俋俌俍俒俓俔俕俖俙俛俠俢俤俥俧俫俬俰俲俴俵俶俷俹俻俼俽俿倀倁倂倃倄倅倆倇倈倉倊個倎倐們倓倕倖倗倛倝倞倠倢倣値倧倫倯倰倱倲倳倴倵倶倷倸倹倻倽倿偀偁偂偄偅偆偉偊偋偍偐偑偒偓偔偖偗偘偙偛偝偞偟偠偡偢偣偤偦偧偨偩偪偫偭偮偯偰偱偲偳側偵偸偹偺偼偽傁傂傃傄傆傇傉傊傋傌傎傏傐傑傒傓傔傕傖傗傘備傚傛傜傝傞傟傠傡傢傤傦傪傫傭傮傯傰傱傳傴債傶傷傸傹傼",
    "傽傾傿僀僁僂僃僄僅僆僇僈僉僊僋僌働僎僐僑僒僓僔僕僗僘僙僛僜僝僞僟僠僡僢僣僤僥僨僩僪僫僯僰僱僲僴僶僷僸價僺僼僽僾僿儀儁儂儃億儅儈儉儊儌儍儎儏儐儑儓儔儕儖儗儘儙儚儛儜儝儞償儠儢儣儤儥儦儧儨儩優儫儬儭儮儯儰儱儲儳儴儵儶儷儸儹儺儻儼儽儾兂兇兊兌兎兏児兒兓兗兘兙兛兝兞兟兠兡兣兤兦內兩兪兯兲兺兾兿冃冄円冇冊冋冎冏冐冑冓冔冘冚冝冞冟冡冣冦冧冨冩冪冭冮冴冸冹冺冾冿凁凂凃凅凈凊凍凎凐凒凓凔凕凖凗",
    "凘凙凚凜凞凟凢凣凥処凧凨凩凪凬凮凱凲凴凷凾刄刅刉刋刌刏刐刓刔刕刜刞刟刡刢刣別刦刧刪刬刯刱刲刴刵刼刾剄剅剆則剈剉剋剎剏剒剓剕剗剘剙剚剛剝剟剠剢剣剤剦剨剫剬剭剮剰剱剳剴創剶剷剸剹剺剻剼剾劀劃劄劅劆劇劉劊劋劌劍劎劏劑劒劔劕劖劗劘劙劚劜劤劥劦劧劮劯劰労劵劶劷劸効劺劻劼劽勀勁勂勄勅勆勈勊勌勍勎勏勑勓勔動勗務勚勛勜勝勞勠勡勢勣勥勦勧勨勩勪勫勬勭勮勯勱勲勳勴勵勶勷勸勻勼勽匁匂匃匄匇匉匊匋匌匎",
    "匑匒匓匔匘匛匜匞匟匢匤匥匧匨匩匫匬匭匯匰匱匲匳匴匵匶匷匸匼匽區卂卄卆卋卌卍卐協単卙卛卝卥卨卪卬卭卲卶卹卻卼卽卾厀厁厃厇厈厊厎厏厐厑厒厓厔厖厗厙厛厜厞厠厡厤厧厪厫厬厭厯厰厱厲厳厴厵厷厸厹厺厼厽厾叀參叄叅叆叇収叏叐叒叓叕叚叜叝叞叡叢叧叴叺叾叿吀吂吅吇吋吔吘吙吚吜吢吤吥吪吰吳吶吷吺吽吿呁呂呄呅呇呉呌呍呎呏呑呚呝呞呟呠呡呣呥呧呩呪呫呬呭呮呯呰呴呹呺呾呿咁咃咅咇咈咉咊咍咑咓咗咘咜咞咟咠咡",
    "咢咥咮咰咲咵咶咷咹咺咼咾哃哅哊哋哖哘哛哠員哢哣哤哫哬哯哰哱哴哵哶哷哸哹哻哾唀唂唃唄唅唈唊唋唌唍唎唒唓唕唖唗唘唙唚唜唝唞唟唡唥唦唨唩唫唭唲唴唵唶唸唹唺唻唽啀啂啅啇啈啋啌啍啎問啑啒啓啔啗啘啙啚啛啝啞啟啠啢啣啨啩啫啯啰啱啲啳啴啹啺啽啿喅喆喌喍喎喐喒喓喕喖喗喚喛喞喠喡喢喣喤喥喦喨喩喪喫喬喭單喯喰喲喴営喸喺喼喿嗀嗁嗂嗃嗆嗇嗈嗊嗋嗎嗏嗐嗕嗗嗘嗙嗚嗛嗞嗠嗢嗧嗩嗭嗮嗰嗱嗴嗶嗸嗹嗺嗻嗼嗿嘂嘃嘄嘅",
    "嘆嘇嘊嘋嘍嘐嘑嘒嘓嘔嘕嘖嘗嘙嘚嘜嘝嘠嘡嘢嘥嘦嘨嘩嘪嘫嘮嘯嘰嘳嘵嘷嘸嘺嘼嘽嘾噀噁噂噃噄噅噆噇噈噉噊噋噏噐噑噒噓噕噖噚噛噝噞噟噠噡噣噥噦噧噭噮噯噰噲噳噴噵噷噸噹噺噽噾噿嚀嚁嚂嚃嚄嚇嚈嚉嚊嚋嚌嚍嚐嚑嚒嚔嚕嚖嚗嚘嚙嚚嚛嚜嚝嚞嚟嚠嚡嚢嚤嚥嚦嚧嚨嚩嚪嚫嚬嚭嚮嚰嚱嚲嚳嚴嚵嚶嚸嚹嚺嚻嚽嚾嚿囀囁囂囃囄囅囆囇囈囉囋囌囍囎囏囐囑囒囓囕囖囘囙囜団囥囦囧囨囩囪囬囮囯囲図囶囷囸囻囼圀圁圂圅圇國圌圍圎圏圐圑",
    "園圓圔圕圖圗團圙圚圛圝圞圠圡圢圤圥圦圧圫圱圲圴圵圶圷圸圼圽圿坁坃坄坅坆坈坉坋坒坓坔坕坖坘坙坢坣坥坧坬坮坰坱坲坴坵坸坹坺坽坾坿垀垁垇垈垉垊垍垎垏垐垑垔垕垖垗垘垙垚垜垝垞垟垥垨垪垬垯垰垱垳垵垶垷垹垺垻垼垽垾垿埀埁埄埅埆埇埈埉埊埌埍埐埑埓埖埗埛埜埞埡埢埣埥埦埧埨埩埪埫埬埮埰埱埲埳埵埶執埻埼埾埿堁堃堄堅堈堉堊堌堎堏堐堒堓堔堖堗堘堚堛堜堝堟堢堣堥堦堧堨堩堫堬堭堮堯報堲堳場堶堷堸堹堺堻堼堽",
    "堾堿塀塁塂塃塅塆塇塈塉塊塋塎塏塐塒塓塕塖塗塙塚塛塜塝塟塠塡塢塣塤塦塧塨塩塪塭塮塯塰塱塲塳塴塵塶塷塸塹塺塻塼塽塿墂墄墆墇墈墊墋墌墍墎墏墐墑墔墕墖増墘墛墜墝墠墡墢墣墤墥墦墧墪墫墬墭墮墯墰墱墲墳墴墵墶墷墸墹墺墻墽墾墿壀壂壃壄壆壇壈壉壊壋壌壍壎壏壐壒壓壔壖壗壘壙壚壛壜壝壞壟壠壡壢壣壥壦壧壨壩壪壭壯壱売壴壵壷壸壺壻壼壽壾壿夀夁夃夅夆夈変夊夋夌夎夐夑夒夓夗夘夛夝夞夠夡夢夣夦夨夬夰夲夳夵夶夻",
    "夽夾夿奀奃奅奆奊奌奍奐奒奓奙奛奜奝奞奟奡奣奤奦奧奨奩奪奫奬奭奮奯奰奱奲奵奷奺奻奼奾奿妀妅妉妋妌妎妏妐妑妔妕妘妚妛妜妝妟妠妡妢妦妧妬妭妰妱妳妴妵妶妷妸妺妼妽妿姀姁姂姃姄姅姇姈姉姌姍姎姏姕姖姙姛姞姟姠姡姢姤姦姧姩姪姫姭姮姯姰姱姲姳姴姵姶姷姸姺姼姽姾娀娂娊娋娍娎娏娐娒娔娕娖娗娙娚娛娝娞娡娢娤娦娧娨娪娫娬娭娮娯娰娳娵娷娸娹娺娻娽娾娿婁婂婃婄婅婇婈婋婌婍婎婏婐婑婒婓婔婖婗婘婙婛婜婝婞婟婠",
    "婡婣婤婥婦婨婩婫婬婭婮婯婰婱婲婳婸婹婻婼婽婾媀媁媂媃媄媅媆媇媈媉媊媋媌媍媎媏媐媑媓媔媕媖媗媘媙媜媝媞媟媠媡媢媣媤媥媦媧媨媩媫媬媭媮媯媰媱媴媶媷媹媺媻媼媽媿嫀嫃嫄嫅嫆嫇嫈嫊嫋嫍嫎嫏嫐嫑嫓嫕嫗嫙嫚嫛嫝嫞嫟嫢嫤嫥嫧嫨嫪嫬嫭嫮嫯嫰嫲嫳嫴嫵嫶嫷嫸嫹嫺嫻嫼嫽嫾嫿嬀嬁嬂嬃嬄嬅嬆嬇嬈嬊嬋嬌嬍嬎嬏嬐嬑嬒嬓嬔嬕嬘嬙嬚嬛嬜嬝嬞嬟嬠嬡嬢嬣嬤嬥嬦嬧嬨嬩嬪嬫嬬嬭嬮嬯嬰嬱嬳嬵嬶嬸嬹嬺嬻嬼嬽嬾嬿孁孂孃孄孅孆孇",
    "孈孉孊孋孌孍孎孏孒孖孞孠孡孧孨孫孭孮孯孲孴孶孷學孹孻孼孾孿宂宆宊宍宎宐宑宒宔宖実宧宨宩宬宭宮宯宱宲宷宺宻宼寀寁寃寈寉寊寋寍寎寏寑寔寕寖寗寘寙寚寛寜寠寢寣實寧審寪寫寬寭寯寱寲寳寴寵寶寷寽対尀専尃尅將專尋尌對導尐尒尓尗尙尛尞尟尠尡尣尦尨尩尪尫尭尮尯尰尲尳尵尶尷屃屄屆屇屌屍屒屓屔屖屗屘屚屛屜屝屟屢層屧屨屩屪屫屬屭屰屲屳屴屵屶屷屸屻屼屽屾岀岃岄岅岆岇岉岊岋岎岏岒岓岕岝岞岟岠岡岤岥岦岧岨",
    "岪岮岯岰岲岴岶岹岺岻岼岾峀峂峃峅峆峇峈峉峊峌峍峎峏峐峑峓峔峕峖峗峘峚峛峜峝峞峟峠峢峣峧峩峫峬峮峯峱峲峳峴峵島峷峸峹峺峼峽峾峿崀崁崄崅崈崉崊崋崌崍崏崐崑崒崓崕崗崘崙崚崜崝崟崠崡崢崣崥崨崪崫崬崯崰崱崲崳崵崶崷崸崹崺崻崼崿嵀嵁嵂嵃嵄嵅嵆嵈嵉嵍嵎嵏嵐嵑嵒嵓嵔嵕嵖嵗嵙嵚嵜嵞嵟嵠嵡嵢嵣嵤嵥嵦嵧嵨嵪嵭嵮嵰嵱嵲嵳嵵嵶嵷嵸嵹嵺嵻嵼嵽嵾嵿嶀嶁嶃嶄嶅嶆嶇嶈嶉嶊嶋嶌嶍嶎嶏嶐嶑嶒嶓嶔嶕嶖嶗嶘嶚嶛嶜嶞嶟嶠",
    "嶡嶢嶣嶤嶥嶦嶧嶨嶩嶪嶫嶬嶭嶮嶯嶰嶱嶲嶳嶴嶵嶶嶸嶹嶺嶻嶼嶽嶾嶿巀巁巂巃巄巆巇巈巉巊巋巌巎巏巐巑巒巓巔巕巖巗巘巙巚巜巟巠巣巤巪巬巭巰巵巶巸巹巺巻巼巿帀帄帇帉帊帋帍帎帒帓帗帞帟帠帡帢帣帤帥帨帩帪師帬帯帰帲帳帴帵帶帹帺帾帿幀幁幃幆幇幈幉幊幋幍幎幏幐幑幒幓幖幗幘幙幚幜幝幟幠幣幤幥幦幧幨幩幪幫幬幭幮幯幰幱幵幷幹幾庁庂広庅庈庉庌庍庎庒庘庛庝庡庢庣庤庨庩庪庫庬庮庯庰庱庲庴庺庻庼庽庿廀廁廂廃廄廅",
    "廆廇廈廋廌廍廎廏廐廔廕廗廘廙廚廜廝廞廟廠廡廢廣廤廥廦廧廩廫廬廭廮廯廰廱廲廳廵廸廹廻廼廽弅弆弇弉弌弍弎弐弒弔弖弙弚弜弝弞弡弢弣弤弨弫弬弮弰弲弳弴張弶強弸弻弽弾弿彁彂彃彄彅彆彇彈彉彊彋彌彍彎彏彑彔彙彚彛彜彞彟彠彣彥彧彨彫彮彯彲彴彵彶彸彺彽彾彿徃徆徍徎徏徑従徔徖徚徛徝從徟徠徢徣徤徥徦徧復徫徬徯徰徱徲徳徴徶徸徹徺徻徾徿忀忁忂忇忈忊忋忎忓忔忕忚忛応忞忟忢忣忥忦忨忩忬忯忰忲忳忴忶忷忹忺忼怇",
    "怈怉怋怌怐怑怓怗怘怚怞怟怢怣怤怬怭怮怰怱怲怳怴怶怷怸怹怺怽怾恀恄恅恆恇恈恉恊恌恎恏恑恓恔恖恗恘恛恜恞恟恠恡恥恦恮恱恲恴恵恷恾悀悁悂悅悆悇悈悊悋悎悏悐悑悓悕悗悘悙悜悞悡悢悤悥悧悩悪悮悰悳悵悶悷悹悺悽悾悿惀惁惂惃惄惇惈惉惌惍惎惏惐惒惓惔惖惗惙惛惞惡惢惣惤惥惪惱惲惵惷惸惻惼惽惾惿愂愃愄愅愇愊愋愌愐愑愒愓愔愖愗愘愙愛愜愝愞愡愢愥愨愩愪愬愭愮愯愰愱愲愳愴愵愶愷愸愹愺愻愼愽愾慀慁慂慃慄慅慆",
    "慇慉態慍慏慐慒慓慔慖慗慘慙慚慛慜慞慟慠慡慣慤慥慦慩慪慫慬慭慮慯慱慲慳慴慶慸慹慺慻慼慽慾慿憀憁憂憃憄憅憆憇憈憉憊憌憍憏憐憑憒憓憕憖憗憘憙憚憛憜憞憟憠憡憢憣憤憥憦憪憫憭憮憯憰憱憲憳憴憵憶憸憹憺憻憼憽憿懀懁懃懄懅懆懇應懌懍懎懏懐懓懕懖懗懘懙懚懛懜懝懞懟懠懡懢懣懤懥懧懨懩懪懫懬懭懮懯懰懱懲懳懴懶懷懸懹懺懻懼懽懾戀戁戂戃戄戅戇戉戓戔戙戜戝戞戠戣戦戧戨戩戫戭戯戰戱戲戵戶戸戹戺戻戼扂扄扅扆扊",
    "扏扐払扖扗扙扚扜扝扞扟扠扡扢扤扥扨扱扲扴扵扷扸扺扻扽抁抂抃抅抆抇抈抋抌抍抎抏抐抔抙抜抝択抣抦抧抩抪抭抮抯抰抲抳抴抶抷抸抺抾拀拁拃拋拏拑拕拝拞拠拡拤拪拫拰拲拵拸拹拺拻挀挃挄挅挆挊挋挌挍挏挐挒挓挔挕挗挘挙挜挦挧挩挬挭挮挰挱挳挴挵挶挷挸挻挼挾挿捀捁捄捇捈捊捑捒捓捔捖捗捘捙捚捛捜捝捠捤捥捦捨捪捫捬捯捰捲捳捴捵捸捹捼捽捾捿掁掃掄掅掆掋掍掑掓掔掕掗掙掚掛掜掝掞掟採掤掦掫掯掱掲掵掶掹掻掽掿揀",
    "揁揂揃揅揇揈揊揋揌揑揓揔揕揗揘揙揚換揜揝揟揢揤揥揦揧揨揫揬揮揯揰揱揳揵揷揹揺揻揼揾搃搄搆搇搈搉搊損搎搑搒搕搖搗搘搙搚搝搟搢搣搤搥搧搨搩搫搮搯搰搱搲搳搵搶搷搸搹搻搼搾摀摂摃摉摋摌摍摎摏摐摑摓摕摖摗摙摚摛摜摝摟摠摡摢摣摤摥摦摨摪摫摬摮摯摰摱摲摳摴摵摶摷摻摼摽摾摿撀撁撃撆撈撉撊撋撌撍撎撏撐撓撔撗撘撚撛撜撝撟撠撡撢撣撥撦撧撨撪撫撯撱撲撳撴撶撹撻撽撾撿擁擃擄擆擇擈擉擊擋擌擏擑擓擔擕擖擙據",
    "擛擜擝擟擠擡擣擥擧擨擩擪擫擬擭擮擯擰擱擲擳擴擵擶擷擸擹擺擻擼擽擾擿攁攂攃攄攅攆攇攈攊攋攌攍攎攏攐攑攓攔攕攖攗攙攚攛攜攝攞攟攠攡攢攣攤攦攧攨攩攪攬攭攰攱攲攳攷攺攼攽敀敁敂敃敄敆敇敊敋敍敎敐敒敓敔敗敘敚敜敟敠敡敤敥敧敨敩敪敭敮敯敱敳敵敶數敹敺敻敼敽敾敿斀斁斂斃斄斅斆斈斉斊斍斎斏斒斔斕斖斘斚斝斞斠斢斣斦斨斪斬斮斱斲斳斴斵斶斷斸斺斻斾斿旀旂旇旈旉旊旍旐旑旓旔旕旘旙旚旛旜旝旞旟旡旣旤旪旫",
    "旲旳旴旵旸旹旻旼旽旾旿昁昄昅昇昈昉昋昍昐昑昒昖昗昘昚昛昜昞昡昢昣昤昦昩昪昫昬昮昰昲昳昷昸昹昺昻昽昿晀時晄晅晆晇晈晉晊晍晎晐晑晘晙晛晜晝晞晠晢晣晥晧晩晪晫晬晭晱晲晳晵晸晹晻晼晽晿暀暁暃暅暆暈暉暊暋暍暎暏暐暒暓暔暕暘暙暚暛暜暞暟暠暡暢暣暤暥暦暩暪暫暬暭暯暰暱暲暳暵暶暷暸暺暻暼暽暿曀曁曂曃曄曅曆曇曈曉曊曋曌曍曎曏曐曑曒曓曔曕曖曗曘曚曞曟曠曡曢曣曤曥曧曨曪曫曬曭曮曯曱曵曶書曺曻曽朁朂會",
    "朄朅朆朇朌朎朏朑朒朓朖朘朙朚朜朞朠朡朢朣朤朥朧朩朮朰朲朳朶朷朸朹朻朼朾朿杁杄杅杇杊杋杍杒杔杕杗杘杙杚杛杝杢杣杤杦杧杫杬杮東杴杶杸杹杺杻杽枀枂枃枅枆枈枊枌枍枎枏枑枒枓枔枖枙枛枟枠枡枤枦枩枬枮枱枲枴枹枺枻枼枽枾枿柀柂柅柆柇柈柉柊柋柌柍柎柕柖柗柛柟柡柣柤柦柧柨柪柫柭柮柲柵柶柷柸柹柺査柼柾栁栂栃栄栆栍栐栒栔栕栘栙栚栛栜栞栟栠栢栣栤栥栦栧栨栫栬栭栮栯栰栱栴栵栶栺栻栿桇桋桍桏桒桖桗桘桙桚桛",
    "桜桝桞桟桪桬桭桮桯桰桱桲桳桵桸桹桺桻桼桽桾桿梀梂梄梇梈梉梊梋梌梍梎梐梑梒梔梕梖梘梙梚梛梜條梞梟梠梡梣梤梥梩梪梫梬梮梱梲梴梶梷梸梹梺梻梼梽梾梿棁棃棄棅棆棇棈棊棌棎棏棐棑棓棔棖棗棙棛棜棝棞棟棡棢棤棥棦棧棨棩棪棫棬棭棯棲棳棴棶棷棸棻棽棾棿椀椂椃椄椆椇椈椉椊椌椏椑椓椔椕椖椗椘椙椚椛検椝椞椡椢椣椥椦椧椨椩椪椫椬椮椯椱椲椳椵椶椷椸椺椻椼椾楀楁楃楄楅楆楇楈楉楊楋楌楍楎楏楐楑楒楓楕楖楘楙楛楜楟",
    "楡楢楤楥楧楨楩楪楬業楯楰楲楳楴極楶楺楻楽楾楿榁榃榅榊榋榌榎榏榐榑榒榓榖榗榙榚榝榞榟榠榡榢榣榤榥榦榩榪榬榮榯榰榲榳榵榶榸榹榺榼榽榾榿槀槂槃槄槅槆槇槈槉構槍槏槑槒槓槕槖槗様槙槚槜槝槞槡槢槣槤槥槦槧槨槩槪槫槬槮槯槰槱槳槴槵槶槷槸槹槺槻槼槾樀樁樂樃樄樅樆樇樈樉樋樌樍樎樏樐樑樒樓樔樕樖標樚樛樜樝樞樠樢樣樤樥樦樧権樫樬樭樮樰樲樳樴樶樷樸樹樺樻樼樿橀橁橂橃橅橆橈橉橊橋橌橍橎橏橑橒橓橔橕橖橗橚",
    "橜橝橞機橠橢橣橤橦橧橨橩橪橫橬橭橮橯橰橲橳橴橵橶橷橸橺橻橽橾橿檁檂檃檅檆檇檈檉檊檋檌檍檏檒檓檔檕檖檘檙檚檛檜檝檞檟檡檢檣檤檥檦檧檨檪檭檮檯檰檱檲檳檴檵檶檷檸檹檺檻檼檽檾檿櫀櫁櫂櫃櫄櫅櫆櫇櫈櫉櫊櫋櫌櫍櫎櫏櫐櫑櫒櫓櫔櫕櫖櫗櫘櫙櫚櫛櫜櫝櫞櫟櫠櫡櫢櫣櫤櫥櫦櫧櫨櫩櫪櫫櫬櫭櫮櫯櫰櫱櫲櫳櫴櫵櫶櫷櫸櫹櫺櫻櫼櫽櫾櫿欀欁欂欃欄欅欆欇欈欉權欋欌欍欎欏欐欑欒欓欔欕欖欗欘欙欚欛欜欝欞欟欥欦欨欩欪欫欬欭欮",
    "欯欰欱欳欴欵欶欸欻欼欽欿歀歁歂歄歅歈歊歋歍歎歏歐歑歒歓歔歕歖歗歘歚歛歜歝歞歟歠歡歨歩歫歬歭歮歯歰歱歲歳歴歵歶歷歸歺歽歾歿殀殅殈殌殎殏殐殑殔殕殗殘殙殜殝殞殟殠殢殣殤殥殦殧殨殩殫殬殭殮殯殰殱殲殶殸殹殺殻殼殽殾毀毃毄毆毇毈毉毊毌毎毐毑毘毚毜毝毞毟毠毢毣毤毥毦毧毨毩毬毭毮毰毱毲毴毶毷毸毺毻毼毾毿氀氁氂氃氄氈氉氊氋氌氎氒気氜氝氞氠氣氥氫氬氭氱氳氶氷氹氺氻氼氾氿汃汄汅汈汋汌汍汎汏汑汒汓汖汘",
    "汙汚汢汣汥汦汧汫汬汭汮汯汱汳汵汷汸決汻汼汿沀沄沇沊沋沍沎沑沒沕沖沗沘沚沜沝沞沠沢沨沬沯沰沴沵沶沷沺泀況泂泃泆泇泈泋泍泎泏泑泒泘泙泚泜泝泟泤泦泧泩泬泭泲泴泹泿洀洂洃洅洆洈洉洊洍洏洐洑洓洔洕洖洘洜洝洟洠洡洢洣洤洦洨洩洬洭洯洰洴洶洷洸洺洿浀浂浄浉浌浐浕浖浗浘浛浝浟浡浢浤浥浧浨浫浬浭浰浱浲浳浵浶浹浺浻浽浾浿涀涁涃涄涆涇涊涋涍涏涐涒涖涗涘涙涚涜涢涥涬涭涰涱涳涴涶涷涹涺涻涼涽涾淁淂淃淈淉淊",
    "淍淎淏淐淒淓淔淕淗淚淛淜淟淢淣淥淧淨淩淪淭淯淰淲淴淵淶淸淺淽淾淿渀渁渂渃渄渆渇済渉渋渏渒渓渕渘渙減渜渞渟渢渦渧渨渪測渮渰渱渳渵渶渷渹渻渼渽渾渿湀湁湂湅湆湇湈湉湊湋湌湏湐湑湒湕湗湙湚湜湝湞湠湡湢湣湤湥湦湧湨湩湪湬湭湯湰湱湲湳湴湵湶湷湸湹湺湻湼湽満溁溂溄溇溈溊溋溌溍溎溑溒溓溔溕準溗溙溚溛溝溞溠溡溣溤溦溨溩溫溬溭溮溰溳溵溸溹溼溾溿滀滃滄滅滆滈滉滊滌滍滎滐滒滖滘滙滛滜滝滣滧滪滫滬滭滮滯",
    "滰滱滲滳滵滶滷滸滺滻滼滽滾滿漀漁漃漄漅漇漈漊漋漌漍漎漐漑漒漖漗漘漙漚漛漜漝漞漟漡漢漣漥漦漧漨漬漮漰漲漴漵漷漸漹漺漻漼漽漿潀潁潂潃潄潅潈潉潊潌潎潏潐潑潒潓潔潕潖潗潙潚潛潝潟潠潡潣潤潥潧潨潩潪潫潬潯潰潱潳潵潶潷潹潻潽潾潿澀澁澂澃澅澆澇澊澋澏澐澑澒澓澔澕澖澗澘澙澚澛澝澞澟澠澢澣澤澥澦澨澩澪澫澬澭澮澯澰澱澲澴澵澷澸澺澻澼澽澾澿濁濃濄濅濆濇濈濊濋濌濍濎濏濐濓濔濕濖濗濘濙濚濛濜濝濟濢濣濤濥",
    "濦濧濨濩濪濫濬濭濰濱濲濳濴濵濶濷濸濹濺濻濼濽濾濿瀀瀁瀂瀃瀄瀅瀆瀇瀈瀉瀊瀋瀌瀍瀎瀏瀐瀒瀓瀔瀕瀖瀗瀘瀙瀜瀝瀞瀟瀠瀡瀢瀤瀥瀦瀧瀨瀩瀪瀫瀬瀭瀮瀯瀰瀱瀲瀳瀴瀶瀷瀸瀺瀻瀼瀽瀾瀿灀灁灂灃灄灅灆灇灈灉灊灋灍灎灐灑灒灓灔灕灖灗灘灙灚灛灜灝灟灠灡灢灣灤灥灦灧灨灩灪灮灱灲灳灴灷灹灺灻災炁炂炃炄炆炇炈炋炌炍炏炐炑炓炗炘炚炛炞炟炠炡炢炣炤炥炦炧炨炩炪炰炲炴炵炶為炾炿烄烅烆烇烉烋烌烍烎烏烐烑烒烓烔烕烖烗烚",
    "烜烝烞烠烡烢烣烥烪烮烰烱烲烳烴烵烶烸烺烻烼烾烿焀焁焂焃焄焅焆焇焈焋焌焍焎焏焑焒焔焗焛焜焝焞焟焠無焢焣焤焥焧焨焩焪焫焬焭焮焲焳焴焵焷焸焹焺焻焼焽焾焿煀煁煂煃煄煆煇煈煉煋煍煏煐煑煒煓煔煕煖煗煘煙煚煛煝煟煠煡煢煣煥煩煪煫煬煭煯煰煱煴煵煶煷煹煻煼煾煿熀熁熂熃熅熆熇熈熉熋熌熍熎熐熑熒熓熕熖熗熚熛熜熝熞熡熢熣熤熥熦熧熩熪熫熭熮熯熰熱熲熴熶熷熸熺熻熼熽熾熿燀燁燂燄燅燆燇燈燉燊燋燌燍燏燐燑燒燓",
    "燖燗燘燙燚燛燜燝燞營燡燢燣燤燦燨燩燪燫燬燭燯燰燱燲燳燴燵燶燷燸燺燻燼燽燾燿爀爁爂爃爄爅爇爈爉爊爋爌爍爎爏爐爑爒爓爔爕爖爗爘爙爚爛爜爞爟爠爡爢爣爤爥爦爧爩爫爭爮爯爲爳爴爺爼爾牀牁牂牃牄牅牆牉牊牋牎牏牐牑牓牔牕牗牘牚牜牞牠牣牤牥牨牪牫牬牭牰牱牳牴牶牷牸牻牼牽犂犃犅犆犇犈犉犌犎犐犑犓犔犕犖犗犘犙犚犛犜犝犞犠犡犢犣犤犥犦犧犨犩犪犫犮犱犲犳犵犺犻犼犽犾犿狀狅狆狇狉狊狋狌狏狑狓狔狕狖狘狚狛",
    "　、。·ˉˇ¨〃々—～‖…‘’“”〔〕〈〉《》「」『』〖〗【】±×÷∶∧∨∑∏∪∩∈∷√⊥∥∠⌒⊙∫∮≡≌≈∽∝≠≮≯≤≥∞∵∴♂♀°′″℃＄¤￠￡‰§№☆★○●◎◇◆□■△▲※→←↑↓〓",
    "ⅰⅱⅲⅳⅴⅵⅶⅷⅸⅹ⒈⒉⒊⒋⒌⒍⒎⒏⒐⒑⒒⒓⒔⒕⒖⒗⒘⒙⒚⒛⑴⑵⑶⑷⑸⑹⑺⑻⑼⑽⑾⑿⒀⒁⒂⒃⒄⒅⒆⒇①②③④⑤⑥⑦⑧⑨⑩€㈠㈡㈢㈣㈤㈥㈦㈧㈨㈩ⅠⅡⅢⅣⅤⅥⅦⅧⅨⅩⅪⅫ",
    "　！＂＃￥％＆＇（）＊＋，－．／０１２３４５６７８９：；＜＝＞？＠ＡＢＣＤＥＦＧＨＩＪＫＬＭＮＯＰＱＲＳＴＵＶＷＸＹＺ［＼］＾＿｀ａｂｃｄｅｆｇｈｉｊｋｌｍｎｏｐｑｒｓｔｕｖｗｘｙｚ｛｜｝￣",
    "ぁあぃいぅうぇえぉおかがきぎくぐけげこごさざしじすずせぜそぞただちぢっつづてでとどなにぬねのはばぱひびぴふぶぷへべぺほぼぽまみむめもゃやゅゆょよらりるれろゎわゐゑをん",
    "ァアィイゥウェエォオカガキギクグケゲコゴサザシジスズセゼソゾタダチヂッツヅテデトドナニヌネノハバパヒビピフブプヘベペホボポマミムメモャヤュユョヨラリルレロヮワヰヱヲンヴヵヶ",
    "ΑΒΓΔΕΖΗΘΙΚΛΜΝΞΟΠΡΣΤΥΦΧΨΩαβγδεζηθικλμνξοπρστυφχψω︐︒︑︓︔︕︖︵︶︹︺︿﹀︽︾﹁﹂﹃﹄︗︘︻︼︷︸︱︙︳︴",
    "АБВГДЕЁЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯабвгдеёжзийклмнопрстуфхцчшщъыьэюя",
    "ˊˋ˙–―‥‵℅℉↖↗↘↙∕∟∣≒≦≧⊿═║╒╓╔╕╖╗╘╙╚╛╜╝╞╟╠╡╢╣╤╥╦╧╨╩╪╫╬╭╮╯╰╱╲╳▁▂▃▄▅▆▇█▉▊▋▌▍▎▏▓▔▕▼▽◢◣◤◥☉⊕〒〝〞āáǎàēéěèīíǐìōóǒòūúǔùǖǘǚǜüêɑḿńňǹɡㄅㄆㄇㄈㄉㄊㄋㄌㄍㄎㄏㄐㄑㄒㄓㄔㄕㄖㄗㄘㄙㄚㄛㄜㄝㄞㄟㄠㄡㄢㄣㄤㄥㄦㄧㄨㄩ",
    "〡〢〣〤〥〦〧〨〩㊣㎎㎏㎜㎝㎞㎡㏄㏎㏑㏒㏕︰￢￤℡㈱‐ー゛゜ヽヾ〆ゝゞ﹉﹊﹋﹌﹍﹎﹏﹐﹑﹒﹔﹕﹖﹗﹙﹚﹛﹜﹝﹞﹟﹠﹡﹢﹣﹤﹥﹦﹨﹩﹪﹫〾⿰⿱⿲⿳⿴⿵⿶⿷⿸⿹⿺⿻〇─━│┃┄┅┆┇┈┉┊┋┌┍┎┏┐┑┒┓└┕┖┗┘┙┚┛├┝┞┟┠┡┢┣┤┥┦┧┨┩┪┫┬┭┮┯┰┱┲┳┴┵┶┷┸┹┺┻┼┽┾┿╀╁╂╃╄╅╆╇╈╉╊╋",
    "狜狝狟狢狣狤狥狦狧狪狫狵狶狹狽狾狿猀猂猄猅猆猇猈猉猋猌猍猏猐猑猒猔猘猙猚猟猠猣猤猦猧猨猭猯猰猲猳猵猶猺猻猼猽獀獁獂獃獄獅獆獇獈獉獊獋獌獎獏獑獓獔獕獖獘獙獚獛獜獝獞獟獡獢獣獤獥獦獧獨獩獪獫獮獰獱",
    "獲獳獴獵獶獷獸獹獺獻獼獽獿玀玁玂玃玅玆玈玊玌玍玏玐玒玓玔玕玗玘玙玚玜玝玞玠玡玣玤玥玦玧玨玪玬玭玱玴玵玶玸玹玼玽玾玿珁珃珄珅珆珇珋珌珎珒珓珔珕珖珗珘珚珛珜珝珟珡珢珣珤珦珨珪珫珬珮珯珰珱珳珴珵珶珷",
    "珸珹珺珻珼珽現珿琀琁琂琄琇琈琋琌琍琎琑琒琓琔琕琖琗琘琙琜琝琞琟琠琡琣琤琧琩琫琭琯琱琲琷琸琹琺琻琽琾琿瑀瑂瑃瑄瑅瑆瑇瑈瑉瑊瑋瑌瑍瑎瑏瑐瑑瑒瑓瑔瑖瑘瑝瑠瑡瑢瑣瑤瑥瑦瑧瑨瑩瑪瑫瑬瑮瑯瑱瑲瑳瑴瑵瑸瑹瑺",
    "瑻瑼瑽瑿璂璄璅璆璈璉璊璌璍璏璑璒璓璔璕璖璗璘璙璚璛璝璟璠璡璢璣璤璥璦璪璫璬璭璮璯環璱璲璳璴璵璶璷璸璹璻璼璽璾璿瓀瓁瓂瓃瓄瓅瓆瓇瓈瓉瓊瓋瓌瓍瓎瓏瓐瓑瓓瓔瓕瓖瓗瓘瓙瓚瓛瓝瓟瓡瓥瓧瓨瓩瓪瓫瓬瓭瓰瓱瓲",
    "瓳瓵瓸瓹瓺瓻瓼瓽瓾甀甁甂甃甅甆甇甈甉甊甋甌甎甐甒甔甕甖甗甛甝甞甠甡產産甤甦甧甪甮甴甶甹甼甽甿畁畂畃畄畆畇畉畊畍畐畑畒畓畕畖畗畘畝畞畟畠畡畢畣畤畧畨畩畫畬畭畮畯異畱畳畵當畷畺畻畼畽畾疀疁疂疄疅疇",
    "疈疉疊疌疍疎疐疓疕疘疛疜疞疢疦疧疨疩疪疭疶疷疺疻疿痀痁痆痋痌痎痏痐痑痓痗痙痚痜痝痟痠痡痥痩痬痭痮痯痲痳痵痶痷痸痺痻痽痾瘂瘄瘆瘇瘈瘉瘋瘍瘎瘏瘑瘒瘓瘔瘖瘚瘜瘝瘞瘡瘣瘧瘨瘬瘮瘯瘱瘲瘶瘷瘹瘺瘻瘽癁療癄",
    "癅癆癇癈癉癊癋癎癏癐癑癒癓癕癗癘癙癚癛癝癟癠癡癢癤癥癦癧癨癩癪癬癭癮癰癱癲癳癴癵癶癷癹発發癿皀皁皃皅皉皊皌皍皏皐皒皔皕皗皘皚皛皜皝皞皟皠皡皢皣皥皦皧皨皩皪皫皬皭皯皰皳皵皶皷皸皹皺皻皼皽皾盀盁盃啊阿埃挨哎唉哀皑癌蔼矮艾碍爱隘鞍氨安俺按暗岸胺案肮昂盎凹敖熬翱袄傲奥懊澳芭捌扒叭吧笆八疤巴拔跋靶把耙坝霸罢爸白柏百摆佰败拜稗斑班搬扳般颁板版扮拌伴瓣半办绊邦帮梆榜膀绑棒磅蚌镑傍谤苞胞包褒剥",
    "盄盇盉盋盌盓盕盙盚盜盝盞盠盡盢監盤盦盧盨盩盪盫盬盭盰盳盵盶盷盺盻盽盿眀眂眃眅眆眊県眎眏眐眑眒眓眔眕眖眗眘眛眜眝眞眡眣眤眥眧眪眫眬眮眰眱眲眳眴眹眻眽眾眿睂睄睅睆睈睉睊睋睌睍睎睏睒睓睔睕睖睗睘睙睜薄雹保堡饱宝抱报暴豹鲍爆杯碑悲卑北辈背贝钡倍狈备惫焙被奔苯本笨崩绷甭泵蹦迸逼鼻比鄙笔彼碧蓖蔽毕毙毖币庇痹闭敝弊必辟壁臂避陛鞭边编贬扁便变卞辨辩辫遍标彪膘表鳖憋别瘪彬斌濒滨宾摈兵冰柄丙秉饼炳",
    "睝睞睟睠睤睧睩睪睭睮睯睰睱睲睳睴睵睶睷睸睺睻睼瞁瞂瞃瞆瞇瞈瞉瞊瞋瞏瞐瞓瞔瞕瞖瞗瞘瞙瞚瞛瞜瞝瞞瞡瞣瞤瞦瞨瞫瞭瞮瞯瞱瞲瞴瞶瞷瞸瞹瞺瞼瞾矀矁矂矃矄矅矆矇矈矉矊矋矌矎矏矐矑矒矓矔矕矖矘矙矚矝矞矟矠矡矤病并玻菠播拨钵波博勃搏铂箔伯帛舶脖膊渤泊驳捕卜哺补埠不布步簿部怖擦猜裁材才财睬踩采彩菜蔡餐参蚕残惭惨灿苍舱仓沧藏操糙槽曹草厕策侧册测层蹭插叉茬茶查碴搽察岔差诧拆柴豺搀掺蝉馋谗缠铲产阐颤昌猖",
    "矦矨矪矯矰矱矲矴矵矷矹矺矻矼砃砄砅砆砇砈砊砋砎砏砐砓砕砙砛砞砠砡砢砤砨砪砫砮砯砱砲砳砵砶砽砿硁硂硃硄硆硈硉硊硋硍硏硑硓硔硘硙硚硛硜硞硟硠硡硢硣硤硥硦硧硨硩硯硰硱硲硳硴硵硶硸硹硺硻硽硾硿碀碁碂碃场尝常长偿肠厂敞畅唱倡超抄钞朝嘲潮巢吵炒车扯撤掣彻澈郴臣辰尘晨忱沉陈趁衬撑称城橙成呈乘程惩澄诚承逞骋秤吃痴持匙池迟弛驰耻齿侈尺赤翅斥炽充冲虫崇宠抽酬畴踌稠愁筹仇绸瞅丑臭初出橱厨躇锄雏滁除楚",
    "碄碅碆碈碊碋碏碐碒碔碕碖碙碝碞碠碢碤碦碨碩碪碫碬碭碮碯碵碶碷碸確碻碼碽碿磀磂磃磄磆磇磈磌磍磎磏磑磒磓磖磗磘磚磛磜磝磞磟磠磡磢磣磤磥磦磧磩磪磫磭磮磯磰磱磳磵磶磸磹磻磼磽磾磿礀礂礃礄礆礇礈礉礊礋礌础储矗搐触处揣川穿椽传船喘串疮窗幢床闯创吹炊捶锤垂春椿醇唇淳纯蠢戳绰疵茨磁雌辞慈瓷词此刺赐次聪葱囱匆从丛凑粗醋簇促蹿篡窜摧崔催脆瘁粹淬翠村存寸磋撮搓措挫错搭达答瘩打大呆歹傣戴带殆代贷袋待逮",
    "礍礎礏礐礑礒礔礕礖礗礘礙礚礛礜礝礟礠礡礢礣礥礦礧礨礩礪礫礬礭礮礯礰礱礲礳礵礶礷礸礹礽礿祂祃祄祅祇祊祋祌祍祎祏祐祑祒祔祕祘祙祡祣祤祦祩祪祫祬祮祰祱祲祳祴祵祶祹祻祼祽祾祿禂禃禆禇禈禉禋禌禍禎禐禑禒怠耽担丹单郸掸胆旦氮但惮淡诞弹蛋当挡党荡档刀捣蹈倒岛祷导到稻悼道盗德得的蹬灯登等瞪凳邓堤低滴迪敌笛狄涤翟嫡抵底地蒂第帝弟递缔颠掂滇碘点典靛垫电佃甸店惦奠淀殿碉叼雕凋刁掉吊钓调跌爹碟蝶迭谍叠",
    "禓禔禕禖禗禘禙禛禜禝禞禟禠禡禢禣禤禥禦禨禩禪禫禬禭禮禯禰禱禲禴禵禶禷禸禼禿秂秄秅秇秈秊秌秎秏秐秓秔秖秗秙秚秛秜秝秞秠秡秢秥秨秪秬秮秱秲秳秴秵秶秷秹秺秼秾秿稁稄稅稇稈稉稊稌稏稐稑稒稓稕稖稘稙稛稜丁盯叮钉顶鼎锭定订丢东冬董懂动栋侗恫冻洞兜抖斗陡豆逗痘都督毒犊独读堵睹赌杜镀肚度渡妒端短锻段断缎堆兑队对墩吨蹲敦顿囤钝盾遁掇哆多夺垛躲朵跺舵剁惰堕蛾峨鹅俄额讹娥恶厄扼遏鄂饿恩而儿耳尔饵洱二",
    "稝稟稡稢稤稥稦稧稨稩稪稫稬稭種稯稰稱稲稴稵稶稸稺稾穀穁穂穃穄穅穇穈穉穊穋穌積穎穏穐穒穓穔穕穖穘穙穚穛穜穝穞穟穠穡穢穣穤穥穦穧穨穩穪穫穬穭穮穯穱穲穳穵穻穼穽穾窂窅窇窉窊窋窌窎窏窐窓窔窙窚窛窞窡窢贰发罚筏伐乏阀法珐藩帆番翻樊矾钒繁凡烦反返范贩犯饭泛坊芳方肪房防妨仿访纺放菲非啡飞肥匪诽吠肺废沸费芬酚吩氛分纷坟焚汾粉奋份忿愤粪丰封枫蜂峰锋风疯烽逢冯缝讽奉凤佛否夫敷肤孵扶拂辐幅氟符伏俘服",
    "窣窤窧窩窪窫窮窯窰窱窲窴窵窶窷窸窹窺窻窼窽窾竀竁竂竃竄竅竆竇竈竉竊竌竍竎竏竐竑竒竓竔竕竗竘竚竛竜竝竡竢竤竧竨竩竪竫竬竮竰竱竲竳竴竵競竷竸竻竼竾笀笁笂笅笇笉笌笍笎笐笒笓笖笗笘笚笜笝笟笡笢笣笧笩笭浮涪福袱弗甫抚辅俯釜斧脯腑府腐赴副覆赋复傅付阜父腹负富讣附妇缚咐噶嘎该改概钙盖溉干甘杆柑竿肝赶感秆敢赣冈刚钢缸肛纲岗港杠篙皋高膏羔糕搞镐稿告哥歌搁戈鸽胳疙割革葛格蛤阁隔铬个各给根跟耕更庚羹",
    "笯笰笲笴笵笶笷笹笻笽笿筀筁筂筃筄筆筈筊筍筎筓筕筗筙筜筞筟筡筣筤筥筦筧筨筩筪筫筬筭筯筰筳筴筶筸筺筼筽筿箁箂箃箄箆箇箈箉箊箋箌箎箏箑箒箓箖箘箙箚箛箞箟箠箣箤箥箮箯箰箲箳箵箶箷箹箺箻箼箽箾箿節篂篃範埂耿梗工攻功恭龚供躬公宫弓巩汞拱贡共钩勾沟苟狗垢构购够辜菇咕箍估沽孤姑鼓古蛊骨谷股故顾固雇刮瓜剐寡挂褂乖拐怪棺关官冠观管馆罐惯灌贯光广逛瑰规圭硅归龟闺轨鬼诡癸桂柜跪贵刽辊滚棍锅郭国果裹过哈",
    "篅篈築篊篋篍篎篏篐篒篔篕篖篗篘篛篜篞篟篠篢篣篤篧篨篩篫篬篭篯篰篲篳篴篵篶篸篹篺篻篽篿簀簁簂簃簄簅簆簈簉簊簍簎簐簑簒簓簔簕簗簘簙簚簛簜簝簞簠簡簢簣簤簥簨簩簫簬簭簮簯簰簱簲簳簴簵簶簷簹簺簻簼簽簾籂骸孩海氦亥害骇酣憨邯韩含涵寒函喊罕翰撼捍旱憾悍焊汗汉夯杭航壕嚎豪毫郝好耗号浩呵喝荷菏核禾和何合盒貉阂河涸赫褐鹤贺嘿黑痕很狠恨哼亨横衡恒轰哄烘虹鸿洪宏弘红喉侯猴吼厚候后呼乎忽瑚壶葫胡蝴狐糊湖",
    "籃籄籅籆籇籈籉籊籋籌籎籏籐籑籒籓籔籕籖籗籘籙籚籛籜籝籞籟籠籡籢籣籤籥籦籧籨籩籪籫籬籭籮籯籰籱籲籵籶籷籸籹籺籾籿粀粁粂粃粄粅粆粇粈粊粋粌粍粎粏粐粓粔粖粙粚粛粠粡粣粦粧粨粩粫粬粭粯粰粴粵粶粷粸粺粻弧虎唬护互沪户花哗华猾滑画划化话槐徊怀淮坏欢环桓还缓换患唤痪豢焕涣宦幻荒慌黄磺蝗簧皇凰惶煌晃幌恍谎灰挥辉徽恢蛔回毁悔慧卉惠晦贿秽会烩汇讳诲绘荤昏婚魂浑混豁活伙火获或惑霍货祸击圾基机畸稽积箕",
    "粿糀糂糃糄糆糉糋糎糏糐糑糒糓糔糘糚糛糝糞糡糢糣糤糥糦糧糩糪糫糬糭糮糰糱糲糳糴糵糶糷糹糺糼糽糾糿紀紁紂紃約紅紆紇紈紉紋紌納紎紏紐紑紒紓純紕紖紗紘紙級紛紜紝紞紟紡紣紤紥紦紨紩紪紬紭紮細紱紲紳紴紵紶肌饥迹激讥鸡姬绩缉吉极棘辑籍集及急疾汲即嫉级挤几脊己蓟技冀季伎祭剂悸济寄寂计记既忌际妓继纪嘉枷夹佳家加荚颊贾甲钾假稼价架驾嫁歼监坚尖笺间煎兼肩艰奸缄茧检柬碱硷拣捡简俭剪减荐槛鉴践贱见键箭件",
    "紷紸紹紺紻紼紽紾紿絀絁終絃組絅絆絇絈絉絊絋経絍絎絏結絑絒絓絔絕絖絗絘絙絚絛絜絝絞絟絠絡絢絣絤絥給絧絨絩絪絫絬絭絯絰統絲絳絴絵絶絸絹絺絻絼絽絾絿綀綁綂綃綄綅綆綇綈綉綊綋綌綍綎綏綐綑綒經綔綕綖綗綘健舰剑饯渐溅涧建僵姜将浆江疆蒋桨奖讲匠酱降蕉椒礁焦胶交郊浇骄娇嚼搅铰矫侥脚狡角饺缴绞剿教酵轿较叫窖揭接皆秸街阶截劫节桔杰捷睫竭洁结解姐戒藉芥界借介疥诫届巾筋斤金今津襟紧锦仅谨进靳晋禁近烬浸",
    "継続綛綜綝綞綟綠綡綢綣綤綥綧綨綩綪綫綬維綯綰綱網綳綴綵綶綷綸綹綺綻綼綽綾綿緀緁緂緃緄緅緆緇緈緉緊緋緌緍緎総緐緑緒緓緔緕緖緗緘緙線緛緜緝緞緟締緡緢緣緤緥緦緧編緩緪緫緬緭緮緯緰緱緲緳練緵緶緷緸緹緺尽劲荆兢茎睛晶鲸京惊精粳经井警景颈静境敬镜径痉靖竟竞净炯窘揪究纠玖韭久灸九酒厩救旧臼舅咎就疚鞠拘狙疽居驹菊局咀矩举沮聚拒据巨具距踞锯俱句惧炬剧捐鹃娟倦眷卷绢撅攫抉掘倔爵觉决诀绝均菌钧军君峻",
    "緻緼緽緾緿縀縁縂縃縄縅縆縇縈縉縊縋縌縍縎縏縐縑縒縓縔縕縖縗縘縙縚縛縜縝縞縟縠縡縢縣縤縥縦縧縨縩縪縫縬縭縮縯縰縱縲縳縴縵縶縷縸縹縺縼總績縿繀繂繃繄繅繆繈繉繊繋繌繍繎繏繐繑繒繓織繕繖繗繘繙繚繛繜繝俊竣浚郡骏喀咖卡咯开揩楷凯慨刊堪勘坎砍看康慷糠扛抗亢炕考拷烤靠坷苛柯棵磕颗科壳咳可渴克刻客课肯啃垦恳坑吭空恐孔控抠口扣寇枯哭窟苦酷库裤夸垮挎跨胯块筷侩快宽款匡筐狂框矿眶旷况亏盔岿窥葵奎魁傀",
    "繞繟繠繡繢繣繤繥繦繧繨繩繪繫繬繭繮繯繰繱繲繳繴繵繶繷繸繹繺繻繼繽繾繿纀纁纃纄纅纆纇纈纉纊纋續纍纎纏纐纑纒纓纔纕纖纗纘纙纚纜纝纞纮纴纻纼绖绤绬绹缊缐缞缷缹缻缼缽缾缿罀罁罃罆罇罈罉罊罋罌罍罎罏罒罓馈愧溃坤昆捆困括扩廓阔垃拉喇蜡腊辣啦莱来赖蓝婪栏拦篮阑兰澜谰揽览懒缆烂滥琅榔狼廊郎朗浪捞劳牢老佬姥酪烙涝勒乐雷镭蕾磊累儡垒擂肋类泪棱楞冷厘梨犁黎篱狸离漓理李里鲤礼莉荔吏栗丽厉励砾历利傈例俐",
    "罖罙罛罜罝罞罠罣罤罥罦罧罫罬罭罯罰罳罵罶罷罸罺罻罼罽罿羀羂羃羄羅羆羇羈羉羋羍羏羐羑羒羓羕羖羗羘羙羛羜羠羢羣羥羦羨義羪羫羬羭羮羱羳羴羵羶羷羺羻羾翀翂翃翄翆翇翈翉翋翍翏翐翑習翓翖翗翙翚翛翜翝翞翢翣痢立粒沥隶力璃哩俩联莲连镰廉怜涟帘敛脸链恋炼练粮凉梁粱良两辆量晾亮谅撩聊僚疗燎寥辽潦了撂镣廖料列裂烈劣猎琳林磷霖临邻鳞淋凛赁吝拎玲菱零龄铃伶羚凌灵陵岭领另令溜琉榴硫馏留刘瘤流柳六龙聋咙笼窿",
    "翤翧翨翪翫翬翭翯翲翴翵翶翷翸翹翺翽翾翿耂耇耈耉耊耎耏耑耓耚耛耝耞耟耡耣耤耫耬耭耮耯耰耲耴耹耺耼耾聀聁聄聅聇聈聉聎聏聐聑聓聕聖聗聙聛聜聝聞聟聠聡聢聣聤聥聦聧聨聫聬聭聮聯聰聲聳聴聵聶職聸聹聺聻聼聽隆垄拢陇楼娄搂篓漏陋芦卢颅庐炉掳卤虏鲁麓碌露路赂鹿潞禄录陆戮驴吕铝侣旅履屡缕虑氯律率滤绿峦挛孪滦卵乱掠略抡轮伦仑沦纶论萝螺罗逻锣箩骡裸落洛骆络妈麻玛码蚂马骂嘛吗埋买麦卖迈脉瞒馒蛮满蔓曼慢漫",
    "聾肁肂肅肈肊肍肎肏肐肑肒肔肕肗肙肞肣肦肧肨肬肰肳肵肶肸肹肻胅胇胈胉胊胋胏胐胑胒胓胔胕胘胟胠胢胣胦胮胵胷胹胻胾胿脀脁脃脄脅脇脈脋脌脕脗脙脛脜脝脟脠脡脢脣脤脥脦脧脨脩脪脫脭脮脰脳脴脵脷脹脺脻脼脽脿谩芒茫盲氓忙莽猫茅锚毛矛铆卯茂冒帽貌贸么玫枚梅酶霉煤没眉媒镁每美昧寐妹媚门闷们萌蒙檬盟锰猛梦孟眯醚靡糜迷谜弥米秘觅泌蜜密幂棉眠绵冕免勉娩缅面苗描瞄藐秒渺庙妙蔑灭民抿皿敏悯闽明螟鸣铭名命谬摸",
    "腀腁腂腃腄腅腇腉腍腎腏腒腖腗腘腛腜腝腞腟腡腢腣腤腦腨腪腫腬腯腲腳腵腶腷腸膁膃膄膅膆膇膉膋膌膍膎膐膒膓膔膕膖膗膙膚膞膟膠膡膢膤膥膧膩膫膬膭膮膯膰膱膲膴膵膶膷膸膹膼膽膾膿臄臅臇臈臉臋臍臎臏臐臑臒臓摹蘑模膜磨摩魔抹末莫墨默沫漠寞陌谋牟某拇牡亩姆母墓暮幕募慕木目睦牧穆拿哪呐钠那娜纳氖乃奶耐奈南男难囊挠脑恼闹淖呢馁内嫩能妮霓倪泥尼拟你匿腻逆溺蔫拈年碾撵捻念娘酿鸟尿捏聂孽啮镊镍涅您柠狞凝宁",
    "臔臕臖臗臘臙臚臛臜臝臞臟臠臡臢臤臥臦臨臩臫臮臯臰臱臲臵臶臷臸臹臺臽臿舃與興舉舊舋舎舏舑舓舕舖舗舘舙舚舝舠舤舥舦舧舩舮舲舺舼舽舿艀艁艂艃艅艆艈艊艌艍艎艐艑艒艓艔艕艖艗艙艛艜艝艞艠艡艢艣艤艥艦艧艩拧泞牛扭钮纽脓浓农弄奴努怒女暖虐疟挪懦糯诺哦欧鸥殴藕呕偶沤啪趴爬帕怕琶拍排牌徘湃派攀潘盘磐盼畔判叛乓庞旁耪胖抛咆刨炮袍跑泡呸胚培裴赔陪配佩沛喷盆砰抨烹澎彭蓬棚硼篷膨朋鹏捧碰坯砒霹批披劈琵毗",
    "艪艫艬艭艱艵艶艷艸艻艼芀芁芃芅芆芇芉芌芐芓芔芕芖芚芛芞芠芢芣芧芲芵芶芺芻芼芿苀苂苃苅苆苉苐苖苙苚苝苢苧苨苩苪苬苭苮苰苲苳苵苶苸苺苼苽苾苿茀茊茋茍茐茒茓茖茘茙茝茞茟茠茡茢茣茤茥茦茩茪茮茰茲茷茻茽啤脾疲皮匹痞僻屁譬篇偏片骗飘漂瓢票撇瞥拼频贫品聘乒坪苹萍平凭瓶评屏坡泼颇婆破魄迫粕剖扑铺仆莆葡菩蒲埔朴圃普浦谱曝瀑期欺栖戚妻七凄漆柒沏其棋奇歧畦崎脐齐旗祈祁骑起岂乞企启契砌器气迄弃汽泣讫掐",
    "茾茿荁荂荄荅荈荊荋荌荍荎荓荕荖荗荘荙荝荢荰荱荲荳荴荵荶荹荺荾荿莀莁莂莃莄莇莈莊莋莌莍莏莐莑莔莕莖莗莙莚莝莟莡莢莣莤莥莦莧莬莭莮莯莵莻莾莿菂菃菄菆菈菉菋菍菎菐菑菒菓菕菗菙菚菛菞菢菣菤菦菧菨菫菬菭恰洽牵扦钎铅千迁签仟谦乾黔钱钳前潜遣浅谴堑嵌欠歉枪呛腔羌墙蔷强抢橇锹敲悄桥瞧乔侨巧鞘撬翘峭俏窍切茄且怯窃钦侵亲秦琴勤芹擒禽寝沁青轻氢倾卿清擎晴氰情顷请庆琼穷秋丘邱球求囚酋泅趋区蛆曲躯屈驱渠",
    "菮華菳菴菵菶菷菺菻菼菾菿萀萂萅萇萈萉萊萐萒萓萔萕萖萗萙萚萛萞萟萠萡萢萣萩萪萫萬萭萮萯萰萲萳萴萵萶萷萹萺萻萾萿葀葁葂葃葄葅葇葈葉葊葋葌葍葎葏葐葒葓葔葕葖葘葝葞葟葠葢葤葥葦葧葨葪葮葯葰葲葴葷葹葻葼取娶龋趣去圈颧权醛泉全痊拳犬券劝缺炔瘸却鹊榷确雀裙群然燃冉染瓤壤攘嚷让饶扰绕惹热壬仁人忍韧任认刃妊纫扔仍日戎茸蓉荣融熔溶容绒冗揉柔肉茹蠕儒孺如辱乳汝入褥软阮蕊瑞锐闰润若弱撒洒萨腮鳃塞赛三叁",
    "葽葾葿蒀蒁蒃蒄蒅蒆蒊蒍蒏蒐蒑蒒蒓蒔蒕蒖蒘蒚蒛蒝蒞蒟蒠蒢蒣蒤蒥蒦蒧蒨蒩蒪蒫蒬蒭蒮蒰蒱蒳蒵蒶蒷蒻蒼蒾蓀蓂蓃蓅蓆蓇蓈蓋蓌蓎蓏蓒蓔蓕蓗蓘蓙蓚蓛蓜蓞蓡蓢蓤蓧蓨蓩蓪蓫蓭蓮蓯蓱蓲蓳蓴蓵蓶蓷蓸蓹蓺蓻蓽蓾蔀蔁蔂伞散桑嗓丧搔骚扫嫂瑟色涩森僧莎砂杀刹沙纱傻啥煞筛晒珊苫杉山删煽衫闪陕擅赡膳善汕扇缮墒伤商赏晌上尚裳梢捎稍烧芍勺韶少哨邵绍奢赊蛇舌舍赦摄射慑涉社设砷申呻伸身深娠绅神沈审婶甚肾慎渗声生甥牲升绳",
    "蔃蔄蔅蔆蔇蔈蔉蔊蔋蔍蔎蔏蔐蔒蔔蔕蔖蔘蔙蔛蔜蔝蔞蔠蔢蔣蔤蔥蔦蔧蔨蔩蔪蔭蔮蔯蔰蔱蔲蔳蔴蔵蔶蔾蔿蕀蕁蕂蕄蕅蕆蕇蕋蕌蕍蕎蕏蕐蕑蕒蕓蕔蕕蕗蕘蕚蕛蕜蕝蕟蕠蕡蕢蕣蕥蕦蕧蕩蕪蕫蕬蕭蕮蕯蕰蕱蕳蕵蕶蕷蕸蕼蕽蕿薀薁省盛剩胜圣师失狮施湿诗尸虱十石拾时什食蚀实识史矢使屎驶始式示士世柿事拭誓逝势是嗜噬适仕侍释饰氏市恃室视试收手首守寿授售受瘦兽蔬枢梳殊抒输叔舒淑疏书赎孰熟薯暑曙署蜀黍鼠属术述树束戍竖墅庶数漱",
    "薂薃薆薈薉薊薋薌薍薎薐薑薒薓薔薕薖薗薘薙薚薝薞薟薠薡薢薣薥薦薧薩薫薬薭薱薲薳薴薵薶薸薺薻薼薽薾薿藀藂藃藄藅藆藇藈藊藋藌藍藎藑藒藔藖藗藘藙藚藛藝藞藟藠藡藢藣藥藦藧藨藪藫藬藭藮藯藰藱藲藳藴藵藶藷藸恕刷耍摔衰甩帅栓拴霜双爽谁水睡税吮瞬顺舜说硕朔烁斯撕嘶思私司丝死肆寺嗣四伺似饲巳松耸怂颂送宋讼诵搜艘擞嗽苏酥俗素速粟僳塑溯宿诉肃酸蒜算虽隋随绥髓碎岁穗遂隧祟孙损笋蓑梭唆缩琐索锁所塌他它她塔",
    "藹藺藼藽藾蘀蘁蘂蘃蘄蘆蘇蘈蘉蘊蘋蘌蘍蘎蘏蘐蘒蘓蘔蘕蘗蘘蘙蘚蘛蘜蘝蘞蘟蘠蘡蘢蘣蘤蘥蘦蘨蘪蘫蘬蘭蘮蘯蘰蘱蘲蘳蘴蘵蘶蘷蘹蘺蘻蘽蘾蘿虀虁虂虃虄虅虆虇虈虉虊虋虌虒虓處虖虗虘虙虛虜虝號虠虡虣虤虥虦虧虨虩虪獭挞蹋踏胎苔抬台泰酞太态汰坍摊贪瘫滩坛檀痰潭谭谈坦毯袒碳探叹炭汤塘搪堂棠膛唐糖倘躺淌趟烫掏涛滔绦萄桃逃淘陶讨套特藤腾疼誊梯剔踢锑提题蹄啼体替嚏惕涕剃屉天添填田甜恬舔腆挑条迢眺跳贴铁帖厅听烃",
    "虭虯虰虲虳虴虵虶虷虸蚃蚄蚅蚆蚇蚈蚉蚎蚏蚐蚑蚒蚔蚖蚗蚘蚙蚚蚛蚞蚟蚠蚡蚢蚥蚦蚫蚭蚮蚲蚳蚷蚸蚹蚻蚼蚽蚾蚿蛁蛂蛃蛅蛈蛌蛍蛒蛓蛕蛖蛗蛚蛜蛝蛠蛡蛢蛣蛥蛦蛧蛨蛪蛫蛬蛯蛵蛶蛷蛺蛻蛼蛽蛿蜁蜄蜅蜆蜋蜌蜎蜏蜐蜑蜔蜖汀廷停亭庭挺艇通桐酮瞳同铜彤童桶捅筒统痛偷投头透凸秃突图徒途涂屠土吐兔湍团推颓腿蜕褪退吞屯臀拖托脱鸵陀驮驼椭妥拓唾挖哇蛙洼娃瓦袜歪外豌弯湾玩顽丸烷完碗挽晚皖惋宛婉万腕汪王亡枉网往旺望忘妄威",
    "蜙蜛蜝蜟蜠蜤蜦蜧蜨蜪蜫蜬蜭蜯蜰蜲蜳蜵蜶蜸蜹蜺蜼蜽蝀蝁蝂蝃蝄蝅蝆蝊蝋蝍蝏蝐蝑蝒蝔蝕蝖蝘蝚蝛蝜蝝蝞蝟蝡蝢蝦蝧蝨蝩蝪蝫蝬蝭蝯蝱蝲蝳蝵蝷蝸蝹蝺蝿螀螁螄螆螇螉螊螌螎螏螐螑螒螔螕螖螘螙螚螛螜螝螞螠螡螢螣螤巍微危韦违桅围唯惟为潍维苇萎委伟伪尾纬未蔚味畏胃喂魏位渭谓尉慰卫瘟温蚊文闻纹吻稳紊问嗡翁瓮挝蜗涡窝我斡卧握沃巫呜钨乌污诬屋无芜梧吾吴毋武五捂午舞伍侮坞戊雾晤物勿务悟误昔熙析西硒矽晰嘻吸锡牺",
    "螥螦螧螩螪螮螰螱螲螴螶螷螸螹螻螼螾螿蟁蟂蟃蟄蟅蟇蟈蟉蟌蟍蟎蟏蟐蟔蟕蟖蟗蟘蟙蟚蟜蟝蟞蟟蟡蟢蟣蟤蟦蟧蟨蟩蟫蟬蟭蟯蟰蟱蟲蟳蟴蟵蟶蟷蟸蟺蟻蟼蟽蟿蠀蠁蠂蠄蠅蠆蠇蠈蠉蠋蠌蠍蠎蠏蠐蠑蠒蠔蠗蠘蠙蠚蠜蠝蠞蠟蠠蠣稀息希悉膝夕惜熄烯溪汐犀檄袭席习媳喜铣洗系隙戏细瞎虾匣霞辖暇峡侠狭下厦夏吓掀锨先仙鲜纤咸贤衔舷闲涎弦嫌显险现献县腺馅羡宪陷限线相厢镶香箱襄湘乡翔祥详想响享项巷橡像向象萧硝霄削哮嚣销消宵淆晓",
    "蠤蠥蠦蠧蠨蠩蠪蠫蠬蠭蠮蠯蠰蠱蠳蠴蠵蠶蠷蠸蠺蠻蠽蠾蠿衁衂衃衆衇衈衉衊衋衎衏衐衑衒術衕衖衘衚衛衜衝衞衟衠衦衧衪衭衯衱衳衴衵衶衸衹衺衻衼袀袃袆袇袉袊袌袎袏袐袑袓袔袕袗袘袙袚袛袝袞袟袠袡袣袥袦袧袨袩袪小孝校肖啸笑效楔些歇蝎鞋协挟携邪斜胁谐写械卸蟹懈泄泻谢屑薪芯锌欣辛新忻心信衅星腥猩惺兴刑型形邢行醒幸杏性姓兄凶胸匈汹雄熊休修羞朽嗅锈秀袖绣墟戌需虚嘘须徐许蓄酗叙旭序畜恤絮婿绪续轩喧宣悬旋玄",
    "袬袮袯袰袲袳袴袵袶袸袹袺袻袽袾袿裀裃裄裇裈裊裋裌裍裏裐裑裓裖裗裚裛補裝裞裠裡裦裧裩裪裫裬裭裮裯裲裵裶裷裺裻製裿褀褁褃褄褅褆複褈褉褋褌褍褎褏褑褔褕褖褗褘褜褝褞褟褠褢褣褤褦褧褨褩褬褭褮褯褱褲褳褵褷选癣眩绚靴薛学穴雪血勋熏循旬询寻驯巡殉汛训讯逊迅压押鸦鸭呀丫芽牙蚜崖衙涯雅哑亚讶焉咽阉烟淹盐严研蜒岩延言颜阎炎沿奄掩眼衍演艳堰燕厌砚雁唁彦焰宴谚验殃央鸯秧杨扬佯疡羊洋阳氧仰痒养样漾邀腰妖瑶",
    "褸褹褺褻褼褽褾褿襀襂襃襅襆襇襈襉襊襋襌襍襎襏襐襑襒襓襔襕襖襗襘襙襚襛襜襝襠襡襢襣襤襥襧襨襩襪襫襬襭襮襯襰襱襲襳襴襵襶襷襸襹襺襼襽襾覀覂覄覅覇覈覉覊見覌覍覎規覐覑覒覓覔覕視覗覘覙覚覛覜覝覞覟覠覡摇尧遥窑谣姚咬舀药要耀椰噎耶爷野冶也页掖业叶曳腋夜液一壹医揖铱依伊衣颐夷遗移仪胰疑沂宜姨彝椅蚁倚已乙矣以艺抑易邑屹亿役臆逸肄疫亦裔意毅忆义益溢诣议谊译异翼翌绎茵荫因殷音阴姻吟银淫寅饮尹引隐",
    "覢覣覤覥覦覧覨覩親覫覬覭覮覯覰覱覲観覴覵覶覷覸覹覺覻覼覽覾覿觀觃觍觓觔觕觗觘觙觛觝觟觠觡觢觤觧觨觩觪觬觭觮觰觱觲觴觵觶觷觸觹觺觻觼觽觾觿訁訂訃訄訅訆計訉訊訋訌訍討訏訐訑訒訓訔訕訖託記訙訚訛訜訝印英樱婴鹰应缨莹萤营荧蝇迎赢盈影颖硬映哟拥佣臃痈庸雍踊蛹咏泳涌永恿勇用幽优悠忧尤由邮铀犹油游酉有友右佑釉诱又幼迂淤于盂榆虞愚舆余俞逾鱼愉渝渔隅予娱雨与屿禹宇语羽玉域芋郁吁遇喻峪御愈欲狱育誉",
    "訞訟訠訡訢訣訤訥訦訧訨訩訪訫訬設訮訯訰許訲訳訴訵訶訷訸訹診註証訽訿詀詁詂詃詄詅詆詇詉詊詋詌詍詎詏詐詑詒詓詔評詖詗詘詙詚詛詜詝詞詟詠詡詢詣詤詥試詧詨詩詪詫詬詭詮詯詰話該詳詴詵詶詷詸詺詻詼詽詾詿誀浴寓裕预豫驭鸳渊冤元垣袁原援辕园员圆猿源缘远苑愿怨院曰约越跃钥岳粤月悦阅耘云郧匀陨允运蕴酝晕韵孕匝砸杂栽哉灾宰载再在咱攒暂赞赃脏葬遭糟凿藻枣早澡蚤躁噪造皂灶燥责择则泽贼怎增憎曾赠扎喳渣札轧",
    "誁誂誃誄誅誆誇誈誋誌認誎誏誐誑誒誔誕誖誗誘誙誚誛誜誝語誟誠誡誢誣誤誥誦誧誨誩說誫説読誮誯誰誱課誳誴誵誶誷誸誹誺誻誼誽誾調諀諁諂諃諄諅諆談諈諉諊請諌諍諎諏諐諑諒諓諔諕論諗諘諙諚諛諜諝諞諟諠諡諢諣铡闸眨栅榨咋乍炸诈摘斋宅窄债寨瞻毡詹粘沾盏斩辗崭展蘸栈占战站湛绽樟章彰漳张掌涨杖丈帐账仗胀瘴障招昭找沼赵照罩兆肇召遮折哲蛰辙者锗蔗这浙珍斟真甄砧臻贞针侦枕疹诊震振镇阵蒸挣睁征狰争怔整拯正政",
    "諤諥諦諧諨諩諪諫諬諭諮諯諰諱諲諳諴諵諶諷諸諹諺諻諼諽諾諿謀謁謂謃謄謅謆謈謉謊謋謌謍謎謏謐謑謒謓謔謕謖謗謘謙謚講謜謝謞謟謠謡謢謣謤謥謧謨謩謪謫謬謭謮謯謰謱謲謳謴謵謶謷謸謹謺謻謼謽謾謿譀譁譂譃譄譅帧症郑证芝枝支吱蜘知肢脂汁之织职直植殖执值侄址指止趾只旨纸志挚掷至致置帜峙制智秩稚质炙痔滞治窒中盅忠钟衷终种肿重仲众舟周州洲诌粥轴肘帚咒皱宙昼骤珠株蛛朱猪诸诛逐竹烛煮拄瞩嘱主著柱助蛀贮铸筑",
    "譆譇譈證譊譋譌譍譎譏譐譑譒譓譔譕譖譗識譙譚譛譜譝譞譟譠譡譢譣譤譥譧譨譩譪譫譭譮譯議譱譲譳譴譵譶護譸譹譺譻譼譽譾譿讀讁讂讃讄讅讆讇讈讉變讋讌讍讎讏讐讑讒讓讔讕讖讗讘讙讚讛讜讝讞讟讬讱讻诇诐诪谉谞住注祝驻抓爪拽专砖转撰赚篆桩庄装妆撞壮状椎锥追赘坠缀谆准捉拙卓桌琢茁酌啄着灼浊兹咨资姿滋淄孜紫仔籽滓子自渍字鬃棕踪宗综总纵邹走奏揍租足卒族祖诅阻组钻纂嘴醉最罪尊遵昨左佐柞做作坐座",
    "谸谹谺谻谼谽谾谿豀豂豃豄豅豈豊豋豍豎豏豐豑豒豓豔豖豗豘豙豛豜豝豞豟豠豣豤豥豦豧豨豩豬豭豮豯豰豱豲豴豵豶豷豻豼豽豾豿貀貁貃貄貆貇貈貋貍貎貏貐貑貒貓貕貖貗貙貚貛貜貝貞貟負財貢貣貤貥貦貧貨販貪貫責貭亍丌兀丐廿卅丕亘丞鬲孬噩丨禺丿匕乇夭爻卮氐囟胤馗毓睾鼗丶亟鼐乜乩亓芈孛啬嘏仄厍厝厣厥厮靥赝匚叵匦匮匾赜卦卣刂刈刎刭刳刿剀剌剞剡剜蒯剽劂劁劐劓冂罔亻仃仉仂仨仡仫仞伛仳伢佤仵伥伧伉伫佞佧攸佚佝",
    "貮貯貰貱貲貳貴貵貶買貸貹貺費貼貽貾貿賀賁賂賃賄賅賆資賈賉賊賋賌賍賎賏賐賑賒賓賔賕賖賗賘賙賚賛賜賝賞賟賠賡賢賣賤賥賦賧賨賩質賫賬賭賮賯賰賱賲賳賴賵賶賷賸賹賺賻購賽賾賿贀贁贂贃贄贅贆贇贈贉贊贋贌贍佟佗伲伽佶佴侑侉侃侏佾佻侪佼侬侔俦俨俪俅俚俣俜俑俟俸倩偌俳倬倏倮倭俾倜倌倥倨偾偃偕偈偎偬偻傥傧傩傺僖儆僭僬僦僮儇儋仝氽佘佥俎龠汆籴兮巽黉馘冁夔勹匍訇匐凫夙兕亠兖亳衮袤亵脔裒禀嬴蠃羸冫冱冽冼",
    "贎贏贐贑贒贓贔贕贖贗贘贙贚贛贜贠赑赒赗赟赥赨赩赪赬赮赯赱赲赸赹赺赻赼赽赾赿趀趂趃趆趇趈趉趌趍趎趏趐趒趓趕趖趗趘趙趚趛趜趝趞趠趡趢趤趥趦趧趨趩趪趫趬趭趮趯趰趲趶趷趹趻趽跀跁跂跅跇跈跉跊跍跐跒跓跔凇冖冢冥讠讦讧讪讴讵讷诂诃诋诏诎诒诓诔诖诘诙诜诟诠诤诨诩诮诰诳诶诹诼诿谀谂谄谇谌谏谑谒谔谕谖谙谛谘谝谟谠谡谥谧谪谫谮谯谲谳谵谶卩卺阝阢阡阱阪阽阼陂陉陔陟陧陬陲陴隈隍隗隰邗邛邝邙邬邡邴邳邶邺",
    "跕跘跙跜跠跡跢跥跦跧跩跭跮跰跱跲跴跶跼跾跿踀踁踂踃踄踆踇踈踋踍踎踐踑踒踓踕踖踗踘踙踚踛踜踠踡踤踥踦踧踨踫踭踰踲踳踴踶踷踸踻踼踾踿蹃蹅蹆蹌蹍蹎蹏蹐蹓蹔蹕蹖蹗蹘蹚蹛蹜蹝蹞蹟蹠蹡蹢蹣蹤蹥蹧蹨蹪蹫蹮蹱邸邰郏郅邾郐郄郇郓郦郢郜郗郛郫郯郾鄄鄢鄞鄣鄱鄯鄹酃酆刍奂劢劬劭劾哿勐勖勰叟燮矍廴凵凼鬯厶弁畚巯坌垩垡塾墼壅壑圩圬圪圳圹圮圯坜圻坂坩垅坫垆坼坻坨坭坶坳垭垤垌垲埏垧垴垓垠埕埘埚埙埒垸埴埯埸埤埝",
    "蹳蹵蹷蹸蹹蹺蹻蹽蹾躀躂躃躄躆躈躉躊躋躌躍躎躑躒躓躕躖躗躘躙躚躛躝躟躠躡躢躣躤躥躦躧躨躩躪躭躮躰躱躳躴躵躶躷躸躹躻躼躽躾躿軀軁軂軃軄軅軆軇軈軉車軋軌軍軏軐軑軒軓軔軕軖軗軘軙軚軛軜軝軞軟軠軡転軣軤堋堍埽埭堀堞堙塄堠塥塬墁墉墚墀馨鼙懿艹艽艿芏芊芨芄芎芑芗芙芫芸芾芰苈苊苣芘芷芮苋苌苁芩芴芡芪芟苄苎芤苡茉苷苤茏茇苜苴苒苘茌苻苓茑茚茆茔茕苠苕茜荑荛荜茈莒茼茴茱莛荞茯荏荇荃荟荀茗荠茭茺茳荦荥",
    "軥軦軧軨軩軪軫軬軭軮軯軰軱軲軳軴軵軶軷軸軹軺軻軼軽軾軿輀輁輂較輄輅輆輇輈載輊輋輌輍輎輏輐輑輒輓輔輕輖輗輘輙輚輛輜輝輞輟輠輡輢輣輤輥輦輧輨輩輪輫輬輭輮輯輰輱輲輳輴輵輶輷輸輹輺輻輼輽輾輿轀轁轂轃轄荨茛荩荬荪荭荮莰荸莳莴莠莪莓莜莅荼莶莩荽莸荻莘莞莨莺莼菁萁菥菘堇萘萋菝菽菖萜萸萑萆菔菟萏萃菸菹菪菅菀萦菰菡葜葑葚葙葳蒇蒈葺蒉葸萼葆葩葶蒌蒎萱葭蓁蓍蓐蓦蒽蓓蓊蒿蒺蓠蒡蒹蒴蒗蓥蓣蔌甍蔸蓰蔹蔟蔺",
    "轅轆轇轈轉轊轋轌轍轎轏轐轑轒轓轔轕轖轗轘轙轚轛轜轝轞轟轠轡轢轣轤轥轪辀辌辒辝辠辡辢辤辥辦辧辪辬辭辮辯農辳辴辵辷辸辺辻込辿迀迃迆迉迊迋迌迍迏迒迖迗迚迠迡迣迧迬迯迱迲迴迵迶迺迻迼迾迿逇逈逌逎逓逕逘蕖蔻蓿蓼蕙蕈蕨蕤蕞蕺瞢蕃蕲蕻薤薨薇薏蕹薮薜薅薹薷薰藓藁藜藿蘧蘅蘩蘖蘼廾弈夼奁耷奕奚奘匏尢尥尬尴扌扪抟抻拊拚拗拮挢拶挹捋捃掭揶捱捺掎掴捭掬掊捩掮掼揲揸揠揿揄揞揎摒揆掾摅摁搋搛搠搌搦搡摞撄摭撖",
    "這逜連逤逥逧逨逩逪逫逬逰週進逳逴逷逹逺逽逿遀遃遅遆遈遉遊運遌過達違遖遙遚遜遝遞遟遠遡遤遦遧適遪遫遬遯遰遱遲遳遶遷選遹遺遻遼遾邁還邅邆邇邉邊邌邍邎邏邐邒邔邖邘邚邜邞邟邠邤邥邧邨邩邫邭邲邷邼邽邿郀摺撷撸撙撺擀擐擗擤擢攉攥攮弋忒甙弑卟叱叽叩叨叻吒吖吆呋呒呓呔呖呃吡呗呙吣吲咂咔呷呱呤咚咛咄呶呦咝哐咭哂咴哒咧咦哓哔呲咣哕咻咿哌哙哚哜咩咪咤哝哏哞唛哧唠哽唔哳唢唣唏唑唧唪啧喏喵啉啭啁啕唿啐唼",
    "郂郃郆郈郉郋郌郍郒郔郕郖郘郙郚郞郟郠郣郤郥郩郪郬郮郰郱郲郳郵郶郷郹郺郻郼郿鄀鄁鄃鄅鄆鄇鄈鄉鄊鄋鄌鄍鄎鄏鄐鄑鄒鄓鄔鄕鄖鄗鄘鄚鄛鄜鄝鄟鄠鄡鄤鄥鄦鄧鄨鄩鄪鄫鄬鄭鄮鄰鄲鄳鄴鄵鄶鄷鄸鄺鄻鄼鄽鄾鄿酀酁酂酄唷啖啵啶啷唳唰啜喋嗒喃喱喹喈喁喟啾嗖喑啻嗟喽喾喔喙嗪嗷嗉嘟嗑嗫嗬嗔嗦嗝嗄嗯嗥嗲嗳嗌嗍嗨嗵嗤辔嘞嘈嘌嘁嘤嘣嗾嘀嘧嘭噘嘹噗嘬噍噢噙噜噌噔嚆噤噱噫噻噼嚅嚓嚯囔囗囝囡囵囫囹囿圄圊圉圜帏帙帔帑帱帻帼",
    "酅酇酈酑酓酔酕酖酘酙酛酜酟酠酦酧酨酫酭酳酺酻酼醀醁醂醃醄醆醈醊醎醏醓醔醕醖醗醘醙醜醝醞醟醠醡醤醥醦醧醨醩醫醬醰醱醲醳醶醷醸醹醻醼醽醾醿釀釁釂釃釄釅釆釈釋釐釒釓釔釕釖釗釘釙釚釛針釞釟釠釡釢釣釤釥帷幄幔幛幞幡岌屺岍岐岖岈岘岙岑岚岜岵岢岽岬岫岱岣峁岷峄峒峤峋峥崂崃崧崦崮崤崞崆崛嵘崾崴崽嵬嵛嵯嵝嵫嵋嵊嵩嵴嶂嶙嶝豳嶷巅彳彷徂徇徉後徕徙徜徨徭徵徼衢彡犭犰犴犷犸狃狁狎狍狒狨狯狩狲狴狷猁狳猃狺",
    "釦釧釨釩釪釫釬釭釮釯釰釱釲釳釴釵釶釷釸釹釺釻釼釽釾釿鈀鈁鈂鈃鈄鈅鈆鈇鈈鈉鈊鈋鈌鈍鈎鈏鈐鈑鈒鈓鈔鈕鈖鈗鈘鈙鈚鈛鈜鈝鈞鈟鈠鈡鈢鈣鈤鈥鈦鈧鈨鈩鈪鈫鈬鈭鈮鈯鈰鈱鈲鈳鈴鈵鈶鈷鈸鈹鈺鈻鈼鈽鈾鈿鉀鉁鉂鉃鉄鉅狻猗猓猡猊猞猝猕猢猹猥猬猸猱獐獍獗獠獬獯獾舛夥飧夤夂饣饧饨饩饪饫饬饴饷饽馀馄馇馊馍馐馑馓馔馕庀庑庋庖庥庠庹庵庾庳赓廒廑廛廨廪膺忄忉忖忏怃忮怄忡忤忾怅怆忪忭忸怙怵怦怛怏怍怩怫怊怿怡恸恹恻恺恂",
    "鉆鉇鉈鉉鉊鉋鉌鉍鉎鉏鉐鉑鉒鉓鉔鉕鉖鉗鉘鉙鉚鉛鉜鉝鉞鉟鉠鉡鉢鉣鉤鉥鉦鉧鉨鉩鉪鉫鉬鉭鉮鉯鉰鉱鉲鉳鉵鉶鉷鉸鉹鉺鉻鉼鉽鉾鉿銀銁銂銃銄銅銆銇銈銉銊銋銌銍銏銐銑銒銓銔銕銖銗銘銙銚銛銜銝銞銟銠銡銢銣銤銥銦銧恪恽悖悚悭悝悃悒悌悛惬悻悱惝惘惆惚悴愠愦愕愣惴愀愎愫慊慵憬憔憧憷懔懵忝隳闩闫闱闳闵闶闼闾阃阄阆阈阊阋阌阍阏阒阕阖阗阙阚丬爿戕氵汔汜汊沣沅沐沔沌汨汩汴汶沆沩泐泔沭泷泸泱泗沲泠泖泺泫泮沱泓泯泾",
    "銨銩銪銫銬銭銯銰銱銲銳銴銵銶銷銸銹銺銻銼銽銾銿鋀鋁鋂鋃鋄鋅鋆鋇鋉鋊鋋鋌鋍鋎鋏鋐鋑鋒鋓鋔鋕鋖鋗鋘鋙鋚鋛鋜鋝鋞鋟鋠鋡鋢鋣鋤鋥鋦鋧鋨鋩鋪鋫鋬鋭鋮鋯鋰鋱鋲鋳鋴鋵鋶鋷鋸鋹鋺鋻鋼鋽鋾鋿錀錁錂錃錄錅錆錇錈錉洹洧洌浃浈洇洄洙洎洫浍洮洵洚浏浒浔洳涑浯涞涠浞涓涔浜浠浼浣渚淇淅淞渎涿淠渑淦淝淙渖涫渌涮渫湮湎湫溲湟溆湓湔渲渥湄滟溱溘滠漭滢溥溧溽溻溷滗溴滏溏滂溟潢潆潇漤漕滹漯漶潋潴漪漉漩澉澍澌潸潲潼潺濑",
    "錊錋錌錍錎錏錐錑錒錓錔錕錖錗錘錙錚錛錜錝錞錟錠錡錢錣錤錥錦錧錨錩錪錫錬錭錮錯錰錱録錳錴錵錶錷錸錹錺錻錼錽錿鍀鍁鍂鍃鍄鍅鍆鍇鍈鍉鍊鍋鍌鍍鍎鍏鍐鍑鍒鍓鍔鍕鍖鍗鍘鍙鍚鍛鍜鍝鍞鍟鍠鍡鍢鍣鍤鍥鍦鍧鍨鍩鍫濉澧澹澶濂濡濮濞濠濯瀚瀣瀛瀹瀵灏灞宀宄宕宓宥宸甯骞搴寤寮褰寰蹇謇辶迓迕迥迮迤迩迦迳迨逅逄逋逦逑逍逖逡逵逶逭逯遄遑遒遐遨遘遢遛暹遴遽邂邈邃邋彐彗彖彘尻咫屐屙孱屣屦羼弪弩弭艴弼鬻屮妁妃妍妩妪妣",
    "鍬鍭鍮鍯鍰鍱鍲鍳鍴鍵鍶鍷鍸鍹鍺鍻鍼鍽鍾鍿鎀鎁鎂鎃鎄鎅鎆鎇鎈鎉鎊鎋鎌鎍鎎鎐鎑鎒鎓鎔鎕鎖鎗鎘鎙鎚鎛鎜鎝鎞鎟鎠鎡鎢鎣鎤鎥鎦鎧鎨鎩鎪鎫鎬鎭鎮鎯鎰鎱鎲鎳鎴鎵鎶鎷鎸鎹鎺鎻鎼鎽鎾鎿鏀鏁鏂鏃鏄鏅鏆鏇鏈鏉鏋鏌鏍妗姊妫妞妤姒妲妯姗妾娅娆姝娈姣姘姹娌娉娲娴娑娣娓婀婧婊婕娼婢婵胬媪媛婷婺媾嫫媲嫒嫔媸嫠嫣嫱嫖嫦嫘嫜嬉嬗嬖嬲嬷孀尕尜孚孥孳孑孓孢驵驷驸驺驿驽骀骁骅骈骊骐骒骓骖骘骛骜骝骟骠骢骣骥骧纟纡纣纥纨纩",
    "鏎鏏鏐鏑鏒鏓鏔鏕鏗鏘鏙鏚鏛鏜鏝鏞鏟鏠鏡鏢鏣鏤鏥鏦鏧鏨鏩鏪鏫鏬鏭鏮鏯鏰鏱鏲鏳鏴鏵鏶鏷鏸鏹鏺鏻鏼鏽鏾鏿鐀鐁鐂鐃鐄鐅鐆鐇鐈鐉鐊鐋鐌鐍鐎鐏鐐鐑鐒鐓鐔鐕鐖鐗鐘鐙鐚鐛鐜鐝鐞鐟鐠鐡鐢鐣鐤鐥鐦鐧鐨鐩鐪鐫鐬鐭鐮纭纰纾绀绁绂绉绋绌绐绔绗绛绠绡绨绫绮绯绱绲缍绶绺绻绾缁缂缃缇缈缋缌缏缑缒缗缙缜缛缟缡缢缣缤缥缦缧缪缫缬缭缯缰缱缲缳缵幺畿巛甾邕玎玑玮玢玟珏珂珑玷玳珀珉珈珥珙顼琊珩珧珞玺珲琏琪瑛琦琥琨琰琮琬",
    "鐯鐰鐱鐲鐳鐴鐵鐶鐷鐸鐹鐺鐻鐼鐽鐿鑀鑁鑂鑃鑄鑅鑆鑇鑈鑉鑊鑋鑌鑍鑎鑏鑐鑑鑒鑓鑔鑕鑖鑗鑘鑙鑚鑛鑜鑝鑞鑟鑠鑡鑢鑣鑤鑥鑦鑧鑨鑩鑪鑬鑭鑮鑯鑰鑱鑲鑳鑴鑵鑶鑷鑸鑹鑺鑻鑼鑽鑾鑿钀钁钂钃钄钑钖钘铇铏铓铔铚铦铻锜锠琛琚瑁瑜瑗瑕瑙瑷瑭瑾璜璎璀璁璇璋璞璨璩璐璧瓒璺韪韫韬杌杓杞杈杩枥枇杪杳枘枧杵枨枞枭枋杷杼柰栉柘栊柩枰栌柙枵柚枳柝栀柃枸柢栎柁柽栲栳桠桡桎桢桄桤梃栝桕桦桁桧桀栾桊桉栩梵梏桴桷梓桫棂楮棼椟椠棹",
    "锧锳锽镃镈镋镕镚镠镮镴镵長镸镹镺镻镼镽镾門閁閂閃閄閅閆閇閈閉閊開閌閍閎閏閐閑閒間閔閕閖閗閘閙閚閛閜閝閞閟閠閡関閣閤閥閦閧閨閩閪閫閬閭閮閯閰閱閲閳閴閵閶閷閸閹閺閻閼閽閾閿闀闁闂闃闄闅闆闇闈闉闊闋椤棰椋椁楗棣椐楱椹楠楂楝榄楫榀榘楸椴槌榇榈槎榉楦楣楹榛榧榻榫榭槔榱槁槊槟榕槠榍槿樯槭樗樘橥槲橄樾檠橐橛樵檎橹樽樨橘橼檑檐檩檗檫猷獒殁殂殇殄殒殓殍殚殛殡殪轫轭轱轲轳轵轶轸轷轹轺轼轾辁辂辄辇辋",
    "闌闍闎闏闐闑闒闓闔闕闖闗闘闙闚闛關闝闞闟闠闡闢闣闤闥闦闧闬闿阇阓阘阛阞阠阣阤阥阦阧阨阩阫阬阭阯阰阷阸阹阺阾陁陃陊陎陏陑陒陓陖陗陘陙陚陜陝陞陠陣陥陦陫陭陮陯陰陱陳陸陹険陻陼陽陾陿隀隁隂隃隄隇隉隊辍辎辏辘辚軎戋戗戛戟戢戡戥戤戬臧瓯瓴瓿甏甑甓攴旮旯旰昊昙杲昃昕昀炅曷昝昴昱昶昵耆晟晔晁晏晖晡晗晷暄暌暧暝暾曛曜曦曩贲贳贶贻贽赀赅赆赈赉赇赍赕赙觇觊觋觌觎觏觐觑牮犟牝牦牯牾牿犄犋犍犏犒挈挲掰",
    "隌階隑隒隓隕隖隚際隝隞隟隠隡隢隣隤隥隦隨隩險隫隬隭隮隯隱隲隴隵隷隸隺隻隿雂雃雈雊雋雐雑雓雔雖雗雘雙雚雛雜雝雞雟雡離難雤雥雦雧雫雬雭雮雰雱雲雴雵雸雺電雼雽雿霂霃霅霊霋霌霐霑霒霔霕霗霘霙霚霛霝霟霠搿擘耄毪毳毽毵毹氅氇氆氍氕氘氙氚氡氩氤氪氲攵敕敫牍牒牖爰虢刖肟肜肓肼朊肽肱肫肭肴肷胧胨胩胪胛胂胄胙胍胗朐胝胫胱胴胭脍脎胲胼朕脒豚脶脞脬脘脲腈腌腓腴腙腚腱腠腩腼腽腭腧塍媵膈膂膑滕膣膪臌朦臊膻",
    "霡霢霣霤霥霦霧霨霩霫霬霮霯霱霳霴霵霶霷霺霻霼霽霿靀靁靂靃靄靅靆靇靈靉靊靋靌靍靎靏靐靑靔靕靗靘靚靜靝靟靣靤靦靧靨靪靫靬靭靮靯靰靱靲靵靷靸靹靺靻靽靾靿鞀鞁鞂鞃鞄鞆鞇鞈鞉鞊鞌鞎鞏鞐鞓鞕鞖鞗鞙鞚鞛鞜鞝臁膦欤欷欹歃歆歙飑飒飓飕飙飚殳彀毂觳斐齑斓於旆旄旃旌旎旒旖炀炜炖炝炻烀炷炫炱烨烊焐焓焖焯焱煳煜煨煅煲煊煸煺熘熳熵熨熠燠燔燧燹爝爨灬焘煦熹戾戽扃扈扉礻祀祆祉祛祜祓祚祢祗祠祯祧祺禅禊禚禧禳忑忐",
    "鞞鞟鞡鞢鞤鞥鞦鞧鞨鞩鞪鞬鞮鞰鞱鞳鞵鞶鞷鞸鞹鞺鞻鞼鞽鞾鞿韀韁韂韃韄韅韆韇韈韉韊韋韌韍韎韏韐韑韒韓韔韕韖韗韘韙韚韛韜韝韞韟韠韡韢韣韤韥韨韮韯韰韱韲韴韷韸韹韺韻韼韽韾響頀頁頂頃頄項順頇須頉頊頋頌頍頎怼恝恚恧恁恙恣悫愆愍慝憩憝懋懑戆肀聿沓泶淼矶矸砀砉砗砘砑斫砭砜砝砹砺砻砟砼砥砬砣砩硎硭硖硗砦硐硇硌硪碛碓碚碇碜碡碣碲碹碥磔磙磉磬磲礅磴礓礤礞礴龛黹黻黼盱眄眍盹眇眈眚眢眙眭眦眵眸睐睑睇睃睚睨",
    "頏預頑頒頓頔頕頖頗領頙頚頛頜頝頞頟頠頡頢頣頤頥頦頧頨頩頪頫頬頭頮頯頰頱頲頳頴頵頶頷頸頹頺頻頼頽頾頿顀顁顂顃顄顅顆顇顈顉顊顋題額顎顏顐顑顒顓顔顕顖顗願顙顚顛顜顝類顟顠顡顢顣顤顥顦顧顨顩顪顫顬顭顮睢睥睿瞍睽瞀瞌瞑瞟瞠瞰瞵瞽町畀畎畋畈畛畲畹疃罘罡罟詈罨罴罱罹羁罾盍盥蠲钅钆钇钋钊钌钍钏钐钔钗钕钚钛钜钣钤钫钪钭钬钯钰钲钴钶钷钸钹钺钼钽钿铄铈铉铊铋铌铍铎铐铑铒铕铖铗铙铘铛铞铟铠铢铤铥铧铨铪",
    "顯顰顱顲顳顴颋颎颒颕颙颣風颩颪颫颬颭颮颯颰颱颲颳颴颵颶颷颸颹颺颻颼颽颾颿飀飁飂飃飄飅飆飇飈飉飊飋飌飍飏飐飔飖飗飛飜飝飠飡飢飣飤飥飦飩飪飫飬飭飮飯飰飱飲飳飴飵飶飷飸飹飺飻飼飽飾飿餀餁餂餃餄餅餆餇铩铫铮铯铳铴铵铷铹铼铽铿锃锂锆锇锉锊锍锎锏锒锓锔锕锖锘锛锝锞锟锢锪锫锩锬锱锲锴锶锷锸锼锾锿镂锵镄镅镆镉镌镎镏镒镓镔镖镗镘镙镛镞镟镝镡镢镤镥镦镧镨镩镪镫镬镯镱镲镳锺矧矬雉秕秭秣秫稆嵇稃稂稞稔",
    "餈餉養餋餌餎餏餑餒餓餔餕餖餗餘餙餚餛餜餝餞餟餠餡餢餣餤餥餦餧館餩餪餫餬餭餯餰餱餲餳餴餵餶餷餸餹餺餻餼餽餾餿饀饁饂饃饄饅饆饇饈饉饊饋饌饍饎饏饐饑饒饓饖饗饘饙饚饛饜饝饞饟饠饡饢饤饦饳饸饹饻饾馂馃馉稹稷穑黏馥穰皈皎皓皙皤瓞瓠甬鸠鸢鸨鸩鸪鸫鸬鸲鸱鸶鸸鸷鸹鸺鸾鹁鹂鹄鹆鹇鹈鹉鹋鹌鹎鹑鹕鹗鹚鹛鹜鹞鹣鹦鹧鹨鹩鹪鹫鹬鹱鹭鹳疒疔疖疠疝疬疣疳疴疸痄疱疰痃痂痖痍痣痨痦痤痫痧瘃痱痼痿瘐瘀瘅瘌瘗瘊瘥瘘瘕瘙",
    "馌馎馚馛馜馝馞馟馠馡馢馣馤馦馧馩馪馫馬馭馮馯馰馱馲馳馴馵馶馷馸馹馺馻馼馽馾馿駀駁駂駃駄駅駆駇駈駉駊駋駌駍駎駏駐駑駒駓駔駕駖駗駘駙駚駛駜駝駞駟駠駡駢駣駤駥駦駧駨駩駪駫駬駭駮駯駰駱駲駳駴駵駶駷駸駹瘛瘼瘢瘠癀瘭瘰瘿瘵癃瘾瘳癍癞癔癜癖癫癯翊竦穸穹窀窆窈窕窦窠窬窨窭窳衤衩衲衽衿袂袢裆袷袼裉裢裎裣裥裱褚裼裨裾裰褡褙褓褛褊褴褫褶襁襦襻疋胥皲皴矜耒耔耖耜耠耢耥耦耧耩耨耱耋耵聃聆聍聒聩聱覃顸颀颃",
    "駺駻駼駽駾駿騀騁騂騃騄騅騆騇騈騉騊騋騌騍騎騏騐騑騒験騔騕騖騗騘騙騚騛騜騝騞騟騠騡騢騣騤騥騦騧騨騩騪騫騬騭騮騯騰騱騲騳騴騵騶騷騸騹騺騻騼騽騾騿驀驁驂驃驄驅驆驇驈驉驊驋驌驍驎驏驐驑驒驓驔驕驖驗驘驙颉颌颍颏颔颚颛颞颟颡颢颥颦虍虔虬虮虿虺虼虻蚨蚍蚋蚬蚝蚧蚣蚪蚓蚩蚶蛄蚵蛎蚰蚺蚱蚯蛉蛏蚴蛩蛱蛲蛭蛳蛐蜓蛞蛴蛟蛘蛑蜃蜇蛸蜈蜊蜍蜉蜣蜻蜞蜥蜮蜚蜾蝈蜴蜱蜩蜷蜿螂蜢蝽蝾蝻蝠蝰蝌蝮螋蝓蝣蝼蝤蝙蝥螓螯螨蟒",
    "驚驛驜驝驞驟驠驡驢驣驤驥驦驧驨驩驪驫驲骃骉骍骎骔骕骙骦骩骪骫骬骭骮骯骲骳骴骵骹骻骽骾骿髃髄髆髇髈髉髊髍髎髏髐髒體髕髖髗髙髚髛髜髝髞髠髢髣髤髥髧髨髩髪髬髮髰髱髲髳髴髵髶髷髸髺髼髽髾髿鬀鬁鬂鬄鬅鬆蟆螈螅螭螗螃螫蟥螬螵螳蟋蟓螽蟑蟀蟊蟛蟪蟠蟮蠖蠓蟾蠊蠛蠡蠹蠼缶罂罄罅舐竺竽笈笃笄笕笊笫笏筇笸笪笙笮笱笠笥笤笳笾笞筘筚筅筵筌筝筠筮筻筢筲筱箐箦箧箸箬箝箨箅箪箜箢箫箴篑篁篌篝篚篥篦篪簌篾篼簏簖簋",
    "鬇鬉鬊鬋鬌鬍鬎鬐鬑鬒鬔鬕鬖鬗鬘鬙鬚鬛鬜鬝鬞鬠鬡鬢鬤鬥鬦鬧鬨鬩鬪鬫鬬鬭鬮鬰鬱鬳鬴鬵鬶鬷鬸鬹鬺鬽鬾鬿魀魆魊魋魌魎魐魒魓魕魖魗魘魙魚魛魜魝魞魟魠魡魢魣魤魥魦魧魨魩魪魫魬魭魮魯魰魱魲魳魴魵魶魷魸魹魺魻簟簪簦簸籁籀臾舁舂舄臬衄舡舢舣舭舯舨舫舸舻舳舴舾艄艉艋艏艚艟艨衾袅袈裘裟襞羝羟羧羯羰羲籼敉粑粝粜粞粢粲粼粽糁糇糌糍糈糅糗糨艮暨羿翎翕翥翡翦翩翮翳糸絷綦綮繇纛麸麴赳趄趔趑趱赧赭豇豉酊酐酎酏酤",
    "魼魽魾魿鮀鮁鮂鮃鮄鮅鮆鮇鮈鮉鮊鮋鮌鮍鮎鮏鮐鮑鮒鮓鮔鮕鮖鮗鮘鮙鮚鮛鮜鮝鮞鮟鮠鮡鮢鮣鮤鮥鮦鮧鮨鮩鮪鮫鮬鮭鮮鮯鮰鮱鮲鮳鮴鮵鮶鮷鮸鮹鮺鮻鮼鮽鮾鮿鯀鯁鯂鯃鯄鯅鯆鯇鯈鯉鯊鯋鯌鯍鯎鯏鯐鯑鯒鯓鯔鯕鯖鯗鯘鯙鯚鯛酢酡酰酩酯酽酾酲酴酹醌醅醐醍醑醢醣醪醭醮醯醵醴醺豕鹾趸跫踅蹙蹩趵趿趼趺跄跖跗跚跞跎跏跛跆跬跷跸跣跹跻跤踉跽踔踝踟踬踮踣踯踺蹀踹踵踽踱蹉蹁蹂蹑蹒蹊蹰蹶蹼蹯蹴躅躏躔躐躜躞豸貂貊貅貘貔斛觖觞觚觜",
    "鯜鯝鯞鯟鯠鯡鯢鯣鯤鯥鯦鯧鯨鯩鯪鯫鯬鯭鯮鯯鯰鯱鯲鯳鯴鯵鯶鯷鯸鯹鯺鯻鯼鯽鯾鯿鰀鰁鰂鰃鰄鰅鰆鰇鰈鰉鰊鰋鰌鰍鰎鰏鰐鰑鰒鰓鰔鰕鰖鰗鰘鰙鰚鰛鰜鰝鰞鰟鰠鰡鰢鰣鰤鰥鰦鰧鰨鰩鰪鰫鰬鰭鰮鰯鰰鰱鰲鰳鰴鰵鰶鰷鰸鰹鰺鰻觥觫觯訾謦靓雩雳雯霆霁霈霏霎霪霭霰霾龀龃龅龆龇龈龉龊龌黾鼋鼍隹隼隽雎雒瞿雠銎銮鋈錾鍪鏊鎏鐾鑫鱿鲂鲅鲆鲇鲈稣鲋鲎鲐鲑鲒鲔鲕鲚鲛鲞鲟鲠鲡鲢鲣鲥鲦鲧鲨鲩鲫鲭鲮鲰鲱鲲鲳鲴鲵鲶鲷鲺鲻鲼鲽鳄鳅鳆鳇鳊鳋",
    "鰼鰽鰾鰿鱀鱁鱂鱃鱄鱅鱆鱇鱈鱉鱊鱋鱌鱍鱎鱏鱐鱑鱒鱓鱔鱕鱖鱗鱘鱙鱚鱛鱜鱝鱞鱟鱠鱡鱢鱣鱤鱥鱦鱧鱨鱩鱪鱫鱬鱭鱮鱯鱰鱱鱲鱳鱴鱵鱶鱷鱸鱹鱺鱻鱽鱾鲀鲃鲄鲉鲊鲌鲏鲓鲖鲗鲘鲙鲝鲪鲬鲯鲹鲾鲿鳀鳁鳂鳈鳉鳑鳒鳚鳛鳠鳡鳌鳍鳎鳏鳐鳓鳔鳕鳗鳘鳙鳜鳝鳟鳢靼鞅鞑鞒鞔鞯鞫鞣鞲鞴骱骰骷鹘骶骺骼髁髀髅髂髋髌髑魅魃魇魉魈魍魑飨餍餮饕饔髟髡髦髯髫髻髭髹鬈鬏鬓鬟鬣麽麾縻麂麇麈麋麒鏖麝麟黛黜黝黠黟黢黩黧黥黪黯鼢鼬鼯鼹鼷鼽鼾齄",
    "鳣鳤鳥鳦鳧鳨鳩鳪鳫鳬鳭鳮鳯鳰鳱鳲鳳鳴鳵鳶鳷鳸鳹鳺鳻鳼鳽鳾鳿鴀鴁鴂鴃鴄鴅鴆鴇鴈鴉鴊鴋鴌鴍鴎鴏鴐鴑鴒鴓鴔鴕鴖鴗鴘鴙鴚鴛鴜鴝鴞鴟鴠鴡鴢鴣鴤鴥鴦鴧鴨鴩鴪鴫鴬鴭鴮鴯鴰鴱鴲鴳鴴鴵鴶鴷鴸鴹鴺鴻鴼鴽鴾鴿鵀鵁鵂",
    "鵃鵄鵅鵆鵇鵈鵉鵊鵋鵌鵍鵎鵏鵐鵑鵒鵓鵔鵕鵖鵗鵘鵙鵚鵛鵜鵝鵞鵟鵠鵡鵢鵣鵤鵥鵦鵧鵨鵩鵪鵫鵬鵭鵮鵯鵰鵱鵲鵳鵴鵵鵶鵷鵸鵹鵺鵻鵼鵽鵾鵿鶀鶁鶂鶃鶄鶅鶆鶇鶈鶉鶊鶋鶌鶍鶎鶏鶐鶑鶒鶓鶔鶕鶖鶗鶘鶙鶚鶛鶜鶝鶞鶟鶠鶡鶢",
    "鶣鶤鶥鶦鶧鶨鶩鶪鶫鶬鶭鶮鶯鶰鶱鶲鶳鶴鶵鶶鶷鶸鶹鶺鶻鶼鶽鶾鶿鷀鷁鷂鷃鷄鷅鷆鷇鷈鷉鷊鷋鷌鷍鷎鷏鷐鷑鷒鷓鷔鷕鷖鷗鷘鷙鷚鷛鷜鷝鷞鷟鷠鷡鷢鷣鷤鷥鷦鷧鷨鷩鷪鷫鷬鷭鷮鷯鷰鷱鷲鷳鷴鷵鷶鷷鷸鷹鷺鷻鷼鷽鷾鷿鸀鸁鸂",
    "鸃鸄鸅鸆鸇鸈鸉鸊鸋鸌鸍鸎鸏鸐鸑鸒鸓鸔鸕鸖鸗鸘鸙鸚鸛鸜鸝鸞鸤鸧鸮鸰鸴鸻鸼鹀鹍鹐鹒鹓鹔鹖鹙鹝鹟鹠鹡鹢鹥鹮鹯鹲鹴鹵鹶鹷鹸鹹鹺鹻鹼鹽麀麁麃麄麅麆麉麊麌麍麎麏麐麑麔麕麖麗麘麙麚麛麜麞麠麡麢麣麤麥麧麨麩麪",
    "麫麬麭麮麯麰麱麲麳麵麶麷麹麺麼麿黀黁黂黃黅黆黇黈黊黋黌黐黒黓黕黖黗黙黚點黡黣黤黦黨黫黬黭黮黰黱黲黳黴黵黶黷黸黺黽黿鼀鼁鼂鼃鼄鼅鼆鼇鼈鼉鼊鼌鼏鼑鼒鼔鼕鼖鼘鼚鼛鼜鼝鼞鼟鼡鼣鼤鼥鼦鼧鼨鼩鼪鼫鼭鼮鼰鼱",
    "鼲鼳鼴鼵鼶鼸鼺鼼鼿齀齁齂齃齅齆齇齈齉齊齋齌齍齎齏齒齓齔齕齖齗齘齙齚齛齜齝齞齟齠齡齢齣齤齥齦齧齨齩齪齫齬齭齮齯齰齱齲齳齴齵齶齷齸齹齺齻齼齽齾龁龂龍龎龏龐龑龒龓龔龕龖龗龘龜龝龞龡龢龣龤龥郎凉秊裏隣",
    "兀嗀﨎﨏﨑﨓﨔礼﨟蘒﨡﨣﨤﨧﨨﨩⺁⺄㑳㑇⺈⺋龴㖞㘚㘎⺌⺗㥮㤘龵㧏㧟㩳㧐龶龷㭎㱮㳠⺧龸⺪䁖䅟⺮䌷⺳⺶⺷䎱䎬⺻䏝䓖䙡䙌龹䜣䜩䝼䞍⻊䥇䥺䥽䦂䦃䦅䦆䦟䦛䦷䦶龺䲣䲟䲠䲡䱷䲢䴓䴔䴕䴖䴗䴘䴙䶮龻",
];

/// GB18030 四字节区:指针 → 码位的线性区间 `(p_start, cp_start, len)`,
/// 满足 `cp = cp_start + (p - p_start)`,p ∈ [p_start, p_start + len);
/// 指针定义见 `decode_gb18030`。块外指针 → U+FFFD。
#[rustfmt::skip]
const GB18030_4BYTE_RANGES: [(u32, u32, u32); 209] = [
    (0, 128, 36), (36, 165, 2), (38, 169, 7),
    (45, 178, 5), (50, 184, 31), (81, 216, 8),
    (89, 226, 6), (95, 235, 1), (96, 238, 4),
    (100, 244, 3), (103, 248, 1), (104, 251, 1),
    (105, 253, 4), (109, 258, 17), (126, 276, 7),
    (133, 284, 15), (148, 300, 24), (172, 325, 3),
    (175, 329, 4), (179, 334, 29), (208, 364, 98),
    (306, 463, 1), (307, 465, 1), (308, 467, 1),
    (309, 469, 1), (310, 471, 1), (311, 473, 1),
    (312, 475, 1), (313, 477, 28), (341, 506, 87),
    (428, 594, 15), (443, 610, 101), (544, 712, 1),
    (545, 716, 13), (558, 730, 183), (741, 930, 1),
    (742, 938, 7), (749, 962, 1), (750, 970, 55),
    (805, 1026, 14), (819, 1104, 1), (820, 1106, 6637),
    (7457, 59335, 1), (7458, 7744, 464), (7922, 8209, 2),
    (7924, 8215, 1), (7925, 8218, 2), (7927, 8222, 7),
    (7934, 8231, 9), (7943, 8241, 1), (7944, 8244, 1),
    (7945, 8246, 5), (7950, 8252, 112), (8062, 8365, 86),
    (8148, 8452, 1), (8149, 8454, 3), (8152, 8458, 12),
    (8164, 8471, 10), (8174, 8482, 62), (8236, 8556, 4),
    (8240, 8570, 22), (8262, 8596, 2), (8264, 8602, 110),
    (8374, 8713, 6), (8380, 8720, 1), (8381, 8722, 3),
    (8384, 8726, 4), (8388, 8731, 2), (8390, 8737, 2),
    (8392, 8740, 1), (8393, 8742, 1), (8394, 8748, 2),
    (8396, 8751, 5), (8401, 8760, 5), (8406, 8766, 10),
    (8416, 8777, 3), (8419, 8781, 5), (8424, 8787, 13),
    (8437, 8802, 2), (8439, 8808, 6), (8445, 8816, 37),
    (8482, 8854, 3), (8485, 8858, 11), (8496, 8870, 25),
    (8521, 8896, 82), (8603, 8979, 333), (8936, 9322, 10),
    (8946, 9372, 100), (9046, 9548, 4), (9050, 9588, 13),
    (9063, 9616, 3), (9066, 9622, 10), (9076, 9634, 16),
    (9092, 9652, 8), (9100, 9662, 8), (9108, 9672, 3),
    (9111, 9676, 2), (9113, 9680, 18), (9131, 9702, 31),
    (9162, 9735, 2), (9164, 9738, 54), (9218, 9793, 1),
    (9219, 9795, 2110), (11329, 11906, 2), (11331, 11909, 3),
    (11334, 11913, 2), (11336, 11917, 10), (11346, 11928, 15),
    (11361, 11944, 2), (11363, 11947, 3), (11366, 11951, 4),
    (11370, 11956, 2), (11372, 11960, 3), (11375, 11964, 14),
    (11389, 11979, 293), (11682, 12284, 4), (11686, 12292, 1),
    (11687, 12312, 5), (11692, 12319, 2), (11694, 12330, 20),
    (11714, 12351, 2), (11716, 12436, 7), (11723, 12447, 2),
    (11725, 12535, 5), (11730, 12543, 6), (11736, 12586, 246),
    (11982, 12842, 7), (11989, 12850, 113), (12102, 12964, 234),
    (12336, 13200, 12), (12348, 13215, 2), (12350, 13218, 34),
    (12384, 13253, 9), (12393, 13263, 2), (12395, 13267, 2),
    (12397, 13270, 113), (12510, 13384, 43), (12553, 13428, 298),
    (12851, 13727, 111), (12962, 13839, 11), (12973, 13851, 765),
    (13738, 14617, 85), (13823, 14703, 96), (13919, 14801, 14),
    (13933, 14816, 147), (14080, 14964, 218), (14298, 15183, 287),
    (14585, 15471, 113), (14698, 15585, 885), (15583, 16471, 264),
    (15847, 16736, 471), (16318, 17208, 116), (16434, 17325, 4),
    (16438, 17330, 43), (16481, 17374, 248), (16729, 17623, 373),
    (17102, 17997, 20), (17122, 18018, 193), (17315, 18212, 5),
    (17320, 18218, 82), (17402, 18301, 16), (17418, 18318, 441),
    (17859, 18760, 50), (17909, 18811, 2), (17911, 18814, 4),
    (17915, 18820, 1), (17916, 18823, 20), (17936, 18844, 3),
    (17939, 18848, 22), (17961, 18872, 703), (18664, 19576, 39),
    (18703, 19620, 111), (18814, 19738, 148), (18962, 19887, 81),
    (19043, 40870, 14426), (33469, 59244, 1), (33470, 59336, 1),
    (33471, 59367, 13), (33484, 59413, 1), (33485, 59417, 5),
    (33490, 59423, 7), (33497, 59431, 4), (33501, 59437, 4),
    (33505, 59443, 8), (33513, 59452, 7), (33520, 59460, 16),
    (33536, 59478, 14), (33550, 59493, 4295), (37845, 63789, 76),
    (37921, 63866, 27), (37948, 63894, 81), (38029, 63976, 9),
    (38038, 63986, 26), (38064, 64016, 1), (38065, 64018, 1),
    (38066, 64021, 3), (38069, 64025, 6), (38075, 64034, 1),
    (38076, 64037, 2), (38078, 64042, 1030), (39108, 65074, 1),
    (39109, 65093, 4), (39113, 65107, 1), (39114, 65112, 1),
    (39115, 65127, 1), (39116, 65132, 149), (39265, 65375, 129),
    (39394, 65510, 26), (189000, 65536, 1048576),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::net::TcpListener;

    // ---- 测试脚手架 ----

    fn skin() -> MadSkin {
        MadSkin::default()
    }

    /// 独立临时目录(不依赖 tempfile crate)。
    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("dlook-web-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(bytes).unwrap();
        path
    }

    fn all_text(doc: &WebDoc) -> String {
        doc.lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// `render` 成功路径(WebDoc 未实现 Debug,故不用 unwrap)。
    fn view(r: Result<WebDoc, String>) -> WebDoc {
        match r {
            Ok(doc) => doc,
            Err(e) => panic!("expected Ok, got Err({e})"),
        }
    }

    /// `render` 失败路径。
    fn fault(r: Result<WebDoc, String>) -> String {
        match r {
            Ok(_) => panic!("expected Err, got Ok"),
            Err(e) => e,
        }
    }

    /// 一行里的前 `width` 个 cell(供链接区断言)。
    fn slice_chars(s: &str, start: usize, end: usize) -> String {
        s.chars().skip(start).take(end.saturating_sub(start)).collect()
    }

    /// 起一个只服务 `responses` 的本地 HTTP 服务(每个连接回一条响应后关闭)。
    /// 返回 `http://127.0.0.1:PORT/`。
    fn serve(responses: Vec<Vec<u8>>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let mut idx = 0usize;
            while idx < responses.len() {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut buf = [0u8; 2048];
                let _ = stream.read(&mut buf);
                if idx < responses.len() {
                    let _ = stream.write_all(&responses[idx]);
                    let _ = stream.flush();
                    idx += 1;
                }
            }
        });
        format!("http://127.0.0.1:{port}/")
    }

    fn http_response(status: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
        let mut out = format!("HTTP/1.1 {status}\r\n").into_bytes();
        for (k, v) in headers {
            out.extend_from_slice(format!("{k}: {v}\r\n").as_bytes());
        }
        out.extend_from_slice(b"Connection: close\r\n");
        out.extend_from_slice(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes());
        out.extend_from_slice(body);
        out
    }

    // ---- 1. 本地文件:标题 / 段落 / 链接绝对化 ----

    #[test]
    fn local_html_renders_title_paragraphs_and_absolute_link() {
        let dir = temp_dir("local");
        let html = r#"<html><head><title>第一 页面</title></head><body>
<h1>Hello Web</h1>
<p>First paragraph with <a href="sub/next.html">a link</a> here.</p>
<p>Second paragraph.</p>
</body></html>"#;
        let path = write_file(&dir, "page.html", html.as_bytes());

        let doc = view(render(path.to_str().unwrap(), 80, &skin()));
        assert_eq!(doc.title, "第一 页面");
        assert!(doc.final_url.starts_with("file://"), "{}", doc.final_url);

        let text = all_text(&doc);
        assert!(text.contains("Hello Web"), "{text}");
        assert!(text.contains("First paragraph"), "{text}");
        assert!(text.contains("Second paragraph"), "{text}");

        assert_eq!(doc.links.len(), 1, "{:?}", doc.links);
        let link = &doc.links[0];
        let dir_url = &doc.final_url[..doc.final_url.rfind('/').unwrap()];
        assert_eq!(link.target, format!("{dir_url}/sub/next.html"));
        // 可点区域覆盖 label(与 markdown 相同:label + " (url)")
        let row: String = doc.lines[link.line]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(slice_chars(&row, link.start, link.start + 6), "a link");
        // 可点区域 = label + " (url)":列 end 紧跟 ")",label 与后缀属同一链接
        assert_eq!(slice_chars(&row, link.end - 1, link.end), ")");
        assert!(slice_chars(&row, link.start + 7, link.end).starts_with("(file://"), "{row}");
        assert!(all_text(&doc).contains("here."), "{row}");
    }

    /// 折行只发生在空白处(整词折行),不把词切开:
    /// width=80 时追加的 ` (url)` 后缀把该段挤宽,折行必须落在 `here.` 之前,
    /// 而不是把 `here.` 切成 `he`+`re.`(html2text 自身折行同样不切词)。
    #[test]
    fn overflow_wrap_breaks_at_word_boundary() {
        let dir = temp_dir("wrap");
        let html = r#"<html><head><title>w</title></head><body>
<p>First paragraph with <a href="sub/next.html">a link</a> here.</p>
</body></html>"#;
        let path = write_file(&dir, "w.html", html.as_bytes());
        let doc = view(render(path.to_str().unwrap(), 80, &skin()));

        let rows: Vec<String> = doc
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
            .filter(|r| !r.trim().is_empty())
            .collect();
        let dir_url = &doc.final_url[..doc.final_url.rfind('/').unwrap()];
        // 每处折行都丢掉一个分隔空白,故「行去掉首尾空白后用单个空格重接」= 原文本
        // (若某个词被硬切,重接后会在词中间多出空格,断言即失败)。
        let rejoined = rows.iter().map(|r| r.trim()).collect::<Vec<_>>().join(" ");
        assert_eq!(
            rejoined,
            format!("First paragraph with a link ({dir_url}/sub/next.html) here."),
            "{rows:?}"
        );
        for row in &rows {
            assert!(row.chars().count() <= 80, "{row:?}");
        }
    }

    /// 相对链接里的 `..`/`.` 与 base 目录语义(纯函数级)。
    #[test]
    fn resolve_url_joins_relative_targets() {
        assert_eq!(
            resolve_url("http://h/a/b/page.html", "next.html"),
            "http://h/a/b/next.html"
        );
        assert_eq!(resolve_url("http://h/a/b/page.html", "../up.html"), "http://h/a/up.html");
        assert_eq!(resolve_url("http://h/a/b/page.html", "/root.html"), "http://h/root.html");
        assert_eq!(resolve_url("http://h/a/page.html", "./x/./y.html"), "http://h/a/x/y.html");
        assert_eq!(resolve_url("http://h/a/page.html?q=1", "b.html"), "http://h/a/b.html");
        assert_eq!(resolve_url("http://h/a/page.html?q=1", "#frag"), "http://h/a/page.html?q=1#frag");
        assert_eq!(resolve_url("http://h/a/page.html", "https://other/z"), "https://other/z");
        assert_eq!(resolve_url("http://h/a/page.html", "//cdn.h/z.js"), "http://cdn.h/z.js");
        assert_eq!(resolve_url("http://h/a/page.html", "mailto:x@y.z"), "mailto:x@y.z");
        assert_eq!(
            resolve_url("file:///docs/page.html", "img/../x.md"),
            "file:///docs/x.md"
        );
    }

    // ---- 2. 表格 + pre 不炸 ----

    #[test]
    fn table_and_pre_render_without_panic() {
        let dir = temp_dir("table");
        let html = r#"<html><head><title>t</title></head><body>
<table><tr><th>Col A</th><th>Col B</th></tr><tr><td>1</td><td>2</td></tr></table>
<pre>
fn main() {
    println!("a very long preformatted line ...............................");
}
</pre>
</body></html>"#;
        let path = write_file(&dir, "t.html", html.as_bytes());
        let doc = view(render(path.to_str().unwrap(), 60, &skin()));
        assert!(!doc.lines.is_empty());
        let text = all_text(&doc);
        assert!(text.contains("Col A"), "{text}");
        assert!(text.contains("fn main()"), "{text}");
    }

    // ---- 3. 安全:img 不进 links、不请求子资源 ----

    #[test]
    fn image_source_is_never_linked_or_fetched() {
        let dir = temp_dir("img");
        // 追踪像素 + 本地图 + 包在 <a> 里的图;render 只吃 HTML 字符串,
        // 除 source 本身外没有任何网络调用(实现事实),故只需断言 links 不含图片 URL。
        let html = r#"<html><head><title>i</title></head><body>
<p>before</p>
<p><img src="http://tracker.example/pixel.gif" alt="pixel alt"></p>
<p><img src="http://tracker.example/pixel2.gif" alt=""></p>
<p><a href="/page"><img src="http://tracker.example/pixel3.gif" alt="InLink"></a></p>
<p>after <a href="/ok.html">ok</a></p>
</body></html>"#;
        let path = write_file(&dir, "i.html", html.as_bytes());
        let doc = view(render(path.to_str().unwrap(), 80, &skin()));

        for link in &doc.links {
            assert!(
                !link.target.contains("tracker.example"),
                "image URL must never become a LinkSpan: {:?}",
                link
            );
        }
        // 图片包在 <a> 里时:Image 注解优先,链接也被丢弃(防追踪像素)
        assert_eq!(doc.links.len(), 1, "{:?}", doc.links);
        assert!(doc.links[0].target.ends_with("/ok.html"));
        let text = all_text(&doc);
        assert!(text.contains("🖼 pixel alt"), "{text}");
    }

    // ---- 4. 非 UTF-8:GBK 字节 + <meta charset> 嗅探 ----

    /// 手工构造的 GBK 字节(`<meta charset="gbk">`,`中文页面`/`网页测试`/`中文网页测试`)。
    const GBK_META_HTML: &[u8] = &[
        0x3C, 0x68, 0x74, 0x6D, 0x6C, 0x3E, 0x3C, 0x68, 0x65, 0x61, 0x64, 0x3E, 0x3C, 0x6D,
        0x65, 0x74, 0x61, 0x20, 0x63, 0x68, 0x61, 0x72, 0x73, 0x65, 0x74, 0x3D, 0x22, 0x67,
        0x62, 0x6B, 0x22, 0x3E, 0x3C, 0x74, 0x69, 0x74, 0x6C, 0x65, 0x3E, 0xD6, 0xD0, 0xCE,
        0xC4, 0xD2, 0xB3, 0xC3, 0xE6, 0x3C, 0x2F, 0x74, 0x69, 0x74, 0x6C, 0x65, 0x3E, 0x3C,
        0x2F, 0x68, 0x65, 0x61, 0x64, 0x3E, 0x3C, 0x62, 0x6F, 0x64, 0x79, 0x3E, 0x3C, 0x68,
        0x31, 0x3E, 0xCD, 0xF8, 0xD2, 0xB3, 0xB2, 0xE2, 0xCA, 0xD4, 0x3C, 0x2F, 0x68, 0x31,
        0x3E, 0x3C, 0x70, 0x3E, 0xD6, 0xD0, 0xCE, 0xC4, 0xCD, 0xF8, 0xD2, 0xB3, 0xB2, 0xE2,
        0xCA, 0xD4, 0x3C, 0x2F, 0x70, 0x3E, 0x3C, 0x2F, 0x62, 0x6F, 0x64, 0x79, 0x3E, 0x3C,
        0x2F, 0x68, 0x74, 0x6D, 0x6C, 0x3E,
    ];

    #[test]
    fn gbk_bytes_are_transcoded_via_meta_sniffing() {
        let dir = temp_dir("gbk");
        let path = write_file(&dir, "gbk.html", GBK_META_HTML);
        let doc = view(render(path.to_str().unwrap(), 80, &skin()));
        assert_eq!(doc.title, "中文页面");
        let text = all_text(&doc);
        assert!(text.contains("网页测试"), "{text}");
        assert!(text.contains("中文网页测试"), "{text}");
    }

    #[test]
    fn declared_charset_wins_over_meta_and_utf8_fallback_is_lossy() {
        // Content-Type 声明优先于 <meta>
        assert!(decode_bytes(GBK_META_HTML, Some("gbk")).contains("中文网页测试"));
        // 无声明无 <meta>:UTF-8 lossy,不 panic(乱码但不炸)
        let no_meta = "<html><body><p>中文</p></body></html>".as_bytes();
        assert!(decode_bytes(no_meta, None).contains("中文"));
        // BOM 优先
        let mut bom = vec![0xEF, 0xBB, 0xBF];
        bom.extend_from_slice("中文".as_bytes());
        assert_eq!(decode_bytes(&bom, Some("gbk")), "中文");
        // UTF-16LE BOM
        let mut utf16 = vec![0xFF, 0xFE];
        for u in "中文".encode_utf16() {
            utf16.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(decode_bytes(&utf16, None), "中文");
        // windows-1252 高位
        assert_eq!(decode_bytes(&[0x80, 0x93, 0xE9], Some("windows-1252")), "€“é");
        // ISO-8859-1:逐字节映射
        assert_eq!(decode_bytes(&[0xE9, 0xDF], Some("iso-8859-1")), "éß");
    }

    /// GB18030 表:双字节(GB2312 常用字/符号/PUA)、四字节(CJK 扩展)、€(0x80)。
    #[test]
    fn gb18030_table_samples() {
        assert_eq!(decode_gb18030(&[0xB0, 0xD6]), "爸");
        assert_eq!(decode_gb18030(&[0xD6, 0xD0]), "中");
        assert_eq!(decode_gb18030(&[0xCE, 0xC4]), "文");
        assert_eq!(decode_gb18030(&[0xA1, 0xA1]), "\u{3000}");
        assert_eq!(decode_gb18030(&[0xA3, 0xAC]), "，");
        assert_eq!(decode_gb18030(&[0x80]), "€");
        assert_eq!(decode_gb18030(&[0x81, 0x40]), "丂");
        assert_eq!(decode_gb18030(&[0x95, 0x32, 0x82, 0x36]), "𠀀");
        assert_eq!(decode_gb18030(&[0xE3, 0x32, 0x9A, 0x35]), "\u{10FFFF}");
        // 非法序列:U+FFFD,且后续 ASCII 不丢
        assert_eq!(decode_gb18030(&[0x81, 0x30]), "\u{FFFD}0");
        assert_eq!(decode_gb18030(b"a\xFFb"), "a\u{FFFD}b");
    }

    /// 表结构不变量(采样测试抓不到的静默错位):
    /// 双字节平面恰好 126×190、映射数固定;四字节区间严格递增互不重叠(二分查找前提)。
    /// 生成数据被改坏(某行少一个字符 → 该行之后整体错位)时此测试立即失败。
    #[test]
    fn gb18030_table_shape_is_intact() {
        assert_eq!(GBK_ROWS.len(), 126);
        for (i, row) in GBK_ROWS.iter().enumerate() {
            assert_eq!(row.chars().count(), 190, "GBK_ROWS[{i}] 行宽异常");
        }
        let assigned: usize = GBK_ROWS
            .iter()
            .map(|r| r.chars().filter(|c| *c != '~').count())
            .sum();
        // 现表 23,940 个双字节码位全部有映射(GB18030 把规范里的「未分配」码位也映射到
        // PUA,故没有 `~` 占位)。数字变化 = 表被改动,须重新逐序列对 encoding_rs 校验。
        assert_eq!(assigned, 23_940, "双字节平面映射数变化");
        assert_eq!(GB18030_4BYTE_RANGES.len(), 209);
        let mut end = 0u32;
        for (i, (start, _code, len)) in GB18030_4BYTE_RANGES.iter().enumerate() {
            assert!(*start >= end, "4 字节区间 [{i}] 与前驱重叠");
            end = *start + *len;
        }
    }

    // ---- 5. 超限 / 空 / 连接拒绝 ----

    #[test]
    fn oversized_empty_and_refused_sources_error_readably() {
        let dir = temp_dir("errs");
        // 17MB 稀疏文件(set_len 不写数据)
        let big = write_file(&dir, "big.html", b"<html></html>");
        let f = std::fs::OpenOptions::new().write(true).open(&big).unwrap();
        f.set_len(17 * 1024 * 1024).unwrap();
        drop(f);
        let err = fault(render(big.to_str().unwrap(), 80, &skin()));
        assert!(err.contains("too large"), "{err}");

        // 空文件
        let empty = write_file(&dir, "empty.html", b"");
        let err = fault(render(empty.to_str().unwrap(), 80, &skin()));
        assert!(err.contains("empty file"), "{err}");

        // 缺文件 / 非网页扩展名
        let err = fault(render(dir.join("nope.html").to_str().unwrap(), 80, &skin()));
        assert!(err.contains("not found"), "{err}");
        let txt = write_file(&dir, "notes.txt", b"hello");
        let err = fault(render(txt.to_str().unwrap(), 80, &skin()));
        assert!(err.contains("not a web page"), "{err}");

        // 连接拒绝(127.0.0.1:1 必然拒绝)
        let err = fault(render("http://127.0.0.1:1/", 80, &skin()));
        assert!(err.contains("fetch failed"), "{err}");

        // 声明 Content-Length 超限:读头即拒,不读 body
        let base = serve(vec![http_response(
            "200 OK",
            &[("Content-Type", "text/html"), ("Content-Length", "20000000")],
            b"<html><body>x</body></html>",
        )]);
        let err = fault(render(&base, 80, &skin()));
        assert!(err.contains("too large"), "{err}");
    }

    // ---- 6. 宽度约束 ----

    #[test]
    fn every_line_fits_width() {
        let dir = temp_dir("width");
        let long_url = "https://example.com/".to_string() + &"very-long-path-segment/".repeat(6);
        let html = format!(
            r#"<html><head><title>w</title></head><body>
<h1>Heading with some words in it</h1>
<p>中文段落也需要正确折行,不能超过宽度限制,否则终端会截断或换行错乱。这段文字足够长,应该被折成多行。</p>
<p>An ascii paragraph that is definitely longer than the narrower test width and must wrap.</p>
<p><a href="{long_url}">long link</a> and <strong>bold text</strong> and <code>inline_code()</code>.</p>
<p>中文段落里带<a href="/中文/路径/一个很长的链接地址.html">链接</a>时也必须按列宽折行,不能溢出。</p>
<pre>preformatted line that is really quite long and should also be wrapped when narrow.</pre>
</body></html>"#
        );
        let path = write_file(&dir, "w.html", html.as_bytes());
        for width in [40u16, 120] {
            let doc = view(render(path.to_str().unwrap(), width, &skin()));
            for (i, line) in doc.lines.iter().enumerate() {
                let chars = line.spans.iter().map(|s| s.content.chars().count()).sum::<usize>();
                assert!(
                    chars <= width as usize,
                    "width={width} line {i} has {chars} chars: {line:?}"
                );
                assert!(
                    line_cells(line) <= width as usize,
                    "width={width} line {i} is {} cells",
                    line_cells(line)
                );
            }
            // LinkSpan 列必须落在行内
            for link in &doc.links {
                assert!(link.line < doc.lines.len());
                let row_chars: usize = doc.lines[link.line]
                    .spans
                    .iter()
                    .map(|s| s.content.chars().count())
                    .sum();
                assert!(link.end <= row_chars, "{link:?} row has {row_chars} chars");
                assert!(link.start < link.end);
            }
        }
    }

    // ---- 7. 远程:重定向后 base = 最终 URL + Content-Type charset ----

    /// 远程 GBK 页面(`<meta charset="gbk">`,`中文页面`/`网页测试`/`中文网页测试`)。
    const GBK_REMOTE_BODY: &[u8] = &[
        0x3C, 0x68, 0x74, 0x6D, 0x6C, 0x3E, 0x3C, 0x68, 0x65, 0x61, 0x64, 0x3E, 0x3C, 0x74,
        0x69, 0x74, 0x6C, 0x65, 0x3E, 0x72, 0x65, 0x6D, 0x6F, 0x74, 0x65, 0x3C, 0x2F, 0x74,
        0x69, 0x74, 0x6C, 0x65, 0x3E, 0x3C, 0x2F, 0x68, 0x65, 0x61, 0x64, 0x3E, 0x3C, 0x62,
        0x6F, 0x64, 0x79, 0x3E, 0x3C, 0x70, 0x3E, 0xD6, 0xD0, 0xCE, 0xC4, 0xCD, 0xF8, 0xD2,
        0xB3, 0xB2, 0xE2, 0xCA, 0xD4, 0x3C, 0x2F, 0x70, 0x3E, 0x3C, 0x70, 0x3E, 0x3C, 0x61,
        0x20, 0x68, 0x72, 0x65, 0x66, 0x3D, 0x22, 0x72, 0x65, 0x6C, 0x2E, 0x68, 0x74, 0x6D,
        0x6C, 0x22, 0x3E, 0xCF, 0xC2, 0xD2, 0xBB, 0xD2, 0xB3, 0x3C, 0x2F, 0x61, 0x3E, 0x3C,
        0x2F, 0x70, 0x3E, 0x3C, 0x2F, 0x62, 0x6F, 0x64, 0x79, 0x3E, 0x3C, 0x2F, 0x68, 0x74,
        0x6D, 0x6C, 0x3E,
    ];

    #[test]
    fn remote_redirect_uses_final_url_as_base_and_honours_charset_header() {
        let base = serve(vec![
            http_response("302 Found", &[("Location", "/final.html")], b""),
            http_response(
                "200 OK",
                &[("Content-Type", "text/html; charset=gbk")],
                GBK_REMOTE_BODY,
            ),
        ]);
        let doc = view(render(&base, 80, &skin()));
        assert!(doc.final_url.ends_with("/final.html"), "{}", doc.final_url);
        assert_eq!(doc.title, "remote");
        let text = all_text(&doc);
        assert!(text.contains("中文网页测试"), "{text}");
        // 相对链接按「重定向后的最终 URL」解析
        assert_eq!(doc.links.len(), 1, "{:?}", doc.links);
        assert!(doc.links[0].target.ends_with("/rel.html"), "{:?}", doc.links[0]);
        assert!(!doc.links[0].target.contains("final.html"), "{:?}", doc.links[0]);
    }

    #[test]
    fn remote_http_error_and_non_html_content_are_rejected() {
        let base = serve(vec![http_response("404 Not Found", &[("Content-Type", "text/html")], b"x")]);
        let err = fault(render(&base, 80, &skin()));
        assert!(err.contains("http 404"), "{err}");

        let base = serve(vec![http_response(
            "200 OK",
            &[("Content-Type", "image/png")],
            b"\x89PNG",
        )]);
        let err = fault(render(&base, 80, &skin()));
        assert!(err.contains("not a web page"), "{err}");
    }
}
