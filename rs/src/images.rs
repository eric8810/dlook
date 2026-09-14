//! 图片渲染支持(DECISIONS D15):本地/远程/data: 图片 → 终端图形协议。
//!
//! 架构:
//!   - picker 探测(kitty/sixel/iTerm2 → halfblocks 回退)由 ratatui-image 提供,
//!     在进入事件循环前调用 `Picker::from_query_stdio()`(它需要直接读写 stdin 收响应)。
//!   - `ImageCtx` 是跨 rebuild 存活的图片注册表:src → 加载/解码/协议编码结果。
//!     网络 IO 与 sixel/kitty 编码耗时,全部丢给后台线程;主线程只查表。
//!   - 加载/重编码完成 bump `dirty` 计数,事件循环发现版本变化即重排(同热重载路径)。
//!   - 图片以 `SlicedProtocol` 形式缓存(ratatui-image sliced 模块):滚动视口下
//!     支持按行部分可见(Kitty 用 unicode placeholder 行偏移,Sixel 按 band 裁剪,
//!     iTerm2 逐行切片,Halfblocks 行跳过)。
//!   - 宽度变化(resize)触发后台重编码;期间继续用旧协议渲染(可能超宽被裁剪),
//!     避免闪烁回占位行。重编码完成 last-write-wins,最终收敛到当前宽度。
//!
//! 环境变量 `DLOOK_IMAGE_PROTOCOL`:
//!   - auto(默认):按需探测(文档含图片才查询,纯文本文档零启动开销)
//!   - halfblocks:跳过探测,强制半块字符(测试/慢终端用)
//!   - off:完全禁用图片,`![alt](src)` 降级为可点击链接

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use image::DynamicImage;
use ratatui::layout::Size;
use ratatui_image::picker::Picker;
use ratatui_image::sliced::SlicedProtocol;

use crate::links;

/// 本地图片文件大小上限(64MB)。
const MAX_LOCAL_BYTES: u64 = 64 * 1024 * 1024;
/// 远程图片大小上限(16MB)。
const MAX_REMOTE_BYTES: u64 = 16 * 1024 * 1024;
/// 远程请求整体超时。
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
/// 解码后最大边长(更大的先降采样,防内存暴涨;终端显示用不到更高分辨率)。
const MAX_DECODED_PX: u32 = 2560;
/// 图片目标行数上限(内容宽度由调用方给出)。
pub const MAX_IMG_ROWS: u16 = 2048;

/// 渲染时对一个图片 src 的查询结果。
pub enum Render {
    /// 协议已就绪(含尺寸信息,见 `SlicedProtocol::size()`)。
    Ready(Arc<SlicedProtocol>),
    /// 后台加载/编码中(此次渲染显示占位行)。
    Loading,
    /// 加载失败(原因用于占位行展示)。
    Failed(Arc<str>),
}

/// 注册表内的一条缓存。
enum Entry {
    /// 首次加载线程在途。
    Loading,
    /// 就绪:解码图 + 当前协议编码 + 编码时目标尺寸。
    Ready {
        img: Arc<DynamicImage>,
        proto: Arc<SlicedProtocol>,
        target: Size,
    },
    /// 宽度变化后的重编码在途:保留旧协议继续渲染,inflight = 编码中目标尺寸。
    Reencoding {
        img: Arc<DynamicImage>,
        old: Arc<SlicedProtocol>,
        inflight: Size,
    },
    /// 失败(缓存不重试;避免 rebuild 循环反复请求坏链接)。
    Failed(Arc<str>),
}

/// 线程间共享的内部状态(加载线程完成时写回结果)。
#[derive(Clone)]
struct Shared {
    map: Arc<Mutex<HashMap<String, Entry>>>,
    dirty: Arc<AtomicU64>,
}

impl Shared {
    fn insert_ready(&self, src: &str, img: Arc<DynamicImage>, proto: SlicedProtocol, target: Size) {
        let mut map = self.map.lock().unwrap();
        map.insert(src.to_string(), Entry::Ready {
            img,
            proto: Arc::new(proto),
            target,
        });
        self.dirty.fetch_add(1, Ordering::SeqCst);
    }

