//! 会话管理弹窗:列表 + CRUD + 编辑表单(Task 6,§4.3/§1.2)。
//!
//! 关键约束:这里渲染在 `app.rs` 的 `egui_ctx.run(|ctx| ...)` 闭包内,只能拿到
//! `&mut UiState`,拿不到 `&mut SessionStore`(否则借用检查器过不了)。所以任何会
//! 改 store / 发起连接的动作,这里只写「意图」到 `UiState`,由 `app.rs` 在
//! `render_frame` 返回、借用释放之后统一施加——与既有 `request_disconnect`/
//! `request_quit` 完全同构。

mod buffer;
mod editor;
mod list;

pub(crate) use buffer::{build_draft, AuthKindUi, ProxyModeUi};
pub use buffer::{EditorBuffer, SaveIntent};

use mullion_store::{GroupId, GroupRecord, SessionRecord};

use crate::theme::{self, Theme};

use super::UiState;

/// 设计稿 §3:880×560 单窗,左栏定宽 300。
pub(crate) const WINDOW_W: f32 = 880.0;
pub(crate) const WINDOW_H: f32 = 560.0;
pub(crate) const LIST_W: f32 = 300.0;
/// 内容区最小高度。egui 的 `Window` 高度默认跟内容走,不撑到 `default_size` 给的
/// 高度;靠这一行把双栏撑满,否则会话少时窗口会缩成一条。见 §3 的待验证假设。
pub(crate) const CONTENT_MIN_HEIGHT: f32 = 480.0;

/// 每个分组桶对应的 `CollapsingHeader` 构造。抽成独立函数**只为了能在测试里
/// 直接调它**:`CollapsingHeader::new` 默认把标题文本本身当 id 源(见 egui 0.30
/// `collapsing_header.rs`)。两个分组恰好同名(且当前会话数也一样,标题完全
/// 一致)时会撞 id、共享同一份展开/收起状态——编译不报错,跑起来才会看到
/// "点开 A,B 也跟着变"。`.id_salt(gid)` 用分组主键(`None`=未分组桶)强制
/// 区分,彻底绕开标题文本。守护测试
/// `collapsing_header_id_salt_disambiguates_same_titled_groups` 直接调这个函数
/// (不是重抄一遍表达式),删掉 `.id_salt(gid)` 这行测试就会红。
fn group_header(title: &str, gid: Option<GroupId>, count: usize) -> egui::CollapsingHeader {
    egui::CollapsingHeader::new(format!("{title}({count})"))
        .id_salt(gid)
        .default_open(true)
}

/// 会话管理器弹窗:双栏(左列表 300px + 右编辑表单)合成单窗(F90)。
/// `store_available=false` 时(待定 G:keyring/库打开失败)不崩,只展示兜底提示。
pub fn show(
    ctx: &egui::Context,
    t: &Theme,
    ui_state: &mut UiState,
    sessions: &[SessionRecord],
    groups: &[GroupRecord],
    store_available: bool,
) {
    if !ui_state.session_manager_open {
        return;
    }

    let mut open = true;
    egui::Window::new("会话管理器")
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size([WINDOW_W, WINDOW_H])
        .min_width(720.0)
        .frame(
            egui::Frame::window(&ctx.style())
                .fill(theme::c32(t.bar_status))
                .rounding(12.0),
        )
        .show(ctx, |ui| {
            ui.set_min_height(CONTENT_MIN_HEIGHT);

            // §3.1 降级:没有会话库时不画双栏,只给一句话,避免用户对着空表单填半天。
            if !store_available {
                ui.colored_label(
                    theme::c32(t.danger),
                    "会话库不可用,无法读写会话(详见状态栏错误)。",
                );
                return;
            }

            egui::SidePanel::left(ui.id().with("sm_list"))
                .exact_width(LIST_W)
                .resizable(false)
                .frame(
                    egui::Frame::none()
                        .fill(theme::c32(t.panel_bg))
                        .inner_margin(10.0),
                )
                .show_inside(ui, |ui| list::show(ui, t, ui_state, sessions, groups));

            egui::CentralPanel::default()
                .frame(
                    egui::Frame::none()
                        .fill(theme::c32(t.bar_status))
                        .inner_margin(14.0),
                )
                .show_inside(ui, |ui| editor::show(ui, t, ui_state, sessions, groups));
        });

    if !open {
        ui_state.session_manager_open = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 复审坑:`egui::CollapsingHeader::new(text)` 默认把标题文本本身当 id 源
    /// (`egui-0.30.0/src/containers/collapsing_header.rs::new`)。两个分组
    /// 名字相同、桶内会话数也相同时,列表里两个 header 的标题文本会完全一致
    /// ——不加 `.id_salt` 就会撞 id、共享同一份展开/收起状态,点开一个另一个
    /// 也跟着变。这条测试直接调 `group_header`(`show()` 内部实际用的同一个
    /// 函数,不是重抄一遍表达式),同一个父 `ui`、相同标题、不同 `gid`:
    /// 去掉 `group_header` 里的 `.id_salt(gid)` 这行,两个 `header_response.id`
    /// 会相等,下面的 `assert_ne!` 就会失败(已实测确认,见提交说明)。
    #[test]
    fn collapsing_header_id_salt_disambiguates_same_titled_groups() {
        let ctx = egui::Context::default();
        let mut ids: Option<(egui::Id, egui::Id)> = None;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let resp_a = group_header("生产", Some(GroupId(1)), 1).show(ui, |_| {});
                let resp_b = group_header("生产", Some(GroupId(2)), 1).show(ui, |_| {});
                ids = Some((resp_a.header_response.id, resp_b.header_response.id));
            });
        });
        let (id_a, id_b) = ids.expect("闭包必须跑到底,写回 ids");
        assert_ne!(
            id_a, id_b,
            "两个分组标题相同时,header 的持久化 id 必须靠 gid 区分,否则展开状态会互相串"
        );
    }
}
