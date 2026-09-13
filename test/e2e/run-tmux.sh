#!/usr/bin/env bash
# dlook tmux 端到端验收(DECISIONS D1–D11 实施后的真实终端验证)。
#
# 与 run_acceptance.py(pty+pyte 模拟器)互补:本脚本跑在真实终端(tmux 服务器)里,
#   - tmux 作为真终端解析 ANSI/OSC/alt-screen/mouse-reporting
#   - set-clipboard on 捕获应用的 OSC 52 → tmux buffer,可断言「复制的文本内容」
#     (pyte 只能断言序列出现,无法验证剪贴板内容 —— 这是本脚本的核心价值)
#   - 鼠标序列用 send-keys -l 注入 pane 输入
#     (注意:直接 > pane_tty 写入走的是输出方向,应用收不到)
#
# 用法:
#   BIN=rs/target/release/dlook bash test/e2e/run-tmux.sh
set -u

BIN="${BIN:-rs/target/release/dlook}"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
FIX="$ROOT/test/fixtures"
SOCK="dlook-e2e"
SESS="dlook"
COLS=100
ROWS=30

PASS=0; FAIL=0
ok()  { echo "  ✓ $1"; PASS=$((PASS+1)); }
bad() { echo "  ✗ $1${2:+  ($2)}"; FAIL=$((FAIL+1)); }
# $1=条件命令的退出码(0=pass) $2=描述
check() {
  if [ "$1" -eq 0 ]; then ok "$2"; else bad "$2"; fi
}

T() { tmux -L "$SOCK" "$@"; }

# 常用转义序列字面量
ESC_SGR7=$(printf '\033[7m')     # 反显(选区高亮)
ESC_SGR4=$(printf '\033[4m')     # 下划线(链接 label)
ESC_TRUECOLOR='38;2;'            # truecolor 前缀(字面匹配)

cleanup() { tmux -L "$SOCK" kill-server 2>/dev/null; }
trap cleanup EXIT

start_session() {
  tmux -L "$SOCK" -f /dev/null new-session -d -x "$COLS" -y "$ROWS" -s "$SESS" bash \
    >/dev/null 2>&1
  T set -g status off
  T set -g set-clipboard on   # OSC 52 → tmux buffer
  # 等 bash 就绪(提示符出现),否则第一条命令可能在 shell 初始化前被发送
  local i
  for ((i = 0; i < 50; i++)); do
    if cap_plain | grep -qE '\$ |# '; then return 0; fi
    sleep 0.1
  done
  sleep 1
}

pane_run() { T send-keys -t "$SESS" "$1" Enter; }
cap_plain() { T capture-pane -p -t "$SESS"; }
cap_esc()   { T capture-pane -p -e -t "$SESS"; }

wait_plain() { # $1=regex $2=超时(秒,默认 6)
  local pat="$1" t="${2:-6}" i limit
  limit=$((t * 10))
  for ((i = 0; i < limit; i++)); do
    if cap_plain | grep -Eq -- "$pat"; then return 0; fi
    sleep 0.1
  done
  return 1
}

# --- 鼠标注入(SGR 1006;坐标 1-based)---
mouse_send() { T send-keys -t "$SESS" -l "$1"; }
m_down() { mouse_send "$(printf '\033[<0;%s;%sM' "$1" "$2")"; }
m_drag() { mouse_send "$(printf '\033[<32;%s;%sM' "$1" "$2")"; }
m_up()   { mouse_send "$(printf '\033[<0;%s;%sm' "$1" "$2")"; }

# --- tmux 剪贴板(OSC 52 捕获)---
clear_buffers() {
  while [ -n "$(T list-buffers 2>/dev/null)" ]; do
    T delete-buffer 2>/dev/null || break
  done
}
buffer_text() { T show-buffer 2>/dev/null; }

# 退出 dlook(pane 回到 bash 提示符)
quit_app() { T send-keys -t "$SESS" q; sleep 0.3; }