    fn insert_failed(&self, src: &str, msg: String) {
        let mut map = self.map.lock().unwrap();
        map.insert(src.to_string(), Entry::Failed(msg.into()));
        self.dirty.fetch_add(1, Ordering::SeqCst);
    }
}

/// 图片注册表:跨 rebuild/resize/导航存活,事件循环持有。
pub struct ImageCtx {
    /// 完全禁用(DLOOK_IMAGE_PROTOCOL=off):渲染层降级为链接。
    off: bool,
    picker: Mutex<Option<Picker>>,
    shared: Shared,
}

/// 启动期的图片策略(acquire_policy 的结果)。
pub enum ImagePolicy {
    /// 已探测/已强制的 picker。
    Picker(Picker),
    /// 未探测(纯文本文档);首次需要图片时惰性 halfblocks
    /// (事件读取线程已存活,不能再探测 stdin)。
    Lazy,
    /// 显式禁用。
    Off,
}

impl ImageCtx {
    pub fn new(policy: ImagePolicy) -> Self {
        let (off, picker) = match policy {
            ImagePolicy::Picker(p) => (false, Some(p)),
            ImagePolicy::Lazy => (false, None),
            ImagePolicy::Off => (true, None),
        };
        Self {
            off,
            picker: Mutex::new(picker),
            shared: Shared {
                map: Arc::new(Mutex::new(HashMap::new())),
                dirty: Arc::new(AtomicU64::new(0)),
            },
        }
    }

    /// 是否完全禁用(渲染层据此降级为链接)。
    pub fn disabled(&self) -> bool {
        self.off
    }

    /// 当前终端图形协议(视频委托 mpv 时据此选 vo;D16)。
    /// None = 未探测 / 已禁用 / 仅 halfblocks —— 视频走降级链。
    pub fn graphics_proto(&self) -> Option<ratatui_image::picker::ProtocolType> {
        use ratatui_image::picker::ProtocolType;
        if self.off {
            return None;
        }
        let guard = self.picker.lock().unwrap();
        match guard.as_ref().map(|p| p.protocol_type()) {
            Some(ProtocolType::Kitty) => Some(ProtocolType::Kitty),
            Some(ProtocolType::Sixel) => Some(ProtocolType::Sixel),
            _ => None,
        }
    }

    /// 终端单元格的像素尺寸(宽, 高);未探测到时为 None。
    ///
    /// 视频委托 mpv 时需要它:mpv 的 `--vo-<vo>-width/height` 是**像素**单位,
    /// 只给 cols/rows 时 mpv 在本机 foot 下拿不到终端像素尺寸、回退到 320×180
    /// (小画面,远小于 body 区)。集成验证期发现,见 media-3 验收 N1。
    pub fn cell_pixel_size(&self) -> Option<(u16, u16)> {
        if self.off {
            return None;
        }
        let guard = self.picker.lock().unwrap();
        guard.as_ref().map(|p| {
            let fs = p.font_size();
            (fs.width, fs.height)
        })
    }

    /// dirty 计数(加载/重编码完成会 +1;事件循环比对后触发重排)。
    pub fn dirty_version(&self) -> u64 {
        self.shared.dirty.load(Ordering::SeqCst)
    }

    /// 拿到可用的 picker(未探测时惰性 halfblocks)。
    fn picker(&self) -> Picker {
        let mut guard = self.picker.lock().unwrap();
        if guard.is_none() {
            *guard = Some(Picker::halfblocks());
        }
        guard.clone().unwrap()
    }

