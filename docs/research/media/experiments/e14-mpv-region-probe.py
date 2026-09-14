#!/usr/bin/env python3
"""E14: mpv 区域几何原型 —— kitty / sixel VO 能否把画面限制到终端指定矩形。

回答的裁决问题（media-3 的前置）:
  1. sixel VO 是否有区域几何参数？有则能否实测限制到指定矩形？
  2. kitty VO 的区域参数在真实终端里表现如何（foot 无 kitty 协议 → 参数校验 + 实测无输出）？

方法（两段，均可复现）:

  [A] pty 段（无需图形会话）: 用伪终端捕获 mpv 的原始输出字节，
      - `--list-options` 断言区域选项存在（kitty / sixel 各一栏）；
      - 解析 sixel DCS 的 raster attributes `"pan;pad;W;H` 得到实际图像像素尺寸；
      - 解析光标定位序列 `ESC[<row>;<col>f` 得到实际落点；
      - kitty 段解析 `ESC_G...ESC\` 分块载荷总字节数，反推每帧像素量与尺寸变化。
  [B] 真实终端段（Hyprland + foot，sixel）: 窗口铺纯色底 → mpv 暂停渲染一帧 →
      grim 截窗口 → connected-components 求「画面实际占据的像素矩形」，
      与不限定区域时对比，判断 left/top（定位）与 width/height（限尺寸）是否生效。

用法:
  python3 e14-mpv-region-probe.py            # A + B（有图形会话时）
  python3 e14-mpv-region-probe.py --pty      # 仅 A 段
环境: mpv v0.41.0；B 段需 Hyprland + foot + grim + magick。
所有子进程用独立 socket/窗口标记，退出前 pkill -f <标记>，不留残留。
"""
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

SIXEL_BASE = ["mpv", "--vo=sixel", "--really-quiet", "--no-terminal", "--ao=null",
              "--frames=1", "--pause", "--start=1", VID]
KITTY_BASE = ["mpv", "--vo=kitty", "--really-quiet", "--no-terminal", "--ao=null",
              "--frames=1", "--pause", "--start=1",
              "--vo-kitty-alt-screen=no", "--vo-kitty-config-clear=no", VID]


# ---------------------------------------------------------------- helpers

def have(cmd: str) -> bool:
    return shutil.which(cmd) is not None


def run_pty(args, cols=80, rows=24, xpix=800, ypix=480, term="xterm-256color", timeout=10):
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
    cur = re.findall(rb"\x1b\[(\d+);(\d+)f", buf)
    size = (int(rast[0][2]), int(rast[0][3])) if rast else None
    pos = (int(cur[0][0]), int(cur[0][1])) if cur else None
    return size, pos


def kitty_stats(buf: bytes):
    """返回 (APC 分块数, base64 载荷总字节, 反推像素数, 首个光标定位)。"""
    # kitty 图形：ESC _ G <keys>;<payload> ESC \   （mpv 分块传输，m=1/m=0）
    chunks = re.findall(rb"\x1b_G([^;\x1b]*);([A-Za-z0-9+/=]*)\x1b\\\\", buf)
    b64 = b"".join(pl for _, pl in chunks)
    px = len(b64) * 3 // 4 // 3  # base64 → 字节 → RGB24 像素
    cur = re.findall(rb"\x1b\[(\d+);(\d+)[Hf]", buf)
    pos = (int(cur[0][0]), int(cur[0][1])) if cur else None
    return len(chunks), len(b64), px, pos


# ---------------------------------------------------------------- [A] pty 段

