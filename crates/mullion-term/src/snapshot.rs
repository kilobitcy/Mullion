//! 渲染快照:纯数据网格,供 app 渲染。零 UI 依赖(架构不变量:term 不依赖 app)。

/// 8-bit RGB。渲染层再转 glyphon/wgpu 颜色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

/// FNV-1a(64 位)的偏移基与质数。
///
/// 为什么手写而不是 `DefaultHasher`:`std` 的 `RandomState` 带随机种子,
/// 同一份内容在同一进程的两帧之间都可能算出不同的值,拿它做跨帧比对是
/// 直接坏掉的;`DefaultHasher::new()` 虽然确定,但标准库**明确不保证**
/// 跨版本稳定。FNV-1a 只有十行,零依赖,可直接单测。
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

#[inline]
fn eat(h: u64, b: u8) -> u64 {
    (h ^ u64::from(b)).wrapping_mul(FNV_PRIME)
}

/// 一行的内容指纹(F12)。**渲染层判"这一行要不要重新整形"的唯一判据。**
///
/// # 最关键的不变量
///
/// 喂进去的字段必须**恰好等于** `mullion-app` 的 `text::row_to_runs` /
/// `row_to_spans` 真正读到的字段。少喂一个,那一类变化就**静默**不重画 ——
/// 症状是屏幕上留着一行陈旧的字,编译不报错、测试不报错、日志不报错,
/// 只有人眼能发现。
///
/// 两层机械守护,缺一不可:
///
/// 1. **存量字段**:本模块 `tests` 里的六条 `a_changed_*_changes_the_row_hash`,
///    一条对一个字段,一条都不能省。
/// 2. **增量字段**:下面那句**穷尽解构**。给 `SnapCell` 加字段(比如
///    underline)时,这里会当场编译报错,强迫作者对"进不进哈希"表态,
///    而不是静默漏掉。**不要**把它改成 `cell.ch` 那种点号取字段的写法 ——
///    那样加字段就没有任何提示了。
///
/// SGR bold 不必单列:`Emulator::snapshot` 已经用 `palette::bold_brighten`
/// 把它烘进了 `fg`。
pub fn hash_row(cells: &[SnapCell]) -> u64 {
    let mut h = FNV_OFFSET;
    for cell in cells {
        // 穷尽解构 —— 见上面的文档,这一行是增量字段的唯一守护。
        let SnapCell {
            ch,
            fg,
            bg,
            width,
            spacer,
            selected,
        } = *cell;
        for b in (ch as u32).to_le_bytes() {
            h = eat(h, b);
        }
        for b in [
            fg.r,
            fg.g,
            fg.b,
            bg.r,
            bg.g,
            bg.b,
            width,
            u8::from(spacer),
            u8::from(selected),
        ] {
            h = eat(h, b);
        }
    }
    h
}

/// 单元格快照。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapCell {
    pub ch: char,
    pub fg: Rgb,
    pub bg: Rgb,
    /// 显示宽度:CJK 宽字符 = 2,其余 = 1(F16)。
    pub width: u8,
    /// 宽字符右半的占位格:渲染时跳过,不重复画。
    pub spacer: bool,
    /// 是否落在当前选区内(F18)。渲染层据此做反色:背景改用 fg 色、
    /// 文字改用 bg 色。宽字符右半的 spacer 与左半同步标记。
    pub selected: bool,
}

/// 光标形状(F125)。**本 crate 自己的枚举**,不把 `vte::ansi::CursorShape`
/// 漏进公开 API —— 架构不变量要求 `mullion-app` 只认识 `mullion-term` 的类型,
/// 而且 alacritty 将来加变体时,映射处会编译报错而不是被 `_ =>` 悄悄吞掉。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorShape {
    /// 实心块。
    Block,
    /// 下划线。
    Underline,
    /// 竖线。**本项目的默认**(见 `Emulator::with_history`)。
    #[default]
    Beam,
    /// 空心框。**当前不可达**:`Term::cursor_style()` 只吐 DECSCUSR/OSC 50
    /// 能表达的三种形状,这个值在 alacritty 里只走 vi-mode,而本项目不开 vi-mode。
    /// 留着是为了让 `map_shape` 能穷尽匹配。(渲染层"非焦点 pane 画空心框"
    /// 是 `gpu.rs` 自己的另一套判断,不读这个字段。)
    HollowBlock,
    /// 远端要求不画光标。**当前不可达**:DECTCEM 走的是 `Cursor::visible`,
    /// `cursor_style()` 不看它。同样只为穷尽匹配而留。
    Hidden,
}

