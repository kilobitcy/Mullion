//! F122:标签属性弹窗 —— 改名 + 配色。
//!
//! **改的是这个标签自己**(`Tab::title_override` / `Tab::color_override`),
//! 一行 store 都不写:会话管理器左栏那条、pane 标题条、状态栏一律不受影响。
//! 覆盖不进 F37 布局快照 —— 关窗口即丢是设计(设计文档 D2)。
//!
//! **这个弹窗仍要登记进 `app.rs::modal_open` 的 `Modal` 枚举** —— 否则里面
//! 敲的字会漏给远端 shell(T8)。但**不再进 `touched_store`**:它不写 store,
//! 登记进去只会让每次改名白跑一次外观全表重算。

use crate::shell::tabs::TabId;
use mullion_term::snapshot::Rgb;

/// 弹窗的编辑缓冲。
pub struct TabPropsDraft {
    pub tab_id: TabId,
    pub name: String,
    /// `None` = 不配颜色(退回会话色,没有会话色就用主题强调色)。
    pub color: Option<egui::Color32>,
}

/// 用户在弹窗里按下的东西。
#[derive(Debug, Clone, PartialEq)]
pub enum TabPropsAction {
    Save {
        tab_id: TabId,
        name: String,
        color: Option<Rgb>,
    },
    Cancel,
}

/// 画标签属性弹窗。`draft` 是唯一的真值来源:`None` = 弹窗关着。
/// 返回本帧用户按下的东西(保存 / 取消),`None` = 还在编辑。
pub fn show(
    ctx: &egui::Context,
    t: &crate::theme::Theme,
    draft: &mut Option<TabPropsDraft>,
) -> Option<TabPropsAction> {
    let d = draft.as_mut()?;
    let mut action = None;
    let mut close = false;
    egui::Window::new("标签属性")
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            crate::ui::annotate::mark(ui.ctx(), "标签属性".to_string(), ui.max_rect());
            ui.horizontal(|ui| {
                ui.label("名称");
                ui.add(
                    egui::TextEdit::singleline(&mut d.name)
                        .desired_width(crate::ui::metrics::FIELD_W_M),
                );
            });
            ui.add_space(crate::ui::metrics::SP_S);
            ui.horizontal(|ui| {
                ui.label("颜色");
                let mut c = d.color.unwrap_or(crate::theme::c32(t.accent));
                if ui.color_edit_button_srgba(&mut c).changed() {
                    d.color = Some(c);
                }
                if ui
                    .button("清除")
                    .on_hover_text("退回会话自己配的颜色")
                    .clicked()
                {
                    d.color = None;
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("保存").clicked() {
                    action = Some(TabPropsAction::Save {
                        tab_id: d.tab_id,
                        name: d.name.clone(),
                        color: d.color.map(|c| Rgb::new(c.r(), c.g(), c.b())),
                    });
                    close = true;
                }
                if ui.button("取消").clicked() {
                    action = Some(TabPropsAction::Cancel);
                    close = true;
                }
            });
        });
    if close {
        *draft = None;
    }
    action
}
