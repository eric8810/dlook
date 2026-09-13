# 本机实验记录（2026-09-13）

环境：Linux x64，mpv v0.41.0，ffmpeg 7.x，chromium（headless 可用），PipeAudio/PipeWire server（`/run/user/1000/pulse/native`）+ ALSA 设备。测试素材由本目录命令生成。

## E1: mpv 终端图形协议输出（决定性）

```bash
# vo 列表官方包含:
#   tct   true-color terminals
#   sixel terminal graphics using sixels
#   kitty Kitty terminal graphics protocol   ← 官方支持,非 fork/PR
mpv --vo=help
```

- `mpv --vo=sixel --ao=null test-video.mp4`（pty 内）→ 产生真 sixel DCS 序列
  `ESC P q "1;1;320;180 ...`，并自行进入 alt-screen（`?1049h`）+ 开鼠标跟踪（`?1003h`）。
- `mpv --vo=kitty`（TERM=xterm-kitty）→ 30 帧（1s@30fps）耗时 **1.48s**、输出 **4.85MB**、1199 个 `_G` 命令（~160KB/帧，自动缩放到终端适配尺寸）。
- 结论：委托 mpv 即可获得 kitty/sixel 协议视频输出，**无需自研帧管线**即可在支持的终端里看视频；帧率上限取决于终端解析速度（本机 pty 无人渲染，实测的是生成速度下限）。

## E2: mpv JSON IPC 控制（决定性）

`--input-ipc-server=<unix socket>` + JSON 行协议：

| 命令 | 响应 |
|---|---|
| get_property pause | `{"data":false,"error":"success"}` |
| set_property pause true | success |
| seek 2 absolute | success + seek/playback-restart 事件 |
| get_property time-pos | `{"data":2.000000}` |
| get_property duration | `{"data":3.000000}` |
| set_property volume 80 | success |
| quit | rc 0 |

- 事件流含 `start-file`/`seek`/`end-file` 等，可驱动 dlook 状态栏 UI。
- 结论：「dlook 掌 UI + mpv 做解码/输出 + IPC 控制」的架构完全可行；音频（`--vo=null --audio-display=no`）路径下 mpv 纯做解码输出，dlook 不让出屏幕。

## E3: chromium headless 整页截图（决定性）

```bash
chromium --headless=new --disable-gpu --no-sandbox --hide-scrollbars \
  --window-size=1280,1800 --screenshot=shot.png file://test-page.html
# 0.45s, 26KB
chromium ... --window-size=1280,12000 --screenshot=shot-tall.png long-page.html
# 0.69s, 226KB —— 整页 12000px 高的「网页长截图」可行
```

- 结论：「网页 → 整页截图 → dlook 已有图片渲染管线（SlicedProtocol 滚动）」是快速可落地的网页预览原语，静态无 JS 交互，但真实 CSS/布局/JS 渲染结果。
- `--virtual-time-budget` 可等 JS 渲染。

## E4: ffmpeg 解码吞吐（排除项）

- 640×360@30fps 60s（1800 帧）→ rawvideo 解码 0.23s（≈**8000fps**）；1280×720 管道 2.0GB/s。
- 结论：**解码永远不是瓶颈**；终端视频的瓶颈 100% 在协议传输与终端解析速度。自研「ffmpeg 子进程 pipe rawvideo → 协议渲染」路线性能取决于终端侧，与 mpv 路线同量级。

## E5: 本机媒体环境（实验可用性）

mpv/ffmpeg/ffplay/chromium 在 PATH；PipeWire+ALSA 正常；browsh/w3m 缺失。
后续 rodio/cpal 音频实验可直接在本机验证。

## E6: mpv --no-terminal + vo=kitty 输出验证（复核新增,2026-09-13）

裁决原「关键未知1/2」:研究 §5.3-4 的对策前提「--no-terminal 下 VO 仍写 stdout」+「退出/启动清图行为」。

脚本 `verify_mpv_noterminal.py`（pty + select 读原始字节流）:

| 变体 | 字节 | `_G` 命令 | `a=d` 次数 | 帧载荷 |
|---|---|---|---|---|
| B: 仅 `--really-quiet` | 2,540,489 | 629 | 2 | `f=24,m=1` RGB24 分块 |
| A: + `--no-terminal`（研究推荐完整参数） | 2,540,479 | 629 | 2 | 同 B,逐字节几乎一致 |

- 首序列即 `\x1b_Ga=d;\x1b\\`（**启动 reconfig 时发**,非仅退出）→ 后接 `\x1b[3;0f` 光标定位 + 帧序列;退出再发一次。
- IPC 在 `--no-terminal` 下正常:`set_property pause` / `seek 1 absolute` 均 `{"error":"success"}`。
- **结论 1**：MVP 形态前提成立——`--no-terminal` 不影响 vo=kitty 输出,dlook 可保留键盘焦点经 IPC 控制。
- **结论 2（新事实）**：即使 `--vo-kitty-config-clear=no`,mpv **启动时**也会清终端全部 kitty 图像（头+尾各一次）。原对策「退出后全量重绘」不完备：M2/P2 场景下文档内嵌图片在 mpv 启动瞬间即消失,播放期间空缺,退出后重绘才恢复。

## E7: 帧率证据强度对照（复核计算,无新实验）

E1 数据 30 帧/1.48s ≈ **20fps 生成吞吐**（含进程启动;扣 ~0.3s 启动 ≈ 25fps,视频 640×360、pty 无人渲染）。即本机 E1 自身未达 24fps,「≤720p@24fps 可达」实际依赖 mpv 开发者机器的外部单点实测（fa9c2a3 修后 1080p24）+ 推断。该表述应视为条件性结论,承诺前需在真实终端矩阵实测。

