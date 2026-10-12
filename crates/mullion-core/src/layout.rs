//! 布局树:自研分屏的几何核心。零 UI/IO,可纯单测(F30–F33)。
//!
//! 语义约定:
//! - `Dir::Horizontal` = 左右并排(沿列方向切分),`Dir::Vertical` = 上下堆叠(沿行方向切分)。
//! - 分隔条不占独立格:两个子 pane 严丝合缝拼满父区域(F30「拼满不重叠」);
//!   渲染层在边界 overlay 分隔线,不从布局里扣格子。

/// pane 的稳定标识。布局树只搬运它,不解释它。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PaneId(pub u32);

/// 布局矩形。左上角 `(col, row)`,尺寸 `cols × rows`。
///
/// **单位由调用方定义**:纯网格场景传格数;app 的分屏(B2-a)传**像素**,因为
/// 32px 标题条和 1px 分隔线都不是格的整数倍。二分几何与单位无关,`u16` 的
/// 上限 65535 对 4K 宽度(3840)绰绰有余。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub col: u16,
    pub row: u16,
    pub cols: u16,
    pub rows: u16,
}

/// 分屏方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// 左右并排,沿列方向切分。
    Horizontal,
    /// 上下堆叠,沿行方向切分。
    Vertical,
}

/// 焦点移动方向(F33 几何法切焦点)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusDir {
    Up,
    Down,
    Left,
    Right,
}

/// 布局树节点:要么是一个叶子 pane,要么是一个二分 Split。
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Leaf(PaneId),
    Split {
        dir: Dir,
        /// 第一个子 `a` 占父区域主轴的比例,取值区间 `(0.0, 1.0)`。
        ratio: f32,
        a: Box<Node>,
        b: Box<Node>,
    },
}

/// pane 的最小尺寸,resize 夹紧下界(F32)。
pub const MIN_PANE_COLS: u16 = 2;
pub const MIN_PANE_ROWS: u16 = 1;

/// 计算每个叶子 pane 的矩形。结果严丝合缝拼满 `area`、两两不重叠(F30)。
pub fn compute_rects(root: &Node, area: Rect) -> Vec<(PaneId, Rect)> {
    let mut out = Vec::new();
    fill(root, area, &mut out);
    out
}

fn fill(node: &Node, area: Rect, out: &mut Vec<(PaneId, Rect)>) {
    match node {
        Node::Leaf(id) => out.push((*id, area)),
        Node::Split { dir, ratio, a, b } => {
            let (ra, rb) = split_area(*dir, *ratio, area);
            fill(a, ra, out);
            fill(b, rb, out);
        }
    }
}

fn split_area(dir: Dir, ratio: f32, area: Rect) -> (Rect, Rect) {
    match dir {
        Dir::Horizontal => {
            let a_cols = split_len(area.cols, ratio);
            (
                Rect {
                    col: area.col,
                    row: area.row,
                    cols: a_cols,
                    rows: area.rows,
                },
                Rect {
                    col: area.col + a_cols,
                    row: area.row,
                    cols: area.cols - a_cols,
                    rows: area.rows,
                },
            )
        }
        Dir::Vertical => {
            let a_rows = split_len(area.rows, ratio);
            (
                Rect {
                    col: area.col,
                    row: area.row,
                    cols: area.cols,
                    rows: a_rows,
                },
                Rect {
                    col: area.col,
                    row: area.row + a_rows,
                    cols: area.cols,
                    rows: area.rows - a_rows,
                },
            )
        }
    }
}

/// 主轴长度按比例切分,返回第一个子的长度(夹到 `[0, total]`)。
fn split_len(total: u16, ratio: f32) -> u16 {
    let a = (f32::from(total) * ratio).round();
    a.clamp(0.0, f32::from(total)) as u16
}

/// 把 `target` 叶子换成一个 Split:原 pane 落在 `a`,新 pane `new_id` 落在 `b`(F30)。
///
/// 返回是否成功;`target` 不在树中时返回 `false` 且树不变。
/// `ratio` 由调用方给定(预设布局用固定值),这里**不夹紧** —— 夹紧是拖分隔条(F32)
/// 的事,预设的 0.333/0.5/0.667 本来就是合法值,在这儿夹一道只会让预设几何变形。
pub fn split_pane(root: &mut Node, target: PaneId, new_id: PaneId, dir: Dir, ratio: f32) -> bool {
    match root {
        Node::Leaf(id) if *id == target => {
            *root = Node::Split {
                dir,
                ratio,
                a: Box::new(Node::Leaf(target)),
                b: Box::new(Node::Leaf(new_id)),
            };
            true
        }
        Node::Leaf(_) => false,
        Node::Split { a, b, .. } => {
            split_pane(a, target, new_id, dir, ratio) || split_pane(b, target, new_id, dir, ratio)
        }
    }
}