/// 光标快照。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub row: u16,
    pub col: u16,
    pub visible: bool,
    /// F125:远端 DECSCUSR 要求的形状,没要求过就是 `Beam`(本项目默认)。
    pub shape: CursorShape,
    /// F125:远端要求闪不闪,没要求过就是 `true`。
    pub blinking: bool,
}

/// 一帧网格快照:行优先,`cells.len() == cols * rows`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridSnapshot {
    pub cols: u16,
    pub rows: u16,
    pub cells: Vec<SnapCell>,
    pub cursor: Cursor,
    /// 每行的内容指纹(F12),长度 == `rows`。
    ///
    /// **私有是有意的**:它必须与 `cells` 严格同步,而字段公开就意味着
    /// crate 外可以手搓一个 `GridSnapshot { .. }` 却把指纹填成 `vec![0; n]`
    /// —— 那样渲染层会认为"这一行永远没变",屏幕永久停在第一帧。
    /// 私有之后唯一的构造入口是 [`GridSnapshot::new`],编译器保证同步。
    row_hash: Vec<u64>,
}

impl GridSnapshot {
    /// **唯一的构造入口**,顺手把每行指纹算好(F12)。
    ///
    /// `cells.len()` 应当等于 `cols × rows`;不足的行按 0 补指纹而不是
    /// panic —— 快照是渲染路径上的东西,宁可这一帧多整形一次,也不能崩。
    ///
    /// **参数顺序**:`cols`、`rows` 都是 `u16` 且相邻,传反了编译器拦不住,
    /// 运行期只会让大部分行的指纹静默退化成 0(当 cols/rows 不等时,
    /// `r * w` 的步长跟真实布局对不上,多数切片会越界)。
    ///
    /// **0 这个哨兵值语义不对称**:越界返回 0,但真实内容也有 1/2^64 的
    /// 概率恰好算出 0。下游做跨帧缓存时,"这行从未渲染过"的标记**不要**
    /// 用裸的 `0u64`(`Vec<u64>` 默认值、`or_insert(0)` 这类写法最自然但
    /// 会踩这个坑)—— 要用 `Option<u64>` 或专门的哨兵区分"没渲染过"和
    /// "渲染过、恰好是 0"。
    pub fn new(cols: u16, rows: u16, cells: Vec<SnapCell>, cursor: Cursor) -> Self {
        let w = cols as usize;
        let row_hash = (0..rows as usize)
            .map(|r| {
                let start = r * w;
                cells.get(start..start + w).map_or(0, hash_row)
            })
            .collect();
        Self {
            cols,
            rows,
            cells,
            cursor,
            row_hash,
        }
    }

    /// 第 `row` 行的单元格切片(长度 == cols)。
    pub fn row(&self, row: u16) -> &[SnapCell] {
        let start = row as usize * self.cols as usize;
        &self.cells[start..start + self.cols as usize]
    }

