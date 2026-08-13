//! 工具栏布局预设(F82)与套用预设时的重排计划(§5)。纯函数,零 IO。
//!
//! 「套用预设」是**声明式**的:结果只取决于目标预设和当前 pane 的几何顺序,
//! 与用户点按钮的历史路径无关。1→4→2 和 1→2 落到同一棵树。

use mullion_core::layout::{compute_rects, Dir, Node, PaneId, Rect};

use super::PaneStatus;

/// 工具栏上的布局预设。一排平铺,全部可见(§3)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    /// 1 屏满窗。工具栏第一个按钮,也是「刚连上、只有一个 pane」这个状态的
    /// `current_preset` 初始值。
    Single,
    TwoLeftRight,
    TwoTopBottom,
    /// 左边一块通高,右边上下分。左右**等宽**——「大」指的是高度
    /// (面积因此是右侧两块各自的两倍)。
    ThreeBigLeft,
    /// 右边一块通高,左边上下分。左右**等宽**,同 `ThreeBigLeft`。
    ThreeBigRight,
    /// 三个等宽竖条。
    ThreeColumns,
    FourGrid,
}

impl Preset {
    /// 全部变体,**同时就是工具栏按钮的绘制顺序**(F82):7 个布局一排平铺、
    /// 始终全部可见,第一个是单屏。
    ///
    /// 一个常量兼两职是有意的:工具栏要覆盖所有布局,「全部变体」与「按钮列表」
    /// 内容必然相同,拆成两个常量只会让它们悄悄漂移。顺序稳定是硬要求 ——
    /// 用户靠肌肉记忆点按钮位置,顺序一变就会点错布局,而点错的代价是真的
    /// 关掉一个 pane。
    pub const ALL: [Preset; 7] = [
        Preset::Single,
        Preset::TwoLeftRight,
        Preset::TwoTopBottom,
        Preset::ThreeBigLeft,
        Preset::ThreeBigRight,
        Preset::ThreeColumns,
        Preset::FourGrid,
    ];

    /// 这个预设要几个 pane。
    pub fn pane_count(self) -> usize {
        match self {
            Preset::Single => 1,
            Preset::TwoLeftRight | Preset::TwoTopBottom => 2,
            Preset::ThreeBigLeft | Preset::ThreeBigRight | Preset::ThreeColumns => 3,
            Preset::FourGrid => 4,
        }
    }

    /// 鼠标悬停提示。按钮是**纯图标**(F82,按钮上没有任何文字),所以这是每个
    /// 布局唯一的文字说明 —— 必须自己把几何讲清楚,不能只写个名字。
    pub fn tooltip(self) -> &'static str {
        match self {
            Preset::Single => "单屏,一块占满窗口",
            Preset::TwoLeftRight => "两屏,左右并排",
            Preset::TwoTopBottom => "两屏,上下堆叠",
            Preset::ThreeBigLeft => "三屏,左右等宽;左边一块通高,右边上下分",
            Preset::ThreeBigRight => "三屏,左右等宽;右边一块通高,左边上下分",
            Preset::ThreeColumns => "三屏,三个等宽竖条",
            Preset::FourGrid => "四屏,2×2 网格",
        }
    }
}

/// 按钮图标里那几个小方块的位置,归一化成 `0.0..=1.0` 的 `[x, y, w, h]`,
/// 按几何顺序排(与 `preset_tree` 的叶子顺序一致)。
///
/// **复用 `preset_tree` + `compute_rects` 算出来,不另立一张图标几何表**:图标
/// 画的就是这个预设的真实布局。另写一份的话,改了实际几何(比如三屏刚从
/// 2/3 : 1/3 改成等宽)图标会继续骗人 —— 而纯图标按钮的图标是用户判断
/// 「点哪个」的全部依据,骗人的代价是点错布局、真的关掉一个 pane。
///
/// 基数取 1200:所有预设的切分比例(1/2、1/3)在 1200 上都是整数,
/// `compute_rects` 的整数运算不引入偏差(否则「三等分」的图标会有一格差 1px)。
pub fn icon_cells(preset: Preset) -> Vec<[f32; 4]> {
    const BASE: u16 = 1200;
    let ids: Vec<PaneId> = (1..=preset.pane_count() as u32).map(PaneId).collect();
    let area = Rect {
        col: 0,
        row: 0,
        cols: BASE,
        rows: BASE,
    };
    let n = f32::from(BASE);
    compute_rects(&preset_tree(preset, &ids), area)
        .into_iter()
        .map(|(_, r)| {
            [
                f32::from(r.col) / n,
                f32::from(r.row) / n,
                f32::from(r.cols) / n,
                f32::from(r.rows) / n,
            ]
        })
        .collect()
}