/// 按几何顺序(DFS,`a` 先 `b` 后)列出所有叶子 pane。
///
/// 与 [`compute_rects`] 的返回顺序**保证一致**,但不需要 `area`:预设重排只关心
/// 「谁在前谁在后」,不关心具体像素(§5.2)。
pub fn leaves(root: &Node) -> Vec<PaneId> {
    let mut out = Vec::new();
    collect_leaves(root, &mut out);
    out
}

fn collect_leaves(node: &Node, out: &mut Vec<PaneId>) {
    match node {
        Node::Leaf(id) => out.push(*id),
        Node::Split { a, b, .. } => {
            collect_leaves(a, out);
            collect_leaves(b, out);
        }
    }
}

/// 关闭 `target` pane:其父 Split 由兄弟节点顶替(F31)。
///
/// 返回是否关闭成功。以下两种情况返回 `false` 且树不变:
/// - `root` 本身就是唯一叶子(最后一个 pane 不可关);
/// - `target` 不在树中。
pub fn close_pane(root: &mut Node, target: PaneId) -> bool {
    if matches!(root, Node::Leaf(_)) {
        return false; // 最后一个 pane 不可关
    }
    close_in(root, target)
}

fn close_in(node: &mut Node, target: PaneId) -> bool {
    let (a_is_target, b_is_target) = match node {
        Node::Split { a, b, .. } => (
            matches!(a.as_ref(), Node::Leaf(id) if *id == target),
            matches!(b.as_ref(), Node::Leaf(id) if *id == target),
        ),
        Node::Leaf(_) => return false,
    };

    if a_is_target {
        promote_sibling(node, Sibling::B);
        return true;
    }
    if b_is_target {
        promote_sibling(node, Sibling::A);
        return true;
    }

    match node {
        Node::Split { a, b, .. } => close_in(a, target) || close_in(b, target),
        Node::Leaf(_) => false,
    }
}

enum Sibling {
    A,
    B,
}

/// 用指定一侧的子节点整体顶替 `node`(丢弃另一侧与 Split 本身)。
fn promote_sibling(node: &mut Node, keep: Sibling) {
    if let Node::Split { a, b, .. } = node {
        let sib = match keep {
            Sibling::A => std::mem::replace(a.as_mut(), Node::Leaf(PaneId(0))),
            Sibling::B => std::mem::replace(b.as_mut(), Node::Leaf(PaneId(0))),
        };
        *node = sib;
    }
}

/// 交换两个「单元」的位置。单元 = 叶子 id 集合恰好覆盖的最小子树
/// (普通 pane = 单叶子;带抽屉的 pane = 父 + 抽屉两片叶子)。
///
/// 返回是否交换成功。以下情况返回 `false` 且树**完全不变**:
/// - 任一集合为空或含重复 id;
/// - 两集合有交集;
/// - 任一集合找不到「叶子集合(无序)恰好等于它」的子树。
///
/// 成功时两棵子树整体互换位置:各自内部结构与 ratio 原样搬走,外层各 Split 的
/// `dir`/`ratio` 不动。
///
/// 为什么不直接对调叶子 id:抽屉是挂在父 pane 下的一个 Split,只对调父叶子会让
/// 抽屉留在原地。core 不认识「抽屉」,只认「这组叶子凑成哪棵子树」。
///
/// 内部用 `PaneId(u32::MAX)` 与 `PaneId(u32::MAX - 1)` 做临时占位,真实 pane id
/// 从小到大分配,不会取到这两个值。
pub fn swap_units(root: &mut Node, x: &[PaneId], y: &[PaneId]) -> bool {
    if !valid_unit(x) || !valid_unit(y) || x.iter().any(|id| y.contains(id)) {
        return false;
    }
    // 先只读校验两组都存在,保证后面的取出/写回不会半途失败而改坏树。
    if !covers(root, x) || !covers(root, y) {
        return false;
    }
    let (mark_x, mark_y) = (PaneId(u32::MAX), PaneId(u32::MAX - 1));
    let (Some(sub_x), Some(sub_y)) = (take_subtree(root, x, mark_x), take_subtree(root, y, mark_y))
    else {
        return false; // covers 已校验过,理论不可达
    };
    // y 的子树写进 x 的占位,x 的子树写进 y 的占位。
    put_back(root, mark_x, sub_y);
    put_back(root, mark_y, sub_x);
    true
}

