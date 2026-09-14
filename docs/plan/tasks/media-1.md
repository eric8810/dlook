---
id: media-1
package: rs
module: media.rs
status: in-progress
depends-on: []
---

# media-1：音频引擎（rodio 薄封装）

## objective

实现 `rs/src/media.rs`：dlook 的音频播放引擎，作为 **唯一 rodio 接触面**。交付后可用
`AudioCtx` 完成：打开本地/远程音频开始播放、播放/暂停、相对与绝对 seek、音量/静音、
回到曲首、状态快照、异步事件计数；无音频设备与解码失败给可读错误而不 panic。

## context

- 设计：[docs/design/media.md](../../design/media.md) §2/§3/§4（Finalized）
- 研究依据：[evidence-audio.md](../../research/media/evidence-audio.md)（栈选择、格式边界、风险）、
  [README §4](../../research/media/README.md)（薄封装要求，rodio 0.22 已破坏性改过一次 API）
- 冻结接口：`rs/src/media.rs` 脚手架内的 `pub fn` 签名与文档注释（**不得改签名**）
- 依赖已声明：`rodio = "0.22"`（Cargo.toml）；本机 libasound 1.2.16 可用
- 复用模式：`rs/src/images.rs` 的 `ImageCtx`（后台线程 + Mutex 状态 + dirty 计数）

## path

- `rs/src/media.rs`（独占；不改其他文件）

## 实现要点

1. **设备与播放器**：按 rodio 0.22 实际 API（`DeviceSinkBuilder::open_default_sink()` →
   `Player::connect_new(&mixer)` 或等价路径，以编译期 API 为准）惰性打开设备——
   `AudioCtx::new()` 不接触音频后端，首次 `open()` 才打开；打开失败 →
   `AudioStatus::Failed("no audio device: <原因>")`。
2. **open(src, base_dir)**：后台线程执行；本地路径按 `base_dir` 解析（复用
   `links::normalize`，与图片/链接一致；支持 `file:` URL）；`http(s)` URL 先下载到
   临时文件（ureq，超时 10s、上限 64MB，与图片先例同族）再打开。
   **必须在文件/来源可读后再创建播放器**；完成后 bump dirty 并发布 Ready；
   重复 open 替换当前会话（停旧、起新）。
3. **时长**：`Source::total_duration()` 在 append 前取得并保存（`Player` 不提供总时长）；
   None 表示未知（UI 显示 `?`）。
4. **控制**：`toggle_pause`（`is_paused()` 判定）、`seek_by`（`get_pos()` ± delta，clamp
   到 [0, duration]）、`seek_to_fraction`（duration 未知时忽略）、`adjust_volume`
   （clamp [0,1]，解除静音）、`toggle_mute`（记录静音前音量）、`restart`（seek 0 + play）。
5. **自然结束**：`Player::empty()`（队列空）且位置接近末尾时置 `finished = true`
   并 bump dirty；不做自动重播。
6. **close()**：停播、释放播放器、会话置空（snapshot → None）。
7. **opus 等无解码器格式**：open 失败 → Failed("unsupported codec: opus")，文案可读。

## verification

单元测试（`cargo test`，随 media.rs 内 `#[cfg(test)]`）：
1. `open` 一个本机生成的 WAV（测试内用 `hound` 或手写 RIFF 头，勿依赖外部文件）→
   状态最终为 Ready、`duration` 正确（容差 ±50ms）。
2. 播放推进：`open` 后 sleep ~300ms，`position` 增长 ≥150ms；`toggle_pause` 后
   两次快照差 <20ms；恢复后继续增长。
3. `seek_by(+1.0)` → position 相对至少 +0.8s（容差）；`seek_to_fraction(0.5)` →
   `|pos − duration/2| < 150ms`；`restart` → position < 200ms。
4. 音量：`adjust_volume(0.05)` 后 snapshot.volume 增加且 clamp 生效（连续 +20 次 ≤ 1.0）；
   `toggle_mute` 后 volume == 0.0 且 `muted == true`，再切回恢复原值。
5. 失败路径：`open("/nonexistent.mp3")` → Failed 且原因含 "not found"/可读文案；
   坏字节文件（临时写 64 字节随机）→ Failed 不 panic。
6. dirty：Ready 事件、close() 各使 dirty_version 递增。
7. `AudioCtx::new()` 不打开设备（在无 `$XDG_RUNTIME_DIR`/无设备假设下构造不 panic——
   以本机可复现为准，不强求 CI 无声卡环境）。

运行：`cd rs && cargo test media::`（新增测试全绿 + 既有 40 项不回归）。

## 交付要求

- 消融实验：尝试去掉薄封装（直接暴露 rodio 类型）或去掉后台加载线程，说明删减后
  破坏了什么需求（预期：封装隔离上游 API、后台避免 UI 阻塞），记录结论。
- 返回：代码（本文件内）、测试结果、消融结论、未完成事项。
- 不得修改脚手架接口签名；需要变更交回主 agent。
