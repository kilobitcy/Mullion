# F322~F325 取消归档强调 / 国家格 hover / 拖标题条换位 / 会话建凭据 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 四项 UI 改进：已归档项目的「取消归档」按钮强调；pane 标题条国家格 hover 反馈；拖标题条交换两块 pane 位置；会话编辑器里把内联认证一键转成去重的共享凭据。

**Architecture:** 依赖方向不变（`app → core/store`）。换位的树操作写成 `mullion-core` 纯函数（零 UI），`Workspace` 做抽屉感知的包装，`pane_title` 只产出「谁拖到谁身上」的意图，`app.rs` 施加。凭据去重判据写在 `mullion-store`（纯同步），app 层负责把「库里已存密文 + 表单里碰过的字段」合并后调用。

**Tech Stack:** Rust workspace、egui 0.30、winit 0.30。

**编号：** F322 取消归档强调 / F323 国家格 hover / F324 拖标题条换位 / F325 会话建凭据。提交信息与测试名带编号。

**已拍板的决策（grill 结论，不再重议）：**
- F322：仅归档态，「取消归档」文字+描边用 accent，仍是空心按钮；排到按钮行最左（「打开」之前）。不引入 accent 实心。
- F323：悬停底色 `panel_head`、文字 `fg_strong`、光标手形；常态纯文字。
- F324：交换语义（非停靠）；起点=标题条空白处（拖动阈值后才开始，按钮/国家格不触发）；落点=目标 pane 整块；目标叠 accent 半透明遮罩，光标抓取手形；拖到自己/窗外/Esc=取消；**抽屉不可拖、不可作落点**；交换后焦点跟被拖的 pane；`分屏 N` 序号按位置重排；抽屉随其父 pane 一起走（整棵单元子树交换）。
- F325：认证为 `Inline`（本会话独有）时显示「创建凭据」；按**编辑缓冲当前内容**（含未保存）生成；去重判据 = 用户名 + 认证类型 + 密码/私钥/口令内容全等，命中则复用并提示，不新建；凭据名默认 `用户@主机`，重名加 ` (2)`；缓冲改为 `Shared`+该 id，会话仍需手动「保存」。

**领域陷阱对照（改完要能指出跑的是哪个测试）：**
- T4：换位后两块 pane 尺寸变化，须各发 `window_change`（走既有 `apply_geometry` 每帧比对 `last_grid`，不要手发）；守护测试 Task 4 的 `swapping_panes_changes_geometry_so_apply_geometry_will_resize`。
- T5/T8：拖动期间指针必须归 egui（`wants_pointer_input` 在 egui 拖动时为真），不得漏到终端造成划选/鼠标上报；键盘不受影响。Task 5 的接线守护。
- T9：新增 UI 字符串只用 ASCII 或已登记 `ui::glyphs::VERIFIED` 的字形；本计划新增字符串均为汉字+ASCII，仍须跑 `tests/glyph_whitelist.rs`。
- T3：不得因本计划引入每帧重绘；拖动状态变化才 `request_repaint`。

---

## File Structure

| 文件 | 职责 | 动作 |
|---|---|---|
| `crates/mullion-app/src/ui/project_manager.rs` | 取消归档按钮强调 | 改 (~L430-440) |
| `crates/mullion-app/src/ui/pane_title.rs` | 国家格 hover；标题条拖动意图；落点判定纯函数 | 改 |
| `crates/mullion-core/src/layout.rs` | `swap_units` 纯函数 | 改 |
| `crates/mullion-app/src/shell/workspace/mod.rs` | `Workspace::swap_panes`（抽屉感知） | 改 |
| `crates/mullion-app/src/ui/mod.rs` | `UiActions.swap_panes` 字段并由 `title_action` 填 | 改 (~L739, ~L1304) |
| `crates/mullion-app/src/app.rs` | `has_real_action` 登记；`apply_layout_actions` 施加；凭据请求施加 | 改 |
| `crates/mullion-store/src/vault.rs` | `find_credential_matching` | 改 |
| `crates/mullion-app/src/ui/session_manager/fields.rs` | 「创建凭据」按钮 | 改 (`auth`, ~L890-1040) |
| `crates/mullion-app/src/ui/mod.rs` | `UiState.credential_from_session_request` | 改 |
| `crates/mullion-app/src/ui/session_manager/buffer.rs` | 缓冲 → 凭据草稿的合并纯函数 | 改 |
| `Cargo.toml`/各 crate | 版本 bump | 发版时 |

