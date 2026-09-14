//! 视频播放(DECISIONS D16,委托 mpv):dlook 掌 UI,mpv 做解码与像素输出,控制走 JSON IPC。
//!
//! 设计要点(研究依据见 docs/research/media/evidence-video.md):
//!   - mpv 官方 VO:`--vo=kitty`(v0.36.0 起无条件编译)/`--vo=sixel`(需发行版带 libsixel)。
//!     无图形协议(halfblocks/未知终端)→ 不启动 mpv,由集成层降级为 ffmpeg 首帧静图。
//!   - 区域几何:`--vo-kitty-left/top/rows/cols` 把视频限制在 body 区域(共屏形态 MVP-A);
//!     sixel 的区域能力由原型实验 E14 实测确认——**与 kitty 同族同名参数**
//!     (`--vo-sixel-left/top/cols/rows/width/height`),故 two proto 走同一条拼参路径,
//!     sixel 不需要退回全屏形态(见 docs/research/media/experiments/README.md §E14)。
//!   - 控制面:JSON 行协议 over unix socket(`--input-ipc-server`,随机临时路径);
//!     状态轮询(每 200ms tick)拿 time-pos/duration/pause/volume/aid。
//!   - 生命周期:start → tick* → stop;stop/退出后必须触发**全量重绘**
//!     (mpv 启动与退出都会发 `\033_Ga=d` 清空终端全部 kitty 图像,研究 §已知坑)。
//!   - 无 mpv:available() = false,集成层走降级链(ffmpeg 首帧 → 信息行)。
//!
//! 实现要点(2026-09-14, task media-3;实测记录见 experiments/README.md):
//!   - **IPC 手写**:`std::os::unix::net::UnixStream` + 自写最小 JSON 解析(无新依赖;
//!     不引入 mpvipc——协议只有「一行一个 JSON」这一条规则,自写更可控且避免 GPL 传染)。
//!     mpv 会向所有客户端推送事件(start-file/file-loaded/…),响应靠 `request_id` 配对,
//!     其余行直接丢弃。
//!   - **socket 就绪等待**:spawn 后 mpv 需要几十毫秒才创建 socket;`start()` 轮询
//!     「socket 文件出现且 connect 成功」≤5s,超时 Failed。消融实验(同文件 tests 内
//!     `ablation_*`,`--ignored`)证明去掉该等待后立即 connect 的失败率显著>0。
//!   - **状态轮询而非 observe_property**:轮询是 pull 语义(200ms tick 拿到的就是当下值,
//!     无读线程/无事件队列),与事件循环模型天然同构;消融实验给出了事件订阅版本的
//!     额外复杂度与滞后(需独占读线程 + 首值仍要 get_property + 事件洪泛时漏读)。
//!   - **set_area 用「重启会话」而非热改属性**:E14 实测 `set_property vo-sixel-left/top/
//!     width/height` 返回 success 且 get_property 能读回,但**输出不变**(raster/落点恒定,
//!     SIGWINCH reconfig 后亦然)→ 区域几何只能靠重新 spawn 生效。实现为:
//!     `set_area()` 记 pending → `tick()` 里等几何稳定(250ms,吸收 resize 抖动)后
//!     带新几何重启,并恢复位置/暂停/音量/静音。
//!   - **静音语义与 AudioCtx 一致**:内部保存逻辑音量与「静音前音量」,静音只把
//!     `snapshot().volume` 置 0(UI 显示 0%),`adjust_volume` 会解除静音。
//!   - **失败原因**:`--really-quiet` 下 mpv 不打 stderr(实测缺失文件 rc=2、stderr 为空),
//!     故本地缺失文件在 `start()` 里先行 fail fast,运行期死掉则用退出码/信号合成可读原因。

use std::ffi::OsString;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

/// 等待 mpv 创建 IPC socket 的上限(task 要求 ≤5s)。
const SOCKET_WAIT: Duration = Duration::from_secs(5);
/// 等「文件已加载、属性可读」的上限(Loading → Playing)。
const READY_WAIT: Duration = Duration::from_secs(2);
/// 单个 IPC 请求的读超时(失败即本 tick 跳过其余轮询,避免拖累 UI)。
const IPC_TIMEOUT: Duration = Duration::from_millis(200);
/// `quit` 之后等待子进程自行退出的上限,超时 kill。
const QUIT_WAIT: Duration = Duration::from_secs(2);
/// `mpv --version` 探测超时。
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// resize 抖动吸收窗口:set_area 后几何稳定这么久才重启会话。
const AREA_SETTLE: Duration = Duration::from_millis(250);
/// 连续多少次 IPC 失败判定会话失联(200ms tick × 5 ≈ 1s)。
const MAX_IPC_FAILURES: u32 = 5;
/// 终端写入门的等待上限:超时则本次不写(下一轮重试),避免拖住 UI 事件循环。
const GATE_TIMEOUT: Duration = Duration::from_millis(150);

/// 终端写入门：保证「任何时刻只有一个写入者，且不在某帧内部」。
///
/// **优先级**：转发器只在整个门空闲且**没有 dlook 写入者在等**时才开始写下一帧。
/// 于是 dlook 的界面写入总能在「当前帧写完后」插进来，不会因终端慢（写 pty 阻塞）
/// 或帧率高而被饿死 —— 这在 SSH（终端消费慢）场景下是必须的，否则媒体栏与按键
/// 反馈会一直不更新。
#[derive(Default)]
struct Gate {
    state: Mutex<GateState>,
    cv: Condvar,
}

#[derive(Default, PartialEq, Eq, Clone, Copy)]
enum Holder {
    #[default]
    None,
    /// 转发器正在写 mpv 的一帧。
    Forward,
    /// dlook 正在写自己的界面。
    Ui,
}

#[derive(Default)]
struct GateState {
    holder: Holder,
    /// 有多少 dlook 写入者在等门（转发器据此让路）。
    ui_waiting: u32,
}

/// 默认逻辑音量(与 AudioCtx 的 DEFAULT_VOLUME 一致;首次 start 前即可调整)。
const DEFAULT_VOLUME: f32 = 0.8;

/// 视频显示区域(终端格坐标,相对整个终端;由集成层按布局算出)。
/// left/top 从 0 计;rows/cols 为区域尺寸;pixel 为像素尺寸(可选)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoArea {
    pub left: u16,
    pub top: u16,
    pub cols: u16,
    pub rows: u16,
    /// body 区的**像素**尺寸(宽, 高);None = 未知(不传 width/height 给 mpv)。
    ///
    /// mpv 的 `--vo-<vo>-width/height` 是像素单位。只给 cols/rows 时,在本机 foot 下
    /// mpv 拿不到终端像素尺寸 → 回退 320×180 小画面(集成验证期发现,media-3 验收 N1)。
    /// 由集成层用 picker 的单元格像素尺寸 × 格数算出。
    pub pixel: Option<(u32, u32)>,
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


/// 载荷/帧边界状态机（转发器用它决定「何时可以放门让 dlook 写界面」）。
///
/// 两种图形协议的结构差异（均为本机实测）：
/// - **sixel**：一帧 = 一个 DCS 载荷（`ESC P` … `ESC \`）。
/// - **kitty**：一帧 = N 个 APC 块（`ESC _ G` … `ESC \`），**只有最后一块 `m=0`**，
///   其余 `m=1`；块与块背靠背（实测 470 块/帧，帧内非末块 m≠1 的数量为 0）。
///
/// 因此「一帧画完」= sixel 的 `ESC \` / kitty 的 `m=0` 块结束。只有此刻放门，
/// dlook 的界面字节才不会被终端当成图形载荷的一部分。
///
/// 另：`ESC` 可能落在两次 read 的边界上，故状态里保留 `pending_esc`，避免漏判。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
enum PayloadState {
    #[default]
    Outside,
    /// sixel DCS 载荷内。
    Dcs,
    /// kitty 帧内：`in_block` = 正在某个 APC 块内（否则在块之间，仍在同一帧）；
    /// `header` 收集块头（到 `;` 为止）用于读 `m=`；`more` 表示还有后续块。
    Kitty {
        in_block: bool,
        /// 刚进入 APC（`ESC _`）后期待引导字节 `G`；它不是参数。
        expect_g: bool,
        header: Vec<u8>,
        more: bool,
    },
}

#[derive(Debug, Default)]
struct PayloadScanner {
    state: PayloadState,
    /// 上一个 chunk 的末尾是 ESC，需要与本 chunk 首个字节合看。
    pending_esc: bool,
}

impl PayloadScanner {
    /// 当前是否在载荷/帧内部（只读，不改变状态）。
    ///
    /// `pending_esc` 也算内部：读到 ESC 但还没看到下一个字节时，无法确定它是不是
    /// 载荷开始（`ESC P` / `ESC _`）。此时若放门，dlook 的字节可能恰好插在载荷开头
    /// 之后 —— 保守起见先不放，等本 chunk 处理完再判。
    fn inside(&self) -> bool {
        self.pending_esc || !matches!(self.state, PayloadState::Outside)
    }

    /// 喂入一个 chunk；返回处理完后是否仍在载荷/帧内部（true = 不能放门）。
    fn feed(&mut self, chunk: &[u8]) -> bool {
        for &b in chunk {
            self.feed_byte(b);
        }
        self.inside()
    }

