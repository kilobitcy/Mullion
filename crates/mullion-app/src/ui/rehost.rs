//! 换节点弹窗:把某一块分屏挂到另一条会话上。
//!
//! 入口是 pane 标题条上的 `⇄`(`pane_title::TitleAction::rehost`)。这里只
//! **选**节点 —— 真正的断开/重连由 `app.rs::spawn_rehost` 做。
//!
//! **这个弹窗要登记进 `app.rs::modal_open` 的 `Modal` 枚举**:里面有搜索框,
//! 不算模态的话敲的字会同时漏给远端 shell(T8)。**不进 `touched_store`**:
//! 它一行 store 都不写(同 `tab_props` 的姿态)。
//!
//! 只列 `Protocol::Ssh` 的会话:SFTP 节点没有 PTY,换过去只会得到一块永远
//! 不出字的黑屏。这是过滤而不是禁用 —— 灰着一条永远点不动的项,比不列出来
//! 更让人以为是 bug。

use mullion_core::layout::PaneId;
use mullion_store::{ColorTarget, Protocol, SessionId, SessionRecord};

use crate::theme::{self, Theme};

/// 列表里一行的 id。**必须是稳定的、按会话主键推出来的** —— 由 egui 自动
/// 分配的话,「点第 N 行」在测试里就只能靠猜坐标,而这个弹窗唯一的功能就是
/// 「点对行」。
fn row_id(id: SessionId) -> egui::Id {
    egui::Id::new(("rehost_row", id.0))
}

/// 弹窗的状态。`Some` = 开着,里面记着要换的是哪块 pane。
pub struct RehostDraft {
    pub pane: PaneId,
    /// 搜索框里的字。会话多起来之后翻列表不现实。
    pub filter: String,
}

impl RehostDraft {
    pub fn new(pane: PaneId) -> Self {
        Self {
            pane,
            filter: String::new(),
        }
    }
}

/// 用户在弹窗里的结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RehostAction {
    /// 选定了节点。`pane` 一并带出来 —— `app.rs` 收到时弹窗已经关了,
    /// 从 draft 里读不到了。
    Pick {
        pane: PaneId,
        session: SessionId,
    },
    Cancel,
}

/// 一条会话在这个列表里是否匹配搜索词。大小写不敏感,名字和地址都算。
fn matches(rec: &SessionRecord, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let n = needle.to_lowercase();
    rec.identity.name.to_lowercase().contains(&n) || rec.connection.host.to_lowercase().contains(&n)
}

/// 这一帧该列出哪些会话。纯函数,便于单测「过滤规则」本身。
fn visible<'a>(sessions: &'a [SessionRecord], needle: &str) -> Vec<&'a SessionRecord> {
    sessions
        .iter()
        .filter(|r| r.connection.protocol == Protocol::Ssh)
        .filter(|r| matches(r, needle))
        .collect()
}

/// 画一行。手写而不是 `ui.add(Button)`:egui 0.30 的 `Button` 没有任何
/// 指定 id 的接口,而稳定 id 是「点对行」可测的前提(同 `pane_title` 里
/// 那个 `small_action_button`)。
fn row(ui: &mut egui::Ui, rec: &SessionRecord, color: Option<egui::Color32>, t: &Theme) -> bool {
    let text = if rec.connection.host.is_empty() {
        rec.identity.name.clone()
    } else {
        format!("{}  ({})", rec.identity.name, rec.connection.host)
    };
    let font = egui::FontId::proportional(14.0);
    let galley = ui
        .painter()
        .layout_no_wrap(text, font, theme::c32(t.fg_strong));
    let w = ui.available_width().max(galley.size().x + 16.0);
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(w, galley.size().y + 8.0), egui::Sense::hover());
    let resp = ui.interact(rect, row_id(rec.id), egui::Sense::click());
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, 3.0, theme::c32(t.panel_head));
    }
    if let Some(c) = color {
        let bar = egui::Rect::from_min_size(
            rect.min + egui::vec2(2.0, 3.0),
            egui::vec2(3.0, rect.height() - 6.0),
        );
        ui.painter().rect_filled(bar, 1.5, c);
    }
    ui.painter().galley(
        rect.min + egui::vec2(12.0, 4.0),
        galley,
        theme::c32(t.fg_strong),
    );
    resp.clicked()
}

