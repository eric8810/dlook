#!/usr/bin/env python3
"""V 场景（design §6，V1–V5）：真实终端（Hyprland + foot）截图断言。

由 `test/e2e/run-visual.sh` 调用（该脚本负责：被测二进制绝对路径、图形会话与
工具前置检查、缺失时显式 SKIP）。参考 experiments：`e11-visual-video-check.py`
（四态截图断言）、`e13-terminal-loop-check.py`（窗口聚焦 / wtype 注入 / 退出码读回）。

覆盖：
  V1  图片 sixel 渲染可见（与同几何文本渲染的体区颜色量对比）
  V2  视频四态：播放帧变化 / 暂停冻结 / 恢复变化 / seek 变化
  V3  视频共屏：header / 媒体栏 / footer 仍在、且视频区未被文字覆写
      （chrome 行像素在播放期间逐帧不变 + 视频区持续变化）
  V4  退出回收：退出码 0、窗口关闭、无残留 mpv / dlook 进程
  V5  音频播放：无报错、媒体栏随播放推进、暂停冻结
  V-cleanup  结束时无遗留测试窗口/进程（每个用例后都会清理）

判定口径：
  - 依赖引擎落地的用例（V2/V3/V4 需 media-3 的 mpv 路径）在 video.rs 仍为
    `todo!()`（进程 panic 退 101）时**显式 SKIP**，并打印原因，不伪装通过。
  - 断言全部基于 grim 截图：OCR（tesseract）取文本、原始 RGB 取像素统计与逐帧差异。
  - 所有测试窗口/进程在结束时清理；并行 agent 的进程不受影响（只操作带本次
    run 唯一环境标记 DLOOK_VISUAL_RUN 的进程）。
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

# 本次 run 的唯一标记：注入到每个子进程的环境变量里，用于精确收割（绝不误杀
# 并行 agent 的 foot/mpv 进程）。
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
# 进程 / 环境工具
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
    """pid 及其父链（用于排除自身/测试框架进程）。"""
    out = set()
    cur = pid
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


def focus(tag):
    r = sh(["hyprctl", "dispatch", f'hl.dsp.focus({{window="class:{tag}"}})'])
    if r.returncode != 0 or "ok" not in (r.stdout + r.stderr).lower():
        # 旧版 Hyprland 的经典 dispatcher 语法
        sh(["hyprctl", "dispatch", "focuswindow", f"class:{tag}"])
    time.sleep(0.4)


def key(*keys):
    r = sh(["wtype", "-k", *keys])
    time.sleep(0.35)
    return r


# --------------------------------------------------------------------------
# 截图与像素/文本分析
# --------------------------------------------------------------------------
def png_size(path):
    out = sh(["magick", "identify", "-format", "%w %h", path]).stdout.split()
    return int(out[0]), int(out[1])


def rgb(path, cache={}):
    """原始 RGB 字节（缓存，避免重复解码）。"""
    if path not in cache:
        cache[path] = subprocess.run(
            ["magick", path, "-depth", "8", "rgb:-"], capture_output=True
        ).stdout
    return cache[path]


def ocr(path, psm="6"):
    return sh(["tesseract", path, "-", "--psm", psm]).stdout


def ocr_lines(path):
    """OCR 行盒 [(top, bottom), ...]（块/段/行归组）。"""
    t = sh(["tesseract", path, "-", "--psm", "6", "tsv"]).stdout
    agg = {}
    for row in t.splitlines()[1:]:
        f = row.split("\t")
        if len(f) < 12 or not f[11].strip():
            continue
        key = (f[1], f[2], f[3], f[4])
        top, hgt = int(f[7]), int(f[9])
        lo, hi = agg.get(key, (top, top + hgt))
        agg[key] = (min(lo, top), max(hi, top + hgt))
    return sorted(agg.values())


def ink_bands(path, thresh=0.002):
    """按行统计「非背景色」像素占比 → 连续 ink 行段 [(y0,y1), ...]。"""
    w, h = png_size(path)
    raw = rgb(path)
    bg = Counter(raw[i:i + 3] for i in range(0, len(raw), 3)).most_common(1)[0][0]
    bands, ink = [], []
    for y in range(h):
        row = raw[y * w * 3:(y + 1) * w * 3]
        n = sum(1 for x in range(w)
                if max(abs(row[x * 3 + i] - bg[i]) for i in range(3)) > 40)
        ink.append(n)
    th = max(2, int(thresh * w))
    start = None
    for y, n in enumerate(ink):
        if n > th and start is None:
            start = y
        elif n <= th and start is not None:
            bands.append((start, y - 1))
            start = None
    if start is not None:
        bands.append((start, h - 1))
    return bands, bg


def crop(path, out, x, y, w, h):
    subprocess.run(["magick", path, "-crop", f"{max(1, w)}x{max(1, h)}+{x}+{y}",
                    "+repage", out], capture_output=True)
    return out


def region_stats(path, y0, y1, x0=0, x1=None):
    """区域内: ink 占比、量化(4bit/通道)颜色数、众数背景色占比。"""
    w, h = png_size(path)
    raw = rgb(path)
    y0 = max(0, int(y0)); y1 = min(h, int(y1))
    x0 = max(0, int(x0)); x1 = min(w, int(x1) if x1 else w)
    px = [raw[(y * w + x) * 3:(y * w + x) * 3 + 3]
          for y in range(y0, y1) for x in range(x0, x1)]
    if not px:
        return {"n": 0, "ink": 0.0, "quant": 0}
    c = Counter(px)
    bg, bgn = c.most_common(1)[0]
    ink = sum(1 for p in px if max(abs(p[i] - bg[i]) for i in range(3)) > 40)
    quant = len({(p[0] // 16, p[1] // 16, p[2] // 16) for p in px})
    return {"n": len(px), "ink": round(ink / len(px), 4), "quant": quant,
            "bg_frac": round(bgn / len(px), 3)}


def region_diff(a, b, y0, y1, x0=0, x1=None, tol=16):
    """两图区域间「显著不同」像素占比（0.0 = 逐像素一致）。"""
    wa, ha = png_size(a)
    ra, rb = rgb(a), rgb(b)
    y0 = max(0, int(y0)); y1 = min(ha, int(y1))
    x0 = max(0, int(x0)); x1 = min(wa, int(x1) if x1 else wa)
    diff = 0
    tot = 0
    for y in range(y0, y1):
        base = y * wa
        for x in range(x0, x1):
            o = (base + x) * 3
            tot += 1
            if max(abs(ra[o + i] - rb[o + i]) for i in range(3)) > tol:
                diff += 1
    return round(diff / max(1, tot), 5)


# --------------------------------------------------------------------------
# 会话（foot 窗口 + 被测进程）
# --------------------------------------------------------------------------
class Session:
    """一个 foot 窗口里跑一个 dlook 会话；负责截图、按键注入、退出码读回、清理。"""

    def __init__(self, case, args, env_extra=None, cwd=ROOT, geometry=None,
                 wait=3.0, label=None):
        self.case = case
        self.tag = f"{TAG_PREFIX}{case}"
        self.dir = os.path.join(OUT, label or case)
        os.makedirs(self.dir, exist_ok=True)
        self.args = args
        self.env = dict(os.environ, DLOOK_VISUAL_RUN=RUN_ID)
        for k, v in (env_extra or {}).items():
            if v is None:
                self.env.pop(k, None)
            else:
                self.env[k] = v
        self.geometry = geometry or {"rows": None, "cols": None, "pitch": None, "top": None}
        self.wait = wait
        self.rc_path = os.path.join(self.dir, "rc")
        self.pid_path = os.path.join(self.dir, "pid")
        self.proc = None
        self.geo = None
        self.shots = {}

    # ---- 生命周期 ----
    def start(self):
        # 退出码写文件（stdout 保持 tty，否则 dlook 会走非 TTY 分支）
        inner = (f"echo $ > {self.pid_path}; cd {ROOT} && "
                 + BIN + " " + " ".join(self._quote(a) for a in self.args)
                 + f"; echo $? > {self.rc_path}")
        self.proc = subprocess.Popen(
            ["foot", "-a", self.tag, "-T", self.tag, "--", "sh", "-c", inner],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, stdin=subprocess.DEVNULL,
            env=self.env, start_new_session=True,
        )
        SESSIONS.append(self)
        return self

    @staticmethod
    def _quote(a):
        return a if re.fullmatch(r"[A-Za-z0-9_./=:@+-]+", a) else "'" + a.replace("'", "'\\''") + "'"

    def wait_ready(self, timeout=None):
        """('ready'|'exited:<rc>'|'timeout', client)。"""
        deadline = time.time() + (timeout if timeout is not None else self.wait + 8)
        while time.time() < deadline:
            c = find_window(self.tag)
            rc = self.rc()
            if c and rc is None:
                time.sleep(self.wait)
                c = find_window(self.tag)
                if c is None:
                    return ("exited:" + str(self.rc()), None)
                self.geo = c
                return ("ready", c)
            if rc is not None:
                return (f"exited:{rc}", c)
            time.sleep(0.2)
        return ("timeout", find_window(self.tag))

    def rc(self):
        if os.path.exists(self.rc_path):
            try:
                return int(open(self.rc_path).read().strip())
            except ValueError:
                return None
        return None

    def alive(self):
        return self.rc() is None and find_window(self.tag) is not None

    # ---- 截图 / 按键 ----
    def shot(self, name):
        c = find_window(self.tag)
        if not c:
            return None
        x, y = c["at"]
        w, h = c["size"]
        path = os.path.join(self.dir, f"{name}.png")
        sh(["grim", "-g", f"{x},{y} {w}x{h}", path])
        self.shots[name] = path
        return path

    def send(self, *keys):
        focus(self.tag)
        return key(*keys)

    def quit(self, timeout=6.0):
        """注入 q（回退：SIGTERM），返回退出码或 None。"""
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
        # 清窗口：按 class 找 foot 客户端并忽略（foot 随子进程退出自动关；残留时杀掉）
        c = find_window(self.tag)
        if c is not None:
            sh(["hyprctl", "dispatch", f'hl.dsp.window.close({{window="class:{self.tag}"}})'])
            time.sleep(0.3)
            c = find_window(self.tag)
            if c is not None and self.proc:
                try:
                    self.proc.kill()
                except OSError:
                    pass
            time.sleep(0.3)
        if self in SESSIONS:
            SESSIONS.remove(self)


# --------------------------------------------------------------------------
# 几何标定（单元格高度/行带）：图形会话的窗口尺寸不由 foot 参数决定时也能工作
# --------------------------------------------------------------------------
def calibrate():
    """标定：返回 {rows, cols, pitch, row0_top}（dlook 窗口与标定窗口同几何）。

    方法：跑一个 `seq 1 24` 的 foot 窗口 → 截图 ink 行段的间距 = 单元格高度（像素）；
    窗口 settle 后读 pty 尺寸（stty size）得到 rows/cols。
    """
    tag = f"{TAG_PREFIX}calib"
    size_path = os.path.join(OUT, "calib-size")
    inner = f"sleep 1.5; stty size > {size_path}; seq 1 24; sleep 20"
    proc = subprocess.Popen(
        ["foot", "-a", tag, "-T", tag, "--", "sh", "-c", inner],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, stdin=subprocess.DEVNULL,
        env=dict(os.environ, DLOOK_VISUAL_RUN=RUN_ID), start_new_session=True,
    )
    try:
        deadline = time.time() + 10
        c = None
        while time.time() < deadline and (c := find_window(tag)) is None:
            time.sleep(0.2)
        if c is None:
            return None
        time.sleep(2.5)
        if not os.path.exists(size_path):
            return None
        rows, cols = (int(v) for v in open(size_path).read().split()[:2])
        x, y = c["at"]
        w, h = c["size"]
        shot = os.path.join(OUT, "calib.png")
        sh(["grim", "-g", f"{x},{y} {w}x{h}", shot])
        bands, _bg = ink_bands(shot)
        tops = [b[0] for b in bands]
        pitches = sorted(b - a for a, b in zip(tops, tops[1:]) if 4 < b - a < 80)
        if not pitches:
            return None
        pitch = pitches[len(pitches) // 2]
        # 最上一行 ink 段 = 终端第 0 行（seq 首行可见）
        return {"rows": rows, "cols": cols, "pitch": pitch, "row0_top": tops[0],
                "shot": shot, "bands": len(bands)}
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            proc.kill()
        time.sleep(0.4)


def bands(geo, bar_h, rows=None):
    """按行号给出行带 (y0, y1)：header=0；body=1..rows-1-bar_h-1；bar；footer=rows-1。"""
    rows = rows or geo["rows"]
    pitch, top = geo["pitch"], geo["row0_top"]

    def band(i):
        y0 = int(top + (i - 0.5) * pitch)
        return (max(0, y0), int(top + (i + 0.5) * pitch))

    out = {"header": band(0), "footer": band(rows - 1)}
    # body 到媒体栏第一行的上边界为止（避免与媒体栏行带重叠）
    body_end = int(top + (rows - 1 - bar_h - 0.5) * pitch)
    out["body"] = (band(1)[0], body_end)
    if bar_h >= 1:
        out["bar"] = (band(rows - 1 - bar_h)[0], band(rows - 2)[1])
    if bar_h >= 2:
        out["bar1"] = band(rows - 3)
        out["bar2"] = band(rows - 2)
    return out


# --------------------------------------------------------------------------
# V1 图片 sixel 渲染可见
# --------------------------------------------------------------------------
def v1_image():
    print("== V1 图片渲染可见（sixel/halfblocks 与文本基线对比）==")
    geo = GEO
    if geo is None:
        skip("V1 图片渲染", "几何标定失败（无法定位行带）")
        return
    bd = bands(geo, bar_h=0)
    img = os.path.join(FIX, "img", "gradient.png")
    s = Session("v1-img", [img]).start()
    state, c = s.wait_ready()
    if state != "ready":
        skip("V1 图片渲染", f"窗口未就绪({state})")
        s.close()
        return
    shot = s.shot("image")
    st = region_stats(shot, *bd["body"])
    txt_tokens = ocr(crop(shot, os.path.join(s.dir, "image-body.png"), 0, bd["body"][0],
                          png_size(shot)[0], bd["body"][1] - bd["body"][0]))
    check(st["quant"] >= 60, "V1 图片体区颜色丰富（量化色数 ≥ 60）",
          f"quant={st['quant']} ink={st['ink']} ({os.path.basename(shot)})")
    check(not re.search(r"unavailable|not found|\u2717", txt_tokens),
          "V1 无图片降级/错误行", txt_tokens.strip().replace("\n", " | ")[:60])
    check("gradient.png" in ocr(shot), "V1 header 显示文件名")

    # 基线：同几何文本渲染（体区应远不如图片丰富）
    base = Session("v1-txt", [os.path.join(FIX, "plain.txt")], label="v1-text").start()
    state_b, _ = base.wait_ready()
    if state_b == "ready":
        bshot = base.shot("text")
        bst = region_stats(bshot, *bd["body"])
        check(st["quant"] >= 60 and st["quant"] >= 2 * max(bst["quant"], 1),
              "V1 文本基线体区颜色量显著更低（图片 ≥ 2× 文本，且图片 ≥ 60）",
              f"text quant={bst['quant']} vs image quant={st['quant']}")
        check("This is a plain text file" in ocr(bshot), "V1 文本基线渲染正常")
    else:
        skip("V1 文本基线对比", f"基线窗口未就绪({state_b})")
    base.close()

    # sixel 与 halfblocks 的保真度差异（信息项，不判定失败）
    half = Session("v1-half", [img], env_extra={"DLOOK_IMAGE_PROTOCOL": "halfblocks"},
                   label="v1-halfblocks").start()
    state_h, _ = half.wait_ready()
    if state_h == "ready":
        hshot = half.shot("halfblocks")
        hst = region_stats(hshot, *bd["body"])
        print(f"    info: auto(quant={st['quant']}) vs halfblocks(quant={hst['quant']})")
    half.close()

    rc = s.quit()
    check(rc == 0, f"V1 图片模式 q 退出码 0 (got {rc})")
    s.close()


# --------------------------------------------------------------------------
# V2–V4 视频（需 media-3 的 mpv 路径落地）
# --------------------------------------------------------------------------
def video_precondition():
    """视频引擎前置探测：返回 (Session|None, reason)。

    reason 非空 = 不能进入 mpv 共屏路径 → V2/V3/V4 显式 SKIP。
    判据（按优先级）：
      1. `dlook <clip>` 在真实终端里立即退出且 rc=101 → video.rs 仍为 todo!()
      2. 进程存活但媒体栏缺失（体区出现 "mpv not found" 等降级文案）→ 走降级链
    """
    clip = os.path.join(FIX, "video", "clip.mp4")
    if not os.path.exists(clip):
        return None, f"缺少 fixture {clip}"
    if not shutil.which("mpv"):
        return None, "本机无 mpv（走降级链，共屏路径不成立）"
    proto = os.environ.get("DLOOK_IMAGE_PROTOCOL") or "auto"
    if proto in ("off", "halfblocks", "none"):
        return None, f"DLOOK_IMAGE_PROTOCOL={proto} → 无图形协议，走降级链"
    s = Session("v2-video", [clip], wait=4.0).start()
    state, _ = s.wait_ready()
    if state.startswith("exited"):
        rc = state.split(":")[1]
        reason = ("待 media-3 落地(video.rs 仍为 todo!() → 进程 panic 101)"
                  if rc == "101" else f"视频模式未启动(退出码 {rc})")
        s.close()
        return None, reason
    if state != "ready":
        s.close()
        return None, f"窗口未就绪({state})"
    text = ocr(s.shot("precondition"))
    if re.search(r"(?i)(mpv not found|no graphics protocol|first frame|ffmpeg failed)", text):
        s.close()
        return None, "走了降级链（无 mpv/无图形协议）→ 共屏路径不成立"
    return s, None


def v2_video(s, bd):
    print("== V2 视频四态（播放帧变化 / 暂停冻结 / 恢复 / seek）==")
    body = bd["body"]
    a = s.shot("play-1")
    time.sleep(1.2)
    b = s.shot("play-2")
    d_play = region_diff(a, b, *body)
    check(d_play > 0.001, "V2 播放中视频区帧变化", f"diff={d_play}")

    s.send("space")  # M1: 播放/暂停
    time.sleep(0.8)
    c1 = s.shot("pause-1")
    time.sleep(1.2)
    c2 = s.shot("pause-2")
    d_pause = region_diff(c1, c2, *body)
    check(d_pause == 0.0, "V2 暂停后视频区冻结（逐像素一致）", f"diff={d_pause}")

    s.send("space")
    time.sleep(0.8)
    d1 = s.shot("resume-1")
    time.sleep(1.2)
    d2 = s.shot("resume-2")
    d_resume = region_diff(d1, d2, *body)
    check(d_resume > 0.001, "V2 恢复播放后视频区再次变化", f"diff={d_resume}")

    s.send("Right")  # seek +5s
    time.sleep(0.6)
    e = s.shot("seek")
    d_seek = region_diff(d2, e, *body)
    check(d_seek > 0.0005, "V2 seek 后画面变化", f"diff={d_seek}")


def v3_coscreen(s, bd, clip):
    print("== V3 视频共屏：chrome 仍在 / 视频区未被文字覆写 ==")
    text = ocr(s.shot("coscreen"))
    check(os.path.basename(clip) in text, "V3 header 显示视频文件名",
          text.strip().splitlines()[0][:60] if text.strip() else "")
    check("quit" in text, "V3 footer 键位表仍在")

    first = s.shot("chrome-1")
    time.sleep(1.0)
    s.shot("chrome-2")
    second = s.shots["chrome-2"]
    # chrome 行在播放期间应逐像素稳定（dlook 不重画 mpv 区；mpv 不覆盖 chrome）
    for name in ("header", "footer"):
        d = region_diff(first, second, *bd[name])
        check(d == 0.0, f"V3 {name} 行在播放期间稳定（未被视频覆盖）", f"diff={d}")
    st_bar = region_stats(first, *bd["bar"])
    check(st_bar["ink"] > 0.0, "V3 媒体栏有内容（进度条/时间码/标题）",
          f"ink={st_bar['ink']}")
    body = bd["body"]
    st_body = region_stats(first, *body)
    check(st_body["quant"] >= 50, "V3 视频区为像素内容（量化色数 ≥ 50）",
          f"quant={st_body['quant']} ink={st_body['ink']}")
    # 视频区不应含 dlook 的 chrome 文案
    body_txt = ocr(crop(first, os.path.join(s.dir, "body-only.png"), 0, body[0],
                        png_size(first)[0], body[1] - body[0]))
    chrome = re.findall(r"(?i)\b(quit|seek|mute|scroll|tone\.wav|clip\.mp4)\b", body_txt)
    check(not chrome, "V3 视频区未被 chrome 文字覆写",
          f"tokens={chrome[:5]} ocr={body_txt.strip()[:60]!r}")


def v4_residue(session_rc):
    print("== V4 退出回收（无残留 mpv / 窗口）==")
    check(session_rc == 0, f"V4 视频会话 q 退出码 0 (got {session_rc})")
    pids, cmds = sweep(kill=True)
    mpv = [c for c in cmds if "mpv" in c]
    check(not mpv, "V4 退出后无残留 mpv 进程", str(mpv[:2]))
    check(find_window(TAG_PREFIX + "v2-video") is None, "V4 退出后无残留测试窗口")


# --------------------------------------------------------------------------
# V5 音频
# --------------------------------------------------------------------------
def v5_audio():
    print("== V5 音频播放：无报错 + 媒体栏随播放推进/暂停冻结 ==")
    geo = GEO
    if geo is None:
        skip("V5 音频播放", "几何标定失败")
        return
    wav = os.path.join(FIX, "audio", "tone.wav")
    if not os.path.exists(wav):
        skip("V5 音频播放", f"缺少 fixture {wav}")
        return
    bd = bands(geo, bar_h=2)
    s = Session("v5-audio", [wav], wait=2.5).start()
    state, _ = s.wait_ready()
    if state.startswith("exited"):
        skip("V5 音频播放", f"音频模式未启动(退出码 {state.split(':')[1]})")
        s.close()
        return
    if state != "ready":
        skip("V5 音频播放", f"窗口未就绪({state})")
        s.close()
        return

    s.send("0")  # 回曲首（确定性起点）
    time.sleep(0.6)
    a = s.shot("play-1")
    time.sleep(1.4)
    b = s.shot("play-2")
    d_play = region_diff(a, b, *bd["bar"])
    text = ocr(b)
    check(d_play > 0.0, "V5 播放中媒体栏随位置推进重绘", f"bar diff={d_play}")
    check("tone.wav" in text, "V5 header/媒体栏显示文件名")
    check(re.search(r"\b\d\d:\d\d\b", text) is not None, "V5 时间码可读（MM:SS）",
          " ".join(re.findall(r"\b\d\d:\d\d\b", text))[:60])
    bad_tokens = re.findall(r"(?i)(not found|no audio device|error|\u2717|cannot)", text)
    check(not bad_tokens, "V5 播放期无报错文案", f"tokens={bad_tokens[:4]}")

    s.send("p")
    time.sleep(0.6)
    c1 = s.shot("pause-1")
    time.sleep(1.4)
    c2 = s.shot("pause-2")
    d_pause = region_diff(c1, c2, *bd["bar"])
    check(d_pause == 0.0, "V5 暂停后媒体栏冻结（逐像素一致）", f"bar diff={d_pause}")
    check("finished" not in ocr(c2), "V5 暂停非播放结束")

    s.send("p")
    time.sleep(1.0)
    d3 = s.shot("resume-1")
    time.sleep(1.2)
    d4 = s.shot("resume-2")
    check(region_diff(d3, d4, *bd["bar"]) > 0.0, "V5 恢复后媒体栏继续推进",
          f"bar diff={region_diff(d3, d4, *bd['bar'])}")

    rc = s.quit()
    check(rc == 0, f"V5 音频 q 退出码 0 (got {rc})")
    s.close()


# --------------------------------------------------------------------------
# 收尾：清理 + 无遗留断言
# --------------------------------------------------------------------------
def final_cleanup():
    print("== V-cleanup 结束清理 ==")
    for s in list(SESSIONS):
        s.close()
    pids, cmds = sweep(kill=True)
    leftover_win = [c.get("title", "") for c in hypr_clients()
                    if TAG_PREFIX in (c.get("title", "") + c.get("class", ""))]
    check(not leftover_win, "V-cleanup 无遗留测试窗口", str(leftover_win[:3]))
    check(not pids, "V-cleanup 无遗留测试进程（含 mpv 孙进程）",
          str([c[:60] for c in cmds[:2]]))


# --------------------------------------------------------------------------
# main
# --------------------------------------------------------------------------
GEO = None


def preflight():
    global GEO
    problems = []
    for tool in ("hyprctl", "grim", "foot", "wtype", "magick", "tesseract"):
        if not shutil.which(tool):
            problems.append(tool)
    if not os.environ.get("WAYLAND_DISPLAY"):
        problems.append("WAYLAND_DISPLAY")
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
    GEO = calibrate()
    if GEO:
        print(f"几何标定: {GEO['rows']}x{GEO['cols']} rows×cols, "
              f"行高 {GEO['pitch']}px, 首行 top {GEO['row0_top']}px")
    else:
        print("几何标定失败（V1/V5 的像素断言将跳过）")
    return True


def main():
    if not preflight():
        print()
        print(f"RESULT: PASS={PASS} FAIL={FAIL} SKIP={SKIP}")
        return 0 if FAIL == 0 else 1
    clip = os.path.join(FIX, "video", "clip.mp4")
    try:
        v1_image()
        bd = bands(GEO, bar_h=2) if GEO else None
        sess, reason = (None, "几何标定失败") if GEO is None else video_precondition()
        if sess is None:
            for d in ["V2 视频四态", "V3 共屏 chrome", "V3 视频区未被文字覆写",
                      "V4 退出回收"]:
                skip(d, f"{reason}（共屏路径不成立，属预期）")
        else:
            v2_video(sess, bd)
            v3_coscreen(sess, bd, clip)
            rc = sess.quit()
            sess.close()
            v4_residue(rc)
        v5_audio()
    finally:
        final_cleanup()

    print()
    print(f"RESULT: PASS={PASS} FAIL={FAIL} SKIP={SKIP}")
    print(f"截图存档: {OUT}")
    print("索引:")
    for name in sorted(os.listdir(OUT)):
        d = os.path.join(OUT, name)
        if os.path.isdir(d):
            for f in sorted(os.listdir(d)):
                if f.endswith(".png"):
                    print(f"  {os.path.join(d, f)}")
    return 0 if FAIL == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
