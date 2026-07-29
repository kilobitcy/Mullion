//! VT 仿真封装:把 `alacritty_terminal::Term` 藏在后面,只暴露「喂字节 / 取回写」,
//! 以及取渲染快照(F10/F16)、resize(F34)。
//!
//! 守护陷阱 T1(F11 前置):Term 通过 `EventListener` 发出的 `Event::PtyWrite`
//! 必须被收集并回写 SSH channel。漏了会导致同步输出探测(`CSI ? 2026 $ p`)无应答
//! → 全屏 TUI 退回逐帧刷新而闪、鼠标上报全废、光标位置查询永久卡死。
//! 本模块把这些字节攒进出站缓冲,由上层取走回写。

use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use vte::ansi::Processor;

use crate::palette;
use crate::selection::{CellSide, SelectionKind};
use crate::snapshot::{Cursor, GridSnapshot, Rgb, SnapCell};

/// 收集 `Event::PtyWrite`——需要回写对端的字节。共享缓冲,Term 持一份克隆。
#[derive(Clone, Default)]
struct PtyWriteCollector {
    buf: Arc<Mutex<Vec<u8>>>,
}

impl EventListener for PtyWriteCollector {
    fn send_event(&self, event: Event) {
        // 只关心需要回写对端的字节;其余事件(标题、响铃等)骨架阶段先忽略。
        if let Event::PtyWrite(text) = event {
            self.buf
                .lock()
                .expect("pty-write buffer poisoned")
                .extend_from_slice(text.as_bytes());
        }
    }
}

/// 仿真器网格尺寸。自己实现 `Dimensions`(alacritty 对 `(usize, usize)` 的实现是
/// `#[cfg(test)]` 私用)。这里只描述**屏面**尺寸;scrollback 深度由
/// `Config::scrolling_history` 决定,与本结构无关(`Term::new` 只读 columns/screen_lines)。
struct GridSize {
    cols: u16,
    rows: u16,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows as usize
    }
    fn screen_lines(&self) -> usize {
        self.rows as usize
    }
    fn columns(&self) -> usize {
        self.cols as usize
    }
}

/// 单个 pane 的 VT 仿真器:喂入字节 → 推进网格状态 + 攒出站回写。
pub struct Emulator {
    term: Term<PtyWriteCollector>,
    parser: Processor,
    collector: PtyWriteCollector,
    /// F80:可注入的默认前景/背景色。默认为出厂值,app 层挂主题后覆盖。
    defaults: palette::DefaultColors,
}

impl Emulator {
    /// 默认 scrollback 深度(行)。F17:约 10k 行,和 alacritty 默认一致。
    /// 这是**上限**约 80 列 × 10k 行(按需增长,短会话实际占用远小于此)。
    pub const DEFAULT_HISTORY: usize = 10_000;

    /// 新建 `cols × rows` 的仿真器,scrollback 用 [`Emulator::DEFAULT_HISTORY`]。
    pub fn new(cols: u16, rows: u16) -> Self {
        Self::with_history(cols, rows, Self::DEFAULT_HISTORY)
    }

    /// 新建仿真器并指定 scrollback 深度(F17 可配置;测试里也用它造浅历史)。
    ///
    /// 注意:`history` 只对 primary grid 生效。alt screen 的 grid 恒 0 行历史,
    /// alacritty 不给改(见 `alt_screen_has_no_scrollback`)。
    ///
    /// `history` 是**上限**而非预分配——alacritty 的 scrollback 存储按需增长
    /// (`Storage` 初始只为可视行分配,历史行滚动到时才 `initialize`),传入 0 或
    /// 极大值都不会立即 panic/OOM。但这里**不做合理性校验**,调用方(接配置文件时)
    /// 自己保证传入值合理。
    pub fn with_history(cols: u16, rows: u16, history: usize) -> Self {
        let collector = PtyWriteCollector::default();
        let dims = GridSize { cols, rows };
        let config = Config {
            scrolling_history: history,
            ..Config::default()
        };
        let term = Term::new(config, &dims, collector.clone());
        Self {
            term,
            parser: Processor::new(),
            collector,
            defaults: palette::DefaultColors::default(),
        }
    }

