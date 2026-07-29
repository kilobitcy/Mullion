//! 选区的对外类型(F18)。
//!
//! alacritty 的 `SelectionType` / `Side` / `Point` **不外泄给 app**:app 只传
//! 0-based viewport 单元格坐标和这里的两个枚举,换算与 alacritty 打交道全在
//! `emulator.rs` 内部。这与 B0 重导出 `TermMode`/`Scroll` 的口径一致——
//! 能封的就封,封不掉的才重导出。

/// 选区类型:拖拽 / 双击选词 / 三击选行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionKind {
    /// 拖拽:精确按格,不做任何扩展。
    Simple,
    /// 双击:向两侧扩展到最近的语义分隔符(词边界)。
    Semantic,
    /// 三击:整行。
    Lines,
}

/// 指针落在单元格的左半还是右半。决定该格算不算进选区,直接影响"跟手"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellSide {
    Left,
    Right,
}