---

### Task 1: F322 取消归档强调

**Files:**
- Modify: `crates/mullion-app/src/ui/project_manager.rs`（按钮行，约 L405-440；`archive_button_label` 约 L778）
- Test: 同文件 `mod tests`

- [ ] **Step 1: 读现状。** 先 `sed -n 395,445p` 看按钮行完整顺序（打开 / 归档 / 删除项目，`is_archived` 变量来源）。

- [ ] **Step 2: 写失败测试。** 抽纯函数 `archive_button_emphasis(archived: bool) -> bool`（归档态才强调），测试：

```rust
/// F322:只有「取消归档」(已归档态)强调;「归档」保持普通按钮。
/// 自证会变红:把 `archive_button_emphasis` 改成恒 `true` / 恒 `false`。
#[test]
fn only_the_unarchive_button_is_emphasised() {
    assert!(archive_button_emphasis(true));
    assert!(!archive_button_emphasis(false));
}
```

Run: `cargo test -p mullion-app archive_button -- --nocapture` → FAIL（函数不存在）。

- [ ] **Step 3: 实现。** 在 `archive_button_label` 旁加：

```rust
/// F322:已归档项目的「取消归档」是撤销归档的唯一入口,要比并排的
/// 「打开」「删除项目」显眼。**只用 accent 描边+文字,不用实心** ——
/// 全场唯一的 accent 实心按钮是会话编辑器的「保存并连接」(见 `add_button` 注释)。
pub(crate) fn archive_button_emphasis(archived: bool) -> bool {
    archived
}
```

按钮行改为：已归档时先画「取消归档」（紧随 `ui.horizontal` 起点、在「打开」之前），用

```rust
let accent = crate::theme::c32(t.accent);
let b = egui::Button::new(egui::RichText::new(archive_button_label(true)).color(accent))
    .stroke(egui::Stroke::new(1.0, accent));
```

未归档时保持原位置不变。注意 `ProjectIntent::SetArchived(id, !is_archived)` 逻辑不动。

- [ ] **Step 4:** `cargo test -p mullion-app archive_button` PASS；`cargo test -p mullion-app --test form_guidelines` PASS（表单规范守护）。

- [ ] **Step 5: Commit**

```bash
git add crates/mullion-app/src/ui/project_manager.rs
git commit -m "feat(app): 已归档项目的「取消归档」按钮 accent 描边并排到最左 (F322)"
```

---

### Task 2: F323 国家格 hover 反馈

**Files:**
- Modify: `crates/mullion-app/src/ui/pane_title.rs`（`country_cell`，约 L310；调用点约 L520）
- Test: 同文件 `mod tests`

- [ ] **Step 1: 写失败测试。** 先抽纯函数决定颜色（避免依赖 painter 断言）：

```rust
/// F323:国家格悬停时文字变 `fg_strong`;常态保持传入的状态色。
/// 自证会变红:`country_text_color` 里去掉 `hovered` 分支。
#[test]
fn the_country_cell_brightens_on_hover() {
    let t = crate::theme::Theme::default();
    let base = egui::Color32::from_rgb(1, 2, 3);
    assert_eq!(country_text_color(&t, base, false), base);
    assert_eq!(country_text_color(&t, base, true), theme::c32(t.fg_strong));
}
```

（`Theme::default()` 若不存在，用文件里其它测试构造 `Theme` 的同一方式。）

- [ ] **Step 2:** 跑确认 FAIL。

- [ ] **Step 3: 实现。** 

```rust
fn country_text_color(t: &Theme, base: egui::Color32, hovered: bool) -> egui::Color32 {
    if hovered { theme::c32(t.fg_strong) } else { base }
}
```

`country_cell` 加参数 `t: &Theme`，改为：先 `allocate_exact_size(galley.size()+vec2(6.0,2.0), Sense::hover())` 留出内边距，再 `ui.interact(rect, id, Sense::click())`；`resp.hovered()` 时 `ui.painter().rect_filled(rect, 3.0, theme::c32(t.panel_head))` 并 `ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand)`；文字用 `country_text_color` 重新 layout 后画在 `rect.center()-size/2`。调用点（约 L520）补传 `t`。**id 与 `ctx.read_response(country_id(..))` 的既有测试保持可用**（不改 id）。

