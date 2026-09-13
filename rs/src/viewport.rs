//! 自定义 ratatui widget：虚拟视口，渲染 lines[top .. top+h]。
//!
//! 有选区时(DECISIONS D11),选区覆盖的行按列范围加 REVERSED 反显。
//! 图片(DECISIONS D15):图片行跳过文字渲染(空行占位),随后按放置记录
//! 把 `SlicedImage` 直接绘制进 Buffer——滚动部分可见、协议适配均由
//! ratatui-image sliced 模块处理;图片行不做选区反显(占位符的
//! 颜色/字符编码了图片 ID,反显会破坏 placeholder 解码)。

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::Widget;
use ratatui_image::sliced::{SignedPosition, SlicedImage};

use crate::doc::DocImage;
use crate::selection;

pub struct Viewport<'a> {
    pub lines: &'a [Line<'a>],
    pub top: usize,
    /// 文本选区(内容坐标);None = 无选区。
    pub selection: Option<selection::Selection>,
    /// 图片放置记录(内容坐标)。
    pub images: &'a [DocImage],
}

/// 行索引是否落在任一图片占位区。
fn is_image_row(images: &[DocImage], li: usize) -> bool {
    images
        .iter()
        .any(|im| li >= im.line && li < im.line + im.h as usize)
}

impl Widget for Viewport<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Phase 1:文字行(图片行跳过 → cells 保持空白,diff 自动清理旧占位符)
        for y in 0..area.height {
            let li = self.top + y as usize;
            if is_image_row(self.images, li) {
                continue;
            }
            let row = area.y + y;
            match self.lines.get(li) {
                Some(line) => {
                    // 选区覆盖此行 → 反显高亮后渲染
                    let highlighted = self
                        .selection
                        .and_then(|sel| selection::line_range(&sel, li))
                        .map(|(x0, x1)| selection::highlight_line(line, x0, x1));
                    match highlighted {
                        Some(hl) => {
                            buf.set_line(area.x, row, &hl, area.width);
                        }
                        None => {
                            buf.set_line(area.x, row, line, area.width);
                        }
                    }
                }
                None => {
                    // 超出文档末尾：填空格
                    let spaces = " ".repeat(area.width as usize);
                    buf.set_string(area.x, row, &spaces, Style::default());
                }
            }
        }

        // Phase 2:图片(在文字之后绘制,覆盖占位行 cells)
        for im in self.images {
            let dy = im.line as i64 - self.top as i64;
            // i16 表达范围外的必然不可见,跳过
            if !(i16::MIN as i64..=i16::MAX as i64).contains(&dy) {
                continue;
            }
            let pos = SignedPosition { x: 0, y: dy as i16 };
            SlicedImage::new(&im.proto, pos).render(area, buf);
        }
    }
}