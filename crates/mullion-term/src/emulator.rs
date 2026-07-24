//! VT 仿真封装:把 `alacritty_terminal::Term` 藏在后面,只暴露「喂字节 / 取回写」,
//! 以及取渲染快照(F10/F16)、resize(F34)。
//!
//! 守护陷阱 T1(F11 前置):Term 通过 `EventListener` 发出的 `Event::PtyWrite`
//! 必须被收集并回写 SSH channel。漏了会导致同步输出探测(`CSI ? 2026 $ p`)无应答
//! → 全屏 TUI 退回逐帧刷新而闪、鼠标上报全废、光标位置查询永久卡死。
//! 本模块把这些字节攒进出站缓冲,由上层取走回写。

use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term};
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
/// `#[cfg(test)]` 私用)。骨架无 scrollback,故 `total_lines == screen_lines`(F17 回溯后续补)。
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
    /// 新建 `cols × rows` 的仿真器。
    pub fn new(cols: u16, rows: u16) -> Self {
        let collector = PtyWriteCollector::default();
        let dims = GridSize { cols, rows };
        let term = Term::new(Config::default(), &dims, collector.clone());
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

    /// 当前屏面的渲染快照(F10/F16)。颜色解析成具体 RGB,宽字符标 width=2。
    /// MVP 无 scrollback(display_offset 恒 0),直接按屏面行列遍历。
    pub fn snapshot(&self) -> GridSnapshot {
        let grid = self.term.grid();
        let colors = self.term.colors();
        let cols = grid.columns();
        let rows = grid.screen_lines();
        let mut cells = Vec::with_capacity(cols * rows);
        for line in 0..rows {
            let row = &grid[Line(line as i32)];
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
        GridSnapshot {
            cols: cols as u16,
            rows: rows as u16,
            cells,
            cursor: Cursor {
                row: p.line.0 as u16,
                col: p.column.0 as u16,
                // MVP 未接 DECTCEM(`\x1b[?25l`/`\x1b[?25h`)光标隐藏/显示,恒 true;后续补。
                visible: true,
            },
        }
    }

    /// 改变网格尺寸(F34:分屏 reflow / 窗口 resize 时调用)。
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.term.resize(GridSize { cols, rows });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::Rgb;

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
}
