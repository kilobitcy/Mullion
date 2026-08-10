# 会话列表三档密度重排 + 选中态改节点色 + 状态点下线 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 会话管理器左栏三档密度统一用 32px 图标、连接状态点整链下线、选中/悬停背景改用会话的节点色、图标底色从独立可配字段改为跟随节点色。

**Architecture:** 全部改动落在 `mullion-app` 的绘制层，`mullion-store` 零改动（`IconSpec.bg` 字段保留但停用，不迁移 schema）。取色与尺寸逻辑一律抽成不收 `Ui`/`Painter` 的纯函数（`row_bg`、`icon_px`、`row_h`、`color_rgb`），这样「选中背景到底是什么色」「哪一档用多大图标」能被直接断言，而不是靠数图元反推。颜色闸门只有一份实现（`badge::color_rgb`），三个绘制落点各传各的 `ColorTarget`。

**Tech Stack:** Rust / egui 0.30 / epaint。测试全部是 `cargo test -p mullion-app` 下的 in-module 单测，靠 `egui::Context::run` 跑一帧再检查 `FullOutput.shapes`。

**Spec:** `docs/superpowers/specs/2026-08-10-session-list-density-and-color-design.md`

---

## 文件结构

| 文件 | 本次的职责 | 改动性质 |
|---|---|---|
| `crates/mullion-app/src/ui/session_manager/list.rs` | 左栏全部绘制：密度常量、行背景取色、行内布局 | 主战场。删状态点整套（约 60 行），加 `row_bg` 纯函数 |
| `crates/mullion-app/src/ui/badge.rs` | 外观数据 → 颜色/图标绘制原语 | 加 `color_rgb` 闸门；`paint_icon` 改签名收底色 |
| `crates/mullion-app/src/ui/session_manager/fields.rs` | 编辑器「外观」页 | 删「底色」UI 与 `DEFAULT_ICON_BG`；预览缩成一档 |
| `crates/mullion-app/src/ui/pane_title.rs` | pane 标题条 | 仅跟进 `paint_icon` 新签名 |
| `crates/mullion-app/src/ui/session_manager/mod.rs` | 左栏容器 | 删 `connected` 参数、改 `LIST_MIN_W` |
| `crates/mullion-app/src/ui/mod.rs` | `UiState` / `UiFrame` | 删 `connecting` / `connect_failed` / `connected_session` |
| `crates/mullion-app/src/app.rs` | 事件循环 | 删上述三个字段的全部写点 |
| `crates/mullion-app/src/theme.rs` | 视觉 token | **不改**（`row_bg` 的混色在 `list.rs` 里做） |

任务顺序是**编译约束**决定的：Task 2 删参数会连锁到 4 个文件，必须一次改完才编得过；Task 4 改 `paint_icon` 签名同理。

---

### Task 1: 三档尺寸与阈值重排

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/list.rs:15-71`（密度枚举、阈值、`row_h`、`icon_px`、`ICON_SLOT_X`、`text_x`）
- Modify: `crates/mullion-app/src/ui/session_manager/list.rs:372-375`（`Full` 档两行文字基线）
- Modify: `crates/mullion-app/src/ui/session_manager/list.rs:405`（副标题 y 坐标）
- Modify: `crates/mullion-app/src/ui/session_manager/mod.rs:54`（`LIST_MIN_W`）
- Test: 同文件 `mod tests`（`list.rs:2128-2148` 现有那条 64px 测试要替换）

- [ ] **Step 1: 替换现有的 64px 断言，写成新的失败测试**

把 `list.rs` 里现有的 `the_narrowest_width_actually_fits_a_64px_icon` 整个函数（含它上面那行 `/// 下限必须真的装得下最窄那一档的内容…` 注释）删掉，替换成下面两条：

```rust
    /// 三档图标统一 32px。行高必须真的装得下它,而且上下留白要够 ——
    /// 只断言 `row_h >= icon_px` 是不够的:`Full` 行高退回 44 时 44 > 32 仍然
    /// 过,而那正是要防的(32px 图标在 44 行高里上下只剩 6px,挤得发闷)。
    ///
    /// `Compact` 的留白阈值故意更松(4px):它就是「省地方」的那一档,
    /// 单行文字 + 40 行高是它存在的意义。
    ///
    /// 自证会变红:把 `icon_px` 任意一档改回 16 或 `ico::LARGE`(第一段炸);
    /// 把 `row_h(Full)` 改回 44(第三段炸)。
    #[test]
    fn every_step_uses_the_32px_frame_and_the_row_fits_it() {
        use super::super::LIST_MIN_W;
        // `Icons` 档存在的前提:阈值必须严格大于左栏能拖到的下限,否则
        // `density_for` 永远落不到它。编译期钉死 —— 这两个数分处两个文件,
        // 靠人记住迟早出事。(写法与 `mod.rs` 里那条宽度联立断言同源。)
        const { assert!(LIST_MIN_W < ICONS_BELOW) };
        for d in [Density::Full, Density::Compact, Density::Icons] {
            assert_eq!(
                icon_px(d),
                crate::ui::ico::SMALL as f32,
                "{d:?} 档该用 32px 那一帧"
            );
        }
        assert!(
            LIST_MIN_W >= icon_px(Density::Icons),
            "左栏下限 {LIST_MIN_W} 横着装不下 {}px 图标",
            icon_px(Density::Icons)
        );
        for (d, min_pad) in [
            (Density::Full, 8.0f32),
            (Density::Icons, 8.0),
            (Density::Compact, 4.0),
        ] {
            let pad = (row_h(d) - icon_px(d)) / 2.0;
            assert!(
                pad >= min_pad,
                "{d:?} 档行高 {} 减掉 {}px 图标后上下各只剩 {pad}px",
                row_h(d),
                icon_px(d)
            );
        }
    }

    /// 状态点下线后图标左移贴边。三档共用同一个槽位中心,文字左界才不会
    /// 随档位跳 —— 那是 `text_x` 这个函数存在的全部理由。
    ///
    /// 自证会变红:把 `ICON_SLOT_X` 改回 38。
    #[test]
    fn the_icon_hugs_the_left_edge_now_that_the_status_dot_is_gone() {
        assert_eq!(
            ICON_SLOT_X - icon_px(Density::Full) / 2.0,
            8.0,
            "图标左边距应当是 8px,与行高上下留白同数"
        );
        assert_eq!(text_x(Density::Full), 48.0);
        assert_eq!(
            text_x(Density::Compact),
            text_x(Density::Full),
            "两个有文字的档必须共用同一条文字左界"
        );
    }

    /// `Full` 档两行文字要在 48 行高里上下居中。行高从 44 涨到 48 时最容易
    /// 漏改这两个基线常量,漏了的现象是两行字整体贴在行的上半部分。
    ///
    /// 这是个**代理断言**:真正的文字包围盒要排版才知道,这里用「名称上沿到
    /// 行顶」对「副标题下沿到行底」的差值当近似,±1px 以内算居中。
    ///
    /// 自证会变红:把 `NAME_TOP` 改回 7.0。
    #[test]
    fn the_two_text_lines_sit_vertically_centred_in_the_full_row() {
        let top_gap = NAME_TOP;
        let bottom_gap = row_h(Density::Full) - (SUB_TOP + SUB_FONT_PX);
        assert!(
            (top_gap - bottom_gap).abs() <= 1.0,
            "上留白 {top_gap} 与下留白 {bottom_gap} 差太多"
        );
    }
```

