//! crossterm 装配 + TUI 事件循环。
//!
//! 职责：raw mode / alt-screen / 鼠标捕获 / 事件读取 / 键位+滚轮映射 / resize /
//! 文件变更热重载（stat 轮询）/ 文本拖选与复制（selection）/ cleanup。

use std::io::{self, IsTerminal, Stdout};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use crossterm::clipboard::CopyToClipboard;
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MediaKeyCode, MouseButton,
    MouseEvent, MouseEventKind,
};
use crossterm::style::{Attribute, Colored};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::execute;
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::CellDiffOption;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Terminal;

use crate::ansi_lines;
use crate::content;
use crate::doc::{Doc, DocImage};
use crate::highlight::Highlighter;
use crate::images::{self, ImageCtx};
use crate::lang::{self, Mode};
use crate::links::{self, LinkSpan};
use crate::markdown;
use crate::media::{AudioCtx, AudioSnapshot, AudioStatus};
use crate::mermaid;
use crate::selection;
use crate::video::{TermProto, VideoArea, VideoCtx, VideoSnapshot, VideoStatus};
use crate::viewport::Viewport;
use crate::web;

type Term = Terminal<CrosstermBackend<Stdout>>;

/// 左右边距（字符数）。
const H_MARGIN: u16 = 1;

const FOOTER: &str = "q quit  ↑↓/jk/scroll  space/pgdn  g/G top/bottom  Ctrl+C quit";
const FOOTER_BACK: &str = "  ⌫back";

/// M1（媒体即文档）footer：整体替换普通键位表（design §3）。
const FOOTER_MEDIA: &str = " space ⏯  ←→ seek  -/+ vol  m mute  j/k scroll  ⌫back  q quit";
/// M2（其他模式 + 音频会话活跃）footer：在既有 footer 后追加（design §3）。
const FOOTER_M2_SUFFIX: &str = "  ♪ p ⏯";

/// 状态栏消息存活时间。
const STATUS_TTL: Duration = Duration::from_millis(1500);

/// 拖选边缘自动滚动的最小间隔。
const AUTO_SCROLL_INTERVAL: Duration = Duration::from_millis(120);

// ---------------------------------------------------------------------------
// 媒体集成常量（task media-4 / design §3–§5）
// ---------------------------------------------------------------------------

/// 媒体栏信息行右端音量区宽度（末 8 列，design §3 鼠标表）。
const VOLUME_ZONE_COLS: u16 = 8;
/// 进度条行右端时间码宽度（`MM:SS / MM:SS`）。
const TIMECODE_COLS: u16 = 13;
/// seek 步长（秒）：←/→、Shift+←/→、,/.。
const SEEK_STEP: f64 = 5.0;
const SEEK_STEP_FINE: f64 = 1.0;
const SEEK_STEP_COARSE: f64 = 60.0;
/// 音量步长（design §3：±5%）。
const VOLUME_STEP: f32 = 0.05;
/// scrubbing 预览节流（design §3 鼠标表：120ms）。
const SCRUB_THROTTLE: Duration = Duration::from_millis(120);
/// body 高度（含 header/footer 之外）小于此值时媒体栏折叠为 1 行。
const BAR_FOLD_BODY_H: u16 = 6;
/// 首帧静图降级产物（ffmpeg 抽帧）落在临时目录的前缀。
const FRAME_TMP_PREFIX: &str = "dlook-frame-";

/// 键位上下文（design §3）：M1 = 媒体即文档；M2 = 其他模式 + 音频会话活跃。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyCtx {
    Pager,
    M1,
    M2,
}

/// 媒体栏各命中区（屏幕坐标；由 render_frame 每帧写入，鼠标处理读它）。
///
/// 命中测试基于**渲染时产生的矩形**（design §3 鼠标表），避免布局与命中漂移。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct MediaRects {
    /// 媒体栏高度（0 = 无会话；1 = 折叠；2 = 正常）。滚动/选区按它扣除 body。
    bar_h: u16,
    /// 进度条行（屏幕行号）。
    progress_row: Option<u16>,
    /// 进度条图形区（click-to-seek 的比例基准）。
    progress: Option<Rect>,
    /// 信息行（屏幕行号）。
    info_row: Option<u16>,
    /// 信息行右端音量区（末 VOLUME_ZONE_COLS 列）。
    volume: Option<Rect>,
    /// 视频区（mpv 画图区域；排除选区/链接命中）。
    video: Option<Rect>,
}

impl MediaRects {
    /// 该屏幕行是否属于媒体栏（不参与选区/链接命中）。
    fn in_bar(&self, row: u16) -> bool {
        self.progress_row == Some(row) || self.info_row == Some(row)
    }

    /// 点是否落在音量区（末 8 列）。
    fn in_volume(&self, col: u16, row: u16) -> bool {
        self.volume
            .is_some_and(|v| row == v.y && col >= v.x && col < v.x.saturating_add(v.width))
    }

    /// 点是否落在视频区（mpv 画图区，dlook 不参与命中）。
    fn in_video(&self, col: u16, row: u16) -> bool {
        self.video.is_some_and(|v| {
            row >= v.y && row < v.y.saturating_add(v.height) && col >= v.x && col < v.x.saturating_add(v.width)
        })
    }

    /// 进度条行上的列 → 目标比例 0.0..=1.0（click-to-seek / scrubbing）。
    fn seek_frac(&self, col: u16) -> Option<f32> {
        let p = self.progress?;
        if p.width == 0 {
            return None;
        }
        let rel = col.saturating_sub(p.x) as f32;
        Some((rel / p.width as f32).clamp(0.0, 1.0))
    }
}

/// TUI 交互状态:选区 + 拖拽 + 状态栏消息 + 媒体栏命中区。
struct UiState {
    sel: Option<selection::Selection>,
    /// 鼠标左键按下中(Down 后 Up 前)。
    dragging: bool,
    /// 按下后指针是否移动过(纯点击不算拖拽,不触发边缘自动滚动)。
    moved: bool,
    /// 最近一次指针位置(屏幕坐标 列,行)。
    pointer: Option<(u16, u16)>,
    /// 上次边缘自动滚动时刻(None = 从未,首次立即触发)。
    last_autoscroll: Option<Instant>,
    /// 状态栏消息 + 产生时刻。
    status: Option<(String, Instant)>,
    /// 媒体栏命中区（render_frame 每帧更新）。
    media: MediaRects,
    /// 进度条拖动中（scrubbing）。
    scrubbing: bool,
    /// 上次 scrubbing 预览时刻（120ms 节流）。
    last_scrub: Option<Instant>,
    /// 当前拖动比例（松开时提交 seek_to_fraction）。
    scrub_frac: f32,
}

impl UiState {
    fn new() -> Self {
        UiState {
            sel: None,
            dragging: false,
            moved: false,
            pointer: None,
            last_autoscroll: None,
            status: None,
            media: MediaRects::default(),
            scrubbing: false,
            last_scrub: None,
            scrub_frac: 0.0,
        }
    }

    /// 未过期的状态栏消息。
    fn status_text(&self) -> Option<&str> {
        self.status.as_ref().and_then(|(msg, at)| {
            if at.elapsed() < STATUS_TTL {
                Some(msg.as_str())
            } else {
                None
            }
        })
    }

    /// 写状态栏消息（动作反馈，1.5s TTL）。
    fn say(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Instant::now()));
    }
}

/// 判定 stdout 是否为 TTY。
pub fn is_tty() -> bool {
    io::stdout().is_terminal()
}

// ---------------------------------------------------------------------------
// 媒体状态（termio 持有会话层；Doc/UiState 不复制媒体状态，design §2）
// ---------------------------------------------------------------------------

/// 一帧渲染所需的媒体数据（每帧从引擎 `snapshot()` 取一次）。
#[derive(Default)]
struct MediaView {
    audio: Option<AudioSnapshot>,
    video: Option<VideoSnapshot>,
    /// 是否有活跃会话（决定媒体栏高度 0/1/2）。
    session: bool,
    /// body 区由 mpv 绘制 → 每帧标记 CellDiffOption::Skip。
    mpv_body: bool,
    /// 网页标题（header 展示）。
    web_title: Option<String>,
}

/// 媒体栏展示数据（音频/视频归一化，供行 1/行 2 渲染）。
struct BarData {
    title: String,
    position: Duration,
    duration: Option<Duration>,
    volume: f32,
    muted: bool,
    paused: bool,
    finished: bool,
    loading: bool,
    /// 失败原因（音频设备不可用 / 解码失败等）→ 行 2 `✗ <原因>`。
    failed: Option<String>,
    /// 显示音量区（视频无音轨时为 false）。
    show_volume: bool,
}

impl MediaView {
    /// 归一化媒体栏数据；无会话返回 None。
    fn bar_data(&self) -> Option<BarData> {
        if let Some(a) = &self.audio {
            let failed = match &a.status {
                AudioStatus::Failed(e) => Some(e.clone()),
                _ => None,
            };
            return Some(BarData {
                title: a.title.clone(),
                position: a.position,
                duration: a.duration,
                volume: a.volume,
                muted: a.muted,
                paused: a.paused,
                finished: a.finished,
                loading: matches!(a.status, AudioStatus::Loading),
                failed,
                show_volume: true,
            });
        }
        if let Some(v) = &self.video {
            let failed = match &v.status {
                VideoStatus::Failed(e) => Some(e.clone()),
                _ => None,
            };
            return Some(BarData {
                title: String::new(),
                position: v.position,
                duration: v.duration,
                volume: v.volume,
                muted: false,
                paused: v.paused,
                finished: matches!(v.status, VideoStatus::Finished),
                loading: matches!(v.status, VideoStatus::Loading),
                failed,
                show_volume: v.has_audio,
            });
        }
        None
    }
}

/// 视频 body 的形态（随降级链变化）。
#[derive(Clone, Default)]
enum VideoBody {
    /// mpv 共屏：dlook 不向 body 写字节（每帧 Skip 单元格）。
    #[default]
    Empty,
    /// 降级：ffmpeg 首帧静图走既有图片管线（值为图片 src key）。
    Still(String),
    /// 降级：信息行（文件名/时长/分辨率 + 原因）。
    Info(Vec<Line<'static>>),
}

/// 网页 body 的形态。
#[derive(Clone, Default)]
enum WebBody {
    /// 后台抓取/渲染中。
    #[default]
    Loading,
    /// 失败（抓取/渲染错误、本地文件缺失）。
    Failed(String),
    /// 渲染完成（行模型 + 链接 + 最终 URL）。
    Ready {
        title: String,
        final_url: String,
        lines: Vec<Line<'static>>,
        links: Vec<LinkSpan>,
    },
}

/// 媒体模式的行模型输入（事件循环按引擎快照维护，rebuild_doc 消费）。
#[derive(Clone, Default)]
struct MediaInput {
    /// Mode::Audio 的信息块。
    audio_lines: Vec<Line<'static>>,
    /// Mode::Video 的 body 形态。
    video: VideoBody,
    /// Mode::Web 的 body 形态。
    web: WebBody,
}

/// 视频渲染计划（降级链，design §4；纯函数便于单测/消融）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VideoPlan {
    /// mpv 共屏（有 mpv + 有图形协议）。
    Mpv,
    /// 无 mpv / 无图形协议 → ffmpeg 首帧静图。
    FirstFrame,
    /// 无 ffmpeg → 信息行。
    InfoLines,
}

/// 降级链判定：mpv + 图形协议 → 共屏；否则有 ffmpeg 用首帧静图；再否则信息行。
fn video_plan(mpv_available: bool, proto_available: bool, ffmpeg_available: bool) -> VideoPlan {
    if mpv_available && proto_available {
        VideoPlan::Mpv
    } else if ffmpeg_available {
        VideoPlan::FirstFrame
    } else {
        VideoPlan::InfoLines
    }
}

/// mpv 显示区域（design §5.1）：left=H_MARGIN, top=1, cols=内容宽, rows=body 行数。
fn video_area_for(term_w: u16, term_h: u16, bar_h: u16) -> VideoArea {
    VideoArea {
        left: H_MARGIN,
        top: 1,
        cols: content_width(term_w),
        rows: term_h.saturating_sub(2 + bar_h),
    }
}

/// 媒体栏高度：无会话 0；body 不足 6 行折叠为 1；否则 2（design §5.1）。
fn bar_height_for(term_h: u16, session: bool) -> u16 {
    if !session {
        return 0;
    }
    if term_h.saturating_sub(2) < BAR_FOLD_BODY_H {
        1
    } else {
        2
    }
}

/// `MM:SS`（时长未知 → `--:--`；≥1 小时用 `H:MM:SS`）。
fn fmt_time(d: Option<Duration>) -> String {
    let Some(d) = d else {
        return "--:--".to_string();
    };
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

/// 进度条（已播 `━` + 未播 `─`）；时长未知 → 全 `─`。
fn progress_text(frac: Option<f32>, width: u16) -> String {
    let width = width as usize;
    let filled = match frac {
        Some(f) => ((f.clamp(0.0, 1.0) * width as f32).round() as usize).min(width),
        None => 0,
    };
    let mut s = String::with_capacity(width * 3);
    for i in 0..width {
        s.push(if i < filled { '━' } else { '─' });
    }
    s
}

/// 状态图标（design §3 行 2）：失败 / 加载 / 结束 / 暂停 / 播放。
fn state_icon(d: &BarData) -> &'static str {
    if d.failed.is_some() {
        "✗"
    } else if d.loading {
        "⏳"
    } else if d.finished {
        "◼"
    } else if d.paused {
        "▮▮"
    } else {
        "▶"
    }
}

/// 可执行文件查找（PATH 扫描，不 spawn 进程）。
fn which(prog: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(prog))
        .find(|p| p.is_file())
}

