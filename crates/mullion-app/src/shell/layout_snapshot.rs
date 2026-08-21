//! 布局树与磁盘格式的互转、恢复时的丢弃规则、窗口几何夹紧、落盘节流
//! (F37,设计 E5~E8)。**纯函数,零 egui/winit/wgpu/tokio**,可纯单测。
//!
//! 这里是 `mullion-core` 的运行时树(`Node`)与 `mullion-store` 的磁盘格式
//! (`SavedNodeEntry` 扁平前序编码)之间那一层。**它存在的全部理由是让这两个
//! 类型不互相认识**(设计 E4):store 一旦依赖 core,布局树就只剩一个真源,
//! 此后 core 每改一次 `Node` 就同时改了磁盘格式。
//!
//! `app.rs` 只做接线 —— 遍历标签、读窗口尺寸、写盘那几步要 `EventLoopProxy`
//! 和真实 `TabContent`,在无头容器里造不出来,所以判据全部搬到这里。

use mullion_core::layout::{Dir, Node, PaneId};
use mullion_store::layout::{SavedDir, SavedLayout, SavedNodeEntry, SavedWindow};
use mullion_store::SessionId;

// ---------------------------------------------------------------- 树互转(E5)

fn to_saved_dir(d: Dir) -> SavedDir {
    match d {
        Dir::Horizontal => SavedDir::Horizontal,
        Dir::Vertical => SavedDir::Vertical,
    }
}

fn from_saved_dir(d: SavedDir) -> Dir {
    match d {
        SavedDir::Horizontal => Dir::Horizontal,
        SavedDir::Vertical => Dir::Vertical,
    }
}

/// 运行时树 → 磁盘格式(前序遍历)。**丢掉 `PaneId`**,只留结构与比例(E5)。
pub fn to_entries(root: &Node) -> Vec<SavedNodeEntry> {
    let mut out = Vec::new();
    push_entries(root, &mut out);
    out
}

fn push_entries(node: &Node, out: &mut Vec<SavedNodeEntry>) {
    match node {
        Node::Leaf(_) => out.push(SavedNodeEntry::leaf()),
        Node::Split { dir, ratio, a, b } => {
            out.push(SavedNodeEntry::split(to_saved_dir(*dir), *ratio));
            push_entries(a, out);
            push_entries(b, out);
        }
    }
}

/// 磁盘格式 → 运行时树,叶子按前序用 `ids` 依次发号。
///
/// `None` = **编码损坏**:空数组、半拉的分割节点(见 `SavedNodeEntry::is_leaf`)、
/// 子树缺失、或尾部有多余项。`ids` 数量与叶子数对不上也返回 `None` ——
/// 调用方必须先用 [`leaf_count`] 算出要几个 id 再来调。
///
/// **尾部多余项也算坏**:一份「前面能拼出完整的树、后面还挂着几项」的数据
/// 说明写它的人跟读它的人对格式的理解不一致,这时候按前缀凑合恢复出来的
/// 布局是猜的。
/// **不先调 [`leaf_count`] 去验一遍**:那样的话下面这些检查全都够不着,
/// 改坏了一条测试都不红 —— 看着像防线,实际是没人守的死代码(变异验收
/// 当场发现)。这里自己验,`leaf_count` 只负责回答调用方「要开几个 pane」。
pub fn from_entries(entries: &[SavedNodeEntry], ids: &[PaneId]) -> Option<Node> {
    let mut at = 0usize;
    let mut next_id = 0usize;
    let node = take_node(entries, &mut at, ids, &mut next_id)?;
    // 尾部有多余项 / id 数比叶子多:两边对格式的理解不一致,按前缀凑合
    // 恢复出来的布局是猜的。
    if at != entries.len() || next_id != ids.len() {
        return None;
    }
    Some(node)
}

