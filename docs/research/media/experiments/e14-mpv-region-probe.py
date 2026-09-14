#!/usr/bin/env python3
r'''E14: mpv 区域几何原型 —— kitty / sixel VO 能否把画面限制到终端指定矩形。

裁决问题（media-3 实现的前置）:
  1. sixel VO 是否有区域几何参数？有则能否实测把画面限制到指定矩形？
  2. kitty VO 的区域参数语义如何（foot 无 kitty 协议 → 只能做字节级证据）？
  3. sixel 的区域集合同 kitty 一样吗（left/top/cols/rows/width/height）？

方法（两段，均可复现）:

  [A] pty 段（无需图形会话，决定性证据）: 用伪终端捕获 mpv 的原始输出字节，
      - `--list-options` 断言区域选项存在（kitty / sixel 各一栏）；
      - sixel：解析 DCS raster attributes `"pan;pad;W;H` 得实际图像像素尺寸，
        解析光标定位序列 `ESC[<row>;<col>H` 得实际落点（字符格）；
      - kitty：解析 APC 分块 `ESC_G<keys>;<base64>ESC\`，从首个 `a=T` 块的
        `s=`/`v=` 读实际像素尺寸，从 `ESC[<row>;<col>H` 读落点。
  [B] 真实终端段（Hyprland + foot，sixel）: 窗口铺纯色底（rgb(20,40,200)）→
      mpv 暂停渲染一帧 → 截图（按 monitor 输出、再按窗口 rect 裁剪）→
      抹掉底色后 trim 出「画面实际占据的像素矩形」，与基准图逐像素 diff 交叉验证。

用法:
  python3 e14-mpv-region-probe.py            # A + B（有图形会话时）
  python3 e14-mpv-region-probe.py --pty      # 仅 A 段
环境: mpv v0.41.0；B 段需 Hyprland + foot + grim + magick。
所有子进程用独立 socket/窗口标记，退出前 pkill -f <标记>，不留残留。
'''
from __future__ import annotations

import fcntl
import json
import os
import pty
import re
import select
import shutil
import struct
import subprocess
import sys
import termios
import time

EXP = os.path.dirname(os.path.abspath(__file__))
VID = os.path.join(EXP, "test-video.mp4")
TAG = "dlook-e14"  # 窗口/进程标记：清理用 pkill -f dlook-e14
BG = "rgb(20,40,200)"  # foot 窗口纯色底（与 mpv 画面无重叠色）

SIXEL_REGION_OPTS = ["--vo-sixel-left", "--vo-sixel-top", "--vo-sixel-cols",
                     "--vo-sixel-rows", "--vo-sixel-width", "--vo-sixel-height"]
KITTY_REGION_OPTS = ["--vo-kitty-left", "--vo-kitty-top", "--vo-kitty-cols",
                     "--vo-kitty-rows", "--vo-kitty-width", "--vo-kitty-height"]

# 一帧、暂停在 1s 处（画面稳定，便于重复测量）
FRAME = ["--frames=1", "--pause", "--start=1"]
SIXEL_BASE = ["mpv", "--vo=sixel", "--really-quiet", "--no-terminal", "--ao=null",
              "--vo-sixel-alt-screen=no", "--vo-sixel-config-clear=no"] + FRAME + [VID]
KITTY_BASE = ["mpv", "--vo=kitty", "--really-quiet", "--no-terminal", "--ao=null",
              "--vo-kitty-alt-screen=no", "--vo-kitty-config-clear=no"] + FRAME + [VID]


# ---------------------------------------------------------------- helpers

def have(cmd: str) -> bool:
    return shutil.which(cmd) is not None