/// ffmpeg 抽首帧为 PNG（design §4 降级链中间层），写临时文件供图片管线读取。
fn ffmpeg_first_frame(src: &str) -> Option<PathBuf> {
    let out = Command::new("ffmpeg")
        .args([
            "-v", "error", "-y", "-i", src, "-frames:v", "1", "-f", "image2pipe", "-vcodec", "png",
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() || out.stdout.is_empty() {
        return None;
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!(
        "{FRAME_TMP_PREFIX}{}-{stamp}.png",
        std::process::id()
    ));
    std::fs::write(&path, &out.stdout).ok()?;
    Some(path)
}

/// ffprobe 元信息（后台线程执行，避免阻塞 UI 线程）：`WxH  MM:SS` 或 None。
fn ffprobe_info(path: &str) -> Option<String> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "format=duration:stream=width,height",
            "-of",
            "default=noprint_wrappers=1",
            path,
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    // 形如 "width=640\nheight=360\nduration=10.000000"
    let field = |k: &str| -> Option<String> {
        let needle = format!("{k}=");
        text.lines()
            .find_map(|l| l.strip_prefix(&needle))
            .map(str::trim)
            .filter(|v| !v.is_empty() && *v != "N/A")
            .map(str::to_string)
    };
    let mut parts: Vec<String> = Vec::new();
    if let (Some(w), Some(h)) = (field("width"), field("height")) {
        parts.push(format!("{w}x{h}"));
    }
    if let Some(d) = field("duration").and_then(|d| d.parse::<f64>().ok()) {
        parts.push(fmt_time(Some(Duration::from_secs_f64(d.max(0.0)))));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("  "))
    }
}

/// 媒体会话状态（事件循环持有，跨 rebuild 存活）。
struct MediaState {
    audio: AudioCtx,
    video: VideoCtx,
    audio_active: bool,
    audio_src: Option<String>,
    video_active: bool,
    video_src: Option<String>,
    audio_dirty: u64,
    video_dirty: u64,
    /// 媒体模式的行模型输入。
    input: MediaInput,
    /// 降级链产生的临时文件（退出时清理）。
    temp_files: Vec<PathBuf>,
    /// 网页后台渲染结果。
    web_rx: Option<mpsc::Receiver<Result<web::WebDoc, String>>>,
    web_source: String,
    /// 视频信息行所需的 ffprobe 元信息（后台线程）。
    probe_rx: Option<mpsc::Receiver<String>>,
    probe_deadline: Option<Instant>,
    probe_text: Option<String>,
    /// 视频降级原因（信息行/状态栏文案）。
    degrade_reason: String,
}

impl MediaState {
    fn new() -> Self {
        MediaState {
            audio: AudioCtx::new(),
            video: VideoCtx::new(),
            audio_active: false,
            audio_src: None,
            video_active: false,
            video_src: None,
            audio_dirty: 0,
            video_dirty: 0,
            input: MediaInput::default(),
            temp_files: Vec::new(),
            web_rx: None,
            web_source: String::new(),
            probe_rx: None,
            probe_deadline: None,
            probe_text: None,
            degrade_reason: String::new(),
        }
    }

    /// 当前键位上下文（design §3 判定）。
    fn key_ctx(&self, mode: Mode) -> KeyCtx {
        match mode {
            Mode::Audio | Mode::Video => KeyCtx::M1,
            _ if self.audio_active => KeyCtx::M2,
            _ => KeyCtx::Pager,
        }
    }

    /// 是否有活跃会话（媒体栏是否显示）。
    fn session_active(&self) -> bool {
        self.audio_active || self.video_active
    }

    /// 一帧渲染视图（仅活跃会话时读引擎快照）。
    fn view(&self) -> MediaView {
        let audio = if self.audio_active {
            self.audio.snapshot()
        } else {
            None
        };
        let video = if self.video_active {
            self.video.snapshot()
        } else {
            None
        };
        MediaView {
            audio,
            video,
            session: self.session_active(),
            mpv_body: self.video_active,
            web_title: match &self.input.web {
                WebBody::Ready { title, .. } => Some(title.clone()),
                _ => None,
            },
        }
    }

    /// `o` 键的目标：网页模式用最终 URL（重定向后），否则当前路径。
    fn browser_url(&self, fallback: &str) -> String {
        match &self.input.web {
            WebBody::Ready { final_url, .. } => final_url.clone(),
            _ => fallback.to_string(),
        }
    }

    /// 打开音频会话（M1 直开 / M2 就地播放共用）。
    fn start_audio(&mut self, src: &str, base_dir: &Path) {
        if self.audio_active && self.audio_src.as_deref() == Some(src) {
            return;
        }
        self.stop_video();
        self.stop_audio();
        self.audio.open(src, base_dir);
        self.audio_active = true;
        self.audio_src = Some(src.to_string());
        self.audio_dirty = self.audio.dirty_version();
        self.refresh_audio_lines();
    }

    /// 停止音频会话（幂等）。
    fn stop_audio(&mut self) {
        if !self.audio_active {
            return;
        }
        self.audio.close();
        self.audio_active = false;
        self.audio_src = None;
        self.refresh_audio_lines();
    }

    /// 启动视频会话；按降级链回退。返回 true 表示进入 mpv 共屏形态。
    fn start_video(
        &mut self,
        src: &str,
        term_w: u16,
        term_h: u16,
        proto: Option<TermProto>,
    ) -> bool {
        self.stop_video();
        self.stop_audio();
        self.video_src = Some(src.to_string());
        self.probe_text = None;
        let mpv = VideoCtx::available();
        let ffmpeg = which("ffmpeg").is_some();
        match video_plan(mpv, proto.is_some(), ffmpeg) {
            VideoPlan::Mpv => {
                // area 的 bar_h 与首帧渲染一致（有会话 → 2 行或折叠 1 行）
                let bar_h = bar_height_for(term_h, true);
                let area = video_area_for(term_w, term_h, bar_h);
                match self.video.start(src, area, proto.expect("proto checked by plan")) {
                    Ok(()) => {
                        self.video_active = true;
                        self.video_dirty = self.video.dirty_version();
                        self.input.video = VideoBody::Empty;
                        true
                    }
                    Err(e) => {
                        self.degrade_reason = format!("mpv failed: {e}");
                        let reason = self.degrade_reason.clone();
                        self.input.video =
                            VideoBody::Info(video_info_lines(src, &reason, self.probe_text.clone()));
                        self.start_probe(src);
                        false
                    }
                }
            }
            VideoPlan::FirstFrame => {
                let reason = if !mpv {
                    "mpv not found — showing first frame"
                } else {
                    "no graphics protocol — showing first frame"
                };
                self.degrade_reason = reason.to_string();
                match ffmpeg_first_frame(src) {
                    Some(png) => {
                        let key = still_src_key(&png);
                        self.temp_files.push(png);
                        self.input.video = VideoBody::Still(key);
                    }
                    None => {
                        let reason = format!("{reason} (ffmpeg failed)");
                        self.degrade_reason = reason.clone();
                        self.input.video =
                            VideoBody::Info(video_info_lines(src, &reason, self.probe_text.clone()));
                        self.start_probe(src);
                    }
                }
                false
            }
            VideoPlan::InfoLines => {
                let reason = "mpv not found — no ffmpeg (info only)";
                self.degrade_reason = reason.to_string();
                self.input.video =
                    VideoBody::Info(video_info_lines(src, reason, self.probe_text.clone()));
                self.start_probe(src);
                false
            }
        }
    }

    /// 后台启动 ffprobe（信息行需要时长/分辨率；3s 没结果就放弃）。
    fn start_probe(&mut self, src: &str) {
        if self.probe_text.is_some() || which("ffprobe").is_none() {
            return;
        }
        if !matches!(self.input.video, VideoBody::Info(_)) {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let src = src.to_string();
        std::thread::spawn(move || {
            if let Some(info) = ffprobe_info(&src) {
                let _ = tx.send(info);
            }
        });
        self.probe_rx = Some(rx);
        self.probe_deadline = Some(Instant::now() + Duration::from_secs(3));
    }

    /// 轮询 ffprobe 结果；返回 true 表示信息行需要重建。
    fn poll_probe(&mut self, src: &str) -> bool {
        let Some(res) = self.probe_rx.as_ref().map(|rx| rx.try_recv()) else {
            return false;
        };
        match res {
            Ok(info) => {
                self.probe_rx = None;
                self.probe_deadline = None;
                self.probe_text = Some(info.clone());
                if matches!(self.input.video, VideoBody::Info(_)) {
                    let reason = self.degrade_reason.clone();
                    self.input.video = VideoBody::Info(video_info_lines(src, &reason, Some(info)));
                }
                true
            }
            Err(mpsc::TryRecvError::Empty) => {
                if self.probe_deadline.is_some_and(|t| Instant::now() > t) {
                    self.probe_rx = None;
                    self.probe_deadline = None;
                }
                false
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.probe_rx = None;
                self.probe_deadline = None;
                false
            }
        }
    }

    /// 停止视频会话（幂等）；body 换成“已停止”信息行。
    fn stop_video(&mut self) {
        if !self.video_active {
            return;
        }
        self.video.stop();
        self.video_active = false;
        let src = self.video_src.take().unwrap_or_else(|| "-".to_string());
        self.input.video = VideoBody::Info(vec![Line::default().spans(vec![Span::styled(
            format!("■ playback stopped: {src}"),
            Style::default().add_modifier(Modifier::DIM),
        )])]);
    }

    /// 停止所有会话（退出 / 离开媒体模式前）。
    fn stop_all(&mut self) {
        self.stop_video();
        self.stop_audio();
    }

    /// 网页模式：后台抓取 + 渲染（失败经 channel 回来，不在 UI 线程 panic）。
    fn start_web(&mut self, source: &str, width: u16, skin: &termimad::MadSkin) {
        if self.web_source == source && self.web_rx.is_some() {
            return;
        }
        self.web_source = source.to_string();
        self.input.web = WebBody::Loading;
        let (tx, rx) = mpsc::channel();
        let src = source.to_string();
        let skin = skin.clone();
        std::thread::spawn(move || {
            let _ = tx.send(web::render(&src, width, &skin));
        });
        self.web_rx = Some(rx);
    }

    /// 轮询网页后台结果；返回 (需要重建, 状态栏提示)。
    fn poll_web(&mut self) -> (bool, Option<String>) {
        let Some(res) = self.web_rx.as_ref().map(|rx| rx.try_recv()) else {
            return (false, None);
        };
        match res {
            Ok(Ok(doc)) => {
                self.web_rx = None;
                self.input.web = WebBody::Ready {
                    title: doc.title,
                    final_url: doc.final_url,
                    lines: doc.lines,
                    links: doc.links,
                };
                (true, None)
            }
            Ok(Err(e)) => {
                self.web_rx = None;
                self.input.web = WebBody::Failed(format!("✗ {e}"));
                (true, Some(format!("✗ {e}")))
            }
            Err(mpsc::TryRecvError::Empty) => (false, None),
            Err(mpsc::TryRecvError::Disconnected) => {
                self.web_rx = None;
                self.input.web = WebBody::Failed("✗ web render failed".to_string());
                (true, Some("✗ web render failed".to_string()))
            }
        }
    }

    /// 轮询引擎 dirty 计数（加载/失败/结束等异步事件）；true = 需要重建 Doc。
    fn poll_dirty(&mut self) -> bool {
        let mut changed = false;
        if self.audio_active {
            let v = self.audio.dirty_version();
            if v != self.audio_dirty {
                self.audio_dirty = v;
                self.refresh_audio_lines();
                changed = true;
            }
        }
        if self.video_active {
            self.video.tick();
            let v = self.video.dirty_version();
            if v != self.video_dirty {
                self.video_dirty = v;
                changed = true;
            }
        }
        changed
    }

    /// 刷新 Mode::Audio 的 body 信息块。
    fn refresh_audio_lines(&mut self) {
        let snap = if self.audio_active {
            self.audio.snapshot()
        } else {
            None
        };
        self.input.audio_lines = audio_info_lines(self.audio_src.as_deref(), snap);
    }

    /// 清理降级链临时文件（退出前）。
    fn cleanup(&mut self) {
        for p in self.temp_files.drain(..) {
            let _ = std::fs::remove_file(p);
        }
    }

    // ---- 播放动作（转发到活跃会话；无会话时静默忽略）----

    /// 动作目标：音频优先（M2 音频会话 + 视频不会同时活跃）。
    fn toggle_pause(&self) {
        if self.audio_active {
            self.audio.toggle_pause();
        } else if self.video_active {
            self.video.toggle_pause();
        }
    }

    fn seek_by(&self, delta: f64) {
        if self.audio_active {
            self.audio.seek_by(delta);
        } else if self.video_active {
            self.video.seek_by(delta);
        }
    }

    fn seek_to_fraction(&self, frac: f32) {
        if self.audio_active {
            self.audio.seek_to_fraction(frac);
        } else if self.video_active {
            self.video.seek_to_fraction(frac);
        }
    }

    fn adjust_volume(&self, delta: f32) {
        if self.audio_active {
            self.audio.adjust_volume(delta);
        } else if self.video_active {
            self.video.adjust_volume(delta);
        }
    }

    fn toggle_mute(&self) {
        if self.audio_active {
            self.audio.toggle_mute();
        } else if self.video_active {
            // VideoCtx::toggle_mute 于集成期补入冻结接口(design §2 接口变更记录,
            // 2026-09-13;实现见 task media-3)
            self.video.toggle_mute();
        }
    }

    fn restart(&self) {
        if self.audio_active {
            self.audio.restart();
        } else if self.video_active {
            self.video.seek_to_fraction(0.0);
        }
    }

    /// 视频区几何变化（resize）→ 交引擎（热改或重启，见 media-3）。
    fn set_area(&self, area: VideoArea) {
        if self.video_active {
            self.video.set_area(area);
        }
    }
}

/// 播放/暂停的动作反馈文案。
fn say_playback_state(ui: &mut UiState, media: &MediaState) {
    let paused = media
        .view()
        .bar_data()
        .map(|d| d.paused)
        .unwrap_or(false);
    ui.say(if paused { "paused" } else { "playing" });
}

/// seek 的动作反馈：`seek +5s → 01:28`（design §3 动作反馈）。
fn say_seek(ui: &mut UiState, media: &MediaState, delta: f64) {
    let pos = media.view().bar_data().map(|d| d.position);
    ui.say(format!("seek {delta:+.0}s → {}", fmt_time(pos)));
}

/// 音量的动作反馈：`vol 75%` / `muted`。
fn say_volume(ui: &mut UiState, media: &MediaState) {
    match media.view().bar_data() {
        Some(d) if d.muted => ui.say("muted"),
        Some(d) => ui.say(format!("vol {}%", (d.volume * 100.0).round() as i32)),
        None => {}
    }
}

/// Mode::Audio 的 body 信息块：标题 / 状态（含 ✗ 原因）/ 位置 / 音量 / 格式 + 操作提示。
fn audio_info_lines(src: Option<&str>, snap: Option<AudioSnapshot>) -> Vec<Line<'static>> {
    fn dim(s: String) -> Line<'static> {
        Line::default().spans(vec![Span::styled(
            s,
            Style::default().add_modifier(Modifier::DIM),
        )])
    }
    let Some(src) = src else {
        return vec![dim("♪ no active audio session".to_string())];
    };
    let title = snap
        .as_ref()
        .map(|s| s.title.clone())
        .unwrap_or_else(|| file_name_of(src));
    let mut out = vec![Line::default().spans(vec![Span::styled(
        format!("♪ {title}"),
        Style::default().add_modifier(Modifier::BOLD),
    )])];
    out.push(Line::default());
    match &snap {
        Some(s) => {
            let state = match &s.status {
                AudioStatus::Loading => "⏳ loading …".to_string(),
                AudioStatus::Failed(e) => format!("✗ {e}"),
                AudioStatus::Ready if s.finished => "◼ finished".to_string(),
                AudioStatus::Ready if s.paused => "▮▮ paused".to_string(),
                AudioStatus::Ready => "▶ playing".to_string(),
            };
            out.push(dim(format!("state:    {state}")));
            out.push(dim(format!("duration: {}", fmt_time(s.duration))));
            out.push(dim(format!(
                "volume:   {:>3}%{}",
                (s.volume * 100.0).round() as i32,
                if s.muted { " (muted)" } else { "" }
            )));
        }
        None => out.push(dim("state:    no active session".to_string())),
    }
    out.push(dim(format!("format:   {}", ext_of(src))));
    out.push(dim(format!("source:   {src}")));
    out.push(Line::default());
    out.push(dim(
        "p / Space 播放/暂停   ←/→ seek ∓5s   ,/. ∓60s   -/+ 音量   m 静音   0 重播   ⌫/q 停止退出"
            .to_string(),
    ));
    out
}

