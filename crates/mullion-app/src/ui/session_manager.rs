//! 会话管理弹窗:列表 + CRUD + 编辑表单(Task 6,§4.3/§1.2)。
//!
//! 关键约束:这里渲染在 `app.rs` 的 `egui_ctx.run(|ctx| ...)` 闭包内,只能拿到
//! `&mut UiState`,拿不到 `&mut SessionStore`(否则借用检查器过不了)。所以任何会
//! 改 store / 发起连接的动作,这里只写「意图」到 `UiState`,由 `app.rs` 在
//! `render_frame` 返回、借用释放之后统一施加——与既有 `request_disconnect`/
//! `request_quit` 完全同构。

use std::path::PathBuf;

use mullion_store::{AuthKind, Protocol, SecretEntry, SessionDraft, SessionId, SessionRecord};

use super::UiState;

/// 编辑表单里认证方式的选择。不复用 `AuthKind` 本身,因为 UI 在密码/公钥两种模式
/// 间切换时要各自保留自己的缓冲(密码框内容、私钥路径都不该因切换选项就丢)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthKindUi {
    Password,
    PublicKey,
}

/// 编辑表单的跨帧字段缓冲。端口用 `String`(保存时才 `parse`),密码/口令是明文
/// 缓冲(仅存在于本进程内存,保存后随 `SaveIntent` 一次性转移给 store 加密)。
#[derive(Clone, Debug)]
pub struct EditorBuffer {
    pub name: String,
    pub host: String,
    pub port: String,
    pub protocol: Protocol,
    pub user: String,
    pub note: String,
    pub auth_kind: AuthKindUi,
    pub password: String,
    pub key_path: String,
    pub passphrase: String,
}

impl Default for EditorBuffer {
    fn default() -> Self {
        Self {
            name: String::new(),
            host: String::new(),
            port: "22".to_string(),
            protocol: Protocol::Ssh,
            user: String::new(),
            note: String::new(),
            auth_kind: AuthKindUi::Password,
            password: String::new(),
            key_path: String::new(),
            passphrase: String::new(),
        }
    }
}

impl EditorBuffer {
    /// 把已有会话的非敏感字段填入表单(密码/口令 store 不会明文回吐,留空 ——
    /// 编辑时留空 = 不改;见 `build_draft` 的说明)。
    fn from_record(rec: &SessionRecord) -> Self {
        let mut buf = Self {
            name: rec.name.clone(),
            host: rec.host.clone(),
            port: rec.port.to_string(),
            protocol: rec.protocol,
            user: rec.user.clone(),
            note: rec.note.clone(),
            ..Self::default()
        };
        match &rec.auth {
            AuthKind::Password => buf.auth_kind = AuthKindUi::Password,
            AuthKind::PublicKey { path, .. } => {
                buf.auth_kind = AuthKindUi::PublicKey;
                buf.key_path = path.display().to_string();
            }
        }
        buf
    }
}

/// 一次「保存」的意图:app 事后据此调用 `store.add`(`editing_id=None`)或
/// `store.update`(`Some(id)`)。
pub struct SaveIntent {
    pub editing_id: Option<SessionId>,
    pub draft: SessionDraft,
}

/// 表单缓冲 → `SessionDraft`。纯函数,不碰 egui,可脱离 GUI 单测。
///
/// 密码认证:密码框留空 → `secret=None`(= 清除已存凭据,留空的语义在 UI 上有提示)。
/// 公钥认证:`has_passphrase` 由口令框是否非空决定;口令非空才带 `secret`。
pub(crate) fn build_draft(buf: &EditorBuffer) -> Result<SessionDraft, String> {
    let port: u16 = buf
        .port
        .trim()
        .parse()
        .map_err(|_| "端口非法,须为 1-65535 的整数".to_string())?;
    let (auth, secret) = match buf.auth_kind {
        AuthKindUi::Password => {
            let secret = if buf.password.is_empty() {
                None
            } else {
                Some(SecretEntry {
                    password: Some(buf.password.clone()),
                    passphrase: None,
                })
            };
            (AuthKind::Password, secret)
        }
        AuthKindUi::PublicKey => {
            let has_passphrase = !buf.passphrase.is_empty();
            let secret = if has_passphrase {
                Some(SecretEntry {
                    password: None,
                    passphrase: Some(buf.passphrase.clone()),
                })
            } else {
                None
            };
            (
                AuthKind::PublicKey {
                    path: PathBuf::from(buf.key_path.trim()),
                    has_passphrase,
                },
                secret,
            )
        }
    };
    Ok(SessionDraft {
        name: buf.name.trim().to_string(),
        host: buf.host.trim().to_string(),
        port,
        protocol: buf.protocol,
        user: buf.user.trim().to_string(),
        note: buf.note.clone(),
        auth,
        secret,
    })
}

