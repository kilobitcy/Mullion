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

/// 一行的主标题:`本地 3306 → db.internal:3306`。
///
/// 方向箭头照着**数据流向**写,不照配置字段顺序 —— 这个功能最常见的错误
/// 就是把两端填反,标题里看不出方向等于把排查成本推到运行时。
pub(crate) fn row_title(rec: &TunnelRecord) -> String {
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

/// 一行的副标题:引用的会话。悬垂时说清楚是**引用**坏了,不是隧道配错了。
fn row_subtitle(
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
pub(crate) fn button_spec(dangling: bool, live: bool) -> ButtonSpec {
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
    ButtonSpec {
        label: "启动",
        enabled: true,
        disabled_hint: None,
    }
}

/// 一行右侧的状态文字。`None` = 没启动,那一行什么都不写 ——
/// 给没启动的隧道常驻一句「未启动」只是噪音。
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

pub(super) fn show(
    ui: &mut Ui,
    t: &Theme,
    ui_state: &mut UiState,
    tunnels: &[TunnelRecord],
    states: &[(TunnelId, TunnelState)],
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
            for id in order {
                let Some(rec) = tunnels.iter().find(|x| x.id == id) else {
                    continue;
                };
                let state = states.iter().find(|(tid, _)| *tid == id).map(|(_, s)| s);
                row(ui, t, ui_state, rec, state, sessions, credentials);
            }
        });
}

fn row(
    ui: &mut Ui,
    t: &Theme,
    ui_state: &mut UiState,
    rec: &TunnelRecord,
    state: Option<&TunnelState>,
    sessions: &[SessionRecord],
    credentials: &[mullion_store::CredentialRecord],
) {
    let selected = ui_state.tunnel_editor_id == Some(rec.id);
    let (sub, dangling) = row_subtitle(rec, sessions, credentials);
    let spec = button_spec(dangling, state.is_some());

    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            let title = row_title(rec);
            let resp = ui.add(egui::SelectableLabel::new(selected, title));
            if resp.clicked() {
                ui_state.tunnel_editor_id = Some(rec.id);
                let buf = super::TunnelEditorBuffer::from_record(rec);
                ui_state.tunnel_editor_baseline = Some(buf.clone());
                ui_state.tunnel_editor = Some(buf);
            }
            let sub_color = if dangling {
                theme::c32(t.danger_text)
            } else {
                theme::c32(t.fg_dimmer)
            };
            ui.colored_label(sub_color, sub);
            // F114:跑起来之后这一行才有状态可写。颜色跟着语义走 ——
            // 「重连中」和「已停止」用同一个灰,等于把需要人管的那条藏起来。
            if let Some(text) = state_label(state) {
                let color = match state {
                    Some(TunnelState::Running) => theme::c32(t.ok),
                    Some(TunnelState::Failed(_)) | Some(TunnelState::Stopped) => {
                        theme::c32(t.danger_text)
                    }
                    _ => theme::c32(t.warn),
                };
                ui.colored_label(color, text);
            }
        });

        // 启动/停止:禁用必须是**真禁用**,不是画成灰的。`ui.disable()` 让
        // `interact` 返回 `enabled=false` 且 `clicked()` 恒假 —— 「看着灰」和
        // 「点不动」必须是同一件事,否则用户点了没反应会以为是隧道配错了。
        // 什么时候该禁用见纯函数 `button_spec`(它自己有穷举测试)。
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
    });
    ui.add_space(2.0);
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
        let live = button_spec(false, true);
        assert_eq!(live.label, "停止");
        assert!(live.enabled, "跑着的隧道必须随时停得掉");

        let ok = button_spec(false, false);
        assert_eq!(ok.label, "启动");
        assert!(ok.enabled);
        assert!(ok.disabled_hint.is_none());

        let dangling = button_spec(true, false);
        assert!(!dangling.enabled, "引用的会话都没了,起不来");
        assert!(dangling.disabled_hint.is_some_and(|h| h.contains("已删除")));

        // 跑着的隧道即使引用悬垂也得能停 —— 端口还占着,不给停就成了
        // 「只能重启客户端才能关掉的通路」。
        assert!(button_spec(true, true).enabled);
    }

    /// 重连中必须同时说清**第几次**和**还有多久**。少任何一半,用户都判断
    /// 不出「是在退避还是已经卡死」,而这正是重连唯一需要被看见的信息。
    #[test]
    fn reconnecting_label_says_which_attempt_and_how_long() {
        assert_eq!(state_label(None), None, "没启动的隧道不该常驻一句状态");
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
}
