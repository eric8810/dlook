#!/usr/bin/env python3
"""E11: 视觉验证 —— mpv 视频在真实终端里播放/暂停的截图断言。

用途：证明「录制/截图 + 视觉检查」可作为视频 E2E 的验证通道。
运行：本脚本在 Hyprland 图形会话内执行（需 grim / hyprctl / foot / mpv）。
"""
import hashlib
import json
import os
import socket
import subprocess
import sys
import time

EXP = "/mnt/data/dlook/docs/research/media/experiments"
SOCK = "/tmp/mpv-e11.sock"
TAG = "dlook-e11"


def hypr_clients():
    out = subprocess.run(["hyprctl", "clients", "-j"], capture_output=True, text=True).stdout
    return json.loads(out)


def find_window(tag):
    for c in hypr_clients():
        if tag in c.get("title", ""):
            x, y = c["at"]
            w, h = c["size"]
            return x, y, w, h
    return None


def shot(x, y, w, h, path):
    subprocess.run(["grim", "-g", f"{x},{y} {w}x{h}", path], check=True)
    return hashlib.md5(open(path, "rb").read()).hexdigest()[:10]


def ipc_cmd(sock_path, cmd, timeout=2.0):
    s = socket.socket(socket.AF_UNIX)
    s.settimeout(timeout)
    s.connect(sock_path)
    s.sendall((json.dumps({"command": cmd}) + "\n").encode())
    time.sleep(0.1)
    s.setblocking(False)
    try:
        data = s.recv(65536).decode(errors="replace")
    except BlockingIOError:
        data = ""
    s.close()
    return data


def main():
    for p in (SOCK,):
        if os.path.exists(p):
            os.unlink(p)

    proc = subprocess.Popen(
        ["foot", "-a", TAG, "-T", TAG, "--",
         "mpv", "--vo=sixel", "--really-quiet", "--loop",
         f"--input-ipc-server={SOCK}", f"{EXP}/test-video.mp4"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        stdin=subprocess.DEVNULL, start_new_session=True)

    # 等窗口 + socket
    deadline = time.time() + 15
    geo = sock_ok = None
    while time.time() < deadline:
        geo = find_window(TAG)
        sock_ok = os.path.exists(SOCK)
        if geo and sock_ok:
            break
        time.sleep(0.4)
    if not geo or not sock_ok:
        print(f"FAIL: window={geo} socket={sock_ok}")
        proc.terminate()
        return 1
    x, y, w, h = geo
    print(f"window at ({x},{y}) {w}x{h}, IPC socket ready")
    time.sleep(2.0)  # 让视频播起来

    # --- 播放中:连拍应为不同画面 ---
    playing = [shot(x, y, w, h, f"/tmp/e11-play-{i}.png") for i in range(5)]
    uniq_playing = len(set(playing))
    print(f"[playing] 5 shots → {uniq_playing} unique  {playing}")

    # --- IPC 暂停 → 连拍应为同一画面 ---
    print("[ipc] set pause=true:", ipc_cmd(SOCK, ["set_property", "pause", True])[:60].strip())
    time.sleep(1.0)
    paused = [shot(x, y, w, h, f"/tmp/e11-pause-{i}.png") for i in range(5)]
    uniq_paused = len(set(paused))
    print(f"[paused ] 5 shots → {uniq_paused} unique  {paused}")

    # --- 恢复播放 → 应再次变化 ---
    ipc_cmd(SOCK, ["set_property", "pause", False])
    time.sleep(0.8)
    resumed = [shot(x, y, w, h, f"/tmp/e11-resume-{i}.png") for i in range(5)]
    uniq_resumed = len(set(resumed))
    print(f"[resumed] 5 shots → {uniq_resumed} unique  {resumed}")

    # --- seek 断言:跳转到 2.5s,时间码区域应变化 ---
    before_seek = shot(x, y, w, h, "/tmp/e11-seek-before.png")
    ipc_cmd(SOCK, ["seek", 2.5, "absolute"])
    time.sleep(0.5)
    after_seek = shot(x, y, w, h, "/tmp/e11-seek-after.png")
    print(f"[seek   ] before={before_seek} after={after_seek} changed={before_seek != after_seek}")

    verdict = (
        uniq_playing >= 4          # 播放中画面在变
        and uniq_paused == 1       # 暂停后画面冻结
        and uniq_resumed >= 3      # 恢复后又在变
    )
    print()
    print("VERDICT:", "PASS — 截图断言可判定播放/暂停/恢复状态" if verdict else "FAIL — 区分度不足")
    ipc_cmd(SOCK, ["quit"])
    time.sleep(0.5)
    proc.terminate()
    return 0 if verdict else 1


if __name__ == "__main__":
    sys.exit(main())