def run_pty(args, cols=80, rows=24, xpix=800, ypix=480, term="xterm-256color", timeout=12):
    """在伪终端内跑 mpv，返回捕获到的原始字节流。"""
    mfd, sfd = pty.openpty()
    fcntl.ioctl(sfd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, xpix, ypix))
    env = dict(os.environ, TERM=term)
    p = subprocess.Popen(args, stdin=sfd, stdout=sfd, stderr=subprocess.DEVNULL,
                         env=env, close_fds=True)
    os.close(sfd)
    buf = b""
    t0 = time.time()
    while True:
        r, _, _ = select.select([mfd], [], [], 0.3)
        if r:
            try:
                d = os.read(mfd, 1 << 20)
            except OSError:
                break
            if not d:
                break
            buf += d
        elif p.poll() is not None:
            break
        if time.time() - t0 > timeout:
            p.kill()
            break
    p.wait()
    os.close(mfd)
    return buf


def sixel_stats(buf: bytes):
    """返回 (图像像素尺寸 (W,H), 首个光标定位 (row,col))。"""
    rast = re.findall(rb'q"(\d+);(\d+);(\d+);(\d+)', buf)
    cur = re.findall(rb"\x1b\[(\d+);(\d+)[Hf]", buf)
    size = (int(rast[0][2]), int(rast[0][3])) if rast else None
    pos = (int(cur[0][0]), int(cur[0][1])) if cur else None
    return size, pos


# kitty 图形：ESC _ G <keys>;<payload> ESC \ （mpv 分块传输，m=1/m=0）
KITTY_CHUNK = re.compile(rb"\x1b_G([^;\x1b]*);([A-Za-z0-9+/=]*)\x1b\\")


def kitty_stats(buf: bytes):
    """返回 (分块数, base64 载荷字节, (s,v,f) 或 None, 首个光标定位 (row,col))。"""
    chunks = KITTY_CHUNK.findall(buf)
    b64 = b"".join(pl for _, pl in chunks)
    dims = None
    for keys, _ in chunks:
        if b"a=T" in keys:
            s = re.search(rb"s=(\d+)", keys)
            v = re.search(rb"v=(\d+)", keys)
            f = re.search(rb"f=(\d+)", keys)
            dims = (int(s.group(1)) if s else None,
                    int(v.group(1)) if v else None,
                    int(f.group(1)) if f else None)
            break
    cur = re.findall(rb"\x1b\[(\d+);(\d+)[Hf]", buf)
    pos = (int(cur[0][0]), int(cur[0][1])) if cur else None
    return len(chunks), len(b64), dims, pos


def region_opts(vo: str):
    """`mpv --vo=<vo> --list-options` 里该 vo 的区域选项（裁决问题的直接证据）。"""
    out = subprocess.run(["mpv", f"--vo={vo}", "--list-options"],
                         capture_output=True, text=True).stdout
    return (sorted({"--" + m.group(1) for m in re.finditer(rf"--(vo-{vo}-(?:left|top|cols|rows|width|height))\b", out)}),
            f"--vo-{vo}-alt-screen" in out)


# ---------------------------------------------------------------- [A] pty 段

