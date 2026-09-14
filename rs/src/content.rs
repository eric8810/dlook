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

/// 由引擎（而非 content）解析的来源：远程 URL + 音频/视频的 `file:` URL。
///
/// `file:` URL 只对「引擎自己能把 URL 还原成路径」的模式放行：音频（media.rs 的
/// `prepare_source`）与视频（mpv 原生接受 file:// URL）。图片/网页保持原来的
/// 本地路径校验语义，不改既有行为。
fn is_engine_url(mode: Mode, arg: &str) -> bool {
    if arg.starts_with("http://") || arg.starts_with("https://") {
        return true;
    }
    arg.starts_with("file:") && matches!(mode, Mode::Audio | Mode::Video)
}

pub fn load_content(file_path: &str) -> Loaded {
    let (mode, syntax_token) = detect_mode_lang(file_path);

    // 远程 URL(D16):无本地文件,字节由 web::render / AudioCtx(下载)/ ImageCtx(fetch)
    // 各自获取;此处不做任何文件系统访问。
    if file_path.starts_with("http://") || file_path.starts_with("https://") {
        return Loaded {
            file_name: file_path.to_string(),
            content: String::new(),
            mode,
            syntax_token,
        };
    }

    // 媒体的 file: URL（file:///abs、file://abs）:与 http(s) 同理由引擎处理
    // （media.rs::prepare_source 把 file: URL 还原为绝对路径），此处不做文件系统访问，
    // 否则 CLI 直开 `dlook file:///.../tone.wav` 会在入口就被判「文件不存在」。
    if is_engine_url(mode, file_path) {
        return Loaded {
            file_name: file_path.to_string(),
            content: String::new(),
            mode,
            syntax_token,
        };
    }

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

    // 媒体模式(D15/D16):不读文本、不做二进制检测;字节由各模块按需读取
    // (图片 images::ImageCtx / 音频 media::AudioCtx / 视频 video::VideoCtx / 网页 web::render)。
    if matches!(mode, Mode::Image | Mode::Audio | Mode::Video | Mode::Web) {
        return Loaded {
            file_name: file_path.to_string(),
            content: String::new(),
            mode,
            syntax_token,
        };
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 引擎自解析的来源:http(s) 与音频/视频的 file: URL 不做本地文件系统校验
    /// (否则 CLI 直开 `dlook file:///.../tone.wav` 在入口即报 not found)。
    #[test]
    fn engine_urls_skip_local_fs_check() {
        assert!(is_engine_url(Mode::Web, "https://example.com/a.html"));
        assert!(is_engine_url(Mode::Audio, "http://127.0.0.1:1/tone.wav"));
        assert!(is_engine_url(Mode::Audio, "file:///tmp/tone.wav"));
        assert!(is_engine_url(Mode::Video, "file:///tmp/clip.mp4"));
        // 图片/网页的 file: URL 不享受该放行(保持既有本地路径校验语义)
        assert!(!is_engine_url(Mode::Image, "file:///tmp/tiny.png"));
        assert!(!is_engine_url(Mode::Image, "file:///tmp/page.html"));
        // 普通路径照旧
        assert!(!is_engine_url(Mode::Audio, "audio/tone.wav"));
    }
}