    /// 第 `row` 行的内容指纹(F12)。越界返回 0。
    ///
    /// 0 是"未知"而不是"某个具体内容":调用方拿它跟缓存里的值比,
    /// 比不上就重新整形 —— 越界那一帧多整形一次,而不是漏画。
    pub fn row_hash(&self, row: u16) -> u64 {
        self.row_hash.get(row as usize).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个基准格。各测试只动其中一个字段,用来证明"那个字段进了哈希"。
    fn base() -> SnapCell {
        SnapCell {
            ch: 'a',
            fg: Rgb::new(0xcc, 0xcc, 0xcc),
            bg: Rgb::new(0x10, 0x10, 0x10),
            width: 1,
            spacer: false,
            selected: false,
        }
    }

    /// 改一个字段,断言整行指纹跟着变。
    fn changing(f: impl FnOnce(&mut SnapCell)) -> (u64, u64) {
        let before = [base(), base()];
        let mut after = before;
        f(&mut after[1]);
        (hash_row(&before), hash_row(&after))
    }

    /// 同样的一行,算两次必须一样。**没有这条,下面六条全是恒绿的**
    /// —— 一个每次返回随机数的 `hash_row` 能让"变了就不等"全部通过。
    ///
    /// 自证会变红:让 `hash_row` 的 `h` 初值改成随每次调用变化的东西。
    #[test]
    fn the_same_row_hashes_the_same_every_time() {
        let row = [base(), base()];
        assert_eq!(hash_row(&row), hash_row(&row));
    }

    /// 内容变(SGR 之外最常见的一种)。
    ///
    /// 自证会变红:从 `hash_row` 里删掉喂 `ch` 的那几行。
    #[test]
    fn a_changed_char_changes_the_row_hash() {
        let (a, b) = changing(|c| c.ch = 'b');
        assert_ne!(a, b, "改了字符,指纹没变 —— 屏幕会留着旧字");
    }

    /// SGR 前景色 / 主题换色 / bold 提亮,最终都落在 `fg` 上。
    ///
    /// 自证会变红:从 `hash_row` 里删掉 `fg.r`/`fg.g`/`fg.b`。
    #[test]
    fn a_changed_fg_changes_the_row_hash() {
        let (a, b) = changing(|c| c.fg = Rgb::new(0xff, 0x00, 0x00));
        assert_ne!(a, b, "改了前景色,指纹没变");
    }

    /// 背景色。选区反色会把它读成文字色(`text.rs::row_to_spans`),
    /// 所以它同样影响整形结果,不是"只影响 quad 层"。
    ///
    /// 自证会变红:从 `hash_row` 里删掉 `bg.r`/`bg.g`/`bg.b`。
    #[test]
    fn a_changed_bg_changes_the_row_hash() {
        let (a, b) = changing(|c| c.bg = Rgb::new(0x00, 0xff, 0x00));
        assert_ne!(a, b, "改了背景色,指纹没变 —— 选区反色会用陈旧字色");
    }

    /// 宽度决定 `row_to_runs` 怎么切 run(F16)。
    ///
    /// 自证会变红:从 `hash_row` 里删掉 `width`。
    #[test]
    fn a_changed_width_changes_the_row_hash() {
        let (a, b) = changing(|c| c.width = 2);
        assert_ne!(a, b, "改了显示宽度,指纹没变");
    }

    /// spacer 决定这一格跳不跳过。
    ///
    /// 自证会变红:从 `hash_row` 里删掉 `spacer`。
    #[test]
    fn a_changed_spacer_changes_the_row_hash() {
        let (a, b) = changing(|c| c.spacer = true);
        assert_ne!(a, b, "改了 spacer 标记,指纹没变");
    }

    /// F18 选区。alacritty 的 `Term::damage()` **不含**选区变化,
    /// 这正是本设计不用 damage 的头号理由 —— 指纹必须自己覆盖它。
    ///
    /// 自证会变红:从 `hash_row` 里删掉 `selected`。
    #[test]
    fn a_changed_selection_changes_the_row_hash() {
        let (a, b) = changing(|c| c.selected = true);
        assert_ne!(a, b, "改了选中标记,指纹没变 —— 划选后文字不反色");
    }

    /// 行变长/变短必须换指纹。定长逐格喂字节天然覆盖,写下来是防止
    /// 日后有人"优化"成只喂非空白格。
    ///
    /// 自证会变红:在 `hash_row` 开头加 `let cells = &cells[..1.min(cells.len())];`。
    #[test]
    fn a_longer_row_hashes_differently() {
        assert_ne!(hash_row(&[base()]), hash_row(&[base(), base()]));
    }

    fn cursor_at_origin() -> Cursor {
        Cursor {
            row: 0,
            col: 0,
            visible: false,
            shape: CursorShape::Beam,
            blinking: false,
        }
    }

    /// `new()` 给每一行都算好指纹,长度 == rows。
    ///
    /// 自证会变红:让 `GridSnapshot::new` 的 `row_hash` 收成 `Vec::new()`。
    #[test]
    fn new_fills_one_hash_per_row() {
        let s = GridSnapshot::new(3, 2, vec![base(); 6], cursor_at_origin());
        assert_eq!(s.row_hash(0), hash_row(s.row(0)));
        assert_eq!(s.row_hash(1), hash_row(s.row(1)));
    }

    /// **F12 的验收标准**(`spec.md`:"只改一行后,脏行集合只含那一行")。
    ///
    /// 自证会变红:让 `GridSnapshot::new` 把每一行的指纹都算成
    /// `hash_row(&cells)`(整份 cells 而不是本行切片)—— 那样改一行会让
    /// 所有行的指纹一起变,差分就退化成全量。
    #[test]
    fn changing_one_row_moves_only_that_rows_hash() {
        let before = GridSnapshot::new(3, 3, vec![base(); 9], cursor_at_origin());
        let mut cells = vec![base(); 9];
        cells[3 + 1].ch = 'Z'; // 第 1 行、第 1 列
        let after = GridSnapshot::new(3, 3, cells, cursor_at_origin());

        assert_eq!(before.row_hash(0), after.row_hash(0), "第 0 行不该变");
        assert_ne!(before.row_hash(1), after.row_hash(1), "第 1 行该变");
        assert_eq!(before.row_hash(2), after.row_hash(2), "第 2 行不该变");
    }

    /// 越界行号返回 0 而不是 panic。渲染层拿到的 `rows` 与快照的 `rows`
    /// 在 resize 那一帧可能短暂不一致,不能让它把进程带走。
    ///
    /// 自证会变红:把 `row_hash()` 的实现改成 `self.row_hash[row as usize]`。
    #[test]
    fn an_out_of_range_row_hash_is_zero_not_a_panic() {
        let s = GridSnapshot::new(3, 2, vec![base(); 6], cursor_at_origin());
        assert_eq!(s.row_hash(9), 0);
    }

    /// `new()` 内部按 `cols*rows` 逐行切片时,`cells` 给少了该怎么办 —— 这是
    /// `.map_or(0, hash_row)` 那个 `None` 分支自己的测试,跟上面那条
    /// `an_out_of_range_row_hash_is_zero_not_a_panic` 不是一回事:那条测的是
    /// `row_hash()` 方法对外层 `Vec<u64>` 的越界访问,`cells.len()` 恰好等于
    /// `cols*rows`,根本不会走到这里的短切片分支。
    ///
    /// 传 `cols=3, rows=5` 但只给 6 个格子(刚好够 2 行):前两行能正常切出
    /// 完整切片,指纹应等于对同一段切片直接调用 `hash_row`;第 2、3、4 行
    /// 切片会越界,指纹按文档承诺补 0,而不是 panic。
    ///
    /// 自证会变红:把 `new()` 里的 `.map_or(0, hash_row)` 改成
    /// `.map(hash_row).unwrap_or_else(|| hash_row(&[]))`
    /// (短行不再是 0,而是空切片的 FNV 初值)。
    #[test]
    fn cells_shorter_than_cols_times_rows_hashes_short_rows_as_zero_without_panicking() {
        let cells = vec![base(); 6]; // 够 2 行(cols=3),不够 5 行
        let s = GridSnapshot::new(3, 5, cells.clone(), cursor_at_origin());

        assert_eq!(
            s.row_hash(0),
            hash_row(&cells[0..3]),
            "第 0 行数据完整,指纹应等于直接对该切片调用 hash_row 的结果"
        );
        assert_eq!(
            s.row_hash(1),
            hash_row(&cells[3..6]),
            "第 1 行数据完整,指纹应等于直接对该切片调用 hash_row 的结果"
        );
        assert_eq!(s.row_hash(2), 0, "第 2 行已超出 cells 长度,指纹应补 0");
        assert_eq!(s.row_hash(3), 0, "第 3 行已超出 cells 长度,指纹应补 0");
        assert_eq!(s.row_hash(4), 0, "第 4 行已超出 cells 长度,指纹应补 0");
    }
}
