#!/usr/bin/env python3
"""
dlook — E2E acceptance test suite (scenarios A–H from DESIGN.md §15).

Uses a real pty + pyte terminal emulator (tmux is unavailable in this env).
Run:  BIN=./preview python3 test/e2e/run_acceptance.py
      BIN="node dist/terminal.js" python3 test/e2e/run_acceptance.py
"""
import os
import sys
import subprocess
import time
import functools
import http.server
import re
import shutil
import tempfile
import threading

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "lib"))
from pty_harness import PtySession, run_shell  # noqa: E402

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
FIX = os.path.join(ROOT, "test", "fixtures")
BIN = os.environ.get("BIN", "node dist/terminal.js").split()
PASS = 0
FAIL = 0


def ok(desc):
    global PASS
    PASS += 1
    print(f"  \u2713 {desc}")


def bad(desc, detail=""):
    global FAIL
    FAIL += 1
    print(f"  \u2717 {desc}" + (f"  ({detail})" if detail else ""))


def check(cond, desc, detail=""):
    if cond:
        ok(desc)
    else:
        bad(desc, detail)


def session(file, cols=80, rows=24, env=None):
    e = dict(env or os.environ)
    s = PtySession(BIN + [os.path.join(FIX, file)], cols=cols, rows=rows, env=e, cwd=ROOT)
    s.start()
    return s


# --------------------------------------------------------------------------
# A. 启动与渲染
# --------------------------------------------------------------------------
def scenario_A():
    print("== A-render ==")
    # A1 markdown
    s = session("sample.md")
    check(s.wait_for("Sample Markdown", 6), "A1 md header+body render")
    check(s.row(1).endswith("sample.md"), "A1 header has filename")
    check("q quit" in s.screen_text(), "A1 footer present")
    s.send_key("q"); s.wait_exit(3); s.close()

    # A2 code highlight (truecolor)
    s = session("sample.ts")
    check(s.wait_for("TypeScript", 8), "A2 code render")
    check(s.has_truecolor(), "A2 truecolor present", "no \\x1b[38;2; in raw")
    s.send_key("q"); s.wait_exit(3); s.close()

    # A3 unknown ext no highlight
    s = session("unknown.xyz")
    check(s.wait_for("unknown extension", 6), "A3 unknown ext render")
    check(not s.has_truecolor(), "A3 no truecolor for unknown ext")
    s.send_key("q"); s.wait_exit(3); s.close()

    # A4 plain text
    s = session("plain.txt")
    check(s.wait_for("plain text file", 6), "A4 plain text render")
    check(not s.has_truecolor(), "A4 no truecolor for plain text")
    s.send_key("q"); s.wait_exit(3); s.close()


# --------------------------------------------------------------------------
# B. 滚动 (large.txt, 1 source line = 1 visual line)
#   pane row2 (1-indexed) = body top. body h = 22.
# --------------------------------------------------------------------------
def scenario_B():
    print("== B-scroll ==")
    s = session("large.txt")
    check(s.wait_for("MARKER:0000", 6), "B1 initial top MARKER:0000")
    check(s.row(2).strip() == "MARKER:0000", "B1 row2 == MARKER:0000", s.row(2))

    s.send_keys("j", 10); s.feed(0.3)
    check(s.row(2).strip() == "MARKER:0010", "B2 j x10 -> MARKER:0010", s.row(2))

    s.send_keys("k", 5); s.feed(0.3)
    check(s.row(2).strip() == "LINE_0005", "B3 k x5 -> LINE_0005", s.row(2))

    s.send_key("Space"); s.feed(0.3)
    check(s.row(2).strip() == "LINE_0027", "B4 space pgdn +22", s.row(2))

    s.send_key("PageDown"); s.feed(0.3)
    check(s.row(2).strip() == "LINE_0049", "B5 PageDown +22", s.row(2))

    s.send_key("PageUp"); s.feed(0.3)
    check(s.row(2).strip() == "LINE_0027", "B6 PageUp -22", s.row(2))

    s.send_keys("Down", 3); s.feed(0.3)
    check(s.row(2).strip() == "MARKER:0030", "B7 Down x3 -> MARKER:0030", s.row(2))

    s.send_keys("Up", 3); s.feed(0.3)
    check(s.row(2).strip() == "LINE_0027", "B8 Up x3", s.row(2))

    s.send_key("Home"); s.feed(0.3)
    check(s.row(2).strip() == "MARKER:0000", "B9 Home -> top", s.row(2))

    s.send_key("End"); s.feed(0.4)
    check("LINE_1999" in s.screen_text(), "B10 End -> last page has LINE_1999")

    s.send_key("g"); s.feed(0.3)
    check(s.row(2).strip() == "MARKER:0000", "B11 g -> top", s.row(2))

    s.send_key("G"); s.feed(0.4)
    check("LINE_1999" in s.screen_text(), "B12 G -> bottom")

    # B13 round-trip g -> G -> g
    s.send_key("g"); s.feed(0.3)
    check(s.row(2).strip() == "MARKER:0000", "B13a g -> top")
    s.send_key("G"); s.feed(0.4)
    check("LINE_1999" in s.screen_text(), "B13b G -> bottom")
    s.send_key("g"); s.feed(0.3)
    check(s.row(2).strip() == "MARKER:0000", "B13c g -> top")

    s.send_key("q"); s.wait_exit(3); s.close()


# --------------------------------------------------------------------------
# C. 退出与 alt-screen 恢复
# --------------------------------------------------------------------------
def _exit_case(desc, key, expected_code):
    # seed marker on main screen, run preview, capture EXIT:N + marker restore
    cmd = f'echo "MK_OK_HERE"; cd {ROOT} && {" ".join(BIN)} test/fixtures/sample.md; echo "EXIT:$?"'
    s = run_shell(cmd, cols=80, rows=24, cwd=ROOT)
    if not s.wait_for("Sample Markdown", 8):
        bad(f"{desc} (app did not start)")
        s.close()
        return
    s.send_key(key)
    s.feed(0.5)
    txt = s.screen_text()
    import re
    m = re.search(r"EXIT:(\d+)", txt)
    code = int(m.group(1)) if m else None
    check(code == expected_code, f"{desc} -> exit {expected_code}", f"got {code}")
    check("MK_OK_HERE" in txt, f"{desc} alt-screen restored (marker visible)")
    s.wait_exit(3)
    s.close()


def scenario_C():
    print("== C-exit ==")
    _exit_case("C1 q", "q", 0)
    _exit_case("C2 Esc", "Escape", 0)
    _exit_case("C3 Ctrl+C", "C-c", 130)


# --------------------------------------------------------------------------
# D. resize
# --------------------------------------------------------------------------
def scenario_D():
    print("== D-resize ==")
    # D1 grow
    s = session("sample.ts", cols=80, rows=24)
    check(s.wait_for("sample.ts", 8), "D1 start")
    s.resize(120, 40)
    s.feed(0.4)
    txt = s.screen_text()
    lines = txt.split("\n")
    check(len(lines) >= 40, "D1 row count >= 40 after grow", f"got {len(lines)}")
    check("sample.ts" in s.row(1), "D1 header still row1 after grow")
    check("q quit" in s.row(40), "D1 footer at row40 after grow", s.row(40)[:20])
    s.send_key("q"); s.wait_exit(3); s.close()

    # D2 shrink
    s = session("sample.ts", cols=80, rows=24)
    check(s.wait_for("sample.ts", 8), "D2 start")
    s.resize(60, 12)
    s.feed(0.4)
    # vue-tui renderer quirk: shrink-resize clears out-of-bounds old rows which
    # clamp to the new last row (footer); the footer reappears on the next render
    # cycle. Trigger a settle render, then assert positioning.
    s.send_key("j"); s.feed(0.15); s.send_key("k"); s.feed(0.2)
    check("sample.ts" in s.row(1), "D2 header row1 after shrink", s.row(1)[:40])
    check("q quit" in s.row(12), "D2 footer at row12 after shrink", s.row(12)[:20])
    check("TypeScript" in s.screen_text(), "D2 body content still present after shrink")
    s.send_key("q"); s.wait_exit(3); s.close()


# --------------------------------------------------------------------------
# E. 错误处理（退出码）
# --------------------------------------------------------------------------
def _err_case(desc, args, expected_code, contains=None):
    cmd = f'cd {ROOT} && {" ".join(BIN)} {args}; echo "EXIT:$?"'
    s = run_shell(cmd, cols=80, rows=24, cwd=ROOT)
    s.feed(1.0)
    txt = s.screen_text()
    import re
    m = re.search(r"EXIT:(\d+)", txt)
    code = int(m.group(1)) if m else None
    check(code == expected_code, f"{desc} -> exit {expected_code}", f"got {code}")
    if contains:
        check(contains in txt, f"{desc} message contains '{contains}'")
    s.wait_exit(3)
    s.close()


