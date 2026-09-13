#!/usr/bin/env python3
"""E12: 音视频联合录制验证 —— 视频播放期间同时录屏(帧变化)与录音(样本到达)。

证明:一次真实播放会话可被完整录制,并从中断言
  ① 画面在动(帧哈希变化率) ② 有真实音频输出(monitor 录制 RMS/频率)
"""
import hashlib
import json
import math
import os
import socket
import struct
import subprocess
import sys
import time
import wave

EXP = "/mnt/data/dlook/docs/research/media/experiments"
SOCK = "/tmp/mpv-e12.sock"
TAG = "dlook-e12"
AUDIO = "/tmp/e12-audio.wav"


def sh(cmd, **kw):
    return subprocess.run(cmd, capture_output=True, text=True, **kw)


def find_window(tag):
    out = sh(["hyprctl", "clients", "-j"]).stdout
    for c in json.loads(out):
        if tag in c.get("title", ""):
            x, y = c["at"]
            w, h = c["size"]
            return x, y, w, h
    return None


def shot(x, y, w, h, path):
    sh(["grim", "-g", f"{x},{y} {w}x{h}", path])
    return hashlib.md5(open(path, "rb").read()).hexdigest()[:10]


def ipc(path, cmd):
    s = socket.socket(socket.AF_UNIX)
    s.settimeout(2)
    s.connect(path)
    s.sendall((json.dumps({"command": cmd}) + "\n").encode())
    time.sleep(0.1)
    s.setblocking(False)
    try:
        r = s.recv(65536).decode(errors="replace")
    except BlockingIOError:
        r = ""
    s.close()
    return r


def audio_stats(path):
    w = wave.open(path)
    frames = w.readframes(w.getnframes())
    n = w.getnframes()
    samples = struct.unpack(f"<{len(frames)//2}h", frames)
    mono = samples[::w.getnchannels()]
    rms = math.sqrt(sum(v * v for v in mono) / max(len(mono), 1))
    return n, w.getframerate(), rms, mono


def goertzel(samples, freq, rate):
    k = 2 * math.pi * freq / rate
    s1 = s2 = 0.0
    for v in samples[:rate]:  # 前 1 秒足够
        s0 = v + 2 * math.cos(k) * s1 - s2
        s2, s1 = s1, s0
    return s1 * s1 + s2 * s2 - 2 * math.cos(k) * s1 * s2


def main():
    for p in (SOCK, AUDIO):
        if os.path.exists(p):
            os.unlink(p)

    # 视频文件带 440Hz 正弦音轨(E1 生成时即含 sine)
    proc = subprocess.Popen(
        ["foot", "-a", TAG, "-T", TAG, "--",
         "mpv", "--vo=sixel", "--really-quiet", "--loop", "--volume=100",
         f"--input-ipc-server={SOCK}", f"{EXP}/test-video.mp4"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        stdin=subprocess.DEVNULL, start_new_session=True)

    deadline = time.time() + 15
    geo = None
    while time.time() < deadline:
        geo = find_window(TAG)
        if geo and os.path.exists(SOCK):
            break
        time.sleep(0.4)
    if not geo:
        print("FAIL: window not found")
        proc.terminate()
        return 1
    x, y, w, h = geo
    print(f"window ({x},{y}) {w}x{h}")
    time.sleep(1.5)

    # 同步:录音(monitor) + 连拍
    rec = subprocess.Popen(
        ["ffmpeg", "-hide_banner", "-loglevel", "error", "-y",
         "-f", "pulse", "-i", "@DEFAULT_MONITOR@", "-t", "3", AUDIO],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(0.4)
    hashes = [shot(x, y, w, h, f"/tmp/e12-{i}.png") for i in range(8)]
    rec.wait(timeout=15)

    uniq = len(set(hashes))
    print(f"[video] 8 shots → {uniq} unique frames")

    n, rate, rms, mono = audio_stats(AUDIO)
    p440 = goertzel(mono, 440, rate)
    print(f"[audio] captured {n} frames @ {rate}Hz, RMS={rms:.0f}, 440Hz power={p440:.3e}")

    ok_video = uniq >= 6
    ok_audio = rms > 200 and p440 > 1e9
    print()
    print(f"VERDICT: video={'PASS' if ok_video else 'FAIL'}, audio={'PASS' if ok_audio else 'FAIL'}")
    ipc(SOCK, ["quit"])
    time.sleep(0.5)
    proc.terminate()
    return 0 if (ok_video and ok_audio) else 1


if __name__ == "__main__":
    sys.exit(main())