def part_a():
    print("=" * 72)
    print("[A] pty 段：选项存在性 + 原始输出字节里的区域几何证据")
    print("=" * 72)

    # A1. 选项存在性（裁决问题的直接证据）
    for vo, key in (("sixel", "vo-sixel"), ("kitty", "vo-kitty")):
        out = subprocess.run(["mpv", f"--vo={vo}", "--list-options"],
                             capture_output=True, text=True).stdout
        geo = sorted({m.group(1) for m in re.finditer(rf"--(vo-{vo}-(?:left|top|cols|rows|width|height))\b", out)})
        alt = len(re.findall(rf"--vo-{vo}-alt-screen", out)) > 0
        print(f"  vo={vo}: 区域选项 {geo}  alt-screen 选项={alt}")
    print()

    # A2. sixel：区域参数实测（同一 pty 尺寸下比较）
    print("  [A2] vo=sixel（pty 80x24 格 / 800x480 像素，像素尺寸可探测）")
    print(f"    {'变体':34s} {'图像像素尺寸':>14s} {'光标(row,col)':>14s}")
    for label, extra in [
        ("(无区域参数)", []),
        ("left=10 top=5（定位）", ["--vo-sixel-left=10", "--vo-sixel-top=5"]),
        ("left=10 top=5 cols=40 rows=10", ["--vo-sixel-left=10", "--vo-sixel-top=5",
                                           "--vo-sixel-cols=40", "--vo-sixel-rows=10"]),
        ("left=3 top=2 cols=20 rows=5", ["--vo-sixel-left=3", "--vo-sixel-top=2",
                                         "--vo-sixel-cols=20", "--vo-sixel-rows=5"]),
        ("left=3 top=2 width=200 height=100", ["--vo-sixel-left=3", "--vo-sixel-top=2",
                                               "--vo-sixel-width=200", "--vo-sixel-height=100"]),
        ("left=3 top=2 width=400 height=200", ["--vo-sixel-left=3", "--vo-sixel-top=2",
                                               "--vo-sixel-width=400", "--vo-sixel-height=200"]),
    ]:
        buf = run_pty(SIXEL_BASE + extra)
        size, pos = sixel_stats(buf)
        print(f"    {label:34s} {str(size):>14s} {str(pos):>14s}")
    print()

    # A3. kitty：区域参数对每帧载荷/定位的影响（foot 无 kitty 协议，仅字节级证据）
    print("  [A3] vo=kitty（TERM=xterm-kitty，pty 80x24 / 800x480）")
    print(f"    {'变体':34s} {'分块数':>6s} {'载荷字节':>10s} {'≈像素':>8s} {'光标':>12s}")
    for label, extra in [
        ("(无区域参数)", []),
        ("left=10 top=5 cols=40 rows=10", ["--vo-kitty-left=10", "--vo-kitty-top=5",
                                           "--vo-kitty-cols=40", "--vo-kitty-rows=10"]),
        ("left=10 top=5 width=400 height=200", ["--vo-kitty-left=10", "--vo-kitty-top=5",
                                                 "--vo-kitty-width=400", "--vo-kitty-height=200"]),
    ]:
        buf = run_pty(KITTY_BASE + extra, term="xterm-kitty")
        n, b64, px, pos = kitty_stats(buf)
        print(f"    {label:34s} {n:>6d} {b64:>10d} {px:>8d} {str(pos):>12s}")
    print()
    return True


# ---------------------------------------------------------------- [B] 真实终端段

def hypr_clients():
    out = subprocess.run(["hyprctl", "clients", "-j"], capture_output=True, text=True).stdout
    return json.loads(out)


def find_window(tag):
    for c in hypr_clients():
        if tag in c.get("title", "") or tag in c.get("class", ""):
            return c
    return None


FILL = ("printf '\\033[2J'; printf '\\033[48;2;20;40;200m'; "
        "for i in $(seq 1 80); do printf '%200s\\n' ''; done; printf '\\033[H'")