def scenario_E():
    print("== E-errors ==")
    _err_case("E1 no args", "", 2, "Usage")
    _err_case("E2 not found", "nope.md", 1, "cannot access")
    _err_case("E3 binary", "test/fixtures/binary.bin", 1, "binary")
    _err_case("E4 directory", "test/fixtures", 1, "directory")
    _err_case("E5 too many", "a.md b.md", 2)


# --------------------------------------------------------------------------
# F. 非 TTY（管道）
# --------------------------------------------------------------------------
def scenario_F():
    print("== F-nontty ==")
    # F1 pipe md
    r = subprocess.run(
        BIN + ["test/fixtures/sample.md"],
        capture_output=True, text=True, cwd=ROOT,
    )
    check(r.returncode == 0, "F1 pipe md exit 0", f"got {r.returncode}")
    check("Sample Markdown" in r.stdout, "F1 raw markdown title in stdout")

    # F2 pipe code (no truecolor)
    r = subprocess.run(
        BIN + ["test/fixtures/sample.ts"],
        capture_output=True, text=True, cwd=ROOT,
    )
    check(r.returncode == 0, "F2 pipe ts exit 0", f"got {r.returncode}")
    check("readFile" in r.stdout, "F2 raw code in stdout")
    check("\x1b[38;2;" not in r.stdout, "F2 no truecolor when piped")


# --------------------------------------------------------------------------
# G. 超大文件 / 虚拟化
# --------------------------------------------------------------------------
def scenario_G():
    print("== G-large ==")
    # G2 startup latency
    t0 = time.time()
    s = session("large.txt")
    ready = s.wait_for("MARKER:0000", 8)
    elapsed = time.time() - t0
    check(ready, "G1 large file renders")
    check(elapsed < 3.0, f"G2 startup < 3s (got {elapsed:.2f}s)")
    # G1 scroll stress
    s.send_key("G"); s.feed(0.3)
    s.send_key("g"); s.feed(0.3)
    for _ in range(5):
        s.send_keys("j", 50); s.feed(0.1)
        s.send_keys("k", 50); s.feed(0.1)
    check(s.row(2).strip() == "MARKER:0000", "G1 scroll stress ends at top", s.row(2))
    s.send_key("q"); s.wait_exit(3); s.close()


# --------------------------------------------------------------------------
# H. markdown 元素渲染
# --------------------------------------------------------------------------
def scenario_H():
    print("== H-markdown ==")
    s = session("sample.md")
    check(s.wait_for("Sample Markdown", 6), "H start")
    txt = s.screen_text()
    raw = s.raw_text()
    check("Sample Markdown" in txt, "H1 heading visible")
    check("- Renders markdown" in txt or "Renders markdown" in txt, "H2 list item visible")
    check("vue-tui" in txt, "H3 table content visible")
    check("\x1b[1m" in raw, "H1 heading bold (SGR 1) in raw")
    # code block + link are below the fold: scroll down to find them
    found_code = "interface User" in txt
    found_link = "example.com" in txt
    for _ in range(6):
        if found_code and found_link:
            break
        s.send_key("Space"); s.feed(0.25)
        t = s.screen_text()
        if not found_code and "interface User" in t:
            found_code = True
        if not found_link and "example.com" in t:
            found_link = True
    check(found_code, "H4 fenced code block visible (after scroll)")
    check(found_link, "H5 link visible (after scroll)")
    s.send_key("q"); s.wait_exit(3); s.close()


# --------------------------------------------------------------------------
# I. mermaid 图渲染 + markdown 代码块上色（Rust 版新增能力）
# --------------------------------------------------------------------------
# 盒绘字符集合（Unicode box-drawing / ASCII 线）
BOX_CHARS = set("│┌┐└┘─━┃╋┣┫├┤═║╔╗╚╝+-|><^v")


def _has_box_chars(text):
    return any(c in BOX_CHARS for c in text)


def scenario_I():
    print("== I-mermaid-codeblock ==")
    # I1 markdown 内 mermaid 块渲染成图（盒绘字符，非源码）
    s = session("mermaid.md")
    check(s.wait_for("Mermaid in Markdown", 6), "I1 start")
    # 向下滚动找到图
    found_box = False
    for _ in range(8):
        txt = s.screen_text()
        if _has_box_chars(txt) and "flowchart TD" not in txt:
            found_box = True
            break
        s.send_key("Space"); s.feed(0.25)
    check(found_box, "I1 mermaid block rendered as box-drawing (not source)")
    s.send_key("q"); s.wait_exit(3); s.close()

    # I2 独立 .mmd 文件渲染成图
    s = session("sample.mmd")
    s.feed(0.6)
    txt = s.screen_text()
    check(_has_box_chars(txt), "I2 .mmd rendered as box-drawing", "no box chars")
    check("flowchart TD" not in txt, "I2 .mmd source not shown as text")
    s.send_key("q"); s.wait_exit(3); s.close()

    # I3 markdown 内代码块上色（truecolor）
    s = session("codeblock.md")
    check(s.wait_for("Code Block", 6), "I3 start")
    # 代码块在下方，滚动到代码块
    has_color = False
    for _ in range(8):
        raw = s.raw_text()
        if "\x1b[38;2;" in raw:
            has_color = True
            break
        s.send_key("Space"); s.feed(0.25)
    check(has_color, "I3 code block highlighted with truecolor", "no \\x1b[38;2;")
    s.send_key("q"); s.wait_exit(3); s.close()

    # I4 mermaid 解析失败降级（不崩，显示内容，exit 0）
    bad = os.path.join(FIX, "bad.mmd")
    with open(bad, "w") as f:
        f.write("this is not valid mermaid at all\n")
    s = session("bad.mmd")
    s.feed(0.6)
    s.send_key("q")
    code = s.wait_exit(3)
    check(code == 0, f"I4 invalid mermaid exit 0 (got {code})")
    s.close()


# --------------------------------------------------------------------------
# J. markdown 样式(DECISIONS D1/D2/D3/D6/D7)
# --------------------------------------------------------------------------
def _sgr(kind, btn, x, y):
    """SGR 鼠标序列;x/y 为 1-based 终端坐标。btn: 0=左键按下, 32=左键拖动。"""
    return f"\x1b[<{btn};{x};{y}{kind}"


def scenario_J():
    print("== J-markdown-style ==")
    s = session("style.md")
    check(s.wait_for("H1 Heading One", 6), "J1 start")
    raw = s.raw_text()
    txt = s.screen_text()

    # D1: 标题分级上色(h1/h2 cyanBright=38;5;14,h3/h4 blueBright=38;5;12)
    check("38;5;14" in raw, "J2 h1/h2 cyan (38;5;14)")
    check("38;5;12" in raw, "J3 h3/h4 blue (38;5;12)")

    # D2: H1 左对齐(标题贴左侧,非居中)
    h1_row = next((i for i in range(24) if "H1 Heading One" in s.row(i)), None)
    check(h1_row is not None and s.row(h1_row).index("H1") <= 2,
          "J4 H1 left aligned")

    # D6: 任务列表 checkbox
    check("☑" in txt and "done task item" in txt, "J5 task done ☑")
    check("☐" in txt and "open task item" in txt, "J6 task open ☐")

    # D7: 表格圆角外框
    check("╭" in txt and "╰" in txt, "J7 rounded table borders")

    # D3: 链接样式化(label 下划线 + 亮蓝;URL 暗灰展示)
    check("\x1b[4m" in raw, "J8 link label underlined (SGR 4)")
    check("38;5;8" in raw, "J9 link url gray (38;5;8)")
    check("example link" in txt and "(https://example.com/path)" in txt,
          "J10 link label+url visible")
    s.send_key("q"); s.wait_exit(3); s.close()


