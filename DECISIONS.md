# 渲染能力补齐:待决策清单

> 依据 [GAP.md](GAP.md) 的差距分析,列出 Rust 版(dlook)补齐渲染能力需要拍板的决策项。
> 每项含选项、代价与推荐;**状态**栏为「待决策」,决策后在「决策记录」区追加结论与日期即可。
> 工作量档:S(≤ 半天)/ M(1–2 天)/ L(≥ 3 天)。

## 总览

| # | 决策项 | 推荐 | 工作量 | 状态 |
|---|---|---|---|---|
| D1 | 标题分级上色 | A:对齐 vue-tui 默认配色 | S | ✅ 已实施 |
| D2 | H1 对齐方式 | A:左对齐 | S | ✅ 已实施 |
| D3 | 链接渲染 | B1:样式化 label(先不做 OSC 8) | M | ✅ 已实施 |
| D4 | 代码语言覆盖 | A:two-face 全量语法集 | M | ✅ 已实施 |
| D5 | 高亮主题选择 | A:暂不做,保持单主题 | — | ✅ 已决策:不做 |
| D6 | 任务列表 checkbox | A:做 | S | ✅ 已实施 |
| D7 | 表格圆角边框 | A:切 ROUNDED preset | S | ✅ 已实施(方案调整) |
| D8 | 数学公式 | A:不做 | — | ✅ 已决策:不做 |
| D9 | 图片(终端图形协议) | A:不做(远期 ratatui-image) | — | ✅ 已决策:不做 |
| D10 | markdown 主题配置化 | A:不做 | — | ✅ 已决策:不做 |
| D11 | 文本拖选与复制 | A:应用内拖选 + OSC 52(分两步) | M | ✅ 已实施(①②均落地) |
| D12 | 产物体积 | 保持全功能 6.59MB;UPX 与功能裁剪均否决 | — | ✅ 已决策 |
| D13 | 项目名/仓库名 | 统一改为 dlook(含 GitHub repo 重命名) | S | ✅ 已实施 |
| D14 | 本地文件链接点击跳转 | A:内容坐标 LinkSpan + 应用内导航栈;外部链接交系统打开器 | M | ✅ 已实施 |
| D15 | 图片渲染(翻案 D9) | 终端图形协议(kitty/sixel/iTerm2+halfblocks 回退)+ 直开图片文件 | L | ✅ 已实施 |

---

## D1 标题分级上色(对应 G1)

**背景**:vue-tui 默认 h1/h2 青、h3/h4 蓝;dlook 全部仅粗体,是当前最大视觉差距。
termimad `MadSkin.headers[i].set_fg()` 原生支持,改动集中在 [termio.rs](rs/src/termio.rs) `build_skin()`。

| 选项 | 说明 | 代价 |
|---|---|---|
| A 对齐 vue-tui 默认 | h1/h2 `cyanBright`、h3/h4 `blueBright`、h5/h6 纯粗体 | 约 15 行;两版观感一致 |
| B 自定配色 | 例如 base16-ocean 色板取色,与代码高亮主题统一 | 需先定设计 |
| C 不做 | 保持纯粗体 | 视觉差距保留 |

**推荐 A**。E2E 影响:H1 用例仅断言 SGR 1(bold)存在,加色不破坏;可顺手加一条颜色断言。

## D2 H1 对齐方式(对应 G2)

**背景**:termimad 默认 H1 居中,vue-tui 全部左对齐(实测)。

| 选项 | 说明 |
|---|---|
| A 左对齐 | `skin.headers[0].align = Left`,与 vue-tui 一致;一屏多标题时阅读动线稳定 |
| B 保留居中 | termimad 默认;单标题文档更像"大标题" |

**推荐 A**(与 vue-tui 对齐,E2E H 用例的屏幕断言需同步核对)。

## D3 链接渲染(对应 G3)

**背景**:minimad 无 `[label](url)` 语法,链接原样输出;vue-tui 渲染为蓝色+下划线 label 并输出
OSC 8 可点击超链接。**阻碍:ratatui 0.30.2 无 hyperlink 支持**(已查源码),OSC 8 只能手工嵌序列。