/// 非空且无重复。
fn valid_unit(ids: &[PaneId]) -> bool {
    !ids.is_empty() && ids.iter().enumerate().all(|(i, id)| !ids[..i].contains(id))
}

/// `node` 的叶子集合(无序)是否恰好等于 `ids`(`ids` 已保证无重复)。
fn leaf_set_is(node: &Node, ids: &[PaneId]) -> bool {
    let mut got = Vec::new();
    collect_leaves(node, &mut got);
    got.len() == ids.len() && got.iter().all(|id| ids.contains(id))
}

/// 树中是否存在叶子集合恰好等于 `ids` 的子树。
fn covers(node: &Node, ids: &[PaneId]) -> bool {
    if leaf_set_is(node, ids) {
        return true;
    }
    match node {
        Node::Split { a, b, .. } => covers(a, ids) || covers(b, ids),
        Node::Leaf(_) => false,
    }
}

/// 自顶向下找到叶子集合等于 `ids` 的子树,换成占位叶子 `mark` 并返回原子树。
/// 先判子节点再判自身,取到的是最小子树。
fn take_subtree(node: &mut Node, ids: &[PaneId], mark: PaneId) -> Option<Node> {
    if let Node::Split { a, b, .. } = node {
        if let Some(sub) = take_subtree(a, ids, mark) {
            return Some(sub);
        }
        if let Some(sub) = take_subtree(b, ids, mark) {
            return Some(sub);
        }
    }
    if leaf_set_is(node, ids) {
        return Some(std::mem::replace(node, Node::Leaf(mark)));
    }
    None
}

/// 把占位叶子 `mark` 替换成 `sub`;找到返回 `true`。
fn put_back(node: &mut Node, mark: PaneId, sub: Node) -> bool {
    match node {
        Node::Leaf(id) if *id == mark => {
            *node = sub;
            true
        }
        Node::Leaf(_) => false,
        Node::Split { a, b, .. } => {
            // sub 只会被消费一次:先试 a,失败时 sub 需要还回来,故用 Option 搬运。
            let mut slot = Some(sub);
            put_back_opt(a, mark, &mut slot) || put_back_opt(b, mark, &mut slot)
        }
    }
}

fn put_back_opt(node: &mut Node, mark: PaneId, slot: &mut Option<Node>) -> bool {
    match node {
        Node::Leaf(id) if *id == mark => {
            if let Some(sub) = slot.take() {
                *node = sub;
            }
            true
        }
        Node::Leaf(_) => false,
        Node::Split { a, b, .. } => put_back_opt(a, mark, slot) || put_back_opt(b, mark, slot),
    }
}

/// 夹紧期望比例,使 `area` 沿 `dir` 切分后两侧都 >= 最小尺寸(F32)。
pub fn clamp_ratio(dir: Dir, area: Rect, desired: f32) -> f32 {
    let (total, min) = match dir {
        Dir::Horizontal => (area.cols, MIN_PANE_COLS),
        Dir::Vertical => (area.rows, MIN_PANE_ROWS),
    };
    if total < min.saturating_mul(2) {
        return 0.5; // 放不下两个最小 pane,退化居中(过小交给渲染层处理)
    }
    let min_ratio = f32::from(min) / f32::from(total);
    desired.clamp(min_ratio, 1.0 - min_ratio)
}

/// 拖分隔条:把 `target` pane 所在 Split 的 `ratio` 设为夹紧后的期望值(F32)。
///
/// 返回实际生效的 ratio;`target` 不在树中、或根就是叶子时返回 `None`。
pub fn resize_for_pane(root: &mut Node, area: Rect, target: PaneId, desired: f32) -> Option<f32> {
    resize_walk(root, area, target, desired)
}

fn resize_walk(node: &mut Node, area: Rect, target: PaneId, desired: f32) -> Option<f32> {
    let Node::Split { dir, ratio, a, b } = node else {
        return None;
    };
    let touches = matches!(a.as_ref(), Node::Leaf(id) if *id == target)
        || matches!(b.as_ref(), Node::Leaf(id) if *id == target);
    if touches {
        let clamped = clamp_ratio(*dir, area, desired);
        *ratio = clamped;
        return Some(clamped);
    }
    let (ra, rb) = split_area(*dir, *ratio, area);
    resize_walk(a, ra, target, desired).or_else(|| resize_walk(b, rb, target, desired))
}

