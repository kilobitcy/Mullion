//! 会话管理器**左栏**:搜索框、分组树、手绘会话行、底部「+ 新建」、删除二次确认。
//!
//! 只读 `UiFrame` 的数据,只往 `UiState` 写意图 —— 不碰 `SessionStore`
//! (egui 闭包里拿不到 `&mut SessionStore`,这是 app 侧的硬约束)。

use egui::{NumExt as _, Ui};
use mullion_store::model::SessionRecord;
use mullion_store::{GroupRecord, SessionId};

use crate::theme::{self, Theme};
use crate::ui::session_manager::{group_header, SwitchTarget};
use crate::ui::UiState;

/// 一行会话的高度(设计稿 §4.1:两行文字 + 上下 8px)。
const ROW_H: f32 = 44.0;

/// 会话是否命中搜索。空查询放行全部。名称 / 主机 / 标签三处都查,
/// 大小写不敏感 —— 用户记得住的常是 IP 尾数或标签,不是当初起的名字。
pub(crate) fn matches(rec: &SessionRecord, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    rec.identity.name.to_lowercase().contains(&q)
        || rec.connection.host.to_lowercase().contains(&q)
        || rec
            .identity
            .tags
            .iter()
            .any(|t| t.to_lowercase().contains(&q))
}

/// 手绘一行会话。不用 `selectable_label`:设计稿要「状态点 + 名称 + user@host
/// 两行 + 选中态左侧强调条」,`selectable_label` 只画得出单行文本。
fn session_row(
    ui: &mut Ui,
    t: &Theme,
    rec: &SessionRecord,
    selected: bool,
    connected: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), ROW_H),
        egui::Sense::click(),
    );
    let p = ui.painter();

    let bg = if selected {
        theme::c32(t.sunken_bg)
    } else if resp.hovered() {
        theme::c32(t.panel_head)
    } else {
        egui::Color32::TRANSPARENT
    };
    p.rect_filled(rect, egui::Rounding::same(6.0), bg);
    if selected {
        p.rect_filled(
            egui::Rect::from_min_size(rect.min, egui::vec2(3.0, ROW_H)),
            egui::Rounding::same(2.0),
            theme::c32(t.accent),
        );
    }

    // §6:状态点只有两态 —— 已连接(ok 绿)/ 未连接(fg_ghost 灰)。
    // 「连接中」态做不出来:UserEvent::ConnectOk/ConnectErr 都不带 SessionId,
    // 无法把在途连接归到某一行上。
    p.circle_filled(
        egui::pos2(rect.left() + 16.0, rect.center().y),
        4.0,
        if connected {
            theme::c32(t.ok)
        } else {
            theme::c32(t.fg_ghost)
        },
    );
    // §6.3:状态点加 tooltip。它是手绘的,没有 Response,只能补一次
    // interact —— 否则用户只能靠猜「这个绿点是什么意思」。
    //
    // 这一层 hover-only interact 会不会抢走 `resp.hovered()`(整行高亮背景
    // 依赖它)?不会:egui-0.30.0 `interaction.rs::interact()` 里,当前没有
    // 点击/拖拽发生时,`hovered` 集合 = `hits.click ∪ hits.drag` 再加上「所有
    // 注册顺序不早于 `top_interactive_order`(即最上层可点击/拖拽部件)的
    // `contains_pointer` 部件」(见该函数 243-284 行的注释与实现)。`dot_rect`
    // 只 sense `hover()`,不参与 `hits.click` 的判定,`hits.click` 仍然是这一行
    // 本身(`allocate_exact_size` 用 `Sense::click()` 注册,是当前唯一的
    // 可点击命中);而 dot 的 `interact()` 调用在这一行之后才发生,注册顺序
    // 更靠后(更「上层」),所以会被上述规则一并并入 `hovered` 集合 —— 行和
    // 点会同时 hovered,不是互斥关系。
    let dot_rect = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 16.0, rect.center().y),
        egui::vec2(12.0, 12.0),
    );
    ui.interact(dot_rect, resp.id.with("dot"), egui::Sense::hover())
        .on_hover_text(if connected { "已连接" } else { "未连接" });
    p.text(
        egui::pos2(rect.left() + 30.0, rect.top() + 7.0),
        egui::Align2::LEFT_TOP,
        &rec.identity.name,
        egui::FontId::proportional(14.0),
        theme::c32(t.fg),
    );
    p.text(
        egui::pos2(rect.left() + 30.0, rect.top() + 25.0),
        egui::Align2::LEFT_TOP,
        format!("{}@{}", rec.auth.user, rec.connection.host),
        egui::FontId::proportional(11.0),
        // WCAG AA:fg_faint(#565b70) on panel_bg(#14161f) 只有 2.69:1,
        // fg_dimmer(#8a90a8) 是 5.71:1。不动 token 本身 —— 它在别处
        // (禁用态、装饰线)是对的。
        theme::c32(t.fg_dimmer),
    );
    resp
}

