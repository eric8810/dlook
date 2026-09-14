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
//!
//! 实现备忘(rodio 0.22.2 实测 API):
//!   - 设备:`DeviceSinkBuilder::open_default_sink()` → `MixerDeviceSink`,
//!     `Player::connect_new(sink.mixer())`;设备句柄须存活到播放结束(持有在 Inner)。
//!   - 惰性:`new()` 不接触音频后端;首次 `open()` 的后台线程才打开设备并把句柄交回
//!     ctx(打开时不持 inner 锁,避免 UI 线程的 `snapshot()` 被设备初始化阻塞)。
//!   - 时长:`Decoder::total_duration()` 必须在 append 前取(`Player` 不提供总时长);
//!     解码器须声明 `with_byte_len()`(可 seek),否则 seek 不可用、mp3/vorbis 的时长
//!     也无法计算——rodio 0.22 的 `Decoder::new()` 默认 Settings 是不可 seek 的。
//!   - 暂停冻结:rodio `Pausable` 暂停时不驱动 `TrackPosition`,故 `get_pos()` 不漂移。

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player, Source};

use crate::links;

/// 远程音频大小上限(64MB,与图片先例同族)。
const MAX_REMOTE_BYTES: u64 = 64 * 1024 * 1024;
/// 远程请求整体超时(与图片先例一致)。
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
/// 默认音量(媒体栏 `▣ 80%`);首次 open 前即可调整,跨会话保持。
const DEFAULT_VOLUME: f32 = 0.8;
/// 自然结束判定容差(队列空 + 位置进入末尾该窗口 → finished)。
const FINISH_EPSILON: Duration = Duration::from_millis(300);

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

// ---------------------------------------------------------------------------
// 内部状态
// ---------------------------------------------------------------------------

/// 一次就绪会话(Ready 态)。
struct Session {
    /// rodio 播放器(源队列 → 设备混音器);drop 即停止播放。
    player: Player,
    title: String,
    duration: Option<Duration>,
    /// 自然结束标志(snapshot() 轮询时置位,只置一次)。
    finished: bool,
    /// 原始来源与基准目录(restart 重建会话时复用)。
    src: String,
    base_dir: PathBuf,
    /// 远程源下载的临时文件:随会话释放而删除(播放中 Linux unlink 后读句柄仍有效)。
    temp: Option<TempFile>,
}

/// 临时文件守卫(drop 即删除,覆盖 close/替换/进程退出路径)。
struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// ctx 会话状态机。
enum State {
    /// 无活动会话(snapshot → None)。
    Idle,
    /// 后台加载中(本地读取 / 远程下载 / 解码器初始化 / 设备打开)。
    Loading { title: String },
    /// 可播放。
    Ready(Box<Session>),
    /// 失败;保留到 close() 或下一次 open()(媒体栏展示 `✗ <原因>`)。
    Failed { title: String, reason: String },
}

/// ctx 全部可变状态(单一 Mutex;后台加载线程先在此取设备再发布结果)。
struct Inner {
    state: State,
    /// open()/close() 序号:后台线程据此丢弃过期结果(重复 open 替换会话)。
    generation: u64,
    /// 惰性打开的默认输出设备;存活至 ctx 释放(未打开/无设备时 None)。
    device: Option<MixerDeviceSink>,
    /// 逻辑音量 0.0..=1.0(未静音时的值;静音只把有效音量置 0)。
    volume: f32,
    muted: bool,
}

/// 跨线程共享句柄(AudioCtx 与后台加载线程共用)。
#[derive(Clone)]
struct Shared {
    inner: Arc<Mutex<Inner>>,
    /// 设备打开串行化:打开时不持有 inner 锁,避免阻塞 UI 线程的快照查询。
    device_open: Arc<Mutex<()>>,
    dirty: Arc<AtomicU64>,
}

impl Shared {
    fn new() -> Self {
        Shared {
            inner: Arc::new(Mutex::new(Inner {
                state: State::Idle,
                generation: 0,
                device: None,
                volume: DEFAULT_VOLUME,
                muted: false,
            })),
            device_open: Arc::new(Mutex::new(())),
            dirty: Arc::new(AtomicU64::new(0)),
        }
    }

    /// 锁内部状态(中毒不级联 panic:状态是自洽的纯数据)。
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn bump_dirty(&self) {
        self.dirty.fetch_add(1, Ordering::SeqCst);
    }

    /// 发布后台线程结果;过期代次(已被更新的 open()/close() 取代)直接丢弃。
    fn publish(&self, generation: u64, next: State) {
        let published = {
            let mut inner = self.lock();
            if inner.generation != generation {
                false
            } else {
                inner.state = next;
                true
            }
        };
        if published {
            self.bump_dirty();
        }
    }

    /// 惰性打开默认输出设备(可被后台线程调用);失败给可读原因。
    fn ensure_device(&self) -> Result<(), String> {
        if self.lock().device.is_some() {
            return Ok(());
        }
        let _opening = self
            .device_open
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        // 双重检查:另一线程可能已打开
        if self.lock().device.is_some() {
            return Ok(());
        }
        let mut sink =
            DeviceSinkBuilder::open_default_sink().map_err(|e| format!("no audio device: {e}"))?;
        // 设备由 ctx 显式持有到进程结束,drop 时无需向 stderr 打提示
        sink.log_on_drop(false);
        self.lock().device = Some(sink);
        Ok(())
    }

