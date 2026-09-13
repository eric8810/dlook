# 交付分析：媒体能力（音频 / 视频 / 网页）

- 交付基线：分支 `feat/media`（自 `c768026`：媒体研究文档已入库）
- 设计依据：[docs/design/media.md](../../design/media.md)（Finalized）+ [docs/research/media/](../../research/media/)
- 目标：MVP（音频原生 + 网页 L1/L3）+ 视频委托 mpv，含媒体栏与全套键位/鼠标交互
- 完成条件：四条任务全部独立验收通过 + 集成验证（回归 138/40 + 新场景 O/P/Q/R/V）通过

## 任务划分与依赖

| 任务 | 内容 | 文件归属（独占） | 依赖 | 状态 |
|---|---|---|---|---|
| media-1 | 音频引擎（rodio 薄封装） | `rs/src/media.rs` | — | ready |
| media-2 | 网页渲染（ureq + html2text → 行模型） | `rs/src/web.rs` | — | ready |
| media-3 | 视频会话（mpv IPC 客户端）+ E14 原型 | `rs/src/video.rs`、`docs/research/media/experiments/` | — | ready |
| media-4 | UI 集成（媒体栏/键位/鼠标/会话生命周期/降级链） | `rs/src/termio.rs`、`doc.rs`、`args.rs`、`main.rs`、`content.rs`、`lang.rs`、`test/e2e/` | 冻结接口（脚手架已入库）；**运行期验证需 media-1/2/3 落地** | ready |
| media-5 | 集成验证（独立执行者） | `test/e2e/`（只读被验代码） | media-1..4 均通过独立验收 | pending |

**共享约定（已由主 agent 预先落地，后续任务不得再改）**：
- `Mode::{Audio,Video,Web}` + 扩展名判定 + URL 判定（lang.rs）
- 媒体模式跳过二进制检测（content.rs）
- 依赖声明：rodio 0.22 / html2text 0.17（Cargo.toml）
- 冻结接口签名：media.rs / web.rs / video.rs 脚手架 + `ImageCtx::graphics_proto()`（images.rs）
- 三个引擎模块互不引用；跨模块调用只发生在 termio.rs（media-4 独占）

**文件级并行安全性**：media-1/2/3/4 各自独占上述文件，无交叉写；
唯一共享只读依赖是脚手架接口。因此四者可并行开发（同一工作区，`cargo check`
的构建锁只影响速度不影响正确性）。

## 跨模块对接（真实链路）

```
termio(media-4) ──> media::AudioCtx      (M1/M2 会话; snapshot/dirty/控制)
                ──> video::VideoCtx      (M1; snapshot/dirty/tick/区域)
                ──> web::render          (Web 模式后台线程 → WebDoc → Doc)
                ──> images::graphics_proto + ImageCtx(ffmpeg 首帧降级复用图片管线)
```

集成负责人：主 agent（统一定义已冻结；media-4 完成后的真实链路验证由 media-5 独立执行）。

## 调度

1. 并行派发 media-1/2/3/4（各自独立可验收）。
2. 任一任务返回确定版本 → 立即安排独立验收（验收者不得参与该任务开发）。
3. 验收 `blocked` → 交回原开发者修复 → 再次验收；`pass` → 进入集成分支。
4. media-1..4 全部 pass → 派发 media-5（集成验证：新场景 + 全量回归 + 真实终端视觉检查）。
5. 全部 pass 后合并 `feat/media` → `main`，更新 DECISIONS（D16）与 README。

## 消融要求（各开发任务交付时一并提交）

尝试移除实现中的包装层/中间状态/额外分支，验证需求与接口仍满足：
- media-1：若薄封装层可直接用 rodio 类型替代而无 UI 泄漏，说明并简化。
- media-2：若类型化注解直通 `Line` 的中间结构可删，说明并简化。
- media-3：若区域几何/IPC 轮询有更简路径（如事件订阅替代轮询），给出对比。
- media-4：若能省去 media_bar 独立状态或某级降级分支，说明理由与影响。
删减项、保留理由与验证证据随交付提交。

## 风险与对策

| 风险 | 对策 |
|---|---|
| rodio 0.22 API 变动（引擎重写中） | media.rs 是唯一接触面（薄封装）；锁定 0.22.x |
| mpv sixel 无区域几何参数 | media-3 的 E14 原型先测；无则 sixel 走降级链/全屏，记录结论 |
| 视频区被 dlook diff 覆写 | `CellDiffOption::Skip` 标记（design §5.2），media-4 实现 + V 场景断言 |
| 无音频设备的 CI | O 场景含降级用例；E9 探针用 null sink |
| AC 阻塞（媒体栏/键位体量大） | media-4 拆为「布局+渲染」与「交互命中」两步提交，验收可分次 |
