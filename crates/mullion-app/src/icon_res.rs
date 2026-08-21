//! 程序图标(F152)在 exe 资源段里的序号。
//!
//! 别跟另外两个同名模块搞混:`ui::icon` 是自绘的控制图标(箭头/叉),
//! `ui::ico` 是用户导入的会话图标。这里说的是**程序自己**那张脸 ——
//! 资源管理器里的文件图标、标题栏左上角、任务栏、Alt-Tab。
//!
//! 单独成模块只为一件事:让这个序号在 Linux 上也编得到、测得着。
//! 真正用它的地方(`Icon::from_resource`)裹在 `cfg(windows)` 里,本机
//! `cargo test` 一行都碰不到,序号写错了在这边完全没有症状。

/// `assets/mullion.rc` 里 `1 ICON "mullion.ico"` 的那个 1。
///
/// 改这里必须同时改 .rc —— 两边对不上时 `Icon::from_resource` 返回 `Err`,
/// 而我们的处置是「取不到就不设」,于是构建绿、exe 文件图标还在,
/// **只有窗口和任务栏图标静默消失**。守护测试见 `tests/icon_resource.rs`。
pub const RESOURCE_ID: u16 = 1;

/// 窗口标题栏左上角那张小图的边长(逻辑像素)。
///
/// `WM_SETICON` 的 `ICON_SMALL` 档。不传尺寸的话 winit 用 `LR_DEFAULTSIZE`,
/// 拿到的是 `SM_CXICON`(32),再被系统压到 16 显示 —— 缩放是最近邻,
/// 细线条会糊。直接要 16,让 Windows 从 ico 里挑那一帧。
pub const SMALL_PX: u32 = 16;