- [ ] **Step 2: 运行，确认三条都失败**

```bash
cargo test -p mullion-app --lib session_manager::list::tests 2>&1 | tail -30
```

预期：编译失败，`cannot find value NAME_TOP / SUB_TOP / SUB_FONT_PX in this scope`（三个常量还没建）。这就是本步要的红。

- [ ] **Step 3: 改密度常量与尺寸函数**

`list.rs` 把第 22-34 行的枚举注释与两个阈值常量改成：

```rust
    /// 32px 图标 + 名称/副标题两行。默认档。
    Full,
    /// 32px 图标 + 名称单行。副标题(user@host)让位。
    Compact,
    /// 只有 32px 图标。**没设图标的行整条隐藏** —— 这一档认图标不认字,
    /// 留一行空白比不留更糟。
    Icons,
}

/// 切档阈值。`Compact` 的上限踩的是「名称 + 副标题两行还读得出东西」的下界;
/// `Icons` 的上限踩的是「32px 图标右边还剩得下几个字」的下界。
///
/// `ICONS_BELOW` 必须**严格大于** `LIST_MIN_W`,否则 `density_for` 永远落不到
/// `Icons`,那一档等于不存在(`narrowing_the_list_only_ever_simplifies_it` 钉着)。
const COMPACT_BELOW: f32 = 208.0;
const ICONS_BELOW: f32 = 88.0;
```

`row_h` 的 match 改成：

```rust
        Density::Full => 48.0,
        Density::Compact => 40.0,
        Density::Icons => 48.0,
```

`icon_px` 整个函数体（第 58-64 行）改成：

```rust
/// 图标边长。三档统一 32 —— 正是用户导入 .ico 时归一化出来的小那一帧
/// (`ui::ico::SMALL`)。64 那一帧从此没有绘制点在用,但归一化仍产出它
/// (那是存储格式的一部分,改它要迁移已有配置,收益为零)。
fn icon_px(_d: Density) -> f32 {
    crate::ui::ico::SMALL as f32
}
```

`ICON_SLOT_X` 与它的注释（第 66-67 行）改成：

```rust
/// 图标槽位中心距行左边缘(逻辑点)。= 左边距 8 + 半个图标。状态点已下线,
/// 图标直接贴左边缘,不再给点留位置。
const ICON_SLOT_X: f32 = 24.0;
```

在 `text_x` 之后（第 71 行下方）新增三个基线常量：

```rust
/// `Full` 档名称文字的上沿距行顶。
const NAME_TOP: f32 = 9.0;
/// `Full` 档副标题文字的上沿距行顶。
const SUB_TOP: f32 = 27.0;
/// 副标题字号。基线居中的断言要用它算下留白,所以不能只写在
/// `FontId::proportional(11.0)` 那一处 —— 两处写同一个数迟早分叉。
const SUB_FONT_PX: f32 = 11.0;
```

- [ ] **Step 4: 把两个基线魔法数换成常量**

`list.rs` 第 372-375 行的 `name_y`：

```rust
    let name_y = match d {
        Density::Compact => rect.center().y - 9.0,
        _ => rect.top() + NAME_TOP,
    };
```

第 403-408 行副标题那次 `paint_highlighted` 的前两个参数：

```rust
    paint_highlighted(
        p,
        egui::pos2(text_left, rect.top() + SUB_TOP),
        sub,
        query,
        egui::FontId::proportional(SUB_FONT_PX),
```

- [ ] **Step 5: 降左栏下限**

`crates/mullion-app/src/ui/session_manager/mod.rs:54` 附近，把 `LIST_MIN_W` 改成 56 并更新它的文档注释：

```rust
/// 左栏能拖到的最窄宽度。= 32px 图标 + 左右各 12px 呼吸。必须**严格小于**
/// `list::ICONS_BELOW`(88),否则纯图标档永远拖不出来。
pub(crate) const LIST_MIN_W: f32 = 56.0;
```

- [ ] **Step 6: 运行测试，确认全绿**

```bash
cargo test -p mullion-app --lib session_manager::list::tests 2>&1 | tail -20
```

预期：`test result: ok`。特别确认 `narrowing_the_list_only_ever_simplifies_it` 仍绿 —— 它断言 `density_for(LIST_MIN_W) == Icons`（56 < 88 ✓）和 `density_for(ICONS_BELOW) == Compact`（88 不小于 88 ✓）。

- [ ] **Step 7: 提交**

```bash
git add crates/mullion-app/src/ui/session_manager/list.rs crates/mullion-app/src/ui/session_manager/mod.rs
git commit -m "feat(ui): 左栏三档图标统一 32px、行高与阈值重排 (F61/F80)

图标从 16/32/64 三档统一成 32;Full 行高 44→48、Icons 72→48;
ICONS_BELOW 132→88、LIST_MIN_W 88→56(两数不错开纯图标档拖不出来);
图标左移贴边(槽位中心 38→24、文字左界 54→48),给下一步删状态点腾位。"
```

---

### Task 2: 连接状态指示点整链下线

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/list.rs`（删 `Status`/`Marker`/`status_of`/`marker_of`/`status_color`/`status_tooltip`/`paint_status` 及三条测试；删 `show`/`row`/`session_row`/`paint_row_body`/`preview_row` 的相关参数）
- Modify: `crates/mullion-app/src/ui/session_manager/mod.rs:285`（`show` 的 `connected` 参数）、`:467`（转发处）
- Modify: `crates/mullion-app/src/ui/mod.rs:82-91`（`UiState.connecting` / `connect_failed`）、`:281`（`UiFrame.connected_session`）、`:370`（转发处）
- Modify: `crates/mullion-app/src/app.rs:197`（字段）、`:266`、`:716-717`、`:1104-1106`、`:1261-1262`、`:1684`、`:1748`
- Test: `list.rs` 的 `mod tests`

- [ ] **Step 1: 写失败测试**

在 `list.rs` 的 `mod tests` 里，先加一个数圆形图元的 helper（放在现有 `drawn_text` 之后）：

```rust
    /// 数一帧里画了几个圆。状态点是左栏唯一的圆形图元 —— 图标走
    /// `Shape::Image`、背景/竖条/按钮走 `Shape::Rect`、文字走 `Shape::Text`、
    /// 分组头的三角走 `Shape::Path`。所以「圆的个数」就是「状态点的个数」。
    fn circle_count(shapes: &[egui::epaint::ClippedShape]) -> usize {
        fn walk(s: &egui::Shape) -> usize {
            match s {
                egui::Shape::Vec(v) => v.iter().map(walk).sum(),
                egui::Shape::Circle(_) => 1,
                _ => 0,
            }
        }
        shapes.iter().map(|cs| walk(&cs.shape)).sum()
    }