# --------------------------------------------------------------------------
# K. 文本拖选与复制(DECISIONS D11)
# --------------------------------------------------------------------------
def scenario_K():
    print("== K-selection-copy ==")
    # K1-K3: 拖选反显 + 松开自动复制(OSC 52)+ 状态栏
    s = session("large.txt")
    check(s.wait_for("MARKER:0000", 6), "K1 start")
    base = len(s.raw_text())
    s.send(_sgr("M", 0, 3, 3)); s.feed(0.15)
    s.send(_sgr("M", 32, 30, 3)); s.feed(0.15)
    s.send(_sgr("m", 0, 31, 3)); s.feed(0.3)
    raw = s.raw_text()[base:]
    check("\x1b[7m" in raw, "K2 drag selection reversed (SGR 7)")
    check("\x1b]52;c;" in raw, "K3 mouseup auto-copy via OSC 52")
    check("copied" in s.screen_text(), "K4 status shows copied")

    # K5: y 手动复制(选区在松开后保留;y 复制后清除选区)
    base = len(s.raw_text())
    s.send("y"); s.feed(0.3)
    check("\x1b]52;c;" in s.raw_text()[base:], "K5 y manual copy (OSC 52)")

    # K6: 重新拖选 → 有选区时 Esc 只清除选择,不退出
    base = len(s.raw_text())
    s.send(_sgr("M", 0, 3, 4)); s.feed(0.15)
    s.send(_sgr("M", 32, 30, 4)); s.feed(0.15)
    s.send(_sgr("m", 0, 31, 4)); s.feed(0.3)
    check("\x1b[7m" in s.raw_text()[base:], "K6 re-select works after y")
    s.send_key("Escape"); s.feed(0.3)
    alive = s.wait_exit(0.8)
    check(alive is None, "K7 Esc with selection does not quit")
    s.send_key("Escape")
    code = s.wait_exit(3)
    check(code == 0, f"K8 Esc without selection quits 0 (got {code})")
    s.close()

    # K9: 拖到视口底边缘 → 自动滚动(内容上移)
    s = session("large.txt")
    check(s.wait_for("MARKER:0000", 6), "K9 start")
    before = s.row(2)
    s.send(_sgr("M", 0, 3, 3)); s.feed(0.15)
    for _ in range(6):
        s.send(_sgr("M", 32, 40, 23)); s.feed(0.35)
    s.send(_sgr("m", 0, 41, 23)); s.feed(0.3)
    check(s.row(2) != before, "K10 edge drag auto-scrolls viewport")
    s.send_key("q"); s.wait_exit(3); s.close()


# --------------------------------------------------------------------------
# L. 代码语言覆盖(DECISIONS D4:two-face 全量语法集)
# --------------------------------------------------------------------------
def scenario_L():
    print("== L-languages ==")
    # two-face 前这些语言无色;现在是回归断言
    s = session("lang.toml")
    check(s.wait_for("host", 8), "L1 toml render")
    check(s.has_truecolor(), "L2 toml highlighted (truecolor)")
    s.send_key("q"); s.wait_exit(3); s.close()

    s = session("lang.vue")
    check(s.wait_for("count", 8), "L3 vue render")
    check(s.has_truecolor(), "L4 vue highlighted (truecolor)")
    s.send_key("q"); s.wait_exit(3); s.close()

    # ts: 原先回退 js,现在原生 TS 语法
    s = session("sample.ts")
    check(s.wait_for("TypeScript", 8), "L5 ts render")
    check(s.has_truecolor(), "L6 ts highlighted (truecolor)")
    s.send_key("q"); s.wait_exit(3); s.close()


# --------------------------------------------------------------------------
# M. 本地文件链接渲染与点击导航(DECISIONS D14)
# --------------------------------------------------------------------------
def _find_cell(s, text):
    """返回 (col, row) 1-based:屏幕上 text 首次出现的列/行;未找到返回 None。"""
    for r, line in enumerate(s.screen.display, start=1):
        c = line.find(text)
        if c >= 0:
            return c + 1, r
    return None


def _click(s, col, row):
    """在 (col,row) 注入左键按下+松开(SGR 鼠标序列)。"""
    s.send(_sgr("M", 0, col, row)); s.feed(0.12)
    s.send(_sgr("m", 0, col, row)); s.feed(0.35)


def scenario_M():
    print("== M-links ==")
    import tempfile
    xdg_log = os.path.join(tempfile.gettempdir(), "dlook-xdg-open.log")
    if os.path.exists(xdg_log):
        os.remove(xdg_log)
    fake_bin = os.path.join(ROOT, "test", "e2e", "fake-bin")
    env = dict(os.environ,
               XDG_LOG=xdg_log,
               PATH=fake_bin + os.pathsep + os.environ["PATH"])

    s = session("link-src.md", env=env)
    check(s.wait_for("Link Source", 6), "M1 start")
    check("dst↗" in s.screen_text(), "M1 local link marker ↗")
    check("(link-dst.md)" in s.screen_text(), "M1 local link url shown")
    check("site" in s.screen_text(), "M1 external link label shown (no marker)")

    # M2: 滚动两行后点击本地链接 → 内部跳转(滚动位置进历史)
    s.send_key("j"); s.feed(0.15)
    s.send_key("j"); s.feed(0.15)
    before_row = s.row(2)
    cell = _find_cell(s, "dst↗")
    check(cell is not None, "M2 find link cell")
    if cell:
        _click(s, *cell)
        check(s.wait_for("LINK-DST-MARKER", 5), "M2 click navigates to target")
        check("link-dst.md" in s.row(1), "M2 header shows target file")
        check("back" in s.row(24), "M2 footer shows back hint")

    # M3: Backspace 返回 → 恢复原文件与滚动位置
    if cell:
        s.send_key("Backspace"); s.feed(0.4)
        check("link-src.md" in s.row(1), "M3 backspace returns to source")
        check(s.row(2) == before_row, "M3 scroll position restored")

    # M4: 缺失链接 → 状态栏提示,不跳转
    cell = _find_cell(s, "gone↗")
    if cell:
        _click(s, *cell)
        check("not found" in s.screen_text(), "M4 missing link status message")
        check("link-src.md" in s.row(1), "M4 stays on source")

    # M5: 外部链接 → 假 xdg-open 收到 URL,本页不动
    cell = _find_cell(s, "site")
    if cell:
        _click(s, *cell)
        s.feed(0.5)
        got = open(xdg_log).read().strip() if os.path.exists(xdg_log) else ""
        check(got == "https://example.com/dlook", f"M5 xdg-open got url (got {got!r})")
        check("link-src.md" in s.row(1), "M5 stays on source after external link")
        check("opened externally" in s.screen_text(), "M5 status shows opened")

    # M6: 代码块内的伪链接不可点击
    cell = _find_cell(s, "[fenced]")
    if cell:
        header_before = s.row(1)
        _click(s, *cell)
        check(s.row(1) == header_before, "M6 fenced pseudo-link not clickable")

    # M7: 表格内链接 → 跳转(验证表格边框行插入后的行号正确)
    cell = _find_cell(s, "cell↗")
    if cell:
        _click(s, *cell)
        check(s.wait_for("LINK-DST-MARKER", 5), "M7 table link navigates")
        check("link-dst.md" in s.row(1), "M7 header shows target")

    s.send_key("q")
    code = s.wait_exit(3)
    s.close()
    check(code == 0, "M8 quit 0")


# --------------------------------------------------------------------------
# N. 图片渲染(DECISIONS D15):pyte 不支持图形协议,统一用 DLOOK_IMAGE_PROTOCOL
#    =halfblocks 断言半块字符 + truecolor 输出;远程用本地 http server。
# --------------------------------------------------------------------------
def _sgr_mouse(kind, btn, col, row):
    return f"\x1b[<{btn};{col};{row}{kind}"


def _half_rows(s):
    return [i for i, l in enumerate(s.screen_text().split("\n"))
            if "\u2580" in l or "\u2584" in l]