fn take_node(
    entries: &[SavedNodeEntry],
    at: &mut usize,
    ids: &[PaneId],
    next_id: &mut usize,
) -> Option<Node> {
    let e = entries.get(*at)?;
    *at += 1;
    match (e.dir, e.ratio) {
        (None, None) => {
            let id = *ids.get(*next_id)?;
            *next_id += 1;
            Some(Node::Leaf(id))
        }
        (Some(dir), Some(ratio)) => {
            let a = take_node(entries, at, ids, next_id)?;
            let b = take_node(entries, at, ids, next_id)?;
            Some(Node::Split {
                dir: from_saved_dir(dir),
                // 落盘的比例可能被手改成 0 / 1 / NaN。夹到 `split_pane` 用的
                // 同一个开区间里 —— 0 或 1 会让一个 pane 收成 0 像素、NaN 会
                // 让 wgpu 拿到 NaN 尺寸(见 docs/gui-render-gotchas.md)。
                ratio: sane_ratio(ratio),
                a: Box::new(a),
                b: Box::new(b),
            })
        }
        // 半拉的分割节点:有 dir 没 ratio(或反过来)。不猜,判坏。
        _ => None,
    }
}

/// 比例夹进 `(0, 1)`。NaN 一律回落 0.5。
fn sane_ratio(r: f32) -> f32 {
    if r.is_nan() {
        return 0.5;
    }
    r.clamp(0.05, 0.95)
}

/// 焦点 pane 在**前序叶子序**里排第几(E5:磁盘上只存序号,不存 `PaneId`)。
///
/// 顺序必须与 [`to_entries`] 一致 —— 两处各写一遍遍历的话,焦点会在恢复时
/// 落到另一个 pane 上,而这种错位肉眼几乎看不出来(用户只会觉得"打字打到
/// 了另一格"),所以两者共用同一条前序约定,并配了一条对拍测试。
///
/// 焦点不在树上(理论上不会发生)时返回 0。
pub fn focus_leaf_index(root: &Node, focus: PaneId) -> usize {
    let mut seen = 0usize;
    find_leaf(root, focus, &mut seen).unwrap_or(0)
}

fn find_leaf(node: &Node, focus: PaneId, seen: &mut usize) -> Option<usize> {
    match node {
        Node::Leaf(id) => {
            if *id == focus {
                Some(*seen)
            } else {
                *seen += 1;
                None
            }
        }
        Node::Split { a, b, .. } => find_leaf(a, focus, seen).or_else(|| find_leaf(b, focus, seen)),
    }
}

/// 这份编码里有几个叶子。`None` = 编码损坏(判据同 [`from_entries`])。
pub fn leaf_count(entries: &[SavedNodeEntry]) -> Option<usize> {
    let mut at = 0usize;
    let mut leaves = 0usize;
    scan(entries, &mut at, &mut leaves)?;
    if at != entries.len() || leaves == 0 {
        return None;
    }
    Some(leaves)
}

fn scan(entries: &[SavedNodeEntry], at: &mut usize, leaves: &mut usize) -> Option<()> {
    let e = entries.get(*at)?;
    *at += 1;
    match (e.dir, e.ratio) {
        (None, None) => {
            *leaves += 1;
            Some(())
        }
        (Some(_), Some(_)) => {
            scan(entries, at, leaves)?;
            scan(entries, at, leaves)
        }
        _ => None,
    }
}

// ------------------------------------------------------------ 丢弃规则(E6)

/// 过滤掉恢复不出来的标签,并把 `active_tab` 夹回合法范围。
///
/// `known` 回答「这个会话 id 现在还在库里吗」。传闭包而不是 `&SessionStore`:
/// store 打不开时(keyring 不可用)照样要能跑这段判据,而且这样才测得动。
///
/// 三条丢弃规则(设计 E6),每条都配了测试:
/// 1. 会话已被删 → 丢这个标签(留着只会给出一个点了必然失败的「重连」)。
/// 2. 树编码损坏 → 丢这个标签(见 [`leaf_count`])。
/// 3. 上面两条丢完之后 `active_tab` 越界 → 夹到 0。
///
/// **绝不因为一个坏标签就丢整份布局**:四个标签里有一个的会话被删了,
/// 用户要的是另外三个照常回来。
pub fn usable(mut layout: SavedLayout, known: &dyn Fn(SessionId) -> bool) -> SavedLayout {
    layout
        .tabs
        .retain(|t| known(t.session_id) && leaf_count(&t.tree).is_some());
    if layout.active_tab >= layout.tabs.len() {
        layout.active_tab = 0;
    }
    layout
}

