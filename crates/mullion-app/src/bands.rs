//! F172:顶点层的行带差分 —— 「这一带的字**画出来**跟上一帧一样吗」。
//!
//! **零 GPU、零 IO**,可纯单测:吃的是行指纹、pane 几何与样式量,吐一个
//! `u64`。真正拿它去决定要不要重建顶点缓冲的胶水在 `text.rs`。
//!
//! # 它补的是哪一层
//!
//! ```text
//! F12  行指纹   →  这一行要不要重新 shape        (省 cosmic-text 的整形)
//! F172 带指纹   →  这一带要不要重新 prepare      (省 glyphon 的顶点重建)  ← 本模块
//! F159 整帧指纹 →  这一帧要不要提交 GPU          (省 present)
//! ```
//!
//! 实机日志量出来的缺口正在中间那层:`reshape=hit:329/miss:1` —— 330 行只
//! 变了 1 行,整形几乎全命中,`text_prepare` 却仍要 8~16ms。glyphon 0.7.0 的
//! `prepare` 开头就是 `glyph_vertices.clear()`,把传进去的**每个字形**重走一遍
//! LRU 查找 + `glyphs_in_use` 插入 + 顶点 push,再整块 `write_buffer`。省掉
//! 整形省不到这一笔,只有「压根不把这一带交给 `prepare`」才省得到。
//!
//! # 判在结果上,不判在原因上
//!
//! 与 F12/F159 同一条推理(见 [ADR-011](../../../docs/adr-011-row-fingerprint-vs-term-damage.md))。
//! 特别地,**不能拿「带内有行走了 `RowPlan::Reshape`」当判据** —— 那是原因侧,
//! 漏三类:
//!
//! - **pane 移动/改大小**:`text.rs` 的 `left`/`top`/`bounds` 是每帧现算的,
//!   注释明写「拖动分屏不需要重整形」。但顶点缓冲把绝对坐标**烤进去了**,
//!   拖一下分界线,旧顶点就画在移动前的位置上。
//! - **换主题**:`default_color` 变了,行内容一个字没动。
//! - **组字**:preedit 走临时 buffer,压根不进整形缓存。
//!
//! 三类的症状都是「某一块留着陈旧的字」,编译/测试/日志全静默。
//!
//! # 这个方案救不了什么(固有边界,不是待办)
//!
//! **滚动态零收益。** 顶点里烤着绝对 y,终端向上滚一行,每一行的 y 都变了
//! → 全部带脏 → 退化成全量,还多付一点带簿记。把整形缓存改成内容寻址能救
//! `reshape`(滚动时现在每帧全屏 miss),但救不了带。

use mullion_term::snapshot::Rgb;

use crate::shell::workspace::PxRect;

/// 一带多少行。
///
/// 权衡:带越窄,一行改动波及的行越少(心跳态 330 行只变 1 行时,16 行一带
/// 就是 5% 的成本);但带越窄,`TextRenderer` 越多 —— 每个带一个顶点缓冲、
/// 一次 draw call、一次 `prepare` 的固定开销。16 是起手值,实机 `bands=`
/// 与 `seg=` 两段回来后再调。
pub const BAND_ROWS: u16 = 16;

/// 这一行归第几带。
pub const fn band_of(row: u16) -> u16 {
    row / BAND_ROWS
}

/// 这个 pane 一共几带。0 行 → 0 带。
pub const fn band_count(rows: u16) -> u16 {
    rows.div_ceil(BAND_ROWS)
}