## 复现

素材生成与实验脚本见本目录 `test-video.mp4`（ffmpeg testsrc2 640×360@30 3s h264+aac）、
`test-60s.mp4`（60s）；E1/E2 的 python 驱动逻辑内嵌于研究记录（pty + select 读原始字节）。

## E8: mpv vo=kitty 区域几何选项核实（复核后追加）

```
$ mpv --vo=kitty --list-options | grep vo-kitty
 --vo-kitty-alt-screen            Flag (default: yes)
 --vo-kitty-cols/rows/width/height/left/top   Integer (default: 0)
 --vo-kitty-config-clear         Flag (default: yes)
 --vo-kitty-use-shm              Flag (default: no)
```

`mpv --vo=kitty --vo-kitty-left=20 --vo-kitty-top=5 --vo-kitty-rows=10 --vo-kitty-cols=60 --frames=3 test-video.mp4`
（`--no-terminal --really-quiet --ao=null`）：选项被接受，正常输出 ~900KB 帧序列。

- 结论：**mpv 可把视频渲染限定在终端子区域**——「dlook 媒体栏 + mpv 画 body 区」的共屏
  形态（MVP-A）获得官方选项支撑。与 dlook UI 行的精确对齐、以及帧序列与 dlook 局部重绘
  在真实 kitty 终端的交错行为，留给 P2 前置原型（本环境无图形终端）。

## E9: 音频 E2E 探针（零外放证明样本到达设备）

脚本：`e9-audio-probe.sh`。原理：`pactl load-module module-null-sink` 建无声 sink →
`pw-record` 录制其 monitor → `paplay` 播 3s 440Hz 正弦 → 分析捕获。

结果：捕获 161,790 帧，**RMS=5263**（静音≈0），**440Hz Goertzel 能量 8.8e11** ——
真实音频样本确实到达输出设备，全程零外放。音频 E2E 断言方式成立。

## E10: mpv IPC 第二客户端旁观（控制链端到端探针）

客户端 A（模拟 dlook）`set_property pause=true` → 客户端 B（测试旁观者）：
- `get_property pause` 读到 true ✓
- `observe_property` 收到 `property-change` 事件（A 的每次操作）✓
- `/proc/<pid>/cmdline` 可读 spawn flags（断言 dlook 传参）✓

结论：测试进程可作为第二 IPC 客户端旁观 dlook 的控制效果——控制链验证零 mock。

## E11: 真实终端视觉验证通道（视频播放/暂停/恢复）

本机图形会话 = Hyprland + foot（**foot 支持 sixel**）。用于把「视频真的在播」
变成可断言的截图证据：
- 启动 `foot -a <tag> -- mpv --vo=sixel --loop --input-ipc-server=…`
- `hyprctl clients -j` 取窗口几何 → `grim -g "x,y WxH"` 截该窗口
- 对截图做 MD5 比对判定「画面是否变化」

```
[playing] 5 shots → 5 unique        ← 播放中画面在变
[ipc] set pause=true: success
[paused ] 5 shots → 1 unique        ← 暂停后画面完全冻结
[resumed] 5 shots → 4 unique        ← 恢复后又变
[seek   ] before≠after, changed=True ← seek 生效
VERDICT: PASS
```

脚本：`e11-visual-video-check.py`。grim 采集上限 ≈16.6 fps（12 张 0.72s），
足够做状态判定（不需要逐帧）。

**视觉判读**：截图经缩放后可直接读画面细节（时间码 `00:00:01.700` vs
`00:00:02.067`、色条位移）——除自动化哈希断外，还能做人工/模型视觉复核。

## E12: 音视频联合录制验证

同一次真实播放会话中同时录屏（grim 连拍）与录音（ffmpeg 采 monitor 源）：

```
[video] 8 shots → 8 unique frames
[audio] captured 147242 frames @ 48000Hz, RMS=2017, 440Hz power=4.405e+15
VERDICT: video=PASS, audio=PASS
```

脚本：`e12-av-record-check.py`。证明音频与视频可在同一会话内被完整录制并断言。

## 环境登记（视觉验证前置条件）

- 合成器 **Hyprland**；截图 `grim`；按键注入 `wtype`；窗口信息 `hyprctl clients -j`；图像处理 `magick`
- 终端 **foot 1.28.0（支持 sixel）** —— pyte/tmux 画不出图形协议，真实终端验证必须用 foot/kitty 等
- 音频 **PipeWire**（`pactl`/`pw-record`/`ffmpeg -f pulse` 均可用）
- 视觉判读：截图缩放后可由具备视觉能力的模型直接读取（也可配 `dim image read`）

## E13: 真实终端闭环（渲染 / 按键注入 / 退出码读回）

在 foot 里跑 `dlook <file>; echo EXIT=$?` → `grim` 截窗口 → `hyprctl dispatch`
聚焦 → `wtype -k q` 注入按键 → 截图读回退出码。脚本：`e13-terminal-loop-check.py`。

```
[1] rendered hash=311574f4a7
[2] focus: ok
[3] wtype q: ok
[4] after-q hash=b88c8f812d  changed=True     ← 按键生效
截图读回: EXIT=0                              ← 退出码经截图确认(视觉可读)
```

- 结论：**真实终端的完整闭环可自动化**——渲染结果、按键注入、退出码全部可断言。
- 附带发现：PATH 中若 `~/.local/bin/dlook` 是旧版(如 0.3.0)，会被优先命中而误判
  「不支持图片」。E2E 脚本**必须用绝对路径**指向被测二进制（已在脚本中固化）。
  截图读回错误信息 `EXIT=1` + `is a binary file, skip` 即由此暴露。