/// 「+ 新建」按钮的显式 id。原来用 `ui.button(...)`,egui 给它分配的是自动 id
/// (`self.next_auto_id_salt` 计数器,只保证同一次调用序列内跨帧稳定,外部测试
/// 代码算不出来,见 egui-0.30.0 `ui.rs::next_auto_id`/`allocate_space`)。复核 F90
/// 缺陷时发现:守护测试靠反查渲染出的「+ 新建」文字锚点来判定按钮是否被挤出
/// 屏幕,但文字锚点只在按钮矮(默认字号)时约等于按钮矩形——按钮被撑高后
/// (比如放大界面缩放/无障碍字号),锚点还停在按钮内容区顶部附近,真正该测的
/// 按钮底边早就跑到远处去了。实测过一次假阳性:按钮真实矩形底边 694,文字
/// 锚点只有 637,屏幕高 680——14px 的真实溢出被锚点判定当成了通过。
///
/// 跟 `toolbar.rs::button_id` 同一个理由、同一种做法:挂一个不依赖任何父 `Ui`
/// id 栈的显式全局 id,测试侧用 `Context::read_response` 直接读回真实
/// `Response::rect` 来判定。这个按钮全程序只出现一次,不会跟别处撞 id。
pub(crate) fn new_button_id() -> egui::Id {
    egui::Id::new("mullion_sm_new_button")
}