    fn feed_byte(&mut self, b: u8) {
        if self.pending_esc {
            self.pending_esc = false;
            match (b, self.state.clone()) {
                (b'P', PayloadState::Outside) => {
                    self.state = PayloadState::Dcs;
                    return;
                }
                (b'_', PayloadState::Outside)
                | (
                    b'_',
                    PayloadState::Kitty {
                        in_block: false, ..
                    },
                ) => {
                    // 帧内新块开始（块与块背靠背，实测块间空隙为 0 或几字节的光标序列）
                    self.state = PayloadState::Kitty {
                        in_block: true,
                        expect_g: true,
                        header: Vec::new(),
                        more: true,
                    };
                    return;
                }
                (b'\\', PayloadState::Dcs) => {
                    self.state = PayloadState::Outside;
                    return;
                }
                (b'\\', PayloadState::Kitty { in_block: true, more, .. }) => {
                    self.state = if more {
                        PayloadState::Kitty {
                            in_block: false,
                            expect_g: false,
                            header: Vec::new(),
                            more: true,
                        }
                    } else {
                        PayloadState::Outside
                    };
                    return;
                }
                (_, _) => {} // 普通转义序列的 ESC，继续按状态处理本字节
            }
        }
        match &mut self.state {
            PayloadState::Outside | PayloadState::Dcs => {
                if b == 0x1b {
                    self.pending_esc = true;
                }
            }
            PayloadState::Kitty {
                in_block,
                expect_g,
                header,
                more,
            } => {
                if !*in_block {
                    if b == 0x1b {
                        self.pending_esc = true;
                    }
                    return;
                }
                if *expect_g {
                    // `ESC _ G` 的 G：kitty 协议的引导字节，不属于参数
                    *expect_g = false;
                    if b == b'G' {
                        return;
                    }
                }
                if b == 0x1b {
                    self.pending_esc = true;
                    return;
                }
                // 块头：读到 ';' 为止，解析 m=（缺省视为还有后续，保守不放门）
                if header.last() != Some(&b';') {
                    if b == b';' {
                        let text = String::from_utf8_lossy(header).to_string();
                        *more = text
                            .split(',')
                            .find_map(|kv| kv.strip_prefix("m="))
                            .and_then(|v| v.chars().next())
                            .map(|c| c != '0')
                            .unwrap_or(true);
                        header.clear();
                        header.push(b';');
                    } else if header.len() < 128 {
                        header.push(b);
                    }
                }
            }
        }
    }
}

/// dlook 唯一写入者的前提：把 mpv 的像素输出经管道转发到终端。
///
/// 线程做的事：读 mpv stdout → 帧外取门 → 写终端 → 帧末放门。门在帧内保持持有，
/// 因此 dlook 的界面写入（acquire_tty）只会落在帧边界上：任何时刻只有一个写入者、
/// 且不在某一帧内部。这取代了「暂停 mpv 再写」的不安全做法（实测：暂停可能停在
/// 载荷中间，仍会撕裂）。
fn spawn_forwarder(
    mut from_mpv: impl Read + Send + 'static,
    gate: Arc<Gate>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("dlook-mpv-fwd".into())
        .spawn(move || {
            let mut out = std::io::stdout();
            let mut buf = vec![0u8; 64 * 1024];
            let mut scanner = PayloadScanner::default();
            loop {
                let n = match from_mpv.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(_) => break,
                };
                if !scanner.inside() {
                    let mut st = gate.state.lock().unwrap_or_else(PoisonError::into_inner);
                    while st.holder != Holder::None || st.ui_waiting > 0 {
                        st = gate.cv.wait(st).unwrap_or_else(PoisonError::into_inner);
                    }
                    st.holder = Holder::Forward;
                }
                let _ = out.write_all(&buf[..n]);
                let _ = out.flush();
                if !scanner.feed(&buf[..n]) {
                    let mut st = gate.state.lock().unwrap_or_else(PoisonError::into_inner);
                    if st.holder == Holder::Forward {
                        st.holder = Holder::None;
                    }
                    gate.cv.notify_all();
                }
            }
            if matches!(scanner.state, PayloadState::Dcs) {
                // sixel 截断：补终止符，别让终端停在 DCS 状态
                let _ = out.write_all(b"\x1b\\");
                let _ = out.flush();
            }
            let mut st = gate.state.lock().unwrap_or_else(PoisonError::into_inner);
            if st.holder == Holder::Forward {
                st.holder = Holder::None;
            }
            gate.cv.notify_all();
        })
        .unwrap_or_else(|_| std::thread::spawn(|| {}))
}

/// 终端写入安全窗口（见 `VideoCtx::begin_write`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteWindow {
    /// 现在可以安全写终端；`resume` 为 true 时写完后须调 `end_write(true)` 恢复播放。
    Safe { resume: bool },
    /// mpv 正在输出且无法暂停：放弃本次写入（否则会撕裂画面）。
    Busy,
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

// ---------------------------------------------------------------------------
// 最小 JSON(只需解析 mpv 的响应;不引入 serde/serde_json)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }

    fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
}

