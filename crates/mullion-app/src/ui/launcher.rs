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
    appearance: &crate::ui::badge::AppearanceCache,
) {
    use crate::ui::metrics::{field_w, FIELD_W_M, SP_L, SP_M, SP_S};
    // 「现在几点」一帧取一次,不是每行取一次(同 `project_manager::show`)。
    let now = time::OffsetDateTime::now_utc();
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
            // 搜索框居中,取 M 档宽:整宽的搜索框在宽屏上拉成一条几百像素的
            // 缝,而搜索词通常只有几个字。
            //
            // T8:这个框在 `CentralPanel` 里,**不需要**新增 `Modal` 项 ——
            // launcher 态一块 pane 都没有(`show` 的 `pane` 恒传 `None`),
            // 没有终端跟它抢键盘。
            ui.vertical_centered(|ui| {
                let w = field_w(ui.available_width(), FIELD_W_M, 0.0);
                let r = ui.add(
                    egui::TextEdit::singleline(&mut ui_state.launcher_search)
                        .hint_text(crate::theme::hint_text(t, "搜索项目名 / 目录 / 节点"))
                        .desired_width(w),
                );
                crate::ui::annotate::mark(ui.ctx(), "启动页/搜索框", r.rect);
            });
            ui.add_space(SP_M);
            // 顺序**复用** `by_recent_access`、过滤**复用** `project::matches`
            // —— 三处列表各写一份的话,同一个搜索词在两个界面给出不同结果,
            // 而用户几分钟内就会都看到一遍。
            let rows: Vec<&ProjectRecord> = crate::ui::project_manager::by_recent_access(projects)
                .into_iter()
                .filter(|p| crate::project::matches(p, &ui_state.launcher_search, sessions))
                .collect();
            if rows.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.label(crate::theme::hint_text(t, "没有匹配的项目"));
                    ui.add_space(SP_S);
                    if ui.button("清空搜索").clicked() {
                        ui_state.launcher_search.clear();
                    }
                });
                return;
            }
            egui::ScrollArea::vertical()
                .id_salt("launcher_projects")
                .show(ui, |ui| {
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
                                query: &ui_state.launcher_search,
                                // 启动页没有「正在编辑哪一个」的概念。
                                selected: false,
                                now,
                                list: "launcher",
                                icon: crate::project::icon_for(p, appearance),
                                icon_bg: crate::project::icon_bg(
                                    p,
                                    appearance,
                                    mullion_store::ColorTarget::ListItem,
                                ),
                            },
                        );
                        if r.clicked() {
                            ui_state.project_open_request = Some((p.id, None));
                        }
                        ui.add_space(SP_S);
                    }
                });
        });
    crate::ui::annotate::mark(ctx, "项目列表(启动页)", panel.response.rect);
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
            icon: None,
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
        assert_eq!(
            request_after_click(|name_at| name_at),
            Some((ProjectId(3), None)),
            "点了一行却没请求打开(或编了一个 pane 出来)"
        );
    }

    /// **整行**可点,不是只有那几个字可点。
    ///
    /// 列表里一行有一大片空白(名字右边到行尾),用户瞄准的是「那一条」,
    /// 落点几乎不可能正好在字上。F141 那条「侧栏本地栏一行都点不中」就是
    /// 判定矩形只罩住了内容 —— 而它当时的症状是**完全静默**:界面画得好好
    /// 的,点了没反应。
    ///
    /// 自证会变红:把 `ui.interact(r.rect, ..)` 的矩形换成
    /// `Rect::from_min_size(r.rect.min, vec2(200.0, r.rect.height()))`
    /// —— 名字仍在里面,上一条照样绿,只有这一条红。
    #[test]
    fn the_whole_row_is_clickable_not_just_the_name() {
        let far_right = request_after_click(|name_at| egui::pos2(SCREEN_W - 24.0, name_at.y));
        assert_eq!(
            far_right,
            Some((ProjectId(3), None)),
            "点在行的右半边(名字右边的空白)没反应 —— 判定矩形没罩住整行"
        );
    }

    const SCREEN_W: f32 = 800.0;

    /// 画两帧、找到项目名的位置、按 `aim` 换算出真正的落点、点下去,
    /// 返回这一下产生的打开请求。
    fn request_after_click(
        aim: impl Fn(egui::Pos2) -> egui::Pos2,
    ) -> Option<(ProjectId, Option<mullion_core::layout::PaneId>)> {
        let ps = vec![proj(3, "接口", "/srv/api", None)];
        let mut ui_state = crate::ui::UiState::default();
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let lamps = std::collections::BTreeMap::new();
        let sessions = vec![sess(7, "web01")];
        // 窗口尺寸写死:落点是按它算的,用默认值的话这条断言会随 egui 版本
        // 改默认窗口而漂。
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(SCREEN_W, 600.0));
        let base = || egui::RawInput {
            screen_rect: Some(screen),
            ..Default::default()
        };
        let mut shapes = Vec::new();
        for _ in 0..2 {
            shapes = ctx
                .run(base(), |ctx| {
                    show(
                        ctx,
                        &t,
                        &mut ui_state,
                        &ps,
                        &lamps,
                        &sessions,
                        &crate::ui::badge::AppearanceCache::default(),
                    );
                })
                .shapes;
        }
        let name_at = shapes
            .iter()
            .find_map(|cs| find(&cs.shape, "接口"))
            .expect("列表里没有这个项目");
        let pos = aim(name_at);
        let mut input = base();
        for pressed in [true, false] {
            input.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            });
        }
        let _ = ctx.run(input, |ctx| {
            show(
                ctx,
                &t,
                &mut ui_state,
                &ps,
                &lamps,
                &sessions,
                &crate::ui::badge::AppearanceCache::default(),
            );
        });
        ui_state.project_open_request
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

    /// 启动页也能搜(F233)。这个页面的标题就叫「继续上次的活」,项目一多
    /// 就只能靠滚,而用户心里已经知道自己要哪一个。
    ///
    /// 自证会变红:把 `show` 里的 `.filter(|p| crate::project::matches(..))` 删掉。
    #[test]
    fn the_launcher_filters_by_the_search_box_too() {
        let ps = vec![
            proj(1, "接口", "/srv/api", None),
            proj(2, "数据库", "/srv/db", None),
        ];
        let texts = texts_with(&ps, "数据");
        assert!(
            texts.iter().any(|s| s == "数据库"),
            "命中的行不见了:{texts:?}"
        );
        assert!(
            !texts.iter().any(|s| s == "接口"),
            "没命中的行还在:{texts:?}"
        );
    }

    /// 搜不到时给一句话 + 一个回到全部列表的出口。一整片空白让用户分不清
    /// 「没有匹配」和「项目都没了」。
    ///
    /// 自证会变红:把 `rows.is_empty()` 那个分支删掉。
    #[test]
    fn a_launcher_query_that_matches_nothing_says_so_and_offers_a_way_back() {
        let ps = vec![proj(1, "接口", "/srv/api", None)];
        let joined = texts_with(&ps, "根本没有").join(" ");
        assert!(joined.contains("没有匹配的项目"), "没给空态说明:{joined}");
        assert!(
            joined.contains("清空搜索"),
            "没给回到全部列表的出口:{joined}"
        );
    }

    /// 启动页每一行要显示最后打开时间(F234)—— 这个页面标题是「继续上次
    /// 的活」,却不告诉你上次是什么时候。
    ///
    /// 自证会变红:把 `project_row::show` 换回原来那个只画名字和副标题的
    /// `row` 实现。
    #[test]
    fn each_launcher_row_says_when_it_was_last_opened() {
        let ps = vec![proj(1, "接口", "/srv/api", None)];
        let joined = texts_with(&ps, "").join(" ");
        assert!(joined.contains("从未打开"), "没显示最后打开时间:{joined}");
    }

    /// 跑两帧收文字。**两帧**:`CentralPanel` 首帧 `fade_in` 只记
    /// `Shape::Noop`(同 `restored` / `files_panel` 那边)。
    fn texts(projects: &[ProjectRecord]) -> Vec<String> {
        texts_with(projects, "")
    }

    /// 同上,但先把搜索词填进 `launcher_search`。
    fn texts_with(projects: &[ProjectRecord], query: &str) -> Vec<String> {
        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                egui::Shape::Text(ts) => out.push(ts.galley.text().to_string()),
                _ => {}
            }
        }
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut ui_state = crate::ui::UiState {
            launcher_search: query.to_string(),
            ..Default::default()
        };
        let lamps = std::collections::BTreeMap::new();
        let sessions = vec![sess(7, "web01")];
        let mut shapes = Vec::new();
        for _ in 0..2 {
            shapes = ctx
                .run(egui::RawInput::default(), |ctx| {
                    show(
                        ctx,
                        &t,
                        &mut ui_state,
                        projects,
                        &lamps,
                        &sessions,
                        &crate::ui::badge::AppearanceCache::default(),
                    );
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