    /// 用当前设备建播放器(先 ensure_device;设备缺失视为内部错误)。
    fn new_player(&self) -> Result<Player, String> {
        let inner = self.lock();
        let device = inner
            .device
            .as_ref()
            .ok_or_else(|| "no audio device: not opened".to_string())?;
        Ok(Player::connect_new(device.mixer()))
    }
}

/// 音频上下文:与 ImageCtx 同构的注册表/句柄(跨 rebuild 存活,由事件循环持有)。
pub struct AudioCtx {
    shared: Shared,
}

impl Default for AudioCtx {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioCtx {
    /// 创建(不打开设备;首次 open 时才接触音频后端,保证纯文本文档零音频开销)。
    pub fn new() -> Self {
        AudioCtx {
            shared: Shared::new(),
        }
    }

    /// 打开并开始播放一个源(本地路径或 http(s) URL)。
    /// 后台执行;期间 snapshot().status == Loading。重复调用替换当前会话。
    /// `base_dir` 用于解析相对路径(与图片/链接一致)。
    pub fn open(&self, src: &str, base_dir: &Path) {
        let src = src.trim();
        let title = display_title(src);
        // 停旧会话(drop Player 即停止;临时文件随 Session drop 清理)并进入 Loading
        let generation = {
            let mut inner = self.shared.lock();
            inner.generation += 1;
            inner.state = State::Loading {
                title: title.clone(),
            };
            inner.generation
        };
        let shared = self.shared.clone();
        let owned_src = src.to_string();
        let base = base_dir.to_path_buf();
        let spawned = std::thread::Builder::new()
            .name("dlook-audio-load".into())
            .spawn(move || load_in_background(shared, generation, owned_src, base));
        if let Err(e) = spawned {
            self.shared.publish(
                generation,
                State::Failed {
                    title,
                    reason: format!("cannot start load thread: {e}"),
                },
            );
        }
    }

    /// 停止并释放当前会话(snapshot 变回 None)。
    pub fn close(&self) {
        {
            let mut inner = self.shared.lock();
            inner.generation += 1; // 在途加载结果作废
            inner.state = State::Idle;
        }
        // 设备保留到 ctx 释放(下次 open 免重开);close 计入一次状态变化(UI 收起媒体栏)
        self.shared.bump_dirty();
    }

    /// 当前会话快照;None = 无活动会话。
    ///
    /// 副作用:检测播放自然结束(队列空 + 位置进入末尾窗口)置 `finished` 并 bump dirty
    /// ——UI 挂在 200ms poll 上,天然是检测点(不额外引入线程)。
    pub fn snapshot(&self) -> Option<AudioSnapshot> {
        let mut finished_now = false;
        let snap = {
            let mut inner = self.shared.lock();
            let volume = inner.volume;
            let muted = inner.muted;
            let effective = if muted { 0.0 } else { volume };
            match &mut inner.state {
                State::Idle => None,
                State::Loading { title } => Some(AudioSnapshot {
                    status: AudioStatus::Loading,
                    title: title.clone(),
                    paused: true,
                    position: Duration::ZERO,
                    duration: None,
                    volume: effective,
                    muted,
                    finished: false,
                }),
                State::Failed { title, reason } => Some(AudioSnapshot {
                    status: AudioStatus::Failed(reason.clone()),
                    title: title.clone(),
                    paused: true,
                    position: Duration::ZERO,
                    duration: None,
                    volume: effective,
                    muted,
                    finished: false,
                }),
                State::Ready(session) => {
                    let position = session.player.get_pos();
                    if !session.finished && session.player.empty() {
                        if let Some(total) = session.duration {
                            if position + FINISH_EPSILON >= total {
                                session.finished = true;
                                finished_now = true;
                            }
                        }
                    }
                    Some(AudioSnapshot {
                        status: AudioStatus::Ready,
                        title: session.title.clone(),
                        paused: session.player.is_paused(),
                        position,
                        duration: session.duration,
                        volume: effective,
                        muted,
                        finished: session.finished,
                    })
                }
            }
        };
        if finished_now {
            self.shared.bump_dirty();
        }
        snap
    }

    /// 状态变化计数(加载完成/失败/自然结束等异步事件 +1);事件循环据此触发重排重绘。
    pub fn dirty_version(&self) -> u64 {
        self.shared.dirty.load(Ordering::SeqCst)
    }

    /// 播放/暂停切换。
    pub fn toggle_pause(&self) {
        let inner = self.shared.lock();
        if let State::Ready(session) = &inner.state {
            if session.player.is_paused() {
                session.player.play();
            } else {
                session.player.pause();
            }
        }
    }

    /// 相对 seek(秒,可负);越界由实现 clamp 到 [0, duration]。
    ///
    /// 已自然结束的会话其源队列已空,seek 不再生效(`restart` 会重建会话,见其文档)。
    pub fn seek_by(&self, delta_secs: f64) {
        let inner = self.shared.lock();
        let State::Ready(session) = &inner.state else {
            return;
        };
        let current = session.player.get_pos().as_secs_f64();
        let target = clamp_position(current + delta_secs, session.duration);
        // 阻塞 0–5ms(rodio 文档);解码器不支持 seek 时静默保持原位
        let _ = session.player.try_seek(target);
    }

    /// 绝对 seek 到比例位置(0.0..=1.0;click-to-seek / scrubbing 用);时长未知时忽略。
    pub fn seek_to_fraction(&self, frac: f32) {
        if !frac.is_finite() {
            return;
        }
        let inner = self.shared.lock();
        let State::Ready(session) = &inner.state else {
            return;
        };
        let Some(total) = session.duration else {
            return;
        };
        let target = total.mul_f32(frac.clamp(0.0, 1.0));
        let _ = session.player.try_seek(target);
    }

