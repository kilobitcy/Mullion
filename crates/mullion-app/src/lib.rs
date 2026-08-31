//! mullion-app —— 窗口、渲染、输入分发。唯一允许知道 core/term/ssh 的地方。
//!
//! 依赖方向 app → {core, term, ssh}。攒帧/帧率/reflow 等纯件抽出来无窗口单测
//! (T2/T3/T4);winit/wgpu/glyphon 的真实渲染在 bin(`main.rs`)占位,
//! 无法在无头容器自动验证,需人工确认。

pub mod app;
pub mod automation;
pub mod bands;
pub mod cli;
pub mod clipboard;
pub mod diag;
pub mod dragout;
pub mod edit;
pub mod files;
pub mod font_pick;
pub mod frame;
pub mod frame_fp;
pub mod gpu;
pub mod grid;
pub mod heapgauge;
pub mod host_key;
pub mod icon_res;
pub mod input;
pub mod localtime;
pub mod logx;
pub mod pane;
pub mod profile;
pub mod reconnect;
pub mod redact;
pub mod reflow;
pub mod remote_bootstrap;
pub mod render;
pub mod row_fp;
pub mod session_pump;
pub mod shaped_cache;
pub mod shell;
pub mod shell_bootstrap;
pub mod shot;
pub mod sysprobe;
pub mod text;
pub mod theme;
pub mod tunnels;
pub mod ui;
pub mod wev;

/// F190:全局分配器换成带记账的那层。
///
/// **必须挂在 lib 上,不能挂在 `main.rs`**:测试链接的是这个 lib、不是那个
/// bin,挂错地方的话 `heapgauge` 的计数器在整个测试进程里恒为 0,四条守护
/// 全部退化成「0 落在 0 附近」式的恒绿 —— 而生产里它照样能工作,于是这个
/// 错误不会有任何症状,直到某天有人依赖那几条测试。
#[global_allocator]
static GLOBAL: heapgauge::CountingAlloc = heapgauge::CountingAlloc(&heapgauge::GLOBAL);