```

再加测试：

```rust
    /// 会话行不再画连接状态点(v0.1.28)。连带那块 12×12 的 hover 热区也没了 ——
    /// 点没了还留着浮层,等于在空白处埋一个看不见的提示。
    ///
    /// 代价是明确接受的:列表从此看不出哪台连上了,连接状态归 pane 标题条管。
    ///
    /// 自证会变红:把 `paint_status` 那次调用加回 `paint_row_body`。
    #[test]
    fn session_rows_no_longer_paint_a_connection_status_dot() {
        let sessions = vec![
            with_icon(rec(1, "dev-box", "192.0.2.10", &[])),
            rec(2, "prod-box", "192.0.2.11", &[]),
        ];
        let cache = cache_of(&sessions);
        let out = run_list_at(super::super::LIST_W, &sessions, &cache);
        assert_eq!(
            circle_count(&out.shapes),
            0,
            "左栏里不该再有圆形图元(状态点是这里唯一会画圆的东西)"
        );
    }
```

- [ ] **Step 2: 运行，确认失败**

```bash
cargo test -p mullion-app --lib session_rows_no_longer_paint -- --nocapture 2>&1 | tail -20
```

预期：FAIL，`assertion `left == right` failed`，left 是 `2`（两行各一个状态点）。

若 left 不是 2 而是更大的数，说明 egui 在左栏别处也画了圆，先跑一次 `git stash` 前的基线确认差值，再把断言改成「删除后比删除前少 2」——但优先按 0 处理，因为左栏设计上没有别的圆。

- [ ] **Step 3: 删掉状态点这一套（list.rs）**

删除 `list.rs` 第 73-161 行整段，即：

- `/// 会话行左侧状态点的四态(走查 4)…` 注释 + `pub(crate) enum Status`
- `/// 状态点画成什么**形状**…` 注释 + `pub(crate) enum Marker`
- `pub(crate) fn status_of`
- `pub(crate) fn marker_of`
- `pub(crate) fn status_tooltip`
- `fn status_color`
- `fn paint_status`

`session_row` 删掉 `status: Status,` 参数，并删掉第 231-249 行那段 dot 的 `ui.interact` 及其上方整段注释（`// §6.3:状态点加 tooltip…` 到 `.on_hover_text(status_tooltip(status));`）。

`paint_row_body` 删掉 `status: Status,` 参数，并删掉第 337-348 行那段 `// 走查 4:四态 + 色形双编码…` 注释与 `paint_status(...)` 调用。

`paint_row_body` 里调用 `session_row` 的实参、以及 `session_row` 调用 `paint_row_body` 的实参同步去掉 `status`。

`preview_row` 删掉 `Status::Idle,` 那个实参，以及它文档注释里的这两行：

```
/// 状态点固定画 `Idle` —— 预览的是外观,不是连接状态;画成绿色会让人以为
/// 这里能看出连没连上。
```

`row` 删掉 `connected: Option<SessionId>,` 参数，删掉第 774-779 行的 `let status = status_of(...)` 整块（含上方注释），并把 `session_row(ui, t, rec, &sub, selected, status, a, &ui_state.search, d)` 改成：

```rust
    let resp = session_row(ui, t, rec, &sub, selected, a, &ui_state.search, d);
```

`show` 删掉 `connected: Option<SessionId>,` 参数，并删掉第 704 行 `row(...)` 调用里的 `connected,` 实参。

删掉 `mod tests` 里这三条测试（连同各自的文档注释）：`status_of` 优先级那条、`marker_of` 四态互不相同那条、`status_tooltip_names_the_state`。把 `run_list_at` 里的 `show(ui, &t, &mut ui_state, sessions, &groups, None, appearance);` 改成：

```rust
                        show(ui, &t, &mut ui_state, sessions, &groups, appearance);
```

若 `SessionId` 在 `list.rs` 顶部的 `use` 变成未使用，一并删掉那个 import。

- [ ] **Step 4: 删掉 `connected` 传递链（mod.rs / ui/mod.rs / app.rs）**

`session_manager/mod.rs`：`show` 的签名删掉 `connected: Option<SessionId>,`；第 467 行转发改成

```rust
                    list::show(ui, t, ui_state, sessions, groups, appearance)
```

`ui/mod.rs`：删掉 `UiFrame` 的 `connected_session` 字段（含上方两行文档注释）；删掉 `UiState` 的 `connecting` 与 `connect_failed` 两个字段（含各自文档注释）；第 370 行 `frame.connected_session,` 这一行实参删掉；第 560 行 `connected_session: None,` 删掉。

`app.rs`：
- 第 195-197 行 `connected_session` 字段及其两行注释删掉
- 第 266 行 `connected_session: None,` 删掉
- 第 716-717 行 `self.ui.connecting = self.ui.connect_request_last;` 与 `self.ui.connect_failed = None;` 两行删掉，连同上方 `// 走查 4:状态点进「连接中」…` 那两行注释
- 第 1104-1106 行 `self.connected_session = self.ui.connect_request_last;` 与 `self.ui.connecting = None;` 删掉，连同 `// 走查 4:拨号结束,「连接中」态收工。` 与它上方 `// ConnectOk 不带 SessionId(见 UserEvent 定义),用发起连接时 / 记下的那条。` 两行注释
- 第 1258-1262 行 `self.ui.connect_failed = ...` / `self.ui.connecting = None;` 及上方 `// 走查 4:把失败落到发起连接的那条会话上…` 四行注释删掉
- 第 1684 行 `connected_session: self.connected_session,` 删掉
- 第 1748 行 `self.connected_session = None;` 及上方 `// 与 self.ws 成对维护…` 两行注释删掉

**不动** `connect_request_last`：`automation.rs:66` 和 `ConnectOk` 里的 `session_id:` 还在用它。

- [ ] **Step 5: 编译并跑全 crate 测试**

```bash
cargo test -p mullion-app 2>&1 | tail -30
```

预期：编译通过、`session_rows_no_longer_paint_a_connection_status_dot` 绿。若出现 `field is never read` / `unused import`，按提示删干净——那正是这一步要清的死代码。

- [ ] **Step 6: clippy 关口**

```bash
cargo clippy -p mullion-app --all-targets -- -D warnings 2>&1 | tail -20
```

预期：无输出。`session_row` / `paint_row_body` 少了一个参数后，`#[allow(clippy::too_many_arguments)]` 可能变成多余的 `allow`——clippy 不报这个，保留即可。

- [ ] **Step 7: 提交**

```bash
git add crates/mullion-app/src
git commit -m "feat(ui): 会话行连接状态指示点整链下线 (F80)

删 Status/Marker/status_of/marker_of/status_color/status_tooltip/paint_status
及 12×12 的 tooltip 热区;连带删掉唯一以它为终点的三条死链:
UiState.connecting、UiState.connect_failed、app.connected_session→UiFrame→show。
connect_request_last 保留(automation 还在用)。

代价明确接受:列表从此看不出哪台连上了,连接状态归 pane 标题条。
守护测试 session_rows_no_longer_paint_a_connection_status_dot(数圆形图元)。"
```

---

### Task 3: 行背景改用节点色，去掉左侧强调条

