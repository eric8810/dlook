//! 文件读取 + 二进制检测 + 模式判定。
//!
//! - 文件不存在 / 不可读 → exit 1
//! - 目录 → exit 1
//! - 二进制（前 8KB 含 \0）→ exit 1
//! - .md/.markdown → Markdown
//! - .mmd/.mermaid → Mermaid
//! - 其它 → Code

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::exit;

use crate::lang::{detect_mode_lang, Mode};

pub struct Loaded {
    pub file_name: String,
    pub content: String,
    pub mode: Mode,
    /// syntect 扩展名 token（仅 Code 模式有意义）。
    pub syntax_token: Option<&'static str>,
}

const BINARY_SAMPLE: usize = 8192;

fn fail(msg: &str) -> ! {
    let stderr = std::io::stderr();
    let mut lock = stderr.lock();
    let _ = writeln!(lock, "{}", msg);
    let _ = lock.flush();
    exit(1);
}

pub fn load_content(file_path: &str) -> Loaded {
    let meta = match fs::metadata(file_path) {
        Ok(m) => m,
        Err(_) => fail(&format!(
            "error: cannot access '{}': no such file or directory",
            file_path
        )),
    };

    if meta.is_dir() {
        fail(&format!("error: '{}' is a directory", file_path));
    }

    let bytes = match fs::read(file_path) {
        Ok(b) => b,
        Err(_) => fail(&format!(
            "error: cannot read '{}': permission denied",
            file_path
        )),
    };

    // 二进制检测：前 8KB 含 NUL 字节视为二进制
    let sample_len = bytes.len().min(BINARY_SAMPLE);
    if bytes[..sample_len].contains(&0) {
        fail(&format!("error: '{}' is a binary file, skip", file_path));
    }

    let content = String::from_utf8_lossy(&bytes).into_owned();
    let (mode, syntax_token) = detect_mode_lang(file_path);

    Loaded {
        file_name: file_path.to_string(),
        content,
        mode,
        syntax_token,
    }
}

/// 重新读取文件内容（用于文件变更后的热重载）。
/// 失败时返回 None（保留旧内容，不中断预览）。
pub fn reload_content(file_path: &str) -> Option<String> {
    let bytes = fs::read(file_path).ok()?;
    let sample_len = bytes.len().min(BINARY_SAMPLE);
    if bytes[..sample_len].contains(&0) {
        return None; // 二进制文件，跳过
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// 点击链接跳转时的读取失败原因（用于状态栏提示）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenError {
    NotFound,
    IsDir,
    Unreadable,
    Binary,
}

/// 读取一个本地文件用于导航跳转。
/// 与 load_content 不同:失败不退出进程,而是返回可提示的错误分类。
pub fn read_for_navigate(path: &Path) -> Result<String, OpenError> {
    let meta = fs::metadata(path).map_err(|_| OpenError::NotFound)?;
    if meta.is_dir() {
        return Err(OpenError::IsDir);
    }
    let bytes = fs::read(path).map_err(|_| OpenError::Unreadable)?;
    let sample_len = bytes.len().min(BINARY_SAMPLE);
    if bytes[..sample_len].contains(&0) {
        return Err(OpenError::Binary);
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