    /// 查询一个 src 的渲染状态;未缓存则 spawn 后台加载线程,
    /// 宽度变化则 spawn 重编码线程并返回旧协议(避免闪烁为占位行)。
    pub fn get_or_load(&self, src: &str, base_dir: &Path, target: Size) -> Render {
        let mut map = self.shared.map.lock().unwrap();
        match map.get(src) {
            Some(Entry::Ready {
                img,
                proto,
                target: t,
            }) if *t == target => Render::Ready(proto.clone()),

            Some(Entry::Ready { img, proto, .. }) => {
                // 宽度变化 → 重编码;期间旧协议继续渲染
                let img = img.clone();
                let old = proto.clone();
                let picker = self.picker();
                let src_owned = src.to_string();
                map.insert(
                    src.to_string(),
                    Entry::Reencoding {
                        img: img.clone(),
                        old,
                        inflight: target,
                    },
                );
                let shared = self.shared.clone();
                std::thread::spawn(move || {
                    match encode_protocol(&img, &picker, target) {
                        Ok(proto) => shared.insert_ready(&src_owned, img, proto, target),
                        Err(e) => shared.insert_failed(&src_owned, e),
                    }
                });
                // 返回旧协议(从上面 clone 的 old)
                if let Entry::Reencoding { old, .. } = map.get(src).unwrap() {
                    Render::Ready(old.clone())
                } else {
                    Render::Loading
                }
            }

            Some(Entry::Reencoding { old, inflight, .. }) if *inflight == target => {
                Render::Ready(old.clone())
            }

            Some(Entry::Reencoding { img, old, .. }) => {
                // 重编码目标又变了(连续 resize):再 spawn 一个,last-write-wins
                let img = img.clone();
                let old = old.clone();
                let picker = self.picker();
                let src_owned = src.to_string();
                if let Some(Entry::Reencoding { inflight, .. }) = map.get_mut(src) {
                    *inflight = target;
                }
                let shared = self.shared.clone();
                std::thread::spawn(move || {
                    match encode_protocol(&img, &picker, target) {
                        Ok(proto) => shared.insert_ready(&src_owned, img, proto, target),
                        Err(e) => shared.insert_failed(&src_owned, e),
                    }
                });
                Render::Ready(old)
            }

            Some(Entry::Loading) => Render::Loading,
            Some(Entry::Failed(e)) => Render::Failed(e.clone()),

            None => {
                // 首次:spawn 完整加载(IO + 解码 + 编码)
                let picker = self.picker();
                let src_owned = src.to_string();
                let base = base_dir.to_path_buf();
                map.insert(src.to_string(), Entry::Loading);
                let shared = self.shared.clone();
                std::thread::spawn(move || {
                    match load_and_encode(&src_owned, &base, &picker, target) {
                        Ok((img, proto)) => shared.insert_ready(&src_owned, img, proto, target),
                        Err(e) => shared.insert_failed(&src_owned, e),
                    }
                });
                Render::Loading
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 加载管线:bytes 获取 → 解码 → 降采样 → 协议编码
// ---------------------------------------------------------------------------

/// 完整加载:获取字节 + 解码 + 协议编码(后台线程内执行)。
fn load_and_encode(
    src: &str,
    base_dir: &Path,
    picker: &Picker,
    target: Size,
) -> Result<(Arc<DynamicImage>, SlicedProtocol), String> {
    let bytes = load_bytes(src, base_dir)?;
    let img = Arc::new(decode_and_scale(&bytes)?);
    let proto = encode_protocol(&img, picker, target)?;
    Ok((img, proto))
}

/// 从已解码的图创建协议编码(重编码路径复用)。
fn encode_protocol(
    img: &DynamicImage,
    picker: &Picker,
    target: Size,
) -> Result<SlicedProtocol, String> {
    SlicedProtocol::new(picker, img.clone(), Some(target))
        .map_err(|e| format!("encode failed: {e}"))
}

/// 获取图片字节:本地路径 / file: URL / http(s) / data: URL。
fn load_bytes(src: &str, base_dir: &Path) -> Result<Vec<u8>, String> {
    let t = src.trim();
    if t.is_empty() {
        return Err("empty image source".into());
    }
    if let Some(rest) = t.strip_prefix("data:") {
        return decode_data_url(rest);
    }
    if t.starts_with("http://") || t.starts_with("https://") {
        return fetch_remote(t);
    }
    // file: URL(file:///abs、file://abs、file:/abs 统一还原为绝对路径;
    // 忽略 ?query——图片模式直开用 query 作缓存指纹 key,文件变化 → 新 key → 重新加载)
    let local = if let Some(rest) = t.strip_prefix("file:") {
        let no_query = rest.split('?').next().unwrap_or("");
        format!("/{}", no_query.trim_start_matches('/'))
    } else {
        t.to_string()
    };
    let path = links::normalize(base_dir, &local);
    let meta = std::fs::metadata(&path).map_err(|_| format!("not found: {t}"))?;
    if meta.is_dir() {
        return Err(format!("is a directory: {t}"));
    }
    if meta.len() > MAX_LOCAL_BYTES {
        return Err(format!("too large (>{}MB): {t}", MAX_LOCAL_BYTES / 1024 / 1024));
    }
    std::fs::read(&path).map_err(|_| format!("unreadable: {t}"))
}

/// `data:[mediatype][;base64],<payload>` → 字节。
fn decode_data_url(rest: &str) -> Result<Vec<u8>, String> {
    let Some((meta, payload)) = rest.split_once(',') else {
        return Err("malformed data: URL".into());
    };
    if meta.split(';').any(|p| p.eq_ignore_ascii_case("base64")) {
        let cleaned: String = payload.chars().filter(|c| !c.is_whitespace()).collect();
        // 标准与 URL-safe 两种字母表都试
        for alphabet in [&base64_simd::STANDARD, &base64_simd::URL_SAFE] {
            if let Ok(v) = alphabet.decode_to_vec(cleaned.as_bytes()) {
                return Ok(v);
            }
        }
        Err("invalid base64 payload".into())
    } else {
        Ok(percent_decode_bytes(payload))
    }
}

/// 字节级 percent-decode(data: URL 的非 base64 payload 是二进制,
/// links::percent_decode 的 lossy UTF-8 转换会损坏字节)。
fn percent_decode_bytes(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// http/https 获取(rustls;跟随重定向;16MB 上限)。
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
        .take(MAX_REMOTE_BYTES)
        .read_to_end(&mut buf)
        .map_err(|e| format!("read failed: {e}"))?;
    if buf.is_empty() {
        return Err("empty response".into());
    }
    Ok(buf)
}

/// 解码 + 超大图降采样(保比例,三角形滤波)。
fn decode_and_scale(bytes: &[u8]) -> Result<DynamicImage, String> {
    let img = image::load_from_memory(bytes).map_err(|e| format!("decode failed: {e}"))?;
    let (w, h) = (img.width(), img.height());
    if w.max(h) > MAX_DECODED_PX {
        let scale = MAX_DECODED_PX as f64 / w.max(h) as f64;
        let resized = img.resize_exact(
            (w as f64 * scale) as u32,
            (h as f64 * scale) as u32,
            image::imageops::FilterType::Triangle,
        );
        Ok(resized)
    } else {
        Ok(img)
    }
}

// ---------------------------------------------------------------------------
// picker 获取(入口在进入事件循环前调用)
// ---------------------------------------------------------------------------

/// 是否值得在启动时探测终端协议(图片模式 / 文档可能含图片)。
/// 纯文本文档跳过探测,保持即时启动(无响应终端上探测要 2s 超时)。
pub fn should_query_protocol(mode: crate::lang::Mode, content: &str) -> bool {
    use crate::lang::Mode;
    match mode {
        Mode::Image => true,
        Mode::Markdown => content.contains("!["),
        _ => false,
    }
}

/// 按 `DLOOK_IMAGE_PROTOCOL` 环境变量与探测需求返回图片策略。
///
/// 必须在进入 raw mode / 事件循环前调用(from_query_stdio 直接读写 stdin)。
pub fn acquire_policy(query: bool) -> ImagePolicy {
    let forced = std::env::var("DLOOK_IMAGE_PROTOCOL")
        .unwrap_or_default()
        .to_lowercase();
    match forced.as_str() {
        "off" | "none" | "disable" | "disabled" => ImagePolicy::Off,
        "halfblocks" | "blocks" | "ascii" => ImagePolicy::Picker(Picker::halfblocks()),
        _ => {
            // auto:按需探测;探测失败时 from_query_stdio 内部已回退 halfblocks
            if query {
                match Picker::from_query_stdio() {
                    Ok(p) => ImagePolicy::Picker(p),
                    Err(_) => ImagePolicy::Lazy,
                }
            } else {
                ImagePolicy::Lazy
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- data: URL ----

    #[test]
    fn data_url_base64() {
        // "hello" 的标准 base64
        let bytes = decode_data_url("image/png;base64,aGVsbG8=").unwrap();
        assert_eq!(bytes, b"hello");
        // 带空白字符
        let bytes = decode_data_url("image/png;base64,aGVs\n bG8=").unwrap();
        assert_eq!(bytes, b"hello");
    }

    #[test]
    fn data_url_percent_encoded() {
        let bytes = decode_data_url("image/png,%89PNG%0d%0a").unwrap();
        assert_eq!(bytes, b"\x89PNG\x0d\x0a");
    }

    #[test]
    fn data_url_malformed() {
        assert!(decode_data_url("image/png;base64,!!!").is_err());
        assert!(decode_data_url("nocomma").is_err());
    }

    // ---- 本地加载 ----

    #[test]
    fn load_local_png_fixture() {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../test/fixtures/img/tiny.png");
        let bytes = load_bytes(p.to_str().unwrap(), Path::new("")).unwrap();
        // PNG magic
        assert_eq!(&bytes[..4], b"\x89PNG");
        let img = decode_and_scale(&bytes).unwrap();
        assert!(img.width() > 0 && img.height() > 0);
    }

    #[test]
    fn load_local_missing_fails() {
        assert!(load_bytes("./no/such/file.png", Path::new("/")).is_err());
    }

    // ---- 协议创建 + 注册表(半块路径,不依赖终端)----

    #[test]
    fn sliced_protocol_halfblocks_sizing() {
        let picker = Picker::halfblocks();
        // 40x40 像素图,目标 80 列宽 → 自然尺寸 4x2 格(halfblocks 字号 10x20)
        let img = DynamicImage::new_rgb8(40, 40);
        let proto =
            SlicedProtocol::new(&picker, img, Some(Size::new(80, MAX_IMG_ROWS))).unwrap();
        let s = proto.size();
        assert_eq!(s.width, 4);
        assert_eq!(s.height, 2);
    }

    #[test]
    fn ctx_load_and_cache() {
        let ctx = ImageCtx::new(ImagePolicy::Lazy); // 惰性 halfblocks
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../test/fixtures/img/tiny.png");
        let src = p.to_str().unwrap().to_string();
        let target = Size::new(80, MAX_IMG_ROWS);
        // 首次 → Loading(线程在途)
        match ctx.get_or_load(&src, Path::new(""), target) {
            Render::Loading => {}
            _ => panic!("expected Loading on first call"),
        }
        // 轮询等待线程完成(最多 5s)
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match ctx.get_or_load(&src, Path::new(""), target) {
                Render::Ready(proto) => {
                    assert!(proto.size().height > 0);
                    break;
                }
                Render::Failed(e) => panic!("load failed: {e}"),
                Render::Loading => {
                    if std::time::Instant::now() > deadline {
                        panic!("image load did not finish in 5s");
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
        // dirty 计数已被 bump
        assert!(ctx.dirty_version() >= 1);
    }

    #[test]
    fn ctx_failed_is_cached() {
        let ctx = ImageCtx::new(ImagePolicy::Lazy);
        let target = Size::new(80, MAX_IMG_ROWS);
        let _ = ctx.get_or_load("./definitely-missing.png", Path::new("/"), target);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match ctx.get_or_load("./definitely-missing.png", Path::new("/"), target) {
                Render::Failed(_) => break,
                Render::Loading => {
                    if std::time::Instant::now() > deadline {
                        panic!("failure did not land in 5s");
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                _ => panic!("expected Failed"),
            }
        }
    }
}
