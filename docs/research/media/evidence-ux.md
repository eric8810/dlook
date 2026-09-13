# dlook 媒体交互调研：键位 / 鼠标 / 状态栏惯例（evidence-ux）

> 调研日期：2026-09-13。范围：交互设计（键位表、鼠标行为、状态栏形态），不含音频/视频技术选型。
> 全部结论基于官方文档、官方源码、官方 issue/PR 网上核验，来源逐条标注。区分「官方支持」与「社区拼装」。

---

## 0. 当前结论（TL;DR）

1. **Space = 播放/暂停是媒体 TUI 的压倒性惯例**（mpv、musikcube、termusic、所有 GUI 播放器一致；ncspot 例外用 Shift+P）。dlook 媒体模式下应让 Space 让位于播放/暂停，翻页交给 PageDown/PageUp/j/k/↑↓ —— pager 的滚动能力不丢失，冲突可干净消解。
2. **click-to-seek 有硬先例**：ncspot 官方实现「点击进度条 → 按列比例 seek」（`x/width × duration`），cmus 官方 mouse 绑定 `mlb_click_bar=player-pause`、`mouse_scroll_up_bar=seek +5`；mpv OSC seekbar 也是 left-click=seek。dlook 的 LinkSpan 命中测试架构完全可复用（进度条是固定 UI 行，比内容坐标的 LinkSpan 更简单）。
3. **TUI 播放器鼠标支持是普遍现实**：ncspot（滚轮列表滚动、滚动条拖拽、进度条点击/滚轮 seek、音量区滚轮）、cmus（默认关闭，`set mouse=true` 开启）、musikcube（鼠标 seek）、w3m（链接点击/中键返回/底行可点击导航）。termusic 是唯一只支持滚轮的。
4. **状态栏形态收敛为「底部 2 行」**：进度条行 + 信息行（ncspot 布局；cmus 也是「second row from the bottom」状态行 + 进度条）。时间码 `MM:SS / MM:SS` 是标准，mpv 终端状态行加百分比 `(13%)`。
5. **倍速键惯例分裂**（mpv `[ ] { }`、termusic `Ctrl+f/b`），非 MVP 必须；媒体键（crossterm `KeyCode::Media`）需要 Kitty 键盘协议，老终端收不到，只能做渐进增强。
6. **歌词跟随播放 = LRC 时间戳 + 定时器轮询**（termusic 按播放进度查当前行；sptlrx 200ms 定时器）。dlook 事件循环已有 200ms poll 周期，天然适配。

---

## 1. 调研对象与证据等级

| 项目 | 角色 | 证据来源 | 等级 |
|---|---|---|---|
| ncspot | Rust/ncurses(cursive) Spotify 客户端，最接近 dlook 技术栈 | 官方 `doc/users.md`、官方源码 `src/ui/statusbar.rs`、PR #47 | A（官方文档+源码） |
| cmus | C/ncurses 轻量播放器，键位设计最完整 | 官方 manual（cmus.1） | A |
| musikcube | C++/ncurses 全功能播放器 | 官方 wiki user-guide、issue #404、PR #383 | A−（键位文档明确，鼠标交互细节靠 issue 佐证） |
| termusic | Rust/tui-realm 音乐+播客播放器 | 官方源码 `lib/src/config/v2/tui/keys/mod.rs`（默认键位定义）、issue #456 | A |
| mpv | 通用媒体播放器，终端键位标杆 | 官方 `etc/input.conf`、manual（INTERACTIVE CONTROL / TERMINAL STATUS LINE / osc.rst / vo.rst） | A |
| ytfzf | yt-dlp 前端（POSIX 脚本） | 官方 README | A（但项目已停止维护） |
| w3m | 「web browser + pager」混合体（场景 C 参照） | 官方 man page、`doc/README.mouse`、`doc/keymap.default` | A |
| sptlrx / lyrics-in-terminal | 终端同步歌词（歌词跟随参照） | 官方 README | A− |

---

## 2. 各项目键位 / 鼠标惯例表

### 2.1 核心操作键位对照（官方默认值）

