//! F225②:项目管理器弹窗(新建 / 编辑 / 删除项目)。
//!
//! 与 `group_manager` 同构:UI 只写「意图」到 `UiState`,由 `app.rs` 在借用
//! 释放后施加并落盘。校验一律委托 `mullion_store::validate_project` ——
//! 这里不重写一份判据,重写就必然与落盘那一侧漂移。
//!
//! 设计见 `docs/superpowers/specs/2026-09-08-f221-f225-project-unit-design.md`。

use mullion_store::{ProjectId, ProjectRecord, Protocol, SessionId, SessionRecord};

/// 一次项目操作的意图。与 `GroupIntent` 同姿态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectIntent {
    /// 新建:只给名字,其余字段进弹窗再填。
    Add(String),
    /// 整份保存。`ProjectRecord.id` 由 `app` 侧以第一个参数为准。
    Save(ProjectId, Box<ProjectRecord>),
    Delete(ProjectId),
    /// F257:归档 / 取消归档。`bool` = 归档后的目标状态。
    ///
    /// **单独一条 intent,不走 `Save`**:`Save` 会跑 `validate_project`,而归档
    /// 一个"名字跟别人撞了、右栏保存正灰着"的项目必须成立 —— 收起一个项目
    /// 不该被别处的校验问题挡住(同 `touch_project_accessed` 的姿态)。
    SetArchived(ProjectId, bool),
}

/// 列表顺序:**按最后访问时间倒序**,从没打开过的排在最后。
///
/// 用户开机后想干的第一件事是回到昨天那个活,不是从一堆项目里找 ——
/// 所以「最近在干的」必须在最上面。F225① 的 launcher 列表用同一个函数,
/// 两处顺序不许各写一份(写两份就会漂,而「顺序不一样」用户第一眼就看得出来)。
///
/// 时间是 RFC3339 字符串,同一时区下**字典序即时间序**;跨时区写入的记录
/// (配置目录被搬到另一台机器,F48)可能排错位置 —— 后果有上限(列表顺序
/// 不对,不影响任何数据),不值得为它引一个日期解析库。
///
/// 同一时间(或都没访问过)时按 `id` 升序兜底 —— **不能靠 `sort_by_key` 的
/// 稳定性**:那样顺序就取决于入参顺序,而入参顺序来自磁盘上 `[[project]]`
/// 的书写次序,用户手改一次配置文件列表就重排了。
pub fn by_recent_access(projects: &[ProjectRecord]) -> Vec<&ProjectRecord> {
    let mut out: Vec<&ProjectRecord> = projects.iter().collect();
    out.sort_by(|a, b| {
        match (&a.last_accessed_at, &b.last_accessed_at) {
            (Some(x), Some(y)) => y.cmp(x),
            // 访问过的一律排在没访问过的前面,与两者具体是什么值无关。
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
        .then(a.id.0.cmp(&b.id.0))
    });
    out
}

/// 能当项目节点的会话:**只有 SSH**。
///
/// SFTP 节点没有 PTY,attach 过去是一块永远不出字的黑屏。`validate_project`
/// 那一道是落盘前的闸;这一道是**根本不让它出现在勾选列表里** ——
/// 让用户勾一个注定被拒的东西,是把校验当交互用。
pub fn selectable_nodes(sessions: &[SessionRecord]) -> Vec<&SessionRecord> {
    sessions
        .iter()
        .filter(|s| s.connection.protocol == Protocol::Ssh)
        .collect()
}

/// 一条会话在 `known_hosts` 里的键。与 SSH 侧**同一个拼法** ——
/// 拼法漂移的后果见 `KnownHostsFile::get` 的文档:同一台主机占两条记录。
pub fn host_key_of(s: &SessionRecord) -> String {
    mullion_ssh::known_hosts::host_key_id(&s.connection.host, s.connection.port)
}

/// 候选节点与项目里**已选中的其余节点**是否同机(F222)。
///
/// `table` 为 `None`(指纹表拿不到)时一律返回待核 —— 不是「同机」:
/// 拿不到证据时报「已核实」是伪阳性的安全结论。
pub fn node_verdict(
    chosen: &[SessionId],
    candidate: &SessionRecord,
    sessions: &[SessionRecord],
    table: Option<&mullion_store::known_hosts::KnownHostsFile>,
) -> mullion_store::SameMachine {
    let Some(table) = table else {
        return mullion_store::SameMachine::Pending;
    };
    let existing: Vec<String> = chosen
        .iter()
        .filter(|id| **id != candidate.id)
        .filter_map(|id| sessions.iter().find(|s| s.id == *id))
        .map(host_key_of)
        .collect();
    mullion_store::can_join(&existing, &host_key_of(candidate), table)
}

/// F239:三个窗口各自的标题。`app.rs::dismiss_areas` 与对应的 `Window::new`
/// 必须用同一个常量算 egui area id,否则标题漂移后「点外面关」会静默失效。
pub(crate) const WINDOW_TITLE: &str = "项目管理";
pub(crate) const TAKEOVER_WINDOW_TITLE: &str = "项目已在别处打开";
pub(crate) const OPEN_CONFIRM_WINDOW_TITLE: &str = "打开项目前先确认";

/// 项目管理弹窗。只写意图,不碰 store。
///
/// `table` 是 `known_hosts` 指纹表(F222)。`None` = 拿不到 —— 此时所有节点
/// 一律标「待核」,**不会**因为拿不到证据就放行成「已核实同机」。
#[allow(clippy::too_many_arguments)]
pub fn show(
    ctx: &egui::Context,
    t: &crate::theme::Theme,
    ui_state: &mut crate::ui::UiState,
    projects: &[ProjectRecord],
    lamps: &std::collections::BTreeMap<mullion_store::ProjectId, crate::project::Lamp>,
    sessions: &[SessionRecord],
    appearance: &crate::ui::badge::AppearanceCache,
    table: Option<&mullion_store::known_hosts::KnownHostsFile>,
) {
    let mut open = ui_state.project_manager_open;
    // 选中项被别的实例删掉(F189 重读)时草稿要跟着退回,否则右栏在编辑一个
    // 已经不存在的项目,保存时静默落空。
    if let Some(id) = ui_state.project_selected {
        if !projects.iter().any(|p| p.id == id) {
            ui_state.project_selected = None;
            ui_state.project_draft = None;
        }
    }
    // 「现在几点」一帧取一次,不是每行取一次 —— 每行各调一次 `now_utc()`
    // 等于每帧几十次系统调用,而这个项目为了空闲期的 CPU 花了整整八个切片。
    let now = time::OffsetDateTime::now_utc();
    // F236:焦点标志**在这里就消费掉**,按值传给右栏。
    //
    // 留着不清的话每帧都抢一次焦点,用户点右栏任何别的输入框都会被当场弹回
    // 名称框 —— 而这个状态没有自愈路径,只能关窗重开。在 `show` 顶层 take 也
    // 让「它被消费了」这件事一眼可见,不用翻到右栏深处去确认。
    //
    // `show` 只在 `project_manager_open` 为真时被调用(见 `ui::show_shell`),
    // 所以这里不会在弹窗关着的时候把标志白白吃掉。
    let focus_name = std::mem::take(&mut ui_state.project_focus_name);
    // F238:「导入 .ico…」被点了没有。放在窗口闭包外面接,原因同 F61 那条 ——
    // 系统文件对话框是阻塞调用,只能在 egui 闭包之外另起线程开,这里先记一笔。
    let mut pick_icon_clicked = false;
    // 宽度从 720 提到 840:左栏从 192 加宽到 `LIST_W`(300),不提的话右栏会
    // 从 442 缩到 334,F237 那个三行「说明」框跟着变窄。
    egui::Window::new(WINDOW_TITLE)
        .open(&mut open)
        .default_width(840.0)
        .show(ctx, |ui| {
            // 内容高度封顶到屏幕(F237)。**光把按钮行钉在窗口底部不够**:
            // `Window` 默认按内容自动撑高,内容 640px 时窗口就是 640px 高,
            // 「底部」跟着跑到屏幕下沿之外 —— 按钮照样够不着,而且滚动区因为
            // 拿不到高度约束根本不会滚。
            //
            // 上界从**这一行离屏幕底还剩多少**实测,不走 `Window::max_height`:
            // 那个限的是内容区,得再自己减一个「标题栏 + 边框 + 内外边距」的
            // 常量(实测 60),而那是猜出来的 reserve —— F217 栽过一次。
            // 这里的 `ui.cursor()` 已经在窗口 chrome 里面,减法自然成立。
            let room = ctx.screen_rect().bottom() - ui.cursor().top() - crate::ui::metrics::SP_M;
            ui.set_max_height(room.max(160.0));
            ui.horizontal_top(|ui| {
                list_column(ui, t, ui_state, projects, lamps, sessions, now, appearance);
                ui.separator();
                ui.vertical(|ui| {
                    form_column(
                        ui,
                        t,
                        ui_state,
                        projects,
                        sessions,
                        table,
                        focus_name,
                        &mut pick_icon_clicked,
                    );
                });
            });
        });
    ui_state.project_manager_open = open;
    if !open {
        // 关窗即丢草稿:留着的话下次打开会拿一份可能已经过期的内容盖上去。
        ui_state.project_draft = None;
        ui_state.project_selected = None;
    }
    // 「导入 .ico…」被点了 → 转成 `pick_icon_request`,由 `app.rs` 事后
    // 另起线程开系统文件框(不能在 egui 闭包里同步阻塞)。同会话编辑器。
    if std::mem::take(&mut pick_icon_clicked) {
        ui_state.icon_target = crate::ui::IconTarget::Project;
        ui_state.pick_icon_request = true;
    }
}

/// 左栏:搜索框 / 列表 / 底部「+ 添加项目」三段式。
///
/// 与会话管理器左栏同构(那边是搜索框 / 分组树 / 底部「+ 新建」)。底部按钮走
/// `TopBottomPanel::bottom(..).show_inside(ui)` **先占位**:egui 的面板布局保证
/// 面板先分配自己的高度、再把外层 `ui` 的可用区底边收缩到面板上沿 —— 直接按
/// 顺序画的话,项目一多列表就会把按钮顶出可视区,而那是唯一的新建入口。
///
/// 宽度从原来的 `FIELD_W_S * 2`(192)提到 `LIST_W`(300):行里现在有副标题和
/// 右对齐的时间列,192 装不下,长项目名会被截成一两个字。
#[allow(clippy::too_many_arguments)]
fn list_column(
    ui: &mut egui::Ui,
    t: &crate::theme::Theme,
    ui_state: &mut crate::ui::UiState,
    projects: &[ProjectRecord],
    lamps: &std::collections::BTreeMap<ProjectId, crate::project::Lamp>,
    sessions: &[SessionRecord],
    now: time::OffsetDateTime,
    appearance: &crate::ui::badge::AppearanceCache,
) {
    use crate::ui::metrics::{field_w, FIELD_W_L, SP_S};
    ui.vertical(|ui| {
        ui.set_width(crate::ui::session_manager::LIST_W);
        // 搜索框独占一整行:原来它旁边挂着「添加」按钮,只剩 96px,一个路径
        // 片段都打不下。
        let w = field_w(ui.available_width(), FIELD_W_L, 0.0);
        let search = ui.add(
            egui::TextEdit::singleline(&mut ui_state.project_search)
                .hint_text(crate::theme::hint_text(t, "搜索项目名 / 目录 / 节点"))
                .desired_width(w),
        );
        crate::ui::annotate::mark(ui.ctx(), "项目管理器/左栏/搜索框", search.rect);
        ui.add_space(SP_S);

        egui::TopBottomPanel::bottom("project_list_bottom")
            .frame(egui::Frame::none())
            .show_inside(ui, |ui| {
                ui.add_space(SP_S);
                let b = add_button(ui);
                crate::ui::annotate::mark(ui.ctx(), "项目管理器/左栏/添加项目", b.rect);
                if b.clicked() {
                    ui_state.project_intent = Some(ProjectIntent::Add(
                        crate::project::fresh_project_name(projects),
                    ));
                }
            });

        // F257:tab 栏。**搜索态整条收起**(设计 D4):搜索穿透两态,留着 tab
        // 的话两个 tab 显示完全相同的结果,切过去画面不变 —— 那是那种
        // 「编译过、跑起来看着像坏了」的 UI。
        //
        // 收起会让下面的列表上移约一行,布局跳一下 —— 接受:它由用户自己
        // 打字触发,是有因果的位移。灰着一排点不动的 tab 反而更像 bug
        // (同 `rehost.rs` 里那条「灰着一条永远点不动的项」的既有判断)。
        if ui_state.project_search.trim().is_empty() {
            ui.horizontal(|ui| {
                for tab in [
                    crate::ui::project_list::Tab::Active,
                    crate::ui::project_list::Tab::Archived,
                ] {
                    let on = ui_state.project_tab == tab;
                    if ui.selectable_label(on, tab.label()).clicked() {
                        ui_state.project_tab = tab;
                    }
                }
            });
            ui.add_space(SP_S);
        }

        // F257:列什么 / 怎么排 / 空了说什么,一律走 `project_list` ——
        // 三处列表共用同一份判据(设计 D8)。
        let tab = ui_state.project_tab;
        let rows = crate::ui::project_list::rows(projects, tab, &ui_state.project_search, sessions);
        if let Some(reason) =
            crate::ui::project_list::empty_reason(projects, tab, &ui_state.project_search, sessions)
        {
            ui.label(
                egui::RichText::new(crate::ui::project_list::empty_text(
                    reason,
                    crate::ui::project_list::Surface::Manager,
                ))
                .color(crate::theme::c32(t.fg_muted)),
            );
            if reason == crate::ui::project_list::EmptyReason::NoMatch {
                ui.add_space(SP_S);
                if ui.button("清空搜索").clicked() {
                    ui_state.project_search.clear();
                }
            }
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt("project_list")
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
                            query: &ui_state.project_search,
                            selected: ui_state.project_selected == Some(p.id),
                            now,
                            list: "manager",
                            icon: crate::project::icon_for(p, appearance),
                            icon_bg: crate::project::icon_bg(
                                p,
                                appearance,
                                mullion_store::ColorTarget::ListItem,
                            ),
                        },
                    );
                    if r.clicked() {
                        ui_state.project_selected = Some(p.id);
                        ui_state.project_draft = Some(p.clone());
                    }
                }
            });
    });
}