    /// 音量相对调整(如 ±0.05),自动 clamp 到 [0,1];调整会解除静音。
    pub fn adjust_volume(&self, delta: f32) {
        let mut inner = self.shared.lock();
        inner.volume = (inner.volume + delta).clamp(0.0, 1.0);
        inner.muted = false;
        let volume = inner.volume;
        if let State::Ready(session) = &inner.state {
            session.player.set_volume(volume);
        }
    }

    /// 静音切换(实现:记录静音前的音量,置 0/恢复)。
    pub fn toggle_mute(&self) {
        let mut inner = self.shared.lock();
        inner.muted = !inner.muted;
        let effective = if inner.muted { 0.0 } else { inner.volume };
        if let State::Ready(session) = &inner.state {
            session.player.set_volume(effective);
        }
    }

    /// 回到本曲开头并继续播放。
    ///
    /// 未播完:seek 0 + play。已播完(源队列空;`finished` 可能尚未被 snapshot() 检出)
    /// 时 seek 已无效 → 重建会话(重新解码同一来源;远程 URL 会重新下载)。
    pub fn restart(&self) {
        let replay = {
            let inner = self.shared.lock();
            match &inner.state {
                State::Ready(session) if session.finished || session.player.empty() => {
                    Some((session.src.clone(), session.base_dir.clone()))
                }
                _ => None,
            }
        };
        if let Some((src, base_dir)) = replay {
            self.open(&src, &base_dir);
            return;
        }
        let inner = self.shared.lock();
        if let State::Ready(session) = &inner.state {
            let _ = session.player.try_seek(Duration::ZERO);
            session.player.play();
        }
    }
}

/// 位置 clamp:[0, duration](时长未知时只保证非负)。
fn clamp_position(secs: f64, duration: Option<Duration>) -> Duration {
    let upper = duration.map(|d| d.as_secs_f64()).unwrap_or(f64::INFINITY);
    Duration::from_secs_f64(secs.clamp(0.0, upper))
}

// ---------------------------------------------------------------------------
// 后台加载:来源解析 → 解码器 → (设备)播放器 → 发布
// ---------------------------------------------------------------------------

/// 后台线程主体:加载失败与设备失败均发布 `Failed`,不 panic。
fn load_in_background(shared: Shared, generation: u64, src: String, base_dir: PathBuf) {
    let title = display_title(&src);
    match prepare_source(&src, &base_dir) {
        Ok(prepared) => match start_playback(&shared, prepared) {
            Ok(session) => shared.publish(generation, State::Ready(Box::new(session))),
            Err(reason) => shared.publish(generation, State::Failed { title, reason }),
        },
        Err(reason) => shared.publish(generation, State::Failed { title, reason }),
    }
}

/// 已加载待播放的来源。
struct Prepared {
    decoder: Decoder<File>,
    duration: Option<Duration>,
    title: String,
    src: String,
    base_dir: PathBuf,
    temp: Option<TempFile>,
}

/// 建播放器并 append(顺序:设备 → 音量 → append,避免起播瞬间满音量)。
fn start_playback(shared: &Shared, prepared: Prepared) -> Result<Session, String> {
    shared.ensure_device()?;
    let player = shared.new_player()?;
    {
        // 逻辑音量跨会话保持;起播前先设好,静音态起播也是静音
        let inner = shared.lock();
        player.set_volume(if inner.muted { 0.0 } else { inner.volume });
    }
    player.append(prepared.decoder);
    Ok(Session {
        player,
        title: prepared.title,
        duration: prepared.duration,
        finished: false,
        src: prepared.src,
        base_dir: prepared.base_dir,
        temp: prepared.temp,
    })
}

/// 解析来源并构建解码器(失败早于设备打开:坏文件不会去碰音频设备)。
fn prepare_source(src: &str, base_dir: &Path) -> Result<Prepared, String> {
    let src = src.trim();
    if src.is_empty() {
        return Err("empty audio source".into());
    }
    let title = display_title(src);
    if src.starts_with("http://") || src.starts_with("https://") {
        let bytes = fetch_remote(src)?;
        let temp = write_temp(&bytes, extension_of(src).as_deref())?;
        let (decoder, duration) = open_decoder(temp.0.as_path(), &title)?;
        return Ok(Prepared {
            decoder,
            duration,
            title,
            src: src.to_string(),
            base_dir: base_dir.to_path_buf(),
            temp: Some(temp),
        });
    }
    // file: URL(file:///abs、file://abs、file:/abs 统一还原为绝对路径;忽略 ?query)
    let local = match src.strip_prefix("file:") {
        Some(rest) => {
            let no_query = rest.split('?').next().unwrap_or("");
            format!("/{}", no_query.trim_start_matches('/'))
        }
        None => src.to_string(),
    };
    let path = links::normalize(base_dir, &local);
    let meta = std::fs::metadata(&path).map_err(|_| format!("not found: {src}"))?;
    if meta.is_dir() {
        return Err(format!("is a directory: {src}"));
    }
    // 本地文件不设大小上限:解码器惰性流式读(rodio 后台线程按需拉取),
    // 内存占用与文件大小无关(研究 §3.4)。
    let (decoder, duration) = open_decoder(&path, &title)?;
    Ok(Prepared {
        decoder,
        duration,
        title,
        src: src.to_string(),
        base_dir: base_dir.to_path_buf(),
        temp: None,
    })
}

/// 打开文件并构建解码器,取总时长(必须在 append 前取:trait 方法)。
fn open_decoder(path: &Path, title: &str) -> Result<(Decoder<File>, Option<Duration>), String> {
    let file = File::open(path).map_err(|_| format!("unreadable: {title}"))?;
    let byte_len = file.metadata().ok().map(|m| m.len()).filter(|n| *n > 0);
    let builder = Decoder::<File>::builder().with_data(file);
    // 必须声明 byte_len/seekable:rodio 0.22 的默认 Settings 不可 seek,
    // 而 seek 与 mp3/vorbis 类格式的时长计算都依赖它(见 DecoderBuilder 文档)。
    let builder = match byte_len {
        Some(len) => builder.with_byte_len(len),
        None => builder.with_seekable(true),
    };
    let decoder = builder
        .build()
        .map_err(|e| decode_error_message(path, &e))?;
    // 未知时长(流式源/个别容器)→ None,UI 显示 `?`
    let duration = decoder.total_duration().filter(|d| !d.is_zero());
    Ok((decoder, duration))
}

/// 解码器错误 → 可读原因(UI 直接展示,不 panic)。
fn decode_error_message(path: &Path, err: &rodio::decoder::DecoderError) -> String {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    // symphonia 无 Opus 解码器(研究 §3.1):.opus 明确报不支持,而不是笼统的格式错
    if ext == "opus" {
        return "unsupported codec: opus (no decoder in this build)".into();
    }
    match err {
        rodio::decoder::DecoderError::UnrecognizedFormat
        | rodio::decoder::DecoderError::NoStreams => {
            if ext.is_empty() {
                "unsupported or corrupt audio: unrecognized format".into()
            } else {
                format!("unsupported or corrupt audio: .{ext}")
            }
        }
        other => format!("decode failed: {other}"),
    }
}

/// http(s) 获取(rustls;跟随重定向;64MB 上限)。
fn fetch_remote(url: &str) -> Result<Vec<u8>, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(FETCH_TIMEOUT)
        .redirects(5)
        .build();
    let resp = agent
        .get(url)
        .call()
        .map_err(|e| format!("fetch failed: {e}"))?;
    let status = resp.status();
    if !(200..300).contains(&status) {
        return Err(format!("http {status}"));
    }
    let mut buf = Vec::new();
    resp.into_reader()
        .take(MAX_REMOTE_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|e| format!("read failed: {e}"))?;
    if buf.is_empty() {
        return Err("empty response".into());
    }
    if buf.len() as u64 > MAX_REMOTE_BYTES {
        return Err(format!("too large (>{}MB)", MAX_REMOTE_BYTES / 1024 / 1024));
    }
    Ok(buf)
}