/// 算一带指纹要吃的全部输入。
///
/// **这个结构体就是「什么会让这一带看起来不一样」的清单**。加字段时编译器
/// 会拦住每一个构造点,强迫作者表态 —— 这正是本项目踩过三次的「列举式门控
/// 在加档时必然漏」的反面:让漏掉变成编译错误,而不是静默画错。
pub struct BandInput<'a> {
    /// 本带各行的行指纹,**按行序**。长度即本带实际行数(末带可能不满)。
    pub row_hashes: &'a [u64],
    /// 该 pane 的终端网格区。顶点烤着绝对坐标,pane 一动这一带就得重建。
    pub term_px: PxRect,
    /// 字体族/字号/DPI 的摘要,取自 `frame_fp::StyleKey`。
    pub style: u64,
    /// glyphon 的兜底文字色(换主题会变,而行内容一个字没动)。
    pub default_fg: Rgb,
    /// 该 pane 的网格列数。组字串的排布(`text::preedit_layout`)吃它 ——
    /// 虽然它在稳态下由 `term_px.w` 推得出来,但**推得出来不等于进了哈希**,
    /// 这个结构体的意义就是不做这种推理。
    pub cols: u16,
    /// 落在本带的 IME 组字串。**不落在本带时必须是空串** —— 组字行走临时
    /// buffer,压根不进整形缓存,行指纹看不见它。
    pub preedit: &'a str,
    /// 组字串画在哪一列。同一串拼音挪一列也是另一幅画面。
    pub preedit_col: u16,
    /// 组字串的文字色。与 [`Self::default_fg`] 是两个量(`prepare_panes` 收
    /// 两个独立参数),换主题时可以分别变。
    pub preedit_fg: Rgb,
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// 增量式 FNV-1a。与 `frame_fp`/`mullion_term::snapshot::hash_row` 同一套常数。
///
/// 这里没复用 `frame_fp::Fnv`(它是私有的)—— 复用要把它提成 `pub(crate)`,
/// 而本模块只需要 `u64`/`u32`/`bytes`/`rgb` 四个口子,自带一份更省事,也让
/// 本模块保持「零跨模块耦合的纯函数」。
struct Fnv(u64);

impl Fnv {
    const fn new() -> Self {
        Self(FNV_OFFSET)
    }
    fn byte(&mut self, b: u8) {
        self.0 ^= b as u64;
        self.0 = self.0.wrapping_mul(FNV_PRIME);
    }
    fn u64(&mut self, v: u64) {
        for b in v.to_le_bytes() {
            self.byte(b);
        }
    }
    fn u32(&mut self, v: u32) {
        self.u64(u64::from(v));
    }
    /// 先吃长度再吃内容 —— 不吃长度的话 `("ab","c")` 与 `("a","bc")` 同哈希。
    fn bytes(&mut self, s: &[u8]) {
        self.u64(s.len() as u64);
        for &b in s {
            self.byte(b);
        }
    }
    fn rgb(&mut self, c: Rgb) {
        let Rgb { r, g, b } = c;
        self.byte(r);
        self.byte(g);
        self.byte(b);
    }
}

/// 这一带的指纹。
///
/// **穷尽解构 `BandInput`**:给它加字段时这里当场编译报错,强迫作者对
/// 「进不进指纹」表态。漏一项的症状是那一带永久留着陈旧的字,而编译、测试、
/// 日志全静默 —— 与 `mullion_term::snapshot` 的行指纹同一手法。
pub fn fingerprint(i: &BandInput<'_>) -> u64 {
    let BandInput {
        row_hashes,
        term_px,
        style,
        default_fg,
        cols,
        preedit,
        preedit_col,
        preedit_fg,
    } = i;
    let mut h = Fnv::new();
    // 先吃行数:不吃的话「末带 3 行」与「末带 4 行、最后一行指纹恰好补上」
    // 会同哈希(缩窗口把行数改小是常见操作)。
    h.u64(row_hashes.len() as u64);
    for &r in *row_hashes {
        h.u64(r);
    }
    let PxRect { x, y, w, h: rh } = *term_px;
    h.u32(x);
    h.u32(y);
    h.u32(w);
    h.u32(rh);
    h.u64(*style);
    h.rgb(*default_fg);
    h.u32(u32::from(*cols));
    h.bytes(preedit.as_bytes());
    h.u32(u32::from(*preedit_col));
    h.rgb(*preedit_fg);
    h.0
}

/// 这一带本帧要不要重建顶点。
///
/// `prev` 为 `None`(这一带上一帧不存在:刚开的 pane、刚拉高的窗口)一律重建
/// —— 它的 `TextRenderer` 里要么没顶点,要么是别人留下的。
pub const fn is_dirty(prev: Option<u64>, now: u64, force_full: bool) -> bool {
    if force_full {
        return true;
    }
    match prev {
        Some(p) => p != now,
        None => true,
    }
}

