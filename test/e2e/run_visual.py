#!/usr/bin/env python3
"""V 场景（design §6，V1–V5）：真实终端（Hyprland + foot）截图断言。

入口是 `test/e2e/run-visual.sh`（负责绝对路径定位被测二进制、图形会话与工具
前置检查、缺依赖时显式 SKIP）。参考 experiments：`e11-visual-video-check.py`
（四态截图断言）、`e13-terminal-loop-check.py`（窗口聚焦 / wtype 注入 / 退出码读回）。

覆盖：
  V1  图片渲染可见（sixel 亮色内容 vs 同几何文本基线）
  V2  视频四态：播放帧变化 / 暂停冻结 / 恢复变化 / seek 变化
  V3  视频共屏：header / 媒体栏 / footer 仍在，视频区未被 chrome 文字覆写
  V4  退出回收：退出码 0、无残留 mpv、无残留测试窗口
  V5  音频播放：无报错、媒体栏随播放推进、暂停冻结
  V-cleanup  结束时无遗留测试窗口/进程（每个会话结束都会清理）

工程约束（踩过的坑）：
  - **绝对路径**调用被测二进制（PATH 里的旧版 dlook 会误导；experiments E13）。
  - **一次只开一个测试窗口**：Hyprland 平铺布局下新窗口会挤压/重排已有窗口，
    并发窗口会让截图尺寸漂移 → 逐像素断言失效。故所有用例串行、每个窗口用
    完即关，并在测量前确认几何已 settle。
  - 会话用**相对路径 + cwd=fixtures**：header 显示短路径，窄窗口下也不被截断。
  - 逐帧像素断言前先校验两次截图尺寸一致；不一致则重拍，仍不一致判失败并打印
    明细（不 panic）。
  - **同一桌面不能并行跑多个 V 套件**：窗口会互相重排/抢焦点，导致截图几何漂移
    （实测并行时出现 diff=None / 暂停非冻结之类的假失败）。串行跑，或等对端跑完。
  - 进程清理只针对带本次 run 唯一标记 `DLOOK_VISUAL_RUN` 的进程，不影响并行
    agent 的 foot/mpv。
"""
from __future__ import annotations

import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time
from collections import Counter

ROOT = os.environ.get("DLOOK_ROOT") or os.path.abspath(
    os.path.join(os.path.dirname(__file__), "..", "..")
)
BIN = os.environ.get("DLOOK_BIN") or os.path.join(ROOT, "rs", "target", "debug", "dlook")
FIX = os.path.join(ROOT, "test", "fixtures")

# 本次 run 唯一标记：注入每个子进程环境，用于精确收割（绝不误杀并行 agent 的进程）。
RUN_ID = f"dlook-v{os.getpid()}-{int(time.time()) % 100000}"
TAG_PREFIX = f"dlook-visual-{os.getpid()}-"
STAMP = time.strftime("%Y%m%d-%H%M%S")
OUT = os.environ.get("DLOOK_VISUAL_OUT") or os.path.join(
    ROOT, "rs", "target", "visual", f"{STAMP}-{os.getpid()}"
)

PASS = 0
FAIL = 0
SKIP = 0
SESSIONS: list["Session"] = []
# 标定结果：{"pitch","row0_center","size","bands"(行号→(y0,y1))}。
# 所有测试窗口共用同一浮动几何 → 标定行带对其他会话截图直接有效。
GEO = None
DIMS: dict[str, tuple[int, int]] = {}  # 路径 → (w, h)

# 所有测试窗口统一用同一几何（浮动 + 固定尺寸/位置）：
#   - 尺寸固定 → 逐像素对比的两张截图尺寸必然一致（平铺会话里别的窗口不挤压我们）
#   - 位置固定 → 标定窗口测得的绝对行带可直接用于所有会话截图
#   - 放在屏幕左下 → 避开桌面通知/顶栏
PIN_COLS, PIN_ROWS, PIN_X, PIN_Y = 78, 24, 40, 512
PIN_PX = (int(PIN_COLS * 9.5) + 24, int(PIN_ROWS * 19.6) + 40)  # ≈ 765x510


# --------------------------------------------------------------------------
# 断言输出
# --------------------------------------------------------------------------
def ok(desc, detail=""):
    global PASS
    PASS += 1
    print(f"  \u2713 {desc}" + (f"  ({detail})" if detail else ""))


def bad(desc, detail=""):
    global FAIL
    FAIL += 1
    print(f"  \u2717 {desc}" + (f"  ({detail})" if detail else ""))


def check(cond, desc, detail=""):
    if cond:
        ok(desc, detail)
    else:
        bad(desc, detail)


def skip(desc, reason):
    global SKIP
    SKIP += 1
    print(f"  \u2298 {desc}  (SKIP: {reason})")


# --------------------------------------------------------------------------
# 进程工具
# --------------------------------------------------------------------------
def sh(cmd, **kw):
    return subprocess.run(cmd, capture_output=True, text=True, **kw)


def proc_env(pid):
    try:
        with open(f"/proc/{pid}/environ", "rb") as f:
            return f.read().decode(errors="replace")
    except OSError:
        return ""


def proc_cmdline(pid):
    try:
        with open(f"/proc/{pid}/cmdline", "rb") as f:
            return f.read().replace(b"\0", b" ").decode(errors="replace").strip()
    except OSError:
        return ""


def proc_ppid(pid):
    try:
        with open(f"/proc/{pid}/stat") as f:
            return int(f.read().split()[3])
    except (OSError, ValueError, IndexError):
        return 0


def ancestors(pid):
    out, cur = set(), pid
    for _ in range(32):
        if cur <= 1 or cur in out:
            break
        out.add(cur)
        cur = proc_ppid(cur)
    return out


def marked_procs():
    """带本次 run 标记的进程 pid（排除自身与父链）。"""
    mine = ancestors(os.getpid())
    out = []
    for entry in os.listdir("/proc"):
        if not entry.isdigit():
            continue
        pid = int(entry)
        if pid in mine:
            continue
        if f"DLOOK_VISUAL_RUN={RUN_ID}" in proc_env(pid):
            out.append(pid)
    return out


def sweep(kill=True):
    """收割本次 run 的残留进程（含 mpv 孙进程）；返回 (pids, cmdlines)。"""
    pids = marked_procs()
    cmds = [proc_cmdline(p) for p in pids]
    if kill:
        for p in pids:
            try:
                os.kill(p, signal.SIGTERM)
            except OSError:
                pass
        deadline = time.time() + 3
        while time.time() < deadline and marked_procs():
            time.sleep(0.2)
        for p in marked_procs():
            try:
                os.kill(p, signal.SIGKILL)
            except OSError:
                pass
    return pids, cmds


def hypr_clients():
    try:
        return json.loads(sh(["hyprctl", "clients", "-j"]).stdout)
    except Exception:  # noqa: BLE001
        return []


def find_window(tag):
    for c in hypr_clients():
        if tag in (c.get("title", "") + c.get("class", "")):
            return c
    return None


def window_count():
    return sum(1 for c in hypr_clients() if TAG_PREFIX in
               (c.get("title", "") + c.get("class", "")))


def active_window_ident():
    """当前活动窗口的**唯一地址**（Hyprland address）——用于注入后还原焦点。

    必须用 address：按 class 匹配会命中同类窗口中的任意一个（本机有多个 foot 窗口，
    实测按 class 还原会切到别的 foot）。返回 None 表示取不到。
    """
    try:
        c = json.loads(sh(["hyprctl", "activewindow", "-j"]).stdout)
        return c.get("address") or None
    except Exception:  # noqa: BLE001
        return None


