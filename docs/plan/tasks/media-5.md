---
id: media-5
package: rs
module: test/e2e（集成验证，只读被验代码）
status: pending
depends-on: [media-1, media-2, media-3, media-4]
---

# media-5：集成验证（独立执行者）

## objective

在 media-1..4 全部通过独立验收后，对**真实调用链**做端到端集成验证：
新场景 O/P/Q/R/V 全绿 + 既有回归全绿 + 真实终端视觉检查，产出可复核的集成结论。

## context

- 设计：[docs/design/media.md](../../design/media.md)（§6 验收场景矩阵）
- 任务：media-1..4 的任务文件与各自验收记录
- 环境：本机 mpv v0.41.0 / ffmpeg / PipeWire（null sink 探针）/ Hyprland+foot（sixel）
- 基线：feat/media 分支上四个任务已验收的合并提交

## 启动条件

media-1..4 均通过独立验收，得到确定的合并提交（在报告中记录 commit 号）。

## verification（必须全部执行并给出证据）

1. **回归**：`BIN=rs/target/release/dlook python3 test/e2e/run_acceptance.py` → A–N 138 项；
   `bash test/e2e/run-tmux.sh` → T1–T36。逐项结果记录（通过数/失败明细）。
2. **新场景**：O1–O12 / P1–P8 / Q1–Q9 / R1–R6 全绿；失败项给出复现命令与原始输出。
3. **真实链路（关键，不得用模拟替代）**：
   - 音频：启动 `dlook <真实 mp3/wav>` → 用 E9 探针（null sink + monitor 录制）验证
     确有样本输出；按键（Space/←→/`-`）后时间码与录音特征变化符合预期。
   - 视频：`dlook <真实 mp4>` → 真实终端（foot）截图四态断言 + `/proc` 断言 mpv 参数 +
     旁观客户端断言 IPC 生效 + 退出后无 mpv 残留进程。
   - 网页：本地起 http server → `dlook http://127.0.0.1:PORT/page.html` 渲染正确；
     canary 断言（server 日志中 dlook 未请求任何子资源）；`o` 键调起浏览器（假 xdg-open 记录参数）。
4. **降级链**（每级都跑）：
   - 无 mpv（`PATH` 注入）→ 首帧静图（有 ffmpeg 时）→ 信息行（无 ffmpeg 时）；
   - 无音频设备（以环境隔离方式模拟或说明本机不可模拟的部分）→ 可读错误且不崩溃；
   - 无图形协议（`DLOOK_IMAGE_PROTOCOL=halfblocks`）→ 视频走降级链。
5. **资源与稳定性**：连续播放/停止 5 个循环无 fd 泄漏、无僵尸进程；`cargo test` 全绿。

## 交付

- 集成验证记录（结论 + 每项证据 + 失败明细 + commit 号）。
- 发现的问题：给出位置、触发条件、证据、影响面；阻塞项交回主 agent 派发修复。
- 不做代码修改（只读被验代码）；修复由开发任务执行者负责。