/// 「说明」多行框的 id。
///
/// 同 [`add_button_id`] 的理由:测试要量它有多高,自动 id 定位不到,只能靠
/// 「猜它画在哪个坐标」—— 而 galley 的高度量的是**文字**,单行内容的多行框
/// 跟单行框一样高,量文字根本分不出来。
pub(crate) fn note_field_id() -> egui::Id {
    egui::Id::new("mullion_pm_note_field")
}

/// 「+ 添加项目」按钮的 id。
///
/// 挂显式 id 而不是用 `ui.button()` 的自动 id:自动 id 由控件在 `Ui` 里的出现
/// 次序算出来,测试要点它就只能靠「猜它画在哪个坐标」—— 布局一动判据就假红/
/// 假绿。同 `session_manager::list::new_button_id()` 的理由。
pub(crate) fn add_button_id() -> egui::Id {
    egui::Id::new("mullion_pm_add_button")
}

/// 手绘「+ 添加项目」按钮,挂 [`add_button_id`],撞满整条左栏宽。
///
/// **视觉重点靠位置和尺寸,不靠颜色**:全场唯一一个 accent 实心按钮是会话
/// 编辑器的「保存并连接」,再加一颗会把那个层级搅浑。撞满整宽 + 独占底栏
/// 已经足够显眼。
///
/// 视觉规则取 `ui.style().interact(&resp)` —— 与 `egui::Button::ui()` 内部算
/// `frame_fill`/`frame_stroke` 用的是同一套(见 egui-0.30.0 `widgets/button.rs`),
/// 所以外观跟默认按钮基本一致。
///
/// `allocate_space` 只预留布局空间、不注册交互(不像 `allocate_exact_size` 会
/// 顺带用自动 id 注册一次 `Sense::hover`),避免同一块矩形被注册成两个互相
/// 打架的部件。
fn add_button(ui: &mut egui::Ui) -> egui::Response {
    let galley = egui::WidgetText::from("+ 添加项目").into_galley(
        ui,
        None,
        ui.available_width(),
        egui::TextStyle::Button,
    );
    let size = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
    let (_auto_id, rect) = ui.allocate_space(size);
    let resp = ui.interact(rect, add_button_id(), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&resp);
        ui.painter().rect(
            rect.expand(visuals.expansion),
            visuals.rounding,
            visuals.weak_bg_fill,
            visuals.bg_stroke,
        );
        let text_pos = egui::Align2::CENTER_CENTER
            .align_size_within_rect(galley.size(), rect)
            .min;
        ui.painter().galley(text_pos, galley, visuals.text_color());
    }
    resp
}

