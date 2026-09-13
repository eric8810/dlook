# 媒体能力设计（音频 / 视频 / 网页）

状态：**Finalized**（2026-09-13；方案与范围经用户确认，据此实现）

> 依据：[docs/research/media/README.md](../research/media/README.md)（三路线结论、验证策略 §6）、
> evidence-audio / evidence-video / evidence-web / evidence-ux、experiments E1–E13。
> 本文件统一跨模块约定（接口、状态归属、交互规范）；决策背景见 research，不在本文重复推导。

## 1. 目标与范围

| 能力 | 本期范围 | 形态 |
|---|---|---|
| 音频 | ✅ 原生播放（rodio + symphonia） | M1 `dlook song.mp3`；M2 markdown 内点击音频链接就地播放 |
| 视频 | ✅ 委托 mpv（官方 kitty/sixel VO + JSON IPC） | M1 `dlook clip.mp4`（共屏：mpv 画 body 区，dlook 画 header/媒体栏/footer） |
| 网页 | ✅ L1 文本渲染（html2text）+ L3 浏览器兜底 | M1 `dlook https://…` 或 `dlook page.html` |
| 网页 L2（chromium 快照）、歌词、倍速、ffmpeg 自绘视频 | ⏳ 不在本期（P2/P3，见 research 分期） | — |

**用户裁决（2026-09-13）**：
- markdown 内 `http(s)` 链接点击 = **保持浏览器打开**（不改为应用内预览）；
  应用内网页预览仅经 CLI URL 或本地 `.html/.htm` 进入。
- 本期范围 = MVP（音频 + 网页）+ **同步推进视频**。

## 2. 模块与状态归属

```
main.rs        入口:模式分派 + 非 TTY 处理
lang.rs        Mode 扩展:Audio / Video / Web + 扩展名判定        [脚手架已完成]
content.rs     媒体模式跳过二进制检测/文本读取                    [脚手架已完成]
media.rs       音频引擎(rodio 薄封装):唯一 rodio 接触面           [task media-1]
web.rs         网页抓取 + html2text 渲染 → 行模型                 [task media-2]
video.rs       mpv 会话(IPC 客户端 + 生命周期)                    [task media-3]
termio.rs      UI 集成:媒体栏、键位上下文、鼠标区、会话生命周期    [task media-4]
doc.rs         行模型(媒体模式复用;不新增媒体状态字段)
images.rs      图片管线(既有 D15);新增 graphics_proto() 供视频选 vo [脚手架已完成]
```

**状态归属原则**：媒体状态只存在于 `AudioCtx` / `VideoCtx`（事件循环持有，跨 rebuild 存活），
`Doc`/`UiState` 不复制媒体状态；UI 每次重绘前调用 `snapshot()` 读取。
异步事件（加载完成、播放结束、失败）通过各自 `dirty_version()` 计数通知事件循环，
与图片 `ImageCtx` 的模式一致（事件循环 200ms poll 周期检测）。

**冻结接口**：见各模块脚手架内 `pub fn` 签名与文档注释（media.rs / web.rs / video.rs）。
实现方不得改变签名；需要变更交回主 agent 统一协调。

## 3. 交互规范（M1 全屏媒体 / M2 媒体栏）

**上下文判定**：
- **M1**：`doc.mode ∈ {Audio, Video}`（媒体即文档）。
- **M2**：`doc.mode ∈ {Markdown, Code, Mermaid, Web}` 且音频会话活跃（markdown 内点击音频链接启动）。
- 其余为普通 pager 模式，行为与 v0.4.0 完全一致。

**键位表**（唯一事实来源 = research/evidence-ux §3.1 + 下表裁决列）：