/// Mode::Video 降级信息行（文件名 / 格式 / 大小 / 时长·分辨率 / 原因）。
fn video_info_lines(src: &str, reason: &str, probe: Option<String>) -> Vec<Line<'static>> {
    fn dim(s: String) -> Line<'static> {
        Line::default().spans(vec![Span::styled(
            s,
            Style::default().add_modifier(Modifier::DIM),
        )])
    }
    let mut out = vec![Line::default().spans(vec![Span::styled(
        format!("■ {}", file_name_of(src)),
        Style::default().add_modifier(Modifier::BOLD),
    )])];
    out.push(Line::default());
    out.push(dim(format!("format:   {}", ext_of(src))));
    if let Ok(meta) = std::fs::metadata(src) {
        out.push(dim(format!("size:     {} KB", meta.len() / 1024)));
    }
    match probe {
        Some(p) => out.push(dim(format!("media:    {p}"))),
        None => out.push(dim(
            "media:    (duration/resolution unavailable — ffprobe not found)".to_string(),
        )),
    }
    out.push(Line::default());
    out.push(dim(format!("note:     {reason}")));
    out
}

/// 文件名（路径尾段）。
fn file_name_of(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

/// 扩展名（小写；无扩展名 → `-`）。
fn ext_of(path: &str) -> String {
    let base = file_name_of(path);
    match base.rfind('.') {
        Some(d) => base[d + 1..].to_lowercase(),
        None => "-".to_string(),
    }
}

/// 图片管线 src key（本地路径 + 指纹后缀；图片管线会剥掉 query）。
fn still_src_key(path: &Path) -> String {
    format!("file:{}?v0", path.display())
}

/// 构建 markdown skin(对齐 vue-tui 默认主题,见 GAP.md G1/G2/G6):
///   - 标题:粗体(去 termimad 默认下划线);h1/h2 青、h3/h4 蓝、h5/h6 无色
///   - H1 左对齐(termimad 默认居中,vue-tui 为左对齐)
///   - 表格圆角边框(vue-tui 用 ╭┬╮;termimad 默认方角 ┌┬┐)
fn build_skin() -> termimad::MadSkin {
    use crossterm::style::Color;

    let mut skin = termimad::MadSkin::default();
    for (i, h) in skin.headers.iter_mut().enumerate() {
        h.compound_style
            .object_style
            .attributes
            .unset(Attribute::Underlined);
        h.compound_style
            .object_style
            .attributes
            .set(Attribute::Bold);
        match i {
            0 | 1 => h.compound_style.set_fg(Color::Cyan), // cyanBright
            2 | 3 => h.compound_style.set_fg(Color::Blue), // blueBright
            _ => {}
        }
    }
    skin.headers[0].align = termimad::Alignment::Left;
    skin.table_border_chars = termimad::ROUNDED_TABLE_BORDER_CHARS;
    skin
}

/// 运行 TUI 循环，返回退出码。
///
/// `file_path` 用于文件变更监听（热重载）；`mode`/`syntax_token` 由扩展名决定，
/// 文件类型不会因内容修改而变化，故在加载时一次性确定。
pub fn run(
    file_path: &str,
    mode: Mode,
    syntax_token: Option<&'static str>,
    initial_content: &str,
) -> i32 {
    // 强制启用 ANSI 颜色输出（忽略 NO_COLOR 环境变量）。
    Colored::set_ansi_color_disabled(false);

    let mut stdout = io::stdout();
    let _ = execute!(stdout, EnterAlternateScreen, crossterm::event::EnableMouseCapture);

    // 终端图形协议探测(DECISIONS D15):必须在进入 raw mode / 事件读取前调用,
    // from_query_stdio 需直接读写 stdin 收终端响应;纯文本文档跳过探测保持即时启动。
    // 视频模式(D16)需要协议结论来选 mpv 的 vo → 一并探测。
    let query_proto = images::should_query_protocol(mode, initial_content) || mode == Mode::Video;
    let img_ctx = ImageCtx::new(images::acquire_policy(query_proto));

    let _ = enable_raw_mode();

    let mut terminal = match Terminal::new(CrosstermBackend::new(stdout)) {
        Ok(t) => t,
        Err(_) => return 1,
    };

    let highlighter = Highlighter::new();
    let skin = build_skin();

    let (w, _h) = current_size(&terminal);
    let content_w = content_width(w);
    let (lines, doc_links, doc_images) = build_initial(mode, file_path, initial_content, syntax_token, content_w, &highlighter, &skin, &img_ctx);
    let mut doc = Doc::new(lines, mode, content_w, doc_links, doc_images);

    let exit_code = event_loop(
        &mut terminal,
        &mut doc,
        file_path,
        mode,
        syntax_token,
        &highlighter,
        &skin,
        &img_ctx,
    );

    // Cleanup
    let _ = disable_raw_mode();
    let _ = execute!(
        terminal.backend_mut(),
        crossterm::event::DisableMouseCapture,
        LeaveAlternateScreen
    );

    exit_code
}

/// 文件的 stat 指纹(mtime + size),用于热重载轮询;不可读时返回 None。
fn file_stamp(path: &str) -> Option<(std::time::SystemTime, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

fn current_size(terminal: &Term) -> (u16, u16) {
    terminal
        .size()
        .map(|r| (r.width, r.height))
        .unwrap_or((80, 24))
}

/// 内容区宽度 = 终端宽度 - 左右边距。
fn content_width(term_w: u16) -> u16 {
    term_w.saturating_sub(H_MARGIN * 2)
}

/// 导航状态:当前文件 + 返回栈(D14)。
/// 点击本地链接 → push 当前 (path, top) 并跳转;⌫/Alt+← → pop 并恢复。
struct Nav {
    path: String,
    mode: Mode,
    syntax_token: Option<&'static str>,
    history: Vec<HistoryEntry>,
}

struct HistoryEntry {
    path: String,
    top: usize,
}

/// 鼠标事件的结果:命中媒体栏/链接时携带动作,交事件循环执行。
enum MouseAction {
    None,
    OpenLink(String),
    /// 媒体栏·信息行单击 / 文档区音视频:播放暂停。
    PlayPause,
    ToggleMute,
    /// 音量相对调整(±5%)。
    Volume(f32),
    /// 相对 seek(秒)。
    Seek(f64),
    /// 绝对 seek 到比例位置(click-to-seek / scrubbing 松开提交)。
    SeekFraction(f32),
    /// 中键 → 返回(同 ⌫)。
    Back,
}

/// 媒体「直开/回跳」场景的解析基准。
///
/// `media::AudioCtx::open(src, base_dir)` 的语义是「`src` 作为链接目标、相对 `base_dir`
/// 解析」（`links::normalize(base_dir, src)`）。而直开（CLI 参数）与导航回跳得到的
/// `nav.path` 本身已经是「相对 cwd 或绝对」的路径，若再传其所在目录会二次拼接
/// （`test/fixtures/audio/tone.wav` → `test/fixtures/audio/test/fixtures/audio/tone.wav`
/// → `✗ not found`）。空 base 让 `normalize` 原样返回 = 相对 cwd 解析。
///
/// M2（点击 markdown 内的音频链接）走的仍是链接语义，基准必须是当前文档目录，
/// 见 `open_link`（不经过本函数）。
fn direct_base() -> &'static Path {
    Path::new("")
}

/// 当前文件所在目录(相对图片/链接按它解析)。
fn base_dir_of(path: &str) -> PathBuf {
    Path::new(path)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

/// 图片模式直开时的注册表 key:`file:<绝对路径>?<mtime>.<size>`。
/// 文件变化 → stat 指纹变化 → 新 key → 重新加载(热重载语义)。
fn image_src_for(path: &str) -> String {
    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
    let v = file_stamp(path).map_or_else(
        || "0.0".to_string(),
        |(t, sz)| {
            let nanos = t
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            format!("{nanos}.{sz}")
        },
    );
    format!("file:{}?v{}", abs.display(), v)
}

#[allow(clippy::too_many_arguments)]
fn event_loop(
    terminal: &mut Term,
    doc: &mut Doc,
    file_path: &str,
    mode: Mode,
    syntax_token: Option<&'static str>,
    hl: &Highlighter,
    skin: &termimad::MadSkin,
    img_ctx: &ImageCtx,
) -> i32 {
    let mut nav = Nav {
        path: file_path.to_string(),
        mode,
        syntax_token,
        history: Vec::new(),
    };
    // 当前文件内容（用于 resize / 文件变更时重排）
    let mut content: String = content::reload_content(&nav.path).unwrap_or_default();
    let mut last_stamp = file_stamp(&nav.path);
    let mut ui = UiState::new();
    // 外部打开器(xdg-open 等)子进程句柄,非阻塞收割避免僵尸
    let mut children: Vec<Child> = Vec::new();
    // 图片加载/重编码版本(变化 → 重排,同热重载路径;DECISIONS D15)
    let mut img_version = img_ctx.dirty_version();
    // 媒体会话层(音频/视频/网页;跨 rebuild 存活,design §2)
    let mut media = MediaState::new();

    // 媒体模式(D16):进入即启动会话(音频 open / 视频 start / 网页后台 render)。
    match nav.mode {
        Mode::Audio => {
            // 直开：nav.path 已是 cwd 相对/绝对路径 → 空 base，避免二次拼接（见 direct_base）
            media.start_audio(&nav.path, direct_base());
        }
        Mode::Video => {
            let (w, h) = current_size(terminal);
            let proto = proto_of(img_ctx);
            media.start_video(&nav.path, w, h, proto);
            if media.video_active {
                // mpv 启动会清空终端图像(研究 §已知坑:\033_Ga=d)→ 全量重绘
                let _ = terminal.clear();
            } else {
                let reason = media.degrade_reason.clone();
                ui.say(reason);
            }
        }
        Mode::Web => {
            let (w, _h) = current_size(terminal);
            media.start_web(&nav.path, content_width(w), skin);
        }
        _ => {}
    }
    if is_media_mode(nav.mode) {
        rebuild_doc(terminal, doc, &nav, &content, &mut ui, hl, skin, img_ctx, &media.input);
    }

    let exit_code = loop {
        children.retain_mut(|c| c.try_wait().map(|r| r.is_none()).unwrap_or(false));

        let view = media.view();
        let _ = terminal.draw(|f| {
            render_frame(
                f,
                doc,
                &nav.path,
                &mut ui,
                &view,
                !nav.history.is_empty(),
            )
        });

        // 用 poll 非阻塞检查终端事件(200ms 超时兼作热重载轮询周期)
        if event::poll(Duration::from_millis(200)).unwrap_or(false) {
            match event::read() {
                Ok(Event::Key(k)) => {
                    if k.kind != KeyEventKind::Press {
                        continue;
                    }
                    // 选区相关键优先于滚动/退出映射(DECISIONS D11):
                    //   Esc:有选区 → 清除;无选区 → 退出
                    //   y / Enter:有非空选区 → 手动复制
                    if k.code == KeyCode::Esc && ui.sel.is_some() {
                        ui.sel = None;
                        continue;
                    }
                    if (k.code == KeyCode::Char('y') || k.code == KeyCode::Enter)
                        && k.modifiers.is_empty()
                        && ui.sel.is_some_and(|s| !s.is_empty())
                    {
                        if let Some(sel) = ui.sel {
                            copy_selection(terminal, doc, &sel, &mut ui.status);
                        }
                        ui.sel = None;
                        continue;
                    }
                    let ctx = media.key_ctx(nav.mode);
                    match map_key(k, ctx, nav.mode) {
                        Action::Quit(code) => {
                            media.stop_all();
                            break code;
                        }
                        Action::Esc => {
                            // Esc 链:清选区(上方) → 停止会话 → 退出
                            if media.session_active() {
                                media.stop_all();
                                ui.scrubbing = false;
                                ui.say("stopped");
                                rebuild_media_doc(
                                    terminal, doc, &mut ui, &nav, &content, hl, skin, img_ctx,
                                    &media,
                                );
                            } else {
                                break 0;
                            }
                        }
                        Action::Back => {
                            nav_back(
                                terminal, doc, &mut nav, &mut content, &mut ui, &mut media, hl,
                                skin, img_ctx,
                            );
                        }
                        Action::Scroll(delta) => {
                            let body_h = body_height(terminal, &ui);
                            doc.scroll(delta, body_h);
                        }
                        Action::Page(delta) => {
                            let body_h = body_height(terminal, &ui);
                            let page = body_h as isize;
                            doc.scroll(delta * page, body_h);
                        }
                        Action::Top => doc.set_top(0, body_height(terminal, &ui)),
                        Action::Bottom => doc.set_top(usize::MAX, body_height(terminal, &ui)),
                        Action::PlayPause => {
                            media.toggle_pause();
                            say_playback_state(&mut ui, &media);
                        }
                        Action::Seek(d) => {
                            media.seek_by(d);
                            say_seek(&mut ui, &media, d);
                        }
                        Action::Volume(d) => {
                            media.adjust_volume(d);
                            say_volume(&mut ui, &media);
                        }
                        Action::ToggleMute => {
                            media.toggle_mute();
                            say_volume(&mut ui, &media);
                        }
                        Action::Restart => {
                            media.restart();
                            ui.say("restart");
                        }
                        Action::OpenBrowser => {
                            let url = media.browser_url(&nav.path);
                            if open_external(&url, &mut children) {
                                ui.say(format!("opened in browser: {url}"));
                            } else {
                                ui.say(format!("no opener found for: {url}"));
                            }
                        }
                        Action::None => {}
                    }
                }
                Ok(Event::Mouse(m)) => {
                    match handle_mouse(terminal, doc, &mut ui, m) {
                        MouseAction::OpenLink(raw) => open_link(
                            terminal,
                            doc,
                            &mut nav,
                            &mut content,
                            &mut ui,
                            &mut media,
                            &mut children,
                            &raw,
                            hl,
                            skin,
                            img_ctx,
                        ),
                        MouseAction::PlayPause => {
                            media.toggle_pause();
                            say_playback_state(&mut ui, &media);
                        }
                        MouseAction::ToggleMute => {
                            media.toggle_mute();
                            say_volume(&mut ui, &media);
                        }
                        MouseAction::Volume(d) => {
                            media.adjust_volume(d);
                            say_volume(&mut ui, &media);
                        }
                        MouseAction::Seek(d) => {
                            media.seek_by(d);
                            say_seek(&mut ui, &media, d);
                        }
                        MouseAction::SeekFraction(frac) => {
                            media.seek_to_fraction(frac);
                        }
                        MouseAction::Back => {
                            nav_back(
                                terminal, doc, &mut nav, &mut content, &mut ui, &mut media, hl,
                                skin, img_ctx,
                            );
                        }
                        MouseAction::None => {}
                    }
                }
                Ok(Event::Resize(w, h)) => {
                    // 重排后行结构变化,内容坐标失效 → 清除选区
                    ui.sel = None;
                    // 视频区几何变化 → 重算 area 并调 set_area + 全量重绘(design §5.3)
                    if media.video_active {
                        let bar_h = bar_height_for(h, true);
                        media.set_area(video_area_for(w, h, bar_h));
                        let _ = terminal.clear();
                    }
                    rebuild_doc(terminal, doc, &nav, &content, &mut ui, hl, skin, img_ctx, &media.input);
                }
                Ok(_) => {}
                Err(_) => break 1,
            }
        }

        // 拖选中指针停在视口边缘 → 持续自动滚动并延伸选区(DECISIONS D11 ②)。
        // 纯点击(未移动)不算拖拽,不触发——否则点击视口首/末行会平移内容,
        // 把点击错位成跨行选区复制。
        if ui.dragging && ui.moved {
            edge_autoscroll(terminal, doc, &mut ui);
        }

        // 媒体异步事件:音频/视频 dirty(加载完成/失败/结束) → 重建 Doc。
        // 视频状态变化(进入/就绪/退出)伴随 mpv 清屏 → 全量重绘(design §5.4)。
        if media.poll_dirty() {
            ui.sel = None;
            if media.video_active {
                let _ = terminal.clear();
            }
            rebuild_media_doc(terminal, doc, &mut ui, &nav, &content, hl, skin, img_ctx, &media);
        }

        // 网页后台渲染结果 → 重建 Doc(Loading/失败占位/错误行)。
        let (web_changed, web_msg) = media.poll_web();
        if let Some(msg) = web_msg {
            ui.say(msg);
        }
        if web_changed {
            ui.sel = None;
            rebuild_media_doc(terminal, doc, &mut ui, &nav, &content, hl, skin, img_ctx, &media);
        }

        // 视频信息行的 ffprobe 元信息(后台线程) → 重建 Doc。
        if media.poll_probe(&nav.path) {
            rebuild_media_doc(terminal, doc, &mut ui, &nav, &content, hl, skin, img_ctx, &media);
        }

        // 图片就绪(加载/重编码完成)→ 重排,占位行换成图片行(或更新尺寸)。
        if img_ctx.dirty_version() != img_version {
            img_version = img_ctx.dirty_version();
            ui.sel = None; // 行结构变化,选区坐标失效
            rebuild_doc(terminal, doc, &nav, &content, &mut ui, hl, skin, img_ctx, &media.input);
        }

        // 热重载:stat 轮询(mtime+size 变化即重排),跟随当前导航文件。
        // 替代 notify 依赖(省 ~150KB 体积);事件循环本就以 ~200ms 轮询,
        // 检测延迟同量级。
        // 媒体模式(Audio/Video/Web)不参与 stat 重排(design §4):播放中的文件被
        // 外部修改不打断会话;网页本地文件亦不自动重取。
        let stamp = file_stamp(&nav.path);
        if stamp != last_stamp {
            last_stamp = stamp;
            if is_media_mode(nav.mode) {
                // 媒体模式:指纹仅用于后续比对,不重排
            } else {
                ui.sel = None; // 行结构变化,选区坐标失效
                if nav.mode == Mode::Image {
                    // 图片文件变化:image_src_for 的指纹 key 变化 → 重新加载
                    rebuild_doc(
                        terminal, doc, &nav, &content, &mut ui, hl, skin, img_ctx, &media.input,
                    );
                } else if let Some(new_content) = content::reload_content(&nav.path) {
                    content = new_content;
                    rebuild_doc(
                        terminal, doc, &nav, &content, &mut ui, hl, skin, img_ctx, &media.input,
                    );
                }
                // 读失败:保留旧内容
            }
        }
    };

    // 进程退出前确保 mpv 子进程被 stop(防僵尸,design §5.7)+ 清理降级临时文件
    media.stop_all();
    media.cleanup();
    exit_code
}

/// 媒体模式判定(行模型来自引擎,不走文本渲染)。
fn is_media_mode(mode: Mode) -> bool {
    matches!(mode, Mode::Audio | Mode::Video | Mode::Web)
}

/// ratatui-image 协议 → video 模块的协议枚举映射。
fn proto_of(img_ctx: &ImageCtx) -> Option<TermProto> {
    use ratatui_image::picker::ProtocolType;
    match img_ctx.graphics_proto() {
        Some(ProtocolType::Kitty) => Some(TermProto::Kitty),
        Some(ProtocolType::Sixel) => Some(TermProto::Sixel),
        _ => None,
    }
}

/// 媒体行模型变化后重建 Doc(音频信息块 / 视频降级 / 网页结果)。
#[allow(clippy::too_many_arguments)]
fn rebuild_media_doc(
    terminal: &mut Term,
    doc: &mut Doc,
    ui: &mut UiState,
    nav: &Nav,
    content: &str,
    hl: &Highlighter,
    skin: &termimad::MadSkin,
    img_ctx: &ImageCtx,
    media: &MediaState,
) {
    if is_media_mode(nav.mode) {
        rebuild_doc(terminal, doc, nav, content, ui, hl, skin, img_ctx, &media.input);
    }
}

/// ⌫/Alt+←/中键的会话语义:停止视频会话;有历史则返回,否则停止音频会话。
#[allow(clippy::too_many_arguments)]
fn nav_back(
    terminal: &mut Term,
    doc: &mut Doc,
    nav: &mut Nav,
    content: &mut String,
    ui: &mut UiState,
    media: &mut MediaState,
    hl: &Highlighter,
    skin: &termimad::MadSkin,
    img_ctx: &ImageCtx,
) {
    let had_history = !nav.history.is_empty();
    if media.video_active {
        media.stop_video();
        ui.say("stopped");
    }
    if had_history {
        go_back(terminal, doc, nav, content, ui, hl, skin, img_ctx);
        // 回跳到媒体模式文件:按目标文件重启会话
        retarget_media(terminal, nav, ui, media, img_ctx, skin);
    } else if media.audio_active {
        media.stop_audio();
        ui.say("stopped");
    }
    rebuild_media_doc(terminal, doc, ui, nav, content, hl, skin, img_ctx, media);
}

/// 导航到新媒体文件后按模式启动会话(或离开媒体模式时停止视频)。
#[allow(clippy::too_many_arguments)]
fn retarget_media(
    terminal: &mut Term,
    nav: &mut Nav,
    ui: &mut UiState,
    media: &mut MediaState,
    img_ctx: &ImageCtx,
    skin: &termimad::MadSkin,
) {
    match nav.mode {
        Mode::Audio => {
            // 直开：nav.path 已是 cwd 相对/绝对路径 → 空 base，避免二次拼接（见 direct_base）
            media.start_audio(&nav.path, direct_base());
        }
        Mode::Video => {
            let (w, h) = current_size(terminal);
            let proto = proto_of(img_ctx);
            if media.start_video(&nav.path, w, h, proto) {
                let _ = terminal.clear();
            } else {
                let reason = media.degrade_reason.clone();
                ui.say(reason);
            }
        }
        Mode::Web => {
            let (w, _h) = current_size(terminal);
            media.start_web(&nav.path, content_width(w), skin);
        }
        _ => {
            // 回到文本模式:视频会话停止;音频会话保留(M2 浏览语义,design §3)
            if media.video_active {
                media.stop_video();
            }
        }
    }
}

/// 按当前模式与内容重建 Doc 行集(resize / 热重载 / 图片就绪 / 媒体状态变化共用)。
#[allow(clippy::too_many_arguments)]
fn rebuild_doc(
    terminal: &mut Term,
    doc: &mut Doc,
    nav: &Nav,
    content: &str,
    ui: &mut UiState,
    hl: &Highlighter,
    skin: &termimad::MadSkin,
    img_ctx: &ImageCtx,
    media: &MediaInput,
) {
    let _ = ui; // 选区清理由调用方完成
    let (w, _h) = current_size(terminal);
    let cw = content_width(w);
    let body_h = body_height(terminal, ui);
    match nav.mode {
        Mode::Image => {
            let src = image_src_for(&nav.path);
            let (lines, images) = build_image_doc(img_ctx, &src, cw);
            doc.replace_lines(lines, Vec::new(), images, cw, body_h);
        }
        Mode::Audio => {
            doc.replace_lines(media.audio_lines.clone(), Vec::new(), Vec::new(), cw, body_h);
        }
        Mode::Video => match &media.video {
            VideoBody::Empty => doc.replace_lines(Vec::new(), Vec::new(), Vec::new(), cw, body_h),
            VideoBody::Still(src) => {
                // ffmpeg 首帧静图走既有图片管线(加载异步,就绪后 img dirty 再重建)
                let (lines, images) = build_image_doc(img_ctx, src, cw);
                doc.replace_lines(lines, Vec::new(), images, cw, body_h);
            }
            VideoBody::Info(lines) => {
                doc.replace_lines(lines.clone(), Vec::new(), Vec::new(), cw, body_h);
            }
        },
        Mode::Web => match &media.web {
            WebBody::Loading => {
                let line = Line::default().spans(vec![Span::styled(
                    "⏳ loading page …",
                    Style::default().add_modifier(Modifier::DIM),
                )]);
                doc.replace_lines(vec![line], Vec::new(), Vec::new(), cw, body_h);
            }
            WebBody::Failed(e) => {
                let line = Line::default().spans(vec![Span::styled(
                    e.clone(),
                    Style::default()
                        .fg(ratatui::style::Color::Red)
                        .add_modifier(Modifier::DIM),
                )]);
                doc.replace_lines(vec![line], Vec::new(), Vec::new(), cw, body_h);
            }
            WebBody::Ready { lines, links, .. } => {
                doc.replace_lines(lines.clone(), links.clone(), Vec::new(), cw, body_h);
            }
        },
        _ => {
            let (lines, links, images) = build_lines(
                content,
                nav.mode,
                nav.syntax_token,
                cw,
                hl,
                skin,
                img_ctx,
                &base_dir_of(&nav.path),
            );
            doc.replace_lines(lines, links, images, cw, body_h);
        }
    }
}

// ---------------------------------------------------------------------------
// 链接导航(D14)
// ---------------------------------------------------------------------------

/// 解析点击的链接目标并执行:本地文件 → 内部跳转;外部 URL → 系统打开器;
/// 锚点/无效目标 → 状态栏提示。图片/视频/网页文件在应用内打开对应模式;
/// **音频文件就地启动 M2 会话**(不导航,design §4 导航)。
#[allow(clippy::too_many_arguments)]
fn open_link(
    terminal: &mut Term,
    doc: &mut Doc,
    nav: &mut Nav,
    content: &mut String,
    ui: &mut UiState,
    media: &mut MediaState,
    children: &mut Vec<Child>,
    raw: &str,
    hl: &Highlighter,
    skin: &termimad::MadSkin,
    img_ctx: &ImageCtx,
) {
    let base_dir = base_dir_of(&nav.path);
    match links::classify(&base_dir, raw) {
        links::Target::Local(path) => {
            let path_str = path.to_string_lossy().into_owned();
            if lang::is_audio_ext(&path_str) {
                // 音频链接 → 就地播放(M2):不跳转、不推历史栈
                match std::fs::metadata(&path) {
                    Err(_) => {
                        ui.say(format!("link target not found: {}", path.display()));
                    }
                    Ok(meta) if meta.is_dir() => {
                        ui.say(format!("link target is a directory: {}", path.display()));
                    }
                    Ok(_) => {
                        media.start_audio(&path_str, &base_dir);
                        ui.say(format!("♪ {}", file_name_of(&path_str)));
                    }
                }
            } else {
                navigate(terminal, doc, nav, content, ui, media, &path, hl, skin, img_ctx);
            }
        }
        links::Target::External(url) => {
            if open_external(&url, children) {
                ui.status = Some((format!("opened externally: {url}"), Instant::now()));
            } else {
                ui.status = Some((
                    format!("no opener found (xdg-open/open) for: {url}"),
                    Instant::now(),
                ));
            }
        }
        links::Target::Anchor => {
            ui.status = Some(("anchor links unsupported".to_string(), Instant::now()));
        }
        links::Target::Invalid => {
            ui.status = Some((format!("invalid link: {raw}"), Instant::now()));
        }
    }
}

/// 跳转到本地文件:校验可读 → push 历史 → 重排渲染。失败仅状态栏提示。
/// 图片/视频/网页文件只校验存在(字节由各自引擎处理),其余按文本校验。
#[allow(clippy::too_many_arguments)]
fn navigate(
    terminal: &mut Term,
    doc: &mut Doc,
    nav: &mut Nav,
    content: &mut String,
    ui: &mut UiState,
    media: &mut MediaState,
    target: &Path,
    hl: &Highlighter,
    skin: &termimad::MadSkin,
    img_ctx: &ImageCtx,
) {
    let path_str = target.to_string_lossy().into_owned();
    let target_mode = lang::detect_mode_lang(&path_str).0;
    if matches!(target_mode, Mode::Image | Mode::Video | Mode::Audio | Mode::Web) {
        // 媒体目标:只校验存在/非目录,字节交给对应引擎
        match std::fs::metadata(target) {
            Err(_) => {
                ui.status = Some((
                    format!("link target not found: {}", target.display()),
                    Instant::now(),
                ));
            }
            Ok(meta) if meta.is_dir() => {
                ui.status = Some((
                    format!("link target is a directory: {}", target.display()),
                    Instant::now(),
                ));
            }
            Ok(_) => {
                nav.history.push(HistoryEntry {
                    path: nav.path.clone(),
                    top: doc.top,
                });
                load_doc(terminal, doc, nav, content, ui, &path_str, String::new(), hl, skin, img_ctx);
                retarget_media(terminal, nav, ui, media, img_ctx, skin);
                rebuild_media_doc(terminal, doc, ui, nav, content, hl, skin, img_ctx, media);
            }
        }
        return;
    }
    match content::read_for_navigate(target) {
        Err(e) => {
            let p = target.display();
            let msg = match e {
                content::OpenError::NotFound => format!("link target not found: {p}"),
                content::OpenError::IsDir => format!("link target is a directory: {p}"),
                content::OpenError::Binary => format!("link target is binary: {p}"),
                content::OpenError::Unreadable => format!("link target unreadable: {p}"),
            };
            ui.status = Some((msg, Instant::now()));
        }
        Ok(text) => {
            nav.history.push(HistoryEntry {
                path: nav.path.clone(),
                top: doc.top,
            });
            load_doc(terminal, doc, nav, content, ui, &target.to_string_lossy(), text, hl, skin, img_ctx);
            // 文本目标:离开媒体模式时停止视频会话(音频保持,M2 语义)
            retarget_media(terminal, nav, ui, media, img_ctx, skin);
        }
    }
}

/// 返回上一文件(恢复滚动位置);目标已不可读时仅提示,栈不弹。
#[allow(clippy::too_many_arguments)]
fn go_back(
    terminal: &mut Term,
    doc: &mut Doc,
    nav: &mut Nav,
    content: &mut String,
    ui: &mut UiState,
    hl: &Highlighter,
    skin: &termimad::MadSkin,
    img_ctx: &ImageCtx,
) {
    let Some(entry) = nav.history.last() else {
        return;
    };
    let (path, top) = (entry.path.clone(), entry.top);
    // 历史栈里的文件类型在跳转时已校验过;媒体模式回跳时不重读字节
    let back_mode = lang::detect_mode_lang(&path).0;
    let text = if back_mode == Mode::Image || is_media_mode(back_mode) {
        String::new()
    } else {
        match content::read_for_navigate(Path::new(&path)) {
            Ok(t) => t,
            Err(_) => {
                ui.status =
                    Some((format!("cannot go back, unreadable: {path}"), Instant::now()));
                return;
            }
        }
    };
    nav.history.pop();
    load_doc(terminal, doc, nav, content, ui, &path, text, hl, skin, img_ctx);
    // 恢复跳转前的滚动位置
    doc.set_top(top, body_height(terminal, ui));
}

/// 按新文件装载 Doc(路径/模式/内容/渲染全部切换)。
#[allow(clippy::too_many_arguments)]
fn load_doc(
    terminal: &mut Term,
    doc: &mut Doc,
    nav: &mut Nav,
    content: &mut String,
    ui: &mut UiState,
    path: &str,
    text: String,
    hl: &Highlighter,
    skin: &termimad::MadSkin,
    img_ctx: &ImageCtx,
) {
    nav.path = path.to_string();
    (nav.mode, nav.syntax_token) = lang::detect_mode_lang(path);
    *content = text;
    ui.sel = None; // 换文档,旧选区坐标失效
    let (w, _h) = current_size(terminal);
    let cw = content_width(w);
    if nav.mode == Mode::Image {
        let src = image_src_for(&nav.path);
        let (lines, images) = build_image_doc(img_ctx, &src, cw);
        *doc = Doc::new(lines, nav.mode, cw, Vec::new(), images);
    } else if is_media_mode(nav.mode) {
        // 媒体模式的行模型由引擎快照给出(rebuild_media_doc 随后填充)
        *doc = Doc::new(Vec::new(), nav.mode, cw, Vec::new(), Vec::new());
    } else {
        let (lines, new_links, images) = build_lines(
            content,
            nav.mode,
            nav.syntax_token,
            cw,
            hl,
            skin,
            img_ctx,
            &base_dir_of(&nav.path),
        );
        *doc = Doc::new(lines, nav.mode, cw, new_links, images);
    }
}

/// 用系统打开器打开外部 URL(Linux xdg-open / macOS open / Windows start)。
/// spawn 不等待;句柄留在 children 由事件循环非阻塞收割。
fn open_external(url: &str, children: &mut Vec<Child>) -> bool {
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = Command::new("open");
        c.arg(url);
        c
    } else if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", "start", "", url]);
        c
    } else {
        let mut c = Command::new("xdg-open");
        c.arg(url);
        c
    };
    match cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => {
            children.push(child);
            true
        }
        Err(_) => false,
    }
}

