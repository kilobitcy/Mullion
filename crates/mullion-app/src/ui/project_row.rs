//! F233/F234:三处项目列表共用的**一行**。
//!
//! 为什么抽出来:改之前三处各写一份 —— 项目管理器左栏是 `selectable_label`,
//! 启动页是 egui 部件嵌套 + `interact`,pane 切换弹窗是纯 painter 手绘。本切片
//! 要往行里同时加搜索命中高亮和相对时间,三份实现各改一遍必然漂移。而「顺序」
//! 这件小得多的事,项目里已经为它写了三条注释反复强调复用 `by_recent_access`
//! (理由:两处顺序不一样用户第一眼就看得出来)。行的**内容**比顺序更显眼。
//!
//! **手绘而不是 egui 部件拼装**:两行文字 + 右对齐的时间列 + 命中分段着色,
//! `selectable_label` 一样都做不到;而 F141 那条「侧栏本地栏一行都点不中」的
//! 教训要求判定矩形罩住整行 —— 手绘天然是「先 `allocate_exact_size` 整行矩形,
//! 再 `interact`」。
//!
//! F238:图标槽位已加上,画在灯之后、文字之前(见 `ICON_X`/`ICON_SIDE`)。
//! 有图标就画、没有就空着 —— 槽位固定不收窄,理由同灯槽:有灯没灯的行文字
//! 左边界必须对齐,不然搜索结果混排时名字会左右错开一截。

use mullion_store::{ProjectId, ProjectRecord, SessionRecord};

use crate::theme::{self, Theme};

/// 行高。两行文字加上下内边距。
///
/// 与会话侧 `session_manager::list::row_h(Density::Full)` 的 48 **刻意一致** ——
/// 两个列表在同一个程序里,行高不一样会显得像两个软件。
pub const ROW_H: f32 = 48.0;

/// 灯的槽位中心距行左边缘(逻辑点)。
const LAMP_X: f32 = 14.0;
/// 图标槽左边缘距行左边缘。紧挨着灯槽右沿。
const ICON_X: f32 = 24.0;
/// 图标边长。走 F61 那套 32px 纹理档(`paint_icon` 按 `side <= 32` 选档),
/// 比 32 略小一点是为了在 48 点行高里上下留出呼吸。
const ICON_SIDE: f32 = 28.0;
/// 文字左边界 = 图标槽右沿 + 一点呼吸。**恒定**:图标是「有就画、没有就
/// 留空」的,有图标没图标的行文字左边界必须对齐(同灯槽那条理由)。
const TEXT_X: f32 = ICON_X + ICON_SIDE + 6.0;
/// 文字区距行右边缘的留白。
const TEXT_RIGHT_PAD: f32 = 8.0;
/// 名称行顶距行顶。
const NAME_TOP: f32 = 6.0;
/// 副标题行顶距行顶。
const SUB_TOP: f32 = 27.0;
/// 名称字号。
const NAME_SIZE: f32 = 14.0;
/// 副标题与时间字号。
const SUB_SIZE: f32 = 11.0;
/// 名称与时间列之间的最小间隙 —— 顶到一起会读成一个词。
const NAME_TIME_GAP: f32 = 8.0;

/// 画一行要的全部输入。
pub struct Row<'a> {
    pub project: &'a ProjectRecord,
    pub lamp: crate::project::Lamp,
    /// 用来把节点 id 解析成机器名。三处调用方手上都有全表。
    pub sessions: &'a [SessionRecord],
    /// 当前搜索词。空串 = 没在搜索,`segments` 会返回整段不高亮。
    pub query: &'a str,
    /// 这一行是不是右栏正在编辑的那个。只有项目管理器左栏会传 `true`。
    pub selected: bool,
    /// 「现在几点」。由调用方**一帧取一次**传进来 —— 每行各取一次
    /// `now_utc()` 等于每帧几十次系统调用,而这个项目为了空闲期的 CPU 花了
    /// 整整八个切片(F157~F183)。
    pub now: time::OffsetDateTime,
    /// 这是哪个列表(`"manager"` / `"launcher"` / `"pick"`)。
    ///
    /// **必须区分**:项目管理器是弹窗、启动页是 `CentralPanel`,同一个项目在
    /// 两处的行会算出同一个 egui id,交互互相打架。
    pub list: &'static str,
    /// F238:这一行的图标。由调用方用 [`crate::project::icon_for`] 解析好
    /// 传进来 —— 三处列表各解析一遍必然漂移。`None` = 项目没设、首选节点
    /// 也没有,槽位留空但**不收窄**(文字左边界恒定)。
    pub icon: Option<&'a mullion_store::IconSpec>,
    /// 图标底色。走 [`crate::project::icon_bg`],同源回落首选节点的节点色。
    pub icon_bg: Option<egui::Color32>,
}