- [ ] **Step 4:** `cargo test -p mullion-app pane_title` 全 PASS（含既有 `country` 相关测试：确认它们没被矩形变大打破；若 stats_fit 测的宽度含此格，按新宽度同步调整测量并在提交说明里写明）。

- [ ] **Step 5: Commit** `feat(app): 标题条国家格悬停有底色与手形光标 (F323)`

---

### Task 3: F324-a core `swap_units`

**Files:**
- Modify: `crates/mullion-core/src/layout.rs`（在 `close_pane` 之后加；测试加在 `mod tests` 末尾）

「单元」= 一组叶子 id 构成的**最小子树**（普通 pane 单元 = `[P]`；带抽屉的单元 = `[P, D]`）。core 不认识抽屉，只认「叶子集合 → 恰好覆盖它们的子树」。

- [ ] **Step 1: 写失败测试**（使用文件内已有的 `leaf/hsplit/vsplit` 辅助）：

```rust
/// F324:两块普通 pane 互换,树形与比例不动,只有叶子 id 对调。
#[test]
fn swapping_two_leaves_keeps_shape_and_ratios() {
    let mut t = hsplit(0.3, leaf(1), vsplit(0.6, leaf(2), leaf(3)));
    assert!(swap_units(&mut t, &[PaneId(1)], &[PaneId(3)]));
    assert_eq!(t, hsplit(0.3, leaf(3), vsplit(0.6, leaf(2), leaf(1))));
}

/// 带抽屉的单元整棵搬走:[2,9] 是 vsplit(2,9) 子树,与叶子 1 互换。
#[test]
fn a_unit_with_a_drawer_moves_as_one_subtree() {
    let mut t = hsplit(0.5, leaf(1), vsplit(0.8, leaf(2), leaf(9)));
    assert!(swap_units(&mut t, &[PaneId(1)], &[PaneId(2), PaneId(9)]));
    assert_eq!(t, hsplit(0.5, vsplit(0.8, leaf(2), leaf(9)), leaf(1)));
}

/// 找不到恰好覆盖的子树(叶子集合凑不成一棵子树)/ 两单元重叠 / 空集:树不变,返回 false。
#[test]
fn an_unmatched_or_overlapping_unit_leaves_the_tree_untouched() {
    let orig = hsplit(0.5, leaf(1), vsplit(0.5, leaf(2), leaf(3)));
    let mut t = orig.clone();
    assert!(!swap_units(&mut t, &[PaneId(1)], &[PaneId(1)]));
    assert!(!swap_units(&mut t, &[PaneId(1)], &[PaneId(1), PaneId(2)]));
    assert!(!swap_units(&mut t, &[PaneId(1), PaneId(3)], &[PaneId(2)])); // 1,3 非同一子树
    assert!(!swap_units(&mut t, &[], &[PaneId(2)]));
    assert!(!swap_units(&mut t, &[PaneId(7)], &[PaneId(2)]));
    assert_eq!(t, orig);
}

/// 换位后几何:同一棵形状,叶子顺序对调,面积分配不变。
#[test]
fn swap_keeps_the_rect_set_and_only_relabels_owners() {
    let mut t = hsplit(0.25, leaf(1), leaf(2));
    let before = compute_rects(&t, AREA);
    swap_units(&mut t, &[PaneId(1)], &[PaneId(2)]);
    let after = compute_rects(&t, AREA);
    assert_eq!(before[0].1, after[0].1);
    assert_eq!(before[1].1, after[1].1);
    assert_eq!(after[0].0, PaneId(2));
    assert_eq!(after[1].0, PaneId(1));
}
```

Run: `cargo test -p mullion-core swap` → FAIL（未定义）。

- [ ] **Step 2: 实现**