/// 几何法找相邻 pane(F33):只在目标方向上、且与当前 pane 投影有重叠的候选里选最近的。
/// 投影不重叠的斜对角 pane 不会被选中。
pub fn focus_neighbor(root: &Node, area: Rect, current: PaneId, dir: FocusDir) -> Option<PaneId> {
    let rects = compute_rects(root, area);
    let cur = rects.iter().find(|(id, _)| *id == current)?.1;
    rects
        .iter()
        .filter(|(id, _)| *id != current)
        .filter_map(|(id, r)| candidate_key(cur, *r, dir).map(|k| (k, *id)))
        .min_by_key(|(k, _)| *k)
        .map(|(_, id)| id)
}

/// 若 `other` 是 `cur` 在 `dir` 方向的合法候选,返回排序键(越小越近);否则 `None`。
fn candidate_key(cur: Rect, other: Rect, dir: FocusDir) -> Option<(u16, u16)> {
    let (cl, cr) = (cur.col, cur.col + cur.cols);
    let (ct, cb) = (cur.row, cur.row + cur.rows);
    let (ol, or) = (other.col, other.col + other.cols);
    let (ot, ob) = (other.row, other.row + other.rows);
    match dir {
        FocusDir::Right if ol >= cr && overlaps(ct, cb, ot, ob) => Some((ol, ot)),
        FocusDir::Left if or <= cl && overlaps(ct, cb, ot, ob) => Some((cl - or, ot)),
        FocusDir::Down if ot >= cb && overlaps(cl, cr, ol, or) => Some((ot, ol)),
        FocusDir::Up if ob <= ct && overlaps(cl, cr, ol, or) => Some((ct - ob, ol)),
        _ => None,
    }
}