| 操作 | ncspot¹ | cmus² | musikcube³ | termusic⁴ | mpv⁵ | dlook 现状 |
|---|---|---|---|---|---|---|
| 播放/暂停 | `Shift+P` | `x`(play) `c`(pause) | `Space` / `^p` | `Space` | `Space` / `p` | —（Space=翻页） |
| 停止 | `Shift+S` | `v` | `^x` | — | `q`（退出） | — |
| 下一首/上一首 | `>` / `<` | `b` / `z`（专辑 `B`/`Z`） | `l` / `j` | `n` / `Shift+N` | `>` / `<`（Enter=下一首） | 未占用 |
| seek 粗 | `F`/`B` ±1s，`Shift+F`/`B` ±10s | `h`/`l` 及 `←`/`→` ±5s；`,`/`.` ±1min | `o`/`u` ±10s | `f`/`b`（步长可配） | `←`/`→` ±5s；`↑`/`↓` ±60s | 未占用 |
| seek 精 | — | — | — | — | `Shift+←`/`→` ±1s（exact） | — |
| 音量 | `-`/`+` ±1%，`[`/`]` ±5% | `[`/`]`/`{`/`}` ±1%，`-`/`+`/`=` ±10% | `i`/`k` ±5% | `+`/`-` | `9`/`0`（`/`/`*`）±2 | 未占用 |
| 静音 | — | — | `m` | — | `m` | 未占用 |
| 倍速 | — | — | — | `Ctrl+f` / `Ctrl+b` | `[`/`]` ×1.1/0.9，`{`/`}` ×2/0.5，`BS` 复位 | — |
| 回曲首 | — | `x`(replay) | — | `0` | `HOME` seek 0 | — |
| 循环/随机 | `R` / `Z` | `r`（含 `^R` 单曲）/ `s` | `.`(repeat) / `,`(shuffle) | — | `L`/`l` loop | — |
| 显示进度 | 常驻进度条 | 常驻（`t` 切已播/剩余） | 常驻 | 常驻 | `o`/`P` show-progress | — |
| 首/尾 | `g` / `G` | `g` / `G`（home/end） | Home/End | `g` / `G` | `HOME` seek 0 | `g`/`G` ✅ |
| 上/下滚动一行 | `k`/`j` | `k`/`j`（`^Y`/`^E`） | ↑/↓ | `k`/`j` | —（视频场景上下=seek） | `j`/`k`/↑↓ ✅ |
| 翻页 | — | `^B`/`^F`（page_up/down） | PgUp/PgDn | PgUp/PgDn | `PGUP`/`PGDWN`=章节 | `Space`/PgUp/PgDn ✅ |
| 退出 | `Q`（`q` 无效⁶） | `q`（`^C` 提示用 :quit） | `^D` | `q`（`Esc`=关层不退出） | `q` / `Q`(记住进度) | `q`/Esc ✅ |
| 返回/上一层 | `Backspace` 关闭视图 | `Backspace`(browser 上级) | `Esc`(命令栏) | `Esc` 关闭弹层 | — | `⌫`/`Alt+←` ✅ |
| 帮助 | `?` | 视图 7（键位列表） | `?` | `Ctrl+h` | `?`（stats 页） | — |

¹ ncspot `doc/users.md`（hrkfdn/ncspot）。注意 ncspot 的 `Space` 是「加入队列」不是暂停，属少数派。
² cmus manual（cmus.1，KEYBINDINGS 节）。播放三态分立：`x` play / `c` pause / `v` stop。
³ musikcube wiki user-guide（clangen/musikcube）。
⁴ termusic 源码默认键位（`lib/src/config/v2/tui/keys/mod.rs`：`toggle_pause: Char(' ')`、`seek_forward: 'f'` 等），全部可经 `tui.toml` 配置且启动时做冲突检查。
⁵ mpv `etc/input.conf` + manual INTERACTIVE CONTROL 节。
⁶ ncspot 用大写 `Q` 退出以保护 `q` 给未来绑定；dlook 用 `q`（pager 惯例）无需跟进。

**关键分歧与共性**：
- 播放/暂停：`Space` 是多数派（musikcube/termusic/mpv/GUI 世界），pager 世界 `Space`=翻页。**这是 dlook 唯一必须裁决的冲突**。
- seek 的 `←/→` ±5s 只有 mpv 用，但 mpv 是事实标准；cmus 的 `h/l` 同义。dlook 的 `←/→` 当前未绑定（仅 `Alt+←`=返回），可直接采用 mpv 惯例。
- 音量 `-/+` 三家一致（ncspot/termusic/cmus 的 ±10% 档），冲突最小。
- `</>` 切歌：ncspot+mpv 一致。
- 上一首的「rewind 语义」：cmus `rewind_offset=5` —— 当前位置 <5s 时 `prev` 跳上一首，否则回本曲开头（播放器通用语义，实现时注意）。

### 2.2 视图 / mini 模式切换惯例