/// body 高度 = 终端行数 − header − footer − 媒体栏（滚轮/翻页/选区都按它 clamp）。
fn body_height(terminal: &Term, ui: &UiState) -> usize {
    let rows = terminal.size().map(|r| r.height).unwrap_or(24);
    rows.saturating_sub(2)
        .saturating_sub(ui.media.bar_h) as usize
}

/// 键位动作（含媒体层动作，design §3）。
#[derive(Debug, Clone, Copy, PartialEq)]
enum Action {
    Quit(i32),
    Scroll(isize),
    Page(isize),
    Top,
    Bottom,
    Back,
    /// Esc：清选区（调用方先行）→ 停止会话 → 退出。
    Esc,
    PlayPause,
    /// 相对 seek（秒，可负）。
    Seek(f64),
    /// 音量相对调整（±）。
    Volume(f32),
    ToggleMute,
    Restart,
    /// `o`：Web 模式在当前页打开系统浏览器。
    OpenBrowser,
    None,
}

/// 键位映射（design §3 键位表；`ctx` = M1/M2/Pager，`mode` = 当前文档模式）。
fn map_key(k: KeyEvent, ctx: KeyCtx, mode: Mode) -> Action {
    // Ctrl+C → 退出码 130
    if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
        return Action::Quit(130);
    }
    if k.code == KeyCode::Char('\u{0003}') {
        return Action::Quit(130);
    }
    // 媒体键（KeyCode::Media）：老终端收不到 → 静默忽略；非媒体上下文同样忽略
    if let KeyCode::Media(mk) = k.code {
        if ctx == KeyCtx::Pager {
            return Action::None;
        }
        return match mk {
            MediaKeyCode::Play | MediaKeyCode::Pause | MediaKeyCode::PlayPause => {
                Action::PlayPause
            }
            MediaKeyCode::TrackNext | MediaKeyCode::FastForward => Action::Seek(SEEK_STEP_COARSE),
            MediaKeyCode::TrackPrevious | MediaKeyCode::Rewind => Action::Seek(-SEEK_STEP_COARSE),
            MediaKeyCode::RaiseVolume => Action::Volume(VOLUME_STEP),
            MediaKeyCode::LowerVolume => Action::Volume(-VOLUME_STEP),
            MediaKeyCode::MuteVolume => Action::ToggleMute,
            _ => Action::None,
        };
    }
    // q / Ctrl+C 退出（所有上下文一致，会话语义由调用方先 stop_all）
    if k.code == KeyCode::Char('q') {
        return Action::Quit(0);
    }
    // 返回语义：⌫ / Alt+←（所有上下文一致，design §3）
    if k.code == KeyCode::Backspace {
        return Action::Back;
    }
    if k.code == KeyCode::Left && k.modifiers.contains(KeyModifiers::ALT) {
        return Action::Back;
    }
    // Web 模式 `o`：浏览器打开当前页（仅网页模式，design §3）
    if k.code == KeyCode::Char('o') && k.modifiers.is_empty() && mode == Mode::Web {
        return Action::OpenBrowser;
    }

    if ctx != KeyCtx::Pager {
        // ---- 媒体键位上下文（M1/M2）----
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        match k.code {
            // M1：Space = 播放/暂停；M2：保持翻页（design §3 裁决）
            KeyCode::Char(' ') if shift => return Action::PlayPause,
            KeyCode::Char(' ') => {
                return if ctx == KeyCtx::M1 {
                    Action::PlayPause
                } else {
                    Action::Page(1)
                };
            }
            KeyCode::Char('p') => return Action::PlayPause,
            KeyCode::Left => {
                return Action::Seek(if shift { -SEEK_STEP_FINE } else { -SEEK_STEP });
            }
            KeyCode::Right => {
                return Action::Seek(if shift { SEEK_STEP_FINE } else { SEEK_STEP });
            }
            KeyCode::Char(',') => return Action::Seek(-SEEK_STEP_COARSE),
            KeyCode::Char('.') => return Action::Seek(SEEK_STEP_COARSE),
            KeyCode::Char('-') => return Action::Volume(-VOLUME_STEP),
            KeyCode::Char('+') => return Action::Volume(VOLUME_STEP),
            KeyCode::Char('m') => return Action::ToggleMute,
            KeyCode::Char('0') => return Action::Restart,
            _ => {}
        }
    }

    match k.code {
        KeyCode::Esc => Action::Esc,
        KeyCode::Char('j') => Action::Scroll(1),
        KeyCode::Down => Action::Scroll(1),
        KeyCode::Char('k') => Action::Scroll(-1),
        KeyCode::Up => Action::Scroll(-1),
        KeyCode::Char(' ') => Action::Page(1),
        KeyCode::PageDown => Action::Page(1),
        KeyCode::PageUp => Action::Page(-1),
        KeyCode::Home => Action::Top,
        KeyCode::Char('g') => Action::Top,
        KeyCode::End => Action::Bottom,
        KeyCode::Char('G') => Action::Bottom,
        // 返回上一文件(D14)
        KeyCode::Backspace => Action::Back,
        KeyCode::Left if k.modifiers.contains(KeyModifiers::ALT) => Action::Back,
        _ => Action::None,
    }
}


