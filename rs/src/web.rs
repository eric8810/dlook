//! 网页文本预览(DECISIONS D16,研究 L1 层):抓取 HTML → 类型化注解 → dlook 行模型。
//!
//! 设计要点(研究依据见 docs/research/media/evidence-web.md):
//!   - `render()` 阻塞(网络 IO),调用方放后台线程;沿用图片先例的限额:
//!     超时 10s、响应上限 16MB、重定向 ≤5(rustls,ureq)。
//!   - 渲染路径:`html2text::from_read_rich()` 的 `RichAnnotation`
//!     (Link/Strong/Emphasis/Code/Preformat/Image) → `Vec<Line>` + `LinkSpan`,
//!     复用 dlook 既有链接样式化/点击跳转/历史栈。
//!   - 安全(aerc 先例):**不拉取任何子资源**——`<img>`/CSS/JS 只作为注解出现,
//!     图片本体不进图片管线(防追踪像素;后续若接线需显式限额,见研究 §L1)。
//!   - 相对链接按 base(最终 URL,含重定向后)解析为绝对 URL 存入 LinkSpan.target。
//!   - 本地 .html/.htm 文件:file:// 读取,base 为文件所在目录。

use std::path::Path;

use ratatui::text::Line;
use termimad::MadSkin;

use crate::links::LinkSpan;

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
pub fn render(_source: &str, _width: u16, _skin: &MadSkin) -> Result<WebDoc, String> {
    todo!("web::render — task media-2")
}

/// 是否网页 URL(http/https)。
pub fn is_web_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// 本地 html 文件的 file:// URL(base 解析用)。
pub fn file_url(path: &Path) -> String {
    format!("file://{}", path.display())
}