| 选项 | 说明 | 代价 |
|---|---|---|
| A 完整对齐(label 样式 + OSC 8) | 在 [markdown.rs](rs/src/markdown.rs) prose 管道解析行内链接,label 上色,URL 写入 OSC 8 序列 | M+:ratatui 不识别 hyperlink,需把 `\x1b]8;;url\x07label\x1b]8;;\x07` 当普通 span 文本输出,ansi-to-tui 能否无损透传 OSC 序列**待验证**;不透传则要改渲染层 |
| B1 仅样式化 | label 蓝+下划线,URL 保留灰色展示 | M:自定义行内解析器(与 minimad 复合样式叠加要小心);观感先对齐 |
| B2 隐藏 URL | 只显示 label | 同 B1,但丢信息,markdown 阅读器不建议 |
| C 不做 | 保持原样文本 | 0 |

**推荐 B1**:先补观感,OSC 8 等 ratatui 官方支持后升级为 A。非 TTY 管道输出不受影响(直出原文)。

## D4 代码语言覆盖(对应 G4)

**背景**:Node 版 shiki 40 语言;dlook 的 syntect 默认集缺 TS(回退 js)、Vue/Svelte/TOML/INI/
GraphQL/Dockerfile/PowerShell/SCSS/Less/Swift/Kotlin/Dart(无色)。涉及 [lang.rs](rs/src/lang.rs)
与 [markdown.rs](rs/src/markdown.rs) `normalize_fence_lang` 两处映射。

| 选项 | 说明 | 代价 |
|---|---|---|
| A two-face crate | 预打包完整 Sublime 语法集(+主题),API 兼容 syntect | M;二进制增大(幅度**待实测**,预计 1–3 MB 级);两处映射表可大幅简化 |
| B extra_syntaxes | 自己挑 `.sublime-syntax` 文件打包,按需增补 | M+;体积可控但要维护语法文件来源与 license |
| C 维持现状 | 缺的语言无色、TS 用 JS 语法 | 0;TS 高亮有偏差(无 type 关键字色) |

**推荐 A**:6 MB → 预计 7–9 MB 仍远小于 Node 版 110 MB;先加一个体积对比 checkpoint 再合入。

## D5 高亮主题选择

**背景**:Node 版固定 github-dark,dlook 固定 base16-ocean.dark,均为单一主题。

| 选项 | 说明 |
|---|---|
| A 暂不做 | 保持单主题;与 Node 版平手 |
| B `--theme` flag | syntect ThemeSet 自带十余主题,加参数成本低(S),但需考虑浅色终端默认值 |

**推荐 A**(先补差距项,主题选择是新功能非补齐)。

## D6 任务列表 checkbox(对应 G5)

**背景**:vue-tui 渲染 `[x]`/`[ ]` 为 checkbox;minimad 原样输出。

| 选项 | 说明 |
|---|---|
| A 做 | prose 预处理 `- [x] `/`- [ ] ` → `☑ `/`☐ `(顺序:在 termimad 排版前替换源文本) |
| B 不做 | 原样文本也可读 |

**推荐 A**,S 工作量,观感收益明显;注意只替换行首列表位置,避免误伤正文中的 `[x]`。

## D7 表格圆角边框(对应 G6)

**背景**:vue-tui 圆角 `╭┬╮`,termimad 默认方角;termimad 自带 `ROUNDED_TABLE_BORDER_CHARS`。

| 选项 | 说明 |
|---|---|
| A 切圆角 | `skin.table_border_chars = ROUNDED_TABLE_BORDER_CHARS`,一行 |
| B 保留方角 | 与 vue-tui 有样式差异,非功能差距 |

**推荐 A**(若追求两版观感一致;E2E 表格用例断言的是单元格文本,不受边框字符影响——合入前跑一遍确认)。

## D8 数学公式(对应 G7)

**背景**:vue-tui 库能力为 optional katex → 行内 Unicode 近似(块级公式不支持);**Node 版未装
katex,实际不可用**。Rust 侧无等价轻量方案。

**推荐 A:不做**。两版产品层现状一致;Unicode 近似公式观感一般,投入产出比低。

## D9 图片(对应 G8)

**背景**:vue-tui 库能力为 kitty/iTerm2 图形协议(需 resolver);Node 版未用。Rust 侧可用
`ratatui-image`,但需检测终端协议支持,且 alt-screen + 滚动视口下图片滚动/重绘复杂度高。

