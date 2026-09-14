---
id: media-2
package: rs
module: web.rs
status: in-progress
depends-on: []
---

# media-2：网页渲染（HTML → dlook 行模型）

## objective

实现 `rs/src/web.rs`：`render(source, width, skin)` 抓取/读取网页并渲染为 dlook 的
`Vec<Line>` + `LinkSpan` 模型（L1 文本层），`WebDoc` 附页面标题与最终 URL。
不拉取任何子资源；失败给可读错误。

## context

- 设计：[docs/design/media.md](../../design/media.md) §2/§4
- 研究依据：[evidence-web.md](../../research/media/evidence-web.md)（html2text 注解质量实测、
  安全先例 aerc、相对链接需按 base 解析）
- 冻结接口：`rs/src/web.rs` 脚手架（**不得改签名**）
- 依赖已声明：`html2text = "0.17"`；`ureq`（既有，图片同款限额先例）

## path

- `rs/src/web.rs`（独占；不改其他文件）

## 实现要点

1. **来源**：`http(s)` URL → ureq 抓取（超时 10s、响应上限 16MB、重定向 ≤5、UA 设
   `dlook/<version>`）；本地 `.html/.htm` → 读文件（上限 16MB），base = `file://<父目录>/`。
2. **字符集**：先按 `Content-Type; charset=` 解码；无声明时嗅探 `<meta charset>`；
   **GBK/GB18030 等非 UTF-8 需转码**（可用 `encoding_rs`，若引入请加进 Cargo.toml 并在
   报告中说明体积影响）；无法识别时按 UTF-8 lossy 且不 panic。
3. **渲染**：`html2text::from_read_rich()` 的类型化注解（`Link(url)`/`Strong`/`Emphasis`/
   `Code`/`Preformat`/`Image(src)`）→ `Vec<Line>` + `LinkSpan`：
   - 链接样式与既有 markdown 一致（label 亮蓝下划线 + ` (url)` 暗灰；复用
     `markdown.rs` 的样式常量或等价实现——**不改 markdown.rs**，如需要公共常量可复制
     并为后续统一留 TODO 注释）。
   - 链接区域坐标 = 渲染后**最终行集**上的字符列（与 markdown 的 LinkSpan 语义一致）。
   - 相对 URL 按 base（最终 URL，含重定向后）解析为绝对；`<img>` 只输出 alt/占位文本，
     **绝不请求**（可显示 `🖼 alt` 样式文本，不建 LinkSpan 到图片——防追踪像素）。
   - 代码块/预格式用单一 dim 样式或等宽前缀，不做语法高亮（MVP）。
4. **文本换行**：按 `width` 折行（html2text 自带宽度参数或自行 wrap；保证行长 ≤ width）。
5. **标题**：`<title>` 缺失回退 URL/文件名。
6. **错误文案**：`fetch failed: <原因>` / `http 404` / `not a web page` 等可读中文/英文短句
   （与项目现有状态栏文案风格一致，英文短语为主）。

## verification

单元测试（`cargo test web::`）：
1. 本地 HTML 文件（测试内 `tempfile` 写入）→ `render` 出行集：包含标题文本、
   两个段落、一个链接；`links[0].target` 为**绝对** URL（相对路径已按 base 解析）。
2. 表格与代码块不炸：含 `<table>`/`<pre>` 的 HTML 渲染成功且行数 >0。
3. 安全断言：HTML 含 `<img src="http://tracker.example/pixel.gif">` →
   `render` 结果的 links 中**不含**该 URL，且渲染期间无任何子资源请求（以函数只接收
   HTML 字符串、不做额外网络调用的实现事实 + 单测断言 links 不含为准）。
4. 非 UTF-8：GBK 编码的 HTML 字节（手工构造）→ 渲染出正确中文（断言含预期汉字）。
5. 超限/异常：>16MB 来源（构造声明 Content-Length 或本地大文件）→ 返回可读错误；
   空文件 → 错误；`http://127.0.0.1:1/`（必然拒绝连接）→ 错误含 "fetch failed"。
6. 宽窄宽度：width=40 与 120 渲染同一 HTML，所有行字符数 ≤ width。

集成自测（不进单测）：起本地 `python3 -m http.server` 提供样例页，用
`cargo run -- http://127.0.0.1:PORT/page.html` 需等 media-4 落地后才可端到端；
本任务以单元测试为准，并在报告中记录该待接项。

## 交付要求

- 消融实验：尝试删去中间类型化结构（如直接把注解映射到 Span）或去掉字符集嗅探，
  说明删减后破坏了什么（预期：注解直通可行则简化；嗅探删除会导致 GBK 乱码）。
- 返回：代码（本文件内）、测试结果、消融结论、未完成事项。
- 若需新增依赖（encoding_rs 等）在返回中说明并等待主 agent 确认后再改 Cargo.toml。