/// 会话管理器弹窗:列表 + CRUD 按钮(+ 内嵌二次确认)。`store_available=false`
/// 时(待定 G:keyring/库打开失败)不崩,只展示 `last_error` 或兜底提示。
pub fn show(
    ctx: &egui::Context,
    ui_state: &mut UiState,
    sessions: &[SessionRecord],
    store_available: bool,
) {
    let mut open = ui_state.session_manager_open;
    egui::Window::new("会话管理器")
        .open(&mut open)
        .default_width(560.0)
        .show(ctx, |ui| {
            if !store_available {
                let msg = ui_state
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "会话功能不可用".to_string());
                ui.colored_label(egui::Color32::RED, msg);
                return;
            }

            egui::Grid::new("session_list_grid")
                .num_columns(5)
                .striped(true)
                .show(ui, |ui| {
                    ui.strong("名称");
                    ui.strong("主机:端口");
                    ui.strong("协议");
                    ui.strong("用户");
                    ui.strong("修改时间");
                    ui.end_row();
                    for rec in sessions {
                        let is_selected = ui_state.selected == Some(rec.id);
                        let name_resp = ui.selectable_label(is_selected, &rec.name);
                        if name_resp.clicked() {
                            ui_state.selected = Some(rec.id);
                        }
                        if name_resp.double_clicked() {
                            ui_state.connect_request = Some(rec.id);
                        }
                        ui.label(format!("{}:{}", rec.host, rec.port));
                        ui.label(match rec.protocol {
                            Protocol::Ssh => "ssh",
                            Protocol::Sftp => "sftp",
                        });
                        ui.label(&rec.user);
                        ui.label(&rec.modified_at);
                        ui.end_row();
                    }
                });

            ui.separator();
            // 双击/点连接失败(如 sftp 会话映射拒绝)、保存/删除失败都写
            // ui_state.last_error;这里必须总是渲染,否则「点了没反应」(复核 #2)。
            if let Some(err) = &ui_state.last_error {
                ui.colored_label(egui::Color32::RED, err);
            }
            ui.horizontal(|ui| {
                if ui.button("新建").clicked() {
                    ui_state.editor_id = None;
                    ui_state.editor = EditorBuffer::default();
                    ui_state.editor_open = true;
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
                        ui_state.editor_open = true;
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

            // 删除二次确认:内嵌一个独立小窗口,确认/取消都不直接碰 store。
            if let Some(id) = ui_state.pending_delete {
                let name = sessions
                    .iter()
                    .find(|s| s.id == id)
                    .map(|s| s.name.clone())
                    .unwrap_or_default();
                egui::Window::new("确认删除")
                    .collapsible(false)
                    .resizable(false)
                    .show(ui.ctx(), |ui| {
                        ui.label(format!("确定删除会话「{name}」?此操作不可撤销。"));
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
                    });
            }
        });
    ui_state.session_manager_open = open;
    if !open {
        // 列表主窗被关掉:编辑子表单不该变成没有父窗的孤儿窗(复核 #5)。
        ui_state.editor_open = false;
    }

    if ui_state.editor_open {
        show_editor(ctx, ui_state);
    }
}

/// 新建/编辑子表单。保存时把缓冲组装成 `SaveIntent` 写进 `ui_state.save_request`,
/// 不在这里直接碰 store。
fn show_editor(ctx: &egui::Context, ui_state: &mut UiState) {
    let mut open = ui_state.editor_open;
    let title = if ui_state.editor_id.is_some() {
        "编辑会话"
    } else {
        "新建会话"
    };
    egui::Window::new(title)
        .open(&mut open)
        .collapsible(false)
        .show(ctx, |ui| {
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

                        ui.label("认证方式");
                        egui::ComboBox::from_id_salt("session_editor_auth_kind")
                            .selected_text(match buf.auth_kind {
                                AuthKindUi::Password => "密码",
                                AuthKindUi::PublicKey => "公钥",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut buf.auth_kind,
                                    AuthKindUi::Password,
                                    "密码",
                                );
                                ui.selectable_value(
                                    &mut buf.auth_kind,
                                    AuthKindUi::PublicKey,
                                    "公钥",
                                );
                            });
                        ui.end_row();

                        match buf.auth_kind {
                            AuthKindUi::Password => {
                                ui.label("密码");
                                ui.add(
                                    egui::TextEdit::singleline(&mut buf.password).password(true),
                                );
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
                                ui.add(
                                    egui::TextEdit::singleline(&mut buf.passphrase).password(true),
                                );
                                ui.end_row();
                            }
                        }
                    });
            }
            if pick_key {
                ui_state.pick_key_request = true;
            }
            ui.label("留空密码 / 私钥口令 = 清除已存凭据(不是「保持不变」)。");

            if let Some(err) = &ui_state.last_error {
                ui.colored_label(egui::Color32::RED, err);
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
                            ui_state.editor_open = false;
                            // 别让刚输入的明文密码/口令原样滞留在 UiState 内存里(复核 #6)。
                            ui_state.editor = EditorBuffer::default();
                        }
                        Err(e) => ui_state.last_error = Some(e),
                    }
                }
                if ui.button("取消").clicked() {
                    ui_state.editor_open = false;
                    ui_state.editor = EditorBuffer::default();
                }
            });
        });
    ui_state.editor_open = ui_state.editor_open && open;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf() -> EditorBuffer {
        EditorBuffer {
            name: "dev".into(),
            host: "192.0.2.10".into(),
            port: "22".into(),
            protocol: Protocol::Ssh,
            user: "user".into(),
            note: "跳板后".into(),
            auth_kind: AuthKindUi::Password,
            password: String::new(),
            key_path: String::new(),
            passphrase: String::new(),
        }
    }

    #[test]
    fn password_session_builds_draft_with_secret() {
        let mut b = buf();
        b.password = "pw".into();
        let draft = build_draft(&b).unwrap();
        assert_eq!(draft.name, "dev");
        assert_eq!(draft.host, "192.0.2.10");
        assert_eq!(draft.port, 22);
        assert!(matches!(draft.auth, AuthKind::Password));
        assert_eq!(
            draft.secret.as_ref().and_then(|s| s.password.clone()),
            Some("pw".to_string())
        );
    }

    #[test]
    fn password_session_with_empty_password_clears_secret() {
        let b = buf(); // password 留空
        let draft = build_draft(&b).unwrap();
        assert!(draft.secret.is_none(), "留空密码应清除已存凭据");
    }

    #[test]
    fn pubkey_with_passphrase_sets_has_passphrase_and_secret() {
        let mut b = buf();
        b.auth_kind = AuthKindUi::PublicKey;
        b.key_path = "/home/me/id_ed25519".into();
        b.passphrase = "ph".into();
        let draft = build_draft(&b).unwrap();
        match draft.auth {
            AuthKind::PublicKey {
                path,
                has_passphrase,
            } => {
                assert_eq!(path, PathBuf::from("/home/me/id_ed25519"));
                assert!(has_passphrase);
            }
            _ => panic!("应为 PublicKey"),
        }
        assert_eq!(
            draft.secret.as_ref().and_then(|s| s.passphrase.clone()),
            Some("ph".to_string())
        );
    }

    #[test]
    fn pubkey_without_passphrase_has_no_secret() {
        let mut b = buf();
        b.auth_kind = AuthKindUi::PublicKey;
        b.key_path = "/home/me/id_ed25519".into();
        let draft = build_draft(&b).unwrap();
        match draft.auth {
            AuthKind::PublicKey { has_passphrase, .. } => assert!(!has_passphrase),
            _ => panic!("应为 PublicKey"),
        }
        assert!(draft.secret.is_none());
    }

    #[test]
    fn invalid_port_is_rejected() {
        let mut b = buf();
        b.port = "not-a-port".into();
        assert!(build_draft(&b).is_err());

        let mut b2 = buf();
        b2.port = "99999999".into(); // 超出 u16 范围
        assert!(build_draft(&b2).is_err());
    }
}
