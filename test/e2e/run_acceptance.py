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


def main():
    print(f"BIN = {BIN}")
    print(f"FIX = {FIX}")
    # ensure large.txt exists
    lg = os.path.join(FIX, "large.txt")
    if not os.path.exists(lg):
        subprocess.run(["bash", os.path.join(ROOT, "test/e2e/gen-large.sh")], check=True)

    for sc in [scenario_A, scenario_B, scenario_C, scenario_D,
               scenario_E, scenario_F, scenario_G, scenario_H, scenario_I,
               scenario_J, scenario_K, scenario_L, scenario_M]:
        try:
            sc()
        except Exception as ex:
            print(f"  \u2717 {sc.__name__} EXCEPTION: {ex}")
            global FAIL
            FAIL += 1

    print()
    print(f"RESULT: PASS={PASS} FAIL={FAIL}")
    sys.exit(0 if FAIL == 0 else 1)


if __name__ == "__main__":
    main()
