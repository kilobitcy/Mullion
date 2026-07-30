//! 布局预设按钮组(F82)。**画在菜单栏那一行、水平居中**,不再是独立的一栏。
//!
//! 交互模型:**一排平铺 7 个纯图标按钮**,第一个是单屏,始终全部可见。
//! 按钮上没有任何文字 —— 每个布局长什么样由图标本身表达,文字说明走 tooltip
//! (`Preset::tooltip`)。**没有快捷键**(用户明确排期到后续切片),全靠鼠标点。
//!
//! 为什么不再独立占一栏:布局按钮是这个程序最常点的东西,单独一栏 48px 换来的
//! 只是「更好找」,而代价是终端永久少一行多。挪进菜单栏空着的中段既省高度,
//! 也和设计稿(`Mullion.dc.html` 的 34px 菜单行 + 居中按钮组)一致。

use crate::shell::workspace::{icon_cells, Preset};
use crate::theme::{self, Theme};

// --- 尺寸常量。**全部是逻辑点,不是物理像素** ---
//
// 项目里「px」有两种语义,判断依据是数据来源、不是变量名:egui 面板层(本文件、
// `chrome.rs` 的菜单栏/状态栏)的设计 token 直接当逻辑点喂给 egui,不除
// `pixels_per_point()`,因为它们的源头(`Mullion.dc.html` 原型)本就是 CSS px,
// 该随系统缩放一起放大;而布局树 `geom.rs` 层(`TITLE_BAR_PX`/`GAP_PX`/
// `PaneGeom.term_px`)算出来的是真物理像素,跨层喂给 egui(如 `pane_title.rs` 的
// `fixed_pos`)才需要 `/ ppp`。这两层弄反一次:125%/150% 缩放下(Windows 最常见
// 设置)按钮组会撑破菜单栏高度。

/// 图标框尺寸(设计稿 22×16)。按钮里剩下的都是内边距。
const ICON_W: f32 = 22.0;
const ICON_H: f32 = 16.0;
/// 图标内格子之间的间隙。格子各自向内缩一半,相邻两格合起来正好是这个值。
const CELL_GAP: f32 = 2.0;
/// 图标四周到按钮边缘的内边距。
const BTN_PAD: f32 = 3.0;
/// 相邻按钮之间的间隙。
const BTN_GAP: f32 = 4.0;
/// 按钮到凹槽容器边缘的内边距。
const GROUP_PAD: f32 = 3.0;
/// 按钮组左边至少要离菜单项这么远 —— 窗口窄到居中位置会压上菜单项时的兜底。
const MIN_GAP: f32 = 12.0;

const CELL_ROUNDING: f32 = 1.5;
const BTN_ROUNDING: f32 = 6.0;
const GROUP_ROUNDING: f32 = 8.0;

fn button_w() -> f32 {
    ICON_W + BTN_PAD * 2.0
}

fn button_h() -> f32 {
    ICON_H + BTN_PAD * 2.0
}

/// 整个凹槽容器要占多大。`n` = 按钮个数。
pub fn group_size(n: usize) -> egui::Vec2 {
    let n = n as f32;
    egui::vec2(
        n * button_w() + (n - 1.0).max(0.0) * BTN_GAP + GROUP_PAD * 2.0,
        button_h() + GROUP_PAD * 2.0,
    )
}

/// 第 `i` 个按钮在凹槽里的矩形。
///
/// 自己算而不是靠 egui 的 `item_spacing`:`menu::bar` 内部会 `set_menu_style`
/// 改掉 spacing,依赖它就等于把按钮间距交给 egui 的菜单样式决定,而这几个
/// 数值是设计稿冻结的。自己算的另一个好处是纯函数、能单测。
pub fn button_rect(group: egui::Rect, i: usize) -> egui::Rect {
    let x = group.left() + GROUP_PAD + i as f32 * (button_w() + BTN_GAP);
    egui::Rect::from_min_size(
        egui::pos2(x, group.top() + GROUP_PAD),
        egui::vec2(button_w(), button_h()),
    )
}