/// 远程字节落临时文件(解码器要 Read + Seek;落盘后流式解码,内存恒定)。
fn write_temp(bytes: &[u8], ext: Option<&str>) -> Result<TempFile, String> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let suffix = ext.map(|e| format!(".{e}")).unwrap_or_default();
    let path =
        std::env::temp_dir().join(format!("dlook-audio-{}-{seq}{suffix}", std::process::id()));
    std::fs::write(&path, bytes).map_err(|e| format!("temp file failed: {e}"))?;
    Ok(TempFile(path))
}

/// URL/路径的文件名部分作为显示标题(去 query/fragment,percent-decode,截断防超长)。
fn display_title(src: &str) -> String {
    let src = src.trim();
    if src.is_empty() {
        return "(no source)".into();
    }
    let without_query = src.split(['?', '#']).next().unwrap_or(src);
    let trimmed = without_query.trim_end_matches('/');
    let tail = trimmed.rsplit('/').next().unwrap_or(trimmed);
    let tail = if tail.is_empty() { trimmed } else { tail };
    let decoded = links::percent_decode(tail);
    // 超长 URL 尾段(签名串等)截断,避免撑爆媒体栏
    let mut out: String = decoded.chars().take(120).collect();
    if decoded.chars().count() > 120 {
        out.push('…');
    }
    out
}

