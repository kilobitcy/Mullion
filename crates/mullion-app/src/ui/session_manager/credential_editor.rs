//! 凭据编辑器:右栏在「凭据」模式下的表单(F74)。
//!
//! 与隧道编辑器同构 —— UI 只写意图到 `UiState`,由 `app.rs` 在借用释放后施加。
//!
//! **表单上没有主机、没有代理**:凭据只回答「以谁的身份」,不回答「连到哪」
//! (设计 D2)。代理口令留在会话自己的密文里(D4)。

use mullion_store::{AuthKind, CredentialDraft, CredentialId, CredentialRecord, SessionRecord};

use crate::theme::{self, Theme};
use crate::ui::metrics::{field_w, FIELD_W_M};
use crate::ui::session_manager::form;
use crate::ui::session_manager::{AuthKindUi, SecretField, SecretPresence};
use crate::ui::UiState;

/// 凭据表单的跨帧缓冲。
///
/// 三个密文框与会话表单同一套约定:**编辑已有凭据时恒为空**(store 不回吐
/// 明文),`*_touched` 才是「用户真的改了」的判据。少了这一位,打开一份凭据
/// 点保存就会把密码清空。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CredentialEditorBuffer {
    pub name: String,
    pub user: String,
    pub auth_kind: AuthKindUi,
    pub password: String,
    pub password_touched: bool,
    pub passphrase: String,
    pub passphrase_touched: bool,
    /// 私钥正文(v5 起私钥进库,不存路径)。
    pub key_data: String,
    pub key_touched: bool,
    /// 「导入…」被点了 → app 事后另起线程开系统文件对话框。
    pub pick_key_clicked: bool,
    /// 导入/清除留下的一行提示,由 `mod.rs` 抽走转成通知。
    pub key_note: Option<String>,
}

impl CredentialEditorBuffer {
    /// 从已有记录回填。密文一律留空 —— store 不回吐明文。
    pub fn from_record(rec: &CredentialRecord) -> Self {
        Self {
            name: rec.name.clone(),
            user: rec.user.clone(),
            auth_kind: match rec.kind {
                AuthKind::Password => AuthKindUi::Password,
                AuthKind::PublicKey { .. } => AuthKindUi::PublicKey,
            },
            ..Self::default()
        }
    }
}

/// 一次「保存凭据」的意图。`editing_id = None` 即新建。
///
/// 三个密文走 `SecretField` 三态而不是 `Option<String>`:二态分不出
/// 「没改」和「清空」,编辑已有凭据时密码框恒为空,二态会把它读成「清空」。
pub struct CredentialSaveIntent {
    pub editing_id: Option<CredentialId>,
    pub draft: CredentialDraft,
    pub password: SecretField,
    pub passphrase: SecretField,
    pub private_key: SecretField,
}

/// 缓冲 → `CredentialDraft`。纯函数,不碰 egui。
///
/// `draft.secret` 这里一律留 `None`:真正的密文由 `app.rs` 用三态与库里
/// 已存的那份合成(同会话侧 `apply_save`)。
pub fn build_credential_draft(buf: &CredentialEditorBuffer) -> Result<CredentialDraft, String> {
    let name = buf.name.trim();
    if name.is_empty() {
        return Err("凭据名称不能为空".into());
    }
    let user = buf.user.trim();
    if user.is_empty() {
        return Err("用户名不能为空".into());
    }
    Ok(CredentialDraft {
        name: name.to_string(),
        user: user.to_string(),
        kind: match buf.auth_kind {
            AuthKindUi::Password => AuthKind::Password,
            // `has_passphrase` 由 `app.rs` 按合成后的密文校正 —— 表单当前
            // 内容说了不算(编辑时口令框恒为空,跟着它走会把 true 写成 false,
            // 下次连接时 russh 拿到加密私钥却不知道要口令)。
            AuthKindUi::PublicKey => AuthKind::PublicKey {
                has_passphrase: false,
            },
        },
        secret: None,
    })
}

/// 三个密文框各自的三态意图。纯函数。
///
/// 当前认证方式用不到的那一支走 `Clear`:密码认证的凭据不该在 secrets.enc
/// 里留一条孤儿私钥(同会话侧 `secret_fields`)。
pub fn credential_secret_fields(
    buf: &CredentialEditorBuffer,
) -> (SecretField, SecretField, SecretField) {
    fn field(touched: bool, v: &str) -> SecretField {
        if !touched {
            SecretField::Keep
        } else if v.is_empty() {
            SecretField::Clear
        } else {
            SecretField::Set(v.to_string())
        }
    }
    match buf.auth_kind {
        AuthKindUi::Password => (
            field(buf.password_touched, &buf.password),
            SecretField::Clear,
            SecretField::Clear,
        ),
        AuthKindUi::PublicKey => (
            SecretField::Clear,
            field(buf.passphrase_touched, &buf.passphrase),
            field(buf.key_touched, &buf.key_data),
        ),
    }
}

