//! 会话管理器**右栏**:标题条 + 错误卡片 + 三个 Tab + 底部按钮条(F90 Task 11)。
//!
//! 原来是一个独立的 `egui::Window`(F90 前),现在是主窗右侧的 `CentralPanel`,
//! 每帧都渲染——「关闭」表单这个概念不复存在,「取消」只是把
//! `ui_state.editor`/`editor_id`/`editor_baseline` 重置回空(等价于回到
//! 「未编辑任何会话」态,画空态提示)。
//!
//! 字段本身的布局是 Task 12 的事,这里只挂三个 Tab 的占位调用点
//! (`super::fields::{basic,auth,network}`)。

use egui::Ui;

use crate::theme::{self, Theme};
use crate::ui::session_manager::SecretPresence;
use crate::ui::UiState;
use mullion_store::GroupRecord;

/// 三个 Tab 的标题。索引即 `UiState::editor_tab`。
const TABS: [&str; 3] = ["连接", "认证", "高级"];

pub(super) fn show(
    ui: &mut Ui,
    t: &Theme,
    ui_state: &mut UiState,
    groups: &[GroupRecord],
    presence: SecretPresence,
) {
    // 没选中任何会话 → 空态提示,不画一张什么都填不进去的空表单。
    let Some(buf) = ui_state.editor.as_mut() else {
        ui.centered_and_justified(|ui| {
            ui.colored_label(theme::c32(t.fg_faint), "从左侧选一条会话,或点「+ 新建」");
        });
        return;
    };

    // 标题条
    ui.horizontal(|ui| {
        let title = if buf.name.trim().is_empty() {
            "新建会话".to_string()
        } else {
            buf.name.clone()
        };
        ui.label(
            egui::RichText::new(title)
                .size(16.0)
                .color(theme::c32(t.fg)),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("复制连接串").clicked() {
                ui.ctx().copy_text(super::connect_string(buf));
            }
        });
    });
    ui.add_space(6.0);

    // 未保存变更确认横幅(F90 Task 14)。`pending_switch` 落在 `mod.rs::show`
    // 里判脏,脏时置 `confirm_switch=true` 后借用已释放,这里只管画。
    // 「丢弃并切换」不能在这里直接调 `apply_switch`(它要重设
    // `ui_state.editor`,而这里正持着 `buf = ui_state.editor.as_mut()` 的
    // `&mut`,同帧内改不了同一个字段)——中转一个 bool,真正施加挪到
    // `mod.rs::show` 里 `Window::show` 借用释放之后。
    if ui_state.confirm_switch {
        egui::Frame::none()
            .fill(theme::c32(t.sunken_bg))
            .stroke(egui::Stroke::new(1.0, theme::c32(t.warn)))
            .rounding(8.0)
            .inner_margin(10.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(theme::c32(t.warn), "有未保存的更改");
                    if ui.button("丢弃并切换").clicked() {
                        ui_state.discard_and_switch = true;
                    }
                    if ui.button("留在这里").clicked() {
                        ui_state.pending_switch = None;
                        ui_state.confirm_switch = false;
                    }
                });
            });
        ui.add_space(8.0);
    }

    // §5.2 错误卡片:比状态栏那行显眼,且可关闭。关闭后下一个新错误会由
    // `UiState::set_error` 重新展开(它复位 error_dismissed)。
    if let (Some(msg), false) = (ui_state.last_error.clone(), ui_state.error_dismissed) {
        egui::Frame::none()
            .fill(theme::c32(t.sunken_bg))
            .stroke(egui::Stroke::new(1.0, theme::c32(t.danger_soft)))
            .rounding(8.0)
            .inner_margin(10.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(theme::c32(t.danger_soft), msg);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("×").clicked() {
                            ui_state.error_dismissed = true;
                        }
                    });
                });
            });
        ui.add_space(8.0);
    }

    // Tab 条
    ui.horizontal(|ui| {
        for (i, name) in TABS.iter().enumerate() {
            if ui
                .selectable_label(ui_state.editor_tab == i, *name)
                .clicked()
            {
                ui_state.editor_tab = i;
            }
        }
    });
    ui.separator();

    // 底部按钮条用 TopBottomPanel 先占位,Tab 内容吃剩余高度。
    //
    // **不要写成 `let bottom = 44.0; let body_h = ui.available_height() - bottom;`**
    // 再喂给 `ScrollArea::max_height` —— 左栏原本就是这么写的,在 Windows 11
    // 实机上把「+ 新建」按钮顶出了可见区(见 c4eb7f1)。两个原因:
    // `ui.available_height()` 在 panel 内返回的是 `Window` 的**布局高度**而非
    // 真实可见高度;硬编码的 44.0 必须与底栏实际渲染高度保持同步,一旦界面缩放
    // 或字号变大就失同步,且没有任何编译错误或测试会提示。
    // panel 布局天然保证「panel 先分配、中央区吃剩余」,不需要猜数字。
    //
    // 「取消」只置意图,不在这里改 `ui_state.editor` —— 见代码块后的借用说明。
    let mut cancel = false;
    egui::TopBottomPanel::bottom(ui.id().with("sm_editor_bottom"))
        .frame(egui::Frame::none())
        .show_separator_line(false)
        .show_inside(ui, |ui| {
            ui.separator();
            ui.horizontal(|ui| {
                let save = ui.button("保存").clicked();
                let save_connect = ui.button("保存并连接").clicked();
                if save || save_connect {
                    ui_state.save_click = Some(save_connect);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    cancel |= ui.button("取消").clicked();
                });
            });
        });

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| match ui_state.editor_tab {
            0 => super::fields::basic(ui, t, buf, groups),
            1 => super::fields::auth(ui, t, buf, presence),
            _ => super::fields::network(ui, t, buf, presence),
        });

    // `buf` 的借用到此结束,现在才能动 `ui_state.editor`。
    if cancel {
        ui_state.editor = None;
        ui_state.editor_baseline = None;
        ui_state.editor_id = None;
    }
}
