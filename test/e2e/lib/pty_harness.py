#!/usr/bin/env python3
"""
Pty harness for look E2E acceptance tests.

Spawns a process in a real pseudo-terminal (isTTY=true, raw mode, alt-screen
all work), feeds output into a pyte terminal-emulator screen for text/position
assertions, and keeps a raw byte log for ANSI color assertions.

Replaces tmux (unavailable in this environment) with equivalent fidelity:
  - real pty => process.stdout.isTTY === true
  - send keys by writing bytes to the pty master
  - capture "screen" via pyte (parses ANSI positioning / alt-screen)
  - capture raw bytes for truecolor (\x1b[38;2;) assertions
  - exit code via waitpid
"""
import os
import pty
import struct
import fcntl
import termios
import time
import select
import re
import signal
import threading

import pyte

# pyte 0.8.x omits the SU (ESC[<n>S) and SD (ESC[<n>T) scroll commands that
# vue-tui's stdout renderer relies on for optimized scrolling. Patch them in.
from pyte.screens import Screen
from pyte.streams import Stream


def _scroll_up(self, count=None):
    count = int(count or 1)
    if count < 1:
        count = 1
    top, bottom = self.margins or (0, self.lines - 1)
    if top >= bottom:
        return
    self.dirty.update(range(self.lines))
    for y in range(top, bottom - count + 1):
        if y + count in self.buffer:
            self.buffer[y] = self.buffer.pop(y + count)
        else:
            self.buffer.pop(y, None)
    for y in range(bottom - count + 1, bottom + 1):
        self.buffer.pop(y, None)


def _scroll_down(self, count=None):
    count = int(count or 1)
    if count < 1:
        count = 1
    top, bottom = self.margins or (0, self.lines - 1)
    if top >= bottom:
        return
    self.dirty.update(range(self.lines))
    for y in range(bottom, top + count - 1, -1):
        if y - count in self.buffer:
            self.buffer[y] = self.buffer.pop(y - count)
        else:
            self.buffer.pop(y, None)
    for y in range(top, top + count):
        self.buffer.pop(y, None)


Screen.scroll_up = _scroll_up
Screen.scroll_down = _scroll_down
Stream.csi["S"] = "scroll_up"
Stream.csi["T"] = "scroll_down"

# pyte 0.8.x `display` raises IndexError when a wide-char stub cell ("") is left
# orphaned — which happens whenever a renderer writes a single-width cell over the
# first half of a CJK/wide glyph (ratatui does this when a media bar row is
# replaced by body text). Real terminals show a space in the stub column; mirror
# that instead of crashing the harness.
from wcwidth import wcwidth as _wcwidth  # noqa: E402


def _display(self):
    def render(line):
        is_wide = False
        for x in range(self.columns):
            if is_wide:  # skip stub column of a wide glyph
                is_wide = False
                continue
            char = line[x].data
            if not char:
                yield " "
                continue
            is_wide = _wcwidth(char[0]) == 2
            yield char

    return ["".join(render(self.buffer[y])) for y in range(self.lines)]


Screen.display = property(_display)


# pyte 0.8.x has no alternate-screen buffer support (modes 47/1047/1049).
# Patch set_mode / reset_mode to swap self.buffer between main and alt.
import copy as _copy
from collections import defaultdict as _dd

_orig_set_mode = Screen.set_mode
_orig_reset_mode = Screen.reset_mode
_ALT_MODES = {47, 1047, 1049}


def _enter_alt(self):
    if getattr(self, "_in_alt", False):
        return
    self._in_alt = True
    self._main_buffer = self.buffer
    self._main_cursor = _copy.copy(self.cursor)
    self.buffer = _dd(self.buffer.default_factory)
    self.cursor.x = 0
    self.cursor.y = 0
    self.dirty.update(range(self.lines))


def _exit_alt(self):
    if not getattr(self, "_in_alt", False):
        return
    self._in_alt = False
    self.buffer = self._main_buffer
    if getattr(self, "_main_cursor", None) is not None:
        self.cursor = self._main_cursor
    self.dirty.update(range(self.lines))


def _patched_set_mode(self, *modes, **kwargs):
    _orig_set_mode(self, *modes, **kwargs)
    if kwargs.get("private"):
        for m in modes:
            if m in _ALT_MODES:
                _enter_alt(self)
                break


def _patched_reset_mode(self, *modes, **kwargs):
    _orig_reset_mode(self, *modes, **kwargs)
    if kwargs.get("private"):
        for m in modes:
            if m in _ALT_MODES:
                _exit_alt(self)
                break


Screen.set_mode = _patched_set_mode
Screen.reset_mode = _patched_reset_mode

# Named keys -> byte sequences understood by vue-tui's stdin parser.
KEYS = {
    "Up": "\x1b[A",
    "Down": "\x1b[B",
    "Right": "\x1b[C",
    "Left": "\x1b[D",
    "Home": "\x1b[H",
    "End": "\x1b[F",
    "PageUp": "\x1b[5~",
    "PageDown": "\x1b[6~",
    "Escape": "\x1b",
    "Enter": "\r",
    "Tab": "\t",
    "Space": " ",
    "Backspace": "\x7f",
    "C-c": "\x03",
}