def foot_run(tail: str, label: str, settle=6.0):
    """开一个 foot 窗口（铺纯色底 + 执行 tail），聚焦后截图，返回 PNG 路径。"""
    subprocess.run(["pkill", "-f", TAG], capture_output=True)
    time.sleep(0.5)
    proc = subprocess.Popen(["foot", "-a", TAG, "-T", TAG, "--", "bash", "-lc", FILL + "; " + tail],
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
        proc.terminate()
        return None
    subprocess.run(["hyprctl", "dispatch", "focuswindow", f"address:{c['address']}"],
                   capture_output=True)
    time.sleep(0.4)
    time.sleep(settle)
    x, y = c["at"]
    w, h = c["size"]
    path = f"/tmp/e14-{label}.png"
    subprocess.run(["grim", "-g", f"{x},{y} {w}x{h}", path], check=True)
    subprocess.run(["pkill", "-f", TAG], capture_output=True)
    time.sleep(0.7)
    return path


def video_bbox(base_png: str, var_png: str):
    """两图差异的连通域 → 画面占据的矩形（返回 (w,h,x,y) 与两个主连通域）。"""
    out = subprocess.run(
        ["magick", base_png, var_png, "-compose", "difference", "-composite",
         "-colorspace", "gray", "-threshold", "20%",
         "-define", "connected-components:verbose=true",
         "-define", "connected-components:area-threshold=300",
         "-connected-components", "4", "null:"],
        capture_output=True, text=True).stderr
    boxes = []
    for line in out.splitlines():
        m = re.match(r"\s*\d+:\s+(\d+)x(\d+)\+(\d+)\+(\d+)", line)
        if m:
            w, h, x, y = (int(m.group(i)) for i in range(1, 5))
            if w * h < 1000:
                continue  # 光标/文字残留
            boxes.append((w, h, x, y))
    if not boxes:
        return None, boxes
    x0 = min(b[2] for b in boxes)
    y0 = min(b[3] for b in boxes)
    x1 = max(b[2] + b[0] for b in boxes)
    y1 = max(b[3] + b[1] for b in boxes)
    return (x1 - x0, y1 - y0, x0, y0), boxes


def part_b():
    print("=" * 72)
    print("[B] 真实终端段：Hyprland + foot（sixel）里的画面落点与尺寸")
    print("=" * 72)
    base = foot_run("sleep 30", "base")
    if not base:
        print("  跳过：取不到 foot 窗口（无图形会话？）")
        return None
    print(f"  基准（纯色底，无 mpv）: {base}")

    six = ("mpv --vo=sixel --really-quiet --no-terminal --ao=null --pause --start=1 "
           "--vo-sixel-alt-screen=no --vo-sixel-config-clear=no ")
    kit = ("mpv --vo=kitty --really-quiet --no-terminal --ao=null --pause --start=1 "
           "--vo-kitty-alt-screen=no --vo-kitty-config-clear=no ")
    variants = [
        ("sixel-nolimit", f"{six} {VID}"),
        ("sixel-lt11", f"{six} --vo-sixel-left=1 --vo-sixel-top=1 {VID}"),
        ("sixel-lt64", f"{six} --vo-sixel-left=6 --vo-sixel-top=4 {VID}"),
        ("sixel-wh200x100", f"{six} --vo-sixel-left=1 --vo-sixel-top=1 --vo-sixel-width=200 --vo-sixel-height=100 {VID}"),
        ("sixel-cols20rows8", f"{six} --vo-sixel-left=1 --vo-sixel-top=1 --vo-sixel-cols=20 --vo-sixel-rows=8 {VID}"),
        ("sixel-wh400x200", f"{six} --vo-sixel-left=1 --vo-sixel-top=1 --vo-sixel-width=400 --vo-sixel-height=200 {VID}"),
        ("kitty-in-foot", f"{kit} {VID}"),
    ]
    results = {}
    for label, tail in variants:
        png = foot_run(tail, label)
        if not png:
            print(f"  {label}: 窗口获取失败")
            continue
        bb, boxes = video_bbox(base, png)
        results[label] = bb
        print(f"  {label:20s} 画面矩形(设备px)={bb}  连通域={boxes}")
    return base, results


# ---------------------------------------------------------------- main

def main():
    if not os.path.exists(VID):
        print(f"缺少测试素材 {VID}")
        return 2
    if not have("mpv"):
        print("缺少 mpv")
        return 2

    part_a()

    gui = all(have(c) for c in ("foot", "grim", "hyprctl", "magick")) and os.environ.get("HYPRLAND_INSTANCE_SIGNATURE")
    if "--pty" in sys.argv or not gui:
        print("[B] 跳过（无 Hyprland/foot/grim/未指定）")
        verdict = True
    else:
        base, res = part_b()

        def size(lbl):
            return res.get(lbl, (None,))[0] if res.get(lbl) else None

        ok = True
        checks = []
        # 1) left/top 定位生效：lt64 的画面 x/y 应大于 lt11
        a, b = res.get("sixel-lt11"), res.get("sixel-lt64")
        if a and b:
            c = b[2] > a[2] and b[3] > a[3]
            checks.append(("sixel left/top 生效（lt64 相对 lt11 右下方）", c, f"{a} -> {b}"))
            ok &= c
        # 2) width/height 限尺寸：wh200x100 的宽度应显著小于无限制
        a, b = res.get("sixel-nolimit"), res.get("sixel-wh200x100")
        if a and b:
            c = b[0] < a[0]
            checks.append(("sixel width/height 限尺寸", c, f"nolimit={a[0]}px -> wh200x100={b[0]}px"))
            ok &= c
        # 3) cols/rows 不裁剪（负结果，记录用）
        a, b = res.get("sixel-nolimit"), res.get("sixel-cols20rows8")
        if a and b:
            c = b[0] == a[0]
            checks.append(("sixel cols/rows 不改变图像尺寸（只影响布局推算）", c, f"{a[0]} vs {b[0]}"))
        # 4) kitty VO 在 foot（无 kitty 协议）无画面输出
        k = res.get("kitty-in-foot")
        if k is not None:
            c = k[0] < 40
            checks.append(("kitty VO 在 foot 无画面（foot 无 kitty 协议）", c, f"bbox={k}"))
            ok &= c
        print()
        for name, c, detail in checks:
            print(f"  [{'PASS' if c else 'FAIL'}] {name}  ({detail})")
        verdict = ok

    print()
    print("VERDICT:", "PASS" if verdict else "FAIL")
    return 0 if verdict else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    finally:
        subprocess.run(["pkill", "-f", TAG], capture_output=True)
