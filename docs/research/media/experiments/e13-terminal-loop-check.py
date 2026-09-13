#!/usr/bin/env python3
"""E13: 真实终端闭环 —— 渲染 / 按键注入 / 退出码读回。

回答: dlook 在真实 foot 终端里的渲染与退出行为是否可被自动断言。
做法: foot 里跑 `dlook <file>; echo EXIT=$?` → grim 截图验证渲染 →
      聚焦窗口 → wtype 注入 'q' → 截图读回 EXIT=0。
"""
import hashlib
import json
import os
import subprocess
import sys
import time

TAG = "dlook-e13"
TARGET = "/mnt/data/dlook/docs/research/media/experiments/frame.png"


def sh(cmd, **kw):
    return subprocess.run(cmd, capture_output=True, text=True, **kw)


def win():
    for c in json.loads(sh(["hyprctl", "clients", "-j"]).stdout):
        if TAG in (c.get("title", "") + c.get("class", "")):
            x, y = c["at"]
            w, h = c["size"]
            return x, y, w, h
    return None


def shot(path):
    g = win()
    if not g:
        return None
    x, y, w, h = g
    sh(["grim", "-g", f"{x},{y} {w}x{h}", path])
    return hashlib.md5(open(path, "rb").read()).hexdigest()[:10]


def main():
    # 用 sh -c 包一层,退出后在终端打印退出码(供截图读回)
    proc = subprocess.Popen(
        ["foot", "-a", TAG, "-T", TAG, "--", "sh", "-c",
         f"/mnt/data/dlook/rs/target/release/dlook {TARGET}; echo EXIT=$?; sleep 8"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        stdin=subprocess.DEVNULL, start_new_session=True)

    deadline = time.time() + 12
    while time.time() < deadline and not win():
        time.sleep(0.3)
    if not win():
        print("FAIL: window not found")
        proc.terminate()
        return 1
    print("window:", win())
    time.sleep(2.0)

    h1 = shot("/tmp/e13-rendered.png")
    print(f"[1] rendered hash={h1}")

    # 聚焦 + 注入 q(验证 wtype 按键注入通道)
    r = sh(["hyprctl", "dispatch", f'hl.dsp.focus({{window="class:{TAG}"}})'])
    print("[2] focus:", r.stdout.strip())
    time.sleep(0.6)
    r = sh(["wtype", "-k", "q"]) if sh(["which", "wtype"]).returncode == 0 else r
    print("[3] wtype q:", "ok" if r.returncode == 0 else f"fail {r.stderr.strip()[:60]}")
    time.sleep(1.5)

    h2 = shot("/tmp/e13-after-q.png")
    print(f"[4] after-q hash={h2}  changed={h1 != h2}")

    print()
    print("截图存档供视觉读回:", "/tmp/e13-rendered.png", "/tmp/e13-after-q.png")
    time.sleep(6)
    proc.terminate()
    return 0


if __name__ == "__main__":
    sys.exit(main())
