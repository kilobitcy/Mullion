//! 菜单栏(含居中的布局预设按钮组)+ 状态栏。
use super::annotate;
use super::toolbar;
use super::UiState;
use crate::shell::workspace::Preset;
use crate::theme::{self, Theme};

/// 菜单栏上下内边距。
const MENU_MARGIN_Y: f32 = 3.0;

/// 菜单栏高度(逻辑点)。
///
/// 由 `toolbar::group_size` 推出来、**不写死**:布局按钮组画在这一行里,两处
/// 各写一个数的话,改了按钮尺寸菜单栏就会把按钮组裁掉半截。
///
/// 固定高度而不是由内容撑开,是因为按钮组只在已连接时画 —— 让 egui 自己撑
/// 会让菜单栏在连接成功那一刻从 30px 跳到 34px,中央区跟着抖一下。
fn menu_px() -> f32 {
    toolbar::group_size(Preset::ALL.len()).y + MENU_MARGIN_Y * 2.0
}

/// 画菜单栏。返回用户这一帧在居中按钮组上点中的布局预设(F82)。
pub fn top_menu(
    ctx: &egui::Context,
    t: &Theme,
    ui_state: &mut UiState,
    connected: bool,
    preset: Option<Preset>,
) -> Option<Preset> {
    // 菜单栏与状态栏底色不同(§2.1),Visuals::panel_fill 只有一个值,
    // 所以各自带 Frame。
    let mut clicked = None;
    let bar = egui::TopBottomPanel::top("menu")
        .exact_height(menu_px())
        .frame(
            egui::Frame::none()
                .fill(theme::c32(t.bar_menu))
                .inner_margin(egui::Margin::symmetric(6.0, MENU_MARGIN_Y))
                .stroke(theme::stroke(t)),
        )
        .show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("会话", |ui| {
                    if ui.button("会话管理器").clicked() {
                        ui_state.session_manager_open = true;
                        ui.close_menu();
                    }
                    if ui.button("分组管理器").clicked() {
                        ui_state.group_manager_open = true;
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(connected, egui::Button::new("断开"))
                        .clicked()
                    {
                        ui_state.request_disconnect = true;
                        ui.close_menu();
                    }
                    if ui.button("退出").clicked() {
                        ui_state.request_quit = true;
                        ui.close_menu();
                    }
                });
                ui.menu_button("配置", |ui| {
                    // F83:标题条占 32px,关掉能换回一行终端。切换后行数会变,
                    // 必须走 apply_geometry 发 window_change(T4),故只置意图。
                    // 原先挂在「分屏」菜单下,该菜单已撤(布局改由同一行的按钮组
                    // 控制),这项是它下面唯一的真功能,挪到「配置」。
                    if ui.button("显示 / 隐藏 pane 标题条").clicked() {
                        ui_state.toggle_title_bars = true;
                        ui.close_menu();
                    }
                    ui.add_enabled(false, egui::Button::new("(F84 设置 · 后续切片)"));
                    ui.add_enabled(false, egui::Button::new("(快捷键 · 后续切片)"));
                });
                ui.menu_button("关于", |ui| {
                    if ui.button("关于 Mullion").clicked() {
                        ui_state.about_open = true;
                        ui.close_menu();
                    }
                });
                // 布局按钮组:菜单项之后画,自己算居中留白(见 `centering_space`)。
                // 只在已连接时画 —— launcher 态没有 pane 可切布局。
                if connected {
                    clicked = toolbar::show_in(ui, t, preset);
                }
            });
        });
    annotate::mark(ctx, "菜单栏", bar.response.rect);
    clicked
}

/// F81 状态栏两栏文案。纯函数,可单测。
///
/// 左栏 `{N} 屏 · {连接态}`,右栏编码。**色点不进字符串**——它要按连接态分别用
/// `ok` / `fg_faint` 上色,塞进文本就只能是一个颜色。
///
/// 「远端 SSH 版本」本来在设计里,复核后砍掉:russh 的 `remote_sshid()` 只在带
/// `session` 参数的 Handler 回调里够得着,F3 用的 `check_server_key` 拿不到,
/// 要做是跨 crate 事件接线,不该混进纯视觉改动(见设计文档 §3.4)。
pub fn status_text(panes: usize, connected: bool) -> (String, String) {
    let left = format!(
        "{} 屏 · {}",
        panes,
        if connected { "已连接" } else { "未连接" }
    );
    (left, "UTF-8".to_string())
}

