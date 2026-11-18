//! F369:状态栏隧道指示器点开的启停浮层。
//!
//! 原先启停隧道要走 菜单「会话」→ 会话管理器 → 隧道页 → 选一条 → 启动/停止,
//! 而这是天天要做的事。浮层每条一行:名字 · 转发串 · 状态 · [启动/停止] [编辑];
//! 底部「+ 新建隧道」「管理隧道…」(F378)。按钮判据与会话管理器列表行**同源**(`tunnel_list::row_spec`)。
//!
//! 「启动」走手动路径(人在屏幕前,首连可弹 TOFU)。浮层开着时顶栏红字容易被
//! 忽略,所以启动当场失败的原因**行内**再写一遍(`errors`)。
//!
//! 只 UI 侧状态与出口;施加(起隧道、开会话管理器)在 `app.rs`。

use std::collections::BTreeMap;

use mullion_ssh::tunnel::TunnelState;
use mullion_store::{SessionRecord, TunnelId, TunnelRecord};

use crate::theme::{self, Theme};
use crate::ui::session_manager::tunnel_list;

/// 浮层状态。`None` = 关着。
pub struct TunnelPopup {
    /// 状态栏指示器那一格的矩形 —— 浮层贴着它的右上角往上弹。
    pub anchor: egui::Rect,
    /// 本次浮层里「启动」当场失败的原因(缺密码 / 端口被占…)。
    pub errors: BTreeMap<TunnelId, String>,
}

impl TunnelPopup {
    pub fn new(anchor: egui::Rect) -> Self {
        Self {
            anchor,
            errors: BTreeMap::new(),
        }
    }
}

/// 用户在浮层里点了什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelOut {
    Start(TunnelId),
    Stop(TunnelId),
    /// 跳会话管理器隧道页并选中这一条(改端口用)。
    Edit(TunnelId),
    /// 跳会话管理器隧道页。
    Manage,
    /// F378:跳会话管理器隧道页,右栏开一份空的新建表单。
    New,
    Close,
}

/// 浮层要读的那几张表(都来自 `UiFrame`)。
pub struct View<'a> {
    pub tunnels: &'a [TunnelRecord],
    pub states: &'a [(TunnelId, TunnelState)],
    pub others: &'a [mullion_store::OtherTunnel],
    pub sessions: &'a [SessionRecord],
}

/// 浮层的 area id。F239「点外面关」按它判里外。
pub fn area_id() -> egui::Id {
    egui::Id::new("mullion_tunnel_popup")
}

/// 一行的状态文字与颜色。本窗口的运行态优先;没有才写别的窗口的;都没有 = 不写。
fn status(
    t: &Theme,
    state: Option<&TunnelState>,
    other: Option<&mullion_store::OtherTunnel>,
) -> Option<(String, egui::Color32)> {
    if let Some(text) = tunnel_list::state_label(state) {
        let color = match state {
            Some(TunnelState::Running) => t.ok,
            Some(TunnelState::Failed(_)) | Some(TunnelState::Stopped) => t.danger_text,
            _ => t.warn,
        };
        return Some((text, theme::c32(color)));
    }
    let o = other?;
    let color = if o.state.blocks_start() {
        t.warn
    } else {
        t.danger_text
    };
    Some((tunnel_list::other_label(o).to_string(), theme::c32(color)))
}

/// 画浮层。返回本帧的出口(施加在 `app.rs`)。
pub fn show(
    ctx: &egui::Context,
    t: &Theme,
    popup: &Option<TunnelPopup>,
    v: View<'_>,
) -> Option<TunnelOut> {
    let p = popup.as_ref()?;
    let mut out = None;
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        out = Some(TunnelOut::Close);
    }
    let max_w = crate::ui::metrics::FIELD_W_L + crate::ui::metrics::LABEL_COL_W;
    egui::Area::new(area_id())
        .order(egui::Order::Foreground)
        .pivot(egui::Align2::RIGHT_BOTTOM)
        .fixed_pos(p.anchor.right_top())
        .constrain(true)
        .show(ctx, |ui| {
            ui.set_max_width(max_w);
            egui::Frame::popup(&ctx.style())
                .fill(theme::c32(t.panel_bg))
                .stroke(theme::stroke(t))
                .show(ui, |ui| {
                    crate::ui::annotate::mark(ui.ctx(), "隧道浮层".to_string(), ui.max_rect());
                    for id in tunnel_list::visible_order(v.tunnels) {
                        let Some(rec) = v.tunnels.iter().find(|r| r.id == id) else {
                            continue;
                        };
                        if let Some(o) = row(ui, t, rec, &v, p.errors.get(&id)) {
                            out = Some(o);
                        }
                    }
                    if v.tunnels.is_empty() {
                        ui.colored_label(theme::c32(t.fg_dimmer), "还没有隧道");
                    }
                    // 不用 `separator`:它在自适应宽度的 Area 里按可用宽度铺,
                    // 第二帧把浮层撑到无穷宽(左上角算成 -inf → NaN 断言)。
                    ui.add_space(crate::ui::metrics::SP_S);
                    ui.horizontal(|ui| {
                        // F378:新建也从这里走,不必再钻 会话 → 会话管理器 → 隧道。
                        if ui.button("+ 新建隧道").clicked() {
                            out = Some(TunnelOut::New);
                        }
                        if ui.button("管理隧道…").clicked() {
                            out = Some(TunnelOut::Manage);
                        }
                    });
                });
        });
    out
}

