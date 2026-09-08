//! F225①:launcher 态中央区的**项目列表** —— 本功能的主入口。
//!
//! 为什么是主入口:开机后想干的第一件事是**回到昨天那个活**,不是「连服务器」。
//! 项目列表 + F224 的灯恰好回答开机第一个问题:「哪个活还在跑?」
//!
//! 这块中央区在 F225 之前**完全没有 egui 内容**(终端是 GPU 自绘,egui 只画
//! 菜单栏/标签栏/状态栏/侧栏),所以这是往空白区新建,不是重排既有布局。
//!
//! 设计见 `docs/superpowers/specs/2026-09-08-f221-f225-project-unit-design.md` §八①。

use mullion_store::{ProjectId, ProjectRecord, SessionRecord};

/// 一行的副标题:`目录 · 节点名`。
///
/// 节点名解析不出来(会话被删了、或项目还没勾节点)时**只显示目录**,
/// 不显示「(未知)」一类占位:那句话对用户没有任何可操作性,而目录本身
/// 已经足以认出这是哪个活。
///
/// 选节点走 [`crate::project::node_for`] —— 和 `plan_open` 真拨号时用的是
/// **同一个函数**。各写一份的话,列表上写着 A、点下去连的是 B。
fn row_subtitle(p: &ProjectRecord, sessions: &[SessionRecord]) -> String {
    let name = crate::project::node_for(p)
        .and_then(|id| sessions.iter().find(|s| s.id == id))
        .map(|s| s.identity.name.as_str());
    match name {
        Some(n) => format!("{} · {}", p.dir, n),
        None => p.dir.clone(),
    }
}

/// 画 launcher 中央区。点中某一行就把「打开这个项目」写进 `ui_state`。
///
/// **必须是本帧最后一个 panel 类部件**(同 `restored::show` / `files_panel`):
/// `CentralPanel` 铺满剩余空间,排在别的 panel 之前会把它们挤没。
///
/// `pane` 恒传 `None`:launcher 态一块 pane 都没有,编一个 `PaneId` 出来的话
/// `decide_project_open` 会拿它去查一块不存在的 pane。
pub fn show(
    ctx: &egui::Context,
    t: &crate::theme::Theme,
    ui_state: &mut crate::ui::UiState,
    projects: &[ProjectRecord],
    lamps: &std::collections::BTreeMap<ProjectId, crate::project::Lamp>,
    sessions: &[SessionRecord],
) {
    use crate::ui::metrics::{SP_L, SP_M, SP_S};
    let panel = egui::CentralPanel::default()
        .frame(egui::Frame::none().fill(crate::theme::c32(t.window_bg)))
        .show(ctx, |ui| {
            ui.add_space(SP_L);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new("继续上次的活")
                        .size(16.0)
                        .color(crate::theme::c32(t.fg_strong)),
                );
            });
            ui.add_space(SP_M);
            if projects.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.label(crate::theme::hint_text(
                        t,
                        "还没有项目。一个项目 = 一台机器上的一个目录 + 一个专属 tmux 会话;\
                         从菜单「会话 → 项目管理器」建一个,以后开机点一下就回到现场。",
                    ));
                });
                return;
            }
            egui::ScrollArea::vertical()
                .id_salt("launcher_projects")
                .show(ui, |ui| {
                    // 顺序**复用** `by_recent_access`,不另写一份:项目管理器
                    // 左栏用的是同一个函数,两处顺序不一样用户第一眼就看得出来。
                    for p in crate::ui::project_manager::by_recent_access(projects) {
                        let lamp = lamps
                            .get(&p.id)
                            .copied()
                            .unwrap_or(crate::project::Lamp::Unknown);
                        if row(ui, t, p, lamp, sessions).clicked() {
                            ui_state.project_open_request = Some((p.id, None));
                        }
                        ui.add_space(SP_S);
                    }
                });
        });
    crate::ui::annotate::mark(ctx, "项目列表(启动页)", panel.response.rect);
}

