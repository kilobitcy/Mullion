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

/// F288:三列是横着排还是竖着堆。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrangement {
    /// 左中右三栏等宽,各自纵向滚动。
    Columns,
    /// 上中下三段,整页一根滚动条。
    Stacked,
}

/// F288:低于这个内容宽就竖排。
///
/// 960 的来历:三栏等宽时每栏正好 320 = [`FIELD_W_M`](crate::ui::metrics::FIELD_W_M),
/// 再窄下去项目行的副标题(`目录 · 节点名`)和右对齐的时间列就开始互相挤 ——
/// 而那两样正是「认出这是哪个活」的全部依据。
pub const STACK_BELOW: f32 = 960.0;

/// F288:横排还是竖排。**纯函数** —— 判据只有宽度一个,不看任何状态,
/// 所以「窄屏到底画成什么样」能脱离窗口单测(同 `metrics::field_w` 的纪律)。
///
/// 不引入「用户当前选中哪一列」之类的模式状态:那种状态一旦落盘,
/// 宽屏改窄屏时它是什么值就永远说不清了。
pub fn arrangement(available_width: f32) -> Arrangement {
    if available_width < STACK_BELOW {
        Arrangement::Stacked
    } else {
        Arrangement::Columns
    }
}

/// F288:启动页三列要画的全部数据。
///
/// 收成一个结构体而不是继续往 `show` 上加形参:三列各要两三样东西,散着传
/// 是九个同为引用的参数,传反了编译照样过(`&[SessionRecord]` 和
/// `&[ProjectRecord]` 类型不同,但 `groups`/`credentials` 这类彼此不同、
/// 将来加一个同类型的就会出事)。
pub struct Lists<'a> {
    pub projects: &'a [ProjectRecord],
    /// F224:每个项目此刻的运行灯(实时那份,画灯用)。
    pub lamps: &'a std::collections::BTreeMap<ProjectId, crate::project::Lamp>,
    pub sessions: &'a [SessionRecord],
    /// 会话的分组。第二列的顺序要跟会话管理器左栏一致(D7),而那边是先归桶
    /// 再按数组顺序 —— 归桶就要这张表。
    pub groups: &'a [mullion_store::GroupRecord],
    /// F74:引用共享凭据的会话,副标题上那个用户名要从这里查。
    pub credentials: &'a [mullion_store::CredentialRecord],
    /// F288 第三列:已经算好的现场行。由 `app.rs` 读盘算好传进来 ——
    /// `ui/` 这一层零 IO。
    pub history: &'a [crate::ui::history::HistoryRow],
    pub appearance: &'a crate::ui::badge::AppearanceCache,
}

/// 画 launcher 中央区的三列(F288):项目 / 会话 / 历史现场。
///
/// **必须是本帧最后一个 panel 类部件**(同 `restored::show` / `files_panel`):
/// `CentralPanel` 铺满剩余空间,排在别的 panel 之前会把它们挤没。
///
/// 三列各自的点击都只写**意图**,真正拨号/摆标签在 `app.rs`:
/// - 项目 → `ui_state.project_open_request`(`pane` 恒传 `None` —— launcher
///   态一块 pane 都没有,编一个 `PaneId` 出来的话 `decide_project_open`
///   会拿它去查一块不存在的 pane)
/// - 会话 → `ui_state.connect_request`(与会话管理器双击行**同一条通道**)
/// - 现场 → `actions.history`(与恢复弹窗**同一条通道**,`has_real_action`
///   已经为它登记过,处置也已经在 `restore_history` 里)
pub fn show(
    ctx: &egui::Context,
    t: &crate::theme::Theme,
    ui_state: &mut crate::ui::UiState,
    lists: &Lists<'_>,
    actions: &mut crate::ui::UiActions,
) {
    use crate::ui::metrics::{field_w, FIELD_W_M, SP_L, SP_M};
    // 「现在几点」一帧取一次,不是每行取一次(同 `project_manager::show`)。
    let now = time::OffsetDateTime::now_utc();
    // F258:排序读的是这次显示期间冻结的灯,画灯仍用实时的 `lists.lamps`。
    //
    // **在闭包外就把行算完**:`frozen` 借的是 `ui_state.launcher_frozen_lamps`,
    // 带进闭包的话它会跟闭包里对 `ui_state` 其它字段的可变借用打架。
    // F257:列什么 / 空了说什么,与另外两处列表共用 `project_list`。
    // 启动页恒传 `Tab::Active` —— 这里是干活入口,没有 tab(设计 D3);
    // 但搜索穿透归档,搜得到。
    let (project_rows, project_empty) = {
        let frozen = crate::ui::freeze_lamps(&mut ui_state.launcher_frozen_lamps, lists.lamps);
        let tab = crate::ui::project_list::Tab::Active;
        let q = ui_state.launcher_search.as_str();
        (
            crate::ui::project_list::rows(lists.projects, tab, q, lists.sessions, frozen),
            crate::ui::project_list::empty_reason(lists.projects, tab, q, lists.sessions, frozen),
        )
    };
    let panel = egui::CentralPanel::default()
        .frame(egui::Frame::none().fill(crate::theme::c32(t.window_bg)))
        .show(ctx, |ui| {
            ui.add_space(SP_L);
            // D6:三列全空 = 真实的「你还什么都没有」。三条并排的「暂无」
            // 等于让用户自己猜这个界面是干嘛的 —— 换一屏能动手的引导。
            //
            // 判据看的是**三份原始数据**而不是过滤后的行:搜索词恰好一个都
            // 搜不到时列表也是空的,但那时用户显然已经有东西了,再弹一次
            // 「欢迎使用」会很莫名其妙。
            if lists.projects.is_empty() && lists.sessions.is_empty() && lists.history.is_empty() {
                guide(ui, t, ui_state);
                return;
            }
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new("继续上次的活")
                        .size(16.0)
                        .color(crate::theme::c32(t.fg_strong)),
                );
            });
            ui.add_space(SP_M);
            // 搜索框居中,取 M 档宽:整宽的搜索框在宽屏上拉成一条几百像素的
            // 缝,而搜索词通常只有几个字。
            //
            // D4:**一个框过滤三列** —— 用户记得的是「昨天那个活」的名字,
            // 不记得它算项目还是会话。
            //
            // T8:这个框在 `CentralPanel` 里,**不需要**新增 `Modal` 项 ——
            // launcher 态一块 pane 都没有,没有终端跟它抢键盘。
            ui.vertical_centered(|ui| {
                let w = field_w(ui.available_width(), FIELD_W_M, 0.0);
                let r = crate::ui::search_box::search_box(
                    ui,
                    &mut ui_state.launcher_search,
                    egui::Id::new("launcher_search"),
                    crate::theme::hint_text(t, "搜索项目 / 会话 / 现场"),
                    w,
                );
                crate::ui::annotate::mark(ui.ctx(), "启动页/搜索框", r.rect);
            });
            ui.add_space(SP_M);
            let cx = ColumnCtx {
                t,
                now,
                lists,
                project_rows: &project_rows,
                project_empty,
            };
            match arrangement(ui.available_width()) {
                // 横排:三栏等宽,**每栏各自一根滚动条** —— 共用一根的话
                // 最长的那一列会把另外两列的底部一起推到屏幕外。
                Arrangement::Columns => ui.columns(3, |cols| {
                    projects_column(&mut cols[0], &cx, ui_state, true);
                    sessions_column(&mut cols[1], &cx, ui_state, true);
                    history_column(&mut cols[2], &cx, &ui_state.launcher_search, actions, true);
                }),
                // 竖排:整页**一根**滚动条。三段各自再套一个 `ScrollArea`
                // 的话就是嵌套纵向滚动,滚轮到底归谁完全不可预测。
                Arrangement::Stacked => {
                    egui::ScrollArea::vertical()
                        .id_salt("launcher_stacked")
                        .show(ui, |ui| {
                            projects_column(ui, &cx, ui_state, false);
                            ui.add_space(SP_L);
                            sessions_column(ui, &cx, ui_state, false);
                            ui.add_space(SP_L);
                            history_column(ui, &cx, &ui_state.launcher_search, actions, false);
                        });
                }
            }
        });
    crate::ui::annotate::mark(ctx, "项目列表(启动页)", panel.response.rect);
}

