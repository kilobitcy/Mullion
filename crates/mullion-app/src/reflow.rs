//! 分屏 reflow:布局变更后对每个受影响 pane 发 `window_change`(F34,守护陷阱 T4)。
//!
//! 不发的话,tmux 里的 Claude Code 还按旧列数排版,全屏 TUI 直接错行。

use mullion_core::layout::{compute_rects, Node, PaneId, Rect};

/// 某个 pane 收到新尺寸的抽象出口。真实实现把它变成 SSH `window_change` 请求;
/// 测试注入 fake sink 断言收到的列/行数。
pub trait ResizeSink {
    fn resize(&mut self, pane: PaneId, cols: u16, rows: u16);
}

/// 按当前布局对每个 pane 发出 resize,列/行数取自新矩形(F34)。
pub fn reflow(root: &Node, area: Rect, sink: &mut impl ResizeSink) {
    for (pane, rect) in compute_rects(root, area) {
        sink.resize(pane, rect.cols, rect.rows);
    }
}
