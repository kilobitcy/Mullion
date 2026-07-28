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

pub fn status_bar(ctx: &egui::Context, status: &str) {
    egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
        ui.label(status);
    });
}
