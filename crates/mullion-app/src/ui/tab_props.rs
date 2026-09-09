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

/// `egui::Color32` → `Rgb`。抽成函数是因为它有**两处调用点**(`show` 里
/// 保存时算一遍,F239 的 `is_dirty` 又要拿它跟已存的覆盖色比一遍)——
/// 两份各写一遍的话,迟早有一处漂,症状是「颜色明明改了却判不脏」。
fn to_rgb(c: Option<egui::Color32>) -> Option<Rgb> {
    c.map(|c| Rgb::new(c.r(), c.g(), c.b()))
}

/// F239:草稿相对这个标签**当前**的两个字段脏不脏。
///
/// 走快照比对而不是手工打脏标记(F37 的教训):改了又改回来,不算脏。
///
/// **`original_name` 要传 `Tab::display_title()`,不是 `title_override`**:
/// 弹窗打开那一刻,`TabPropsDraft::name` 就是拿 `display_title()` 填的
/// (标签没配过名字时它等于 `Tab::title`,不是空串)。拿 `title_override`
/// (`None` 时等于空串)去比的话,任何一个从没改过名字的标签一打开这个
/// 弹窗就会被判成「脏」——纯 `is_dirty` 单测测不出来,只有跑一遍
/// 「打开就没动、点外面」的完整场景才会现形。
pub fn is_dirty(d: &TabPropsDraft, original_name: &str, color_override: Option<Rgb>) -> bool {
    d.name.trim() != original_name || to_rgb(d.color) != color_override
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
                        color: to_rgb(d.color),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(name: &str, color: Option<egui::Color32>) -> TabPropsDraft {
        TabPropsDraft {
            tab_id: TabId(0),
            name: name.into(),
            color,
        }
    }

    /// 刚打开、什么都没动 —— 不脏。
    #[test]
    fn an_untouched_draft_is_not_dirty() {
        let d = draft("生产", Some(egui::Color32::RED));
        assert!(!is_dirty(&d, "生产", Some(Rgb::new(255, 0, 0))));
    }

    /// 改了名字 —— 脏。
    #[test]
    fn a_changed_name_is_dirty() {
        let d = draft("测试", None);
        assert!(is_dirty(&d, "生产", None));
    }

    /// 改了颜色 —— 脏。
    #[test]
    fn a_changed_color_is_dirty() {
        let d = draft("生产", Some(egui::Color32::RED));
        assert!(is_dirty(&d, "生产", None));
    }

    /// 改了又改回原样 —— 不脏。这是快照比对相对手工脏标记的全部价值:
    /// 手工标记只会「碰过就标脏」,改回原样也摘不掉。
    ///
    /// 自证会变红:把 `to_rgb(d.color) != color_override` 改成恒 `false`。
    #[test]
    fn reverting_back_to_the_original_is_not_dirty() {
        let mut d = draft("生产", Some(egui::Color32::RED));
        d.name = "改一下".into();
        d.color = Some(egui::Color32::BLUE);
        assert!(is_dirty(&d, "生产", Some(Rgb::new(255, 0, 0))));
        d.name = "生产".into();
        d.color = Some(egui::Color32::RED);
        assert!(!is_dirty(&d, "生产", Some(Rgb::new(255, 0, 0))));
    }

    /// 没配过名字的标签(`title_override` 是 `None`,草稿拿 `display_title()`
    /// 填,等于 `Tab::title`)一打开这个弹窗,不该被当场判成脏。
    #[test]
    fn a_tab_that_never_had_a_name_override_opens_clean() {
        let d = draft("会话A", None);
        assert!(!is_dirty(&d, "会话A", None));
    }
}