/// 焦点叶子序号夹回合法范围。越界 = 树被改小了,焦点落回第一个叶子。
pub fn sane_focus_leaf(focus_leaf: usize, leaves: usize) -> usize {
    if leaves == 0 || focus_leaf >= leaves {
        0
    } else {
        focus_leaf
    }
}

// -------------------------------------------------------- 窗口几何夹紧(E8)

/// 一块显示器的工作区(逻辑点)。
///
/// **winit 的 `MonitorHandle` 不进这一层** —— 它在无头容器里构造不出来,
/// 让它进来就等于把「窗口会不会恢复到看不见的地方」这条判据变成只能靠人眼验。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MonitorRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl MonitorRect {
    fn intersects(&self, x: f32, y: f32, w: f32, h: f32) -> bool {
        x < self.x + self.width && x + w > self.x && y < self.y + self.height && y + h > self.y
    }
}

/// 窗口尺寸下界(逻辑点)。
///
/// 存到一个 0×0 会发生:Windows 最小化时会送 `Resized(0,0)`
/// (见 `window_state.rs`),若快照恰好在那一帧拍下,恢复出来就是个抓不住的
/// 窗口。下界比「能看见菜单栏和一行终端」再宽裕一点。
pub const MIN_W: f32 = 480.0;
pub const MIN_H: f32 = 320.0;

/// 恢复窗口时真正要用的几何。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RestoredWindow {
    pub width: f32,
    pub height: f32,
    /// `None` = **不设位置**,交给窗口管理器摆(见 [`clamp_to_monitors`])。
    pub pos: Option<(f32, f32)>,
    pub maximized: bool,
}

/// 把保存下来的窗口几何夹回「这次开机看得见的地方」(E8)。
///
/// 位置只有两种结果:**原样用**或**整个丢掉**。不做「夹到最近的边」——
/// 夹出来的位置多半压在任务栏底下或死贴屏幕边缘,比让窗口管理器自己摆更差,
/// 而且用户会以为是程序把窗口挪走了。
///
/// `monitors` 为空(拿不到显示器列表)时保守处理:**丢掉位置**。宁可让 WM
/// 摆一次,也不要把窗口恢复到一个可能不存在的坐标上——那是「双击了图标,
/// 任务栏有它,屏幕上找不到」这类最难自查的故障。
pub fn clamp_to_monitors(saved: SavedWindow, monitors: &[MonitorRect]) -> RestoredWindow {
    let width = sane_side(saved.width, MIN_W);
    let height = sane_side(saved.height, MIN_H);
    let pos = match (saved.x, saved.y) {
        (Some(x), Some(y))
            if x.is_finite()
                && y.is_finite()
                && monitors.iter().any(|m| m.intersects(x, y, width, height)) =>
        {
            Some((x, y))
        }
        _ => None,
    };
    RestoredWindow {
        width,
        height,
        pos,
        maximized: saved.maximized,
    }
}

fn sane_side(v: f32, min: f32) -> f32 {
    if v.is_finite() && v > min {
        v
    } else {
        min
    }
}

// ------------------------------------------------------------ 落盘节流(E7)

/// 两次落盘之间至少隔这么久。
///
/// 拖分隔条是**每帧**都在变的动作,而落盘是「写临时文件 + rename」的原子写。
/// 不节流就是拖一秒往磁盘扔上百个文件。代价写在设计 E7:进程被杀时最后
/// 这段时间内的布局变化会丢,恢复出来的分屏比例是两秒前的。
pub const FLUSH_INTERVAL_MS: u64 = 2000;