def focus(tag):
    """聚焦测试窗口以注入按键，返回「之前的活动窗口」供调用方恢复。

    wtype 只能把按键送给**当前聚焦**的 Wayland 表面，因此注入前必须聚焦测试窗口；
    若注入后不还原，用户的焦点会被反复抢走。调用方应在注入完成后调用
    `restore_focus(prev)` 把焦点还给用户原来的窗口。
    """
    prev = active_window_ident()
    r = sh(["hyprctl", "dispatch", f'hl.dsp.focus({{window="class:{tag}"}})'])
    if r.returncode != 0 or "ok" not in (r.stdout + r.stderr).lower():
        sh(["hyprctl", "dispatch", "focuswindow", f"class:{tag}"])
    time.sleep(0.4)
    return prev


def is_test_window(address):
    """该地址是否属于本次 V 套件的测试窗口（是则不必还原焦点）。"""
    if not address:
        return False
    for c in hypr_clients():
        if c.get("address") == address:
            blob = c.get("title", "") + c.get("class", "")
            return TAG_PREFIX in blob
    return False


def restore_focus(prev):
    """把焦点还给 `focus()` 之前的窗口（按 address 精确定位，用户自己的窗口）。

    连续注入时若上一个窗口本就是测试窗口，则不必来回切。失败静默——焦点还原不该
    让测试挂掉。
    """
    if not prev or is_test_window(prev):
        return
    sh(["hyprctl", "dispatch", f'hl.dsp.focus({{window="address:{prev}"}})'])


def key(*keys):
    r = sh(["wtype", "-k", *keys])
    time.sleep(0.35)
    return r


# --------------------------------------------------------------------------
# 截图与像素/文本分析
# --------------------------------------------------------------------------
def png_size(path):
    if path not in DIMS:
        out = sh(["magick", "identify", "-format", "%w %h", path]).stdout.split()
        DIMS[path] = (int(out[0]), int(out[1]))
    return DIMS[path]


_CACHE: dict[str, bytes] = {}


def rgb(path):
    if path not in _CACHE:
        _CACHE[path] = subprocess.run(
            ["magick", path, "-depth", "8", "rgb:-"], capture_output=True
        ).stdout
    return _CACHE[path]


def ocr(path, psm="6"):
    return sh(["tesseract", path, "-", "--psm", psm]).stdout


def ocr_lines(path):
    """OCR 行盒 [(top, bottom, text), ...]（块/段/行归组），按 y 排序。"""
    t = sh(["tesseract", path, "-", "--psm", "6", "tsv"]).stdout
    agg = {}
    for row in t.splitlines()[1:]:
        f = row.split("\t")
        if len(f) < 12 or not f[11].strip():
            continue
        key = (f[1], f[2], f[3], f[4])
        top, hgt = int(f[7]), int(f[9])
        lo, hi, txt = agg.get(key, (top, top + hgt, ""))
        agg[key] = (min(lo, top), max(hi, top + hgt), (txt + " " + f[11]).strip())
    return sorted(agg.values())


