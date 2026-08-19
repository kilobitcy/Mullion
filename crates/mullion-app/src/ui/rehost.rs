//! 换节点弹窗:把某一块分屏挂到另一条会话上。
//!
//! 入口是 pane 标题条上的换节点按钮(`pane_title::TitleAction::rehost`)。这里只
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
use mullion_store::{ColorTarget, GroupRecord, Protocol, SessionId, SessionRecord};

use crate::theme::{self, Theme};

/// 列表里一行的 id。**必须是稳定的、按会话主键推出来的** —— 由 egui 自动
/// 分配的话,「点第 N 行」在测试里就只能靠猜坐标,而这个弹窗唯一的功能就是
/// 「点对行」。
fn row_id(id: SessionId) -> egui::Id {
    egui::Id::new(("rehost_row", id.0))
}

/// 弹窗那块 `Area` 的 id。`show` 与测试都从这里取(同 `pane_title::area_id`
/// 的姿态)——`ctx.memory(|m| m.area_rect(id))` 是「它到底画在哪儿」唯一
/// 能自动验的抓手。按 pane 分 id:两块分屏各自开着的话不该互相抢位置。
pub(super) fn area_id(pane: PaneId) -> egui::Id {
    egui::Id::new(("rehost_area", pane.0))
}

/// 弹窗离 pane 边缘的留白。
const INSET: f32 = 8.0;

/// 固定部分(说明行 + 搜索框 + 分隔线 + 取消按钮 + 边框内边距)的高度预算。
/// 列表高度 = pane 可用高度 − 这个数,所以宁可估大一点:估小了列表会把取消
/// 按钮顶出 pane 外面,而那是唯一的退出口。
const CHROME_H: f32 = 120.0;

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