    /// 喂入一段来自对端的字节,推进 VT 状态机(不节流,VT 状态机很快)。
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    /// 取走并清空出站缓冲——这些字节必须回写 SSH channel(T1 红线)。
    pub fn take_pty_writes(&mut self) -> Vec<u8> {
        std::mem::take(
            &mut *self
                .collector
                .buf
                .lock()
                .expect("pty-write buffer poisoned"),
        )
    }

    /// 注入默认前景/背景色(F80 主题)。影响所有「未显式指定颜色」的格子,
    /// 即 SGR 39/49 语义下的默认色。OSC 10/11 运行时覆盖仍优先于此。
    pub fn set_default_colors(&mut self, fg: Rgb, bg: Rgb) {
        self.defaults = palette::DefaultColors { fg, bg };
    }

    /// 当前**可视区**的渲染快照(F10/F16/F17)。颜色解析成具体 RGB,宽字符标 width=2。
    ///
    /// F17 陷阱:`grid[Line(i)]` 的行号**不含** `display_offset`,回溯时必须自己减掉,
    /// 否则数据滚了画面不动。光标滚出可视区时置 `visible=false`——下游 `quads_for`
    /// 只在 `visible` 时画光标,否则光标会被钉在边缘行。
    pub fn snapshot(&self) -> GridSnapshot {
        let grid = self.term.grid();
        let colors = self.term.colors();
        let cols = grid.columns();
        let rows = grid.screen_lines();
        let offset = grid.display_offset() as i32;
        // 选区范围与下面的行号换算同源(都基于 display_offset),否则回溯时
        // 高亮会停在原来的屏幕位置上。
        let sel = self
            .term
            .selection
            .as_ref()
            .and_then(|s| s.to_range(&self.term));
        let mut cells = Vec::with_capacity(cols * rows);
        for line in 0..rows {
            let buf_line = Line(line as i32 - offset);
            let row = &grid[buf_line];
            for col in 0..cols {
                let cell = &row[Column(col)];
                let flags = cell.flags;
                let spacer = flags.contains(Flags::WIDE_CHAR_SPACER);
                let leading = flags.contains(Flags::WIDE_CHAR);
                // 宽字的两半在选区范围里只会命中一格(范围按格号算),另一半要跟随,
                // 否则中文选中只有半个字有底色。两个方向都要:从左半开始选时
                // 右半 spacer 跟随左半;从右半开始选时左半跟随 spacer。
                // `col + 1` 不会越界:alacritty 的 input() 写宽字符前会先检查
                // `column + 1 >= columns`,放不下就整体换行,WIDE_CHAR 左半
                // 永远不会落在最后一列(见 term/mod.rs input(),已读源码确认)。
                let selected = sel.is_some_and(|r| {
                    r.contains(Point::new(buf_line, Column(col)))
                        || (spacer && col > 0 && r.contains(Point::new(buf_line, Column(col - 1))))
                        || (leading && r.contains(Point::new(buf_line, Column(col + 1))))
                });
                cells.push(SnapCell {
                    ch: cell.c,
                    fg: palette::resolve(cell.fg, colors, self.defaults),
                    bg: palette::resolve(cell.bg, colors, self.defaults),
                    width: if flags.contains(Flags::WIDE_CHAR) {
                        2
                    } else {
                        1
                    },
                    // 只有 WIDE_CHAR_SPACER(宽字符右半)才跳过渲染;LEADING_WIDE_CHAR_SPACER
                    // 是行尾放不下整体换行时插入的独立空白占位格,左边不是宽字,
                    // 当普通空格处理——标成 spacer 会让下游静默漏画这一列的背景。
                    spacer,
                    selected,
                });
            }
        }
        let p = grid.cursor.point;
        // 光标行是「相对屏面」的,加上 offset 才是「相对可视区」。
        let cursor_row = p.line.0 + offset;
        GridSnapshot {
            cols: cols as u16,
            rows: rows as u16,
            cells,
            cursor: Cursor {
                row: cursor_row.max(0) as u16,
                col: p.column.0 as u16,
                // MVP 未接 DECTCEM(`\x1b[?25l`/`\x1b[?25h`)光标隐藏/显示;
                // 这里只处理「滚出可视区」这一种不可见(F17)。
                visible: cursor_row >= 0 && (cursor_row as usize) < rows,
            },
        }
    }

