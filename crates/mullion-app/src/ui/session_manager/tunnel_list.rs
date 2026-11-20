//! 隧道模式的左栏(F116)。
//!
//! 与会话列表**不共用**渲染:隧道没有分组、没有图标、条数少,套三档密度那套
//! 只会把「一行说清一条转发规则」这件事复杂化(设计 D11)。

use egui::Ui;
use mullion_ssh::tunnel::TunnelState;
use mullion_store::{SessionId, SessionRecord, TunnelId, TunnelKind, TunnelRecord};

use crate::theme::{self, Theme};
use crate::ui::annotate;

use super::UiState;

/// 「+ 新建隧道」按钮的显式 id。理由同 `list::new_button_id()` ——
/// 守护测试要用 `Context::read_response` 读回真实矩形,egui 的自动 id
/// 外部算不出来。
pub(crate) fn new_button_id() -> egui::Id {
    egui::Id::new("mullion_tunnel_new_button")
}

/// 某条隧道那一行「启动」按钮的显式 id。同上,测试要靠它读回 enabled 状态。
pub(crate) fn start_button_id(id: TunnelId) -> egui::Id {
    egui::Id::new(("mullion_tunnel_start", id.0))
}

/// 转发串:`本地 3306 → db.internal:3306`。**不含名称**。
///
/// 方向箭头照着**数据流向**写,不照配置字段顺序 —— 这个功能最常见的错误
/// 就是把两端填反,标题里看不出方向等于把排查成本推到运行时。
pub(crate) fn forward_summary(rec: &TunnelRecord) -> String {
    match &rec.kind {
        TunnelKind::Local {
            target_host,
            target_port,
            ..
        } => format!("本地 {} → {}:{}", rec.listen_port, target_host, target_port),
        TunnelKind::Remote {
            target_host,
            target_port,
            ..
        } => format!("远端 {} → {}:{}", rec.listen_port, target_host, target_port),
        TunnelKind::Dynamic => format!("动态 {} (SOCKS5)", rec.listen_port),
    }
}

/// F268:一行的主标题。起了名就用名字,没起名退回转发串。
///
/// 退回的是**转发串本身**而不是「未命名隧道」之类的占位:没起名的隧道恰恰
/// 是靠「本地 3306 → db:3306」认出来的,拿一句占位把它盖掉,等于让所有
/// 没起名的行长得一模一样。
pub(crate) fn row_title(rec: &TunnelRecord) -> String {
    let name = rec.name.trim();
    if name.is_empty() {
        forward_summary(rec)
    } else {
        name.to_string()
    }
}

/// F385:列表里的转发串。与 `forward_summary` 同序,只是**本机监听端口前加 `:`**——
/// 名称占了第一行之后,第二行一眼要抓的是端口,`:3306` 比裸 `3306` 更像端口。
///
/// 只用在列表里;状态栏浮层、删除确认框等仍走 `forward_summary`(不扩大改动面)。
pub(crate) fn forward_line(rec: &TunnelRecord) -> String {
    match &rec.kind {
        TunnelKind::Local {
            target_host,
            target_port,
            ..
        } => format!(
            "本地 :{} → {}:{}",
            rec.listen_port, target_host, target_port
        ),
        TunnelKind::Remote {
            target_host,
            target_port,
            ..
        } => format!(
            "远端 :{} → {}:{}",
            rec.listen_port, target_host, target_port
        ),
        TunnelKind::Dynamic => format!("动态 :{} (SOCKS5)", rec.listen_port),
    }
}

/// F385:列表第一行的标题。起了名用名字,没起名退回(带 `:` 的)转发串 ——
/// 理由同 `row_title`。
fn list_title(rec: &TunnelRecord) -> String {
    if rec.name.trim().is_empty() {
        forward_line(rec)
    } else {
        rec.name.trim().to_string()
    }
}

/// F385:列表第二行。只有起了名的才有 —— 没起名时标题本身就是转发串,
/// 再画一遍等于同一句话占两行。
///
/// F268:转发串是判断「这条是不是我要找的那条」的关键信息,不能因为起了名就看不见。
fn forward_row(rec: &TunnelRecord) -> Option<String> {
    (!rec.name.trim().is_empty()).then(|| forward_line(rec))
}

/// 列表第三行:引用的会话。悬垂时说清楚是**引用**坏了,不是隧道配错了。
fn via_line(
    rec: &TunnelRecord,
    sessions: &[SessionRecord],
    credentials: &[mullion_store::CredentialRecord],
) -> (String, bool) {
    match sessions.iter().find(|s| s.id == rec.session_id) {
        Some(s) => (
            format!(
                "经 {} ({}@{})",
                s.identity.name,
                mullion_store::display_user(&s.auth, credentials),
                s.connection.host
            ),
            false,
        ),
        None => ("引用的会话已删除".to_string(), true),
    }
}

/// 稳定顺序:按 `TunnelId` 升序。存储顺序会随增删漂移,直接拿来渲染会让
/// 列表在用户眼皮底下重排。
pub(crate) fn visible_order(tunnels: &[TunnelRecord]) -> Vec<TunnelId> {
    let mut ids: Vec<TunnelId> = tunnels.iter().map(|t| t.id).collect();
    ids.sort();
    ids
}

/// 一行右侧那个按钮该是什么样。**纯函数** —— 「什么时候能点」这件事必须能
/// 脱离 egui 单测:egui 里读回 `enabled` 要跑够帧数、还要在 `ctx.run` 内部读
/// (见 mod.rs 那条守护测试的长注释),不适合穷举所有档位。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ButtonSpec {
    pub label: &'static str,
    pub enabled: bool,
    /// 禁用时的悬停解释。禁用而不说明原因,用户只会反复点。
    pub disabled_hint: Option<&'static str>,
}