/// 编码一个 JSON 值(命令参数只用得到 Str/Num/Bool/Null 与数组)。
fn encode(value: &Json) -> String {
    match value {
        Json::Null => "null".into(),
        Json::Bool(b) => b.to_string(),
        Json::Num(n) => {
            if n.fract() == 0.0 && n.abs() < 1e15 {
                format!("{}", *n as i64)
            } else {
                format!("{n}")
            }
        }
        Json::Str(s) => {
            let mut out = String::with_capacity(s.len() + 2);
            out.push('"');
            for ch in s.chars() {
                match ch {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                    c => out.push(c),
                }
            }
            out.push('"');
            out
        }
        Json::Arr(items) => {
            let parts: Vec<String> = items.iter().map(encode).collect();
            format!("[{}]", parts.join(","))
        }
        Json::Obj(fields) => {
            let parts: Vec<String> = fields
                .iter()
                .map(|(k, v)| format!("{}:{}", encode(&Json::Str(k.clone())), encode(v)))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
    }
}

/// 解析一个 JSON 值;返回 (值, 余下未消费的片段)。
fn parse_value(input: &str) -> Option<(Json, &str)> {
    let s = input.trim_start();
    let (first, rest) = s.split_at(s.chars().next().map(char::len_utf8).unwrap_or(0));
    let _ = &rest;
    let consumed = input.len() - s.len() + first.len();
    match first {
        "" => None,
        "n" => s.strip_prefix("null").map(|r| (Json::Null, r)),
        "t" => s.strip_prefix("true").map(|r| (Json::Bool(true), r)),
        "f" => s.strip_prefix("false").map(|r| (Json::Bool(false), r)),
        "\"" => {
            let mut out = String::new();
            let mut it = s[1..].char_indices();
            while let Some((i, ch)) = it.next() {
                match ch {
                    '"' => return Some((Json::Str(out), &s[1 + i + 1..])),
                    '\\' => {
                        let (_, esc) = it.next()?;
                        match esc {
                            'n' => out.push('\n'),
                            't' => out.push('\t'),
                            'r' => out.push('\r'),
                            'b' => out.push('\u{8}'),
                            'f' => out.push('\u{c}'),
                            'u' => {
                                let hex: String = (0..4).filter_map(|_| it.next().map(|(_, c)| c)).collect();
                                let code = u32::from_str_radix(&hex, 16).ok()?;
                                out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                            }
                            other => out.push(other),
                        }
                    }
                    c => out.push(c),
                }
            }
            None
        }
        "[" => {
            let mut items = Vec::new();
            let mut rest = &s[1..];
            loop {
                rest = rest.trim_start();
                if let Some(r) = rest.strip_prefix(']') {
                    return Some((Json::Arr(items), r));
                }
                let (value, r) = parse_value(rest)?;
                items.push(value);
                rest = r.trim_start();
                if let Some(r) = rest.strip_prefix(',') {
                    rest = r;
                }
            }
        }
        "{" => {
            let mut fields = Vec::new();
            let mut rest = &s[1..];
            loop {
                rest = rest.trim_start();
                if let Some(r) = rest.strip_prefix('}') {
                    return Some((Json::Obj(fields), r));
                }
                let (key, r) = parse_value(rest)?;
                let Json::Str(key) = key else { return None };
                let r = r.trim_start().strip_prefix(':')?;
                let (value, r) = parse_value(r)?;
                fields.push((key, value));
                rest = r.trim_start();
                if let Some(r) = rest.strip_prefix(',') {
                    rest = r;
                }
            }
        }
        _ => {
            let end = s
                .find(|c: char| c == ',' || c == '}' || c == ']' || c.is_whitespace())
                .unwrap_or(s.len());
            let token = &s[..end];
            let _ = consumed;
            token.parse::<f64>().ok().map(|n| (Json::Num(n), &s[end..]))
        }
    }
}

fn parse_json(text: &str) -> Option<Json> {
    parse_value(text).map(|(v, _)| v)
}

// ---------------------------------------------------------------------------
// IPC 客户端(UnixStream + JSON 行协议)
// ---------------------------------------------------------------------------

/// mpv IPC 连接:发送 `{"command":[…],"request_id":n}` 行,按 request_id 取回响应。
struct Ipc {
    stream: UnixStream,
    /// 未消费的原始字节(跨调用保留;超时不丢半行)。
    pending: Vec<u8>,
    next_id: u64,
}

impl Ipc {
    fn connect(path: &Path) -> std::io::Result<Ipc> {
        let stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(IPC_TIMEOUT))?;
        Ok(Ipc {
            stream,
            pending: Vec::new(),
            next_id: 0,
        })
    }

    /// 发送命令并等响应;跳过事件行(无 request_id 或不匹配)。
    /// mpv 以 `{"error":"<非 success>"}` 回复是**正常响应**(如 `property unavailable`),
    /// 用 `request_soft` 区分「mpv 明确拒绝」与「传输层失败」。
    fn request(&mut self, args: &[Json]) -> Result<Json, String> {
        match self.request_soft(args)? {
            Ok(data) => Ok(data),
            Err(mpv_error) => Err(format!("mpv error: {mpv_error}")),
        }
    }

    /// Ok(Ok(data)) = success;Ok(Err(mpv_error)) = mpv 明确报错;Err(_) = 传输层失败。
    fn request_soft(&mut self, args: &[Json]) -> Result<Result<Json, String>, String> {
        let id = self.next_id;
        self.next_id += 1;
        let line = format!(
            "{{\"command\":{},\"request_id\":{id}}}\n",
            encode(&Json::Arr(args.to_vec()))
        );
        self.stream
            .write_all(line.as_bytes())
            .and_then(|_| self.stream.flush())
            .map_err(|e| format!("ipc write failed: {e}"))?;
        let deadline = Instant::now() + IPC_TIMEOUT + Duration::from_millis(100);
        loop {
            if let Some(text) = self.take_line() {
                let Some(value) = parse_json(&text) else { continue };
                let Some(rid) = value.get("request_id").and_then(Json::as_f64) else {
                    continue; // 事件行(无 request_id)
                };
                if rid as u64 != id {
                    continue; // 其他请求的迟到响应
                }
                return match value.get("error").and_then(Json::as_str) {
                    Some("success") => Ok(Ok(value.get("data").cloned().unwrap_or(Json::Null))),
                    Some(other) => Ok(Err(other.to_string())),
                    None => Ok(Err("response without error field".into())),
                };
            }
            if Instant::now() >= deadline {
                return Err("ipc response timeout".into());
            }
            self.read_more()?;
        }
    }

    fn read_more(&mut self) -> Result<(), String> {
        let mut buf = [0u8; 8192];
        match self.stream.read(&mut buf) {
            Ok(0) => Err("ipc closed by mpv".into()),
            Ok(n) => {
                self.pending.extend_from_slice(&buf[..n]);
                Ok(())
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                Ok(()) // 读超时:由调用方的 deadline 决定何时放弃
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => Ok(()),
            Err(e) => Err(format!("ipc read failed: {e}")),
        }
    }

    /// 从缓冲区取出一整行(不含换行);没有整行时返回 None。
    fn take_line(&mut self) -> Option<String> {
        let pos = self.pending.iter().position(|b| *b == b'\n')?;
        let line: Vec<u8> = self.pending.drain(..=pos).collect();
        String::from_utf8(line)
            .ok()
            .map(|s| s.trim_end_matches(['\n', '\r']).to_string())
    }

    /// 严格读:仅测试/消融用(mpv 报错即 Err);生产轮询走 `get_property_opt`。
    #[cfg(test)]
    fn get_property(&mut self, name: &str) -> Result<Json, String> {
        self.request(&[Json::Str("get_property".into()), Json::Str(name.into())])
    }

    /// 软读:mpv 明确拒绝(如 `property unavailable`,文件尚未加载/已 EOF)时返回 Ok(None),
    /// 只有传输层失败才是 Err——避免把「属性暂不可用」误判为 IPC 失联。
    fn get_property_opt(&mut self, name: &str) -> Result<Option<Json>, String> {
        match self.request_soft(&[Json::Str("get_property".into()), Json::Str(name.into())])? {
            Ok(data) => Ok(Some(data)),
            Err(_mpv_error) => Ok(None),
        }
    }

    fn set_property(&mut self, name: &str, value: Json) -> Result<Json, String> {
        self.request(&[Json::Str("set_property".into()), Json::Str(name.into()), value])
    }

    fn seek(&mut self, target: f64, mode: &str) -> Result<Json, String> {
        self.request(&[
            Json::Str("seek".into()),
            Json::Num(target),
            Json::Str(mode.into()),
        ])
    }

    fn quit(&mut self) {
        let _ = self.request(&[Json::Str("quit".into())]);
    }
}

// ---------------------------------------------------------------------------
// 会话状态
// ---------------------------------------------------------------------------

/// 一次 mpv 会话。
struct Session {
    src: String,
    proto: TermProto,
    /// 当前生效的区域(重启后即新值)。
    area: VideoArea,
    /// 子进程;正常退出/stop 后为 None(会话仍保留,状态显示 Finished/Failed)。
    child: Option<Child>,
    /// IPC socket 路径(退出/stop 后删除)。
    sock: PathBuf,
    ipc: Option<Ipc>,
    status: VideoStatus,
    position: Duration,
    duration: Option<Duration>,
    paused: bool,
    has_audio: bool,
    /// 逻辑音量 0.0..=1.0(未静音时的值;静音只把有效音量置 0,与 AudioCtx 同语义)。
    volume: f32,
    muted: bool,
    /// 静音前音量(toggle_mute 恢复用)。
    volume_before_mute: f32,
    /// 连续 IPC 失败计数(失联判定)。
    ipc_failures: u32,
    /// 终端写入门;None = 未启用转发(测试用 Stdio::null(),终端无 mpv 输出)。
    gate: Option<Arc<Gate>>,
    /// set_area 记下的待生效区域(几何稳定后由 tick 重启会话)。
    pending_area: Option<VideoArea>,
    pending_since: Option<Instant>,
}

impl Session {
    fn snapshot(&self) -> VideoSnapshot {
        VideoSnapshot {
            status: self.status.clone(),
            position: self.position,
            duration: self.duration,
            paused: self.paused,
            volume: if self.muted { 0.0 } else { self.volume },
            has_audio: self.has_audio,
        }
    }
}

/// ctx 会话状态机。
enum State {
    /// 无活动会话(snapshot → None)。
    Idle,
    /// 活动会话(Loading/Playing/Paused/Finished/Failed 都在 Session.status 上)。
    Live(Box<Session>),
    /// 会话未能建立即失败(spawn/socket/文件缺失):保留原因,stop() 清回 Idle。
    Failed(String),
}

// ---------------------------------------------------------------------------
// 共享句柄
// ---------------------------------------------------------------------------

struct Inner {
    state: State,
}

struct Shared {
    inner: Arc<Mutex<Inner>>,
    dirty: Arc<AtomicU64>,
}

impl Shared {
    fn new() -> Self {
        Shared {
            inner: Arc::new(Mutex::new(Inner { state: State::Idle })),
            dirty: Arc::new(AtomicU64::new(0)),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn bump_dirty(&self) {
        self.dirty.fetch_add(1, Ordering::SeqCst);
    }
}

/// 安全网:最后一个 VideoCtx 句柄释放时,若还有活动会话就直接 kill + wait(不阻塞等 quit)。
/// 正常路径由集成层显式 `stop()`;Drop 不是收尸主路径,且不得 panic。
impl Drop for Shared {
    fn drop(&mut self) {
        if Arc::strong_count(&self.inner) != 1 {
            return;
        }
        let session = {
            let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            match std::mem::replace(&mut inner.state, State::Idle) {
                State::Live(session) => Some(session),
                State::Idle | State::Failed(_) => None,
            }
        };
        if let Some(mut session) = session {
            kill_and_reap(&mut session);
            let _ = std::fs::remove_file(&session.sock);
        }
    }
}

/// 立即 kill + wait(不尝试 IPC quit;Drop 等异常路径用)。
fn kill_and_reap(session: &mut Session) {
    if let Some(child) = session.child.as_mut() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// 视频上下文(mpv 会话句柄)。与 ImageCtx/AudioCtx 同构:事件循环持有,tick 驱动。
pub struct VideoCtx {
    shared: Shared,
}

impl Default for VideoCtx {
    fn default() -> Self {
        Self::new()
    }
}

impl VideoCtx {
    pub fn new() -> Self {
        VideoCtx {
            shared: Shared::new(),
        }
    }

    /// mpv 是否可用(缓存探测:PATH 中能找到 mpv 且可执行)。
    pub fn available() -> bool {
        static CACHE: OnceLock<bool> = OnceLock::new();
        *CACHE.get_or_init(|| probe_mpv(None))
    }

    /// 启动播放(后台 spawn + 等待 IPC 就绪;期间 snapshot().status == Loading)。
    /// `src` 为本地路径;`proto` 为 None 时返回错误(集成层改走降级链)。
    pub fn start(&self, src: &str, area: VideoArea, proto: TermProto) -> Result<(), String> {
        self.start_with(src, area, proto, &[], None)
    }

    /// 停止(发 quit,等待子进程回收,清理 socket);幂等。
    pub fn stop(&self) {
        let session = {
            let mut inner = self.shared.lock();
            match std::mem::replace(&mut inner.state, State::Idle) {
                State::Live(session) => Some(session),
                State::Idle | State::Failed(_) => None,
            }
        };
        if let Some(mut session) = session {
            // 在锁外做「quit → 等 ≤2s → kill」,避免阻塞其它调用最长 2s
            if let Some(ipc) = session.ipc.as_mut() {
                ipc.quit();
            }
            session.ipc = None;
            wait_or_kill(session.child.as_mut(), QUIT_WAIT);
            if let Some(child) = session.child.as_mut() {
                let _ = child.wait();
            }
            let _ = std::fs::remove_file(&session.sock);
        }
        self.shared.bump_dirty();
    }

    pub fn toggle_pause(&self) {
        let mut inner = self.shared.lock();
        let State::Live(session) = &mut inner.state else {
            return;
        };
        if session.child.is_none() {
            return; // 已退出(mpv 不在,IPC 不可用)
        }
        let Some(ipc) = session.ipc.as_mut() else {
            return;
        };
        let target = !session.paused;
        if ipc
            .set_property("pause", Json::Bool(target))
            .is_ok()
        {
            session.paused = target;
            session.status = if target {
                VideoStatus::Paused
            } else {
                VideoStatus::Playing
            };
        }
        let _ = refresh_playback(session);
        drop(inner);
        self.shared.bump_dirty();
    }

    /// 显式暂停（幂等）。供集成层取「安全写入窗口」：mpv 暂停时**不再输出**
    /// sixel/kitty 载荷（实测：pause=true 后 0.5s 窗口内输出字节从 ~260KB 降到 0），
    /// 相对 seek(秒,可负);mpv `seek <delta> relative`。
    pub fn seek_by(&self, delta_secs: f64) {
        if !delta_secs.is_finite() {
            return;
        }
        let mut inner = self.shared.lock();
        let State::Live(session) = &mut inner.state else {
            return;
        };
        let Some(ipc) = session.ipc.as_mut() else {
            return;
        };
        if ipc.seek(delta_secs, "relative").is_ok() {
            let _ = refresh_playback(session);
        }
    }

    /// 绝对 seek 到比例位置(click-to-seek / scrubbing);mpv `seek <frac> absolute-percent`。
    ///
    /// 特例(design §3 `0` = 回到曲首):会话已**自然结束**(mpv 已退出,IPC 不可用)时,
    /// `frac == 0.0` 触发重建会话从头播放(与 AudioCtx::restart 在播完后的重建同语义);
    /// 否则 seek 会是静默 no-op,`0` 键在播完后失效。
    pub fn seek_to_fraction(&self, frac: f32) {
        if !frac.is_finite() {
            return;
        }
        let frac = frac.clamp(0.0, 1.0);
        if frac == 0.0 {
            let replay = {
                let inner = self.shared.lock();
                match &inner.state {
                    State::Live(session)
                        if session.child.is_none()
                            && session.status == VideoStatus::Finished
                            && !session.src.is_empty() =>
                    {
                        Some(RestartPlan {
                            src: session.src.clone(),
                            proto: session.proto,
                            area: session.area,
                            position: Duration::ZERO,
                            paused: false,
                            volume: session.volume,
                            muted: session.muted,
                            volume_before_mute: session.volume_before_mute,
                        })
                    }
                    _ => None,
                }
            };
            if let Some(plan) = replay {
                self.restart_session(plan);
                return;
            }
        }
        let mut inner = self.shared.lock();
        let State::Live(session) = &mut inner.state else {
            return;
        };
        let Some(ipc) = session.ipc.as_mut() else {
            return;
        };
        if ipc.seek((frac as f64) * 100.0, "absolute-percent").is_ok() {
            let _ = refresh_playback(session);
        }
    }

    /// 音量相对调整(±0.05,mpv volume 为 0–100,内部换算)。调整会解除静音(design §3)。
    pub fn adjust_volume(&self, delta: f32) {
        if !delta.is_finite() {
            return;
        }
        let mut inner = self.shared.lock();
        let State::Live(session) = &mut inner.state else {
            return;
        };
        let target = (session.volume + delta).clamp(0.0, 1.0);
        session.volume = target;
        if session.muted {
            session.muted = false;
        }
        if let Some(ipc) = session.ipc.as_mut() {
            let _ = ipc.set_property("volume", Json::Num((target * 100.0) as f64));
            let _ = ipc.set_property("mute", Json::Bool(false));
        }
        drop(inner);
        self.shared.bump_dirty();
    }

    /// 静音切换(mpv `mute` 属性;实现需记录静音前音量以恢复,与 AudioCtx 语义一致)。
    /// 接口由主 agent 于集成期补充(termio 报告 §3 `m` 键对视频缺失;2026-09-13)。
    pub fn toggle_mute(&self) {
        let mut inner = self.shared.lock();
        let State::Live(session) = &mut inner.state else {
            return;
        };
        if session.muted {
            session.muted = false;
            let restore = session.volume_before_mute.clamp(0.0, 1.0);
            session.volume = restore;
            if let Some(ipc) = session.ipc.as_mut() {
                let _ = ipc.set_property("mute", Json::Bool(false));
                let _ = ipc.set_property("volume", Json::Num((restore * 100.0) as f64));
            }
        } else {
            session.volume_before_mute = session.volume;
            session.muted = true;
            if let Some(ipc) = session.ipc.as_mut() {
                let _ = ipc.set_property("mute", Json::Bool(true));
            }
        }
        drop(inner);
        self.shared.bump_dirty();
    }

    /// 取「终端写入窗口」：等待转发器到达 sixel 载荷边界后，独占终端。
    ///
    /// 与早期实现的区别：**不需要暂停 mpv**（暂停可能停在载荷中间，仍会撕裂，且造成
    /// 卡顿）。现在由转发器保证：载荷中间持门，dlook 只在边界获得窗口。
    /// 返回 false = 等待超时，调用方跳过本次写入、下一轮再试。
    pub fn begin_write(&self) -> bool {
        self.acquire_tty().is_ok()
    }

    /// 结束写入窗口（见 `begin_write`）。
    pub fn end_write(&self) {
        self.release_tty();
    }

    /// 设置显示区域(resize 时调用)。
    ///
    /// 实现选择:**重启会话**(不是热改属性)。依据 = E14 实测:IPC `set_property
    /// vo-sixel-left/top/width/height` 返回 success 且 get_property 读得回,但画面
    /// 输出完全不变(raster/落点恒定,SIGWINCH reconfig 后亦然);kitty 走同一
    /// 选项缓存机制,同样不可依赖。故此处只记 pending,由 `tick()` 在几何稳定
    /// (AREA_SETTLE)后按新几何重启会话,并恢复位置/暂停/音量/静音。
    pub fn set_area(&self, area: VideoArea) {
        let mut inner = self.shared.lock();
        let State::Live(session) = &mut inner.state else {
            return;
        };
        if session.area == area {
            session.pending_area = None;
            session.pending_since = None;
            return;
        }
        if session.pending_area != Some(area) {
            session.pending_area = Some(area);
            session.pending_since = Some(Instant::now());
        }
    }

    pub fn snapshot(&self) -> Option<VideoSnapshot> {
        let inner = self.shared.lock();
        match &inner.state {
            State::Idle => None,
            State::Live(session) => Some(session.snapshot()),
            State::Failed(reason) => Some(failed_snapshot(reason)),
        }
    }

    /// 事件循环每 ~200ms 调用:轮询 mpv 状态、收割已退出的子进程。
    pub fn tick(&self) {
        let mut changed = false;
        let mut restart: Option<RestartPlan> = None;
        {
            let mut inner = self.shared.lock();
            let State::Live(session) = &mut inner.state else {
                return;
            };

            // 1) 子进程退出(自然结束 rc=0 → Finished;异常 → Failed)
            if let Some(child) = session.child.as_mut() {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        let reason = exit_reason(status, &session.src);
                        session.status = if status.success() {
                            VideoStatus::Finished
                        } else {
                            VideoStatus::Failed(reason)
                        };
                        session.paused = true;
                        session.ipc = None;
                        session.child = None;
                        let _ = std::fs::remove_file(&session.sock);
                        changed = true;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        session.status = VideoStatus::Failed(format!("cannot reap mpv: {e}"));
                        session.child = None;
                        session.ipc = None;
                        let _ = std::fs::remove_file(&session.sock);
                        changed = true;
                    }
                }
            }

            // 2) 轮询播放状态(IPC 失联达阈值 → Failed)
            if session.child.is_some() && session.ipc.is_some() {
                let before = session.status.clone();
                match refresh_playback(session) {
                    Ok(_) => session.ipc_failures = 0,
                    Err(_) => {
                        session.ipc_failures += 1;
                        if session.ipc_failures >= MAX_IPC_FAILURES {
                            session.status =
                                VideoStatus::Failed("mpv IPC unresponsive".to_string());
                            session.paused = true;
                            if let Some(child) = session.child.as_mut() {
                                let _ = child.kill();
                                let _ = child.wait();
                            }
                            session.child = None;
                            session.ipc = None;
                            let _ = std::fs::remove_file(&session.sock);
                        }
                    }
                }
                if session.status != before {
                    changed = true;
                }
            }

            // 3) 待生效区域:几何稳定后重启
            if let (Some(area), Some(since)) = (session.pending_area, session.pending_since) {
                if since.elapsed() >= AREA_SETTLE && area != session.area && session.child.is_some() {
                    restart = Some(RestartPlan {
                        src: session.src.clone(),
                        proto: session.proto,
                        area,
                        position: session.position,
                        paused: session.paused,
                        volume: session.volume,
                        muted: session.muted,
                        volume_before_mute: session.volume_before_mute,
                    });
                    session.pending_area = None;
                    session.pending_since = None;
                }
            }
        }
        if let Some(plan) = restart {
            self.restart_session(plan);
            changed = true;
        }
        if changed {
            self.shared.bump_dirty();
        }
    }

    /// 状态变化计数(就绪/失败/结束等异步事件),事件循环据此重绘。
    pub fn dirty_version(&self) -> u64 {
        self.shared.dirty.load(Ordering::SeqCst)
    }

    // -----------------------------------------------------------------------
    // 内部实现
    // -----------------------------------------------------------------------

    /// start 的实际实现:测试可注入额外 mpv 参数与 stdout 处理(`--ao=null` / Stdio::null)。
    /// 取「终端写入窗口」：等待 mpv 转发器处于载荷边界（不在 sixel 载荷中间）。
    ///
    /// 为什么需要它：mpv 与 dlook 都写同一个 tty。若 dlook 的字节落在 mpv 一条
    /// sixel 载荷（DCS `ESC P`..`ESC \\`，实测约 97KB）中间，终端会把它们当载荷数据，
    /// 直到 mpv 补上终止符——表现为画面撕裂/满屏乱码（独立验收 media-4 B2 与
    /// pty 场景 S7b 都实测到）。
    ///
    /// 早期做法是「暂停 mpv → 写 → 恢复」，但 mpv 可能**停在载荷中间**（暂停命令
    /// 到达时它已在写一条大载荷），所以那个方案仍会撕裂 —— 且会让播放卡顿。
    /// 现在改为：mpv 的输出经管道由 dlook 转发，转发器在载荷中间持有本门；dlook 只在
    /// 门空闲时写自己的 chrome，于是**任何时刻都只有一个写入者、且不在载荷中间**。
    ///
    /// 返回 Err 表示超时（等待超过 `GATE_TIMEOUT`）——调用方跳过本次写入、下一轮再试。
    fn acquire_tty(&self) -> Result<(), ()> {
        let gate = {
            let inner = self.shared.lock();
            match &inner.state {
                State::Live(session) => session.gate.clone(),
                _ => return Ok(()), // 无会话：终端只有 dlook 一个写入者
            }
        };
        let Some(gate) = gate else {
            return Ok(());
        };
        let deadline = Instant::now() + GATE_TIMEOUT;
        let mut st = gate.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.ui_waiting += 1;
        let got = loop {
            if st.holder == Holder::None {
                st.holder = Holder::Ui;
                break true;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break false;
            }
            let (g, _) = gate
                .cv
                .wait_timeout(st, left)
                .unwrap_or_else(PoisonError::into_inner);
            st = g;
        };
        st.ui_waiting -= 1;
        if got {
            Ok(())
        } else {
            gate.cv.notify_all();
            Err(())
        }
    }

    fn release_tty(&self) {
        let gate = {
            let inner = self.shared.lock();
            match &inner.state {
                State::Live(session) => session.gate.clone(),
                _ => None,
            }
        };
        if let Some(gate) = gate {
            let mut st = gate.state.lock().unwrap_or_else(PoisonError::into_inner);
            if st.holder == Holder::Ui {
                st.holder = Holder::None;
            }
            gate.cv.notify_all();
        }
    }

    /// `out` = None 表示生产路径：mpv stdout 走管道 + 转发线程（dlook 成为唯一写入者，
    /// 详见 `spawn_forwarder`）；Some(stdio) 供测试直接指定（如 Stdio::null()）。
    fn start_with(
        &self,
        src: &str,
        area: VideoArea,
        proto: TermProto,
        extra_args: &[&str],
        out: Option<Stdio>,
    ) -> Result<(), String> {
        let src = src.trim();
        if src.is_empty() {
            return Err("empty source".into());
        }
        if proto == TermProto::None {
            return Err("no graphics protocol".into());
        }
        // 本地路径先 fail fast(`--really-quiet` 下 mpv 不打 stderr,实测 rc=2 且无输出)
        if !src.contains("://") && !Path::new(src).exists() {
            return Err(format!("file not found: {src}"));
        }
        self.stop(); // 替换当前会话(幂等)

        let sock = unique_socket_path();
        let _ = std::fs::remove_file(&sock);
        let (program, args) = build_command(src, area, proto, &sock, extra_args);
        let forward = out.is_none();
        let stdout_cfg = out.unwrap_or_else(|| Stdio::piped());
        let mut child = Command::new(program)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(stdout_cfg)
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot spawn mpv: {e}"))?;

        // 生产路径：把 mpv 的像素输出接过来转发，并建立写入门
        let gate = if forward {
            let pipe = child.stdout.take().ok_or_else(|| {
                let _ = child.kill();
                "cannot capture mpv stdout".to_string()
            })?;
            let gate = Arc::new(Gate::default());
            let _fwd = spawn_forwarder(pipe, gate.clone());
            Some(gate)
        } else {
            None
        };

        // 等 socket 就绪(≤5s);期间子进程若退出则直接失败
        let deadline = Instant::now() + SOCKET_WAIT;
        let ipc = loop {
            if let Some(status) = child.try_wait().ok().flatten() {
                let _ = std::fs::remove_file(&sock);
                let reason = exit_reason(status, src);
                self.publish_failed(reason.clone());
                return Err(reason);
            }
            match Ipc::connect(&sock) {
                Ok(ipc) => break ipc,
                Err(_) => {}
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&sock);
                let reason = format!("mpv IPC socket not ready within {}s", SOCKET_WAIT.as_secs());
                self.publish_failed(reason.clone());
                return Err(reason);
            }
            std::thread::sleep(Duration::from_millis(10));
        };

        let mut session = Box::new(Session {
            src: src.to_string(),
            proto,
            area,
            child: Some(child),
            sock,
            ipc: Some(ipc),
            status: VideoStatus::Loading,
            position: Duration::ZERO,
            duration: None,
            paused: false,
            has_audio: false,
            volume: DEFAULT_VOLUME,
            muted: false,
            volume_before_mute: DEFAULT_VOLUME,
            gate,
            ipc_failures: 0,
            pending_area: None,
            pending_since: None,
        });

        // Loading → Playing:等属性可读(≤2s);进程死则 Failed
        let deadline = Instant::now() + READY_WAIT;
        loop {
            if let Some(status) = session.child.as_mut().and_then(|c| c.try_wait().ok().flatten()) {
                let reason = exit_reason(status, src);
                session.status = VideoStatus::Failed(reason.clone());
                session.child = None;
                session.ipc = None;
                let _ = std::fs::remove_file(&session.sock);
                self.publish(state_of(session), true);
                return Err(reason);
            }
            match refresh_playback(&mut session) {
                Ok(true) => {
                    session.status = if session.paused {
                        VideoStatus::Paused
                    } else {
                        VideoStatus::Playing
                    };
                    break;
                }
                // 文件尚未加载(duration/time-pos 都还不可用):继续等
                Ok(false) | Err(_) => {}
            }
            if Instant::now() >= deadline {
                // 超时但进程仍在:交给 tick 继续轮询(不视为失败,大文件加载可能更慢)
                session.status = VideoStatus::Playing;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        self.publish(state_of(session), true);
        Ok(())
    }

    fn publish(&self, state: State, bump: bool) {
        {
            let mut inner = self.shared.lock();
            inner.state = state;
        }
        if bump {
            self.shared.bump_dirty();
        }
    }

    fn publish_failed(&self, reason: String) {
        self.publish(State::Failed(reason), true);
    }

    /// 带新几何重启会话,并恢复位置/暂停/音量/静音(见 set_area 文档)。
    fn restart_session(&self, plan: RestartPlan) {
        // 先停旧会话(不动 pending 状态)
        {
            let session = {
                let mut inner = self.shared.lock();
                match std::mem::replace(&mut inner.state, State::Idle) {
                    State::Live(session) => Some(session),
                    State::Idle | State::Failed(_) => None,
                }
            };
            if let Some(mut session) = session {
                if let Some(ipc) = session.ipc.as_mut() {
                    ipc.quit();
                }
                session.ipc = None;
                wait_or_kill(session.child.as_mut(), QUIT_WAIT);
                let _ = std::fs::remove_file(&session.sock);
            }
        }
        let started = self.start_with(&plan.src, plan.area, plan.proto, &[], None);
        if started.is_err() {
            self.shared.bump_dirty();
            return;
        }
        // 恢复播放状态
        let mut inner = self.shared.lock();
        let State::Live(session) = &mut inner.state else {
            return;
        };
        session.volume = plan.volume;
        session.volume_before_mute = plan.volume_before_mute;
        session.muted = plan.muted;
        if plan.paused {
            session.paused = true;
            session.status = VideoStatus::Paused;
        }
        if let Some(ipc) = session.ipc.as_mut() {
            let _ = ipc.set_property("volume", Json::Num((plan.volume * 100.0) as f64));
            let _ = ipc.set_property("mute", Json::Bool(plan.muted));
            if plan.position > Duration::ZERO {
                let _ = ipc.seek(plan.position.as_secs_f64(), "absolute");
            }
            if plan.paused {
                let _ = ipc.set_property("pause", Json::Bool(true));
            }
        }
        let _ = refresh_playback(session);
        drop(inner);
        self.shared.bump_dirty();
    }
}

/// 会话重启所需的完整参数(set_area 生效路径)。
struct RestartPlan {
    src: String,
    proto: TermProto,
    area: VideoArea,
    position: Duration,
    paused: bool,
    volume: f32,
    muted: bool,
    volume_before_mute: f32,
}

// ---------------------------------------------------------------------------
// 命令构造 / 探测 / 生命周期辅助
// ---------------------------------------------------------------------------

/// 拼 mpv 命令行(纯函数,便于单测断言参数集)。
fn build_command(
    src: &str,
    area: VideoArea,
    proto: TermProto,
    sock: &Path,
    extra_args: &[&str],
) -> (String, Vec<OsString>) {
    let vo = match proto {
        TermProto::Kitty => "kitty",
        TermProto::Sixel => "sixel",
        TermProto::None => "null",
    };
    let mut args: Vec<OsString> = vec![
        format!("--vo={vo}").into(),
        // 区域几何:E14 实测两 vo 同名同语义;mpv 的 left/top 从 1 计,VideoArea 从 0 计
        format!("--vo-{vo}-left={}", area.left.saturating_add(1)).into(),
        format!("--vo-{vo}-top={}", area.top.saturating_add(1)).into(),
        format!("--vo-{vo}-cols={}", area.cols.max(1)).into(),
        format!("--vo-{vo}-rows={}", area.rows.max(1)).into(),
    ];
    // 像素尺寸:只给 cols/rows 时 mpv 在本机 foot 下拿不到终端像素尺寸,会回退
    // 320×180 小画面(集成验证期实测;media-3 验收 N1)。给出后 mpv 按像素精确渲染。
    if let Some((px_w, px_h)) = area.pixel {
        args.push(format!("--vo-{vo}-width={}", px_w.max(1)).into());
        args.push(format!("--vo-{vo}-height={}", px_h.max(1)).into());
    }
    args.extend([
        // alt-screen / config-clear 必须显式关:否则与 dlook 的 ratatui alt-screen 打架,
        // 且 reconfig 时清空终端全部图像(研究 §已知坑)
        format!("--vo-{vo}-alt-screen=no").into(),
        format!("--vo-{vo}-config-clear=no").into(),
        // 固定参数:不读终端输入(dlook 掌键位,经 IPC 转发)、静默、退出时不留 alt-screen
        "--no-terminal".into(),
        "--really-quiet".into(),
        "--loop=no".into(),
        // 精确 seek:默认 hr-seek=default 会把 relative/percent seek 吸附到关键帧
        // (实测: 3s 素材上 `seek +1 relative` 停在 0 附近、`seek 50 absolute-percent` 落到 0),
        // scrubbing/click-to-seek 需要精确落点
        "--hr-seek=yes".into(),
        "--idle=no".into(),
        "--audio-display=no".into(), // 视频模式下不弹音频可视化窗
        format!("--input-ipc-server={}", sock.display()).into(),
        src.into(),
    ]);
    for extra in extra_args {
        args.push((*extra).into());
    }
    ("mpv".to_string(), args)
}

/// 唯一 socket 路径:用户私有运行时目录 + 随机名(研究 §5.3-8/9:socket 绝不固定公开路径)。
fn unique_socket_path() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    dir.join(format!(
        "dlook-mpv-{}-{n}-{nanos}.sock",
        std::process::id()
    ))
}

/// `mpv --version` 探测(超时 2s);`path_override` 供测试注入 PATH 做隔离验证。
fn probe_mpv(path_override: Option<&std::ffi::OsStr>) -> bool {
    let mut cmd = Command::new("mpv");
    cmd.arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(path) = path_override {
        cmd.env("PATH", path);
    }
    let Ok(mut child) = cmd.spawn() else {
        return false;
    };
    let deadline = Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {}
            Err(_) => return false,
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// 等子进程退出 ≤limit,超时 kill(不阻塞更久)。
fn wait_or_kill(child: Option<&mut Child>, limit: Duration) {
    let Some(child) = child else { return };
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => {}
            Err(_) => return,
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// 可读的退出原因(信号/退出码;`--really-quiet` 下 mpv 不打 stderr,只能合成)。
fn exit_reason(status: ExitStatus, src: &str) -> String {
    use std::os::unix::process::ExitStatusExt;
    if let Some(sig) = status.signal() {
        return format!("mpv killed by signal {sig} ({src})");
    }
    match status.code() {
        Some(0) => format!("mpv exited (end of file): {src}"),
        Some(code) => format!("mpv exited with status {code} (cannot play {src})"),
        None => format!("mpv exited abnormally: {src}"),
    }
}

/// 轮询一次播放状态(time-pos/duration/pause/volume/aid),写入 session。
///
/// 返回 `Ok(loaded)`:loaded = 本次至少读到了一个媒体属性(duration/time-pos),
/// 即「文件已加载」——`start()` 用它决定 Loading → Playing 的时机。
/// 属性级错误(mpv 回 `property unavailable`)不算失败(见 `get_property_opt`),
/// 只有传输层失败才是 `Err`(tick 据此累计失联计数)。
fn refresh_playback(session: &mut Session) -> Result<bool, String> {
    let Some(ipc) = session.ipc.as_mut() else {
        return Err("no ipc".into());
    };
    let duration = ipc.get_property_opt("duration")?;
    let position = ipc.get_property_opt("time-pos")?;
    let paused = ipc.get_property_opt("pause")?;
    let volume = ipc.get_property_opt("volume")?;
    let aid = ipc.get_property_opt("aid")?;

    session.duration = duration
        .as_ref()
        .and_then(Json::as_f64)
        .filter(|d| *d > 0.0)
        .map(Duration::from_secs_f64);
    if let Some(pos) = position.as_ref().and_then(Json::as_f64) {
        session.position = Duration::from_secs_f64(pos.max(0.0));
    }
    if let Some(p) = paused.as_ref().and_then(Json::as_bool) {
        session.paused = p;
        session.status = match (p, session.status.clone()) {
            (true, _) => VideoStatus::Paused,
            (false, VideoStatus::Paused) | (false, VideoStatus::Loading) => VideoStatus::Playing,
            (false, other) => other,
        };
    }
    if let Some(vol) = volume.as_ref().and_then(Json::as_f64) {
        if !session.muted {
            session.volume = ((vol / 100.0).clamp(0.0, 1.0)) as f32;
        }
    }
    let loaded = duration.is_some() || position.is_some();
    if let Some(aid) = aid.as_ref() {
        session.has_audio = aid_has_audio(aid);
    }
    Ok(loaded)
}

/// 会话建立失败时的快照(stop() 前集成层据此显示 `✗ <原因>`)。
fn failed_snapshot(reason: &str) -> VideoSnapshot {
    VideoSnapshot {
        status: VideoStatus::Failed(reason.to_string()),
        position: Duration::ZERO,
        duration: None,
        paused: true,
        volume: DEFAULT_VOLUME,
        has_audio: false,
    }
}

fn state_of(session: Box<Session>) -> State {
    State::Live(session)
}

/// `aid` 语义(mpv 0.41 实测):有音轨 → 数字 ≥1 或字符串 "auto";无音轨 → false。
fn aid_has_audio(aid: &Json) -> bool {
    match aid {
        Json::Num(n) => *n >= 1.0,
        Json::Str(s) => s != "no" && s != "auto" || s == "auto",
        Json::Bool(b) => *b,
        _ => false,
    }
}

/// 仅供测试:让集成层以外的地方也能确认「会话真的活着」(测试断言用)。
#[cfg(test)]
impl VideoCtx {
    fn active_area(&self) -> Option<VideoArea> {
        let inner = self.shared.lock();
        match &inner.state {
            State::Live(session) if session.child.is_some() => Some(session.area),
            _ => None,
        }
    }

    fn live_pid(&self) -> Option<u32> {
        let inner = self.shared.lock();
        match &inner.state {
            State::Live(session) => session.child.as_ref().map(|c| c.id()),
            _ => None,
        }
    }

    fn socket_path(&self) -> Option<PathBuf> {
        let inner = self.shared.lock();
        match &inner.state {
            State::Live(session) => Some(session.sock.clone()),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// 测试(真实 mpv 子进程;E14 结论断言;消融对照)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod payload_scanner_tests {
    use super::*;

    fn scan(chunks: &[&[u8]]) -> Vec<bool> {
        let mut sc = PayloadScanner::default();
        chunks.iter().map(|c| sc.feed(c)).collect()
    }

    #[test]
    fn sixel_frame_is_one_dcs() {
        assert_eq!(scan(&[b"\x1bPq#0;2;0;0;0AAA\x1b\\"]), vec![false]);
        assert_eq!(scan(&[b"\x1bPqAAA", b"BBB"]), vec![true, true]);
        assert_eq!(scan(&[b"\x1bPqAAA", b"BBB\x1b\\"]), vec![true, false]);
    }

    #[test]
    fn kitty_frame_is_blocks_until_m0() {
        let mid = b"\x1b_Gm=1;AAAA\x1b\\";
        let last = b"\x1b_Gm=0;BBBB\x1b\\";
        let mut sc = PayloadScanner::default();
        assert!(sc.feed(mid), "第一块后仍在帧内");
        assert!(sc.feed(mid), "第二块后仍在帧内");
        assert!(!sc.feed(last), "m=0 块结束 → 帧结束，可放门");
    }

    #[test]
    fn kitty_missing_m_parameter_stays_inside() {
        let mut sc = PayloadScanner::default();
        assert!(sc.feed(b"\x1b_Gi=1;AAAA\x1b\\"), "无 m= 时保守处理：不放门");
    }

    #[test]
    fn esc_split_across_chunks_is_not_missed() {
        let mut sc = PayloadScanner::default();
        assert!(sc.feed(b"\x1b"), "只有 ESC：视为可能开始");
        assert!(sc.feed(b"PqAAA"), "ESC + P → sixel 帧内");
        assert!(sc.feed(b"\x1b"), "帧内 ESC：可能结束");
        assert!(!sc.feed(b"\\"), "ESC + \\ → 帧结束");
    }

    #[test]
    fn ordinary_sequences_do_not_open_a_frame() {
        let mut sc = PayloadScanner::default();
        assert!(!sc.feed(b"\x1b[2;2f"));
        assert!(!sc.feed(b"\x1b[1;1H\x1b[1mhello\x1b[0m"));
        assert!(!sc.feed(b"\x1b]0;title\x07"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_path(rel: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(rel)
    }

    /// 把素材复制成**测试独有**路径,便于用 `pgrep -f <marker>` 精确判定无残留进程。
    struct TempVideo(PathBuf);

    impl TempVideo {
        fn new(tag: &str, rel: &str) -> TempVideo {
            static N: AtomicU64 = AtomicU64::new(0);
            let src = repo_path(rel);
            let dst = std::env::temp_dir().join(format!(
                "dlook-video-{}-{tag}-{}.mp4",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::copy(&src, &dst).expect("copy fixture");
            TempVideo(dst)
        }

        fn path(&self) -> &str {
            self.0.to_str().unwrap()
        }
    }

    impl Drop for TempVideo {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    const TEST_VIDEO: &str = "docs/research/media/experiments/test-video.mp4"; // 3s + aac
    const CLIP: &str = "test/fixtures/video/clip.mp4"; // 4s 无音轨

    fn area() -> VideoArea {
        VideoArea {
            left: 2,
            top: 3,
            cols: 60,
            rows: 16,
            // 像素尺寸:集成验证期发现不给它时 mpv 回退 320×180 小画面(验收 N1),
            // 故测试默认带上,让 build_command 的像素分支也被覆盖。
            pixel: Some((640, 360)),
        }
    }

    /// 测试用 start:`--ao=null` 避免真的放音(素材含 440Hz 音轨),stdout 丢弃
    /// 避免把 kitty/sixel 转义序列写进终端(cargo test 下 fd 1 未被重定向)。
    fn start_quiet(
        ctx: &VideoCtx,
        src: &str,
        area: VideoArea,
        proto: TermProto,
    ) -> Result<(), String> {
        ctx.start_with(src, area, proto, &["--ao=null"], Some(Stdio::null()))
    }

    fn wait_status(ctx: &VideoCtx, want: &VideoStatus, limit: Duration) -> Option<VideoSnapshot> {
        let deadline = Instant::now() + limit;
        loop {
            if let Some(snap) = ctx.snapshot() {
                if &snap.status == want {
                    return Some(snap);
                }
            }
            if Instant::now() >= deadline {
                return ctx.snapshot();
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn mpv_processes_with(marker: &str) -> Vec<String> {
        let out = Command::new("pgrep")
            .args(["-af", marker])
            .output()
            .expect("pgrep");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.to_string())
            .filter(|l| l.contains("[mpv]") || l.contains("/mpv "))
            .collect()
    }

    fn fd_count() -> usize {
        std::fs::read_dir("/proc/self/fd")
            .map(|it| it.count())
            .unwrap_or(0)
    }

    // ----------------------------------------------------------- available

    #[test]
    fn available_is_true_on_this_machine() {
        if !probe_mpv(None) {
            eprintln!("mpv 不在 PATH —— 跳过(本机化测试)");
            return;
        }
        assert!(VideoCtx::available(), "缓存探测应为 true");
    }

    #[test]
    fn available_false_with_empty_path() {
        // 隔离验证:只对子进程注入 PATH,不改进程全局环境(测试可并行)
        assert!(!probe_mpv(Some(std::ffi::OsStr::new("/nonexistent"))));
        assert!(!probe_mpv(Some(std::ffi::OsStr::new(""))));
    }

    // ----------------------------------------------------------- 命令构造

    #[test]
    fn kitty_command_has_region_and_safety_flags() {
        let sock = PathBuf::from("/run/user/1000/dlook.sock");
        let (program, args) = build_command("/tmp/v.mp4", area(), TermProto::Kitty, &sock, &[]);
        let joined: Vec<String> = args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(program, "mpv");
        for want in [
            "--vo=kitty",
            "--vo-kitty-left=3", // VideoArea.left=2 → mpv 1 起 = 3
            "--vo-kitty-top=4",
            "--vo-kitty-cols=60",
            "--vo-kitty-rows=16",
            // 像素尺寸必须传:只给 cols/rows 时 mpv 在本机 foot 下拿不到终端像素尺寸,
            // 会回退 320×180 小画面(集成验证期实测,media-3 验收 N1)
            "--vo-kitty-width=640",
            "--vo-kitty-height=360",
            "--vo-kitty-alt-screen=no",
            "--vo-kitty-config-clear=no",
            "--no-terminal",
            "--really-quiet",
            "--loop=no",
            "--audio-display=no",
            "--input-ipc-server=/run/user/1000/dlook.sock",
            "/tmp/v.mp4",
        ] {
            assert!(joined.iter().any(|a| a == want), "缺少参数 {want}: {joined:?}");
        }
    }

    #[test]
    fn sixel_command_uses_sixel_region_options() {
        // E14 结论:sixel 的区域参数与 kitty 同族同名 → 走同一条拼参路径,不返回 Err
        let (_, args) = build_command(
            "/tmp/v.mp4",
            area(),
            TermProto::Sixel,
            Path::new("/tmp/s.sock"),
            &[],
        );
        let joined: Vec<String> = args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        for want in [
            "--vo=sixel",
            "--vo-sixel-left=3",
            "--vo-sixel-top=4",
            "--vo-sixel-cols=60",
            "--vo-sixel-rows=16",
            "--vo-sixel-width=640",
            "--vo-sixel-height=360",
            "--vo-sixel-alt-screen=no",
            "--vo-sixel-config-clear=no",
        ] {
            assert!(joined.iter().any(|a| a == want), "缺少参数 {want}: {joined:?}");
        }
    }

    #[test]
    fn e14_option_evidence_still_holds_locally() {
        // E14 断言的回归化:sixel 与 kitty 的区域选项在**本机 mpv** 上存在且同名同数
        if !probe_mpv(None) {
            return;
        }
        for vo in ["sixel", "kitty"] {
            let out = Command::new("mpv")
                .args([format!("--vo={vo}"), "--list-options".into()])
                .output()
                .expect("mpv --list-options");
            let text = String::from_utf8_lossy(&out.stdout);
            for opt in ["left", "top", "cols", "rows", "width", "height"] {
                let needle = format!("--vo-{vo}-{opt}");
                assert!(
                    text.contains(&needle),
                    "E14 结论失效:{needle} 不在 vo={vo} 选项表里"
                );
            }
        }
    }

    // ----------------------------------------------------------- 控制链

    #[test]
    fn start_plays_and_control_chain_works() {
        if !probe_mpv(None) {
            return;
        }
        let video = TempVideo::new("chain", TEST_VIDEO);
        let ctx = VideoCtx::new();
        start_quiet(&ctx, video.path(), area(), TermProto::Kitty).expect("start");

        // 1) 2s 内进入 Playing;元数据可读(3s 素材 + 音轨)
        let snap = wait_status(&ctx, &VideoStatus::Playing, Duration::from_secs(3)).expect("snapshot");
        assert_eq!(snap.status, VideoStatus::Playing);
        assert!(snap.duration.is_some(), "duration 应可读");
        let d = snap.duration.unwrap();
        assert!(
            (d.as_secs_f64() - 3.0).abs() < 0.5,
            "duration 应≈3s,实际 {d:?}"
        );
        assert!(snap.has_audio, "test-video.mp4 含音轨 → has_audio");

        // 2) 暂停:进入 Paused(seek 断言在暂停态做——播放态下位置自身在前进,
        //    「落点」无法与自然推进区分,见下)
        ctx.toggle_pause();
        let snap = ctx.snapshot().unwrap();
        assert!(snap.paused, "toggle_pause 后应暂停");
        assert_eq!(snap.status, VideoStatus::Paused);

        // 3) 相对 seek(+1s):暂停态下位置只可能被 seek 移动 → 断言「前进 ≥0.8s」无歧义
        let before = ctx.snapshot().unwrap().position;
        ctx.seek_by(1.0);
        let mut moved = false;
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            let now = ctx.snapshot().unwrap().position;
            if now.as_secs_f64() >= before.as_secs_f64() + 0.8 {
                moved = true;
                break;
            }
            ctx.tick();
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(moved, "seek_by(1.0) 后位置未前进: {before:?}");
        let after_rel = ctx.snapshot().unwrap().position;

        // 4) 绝对 seek:回到 0%,再跳到 50%(暂停态 → 位置冻结在目标处,可精确断言)
        ctx.seek_to_fraction(0.0);
        let mut back = false;
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if ctx.snapshot().unwrap().position < Duration::from_millis(300) {
                back = true;
                break;
            }
            ctx.tick();
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            back,
            "seek_to_fraction(0.0) 未回到片头: {after_rel:?} → {:?}",
            ctx.snapshot().unwrap().position
        );
        ctx.seek_to_fraction(0.5);
        let mut half = false;
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if ctx.snapshot().unwrap().position.as_secs_f64() >= 1.2 {
                half = true;
                break;
            }
            ctx.tick();
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            half,
            "seek_to_fraction(0.5) 未到 ~1.5s: {:?}",
            ctx.snapshot().unwrap().position
        );

        // 5) 恢复播放
        ctx.toggle_pause();
        let snap = ctx.snapshot().unwrap();
        assert!(!snap.paused, "再次 toggle_pause 应恢复播放");
        assert_eq!(snap.status, VideoStatus::Playing);

        // 5) 音量下降
        let vol_before = ctx.snapshot().unwrap().volume;
        ctx.adjust_volume(-0.2);
        let vol_after = ctx.snapshot().unwrap().volume;
        assert!(
            vol_after < vol_before,
            "音量应下降: {vol_before} → {vol_after}"
        );

        // 6) 静音切换(记录静音前音量,恢复一致)
        let pre_mute = ctx.snapshot().unwrap().volume;
        ctx.toggle_mute();
        assert_eq!(ctx.snapshot().unwrap().volume, 0.0);
        ctx.toggle_mute();
        assert!(
            (ctx.snapshot().unwrap().volume - pre_mute).abs() < 0.001,
            "解除静音应恢复静音前音量"
        );

        // 7) stop 幂等 + 无残留进程/socket
        let sock = ctx.socket_path().expect("socket path");
        let pid = ctx.live_pid().expect("live pid");
        ctx.stop();
        ctx.stop(); // 幂等
        assert!(ctx.snapshot().is_none(), "stop 后 snapshot 应为 None");
        assert!(!sock.exists(), "socket 文件应被清理: {sock:?}");
        assert!(
            !Path::new(&format!("/proc/{pid}")).exists() || {
                // 极端情况下 pid 被回收给别的进程:再用 pgrep 兜底
                mpv_processes_with(video.path()).is_empty()
            },
            "mpv 子进程仍在: pid={pid}"
        );
        for marker in [sock.to_string_lossy().into_owned(), video.path().to_string()] {
            assert!(
                mpv_processes_with(&marker).is_empty(),
                "残留 mpv 进程提到 {marker}: {:?}",
                mpv_processes_with(&marker)
            );
        }
    }

    #[test]
    fn proto_none_errs_and_missing_file_errs_readably() {
        let ctx = VideoCtx::new();
        let err = start_quiet(&ctx, "/tmp/whatever.mp4", area(), TermProto::None).unwrap_err();
        assert!(err.contains("graphics protocol"), "原因应可读: {err}");
        assert!(ctx.snapshot().is_none(), "未启动会话 → snapshot None");

        let err = start_quiet(&ctx, "/nonexistent.mp4", area(), TermProto::Kitty).unwrap_err();
        assert!(err.contains("not found"), "缺失文件原因应可读: {err}");
        assert!(err.contains("/nonexistent.mp4"));

        let err = start_quiet(&ctx, "   ", area(), TermProto::Kitty).unwrap_err();
        assert!(err.contains("empty source"), "{err}");
    }

    #[test]
    fn runtime_failure_surfaces_as_failed_status() {
        // 文件存在但内容不是视频 → mpv 启动后很快退出,status 应变 Failed(而非 panic/静默)
        let junk = std::env::temp_dir().join(format!("dlook-video-junk-{}.mp4", std::process::id()));
        std::fs::write(&junk, b"not a video at all").unwrap();
        let ctx = VideoCtx::new();
        let res = start_quiet(&ctx, junk.to_str().unwrap(), area(), TermProto::Kitty);
        // start 可能直接 Err(mpv 在 socket 就绪前就退出),也可能 Ok 后由 tick 检出
        let failed = match res {
            Err(e) => e,
            Ok(()) => {
                let deadline = Instant::now() + Duration::from_secs(3);
                loop {
                    ctx.tick();
                    match ctx.snapshot().map(|s| s.status) {
                        Some(VideoStatus::Failed(e)) => break e,
                        _ if Instant::now() >= deadline => panic!("未在 3s 内转 Failed"),
                        _ => std::thread::sleep(Duration::from_millis(50)),
                    }
                }
            }
        };
        assert!(
            failed.contains("status") || failed.contains("mpv"),
            "失败原因应可读: {failed}"
        );
        ctx.stop();
        let _ = std::fs::remove_file(&junk);
    }

    #[test]
    fn tick_is_idempotent_and_does_not_leak_fds() {
        if !probe_mpv(None) {
            return;
        }
        let video = TempVideo::new("tick", CLIP); // 无音轨素材:顺带覆盖 has_audio=false
        let ctx = VideoCtx::new();
        start_quiet(&ctx, video.path(), area(), TermProto::Kitty).expect("start");
        assert!(wait_status(&ctx, &VideoStatus::Playing, Duration::from_secs(3)).is_some());
        assert!(!ctx.snapshot().unwrap().has_audio, "clip.mp4 无音轨");

        let fds_before = fd_count();
        for _ in 0..100 {
            ctx.tick();
        }
        let fds_after = fd_count();
        assert!(
            fds_after <= fds_before + 2,
            "fd 泄漏: {fds_before} → {fds_after}"
        );

        ctx.stop();
        for _ in 0..20 {
            ctx.tick(); // stop 后 tick 应为无操作
        }
        assert!(ctx.snapshot().is_none());
    }

    #[test]
    fn natural_end_becomes_finished() {
        if !probe_mpv(None) {
            return;
        }
        let video = TempVideo::new("eof", CLIP); // 4s,`--loop=no` → 播完 mpv 退出 rc=0
        let ctx = VideoCtx::new();
        start_quiet(&ctx, video.path(), area(), TermProto::Kitty).expect("start");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            ctx.tick();
            match ctx.snapshot().map(|s| s.status) {
                Some(VideoStatus::Finished) => break,
                Some(VideoStatus::Failed(e)) => panic!("应自然结束而非失败: {e}"),
                _ if Instant::now() >= deadline => panic!("未在 10s 内结束"),
                _ => std::thread::sleep(Duration::from_millis(100)),
            }
        }
        ctx.stop();
    }

    #[test]
    fn seek_to_zero_replays_finished_session() {
        if !probe_mpv(None) {
            return;
        }
        let video = TempVideo::new("replay", CLIP);
        let ctx = VideoCtx::new();
        start_quiet(&ctx, video.path(), area(), TermProto::Kitty).expect("start");
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            ctx.tick();
            if ctx.snapshot().map(|s| s.status) == Some(VideoStatus::Finished) {
                break;
            }
            assert!(Instant::now() < deadline, "未在 12s 内播完");
            std::thread::sleep(Duration::from_millis(100));
        }
        // 0 键语义:回到曲首 = 重建会话重新播放(design §3)
        ctx.seek_to_fraction(0.0);
        let snap = wait_status(&ctx, &VideoStatus::Playing, Duration::from_secs(6))
            .expect("重播后应回到 Playing");
        assert_eq!(snap.status, VideoStatus::Playing);
        assert!(snap.position < Duration::from_secs(2), "重播应从片头开始");
        ctx.stop();
    }

    #[test]
    fn set_area_restarts_session_preserving_playback() {
        if !probe_mpv(None) {
            return;
        }
        let video = TempVideo::new("area", CLIP);
        let ctx = VideoCtx::new();
        let a1 = area();
        start_quiet(&ctx, video.path(), a1, TermProto::Kitty).expect("start");
        assert!(wait_status(&ctx, &VideoStatus::Playing, Duration::from_secs(3)).is_some());

        // 暂停 + 前进到 ~1s,便于断言「状态被保留」
        ctx.toggle_pause();
        let pid_before = ctx.live_pid().expect("pid");
        let a2 = VideoArea {
            left: 5,
            top: 1,
            cols: 40,
            rows: 10,
            pixel: Some((400, 240)),
        };
        ctx.set_area(a2);
        let deadline = Instant::now() + Duration::from_secs(6);
        loop {
            ctx.tick();
            if ctx.active_area() == Some(a2) {
                break;
            }
            if Instant::now() >= deadline {
                panic!("set_area 未在 6s 内生效(当前 {:?})", ctx.active_area());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let pid_after = ctx.live_pid().expect("pid after");
        assert_ne!(pid_before, pid_after, "区域变化应重启会话(新进程)");
        let snap = wait_status(&ctx, &VideoStatus::Paused, Duration::from_secs(3)).expect("snap");
        assert!(snap.paused, "重启后应保持暂停状态");
        assert_eq!(ctx.snapshot().unwrap().status, VideoStatus::Paused);
        ctx.stop();
    }

    #[test]
    fn json_parser_reads_mpv_responses() {
        let v = parse_json(r#"{"data":false,"request_id":0,"error":"success"}"#).unwrap();
        assert_eq!(v.get("data"), Some(&Json::Bool(false)));
        assert_eq!(v.get("error").and_then(Json::as_str), Some("success"));
        let v = parse_json(r#"{"data":123.5,"request_id":7,"error":"success"}"#).unwrap();
        assert_eq!(v.get("data").and_then(Json::as_f64), Some(123.5));
        let v = parse_json(r#"{"data":"auto","request_id":1,"error":"success"}"#).unwrap();
        assert_eq!(v.get("data").and_then(Json::as_str), Some("auto"));
        let v = parse_json(r#"{"data":null,"request_id":2,"error":"success"}"#).unwrap();
        assert_eq!(v.get("data"), Some(&Json::Null));
        let v = parse_json(r#"{"event":"file-loaded"}"#).unwrap();
        assert!(v.get("request_id").is_none());
        assert!(parse_json("{").is_none());
        assert_eq!(aid_has_audio(&Json::Str("auto".into())), true);
        assert_eq!(aid_has_audio(&Json::Bool(false)), false);
        assert_eq!(aid_has_audio(&Json::Num(1.0)), true);
    }

    // ----------------------------------------------------------- 消融对照
    // 默认 `#[ignore]`:不拖慢常规 `cargo test video::`;需要时手动跑:
    //   cd rs && cargo test video::ablation -- --ignored --nocapture

    /// 消融 A(去掉 socket 就绪等待):spawn 后立刻 connect 一次,统计失败率。
    /// 结论:失败率显著 > 0 → 「就绪等待」不是可有可无的防御。
    #[test]
    #[ignore]
    fn ablation_a_without_socket_ready_wait() {
        if !probe_mpv(None) {
            return;
        }
        let video = TempVideo::new("abl-a", TEST_VIDEO);
        let mut failures = 0;
        const N: usize = 12;
        for i in 0..N {
            let sock = unique_socket_path();
            let (program, args) = build_command(
                video.path(),
                area(),
                TermProto::Kitty,
                &sock,
                &["--ao=null"],
            );
            let mut child = Command::new(program)
                .args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn");
            // 立即 connect(不等 socket 就绪)—— 消融版本
            let immediate = Ipc::connect(&sock);
            let failed_immediately = immediate.is_err();
            if failed_immediately {
                failures += 1;
            }
            // 带等待的版本(生产实现)作对照
            let mut waited_ok = false;
            let deadline = Instant::now() + SOCKET_WAIT;
            while Instant::now() < deadline {
                if Ipc::connect(&sock).is_ok() {
                    waited_ok = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(&sock);
            println!("  第 {i} 次: 立即 connect 失败={failed_immediately}, 等待后成功={waited_ok}");
            assert!(waited_ok, "带就绪等待的版本必须成功");
        }
        println!("消融 A 结论: {failures}/{N} 次立即 connect 失败(带等待版本全部成功)");
        assert!(failures > 0, "若为 0 则该消融无区分度,需增大 N 或改环境");
    }

    /// 消融 B(用 observe_property 事件订阅替代状态轮询):量出额外复杂度与滞后。
    /// 结论:事件订阅需要独占读线程(本实现的所有请求共用一条流),且**首次值拿不到**
    /// (observe_property 只在属性变化时推送,必须补一次 get_property),
    /// 事件洪泛时无法按 request_id 精确取值——pull 轮询与 200ms tick 同构且实现更小。
    #[test]
    #[ignore]
    fn ablation_b_observe_property_event_subscription() {
        if !probe_mpv(None) {
            return;
        }
        let video = TempVideo::new("abl-b", TEST_VIDEO);
        let sock = unique_socket_path();
        let (program, args) =
            build_command(video.path(), area(), TermProto::Kitty, &sock, &["--ao=null"]);
        let mut child = Command::new(program)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn");
        let deadline = Instant::now() + SOCKET_WAIT;
        let mut ipc = loop {
            if let Ok(ipc) = Ipc::connect(&sock) {
                break ipc;
            }
            assert!(Instant::now() < deadline, "socket 未就绪");
            std::thread::sleep(Duration::from_millis(5));
        };

        // observe_property 订阅 time-pos:注册成功后**没有初始值**,只有变化才有事件
        ipc.request(&[
            Json::Str("observe_property".into()),
            Json::Num(1.0),
            Json::Str("time-pos".into()),
        ])
        .expect("observe_property");

        // 用「读一行」模拟事件订阅:事件行与响应行混在同一条流上
        let mut events = 0;
        let got_initial;
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_millis(700) {
            if let Some(line) = ipc.take_line() {
                println!("  事件行: {}", &line[..line.len().min(100)]);
                if line.contains("property-change") {
                    events += 1;
                }
            } else if ipc.read_more().is_err() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        // 首值必须额外 get_property —— 这正是「事件订阅替代轮询」的隐藏成本
        let first = ipc.get_property("time-pos");
        got_initial = matches!(first, Ok(Json::Num(_)));
        println!(
            "消融 B 结论: 700ms 内 property-change 事件 {events} 条;<b>首值仍需 get_property</b>={got_initial}; \
             订阅期间响应行与事件行在同一条流上互相穿插(需要 request_id 配对或独占读线程)"
        );
        assert!(got_initial, "事件订阅拿不到首值,必须补 get_property");

        ipc.quit();
        let _ = child.wait();
        let _ = std::fs::remove_file(&sock);
    }

    /// 消融 C(去掉 ready 等待,即不等 duration 可读就置 Playing):观察 start 返回后
    /// 立即 snapshot 的状态是否不可靠(duration 可能仍为 None)。
    #[test]
    #[ignore]
    fn ablation_c_without_ready_wait_duration_may_be_missing() {
        if !probe_mpv(None) {
            return;
        }
        let video = TempVideo::new("abl-c", TEST_VIDEO);
        let sock = unique_socket_path();
        let (program, args) =
            build_command(video.path(), area(), TermProto::Kitty, &sock, &["--ao=null"]);
        let mut child = Command::new(program)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn");
        let deadline = Instant::now() + SOCKET_WAIT;
        let mut ipc = loop {
            if let Ok(ipc) = Ipc::connect(&sock) {
                break ipc;
            }
            assert!(Instant::now() < deadline, "socket 未就绪");
            std::thread::sleep(Duration::from_millis(5));
        };
        // 立刻(不等 ready)读 duration
        let immediate = ipc.get_property("duration");
        let immediate_ok = matches!(&immediate, Ok(Json::Num(_)));
        std::thread::sleep(Duration::from_millis(400));
        let later = ipc.get_property("duration");
        println!("消融 C 结论: connect 后立即读 duration={immediate:?}(可用={immediate_ok}), 400ms 后={later:?}");
        ipc.quit();
        let _ = child.wait();
        let _ = std::fs::remove_file(&sock);
    }
}
