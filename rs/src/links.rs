//! 行内链接的数据模型与目标解析(DECISIONS D14)。
//!
//! 坐标模型:与 selection.rs 一致的**内容坐标**——
//! `LinkSpan.line` 为 doc.lines 行索引,`start`/`end` 为行内字符列([start, end) 左闭右开),
//! 滚动期间稳定;resize/热重载重建 lines 时由调用方整体替换。
//!
//! 目标分类(点击时解析,渲染期只按语法粗分是否本地链接):
//!   - `#anchor` → Anchor(暂不支持跳转)
//!   - `scheme:` → External(http/https/mailto/ftp 等,交系统打开器)
//!   - `file://` → Local(剥掉 scheme)
//!   - 其余 → Local(相对当前文件目录解析,percent-decode,fold `.`/`..`)

use std::path::{Component, Path, PathBuf};

/// 一个可点击链接在渲染后文档中的位置与原始目标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkSpan {
    /// doc.lines 行索引。
    pub line: usize,
    /// 可点击区域起始列(含)。
    pub start: usize,
    /// 可点击区域结束列(不含)。
    pub end: usize,
    /// 原始链接目标(未解码,点击时再解析)。
    pub target: String,
}

/// 点击时解析出的目标类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// 本地文件(已按当前文件目录解析、解码、规整)。
    Local(PathBuf),
    /// 外部 URL(http/https/mailto/…,交系统打开器)。
    External(String),
    /// 文档内锚点(`#…`,暂不支持)。
    Anchor,
    /// 无法解析(空目标等)。
    Invalid,
}

/// 渲染期粗判:目标是否「本地文件链接」(无 scheme、非锚点,或 file://)。
/// 决定是否给 label 加 ↗ 标记;真正能否打开在点击时校验。
pub fn is_local_target(raw: &str) -> bool {
    let t = raw.trim();
    if t.is_empty() || t.starts_with('#') {
        return false;
    }
    if let Some(rest) = t.strip_prefix("file://") {
        return !rest.is_empty();
    }
    !has_scheme(t)
}

/// 点击时解析链接目标。
/// `base_dir` 为当前文件所在目录,相对路径以它为基准。
pub fn classify(base_dir: &Path, raw: &str) -> Target {
    let t = raw.trim();
    if t.is_empty() {
        return Target::Invalid;
    }
    if let Some(rest) = t.strip_prefix("file://") {
        return Target::Local(normalize(base_dir, rest));
    }
    if has_scheme(t) {
        return Target::External(t.to_string());
    }
    if t.starts_with('#') {
        return Target::Anchor;
    }
    Target::Local(normalize(base_dir, t))
}

/// `scheme:` 前缀判定:首字符字母,scheme 体内 [A-Za-z0-9+.-],后跟 `:`。
fn has_scheme(s: &str) -> bool {
    let b = s.as_bytes();
    let Some(colon) = b.iter().position(|&c| c == b':') else {
        return false;
    };
    colon > 0
        && b[0].is_ascii_alphabetic()
        && b[1..colon].iter().all(|&c| {
            c.is_ascii_alphanumeric() || c == b'+' || c == b'-' || c == b'.'
        })
}

/// 相对 base_dir 解析 + percent-decode + 折叠 `.`/`..`。
pub fn normalize(base_dir: &Path, rel: &str) -> PathBuf {
    let decoded = percent_decode(rel);
    let joined = if Path::new(&decoded).is_absolute() {
        PathBuf::from(decoded)
    } else {
        base_dir.join(decoded)
    };
    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// URL percent-decoding:`%XX` → 字节 XX;非法序列原样保留。
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = &s[i + 1..i + 3];
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> &'static Path {
        Path::new("/docs")
    }

    // ---- is_local_target(渲染期粗判)----

    #[test]
    fn local_target_classification() {
        assert!(is_local_target("./guide.md"));
        assert!(is_local_target("guide.md"));
        assert!(is_local_target("../up/x.md"));
        assert!(is_local_target("/abs/path.md"));
        assert!(is_local_target("file:///abs/x.md"));
        assert!(!is_local_target("https://example.com"));
        assert!(!is_local_target("mailto:a@b.c"));
        assert!(!is_local_target("#anchor"));
        assert!(!is_local_target("  "));
    }

    // ---- classify(点击期)----

    #[test]
    fn classify_external_and_anchor() {
        assert_eq!(
            classify(base(), "https://example.com/a?b=1"),
            Target::External("https://example.com/a?b=1".to_string())
        );
        assert_eq!(
            classify(base(), "mailto:x@y.z"),
            Target::External("mailto:x@y.z".to_string())
        );
        assert_eq!(classify(base(), "#section"), Target::Anchor);
        assert_eq!(classify(base(), "  "), Target::Invalid);
    }

    #[test]
    fn classify_local_relative_and_absolute() {
        assert_eq!(
            classify(base(), "guide.md"),
            Target::Local(PathBuf::from("/docs/guide.md"))
        );
        assert_eq!(
            classify(base(), "./sub/../x.md"),
            Target::Local(PathBuf::from("/docs/x.md"))
        );
        assert_eq!(
            classify(base(), "/tmp/a.md"),
            Target::Local(PathBuf::from("/tmp/a.md"))
        );
    }

    #[test]
    fn classify_file_scheme_and_percent() {
        assert_eq!(
            classify(base(), "file:///tmp/x.md"),
            Target::Local(PathBuf::from("/tmp/x.md"))
        );
        assert_eq!(
            classify(base(), "my%20doc.md"),
            Target::Local(PathBuf::from("/docs/my doc.md"))
        );
    }

    // ---- normalize 边界 ----

    #[test]
    fn normalize_parent_at_root_noop() {
        assert_eq!(
            normalize(Path::new(""), "../x.md"),
            PathBuf::from("x.md")
        );
        // 相对路径 + 空 base → 保持相对(CWD 语义)
        assert_eq!(
            normalize(Path::new(""), "a/b.md"),
            PathBuf::from("a/b.md")
        );
    }

    #[test]
    fn percent_decode_cases() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("%e4%b8%ad"), "中");
    }
}