**Files:**
- Modify: `crates/mullion-app/src/ui/badge.rs:36-42`（`should_paint` 拆出 `color_rgb`）
- Modify: `crates/mullion-app/src/ui/session_manager/list.rs`（新增 `row_bg` + `blend`；改 `session_row` 的背景绘制）
- Test: `list.rs` 的 `mod tests`、`badge.rs` 的 `mod tests`

- [ ] **Step 1: 写失败测试（badge 闸门拆分）**

在 `badge.rs` 的 `mod tests` 末尾加：

```rust
    /// 闸门只有一份实现:`should_paint` 是 `color_rgb` 的 `Color32` 包装。
    /// 拆两层是因为对比度实算(`theme::contrast_ratio`)只吃 `Rgb`,而会话行
    /// 背景要跟 `fg` 算对比度;两处各写一遍 `apply_to` 过滤迟早分叉。
    #[test]
    fn should_paint_is_just_the_color32_wrapper_over_color_rgb() {
        let a = colored("#e06767", &[ColorTarget::ListItem]);
        assert_eq!(
            color_rgb(a.color.as_ref(), ColorTarget::ListItem),
            theme::parse_hex("#e06767")
        );
        assert_eq!(
            color_rgb(a.color.as_ref(), ColorTarget::PaneTitle),
            None,
            "没勾的落点一律 None"
        );
        assert_eq!(
            should_paint(&a, ColorTarget::ListItem),
            color_rgb(a.color.as_ref(), ColorTarget::ListItem).map(theme::c32)
        );
        assert_eq!(color_rgb(None, ColorTarget::ListItem), None, "没设色 → None");
    }
```

- [ ] **Step 2: 写失败测试（`row_bg` 取色）**

在 `list.rs` 的 `mod tests` 里加：

```rust
    /// 行背景的四条规矩,一次钉死:
    /// 1. 普通态透明(`None`)
    /// 2. 有节点色时,选中/悬停都由它主导
    /// 3. 选中比悬停**更靠近**节点色(否则两态分不出来)
    /// 4. 没节点色时回落到改动前的 `sunken_bg` / `panel_head`
    ///
    /// 抽成纯函数才测得了:混在 `session_row` 里的话,「选中背景到底是什么色」
    /// 只能靠数图元反推,测了也不知道测的是不是那一块。
    ///
    /// 自证会变红:让 `row_bg` 忽略 `node` 直接返回 `sunken_bg`(第 2、3 段炸)。
    #[test]
    fn row_background_is_driven_by_the_node_colour_with_a_theme_fallback() {
        let t = &crate::theme::MULLION_DARK;
        let node = crate::theme::parse_hex("#e06767").unwrap();

        assert_eq!(row_bg(false, false, None, t), None, "普通态必须透明");
        assert_eq!(
            row_bg(false, false, Some(node), t),
            None,
            "没选中也没悬停时,配了颜色也不铺背景"
        );

        let sel = row_bg(true, false, Some(node), t).expect("选中态要有背景");
        let hov = row_bg(false, true, Some(node), t).expect("悬停态要有背景");
        assert_ne!(sel, crate::theme::c32(t.sunken_bg), "配了色就不该还是灰底");

        // 选中比悬停更靠近节点色:用「与 panel_bg 的距离」当单调性代理。
        let dist = |c: egui::Color32| {
            let b = crate::theme::c32(t.panel_bg);
            (c.r() as i32 - b.r() as i32).abs()
                + (c.g() as i32 - b.g() as i32).abs()
                + (c.b() as i32 - b.b() as i32).abs()
        };
        assert!(
            dist(sel) > dist(hov),
            "选中({sel:?})必须比悬停({hov:?})更浓"
        );

        assert_eq!(
            row_bg(true, false, None, t),
            Some(crate::theme::c32(t.sunken_bg)),
            "没配色的选中行保持改动前的样子"
        );
        assert_eq!(
            row_bg(false, true, None, t),
            Some(crate::theme::c32(t.panel_head)),
            "没配色的悬停行保持改动前的样子"
        );
    }

    /// 8 个预设色板铺成选中背景之后,`fg` 白字仍要读得出来。这正是选低透明度
    /// 混色而不是纯色铺满的理由 —— 纯色铺满时「黄」上的白字会掉到 1.5:1。
    ///
    /// 阈值取 WCAG AA 正文 4.5:1。
    ///
    /// 自证会变红:把 `SELECTED_ALPHA` 提到 0.9。
    #[test]
    fn every_preset_colour_keeps_the_row_text_readable_when_selected() {
        let t = &crate::theme::MULLION_DARK;
        for (name, hex, _) in crate::theme::LABEL_PALETTE {
            let node = crate::theme::parse_hex(hex).unwrap();
            let bg = row_bg(true, false, Some(node), t).unwrap();
            let bg_rgb = mullion_term::snapshot::Rgb::new(bg.r(), bg.g(), bg.b());
            let ratio = crate::theme::contrast_ratio(t.fg, bg_rgb);
            assert!(
                ratio >= 4.5,
                "预设色「{name}」({hex})铺成选中底后,fg 对比度只有 {ratio:.2}:1"
            );
        }
    }
```

- [ ] **Step 3: 写失败测试（强调条真的没了）**

`row_bg` 只管取色，管不到「那条 3px 竖条还画不画」。补一条数图元的：先把 `run_list_at` 抽成带选中参数的版本（原函数保留成薄封装，现有测试不动），再加计数 helper 与断言。

`list.rs` 的 `mod tests` 里，把 `run_list_at` 改成：

```rust
    fn run_list_at(
        width: f32,
        sessions: &[SessionRecord],
        appearance: &crate::ui::badge::AppearanceCache,
    ) -> egui::FullOutput {
        run_list_selecting(width, sessions, appearance, None)
    }

    /// 同上,但可以指定哪一条处于选中态(`UiState::editor_id`)。
    fn run_list_selecting(
        width: f32,
        sessions: &[SessionRecord],
        appearance: &crate::ui::badge::AppearanceCache,
        selected: Option<SessionId>,
    ) -> egui::FullOutput {
        let t = crate::theme::MULLION_DARK;
        let groups: Vec<GroupRecord> = Vec::new();
        let mut ui_state = UiState::default();
        ui_state.editor_id = selected;
        let ctx = egui::Context::default();
        ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 600.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::none())
                    .show(ctx, |ui| {
                        show(ui, &t, &mut ui_state, sessions, &groups, appearance);
                    });
            },
        )
    }

    /// 数一帧里画了几个矩形。
    fn rect_count(shapes: &[egui::epaint::ClippedShape]) -> usize {
        fn walk(s: &egui::Shape) -> usize {
            match s {
                egui::Shape::Vec(v) => v.iter().map(walk).sum(),
                egui::Shape::Rect(_) => 1,
                _ => 0,
            }
        }
        shapes.iter().map(|cs| walk(&cs.shape)).sum()
    }
```

再加断言：

