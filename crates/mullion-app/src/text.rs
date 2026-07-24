//! 文字前景层:把网格快照映射成 glyphon 富文本(纯,可测),
//! 以及 glyphon 资源封装(GPU 胶水,见 Task 8)。

use glyphon::Color;
use mullion_term::snapshot::{Rgb, SnapCell};

/// term 的 Rgb → glyphon 颜色。
pub fn to_color(c: Rgb) -> Color {
    Color::rgb(c.r, c.g, c.b)
}

/// 把一行单元格切成 (文本, 颜色) 段:连续同前景色合一段,跳过宽字符 spacer。
/// 供 `glyphon::Buffer::set_rich_text` 使用(每段一个 `Attrs` 带 fg 色)。
pub fn row_to_spans(cells: &[SnapCell]) -> Vec<(String, Color)> {
    let mut spans: Vec<(String, Color)> = Vec::new();
    for cell in cells {
        if cell.spacer {
            continue; // 宽字符右半:字形已由左格承载
        }
        let color = to_color(cell.fg);
        match spans.last_mut() {
            Some((s, c)) if *c == color => s.push(cell.ch),
            _ => spans.push((cell.ch.to_string(), color)),
        }
    }
    spans
}

use glyphon::{
    Attrs, Buffer, Cache, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache, TextArea,
    TextAtlas, TextBounds, TextRenderer, Viewport,
};
use mullion_term::snapshot::GridSnapshot;

/// 显示字体族名。须在系统里已安装;未装则 cosmic-text 回退到默认字体(不崩,
/// 但等宽/对齐可能变差)。TODO:做成可配置(见 spec F21)。
const FONT_FAMILY: &str = "Google Sans Code";

/// glyphon 文字资源 + 每行一个 Buffer。GPU 胶水:无单测。
pub struct TextLayer {
    font_system: FontSystem,
    swash: SwashCache,
    atlas: TextAtlas,
    viewport: Viewport,
    renderer: TextRenderer,
    buffers: Vec<Buffer>, // 每屏面行一个
    pub cell_w: f32,
    pub cell_h: f32,
}

impl TextLayer {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        font_px: f32,
    ) -> Self {
        let mut font_system = FontSystem::new();
        let swash = SwashCache::new();
        let cache = Cache::new(device);
        let viewport = Viewport::new(device, &cache);
        let mut atlas = TextAtlas::new(device, queue, &cache, format);
        let renderer =
            TextRenderer::new(&mut atlas, device, wgpu::MultisampleState::default(), None);
        let line_h = (font_px * 1.25).ceil();
        // 用 'M' 的 advance 估等宽单元格宽度。
        let cell_w = measure_cell_w(&mut font_system, font_px, line_h);
        Self {
            font_system,
            swash,
            atlas,
            viewport,
            renderer,
            buffers: Vec::new(),
            cell_w,
            cell_h: line_h,
        }
    }

    /// 每帧:按快照重建各行 Buffer 文本,prepare 上传。
    ///
    /// 每帧全量重建/重新 shape 所有行;差分渲染是 F12,后续 spec,这里不做。
    /// 失败(如 `AtlasFull`)时不 panic,交调用方决定跳过本帧(渲染路径不许 panic)。
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        snap: &GridSnapshot,
        res: Resolution,
    ) -> Result<(), glyphon::PrepareError> {
        self.viewport.update(queue, res);
        let metrics = Metrics::new(self.cell_h * 0.8, self.cell_h);
        self.buffers.clear();
        for row in 0..snap.rows {
            let spans = row_to_spans(snap.row(row));
            let mut buf = Buffer::new(&mut self.font_system, metrics);
            buf.set_size(
                &mut self.font_system,
                Some(res.width as f32),
                Some(self.cell_h),
            );
            let attrs = Attrs::new().family(Family::Name(FONT_FAMILY));
            let iter = spans.iter().map(|(s, c)| (s.as_str(), attrs.color(*c)));
            buf.set_rich_text(&mut self.font_system, iter, attrs, Shaping::Advanced);
            buf.shape_until_scroll(&mut self.font_system, false);
            self.buffers.push(buf);
        }
        let cell_h = self.cell_h;
        let areas: Vec<TextArea> = self
            .buffers
            .iter()
            .enumerate()
            .map(|(row, buf)| TextArea {
                buffer: buf,
                left: 0.0,
                top: row as f32 * cell_h,
                scale: 1.0,
                bounds: TextBounds {
                    left: 0,
                    top: 0,
                    right: res.width as i32,
                    bottom: res.height as i32,
                },
                default_color: glyphon::Color::rgb(0xcc, 0xcc, 0xcc),
                custom_glyphs: &[],
            })
            .collect();
        self.renderer.prepare(
            device,
            queue,
            &mut self.font_system,
            &mut self.atlas,
            &self.viewport,
            areas,
            &mut self.swash,
        )
    }

    /// 把已 `prepare` 的文字画进 `pass`。失败(如图集条目在 prepare 之后被淘汰)
    /// 不 panic,交调用方决定跳过。
    pub fn render<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
    ) -> Result<(), glyphon::RenderError> {
        self.renderer.render(&self.atlas, &self.viewport, pass)
    }

    /// 清理图集里不再被引用的字形条目。glyphon 的 LRU 只有 `trim()` 才会真正淘汰;
    /// 不调用的话长会话(尤其中文/高频刷新)迟早把图集喂满,`prepare` 返回
    /// `PrepareError::AtlasFull`。每帧 present 之后调用一次(T3 之外的又一道守护)。
    pub fn trim(&mut self) {
        self.atlas.trim();
    }
}