/// 一行的副标题:`目录 · 节点名`。
///
/// 节点名解析不出来(会话被别的实例删了、或项目还没勾节点)时**只显示目录**,
/// 不写「(未知)」一类占位:那句话对用户没有任何可操作性,而目录本身已经足以
/// 认出这是哪个活。
///
/// 选节点走 [`crate::project::node_for`] —— 和 `plan_open` 真拨号时用的是**同一
/// 个函数**。各写一份的话,列表上写着 A、点下去连的是 B。
pub fn subtitle(p: &ProjectRecord, sessions: &[SessionRecord]) -> String {
    let name = crate::project::node_for(p)
        .and_then(|id| sessions.iter().find(|s| s.id == id))
        .map(|s| s.identity.name.as_str());
    match name {
        Some(n) => format!("{} · {}", p.dir, n),
        None => p.dir.clone(),
    }
}

/// F245:这一行副标题最终画什么。
///
/// 默认是 [`subtitle`];但当某个查询词**只**在说明 / tmux 名里命中时,换成
/// 那段正文的片段(见 [`crate::project::hidden_hit_snippet`])。
///
/// 为什么必须有这一层:F245 让搜索收了说明和 tmux 名,而这两样**行上一个字
/// 都不显示**。不换的话,搜出来的行既没有高亮、也没有任何线索说明它凭什么
/// 出现 —— 那正是 F233 当初否掉「收 note」的理由,原样吃回去等于确认了它。
///
/// **只在「只有隐藏字段命中」时换**:普通搜索(打项目名)的行不该平白变样,
/// 目录和节点名比一段说明更能认出这是哪个活。
pub fn subtitle_for_query(p: &ProjectRecord, sessions: &[SessionRecord], query: &str) -> String {
    crate::project::hidden_hit_snippet(p, sessions, query).unwrap_or_else(|| subtitle(p, sessions))
}

/// 行尾那一列时间。
///
/// 从没打开过的说「从未打开」,不留空 —— 空白会被读成「这一列坏了」,而
/// 「从未打开」本身就是用户要的信息(尤其在按时间排序的列表里,它解释了这些
/// 行为什么都堆在最下面)。
pub fn time_text(p: &ProjectRecord, now: time::OffsetDateTime) -> String {
    // F257:归档态优先。搜索穿透两态之后结果里会混着两种行,不标的话用户点开
    // 一个归档项目却不知道它是归档的。
    //
    // **占用时间列而不是新加一列**:行宽在弹窗里是最紧张的资源(pane 宽度减
    // 内边距),新加一列会把名字挤掉一截;而归档项目的「最后打开时间」本来就是
    // 这一行上最没用的信息。
    if let Some(at) = p.archived_at.as_deref() {
        return format!(
            "已归档 · {}",
            crate::localtime::relative(at, now, crate::localtime::offset())
        );
    }
    match p.last_accessed_at.as_deref() {
        Some(s) => crate::localtime::relative(s, now, crate::localtime::offset()),
        None => "从未打开".to_string(),
    }
}

