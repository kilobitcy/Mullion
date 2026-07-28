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
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use vte::ansi::Processor;

use crate::palette;
use crate::snapshot::{Cursor, GridSnapshot, SnapCell};

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
        let mut cells = Vec::with_capacity(cols * rows);
        for line in 0..rows {
            let row = &grid[Line(line as i32 - offset)];
            for col in 0..cols {
                let cell = &row[Column(col)];
                let flags = cell.flags;
                cells.push(SnapCell {
                    ch: cell.c,
                    fg: palette::resolve(cell.fg, colors),
                    bg: palette::resolve(cell.bg, colors),
                    width: if flags.contains(Flags::WIDE_CHAR) {
                        2
                    } else {
                        1
                    },
                    // 只有 WIDE_CHAR_SPACER(宽字符右半)才跳过渲染;LEADING_WIDE_CHAR_SPACER
                    // 是行尾放不下整体换行时插入的独立空白占位格,左边不是宽字,
                    // 当普通空格处理——标成 spacer 会让下游静默漏画这一列的背景。
                    spacer: flags.contains(Flags::WIDE_CHAR_SPACER),
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
}