/// `live` = 这条隧道在运行时表里(已启动)。
///
/// **不看转发类型**:三种都实现了(F111/F112/F113)。T-b 时这里有一档
/// 「`-R`/`-D` 还没做」的禁用,T-c 落地后删掉了 —— 留着就成了一个永远
/// 说着谎的禁用理由。
///
/// F338:`elsewhere` = 另一个窗口(实例)里正跑着(或正在建链)这条隧道。本实例
/// 再启动会去抢同一个端口,所以禁用并说明原因。**排在 `live` 之后**:本实例自己
/// 跑着的,永远停得掉。
pub(crate) fn button_spec(dangling: bool, live: bool, elsewhere: bool) -> ButtonSpec {
    if live {
        return ButtonSpec {
            label: "停止",
            enabled: true,
            disabled_hint: None,
        };
    }
    if dangling {
        return ButtonSpec {
            label: "启动",
            enabled: false,
            disabled_hint: Some("引用的会话已删除 —— 先把这条隧道改到一个还在的会话上"),
        };
    }
    if elsewhere {
        return ButtonSpec {
            label: "启动",
            enabled: false,
            disabled_hint: Some("这条隧道正在另一个窗口运行,请到那个窗口停止"),
        };
    }
    ButtonSpec {
        label: "启动",
        enabled: true,
        disabled_hint: None,
    }
}

/// F369:一行的按钮,以及要显示的「别的窗口」状态。列表行与状态栏浮层**同源**,
/// 否则两处对「能不能点启动」各说各话。
///
/// 本窗口自己有运行态就以它为准(别的窗口的上报不再显示);没有才看别的窗口(F338)。
pub(crate) fn row_spec<'a>(
    dangling: bool,
    state: Option<&TunnelState>,
    other: Option<&'a mullion_store::OtherTunnel>,
) -> (ButtonSpec, Option<&'a mullion_store::OtherTunnel>) {
    let other = other.filter(|_| state.is_none());
    let spec = button_spec(
        dangling,
        state.is_some(),
        other.is_some_and(|o| o.state.blocks_start()),
    );
    (spec, other)
}

/// F338:这条隧道在别的实例里的状态。多个实例都报了时,**优先取会挡启动的那条**
/// (否则「一个实例里已失败、另一个实例里正跑着」会被显示成已停止,按钮却被挡)。
pub(crate) fn other_for(
    id: TunnelId,
    others: &[mullion_store::OtherTunnel],
) -> Option<&mullion_store::OtherTunnel> {
    let mut hits = others.iter().filter(|o| o.tunnel == id);
    let first = hits.next()?;
    if first.state.blocks_start() {
        return Some(first);
    }
    hits.find(|o| o.state.blocks_start()).or(Some(first))
}

/// F338:别的实例里这条隧道的一行状态文字。
pub(crate) fn other_label(o: &mullion_store::OtherTunnel) -> &'static str {
    use mullion_store::RemoteTunnelState as R;
    match o.state {
        R::Running => "在另一个窗口运行中",
        R::Connecting => "在另一个窗口连接中…",
        R::Reconnecting => "在另一个窗口重连中",
        R::Failed => "在另一个窗口已停止",
    }
}

/// F338:悬停提示。实例 id 对用户没意义,给 pid(任务管理器里对得上号)和启动时间。
pub(crate) fn other_hover(o: &mullion_store::OtherTunnel, now: i64) -> String {
    format!(
        "PID {},{}启动",
        o.pid,
        crate::ui::history::when_text(now, o.started_at)
    )
}

/// 本窗口运行态的状态文字。`None` = 没启动;列表里常驻的「未启动」由
/// `row_status` 兜底(F385),状态栏浮层仍只在有运行态时写一句。
pub(crate) fn state_label(state: Option<&TunnelState>) -> Option<String> {
    Some(match state? {
        TunnelState::Connecting => "连接中…".to_string(),
        TunnelState::Running => "运行中".to_string(),
        // 第几次、还有多久 —— 少了任何一半,用户都判断不出「是不是卡死了」。
        TunnelState::Reconnecting { attempt, retry_in } => {
            format!("重连中(第 {attempt} 次,{}s 后)", retry_in.as_secs())
        }
        TunnelState::Failed(msg) => format!("已停止:{msg}"),
        TunnelState::Stopped => "已停止".to_string(),
    })
}

/// F385:状态的语义档。圆点和文字共用这一档,颜色只在渲染处映射到主题。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tone {
    /// 没启动:灰色空心点。
    Idle,
    Ok,
    Warn,
    Danger,
}

/// F385:一行的状态(常驻)。本窗口有运行态以它为准,没有才看别的窗口(F338),
/// 都没有 = 「未启动」。
///
/// 颜色语义沿用 F114:「重连中」和「已停止」不能同色,否则需要人管的那条被藏起来。
pub(crate) fn row_status(
    state: Option<&TunnelState>,
    other: Option<&mullion_store::OtherTunnel>,
) -> (String, Tone) {
    if let Some(text) = state_label(state) {
        let tone = match state {
            Some(TunnelState::Running) => Tone::Ok,
            Some(TunnelState::Failed(_)) | Some(TunnelState::Stopped) => Tone::Danger,
            _ => Tone::Warn,
        };
        return (text, tone);
    }
    if let Some(o) = other {
        let tone = if o.state.blocks_start() {
            Tone::Warn
        } else {
            Tone::Danger
        };
        return (other_label(o).to_string(), tone);
    }
    ("未启动".to_string(), Tone::Idle)
}