/// 这一刻该不该把布局写盘。
///
/// **不脏就一定不写** —— 没有这一条,空闲时每 2 秒也会写一次盘,笔记本上
/// 就是硬盘永远睡不下去。
pub fn should_flush(dirty: bool, since_last_ms: u64) -> bool {
    dirty && since_last_ms >= FLUSH_INTERVAL_MS
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_store::layout::{SavedTab, SavedTabKind};

    fn ids(n: u32) -> Vec<PaneId> {
        (1..=n).map(PaneId).collect()
    }

    /// H(1, V(2, 3)) —— 一棵不对称、两种方向、两个不同 ratio 的树。
    /// 对称的树测不出「a/b 搞反了」。
    fn sample_tree() -> Node {
        Node::Split {
            dir: Dir::Horizontal,
            ratio: 0.25,
            a: Box::new(Node::Leaf(PaneId(1))),
            b: Box::new(Node::Split {
                dir: Dir::Vertical,
                ratio: 0.75,
                a: Box::new(Node::Leaf(PaneId(2))),
                b: Box::new(Node::Leaf(PaneId(3))),
            }),
        }
    }

    /// E5 的核心:结构与比例逐字段回来,`PaneId` 按传入的新号重发。
    ///
    /// 自证会变红:`push_entries` 里把 a/b 的顺序对调、或 `to_saved_dir`
    /// 把两个方向映反。
    #[test]
    fn a_tree_round_trips_through_the_flat_encoding() {
        let before = sample_tree();
        let entries = to_entries(&before);
        let after = from_entries(&entries, &ids(3)).expect("三个叶子三个 id,该拼得出来");
        assert_eq!(after, before, "编码:{entries:?}");
    }

    /// `focus_leaf_index` 与 `to_entries` 必须走同一条前序。**对拍**:
    /// 逐个叶子问一遍「你排第几」,答案必须正好是 0..n 且顺序与
    /// `from_entries` 发号的顺序一致 —— 后者是恢复时真正决定「第 k 个叶子
    /// 拿到哪个 `PaneId`」的那段代码。
    ///
    /// 两处各写一遍遍历时的现象是「恢复后光标在另一格里」,肉眼几乎看不出来,
    /// 所以必须机械对拍,不能靠读代码确认。
    ///
    /// 自证会变红:`find_leaf` 里把 `a`/`b` 的先后对调。
    #[test]
    fn the_focus_index_counts_leaves_in_the_same_order_the_encoder_writes_them() {
        let tree = sample_tree();
        let entries = to_entries(&tree);
        let rebuilt = from_entries(&entries, &ids(3)).expect("三个叶子三个 id");
        // `from_entries` 按前序把 ids(3) = [1,2,3] 发下去,所以「叶子 k 的
        // PaneId」就是 `ids(3)[k]`;拿它反问 `focus_leaf_index`,必须得回 k。
        for (k, id) in ids(3).into_iter().enumerate() {
            assert_eq!(
                focus_leaf_index(&rebuilt, id),
                k,
                "第 {k} 个叶子({id:?})的前序序号对不上 —— 存焦点和恢复焦点\
                 用的不是同一条遍历,恢复后光标会落到别的 pane 上"
            );
        }
    }

    /// 焦点不在树上(理论上不会发生)时落回 0,不 panic —— 快照是每两秒跑
    /// 一次的后台动作,为它崩掉整个进程不成比例。
    #[test]
    fn an_unknown_focus_falls_back_to_the_first_leaf() {
        assert_eq!(focus_leaf_index(&sample_tree(), PaneId(99)), 0);
    }

    /// 恢复用的是**新发的号**,不是保存时的号 —— `PaneId` 跨重启没有意义(E5)。
    #[test]
    fn leaves_are_renumbered_in_preorder_with_the_ids_given() {
        let entries = to_entries(&sample_tree());
        let fresh = vec![PaneId(40), PaneId(41), PaneId(42)];
        let tree = from_entries(&entries, &fresh).unwrap();
        assert_eq!(
            mullion_core::layout::leaves(&tree),
            fresh,
            "叶子必须按前序拿到 ids 的顺序 —— 顺序错了,焦点叶子序号就指错 pane"
        );
    }

    #[test]
    fn a_single_leaf_tree_round_trips() {
        let one = Node::Leaf(PaneId(9));
        let entries = to_entries(&one);
        assert_eq!(entries.len(), 1);
        assert_eq!(leaf_count(&entries), Some(1));
        assert_eq!(
            from_entries(&entries, &[PaneId(5)]),
            Some(Node::Leaf(PaneId(5)))
        );
    }

    /// 损坏的编码一律 `None`,**不猜**。每一条都对应一种手改/半截写入。
    ///
    /// 自证会变红:把 `from_entries` 尾部那句 `at != entries.len()` 删掉,
    /// 或把 `take_node` 的 `_ => None` 改成当叶子处理。
    #[test]
    fn corrupt_encodings_are_rejected_rather_than_guessed_at() {
        let split = SavedNodeEntry::split(SavedDir::Horizontal, 0.5);
        let leaf = SavedNodeEntry::leaf();
        let half = SavedNodeEntry {
            dir: Some(SavedDir::Horizontal),
            ratio: None,
        };

        assert_eq!(leaf_count(&[]), None, "空数组:没有树");
        assert_eq!(leaf_count(&[split]), None, "分割节点缺了两棵子树");
        assert_eq!(leaf_count(&[split, leaf]), None, "分割节点只有一棵子树");
        assert_eq!(leaf_count(&[leaf, leaf]), None, "尾部有多余项");
        assert_eq!(leaf_count(&[half, leaf, leaf]), None, "半拉的分割节点");

        assert_eq!(from_entries(&[], &[]), None);
        assert_eq!(from_entries(&[leaf, leaf], &ids(2)), None, "尾部多余项");
        assert_eq!(
            from_entries(&[split, leaf, leaf], &ids(3)),
            None,
            "id 数比叶子多:调用方算错了,不该悄悄用前两个"
        );
        assert_eq!(
            from_entries(&[split, leaf, leaf], &ids(1)),
            None,
            "id 数比叶子少"
        );
        // 半拉的分割节点**不许被当成叶子**。要用「零个 id」这种刁钻输入才
        // 分辨得出来:`from_entries` 尾部还有一道「id 数对不上」的检查,
        // 正常的 id 数会把这条差异掩盖掉(变异验收发现,`take_node` 的
        // `_ => None` 分支原本一条测试都够不着)。
        assert_eq!(
            from_entries(&[half], &[]),
            None,
            "半拉的分割节点被当成了叶子"
        );
    }

    /// 手改出来的极端比例会让一个 pane 收成 0 像素;NaN 会一路传到 wgpu
    /// 的表面尺寸(`docs/gui-render-gotchas.md` 记的坑)。落盘的数是**外部
    /// 输入**,进来就要夹。
    #[test]
    fn insane_ratios_from_a_hand_edited_file_are_clamped() {
        let leaf = SavedNodeEntry::leaf();
        for (given, expect) in [(0.0f32, 0.05f32), (1.0, 0.95), (-3.0, 0.05), (17.0, 0.95)] {
            let e = [SavedNodeEntry::split(SavedDir::Vertical, given), leaf, leaf];
            let Some(Node::Split { ratio, .. }) = from_entries(&e, &ids(2)) else {
                panic!("该拼得出来");
            };
            assert_eq!(ratio, expect, "ratio {given} 没夹住");
        }
        let e = [
            SavedNodeEntry::split(SavedDir::Vertical, f32::NAN),
            leaf,
            leaf,
        ];
        let Some(Node::Split { ratio, .. }) = from_entries(&e, &ids(2)) else {
            panic!("该拼得出来");
        };
        assert_eq!(ratio, 0.5, "NaN 必须回落到 0.5,不能传下去");
    }

    fn tab(id: u64, tree: Vec<SavedNodeEntry>) -> SavedTab {
        SavedTab {
            kind: SavedTabKind::Terminal,
            session_id: SessionId(id),
            title: format!("s{id}"),
            focus_leaf: 0,
            tree,
        }
    }

    fn one_leaf() -> Vec<SavedNodeEntry> {
        vec![SavedNodeEntry::leaf()]
    }

    /// E6:会话被删了的标签丢掉,**其余照常恢复**。
    ///
    /// 自证会变红:把 `retain` 的 `known(...)` 条件去掉。
    #[test]
    fn a_tab_whose_session_was_deleted_is_dropped_but_the_others_survive() {
        let layout = SavedLayout {
            schema_version: 1,
            active_tab: 0,
            // F148 的时刻字段;这些测试只关心树与会话筛选,填 0 即可。
            updated_at: 0,
            window: None,
            tabs: vec![tab(1, one_leaf()), tab(2, one_leaf()), tab(3, one_leaf())],
        };
        let got = usable(layout, &|id| id != SessionId(2));
        assert_eq!(
            got.tabs.iter().map(|t| t.session_id.0).collect::<Vec<_>>(),
            vec![1, 3],
            "只该丢掉会话已删的那个"
        );
    }

    /// E6:树编码坏了的标签丢掉。留着它等于给用户一个点了必然出错的标签。
    #[test]
    fn a_tab_with_a_corrupt_tree_is_dropped() {
        let layout = SavedLayout {
            schema_version: 1,
            active_tab: 0,
            // F148 的时刻字段;这些测试只关心树与会话筛选,填 0 即可。
            updated_at: 0,
            window: None,
            tabs: vec![
                tab(1, vec![SavedNodeEntry::split(SavedDir::Horizontal, 0.5)]),
                tab(2, one_leaf()),
            ],
        };
        let got = usable(layout, &|_| true);
        assert_eq!(got.tabs.len(), 1);
        assert_eq!(got.tabs[0].session_id, SessionId(2));
    }

    /// E6:丢完标签之后 `active_tab` 可能指到空气上。
    ///
    /// 自证会变红:把夹紧那两行删掉 —— 然后 `tabs.switch_to_index(越界)`
    /// 会静默不动,活动标签停在第 0 个,现象是「恢复出来焦点老是错」,
    /// 而不是崩溃,所以特别难发现。
    #[test]
    fn an_out_of_range_active_tab_is_clamped_to_the_first_one() {
        let layout = SavedLayout {
            schema_version: 1,
            active_tab: 2,
            // F148 的时刻字段;这些测试只关心树与会话筛选,填 0 即可。
            updated_at: 0,
            window: None,
            tabs: vec![tab(1, one_leaf()), tab(2, one_leaf()), tab(3, one_leaf())],
        };
        // 丢掉两个之后只剩 1 个,active_tab = 2 越界。
        let got = usable(layout, &|id| id == SessionId(1));
        assert_eq!(got.tabs.len(), 1);
        assert_eq!(got.active_tab, 0);
    }

    #[test]
    fn dropping_every_tab_is_the_same_as_having_no_layout_at_all() {
        let layout = SavedLayout {
            schema_version: 1,
            active_tab: 1,
            // F148 的时刻字段;这些测试只关心树与会话筛选,填 0 即可。
            updated_at: 0,
            window: None,
            tabs: vec![tab(1, one_leaf()), tab(2, one_leaf())],
        };
        let got = usable(layout, &|_| false);
        assert!(got.tabs.is_empty());
        assert_eq!(got.active_tab, 0);
    }

    #[test]
    fn focus_leaf_out_of_range_falls_back_to_the_first_leaf() {
        assert_eq!(sane_focus_leaf(2, 4), 2);
        assert_eq!(sane_focus_leaf(4, 4), 0, "0-based,4 个叶子的合法上限是 3");
        assert_eq!(sane_focus_leaf(9, 1), 0);
        assert_eq!(sane_focus_leaf(0, 0), 0);
    }

    fn primary() -> MonitorRect {
        MonitorRect {
            x: 0.0,
            y: 0.0,
            width: 1920.0,
            height: 1080.0,
        }
    }

    /// 副屏在主屏左边(负坐标)。Windows 上这是最常见的多屏摆法,
    /// 也是「保存了负的 x,下次副屏不在了」的来源。
    fn left_secondary() -> MonitorRect {
        MonitorRect {
            x: -1920.0,
            y: 0.0,
            width: 1920.0,
            height: 1080.0,
        }
    }

    fn saved_at(x: f32, y: f32) -> SavedWindow {
        SavedWindow {
            width: 1200.0,
            height: 800.0,
            x: Some(x),
            y: Some(y),
            maximized: false,
        }
    }

    #[test]
    fn a_window_on_a_still_present_monitor_is_restored_where_it_was() {
        let got = clamp_to_monitors(saved_at(-1800.0, 40.0), &[primary(), left_secondary()]);
        assert_eq!(got.pos, Some((-1800.0, 40.0)));
        assert_eq!((got.width, got.height), (1200.0, 800.0));
    }

    /// E8 的核心:副屏拔了之后,原来的位置整个丢掉,交给窗口管理器。
    ///
    /// 不夹到最近的边 —— 夹出来多半压在任务栏底下,而用户会以为是程序
    /// 把窗口挪走了。
    ///
    /// 自证会变红:把 `monitors.iter().any(..)` 改成恒真。
    #[test]
    fn a_window_on_a_monitor_that_is_gone_loses_its_position() {
        let got = clamp_to_monitors(saved_at(-1800.0, 40.0), &[primary()]);
        assert_eq!(got.pos, None, "副屏没了,位置该交给 WM");
        assert_eq!(
            (got.width, got.height),
            (1200.0, 800.0),
            "尺寸是用户调的,该留着"
        );
    }

    /// 只露出一角也算「看得见」——那是用户自己拖成那样的,别替他改。
    #[test]
    fn a_window_hanging_off_the_edge_is_left_alone_because_the_user_put_it_there() {
        let got = clamp_to_monitors(saved_at(1900.0, 1000.0), &[primary()]);
        assert_eq!(got.pos, Some((1900.0, 1000.0)));
    }

    /// 正好贴着右边缘外侧 = 无交集。这条钉的是 `intersects` 的 `<` 与 `<=`:
    /// 写成 `<=` 的话,一个 x 恰好等于屏幕宽度的窗口会被判成「看得见」。
    #[test]
    fn a_window_exactly_past_the_right_edge_does_not_count_as_visible() {
        let got = clamp_to_monitors(saved_at(1920.0, 0.0), &[primary()]);
        assert_eq!(got.pos, None);
    }

    /// 拿不到显示器列表时保守:丢位置。恢复到一个可能不存在的坐标上
    /// = 「任务栏里有它,屏幕上找不到」,是最难自查的一类故障。
    #[test]
    fn an_empty_monitor_list_is_treated_as_unknown_and_the_position_is_dropped() {
        let got = clamp_to_monitors(saved_at(100.0, 100.0), &[]);
        assert_eq!(got.pos, None);
    }

    /// 快照可能恰好拍在 Windows 最小化送的 `Resized(0,0)` 那一帧上
    /// (见 `window_state.rs`),恢复出来会是个抓不住的窗口。
    #[test]
    fn a_degenerate_size_is_raised_to_the_minimum() {
        let tiny = SavedWindow {
            width: 0.0,
            height: 0.0,
            x: Some(10.0),
            y: Some(10.0),
            maximized: false,
        };
        let got = clamp_to_monitors(tiny, &[primary()]);
        assert_eq!((got.width, got.height), (MIN_W, MIN_H));

        let nan = SavedWindow {
            width: f32::NAN,
            height: 700.0,
            x: Some(f32::NAN),
            y: Some(10.0),
            maximized: true,
        };
        let got = clamp_to_monitors(nan, &[primary()]);
        assert_eq!(got.width, MIN_W, "NaN 尺寸会一路传到 wgpu 的表面");
        assert_eq!(got.height, 700.0);
        assert_eq!(got.pos, None, "NaN 坐标同理");
        assert!(got.maximized, "最大化状态与位置无关,不该被一起丢掉");
    }

    /// E7:不脏就一定不写。没有这条,空闲时每 2 秒也要写一次盘,
    /// 笔记本的硬盘永远睡不下去。
    ///
    /// 自证会变红:把 `dirty &&` 去掉。
    #[test]
    fn a_clean_layout_is_never_written_no_matter_how_long_it_has_been() {
        assert!(!should_flush(false, 0));
        assert!(!should_flush(false, FLUSH_INTERVAL_MS));
        assert!(!should_flush(false, u64::MAX));
    }

    /// E7:脏了也要等到节流窗口。拖分隔条是每帧都在变的动作,
    /// 每帧一次原子写(写临时文件 + rename)等于一秒往磁盘扔上百个文件。
    ///
    /// 自证会变红:把 `since_last_ms >= FLUSH_INTERVAL_MS` 改成恒真。
    #[test]
    fn a_dirty_layout_waits_for_the_throttle_window() {
        assert!(!should_flush(true, 0));
        assert!(!should_flush(true, FLUSH_INTERVAL_MS - 1));
        assert!(should_flush(true, FLUSH_INTERVAL_MS));
        assert!(should_flush(true, FLUSH_INTERVAL_MS * 3));
    }
}
