//! Pane:一格分屏,串起一个 VT 仿真器与(后续)一条 SSH PtySession。
//! 骨架占位——真实 SSH channel 串接未验证,需人工确认。

use mullion_core::layout::PaneId;
use mullion_term::emulator::Emulator;

/// 一格分屏。骨架只持有 VT 仿真器;SSH PtySession 后续接入(F35 复用连接开 channel)。
pub struct Pane {
    pub id: PaneId,
    pub emulator: Emulator,
}

impl Pane {
    /// `defaults` 来自 `theme::term_default_colors`(F80 三处同源),不要传
    /// `palette::DEFAULT_*`——那样终端底色会和 clear 色失配。
    pub fn new(
        id: PaneId,
        cols: u16,
        rows: u16,
        defaults: mullion_term::palette::DefaultColors,
    ) -> Self {
        let mut emulator = Emulator::new(cols, rows);
        emulator.set_default_colors(defaults.fg, defaults.bg);
        Self { id, emulator }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{term_default_colors, MULLION_DARK};

    /// §3.2 三处同源的第三处:Emulator 的注入值。前两处是编译期常量、好守,
    /// 这处发生在运行时接线,恰恰最容易漏改——所以单独守一条。
    #[test]
    fn terminal_defaults_come_from_theme() {
        let d = term_default_colors(&MULLION_DARK);
        let pane = Pane::new(PaneId(1), 4, 2, d);
        let snap = pane.emulator.snapshot();
        let cell = &snap.row(0)[0];
        assert_eq!(cell.bg, MULLION_DARK.term_bg, "空格背景应是主题底色");
        assert_eq!(cell.fg, MULLION_DARK.term_fg, "空格前景应是主题前景");
    }
}