/// 用 'M' 估等宽字符宽度。核对 cosmic-text 0.12 的 LayoutRun / glyph 结构后取宽度。
fn measure_cell_w(fs: &mut FontSystem, font_px: f32, line_h: f32) -> f32 {
    let mut buf = Buffer::new(fs, Metrics::new(font_px, line_h));
    buf.set_text(
        fs,
        "M",
        Attrs::new().family(Family::Name(FONT_FAMILY)),
        Shaping::Advanced,
    );
    buf.shape_until_scroll(fs, false);
    buf.layout_runs()
        .next()
        .and_then(|run| run.glyphs.last().map(|g| g.x + g.w))
        .unwrap_or(font_px * 0.6)
        .max(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(ch: char, fg: Rgb, spacer: bool) -> SnapCell {
        SnapCell {
            ch,
            fg,
            bg: Rgb::new(0, 0, 0),
            width: if ch == '中' { 2 } else { 1 },
            spacer,
        }
    }

    #[test]
    fn splits_spans_by_fg() {
        let white = Rgb::new(0xcc, 0xcc, 0xcc);
        let red = Rgb::new(205, 0, 0);
        let row = [cell('a', white, false), cell('b', red, false)];
        let spans = row_to_spans(&row);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].0, "a");
        assert_eq!(spans[1].0, "b");
        assert_eq!(spans[0].1, to_color(white));
        assert_eq!(spans[1].1, to_color(red));
    }

    #[test]
    fn merges_same_fg_run() {
        let w = Rgb::new(0xcc, 0xcc, 0xcc);
        let row = [
            cell('a', w, false),
            cell('b', w, false),
            cell('c', w, false),
        ];
        let spans = row_to_spans(&row);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].0, "abc");
    }

    #[test]
    fn skips_wide_char_spacer() {
        // F16:宽字符右半 spacer 不产生字形;'中' 与后续 'x' 同色应合并成 "中x"。
        let w = Rgb::new(0xcc, 0xcc, 0xcc);
        let row = [
            cell('中', w, false),
            cell(' ', w, true),
            cell('x', w, false),
        ];
        let spans = row_to_spans(&row);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].0, "中x");
    }
}