# ===========================================================================
# T1. markdown 样式(style.md)
# ===========================================================================
scenario_style() {
  echo "== T-style (markdown: D1/D2/D3/D6/D7) =="
  pane_run "$BIN $FIX/style.md"
  if wait_plain 'H1 Heading One'; then ok "T1 style.md renders"; else bad "T1 style.md renders"; quit_app; return; fi

  cap_esc | grep -qF '38;5;14'; check $? "T2 h1/h2 cyan (38;5;14)"
  cap_esc | grep -qF '38;5;12'; check $? "T3 h3/h4 blue (38;5;12)"

  # H1 左对齐:行首空白 ≤ 2(居中会有大量前导空格)
  local h1line
  h1line=$(cap_plain | grep 'H1 Heading One' | head -1)
  if printf '%s' "$h1line" | grep -Eq '^ {0,2}H1'; then
    ok "T4 H1 left aligned"
  else
    bad "T4 H1 left aligned" "got: ${h1line:0:20}..."
  fi

  cap_plain | grep -q '☑ done task item'; check $? "T5 task done ☑"
  cap_plain | grep -q '☐ open task item';  check $? "T6 task open ☐"
  if cap_plain | grep -q '╭' && cap_plain | grep -q '╰'; then
    ok "T7 rounded table border"
  else
    bad "T7 rounded table border"
  fi

  cap_esc   | grep -qF "$ESC_SGR4"; check $? "T8 link label underlined (SGR 4)"
  cap_esc   | grep -qF '38;5;8';    check $? "T9 link url gray (38;5;8)"
  if cap_plain | grep -qF 'example link' && cap_plain | grep -qF '(https://example.com/path)'; then
    ok "T10 link label+url visible"
  else
    bad "T10 link label+url visible"
  fi
  quit_app
}

# ===========================================================================
# T2. 代码语言覆盖(D4:two-face)
# ===========================================================================
scenario_langs() {
  echo "== T-langs (two-face syntax set: D4) =="
  local f marker
  for f in lang.toml lang.vue sample.ts; do
    case "$f" in
      lang.toml) marker='host' ;;
      lang.vue)  marker='count' ;;
      *)         marker='TypeScript' ;;
    esac
    pane_run "$BIN $FIX/$f"
    if wait_plain "$marker"; then
      ok "T11 $f renders"
    else
      bad "T11 $f renders"; quit_app; continue
    fi
    cap_esc | grep -qF "$ESC_TRUECOLOR"; check $? "T12 $f truecolor highlight"
    quit_app
  done
}

# ===========================================================================
# T3. mermaid
# ===========================================================================
scenario_mermaid() {
  echo "== T-mermaid =="
  pane_run "$BIN $FIX/sample.mmd"
  sleep 1.0
  if cap_plain | grep -qE '[┌╭┬├│─→]' && ! cap_plain | grep -q 'flowchart TD'; then
    ok "T13 mermaid rendered as box-drawing (not source)"
  else
    bad "T13 mermaid rendered as box-drawing (not source)"
  fi
  quit_app
}

# ===========================================================================
# T4. 退出码
# ===========================================================================
scenario_exit() {
  echo "== T-exit =="
  pane_run "$BIN $FIX/plain.txt; echo EXIT1:\$?"
  wait_plain 'plain text file'
  quit_app
  wait_plain 'EXIT1:0'; check $? "T14 q quit → exit 0"

  pane_run "$BIN; echo EXIT2:\$?"
  wait_plain 'EXIT2:2'; check $? "T15 no args → exit 2"

  pane_run "$BIN $FIX/binary.bin; echo EXIT3:\$?"
  wait_plain 'EXIT3:1'; check $? "T16 binary file → exit 1"
}

# ===========================================================================
# T5. 拖选与 OSC 52 复制(D11)
# ===========================================================================
scenario_selection() {
  echo "== T-selection (drag select + OSC 52 clipboard: D11) =="
  pane_run "$BIN $FIX/large.txt"
  if wait_plain 'MARKER:0000'; then ok "T17 large.txt start"; else bad "T17 large.txt start"; quit_app; return; fi

  # 拖选第 3 行(1-based)col 3..31 → 内容应为 large.txt 第 2 行去掉首列:"INE_0001"
  clear_buffers
  m_down 3 3;  sleep 0.15
  m_drag 30 3; sleep 0.15
  m_up   31 3; sleep 0.4

  cap_esc | grep -qF "$ESC_SGR7"; check $? "T18 drag selection reversed (SGR 7)"
  cap_plain | grep -q 'copied';    check $? "T19 status shows copied"

  # 核心断言:tmux 捕获的 OSC 52 剪贴板内容
  if buffer_text | grep -q 'INE_0001'; then
    ok "T20 OSC 52 clipboard content correct (got: $(buffer_text | head -1))"
  else
    bad "T20 OSC 52 clipboard content correct" "buffer: $(buffer_text | head -1)"
  fi

  # y 手动复制:选区在松开后保留
  clear_buffers
  T send-keys -t "$SESS" y; sleep 0.4
  if buffer_text | grep -q 'INE_0001'; then
    ok "T21 y manual copy (OSC 52)"
  else
    bad "T21 y manual copy (OSC 52)" "buffer: $(buffer_text | head -1)"
  fi

  # 重新拖选 → Esc 只清除选择不退出 → 再 Esc 退出
  m_down 3 4;  sleep 0.15
  m_drag 30 4; sleep 0.15
  m_up   31 4; sleep 0.3
  cap_esc | grep -qF "$ESC_SGR7"; check $? "T22 re-select after y"
  T send-keys -t "$SESS" Escape; sleep 0.3
  # Esc 后:反显消失,且应用仍在运行(pane 进程仍是 dlook)
  if cap_esc | grep -qF "$ESC_SGR7"; then
    bad "T23 Esc clears selection highlight"
  else
    ok "T23 Esc clears selection highlight"
  fi
  if T list-panes -F '#{pane_current_command}' | grep -q dlook; then
    ok "T24 still running after Esc (no exit)"
  else
    bad "T24 still running after Esc (no exit)"
  fi
  T send-keys -t "$SESS" Escape; sleep 0.3
  if T list-panes -F '#{pane_current_command}' | grep -q dlook; then
    bad "T25 second Esc quits"
  else
    ok "T25 second Esc quits"
  fi
  sleep 0.3
}