| 键 | M1 | M2 | 备注 |
|---|---|---|---|
| `Space` | 播放/暂停 | **保持翻页**（不覆盖） | M2 暂停走 `p` |
| `p` / `Shift+Space` | 播放/暂停 | 播放/暂停 | — |
| `←`/`→` | seek ∓5s | 同左 | `Alt+←` 仍是返回 |
| `Shift+←`/`Shift+→` | seek ∓1s | 同左 | — |
| `,`/`.` | seek ∓60s | 同左 | — |
| `-`/`+` | 音量 ∓5% | 同左 | 会解除静音 |
| `m` | 静音切换 | 同左 | — |
| `0` | 回到曲首 | 同左 | — |
| `j`/`k`/`↑`/`↓`/`PgUp`/`PgDn`/`g`/`G`/`Home`/`End` | 保持滚动 | 保持滚动 | pager 基因不丢 |
| `⌫`/`Alt+←` | 返回（停止会话） | 返回（停止会话） | 导航语义不变 |
| `q`/`Ctrl+C` | 退出（停止会话） | 退出（停止会话） | 退出码不变（0/130） |
| `Esc` | 链式：清选区 → 停止播放 → 退出 | 链式：清选区 → 停止会话并隐藏媒体栏 → 原有退出 | — |
| `o` | 视频/音频：无操作（预留） | — | 网页模式：浏览器打开当前页 |
| 媒体键（`KeyCode::Media`） | PlayPause/TrackNext/Prev/VolumeUp/Down/Mute 映射同语义动作 | 同左 | 老终端收不到，静默忽略 |

**鼠标区**（命中测试基于渲染时计算的矩形，屏幕坐标）：

| 区域 | 单击 | 拖动 | 滚轮 |
|---|---|---|---|
| 媒体栏·进度条行 | click-to-seek：`frac = x/width` | scrubbing：120ms 节流预览（时间码跟随），松开提交 | seek ±5s |
| 媒体栏·信息行（非音量区） | 播放/暂停 | — | — |
| 媒体栏·信息行右端音量区（末 8 列） | 静音切换 | — | 音量 ±5% |
| 文档区 | 保持现状（链接跳转 / 拖选复制） | 保持现状 | 滚动文档 |
| 任意位置·中键 | 返回（同 ⌫，w3m 先例） | — | — |

**媒体栏布局**（替换 body 底部 N 行）：

```
行1  ━━━━━━━━━━━╾──────────────────────  01:23 / 04:56
行2  ▶ Song Title — Artist                ▣ 80%
```
- 正常高度 2 行；`body_h < 6` 时折叠为 1 行（进度条 + 时间码）。
- 行1 进度条：`━` 已播 + `─` 未播（实心填充比例 = position/duration；时长未知时用 `?`）。
- 行1 右端时间码 `MM:SS / MM:SS`；行2 左端状态图标（`▶` / `▮▮` / `◼`）+ 标题；右端音量
  `▣ 80%`（`▣` 静音时改为 `▁` 或 `mute`）。
- 视频模式：媒体栏同款（数据来自 `VideoSnapshot`）。
- 刷新：挂在既有 200ms poll；暂停时位置冻结、不产生额外重绘。
- footer 分层：M1 整体替换为媒体键位（`space ⏯  ←→ seek  -/+ vol  m mute  j/k scroll  ⌫back  q quit`）；
  M2 在既有 footer 后追加 `  ♪ p ⏯`。

**动作反馈**复用状态栏消息（1.5s TTL）：`seek +5s → 01:28`、`vol 75%`、`muted`、
`opened in browser: <url>`、`no audio device`、`mpv not found — showing first frame` 等。

## 4. 模式行为与降级链

| 模式 | 正常 | 降级 |
|---|---|---|
| Audio | 媒体栏 + 控制 | 无音频设备 / 解码失败 → 媒体栏显示 `✗ <原因>`，文档区给提示行，不退出 |
| Video | 委托 mpv（kitty/sixel），mpv 画 body 区，dlook 画 chrome | ① 无 mpv → `mpv not found` 提示 + ffmpeg 首帧静图（既有图片管线）② 无 ffmpeg → 信息行（文件名/时长/分辨率）③ 无图形协议（halfblocks）→ 同 ①/② |
| Web | 后台抓取 + 文本渲染 | 抓取/渲染失败 → `✗ <原因>` 行 + 状态栏提示；本地文件缺失 → 同图片的 not found 语义 |

**非 TTY**（管道）：Audio/Video/Web 与 Image 一致——报错 `error: '<arg>' needs a terminal` 退出 1。