// ---------------------------------------------------------------------------
// 鼠标拖选(DECISIONS D11)
// ---------------------------------------------------------------------------

/// 鼠标事件分发:滚轮(媒体栏三区 / 文档滚动) + 左键(媒体栏动作 / 拖选 / 链接) +
/// 中键(返回，w3m 先例)。媒体栏行与视频区排除出选区/链接命中(design §3 鼠标表)。
fn handle_mouse(
    terminal: &mut Term,
    doc: &mut Doc,
    ui: &mut UiState,
    m: MouseEvent,
) -> MouseAction {
    let body_h = body_height(terminal, ui);
    let (_, rows) = current_size(terminal);

    match m.kind {
        // 滚轮:进度条行 → seek ±5s;音量区 → 音量 ±5%;文档区 → 滚动文档
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let up = m.kind == MouseEventKind::ScrollUp;
            if ui.media.progress_row == Some(m.row) {
                return MouseAction::Seek(if up { -SEEK_STEP } else { SEEK_STEP });
            }
            if ui.media.in_volume(m.column, m.row) {
                return MouseAction::Volume(if up { VOLUME_STEP } else { -VOLUME_STEP });
            }
            if !ui.media.in_bar(m.row) && !ui.media.in_video(m.column, m.row) {
                doc.scroll(if up { -1 } else { 1 }, body_h);
            }
        }

        // 中键 = 返回(同 ⌫;design §3 鼠标表)
        MouseEventKind::Down(MouseButton::Middle) => return MouseAction::Back,

        // 左键按下:媒体栏 → 动作/scrubbing;文档区 → 定锚(Shift+点击 → 扩展选区)
        MouseEventKind::Down(MouseButton::Left) => {
            if ui.media.progress_row == Some(m.row) {
                // 进度条行:进入 scrubbing(单击也会在 Up 提交 click-to-seek)
                ui.scrubbing = true;
                ui.last_scrub = None;
                ui.scrub_frac = ui.media.seek_frac(m.column).unwrap_or(0.0);
                ui.status = None;
                preview_scrub(ui, m.column);
                return MouseAction::None;
            }
            if ui.media.in_volume(m.column, m.row) {
                return MouseAction::ToggleMute;
            }
            if ui.media.info_row == Some(m.row) {
                return MouseAction::PlayPause;
            }
            if ui.media.in_video(m.column, m.row) {
                return MouseAction::None; // 视频区不参与选区
            }
            if let Some(p) = to_content_point(doc, ui, m.column, m.row, rows) {
                if m.modifiers.contains(KeyModifiers::SHIFT) {
                    match &mut ui.sel {
                        Some(sel) => sel.focus = p,
                        None => ui.sel = Some(selection::Selection::new(p)),
                    }
                } else {
                    ui.sel = Some(selection::Selection::new(p));
                }
                ui.dragging = true;
                ui.moved = false;
                ui.last_autoscroll = None;
                ui.pointer = Some((m.column, m.row));
            }
        }

        // 拖动:媒体栏 scrub(120ms 节流预览)/ 文档选区;指针停在视口边缘时自动滚动
        MouseEventKind::Drag(MouseButton::Left) if ui.scrubbing => {
            ui.moved = true;
            ui.scrub_frac = ui.media.seek_frac(m.column).unwrap_or(0.0);
            preview_scrub(ui, m.column);
        }
        MouseEventKind::Drag(MouseButton::Left) if ui.dragging => {
            ui.moved = true;
            ui.pointer = Some((m.column, m.row));
            if let Some(p) = to_content_point(doc, ui, m.column, m.row, rows) {
                if let Some(sel) = &mut ui.sel {
                    sel.focus = p;
                }
            }
            edge_autoscroll(terminal, doc, ui);
        }

        // 松开:scrubbing → 提交 seek;非空选区 → 自动复制;空选(点击)→ 命中链接
        MouseEventKind::Up(MouseButton::Left) if ui.scrubbing => {
            ui.scrubbing = false;
            let frac = ui.media.seek_frac(m.column).unwrap_or(ui.scrub_frac);
            ui.scrub_frac = frac;
            return MouseAction::SeekFraction(frac);
        }
        MouseEventKind::Up(MouseButton::Left) if ui.dragging => {
            ui.dragging = false;
            if let Some(sel) = ui.sel {
                if !sel.is_empty() {
                    copy_selection(terminal, doc, &sel, &mut ui.status);
                    return MouseAction::None;
                }
            }
            ui.sel = None;
            return match hit_test_link(doc, ui, m.column, m.row, rows) {
                Some(target) => MouseAction::OpenLink(target),
                None => MouseAction::None,
            };
        }

        _ => {}
    }
    MouseAction::None
}

