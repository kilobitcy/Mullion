//! 会话管理弹窗:列表 + CRUD + 编辑表单(Task 6,§4.3/§1.2)。
//!
//! 关键约束:这里渲染在 `app.rs` 的 `egui_ctx.run(|ctx| ...)` 闭包内,只能拿到
//! `&mut UiState`,拿不到 `&mut SessionStore`(否则借用检查器过不了)。所以任何会
//! 改 store / 发起连接的动作,这里只写「意图」到 `UiState`,由 `app.rs` 在
//! `render_frame` 返回、借用释放之后统一施加——与既有 `request_disconnect`/
//! `request_quit` 完全同构。

use std::path::PathBuf;

use mullion_store::{
    AppearancePrefs, Auth, AuthKind, Connection, GroupId, Identity, Protocol, SecretEntry,
    SessionDraft, SessionId, SessionRecord, TerminalPrefs,
};

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

    // ↓↓↓ 透传字段:UI 目前没有编辑分组/标签/终端偏好/外观偏好的入口(那是
    // P0-b/P2 的事),但 `Vault::update` 对 `identity`/`terminal`/`appearance`
    // 是整体字段替换而非合并(见 vault.rs)。所以编辑表单必须把这些字段原样
    // 存下来再原样写回,否则「编辑会话」会静默清空它们。新建会话时没有
    // `SessionRecord` 可读,保持默认值(未分组/无标签/默认偏好)。
    pub preserved_group_id: Option<GroupId>,
    pub preserved_tags: Vec<String>,
    pub preserved_terminal: TerminalPrefs,
    pub preserved_appearance: AppearancePrefs,
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
            preserved_group_id: None,
            preserved_tags: Vec::new(),
            preserved_terminal: TerminalPrefs::default(),
            preserved_appearance: AppearancePrefs::default(),
        }
    }
}

impl EditorBuffer {
    /// 把已有会话的非敏感字段填入表单(密码/口令 store 不会明文回吐,留空 ——
    /// 编辑时留空 = 不改;见 `build_draft` 的说明)。
    fn from_record(rec: &SessionRecord) -> Self {
        let mut buf = Self {
            name: rec.identity.name.clone(),
            host: rec.connection.host.clone(),
            port: rec.connection.port.to_string(),
            protocol: rec.connection.protocol,
            user: rec.auth.user.clone(),
            note: rec.identity.note.clone(),
            preserved_group_id: rec.identity.group_id,
            preserved_tags: rec.identity.tags.clone(),
            preserved_terminal: rec.terminal.clone(),
            preserved_appearance: rec.appearance.clone(),
            ..Self::default()
        };
        match &rec.auth.kind {
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
        identity: Identity {
            name: buf.name.trim().to_string(),
            // note 不 trim:用户备注里的前后空格属于用户数据(既有行为)。
            note: buf.note.clone(),
            group_id: buf.preserved_group_id,
            tags: buf.preserved_tags.clone(),
        },
        connection: Connection {
            host: buf.host.trim().to_string(),
            port,
            protocol: buf.protocol,
        },
        auth: Auth {
            user: buf.user.trim().to_string(),
            kind: auth,
        },
        terminal: buf.preserved_terminal.clone(),
        appearance: buf.preserved_appearance.clone(),
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
                    .map(|s| s.identity.name.clone())
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
    use mullion_store::{ColorSpec, ColorTarget, IconKind, IconSpec};

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
            ..EditorBuffer::default()
        }
    }

    #[test]
    fn password_session_builds_draft_with_secret() {
        let mut b = buf();
        b.password = "pw".into();
        let draft = build_draft(&b).unwrap();
        assert_eq!(draft.identity.name, "dev");
        assert_eq!(draft.connection.host, "192.0.2.10");
        assert_eq!(draft.connection.port, 22);
        assert!(matches!(draft.auth.kind, AuthKind::Password));
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
        match draft.auth.kind {
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
        match draft.auth.kind {
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

    /// 回归测试(critical):编辑表单编辑不到的字段(分组/标签/终端偏好/外观偏好)
    /// 在「读入表单 → 写回 draft」这趟往返里必须原样保留,不能被表单的占位默认值
    /// 悄悄清空。`Vault::update` 对这四项是整体替换而非合并,一旦 build_draft 填了
    /// 默认值,保存就会真的把用户数据清空。
    #[test]
    fn editing_a_session_preserves_fields_the_form_cannot_edit() {
        let rec = SessionRecord {
            id: SessionId(7),
            modified_at: "2026-07-25T00:00:00Z".into(),
            identity: Identity {
                name: "dev".into(),
                note: "跳板后".into(),
                group_id: Some(GroupId(1)),
                tags: vec!["web01".into()],
            },
            connection: Connection {
                host: "192.0.2.10".into(),
                port: 22,
                protocol: Protocol::Ssh,
            },
            auth: Auth {
                user: "user".into(),
                kind: AuthKind::Password,
            },
            terminal: TerminalPrefs {
                scrollback: Some(12345),
            },
            appearance: AppearancePrefs {
                icon: Some(IconSpec {
                    kind: IconKind::Emoji,
                    value: "🚀".into(),
                }),
                color: Some(ColorSpec {
                    hex: "#ff0000".into(),
                    apply_to: vec![ColorTarget::Tab],
                }),
            },
        };

        let editor_buf = EditorBuffer::from_record(&rec);
        let draft = build_draft(&editor_buf).unwrap();

        assert_eq!(
            draft.identity.group_id, rec.identity.group_id,
            "编辑不该清空 UI 编辑不到的字段:group_id"
        );
        assert_eq!(
            draft.identity.tags, rec.identity.tags,
            "编辑不该清空 UI 编辑不到的字段:tags"
        );
        assert_eq!(
            draft.terminal, rec.terminal,
            "编辑不该清空 UI 编辑不到的字段:terminal"
        );
        assert_eq!(
            draft.appearance, rec.appearance,
            "编辑不该清空 UI 编辑不到的字段:appearance"
        );
    }

    /// 钉死 note 不被 trim(既有行为:旧代码是 `buf.note.clone()`,迁移不该顺手改成
    /// `trim()`——用户备注里的前后空格属于用户数据,不该被悄悄吃掉)。
    #[test]
    fn note_is_not_trimmed_when_building_draft() {
        let mut b = buf();
        b.note = "  缩进备注  ".into();
        let draft = build_draft(&b).unwrap();
        assert_eq!(
            draft.identity.note, "  缩进备注  ",
            "note 不应被 trim,前后空格属于用户数据"
        );
    }

    /// 新建会话(没有 SessionRecord 可读)时,这四项仍应是默认值——确认
    /// `EditorBuffer::default()` 的新建路径没被这次修复带偏。
    #[test]
    fn new_session_defaults_have_no_preserved_fields() {
        let b = EditorBuffer::default();
        let draft = build_draft(&b).unwrap();
        assert_eq!(draft.identity.group_id, None);
        assert_eq!(draft.identity.tags, Vec::<String>::new());
        assert_eq!(draft.terminal, TerminalPrefs::default());
        assert_eq!(draft.appearance, AppearancePrefs::default());
    }
}
