//! dlook —— 终端文件预览 CLI 入口（Rust 版）。
//!
//! 装配流程：
//!   parse_args → load_content(二进制检测 + 模式判定) → 非 TTY 直出
//!   → 按 mode 生成 Doc.lines(md/code/mermaid)
//!   → TUI 循环(crossterm + ratatui)
//!
//! 退出码：q/Esc→0；Ctrl+C→130；参数错误→2；文件不存在/二进制/目录→1。

mod ansi_lines;
mod args;
mod content;
mod doc;
mod highlight;
mod images;
mod lang;
mod links;
mod markdown;
mod media;
mod mermaid;
mod selection;
mod termio;
mod video;
mod viewport;
mod web;

use std::process::exit;

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let parsed = args::parse_args(&argv);

    let loaded = content::load_content(&parsed.file);

    // 非 TTY（管道重定向）：pager 无意义 → 直出
    if !termio::is_tty() {
        // 媒体模式(音频/视频/网页)需要终端:播放、图形协议、鼠标交互都依赖 TTY
        // (design §4 非 TTY)。
        if matches!(
            loaded.mode,
            lang::Mode::Audio | lang::Mode::Video | lang::Mode::Web
        ) {
            eprintln!("error: '{}' needs a terminal", parsed.file);
            exit(1);
        }
        // 图片无法在管道中渲染(图形协议需要 TTY)
        if loaded.mode == lang::Mode::Image {
            eprintln!(
                "error: '{}' is an image file; run dlook in a terminal to view it",
                parsed.file
            );
            exit(1);
        }
        let stdout = std::io::stdout();
        use std::io::Write;
        let mut lock = stdout.lock();
        // mermaid 文件：渲染成 ASCII 图再输出（源码直出无意义）
        // markdown / code：直出原始内容
        let output: Vec<u8> = match loaded.mode {
            lang::Mode::Mermaid => {
                match mermaid::render_mermaid_to_plain(&loaded.content) {
                    Ok(text) => text.into_bytes(),
                    Err(_) => loaded.content.clone().into_bytes(),
                }
            }
            _ => loaded.content.clone().into_bytes(),
        };
        let _ = lock.write_all(&output);
        let _ = lock.flush();
        exit(0);
    }

    // TUI 模式
    let exit_code = termio::run(
        &parsed.file,
        loaded.mode,
        loaded.syntax_token,
        &loaded.content,
    );
    exit(exit_code);
}