/// 第 `i` 个按钮的 egui id。显式给而不用自动 id:自动 id 依赖调用顺序,
/// 而测试要靠 `Context::read_response` 按索引查按钮是否画出来、画在哪。
pub fn button_id(i: usize) -> egui::Id {
    egui::Id::new(("mullion_preset_button", i))
}

/// 把按钮组的左边缘推到整栏正中,需要在当前笔位置之后再空多少。
///
/// 居中的是**整条菜单栏**、不是菜单项右边的剩余空间 —— 后者会让按钮组随菜单
/// 文字长度左右漂移。窗口窄到居中位置会压上菜单项时,退化成紧贴菜单项右侧
/// (`min_gap`);再窄下去按钮组会被栏右边裁掉,那是可接受的降级(拉宽窗口即可),
/// 但**绝不能**让它盖住菜单项 —— 那会让「会话/配置/关于」点不开。
pub fn centering_space(
    bar_left: f32,
    bar_w: f32,
    cursor_x: f32,
    group_w: f32,
    min_gap: f32,
) -> f32 {
    let want_left = bar_left + bar_w / 2.0 - group_w / 2.0;
    (want_left - cursor_x).max(min_gap)
}

/// 在**当前 ui**(菜单栏那一行)里画按钮组,返回用户这一帧点中的预设。
///
/// `current` 是当前生效的预设(用来画选中态)。`None` 表示当前布局不对应任何
/// 预设(手动关掉一个 pane、或 B2-b 的拖拽会造成这种状态),此时全都不高亮。
pub fn show_in(ui: &mut egui::Ui, t: &Theme, current: Option<Preset>) -> Option<Preset> {
    let size = group_size(Preset::ALL.len());
    let bar = ui.max_rect();
    let space = centering_space(bar.left(), bar.width(), ui.cursor().min.x, size.x, MIN_GAP);
    ui.add_space(space);
    let (_, group) = ui.allocate_space(size);
    ui.painter().rect(
        group,
        GROUP_ROUNDING,
        theme::c32(t.sunken_bg),
        theme::stroke(t),
    );

    let mut clicked = None;
    for (i, p) in Preset::ALL.into_iter().enumerate() {
        let rect = button_rect(group, i);
        let resp = ui
            .interact(rect, button_id(i), egui::Sense::click())
            .on_hover_text(p.tooltip());
        let on = current == Some(p);
        // 常态格子用 fg_faint 而不是设计稿的白 16%(叠出来约 #3a3c42):16px 的
        // 图标在 125% 缩放下只有 20 物理像素高,那个亮度分不出「上下分」和
        // 「左右分」。有意偏离,换可辨认度。
        let (btn_bg, cell_fg) = if on {
            (theme::c32(t.accent), theme::c32(t.accent_fg))
        } else if resp.hovered() {
            (theme::c32(t.panel_head), theme::c32(t.fg))
        } else {
            (egui::Color32::TRANSPARENT, theme::c32(t.fg_faint))
        };
        ui.painter().rect_filled(rect, BTN_ROUNDING, btn_bg);

        let icon = rect.shrink(BTN_PAD);
        for c in icon_cells(p) {
            let cell = egui::Rect::from_min_size(
                icon.min + egui::vec2(c[0] * icon.width(), c[1] * icon.height()),
                egui::vec2(c[2] * icon.width(), c[3] * icon.height()),
            )
            .shrink(CELL_GAP / 2.0);
            ui.painter().rect_filled(cell, CELL_ROUNDING, cell_fg);
        }
        if resp.clicked() {
            clicked = Some(p);
        }
    }
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个按钮都得有非空 tooltip:按钮是纯图标,tooltip 是唯一的文字说明,
    /// 空了用户就只能靠猜。
    #[test]
    fn every_preset_has_a_tooltip() {
        for p in Preset::ALL {
            assert!(!p.tooltip().is_empty(), "{p:?} 没有提示");
        }
    }

    /// 按钮从左到右平铺、互不重叠、且全部落在凹槽容器内(留出 GROUP_PAD)。
    ///
    /// 破坏性验证:把 `button_rect` 里的 `+ BTN_GAP` 去掉,「互不重叠」那条
    /// 仍过(正好相接),但把 `button_w()` 改大 1px 后立刻红。
    #[test]
    fn button_rects_tile_the_group_left_to_right() {
        let n = Preset::ALL.len();
        let size = group_size(n);
        let group = egui::Rect::from_min_size(egui::pos2(100.0, 10.0), size);
        let rects: Vec<egui::Rect> = (0..n).map(|i| button_rect(group, i)).collect();
        for (i, r) in rects.iter().enumerate() {
            assert!(
                group.contains_rect(*r),
                "第 {i} 个按钮跑出了凹槽: {r:?} 不在 {group:?} 内"
            );
        }
        for w in rects.windows(2) {
            assert!(
                w[0].right() <= w[1].left(),
                "按钮重叠: {:?} 与 {:?}",
                w[0],
                w[1]
            );
            assert!(w[0].top() == w[1].top(), "按钮不在同一行");
        }
        // 组宽必须刚好包住最后一个按钮 + 右侧内边距,不多不少 —— 多了居中会偏。
        assert!(
            (rects[n - 1].right() + GROUP_PAD - group.right()).abs() < 1e-3,
            "group_size 与 button_rect 对不上:最后一个按钮右边缘 {} vs 组右边缘 {}",
            rects[n - 1].right(),
            group.right()
        );
    }

    /// 宽栏:按钮组的中心落在整栏中心上。
    ///
    /// 破坏性验证:把 `centering_space` 里的 `bar_left + bar_w / 2.0` 改成
    /// `cursor_x + (bar_w - cursor_x) / 2.0`(即「居中于剩余空间」),本测试红。
    #[test]
    fn centering_space_puts_the_group_at_the_bar_center() {
        let (bar_left, bar_w, group_w) = (0.0, 1000.0, 226.0);
        let cursor_x = 200.0; // 菜单项占掉了左边 200
        let space = centering_space(bar_left, bar_w, cursor_x, group_w, MIN_GAP);
        let group_center = cursor_x + space + group_w / 2.0;
        assert!(
            (group_center - (bar_left + bar_w / 2.0)).abs() < 1e-3,
            "组中心 {group_center} 应落在栏中心 {}",
            bar_left + bar_w / 2.0
        );
    }

    /// 窄栏:居中位置会压上菜单项时,退化成 `min_gap`,**绝不返回负值**。
    ///
    /// 负值会让按钮组盖住「会话/配置/关于」——菜单点不开,而用户唯一的补救
    /// 手段(拉宽窗口)也在菜单栏上。
    ///
    /// 破坏性验证:把 `.max(min_gap)` 删掉,本测试红。
    #[test]
    fn centering_space_never_overlaps_the_menu_items_on_a_narrow_bar() {
        // 菜单项已占到 300,栏只有 400 宽,组要 226 —— 居中位置在 87,远在菜单项左边。
        let space = centering_space(0.0, 400.0, 300.0, 226.0, MIN_GAP);
        assert_eq!(space, MIN_GAP, "窄栏应退化成最小间距,不得为负");
        assert!(space > 0.0);
    }

    /// 按钮 id 按索引区分:同一个 id 会让 egui 把 7 个按钮当成一个,
    /// 点最后一个会触发第一个的动作。
    #[test]
    fn button_ids_are_distinct_per_index() {
        let ids: Vec<egui::Id> = (0..Preset::ALL.len()).map(button_id).collect();
        for (i, a) in ids.iter().enumerate() {
            for (j, b) in ids.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "第 {i} 和第 {j} 个按钮 id 相同");
                }
            }
        }
    }
}
