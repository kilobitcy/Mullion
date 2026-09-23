# F290~F292 命令抽屉 / 搜索通配 / 按字母定位 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 三条独立的用户实报：① 终端 pane 底下一键拉出一个裸 shell「抽屉」（父 pane 的 1/5 高、同节点、自动 `cd` 到父 pane 上报的目录，再按一次关掉）；② 文件面板远端递归搜索支持 `*.docx` 这类通配；③ 文件面板两栏按字母键定位到以该前缀开头的条目。

**Architecture:** ① 抽屉在布局树里就是一块普通叶子（`Dir::Vertical` + ratio 0.8），`Workspace` 只多记一张「抽屉 → 父 pane」的标记表；所有与「树形状」有关的既有路径（F241 关后重排、预设重建、F37 存盘）把抽屉**摘出去再挂回来**，抽屉不参与它们的计数。`cd` 走既有自动化通道（`on_pane_ready` → `automation::run`），只是计划由 `pending_for_drawer` 现造。② `find.rs` 加第三个具名判据 `glob_matches`，由 `query_matches` 按「查询串里有没有 `*`/`?`」分派，`Walk::accept` 改调分派函数；子序列判据 `matches` 一个字不动。③ 新纯模块 `files/type_ahead.rs`（注入 `now: Instant`），`PaneState::type_ahead` 消费它并复用 F218 的 `select_only` + `scroll_to`；`handle_panel_key` 的 `_ => {}` 臂接线。

**Tech Stack:** Rust / egui 0.30 / winit 0.30 / mullion-core layout 树 / 既有 `UserEvent` 回路。

---

## 已经定死的设计决策（grill 阶段用户确认过，**不要重新讨论**）

