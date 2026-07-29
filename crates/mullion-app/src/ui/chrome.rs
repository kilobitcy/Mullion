//! 菜单栏 + 状态栏。
use super::UiState;
use crate::theme::{self, Theme};

pub fn top_menu(ctx: &egui::Context, t: &Theme, ui_state: &mut UiState, connected: bool) {
    // 菜单栏与状态栏底色不同(§2.1),Visuals::panel_fill 只有一个值,
    // 所以各自带 Frame。栏高由 inner_margin 决定(目标 30px),精确值人眼验。
    egui::TopBottomPanel::top("menu")
        .frame(
            egui::Frame::none()
                .fill(theme::c32(t.bar_menu))
                .inner_margin(egui::Margin::symmetric(6.0, 3.0))
                .stroke(theme::stroke(t)),
        )
        .show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("会话", |ui| {
                    if ui.button("会话管理器").clicked() {
                        ui_state.session_manager_open = true;
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
                ui.menu_button("分屏", |ui| {
                    ui.add_enabled(false, egui::Button::new("用工具栏的布局按钮切换"));
                    // F83:标题条占 32px,关掉能换回一行终端。切换后行数会变,
                    // 必须走 apply_geometry 发 window_change(T4),故只置意图。
                    if ui.button("显示 / 隐藏 pane 标题条").clicked() {
                        ui_state.toggle_title_bars = true;
                        ui.close_menu();
                    }
                    ui.add_enabled(false, egui::Button::new("(快捷键 · 后续切片)"));
                });
                ui.menu_button("配置", |ui| {
                    ui.add_enabled(false, egui::Button::new("(F84 设置 · 后续切片)"));
                });
                ui.menu_button("关于", |ui| {
                    if ui.button("关于 Mullion").clicked() {
                        ui_state.about_open = true;
                        ui.close_menu();
                    }
                });
            });
        });
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
