---
id: media-3
package: rs
module: video.rs
status: ready
depends-on: []
---

# media-3：视频会话（mpv 委托 + IPC 控制）+ E14 区域原型

## objective

实现 `rs/src/video.rs`：mpv 会话管理（spawn / JSON IPC 控制 / 状态轮询 / 生命周期 / 区域几何），
并完成 **E14 原型实验**：在真实终端确认 mpv 的 kitty 与 sixel VO 的区域几何能力，
给出共屏形态的可行性结论。

## context

- 设计：[docs/design/media.md](../../design/media.md) §2/§4/§5
- 研究依据：[evidence-video.md](../../research/media/evidence-video.md)（官方 vo、IPC、已知坑：
  启动与退出各发一次 `\033_Ga=d` 清屏）、[README §6](../../research/media/README.md)（E10 第二客户端验证先例）
- 冻结接口：`rs/src/video.rs` 脚手架（**不得改签名**）
- 本机环境：mpv v0.41.0（`--vo=kitty`/`--vo=sixel` 均可用）、Hyprland + foot（sixel）图形会话，
  测试素材 `docs/research/media/experiments/test-video.mp4`（3s 640×360，含 440Hz 音轨）
- 无新依赖：IPC 用 `std::os::unix::net::UnixStream` 手写 JSON 行协议（**不要引入 mpvipc 等 crate**）

## path

- `rs/src/video.rs`（独占）
- `docs/research/media/experiments/`（E14 实验脚本与记录；不改其他研究文件）

## 实现要点

1. **available()**：缓存探测——PATH 中 `mpv` 存在且 `mpv --version` 成功（超时 2s）。
2. **start(src, area, proto)**：
   - `proto == None` → Err("no graphics protocol")（集成层走降级链）。
   - 按 proto 拼 vo：Kitty → `--vo=kitty --vo-kitty-left/top/rows/cols=<area> --vo-kitty-alt-screen=no`；
     Sixel → 以 E14 结论为准（若无区域参数：Err 交给集成层决策全屏/降级，或按 E14 可行方案）。
   - 固定参数：`--no-terminal --really-quiet --input-ipc-server=<随机临时路径>
     --loop=no --audio-display=no`；保留音频输出（视频音轨）。
   - spawn 后等 socket 就绪（≤5s，超时 Failed）；`snapshot().status = Loading → Playing`。
3. **IPC 客户端**：`UnixStream` 连接；发送 `{"command":[...]}\n`；`get_property` 解析响应。
   命令封装：`set_property pause true/false`、`seek <Δ> relative`、
   `seek <frac*100> absolute-percent`、`set_property volume 0-100`、`quit`。
4. **tick()**（事件循环 200ms 调用）：轮询 `time-pos`/`duration`/`pause`/`volume`/`aid`
   （有音轨则 has_audio=true）；检测子进程退出（`try_wait`）→ status Finished/Failed；
   收尸并清理 socket 文件。
5. **生命周期**：`stop()` 幂等——发 quit、等待 ≤2s（超时 kill）、join、删 socket；
   `Drop` 不 panic（不能依赖 Drop 收尸，集成层显式 stop）。
6. **set_area(area)**：实现选择——优先 `set_property` 热改（若 E14 证明可用），否则
   记录为「需重启会话」（返回 / 置标记由集成层决定重启），**在报告中明确选择与依据**。
7. **resize/几何**：area 由集成层按布局计算；本模块只负责传递。

## E14 原型实验（必须完成并记录）

在 Hyprland + foot（sixel）真实会话中，最小化验证：
- kitty VO 区域参数在**非 kitty 终端**的行为（foot 用 sixel；kitty 参数仅作参数校验）；
- **sixel VO 是否有区域几何能力**（`mpv --vo=sixel --list-options | grep vo-sixel`），
  有则实测把画面限制在指定矩形；无则记录「sixel 只能全屏/游标起点」及依据；
- 结论写入 `docs/research/media/experiments/README.md`（追加 E14 小节）+ 脚本
  `e14-mpv-region-probe.py`（可复现）。

## verification

单元/集成测试（`cargo test video::`，本机有 mpv，可跑真实子进程）：
1. `available()` 为 true（本机）；伪造 PATH 为空的场景以下不要求。
2. `start(test-video.mp4, area, Kitty)` → 2s 内 snapshot.status == Playing；
   `toggle_pause` 后 paused == true；`seek_by(1.0)` 后 position ≥0.8s；
   `adjust_volume(-0.2)` 后 volume 下降；`stop()` 后子进程消失（`pgrep -f input-ipc-server=<path>` 为空）。
3. `start(..., TermProto::None)` → Err 且信息可读。
4. 失败路径：`start("/nonexistent.mp4", ...)` → Failed 且原因可读；mpv 缺失场景以
   注入假 PATH（`PATH=/nonexistent` 環境变量构造）验证 `available()==false`（测试内使用
   `std::process::Command::env` 隔离，不改全局状态）。
5. tick 幂等：连续 tick 100 次不 panic、不泄漏 fd（可粗查 /proc/self/fd 数量稳定）。
6. E14 结论：kitty 区域参数被接受（本机 `--list-options` 断言存在）；sixel 区域能力
   有明确结论（有/无 + 命令证据）。

运行：`cd rs && cargo test video::`（含真实 mpv 子进程用例；超时上限 60s）。

## 交付要求

- 消融实验：尝试删去状态轮询（改用 observe_property 事件订阅）或删去 socket 就绪等待，
  说明删减后破坏了什么（预期：事件订阅更复杂且需读线程；无就绪等待会竞态失败），记录结论。
- 返回：代码、测试结果、E14 结论（含证据命令）、消融结论、未完成事项。
- 不得修改脚手架接口签名；sixel 区域结论若要求接口变更，交回主 agent 决定。
