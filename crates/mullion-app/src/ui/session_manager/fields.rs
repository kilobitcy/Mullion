//! 右栏三个 Tab 的字段布局。从 `editor.rs` 切出来是因为字段多、改动频繁,
//! 混在窗口骨架里会让 `editor.rs` 涨到读不动。

use egui::Ui;
use mullion_store::{GroupRecord, Protocol};

use crate::theme::Theme;
use crate::ui::session_manager::{AuthKindUi, EditorBuffer, ProxyModeUi, SecretPresence};

/// 两列表单的统一样式:左列标签定宽,右列输入撑满。
fn grid(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    egui::Grid::new(id)
        .num_columns(2)
        .spacing([12.0, 8.0])
        .min_col_width(88.0)
        .show(ui, add);
}

pub(super) fn basic(ui: &mut Ui, t: &Theme, buf: &mut EditorBuffer, groups: &[GroupRecord]) {
    let _ = t;
    grid(ui, "sm_basic", |ui| {
        ui.label("名称");
        ui.add(egui::TextEdit::singleline(&mut buf.name).desired_width(f32::INFINITY));
        ui.end_row();

        ui.label("主机");
        ui.add(egui::TextEdit::singleline(&mut buf.host).desired_width(f32::INFINITY));
        ui.end_row();

        ui.label("端口");
        ui.add(egui::TextEdit::singleline(&mut buf.port).desired_width(80.0));
        ui.end_row();

        ui.label("协议");
        egui::ComboBox::from_id_salt("sm_protocol")
            .selected_text(match buf.protocol {
                Protocol::Ssh => "ssh",
                Protocol::Sftp => "sftp",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut buf.protocol, Protocol::Ssh, "ssh");
                ui.selectable_value(&mut buf.protocol, Protocol::Sftp, "sftp");
            });
        ui.end_row();

        ui.label("分组");
        let current = buf
            .preserved_group_id
            .and_then(|gid| groups.iter().find(|g| g.id == gid))
            .map(|g| g.name.clone())
            .unwrap_or_else(|| "未分组".to_string());
        egui::ComboBox::from_id_salt("sm_group")
            .selected_text(current)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut buf.preserved_group_id, None, "未分组");
                for g in groups {
                    ui.selectable_value(&mut buf.preserved_group_id, Some(g.id), &g.name);
                }
            });
        ui.end_row();

        ui.label("备注");
        ui.add(egui::TextEdit::multiline(&mut buf.note).desired_rows(3));
        ui.end_row();
    });
}

pub(super) fn auth(ui: &mut Ui, t: &Theme, buf: &mut EditorBuffer, presence: SecretPresence) {
    grid(ui, "sm_auth", |ui| {
        ui.label("用户名");
        ui.add(egui::TextEdit::singleline(&mut buf.user).desired_width(f32::INFINITY));
        ui.end_row();

        ui.label("认证方式");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut buf.auth_kind, AuthKindUi::Password, "密码");
            ui.selectable_value(&mut buf.auth_kind, AuthKindUi::PublicKey, "公钥");
        });
        ui.end_row();

        match buf.auth_kind {
            AuthKindUi::Password => {
                ui.label("密码");
                super::secret_edit(
                    ui,
                    t,
                    "sm_password",
                    &mut buf.password,
                    &mut buf.password_touched,
                    presence.password,
                );
                ui.end_row();
            }
            AuthKindUi::PublicKey => {
                ui.label("私钥");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut buf.key_path));
                    if ui.button("浏览…").clicked() {
                        buf.pick_key_clicked = true;
                    }
                });
                ui.end_row();

                ui.label("私钥口令");
                super::secret_edit(
                    ui,
                    t,
                    "sm_passphrase",
                    &mut buf.passphrase,
                    &mut buf.passphrase_touched,
                    presence.passphrase,
                );
                ui.end_row();
            }
        }
    });
}

pub(super) fn network(ui: &mut Ui, t: &Theme, buf: &mut EditorBuffer, presence: SecretPresence) {
    grid(ui, "sm_network", |ui| {
        ui.label("代理");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut buf.proxy_mode, ProxyModeUi::Inherit, "继承分组");
            ui.selectable_value(&mut buf.proxy_mode, ProxyModeUi::Direct, "直连");
            ui.selectable_value(&mut buf.proxy_mode, ProxyModeUi::Socks5, "SOCKS5");
            ui.selectable_value(&mut buf.proxy_mode, ProxyModeUi::HttpConnect, "HTTP");
        });
        ui.end_row();

        if matches!(
            buf.proxy_mode,
            ProxyModeUi::Socks5 | ProxyModeUi::HttpConnect
        ) {
            ui.label("代理地址");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut buf.proxy_host));
                ui.add(egui::TextEdit::singleline(&mut buf.proxy_port).desired_width(70.0));
            });
            ui.end_row();

            ui.label("代理用户");
            ui.add(egui::TextEdit::singleline(&mut buf.proxy_user).desired_width(f32::INFINITY));
            ui.end_row();

            ui.label("代理口令");
            super::secret_edit(
                ui,
                t,
                "sm_proxy_password",
                &mut buf.proxy_password,
                &mut buf.proxy_password_touched,
                presence.proxy_password,
            );
            ui.end_row();
        }

        ui.label("跳板链");
        ui.vertical(|ui| {
            ui.checkbox(&mut buf.jump_set, "启用跳板");
            if buf.jump_set {
                ui.colored_label(
                    crate::theme::c32(t.fg_faint),
                    format!("已配置 {} 跳(在分组管理里编辑)", buf.jump_chain.len()),
                );
            }
        });
        ui.end_row();
    });
}