/// 名称至少留这么宽才让状态文字留在第一行。名字本身更短就按名字实宽算。
const NAME_MIN_W: f32 = 64.0;

/// F385:状态文字能不能和名称、按钮挤在第一行。`fixed` = 圆点之后除名称和状态外
/// 必占的宽(按钮 + 间距)。放不下就挪到第三行末尾,第一行只剩圆点 —— **宁可挪
/// 状态,也不让名称被挤到看不出是哪条**。
pub(crate) fn status_fits_first_row(avail: f32, fixed: f32, status_w: f32, name_w: f32) -> bool {
    avail - fixed - status_w >= name_w.min(NAME_MIN_W)
}

// F338 把参数从 7 顶到 8。同 `session_manager::show` 的取舍:这是内部叶子函数,
// 为压参数数引入一个专用结构体没有实际收益。
#[allow(clippy::too_many_arguments)]
pub(super) fn show(
    ui: &mut Ui,
    t: &Theme,
    ui_state: &mut UiState,
    tunnels: &[TunnelRecord],
    states: &[(TunnelId, TunnelState)],
    others: &[mullion_store::OtherTunnel],
    sessions: &[SessionRecord],
    credentials: &[mullion_store::CredentialRecord],
) {
    // 底部「+ 新建」先占位,理由同 `list::show` —— 面板先分配高度,
    // 下面的 `ScrollArea` 才吃得到真实剩余高度。
    egui::TopBottomPanel::bottom(ui.id().with("tunnel_list_bottom"))
        .frame(egui::Frame::none())
        .show_separator_line(false)
        .show_inside(ui, |ui| {
            ui.separator();
            ui.horizontal(|ui| {
                let b = new_button(ui);
                annotate::mark(ui.ctx(), "会话管理器/左栏/新建隧道按钮", b.rect);
                if b.clicked() {
                    // 新建:清空 id 并给一份空缓冲。基线同步设置,否则新表单
                    // 一打开就被判成脏。
                    ui_state.tunnel_editor_id = None;
                    let fresh = super::TunnelEditorBuffer::default();
                    ui_state.tunnel_editor_baseline = Some(fresh.clone());
                    ui_state.tunnel_editor = Some(fresh);
                }
            });
        });

    if tunnels.is_empty() {
        ui.add_space(crate::ui::metrics::SP_XS);
        ui.colored_label(theme::c32(t.fg_dimmer), "还没有隧道");
        ui.colored_label(
            theme::c32(t.fg_dimmer),
            "隧道把端口转发到会话所连的那台机器上。",
        );
        return;
    }

    let order = visible_order(tunnels);
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let mut first = true;
            for id in order {
                let Some(rec) = tunnels.iter().find(|x| x.id == id) else {
                    continue;
                };
                let state = states.iter().find(|(tid, _)| *tid == id).map(|(_, s)| s);
                let other = other_for(id, others);
                // F385:条目之间画分割线,否则三行一条的列表分不清哪行归哪条。
                if !first {
                    ui.separator();
                }
                first = false;
                row(ui, t, ui_state, rec, state, other, sessions, credentials);
            }
        });
}