fn split(dir: Dir, ratio: f32, a: Node, b: Node) -> Node {
    Node::Split {
        dir,
        ratio,
        a: Box::new(a),
        b: Box::new(b),
    }
}

/// 用给定的 pane id 搭出预设布局树(§5.1)。
///
/// # Panics
/// `ids.len()` 必须等于 `preset.pane_count()`。调用方(`Workspace::apply_preset`)
/// 保证这点;数量对不上是编程错误,不是运行时输入错误,故直接 panic 而不是返回
/// Result —— 静默补一个 pane 出来只会让布局错得更难查。
pub fn preset_tree(preset: Preset, ids: &[PaneId]) -> Node {
    assert_eq!(
        ids.len(),
        preset.pane_count(),
        "预设 {preset:?} 需要 {} 个 pane,给了 {}",
        preset.pane_count(),
        ids.len()
    );
    let l = |i: usize| Node::Leaf(ids[i]);
    let h = Dir::Horizontal;
    let v = Dir::Vertical;
    match preset {
        Preset::Single => l(0),
        Preset::TwoLeftRight => split(h, 0.5, l(0), l(1)),
        Preset::TwoTopBottom => split(v, 0.5, l(0), l(1)),
        // 左右等宽,「大」只体现在高度:大块通高,另一侧对半切上下两块。
        Preset::ThreeBigLeft => split(h, 0.5, l(0), split(v, 0.5, l(1), l(2))),
        Preset::ThreeBigRight => split(h, 0.5, split(v, 0.5, l(0), l(1)), l(2)),
        // 先切掉左边 1/3,剩下的 2/3 再对半 → 三个等宽竖条。
        Preset::ThreeColumns => split(h, 1.0 / 3.0, l(0), split(h, 0.5, l(1), l(2))),
        Preset::FourGrid => split(v, 0.5, split(h, 0.5, l(0), l(1)), split(h, 0.5, l(2), l(3))),
    }
}

/// 套用预设的重排计划(§5.2/§5.3)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetPlan {
    /// 按几何顺序保留下来的现有 pane。它们依次填进新树的前若干个叶子位。
    pub keep: Vec<PaneId>,
    /// 还差几个 pane,需要新开 channel。它们排在 `keep` 之后填满剩余叶子位。
    pub spawn: usize,
    /// 要关掉的 pane,按关闭顺序(先断开的、后活着的)。
    pub close: Vec<PaneId>,
}

/// 算出套用 `preset` 需要保留 / 新建 / 关闭哪些 pane。
///
/// `current` 必须按**几何顺序**给(`mullion_core::layout::leaves` 的返回顺序),
/// 不然重排后 pane 会互相换位,用户会觉得内容"跳"了。`current` 里的 `PaneId`
/// 还必须互不重复 —— `keep` 是用 `!close.contains(id)` 过滤出来的,重复 id 会
/// 让这条过滤行为不可预测。
pub fn plan_preset(preset: Preset, current: &[(PaneId, PaneStatus)]) -> PresetPlan {
    plan_for_count(preset.pane_count(), current)
}

