//! 布局预设工具栏(F82)。菜单栏之下、终端之上,固定 48px 高。
//!
//! 交互模型学 `Mullion Standalone.html` 原型:两段式 —— 先按屏数分组,
//! 组内是该屏数下的子布局。**没有快捷键**(用户明确排期到后续切片),
//! 全靠鼠标点。

use crate::shell::workspace::Preset;
use crate::theme::{self, Theme};

/// 工具栏高度。
///
/// **这是逻辑点,不是物理像素**——项目里「px」有两种语义,判断依据是数据来源,
/// 不是变量名带不带 `_PX`:egui 面板层(本文件、`chrome.rs` 的菜单栏/状态栏)
/// 的设计 token 直接当逻辑点喂给 egui,不除 `pixels_per_point()`,因为它们的
/// 源头(`Mullion Standalone.html` 原型)本就是 CSS px,该随系统缩放放大;
/// 而布局树 `geom.rs` 层(`TITLE_BAR_PX`/`GAP_PX`/`PaneGeom.term_px`)算出来的
/// 是真物理像素,跨层喂给 egui(如 `pane_title.rs` 的 `fixed_pos`)才需要
/// `/ ppp`。这两层弄反一次:125%/150% 缩放下(Windows 最常见设置)工具栏内容
/// 会撑破固定高度。
pub const TOOLBAR_PX: f32 = 48.0;

/// 按屏数把预设分组,组内保持 `Preset::ALL` 的顺序。
///
/// 顺序稳定是硬要求:用户靠肌肉记忆点按钮位置,顺序变了就会点错布局,
/// 而点错的代价是真的关掉一个 pane。
pub fn preset_groups() -> Vec<(usize, Vec<Preset>)> {
    let mut groups: Vec<(usize, Vec<Preset>)> = Vec::new();
    for p in Preset::ALL {
        match groups.iter_mut().find(|(n, _)| *n == p.group()) {
            Some((_, v)) => v.push(p),
            None => groups.push((p.group(), vec![p])),
        }
    }
    groups
}

/// 画工具栏,返回用户这一帧点中的预设。
///
/// `current` 是当前生效的预设(用来画选中态)。`None` 表示当前布局不是任何
/// 预设(B2-b 的手动拖拽会造成这种状态),此时所有按钮都不高亮。
pub fn show(ctx: &egui::Context, t: &Theme, current: Option<Preset>) -> Option<Preset> {
    let mut clicked = None;
    egui::TopBottomPanel::top("toolbar")
        .exact_height(TOOLBAR_PX)
        .frame(
            egui::Frame::none()
                .fill(theme::c32(t.bar_tool))
                .inner_margin(egui::Margin::symmetric(8.0, 6.0))
                .stroke(theme::stroke(t)),
        )
        .show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                for (gi, (_, presets)) in preset_groups().into_iter().enumerate() {
                    if gi > 0 {
                        ui.separator();
                    }
                    for p in presets {
                        let on = current == Some(p);
                        let btn = egui::Button::new(
                            egui::RichText::new(p.label()).color(theme::c32(if on {
                                t.accent_fg
                            } else {
                                t.fg_muted
                            })),
                        )
                        .fill(theme::c32(if on {
                            t.accent
                        } else {
                            t.sunken_bg
                        }));
                        if ui.add(btn).on_hover_text(p.tooltip()).clicked() {
                            clicked = Some(p);
                        }
                    }
                }
            });
        });
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 按钮必须按屏数分组、且组内保持稳定顺序 —— 用户靠肌肉记忆点位置,
    /// 顺序一变(比如哪天给 ALL 加了个预设插在中间)就点错。
    #[test]
    fn toolbar_groups_presets_by_pane_count() {
        let groups = preset_groups();
        assert_eq!(groups.len(), 4, "1/2/3/4 屏共四组");
        assert_eq!(groups[0], (1, vec![Preset::Single]));
        assert_eq!(
            groups[1],
            (2, vec![Preset::TwoLeftRight, Preset::TwoTopBottom])
        );
        assert_eq!(groups[2].1.len(), 3);
        assert_eq!(groups[3], (4, vec![Preset::FourGrid]));
    }

    /// 每个按钮都得有非空的文字和提示,否则工具栏上会出现空白方块。
    #[test]
    fn every_preset_has_a_label_and_tooltip() {
        for p in Preset::ALL {
            assert!(!p.label().is_empty(), "{p:?} 没有按钮文字");
            assert!(!p.tooltip().is_empty(), "{p:?} 没有提示");
        }
    }

    /// 分组是 ALL 的一个划分:不重不漏。
    #[test]
    fn groups_partition_all_presets() {
        let flat: Vec<Preset> = preset_groups().into_iter().flat_map(|(_, v)| v).collect();
        assert_eq!(flat.len(), Preset::ALL.len());
        for p in Preset::ALL {
            assert!(flat.contains(&p), "{p:?} 掉出了分组");
        }
    }
}
