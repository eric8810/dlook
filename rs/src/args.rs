//! argv 解析：dlook <file> | --help/-h | --version/-V
//!
//! 退出码：
//!   无参 / 参数 >1 → 2
//!   --help / --version → 0（打印后直接退出）
//!   正好 1 个位置参数 → 返回该文件路径

use std::io::Write;
use std::process::exit;

/// 版本号从 Cargo.toml 派生（单一来源，避免与 crate version 脱节）。
pub const VERSION: &str = concat!("dlook ", env!("CARGO_PKG_VERSION"));

pub const HELP: &str = "\
dlook — a minimal terminal file previewer

Usage:
  dlook <file>        Preview a file: markdown / code / mermaid / image /
                      audio / video / web page (http(s) URL or local .html)
  dlook --help, -h    Show this help
  dlook --version, -V Show version

Keys (pager):
  q / Esc            Quit
  j / k              Scroll down / up one line
  Space / PageDown   Scroll down one page
  PageUp             Scroll up one page
  g / G              Go to top / bottom
  Arrow Up/Down      Scroll one line
  Home / End         Go to top / bottom
  Ctrl+C             Quit (exit 130)
  ⌫ / Alt+←          Back (previous file)
  drag / y           Copy selection (OSC 52)

Keys (media: audio/video M1, audio link in a document M2):
  Space              Play/pause (M1) — in M2 Space still scrolls a page
  p                  Play/pause
  ← / →              Seek ∓5s (Shift: ∓1s);  , / . : ∓60s
  - / +              Volume ∓5%;  m: mute;  0: restart
  o                  Open the current web page in a browser (web mode only)
  Esc                Clear selection → stop the session → quit
  ⌫ / q / Ctrl+C     Stop the session and go back / quit

Notes:
  - Audio/video/web need a terminal; piping them errors with exit 1.
  - Media sessions do not participate in hot reload: editing a playing file
    does not interrupt playback, and web pages are not re-fetched.
";

pub struct ParsedArgs {
    pub file: String,
}

fn print_stdout_and_exit(code: i32, text: &str) -> ! {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = writeln!(lock, "{}", text);
    let _ = lock.flush();
    exit(code);
}

fn print_stderr_and_exit(code: i32, text: &str) -> ! {
    let stderr = std::io::stderr();
    let mut lock = stderr.lock();
    let _ = writeln!(lock, "{}", text);
    let _ = lock.flush();
    exit(code);
}

pub fn parse_args(argv: &[String]) -> ParsedArgs {
    let mut positional: Vec<&str> = Vec::new();

    for arg in argv {
        if arg == "--help" || arg == "-h" {
            print_stdout_and_exit(0, HELP);
        }
        if arg == "--version" || arg == "-V" {
            print_stdout_and_exit(0, VERSION);
        }
        positional.push(arg);
    }

    if positional.is_empty() {
        print_stderr_and_exit(2, &format!("{}\n", HELP));
    }

    if positional.len() > 1 {
        print_stderr_and_exit(
            2,
            &format!(
                "error: too many arguments (expected 1, got {})\n\n{}",
                positional.len(),
                HELP
            ),
        );
    }

    ParsedArgs {
        file: positional[0].to_string(),
    }
}