# ===========================================================================
# T6. 边缘自动滚动(D11 ②)
# ===========================================================================
scenario_autoscroll() {
  echo "== T-autoscroll (drag at viewport edge: D11) =="
  pane_run "$BIN $FIX/large.txt"
  if wait_plain 'MARKER:0000'; then ok "T26 start"; else bad "T26 start"; quit_app; return; fi
  local before after
  before=$(cap_plain | sed -n '3p')
  m_down 3 3; sleep 0.15
  local i
  for i in 1 2 3 4 5 6; do
    m_drag 40 29; sleep 0.35   # 29 = 底部 body 行(ROWS=30)
  done
  m_up 41 29; sleep 0.3
  after=$(cap_plain | sed -n '3p')
  if [ "$before" != "$after" ]; then
    ok "T27 edge drag auto-scrolls (row3: '$before' → '$after')"
  else
    bad "T27 edge drag auto-scrolls"
  fi
  quit_app
}

# ===========================================================================
# T7. resize
# ===========================================================================
# ===========================================================================
# T-images (DECISIONS D15: 图片渲染;tmux 不支持图形协议,验证 halfblocks 路径
# 的真终端 truecolor 输出。kitty/sixel/iTerm2 协议路径由协议实现方(ratatui-image)
# 保证,pyte 场景 N 覆盖降级逻辑)
# ===========================================================================
scenario_images() {
  echo "== T-images (D15 halfblocks truecolor) =="
  pane_run "DLOOK_IMAGE_PROTOCOL=halfblocks $BIN $FIX/img-local.md"
  local i
  for ((i = 0; i < 60; i++)); do
    cap_plain | grep -qE '▀|▄' && break
    sleep 0.1
  done
  cap_plain | grep -qE '▀|▄'; check $? "T30 md local image halfblocks render"
  cap_esc  | grep -qF  '38;2;'; check $? "T31 image pixels truecolor SGR"
  cap_plain | grep -q 'text before image'; check $? "T32 text around image intact"
  cap_plain | grep -q 'unavailable'; check $? "T33 missing image error line"
  quit_app

  pane_run "DLOOK_IMAGE_PROTOCOL=halfblocks $BIN $FIX/img/tiny.png"
  for ((i = 0; i < 60; i++)); do
    cap_plain | grep -qE '▀|▄' && break
    sleep 0.1
  done
  cap_plain | grep -qE '▀|▄'; check $? "T34 direct image open renders"
  cap_plain | grep -q 'tiny.png'; check $? "T35 image mode header shows filename"
  # 图片模式滚动到尾(2 行图,应到文档底)不崩,footer 正常
  T send-keys -t "$SESS" G; sleep 0.3
  cap_plain | grep -q 'q quit'; check $? "T36 image mode G/footer intact"
  quit_app
}

# ===========================================================================
# T-resize
# ===========================================================================
scenario_resize() {
  echo "== T-resize =="
  pane_run "$BIN $FIX/style.md"
  wait_plain 'H1 Heading One'
  T resize-pane -t "$SESS" -x 120 -y 24
  sleep 0.6
  cap_plain | grep -q 'H1 Heading One'; check $? "T28 content survives resize 100x30→120x24"
  cap_plain | grep -q '╭'; check $? "T29 table frame after resize"
  quit_app
}

main() {
  command -v tmux >/dev/null || { echo "error: tmux not found"; exit 1; }
  [ -x "$BIN" ] || { echo "error: binary not found: $BIN (build: cd rs && cargo build --release)"; exit 1; }
  cd "$ROOT"   # pane 的工作目录继承自此;相对路径 BIN 依赖它

  start_session
  scenario_style
  scenario_langs
  scenario_mermaid
  scenario_exit
  scenario_selection
  scenario_autoscroll
  scenario_resize
  scenario_images

  echo
  echo "RESULT: PASS=$PASS FAIL=$FAIL"
  [ "$FAIL" -eq 0 ]
}

main