- **cmus**：7 个视图，`1`-`7` 直达，`tab` 循环（`prev-view`/`left-view`/`right-view` 命令）。设置视图(7)本身是键位浏览器。
- **ncspot**：`F1` 队列 / `F2` 搜索 / `F3` 库 / `F8` 专辑封面（cover feature）；`Backspace` 关闭当前视图返回上层。全屏封面页 = 隐藏文档区的「沉浸模式」先例。
- **musikcube**：`TAB` 在窗格间切换焦点，`~`/`a`/`s` 切主视图；`v` 显隐可视化器（叠加层开关先例）。
- **termusic**：`1`/`2`/`3` 切 库/数据库/播客，`Ctrl+h` 帮助弹层，`Esc` 逐层关闭。
- **mpv**：无视图概念；`o`/`P` 临时显示进度条（transient overlay 先例），`DEL` 循环 OSC 可见性 never/auto/always。

结论：**「一个键切全屏媒体视图 + Esc/Backspace 逐层退回」是被 cmus/ncspot/termusic 共同验证的分层退出模型**，与 dlook 现有「Esc 先清选区再退出」的链式语义同构。

### 2.3 鼠标能力对照表

| 能力 | ncspot | cmus | musikcube | termusic | mpv（窗口） | w3m |
|---|---|---|---|---|---|---|
| 默认开启 | ✅（cursive 自带） | ❌ `mouse=false`，需 `set mouse=true` | ✅ | 仅滚轮 | ✅（`--input-cursor`） | ✅（`-no-mouse` 关闭） |
| 列表滚轮 | ✅（#442 在用，有 bug 报告即证据） | ✅ `mouse_scroll_up=win-up` | ✅ | ✅ | — | ✅ |
| 滚动条拖拽 | ✅（#1073） | — | ？ | ❌ | — | — |
| **进度条点击 seek** | ✅ **官方实现**：`f = x/width; seek(duration×f)`（statusbar.rs） | ⚠️ 官方绑定 `mlb_click_bar=player-pause`（点击条=暂停，非按比例 seek） | ✅（issue #404 证实可鼠标 seek，细节未文档化） | ❌ | ✅ OSC seekbar left-click=seek | — |
| 进度条拖动 scrub | 曾支持 Hold（PR #47），现行版仅 Press | — | ？ | ❌ | ❌（仅点击） | — |
| **滚轮在进度条上** | ✅ seek ±500ms | ✅ `mouse_scroll_up_bar=seek +5`；右键滚轮=`vol +1%` | ？ | ❌ | ✅ OSC seekbar wheel=seek | — |
| 滚轮=音量 | ✅（光标在音量显示区） | ✅（右键滚轮在 bar 上） | ？ | ❌ | ✅ `WHEEL_UP=volume +2`（全局） | — |
| 点击状态行=播放/暂停 | ✅ | ✅（同 mlb_click_bar） | ？ | ❌ | ✅（OSC play 按钮） | — |
| 链接点击 | — | — | — | — | — | ✅ 左键活动链接=跳转 |
| **中键=返回** | — | — | — | — | — | ✅ `button 2 default BACK` |
| 底行可点击导航 | — | — | — | — | — | ✅ lastline `<=UpDn`：点击 `<=`=BACK、Up/Dn=翻页 |
| 滚轮在标题栏=切视图 | — | ✅ `mouse_scroll_up_title=left-view` | — | — | — | — |
| 鼠标开关快捷键 | — | — | — | — | — | ✅ `m` MOUSE_TOGGLE |

来源：ncspot `src/ui/statusbar.rs`（现行 master，2019 由 PR #47 引入）；cmus manual KEYBINDINGS 节的 mouse 事件绑定 + CONFIGURATION `mouse(false)`；musikcube issue #404（"seek back … both by mouse and keyboard"）+ PR #383（owner 为鼠标输入专门做低延迟优化）；termusic issue #456（"mouse may only support the mouse wheel"，点击是 open FR）；mpv `etc/input.conf` 鼠标段 + osc.rst；w3m `doc/README.mouse`。

**判读**：
- click-to-seek 的「按列比例」算法有 ncspot 源码级先例，dlook 可直接照抄公式（命中行内 `x/(行宽) × 总时长`）。
- 「滚轮语义随光标区域变化」是 cmus+ncspot 的共同模式（列表=滚动、进度条=seek、音量区=音量），符合直觉且无全局副作用，适合 dlook。
- cmus 默认关鼠标（ncurses 滚轮延迟历史问题）；dlook 已经默认开鼠标（拖选是核心功能），无需跟进 cmus 的保守策略。

### 2.4 进度条 / 状态栏形态惯例

- **ncspot**（2 行状态栏，底部）：
  - 第 1 行（进度条）：`━` 重水平线字符填充已播部分，背景 `—`；现行版 `"━".repeat(duration_width)`。
  - 第 2 行（信息）：`▶ / ▮▮ / ◼`（NerdFont 可换）+ 曲名，右侧 `MM:SS / MM:SS`，再右 `[R]`/`[R1]` repeat、`[Z]` shuffle 标记。
  - 音量显示区在右端（滚轮热区）。