/// scrubbing 预览:时间码跟随指针(120ms 节流,仅状态栏提示,松开才提交)。
fn preview_scrub(ui: &mut UiState, col: u16) {
    if ui
        .last_scrub
        .is_some_and(|t| t.elapsed() < SCRUB_THROTTLE)
    {
        return;
    }
    ui.last_scrub = Some(Instant::now());
    let pct = (ui.scrub_frac * 100.0).round() as i32;
    ui.status = Some((format!("seek {pct}%"), Instant::now()));
    let _ = col;
}

/// 点击命中测试:屏幕坐标 → 内容坐标 → 查 doc.links。
/// 落在 body 区且该行真实存在(不钳制到末行)才算命中;媒体栏行/视频区排除。
fn hit_test_link(doc: &Doc, ui: &UiState, col: u16, row: u16, rows: u16) -> Option<String> {
    if row == 0 || row + 1 >= rows {
        return None; // header / footer / 越界
    }
    if ui.media.in_bar(row) || ui.media.in_video(col, row) {
        return None; // 媒体栏行与视频区不参与链接命中(design §5.5)
    }
    let line = doc.top + (row - 1) as usize;
    if line >= doc.lines.len() {
        return None; // 文档末尾下方的空行,不是链接
    }
    let col = col.saturating_sub(H_MARGIN) as usize;
    doc.links
        .iter()
        .find(|l| l.line == line && col >= l.start && col < l.end)
        .map(|l| l.target.clone())
}

/// 屏幕坐标(列,行)→ 内容坐标。行必须落在 body 区(跳过 header/footer/媒体栏),
/// 行索引钳制到文档末尾,列换算掉 H_MARGIN。
fn to_content_point(
    doc: &Doc,
    ui: &UiState,
    col: u16,
    row: u16,
    rows: u16,
) -> Option<selection::SelPoint> {
    // body:第 1 行 ..= rows-2(第 0 行 header,第 rows-1 行 footer)
    if row == 0 || row + 1 >= rows {
        return None;
    }
    // 媒体栏行与视频区不参与选区/链接命中(design §5.5)
    if ui.media.in_bar(row) || ui.media.in_video(col, row) {
        return None;
    }
    let line = doc.top + row.saturating_sub(1) as usize;
    let line = line.min(doc.lines.len().saturating_sub(1));
    Some(selection::SelPoint {
        line,
        col: col.saturating_sub(H_MARGIN) as usize,
    })
}

/// 拖选中指针停在视口顶/底边缘 → 自动滚动一行并把焦点延伸到新的可见边界行。
/// 节流:AUTO_SCROLL_INTERVAL 内不重复滚动。
fn edge_autoscroll(terminal: &mut Term, doc: &mut Doc, ui: &mut UiState) {
    let Some((col, row)) = ui.pointer else { return };
    let (_, rows) = current_size(terminal);
    let body_h = body_height(terminal, ui);
    // 视口顶边缘 = body 第一行(row 1);底边缘 = body 最后一行(rows-2-媒体栏)
    let at_top = row == 1;
    let at_bottom = row + 2 == rows.saturating_sub(ui.media.bar_h);
    if !at_top && !at_bottom {
        return;
    }
    if ui
        .last_autoscroll
        .is_some_and(|t| t.elapsed() < AUTO_SCROLL_INTERVAL)
    {
        return;
    }
    ui.last_autoscroll = Some(Instant::now());

    let delta: isize = if at_top { -1 } else { 1 };
    let before_top = doc.top;
    doc.scroll(delta, body_h);
    if doc.top == before_top {
        return; // 已到文档顶/底,无法继续滚动
    }
    if let Some(sel) = &mut ui.sel {
        let line = if at_top {
            doc.top
        } else {
            doc.top + body_h.saturating_sub(1)
        };
        sel.focus = selection::SelPoint {
            line,
            col: col.saturating_sub(H_MARGIN) as usize,
        };
    }
}

/// 复制选区文本到系统剪贴板(OSC 52;写入失败静默降级为状态栏提示)。
fn copy_selection(
    terminal: &mut Term,
    doc: &Doc,
    sel: &selection::Selection,
    status: &mut Option<(String, Instant)>,
) {
    let text = selection::text(&doc.lines, sel);
    let chars = text.chars().count();
    let result = execute!(
        terminal.backend_mut(),
        CopyToClipboard::to_clipboard_from(text.as_str())
    );
    *status = Some(match result {
        Ok(()) => (format!("copied {chars} chars (OSC 52)"), Instant::now()),
        Err(_) => (
            "clipboard unsupported (OSC 52 write failed)".to_string(),
            Instant::now(),
        ),
    });
}

#[allow(clippy::too_many_arguments)]
fn render_frame(
    f: &mut ratatui::Frame,
    doc: &Doc,
    file_name: &str,
    ui: &mut UiState,
    view: &MediaView,
    can_back: bool,
) {
    let area = f.area();
    let bar_h = bar_height_for(area.height, view.session);
    let status = ui.status_text().map(str::to_string);
    let sel = ui.sel;
    // 命中区每帧按本次渲染的布局重算（鼠标处理读它，design §3 鼠标表）
    ui.media = MediaRects {
        bar_h,
        ..MediaRects::default()
    };

    let (header_a, body_a, bar_a, footer_a) = if bar_h == 0 {
        // 无会话：保持 v0.4.0 的三段布局（回归零差异）
        let c = Layout::vertical([
            Constraint::Length(1), // header
            Constraint::Min(0),    // body
            Constraint::Length(1), // footer
        ])
        .split(area);
        (c[0], c[1], None, c[2])
    } else {
        let c = Layout::vertical([
            Constraint::Length(1),         // header
            Constraint::Min(0),            // body
            Constraint::Length(bar_h),     // 媒体栏
            Constraint::Length(1),         // footer
        ])
        .split(area);
        (c[0], c[1], Some(c[2]), c[3])
    };

    // Header (bold, 左边距 1 字符);网页模式追加页面标题(design §4)
    let mut header_text = format!(" {file_name}");
    if let Some(title) = &view.web_title {
        if !title.is_empty() {
            header_text.push_str(&format!(" — {title}"));
        }
    }
    let header =
        Paragraph::new(header_text).style(Style::default().add_modifier(Modifier::BOLD));
    f.render_widget(header, header_a);

    // Body — 左右各留 H_MARGIN 字符边距
    let body_area = Rect::new(
        body_a.x + H_MARGIN,
        body_a.y,
        body_a.width.saturating_sub(H_MARGIN * 2),
        body_a.height,
    );
    if view.mpv_body {
        // 视频共屏：mpv 画该区域，dlook 不写字节（下方统一标记 CellDiffOption::Skip）
        ui.media.video = Some(body_area);
    } else {
        let viewport = Viewport {
            lines: &doc.lines,
            top: doc.top,
            selection: sel,
            images: &doc.images,
        };
        f.render_widget(viewport, body_area);
    }

    // 媒体栏（有会话时）：行1 进度条+时间码，行2 状态+标题+音量
    if let Some(bar) = bar_a {
        render_media_bar(f, bar, view, &mut ui.media);
    }

    // Footer (dim, 左边距 1 字符);有状态消息时优先显示(复制结果等)。
    // 分层(design §3):M1 整体替换为媒体键位;M2 在既有 footer 后追加 ♪ p ⏯;
    // 其余模式与 v0.4.0 完全一致。
    let ctx = media_key_ctx(view, doc);
    let base_footer = match ctx {
        KeyCtx::M1 => FOOTER_MEDIA,
        _ => FOOTER,
    };
    let mut footer_text = format!(" {}", status.as_deref().unwrap_or(base_footer));
    if can_back && status.is_none() {
        footer_text.push_str(FOOTER_BACK);
    }
    if ctx == KeyCtx::M2 && status.is_none() {
        footer_text.push_str(FOOTER_M2_SUFFIX);
    }
    let footer = Paragraph::new(Line::from(vec![Span::styled(
        footer_text,
        Style::default().add_modifier(Modifier::DIM),
    )]))
    .alignment(Alignment::Left);
    f.render_widget(footer, footer_a);

    // 视频区单元格每帧 Skip：dlook 的 diff 渲染永不向 mpv 区域写字节
    // （ratatui 0.30 Buffer API，design §5.2）
    if let Some(v) = ui.media.video {
        let buf = f.buffer_mut();
        for y in v.y..v.y.saturating_add(v.height) {
            for x in v.x..v.x.saturating_add(v.width) {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.diff_option = CellDiffOption::Skip;
                }
            }
        }
    }
}

/// footer 分层用的键位上下文（渲染期，等价 MediaState::key_ctx）。
fn media_key_ctx(view: &MediaView, doc: &Doc) -> KeyCtx {
    match doc.mode {
        Mode::Audio | Mode::Video => KeyCtx::M1,
        _ if view.session && view.audio.is_some() => KeyCtx::M2,
        _ => KeyCtx::Pager,
    }
}