```rust
    /// 选中态只多画**一块**背景 —— 那条左侧 3px 强调条已经删了。
    ///
    /// 数矩形而不是比颜色:强调条和背景都是 `Shape::Rect`,差值为 1 说明只多了
    /// 背景那一块,为 2 说明强调条还在。`row_bg` 的单测管不到这件事(它只回答
    /// 「背景是什么色」,回答不了「除了背景还画了什么」)。
    ///
    /// 自证会变红:把 `session_row` 里那段 accent `rect_filled` 加回去。
    #[test]
    fn selecting_a_row_adds_only_a_background_no_accent_bar() {
        let sessions = vec![with_icon(rec(1, "dev-box", "192.0.2.10", &[]))];
        let cache = cache_of(&sessions);
        let id = sessions[0].id;
        let plain =
            rect_count(&run_list_selecting(super::super::LIST_W, &sessions, &cache, None).shapes);
        let sel = rect_count(
            &run_list_selecting(super::super::LIST_W, &sessions, &cache, Some(id)).shapes,
        );
        assert_eq!(
            sel - plain,
            1,
            "选中只该多画背景一块,实际多了 {}",
            sel - plain
        );
    }
```

- [ ] **Step 4: 运行，确认失败**

```bash
cargo test -p mullion-app --lib 2>&1 | tail -20
```

预期：编译失败，`cannot find function color_rgb` / `cannot find function row_bg`。修好编译后 `selecting_a_row_adds_only_a_background_no_accent_bar` 应当以 `sel - plain == 2` 失败——那是强调条还在的证据。

- [ ] **Step 5: badge.rs 拆出 `color_rgb`**

把 `badge.rs` 第 32-42 行（`should_paint` 及其文档注释）替换成：

```rust
/// 这个落点该用什么颜色画,**未转成 `Color32`**。
///
/// 收 `Option<&ColorSpec>` 而不是 `&Appearance`:编辑器手上是一份还没保存的
/// `AppearancePrefs`,构造不出 `Appearance`。收最小的那个东西,三处调用方
/// (列表 / pane 标题条 / 编辑器预览)才能共用同一份 `apply_to` 过滤。
///
/// 返回 `Rgb` 而不是 `Color32`:对比度实算(`theme::contrast_ratio`)只吃 `Rgb`,
/// 而会话行背景要跟 `fg` 算对比度。
pub fn color_rgb(
    color: Option<&ColorSpec>,
    target: ColorTarget,
) -> Option<mullion_term::snapshot::Rgb> {
    let c = color?;
    if !c.apply_to.contains(&target) {
        return None;
    }
    theme::parse_hex(&c.hex)
}

/// 这个落点该用什么颜色画。`None` = 不画。`color_rgb` 的 `Color32` 包装。
///
/// 三处落点共用,所以「`apply_to` 过滤 + hex 解析失败降级」这两件事
/// 只有一份实现、只能错一次。
pub fn should_paint(a: &Appearance, target: ColorTarget) -> Option<egui::Color32> {
    color_rgb(a.color.as_ref(), target).map(theme::c32)
}
```

- [ ] **Step 6: list.rs 加 `row_bg` 与 `blend`**

在 `list.rs` 的 `text_x` / 基线常量之后（Task 1 加的三个常量下方）插入：

```rust
/// 选中态节点色的混合比例。低透明度而不是纯色铺满:8 个预设里有浅色(黄),
/// 纯色铺满时 `fg` 白字会掉到不可读;混合后底色始终由 `panel_bg` 主导,
/// 文字对比度不随用户选的颜色漂移(`every_preset_colour_keeps_the_row_text_readable_when_selected` 钉着)。
const SELECTED_ALPHA: f32 = 0.28;
/// 悬停态的混合比例。必须小于 `SELECTED_ALPHA`,否则两态分不出来。
const HOVER_ALPHA: f32 = 0.14;

/// 把 `top` 按 `a` 的比例混到 `base` 上,得到一个**不透明**的结果色。
///
/// 不用 `Color32::from_rgba_unmultiplied` 交给 GPU 混:那样算出来的最终像素
/// 依赖底下画了什么,测不了。这里显式跟 `panel_bg` 混,结果是确定的一个色值。
fn blend(base: egui::Color32, top: egui::Color32, a: f32) -> egui::Color32 {
    let mix = |b: u8, t: u8| {
        (b as f32 + (t as f32 - b as f32) * a)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    egui::Color32::from_rgb(
        mix(base.r(), top.r()),
        mix(base.g(), top.g()),
        mix(base.b(), top.b()),
    )
}

/// 一行会话的背景色。`None` = 不铺(普通态)。
///
/// 抽成纯函数(不收 `Ui`/`Painter`)是这一整块能被测的前提:混在 `session_row`
/// 里的话,「选中背景到底是什么色」只能靠数图元反推。
///
/// `node` 已经过了 `apply_to` 闸门(调用方传 `badge::color_rgb(..., ListItem)`)——
/// 用户明确取消勾选「会话列表」之后,颜色不该还从背景里冒出来。
pub(crate) fn row_bg(
    selected: bool,
    hovered: bool,
    node: Option<mullion_term::snapshot::Rgb>,
    t: &Theme,
) -> Option<egui::Color32> {
    let alpha = if selected {
        SELECTED_ALPHA
    } else if hovered {
        HOVER_ALPHA
    } else {
        return None;
    };
    Some(match node {
        Some(c) => blend(theme::c32(t.panel_bg), theme::c32(c), alpha),
        None if selected => theme::c32(t.sunken_bg),
        None => theme::c32(t.panel_head),
    })
}
```

- [ ] **Step 7: 改 `session_row` 的背景绘制，删掉左侧强调条**

把 `session_row` 里第 203-217 行（`let bg = if selected {...}` 到 `if selected { p.rect_filled(...accent...) }` 整块）替换成：

```rust
    // F62:选中/悬停背景由会话自己的节点色主导(过 `ListItem` 闸门)。原来的
    // 「左 3px accent 竖条」已删 —— 整行都是节点色之后,再压一条固定色的竖条
    // 是两套颜色语言在同一行里打架。
    let node = crate::ui::badge::color_rgb(
        appearance.color.as_ref(),
        mullion_store::ColorTarget::ListItem,
    );
    if let Some(bg) = row_bg(selected, resp.hovered(), node, t) {
        p.rect_filled(rect, egui::Rounding::same(6.0), bg);
    }
```

同时更新 `session_row` 的文档注释：把第 180 行「两行 + 选中态左侧强调条」改成「两行 + 选中态节点色背景」，把第 183 行「左 3px 已被选中态 accent 占了，两者各占一边才不打架」改成「未选中行认色全靠它 —— 选中行有背景色，未选中行只有这条竖条」。

- [ ] **Step 8: 运行测试**

```bash
cargo test -p mullion-app 2>&1 | tail -30
```

预期：全绿。若 `every_preset_colour_keeps_the_row_text_readable_when_selected` 报某个色不达 4.5:1，把 `SELECTED_ALPHA` 下调 0.02 重跑，直到全部通过——不要改测试阈值。

- [ ] **Step 9: clippy + 提交**

```bash
cargo clippy -p mullion-app --all-targets -- -D warnings 2>&1 | tail -20
git add crates/mullion-app/src
git commit -m "feat(ui): 会话行选中/悬停改用节点色背景,删左侧强调条 (F62/F80)

badge 拆出 color_rgb(收 Option<&ColorSpec>、返回 Rgb),should_paint 变成它的
Color32 包装 —— apply_to 闸门仍只有一份实现。
list 新增纯函数 row_bg:选中 28%、悬停 14% 与 panel_bg 显式混色,未设色回落
sunken_bg/panel_head。8 个预设色铺底后 fg 对比度 ≥ 4.5:1 有测试钉着。"
```

