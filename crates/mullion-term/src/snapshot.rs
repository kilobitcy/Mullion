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
    /// 空心框。远端主动要求时才会出现(我们自己用它表示"非焦点 pane",
    /// 那条路径不看这个字段)。
    HollowBlock,
    /// 远端要求不画光标。
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
}

impl GridSnapshot {
    /// 第 `row` 行的单元格切片(长度 == cols)。
    pub fn row(&self, row: u16) -> &[SnapCell] {
        let start = row as usize * self.cols as usize;
        &self.cells[start..start + self.cols as usize]
    }
}
