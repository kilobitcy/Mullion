//! 菜单栏 + 状态栏。
use super::UiState;

pub fn top_menu(ctx: &egui::Context, ui_state: &mut UiState, connected: bool) {
    egui::TopBottomPanel::top("menu").show(ctx, |ui| {
        egui::menu::bar(ui, |ui| {
            ui.menu_button("对话", |ui| {
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
                ui.add_enabled(false, egui::Button::new("(切片 B 实现)"));
            });
            ui.menu_button("配置", |ui| {
                ui.add_enabled(false, egui::Button::new("(切片 C:字体等)"));
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

/// `last_error`(F3 落盘失败等)必须总有个展示位:它可能是在会话管理器/编辑器
/// 都已关闭之后才产生的(如主机密钥确认后 `ConnectOk` 顺手关掉了会话管理器),
/// 那两处的 `last_error` 渲染此时根本不会被调用到(复核 A4)。状态栏常驻,
/// 不受那两个弹窗开关状态影响,兜底展示。
pub fn status_bar(ctx: &egui::Context, status: &str, last_error: Option<&str>) {
    egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.label(status);
            if let Some(err) = last_error {
                ui.separator();
                ui.colored_label(egui::Color32::RED, err);
            }
        });
    });
}