**原决策(2026-09-03)**:A:不做(远期)。
**2026-09-13 翻案,见 [D15](#d15-图片渲染2026-09-13-追加)**:用户需求驱动,ratatui-image 11
已提供滚动部分可见(sliced 模块)与协议探测/回退,复杂度风险已被库消化。

## D10 markdown 主题配置化(对应 G9)

**背景**:vue-tui 有 `theme` 覆盖 prop(hex 真彩);Node 版未用。dlook 若做需设计 CLI 参数或配置文件。

**推荐 A:不做**。D1 定稿默认配色即可;配置化属于新功能。

## D11 文本拖选与复制(对应 G10)

**背景**:两版产品层都没有可用的选择复制——Node 版 vue-tui 库有完整能力
(拖选反显、视口边缘自动滚动、松开即复制、OSC 52、Escape 清除),但 app 未传 `selection` 未启用
(实测拖选零输出);Rust 版无实现。且两版都开了鼠标捕获,**终端原生拖选被吞**,只能 Shift+拖动绕过。
对标行为见 [GAP.md](GAP.md) G10。

| 选项 | 说明 | 代价 |
|---|---|---|
| A 应用内拖选 + OSC 52(对齐 vue-tui 默认行为) | Cargo 开 crossterm `osc52` feature;事件循环加 `MouseEventKind::Down/Drag/Up(Left)` 状态机;渲染时选中 span 加 `REVERSED`;松开鼠标执行 `CopyToClipboard` | M。建议**分两步**:① 拖选高亮 + `y`/Enter 手动复制;② 升级为松开即复制(autoCopy)+ 拖到视口边缘自动滚动 |
| B 零成本止血 | README 写明「Shift+拖动 = 终端原生选择复制」 | 0;不算功能,只是行为说明 |
| C 不做 | 维持现状 | 0 |

**推荐 A(分两步)+ 无论选哪个都顺手做 B(README 一句话)**。

注意事项:
- OSC 52 依赖终端支持(kitty/Ghostty/iTerm2/wezterm/Alacritty 支持;tmux 需 `set-clipboard on`);
  复制失败应静默降级(状态栏提示「clipboard unsupported」即可,不报错)。
- 选区取文本需基于 `Doc.lines` 的屏幕行(截断/换行后的),与 vue-tui 的
  `SelectionTextProvider` 做法等价;md 模式注意已换行的段落拼回时按视觉行复制即可。
- E2E:现有用例不受影响(滚轮映射不变);新增用例需 pty 发 SGR 鼠标序列
  (press/drag/release)断言反显 SGR 7 与 `\x1b]52;c;` 输出。

---

## D12 产物体积(2026-09-03 追加)

**背景**:v0.2.0 产物 6.68MB,目标 1–2MB。

**体积构成(对照实验逐步砍依赖实测)**:

| 组件 | 体积 | 占比 | 可否内裁 |
|---|---|---|---|
| mermaid 链(mermansi → merman-core + lalrpop) | 3.47MB | 52% | ❌ mermansi/merman-core 均无图类型 feature,整块 |
| syntect 机器(fancy-regex/解析/高亮运行时) | 1.43MB | 21% | ❌ token 级高亮的固定成本 |
| 语法数据(two-face 全量 959KB + 主题 62KB) | 1.02MB | 15% | ✅ 可换 ~20 语言最小集(~70KB) |
| 骨架(ratatui/crossterm/termimad/notify/std) | 0.77MB | 12% | ✅ 部分(notify 已裁) |

**UPX 评估(实测)**:`upx --lzma --best` 6.68MB → 2.57MB(38.5%);压缩后仍为合法 ELF 直接运行,
pyte E2E 96 + tmux E2E 33 全过;代价:启动 1.2ms → 104ms、杀软误报风险、macOS 压缩后签名失效需重签。→ **用户否决**。

**决策**:保持全功能,放弃 1–2MB 目标(保留 mermaid 则体积下限 ≈5.5MB);采纳无风险优化:
notify → stat 轮询热重载(行为等价,事件循环本就以 200ms 轮询,去掉整个 watcher 线程/channel/依赖)。

**结果**:6,680,568 → **6,589,160 字节(−91KB)**;验证:单元 15 + pyte 96 + tmux 33 全过,热重载冒烟(编辑后 ≤500ms 重排)通过。

---

## D13 项目名/仓库名统一改为 dlook(2026-09-03 追加)

**背景**:v0.2.0 起产物二进制已名为 dlook,但项目/仓库名仍是 `look`(github.com/eric8810/look),
视觉评审也指出「dlook vs look」易造成认知不一致。决策:**统一为 dlook**。

**实施**:
- GitHub repo 重命名 `eric8810/look` → `eric8810/dlook`(旧 URL 自动 301 重定向,旧安装命令仍有效)
- install.sh `REPO=dlook`;README 徽章/安装 URL;rs/Cargo.toml `repository`
- package.json(遗留 Node 版)name/bin、设计文档标题 + 改名说明
- 宣传图内嵌安装命令同步重生成(gen-promo.py / gen-mascot-banner.py)

---

## D14 本地文件链接点击跳转(2026-09-12 追加)

**背景**:D3 已把 `[label](url)` 渲染为「亮蓝下划线 label + 暗灰 (url)」,但只是样式,
无法交互。用户需求:markdown 里的**本地文件链接**(如 `[guide](./docs/guide.md)`)可点击快速打开。

| 选项 | 说明 | 代价 |
|---|---|---|
| A 应用内导航(推荐) | 点击即换 Doc,像浏览器;⌫/Alt+← 返回栈(含滚动位置) | 命中测试 + Nav 状态 ~200 行 |
| B 系统打开器 | xdg-open 跳出终端 | 丢上下文,不适合预览器 |
| C OSC 8 超链接 | 终端原生点击 | 多数终端不支持,ratatui 0.30 无 hyperlink(D3 已验证) |

**决策:A**;外部链接(http 等,用户选择)交系统打开器(xdg-open/open/start,spawn 不等待,
事件循环非阻塞收割避免僵尸)。要点:

- **LinkSpan 内容坐标**(links.rs):`(line, start, end, target)`,与 selection.rs 同一套坐标模型,
  滚动期间稳定;resize/热重载/导航时随 lines 重建。可点击区域 = label + `(url)` 后缀整段。
- **管线顺序调整**:markdown.rs 先 `frame_tables` 后 `style_links`——表格圆角边框会插入行,
  链接行号必须基于最终行集(单元测试显式覆盖该场景)。
- **本地链接 ↗ 标记**:渲染期按语法粗分(无 scheme/非锚点/file://),点击期才真正校验
  (不存在/目录/二进制 → 状态栏提示,不跳转);`%XX` 解码 + `<path with spaces>` 两种形式都支持。
- **导航语义**:相对路径按当前文件目录解析;跳转后按新扩展名重判 mode+语法高亮;
  热重载 stat 轮询跟随当前文件;返回时恢复跳转前滚动位置。
- **顺带修复**:纯点击视口首/末行曾被 edge_autoscroll 误判为「拖到边缘」而平移内容
  (Down/Up 内容坐标错位 → 变成跨行选区复制)。现要求指针移动过(真拖拽)才自动滚动。

**结果**:单元 25(+10)/pyte 115(+19,新增 M 场景:标记渲染、点击跳转、返回+滚动恢复、
缺失目标提示、假 xdg-open 外部链接、代码块伪链接不可点、表格内链接)/tmux 33 全过。

---

## D15 图片渲染(2026-09-13 追加,翻案 D9)

**背景**:用户需求——dlook 支持渲染本地/远程图片。D9 曾以「alt-screen 滚动视口下图片滚动/重绘复杂度高」为由不做;重评发现 ratatui-image 11 已把两块硬骨头(协议探测+回退、滚动部分可见)做成了库能力,风险被消化。

**方案**(用户选定:终端图形协议 + 支持直开图片文件):

- **协议栈**:ratatui-image 11(与本项目 ratatui 0.30 / crossterm 0.29 完全匹配,default-features 去掉 chafa)。启动时 `Picker::from_query_stdio()` 探测 → kitty/sixel/iTerm2 → 无响应回退 halfblocks(半块字符 truecolor,任何终端可用);tmux 自动检测 + `allow-passthrough on`。环境变量 `DLOOK_IMAGE_PROTOCOL=auto|halfblocks|off`。
- **滚动集成**:核心洞见——图片在 `doc.lines` 里占位空行(行数=图片行数),滚动/选区/链接/`max_top` 数学**全部照旧**;`Doc.images` 记录 `(line, SlicedProtocol)` 放置,Viewport 渲染完文字行后把 `SlicedImage`(支持负 Y 位置=顶部滚出视口)画进 Buffer。ratatui-image sliced 模块按协议处理部分可见:Kitty 用 unicode placeholder 行偏移,Sixel 按 band 裁剪,iTerm2 逐行切片,Halfblocks 行跳过。
- **加载管线**(images.rs):`ImageCtx` 注册表跨 rebuild 存活,src → {Loading/Ready/Failed};本地(相对 md 目录解析、percent-decode、64MB 上限)/http+https(ureq+rustls,10s 超时、16MB 上限、5 跳重定向)/`data:` URL(base64/percent)→ image crate 解码(>2560px 降采样)→ 协议编码,全部在后台线程;完成 bump dirty 计数,事件循环(200ms 轮询)发现变化即重排(同热重载路径)。resize 触发后台重编码,期间旧协议继续渲染(防闪烁);失败缓存不重试(防 rebuild 循环反复请求坏链接)。
- **markdown 集成**:独立段落 `![alt](src)` 在 `split_at_fences` 阶段提取为 `Segment::Image`(不经 termimad);行内图片预处理降级为 `[alt](src)` 普通链接(样式化+可点击,与 vue-tui 的 alt-text 降级一致)。图片禁用(off)/失败 → `🖼 alt (src)` 链接行或 `✗` 错误行。
- **直开图片文件**:扩展名(png/jpg/jpeg/gif/webp/bmp/ico/tiff)→ `Mode::Image`,跳过二进制拒绝;`dlook photo.png` 直接渲染;点击指向图片的本地链接在应用内打开(图片模式),⌫ 返回;非 TTY 直开图片 → 明确报错退出 1。热重载:图片模式的注册表 key 携带 stat 指纹(`file:<path>?v<mtime>.<size>`),文件变化自动重新加载。
- **选区**:图片行不做反显(placeholder 的字符/颜色编码了图片 ID,REVERSED 会破坏 kitty 解码);拖选跨图片行复制为空行。

**协议探测时机**:from_query_stdio 需直接读写 stdin,必须在事件循环前调用;且无响应终端要 2s 超时——因此**按需探测**:图片模式或 md 含 `![` 才查询,纯文本文档零启动开销;启动后热重载新引入的图片惰性降级 halfblocks(事件线程已存活,不能再探测 stdin)。

**体积 checkpoint**:**6,589,160 → 9,207,584 字节(+2.62MB,+39.8%)**,构成≈image 解码器 +1.2MB、ratatui-image+icy_sixel +0.9MB、ureq+rustls +0.5MB。D12 的「保持全功能优先于体积」原则延续;若未来要回收,可裁 image 格式 feature 或远程支持。

**验证**:单元 40(+15:图片语法解析/独立段判定/行内降级/data: URL/本地加载/协议尺寸/注册表缓存与失败缓存)/pyte E2E 138(+23,场景 N:本地图片 halfblock+truecolor、异步占位替换、行内降级链接、缺失错误行、点击跳转图片模式+⌫ 返回、直开、远程 http 图片、off 降级 🖼 链接、非 TTY rc=1)/tmux 40(+7,T30–T36:真终端 halfblocks truecolor、周边文本、直开、滚动)全过。

**遗留**(记录不做):GIF 动画只取首帧;图片引用语法 `![alt][ref]` 不支持;远端图片失败不自动重试(重开文件即重试);kitty/sixel 协议路径无法在 pyte/tmux 中自动验证(两者都不支持图形协议),依赖 ratatui-image 的跨终端截图测试矩阵。





2026-09-03,全部 11 项按推荐方案落地(实施与验证见下表;E2E 套件扩至 A–L,96 项全过;单元测试 15 项全过;
另有 tmux 真实终端套件 [run-tmux.sh](test/e2e/run-tmux.sh) T1–T29 共 33 项全过 —— 含 **OSC 52 剪贴板内容**验证,
即 `set-clipboard on` 下 tmux 捕获的粘贴 buffer 与选区文本逐字一致)。

| # | 结论 | 日期 | 备注 |
|---|---|---|---|
| D1 | A:h1/h2 青(`Color::Cyan`→SGR 38;5;14)、h3/h4 蓝(`Color::Blue`→38;5;12)、h5/h6 纯粗体 | 2026-09-03 | `termio.rs build_skin`;E2E J2/J3 |
| D2 | A:H1 左对齐(`headers[0].align = Left`) | 2026-09-03 | E2E J4 |
| D3 | B1:`[label](url)` → label 亮蓝+下划线(SGR 4/38;5;12)+ ` (url)` 暗灰(38;5;8) | 2026-09-03 | markdown.rs `style_links`,仅作用于无样式 span;OSC 8 待 ratatui 支持(0.30.2 无 hyperlink,已验证);E2E J8–J10 |
| D4 | A:two-face 0.5.2(`syntect-default-fancy`,无 onig C 依赖);lang.rs/markdown.rs 映射扩展 + 回退链(tsx→js、vue/svelte→html、kotlin→java) | 2026-09-03 | **体积 checkpoint:6,067,296 → 6,680,552 字节(+613 KB,+10.1%)**,远低于预估 1–3 MB;TOML/Vue/TS 原生语法,E2E L1–L6 |
| D5 | A:不做,保持 base16-ocean.dark 单主题 | 2026-09-03 | — |
| D6 | A:`- [x]`/`[ ]`(含有序/嵌套)→ ☑/☐,仅 prose 段 | 2026-09-03 | markdown.rs `render_task_checkboxes`;E2E J5/J6 |
| D7 | A(方案调整):termimad FmtText 路径把 TableRule 固定为 Other 位置、**不画外框**(与 D7 原前提不符),preset 切换无效 → 改为渲染后处理 `frame_tables`:按分隔线几何插入 `╭─┬─╮`/`╰─┴─╯` | 2026-09-03 | E2E J7;`skin.table_border_chars` 同时切 ROUNDED(为未来路径留位) |
| D8 | A:不做 | 2026-09-03 | — |
| D9 | A:不做(远期 ratatui-image) → **2026-09-13 翻案为 D15** | 2026-09-03 | ratatui-image 11 + sliced 模块消化了滚动/探测复杂度 |
| D10 | A:不做 | 2026-09-03 | — |
| D11 | A(①②均落地)+ B:拖选反显(SGR 7)、Shift+点击扩展、视口边缘自动滚动(120ms 节流)、**松开即 OSC 52 复制**(`crossterm osc52` feature)、`y`/Enter 手动复制、Esc 清除(无选区才退出)、状态栏 `copied N chars`;README 记录 Shift+拖动原生选择 | 2026-09-03 | 新增 `selection.rs`(内容坐标模型,滚动稳定)+ `viewport.rs` 反显;resize/热重载清除选区;复制失败静默降级;E2E K1–K10 |
| D12 | 保持全功能 6.59MB(−91KB);UPX 实测可行(2.57MB)但用户否决;mermaid 链 3.47MB 不可内裁,1–2MB 目标放弃;采纳 notify→stat 轮询热重载(行为等价,去 watcher 线程与依赖) | 2026-09-03 | 体积构成见 D12 小节;验证 15+96+33+热重载冒烟全过 |
| D14 | 本地文件链接应用内点击跳转:内容坐标 `LinkSpan`(links.rs)+ Nav 历史栈(⌫/Alt+← 返回,恢复滚动);本地链接 ↗ 标记(语法粗分),点击期校验(缺失/目录/二进制 → 状态栏);外部链接 xdg-open/open/start(spawn 不等待,非阻塞收割);`%XX` + `<空格路径>` 支持;顺带修复纯点击视口首/末行被 edge_autoscroll 平移的缺陷 | 2026-09-12 | 详见 D14 小节;markdown.rs 管线改为先 frame_tables 后 style_links(链接行号基于最终行集);验证 25+115+33 全过;E2E M1–M8 |
| D15 | 图片渲染(翻案 D9):ratatui-image 11 协议探测(kitty/sixel/iTerm2 → halfblocks 回退)+ `Doc.images` 占位行集成(sliced 滚动部分可见)+ `ImageCtx` 后台加载注册表(本地/http(s)/data:,dirty 计数重排)+ md 独立段落图片/行内降级链接 + 直开图片文件(Mode::Image)+ `DLOOK_IMAGE_PROTOCOL` 环境变量 | 2026-09-13 | 详见 D15 小节;体积 6.59→9.21MB(+2.62MB);验证 40+138+40 全过;E2E N1–N8、tmux T30–T36 |