/// F385:一条隧道 = 一块整体可点的区域,三行:
/// ① 状态圆点 + 名称 …… 状态文字 + 启停按钮;② 转发串(仅起了名的);③ 经哪个会话。
///
/// 整块用 `UiBuilder::sense` 收点击:它登记在块内控件**之下**,点到启停按钮仍归按钮。
/// 块内文字一律 `selectable(false)` —— 可划选的 `Label` 会吞掉点击,点在字上就选不中。
// F338 把参数从 7 顶到 8。同 `session_manager::show` 的取舍:这是内部叶子函数,
// 为压参数数引入一个专用结构体没有实际收益。
#[allow(clippy::too_many_arguments)]
fn row(
    ui: &mut Ui,
    t: &Theme,
    ui_state: &mut UiState,
    rec: &TunnelRecord,
    state: Option<&TunnelState>,
    other: Option<&mullion_store::OtherTunnel>,
    sessions: &[SessionRecord],
    credentials: &[mullion_store::CredentialRecord],
) {
    let selected = ui_state.tunnel_editor_id == Some(rec.id);
    let (via, dangling) = via_line(rec, sessions, credentials);
    // F338:本实例自己有运行态就以它为准;没有才看别的实例。
    let (spec, other) = row_spec(dangling, state, other);
    let (status, tone) = row_status(state, other);
    let tone_color = match tone {
        Tone::Idle => theme::c32(t.fg_dimmer),
        Tone::Ok => theme::c32(t.ok),
        Tone::Warn => theme::c32(t.warn),
        Tone::Danger => theme::c32(t.danger_text),
    };
    let status_label = |ui: &mut Ui| {
        let r = ui.add(
            egui::Label::new(egui::RichText::new(&status).color(tone_color)).selectable(false),
        );
        if let Some(o) = other {
            r.on_hover_text(other_hover(o, mullion_store::now_secs()));
        }
    };

    // 背景先占位,等整块画完、知道矩形和悬停态再填。
    let bg = ui.painter().add(egui::Shape::Noop);
    let block = ui.scope_builder(
        egui::UiBuilder::new()
            .id_salt(("mullion_tunnel_row", rec.id.0))
            .sense(egui::Sense::click()),
        |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            let gap = ui.spacing().item_spacing.x;
            let line_h = ui.text_style_height(&egui::TextStyle::Body);
            let mut status_below = false;
            ui.horizontal(|ui| {
                let (dot, _) =
                    ui.allocate_exact_size(egui::vec2(10.0, line_h), egui::Sense::hover());
                let c = dot.center();
                if tone == Tone::Idle {
                    ui.painter()
                        .circle_stroke(c, 3.5, egui::Stroke::new(1.2, tone_color));
                } else {
                    ui.painter().circle_filled(c, 4.0, tone_color);
                }

                let name_size = ui
                    .style()
                    .text_styles
                    .get(&egui::TextStyle::Body)
                    .map_or(14.0, |f| f.size)
                    + 1.0;
                let title = egui::RichText::new(list_title(rec))
                    .size(name_size)
                    .color(theme::c32(t.fg_strong));
                let width_of = |ui: &Ui, text: egui::WidgetText| {
                    text.into_galley(
                        ui,
                        Some(egui::TextWrapMode::Extend),
                        f32::INFINITY,
                        egui::TextStyle::Body,
                    )
                    .size()
                    .x
                };
                let name_w = width_of(ui, title.clone().into());
                let status_w = width_of(ui, status.as_str().into());
                let btn_w = width_of(ui, spec.label.into()) + ui.spacing().button_padding.x * 2.0;
                let fixed = btn_w + gap * 2.0;
                let fits =
                    status_fits_first_row(ui.available_width(), fixed, status_w + gap, name_w);
                status_below = !fits;
                let name_room =
                    (ui.available_width() - fixed - if fits { status_w + gap } else { 0.0 })
                        .max(0.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(name_room, line_h),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.set_max_width(name_room);
                        ui.add(egui::Label::new(title).truncate().selectable(false));
                    },
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    start_button(ui, ui_state, rec, spec, state);
                    if fits {
                        status_label(ui);
                    }
                });
            });
            if let Some(fwd) = forward_row(rec) {
                ui.add(
                    egui::Label::new(egui::RichText::new(fwd).color(theme::c32(t.fg)))
                        .truncate()
                        .selectable(false),
                );
            }
            ui.horizontal(|ui| {
                let via_color = if dangling {
                    theme::c32(t.danger_text)
                } else {
                    theme::c32(t.fg_dimmer)
                };
                let via_w = if status_below {
                    (ui.available_width() * 0.5).max(0.0)
                } else {
                    ui.available_width()
                };
                ui.allocate_ui_with_layout(
                    egui::vec2(via_w, line_h),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.set_max_width(via_w);
                        ui.add(
                            egui::Label::new(egui::RichText::new(&via).small().color(via_color))
                                .truncate()
                                .selectable(false),
                        );
                    },
                );
                if status_below {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        status_label(ui);
                    });
                }
            });
        },
    );
    let resp = block.response;
    // F268:克隆。整条复制成新的一条 —— 同一台机器上再开一条转发
    // (换个端口、换个目标)是最常见的动作,手工重填一遍类型/会话/
    // 目标又慢又容易把方向填反。**端口不自动挪**,见 `Vault::clone_tunnel`。
    resp.context_menu(|ui| {
        if ui.button("克隆").clicked() {
            ui_state.tunnel_clone_request = Some(rec.id);
            ui.close_menu();
        }
    });
    if resp.clicked() {
        ui_state.tunnel_editor_id = Some(rec.id);
        let buf = super::TunnelEditorBuffer::from_record(rec);
        ui_state.tunnel_editor_baseline = Some(buf.clone());
        ui_state.tunnel_editor = Some(buf);
    }
    let fill = if selected {
        Some(ui.visuals().selection.bg_fill)
    } else if resp.hovered() {
        Some(ui.visuals().widgets.hovered.weak_bg_fill)
    } else {
        None
    };
    if let Some(fill) = fill {
        ui.painter().set(
            bg,
            egui::Shape::rect_filled(resp.rect.expand(2.0), 3.0, fill),
        );
    }
}