/// 一盏灯长什么样、以及它是什么意思。
///
/// 颜色**不承担区分职责**,形状才是:实心/空心/带点各不相同。色觉障碍、以及
/// 深色底上绿灰难辨的情况下,这盏灯仍然读得出来。**不写字符**:●/○/◐ 都在
/// GBK 外,egui 的两级字体链画不出来就是豆腐块,而那在 Linux 开发机上多半是
/// 正常的(T9)。
fn lamp_look(
    lamp: crate::project::Lamp,
    t: &Theme,
) -> (
    crate::ui::icon::Glyph,
    mullion_term::snapshot::Rgb,
    &'static str,
) {
    use crate::project::Lamp;
    use crate::ui::icon::Glyph;
    match lamp {
        Lamp::Lit => (
            Glyph::LampLit,
            t.ok,
            "正在跑:有终端接在这个项目的 tmux 会话上",
        ),
        Lamp::Dark => (
            Glyph::LampDark,
            t.fg_muted,
            "没在跑:本机所有终端都已上报,没有一个接在它上面",
        ),
        Lamp::Unknown => (
            Glyph::LampUnknown,
            t.warn,
            "还不确定:有终端还没上报过状态,它可能正接在这个项目上",
        ),
    }
}

/// 一行的交互 id。**按项目主键 + 列表名推**,不用自动 id:自动 id 由控件在
/// `Ui` 里的出现次序算出来,测试要点某一行就只能猜坐标。带上 `list` 是因为
/// 同一个项目会同时出现在三处列表里,共用一个 id 的话 egui 会认成同一个控件。
pub fn row_id(list: &'static str, id: ProjectId) -> egui::Id {
    egui::Id::new(("project_row", list, id.0))
}

