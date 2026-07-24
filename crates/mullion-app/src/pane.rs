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
    pub fn new(id: PaneId, cols: u16, rows: u16) -> Self {
        Self {
            id,
            emulator: Emulator::new(cols, rows),
        }
    }
}