/// [`plan_preset`] 的按数量版本。F37 恢复任意树形状时用得到 —— 恢复出来的
/// 叶子数是**文件里存的**,不对应任何一个 `Preset`。
///
/// 保留/新建/关闭的取舍逻辑与预设完全一致,故意共用一份:两处各写一遍的话,
/// 「减屏时先关已断开的」这类取舍迟早会在其中一处走样。
pub fn plan_for_count(want: usize, current: &[(PaneId, PaneStatus)]) -> PresetPlan {
    if current.len() <= want {
        return PresetPlan {
            keep: current.iter().map(|(id, _)| *id).collect(),
            spawn: want - current.len(),
            close: Vec::new(),
        };
    }
    // 减屏:先关已断开的,再关活着的,同类里按几何逆序(右下角先走)。
    let extra = current.len() - want;
    let by_status = |want_status: PaneStatus| {
        current
            .iter()
            .rev()
            .filter(move |(_, s)| *s == want_status)
            .map(|(id, _)| *id)
    };
    let close: Vec<PaneId> = by_status(PaneStatus::Disconnected)
        .chain(by_status(PaneStatus::Live))
        .take(extra)
        .collect();
    PresetPlan {
        keep: current
            .iter()
            .map(|(id, _)| *id)
            .filter(|id| !close.contains(id))
            .collect(),
        spawn: 0,
        close,
    }
}