/// 启动/停止:禁用必须是**真禁用**,不是画成灰的。`ui.disable()` 让
/// `interact` 返回 `enabled=false` 且 `clicked()` 恒假 —— 「看着灰」和
/// 「点不动」必须是同一件事,否则用户点了没反应会以为是隧道配错了。
/// 什么时候该禁用见纯函数 `button_spec`(它自己有穷举测试)。
///
/// 包一层 `scope`:`disable()` 只作用到按钮,别把同排的状态文字也画灰。
fn start_button(
    ui: &mut Ui,
    ui_state: &mut UiState,
    rec: &TunnelRecord,
    spec: ButtonSpec,
    state: Option<&TunnelState>,
) {
    ui.scope(|ui| {
        if !spec.enabled {
            ui.disable();
        }
        let galley = egui::WidgetText::from(spec.label).into_galley(
            ui,
            None,
            ui.available_width(),
            egui::TextStyle::Button,
        );
        let padding = ui.spacing().button_padding;
        let size = galley.size() + padding * 2.0;
        let (_auto, rect) = ui.allocate_space(size);
        let mut resp = ui.interact(rect, start_button_id(rec.id), egui::Sense::click());
        if let Some(hint) = spec.disabled_hint {
            resp = resp.on_disabled_hover_text(hint);
        }
        if resp.clicked() {
            // 只写意图:组 `SshConfig`、bind 端口、起监管任务都要 store /
            // tokio runtime,egui 闭包里一样都够不着(与 `connect_request`
            // 同构,见 `UiState` 上那两个字段的文档)。
            if state.is_some() {
                ui_state.tunnel_stop_request = Some(rec.id);
            } else {
                ui_state.tunnel_start_request = Some(rec.id);
            }
        }
        if ui.is_rect_visible(rect) {
            let visuals = ui.style().interact(&resp);
            ui.painter().rect(
                rect,
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
    });
}

/// 手绘「+ 新建」,挂显式 id。做法照抄 `list::new_button`(为什么不能用
/// `ui.button()` 的自动 id,见那里的长注释)。
fn new_button(ui: &mut Ui) -> egui::Response {
    let galley = egui::WidgetText::from("+ 新建隧道").into_galley(
        ui,
        None,
        ui.available_width(),
        egui::TextStyle::Button,
    );
    let padding = ui.spacing().button_padding;
    let size = galley.size() + padding * 2.0;
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

/// 删除会话的确认框里那句「这会影响哪些隧道」。
///
/// 超过 `CAP` 条只列前 `CAP` 条,并**显式说还有多少条** —— 截断而不说等于
/// 告诉用户「就这些」。
pub(crate) const AFFECTED_CAP: usize = 5;

pub(crate) fn affected_lines(id: SessionId, tunnels: &[TunnelRecord]) -> Vec<String> {
    let hit = mullion_store::tunnel::tunnels_referencing(id, tunnels);
    let mut out: Vec<String> = hit
        .iter()
        .take(AFFECTED_CAP)
        .map(|t| row_title(t))
        .collect();
    if hit.len() > AFFECTED_CAP {
        out.push(format!("另有 {} 条", hit.len() - AFFECTED_CAP));
    }
    out
}

/// 删这条会话时,引用它的隧道里有几条**正在跑**。
///
/// 「会失去引用」和「正在跑的会被停掉」是两件不同分量的事:前者是配置层面的
/// 悬垂(还能回头改),后者是**此刻**有本机端口要被关掉、上面的连接会断。
/// 确认框里不区分,用户按下删除时就不知道自己会打断什么。
pub(crate) fn running_note(
    id: SessionId,
    tunnels: &[TunnelRecord],
    states: &[(TunnelId, TunnelState)],
) -> Option<String> {
    let n = mullion_store::tunnel::tunnels_referencing(id, tunnels)
        .iter()
        .filter(|t| states.iter().any(|(sid, _)| *sid == t.id))
        .count();
    (n > 0).then(|| format!("其中 {n} 条正在运行 —— 删除会一并停掉,本机端口随之释放"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: u64, kind: TunnelKind) -> TunnelRecord {
        TunnelRecord {
            id: TunnelId(id),
            session_id: SessionId(7),
            listen_port: 3306,
            name: String::new(),
            note: String::new(),
            autostart: false,
            kind,
        }
    }

    fn local() -> TunnelKind {
        TunnelKind::Local {
            target_host: "db.internal".into(),
            target_port: 3306,
            expose: false,
        }
    }

    /// 标题里必须能看出**方向**。填反两端是这个功能最常见的错误,而症状
    /// (「连上了但连不通」)出现在远端,列表上看不出方向等于没有第一道防线。
    #[test]
    fn row_title_shows_the_direction_of_data_flow() {
        let l = row_title(&rec(1, local()));
        let r = row_title(&rec(
            2,
            TunnelKind::Remote {
                target_host: "127.0.0.1".into(),
                target_port: 3000,
                expose: false,
            },
        ));
        assert!(l.starts_with("本地"), "实际: {l}");
        assert!(r.starts_with("远端"), "实际: {r}");
        assert_ne!(l, r);
        let d = row_title(&rec(3, TunnelKind::Dynamic));
        assert!(d.contains("SOCKS5"), "动态转发要写明是 SOCKS5,实际: {d}");
        assert!(
            !d.contains('→'),
            "动态转发没有固定目标,不该画箭头,实际: {d}"
        );
    }

    #[test]
    fn visible_order_is_by_id_not_storage_order() {
        let list = vec![rec(9, local()), rec(2, local()), rec(5, local())];
        assert_eq!(
            visible_order(&list),
            vec![TunnelId(2), TunnelId(5), TunnelId(9)]
        );
    }

    /// 截断必须说出来。列 5 条就停、什么都不说,用户会以为影响面只有 5 条。
    #[test]
    fn affected_lines_are_capped_and_say_so() {
        let many: Vec<TunnelRecord> = (1..=8).map(|i| rec(i, local())).collect();
        let lines = affected_lines(SessionId(7), &many);
        assert_eq!(lines.len(), AFFECTED_CAP + 1);
        assert_eq!(lines.last().unwrap(), "另有 3 条");

        let few: Vec<TunnelRecord> = (1..=2).map(|i| rec(i, local())).collect();
        let lines = affected_lines(SessionId(7), &few);
        assert_eq!(lines.len(), 2, "没超过上限时不该多出一行「另有」");
        assert!(!lines.iter().any(|l| l.starts_with("另有")));
    }

    /// 「什么时候能点」的穷举。唯一还剩的禁用理由是**悬垂引用**,
    /// 而且必须说清是引用坏了,不是「暂不可用」—— 后者会让用户以为是
    /// 功能没做,于是不去改那条引用。
    #[test]
    fn button_spec_only_blocks_dangling_references_and_flips_to_stop_when_running() {
        let live = button_spec(false, true, false);
        assert_eq!(live.label, "停止");
        assert!(live.enabled, "跑着的隧道必须随时停得掉");

        let ok = button_spec(false, false, false);
        assert_eq!(ok.label, "启动");
        assert!(ok.enabled);
        assert!(ok.disabled_hint.is_none());

        let dangling = button_spec(true, false, false);
        assert!(!dangling.enabled, "引用的会话都没了,起不来");
        assert!(dangling.disabled_hint.is_some_and(|h| h.contains("已删除")));

        // 跑着的隧道即使引用悬垂也得能停 —— 端口还占着,不给停就成了
        // 「只能重启客户端才能关掉的通路」。
        assert!(button_spec(true, true, false).enabled);
    }

    /// F369:`row_spec` 是列表行与浮层共用的判据。本窗口有运行态就压过别的窗口
    /// 的上报(按钮是「停止」、不显示别处状态);没有才由别处的阻挡态禁用「启动」。
    #[test]
    fn row_spec_prefers_the_local_state_over_other_windows() {
        let o = mullion_store::OtherTunnel {
            tunnel: TunnelId(1),
            state: mullion_store::RemoteTunnelState::Running,
            pid: 1,
            started_at: 0,
            instance: String::new(),
        };
        let (spec, shown) = row_spec(false, Some(&TunnelState::Running), Some(&o));
        assert_eq!(spec.label, "停止");
        assert!(shown.is_none(), "本窗口有运行态就不该再显示别处状态");
        let (spec, shown) = row_spec(false, None, Some(&o));
        assert!(!spec.enabled, "别处在跑,本窗口不该能启动");
        assert!(shown.is_some());
    }

    /// 重连中必须同时说清**第几次**和**还有多久**。少任何一半,用户都判断
    /// 不出「是在退避还是已经卡死」,而这正是重连唯一需要被看见的信息。
    #[test]
    fn reconnecting_label_says_which_attempt_and_how_long() {
        assert_eq!(
            state_label(None),
            None,
            "没启动的「未启动」由 row_status 兜底,浮层不写"
        );
        let s = state_label(Some(&TunnelState::Reconnecting {
            attempt: 3,
            retry_in: std::time::Duration::from_secs(4),
        }))
        .unwrap();
        assert!(s.contains('3'), "实际: {s}");
        assert!(s.contains('4'), "实际: {s}");

        let failed = state_label(Some(&TunnelState::Failed("端口被占".into()))).unwrap();
        assert!(failed.contains("端口被占"), "根因必须带出来: {failed}");
        assert_ne!(
            failed,
            state_label(Some(&TunnelState::Stopped)).unwrap(),
            "「自己挂了」和「被停了」不能是同一句话"
        );
    }

    /// 「会失去引用」和「正在跑的会被停掉」分量不同,确认框里必须分开说。
    #[test]
    fn delete_confirm_counts_only_the_tunnels_that_are_actually_running() {
        let tunnels = vec![rec(1, local()), rec(2, local()), rec(3, local())];
        let states = vec![
            (TunnelId(1), TunnelState::Running),
            (TunnelId(3), TunnelState::Connecting),
        ];
        let note = running_note(SessionId(7), &tunnels, &states).expect("有在跑的就得说");
        assert!(note.contains('2'), "只算在运行时表里的那些,实际: {note}");

        assert_eq!(
            running_note(SessionId(7), &tunnels, &[]),
            None,
            "一条都没跑时不该多画一行"
        );
        assert_eq!(
            running_note(SessionId(99), &tunnels, &states),
            None,
            "别的会话的隧道不该算进来"
        );
    }

    #[test]
    fn affected_lines_is_empty_when_nothing_references_the_session() {
        let list = vec![rec(1, local())];
        assert!(affected_lines(SessionId(99), &list).is_empty());
    }

    fn other(
        id: u64,
        state: mullion_store::RemoteTunnelState,
        pid: u32,
    ) -> mullion_store::OtherTunnel {
        mullion_store::OtherTunnel {
            tunnel: TunnelId(id),
            state,
            pid,
            started_at: 0,
            instance: format!("i{pid}"),
        }
    }

    /// F338:自己跑着的永远停得掉;别处在跑才禁用,且说明原因。
    #[test]
    fn elsewhere_disables_start_with_a_reason_but_never_blocks_our_own_stop() {
        let s = button_spec(false, false, true);
        assert!(!s.enabled);
        assert!(s.disabled_hint.is_some_and(|h| h.contains("另一个窗口")));
        assert!(
            button_spec(false, true, true).enabled,
            "本实例自己跑着的必须停得掉"
        );
        assert_eq!(button_spec(false, true, true).label, "停止");
    }

    /// 两个实例报了同一条:一个已失败、一个正跑 —— 显示和挡启动都得跟着**正跑**的。
    /// 自证会变红:把 `other_for` 改成只取第一条。
    #[test]
    fn a_blocking_report_wins_over_a_failed_one_for_the_same_tunnel() {
        use mullion_store::RemoteTunnelState as R;
        let both = [other(1, R::Failed, 10), other(1, R::Running, 20)];
        assert_eq!(other_for(TunnelId(1), &both).unwrap().pid, 20);
        let only_failed = [other(1, R::Failed, 10)];
        assert_eq!(other_for(TunnelId(1), &only_failed).unwrap().pid, 10);
        assert!(other_for(TunnelId(2), &both).is_none());
    }

    #[test]
    fn the_other_window_label_names_the_state_and_the_hover_names_the_pid() {
        use mullion_store::RemoteTunnelState as R;
        assert!(other_label(&other(1, R::Running, 1)).contains("运行中"));
        assert!(other_label(&other(1, R::Failed, 1)).contains("已停止"));
        assert_ne!(
            other_label(&other(1, R::Connecting, 1)),
            other_label(&other(1, R::Reconnecting, 1))
        );
        let h = other_hover(&other(1, R::Running, 4242), 3600);
        assert!(h.contains("4242") && h.contains("小时前"), "实际: {h}");
    }

    fn named(id: u64, name: &str) -> TunnelRecord {
        let mut r = rec(id, local());
        r.name = name.into();
        r
    }

    /// F268:起了名的行,名字当主标题,**转发串降到第二行**而不是消失。
    ///
    /// 转发串是判断「这条是不是我要找的那条」的关键信息:名字可以撞、可以
    /// 过期(改了端口忘了改名),而「本地 3306 → db:3306」是从配置现算出来的。
    ///
    /// 自证会变红:把 `row_title`/`list_title` 里的空名判断反过来,或让
    /// `forward_row` 对起了名的返回 `None`。
    #[test]
    fn a_named_tunnel_shows_its_name_up_top_and_keeps_the_forward_string_below() {
        let r = named(1, "生产库");
        assert_eq!(row_title(&r), "生产库");
        assert_eq!(list_title(&r), "生产库");
        assert_eq!(
            forward_row(&r).as_deref(),
            Some("本地 :3306 → db.internal:3306")
        );
        let (via, dangling) = via_line(&r, &[], &[]);
        assert!(dangling, "引用的会话不在表里,仍然要判成悬垂");
        assert_eq!(via, "引用的会话已删除");
    }

    /// 没起名的行:标题就是转发串,不塞「未命名隧道」之类的占位,也**不再画
    /// 第二行**(那会变成同一句话画两行)。
    ///
    /// 自证会变红:把 `row_title` 的兜底改成 `"未命名隧道"`,或把
    /// `forward_row` 里的空名判断去掉。
    #[test]
    fn an_unnamed_tunnel_renders_exactly_as_before() {
        let r = rec(1, local());
        assert_eq!(row_title(&r), "本地 3306 → db.internal:3306");
        assert_eq!(list_title(&r), "本地 :3306 → db.internal:3306");
        assert_eq!(forward_row(&r), None, "没起名就别再画一行转发串");
    }

    /// 名字只有空白 = 没起名。`build_tunnel_draft` 那边是 `trim()` 后落盘的,
    /// 但手改 toml 塞一串空格进去照样读得回来 —— 渲染这边不能因此画出一行
    /// 看上去什么都没写的标题。
    ///
    /// 自证会变红:把 `row_title` 里的 `rec.name.trim()` 改成 `rec.name`。
    #[test]
    fn a_whitespace_only_name_counts_as_unnamed() {
        assert_eq!(
            row_title(&named(1, "   ")),
            "本地 3306 → db.internal:3306",
            "只有空白的名字不能顶掉转发串"
        );
    }

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

    /// F268:右键菜单里的「克隆」点下去要真的落下意图。
    ///
    /// 纯函数(`Vault::clone_tunnel`)测得再扎实也证不了这一条 —— 菜单项与
    /// 施加点之间的那根线断了(没写 `ui_state.tunnel_clone_request`),症状是
    /// 「点了没反应」,而所有纯函数测试照样全绿。
    ///
    /// 驱动方式照抄 `list::right_click_offers_clone_between_move_to_group_and_delete`
    /// (它已经解决了「`context_menu` 要一次右键 + 一帧 sizing pass 才展开」;
    /// 本仓库为预热帧数不足吃过两次静默假绿)。
    ///
    /// 自证会变红:把 `ui_state.tunnel_clone_request = Some(rec.id);` 那一行删掉。
    #[test]
    fn right_clicking_a_tunnel_row_offers_clone_and_it_lands_the_intent() {
        let t = crate::theme::MULLION_DARK;
        let tunnels = vec![named(4, "tunnel-clone-target")];
        let mut ui_state = UiState::default();
        let ctx = egui::Context::default();

        let run = |ctx: &egui::Context, ui_state: &mut UiState, input: egui::RawInput| {
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(ui, &t, ui_state, &tunnels, &[], &[], &[], &[]);
                });
            })
        };
        let click = |pos, button, pressed| egui::Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers: egui::Modifiers::default(),
        };

        let _ = run(&ctx, &mut ui_state, egui::RawInput::default());
        let out = run(&ctx, &mut ui_state, egui::RawInput::default());
        let row_pos =
            find_text_pos(&out.shapes, "tunnel-clone-target").expect("这一行应该已经画出来了");
        let row_click_pos = egui::pos2(row_pos.x + 4.0, row_pos.y + 4.0);
        let _ = run(
            &ctx,
            &mut ui_state,
            egui::RawInput {
                events: vec![
                    egui::Event::PointerMoved(row_click_pos),
                    click(row_click_pos, egui::PointerButton::Secondary, true),
                    click(row_click_pos, egui::PointerButton::Secondary, false),
                ],
                ..Default::default()
            },
        );
        // 菜单是 `Area`,首帧先做一趟不可见的 sizing pass。
        let out = run(&ctx, &mut ui_state, egui::RawInput::default());
        let clone_pos = find_text_pos(&out.shapes, "克隆").expect("菜单里应该有「克隆」");
        let _ = run(
            &ctx,
            &mut ui_state,
            egui::RawInput {
                events: vec![
                    egui::Event::PointerMoved(clone_pos),
                    click(clone_pos, egui::PointerButton::Primary, true),
                    click(clone_pos, egui::PointerButton::Primary, false),
                ],
                ..Default::default()
            },
        );
        assert_eq!(
            ui_state.tunnel_clone_request,
            Some(TunnelId(4)),
            "点「克隆」必须落下克隆意图,否则点了没反应"
        );
        assert!(
            ui_state.pending_tunnel_delete.is_none(),
            "克隆不该顺带把这一行推进删除确认"
        );
    }

    /// F385:端口前加 `:`,三种转发都一样;目标端的 `host:port` 原样不动。
    #[test]
    fn the_list_forward_line_marks_the_listen_port_with_a_colon() {
        assert_eq!(
            forward_line(&rec(1, local())),
            "本地 :3306 → db.internal:3306"
        );
        let r = forward_line(&rec(
            2,
            TunnelKind::Remote {
                target_host: "127.0.0.1".into(),
                target_port: 3000,
                expose: false,
            },
        ));
        assert_eq!(r, "远端 :3306 → 127.0.0.1:3000");
        assert_eq!(
            forward_line(&rec(3, TunnelKind::Dynamic)),
            "动态 :3306 (SOCKS5)"
        );
    }

    /// F385:状态常驻。没启动 = 「未启动」灰档;本窗口运行态压过别的窗口;
    /// 重连是黄、停止是红,两者不能同档(F114)。
    ///
    /// 自证会变红:把兜底改回不显示(返回空串),或把 `Reconnecting` 归进 `Danger`。
    #[test]
    fn the_status_is_always_shown_and_toned_by_meaning() {
        use mullion_store::RemoteTunnelState as R;
        assert_eq!(row_status(None, None), ("未启动".to_string(), Tone::Idle));
        assert_eq!(row_status(Some(&TunnelState::Running), None).1, Tone::Ok);
        let recon = TunnelState::Reconnecting {
            attempt: 1,
            retry_in: std::time::Duration::from_secs(2),
        };
        assert_eq!(row_status(Some(&recon), None).1, Tone::Warn);
        assert_eq!(
            row_status(Some(&TunnelState::Stopped), None).1,
            Tone::Danger
        );
        let o = other(1, R::Running, 9);
        let (text, tone) = row_status(None, Some(&o));
        assert!(text.contains("另一个窗口"), "实际: {text}");
        assert_eq!(tone, Tone::Warn);
        assert_eq!(
            row_status(None, Some(&other(1, R::Failed, 9))).1,
            Tone::Danger
        );
    }

    /// F385:状态放不下就挪走,名称至少留 `NAME_MIN_W`(名字更短按实宽)。
    #[test]
    fn the_status_moves_off_the_first_row_before_the_name_gets_squeezed() {
        assert!(status_fits_first_row(300.0, 60.0, 50.0, 100.0));
        assert!(
            !status_fits_first_row(150.0, 60.0, 50.0, 100.0),
            "名称只剩 40 了"
        );
        assert!(
            status_fits_first_row(150.0, 60.0, 50.0, 30.0),
            "名字本身只有 30 宽,够放就别挪"
        );
    }

    fn render(
        ctx: &egui::Context,
        ui_state: &mut UiState,
        tunnels: &[TunnelRecord],
        input: egui::RawInput,
    ) -> egui::FullOutput {
        let t = crate::theme::MULLION_DARK;
        ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                show(ui, &t, ui_state, tunnels, &[], &[], &[], &[]);
            });
        })
    }

    fn count_hlines(shapes: &[egui::epaint::ClippedShape]) -> usize {
        fn walk(s: &egui::Shape) -> usize {
            match s {
                egui::Shape::Vec(v) => v.iter().map(walk).sum(),
                egui::Shape::LineSegment { points, .. } if points[0].y == points[1].y => 1,
                _ => 0,
            }
        }
        shapes.iter().map(|cs| walk(&cs.shape)).sum()
    }

    /// F385:条目之间有分割线 —— N 条隧道比 1 条多出 N-1 根横线。
    ///
    /// 自证会变红:删掉 `show` 里循环中的 `ui.separator()`。
    #[test]
    fn entries_are_separated_by_a_line_between_each_pair() {
        let one = vec![named(1, "a")];
        let three = vec![named(1, "a"), named(2, "b"), named(3, "c")];
        let lines = |list: &[TunnelRecord]| {
            let ctx = egui::Context::default();
            let mut s = UiState::default();
            let _ = render(&ctx, &mut s, list, egui::RawInput::default());
            count_hlines(&render(&ctx, &mut s, list, egui::RawInput::default()).shapes)
        };
        assert_eq!(lines(&three), lines(&one) + 2);
    }

    /// F385:点在整条的任意位置都选中 —— 这里点第三行「经 …」那句,它不是
    /// 标题,改版前点它没反应。也顺带证「未启动」是常驻画出来的。
    ///
    /// 自证会变红:把 `UiBuilder` 上的 `.sense(egui::Sense::click())` 去掉,或把第三行
    /// 的 `selectable(false)` 去掉(可划选的 Label 吞点击)。
    #[test]
    fn clicking_anywhere_on_the_row_selects_the_tunnel() {
        let tunnels = vec![named(4, "row-click-target")];
        let ctx = egui::Context::default();
        let mut s = UiState::default();
        let _ = render(&ctx, &mut s, &tunnels, egui::RawInput::default());
        let out = render(&ctx, &mut s, &tunnels, egui::RawInput::default());
        assert!(
            find_text_pos(&out.shapes, "未启动").is_some(),
            "没启动的也要常驻状态"
        );
        let p = find_text_pos(&out.shapes, "引用的会话已删除").expect("第三行应该画出来了");
        let p = egui::pos2(p.x + 4.0, p.y + 4.0);
        let click = |pressed| egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let _ = render(
            &ctx,
            &mut s,
            &tunnels,
            egui::RawInput {
                events: vec![egui::Event::PointerMoved(p), click(true), click(false)],
                ..Default::default()
            },
        );
        assert_eq!(s.tunnel_editor_id, Some(TunnelId(4)));
        assert!(s.tunnel_editor.is_some());
    }
}