def part_a():
    print("=" * 72)
    print("[A] pty 段：选项存在性 + 原始输出字节里的区域几何证据")
    print("=" * 72)

    a1 = {}
    for vo in ("sixel", "kitty"):
        opts, alt = region_opts(vo)
        a1[vo] = opts
        print(f"  vo={vo:5s}: 区域选项 {opts}")
        print(f"            alt-screen 选项存在={alt}")
    ok_opts = (set(a1["sixel"]) == set(SIXEL_REGION_OPTS)) and (set(a1["kitty"]) == set(KITTY_REGION_OPTS))
    print(f"  → 两 vo 的区域选项集合相同且齐备: {ok_opts}")
    print()

    print("  [A2] vo=sixel（pty 80x24 格 / 800x480 像素）")
    print(f"    {'变体':36s} {'图像像素(WxH)':>14s} {'光标(row,col)':>14s}")
    a2 = {}
    for label, extra in [
        ("(无区域参数)", []),
        ("left=10 top=5（仅定位）", ["--vo-sixel-left=10", "--vo-sixel-top=5"]),
        ("left=3 top=2 cols=20 rows=5", ["--vo-sixel-left=3", "--vo-sixel-top=2",
                                         "--vo-sixel-cols=20", "--vo-sixel-rows=5"]),
        ("left=3 top=2 width=200 height=100", ["--vo-sixel-left=3", "--vo-sixel-top=2",
                                               "--vo-sixel-width=200", "--vo-sixel-height=100"]),
        ("left=3 top=2 width=400 height=200", ["--vo-sixel-left=3", "--vo-sixel-top=2",
                                               "--vo-sixel-width=400", "--vo-sixel-height=200"]),
    ]:
        buf = run_pty(SIXEL_BASE + extra)
        size, pos = sixel_stats(buf)
        a2[label] = (size, pos)
        print(f"    {label:36s} {str(size):>14s} {str(pos):>14s}")
    print()

    print("  [A3] vo=kitty（TERM=xterm-kitty，pty 80x24 / 800x480）")
    print(f"    {'变体':36s} {'分块':>5s} {'载荷字节':>9s} {'(s,v,f)':>16s} {'光标':>12s}")
    a3 = {}
    for label, extra in [
        ("(无区域参数)", []),
        ("left=10 top=5（仅定位）", ["--vo-kitty-left=10", "--vo-kitty-top=5"]),
        ("left=10 top=5 cols=40 rows=10", ["--vo-kitty-left=10", "--vo-kitty-top=5",
                                           "--vo-kitty-cols=40", "--vo-kitty-rows=10"]),
        ("left=10 top=5 width=400 height=200", ["--vo-kitty-left=10", "--vo-kitty-top=5",
                                                "--vo-kitty-width=400", "--vo-kitty-height=200"]),
        ("left=10 top=5 width=200 height=100", ["--vo-kitty-left=10", "--vo-kitty-top=5",
                                                "--vo-kitty-width=200", "--vo-kitty-height=100"]),
    ]:
        buf = run_pty(KITTY_BASE + extra, term="xterm-kitty")
        n, b64, dims, pos = kitty_stats(buf)
        a3[label] = (n, b64, dims, pos)
        print(f"    {label:36s} {n:>5d} {b64:>9d} {str(dims):>16s} {str(pos):>12s}")
    print()

    checks = []
    # sixel: left/top 改变落点（字符格），像素尺寸不变
    s_plain = a2["(无区域参数)"]
    s_lt = a2["left=10 top=5（仅定位）"]
    checks.append(("sixel left/top 生效（落点随参数移动）",
                   s_lt[1] == (5, 10) and s_lt[0] == s_plain[0],
                   f"nolimit pos={s_plain[1]} size={s_plain[0]} → lt(10,5) pos={s_lt[1]} size={s_lt[0]}"))
    # sixel: width/height 裁剪图像像素尺寸（并保持宽高比、高度取 6 的倍数）
    w200 = a2["left=3 top=2 width=200 height=100"][0]
    w400 = a2["left=3 top=2 width=400 height=200"][0]
    checks.append(("sixel width/height 裁剪图像尺寸",
                   w200 is not None and w200[0] < s_plain[0][0] and w200[1] <= 100 and w200[1] % 6 == 0,
                   f"nolimit={s_plain[0]} → wh200x100={w200} → wh400x200={w400}"))
    # sixel: cols/rows 不裁剪图像像素尺寸
    cr = a2["left=3 top=2 cols=20 rows=5"][0]
    checks.append(("sixel cols/rows 不裁剪图像尺寸（只声明可用格数）", cr == s_plain[0],
                   f"nolimit={s_plain[0]} vs cols20rows5={cr}"))
    # kitty: 同样语义
    k_plain, k_lt = a3["(无区域参数)"], a3["left=10 top=5（仅定位）"]
    checks.append(("kitty left/top 生效（光标定位随参数移动）",
                   k_lt[3] == (5, 10) and k_lt[2] == k_plain[2],
                   f"nolimit pos={k_plain[3]} (s,v,f)={k_plain[2]} → lt(10,5) pos={k_lt[3]}"))
    k_wh = a3["left=10 top=5 width=200 height=100"][2]
    checks.append(("kitty width/height 裁剪图像尺寸",
                   k_wh is not None and k_wh[0] < k_plain[2][0],
                   f"nolimit (s,v,f)={k_plain[2]} → wh200x100={k_wh}"))
    checks.append(("两 vo 区域选项集合相同（同一族参数）", ok_opts, f"sixel={a1['sixel']}"))
    return checks