### F290 抽屉
- **模型 = 普通上下分屏**，ratio 0.8（父 4 : 抽屉 1），受 F32 夹紧，开后允许拖分隔条。不做成设置项。
- **裸 shell，不进 tmux**。断线即消失（不参与 F128 接回），不进 F37 现场（存盘前剪掉）。
- **目录 = 开抽屉那一帧快照父 pane 的 F123 `cwd`**，之后不跟随。回退：① pane 所属项目（F221）的 `dir` → ② 不 `cd`，状态栏一行提示，不弹窗。
- `cd` 走**键盘字节**（自动化通道，等首字节再发），不是 exec channel。
- 热键 **`` Ctrl+` ``**：无抽屉 → 开并聚焦抽屉；焦点在抽屉或有抽屉的父 pane → 关，焦点回父。父 pane 关闭 → 抽屉一起关。抽屉本身被 × 关掉 → 标记清除。回父 pane 不关抽屉走既有方向键（F33），不加键。
- 标题条给抽屉加「抽屉」字样（GBK 内，不用符号）。

### F291 通配
- 查询串**含 `*` 或 `?`** → glob 整名匹配（大小写不敏感，`*` 匹配任意段含 `.`，`?` 一个字符，`[...]` 不支持）；否则维持 F278 子序列。不引新依赖。hint 补「支持 * ?」。

### F292 按字母定位
- 面板有焦点、无输入框在编辑时，可打印字符 → 光标跳到**下一个**以它开头的条目（从光标之后找起，到底绕回），大小写不敏感。
- **1 秒内连按累积成前缀**；同一字母反复按 = 在同字母条目间循环。目录/文件一视同仁，按当前排序。
- **两栏都做**。跳到后滚到可见（复用 `scroll_to`）。排除带 Ctrl/Alt/Super 的组合。

## 本计划自己补的范围决策（实现者不要擅自扩大）

1. **一块 pane 至多一个抽屉，抽屉不能再开抽屉**（焦点在抽屉上按热键 = 关）。
2. **断线处理**：`Workspace::pump` 末尾把状态刚变成 `Reconnecting` 的抽屉直接关掉（等价于「断线即消失」）；用户在抽屉里敲 `exit`（`Disconnected`）的抽屉留着，按热键或 × 关。
3. **F241 关后重排 / 预设重建**：这两条路径把抽屉摘出去算，算完按「父 pane 还在就挂回去（ratio 重置为 0.8），父 pane 没了就连抽屉一起关」处理。**抽屉自己关闭不触发 F241 重排**。
4. **重连（F128）路径不改**：抽屉在 `pump` 里已被关掉，不会出现在 `channels` 里。
5. F292 的字符判据：`WinitKey::Character(s)` 且 `s` 恰好一个 `char`、不是空白也不是控制字符。空格**不**参与（留给将来的选中切换）。
6. F291 的 `*` 单独一个字符也是合法 glob（匹配一切）——用户自己敲的，不拦。

---

## File Structure

| 文件 | 职责 | 动作 |
|---|---|---|
| `crates/mullion-app/src/shell/workspace/mod.rs` | `Drawer` 标记表 + `open_drawer`/`close_drawer`/`drawer_toggle`/`tree_without_drawers`/`reattach_drawers`；`close_pane`/`apply_preset`/`pump` 三处让位 | 改 |
| `crates/mullion-app/src/automation.rs` | `pending_for_drawer`（纯函数：`cd` 计划） | 改 |
| `crates/mullion-app/src/shell/drawer.rs` | `drawer_cwd`（目录回退判据，纯函数） | **新建** |
| `crates/mullion-app/src/shell/mod.rs` | 挂 `pub mod drawer;` | 改 |
| `crates/mullion-app/src/ui/pane_title.rs` | `TitleView.drawer` + `title_text` 加参 | 改 |
| `crates/mullion-app/src/app.rs` | `drawer_hotkey_event`/`apply_drawer_hotkey`、`PaneOpened` 分叉、`snapshot_tabs_of` 剪树、`window_event` 接线、标题构造点、`handle_panel_key` 字母臂 | 改 |
| `crates/mullion-app/src/files/find.rs` | `is_glob`/`glob_matches`/`query_matches`；`Walk::accept` 改调 | 改 |
| `crates/mullion-app/src/ui/files_panel.rs` | 搜索条 hint 文案 | 改 |
| `crates/mullion-app/tests/search_box_adoption.rs` | 若 LEDGER 里登记了 hint 原文则同步 | 视情况改 |
| `crates/mullion-app/src/files/type_ahead.rs` | 纯函数 `next_index` + `key_char` | **新建** |
| `crates/mullion-app/src/files/mod.rs` | 挂 `pub mod type_ahead;` | 改 |
| `crates/mullion-app/src/files/state.rs` | `PaneState.type_ahead` 字段 + `type_ahead()` 方法 | 改 |
| `spec.md` | F290/F291/F292 三行 | 改 |
| `Cargo.toml` | 0.1.116 → 0.1.117 | 改（发版任务） |

---

## 实现者必须先读的既有约定（照做，别自创）

**A. `Workspace::close_pane` 有 F241 重排**（`workspace/mod.rs:444`）：关完按剩余数量 1/2/3 → `Single`/`TwoLeftRight`/`ThreeColumns` 重建整棵树。抽屉不能参与这一步的计数，否则「2 块 pane + 1 抽屉，关掉一块」会被重排成「pane 与抽屉左右并排」。

**B. `apply_preset`**（`:340`）拿 `self.statuses()`（树上全部叶子）算 keep/close/spawn，再 `preset_tree` 从头建树。同 A，抽屉要先摘出去。

**C. 新 pane 的唯一就绪入口是 `on_pane_ready`**（`app.rs:9506`），有守护 `every_pane_ready_path_goes_through_on_pane_ready`。抽屉的 `cd` 计划也从这里进，不许另开通道。

**D. 热键截击必须在输入分流之前**（`window_event`，`app.rs:12795` 起的那串 `if self.xxx_hotkey_event(&event) { return; }`），且每条都有一条 `xxx_shortcut_is_swallowed_before_the_input_routing` 源码切片守护。新加一条要配一条。

**E. F286：真开出新格子才交出 egui 键盘焦点**（`ctx.memory_mut(|m| m.stop_text_input())`），关格子不动。

**F. F255：`focus_host_ix` 必须在改树之前取**（`land_layout_actions` 的写法）。

**G. `PaneState`（files/state.rs）加字段要同步 `PaneState::new`**；`PanelFrame::default()` 兼作借用过桥占位，新字段只能是 `Option`/零值这类真实安全默认（`files_panel.rs:2344` 那段警告）。

**H. 源码切片守护的写法**：锚点字符串用 `concat!` 拆开拼（否则匹配到测试自己），`prod_src()` 已剥注释（F289），先断言切片非空再断内容。

**I. 守护测试的纪律**：每条断言的 doc 注释写「自证会变红：把哪一行改成什么」。变异验证前**先 commit**（历史上五次被 `git checkout` 吞掉未提交编辑）。

**J. 字形白名单（T9）**：UI 字符串里的新字只许 GBK 内汉字与 ASCII。「抽屉」两字都在 GBK 内。

---

## Task 1: `Workspace` 抽屉标记表与开/关

**Files:**
- Modify: `crates/mullion-app/src/shell/workspace/mod.rs`（结构体 :237、`close_pane` :444、`split_focused` :474 附近）
- Test: 同文件 `mod tests`

- [ ] **Step 1: 写失败测试**（放进 `mod tests`，用既有 `ws_with`/`fake_pane`）

```rust
    // ---- F290 命令抽屉 ----

    /// F290:开抽屉 = 对焦点 pane 做一次上下分屏,抽屉在下、占 1/5,焦点当场
    /// 给抽屉(开了就是要敲命令)。
    ///
    /// 自证会变红:把 `open_drawer` 里的 `Dir::Vertical` 改成 `Horizontal`,
    /// 或 `DRAWER_RATIO` 改成 0.5,或删掉最后那句 `self.focus = id`。
    #[test]
    fn opening_a_drawer_splits_the_focused_pane_vertically_at_one_fifth() {
        let (mut ws, _) = ws_with(1);
        let id = ws.open_drawer(Some(b"/srv/app".to_vec())).expect("单屏上必能开");
        assert_eq!(ws.focus(), id, "焦点要当场给抽屉");
        assert_eq!(ws.drawer_of(PaneId(1)), Some(id));
        assert!(ws.is_drawer(id));
        assert_eq!(ws.drawer(id).and_then(|d| d.cwd.clone()), Some(b"/srv/app".to_vec()));
        match ws.tree() {
            Node::Split { dir, ratio, a, b } => {
                assert_eq!(*dir, Dir::Vertical, "抽屉在父 pane **底下**");
                assert!((ratio - DRAWER_RATIO).abs() < 1e-6, "父占 4/5");
                assert_eq!(**a, Node::Leaf(PaneId(1)));
                assert_eq!(**b, Node::Leaf(id));
            }
            other => panic!("开完不是一个 Split:{other:?}"),
        }
    }

    /// 一块 pane 至多一个抽屉;抽屉自己不能再开抽屉。两种情形都返回 `None`
    /// 且树不动。
    ///
    /// 自证会变红:删掉 `open_drawer` 开头那两句拒绝判据。
    #[test]
    fn a_pane_gets_at_most_one_drawer_and_a_drawer_cannot_nest() {
        let (mut ws, _) = ws_with(1);
        let d = ws.open_drawer(None).unwrap();
        let before = ws.tree().clone();
        assert!(ws.open_drawer(None).is_none(), "焦点在抽屉上再按 = 关,不是再开");
        ws.set_focus(PaneId(1));
        assert!(ws.open_drawer(None).is_none(), "父 pane 已经有抽屉了");
        assert_eq!(*ws.tree(), before);
        assert_eq!(ws.drawer_of(PaneId(1)), Some(d));
    }

    /// 关抽屉:树上兄弟顶替、channel 显式关掉(F140)、标记清除、焦点回父 pane。
    /// **不走 F241 重排** —— 三块竖排 pane 关掉其中一块的抽屉,三块还是竖排。
    ///
    /// 自证会变红:把 `close_drawer` 改成直接调 `close_pane`(重排会把下面
    /// 那棵手搭的竖排树拍成 `ThreeColumns`);或删掉 `p.pty.close()`。
    #[test]
    fn closing_a_drawer_restores_the_parent_and_never_rearranges_the_rest() {
        let (mut ws, probes) = ws_with(1);
        let (p2, _) = fake_pane(2);
        let (p3, _) = fake_pane(3);
        ws.attach_pane(p2);
        ws.attach_pane(p3);
        // 手搭一棵竖排三块:1 在上,2 中,3 下。
        ws.set_tree_for_test(Node::Split {
            dir: Dir::Vertical,
            ratio: 0.33,
            a: Box::new(Node::Leaf(PaneId(1))),
            b: Box::new(Node::Split {
                dir: Dir::Vertical,
                ratio: 0.5,
                a: Box::new(Node::Leaf(PaneId(2))),
                b: Box::new(Node::Leaf(PaneId(3))),
            }),
        });
        ws.set_focus(PaneId(2));
        let before = ws.tree().clone();
        let d = ws.open_drawer(None).unwrap();
        let (dp, dprobe) = fake_pane(d.0);
        ws.attach_pane(dp);
        assert!(ws.close_drawer(d));
        assert_eq!(*ws.tree(), before, "关抽屉之后树必须**原样**回到开之前");
        assert_eq!(ws.focus(), PaneId(2), "焦点回父 pane");
        assert!(ws.drawer_of(PaneId(2)).is_none());
        assert!(ws.pane(d).is_none());
        assert_eq!(*dprobe.closes.lock().unwrap(), 1, "抽屉的 channel 要显式关(F140)");
        assert_eq!(*probes[0].closes.lock().unwrap(), 0, "别人的 channel 不许动");
    }

    /// `close_pane` 收到抽屉的 id(用户点了抽屉标题条上的 ×)→ 转 `close_drawer`,
    /// 同样不重排。
    ///
    /// 自证会变红:删掉 `close_pane` 开头 `if self.is_drawer(id)` 那一句。
    #[test]
    fn closing_a_drawer_through_the_generic_path_takes_the_drawer_route() {
        let (mut ws, _) = ws_with(2); // 1 | 2 左右
        ws.set_focus(PaneId(2));
        let before = ws.tree().clone();
        let d = ws.open_drawer(None).unwrap();
        let (dp, _) = fake_pane(d.0);
        ws.attach_pane(dp);
        assert!(ws.close_pane(d));
        assert_eq!(*ws.tree(), before);
        assert!(ws.drawer_of(PaneId(2)).is_none());
    }

    /// 父 pane 被关 → 抽屉一起关(它是附属物)。之后 F241 重排照旧,但重排的
    /// 计数**不含抽屉**。
    ///
    /// 自证会变红:删掉 `close_pane` 里级联关抽屉那一段 —— 抽屉会被兄弟顶替
    /// 成一块孤儿 pane,`pane_count` 是 2 不是 1。
    #[test]
    fn closing_the_parent_takes_its_drawer_with_it() {
        let (mut ws, _) = ws_with(2);
        ws.set_focus(PaneId(2));
        let d = ws.open_drawer(None).unwrap();
        let (dp, dprobe) = fake_pane(d.0);
        ws.attach_pane(dp);
        assert!(ws.close_pane(PaneId(2)));
        assert_eq!(ws.pane_count(), 1);
        assert_eq!(*ws.tree(), Node::Leaf(PaneId(1)));
        assert!(ws.pane(d).is_none(), "抽屉的 PaneState 也要丢");
        assert_eq!(*dprobe.closes.lock().unwrap(), 1);
        assert!(ws.drawers().is_empty());
    }

    /// 关掉**别的** pane 触发 F241 重排时,抽屉先摘出去、重排完再挂回父 pane
    /// 底下。三块横排 + 1 号有抽屉,关掉 3 号 → 两块横排,抽屉仍在 1 号底下。
    ///
    /// 自证会变红:把 `close_pane` 里 `ids.retain(|id| !self.is_drawer(*id))`
    /// 删掉(抽屉会被当成第三块 pane 排成 `ThreeColumns`);或删掉重排后那句
    /// `self.reattach_drawers()`(抽屉从树上消失、`PaneState` 却还在)。
    #[test]
    fn rearranging_after_a_close_keeps_the_drawer_under_its_parent() {
        let (mut ws, _) = ws_with(3);
        ws.set_focus(PaneId(1));
        let d = ws.open_drawer(None).unwrap();
        let (dp, _) = fake_pane(d.0);
        ws.attach_pane(dp);
        assert!(ws.close_pane(PaneId(3)));
        let expect = Node::Split {
            dir: Dir::Horizontal,
            ratio: 0.5,
            a: Box::new(Node::Split {
                dir: Dir::Vertical,
                ratio: DRAWER_RATIO,
                a: Box::new(Node::Leaf(PaneId(1))),
                b: Box::new(Node::Leaf(d)),
            }),
            b: Box::new(Node::Leaf(PaneId(2))),
        };
        assert_eq!(*ws.tree(), expect);
        assert_eq!(ws.drawer_of(PaneId(1)), Some(d));
    }

    /// 点预设按钮(`apply_preset`)同理:抽屉不参与 keep/close/spawn 的计数,
    /// 重建完挂回去。单屏 + 抽屉 → 点「两栏」:新开 **1** 块(不是 0 块,
    /// 也不是把抽屉当第二块),抽屉仍在 1 号底下。
    ///
    /// 自证会变红:把 `apply_preset` 里的 `statuses_without_drawers()` 换回
    /// `statuses()`(`fresh` 变成空);或删掉末尾 `reattach_drawers()`。
    #[test]
    fn presets_count_panes_without_the_drawers_and_reattach_them_afterwards() {
        let (mut ws, _) = ws_with(1);
        let d = ws.open_drawer(None).unwrap();
        let (dp, _) = fake_pane(d.0);
        ws.attach_pane(dp);
        let fresh = ws.apply_preset(Preset::TwoLeftRight);
        assert_eq!(fresh.len(), 1, "抽屉不算一块 pane");
        let new = fresh[0];
        let expect = Node::Split {
            dir: Dir::Horizontal,
            ratio: 0.5,
            a: Box::new(Node::Split {
                dir: Dir::Vertical,
                ratio: DRAWER_RATIO,
                a: Box::new(Node::Leaf(PaneId(1))),
                b: Box::new(Node::Leaf(d)),
            }),
            b: Box::new(Node::Leaf(new)),
        };
        assert_eq!(*ws.tree(), expect);
        assert_eq!(ws.focus(), new, "F286:新格子拿焦点,与没有抽屉时一致");
    }

    /// 预设把抽屉的父 pane 关掉了(三块 → 单屏,3 号有抽屉)→ 抽屉随父一起关。
    ///
    /// 自证会变红:删掉 `apply_preset` 里对 `plan.close` 级联关抽屉那一段。
    #[test]
    fn a_preset_that_closes_the_parent_closes_its_drawer_too() {
        let (mut ws, _) = ws_with(3);
        ws.set_focus(PaneId(3));
        let d = ws.open_drawer(None).unwrap();
        let (dp, dprobe) = fake_pane(d.0);
        ws.attach_pane(dp);
        let fresh = ws.apply_preset(Preset::Single);
        assert!(fresh.is_empty());
        assert_eq!(*ws.tree(), Node::Leaf(PaneId(1)));
        assert!(ws.pane(d).is_none());
        assert!(ws.drawers().is_empty());
        assert_eq!(*dprobe.closes.lock().unwrap(), 1);
    }

    /// `drawer_toggle`:热键按下去做什么,三种情形。
    ///
    /// 自证会变红:把 `drawer_toggle` 里 `is_drawer(focus)` 那一臂删掉
    /// (焦点在抽屉上会判成 `Open`)。
    #[test]
    fn the_toggle_decides_open_or_close_from_where_the_focus_sits() {
        let (mut ws, _) = ws_with(1);
        assert_eq!(ws.drawer_toggle(), DrawerToggle::Open);
        let d = ws.open_drawer(None).unwrap();
        assert_eq!(ws.drawer_toggle(), DrawerToggle::Close(d), "焦点在抽屉上");
        ws.set_focus(PaneId(1));
        assert_eq!(ws.drawer_toggle(), DrawerToggle::Close(d), "焦点在有抽屉的父 pane 上");
    }
```

同时在 `impl Workspace` 的 `#[cfg(test)]` 区加一个测试专用 setter（`tree_mut_for_test` 旁边）：

```rust
    #[cfg(test)]
    pub(crate) fn set_tree_for_test(&mut self, tree: Node) {
        self.next_id = self
            .next_id
            .max(leaves(&tree).iter().map(|id| id.0 + 1).max().unwrap_or(0));
        self.tree = tree;
    }
```

