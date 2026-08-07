//! 菜单栏(含居中的布局预设按钮组)+ 状态栏。
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
    egui::TopBottomPanel::top("menu")
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
pub fn status_bar(
    ctx: &egui::Context,
    t: &Theme,
    panes: usize,
    connected: bool,
    last_error: Option<&str>,
    automation: Option<&str>,
) {
    let (left, right) = status_text(panes, connected);
    egui::TopBottomPanel::bottom("status")
        .frame(
            egui::Frame::none()
                .fill(theme::c32(t.bar_status))
                .inner_margin(egui::Margin::symmetric(8.0, 2.0))
                .stroke(theme::stroke(t)),
        )
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
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
                    ui.colored_label(theme::c32(t.fg_faint), right);
                });
            });
        });
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
}
