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
///
/// `hidden`:F126 组字期间,preedit 覆盖的 `[起列, 止列)` 区间要让正文完全不
/// 出字形——**quad 批先画、文字批后画**,`gpu::preedit_quads` 铺的背景 quad
/// 只盖得住已经画完的 quad 层,盖不住排在它后面的文字层;真正让原字符消失
/// 的必须是文字层自己不产出那几列的字形。区间内的格按「整字丢弃」处理(不是
/// 「按空格填充」——填空格会让一个 run 保持完整但中间夹着不可见字符,读起来
/// 正确,但语义上仍是"这一列有 run 覆盖",不如直接断开、语义对齐渲染顺序更
/// 直白),这会把跨区间的 run 自然劈成两段。宽字符只要有一列落在区间内就整字
/// 都不画(半个宽字是花屏),用**列区间重叠**判定而不是单列包含,这样宽字左
/// 半未被区间直接命中、但右半(spacer 对应的那一列)被命中时,仍能整字剔除。
pub fn row_to_runs(cells: &[SnapCell], hidden: Option<(u16, u16)>) -> Vec<RowRun> {
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
        if let Some((h0, h1)) = hidden {
            // 与 `cell.width` 同一套宽度判据(不新起一套),区间重叠即整字剔除。
            let w = u16::from(cell.width.max(1));
            if col < h1 && col + w > h0 {
                flush(&mut open, &mut runs); // 断开当前 run,这一格不产出字形
                continue;
            }
        }
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

/// F126:preedit 串占据的列区间 `[起列, 止列)`——止列是最后一格的
/// `col + width`。空串返回 `None`。
///
/// 喂给 `row_to_runs` 的 `hidden` 参数,让正文文字层在这个区间让路——
/// 否则组字位置不在行尾时,拼音会跟原字符的字形叠在同一批 buffer 里,
/// 背景 quad(先画)盖不住排在它后面的文字层。
pub fn preedit_span(cells: &[PreeditCell]) -> Option<(u16, u16)> {
    let first = cells.first()?;
    let last = cells.last()?;
    Some((first.col, last.col + u16::from(last.width)))
}

use crate::gpu::PaneRender;
use crate::shell::workspace::PxRect;
use glyphon::{
    Attrs, Buffer, Cache, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache, TextArea,
    TextAtlas, TextBounds, TextRenderer, Viewport,
};

/// F126(代码质量复核 Important #2):这一行该不该把某段列区间当成"空白"
/// 交给 `row_to_runs` —— 判据是纯数据(是不是光标所在行 / 是不是在组字 /
/// 光标本身可不可见),不碰 wgpu、不碰 `FontSystem`,可以脱离 GPU 单测。
///
/// **原 bug 正出在这条判断上**:上一轮直接把它内联写在 `prepare_panes` 的
/// 循环里,`prepare_panes` 整体要真实 wgpu `Device`/`Queue` 才能跑,这条纯
/// 数据判断因此一起被挡在 GPU 门外、从没被单测覆盖过。抽成自由函数,
/// `PaneRender` 本身是纯数据类型(`gpu.rs` 的既有测试大量直接构造它),
/// 三个条件都能在没有窗口的情况下单测到。
pub fn hidden_span_for_row(p: &PaneRender<'_>, row: u16) -> Option<(u16, u16)> {
    if row != p.snap.cursor.row || p.preedit.is_empty() || !p.snap.cursor.visible {
        return None;
    }
    let cells = preedit_layout(p.snap.cols, p.snap.cursor.col, p.preedit);
    preedit_span(&cells)
}

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
    /// F12:跨帧的整形缓存,按 `(PaneId, row)` 分槽。取代了原来那个
    /// "每帧从头填一遍"的 `Vec<Buffer>`。
    cache: crate::shaped_cache::ShapedCache<Buffer>,
    /// 空闲的 `Buffer` 回收池。缓存逐出、重整形、清空时的旧 buffer 都进这里,
    /// 整形时优先从这里取 —— 每帧新建上千个 `Buffer` 就是陷阱 T3,而且滚动
    /// 场景(每帧每行都变)不回收会比改之前更慢。
    pool: Vec<Buffer>,
    /// 临时槽:IME 组字行的正文 + 拼音串 overlay。**它们绝不进 `cache`**
    /// (理由见 `shaped_cache::plan_row` 的文档)。当池子用,不每帧清空。
    temp: Vec<Buffer>,
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

/// 把一段 run 整形进 `buf`。**整个渲染路径上唯一调用 `shape_until_scroll`
/// 的地方** —— 缓存命中时根本不会走到这里,那正是 F12 的收益。
///
/// `avail`(交给 `Buffer::set_size` 的可用宽度)按「从这一列到 pane 右缘」
/// 给,不是整个 pane 宽度:给多了 cosmic-text 不会截断(我们本来就不靠它
/// 换行,行尾由 `TextBounds` 裁),给少了才会误折行。
///
/// 复用来的 buffer 带着上一次的 metrics,所以每次都要 `set_metrics`
/// (换字号/换 DPI 那一帧不重设,字会按旧行高排)。
#[allow(clippy::too_many_arguments)]
fn shape_run(
    buf: &mut Buffer,
    fs: &mut FontSystem,
    metrics: Metrics,
    spans: &[(String, Color)],
    col: u16,
    term_w: u32,
    cell_w: f32,
    cell_h: f32,
    attrs: Attrs<'_>,
) {
    buf.set_metrics(fs, metrics);
    let avail = term_w
        .saturating_sub((f32::from(col) * cell_w) as u32)
        .max(1) as f32;
    buf.set_size(fs, Some(avail), Some(cell_h));
    let iter = spans.iter().map(|(s, c)| (s.as_str(), attrs.color(*c)));
    buf.set_rich_text(fs, iter, attrs, Shaping::Advanced);
    buf.shape_until_scroll(fs, false);
}

/// 本帧某个 `TextArea` 的 buffer 存在哪。
#[derive(Clone, Copy)]
enum BufSrc {
    /// 在跨帧缓存里:`(键, 这一行第几个 run)`。
    Cached((mullion_core::layout::PaneId, u16), usize),
    /// 在临时槽里(IME 组字)。
    Temp(usize),
}

/// 本帧要画的一段文字:画在哪个 pane 的哪一行哪一列,buffer 在哪。
struct Placement {
    pane_ix: usize,
    row: u16,
    col: u16,
    src: BufSrc,
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
            cache: crate::shaped_cache::ShapedCache::new(),
            pool: Vec::new(),
            temp: Vec::new(),
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
        // F12:换字体族/字号/DPI 会让所有已 shape 的 buffer 的 metrics 整体
        // 作废。这是缓存**唯一的显式失效 hook** —— 别的失效源(内容、SGR、
        // 选区、主题、pane 宽度)全部由行指纹与 `term_w` 自动覆盖,不需要
        // 也不应该在各自的入口处再加 hook。
        //
        // `pool` / `temp` 不必清:整形路径每次都调 `set_metrics`,池里的
        // buffer 不会带着陈旧 metrics 上屏。
        self.cache.clear(&mut self.pool);
    }

    /// 当前生效的族名,交给 cosmic-text 的那一份(`None` 时是内置默认)。
    fn family_name(&self) -> &str {
        self.family.as_deref().unwrap_or(DEFAULT_FONT_FAMILY)
    }

    /// F159:影响文字层最终长相、但不进任何行指纹的样式量。整帧指纹吃它。
    ///
    /// 字体族 / 字号 / DPI 换了而所有行的内容都没变时,行指纹一个都不会变
    /// —— 少了这一项,换完字体屏幕会停在旧字体的那一帧上,编译/测试/日志
    /// 全静默(F12 的 `set_font` 显式清缓存治的是另一半:整形结果作废)。
    pub fn style_key(&self) -> crate::frame_fp::StyleKey<'_> {
        crate::frame_fp::StyleKey {
            family: self.family_name(),
            font_px: self.font_px,
            cell_w: self.cell_w,
            cell_h: self.cell_h,
            default_fg: self.default_fg,
        }
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
    /// # F12 差分整形
    ///
    /// 第一遍逐 `(PaneId, row)` 查 [`crate::shaped_cache::ShapedCache`]:
    /// 行指纹与 pane 像素宽都没变就**直接复用上一帧 shape 好的 buffer**,
    /// 连 `row_to_runs` 都不调(它每行要建一批 `String`)。
    ///
    /// 改这里之前先读 `shaped_cache::plan_row` 的文档 —— 尤其是"组字行为
    /// 什么绝不能进缓存"。
    ///
    /// 第二遍建 `TextArea`:glyphon 的 `prepare` 要求 buffer 借用活到
    /// `render`,所以两遍不能合成一遍。`left`/`top`/`bounds` 每帧现算,
    /// 因此**拖动分屏、移动 pane 不需要重整形**,只有宽度变了才需要。
    pub fn prepare_panes(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        panes: &[PaneRender<'_>],
        res: Resolution,
        preedit_fg: Rgb,
    ) -> Result<(), glyphon::PrepareError> {
        use crate::shaped_cache::{CachedRun, RowPlan};

        self.viewport.update(queue, res);
        let metrics = grid_metrics(self.font_px, self.cell_h);
        // 族名先克隆到局部:`Attrs` 借的是 `&str`,直接借 `self.family` 的话
        // 下面就没法再 `&mut self.font_system` 了(E0502)。每帧一次短字符串
        // 克隆,相对整形开销可以忽略。
        let family_owned = self.family_name().to_string();
        let attrs = Attrs::new().family(Family::Name(&family_owned));
        // 字段级借用分割:这几个是 `TextLayer` 的不同字段,分别可变借用
        // 合法;写成 `self.xxx` 穿插调用就借不出来了。
        let fs = &mut self.font_system;
        let cache = &mut self.cache;
        let pool = &mut self.pool;
        let temp = &mut self.temp;
        let (cell_w, cell_h) = (self.cell_w, self.cell_h);

        cache.begin_frame();
        let mut plan: Vec<Placement> = Vec::new();
        let mut temp_n = 0usize;
        let (mut hits, mut misses) = (0u64, 0u64);

        for (pane_ix, p) in panes.iter().enumerate() {
            // 缓存键必须是稳定身份。`pane_ix` 是当帧下标,关掉中间一块
            // pane 会让它挪位 —— 拿它当键会张冠李戴。
            let pane_id = p.geom.id;
            let term_w = p.geom.term_px.w;
            for row in 0..p.snap.rows {
                // F126:组字中的拼音串占的列区间只在光标行生效——正文 run 要
                // 在这个区间让路(见 `row_to_runs` 的 `hidden` 参数文档),
                // 不然背景 quad 盖不住排在它后面的文字层,拼音会和原字符的
                // 字形叠在一起。
                let hidden = hidden_span_for_row(p, row);
                let key = (pane_id, row);
                let hash = p.snap.row_hash(row);
                match crate::shaped_cache::plan_row(cache.get(key), hash, term_w, hidden.is_some())
                {
                    RowPlan::Reuse => {
                        hits += 1;
                        cache.touch(key);
                        if let Some(r) = cache.get(key) {
                            for (ix, run) in r.runs.iter().enumerate() {
                                plan.push(Placement {
                                    pane_ix,
                                    row,
                                    col: run.col,
                                    src: BufSrc::Cached(key, ix),
                                });
                            }
                        }
                    }
                    RowPlan::Reshape => {
                        misses += 1;
                        // 先把旧载荷收回池子再整形,否则滚动场景每帧新建
                        // 上千个 `Buffer`(T3),比改之前还慢。
                        cache.recycle_row(key, pool);
                        let mut runs: Vec<CachedRun<Buffer>> = Vec::new();
                        for run in row_to_runs(p.snap.row(row), hidden) {
                            let mut buf = pool.pop().unwrap_or_else(|| Buffer::new(fs, metrics));
                            shape_run(
                                &mut buf, fs, metrics, &run.spans, run.col, term_w, cell_w, cell_h,
                                attrs,
                            );
                            plan.push(Placement {
                                pane_ix,
                                row,
                                col: run.col,
                                src: BufSrc::Cached(key, runs.len()),
                            });
                            runs.push(CachedRun {
                                col: run.col,
                                payload: buf,
                            });
                        }
                        // 空 `runs` 也要写:整行空白的产物就是空集,不写条目
                        // 的话空行永远 miss,而空行是空闲画面的大头。
                        cache.insert(key, hash, term_w, runs);
                    }
                    RowPlan::Temporary => {
                        for run in row_to_runs(p.snap.row(row), hidden) {
                            if temp_n == temp.len() {
                                temp.push(Buffer::new(fs, metrics));
                            }
                            shape_run(
                                &mut temp[temp_n],
                                fs,
                                metrics,
                                &run.spans,
                                run.col,
                                term_w,
                                cell_w,
                                cell_h,
                                attrs,
                            );
                            plan.push(Placement {
                                pane_ix,
                                row,
                                col: run.col,
                                src: BufSrc::Temp(temp_n),
                            });
                            temp_n += 1;
                        }
                    }
                }
            }

            // F126:组字中的拼音串本身。走临时槽,颜色取默认前景色(它盖在
            // 自己铺的默认背景上,不跟随底下那格原本的 SGR 颜色 —— 那格
            // 颜色可能恰好等于背景色,拼音就隐形了)。守卫与
            // `hidden_span_for_row` 内部判据同源(非空 + 光标可见)。
            let preedit_cells = if !p.preedit.is_empty() && p.snap.cursor.visible {
                preedit_layout(p.snap.cols, p.snap.cursor.col, p.preedit)
            } else {
                Vec::new()
            };
            for c in &preedit_cells {
                if temp_n == temp.len() {
                    temp.push(Buffer::new(fs, metrics));
                }
                let spans = [(c.ch.to_string(), to_color(preedit_fg))];
                shape_run(
                    &mut temp[temp_n],
                    fs,
                    metrics,
                    &spans,
                    c.col,
                    term_w,
                    cell_w,
                    cell_h,
                    attrs,
                );
                plan.push(Placement {
                    pane_ix,
                    row: p.snap.cursor.row,
                    col: c.col,
                    src: BufSrc::Temp(temp_n),
                });
                temp_n += 1;
            }
        }

        // 帧末逐出:本帧没访问过的键(pane 关了、行数缩了、切了标签)全删,
        // 载荷回池子。刻意不在 `close_pane` 之类的地方各加清理 hook。
        cache.end_frame(pool);
        crate::diag::count_reshape(hits, misses);

        // 第二遍:建 TextArea,bounds 用**该 pane 的**矩形而不是整窗。
        // `left` 加上 `col × cell_w` —— 这一项就是 CJK 对齐的落点:与
        // `gpu::quads_for` 画底色/光标用的是同一个式子。
        let mut areas: Vec<TextArea> = Vec::with_capacity(plan.len());
        for pl in &plan {
            let Some(p) = panes.get(pl.pane_ix) else {
                continue;
            };
            let buffer = match pl.src {
                BufSrc::Cached(key, ix) => match self.cache.get(key).and_then(|r| r.runs.get(ix)) {
                    Some(run) => &run.payload,
                    None => continue,
                },
                BufSrc::Temp(i) => match self.temp.get(i) {
                    Some(b) => b,
                    None => continue,
                },
            };
            let (left, top, right, bottom) = pane_bounds_ltrb(p.geom.term_px);
            areas.push(TextArea {
                buffer,
                left: p.geom.term_px.x as f32 + f32::from(pl.col) * self.cell_w,
                top: p.geom.term_px.y as f32 + f32::from(pl.row) * self.cell_h,
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
    use crate::shell::workspace::{PaneGeom, PxRect};
    use mullion_term::snapshot::GridSnapshot;

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
        let runs = row_to_runs(&row, None);
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
        let runs = row_to_runs(&row, None);
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
        let runs = row_to_runs(&[cell('a', white, false), cell('b', red, false)], None);
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
        let cols: Vec<u16> = row_to_runs(&row, None).iter().map(|r| r.col).collect();
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
        assert!(row_to_runs(&row, None).is_empty());
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
        assert_eq!(row_to_runs(&row, None).len(), 1);
    }

    /// F126(spec 复核挖出的真 bug):`hidden` 区间要让正文 run 完全让路,
    /// 不能只是"背景 quad 盖住"——quad 批先画、文字批后画,背景 quad 盖不住
    /// 排在它后面的原字符字形。`abcdef` 隐藏 `[2, 4)` 应该劈成两段:
    /// 第 0-1 列("ab")与第 4-5 列("ef"),第 2、3 列不产出任何字形。
    ///
    /// 自证会变红:把 `row_to_runs` 里 `if let Some((h0, h1)) = hidden` 这整段
    /// 判断删掉(等价于永远不传 `hidden`)——`row_to_runs` 会把 `abcdef` 排成
    /// 一个不劈段的 run,`texts` 就是 `["abcdef"]` 而不是 `["ab", "ef"]`。
    #[test]
    fn hidden_span_splits_a_run_and_drops_the_covered_columns() {
        let w = Rgb::new(0xcc, 0xcc, 0xcc);
        let row: Vec<SnapCell> = "abcdef".chars().map(|c| cell(c, w, false)).collect();
        let runs = row_to_runs(&row, Some((2, 4)));
        let cols: Vec<u16> = runs.iter().map(|r| r.col).collect();
        let texts: Vec<String> = runs
            .iter()
            .map(|r| r.spans.iter().map(|(s, _)| s.as_str()).collect())
            .collect();
        assert_eq!(cols, vec![0, 4], "被隐藏区间劈成两段,第二段从第 4 列起");
        assert_eq!(texts, vec!["ab", "ef"], "第 2、3 列不产出任何字形");
    }

    /// F126:宽字符只要有一列落在隐藏区间内就整字都不画,不能只切掉半个字
    /// (半个宽字是花屏)。'中' 占第 2、3 两列(spacer 在第 3 列)。
    ///
    /// - 隐藏区间 `[2, 4)`(整字都在区间内):整字消失。
    /// - 隐藏区间 `[3, 4)`(只覆盖 spacer 那一列,即宽字的右半):约定按同一条
    ///   "列区间重叠即整字剔除" 判据处理——只要重叠就剔除整字,不单独为
    ///   "只压中右半" 开一条特例分支(特例分支等于又长出一套宽度判据,
    ///   与 `preedit_layout`"半个汉字是花屏"的口径不一致)。
    ///
    /// 自证会变红:把重叠判据 `col < h1 && col + w > h0` 改成单列包含判据
    /// `col >= h0 && col < h1`——那样 `[3, 4)` 命中不了宽字的主格(col=2),
    /// 宽字的左半会被当作没受影响、继续画出来,右半的 spacer 本来就不产字形,
    /// 表面上看不出错,但换成"宽字在 [1,2) 之类只压左半列"的场景就会露出半个
    /// 宽字。
    #[test]
    fn wide_char_is_dropped_whole_when_hidden_span_overlaps_either_half() {
        let w = Rgb::new(0xcc, 0xcc, 0xcc);
        let row = [
            cell('a', w, false),
            cell('b', w, false),
            cell('中', w, false),
            cell(' ', w, true), // spacer:'中' 的右半
            cell('x', w, false),
        ];
        for hidden in [(2, 4), (3, 4)] {
            let runs = row_to_runs(&row, Some(hidden));
            let texts: Vec<String> = runs
                .iter()
                .flat_map(|r| r.spans.iter().map(|(s, _)| s.clone()))
                .collect();
            assert!(
                !texts.iter().any(|s| s.contains('中')),
                "隐藏区间 {hidden:?} 与宽字有重叠,'中' 不该出现在任何 run 里: {texts:?}"
            );
        }
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

    /// F126:`preedit_span` 是喂给 `row_to_runs` 的 `hidden` 区间的唯一来源。
    /// 空 cells(没在组字)必须是 `None`,不能是 `Some((0,0))` 那种退化区间 ——
    /// `hidden` 的重叠判据 `col < h1 && ...` 对 `(0,0)` 恰好永远不重叠,退化区间
    /// 不会露出可见 bug,但语义上是错的,留着会在未来改判据时变成隐患。
    ///
    /// 自证会变红:把 `preedit_span` 的 `?` 提前返回删掉,换成
    /// `cells.first().map(...).unwrap_or_default()`。
    #[test]
    fn preedit_span_covers_first_to_last_and_is_none_when_empty() {
        assert_eq!(preedit_span(&[]), None, "没在组字时不该有隐藏区间");
        let cells = preedit_layout(20, 3, "abc");
        assert_eq!(preedit_span(&cells), Some((3, 6)));
        let wide = preedit_layout(20, 0, "你a");
        assert_eq!(preedit_span(&wide), Some((0, 3)), "宽字占两格,止列要算上它");
    }

    /// F126(代码质量复核 Important #2)以下 `hidden_span_for_row` 系列测试
    /// 覆盖的正是原 bug 所在的那层接线:`prepare_panes` 决定"这一行该不该把
    /// 某段列区间藏起来交给 `row_to_runs`"的判断逻辑,曾经内联写在
    /// `prepare_panes` 的行循环里、被"整体需要真实 wgpu Device/Queue"这个
    /// 借口一起挡在了 GPU 门外。抽成纯函数后,`PaneRender` 本身是纯数据类型,
    /// 这里直接构造即可,不用碰 wgpu、不用碰 FontSystem。
    fn geom_for_hidden_span_tests() -> PaneGeom {
        PaneGeom {
            id: mullion_core::layout::PaneId(1),
            px: PxRect {
                x: 0,
                y: 0,
                w: 400,
                h: 600,
            },
            title_px: PxRect {
                x: 0,
                y: 0,
                w: 400,
                h: 0,
            },
            term_px: PxRect {
                x: 0,
                y: 0,
                w: 400,
                h: 600,
            },
            grid: (20, 4),
        }
    }

    fn snapshot_for_hidden_span_tests(
        cursor_row: u16,
        cursor_col: u16,
        visible: bool,
    ) -> GridSnapshot {
        let blank = SnapCell {
            ch: ' ',
            fg: Rgb::new(0xcc, 0xcc, 0xcc),
            bg: Rgb::new(0x10, 0x10, 0x10),
            width: 1,
            spacer: false,
            selected: false,
        };
        GridSnapshot::new(
            20,
            4,
            vec![blank; 20 * 4],
            mullion_term::snapshot::Cursor {
                row: cursor_row,
                col: cursor_col,
                visible,
                shape: mullion_term::snapshot::CursorShape::Block,
                blinking: true,
            },
        )
    }

    /// 光标行 + preedit 非空 + 光标可见 → `Some(正确区间)`。
    ///
    /// 自证会变红:把 `hidden_span_for_row` 里 `preedit_span(&cells)` 换成
    /// `None`(或者干脆让整个函数体永远 `None`)。
    #[test]
    fn hidden_span_for_row_on_cursor_row_covers_the_preedit() {
        let geom = geom_for_hidden_span_tests();
        let snap = snapshot_for_hidden_span_tests(1, 3, true);
        let p = PaneRender {
            geom,
            snap: &snap,
            focused: true,
            preedit: "abc",
        };
        assert_eq!(hidden_span_for_row(&p, 1), Some((3, 6)));
    }

    /// 非光标行 → `None` —— preedit 只在光标所在那一行生效,别的行不该有任何
    /// 列被藏起来。
    ///
    /// 自证会变红:把 `row != p.snap.cursor.row` 这个判据删掉(不再检查行号)。
    #[test]
    fn hidden_span_for_row_on_a_different_row_is_none() {
        let geom = geom_for_hidden_span_tests();
        let snap = snapshot_for_hidden_span_tests(1, 3, true);
        let p = PaneRender {
            geom,
            snap: &snap,
            focused: true,
            preedit: "abc",
        };
        assert_eq!(hidden_span_for_row(&p, 0), None, "第 0 行不是光标所在行");
        assert_eq!(hidden_span_for_row(&p, 2), None, "第 2 行不是光标所在行");
    }

    /// preedit 为空(没在组字)→ `None`。
    ///
    /// 自证说明(已亲手验证,不是想当然):单独删掉 `hidden_span_for_row` 里的
    /// `p.preedit.is_empty()` 这一支判据**不会**让这条测试变红——`preedit_layout`
    /// 对空串本就返回空 `Vec`,`preedit_span(&[])` 自身的 `?` 提前返回已经兜底
    /// 出 `None`,这条判据目前是（有意保留的）冗余短路,只省一次无意义的
    /// `preedit_layout` 调用,不改变可观察行为。真正钉住"preedit 为空 → None"
    /// 这条契约、会让本测试变红的变异在更底层:把 `preedit_span` 的 `?` 提前
    /// 返回删掉换成 `unwrap_or_default()`(见 `preedit_span_covers_first_to_last_and_is_none_when_empty`
    /// 那条测试的自证注释,它已经钉住这个变异)。这里仍然保留这条测试,是为了在
    /// `hidden_span_for_row` 这一层直接钉住"preedit 为空 → None"的契约,不依赖
    /// 读者跳到 `preedit_span` 才能确认。
    #[test]
    fn hidden_span_for_row_with_empty_preedit_is_none() {
        let geom = geom_for_hidden_span_tests();
        let snap = snapshot_for_hidden_span_tests(1, 3, true);
        let p = PaneRender {
            geom,
            snap: &snap,
            focused: true,
            preedit: "",
        };
        assert_eq!(hidden_span_for_row(&p, 1), None);
    }

    /// 光标不可见(比如滚动到回溯历史区)→ `None` —— 呼应 gpu.rs 里
    /// `preedit_has_zero_effect_when_the_cursor_is_invisible` 的同一条约束,
    /// 这里钉的是产生 hidden 区间的源头。
    ///
    /// 自证会变红:把 `!p.snap.cursor.visible` 这个判据删掉。
    #[test]
    fn hidden_span_for_row_with_invisible_cursor_is_none() {
        let geom = geom_for_hidden_span_tests();
        let snap = snapshot_for_hidden_span_tests(1, 3, false);
        let p = PaneRender {
            geom,
            snap: &snap,
            focused: true,
            preedit: "abc",
        };
        assert_eq!(hidden_span_for_row(&p, 1), None);
    }

    /// 宽字 preedit 的区间端点正确 —— 止列要算上宽字占的两格,不能只按字符数算。
    ///
    /// 自证会变红(已亲手验证):把 `hidden_span_for_row` 里的
    /// `preedit_span(&cells)` 换成按"字符数"而不是"宽度和"算止列,例如
    /// `cells.first().map(|f| (f.col, f.col + p.preedit.chars().count() as u16))`。
    /// 纯 ASCII 场景下字符数恰好等于宽度和,`hidden_span_for_row_on_cursor_row_covers_the_preedit`
    /// 那条测试测不出来;但 "你a" 里 `你` 占两格、字符数却只算 1,止列会退化成
    /// `Some((0, 2))`,与期望的 `Some((0, 3))` 不符,只有这条测试会红。
    #[test]
    fn hidden_span_for_row_wide_char_endpoints_account_for_double_width() {
        let geom = geom_for_hidden_span_tests();
        let snap = snapshot_for_hidden_span_tests(1, 0, true);
        let p = PaneRender {
            geom,
            snap: &snap,
            focused: true,
            preedit: "你a",
        };
        assert_eq!(
            hidden_span_for_row(&p, 1),
            Some((0, 3)),
            "'你' 占第 0-1 列,'a' 占第 2 列,止列该是 3"
        );
    }

    /// 把一串文本铺成一行 `SnapCell`,不足 `cols` 的用空格补满 —— 与
    /// `Emulator::snapshot` 出来的行同形(宽字符后面跟一个 spacer 格)。
    fn bench_row(text: &str, cols: u16) -> Vec<SnapCell> {
        use unicode_width::UnicodeWidthChar;
        let fg = Rgb::new(0xcc, 0xcc, 0xcc);
        let bg = Rgb::new(0x1e, 0x1e, 0x1e);
        let blank = SnapCell {
            ch: ' ',
            fg,
            bg,
            width: 1,
            spacer: false,
            selected: false,
        };
        let mut v: Vec<SnapCell> = Vec::with_capacity(cols as usize);
        for ch in text.chars() {
            let w = ch.width().unwrap_or(1) as u8;
            if v.len() + usize::from(w) > cols as usize {
                break;
            }
            v.push(SnapCell {
                ch,
                fg,
                bg,
                width: w,
                spacer: false,
                selected: false,
            });
            if w == 2 {
                v.push(SnapCell {
                    ch,
                    fg,
                    bg,
                    width: 2,
                    spacer: true,
                    selected: false,
                });
            }
        }
        v.resize(cols as usize, blank);
        v
    }

    /// C1 量化脚手架:一帧 shaping 到底花多少。**不是断言型测试**,所以标了
    /// `#[ignore]`,由人工跑:
    ///
    /// ```text
    /// cargo test -p mullion-app --release shaping_cost -- --ignored --nocapture
    /// ```
    ///
    /// 为什么需要它:`prepare_panes` 每帧对**每个 run** 无条件
    /// `set_rich_text` + `shape_until_scroll`,屏幕内容一帧没变也照做一遍。
    /// 该不该给它上缓存(cosmic-text 有个默认关着的 `shape-run-cache`)、
    /// 还是该按 `Term::damage()` 做脏行短路,得先知道这一步的量级 ——
    /// 而 shaping 是纯 CPU 的、不碰 GPU,无头机器上就量得出来。
    ///
    /// 两种输入必须分开量,平均数会把关键的不对称性抹掉。`row_to_runs` 把
    /// 「advance 等于格宽」的连续格并成一个 run,于是:
    ///   - 满屏 ASCII:一行只有 1 个 run,但每行文本各不相同
    ///   - 满屏 CJK :每个汉字自成 1 个 run,run 数是行数的几十倍
    ///
    /// 缓存对前者几乎必然不命中(key 就是整行文本,滚动日志每行都是新的),
    /// 对后者命中率极高(key 集合收敛到常用汉字那几千个)。所以这里的 CJK
    /// 行刻意从一个有界的字集里取字,ASCII 行刻意每行都不一样 —— 这才是
    /// 两种场景各自的真实形态。
    ///
    /// **绝对值只在同一台机器上横向比较有意义**:Linux 上既没有
    /// Google Sans Code 也没有微软雅黑,两种文字都会 fallback 到别的字体,
    /// shaping 成本与 Windows 实机不同。不要拿这里的数字去对 N2/N7 的表。
    ///
    /// 调用序列是照着 `prepare_panes` 的内层循环抄的 —— **那边改了这里要
    /// 跟着改**,否则量的就不是真实路径了。
    ///
    /// # 本机首测结果(2026-08-21,Linux 开发机,仅供同机横向比较)
    ///
    /// | 场景 | 现状 | 开 `shape-run-cache` 对照 |
    /// |---|---|---|
    /// | ASCII 静态 | 2.263 ms | 0.978 ms(−57%) |
    /// | ASCII 滚动 | 2.166 ms | 0.855 ms(−61%) |
    /// | CJK 静态 | 2.852 ms | 2.040 ms(−28%) |
    /// | CJK 滚动 | 2.877 ms | 2.068 ms(−28%) |
    ///
    /// 三条结论,都跟动手前的直觉相反,记下来免得再猜一遍:
    ///
    /// 1. **单 pane 就吃掉 60Hz 一帧预算的 13~17%**(2.2~2.9ms / 16.667ms)。
    ///    8 pane 分屏 ≈ 18~23ms/帧,单这一步就超预算 —— N2 的头号嫌疑人。
    /// 2. **缓存对 ASCII 滚动同样有效**(−61%),不像"每行文本都不同就必不
    ///    命中"那么直觉。因为 cosmic-text 的 key 粒度是 `ShapeWord` 里
    ///    「词内同 script 段」而不是整行(见其 `shape.rs::shape_run_cached`
    ///    的 `line[start_run..end_run]`),日志里反复出现的 `worker`/`task`/
    ///    `bytes` 这类词全都命中。反过来说,UUID / hash / base64 那种高熵
    ///    输出仍会不断产生新 key,而 `ShapeRunCache::trim` 在 cosmic-text
    ///    和 glyphon 内部**都没有调用点** —— 真要开它,得自己定期 trim。
    /// 3. **CJK 的瓶颈不是 shaping 算法**。每个汉字自成 run、key 是单字、
    ///    命中率应接近 100%,却只快 28%;成本大头是 2400 个 run 的固定
    ///    管理开销(`set_size`/`set_rich_text`/`shape_until_scroll` 各调
    ///    2400 次)。缓存治不了这个,只有减少「每帧重建的 run 数」能治 ——
    ///    也就是按 `Term::damage()` 做脏行短路。
    #[test]
    #[ignore = "量化脚手架,不做断言。人工跑:-- --ignored --nocapture"]
    fn shaping_cost_per_frame() {
        use std::time::Instant;

        const COLS: u16 = 120;
        const ROWS: u16 = 40;
        const FRAMES: u32 = 60;
        /// 常用汉字的近似字集大小 —— CJK 行从这个区间里取字,模拟真实中文
        /// 「字数有界、组合无界」的形态。
        const CJK_POOL: u32 = 3000;

        // 多造 FRAMES 行:滚动模式下逐帧往下挪一行窗口。
        let total = u32::from(ROWS) + FRAMES;
        let ascii: Vec<Vec<SnapCell>> = (0..total)
            .map(|i| {
                bench_row(
                    &format!(
                        "[{i:04}] worker-{i} finished task in {}ms, bytes={}, ok",
                        i * 7 % 997,
                        i * 131 % 65536
                    ),
                    COLS,
                )
            })
            .collect();
        let cjk: Vec<Vec<SnapCell>> = (0..total)
            .map(|i| {
                let s: String = (0..u32::from(COLS / 2))
                    .filter_map(|k| char::from_u32(0x4E00 + (i * 31 + k) % CJK_POOL))
                    .collect();
                bench_row(&s, COLS)
            })
            .collect();

        let font_px = 16.0_f32;
        let cell_h = (font_px * 1.25).ceil();
        let metrics = grid_metrics(font_px, cell_h);

        // 「时间维度」的两种形态必须都量,它们对任何按文本做 key 的缓存意义
        // 完全相反:
        //   静态 —— 同一屏重绘(切焦点、拖分屏边界、光标闪烁),每帧文本一模
        //            一样,缓存 100% 命中
        //   滚动 —— 流式输出(**本项目的主场景**:远端 TUI 在刷屏),每帧都有
        //            新行进来,ASCII 那一半的 key 是一次性的,必然不命中
        // 只量静态会把缓存的收益放大到不真实 —— 何况 T3 的帧率节流本来就让
        // 静置时不重绘,静态那一列的现实权重远小于滚动。
        for (text_kind, lines) in [("ASCII", &ascii), ("CJK", &cjk)] {
            for scrolling in [false, true] {
                // FontSystem::new() 会扫系统字体,几百毫秒起 —— 建在计时之外。
                // 每组各建一个,免得上一组把字形缓存捂热了带进下一组。
                let mut fs = FontSystem::new();
                let mut bufs: Vec<Buffer> = Vec::new();
                let attrs = Attrs::new().family(Family::Name(DEFAULT_FONT_FAMILY));

                let one_frame = |f: u32, fs: &mut FontSystem, bufs: &mut Vec<Buffer>| -> usize {
                    let top = if scrolling { f as usize } else { 0 };
                    let mut n = 0usize;
                    for row in &lines[top..top + usize::from(ROWS)] {
                        for run in row_to_runs(row, None) {
                            if n == bufs.len() {
                                bufs.push(Buffer::new(fs, metrics));
                            }
                            let buf = &mut bufs[n];
                            buf.set_metrics(fs, metrics);
                            // 给足够宽,跟 prepare_panes 一样不靠 cosmic-text 换行。
                            buf.set_size(fs, Some(f32::from(COLS) * font_px), Some(cell_h));
                            let iter = run.spans.iter().map(|(s, c)| (s.as_str(), attrs.color(*c)));
                            buf.set_rich_text(fs, iter, attrs, Shaping::Advanced);
                            buf.shape_until_scroll(fs, false);
                            n += 1;
                        }
                    }
                    bufs.truncate(n);
                    n
                };

                // 首帧含字形加载/缓存预热,与稳态分开报。
                let t0 = Instant::now();
                let runs = one_frame(0, &mut fs, &mut bufs);
                let first = t0.elapsed();

                let t1 = Instant::now();
                for f in 0..FRAMES {
                    one_frame(f, &mut fs, &mut bufs);
                }
                let steady = t1.elapsed() / FRAMES;

                let mode = if scrolling { "滚动" } else { "静态" };
                println!(
                    "{text_kind:<5} {mode} {COLS}×{ROWS}: run={runs:<5} \
                     首帧={first:>9.3?}  稳态={steady:>9.3?}/帧"
                );
            }
        }
        println!("(60Hz 一帧预算 16.667ms。以上是**单 pane**,8 pane 分屏乘 8)");
    }
}
