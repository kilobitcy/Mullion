//! 工具栏布局预设(F82)与套用预设时的重排计划(§5)。纯函数,零 IO。
//!
//! 「套用预设」是**声明式**的:结果只取决于目标预设和当前 pane 的几何顺序,
//! 与用户点按钮的历史路径无关。1→4→2 和 1→2 落到同一棵树。

use mullion_core::layout::{Dir, Node, PaneId};

use super::PaneStatus;

/// 工具栏上的布局预设。分两段:先选屏数,再选该屏数下的子布局(§3)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    /// 1 屏满窗。**原型没有这个按钮**(分屏后回不到单屏),我们补上。
    Single,
    TwoLeftRight,
    TwoTopBottom,
    /// 左边一大块,右边上下分。
    ThreeBigLeft,
    /// 右边一大块,左边上下分。
    ThreeBigRight,
    /// 三个等宽竖条。
    ThreeColumns,
    FourGrid,
}

impl Preset {
    /// 工具栏按钮的绘制顺序(§3):先 1/2/3/4 屏,再是各屏数下的子布局。
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

    /// 按钮所属的屏数分组。当前与 `pane_count` 同值,分开写是因为语义不同:
    /// 一个是 UI 分组,一个是要开几条 channel。
    pub fn group(self) -> usize {
        self.pane_count()
    }

    /// 按钮上的字形 + 文字(F82)。字形用几何方块,不依赖字体的图标集。
    pub fn label(self) -> &'static str {
        match self {
            Preset::Single => "▢ 1 屏",
            Preset::TwoLeftRight => "▥ 左右分",
            Preset::TwoTopBottom => "▤ 上下分",
            Preset::ThreeBigLeft => "⊟ 左大",
            Preset::ThreeBigRight => "⊞ 右大",
            Preset::ThreeColumns => "▦ 三等分",
            Preset::FourGrid => "▩ 2×2",
        }
    }

    /// 鼠标悬停提示。
    pub fn tooltip(self) -> &'static str {
        match self {
            Preset::Single => "单屏满窗",
            Preset::TwoLeftRight => "两屏,左右并排",
            Preset::TwoTopBottom => "两屏,上下堆叠",
            Preset::ThreeBigLeft => "三屏,左边一大块,右边上下分",
            Preset::ThreeBigRight => "三屏,右边一大块,左边上下分",
            Preset::ThreeColumns => "三屏,三个等宽竖条",
            Preset::FourGrid => "四屏,2×2 网格",
        }
    }
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
        Preset::ThreeBigLeft => split(h, 2.0 / 3.0, l(0), split(v, 0.5, l(1), l(2))),
        Preset::ThreeBigRight => split(h, 1.0 / 3.0, split(v, 0.5, l(0), l(1)), l(2)),
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
    let want = preset.pane_count();
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
    use mullion_core::layout::{compute_rects, leaves, Rect};

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
    fn preset_pane_counts_match_their_group() {
        assert_eq!(Preset::Single.pane_count(), 1);
        assert_eq!(Preset::TwoLeftRight.pane_count(), 2);
        assert_eq!(Preset::TwoTopBottom.pane_count(), 2);
        assert_eq!(Preset::ThreeBigLeft.pane_count(), 3);
        assert_eq!(Preset::ThreeBigRight.pane_count(), 3);
        assert_eq!(Preset::ThreeColumns.pane_count(), 3);
        assert_eq!(Preset::FourGrid.pane_count(), 4);
        for p in Preset::ALL {
            assert_eq!(p.group(), p.pane_count(), "按钮分组就是屏数");
        }
    }

    /// 三等分必须是**三个竖条**,不能是「左半 + 右半再对半」那种 1/2:1/4:1/4。
    #[test]
    fn three_columns_are_equal_width() {
        let rects = compute_rects(&preset_tree(Preset::ThreeColumns, &ids(3)), AREA);
        let widths: Vec<u16> = rects.iter().map(|(_, r)| r.cols).collect();
        assert_eq!(widths, vec![400, 400, 400]);
    }

    /// 左大右上下:左边一整条,右边被横向切两块。
    #[test]
    fn three_big_left_geometry() {
        let rects = compute_rects(&preset_tree(Preset::ThreeBigLeft, &ids(3)), AREA);
        assert_eq!(rects[0].1.cols, 800, "左块占 2/3 宽");
        assert_eq!(rects[0].1.rows, 600, "左块通高");
        assert_eq!(rects[1].1.rows, 300);
        assert_eq!(rects[2].1.rows, 300);
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
