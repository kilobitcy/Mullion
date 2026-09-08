//! F225③:pane 标题条上的「切到另一个项目」弹窗。
//!
//! 入口是 `pane_title::TitleAction::pick_project`。这里只**选**项目 ——
//! 真正的断开/重连走 F223 那条链路(`UiState::project_open_request`),
//! 和项目管理器里点「打开」完全同一条路,不另开一条。
//!
//! 形态照抄 `ui/rehost.rs`(按 pane 定位、搜索框 + 列表 + 取消),**但不合并
//! 进那个弹窗**(设计拍板):底层机制虽然共享,用户心智是两件事 ——「换一条
//! 路线到同一台机器」vs「切到另一个活」。合并成 tab 会让每次普通换节点都先
//! 撞见一个不相干的 tab,把一个已实机验收过的功能搅浑。
//!
//! **这个弹窗要登记进 `app.rs::modal_open` 的 `Modal` 枚举**:里面有搜索框,
//! 不算模态的话敲的字会同时漏给远端 shell(T8)。**不进 `touched_store`**:
//! 它一行 store 都不写。

use mullion_core::layout::PaneId;
use mullion_store::{ProjectId, ProjectRecord, SessionRecord};

use crate::theme::{self, Theme};

/// 列表里一行的 id。**必须按项目主键推**,理由同 `rehost::row_id`:
/// 自动 id 下「点第 N 行」在测试里只能靠猜坐标,而这个弹窗唯一的功能
/// 就是「点对行」。
fn row_id(id: ProjectId) -> egui::Id {
    egui::Id::new(("project_pick_row", id.0))
}

/// 取消按钮的 id。同上,手写 id 才点得到。
fn cancel_id() -> egui::Id {
    egui::Id::new("project_pick_cancel")
}

/// 弹窗那块 `Area` 的 id。按 pane 分:两块分屏各自开着的话不该互相抢位置。
pub(super) fn area_id(pane: PaneId) -> egui::Id {
    egui::Id::new(("project_pick_area", pane.0))
}

/// 弹窗离 pane 边缘的留白。
const INSET: f32 = 8.0;

/// 固定部分(说明行 + 搜索框 + 分隔线 + 取消按钮 + 边框内边距)的高度预算。
/// 宁可估大:估小了列表会把取消按钮顶出 pane 外面,而那是唯一的退出口。
const CHROME_H: f32 = 120.0;

/// 弹窗的状态。`Some` = 开着。
pub struct ProjectPickDraft {
    pub pane: PaneId,
    /// 搜索框里的字。
    pub filter: String,
}

impl ProjectPickDraft {
    pub fn new(pane: PaneId) -> Self {
        Self {
            pane,
            filter: String::new(),
        }
    }
}

/// 用户在弹窗里的结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickAction {
    /// 选定了项目。`pane` 一并带出来 —— `app.rs` 收到时弹窗已经关了,
    /// 从 draft 里读不到了。
    Pick {
        pane: PaneId,
        project: ProjectId,
    },
    Cancel,
}

/// 一个项目是否匹配搜索词。大小写不敏感;**名字和目录都算** —— 用户记得住
/// 的往往是路径(`/srv/api`)而不是自己当初起的项目名。
fn matches(p: &ProjectRecord, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let n = needle.to_lowercase();
    p.name.to_lowercase().contains(&n) || p.dir.to_lowercase().contains(&n)
}