- **cmus**：状态行位于「second row from the bottom」，右端字段序列 `aaa_mode | volume | continue follow repeat shuffle`；`progress_bar` 选项控制样式：`disabled | line | shuttle | color | color_shuttle`（line=线形，shuttle=游标标记）。`t` 切换显示剩余时间。`format_statusline`/`format_current` 可自定义格式串。
- **musikcube**：底部命令栏（`Esc` 聚焦），播放信息在底部 status 区（截图确认）。
- **mpv（终端形态）**：无 TUI 框架，直接一行滚动状态：`AV: 00:03:12 / 00:24:25 (13%) A-V: -0.000`（`--term-status-msg` 可覆盖格式；含速度 `x2.0`、缓存 `Cache: 2s/134KB` 等条件字段）。这是「一行、时间码+百分比+状态标志」的极简范式。
- **mpv OSC**（窗口形态）：鼠标移动浮现、0.5s 无操作隐藏；seekbar 支持 click-to-seek/滚轮 seek；`DEL` 键循环 never/auto/always。**终端 VO（tct/kitty/sixel）下不适用**：OSC 依赖窗口 VO 的 OSD 图层与窗口内鼠标事件，tct 文档只描述了半块字符渲染与输出同步问题（高置信推断，未实测）。
- **ratatui 0.30**：`Gauge`（`percent`/`ratio`、居中 label 默认为百分比、`use_unicode` 1/8 cell 精度）与 `LineGauge`（细条、左对齐 label）。官方文档确认。1 行媒体栏内建议用 Gauge 时把 label 覆盖为时间码（否则默认百分比 label 会与右对齐时间码重叠）。

**收敛结论**：TUI 媒体状态栏事实标准 = **底部 1-2 行、字符 gauge + 右对齐 `MM:SS / MM:SS`（+可选百分比）、播放状态图标、repeat/shuffle 标记**。cmus 的「已播/剩余切换」和 mpv 的「transient 显示（o/P）」是两个可选增强。

### 2.5 歌词滚动 / 跟随播放（LRC）

- **termusic**（源码 `tui/src/ui/components/lyric.rs`）：歌词面板是 Textarea；每次播放进度更新调用 `parsed_lyrics.get_text(current_track_pos)` —— **按 LRC 时间戳查当前行并刷新显示**；`Shift+F`/`Shift+B` 手动校准歌词偏移（写回 tag），`Shift+T` 多语言歌词帧切换。键盘可手动滚动歌词（↑↓/PgUp/PgDn/Home/End/j/k/g/G）。
- **sptlrx**（Go，299★+ termusic 同类）：独立同步歌词 TUI，兼容 Spotify/MPD/Mopidy/MPRIS/浏览器；内部定时器 `timerInterval: 200ms` 驱动重绘，位置轮询 `updateInterval: 2000ms`；当前行高亮 + 前后行样式区分，长行换行处理。
- **lyrics-in-terminal**（Python curses）：跟随 MPRIS2 播放显示歌词。
- **cmus/ncspot/musikcube**：均无歌词功能。**「歌词跟随」是 termusic/sptlrx 一系（社区需求驱动）而非传统播放器标配**。

**机制共识**：LRC 行时间戳 → 周期性（~200ms）用 `播放进度` 二分/线性查找 `ts <= pos` 的最后一行 → 高亮并滚动到视口锚点。用户手动滚动时暂停跟随（sptlrx 无手动滚动；termusic 允许手动浏览，回跟随靠下一次进度更新——体验粗糙，dlook 若做应加「手动滚动后 N 秒暂停跟随」）。

### 2.6 ratatui / crossterm 能力边界（dlook 实现成本相关）

- **crossterm 0.29 媒体键**：`KeyCode::Media(MediaKeyCode)` 存在，变体 13 个：`Play/Pause/PlayPause/Reverse/Stop/FastForward/Rewind/TrackNext/TrackPrevious/Record/LowerVolume/RaiseVolume/MuteVolume`。**关键限制（官方文档原注）：需要 `KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES`（Kitty 键盘协议，`PushKeyboardEnhancementFlags` 启用）才能读到** —— 仅 kitty/WezTerm/foot/Ghostty/新版 iTerm2 等现代终端支持；xterm 默认、tmux 内、SSH 老终端等场景收不到。→ 媒体键只能作为渐进增强，不能作为唯一路径。
- **MPRIS 是另一条媒体键路径**（OS 级）：ncspot（`mpris` feature）、cmus（`mpris=true` 默认开）、termusic（dbus 依赖）、musikcube（#766：键盘媒体键可控制，任务栏控制器不行）均走 D-Bus。对 dlook（短生命周期 pager）优先级低，且不影响键位表设计。
- **鼠标事件能力**（dlook 已在用）：`Down/Drag/Up/ScrollUp/ScrollDown/Moved` 均有；click-to-seek 需要的 `Down(Left)` + 列坐标、scrubbing 需要的 `Drag(Left)` 节流，现有 `termio.rs` 的 `AUTO_SCROLL_INTERVAL=120ms` 与拖选架构就是先例。无障碍缺口：crossterm 无「双击」事件（mpv 的 dbl-click 全屏无法低成本复刻，不建议做）。
- **ratatui Gauge**：官方支持，见 2.4。**结论：整个建议键位表在现有 crossterm 事件模型内零新增依赖可实现**（媒体键除外）。

