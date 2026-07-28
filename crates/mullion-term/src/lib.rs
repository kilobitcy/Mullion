//! mullion-term —— VT 仿真封装 + 输入编码。只依赖 alacritty_terminal / vte。
//!
//! 架构不变量:本 crate 不认识「pane」「窗口」,只做 VT 状态机封装与键鼠编码。

pub mod emulator;
pub mod keymap;
pub mod palette;
pub mod snapshot;

/// alacritty 的终端模式位与滚动指令。app 层做滚轮分流要用,经这里重导出,
/// 免得 mullion-app 直接依赖 alacritty_terminal(架构不变量:app 只认识我们四个 crate)。
pub use alacritty_terminal::grid::Scroll;
pub use alacritty_terminal::term::TermMode;