/// 半开区间 `[a0, a1)` 与 `[b0, b1)` 是否重叠。
fn overlaps(a0: u16, a1: u16, b0: u16, b1: u16) -> bool {
    a0 < b1 && b0 < a1
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect {
        col: 0,
        row: 0,
        cols: 80,
        rows: 24,
    };

    fn leaf(id: u32) -> Node {
        Node::Leaf(PaneId(id))
    }
    fn hsplit(r: f32, a: Node, b: Node) -> Node {
        Node::Split {
            dir: Dir::Horizontal,
            ratio: r,
            a: Box::new(a),
            b: Box::new(b),
        }
    }
    fn vsplit(r: f32, a: Node, b: Node) -> Node {
        Node::Split {
            dir: Dir::Vertical,
            ratio: r,
            a: Box::new(a),
            b: Box::new(b),
        }
    }

    fn intersects(a: Rect, b: Rect) -> bool {
        a.col < b.col + b.cols
            && b.col < a.col + a.cols
            && a.row < b.row + b.rows
            && b.row < a.row + a.rows
    }

    /// F30 的核心断言:矩形两两不重叠,且面积和恰好拼满 area。
    fn assert_tiles_exactly(rects: &[(PaneId, Rect)], area: Rect) {
        let mut covered: u32 = 0;
        for (i, (_, r)) in rects.iter().enumerate() {
            covered += u32::from(r.cols) * u32::from(r.rows);
            for (_, other) in rects.iter().skip(i + 1) {
                assert!(!intersects(*r, *other), "矩形重叠: {r:?} vs {other:?}");
            }
        }
        assert_eq!(
            covered,
            u32::from(area.cols) * u32::from(area.rows),
            "未拼满 area"
        );
    }

    #[test]
    fn single_leaf_fills_area() {
        assert_eq!(compute_rects(&leaf(1), AREA), vec![(PaneId(1), AREA)]);
    }

    #[test]
    fn horizontal_split_geometry() {
        let rects = compute_rects(&hsplit(0.5, leaf(1), leaf(2)), AREA);
        assert_eq!(
            rects,
            vec![
                (
                    PaneId(1),
                    Rect {
                        col: 0,
                        row: 0,
                        cols: 40,
                        rows: 24
                    }
                ),
                (
                    PaneId(2),
                    Rect {
                        col: 40,
                        row: 0,
                        cols: 40,
                        rows: 24
                    }
                ),
            ]
        );
    }

    #[test]
    fn nested_split_tiles_area_without_overlap_f30() {
        // 左半是 L1,右半上下分成 L2/L3。
        let tree = hsplit(0.5, leaf(1), vsplit(0.5, leaf(2), leaf(3)));
        let rects = compute_rects(&tree, AREA);
        assert_eq!(rects.len(), 3);
        assert_tiles_exactly(&rects, AREA);
    }

    #[test]
    fn close_pane_promotes_sibling_f31() {
        let mut tree = hsplit(0.5, leaf(1), leaf(2));
        assert!(close_pane(&mut tree, PaneId(1)));
        assert_eq!(tree, leaf(2));
        assert_eq!(compute_rects(&tree, AREA), vec![(PaneId(2), AREA)]);
    }

    #[test]
    fn close_pane_nested_promotes_sibling_f31() {
        let mut tree = hsplit(0.5, leaf(1), vsplit(0.5, leaf(2), leaf(3)));
        assert!(close_pane(&mut tree, PaneId(2)));
        assert_eq!(tree, hsplit(0.5, leaf(1), leaf(3)));
    }

    #[test]
    fn last_pane_cannot_be_closed_f31() {
        let mut tree = leaf(1);
        assert!(!close_pane(&mut tree, PaneId(1)));
        assert_eq!(tree, leaf(1));
    }

    #[test]
    fn closing_unknown_pane_is_noop_f31() {
        let mut tree = hsplit(0.5, leaf(1), leaf(2));
        let before = tree.clone();
        assert!(!close_pane(&mut tree, PaneId(9)));
        assert_eq!(tree, before);
    }

    #[test]
    fn resize_clamps_high_end_to_min_size_f32() {
        let mut tree = hsplit(0.5, leaf(1), leaf(2));
        // 期望把分隔条拖到极右,b 会被夹到最小宽度。
        let r = resize_for_pane(&mut tree, AREA, PaneId(1), 0.99).unwrap();
        assert!(r < 0.99, "ratio 未被夹紧: {r}");
        for (_, rect) in &compute_rects(&tree, AREA) {
            assert!(rect.cols >= MIN_PANE_COLS, "pane 宽度低于最小值: {rect:?}");
        }
    }

    #[test]
    fn resize_clamps_low_end_to_min_size_f32() {
        let mut tree = hsplit(0.5, leaf(1), leaf(2));
        let r = resize_for_pane(&mut tree, AREA, PaneId(1), 0.0).unwrap();
        assert!(r > 0.0);
        for (_, rect) in &compute_rects(&tree, AREA) {
            assert!(rect.cols >= MIN_PANE_COLS, "pane 宽度低于最小值: {rect:?}");
        }
    }

    /// 2x2 网格:上排 L1/L2,下排 L3/L4。
    fn grid_2x2() -> Node {
        vsplit(
            0.5,
            hsplit(0.5, leaf(1), leaf(2)), // 上:L1 左上, L2 右上
            hsplit(0.5, leaf(3), leaf(4)), // 下:L3 左下, L4 右下
        )
    }

    #[test]
    fn focus_moves_orthogonally_not_diagonally_f33() {
        let g = grid_2x2();
        // 左上向右→右上(不是右下),向下→左下(不是右下)。
        assert_eq!(
            focus_neighbor(&g, AREA, PaneId(1), FocusDir::Right),
            Some(PaneId(2))
        );
        assert_eq!(
            focus_neighbor(&g, AREA, PaneId(1), FocusDir::Down),
            Some(PaneId(3))
        );
        // 左上无左邻/上邻。
        assert_eq!(focus_neighbor(&g, AREA, PaneId(1), FocusDir::Left), None);
        assert_eq!(focus_neighbor(&g, AREA, PaneId(1), FocusDir::Up), None);
        // 右下向上→右上,向左→左下。
        assert_eq!(
            focus_neighbor(&g, AREA, PaneId(4), FocusDir::Up),
            Some(PaneId(2))
        );
        assert_eq!(
            focus_neighbor(&g, AREA, PaneId(4), FocusDir::Left),
            Some(PaneId(3))
        );
    }

    #[test]
    fn split_pane_replaces_leaf_f30() {
        let mut tree = leaf(1);
        assert!(split_pane(
            &mut tree,
            PaneId(1),
            PaneId(2),
            Dir::Horizontal,
            0.5
        ));
        assert_eq!(tree, hsplit(0.5, leaf(1), leaf(2)));
        let rects = compute_rects(&tree, AREA);
        assert_eq!(rects.len(), 2);
        assert_tiles_exactly(&rects, AREA);
    }

    #[test]
    fn split_pane_targets_nested_leaf_f30() {
        // 深层叶子也要能切,否则 2 屏切成 3 屏时只能动最外层。
        let mut tree = hsplit(0.5, leaf(1), leaf(2));
        assert!(split_pane(
            &mut tree,
            PaneId(2),
            PaneId(3),
            Dir::Vertical,
            0.5
        ));
        assert_eq!(tree, hsplit(0.5, leaf(1), vsplit(0.5, leaf(2), leaf(3))));
        assert_tiles_exactly(&compute_rects(&tree, AREA), AREA);
    }

    #[test]
    fn split_pane_unknown_target_is_noop_f30() {
        let mut tree = hsplit(0.5, leaf(1), leaf(2));
        let before = tree.clone();
        assert!(!split_pane(
            &mut tree,
            PaneId(9),
            PaneId(3),
            Dir::Vertical,
            0.5
        ));
        assert_eq!(tree, before);
    }

    /// `leaves` 的顺序必须和 `compute_rects` 一致 —— 预设重排(§5.2)靠这个顺序
    /// 把现有 pane 填进新树的叶子位;两者不一致,套预设后 pane 会互相换位。
    #[test]
    fn leaves_order_matches_compute_rects() {
        let tree = grid_2x2();
        let from_rects: Vec<PaneId> = compute_rects(&tree, AREA)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(leaves(&tree), from_rects);
        assert_eq!(
            leaves(&tree),
            vec![PaneId(1), PaneId(2), PaneId(3), PaneId(4)]
        );
    }

    fn ids(v: &[u32]) -> Vec<PaneId> {
        v.iter().map(|&i| PaneId(i)).collect()
    }

    /// 自证会变红:若实现只对调叶子 id 的位置而不保留 ratio,或把外层 ratio 一并搬走,
    /// 期望树里的 0.3 / 0.6 就对不上。
    #[test]
    fn swapping_two_leaves_keeps_shape_and_ratios() {
        let mut t = hsplit(0.3, leaf(1), vsplit(0.6, leaf(2), leaf(3)));
        assert!(swap_units(&mut t, &ids(&[1]), &ids(&[3])));
        assert_eq!(t, hsplit(0.3, leaf(3), vsplit(0.6, leaf(2), leaf(1))));
    }

    /// 自证会变红:若实现退化成「只对调父叶子 id」,抽屉叶子 9 会留在原地,
    /// 得到 hsplit(.5, leaf(2), vsplit(.8, leaf(1), leaf(9))),与期望不等。
    #[test]
    fn a_unit_with_a_drawer_moves_as_one_subtree() {
        let mut t = hsplit(0.5, leaf(1), vsplit(0.8, leaf(2), leaf(9)));
        assert!(swap_units(&mut t, &ids(&[1]), &ids(&[2, 9])));
        assert_eq!(t, hsplit(0.5, vsplit(0.8, leaf(2), leaf(9)), leaf(1)));
    }

    /// 自证会变红:若任一非法输入(空集/相交/凑不成子树/不存在)没有在改树之前
    /// 校验就返回,树会被改动或返回 true。
    #[test]
    fn an_unmatched_or_overlapping_unit_leaves_the_tree_untouched() {
        let orig = hsplit(0.5, leaf(1), vsplit(0.5, leaf(2), leaf(3)));
        let cases: [(&[u32], &[u32]); 5] = [
            (&[1], &[1]),
            (&[1], &[1, 2]),
            (&[1, 3], &[2]),
            (&[], &[2]),
            (&[7], &[2]),
        ];
        for (x, y) in cases {
            let mut t = orig.clone();
            assert!(!swap_units(&mut t, &ids(x), &ids(y)), "{x:?} {y:?}");
            assert_eq!(t, orig, "{x:?} {y:?}");
        }
    }

    /// 自证会变红:若交换时改动了外层 Split 的 dir/ratio,矩形集合就会变;
    /// 若没有真正换属主(空操作),属主 id 不会对调。
    #[test]
    fn swap_keeps_the_rect_set_and_only_relabels_owners() {
        let mut t = hsplit(0.25, leaf(1), leaf(2));
        let before = compute_rects(&t, AREA);
        assert!(swap_units(&mut t, &ids(&[1]), &ids(&[2])));
        let after = compute_rects(&t, AREA);
        let rects = |v: &[(PaneId, Rect)]| v.iter().map(|(_, r)| *r).collect::<Vec<_>>();
        assert_eq!(rects(&before), rects(&after));
        assert_eq!(before[0].0, PaneId(1));
        assert_eq!(after[0].0, PaneId(2));
        assert_eq!(after[1].0, PaneId(1));
    }
}
