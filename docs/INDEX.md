# 文档索引

项目文档入口。规则与流程正文见相关文档自身，本索引只做导航。

## 产品与设计

| 文档 | 内容 |
|---|---|
| [README.md](../README.md) | 项目说明、用法、键位、行为、测试与体积 |
| [DESIGN.md](../DESIGN.md) | 初版（Node/vue-tui）设计 |
| [DESIGN-rust.md](../DESIGN-rust.md) | Rust 重写设计 |
| [GAP.md](../GAP.md) | 渲染能力差距对照（vue-tui vs dlook） |
| [DECISIONS.md](../DECISIONS.md) | 决策记录 D1–D16 |

## 设计与交付

| 文档 | 内容 |
|---|---|
| [design/media.md](design/media.md) | 媒体能力（音频/视频/网页）跨模块设计：接口、M1/M2 交互、降级链、验收场景 |
| [plan/analysis/media-mvp.md](plan/analysis/media-mvp.md) | 媒体交付分析：任务划分、依赖、调度 |
| [plan/tasks/](plan/tasks/) | 任务文件 media-1..5（音频引擎 / 网页渲染 / 视频会话 / UI 集成 / 集成验证） |

## 研究

| 文档 | 内容 |
|---|---|
| [research/media/README.md](research/media/README.md) | 媒体能力研究总报告：三路线结论、验证策略、分期 |
| [research/media/evidence-video.md](research/media/evidence-video.md) | 视频：终端协议播放路径、mpv 官方 VO、IPC、帧率证据 |
| [research/media/evidence-audio.md](research/media/evidence-audio.md) | 音频：rodio/symphonia 栈、体积、格式边界、许可 |
| [research/media/evidence-web.md](research/media/evidence-web.md) | 网页：协议层否定结论、三层方案、html2text 实测 |
| [research/media/evidence-ux.md](research/media/evidence-ux.md) | 交互：五款播放器键位/鼠标惯例、媒体栏形态 |
| [research/media/experiments/](research/media/experiments/) | 可复现实验 E1–E14（mpv/sixel/kitty、IPC、chromium、rodio 体积、视觉与音频探针） |