/// 手绘「+ 新建」按钮,挂 `new_button_id()`(为什么不能再用 `ui.button()` 的
/// 自动 id,见上方注释)。背景色/描边直接取 `ui.style().interact(&resp)`——跟
/// `egui::Button::ui()` 内部算 `frame_fill`/`frame_stroke` 用的是同一套视觉规则
/// (见 egui-0.30.0 `widgets/button.rs`),所以外观跟默认按钮基本一致。
///
/// `ui.allocate_space` 只预留布局空间、不注册交互(不像 `allocate_exact_size`
/// 会顺带用自动 id 注册一次 `Sense::hover` 部件)——跟 `toolbar.rs::show_in` 里
/// 「先 `allocate_space` 占位,再逐个用显式 id `interact`」是同一个套路,避免
/// 同一块矩形被注册成两个互相打架的部件。
fn new_button(ui: &mut Ui) -> egui::Response {
    let galley = egui::WidgetText::from("+ 新建").into_galley(
        ui,
        None,
        ui.available_width(),
        egui::TextStyle::Button,
    );
    let padding = ui.spacing().button_padding;
    let size =
        (galley.size() + padding * 2.0).at_least(egui::vec2(0.0, ui.spacing().interact_size.y));
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

pub(super) fn show(
    ui: &mut Ui,
    t: &Theme,
    ui_state: &mut UiState,
    sessions: &[SessionRecord],
    groups: &[GroupRecord],
    connected: Option<SessionId>,
) {
    // 搜索框
    ui.add(
        egui::TextEdit::singleline(&mut ui_state.search)
            .hint_text("搜索名称 / 主机 / 标签")
            .desired_width(f32::INFINITY),
    );
    ui.add_space(8.0);

    // 待确认删除的目标一旦这一帧没被真正渲染出来 —— 原因可能是搜索词把它
    // 滤掉、所在分组被手动折叠(`CollapsingHeader` 折叠时根本不会执行 body
    // 闭包,见 egui-0.30.0 `collapsing_header.rs:199-205` 的 `openness <= 0.0`
    // 分支)、会话本身已经不存在,或者将来任何新增的隐藏方式 —— 就必须清空
    // `pending_delete`。不清的话:确认框跟着那一行一起从视觉上消失,但状态
    // 还在原地;用户清空搜索词、重新展开分组、或者关闭再打开会话管理器,
    // 确认框会带着上次的意图凭空重新出现,用户可能在不知情的情况下点到
    // 「删除」——这正好抵消了做二次确认的初衷。
    //
    // 用「渲染前捕获的旧值」而不是逐个原因特判:`pending_delete_target` 在
    // 循环开始前就固定下来,`row()` 只在 `rec.id == pending_delete_target`
    // 时才把 `pending_delete_rendered` 置位。这样如果本帧内某一行刚被右键
    // 新设了 `pending_delete`(新值不等于循环前捕获的旧 `pending_delete_target`),
    // 不会被误当成「目标已渲染」,于是不会在同一帧里被下面的清空逻辑立刻抹掉。
    let pending_delete_target = ui_state.pending_delete;
    let mut pending_delete_rendered = false;

    // 底部「分隔线 + 新建按钮」用 `TopBottomPanel::bottom` 先占位:egui 的面板
    // 布局保证面板先分配自己的高度,再把外层 `ui` 的可用区底边收缩到面板上沿
    // (见 egui-0.30.0 `containers/panel.rs::show_inside` 里 `TopBottomSide::Bottom`
    // 分支的 `cursor.max.y = rect.min.y`),下面的 `ScrollArea` 就能吃到真实剩余
    // 高度——不再需要手算一个「必须跟底部实际渲染内容同步」的魔法数字
    // (原 `BOTTOM_BAR_H`,已删除,这正是那条注释警告过的坑)。
    // **必须在 `ScrollArea` 之前调用**,顺序颠倒的话可用区收缩不会对后面的
    // 部件生效。
    //
    // `.frame(Frame::none())`:面板默认背景取 `style.visuals.panel_fill`,跟外层
    // `SidePanel`(`mod.rs` 里用主题色 `t.panel_bg` 铺底)对不上,不清空会在底部
    // 露出一条颜色不一致的色带。`.show_separator_line(false)`:面板自带分隔线,
    // 不关掉会跟手绘的 `ui.separator()` 叠成两条线。
    egui::TopBottomPanel::bottom(ui.id().with("sm_list_bottom"))
        .frame(egui::Frame::none())
        .show_separator_line(false)
        .show_inside(ui, |ui| {
            ui.separator();
            ui.horizontal(|ui| {
                if new_button(ui).clicked() {
                    ui_state.pending_switch = Some(SwitchTarget::NewDraft);
                }
            });
        });

    let searching = !ui_state.search.trim().is_empty();

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // 用 `group_manager::group_sessions` 归桶(而不是自己按 group_id 手动
            // 分组):它已经处理了「分组被删后会话落进未分组桶而不是消失」这条
            // 既有保证(见 `group_manager::tests::session_with_dangling_group_id_falls_into_ungrouped_not_dropped`),
            // 自己重写一遍分组逻辑会悄悄丢掉这条回归保护。
            for (gid, bucket) in crate::ui::group_manager::group_sessions(groups, sessions) {
                let members: Vec<&SessionRecord> = bucket
                    .into_iter()
                    .filter(|r| matches(r, &ui_state.search))
                    .collect();
                if members.is_empty() && searching {
                    continue; // 搜索时不显示空分组
                }
                let title = match gid {
                    Some(id) => groups
                        .iter()
                        .find(|g| g.id == id)
                        .map(|g| g.name.clone())
                        .unwrap_or_else(|| "未分组".to_string()),
                    None => "未分组".to_string(),
                };
                // 搜索期间强制展开:`default_open` 只在 CollapsingState 首次
                // 加载时生效,用户手动折叠过就被持久化进 ctx.data(),再也展不开。
                let force = if searching { Some(true) } else { None };
                group_header(&title, gid, members.len())
                    .open(force)
                    .show(ui, |ui| {
                        for r in &members {
                            row(
                                ui,
                                t,
                                ui_state,
                                r,
                                connected,
                                pending_delete_target,
                                &mut pending_delete_rendered,
                            );
                        }
                    });
            }
        });

    // 多加一层「当前值仍等于帧初捕获的旧值」才清空:没有这层的话,
    // 「旧目标 X 这一帧没渲染」+「同一帧内另一行 Z 被右键新设了
    // `pending_delete`」这两件事一旦同时发生,会把 Z 刚写下的新值一并
    // 当成「目标未渲染」误删——即使当下的调用路径(右键菜单一次只能设
    // 一个目标、且设置发生在渲染期间而非渲染后)让这个组合在今天走不到,
    // 这个不变量也不该靠「时序上凑不出来」去担保:后续任务要改右栏和
    // 切换确认接线,谁也不能保证不会在渲染循环中间插入新的赋值点。加一次
    // `Option<SessionId>` 比较,把「只清我这一帧开始时看到的那个目标」变成
    // 结构上精确的不变量,不再依赖任何时序论证。
    if pending_delete_target.is_some()
        && !pending_delete_rendered
        && ui_state.pending_delete == pending_delete_target
    {
        ui_state.pending_delete = None;
    }
}