/// 右栏:选中项目的表单。
#[allow(clippy::too_many_arguments)]
fn form_column(
    ui: &mut egui::Ui,
    t: &crate::theme::Theme,
    ui_state: &mut crate::ui::UiState,
    projects: &[ProjectRecord],
    sessions: &[SessionRecord],
    table: Option<&mullion_store::known_hosts::KnownHostsFile>,
    focus_name: bool,
    pick_icon_clicked: &mut bool,
) {
    use crate::ui::metrics::{field_w, FIELD_W_L, FIELD_W_M, SP_M, SP_S, SP_XS};
    let Some(draft) = ui_state.project_draft.as_mut() else {
        ui.label(
            egui::RichText::new("从左边选一个项目来编辑。").color(crate::theme::c32(t.fg_muted)),
        );
        return;
    };
    let id = draft.id;
    // 校验与「能不能开」在**画之前**先算好:按钮行下面搬进了
    // `TopBottomPanel::bottom`,那一段的闭包排在滚动区之前,拿不到滚动区里
    // 那份 `draft` 的可变借用 —— 所以先 clone/copy 成局部量。
    let issue = mullion_store::validate_project(draft, projects, sessions).err();
    let blank = draft.name.trim().is_empty() || draft.dir.trim().is_empty();
    // F223:「打开」拿的是**库里那份**,不是右栏这份草稿 —— 草稿改了没保存
    // 就打开的话,连过去的是草稿里的目录/tmux 名,而配置库里根本没这回事,
    // 下次再打开又变回去。所以草稿脏了就先不让开。
    let stored = projects.iter().find(|p| p.id == id);
    let dirty = stored != Some(&*draft);
    let openable = stored.is_some_and(|p| !p.nodes.is_empty()) && !dirty;
    let save_draft = draft.clone();
    // F257:按钮上的字取**库里那份**的归档态,不是草稿 —— 草稿里的
    // `archived_at` 永远不会被右栏任何一个控件改到(设计:只由这个按钮写),
    // 但草稿是「保存前的编辑缓冲」,拿它当真值会在保存失败时给出过期的字。
    let is_archived = stored.is_some_and(|p| p.archived_at.is_some());

    // 按钮行钉在底部,**不跟着内容滚**(F237)。右栏加了三行说明之后内容约
    // 640px,在 1080p + 150% 缩放(逻辑高 720)下,按顺序画的话「保存 / 打开 /
    // 删除项目」会被顶出可视区 —— 而那是这个界面唯一的出口。
    egui::TopBottomPanel::bottom("project_form_bottom")
        .frame(egui::Frame::none())
        .show_inside(ui, |ui| {
            ui.add_space(SP_M);
            if let Some(ref e) = issue {
                ui.label(
                    egui::RichText::new(issue_text(e, projects, sessions))
                        .color(crate::theme::c32(t.danger_text)),
                );
                ui.add_space(SP_XS);
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(issue.is_none() && !blank, egui::Button::new("保存"))
                    .clicked()
                {
                    ui_state.project_intent =
                        Some(ProjectIntent::Save(id, Box::new(save_draft.clone())));
                }
                ui.add_space(SP_S);
                if ui
                    .add_enabled(openable, egui::Button::new("打开"))
                    .clicked()
                {
                    ui_state.project_open_request = Some((id, None));
                }
                ui.add_space(SP_S);
                // F257:归档摆在「删除项目」**左边** —— 危险程度从右往左递减,
                // 同 `pane_title` 三个按钮的既有规矩。**不上确认框**:归档可逆、
                // 撤销就在同一个按钮上,而「高频路径上的无谓确认会被用户练成
                // 闭眼点确定,那时它对真正危险的几种也失效」(`show_open_confirm`
                // 的原话)。
                if ui.button(archive_button_label(is_archived)).clicked() {
                    ui_state.project_intent = Some(ProjectIntent::SetArchived(id, !is_archived));
                }
                ui.add_space(SP_S);
                if ui.button("删除项目").clicked() {
                    ui_state.project_intent = Some(ProjectIntent::Delete(id));
                }
            });
            if dirty {
                ui.add_space(SP_XS);
                ui.label(
                    egui::RichText::new("有未保存的改动,先保存再打开。")
                        .color(crate::theme::c32(t.fg_muted)),
                );
            }
        });

    let mut first = true;
    egui::ScrollArea::vertical()
        .id_salt("project_form")
        .show(ui, |ui| {
            crate::ui::session_manager::form::section(ui, t, "项目管理器", "基本", &mut first);
            crate::ui::session_manager::form::grid(ui, "project_basic", |ui| {
                ui.label("名称");
                let w = field_w(ui.available_width(), FIELD_W_M, 0.0);
                let name_resp =
                    ui.add(egui::TextEdit::singleline(&mut draft.name).desired_width(w));
                // F236:刚点完「+ 添加项目」—— 把焦点送进来并全选。
                //
                // 全选的理由:默认名「新项目」是占位,用户第一个动作必然是把它删掉重打。
                // `TextEditState` 的游标区间是 egui 里唯一能表达「全选」的地方
                // (`TextEdit` 自己没有 select-all 的构造项)。
                if focus_name {
                    name_resp.request_focus();
                    if let Some(mut st) =
                        egui::widgets::text_edit::TextEditState::load(ui.ctx(), name_resp.id)
                    {
                        let n = draft.name.chars().count();
                        st.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                            egui::text::CCursor::new(0),
                            egui::text::CCursor::new(n),
                        )));
                        st.store(ui.ctx(), name_resp.id);
                    }
                }
                ui.end_row();
                // 说明改多行(F237)。标签**顶对齐**:`Grid` 每行默认 `Align::Center`,
                // 3 行高的 multiline 旁边的短标签会被垂直居中,跟上面几行的标签对不齐。
                ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                    ui.label("说明");
                });
                let w = field_w(ui.available_width(), FIELD_W_L, 0.0);
                ui.add(
                    egui::TextEdit::multiline(&mut draft.note)
                        .id(note_field_id())
                        .desired_rows(3)
                        .desired_width(w),
                );
                ui.end_row();
                ui.label("目录");
                let w = field_w(ui.available_width(), FIELD_W_L, 0.0);
                ui.add(
                    egui::TextEdit::singleline(&mut draft.dir)
                        .hint_text("/srv/app")
                        .desired_width(w),
                );
                ui.end_row();
                ui.label("tmux 名");
                let mut spelled = draft.tmux_name.clone().unwrap_or_default();
                let w = field_w(ui.available_width(), FIELD_W_M, 0.0);
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut spelled)
                            .hint_text("留空 = 用项目名")
                            .desired_width(w),
                    )
                    .changed()
                {
                    draft.tmux_name =
                        (!spelled.trim().is_empty()).then(|| spelled.trim().to_string());
                }
                ui.end_row();
            });
            ui.add_space(SP_XS);
            ui.label(
                egui::RichText::new(format!(
                    "会 attach 的 tmux 会话:{}",
                    mullion_store::project_tmux_name(draft)
                ))
                .color(crate::theme::c32(t.fg_muted)),
            );
            // 设计里明写要让用户看见的那条:`-c` 只对新建会话生效。
            ui.label(
                egui::RichText::new(
                    "改目录只影响新建的 tmux 会话;已经存在的那个不会移动,需先在远端结束它。",
                )
                .color(crate::theme::c32(t.fg_muted)),
            );

            appearance_section(
                ui,
                t,
                draft,
                &mut ui_state.icon_error,
                ui_state.icon_target,
                pick_icon_clicked,
                &mut first,
            );

            crate::ui::session_manager::form::section(ui, t, "项目管理器", "节点", &mut first);
            ui.label(
                egui::RichText::new(
                    "同一台机器的等价路线(不同凭据 / 端口 / 跳板)。只列 SSH 会话。",
                )
                .color(crate::theme::c32(t.fg_muted)),
            );
            ui.add_space(SP_XS);
            let candidates = selectable_nodes(sessions);
            if candidates.is_empty() {
                ui.label(
                    egui::RichText::new("还没有 SSH 会话可选。先在会话管理器里建一条。")
                        .color(crate::theme::c32(t.fg_muted)),
                );
            }
            egui::ScrollArea::vertical()
                .id_salt("project_nodes")
                .max_height(180.0)
                .show(ui, |ui| {
                    for s in &candidates {
                        ui.horizontal(|ui| {
                            let mut on = draft.nodes.contains(&s.id);
                            if ui.checkbox(&mut on, &s.identity.name).changed() {
                                if on {
                                    draft.nodes.push(s.id);
                                } else {
                                    draft.nodes.retain(|n| *n != s.id);
                                    if draft.preferred == Some(s.id) {
                                        draft.preferred = None;
                                    }
                                }
                            }
                            if on {
                                let verdict = node_verdict(&draft.nodes, s, sessions, table);
                                let (text, color) = match verdict {
                                    mullion_store::SameMachine::Same => {
                                        ("已核实同机", crate::theme::c32(t.fg_muted))
                                    }
                                    mullion_store::SameMachine::Pending => {
                                        ("待核", crate::theme::c32(t.fg_muted))
                                    }
                                    mullion_store::SameMachine::Different { .. } => {
                                        ("指纹与其他节点不一致", crate::theme::c32(t.danger_text))
                                    }
                                };
                                ui.label(egui::RichText::new(text).size(11.0).color(color));
                                let mut pref = draft.preferred == Some(s.id);
                                if ui.radio(pref, "首选").clicked() {
                                    pref = true;
                                }
                                if pref {
                                    draft.preferred = Some(s.id);
                                }
                            }
                        });
                    }
                });

            crate::ui::session_manager::form::section(ui, t, "项目管理器", "记录", &mut first);
            // 时间走本地时区,不把 RFC3339 的 UTC 原文糊在界面上:左栏那边说
            // 「3 小时前」、这里说「…T05:00:00Z」的话,同一个字段在同一个弹窗里
            // 分裂成两种读法。偏移取 `localtime::offset()`(进程启动时取的那一次,
            // Windows 上来自 `GetTimeZoneInformation`,就是系统设置里的时区)。
            let now = time::OffsetDateTime::now_utc();
            let off = crate::localtime::offset();
            ui.label(
                egui::RichText::new(format!(
                    "创建于 {} · 最后打开 {}",
                    crate::localtime::relative(&draft.created_at, now, off),
                    draft.last_accessed_at.as_deref().map_or_else(
                        || "从未".to_string(),
                        |s| crate::localtime::relative(s, now, off)
                    )
                ))
                .color(crate::theme::c32(t.fg_muted)),
            );
        });
}