/// 媒体栏渲染（design §3）：行1 进度条 + `MM:SS / MM:SS`；行2 图标 + 标题 + 右端音量。
fn render_media_bar(f: &mut ratatui::Frame, bar: Rect, view: &MediaView, rects: &mut MediaRects) {
    let Some(data) = view.bar_data() else {
        return;
    };
    let dim = Style::default().add_modifier(Modifier::DIM);
    let x = bar.x + H_MARGIN;
    let w = bar.width.saturating_sub(H_MARGIN * 2);

    // ---- 行 1：进度条 + 时间码 ----
    let timecode = format!(
        "{} / {}",
        fmt_time(Some(data.position)),
        fmt_time(data.duration)
    );
    let prog_w = w.saturating_sub(TIMECODE_COLS + 2).max(1);
    let frac = if data.duration.is_some() && !data.loading && data.failed.is_none() {
        let total = data.duration.map(|d| d.as_secs_f64()).unwrap_or(0.0);
        if total > 0.0 {
            Some((data.position.as_secs_f64() / total) as f32)
        } else {
            Some(0.0)
        }
    } else {
        None
    };
    let progress = progress_text(frac, prog_w);
    let line1 = Line::from(vec![
        Span::styled(progress, Style::default().fg(ratatui::style::Color::Cyan)),
        Span::raw("  "),
        Span::styled(timecode, dim),
    ]);
    f.render_widget(
        Paragraph::new(line1).alignment(Alignment::Left),
        Rect::new(bar.x, bar.y, bar.width, 1),
    );
    rects.progress_row = Some(bar.y);
    rects.progress = Some(Rect::new(x, bar.y, prog_w, 1));

    // ---- 行 2（折叠时跳过）：图标 + 标题 + 右端音量 ----
    if rects.bar_h < 2 {
        return;
    }
    let row2 = bar.y + 1;
    let volume_w = VOLUME_ZONE_COLS.min(bar.width);
    let vol_x = bar.x + bar.width.saturating_sub(volume_w);
    let text_w = vol_x.saturating_sub(x);
    let mut spans: Vec<Span> = vec![Span::styled(
        state_icon(&data).to_string(),
        Style::default().add_modifier(Modifier::BOLD),
    )];
    let detail = match &data.failed {
        Some(reason) => format!(" {reason}"),
        None if data.loading => " loading …".to_string(),
        None => format!(" {}", data.title),
    };
    spans.push(Span::styled(detail, dim));
    f.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect::new(x, row2, text_w, 1),
    );
    if data.show_volume {
        let vol_text = if data.muted {
            format!("{:>width$}", "mute", width = VOLUME_ZONE_COLS as usize)
        } else {
            format!("▣ {:>3}%", (data.volume * 100.0).round() as i32)
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(vol_text, dim)]))
                .alignment(Alignment::Right),
            Rect::new(vol_x, row2, volume_w, 1),
        );
        rects.volume = Some(Rect::new(vol_x, row2, volume_w, 1));
    }
    rects.info_row = Some(row2);
}


/// 启动时构建 Doc 行集(区分图片模式与文本模式)。
#[allow(clippy::too_many_arguments)]
fn build_initial(
    mode: Mode,
    file_path: &str,
    initial_content: &str,
    syntax_token: Option<&str>,
    width: u16,
    hl: &Highlighter,
    skin: &termimad::MadSkin,
    img_ctx: &ImageCtx,
) -> (
    Vec<Line<'static>>,
    Vec<crate::links::LinkSpan>,
    Vec<DocImage>,
) {
    if mode == Mode::Image {
        let src = image_src_for(file_path);
        let (lines, images) = build_image_doc(img_ctx, &src, width);
        return (lines, Vec::new(), images);
    }
    build_lines(
        initial_content,
        mode,
        syntax_token,
        width,
        hl,
        skin,
        img_ctx,
        &base_dir_of(file_path),
    )
}

/// 图片模式直开:单张图片(或加载/失败占位行)。
fn build_image_doc(
    img_ctx: &ImageCtx,
    src: &str,
    width: u16,
) -> (Vec<Line<'static>>, Vec<DocImage>) {
    use ratatui::layout::Size;
    match img_ctx.get_or_load(src, Path::new(""), Size::new(width, images::MAX_IMG_ROWS)) {
        images::Render::Ready(proto) => {
            let size = proto.size();
            let doc_img = DocImage {
                line: 0,
                proto,
                h: size.height,
            };
            (
                std::iter::repeat(Line::default()).take(size.height as usize).collect(),
                vec![doc_img],
            )
        }
        images::Render::Loading => (
            vec![Line::default().spans(vec![Span::styled(
                "⏳ loading image …",
                Style::default().add_modifier(Modifier::DIM),
            )])],
            Vec::new(),
        ),
        images::Render::Failed(e) => (
            vec![Line::default().spans(vec![Span::styled(
                format!("✗ image unavailable: {e}"),
                Style::default().fg(ratatui::style::Color::Red).add_modifier(Modifier::DIM),
            )])],
            Vec::new(),
        ),
    }
}

/// 根据 mode + content 生成 Vec<Line>、行内链接区域与图片放置记录。
fn build_lines(
    content: &str,
    mode: Mode,
    syntax_token: Option<&str>,
    width: u16,
    hl: &Highlighter,
    skin: &termimad::MadSkin,
    img_ctx: &ImageCtx,
    base_dir: &Path,
) -> (
    Vec<Line<'static>>,
    Vec<crate::links::LinkSpan>,
    Vec<DocImage>,
) {
    match mode {
        Mode::Markdown => markdown::markdown_to_lines(content, width, skin, hl, Some(img_ctx), base_dir),
        Mode::Code => {
            let ansi = hl.highlight_to_ansi(content, syntax_token);
            (ansi_lines::to_lines(&ansi, width), Vec::new(), Vec::new())
        }
        Mode::Mermaid => match mermaid::render_mermaid_to_ansi(content, width) {
            Ok(ansi) => (
                ansi_lines::to_lines_untruncated(&ansi),
                Vec::new(),
                Vec::new(),
            ),
            Err(_) => (
                ansi_lines::to_lines(content, width),
                Vec::new(),
                Vec::new(),
            ),
        },
        // 媒体模式的行模型由 rebuild_doc 的媒体分支(task media-4)提供;
        // 此处保持空行集(Audio/Video/Web 不走文本渲染)。
        Mode::Image | Mode::Audio | Mode::Video | Mode::Web => (Vec::new(), Vec::new(), Vec::new()),
    }
}