- [ ] **Step 2: 跑测试确认编译失败**

Run: `cargo test -p mullion-app --lib shell::workspace::tests::opening_a_drawer 2>&1 | tail -5`
Expected: 编译错误 `no method named open_drawer`。

- [ ] **Step 3: 实现**

在 `pub struct Workspace` 上方加：

```rust
/// F290:抽屉的高度比例 —— 父 pane 占 4/5,抽屉占 1/5。开的时候用一次;
/// 之后用户拖分隔条改掉的比例**不追回**(重排挂回去时重置为这个值)。
pub const DRAWER_RATIO: f32 = 0.8;

/// F290:「这块叶子是谁的抽屉」。抽屉在布局树里就是普通叶子,`Workspace`
/// 只多记这一张表;凡是按「树形状」做事的路径(F241 关后重排、预设重建、
/// F37 存盘)都要把它摘出去算,理由见各处注释。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drawer {
    pub id: PaneId,
    pub parent: PaneId,
    /// 开抽屉那一帧快照下来的目标目录(`cd` 的参数)。`None` = 没定位到,
    /// 不发 `cd`。**快照**而不是每次现读父 pane —— 父 pane 里 Claude Code
    /// `cd` 来 `cd` 去时抽屉不该被动跳目录。
    pub cwd: Option<Vec<u8>>,
}

/// F290:热键按下去该做什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawerToggle {
    /// 焦点 pane 没有抽屉 → 给它开一个。
    Open,
    /// 焦点在抽屉上、或焦点 pane 已有抽屉 → 关掉这个抽屉。
    Close(PaneId),
}
```

`Workspace` 加字段 `drawers: Vec<Drawer>`（`new` 里 `drawers: Vec::new()`）。加方法：

```rust
    // ---- F290 命令抽屉 ----

    pub fn drawers(&self) -> &[Drawer] {
        &self.drawers
    }
    pub fn drawer(&self, id: PaneId) -> Option<&Drawer> {
        self.drawers.iter().find(|d| d.id == id)
    }
    pub fn is_drawer(&self, id: PaneId) -> bool {
        self.drawer(id).is_some()
    }
    /// `parent` 底下挂着的抽屉。
    pub fn drawer_of(&self, parent: PaneId) -> Option<PaneId> {
        self.drawers.iter().find(|d| d.parent == parent).map(|d| d.id)
    }

    /// 热键的判定。纯查询,不改任何状态。
    pub fn drawer_toggle(&self) -> DrawerToggle {
        let f = self.focus;
        if self.is_drawer(f) {
            return DrawerToggle::Close(f);
        }
        match self.drawer_of(f) {
            Some(d) => DrawerToggle::Close(d),
            None => DrawerToggle::Open,
        }
    }

    /// 给焦点 pane 开抽屉:上下分屏、抽屉在下占 1/5、焦点当场给抽屉。
    /// 返回抽屉的 id(还没有 `PaneState`,同 `apply_preset` 的空窗期约定)。
    ///
    /// `None` = 焦点本身是抽屉,或焦点 pane 已经有抽屉(一块 pane 至多一个)。
    pub fn open_drawer(&mut self, cwd: Option<Vec<u8>>) -> Option<PaneId> {
        let parent = self.focus;
        if self.is_drawer(parent) || self.drawer_of(parent).is_some() {
            return None;
        }
        let id = self.alloc_id();
        if !split_pane(&mut self.tree, parent, id, Dir::Vertical, DRAWER_RATIO) {
            self.next_id -= 1;
            return None;
        }
        self.drawers.push(Drawer { id, parent, cwd });
        // 开了就是要敲命令 —— 与 F286「用户亲手开的格子焦点跟过去」同一条规矩。
        self.focus = id;
        Some(id)
    }

    /// 关抽屉:兄弟顶替、channel 显式关(F140)、标记清除、焦点回父 pane。
    ///
    /// **刻意不走 F241 重排**:抽屉不是用户布局的一部分,关它不该动别的格子。
    pub fn close_drawer(&mut self, id: PaneId) -> bool {
        let Some(pos) = self.drawers.iter().position(|d| d.id == id) else {
            return false;
        };
        if !close_pane(&mut self.tree, id) {
            return false;
        }
        let parent = self.drawers.remove(pos).parent;
        self.drop_pane_state(id);
        let alive = leaves(&self.tree);
        if !alive.contains(&self.focus) {
            self.focus = if alive.contains(&parent) {
                parent
            } else {
                next_focus(self.focus, &alive)
            };
        }
        true
    }

    /// 丢一块 pane 的 `PaneState`,并显式关它的 channel(F140)。
    fn drop_pane_state(&mut self, id: PaneId) {
        if let Some(p) = self.panes.iter().find(|p| p.id == id) {
            p.pty.close();
        }
        self.panes.retain(|p| p.id != id);
    }

    /// 重排之后把抽屉挂回各自父 pane 底下;父 pane 已经不在树上的抽屉一并关掉。
    fn reattach_drawers(&mut self) {
        let alive = leaves(&self.tree);
        let drawers = self.drawers.clone();
        for d in drawers {
            if alive.contains(&d.id) {
                continue;
            }
            if alive.contains(&d.parent)
                && split_pane(&mut self.tree, d.parent, d.id, Dir::Vertical, DRAWER_RATIO)
            {
                continue;
            }
            self.drawers.retain(|x| x.id != d.id);
            self.drop_pane_state(d.id);
        }
    }

    /// `statuses()` 去掉抽屉:预设与 F241 的计数只看用户自己的格子。
    fn statuses_without_drawers(&self) -> Vec<(PaneId, PaneStatus)> {
        self.statuses()
            .into_iter()
            .filter(|(id, _)| !self.is_drawer(*id))
            .collect()
    }

    /// F37:存盘用的树 —— 抽屉全部剪掉,焦点若在抽屉上则落回父 pane。
    /// 返回 `(树, 焦点)`;不改自身。
    pub fn tree_without_drawers(&self) -> (Node, PaneId) {
        let mut tree = self.tree.clone();
        let mut focus = self.focus;
        for d in &self.drawers {
            close_pane(&mut tree, d.id);
            if focus == d.id {
                focus = d.parent;
            }
        }
        let alive = leaves(&tree);
        if !alive.contains(&focus) {
            focus = alive[0];
        }
        (tree, focus)
    }
```

改 `close_pane`：

```rust
    pub fn close_pane(&mut self, id: PaneId) -> bool {
        // F290:关的是抽屉 → 走抽屉自己那条(不重排)。
        if self.is_drawer(id) {
            return self.close_drawer(id);
        }
        // F290:父 pane 走了,抽屉一起走(先关它,树上少一层,下面的兄弟顶替
        // 才顶得对)。
        if let Some(d) = self.drawer_of(id) {
            self.close_drawer(d);
        }
        if !close_pane(&mut self.tree, id) {
            return false;
        }
        self.drop_pane_state(id);
        // F241 重排的计数**不含抽屉**,重排完再挂回去。
        let mut ids = leaves(&self.tree);
        ids.retain(|id| !self.is_drawer(*id));
        if let Some(preset) = preset::layout_after_close(ids.len()) {
            self.tree = preset_tree(preset, &ids);
            self.reattach_drawers();
        }
        self.focus = next_focus(self.focus, &leaves(&self.tree));
        true
    }
```

改 `apply_preset`：

```rust
    pub fn apply_preset(&mut self, preset: Preset) -> Vec<PaneId> {
        // F290:抽屉不参与 keep/close/spawn 的计数。
        let plan = plan_preset(preset, &self.statuses_without_drawers());
        for id in &plan.close {
            if let Some(d) = self.drawer_of(*id) {
                self.drawers.retain(|x| x.id != d);
                self.drop_pane_state(d);
            }
            self.drop_pane_state(*id);
        }
        let mut ids = plan.keep;
        let mut fresh = Vec::new();
        for _ in 0..plan.spawn {
            let id = self.alloc_id();
            ids.push(id);
            fresh.push(id);
        }
        self.tree = preset_tree(preset, &ids);
        self.reattach_drawers();
        self.focus = next_focus(self.focus, &leaves(&self.tree));
        // ……(F286 那段原样保留)
        if let Some(&first) = fresh.first() {
            self.focus = first;
        }
        fresh
    }
```

注意：原来 `plan.close` 只做 `self.panes.retain(..)`（不关 channel）——**这是既有行为**，本任务把它改成 `drop_pane_state`（显式 `close()`）。这是 F140 同款修正，doc 注释里写明。若既有测试因此变红（例如断言 close 计数为 0），**先看那条测试的判据是什么再决定**，不要为了过测试把 `close()` 拿掉。

- [ ] **Step 4: 跑测试**