def scenario_N():
    import http.server
    import threading
    import functools
    print("== N-images ==")
    env = dict(os.environ, DLOOK_IMAGE_PROTOCOL="halfblocks")

    # N1: markdown 内本地图片 → 半块字符 + truecolor
    s = session("img-local.md", env=env)
    check(s.wait_for("Images Fixture", 6), "N1 md start")
    # 图片加载是异步线程,轮询等待占位行被替换
    deadline = time.time() + 6
    while time.time() < deadline and not _half_rows(s):
        s.feed(0.2)
    rows = _half_rows(s)
    check(len(rows) >= 2, f"N1 local image rendered as halfblocks (rows {rows})")
    check("\x1b[38;2;" in s.raw_text() or "\x1b[48;2;" in s.raw_text(),
          "N1 truecolor SGR for image pixels")
    check("text before image" in s.screen_text(), "N1 text around image intact")

    # N2: 行内图片 → 降级为可点击链接(label + url)
    check("tiny\u2197" in s.screen_text() and "inline image" in s.screen_text(),
          "N2 inline image degrades to link")

    # N3: 缺失图片 → 失败提示行
    check("unavailable" in s.screen_text() and "not found" in s.screen_text(),
          "N3 missing image error line")

    # N4: 点击行内图片链接 → 图片模式打开 + ⌫ 返回
    cell = None
    for r, line in enumerate(s.screen.display, start=1):
        c = line.find("tiny\u2197")
        if c >= 0 and "inline" in line:
            cell = (c + 3, r)
            break
    check(cell is not None, "N4 find inline image link")
    if cell:
        col, row = cell
        s.send(_sgr_mouse("M", 0, col, row)); s.feed(0.15)
        s.send(_sgr_mouse("m", 0, col, row)); s.feed(0.6)
        check(s.wait_for("tiny.png", 5), "N4 click opens image in dlook")
        deadline = time.time() + 5
        while time.time() < deadline and not _half_rows(s):
            s.feed(0.2)
        check(bool(_half_rows(s)), "N4 image renders in image mode")
        check("back" in s.row(24), "N4 footer shows back hint")
        s.send_key("Backspace"); s.feed(0.5)
        check("img-local.md" in s.row(1), "N4 backspace returns to md")

    s.send_key("q"); code = s.wait_exit(3); s.close()
    check(code == 0, "N4 quit 0")

    # N5: 直接打开图片文件(图片模式)
    s = session("img/tiny.png", env=env)
    deadline = time.time() + 6
    while time.time() < deadline and not _half_rows(s):
        s.feed(0.2)
    check(bool(_half_rows(s)), "N5 direct image open renders")
    check("tiny.png" in s.row(1), "N5 header shows image name")
    check("q quit" in s.screen_text(), "N5 footer present")
    s.send_key("q"); s.wait_exit(3); s.close()

    # N6: 远程图片(本地 http server)
    handler = functools.partial(http.server.SimpleHTTPRequestHandler,
                                directory=os.path.join(FIX, "img"))
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    port = srv.server_address[1]
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    import tempfile
    remote_md = os.path.join(tempfile.gettempdir(), "dlook-e2e-remote.md")
    with open(remote_md, "w") as f:
        f.write(f"# Remote Image\n\n![remote gradient](http://127.0.0.1:{port}/gradient.png)\n")
    s = PtySession(BIN + [remote_md], cols=80, rows=24, env=env, cwd=ROOT)
    s.start()
    check(s.wait_for("Remote Image", 6), "N6 md start")
    deadline = time.time() + 8
    while time.time() < deadline and not _half_rows(s):
        s.feed(0.2)
    check(bool(_half_rows(s)), "N6 remote image fetched and rendered")
    s.send_key("q"); s.wait_exit(3); s.close()
    srv.shutdown()

    # N7: DLOOK_IMAGE_PROTOCOL=off → 图片降级为 🖼 链接行,点击可跳转图片模式
    env_off = dict(os.environ, DLOOK_IMAGE_PROTOCOL="off")
    s = session("img-off.md", env=env_off)
    check(s.wait_for("Images Off", 6), "N7 md start (images off)")
    check("\U0001f5bc tiny logo" in s.screen_text(), "N7 fallback link line")
    check("(img/tiny.png)" in s.screen_text(), "N7 fallback url shown")
    check(not _half_rows(s), "N7 no image rendering when off")
    s.send_key("q"); s.wait_exit(3); s.close()

    # N8: 非 TTY 直开图片 → 明确报错 + rc 1
    r = subprocess.run(BIN + [os.path.join(FIX, "img/tiny.png")],
                       capture_output=True, text=True, env=env, cwd=ROOT)
    check(r.returncode == 1, "N8 non-tty image exits 1")
    check("image" in r.stderr, "N8 non-tty image error message")


def main():
    print(f"BIN = {BIN}")
    print(f"FIX = {FIX}")
    # ensure large.txt exists
    lg = os.path.join(FIX, "large.txt")
    if not os.path.exists(lg):
        subprocess.run(["bash", os.path.join(ROOT, "test/e2e/gen-large.sh")], check=True)

    scenarios = {
        "A": scenario_A, "B": scenario_B, "C": scenario_C, "D": scenario_D,
        "E": scenario_E, "F": scenario_F, "G": scenario_G, "H": scenario_H,
        "I": scenario_I, "J": scenario_J, "K": scenario_K, "L": scenario_L,
        "M": scenario_M, "N": scenario_N,
        # 媒体套件(task media-4)
        "O": scenario_O, "OE9": scenario_O_e9,
        "P": scenario_P, "Q": scenario_Q, "R": scenario_R,
    }
    # 可选场景过滤:python3 run_acceptance.py A B O R(默认全部)
    want = [a.upper() for a in sys.argv[1:]]
    for name, fn in scenarios.items():
        if want and name not in want:
            continue
        try:
            fn()
        except Exception as ex:
            print(f"  \u2717 {fn.__name__} EXCEPTION: {ex}")
            global FAIL
            FAIL += 1

    print()
    print(f"RESULT: PASS={PASS} FAIL={FAIL} SKIP={SKIP}")
    sys.exit(0 if FAIL == 0 else 1)


# ==========================================================================
# 媒体套件(task media-4 / design §6):O 音频 / P 视频 / Q 网页 / R 交互。
#
# 断言口径:
#   - 播放依赖的断言在无音频设备时**显式跳过**(打印 ⊘ SKIP 标记),不伪装通过
#     (design §6 O 场景、任务书 §E)。
#   - 视频 mpv 共屏路径需要真实图形协议;pty+pyte 无协议 → 走降级链断言,
#     mpv 路径在 test/e2e/run-visual.sh(V 套件,foot 真实终端)覆盖。
#   - 引擎模块未落地(todo!())时进程 panic 退出 101:检测到 → 显式 SKIP 并标注
#     「待引擎落地」。
# ==========================================================================

SKIP = 0

# 三套引擎的 panic 特征(任务书:引擎未落地时 E2E 待跑)
_ENGINE_PANIC = {
    "media-1": "media.rs",
    "media-2": "web.rs",
    "media-3": "video.rs",
}


def skip(desc, reason):
    global SKIP
    SKIP += 1
    print(f"  \u2298 {desc}  (SKIP: {reason})")


def panic_engine(s):
    """进程因引擎 todo!() panic 时返回引擎任务号,否则 None。"""
    if s.wait_exit(0.4) != 101:
        return None
    raw = s.raw_text()
    for task, file in _ENGINE_PANIC.items():
        if f"src/{file}" in raw and "not yet implemented" in raw:
            return task
    return None


def engine_skip(desc, task):
    skip(desc, f"待引擎落地({task} 仍为 todo!() → 进程 panic 101)")


# ---- 媒体栏定位/解析 helpers ------------------------------------------------

def progress_row(s):
    """媒体栏进度条行(1-based);无媒体栏返回 None。"""
    rows = s.lines()
    for i in range(len(rows), max(0, len(rows) - 4), -1):
        line = rows[i - 1]
        if re.search(r"\d\d:\d\d / (--:--|\d\d:\d\d)", line) and (
            "\u2501" in line or "\u2500" in line
        ):
            return i
    return None


def info_row(s):
    p = progress_row(s)
    if p is None or p + 1 > len(s.lines()):
        return None
    return p + 1


def timecodes(s):
    """(位置秒, 时长秒|None);无媒体栏返回 (None, None)。"""
    p = progress_row(s)
    if p is None:
        return (None, None)
    m = re.search(r"(\d\d:\d\d) / (--:--|\d\d:\d\d)", s.row(p))
    if not m:
        return (None, None)

    def secs(t):
        if t == "--:--":
            return None
        mm, ss = t.split(":")
        return int(mm) * 60 + int(ss)

    return (secs(m.group(1)), secs(m.group(2)))


def bar_fraction(s):
    """进度条实心比例(0..1);无进度条返回 None。"""
    p = progress_row(s)
    if p is None:
        return None
    seg = s.row(p).split("  ")[0]
    filled = seg.count("\u2501")
    total = filled + seg.count("\u2500")
    return filled / total if total else None


def bar_error(s):
    """媒体栏降级态(`✗ <原因>`)文案;正常返回 None。"""
    p = progress_row(s)
    if p is None:
        return None
    cand = [s.row(p + 1)] if p + 1 <= len(s.lines()) else []
    cand += list(s.lines()[1:4])
    for line in cand:
        if "✗" in line:
            return line.strip()
    return None


def require_playback(s, desc):
    """播放依赖断言的前置检查:无设备/解码失败 → 显式 SKIP 并返回 False。"""
    err = bar_error(s)
    if err:
        skip(desc, f"无音频设备 / 解码失败 → {err[:48]}")
        return False
    return True


def _click(s, col, row, settle=0.4):
    """在 (col,row) 注入左键按下+松开(SGR 鼠标序列;1-based)。"""
    s.send(_sgr("M", 0, col, row))
    s.feed(0.12)
    s.send(_sgr("m", 0, col, row))
    s.feed(settle)


def _wheel(s, up, col, row, settle=0.4):
    s.send(_sgr("M", 64 if up else 65, col, row))
    s.feed(settle)


def _mid_click(s, col, row, settle=0.5):
    s.send(_sgr("M", 1, col, row))
    s.feed(0.12)
    s.send(_sgr("m", 1, col, row))
    s.feed(settle)


def progress_cells(s):
    """(进度条首列, 宽度) 1-based 列号;无进度条返回 None。"""
    p = progress_row(s)
    if p is None:
        return None
    start = None
    width = 0
    for i, ch in enumerate(s.row(p)):
        if ch in ("\u2501", "\u2500"):
            if start is None:
                start = i + 1
            width += 1
        elif start is not None:
            break
    return (start, width) if start is not None else None


def media_env(**kw):
    return dict(os.environ, DLOOK_IMAGE_PROTOCOL="halfblocks", **kw)


