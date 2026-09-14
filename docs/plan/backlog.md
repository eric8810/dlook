# 剩余问题（不阻塞本次媒体交付）

来源：media-1 独立验收（`/tmp/reviews/media-1-dimagent.md`）等。缺陷已确认但按当前
授权范围内不影响交付；修复后从本表移除并注明版本。

## 待修（有实际影响，优先级降序）

| # | 问题 | 位置 | 影响 | 建议 |
|---|---|---|---|---|
| B1 | `seek_by(NaN)` panic（`Duration::from_secs_f64` 对 NaN 报错→unwrap 路径） | `rs/src/media.rs:426-429` | 当前调用方传字面量故不可达；未来接 UI 输入（如 scrub 比例为 NaN）会崩 | 入口对 NaN/inf 直接 return；顺手加单测 |
| B2 | duration 未知的源永不置 `finished` | `rs/src/media.rs:304-310` | 零数据 WAV 实测永久 `▶ playing` + `duration: --:--`，媒体栏不收敛 | duration 为 None 时以「播放位置停滞 + 队列空」判定结束，或 UI 显示为 live |
| B3 | 暂停后 body 块仍显示 `▶ playing`（媒体栏正确显示 `▮▮ paused`） | `rs/src/termio.rs`（音频信息块渲染） | 视觉不一致，误导用户 | 信息块状态取自 `snapshot().paused` |
| B4 | 无音频设备时 alsa-lib 往 stderr 刷错误，污染媒体栏行 | 引擎/终端初始化 | 设计要求的状态文案本身达标，但多出一行噪声 | 初始化期临时接管 stderr 或过滤，评估成本 |
| B5 | 单测强度不足的三处 | `rs/src/media.rs` 测试 | ① `volume_clamp_and_mute_roundtrip` 不验证 `set_volume` 落设备 ② `reopen_replaces…` 未真触发竞态 ③ `new_is_lazy…` 的 20ms 阈值低于实测冷启动 10.8ms，不足以证明惰性 | ②③ 用更强的可观测判据（如流索引出现/消失、设备打开计数） |
| B6 | E12 音频实验缺静音基线对照 | `docs/research/media/experiments/` | 方法本身正确（ffmpeg pulse）但无对照，结论强度低于 E9 修正版 | 补基线校验（参照修正后的 E9） |

## 未覆盖的验证面（记录，不阻塞）

- 真实 mp3/ogg 端到端播放（本轮只用 WAV）；64MB 上限与 10s 超时未真实触发
- 真实 https/TLS、>5 跳重定向、真实 404 站点（web）
- 未识别字符集（Big5/Shift_JIS/EUC-KR）仍 UTF-8 lossy 乱码（设计已知边界）
- `RichAnnotation::Colour` 忽略、`FragmentStart` 页内锚点未接线、Web L2 快照（P3）
- 视频帧率矩阵实测（720p/1080p，目标终端矩阵）——P2 前置任务
- mpv sixel 区域几何能力（E14 结论见 experiments）

## 已解决（保留短记录）

- `.ts` 被误判为 MPEG-TS 视频 → TypeScript 回归（提交 13ec95b）
- 音频直开路径二次拼接（`dlook tone.wav` 报 not found）→ 提交 045da63 + `direct_src`
- HELP 超出 24 行终端首屏 → 提交 045da63
- **E9 音频探针假阳性**（pw-record 未绑定 monitor，静音也过阈值）→ 提交 fde1abd + 394a016
- `dlook http://…/tone.wav` URL 直开（`direct_src` 把 URL 拼成 `/cwd/http://…`）→ 修复随 media-4 提交