/// 三列共用的那点只读上下文。逐个传参的话每个列函数都是六七个形参。
struct ColumnCtx<'a> {
    t: &'a crate::theme::Theme,
    now: time::OffsetDateTime,
    lists: &'a Lists<'a>,
    /// 已经排好序、过滤好的项目行(在 `show` 的闭包外算,见那里的注释)。
    project_rows: &'a [&'a ProjectRecord],
    project_empty: Option<crate::ui::project_list::EmptyReason>,
}

/// 一列的壳:小标题 + (横排时)自己的滚动条。
///
/// `scroll` 由调用方按 [`Arrangement`] 传 —— 竖排时整页已经有一根滚动条,
/// 这里再套一个就是嵌套纵向滚动。
fn column(
    ui: &mut egui::Ui,
    t: &crate::theme::Theme,
    title: &str,
    scroll: bool,
    body: impl FnOnce(&mut egui::Ui),
) {
    use crate::ui::metrics::SP_S;
    ui.label(
        egui::RichText::new(title)
            .size(13.0)
            .color(crate::theme::c32(t.fg_muted)),
    );
    ui.add_space(SP_S);
    if scroll {
        egui::ScrollArea::vertical()
            .id_salt(format!("launcher_col_{title}"))
            .show(ui, body);
    } else {
        body(ui);
    }
}