/// 本帧可不可以 `atlas.trim()`。**唯一判据:所有带都重建了。**
///
/// glyphon 的图集淘汰(`text_atlas.rs` 的 `try_allocate`)只保护
/// `glyphs_in_use` 里的字形,而 `trim()` 把这张表整个清空、靠随后的
/// `prepare` 重新填。分带之后,没重新 prepare 的带的字形不在表里 →
/// 图集满时被踢掉、槽位让给新字形 → **那一带的旧顶点指向别的字形的图集
/// 坐标,画出别的字**。
///
/// 所以 trim 与全量重建必须绑死。平时不 trim,图集只涨不缩;换字体/字号/
/// 主题、窗口 resize、pane 几何变、切标签、滚动本来就会让全带脏,自然就
/// trim 了。真撞 `AtlasFull` 时靠「强制全量 + trim」自愈。
pub const fn may_trim(dirty: usize, total: usize) -> bool {
    dirty == total
}

/// 一帧里一条带的排期。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BandPlan {
    /// `(PaneId.0, 带号)`。`PaneId` 没派生 `Ord`,拆成裸 `u32` 才能进 `BTreeMap`。
    pub key: (u32, u16),
    pub fp: u64,
    pub dirty: bool,
}

/// 本帧每一带的指纹与去留。**纯函数** —— 不碰 wgpu,所以整条判据在无头环境
/// 里可测,而 `prepare_panes` 本身不行(要真实 Device/Queue)。
///
/// `prev` 回答「这一带上一次 prepare 成功后的指纹」,`None` = 没有可信顶点。
///
/// **带的排布顺序即 pane 的顺序**,每个 pane 内按带号升序 —— `text.rs` 靠
/// 这条把 `Placement` 分桶到下标 `pane 起始 + band_of(row)` 上,不建哈希表。
pub fn plan_bands(
    panes: &[crate::gpu::PaneRender<'_>],
    prev: &dyn Fn((u32, u16)) -> Option<u64>,
    style: u64,
    default_fg: Rgb,
    preedit_fg: Rgb,
    force_full: bool,
) -> Vec<BandPlan> {
    let mut out = Vec::new();
    for p in panes {
        let pane_id = p.geom.id.0;
        for b in 0..band_count(p.snap.rows) {
            let lo = b * BAND_ROWS;
            let hi = (lo + BAND_ROWS).min(p.snap.rows);
            let hashes: Vec<u64> = (lo..hi).map(|r| p.snap.row_hash(r)).collect();
            // 组字串只落在光标那一行所属的带上 —— `text.rs` 把 preedit 的每个
            // 临时 buffer 都摆在 `cursor.row`。别的带传空串:传全串的话打一个
            // 拼音就让整个 pane 的每一带都脏,收益归零。
            // 守卫与 `text::hidden_span_for_row` 内部判据同源(非空 + 光标可见)。
            let here = !p.preedit.is_empty()
                && p.snap.cursor.visible
                && (lo..hi).contains(&p.snap.cursor.row);
            let fp = fingerprint(&BandInput {
                row_hashes: &hashes,
                term_px: p.geom.term_px,
                style,
                default_fg,
                cols: p.snap.cols,
                preedit: if here { p.preedit } else { "" },
                preedit_col: if here { p.snap.cursor.col } else { 0 },
                preedit_fg,
            });
            let key = (pane_id, b);
            out.push(BandPlan {
                key,
                fp,
                dirty: is_dirty(prev(key), fp, force_full),
            });
        }
    }
    out
}

/// 一串行号里有几个连通段(相邻行号算同一段)。
///
/// 只作诊断用:回答「[`BAND_ROWS`] 选得对不对」。变化的行若连成一两段,
/// 粗带就够;若散得到处都是,粗带会退化成全量,那才需要考虑把差分做到行级
/// (代价是要 vendor glyphon 的顶点循环)。
///
/// **要求 `rows` 升序**。乱序时段数只会被高估,不会被低估 —— 保守方向:
/// 宁可报「局部性差」让人去查,不报「局部性好」让人放心。
pub fn segments(rows: &[u16]) -> u32 {
    let mut n = 0u32;
    let mut prev: Option<u16> = None;
    for &r in rows {
        if prev.is_none_or(|p| r != p && r != p.wrapping_add(1)) {
            n += 1;
        }
        prev = Some(r);
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: u32, y: u32, w: u32, h: u32) -> PxRect {
        PxRect { x, y, w, h }
    }

    fn base<'a>(rows: &'a [u64], preedit: &'a str) -> BandInput<'a> {
        BandInput {
            row_hashes: rows,
            term_px: rect(10, 20, 800, 600),
            style: 0xabcd,
            default_fg: Rgb { r: 1, g: 2, b: 3 },
            cols: 120,
            preedit,
            preedit_col: 7,
            preedit_fg: Rgb { r: 4, g: 5, b: 6 },
        }
    }

    /// 行归带的算术。末带不满是常态(行数几乎不会恰好是 16 的倍数)。
    #[test]
    fn rows_map_into_bands_and_the_last_band_may_be_short() {
        assert_eq!(band_of(0), 0);
        assert_eq!(band_of(BAND_ROWS - 1), 0);
        assert_eq!(band_of(BAND_ROWS), 1);
        assert_eq!(band_count(0), 0, "没有行就没有带");
        assert_eq!(band_count(1), 1);
        assert_eq!(band_count(BAND_ROWS), 1);
        assert_eq!(band_count(BAND_ROWS + 1), 2, "末带不满也要算一带");
    }

    /// **指纹的字段覆盖面**:每一项输入单独改动都必须改变指纹。
    ///
    /// 漏掉任何一项的症状都一样 —— 那一带永久留着陈旧的字,而编译、测试、
    /// 日志全静默,只有人眼能发现(且要拖分屏/换主题/打拼音才触发)。
    ///
    /// 自证会变红:把 `fingerprint` 里任意一句 `h.xxx(...)` 删掉。
    #[test]
    fn every_input_that_changes_the_picture_changes_the_fingerprint() {
        let rows = [1u64, 2, 3];
        let f0 = fingerprint(&base(&rows, "ni"));

        let other_rows = [1u64, 2, 4];
        assert_ne!(f0, fingerprint(&base(&other_rows, "ni")), "行内容");

        let short = [1u64, 2];
        assert_ne!(f0, fingerprint(&base(&short, "ni")), "行数");

        let mut i = base(&rows, "ni");
        i.term_px = rect(11, 20, 800, 600);
        assert_ne!(f0, fingerprint(&i), "pane 左移一像素(拖分屏)");

        let mut i = base(&rows, "ni");
        i.term_px = rect(10, 20, 801, 600);
        assert_ne!(f0, fingerprint(&i), "pane 变宽(拖分界线)");

        let mut i = base(&rows, "ni");
        i.style = 0xabce;
        assert_ne!(f0, fingerprint(&i), "换字体/字号/DPI");

        let mut i = base(&rows, "ni");
        i.default_fg = Rgb { r: 9, g: 2, b: 3 };
        assert_ne!(f0, fingerprint(&i), "换主题");

        let mut i = base(&rows, "ni");
        i.cols = 121;
        assert_ne!(f0, fingerprint(&i), "网格列数(组字串的排布吃它)");

        let mut i = base(&rows, "ni");
        i.preedit_fg = Rgb { r: 4, g: 5, b: 7 };
        assert_ne!(f0, fingerprint(&i), "组字串文字色");

        assert_ne!(f0, fingerprint(&base(&rows, "nihao")), "组字串");
        assert_ne!(f0, fingerprint(&base(&rows, "")), "组字结束");

        let mut i = base(&rows, "ni");
        i.preedit_col = 8;
        assert_ne!(f0, fingerprint(&i), "组字串挪了一列");
    }

    /// 同样的输入必须得到同样的指纹 —— 否则每一带每一帧都脏,改动纯亏。
    #[test]
    fn the_same_picture_gets_the_same_fingerprint() {
        let rows = [7u64, 8];
        assert_eq!(
            fingerprint(&base(&rows, "a")),
            fingerprint(&base(&rows, "a"))
        );
    }

    /// 行数要单独进哈希。不进的话「3 行」与「4 行且末行指纹恰好补齐」同哈希
    /// —— 缩窗口把行数改小是常见操作,那一帧会留着多出来的一行旧字。
    ///
    /// 自证会变红:把 `fingerprint` 里的 `h.u64(row_hashes.len() as u64)` 删掉。
    #[test]
    fn the_row_count_is_hashed_so_a_shrunken_band_is_not_mistaken_for_a_full_one() {
        // 构造出「短的那组 + 一个补齐值」与长的那组内容相同的情形。
        let long = [1u64, 2, 3];
        let short = [1u64, 2];
        assert_ne!(
            fingerprint(&base(&long, "")),
            fingerprint(&base(&short, "")),
            "行数没进哈希"
        );
    }

    /// 上一帧不存在的带(刚开的 pane、刚拉高的窗口)一律重建。
    ///
    /// 判成干净的话,那一带的 `TextRenderer` 要么空着(整带不显示),要么
    /// 留着上一个占用者的顶点(画出别的 pane 的内容)。
    ///
    /// 自证会变红:把 `is_dirty` 的 `None` 分支改成 `false`。
    #[test]
    fn a_band_with_no_previous_frame_is_always_rebuilt() {
        assert!(is_dirty(None, 42, false), "新带没有上一帧可比,必须重建");
        assert!(!is_dirty(Some(42), 42, false), "指纹相同该复用");
        assert!(is_dirty(Some(41), 42, false), "指纹不同该重建");
    }

    /// `force_full` 压过一切。它是 `AtlasFull` 之后的自愈路径:图集里的坐标
    /// 已经乱了,指纹相同也不能信。
    ///
    /// 自证会变红:把 `is_dirty` 开头那句 `if force_full` 删掉。
    #[test]
    fn a_forced_rebuild_overrides_a_matching_fingerprint() {
        assert!(is_dirty(Some(42), 42, true), "强制全量时指纹相同也要重建");
    }

    /// **trim 与全量重建必须绑死**。只要有一带没重建,就不许 trim ——
    /// 那一带的字形不在 `glyphs_in_use` 里,会被图集踢掉、槽位让给新字形,
    /// 于是那一带画出别的字。这是本切片唯一会静默画错的地方。
    ///
    /// 自证会变红:把 `may_trim` 改成 `dirty > 0` 或恒 `true`。
    #[test]
    fn trimming_the_atlas_is_only_allowed_when_every_band_was_rebuilt() {
        assert!(may_trim(4, 4), "全带重建了,可以 trim");
        assert!(!may_trim(3, 4), "还有一带没重建,不许 trim");
        assert!(!may_trim(0, 4), "一带都没重建,更不许 trim");
        assert!(may_trim(0, 0), "压根没有带时无所谓(launcher 态)");
    }

    /// 连通段数:相邻行算同一段。
    #[test]
    fn adjacent_rows_count_as_one_segment() {
        assert_eq!(segments(&[]), 0);
        assert_eq!(segments(&[5]), 1);
        assert_eq!(segments(&[5, 6, 7]), 1, "连续三行是一段");
        assert_eq!(segments(&[5, 7]), 2, "隔一行是两段");
        assert_eq!(segments(&[1, 2, 10, 11, 30]), 3);
    }

    /// 重复行号不新开一段 —— 同一行被记两次不该让局部性看起来更差。
    #[test]
    fn a_repeated_row_does_not_open_a_new_segment() {
        assert_eq!(segments(&[5, 5, 6]), 1);
    }

    // ---- `plan_bands`:整条判据的端到端(仍是纯函数,无 wgpu)----

    mod plan {
        use super::*;
        use crate::gpu::PaneRender;
        use crate::shell::workspace::PaneGeom;
        use mullion_core::layout::PaneId;
        use mullion_term::snapshot::{Cursor, CursorShape, GridSnapshot, SnapCell};

        const COLS: u16 = 8;
        /// 刻意跨两带:16 行一带,40 行 = 3 带(16+16+8)。带边界上的错误
        /// (`<=` 写成 `<`、末带漏掉)只有多带才暴露得出来。
        const ROWS: u16 = 40;

        fn cell(ch: char) -> SnapCell {
            SnapCell {
                ch,
                fg: Rgb {
                    r: 200,
                    g: 200,
                    b: 200,
                },
                bg: Rgb { r: 0, g: 0, b: 0 },
                width: 1,
                spacer: false,
                selected: false,
            }
        }

        /// 一屏 40×8 的 `.`,第 `mark_row` 行头一格换成 `X`。
        fn snap(mark_row: Option<u16>, cursor_row: u16) -> GridSnapshot {
            let mut cells: Vec<SnapCell> = (0..ROWS as usize * COLS as usize)
                .map(|_| cell('.'))
                .collect();
            if let Some(r) = mark_row {
                cells[r as usize * COLS as usize] = cell('X');
            }
            GridSnapshot::new(
                COLS,
                ROWS,
                cells,
                Cursor {
                    row: cursor_row,
                    col: 2,
                    visible: true,
                    shape: CursorShape::Beam,
                    blinking: true,
                },
            )
        }

        fn geom(x: u32) -> PaneGeom {
            let r = PxRect {
                x,
                y: 0,
                w: 800,
                h: 600,
            };
            PaneGeom {
                id: PaneId(7),
                px: r,
                title_px: PxRect {
                    x,
                    y: 0,
                    w: 800,
                    h: 0,
                },
                term_px: r,
                grid: (COLS, ROWS),
            }
        }

        const FG: Rgb = Rgb {
            r: 200,
            g: 200,
            b: 200,
        };
        const PRE_FG: Rgb = Rgb {
            r: 10,
            g: 20,
            b: 30,
        };

        fn plan(p: &[PaneRender<'_>], prev: &dyn Fn((u32, u16)) -> Option<u64>) -> Vec<BandPlan> {
            plan_bands(p, prev, 0xabcd, FG, PRE_FG, false)
        }

        fn render<'a>(s: &'a GridSnapshot, g: PaneGeom, preedit: &'a str) -> PaneRender<'a> {
            PaneRender {
                geom: g,
                snap: s,
                focused: true,
                preedit,
            }
        }

        /// 带的排布必须是「按 pane 顺序、每 pane 内按带号升序」——`text.rs`
        /// 靠这条把行摆进 `pane 起始下标 + band_of(row)` 的桶里,不建哈希表。
        /// 顺序错了,行会摆进别的带 → 那一带画出别的行的内容。
        #[test]
        fn bands_come_out_in_pane_order_then_band_order() {
            let s = snap(None, 0);
            let p = [render(&s, geom(0), "")];
            let got = plan(&p, &|_| None);
            assert_eq!(got.len(), 3, "40 行 = 3 带(16+16+8)");
            assert_eq!(
                got.iter().map(|b| b.key).collect::<Vec<_>>(),
                vec![(7, 0), (7, 1), (7, 2)]
            );
        }

        /// **本切片的全部收益都压在这一条上**:只改一行,只有那一行所在的带
        /// 该重建。它红了(或退化成全带脏)的话画面完全正确、日志一切正常、
        /// 性能悄悄回到改之前 —— 只有实机 `bands=` 的比值看得出来。
        ///
        /// 自证会变红:把 `plan_bands` 里的 `hashes` 改成整个 pane 的行指纹
        /// (而不是本带那一段)。
        #[test]
        fn touching_one_row_dirties_only_the_band_that_row_lives_in() {
            let before = snap(None, 0);
            let after = snap(Some(20), 0); // 第 20 行 → 第 1 带
            let g = geom(0);

            let base = plan(&[render(&before, g, "")], &|_| None);
            let prev: std::collections::BTreeMap<_, _> =
                base.iter().map(|b| (b.key, b.fp)).collect();

            let got = plan(&[render(&after, g, "")], &|k| prev.get(&k).copied());
            let dirty: Vec<u16> = got.iter().filter(|b| b.dirty).map(|b| b.key.1).collect();
            assert_eq!(dirty, vec![1], "只有第 20 行所在的第 1 带该重建");
        }

        /// 拖分屏分界线:pane 挪了位置,一行内容都没变。
        ///
        /// **这条是「判在结果上不判在原因上」的正身**:`text.rs` 的 `left`/`top`
        /// 每帧现算、注释明写「拖分屏不需要重整形」,所以整形缓存全命中、
        /// 一行都没走 `Reshape`。但顶点缓冲把绝对坐标烤进去了 —— 拿「有没有
        /// 行走了 Reshape」当判据,拖一下分界线就把旧顶点留在移动前的位置上。
        ///
        /// 自证会变红:把 `fingerprint` 里那四句 `h.u32(x/y/w/rh)` 删掉。
        #[test]
        fn moving_a_pane_dirties_every_band_even_though_no_row_changed() {
            let s = snap(None, 0);
            let base = plan(&[render(&s, geom(0), "")], &|_| None);
            let prev: std::collections::BTreeMap<_, _> =
                base.iter().map(|b| (b.key, b.fp)).collect();

            let got = plan(&[render(&s, geom(40), "")], &|k| prev.get(&k).copied());
            assert!(
                got.iter().all(|b| b.dirty),
                "pane 挪了位置,顶点里烤着旧坐标,全部带都得重建"
            );
        }

        /// 换主题:`default_color` 变了,行内容一个字没动 —— 同样是整形缓存
        /// 看不见、顶点里却烤着的东西。
        #[test]
        fn changing_the_theme_dirties_every_band() {
            let s = snap(None, 0);
            let p = [render(&s, geom(0), "")];
            let base = plan_bands(&p, &|_| None, 0xabcd, FG, PRE_FG, false);
            let prev: std::collections::BTreeMap<_, _> =
                base.iter().map(|b| (b.key, b.fp)).collect();

            let other = Rgb { r: 9, g: 9, b: 9 };
            let got = plan_bands(&p, &|k| prev.get(&k).copied(), 0xabcd, other, PRE_FG, false);
            assert!(got.iter().all(|b| b.dirty), "换了兜底文字色,全部带都得重建");
        }

        /// 组字:拼音串走临时 buffer、压根不进整形缓存,行指纹看不见它。
        ///
        /// 同时钉住**它只弄脏光标那一带**——把 preedit 无条件喂给每一带的话
        /// 打一个拼音就全屏重建,收益归零(而画面完全正确,没人会发现)。
        ///
        /// 自证会变红:把 `plan_bands` 里的 `if here { p.preedit } else { "" }`
        /// 改成恒 `p.preedit`(全带脏),或恒 `""`(打拼音不上屏)。
        #[test]
        fn a_preedit_dirties_only_the_cursor_band() {
            // 光标在第 20 行 → 第 1 带。
            let s = snap(None, 20);
            let g = geom(0);
            let base = plan(&[render(&s, g, "")], &|_| None);
            let prev: std::collections::BTreeMap<_, _> =
                base.iter().map(|b| (b.key, b.fp)).collect();

            let got = plan(&[render(&s, g, "ni")], &|k| prev.get(&k).copied());
            let dirty: Vec<u16> = got.iter().filter(|b| b.dirty).map(|b| b.key.1).collect();
            assert_eq!(dirty, vec![1], "组字只该弄脏光标所在的那一带");
        }

        /// 静止的一帧必须全带命中 —— 这是心跳态省下 8~16ms 的前提。
        /// 它红了说明指纹里混进了每帧都变的东西。
        #[test]
        fn a_still_frame_rebuilds_nothing() {
            let s = snap(None, 0);
            let p = [render(&s, geom(0), "")];
            let base = plan(&p, &|_| None);
            let prev: std::collections::BTreeMap<_, _> =
                base.iter().map(|b| (b.key, b.fp)).collect();
            let got = plan(&p, &|k| prev.get(&k).copied());
            assert!(got.iter().all(|b| !b.dirty), "画面没变却要重建顶点");
        }

        /// `force_full` 压过一切(`AtlasFull` 之后的自愈路径)。
        #[test]
        fn a_forced_frame_rebuilds_everything() {
            let s = snap(None, 0);
            let p = [render(&s, geom(0), "")];
            let base = plan(&p, &|_| None);
            let prev: std::collections::BTreeMap<_, _> =
                base.iter().map(|b| (b.key, b.fp)).collect();
            let got = plan_bands(&p, &|k| prev.get(&k).copied(), 0xabcd, FG, PRE_FG, true);
            assert!(got.iter().all(|b| b.dirty));
        }
    }
}