Run: `cargo test -p mullion-app --lib shell::workspace 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: 全过。

- [ ] **Step 5: Commit**

```bash
git add crates/mullion-app/src/shell/workspace/mod.rs
git commit -m "feat(app): Workspace 抽屉标记表 + 开/关/重排让位 (F290)"
```

---

## Task 2: 断线即消失（`pump` 末尾收抽屉）

**Files:**
- Modify: `crates/mullion-app/src/shell/workspace/mod.rs`（`pump` :489）
- Test: 同文件 `mod tests`

- [ ] **Step 1: 写失败测试**

```rust
    /// F290:抽屉是裸 shell,不参与 F128 接回 —— 链路一断(状态刚变成
    /// `Reconnecting`)就直接关掉。同一帧里父 pane 照常进入 `Reconnecting`
    /// 等重连。
    ///
    /// 自证会变红:删掉 `pump` 末尾 `reap_dead_drawers()` 那一句。
    #[test]
    fn a_drawer_whose_link_died_is_closed_instead_of_queued_for_reconnect() {
        let (mut ws, probes) = ws_with(1);
        ws.link_alive = |_, _| false;
        let d = ws.open_drawer(None).unwrap();
        let (dp, dprobe) = fake_pane(d.0);
        ws.attach_pane(dp);
        drop(dprobe.tx); // 抽屉的 rx 关掉 = 链路死了
        drop(probes.into_iter().next().unwrap().tx);
        ws.pump(0);
        assert!(ws.pane(d).is_none(), "抽屉该被关掉");
        assert!(ws.drawers().is_empty());
        assert_eq!(*ws.tree(), Node::Leaf(PaneId(1)));
        assert_eq!(
            ws.pane(PaneId(1)).unwrap().status,
            PaneStatus::Reconnecting,
            "父 pane 照常等重连"
        );
    }

    /// 用户在抽屉里敲 `exit`(链路活着,`Disconnected`)→ 抽屉**留着**,
    /// 由用户按热键或 × 关。
    ///
    /// 自证会变红:把 `reap_dead_drawers` 的判据从 `Reconnecting` 放宽成
    /// `!= Live`。
    #[test]
    fn a_drawer_the_user_exited_stays_until_they_close_it() {
        let (mut ws, _) = ws_with(1);
        ws.link_alive = |_, _| true;
        let d = ws.open_drawer(None).unwrap();
        let (dp, dprobe) = fake_pane(d.0);
        ws.attach_pane(dp);
        drop(dprobe.tx);
        ws.pump(0);
        assert_eq!(ws.pane(d).unwrap().status, PaneStatus::Disconnected);
        assert_eq!(ws.drawer_of(PaneId(1)), Some(d));
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib shell::workspace::tests::a_drawer_whose_link_died 2>&1 | grep -E "test result|panicked"`
Expected: FAIL（抽屉仍在）。

- [ ] **Step 3: 实现**

`pump` 的 `for p in &mut self.panes { ... }` 循环结束后加一句 `self.reap_dead_drawers();`，并加方法：

```rust
    /// F290:链路死了的抽屉直接关掉,不排队重连(设计决策:裸 shell、用完即扔)。
    /// 判据**只认 `Reconnecting`**:`Disconnected` 是用户自己敲的 `exit`,
    /// 那块留给用户处置。
    fn reap_dead_drawers(&mut self) {
        let dead: Vec<PaneId> = self
            .drawers
            .iter()
            .filter(|d| {
                self.pane(d.id)
                    .is_some_and(|p| p.status == PaneStatus::Reconnecting)
            })
            .map(|d| d.id)
            .collect();
        for id in dead {
            self.close_drawer(id);
        }
    }
```

- [ ] **Step 4: 跑测试**

Run: `cargo test -p mullion-app --lib shell::workspace 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: 全过。

- [ ] **Step 5: Commit**

```bash
git add crates/mullion-app/src/shell/workspace/mod.rs
git commit -m "feat(app): 链路一断抽屉即关,不参与 F128 接回 (F290)"
```

---

## Task 3: `cd` 计划与目录回退判据（纯函数）

**Files:**
- Modify: `crates/mullion-app/src/automation.rs`（`pending_for_extra_pane` :134 之后）
- Create: `crates/mullion-app/src/shell/drawer.rs`
- Modify: `crates/mullion-app/src/shell/mod.rs`（挂模块）

- [ ] **Step 1: 写失败测试**

`automation.rs` 的 `mod tests` 里：

```rust
    /// F290:抽屉只发一句 `cd '<目录>'\n`,目录走单引号转义(`exec::shell_quote`,
    /// 字节级),延时取模板的 `initial_delay_ms`(与分屏同款:等 MOTD 打完)。
    ///
    /// 自证会变红:把 `shell_quote` 拿掉直接拼路径(带单引号的目录那条红);
    /// 或把 `\n` 删掉。
    #[test]
    fn a_drawer_plan_is_a_single_quoted_cd() {
        let tpl = ResolvedAutomation {
            initial_delay_ms: 450,
            ready_timeout_ms: 9_000,
            ..plain_template()
        };
        let plan = pending_for_drawer(Some(b"/srv/it's"), Some(&tpl)).expect("有目录就有计划");
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].bytes, b"cd '/srv/it'\\''s'\n".to_vec());
        assert_eq!(plan.steps[0].delay, Duration::from_millis(450));
        assert_eq!(plan.ready_timeout_ms, 9_000);
        assert!(plan.gate.is_none());
    }

    /// 没定位到目录 → 没有计划(连 oneshot 都不建);没有模板 → 用内置默认延时。
    ///
    /// 自证会变红:`cwd` 为 `None` 时返回一句空 `cd`。
    #[test]
    fn a_drawer_without_a_directory_has_no_plan_and_no_template_means_defaults() {
        assert!(pending_for_drawer(None, None).is_none());
        let plan = pending_for_drawer(Some(b"/tmp"), None).unwrap();
        assert_eq!(
            plan.steps[0].delay,
            Duration::from_millis(u64::from(mullion_store::DEFAULT_INITIAL_DELAY_MS))
        );
        assert_eq!(plan.ready_timeout_ms, mullion_store::DEFAULT_READY_TIMEOUT_MS);
    }
```

`plain_template()`：看 `automation.rs` 测试模块里有没有现成的 `ResolvedAutomation` 构造辅助（:489 附近有 `initial_delay_ms: 300` 的字面构造）；有就复用其名字，没有就照那处抄一个 `fn plain_template() -> ResolvedAutomation`。

`shell/drawer.rs`（新建，含测试）：

```rust
//! F290:命令抽屉的纯判据。零 IO、零 egui。

/// 抽屉该 `cd` 到哪。按序回退:父 pane 上报的**绝对**路径 → 所属项目的
/// 目录 → 不 `cd`(`None`)。
///
/// 只认绝对路径:F123 的窗口标题那条腿会报 `~/x`,而抽屉是一条**新** shell,
/// `cd '~/x'` 单引号里的 `~` 不展开,会 `cd` 失败还多一行报错。
/// 项目目录是用户手填的,同样要求以 `/` 开头。
pub fn drawer_cwd(pane_cwd: Option<&[u8]>, project_dir: Option<&str>) -> Option<Vec<u8>> {
    if let Some(c) = pane_cwd.filter(|c| c.starts_with(b"/")) {
        return Some(c.to_vec());
    }
    project_dir
        .filter(|d| d.starts_with('/'))
        .map(|d| d.as_bytes().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 自证会变红:把第一段的 `starts_with(b"/")` 过滤删掉(`~/x` 会被当目录)。
    #[test]
    fn the_pane_directory_wins_only_when_it_is_absolute() {
        assert_eq!(drawer_cwd(Some(b"/srv/app"), Some("/proj")), Some(b"/srv/app".to_vec()));
        assert_eq!(drawer_cwd(Some(b"~/app"), Some("/proj")), Some(b"/proj".to_vec()));
    }

    /// 自证会变红:把项目目录那段的 `starts_with('/')` 删掉。
    #[test]
    fn the_project_directory_is_the_fallback_and_relative_ones_are_refused() {
        assert_eq!(drawer_cwd(None, Some("/proj")), Some(b"/proj".to_vec()));
        assert_eq!(drawer_cwd(None, Some("proj")), None);
        assert_eq!(drawer_cwd(None, None), None);
    }
}
```

- [ ] **Step 2: 跑测试确认编译失败**

Run: `cargo test -p mullion-app --lib automation::tests::a_drawer_plan 2>&1 | tail -3`
Expected: `cannot find function pending_for_drawer`。

- [ ] **Step 3: 实现**

`automation.rs` 在 `pending_for_extra_pane` 之后：

```rust
/// F290:抽屉 pane 该跑什么 —— **只有一句 `cd`**,不跑登录后命令、不 export。
/// 抽屉是用户临时敲系统命令的地方,登录后命令那套(启动服务、attach)全不该
/// 在这儿再来一遍。`None` = 没定位到目录,什么都不发。
///
/// 延时与超时取模板(与分屏那条同源);标签没有模板时用内置默认。
pub fn pending_for_drawer(
    cwd: Option<&[u8]>,
    tpl: Option<&ResolvedAutomation>,
) -> Option<PendingAutomation> {
    let cwd = cwd?;
    let mut bytes = b"cd ".to_vec();
    bytes.extend(mullion_ssh::exec::shell_quote(cwd));
    bytes.push(b'\n');
    let delay_ms = tpl.map_or(mullion_store::DEFAULT_INITIAL_DELAY_MS, |t| t.initial_delay_ms);
    Some(PendingAutomation {
        steps: vec![Step {
            delay: Duration::from_millis(u64::from(delay_ms)),
            bytes,
        }],
        ready_timeout_ms: tpl.map_or(mullion_store::DEFAULT_READY_TIMEOUT_MS, |t| t.ready_timeout_ms),
        gate: None,
    })
}
```

确认 `mullion_store` 导出了 `DEFAULT_INITIAL_DELAY_MS` / `DEFAULT_READY_TIMEOUT_MS`（`grep -n "pub use" crates/mullion-store/src/lib.rs`）；没导出就加到 `lib.rs` 的 `pub use automation::{...}` 里。确认 `mullion_ssh::exec` 是 `pub mod`。

`shell/mod.rs` 加 `pub mod drawer;`。

- [ ] **Step 4: 跑测试**

Run: `cargo test -p mullion-app --lib automation:: shell::drawer 2>&1 | grep -E "test result|FAILED"`
Expected: 全过。

- [ ] **Step 5: Commit**

```bash
git add crates/mullion-app/src/automation.rs crates/mullion-app/src/shell/drawer.rs crates/mullion-app/src/shell/mod.rs
git commit -m "feat(app): 抽屉的 cd 计划与目录回退判据 (F290)"
```

---

## Task 4: 标题条「抽屉」字样

**Files:**
- Modify: `crates/mullion-app/src/ui/pane_title.rs`（`TitleView` :13、`title_text` :91、调用点 :456、测试 :490 起）
- Modify: `crates/mullion-app/src/app.rs`（`TitleView` 构造点 :13696 附近）

- [ ] **Step 1: 写失败测试**（`pane_title.rs` 的 `mod tests`）

```rust
    /// F290:抽屉的标题条在序号后面写「抽屉」,其余段照旧 —— 用户要一眼分得清
    /// 它和普通分屏。断开态也带。
    ///
    /// 自证会变红:`title_text` 里 `drawer` 那句 `parts.insert(1, ..)` 删掉。
    #[test]
    fn a_drawer_title_says_so_right_after_the_index() {
        assert_eq!(
            title_text(3, Some("build-01"), Some("app"), None, None, PaneStatus::Live, None, true),
            "3 · 抽屉 · build-01 · app"
        );
        assert_eq!(
            title_text(3, Some("build-01"), None, None, None, PaneStatus::Disconnected, None, true),
            "3 · 抽屉 · build-01 (已断开)"
        );
    }
```

既有 `title_text(` 的全部测试调用补一个尾参 `false`。

- [ ] **Step 2: 跑测试确认编译失败**

Run: `cargo test -p mullion-app --lib ui::pane_title 2>&1 | tail -3`
Expected: 参数个数不匹配。

- [ ] **Step 3: 实现**

`title_text` 加最后一个参数 `drawer: bool`；在 `let Some(h) = host else {...}` 之后、`Disconnected` 分支之前不好插（那两条是 `format!`），改成：

```rust
    let tag = if drawer { " · 抽屉" } else { "" };
    let Some(h) = host else {
        return tail(format!("{index}{tag} · 连接中…"));
    };
    if status == PaneStatus::Disconnected {
        return tail(format!("{index}{tag} · {h} (已断开)"));
    }
    let mut parts = vec![index.to_string()];
    if drawer {
        parts.push("抽屉".to_string());
    }
    parts.push(h.to_string());
```

`TitleView` 加字段 `pub drawer: bool`（doc：「F290:这块是抽屉。构造点按 `ws.is_drawer` 现查,不另记」）。`show` 里调用 `title_text(..., v.notice, v.drawer)`。

`app.rs` 构造点加 `drawer: ws.is_drawer(g.id),`。

- [ ] **Step 4: 跑测试 + 字形白名单**

Run: `cargo test -p mullion-app --lib ui::pane_title 2>&1 | grep -E "test result|FAILED"; cargo test -p mullion-app --test glyph_whitelist 2>&1 | grep -E "test result|FAILED"`
Expected: 全过（「抽屉」二字在 GBK 内）。

- [ ] **Step 5: Commit**

```bash
git add crates/mullion-app/src/ui/pane_title.rs crates/mullion-app/src/app.rs
git commit -m "feat(app): 抽屉 pane 标题条标「抽屉」 (F290)"
```

---

## Task 5: 热键、开 channel、`cd` 分叉、存盘剪树（`app.rs` 接线）

**Files:**
- Modify: `crates/mullion-app/src/app.rs`：`files_hotkey_event` :5043 附近加两个方法；`window_event` :12822 后插一句；`PaneOpened` :12206 分叉；`snapshot_tabs_of` :1936 剪树

- [ ] **Step 1: 写失败测试**（`app.rs` 的 `mod tests`；全部是接线守护或自由函数）

```rust
    // ---- F290 命令抽屉 ----

    /// **接线守护 / T8**:`` Ctrl+` `` 必须在输入分流**之前**被截走 —— 走到
    /// 下面会被 `encode_char` 编成控制字符写给远端。与 files/focus/project
    /// 三条同构。
    ///
    /// 自证会变红:把 `window_event` 里那句 `if self.drawer_hotkey_event(&event)`
    /// 整段删掉,或挪到 `egui_should_see` 那段之后。
    #[test]
    fn drawer_shortcut_is_swallowed_before_the_input_routing() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn window_event(")
            .nth(1)
            .expect("找不到 window_event 的定义");
        let hotkey = after
            .find("self.drawer_hotkey_event(&event)")
            .expect("window_event 里没调 drawer_hotkey_event —— Ctrl+` 会被编码进 PTY 写给远端");
        let routing = after.find("egui_should_see").expect("找不到输入分流那一段");
        assert!(hotkey < routing, "drawer_hotkey_event 排在了输入分流之后");
    }

    /// F290:热键判定只认 `Ctrl` + 反引号(`~` 是同一个键按了 Shift 的布局),
    /// 弹窗开着不响应。
    ///
    /// 自证会变红:把 `drawer_hotkey_event` 里 `'`'` 改成别的字符,或删掉
    /// `self.modal_open()` 那一项。
    #[test]
    fn the_drawer_hotkey_is_ctrl_backtick_and_yields_to_modals() {
        let body = strip_comments(body_of(prod_src(), "fn drawer_hotkey_event("));
        assert!(body.contains(concat!("Key::Char('`'", " | '~')")), "键不是反引号");
        assert!(body.contains("self.modal_open()"), "弹窗开着也响应 —— 会在输入框里打字时突然分屏");
        assert!(body.contains("!mods.ctrl"), "没要求 Ctrl");
        assert!(body.contains("mods.shift"), "没排除 Shift");
    }

    /// F290:开抽屉这一路必须做全四件事,少一件都是静默坏:
    /// ① `focus_host_ix` 在 `open_drawer` **之前**取(F255:改树会挪焦点);
    /// ② `drawer_cwd` 算目录(回退判据在纯函数里);
    /// ③ 真开出来才 `stop_text_input()`(F286);
    /// ④ 走 `spawn_fresh_panes` 开 channel(唯一的分屏开口)。
    ///
    /// 自证会变红:把 `apply_drawer_hotkey` 里 `focused_pane_host_ix()` 那句挪到
    /// `open_drawer(` 之后;或把 `spawn_fresh_panes(` 换成别的。
    #[test]
    fn opening_a_drawer_captures_the_host_first_then_spawns_through_the_split_path() {
        let body = strip_comments(body_of(prod_src(), "fn apply_drawer_hotkey("));
        let host_at = body.find("focused_pane_host_ix()").expect("没取 focus_host_ix");
        let open_at = body.find("open_drawer(").expect("没调 open_drawer");
        assert!(host_at < open_at, "F255:host_ix 要在改树之前取");
        assert!(body.contains(concat!("drawer::", "drawer_cwd(")), "目录回退没走纯函数");
        assert!(body.contains("stop_text_input()"), "F286:没交出 egui 键盘焦点");
        assert!(body.contains("spawn_fresh_panes("), "没走唯一的分屏开口");
        assert!(body.contains("close_drawer("), "关那一路没接");
    }

    /// F290:`PaneOpened` 里抽屉拿的是 `cd` 计划,不是登录后命令。
    ///
    /// 自证会变红:把 `PaneOpened` 臂里 `pending_for_drawer(` 那句删掉。
    #[test]
    fn a_freshly_opened_drawer_gets_the_cd_plan_not_the_login_commands() {
        let body = strip_comments(multiline_arm_of(prod_src(), "UserEvent::PaneOpened {"));
        assert!(body.contains(concat!("pending_for_", "drawer(")), "抽屉没拿 cd 计划");
        assert!(body.contains(concat!("pending_for_", "extra_pane")), "普通分屏那条被弄丢了");
    }

    /// F290/F37:存现场时抽屉要剪掉 —— 树和焦点都取 `tree_without_drawers()`,
    /// 不许一处取剪过的、另一处取原树(焦点下标会指到一个不存在的叶子)。
    ///
    /// 自证会变红:把 `snapshot_tabs_of` 里 `tree_without_drawers()` 换回
    /// `t.ws.tree()` / `t.ws.focus()`。
    #[test]
    fn the_layout_snapshot_is_taken_from_the_tree_without_drawers() {
        let body = strip_comments(body_of(prod_src(), "fn snapshot_tabs_of("));
        assert!(body.contains("tree_without_drawers()"), "存盘没剪抽屉");
        assert!(!body.contains("t.ws.tree()"), "还有地方直接读原树");
        assert!(!body.contains("t.ws.focus()"), "焦点还在读原值");
    }
```

`multiline_arm_of` 若不存在（只有 `arm_of`/`brace_balanced_arm`），用现有的 `brace_balanced_arm(rest)`：`let rest = prod_src().split("UserEvent::PaneOpened {").nth(1).unwrap(); let body = strip_comments(brace_balanced_arm(rest));`——先看 :25151 的签名再决定。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib tests::drawer_ tests::the_drawer_ tests::opening_a_drawer tests::a_freshly_opened_drawer tests::the_layout_snapshot_is_taken 2>&1 | grep -E "test result|panicked"`
Expected: 5 条 FAIL。

- [ ] **Step 3: 实现**

(a) `files_hotkey_event` 后面加：

```rust
    /// F290:`` Ctrl+` `` 开/关命令抽屉。选反引号是 VS Code「切换终端」的
    /// 肌肉记忆,而且它在终端里没有含义、不和 tmux 前缀撞。`'~'` 一起收:
    /// 某些布局按住 Shift 时 winit 给的是 `~`,但 Shift 本身被下面排除了,
    /// 这里只是防 `logical_key` 口径漂移。
    ///
    /// 同 `files_hotkey_event`:必须在 `window_event` 里输入分流**之前**调用
    /// (T8)—— 走到下面 `` Ctrl+` `` 会被 `encode_char` 编成控制字符写给远端。
    fn drawer_hotkey_event(&mut self, event: &WindowEvent) -> bool {
        let WindowEvent::KeyboardInput { event: ke, .. } = event else {
            return false;
        };
        if ke.state != ElementState::Pressed {
            return false;
        }
        let Some((key, mods)) = input::translate_key(ke, self.mods) else {
            return false;
        };
        if self.modal_open() || !mods.ctrl || mods.shift || mods.alt || mods.sup {
            return false;
        }
        if !matches!(key, Key::Char('`' | '~')) {
            return false;
        }
        self.apply_drawer_hotkey();
        self.request_ui_redraw();
        true
    }

    /// F290:热键按下之后做什么。判定在 `Workspace::drawer_toggle`,目录回退在
    /// `shell::drawer::drawer_cwd`,这里只取现场、按结果落地。
    fn apply_drawer_hotkey(&mut self) {
        // F255:在改树**之前**取(`open_drawer` 会把焦点挪到抽屉上)。
        let focus_host_ix = self
            .tabs
            .active()
            .and_then(|t| t.content.focused_pane_host_ix());
        let egui_ctx = self.active.as_ref().map(|a| a.egui_ctx.clone());
        // 项目目录(回退②)要读 `self.store`,得在借出 `ws` 之前算完。
        let project_dir: Option<String> = {
            let projects: &[mullion_store::ProjectRecord] =
                self.store.as_ref().map_or(&[], |s| s.projects());
            self.active_term()
                .and_then(|t| t.ws.focused())
                .and_then(|p| crate::project::project_of(p.tmux.as_deref(), projects))
                .map(|pj| pj.dir.clone())
        };
        let Some(t) = self.active_term_mut() else { return };
        match t.ws.drawer_toggle() {
            crate::shell::workspace::DrawerToggle::Close(id) => {
                if t.ws.close_drawer(id) {
                    mark_ui_dirty!(self.ui_dirty);
                }
            }
            crate::shell::workspace::DrawerToggle::Open => {
                let pane_cwd = t.ws.focused().and_then(|p| p.cwd.clone());
                let cwd = crate::shell::drawer::drawer_cwd(pane_cwd.as_deref(), project_dir.as_deref());
                let missing = cwd.is_none();
                let Some(id) = t.ws.open_drawer(cwd) else { return };
                // F286:真开出了格子才交出 egui 的键盘焦点。
                if let Some(ctx) = egui_ctx.as_ref() {
                    ctx.memory_mut(|m| m.stop_text_input());
                }
                mark_ui_dirty!(self.ui_dirty);
                if missing {
                    self.ui.set_error("抽屉未能定位到父分屏的目录,停在登录目录".to_string());
                }
                self.spawn_fresh_panes(vec![id], focus_host_ix);
            }
        }
    }
```

若 `self.ui.set_error` 在 `t` 借用期间借不到（`t` 借的是 `self.tabs`，`self.ui` 是另一字段，字段级不相交借用应该没问题；`active_term_mut()` 是 `&mut self` 方法则会冲突）——冲突就把 `missing`/`id` 先算出来、`t` 作用域结束后再 `set_error` + `spawn_fresh_panes`。

(b) `window_event` 里 `project_hotkey_event` 那段之后插：

```rust
        // F290/T8:命令抽屉热键同样必须在分流之前截 —— `` Ctrl+` `` 走到下面
        // 会被编成控制字符写给远端。
        if self.drawer_hotkey_event(&event) {
            return;
        }
```

(c) `PaneOpened` 臂里 `let plan = ...` 改成：

```rust
                if let Some(sink) = attached {
                    let tab = self
                        .tabs
                        .by_generation(generation)
                        .and_then(|tab| tab.content.as_terminal());
                    let tpl = tab.and_then(|t| t.automation_template.as_ref());
                    // F290:抽屉只发一句 `cd`,不跑登录后命令(`pending_for_drawer`
                    // 的文档)。判据是 `ws.drawer(id)`——抽屉标记在 `open_drawer`
                    // 那一帧就写好了,不会后发先至。
                    let plan = match tab.and_then(|t| t.ws.drawer(id)) {
                        Some(d) => crate::automation::pending_for_drawer(d.cwd.as_deref(), tpl),
                        None => tpl.and_then(crate::automation::pending_for_extra_pane),
                    };
                    self.on_pane_ready(generation, id, sink, plan, true);
                }
```

(d) `snapshot_tabs_of` 的 Terminal 臂：

```rust
            (TabContent::Terminal(t), Some(session_id)) => {
                // F290:抽屉不进现场 —— 树和焦点**同一份**剪过的。
                let (tree, focus) = t.ws.tree_without_drawers();
                SavedTab {
                    kind: SavedTabKind::Terminal,
                    session_id,
                    title: tab.title.clone(),
                    focus_leaf: snap::focus_leaf_index(&tree, focus),
                    tree: snap::to_entries(&tree, &|id| {
                        leaf_identity_of(
                            &|ix| t.ws.hosts.get(ix).and_then(|h| h.session_id),
                            t.ws.pane(id),
                            &t.leaf_wanted,
                            id,
                        )
                    }),
                }
            }
```

- [ ] **Step 4: 跑测试**

Run: `cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: 全过。特别留意既有的 `every_pane_ready_path_goes_through_on_pane_ready`、`panel_key_handling_is_routed_by_generation_not_by_the_active_tab`、以及 `snapshot_tabs_of` 相关的既有测试。

- [ ] **Step 5: Commit**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): Ctrl+\` 开关命令抽屉,cd 到父分屏目录,存现场时剪掉 (F290)"
```