/// 把选中的私钥文件读进**凭据**缓冲。判定与措辞跟会话侧同源
/// (`buffer::read_key_file`),IO 由调用方注入,可脱离 GUI 单测。
pub fn import_credential_key_file(
    buf: &mut CredentialEditorBuffer,
    path: &std::path::Path,
    read: impl FnOnce(&std::path::Path) -> std::io::Result<String>,
) {
    match super::buffer::read_key_file(path, read) {
        Ok((text, note)) => {
            buf.key_data = text;
            buf.key_touched = true;
            buf.key_note = Some(note);
        }
        Err(note) => buf.key_note = Some(note),
    }
}

/// 引用这份凭据的会话名。空 = 没人在用,可以删。
///
/// 在 UI 侧直接算而不是等 store 报错:删除按钮要**当场**置灰并说清是谁在用,
/// 让用户点一下才收到一句「删不了」等于把排查推后一步。store 侧的
/// `CredentialInUse` 仍是最后一道防线(别的入口、别的时序)。
pub fn users_of(id: CredentialId, sessions: &[SessionRecord]) -> Vec<String> {
    sessions
        .iter()
        .filter(|s| s.auth.credential_id() == Some(id))
        .map(|s| s.identity.name.clone())
        .collect()
}

/// 「N 个会话在用」那句红字。`None` = 没人用。
pub fn in_use_message(users: &[String]) -> Option<String> {
    if users.is_empty() {
        return None;
    }
    Some(format!(
        "{} 个会话在用:{} —— 先把它们改成别的凭据或「本会话独有」",
        users.len(),
        users.join("、")
    ))
}