---

## 3. dlook 场景设计建议（核心交付）

### 3.0 总体架构：两种媒体形态，而非一种

| 形态 | 触发 | 布局 | 键位域 |
|---|---|---|---|
| **M1 媒体模式**（全屏） | `dlook music.mp3` 直接打开媒体文件 | 文档区=媒体信息视图（元数据/封面/歌词），底部插 2 行媒体栏 | 上下文相关：pager 键 + 媒体键叠加 |
| **M2 mini 媒体栏**（叠加） | 在 markdown 里点击内嵌媒体链接（场景 B） | 文档不动，footer 上方插 2 行媒体栏（可折叠 1 行） | pager 键位保持 + 媒体栏鼠标热区 |

理由：
- dlook 的本体是 pager —— 场景 B 里用户在「读文档」，强行切到全屏播放界面（cmus 式）会打断阅读上下文；mini 栏保住 pager 基因（w3m 的 mailcap 外部打开是反例：完全丢上下文；termusic 播放时仍可浏览库是正例）。
- 场景 A 里「打开的就是媒体」，用户意图是播放，给完整媒体模式 + 信息视图（ncspot `F8` 全屏封面 / cmus 视图先例）。
- M1/M2 共享同一媒体栏组件与键位表，实现上是同一套代码的两个挂载位置。
- 媒体栏是**固定 UI 元素（屏幕坐标）**，不进 Doc.lines 内容坐标系 → LinkSpan 的「内容坐标滚动稳定」难题不存在；hit-test 是常量时间行判定。

### 3.1 建议键位表（媒体模式 M1 / 媒体栏 M2 通用媒体键）

| 键 | 动作 | 依据 | dlook 冲突裁决 |
|---|---|---|---|
| `Space` | 播放/暂停 | mpv/musikcube/termusic 压倒性惯例 | **媒体模式下覆盖翻页**；翻页仍可用 `PageDown/PageUp`（dlook 已绑定）。媒体模式中 Space 翻页需求由媒体信息文档很短这一事实弱化 |
| `p` | 播放/暂停（同义） | mpv | 无冲突，未占用 |
| `←` / `→` | seek −5s / +5s | mpv（±5s 标准） | dlook 未绑定（`Alt+←`=返回保留不动） |
| `Shift+←` / `Shift+→` | seek −1s / +1s（精确） | mpv | 无冲突 |
| `,` / `.` | seek −1min / +1min | cmus | 无冲突 |
| `-` / `+` | 音量 −5% / +5% | ncspot（±1/±5 双档）、cmus、termusic | 无冲突；先做单档 ±5% |
| `m` | 静音切换 | mpv / musikcube | 无冲突 |
| `<` / `>` | 上一首 / 下一首 | ncspot + mpv 双印证 | 无冲突 |
| `0` | 回到本曲开头 | termusic `restart_track`、mpv `HOME seek 0` | 无冲突 |
| `j`/`k`/`↑`/`↓` | **保持滚动**（媒体信息视图/歌词） | dlook pager 基因；cmus 用 h/l=seek 是因为它没有文档 | **不学 cmus**：dlook 的 seek 交给 ←/→，滚动保持 j/k。避免「按 j 结果跳了 5 秒」的背叛感 |
| `g`/`G`/Home/End | 首行/末行 | 现状保留 | 无新增 |
| `q` / `Ctrl+C` | 退出（停止播放） | 现状 | 无冲突 |
| `Esc` | 链式：清选区 → 停止并退出媒体形态 → 退出 | dlook Esc 链（清选区→退出）+ termusic Esc 逐层关 | 与现有「Esc 先清选区」同构，插入中间层 |
| `⌫` / `Alt+←` | 返回上一个文件（**保持原义，退出媒体形态**） | 现状 | **不学 mpv 的 BS=速度复位** —— 返回栈是 dlook 导航核心 |
| `Tab` | M1 内：媒体信息视图 ↔ 歌词/封面视图；M2 内：聚焦切换（预留） | cmus `tab` 视图循环、musikcube `TAB` 窗格 | dlook 未占用 Tab |
| `[` / `]`（P2） | 倍速 ×0.9 / ×1.1 | mpv | 无冲突；MVP 可缓 |
| `t`（P2） | 时间码 已播/剩余 切换 | cmus | 无冲突 |
| 媒体键（增强） | PlayPause/TrackNext/TrackPrevious/Raise/LowerVolume/MuteVolume | crossterm `KeyCode::Media` | 仅 Kitty 协议终端可达；老终端静默降级 |
| `y`/`Enter` | 有选区时复制（保持） | 现状 | 媒体栏区域无文本可选，不冲突 |

