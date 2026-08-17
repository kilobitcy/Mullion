//! E2/E3:标签属性弹窗 —— 改名 + 配色。
//!
//! **改的是会话记录本身**(`identity.name` / `appearance.color`),不是
//! 「只对这个标签生效的运行期覆盖」:那要新增一份不持久化的标题/颜色
//! 状态,与 F37 的布局持久化语义打架(关窗口存的是会话 id,不是标题)。
//!
//! **这个弹窗必须同时登记进两张表**(切片 I 的教训):
//! - `app.rs::modal_open` 的 `Modal` 枚举 —— 否则里面敲的字会漏给远端 shell(T8)
//! - `app.rs::touched_store` —— 否则改了颜色要重启才看得见(F61/F62 的
//!   `AppearanceCache` 只在 store 变更后 rebuild,陷阱 T3)

use mullion_store::{ColorSpec, ColorTarget, SessionId};

// `hex_of`/`COLOR_TARGET_LABELS`:复用 `session_manager::fields` 里的那份
// (已提到 `pub(crate)`)——原先这里各自独立留了一份格式/文案相同的拷贝,
// 复核指出两份迟早会各自漂移,已收敛成一份。
use crate::ui::session_manager::fields::{hex_of, COLOR_TARGET_LABELS};

/// 弹窗的编辑缓冲。
pub struct TabPropsDraft {
    pub session_id: SessionId,
    pub name: String,
    /// `None` = 不配颜色(退回主题 accent)。
    pub color: Option<egui::Color32>,
    pub targets: Vec<ColorTarget>,
}

/// 用户在弹窗里按下的东西。
#[derive(Debug, Clone, PartialEq)]
pub enum TabPropsAction {
    Save {
        session_id: SessionId,
        name: String,
        color: Option<ColorSpec>,
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
                if ui.button("清除").clicked() {
                    d.color = None;
                }
            });
            ui.add_space(crate::ui::metrics::SP_S);
            ui.label("应用到");
            // 与 `session_manager::fields::appearance` 的「作用于」共用同一份
            // target/文案表(`COLOR_TARGET_LABELS`)。
            for (target, label) in COLOR_TARGET_LABELS {
                let mut on = d.targets.contains(&target);
                if ui.checkbox(&mut on, label).changed() {
                    if on {
                        d.targets.push(target);
                    } else {
                        d.targets.retain(|x| *x != target);
                    }
                }
            }
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("保存").clicked() {
                    action = Some(TabPropsAction::Save {
                        session_id: d.session_id,
                        name: d.name.clone(),
                        color: d.color.map(|c| ColorSpec {
                            hex: hex_of(c),
                            apply_to: d.targets.clone(),
                        }),
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

// `hex_of` 的格式(六位、小写)已经在 `session_manager::fields` 里被
// `a_colour_survives_the_round_trip_between_the_picker_and_the_hex_text`
// 测过——`hex_of` 现在是复用那份的同一个函数,不是格式相同的另一份拷贝,
// 这里不再重复一条只测格式的用例。