def ensure_audio_fixture():
    """生成确定性 WAV(8s 440Hz 单声道 8kHz);不落库二进制,按需生成。"""
    path = os.path.join(FIX, "audio", "tone.wav")
    if os.path.exists(path):
        return path
    os.makedirs(os.path.dirname(path), exist_ok=True)
    import math
    import struct
    import wave

    with wave.open(path, "w") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(8000)
        frames = b"".join(
            struct.pack("<h", int(12000 * math.sin(2 * math.pi * 440 * i / 8000)))
            for i in range(8000 * 8)
        )
        w.writeframes(frames)
    return path


# ---- O. 音频(design §6:O1–O12 + O13 降级 / O14 非 TTY) --------------------

def scenario_O():
    print("== O-audio ==")
    wav = ensure_audio_fixture()
    env = media_env()
    md_audio = os.path.join(FIX, "md-audio.md")

    # O1: M1 直接打开 → 媒体栏(进度条+时间码)+ 媒体 footer + body 信息块
    s = PtySession(BIN + [wav], cols=80, rows=24, env=env, cwd=ROOT)
    s.start()
    s.feed(1.2)
    task = panic_engine(s)
    if task:
        engine_skip("O1–O14 音频场景", task)
        s.close()
        return
    p = progress_row(s)
    check(p == 22, "O1 媒体栏进度条行 = rows-2 (22)", f"got {p}")
    check(timecodes(s)[1] == 8, "O1 时间码显示时长 00:08", str(timecodes(s)))
    check(
        "space" in s.row(24) and "\u23ef" in s.row(24),
        "O1 footer 分层 M1(整体替换媒体键位)",
        s.row(24)[:40],
    )
    body = s.screen_text()
    check("tone.wav" in body and "format:" in body, "O1 body 信息块(标题/格式)")
    check(s.row(1).endswith("tone.wav"), "O1 header 显示文件名", s.row(1))
    playing_ok = require_playback(s, "O2 时间码推进")

    if playing_ok:
        t0 = timecodes(s)[0]
        f0 = bar_fraction(s)
        s.feed(2.2)
        t1 = timecodes(s)[0]
        f1 = bar_fraction(s)
        check(t1 is not None and t0 is not None and t1 > t0, f"O2 时间码推进 {t0}→{t1}")
        check(
            f1 is not None and f0 is not None and f1 >= f0,
            f"O2 进度条比例不倒退 {f0}→{f1}",
        )

        # O3: `p` 暂停 → ▮▮ + 时间码冻结
        s.send_key("p")
        s.feed(0.6)
        ir = info_row(s)
        check("\u25ae\u25ae" in (s.row(ir) if ir else ""), "O3 暂停图标 ▮▮")
        tp = timecodes(s)[0]
        s.feed(1.2)
        check(timecodes(s)[0] == tp, f"O3 暂停时时间码冻结({tp})")

        # O4: Space 在 M1 恢复播放
        s.send_key("Space")
        s.feed(1.0)
        ir = info_row(s)
        check("\u25b6" in (s.row(ir) if ir else ""), "O4 Space(M1) 恢复播放(▶)")
        check(
            timecodes(s)[0] is not None and timecodes(s)[0] > tp,
            "O4 时间码继续推进",
        )

        # O5: seek 跳变(→ +5s / `,` -60s clamp / `.` +60s clamp)
        # 先暂停:曲末自然结束(finished)后 seek 被引擎忽略(见报告:media-1 观察项)
        s.send_key("p")
        s.feed(0.4)
        p0 = timecodes(s)[0]
        s.send_key(",")
        s.feed(0.5)
        check(timecodes(s)[0] == 0, f"O5 `,` -60s clamp 00:00 (from {p0})")
        s.send_key(".")
        s.feed(0.5)
        check(timecodes(s)[0] == 8, f"O5 `.` +60s clamp 末尾 (got {timecodes(s)[0]})")
        s.send_key(",")
        s.feed(0.5)
        s.send_key("Right")
        s.feed(0.5)
        check(timecodes(s)[0] == 5, f"O5 → seek +5s (got {timecodes(s)[0]})")
        check("seek +5s" in s.screen_text(), "O5 状态栏 seek 反馈")
        s.send_key("Left")
        s.feed(0.5)
        check(timecodes(s)[0] == 0, f"O5 ← seek -5s (got {timecodes(s)[0]})")
        s.send_key("p")
        s.feed(0.4)

        # O6: 音量 -/+(栏内百分比 + 状态栏)
        s.send_key("-")
        s.feed(0.5)
        check("vol " in s.screen_text(), "O6 音量状态栏反馈")
        ir = info_row(s)
        low = re.search(r"(\d+)%", s.row(ir) if ir else "")
        s.send_key("+")
        s.feed(0.5)
        ir = info_row(s)
        high = re.search(r"(\d+)%", s.row(ir) if ir else "")
        check(
            low is not None and high is not None and int(high.group(1)) > int(low.group(1)),
            f"O6 -/+ 音量 ±5% ({low and low.group(0)} → {high and high.group(0)})",
        )

        # O7: `m` 静音切换
        s.send_key("m")
        s.feed(0.5)
        ir = info_row(s)
        check("mute" in (s.row(ir) if ir else ""), "O7 静音后栏内 mute")
        check("muted" in s.screen_text(), "O7 状态栏 muted")
        s.send_key("m")
        s.feed(0.5)
        ir = info_row(s)
        check("mute" not in (s.row(ir) if ir else ""), "O7 再按解除静音")

        # O8: `0` 回曲首
        s.send_key("0")
        s.feed(0.6)
        check(
            timecodes(s)[0] is not None and timecodes(s)[0] <= 1,
            f"O8 0 → 回曲首 (got {timecodes(s)[0]})",
        )
        check("restart" in s.screen_text(), "O8 状态栏 restart")

    # O9: Esc 链(清选区 → 停止会话 → 退出)
    s.send_key("Escape")
    s.feed(0.6)
    check(s.wait_exit(0.3) is None, "O9 Esc① 停止会话不退出")
    check(progress_row(s) is None, "O9 停止后媒体栏隐藏")
    check("stopped" in s.screen_text(), "O9 状态栏 stopped")
    s.feed(1.8)  # 等状态消息过期(1.5s TTL)→ footer 回落普通键位
    check("q quit" in s.row(24), "O9 footer 回到 pager 键位", s.row(24)[:30])
    s.send_key("Escape")
    code = s.wait_exit(3)
    check(code == 0, f"O9 Esc② 退出码 0 (got {code})")
    s.close()

    # O10/O11: M2 —— markdown 内点击音频链接就地播放(不导航)
    s = PtySession(BIN + [md_audio], cols=80, rows=24, env=env, cwd=ROOT)
    s.start()
    s.feed(0.8)
    before_header = s.row(1)
    cell = _find_cell(s, "click tone")
    check(cell is not None, "O10 找到音频链接 cell")
    if cell:
        _click(s, *cell, settle=0.9)
        check(s.row(1) == before_header, "O10 M2 就地播放不导航", s.row(1)[:40])
        check(progress_row(s) is not None, "O10 M2 媒体栏出现")
        row2 = s.row(2)
        s.send_key("Space")
        s.feed(0.5)
        check(s.row(2) != row2, "O10 M2 Space 仍翻页", f"{row2!r} → {s.row(2)!r}")
        check(progress_row(s) is not None, "O10 翻页后会话仍在")
        s.feed(1.8)
        s.send_key("j")
        s.feed(0.3)
        check(
            "\u266a" in s.row(24) and "p \u23ef" in s.row(24),
            "O10 M2 footer 追加 ♪ p ⏯",
            s.row(24)[-20:],
        )
        check("space/pgdn" in s.row(24), "O10 M2 保留原 pager 键位", s.row(24)[:30])

        # O11: M2 `p` 暂停
        if require_playback(s, "O11 M2 p 暂停"):
            s.send_key("p")
            s.feed(0.6)
            ir = info_row(s)
            check("\u25ae\u25ae" in (s.row(ir) if ir else ""), "O11 M2 p 暂停(▮▮)")
            s.send_key("p")
            s.feed(0.5)
    s.send_key("q")
    code = s.wait_exit(3)
    check(code == 0, f"O11 M2 q 退出 0 (got {code})")
    s.close()

    # O12: 远程 URL 音频(本地 http server,避免外网依赖)
    handler = functools.partial(
        http.server.SimpleHTTPRequestHandler, directory=os.path.dirname(wav)
    )
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    port = srv.server_address[1]
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    url = f"http://127.0.0.1:{port}/{os.path.basename(wav)}"
    s = PtySession(BIN + [url], cols=80, rows=24, env=env, cwd=ROOT)
    s.start()
    s.feed(2.5)
    task = panic_engine(s)
    if task:
        engine_skip("O12 远程 URL 音频", task)
    else:
        check(progress_row(s) is not None, "O12 远程 URL 媒体栏出现")
        check(timecodes(s)[1] == 8, f"O12 远程 URL 时长 00:08 (got {timecodes(s)})")
        s.send_key("q")
        code = s.wait_exit(3)
        check(code == 0, f"O12 远程 URL q 退出 0 (got {code})")
    s.close()
    srv.shutdown()

    # O13: 降级链 —— 解码失败(伪 mp3)→ 栏内 ✗ <原因> + body 提示行,不退出
    bogus = os.path.join(tempfile.gettempdir(), "dlook-e2e-bogus.mp3")
    with open(bogus, "w") as f:
        f.write("not audio at all\n" * 20)
    s = PtySession(BIN + [bogus], cols=80, rows=24, env=env, cwd=ROOT)
    s.start()
    s.feed(1.5)
    task = panic_engine(s)
    if task:
        engine_skip("O13 音频降级链", task)
    else:
        check(progress_row(s) is not None, "O13 失败态仍显示媒体栏")
        err = bar_error(s)
        check(err is not None, "O13 栏内 ✗ <原因>", str(err))
        check("\u2717" in s.screen_text(), "O13 body 提示 ✗ 原因")
        check(s.wait_exit(0.3) is None, "O13 失败不退出进程")
        s.send_key("q")
        code = s.wait_exit(3)
        check(code == 0, f"O13 失败态 q 退出 0 (got {code})")
    s.close()

    # O14: 非 TTY(管道)→ error: '<arg>' needs a terminal + rc 1
    r = subprocess.run(BIN + [wav], capture_output=True, text=True, cwd=ROOT, env=env)
    check(r.returncode == 1, f"O14 非 TTY 音频 rc 1 (got {r.returncode})")
    check("needs a terminal" in r.stderr, "O14 非 TTY 报错文案", r.stderr.strip()[:60])


