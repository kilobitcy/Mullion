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

/// F195:合并判据的容差 —— 一格的 advance 与 `width × cell_w` 的最大允许偏差,
/// 按 `cell_w` 的比例给。
///
/// 与 [`MAX_MERGED_CELLS`] 是**一对**:偏差逐格累加,最坏漂移是两者之积。
/// 单看任何一个都判断不了「用户会不会看出来」,守护
/// `tests::the_tolerance_and_the_cap_keep_worst_case_drift_well_under_a_cell`
/// 钉的是那个积。
pub const ADVANCE_EPS_RATIO: f32 = 0.001;

/// F195:一个 run 最多并多少格。见 [`ADVANCE_EPS_RATIO`]。
pub const MAX_MERGED_CELLS: usize = 128;

/// F195:这一格的字形 advance 是不是**量出来就等于** `width × cell_w`。
///
/// # 为什么换掉「是不是 ASCII」
///
/// 老判据是"ASCII 可打印且非宽字",其余每格自成一个 run。它安全但过度保守:
/// 在 tmux 里跑 Claude Code 是满屏框线 `─│┌└`,而这些字符在等宽字体里
/// **本来就有字形、advance 就是 `cell_w`** —— 却被一律拆成一格一个
/// `glyphon::Buffer`,一行 120 列就是 120 个。按实测的两项定价,一行
/// 239KB → 33KB(7.2 倍)。
///
/// 新判据直接问那个真正要紧的问题:这个字形排出来占的宽度,跟格子对得上吗。
/// 对得上就并,对不上就单独站开(回退字体的 advance 与 `cell_w` 没有任何
/// 关系,并进去就是从那一列起整段错位)。
///
/// # 记忆化
///
/// 量一次要建一个 `Buffer` 并整形,不能每格都来 —— 按字符记进 `memo`
/// (同一字符的 `SnapCell::width` 由 unicode-width 决定,是字符的函数,
/// 所以键只用 char 就够)。换字体族/字号/DPI 时 `cell_w` 变了,全部结论
/// 作废,`TextLayer::set_font` 里连同整形缓存一起清。
///
/// 新字符第一次出现时会在帧路径上整形一次(T3)。这是有界的:一个会话里
/// 不同字符就那么多,且每个只付一次。
///
/// # 配置的字体装不上时会退化成「一格一个 run」
///
/// 判据问的是「字形宽度对不对得上格子」。字体族不存在时 cosmic-text 会回退,
/// 而回退的多半是**比例字体** —— 本机(无 Google Sans Code)实测:
/// `M` 13.80、`i` 4.44、`x` 9.47、`─` 9.63。一格都对不上,于是**连 ASCII 都
/// 不再合并**,一行 120 列就是 120 个 buffer。
///
/// 这不是判据写错了:那种情况下逐格定位才是**对**的(合并会让整行错位,
/// 那正是 F195 之前 CJK 的老毛病)。但它是一道性能悬崖,且触发条件
/// (字体没装)在日志里不显眼。实机若见 `text:` 与 `reshape=miss:` 同时
/// 异常高,先查字体族名拼对没有。
fn advance_fits_its_cells(
    fs: &mut FontSystem,
    memo: &mut std::collections::HashMap<char, bool>,
    font_px: f32,
    line_h: f32,
    family: &str,
    cell_w: f32,
    cell: &SnapCell,
) -> bool {
    // 期望宽是 `width × cell_w`;width 为 0 时期望 0,而真画出东西的格
    // advance 不可能是 0 —— 与其让它走一遍必然为假的测量,不如在这里挡住,
    // 顺便让「有人为了放宽判据把 0 宽放进来」这件事需要显式删掉这一行。
    if cell.width < 1 {
        return false;
    }
    let want = f32::from(cell.width) * cell_w;
    let eps = cell_w * ADVANCE_EPS_RATIO;
    *memo.entry(cell.ch).or_insert_with(|| {
        let got = measure_advance(fs, font_px, line_h, Some(family), cell.ch);
        (got - want).abs() <= eps
    })
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
/// 切法:`fits` 为真的连续格并成一个 run,其余每格自成一个 run。run 长度
/// 封顶在 [`MAX_MERGED_CELLS`](累积漂移要有界)。
///
/// `fits` **注入**而不是写死:生产传的是 [`advance_fits_its_cells`](要一个
/// `FontSystem`),而这个函数的一批纯单测扎的是**切分机制**(断在哪、列号
/// 怎么算、空白怎么剪、组字区间怎么劈),与判据是什么无关。写成方法或
/// 直接调 `advance_fits_its_cells`,那批断言就得各自背一个字体系统 ——
/// 本项目的架构约束(无头可测)第一条就是不许这么干。
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
pub fn row_to_runs(
    cells: &[SnapCell],
    hidden: Option<(u16, u16)>,
    fits: &mut impl FnMut(&SnapCell) -> bool,
) -> Vec<RowRun> {
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
        if fits(cell) {
            // 满了就先收掉再另起:漂移逐格累加,不封顶的话一行几百列能漂过
            // 半格,而画面只是「行尾那几个字有点歪」,没有任何东西会报错。
            if open
                .as_ref()
                .is_some_and(|(_, g)| g.len() >= MAX_MERGED_CELLS)
            {
                flush(&mut open, &mut runs);
            }
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

/// F192:一个整形完的 `Buffer` 的**固定**开销(`BufferLine` / `Vec` 头 /
/// shape 与 layout 两层 cache 的空壳),与它里面排了几个字形无关。
///
/// 与 [`GLYPH_EST_BYTES`] 一起构成两项模型 `固定 + 边际 × 字形数`。这两个数
/// 是实测拟合出来的(四个点:1→2371、20→7580、60→16900、200→55920 字节,
/// 最小二乘,误差 ≤16%),标定测试见
/// `the_shaped_buffer_price_model_matches_what_it_actually_costs`。
///
/// **为什么不能用单常数。** F169 当初写的是一个 4096。但单字 run 值 2.4KB、
/// 200 格的 ASCII 行值 56KB —— 24 倍跨度,单常数在其中一端必错一个数量级。
/// 更要命的是它会**倒过来误导优化**:F195 要把满屏 CJK 的 120 个单字 run
/// 合成 1 个,单常数模型报「省 120 倍」,两项模型报「省 7.2 倍」——
/// 后者才是真的。拿前者立项就是照着一个错的尺子调优。
pub const BUFFER_FIXED_BYTES: usize = 1770;

/// F192:每多排一个字形的**边际**堆开销(cluster / glyph / layout 三处各一份
/// 定长结构)。见 [`BUFFER_FIXED_BYTES`] 的文档。
pub const GLYPH_EST_BYTES: usize = 269;

/// F192:文字层驻留内存估算的**算术本体**。抽成自由函数 + 对载荷泛型,
/// 是为了让它能在没有 GPU 的机器上被断言 —— `TextLayer` 要真实 wgpu
/// `Device` 才构造得出来,把这段算术留在方法里等于永远测不到。
///
/// **计的是 Buffer 数,不是行数。** `cache` 一行装的是 `Vec<CachedRun<T>>`,
/// 而 `row_to_runs` 把每个非 ASCII 字符单独切成一个 run —— 满屏框线的 TUI 下
/// 一行 120 列就是 120 个 Buffer。F169 当初写的 `cache.len()` 是行数,
/// 在这种画面上低报整整一个数量级(实机 v0.1.81:三笔记账合计 27MB,
/// 而 `堆=98MB`,71MB 无处归因)。
///
/// **两项定价**:`buffers × 固定价 + glyphs × 边际价`。单常数模型在长短两端
/// 各错一个方向,理由见 [`BUFFER_FIXED_BYTES`]。
///
/// `pool` / `temp` 收 `(载荷, 字形数)`:池子里躺的多半是长行退下来的 buffer,
/// 只数个数会把它们按 1.8KB 计,而它们单个值 56KB。
pub fn bytes_estimate_of<T>(
    cache: &crate::shaped_cache::ShapedCache<T>,
    pool: &[(T, u32)],
    temp: &[(T, u32)],
) -> usize {
    let loose_glyphs = |v: &[(T, u32)]| v.iter().map(|(_, g)| *g as usize).sum::<usize>();
    let buffers = cache.payload_count() + pool.len() + temp.len();
    let glyphs = cache.glyph_count() + loose_glyphs(pool) + loose_glyphs(temp);
    buffers * BUFFER_FIXED_BYTES + glyphs * GLYPH_EST_BYTES
}

/// F172:一条行带的顶点槽。
///
/// `fp` 是**上一次 prepare 成功之后**这一带的指纹。`None` 表示这一带的
/// `renderer` 里没有可信顶点(刚建、或上一帧 prepare 失败),必须重建。
struct BandSlot {
    renderer: TextRenderer,
    fp: Option<u64>,
    /// 帧序号,帧末逐出用。与 `ShapedCache` 同一手法:不每帧新建访问集合
    /// (帧路径上不分配,T3)。
    last_seen: u64,
    /// F172 影子校验(debug 档)专用:上一帧这一带**实际**交给 glyphon 的
    /// `TextArea` 摘要。与 `fp` 是两条独立的推导路径 —— `fp` 从输入算,
    /// 这个从结果算,对不上就说明指纹漏了输入项。
    shadow: Option<u64>,
}

/// glyphon 文字资源 + 每行一个 Buffer。GPU 胶水:无单测。
pub struct TextLayer {
    font_system: FontSystem,
    swash: SwashCache,
    atlas: TextAtlas,
    viewport: Viewport,
    /// F172:一带一个 `TextRenderer`,按 `(PaneId, 带号)` 分槽,共用上面那个
    /// `atlas`(glyphon 支持:`render` 收 `&TextAtlas`,只有 `prepare` 要 `&mut`)。
    ///
    /// 用 `BTreeMap` 而不是 `HashMap`:遍历顺序稳定,`render` 的 draw 顺序
    /// 因此可复现。各带互不重叠,顺序本身不影响画面,但可复现的顺序让实机
    /// 出问题时的对比有意义。
    /// 键是 **`(PaneId.0, 带号)`** 而不是 `(PaneId, 带号)` —— `PaneId` 只
    /// 派生了 `Eq/Hash`,没派生 `Ord`。为了这里给 core 加 `Ord` 是把渲染层的
    /// 需要漏进布局层,不值当;拆成裸 `u32` 顺序一样确定。
    bands: std::collections::BTreeMap<(u32, u16), BandSlot>,
    /// 帧序号,喂 `BandSlot::last_seen`。
    frame: u64,
    /// F172:下一帧强制全量重建。**唯一的置真来源是 `AtlasFull`** ——
    /// 那时图集里哪些坐标还有效已经说不清了,指纹相同也不能信。
    ///
    /// 别的失效源不需要在这里加 hook:字体/字号/主题/几何/组字全部由带指纹
    /// 自动覆盖(这正是「判在结果上」的收益),而新开的带 `fp` 是 `None`、
    /// 本来就会重建。多加一个列举式 hook 就是多一处日后会漏的地方。
    force_full: bool,
    /// F12:跨帧的整形缓存。F174 起按 [`crate::shaped_cache::ShapeKey`]
    /// (内容 + `term_w`)**内容寻址**分槽,取代了原来那个按 `(PaneId, row)`
    /// 位置寻址的键 —— 滚动时后者会让整块 pane 全部 miss。
    cache: crate::shaped_cache::ShapedCache<Buffer>,
    /// F174:位置寻址的行指纹台账,**只喂 `seg=` 这个诊断量**。
    ///
    /// 缓存改内容寻址之后,「哪些行的内容变了」问不到 `cache` 了(它只知道
    /// 「这份内容见没见过」)。两张表职责切开,理由见 [`crate::row_fp`]。
    row_fp: crate::row_fp::RowFingerprints,
    /// 空闲的 `Buffer` 回收池。缓存逐出、重整形、清空时的旧 buffer 都进这里,
    /// 整形时优先从这里取 —— 每帧新建上千个 `Buffer` 就是陷阱 T3,而且滚动
    /// 场景(每帧每行都变)不回收会比改之前更慢。
    pool: Vec<(Buffer, u32)>,
    /// F195:`advance_fits_its_cells` 的记忆化表(字符 → 这一格并不并得进
    /// run)。量一次要建 `Buffer` 并整形,不记住就是每帧每格来一遍。
    ///
    /// `cell_w` 变了全部结论作废 —— 与整形缓存一起在 `set_font` 里清。
    advance_memo: std::collections::HashMap<char, bool>,
    /// 临时槽:IME 组字行的正文 + 拼音串 overlay。**它们绝不进 `cache`**
    /// (理由见 `shaped_cache::plan_row` 的文档)。当池子用,不每帧清空。
    temp: Vec<(Buffer, u32)>,
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
) -> u32 {
    buf.set_metrics(fs, metrics);
    let avail = term_w
        .saturating_sub((f32::from(col) * cell_w) as u32)
        .max(1) as f32;
    buf.set_size(fs, Some(avail), Some(cell_h));
    let iter = spans.iter().map(|(s, c)| (s.as_str(), attrs.color(*c)));
    buf.set_rich_text(fs, iter, attrs, Shaping::Advanced);
    buf.shape_until_scroll(fs, false);
    // F192:字形数只有在这里数得准(排完版才知道回退/合字之后到底几个字形),
    // 而记账发生在几秒后的 gauge 采样里 —— 那时再遍历上千个 `Buffer` 就跑到
    // 帧路径上去了(T3)。所以整形完顺手数一次,存进 `CachedRun::glyphs`。
    buf.layout_runs().map(|r| r.glyphs.len()).sum::<usize>() as u32
}

/// 本帧某个 `TextArea` 的 buffer 存在哪。
#[derive(Clone, Copy)]
enum BufSrc {
    /// 在跨帧缓存里:`(内容键, 这一行第几个 run)`。F174 起是内容寻址,
    /// 同一条目会被本帧多处 `Placement` 共同引用(所有空行共用一条)。
    Cached(crate::shaped_cache::ShapeKey, usize),
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
        let atlas = TextAtlas::new(device, queue, &cache, format);
        let line_h = (font_px * 1.25).ceil();
        // 用 'M' 的 advance 估等宽单元格宽度。
        let cell_w = measure_cell_w(&mut font_system, font_px, line_h, family);
        Self {
            font_system,
            swash,
            atlas,
            viewport,
            bands: std::collections::BTreeMap::new(),
            frame: 0,
            force_full: false,
            cache: crate::shaped_cache::ShapedCache::new(),
            row_fp: crate::row_fp::RowFingerprints::new(),
            pool: Vec::new(),
            advance_memo: std::collections::HashMap::new(),
            temp: Vec::new(),
            cell_w,
            cell_h: line_h,
            family: family.map(str::to_string),
            font_px,
            default_fg,
        }
    }

    /// F169:文字层驻留内存估算 = (缓存 + 池 + 临时槽)的 Buffer 数 × 单价。
    /// 算术在 [`bytes_estimate_of`](crate::text::bytes_estimate_of),那里能单测。
    pub fn bytes_estimate(&self) -> usize {
        bytes_estimate_of(&self.cache, &self.pool, &self.temp)
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
        // F195:合并判据是拿旧的 `cell_w` 和旧字体量出来的,一条都不能留 ——
        // 留着的话新字体下该断开的地方仍然并着,整行从那一列起错位,而内容
        // 没变、指纹不变,**没有自愈路径**。
        self.advance_memo.clear();
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
    ///
    /// # F172 行带差分
    ///
    /// 第二遍按 [`crate::bands`] 的带号分桶,**只把脏带交给 `prepare`**。
    /// 干净带的 `TextRenderer` 留着上一帧的顶点缓冲,`render` 时照画。
    ///
    /// 收益的来源:glyphon 0.7.0 的 `prepare` 开头就 `glyph_vertices.clear()`,
    /// 把传进去的每个字形重走一遍 LRU 查找 + 顶点 push,成本与**传进去的
    /// 字形数**成正比,与「有没有变」无关。整形缓存省不到这一笔。
    ///
    /// **`trim` 必须与全量重建绑死**,理由见 [`crate::bands::may_trim`] ——
    /// 这是本路径唯一会静默画错字的地方。
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
        let row_fp = &mut self.row_fp;
        let pool = &mut self.pool;
        let temp = &mut self.temp;
        let advance_memo = &mut self.advance_memo;
        let (cell_w, cell_h) = (self.cell_w, self.cell_h);
        let font_px = self.font_px;

        cache.begin_frame();
        row_fp.begin_frame();
        let mut plan: Vec<Placement> = Vec::new();
        let mut temp_n = 0usize;
        let (mut hits, mut misses) = (0u64, 0u64);
        // F172 诊断:本帧内容变了的行号,按 pane 内行序升序累加(段数只统计
        // 局部性,跨 pane 拼在一起会把 pane 边界算成一次断裂 —— 那正是我们
        // 想知道的「变化散不散」,不必分 pane 统计)。
        //
        // F174:这里**必须**问位置寻址的 `row_fp`,不能拿整形 miss 集合顶替。
        // 内容寻址之后 miss 的含义是「这份内容没见过」:滚一行只 miss 一行,
        // 而屏幕上每一行显示的内容都换了 —— 拿 miss 当「变了的行」会报出
        // 「局部性极好」,而带差分的调参依据静默失真(见 `row_fp` 模块文档)。
        let mut changed_rows: Vec<u16> = Vec::new();
        let mut segments = 0u32;

        for (pane_ix, p) in panes.iter().enumerate() {
            // `row_fp` 的键必须是稳定身份。`pane_ix` 是当帧下标,关掉中间一块
            // pane 会让它挪位 —— 拿它当键会张冠李戴。
            let pane_id = p.geom.id;
            let term_w = p.geom.term_px.w;
            for row in 0..p.snap.rows {
                // F126:组字中的拼音串占的列区间只在光标行生效——正文 run 要
                // 在这个区间让路(见 `row_to_runs` 的 `hidden` 参数文档),
                // 不然背景 quad 盖不住排在它后面的文字层,拼音会和原字符的
                // 字形叠在一起。
                let hidden = hidden_span_for_row(p, row);
                let hash = p.snap.row_hash(row);
                // 内容寻址的整形缓存键:整形结果只取决于(内容, term_w, 字体),
                // 字体那一维走 `set_font` 里的显式 clear。
                let key = crate::shaped_cache::ShapeKey { hash, term_w };
                // 诊断记账对**每一行**都做,组字行也不例外:问的是「这一行的
                // 正文内容变了吗」,与它这一帧走不走缓存无关。
                if row_fp.note((pane_id, row), hash) {
                    changed_rows.push(row);
                }
                match crate::shaped_cache::plan_row(cache.get(key), hidden.is_some()) {
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
                        // 内容寻址下这一档意味着「这份内容没见过」,同键旧载荷
                        // 不存在,不必先摘再整形。旧 buffer 的回收全部收口在
                        // `cache.end_frame`(滚出视野时),稳态是平的。
                        let mut runs: Vec<CachedRun<Buffer>> = Vec::new();
                        // F195:合并判据现建现丢 —— 它借着 `fs`,而下面
                        // `shape_run` 也要 `fs`。`row_to_runs` 返回的是自有的
                        // `Vec<RowRun>`,借用到这一句就结束了。
                        let row_runs = {
                            let mut fits = |c: &SnapCell| {
                                advance_fits_its_cells(
                                    fs,
                                    advance_memo,
                                    font_px,
                                    cell_h,
                                    &family_owned,
                                    cell_w,
                                    c,
                                )
                            };
                            row_to_runs(p.snap.row(row), hidden, &mut fits)
                        };
                        for run in row_runs {
                            let mut buf = pool
                                .pop()
                                .map_or_else(|| Buffer::new(fs, metrics), |(b, _)| b);
                            let glyphs = shape_run(
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
                                glyphs,
                            });
                        }
                        // 空 `runs` 也要写:整行空白的产物就是空集,不写条目
                        // 的话空行永远 miss,而空行是空闲画面的大头。
                        cache.insert(key, runs);
                    }
                    RowPlan::Temporary => {
                        let row_runs = {
                            let mut fits = |c: &SnapCell| {
                                advance_fits_its_cells(
                                    fs,
                                    advance_memo,
                                    font_px,
                                    cell_h,
                                    &family_owned,
                                    cell_w,
                                    c,
                                )
                            };
                            row_to_runs(p.snap.row(row), hidden, &mut fits)
                        };
                        for run in row_runs {
                            if temp_n == temp.len() {
                                temp.push((Buffer::new(fs, metrics), 0));
                            }
                            temp[temp_n].1 = shape_run(
                                &mut temp[temp_n].0,
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
                    temp.push((Buffer::new(fs, metrics), 0));
                }
                let spans = [(c.ch.to_string(), to_color(preedit_fg))];
                temp[temp_n].1 = shape_run(
                    &mut temp[temp_n].0,
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

        // 帧末逐出:本帧没访问过的键(pane 关了、行数缩了、切了标签、滚出
        // 视野)全删,载荷回池子。刻意不在 `close_pane` 之类的地方各加清理
        // hook。两张表同一条判据,一起逐。
        cache.end_frame(pool);
        row_fp.end_frame();
        crate::diag::count_reshape(hits, misses);
        segments += crate::bands::segments(&changed_rows);

        self.frame = self.frame.wrapping_add(1);
        let style = crate::frame_fp::style_digest(self.style_key());
        let force_full = std::mem::take(&mut self.force_full);

        // --- F172 第一步:算每一带的指纹,定出脏带集合。---
        //
        // 指纹从**输入**算(行指纹 + pane 几何 + 样式 + 组字),不从「哪一行
        // 走了 Reshape」算 —— 后者是原因侧,漏掉 pane 移动/换主题/组字三类。
        // 整条判据收口在 `bands::plan_bands`(纯函数,无头可测)。
        let slots_ro = &self.bands;
        let plans = crate::bands::plan_bands(
            panes,
            &|k| slots_ro.get(&k).and_then(|s| s.fp),
            style,
            self.default_fg,
            preedit_fg,
            force_full,
        );
        let dirty_n = plans.iter().filter(|b| b.dirty).count();
        crate::diag::count_bands(&plans, u64::from(segments));

        // --- F172 第二步:trim。**只在全带重建的帧做**,理由见 `bands::may_trim`。---
        //
        // 必须排在 prepare 之前:trim 清空 `glyphs_in_use`,随后的 prepare 把
        // 本帧用到的字形重新填回去。反过来的话本帧刚填的标记当场被清掉。
        if crate::bands::may_trim(dirty_n, plans.len()) {
            self.atlas.trim();
        }

        // --- F172 第三步:第二遍建 TextArea,按带分桶,只把脏带交给 prepare。---
        //
        // bounds 用**该 pane 的**矩形而不是整窗。`left` 加上 `col × cell_w`
        // —— 这一项就是 CJK 对齐的落点:与 `gpu::quads_for` 画底色/光标用的是
        // 同一个式子。
        let mut buckets: Vec<Vec<&Placement>> = vec![Vec::new(); plans.len()];
        // (pane_ix, 带号) → 桶下标。带按 pane 顺序连续排开,所以记下每个 pane
        // 的起始下标就够,不必建 HashMap(帧路径上不分配哈希表,T3)。
        let mut pane_base: Vec<usize> = Vec::with_capacity(panes.len());
        {
            let mut acc = 0usize;
            for p in panes {
                pane_base.push(acc);
                acc += crate::bands::band_count(p.snap.rows) as usize;
            }
        }
        for pl in &plan {
            let Some(base) = pane_base.get(pl.pane_ix) else {
                continue;
            };
            let b = crate::bands::band_of(pl.row) as usize;
            // 越界只可能来自「光标行号 ≥ 快照行数」这类不一致,丢弃而不是
            // panic:渲染路径不许 panic。
            if let Some(bucket) = buckets.get_mut(base + b) {
                bucket.push(pl);
            }
        }

        let shadow_on = log::log_enabled!(target: "mullion", log::Level::Debug);
        // 字段级借用分割:`bands` 要 `&mut`,而 `cache`/`temp` 只要 `&`。
        // 写成 `self.xxx` 穿插调用就借不出来了。
        let cache = &self.cache;
        let temp = &self.temp;
        let (cell_w, cell_h, default_fg) = (self.cell_w, self.cell_h, self.default_fg);
        let slots = &mut self.bands;
        let atlas = &mut self.atlas;
        let fs = &mut self.font_system;
        let swash = &mut self.swash;
        let viewport = &self.viewport;
        let frame = self.frame;

        for (i, b) in plans.iter().enumerate() {
            let slot = slots.entry(b.key).or_insert_with(|| BandSlot {
                renderer: TextRenderer::new(atlas, device, wgpu::MultisampleState::default(), None),
                fp: None,
                last_seen: 0,
                shadow: None,
            });
            slot.last_seen = frame;
            if !b.dirty && !shadow_on {
                continue;
            }
            let mut areas: Vec<TextArea> = Vec::with_capacity(buckets[i].len());
            for pl in &buckets[i] {
                let Some(p) = panes.get(pl.pane_ix) else {
                    continue;
                };
                let buffer = match pl.src {
                    BufSrc::Cached(k, ix) => match cache.get(k).and_then(|r| r.runs.get(ix)) {
                        Some(run) => &run.payload,
                        None => continue,
                    },
                    BufSrc::Temp(t) => match temp.get(t) {
                        Some((b, _)) => b,
                        None => continue,
                    },
                };
                let (left, top, right, bottom) = pane_bounds_ltrb(p.geom.term_px);
                areas.push(TextArea {
                    buffer,
                    left: p.geom.term_px.x as f32 + f32::from(pl.col) * cell_w,
                    top: p.geom.term_px.y as f32 + f32::from(pl.row) * cell_h,
                    scale: 1.0,
                    bounds: TextBounds {
                        left,
                        top,
                        right,
                        bottom,
                    },
                    default_color: glyphon::Color::rgb(default_fg.r, default_fg.g, default_fg.b),
                    custom_glyphs: &[],
                });
            }

            // F172 影子校验(debug 档):从**结果**侧再推一遍这一带长什么样,
            // 与从**输入**侧算的指纹互为独立证据。指纹漏了输入项时,判成干净
            // 的带的实际 TextArea 会与上一帧不同 —— 这里当场喊出来。
            if shadow_on {
                let d = shadow_digest(&areas);
                if !b.dirty && slot.shadow != Some(d) {
                    log::error!(
                        target: "mullion",
                        "F172 影子校验失配:带 {:?} 判为干净,实际 TextArea 变了 \
                         ({:?} → {d:x}) —— 带指纹漏了输入项,屏幕上这一块会留着陈旧的字",
                        b.key,
                        slot.shadow,
                    );
                }
                slot.shadow = Some(d);
            }
            if !b.dirty {
                continue;
            }

            if let Err(e) = slot
                .renderer
                .prepare(device, queue, fs, atlas, viewport, areas, swash)
            {
                // 图集满了:哪些坐标还有效已经说不清,**全部带的指纹一律作废**,
                // 下一帧强制全量重建 + trim 自愈。只把当前这一带作废是不够的
                // —— grow/淘汰是全图集范围的。
                for s in slots.values_mut() {
                    s.fp = None;
                    s.shadow = None;
                }
                self.force_full = true;
                return Err(e);
            }
            slot.fp = Some(b.fp);
        }

        // 帧末逐出:本帧没出现过的带(pane 关了、行数缩了、切了标签)连同它的
        // 顶点缓冲一起丢掉。与 `ShapedCache::end_frame` 同一手法。
        //
        // **F196:这一条是安全不变量,不是可调的回收策略。** 有人提过改成
        // 「N 帧未见才回收」以省一批 GPU 分配 churn —— 不能改:`TextRenderer`
        // 缓存的顶点里存的是**图集坐标**,而图集会 grow/淘汰/`trim`。休眠的带
        // 不参与上面的 `prepare`,它那份坐标没人续租也没人作废,等它某帧醒来
        // 时按陈旧坐标取到的是**别的字形**。症状是画面上出现串字,编译、测试、
        // 日志全静默,只有人眼能看见(同族的坑见 `docs/gui-render-gotchas.md`
        // 的 `atlas.trim` 条)。要省 churn 得先给休眠带做坐标续租,那是另一个
        // 设计,不是把 `== frame` 放宽成 `+ N`。
        slots.retain(|_, s| s.last_seen == frame);
        Ok(())
    }

    /// 把已 `prepare` 的文字画进 `pass`。失败(如图集条目在 prepare 之后被淘汰)
    /// 不 panic,交调用方决定跳过。
    ///
    /// F172:一带一次 draw call。带之间不重叠,顺序不影响画面;`BTreeMap`
    /// 保证顺序可复现。
    pub fn render<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
    ) -> Result<(), glyphon::RenderError> {
        for slot in self.bands.values() {
            slot.renderer.render(&self.atlas, &self.viewport, pass)?;
        }
        Ok(())
    }
}

/// F172 影子校验用的摘要:这一带**实际**交给 glyphon 的东西长什么样。
///
/// 刻意从 `TextArea` 与已 shape 的字形推,而不是从行指纹推 —— 它要当带指纹
/// 的独立证人,共用输入就失去意义了。**只在 debug 档调用**:它要走一遍每个
/// 字形,那正是本切片要省掉的开销。
fn shadow_digest(areas: &[TextArea<'_>]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET;
    let mut eat = |v: u64| {
        for b in v.to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(PRIME);
        }
    };
    eat(areas.len() as u64);
    for a in areas {
        eat(u64::from(a.left.to_bits()));
        eat(u64::from(a.top.to_bits()));
        eat(a.bounds.left as u64);
        eat(a.bounds.top as u64);
        eat(a.bounds.right as u64);
        eat(a.bounds.bottom as u64);
        eat(u64::from(a.default_color.0));
        for run in a.buffer.layout_runs() {
            eat(u64::from(run.line_top.to_bits()));
            for g in run.glyphs {
                eat(u64::from(g.glyph_id));
                eat(u64::from(g.x.to_bits()));
                eat(u64::from(g.y.to_bits()));
                eat(u64::from(g.color_opt.map_or(0, |c| c.0)));
            }
        }
    }
    h
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
        let runs = row_to_runs(&row, None, &mut ascii_only);
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

    /// F195 之前的判据(「ASCII 可打印且非宽字」)。**只给纯单测用。**
    ///
    /// 生产走的是量出来的 advance([`advance_fits_its_cells`])—— 那个要
    /// `FontSystem`,而这一批测试扎的是 `row_to_runs` 的**切分机制**
    /// (断在哪、列号怎么算、空白怎么剪、组字区间怎么劈),与判据是什么无关。
    /// 谓词做成注入的,正是为了这批断言不必背上一个字体系统。
    fn ascii_only(c: &SnapCell) -> bool {
        c.width == 1 && c.ch.is_ascii() && !c.ch.is_ascii_control()
    }

    /// F195:判据说「这一格的 advance 就是 `width × cell_w`」时,连续的
    /// **非 ASCII** 也并成一个 run。
    ///
    /// 这是本切片的收益来源。改之前每个非 ASCII 字符自成一个 run —— 在 tmux
    /// 里跑 Claude Code 是满屏框线 `─│┌└`,一行 120 列就是 120 个
    /// `glyphon::Buffer`。实测按两项模型算,合并后 239KB → 33KB(7.2 倍)。
    ///
    /// 自证会变红:把 `row_to_runs` 里「谓词为真就并进当前 run」改回
    /// 「只有 ASCII 才并」。
    #[test]
    fn cells_whose_advance_matches_the_grid_merge_into_one_run() {
        let w = Rgb::new(0xcc, 0xcc, 0xcc);
        let row: Vec<SnapCell> = "┌──────┐".chars().map(|c| cell(c, w, false)).collect();
        let mut all_fit = |_: &SnapCell| true;
        let runs = row_to_runs(&row, None, &mut all_fit);
        assert_eq!(runs.len(), 1, "框线 advance 对得上格子时该并成一个 run");
        assert_eq!(runs[0].col, 0);
    }

    /// F195:判据说不对得上的那一格照旧单独站开,**并把两边劈断**。
    ///
    /// 这是安全侧:回退字体的 advance 与 `cell_w` 没有任何关系,并进去就是
    /// 从那一列起整段错位(用户报过的「粘贴的内容和光标之间有空白」)。
    ///
    /// 自证会变红:让 `row_to_runs` 忽略谓词、无条件合并。
    #[test]
    fn a_cell_whose_advance_is_off_still_stands_alone() {
        let w = Rgb::new(0xcc, 0xcc, 0xcc);
        let row: Vec<SnapCell> = "aa😀aa".chars().map(|c| cell(c, w, false)).collect();
        let mut fits = |c: &SnapCell| c.ch != '😀';
        let texts: Vec<String> = row_to_runs(&row, None, &mut fits)
            .iter()
            .map(|r| r.spans.iter().map(|(s, _)| s.as_str()).collect())
            .collect();
        assert_eq!(texts, vec!["aa", "😀", "aa"]);
    }

    /// F195:run 有长度上限 —— 累积漂移必须封顶。
    ///
    /// 判据是「advance 与 `width × cell_w` 之差在容差内」,不是「相等」。
    /// 差值会**逐格累加**:k 格之后最坏偏 `k × 容差`。不封顶的话一行 300 列
    /// 就能漂过半格,而画面只是「行尾那几个字有点歪」—— 没有任何东西会报错。
    ///
    /// 自证会变红:把 `MAX_MERGED_CELLS` 那条分支删掉。
    #[test]
    fn a_merged_run_is_capped_so_drift_cannot_accumulate() {
        let w = Rgb::new(0xcc, 0xcc, 0xcc);
        let n = MAX_MERGED_CELLS * 2 + 3;
        let row: Vec<SnapCell> = (0..n).map(|_| cell('x', w, false)).collect();
        let mut all_fit = |_: &SnapCell| true;
        let runs = row_to_runs(&row, None, &mut all_fit);
        assert_eq!(runs.len(), 3, "{n} 格该切成 3 段");
        let widest: usize = runs
            .iter()
            .map(|r| {
                r.spans
                    .iter()
                    .map(|(s, _)| s.chars().count())
                    .sum::<usize>()
            })
            .max()
            .unwrap_or(0);
        assert!(widest <= MAX_MERGED_CELLS, "有一段 {widest} 格,超了上限");
    }

    /// F195:容差 × 上限 = 最坏漂移,必须远小于一格。
    ///
    /// 两个常数单看都合理,乘起来才是用户看得见的那个量。任何一个被调大时
    /// 这条会拦住。
    ///
    /// 自证会变红:把 `ADVANCE_EPS_RATIO` 或 `MAX_MERGED_CELLS` 调大十倍。
    #[test]
    fn the_tolerance_and_the_cap_keep_worst_case_drift_well_under_a_cell() {
        let worst = ADVANCE_EPS_RATIO * MAX_MERGED_CELLS as f32;
        assert!(
            worst < 0.25,
            "最坏漂移 {worst} 格,超过四分之一格就该看得见了"
        );
    }

    /// F195:`width == 0` 的格永远不并。
    ///
    /// 期望 advance 是 `width × cell_w`,width 为 0 时期望值是 0 —— 而那一格
    /// 真画出东西时 advance 必然不是 0,于是「不等」→ 不并。这条是把它写死,
    /// 免得日后有人为了「让判据更宽松」把 0 宽也放进来:那会让整段的期望位置
    /// 与实际位置从这一格起彻底脱钩。
    ///
    /// 这一条不需要字体 —— 守卫在量之前就返回了。
    ///
    /// 自证会变红:把 `cell.width < 1` 那条守卫删掉(会变成去量 advance,
    /// 在无字体的机器上行为不定)。
    #[test]
    fn a_zero_width_cell_never_merges() {
        let mut fs = FontSystem::new();
        let mut memo = std::collections::HashMap::new();
        let c = SnapCell {
            ch: 'x',
            fg: Rgb::new(0xcc, 0xcc, 0xcc),
            bg: Rgb::new(0, 0, 0),
            width: 0,
            spacer: false,
            selected: false,
        };
        assert!(!advance_fits_its_cells(
            &mut fs,
            &mut memo,
            16.0,
            20.0,
            DEFAULT_FONT_FAMILY,
            10.0,
            &c
        ));
        assert!(memo.is_empty(), "守卫该在量之前就返回,不留缓存条目");
    }

    /// F195:`cell_w` 就是拿 'M' 量出来的 —— 所以 'M' 一定并得进去。
    ///
    /// 这条与平台无关:不管系统上最终挑中哪个字体,`measure_cell_w` 与
    /// 这里量的是**同一个字符、同一套 metrics**,差值恒为 0。它扎的是
    /// 「判据的方向没写反」(比如把 `<=` 写成 `>=`,或忘了乘 `width`)。
    ///
    /// 自证会变红:把判据里的 `<=` 改成 `>`。
    #[test]
    fn the_character_the_cell_width_came_from_always_merges() {
        let mut fs = FontSystem::new();
        let mut memo = std::collections::HashMap::new();
        let (font_px, line_h) = (16.0, 20.0);
        let cell_w = measure_cell_w(&mut fs, font_px, line_h, None);
        let c = SnapCell {
            ch: 'M',
            fg: Rgb::new(0xcc, 0xcc, 0xcc),
            bg: Rgb::new(0, 0, 0),
            width: 1,
            spacer: false,
            selected: false,
        };
        assert!(advance_fits_its_cells(
            &mut fs,
            &mut memo,
            font_px,
            line_h,
            DEFAULT_FONT_FAMILY,
            cell_w,
            &c
        ));
        assert_eq!(
            memo.len(),
            1,
            "量过的字符该进缓存 —— 不然每帧每格都要整形一次"
        );
    }

    /// F195:走一遍真实 VT —— 字节流喂 `Emulator`,快照喂 `row_to_runs`,
    /// 用**生产那个量出来的判据**。断言两件事:
    ///
    /// 1. **不丢字、不错位**:把各 run 按 `col` 摆回网格,必须还原成原行。
    ///    这一条与平台无关,是合并的安全底线 —— 合并把「一格一个 buffer」
    ///    的天然对齐拿掉了,列号一旦算错就是整段错位,而画面上只是「字挤在
    ///    一起」,没有任何东西会报错。
    /// 2. **真的并起来了**:判据接受 `─` 时,那一行框线必须是一个 run 而不是
    ///    十几个。写成条件断言是因为收益取决于本机字体有没有框线字形
    ///    (Linux 开发机与 Windows 不同);但只要它接受了,合并就必须发生 ——
    ///    「忽略谓词、照旧一格一个」这个变异照样被杀。
    ///
    /// 字节流是**合成**的(无头容器里录不到真 TUI),不是
    /// `tests/fixtures/*.bin` 那种录制品。真机录一份仍然欠着。
    ///
    /// 自证会变红:让 `row_to_runs` 忽略 `fits`,或把 run 的 `col` 改成
    /// 「已输出字符数」(宽字之后整体左移一格)。
    #[test]
    fn a_real_vt_row_of_box_drawing_and_cjk_survives_the_round_trip() {
        use mullion_term::emulator::Emulator;
        const COLS: u16 = 40;
        let mut em = Emulator::new(COLS, 4);
        // 第 0 行纯框线,第 1 行中英混排 —— 这正是 tmux 里 Claude Code 的画面。
        em.feed("┌──────────────┐\r\n".as_bytes());
        em.feed("│ 中文 abc 混排 │\r\n".as_bytes());
        let snap = em.snapshot();

        let mut fs = FontSystem::new();
        let mut memo = std::collections::HashMap::new();
        let (font_px, line_h) = (16.0, 20.0);
        let cell_w = measure_cell_w(&mut fs, font_px, line_h, None);

        for row in 0..2u16 {
            let cells = snap.row(row);
            let runs = {
                let mut fits = |c: &SnapCell| {
                    advance_fits_its_cells(
                        &mut fs,
                        &mut memo,
                        font_px,
                        line_h,
                        DEFAULT_FONT_FAMILY,
                        cell_w,
                        c,
                    )
                };
                row_to_runs(cells, None, &mut fits)
            };

            // 契约一:按 col 摆回网格,原样还原。
            let mut grid: Vec<char> = vec![' '; COLS as usize];
            for r in &runs {
                let mut col = usize::from(r.col);
                for ch in r.spans.iter().flat_map(|(s, _)| s.chars()) {
                    grid[col] = ch;
                    col += usize::from(cells[col].width.max(1));
                }
            }
            let want: String = cells
                .iter()
                .map(|c| if c.spacer { ' ' } else { c.ch })
                .collect::<String>()
                .trim_end()
                .to_string();
            let got: String = grid.iter().collect::<String>().trim_end().to_string();
            assert_eq!(got, want, "第 {row} 行按 col 摆回去对不上");
        }

        // 契约二:判据接受框线就必须真并起来。
        let box_cell = SnapCell {
            ch: '─',
            fg: Rgb::new(0xcc, 0xcc, 0xcc),
            bg: Rgb::new(0, 0, 0),
            width: 1,
            spacer: false,
            selected: false,
        };
        let accepts = advance_fits_its_cells(
            &mut fs,
            &mut memo,
            font_px,
            line_h,
            DEFAULT_FONT_FAMILY,
            cell_w,
            &box_cell,
        );
        let mut fits = |c: &SnapCell| {
            advance_fits_its_cells(
                &mut fs,
                &mut memo,
                font_px,
                line_h,
                DEFAULT_FONT_FAMILY,
                cell_w,
                c,
            )
        };
        let n = row_to_runs(snap.row(0), None, &mut fits).len();
        if accepts {
            assert_eq!(n, 1, "判据接受框线却没并起来 —— F195 的收益没落地");
        } else {
            println!("本机字体不接受 `─`(advance 与 cell_w 对不上),那一行仍是 {n} 个 run");
        }
    }

    /// F195:`set_font` 必须**同时**清整形缓存和合并判据的记忆化表。
    ///
    /// 两张表都建在旧的 `cell_w` / 旧字体上。只清一张的症状是静默且**无自愈
    /// 路径**:换字体后 `advance_memo` 里那些「这一格并得进去」的旧结论继续
    /// 生效,该断开的地方仍然并着 → 整行从那一列起错位;而内容没变、行指纹
    /// 不变、整形缓存刚清过会重整形一次然后就一直命中 —— 除非用户再换一次
    /// 字体,否则永远回不来。
    ///
    /// `set_font` 要真实 wgpu Device 才跑得起来,只能从源码上扎。判据是
    /// **行下标邻近**:`memo` 那一句必须紧跟在 `cache.clear` 后面 ——
    /// 「挪到某个提前 return 之上/之下」这类变异,靠「函数体里包含」是
    /// 抓不住的(F181 的 cfg 分支 early-return 盲区就是这么来的)。
    ///
    /// 自证会变红:删掉 `self.advance_memo.clear();`,或把它挪走。
    #[test]
    fn changing_the_font_clears_the_merge_verdicts_next_to_the_shaping_cache() {
        let lines = prod_lines();
        let cache_clear = concat!("self.cache", ".clear(&mut self.pool);");
        let memo_clear = concat!("self.advance_", "memo.clear();");
        let at = lines
            .iter()
            .position(|l| l.trim() == cache_clear)
            .expect("set_font 里应有一处清整形缓存");
        // 紧邻的下一条**代码**行(注释已被 prod_lines 滤掉)。
        assert_eq!(
            lines.get(at + 1).map(|l| l.trim()),
            Some(memo_clear),
            "清完整形缓存的下一句不是清 advance_memo —— 换字体后合并判据仍用\
             旧 cell_w 的结论,整行错位且没有自愈路径"
        );
    }

    /// F195:整形用的 `Attrs` 不许带 `.weight(` / `.style(`。
    ///
    /// 合并成一个 run 的前提是**整段共用一套字体选择**。粗体/斜体会换 face,
    /// 换了 face 的 advance 与 `cell_w` 没有关系 —— 而判据是**逐字符**量的,
    /// 量的时候用的是不带 weight/style 的 `Attrs`。两边不一致,合并出来的
    /// 长 run 会整段错位,且**只在那一行恰好有 SGR 粗体时**才犯病。
    ///
    /// (顺带解释了为什么 SGR 的 bold 目前是靠颜色表达而不是换 face。)
    ///
    /// 自证会变红:在 `prepare_panes` 的 `attrs` 上挂一个 `.weight(...)`。
    #[test]
    fn shaping_attrs_carry_no_weight_or_style_so_a_merged_run_keeps_one_face() {
        let lines = prod_lines();
        let bad: Vec<&str> = lines
            .iter()
            .copied()
            .filter(|l| l.contains(concat!("Attrs", "::new()")))
            .filter(|l| l.contains(concat!(".weight", "(")) || l.contains(concat!(".style", "(")))
            .collect();
        assert!(
            bad.is_empty(),
            "整形 Attrs 带上了 weight/style:{bad:?} —— 换 face 就换 advance,\
             而合并判据是按不带它们的 Attrs 量的,长 run 会整段错位"
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
        let runs = row_to_runs(&row, None, &mut ascii_only);
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
        let runs = row_to_runs(
            &[cell('a', white, false), cell('b', red, false)],
            None,
            &mut ascii_only,
        );
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
        let cols: Vec<u16> = row_to_runs(&row, None, &mut ascii_only)
            .iter()
            .map(|r| r.col)
            .collect();
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
        assert!(row_to_runs(&row, None, &mut ascii_only).is_empty());
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
        assert_eq!(row_to_runs(&row, None, &mut ascii_only).len(), 1);
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
        let runs = row_to_runs(&row, Some((2, 4)), &mut ascii_only);
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
            let runs = row_to_runs(&row, Some(hidden), &mut ascii_only);
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

    /// 本文件里去掉 `///` 注释行之后的源码。源码切片守护共用。
    fn code_lines() -> Vec<&'static str> {
        include_str!("text.rs")
            .lines()
            .filter(|l| {
                let t = l.trim_start();
                !t.starts_with("///") && !t.starts_with("//!") && !t.starts_with("//")
            })
            .collect()
    }

    /// 只要**生产代码**那一段(测试模块之前)。
    ///
    /// 给「全文件只许有 N 处 X」这类计数型判据用。计数扎的是生产代码里的
    /// 岔路数,而测试自己为了构造被测对象难免要写同一个字面量 —— 拿全文件
    /// 去数,加一条测试就把守护打红,唯一的"修法"是把断言的数字往上调,
    /// 那条守护就此报废。针照旧在运行时拼(见下)。
    fn prod_lines() -> Vec<&'static str> {
        let marker = concat!("#[cfg", "(test)]");
        let all = code_lines();
        let end = all
            .iter()
            .position(|l| l.trim() == marker)
            .unwrap_or(all.len());
        all[..end].to_vec()
    }

    /// F196:**行带的帧末逐出判据必须是「本帧没出现过就丢」,一帧都不许宽限。**
    ///
    /// 这一条被人正经提议过放宽(「N 帧未见才回收,省一批 GPU 分配 churn」),
    /// 所以它需要的不是一句注释而是一条会变红的断言。休眠的带不参与 `prepare`,
    /// 它缓存的顶点里那份**图集坐标**没人续租也没人作废;图集 grow/淘汰/`trim`
    /// 之后那份坐标指向的是别的字形。醒来那一帧画出来的是串字 —— 编译、测试、
    /// 日志全静默,只有人眼能看见。
    ///
    /// 自证会变红:把判据改成 `s.last_seen + 300 >= frame`,或整句删掉。
    #[test]
    fn a_band_that_missed_one_frame_is_evicted_because_its_atlas_coords_are_unrenewed() {
        let needle = concat!("slots.retain(|_, s| s.last_seen ", "== frame);");
        let hits = prod_lines().iter().filter(|l| l.contains(needle)).count();
        assert_eq!(
            hits, 1,
            "行带逐出那句必须原样存在且判据是「等于本帧」。放宽成 N 帧宽限会让\
             休眠带拿陈旧图集坐标画出别的字形,而且完全静默。"
        );
    }

    /// F172:**整个 crate 里只许有一处 `atlas.trim()`,且必须挂在
    /// `bands::may_trim` 的闸下**。
    ///
    /// 这是本切片唯一会静默画错字的地方(理由见 `bands::may_trim`):trim 清空
    /// `glyphs_in_use`,只有本帧真的 prepare 过的带才会把自己的字形标回去;
    /// 干净带的字形失去保护,图集满时被踢掉、槽位让给新字形,而那些带的顶点
    /// 还指着旧坐标 —— **屏幕上画出别的字,不报错、不 panic、日志一片正常**。
    ///
    /// 判据用**行下标邻近**而不是「文件里包含 may_trim」:后者对
    /// 「把 trim 挪出 if、may_trim 那句留在别处」这个变异恒绿。
    ///
    /// 自证会变红:把 `self.atlas.trim()` 挪出那个 `if`,或再加一处。
    #[test]
    fn the_atlas_is_trimmed_only_behind_the_full_rebuild_gate() {
        let lines = code_lines();
        let needle = concat!("atlas", ".trim()");
        let at: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.contains(needle))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            at.len(),
            1,
            "图集 trim 的调用点出现了 {} 次,只许有一处 —— 多一处就是多一条\
             绕过全量重建闸的路,症状是屏幕上某一带画出别的字",
            at.len()
        );
        let i = at[0];
        let gate = concat!("may", "_trim(");
        // 判据是**紧邻的上一行是 `if …may_trim(…) {`**,不是「附近出现过
        // may_trim」——后者对「把闸算进一个变量、trim 照样无条件调」这个
        // 变异恒绿(实测过)。
        let prev = lines.get(i.wrapping_sub(1)).map_or("", |l| l.trim_start());
        assert!(
            prev.starts_with("if ") && prev.contains(gate),
            "图集 trim 的上一行不是全量重建闸(`if …may_trim(…) {{`),而是 `{prev}` \
             —— trim 脱离了「本帧全部带都重建了」这个前提,干净带的字形会被\
             图集踢掉、那一带画出别的字"
        );
    }

    /// F172:干净带**必须**在 `prepare` 之前被 `continue` 掉。
    ///
    /// 这是本切片的全部收益所在。漏了这道 `continue` 的话画面完全正确、
    /// 测试全绿、日志一切正常,只有实机剖面的 `bands=N/N`(比值顶到 1)
    /// 看得出来性能悄悄回到了改之前。
    ///
    /// 自证会变红:把 `prepare` 之前那句 `if !b.dirty { continue; }` 删掉。
    #[test]
    fn a_clean_band_never_reaches_prepare() {
        let lines = code_lines();
        let call = concat!(
            ".pre",
            "pare(device, queue, fs, atlas, viewport, areas, swash)"
        );
        let i = lines
            .iter()
            .position(|l| l.contains(call))
            .expect("找不到 glyphon prepare 的调用点 —— 改了签名就把这条守护一起更新");
        let guard = concat!("if !b.", "dirty {");
        assert!(
            lines[i.saturating_sub(12)..i]
                .iter()
                .any(|l| l.contains(guard)),
            "prepare 调用点上方十二行内没有 `if !b.dirty {{ continue }}` —— \
             干净带也被交给 prepare 了,F172 的收益归零而画面完全正确"
        );
    }

    /// F172:`AtlasFull` 之后必须让**每一带**的指纹作废,而不只是当前这一带。
    ///
    /// 图集的 grow/淘汰是全图集范围的,撞满之后哪些坐标还有效说不清。只作废
    /// 当前带的话,别的带留着「指纹匹配」的旧顶点,指向已经被别人占用的图集
    /// 槽位 —— 自愈路径反而变成永久画错。
    ///
    /// 自证会变红:把错误分支里的 `for s in slots.values_mut()` 循环删掉,
    /// 或把 `self.force_full = true` 删掉。
    #[test]
    fn an_atlas_full_invalidates_every_band_not_just_the_one_that_hit_it() {
        let lines = code_lines();
        let ret = concat!("return Err(", "e);");
        let i = lines
            .iter()
            .position(|l| l.contains(ret))
            .expect("找不到 prepare 失败的返回点");
        let window = &lines[i.saturating_sub(8)..i];
        assert!(
            window
                .iter()
                .any(|l| l.contains(concat!("slots.", "values_mut()"))),
            "prepare 失败时没有遍历全部带作废指纹 —— 别的带会留着指向乱掉的\
             图集槽位的旧顶点,自愈路径变成永久画错"
        );
        assert!(
            window
                .iter()
                .any(|l| l.contains(concat!("force_", "full = true"))),
            "prepare 失败时没有置 `force_full` —— 下一帧不会全量重建 + trim,\
             图集永远满着,画面冻在最后一次成功帧"
        );
    }

    /// F174:整形缓存键的两个字段都必须由**当帧的活值**填,只许用字段简写。
    ///
    /// `shaped_cache` 那边的单测扎的是「键不同则 miss」,扎不到这一层的接线:
    /// 把宽度那一维写死成常量,`shaped_cache::tests` 全绿、编译全绿、静置画面
    /// 也全对 —— 只有分屏拖动改了 pane 宽度那一刻才会犯病:内容没变,于是永远
    /// 命中按旧宽度整形出来的那份产物,文字**永久**按旧列宽排,且没有自愈路径
    /// (实测:这条变异原先没有任何测试能杀掉)。
    ///
    /// `prepare_panes` 需要真实 wgpu Device/Queue,这一层跑不起来,所以只能从
    /// 源码上扎。判据是字段简写(`hash, term_w` 紧跟大括号),`term_w: 0` 这类
    /// 显式赋值一律不认。
    ///
    /// 针在运行时拼:写成字面量的话这条测试自己的源码会匹配上自己,恒绿。
    #[test]
    fn the_shape_key_is_built_from_this_frames_live_hash_and_width() {
        // 只数生产代码:测试要构造 `ShapeKey` 才能断言别的事,拿全文件去数
        // 等于每加一条测试就得把上面那个 `1` 往上调,守护就此报废。
        let lines = prod_lines();
        let ctor = concat!("Shape", "Key { ");
        let at: Vec<&str> = lines.iter().copied().filter(|l| l.contains(ctor)).collect();
        assert_eq!(
            at.len(),
            1,
            "text.rs 里构造整形缓存键的代码行有 {} 处,只许有一处 —— 多一处就是\
             多一条可能填错维度的路",
            at.len()
        );
        let shorthand = concat!("hash, ", "term_w }");
        assert!(
            at[0].contains(shorthand),
            "整形缓存键没有用字段简写填,而是 `{}` —— 任何一维被写死成常量,\
             那一维的失效就静默失灵(宽度那维的症状是分屏改宽后文字永久按旧\
             列宽排,且不会自愈)",
            at[0].trim()
        );
    }

    /// F174:行指纹台账按 `PaneId` 记账,不按当帧下标。
    ///
    /// 关掉中间一块 pane 会让其后每块 pane 的当帧下标挪位,A 的指纹被当成 B 的
    /// 用 —— `seg=` 在分屏变动那一帧彻底乱掉。`row_fp` 自己的单测扎了「键是
    /// 稳定身份」,但扎不到调用点传进来的到底是哪个值。
    ///
    /// 自证会变红:把调用点的 `pane_id` 换成 `pane_ix`(能编过,因为下标只是
    /// 另一个整数,套进 `PaneId` 也不报错)。
    #[test]
    fn the_row_fingerprint_ledger_is_keyed_by_pane_id_at_the_call_site() {
        let lines = code_lines();
        let call = concat!("row_fp.", "note((");
        let at: Vec<&str> = lines.iter().copied().filter(|l| l.contains(call)).collect();
        assert_eq!(at.len(), 1, "行指纹记账点只许有一处");
        assert!(
            at[0].contains(concat!("pane", "_id, row")),
            "行指纹记账用的不是稳定身份,而是 `{}` —— 分屏关掉中间一块之后\
             下标会挪位,A 的指纹被当成 B 的用,`seg=` 静默失真",
            at[0].trim()
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
    /// F192:记账按**每个 Buffer** 计价,不是每行。
    ///
    /// 这两个口径在本项目里差一个数量级 —— `row_to_runs` 把每个非 ASCII
    /// 字符单独切成一个 run,满屏框线的 TUI 下一行 120 列就是 120 个 Buffer。
    /// 按行计价会让 `profile.mem` 的 `text:` 低报同一个数量级(实机 v0.1.81:
    /// 记账 27MB / 堆 98MB,71MB 无处归因)。
    ///
    /// 泛型化 `T` 就是为了这条能在无头机器上跑:`Buffer` 要 `FontSystem`
    /// 才构造得出来,而这里要断言的是**计数口径**,与载荷是什么无关。
    ///
    /// 自证会变红:把 `bytes_estimate_of` 里的 `payload_count()` 换回 `len()`。
    #[test]
    fn the_text_gauge_prices_every_buffer_not_every_row() {
        use crate::shaped_cache::{ShapeKey, ShapedCache};
        let mut c = ShapedCache::<()>::new();
        c.begin_frame();
        c.insert(
            ShapeKey {
                hash: 1,
                term_w: 80,
            },
            vec![run_of(1), run_of(1), run_of(1)],
        );
        c.insert(
            ShapeKey {
                hash: 2,
                term_w: 80,
            },
            vec![run_of(1), run_of(1)],
        );
        assert_eq!(
            bytes_estimate_of(&c, &[], &[]),
            5 * (BUFFER_FIXED_BYTES + GLYPH_EST_BYTES),
            "两行装了 5 个 Buffer,按行记就只算 2 —— 差的正是 F192 那一个数量级"
        );
    }

    fn run_of(glyphs: u32) -> crate::shaped_cache::CachedRun<()> {
        crate::shaped_cache::CachedRun {
            col: 0,
            payload: (),
            glyphs,
        }
    }

    /// F192:同样多的字形,摊在 200 个 Buffer 里比装在 1 个里贵 —— 但只贵
    /// **7.2 倍**,不是 200 倍。
    ///
    /// 这条钉的是两项模型的形状本身。单常数模型(F169 的 4096)会说 200 倍,
    /// 而实测是 33KB vs 239KB。差别不是精度问题,是**会把优化立项引到错误方向**:
    /// F195 的 run 合并按单常数算「省两个数量级」,按实测算「省 7.2 倍」——
    /// 后者仍然值得做,但不该拿前者去排优先级。
    ///
    /// 自证会变红:把 `bytes_estimate_of` 的 `glyphs × GLYPH_EST_BYTES` 那项
    /// 删掉(退回单常数),两边就变成整 200 倍。
    #[test]
    fn the_text_gauge_charges_for_glyphs_on_top_of_buffers() {
        use crate::shaped_cache::{ShapeKey, ShapedCache};
        const G: usize = 200;

        let mut merged = ShapedCache::<()>::new();
        merged.begin_frame();
        merged.insert(
            ShapeKey {
                hash: 1,
                term_w: 80,
            },
            vec![run_of(u32::try_from(G).unwrap())],
        );

        let mut split = ShapedCache::<()>::new();
        split.begin_frame();
        split.insert(
            ShapeKey {
                hash: 1,
                term_w: 80,
            },
            (0..G).map(|_| run_of(1)).collect(),
        );

        assert_eq!(
            bytes_estimate_of(&merged, &[], &[]),
            BUFFER_FIXED_BYTES + G * GLYPH_EST_BYTES,
            "合并成一个 run:一份固定价 + 200 份边际价"
        );
        assert_eq!(
            bytes_estimate_of(&split, &[], &[]),
            G * BUFFER_FIXED_BYTES + G * GLYPH_EST_BYTES,
            "切成 200 个 run:多的只是 199 份固定价,字形那部分一分不省"
        );
    }

    /// F192:池子里的 buffer 按**它自己的**字形数计价,不是按固定价。
    ///
    /// 池子恰恰是没有上限的那一个(F196 才给它加 cap),里面躺的多半是长
    /// ASCII 行退下来的 buffer,单个值 56KB。丢了字形数就按 1.8KB 计 ——
    /// 池子越大低报越狠,而且**完全静默**。
    ///
    /// 自证会变红:让 `bytes_estimate_of` 里 `pool`/`temp` 那两项只数 `len()`。
    #[test]
    fn the_text_gauge_prices_a_pooled_buffer_by_the_glyphs_it_holds() {
        use crate::shaped_cache::ShapedCache;
        let empty = ShapedCache::<()>::new();
        let fat: Vec<((), u32)> = vec![((), 200)];
        let thin: Vec<((), u32)> = vec![((), 1)];
        assert_eq!(
            bytes_estimate_of(&empty, &fat, &[]),
            BUFFER_FIXED_BYTES + 200 * GLYPH_EST_BYTES
        );
        assert_eq!(
            bytes_estimate_of(&empty, &[], &thin),
            BUFFER_FIXED_BYTES + GLYPH_EST_BYTES
        );
    }

    /// F192:两项定价模型的实测标定。**在长短两端各量一次。**
    ///
    /// 为什么必须两端都量:一开始只量了单字 run(2371 字节),照它标一个单常数
    /// 看着挺准 —— 直到把 run 拉长才发现 200 格的 ASCII 行值 55920 字节。
    /// 24 倍跨度,单常数在另一端必错一个数量级。两项模型是从这四个实测点
    /// (1→2371、20→7580、60→16900、200→55920)最小二乘拟出来的。
    ///
    /// **不能照抄 F190 那套私有计数器的手法** —— 那对分配器不可见,而这里要量
    /// 的正是「一个已整形 `Buffer` 实际吃掉多少堆」,只能读进程全局的
    /// `heapgauge::GLOBAL`。而 F190 自己记着的教训是:1600+ 条测试并行跑,
    /// 全局计数上的绝对增量测不准。
    ///
    /// 三条对策一起上,让这条既测得准又不 flaky:
    ///
    /// 1. **信号做大到噪声之上**:一次整形并**持有** N 个 buffer(`held` 必须
    ///    活到第二次读数之后,否则量到的是 0)。两端各自把 N 调到总量 ~25MB。
    /// 2. **多轮取中位数**:第一轮含 `FontSystem` 的一次性增长(字体数据、
    ///    shape cache),是必然的离群值,中位数把它削掉。
    /// 3. **只断言量级**(`[预测/4, 预测×4]`):精度要求是量级正确。要打红一个
    ///    4 倍带宽的中位数断言,邻居测试得在同一个窗口里**净**漂几十 MB。
    ///
    /// 两端合起来才钉得住两个常数:短端主要约束 [`BUFFER_FIXED_BYTES`],
    /// 长端主要约束 [`GLYPH_EST_BYTES`]。**少任何一端,另一个常数就自由了。**
    ///
    /// 平台漂是明知的:Linux 开发机的回退字体与 Windows 不同,实测值会差。
    /// 断言只钉量级正是为此 —— 常数漂出一个量级时逼人回来重标,日常波动不红。
    ///
    /// 自证会变红:把 `GLYPH_EST_BYTES` 改成 1(长端立刻红,短端仍绿 ——
    /// 这正是"少一端就钉不住"的现场)。
    #[test]
    fn the_shaped_buffer_price_model_matches_what_it_actually_costs() {
        // (一个 run 里的字形数, 持有多少个 run)。乘积按总量 ~25MB 选。
        const POINTS: [(usize, usize); 2] = [(1, 10_000), (200, 500)];
        let mut fs = FontSystem::new();
        let metrics = grid_metrics(16.0, 20.0);

        for (glyphs, n) in POINTS {
            let text = "x".repeat(glyphs);
            let mut rounds = [0usize; 3];
            for slot in &mut rounds {
                let before = crate::heapgauge::GLOBAL.live();
                let mut held: Vec<Buffer> = Vec::with_capacity(n);
                for _ in 0..n {
                    let mut b = Buffer::new(&mut fs, metrics);
                    b.set_text(
                        &mut fs,
                        &text,
                        Attrs::new().family(Family::Name(DEFAULT_FONT_FAMILY)),
                        Shaping::Advanced,
                    );
                    b.shape_until_scroll(&mut fs, false);
                    held.push(b);
                }
                let after = crate::heapgauge::GLOBAL.live();
                *slot = after.saturating_sub(before) as usize / n;
                drop(held);
            }
            rounds.sort_unstable();
            let measured = rounds[1];
            let predicted = BUFFER_FIXED_BYTES + glyphs * GLYPH_EST_BYTES;
            println!(
                "{glyphs} 字形/run:实测中位数 {measured} 字节,模型 {predicted}(三轮 {rounds:?})"
            );
            assert!(
                measured >= predicted / 4 && measured <= predicted * 4,
                "{glyphs} 字形的 run:模型报 {predicted}、实测 {measured},差了一个量级\
                 以上,该重新标定 BUFFER_FIXED_BYTES/GLYPH_EST_BYTES 了(三轮 {rounds:?})"
            );
        }
    }

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
                        for run in row_to_runs(row, None, &mut ascii_only) {
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