/// 画换节点弹窗。`draft` 是唯一真值来源:`None` = 弹窗关着。
/// 返回本帧的结论,`None` = 还在挑。
pub fn show(
    ctx: &egui::Context,
    t: &Theme,
    draft: &mut Option<RehostDraft>,
    sessions: &[SessionRecord],
    appearance: &crate::ui::badge::AppearanceCache,
) -> Option<RehostAction> {
    let d = draft.as_mut()?;
    let pane = d.pane;
    let mut action = None;
    egui::Window::new("换节点")
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            crate::ui::annotate::mark(ui.ctx(), "换节点".to_string(), ui.max_rect());
            ui.label(egui::RichText::new("这块分屏要换到哪个节点").color(theme::c32(t.fg_muted)));
            ui.add_space(crate::ui::metrics::SP_S);
            ui.add(
                egui::TextEdit::singleline(&mut d.filter)
                    .hint_text("搜索名称或地址")
                    .desired_width(crate::ui::metrics::FIELD_W_M),
            );
            ui.add_space(crate::ui::metrics::SP_S);
            let rows = visible(sessions, &d.filter);
            if rows.is_empty() {
                ui.label(egui::RichText::new("没有可用的 SSH 会话").color(theme::c32(t.fg_muted)));
            }
            egui::ScrollArea::vertical()
                .max_height(260.0)
                .show(ui, |ui| {
                    ui.set_min_width(crate::ui::metrics::FIELD_W_M);
                    for rec in rows {
                        let color = appearance
                            .get(rec.id)
                            .and_then(|a| crate::ui::badge::should_paint(a, ColorTarget::ListItem));
                        if row(ui, rec, color, t) {
                            action = Some(RehostAction::Pick {
                                pane,
                                session: rec.id,
                            });
                        }
                    }
                });
            ui.separator();
            if ui.button("取消").clicked() {
                action = Some(RehostAction::Cancel);
            }
        });
    if action.is_some() {
        *draft = None;
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::MULLION_DARK;
    use mullion_store::{Auth, AuthKind, Connection, Identity};

    fn rec(id: u64, name: &str, host: &str, proto: Protocol) -> SessionRecord {
        SessionRecord {
            id: SessionId(id),
            modified_at: "2026-08-17T00:00:00Z".into(),
            identity: Identity {
                name: name.into(),
                note: String::new(),
                group_id: None,
                tags: Vec::new(),
            },
            connection: Connection {
                host: host.into(),
                port: 22,
                protocol: proto,
            },
            auth: Auth::inline("user", AuthKind::Password),
            terminal: Default::default(),
            appearance: Default::default(),
            network: Default::default(),
            automation: Default::default(),
            sftp: Default::default(),
        }
    }

    /// 跑两帧预热(**时间必须显式推进**:`Area` 的 fade-in 没走完时内容
    /// 不可交互,点下去毫无反应 —— 同 `pane_title::click_button` 那个坑),
    /// 再在 `which` 的中心点一下,返回 `show` 的结论。
    fn click(
        draft: &mut Option<RehostDraft>,
        sessions: &[SessionRecord],
        which: egui::Id,
    ) -> Option<RehostAction> {
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(1.0);
        let cache = crate::ui::badge::AppearanceCache::default();
        let base = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 700.0),
            )),
            ..Default::default()
        };
        for t in [0.0_f64, 1.0] {
            let _ = ctx.run(
                egui::RawInput {
                    time: Some(t),
                    ..base()
                },
                |ctx| {
                    show(ctx, &MULLION_DARK, draft, sessions, &cache);
                },
            );
        }
        let rect = ctx
            .read_response(which)
            .unwrap_or_else(|| panic!("弹窗里找不到 {which:?}"))
            .rect;
        let pos = rect.center();
        let m = egui::Modifiers::default();
        let mut out = None;
        let _ = ctx.run(
            egui::RawInput {
                time: Some(2.0),
                events: vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: m,
                    },
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: m,
                    },
                ],
                ..base()
            },
            |ctx| {
                out = show(ctx, &MULLION_DARK, draft, sessions, &cache);
            },
        );
        out
    }

    /// 用户点的是哪一行,换的就该是哪个节点。点第二行却连到第一行那台上,
    /// 是一次**看不出错**的误连:两块分屏长得一样,用户以为换成功了。
    ///
    /// 自证会变红:把 `show` 里的 `session: rec.id` 改成 `sessions[0].id`。
    #[test]
    fn picking_a_row_reports_that_rows_session_and_the_pane_it_came_from() {
        let sessions = vec![
            rec(7, "web", "10.0.0.1", Protocol::Ssh),
            rec(9, "db", "10.0.0.2", Protocol::Ssh),
        ];
        let mut draft = Some(RehostDraft::new(PaneId(3)));
        let out = click(&mut draft, &sessions, row_id(SessionId(9)));
        assert_eq!(
            out,
            Some(RehostAction::Pick {
                pane: PaneId(3),
                session: SessionId(9)
            })
        );
        assert!(draft.is_none(), "选完节点弹窗该自己关上");
    }

    /// SFTP 会话没有 PTY,换过去只会是一块永远不出字的黑屏。它连行都不该有,
    /// 所以 `row_id` 在 widget 表里查不到 —— 这是比「点了没反应」强得多的判据。
    ///
    /// 自证会变红:把 `visible` 里的 `protocol == Protocol::Ssh` 过滤删掉。
    #[test]
    fn sftp_nodes_are_not_offered_because_they_have_no_pty() {
        let sessions = vec![
            rec(7, "web", "10.0.0.1", Protocol::Ssh),
            rec(8, "files", "10.0.0.2", Protocol::Sftp),
        ];
        assert_eq!(
            visible(&sessions, "")
                .iter()
                .map(|r| r.id)
                .collect::<Vec<_>>(),
            vec![SessionId(7)],
            "SFTP 节点不该出现在换节点列表里"
        );
    }

    /// 搜索框:名字和地址都要能搜到。只搜名字的话,记得住 IP 记不住名字的
    /// 用户(这个项目的典型场景)就只能一行行翻。
    ///
    /// 自证会变红:把 `matches` 里 `rec.connection.host` 那一项去掉。
    #[test]
    fn the_filter_matches_both_name_and_host() {
        let sessions = vec![
            rec(7, "生产机", "10.0.0.7", Protocol::Ssh),
            rec(8, "测试机", "192.168.1.8", Protocol::Ssh),
        ];
        assert_eq!(visible(&sessions, "").len(), 2, "空搜索词该全放行");
        assert_eq!(
            visible(&sessions, "生产")
                .iter()
                .map(|r| r.id)
                .collect::<Vec<_>>(),
            vec![SessionId(7)],
            "按名字搜不到"
        );
        assert_eq!(
            visible(&sessions, "192.168")
                .iter()
                .map(|r| r.id)
                .collect::<Vec<_>>(),
            vec![SessionId(8)],
            "按地址搜不到"
        );
    }
}