**冲突消解原则总结**：媒体键只占用 dlook 未绑定或弱绑定的键（←/→、-、+、m、<、>、,、.、0、p、[、]、t），唯一强冲突 Space 用「模式覆盖」解决；j/k/↑↓/⌫/Esc/q 的 pager 语义原样保留。这样即使误按也不产生不可逆动作。

### 3.2 鼠标行为清单（媒体形态下）

| 手势 | 区域 | 行为 | 依据 |
|---|---|---|---|
| 左键单击 | 媒体栏·进度条行 | **click-to-seek**：`seek((x−bar左端)/bar宽 × 总时长)`，clamp [0,duration] | ncspot 源码级公式 |
| 左键按住拖动 | 进度条行 | **scrubbing**：进入 scrub 态 → `Drag` 事件 ~120ms 节流刷新预览（时间码跟随光标、进度条显示幽灵位置）→ `Up` 才提交真 seek | ncspot PR #47 的 Hold 行为 + dlook AUTO_SCROLL_INTERVAL=120ms 先例 |
| 左键单击 | 媒体栏·信息行（非音量区） | 播放/暂停 | ncspot 点击状态行 / cmus `mlb_click_bar=player-pause` |
| 滚轮 | 进度条行 | seek ±5s（step 与 ←/→ 一致） | cmus `mouse_scroll_up_bar=seek+5` + ncspot ±500ms（取 cmus 步长更实用） |
| 滚轮 | 信息行右端音量显示区 | 音量 ±5% | ncspot 音量热区 |
| 滚轮 | 文档区 | 滚动文档（保持现状） | pager 本能；cmus `mouse_scroll_up=win-up` |
| 左键拖选 | 文档区 | 文本选区 + OSC52（保持现状）；媒体栏行不可选 | 现状 |
| Shift+点击 | 文档区 | 扩展选区（保持） | 现状 |
| 左键单击 | 文档区链接 | 现有跳转规则；**内嵌媒体链接 → 启动/停止 M2** | 场景 B |
| 中键（可选） | 任意 | 返回上一个文件（同 ⌫） | w3m `button 2 BACK` |

**命中测试实现注记**：媒体栏 rect 在 render 时已知（Layout chunk），命中 = `y == bar行 && x ∈ [0, width)`；比例 = `x as f32 / width as f32`（ncspot 同式）。无内容坐标映射成本。scrub 节流复用 `Instant` 间隔模式（现有 120ms 自动滚动）。

**滚轮语义裁决**：光标区域决定语义（列表=滚动、条=seek、音量=音量）优于「媒体模式下全局滚轮=音量」（mpv 窗口模式的做法），因为 dlook 文档区仍需滚动，且区域化语义在 cmus+ncspot 双先例验证过、无意外副作用。

### 3.3 状态栏（媒体栏）形态建议

2 行、位于 footer 正上方（替换 body 高度 2 行）：

```
行1  ━━━━━━━━━━━╾──────────────────────  01:23 / 04:56
行2  ▶ Song Title — Artist        ▣ 80%  R Z   ⌫back
```

- 行 1：`━` 填充已播、`─` 背景（ncspot 现行样式）；右端时间码 `MM:SS / MM:SS`；mpv 风格百分比 `(42%)` 可选（终端窄时优先保时间码）。
- 行 2：播放状态图标（`▶`/`▮▮`/`◼`，ncspot 原始字符集，零 NerdFont 依赖）；曲名—艺人；右端音量 `▣ 80%`（滚轮热区）；repeat/shuffle 状态字母（cmus/ncspot 的 `[R]`/`[Z]` 简化为单字母+色）。
- 实现：ratatui `Gauge`（`use_unicode` 可选精度）或直接手写 Span 拼接（ncspot 式，最省）；label 若用 Gauge 需覆盖为时间码。
- 刷新粒度：挂在现有 200ms poll 循环上（sptlrx timerInterval=200ms 同量级），暂停时静止。
- 小终端（body < ~6 行）：折叠为 1 行（进度条+时间码，信息进 footer 状态消息）。
- 播放/seek/音量操作反馈复用 footer 状态消息（TTL 1.5s，现状机制）：如 `seek +5s → 01:28`、`vol 75%`。

