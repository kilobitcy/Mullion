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
///
/// **只在一个 [`RowRun`] 内部用**。整行直接交给它排版会踩 [`row_to_runs`]
/// 文档里那个错位 bug。
fn row_to_spans(cells: &[SnapCell]) -> Vec<(String, Color)> {
    let mut spans: Vec<(String, Color)> = Vec::new();
    for cell in cells {
        if cell.spacer {
            continue; // 宽字符右半:字形已由左格承载
        }
        // F18:选中格反色——底色那趟已用 fg 画底(`gpu::quads_for`),
        // 文字这趟必须同步换成 bg,否则就是同色底同色字,选中后文字消失。
        let color = to_color(if cell.selected { cell.bg } else { cell.fg });
        match spans.last_mut() {
            Some((s, c)) if *c == color => s.push(cell.ch),
            _ => spans.push((cell.ch.to_string(), color)),
        }
    }
    spans
}

/// 一行里的一段**可以交给字体自由排版**的连续文本,以及它起始于第几列。
pub struct RowRun {
    /// 0-based 起始列。渲染时 `left = term_px.x + col × cell_w`。
    pub col: u16,
    /// run 内部按前景色切好的段(F18 选区反色、SGR 颜色都靠它)。
    pub spans: Vec<(String, Color)>,
}

/// 一格的字形 advance 是否可信等于 `cell_w`。
///
/// 判据是"ASCII 可打印且不是宽字符":等宽字体对这段码位一定有字形、且 advance
/// 就是我们按 'M' 量出来的那个值。任何别的字符(CJK、制表符号、emoji、带重音的
/// 拉丁字母)都可能触发 cosmic-text 的字体回退,回退字体的 advance 与 `cell_w`
/// 没有任何关系 —— 那正是错位的来源,所以它们各自单独定位。
fn advance_is_cell_wide(cell: &SnapCell) -> bool {
    cell.width <= 1 && cell.ch.is_ascii() && !cell.ch.is_ascii_control()
}

/// 把一行切成逐格对齐的 run(**CJK 对齐的核心**)。
///
/// 原来的做法是整行一个 buffer、`left` 固定在行首,字的位置由字体 advance
/// 累加决定;而底色 / 光标 / 选区高亮由 `gpu::quads_for` 按 `col × cell_w`
/// 精确画。两套定位只在"每个字形的 advance 恰好等于 `cell_w`"时才重合。
/// 一行里出现一个中文就够了:等宽字体没有 CJK 字形,cosmic-text 回退到系统
/// 字体,那个字体的 advance 与我们按 'M' 量的 `cell_w` 无关,于是从那一列起
/// **整行的字与格子全线错开**(用户报的"粘贴的内容和光标之间有空白")。
///
/// 切法:[`advance_is_cell_wide`] 为真的连续格并成一个 run(纯 ASCII 的一行
/// 仍然只有一个 run,不增加 buffer 数);其余每格自成一个 run。
///
/// 列号取**枚举下标**而不是"已输出字符数":宽字符占两格,后者会让宽字之后的
/// 所有内容左移一格。
///
/// 全空白且未选中的 run 直接丢掉:刚连上时满屏是空格,不剪的话一行要建几十个
/// 什么都不画的 buffer(T3)。选中的空白**必须**留着 —— 它的字色被反成了 bg,
/// 丢掉会让选区里的空格露出底色块上的原字色,看起来像"选区里有洞"。
pub fn row_to_runs(cells: &[SnapCell]) -> Vec<RowRun> {
    let mut runs: Vec<RowRun> = Vec::new();
    // 当前正在攒的 run:(起始列, 该 run 覆盖的格)。
    let mut open: Option<(u16, Vec<SnapCell>)> = None;
    let flush = |open: &mut Option<(u16, Vec<SnapCell>)>, runs: &mut Vec<RowRun>| {
        let Some((col, group)) = open.take() else {
            return;
        };
        if group
            .iter()
            .all(|c| !c.selected && (c.ch == ' ' || c.ch == '\0'))
        {
            return; // 什么都不画的 run
        }
        let spans = row_to_spans(&group);
        if !spans.is_empty() {
            runs.push(RowRun { col, spans });
        }
    };
    for (ix, cell) in cells.iter().enumerate() {
        if cell.spacer {
            continue; // 宽字符右半:字形已由左格承载,也不该另起一列
        }
        let col = ix as u16;
        if advance_is_cell_wide(cell) {
            match open.as_mut() {
                Some((_, group)) => group.push(*cell),
                None => open = Some((col, vec![*cell])),
            }
        } else {
            // 非等宽字形:先收掉在攒的 ASCII run,自己单独占一个 run。
            flush(&mut open, &mut runs);
            open = Some((col, vec![*cell]));
            flush(&mut open, &mut runs);
        }
    }
    flush(&mut open, &mut runs);
    runs
}