/// 画弹窗。`draft` 是唯一真值来源:`None` = 关着。返回本帧的结论。
///
/// `pane_rect` 是**发起它的那块 pane** 的矩形(逻辑点)。理由同 `rehost`:
/// 飘在窗口正中的框看不出切的是哪一块,而切错分屏是一次看不出错的误操作。
#[allow(clippy::too_many_arguments)]
pub fn show(
    ctx: &egui::Context,
    t: &Theme,
    draft: &mut Option<ProjectPickDraft>,
    projects: &[ProjectRecord],
    lamps: &std::collections::BTreeMap<ProjectId, crate::project::Lamp>,
    sessions: &[SessionRecord],
    pane_rect: Option<egui::Rect>,
) -> Option<PickAction> {
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
    // `Area` 而不是 `Window`:`Window` 的位置记在 egui memory 里、还能被拖走,
    // 「永远在这块 pane 里」就守不住了。同 `rehost::show`。
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
                    crate::ui::annotate::mark(ui.ctx(), "切换项目".to_string(), ui.max_rect());
                    ui.label(
                        egui::RichText::new("这块分屏要切到哪个项目").color(theme::c32(t.fg_muted)),
                    );
                    ui.add_space(crate::ui::metrics::SP_S);
                    ui.add(
                        egui::TextEdit::singleline(&mut d.filter)
                            .hint_text("搜索项目名或目录")
                            .desired_width(field_w),
                    );
                    ui.add_space(crate::ui::metrics::SP_S);
                    // 顺序复用 `by_recent_access` —— 与 launcher 列表、项目
                    // 管理器左栏同一个函数。
                    let rows: Vec<_> = crate::ui::project_manager::by_recent_access(projects)
                        .into_iter()
                        .filter(|p| matches(p, &d.filter))
                        .collect();
                    if rows.is_empty() {
                        ui.label(
                            egui::RichText::new(if projects.is_empty() {
                                "还没有项目。从「会话 → 项目管理器」建一个。"
                            } else {
                                "没有匹配的项目"
                            })
                            .color(theme::c32(t.fg_muted)),
                        );
                    }
                    egui::ScrollArea::vertical()
                        .max_height(list_h)
                        .show(ui, |ui| {
                            ui.set_min_width(field_w);
                            for p in rows {
                                let lamp = lamps
                                    .get(&p.id)
                                    .copied()
                                    .unwrap_or(crate::project::Lamp::Unknown);
                                if row(ui, t, p, lamp, sessions) {
                                    action = Some(PickAction::Pick {
                                        pane,
                                        project: p.id,
                                    });
                                }
                            }
                        });
                    ui.separator();
                    // 手写而不是 `ui.button`:后者的 id 由布局顺序自动
                    // 生成,测试没法取回它的矩形去点 —— 而「取消」是这个
                    // 模态弹窗唯一的退出口,点不动就等于这块 pane 废了。
                    let galley = ui.painter().layout_no_wrap(
                        "取消".to_string(),
                        egui::FontId::proportional(14.0),
                        theme::c32(t.fg_strong),
                    );
                    let (rect, _) = ui.allocate_exact_size(
                        galley.size() + egui::vec2(20.0, 8.0),
                        egui::Sense::hover(),
                    );
                    let resp = ui.interact(rect, cancel_id(), egui::Sense::click());
                    ui.painter().rect(
                        rect,
                        3.0,
                        theme::c32(if resp.hovered() {
                            t.panel_head
                        } else {
                            t.panel_bg
                        }),
                        theme::stroke(t),
                    );
                    ui.painter().galley(
                        rect.center() - galley.size() / 2.0,
                        galley,
                        theme::c32(t.fg_strong),
                    );
                    if resp.clicked() {
                        action = Some(PickAction::Cancel);
                    }
                });
        });
    if action.is_some() {
        *draft = None;
    }
    action
}