### 3.4 footer 可发现性：分层提示，不爆炸

- **pager 模式**：footer 不变（`q quit ↑↓/jk/scroll space/pgdn g/G top/bottom Ctrl+C quit` + `⌫back`）。
- **M1 媒体模式**：footer **整体替换**为媒体提示，滚动键并入：`space ⏯  ←→ seek  -/+ vol  </> track  m mute  j/k scroll  ⌫back  q quit`（同样一行内完成，字符预算与现 footer 相当）。M1 下 Space=暂停正确；完整 M1/M2 双列键位表以主报告 README 为准。
- **M2 mini 栏激活**：footer 只追加一个短 token：` … ♪ p ⏯`（M2 下 Space 保持翻页，暂停用 `p`——见主报告 README 键位表；本节为旧表述已修订）。
- 状态消息（1.5s TTL）优先覆盖 footer 内容 —— 现有机制天然承担「操作回显」。
- P2：`?` 帮助视图（ncspot `?`/musikcube `?`/cmus 视图7/w3m `H` 全体惯例）列出完整键位表，footer 只留高频 6-7 个。
- 设计底线：**任何模式下 footer 提示 token ≤ 8 个**，超出进帮助视图。

### 3.5 场景推演

**场景 A：`dlook music.mp3`（音频直开）**
1. 识别为音频 → 进 M1：文档区渲染媒体信息视图（标题/艺人/专辑/时长/封面[ratatui-image 已有]/歌词若有 .lrc）。
2. 自动开始播放（用户打开播放器的意图明确；mpv/ytfzf 均是打开即播）。footer 切换媒体提示。
3. 键盘全按 3.1 表；鼠标全按 3.2 表。
4. 播放结束：单曲即停，媒体栏变 `◼` 状态；多文件参数（`dlook a.mp3 b.mp3`）→ `</>` 在列表内切换（mpv playlist 语义）。
5. `q`：停止并退出（130 退出码语义保留给 Ctrl+C）。

**场景 B：markdown 内嵌音频/视频**
1. 语法：媒体链接（`!audio[](x.mp3)` 之类）渲染为可点击媒体元素（LinkSpan 加 kind=media），普通 `<a>` 指向媒体文件后缀同样识别。
2. 点击 → **不推返回栈、不切视图**：挂 M2 mini 媒体栏，播放开始；文档保持原滚动位置可继续读/选（dlook 内容坐标架构保证滚动稳定）。
3. 再点同一链接 / 点击媒体栏播放图标切暂停；`Esc` 链此时多一层：清选区 → 停止 M2 摘除媒体栏 → （若在 M1）退媒体模式 → 退出。
4. 视频链接：M2 播音频不可行时退化为（a）外部 mpv 打开（现状 open_link）或（b）M1 嵌入播放（技术选型 agent 决定）；交互层此键位表两者通用。
5. 判据：媒体栏是「叠加态」不是「导航态」—— ⌫ 返回栈只在 M1 直接打开或链接跳转到新文档时使用。这避免「按返回想回文档结果音乐停了」的错乱。

**场景 C：网页预览（滚动/返回）**
- 复用：⌫/Alt+← 返回栈、j/k/滚轮滚动、链接点击分流（本地/外部）——全部现状。
- 借鉴 w3m 补强（均有官方先例，成本低）：
  - 中键=返回（可选，w3m `button 2 BACK`）；
  - footer 显示历史深度（`⌫back×3`）提示可回退层数；
  - 若页面加载有等待态，footer 状态消息显示加载进度（w3m 无此惯例，dlook 已有 TTL 消息机制，自然延伸）。
- 不引入 w3m 的 `B`/`SPC` 键位重映射 —— dlook 已有等价物（⌫/Space），双绑定徒增混乱。

### 3.6 歌词跟随（P2/P3，媒体模式的增强视图）

- 数据：同目录 `.lrc`（或内嵌 USLT/SYLT，技术选型另定）。解析 `[mm:ss.xx] 行文本` 时间戳。
- 跟随：现有 200ms poll → `pos` 查 `ts <= pos` 的最后一行 → 高亮 + 若该行不在视口中带滚动（锚定视口上部 1/3 或中部）。
- 手动干预：用户 j/k/滚轮滚动 → 暂停跟随 ~3s（超时回跟随，或按 `0`/`g` 立即回跟随）；sptlrx 证明 200ms 粒度足够，termusic 的「无暂停跟随」是反面教材。
- 校准：`Shift+F`/`Shift+B` 偏移 ±0.5s（termusic 惯例），状态消息回显 `lyric +0.5s`。

### 3.7 实现成本注记（给技术选型 agent 的接口约束）