/// F126:preedit 串里的一格。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreeditCell {
    /// 0-based 列号(终端网格)。
    pub col: u16,
    pub ch: char,
    /// 显示宽度:CJK = 2,其余 = 1。
    pub width: u8,
}

/// F126:把组字中的拼音串摆到终端网格上,从光标格起。
///
/// - 宽字符占两格,与 `SnapCell::width` 同一套判据(`unicode-width`),
///   不另起一套 —— 底色/下划线是按格子画的,两套宽度判据会当场错位。
/// - **超出行尾直接截断**,不折行:preedit 是纯覆盖层,不该有改动行内容布局的权力。
/// - 宽字符跨不过行尾时整个丢掉,不画左半(半个汉字是花屏)。
pub fn preedit_layout(cols: u16, cursor_col: u16, text: &str) -> Vec<PreeditCell> {
    use unicode_width::UnicodeWidthChar;
    let mut out = Vec::new();
    let mut col = cursor_col;
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0).clamp(1, 2) as u16;
        if col + w > cols {
            break;
        }
        out.push(PreeditCell {
            col,
            ch,
            width: w as u8,
        });
        col += w;
    }
    out
}

/// F126:组字期间光标该画在哪一列 —— 拼音串**末尾**(已拍板)。
/// 空串(没在组字)时就是原位。
pub fn preedit_cursor_col(cols: u16, cursor_col: u16, text: &str) -> u16 {
    match preedit_layout(cols, cursor_col, text).last() {
        Some(c) => c.col + u16::from(c.width),
        None => cursor_col,
    }
}

use crate::gpu::PaneRender;
use crate::shell::workspace::PxRect;
use glyphon::{
    Attrs, Buffer, Cache, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache, TextArea,
    TextAtlas, TextBounds, TextRenderer, Viewport,
};

/// 内置默认字体族名。须在系统里已安装;未装则 cosmic-text 回退到默认字体
/// (不崩,但等宽/对齐可能变差)。F21 起用户可以在设置里改成别的族名,
/// 没设时仍是这一款。
pub const DEFAULT_FONT_FAMILY: &str = "Google Sans Code";

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
    /// F21:当前生效的字体族。`None` = [`DEFAULT_FONT_FAMILY`]。
    family: Option<String>,
    /// F21:当前字号的**物理像素**值(= pt × scale × 96/72,换算在 app 侧)。
    /// 留着是为了 `ScaleFactorChanged` 只换 scale 时能重算,不必回头去问设置。
    font_px: f32,
    /// F80:glyphon 的兜底文字色(span 未带显式色时用)。当前每个 span 都带色,
    /// 取不到;留着是为了主题一换就整体跟走,不留一处旧灰的潜伏陷阱。
    default_fg: Rgb,
}

/// 一个 pane 的文字裁剪矩形,`(left, top, right, bottom)`。
///
/// 返回裸元组而不是 `glyphon::TextBounds`,是为了能不依赖 glyphon 类型是否
/// derive `PartialEq` 就把这段几何单测掉 —— 裁错的症状(分屏边界上冒出半行
/// 别人的字)只在滚动时偶发,靠肉眼盯几乎抓不住。
pub fn pane_bounds_ltrb(term: PxRect) -> (i32, i32, i32, i32) {
    let l = term.x as i32;
    let t = term.y as i32;
    (l, t, l + term.w as i32, t + term.h as i32)
}