/// 一行:灯 + 项目名 + `目录 · 节点名`。**整行**可点。
fn row(
    ui: &mut egui::Ui,
    t: &crate::theme::Theme,
    p: &ProjectRecord,
    lamp: crate::project::Lamp,
    sessions: &[SessionRecord],
) -> egui::Response {
    use crate::ui::metrics::{SP_M, SP_S};
    // 整行画完再 `interact` 一次:**不能**靠里面某个 label 的 `sense`,那样
    // 只有字上那几十个像素点得中(F141 那条「侧栏本地栏一行都点不中」就是
    // 这么来的)。
    let r = egui::Frame::none()
        .inner_margin(egui::Margin::symmetric(SP_M, SP_S))
        .fill(crate::theme::c32(t.panel_bg))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                crate::ui::project_manager::lamp_dot(ui, t, lamp);
                ui.add_space(SP_S);
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(&p.name).color(crate::theme::c32(t.fg_strong)));
                    ui.label(crate::theme::hint_text(t, row_subtitle(p, sessions)));
                });
                ui.allocate_space(egui::vec2(ui.available_width(), 0.0));
            });
        })
        .response;
    let resp = ui.interact(r.rect, ui.id().with(p.id.0), egui::Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_store::{Protocol, SessionId};

    fn proj(id: u64, name: &str, dir: &str, accessed: Option<&str>) -> ProjectRecord {
        ProjectRecord {
            id: ProjectId(id),
            name: name.into(),
            note: String::new(),
            nodes: vec![SessionId(7)],
            preferred: None,
            dir: dir.into(),
            tmux_name: None,
            created_at: "2026-09-01T00:00:00Z".into(),
            last_accessed_at: accessed.map(str::to_string),
        }
    }

    fn sess(id: u64, name: &str) -> SessionRecord {
        SessionRecord {
            id: SessionId(id),
            modified_at: "t".into(),
            identity: mullion_store::Identity {
                name: name.into(),
                note: String::new(),
                group_id: None,
                tags: Vec::new(),
            },
            connection: mullion_store::Connection {
                host: "h".into(),
                port: 22,
                protocol: Protocol::Ssh,
            },
            auth: mullion_store::Auth::inline("u", mullion_store::AuthKind::Password),
            terminal: Default::default(),
            appearance: Default::default(),
            network: Default::default(),
            automation: Default::default(),
            sftp: Default::default(),
        }
    }

    /// 一行必须同时说清**在哪台机器上的哪个目录** —— 只有项目名的话,
    /// 「api」和「api(测试机)」这种命名在列表里根本分不出来。
    #[test]
    fn a_row_names_both_the_directory_and_the_node_it_will_dial() {
        let s = vec![sess(7, "web01")];
        assert_eq!(
            row_subtitle(&proj(1, "接口", "/srv/api", None), &s),
            "/srv/api · web01"
        );
    }

    /// 节点解析不出来(会话被别的实例删了)只显示目录,**不显示占位文字**。
    /// 那句话对用户没有任何可操作性,而目录已经够认出这是哪个活。
    #[test]
    fn a_row_whose_node_is_gone_still_says_which_directory_it_is() {
        assert_eq!(
            row_subtitle(&proj(1, "接口", "/srv/api", None), &[]),
            "/srv/api"
        );
    }

    /// 列表顺序**复用** `by_recent_access`(最近访问的在最上面)。
    ///
    /// 自己再写一遍排序的话,项目管理器左栏和这里会给出两个顺序 —— 而这
    /// 两个列表用户几分钟内就会都看到一遍。
    ///
    /// 自证会变红:把 `for p in by_recent_access(projects)` 换成
    /// `for p in projects`。
    #[test]
    fn the_launcher_puts_the_most_recently_used_project_first() {
        let ps = vec![
            proj(1, "老活", "/srv/old", Some("2026-09-01T00:00:00Z")),
            proj(2, "昨天的活", "/srv/new", Some("2026-09-07T00:00:00Z")),
        ];
        let texts = texts(&ps);
        let a = texts
            .iter()
            .position(|s| s == "昨天的活")
            .expect("没画新的那个");
        let b = texts
            .iter()
            .position(|s| s == "老活")
            .expect("没画老的那个");
        assert!(a < b, "最近访问的没排在最上面:{texts:?}");
    }

    /// 一个项目都没有时得说清**项目是什么**、以及去哪儿建。
    /// 空列表配一句「暂无数据」等于让用户自己猜这个界面是干嘛的。
    #[test]
    fn an_empty_launcher_explains_what_a_project_is_and_where_to_make_one() {
        let joined = texts(&[]).join(" ");
        assert!(joined.contains("项目管理器"), "空态没指路:{joined}");
    }

    /// 点一行 = 请求打开那个项目,而且 **pane 传 `None`**:launcher 态一块
    /// pane 都没有,编一个 `PaneId` 出来的话 `decide_project_open` 会拿它去
    /// 查一块不存在的 pane。
    ///
    /// 自证会变红:把 `Some((p.id, None))` 改成 `None`(第一段红);
    /// 或改成给一个 `Some(PaneId(..))`(第二段红)。
    #[test]
    fn clicking_a_row_asks_to_open_that_project_with_no_pane() {
        let ps = vec![proj(3, "接口", "/srv/api", None)];
        let mut ui_state = crate::ui::UiState::default();
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let lamps = std::collections::BTreeMap::new();
        let sessions = vec![sess(7, "web01")];
        let mut shapes = Vec::new();
        for _ in 0..2 {
            shapes = ctx
                .run(egui::RawInput::default(), |ctx| {
                    show(ctx, &t, &mut ui_state, &ps, &lamps, &sessions);
                })
                .shapes;
        }
        let pos = shapes
            .iter()
            .find_map(|cs| find(&cs.shape, "接口"))
            .expect("列表里没有这个项目");
        let mut input = egui::RawInput::default();
        for pressed in [true, false] {
            input.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            });
        }
        let _ = ctx.run(input, |ctx| {
            show(ctx, &t, &mut ui_state, &ps, &lamps, &sessions);
        });
        assert_eq!(
            ui_state.project_open_request,
            Some((ProjectId(3), None)),
            "点了一行却没请求打开(或编了一个 pane 出来)"
        );
    }

    fn find(shape: &egui::Shape, label: &str) -> Option<egui::Pos2> {
        match shape {
            egui::Shape::Vec(v) => v.iter().find_map(|s| find(s, label)),
            egui::Shape::Text(ts) if ts.galley.text() == label => {
                Some(ts.pos + ts.galley.size() / 2.0)
            }
            _ => None,
        }
    }

    /// 跑两帧收文字。**两帧**:`CentralPanel` 首帧 `fade_in` 只记
    /// `Shape::Noop`(同 `restored` / `files_panel` 那边)。
    fn texts(projects: &[ProjectRecord]) -> Vec<String> {
        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                egui::Shape::Text(ts) => out.push(ts.galley.text().to_string()),
                _ => {}
            }
        }
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut ui_state = crate::ui::UiState::default();
        let lamps = std::collections::BTreeMap::new();
        let sessions = vec![sess(7, "web01")];
        let mut shapes = Vec::new();
        for _ in 0..2 {
            shapes = ctx
                .run(egui::RawInput::default(), |ctx| {
                    show(ctx, &t, &mut ui_state, projects, &lamps, &sessions);
                })
                .shapes;
        }
        let mut out = Vec::new();
        for cs in &shapes {
            walk(&cs.shape, &mut out);
        }
        out
    }
}
