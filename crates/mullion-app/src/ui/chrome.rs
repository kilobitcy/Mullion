//! 菜单栏(含居中的布局预设按钮组)+ 标签栏(F36)+ 状态栏。
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
                    // F50:开关文件侧栏。快捷键 `Ctrl+Shift+B` 的接线在
                    // `app.rs::files_hotkey_event`,这里只是菜单入口。
                    if ui.button("文件面板\tCtrl+Shift+B").clicked() {
                        ui_state.files_sidebar_open = !ui_state.files_sidebar_open;
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

/// 标签栏上下内边距。
const TAB_MARGIN_Y: f32 = 3.0;

/// 单个标签的高度与固定宽度(逻辑点)。
///
/// 宽度固定而不是按标题长短自适应:自适应会让「切一下标签,别的标签跟着左右
/// 挪位」——用户点第 3 个标签的手势记忆每次都得重建。
const TAB_H: f32 = 24.0;
const TAB_W: f32 = 160.0;

/// 活动标签底部那条节点色横杠的厚度。
const TAB_UNDERLINE_H: f32 = 2.0;

/// 标签内的图标边长。走 F61 那套 32px 纹理(`paint_icon` 按 `side <= 32` 选档),
/// 只是画小一点 —— 标签栏是常驻横条,每多 1px 高就少一行终端。
const TAB_ICON: f32 = 16.0;

/// 标签栏高度(逻辑点)。由 `TAB_H` 推出来,**不写死**:两处各写一个数的话,
/// 改了标签高度标签栏就会把标签裁掉半截(同 `menu_px` 的理由)。
pub fn tab_bar_px() -> f32 {
    TAB_H + TAB_MARGIN_Y * 2.0
}

/// 标签栏上一个标签要显示的东西。
pub struct TabView<'a> {
    pub title: &'a str,
    pub active: bool,
    /// F61/F62:这个标签所属会话的已解析外观。`None` = 没有对应会话记录
    /// (快速连接、或 store 不可用)。**必须来自 `badge::AppearanceCache`**,
    /// 不许在这里现解析(陷阱 T3)。
    pub appearance: Option<&'a crate::ui::badge::Appearance>,
}

/// 用户这一帧在标签栏上做了什么。每帧至多一个。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabAction {
    Switch(usize),
    Close(usize),
    /// 右侧 `+`:打开会话管理器。**不是「新建空标签」**——没连接的标签在本项目
    /// 里什么都画不了,建出来只是一个空壳。
    NewSession,
}

/// 画标签栏(F36)。返回用户这一帧的动作。
///
/// **常驻,不自动隐藏**(spec F36 验收):标签栏的显隐是一次高度跳变,整幅终端
/// 要跟着 reflow 发 `window_change`(T4)。只剩一个标签就藏起来,等于把这条最
/// 昂贵的重排绑在「用户关掉倒数第二个标签」这个高频动作上。launcher 态(零个
/// 标签)同样画,只是里面只有一个 `+` —— 否则「连上第一台机」会多一次高度跳变。
///
/// 固定高度而不是由内容撑开,理由同 `menu_px`。
pub fn tab_bar(ctx: &egui::Context, t: &Theme, views: &[TabView<'_>]) -> Option<TabAction> {
    let mut action = None;
    let bar = egui::TopBottomPanel::top("tabs")
        .exact_height(tab_bar_px())
        .frame(
            egui::Frame::none()
                .fill(theme::c32(t.bar_tool))
                .inner_margin(egui::Margin::symmetric(4.0, TAB_MARGIN_Y))
                .stroke(theme::stroke(t)),
        )
        .show(ctx, |ui| {
            // `+` 先用 right_to_left 占住右端,它**不进滚动区** —— 标签多到要
            // 横滚时,新建入口跟着滚出视野是最不该发生的事。
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let plus = ui.add(egui::Button::new("+").frame(false));
                if plus.clicked() {
                    action = Some(TabAction::NewSession);
                }
                annotate::mark(ctx, "标签栏/新建", plus.rect);
                plus.on_hover_text("打开会话管理器");
                // 剩下的宽度(已扣掉 `+`)给标签本身。排不下就横向滚动,
                // 不做溢出下拉菜单:下拉里的标签点不到、也看不出哪个是活动的。
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    egui::ScrollArea::horizontal()
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                for (ix, v) in views.iter().enumerate() {
                                    if let Some(a) = one_tab(ui, t, ix, v) {
                                        action = Some(a);
                                    }
                                }
                            });
                        });
                });
            });
        });
    annotate::mark(ctx, "标签栏", bar.response.rect);
    action
}

