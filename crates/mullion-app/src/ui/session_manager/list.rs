//! 会话管理器**左栏**:分组树、会话行、底部操作、删除二次确认。
//!
//! 只读 `UiFrame` 的数据,只往 `UiState` 写意图 —— 不碰 `SessionStore`
//! (egui 闭包里拿不到 `&mut SessionStore`,这是 app 侧的硬约束)。

use egui::Ui;
use mullion_store::model::SessionRecord;
use mullion_store::{GroupRecord, Protocol};

use crate::theme::{self, Theme};
use crate::ui::session_manager::{group_header, EditorBuffer};
use crate::ui::UiState;

pub(super) fn show(
    ui: &mut Ui,
    t: &Theme,
    ui_state: &mut UiState,
    sessions: &[SessionRecord],
    groups: &[GroupRecord],
) {
    for (gid, bucket) in crate::ui::group_manager::group_sessions(groups, sessions) {
        let title = match gid {
            Some(id) => groups
                .iter()
                .find(|g| g.id == id)
                .map(|g| g.name.clone())
                .unwrap_or_else(|| "未分组".to_string()),
            None => "未分组".to_string(),
        };
        group_header(&title, gid, bucket.len()).show(ui, |ui| {
            egui::Grid::new(format!("session_list_grid_{gid:?}"))
                .num_columns(5)
                .striped(true)
                .show(ui, |ui| {
                    ui.strong("名称");
                    ui.strong("主机:端口");
                    ui.strong("协议");
                    ui.strong("用户");
                    ui.strong("修改时间");
                    ui.end_row();
                    for rec in &bucket {
                        let is_selected = ui_state.selected == Some(rec.id);
                        let name_resp = ui.selectable_label(is_selected, &rec.identity.name);
                        if name_resp.clicked() {
                            ui_state.selected = Some(rec.id);
                        }
                        if name_resp.double_clicked() {
                            ui_state.connect_request = Some(rec.id);
                        }
                        ui.label(format!("{}:{}", rec.connection.host, rec.connection.port));
                        ui.label(match rec.connection.protocol {
                            Protocol::Ssh => "ssh",
                            Protocol::Sftp => "sftp",
                        });
                        ui.label(&rec.auth.user);
                        ui.label(&rec.modified_at);
                        ui.end_row();
                    }
                });
        });
    }

    ui.separator();
    // 双击/点连接失败(如 sftp 会话映射拒绝)、保存/删除失败都写
    // ui_state.last_error;这里必须总是渲染,否则「点了没反应」(复核 #2)。
    if let Some(err) = &ui_state.last_error {
        ui.colored_label(theme::c32(t.danger), err);
    }
    ui.horizontal(|ui| {
        if ui.button("新建").clicked() {
            ui_state.editor_id = None;
            ui_state.editor = EditorBuffer::default();
        }
        let has_selection = ui_state.selected.is_some();
        if ui
            .add_enabled(has_selection, egui::Button::new("编辑"))
            .clicked()
        {
            if let Some(rec) = ui_state
                .selected
                .and_then(|id| sessions.iter().find(|s| s.id == id))
            {
                ui_state.editor_id = Some(rec.id);
                ui_state.editor = EditorBuffer::from_record(rec);
            }
        }
        if ui
            .add_enabled(has_selection, egui::Button::new("删除"))
            .clicked()
        {
            ui_state.pending_delete = ui_state.selected;
        }
        if ui
            .add_enabled(has_selection, egui::Button::new("连接"))
            .clicked()
        {
            ui_state.connect_request = ui_state.selected;
        }
    });

    // 删除二次确认:原来是内嵌的独立小窗口(F90 前),现在合入单窗,直接画在
    // 按钮行下方——不再弹出第二个 `egui::Window`(会破坏单窗断言)。
    if let Some(id) = ui_state.pending_delete {
        let name = sessions
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.identity.name.clone())
            .unwrap_or_default();
        ui.separator();
        ui.colored_label(
            theme::c32(t.danger),
            format!("确定删除会话「{name}」?此操作不可撤销。"),
        );
        ui.horizontal(|ui| {
            if ui.button("确认删除").clicked() {
                ui_state.delete_request = Some(id);
                ui_state.pending_delete = None;
                if ui_state.selected == Some(id) {
                    ui_state.selected = None;
                }
            }
            if ui.button("取消").clicked() {
                ui_state.pending_delete = None;
            }
        });
    }
}