**热重载**：媒体模式不参与 stat 轮询重排（播放中的文件被外部修改不打断会话）；
Web 本地文件与 URL 亦不自动重取。文档（README/help）记录该边界。

**导航**：
- 点击 markdown 中指向音频文件的链接 → **就地启动 M2 会话**（不跳转、不推历史栈）。
- 点击指向视频/图片/网页文件（或 `.html`）的链接 → 应用内跳转到对应模式（推历史栈，⌫ 返回）。
- 网页预览内的链接 → 与 markdown 同规则：`http(s)` 交系统浏览器；本地文件应用内跳转。
- `o` 键：Web 模式打开当前页于浏览器；Audio/Video 预留不动作。

## 5. 集成要点（termio.rs / task media-4）

1. **布局**：`Layout::vertical([Length(1) header, Min(0) body, Length(media_bar_h) bar, Length(1) footer])`；
   `media_bar_h = 0 | 1 | 2`（无会话 0；折叠 1；正常 2）。视频模式下 body 即 `VideoArea`。
2. **视频区不被覆写**：进入视频模式后，body 矩形内单元格每帧标记
   `CellDiffOption::Skip`（ratatui 0.30 Buffer API，ratatui-image 同款技术），
   保证 dlook 的 diff 渲染永不向该区域写字节；退出/失败恢复普通渲染。
3. **重绘时机**：`AudioCtx/VideoCtx::dirty_version()` 变化 → 重建 Doc + 清选区（复用图片的
   dirty 检测路径）；视频区域几何变化（resize）→ `video.set_area()`（实现选择热改或重启，
   见 media-3 说明）+ 全量重绘。
4. **全量重绘**：进入/退出视频模式、mpv 启动完成与退出后各触发一次（mpv 会清空终端图像，
   研究 §已知坑：`\033_Ga=d`）。实现手段：`terminal.clear()` 后重绘整帧。
5. **选区**：媒体栏行不参与选区/复制（命中测试排除）；视频区同样排除。
6. **Esc 链**：在现有「清选区 → 退出」中插入会话层（见 §3 键位表 M1/M2 列）。
7. **会话生命周期**：`Mode::Audio` 打开即启动会话；离开（⌫/q/Esc）→ `close()`；
   Video 同理 `stop()`。进程退出前确保 mpv 子进程被 stop（防僵尸，沿用 children 收割模式）。

## 6. 验收场景（映射 research §6 场景矩阵）

| 场景 | 覆盖 | 层 |
|---|---|---|
| **O 音频** | M1 打开/媒体栏/时间码推进-暂停冻结-seek 跳变/音量/Esc 链/M2（Space 仍翻页、p 暂停）/远程 URL/无设备降级/E9 探针 | pty+pyte（O1–O12） |
| **P 视频** | spawn 参数（/proc cmdline）/IPC 控制链（E10 第二客户端）/退出回收码 0/无 mpv 降级/无 ffmpeg 降级/无图形协议降级 | pty+pyte（P1–P8） |
| **V 视觉** | 真实终端截图断言（E11 四态）、dlook 视频共屏不破图、chromium 缺席 L2 回落 | Hyprland+foot 图形会话（V1–V5，可自动） |
| **Q 网页** | L1 渲染（标题/链接/表格）/链接点击交浏览器（用户裁决）/⌫ 返回/安全 canary（不请求子资源）/GBK 不炸/4xx 提示/o 键浏览器 | pty+pyte（Q1–Q9） |
| **R 交互** | click-to-seek 比例断言/scrubbing 节流/滚轮三区语义/footer 分层 token ≤8 | pty+pyte（R1–R6） |
| **回归** | v0.4.0 既有 138 项 pyte + 40 项 tmux 全绿 | 既有套件 |

## 7. 未决与已知边界（记录，不阻塞本期）

- mpv sixel VO 的区域几何能力待 E14 原型确认（kitty 已有官方 `--vo-kitty-*`；sixel 若无区域
  参数，则 sixel 终端下视频降级为全屏形态或走降级链，media-3 记录结论）。
- 视频真实帧率承诺（720p/1080p）需目标终端矩阵实测（research §5 P2 前置②）。
- MPRIS（OS 媒体键）与歌词、倍速、L2 快照：P3，见 research 分期表。