    /// 改变网格尺寸(F34:分屏 reflow / 窗口 resize 时调用)。
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.term.resize(GridSize { cols, rows });
    }

    /// 滚动可视区(F17 回溯)。`Scroll::Delta(正数)` = 往历史方向(向上)。
    ///
    /// alt screen 下 alacritty 内部**静默无效**(alt grid 的 history 恒 0),
    /// 别指望这里报错——分流必须在上层用 [`Emulator::mode`] 先判(见 `keymap::wheel_action`)。
    pub fn scroll(&mut self, scroll: Scroll) {
        self.term.scroll_display(scroll);
    }

    /// 跳回最新输出。用户一按普通键就该回到底部,否则「打字了但看不到」。
    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    /// 当前终端模式位(F17:滚轮分流要看 ALT_SCREEN / MOUSE_MODE / ALTERNATE_SCROLL)。
    pub fn mode(&self) -> TermMode {
        *self.term.mode()
    }

    /// 0-based viewport 单元格 → alacritty 的 buffer `Point`。
    ///
    /// **F18 头号坑**:alacritty 的 `Line` 带符号,`0` = 当前视口顶行、负数是历史。
    /// 回溯之后同一个屏幕位置对应的 buffer 行会变,不减 `display_offset`
    /// 选出来的就是另一段文本(见 `scrolled_selection_keeps_pointing_at_same_text`)。
    ///
    /// 列/行都夹紧在网格内:这是双重防御——即便这里不夹紧,alacritty 的
    /// `to_range` 也会 `grid_clamp`;预先夹紧是为了让越界坐标在本模块内的
    /// 行为可预测。
    fn point_at(&self, col: u16, row: u16) -> Point {
        let grid = self.term.grid();
        let offset = grid.display_offset() as i32;
        let max_col = grid.columns().saturating_sub(1);
        let max_row = grid.screen_lines().saturating_sub(1);
        let row = (row as usize).min(max_row) as i32;
        Point::new(Line(row - offset), Column((col as usize).min(max_col)))
    }

    /// 开始一段选区(左键按下)。`kind` 由 app 侧的连击判定给出。
    pub fn selection_start(&mut self, col: u16, row: u16, kind: SelectionKind, side: CellSide) {
        let point = self.point_at(col, row);
        let ty = match kind {
            SelectionKind::Simple => SelectionType::Simple,
            SelectionKind::Semantic => SelectionType::Semantic,
            SelectionKind::Lines => SelectionType::Lines,
        };
        self.term.selection = Some(Selection::new(ty, point, side_of(side)));
    }

    /// 更新选区终点(拖拽中)。没有活跃选区时静默忽略。
    pub fn selection_update(&mut self, col: u16, row: u16, side: CellSide) {
        let point = self.point_at(col, row);
        if let Some(sel) = self.term.selection.as_mut() {
            sel.update(point, side_of(side));
        }
    }

    /// 清除选区(点空白、按键、断开连接时调)。
    pub fn selection_clear(&mut self) {
        self.term.selection = None;
    }

    /// 当前选区文本。宽字符、行尾空格裁剪、跨 scrollback 拼接都由上游
    /// `selection_to_string` 负责,不重造。
    ///
    /// 空选区返回 `None` 而非 `Some("")`:上层据此判断要不要写剪贴板,
    /// 空串会把用户剪贴板里原有的内容清掉。
    ///
    /// 注意这同时把「没有选区」和「选中的是一段纯空白(上游返回 `Some("")`)」
    /// 折叠成了同一个 `None`。这是有意的:上层只用它判断要不要写剪贴板,
    /// 选一段空白不值得覆盖用户剪贴板里的内容。
    pub fn selection_text(&self) -> Option<String> {
        self.term.selection_to_string().filter(|s| !s.is_empty())
    }
}