pub(super) fn show(
    ui: &mut egui::Ui,
    t: &Theme,
    ui_state: &mut UiState,
    sessions: &[SessionRecord],
    presence: SecretPresence,
) {
    let editing_id = ui_state.credential_editor_id;
    let Some(buf) = ui_state.credential_editor.as_mut() else {
        ui.colored_label(
            theme::c32(t.fg_dimmer),
            "从左边选一份凭据,或点「+ 新建凭据」。",
        );
        return;
    };

    let mut first = true;
    form::section(ui, t, "会话管理器/右栏", "身份", &mut first);
    form::grid(ui, "cred_identity", |ui| {
        form::required(ui, t, "名称");
        ui.add(
            egui::TextEdit::singleline(&mut buf.name)
                .desired_width(field_w(ui.available_width(), FIELD_W_M, 0.0))
                .hint_text(theme::hint_text(t, "这份凭据叫什么(如「运维号」)")),
        );
        ui.end_row();
        form::field_error(ui, t, buf.name.trim().is_empty(), "凭据名称不能为空");

        form::required(ui, t, "用户名");
        ui.add(
            egui::TextEdit::singleline(&mut buf.user).desired_width(field_w(
                ui.available_width(),
                FIELD_W_M,
                0.0,
            )),
        );
        ui.end_row();
        form::field_error(ui, t, buf.user.trim().is_empty(), "用户名不能为空");

        ui.label("认证方式");
        ui.horizontal(|ui| {
            let vis = &mut ui.visuals_mut().selection;
            vis.bg_fill = theme::c32(t.accent).linear_multiply(0.35);
            ui.selectable_value(&mut buf.auth_kind, AuthKindUi::Password, "密码");
            ui.selectable_value(&mut buf.auth_kind, AuthKindUi::PublicKey, "公钥");
        });
        ui.end_row();
    });

    form::section(ui, t, "会话管理器/右栏", "凭据", &mut first);
    form::grid(ui, "cred_secret", |ui| match buf.auth_kind {
        AuthKindUi::Password => {
            ui.label("密码");
            super::secret_edit(
                ui,
                t,
                "cred_password",
                &mut buf.password,
                &mut buf.password_touched,
                presence.password,
                "未设置",
            );
            ui.end_row();
        }
        AuthKindUi::PublicKey => {
            ui.label("私钥");
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("导入…").clicked() {
                        buf.pick_key_clicked = true;
                    }
                    let has_key =
                        presence.private_key || (buf.key_touched && !buf.key_data.is_empty());
                    if has_key && ui.button("清除").clicked() {
                        buf.key_data.clear();
                        buf.key_touched = true;
                        buf.key_note = Some("已清除私钥(保存后生效)".into());
                    }
                    // 只报状态,不显示正文。判据与会话侧那段同构。
                    let (text, color) = if buf.key_touched && !buf.key_data.is_empty() {
                        ("已导入(未保存)", t.fg)
                    } else if buf.key_touched {
                        ("已清除(未保存)", t.danger_text)
                    } else if presence.private_key {
                        ("已导入", t.fg)
                    } else {
                        ("未设置 —— 请导入私钥文件", t.danger_text)
                    };
                    ui.colored_label(theme::c32(color), text);
                });
            });
            ui.end_row();

            ui.label("私钥口令");
            super::secret_edit(
                ui,
                t,
                "cred_passphrase",
                &mut buf.passphrase,
                &mut buf.passphrase_touched,
                presence.passphrase,
                "留空表示无口令",
            );
            ui.end_row();
        }
    });

    ui.add_space(crate::ui::metrics::SP_XS);
    ui.label(
        egui::RichText::new(super::fields::SECRET_STORAGE_NOTE)
            .size(11.0)
            .color(theme::c32(t.fg_muted)),
    );

    // 引用者。**编辑已有凭据时才有意义** —— 新建的还没人能引用。
    let in_use = editing_id
        .map(|id| users_of(id, sessions))
        .unwrap_or_default();
    let blocked = in_use_message(&in_use);

    ui.add_space(crate::ui::metrics::SP_M);
    ui.horizontal(|ui| {
        if ui.button("保存").clicked() {
            ui_state.credential_save_click = true;
        }
        if editing_id.is_some() {
            // 被引用时**真禁用**,不是画成灰的:「看着灰」和「点不动」必须是
            // 同一件事(同隧道那条启动按钮)。
            let resp = ui
                .add_enabled(blocked.is_none(), egui::Button::new("删除"))
                .on_disabled_hover_text("这份凭据还有会话在用");
            if resp.clicked() {
                ui_state.pending_credential_delete = editing_id;
            }
        }
    });

    // 红字列在按钮下面而不是 tooltip 里:tooltip 要悬停才看得到,而「谁在用」
    // 正是用户接下来要去改的清单。
    if let Some(msg) = blocked {
        ui.colored_label(theme::c32(t.danger_text), msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_store::{Auth, Connection, Identity, NetworkPrefs, Protocol, SessionId};

    fn session(id: u64, name: &str, auth: Auth) -> SessionRecord {
        SessionRecord {
            id: SessionId(id),
            modified_at: "t".into(),
            identity: Identity {
                name: name.into(),
                note: String::new(),
                group_id: None,
                tags: Vec::new(),
            },
            connection: Connection {
                host: "h".into(),
                port: 22,
                protocol: Protocol::Ssh,
            },
            auth,
            terminal: Default::default(),
            appearance: Default::default(),
            network: NetworkPrefs::default(),
            automation: Default::default(),
            sftp: Default::default(),
        }
    }

    /// F74/D7:删除要拦下来,而且要说清**是谁**在用 —— 只说「有会话在用」
    /// 等于让用户挨个点开会话去找。
    #[test]
    fn the_in_use_message_names_every_referencing_session() {
        let sessions = vec![
            session(1, "web01", Auth::Ref(CredentialId(7))),
            session(2, "db02", Auth::Ref(CredentialId(7))),
            // 引用的是别的凭据,不该被算进来。
            session(3, "无关", Auth::Ref(CredentialId(8))),
            // 自带认证的会话同样不算。
            session(4, "独立", Auth::inline("ops", AuthKind::Password)),
        ];
        let users = users_of(CredentialId(7), &sessions);
        assert_eq!(users, vec!["web01".to_string(), "db02".to_string()]);

        let msg = in_use_message(&users).expect("有人在用就该拦");
        assert!(msg.contains("web01") && msg.contains("db02"), "实际:{msg}");
        assert!(!msg.contains("无关"), "别的凭据的引用者不该混进来:{msg}");
    }

    /// 没人引用 → 不拦。恒拦的话凭据就成了一份删不掉的垃圾。
    #[test]
    fn nothing_blocks_deleting_an_unreferenced_credential() {
        let sessions = vec![session(1, "web01", Auth::inline("ops", AuthKind::Password))];
        assert!(in_use_message(&users_of(CredentialId(7), &sessions)).is_none());
    }

    /// 编辑已有凭据时三个密文框恒为空 —— 没碰过就必须是 `Keep`,
    /// 否则打开一份凭据点一下保存,库里的密码就没了。
    #[test]
    fn untouched_secret_fields_keep_whatever_is_already_stored() {
        let buf = CredentialEditorBuffer {
            name: "运维号".into(),
            user: "ops".into(),
            auth_kind: AuthKindUi::Password,
            ..Default::default()
        };
        let (password, passphrase, private_key) = credential_secret_fields(&buf);
        assert_eq!(password, SecretField::Keep, "没碰过的密码必须原样留着");
        // 密码认证用不到的两支走 Clear,不留孤儿密文。
        assert_eq!(passphrase, SecretField::Clear);
        assert_eq!(private_key, SecretField::Clear);
    }

    #[test]
    fn a_credential_needs_both_a_name_and_a_user() {
        let ok = CredentialEditorBuffer {
            name: "运维号".into(),
            user: "ops".into(),
            ..Default::default()
        };
        assert!(build_credential_draft(&ok).is_ok());

        for (name, user) in [("", "ops"), ("  ", "ops"), ("运维号", ""), ("运维号", " ")] {
            let buf = CredentialEditorBuffer {
                name: name.into(),
                user: user.into(),
                ..Default::default()
            };
            assert!(
                build_credential_draft(&buf).is_err(),
                "名称={name:?} 用户名={user:?} 本该被拦下"
            );
        }
    }
}
