//! 凭据模式的左栏(F74)。
//!
//! 与会话列表**不共用**渲染:凭据没有分组、没有图标、没有连接状态,
//! 套三档密度那套只会把「一行说清一份身份」复杂化(同隧道档 D11)。

use egui::Ui;
use mullion_store::{AuthKind, CredentialId, CredentialRecord, SessionRecord};

use crate::theme::{self, Theme};
use crate::ui::annotate;

use super::UiState;

/// 「+ 新建凭据」按钮的显式 id。理由同 `tunnel_list::new_button_id()` ——
/// 守护测试要用 `Context::read_response` 读回真实矩形。
pub(crate) fn new_button_id() -> egui::Id {
    egui::Id::new("mullion_credential_new_button")
}

/// 一行的副标题:`ops · 密码`。
pub(crate) fn row_subtitle(rec: &CredentialRecord) -> String {
    let kind = match rec.kind {
        AuthKind::Password => "密码",
        AuthKind::PublicKey { .. } => "公钥",
    };
    format!("{} · {}", rec.user, kind)
}

/// 一行的第三行:有多少条会话在用。**没人用时写「未被引用」而不是留空** ——
/// 空白读起来像「还没算出来」,而「有没有人在用」正是删之前要知道的事。
pub(crate) fn usage_label(id: CredentialId, sessions: &[SessionRecord]) -> String {
    let n = sessions
        .iter()
        .filter(|s| s.auth.credential_id() == Some(id))
        .count();
    if n == 0 {
        "未被引用".to_string()
    } else {
        format!("{n} 个会话在用")
    }
}

/// 稳定顺序:按 `CredentialId` 升序。存储顺序会随增删漂移,直接拿来渲染
/// 会让列表在用户眼皮底下重排(同 `tunnel_list::visible_order`)。
pub(crate) fn visible_order(credentials: &[CredentialRecord]) -> Vec<CredentialId> {
    let mut ids: Vec<CredentialId> = credentials.iter().map(|c| c.id).collect();
    ids.sort();
    ids
}

pub(super) fn show(
    ui: &mut Ui,
    t: &Theme,
    ui_state: &mut UiState,
    credentials: &[CredentialRecord],
    sessions: &[SessionRecord],
) {
    // 底部「+ 新建」先占位,理由同 `tunnel_list::show` —— 面板先分配高度,
    // 下面的 `ScrollArea` 才吃得到真实剩余高度。
    egui::TopBottomPanel::bottom(ui.id().with("credential_list_bottom"))
        .frame(egui::Frame::none())
        .show_separator_line(false)
        .show_inside(ui, |ui| {
            ui.separator();
            ui.horizontal(|ui| {
                let b = new_button(ui);
                annotate::mark(ui.ctx(), "会话管理器/左栏/新建凭据按钮", b.rect);
                if b.clicked() {
                    ui_state.credential_editor_id = None;
                    let fresh = super::CredentialEditorBuffer::default();
                    ui_state.credential_editor_baseline = Some(fresh.clone());
                    ui_state.credential_editor = Some(fresh);
                }
            });
        });

    if credentials.is_empty() {
        ui.add_space(crate::ui::metrics::SP_XS);
        ui.colored_label(theme::c32(t.fg_dimmer), "还没有共享凭据");
        ui.colored_label(
            theme::c32(t.fg_dimmer),
            "一份凭据可被多条会话引用,换密钥只改一处。",
        );
        return;
    }

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for id in visible_order(credentials) {
                let Some(rec) = credentials.iter().find(|c| c.id == id) else {
                    continue;
                };
                row(ui, t, ui_state, rec, sessions);
            }
        });
}

