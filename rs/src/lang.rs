//! 扩展名 → 语言 ID / 模式映射。
//! .md/.markdown → Markdown 模式；.mmd/.mermaid → Mermaid 模式；其它 → Code 模式。
//!
//! 语法集为 two-face 全量 Sublime 语法(DECISIONS D4),覆盖 TS/Vue/Svelte/TOML/
//! INI/GraphQL/Dockerfile/PowerShell/SCSS/Less/Swift/Kotlin/Dart 等;
//! 个别缺失语言由 Highlighter::find_syntax 的回退链兜底。

/// 预览模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Markdown,
    Code,
    Mermaid,
    /// 图片文件(png/jpeg/gif/webp/bmp/ico/tiff,DECISIONS D15)。
    Image,
    /// 音频文件(mp3/flac/wav/ogg/m4a/aac/opus,DECISIONS D16)。
    Audio,
    /// 视频文件(mp4/mkv/webm/mov/avi 等,DECISIONS D16,委托 mpv)。
    Video,
    /// 网页(html/htm 本地文件或 http(s) URL,DECISIONS D16 L1 文本渲染)。
    Web,
}

/// 扩展名 → syntect token(先按扩展名查,再按 token 查,见 Highlighter::find_syntax)。
fn ext_to_syntax_token(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "ts" | "mts" | "cts" => "ts",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" => "js",
        "jsx" => "jsx",
        "py" | "pyi" => "py",
        "rs" => "rs",
        "go" => "go",
        "java" => "java",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" => "cpp",
        "cs" => "cs",
        "rb" => "rb",
        "php" => "php",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "dart" => "dart",
        "scala" => "scala",
        "sh" | "bash" | "zsh" | "fish" => "sh",
        "json" | "jsonc" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "ini" | "cfg" | "conf" => "ini",
        "html" | "htm" => "html",
        "xml" => "xml",
        "css" => "css",
        "scss" => "scss",
        "less" => "less",
        "vue" => "vue",
        "svelte" => "svelte",
        "sql" => "sql",
        "graphql" | "gql" => "graphql",
        "md" | "markdown" => "md",
        "lua" => "lua",
        "r" => "R",
        "pl" | "pm" => "pl",
        "diff" | "patch" => "diff",
        "bat" | "cmd" => "bat",
        "ps1" | "ps" | "psm1" => "ps1",
        _ => return None,
    })
}

/// 特殊文件名（无扩展名但有意义的语言）→ syntect token。
fn name_to_syntax_token(name: &str) -> Option<&'static str> {
    Some(match name {
        "makefile" | "justfile" => "makefile",
        "gemfile" | "rakefile" => "rb",
        "dockerfile" => "dockerfile",
        _ => return None,
    })
}

/// 返回文件名对应的 syntect 扩展名 token（用于代码高亮查找）。
/// 仅 Code 模式有意义。
pub fn detect_syntax_token(file_name: &str) -> Option<&'static str> {
    let base = file_name.rsplit('/').next().unwrap_or(file_name);
    let lower = base.to_lowercase();
    if let Some(t) = name_to_syntax_token(&lower) {
        return Some(t);
    }
    match lower.rfind('.') {
        Some(dot) => ext_to_syntax_token(&lower[dot + 1..]),
        None => None,
    }
}

/// 是否为 markdown 扩展名（走 termimad 渲染）。
pub fn is_markdown_ext(file_name: &str) -> bool {
    let base = file_name.rsplit('/').next().unwrap_or(file_name);
    let lower = base.to_lowercase();
    match lower.rfind('.') {
        Some(dot) => {
            let ext = &lower[dot + 1..];
            ext == "md" || ext == "markdown"
        }
        None => false,
    }
}

/// 是否为 mermaid 扩展名。
pub fn is_mermaid_ext(file_name: &str) -> bool {
    let base = file_name.rsplit('/').next().unwrap_or(file_name);
    let lower = base.to_lowercase();
    match lower.rfind('.') {
        Some(dot) => {
            let ext = &lower[dot + 1..];
            ext == "mmd" || ext == "mermaid"
        }
        None => false,
    }
}

/// 是否为图片扩展名(DECISIONS D15;与 Cargo.toml 里 image crate 启用的解码器一致)。
pub fn is_image_ext(file_name: &str) -> bool {
    let base = file_name.rsplit('/').next().unwrap_or(file_name);
    let lower = base.to_lowercase();
    match lower.rfind('.') {
        Some(dot) => {
            let ext = &lower[dot + 1..];
            matches!(
                ext,
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "tiff" | "tif"
            )
        }
        None => false,
    }
}

/// 是否为音频扩展名(DECISIONS D16)。
/// 注意:opus 在 symphonia 无解码器(上游未发布),仍归入 Audio 由播放层给出明确错误。
pub fn is_audio_ext(file_name: &str) -> bool {
    let base = file_name.rsplit('/').next().unwrap_or(file_name);
    let lower = base.to_lowercase();
    match lower.rfind('.') {
        Some(dot) => {
            let ext = &lower[dot + 1..];
            matches!(
                ext,
                "mp3" | "flac" | "wav" | "ogg" | "oga" | "m4a" | "aac" | "opus"
            )
        }
        None => false,
    }
}

/// 是否为视频扩展名(DECISIONS D16,委托 mpv 播放)。
pub fn is_video_ext(file_name: &str) -> bool {
    let base = file_name.rsplit('/').next().unwrap_or(file_name);
    let lower = base.to_lowercase();
    match lower.rfind('.') {
        Some(dot) => {
            let ext = &lower[dot + 1..];
            matches!(
                ext,
                "mp4" | "mkv" | "webm" | "mov" | "avi" | "m4v" | "mpg" | "mpeg" | "ts" | "flv"
            )
        }
        None => false,
    }
}

/// 是否为网页扩展名(本地 html 文件,DECISIONS D16)。
pub fn is_web_ext(file_name: &str) -> bool {
    let base = file_name.rsplit('/').next().unwrap_or(file_name);
    let lower = base.to_lowercase();
    match lower.rfind('.') {
        Some(dot) => matches!(&lower[dot + 1..], "html" | "htm"),
        None => false,
    }
}

/// 判定预览模式 + 语法 token。
pub fn detect_mode_lang(file_name: &str) -> (Mode, Option<&'static str>) {
    // http(s) URL(D16): 网页预览,不按扩展名判定
    if file_name.starts_with("http://") || file_name.starts_with("https://") {
        return (Mode::Web, None);
    }
    if is_markdown_ext(file_name) {
        return (Mode::Markdown, None);
    }
    if is_mermaid_ext(file_name) {
        return (Mode::Mermaid, None);
    }
    if is_image_ext(file_name) {
        return (Mode::Image, None);
    }
    if is_audio_ext(file_name) {
        return (Mode::Audio, None);
    }
    if is_video_ext(file_name) {
        return (Mode::Video, None);
    }
    if is_web_ext(file_name) {
        return (Mode::Web, None);
    }
    (Mode::Code, detect_syntax_token(file_name))
}