/// 扩展名(小写;仅用于临时文件命名与可读报错)。
fn extension_of(src: &str) -> Option<String> {
    let path = src.split(['?', '#']).next().unwrap_or(src);
    let ext = Path::new(path).extension()?.to_str()?;
    if ext.is_empty() || ext.len() > 5 || !ext.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(ext.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    // ---- 测试素材:全部代码生成,不依赖外部文件 ----

    /// 单声道 8kHz 16-bit PCM WAV(静音;设备只收到零采样,测试期间不发声)。
    fn wav_silence(secs: f32) -> Vec<u8> {
        assert!(secs > 0.0);
        let rate: u32 = 8000;
        let frames = (secs * rate as f32) as u32;
        let data_len = frames * 2; // 16-bit 单声道
        let mut buf = Vec::with_capacity(44 + data_len as usize);
        buf.extend_from_slice(b"RIFF");
        buf.extend_from_slice(&(36 + data_len).to_le_bytes());
        buf.extend_from_slice(b"WAVE");
        buf.extend_from_slice(b"fmt ");
        buf.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk 大小
        buf.extend_from_slice(&1u16.to_le_bytes()); // PCM
        buf.extend_from_slice(&1u16.to_le_bytes()); // 单声道
        buf.extend_from_slice(&rate.to_le_bytes());
        buf.extend_from_slice(&(rate * 2).to_le_bytes()); // byte rate
        buf.extend_from_slice(&2u16.to_le_bytes()); // block align
        buf.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
        buf.extend_from_slice(b"data");
        buf.extend_from_slice(&data_len.to_le_bytes());
        buf.resize(44 + data_len as usize, 0);
        buf
    }

    /// 临时素材守卫(文件名带 pid,drop 时清理)。
    struct Temp(PathBuf);

    impl Temp {
        fn new(tag: &str, bytes: &[u8]) -> Self {
            let path =
                std::env::temp_dir().join(format!("dlook-media-test-{}-{tag}", std::process::id()));
            std::fs::write(&path, bytes).expect("write temp fixture");
            Temp(path)
        }

        fn str(&self) -> &str {
            self.0.to_str().expect("utf-8 temp path")
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// 设备相关测试串行化:并发多个 ALSA 流会让时序断言不稳。
    fn device_lock() -> MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 轮询 snapshot 直到离开 Loading(最多 5s)。
    fn wait_settled(ctx: &AudioCtx) -> AudioSnapshot {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(snap) = ctx.snapshot() {
                if snap.status != AudioStatus::Loading {
                    return snap;
                }
            }
            assert!(Instant::now() < deadline, "音频加载 5s 未完成");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// 无声卡环境(CI/容器)跳过依赖真实播放的断言;本机有声卡时完整执行。
    fn skip_without_device(snap: &AudioSnapshot) -> bool {
        matches!(&snap.status, AudioStatus::Failed(r) if r.starts_with("no audio device"))
    }

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-6
    }

    // ---- 素材自检 ----

    #[test]
    fn generated_wav_header_is_valid() {
        let wav = wav_silence(1.0);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(u32::from_le_bytes(wav[16..20].try_into().unwrap()), 16);
        assert_eq!(u16::from_le_bytes(wav[22..24].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 8000);
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 16000);
        assert_eq!(wav.len(), 44 + 16000);
    }

    // ---- 惰性 / 空会话 ----

    #[test]
    fn new_is_lazy_and_controls_are_noop_without_session() {
        // new() 不接触音频后端:构造与无会话快照都是纯内存操作(远低于设备打开耗时)
        let t0 = Instant::now();
        let ctx = AudioCtx::new();
        assert!(t0.elapsed() < Duration::from_millis(20));
        assert!(ctx.snapshot().is_none());
        assert_eq!(ctx.dirty_version(), 0);
        assert!(ctx.snapshot().is_none(), "重复快照仍无会话");
        assert!(t0.elapsed() < Duration::from_millis(50));

        // 无会话时控制方法全部空操作(不 panic、不阻塞、不产生会话)
        let t1 = Instant::now();
        ctx.toggle_pause();
        ctx.seek_by(5.0);
        ctx.seek_to_fraction(0.5);
        ctx.adjust_volume(0.05);
        ctx.toggle_mute();
        ctx.restart();
        assert!(t1.elapsed() < Duration::from_millis(50));
        assert!(ctx.snapshot().is_none());

        ctx.close();
        assert!(ctx.snapshot().is_none());
        assert!(ctx.dirty_version() >= 1, "close 应 bump dirty");
    }

    // ---- 打开:状态与时长 ----

    #[test]
    fn open_local_wav_ready_with_duration() {
        let _serial = device_lock();
        let wav = Temp::new("2s.wav", &wav_silence(2.0));
        let ctx = AudioCtx::new();
        let d0 = ctx.dirty_version();
        ctx.open(wav.str(), Path::new("/"));
        let snap = wait_settled(&ctx);
        if skip_without_device(&snap) {
            eprintln!("skip: 本机无音频设备");
            return;
        }
        assert_eq!(snap.status, AudioStatus::Ready);
        let dur = snap.duration.expect("WAV 时长应已知");
        assert!(
            (dur.as_secs_f32() - 2.0).abs() <= 0.05,
            "duration = {dur:?}"
        );
        assert_eq!(snap.title, wav.0.file_name().unwrap().to_string_lossy());
        assert!(!snap.paused, "打开即播放");
        assert!(!snap.muted && approx(snap.volume, DEFAULT_VOLUME));
        assert!(!snap.finished);
        assert!(
            snap.position < Duration::from_millis(500),
            "{:?}",
            snap.position
        );
        assert!(ctx.dirty_version() > d0, "Ready 应 bump dirty");
    }

    #[test]
    fn relative_path_and_file_url_resolve() {
        let _serial = device_lock();
        let dir = std::env::temp_dir().join(format!("dlook-media-test-{}-rel", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("clip.wav"), wav_silence(1.0)).expect("write");
        let ctx = AudioCtx::new();

        // 相对路径按 base_dir 解析(与图片/链接一致)
        ctx.open("clip.wav", &dir);
        let snap = wait_settled(&ctx);
        assert_eq!(snap.title, "clip.wav");
        if !skip_without_device(&snap) {
            assert_eq!(snap.status, AudioStatus::Ready);
        }

        // file: URL(带 query)等价
        let url = format!("file://{}?x=1", dir.join("clip.wav").display());
        ctx.open(&url, Path::new("/"));
        let snap = wait_settled(&ctx);
        assert_eq!(snap.title, "clip.wav", "file: URL 取尾段并去 query");
        if !skip_without_device(&snap) {
            assert_eq!(snap.status, AudioStatus::Ready);
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- 播放推进 / 暂停冻结 ----

    #[test]
    fn position_advances_then_pause_freezes() {
        let _serial = device_lock();
        let wav = Temp::new("3s.wav", &wav_silence(3.0));
        let ctx = AudioCtx::new();
        ctx.open(wav.str(), Path::new("/"));
        let snap = wait_settled(&ctx);
        if skip_without_device(&snap) {
            eprintln!("skip: 本机无音频设备");
            return;
        }
        assert!(!snap.paused);
        std::thread::sleep(Duration::from_millis(400));
        let p1 = ctx.snapshot().unwrap().position;
        assert!(p1 >= Duration::from_millis(150), "位置未推进: {p1:?}");

        // 暂停 → 位置冻结(rodio Pausable 暂停时不驱动 TrackPosition)
        ctx.toggle_pause();
        std::thread::sleep(Duration::from_millis(60));
        let a = ctx.snapshot().unwrap();
        assert!(a.paused);
        std::thread::sleep(Duration::from_millis(150));
        let b = ctx.snapshot().unwrap();
        let drift = b.position.abs_diff(a.position);
        assert!(
            drift < Duration::from_millis(20),
            "暂停后位置漂移 {drift:?}"
        );

        // 恢复 → 继续推进
        ctx.toggle_pause();
        std::thread::sleep(Duration::from_millis(300));
        let c = ctx.snapshot().unwrap();
        assert!(!c.paused);
        assert!(
            c.position >= b.position + Duration::from_millis(150),
            "恢复后未推进: {:?} → {:?}",
            b.position,
            c.position
        );
    }

    // ---- seek ----

    #[test]
    fn seek_by_fraction_clamp_and_restart() {
        let _serial = device_lock();
        let wav = Temp::new("4s.wav", &wav_silence(4.0));
        let ctx = AudioCtx::new();
        ctx.open(wav.str(), Path::new("/"));
        let snap = wait_settled(&ctx);
        if skip_without_device(&snap) {
            eprintln!("skip: 本机无音频设备");
            return;
        }
        let total = snap.duration.expect("时长已知");

        // 相对 seek:+1.0s 至少 +0.8s
        let before = ctx.snapshot().unwrap().position;
        ctx.seek_by(1.0);
        std::thread::sleep(Duration::from_millis(80));
        let after = ctx.snapshot().unwrap().position;
        assert!(
            after >= before + Duration::from_millis(800),
            "seek_by(+1.0): {before:?} → {after:?}"
        );

        // 绝对比例 seek:0.5 → duration/2
        ctx.seek_to_fraction(0.5);
        std::thread::sleep(Duration::from_millis(80));
        let mid = ctx.snapshot().unwrap().position;
        let target = total / 2;
        let off = mid.abs_diff(target);
        assert!(
            off < Duration::from_millis(150),
            "seek_to_fraction(0.5): {mid:?} 期望 {target:?}"
        );

        // 越界 clamp:下界(负 delta → 0)
        ctx.seek_by(-999.0);
        std::thread::sleep(Duration::from_millis(80));
        let lo = ctx.snapshot().unwrap().position;
        assert!(lo < Duration::from_millis(200), "clamp 下界失败: {lo:?}");

        // 越界 clamp:上界(不得越过总时长;可能直接播完)
        ctx.seek_by(999.0);
        std::thread::sleep(Duration::from_millis(80));
        let hi = ctx.snapshot().unwrap().position;
        assert!(
            hi <= total + Duration::from_millis(50),
            "clamp 上界失败: {hi:?}"
        );

        // restart:未播完是 seek 0;已播完(队列空)则重建会话
        ctx.restart();
        let head = wait_settled(&ctx);
        assert_eq!(head.status, AudioStatus::Ready);
        std::thread::sleep(Duration::from_millis(50));
        let head = ctx.snapshot().unwrap();
        assert!(
            head.position < Duration::from_millis(200),
            "restart: {:?}",
            head.position
        );
        assert!(!head.paused, "restart 后继续播放");
        assert!(!head.finished);
    }

    #[test]
    fn seek_calls_ignore_invalid_state() {
        // 失败态:seek 全部空操作(不 panic)
        let ctx = AudioCtx::new();
        ctx.open("./missing.mp3", Path::new("/"));
        let _ = wait_settled(&ctx);
        ctx.seek_to_fraction(0.5);
        ctx.seek_to_fraction(f32::NAN);
        ctx.seek_by(-3.0);
        assert!(matches!(
            ctx.snapshot().unwrap().status,
            AudioStatus::Failed(_)
        ));
    }

    // ---- 自然结束 ----

    #[test]
    fn natural_finish_sets_flag_and_restart_replays() {
        let _serial = device_lock();
        let wav = Temp::new("short.wav", &wav_silence(0.4));
        let ctx = AudioCtx::new();
        ctx.open(wav.str(), Path::new("/"));
        let snap = wait_settled(&ctx);
        if skip_without_device(&snap) {
            eprintln!("skip: 本机无音频设备");
            return;
        }
        let d0 = ctx.dirty_version();
        let deadline = Instant::now() + Duration::from_secs(4);
        let mut finished = false;
        while Instant::now() < deadline {
            if ctx.snapshot().unwrap().finished {
                finished = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(finished, "0.4s 音频未在 4s 内置 finished");
        assert!(ctx.dirty_version() > d0, "finished 应 bump dirty");

        // 播完后 restart:队列已空 → 重建会话从头播放
        ctx.restart();
        let snap = wait_settled(&ctx);
        assert_eq!(snap.status, AudioStatus::Ready, "restart 重建会话");
        assert!(!snap.finished);
        assert!(
            snap.position < Duration::from_millis(300),
            "{:?}",
            snap.position
        );
    }

    // ---- 音量 / 静音 ----

    #[test]
    fn volume_clamp_and_mute_roundtrip() {
        // 失败态的快照同样带 ctx 级音量(无需设备即可验证 clamp/静音语义)
        let ctx = AudioCtx::new();
        ctx.open("./definitely-missing.mp3", Path::new("/"));
        let snap = wait_settled(&ctx);
        assert!(matches!(snap.status, AudioStatus::Failed(_)));
        assert!(approx(snap.volume, DEFAULT_VOLUME), "默认音量应 80%");
        assert!(!snap.muted);

        ctx.adjust_volume(0.05);
        let v = ctx.snapshot().unwrap().volume;
        assert!(v > DEFAULT_VOLUME && approx(v, 0.85), "音量应 +0.05: {v}");

        for _ in 0..20 {
            ctx.adjust_volume(0.05);
        }
        let v = ctx.snapshot().unwrap();
        assert!(
            approx(v.volume, 1.0) && !v.muted,
            "clamp 上界: {}",
            v.volume
        );

        for _ in 0..40 {
            ctx.adjust_volume(-0.05);
        }
        let v = ctx.snapshot().unwrap().volume;
        assert!(approx(v, 0.0), "clamp 下界: {v}");

        // 回到 ≥0.85,验证静音往返恢复原值
        while ctx.snapshot().unwrap().volume < 0.85 {
            ctx.adjust_volume(0.05);
        }
        let before = ctx.snapshot().unwrap();
        ctx.toggle_mute();
        let muted = ctx.snapshot().unwrap();
        assert!(muted.muted && approx(muted.volume, 0.0), "静音后有效音量 0");
        ctx.toggle_mute();
        let back = ctx.snapshot().unwrap();
        assert!(
            !back.muted && approx(back.volume, before.volume),
            "解除静音应恢复原值 {} → {}",
            before.volume,
            back.volume
        );

        // 静音状态下调音量 → 解除静音
        ctx.toggle_mute();
        assert_eq!(ctx.snapshot().unwrap().volume, 0.0);
        ctx.adjust_volume(-0.05);
        let after = ctx.snapshot().unwrap();
        assert!(!after.muted && after.volume > 0.0, "调音量应解除静音");
    }

    // ---- 失败路径:不 panic,原因是可读文案 ----

    #[test]
    fn missing_file_reports_not_found() {
        let ctx = AudioCtx::new();
        ctx.open("./no/such/track.mp3", Path::new("/"));
        match wait_settled(&ctx).status {
            AudioStatus::Failed(reason) => {
                assert!(reason.contains("not found"), "reason = {reason}");
                assert!(reason.contains("track.mp3"), "reason 应含来源: {reason}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn garbage_bytes_fail_without_panic() {
        let junk = Temp::new("junk.mp3", &[0x5a; 64]);
        let ctx = AudioCtx::new();
        ctx.open(junk.str(), Path::new("/"));
        match wait_settled(&ctx).status {
            AudioStatus::Failed(reason) => {
                assert!(
                    reason.contains("mp3") || reason.contains("corrupt"),
                    "{reason}"
                )
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn directory_and_empty_source_fail_readably() {
        let ctx = AudioCtx::new();
        ctx.open("/tmp", Path::new("/"));
        match wait_settled(&ctx).status {
            AudioStatus::Failed(reason) => assert!(reason.contains("directory"), "{reason}"),
            other => panic!("expected Failed, got {other:?}"),
        }

        let ctx = AudioCtx::new();
        ctx.open("   ", Path::new("/"));
        match wait_settled(&ctx).status {
            AudioStatus::Failed(reason) => assert!(reason.contains("empty"), "{reason}"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn opus_reports_unsupported_codec() {
        // 240B 真 OggOpus(ffmpeg 生成后内联;本构建无 opus 解码器)
        const OPUS_OGG: &[u8] = &[
            0x4f, 0x67, 0x67, 0x53, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x8d, 0x09, 0x17, 0xb4, 0x00, 0x00, 0x00, 0x00, 0xda, 0x57, 0x0d, 0x23, 0x01, 0x13,
            0x4f, 0x70, 0x75, 0x73, 0x48, 0x65, 0x61, 0x64, 0x01, 0x01, 0x38, 0x01, 0x40, 0x1f,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x4f, 0x67, 0x67, 0x53, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x8d, 0x09, 0x17, 0xb4, 0x01, 0x00, 0x00, 0x00, 0xfd,
            0xbd, 0x95, 0xe4, 0x01, 0x3c, 0x4f, 0x70, 0x75, 0x73, 0x54, 0x61, 0x67, 0x73, 0x0c,
            0x00, 0x00, 0x00, 0x4c, 0x61, 0x76, 0x66, 0x36, 0x33, 0x2e, 0x31, 0x2e, 0x31, 0x30,
            0x31, 0x01, 0x00, 0x00, 0x00, 0x1c, 0x00, 0x00, 0x00, 0x65, 0x6e, 0x63, 0x6f, 0x64,
            0x65, 0x72, 0x3d, 0x4c, 0x61, 0x76, 0x63, 0x36, 0x33, 0x2e, 0x31, 0x2e, 0x31, 0x30,
            0x31, 0x20, 0x6c, 0x69, 0x62, 0x6f, 0x70, 0x75, 0x73, 0x4f, 0x67, 0x67, 0x53, 0x00,
            0x04, 0xb8, 0x26, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x8d, 0x09, 0x17, 0xb4, 0x02,
            0x00, 0x00, 0x00, 0x68, 0xc9, 0x05, 0x8f, 0x0b, 0x07, 0x06, 0x06, 0x06, 0x06, 0x06,
            0x06, 0x06, 0x06, 0x06, 0x06, 0x08, 0x0b, 0xe6, 0x3b, 0x23, 0xab, 0x60, 0x08, 0x08,
            0xac, 0xb3, 0x0e, 0xc6, 0x08, 0x08, 0xac, 0xb3, 0x0e, 0xc6, 0x08, 0x08, 0xac, 0xb3,
            0x0e, 0xc6, 0x08, 0x08, 0xac, 0xb3, 0x0e, 0xc6, 0x08, 0x08, 0xac, 0xb3, 0x0e, 0xc6,
            0x08, 0x08, 0xac, 0xb3, 0x0e, 0xc6, 0x08, 0x08, 0xac, 0xb3, 0x0e, 0xc6, 0x08, 0x08,
            0xac, 0xb3, 0x0e, 0xc6, 0x08, 0x08, 0xac, 0xb3, 0x0e, 0xc6, 0x08, 0x08, 0xac, 0xb3,
            0x0e, 0xc6,
        ];
        let opus = Temp::new("tiny.opus", OPUS_OGG);
        let ctx = AudioCtx::new();
        ctx.open(opus.str(), Path::new("/"));
        match wait_settled(&ctx).status {
            AudioStatus::Failed(reason) => assert!(reason.contains("opus"), "reason = {reason}"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn failed_load_keeps_volume_settings() {
        let ctx = AudioCtx::new();
        ctx.adjust_volume(-0.3);
        ctx.toggle_mute();
        ctx.open("./missing.flac", Path::new("/"));
        let snap = wait_settled(&ctx);
        assert!(snap.muted && approx(snap.volume, 0.0));
        assert_eq!(snap.title, "missing.flac");
    }

    // ---- dirty 计数 / close / 会话替换 ----

    #[test]
    fn dirty_bumps_on_failure_and_close() {
        let ctx = AudioCtx::new();
        assert_eq!(ctx.dirty_version(), 0);
        ctx.open("./missing.mp3", Path::new("/"));
        let _ = wait_settled(&ctx);
        let d1 = ctx.dirty_version();
        assert!(d1 >= 1, "失败应 bump dirty");
        assert!(ctx.snapshot().is_some(), "失败态保留快照供媒体栏展示 ✗");

        ctx.close();
        assert!(ctx.snapshot().is_none(), "close 后无活动会话");
        assert!(ctx.dirty_version() > d1, "close 应 bump dirty");

        let d2 = ctx.dirty_version();
        ctx.close();
        assert!(ctx.dirty_version() > d2, "重复 close 仍计入状态变化");
    }

    #[test]
    fn reopen_replaces_session_and_discards_stale_result() {
        let ctx = AudioCtx::new();
        ctx.open("./missing-a.mp3", Path::new("/"));
        let _ = wait_settled(&ctx);
        // 老会话就绪后立刻 open 新源:老结果不得覆盖新状态
        ctx.open("./missing-b.mp3", Path::new("/"));
        let snap = wait_settled(&ctx);
        assert_eq!(snap.title, "missing-b.mp3");
        match snap.status {
            AudioStatus::Failed(reason) => assert!(reason.contains("missing-b.mp3"), "{reason}"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    // ---- 纯函数 ----

    #[test]
    fn display_title_and_extension() {
        assert_eq!(display_title("/a/b/song.mp3"), "song.mp3");
        assert_eq!(display_title("https://x/y/z%20w.ogg?sig=1#frag"), "z w.ogg");
        assert_eq!(display_title("https://x/dir/"), "dir");
        assert_eq!(display_title(""), "(no source)");
        let long = format!("/x/{}", "a".repeat(200));
        let out = display_title(&long);
        assert_eq!(out.chars().count(), 121, "超长尾段截断");
        assert!(out.ends_with('…'));
        assert_eq!(extension_of("https://x/a.FLAC?v=1"), Some("flac".into()));
        assert_eq!(extension_of("https://x/noext"), None);
        assert_eq!(extension_of("https://x/weird?a=1"), None);
    }

    #[test]
    fn decode_errors_are_readable() {
        use rodio::decoder::DecoderError;
        let opus = decode_error_message(Path::new("x.opus"), &DecoderError::UnrecognizedFormat);
        assert!(opus.contains("unsupported codec: opus"), "{opus}");
        let wav = decode_error_message(Path::new("x.wav"), &DecoderError::UnrecognizedFormat);
        assert!(
            wav.contains("unsupported or corrupt") && wav.contains(".wav"),
            "{wav}"
        );
        let io = decode_error_message(Path::new("x.ogg"), &DecoderError::IoError("boom".into()));
        assert!(io.starts_with("decode failed:"), "{io}");
    }

    #[test]
    fn clamp_position_bounds() {
        let total = Some(Duration::from_secs(5));
        assert_eq!(clamp_position(-3.0, total), Duration::ZERO);
        assert_eq!(clamp_position(2.5, total), Duration::from_secs_f64(2.5));
        assert_eq!(clamp_position(99.0, total), Duration::from_secs(5));
        assert_eq!(clamp_position(99.0, None), Duration::from_secs_f64(99.0));
    }
}
