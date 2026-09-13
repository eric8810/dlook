//! 音频播放引擎(DECISIONS D16):rodio 薄封装,隔离上游 API 面。
//!
//! 设计要点(研究依据见 docs/research/media/):
//!   - rodio 0.22 正在引擎重写(0.22 已把 `Sink` 破坏性改为 `Player`),本模块是
//!     **唯一的 rodio 接触面**——UI/事件循环只通过本模块的类型与函数交互,
//!     上游 API 变动时改动半径限于本文件(研究 §4 薄封装要求)。
//!   - `open()` 的后台加载:本地文件直接打开(解码器惰性流式读),http(s) URL 先
//!     下载到临时文件再打开;完成/失败通过 dirty_version() 通知事件循环重绘。
//!   - 播放控制(pause/play/seek/volume)是轻量调用(rodio 文档:try_seek 阻塞 0–5ms),
//!     直接在 UI 线程执行,不引入额外线程。
//!   - 无音频设备 / 解码失败:置 `AudioStatus::Failed(可读原因)`,不 panic;
//!     UI 以状态行展示(D16 降级要求)。
//!
//! 状态归属:本模块持有播放器与当前会话;Doc/termio 不保存音频状态,只读 snapshot()。

use std::path::Path;
use std::time::Duration;

/// 会话状态(加载 → 就绪 / 失败)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioStatus {
    /// 后台加载中(打开文件 / 下载远程 / 解码器初始化)。
    Loading,
    /// 可播放。
    Ready,
    /// 失败(设备不可用、解码失败、文件缺失等),含可读原因。
    Failed(String),
}

/// UI 读取的音频状态快照(每帧或每次重绘前取一次)。
#[derive(Debug, Clone)]
pub struct AudioSnapshot {
    pub status: AudioStatus,
    /// 显示标题(文件名或 URL 尾段)。
    pub title: String,
    pub paused: bool,
    pub position: Duration,
    /// 总时长;未知(如流式源)为 None。
    pub duration: Option<Duration>,
    /// 有效音量 0.0..=1.0(muted 时为 0.0)。
    pub volume: f32,
    pub muted: bool,
    /// 播放自然结束(位置到达末尾且队列空)。
    pub finished: bool,
}

/// 音频上下文:与 ImageCtx 同构的注册表/句柄(跨 rebuild 存活,由事件循环持有)。
pub struct AudioCtx {
    _priv: (),
}

impl Default for AudioCtx {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioCtx {
    /// 创建(不打开设备;首次 open 时才接触音频后端,保证纯文本文档零音频开销)。
    pub fn new() -> Self {
        AudioCtx { _priv: () }
    }

    /// 打开并开始播放一个源(本地路径或 http(s) URL)。
    /// 后台执行;期间 snapshot().status == Loading。重复调用替换当前会话。
    /// `base_dir` 用于解析相对路径(与图片/链接一致)。
    pub fn open(&self, _src: &str, _base_dir: &Path) {
        todo!("media::AudioCtx::open — task media-1")
    }

    /// 停止并释放当前会话(snapshot 变回 None)。
    pub fn close(&self) {
        todo!("media::AudioCtx::close — task media-1")
    }

    /// 当前会话快照;None = 无活动会话。
    pub fn snapshot(&self) -> Option<AudioSnapshot> {
        todo!("media::AudioCtx::snapshot — task media-1")
    }

    /// 状态变化计数(加载完成/失败/自然结束等异步事件 +1);事件循环据此触发重排重绘。
    pub fn dirty_version(&self) -> u64 {
        todo!("media::AudioCtx::dirty_version — task media-1")
    }

    /// 播放/暂停切换。
    pub fn toggle_pause(&self) {
        todo!("media::AudioCtx::toggle_pause — task media-1")
    }

    /// 相对 seek(秒,可负);越界由实现 clamp 到 [0, duration]。
    pub fn seek_by(&self, _delta_secs: f64) {
        todo!("media::AudioCtx::seek_by — task media-1")
    }

    /// 绝对 seek 到比例位置(0.0..=1.0;click-to-seek / scrubbing 用);时长未知时忽略。
    pub fn seek_to_fraction(&self, _frac: f32) {
        todo!("media::AudioCtx::seek_to_fraction — task media-1")
    }

    /// 音量相对调整(如 ±0.05),自动 clamp 到 [0,1];调整会解除静音。
    pub fn adjust_volume(&self, _delta: f32) {
        todo!("media::AudioCtx::adjust_volume — task media-1")
    }

    /// 静音切换(实现:记录静音前的音量,置 0/恢复)。
    pub fn toggle_mute(&self) {
        todo!("media::AudioCtx::toggle_mute — task media-1")
    }

    /// 回到本曲开头并继续播放。
    pub fn restart(&self) {
        todo!("media::AudioCtx::restart — task media-1")
    }
}