```rust
/// F324:交换两个「单元」的位置。单元 = 叶子 id 集合恰好覆盖的**最小子树**
/// (普通 pane = 单叶子;带抽屉的 pane = 父+抽屉两片叶子)。
///
/// 为什么不直接对调两个叶子 id:抽屉是挂在父 pane 下面的 `Split`,只对调
/// 父叶子会让抽屉留在原地、归属却变了。core 不认识「抽屉」,只认「这组叶子
/// 凑成哪棵子树」,由调用方(`Workspace::swap_panes`)告诉它哪组是一个单元。
///
/// 返回是否交换成功。以下情况返回 `false` 且树不变:任一集合为空、两集合
/// 有交集、任一集合找不到恰好覆盖它的子树(如两叶子不同属一棵子树)。
pub fn swap_units(root: &mut Node, x: &[PaneId], y: &[PaneId]) -> bool {
    if x.is_empty() || y.is_empty() || x.iter().any(|a| y.contains(a)) {
        return false;
    }
    if !covers(root, x) || !covers(root, y) {
        return false;
    }
    let mut taken_x = None;
    let mut taken_y = None;
    take_subtree(root, x, &mut taken_x);
    take_subtree(root, y, &mut taken_y);
    // 两个都取出后,原位各留一个占位叶子;按各自占位写回对方。
    // 实现见 `take_subtree`/`put_back`:占位用 `Node::Leaf(PaneId(u32::MAX))` 与 `PaneId(u32::MAX - 1)`。
    put_back(root, PaneId(u32::MAX), taken_y.take().unwrap());
    put_back(root, PaneId(u32::MAX - 1), taken_x.take().unwrap());
    true
}
```

辅助函数（完整实现，不得留空）：
- `leaf_set(node) -> Vec<PaneId>`（用已有 `collect_leaves`）。
- `covers(node, ids) -> bool`：递归找到一棵子树，其叶子集合（无序）等于 `ids`。
- `take_subtree(node, ids, out)`：找到那棵子树，`*out = Some(std::mem::replace(sub, Node::Leaf(PaneId(MARK))))`，MARK 对 x 用 `u32::MAX`、对 y 用 `u32::MAX-1`（给 `take_subtree` 传占位 id 参数）。
- `put_back(node, mark, sub)`：找到值为 `mark` 的叶子，替换为 `sub`。

（占位 id 方案规避了对同一 `&mut` 树同时持有两个可变子树引用的借用问题；`PaneId` 的真实值永远不会取到 `u32::MAX`。）

- [ ] **Step 3:** `cargo test -p mullion-core` 全 PASS。`cargo clippy -p mullion-core --all-targets -- -D warnings` 无输出。

- [ ] **Step 4: Commit** `feat(core): 布局树新增 swap_units 交换两个 pane 单元 (F324)`

---

### Task 4: F324-b `Workspace::swap_panes`

**Files:**
- Modify: `crates/mullion-app/src/shell/workspace/mod.rs`（`close_pane` 附近；测试进同文件 tests，沿用已有 `Workspace` 测试构造方式——先 `grep -n "fn ws_with\|fn test_ws\|fn new_ws" shell/workspace/mod.rs` 找辅助）

- [ ] **Step 1: 写失败测试**

```rust
/// F324:两块普通 pane 互换,返回 true,焦点给被拖的那块(from)。
#[test]
fn swap_panes_exchanges_positions_and_focuses_the_dragged_one() { /* 用 apply_preset 造 2 屏 [a,b];swap_panes(a,b) 后 leaves()==[b,a],focus()==a */ }

/// 抽屉是附属物:带抽屉的 pane 与普通 pane 互换,抽屉跟着父 pane 走。
#[test]
fn a_drawer_travels_with_its_parent_when_swapped() { /* 2 屏,给 a 开抽屉 d;swap_panes(a,b);leaves 里 a,d 仍相邻且在原 b 的位置 */ }

/// 抽屉本身既不能当起点也不能当落点;自己换自己、未知 id 都拒绝且树不变。
#[test]
fn drawers_unknown_ids_and_self_are_refused() { /* swap_panes(d,b)/ (b,d)/(a,a)/(a,PaneId(99)) 全 false,tree 不变 */ }

/// T4 守护:换位让两块不同尺寸的 pane 几何互换,`last_grid` 与新几何不符,
/// 下一帧 `apply_geometry` 就会各发一次 window_change。这里钉住「几何确实变了」。
#[test]
fn swapping_panes_changes_geometry_so_apply_geometry_will_resize() { /* 0.3/0.7 的两屏,swap 前后 compute 出的 a 的 rect 不同 */ }
```

实现测试时写出完整代码，参考同文件现有抽屉测试（~L1944 附近 `open_drawer` 的用法）。

- [ ] **Step 2:** 跑确认 FAIL。

- [ ] **Step 3: 实现**