/// F238:把选中的那份 `.ico` 正文装进项目草稿。返回错误文案(`None` = 成功)。
///
/// **只写草稿不落盘** —— 落盘是「保存」那颗按钮的事,在这里写等于绕开了
/// `validate_project`。
///
/// 抽成自由函数是为了能单测:`app.rs` 里那条 `UserEvent` 分支在无窗口下
/// 造不出来,内联的话这段「解码失败要不要动 icon」的判断没人看着。
pub(crate) fn apply_picked_icon(
    draft: &mut ProjectRecord,
    path: &std::path::Path,
    bytes: std::io::Result<Vec<u8>>,
) -> Option<String> {
    match bytes {
        Err(e) => Some(format!("读不了 {}:{e}", path.display())),
        Ok(bytes) => match crate::ui::ico::import(&bytes) {
            Ok(value) => {
                draft.icon = Some(mullion_store::IconSpec {
                    kind: mullion_store::IconKind::Ico,
                    value,
                    bg: None,
                });
                None
            }
            // 解码失败绝不能碰 `draft.icon`:导入一张坏图不该把用户原来那张
            // 好图抹掉。
            Err(e) => Some(e.message()),
        },
    }
}

/// F238:项目图标。与会话编辑器的「外观」分节(`session_manager::fields::appearance`)
/// 同构 —— 两处长得不一样的话,用户会以为项目的图标是另一种东西。
///
/// 参数拆成几个 `&mut` 是因为 `ui_state` 整体传进去会跟 `draft`(借自
/// `ui_state.project_draft.as_mut()`)撞借用检查。
fn appearance_section(
    ui: &mut egui::Ui,
    t: &crate::theme::Theme,
    draft: &mut ProjectRecord,
    icon_error: &mut Option<String>,
    icon_target: crate::ui::IconTarget,
    pick_clicked: &mut bool,
    first: &mut bool,
) {
    crate::ui::session_manager::form::section(ui, t, "项目管理器", "外观", first);
    crate::ui::session_manager::form::grid(ui, "project_appearance", |ui| {
        ui.label("图标");
        ui.vertical(|ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button("导入 .ico…").clicked() {
                    *pick_clicked = true;
                }
                if draft.icon.is_some() && ui.button("清除").clicked() {
                    draft.icon = None;
                    *icon_error = None;
                }
            });
            // F238:错误文案跟着「这次导入是替谁选的」走。`icon_error` 是两处
            // 外观分节共用的一个字段,不按归属门控的话,在会话那边导入失败、
            // 转头打开项目的外观分节,会看到一条与当前项目毫不相干的旧错误 ——
            // 而它长得跟真错误一模一样。
            if icon_target == crate::ui::IconTarget::Project {
                if let Some(e) = icon_error.as_deref() {
                    ui.colored_label(crate::theme::c32(t.danger_text), e);
                }
            }
            if let Some(icon) = draft.icon.as_ref() {
                let side = crate::ui::ico::SMALL as f32;
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
                crate::ui::badge::paint_icon(ui.painter(), rect, icon, None);
            } else {
                // 没设时说清回落到哪 —— 一片空白会被读成「这里坏了」。
                ui.colored_label(crate::theme::c32(t.fg_muted), "没设:跟首选节点的图标走");
            }
        });
        ui.end_row();
    });
}

/// F223:开之前要问的那一下。
///
/// **节点在问之前就选定了**(`plan_open` 返回时带出来的)—— 确认框开着的那段
/// 时间里配置完全可能被改(F189 别的实例、或用户自己在管理器里改),问完再
/// 算一遍就会拨到别处,而用户以为自己确认的是刚才那一下。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenAsk {
    pub project: ProjectId,
    pub node: SessionId,
    pub pane: Option<mullion_core::layout::PaneId>,
    /// 逐条理由,来自 `crate::project::confirm_reasons`。
    pub reasons: Vec<&'static str>,
}

/// F224:「项目已在别处打开」确认框要显示的那点数据。
///
/// 计划和 sink 不在这里 —— 它们在 `App::project_takeover` 上。UI 层拿不到
/// 也不该拿:一份 `PendingAutomation` 漏进 `UiState` 就等于把「发什么字节」
/// 的决定权分了一半给渲染层。
#[derive(Clone)]
pub struct TakeoverAsk {
    pub project: String,
    /// 远端此刻挂着几个客户端(`tmux list-clients` 数出来的)。
    pub clients: usize,
}

/// F224:踢人确认框。返回 `Some(true)` = 用户认了「踢下线」。
///
/// **取消排在前面**:这是设计定的默认动作(「默认按钮是取消」)。egui 没有
/// 「默认按钮」的概念,唯一能表达的就是位置与配色 —— 危险的那颗在右、用
/// `danger_text` 上色并把后果写进按钮文字里,而不是叫「确定」。
pub fn show_takeover_confirm(
    ctx: &egui::Context,
    t: &crate::theme::Theme,
    ask: &TakeoverAsk,
) -> Option<bool> {
    use crate::ui::metrics::{SP_M, SP_S};
    let mut out = None;
    egui::Window::new(TAKEOVER_WINDOW_TITLE)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            crate::ui::annotate::mark(ui.ctx(), "项目踢人确认框".to_string(), ui.max_rect());
            ui.label(format!(
                "「{}」的 tmux 会话此刻挂着 {} 个客户端。",
                ask.project, ask.clients
            ));
            ui.add_space(SP_S);
            ui.colored_label(
                crate::theme::c32(t.danger_text),
                "继续会把对方全部踢下线(他们的画面当场断开,远端进程不受影响)。",
            );
            ui.add_space(SP_M);
            ui.horizontal(|ui| {
                if ui.button("取消").clicked() {
                    out = Some(false);
                }
                ui.add_space(SP_S);
                if ui
                    .button(
                        egui::RichText::new("踢下线并打开").color(crate::theme::c32(t.danger_text)),
                    )
                    .clicked()
                {
                    out = Some(true);
                }
            });
        });
    out
}

/// F257:归档按钮上的字。抽成函数只为了能单测 —— 写死「归档」的话,已归档的
/// 项目右栏会给出一个点了看不出变化的按钮,而这是撤销归档的唯一入口。
pub(crate) fn archive_button_label(archived: bool) -> &'static str {
    if archived {
        "取消归档"
    } else {
        "归档"
    }
}

/// F223:打开项目前的确认框。返回 `true` = 用户点了「继续」。
///
/// 只在**真有东西会丢**时才会被调用(判据在 `crate::project::plan_open`)。
/// 高频路径上的无谓确认会被用户练成闭眼点确定,那时它对真正危险的几种也失效。
pub fn show_open_confirm(
    ctx: &egui::Context,
    t: &crate::theme::Theme,
    ask: &OpenAsk,
    name: &str,
) -> Option<bool> {
    use crate::ui::metrics::{SP_M, SP_S};
    let mut out = None;
    egui::Window::new(OPEN_CONFIRM_WINDOW_TITLE)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            crate::ui::annotate::mark(ui.ctx(), "打开项目确认框".to_string(), ui.max_rect());
            ui.label(format!(
                "打开「{name}」会把当前 pane 从现在这条连接上摘下来。"
            ));
            ui.add_space(SP_S);
            for r in &ask.reasons {
                ui.colored_label(crate::theme::c32(t.danger_text), *r);
            }
            ui.add_space(SP_M);
            ui.horizontal(|ui| {
                if ui.button("继续打开").clicked() {
                    out = Some(true);
                }
                ui.add_space(SP_S);
                if ui.button("取消").clicked() {
                    out = Some(false);
                }
            });
        });
    out
}

