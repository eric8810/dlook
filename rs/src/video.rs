//! 视频播放(DECISIONS D16,委托 mpv):dlook 掌 UI,mpv 做解码与像素输出,控制走 JSON IPC。
//!
//! 设计要点(研究依据见 docs/research/media/evidence-video.md):
//!   - mpv 官方 VO:`--vo=kitty`(v0.36.0 起无条件编译)/`--vo=sixel`(需发行版带 libsixel)。
//!     无图形协议(halfblocks/未知终端)→ 不启动 mpv,由集成层降级为 ffmpeg 首帧静图。
//!   - 区域几何:`--vo-kitty-left/top/rows/cols` 把视频限制在 body 区域(共屏形态 MVP-A);
//!     sixel 的区域能力以原型实验结论为准(experiments/E14)。
//!   - 控制面:JSON 行协议 over unix socket(`--input-ipc-server`,随机临时路径);
//!     状态轮询(每 200ms tick)拿 time-pos/duration/pause/volume。
//!   - 生命周期:start → tick* → stop;stop/退出后必须触发**全量重绘**
//!     (mpv 启动与退出都会发 `\033_Ga=d` 清空终端全部 kitty 图像,研究 §已知坑)。
//!   - 无 mpv:available() = false,集成层走降级链(ffmpeg 首帧 → 信息行)。

use std::time::Duration;

/// 视频显示区域(终端格坐标,相对整个终端;由集成层按布局算出)。
/// left/top 从 0 计;rows/cols 为区域尺寸。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoArea {
    pub left: u16,
    pub top: u16,
    pub cols: u16,
    pub rows: u16,
}

/// 终端图形协议(来自 ratatui-image picker 的探测结论,决定 mpv 的 vo)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermProto {
    Kitty,
    Sixel,
    /// 无图形协议(halfblocks / 未探测 / 禁用):视频不可像素级播放。
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoStatus {
    /// mpv 启动中(子进程已 spawn,等待 IPC socket)。
    Loading,
    Playing,
    Paused,
    /// 播放自然结束(mpv 仍在,可重播;或已退出)。
    Finished,
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct VideoSnapshot {
    pub status: VideoStatus,
    pub position: Duration,
    pub duration: Option<Duration>,
    pub paused: bool,
    /// 音量 0.0..=1.0(来自 mpv `volume`,0–100 归一化)。
    pub volume: f32,
    /// 媒体文件是否含音频轨(无音轨时 UI 不显示音量区)。
    pub has_audio: bool,
}

/// 视频上下文(mpv 会话句柄)。与 ImageCtx/AudioCtx 同构:事件循环持有,tick 驱动。
pub struct VideoCtx {
    _priv: (),
}

impl Default for VideoCtx {
    fn default() -> Self {
        Self::new()
    }
}

impl VideoCtx {
    pub fn new() -> Self {
        VideoCtx { _priv: () }
    }

    /// mpv 是否可用(缓存探测:PATH 中能找到 mpv 且可执行)。
    pub fn available() -> bool {
        todo!("video::VideoCtx::available — task media-3")
    }

    /// 启动播放(后台 spawn + 等待 IPC 就绪;期间 snapshot().status == Loading)。
    /// `src` 为本地路径;`proto` 为 None 时返回错误(集成层改走降级链)。
    pub fn start(&self, _src: &str, _area: VideoArea, _proto: TermProto) -> Result<(), String> {
        todo!("video::VideoCtx::start — task media-3")
    }

    /// 停止(发 quit,等待子进程回收,清理 socket);幂等。
    pub fn stop(&self) {
        todo!("video::VideoCtx::stop — task media-3")
    }

    pub fn toggle_pause(&self) {
        todo!("video::VideoCtx::toggle_pause — task media-3")
    }

    /// 相对 seek(秒,可负);mpv `seek <delta> relative`。
    pub fn seek_by(&self, _delta_secs: f64) {
        todo!("video::VideoCtx::seek_by — task media-3")
    }

    /// 绝对 seek 到比例位置(click-to-seek / scrubbing);mpv `seek <frac> absolute-percent`。
    pub fn seek_to_fraction(&self, _frac: f32) {
        todo!("video::VideoCtx::seek_to_fraction — task media-3")
    }

    /// 音量相对调整(±0.05,mpv volume 为 0–100,内部换算)。
    pub fn adjust_volume(&self, _delta: f32) {
        todo!("video::VideoCtx::adjust_volume — task media-3")
    }

    /// 静音切换(mpv `mute` 属性;实现需记录静音前音量以恢复,与 AudioCtx 语义一致)。
    /// 接口由主 agent 于集成期补充(termio 报告 §3 `m` 键对视频缺失;2026-09-13)。
    pub fn toggle_mute(&self) {
        todo!("video::VideoCtx::toggle_mute — task media-3")
    }

    /// 设置显示区域(resize 时调用;实现按 mpv 能力选择重启或热改属性)。
    pub fn set_area(&self, _area: VideoArea) {
        todo!("video::VideoCtx::set_area — task media-3")
    }

    pub fn snapshot(&self) -> Option<VideoSnapshot> {
        todo!("video::VideoCtx::snapshot — task media-3")
    }

    /// 事件循环每 ~200ms 调用:轮询 mpv 状态、收割已退出的子进程。
    pub fn tick(&self) {
        todo!("video::VideoCtx::tick — task media-3")
    }

    /// 状态变化计数(就绪/失败/结束等异步事件),事件循环据此重绘。
    pub fn dirty_version(&self) -> u64 {
        todo!("video::VideoCtx::dirty_version — task media-3")
    }
}