# ---------------------------------------------------------------- [B] 真实终端段

def hypr(cmd):
    return json.loads(subprocess.run(["hyprctl"] + cmd, capture_output=True, text=True).stdout)


def find_window(tag):
    for c in hypr(["clients", "-j"]):
        if tag in c.get("title", "") or tag in c.get("class", ""):
            return c
    return None


def monitor_of(c):
    for m in hypr(["monitors", "-j"]):
        if m["id"] == c["monitor"]:
            return m
    return None


FILL = "printf '\\033[?25l\\033[48;2;20;40;200m\\033[2J\\033[H'; "  # 纯色底 + 藏光标


def grab(crop_tag):
    """当前 foot 窗口的截图（按 monitor 输出 → 按窗口 rect 裁剪为设备像素）。"""
    c = find_window(TAG)
    if not c:
        return None, None
    mon = monitor_of(c)
    if not mon:
        return None, None
    sc = mon["scale"]
    full = f"/tmp/e14-{crop_tag}-mon.png"
    subprocess.run(["grim", "-o", mon["name"], full], check=True)
    rx = int((c["at"][0] - mon["x"]) * sc)
    ry = int((c["at"][1] - mon["y"]) * sc)
    rw = int(c["size"][0] * sc)
    rh = int(c["size"][1] * sc)
    out = f"/tmp/e14-{crop_tag}.png"
    r = subprocess.run(["magick", full, "-crop", f"{rw}x{rh}+{rx}+{ry}", "+repage", out],
                       capture_output=True, text=True)
    if r.returncode != 0:
        return None, None
    return out, (rw, rh, sc)


def foot_pair(mpv_cmd_line, label, pre=4.0, settle=4.0, tries=3):
    """同一窗口的 before/after 对照（几何一致 → diff 只含 mpv 画面）。

    脚本: 铺纯色底 → sleep pre → mpv（--pause --frames=1，渲染完一帧即退出，
    alt-screen=no 使画面留在屏上）→ sleep。before 在 mpv 启动前截，after 在其后截。
    几何在两次截图间变化时重试（混合 scale 多显示器环境下窗口可能被重排）。
    """
    for attempt in range(tries):
        subprocess.run(["pkill", "-f", TAG], capture_output=True)
        time.sleep(0.4)
        script = FILL + f"sleep {int(pre)}; {mpv_cmd_line}; sleep 45"
        subprocess.Popen(["foot", "-a", TAG, "-T", TAG, "-W", "80x24", "--",
                          "bash", "-lc", script],
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                         stdin=subprocess.DEVNULL, start_new_session=True)
        c = None
        deadline = time.time() + 15
        while time.time() < deadline:
            c = find_window(TAG)
            if c:
                break
            time.sleep(0.3)
        if not c:
            continue
        subprocess.run(["hyprctl", "dispatch", "focuswindow", f"address:{c['address']}"],
                       capture_output=True)
        time.sleep(pre - 1.2)
        a, ga = grab(f"{label}-before")
        time.sleep(settle)
        b, gb = grab(f"{label}-after")
        subprocess.run(["pkill", "-f", TAG], capture_output=True)
        time.sleep(0.5)
        if a and b and ga and gb and ga[:2] == gb[:2]:
            return diff_bbox(b, a), gb
    return None, None


def diff_bbox(after, before):
    """两图逐像素 diff → 变化区域矩形 (w,h,x,y)（设备像素）。"""
    r = subprocess.run(["magick", after, "(", before, ")", "-compose", "difference",
                        "-composite", "-colorspace", "gray", "-threshold", "25%",
                        "-trim", "-format", "%w %h %X %Y", "info:"],
                       capture_output=True, text=True)
    if r.returncode != 0 or not r.stdout.strip():
        return None
    try:
        w, h, x, y = (int(v) for v in r.stdout.split())
    except ValueError:
        return None
    return (w, h, x, y) if w > 2 and h > 2 else None