/// 把校验失败翻成人话。**逐条指出撞在谁身上** —— 只说「名字重复」的话
/// 用户得自己翻一遍项目表去找。
fn issue_text(
    e: &mullion_store::ProjectIssue,
    projects: &[ProjectRecord],
    sessions: &[SessionRecord],
) -> String {
    use mullion_store::{ProjectIssue as I, TmuxNameOwner as O};
    let pname = |id| {
        projects
            .iter()
            .find(|p| p.id == id)
            .map_or_else(|| "(已删除)".to_string(), |p| p.name.clone())
    };
    let sname = |id| {
        sessions
            .iter()
            .find(|s| s.id == id)
            .map_or_else(|| "(已删除)".to_string(), |s| s.identity.name.clone())
    };
    match e {
        I::DuplicateName { with } => format!("项目名与「{}」重复。", pname(*with)),
        I::TmuxNameClash {
            name,
            with: O::Project(p),
        } => format!("tmux 名「{name}」与项目「{}」撞车。", pname(*p)),
        I::TmuxNameClash {
            name,
            with: O::Session(s),
        } => format!("tmux 名「{name}」与会话「{}」写死的名字撞车。", sname(*s)),
        I::PreferredNotInNodes => "首选节点不在已勾选的节点里。".to_string(),
        I::NonSshNode { node } => format!("「{}」不是 SSH 会话,不能当项目节点。", sname(*node)),
        I::TmuxNameEmpty => {
            "tmux 名去掉非法字符后是空的,请另起一个名字(否则打开项目不会有任何反应)。".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proj(id: u64, name: &str, accessed: Option<&str>) -> ProjectRecord {
        ProjectRecord {
            id: ProjectId(id),
            name: name.into(),
            note: String::new(),
            nodes: Vec::new(),
            preferred: None,
            dir: "/srv/app".into(),
            tmux_name: None,
            created_at: "2026-09-01T00:00:00Z".into(),
            last_accessed_at: accessed.map(str::to_string),
            archived_at: None,
            icon: None,
        }
    }

    fn sess(id: u64, name: &str, proto: Protocol) -> SessionRecord {
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
                protocol: proto,
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

    #[test]
    fn the_most_recently_opened_project_comes_first() {
        let ps = vec![
            proj(1, "旧", Some("2026-09-01T00:00:00Z")),
            proj(2, "新", Some("2026-09-07T00:00:00Z")),
        ];
        let got = by_recent_access(&ps);
        assert_eq!(got[0].name, "新", "倒序:最近访问的排最上面");
        assert_eq!(got[1].name, "旧");
    }

    /// 从没打开过的排最后 —— 不管它的 id 多小、也不管它在磁盘上写在哪一行。
    ///
    /// 自证会变红:把 `(Some(_), None)` 那一臂改成 `Greater`。
    #[test]
    fn a_project_never_opened_sinks_below_every_opened_one() {
        let ps = vec![
            proj(1, "没打开过", None),
            proj(2, "打开过", Some("2026-09-01T00:00:00Z")),
        ];
        let got = by_recent_access(&ps);
        assert_eq!(got[0].name, "打开过");
        assert_eq!(got[1].name, "没打开过");
    }

    /// 平局按 id 兜底,**不靠排序的稳定性**。
    ///
    /// 靠稳定性的话顺序就等于 `[[project]]` 在磁盘上的书写次序 ——
    /// 用户手改一次配置文件、或哪天换成 `sort_unstable_by`,列表就重排了,
    /// 而这两件事都不会有任何报错。
    ///
    /// 自证会变红:把 `.then(a.id.0.cmp(&b.id.0))` 去掉 —— 入参是逆序的,
    /// 稳定排序会原样保留 `[b, a]`。
    #[test]
    fn projects_that_tie_are_ordered_by_id_not_by_input_order() {
        let ps = vec![proj(9, "后写的", None), proj(2, "先写的", None)];
        let got = by_recent_access(&ps);
        assert_eq!(
            got.iter().map(|p| p.id.0).collect::<Vec<_>>(),
            vec![2, 9],
            "平局要按 id,不能沿用入参顺序"
        );
    }

    /// SFTP 会话根本不该出现在节点勾选列表里 —— 让用户勾一个注定被
    /// `validate_project` 拒掉的东西,是把校验当交互用。
    ///
    /// 自证会变红:把 `filter` 去掉。
    #[test]
    fn an_sftp_session_never_shows_up_as_a_candidate_node() {
        let ss = vec![
            sess(1, "ssh 的", Protocol::Ssh),
            sess(2, "sftp 的", Protocol::Sftp),
        ];
        let got = selectable_nodes(&ss);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].identity.name, "ssh 的");
    }

    /// 拿不到指纹表时是**待核**,不是同机 —— 报「已核实同机」是伪阳性的
    /// 安全结论,而界面上待核标记也不会出现,用户以为核过了。
    ///
    /// 自证会变红:把早退改成 `SameMachine::Same`。
    #[test]
    fn without_a_fingerprint_table_every_node_is_pending_not_same() {
        let ss = vec![sess(1, "a", Protocol::Ssh), sess(2, "b", Protocol::Ssh)];
        assert_eq!(
            node_verdict(&[SessionId(1)], &ss[1], &ss, None),
            mullion_store::SameMachine::Pending
        );
    }

    /// 候选**自己**已经在已选列表里(重新校验一条已勾的)时,不能拿它跟
    /// 自己比 —— 那永远是 `Same`,会把「这一条其实还没连过」的待核态
    /// 掩盖成「已核实」。
    ///
    /// 自证会变红:把 `filter(|id| **id != candidate.id)` 去掉。
    #[test]
    fn a_node_is_never_compared_against_itself() {
        let ss = vec![sess(1, "a", Protocol::Ssh)];
        let mut table = mullion_store::known_hosts::KnownHostsFile::default();
        table.record(
            &host_key_of(&ss[0]),
            mullion_store::known_hosts::HostKeyEntry {
                algo: "ssh-ed25519".into(),
                fingerprint: "SHA256:AAAA".into(),
            },
        );
        assert_eq!(
            node_verdict(&[SessionId(1)], &ss[0], &ss, Some(&table)),
            mullion_store::SameMachine::Pending,
            "只有它自己一条时没有可比对的对象,应是待核"
        );
    }

    // ---- 左栏三段式(F233/F236)-------------------------------------------

    /// 跑两帧,把弹窗画出来的全部文字收上来。
    ///
    /// **两帧**:第一帧 `egui::Window` 还在量自己的尺寸,内容矩形没定,行会被
    /// 裁掉。
    fn window_texts(projects: &[ProjectRecord], query: &str) -> Vec<String> {
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
            project_manager_open: true,
            project_search: query.to_string(),
            ..Default::default()
        };
        let lamps = std::collections::BTreeMap::new();
        let sessions: Vec<SessionRecord> = Vec::new();
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
                        None,
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

    /// 搜索词把不匹配的行滤掉。
    ///
    /// 自证会变红:把 `list_column` 里的
    /// `.filter(|p| crate::project::matches(..))` 那一行删掉。
    #[test]
    fn the_left_column_hides_projects_that_do_not_match_the_query() {
        let ps = vec![proj(1, "接口", None), proj(2, "数据库", None)];
        let texts = window_texts(&ps, "接口");
        assert!(
            texts.iter().any(|s| s == "接口"),
            "命中的行不见了:{texts:?}"
        );
        assert!(
            !texts.iter().any(|s| s == "数据库"),
            "没命中的行还在:{texts:?}"
        );
    }

    /// 搜不到任何东西时列表是一整片空白 —— 用户分不清「没有匹配」和「项目都
    /// 没了」。给一句话 + 一个回到全部列表的出口(走查 22)。
    ///
    /// 自证会变红:把那个 `rows.is_empty()` 分支删掉。
    #[test]
    fn a_query_that_matches_nothing_says_so_and_offers_a_way_back() {
        let ps = vec![proj(1, "接口", None)];
        let joined = window_texts(&ps, "根本没有这个").join(" ");
        assert!(joined.contains("没有匹配的项目"), "没给空态说明:{joined}");
        assert!(
            joined.contains("清空搜索"),
            "没给回到全部列表的出口:{joined}"
        );
    }

    /// 新建按钮写「+ 添加项目」,不是「添加」——「添加」什么?旁边原来那个
    /// 输入框已经改成搜索框了,不说清楚就读成「添加搜索结果」。
    ///
    /// 判据读的是**渲染出来的文字**,不是源码里的字面量 —— 后者换个拼法就恒绿。
    ///
    /// 自证会变红:把按钮文案改回「添加」。
    #[test]
    fn the_add_button_spells_out_that_it_makes_a_project() {
        let joined = window_texts(&[], "").join(" ");
        assert!(joined.contains("+ 添加项目"), "按钮文案不对:{joined}");
    }

    /// 跑三帧,真点一次「+ 添加项目」,返回它发出的意图。
    ///
    /// 靠显式 `add_button_id()` 定位按钮矩形,不靠「猜它画在哪个坐标」——
    /// 布局一动,坐标式判据就假红/假绿。
    fn intent_after_clicking_add(projects: &[ProjectRecord]) -> Option<ProjectIntent> {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut ui_state = crate::ui::UiState {
            project_manager_open: true,
            ..Default::default()
        };
        let lamps = std::collections::BTreeMap::new();
        let sessions: Vec<SessionRecord> = Vec::new();
        let draw = |input: egui::RawInput, ui_state: &mut crate::ui::UiState| {
            let _ = ctx.run(input, |ctx| {
                show(
                    ctx,
                    &t,
                    ui_state,
                    projects,
                    &lamps,
                    &sessions,
                    &crate::ui::badge::AppearanceCache::default(),
                    None,
                );
            });
        };
        for _ in 0..2 {
            draw(egui::RawInput::default(), &mut ui_state);
        }
        let rect = ctx
            .read_response(add_button_id())
            .expect("「+ 添加项目」没被登记 —— 它的 id 变了还是根本没画?")
            .rect;
        let pos = rect.center();
        let mut input = egui::RawInput {
            events: vec![egui::Event::PointerMoved(pos)],
            ..Default::default()
        };
        for pressed in [true, false] {
            input.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            });
        }
        draw(input, &mut ui_state);
        ui_state.project_intent
    }

    /// 点一下就建出项目,**不用先在哪个框里打字**。
    ///
    /// 名字由 `project::fresh_project_name` 现算 —— 原来左栏那个「新建项目」
    /// 输入框已经改成搜索框了,任何「从 UI 缓冲里读名字」的写法在这之后都会
    /// 建出名字为空的项目,而 `validate_project` 会拒掉它:右栏「保存」永远
    /// 灰着,而且没有任何解释。
    ///
    /// 自证会变红:把 `list_column` 里的
    /// `ProjectIntent::Add(crate::project::fresh_project_name(projects))`
    /// 换成 `ProjectIntent::Add(ui_state.project_search.clone())`。
    #[test]
    fn clicking_add_creates_a_project_straight_away_with_a_generated_name() {
        assert_eq!(
            intent_after_clicking_add(&[]),
            Some(ProjectIntent::Add("新项目".into()))
        );
    }

    /// 连点两次不会撞名 —— 第二次给的是下一个空号。
    ///
    /// 撞名的记录**存不进去**(`validate_project` 要求项目名全局唯一),而
    /// `Add` 是立刻落盘的:列表里会出现两行同名、右栏「保存」灰着。
    #[test]
    fn a_second_click_picks_the_next_free_number_instead_of_clashing() {
        let ps = vec![proj(1, "新项目", None)];
        assert_eq!(
            intent_after_clicking_add(&ps),
            Some(ProjectIntent::Add("新项目 2".into()))
        );
    }

    /// 新建完之后焦点要落到右栏「名称」框上 —— 否则用户得先用鼠标点进去才能
    /// 给这个活起名字,而「起名字」正是新建之后唯一要做的事。
    ///
    /// 自证会变红:把 `form_column` 里那段 `request_focus()` 删掉。
    #[test]
    fn right_after_adding_a_project_the_name_field_takes_focus() {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let p = proj(1, "新项目", None);
        let mut ui_state = crate::ui::UiState {
            project_manager_open: true,
            project_selected: Some(p.id),
            project_draft: Some(p.clone()),
            project_focus_name: true,
            ..Default::default()
        };
        let lamps = std::collections::BTreeMap::new();
        let ps = vec![p];
        let sessions: Vec<SessionRecord> = Vec::new();
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                show(
                    ctx,
                    &t,
                    &mut ui_state,
                    &ps,
                    &lamps,
                    &sessions,
                    &crate::ui::badge::AppearanceCache::default(),
                    None,
                );
            });
        }
        assert!(
            ctx.memory(|m| m.focused()).is_some(),
            "新建之后没有任何部件拿到焦点 —— 用户得先用鼠标点进名称框才能起名"
        );
        assert!(
            !ui_state.project_focus_name,
            "焦点标志没被消费 —— 会每帧抢一次焦点,用户点右栏别的框都会被弹回来,\
             而且这个状态没有自愈路径"
        );
    }

    /// 一个项目都没有时,「+ 添加项目」**仍然要在**。
    ///
    /// 空态那一支是 `return` —— 按钮如果排在它后面就永远画不出来,而那时正是
    /// 用户最需要它的时候(界面上只有一句「还没有项目」和一个点不了的搜索框)。
    ///
    /// 自证会变红:把 `TopBottomPanel::bottom(..)` 那一整段挪到
    /// `if projects.is_empty() { .. return; }` 之后。
    #[test]
    fn the_add_button_is_still_there_when_there_are_no_projects_at_all() {
        let joined = window_texts(&[], "").join(" ");
        assert!(joined.contains("还没有项目"), "空态说明不见了:{joined}");
        assert!(
            joined.contains("+ 添加项目"),
            "空手上门时反而没有新建入口:{joined}"
        );
    }

    // ---- F257:左栏 tab 栏 -------------------------------------------------

    /// 跑两帧,画左栏,返回 tab 栏是否被画出来(按 [`crate::ui::project_list::Tab::label`]
    /// 的文字找)。
    ///
    /// 不选中任何项目,与右栏那颗「归档」动作按钮(只在选中项目时才画)区分开。
    fn tab_bar_visible(query: &str) -> bool {
        let joined = window_texts(&[], query).join(" ");
        joined.contains(crate::ui::project_list::Tab::Active.label())
            && joined.contains(crate::ui::project_list::Tab::Archived.label())
    }

    /// D4:**搜索态没有 tab**。搜索框一非空,tab 栏整条收起 —— 留着的话两个
    /// tab 会显示完全相同的结果(搜索穿透两态),切过去画面不变,用户会以为
    /// 点坏了。
    ///
    /// 自证会变红:把 `if ui_state.project_search.trim().is_empty()` 那道闸删掉。
    #[test]
    fn the_tab_bar_disappears_while_searching_because_both_tabs_would_look_identical() {
        assert!(tab_bar_visible(""), "不搜索时 tab 栏要在");
        assert!(!tab_bar_visible("alpha"), "搜索时 tab 栏必须收起");
    }

    /// F257:真点一次「归档」tab,核对**当帧**画出的行换了没有。
    ///
    /// 只守 tab 栏「在不在」不够 ——「点了『归档』→ `ui_state.project_tab`
    /// 变成 `Archived` → 当帧 `rows(..)` 按新 tab 列行」这条接线此前没有测试
    /// 顶着。纯函数 `project_list::rows` 单测很扎实,但纯函数测得扎实、
    /// 接线没人看着是本仓库反复踩中的形状。
    ///
    /// 判据是**画出来的项目名**(`阿尔法`/`贝塔`,故意不含「归档」「在用」
    /// 字样,不会被 tab 按钮文字或将来行上可能加的「已归档」标记污染)——
    /// 不查 `ui_state.project_tab` 这个字段:只查字段的话,「字段变了但
    /// `rows` 读的是别的东西」这种接线错照样绿。
    ///
    /// 手法照抄 `intent_after_clicking_add`:定位控件矩形 / 注入指针事件
    /// 模拟真实点击,press+release 同一批事件喂进同一次 `ctx.run`,点击与
    /// 重绘落在同一帧。
    ///
    /// 自证会变红:
    /// - 变异 A:删掉 `list_column` 里 `ui_state.project_tab = tab;` 这行赋值
    ///   (点了不生效,归档 tab 点了画面纹丝不动)。
    /// - 变异 B:把 `let tab = ui_state.project_tab;` 挪到 tab 栏渲染那段
    ///   **之前**(当帧读到的是点击前的旧值,画面慢一帧才换)。
    #[test]
    fn clicking_the_archived_tab_swaps_the_rows_drawn_this_frame() {
        fn walk(shape: &egui::Shape, out: &mut Vec<(String, egui::Pos2)>) {
            match shape {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                egui::Shape::Text(ts) => {
                    out.push((
                        ts.galley.text().to_string(),
                        ts.pos + ts.galley.size() / 2.0,
                    ));
                }
                _ => {}
            }
        }
        fn spots_of(out: &egui::FullOutput) -> Vec<(String, egui::Pos2)> {
            let mut v = Vec::new();
            for cs in &out.shapes {
                walk(&cs.shape, &mut v);
            }
            v
        }

        let mut active = proj(1, "阿尔法", None);
        active.archived_at = None;
        let mut archived = proj(2, "贝塔", None);
        archived.archived_at = Some("2026-09-01T00:00:00Z".into());
        let ps = vec![active, archived];

        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut ui_state = crate::ui::UiState {
            project_manager_open: true,
            ..Default::default()
        };
        let lamps = std::collections::BTreeMap::new();
        let sessions: Vec<SessionRecord> = Vec::new();
        let draw = |input: egui::RawInput, ui_state: &mut crate::ui::UiState| {
            ctx.run(input, |ctx| {
                show(
                    ctx,
                    &t,
                    ui_state,
                    &ps,
                    &lamps,
                    &sessions,
                    &crate::ui::badge::AppearanceCache::default(),
                    None,
                );
            })
        };

        // 预热两帧:第一帧 `egui::Window` 还在量自己的尺寸,内容矩形没定。
        let _ = draw(egui::RawInput::default(), &mut ui_state);
        let out = draw(egui::RawInput::default(), &mut ui_state);
        let spots = spots_of(&out);
        assert!(
            spots.iter().any(|(s, _)| s == "阿尔法"),
            "初始(在用 tab)没画出在用项目:{spots:?}"
        );
        assert!(
            !spots.iter().any(|(s, _)| s == "贝塔"),
            "初始(在用 tab)不该看见归档项目:{spots:?}"
        );

        let pos = spots
            .iter()
            .find(|(s, _)| s == crate::ui::project_list::Tab::Archived.label())
            .expect("「归档」tab 没画出来")
            .1;
        let mut input = egui::RawInput {
            events: vec![egui::Event::PointerMoved(pos)],
            ..Default::default()
        };
        for pressed in [true, false] {
            input.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            });
        }
        let out = draw(input, &mut ui_state);
        let spots = spots_of(&out);
        assert!(
            !spots.iter().any(|(s, _)| s == "阿尔法"),
            "点了「归档」tab,在用项目当帧还留在画面里:{spots:?}"
        );
        assert!(
            spots.iter().any(|(s, _)| s == "贝塔"),
            "点了「归档」tab,归档项目当帧没画出来:{spots:?}"
        );
    }

    // ---- 右栏:说明多行 / 内容滚动 / 按钮钉底(F237)------------------------

    /// 画两帧,量「说明」输入框**控件本身**的高度。
    ///
    /// 量的不是 galley —— 那是文字的高度,一行内容的多行框跟单行框一样高,
    /// 分不出来。走 [`note_field_id`] 拿控件矩形才问得着「这个框有几行」。
    fn note_field_height() -> f32 {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let p = proj(1, "接口", None);
        let mut ui_state = crate::ui::UiState {
            project_manager_open: true,
            project_selected: Some(p.id),
            project_draft: Some(p.clone()),
            ..Default::default()
        };
        let lamps = std::collections::BTreeMap::new();
        let ps = vec![p];
        let sessions: Vec<SessionRecord> = Vec::new();
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                show(
                    ctx,
                    &t,
                    &mut ui_state,
                    &ps,
                    &lamps,
                    &sessions,
                    &crate::ui::badge::AppearanceCache::default(),
                    None,
                );
            });
        }
        ctx.read_response(note_field_id())
            .expect("「说明」框没被登记 —— id 变了还是根本没画?")
            .rect
            .height()
    }

    /// F237:「说明」是多行框。
    ///
    /// 判据是**画出来的输入框高度**,不是源码里出现了 `multiline` ——
    /// 后者换个写法就恒绿。单行框约 20 高(一行文字 + 内边距),三行框必然
    /// 显著高于它,取 40 当阈值:两边各有一行的余量。
    ///
    /// 自证会变红:把 `multiline` 换回 `singleline`。
    #[test]
    fn the_note_field_is_tall_enough_to_hold_more_than_one_line() {
        let h = note_field_height();
        assert!(h > 40.0, "「说明」框只有 {h} 高 —— 还是单行的");
    }

    /// 屏幕矮到装不下右栏全部内容时,「删除项目」那一行**仍然落在屏幕里**。
    ///
    /// 判据是它画出来的**位置**,不是「画没画」——按顺序画时 egui 照样会把
    /// 按钮的 shape 交出来,只是坐标在屏幕下沿之外,人根本够不着。这正是
    /// 1080p + 150% 缩放(逻辑高 720)下的真实症状,而那是这个界面唯一的出口。
    ///
    /// 自证会变红:把 `TopBottomPanel::bottom` 那一段拆掉,改成排在滚动区
    /// **之后**按顺序画。
    #[test]
    fn the_way_out_survives_a_screen_too_short_for_the_form() {
        let spots = form_texts_on_a_short_screen();
        let del = spots
            .iter()
            .find(|(s, _)| s == "删除项目")
            .expect("「删除项目」压根没画出来");
        assert!(
            del.1.bottom() <= SHORT_H,
            "「删除项目」画在 y={} —— 屏幕只有 {SHORT_H} 高,够不着",
            del.1.bottom()
        );
    }

    /// 测试屏幕的逻辑高度。
    ///
    /// 压到 260 是有意的:封顶后右栏可用高约 200,而内容约 374 —— 差出一百多
    /// 像素,「按钮跟着内容走」和「按钮钉在底部」才分得开。420 那会儿两者只差
    /// 两像素,判据看起来在测钉底,其实只测出了封顶。
    const SHORT_H: f32 = 260.0;

    /// 在给定高度的屏幕上画两帧,收全部文字**和它们的位置**。
    /// `icon` 非空时把它塞进草稿的 `icon` 字段(F238 测试要看「已设图标」的样子)。
    fn form_texts_at(
        height: f32,
        icon: Option<mullion_store::IconSpec>,
    ) -> Vec<(String, egui::Rect)> {
        fn walk(shape: &egui::Shape, out: &mut Vec<(String, egui::Rect)>) {
            match shape {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                egui::Shape::Text(ts) => out.push((
                    ts.galley.text().to_string(),
                    egui::Rect::from_min_size(ts.pos, ts.galley.size()),
                )),
                _ => {}
            }
        }
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut p = proj(1, "接口", None);
        p.icon = icon;
        let mut ui_state = crate::ui::UiState {
            project_manager_open: true,
            project_selected: Some(p.id),
            project_draft: Some(p.clone()),
            ..Default::default()
        };
        let lamps = std::collections::BTreeMap::new();
        let ps = vec![p];
        let sessions: Vec<SessionRecord> = Vec::new();
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, height),
            )),
            ..Default::default()
        };
        let mut shapes = Vec::new();
        for _ in 0..2 {
            shapes = ctx
                .run(input(), |ctx| {
                    show(
                        ctx,
                        &t,
                        &mut ui_state,
                        &ps,
                        &lamps,
                        &sessions,
                        &crate::ui::badge::AppearanceCache::default(),
                        None,
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

    /// 在一块矮屏幕上画两帧,收全部文字**和它们的位置**。
    fn form_texts_on_a_short_screen() -> Vec<(String, egui::Rect)> {
        form_texts_at(SHORT_H, None)
    }

    /// 屏幕够高、右栏能画到「外观」分节,草稿没设图标。
    fn form_texts_with_selection() -> Vec<String> {
        form_texts_at(900.0, None)
            .into_iter()
            .map(|(s, _)| s)
            .collect()
    }

    /// 同上,但草稿已经设了一枚真实可解码的图标。
    fn form_texts_with_icon() -> Vec<String> {
        let icon = mullion_store::IconSpec {
            kind: mullion_store::IconKind::Ico,
            value: crate::ui::ico::import(&crate::ui::ico::tests_support::solid_ico(
                32,
                [255, 0, 0, 255],
            ))
            .expect("测试用 ico 应能导入"),
            bg: None,
        };
        form_texts_at(900.0, Some(icon))
            .into_iter()
            .map(|(s, _)| s)
            .collect()
    }

    /// F238:右栏有一个「外观」分节,能导入 .ico。
    ///
    /// 判据是**画面上的字**,不是「调了哪个函数」。
    ///
    /// 自证会变红:把 `appearance_section` 的调用注释掉。
    #[test]
    fn the_form_has_an_appearance_section_with_an_icon_import_button() {
        let texts = form_texts_with_selection();
        assert!(
            texts.iter().any(|s| s == "外观"),
            "右栏没有「外观」分节:{texts:?}"
        );
        assert!(
            texts.iter().any(|s| s.contains("导入 .ico")),
            "右栏没有导入 .ico 的入口:{texts:?}"
        );
    }

    /// 已经设了图标才有「清除」—— 没设时摆一颗按不动的按钮,用户会以为
    /// 自己漏看了什么(同会话侧「外观」页的判据)。
    ///
    /// 自证会变红:把 `draft.icon.is_some()` 那个条件去掉,让「清除」无条件出现。
    #[test]
    fn the_clear_button_only_shows_up_once_an_icon_is_set() {
        assert!(
            !form_texts_with_selection().iter().any(|s| s == "清除"),
            "还没设图标就摆出了「清除」"
        );
        let texts = form_texts_with_icon();
        assert!(
            texts.iter().any(|s| s == "清除"),
            "设了图标却没有「清除」:{texts:?}"
        );
    }

    /// 画两帧,`icon_error`/`icon_target` 按参数设定,收全部文字。
    fn form_texts_with_icon_error(icon_target: crate::ui::IconTarget) -> Vec<String> {
        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                egui::Shape::Text(ts) => out.push(ts.galley.text().to_string()),
                _ => {}
            }
        }
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let p = proj(1, "接口", None);
        let mut ui_state = crate::ui::UiState {
            project_manager_open: true,
            project_selected: Some(p.id),
            project_draft: Some(p.clone()),
            icon_error: Some("会话那边导入失败的旧错误".to_string()),
            icon_target,
            ..Default::default()
        };
        let lamps = std::collections::BTreeMap::new();
        let ps = vec![p];
        let sessions: Vec<SessionRecord> = Vec::new();
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 900.0),
            )),
            ..Default::default()
        };
        let mut shapes = Vec::new();
        for _ in 0..2 {
            shapes = ctx
                .run(input(), |ctx| {
                    show(
                        ctx,
                        &t,
                        &mut ui_state,
                        &ps,
                        &lamps,
                        &sessions,
                        &crate::ui::badge::AppearanceCache::default(),
                        None,
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

    /// F238:错误文案跟着归属走 —— 会话那边导入失败留下的话,不该出现在
    /// 项目的「外观」分节里。
    ///
    /// `icon_error` 是两处共用的一个字段,不门控的话那句话会原样串过来,
    /// 长得跟真错误一模一样。
    ///
    /// 自证会变红:把 `appearance_section` 里那道 `icon_target` 判据去掉。
    #[test]
    fn an_error_left_by_the_session_editor_does_not_show_up_in_the_project_form() {
        let msg = "会话那边导入失败的旧错误";
        let session_texts = form_texts_with_icon_error(crate::ui::IconTarget::Session);
        assert!(
            !session_texts.iter().any(|s| s.contains(msg)),
            "icon_target = Session 时,项目的外观分节不该画出这句话:{session_texts:?}"
        );
        let project_texts = form_texts_with_icon_error(crate::ui::IconTarget::Project);
        assert!(
            project_texts.iter().any(|s| s.contains(msg)),
            "icon_target = Project 时,这句话才该出现:{project_texts:?}"
        );
    }

    /// 跑两帧把右栏画出来,真点一次「导入 .ico…」,返回点击后的 `UiState`。
    ///
    /// 定位按钮靠**渲染出的文字锚点**,不靠猜坐标(同 `intent_after_clicking_add`
    /// 的手法)——按钮是普通 `ui.button(..)`,没有挂显式 id。
    fn ui_state_after_clicking_import_icon() -> crate::ui::UiState {
        fn find_pos(shapes: &[egui::epaint::ClippedShape], needle: &str) -> Option<egui::Pos2> {
            fn walk(shape: &egui::Shape, needle: &str) -> Option<egui::Pos2> {
                match shape {
                    egui::Shape::Vec(v) => v.iter().find_map(|s| walk(s, needle)),
                    egui::Shape::Text(ts) if ts.galley.text().contains(needle) => {
                        Some(ts.pos + ts.galley.size() / 2.0)
                    }
                    _ => None,
                }
            }
            shapes.iter().find_map(|cs| walk(&cs.shape, needle))
        }
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let p = proj(1, "接口", None);
        let mut ui_state = crate::ui::UiState {
            project_manager_open: true,
            project_selected: Some(p.id),
            project_draft: Some(p.clone()),
            ..Default::default()
        };
        let lamps = std::collections::BTreeMap::new();
        let ps = vec![p];
        let sessions: Vec<SessionRecord> = Vec::new();
        let draw = |input: egui::RawInput, ui_state: &mut crate::ui::UiState| {
            ctx.run(input, |ctx| {
                show(
                    ctx,
                    &t,
                    ui_state,
                    &ps,
                    &lamps,
                    &sessions,
                    &crate::ui::badge::AppearanceCache::default(),
                    None,
                );
            })
        };
        let _ = draw(egui::RawInput::default(), &mut ui_state);
        let out = draw(egui::RawInput::default(), &mut ui_state);
        let pos = find_pos(&out.shapes, "导入 .ico").expect("「导入 .ico…」没画出来");
        let mut input = egui::RawInput {
            events: vec![egui::Event::PointerMoved(pos)],
            ..Default::default()
        };
        for pressed in [true, false] {
            input.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            });
        }
        draw(input, &mut ui_state);
        ui_state
    }

    /// F238:点「导入 .ico…」要同时置好**两件事** —— 请求开文件框,
    /// 以及这次是替**谁**选的。
    ///
    /// 只置 `pick_icon_request` 而漏了 `icon_target` 的话,选完的那张图会被
    /// 写进会话编辑器(默认档是 `Session`),项目这边一切正常、什么都不显示,
    /// 而用户根本不知道自己刚把某条会话的图标改掉了。
    ///
    /// 自证会变红:把 `show` 里 `icon_target = IconTarget::Project` 那行删掉。
    #[test]
    fn clicking_import_says_the_icon_is_for_the_project_not_the_session() {
        let ui_state = ui_state_after_clicking_import_icon();
        assert!(
            ui_state.pick_icon_request,
            "点「导入 .ico…」没有请求打开文件框"
        );
        assert_eq!(
            ui_state.icon_target,
            crate::ui::IconTarget::Project,
            "文件框选完之后,这次导入不知道该写回谁的草稿"
        );
    }

    /// F238:导入成功 —— 草稿拿到解码后的图标,不报错。
    #[test]
    fn a_successfully_decoded_icon_is_written_into_the_draft() {
        let mut draft = proj(1, "接口", None);
        let bytes = crate::ui::ico::tests_support::solid_ico(32, [255, 0, 0, 255]);
        let err = apply_picked_icon(&mut draft, std::path::Path::new("x.ico"), Ok(bytes));
        assert_eq!(err, None, "能解码的 .ico 不该报错");
        assert_eq!(
            draft.icon.as_ref().map(|i| i.kind),
            Some(mullion_store::IconKind::Ico),
            "草稿没拿到解码后的图标"
        );
    }

    /// F238:导入失败 —— 报错,且**不动**草稿里原来那张图。
    ///
    /// 这条尤其重要:导入一张坏图不该把用户原来那张好图抹掉。
    ///
    /// 自证会变红:把解码失败那一支改成顺手把 `draft.icon` 置 `None`。
    #[test]
    fn a_failed_decode_reports_an_error_and_leaves_the_existing_icon_untouched() {
        let existing = mullion_store::IconSpec {
            kind: mullion_store::IconKind::Ico,
            value: crate::ui::ico::import(&crate::ui::ico::tests_support::solid_ico(
                32,
                [0, 255, 0, 255],
            ))
            .expect("测试用 ico 应能导入"),
            bg: None,
        };
        let mut draft = proj(1, "接口", None);
        draft.icon = Some(existing.clone());
        let err = apply_picked_icon(
            &mut draft,
            std::path::Path::new("bad.ico"),
            Ok(b"not an ico".to_vec()),
        );
        assert!(err.is_some(), "解码失败该报错");
        assert_eq!(draft.icon, Some(existing), "解码失败不该把原来那张好图抹掉");
    }

    /// 高屏幕上「归档」与「删除项目」两个按钮画出来的矩形。
    fn archive_and_delete_button_rects() -> (egui::Rect, egui::Rect) {
        let spots = form_texts_at(900.0, None);
        let find = |label: &str| {
            spots
                .iter()
                .find(|(s, _)| s == label)
                .unwrap_or_else(|| panic!("「{label}」压根没画出来"))
                .1
        };
        (find("归档"), find("删除项目"))
    }

    /// 归档按钮摆在「删除项目」**左边** —— 危险程度从右往左递减,
    /// 同 `pane_title` 那三个按钮的既有规矩(× 最右)。
    ///
    /// 量的是**画出来的横坐标**,不是源码里的书写顺序:`ui.horizontal` 里
    /// 换个位置写、但用 `right_to_left` 布局的话,源码顺序会骗人。
    ///
    /// 自证会变红:把「归档」那个 `if ui.button(..)` 挪到「删除项目」后面。
    #[test]
    fn the_archive_button_sits_to_the_left_of_delete() {
        let (archive, delete) = archive_and_delete_button_rects();
        assert!(
            archive.right() <= delete.left(),
            "归档按钮({archive:?})必须整个在删除按钮({delete:?})左边"
        );
    }

    /// 归档态决定按钮上的字。写死「归档」的话,已归档的项目右栏会给出一个
    /// 点了没有任何变化的按钮 —— 而归档是可逆的,撤销入口就是这一个。
    #[test]
    fn the_button_says_undo_when_the_project_is_already_archived() {
        assert_eq!(archive_button_label(false), "归档");
        assert_eq!(archive_button_label(true), "取消归档");
    }
}