/// 我们的 `CellSide` → alacritty 的 `Side`(= `Direction`)。
fn side_of(side: CellSide) -> Side {
    match side {
        CellSide::Left => Side::Left,
        CellSide::Right => Side::Right,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::Rgb;
    use alacritty_terminal::grid::Scroll;

    #[test]
    fn pty_write_is_collected() {
        // T1:喂入光标位置查询(DSR 6 / CPR),Term 应通过 PtyWrite 回一个应答。
        let mut emu = Emulator::new(80, 24);
        assert!(emu.take_pty_writes().is_empty(), "初始不应有回写");

        emu.feed(b"\x1b[6n"); // ESC [ 6 n —— 请求光标位置报告

        let out = emu.take_pty_writes();
        assert!(
            !out.is_empty(),
            "PtyWrite 未被收集——同步输出探测会无应答→闪(T1)"
        );
        // CPR 应答形如 ESC [ row ; col R;新仿真器光标在左上角 → \x1b[1;1R。
        assert_eq!(out, b"\x1b[1;1R");
        // 取走后缓冲应清空。
        assert!(emu.take_pty_writes().is_empty(), "take 之后应清空");
    }

    #[test]
    fn snapshot_has_chars_and_dims() {
        let mut emu = Emulator::new(10, 3);
        emu.feed(b"Hi");
        let snap = emu.snapshot();
        assert_eq!((snap.cols, snap.rows), (10, 3));
        assert_eq!(snap.row(0)[0].ch, 'H');
        assert_eq!(snap.row(0)[1].ch, 'i');
    }

    #[test]
    fn snapshot_sgr_red_sets_fg() {
        let mut emu = Emulator::new(4, 1);
        emu.feed(b"\x1b[31mR");
        let snap = emu.snapshot();
        assert_eq!(
            snap.row(0)[0].fg,
            Rgb::new(205, 0, 0),
            "SGR 31 应解析成我们表里的红"
        );
    }

    #[test]
    fn snapshot_cjk_is_double_width_with_spacer() {
        // F16:CJK 宽字符占两格,右半是 spacer,渲染跳过。
        let mut emu = Emulator::new(6, 1);
        emu.feed("中".as_bytes());
        let snap = emu.snapshot();
        assert_eq!(snap.row(0)[0].ch, '中');
        assert_eq!(snap.row(0)[0].width, 2, "宽字符必须占两格(F16)");
        assert!(snap.row(0)[1].spacer, "宽字符右侧必须是 spacer");
    }

    #[test]
    fn snapshot_cjk_line_wrap_leading_spacer_is_not_spacer() {
        // F16:3 列窄网格写 "ab" 后写「中」,放不下就整体换到下一行。
        // 行尾(row0 col2)留下的是 LEADING_WIDE_CHAR_SPACER——独立空白占位格,
        // 左边不是宽字,不该标 spacer,否则下游会静默跳过这一列的背景绘制。
        let mut emu = Emulator::new(3, 2);
        emu.feed(b"ab");
        emu.feed("中".as_bytes());
        let snap = emu.snapshot();

        let leading = &snap.row(0)[2];
        assert_eq!(leading.ch, ' ');
        assert_eq!(leading.width, 1);
        assert!(
            !leading.spacer,
            "行尾 LEADING 占位格不是「宽字符右半」,不该标 spacer,否则背景漏画"
        );

        assert_eq!(snap.row(1)[0].ch, '中', "宽字符放不下应换到下一行行首");
        assert_eq!(snap.row(1)[0].width, 2);
        assert!(!snap.row(1)[0].spacer);
        assert!(
            snap.row(1)[1].spacer,
            "宽字符右半的真 spacer(WIDE_CHAR_SPACER)仍要标 true,渲染要跳过"
        );
    }

    #[test]
    fn resize_changes_dims() {
        let mut emu = Emulator::new(10, 3);
        emu.resize(20, 5);
        let snap = emu.snapshot();
        assert_eq!((snap.cols, snap.rows), (20, 5));
    }

    #[test]
    fn cursor_starts_top_left() {
        let emu = Emulator::new(8, 4);
        let snap = emu.snapshot();
        assert_eq!((snap.cursor.row, snap.cursor.col), (0, 0));
        assert!(snap.cursor.visible);
    }

    /// 取快照某一行的可见文本(去掉行尾填充空格),断言可读性好很多。
    fn row_text(snap: &crate::snapshot::GridSnapshot, row: u16) -> String {
        snap.row(row)
            .iter()
            .map(|c| c.ch)
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn snapshot_follows_display_offset() {
        // F17:2 行屏面喂 3 行 → "one" 被推进 scrollback,可视区是 two/three。
        // 回溯一行后首行必须变成 "one"——不按 display_offset 偏移的话
        // 「数据滚了、画面不动」,滚轮看起来完全没反应。
        let mut emu = Emulator::new(10, 2);
        emu.feed(b"one\r\ntwo\r\nthree");
        assert_eq!(row_text(&emu.snapshot(), 0), "two");

        emu.scroll(Scroll::Delta(1));
        assert_eq!(row_text(&emu.snapshot(), 0), "one");
        assert_eq!(row_text(&emu.snapshot(), 1), "two");

        emu.scroll_to_bottom();
        assert_eq!(
            row_text(&emu.snapshot(), 0),
            "two",
            "回底后应重新贴最新输出"
        );
    }

    #[test]
    fn cursor_hidden_when_scrolled_out_of_viewport() {
        // F17:光标随内容滚出可视区后必须停画,否则会被钉在边缘行上闪。
        let mut emu = Emulator::new(10, 2);
        emu.feed(b"one\r\ntwo\r\nthree");
        assert!(emu.snapshot().cursor.visible);

        emu.scroll(Scroll::Delta(1));
        assert!(
            !emu.snapshot().cursor.visible,
            "光标已滚出可视区,仍 visible 会画在错误的行上"
        );

        emu.scroll_to_bottom();
        assert!(emu.snapshot().cursor.visible, "回底后光标应恢复");
    }

    #[test]
    fn scrollback_holds_configured_lines() {
        // F17:history=3 时最多回溯 3 行。喂 10 行(每行带 \r\n)后,末尾那个换行
        // 又把光标推到一个空行上,所以滚动前可视区其实是 L9 + 空行,历史是
        // [L6,L7,L8](实测确认,不是「凑整」的 L8/L9 + L5/L6/L7)。
        // 滚到顶 → 顶端可视首行 = 最旧的历史行 L6。
        let mut emu = Emulator::with_history(10, 2, 3);
        for i in 0..10 {
            emu.feed(format!("L{i}\r\n").as_bytes());
        }
        emu.scroll(Scroll::Top);
        assert_eq!(row_text(&emu.snapshot(), 0), "L6");
    }

    #[test]
    fn default_history_survives_short_session() {
        // 默认 DEFAULT_HISTORY(10_000)足够深:这里只喂 100 行做下限抽样
        // (远小于 DEFAULT_HISTORY,不是说默认深度就是 100),滚到顶还能看到第一行。
        let mut emu = Emulator::new(10, 2);
        for i in 0..100 {
            emu.feed(format!("L{i}\r\n").as_bytes());
        }
        emu.scroll(Scroll::Top);
        assert_eq!(row_text(&emu.snapshot(), 0), "L0");
    }

    #[test]
    fn alt_screen_has_no_scrollback() {
        // F17 的根本理由:alt screen(tmux/vim)的 grid history 恒 0,
        // alacritty 里改不了。滚轮在 alt 下必须分流(上报/方向键),
        // 指望本地回溯是白费——这条测试把这个事实钉住。
        let mut emu = Emulator::new(10, 2);
        emu.feed(b"\x1b[?1049h"); // 进 alt screen
        emu.feed(b"a\r\nb\r\nc");
        assert_eq!(row_text(&emu.snapshot(), 0), "b");
        emu.scroll(Scroll::Delta(5));
        assert_eq!(
            row_text(&emu.snapshot(), 0),
            "b",
            "alt screen 下 scroll_display 静默无效,画面不该动"
        );
    }

    #[test]
    fn simple_selection_returns_dragged_text() {
        // 从 (0,0) 拖到 (4,0):hello 五个字符全在内。右端取 Right 侧,
        // 否则最后一格落在"格左半"上不算进选区,选出来会少一个字符。
        let mut emu = Emulator::new(20, 3);
        emu.feed(b"hello world");
        emu.selection_start(0, 0, SelectionKind::Simple, CellSide::Left);
        emu.selection_update(4, 0, CellSide::Right);
        assert_eq!(emu.selection_text().as_deref(), Some("hello"));
    }

    #[test]
    fn empty_selection_is_none() {
        // 没开选区时不能返回 Some("")——上层据此判断"要不要写剪贴板",
        // 返回空串会把用户剪贴板里原有的内容清掉。
        let emu = Emulator::new(20, 3);
        assert_eq!(emu.selection_text(), None);
    }

    #[test]
    fn cleared_selection_is_none() {
        let mut emu = Emulator::new(20, 3);
        emu.feed(b"hello");
        emu.selection_start(0, 0, SelectionKind::Simple, CellSide::Left);
        emu.selection_update(4, 0, CellSide::Right);
        assert!(emu.selection_text().is_some());
        emu.selection_clear();
        assert_eq!(emu.selection_text(), None);
    }

    #[test]
    fn scrolled_selection_keeps_pointing_at_same_text() {
        // F18 头号坑:alacritty 的 `Line` 带符号,0 = 当前视口顶行、负数是历史。
        // viewport row → buffer line 不减 display_offset 的话,回溯之后在同一个
        // 屏幕位置划选,选出来的是另一段文本(视觉与选区错位)。
        let mut emu = Emulator::new(20, 2);
        emu.feed(b"alpha\r\nbravo\r\ncharlie");
        // 不滚动时:视口是 bravo / charlie,顶行选出 bravo。
        emu.selection_start(0, 0, SelectionKind::Simple, CellSide::Left);
        emu.selection_update(4, 0, CellSide::Right);
        assert_eq!(emu.selection_text().as_deref(), Some("bravo"));

        // 回溯一行后视口顶行变成 alpha,同一个屏幕位置必须选出 alpha。
        emu.selection_clear();
        emu.scroll(Scroll::Delta(1));
        emu.selection_start(0, 0, SelectionKind::Simple, CellSide::Left);
        emu.selection_update(4, 0, CellSide::Right);
        assert_eq!(
            emu.selection_text().as_deref(),
            Some("alpha"),
            "未减 display_offset:回溯后选区指向了错误的 buffer 行"
        );
    }

    #[test]
    fn semantic_selection_expands_to_word() {
        // 双击选词:点在词中间,向两侧扩到语义分隔符。
        let mut emu = Emulator::new(20, 2);
        emu.feed(b"hello world");
        emu.selection_start(8, 0, SelectionKind::Semantic, CellSide::Left);
        assert_eq!(emu.selection_text().as_deref(), Some("world"));
    }

    #[test]
    fn lines_selection_takes_whole_line() {
        // 三击选行:点在行中任意位置,整行都进选区(alacritty 的 Lines 末尾补 \n)。
        let mut emu = Emulator::new(20, 2);
        emu.feed(b"hello world");
        emu.selection_start(3, 0, SelectionKind::Lines, CellSide::Left);
        assert_eq!(emu.selection_text().as_deref(), Some("hello world\n"));
    }

    #[test]
    fn snapshot_marks_selected_cells() {
        // 反色渲染只能靠这个标记——渲染层拿不到 alacritty 的选区。
        let mut emu = Emulator::new(10, 2);
        emu.feed(b"hello");
        emu.selection_start(0, 0, SelectionKind::Simple, CellSide::Left);
        emu.selection_update(2, 0, CellSide::Right);
        let snap = emu.snapshot();
        let marks: Vec<bool> = snap.row(0)[..5].iter().map(|c| c.selected).collect();
        assert_eq!(marks, vec![true, true, true, false, false]);
        // 第二行没选中。
        assert!(snap.row(1).iter().all(|c| !c.selected));
    }

    #[test]
    fn snapshot_marks_wide_char_spacer_as_selected_too() {
        // F16 + F18:选中中文时右半 spacer 也要标中,否则反色底只画半个字。
        let mut emu = Emulator::new(10, 1);
        emu.feed("中x".as_bytes());
        emu.selection_start(0, 0, SelectionKind::Simple, CellSide::Left);
        emu.selection_update(0, 0, CellSide::Right);
        let snap = emu.snapshot();
        assert!(snap.row(0)[0].selected, "宽字左半应选中");
        assert!(
            snap.row(0)[1].selected,
            "宽字右半 spacer 未标中 → 中文选区只有半个字有底色"
        );
        assert!(!snap.row(0)[2].selected);
    }

    #[test]
    fn snapshot_marks_wide_char_leading_half_when_selection_starts_on_spacer() {
        // 上一条的镜像:鼠标落在中文字的右半再往右拖(完全正常的操作),
        // 选区起点在 spacer 列上,左半也必须标中,否则同样只画半个字的底色。
        let mut emu = Emulator::new(10, 1);
        emu.feed("中x".as_bytes());
        emu.selection_start(1, 0, SelectionKind::Simple, CellSide::Left);
        emu.selection_update(2, 0, CellSide::Right);
        let snap = emu.snapshot();
        assert!(
            snap.row(0)[0].selected,
            "宽字左半未跟随 spacer → 中文选区只有半个字有底色(镜像版)"
        );
        assert!(snap.row(0)[1].selected);
    }

    #[test]
    fn snapshot_selection_follows_scroll() {
        // 选区标记与 display_offset 同源:滚动后标记必须跟着内容走,
        // 否则回溯时高亮停在原来的屏幕位置上。
        let mut emu = Emulator::new(10, 2);
        emu.feed(b"alpha\r\nbravo\r\ncharlie");
        emu.selection_start(0, 0, SelectionKind::Simple, CellSide::Left);
        emu.selection_update(4, 0, CellSide::Right);
        assert!(emu.snapshot().row(0)[0].selected, "选的是当前顶行 bravo");

        emu.scroll(Scroll::Delta(1));
        let snap = emu.snapshot();
        assert!(
            !snap.row(0)[0].selected,
            "回溯一行后顶行是 alpha,不该还高亮"
        );
        assert!(snap.row(1)[0].selected, "bravo 下移一行,高亮应跟着走");
    }

    /// F80:注入的默认色必须真的穿透 resolve 到达 snapshot。
    /// 只测 palette 层不够——中间隔着 Emulator 的字段和 snapshot 的调用。
    #[test]
    fn injected_default_colors_reach_snapshot() {
        let mut emu = Emulator::new(4, 2);
        emu.set_default_colors(Rgb::new(0xe4, 0xe6, 0xf0), Rgb::new(0x14, 0x16, 0x1f));
        emu.feed(b"x");
        let snap = emu.snapshot();
        let cell = &snap.row(0)[0];
        assert_eq!(cell.ch, 'x');
        assert_eq!(cell.fg, Rgb::new(0xe4, 0xe6, 0xf0), "前景应是注入值");
        assert_eq!(cell.bg, Rgb::new(0x14, 0x16, 0x1f), "背景应是注入值");
    }

    /// 不注入时保持出厂值,老行为不变。
    #[test]
    fn without_injection_snapshot_uses_factory_defaults() {
        let mut emu = Emulator::new(4, 2);
        emu.feed(b"x");
        let snap = emu.snapshot();
        let cell = &snap.row(0)[0];
        assert_eq!(cell.fg, crate::palette::DEFAULT_FG);
        assert_eq!(cell.bg, crate::palette::DEFAULT_BG);
    }
}