/// 画一行 + 挂交互(单击选中 / 双击连接 / 右键删除确认)。
///
/// `pending_delete_target` / `pending_delete_rendered`:「这一帧是否真的渲染过
/// 待确认删除的目标行」的事后判定标志,见调用侧 `show()` 里的说明——
/// 只在 `rec.id == pending_delete_target` 时置位,不直接读 `ui_state.pending_delete`,
/// 避免本帧内刚发生的新右键覆盖被误当成「旧目标已渲染」。
fn row(
    ui: &mut Ui,
    t: &Theme,
    ui_state: &mut UiState,
    rec: &SessionRecord,
    connected: Option<SessionId>,
    pending_delete_target: Option<SessionId>,
    pending_delete_rendered: &mut bool,
) {
    if pending_delete_target == Some(rec.id) {
        *pending_delete_rendered = true;
    }

    let selected = ui_state.editor_id == Some(rec.id);
    let resp = session_row(ui, t, rec, selected, connected == Some(rec.id));
    if resp.clicked() {
        ui_state.pending_switch = Some(SwitchTarget::Session(rec.id));
    }
    // egui 的点击检测在双击时也会让 `clicked()` 为 true(实现见
    // egui-0.30.0 `response.rs:138-145` 配合 `context.rs:1306-1308` 的点击
    // 计数逻辑,并非某条可引用的文档原文),所以双击这一行会在同一帧里把
    // `pending_switch` 和 `connect_request` 都写下。目前无害(`pending_switch`
    // 还没有消费点),但 Task 14 接脏检查确认时,需要决定 `connect_request`
    // 是否也要走那道确认门。
    if resp.double_clicked() {
        ui_state.connect_request = Some(rec.id);
    }
    resp.context_menu(|ui| {
        if ui.button("删除").clicked() {
            ui_state.pending_delete = Some(rec.id);
            ui.close_menu();
        }
    });

    // §4.3:删除确认内联展开在被删那一行下面,不再弹第三个窗口。
    if ui_state.pending_delete == Some(rec.id) {
        egui::Frame::none()
            .fill(theme::c32(t.sunken_bg))
            .inner_margin(8.0)
            .rounding(6.0)
            .show(ui, |ui| {
                ui.colored_label(
                    theme::c32(t.danger_soft),
                    format!("删除「{}」?", rec.identity.name),
                );
                ui.horizontal(|ui| {
                    if ui.button("删除").clicked() {
                        ui_state.delete_request = Some(rec.id);
                        ui_state.pending_delete = None;
                    }
                    if ui.button("取消").clicked() {
                        ui_state.pending_delete = None;
                    }
                });
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_store::model::{Auth, AuthKind, Connection, Identity, Protocol, SessionRecord};
    use mullion_store::SessionId;

    fn rec(id: u64, name: &str, host: &str, tags: &[&str]) -> SessionRecord {
        SessionRecord {
            id: SessionId(id),
            modified_at: "2026-08-03T00:00:00Z".into(),
            identity: Identity {
                name: name.into(),
                note: String::new(),
                group_id: None,
                tags: tags.iter().map(|s| s.to_string()).collect(),
            },
            connection: Connection {
                host: host.into(),
                port: 22,
                protocol: Protocol::Ssh,
            },
            auth: Auth {
                user: "user".into(),
                kind: AuthKind::Password,
            },
            terminal: Default::default(),
            appearance: Default::default(),
            network: Default::default(),
            automation: Default::default(),
        }
    }

    /// 搜索要覆盖名称 / 主机 / 标签三处,且大小写不敏感 —— 用户记得住的往往是
    /// IP 尾数或标签,不是当初起的名字。
    #[test]
    fn search_matches_name_host_and_tags_case_insensitively() {
        let r = rec(1, "Prod-DB", "192.0.2.10", &["生产", "MySQL"]);
        assert!(matches(&r, ""), "空查询放行全部");
        assert!(matches(&r, "  "), "只有空白的查询等同空查询");
        assert!(matches(&r, "prod"), "名称匹配应大小写不敏感");
        assert!(matches(&r, "2.10"), "主机子串应匹配");
        assert!(matches(&r, "mysql"), "标签匹配应大小写不敏感");
        assert!(!matches(&r, "staging"), "无关词不该匹配");
    }

    /// 复核坑:待确认删除的会话被搜索过滤掉后,`pending_delete` 必须清空。
    /// 不清的话,那一行连同确认框一起从视觉上消失,但状态还在原地——用户
    /// 清空搜索词、或关闭再打开会话管理器,确认框会带着上次的意图凭空重新
    /// 出现,用户可能在不知情下点到「删除」,抵消了二次确认的意义。
    ///
    /// 自证会变红:把 `show()` 里 `pending_delete_target.is_some() && !pending_delete_rendered`
    /// 这段清空逻辑注释掉,这条立刻报 `pending_delete` 仍是 `Some(SessionId(1))`。
    #[test]
    fn pending_delete_is_cleared_when_the_session_is_filtered_out_by_search() {
        let t = crate::theme::MULLION_DARK;
        let sessions = vec![rec(1, "dev-box", "192.0.2.10", &[])];
        let groups: Vec<GroupRecord> = Vec::new();
        let mut ui_state = UiState {
            pending_delete: Some(SessionId(1)),
            search: "no-match-at-all".into(),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                show(ui, &t, &mut ui_state, &sessions, &groups, None);
            });
        });
        assert_eq!(
            ui_state.pending_delete, None,
            "该会话被搜索过滤掉后,待确认删除状态必须清空,否则确认框会在\
             搜索词清空或重新打开窗口时凭空复现"
        );
    }

    /// 复核坑:待确认删除的会话所在分组被**手动折叠**时,`pending_delete` 也
    /// 必须清空——不是靠搜索过滤,而是 `CollapsingHeader` 折叠时根本不执行
    /// body 闭包(`openness <= 0.0` 直接返回,见 egui-0.30.0
    /// `collapsing_header.rs:199-205`),`row()` 从未被调用。上一轮「查
    /// sessions 里是否存在 + 是否命中搜索」的清空逻辑对这条路径完全无效:
    /// 分组折叠根本不经过搜索过滤,`matches()` 仍然为真,`pending_delete`
    /// 就会原地悬空,直到用户重新展开分组时凭空复现确认框。
    ///
    /// 用真实的 `CollapsingState::load_with_default_open`、`set_open(false)`
    /// 和 `store` 在渲染前把「未分组」桶的持久化折叠状态落地成「已折叠」——
    /// 这跟用户之前手动点过一次折叠按钮落地的状态完全一样,不是绕过注入点
    /// 的假测试。id 按生产代码里的真实推导链手工算出,一共叠了两层
    /// `Id::from("child")`(不是一层——曾经因为漏算这一层,用错的 id 导致
    /// 这条测试第一次跑起来是绿的假阳性,靠打印 `.value()` 全量 64 位跟生产
    /// 代码实际用的 id 逐层比对,才挖出这第二层):
    /// 1. `ScrollArea` 的 `content_ui`:id_salt 缺省是 `Id::from("child")`
    ///    (见 egui `ui.rs:265` `new_child` + `ui.rs:592-596` `ScrollArea::begin`
    ///    没有显式设置 `.id_salt()`);
    /// 2. `CollapsingHeader::show` 内部把 header + body 包了一层
    ///    `ui.vertical(|ui| { self.begin(ui) .. })`(见 `collapsing_header.rs:639-648`
    ///    的 `show_dyn`),`Ui::vertical` 同样没设 id_salt(见 `ui.rs:2519-2524`),
    ///    也缺省成 `Id::from("child")`;
    /// 3. 最后叠 `group_header` 里 `.id_salt(gid)` 对应的 `Id::new(gid)`
    ///    (`ui.make_persistent_id` = `self.id.with(&id_salt)`,见 `ui.rs:1022-1027`)。
    ///
    /// 自证会变红:把 `show()` 里 `pending_delete_target.is_some() && !pending_delete_rendered`
    /// 这段清空逻辑注释掉,这条立刻报 `pending_delete` 仍是 `Some(SessionId(1))`。
    #[test]
    fn pending_delete_is_cleared_when_the_group_is_manually_collapsed() {
        let t = crate::theme::MULLION_DARK;
        let sessions = vec![rec(1, "dev-box", "192.0.2.10", &[])];
        let groups: Vec<GroupRecord> = Vec::new();
        let mut ui_state = UiState {
            pending_delete: Some(SessionId(1)),
            search: String::new(),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                // 把「未分组」桶(gid = None)的折叠状态,用跟生产代码完全
                // 一致的 id 推导链(见上方文档注释的三步),提前落地成「已折叠」。
                let header_id = ui
                    .id()
                    .with(egui::Id::from("child")) // ScrollArea content_ui
                    .with(egui::Id::from("child")) // CollapsingHeader 内部的 ui.vertical()
                    .with(egui::Id::new(None::<mullion_store::GroupId>));
                let mut state =
                    egui::containers::collapsing_header::CollapsingState::load_with_default_open(
                        ui.ctx(),
                        header_id,
                        true,
                    );
                state.set_open(false);
                state.store(ui.ctx());

                show(ui, &t, &mut ui_state, &sessions, &groups, None);
            });
        });
        assert_eq!(
            ui_state.pending_delete, None,
            "分组被手动折叠后那一行根本没有渲染,待确认删除状态必须清空,\
             否则用户重新展开分组时确认框会带着上次意图凭空复现"
        );
    }

    /// 在渲染出的 `FullOutput.shapes` 里找一段文本第一次出现的锚点(`TextShape::pos`)。
    /// 跟 `ui/mod.rs` 里 `rendered_text` helper 一样,是这个项目验证「真按坐标点下去」
    /// 时的既有手法(见 `build_ui_clicking_a_preset_button_wires_through_to_actions_f82`),
    /// 不是猜像素——`session_row`/`row()` 都是手绘的,没有 label 能挂 id,只能反过来
    /// 从已经画出来的文本反推矩形。
    fn find_text_pos(shapes: &[egui::epaint::ClippedShape], needle: &str) -> Option<egui::Pos2> {
        fn walk(shape: &egui::Shape, needle: &str) -> Option<egui::Pos2> {
            match shape {
                egui::Shape::Vec(v) => v.iter().find_map(|s| walk(s, needle)),
                egui::Shape::Text(t) if t.galley.job.text.contains(needle) => Some(t.pos),
                _ => None,
            }
        }
        shapes.iter().find_map(|cs| walk(&cs.shape, needle))
    }

    /// 复核明确要求验证的「同帧竞态」:`show()` 在渲染前把 `ui_state.pending_delete`
    /// 复制成局部变量 `pending_delete_target`(`Option<SessionId>` 是 `Copy`,复制后
    /// 这份局部值不可能再被后续任何赋值影响到)。如果右键删除确认恰好在**这一帧**
    /// 里真正落地(点了菜单里的「删除」),新值必然不等于循环前复制的旧
    /// `pending_delete_target`,所以帧尾那段「未渲染就清空」的逻辑
    /// (只在 `pending_delete_target.is_some() && !pending_delete_rendered` 时才清)
    /// 不会把这一帧刚写下的新值当场抹掉。
    ///
    /// 用真实指针事件驱动(右键在会话行上打开菜单 → 下一帧点菜单里的「删除」
    /// 按钮),不是直接手动赋值 `ui_state.pending_delete` 后调 `show()`——那样测的是
    /// 「值已经在那儿」的稳态,证不了「这一帧刚写入」的竞态时序。菜单按钮的矩形
    /// 通过扫描 `FullOutput.shapes` 里已经画出来的「删除」文字反推(`find_text_pos`),
    /// 不是猜像素坐标。
    #[test]
    fn pending_delete_set_this_frame_by_context_menu_is_not_erased_in_the_same_frame() {
        let t = crate::theme::MULLION_DARK;
        let sessions = vec![
            rec(1, "session-a-unique-name", "192.0.2.10", &[]),
            rec(2, "session-b-unique-name", "192.0.2.20", &[]),
        ];
        let groups: Vec<GroupRecord> = Vec::new();
        let mut ui_state = UiState::default();
        let ctx = egui::Context::default();

        let run = |ctx: &egui::Context, ui_state: &mut UiState, input: egui::RawInput| {
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(ui, &t, ui_state, &sessions, &groups, None);
                });
            })
        };

        // 前两帧只是让布局稳定下来,不带任何指针事件。
        let _ = run(&ctx, &mut ui_state, egui::RawInput::default());
        let out = run(&ctx, &mut ui_state, egui::RawInput::default());

        let row_pos = find_text_pos(&out.shapes, "session-a-unique-name")
            .expect("session-a 这一行应该已经画出来了");
        // `session_row` 画名字时用的锚点是 `rect.left()+30, rect.top()+7`
        // (见本文件顶部 `session_row` 的 `p.text` 调用),反推回行内一个安全点。
        let row_click_pos = egui::pos2(row_pos.x - 20.0, row_pos.y + 15.0);

        let secondary_click = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let _ = run(
            &ctx,
            &mut ui_state,
            egui::RawInput {
                events: vec![
                    egui::Event::PointerMoved(row_click_pos),
                    secondary_click(row_click_pos, true),
                    secondary_click(row_click_pos, false),
                ],
                ..Default::default()
            },
        );
        // 右键这一帧不该直接写 `pending_delete`——菜单里的「删除」按钮还没被点。
        assert_eq!(
            ui_state.pending_delete, None,
            "右键只应该打开菜单,不应该在同一次点击里就直接确认删除"
        );

        // 菜单弹出用的是 `Area`,跟 `rendered_text` 文档注释里说的一样:
        // 首次遇到某个 id 时先做一趟不可见的 sizing pass,真正把内容画出来
        // 要等下一帧——所以这里再空跑一帧(不带任何指针事件)才能看到「删除」。
        let out = run(&ctx, &mut ui_state, egui::RawInput::default());
        let delete_btn_pos =
            find_text_pos(&out.shapes, "删除").expect("右键打开的菜单里应该画出了「删除」按钮");
        let primary_click = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let _ = run(
            &ctx,
            &mut ui_state,
            egui::RawInput {
                events: vec![
                    egui::Event::PointerMoved(delete_btn_pos),
                    primary_click(delete_btn_pos, true),
                    primary_click(delete_btn_pos, false),
                ],
                ..Default::default()
            },
        );

        assert_eq!(
            ui_state.pending_delete,
            Some(SessionId(1)),
            "点「删除」这一帧刚写下的 pending_delete 不该被同一帧末尾的清空逻辑\
             当场抹掉——它是本帧渲染前复制的旧值(None)对比出来的,不该影响\
             本帧新写入的值"
        );
    }

    /// 复核指出的边界洞:「旧目标 X 这一帧没渲染」+「同一帧内另一行 Z 被
    /// 右键新设了 `pending_delete`」同时发生时,不能把 Z 刚写下的新值当成
    /// 「目标未渲染」误删。构造方法:搜索词从头到尾固定成只命中 session-a,
    /// 让 X(session-b)自始至终都不被渲染(避免菜单弹窗跟着行位置重新计算——
    /// 我第一版让搜索词中途变化,结果菜单挪了位置,用旧坐标点击落空,那是
    /// 另一个问题,不是这里要测的东西);先用真实右键在 session-a(Z)上打开
    /// 删除确认菜单,再直接把 `ui_state.pending_delete` 预置成
    /// `Some(session-b)`(相当于「进这一帧之前,待确认删除的目标就已经是
    /// 那个此刻并不渲染的 X」,是搭建前置状态,不是绕过注入点),最后在
    /// **同一帧**里真的点掉 session-a 菜单里的「删除」——`ui_state.pending_delete`
    /// 会在这一帧的渲染过程中被改写成 `Some(session-a)`。这样帧尾清空逻辑
    /// 看到的就是「target=X 这一帧没渲染」+「当前值已经不是 X」同时成立,
    /// 真实复现了这条边界。
    ///
    /// 自证会变红:把 `show()` 里新加的
    /// `ui_state.pending_delete == pending_delete_target` 这层判定去掉(退回
    /// 只看 `is_some() && !rendered` 就清空),这条立刻报 `pending_delete`
    /// 变成了 `None`,而不是 `Some(SessionId(1))`——Z 刚写下的新值被 X 的
    /// 清空逻辑误删了。
    #[test]
    fn pending_delete_newly_set_this_frame_is_not_erased_by_a_different_stale_target_hiding() {
        let t = crate::theme::MULLION_DARK;
        let sessions = vec![
            rec(1, "session-a-unique-name", "192.0.2.10", &[]),
            rec(2, "session-b-unique-name", "192.0.2.20", &[]),
        ];
        let groups: Vec<GroupRecord> = Vec::new();
        // 搜索词固定只命中 session-a:X(session-b)整场测试都不会被渲染,
        // 布局不会因为搜索词变化而中途改变,菜单弹窗的位置全程稳定。
        let mut ui_state = UiState {
            search: "session-a".into(),
            ..Default::default()
        };
        let ctx = egui::Context::default();

        let run = |ctx: &egui::Context, ui_state: &mut UiState, input: egui::RawInput| {
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(ui, &t, ui_state, &sessions, &groups, None);
                });
            })
        };

        // 前两帧只是让布局稳定下来。
        let _ = run(&ctx, &mut ui_state, egui::RawInput::default());
        let out = run(&ctx, &mut ui_state, egui::RawInput::default());

        // 在 session-a(Z)上右键,打开它的删除确认菜单。
        let row_pos = find_text_pos(&out.shapes, "session-a-unique-name")
            .expect("session-a 这一行应该已经画出来了");
        let row_click_pos = egui::pos2(row_pos.x - 20.0, row_pos.y + 15.0);
        let secondary_click = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let _ = run(
            &ctx,
            &mut ui_state,
            egui::RawInput {
                events: vec![
                    egui::Event::PointerMoved(row_click_pos),
                    secondary_click(row_click_pos, true),
                    secondary_click(row_click_pos, false),
                ],
                ..Default::default()
            },
        );
        // 菜单要多等一帧才画出「删除」文字(Area 首次出现有一趟不可见的
        // sizing pass,见前一条测试同样的说明)。
        let out = run(&ctx, &mut ui_state, egui::RawInput::default());
        let delete_btn_pos =
            find_text_pos(&out.shapes, "删除").expect("右键打开的菜单里应该画出了「删除」按钮");

        // 搭建前置状态:进关键帧之前,待确认删除的目标是 X(session-b)——
        // 它整场都没被渲染过,`pending_delete_target` 这一帧会捕获到这个值。
        ui_state.pending_delete = Some(SessionId(2));

        // 关键帧:X 这一帧仍不渲染(搜索词没变),同时真的点掉 session-a
        // 菜单里的「删除」——两件事在这一帧里同时成立。
        let primary_click = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let _ = run(
            &ctx,
            &mut ui_state,
            egui::RawInput {
                events: vec![
                    egui::Event::PointerMoved(delete_btn_pos),
                    primary_click(delete_btn_pos, true),
                    primary_click(delete_btn_pos, false),
                ],
                ..Default::default()
            },
        );

        assert_eq!(
            ui_state.pending_delete,
            Some(SessionId(1)),
            "session-b(X)这一帧没有渲染、同时 session-a(Z)在这同一帧被右键\
             确认删除——Z 刚写下的新值不该被『X 未渲染』的清空逻辑误删"
        );
    }
}
