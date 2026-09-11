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

/// 取消按钮的 id。同上,手写 id 才点得到。
fn cancel_id() -> egui::Id {
    egui::Id::new("project_pick_cancel")
}

/// 弹窗那块 `Area` 的 id。按 pane 分:两块分屏各自开着的话不该互相抢位置。
pub(crate) fn area_id(pane: PaneId) -> egui::Id {
    egui::Id::new(("project_pick_area", pane.0))
}

/// 弹窗离 pane 边缘的留白。
const INSET: f32 = 8.0;

/// 固定部分(说明行 + 搜索框 + 分隔线 + 取消按钮 + 边框内边距)的高度预算。
/// 宁可估大:估小了列表会把取消按钮顶出 pane 外面,而那是唯一的退出口。
///
/// F233:行从「一行拼接文本」换成 `project_row`(两行 + 时间列,`ROW_H` = 48)
/// 之后,同一块 pane 里能放下的行数少了一半多。这个常量本身只管**固定部分**、
/// 不随行高变 —— 但下面 `list_h` 的下界必须按新行高走,否则最后一行会被切掉
/// 一半,看起来像渲染坏了。
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
    appearance: &crate::ui::badge::AppearanceCache,
    pane_rect: Option<egui::Rect>,
) -> Option<PickAction> {
    let d = draft.as_mut()?;
    let pane = d.pane;
    // 一帧取一次(同另外两处列表)。
    let now = time::OffsetDateTime::now_utc();
    let mut action = None;
    let host = pane_rect.unwrap_or_else(|| ctx.screen_rect());
    let avail = host.shrink(INSET);
    let field_w = crate::ui::metrics::field_w(
        avail.width(),
        crate::ui::metrics::FIELD_W_M,
        2.0 * crate::ui::metrics::SP_M,
    );
    // 下界取一整行:放不下一整行的话最后那行会被切一半,看起来像渲染坏了。
    let list_h = (avail.height() - CHROME_H).clamp(crate::ui::project_row::ROW_H, 260.0);
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
                            .hint_text("搜索项目名 / 目录 / 节点")
                            .desired_width(field_w),
                    );
                    ui.add_space(crate::ui::metrics::SP_S);
                    // F257:同 launcher —— 恒 `Tab::Active`,搜索穿透归档。
                    let rows = crate::ui::project_list::rows(
                        projects,
                        crate::ui::project_list::Tab::Active,
                        &d.filter,
                        sessions,
                    );
                    if let Some(reason) = crate::ui::project_list::empty_reason(
                        projects,
                        crate::ui::project_list::Tab::Active,
                        &d.filter,
                        sessions,
                    ) {
                        ui.label(
                            egui::RichText::new(crate::ui::project_list::empty_text(
                                reason,
                                crate::ui::project_list::Surface::Pick,
                            ))
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
                                let r = crate::ui::project_row::show(
                                    ui,
                                    t,
                                    &crate::ui::project_row::Row {
                                        project: p,
                                        lamp,
                                        sessions,
                                        query: &d.filter,
                                        // 这个弹窗没有「正在编辑哪一个」的概念。
                                        selected: false,
                                        now,
                                        list: "pick",
                                        icon: crate::project::icon_for(p, appearance),
                                        icon_bg: crate::project::icon_bg(
                                            p,
                                            appearance,
                                            mullion_store::ColorTarget::ListItem,
                                        ),
                                    },
                                );
                                if r.clicked() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::MULLION_DARK;
    use mullion_store::SessionId;

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
            archived_at: None,
            icon: None,
        }
    }

    /// 同 `proj`,但归了档 —— F257 测试专用。
    fn proj_archived(id: u64, name: &str, dir: &str) -> ProjectRecord {
        let mut p = proj(id, name, dir, None);
        p.archived_at = Some("2026-09-01T00:00:00Z".into());
        p
    }

    fn some_projects() -> Vec<ProjectRecord> {
        vec![
            proj(1, "老活", "/srv/old", Some("2026-09-01T00:00:00Z")),
            proj(2, "昨天的活", "/srv/api", Some("2026-09-07T00:00:00Z")),
        ]
    }

    /// 切换项目弹窗同上:默认只列在用的,搜索穿透归档。
    #[test]
    fn the_pick_popup_hides_archived_projects_until_you_search_for_them() {
        let ps = vec![
            proj_archived(1, "老活", "/data/old"),
            proj(2, "在做的", "/data/now", None),
        ];
        assert_eq!(names_drawn(&ps, ""), vec!["在做的"]);
        assert_eq!(names_drawn(&ps, "老活"), vec!["老活"]);
    }

    /// 切换弹窗的行也要带最后打开时间(F234)—— 这个弹窗回答的问题和启动页
    /// 完全一样(「切到哪个活」),两处一个有时间一个没有,用户会以为其中
    /// 一处坏了。
    ///
    /// 自证会变红:把 `project_row::show` 换回原来那个把名字和副标题拼成
    /// 一行的私有 `row`。
    #[test]
    fn each_pick_row_says_when_it_was_last_opened() {
        let joined = pick_texts(&[proj(1, "接口", "/srv/api", None)], "").join(" ");
        assert!(joined.contains("从未打开"), "没显示最后打开时间:{joined}");
    }

    /// 搜索匹配判据与另外两处**同一个函数**(F233)。各写一份的话,同一个
    /// 搜索词在两个界面给出不同结果。这里钉的是「按节点名也搜得到」,那正是
    /// 原来那份私有实现做不到的 —— 它只查 name/dir。
    ///
    /// 自证会变红:把 `crate::project::matches` 换回只查 name/dir 的判据。
    #[test]
    fn the_pick_dialog_finds_a_project_by_its_node_name_like_the_other_two_lists() {
        let mut p = proj(1, "接口", "/srv/api", None);
        p.nodes = vec![SessionId(7)];
        let joined = pick_texts(&[p], "web01").join(" ");
        assert!(joined.contains("接口"), "按节点名搜不到:{joined}");
    }

    /// 哪些项目的行**真被画出来了**——不靠比对画出来的文字:搜索框里的字
    /// 本身也是一段 `Shape::Text`,查询词恰好等于项目名时(下面
    /// `the_pick_popup_hides_archived_projects_until_you_search_for_them`
    /// 就是这种情况)会把搜索框那份也算进去,平白多算一条。改用
    /// `read_response` 查每个项目那一行的 id 有没有被 `ui.interact` 过 ——
    /// 姿态同 `launcher::tests::names_drawn`。
    ///
    /// **`read_response` 必须在 `ctx.run` 的闭包内部调用,在 `show(...)` 之后
    /// 立刻读。** `run()` 返回之后再读,拿到的是上上一帧(N-2)的陈旧记录 ——
    /// 原理和实测数字见 `session_manager/mod.rs:2448` 那条注释(`this_pass`/
    /// `prev_pass` 在 `end_pass` 里 `mem::swap`,`read_response` 优先命中
    /// `this_pass`,在闭包外读到的其实是 swap 之前的旧值)。
    fn names_drawn(projects: &[ProjectRecord], query: &str) -> Vec<String> {
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(1.0);
        let lamps = std::collections::BTreeMap::new();
        let sessions = vec![node_session()];
        let mut draft = Some(ProjectPickDraft {
            pane: PaneId(7),
            filter: query.to_string(),
        });
        let mut drawn = Vec::new();
        for time in [0.0_f64, 1.0] {
            let _ = ctx.run(
                egui::RawInput {
                    time: Some(time),
                    ..base_input()
                },
                |ctx| {
                    show(
                        ctx,
                        &MULLION_DARK,
                        &mut draft,
                        projects,
                        &lamps,
                        &sessions,
                        &crate::ui::badge::AppearanceCache::default(),
                        Some(pane()),
                    );
                    drawn = projects
                        .iter()
                        .filter(|p| {
                            ctx.read_response(crate::ui::project_row::row_id("pick", p.id))
                                .is_some()
                        })
                        .map(|p| p.name.clone())
                        .collect();
                },
            );
        }
        drawn
    }

    /// 跑两帧,把弹窗画出来的全部文字收上来。姿态同 `launcher::tests::texts_with`。
    fn pick_texts(ps: &[ProjectRecord], query: &str) -> Vec<String> {
        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                egui::Shape::Text(ts) => out.push(ts.galley.text().to_string()),
                _ => {}
            }
        }
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(1.0);
        let lamps = std::collections::BTreeMap::new();
        let sessions = vec![node_session()];
        let mut draft = Some(ProjectPickDraft {
            pane: PaneId(7),
            filter: query.to_string(),
        });
        let mut shapes = Vec::new();
        for time in [0.0_f64, 1.0] {
            shapes = ctx
                .run(
                    egui::RawInput {
                        time: Some(time),
                        ..base_input()
                    },
                    |ctx| {
                        show(
                            ctx,
                            &MULLION_DARK,
                            &mut draft,
                            ps,
                            &lamps,
                            &sessions,
                            &crate::ui::badge::AppearanceCache::default(),
                            Some(pane()),
                        );
                    },
                )
                .shapes;
        }
        let mut out = Vec::new();
        for cs in &shapes {
            walk(&cs.shape, &mut out);
        }
        out
    }

    /// 一条能当项目节点的 SSH 会话。
    fn node_session() -> SessionRecord {
        SessionRecord {
            id: SessionId(7),
            modified_at: "t".into(),
            identity: mullion_store::Identity {
                name: "web01".into(),
                note: String::new(),
                group_id: None,
                tags: Vec::new(),
            },
            connection: mullion_store::Connection {
                host: "h".into(),
                port: 22,
                protocol: mullion_store::Protocol::Ssh,
            },
            auth: mullion_store::Auth::inline("u", mullion_store::AuthKind::Password),
            terminal: Default::default(),
            appearance: Default::default(),
            network: Default::default(),
            automation: Default::default(),
            sftp: Default::default(),
        }
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
        let got = click(
            &mut draft,
            &ps,
            crate::ui::project_row::row_id("pick", ProjectId(2)),
        );
        assert_eq!(
            got,
            Some(PickAction::Pick {
                pane: PaneId(7),
                project: ProjectId(2)
            })
        );
        assert!(draft.is_none(), "选完了弹窗还开着");
    }

    /// 顺序**复用** `project_list::rows`(内部按 `segment_order`/
    /// `newest_first` 排序),与 launcher 列表、项目管理器左栏一致。
    ///
    /// 自证会变红:把 `project_list.rs` 里 `newest_first` 的
    /// `y.cmp(x)` 改成 `x.cmp(y)`(比较方向反过来,最近访问的排到最后面)。
    #[test]
    fn the_list_puts_the_most_recently_used_project_first() {
        let ps = some_projects();
        let mut draft = Some(ProjectPickDraft::new(PaneId(7)));
        let ctx = draw(&mut draft, &ps);
        let first = ctx
            .read_response(crate::ui::project_row::row_id("pick", ProjectId(2)))
            .expect("列表里没有最近那个项目")
            .rect;
        let second = ctx
            .read_response(crate::ui::project_row::row_id("pick", ProjectId(1)))
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
                    show(
                        ctx,
                        &MULLION_DARK,
                        draft,
                        ps,
                        &lamps,
                        &[],
                        &crate::ui::badge::AppearanceCache::default(),
                        Some(pane()),
                    );
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
                out = show(
                    ctx,
                    &MULLION_DARK,
                    draft,
                    ps,
                    &lamps,
                    &[],
                    &crate::ui::badge::AppearanceCache::default(),
                    Some(pane()),
                );
            },
        );
        out
    }
}