/// 画一行:灯 + 项目名 + 目录。手写而不是 `ui.add(Button)`:egui 0.30 的
/// `Button` 没有任何指定 id 的接口,而稳定 id 是「点对行」可测的前提。
fn row(
    ui: &mut egui::Ui,
    t: &Theme,
    p: &ProjectRecord,
    lamp: crate::project::Lamp,
    sessions: &[SessionRecord],
) -> bool {
    let sub = crate::ui::launcher::row_subtitle(p, sessions);
    let font = egui::FontId::proportional(14.0);
    let galley = ui.painter().layout_no_wrap(
        format!("{}  ({sub})", p.name),
        font,
        theme::c32(t.fg_strong),
    );
    let w = ui.available_width().max(galley.size().x + 28.0);
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(w, galley.size().y + 8.0), egui::Sense::hover());
    let resp = ui.interact(rect, row_id(p.id), egui::Sense::click());
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, 3.0, theme::c32(t.panel_head));
    }
    // 灯画在最左边。走 `icon` 自绘、颜色不承担区分职责(形状才是)——
    // 同 `project_manager::lamp_dot` 的判据,这里是它的 painter 版:
    // 那个要 `&mut Ui` 并自己 allocate,与本行「整行一个 rect」冲突。
    let side = rect.height() * 0.7;
    let dot = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 10.0, rect.center().y),
        egui::vec2(side, side),
    );
    let (glyph, color) = match lamp {
        crate::project::Lamp::Lit => (crate::ui::icon::Glyph::LampLit, t.ok),
        crate::project::Lamp::Dark => (crate::ui::icon::Glyph::LampDark, t.fg_dim),
        crate::project::Lamp::Unknown => (crate::ui::icon::Glyph::LampUnknown, t.fg_muted),
    };
    ui.painter().extend(crate::ui::icon::shapes(
        dot,
        glyph,
        egui::Stroke::new(1.2, theme::c32(color)),
    ));
    ui.painter().galley(
        rect.min + egui::vec2(22.0, 4.0),
        galley,
        theme::c32(t.fg_strong),
    );
    resp.clicked()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::MULLION_DARK;

    fn proj(id: u64, name: &str, dir: &str, accessed: Option<&str>) -> ProjectRecord {
        ProjectRecord {
            id: ProjectId(id),
            name: name.into(),
            note: String::new(),
            nodes: Vec::new(),
            preferred: None,
            dir: dir.into(),
            tmux_name: None,
            created_at: "2026-09-01T00:00:00Z".into(),
            last_accessed_at: accessed.map(str::to_string),
        }
    }

    fn some_projects() -> Vec<ProjectRecord> {
        vec![
            proj(1, "老活", "/srv/old", Some("2026-09-01T00:00:00Z")),
            proj(2, "昨天的活", "/srv/api", Some("2026-09-07T00:00:00Z")),
        ]
    }

    /// 搜索词只认名字就漏掉一半:用户记得住的常常是路径。
    #[test]
    fn the_search_box_matches_the_directory_too_not_just_the_name() {
        let p = proj(1, "接口", "/srv/payments", None);
        assert!(matches(&p, "PAY"), "目录没参与匹配");
        assert!(matches(&p, "接口"));
        assert!(!matches(&p, "完全不沾边"));
    }

    /// 点某一行 = 选定那个项目,**并且带上发起它的那块 pane**。
    ///
    /// pane 必须由 draft 带出来:`app.rs` 收到结论时弹窗已经关了,那时再去
    /// 问「当前焦点是哪块」会切错分屏 —— 用户可能在等待期间点了别处。
    ///
    /// 自证会变红:把 `PickAction::Pick` 里的 `pane` 换成任何别的来源。
    #[test]
    fn picking_a_row_reports_both_the_project_and_the_pane_it_was_opened_for() {
        let ps = some_projects();
        let mut draft = Some(ProjectPickDraft::new(PaneId(7)));
        let got = click(&mut draft, &ps, row_id(ProjectId(2)));
        assert_eq!(
            got,
            Some(PickAction::Pick {
                pane: PaneId(7),
                project: ProjectId(2)
            })
        );
        assert!(draft.is_none(), "选完了弹窗还开着");
    }

    /// 顺序**复用** `by_recent_access`,与 launcher 列表、项目管理器左栏一致。
    ///
    /// 自证会变红:把 `by_recent_access(projects)` 换成 `projects.iter()`。
    #[test]
    fn the_list_puts_the_most_recently_used_project_first() {
        let ps = some_projects();
        let mut draft = Some(ProjectPickDraft::new(PaneId(7)));
        let ctx = draw(&mut draft, &ps);
        let first = ctx
            .read_response(row_id(ProjectId(2)))
            .expect("列表里没有最近那个项目")
            .rect;
        let second = ctx
            .read_response(row_id(ProjectId(1)))
            .expect("列表里没有老那个项目")
            .rect;
        assert!(
            first.top() < second.top(),
            "最近访问的没排在上面:{first:?} vs {second:?}"
        );
    }

    /// 取消是唯一的退出口 —— 它不工作的话,这块 pane 上就再也点不到别的
    /// 东西了(弹窗盖着,而且是模态)。
    #[test]
    fn cancel_closes_the_dialog_without_switching_anything() {
        let ps = some_projects();
        let mut draft = Some(ProjectPickDraft::new(PaneId(7)));
        let got = click(&mut draft, &ps, egui::Id::new("project_pick_cancel"));
        assert_eq!(got, Some(PickAction::Cancel));
        assert!(draft.is_none(), "点了取消弹窗还开着");
    }

    /// 弹窗必须落在**发起它的那块 pane** 里,理由同 `rehost` 那条:飘在窗口
    /// 正中的框看不出切的是哪一块,而两块 pane 长得一模一样。
    ///
    /// 自证会变红:把 `show` 里的 `host` 定死成 `ctx.screen_rect()`。
    #[test]
    fn the_dialog_sits_inside_the_pane_that_opened_it() {
        let ps = some_projects();
        let mut draft = Some(ProjectPickDraft::new(PaneId(7)));
        let ctx = draw(&mut draft, &ps);
        let rect = ctx
            .memory(|m| m.area_rect(area_id(PaneId(7))))
            .expect("弹窗没画出来");
        assert!(
            pane().contains_rect(rect.shrink(0.5)),
            "弹窗跑出了发起它的那块 pane:{rect:?} 不在 {:?} 里",
            pane()
        );
    }

    /// 一块 pane 也占不满的窗口。刻意不从原点起,几何断言才抓得住「没接
    /// pane 几何」。
    fn pane() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(120.0, 60.0), egui::vec2(520.0, 480.0))
    }

    fn base_input() -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 700.0),
            )),
            ..Default::default()
        }
    }

    /// 跑两帧预热。**时间必须显式推进**:`Area` 的 fade-in 没走完时内容
    /// 不可交互,点下去毫无反应(同 `pane_title::click_button` 那个坑)。
    fn draw(draft: &mut Option<ProjectPickDraft>, ps: &[ProjectRecord]) -> egui::Context {
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(1.0);
        let lamps = std::collections::BTreeMap::new();
        for t in [0.0_f64, 1.0] {
            let _ = ctx.run(
                egui::RawInput {
                    time: Some(t),
                    ..base_input()
                },
                |ctx| {
                    show(ctx, &MULLION_DARK, draft, ps, &lamps, &[], Some(pane()));
                },
            );
        }
        ctx
    }

    fn click(
        draft: &mut Option<ProjectPickDraft>,
        ps: &[ProjectRecord],
        which: egui::Id,
    ) -> Option<PickAction> {
        let ctx = draw(draft, ps);
        let pos = ctx
            .read_response(which)
            .unwrap_or_else(|| panic!("弹窗里找不到 {which:?}"))
            .rect
            .center();
        let m = egui::Modifiers::default();
        let lamps = std::collections::BTreeMap::new();
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
                ..base_input()
            },
            |ctx| {
                out = show(ctx, &MULLION_DARK, draft, ps, &lamps, &[], Some(pane()));
            },
        );
        out
    }
}