/// 画一个标签。返回它这一帧被点出来的动作。
///
/// 几何上先 `allocate_exact_size` 把地盘钉死,再用 painter 画底/横杠、用一个
/// **不折回父 ui 尺寸**的 `new_child` 摆内容 —— 同 `pane_title::show` 的两处
/// 越界坑(`Frame` 的 `min_rect + margin` 撑破预算、`set_min_size` 只设下限)。
/// 标签宽度一旦被长标题撑开,固定宽度这条承诺就没了。
fn one_tab(ui: &mut egui::Ui, t: &Theme, ix: usize, v: &TabView<'_>) -> Option<TabAction> {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(TAB_W, TAB_H),
        egui::Sense::click().union(egui::Sense::hover()),
    );
    // F62:节点色只做**底部那条横杠**,不做整块底色 —— 用户配的是任意
    // #rrggbb,拿它当底色会让标题文字的对比度落到不可读区,而 F80 的色板
    // 纪律管不到用户自己挑的颜色。没配节点色就退回 accent。
    let node = v
        .appearance
        .and_then(|a| crate::ui::badge::should_paint(a, mullion_store::ColorTarget::Tab));
    let bg = if v.active { t.panel_head } else { t.bar_tool };
    let p = ui.painter();
    p.rect_filled(rect, egui::Rounding::same(4.0), theme::c32(bg));
    if v.active {
        p.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(rect.min.x, rect.max.y - TAB_UNDERLINE_H),
                rect.max,
            ),
            egui::Rounding::same(1.0),
            node.unwrap_or(theme::c32(t.accent)),
        );
    }
    let inner = rect.shrink2(egui::vec2(6.0, 2.0));
    let mut content = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    content.set_clip_rect(inner);
    let mut closed = false;
    content.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let x = ui.small_button("×");
        if x.clicked() {
            closed = true;
        }
        annotate::mark(ui.ctx(), format!("标签栏/标签 {}/关闭", ix + 1), x.rect);
        x.on_hover_text("关闭此标签");
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            if let Some(icon) = v.appearance.and_then(|a| a.icon.as_ref()) {
                let (r, _) =
                    ui.allocate_exact_size(egui::vec2(TAB_ICON, TAB_ICON), egui::Sense::hover());
                crate::ui::badge::paint_icon(ui.painter(), r, icon, node);
            }
            ui.add(
                egui::Label::new(egui::RichText::new(v.title).color(theme::c32(if v.active {
                    t.fg_strong
                } else {
                    t.fg_muted
                })))
                .truncate(),
            );
        });
    });
    annotate::mark(ui.ctx(), format!("标签栏/标签 {}", ix + 1), rect);
    // 标题在固定宽度里多半是截断的,tooltip 给全名 —— 不然用户分不清
    // 「同一台机的两个标签」谁是谁。
    let clicked = resp.clicked();
    resp.on_hover_text(v.title);
    // × 的判定排在整块之前:它压在标签矩形上面,点它同时也落在标签里,
    // 先判关闭才不会变成「点 × 只是切过去」。
    if closed {
        Some(TabAction::Close(ix))
    } else if clicked {
        Some(TabAction::Switch(ix))
    } else {
        None
    }
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

    fn tab_titles(views: &[TabView<'_>]) -> Vec<String> {
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(1.0);
        let mut acc = Vec::new();
        // 同 `run_status`:面板首帧 `fade_in`,形状全被记成 Noop,必须跑两帧。
        for _ in 0..2 {
            let out = ctx.run(Default::default(), |ctx| {
                tab_bar(ctx, &crate::theme::MULLION_DARK, views);
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

    /// S3 / spec F36 验收:**只剩一个标签也画标签栏**。
    ///
    /// 标签栏的显隐是一次高度跳变,整幅终端要跟着 reflow 发 `window_change`
    /// (T4)。「只剩一个就自动隐藏」等于把这条最昂贵的重排绑在「关掉倒数第二个
    /// 标签」这个高频动作上,而用户看到的是整屏 TUI 抖一下。
    ///
    /// 自证会变红:在 `tab_bar` 开头加 `if views.len() < 2 { return None; }`。
    #[test]
    fn tab_bar_is_shown_even_with_a_single_tab() {
        let views = [TabView {
            title: "build-01",
            active: true,
            appearance: None,
        }];
        let texts = tab_titles(&views);
        assert!(
            texts.iter().any(|s| s.contains("build-01")),
            "只剩一个标签时标签栏消失了:{texts:?}"
        );
    }

    /// F62:活动标签底部那条横杠用会话的节点色 —— 且必须过 `ColorTarget::Tab`
    /// 这道闸门。没勾「标签页」的会话不该在这儿上色(用它自己的颜色画,等于
    /// 绕过用户在编辑器里做的选择)。
    ///
    /// 数「这个颜色的填充矩形」而不是数图形总数:活动标签无论有没有节点色都会
    /// 画一条横杠(退回 `accent`),总数不变,数总数这条断言会恒绿。
    ///
    /// 自证会变红:把 `one_tab` 里 `should_paint` 的 `ColorTarget::Tab` 换成
    /// `ColorTarget::ListItem`,或把 `node.unwrap_or(...)` 改成只用 `accent`。
    #[test]
    fn active_tab_uses_the_session_color() {
        use mullion_store::{ColorSpec, ColorTarget};
        let color = egui::Color32::from_rgb(0x1e, 0x88, 0xe5);

        fn appearance(targets: Vec<ColorTarget>) -> crate::ui::badge::Appearance {
            crate::ui::badge::Appearance {
                icon: None,
                color: Some(ColorSpec {
                    hex: "#1e88e5".into(),
                    apply_to: targets,
                }),
            }
        }

        fn fills_of(views: &[TabView<'_>], want: egui::Color32) -> usize {
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(1.0);
            // 显式推进时间而不是跑两帧靠墙钟:这条测试比的是**颜色**,
            // `fade_in` 半路上的不透明度会把 RGB 一起缩放(premultiplied),
            // 颜色就对不上了。同 `pane_title` 里
            // `icon_backdrop_uses_the_pane_title_target_not_list_item` 的做法。
            let mut out = None;
            for time in [0.0f64, 1.0] {
                out = Some(ctx.run(
                    egui::RawInput {
                        time: Some(time),
                        ..Default::default()
                    },
                    |ctx| {
                        tab_bar(ctx, &crate::theme::MULLION_DARK, views);
                    },
                ));
            }
            fn walk(s: &egui::Shape, want: egui::Color32) -> usize {
                match s {
                    egui::Shape::Vec(v) => v.iter().map(|s| walk(s, want)).sum(),
                    egui::Shape::Rect(r) if r.fill == want => 1,
                    _ => 0,
                }
            }
            out.unwrap()
                .shapes
                .iter()
                .map(|cs| walk(&cs.shape, want))
                .sum()
        }

        let not_tab = appearance(vec![ColorTarget::ListItem]);
        let views = [TabView {
            title: "build-01",
            active: true,
            appearance: Some(&not_tab),
        }];
        assert_eq!(
            fills_of(&views, color),
            0,
            "只勾了「会话列表」的会话,不该把这个颜色画到标签上"
        );

        let tab = appearance(vec![ColorTarget::Tab]);
        let views = [TabView {
            title: "build-01",
            active: true,
            appearance: Some(&tab),
        }];
        assert_eq!(
            fills_of(&views, color),
            1,
            "勾了「标签页」的活动标签,底部横杠应该恰好用这个颜色画一次"
        );
    }

    /// 标签栏占了中央区多少高度。`None` = 这一帧根本没画标签栏。
    fn bar_height(tabs: usize) -> f32 {
        let views: Vec<TabView<'_>> = (0..tabs)
            .map(|i| TabView {
                title: "build-01",
                active: i == 0,
                appearance: None,
            })
            .collect();
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(1.0);
        let mut h = 0.0;
        for _ in 0..2 {
            let _ = ctx.run(Default::default(), |ctx| {
                tab_bar(ctx, &crate::theme::MULLION_DARK, &views);
                h = ctx.screen_rect().height() - ctx.available_rect().height();
            });
        }
        h
    }

    /// **T4 链路的入口**:标签栏一旦出现就吃掉中央区的高度,而且**高度恒定**。
    ///
    /// 两条断言各挡一件事:
    /// 1. **占位**:`central_px` 是 `compute_geoms` → `apply_geometry` →
    ///    `window_change` 这条链的源头。标签栏不占位就意味着它画在终端上面 ——
    ///    遮住第一行,还吃掉那一行的指针事件(T8 变体)。
    /// 2. **恒高**:launcher 态(零标签,栏里只有 `+`)与有标签时必须一样高。
    ///    实测过 `exact_height` 在 egui 0.30 只是**下限**、内容更高会把面板撑开
    ///    (零标签时内容自然高 24 < 30,靠 `exact_height` 顶到 30;有标签时恰好
    ///    30)。也就是说这条承诺全靠 `exact_height` 那个数比内容大——标签内容
    ///    哪天长高一点(加个状态点、换个字号),有标签时会撑高而 launcher 态不会,
    ///    于是「连上第一台机」凭空多一次高度跳变 = 整幅终端 reflow。
    ///
    /// 自证会变红:把 `exact_height(tab_bar_px())` 改成 `exact_height(0.0)`
    /// (第 2 条红),或把 `TopBottomPanel` 换成不参与面板空间分配的 `Area`
    /// (第 1 条红)。
    #[test]
    fn tab_bar_takes_a_constant_slice_of_the_central_rect() {
        let one = bar_height(1);
        assert!(
            one >= tab_bar_px(),
            "标签栏只占了 {one} 点,至少该占 {}(没占位 = 它盖在终端上面)",
            tab_bar_px()
        );
        for tabs in [0usize, 2, 8] {
            let h = bar_height(tabs);
            assert!(
                (h - one).abs() < 0.5,
                "{tabs} 个标签时标签栏高 {h},1 个标签时高 {one} —— 高度一变\
                 整幅终端就要 reflow 发 window_change(T4)"
            );
        }
    }

    /// F100:标签栏、每个标签、每个关闭按钮、`+` 都要登记 —— 走查里
    /// 「第 2 个标签的 × 太小」这类反馈标不到就只能用嘴描述。
    ///
    /// 自证会变红:注释掉 `tab_bar` / `one_tab` 里任一句 `annotate::mark`。
    #[test]
    fn annotate_mode_registers_the_tab_bar_and_each_tab() {
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(1.0);
        annotate::toggle(&ctx);
        let views = [
            TabView {
                title: "build-01",
                active: true,
                appearance: None,
            },
            TabView {
                title: "build-02",
                active: false,
                appearance: None,
            },
        ];
        let mut paths = Vec::new();
        for _ in 0..2 {
            let _ = ctx.run(Default::default(), |ctx| {
                tab_bar(ctx, &crate::theme::MULLION_DARK, &views);
                paths = annotate::spot_paths(ctx);
            });
        }
        for want in [
            "标签栏",
            "标签栏/新建",
            "标签栏/标签 1",
            "标签栏/标签 2",
            "标签栏/标签 2/关闭",
        ] {
            assert!(
                paths.iter().any(|p| p == want),
                "「{want}」没登记:{paths:?}"
            );
        }
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