/// 这一帧该列出哪些会话。**顺序来自会话管理器左栏的 `visible_order`**——
/// 两边各排一套的话,左栏里挨着的两条在这儿会隔着半屏,而用户是照着左栏的
/// 记忆找的(F130)。搜索词传空:借它的**顺序**,不借它的过滤规则(左栏还搜
/// 标签,这里只搜名字和地址)。
///
/// `Protocol::Ssh` 同时替掉了原先那道协议过滤:SFTP 节点没有 PTY,换过去
/// 只有一块永远不出字的黑屏。
fn visible<'a>(
    sessions: &'a [SessionRecord],
    groups: &[GroupRecord],
    needle: &str,
) -> Vec<&'a SessionRecord> {
    crate::ui::session_manager::list::visible_order(sessions, groups, "", Protocol::Ssh)
        .into_iter()
        .filter_map(|id| sessions.iter().find(|r| r.id == id))
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
///
/// `pane_rect` 是**发起它的那块 pane** 的矩形(逻辑点,由调用方从 `PaneGeom.px`
/// 除 `pixels_per_point` 换算)。弹窗钉在这块矩形里面:分屏之后飘在窗口正中的
/// 一个「换节点」框根本看不出换的是哪一块,而换错分屏是一次看不出错的误连
/// (两块 pane 长得一样)。`None` = 拿不到几何(pane 已经没了),退回整窗,
/// 总比画不出来强。
pub fn show(
    ctx: &egui::Context,
    t: &Theme,
    draft: &mut Option<RehostDraft>,
    sessions: &[SessionRecord],
    groups: &[GroupRecord],
    appearance: &crate::ui::badge::AppearanceCache,
    pane_rect: Option<egui::Rect>,
) -> Option<RehostAction> {
    let d = draft.as_mut()?;
    let pane = d.pane;
    let mut action = None;
    let host = pane_rect.unwrap_or_else(|| ctx.screen_rect());
    let avail = host.shrink(INSET);
    let field_w = crate::ui::metrics::field_w(
        avail.width(),
        crate::ui::metrics::FIELD_W_M,
        2.0 * crate::ui::metrics::SP_M,
    );
    let list_h = (avail.height() - CHROME_H).clamp(48.0, 260.0);
    // 用 `Area` 而不是 `Window`:`Window` 的位置记在 egui memory 里、还能被拖走,
    // 「永远在这块 pane 里」就守不住了。`constrain_to` 保证内容比 pane 还大时
    // 是被推回边界内,而不是溢出到邻居分屏上。
    egui::Area::new(area_id(pane))
        .order(egui::Order::Foreground)
        .fixed_pos(avail.min)
        .constrain_to(host)
        .show(ctx, |ui| {
            ui.set_max_width(field_w + 2.0 * crate::ui::metrics::SP_M);
            egui::Frame::popup(&ctx.style())
                .fill(theme::c32(t.panel_bg))
                .stroke(theme::stroke(t))
                .show(ui, |ui| {
                    crate::ui::annotate::mark(ui.ctx(), "换节点".to_string(), ui.max_rect());
                    ui.label(
                        egui::RichText::new("这块分屏要换到哪个节点").color(theme::c32(t.fg_muted)),
                    );
                    ui.add_space(crate::ui::metrics::SP_S);
                    ui.add(
                        egui::TextEdit::singleline(&mut d.filter)
                            .hint_text("搜索名称或地址")
                            .desired_width(field_w),
                    );
                    ui.add_space(crate::ui::metrics::SP_S);
                    let rows = visible(sessions, groups, &d.filter);
                    if rows.is_empty() {
                        ui.label(
                            egui::RichText::new("没有可用的 SSH 会话")
                                .color(theme::c32(t.fg_muted)),
                        );
                    }
                    egui::ScrollArea::vertical()
                        .max_height(list_h)
                        .show(ui, |ui| {
                            ui.set_min_width(field_w);
                            for rec in rows {
                                let color = appearance.get(rec.id).and_then(|a| {
                                    crate::ui::badge::should_paint(a, ColorTarget::ListItem)
                                });
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

    /// 测试用的「那块分屏」。刻意不占满 900×700 的窗口,也刻意不从原点起:
    /// 弹窗要是没真的跟着 pane 走,下面那条几何断言才抓得住。
    fn pane() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(120.0, 60.0), egui::vec2(520.0, 480.0))
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
                    show(
                        ctx,
                        &MULLION_DARK,
                        draft,
                        sessions,
                        &[],
                        &cache,
                        Some(pane()),
                    );
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
                out = show(
                    ctx,
                    &MULLION_DARK,
                    draft,
                    sessions,
                    &[],
                    &cache,
                    Some(pane()),
                );
            },
        );
        out
    }

    /// 弹窗必须落在**发起它的那块 pane** 里。飘在窗口正中的话,分屏之后根本
    /// 看不出要换的是哪一块 —— 而换错分屏是一次看不出错的误连(两块 pane
    /// 长得一样,用户以为换成功了)。
    ///
    /// 自证会变红:把 `show` 里的 `host` 定死成 `ctx.screen_rect()`(等价于
    /// 「没接 pane 几何」),弹窗跑到 (8,8),断言立刻红。注意 `fixed_pos` 与
    /// `constrain_to` 在这条测试里互为兜底,单独去掉任何一条都还是绿的 ——
    /// 它守的是「落在 pane 里」这个结果,不是某一行的写法。
    #[test]
    fn the_picker_is_drawn_inside_the_pane_that_asked_for_it() {
        let sessions = vec![rec(7, "web", "10.0.0.1", Protocol::Ssh)];
        let mut draft = Some(RehostDraft::new(PaneId(3)));
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(1.0);
        let cache = crate::ui::badge::AppearanceCache::default();
        for time in [0.0_f64, 1.0] {
            let _ = ctx.run(
                egui::RawInput {
                    time: Some(time),
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(900.0, 700.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    show(
                        ctx,
                        &MULLION_DARK,
                        &mut draft,
                        &sessions,
                        &[],
                        &cache,
                        Some(pane()),
                    );
                },
            );
        }
        let got = ctx
            .memory(|m| m.area_rect(area_id(PaneId(3))))
            .expect("弹窗开着却没有 Area —— 它根本没画出来");
        assert!(
            pane().contains_rect(got),
            "换节点弹窗跑到 pane 外面去了:pane={:?} 弹窗={got:?}",
            pane()
        );
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
            visible(&sessions, &[], "")
                .iter()
                .map(|r| r.id)
                .collect::<Vec<_>>(),
            vec![SessionId(7)],
            "SFTP 节点不该出现在换节点列表里"
        );
    }

    /// F130:弹窗的行顺序必须跟会话管理器左栏一模一样。两边各排一套的话,
    /// 左栏里挨着的两条在这儿可能隔着半屏 —— 而用户是照着左栏的记忆找的。
    ///
    /// 顺序的唯一真值来源是 `list::visible_order`(左栏渲染与键盘导航也用它)。
    ///
    /// fixture 刻意让**数组顺序与分组顺序相反**:`sessions` 里 7、8、9 依次是
    /// 未分组、组 2、组 1,而 `groups` 是 [组 1, 组 2]。照数组顺序出是
    /// [7,8,9],照分组出是 [9,8,7] —— 不这么造的话这条断言在旧实现下也是绿的。
    ///
    /// 自证会变红:把 `visible` 改回 `sessions.iter().filter(..)`。
    #[test]
    fn rows_are_ordered_exactly_like_the_session_manager_list() {
        use mullion_store::{GroupId, GroupRecord};

        let groups = vec![
            GroupRecord {
                id: GroupId(1),
                name: "一组".into(),
                tags: Vec::new(),
                terminal: Default::default(),
                appearance: Default::default(),
                network: Default::default(),
                automation: Default::default(),
            },
            GroupRecord {
                id: GroupId(2),
                name: "二组".into(),
                tags: Vec::new(),
                terminal: Default::default(),
                appearance: Default::default(),
                network: Default::default(),
                automation: Default::default(),
            },
        ];
        let mut a = rec(7, "未分组", "10.0.0.7", Protocol::Ssh);
        let mut b = rec(8, "二组的", "10.0.0.8", Protocol::Ssh);
        let mut c = rec(9, "一组的", "10.0.0.9", Protocol::Ssh);
        a.identity.group_id = None;
        b.identity.group_id = Some(GroupId(2));
        c.identity.group_id = Some(GroupId(1));
        let sessions = vec![a, b, c];

        let got: Vec<SessionId> = visible(&sessions, &groups, "")
            .iter()
            .map(|r| r.id)
            .collect();
        let want =
            crate::ui::session_manager::list::visible_order(&sessions, &groups, "", Protocol::Ssh);
        assert_eq!(got, want, "换节点弹窗的顺序跟左栏对不上");
        assert_eq!(
            got,
            vec![SessionId(9), SessionId(8), SessionId(7)],
            "分组顺序没生效 —— fixture 或实现有一处没按分组排"
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
        assert_eq!(visible(&sessions, &[], "").len(), 2, "空搜索词该全放行");
        assert_eq!(
            visible(&sessions, &[], "生产")
                .iter()
                .map(|r| r.id)
                .collect::<Vec<_>>(),
            vec![SessionId(7)],
            "按名字搜不到"
        );
        assert_eq!(
            visible(&sessions, &[], "192.168")
                .iter()
                .map(|r| r.id)
                .collect::<Vec<_>>(),
            vec![SessionId(8)],
            "按地址搜不到"
        );
    }
}