/// 画一行,返回它的 `Response`。调用方自己判 `clicked()`。
pub fn show(ui: &mut egui::Ui, t: &Theme, row: &Row) -> egui::Response {
    let w = ui.available_width();
    // 先占整行矩形再 `interact`:靠里面某个 label 的 `sense` 的话,只有字上那
    // 几十个像素点得中 —— F141 那条「侧栏本地栏一行都点不中」就是这么来的,
    // 而它的症状**完全静默**:界面画得好好的,点了没反应。
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, ROW_H), egui::Sense::hover());
    let id = row_id(row.list, row.project.id);
    let resp = ui.interact(rect, id, egui::Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    // 自绘的行在 accesskit 树里是个没名字的空节点 —— 补一个名字,给屏幕阅读器
    // 和 F100 的自动候选用(同 `icon_button` / `lamp_dot` 的理由)。**在早退
    // 之前登记**:滚出可视区的行不画,但它仍然是个存在的部件。
    let label = row.project.name.clone();
    resp.widget_info(move || egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
    if !ui.is_rect_visible(rect) {
        return resp;
    }

    // 底色走会话侧同一份 `row_bg`:选中/悬停两态的透明度在那里定死,两个列表
    // 各调一次的话同一种状态会有两种深浅。
    if let Some(bg) =
        crate::ui::session_manager::list::row_bg(row.selected, resp.hovered(), None, t)
    {
        ui.painter().rect_filled(rect, 4.0, bg);
    }

    let p = ui.painter();

    // 灯。形状与含义见 `lamp_look`。
    let side = ROW_H * 0.28;
    let dot = egui::Rect::from_center_size(
        egui::pos2(rect.left() + LAMP_X, rect.center().y),
        egui::vec2(side, side),
    );
    let (glyph, color, tip) = lamp_look(row.lamp, t);
    p.extend(crate::ui::icon::shapes(
        dot,
        glyph,
        egui::Stroke::new(1.4, theme::c32(color)),
    ));
    // 三态各配一句话。「未知」尤其需要说明 —— 一个既不亮也不灭的圈,不解释的话
    // 用户只会当它坏了(F224 定的)。
    //
    // 手画 tooltip、**不新建部件**:在灯上再 `interact` 一次会跟整行那个判定
    // 矩形重叠,而整行可点是这一行存在的理由(F141)。
    if resp.hovered()
        && ui
            .ctx()
            .pointer_latest_pos()
            .is_some_and(|q| dot.expand(4.0).contains(q))
    {
        egui::show_tooltip_at_pointer(ui.ctx(), ui.layer_id(), id.with("lamp"), |ui| {
            ui.label(tip);
        });
    }

    // 图标(F238)。**画在灯之后、文字之前**:槽位固定,有就画、没有就空着。
    if let Some(icon) = row.icon {
        let slot = egui::Rect::from_center_size(
            egui::pos2(rect.left() + ICON_X + ICON_SIDE / 2.0, rect.center().y),
            egui::vec2(ICON_SIDE, ICON_SIDE),
        );
        crate::ui::badge::paint_icon(p, slot, icon, row.icon_bg);
    }

    let text_left = rect.left() + TEXT_X;
    let text_avail = (rect.right() - TEXT_RIGHT_PAD - text_left).max(0.0);

    // 时间先量宽度:名称的可用宽度要**先**扣掉它。不扣的话长项目名会一路截断
    // 到右边缘,把时间整列挤出行外 —— 整条看不见,而且没有任何报错。
    let time = time_text(row.project, row.now);
    let time_galley = p.layout_no_wrap(
        time,
        egui::FontId::proportional(SUB_SIZE),
        theme::c32(t.fg_muted),
    );
    let time_w = time_galley.size().x;
    p.galley(
        egui::pos2(
            rect.right() - TEXT_RIGHT_PAD - time_w,
            rect.top() + NAME_TOP + (NAME_SIZE - SUB_SIZE) / 2.0,
        ),
        time_galley,
        theme::c32(t.fg_muted),
    );

    // 名称和副标题都过命中着色。副标题里就是目录和节点名 —— 搜「web01」命中的
    // 正是那里,不标出来的话用户完全不知道这一行为什么会出现。
    crate::ui::session_manager::list::paint_highlighted(
        p,
        egui::pos2(text_left, rect.top() + NAME_TOP),
        &row.project.name,
        row.query,
        egui::FontId::proportional(NAME_SIZE),
        theme::c32(t.fg_strong),
        t,
        (text_avail - time_w - NAME_TIME_GAP).max(0.0),
    );
    crate::ui::session_manager::list::paint_highlighted(
        p,
        egui::pos2(text_left, rect.top() + SUB_TOP),
        &subtitle_for_query(row.project, row.sessions, row.query),
        row.query,
        egui::FontId::proportional(SUB_SIZE),
        theme::c32(t.fg_muted),
        t,
        text_avail,
    );

    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_store::{ProjectId, Protocol, SessionId};

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

    fn now() -> time::OffsetDateTime {
        time::OffsetDateTime::parse(
            "2026-09-08T12:00:00Z",
            &time::format_description::well_known::Rfc3339,
        )
        .expect("测试基准时间写错了")
    }

    const SCREEN_W: f32 = 600.0;

    /// 一行必须同时说清**在哪台机器上的哪个目录** —— 只有项目名的话,
    /// 「api」和「api(测试机)」这种命名在列表里根本分不出来。
    #[test]
    fn a_row_names_both_the_directory_and_the_node_it_will_dial() {
        let s = vec![sess(7, "web01")];
        assert_eq!(
            subtitle(&proj(1, "接口", "/srv/api", None), &s),
            "/srv/api · web01"
        );
    }

    /// 节点解析不出来(会话被别的实例删了)只显示目录,**不写占位文字**。
    #[test]
    fn a_row_whose_node_is_gone_still_says_which_directory_it_is() {
        assert_eq!(
            subtitle(&proj(1, "接口", "/srv/api", None), &[]),
            "/srv/api"
        );
    }

    /// 从没打开过的行要说「从未打开」,不能留空 —— 空白会被读成「这一列坏了」,
    /// 而「从未打开」本身就是用户要的信息。
    ///
    /// 自证会变红:把 `None` 那一臂改成 `String::new()`。
    #[test]
    fn a_project_that_was_never_opened_says_so_instead_of_leaving_a_blank() {
        assert_eq!(
            time_text(&proj(1, "接口", "/srv/api", None), now()),
            "从未打开"
        );
    }

    #[test]
    fn a_project_opened_this_morning_shows_a_relative_time() {
        let p = proj(1, "接口", "/srv/api", Some("2026-09-08T09:00:00Z"));
        assert_eq!(time_text(&p, now()), "3 小时前");
    }

    /// F257:归档项目的时间列改写「已归档 · <相对时间>」。
    ///
    /// 搜索穿透两态之后,结果里会混着在用的和归档的 —— 不标的话用户点开一个
    /// 归档项目却不知道它是归档的,而「为什么它在列表里」这个问题没人回答。
    ///
    /// 复用**时间列**而不是新加一列:行宽是弹窗里最紧张的资源(pane 宽度减去
    /// 内边距),新加一列会把名字挤掉一截;而归档项目的「最后打开时间」本来就是
    /// 这一行上最没用的信息。
    ///
    /// 自证会变红:把 `time_text` 里的归档分支删掉。
    #[test]
    fn an_archived_project_says_so_in_the_time_column() {
        let now = time::OffsetDateTime::parse(
            "2026-09-11T12:00:00Z",
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap();
        let mut p = proj(1, "老活", "/data/old", Some("2026-09-01T00:00:00Z"));
        assert!(!time_text(&p, now).contains("归档"), "在用的项目不该说归档");
        p.archived_at = Some("2026-09-10T12:00:00Z".into());
        let s = time_text(&p, now);
        assert!(s.starts_with("已归档"), "归档态要一眼看得见:{s}");
    }

    /// 归档态的时间取的是**归档时间**,不是最后打开时间 —— 两者在这条判据里
    /// 被造成不同的相对档位,取错会红。
    #[test]
    fn the_archived_row_shows_when_it_was_archived_not_when_it_was_opened() {
        let now = time::OffsetDateTime::parse(
            "2026-09-11T12:00:00Z",
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap();
        let mut p = proj(1, "老活", "/data/old", Some("2026-01-01T00:00:00Z"));
        p.archived_at = Some("2026-09-11T11:00:00Z".into());
        let s = time_text(&p, now);
        let opened =
            crate::localtime::relative("2026-01-01T00:00:00Z", now, crate::localtime::offset());
        assert!(!s.contains(&opened), "取成了最后打开时间:{s}");
    }

    /// **整行**可点,不是只有那几个字可点。
    ///
    /// 列表里一行有一大片空白(名字右边到时间列之间),用户瞄准的是「那一条」,
    /// 落点几乎不可能正好在字上。F141 那条「侧栏本地栏一行都点不中」就是判定
    /// 矩形只罩住了内容 —— 症状**完全静默**:界面画得好好的,点了没反应。
    ///
    /// 自证会变红:把 `show` 里 `ui.interact(rect, ..)` 的矩形换成
    /// `egui::Rect::from_min_size(rect.min, egui::vec2(60.0, ROW_H))`。
    #[test]
    fn the_whole_row_is_clickable_not_just_the_name() {
        assert!(
            clicked_at(0.85),
            "点在行的右半边没反应 —— 判定矩形没罩住整行"
        );
        assert!(clicked_at(0.1), "点在行的左端没反应");
    }

    /// 跑两帧,在**行占位区**宽度的 `frac` 处点一下,返回是否点中。
    ///
    /// **落点必须从 `show` 调用之前的可用区算,不能从它返回的 `Response.rect`
    /// 算。** 后者就是判定矩形本身:判定矩形一缩小,落点按比例跟着缩小,永远
    /// 命中 —— 这条守护会变成恒绿。这不是假设,是本切片实测到的:第一版就是
    /// 那么写的,把 `ui.interact` 的矩形砍成 60px 宽,测试照样全绿。
    ///
    /// **两帧**:`CentralPanel` 首帧只记 `Shape::Noop`,布局矩形要下一帧才稳。
    fn clicked_at(frac: f32) -> bool {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let p = proj(3, "接口", "/srv/api", None);
        let ss = vec![sess(7, "web01")];
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(SCREEN_W, 400.0));
        let base = || egui::RawInput {
            screen_rect: Some(screen),
            ..Default::default()
        };
        let row = || Row {
            project: &p,
            lamp: crate::project::Lamp::Unknown,
            sessions: &ss,
            query: "",
            selected: false,
            now: now(),
            list: "test",
            icon: None,
            icon_bg: None,
        };
        // 这一行**应该**占住的地方:光标位置 + 整条可用宽 + `ROW_H`。
        let mut slot = egui::Rect::NOTHING;
        for _ in 0..2 {
            let _ = ctx.run(base(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    slot = egui::Rect::from_min_size(
                        ui.next_widget_position(),
                        egui::vec2(ui.available_width(), ROW_H),
                    );
                    let _ = show(ui, &t, &row());
                });
            });
        }
        let pos = egui::pos2(slot.left() + slot.width() * frac, slot.center().y);
        let mut input = base();
        input.events.push(egui::Event::PointerMoved(pos));
        for pressed in [true, false] {
            input.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            });
        }
        let mut hit = false;
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                hit = show(ui, &t, &row()).clicked();
            });
        });
        hit
    }

    /// 搜索命中的片段要染成 accent。**过滤了却标不出命中在哪,用户会以为搜索
    /// 坏了**(走查 22 的原话)—— 尤其在项目列表里,搜「web01」命中的是副标题
    /// 里的节点名,不标出来的话这一行为什么会出现完全没线索。
    ///
    /// 判据是「画出来的文字里有 accent 色的那一段」,不是「调用了
    /// `paint_highlighted`」—— 后者是读源码,换个函数名就恒绿。
    ///
    /// 自证会变红:把两处 `paint_highlighted` 的 `row.query` 实参换成 `""`。
    #[test]
    fn the_matching_piece_is_tinted_so_the_user_can_see_why_this_row_showed_up() {
        let t = crate::theme::MULLION_DARK;
        let accent = crate::theme::c32(t.accent);
        let ctx = egui::Context::default();
        let p = proj(3, "接口", "/srv/api", None);
        let ss = vec![sess(7, "web01")];
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(SCREEN_W, 400.0));
        let mut shapes = Vec::new();
        for _ in 0..2 {
            shapes = ctx
                .run(
                    egui::RawInput {
                        screen_rect: Some(screen),
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            show(
                                ui,
                                &t,
                                &Row {
                                    project: &p,
                                    lamp: crate::project::Lamp::Unknown,
                                    sessions: &ss,
                                    query: "web01",
                                    selected: false,
                                    now: now(),
                                    list: "test",
                                    icon: None,
                                    icon_bg: None,
                                },
                            );
                        });
                    },
                )
                .shapes;
        }
        assert!(
            shapes.iter().any(|cs| has_accent_text(&cs.shape, accent)),
            "命中「web01」的那一段没有染成 accent —— 用户看不出这一行为什么会出现"
        );
    }

    /// 这堆 shape 里有没有一段用 accent 色画的文字。
    ///
    /// `Shape::Text` 的每个 section 各带自己的 `format.color`,所以要下钻到
    /// `galley.job.sections` —— 只看 `TextShape` 顶层那个 `fallback_color` 的话,
    /// 分段着色的命中色根本不在那里。
    fn has_accent_text(shape: &egui::Shape, accent: egui::Color32) -> bool {
        match shape {
            egui::Shape::Vec(v) => v.iter().any(|s| has_accent_text(s, accent)),
            egui::Shape::Text(ts) => ts
                .galley
                .job
                .sections
                .iter()
                .any(|s| s.format.color == accent),
            _ => false,
        }
    }

    /// F245:只命中说明时,副标题换成那段说明的片段;否则照旧是「目录 · 节点名」。
    #[test]
    fn the_subtitle_falls_back_to_the_note_only_when_nothing_visible_matched() {
        let ss = vec![sess(7, "web01")];
        let mut p = proj(1, "proj-7", "/srv/api", None);
        p.note = "每天凌晨跑爬虫".into();
        assert!(
            subtitle_for_query(&p, &ss, "爬虫").contains("爬虫"),
            "只命中说明,副标题该换成说明片段"
        );
        assert_eq!(
            subtitle_for_query(&p, &ss, "web01"),
            "/srv/api · web01",
            "命中的是节点名,副标题不该变样"
        );
        assert_eq!(
            subtitle_for_query(&p, &ss, ""),
            "/srv/api · web01",
            "没在搜索时副标题不该变样"
        );
    }

    /// F245:**行上真的画出了**那段说明片段。
    ///
    /// 判据是「画出来的文字里有说明的正文」,不是「`show` 里调了
    /// `subtitle_for_query`」—— 后者是读源码,换个函数名就恒绿。这条守的正是
    /// 「纯函数测得扎实、接线没人看着」那类缺口。
    ///
    /// 自证会变红:把 `show` 里的 `subtitle_for_query(row.project, row.sessions,
    /// row.query)` 换回 `subtitle(row.project, row.sessions)`。
    #[test]
    fn a_row_that_only_matched_the_note_actually_paints_the_note() {
        let mut p = proj(3, "proj-7", "/srv/api", None);
        p.note = "每天凌晨跑爬虫".into();
        let painted = painted_text(&p, "爬虫");
        assert!(
            painted.iter().any(|s| s.contains("爬虫")),
            "行上没画出说明片段,用户看不出这一行凭什么出现:{painted:?}"
        );
    }

    /// 跑两帧,收集这一行画出来的全部文字。
    fn painted_text(p: &ProjectRecord, query: &str) -> Vec<String> {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let ss = vec![sess(7, "web01")];
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(SCREEN_W, 400.0));
        let mut out = Vec::new();
        for _ in 0..2 {
            out = ctx
                .run(
                    egui::RawInput {
                        screen_rect: Some(screen),
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            show(
                                ui,
                                &t,
                                &Row {
                                    project: p,
                                    lamp: crate::project::Lamp::Unknown,
                                    sessions: &ss,
                                    query,
                                    selected: false,
                                    now: now(),
                                    list: "test",
                                    icon: None,
                                    icon_bg: None,
                                },
                            );
                        });
                    },
                )
                .shapes;
        }
        let mut texts = Vec::new();
        fn walk(s: &egui::Shape, out: &mut Vec<String>) {
            match s {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                egui::Shape::Text(ts) => out.push(ts.galley.job.text.clone()),
                _ => {}
            }
        }
        out.iter().for_each(|cs| walk(&cs.shape, &mut texts));
        texts
    }

    /// 三盏灯各自有一句**互不相同**的说明。
    ///
    /// 「未知」尤其需要它:一个既不亮也不灭的圈,不解释的话用户只会当它坏了
    /// (F224 定的)。这条判据钉的是「说明还在、且三态确实说的是三件事」——
    /// 原来那份说明挂在 `project_manager::lamp_dot` 上,换成共享行的时候差点
    /// 连着那个函数一起被删掉,而丢了它**画面上完全看不出来**。
    ///
    /// 自证会变红:把任意两臂的 tip 写成同一句。
    #[test]
    fn all_three_lamps_explain_themselves_differently() {
        use crate::project::Lamp;
        let t = &crate::theme::MULLION_DARK;
        let tips: Vec<&str> = [Lamp::Lit, Lamp::Dark, Lamp::Unknown]
            .into_iter()
            .map(|l| lamp_look(l, t).2)
            .collect();
        assert!(
            tips.iter().all(|s| !s.is_empty()),
            "有一盏灯没有说明:{tips:?}"
        );
        let uniq: std::collections::BTreeSet<_> = tips.iter().collect();
        assert_eq!(uniq.len(), 3, "三态的说明重了:{tips:?}");
    }

    // ---- F238:图标 --------------------------------------------------------

    /// F238:行上真的画出了那张图。
    ///
    /// 判据是「画面上出现了一张 `Shape::Mesh` 且纹理不是字体图集」,不是
    /// 「调用了 `paint_icon`」—— 后者是读源码,换个函数名就恒绿。
    ///
    /// 自证会变红:把 `show` 里那段 `paint_icon` 删掉;或把 `ICON_SIDE`
    /// 改成 `0.0`(矩形退化,`paint_icon` 走降级不画)。
    #[test]
    fn a_row_paints_the_icon_it_was_given() {
        assert!(
            has_image(&row_shapes(Some(&test_ico()))),
            "给了图标却一张图都没画出来"
        );
        assert!(!has_image(&row_shapes(None)), "没给图标却凭空画了一张图");
    }

    /// 有图标没图标的行,**文字左边界必须一样** —— 两种行混在一列里,
    /// 名字左右错开 30 点比缺一张图难看得多(同灯槽那条恒定判据)。
    ///
    /// 自证会变红:把 `show` 里的 `text_left` 改成
    /// `rect.left() + if row.icon.is_some() { TEXT_X } else { LAMP_X + 12.0 }`。
    #[test]
    fn the_text_starts_at_the_same_x_whether_or_not_there_is_an_icon() {
        let with = first_text_x(&row_shapes(Some(&test_ico())));
        let without = first_text_x(&row_shapes(None));
        assert_eq!(
            with, without,
            "有图标/没图标两种行的文字左边界不一样:{with:?} vs {without:?}"
        );
    }

    fn test_ico() -> mullion_store::IconSpec {
        // 一张真 ico:`paint_icon` 会先解码,解不开就整段不画,
        // 拿假 base64 的话这条测试会因为「解码失败」而假红。
        mullion_store::IconSpec {
            kind: mullion_store::IconKind::Ico,
            value: crate::ui::ico::import(&crate::ui::ico::tests_support::solid_ico(
                32,
                [255, 0, 0, 255],
            ))
            .expect("测试用 ico 应能导入"),
            bg: None,
        }
    }

    /// 跑两帧,返回这一行画出来的全部 shape。
    fn row_shapes(icon: Option<&mullion_store::IconSpec>) -> Vec<egui::epaint::ClippedShape> {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let p = proj(3, "接口", "/srv/api", None);
        let ss = vec![sess(7, "web01")];
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(SCREEN_W, 400.0));
        let mut out = Vec::new();
        for _ in 0..2 {
            out = ctx
                .run(
                    egui::RawInput {
                        screen_rect: Some(screen),
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            show(
                                ui,
                                &t,
                                &Row {
                                    project: &p,
                                    lamp: crate::project::Lamp::Unknown,
                                    sessions: &ss,
                                    query: "",
                                    selected: false,
                                    now: now(),
                                    list: "test",
                                    icon,
                                    icon_bg: None,
                                },
                            );
                        });
                    },
                )
                .shapes;
        }
        out
    }

    fn has_image(shapes: &[egui::epaint::ClippedShape]) -> bool {
        shapes.iter().any(|cs| contains_image(&cs.shape))
    }

    fn contains_image(s: &egui::Shape) -> bool {
        match s {
            egui::Shape::Vec(v) => v.iter().any(contains_image),
            // 「有面积」这一半不是凑数:`paint_icon` 对退化矩形没有 early-return,
            // 边长 0 的槽照样发出一个带真纹理的 `Mesh` —— 只判纹理的话,把
            // `ICON_SIDE` 改成 0 这条变异杀不掉(实测过),而画面上一张图都看不见。
            egui::Shape::Mesh(m) => {
                let b = m.calc_bounds();
                m.texture_id != egui::TextureId::default() && b.width() > 0.0 && b.height() > 0.0
            }
            _ => false,
        }
    }

    /// 这一行第一段文字的左边界 x。
    fn first_text_x(shapes: &[egui::epaint::ClippedShape]) -> Option<u32> {
        fn walk(s: &egui::Shape, out: &mut Vec<f32>) {
            match s {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                egui::Shape::Text(ts) => out.push(ts.pos.x),
                _ => {}
            }
        }
        let mut xs = Vec::new();
        shapes.iter().for_each(|cs| walk(&cs.shape, &mut xs));
        // 时间列在最右,名称/副标题在左 —— 取最小的那个就是文字左边界。
        xs.into_iter().map(|x| x.round() as u32).min()
    }
}
