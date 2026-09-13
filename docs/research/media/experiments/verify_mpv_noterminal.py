#!/usr/bin/env python3
"""裁决 dlook 研究「关键未知1/2」:mpv --no-terminal 下 vo=kitty 是否仍输出帧;退出/resize 是否发 _Ga=d 清全部图像。"""
import os, pty, select, subprocess, sys, tempfile, time, json, socket

MP4 = os.path.join(os.path.dirname(os.path.abspath(__file__)), "test-video.mp4")
sock = tempfile.mktemp(prefix="dlook-rev-")

# 变体A: 研究推荐的完整参数(--no-terminal)
cmdA = ["mpv", "--vo=kitty", "--really-quiet", "--no-terminal",
        "--vo-kitty-alt-screen=no", "--vo-kitty-config-clear=no",
        f"--input-ipc-server={sock}", "--frames=10", MP4]
# 变体B: 对照,不带 --no-terminal(仅 --really-quiet)
cmdB = ["mpv", "--vo=kitty", "--really-quiet",
        "--vo-kitty-alt-screen=no", "--vo-kitty-config-clear=no",
        f"--input-ipc-server={sock}", "--frames=10", MP4]

def run(cmd, label):
    pid, fd = pty.fork()
    if pid == 0:
        os.environ["TERM"] = "xterm-kitty"
        os.execvp(cmd[0], cmd)
    buf = b""
    t0 = time.time()
    while time.time() - t0 < 30:
        r, _, _ = select.select([fd], [], [], 1.0)
        if r:
            try:
                chunk = os.read(fd, 1 << 16)
            except OSError:
                break
            if not chunk:
                break
            buf += chunk
    try:
        os.close(fd)
    except OSError:
        pass
    _, status = os.waitpid(pid, 0)
    gs = buf.count(b"_G")
    ad = buf.count(b"a=d")  # 清除全部 kitty 图像
    print(f"[{label}] bytes={len(buf)} _G_cmds={gs} a=d_count={ad} exit={os.waitstatus_to_exitcode(status)}")
    print(f"[{label}] payload_markers: m=1:{b'm=1' in buf} f=24:{b'f=24' in buf} f=100:{b'f=100' in buf}")
    # 提取前 120 字节的转义序列样例
    idx = buf.find(b"\x1b_G")
    if idx >= 0:
        print(f"[{label}] first_G_seq={buf[idx:idx+80]!r}")
    return buf

run(cmdB, "B:quiet-only")
run(cmdA, "A:no-terminal")

# IPC 冒烟:变体A运行中用另一个进程验证 pause/seek 可用性(通过 socket 文件)
sock2 = tempfile.mktemp(prefix="dlook-rev2-")
cmdC = ["mpv", "--vo=kitty", "--really-quiet", "--no-terminal",
        "--vo-kitty-alt-screen=no", "--vo-kitty-config-clear=no",
        f"--input-ipc-server={sock2}", "--pause=yes", MP4]
pid = subprocess.Popen(cmdC, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                       env={**os.environ, "TERM": "xterm-kitty"})
for _ in range(50):
    if os.path.exists(sock2):
        break
    time.sleep(0.1)
try:
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(sock2)
    s.sendall(b'{"command":["set_property","pause",false]}\n')
    print("[C:ipc-under-no-terminal]", s.recv(4096).decode().strip())
    s.sendall(b'{"command":["seek","1","absolute"]}\n')
    print("[C:ipc-seek]", s.recv(4096).decode().strip())
    s.close()
except Exception as e:
    print("[C:ipc] FAILED:", e)
finally:
    pid.terminate()
    pid.wait()
    try: os.unlink(sock2)
    except OSError: pass
    try: os.unlink(sock)
    except OSError: pass