- 全部键位/鼠标建议在 crossterm 0.29 + ratatui 0.30 现有能力内可实现；无新事件类型需求。
- 媒体键需 `PushKeyboardEnhancementFlags(DISAMBIGUATE_ESCAPE_CODES)` + 退出时 Pop，且要有「检测不到就静默降级」策略。
- scrubbing 节流 120ms 与拖选自动滚动同构，可复用同一个节流器。
- 媒体栏 hit-test 是屏幕坐标常量判定，不触碰 Doc.lines/Selection 的内容坐标稳定性。
- 播放进度状态需要从播放线程以 ~100-200ms 粒度回传主循环（现有 poll(200ms) 兼容；若要更顺滑的进度条可缩短 poll 或用 channel 唤醒）。

---

## 4. 关键未知与待检验问题

1. **mpv 终端 VO（tct/kitty/sixel）下 OSC 是否绝对不渲染** —— 结论由 osc.rst（"mouse moved inside the player window"）+ tct 文档（仅描述字符渲染、输出不同步）推断为「不适用」，置信高但未实机验证。若 dlook 内嵌 mpv 需验证。
2. **musikcube 鼠标 seek 的具体手势**（点击比例定位还是拖动）无官方文档，仅 issue #404 证其存在；不影响 dlook 设计（ncspot 公式已足够）。
3. **termusic seek 键 `f`/`b` 的默认步长**未核实（配置项存在，默认值未读）——不影响设计，dlook 定 ±5s。
4. **cmus `progress_bar` 各样式（line/shuttle/color）的确切视觉**未逐一截图核验；不影响 dlook 选 `━` 样式（ncspot 实证）。
5. **Kitty 键盘协议媒体键在 foot/Ghostty/WezTerm/tmux 下的实际到达率**未实测——实现时需要终端矩阵测试；老终端必须静默降级到纯键位。
6. **媒体模式下 Space 覆盖翻页的用户接受度**：方案有惯例支撑（播放器视角 Space=暂停是本能），但 dlook 老用户（pager 肌肉记忆）可能困惑。建议首次进入媒体模式时 footer 状态消息提示一次 `space=pause, pgdn=page`。
7. **媒体栏 2 行在极小终端的折叠规则**（阈值、折叠后信息取舍）需实测定夺。
8. **场景 B 中视频在 M2 的形态**（只播声音？抽帧封面？直接 M1？）依赖技术选型 agent 的解码路径结论，交互规范已预留两种挂载方式。
9. 未调研：ncmpcpp（issue #456 中被提及有较全鼠标）、Cava/vis 等可视化器（与媒体栏叠加布局可能冲突）——若后续需要可补查。

---

## 附：来源清单（全部访问于 2026-09-13）

- ncspot 键位：https://github.com/hrkfdn/ncspot/blob/master/doc/users.md
- ncspot 鼠标 seek 实现：https://github.com/hrkfdn/ncspot/pull/47 、https://github.com/hrkfdn/ncspot/blob/master/src/ui/statusbar.rs
- ncspot 鼠标在用证据：issues #442、#1073
- cmus manual（键位/鼠标绑定/选项）：https://man.archlinux.org/man/cmus.1.en
- musikcube user-guide：https://github.com/clangen/musikcube/wiki/user-guide ；鼠标：issues #404、#766，PR #383
- termusic 默认键位源码：https://github.com/tramhao/termusic/blob/master/lib/src/config/v2/tui/keys/mod.rs ；歌词：tui/src/ui/components/lyric.rs ；鼠标现状：issue #456
- mpv 默认键位：https://github.com/mpv-player/mpv/blob/master/etc/input.conf ；manual：INTERACTIVE CONTROL / TERMINAL STATUS LINE（mpv.io/manual/master）；OSC：DOCS/man/osc.rst；终端 VO：DOCS/man/vo.rst（tct/kitty/sixel 节）
- ytfzf：https://github.com/pystardust/ytfzf（README 声明 no longer maintained）
- w3m：man page（archlinux）；鼠标默认映射：https://github.com/tats/w3m/blob/master/doc/README.mouse ；默认键位：doc/keymap.default
- sptlrx：https://github.com/raitonoberu/sptlrx（README：timerInterval 200ms）
- lyrics-in-terminal：https://github.com/Jugran/lyrics-in-terminal
- crossterm KeyCode/MediaKeyCode：https://docs.rs/crossterm/0.29.0/crossterm/event/enum.KeyCode.html 、enum.MediaKeyCode.html
- ratatui Gauge/LineGauge：https://docs.rs/ratatui/0.30.0/ratatui/widgets/struct.Gauge.html
- dlook 现状：`rs/src/termio.rs`（footer 常量 L45、键位映射 L655-679、鼠标处理 L700-734、状态 TTL L49）