fn row(
    ui: &mut egui::Ui,
    t: &Theme,
    rec: &TunnelRecord,
    v: &View<'_>,
    error: Option<&String>,
) -> Option<TunnelOut> {
    let mut out = None;
    let state = v
        .states
        .iter()
        .find(|(id, _)| *id == rec.id)
        .map(|(_, s)| s);
    let dangling = !v.sessions.iter().any(|s| s.id == rec.session_id);
    let (spec, other) =
        tunnel_list::row_spec(dangling, state, tunnel_list::other_for(rec.id, v.others));
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(tunnel_list::row_title(rec)).color(theme::c32(t.fg_strong)));
        // 起了名的,转发串跟在后面;没起名的标题本身就是转发串,不重复。
        if !rec.name.trim().is_empty() {
            ui.colored_label(theme::c32(t.fg_dimmer), tunnel_list::forward_summary(rec));
        }
        if rec.autostart {
            ui.colored_label(theme::c32(t.fg_dimmer), "[自动]")
                .on_hover_text("打开 Mullion 后自动启动(在隧道页改)");
        }
        if let Some((text, color)) = status(t, state, other) {
            ui.colored_label(color, text);
        }
        // 按钮顺排在行尾,不用 `right_to_left`:Area 宽度随内容自适应,右对齐
        // 会按可用宽度铺开(同下面 `separator` 的坑)。
        // 禁用是**真禁用**(`add_enabled`),理由同列表行。
        let mut r = ui.add_enabled(spec.enabled, egui::Button::new(spec.label));
        if let Some(hint) = spec.disabled_hint {
            r = r.on_disabled_hover_text(hint);
        }
        if r.clicked() {
            out = Some(if state.is_some() {
                TunnelOut::Stop(rec.id)
            } else {
                TunnelOut::Start(rec.id)
            });
        }
        if ui.button("编辑").clicked() {
            out = Some(TunnelOut::Edit(rec.id));
        }
    });
    if let Some(e) = error {
        ui.colored_label(theme::c32(t.danger_text), e);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_store::SessionId;

    fn rec(id: u64, session: u64) -> TunnelRecord {
        TunnelRecord {
            id: TunnelId(id),
            session_id: SessionId(session),
            listen_port: 3306,
            name: "库".into(),
            note: String::new(),
            autostart: true,
            kind: mullion_store::TunnelKind::Dynamic,
        }
    }

    fn sess(id: u64) -> SessionRecord {
        use mullion_store::model::{Auth, AuthKind, Connection, Identity};
        SessionRecord {
            id: SessionId(id),
            modified_at: "t".into(),
            identity: Identity {
                name: "s".into(),
                note: String::new(),
                group_id: None,
                tags: Vec::new(),
            },
            connection: Connection {
                host: "h".into(),
                port: 22,
                protocol: mullion_store::Protocol::Ssh,
            },
            auth: Auth::inline("u", AuthKind::Password),
            terminal: Default::default(),
            appearance: Default::default(),
            network: Default::default(),
            automation: Default::default(),
            sftp: Default::default(),
            last_connected_at: None,
        }
    }

    struct Fixture {
        tunnels: Vec<TunnelRecord>,
        states: Vec<(TunnelId, TunnelState)>,
        others: Vec<mullion_store::OtherTunnel>,
        sessions: Vec<SessionRecord>,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                tunnels: vec![rec(1, 7)],
                states: Vec::new(),
                others: Vec::new(),
                sessions: vec![sess(7)],
            }
        }
        fn view(&self) -> View<'_> {
            View {
                tunnels: &self.tunnels,
                states: &self.states,
                others: &self.others,
                sessions: &self.sessions,
            }
        }
    }

    fn popup() -> Option<TunnelPopup> {
        Some(TunnelPopup::new(egui::Rect::from_min_size(
            egui::pos2(500.0, 580.0),
            egui::vec2(80.0, 20.0),
        )))
    }

    fn texts(shapes: &[egui::epaint::ClippedShape]) -> Vec<(String, egui::Pos2)> {
        fn walk(s: &egui::Shape, out: &mut Vec<(String, egui::Pos2)>) {
            match s {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                egui::Shape::Text(ts) => out.push((
                    ts.galley.text().to_string(),
                    ts.pos + ts.galley.size() / 2.0,
                )),
                _ => {}
            }
        }
        let mut acc = Vec::new();
        shapes.iter().for_each(|cs| walk(&cs.shape, &mut acc));
        acc
    }

    fn screen() -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        }
    }

    /// 画两帧,返回画出的文字;再在写着 `label` 的地方点一下(若给了),返回那帧出口。
    fn run(
        f: &Fixture,
        p: &Option<TunnelPopup>,
        label: Option<&str>,
        extra: Vec<egui::Event>,
    ) -> (Vec<String>, Option<TunnelOut>) {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut shapes = Vec::new();
        for _ in 0..2 {
            shapes = ctx
                .run(screen(), |ctx| {
                    show(ctx, &t, p, f.view());
                })
                .shapes;
        }
        let drawn = texts(&shapes);
        let mut input = screen();
        if let Some(label) = label {
            let pos = drawn
                .iter()
                .find(|(s, _)| s == label)
                .unwrap_or_else(|| panic!("浮层里没有「{label}」:{drawn:?}"))
                .1;
            for pressed in [true, false] {
                input.events.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                });
            }
        }
        input.events.extend(extra);
        let mut out = None;
        let _ = ctx.run(input, |ctx| {
            out = show(ctx, &t, p, f.view());
        });
        (drawn.into_iter().map(|(s, _)| s).collect(), out)
    }

    /// 每行的三颗按钮各自带出自己的出口;没启动的是「启动」,跑着的是「停止」。
    #[test]
    fn the_buttons_report_start_stop_edit_and_manage() {
        let mut f = Fixture::new();
        assert_eq!(
            run(&f, &popup(), Some("启动"), vec![]).1,
            Some(TunnelOut::Start(TunnelId(1)))
        );
        assert_eq!(
            run(&f, &popup(), Some("编辑"), vec![]).1,
            Some(TunnelOut::Edit(TunnelId(1)))
        );
        assert_eq!(
            run(&f, &popup(), Some("管理隧道…"), vec![]).1,
            Some(TunnelOut::Manage)
        );
        f.states.push((TunnelId(1), TunnelState::Running));
        let (drawn, out) = run(&f, &popup(), Some("停止"), vec![]);
        assert_eq!(out, Some(TunnelOut::Stop(TunnelId(1))));
        assert!(drawn.iter().any(|s| s == "运行中"), "{drawn:?}");
    }

    /// F378:底部「+ 新建隧道」带出 `New`;一条隧道都没有时浮层照样能开、能新建。
    ///
    /// 自证会变红:删掉「+ 新建隧道」按钮。
    #[test]
    fn the_popup_can_create_a_tunnel_even_when_there_is_none() {
        let mut f = Fixture::new();
        assert_eq!(
            run(&f, &popup(), Some("+ 新建隧道"), vec![]).1,
            Some(TunnelOut::New)
        );
        f.tunnels.clear();
        let (drawn, out) = run(&f, &popup(), Some("+ 新建隧道"), vec![]);
        assert_eq!(out, Some(TunnelOut::New));
        assert!(drawn.iter().any(|s| s == "还没有隧道"), "{drawn:?}");
    }

    /// 别的窗口在跑的那条:写明在别处,「启动」真禁用(点了没有出口)。
    #[test]
    fn a_tunnel_running_elsewhere_cannot_be_started_here() {
        let mut f = Fixture::new();
        f.others.push(mullion_store::OtherTunnel {
            tunnel: TunnelId(1),
            state: mullion_store::RemoteTunnelState::Running,
            pid: 42,
            started_at: 0,
            instance: String::new(),
        });
        let (drawn, out) = run(&f, &popup(), Some("启动"), vec![]);
        assert_eq!(out, None, "禁用的「启动」不该有出口");
        assert!(drawn.iter().any(|s| s == "在另一个窗口运行中"), "{drawn:?}");
    }

    /// 启动当场失败的原因写在那一行下面;自动启动的有小标记。
    #[test]
    fn the_last_start_error_is_shown_inline_and_autostart_is_marked() {
        let f = Fixture::new();
        let mut p = popup();
        p.as_mut()
            .unwrap()
            .errors
            .insert(TunnelId(1), "端口 3306 已被占用".into());
        let (drawn, _) = run(&f, &p, None, vec![]);
        assert!(drawn.iter().any(|s| s == "端口 3306 已被占用"), "{drawn:?}");
        assert!(drawn.iter().any(|s| s == "[自动]"), "{drawn:?}");
    }

    /// Esc 关浮层(它登记为 Modal,键先给 egui,T8)。
    #[test]
    fn escape_closes_the_popup() {
        let f = Fixture::new();
        let esc = egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        };
        assert_eq!(run(&f, &popup(), None, vec![esc]).1, Some(TunnelOut::Close));
    }

    /// 关着就什么都不画、没有出口。
    #[test]
    fn a_closed_popup_draws_nothing() {
        let f = Fixture::new();
        let (drawn, out) = run(&f, &None, None, vec![]);
        assert!(drawn.is_empty() && out.is_none());
    }
}
