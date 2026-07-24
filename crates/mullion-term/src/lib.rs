//! mullion-term —— VT 仿真封装 + 输入编码。只依赖 alacritty_terminal / vte。
//!
//! 架构不变量:本 crate 不认识「pane」「窗口」,只做 VT 状态机封装与键鼠编码。

pub mod emulator;
pub mod keymap;
pub mod palette;
pub mod snapshot;
