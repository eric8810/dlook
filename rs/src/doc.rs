//! Doc 模型 + 滚动数学（1:1 复刻 E2E B 用例的 clamp 行为）。

use std::sync::Arc;

use ratatui::text::Line;
use ratatui_image::sliced::SlicedProtocol;

use crate::lang::Mode;
use crate::links::LinkSpan;

/// 文档中的一张图片:占位 `line .. line+h` 行,由 Viewport 用图形协议绘制。
/// 图片行在 `lines` 中为空行——滚动/选区/链接坐标全部照常工作。
#[derive(Clone)]
pub struct DocImage {
    /// 图片首行在 doc.lines 中的索引。
    pub line: usize,
    /// 协议编码(ratatui-image sliced;支持滚动部分可见,尺寸见 size())。
    pub proto: Arc<SlicedProtocol>,
    /// 图片高度(行数)。
    pub h: u16,
}

pub struct Doc {
    /// 整篇文档渲染后的带样式行（已按当前宽度换行/截断）。
    pub lines: Vec<Line<'static>>,
    /// 行内链接的可点击区域（内容坐标，随 lines 一起重建）。
    pub links: Vec<LinkSpan>,
    /// 图片放置记录(内容坐标,随 lines 一起重建;DECISIONS D15)。
    pub images: Vec<DocImage>,
    /// 滚动位置（视口首行在 lines 中的索引）。
    pub top: usize,
    /// 预览模式（保留用于模式感知的重排决策）。
    #[allow(dead_code)]
    pub mode: Mode,
    /// 当前排版宽度（= 终端列数）。
    pub width: u16,
}

impl Doc {
    pub fn new(
        lines: Vec<Line<'static>>,
        mode: Mode,
        width: u16,
        links: Vec<LinkSpan>,
        images: Vec<DocImage>,
    ) -> Self {
        Self {
            lines,
            links,
            images,
            top: 0,
            mode,
            width,
        }
    }

    /// 最大滚动位置 = lines.len() - body_h（饱和减）。
    pub fn max_top(&self, body_h: usize) -> usize {
        self.lines.len().saturating_sub(body_h)
    }

    /// 设置滚动位置，clamp 到 [0, max_top]。
    pub fn set_top(&mut self, t: usize, body_h: usize) {
        self.top = t.min(self.max_top(body_h));
    }

    /// 相对滚动 delta 行（正向下，负向上）。
    pub fn scroll(&mut self, delta: isize, body_h: usize) {
        let new_top = (self.top as isize + delta).max(0) as usize;
        self.set_top(new_top, body_h);
    }

    /// 按新宽度重排（md 换行 / code 截断 / mermaid 重渲）。
    /// 由 main.rs 在 resize 时调用，传入重新生成的 lines。
    pub fn replace_lines(
        &mut self,
        lines: Vec<Line<'static>>,
        links: Vec<LinkSpan>,
        images: Vec<DocImage>,
        width: u16,
        body_h: usize,
    ) {
        self.lines = lines;
        self.links = links;
        self.images = images;
        self.width = width;
        self.set_top(self.top, body_h);
    }
}