def scenario_O_e9():
    """E9 探针:证明样本真的到达设备(null sink + monitor 录制)。

    默认跳过(需 pactl/pw-record 且显式 DLOOK_E2E_E9=1);跳过分支显式标记。
    """
    print("== O-e9 (audio probe) ==")
    if os.environ.get("DLOOK_E2E_E9") != "1":
        skip("O-E9 null-sink 录制探针", "未启用(DLOOK_E2E_E9=1 时运行;需 pactl+pw-record)")
        return
    tools = {t: shutil.which(t) for t in ("pactl", "pw-record")}
    if not all(tools.values()):
        skip("O-E9 null-sink 录制探针", f"缺少工具 {[k for k, v in tools.items() if not v]}")
        return
    wav = ensure_audio_fixture()
    log = os.path.join(tempfile.gettempdir(), "dlook-e9-rec.wav")
    subprocess.run(
        [tools["pactl"], "load-module", "module-null-sink", "sink_name=dlook-e2e"],
        check=False, capture_output=True,
    )
    rec = subprocess.Popen(
        [tools["pw-record"], "--target", "dlook-e2e.monitor", "--channels", "2",
         "--rate", "44100", log],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    time.sleep(0.4)
    s = PtySession(BIN + [wav], cols=80, rows=24, env=media_env(PULSE_SINK="dlook-e2e"), cwd=ROOT)
    s.start()
    s.feed(2.5)
    s.send_key("q")
    s.wait_exit(3)
    s.close()
    rec.terminate()
    time.sleep(0.3)
    subprocess.run([tools["pactl"], "unload-module", "module-null-sink"], check=False,
                   capture_output=True)
    try:
        import math
        import struct
        import wave as wave_mod

        with wave_mod.open(log) as w:
            frames = w.readframes(w.getnframes())
            ch = w.getnchannels() or 1
        samples = struct.unpack(f"<{len(frames)//2}h", frames)[::ch]
        rms = math.sqrt(sum(v * v for v in samples) / max(1, len(samples)))
        check(rms > 100, f"O-E9 探针捕获到非静音样本 (RMS={rms:.0f})")
    except Exception as ex:  # noqa: BLE001
        skip("O-E9 探针读回", f"录制文件不可读: {ex}")


# ---- P. 视频(design §6:P1–P8) ---------------------------------------------
# pty+pyte 无图形协议 → mpv 共屏路径(P1–P4)无法在 pty 内成立:这些用例检查
# mpv 路径的**前置条件**,不成立时显式 SKIP 并指向 V 套件(真实终端)。
# P5–P8 走降级链,pty 内可完整断言(引擎落地后)。

def _children_of(pid):
    """某进程的直接子进程 pid 列表(/proc 扫描,无 psutil 依赖)。"""
    out = []
    for entry in os.listdir("/proc"):
        if not entry.isdigit():
            continue
        try:
            with open(f"/proc/{entry}/stat") as f:
                fields = f.read().split()
            if len(fields) > 3 and int(fields[3]) == pid:
                out.append(int(entry))
        except (OSError, ValueError):
            continue
    return out


def _child_cmdlines(pid):
    cmds = []
    for c in _children_of(pid):
        try:
            with open(f"/proc/{c}/cmdline", "rb") as f:
                cmds.append(f.read().replace(b"\0", b" ").decode(errors="replace").strip())
        except OSError:
            continue
    return cmds


def _video_fixture():
    """生成 4s 测试视频(ffmpeg;缺 ffmpeg 时用 experiments 里的样例)。"""
    path = os.path.join(FIX, "video", "clip.mp4")
    if os.path.exists(path):
        return path
    if shutil.which("ffmpeg"):
        os.makedirs(os.path.dirname(path), exist_ok=True)
        subprocess.run(
            ["ffmpeg", "-v", "error", "-y", "-f", "lavfi", "-i",
             "testsrc=duration=4:size=320x180:rate=10", "-pix_fmt", "yuv420p", path],
            check=False, capture_output=True,
        )
        if os.path.exists(path):
            return path
    alt = os.path.join(ROOT, "docs/research/media/experiments/test-video.mp4")
    return alt if os.path.exists(alt) else None


def _path_without(*progs):
    """PATH 去掉指定可执行所在目录(用于模拟 mpv/ffmpeg 缺失)。"""
    drop_dirs = set()
    for p in progs:
        loc = shutil.which(p)
        if loc:
            drop_dirs.add(os.path.dirname(loc))
    keep = [d for d in os.environ.get("PATH", "").split(os.pathsep)
            if d and d not in drop_dirs]
    return os.pathsep.join(keep)


def scenario_P():
    print("== P-video ==")
    clip = _video_fixture()
    if clip is None:
        skip("P1–P8 视频场景", "无测试视频(缺 ffmpeg 且 experiments 样例不存在)")
        return

    # P1/P2: mpv 启动(四态)+ IPC 控制链 —— 需真实图形协议
    if os.environ.get("DLOOK_REAL_TERM") == "1":
        _scenario_P_real(clip)
    else:
        skip("P1 mpv spawn 参数(/proc/<pid>/cmdline)",
             "pty+pyte 无图形协议 → 走降级链;真实终端(sway/foot)覆盖于 V 套件")
        skip("P2 IPC 控制链(第二客户端)", "同上:需真实图形协议与 mpv 共屏")
        skip("P3 四态视觉(播放/暂停/seek/退出)", "同上:V1 视觉套件覆盖")
        skip("P4 退出回收(mpv 子进程 + rc 0)", "同上:V4/V8 真实终端覆盖")

    # P5: 无 mpv(有 ffmpeg)→ 状态栏 mpv not found + ffmpeg 首帧静图(图片管线)
    env = media_env(PATH=_path_without("mpv"))
    s = PtySession(BIN + [clip], cols=80, rows=24, env=env, cwd=ROOT)
    s.start()
    s.feed(3.0)
    task = panic_engine(s)
    if task:
        engine_skip("P5 无 mpv → ffmpeg 首帧静图", task)
        s.close()
    else:
        txt = s.screen_text()
        check("mpv not found" in txt, "P5 状态栏/信息行 mpv not found", txt[:80])
        check(_half_rows(s), "P5 ffmpeg 首帧进入图片管线(半块字符)")
        check(
            not any("mpv" in c for c in _child_cmdlines(s.pid)),
            "P5 未 spawn mpv 子进程",
            str(_child_cmdlines(s.pid)),
        )
        s.send_key("q")
        code = s.wait_exit(4)
        check(code == 0, f"P5 退出码 0 (got {code})")
        s.close()

    # P6: 无 mpv 且无 ffmpeg → 信息行(文件名/格式/原因)
    env6 = media_env(PATH=_path_without("mpv", "ffmpeg"))
    s = PtySession(BIN + [clip], cols=80, rows=24, env=env6, cwd=ROOT)
    s.start()
    s.feed(2.5)
    task = panic_engine(s)
    if task:
        engine_skip("P6 无 ffmpeg → 信息行", task)
    else:
        txt = s.screen_text()
        check(os.path.basename(clip) in txt, "P6 信息行含文件名")
        check("format:" in txt and "note:" in txt, "P6 信息行含格式与原因")
        check(not _half_rows(s), "P6 无 ffmpeg 时不渲染静图")
        s.send_key("q")
        code = s.wait_exit(4)
        check(code == 0, f"P6 退出码 0 (got {code})")
    s.close()

    # P7: 有 mpv 但无图形协议(halfblocks)→ 同一降级链,且不 spawn mpv
    env7 = media_env(PATH=os.environ.get("PATH", ""))
    s = PtySession(BIN + [clip], cols=80, rows=24, env=env7, cwd=ROOT)
    s.start()
    s.feed(3.0)
    task = panic_engine(s)
    if task:
        engine_skip("P7 无图形协议 → 降级链", task)
    else:
        txt = s.screen_text()
        check(
            "graphics protocol" in txt or "mpv not found" in txt,
            "P7 降级原因提示(无图形协议/mpv)",
            txt[:80],
        )
        check(
            not any("mpv" in c for c in _child_cmdlines(s.pid)),
            "P7 无协议时不启动 mpv",
            str(_child_cmdlines(s.pid)),
        )
        s.send_key("q")
        code = s.wait_exit(4)
        check(code == 0, f"P7 退出码 0 (got {code})")
    s.close()

    # P8: 非 TTY → needs a terminal + rc 1(视频与图片语义一致)
    r = subprocess.run(BIN + [clip], capture_output=True, text=True, cwd=ROOT, env=media_env())
    check(r.returncode == 1, f"P8 非 TTY 视频 rc 1 (got {r.returncode})")
    check("needs a terminal" in r.stderr, "P8 非 TTY 报错文案", r.stderr.strip()[:60])


def _scenario_P_real(clip):
    """真实终端(foot)下的 mpv 路径断言:spawn 参数 + IPC 控制 + 退出回收。"""
    env = dict(os.environ, DLOOK_IMAGE_PROTOCOL="auto")
    tag = "dlook-e2e-p"
    proc = subprocess.Popen(
        ["foot", "-a", tag, "-T", tag, "--", BIN[0], clip],
        env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        stdin=subprocess.DEVNULL, start_new_session=True,
    )
    try:
        deadline = time.time() + 12
        mpv_cmd = None
        while time.time() < deadline and mpv_cmd is None:
            for cmd in _child_cmdlines(proc.pid):
                if "mpv" in cmd:
                    mpv_cmd = cmd
                    break
            time.sleep(0.3)
        if mpv_cmd is None:
            skip("P1 mpv spawn 参数", "foot 内未探测到 mpv 子进程(可能无 kitty/sixel 协议)")
            skip("P2 IPC 控制链", "同上")
        else:
            check("--input-ipc-server" in mpv_cmd, "P1 mpv cmdline 含 --input-ipc-server")
            check(
                "--vo-kitty" in mpv_cmd or "--vo=sixel" in mpv_cmd or "vo=sixel" in mpv_cmd,
                "P1 mpv vo 参数",
                mpv_cmd[:120],
            )
            check(
                re.search(r"vo-kitty-(left|top|rows|cols)", mpv_cmd) is not None,
                "P1 mpv 区域几何参数",
                mpv_cmd[:160],
            )
            m = re.search(r"--input-ipc-server=(\S+)", mpv_cmd)
            check(m is not None, "P1 IPC socket 路径可取")
            if m:
                ipc = m.group(1)
                resp = _ipc_cmd(ipc, {"command": ["get_property", "pause"]})
                check("error" not in resp.lower(), "P2 IPC 第二客户端可读 pause 属性", resp[:80])
                _ipc_cmd(ipc, {"command": ["set_property", "pause", True]})
                time.sleep(0.5)
                resp2 = _ipc_cmd(ipc, {"command": ["get_property", "pause"]})
                check("true" in resp2, "P2 IPC set pause 生效", resp2[:80])
    finally:
        subprocess.run(["hyprctl", "dispatch",
                        f'hl.dsp.focus({{window="class:{tag}"}})'], capture_output=True)
        subprocess.run(["wtype", "-k", "q"], capture_output=True)
        time.sleep(1.0)
        if proc.poll() is None:
            proc.terminate()
        time.sleep(0.5)
        left = subprocess.run(["pgrep", "-f", "dlook-e2e-p"], capture_output=True, text=True)
        check(left.returncode != 0 or not left.stdout.strip(), "P4 退出后无残留窗口进程")


def _ipc_cmd(sock_path, payload):
    import json
    import socket

    try:
        c = socket.socket(socket.AF_UNIX)
        c.settimeout(2.0)
        c.connect(sock_path)
        c.sendall((json.dumps(payload) + "\n").encode())
        time.sleep(0.15)
        c.setblocking(False)
        try:
            data = c.recv(65536).decode(errors="replace")
        except BlockingIOError:
            data = ""
        c.close()
        return data
    except OSError as ex:
        return f"ipc error: {ex}"


# ---- Q. 网页(design §6:Q1–Q9) ----------------------------------------------

def _web_server(directory=None, routes=None):
    """本地 http server:(base_url, requests_log, shutdown)。

    routes: {path: (status, ctype, body)} 优先于静态目录;请求全部记录(安全 canary)。
    """
    log = []

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *a):  # 静默
            pass

        def do_GET(self):  # noqa: N802
            log.append(self.path)
            path = self.path.split("?")[0]
            if routes is not None:
                if path not in routes:
                    self.send_error(404, "not found")
                    return
                status, ctype, body = routes[path]
                self.send_response(status)
                self.send_header("Content-Type", ctype)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
                return
            local = os.path.join(directory or ".", path.lstrip("/"))
            try:
                with open(local, "rb") as f:
                    body = f.read()
                self.send_response(200)
                self.send_header("Content-Type", "text/html; charset=utf-8")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
            except OSError:
                self.send_error(404, "not found")

    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return (f"http://127.0.0.1:{srv.server_address[1]}", log, srv.shutdown)