---

### Task 4: `paint_icon` 底色改由调用方传入

**Files:**
- Modify: `crates/mullion-app/src/ui/badge.rs:196-226`（`paint_icon`）
- Modify: `crates/mullion-app/src/ui/session_manager/list.rs:359-365`（列表行调用）
- Modify: `crates/mullion-app/src/ui/pane_title.rs:128-132`（标题条调用）
- Modify: `crates/mullion-app/src/ui/session_manager/fields.rs:465-478`（预览调用，Task 5 会再动一次）
- Modify: `crates/mullion-store/src/model.rs:154-163`（`IconSpec.bg` 停用注释）
- Test: `badge.rs` 的 `mod tests`

- [ ] **Step 1: 改写 badge 的底色测试**

把 `badge.rs` 里的 `shapes_with` helper 换成收底色的版本：

```rust
    /// 跑一帧,返回「画了 `icon`(底色 `bg`)时的图形数」。传 `None` 得到不画
    /// 任何图标的基线 —— `CentralPanel` 自己也会画背景,不能拿绝对数字当断言。
    fn shapes_with_bg(icon: Option<&IconSpec>, bg: Option<egui::Color32>) -> usize {
        let ctx = egui::Context::default();
        let out = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                if let Some(i) = icon {
                    let rect =
                        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(16.0, 16.0));
                    paint_icon(ui.painter(), rect, i, bg);
                }
            });
        });
        count_shapes(&out.shapes)
    }

    /// 不带底色的简写。现有几条测试只关心「画没画图标」。
    fn shapes_with(icon: Option<&IconSpec>) -> usize {
        shapes_with_bg(icon, None)
    }
```

把现有的 `a_background_colour_is_painted_underneath_the_icon` 整个函数（含文档注释）替换成：

```rust
    /// 底色现在由**调用方**传:图标画在哪个落点,就用哪个落点的 `apply_to`
    /// 判定,而 `paint_icon` 自己拿不到 `target`(同一张图在列表和标题条上
    /// 该不该有底色,答案可能不同)。
    ///
    /// 第二段钉死 `IconSpec.bg` 真的停用了:schema 里还留着这个字段(旧配置
    /// 里可能有值,读到不该崩),但**不该再影响绘制**。
    ///
    /// 自证会变红:让 `paint_icon` 回头去读 `icon.bg`(第二段炸);
    /// 把那句 `rect_filled` 删掉(第一段炸)。
    #[test]
    fn the_backdrop_comes_from_the_caller_not_from_the_stored_bg_field() {
        let b64 = real_ico([9, 9, 9, 255]);
        let plain = shapes_with_bg(Some(&icon(IconKind::Ico, &b64)), None);
        let with_bg = shapes_with_bg(
            Some(&icon(IconKind::Ico, &b64)),
            Some(egui::Color32::from_rgb(0x1e, 0x88, 0xe5)),
        );
        assert!(
            with_bg > plain,
            "传了底色应当多画一层(没传 {plain},传了 {with_bg})"
        );

        let legacy = shapes_with_bg(
            Some(&IconSpec {
                kind: IconKind::Ico,
                value: b64,
                bg: Some("#1e88e5".into()),
            }),
            None,
        );
        assert_eq!(legacy, plain, "已停用的 IconSpec.bg 不该再影响绘制");
    }
```

- [ ] **Step 2: 运行，确认失败**

```bash
cargo test -p mullion-app --lib badge:: 2>&1 | tail -20
```

预期：编译失败，`this function takes 3 arguments but 4 arguments were supplied`。

- [ ] **Step 3: 改 `paint_icon` 签名与实现**

`badge.rs` 把 `paint_icon` 的签名与前 4 行改成：

```rust
pub fn paint_icon(
    p: &egui::Painter,
    rect: egui::Rect,
    icon: &IconSpec,
    bg: Option<egui::Color32>,
) {
    if icon.kind != IconKind::Ico {
        return;
    }
    if let Some(bg) = bg {
        p.rect_filled(rect, egui::Rounding::same(ICON_BG_ROUNDING), bg);
    }
```

并把它文档注释里的规则 2 改成：

```
/// 2. **底色垫在图标下面,由调用方给**。用户导入的 .ico 多半是给浅色资源
///    管理器画的,深色图标糊在深色面板上等于没有;垫一块底色是不改图本身就能
///    救回来的唯一办法。底色 = 会话的节点色(过该落点的 `apply_to` 闸门),
///    传 `None` 就直接画在面板上。`IconSpec.bg` 那个独立可配字段已停用。
```

- [ ] **Step 4: 跟进三个调用点**

`list.rs` 第 359-365 行改成（`node` 是 Step 3 Task 3 已经在 `session_row` 算好的，但 `paint_row_body` 是独立函数，这里自己算）：

```rust
    if let Some(icon) = &appearance.icon {
        crate::ui::badge::paint_icon(
            p,
            egui::Rect::from_center_size(icon_center, egui::vec2(px, px)),
            icon,
            crate::ui::badge::should_paint(appearance, mullion_store::ColorTarget::ListItem),
        );
    }
```

`pane_title.rs` 第 128-132 行改成：

```rust
                        if let Some(icon) = v.appearance.and_then(|a| a.icon.as_ref()) {
                            let (r, _) = ui
                                .allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                            crate::ui::badge::paint_icon(
                                ui.painter(),
                                r,
                                icon,
                                v.appearance.and_then(|a| {
                                    crate::ui::badge::should_paint(
                                        a,
                                        mullion_store::ColorTarget::PaneTitle,
                                    )
                                }),
                            );
                        }
```

`fields.rs` 第 471 行（`icon_preview` 内）暂时传 `None` 保编译，Task 5 会补上真实取色：

```rust
            crate::ui::badge::paint_icon(ui.painter(), rect, icon, None);
```

- [ ] **Step 5: 给 store 的 `bg` 字段补停用注释**

`crates/mullion-store/src/model.rs` 把 `IconSpec.bg` 的文档注释（第 154-162 行）改成：

```rust
    /// **已停用(v0.1.28)。** 图标底色改为跟随会话的节点色(`ColorSpec`),
    /// 不再单独配置;绘制侧 `badge::paint_icon` 不再读这个字段。
    ///
    /// 字段保留而非删除:v6 的文件里可能存着值,读到不该崩、也不该丢用户数据。
    /// 不做迁移 —— 迁移要动 `SCHEMA_VERSION`,而这里没有任何东西需要转换。
    ///
    /// 带 `default` + `skip_serializing_if`:没垫底色的图标不该往 TOML 里
    /// 写一行 `bg = ""`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
```

- [ ] **Step 6: 跑测试 + clippy**

```bash
cargo test --workspace 2>&1 | tail -20
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20
```

预期：全绿、clippy 无输出。

- [ ] **Step 7: 提交**