---

## Task 6: F291 通配匹配

**Files:**
- Modify: `crates/mullion-app/src/files/find.rs`（`matches` :26 之后加三个函数；`accept` :218 改调）
- Modify: `crates/mullion-app/src/ui/files_panel.rs:1138` hint
- Check: `crates/mullion-app/tests/search_box_adoption.rs` 的 `LEDGER` 有没有登记 hint 原文

- [ ] **Step 1: 写失败测试**（`find.rs` 的 `mod tests`）

```rust
    // ---- F291 通配 ----

    /// `*` 任意段(含 `.` 与空串)、`?` 恰一个字符、整名锚定、大小写不敏感。
    ///
    /// 自证会变红:把 `glob_matches` 的整名锚定去掉(`*.docx` 会命中
    /// `a.docx.bak`);或把小写化去掉(`*.DOCX` 那条红)。
    #[test]
    fn a_glob_matches_the_whole_name_case_insensitively() {
        assert!(glob_matches("*.docx", "报告.docx"));
        assert!(glob_matches("*.docx", "a.DOCX"));
        assert!(glob_matches("*.DOCX", "a.docx"));
        assert!(!glob_matches("*.docx", "a.docx.bak"), "整名锚定");
        assert!(glob_matches("a?c", "abc"));
        assert!(!glob_matches("a?c", "abbc"));
        assert!(glob_matches("*", "anything"));
        assert!(glob_matches("*", ""));
        assert!(glob_matches("a*b*c", "aXXbYYc"));
        assert!(!glob_matches("a*b*c", "aXXcYYb"));
        assert!(glob_matches("*.docx", ".docx"), "`*` 可以是空串");
    }

    /// 分派:查询串里**有** `*`/`?` 才走 glob,否则维持 F278 子序列。
    /// `.docx`(不带星)仍是子序列 —— 用户分得清哪种在生效的唯一办法。
    ///
    /// 自证会变红:把 `query_matches` 改成恒走 `matches`(`*.docx` 全灭);
    /// 或恒走 glob(`.docx` 命中 `d.o.c.x` 那条红)。
    #[test]
    fn a_query_with_a_wildcard_is_a_glob_and_without_one_stays_a_subsequence() {
        assert!(query_matches("*.docx", "a.docx"));
        assert!(!query_matches("*.docx", "a.docx.bak"));
        assert!(!query_matches("*.docx", "docx"), "glob 下 `.` 是字面量,必须出现");
        assert!(query_matches(".docx", "d.o.c.x-notes"), "无通配 = 子序列,行为不变");
        assert!(query_matches("appr", "app.rs"));
        assert!(!query_matches("*", ""), "空名字不存在,但 `*` 的空匹配语义交给 glob");
    }

    /// `Walk` 用的是分派函数,不是裸 `matches` —— 否则上面两条纯函数测试
    /// 全绿、真搜索照旧不认 `*`。
    ///
    /// 自证会变红:把 `accept` 里的 `query_matches(` 改回 `matches(`。
    #[test]
    fn the_walk_dispatches_through_query_matches() {
        let mut w = Walk::new(rp("/root"), "*.docx".to_string(), true);
        let dir = w.take_runnable(1).pop().unwrap();
        w.accept(
            &dir,
            Ok(vec![entry("a.docx", EntryKind::File), entry("a.docx.bak", EntryKind::File)]),
        );
        let hits: Vec<String> = w.hits().iter().map(|h| h.path.display()).collect();
        assert_eq!(hits, vec!["/root/a.docx".to_string()]);
    }
```

`entry(name, kind)` / `take_runnable` / `hits()` 的名字以 `find.rs` 现有测试里的写法为准（先 `grep -n "fn entry\|take_runnable\|fn hits" crates/mullion-app/src/files/find.rs`），照抄现有辅助。最后一条里 `assert!(!query_matches("*", ""))` 若与 glob 语义冲突（`*` 匹配空串），删掉那一行——它不是判据。

- [ ] **Step 2: 跑测试确认编译失败**

Run: `cargo test -p mullion-app --lib files::find 2>&1 | tail -3`
Expected: `cannot find function glob_matches`。

- [ ] **Step 3: 实现**

`matches` 之后加：

```rust
/// F291:查询串里有没有通配符。有 → [`glob_matches`],没有 → [`matches`]。
pub fn is_glob(query: &str) -> bool {
    query.contains(['*', '?'])
}

/// F291:按查询串的形状分派。**这是 `Walk` 唯一该调的入口。**
pub fn query_matches(query: &str, name: &str) -> bool {
    if is_glob(query) {
        glob_matches(query, name)
    } else {
        matches(query, name)
    }
}

/// F291:glob 整名匹配。`*` 任意段(含空串、含 `.`),`?` 恰一个字符,其余
/// 字面量;大小写不敏感;**整名锚定**(`*.docx` 不命中 `a.docx.bak`)。
/// 不支持 `[...]`,方括号是字面量。
///
/// 手写而不引 `globset`:判据就这两个元字符,一个依赖换 30 行不值。
/// 经典双指针 + 单回溯点:遇到 `*` 记下位置,失配时回到上一个 `*` 多吃一个字符。
pub fn glob_matches(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().flat_map(char::to_lowercase).collect();
    let n: Vec<char> = name.chars().flat_map(char::to_lowercase).collect();
    let (mut pi, mut ni) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None; // (pattern 里 `*` 之后的位置, 当时的 name 位置)
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi + 1, ni));
            pi += 1;
        } else if let Some((sp, sn)) = star {
            pi = sp;
            ni = sn + 1;
            star = Some((sp, ni));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}
```

`accept` 里 `if matches(&self.query, &e.name.display())` → `if query_matches(&self.query, &e.name.display())`。

hint：`"文件名(模糊匹配,回车开始)"` → `"文件名(模糊匹配,支持 * ?,回车开始)"`。若 `tests/search_box_adoption.rs` 的 `LEDGER` 登记了 hint 原文，同步改；跑 `cargo test -p mullion-app --test search_box_adoption`。

- [ ] **Step 4: 跑测试**

Run: `cargo test -p mullion-app --lib files::find 2>&1 | grep -E "test result|FAILED"; cargo test -p mullion-app --test search_box_adoption --test glyph_whitelist 2>&1 | grep -E "test result|FAILED"`
Expected: 全过。

- [ ] **Step 5: Commit**

```bash
git add crates/mullion-app/src/files/find.rs crates/mullion-app/src/ui/files_panel.rs crates/mullion-app/tests/search_box_adoption.rs
git commit -m "feat(app): 远端递归搜索支持 * ? 通配 (F291)"
```

---

## Task 7: F292 按字母定位 —— 纯函数

**Files:**
- Create: `crates/mullion-app/src/files/type_ahead.rs`
- Modify: `crates/mullion-app/src/files/mod.rs`（`pub mod type_ahead;`）

- [ ] **Step 1: 写文件（测试与实现一起，先跑一遍确认测试真的红）**

```rust
//! F292:文件面板「按字母定位」的纯判据。零 IO、零 egui;`now` 注入,
//! 与 `input::click_kind` 同一套写法。
//!
//! 行为照 Windows 资源管理器:按一个字符跳到**下一个**以它开头的条目(从
//! 光标之后找起,到底绕回);1 秒内连按累积成前缀;同一字母反复按 = 在同
//! 字母条目间循环。目录/文件一视同仁,按当前显示顺序找。

use std::time::Instant;

use mullion_ssh::sftp::{Entry, RemotePath};

/// 两次按键相隔不超过这个毫秒数就累积成前缀。
pub const TYPE_AHEAD_MS: u128 = 1000;

/// 上一次按键留下的状态。存在 `PaneState` 上(两栏各一份)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeAhead {
    pub prefix: String,
    pub at: Instant,
}

/// winit 的 `Key::Character(s)` 里哪些算「可打印字符」:恰好一个 `char`,
/// 不是空白也不是控制字符。空格**不算**(留给将来的选中切换)。
pub fn key_char(s: &str) -> Option<char> {
    let mut it = s.chars();
    let c = it.next()?;
    if it.next().is_some() || c.is_whitespace() || c.is_control() {
        return None;
    }
    Some(c)
}

/// 下一个该落到哪一行,以及更新后的状态。`rows` 是这一帧**画出来**的行
/// (`PaneState::rows()`,已过隐藏项过滤 + 排序),`cursor` 是当前光标行。
///
/// 起点规则:单字符(含循环模式)从光标**之后**找;多字符前缀从光标**本身**
/// 找起 —— 用户敲 `d`、`o` 时光标已经在 `d...` 上,再跳过它就把 `do...`
/// 本身漏了。找不到返回 `None`,状态照样更新(下一键仍按累积算)。
pub fn next_index(
    rows: &[&Entry],
    cursor: Option<&RemotePath>,
    prev: Option<&TypeAhead>,
    now: Instant,
    ch: char,
) -> (Option<usize>, TypeAhead) {
    let prefix = match prev {
        Some(p) if now.duration_since(p.at).as_millis() <= TYPE_AHEAD_MS => {
            let mut s = p.prefix.clone();
            s.push(ch);
            s
        }
        _ => ch.to_string(),
    };
    let first = prefix.chars().next().unwrap_or(ch);
    let cycling = prefix.chars().count() > 1 && prefix.chars().all(|c| c == first);
    let needle: String = if cycling {
        first.to_lowercase().collect()
    } else {
        prefix.chars().flat_map(char::to_lowercase).collect()
    };
    let state = TypeAhead { prefix, at: now };
    if rows.is_empty() {
        return (None, state);
    }
    let at = cursor.and_then(|c| rows.iter().position(|e| e.name == *c));
    let single = cycling || state.prefix.chars().count() == 1;
    let start = match at {
        Some(i) if single => i + 1,
        Some(i) => i,
        None => 0,
    };
    let n = rows.len();
    let hit = (0..n).map(|k| (start + k) % n).find(|&ix| {
        let name: String = rows[ix].name.display().chars().flat_map(char::to_lowercase).collect();
        name.starts_with(&needle)
    });
    (hit, state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn entry(name: &str) -> Entry {
        Entry {
            name: RemotePath::from_bytes(name.as_bytes().to_vec()),
            kind: mullion_ssh::sftp::EntryKind::File,
            size: 0,
            mtime: 0,
            mode: 0o644,
            uid: 0,
            gid: 0,
            link_target: None,
        }
    }

    /// 单字符从光标**之后**找,到底绕回;大小写不敏感。
    ///
    /// 自证会变红:把 `single` 那条 `i + 1` 改成 `i`(光标在 `apple` 上按 a
    /// 会原地不动);或把小写化删掉(`B` 找不到 `banana`)。
    #[test]
    fn a_single_letter_jumps_to_the_next_match_after_the_cursor_and_wraps() {
        let rows_owned = [entry("apple"), entry("banana"), entry("avocado")];
        let rows: Vec<&Entry> = rows_owned.iter().collect();
        let t0 = Instant::now();
        let (hit, st) = next_index(&rows, Some(&rows[0].name), None, t0, 'a');
        assert_eq!(hit, Some(2), "从 apple 之后找,落到 avocado");
        let (hit, _) = next_index(&rows, Some(&rows[2].name), Some(&st), t0 + Duration::from_millis(5000), 'a');
        assert_eq!(hit, Some(0), "超时后是新的单字符,绕回 apple");
        let (hit, _) = next_index(&rows, None, None, t0, 'B');
        assert_eq!(hit, Some(1), "没有光标从头找;大写也认");
        let (hit, _) = next_index(&rows, None, None, t0, 'z');
        assert_eq!(hit, None);
    }

    /// 1 秒内连按累积成前缀,从光标**本身**找起(光标已在 `d...` 上时不跳过它)。
    ///
    /// 自证会变红:把多字符那条 `Some(i) => i` 改成 `i + 1`(`do` 会跳过
    /// `dog` 落到 `door`);或把 `TYPE_AHEAD_MS` 改成 0。
    #[test]
    fn letters_within_a_second_accumulate_into_a_prefix() {
        let rows_owned = [entry("cat"), entry("dog"), entry("door"), entry("dot")];
        let rows: Vec<&Entry> = rows_owned.iter().collect();
        let t0 = Instant::now();
        let (hit, st) = next_index(&rows, None, None, t0, 'd');
        assert_eq!(hit, Some(1));
        let (hit, st) = next_index(&rows, Some(&rows[1].name), Some(&st), t0 + Duration::from_millis(300), 'o');
        assert_eq!(hit, Some(1), "`do` 仍然是 dog 自己");
        assert_eq!(st.prefix, "do");
        let (hit, st) = next_index(&rows, Some(&rows[1].name), Some(&st), t0 + Duration::from_millis(600), 'o');
        assert_eq!(hit, Some(2), "`doo` → door");
        assert_eq!(st.prefix, "doo");
        let (hit, _) = next_index(&rows, Some(&rows[2].name), Some(&st), t0 + Duration::from_millis(900), 'x');
        assert_eq!(hit, None, "`doox` 没有,光标不动");
    }

    /// 同一字母反复按 = 在同字母条目间循环(`dd` 不是找 `dd` 开头,是第二个 d)。
    ///
    /// 自证会变红:删掉 `cycling` 判据(`dd` 会去找以 `dd` 开头的,返回 None)。
    #[test]
    fn repeating_the_same_letter_cycles_through_that_letter() {
        let rows_owned = [entry("dog"), entry("door"), entry("dot"), entry("egg")];
        let rows: Vec<&Entry> = rows_owned.iter().collect();
        let t0 = Instant::now();
        let (hit, st) = next_index(&rows, None, None, t0, 'd');
        assert_eq!(hit, Some(0));
        let (hit, st) = next_index(&rows, Some(&rows[0].name), Some(&st), t0 + Duration::from_millis(200), 'd');
        assert_eq!(hit, Some(1));
        let (hit, st) = next_index(&rows, Some(&rows[1].name), Some(&st), t0 + Duration::from_millis(400), 'd');
        assert_eq!(hit, Some(2));
        let (hit, _) = next_index(&rows, Some(&rows[2].name), Some(&st), t0 + Duration::from_millis(600), 'd');
        assert_eq!(hit, Some(0), "绕回");
    }

    /// 空列表没有答案但状态照样记。
    #[test]
    fn an_empty_list_has_no_answer() {
        let (hit, st) = next_index(&[], None, None, Instant::now(), 'a');
        assert_eq!(hit, None);
        assert_eq!(st.prefix, "a");
    }

    /// `key_char`:单个可打印字符才算;空格、制表、多字符串、控制字符都不算。
    ///
    /// 自证会变红:删掉 `is_whitespace()` 那一项(空格会开始定位)。
    #[test]
    fn only_a_single_printable_character_counts() {
        assert_eq!(key_char("a"), Some('a'));
        assert_eq!(key_char("中"), Some('中'));
        assert_eq!(key_char(" "), None);
        assert_eq!(key_char("\t"), None);
        assert_eq!(key_char("ab"), None);
        assert_eq!(key_char(""), None);
        assert_eq!(key_char("\u{1b}"), None);
    }
}
```

`files/mod.rs` 加 `pub mod type_ahead;`。

- [ ] **Step 2: 跑测试**

Run: `cargo test -p mullion-app --lib files::type_ahead 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: 5 passed。若有红，先看判据再改实现（不改测试）。

- [ ] **Step 3: Commit**

```bash
git add crates/mullion-app/src/files/type_ahead.rs crates/mullion-app/src/files/mod.rs
git commit -m "feat(app): 按字母定位的纯判据 (F292)"
```

---

## Task 8: F292 接进 `PaneState` 与 `handle_panel_key`

**Files:**
- Modify: `crates/mullion-app/src/files/state.rs`（字段 + `new` + 方法）
- Modify: `crates/mullion-app/src/app.rs`（`handle_panel_key` :7364 的 `_ => {}` 之前加臂）

- [ ] **Step 1: 写失败测试**

`state.rs` 的 `mod tests`（先看该文件现有测试怎么造 `PaneState` 与 `Entry`，照抄辅助）：

```rust
    /// F292:`type_ahead` 命中 → 单选到那一条 + 置 `scroll_to`(F218 同款,
    /// 渲染侧下一帧居中);状态记下来给下一键累积。没命中 → 选中不动,但
    /// 状态照记。
    ///
    /// 自证会变红:删掉 `self.scroll_to = Some(..)`(选中了却滚不到,虚拟
    /// 滚动下用户看不见);或删掉 `self.type_ahead = Some(st)`(第二键
    /// 永远累积不起来)。
    #[test]
    fn type_ahead_selects_the_hit_and_asks_the_view_to_scroll_there() {
        let mut st = PaneState::new(RemotePath::from_bytes(b"/".to_vec()));
        st.entries = vec![test_entry(b"apple"), test_entry(b"banana"), test_entry(b"bean")];
        let t0 = std::time::Instant::now();
        assert!(st.type_ahead('b', t0));
        assert_eq!(st.cursor.as_ref().map(|c| c.as_bytes()), Some(&b"banana"[..]));
        assert_eq!(st.scroll_to.as_ref().map(|c| c.as_bytes()), Some(&b"banana"[..]));
        assert_eq!(st.selected.len(), 1);
        assert!(st.type_ahead('e', t0 + std::time::Duration::from_millis(200)));
        assert_eq!(st.cursor.as_ref().map(|c| c.as_bytes()), Some(&b"bean"[..]), "`be` 累积");
        assert!(!st.type_ahead('z', t0 + std::time::Duration::from_millis(400)));
        assert_eq!(st.cursor.as_ref().map(|c| c.as_bytes()), Some(&b"bean"[..]), "没命中不动");
    }

    /// 隐藏项关着时,`.`开头的条目不参与定位(它们根本没画出来)。
    ///
    /// 自证会变红:把 `type_ahead` 里的 `self.rows()` 换成遍历 `self.entries`。
    #[test]
    fn type_ahead_only_walks_the_rows_that_are_actually_shown() {
        let mut st = PaneState::new(RemotePath::from_bytes(b"/".to_vec()));
        st.show_hidden = false;
        st.entries = vec![test_entry(b".bashrc"), test_entry(b"bin")];
        assert!(st.type_ahead('.', std::time::Instant::now()) == false);
        assert!(st.type_ahead('b', std::time::Instant::now()));
        assert_eq!(st.cursor.as_ref().map(|c| c.as_bytes()), Some(&b"bin"[..]));
    }
```

`app.rs` 的 `mod tests`：

```rust
    /// **接线守护 / F292**:`handle_panel_key` 把裸字符键交给
    /// `PaneState::type_ahead`,且**排除** Ctrl/Alt/Super 组合 —— 否则
    /// `Ctrl+H`(切隐藏)这类没被上面 `if mods.control_key()` 段 return 掉的
    /// 组合键会顺手在列表里跳一下。
    ///
    /// 自证会变红:把那条臂的 `!mods.control_key()` 删掉;或把 `type_ahead(`
    /// 换成 `select_only(`。
    #[test]
    fn printable_keys_in_the_files_panel_drive_the_type_ahead_without_modifiers() {
        let body = strip_comments(body_of(prod_src(), "fn handle_panel_key("));
        let arm = body
            .find(concat!("type_ahead::", "key_char("))
            .expect("handle_panel_key 没接 key_char");
        let guard = &body[arm.saturating_sub(400)..arm];
        assert!(guard.contains("!mods.control_key()"), "没排除 Ctrl");
        assert!(guard.contains("!mods.alt_key()"), "没排除 Alt");
        assert!(guard.contains("!mods.super_key()"), "没排除 Super");
        assert!(body.contains(".type_ahead("), "没调 PaneState::type_ahead");
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib files::state::tests::type_ahead tests::printable_keys 2>&1 | grep -E "test result|error|panicked" | head`
Expected: 编译错误（无字段/方法）。

- [ ] **Step 3: 实现**

`state.rs`：字段

```rust
    /// F292:按字母定位的上一键状态(前缀 + 时刻)。`None` = 没按过 / 已超时
    /// 不重要(超时判据在 `type_ahead::next_index` 里比时间,不清这个字段)。
    pub type_ahead: Option<super::type_ahead::TypeAhead>,
```

`new` 里 `type_ahead: None,`。方法（放 `select_only` 附近）：

```rust
    /// F292:按下一个可打印字符。返回有没有跳到某一行。
    ///
    /// 走 `rows()` 不走 `entries`:隐藏项过滤和排序都在那一层,遍历原始
    /// `entries` 会跳到一条**画不出来**的行上。命中后与 F218 同款:单选 +
    /// `scroll_to`(虚拟滚动下不置这个,跨大距离跳转用户看不见)。
    pub fn type_ahead(&mut self, ch: char, now: std::time::Instant) -> bool {
        let rows = self.rows();
        let (hit, st) =
            super::type_ahead::next_index(&rows, self.cursor.as_ref(), self.type_ahead.as_ref(), now, ch);
        let name = hit.map(|ix| rows[ix].name.clone());
        drop(rows);
        self.type_ahead = Some(st);
        let Some(name) = name else {
            return false;
        };
        self.select_only(&name);
        self.scroll_to = Some(name);
        true
    }
```

`app.rs` `handle_panel_key` 的 `match key` 里、`_ => {}` 之前：

```rust
            // F292:裸字符键 = 按字母定位。**排除**带 Ctrl/Alt/Super 的组合 ——
            // 上面 `if mods.control_key()` 那段只 return 了 h/n/c/x/v 五个,
            // 别的 Ctrl+字母会落到这里。Shift 允许(大写字母)。
            WinitKey::Character(s)
                if !mods.control_key() && !mods.alt_key() && !mods.super_key() =>
            {
                let Some(ch) = crate::files::type_ahead::key_char(s) else {
                    return;
                };
                if let Some(state) = self
                    .tabs
                    .by_generation_mut(generation)
                    .and_then(|t| t.content.files_panel_mut())
                    .map(|f| f.active_state_mut())
                {
                    if state.type_ahead(ch, std::time::Instant::now()) {
                        mark_ui_dirty!(self.ui_dirty);
                    }
                }
            }
```

- [ ] **Step 4: 跑测试**

Run: `cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: 全过。留意 `PanelFrame::default()` 相关的既有守护（`Option` 的 `None` 合格）。

- [ ] **Step 5: Commit**

```bash
git add crates/mullion-app/src/files/state.rs crates/mullion-app/src/app.rs
git commit -m "feat(app): 文件面板两栏按字母定位到条目 (F292)"
```

---

## Task 9: spec 登记 + 全绿 + 变异验证

**Files:**
- Modify: `spec.md`（F288 那行之后加 F290/F291/F292 三行）

- [ ] **Step 1: spec 三行**（照 F288 的格式：ID / 需求 / 优先级 / 验收标准，验收标准里写落点 + 守护 + 否掉的备选）

F290 备选记录：否掉「叶子挂子区域」（要让 Workspace/渲染/输入路由/焦点/IME 都认识第二种终端区，T 系列陷阱高发）；否掉「进 tmux 开 window」（污染项目专属 tmux 的窗口列表、抢 `Ctrl+B n`）；否掉「跟随父 pane 目录」（用户敲一半命令时被动跳目录）。
F291 备选记录：否掉「`.docx` 不带星也当后缀」（子序列在含 `.` 时变语义，用户分不清哪种在生效）；否掉引 `globset`。
F292 备选记录：否掉只做远端栏（纯列表游标操作，不涉及网络）。

- [ ] **Step 2: 全绿**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log | grep -v "0 failed" ; cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3; cargo fmt --check
```
Expected: 无 FAILED，clippy 无输出，fmt 无 diff。

- [ ] **Step 3: 变异验证（先确认工作区干净 `git status`）**

逐条做、每条 `git checkout -- <file>` 复原；记录哪条测试红了：

1. `open_drawer` 的 `Dir::Vertical` → `Horizontal` → 期望 `opening_a_drawer_splits_...` 红。
2. `close_pane` 删 `if self.is_drawer(id)` → 期望 `closing_a_drawer_through_the_generic_path...` 红。
3. `close_pane` 删 `ids.retain(|id| !self.is_drawer(*id))` → 期望 `rearranging_after_a_close_keeps...` 红。
4. `apply_preset` 的 `statuses_without_drawers()` → `statuses()` → 期望 `presets_count_panes_without...` 红。
5. `pump` 删 `reap_dead_drawers()` → 期望 `a_drawer_whose_link_died...` 红。
6. `pending_for_drawer` 去掉 `shell_quote` → 期望 `a_drawer_plan_is_a_single_quoted_cd` 红。
7. `window_event` 删 `drawer_hotkey_event` 那段 → 期望 `drawer_shortcut_is_swallowed...` 红。
8. `PaneOpened` 臂把 `pending_for_drawer` 分支删掉 → 期望 `a_freshly_opened_drawer_gets_the_cd_plan...` 红。
9. `snapshot_tabs_of` 换回 `t.ws.tree()` → 期望 `the_layout_snapshot_is_taken_from...` 红。
10. `accept` 里 `query_matches(` → `matches(` → 期望 `the_walk_dispatches_through_query_matches` 红。
11. `glob_matches` 末尾 `pi == p.len()` → `true` → 期望 `a_glob_matches_the_whole_name...` 红。
12. `next_index` 里 `single` 的 `i + 1` → `i` → 期望 `a_single_letter_jumps...` 红。
13. `PaneState::type_ahead` 删 `self.scroll_to = Some(name)` → 期望 `type_ahead_selects_the_hit_and_asks_the_view_to_scroll_there` 红。
14. `handle_panel_key` 那条臂删 `!mods.control_key()` → 期望 `printable_keys_in_the_files_panel...` 红。

任何一条**没红**：那条守护恒绿，回去改守护（判据放到测得着的层），不许放过。

- [ ] **Step 4: Commit**

```bash
git add spec.md
git commit -m "docs(spec): 登记 F290 命令抽屉 / F291 搜索通配 / F292 按字母定位"
```

---

## Task 10: 发版（release-windows 一条龙）

按 `.claude/skills/release-windows/SKILL.md` 做：升 `Cargo.toml` 到 0.1.117 → 跑绿 → 交叉编译 + objdump 验收 → 签名 → GitHub Release（走代理）→ 报链接 + 人工验收清单。

**人工验收清单（写进 Release notes）**：
- F290:焦点在跑着 Claude Code 的 pane 上按 `` Ctrl+` ``:底下出现 1/5 高的抽屉、标题条带「抽屉」、焦点在抽屉、几秒内自动 `cd` 到 Claude Code 所在目录(标题条目录名一致);再按一次抽屉关掉、焦点回原 pane、**其余分屏布局一动不动**。点布局按钮(两栏/三栏)抽屉仍在原 pane 底下。关掉父 pane(×)抽屉一起消失。关掉整个程序再恢复现场,抽屉不出现。拔网线/断代理:抽屉消失,其余 pane 进入重连。
- F291:远端栏放大镜里输 `*.docx` 回车,只列 `.docx` 结尾的;输 `docx`(无星)行为与之前一致。
- F292:远端栏/本地栏点一下让面板有焦点,按 `b` 跳到第一个 b 开头的、再按 `b` 跳下一个、快速按 `b` `i` 跳到 `bi...`;大列表里跳转后目标行在视野里。搜索条/改名框开着时按字母只进输入框,列表不跳。