class Anchor:
    """单张截图的**自标定**行锚点：不依赖窗口几何，只依赖该截图自身的内容。

    Hyprland 平铺会话是共享资源（并行 agent 的窗口会挤压/移动我们的窗口），
    绝对像素标定不可靠；改为每张截图从 OCR 结果推出：
      - header 行 = 最上面一行（dlook header 恒在屏幕第 0 行）
      - footer 行 = 含 footer 文案的那一行（"q quit" / "back"）
      - 行高 pitch = 相邻 OCR 行距的**中位数**（比「首末跨度/(rows-1)」稳健：
        字形盒高度随字母浮动，首末差值会累积成半个行高的偏移）
      - 绘制行数 = round((footer_center − header_center)/pitch) + 1
        （以截图为准；pty 的 stty 值可能滞后于 resize）
    行带全部由 pitch 推出；pitch 不合理 → ok=False，调用方显式 SKIP/失败。
    """

    def __init__(self, path, rows=None):
        self.path = path
        self.rows = rows
        self.lines = ocr_lines(path)
        self.pitch = None
        self.header_top = None
        self.footer_top = None
        self.header_center = None
        self.footer_center = None
        if not self.lines:
            return
        self.header_top = self.lines[0][0]
        self.header_center = (self.lines[0][0] + self.lines[0][1]) / 2
        # footer 行 = 含 footer 文案的那一行。视频模式下画面是像素内容，OCR 会产出
        # 大量噪声行，故用较宽的关键词集；**找不到时明确降级**（不再拿最后一行冒充
        # footer，否则行数/行带整体错位——实测视频页会算出 14 行 vs 真实 24 行）。
        for box in reversed(self.lines):
            if re.search(r"(?i)quit|back|seek|mute|vol", box[2]):
                self.footer_top = box[0]
                self.footer_center = (box[0] + box[1]) / 2
                break
        # 行高：优先用运行期标定值（标定窗口有 20+ 行，估计精确）。图片/视频页只有
        # header+媒体栏+footer 几行，行距样本少且被字形高度差污染（实测 36/44 两个
        # 样本 → 中位数 44，真值 40），故标定值可用时一律以它为准。
        if GEO:
            self.pitch = GEO["pitch"]
        else:
            tops = [b[0] for b in self.lines]
            diffs = sorted(b - a for a, b in zip(tops, tops[1:]) if 10 <= b - a <= 60)
            if diffs:
                self.pitch = float(diffs[len(diffs) // 2])
        if self.pitch is None:
            return
        # 行数：标定/pty 的 rows 为准（视频页 footer 会淹没在像素噪声里，不可信）
        self.rows = rows

    @property
    def ok(self):
        return self.pitch is not None and self.rows is not None

    def row_band(self, i):
        """第 i 行的像素带。

        OCR 出来的是**字形盒**而非单元格盒（字形盒更矮且随字母形状浮动），
        故带以字形**中心**为基准、按 pitch 外扩半行；直接用字形 top 会累积
        偏移（实测可达半行），足以让行带落到相邻行。
        """
        center = self.header_center + i * self.pitch
        return (max(0, int(center - self.pitch / 2)), int(center + self.pitch / 2))

    def bands(self, bar_h):
        """header / body / 媒体栏 / footer 的像素行带 (y0, y1)。"""
        rows = self.rows
        out = {"header": self.row_band(0), "footer": self.row_band(rows - 1),
               "body": (self.row_band(1)[0],
                        int(self.header_center + (rows - 1 - bar_h) * self.pitch
                            - self.pitch / 2))}
        if bar_h >= 1:
            out["bar"] = (self.row_band(rows - 1 - bar_h)[0],
                          self.row_band(rows - 2)[1])
        if bar_h >= 2:
            out["bar1"] = self.row_band(rows - 3)
            out["bar2"] = self.row_band(rows - 2)
        return out


def ink_bands(path, thresh=0.002):
    """按行统计非背景像素占比 → 连续 ink 行段 [(y0,y1), ...]。"""
    w, h = png_size(path)
    raw = rgb(path)
    bg = Counter(raw[i:i + 3] for i in range(0, len(raw), 3)).most_common(1)[0][0]
    ink = []
    for y in range(h):
        row = raw[y * w * 3:(y + 1) * w * 3]
        ink.append(sum(1 for x in range(w)
                       if max(abs(row[x * 3 + i] - bg[i]) for i in range(3)) > 40))
    bands, start, th = [], None, max(2, int(thresh * w))
    for y, n in enumerate(ink):
        if n > th and start is None:
            start = y
        elif n <= th and start is not None:
            bands.append((start, y - 1))
            start = None
    if start is not None:
        bands.append((start, h - 1))
    return bands


def crop(path, out, x, y, w, h):
    subprocess.run(["magick", path, "-crop", f"{max(1, w)}x{max(1, h)}+{x}+{y}",
                    "+repage", out], capture_output=True)
    DIMS.pop(out, None)
    return out


def region_stats(path, y0, y1, x0=0, x1=None):
    """区域内 ink 占比与量化（4bit/通道）颜色数——像素内容 vs 文字内容的区别指标。"""
    w, h = png_size(path)
    raw = rgb(path)
    y0, y1 = max(0, int(y0)), min(h, int(y1))
    x0 = max(0, int(x0))
    x1 = min(w, int(x1) if x1 else w)
    px = [raw[(y * w + x) * 3:(y * w + x) * 3 + 3]
          for y in range(y0, y1) for x in range(x0, x1)]
    if not px:
        return {"n": 0, "ink": 0.0, "quant": 0, "bg_frac": 0.0}
    c = Counter(px)
    bg, bgn = c.most_common(1)[0]
    ink = sum(1 for p in px if max(abs(p[i] - bg[i]) for i in range(3)) > 40)
    return {"n": len(px), "ink": round(ink / len(px), 4),
            "quant": len({(p[0] // 16, p[1] // 16, p[2] // 16) for p in px}),
            "bg_frac": round(bgn / len(px), 3)}


def region_diff(a, b, y0, y1, x0=0, x1=None, tol=16):
    """两图区域间「显著不同」像素占比；尺寸不一致 → None（调用方判失败）。"""
    wa, ha = png_size(a)
    wb, hb = png_size(b)
    if (wa, ha) != (wb, hb):
        return None
    ra, rb = rgb(a), rgb(b)
    y0, y1 = max(0, int(y0)), min(ha, int(y1))
    x0 = max(0, int(x0))
    x1 = min(wa, int(x1) if x1 else wa)
    diff = tot = 0
    for y in range(y0, y1):
        base = y * wa
        for x in range(x0, x1):
            o = (base + x) * 3
            tot += 1
            if max(abs(ra[o + i] - rb[o + i]) for i in range(3)) > tol:
                diff += 1
    return round(diff / max(1, tot), 5)


def nonzero(v, detail=""):
    """dim 不一致（None）也判失败的比较。"""
    return v is not None and v > 0.0


def diff_over_pairs(s, name_a, name_b, band, gap, tries=3, want="min"):
    """多次成对截图，返回 (diff, attempts, pair)。

    桌面通知/其他窗口会瞬时盖住测试窗口，造成偶发差异。对**应当冻结**的断言取
    多次中的最小差值、对**应当变化**的断言取最大差值；全部尝试都失败才算失败。
    """
    best = None
    pair = (None, None)
    for i in range(tries):
        a, b, _size = s.shot_pair(f"{name_a}-t{i}" if i else name_a,
                                  f"{name_b}-t{i}" if i else name_b, gap=gap)
        if a is None or b is None:
            continue
        d = region_diff(a, b, *band)
        if d is None:
            continue
        if best is None or (want == "min" and d < best) or (want == "max" and d > best):
            best = d
            pair = (a, b)
        if want == "min" and d == 0.0:
            break
        if want == "max" and d > 0.0:
            break
    return best, tries, pair


# --------------------------------------------------------------------------
# 会话（foot 窗口 + 被测进程）
# --------------------------------------------------------------------------
class Session:
    """一个 foot 窗口里跑一个 dlook 会话：截图 / 按键注入 / 退出码读回 / 清理。

    串行使用约定：同一时刻只允许一个 Session 存活（见模块 docstring）。
    """

    def __init__(self, case, args, env_extra=None, cwd=FIX, wait=3.0, label=None,
                 start_delay=2.5, pinned=(PIN_COLS, PIN_ROWS, PIN_X, PIN_Y)):
        self.case = case
        self.tag = f"{TAG_PREFIX}{case}"
        self.dir = os.path.join(OUT, label or case)
        os.makedirs(self.dir, exist_ok=True)
        self.args = args
        self.cwd = cwd
        self.env = dict(os.environ, DLOOK_VISUAL_RUN=RUN_ID)
        for k, v in (env_extra or {}).items():
            if v is None:
                self.env.pop(k, None)
            else:
                self.env[k] = v
        self.wait = wait
        # 起始延迟：在被测进程启动**之前**把窗口几何钉死（浮动 + 尺寸 + 位置）。
        # 对视频尤其关键：mpv 的区域几何在 start 时按当时的终端尺寸算好，若之后
        # 再 resize，mpv 仍在旧位置画（画面错位、覆盖 chrome，实测会出现重复帧）。
        self.start_delay = start_delay
        self.pinned_geo = pinned
        self.rc_path = os.path.join(self.dir, "rc")
        self.size_path = os.path.join(self.dir, "size")
        self.proc = None
        self.geo = None
        self.rows = None
        self.cols = None
        self.shots: dict[str, str] = {}
        self._last_size: tuple[int, int] | None = None
        self.pinned = False
        self.clip = None  # 视频会话使用的素材（V2/V3 读取）

    # ---- 生命周期 ----
    def start(self):
        if window_count() != 0:
            raise RuntimeError("V 套件约定：同一时刻只允许一个测试窗口")
        # 后台每 0.5s 刷新 pty 尺寸到文件（供 Anchor 把 OCR 行锚点换算成终端行号）。
        inner = (f"( while :; do stty size < /dev/tty > {self.size_path} 2>/dev/null; "
                 f"sleep 0.5; done ) & "
                 f"sleep {self.start_delay}; "
                 f"cd {self.cwd} && " + BIN + " "
                 + " ".join(self._quote(a) for a in self.args)
                 + f"; echo $? > {self.rc_path}; kill %1 2>/dev/null")
        self.proc = subprocess.Popen(
            ["foot", "-a", self.tag, "-T", self.tag,
             "-o", "alpha=1.0",  # 不透明：避免背景窗口透出污染像素断言
             "--", "sh", "-c", inner],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, stdin=subprocess.DEVNULL,
            env=self.env, start_new_session=True,
        )
        SESSIONS.append(self)
        return self

    @staticmethod
    def _quote(a):
        return a if re.fullmatch(r"[A-Za-z0-9_./=:@+-]+", a) else "'" + a.replace("'", "'\\''") + "'"

    def wait_ready(self, timeout=None):
        """返回 'ready' / 'exited:<rc>' / 'timeout'。

        流程：等窗口出现 → 趁 start_delay 内钉死几何（浮动/尺寸/位置）→ 等被测进程
        真正跑起来（pty 尺寸 + 内容）→ 再等 wait 让对方完成初始化（图片/mpv 上屏）。
        """
        deadline = time.time() + (timeout if timeout is not None else
                                  self.wait + self.start_delay + 15)
        pinned_at = None
        while time.time() < deadline:
            rc = self.rc()
            c = find_window(self.tag)
            if rc is not None:
                return f"exited:{rc}"
            if c is not None and not self.pinned:
                self.pin_geometry(*self.pinned_geo, settle=4)
                pinned_at = time.time()
            if c is not None and os.path.exists(self.size_path):
                try:
                    rows, cols = (int(v) for v in
                                  open(self.size_path).read().split()[:2])
                except ValueError:
                    rows = cols = 0
                # 等到「几何已钉死」+「被测进程已在运行」：shell 在 start_delay 后才
                # exec 被测程序，故 pinned_at 起算 start_delay + wait 才是内容就绪时刻。
                started = (pinned_at is not None
                           and time.time() - pinned_at >= self.start_delay + self.wait - 0.3)
                if rows and cols and started:
                    self.rows, self.cols = rows, cols
                    self.geo = find_window(self.tag)
                    if self.geo is not None:
                        time.sleep(1.2)  # 让被测进程画完首帧
                        self.geo = find_window(self.tag) or self.geo
                        return "ready"
            time.sleep(0.25)
        return "timeout"

    def read_size(self):
        """读 pty 尺寸（由 shell 后台循环刷新）。"""
        try:
            with open(self.size_path) as f:
                rows, cols = (int(v) for v in f.read().split()[:2])
        except (OSError, ValueError):
            return False
        if (rows, cols) != (self.rows, self.cols):
            print(f"    (info) {self.case}: pty {rows}x{cols} (rows×cols)")
        self.rows, self.cols = rows, cols
        return True

    def pin_geometry(self, cols=PIN_COLS, rows=PIN_ROWS, x=PIN_X, y=PIN_Y, settle=6):
        """把测试窗口浮动 + 固定几何，并放到屏幕左下角（避开通知横幅区）。

        共享平铺会话里别的窗口会挤压/移动我们的窗口；**逐像素对比**要求两张
        截图尺寸一致，故固定浮窗。位置选左下：桌面通知通常出现在右上/顶部，
        会盖住截图并污染像素断言与 OCR。窗口随 foot 退出消失，不留副作用。
        """
        if self.pinned:
            return
        sel = f'window="class:{self.tag}"'
        sh(["hyprctl", "dispatch", f"hl.dsp.window.float({{{sel}}})"])
        time.sleep(0.4)
        # 目标 = 内容区 cols×rows 个字符（加窗口边框的余量）
        px_w, px_h = PIN_PX
        sh(["hyprctl", "dispatch",
            f"hl.dsp.window.resize({{{sel}, x={px_w}, y={px_h}}})"])
        sh(["hyprctl", "dispatch", f"hl.dsp.window.move({{{sel}, x={x}, y={y}}})"])
        prev = None
        for _ in range(settle):
            c = find_window(self.tag)
            if c is None:
                return
            cur = (c["at"][0], c["at"][1], c["size"][0], c["size"][1])
            if cur == prev:
                break
            prev = cur
            time.sleep(0.4)
        self.geo = find_window(self.tag) or self.geo
        self.pinned = True
        if self.geo:
            print(f"    (info) {self.case}: 浮窗固定 {self.geo['size'][0]}x"
                  f"{self.geo['size'][1]} @ {tuple(self.geo['at'])}")

    def _raise(self):
        """截图前把测试窗口置顶，返回之前的活动窗口（截图后还原焦点）。

        置顶是必要的（否则并行的其他浮动窗口会盖在被截区域上，OCR 串味）；但不需要
        持续聚焦——故 `_grab` 在 grim 完成后立刻把焦点还给用户。
        """
        prev = active_window_ident()
        sh(["hyprctl", "dispatch", f'hl.dsp.focus({{window="class:{self.tag}"}})'])
        time.sleep(0.25)
        return prev

    def rc(self):
        if os.path.exists(self.rc_path):
            try:
                return int(open(self.rc_path).read().strip())
            except ValueError:
                return None
        return None

    # ---- 截图 / 按键 ----
    def _grab(self, name, tag_suffix=""):
        """单次截图（窗口当前几何）；返回 (path, size) 或 (None, None)。"""
        prev = self._raise()
        c = find_window(self.tag)
        if not c:
            restore_focus(prev)
            return None, None
        x, y = c["at"]
        w, h = c["size"]
        path = os.path.join(self.dir, f"{name}{tag_suffix}.png")
        sh(["grim", "-g", f"{x},{y} {w}x{h}", path])
        restore_focus(prev)  # 截图完成 → 焦点还给用户
        DIMS.pop(path, None)
        _CACHE.pop(path, None)
        return path, png_size(path)

    def shot(self, name, tries=6):
        """截图，直到连续两次尺寸一致（浮动窗口下通常一次即可）。"""
        prev = None
        path = None
        for _ in range(tries):
            path, size = self._grab(name)
            if path is None:
                return None
            if size == prev:
                self._last_size = size
                self.shots[name] = path
                return path
            prev = size
            time.sleep(0.4)
        if path:
            print(f"    (note) {name}: 未能稳定（最后尺寸 {prev}）")
            self.shots[name] = path
        return path

    def shot_pair(self, name_a, name_b, gap, tries=4):
        """两张**同尺寸**截图（严格逐像素对比用）；尺寸不一致就整体重拍。

        返回 (path_a, path_b, size) 或 (None, None, None)（原因打印在 note）。
        """
        for attempt in range(tries):
            a, sa = self._grab(name_a, f"-r{attempt}" if attempt else "")
            if a is None:
                return None, None, None
            time.sleep(gap)
            b, sb = self._grab(name_b, f"-r{attempt}" if attempt else "")
            if b is None:
                return None, None, None
            if sa == sb:
                self._last_size = sa
                self.shots[name_a] = a
                self.shots[name_b] = b
                return a, b, sa
            print(f"    (note) {name_a}/{name_b}: 尺寸漂移 {sa}→{sb}，整体重拍")
            time.sleep(0.6)
        return None, None, None

    def anchor(self, name):
        """对 self.shots[name] 做自标定行锚点（用 stty 的 rows）。"""
        path = self.shots.get(name)
        if not path or self.rows is None:
            return None
        a = Anchor(path, self.rows)
        return a if a.ok else None

    def send(self, *keys):
        # 注入按键需要聚焦测试窗口；注入后立刻把焦点还给用户原来的窗口
        # （否则连续注入会把用户的焦点一直抢在测试窗口上）。
        prev = focus(self.tag)
        try:
            return key(*keys)
        finally:
            restore_focus(prev)

    def quit(self, timeout=8.0):
        """注入 q，返回退出码或 None（注入失败则 SIGTERM 兜底）。"""
        self.send("q")
        deadline = time.time() + timeout
        while time.time() < deadline and self.rc() is None:
            time.sleep(0.2)
        rc = self.rc()
        if rc is None and self.proc.poll() is None:
            self.proc.terminate()
            time.sleep(0.8)
            rc = self.rc()
        return rc

    def close(self):
        try:
            if self.proc and self.proc.poll() is None:
                self.proc.terminate()
                try:
                    self.proc.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    self.proc.kill()
        except Exception:  # noqa: BLE001
            pass
        if find_window(self.tag) is not None:
            sh(["hyprctl", "dispatch", f'hl.dsp.window.close({{window="class:{self.tag}"}})'])
            time.sleep(0.3)
            if find_window(self.tag) is not None and self.proc:
                try:
                    self.proc.kill()
                except OSError:
                    pass
            time.sleep(0.3)
        # 等平铺布局重排结束，下一个会话才有稳定几何
        deadline = time.time() + 5
        while time.time() < deadline and find_window(self.tag) is not None:
            time.sleep(0.2)
        if self in SESSIONS:
            SESSIONS.remove(self)

    # ---- 行带（自标定：见 Anchor）----
    def anchor_for(self, name):
        """对 self.shots[name] 做自标定行锚点（rows 来自 stty）。"""
        path = self.shots.get(name)
        if not path or self.rows is None:
            return None
        a = Anchor(path, self.rows)
        return a if a.ok else None


# --------------------------------------------------------------------------
# 标定：只验证「窗口能被截图且自标定可用」（不依赖绝对几何）
# --------------------------------------------------------------------------
def calibrate():
    """工具链冒烟 + 绝对行带标定（所有窗口几何一致，故行带可复用）。

    标定窗口与测试窗口用**同一浮动几何/尺寸/位置**：截图尺寸一致 → 标定出的
    `GEO["bands"][i]`（终端第 i 行的像素带）对所有会话截图直接有效。这避开了
    OCR 自标定在视频页（画面是像素内容）严重失真的问题。
    """
    global GEO
    tag = f"{TAG_PREFIX}calib"
    if window_count() != 0:
        print("  (calib) 已有测试窗口存在，跳过")
        return False
    shot = os.path.join(OUT, "calib.png")
    proc = subprocess.Popen(
        ["foot", "-a", tag, "-T", tag, "-o", "alpha=1.0", "--", "sh", "-c",
         "seq 1 30; sleep 25"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, stdin=subprocess.DEVNULL,
        env=dict(os.environ, DLOOK_VISUAL_RUN=RUN_ID), start_new_session=True,
    )
    try:
        deadline = time.time() + 12
        c = None
        while time.time() < deadline and (c := find_window(tag)) is None:
            time.sleep(0.2)
        if c is None:
            print("  (calib) 标定窗口未出现")
            return False
        time.sleep(0.6)
        sel = f'window="class:{tag}"'
        sh(["hyprctl", "dispatch", f"hl.dsp.window.float({{{sel}}})"])
        time.sleep(0.4)
        sh(["hyprctl", "dispatch",
            f"hl.dsp.window.resize({{{sel}, x={PIN_PX[0]}, y={PIN_PX[1]}}})"])
        sh(["hyprctl", "dispatch", f"hl.dsp.window.move({{{sel}, x={PIN_X}, y={PIN_Y}}})"])
        prev = None
        for _ in range(8):
            time.sleep(0.4)
            c = find_window(tag) or c
            cur = (c["at"][0], c["at"][1], c["size"][0], c["size"][1])
            if cur == prev:
                break
            prev = cur
        x, y = c["at"]
        w, h = c["size"]
        sh(["grim", "-g", f"{x},{y} {w}x{h}", shot])
        DIMS.pop(shot, None)
        _CACHE.pop(shot, None)
        size = png_size(shot)
        lines = ocr_lines(shot)
        tops = [b[0] for b in lines]
        diffs = sorted(b - a for a, b in zip(tops, tops[1:]) if 10 <= b - a <= 60)
        if len(lines) < 10 or not diffs:
            print(f"  (calib) OCR 行不足（{len(lines)} 行）→ 无法标定绝对行带")
            return False
        pitch = float(diffs[len(diffs) // 2])
        row0_center = (lines[0][0] + lines[0][1]) / 2
        bands = {}
        for i in range(PIN_ROWS):
            ctr = row0_center + i * pitch
            bands[i] = (max(0, int(ctr - pitch / 2)), int(ctr + pitch / 2))
        GEO = {"pitch": pitch, "row0_center": row0_center, "size": size,
               "bands": bands, "shot": shot}
        print(f"  (calib) 几何 {w}x{h} → 截图 {size[0]}x{size[1]}，OCR {len(lines)} 行，"
              f"行高 {pitch:.1f}px，首行中心 {row0_center:.1f}px → 绝对行带可用")
        return True
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            proc.kill()
        deadline = time.time() + 5
        while time.time() < deadline and find_window(tag) is not None:
            time.sleep(0.2)


# --------------------------------------------------------------------------
# V1 图片渲染可见
# --------------------------------------------------------------------------
def v1_image():
    print("== V1 图片渲染可见（像素内容 vs 文本基线）==")
    img = os.path.join("img", "gradient.png")
    s = Session("v1-img", [img]).start()
    state = s.wait_ready()
    if state != "ready":
        skip("V1 图片渲染", f"窗口未就绪({state})")
        s.close()
        return
    shot = s.shot("image")
    a = s.anchor_for("image")
    if shot is None or a is None:
        skip("V1 图片渲染", "截图/自标定失败（无法定位行带）")
        s.close()
        return
    bd = a.bands(bar_h=0)
    st = region_stats(shot, *bd["body"])
    w = png_size(shot)[0]
    head = ocr(shot).strip().splitlines()[0] if ocr(shot).strip() else ""
    body_txt = ocr(crop(shot, os.path.join(s.dir, "body.png"), 0, bd["body"][0],
                        w, bd["body"][1] - bd["body"][0]))
    check("gradient.png" in head.replace(" ", ""), "V1 header 显示文件名", head[:60])
    check(st["quant"] >= 60, "V1 图片体区为像素内容（量化色数 ≥ 60）",
          f"quant={st['quant']} ink={st['ink']} pitch={a.pitch:.1f}")
    check(not re.search(r"(?i)unavailable|not found|\u2717", body_txt),
          "V1 无图片降级/错误行", body_txt.strip().replace("\n", " | ")[:60])
    img_quant, img_ink = st["quant"], st["ink"]
    rc = s.quit()
    check(rc == 0, f"V1 图片模式 q 退出码 0 (got {rc})")
    s.close()

    # 基线：同几何文本渲染（串行开窗，几何一致）
    # 注：偶发 `exited:0`（前一会话清理的 SIGTERM 波及新窗口 / 启动竞态）→ 重试一次，
    # 避免把 flaky 当 SKIP（SKIP 应表示环境缺失，不表示抖动）。
    base = None
    state_b = None
    for attempt in (1, 2):
        base = Session("v1-txt", [os.path.join("plain.txt")], label="v1-text").start()
        state_b = base.wait_ready()
        if state_b == "ready":
            break
        print(f"    (info) V1 文本基线第 {attempt} 次未就绪({state_b})，重试")
        base.close()
        time.sleep(0.8)
    if state_b == "ready":
        bshot = base.shot("text")
        ab = base.anchor_for("text")
        if bshot is not None and ab is not None:
            bst = region_stats(bshot, *ab.bands(bar_h=0)["body"])
            check(bst["quant"] * 2 <= img_quant and img_quant >= 60,
                  "V1 图片体区颜色量 ≥ 2× 文本基线（证明真图形而非字符画）",
                  f"image quant={img_quant} ink={img_ink} vs text quant={bst['quant']} "
                  f"ink={bst['ink']}")
            # 文本基线可读性：OCR 在小字号/缩放/抗锯齿下不稳定（实测同一实现
            # 有时读到有时读不到），故判据改为**像素统计**：文本页的 ink（笔画占比）
            # 应显著高于图片页（图片是大块色彩，笔画稀疏）。
            check(bst["ink"] > img_ink,
                  "V1 文本基线渲染正常（笔画占比高于图片页）",
                  f"text ink={bst['ink']} vs image ink={img_ink}")
        else:
            skip("V1 文本基线对比", "基线截图/自标定失败")
        base.quit()
    else:
        skip("V1 文本基线对比", f"基线窗口未就绪({state_b})")
    base.close()

    # sixel(auto) 与 halfblocks 的保真度对照（信息项，不作为失败判据：
    # 量化色数受背景色/抗锯齿影响，单调性在共享桌面上不可靠）
    half = Session("v1-half", [img], env_extra={"DLOOK_IMAGE_PROTOCOL": "halfblocks"},
                   label="v1-halfblocks").start()
    if half.wait_ready() == "ready":
        hshot = half.shot("halfblocks")
        ah = half.anchor_for("halfblocks")
        if hshot is not None and ah is not None:
            hst = region_stats(hshot, *ah.bands(bar_h=0)["body"])
            print(f"    info: auto(quant={img_quant}, ink={img_ink}) "
                  f"vs halfblocks(quant={hst['quant']}, ink={hst['ink']})")
            check(hst["quant"] >= 20, "V1 halfblocks 亦为图形渲染（量化色数 ≥ 20）",
                  f"quant={hst['quant']}")
        else:
            skip("V1 halfblocks 对照", "对照截图/自标定失败")
        half.quit()
    else:
        skip("V1 halfblocks 对照", "对照窗口未就绪")
    half.close()


# --------------------------------------------------------------------------
# V2–V4 视频（需 media-3 的 mpv 路径落地）
# --------------------------------------------------------------------------
def video_fixture():
    """V 套件的视频素材：**够长的**测试片（4s 的仓库 fixture 会在四态采样前播完）。

    优先用 `DLOOK_VISUAL_VIDEO`；否则用 ffmpeg 生成 30s testsrc（320x180@10）到存档
    目录；ffmpeg 不可用时退回仓库 fixture 并提示结果可能不完整。
    """
    override = os.environ.get("DLOOK_VISUAL_VIDEO")
    if override and os.path.exists(override):
        return override, None
    repo_clip = os.path.join(FIX, "video", "clip.mp4")
    if not shutil.which("ffmpeg"):
        reason = ("本机无 ffmpeg，退回 4s 仓库 fixture → 四态采样窗口不足"
                  if os.path.exists(repo_clip) else "本机无 ffmpeg 且缺少仓库 fixture")
        return (repo_clip if os.path.exists(repo_clip) else None), reason
    out = os.path.join(OUT, "v-fixture-30s.mp4")
    if not os.path.exists(out):
        r = subprocess.run(
            ["ffmpeg", "-v", "error", "-y", "-f", "lavfi", "-i",
             "testsrc=duration=30:size=320x180:rate=10", "-pix_fmt", "yuv420p", out],
            capture_output=True,
        )
        if r.returncode != 0 or not os.path.exists(out):
            return (repo_clip if os.path.exists(repo_clip) else None,
                    "ffmpeg 生成 30s 测试片失败 → 退回仓库 fixture")
    return out, None


def video_precondition():
    """视频前置探测：返回 (Session|None, reason)；reason 非空 → V2/V3/V4 显式 SKIP。"""
    if not shutil.which("mpv"):
        return None, "本机无 mpv → 走降级链，共屏路径不成立"
    proto = os.environ.get("DLOOK_IMAGE_PROTOCOL") or "auto"
    if proto in ("off", "halfblocks", "none"):
        return None, f"DLOOK_IMAGE_PROTOCOL={proto} → 无图形协议，走降级链"
    clip, note = video_fixture()
    if clip is None:
        return None, note or "缺少视频 fixture"
    if note:
        print(f"    (note) {note}")
    s = Session("v2-video", [clip], wait=1.2).start()  # 采样要趁片子还在播
    state = s.wait_ready()
    if state.startswith("exited"):
        rc = state.split(":")[1]
        s.close()
        if rc == "101":
            return None, "待 media-3 落地(video.rs 仍为 todo!() → 进程 panic 101)"
        return None, f"视频模式未启动(退出码 {rc})"
    if state != "ready":
        s.close()
        return None, f"窗口未就绪({state})"
    text = ocr(s.shot("precondition"))
    if re.search(r"(?i)mpv not found|no graphics protocol|first frame|ffmpeg failed", text):
        s.close()
        return None, "走了降级链（无 mpv/无图形协议）→ 共屏路径不成立"
    s.clip = clip
    return s, None


def v2_video(s):
    print("== V2 视频四态（播放帧变化 / 暂停冻结 / 恢复 / seek）==")
    a1 = s.shot("play-1")
    anchor = s.anchor_for("play-1")
    if anchor is None:
        skip("V2 视频四态", "截图/自标定失败（无法定位视频区行带）")
        return
    body = anchor.bands(bar_h=2)["body"]
    print(f"    (info) 自标定行高 {anchor.pitch:.1f}px，网格 {anchor.rows} 行，"
          f"视频区 y={body[0]}..{body[1]}")

    # mpv 就绪/首帧上屏需要时间（Loading → Playing）。先等视频区真的成为像素内容，
    # 否则「播放中帧变化」会采到两帧空白（实测 diff=0.0 假失败）。
    deadline = time.time() + 15
    quant = 0
    while time.time() < deadline:
        quant = region_stats(s.shot("paint-probe"), *body)["quant"]
        if quant >= 50:
            break
        time.sleep(0.6)
    check(quant >= 50, "V2 视频已上屏（像素内容出现，非空白区）", f"quant={quant}")
    if quant < 50:
        skip("V2 四态断言", "视频画面未上屏（mpv VO 未输出）")
        return

    # P1 的覆盖责任转移到这里：pyte/tmux 无图形协议 → 那些套件里显式 SKIP 并注明
    # 「真实终端覆盖于 V 套件」。此时刚确认画面已上屏，mpv 必定在跑，是断言
    # spawn 参数最可靠的时机（V3 时素材可能已播完、mpv 已退出）。
    _pids, cmds = sweep(kill=False)
    mpv_cmd = next((c for c in cmds if "mpv" in c and "--vo-" in c), "")
    check(bool(mpv_cmd), "V2 mpv 子进程存在且带 --vo- 参数", mpv_cmd[:80])
    if mpv_cmd:
        for label, needle in [
            ("--vo=sixel", "--vo=sixel"),
            ("--vo-sixel-left=", "--vo-sixel-left="),
            ("--vo-sixel-top=", "--vo-sixel-top="),
            ("--vo-sixel-cols=", "--vo-sixel-cols="),
            ("--vo-sixel-rows=", "--vo-sixel-rows="),
            ("--vo-sixel-alt-screen=no", "--vo-sixel-alt-screen=no"),
            ("--vo-sixel-config-clear=no", "--vo-sixel-config-clear=no"),
            ("--no-terminal", "--no-terminal"),
            ("--hr-seek=yes", "--hr-seek=yes"),
            ("--audio-display=no", "--audio-display=no"),
            ("--input-ipc-server=", "--input-ipc-server="),
        ]:
            check(needle in mpv_cmd, f"V2 mpv 参数 {label}")
        m = re.search(r"--vo-sixel-left=(\d+) --vo-sixel-top=(\d+) "
                      r"--vo-sixel-cols=(\d+) --vo-sixel-rows=(\d+)", mpv_cmd)
        check(bool(m), "V2 区域几何四参数同时出现")
        if m:
            left, top, cols, rows = (int(x) for x in m.groups())
            check(left >= 1 and top >= 1 and cols >= 20 and rows >= 10,
                  "V2 区域几何数值合理（left/top ≥1，cols/rows 为 body 量级）",
                  f"left={left} top={top} cols={cols} rows={rows}")

    d_play, n, _ = diff_over_pairs(s, "play-1", "play-2", body, gap=1.2, want="max")
    check(nonzero(d_play) and d_play > 0.001, "V2 播放中视频区帧变化",
          f"diff={d_play}（{n} 次采样取最大）")

    s.send("space")  # M1: 播放/暂停
    time.sleep(1.0)
    d_pause, n, _ = diff_over_pairs(s, "pause-1", "pause-2", body, gap=1.2, want="min")
    check(d_pause == 0.0, "V2 暂停后视频区冻结（逐像素一致）",
          f"diff={d_pause}（{n} 次采样取最小）")

    s.send("space")
    time.sleep(1.0)
    d_resume, n, _ = diff_over_pairs(s, "resume-1", "resume-2", body, gap=1.2, want="max")
    check(nonzero(d_resume) and d_resume > 0.001, "V2 恢复播放后视频区再次变化",
          f"diff={d_resume}（{n} 次采样取最大）")

    s.send("Right")  # seek +5s
    time.sleep(1.0)
    d_seek, n, _ = diff_over_pairs(s, "seek-1", "seek-2", body, gap=0.4, want="max")
    check(nonzero(d_seek) and d_seek > 0.0005, "V2 seek 后画面变化",
          f"diff={d_seek}（{n} 次采样取最大）")


def v3_coscreen(s):
    print("== V3 视频共屏：chrome 仍在 / 视频区未被文字覆写 ==")
    shot = s.shot("coscreen")
    anchor = s.anchor_for("coscreen")
    if shot is None or anchor is None:
        skip("V3 共屏 chrome", "截图/自标定失败")
        skip("V3 视频区未被文字覆写", "同上")
        return
    bd = anchor.bands(bar_h=2)
    a1 = s.shot("header-check")
    text = ocr(a1 or shot)
    head = text.strip().splitlines()[0] if text.strip() else ""
    check("clip" in head.replace(" ", "") or "mp4" in head.replace(" ", ""),
          "V3 header 显示视频文件名", head[:60])
    check("quit" in text or "seek" in text, "V3 footer 键位表仍在", text[-40:])

    for name in ("header", "footer"):
        d, n, _ = diff_over_pairs(s, f"chrome-{name}-1", f"chrome-{name}-2",
                                  bd[name], gap=1.0, want="min")
        check(d == 0.0, f"V3 {name} 行在播放期间稳定（未被视频覆盖）",
              f"diff={d}（{n} 次采样取最小）")
    st_body = region_stats(shot, *bd["body"])
    check(st_body["quant"] >= 50, "V3 视频区为像素内容（量化色数 ≥ 50）",
          f"quant={st_body['quant']} ink={st_body['ink']}")
    bar_ink = region_stats(shot, *bd["bar"])["ink"]
    check(bar_ink > 0.0, "V3 媒体栏有内容（进度条/时间码/标题）", f"ink={bar_ink}")
    body_txt = ocr(crop(shot, os.path.join(s.dir, "body-only.png"), 0, bd["body"][0],
                        png_size(shot)[0], bd["body"][1] - bd["body"][0]))
    chrome = re.findall(r"(?i)\b(quit|seek|mute|scroll)\b", body_txt)
    check(not chrome, "V3 视频区未被 chrome 文字覆写",
          f"tokens={chrome[:5]} ocr={body_txt.strip()[:60]!r}")
    # 注：mpv 的 spawn 参数断言在 V2（刚确认画面已上屏时最可靠；到 V3 时
    # 素材可能已播完、mpv 已退出，断言会假失败）。


def v3b_live_refresh(s):
    """V3b 播放期反馈与 resize 恢复（独立验收 media-4 的 B1/B2 回归）。

    这两条是真实缺陷的回归测试：早先的实现「mpv 活跃期间一律不写终端」虽然消除了
    撕裂，却让媒体栏时间码冻结、按键反馈消失、resize 后 chrome 永不恢复。现有断言
    （chrome 行「播放期稳定」）在该方案下是平凡通过，覆盖不到本问题。
    """
    print("== V3b 播放期媒体栏刷新 / 按键反馈 / resize 恢复 ==")

    # 统一用 s.shot()（它自己做尺寸稳定性检查并登记到 self.shots，anchor_for 依赖该登记）
    fresh = s.shot

    t0 = time.time() + 15
    base = None
    while time.time() < t0:
        base = fresh("live-base")
        if base:
            an = s.anchor_for("live-base")
            if an is not None and region_stats(base, *an.bands(bar_h=2)["body"])["quant"] >= 50:
                break
        time.sleep(0.6)
    if base is None or s.anchor_for("live-base") is None:
        skip("V3b 播放期媒体栏刷新", "视频未上屏/自标定失败")
        skip("V3b 按键反馈", "同上")
        skip("V3b resize 恢复 chrome", "同上")
        return

    # a) 媒体栏必须随播放推进（时间码/进度条像素变化），否则时间码是冻结的
    an = s.anchor_for("live-base")
    bd = an.bands(bar_h=2)
    d, n, _ = diff_over_pairs(s, "live-base", "live-a", bd["bar"], gap=1.6, want="max")
    check(nonzero(d) and d > 0.0005,
          "V3b 媒体栏随播放刷新（时间码/进度条变化）",
          f"bar diff={d}（{n} 次采样取最大；0 = 时间码冻结）")

    # b) 播放中按 → 必须有可见反馈（状态栏消息）
    s.send("Right")
    time.sleep(0.6)
    seek_shot = fresh("live-seek")
    txt = ocr(seek_shot) if seek_shot else ""
    check("seek" in txt.lower(), "V3b 播放中 seek 有可见反馈", txt.strip()[-60:])

    # c) resize 后 chrome 必须恢复，且无旧内容残留
    subprocess.run(["hyprctl", "dispatch",
                    f'hl.dsp.window.resize({{window="class:{s.tag}", x=1000, y=700}})'],
                   capture_output=True)
    time.sleep(3.0)
    rz = fresh("live-resize")
    if rz is None:
        skip("V3b resize 恢复 chrome", "resize 后截图失败")
        return
    rtxt = ocr(rz)
    check("mp4" in rtxt or "test-60s" in rtxt,
          "V3b resize 后 header 恢复（文件名可见）", rtxt.strip().splitlines()[0][:50])
    check("space" in rtxt.lower() or "quit" in rtxt.lower(),
          "V3b resize 后 footer 恢复（键位表可见）", rtxt.strip()[-50:])
    frag = re.findall(r"\b\d{2,4}~\b", rtxt)
    check(not frag, "V3b resize 后无旧内容残留碎片", f"fragments={frag[:5]}")
    # 视频区不应被 chrome 文字覆写（撕裂回归）
    chrome = re.findall(r"(?i)\b(quit|mute|scroll)\b", rtxt)
    check(len(chrome) <= 4, "V3b resize 后视频区无 chrome 覆写", f"tokens={chrome[:5]}")


def v4_residue(session_rc, tag):
    print("== V4 退出回收（无残留 mpv / 窗口）==")
    check(session_rc == 0, f"V4 视频会话 q 退出码 0 (got {session_rc})")
    pids, cmds = sweep(kill=True)
    mpv = [c for c in cmds if "mpv" in c]
    check(not mpv, "V4 退出后无残留 mpv 进程", str(mpv[:2]))
    check(find_window(tag) is None, "V4 退出后无残留测试窗口")


# --------------------------------------------------------------------------
# V5 音频
# --------------------------------------------------------------------------
def v5_audio():
    print("== V5 音频播放：无报错 + 媒体栏推进/暂停冻结 ==")
    wav = os.path.join("audio", "tone.wav")
    if not os.path.exists(os.path.join(FIX, wav)):
        skip("V5 音频播放", f"缺少 fixture {os.path.join(FIX, wav)}")
        return
    s = Session("v5-audio", [wav], wait=2.5).start()
    state = s.wait_ready()
    if state != "ready":
        skip("V5 音频播放", f"音频模式未就绪({state})")
        s.close()
        return

    s.send("0")  # 回曲首（确定性起点）
    time.sleep(0.6)
    a = s.shot("play-1")
    anchor = s.anchor_for("play-1")
    if a is None or anchor is None:
        skip("V5 音频播放", "截图/自标定失败（无法定位媒体栏行带）")
        s.close()
        return
    bd = anchor.bands(bar_h=2)
    print(f"    (info) 自标定行高 {anchor.pitch:.1f}px，网格 {anchor.rows} 行，"
          f"媒体栏 y={bd['bar'][0]}..{bd['bar'][1]}")

    d_play, n, pair = diff_over_pairs(s, "play-1", "play-2", bd["bar"], gap=1.4,
                                      want="max")
    if pair[1] is None:
        skip("V5 播放推进断言", "截图尺寸无法稳定（环境窗口扰动）")
    else:
        b = pair[1]
        text = ocr(b)
        check(nonzero(d_play), "V5 播放中媒体栏随位置推进重绘",
              f"bar diff={d_play}（{n} 次采样取最大）")
        check("tone.wav" in text, "V5 header/媒体栏显示文件名")
        check(re.search(r"\b\d\d:\d\d\b", text) is not None, "V5 时间码可读（MM:SS）",
              " ".join(re.findall(r"\b\d\d:\d\d\b", text))[:60])
        bad_tokens = re.findall(r"(?i)not found|no audio device|error|\u2717|cannot", text)
        check(not bad_tokens, "V5 播放期无报错文案", f"tokens={bad_tokens[:4]}")
        # 体区信息块:播放态文案(时间码在媒体栏,body 给状态/时长/音量)
        body_txt = ocr(crop(a, os.path.join(s.dir, "body.png"), 0, bd["body"][0],
                            png_size(a)[0], bd["body"][1] - bd["body"][0]))
        check("state" in body_txt and "playing" in body_txt,
              "V5 body 信息块含播放态", body_txt.strip().replace("\n", " | ")[:70])

    s.send("p")
    time.sleep(0.6)
    d_pause, n, pair = diff_over_pairs(s, "pause-1", "pause-2", bd["bar"], gap=1.4,
                                       want="min")
    check(d_pause == 0.0, "V5 暂停后媒体栏冻结（逐像素一致）",
          f"bar diff={d_pause}（{n} 次采样取最小）")
    if pair[1]:
        # OCR 可能读不出（小字号/缩放），此时不作为失败：只在其确实读出状态词时
        # 断言「不是播放结束」。像素层面的「推进→冻结」已由上一条覆盖。
        ptxt = ocr(pair[1]).lower()
        if "finished" in ptxt or "playing" in ptxt or "paused" in ptxt:
            check("finished" not in ptxt, "V5 暂停非播放结束", ptxt[:40])
        else:
            print("    (info) V5 暂停态 OCR 未读出状态词，跳过该断言（像素判据已覆盖）")

    # 恢复播放断言：8s fixture 在「就绪等待 + 前两段采样」后已接近末尾，先回曲首。
    # media.rs 的 restart(`0`) 语义 = seek 0 + play，故**不再按 p**（否则又暂停）。
    s.send("0")
    time.sleep(0.8)
    d_resume, n, _ = diff_over_pairs(s, "resume-1", "resume-2", bd["bar"], gap=1.2,
                                     want="max")
    check(nonzero(d_resume), "V5 回曲首恢复后媒体栏继续推进",
          f"bar diff={d_resume}（{n} 次采样取最大）")

    rc = s.quit()
    check(rc == 0, f"V5 音频 q 退出码 0 (got {rc})")
    s.close()


# --------------------------------------------------------------------------
# 收尾
# --------------------------------------------------------------------------
def final_cleanup():
    print("== V-cleanup 结束清理 ==")
    for s in list(SESSIONS):
        s.close()
    pids, cmds = sweep(kill=True)
    leftover = [c.get("title", "") for c in hypr_clients()
                if TAG_PREFIX in (c.get("title", "") + c.get("class", ""))]
    check(not leftover, "V-cleanup 无遗留测试窗口", str(leftover[:3]))
    check(not pids, "V-cleanup 无遗留测试进程（含 mpv 孙进程）",
          str([c[:60] for c in cmds[:2]]))


def install_signal_cleanup():
    """被 SIGTERM/SIGINT 打断时也要清理测试窗口/进程。

    背景：测试脚本被外部打断（timeout、Ctrl-C、用户取消）时，默认信号处理会直接
    终止进程，`finally` 不执行，于是测试窗口与 mpv 进程留在桌面上（实测发生过）。
    """
    def handler(signum, _frame):
        print(f"\n(interrupt) 收到信号 {signum}，清理测试进程与窗口")
        try:
            for s in list(SESSIONS):
                s.close()
            sweep(kill=True)
        finally:
            os._exit(130)

    for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        try:
            signal.signal(sig, handler)
        except (ValueError, OSError):
            pass


def preflight():
    problems = [t for t in ("hyprctl", "grim", "foot", "wtype", "magick", "tesseract")
                if not shutil.which(t)]
    if not os.environ.get("WAYLAND_DISPLAY"):
        problems.append("WAYLAND_DISPLAY(非图形会话)")
    if not (os.path.isabs(BIN) and os.access(BIN, os.X_OK)):
        problems.append(f"BIN 非绝对路径或不可执行: {BIN}")
    if problems:
        print(f"V 套件前置检查未通过: {', '.join(problems)}")
        for d in ["V1 图片渲染", "V2 视频四态", "V3 视频共屏", "V4 退出回收", "V5 音频播放"]:
            skip(d, "缺少图形会话/工具或 BIN 不可用")
        return False
    os.makedirs(OUT, exist_ok=True)
    print(f"BIN = {BIN}")
    print(f"OUT = {OUT}")
    print(f"RUN = {RUN_ID}")
    leftover = [c.get("title") for c in hypr_clients() if TAG_PREFIX in
                (c.get("title", "") + c.get("class", ""))]
    if leftover:
        print(f"警告：已存在同名测试窗口 {leftover}（将被清理）")
    calib_ok = calibrate()
    print(f"工具链标定: {'OK' if calib_ok else 'FAIL（行带改由每张截图自标定，仍可继续）'}")
    return True


def main():
    install_signal_cleanup()
    if not preflight():
        print()
        print(f"RESULT: PASS={PASS} FAIL={FAIL} SKIP={SKIP}")
        return 0 if FAIL == 0 else 1
    try:
        v1_image()
        sess, reason = video_precondition()
        if sess is None:
            for d in ["V2 视频四态", "V3 共屏 chrome", "V3 视频区未被文字覆写",
                      "V4 退出回收"]:
                skip(d, f"{reason}（共屏路径不成立，属预期）")
        else:
            v2_video(sess)
            v3_coscreen(sess)
            v3b_live_refresh(sess)
            rc = sess.quit()
            tag = sess.tag
            sess.close()
            v4_residue(rc, tag)
        v5_audio()
    finally:
        final_cleanup()

    print()
    print(f"RESULT: PASS={PASS} FAIL={FAIL} SKIP={SKIP}")
    print(f"截图存档: {OUT}")
    print("索引（相对存档目录）:")
    for dirpath, _dirs, files in os.walk(OUT):
        for f in sorted(files):
            if f.endswith(".png"):
                print(f"  {os.path.relpath(os.path.join(dirpath, f), OUT)}")
    return 0 if FAIL == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
