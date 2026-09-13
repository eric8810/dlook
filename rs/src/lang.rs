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

/// 判定预览模式 + 语法 token。
pub fn detect_mode_lang(file_name: &str) -> (Mode, Option<&'static str>) {
    if is_markdown_ext(file_name) {
        return (Mode::Markdown, None);
    }
    if is_mermaid_ext(file_name) {
        return (Mode::Mermaid, None);
    }
    if is_image_ext(file_name) {
        return (Mode::Image, None);
    }
    (Mode::Code, detect_syntax_token(file_name))
}