/// `last_error`(F3 落盘失败等)必须总有个展示位:它可能是在会话管理器/编辑器
/// 都已关闭之后才产生的(如主机密钥确认后 `ConnectOk` 顺手关掉了会话管理器),
/// 那两处的 `last_error` 渲染此时根本不会被调用到(复核 A4)。状态栏常驻,
/// 不受那两个弹窗开关状态影响,兜底展示。
// 状态栏就是「一格一个参数」的东西,聚成结构体只是把同样的字段换个地方写
// (理由同 `session_manager::show`:内部叶子函数,压参数数没有实际收益)。
#[allow(clippy::too_many_arguments)]
pub fn status_bar(
    ctx: &egui::Context,
    t: &Theme,
    panes: usize,
    connected: bool,
    last_error: Option<&str>,
    automation: Option<&str>,
    // F115:隧道指示器。`None` = 一条都没启动,那就**不占格** ——
    // 状态栏每多常驻一格,别的信息就少一分被看见的机会。
    tunnel: Option<&crate::tunnels::Indicator>,
    // F62:当前聚焦 pane 所属会话的语义色。**只有勾了「状态栏」落点才是
    // `Some`**(过滤在 `badge::should_paint` 里做,这里只负责画)。
    session_color: Option<egui::Color32>,
) {
    let (left, right) = status_text(panes, connected);
    let bar = egui::TopBottomPanel::bottom("status")
        .frame(
            egui::Frame::none()
                .fill(theme::c32(t.bar_status))
                .inner_margin(egui::Margin::symmetric(8.0, 2.0))
                .stroke(theme::stroke(t)),
        )
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                // F62:会话语义色是**画**出来的一个小竖块,不是拼进 `status_text`
                // 的字形 —— 那条纯函数有 `status_text_carries_no_dot_glyph` 守着
                // 「字形不进字符串」,而它是对的:塞进文本就只能是一个颜色。
                if let Some(c) = session_color {
                    let (r, _) = ui.allocate_exact_size(
                        egui::vec2(crate::ui::badge::EDGE_BAR_W, 12.0),
                        egui::Sense::hover(),
                    );
                    ui.painter().rect_filled(r, egui::Rounding::same(1.5), c);
                }
                let dot = if connected { t.ok } else { t.fg_faint };
                ui.colored_label(theme::c32(dot), "●");
                ui.colored_label(theme::c32(t.fg_faint), left);
                // last_error 必须可见:右对齐区先画它,再画常规右栏。
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(err) = last_error {
                        ui.colored_label(theme::c32(t.danger), err);
                        ui.separator();
                    }
                    // F40~F44:自动化状态排在错误之后、常规右栏之前。
                    // 它是「这次连接发生了什么」的唯一可见证据——用户看不见,
                    // 就无法判断自动化是没跑还是跑了没效果。
                    if let Some(a) = automation {
                        ui.colored_label(theme::c32(t.fg_muted), a);
                        ui.separator();
                    }
                    // F115:隧道排在自动化之后。颜色跟着**最坏**那条走
                    // (见 `tunnels::indicator`)—— 一律画成灰的,等于把
                    // 「有一条挂了」这条唯一需要被看见的信息藏起来。
                    if let Some(ind) = tunnel {
                        let color = match ind.severity {
                            crate::tunnels::Severity::Calm => t.fg_muted,
                            crate::tunnels::Severity::Warn => t.warn,
                            crate::tunnels::Severity::Danger => t.danger,
                        };
                        let r = ui.colored_label(theme::c32(color), &ind.text);
                        annotate::mark(ui.ctx(), "状态栏/隧道指示器", r.rect);
                        ui.separator();
                    }
                    ui.colored_label(theme::c32(t.fg_faint), right);
                });
            });
        });
    annotate::mark(ctx, "状态栏", bar.response.rect);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_text_connected_single_pane() {
        let (left, right) = status_text(1, true);
        assert_eq!(left, "1 屏 · 已连接");
        assert_eq!(right, "UTF-8");
    }

    #[test]
    fn status_text_disconnected() {
        let (left, _) = status_text(1, false);
        assert_eq!(left, "1 屏 · 未连接");
    }

    /// 分屏(F30)落地后 N 会变;格式化提前按多屏写好,免得那时再动状态栏。
    #[test]
    fn status_text_multi_pane() {
        let (left, _) = status_text(4, true);
        assert_eq!(left, "4 屏 · 已连接");
    }

    /// 色点不进字符串:它要按连接态分别用 ok / fg_faint 上色,
    /// 混在文本里就只能是一个颜色。
    #[test]
    fn status_text_carries_no_dot_glyph() {
        let (left, right) = status_text(1, true);
        assert!(!left.contains('●'), "色点应由调用方单独上色绘制");
        assert!(!right.contains('●'));
    }

    fn count_shapes(shapes: &[egui::epaint::ClippedShape]) -> usize {
        fn walk(s: &egui::Shape) -> usize {
            match s {
                egui::Shape::Vec(v) => v.iter().map(walk).sum(),
                egui::Shape::Noop => 0,
                _ => 1,
            }
        }
        shapes.iter().map(|cs| walk(&cs.shape)).sum()
    }

    fn run_status(session_color: Option<egui::Color32>) -> usize {
        let ctx = egui::Context::default();
        // 跑两帧:egui 的面板/Area 默认 `fade_in`,第一帧 opacity=0 会让
        // `Painter::add` 把所有形状记成 `Shape::Noop`,数出来全是 0。
        // 同 `ui/mod.rs::rendered_text` 的做法。
        let _ = ctx.run(Default::default(), |ctx| {
            status_bar(
                ctx,
                &crate::theme::MULLION_DARK,
                1,
                true,
                None,
                None,
                None,
                session_color,
            );
        });
        let out = ctx.run(Default::default(), |ctx| {
            status_bar(
                ctx,
                &crate::theme::MULLION_DARK,
                1,
                true,
                None,
                None,
                None,
                session_color,
            );
        });
        count_shapes(&out.shapes)
    }

    /// 状态栏文字的**绘制顺序**。`right_to_left` 布局里先画的在最右,
    /// 所以这个顺序就是视觉上从右往左。
    fn status_texts(
        automation: Option<&str>,
        tunnel: Option<&crate::tunnels::Indicator>,
    ) -> Vec<String> {
        let ctx = egui::Context::default();
        let mut acc = Vec::new();
        // 同 `run_status`:面板首帧 `fade_in`,形状全被记成 Noop,必须跑两帧。
        for _ in 0..2 {
            let out = ctx.run(Default::default(), |ctx| {
                status_bar(
                    ctx,
                    &crate::theme::MULLION_DARK,
                    1,
                    true,
                    None,
                    automation,
                    tunnel,
                    None,
                );
            });
            acc.clear();
            fn walk(s: &egui::Shape, out: &mut Vec<String>) {
                match s {
                    egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                    egui::Shape::Text(t) => out.push(t.galley.text().to_string()),
                    _ => {}
                }
            }
            out.shapes.iter().for_each(|cs| walk(&cs.shape, &mut acc));
        }
        acc
    }

    /// F115:隧道那一格排在自动化和编码之间。三条信息挤在同一个右对齐区里,
    /// 顺序一变用户就得每次重新找 —— 而这一格恰恰是「有没有出事」的那一格。
    #[test]
    fn status_bar_shows_the_tunnel_indicator_between_automation_and_encoding() {
        let ind = crate::tunnels::Indicator {
            text: "隧道 1/2 ↻".to_string(),
            severity: crate::tunnels::Severity::Warn,
        };
        let texts = status_texts(Some("自动化已就绪"), Some(&ind));
        let pos = |needle: &str| {
            texts
                .iter()
                .position(|s| s.contains(needle))
                .unwrap_or_else(|| panic!("状态栏没画出「{needle}」:{texts:?}"))
        };
        assert!(
            pos("自动化") < pos("隧道"),
            "隧道要排在自动化之后:{texts:?}"
        );
        assert!(pos("隧道") < pos("UTF-8"), "隧道要排在编码之前:{texts:?}");
    }

    /// 一条都没启动时**不占格**。状态栏每多常驻一格,别的信息就少一分被
    /// 看见的机会;而「你配了但没开」这件事在会话管理器里看就够了。
    #[test]
    fn status_bar_has_no_tunnel_indicator_when_none_configured() {
        let texts = status_texts(None, None);
        assert!(
            !texts.iter().any(|s| s.contains("隧道")),
            "没有在跑的隧道时不该占一格:{texts:?}"
        );
    }

    /// F100:菜单栏与状态栏也要登记 —— 走查里「状态栏那行字太靠边」这类反馈
    /// 很常见,标不到它就得回到「用嘴描述」。
    ///
    /// 自证会变红:注释掉 `top_menu` / `status_bar` 末尾任一句 `annotate::mark`。
    #[test]
    fn annotate_mode_registers_the_menu_and_status_bars() {
        let ctx = egui::Context::default();
        let mut ui_state = UiState::default();
        annotate::toggle(&ctx);
        let mut paths = Vec::new();
        // 同 `run_status`:面板首帧 `fade_in`,跑两帧再取。
        for _ in 0..2 {
            let _ = ctx.run(Default::default(), |ctx| {
                top_menu(ctx, &crate::theme::MULLION_DARK, &mut ui_state, true, None);
                status_bar(
                    ctx,
                    &crate::theme::MULLION_DARK,
                    1,
                    true,
                    None,
                    None,
                    None,
                    None,
                );
                paths = annotate::spot_paths(ctx);
            });
        }
        assert!(
            paths.iter().any(|p| p == "菜单栏"),
            "菜单栏没登记:{paths:?}"
        );
        assert!(
            paths.iter().any(|p| p == "状态栏"),
            "状态栏没登记:{paths:?}"
        );
    }

    /// F62:状态栏的会话色是**画**出来的一个小色块,不是拼进文本的字形。
    #[test]
    fn status_bar_paints_a_session_color_block_when_given_one() {
        let none = run_status(None);
        let with = run_status(Some(egui::Color32::from_rgb(0xe0, 0x67, 0x67)));
        assert!(
            with > none,
            "给了会话色就该多画一个色块(无 {none} 个图形,有 {with} 个)"
        );
    }
}