fn row(
    ui: &mut Ui,
    t: &Theme,
    ui_state: &mut UiState,
    rec: &CredentialRecord,
    sessions: &[SessionRecord],
) {
    let selected = ui_state.credential_editor_id == Some(rec.id);
    ui.vertical(|ui| {
        let resp = ui.add(egui::SelectableLabel::new(selected, rec.name.clone()));
        if resp.clicked() {
            ui_state.credential_editor_id = Some(rec.id);
            let buf = super::CredentialEditorBuffer::from_record(rec);
            ui_state.credential_editor_baseline = Some(buf.clone());
            ui_state.credential_editor = Some(buf);
        }
        ui.colored_label(theme::c32(t.fg_dimmer), row_subtitle(rec));
        ui.colored_label(theme::c32(t.fg_dimmer), usage_label(rec.id, sessions));
    });
    ui.add_space(crate::ui::metrics::SP_XS);
}

/// 手绘「+ 新建」,挂显式 id。做法照抄 `tunnel_list::new_button`。
fn new_button(ui: &mut Ui) -> egui::Response {
    let galley = egui::WidgetText::from("+ 新建凭据").into_galley(
        ui,
        None,
        ui.available_width(),
        egui::TextStyle::Button,
    );
    let padding = ui.spacing().button_padding;
    let size = galley.size() + padding * 2.0;
    let (_auto_id, rect) = ui.allocate_space(size);
    let resp = ui.interact(rect, new_button_id(), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&resp);
        ui.painter().rect(
            rect.expand(visuals.expansion),
            visuals.rounding,
            visuals.weak_bg_fill,
            visuals.bg_stroke,
        );
        let text_pos = ui
            .layout()
            .align_size_within_rect(galley.size(), rect.shrink2(padding))
            .min;
        ui.painter().galley(text_pos, galley, visuals.text_color());
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_store::{Auth, Connection, Identity, NetworkPrefs, Protocol, SessionId};

    fn cred(id: u64, name: &str, user: &str, kind: AuthKind) -> CredentialRecord {
        CredentialRecord {
            id: CredentialId(id),
            name: name.into(),
            user: user.into(),
            kind,
        }
    }

    fn session(id: u64, auth: Auth) -> SessionRecord {
        SessionRecord {
            id: SessionId(id),
            modified_at: "t".into(),
            identity: Identity {
                name: format!("s{id}"),
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

    /// 一行要同时说清「是谁」和「什么认证方式」:凭据名是用户自己起的,
    /// 「运维号」这种名字光看名字分不出是 root 还是 ops、密码还是公钥。
    #[test]
    fn a_row_shows_both_the_user_and_the_auth_kind() {
        assert_eq!(
            row_subtitle(&cred(1, "运维号", "ops", AuthKind::Password)),
            "ops · 密码"
        );
        assert_eq!(
            row_subtitle(&cred(
                2,
                "部署号",
                "root",
                AuthKind::PublicKey {
                    has_passphrase: true
                }
            )),
            "root · 公钥"
        );
    }

    /// 「几个会话在用」是删之前唯一要知道的事,不能只数会话总数。
    #[test]
    fn the_usage_label_counts_only_the_sessions_referencing_this_credential() {
        let sessions = vec![
            session(1, Auth::Ref(CredentialId(7))),
            session(2, Auth::Ref(CredentialId(7))),
            session(3, Auth::Ref(CredentialId(8))),
            session(4, Auth::inline("ops", AuthKind::Password)),
        ];
        assert_eq!(usage_label(CredentialId(7), &sessions), "2 个会话在用");
        assert_eq!(usage_label(CredentialId(8), &sessions), "1 个会话在用");
        assert_eq!(usage_label(CredentialId(9), &sessions), "未被引用");
    }

    /// 顺序按 id 升序,不跟存储顺序走 —— 否则改一份凭据、列表整个重排。
    #[test]
    fn rows_are_ordered_by_id_not_by_storage_order() {
        let creds = vec![
            cred(3, "c", "u", AuthKind::Password),
            cred(1, "a", "u", AuthKind::Password),
            cred(2, "b", "u", AuthKind::Password),
        ];
        assert_eq!(
            visible_order(&creds),
            vec![CredentialId(1), CredentialId(2), CredentialId(3)]
        );
    }
}
