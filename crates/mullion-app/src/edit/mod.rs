//! F53:远端文件的编辑通路。
//!
//! **不走传输队列**(设计 D3-1):临时路径是我们自己造的,冲突 / 重名 /
//! Windows 非法名那一整套语义全都不适用;而传输面板是「用户发起的传输」的
//! 账本,混进「打开一个文件」会让「全部取消」的语义变歧义。
//!
//! 这一层是纯逻辑:零 egui、零 tokio、零网络。窗口在 `ui::editor_window`,
//! 「编辑中」列表在 `ui::edit_panel`,接线在 `app.rs`。

/// 内置编辑器的上限(D3-2)。整个文件要变成一个 `String` 塞进 `TextEdit`,
/// 再大 egui 每帧重新布局就顶不住了。
pub const INLINE_LIMIT: u64 = 1024 * 1024;

/// 外部编辑的上限(D3-2)。这一条不是 egui 的问题,是**回传**的问题:
/// 编辑走的是全量读 + 全量覆盖写,没有断点续传,高延迟链路上传一个几百 MB
/// 的文件失败一次就全白干 —— 那种量该走传输队列。
pub const EXTERNAL_LIMIT: u64 = 64 * 1024 * 1024;

pub mod launch;
pub mod sessions;
pub mod tempdir;
pub mod text;