impl TextLayer {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        font_px: f32,
        family: Option<&str>,
        default_fg: Rgb,
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
        let cell_w = measure_cell_w(&mut font_system, font_px, line_h, family);
        Self {
            font_system,
            swash,
            atlas,
            viewport,
            renderer,
            buffers: Vec::new(),
            cell_w,
            cell_h: line_h,
            family: family.map(str::to_string),
            font_px,
            default_fg,
        }
    }

    /// F21:换字体族 / 字号。重算单元格尺寸,**不重建 `TextLayer`**。
    ///
    /// 尺寸怎么传到远端:**什么都不用另做**。`App::compute_geoms` 直接读
    /// `cell_w`/`cell_h`,下一帧 `Workspace::apply_geometry` 比对 `last_grid`
    /// 不同就发 `window_change`(T4/F34)——与拖窗口、开侧栏、标签栏出现
    /// 完全同一条路径。**任何为了改字体方便而另算一份 cols/rows 的写法都是
    /// 第二条尺寸传播路径**,正是 T4 要挡的东西。
    ///
    /// 非有限或非正的 `font_px` **直接忽略**:让它进去的话 `cell_h` 会变成
    /// NaN/0,前者进 wgpu 的尺寸计算(`gui-render-gotchas.md` 记过的崩溃点),
    /// 后者让 `rows` 变成除零。
    pub fn set_font(&mut self, family: Option<&str>, font_px: f32) {
        if !font_px.is_finite() || font_px <= 0.0 {
            return;
        }
        self.family = family.map(str::to_string);
        self.font_px = font_px;
        self.cell_h = (font_px * 1.25).ceil();
        self.cell_w = measure_cell_w(&mut self.font_system, font_px, self.cell_h, family);
    }

    /// 当前生效的族名,交给 cosmic-text 的那一份(`None` 时是内置默认)。
    fn family_name(&self) -> &str {
        self.family.as_deref().unwrap_or(DEFAULT_FONT_FAMILY)
    }

    /// F21:系统里装了哪些字体族。整理规则(去重/排序/打标)在
    /// `font_pick::sort_families`(纯函数,有测试),这里只负责去问 fontdb。
    pub fn families(&self) -> Vec<crate::font_pick::FontChoice> {
        let raw = self
            .font_system
            .db()
            .faces()
            .flat_map(|f| {
                f.families
                    .iter()
                    .map(move |(name, _lang)| (name.clone(), f.monospaced))
            })
            .collect();
        crate::font_pick::sort_families(raw)
    }

    /// F21:量一个字符在**当前字体**下的 advance,给等宽校验用
    /// (`font_pick::is_monospace_advance`)。
    pub fn advance_of(&mut self, ch: char) -> f32 {
        let family = self.family.clone();
        measure_advance(
            &mut self.font_system,
            self.font_px,
            self.cell_h,
            family.as_deref(),
            ch,
        )
    }

    /// 为所有 pane 准备文字。每个 pane 用自己的 `term_px` 作原点**和**裁剪框。
    ///
    /// buffers 按 `pane_ix` 分段线性存放,与 `areas` 的顺序一一对应 —— glyphon
    /// 的 `prepare` 要求 buffer 借用活到 `render`,所以不能边建边丢。
    pub fn prepare_panes(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        panes: &[PaneRender<'_>],
        res: Resolution,
    ) -> Result<(), glyphon::PrepareError> {
        self.viewport.update(queue, res);
        let metrics = grid_metrics(self.font_px, self.cell_h);

        // 第一遍:填 buffer(要先全部填完,才能借它们建 TextArea)。
        // 每个 buffer 对应一个 `RowRun`,`placements` 记它该落在哪(第二遍用)。
        // 一个 buffer 一个 run 而不是一行:见 `row_to_runs` 的文档(CJK 错位)。
        //
        // **复用而不是每帧 clear + new**:满屏 CJK 时 run 数是行数的几十倍
        // (每个汉字自成一 run),每帧重新分配近千个 `Buffer` 就是 T3 那一类
        // 「GPU 没事干、CPU 在烧」。`buffers` 当池子用,末尾多出来的 truncate 掉。
        let mut placements: Vec<(usize, u16, u16)> = Vec::new();
        // 族名先克隆到局部:`Attrs` 借的是 `&str`,直接借 `self.family` 的话
        // 下面就没法再 `&mut self.font_system` 了(E0502)。每帧一次短字符串
        // 克隆,相对每帧几千个 glyph 的整形开销可以忽略。
        let family_owned = self.family_name().to_string();
        let attrs = Attrs::new().family(Family::Name(&family_owned));
        // 字段级借用分割:`font_system` 与 `buffers` 是两个字段,分别可变借用
        // 合法;写成 `self.xxx` 穿插调用就借不出来了。
        let fs = &mut self.font_system;
        let bufs = &mut self.buffers;
        let (cell_w, cell_h) = (self.cell_w, self.cell_h);
        let mut n = 0usize;
        for (pi, p) in panes.iter().enumerate() {
            for row in 0..p.snap.rows {
                for run in row_to_runs(p.snap.row(row)) {
                    if n == bufs.len() {
                        bufs.push(Buffer::new(fs, metrics));
                    }
                    let buf = &mut bufs[n];
                    // 复用来的 buffer 带着上一次的 metrics:换字号/换 DPI 那一帧
                    // 不重设,字会按旧行高排(F21 的 `set_font` 只改 `cell_h`)。
                    buf.set_metrics(fs, metrics);
                    // 宽度按「从这一列到 pane 右缘」给,不是整个 pane 宽度 ——
                    // 给多了 cosmic-text 不会截断(我们本来就不靠它换行,行尾
                    // 由 `TextBounds` 裁),给少了才会误折行。
                    let avail = p
                        .geom
                        .term_px
                        .w
                        .saturating_sub((f32::from(run.col) * cell_w) as u32)
                        .max(1) as f32;
                    buf.set_size(fs, Some(avail), Some(cell_h));
                    let iter = run.spans.iter().map(|(s, c)| (s.as_str(), attrs.color(*c)));
                    buf.set_rich_text(fs, iter, attrs, Shaping::Advanced);
                    buf.shape_until_scroll(fs, false);
                    placements.push((pi, row, run.col));
                    n += 1;
                }
            }
        }
        // 池子里多出来的必须砍掉:留着的话上一帧的字会被下面第二遍以外的
        // 路径看到(`buffers.len()` 是池容量,不是本帧 run 数)。
        bufs.truncate(n);

        // 第二遍:建 TextArea,bounds 用**该 pane 的**矩形而不是整窗。
        // `left` 加上 `col × cell_w` —— 这一项就是 CJK 对齐的落点:与
        // `gpu::quads_for` 画底色/光标用的是同一个式子。
        let mut areas: Vec<TextArea> = Vec::with_capacity(self.buffers.len());
        for (bi, &(pi, row, col)) in placements.iter().enumerate() {
            let p = &panes[pi];
            let (left, top, right, bottom) = pane_bounds_ltrb(p.geom.term_px);
            areas.push(TextArea {
                buffer: &self.buffers[bi],
                left: p.geom.term_px.x as f32 + f32::from(col) * self.cell_w,
                top: p.geom.term_px.y as f32 + f32::from(row) * self.cell_h,
                scale: 1.0,
                bounds: TextBounds {
                    left,
                    top,
                    right,
                    bottom,
                },
                default_color: glyphon::Color::rgb(
                    self.default_fg.r,
                    self.default_fg.g,
                    self.default_fg.b,
                ),
                custom_glyphs: &[],
            });
        }

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

/// 终端网格的排版度量。**唯一来源** —— 量 `cell_w` 的那次和真正排版的那次
/// 必须用同一个 `Metrics`。
///
/// 曾经不同源:排版用 `Metrics::new(cell_h * 0.8, cell_h)`,而 `cell_w` 是按
/// `Metrics::new(font_px, cell_h)` 量的。`cell_h = ceil(font_px * 1.25)`,
/// 两者只在 `font_px * 1.25` 恰好是整数时相等 —— 10pt@150% 相等、10pt@100%
/// 差 2%,一行 60 列漂出 1.2 格,字压字。守护:
/// `the_font_size_used_for_layout_is_the_one_cell_w_was_measured_with`。
fn grid_metrics(font_px: f32, cell_h: f32) -> Metrics {
    Metrics::new(font_px, cell_h)
}

/// 用 'M' 估等宽字符宽度。核对 cosmic-text 0.12 的 LayoutRun / glyph 结构后取宽度。
fn measure_cell_w(fs: &mut FontSystem, font_px: f32, line_h: f32, family: Option<&str>) -> f32 {
    measure_advance(fs, font_px, line_h, family, 'M')
}

/// 量单个字符的 advance。`measure_cell_w`(定单元格宽)与 `advance_of`
/// (等宽校验)共用 —— 两处若各写一份,校验量的就不是排版实际用的那个宽度。
fn measure_advance(
    fs: &mut FontSystem,
    font_px: f32,
    line_h: f32,
    family: Option<&str>,
    ch: char,
) -> f32 {
    let name = family.unwrap_or(DEFAULT_FONT_FAMILY);
    let mut buf = Buffer::new(fs, grid_metrics(font_px, line_h));
    buf.set_text(
        fs,
        &ch.to_string(),
        Attrs::new().family(Family::Name(name)),
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
    use crate::shell::workspace::PxRect;

    fn cell(ch: char, fg: Rgb, spacer: bool) -> SnapCell {
        SnapCell {
            ch,
            fg,
            bg: Rgb::new(0, 0, 0),
            width: if ch == '中' { 2 } else { 1 },
            spacer,
            selected: false,
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

    #[test]
    fn selected_cell_draws_text_in_background_color() {
        // 与 gpu::quads_for 的反色底配套:底用 fg、字用 bg,两边必须同时改,
        // 只改一边就是「白底白字」或「黑底黑字」——选中后文字直接消失。
        let fg = Rgb::new(0xcc, 0xcc, 0xcc);
        let bg = Rgb::new(0, 0, 0);
        let row = [SnapCell {
            ch: 'a',
            fg,
            bg,
            width: 1,
            spacer: false,
            selected: true,
        }];
        let spans = row_to_spans(&row);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].1, to_color(bg));
    }

    #[test]
    fn selection_boundary_splits_spans() {
        // 选中与未选中相邻时颜色不同,必须切成两段,否则整段用同一个颜色画,
        // 高亮边界会错位。
        let fg = Rgb::new(0xcc, 0xcc, 0xcc);
        let bg = Rgb::new(0, 0, 0);
        let mk = |ch, selected| SnapCell {
            ch,
            fg,
            bg,
            width: 1,
            spacer: false,
            selected,
        };
        let spans = row_to_spans(&[mk('a', true), mk('b', false)]);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].0, "a");
        assert_eq!(spans[1].0, "b");
    }

    // ------------------------------------------------ 逐格定位(CJK 对齐)

    /// **本项目最久的一个隐形 bug**:整行文字交给 glyphon 一次性排版,字的位置
    /// 由字体 advance 决定;而底色 / 光标 / 选区高亮由 `gpu::quads_for` 按
    /// `col × cell_w` 精确画。两套定位只在"每个字形的 advance 恰好等于 cell_w"
    /// 时才重合 —— 一行里只要出现一个中文(等宽字体没有 CJK 字形,cosmic-text
    /// 回退到系统字体,那个字体的 advance 与我们按 'M' 量出来的 cell_w 无关),
    /// 从那一列起整行的字与格子全线错开。
    ///
    /// 用户报的现象是"右键粘贴后,粘贴的内容和光标之间有空白"——光标画在
    /// 格子上,字漂在别处。
    ///
    /// 修法是按 **run** 切:ASCII 连续段合一个 run(等宽字体里 advance 就是
    /// cell_w),其余每个字符自成一个 run 单独定位。每个 run 记住自己的起始列,
    /// 渲染时 `left = term_px.x + col × cell_w`。
    ///
    /// 自证会变红:让 `row_to_runs` 无条件把整行并成一个 run。
    #[test]
    fn a_cjk_char_starts_its_own_run_so_the_rest_of_the_line_stays_on_the_grid() {
        let w = Rgb::new(0xcc, 0xcc, 0xcc);
        // "ab中x":中占两格,spacer 在第 3 列。
        let row = [
            cell('a', w, false),
            cell('b', w, false),
            cell('中', w, false),
            cell(' ', w, true),
            cell('x', w, false),
        ];
        let runs = row_to_runs(&row);
        let cols: Vec<u16> = runs.iter().map(|r| r.col).collect();
        let texts: Vec<String> = runs
            .iter()
            .map(|r| r.spans.iter().map(|(s, _)| s.as_str()).collect())
            .collect();
        assert_eq!(texts, vec!["ab", "中", "x"], "CJK 必须自成一段");
        assert_eq!(
            cols,
            vec![0, 2, 4],
            "'x' 的起始列必须是 4(中占了 2、3 两格)——跳格算错就是原来那个错位"
        );
    }

    /// 纯 ASCII 的一行(绝大多数情况)仍然只有一个 run:等宽字体里它们的
    /// advance 就是 cell_w,拆开只是白白多建 buffer(T3)。
    ///
    /// 自证会变红:让 `row_to_runs` 每个字符都单独成 run。
    #[test]
    fn a_plain_ascii_line_is_still_a_single_run() {
        let w = Rgb::new(0xcc, 0xcc, 0xcc);
        let row: Vec<SnapCell> = "hello world".chars().map(|c| cell(c, w, false)).collect();
        let runs = row_to_runs(&row);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].col, 0);
    }

    /// run 内部照旧按前景色切段 —— 逐格定位不能把 F18 的选区反色 / SGR 颜色
    /// 分段吃掉。
    ///
    /// 自证会变红:让 `row_to_runs` 只产出一个不带颜色分段的字符串。
    #[test]
    fn runs_still_split_spans_by_color() {
        let white = Rgb::new(0xcc, 0xcc, 0xcc);
        let red = Rgb::new(205, 0, 0);
        let runs = row_to_runs(&[cell('a', white, false), cell('b', red, false)]);
        assert_eq!(runs.len(), 1, "同为 ASCII,仍是一个 run");
        assert_eq!(runs[0].spans.len(), 2, "run 内部要按颜色切成两段");
        assert_eq!(runs[0].spans[0].1, to_color(white));
        assert_eq!(runs[0].spans[1].1, to_color(red));
    }

    /// 行首/行中的空洞(spacer 之后紧跟另一个宽字)不能让列号漂移。
    ///
    /// 自证会变红:把 `row_to_runs` 里的列号从「枚举下标」改成「已输出字符数」。
    #[test]
    fn consecutive_wide_chars_each_get_their_own_column() {
        let w = Rgb::new(0xcc, 0xcc, 0xcc);
        let row = [
            cell('中', w, false),
            cell(' ', w, true),
            cell('中', w, false),
            cell(' ', w, true),
        ];
        let cols: Vec<u16> = row_to_runs(&row).iter().map(|r| r.col).collect();
        assert_eq!(cols, vec![0, 2]);
    }

    /// 尾部空白格不该产出 run:满屏空格的终端(刚连上的那一屏)本来一行只要
    /// 0 个 buffer,拆成 80 个就是白烧 CPU(T3)。
    ///
    /// 自证会变红:去掉 `row_to_runs` 里的"全空白 run 丢弃"。
    #[test]
    fn a_blank_line_produces_no_runs() {
        let w = Rgb::new(0xcc, 0xcc, 0xcc);
        let row: Vec<SnapCell> = std::iter::repeat_n(cell(' ', w, false), 80).collect();
        assert!(row_to_runs(&row).is_empty());
    }

    /// 选中的空白格**必须**保留 —— 它的字色被反成了 bg,底色那趟也画了反色块;
    /// 丢掉的话选区里的空格会露出底色块上的原字色,看起来像"选区里有洞"。
    ///
    /// 自证会变红:把丢弃条件从"未选中的空白"放宽成"所有空白"。
    #[test]
    fn selected_blanks_survive_the_blank_run_pruning() {
        let fg = Rgb::new(0xcc, 0xcc, 0xcc);
        let bg = Rgb::new(0, 0, 0);
        let row = [SnapCell {
            ch: ' ',
            fg,
            bg,
            width: 1,
            spacer: false,
            selected: true,
        }];
        assert_eq!(row_to_runs(&row).len(), 1);
    }

    /// §7.1:每个 pane 的 TextArea 必须裁到**自己的** term_px。
    /// 沿用单 pane 时代的整窗 bounds,pane 1 最后一行的字会溢出到 pane 2 上 ——
    /// 症状是"分屏边界附近有半行别人的字",且滚动时才出现,极难复现定位。
    #[test]
    fn pane_bounds_clip_to_the_pane_not_the_window() {
        let term = PxRect {
            x: 400,
            y: 132,
            w: 399,
            h: 568,
        };
        assert_eq!(pane_bounds_ltrb(term), (400, 132, 799, 700));
    }

    /// 零尺寸 pane(窗口被拖到极小)不能算出反向矩形,glyphon 会画出诡异结果。
    #[test]
    fn zero_sized_pane_yields_a_degenerate_but_ordered_rect() {
        let (l, t, r, b) = pane_bounds_ltrb(PxRect {
            x: 10,
            y: 20,
            w: 0,
            h: 0,
        });
        assert!(r >= l && b >= t, "left/top 必须不大于 right/bottom");
    }

    /// ①:**排版用的字号必须与量 `cell_w` 用的字号是同一个**。
    ///
    /// 不同源的话每格都差一点点,一行 60 列累计成整格,后面的字直接压到前面
    /// 的字上(用户报的现象:`.md` 和 `12 条` 重叠)。而且 `cell_h * 0.8`
    /// 与 `font_px` 在 `font_px * 1.25` 是整数时**恰好相等**,所以这个 bug
    /// 只在部分「字号 × 缩放」组合下出现 —— 必须遍历几组才盯得住。
    ///
    /// 判据是「60 个 `M` 的实际 advance == 60 × cell_w」,容差 0.5px:
    /// 半个像素以内人眼看不出,超过就是会累积的系统偏差。
    ///
    /// 这条位移断言守的是端到端不漂,但它依赖的 `cell_w` 和排版 buffer
    /// **现在都经过同一个 `grid_metrics` 调用**——两边同源之后,把
    /// `grid_metrics` 内部公式改坏是「两处一起坏但仍互相自洽」,cosmic-text
    /// 对同字符重复排版天然线性(60 × 单字宽度),这条断言对 `grid_metrics`
    /// 内部改动**不敏感**,恒绿。真正钉死 `grid_metrics` 契约的是下面那对
    /// `assert_eq!`:自证会变红,把 `grid_metrics` 的第一个参数改回
    /// `cell_h * 0.8`。位移断言本身仍然留着,它守的是另一件事——
    /// 「cosmic-text 排同一字符是线性的」这个前提,不能删。
    #[test]
    fn the_font_size_used_for_layout_is_the_one_cell_w_was_measured_with() {
        let mut fs = FontSystem::new();
        for pt in [10.0_f32, 11.0, 13.0] {
            for scale in [1.0_f32, 1.25, 1.5] {
                let font_px = pt * scale * 96.0 / 72.0;
                let cell_h = (font_px * 1.25).ceil();

                // 契约断言:排版字号就是量 cell_w 用的 font_px 本身,不是任何
                // 由 cell_h 反推出来的近似值(原 bug 就是 `cell_h * 0.8`)。
                let m = grid_metrics(font_px, cell_h);
                assert_eq!(
                    m.font_size, font_px,
                    "pt={pt} scale={scale}: 排版字号不是 font_px 本身"
                );
                assert_eq!(
                    m.line_height, cell_h,
                    "pt={pt} scale={scale}: 行高不是 cell_h"
                );

                let cell_w = measure_cell_w(&mut fs, font_px, cell_h, None);

                const COLS: usize = 60;
                let mut buf = Buffer::new(&mut fs, grid_metrics(font_px, cell_h));
                buf.set_text(
                    &mut fs,
                    &"M".repeat(COLS),
                    Attrs::new().family(Family::Name(DEFAULT_FONT_FAMILY)),
                    Shaping::Advanced,
                );
                buf.shape_until_scroll(&mut fs, false);
                let laid = buf
                    .layout_runs()
                    .next()
                    .and_then(|run| run.glyphs.last().map(|g| g.x + g.w))
                    .expect("60 个 M 应该排出一行");

                let want = cell_w * COLS as f32;
                assert!(
                    (laid - want).abs() < 0.5,
                    "pt={pt} scale={scale}: 排版排出 {laid:.2}px,按 cell_w \
                     算应是 {want:.2}px,偏了 {:.2}px({:.2} 格)—— 排版字号\
                     与量 cell_w 的字号不同源",
                    laid - want,
                    (laid - want) / cell_w
                );
            }
        }
    }

    /// ①:`Metrics::new` 在本文件里只许出现一次 —— 就是 `grid_metrics` 内部
    /// 那一次。
    ///
    /// 上面那条测试有个盲区:它验不到 `prepare_panes`(那需要真实 wgpu
    /// Device/Queue,这一层跑不起来)。所以「排版那一处有没有绕过唯一来源」
    /// 只能从源码上扎。将来若有人在 `prepare_panes` 里重新手写一个
    /// `Metrics::new(...)`,文字重叠会原样复发,而端到端只有人眼看得出来。
    ///
    /// 针在运行时拼:写成字面量的话这条测试自己的源码就会匹配上自己,恒绿。
    #[test]
    fn only_grid_metrics_constructs_the_grid_metrics() {
        let needle = concat!("Metrics", "::new(");
        let n = include_str!("text.rs")
            .lines()
            .filter(|l| !l.trim_start().starts_with("///"))
            .filter(|l| l.contains(needle))
            .count();
        assert_eq!(
            n, 1,
            "text.rs 里构造 Metrics 的代码行出现了 {n} 次,应该只有 \
             `grid_metrics` 内部那一次 —— 别的地方直接构造 Metrics 就绕开了\
             唯一来源,排版字号和 cell_w 会再次不同源"
        );
    }

    /// F126:拼音串从光标格开始逐格摆,ASCII 一格一个。
    #[test]
    fn preedit_starts_at_the_cursor_cell() {
        let cells = preedit_layout(20, 3, "abc");
        assert_eq!(cells.len(), 3);
        assert_eq!((cells[0].col, cells[0].ch, cells[0].width), (3, 'a', 1));
        assert_eq!((cells[2].col, cells[2].ch, cells[2].width), (5, 'c', 1));
    }

    /// F126:已转换出的汉字占两格 —— 按一格摆的话,后面的字会左移,
    /// 而底色/下划线是按格子画的,两套定位当场分家。
    ///
    /// 自证会变红:把 `preedit_layout` 里的宽度改成恒 1。
    #[test]
    fn wide_chars_take_two_cells() {
        let cells = preedit_layout(20, 0, "你a");
        assert_eq!((cells[0].col, cells[0].width), (0, 2));
        assert_eq!((cells[1].col, cells[1].width), (2, 1), "汉字之后让开两格");
    }

    /// F126:超出行尾直接截断,不折行 —— preedit 是纯覆盖层,不该有改行内容
    /// 布局的权力。
    ///
    /// 自证会变红:把截断判据 `col + w > cols` 改成 `col > cols`。
    #[test]
    fn preedit_is_truncated_at_the_line_end() {
        let cells = preedit_layout(5, 3, "abcde");
        assert_eq!(cells.len(), 2, "只放得下两格");
        assert_eq!(cells.last().unwrap().col, 4);
    }

    /// F126:宽字符跨不过行尾时整个丢掉,不能只画左半 —— 半个汉字是花屏。
    #[test]
    fn a_wide_char_that_does_not_fit_is_dropped_whole() {
        let cells = preedit_layout(5, 4, "你");
        assert!(cells.is_empty(), "第 4 列放不下两格宽的字");
    }

    /// F126:光标停在拼音串**末尾**(已拍板)。串放不下时停在最后画出来的那格之后。
    ///
    /// 自证会变红:把 `preedit_cursor_col` 改成直接返回 `cursor_col`。
    #[test]
    fn cursor_sits_at_the_end_of_the_preedit() {
        assert_eq!(preedit_cursor_col(20, 3, "abc"), 6);
        assert_eq!(preedit_cursor_col(20, 0, "你a"), 3);
        assert_eq!(preedit_cursor_col(5, 3, "abcde"), 5, "截断后停在行尾");
        assert_eq!(preedit_cursor_col(20, 7, ""), 7, "没在组字就是原位");
    }
}