def mpv_cmd(sixel: bool, extra):
    vo = "sixel" if sixel else "kitty"
    return (f"mpv --vo={vo} --really-quiet --no-terminal --ao=null --pause --start=1 "
            f"--vo-{vo}-alt-screen=no --vo-{vo}-config-clear=no "
            + " ".join(extra) + " " + VID)


def part_b():
    print("=" * 72)
    print("[B] 真实终端段：Hyprland + foot（sixel）里的画面落点与尺寸")
    print("=" * 72)
    print("  方法：同一 foot 窗口 before/after 对照（纯色底 → mpv --pause --frames=1 渲染一帧）")
    variants = [
        ("six-nolimit", True, []),
        ("six-lt1-1", True, ["--vo-sixel-left=1", "--vo-sixel-top=1"]),
        ("six-lt6-4", True, ["--vo-sixel-left=6", "--vo-sixel-top=4"]),
        ("six-lt12-9", True, ["--vo-sixel-left=12", "--vo-sixel-top=9"]),
        ("six-wh200x100", True, ["--vo-sixel-left=1", "--vo-sixel-top=1",
                                 "--vo-sixel-width=200", "--vo-sixel-height=100"]),
        ("kit-in-foot", False, []),
    ]
    res = {}
    for label, sixel, extra in variants:
        bb, geom = foot_pair(mpv_cmd(sixel, extra), label)
        res[label] = {"bbox": bb, "geom": geom}
        print(f"  {label:16s} 变化区(w h x y，设备px)={bb}  窗口设备px={geom[:2] if geom else None}")
    return res


def verdict_b(res):
    checks = []
    def bb(lbl):
        return res.get(lbl, {}).get("bbox")

    nl, lt11, lt64, lt129 = bb("six-nolimit"), bb("six-lt1-1"), bb("six-lt6-4"), bb("six-lt12-9")
    if nl and lt64 and lt129:
        mono = (lt64[2] > nl[2]) and (lt129[2] > lt64[2]) and (lt129[3] > lt64[3])
        checks.append(("sixel left/top 生效（画面原点随格坐标单调右/下移）", mono,
                       f"nolimit={nl} → lt6,4={lt64} → lt12,9={lt129}"))
    if nl and lt11:
        checks.append(("sixel left=1/top=1 与默认（auto）原点不同",
                       lt11[2:] != nl[2:], f"nolimit 原点={nl[2:]} lt1,1 原点={lt11[2:]}"))
    wh = bb("six-wh200x100")
    if nl and wh:
        checks.append(("sixel width/height 限尺寸（画面像素尺寸显著变小）",
                       wh[0] < nl[0] * 0.75 and wh[1] < nl[1] * 0.75,
                       f"nolimit={nl[:2]} → wh200x100={wh[:2]}（设备px；与 pty 段 raster 一致）"))
    k = bb("kit-in-foot")
    if k is not None:
        checks.append(("kitty VO 在 foot 无画面（foot 无 kitty 协议）",
                       k[0] < 120 and k[1] < 120, f"变化区={k}"))
    return checks


def main():
    if not os.path.exists(VID):
        print(f"缺少测试素材 {VID}")
        return 2
    if not have("mpv"):
        print("缺少 mpv")
        return 2

    checks = list(part_a())
    gui = all(have(c) for c in ("foot", "grim", "hyprctl", "magick")) and \
        os.environ.get("HYPRLAND_INSTANCE_SIGNATURE")
    if "--pty" in sys.argv or not gui:
        print("[B] 跳过（无 Hyprland/foot/grim 或无 HYPRLAND_INSTANCE_SIGNATURE，或 --pty）")
    else:
        res = part_b()
        checks += verdict_b(res)

    print()
    ok = True
    for name, c, detail in checks:
        print(f"  [{'PASS' if c else 'FAIL'}] {name}\n          {detail}")
        ok &= bool(c)
    print()
    print("VERDICT:", "PASS" if ok else "FAIL")
    return 0 if ok else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    finally:
        subprocess.run(["pkill", "-f", TAG], capture_output=True)