```rust
/// F324:拖标题条换位。`from` 被拖,`to` 是落点;两者都必须是**非抽屉**的真 pane
/// 且互不相同,否则什么都不发生。带抽屉的 pane 连抽屉整棵子树一起走。
/// 焦点给 `from`(用户刚亲手操作的那块,同 F286/F156-b 的「亲手操作焦点跟过去」)。
/// 只改树,不碰 PTY —— 几何变了,下一帧 `apply_geometry` 比对 `last_grid`
/// 自会各发一次 `window_change`(T4)。
pub fn swap_panes(&mut self, from: PaneId, to: PaneId) -> bool {
    if from == to || self.is_drawer(from) || self.is_drawer(to) {
        return false;
    }
    if self.pane(from).is_none() || self.pane(to).is_none() {
        return false;
    }
    let unit = |id: PaneId| -> Vec<PaneId> {
        let mut v = vec![id];
        v.extend(self.drawer_of(id));
        v
    };
    let (ux, uy) = (unit(from), unit(to));
    if !swap_units(&mut self.tree, &ux, &uy) {
        return false;
    }
    self.focus = from;
    true
}
```

`swap_units` 需加入文件顶部 `use mullion_core::layout::{...}`。`pane()` 的签名以现有为准（Task 内核对）。

- [ ] **Step 4:** `cargo test -p mullion-app swap_panes drawer_travels drawers_unknown swapping_panes_changes` PASS；再跑 `cargo test -p mullion-app workspace` 确认既有抽屉/F241 测试未被打破。

- [ ] **Step 5: Commit** `feat(app): Workspace::swap_panes 抽屉随父走的 pane 换位 (F324)`；正文写明跑了 T4 守护测试。

---

### Task 5: F324-c 标题条拖动意图 + 接线

**Files:**
- Modify: `crates/mullion-app/src/ui/pane_title.rs`（`TitleAction` 加 `swap: Option<(PaneId, PaneId)>`；`show`）
- Modify: `crates/mullion-app/src/ui/mod.rs`（`UiActions` 加 `swap_panes: Option<(PaneId, PaneId)>`，约 L739；填充约 L1304）
- Modify: `crates/mullion-app/src/app.rs`（`has_real_action` 约 L17584 加 `|| a.swap_panes.is_some()`；`apply_layout_actions` 约 L16103 施加）
- Test: pane_title.rs tests；app.rs 接线守护（仿 L22799 `refresh_node_pane` 的源码切片守护——**先读 `source-slice-guards-do-not-strip-comments.md` 记忆里的注意点再写**）

- [ ] **Step 1: 纯函数 + 失败测试。** 落点判定独立成纯函数，零 egui 状态：

```rust
/// F324:`pos`(egui 点坐标)落在哪块**可作落点**的 pane 上。抽屉(`drawer`)、
/// 起点自己都不是落点。`views` 的 `geom.px` 是物理像素,除以 `ppp` 转点。
pub(crate) fn drop_target(views: &[TitleView<'_>], ppp: f32, pos: egui::Pos2, from: PaneId) -> Option<PaneId> { ... }
```

测试（构造 `TitleView` 的辅助沿用文件内现有测试里的构造方式）：
1. 落在另一块普通 pane → `Some(它)`；
2. 落在起点自己 → `None`；
3. 落在抽屉 → `None`；
4. 落在所有 pane 之外 → `None`。

- [ ] **Step 2: 拖动状态机（纯函数 + 测试）。**

```rust
/// 一次标题条拖动在松手时的结局。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DragEnd { Swap(PaneId, PaneId), Cancelled }

pub(crate) fn drag_end(from: PaneId, target: Option<PaneId>, esc_pressed: bool) -> DragEnd {
    match (esc_pressed, target) {
        (false, Some(to)) if to != from => DragEnd::Swap(from, to),
        _ => DragEnd::Cancelled,
    }
}
```

测试：Esc 优先取消；无落点取消；落点=自己取消；正常 `Swap`。

- [ ] **Step 3: 接入 `show`。** 在 `show` 的 Area 闭包里、构建 `content` **之前**：

```rust
// F324:标题条整条登记一个 drag 感知。**必须早于三个按钮/国家格的 interact**
// (egui 里后登记的在上层先吃指针),这样点按钮仍是点击,只有按在空白处才能拖。
// 抽屉不可拖:不登记 drag。
let drag = (!v.drawer).then(|| ui.interact(full, drag_id(v.geom.id), egui::Sense::drag()));
```