/// 焦点 pane 被关掉后落到哪(§5.3):几何顺序第一个存活 pane。
///
/// `survivors` 为空时原样返回 —— 最后一个 pane 不可关(core 的 `close_pane`
/// 已经保证),真到了这一步说明上游有 bug,不该在这里静默造一个 id 出来。
pub fn next_focus(focus: PaneId, survivors: &[PaneId]) -> PaneId {
    if survivors.contains(&focus) {
        focus
    } else {
        survivors.first().copied().unwrap_or(focus)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_core::layout::leaves;

    const AREA: Rect = Rect {
        col: 0,
        row: 0,
        cols: 1200,
        rows: 600,
    };

    fn ids(n: u32) -> Vec<PaneId> {
        (1..=n).map(PaneId).collect()
    }

    fn assert_tiles(tree: &Node, want: usize) {
        let rects = compute_rects(tree, AREA);
        assert_eq!(rects.len(), want, "叶子数不对");
        let covered: u64 = rects
            .iter()
            .map(|(_, r)| u64::from(r.cols) * u64::from(r.rows))
            .sum();
        assert_eq!(
            covered,
            u64::from(AREA.cols) * u64::from(AREA.rows),
            "未拼满"
        );
    }

    #[test]
    fn every_preset_tiles_exactly_f30() {
        for p in Preset::ALL {
            assert_tiles(&preset_tree(p, &ids(p.pane_count() as u32)), p.pane_count());
        }
    }

    #[test]
    fn preset_pane_counts_are_what_the_names_say() {
        assert_eq!(Preset::Single.pane_count(), 1);
        assert_eq!(Preset::TwoLeftRight.pane_count(), 2);
        assert_eq!(Preset::TwoTopBottom.pane_count(), 2);
        assert_eq!(Preset::ThreeBigLeft.pane_count(), 3);
        assert_eq!(Preset::ThreeBigRight.pane_count(), 3);
        assert_eq!(Preset::ThreeColumns.pane_count(), 3);
        assert_eq!(Preset::FourGrid.pane_count(), 4);
    }

    /// 工具栏就是 7 个按钮、第一个是单屏(实机验收定的形态)。
    ///
    /// 破坏性验证:从 `Preset::ALL` 里去掉 `Single`(顺带把长度改成 6),
    /// 两条断言都红。
    #[test]
    fn toolbar_is_seven_buttons_starting_with_single() {
        assert_eq!(Preset::ALL.len(), 7, "工具栏平铺 7 个布局按钮");
        assert_eq!(Preset::ALL[0], Preset::Single, "第一个按钮是单屏");
    }

    /// 图标格子必须是**真实布局的投影**:数量对、拼满整个图标框、无零尺寸格。
    /// 图标是纯图标按钮的全部视觉信息,画错等于骗用户点错布局。
    ///
    /// 破坏性验证:把 `compute_rects` 的结果换成「每格都占满整框」(偷懒画法),
    /// 实测 `TwoLeftRight 图标格子未拼满,合计 2` 红。
    ///
    /// 注意这条守不住「比例自洽但画错」——照设计稿手写一张 `1.6fr 1fr` 的表
    /// 也能拼满(实测这条仍绿)。那种情况由
    /// `icon_cells_show_every_three_pane_preset_as_equal_width` 兜。
    #[test]
    fn icon_cells_are_a_projection_of_the_real_layout() {
        for p in Preset::ALL {
            let cells = icon_cells(p);
            assert_eq!(cells.len(), p.pane_count(), "{p:?} 图标格子数应等于屏数");
            let area: f32 = cells.iter().map(|c| c[2] * c[3]).sum();
            assert!(
                (area - 1.0).abs() < 1e-4,
                "{p:?} 图标格子未拼满,合计 {area}"
            );
            for c in &cells {
                assert!(c[2] > 0.0 && c[3] > 0.0, "{p:?} 有零尺寸格子: {c:?}");
            }
        }
    }

    /// 三屏的三个预设在图标里也必须**每格等宽** —— 用户就是因为图标/文案暗示
    /// 「左边更宽」才提的那个偏差(v0.1.12)。图标几何复用 `preset_tree`,
    /// 所以这条同时守着「图标没有绕过布局树自己画一套」。
    ///
    /// 破坏性验证:把 `preset_tree` 里 `ThreeBigLeft` 的 `split(h, 0.5, ...)`
    /// 改回 `2.0 / 3.0`,本测试红(图标层)+ `three_big_left_...` 红(布局层)。
    #[test]
    fn icon_cells_show_every_three_pane_preset_as_equal_width() {
        for p in [
            Preset::ThreeBigLeft,
            Preset::ThreeBigRight,
            Preset::ThreeColumns,
        ] {
            let widths: Vec<f32> = icon_cells(p).iter().map(|c| c[2]).collect();
            let first = widths[0];
            for w in &widths {
                assert!(
                    (w - first).abs() < 1e-4,
                    "{p:?} 图标格子宽度不一致: {widths:?}"
                );
            }
        }
    }

    /// 三等分必须是**三个竖条**,不能是「左半 + 右半再对半」那种 1/2:1/4:1/4。
    #[test]
    fn three_columns_are_equal_width() {
        let rects = compute_rects(&preset_tree(Preset::ThreeColumns, &ids(3)), AREA);
        let widths: Vec<u16> = rects.iter().map(|(_, r)| r.cols).collect();
        assert_eq!(widths, vec![400, 400, 400]);
    }

    /// 左满高:左右**等宽**,左边那块通高,右边对半切上下两块。
    ///
    /// 等宽这条是实机验收提的(v0.1.12 是 2/3 : 1/3,用户要的「大」只指高度)。
    /// 破坏性验证:把 `preset_tree` 里的 `split(h, 0.5, ...)` 改回 `2.0 / 3.0`,
    /// 第一条等宽断言变红。
    #[test]
    fn three_big_left_is_equal_width_with_a_full_height_left_block() {
        let rects = compute_rects(&preset_tree(Preset::ThreeBigLeft, &ids(3)), AREA);
        let widths: Vec<u16> = rects.iter().map(|(_, r)| r.cols).collect();
        assert_eq!(widths, vec![600, 600, 600], "三块等宽:大只在高度上");
        assert_eq!(rects[0].1.rows, 600, "左块通高");
        assert_eq!(rects[1].1.rows, 300);
        assert_eq!(rects[2].1.rows, 300);
    }

    /// 右满高:`ThreeBigLeft` 的镜像。单独一条是因为两者的树形不是简单对称
    /// (通高的那块在 `split` 的另一侧),改一个漏改另一个不会被上面那条抓到。
    #[test]
    fn three_big_right_is_equal_width_with_a_full_height_right_block() {
        let rects = compute_rects(&preset_tree(Preset::ThreeBigRight, &ids(3)), AREA);
        let widths: Vec<u16> = rects.iter().map(|(_, r)| r.cols).collect();
        assert_eq!(widths, vec![600, 600, 600], "三块等宽:大只在高度上");
        assert_eq!(rects[0].1.rows, 300);
        assert_eq!(rects[1].1.rows, 300);
        assert_eq!(rects[2].1.rows, 600, "右块通高");
    }

    #[test]
    fn preset_tree_fills_leaves_in_geometric_order() {
        let tree = preset_tree(Preset::FourGrid, &ids(4));
        assert_eq!(
            leaves(&tree),
            vec![PaneId(1), PaneId(2), PaneId(3), PaneId(4)]
        );
    }

    #[test]
    fn growing_keeps_existing_panes_and_spawns_the_rest() {
        let plan = plan_preset(Preset::FourGrid, &[(PaneId(1), PaneStatus::Live)]);
        assert_eq!(plan.keep, vec![PaneId(1)]);
        assert_eq!(plan.spawn, 3);
        assert!(plan.close.is_empty());
    }

    #[test]
    fn same_count_keeps_everyone() {
        let cur = [(PaneId(1), PaneStatus::Live), (PaneId(2), PaneStatus::Live)];
        let plan = plan_preset(Preset::TwoTopBottom, &cur);
        assert_eq!(plan.keep, vec![PaneId(1), PaneId(2)]);
        assert_eq!(plan.spawn, 0);
        assert!(plan.close.is_empty(), "换子布局不该重开任何 channel");
    }

    /// §5.3:减屏优先关**已断开**的 pane —— 用户多半就是想把死掉的那块清掉,
    /// 关掉还活着的反而丢工作。
    #[test]
    fn close_prefers_disconnected_panes() {
        let cur = [
            (PaneId(1), PaneStatus::Live),
            (PaneId(2), PaneStatus::Disconnected),
            (PaneId(3), PaneStatus::Live),
            (PaneId(4), PaneStatus::Disconnected),
        ];
        let plan = plan_preset(Preset::TwoLeftRight, &cur);
        assert_eq!(
            plan.close,
            vec![PaneId(4), PaneId(2)],
            "两个断开的先走(几何逆序)"
        );
        assert_eq!(plan.keep, vec![PaneId(1), PaneId(3)]);
        assert_eq!(plan.spawn, 0);
    }

    /// 断开的不够关时,继续按几何逆序关活着的。
    #[test]
    fn close_falls_back_to_live_panes_in_reverse_order() {
        let cur = [
            (PaneId(1), PaneStatus::Live),
            (PaneId(2), PaneStatus::Live),
            (PaneId(3), PaneStatus::Disconnected),
            (PaneId(4), PaneStatus::Live),
        ];
        let plan = plan_preset(Preset::Single, &cur);
        assert_eq!(plan.close, vec![PaneId(3), PaneId(4), PaneId(2)]);
        assert_eq!(plan.keep, vec![PaneId(1)]);
    }

    #[test]
    fn focus_survives_when_its_pane_survives() {
        assert_eq!(next_focus(PaneId(3), &[PaneId(1), PaneId(3)]), PaneId(3));
    }

    /// §5.3:焦点 pane 被关掉 → 落到几何顺序第一个存活 pane。
    #[test]
    fn focus_falls_back_to_first_survivor() {
        assert_eq!(next_focus(PaneId(9), &[PaneId(2), PaneId(5)]), PaneId(2));
    }

    /// 声明式:路径不影响结果。
    #[test]
    fn applying_a_preset_is_path_independent() {
        let direct = plan_preset(Preset::TwoLeftRight, &[(PaneId(1), PaneStatus::Live)]);
        let via_four = plan_preset(
            Preset::TwoLeftRight,
            &[
                (PaneId(1), PaneStatus::Live),
                (PaneId(2), PaneStatus::Live),
                (PaneId(3), PaneStatus::Live),
            ],
        );
        // 起点不同,但两次的结果都是「1 号留在首位」。
        assert_eq!(direct.keep.first(), via_four.keep.first());
        assert_eq!(direct.keep.len() + direct.spawn, 2);
        assert_eq!(via_four.keep.len() + via_four.spawn, 2);
    }
}
