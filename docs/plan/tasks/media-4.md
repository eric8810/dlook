---
id: media-4
package: rs
module: termio.rs / doc.rs / args.rs / main.rs / content.rs / lang.rs / test/e2e
status: in-progress
depends-on: []
---

# media-4：UI 集成（媒体栏 / 键位上下文 / 鼠标区 / 会话生命周期 / 降级链）

## objective

在 `termio.rs` 等文件接入三个引擎模块，交付用户可见的完整媒体体验：
M1（媒体即文档）/ M2（浏览器文档 + 音频栏）两套键位上下文、媒体栏渲染与鼠标区、
会话生命周期、视频共屏（mpv 画 body、dlook 画 chrome）、网页预览路径、降级链与非 TTY 行为。

**启动条件说明**：三个引擎模块（media.rs / web.rs / video.rs）的接口已冻结并入库，
本任务可立即并行开发；但**运行期 E2E 需等 media-1/2/3 落地**。若开发中引擎仍为
`todo!()`，先完成实现与静态检查，E2E 用例写好待跑，并在报告中标注待验项。

## context

- 设计（必读）：[docs/design/media.md](../../design/media.md) §3 交互规范、§4 模式行为与降级链、
  §5 集成要点、§6 验收场景
- 交互依据：[evidence-ux.md](../../research/media/evidence-ux.md) §3（键位/鼠标/栏形态）
- 冻结接口：`media::AudioCtx` / `video::VideoCtx` / `web::render` / `images::ImageCtx::graphics_proto`
- 既有代码：`termio.rs` 的事件循环/render_frame/鼠标与键盘分发、`images.rs` 的 dirty 检测模式
- 测试基建：`test/e2e/run_acceptance.py`（pyte 套件 A–N，138 项）、`test/e2e/run-tmux.sh`（T1–T36）、
  `docs/research/media/experiments/`（E9 null-sink 探针、E10 IPC 旁观、E11–E13 视觉/闭环脚本可参考）

## path（独占）

- `rs/src/termio.rs`、`rs/src/doc.rs`、`rs/src/args.rs`、`rs/src/main.rs`、
  `rs/src/content.rs`、`rs/src/lang.rs`、`test/e2e/`（新增/扩展测试）
- **不得修改**：`media.rs` / `web.rs` / `video.rs` / `images.rs` / `markdown.rs`

## 实现要点

### A. 布局与媒体栏（render_frame）
1. `Layout::vertical([Length(1) header, Min(0) body, Length(bar_h) bar, Length(1) footer])`，
   `bar_h = 0`（无会话）| `1`（body_h<6 折叠）| `2`（正常）。
2. 媒体栏内容按 design §3：行1 进度条（`━`/`─`）+ `MM:SS / MM:SS`；行2 `▶/▮▮/◼` + 标题 + 右端
   `▣ NN%`（静音 `mute`）。数据来自 `AudioSnapshot` 或 `VideoSnapshot`。
3. footer 分层：M1 整体替换为媒体键位；M2 追加 `  ♪ p ⏯`；普通 pager 不变。

### B. 键位上下文
4. `M1 = mode ∈ {Audio,Video}`；`M2 = 其他模式 + 音频会话活跃`。按 design §3 键位表实现全部键
   （Space 在 M2 保持翻页、`p`/`Shift+Space` 播放暂停、←→/Shift/`,`/`.` seek、`-`/`+`/`m`、
   `0`、`o` 仅 Web、媒体键映射）。
5. Esc 链与 ⌫/Alt+←/q/Ctrl+C 语义按 design §3（会话层插入到现有链）。

### C. 鼠标区（命中测试基于渲染产生的矩形）
6. 进度条行：单击 click-to-seek（`frac = (x−left)/width`）；拖动 scrubbing（120ms 节流，
   预览时间码可仅状态栏提示，松开提交 `seek_to_fraction`）；滚轮 seek ±5s。