class PtySession:
    def __init__(self, argv, cols=80, rows=24, env=None, cwd=None,
                 feed_screen=True):
        """`feed_screen=False` 只收集原始字节、不喂 pyte。

        图形协议（sixel/kitty）的视频流每秒可达数十 MB，pyte 是纯 Python 解析器，
        全量喂它会让读取线程跟不上 pty 写入速度 → 内核 pty 缓冲填满 → 被测进程被
        背压阻塞 → 观察到的行为失真（实测 kitty 视频只测到 2 帧/2s，真实是 28fps）。
        断言这类流量应直接用 self.raw。
        """
        self.cols = cols
        self.rows = rows
        self.argv = argv
        self.env = dict(env or os.environ)
        self.cwd = cwd or os.getcwd()
        self.feed_screen = feed_screen
        self.screen = pyte.Screen(cols, rows)
        self.stream = pyte.Stream(self.screen)
        self.raw = bytearray()
        self._exit_code = None
        self._stop = threading.Event()
        self.pid = None
        self.fd = None
        self._thread = None

    def start(self):
        pid, fd = os.forkpty()
        if pid == 0:
            # child
            try:
                fcntl.ioctl(1, termios.TIOCSWINSZ,
                            struct.pack("HHHH", self.rows, self.cols, 0, 0))
            except Exception:
                pass
            try:
                os.chdir(self.cwd)
            except Exception:
                pass
            try:
                os.execvpe(self.argv[0], self.argv, self.env)
            except Exception:
                os._exit(127)
        # parent
        self.pid = pid
        self.fd = fd
        self._set_winsize(self.rows, self.cols)
        os.set_blocking(fd, False)
        self._thread = threading.Thread(target=self._read_loop, daemon=True)
        self._thread.start()

    def _set_winsize(self, rows, cols):
        try:
            fcntl.ioctl(self.fd, termios.TIOCSWINSZ,
                        struct.pack("HHHH", rows, cols, 0, 0))
        except Exception:
            pass

    def _read_loop(self):
        while not self._stop.is_set():
            try:
                ready, _, _ = select.select([self.fd], [], [], 0.05)
            except (OSError, ValueError):
                break
            if not ready:
                if self._exit_code is None:
                    self._reap(block=False)
                continue
            # 一次唤醒读干 pty 缓冲：图形协议场景（视频）单帧可达 MB 级，若每次只读
            # 64KB 就跟不上写入速度，pty 缓冲填满 → 被测进程被背压阻塞 → 观察到的行为
            # 失真（实测 kitty 视频只测到 2 帧/2s，而真实是 28fps）。
            drained = False
            while True:
                try:
                    data = os.read(self.fd, 262144)
                except (BlockingIOError, InterruptedError):
                    break
                except OSError:
                    drained = True
                    break
                if not data:
                    drained = True
                    break
                self.raw.extend(data)
                if self.feed_screen:
                    try:
                        self.stream.feed(data.decode("utf-8", errors="replace"))
                    except Exception:
                        pass
            if drained:
                break
            continue
            # --- 旧路径（保留为不可达，避免误改行为）---
            try:
                data = os.read(self.fd, 65536)
            except OSError:
                break
            if not data:
                break
            self.raw.extend(data)
            try:
                text = data.decode("utf-8", errors="replace")
                self.stream.feed(text)
            except Exception:
                pass
        self._reap(block=True)

    def _reap(self, block):
        if self._exit_code is not None:
            return
        try:
            flags = 0 if block else os.WNOHANG
            pid, status = os.waitpid(self.pid, flags)
            if pid == self.pid:
                if os.WIFEXITED(status):
                    self._exit_code = os.WEXITSTATUS(status)
                elif os.WIFSIGNALED(status):
                    self._exit_code = 128 + os.WTERMSIG(status)
        except ChildProcessError:
            self._exit_code = -1

    # ---- public API ----
    def send(self, s):
        if isinstance(s, str):
            s = s.encode("utf-8")
        os.write(self.fd, s)

    def send_key(self, name):
        self.send(KEYS.get(name, name))

    def send_keys(self, name, times):
        for _ in range(times):
            self.send_key(name)

    def feed(self, duration=0.25):
        time.sleep(duration)

    def lines(self):
        return [line.rstrip() for line in self.screen.display]

    def screen_text(self):
        return "\n".join(self.lines())

    def row(self, n):
        """1-indexed pane row (top = 1)."""
        lines = self.lines()
        if 1 <= n <= len(lines):
            return lines[n - 1]
        return ""

    def raw_text(self):
        return self.raw.decode("utf-8", errors="replace")

    def has_truecolor(self):
        return b"\x1b[38;2;" in self.raw

    def wait_for(self, pattern, timeout=5.0):
        rx = re.compile(pattern)
        deadline = time.time() + timeout
        while time.time() < deadline:
            if rx.search(self.screen_text()):
                return True
            time.sleep(0.05)
        return False

    def wait_for_raw(self, pattern, timeout=5.0):
        rx = re.compile(pattern)
        deadline = time.time() + timeout
        while time.time() < deadline:
            if rx.search(self.raw_text()):
                return True
            time.sleep(0.05)
        return False

    def wait_exit(self, timeout=5.0):
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self._exit_code is not None:
                return self._exit_code
            time.sleep(0.05)
        return self._exit_code

    def resize(self, cols, rows):
        self.cols, self.rows = cols, rows
        self.screen.resize(rows, cols)
        self._set_winsize(rows, cols)
        time.sleep(0.2)

    def close(self):
        self._stop.set()
        if self._exit_code is None:
            try:
                os.kill(self.pid, signal.SIGKILL)
            except Exception:
                pass
        try:
            os.close(self.fd)
        except Exception:
            pass
        self._reap(block=True)


def run_shell(shell_cmd, cols=80, rows=24, cwd=None, env=None):
    """Run `bash -c shell_cmd` in a pty. Useful for sequencing echo + look + echo EXIT:$?."""
    e = dict(env or os.environ)
    e.setdefault("PS1", "")
    e.setdefault("PS2", "")
    e.setdefault("TERM", "xterm-256color")
    argv = ["bash", "--norc", "--noprofile", "-c", shell_cmd]
    s = PtySession(argv, cols=cols, rows=rows, env=e, cwd=cwd)
    s.start()
    return s