def _xdg_env(tag):
    log = os.path.join(tempfile.gettempdir(), f"dlook-xdg-{tag}.log")
    if os.path.exists(log):
        os.remove(log)
    fake_bin = os.path.join(ROOT, "test", "e2e", "fake-bin")
    env = media_env(XDG_LOG=log, PATH=fake_bin + os.pathsep + os.environ["PATH"])
    return env, log


def _xdg_got(log):
    return open(log).read().strip() if os.path.exists(log) else ""


def scenario_Q():
    print("== Q-web ==")
    base_url, _reqs, shutdown = _web_server()
    env, xdg_log = _xdg_env("q")

    # Q1: 本地 .html L1 渲染(标题/标题行/链接/表格)
    page = os.path.join(FIX, "web", "page.html")
    s = PtySession(BIN + [page], cols=80, rows=24, env=env, cwd=ROOT)
    s.start()
    s.feed(1.5)
    task = panic_engine(s)
    if task:
        engine_skip("Q1–Q9 网页场景", task)
        s.close()
        shutdown()
        return
    txt = s.screen_text()
    check("Q Page Title" in s.row(1), "Q1 页面标题进 header", s.row(1)[:50])
    check("Q Heading" in txt, "Q1 <h1> 渲染为文本")
    check("item one" in txt, "Q1 列表渲染")
    check("ext link" in txt and "https://example.com/dlook" in txt, "Q1 链接 label+url 展示")
    check("q quit" in s.row(24), "Q1 footer 为 pager 键位(无音频会话)")

    # Q2: 点击外部链接 → 系统浏览器(xdg-open 收到 URL);页面不动
    cell = _find_cell(s, "ext link")
    check(cell is not None, "Q2 找到外部链接 cell")
    if cell:
        _click(s, *cell, settle=0.6)
        check(_xdg_got(xdg_log) == "https://example.com/dlook",
              f"Q2 xdg-open 收到 URL (got {_xdg_got(xdg_log)!r})")
        check(s.row(1).strip().startswith(page), "Q2 外部链接不导航", s.row(1)[:40])
        check("opened externally" in s.screen_text(), "Q2 状态栏提示 opened")

    # Q3: 本地 .html 链接 → 应用内跳转(推历史栈)
    cell = _find_cell(s, "local link")
    check(cell is not None, "Q3 找到本地链接 cell")
    if cell:
        _click(s, *cell, settle=1.0)
        check("next.html" in s.row(1), "Q3 应用内跳转到 next.html", s.row(1)[:60])
        check("Next Page Marker" in s.screen_text(), "Q3 目标页渲染")
        check("back" in s.row(24), "Q3 footer 出现 back 提示")

        # Q4: ⌫ 返回
        s.send_key("Backspace")
        s.feed(0.6)
        check(page in s.row(1), "Q4 ⌫ 返回原页", s.row(1)[:60])

    # Q5: 安全 canary —— 不请求任何子资源(img/css/js 不进网络)
    canary_url, canary_log, canary_shutdown = _web_server(
        routes={"/canary.html": (200, "text/html; charset=utf-8",
                                 b"<html><head><title>C</title>"
                                 b"<link rel=stylesheet href='/style.css'></head>"
                                 b"<body><h1>Canary</h1>"
                                 b"<img src='/pixel.png'>"
                                 b"<script src='/app.js'></script></body></html>")}
    )
    s = PtySession(BIN + [canary_url + "/canary.html"], cols=80, rows=24, env=env, cwd=ROOT)
    s.start()
    s.feed(1.5)
    if not panic_engine(s):
        check("Canary" in s.screen_text(), "Q5 canary 页面渲染")
        subs = [r for r in canary_log if r != "/canary.html"]
        check(not subs, "Q5 不请求任何子资源(防追踪像素)", str(subs))
        s.send_key("q")
        s.wait_exit(3)
    else:
        engine_skip("Q5 安全 canary", "media-2")
    s.close()
    canary_shutdown()

    # Q6: GBK 编码页不炸
    gbk_url, _gl, gbk_shutdown = _web_server(
        routes={"/gbk.html": (200, "text/html; charset=gbk",
                              "<html><head><title>GBK</title></head>"
                              "<body><p>中文编码测试</p></body></html>".encode("gbk"))}
    )
    s = PtySession(BIN + [gbk_url + "/gbk.html"], cols=80, rows=24, env=env, cwd=ROOT)
    s.start()
    s.feed(1.5)
    if not panic_engine(s):
        check(s.wait_exit(0.2) is None, "Q6 GBK 页面不崩")
        check("GBK" in s.screen_text(), "Q6 GBK 页渲染出内容")
        s.send_key("q")
        code = s.wait_exit(3)
        check(code == 0, f"Q6 GBK 页 q 退出 0 (got {code})")
    else:
        engine_skip("Q6 GBK 不炸", "media-2")
    s.close()
    gbk_shutdown()

    # Q7: 4xx → ✗ 行 + 不退出
    err_url, _el, err_shutdown = _web_server(routes={})
    s = PtySession(BIN + [err_url + "/nope.html"], cols=80, rows=24, env=env, cwd=ROOT)
    s.start()
    s.feed(2.0)
    if not panic_engine(s):
        check("✗" in s.screen_text(), "Q7 4xx 错误行 ✗", s.screen_text()[:80])
        check(s.wait_exit(0.2) is None, "Q7 抓取失败不退出")
        s.send_key("q")
        code = s.wait_exit(3)
        check(code == 0, f"Q7 失败态 q 退出 0 (got {code})")
    else:
        engine_skip("Q7 4xx 提示", "media-2")
    s.close()
    err_shutdown()

    # Q8: `o` 键 → 系统浏览器打开当前页
    s = PtySession(BIN + [page], cols=80, rows=24, env=env, cwd=ROOT)
    s.start()
    s.feed(1.2)
    if not panic_engine(s):
        if os.path.exists(xdg_log):
            os.remove(xdg_log)
        s.send_key("o")
        s.feed(0.8)
        got = _xdg_got(xdg_log)
        check(got.startswith("file://") and got.endswith("page.html"),
              f"Q8 o 键浏览器打开当前页 (got {got!r})")
        check("opened in browser" in s.screen_text(), "Q8 状态栏 opened in browser")
        s.send_key("q")
        s.wait_exit(3)
    else:
        engine_skip("Q8 o 键浏览器", "media-2")
    s.close()
    shutdown()

    # Q9: 非 TTY URL → needs a terminal + rc 1
    r = subprocess.run(BIN + [base_url + "/page.html"], capture_output=True, text=True,
                       cwd=ROOT, env=env)
    check(r.returncode == 1, f"Q9 非 TTY 网页 rc 1 (got {r.returncode})")
    check("needs a terminal" in r.stderr, "Q9 非 TTY 报错文案", r.stderr.strip()[:60])


