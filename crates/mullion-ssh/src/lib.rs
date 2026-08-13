//! mullion-ssh —— russh PTY channel。不认识「pane」「窗口」,只认字节流。
//!
//! 架构不变量:本 crate 不依赖 core/term/app。

pub mod config;
pub mod dial;
pub mod error;
pub mod exec;
pub mod hop;
pub mod known_hosts;
pub mod proxy;
pub mod remove_tree;
pub mod schedule;
pub mod session;
pub mod sftp;
mod socks5_server;
pub mod tunnel;