```bash
git add crates/
git commit -m "feat(ui): 图标底色改由调用方传入,跟随节点色 (F61/F62)

paint_icon 加 bg 参数、不再读 IconSpec.bg:同一张图在列表和 pane 标题条上
该不该垫底,取决于各自落点的 apply_to,而 paint_icon 拿不到 target。
列表传 should_paint(ListItem)、标题条传 should_paint(PaneTitle)。
IconSpec.bg 保留在 schema 里但停用(旧配置不崩、不丢、不迁移)。"
```

---

### Task 5: 编辑器删「底色」UI，预览只剩 32px

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/fields.rs:449-452`（删 `DEFAULT_ICON_BG`）
- Modify: `crates/mullion-app/src/ui/session_manager/fields.rs:460-478`（`icon_preview`）
- Modify: `crates/mullion-app/src/ui/session_manager/fields.rs:538-568`（删「底色」那一行）
- Modify: `crates/mullion-app/src/ui/session_manager/fields.rs:4238-4242`（测试里的 `bg` 赋值）
- Test: `fields.rs` 的 `mod tests`（替换 `the_background_colour_stays_unset_until_you_ask_for_it`）

- [ ] **Step 1: 写失败测试**

把 `fields.rs` 里 `the_background_colour_stays_unset_until_you_ask_for_it` 整个函数（含它上方的三行文档注释）替换成：

```rust
    /// 「底色」那一行已下线(v0.1.28):图标底色跟节点色走,不再单独配。
    ///
    /// 第二段钉住「不再往库里写」:UI 没了但代码若还在某处塞 `DEFAULT_ICON_BG`,
    /// 用户看不见却被写进了配置文件,那是最难查的一类脏数据。
    ///
    /// 自证会变红:把那段底色 `ui.horizontal` 加回 `appearance()`。
    #[test]
    fn the_appearance_page_no_longer_offers_a_separate_icon_background() {
        let mut buf = ico_buf();
        let out = run_appearance(&mut buf);
        assert!(
            find_text_pos(&out.shapes, "底色").is_none(),
            "「底色」那一行该没了"
        );
        assert_eq!(
            buf.preserved_appearance
                .icon
                .as_ref()
                .and_then(|i| i.bg.as_ref()),
            None,
            "UI 下线后不该还有代码往库里写底色"
        );
    }

    /// 预览只剩 32px 一档 —— 列表三档现在都按 32 取图,继续预览 64 是在骗人。
    /// 「小尺寸下还认不认得出」正是选图标时唯一要判断的事,预览尺寸必须与列表
    /// 真实取图的尺寸一致。
    ///
    /// 自证会变红:把 `icon_preview` 的循环改回 `[SMALL, LARGE]`。
    #[test]
    fn the_icon_preview_shows_only_the_size_the_list_actually_uses() {
        let mut buf = ico_buf();
        let out = run_appearance(&mut buf);
        assert!(
            find_text_pos(&out.shapes, "32px").is_some(),
            "该有 32px 那一档"
        );
        assert!(
            find_text_pos(&out.shapes, "64px").is_none(),
            "64px 那一档该没了"
        );
    }
```

- [ ] **Step 2: 运行，确认失败**

```bash
cargo test -p mullion-app --lib fields::tests::the_appearance_page_no_longer fields::tests::the_icon_preview_shows 2>&1 | tail -20
```

预期：两条都 FAIL —— 第一条因为「底色」文本还在，第二条因为「64px」还在。

- [ ] **Step 3: 删「底色」那一行 UI**

`fields.rs` 把 `if has_ico { ... }` 这一整块（第 538-568 行）替换成：

```rust
            if has_ico {
                // 预览:按列表真实取图的尺寸画一次。不当场给看,用户得先存、
                // 再去拖列表才知道自己选的图标在小尺寸下还认不认得出。
                //
                // 底色跟节点色走(过 `ListItem` 闸门),与列表行同源 —— 只勾了
                // 「pane 标题条」时预览也不垫底,预览的是列表里的真实效果,
                // 不是理想效果。
                icon_preview(
                    ui,
                    t,
                    buf.preserved_appearance.icon.as_ref(),
                    crate::ui::badge::color_rgb(
                        buf.preserved_appearance.color.as_ref(),
                        ColorTarget::ListItem,
                    )
                    .map(crate::theme::c32),
                );
            }
```

删掉 `DEFAULT_ICON_BG` 常量及其上方三行文档注释（第 449-452 行）。

- [ ] **Step 4: 改 `icon_preview`**

`fields.rs` 把 `icon_preview` 整个函数（含文档注释，第 460-478 行）替换成：

```rust
/// 图标在列表实际取图尺寸下的实时预览。
///
/// 只画 32px 一档:三档列表现在都按 32 取图,继续预览 64 是在骗人 ——
/// 而「小尺寸下还认不认得出」正是选图标时唯一要判断的事。
fn icon_preview(
    ui: &mut Ui,
    t: &Theme,
    icon: Option<&mullion_store::IconSpec>,
    bg: Option<egui::Color32>,
) {
    let Some(icon) = icon else { return };
    let size = crate::ui::ico::SMALL;
    ui.horizontal(|ui| {
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(size as f32, size as f32), egui::Sense::hover());
        crate::ui::badge::paint_icon(ui.painter(), rect, icon, bg);
        ui.label(
            egui::RichText::new(format!("{size}px"))
                .size(11.0)
                .color(crate::theme::c32(t.fg_muted)),
        );
    });
}
```

- [ ] **Step 5: 修好引用了 `DEFAULT_ICON_BG` 的那条测试**

`fields.rs` 第 4238-4242 行，删掉这两行：

```rust
                buf.preserved_appearance.icon.as_mut().unwrap().bg =
                    Some(super::DEFAULT_ICON_BG.into());
```

并把它上方注释里的「有图标时才会出现「底色」那一行和两档预览」改成「有图标时才会出现预览那一档」。

- [ ] **Step 6: 更新预览行下方那句说明**

`fields.rs` 第 719-722 行那条 `ui.colored_label`，把文案改成：

```rust
    ui.colored_label(
        crate::theme::c32(t.fg_dimmer),
        "左栏列表里就是这个样子。选中时的背景色、右侧竖条和图标底色,都只在「作用于」勾了「会话列表」时出现。",
    );
```

- [ ] **Step 7: 跑测试 + clippy**

```bash
cargo test --workspace 2>&1 | tail -20
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20
```

预期：全绿、clippy 无输出。注意 `appearance_page_never_paints_past_the_panel_at_any_width_or_dpi` 必须仍绿——少了一行 UI 只会让页面更窄。

- [ ] **Step 8: 提交**

```bash
git add crates/mullion-app/src
git commit -m "feat(ui): 编辑器删「底色」配置,图标预览缩成 32px 一档 (F61)