7. 信息行：非音量区单击 = 播放/暂停；右端 8 列 = 静音（单击）/ 音量 ±5%（滚轮）。
8. 中键 = 返回；文档区鼠标行为完全保持（滚动/拖选/链接点击）。
9. 媒体栏行与视频区排除出选区/复制/链接命中。

### D. 模式与会话
10. `Mode::Audio`：打开即 `AudioCtx::open`（本地或 URL）；body 显示信息块（标题/时长/格式/操作提示）；
    离开（⌫/q/Esc 到退出）→ `close()`。
11. `Mode::Video`：启动 `VideoCtx::start(src, area, proto)`；`area = (H_MARGIN, 1, content_width, body_rows)`；
    `proto` 由 `ImageCtx::graphics_proto()` 映射；body 区单元格每帧 `CellDiffOption::Skip`
    （ratatui Buffer API），dlook 不向该区域写字节；进入/退出与 mpv 就绪后各做一次
    `terminal.clear()` + 全量重绘（mpv `\033_Ga=d` 清屏坑，design §5.4）。
12. `Mode::Web`（CLI URL 或本地 .html）：后台线程 `web::render` → dirty 计数 → 重建 Doc；
    Loading/Failed 用占位/错误行；`o` 交系统打开器（复用既有 xdg-open 逻辑）。
13. **降级链**（design §4）：视频无 mpv → 状态栏 `mpv not found` + ffmpeg 首帧静图
    （`ffmpeg -i <f> -frames:v 1 -f image2pipe -vcodec png -` → 现有图片管线；无 ffmpeg → 信息行）；
    无图形协议同样走此链；音频无设备 → 栏内 `✗ <原因>` + 提示行。
14. markdown 链接点击：指向**音频**文件的链接 → 就地启动 M2 会话（不导航）；
    指向视频/图片/.html 的链接 → 应用内跳转对应模式（推历史栈）。
15. 非 TTY（main.rs）：Audio/Video/Web 报 `error: '<arg>' needs a terminal` 退出 1。
16. 热重载：媒体模式不参与 stat 重排（design §4）；普通模式行为不变（回归）。
17. resize：重算 bar 高度与视频 area；视频 area 变化调用 `video.set_area`（按其实现选择热改/重启）。

### E. E2E 场景（写入 test/e2e/）
18. 在 `run_acceptance.py` 追加场景 **O（音频 O1–O12）/ P（视频 P1–P8）/ Q（网页 Q1–Q9）/
    R（鼠标交互 R1–R6）**，对齐 design §6 表格；音频断言用 E9 探针方式（null sink 可选，
    无设备环境自动跳过的分支要显式标记）；视频用 E10 旁观客户端 + `/proc/<pid>/cmdline`。
19. 新增 `test/e2e/run-visual.sh`（或 .py）承载 **V 场景（V1–V5）**：真实终端（Hyprland+foot）
    截图断言视频四态、共屏不破图、dlook 图片/视频渲染视觉复核——参考 E11–E13 脚本。

## verification

- `cd rs && cargo test`：既有 40 项 + 引擎任务新增项全绿；
  `cargo build --release` 无新增警告。
- 回归：`BIN=rs/target/release/dlook python3 test/e2e/run_acceptance.py` → A–N 138 项全绿；
  `bash test/e2e/run-tmux.sh` → T1–T36 全绿（体积/行为不变部分）。
- 新增：O/P/Q/R 场景全绿（依赖引擎落地；未落地时报告标注）。
- 手工/视觉：V 场景脚本在本机图形会话跑通，截图存档并给出断言结果。

## 交付要求

- 消融实验：尝试删去媒体栏独立高度分支（固定 2 行）或删去视频降级链中间层
  （无 mpv 直接信息行），说明删减后破坏了什么（预期：折叠行是窄终端可用性；降级链中间层
  是无 mpv 但可截帧场景的主要体验），记录结论。
- 返回：代码、测试结果（含未跑通项与原因）、消融结论、未完成事项。
- 引擎接口若发现设计缺陷，交回主 agent 协调，不自行改引擎文件。
