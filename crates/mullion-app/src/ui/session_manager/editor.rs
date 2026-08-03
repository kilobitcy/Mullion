//! 会话管理器**右栏**:被编辑会话的表单。
//!
//! 原来是一个独立的 `egui::Window`(F90 前),现在是主窗右侧的 `CentralPanel`,
//! 每帧都渲染——「关闭」表单这个概念不复存在,保存/取消只是把
//! `ui_state.editor`/`editor_id` 重置回空白(等价于回到「未编辑任何会话」态)。

use egui::Ui;
use mullion_store::model::SessionRecord;
use mullion_store::{GroupRecord, Protocol};

use crate::theme::{self, Theme};
use crate::ui::session_manager::{build_draft, AuthKindUi, EditorBuffer, ProxyModeUi, SaveIntent};
use crate::ui::UiState;

pub(super) fn show(
    ui: &mut Ui,
    t: &Theme,
    ui_state: &mut UiState,
    sessions: &[SessionRecord],
    groups: &[GroupRecord],
) {
    let editing_id = ui_state.editor_id;
    let title = if ui_state.editor_id.is_some() {
        "编辑会话"
    } else {
        "新建会话"
    };
    ui.heading(title);
    ui.separator();

    // 在 `buf` 的可变借用块里碰不到 ui_state,先记在局部,出块后再写意图。
    let mut pick_key = false;
    {
        let buf = &mut ui_state.editor;
        egui::Grid::new("editor_form_grid")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label("名称");
                ui.text_edit_singleline(&mut buf.name);
                ui.end_row();

                ui.label("主机");
                ui.text_edit_singleline(&mut buf.host);
                ui.end_row();

                ui.label("端口");
                ui.text_edit_singleline(&mut buf.port);
                ui.end_row();

                ui.label("协议");
                egui::ComboBox::from_id_salt("session_editor_protocol")
                    .selected_text(match buf.protocol {
                        Protocol::Ssh => "ssh",
                        Protocol::Sftp => "sftp",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut buf.protocol, Protocol::Ssh, "ssh");
                        ui.selectable_value(&mut buf.protocol, Protocol::Sftp, "sftp");
                    });
                ui.end_row();

                ui.label("用户名");
                ui.text_edit_singleline(&mut buf.user);
                ui.end_row();

                ui.label("备注");
                ui.text_edit_singleline(&mut buf.note);
                ui.end_row();

                ui.label("分组");
                egui::ComboBox::from_id_salt("editor_group")
                    .selected_text(
                        buf.preserved_group_id
                            .and_then(|id| groups.iter().find(|g| g.id == id))
                            .map(|g| g.name.as_str())
                            .unwrap_or("未分组"),
                    )
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut buf.preserved_group_id, None, "未分组");
                        for g in groups {
                            ui.selectable_value(&mut buf.preserved_group_id, Some(g.id), &g.name);
                        }
                    });
                ui.end_row();

                ui.label("认证方式");
                egui::ComboBox::from_id_salt("session_editor_auth_kind")
                    .selected_text(match buf.auth_kind {
                        AuthKindUi::Password => "密码",
                        AuthKindUi::PublicKey => "公钥",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut buf.auth_kind, AuthKindUi::Password, "密码");
                        ui.selectable_value(&mut buf.auth_kind, AuthKindUi::PublicKey, "公钥");
                    });
                ui.end_row();

                match buf.auth_kind {
                    AuthKindUi::Password => {
                        ui.label("密码");
                        ui.add(egui::TextEdit::singleline(&mut buf.password).password(true));
                        ui.end_row();
                    }
                    AuthKindUi::PublicKey => {
                        ui.label("私钥路径");
                        ui.horizontal(|ui| {
                            ui.text_edit_singleline(&mut buf.key_path);
                            if ui.button("选择…").clicked() {
                                pick_key = true;
                            }
                        });
                        ui.end_row();

                        ui.label("私钥口令");
                        ui.add(egui::TextEdit::singleline(&mut buf.passphrase).password(true));
                        ui.end_row();
                    }
                }

                ui.label("代理");
                egui::ComboBox::from_id_salt("editor_proxy_mode")
                    .selected_text(match buf.proxy_mode {
                        ProxyModeUi::Inherit => "跟随分组",
                        ProxyModeUi::Direct => "不使用代理",
                        ProxyModeUi::Socks5 => "SOCKS5",
                        ProxyModeUi::HttpConnect => "HTTP CONNECT",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut buf.proxy_mode, ProxyModeUi::Inherit, "跟随分组");
                        ui.selectable_value(&mut buf.proxy_mode, ProxyModeUi::Direct, "不使用代理");
                        ui.selectable_value(&mut buf.proxy_mode, ProxyModeUi::Socks5, "SOCKS5");
                        ui.selectable_value(
                            &mut buf.proxy_mode,
                            ProxyModeUi::HttpConnect,
                            "HTTP CONNECT",
                        );
                    });
                ui.end_row();

                if matches!(
                    buf.proxy_mode,
                    ProxyModeUi::Socks5 | ProxyModeUi::HttpConnect
                ) {
                    ui.label("代理地址");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut buf.proxy_host).desired_width(160.0),
                        );
                        ui.label(":");
                        ui.add(egui::TextEdit::singleline(&mut buf.proxy_port).desired_width(60.0));
                    });
                    ui.end_row();

                    ui.label("代理用户名");
                    ui.text_edit_singleline(&mut buf.proxy_user);
                    ui.end_row();

                    ui.label("代理口令");
                    ui.add(egui::TextEdit::singleline(&mut buf.proxy_password).password(true));
                    ui.end_row();
                }

                ui.label("跳板");
                ui.vertical(|ui| {
                    if !buf.jump_set {
                        ui.horizontal(|ui| {
                            ui.label("跟随分组");
                            if ui.button("改为自定义").clicked() {
                                buf.jump_set = true;
                            }
                        });
                    } else {
                        let mut remove_at = None;
                        for (i, id) in buf.jump_chain.iter().enumerate() {
                            ui.horizontal(|ui| {
                                let name = sessions
                                    .iter()
                                    .find(|r| r.id == *id)
                                    .map(|r| r.identity.name.clone())
                                    // 悬空引用在 UI 上就点出来,不要等到连接时才报错。
                                    .unwrap_or_else(|| format!("<已删除的会话 {:?}>", id));
                                ui.label(format!("{}. {name}", i + 1));
                                if ui.button("移除").clicked() {
                                    remove_at = Some(i);
                                }
                            });
                        }
                        if let Some(i) = remove_at {
                            buf.jump_chain.remove(i);
                        }
                        egui::ComboBox::from_id_salt("editor_jump_add")
                            .selected_text("添加跳板…")
                            .show_ui(ui, |ui| {
                                for rec in sessions {
                                    // 不能把自己当自己的跳板(那是环)。
                                    if Some(rec.id) == editing_id {
                                        continue;
                                    }
                                    if ui.button(&rec.identity.name).clicked() {
                                        buf.jump_chain.push(rec.id);
                                    }
                                }
                            });
                        if ui.button("恢复为跟随分组").clicked() {
                            buf.jump_set = false;
                            buf.jump_chain.clear();
                        }
                    }
                });
                ui.end_row();
            });
    }
    if pick_key {
        ui_state.pick_key_request = true;
    }
    ui.label("留空密码 / 私钥口令 = 清除已存凭据(不是「保持不变」)。");

    if let Some(err) = &ui_state.last_error {
        ui.colored_label(theme::c32(t.danger), err);
    }

    ui.horizontal(|ui| {
        if ui.button("保存").clicked() {
            match build_draft(&ui_state.editor) {
                Ok(draft) => {
                    ui_state.last_error = None;
                    ui_state.save_request = Some(SaveIntent {
                        editing_id: ui_state.editor_id,
                        draft,
                    });
                    // 别让刚输入的明文密码/口令原样滞留在 UiState 内存里(复核 #6)。
                    ui_state.editor = EditorBuffer::default();
                    ui_state.editor_id = None;
                }
                Err(e) => ui_state.last_error = Some(e),
            }
        }
        if ui.button("取消").clicked() {
            ui_state.editor = EditorBuffer::default();
            ui_state.editor_id = None;
        }
    });
}
