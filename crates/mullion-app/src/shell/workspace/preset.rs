//! 工具栏布局预设(F82)与套用预设时的重排计划(§5)。纯函数,零 IO。
//!
//! 「套用预设」是**声明式**的:结果只取决于目标预设和当前 pane 的几何顺序,
//! 与用户点按钮的历史路径无关。1→4→2 和 1→2 落到同一棵树。

use mullion_core::layout::{compute_rects, Dir, Node, PaneId, Rect};

use super::PaneStatus;

/// 工具栏上的布局预设。一排平铺,全部可见(§3)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    /// 1 屏满窗。工具栏第一个按钮,也是「刚连上、只有一个 pane」时
    /// `preset_of` 会认出的形状。
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

/// 比例相等的判定阈值。预设里出现的比例只有 0.5 和 1/3,两者相差 1/6,
/// 1e-3 远小于它 —— 既认得出 `1.0 / 3.0` 的浮点表示,又不会把用户拖出来的
/// 0.49 误判成 0.5(拖动的最小可见增量远大于 1e-3)。
const RATIO_EPS: f32 = 1e-3;

/// 当前这棵树的形状是否**正好**等于某个预设。是则返回它,用于工具栏高亮(F232)。
///
/// 这是个**派生量**,不是被保存的状态:曾经有一个 `TerminalTab::current_preset`
/// 字段影子跟踪它,而"关掉一个 pane 就无条件清空高亮"是那份影子状态唯一的
/// 更新规则 —— 于是「两屏关掉一块后其实就是单屏」这种情况下,单屏按钮不亮。
/// 从树现算就没有这类漂移:形状是什么就是什么。
///
/// 代价是拖动分隔条会熄灭高亮(比例不再是预设值)。这是对的:高亮的含义是
/// "当前形状就是这个预设",不是"上次点了哪个按钮"。
pub fn preset_of(tree: &Node) -> Option<Preset> {
    Preset::ALL.into_iter().find(|p| {
        let ids: Vec<PaneId> = (1..=p.pane_count() as u32).map(PaneId).collect();
        same_shape(tree, &preset_tree(*p, &ids))
    })
}

/// 两棵树形状是否一致:方向与比例逐层比,**叶子上的 `PaneId` 一律不看** ——
/// id 是运行期分配的,跟"这是哪个预设"无关。
fn same_shape(a: &Node, b: &Node) -> bool {
    match (a, b) {
        (Node::Leaf(_), Node::Leaf(_)) => true,
        (
            Node::Split {
                dir: d1,
                ratio: r1,
                a: a1,
                b: b1,
            },
            Node::Split {
                dir: d2,
                ratio: r2,
                a: a2,
                b: b2,
            },
        ) => d1 == d2 && (r1 - r2).abs() < RATIO_EPS && same_shape(a1, a2) && same_shape(b1, b2),
        _ => false,
    }
}