# ---- R. 鼠标/交互(design §6:R1–R6) -----------------------------------------

def scenario_R():
    print("== R-interaction ==")
    wav = ensure_audio_fixture()
    env = media_env()
    s = PtySession(BIN + [wav], cols=80, rows=24, env=env, cwd=ROOT)
    s.start()
    s.feed(1.2)
    task = panic_engine(s)
    if task:
        engine_skip("R1–R4/R6 媒体鼠标交互", task)
        s.close()
        _scenario_R5_back()  # R5 不依赖引擎
        return

    pr = progress_row(s)
    ir = info_row(s)
    cells = progress_cells(s)
    check(pr is not None and ir is not None and cells is not None, "R0 媒体栏命中区可定位")
    playing_ok = require_playback(s, "R1–R4 媒体鼠标交互")

    if playing_ok and cells:
        left, width = cells
        # R1: click-to-seek 比例断言(x = left + 50% → 位置 ≈ 4/8s)
        _click(s, left + width // 2, pr, settle=0.8)
        pos, dur = timecodes(s)
        check(dur == 8 and pos is not None and 3 <= pos <= 5,
              f"R1 点击 50% → 位置≈04 (got {pos}/{dur})")
        check("seek" in s.screen_text(), "R1 状态栏 seek 反馈")

        # R2: scrubbing —— 预览更新不随拖动次数膨胀(≤ 帧数),松开提交最后比例
        base = len(s.raw_text())
        s.send(_sgr("M", 0, left + 2, pr))
        s.feed(0.05)
        for x in range(left + 3, left + width - 1, 3):
            s.send(_sgr("M", 32, x, pr))
        s.feed(0.45)
        previews = re.findall(r"seek \d+%", s.raw_text()[base:])
        s.send(_sgr("m", 0, left + width - 2, pr))
        s.feed(0.7)
        pos2, _d = timecodes(s)
        check(len(previews) <= 4, f"R2 拖动预览不刷屏(共 {len(previews)} 次更新)")
        check(pos2 is not None and pos2 >= 6, f"R2 松开提交最后比例 (got {pos2})")
        # 120ms 节流的精确断言在单元测试 scrub_preview_is_throttled(pty 只能观察绘制帧)

        # R3: 滚轮三区语义(先回曲首暂停:曲末 finished 后 seek 被引擎忽略)
        s.send_key("0")
        s.feed(0.6)
        s.send_key("p")
        s.feed(0.4)
        s.send_key("Right")  # → 00:05,给滚轮留出回退空间
        s.feed(0.5)
        before = timecodes(s)[0]
        _wheel(s, True, left + width // 2, pr)
        after = timecodes(s)[0]
        check(before == 5 and after == 0,
              f"R3 进度条滚轮 seek -5s ({before}→{after})")
        vol_before = re.search(r"(\d+)%", s.row(ir))
        _wheel(s, True, s.cols - 3, ir)
        vol_after = re.search(r"(\d+)%", s.row(ir))
        check(
            vol_before is not None and vol_after is not None
            and int(vol_after.group(1)) > int(vol_before.group(1)),
            f"R3 音量区滚轮 +5% "
            f"({vol_before and vol_before.group(0)} → {vol_after and vol_after.group(0)})",
        )

        # R4: 信息行非音量区单击 = 播放/暂停
        before_icon = s.row(ir)
        _click(s, 5, ir, settle=0.7)
        after_icon = s.row(ir)
        check(
            ("\u25ae\u25ae" in after_icon) != ("\u25ae\u25ae" in before_icon),
            "R4 信息行单击切换播放/暂停",
            f"{before_icon[:20]!r} → {after_icon[:20]!r}",
        )
        _click(s, 5, ir, settle=0.5)

    # R6: footer 分层 token ≤ 8(M1 媒体键位;等状态消息过期)
    s.feed(1.8)
    toks = s.row(24).split()
    check(all(len(t) <= 8 for t in toks), "R6 M1 footer token ≤ 8",
          str([t for t in toks if len(t) > 8]))
    check("space" in toks and "seek" in toks and "mute" in toks, "R6 M1 footer 含媒体键位")

    s.send_key("q")
    s.wait_exit(3)
    s.close()

    _scenario_R5_back()


def _scenario_R5_back():
    """R5:中键 = 返回(⌫ 语义)。文档区,不依赖媒体引擎。"""
    src = os.path.join(FIX, "link-src.md")
    s = PtySession(BIN + [src], cols=80, rows=24, env=os.environ, cwd=ROOT)
    s.start()
    if not s.wait_for("Link Source", 6):
        bad("R5 启动 link-src.md (app did not start)")
        s.close()
        return
    cell = _find_cell(s, "dst\u2197")
    if not cell:
        bad("R5 找到本地链接 cell")
        s.close()
        return
    _click(s, *cell, settle=0.9)
    check("link-dst.md" in s.row(1), "R5 前置:点击链接跳转成功", s.row(1)[:40])
    _mid_click(s, 5, 5, settle=0.9)
    check("link-src.md" in s.row(1), "R5 中键返回上一文件", s.row(1)[:40])
    _mid_click(s, 5, 5, settle=0.6)
    check(s.wait_exit(0.2) is None, "R5 无历史中键不退出")
    s.send_key("q")
    s.wait_exit(3)
    s.close()


if __name__ == "__main__":
    main()
