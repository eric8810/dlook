#!/usr/bin/env bash
# V 场景（design §6 V1–V5）：真实终端（Hyprland + foot）截图断言。
#
# 用法：  bash test/e2e/run-visual.sh
#         BIN=rs/target/release/dlook bash test/e2e/run-visual.sh
#         DLOOK_VISUAL_OUT=/tmp/vshots bash test/e2e/run-visual.sh
#
# 说明：
#   - 被测二进制一律用**绝对路径**调用（PATH 里的旧版 dlook 会误导，见
#     docs/research/media/experiments/README.md E13 记录）。
#   - 需要图形会话（Hyprland + foot + grim + wtype + ImageMagick + tesseract）；
#     任一缺失 → 打印 SKIP 并以 0 退出（不伪装通过）。
#   - 实现的断言在 test/e2e/run_visual.py；结束时清理所有测试窗口与进程
#     （只处理带本次 run 唯一环境标记的进程，不影响并行 agent）。
set -u

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="${BIN:-$ROOT/rs/target/debug/dlook}"
case "$BIN" in
  /*) ;;
  *) BIN="$ROOT/$BIN" ;;
esac
BIN="$(readlink -f "$BIN" 2>/dev/null || echo "$BIN")"

export DLOOK_ROOT="$ROOT"
export DLOOK_BIN="$BIN"

if [ ! -x "$BIN" ]; then
  echo "✗ 被测二进制不可执行: $BIN"
  echo "  先构建(绝对路径调用): cargo build --manifest-path $ROOT/rs/Cargo.toml"
  exit 1
fi

PY="python3"
[ -x "$ROOT/.venv/bin/python" ] && PY="$ROOT/.venv/bin/python"

echo "run-visual: BIN=$BIN"
exec "$PY" "$ROOT/test/e2e/run_visual.py" "$@"