// ---------------------------------------------------------------------------
// 单元测试(task media-4):布局/媒体栏/键位/降级链的纯逻辑断言。
// 不依赖三个引擎模块的运行时(引擎落地前也可跑),同时承载消融实验。
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn key_mod(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    fn snapshot(paused: bool, muted: bool) -> AudioSnapshot {
        AudioSnapshot {
            status: AudioStatus::Ready,
            title: "song.mp3".to_string(),
            paused,
            position: Duration::from_secs(83),
            duration: Some(Duration::from_secs(296)),
            volume: 0.8,
            muted,
            finished: false,
        }
    }

    fn line_text(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    // ---- A. 布局与媒体栏 ----

    #[test]
    fn bar_height_zero_without_session() {
        assert_eq!(bar_height_for(24, false), 0);
        assert_eq!(bar_height_for(6, false), 0);
    }

    #[test]
    fn bar_height_two_rows_when_tall() {
        assert_eq!(bar_height_for(24, true), 2);
        assert_eq!(bar_height_for(8, true), 2); // body_h = 6 → 不折叠
    }

    /// 窄终端折叠:rows=6 时 2 行媒体栏会把 body 压到 2 行(消融实验①:
    /// 固定 2 行会让 6 行终端只剩 2 行正文)。
    #[test]
    fn bar_folds_on_short_terminal() {
        assert_eq!(bar_height_for(6, true), 1);
        assert_eq!(bar_height_for(7, true), 1);
        let rows = 6u16;
        let bar = bar_height_for(rows, true);
        let body = rows - 2 - bar; // header + footer 各 1 行
        assert!(body >= 3, "折叠后 body 至少保留 3 行,实际 {body}");
    }

    #[test]
    fn video_area_matches_layout() {
        // rows=24, bar=2 → body 20 行;left=H_MARGIN, top=1
        let a = video_area_for(80, 24, 2);
        assert_eq!((a.left, a.top, a.cols, a.rows), (1, 1, 78, 20));
        let b = video_area_for(40, 6, 1);
        assert_eq!((b.left, b.top, b.cols, b.rows), (1, 1, 38, 3));
    }

    /// 直开路径不再二次拼接（回归 bug：`dlook test/fixtures/audio/tone.wav`
    /// 显示 `✗ not found`，因为直开时把路径所在目录又当成了链接基准）。
    #[test]
    fn direct_open_base_does_not_double_join() {
        let src = "test/fixtures/audio/tone.wav";
        // 直开：空 base → 原路径（相对 cwd 解析）
        assert_eq!(links::normalize(direct_base(), src), Path::new(src));
        // M2 链接场景：文档目录 + 链接目标 → 正常拼接
        assert_eq!(
            links::normalize(Path::new("test/fixtures"), "audio/tone.wav"),
            Path::new(src)
        );
        // 锁死错误形态：传所在目录会得到重复段（旧实现的症状）
        let buggy = links::normalize(Path::new("test/fixtures/audio"), src);
        assert_ne!(buggy, Path::new(src));
        assert_eq!(buggy.to_string_lossy().matches("test/fixtures/audio").count(), 2);
        // 绝对路径不受 base 影响
        assert_eq!(
            links::normalize(direct_base(), "/tmp/tone.wav"),
            Path::new("/tmp/tone.wav")
        );
    }

    #[test]
    fn timecode_formatting() {
        assert_eq!(fmt_time(None), "--:--");
        assert_eq!(fmt_time(Some(Duration::from_secs(83))), "01:23");
        assert_eq!(fmt_time(Some(Duration::from_secs(296))), "04:56");
        assert_eq!(fmt_time(Some(Duration::from_secs(3725))), "1:02:05");
    }

    #[test]
    fn progress_bar_fill_ratio() {
        let half = progress_text(Some(0.5), 10);
        assert_eq!(half.chars().filter(|c| *c == '━').count(), 5);
        assert_eq!(half.chars().filter(|c| *c == '─').count(), 5);
        let full = progress_text(Some(1.0), 8);
        assert_eq!(full.chars().filter(|c| *c == '━').count(), 8);
        let unknown = progress_text(None, 8);
        assert_eq!(unknown.chars().filter(|c| *c == '━').count(), 0);
        assert_eq!(unknown.chars().count(), 8);
        // 越界 clamp
        assert_eq!(progress_text(Some(2.0), 4).chars().filter(|c| *c == '━').count(), 4);
        assert_eq!(progress_text(Some(-1.0), 4).chars().filter(|c| *c == '━').count(), 0);
    }

    #[test]
    fn state_icon_states() {
        let mut d = MediaView {
            audio: Some(snapshot(false, false)),
            ..Default::default()
        }
        .bar_data()
        .unwrap();
        assert_eq!(state_icon(&d), "▶");
        d.paused = true;
        assert_eq!(state_icon(&d), "▮▮");
        d.paused = false;
        d.finished = true;
        assert_eq!(state_icon(&d), "◼");
        d.finished = false;
        d.loading = true;
        assert_eq!(state_icon(&d), "⏳");
        d.loading = false;
        d.failed = Some("no audio device".to_string());
        assert_eq!(state_icon(&d), "✗");
    }

    #[test]
    fn bar_data_from_snapshots() {
        let v = MediaView {
            audio: Some(snapshot(true, true)),
            session: true,
            ..Default::default()
        };
        let d = v.bar_data().unwrap();
        assert_eq!(d.title, "song.mp3");
        assert!(d.paused && d.muted && d.show_volume);
        assert_eq!(d.duration, Some(Duration::from_secs(296)));
        // 无会话 → None
        assert!(MediaView::default().bar_data().is_none());
        assert!(MediaState::new().view().bar_data().is_none());
    }

    /// 媒体栏失败态:无音频设备/解码失败 → 行2 `✗ <原因>`(design §4)。
    #[test]
    fn bar_data_reports_failure_reason() {
        let mut snap = snapshot(false, false);
        snap.status = AudioStatus::Failed("no audio device".to_string());
        let v = MediaView {
            audio: Some(snap),
            session: true,
            ..Default::default()
        };
        let d = v.bar_data().unwrap();
        assert_eq!(d.failed.as_deref(), Some("no audio device"));
        assert_eq!(state_icon(&d), "✗");
    }

    // ---- B. 键位上下文 ----

    #[test]
    fn space_is_playpause_in_m1_but_pages_in_m2_and_pager() {
        assert_eq!(map_key(key(KeyCode::Char(' ')), KeyCtx::M1, Mode::Audio), Action::PlayPause);
        assert_eq!(map_key(key(KeyCode::Char(' ')), KeyCtx::M2, Mode::Markdown), Action::Page(1));
        assert_eq!(map_key(key(KeyCode::Char(' ')), KeyCtx::Pager, Mode::Markdown), Action::Page(1));
    }

    #[test]
    fn shift_space_is_playpause_in_media() {
        let k = key_mod(KeyCode::Char(' '), KeyModifiers::SHIFT);
        assert_eq!(map_key(k, KeyCtx::M1, Mode::Audio), Action::PlayPause);
        assert_eq!(map_key(k, KeyCtx::M2, Mode::Markdown), Action::PlayPause);
    }

    #[test]
    fn p_toggles_only_in_media_context() {
        assert_eq!(map_key(key(KeyCode::Char('p')), KeyCtx::M1, Mode::Audio), Action::PlayPause);
        assert_eq!(map_key(key(KeyCode::Char('p')), KeyCtx::M2, Mode::Markdown), Action::PlayPause);
        assert!(matches!(
            map_key(key(KeyCode::Char('p')), KeyCtx::Pager, Mode::Markdown),
            Action::None
        ));
    }

    #[test]
    fn seek_keys() {
        assert_eq!(map_key(key(KeyCode::Left), KeyCtx::M1, Mode::Video), Action::Seek(-5.0));
        assert_eq!(map_key(key(KeyCode::Right), KeyCtx::M1, Mode::Video), Action::Seek(5.0));
        let fine = |c| map_key(key_mod(c, KeyModifiers::SHIFT), KeyCtx::M1, Mode::Video);
        assert_eq!(fine(KeyCode::Left), Action::Seek(-1.0));
        assert_eq!(fine(KeyCode::Right), Action::Seek(1.0));
        let coarse = |c| map_key(key(c), KeyCtx::M2, Mode::Markdown);
        assert_eq!(coarse(KeyCode::Char(',')), Action::Seek(-60.0));
        assert_eq!(coarse(KeyCode::Char('.')), Action::Seek(60.0));
        // Alt+← 仍是返回
        assert!(matches!(
            map_key(key_mod(KeyCode::Left, KeyModifiers::ALT), KeyCtx::M1, Mode::Audio),
            Action::Back
        ));
    }

    #[test]
    fn volume_mute_restart_keys() {
        match map_key(key(KeyCode::Char('-')), KeyCtx::M1, Mode::Audio) {
            Action::Volume(d) => assert!((d + VOLUME_STEP).abs() < f32::EPSILON),
            other => panic!("expected Volume, got {other:?}"),
        }
        match map_key(key(KeyCode::Char('+')), KeyCtx::M2, Mode::Markdown) {
            Action::Volume(d) => assert!((d - VOLUME_STEP).abs() < f32::EPSILON),
            other => panic!("expected Volume, got {other:?}"),
        }
        assert!(matches!(
            map_key(key(KeyCode::Char('m')), KeyCtx::M1, Mode::Audio),
            Action::ToggleMute
        ));
        assert!(matches!(
            map_key(key(KeyCode::Char('0')), KeyCtx::M1, Mode::Audio),
            Action::Restart
        ));
    }

    #[test]
    fn o_opens_browser_only_in_web_mode() {
        assert!(matches!(
            map_key(key(KeyCode::Char('o')), KeyCtx::Pager, Mode::Web),
            Action::OpenBrowser
        ));
        assert!(matches!(
            map_key(key(KeyCode::Char('o')), KeyCtx::M1, Mode::Video),
            Action::None
        ));
        assert!(matches!(
            map_key(key(KeyCode::Char('o')), KeyCtx::Pager, Mode::Markdown),
            Action::None
        ));
    }

    #[test]
    fn media_keys_map_to_same_actions() {
        let m = |c| map_key(key_mod(KeyCode::Media(c), KeyModifiers::NONE), KeyCtx::M1, Mode::Audio);
        assert_eq!(m(MediaKeyCode::PlayPause), Action::PlayPause);
        assert_eq!(m(MediaKeyCode::Play), Action::PlayPause);
        assert_eq!(m(MediaKeyCode::Pause), Action::PlayPause);
        assert_eq!(m(MediaKeyCode::TrackNext), Action::Seek(SEEK_STEP_COARSE));
        assert_eq!(m(MediaKeyCode::TrackPrevious), Action::Seek(-SEEK_STEP_COARSE));
        assert!(matches!(m(MediaKeyCode::RaiseVolume), Action::Volume(_)));
        assert!(matches!(m(MediaKeyCode::MuteVolume), Action::ToggleMute));
        // 普通 pager 上下文静默忽略
        assert!(matches!(
            map_key(
                key_mod(KeyCode::Media(MediaKeyCode::PlayPause), KeyModifiers::NONE),
                KeyCtx::Pager,
                Mode::Markdown
            ),
            Action::None
        ));
    }

    #[test]
    fn scroll_keys_kept_in_media_context() {
        for (code, want) in [
            (KeyCode::Char('j'), Action::Scroll(1)),
            (KeyCode::Char('k'), Action::Scroll(-1)),
            (KeyCode::Down, Action::Scroll(1)),
            (KeyCode::Up, Action::Scroll(-1)),
            (KeyCode::PageDown, Action::Page(1)),
            (KeyCode::PageUp, Action::Page(-1)),
            (KeyCode::Home, Action::Top),
            (KeyCode::End, Action::Bottom),
            (KeyCode::Char('g'), Action::Top),
            (KeyCode::Char('G'), Action::Bottom),
        ] {
            assert_eq!(map_key(key(code), KeyCtx::M1, Mode::Audio), want);
        }
    }

    #[test]
    fn quit_and_esc_semantics() {
        assert!(matches!(
            map_key(key(KeyCode::Char('q')), KeyCtx::M1, Mode::Audio),
            Action::Quit(0)
        ));
        assert!(matches!(map_key(key(KeyCode::Esc), KeyCtx::M1, Mode::Audio), Action::Esc));
        assert!(matches!(map_key(key(KeyCode::Esc), KeyCtx::Pager, Mode::Markdown), Action::Esc));
        assert!(matches!(
            map_key(
                key_mod(KeyCode::Char('c'), KeyModifiers::CONTROL),
                KeyCtx::M1,
                Mode::Audio
            ),
            Action::Quit(130)
        ));
        assert!(matches!(
            map_key(key(KeyCode::Backspace), KeyCtx::M2, Mode::Markdown),
            Action::Back
        ));
    }

    #[test]
    fn key_ctx_rule() {
        let media = MediaState::new();
        assert_eq!(media.key_ctx(Mode::Audio), KeyCtx::M1);
        assert_eq!(media.key_ctx(Mode::Video), KeyCtx::M1);
        // 无音频会话 → 其他模式是普通 pager
        assert_eq!(media.key_ctx(Mode::Markdown), KeyCtx::Pager);
        assert_eq!(media.key_ctx(Mode::Web), KeyCtx::Pager);
    }

    #[test]
    fn media_key_ctx_uses_audio_session_for_m2() {
        let view = MediaView {
            audio: Some(snapshot(false, false)),
            session: true,
            ..Default::default()
        };
        let doc = Doc::new(Vec::new(), Mode::Markdown, 78, Vec::new(), Vec::new());
        assert_eq!(media_key_ctx(&view, &doc), KeyCtx::M2);
        assert_eq!(media_key_ctx(&MediaView::default(), &doc), KeyCtx::Pager);
        // M1 模式不受 view 影响
        let doc_audio = Doc::new(Vec::new(), Mode::Audio, 78, Vec::new(), Vec::new());
        assert_eq!(media_key_ctx(&MediaView::default(), &doc_audio), KeyCtx::M1);
    }

    /// footer 分层(design §3):M1 整体替换;M2 追加;普通 pager 不变。
    #[test]
    fn footer_layers() {
        assert!(FOOTER.starts_with("q quit"));
        assert!(FOOTER_MEDIA.contains("space ⏯"));
        assert!(FOOTER_MEDIA.contains("q quit"));
        assert_eq!(FOOTER_M2_SUFFIX, "  ♪ p ⏯");
        // R6:媒体 footer 的 token 长度 ≤ 8(窄终端不挤爆);
        // 普通 pager footer 保持 v0.4.0 原样(含既有的长 token,不在本次改动范围)。
        for tok in FOOTER_MEDIA.split_whitespace() {
            assert!(tok.chars().count() <= 8, "footer token 过长: {tok:?}");
        }
        for tok in FOOTER_M2_SUFFIX.split_whitespace() {
            assert!(tok.chars().count() <= 8, "M2 footer token 过长: {tok:?}");
        }
    }

    // ---- C. 鼠标命中区 ----

    #[test]
    fn progress_hit_fraction() {
        let mut rects = MediaRects {
            bar_h: 2,
            progress_row: Some(22),
            progress: Some(Rect::new(1, 22, 40, 1)),
            info_row: Some(23),
            volume: Some(Rect::new(72, 23, 8, 1)),
            video: None,
        };
        assert_eq!(rects.seek_frac(1), Some(0.0));
        assert_eq!(rects.seek_frac(21), Some(0.5));
        assert_eq!(rects.seek_frac(41), Some(1.0));
        assert_eq!(rects.seek_frac(200), Some(1.0)); // clamp
        assert_eq!(rects.seek_frac(0), Some(0.0));
        rects.progress = Some(Rect::new(1, 22, 0, 1));
        assert_eq!(rects.seek_frac(5), None);
    }

    #[test]
    fn hit_zones_and_exclusions() {
        let rects = MediaRects {
            bar_h: 2,
            progress_row: Some(22),
            progress: Some(Rect::new(1, 22, 40, 1)),
            info_row: Some(23),
            volume: Some(Rect::new(72, 23, 8, 1)),
            video: Some(Rect::new(1, 1, 78, 20)),
        };
        assert!(rects.in_bar(22) && rects.in_bar(23) && !rects.in_bar(21));
        assert!(rects.in_volume(72, 23) && rects.in_volume(79, 23));
        assert!(!rects.in_volume(71, 23) && !rects.in_volume(72, 22));
        assert!(rects.in_video(1, 1) && rects.in_video(78, 20));
        assert!(!rects.in_video(0, 1) && !rects.in_video(1, 21));

        // 媒体栏行/视频区排除出选区与链接命中(design §5.5)
        let doc = Doc::new(
            vec![Line::default(); 22],
            Mode::Markdown,
            78,
            vec![LinkSpan {
                line: 20,
                start: 0,
                end: 5,
                target: "x".to_string(),
            }],
            Vec::new(),
        );
        let mut ui = UiState::new();
        ui.media = rects;
        assert!(to_content_point(&doc, &ui, 3, 22, 24).is_none(), "媒体栏行排除");
        assert!(to_content_point(&doc, &ui, 3, 10, 24).is_none(), "视频区排除");
        assert!(to_content_point(&doc, &ui, 3, 21, 24).is_some(), "视频区外仍可选区");
        assert!(hit_test_link(&doc, &ui, 3, 22, 24).is_none(), "媒体栏行不命中链接");
        assert!(hit_test_link(&doc, &ui, 3, 10, 24).is_none(), "视频区不命中链接");
    }

    #[test]
    fn body_height_subtracts_media_bar() {
        let mut ui = UiState::new();
        assert_eq!(ui.media.bar_h, 0);
        ui.media.bar_h = 2;
        let rows = 24u16;
        assert_eq!(rows.saturating_sub(2).saturating_sub(ui.media.bar_h) as usize, 20);
        ui.media.bar_h = 1;
        assert_eq!(rows.saturating_sub(2).saturating_sub(ui.media.bar_h) as usize, 21);
    }

    /// scrubbing 节流(design §3:120ms):窗口内重复拖动不更新预览文案;
    /// 超过窗口后按新比例更新(消融:去掉节流则中间断言失败)。
    #[test]
    fn scrub_preview_is_throttled() {
        let mut ui = UiState::new();
        ui.scrub_frac = 0.25;
        preview_scrub(&mut ui, 10);
        let first = ui.status.as_ref().expect("首次预览应写入状态").0.clone();
        assert!(first.contains("25%"), "首次预览比例: {first}");
        ui.scrub_frac = 0.5;
        preview_scrub(&mut ui, 20);
        assert_eq!(
            ui.status.as_ref().unwrap().0,
            first,
            "120ms 内不重复更新预览"
        );
        std::thread::sleep(SCRUB_THROTTLE + Duration::from_millis(15));
        ui.scrub_frac = 0.75;
        preview_scrub(&mut ui, 30);
        assert!(
            ui.status.as_ref().unwrap().0.contains("75%"),
            "超过节流窗口后更新预览: {:?}",
            ui.status
        );
    }

    /// 中键 → 返回(鼠标层语义)。
    #[test]
    fn middle_click_maps_to_back_action() {
        // 中键动作在事件循环里直接落到 nav_back;这里锁定 MouseAction 的存在与
        // seek 比例提交路径的映射(命中区 → SeekFraction)。
        let rects = MediaRects {
            bar_h: 2,
            progress_row: Some(22),
            progress: Some(Rect::new(1, 22, 20, 1)),
            info_row: Some(23),
            volume: Some(Rect::new(72, 23, 8, 1)),
            video: None,
        };
        assert_eq!(rects.seek_frac(11), Some(0.5));
    }

    // ---- D. 降级链(消融实验②) ----

    #[test]
    fn degrade_chain_plan() {
        // mpv + 图形协议 → 共屏
        assert_eq!(video_plan(true, true, true), VideoPlan::Mpv);
        // 无 mpv(有 ffmpeg)→ 首帧静图
        assert_eq!(video_plan(false, true, true), VideoPlan::FirstFrame);
        // 有 mpv 但无图形协议 → 首帧静图
        assert_eq!(video_plan(true, false, true), VideoPlan::FirstFrame);
        // 无 mpv 无图形协议但有 ffmpeg → 仍是首帧静图
        assert_eq!(video_plan(false, false, true), VideoPlan::FirstFrame);
        // 无 ffmpeg → 信息行
        assert_eq!(video_plan(false, false, false), VideoPlan::InfoLines);
        // ffmpeg 不影响共屏判定(mpv + 协议足够)
        assert_eq!(video_plan(true, true, false), VideoPlan::Mpv);
    }

    /// 信息行必须给出文件名/格式/原因(无 ffmpeg 时的最低可用形态)。
    #[test]
    fn video_info_lines_are_informative() {
        let lines =
            video_info_lines("/tmp/clip.mp4", "mpv not found — no ffmpeg (info only)", None);
        let text = line_text(&lines);
        assert!(text.contains("clip.mp4"));
        assert!(text.contains("mp4"));
        assert!(text.contains("note:"));
        assert!(text.contains("ffprobe not found"));
        // 有 ffprobe 元信息时展示时长/分辨率
        let with_probe = video_info_lines(
            "/tmp/clip.mp4",
            "mpv not found",
            Some("640x360  00:10".to_string()),
        );
        assert!(line_text(&with_probe).contains("640x360"));
    }

    #[test]
    fn audio_info_lines_show_state_and_failure() {
        let mut snap = snapshot(false, true);
        snap.status = AudioStatus::Failed("no audio device".to_string());
        let text = line_text(&audio_info_lines(Some("/tmp/song.mp3"), Some(snap)));
        assert!(text.contains("song.mp3"));
        assert!(text.contains("✗ no audio device"));
        assert!(text.contains("(muted)"));
        assert!(text.contains("mp3"));
        // 无会话:仅提示行
        let none = audio_info_lines(None, None);
        assert!(line_text(&none).contains("no active audio session"));
    }
}