`drag_id(pane)` 与现有 `close_id` 同风格（`egui::Id::new(("mullion_title_drag", pane.0))`）。拖动期间（`drag.dragged()`）：
- `ctx.set_cursor_icon(egui::CursorIcon::Grabbing)`；
- 用 `ctx.pointer_interact_pos()` + `drop_target` 算当前目标，目标 pane 区域在 `Order::Foreground` 的 `layer_painter` 上画 `accent.gamma_multiply(0.18)` 的半透明矩形 + 1px accent 描边；
- `ctx.input(|i| i.key_pressed(Escape))` → 把「已取消」记入 `ctx.data_mut(|d| d.insert_temp(cancel_key(pane), true))`，之后该次拖动松手一律 `Cancelled`；
- `drag.drag_stopped()` 时：`let esc = ctx.data_mut(|d| d.remove_temp::<bool>(cancel_key(pane))).unwrap_or(false)`，`match drag_end(...) { Swap(f,t) => action.swap = Some((f,t)), Cancelled => {} }`。

拖动期间 `ctx.request_repaint()` 仅在 `drag.dragged()` 时调用（T3：不拖时零额外重绘）。

- [ ] **Step 4: 贯通 `UiActions`。** `ui/mod.rs`：`pub swap_panes: Option<(PaneId, PaneId)>` + `actions.swap_panes = title_action.swap;`。`app.rs`：`has_real_action` 加一行；`apply_layout_actions` 开头早退条件加 `&& actions.swap_panes.is_none()`，在 `close_pane` 之后加：

```rust
if let Some((from, to)) = actions.swap_panes {
    // 抽屉/未知 id/自己换自己会被 `swap_panes` 拒绝 —— 那一帧是纯 noop,不标脏(T3)。
    if ws.swap_panes(from, to) {
        changed = true;
    }
}
```

- [ ] **Step 5: 接线守护测试**（仿 `refresh_node_pane` 那条）：
  (a) `has_real_action` 函数体含 `a.swap_panes`；(b) `apply_layout_actions` 体含 `ws.swap_panes(`。写法与 L22799 同模式，并写「自证会变红」说明。

- [ ] **Step 6: T5/T8 核对。** 读 `input_route::pointer_route` 与 `app.rs:13789` 一带，确认拖动中 `egui_wants_pointer` 为真（egui `is_using_pointer`）从而 Route::Egui；若发现松手那一下在标题条拖到终端区上方时会漏给终端，**停下来报告**，不要自行加绕过。结论写进提交正文。

- [ ] **Step 7:** `cargo test -p mullion-app` 全过；`cargo clippy --workspace --all-targets -- -D warnings`。

- [ ] **Step 8: Commit** `feat(app): 拖标题条把 pane 换到另一块 pane 的位置 (F324)`；正文列守护测试名，并标注「手感/遮罩视觉/光标未验证，需人工确认」。

---

### Task 6: F325-a store 去重查找

**Files:**
- Modify: `crates/mullion-store/src/vault.rs`（`credential_secret` 附近，约 L1091）
- Test: 同文件 tests（沿用已有 `add_credential` 测试的 vault 构造）

- [ ] **Step 1: 写失败测试**

```rust
/// F325:同用户名+同类型+同密文 → 命中;任何一项不同 → 不命中。
#[test]
fn a_credential_with_the_same_user_kind_and_secret_is_found() { /* add 一条 password 凭据 alice/pw1;find(alice,Password,pw1)=Some(id);find(bob,..)/find(alice,..,pw2)/find(alice,PublicKey{..},..) 全 None */ }

/// 公钥:私钥正文和口令都要相同(口令有无也算)。
#[test]
fn key_credentials_must_match_on_key_body_and_passphrase() { }

/// proxy_password 不参与(凭据本不含代理口令,设计 D4):草稿里带了也不影响命中。
#[test]
fn proxy_password_is_ignored_when_matching() { }
```

- [ ] **Step 2: 实现**