底色改跟节点色,单独配的勾选框 + 色盘和 DEFAULT_ICON_BG 一并删掉。
预览只画 32px:三档列表现在都按 32 取图,继续预览 64 是在骗人。
预览的底色过 ListItem 闸门,与列表行同源。"
```

---

### Task 6: 全量绿 + 交付（版本 / 交叉编译 / Release）

按 `CLAUDE.md`「交付约定」一条龙做完，不再回头问。

**Files:**
- Modify: `Cargo.toml:12`（`workspace.package.version`）
- Modify: `spec.md`（F61/F62 的验收列补一句三档统一 32px、状态点下线）

- [ ] **Step 1: 更新 spec.md**

`spec.md` 第 178 行 F61 那一行的「验收」列末尾追加：

```
；三档密度(Full/Compact/Icons)统一用 32px 帧,行高与阈值有 `every_step_uses_the_32px_frame_and_the_row_fits_it` 钉着
```

第 179 行 F62 那一行的「验收」列末尾追加：

```
；选中/悬停行背景 = 节点色低透明度混色(选中 28% / 悬停 14%),8 个预设铺底后 `fg` 对比度 ≥ 4.5:1 单测;图标底色同源于该闸门
```

- [ ] **Step 2: 跑「绿」的完整定义**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log | tail -20
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
cargo fmt --check && echo "fmt OK"
```

预期：所有 `test result: ok`、clippy 无输出、`fmt OK`。任何一项不过就停下修，不进下一步。

- [ ] **Step 3: 升 patch 版本并单独提交**

`Cargo.toml` 第 12 行改成 `version = "0.1.28"`，然后：

```bash
git add Cargo.toml Cargo.lock spec.md
git commit -m "chore: 版本 0.1.28(左栏三档统一 32px 图标、状态点下线、选中行改节点色背景)"
```

- [ ] **Step 4: 交叉编译并做依赖验收**

```bash
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app 2>&1 | tail -5
objdump -p target/x86_64-pc-windows-gnu/release/mullion.exe | grep -i "DLL Name"
```

预期：编译成功；DLL 列表里**不得**出现 `libgcc_s_seh-1.dll` 或 `libwinpthread-1.dll`（出现即不合格，按 `docs/cross-compile-windows.md` 修）。

- [ ] **Step 5: 算 sha256 并写 release notes**

```bash
cp target/x86_64-pc-windows-gnu/release/mullion.exe /tmp/mullion.exe
cd /tmp && sha256sum mullion.exe > mullion.exe.sha256 && cat mullion.exe.sha256
```

把下面这份写进 `/tmp/notes.md`（`<SHA256>` 换成上一条命令实际输出的哈希）：

````markdown
## 改了什么

**会话管理器左栏（F61/F62/F80）**

1. **三档密度的图标统一成 32×32**。原来是 16（完整档）/ 32（紧凑档）/ 64（纯图标档）三个尺寸，
   现在都用导入 .ico 时归一化出来的 32px 那一帧。完整档行高 44→48、纯图标档 72→48；
   切档阈值 132→88，左栏最窄宽度 88→56（两个数不错开的话纯图标档永远拖不出来）。
   图标同时左移贴边（槽位中心 38→24），文字左界 54→48。

2. **连接状态指示点下线**。会话行左侧那颗四态圆点（已连接/连接中/失败/未连接）以及
   它那块 12×12 的隐形 tooltip 热区一并去掉。**代价：列表从此看不出哪台连上了**——
   连接状态归 pane 标题条那颗点管。

3. **选中/悬停行改用会话自己的颜色做背景**。选中 = 节点色 28% 与面板底混色，
   悬停 = 14%；没配颜色的会话保持改动前的灰底。原来那条左侧 3px 强调条删掉。
   8 个预设色铺底后正文对比度 ≥ 4.5:1 有单测钉着。

4. **图标底色改为跟随节点色**。编辑器「图标」页那一行独立的「底色」勾选框 + 色盘删掉；
   底色现在由各绘制落点按自己的「作用于」勾选决定（列表用「会话列表」、
   pane 标题条用「pane 标题条」）。图标预览也从 32/64 两档缩成只画 32px——
   列表已经不再用 64，继续预览它是在骗人。

旧配置里存过的图标底色（`IconSpec.bg`）**不会丢也不会报错**，只是不再生效；
配置格式版本没动，不需要迁移。

## 人工验收清单

无头容器验不了 GPU 渲染、颜色观感和对齐，以下全部**需要人工确认**：

1. 左栏拖到最窄（56）：是一条纯图标带，没设图标的会话整条消失，底部有「+N 无图标」
2. 拖到 ~120：图标 + 名称单行，名称过长有省略号
3. 拖到默认 300：32px 图标 + 名称 + `user@host` 两行，图标左边距 8px 看着不偏
4. 三档下都没有连接状态点，行左也没有隐形的 tooltip 热区
5. 选中一台配了颜色的会话：整行淡淡一层该颜色，文字清楚可读；换成浅色（黄）再看一次
6. 选中一台没配颜色的会话：跟改动前的灰底一致
7. 悬停未选中行：比选中态更淡的同色
8. 选中行左侧没有强调条；未选中的彩色行右边缘仍有 3px 竖条
9. 图标底色 = 节点色（同一块色）；编辑器「图标」页已无「底色」那一行
10. 编辑器图标预览只剩 32px 一档，与列表里看到的一致
11. pane 标题条那个 14px 图标：勾了「pane 标题条」落点时有底色，没勾时没有
12. 打开一份旧配置（里面存过图标底色）：不报错、图标照常显示、底色按节点色走

## 校验

```
SHA256: <SHA256>
```

## 首次运行

未签名的 exe 每个版本都会被 SmartScreen 拦一次。下载后先解除阻止：

```powershell
Unblock-File .\mullion.exe
```

详见 `docs/cross-compile-windows.md`。
````

- [ ] **Step 6: 发 Release**

```bash
cd /tmp && HTTPS_PROXY=http://127.0.0.1:7890 gh release create v0.1.28 \
  mullion.exe mullion.exe.sha256 -t "v0.1.28" -F notes.md --repo kilobitcy/Mullion
```

标题只能是纯版本号 `v0.1.28`。

- [ ] **Step 7: 报给用户**

Release 链接 + sha256 + 上面那份 12 条人工验收清单。明确标注：GPU 渲染效果、颜色观感、对齐、是否好看，全部**未验证，需人工确认**。

---

## 回归风险清单

实施时逐条留意，出现即停下问，不要自己绕：

1. **`circle_count` 基线不为 0**（Task 2 Step 2）：说明 egui 在左栏别处画了圆。先确认是哪来的，再决定改断言还是改实现。
2. **`SELECTED_ALPHA = 0.28` 有预设色过不了 4.5:1**（Task 3 Step 7）：下调 alpha，**不要**放宽测试阈值。
3. **`Density` 参数变成 `_d` 后 clippy 提示**（Task 1 Step 3）：`icon_px(_d: Density)` 保留参数是为了让三档共用一个调用形态、将来分档时不用改调用点。若 clippy 报 `unused_variables` 以外的东西，改成删参数并更新 5 处调用。
4. **`mullion_term::snapshot::Rgb` 的可见性**（Task 3）：`theme.rs` 已经 `use mullion_term::snapshot::Rgb`，`badge.rs`/`list.rs` 里写全路径即可，不要在 `mullion-term` 里加任何东西——那会违反依赖方向。
5. **陷阱 T3**：本次没动喂数据/重绘的解耦，但 `row_bg` 是每帧每行调用的。它是纯算术、无分配，不会引入 T3；若实施中想在里面加缓存或查表，先停下问。