fn projects_column(
    ui: &mut egui::Ui,
    cx: &ColumnCtx<'_>,
    ui_state: &mut crate::ui::UiState,
    scroll: bool,
) {
    use crate::ui::metrics::{SP_S, SP_XS};
    let t = cx.t;
    column(ui, t, "项目", scroll, |ui| {
        if let Some(reason) = cx.project_empty {
            // `hint_text(t, s: impl Into<String>)` —— 传 `String` 本身,
            // **不要**传 `&String`:泛型参数上不发生 deref coercion,
            // `&String` 不实现 `Into<String>`,那样编译不过。
            ui.label(crate::theme::hint_text(
                t,
                crate::ui::project_list::empty_text(
                    reason,
                    crate::ui::project_list::Surface::Launcher,
                ),
            ));
            if reason == crate::ui::project_list::EmptyReason::NoMatch {
                ui.add_space(SP_XS);
                if ui.button("清空搜索").clicked() {
                    ui_state.launcher_search.clear();
                }
            }
            return;
        }
        for p in cx.project_rows {
            let lamp = cx
                .lists
                .lamps
                .get(&p.id)
                .copied()
                .unwrap_or(crate::project::Lamp::Unknown);
            let r = crate::ui::project_row::show(
                ui,
                t,
                &crate::ui::project_row::Row {
                    project: p,
                    lamp,
                    sessions: cx.lists.sessions,
                    query: &ui_state.launcher_search,
                    // 启动页没有「正在编辑哪一个」的概念。
                    selected: false,
                    now: cx.now,
                    list: "launcher",
                    icon: crate::project::icon_for(p, cx.lists.appearance),
                    icon_bg: crate::project::icon_bg(
                        p,
                        cx.lists.appearance,
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
}

/// 第二列的顺序(D7):**跟会话管理器左栏同一份** —— 先按分组归桶、桶内按
/// 数组顺序(F121「数组顺序即真值」,用户亲手拖过)。
///
/// 自己另写一套排序的话,同一批会话在启动页和会话管理器里是两个顺序,而这
/// 两个列表用户几分钟内就会都看到一遍(`project_row` 文件头记的同一个教训)。
///
/// 与会话管理器唯一的差别是**不按协议分页** —— 启动页只有一列,分页的是那个
/// 弹窗自己的事。
pub fn session_order<'a>(
    sessions: &'a [SessionRecord],
    groups: &[mullion_store::GroupRecord],
    query: &str,
) -> Vec<&'a SessionRecord> {
    crate::ui::group_manager::group_sessions(groups, sessions)
        .into_iter()
        .flat_map(|(_, bucket)| bucket)
        .filter(|r| crate::ui::session_manager::list::matches(r, query))
        .collect()
}

/// 第二列一行的 egui id。
pub fn session_row_id(id: mullion_store::SessionId) -> egui::Id {
    egui::Id::new(("mullion_launcher_session", id))
}

/// 一行会话的副标题。
///
/// 用 [`mullion_store::display_user`] 而不是直接读 `auth`:引用共享凭据的
/// 会话身上没有用户名,那样会画出一个空的 `@host`(F74)。
///
/// **不追加** `dedupe::disambiguate` 那段区分信息:那是会话管理器的功能,
/// 而它是 `pub(super)`。启动页这一列是「挑一台机器连上去」,重名的代价是
/// 多看一眼,不值得为它把那个模块的可见性放开。
pub fn session_subtitle(
    r: &SessionRecord,
    credentials: &[mullion_store::CredentialRecord],
) -> String {
    format!(
        "{}@{}",
        mullion_store::display_user(&r.auth, credentials),
        r.connection.host
    )
}

fn sessions_column(
    ui: &mut egui::Ui,
    cx: &ColumnCtx<'_>,
    ui_state: &mut crate::ui::UiState,
    scroll: bool,
) {
    use crate::ui::metrics::{SP_S, SP_XS};
    let t = cx.t;
    let rows = session_order(
        cx.lists.sessions,
        cx.lists.groups,
        &ui_state.launcher_search,
    );
    column(ui, t, "会话", scroll, |ui| {
        if rows.is_empty() {
            ui.label(crate::theme::hint_text(
                t,
                if cx.lists.sessions.is_empty() {
                    "还没有会话"
                } else {
                    "没有匹配的会话"
                },
            ));
        }
        for r in &rows {
            // 整行可点(F141 那条「侧栏本地栏一行都点不中」的教训):
            // 占位只占位,点击判定交给挂了显式 id 的 `interact`。
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), crate::ui::project_row::ROW_H),
                egui::Sense::hover(),
            );
            let resp = ui.interact(rect, session_row_id(r.id), egui::Sense::click());
            // 底色与会话管理器左栏同源 —— 那边是这个函数,这里直接调它
            // (`pub(crate)`,同一个 crate 内)。节点色传 `None`:启动页这
            // 一列没有选中态,只有悬停。
            if let Some(bg) =
                crate::ui::session_manager::list::row_bg(false, resp.hovered(), None, t)
            {
                ui.painter()
                    .rect_filled(rect, egui::Rounding::same(4.0), bg);
            }
            let p = ui.painter();
            // F293:图标槽。几何与同屏的项目列**同源**(`project_row::ICON_X /
            // ICON_SIDE / TEXT_X`)—— 三列并排,名字左沿必须对齐。槽位**恒定**:
            // 有图标画、没有留空,不画任何占位。图标来源与会话管理器左栏一样
            // 走 `AppearanceCache`(已含会话→分组继承),底色同 `should_paint`。
            let appearance = cx.lists.appearance.get(r.id);
            if let Some(icon) = appearance.and_then(|a| a.icon.as_ref()) {
                use crate::ui::project_row::{ICON_SIDE, ICON_X};
                let slot = egui::Rect::from_center_size(
                    egui::pos2(rect.left() + ICON_X + ICON_SIDE / 2.0, rect.center().y),
                    egui::vec2(ICON_SIDE, ICON_SIDE),
                );
                let bg = appearance.and_then(|a| {
                    crate::ui::badge::should_paint(a, mullion_store::ColorTarget::ListItem)
                });
                crate::ui::badge::paint_icon(p, slot, icon, bg);
            }
            let left = rect.left() + crate::ui::project_row::TEXT_X;
            let avail = (rect.width()
                - crate::ui::project_row::TEXT_X
                - crate::ui::project_row::TEXT_RIGHT_PAD)
                .max(0.0);
            // 名称走命中高亮 —— 一个框过滤三列(D4),用户要看得出为什么
            // 这一行留下来了。与会话管理器左栏同一个函数。
            crate::ui::session_manager::list::paint_highlighted(
                p,
                egui::pos2(left, rect.top() + 6.0),
                &r.identity.name,
                &ui_state.launcher_search,
                egui::FontId::proportional(14.0),
                crate::theme::c32(t.fg_strong),
                t,
                avail,
            );
            crate::ui::session_manager::list::paint_highlighted(
                p,
                egui::pos2(left, rect.top() + 27.0),
                &session_subtitle(r, cx.lists.credentials),
                &ui_state.launcher_search,
                egui::FontId::proportional(11.0),
                crate::theme::c32(t.fg_muted),
                t,
                avail,
            );
            if resp.clicked() {
                // D2:点一行就连。与会话管理器双击行**同一条通道** ——
                // 另开一条的话「连接」这件事就有两套处置了。
                ui_state.connect_request = Some(r.id);
            }
            ui.add_space(SP_XS);
        }
        // D2:CRUD 一行不搬。想改配置去那个弹窗,这里只给一个入口。
        ui.add_space(SP_S);
        if ui.button("管理会话…").clicked() {
            ui_state.session_manager_open = true;
        }
    });
}

/// `query` 单独传而不是放进 [`ColumnCtx`]:放进去的话它会一直借着
/// `ui_state.launcher_search`,跟前两列对 `ui_state` 的可变借用打架。
/// 三列是**依次**调用的,分开传就各借各的。
fn history_column(
    ui: &mut egui::Ui,
    cx: &ColumnCtx<'_>,
    query: &str,
    actions: &mut crate::ui::UiActions,
    scroll: bool,
) {
    use crate::ui::metrics::SP_XS;
    let t = cx.t;
    let rows: Vec<&crate::ui::history::HistoryRow> = cx
        .lists
        .history
        .iter()
        .filter(|r| history_matches(r, query))
        .collect();
    column(ui, t, "历史现场", scroll, |ui| {
        if rows.is_empty() {
            ui.label(crate::theme::hint_text(
                t,
                if cx.lists.history.is_empty() {
                    "没有可恢复的现场"
                } else {
                    "没有匹配的现场"
                },
            ));
        }
        for r in rows {
            // 启动页这一列没有「选中第几条」的概念(D3:单击当场恢复),
            // `selected` 恒传 `false`。
            let resp = crate::ui::history::row(ui, t, crate::ui::history::LAUNCHER_LIST, r, false);
            if resp.clicked() {
                // D3:单击当场恢复。走弹窗**同一条通道** ——
                // `has_real_action` 已经登记过 `a.history`,处置也已经在
                // `restore_history` 里。
                //
                // `get_or_insert`:恢复弹窗也能在 launcher 态开着(菜单里
                // 有常驻入口),它排在本函数之前画。同一帧两边都有结论时,
                // 用户点的是浮在上面那个弹窗。
                actions
                    .history
                    .get_or_insert(crate::ui::history::HistoryOut::Restore(r.id.clone()));
            }
            ui.add_space(SP_XS);
        }
    });
}

/// 一条现场记录命中搜索了吗。
///
/// 匹配面就是**画出来的那几行字**(时间/标签数 + 会话名摘要 + 标注)——
/// 现场记录没有名字,用户能拿来搜的只有他在列表上看见的东西。
///
/// 分词走 [`crate::search::matches_all`],**和另外两列同一份** —— 只改一处的
/// 话同一个词 `web 01` 在项目列找得到、在这里找不到(F245 在会话侧记过同一条)。
pub fn history_matches(r: &crate::ui::history::HistoryRow, query: &str) -> bool {
    crate::search::matches_all(
        query,
        &[r.head.as_str(), r.summary.as_str(), r.note.as_str()],
    )
}

/// D6:三列全空时的那一屏。
///
/// 首次运行原本会自动弹会话管理器(F148 D9),F288 取消了那条 —— 取消之后
/// 这一屏必须自己说清「这是什么、从哪儿开始」,否则用户看到的是一片空白。
fn guide(ui: &mut egui::Ui, t: &crate::theme::Theme, ui_state: &mut crate::ui::UiState) {
    use crate::ui::metrics::{SP_L, SP_M};
    ui.vertical_centered(|ui| {
        ui.label(
            egui::RichText::new("还没有任何会话")
                .size(16.0)
                .color(crate::theme::c32(t.fg_strong)),
        );
        ui.add_space(SP_M);
        ui.label(crate::theme::hint_text(
            t,
            "Mullion 用会话记住一台机器怎么连。建好之后,这里会列出你的项目、会话和上次关掉的现场。",
        ));
        ui.add_space(SP_L);
        ui.horizontal(|ui| {
            if ui
                .add(egui::Button::new("新建会话").min_size([120.0, 30.0].into()))
                .clicked()
            {
                // 两句缺一不可:只开弹窗的话用户看到的还是一个空列表,
                // 还得自己找那个 `+`;只发 `pending_switch` 的话没人消费它
                // (它在 `session_manager::show` 里才被取走)。
                ui_state.session_manager_open = true;
                ui_state.pending_switch = Some(crate::ui::session_manager::SwitchTarget::NewDraft);
            }
            ui.add_space(crate::ui::metrics::SP_S);
            // F2:已经有 ssh config 的人不该一条条重建。
            if ui.button("从 ssh config 导入…").clicked() {
                ui_state.import_pick_request = true;
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_store::{Protocol, SessionId};

    /// 测试统一的调用口:把散着的几张表凑成 [`Lists`] 再调 `show`。
    ///
    /// 不让每条测试自己拼 `Lists`:往里加一个字段就要改十几处,而那十几处
    /// 里只要有一处填错(比如把 `history` 填成空表)就是**静默**的少画一列。
    fn draw(
        ctx: &egui::Context,
        ui_state: &mut crate::ui::UiState,
        projects: &[ProjectRecord],
        lamps: &std::collections::BTreeMap<ProjectId, crate::project::Lamp>,
        sessions: &[SessionRecord],
        history: &[crate::ui::history::HistoryRow],
        actions: &mut crate::ui::UiActions,
    ) {
        show(
            ctx,
            &crate::theme::MULLION_DARK,
            ui_state,
            &Lists {
                projects,
                lamps,
                sessions,
                groups: &[],
                credentials: &[],
                history,
                appearance: &crate::ui::badge::AppearanceCache::default(),
            },
            actions,
        );
    }

    /// F288:窄到三栏装不下时改成竖排,而不是把每栏挤到读不出来。
    ///
    /// 判据落在这个纯函数上而不是靠人眼看截图:这一条是**本项目验证不了**
    /// 的那一类(GPU 渲染是否好看)里唯一能机械判定的部分 —— 把「该横还是
    /// 该竖」从画面里剥出来,它就只是一个宽度比较。
    ///
    /// 阈值两侧各一点 + 边界本身:只测两头的话,把判据写成 `<=` 还是 `<`
    /// 这种差一位的错永远测不出来。
    ///
    /// 自证会变红:把 `arrangement` 的函数体改成恒返回 `Columns`。
    #[test]
    fn a_page_too_narrow_for_three_columns_stacks_them_instead_of_squeezing() {
        assert_eq!(arrangement(2560.0), Arrangement::Columns, "宽屏该横排");
        assert_eq!(
            arrangement(STACK_BELOW),
            Arrangement::Columns,
            "正好等于阈值时每栏恰好 320,还够用,该横排"
        );
        assert_eq!(
            arrangement(STACK_BELOW - 1.0),
            Arrangement::Stacked,
            "刚跌破阈值就该竖排 —— 差一位的边界错只有这一条测得出来"
        );
        assert_eq!(arrangement(640.0), Arrangement::Stacked, "小屏该竖排");
    }

    fn hist(id: &str, head: &str, summary: &str) -> crate::ui::history::HistoryRow {
        crate::ui::history::HistoryRow {
            id: id.into(),
            head: head.into(),
            summary: summary.into(),
            note: String::new(),
        }
    }

    /// 三列各自**真被画出来的**那些行。
    ///
    /// 判据一律走 `read_response` 查行 id 有没有被 `ui.interact` 过,不比对
    /// 画出来的文字:搜索框里的字本身也是一段 `Shape::Text`,查询词恰好等于
    /// 某个名字时会平白多算一条(同本文件 `names_drawn` 的注释)。
    ///
    /// **`read_response` 必须在 `ctx.run` 闭包内读**,理由同上。
    #[allow(clippy::type_complexity)]
    fn columns_drawn(
        projects: &[ProjectRecord],
        sessions: &[SessionRecord],
        history: &[crate::ui::history::HistoryRow],
        query: &str,
    ) -> (Vec<u64>, Vec<u64>, Vec<String>) {
        let ctx = egui::Context::default();
        let mut ui_state = crate::ui::UiState {
            launcher_search: query.to_string(),
            ..Default::default()
        };
        let lamps = std::collections::BTreeMap::new();
        let mut actions = crate::ui::UiActions::default();
        let mut out = (Vec::new(), Vec::new(), Vec::new());
        for _ in 0..2 {
            let _ = ctx.run(wide(), |ctx| {
                draw(
                    ctx,
                    &mut ui_state,
                    projects,
                    &lamps,
                    sessions,
                    history,
                    &mut actions,
                );
                out = (
                    projects
                        .iter()
                        .filter(|p| {
                            ctx.read_response(crate::ui::project_row::row_id("launcher", p.id))
                                .is_some()
                        })
                        .map(|p| p.id.0)
                        .collect(),
                    sessions
                        .iter()
                        .filter(|s| ctx.read_response(session_row_id(s.id)).is_some())
                        .map(|s| s.id.0)
                        .collect(),
                    history
                        .iter()
                        .filter(|h| {
                            ctx.read_response(crate::ui::history::row_id(
                                crate::ui::history::LAUNCHER_LIST,
                                &h.id,
                            ))
                            .is_some()
                        })
                        .map(|h| h.id.clone())
                        .collect(),
                );
            });
        }
        out
    }

    /// 宽到走横排那一支的输入([`STACK_BELOW`] 以上)。写死尺寸:用默认值的
    /// 话这几条会随 egui 换默认窗口大小而在横/竖之间漂。
    fn wide() -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1600.0, 900.0),
            )),
            ..Default::default()
        }
    }

    /// F288 的核心交付:**三列各自真的有东西**。
    ///
    /// 这一条是整个切片唯一一条「三列同时在场」的判据 —— 少画一整列在无头
    /// 环境下是完全静默的(另外两列照常,测试照绿)。
    ///
    /// 自证会变红:把 `show` 里 `sessions_column(&mut cols[1], ..)` 那一行
    /// 删掉(第二段红),或 `history_column(&mut cols[2], ..)`(第三段红)。
    #[test]
    fn all_three_columns_draw_their_own_rows() {
        let ps = vec![proj(1, "接口", "/srv/api", None)];
        let ss = vec![sess(7, "web01")];
        let hs = vec![hist("inst-a", "3 小时前 · 2 个标签", "web01 · db01")];
        let (p, s, h) = columns_drawn(&ps, &ss, &hs, "");
        assert_eq!(p, vec![1], "第一列(项目)没画出来");
        assert_eq!(s, vec![7], "第二列(会话)没画出来");
        assert_eq!(h, vec!["inst-a".to_string()], "第三列(历史现场)没画出来");
    }

    /// D6:**三列全空**才换引导屏。三样里只要有一样非空,用户就已经有东西
    /// 了,再弹一屏「还没有任何会话」很莫名其妙。
    ///
    /// 四种组合都测:门控写成「项目空就引导」这种漏一档的写法,只测全空 +
    /// 全满的话照样绿(本库「列举式门控在加档时必然漏」已经踩中过四次)。
    ///
    /// 自证会变红:把 `show` 里那个 `if` 的三段条件去掉任意一段。
    #[test]
    fn the_guide_screen_shows_up_only_when_all_three_lists_are_empty() {
        let guide_shown =
            |ps: &[ProjectRecord], ss: &[SessionRecord], hs: &[crate::ui::history::HistoryRow]| {
                texts_full(ps, ss, hs, "")
                    .join(" ")
                    .contains("还没有任何会话")
            };
        let ps = vec![proj(1, "接口", "/srv/api", None)];
        let ss = vec![sess(7, "web01")];
        let hs = vec![hist("inst-a", "3 小时前", "web01")];
        assert!(guide_shown(&[], &[], &[]), "三列全空时没给引导屏");
        assert!(!guide_shown(&ps, &[], &[]), "只有项目时不该弹引导屏");
        assert!(!guide_shown(&[], &ss, &[]), "只有会话时不该弹引导屏");
        assert!(!guide_shown(&[], &[], &hs), "只有现场时不该弹引导屏");
    }

    /// 引导屏得说清**从哪儿开始**,而不是一句「暂无数据」。
    ///
    /// F288 取消了首次运行自动弹会话管理器(D1),取消之后这一屏是新用户
    /// 看到的**全部内容** —— 没有出口就等于死路。
    ///
    /// 自证会变红:把 `guide` 里那个「新建会话」按钮删掉。
    #[test]
    fn the_guide_screen_offers_a_way_to_make_the_first_session() {
        let joined = texts_full(&[], &[], &[], "").join(" ");
        assert!(
            joined.contains("新建会话"),
            "引导屏没给建会话的出口:{joined}"
        );
        assert!(
            joined.contains("ssh config"),
            "引导屏没给导入的出口 —— 已经有 ssh config 的人不该一条条重建:{joined}"
        );
    }

    /// D4:**一个框过滤三列**。用户记得的是「昨天那个活」的名字,不记得它
    /// 算项目、会话还是现场。
    ///
    /// 判据用一个**只命中第二列**的词:三列共用一个过滤函数还是各用各的,
    /// 只有在「某一列该空、另两列不该空」时才分得开。只测「命中的那列还在」
    /// 的话,哪一列压根没接搜索都测不出来。
    ///
    /// 自证会变红:把 `sessions_column` 里 `session_order(..)` 的 `query`
    /// 实参换成 `""`(第一段红);或把 `history_column` 的
    /// `.filter(|r| history_matches(r, query))` 删掉(第三段红)。
    ///
    /// 会话故意用一个**不在任何项目 `nodes` 里**的 id:项目搜索是穿透到节点
    /// 名的(F233),拿项目自己的节点名当查询词的话第一段必然红,而那是正确
    /// 行为不是缺陷。
    #[test]
    fn one_search_box_filters_all_three_columns() {
        let ps = vec![proj(1, "接口", "/srv/api", None)];
        // 第二条会话是**不命中**的那条:少了它,「会话列压根没接搜索」这
        // 个变异杀不掉(命中的那条无论过不过滤都在)。
        let ss = vec![sess(9, "独立机"), sess(11, "别的机器")];
        let hs = vec![hist("inst-a", "3 小时前", "数据库")];
        let (p, s, h) = columns_drawn(&ps, &ss, &hs, "独立机");
        assert!(p.is_empty(), "搜索词只命中会话,项目列该空:{p:?}");
        assert_eq!(s, vec![9], "会话列没按搜索词过滤(或命中的那条不见了)");
        assert!(h.is_empty(), "搜索词只命中会话,现场列该空:{h:?}");
    }

    /// 「行右半边的空白」那个落点。
    ///
    /// **不能写成 `rect.right_center()`** —— 那个 `rect` 就是被测的判定矩形
    /// 本身,把它缩窄的变异会让落点跟着缩回去,断言照绿(判据与被测量同源
    /// 平移 = 恒绿)。这里改成从行的**左边**往右量一个固定距离,而这个距离
    /// 的参照物是 [`wide`] 那块写死的屏宽(1600 / 三栏 ≈ 520,行宽比它略窄),
    /// 与判定矩形无关。
    fn blank_half(rect: egui::Rect) -> egui::Pos2 {
        egui::pos2(rect.left() + 320.0, rect.center().y)
    }

    /// 画两帧、量出 `target` 那一行的矩形、按 `aim` 换算落点点下去,返回这
    /// 一下产生的两条意图(会话 / 现场)。
    #[allow(clippy::type_complexity)]
    fn click_in_row(
        target: egui::Id,
        aim: impl Fn(egui::Rect) -> egui::Pos2,
    ) -> (Option<SessionId>, Option<crate::ui::history::HistoryOut>) {
        let ps = vec![proj(1, "接口", "/srv/api", None)];
        let ss = vec![sess(9, "独立机")];
        let hs = vec![hist("inst-a", "3 小时前", "数据库")];
        let ctx = egui::Context::default();
        let mut ui_state = crate::ui::UiState::default();
        let lamps = std::collections::BTreeMap::new();
        let mut rect = None;
        for _ in 0..2 {
            let mut actions = crate::ui::UiActions::default();
            let _ = ctx.run(wide(), |ctx| {
                draw(ctx, &mut ui_state, &ps, &lamps, &ss, &hs, &mut actions);
                rect = ctx.read_response(target).map(|r| r.rect);
            });
        }
        let pos = aim(rect.expect("这一行根本没画出来"));
        let mut input = wide();
        for pressed in [true, false] {
            input.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            });
        }
        let mut actions = crate::ui::UiActions::default();
        let _ = ctx.run(input, |ctx| {
            draw(ctx, &mut ui_state, &ps, &lamps, &ss, &hs, &mut actions);
        });
        (ui_state.connect_request, actions.history)
    }

    /// D2:第二列点一行就**连**,而且走的是 `connect_request` ——
    /// 与会话管理器双击行同一条通道(另开一条就有两套处置了)。
    ///
    /// 同时钉住**整行可点**:一行右半边全是空白,用户瞄准的是「那一条」,
    /// 落点几乎不可能正好压在字上。F141 那条「侧栏本地栏一行都点不中」
    /// 当时的症状是完全静默 —— 画得好好的,点了没反应。
    ///
    /// 自证会变红:把 `sessions_column` 里 `ui_state.connect_request = ..`
    /// 那一行删掉(两段都红);或把 `ui.interact(rect, ..)` 的矩形缩成只罩
    /// 住左边 80 点(只有第二段红)。
    #[test]
    fn clicking_a_session_row_asks_to_connect_and_the_whole_row_is_clickable() {
        let id = session_row_id(SessionId(9));
        let (on_text, _) = click_in_row(id, |r| r.left_center() + egui::vec2(8.0, 0.0));
        assert_eq!(on_text, Some(SessionId(9)), "点在名字上没发出连接请求");
        let (on_blank, _) = click_in_row(id, blank_half);
        assert_eq!(
            on_blank,
            Some(SessionId(9)),
            "点在行的右半边(名字右边的空白)没反应 —— 判定矩形没罩住整行"
        );
    }

    /// D3:第三列**单击当场恢复**,走 `actions.history` —— 与恢复弹窗同一条
    /// 通道(`has_real_action` 已经为它登记过,处置也已经在 `restore_history`
    /// 里)。新开一条通道的话得在那两处各补一遍,而漏了任何一处都是静默的
    /// 「点了没反应」。
    ///
    /// 同样钉住整行可点,理由同上一条。
    ///
    /// 自证会变红:把 `history_column` 里 `actions.history.get_or_insert(..)`
    /// 那一段删掉;或把 `history::row` 里 `ui.interact` 的矩形缩窄。
    #[test]
    fn clicking_a_history_row_asks_to_restore_it_and_the_whole_row_is_clickable() {
        let id = crate::ui::history::row_id(crate::ui::history::LAUNCHER_LIST, "inst-a");
        let want = Some(crate::ui::history::HistoryOut::Restore("inst-a".into()));
        let (_, on_text) = click_in_row(id, |r| r.left_center() + egui::vec2(8.0, 0.0));
        assert_eq!(on_text, want, "点在文字上没发出恢复请求");
        let (_, on_blank) = click_in_row(id, blank_half);
        assert_eq!(on_blank, want, "点在行的右半边没反应 —— 判定矩形没罩住整行");
    }

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
            last_connected_at: None,
        }
    }

    /// 列表顺序**复用** `project_list::rows`(内部按 `segment_order`/
    /// `newest_first` 排序,最近访问的在最上面)——`show()` 里调的就是它
    /// (见本文件 `show()` 里 `crate::ui::project_list::rows(..)` 那一行)。
    ///
    /// 自己再写一遍排序的话,项目管理器左栏和这里会给出两个顺序 —— 而这
    /// 两个列表用户几分钟内就会都看到一遍。
    ///
    /// 自证会变红:把 `project_list.rs` 里 `newest_first` 的
    /// `y.cmp(x)` 改成 `x.cmp(y)`(比较方向反过来,最近访问的排到最后面)。
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
        let ctx = egui::Context::default();
        let lamps = std::collections::BTreeMap::new();
        let sessions = vec![sess(7, "web01")];
        let mut actions = crate::ui::UiActions::default();
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
                    draw(
                        ctx,
                        &mut ui_state,
                        &ps,
                        &lamps,
                        &sessions,
                        &[],
                        &mut actions,
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
            draw(
                ctx,
                &mut ui_state,
                &ps,
                &lamps,
                &sessions,
                &[],
                &mut actions,
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

    /// 启动页默认只列「在用」的 —— 归档项目在干活入口里不该碍事(设计 D3)。
    /// 但**搜得到**:搜索穿透归档(设计 D4)。
    ///
    /// 自证会变红:把 `Tab::Active` 换成把 `projects` 直接喂给 `project_row`。
    #[test]
    fn the_launcher_hides_archived_projects_until_you_search_for_them() {
        let ps = vec![
            proj_archived(1, "老活", "/data/old"),
            proj(2, "在做的", "/data/now", None),
        ];
        assert_eq!(names_drawn(&ps, ""), vec!["在做的"], "归档的不该默认出现");
        assert_eq!(
            names_drawn(&ps, "老活"),
            vec!["老活"],
            "搜索必须能搜到归档的"
        );
    }

    /// 库里有项目、只是全归档了 —— 启动页不能喊「还没有项目,去建一个」。
    #[test]
    fn a_launcher_with_only_archived_projects_does_not_tell_you_to_create_one() {
        let ps = vec![proj_archived(1, "老活", "/data/old")];
        let text = drawn_text(&ps, "");
        assert!(text.contains("没有在用的项目"), "{text}");
        assert!(!text.contains("还没有项目"), "{text}");
    }

    /// 跑两帧收文字。**两帧**:`CentralPanel` 首帧 `fade_in` 只记
    /// `Shape::Noop`(同 `restored` / `files_panel` 那边)。
    fn texts(projects: &[ProjectRecord]) -> Vec<String> {
        texts_with(projects, "")
    }

    /// 哪些项目的行**真被画出来了**——不靠比对画出来的文字:搜索框里的字
    /// 本身也是一段 `Shape::Text`,查询词恰好等于项目名时(本文件的
    /// `the_launcher_hides_archived_projects_until_you_search_for_them` 就是
    /// 这种情况)会把搜索框那份也算进去,平白多算一条。改用 `read_response`
    /// 查每个项目那一行的 id 有没有被 `ui.interact` 过 —— 那才是「这一行
    /// 存在」的真凭据,姿态同 `project_pick::tests` 的 `draw`。
    ///
    /// **`read_response` 必须在 `ctx.run` 的闭包内部调用,在 `show(...)` 之后
    /// 立刻读。** `run()` 返回之后再读,拿到的是上上一帧(N-2)的陈旧记录 ——
    /// 原理和实测数字见 `session_manager/mod.rs:2448` 那条注释(`this_pass`/
    /// `prev_pass` 在 `end_pass` 里 `mem::swap`,`read_response` 优先命中
    /// `this_pass`,在闭包外读到的其实是 swap 之前的旧值)。
    fn names_drawn(projects: &[ProjectRecord], query: &str) -> Vec<String> {
        let ctx = egui::Context::default();
        let mut ui_state = crate::ui::UiState {
            launcher_search: query.to_string(),
            ..Default::default()
        };
        let lamps = std::collections::BTreeMap::new();
        let sessions = vec![sess(7, "web01")];
        let mut actions = crate::ui::UiActions::default();
        let mut drawn = Vec::new();
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                draw(
                    ctx,
                    &mut ui_state,
                    projects,
                    &lamps,
                    &sessions,
                    &[],
                    &mut actions,
                );
                drawn = projects
                    .iter()
                    .filter(|p| {
                        ctx.read_response(crate::ui::project_row::row_id("launcher", p.id))
                            .is_some()
                    })
                    .map(|p| p.name.clone())
                    .collect();
            });
        }
        drawn
    }

    /// 收全部画出来的文字、拼成一句话 —— 用来断言空态文案。
    fn drawn_text(projects: &[ProjectRecord], query: &str) -> String {
        texts_with(projects, query).join(" ")
    }

    /// 同上,但先把搜索词填进 `launcher_search`。会话固定给一条 ——
    /// 三列**全空**才走引导屏(D6),这里要测的是项目那一列。
    fn texts_with(projects: &[ProjectRecord], query: &str) -> Vec<String> {
        texts_full(projects, &[sess(7, "web01")], &[], query)
    }

    /// 三张表都自己给。跑两帧收全部画出来的文字。
    fn texts_full(
        projects: &[ProjectRecord],
        sessions: &[SessionRecord],
        history: &[crate::ui::history::HistoryRow],
        query: &str,
    ) -> Vec<String> {
        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                egui::Shape::Text(ts) => out.push(ts.galley.text().to_string()),
                _ => {}
            }
        }
        let ctx = egui::Context::default();
        let mut ui_state = crate::ui::UiState {
            launcher_search: query.to_string(),
            ..Default::default()
        };
        let lamps = std::collections::BTreeMap::new();
        let mut actions = crate::ui::UiActions::default();
        let mut shapes = Vec::new();
        for _ in 0..2 {
            shapes = ctx
                .run(egui::RawInput::default(), |ctx| {
                    draw(
                        ctx,
                        &mut ui_state,
                        projects,
                        &lamps,
                        sessions,
                        history,
                        &mut actions,
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

    /// D13 接线判据(复核 Critical):`show` 传给 `project_list::rows` 的必须
    /// 是**冻结的灯**(`ui_state.launcher_frozen_lamps`),不是每帧的实时
    /// `lamps` 表 —— 姿态同 `project_manager::tests` 里同名判据那条,理由
    /// 见该处注释(灯异步变、按实时灯排会在用户要点某一行时把它挤下去)。
    ///
    /// **`ctx.read_response` 必须在 `ctx.run` 闭包内部读**,理由同本文件
    /// `names_drawn` 的注释。
    ///
    /// 自证会变红:把本文件 `show` 里 `project_list::rows(..)` 最后一个
    /// 实参从 `frozen` 换成 `lamps`。
    #[test]
    fn the_launcher_list_order_is_pinned_to_the_lamps_frozen_when_first_shown() {
        let ps = vec![
            proj(1, "老项目", "/srv/old", Some("2026-09-01T00:00:00Z")),
            proj(2, "新项目", "/srv/new", Some("2026-09-10T00:00:00Z")),
        ];
        let ctx = egui::Context::default();
        let mut ui_state = crate::ui::UiState::default();
        let sessions = vec![sess(7, "web01")];
        let mut live: std::collections::BTreeMap<ProjectId, crate::project::Lamp> = [
            (ProjectId(1), crate::project::Lamp::Dark),
            (ProjectId(2), crate::project::Lamp::Dark),
        ]
        .into_iter()
        .collect();

        let order = |ctx: &egui::Context,
                     ui_state: &mut crate::ui::UiState,
                     live: &std::collections::BTreeMap<ProjectId, crate::project::Lamp>|
         -> Vec<u64> {
            let mut rows: Vec<(u64, f32)> = Vec::new();
            let mut actions = crate::ui::UiActions::default();
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                draw(ctx, ui_state, &ps, live, &sessions, &[], &mut actions);
                for id in [1u64, 2] {
                    if let Some(r) =
                        ctx.read_response(crate::ui::project_row::row_id("launcher", ProjectId(id)))
                    {
                        rows.push((id, r.rect.top()));
                    }
                }
            });
            rows.sort_by(|a, b| a.1.total_cmp(&b.1));
            rows.into_iter().map(|(id, _)| id).collect()
        };

        // 预热,不改活灯表(同本文件其它多帧测试:`CentralPanel` 首帧还在
        // fade_in)。
        order(&ctx, &mut ui_state, &live);
        let frame1 = order(&ctx, &mut ui_state, &live);
        assert_eq!(frame1, vec![2, 1], "都灭灯时该按最近访问排,新的在前");

        // 只改活的灯表,不碰 ui_state / 不重新打开。
        live.insert(ProjectId(1), crate::project::Lamp::Lit);
        let frame2 = order(&ctx, &mut ui_state, &live);
        assert_eq!(
            frame2, frame1,
            "排序该读冻结的灯,活灯表变了不该让行序跟着跳"
        );

        // 反向断言:清掉冻结槽,等价于「离开启动页再回来」。
        ui_state.launcher_frozen_lamps = None;
        let frame3 = order(&ctx, &mut ui_state, &live);
        assert_eq!(frame3, vec![1, 2], "重新冻结后,亮着灯的项目该置顶");
    }

    // ---- F293:会话列图标 ---------------------------------------------------

    /// 只画会话列要用的那几张表,返回两帧后的全部 shape。
    ///
    /// 项目 / 历史两列都给空表:这两列里没有任何图片,于是「画面上出现了一张
    /// 图」这个判据只可能来自会话列。
    fn session_shapes(sessions: &[SessionRecord]) -> Vec<egui::epaint::ClippedShape> {
        let ctx = egui::Context::default();
        let mut ui_state = crate::ui::UiState::default();
        let lamps = std::collections::BTreeMap::new();
        let mut actions = crate::ui::UiActions::default();
        let mut cache = crate::ui::badge::AppearanceCache::default();
        cache.rebuild(sessions, &[]);
        let mut out = Vec::new();
        for _ in 0..2 {
            out = ctx
                .run(wide(), |ctx| {
                    show(
                        ctx,
                        &crate::theme::MULLION_DARK,
                        &mut ui_state,
                        &Lists {
                            projects: &[],
                            lamps: &lamps,
                            sessions,
                            groups: &[],
                            credentials: &[],
                            history: &[],
                            appearance: &cache,
                        },
                        &mut actions,
                    );
                })
                .shapes;
        }
        out
    }

    /// 给会话挂一张**真** ico:`paint_icon` 会先解码,解不开就整段不画,
    /// 拿假 base64 的话测试会因为「解码失败」而假红。
    fn with_icon(mut r: SessionRecord) -> SessionRecord {
        r.appearance.icon = Some(mullion_store::IconSpec {
            kind: mullion_store::IconKind::Ico,
            value: crate::ui::ico::import(&crate::ui::ico::tests_support::solid_ico(
                32,
                [255, 0, 0, 255],
            ))
            .expect("测试用 ico 应能导入"),
            bg: None,
        });
        r
    }

    /// 第一张「带真纹理且有面积」的 Mesh 的包围盒。判据照抄
    /// `project_row::tests::contains_image`:退化矩形也会发 Mesh,只判纹理
    /// 杀不掉「边长改 0」这条变异。
    fn image_bounds(shapes: &[egui::epaint::ClippedShape]) -> Option<egui::Rect> {
        fn walk(s: &egui::Shape) -> Option<egui::Rect> {
            match s {
                egui::Shape::Vec(v) => v.iter().find_map(walk),
                egui::Shape::Mesh(m) => {
                    let b = m.calc_bounds();
                    (m.texture_id != egui::TextureId::default()
                        && b.width() > 0.0
                        && b.height() > 0.0)
                        .then_some(b)
                }
                _ => None,
            }
        }
        shapes.iter().find_map(|cs| walk(&cs.shape))
    }

    /// 正文恰好等于 `needle` 的那段文字的左边界 x。`paint_highlighted` 一整段
    /// 名字排成一个 galley,所以按全等找得到。
    fn text_x_of(shapes: &[egui::epaint::ClippedShape], needle: &str) -> Option<f32> {
        fn walk(s: &egui::Shape, needle: &str) -> Option<f32> {
            match s {
                egui::Shape::Vec(v) => v.iter().find_map(|s| walk(s, needle)),
                egui::Shape::Text(ts) if ts.galley.text() == needle => Some(ts.pos.x),
                _ => None,
            }
        }
        shapes.iter().find_map(|cs| walk(&cs.shape, needle))
    }

    /// F293:会话行真的画出了它的图标;没图标的行不凭空画。
    ///
    /// 自证会变红:把 `sessions_column` 里 `paint_icon` 那段删掉。
    #[test]
    fn a_session_row_paints_the_icon_of_its_session() {
        assert!(
            image_bounds(&session_shapes(&[with_icon(sess(7, "web01"))])).is_some(),
            "会话有图标,启动页会话列却一张图都没画"
        );
        assert!(
            image_bounds(&session_shapes(&[sess(7, "web01")])).is_none(),
            "会话没图标却凭空画了一张图"
        );
    }

    /// F293:有 / 无图标两种行的**名字左边界一样**,而且名字不压在图标上。
    ///
    /// 两条断言缺一不可:只比「一样」的话,把文字左沿改回 `SP_S`(两种行
    /// 都压在图标上)照样绿 —— 「文字不压图标」引入了图标自己的包围盒这个
    /// 第三方参照物(本仓记过的「判据与被测量同源平移 = 恒绿」)。
    ///
    /// 自证会变红:把 `sessions_column` 里 `left` 改回 `rect.left() + SP_S`
    /// (第二条红);改成 `if icon.is_some() { TEXT_X } else { SP_S }`(第一条红)。
    #[test]
    fn the_session_name_starts_at_the_same_x_with_or_without_an_icon() {
        let with = session_shapes(&[with_icon(sess(7, "web01"))]);
        let without = session_shapes(&[sess(7, "web01")]);
        let x_with = text_x_of(&with, "web01").expect("有图标的行没画名字");
        let x_without = text_x_of(&without, "web01").expect("没图标的行没画名字");
        assert!(
            (x_with - x_without).abs() < 0.5,
            "有图标 / 没图标两种行的名字左边界不一样:{x_with} vs {x_without}"
        );
        let icon = image_bounds(&with).expect("有图标的行没画图");
        assert!(
            x_with >= icon.right(),
            "名字({x_with})压在图标({:?})上",
            icon
        );
    }
}