```rust
/// F325:找一条与 (user, kind, secret) 全等的已有凭据。比较 `password`/
/// `passphrase`/`private_key` 三项,**不比 `proxy_password`**(它是会话的
/// 东西,不是身份的东西,设计 D4;`credential_secret` 的文档同一条)。
/// 多条都命中取 id 最小的,结果稳定。
pub fn find_credential_matching(
    &self,
    user: &str,
    kind: &AuthKind,
    secret: &SecretEntry,
) -> Option<CredentialId> {
    self.credentials
        .iter()
        .filter(|c| c.user == user && &c.kind == kind)
        .filter(|c| {
            let s = self.credential_secret(c.id).cloned().unwrap_or_default();
            s.password == secret.password
                && s.passphrase == secret.passphrase
                && s.private_key == secret.private_key
        })
        .map(|c| c.id)
        .min_by_key(|id| id.0)
}
```

- [ ] **Step 3:** `cargo test -p mullion-store` PASS；clippy 无输出。

- [ ] **Step 4: Commit** `feat(store): 按用户+类型+密文查找已有凭据,供去重 (F325)`

---

### Task 7: F325-b 编辑器按钮 + app 施加

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/buffer.rs`（合并纯函数 + 测试）
- Modify: `crates/mullion-app/src/ui/session_manager/fields.rs`（`auth`，Own 档的「凭据」节）
- Modify: `crates/mullion-app/src/ui/mod.rs`（`UiState.credential_from_session_request`）
- Modify: `crates/mullion-app/src/app.rs`（凭据请求施加点，紧邻 L15677 `credential_save_request`）

**事实：** 编辑已有会话时，store 不回吐明文，缓冲里 `password`/`key_data`/`passphrase` 恒空，`*_touched` 标记碰过。所以「当前内容」= 库里已存密文（`vault.secret(session_id)`）上叠加缓冲中 `touched` 的字段。新建会话（无 id）则只有缓冲。

- [ ] **Step 1: 合并纯函数 + 失败测试（buffer.rs）**

```rust
/// F325:把「库里这条会话已存的密文」与「表单里碰过的字段」合成该会话**此刻**的
/// 身份密文。`stored` = 新建会话时为 `None`。
/// 未碰的字段保留库里的值;碰过且非空 = 覆盖;碰过且为空 = 清除(与 `SecretField` 语义一致)。
/// 代理口令不属于身份,恒不带(设计 D4)。
pub(crate) fn merged_identity_secret(buf: &EditorBuffer, stored: Option<&SecretEntry>) -> SecretEntry
```

测试：
1. 新建会话、填了密码 → 带密码；
2. 已存密码、未碰 → 沿用库里的；
3. 已存密码、碰过且改成新值 → 新值；
4. 碰过且清空 → `None`；
5. 公钥：`key_data` 碰过覆盖 `private_key`，口令同理；
6. `proxy_password` 永不出现。

- [ ] **Step 2: 草稿构造 + 命名**

```rust
/// F325:默认凭据名 `用户@主机`;已被占用时加 ` (2)`、` (3)`…。
/// 凭据名本不要求唯一(主键是 id),这里只是给用户一个好认的名字。
pub(crate) fn default_credential_name(user: &str, host: &str, taken: &[CredentialRecord]) -> String
```

测试：无冲突 → `root@10.0.0.5`；已有同名 → `root@10.0.0.5 (2)`；(2) 也占用 → `(3)`。

- [ ] **Step 3: UI 按钮（fields.rs `auth` 的 Own 档、认证方式 grid 之后）。** 按钮 `创建凭据`（`ui.button`，固定 id 方便测试：`egui::Id::new("sm_create_credential")`），旁边灰字提示 `把这份用户名与密码/私钥存为共享凭据并改为引用(相同的会复用)`。点击置 `buf.create_credential_clicked = true`（新瞬态字段，**不参与 `is_dirty`**——参照 `pick_key_clicked` 的处理方式，grep 它的所有出现点并照做，含手写 Debug/PartialEq 排除；漏一处会让表单假脏）。条件：`buf.cred_source == Own` 才显示（Shared 档本来就 `return` 在前，不会走到）；用户名为空时禁用并 `on_disabled_hover_text("先填用户名")`；公钥模式且既无已存钥匙又没导入时禁用（`presence.private_key || !buf.key_data.is_empty()`）。

- [ ] **Step 4: mod.rs 抽取点。** 仿 `std::mem::take(&mut ui_state.credential_save_click)`（`session_manager/mod.rs:786`）：`take(&mut buf.create_credential_clicked)` 为真时置 `ui_state.credential_from_session_request = Some(CredentialFromSession { editing_id, buffer: buf.clone() })`。（`buffer` 含明文，沿用 `SaveIntent` 同样的手写打码 Debug 约束——**不 derive Debug**。）

- [ ] **Step 5: app.rs 施加。**

```rust
if let Some(req) = self.ui.credential_from_session_request.take() {
    if let Some(store) = self.store.as_mut() {
        let stored = req.editing_id.and_then(|id| store.secret(id).cloned());
        let secret = merged_identity_secret(&req.buffer, stored.as_ref());
        let kind = match req.buffer.auth_kind { Password => AuthKind::Password, PublicKey => AuthKind::PublicKey { has_passphrase: secret.passphrase.is_some() } };
        let user = req.buffer.user.trim().to_string();
        let (id, reused) = match store.find_credential_matching(&user, &kind, &secret) {
            Some(id) => (id, true),
            None => {
                let name = default_credential_name(&user, req.buffer.host.trim(), store.credentials());
                (store.add_credential(CredentialDraft { name, user, kind, secret: Some(secret) }), false)
            }
        };
        // 把「正在编辑的缓冲」改为引用它 —— 缓冲在 UiState 里,不在 req 里。
        if let Some(buf) = self.ui.editor.as_mut() { buf.cred_source = Shared; buf.credential_id = Some(id); }
        // toast:复用 / 新建
    }
}
```

字段/方法名（`self.ui.editor` 等）以现有代码为准，先 grep；`add_credential` 之后须走与 `apply_credential_save` 相同的**落盘**路径（读 `apply_credential_save` 看它如何 `save`，照做；漏了落盘 = 重启后凭据丢失而会话引用悬空）。Toast：新建 → `已创建凭据「{name}」,并改为引用`；复用 → `已复用凭据「{name}」(内容相同,未新建)`。

- [ ] **Step 6: 测试**
  - app 层纯逻辑若难单测，把 Step 5 的核心抽成 `fn convert_inline_to_credential(store: &mut Vault, req: &CredentialFromSession) -> Result<(CredentialId, bool /*reused*/), String>`，写测试：新建 / 第二次相同内容命中复用（凭据条数不增）/ 未碰密码的已存会话取库里的密文 / 公钥带口令。
  - `fields.rs` 的 UI 测试：用户名空时按钮禁用；点击后 `create_credential_clicked` 为真（仿 `picking_a_credential_from_the_combo_updates_the_buffer` 的写法）。
  - `is_dirty` 不受 `create_credential_clicked` 影响的守护。

- [ ] **Step 7:** `cargo test -p mullion-app`，`cargo test -p mullion-app --test form_guidelines`，`--test glyph_whitelist` 全 PASS。

- [ ] **Step 8: Commit** `feat(app): 会话编辑器「创建凭据」,按内容去重复用 (F325)`

---

### Task 8: 全量验证 + 发版

- [ ] **Step 1: 绿的定义。** `cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log`；`cargo clippy --workspace --all-targets -- -D warnings`；`cargo fmt --check`。三者都过才叫绿。
- [ ] **Step 2: 复核。** 起 `reviewer` subagent 复核 Task 5（输入路由/重绘）与 Task 7（密文处理：明文是否落日志/Debug、落盘是否漏）。涉及密文，按全局约定做。
- [ ] **Step 3: 发版。** 调用 `release-windows` skill（升 patch → 0.1.129、交叉编译、objdump、签名、Release、报链接与人工验收清单）。
- [ ] **Step 4: 人工验收清单（写进 Release notes）：** 
  1. 归档项目右栏「取消归档」呈 accent 描边且在最左；未归档项目的「归档」按钮外观不变。
  2. 悬停国家格有底色+手形；点击仍刷新国家。
  3. 两屏以上，按住标题条空白处拖到另一块 pane：出现遮罩与抓取光标，松手两块互换；Esc、拖出窗口、拖回自己都不动；点 ×/换节点/项目按钮不触发拖动；抽屉标题条拖不动、也放不上去；带抽屉的 pane 连抽屉一起换；换位后 tmux 里的 TUI 按新尺寸重排（T4）；拖动时终端里没有出现划选。
  4. 会话编辑器 Own 档：点「创建凭据」生成凭据并切到共享；对另一条用户名+密码相同的会话再点，提示「已复用」，凭据页条数不增；未改密码的已存会话也能转换；保存后重启，引用仍有效。