/// F241:关掉一块 pane 之后,剩下的 `n` 块该排成哪个预设。`None` = 不重排。
///
/// **无条件重排成「N 屏水平并列」**,不看关闭前是什么形状。理由分两层:
///
/// 一是几何上非治不可。三等宽竖条在二叉树里只能是
/// `split(h, 1/3, l0, split(h, 0.5, l1, l2))` —— 关掉中间或右边那块之后,
/// `close_pane` 只做兄弟顶替,**外层那个 1/3 原封不动**,剩下两块变成
/// 1/3 : 2/3。宽度不齐,而且 `preset_of` 认不出它,工具栏「两屏左右」不亮。
///
/// 二是这道规则**不能带条件**。曾经的备选是「只在关闭前正好是预设时才重排」
/// (好处是保住用户手拖出来的比例),但那让「会不会重排」取决于一件**界面上
/// 完全不可见**的事:三天前有没有拖过一次分隔条。同样的操作有时重排有时不,
/// 用户无从解释。丢掉的比例再拖一次就有;而拖过之后关一块得到的 1/3 : 2/3,
/// 是用户从来没要求过的比例。
///
/// `n > 3` 不重排:四屏只有 2×2 一种形态,不是「水平并列」,硬凑等于改语义。
/// 这条只可能来自 F37 恢复一棵手改过的 `layout.toml`(UI 里点不出 5 屏)。
pub fn layout_after_close(n: usize) -> Option<Preset> {
    match n {
        1 => Some(Preset::Single),
        2 => Some(Preset::TwoLeftRight),
        3 => Some(Preset::ThreeColumns),
        _ => None,
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
    // 减屏:按「关掉的代价」从小到大关。**穷尽 match**——加状态时这里
    // 编译报错,而不是新状态悄悄一个都关不掉(那样 close 会凑不够 extra 个,
    // 减屏静默失效)。
    fn close_priority(s: PaneStatus) -> u8 {
        match s {
            PaneStatus::Disconnected => 0, // 已经死透,先关
            PaneStatus::Reconnecting => 1, // 还有救,但没内容在动
            PaneStatus::Live => 2,         // 最后才关活的
        }
    }
    let extra = current.len() - want;
    let mut ranked: Vec<(u8, usize, PaneId)> = current
        .iter()
        .enumerate()
        .map(|(i, (id, s))| (close_priority(*s), usize::MAX - i, *id))
        .collect();
    // 同优先级里按几何逆序(右下角先走)——`usize::MAX - i` 就是逆序键。
    ranked.sort_by_key(|(p, rev, _)| (*p, *rev));
    let close: Vec<PaneId> = ranked
        .into_iter()
        .take(extra)
        .map(|(_, _, id)| id)
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

    /// F128:三态的相对顺序必须是 `Disconnected` → `Reconnecting` → `Live`。
    /// 穷尽 match 只保证「加状态时编译报错」,保证不了这三个数字没写反 ——
    /// 写反了就是减屏时**杀活的、留正在重连的**,而且悄无声息。
    ///
    /// 自证会变红:把 `close_priority` 里 `Reconnecting` 和 `Live` 的返回值对调。
    #[test]
    fn close_prefers_reconnecting_over_live() {
        let cur = [
            (PaneId(1), PaneStatus::Live),
            (PaneId(2), PaneStatus::Reconnecting),
            (PaneId(3), PaneStatus::Live),
            (PaneId(4), PaneStatus::Disconnected),
        ];
        let plan = plan_preset(Preset::TwoLeftRight, &cur);
        assert_eq!(
            plan.close,
            vec![PaneId(4), PaneId(2)],
            "先关死透的,再关重连中的,活的一个不动"
        );
        assert_eq!(plan.keep, vec![PaneId(1), PaneId(3)]);
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

    /// F232:每个预设自己的树必须被认回自己。这条是 `preset_of` 的自反性 ——
    /// 认不回来的话工具栏点完预设当场就不高亮。
    #[test]
    fn every_preset_tree_is_recognised_as_itself() {
        for p in Preset::ALL {
            assert_eq!(
                preset_of(&preset_tree(p, &ids(p.pane_count() as u32))),
                Some(p),
                "{p:?} 认不回自己"
            );
        }
    }

    /// F232:`preset_of` 只看**形状**,不看 pane id —— 树上的 id 是运行期分配的,
    /// 跟预设无关。
    ///
    /// 自证会变红:把 `same_shape` 的 `(Node::Leaf(_), Node::Leaf(_)) => true`
    /// 改成 `(Node::Leaf(a), Node::Leaf(b)) => a == b`,本条红。
    #[test]
    fn preset_of_ignores_pane_ids() {
        let tree = preset_tree(Preset::TwoLeftRight, &[PaneId(77), PaneId(9)]);
        assert_eq!(preset_of(&tree), Some(Preset::TwoLeftRight));
    }

    /// F232 的全部意义:关掉一块 pane 之后,如果剩下的形状**正好**等于某个预设,
    /// 工具栏那个按钮就该重新亮起来。旧实现在这里无条件把高亮清成 None。
    ///
    /// **注意这里调的是 core 的 `close_pane`,不是 `Workspace::close_pane`。**
    /// 本条描述的是 `preset_of` 这一层:「形状对上了就认得出」。F241 之后
    /// app 的实际行为多了一步重排(`layout_after_close`),下面第三例的
    /// `TwoTopBottom` 在真实 app 里会被重排成 `TwoLeftRight` —— 那是**上一层**
    /// 的策略,不该混进这条对 `preset_of` 的描述里。
    ///
    /// 自证会变红:让 `preset_of` 恒返回 `None`,四条 assert_eq 全红。
    #[test]
    fn closing_a_pane_can_relight_a_preset_button() {
        use mullion_core::layout::close_pane;

        // 两屏左右,关掉右边 → 单屏。
        let mut t = preset_tree(Preset::TwoLeftRight, &ids(2));
        assert!(close_pane(&mut t, PaneId(2)));
        assert_eq!(preset_of(&t), Some(Preset::Single));

        // 两屏上下,关掉下面 → 单屏。
        let mut t = preset_tree(Preset::TwoTopBottom, &ids(2));
        assert!(close_pane(&mut t, PaneId(2)));
        assert_eq!(preset_of(&t), Some(Preset::Single));

        // 左满高三屏,关掉通高的那块 → 右侧上下两块顶替 → 两屏上下。
        let mut t = preset_tree(Preset::ThreeBigLeft, &ids(3));
        assert!(close_pane(&mut t, PaneId(1)));
        assert_eq!(preset_of(&t), Some(Preset::TwoTopBottom));

        // 左满高三屏,关掉右上那块 → 左通高 + 右一块 → 两屏左右。
        let mut t = preset_tree(Preset::ThreeBigLeft, &ids(3));
        assert!(close_pane(&mut t, PaneId(2)));
        assert_eq!(preset_of(&t), Some(Preset::TwoLeftRight));
    }

    /// F232 的另一半:剩下的形状**不**等于任何预设时必须老实返回 `None`,
    /// 不许"就近凑一个"。凑错了会让用户以为当前是某个预设,再点一次同名按钮
    /// 反而重排整棵树、真的关掉 pane。
    ///
    /// 同上条:这里调的是 core 的 `close_pane`,描述的是 `preset_of` 这一层。
    /// F241 之后这两棵树在真实 app 里都到不了 —— `Workspace::close_pane` 会
    /// 先把它们重排成水平并列。「就近凑」的禁令本身仍然成立:`preset_of` 只
    /// 认精确形状,重排是**改树**而不是放宽识别。
    ///
    /// 这两例是从 `preset_tree` 原文推出来的,不是猜的:
    /// - `ThreeColumns` = `split(h, 1/3, l0, split(h, 0.5, l1, l2))`,关掉中间
    ///   那块后外层 ratio 仍是 1/3,不等于 `TwoLeftRight` 的 0.5。
    /// - `FourGrid` 外层是**竖**分(上下两行),关掉右下后剩「上行左右分 + 下行
    ///   通宽」,横竖方向对不上任何三屏预设。
    ///
    /// 自证会变红:把 `RATIO_EPS` 从 1e-3 放大到 0.2,第一条红。
    #[test]
    fn a_shape_that_matches_no_preset_reports_none() {
        use mullion_core::layout::close_pane;

        let mut t = preset_tree(Preset::ThreeColumns, &ids(3));
        assert!(close_pane(&mut t, PaneId(2)));
        assert_eq!(preset_of(&t), None, "1/3 : 2/3 不是任何预设");

        let mut t = preset_tree(Preset::FourGrid, &ids(4));
        assert!(close_pane(&mut t, PaneId(4)));
        assert_eq!(preset_of(&t), None, "上行左右分 + 下行通宽,不是任何预设");
    }

    /// F241:关完之后剩下几块,就排成几屏水平并列。逐格钉住 —— 这张表写错
    /// 一格就是「关掉一块之后布局变成另一种东西」,而且看起来像是随机的。
    ///
    /// 自证会变红:把 `3 => Some(Preset::ThreeColumns)` 改成
    /// `Some(Preset::ThreeBigLeft)`(第三条红);把 `_ => None` 改成
    /// `_ => Some(Preset::FourGrid)`(最后一条红)。
    #[test]
    fn what_is_left_after_a_close_is_always_n_panes_side_by_side() {
        assert_eq!(layout_after_close(1), Some(Preset::Single));
        assert_eq!(layout_after_close(2), Some(Preset::TwoLeftRight));
        assert_eq!(layout_after_close(3), Some(Preset::ThreeColumns));
        // 四屏没有「水平并列」形态,不重排 —— 只可能来自 F37 恢复一棵手改过
        // 的 layout.toml,UI 里点不出 5 屏。
        assert_eq!(layout_after_close(4), None);
        assert_eq!(layout_after_close(9), None);
    }

    /// F241:重排是**无条件**的 —— 关闭前是不是预设、比例有没有被拖歪,
    /// 一概不看。
    ///
    /// 这条单独立是因为「有条件」和「无条件」两种实现下,拿预设树做输入的
    /// 测试**长得一模一样**:预设树本来就被 `preset_of` 认得出,两种实现都会
    /// 重排。只有喂一棵比例已经歪掉的树,两者才分得开。
    ///
    /// 自证会变红:在 `Workspace::close_pane` 的重排前面加一道
    /// `if preset_of(&before).is_some()` 的门(把关闭前的树先克隆出来判)。
    #[test]
    fn a_hand_dragged_layout_is_rearranged_on_close_just_like_a_preset_one() {
        use mullion_core::layout::close_pane;

        // 三等宽竖条被拖歪:外层 0.2、内层 0.7,`preset_of` 认不出它。
        let mut t = preset_tree(Preset::ThreeColumns, &ids(3));
        let Node::Split { ratio, b, .. } = &mut t else {
            panic!("三等宽竖条的根必须是 Split");
        };
        *ratio = 0.2;
        let Node::Split { ratio: inner, .. } = b.as_mut() else {
            panic!("三等宽竖条的右子必须是 Split");
        };
        *inner = 0.7;
        assert_eq!(preset_of(&t), None, "前提:这棵树已经不是任何预设");

        // 走一遍 `Workspace::close_pane` 的两步:兄弟顶替 → 按剩余块数重排。
        assert!(close_pane(&mut t, PaneId(2)));
        let survivors = leaves(&t);
        let p = layout_after_close(survivors.len()).expect("剩两块该有目标预设");
        t = preset_tree(p, &survivors);

        assert_eq!(preset_of(&t), Some(Preset::TwoLeftRight));
        let widths: Vec<u16> = compute_rects(&t, AREA).iter().map(|(_, r)| r.cols).collect();
        assert_eq!(widths, vec![600, 600], "拖歪过的比例照样被冲成等宽");
    }

    /// F241:重排**不改变 pane 的先后顺序** —— `preset_tree` 按几何顺序填叶子,
    /// 而喂进去的正是 `leaves` 的返回顺序。顺序一乱,用户看到的是内容互相换位
    /// (§5.2 警告过的那种「跳」)。
    ///
    /// 自证会变红:把重排那句改成 `preset_tree(p, &{ let mut v = survivors
    /// .clone(); v.reverse(); v })`。
    #[test]
    fn rearranging_after_a_close_keeps_the_survivors_in_geometric_order() {
        use mullion_core::layout::close_pane;

        // 2×2 关掉右下 → 剩左上、右上、左下 → 三等宽竖条的左/中/右。
        let mut t = preset_tree(Preset::FourGrid, &ids(4));
        assert!(close_pane(&mut t, PaneId(4)));
        let survivors = leaves(&t);
        assert_eq!(survivors, vec![PaneId(1), PaneId(2), PaneId(3)]);
        let p = layout_after_close(survivors.len()).expect("剩三块该有目标预设");
        t = preset_tree(p, &survivors);

        assert_eq!(preset_of(&t), Some(Preset::ThreeColumns));
        assert_eq!(
            leaves(&t),
            vec![PaneId(1), PaneId(2), PaneId(3)],
            "存活 pane 的先后顺序不该被重排打乱"
        );
    }

    /// F232:用户拖了分隔条 → 比例不再是预设值 → 高亮熄灭。这是**有意**的:
    /// 高亮的含义是"当前形状就是这个预设",不是"上次点了这个按钮"。
    #[test]
    fn dragging_a_splitter_extinguishes_the_highlight() {
        let mut t = preset_tree(Preset::TwoLeftRight, &ids(2));
        if let Node::Split { ratio, .. } = &mut t {
            *ratio = 0.7;
        } else {
            panic!("两屏预设的根必须是 Split");
        }
        assert_eq!(preset_of(&t), None);
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
