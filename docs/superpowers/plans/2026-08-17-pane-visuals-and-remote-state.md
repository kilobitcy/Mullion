# 分屏视觉 + 远端状态 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修掉终端文字重叠与标题条图标截断,给分屏加上分界线与焦点提示,让标题条显示远端目录/tmux 会话名并把它继承给 SFTP 面板,同时把标签栏隐藏成一个可回退的开关。

**Architecture:** 七项改动分成三条独立的链。**渲染链**(①⑤):`text.rs` 把网格字号收成一个唯一来源;`geom.rs` 把标题条高度从物理像素常量改成随 DPI 缩放的逻辑点。**视觉链**(③④⑦):新增 `ui/pane_edges.rs`,用 `ctx.layer_painter` 往 `layout_geometry` 已经让出来的 `GAP_PX` 缝里画分界线、往 pane 边界画焦点描边(不 allocate `Area`,不碰几何,因此既不吃指针事件也不触发 `window_change`);`chrome.rs` 加一个源码常量把标签栏整块短路掉。**远端状态链**(②⑥):`mullion-term` 新增纯函数模块 `remote_state`(OSC 7 嗅探 + 标题解析),`Emulator::feed` 顺路扫一遍,`Workspace::pump` 落到 `PaneState.cwd`/`.tmux`,再从那里分别流向标题条右区和 SFTP 面板的起始目录。

**Tech Stack:** Rust / egui 0.30 / cosmic-text(glyphon)/ alacritty_terminal 0.26 / vte。

**Spec:** `docs/superpowers/specs/2026-08-17-pane-visuals-and-remote-state-design.md`

---

## File Structure

| 文件 | 责任 | 动作 |
|---|---|---|
| `crates/mullion-app/src/text.rs` | 网格字号唯一来源 `grid_metrics` | 改 |
| `crates/mullion-app/src/ui/chrome.rs` | 标签栏开关 `SHOW_TAB_BAR` + `tab_bar_inner` | 改 |
| `crates/mullion-app/src/shell/workspace/geom.rs` | `TITLE_BAR_PT` + `title_bar_px(ppp)`,`layout_geometry` 收 ppp | 改 |
| `crates/mullion-app/src/shell/workspace/mod.rs` | 导出改名;`PaneState.cwd/.tmux`;`pump` 收远端状态 | 改 |
| `crates/mullion-app/src/theme.rs` | 新增 `divider` 色板项 | 改 |
| `crates/mullion-app/src/ui/pane_edges.rs` | ③④:分界线 + 焦点描边(唯一新文件,纯几何 + 一个 paint) | **新建** |
| `crates/mullion-app/src/ui/pane_title.rs` | ⑤ `icon_side`;④ 焦点 tint;⑥ 右区 `dir_leaf`/`side_text` | 改 |
| `crates/mullion-term/src/remote_state.rs` | ⑥ 纯解析:`RemoteState`/`Osc7Sniffer`/`parse_osc7`/`parse_title` | **新建** |
| `crates/mullion-term/src/emulator.rs` | ⑥ 接线:标题槽位 + 嗅探器 + `take_remote_state` | 改 |
| `crates/mullion-app/src/app.rs` | ⑥ 填 `TitleView`;② `files_start_dir` + 侧栏跃迁同步 | 改 |
| `crates/mullion-app/src/ui/mod.rs` | 调用 `pane_edges::paint`;测试里的 `TITLE_BAR_PX` | 改 |
| `docs/gui-render-gotchas.md` | 新增两条坑(网格字号同源、`layer_painter` vs `Area`) | 改 |
| `docs/remote-state-setup.md` | ⑥ 远端 tmux/shell 一行配置的运行手册 | **新建** |

---

## Task 1: ① 网格字号同源(修文字重叠)

**根因**:`prepare_panes` 用 `Metrics::new(self.cell_h * 0.8, self.cell_h)` 排版,而 `cell_w` 是用 `Metrics::new(font_px, line_h)` 量出来的。`cell_h = ceil(font_px * 1.25)`,所以 `cell_h * 0.8` 与 `font_px` 只在 `font_px * 1.25` 恰好是整数时相等。10pt@100% 时 `font_px = 13.33`、`cell_h = 17`、`cell_h * 0.8 = 13.6`,字比量的时候大 2%,一行 60 列累计漂出 1.2 格 —— 后面的字就压到前面的字上。10pt@150% 时 `font_px = 20.0`、`cell_h = 25`、`25 * 0.8 = 20.0` 恰好相等,所以同一份代码在某些缩放下看不出问题。

**Files:**
- Modify: `crates/mullion-app/src/text.rs:254`(`prepare_panes` 的 `metrics`)、`:377`(`measure_advance` 的 `Buffer::new`)
- Test: `crates/mullion-app/src/text.rs`(尾部 `mod tests`)

- [ ] **Step 1: Write the failing test**

在 `crates/mullion-app/src/text.rs` 的 `mod tests` 里追加(放在最后一个 `#[test]` 之后、`}` 之前):

```rust
    /// ①:**排版用的字号必须与量 `cell_w` 用的字号是同一个**。
    ///
    /// 不同源的话每格都差一点点,一行 60 列累计成整格,后面的字直接压到前面
    /// 的字上(用户报的现象:`.md` 和 `12 条` 重叠)。而且 `cell_h * 0.8`
    /// 与 `font_px` 在 `font_px * 1.25` 是整数时**恰好相等**,所以这个 bug
    /// 只在部分「字号 × 缩放」组合下出现 —— 必须遍历几组才盯得住。
    ///
    /// 判据是「60 个 `M` 的实际 advance == 60 × cell_w」,容差 0.5px:
    /// 半个像素以内人眼看不出,超过就是会累积的系统偏差。
    ///
    /// 自证会变红:把 `grid_metrics` 的第一个参数改回 `cell_h * 0.8`。
    #[test]
    fn the_font_size_used_for_layout_is_the_one_cell_w_was_measured_with() {
        let mut fs = FontSystem::new();
        for pt in [10.0_f32, 11.0, 13.0] {
            for scale in [1.0_f32, 1.25, 1.5] {
                let font_px = pt * scale * 96.0 / 72.0;
                let cell_h = (font_px * 1.25).ceil();
                let cell_w = measure_cell_w(&mut fs, font_px, cell_h, None);

                const COLS: usize = 60;
                let mut buf = Buffer::new(&mut fs, grid_metrics(font_px, cell_h));
                buf.set_text(
                    &mut fs,
                    &"M".repeat(COLS),
                    Attrs::new().family(Family::Name(DEFAULT_FONT_FAMILY)),
                    Shaping::Advanced,
                );
                buf.shape_until_scroll(&mut fs, false);
                let laid = buf
                    .layout_runs()
                    .next()
                    .and_then(|run| run.glyphs.last().map(|g| g.x + g.w))
                    .expect("60 个 M 应该排出一行");

                let want = cell_w * COLS as f32;
                assert!(
                    (laid - want).abs() < 0.5,
                    "pt={pt} scale={scale}: 排版排出 {laid:.2}px,按 cell_w \
                     算应是 {want:.2}px,偏了 {:.2}px（{:.2} 格）—— 排版字号\
                     与量 cell_w 的字号不同源",
                    laid - want,
                    (laid - want) / cell_w
                );
            }
        }
    }
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p mullion-app --lib text::tests::the_font_size_used_for_layout_is_the_one_cell_w_was_measured_with 2>&1 | tail -20
```

Expected: 编译失败 `cannot find function 'grid_metrics' in this scope`。

- [ ] **Step 3: Add `grid_metrics` and route both sites through it**

在 `crates/mullion-app/src/text.rs` 里 `measure_cell_w` 的**上方**插入:

```rust
/// 终端网格的排版度量。**唯一来源** —— 量 `cell_w` 的那次和真正排版的那次
/// 必须用同一个 `Metrics`。
///
/// 曾经不同源:排版用 `Metrics::new(cell_h * 0.8, cell_h)`,而 `cell_w` 是按
/// `Metrics::new(font_px, cell_h)` 量的。`cell_h = ceil(font_px * 1.25)`,
/// 两者只在 `font_px * 1.25` 恰好是整数时相等 —— 10pt@150% 相等、10pt@100%
/// 差 2%,一行 60 列漂出 1.2 格,字压字。守护:
/// `the_font_size_used_for_layout_is_the_one_cell_w_was_measured_with`。
fn grid_metrics(font_px: f32, cell_h: f32) -> Metrics {
    Metrics::new(font_px, cell_h)
}
```

把 `prepare_panes` 里的

```rust
        let metrics = Metrics::new(self.cell_h * 0.8, self.cell_h);
```

改成

```rust
        let metrics = grid_metrics(self.font_px, self.cell_h);
```

把 `measure_advance` 里的

```rust
    let mut buf = Buffer::new(fs, Metrics::new(font_px, line_h));
```

改成

```rust
    let mut buf = Buffer::new(fs, grid_metrics(font_px, line_h));
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test -p mullion-app --lib text:: 2>&1 | tail -5
```

Expected: `test result: ok.`,`text::tests` 全绿。

- [ ] **Step 5: Commit**

```bash
git add crates/mullion-app/src/text.rs
git commit -m "fix(app): 网格排版字号与 cell_w 同源,修文字重叠 (F80)

排版用 Metrics::new(cell_h * 0.8, cell_h)、量 cell_w 用
Metrics::new(font_px, cell_h),两者只在 font_px * 1.25 为整数时相等。
10pt@100% 下每格差 2%,一行 60 列漂 1.2 格,后面的字压到前面的字上。
收成唯一来源 grid_metrics。

守护测试:text::tests::the_font_size_used_for_layout_is_the_one_cell_w_was_measured_with"
```

---

## Task 2: ⑦ 隐藏标签栏(留开关)

**做法**:源码常量 `SHOW_TAB_BAR`,`tab_bar` 变成薄壳、真正的实现搬进 `tab_bar_inner(ctx, t, views, enabled)`。既有三条标签栏测试全部走 `tab_bar_inner(..., true)` 继续跑 —— **不删断言**,F36 的行为约定仍然被守着,将来把常量翻回 `true` 就能直接复用。

**Files:**
- Modify: `crates/mullion-app/src/ui/chrome.rs`
- Test: `crates/mullion-app/src/ui/chrome.rs`(`mod tests`)

- [ ] **Step 1: Write the failing test**

在 `crates/mullion-app/src/ui/chrome.rs` 的 `mod tests` 尾部追加:

```rust
    /// ⑦:标签栏现在默认隐藏 —— **一个像素都不占**(不是画成透明、也不是
    /// 高度置 0 的空 panel:空 panel 仍会分掉中央区,`central_px` 跟着变,
    /// 白发一次 `window_change`,T4)。
    ///
    /// 同时钉住「只是隐藏、没被删掉」:`tab_bar_inner(.., true)` 必须照旧
    /// 出条 —— 这条测试和下面三条既有测试一起构成「开关能翻回来」的证据。
    ///
    /// 自证会变红:把 `SHOW_TAB_BAR` 改回 `true`(第一条断言红);把
    /// `tab_bar_inner` 里的 `TopBottomPanel` 删掉(第二条红)。
    #[test]
    fn the_tab_bar_is_hidden_by_default_but_still_buildable() {
        let views = [TabView {
            title: "a",
            active: true,
            session_id: None,
            appearance: None,
            color_override: None,
        }];

        let ctx = egui::Context::default();
        let t = crate::theme::MULLION_DARK;
        let mut before = 0.0;
        let mut after = 0.0;
        let _ = ctx.run(Default::default(), |ctx| {
            before = ctx.available_rect().height();
            tab_bar(ctx, &t, &views);
            after = ctx.available_rect().height();
        });
        assert!(
            (before - after).abs() < f32::EPSILON,
            "标签栏关掉之后还在吃中央区的高度({before} → {after})"
        );

        let ctx2 = egui::Context::default();
        let mut enabled_after = 0.0;
        let mut enabled_before = 0.0;
        let _ = ctx2.run(Default::default(), |ctx| {
            enabled_before = ctx.available_rect().height();
            tab_bar_inner(ctx, &t, &views, true);
            enabled_after = ctx.available_rect().height();
        });
        assert!(
            enabled_before - enabled_after > 1.0,
            "把开关打开之后标签栏应该照旧出条,实测中央区只矮了 {}",
            enabled_before - enabled_after
        );
    }
```

> 注:`TabView` 的字段以 `chrome.rs` 里 `pub struct TabView` 的当前定义为准 —— 实现时照抄同文件既有测试(`tab_bar_is_shown_even_with_a_single_tab`)里构造 `TabView` 的写法,不要凭这里的字段列表猜。

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p mullion-app --lib ui::chrome::tests::the_tab_bar_is_hidden_by_default_but_still_buildable 2>&1 | tail -20
```

Expected: 编译失败 `cannot find function 'tab_bar_inner'`。

- [ ] **Step 3: Add the switch**

在 `crates/mullion-app/src/ui/chrome.rs` 里 `pub fn tab_bar` 的**上方**插入:

```rust
/// ⑦:标签栏总开关。当前隐藏 —— 用户嫌它占地方,而多标签今天还能靠
/// `Ctrl+Tab`(D0 的 `shell::tabs::hotkey`)和菜单切换,不缺入口。
///
/// **留常量而不是删代码**:F36 的行为约定(一个标签也出条、恒定高度、
/// annotate 注册)全都还有测试守着 —— 那些测试走 `tab_bar_inner(.., true)`,
/// 把这里翻回 `true` 就是完整可用的标签栏,不需要考古。
const SHOW_TAB_BAR: bool = false;
```

把 `pub fn tab_bar(ctx: &egui::Context, t: &Theme, views: &[TabView<'_>]) -> Option<TabAction> {` 这一行连同**紧跟其后的整个函数体**改成:

```rust
pub fn tab_bar(ctx: &egui::Context, t: &Theme, views: &[TabView<'_>]) -> Option<TabAction> {
    tab_bar_inner(ctx, t, views, SHOW_TAB_BAR)
}

/// 标签栏本体。`enabled == false` 时**什么都不建**并立刻返回 —— 不能建一个
/// 高度 0 的 panel:`TopBottomPanel` 无论多矮都会从 `available_rect` 里分走
/// 自己那一份,中央区因此变矮,`central_px` → `layout_geometry` →
/// `pty.resize` 白发一次 `window_change`(T4)。
fn tab_bar_inner(
    ctx: &egui::Context,
    t: &Theme,
    views: &[TabView<'_>],
    enabled: bool,
) -> Option<TabAction> {
    if !enabled {
        return None;
    }
    // …… 原 `tab_bar` 的函数体原封不动搬到这里 ……
}
```

> 实现要求:原函数体**逐字搬移**,不改一个字符。搬完 `git diff` 里除了新增的壳、常量、`if !enabled` 三处,不应有其它行变动。

同文件三处既有测试的调用点改为显式开启:
- `fn tab_titles(views: ...)` 里的 `tab_bar(ctx, &t, views)` → `tab_bar_inner(ctx, &t, views, true)`
- `fn bar_height(tabs: ...)` 里的 `tab_bar(ctx, &t, ...)` → `tab_bar_inner(ctx, &t, ..., true)`
- `annotate_mode_registers_the_tab_bar_and_each_tab` 里的 `tab_bar(...)` → `tab_bar_inner(..., true)`

并在这三处各自的文档注释末尾补一行:

```rust
    /// ⑦:走 `tab_bar_inner(.., true)` 而不是 `tab_bar` —— 标签栏默认隐藏了,
    /// 这条测试守的是「开关打开时的行为」,不该跟着开关一起失效。
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p mullion-app --lib ui::chrome:: 2>&1 | tail -5
```

Expected: `test result: ok.`,包含新加的那条和三条既有标签栏测试。

- [ ] **Step 5: Commit**

```bash
git add crates/mullion-app/src/ui/chrome.rs
git commit -m "feat(app): 标签栏改成源码常量开关,默认隐藏 (F36)

SHOW_TAB_BAR=false 时 tab_bar 直接返回 None,一个 panel 都不建 ——
建高度 0 的 panel 仍会分走中央区高度,白发一次 window_change(T4)。
F36 的三条行为测试改走 tab_bar_inner(.., true),开关翻回来即可用。

守护测试:ui::chrome::tests::the_tab_bar_is_hidden_by_default_but_still_buildable"
```

---

## Task 3: ⑤ 标题条高度随 DPI 缩放 + 图标夹紧

**根因**:`TITLE_BAR_PX = 32` 是**物理像素**常量,而标题条内容(`shrink2(8, 4)`、图标 `14.0`)是**逻辑点**。150% 缩放下标题条只有 `32 / 1.5 = 21.33` 点高,内高 `21.33 - 8 = 13.33` 点,装不下 14 点的图标 —— 底部被 `set_clip_rect(inner)` 裁掉。

**两处都改**:标题条高度改成随 DPI 缩放的 32 **逻辑点**(根治,顺带解决主机名在高 DPI 下被压扁);图标边长按内高夹紧(兜住极小 pane 上 `TITLE_BAR_PX.min(px.h)` 把标题条压扁的情形)。

**Files:**
- Modify: `crates/mullion-app/src/shell/workspace/geom.rs`、`crates/mullion-app/src/shell/workspace/mod.rs:9`、`crates/mullion-app/src/ui/pane_title.rs`、`crates/mullion-app/src/app.rs`、`crates/mullion-app/src/ui/mod.rs`
- Test: `crates/mullion-app/src/shell/workspace/geom.rs`、`crates/mullion-app/src/ui/pane_title.rs`

### 3a: `title_bar_px(ppp)`

- [ ] **Step 1: Write the failing test**

在 `crates/mullion-app/src/shell/workspace/geom.rs` 的 `mod tests` 尾部追加:

```rust
    /// ⑤:标题条高度是**逻辑点**,要随 DPI 缩放。
    ///
    /// 32 物理像素在 150% 缩放下只有 21.33 逻辑点,而标题条内容(上下各
    /// 4 点边距 + 14 点图标)按逻辑点排 —— 内高 13.33 点装不下 14 点的图标,
    /// 底部被 clip 掉(用户报的「图标底部被截断」)。
    ///
    /// 自证会变红:把 `title_bar_px` 改回 `|_| 32`。
    #[test]
    fn the_title_bar_is_thirty_two_logical_points_at_any_scale() {
        assert_eq!(title_bar_px(1.0), 32);
        assert_eq!(title_bar_px(1.25), 40);
        assert_eq!(title_bar_px(1.5), 48);
        assert_eq!(title_bar_px(2.0), 64);
    }

    /// 坏 ppp 不能把标题条算成 0 或天文数字:winit 在某些显示器热插拔的瞬间
    /// 报过 0 / NaN 的 scale_factor(见 docs/gui-render-gotchas.md 的 wgpu
    /// 尺寸 NaN 那条),算出 0 的话标题条整排消失、算出巨值的话终端区被挤成
    /// 0 行,两者都不可恢复(下一帧的 ppp 正常了才回来)。
    ///
    /// 自证会变红:删掉 `title_bar_px` 里的 `is_finite() && > 0.0` 兜底。
    #[test]
    fn a_bogus_scale_factor_falls_back_to_one() {
        assert_eq!(title_bar_px(0.0), 32);
        assert_eq!(title_bar_px(-2.0), 32);
        assert_eq!(title_bar_px(f32::NAN), 32);
        assert_eq!(title_bar_px(f32::INFINITY), 32);
    }
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p mullion-app --lib shell::workspace::geom::tests::the_title_bar_is 2>&1 | tail -20
```

Expected: 编译失败 `cannot find function 'title_bar_px'`。

- [ ] **Step 3: Implement**

在 `crates/mullion-app/src/shell/workspace/geom.rs` 里,把

```rust
pub const TITLE_BAR_PX: u32 = 32;
```

替换成(**保留原有的文档注释,在其后追加下面的说明**):

```rust
/// 标题条高度,**逻辑点**。物理像素要走 [`title_bar_px`]。
///
/// 曾经是物理像素常量 `TITLE_BAR_PX = 32`。标题条里的内容(`shrink2(8, 4)`
/// 的边距、图标边长)全是逻辑点,150% 缩放下 32 物理像素只有 21.33 点,
/// 内高 13.33 点装不下 14 点的图标 —— 底部被 clip 掉。geom 层是物理像素的
/// 地盘(见 `ui/toolbar.rs` 顶部那段单位说明),所以缩放在**边界**上做:
/// 常量存点,`layout_geometry` 收 ppp 换成像素。
pub const TITLE_BAR_PT: f32 = 32.0;

/// 当前 DPI 下标题条占多少物理像素。
///
/// 非有限或非正的 `ppp` 落回 1.0:winit 在显示器热插拔的瞬间报过 0 / NaN
/// 的 `scale_factor`,算出 0 会让标题条整排消失、算出巨值会把终端区挤成
/// 0 行,而且两者都要等下一帧 ppp 正常了才恢复。
pub fn title_bar_px(ppp: f32) -> u32 {
    let ppp = if ppp.is_finite() && ppp > 0.0 { ppp } else { 1.0 };
    (TITLE_BAR_PT * ppp).round() as u32
}
```

`layout_geometry` 加参数(签名与 `title_h` 那一处):

```rust
pub fn layout_geometry(
    tree: &Tree,
    area: PxRect,
    cell: (f32, f32),
    title_bars: bool,
    /// 当前窗口的 `scale_factor`。标题条高度按它换算(⑤)——
    /// **不要在这里读全局**,geom 是纯函数层,ppp 由调用方(`compute_geoms`)
    /// 从 `window.scale_factor()` 传进来。
    ppp: f32,
) -> Vec<PaneGeom> {
```

函数体里:

```rust
            let title_h = if title_bars {
                title_bar_px(ppp).min(px.h)
```

`crates/mullion-app/src/shell/workspace/mod.rs:9` 的导出改成:

```rust
pub use geom::{layout_geometry, title_bar_px, PaneGeom, PxRect, GAP_PX, TITLE_BAR_PT};
```

- [ ] **Step 4: Fix every call site**

```bash
cd /data/Mullion
grep -rn "layout_geometry(\|TITLE_BAR_PX" crates/ --include=*.rs
```

逐一处理:
- `crates/mullion-app/src/app.rs:3634` 的 `compute_geoms`:末尾补一个真实 ppp 参数 —— 该函数里已有 `a.window.scale_factor()`(与 `ui/mod.rs` 算 `central_px` 用的同一个值),传 `a.window.scale_factor() as f32`。
- `geom.rs`/`app.rs`/其它测试里的 `layout_geometry(&tree, area, cell, bool)` → 末尾补 `, 1.0`。
- `geom.rs` 测试里的 `TITLE_BAR_PX` → `title_bar_px(1.0)`。
- `pane_title.rs:281`、`:405`、`ui/mod.rs:1381`、`:1385`、`:1387`(测试里造 `PxRect`)→ `title_bar_px(1.0)`;对应的 `use` 改成 `title_bar_px`。
- `pane_title.rs:372`、`:701`(按 ppp 算期望高度的 DPI 测试)→ `title_bar_px(ppp) as f32 / ppp`。
- `ui/toolbar.rs:19` 的注释里 `TITLE_BAR_PX` → `TITLE_BAR_PT`。

```bash
cargo test -p mullion-app --lib 2>&1 | tail -5
```

Expected: `test result: ok.`(全 crate 单测通过)。

### 3b: 图标边长按内高夹紧

- [ ] **Step 5: Write the failing test**

在 `crates/mullion-app/src/ui/pane_title.rs` 的 `mod tests` 尾部追加:

```rust
    /// ⑤:图标必须**整个**画在标题条内容区里,任何 DPI 下都不许被裁。
    ///
    /// `paint_icon` 走 `painter.image`,在形状列表里是唯一的 `Shape::Mesh`
    /// (文字是 `Shape::Text`,底色/竖条是 `Shape::Rect`)—— 拿它的
    /// `visual_bounding_rect()` 跟 `inner`(= `full.shrink2(8, 4)`)比。
    ///
    /// 曾经的 bug:标题条高度是 32 **物理**像素常量,而边距/图标是逻辑点,
    /// 150% 缩放下内高只有 13.33 点,装不下 14 点的图标,底部被
    /// `set_clip_rect(inner)` 裁掉。
    ///
    /// 自证会变红:把 `icon_side` 的 `clamp` 上限改成 `f32::MAX`(高 DPI 下
    /// 图标比内高还大);或把 `allocate_exact_size` 的边长写回字面量 `14.0`
    /// 并把 Task 3a 的 `title_bar_px` 改回 `|_| 32`。
    #[test]
    fn the_icon_fits_inside_the_title_bar_at_every_scale() {
        for ppp in [1.0_f32, 1.25, 1.5, 2.0] {
            let ap = crate::ui::badge::Appearance {
                icon: Some(real_ico()),
                ..Default::default()
            };
            let id = PaneId(7);
            let views = [TitleView {
                geom: geom_800x600_title32(id, ppp),
                index: 1,
                host: Some("h"),
                status: PaneStatus::Live,
                focused: false,
                appearance: Some(&ap),
                cwd_leaf: None,
                tmux: None,
            }];

            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(ppp);
            let t = crate::theme::MULLION_DARK;
            let mut shapes = Vec::new();
            for _ in 0..2 {
                let out = ctx.run(Default::default(), |ctx| {
                    show(ctx, &t, &views);
                });
                shapes = out.shapes;
            }

            let full = {
                let tp = views[0].geom.title_px;
                egui::Rect::from_min_size(
                    egui::pos2(tp.x as f32 / ppp, tp.y as f32 / ppp),
                    egui::vec2(tp.w as f32 / ppp, tp.h as f32 / ppp),
                )
            };
            let inner = full.shrink2(egui::vec2(8.0, 4.0));

            let mut seen = 0;
            for cs in &shapes {
                collect_meshes(&cs.shape, &mut |r: egui::Rect| {
                    seen += 1;
                    assert!(
                        inner.contains_rect(r),
                        "ppp={ppp}: 图标 {r:?} 没装进内容区 {inner:?}（内高 {:.2} 点）",
                        inner.height()
                    );
                });
            }
            assert_eq!(seen, 1, "ppp={ppp}: 该恰好画出一个图标 mesh,实际 {seen} 个");
        }
    }

    /// 递归找 `Shape::Mesh`（`Vec` / `Callback` 之外的容器只有 `Vec`）。
    fn collect_meshes(s: &egui::Shape, f: &mut impl FnMut(egui::Rect)) {
        match s {
            egui::Shape::Mesh(_) => f(s.visual_bounding_rect()),
            egui::Shape::Vec(v) => {
                for x in v {
                    collect_meshes(x, f);
                }
            }
            _ => {}
        }
    }
```

同文件的 `geom_800x600_title32(id)` 改成收 ppp:

```rust
    /// 800×600 物理像素、开着标题条的单 pane 几何。
    ///
    /// ⑤:标题条高度现在随 DPI 走,所以要传 ppp —— 传死 32 的话高 DPI 的
    /// 用例量的就不是真实几何。
    fn geom_800x600_title32(id: PaneId, ppp: f32) -> PaneGeom {
```

(函数体里 `title_bar_px(1.0)` 改 `title_bar_px(ppp)`;其余既有调用点传 `1.0`,DPI 相关的那两条传各自的 `ppp`。)

- [ ] **Step 6: Run test to verify it fails**

```bash
cargo test -p mullion-app --lib ui::pane_title::tests::the_icon_fits 2>&1 | tail -30
```

Expected: 编译失败(`cwd_leaf`/`tmux` 字段还不存在、`icon_side` 还没有)。**先只加 `icon_side` 与 `TitleView` 的两个新字段(值先都填 `None`,渲染在 Task 8 接)**,再跑一次,Expected: `ppp=1.5: 图标 ... 没装进内容区` FAIL。

> 说明:这条测试跨 Task 3 与 Task 8 两处改动。Task 3 只需要让它变绿到「几何正确」为止,所以这一步就把 `TitleView` 的 `cwd_leaf: Option<String>` / `tmux: Option<&'a str>` 两个字段加上(带 Task 8 里给出的文档注释),`show` 里先不读它们 —— 编译器会因为「字段未使用」而不报错(pub 字段不触发 dead_code)。

- [ ] **Step 7: Implement `icon_side`**

在 `crates/mullion-app/src/ui/pane_title.rs` 里 `pub fn title_text` 的**上方**插入:

```rust
/// ⑤:图标边长(逻辑点)。
///
/// 目标是 16 点(设计稿尺寸,再大就压过主机名);上限之外还要有下限:
/// 极小 pane 上 `layout_geometry` 会用 `title_bar_px(ppp).min(px.h)` 把标题条
/// 压扁,那时按内高缩到 10 点为止 —— 再小就是一坨糊,不如让它糊得整齐些。
/// 减 2 点是给图标留呼吸空间,不让它顶到边距线上。
pub fn icon_side(inner_h: f32) -> f32 {
    (inner_h - 2.0).clamp(10.0, 16.0)
}
```

把 `show` 里的

```rust
                            let (r, _) = ui
                                .allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
```

改成

```rust
                            // ⑤:边长按内容区高度算,不写死 —— 见 `icon_side`。
                            let side = icon_side(inner.height());
                            let (r, _) = ui.allocate_exact_size(
                                egui::vec2(side, side),
                                egui::Sense::hover(),
                            );
```

- [ ] **Step 8: Run tests**

```bash
cargo test -p mullion-app --lib 2>&1 | tail -5
cargo test -p mullion-app --lib ui::pane_title::tests::the_icon_fits -- --exact 2>&1 | tail -5
```

Expected: 两条都 `test result: ok.`。

- [ ] **Step 9: Commit**

```bash
git add crates/mullion-app/src
git commit -m "fix(app): 标题条高度随 DPI 缩放 + 图标按内高夹紧,修图标截断 (F83)

TITLE_BAR_PX=32 是物理像素常量,而边距/图标是逻辑点:150% 缩放下标题条
只有 21.33 点、内高 13.33 点,装不下 14 点的图标,底部被 clip 掉。
常量改成 TITLE_BAR_PT(逻辑点)+ title_bar_px(ppp),layout_geometry 收 ppp;
图标边长走 icon_side(内高) 兜住极小 pane 把标题条压扁的情形。

守护测试:shell::workspace::geom::tests::the_title_bar_is_thirty_two_logical_points_at_any_scale
          shell::workspace::geom::tests::a_bogus_scale_factor_falls_back_to_one
          ui::pane_title::tests::the_icon_fits_inside_the_title_bar_at_every_scale
T4:layout_geometry 的调用点只有 compute_geoms 一处,ppp 从 window.scale_factor() 来"
```

---

## Task 4: ③ 分界线色板项

**Files:**
- Modify: `crates/mullion-app/src/theme.rs`
- Test: `crates/mullion-app/src/theme.rs`(`mod tests`)

- [ ] **Step 1: Write the failing test**

在 `crates/mullion-app/src/theme.rs` 的 `mod tests` 尾部追加:

```rust
    /// ③:分界线要「不干扰视觉重点,但也不能忽略」。
    ///
    /// **不能退回 `theme::stroke`**(白 6%):1 物理像素的 6% 白叠在
    /// `term_bg #14161f` 上只抬约 14/255,深色屏 + 1px 宽下基本看不见 ——
    /// 那就成了「可以忽略」,正好是用户明确否掉的一头。
    /// 也不能太亮:比终端底色抬超过 64/255 就成了一条抢眼的白线。
    ///
    /// 自证会变红:把 `divider` 改成与 `term_bg` 同值(下界红);
    /// 改成 `#ffffff`(上界红)。
    #[test]
    fn the_divider_is_visible_but_not_loud_against_the_terminal_background() {
        let t = MULLION_DARK;
        for (name, a, b) in [
            ("r", t.divider.r, t.term_bg.r),
            ("g", t.divider.g, t.term_bg.g),
            ("b", t.divider.b, t.term_bg.b),
        ] {
            let lift = i32::from(a) - i32::from(b);
            assert!(
                (16..=64).contains(&lift),
                "divider 的 {name} 通道比 term_bg 抬了 {lift}/255,该落在 16..=64:\
                 低于 16 眼睛看不见(等于没画),高于 64 是一条抢眼的白线"
            );
        }
    }
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p mullion-app --lib theme::tests::the_divider_is_visible 2>&1 | tail -20
```

Expected: 编译失败 `no field 'divider' on type 'Theme'`。

- [ ] **Step 3: Implement**

在 `crates/mullion-app/src/theme.rs` 的 `pub struct Theme` 里,`stroke_alpha` 字段之后插入:

```rust
    /// ③ F80:分屏之间那 1 物理像素分界线的颜色。
    ///
    /// **F80 冻结色表里没有这一项**,因为当时根本没画分界线 —— 这是新增项,
    /// 不是改既有值。为什么不复用 `stroke`(白 6%):`stroke` 是画在
    /// `panel_bg` 上的边框,分界线画在 `term_bg` 上、只有 1 物理像素宽,
    /// 6% 的白在那个条件下看不见。守护:
    /// `the_divider_is_visible_but_not_loud_against_the_terminal_background`。
    pub divider: Rgb,
```

在 `MULLION_DARK` 里对应位置插入:

```rust
    divider: Rgb::new(0x28, 0x2b, 0x38),
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p mullion-app --lib theme:: 2>&1 | tail -5
```

Expected: `test result: ok.`。

- [ ] **Step 5: Commit**

```bash
git add crates/mullion-app/src/theme.rs
git commit -m "feat(app): 色板新增 divider(分屏分界线) (F80)

F80 冻结色表里没有这一项 —— 当时不画分界线,属新增而非改既有值。
不复用 stroke(白 6%):1px 宽 + 深色 term_bg 下看不见。

守护测试:theme::tests::the_divider_is_visible_but_not_loud_against_the_terminal_background"
```

---

## Task 5: ③④ 分界线 + 焦点描边

**关键约束**(违反其中任何一条都会引入比原问题更糟的 bug):
- **不 allocate `Area`** —— 走 `ctx.layer_painter`。`Area` 会占一块可交互矩形,盖在终端上就把指针事件吃掉,划选当场失效(T8 的指针路由是「先喂 egui 后判」)。
- **不改几何** —— 分界线画进 `layout_geometry` 已经让出来的 `GAP_PX` 缝里。给焦点 pane 缩 `term_px` 会改 `grid`,每切一次焦点就发一次 `window_change`,远端 TUI 每点一下重排一次(T4)。

**Files:**
- Create: `crates/mullion-app/src/ui/pane_edges.rs`
- Modify: `crates/mullion-app/src/ui/mod.rs`(`mod pane_edges;` + 调用)、`crates/mullion-app/src/ui/pane_title.rs`(焦点 tint)

- [ ] **Step 1: Write the failing test (纯几何)**

新建 `crates/mullion-app/src/ui/pane_edges.rs`,先只写测试与文档:

```rust
//! ③④:分屏之间的分界线与当前焦点分屏的描边。
//!
//! **两件事都画在既有像素里,不改任何几何。** 分界线落在 `GAP_PX` 那条缝上
//! (`layout_geometry` 里非最右/最下的 pane 各让出 1 物理像素);焦点描边落在
//! pane `px` 的内边界上。给焦点 pane 缩终端区是不行的 —— 那会改 `grid`,
//! 每切一次焦点都要发 `window_change`(T4),远端 TUI 每点一下重排一次。
//!
//! **用 `ctx.layer_painter` 而不是 `egui::Area`**:`Area` 会 allocate 一块
//! 可交互矩形,盖在终端上就把指针事件吃了(T8 的指针路由是「先喂 egui
//! 后判」),划选当场失效。`layer_painter` 只画不占。层序取 `Order::Background`:
//! egui 整层 composite 在 wgpu 自绘的终端之上,所以 `Background` 也在终端
//! 之上,同时在面板/弹窗之下 —— 不会盖住模态框。

use crate::shell::workspace::{PaneGeom, PxRect, GAP_PX};
use crate::theme::{self, Theme};
use crate::ui::pane_title::TitleView;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::workspace::layout_geometry;
    use mullion_core::layout::{Dir, PaneId, Tree};

    const AREA: PxRect = PxRect {
        x: 0,
        y: 0,
        w: 800,
        h: 600,
    };
    const CELL: (f32, f32) = (10.0, 20.0);

    /// 单 pane 没有邻居,一条线都不该画 —— 画了就是在终端最外一列/一行上
    /// 糊一道亮线,而那里根本没有缝。
    ///
    /// 自证会变红:把 `divider_lines_of` 的两个判断改成恒 `true`。
    #[test]
    fn a_lone_pane_has_no_divider_at_all() {
        let tree = Tree::new(PaneId(1));
        let g = layout_geometry(&tree, AREA, CELL, false, 1.0);
        assert_eq!(divider_lines_of(&g[0]), (None, None));
    }

    /// ③ 的核心几何约定:竖线必须落在**两块 pane 的 `term_px` 都没占用**的
    /// 那 1 像素上。压进任一块 `term_px` 就是盖掉终端最外一列字形 ——
    /// 用户会看到行尾/行首的字被切掉一条,而且只在分屏时出现,极难归因。
    ///
    /// 自证会变红:把竖线的 x 改成 `g.px.x + g.term_px.w - 1`(压左 pane)
    /// 或 `g.px.x + g.px.w`(压右 pane)。
    #[test]
    fn the_vertical_divider_lands_in_the_gap_that_no_pane_owns() {
        let mut tree = Tree::new(PaneId(1));
        tree.split(PaneId(1), Dir::Horizontal, PaneId(2));
        let g = layout_geometry(&tree, AREA, CELL, false, 1.0);
        assert_eq!(g.len(), 2, "该分出两块");

        let (right, bottom) = divider_lines_of(&g[0]);
        let line = right.expect("左 pane 该有一条右缘竖线");
        assert_eq!(bottom, None, "左右分屏不该有横线");
        assert_eq!(line.w, GAP_PX, "分界线就是那条缝的宽度,不许更宽");

        for p in &g {
            let te = p.term_px;
            assert!(
                line.x >= te.x + te.w || line.x + line.w <= te.x,
                "竖线 {line:?} 压在 pane {:?} 的终端区 {te:?} 上",
                p.id
            );
        }
        assert_eq!(
            divider_lines_of(&g[1]),
            (None, None),
            "最右那块没让出缝,不该再画线(会画到窗口边缘外/邻居身上)"
        );
    }

    /// 上下分屏的对偶。**单独一条**而不是并进上面那条:横线的 y 要跳过
    /// 标题条那一段,算错的话线会横穿终端第一行。
    ///
    /// 自证会变红:把横线的 y 改成 `g.px.y + g.px.h - 1`。
    #[test]
    fn the_horizontal_divider_lands_in_the_gap_that_no_pane_owns() {
        let mut tree = Tree::new(PaneId(1));
        tree.split(PaneId(1), Dir::Vertical, PaneId(2));
        let g = layout_geometry(&tree, AREA, CELL, true, 1.0);

        let (right, bottom) = divider_lines_of(&g[0]);
        assert_eq!(right, None, "上下分屏不该有竖线");
        let line = bottom.expect("上 pane 该有一条下缘横线");
        assert_eq!(line.h, GAP_PX);

        for p in &g {
            let te = p.term_px;
            assert!(
                line.y >= te.y + te.h || line.y + line.h <= te.y,
                "横线 {line:?} 压在 pane {:?} 的终端区 {te:?} 上",
                p.id
            );
        }
        assert_eq!(divider_lines_of(&g[1]), (None, None));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

先在 `crates/mullion-app/src/ui/mod.rs` 的 `mod` 声明区(与 `mod pane_title;` 相邻)加一行:

```rust
mod pane_edges;
```

```bash
cargo test -p mullion-app --lib ui::pane_edges:: 2>&1 | tail -20
```

Expected: 编译失败 `cannot find function 'divider_lines_of'`。

- [ ] **Step 3: Implement `divider_lines_of`**

在 `pane_edges.rs` 的 `use` 之后、`mod tests` 之前插入:

```rust
/// 这块 pane 让给分界线的两条缝:`(右缘竖线, 下缘横线)`。没让位就是 `None`。
///
/// 判据是「`term_px` 比 `px` 小」—— 与 `layout_geometry` 里 `at_right` /
/// `at_bottom` 那两个判断同源,不在这里重新推一遍「谁在边上」(推第二遍
/// 就会有第二份真值,布局一改就分叉)。
///
/// 竖线只跨 `term_px` 的纵向范围:标题条那一段的同一列像素由标题条自己的
/// 底色填满,再画一道会在标题条上多出一截亮线。
pub fn divider_lines_of(g: &PaneGeom) -> (Option<PxRect>, Option<PxRect>) {
    let right = (g.term_px.w < g.px.w).then(|| PxRect {
        x: g.px.x + g.term_px.w,
        y: g.term_px.y,
        w: GAP_PX,
        h: g.term_px.h,
    });
    let bottom = (g.term_px.y + g.term_px.h < g.px.y + g.px.h).then(|| PxRect {
        x: g.px.x,
        y: g.term_px.y + g.term_px.h,
        w: g.px.w,
        h: GAP_PX,
    });
    (right, bottom)
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p mullion-app --lib ui::pane_edges:: 2>&1 | tail -5
```

Expected: `test result: ok. 3 passed`。

- [ ] **Step 5: Write the failing test (焦点描边)**

在 `pane_edges.rs` 的 `mod tests` 尾部追加:

```rust
    fn view(g: PaneGeom, index: usize, focused: bool) -> TitleView<'static> {
        TitleView {
            geom: g,
            index,
            host: Some("h"),
            status: crate::shell::workspace::PaneStatus::Live,
            focused,
            appearance: None,
            cwd_leaf: None,
            tmux: None,
        }
    }

    /// 画一帧,返回所有形状。两帧是因为 egui 的部件首帧只是「上帧」——
    /// 本文件不建 `Area`,一帧其实就够,但保持与 `pane_title` 测试同一套
    /// 惯例,免得将来加了 `Area` 才发现要补第二帧。
    fn run_shapes(views: &[TitleView<'_>]) -> Vec<egui::Shape> {
        let ctx = egui::Context::default();
        let t = crate::theme::MULLION_DARK;
        let mut out = Vec::new();
        for _ in 0..2 {
            let full = ctx.run(Default::default(), |ctx| paint(ctx, &t, views));
            out = full.shapes.into_iter().map(|c| c.shape).collect();
        }
        out
    }

    fn strokes_colored(shapes: &[egui::Shape], want: egui::Color32) -> usize {
        shapes
            .iter()
            .filter(|s| {
                matches!(s, egui::Shape::Rect(r)
                    if r.stroke.width > 0.0 && r.stroke.color == want)
            })
            .count()
    }

    /// ④:焦点分屏要有一圈 accent 描边,**标题条被 F83 关掉时也要有** ——
    /// 关掉之后标题条那层 tint 就不存在了,描边是唯一的焦点提示。用户的要求
    /// 是「一眼从众多分屏中找到当前获得焦点的那个」。
    ///
    /// 自证会变红:把 `paint` 里 `if v.focused` 那段删掉;或给它加一条
    /// 「标题条关了就不画」的短路。
    #[test]
    fn the_focused_pane_gets_an_accent_ring_even_without_a_title_bar() {
        let mut tree = Tree::new(PaneId(1));
        tree.split(PaneId(1), Dir::Horizontal, PaneId(2));
        let g = layout_geometry(&tree, AREA, CELL, false, 1.0);
        let views = [view(g[0], 1, true), view(g[1], 2, false)];

        let accent = theme::c32(crate::theme::MULLION_DARK.accent);
        assert_eq!(
            strokes_colored(&run_shapes(&views), accent),
            1,
            "该恰好一圈 accent 描边(焦点那块),不多不少"
        );
    }

    /// 没有焦点(极端情形:焦点 pane 刚被关掉、下一帧才补上)时一圈都不画 ——
    /// 画错位置比不画更糟:用户会以为焦点在别处,对着错的分屏敲键。
    ///
    /// 自证会变红:把 `if v.focused` 改成恒 `true`。
    #[test]
    fn no_ring_is_painted_when_nothing_has_focus() {
        let tree = Tree::new(PaneId(1));
        let g = layout_geometry(&tree, AREA, CELL, false, 1.0);
        let views = [view(g[0], 1, false)];
        let accent = theme::c32(crate::theme::MULLION_DARK.accent);
        assert_eq!(strokes_colored(&run_shapes(&views), accent), 0);
    }

    /// ③ 的绘制腿:分界线真的画出来了,且用的是 `divider` 那一档色
    /// (不是 `stroke` 的白 6%,那个在 1px + 深底下看不见 —— 见
    /// `theme::tests::the_divider_is_visible_but_not_loud_against_the_terminal_background`)。
    ///
    /// 自证会变红:把 `rect_filled` 的颜色改成 `theme::c32(t.term_bg)`;
    /// 或把画分界线那个 `for` 循环删掉。
    #[test]
    fn the_divider_is_actually_filled_with_the_divider_color() {
        let mut tree = Tree::new(PaneId(1));
        tree.split(PaneId(1), Dir::Horizontal, PaneId(2));
        let g = layout_geometry(&tree, AREA, CELL, false, 1.0);
        let views = [view(g[0], 1, false), view(g[1], 2, false)];

        let want = theme::c32(crate::theme::MULLION_DARK.divider);
        let n = run_shapes(&views)
            .iter()
            .filter(|s| matches!(s, egui::Shape::Rect(r) if r.fill == want))
            .count();
        assert_eq!(n, 1, "左右分屏该恰好一条 divider 色的填充(那条竖线)");
    }
```

- [ ] **Step 6: Run test to verify it fails**

```bash
cargo test -p mullion-app --lib ui::pane_edges:: 2>&1 | tail -20
```

Expected: 编译失败 `cannot find function 'paint'`。

- [ ] **Step 7: Implement `paint`**

在 `pane_edges.rs` 的 `divider_lines_of` 之后插入:

```rust
/// 焦点描边的宽度(逻辑点)。1 点 —— 再粗就从「提示」变成「装饰」,
/// 用户明确要的是「不干扰视觉重点」。
const RING_W: f32 = 1.0;

/// 画分界线 + 焦点描边。
///
/// `views` 就是本帧交给 [`crate::ui::pane_title::show`] 的那一份 ——
/// 几何与焦点都从它取,**不新开第二条几何来源**(开了就会有两份真值,
/// 布局一改分界线就跟 pane 错位)。
pub fn paint(ctx: &egui::Context, t: &Theme, views: &[TitleView<'_>]) {
    let ppp = ctx.pixels_per_point();
    // `layer_painter` 而不是 `Area`:见模块文档(T8)。
    let p = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Background,
        egui::Id::new("pane_edges"),
    ));
    // 物理像素 → 逻辑点。**不做 `.max(1.0)` 之类的加粗**:`PxRect` 的坐标
    // 是整数物理像素,除以 ppp 再乘回去正好落在像素边界上,1 物理像素宽的
    // 矩形光栅化出来就是 1 个像素,不糊。
    let to_pt = |r: PxRect| {
        egui::Rect::from_min_size(
            egui::pos2(r.x as f32 / ppp, r.y as f32 / ppp),
            egui::vec2(r.w as f32 / ppp, r.h as f32 / ppp),
        )
    };
    for v in views {
        let (right, bottom) = divider_lines_of(&v.geom);
        for line in [right, bottom].into_iter().flatten() {
            p.rect_filled(to_pt(line), 0.0, theme::c32(t.divider));
        }
        if v.focused {
            // `shrink(RING_W / 2)`:egui 的描边以路径为中心向两侧各铺一半,
            // 不缩的话有半个像素落在 pane 之外、压到邻居身上。
            p.rect_stroke(
                to_pt(v.geom.px).shrink(RING_W / 2.0),
                0.0,
                egui::Stroke::new(RING_W, theme::c32(t.accent)),
            );
        }
    }
}
```

- [ ] **Step 8: Run tests**

```bash
cargo test -p mullion-app --lib ui::pane_edges:: 2>&1 | tail -5
```

Expected: `test result: ok. 6 passed`。

- [ ] **Step 9: Wire it into the frame**

在 `crates/mullion-app/src/ui/mod.rs` 里,把

```rust
    let title_action = pane_title::show(ctx, t, frame.titles);
```

改成

```rust
    // ③④:分界线 + 焦点描边。**在标题条之前画** —— 标题条是 `Area`
    // (`Order::Middle`),分界线是 `Order::Background` 的 layer_painter,
    // 层序由 Order 决定而不是调用顺序,但把「底衬」放前面读起来才顺。
    pane_edges::paint(ctx, t, frame.titles);
    let title_action = pane_title::show(ctx, t, frame.titles);
```

- [ ] **Step 10: 焦点 tint 换掉 `panel_head`**

把 `crates/mullion-app/src/ui/pane_title.rs` 里的

```rust
                ui.painter().rect(
                    full,
                    0.0,
                    theme::c32(if v.focused { t.panel_head } else { t.panel_bg }),
                    theme::stroke(t),
                );
```

改成

```rust
                ui.painter()
                    .rect(full, 0.0, theme::c32(t.panel_bg), theme::stroke(t));
                // ④:焦点分屏的标题条上叠一层薄 accent。底色统一走 `panel_bg`
                // 再叠色,而不是焦点时换成 `panel_head` —— `panel_head` 是
                // 「面板表头」那一档灰,跟 F62 的会话语义色、跟未聚焦态都太近,
                // 分屏一多就分不出来。`gamma_multiply` 把不透明的 accent 变成
                // 半透明,叠出来是一层薄色而不是一块纯 accent 底。
                if v.focused {
                    ui.painter().rect_filled(
                        full,
                        0.0,
                        theme::c32(t.accent).gamma_multiply(FOCUS_TINT),
                    );
                }
```

在同文件 `icon_side` 的上方插入:

```rust
/// ④:焦点分屏标题条上那层 accent 的不透明度。0.14 —— 够看出「这块亮一点」,
/// 又不至于把主机名的对比度压下去。
const FOCUS_TINT: f32 = 0.14;
```

在 `pane_title.rs` 的 `mod tests` 尾部追加:

```rust
    /// ④:焦点分屏的标题条要叠一层半透明 accent。
    ///
    /// 判据取「有一个覆盖整条标题条、且 alpha < 255 的填充」——
    /// 不能只数形状个数(加一个别的装饰就跟着变绿),也不能比精确颜色
    /// (`gamma_multiply` 的结果依赖 egui 内部的 gamma 曲线,钉死就成了
    /// 测 egui 而不是测我们)。
    ///
    /// 自证会变红:把 `if v.focused` 那段 `rect_filled` 删掉;或把
    /// `FOCUS_TINT` 改成 1.0(alpha 变 255,不再是「薄」的一层)。
    #[test]
    fn the_focused_panes_title_bar_is_tinted_with_translucent_accent() {
        let id = PaneId(3);
        let g = geom_800x600_title32(id, 1.0);
        let full = egui::Rect::from_min_size(
            egui::pos2(g.title_px.x as f32, g.title_px.y as f32),
            egui::vec2(g.title_px.w as f32, g.title_px.h as f32),
        );

        let tinted = |focused: bool| {
            let views = [TitleView {
                geom: g,
                index: 1,
                host: Some("h"),
                status: PaneStatus::Live,
                focused,
                appearance: None,
                cwd_leaf: None,
                tmux: None,
            }];
            let ctx = egui::Context::default();
            let t = crate::theme::MULLION_DARK;
            let mut shapes = Vec::new();
            for _ in 0..2 {
                shapes = ctx
                    .run(Default::default(), |ctx| {
                        show(ctx, &t, &views);
                    })
                    .shapes;
            }
            let mut hits = 0;
            for cs in &shapes {
                count_translucent_covers(&cs.shape, full, &mut hits);
            }
            hits
        };

        assert_eq!(tinted(true), 1, "焦点标题条上该有一层半透明 accent");
        assert_eq!(tinted(false), 0, "非焦点标题条不该有任何半透明覆盖");
    }

    fn count_translucent_covers(s: &egui::Shape, full: egui::Rect, hits: &mut usize) {
        match s {
            egui::Shape::Rect(r) => {
                let a = r.fill.a();
                if a > 0 && a < 255 && r.rect.contains_rect(full.shrink(0.5)) {
                    *hits += 1;
                }
            }
            egui::Shape::Vec(v) => {
                for x in v {
                    count_translucent_covers(x, full, hits);
                }
            }
            _ => {}
        }
    }
```

- [ ] **Step 11: Run the full app suite**

```bash
cargo test -p mullion-app --lib 2>&1 | tail -5
```

Expected: `test result: ok.`。若 `pane_title_paints_an_edge_bar_when_apply_to_includes_pane_title` 之类**数形状个数**的既有测试因为多了一个 tint 矩形而红,把它的期望值 +1 并在其文档注释里补一行说明「④ 焦点态多一层 tint」——**只在 `focused: true` 的用例上改**,不要把断言放宽成 `>=`。

- [ ] **Step 12: Commit**

```bash
git add crates/mullion-app/src/ui
git commit -m "feat(app): 分屏分界线 + 焦点描边/标题条 tint (F30/F80)

分界线画进 layout_geometry 已让出的 GAP_PX 缝里,焦点描边画在 pane px
内边界上 —— 都不改几何,所以切焦点不会发 window_change(T4)。
走 ctx.layer_painter 而不是 egui::Area:Area 会占可交互矩形、盖在终端上
吃掉指针事件,划选当场失效(T8)。焦点标题条底色统一 panel_bg 再叠
14% accent,不再换成 panel_head(那一档跟未聚焦态和 F62 语义色都太近)。

守护测试:ui::pane_edges::tests(6 条)
          ui::pane_title::tests::the_focused_panes_title_bar_is_tinted_with_translucent_accent"
```

---

## Task 6: ⑥ 远端状态解析(纯函数)

**为什么只能从字节流拿**:adr-009 一条 SSH 连接承载所有分屏,旁路 exec 通道的 `$SSH_CONNECTION` 四元组完全相同,分不出是哪块分屏;`channel.set_env` 又受 sshd `AcceptEnv` 限制(默认只放 `LANG`/`LC_*`)。所以只剩 OSC。

**Files:**
- Create: `crates/mullion-term/src/remote_state.rs`
- Modify: `crates/mullion-term/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

新建 `crates/mullion-term/src/remote_state.rs`:

```rust
//! ⑥:从 pane 自己的字节流里认出「当前目录」和「tmux 会话名」。
//!
//! 为什么只能从字节流拿:adr-009 一条 SSH 连接承载所有分屏,旁路 exec 通道的
//! `$SSH_CONNECTION` 四元组完全相同,分不出是哪块分屏;`channel.set_env` 又受
//! sshd `AcceptEnv` 限制(默认只放 `LANG`/`LC_*`)。所以只剩 OSC。
//!
//! 两条腿:
//! - **OSC 7**(`ESC ] 7 ; file://host/path BEL|ST`):现代 shell 上报 cwd 的
//!   标准做法。alacritty 0.26 **不解析**它(`ansi::Handler` 里没有
//!   `set_current_directory`),所以我们自己在 [`crate::emulator::Emulator::feed`]
//!   里扫一遍。
//! - **OSC 0/2**(窗口标题):alacritty 已经解析成 `Event::Title`。tmux 开了
//!   `set-titles on` 之后会按 `#S:#I:#W` 发,会话名就在第一段。
//!
//! **cwd 以 OSC 7 为准**:它是路径本身;标题里那个是给人看的(带 `~` 缩写、
//! 可能被 shell 截断)。远端要怎么配见 `docs/remote-state-setup.md`。

/// 一次采集到的远端状态。**拿不到就是 `None`,不猜、不填占位** ——
/// ② 会拿 `cwd` 去当 SFTP 起始目录,猜错比不显示危险得多。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteState {
    /// 当前目录。**字节**语义:远端文件名不保证是 UTF-8,而这个值最终要拼成
    /// SFTP 路径(`mullion-ssh` 的 `RemotePath` 也是字节)。这里不引
    /// `RemotePath` —— 依赖方向是 `app → {core, term, ssh, store}`,
    /// term 不认识 ssh。
    pub cwd: Option<Vec<u8>>,
    /// tmux 会话名。
    pub tmux: Option<String>,
    /// 这一批里收到过新标题没有。
    ///
    /// 收到了的话 `tmux` 就是**权威值,包括「没有」**:用户退出 tmux 之后
    /// bash 会发自己的标题,这时必须把会话名清掉,否则标题条上会永久挂着一个
    /// 已经不存在的 tmux 名。`cwd` 不吃这一套 —— 它只增不清,拿不到新值时
    /// 保留上一个已知值比闪成「未知」有用。
    pub title_seen: bool,
}

/// 一条未完成的 OSC 最多攒多少字节。超了就丢弃当前这条并回到「找 `ESC ]`」
/// 状态 —— 没有上限的话,一个畸形的流(有 `ESC ] 7 ;` 却永远不发终止符)
/// 会让我们无界增长。
pub const MAX_PENDING: usize = 4096;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc7_yields_the_path_and_ignores_the_hostname() {
        assert_eq!(
            parse_osc7(b"file://build-01/home/dev/Mullion").as_deref(),
            Some(&b"/home/dev/Mullion"[..])
        );
        // 主机名段空着也合法(shell 拿不到 hostname 时会这么发)。
        assert_eq!(parse_osc7(b"file:///tmp").as_deref(), Some(&b"/tmp"[..]));
    }

    /// 百分号转义按**字节**解码。先转成 `String` 再解码会在非 UTF-8 路径上
    /// 炸掉(而路径里的非 ASCII 恰恰就是以 `%XX` 编码进来的)。
    #[test]
    fn osc7_percent_escapes_are_decoded_as_bytes() {
        assert_eq!(
            parse_osc7(b"file://h/tmp/a%20b").as_deref(),
            Some(&b"/tmp/a b"[..])
        );
        assert_eq!(
            parse_osc7(b"file://h/%E4%B8%AD").as_deref(),
            Some("/中".as_bytes())
        );
        // 残缺的转义原样留着,不吞字节 —— `%` 是合法文件名字符。
        assert_eq!(parse_osc7(b"file://h/a%").as_deref(), Some(&b"/a%"[..]));
        assert_eq!(parse_osc7(b"file://h/a%zz").as_deref(), Some(&b"/a%zz"[..]));
    }

    /// 认不出来就 `None`。**宁可不显示,也不要显示一个错的目录** ——
    /// ② 会拿它当 SFTP 起始目录。
    ///
    /// 自证会变红:把 `strip_prefix(b"file://")?` 换成
    /// `strip_prefix(b"file://").unwrap_or(payload)`。
    #[test]
    fn a_malformed_osc7_is_rejected_rather_than_guessed() {
        assert_eq!(parse_osc7(b""), None);
        assert_eq!(parse_osc7(b"file:/tmp"), None);
        assert_eq!(parse_osc7(b"/tmp"), None);
        assert_eq!(parse_osc7(b"file://hostname-with-no-path"), None);
    }

    /// tmux 的默认 `set-titles-string` 是 `#S:#I:#W`。
    #[test]
    fn a_tmux_default_title_gives_up_the_session_name() {
        let st = parse_title("main:0:bash");
        assert_eq!(st.tmux.as_deref(), Some("main"));
        assert_eq!(st.cwd, None, "默认串里没有路径,不该凭空造一个");
    }

    /// **第二段必须是纯数字**,否则 Ubuntu 默认 bash 的 `user@host: ~/dir`
    /// 会被当成 tmux 会话名 `user@host` —— 标题条上永久挂一个不存在的会话名,
    /// 而用户根本没开 tmux。
    ///
    /// 自证会变红:把 `parse_title` 里 `is_ascii_digit()` 那个条件删掉。
    #[test]
    fn a_plain_bash_title_is_not_mistaken_for_a_tmux_session() {
        let st = parse_title("dev@build-01: ~/Mullion");
        assert_eq!(st.tmux, None);
        assert_eq!(st.cwd.as_deref(), Some("~/Mullion".as_bytes()));
    }

    #[test]
    fn a_title_carrying_an_absolute_path_gives_up_the_cwd() {
        let st = parse_title("main:0:bash /home/dev/Mullion");
        assert_eq!(st.tmux.as_deref(), Some("main"));
        assert_eq!(st.cwd.as_deref(), Some(&b"/home/dev/Mullion"[..]));
    }

    #[test]
    fn a_title_with_neither_gives_up_nothing() {
        assert_eq!(parse_title("bash"), RemoteState::default());
        assert_eq!(parse_title(""), RemoteState::default());
    }

    /// **本项目的主场景是高延迟链路**,一条 OSC 被 TCP 切在任意字节位置是常态
    /// (Nagle + 延迟 ACK)。切在哪里都必须认得出来 —— 认不出的现象是目录名
    /// 时有时无地闪,而且只在慢链路上出现,本地怎么试都试不出来。
    ///
    /// 自证会变红:把 `Osc7Sniffer::feed` 改成不留 `pending`(每次调用从
    /// 头找 `ESC ]`),`cut` 落在序列中间的那些用例立刻红。
    #[test]
    fn an_osc7_split_at_any_byte_boundary_is_still_recognised() {
        let seq = b"\x1b]7;file://host/home/dev/Mullion\x07";
        for cut in 0..=seq.len() {
            let mut s = Osc7Sniffer::default();
            let a = s.feed(&seq[..cut]);
            let b = s.feed(&seq[cut..]);
            assert_eq!(
                a.or(b).as_deref(),
                Some(&b"/home/dev/Mullion"[..]),
                "切在第 {cut} 字节就认不出来了"
            );
        }
    }

    /// ST(`ESC \`)也是合法终止符,而且它本身跨得过 `feed` 边界。
    #[test]
    fn st_terminated_osc7_works_including_across_a_feed_boundary() {
        let mut s = Osc7Sniffer::default();
        assert_eq!(
            s.feed(b"\x1b]7;file://h/tmp\x1b\\").as_deref(),
            Some(&b"/tmp"[..])
        );

        let mut s = Osc7Sniffer::default();
        assert_eq!(s.feed(b"\x1b]7;file://h/tmp\x1b"), None);
        assert_eq!(s.feed(b"\\").as_deref(), Some(&b"/tmp"[..]));
    }

    /// 标题那条 OSC(`0;` / `2;`)不能被当成 cwd —— 它由 alacritty 那条腿
    /// 处理,我们只是路过。
    ///
    /// 自证会变红:把 `feed` 里的 `strip_prefix(b"7;")` 去掉。
    #[test]
    fn a_title_osc_is_not_mistaken_for_a_cwd() {
        let mut s = Osc7Sniffer::default();
        assert_eq!(s.feed(b"\x1b]0;dev@h: ~/x\x07"), None);
        assert_eq!(s.feed(b"\x1b]2;file://h/tmp\x07"), None);
    }

    /// 畸形流(开了头永不终止)不能把内存吃光,而且**丢掉之后要能恢复**——
    /// 后面那条正常的 OSC 7 仍须认出来。只测「没 OOM」是不够的:把
    /// `pending = None` 写成 `pending = Some(Vec::new())` 会永远卡在
    /// OSC 状态里,内存不涨但从此再也认不出任何 cwd。
    ///
    /// 自证会变红:删掉 `feed` 里的 `> MAX_PENDING` 那一段(第一条断言的
    /// 内存不涨没法直接测,但第二条会因为 4096 之后的字节继续攒着、
    /// 后面那条 OSC 被当成前一条的载荷而红)。
    #[test]
    fn an_unterminated_osc_is_dropped_and_the_sniffer_recovers() {
        let mut s = Osc7Sniffer::default();
        assert_eq!(s.feed(b"\x1b]7;file://h/"), None);
        assert_eq!(s.feed(&vec![b'x'; MAX_PENDING + 16]), None);
        assert_eq!(
            s.feed(b"\x1b]7;file://h/tmp\x07").as_deref(),
            Some(&b"/tmp"[..]),
            "丢掉畸形那条之后应该能重新认出正常的 OSC 7"
        );
    }

    /// 一次 `feed` 里有多条时取**最后一条** —— cwd 是「当前值」,不是流水。
    #[test]
    fn the_last_osc7_in_a_batch_wins() {
        let mut s = Osc7Sniffer::default();
        assert_eq!(
            s.feed(b"\x1b]7;file://h/a\x07\x1b]7;file://h/b\x07")
                .as_deref(),
            Some(&b"/b"[..])
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

在 `crates/mullion-term/src/lib.rs` 的 `pub mod palette;` 之后加一行:

```rust
pub mod remote_state;
```

```bash
cargo test -p mullion-term remote_state 2>&1 | tail -20
```

Expected: 编译失败 `cannot find function 'parse_osc7'` / `cannot find type 'Osc7Sniffer'`。

- [ ] **Step 3: Implement**

在 `crates/mullion-term/src/remote_state.rs` 的 `MAX_PENDING` 之后、`mod tests` 之前插入:

```rust
/// 解析 OSC 7 的载荷(`7;` 之后那一段)。
///
/// 形如 `file://hostname/path`;主机名段**忽略** —— 远端自己报的名字对我们
/// 没用,而且在 tmux/容器里经常是错的。
///
/// 不是 `file://` 开头、或者主机名段之后没有 `/` 的,一律 `None`。
pub fn parse_osc7(payload: &[u8]) -> Option<Vec<u8>> {
    let rest = payload.strip_prefix(b"file://")?;
    let slash = rest.iter().position(|&b| b == b'/')?;
    Some(percent_decode(&rest[slash..]))
}

/// 按**字节**解 `%XX`。残缺的转义原样留着 —— `%` 是合法文件名字符,
/// 吞掉它会把路径改成一个不存在的目录。
fn percent_decode(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%' && i + 2 < s.len() {
            if let (Some(h), Some(l)) = (hex(s[i + 1]), hex(s[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// 从窗口标题里认 tmux 会话名和 cwd。
///
/// 规则:
/// 1. **tmux 会话名**:标题形如 `<非空白>:<纯数字>:…` 就取第一段。tmux 的
///    默认 `set-titles-string` 是 `#S:#I:#W`(如 `main:0:bash`)。第二段
///    必须是纯数字 —— 否则 Ubuntu 默认 bash 的 `user@host: ~/dir` 会被当成
///    会话名 `user@host`,用户没开 tmux 却看到一个会话名。
/// 2. **cwd**:第一个以 `/`、`~/` 开头或恰好是 `~` 的空白分隔 token。
///    `~` **不展开** —— 展开需要知道远端的 `$HOME`,而我们不知道;调用方
///    (标题条)只拿它取最后一级目录名,② 那边则明确只接受绝对路径。
pub fn parse_title(title: &str) -> RemoteState {
    let mut out = RemoteState {
        title_seen: true,
        ..RemoteState::default()
    };
    if let Some((name, rest)) = title.split_once(':') {
        let second_is_index = rest
            .split_once(':')
            .is_some_and(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        if !name.is_empty() && !name.contains(char::is_whitespace) && second_is_index {
            out.tmux = Some(name.to_string());
        }
    }
    out.cwd = title
        .split_whitespace()
        .find(|tok| tok.starts_with('/') || tok.starts_with("~/") || *tok == "~")
        .map(|tok| tok.as_bytes().to_vec());
    out
}

/// OSC 7 嗅探器。**有状态**:一条 OSC 可能被 TCP 切在任意字节位置(高延迟
/// 链路上是常态,正是本项目的主场景),所以未完成的前缀要留着。
#[derive(Debug, Default)]
pub struct Osc7Sniffer {
    /// `None` = 不在一条 OSC 里面。`Some(buf)` = 正在攒载荷(不含 `ESC ]`)。
    pending: Option<Vec<u8>>,
    /// 上一个字节是不是 `ESC`。认 `ESC ]`(开头)和 `ESC \`(ST 结尾)都要
    /// 它 —— 这两个二字节序列本身就可能跨 `feed` 被切开。
    saw_esc: bool,
}

impl Osc7Sniffer {
    /// 喂一段字节,返回这一段里**最后一条**完整 OSC 7 给出的路径。
    ///
    /// 只认 `7;` 开头的;其余(标题的 `0;`/`2;`、调色板的 `4;`……)攒到终止符
    /// 就丢 —— 它们由 alacritty 那条腿处理,我们只是路过。
    pub fn feed(&mut self, bytes: &[u8]) -> Option<Vec<u8>> {
        let mut found = None;
        for &b in bytes {
            match &mut self.pending {
                None => {
                    if self.saw_esc && b == b']' {
                        self.pending = Some(Vec::new());
                    }
                    self.saw_esc = b == 0x1b;
                }
                Some(buf) => {
                    if b == 0x07 || (self.saw_esc && b == b'\\') {
                        if self.saw_esc {
                            buf.pop(); // 把上一轮攒进去的 ESC 拿掉
                        }
                        if let Some(p) = buf.strip_prefix(b"7;").and_then(parse_osc7) {
                            found = Some(p);
                        }
                        self.pending = None;
                        self.saw_esc = false;
                        continue;
                    }
                    buf.push(b);
                    self.saw_esc = b == 0x1b;
                    if buf.len() > MAX_PENDING {
                        self.pending = None;
                        self.saw_esc = false;
                    }
                }
            }
        }
        found
    }
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p mullion-term remote_state 2>&1 | tail -5
```

Expected: `test result: ok. 11 passed`。

- [ ] **Step 5: Commit**

```bash
git add crates/mullion-term/src/remote_state.rs crates/mullion-term/src/lib.rs
git commit -m "feat(term): OSC 7 / 窗口标题解析出远端 cwd 与 tmux 会话名 (F30)

纯函数层,零 IO。alacritty 0.26 不解析 OSC 7(Handler 里没有
set_current_directory),自己嗅探;标题走 alacritty 已有的 Event::Title。
嗅探器有状态:高延迟链路上一条 OSC 被 TCP 切在任意字节位置是常态。
tmux 名要求第二段是纯数字,否则 bash 的 'user@host: ~/dir' 会被误认。

守护测试:remote_state::tests(11 条,含 an_osc7_split_at_any_byte_boundary_is_still_recognised)"
```

---

## Task 7: ⑥ Emulator 接线

**Files:**
- Modify: `crates/mullion-term/src/emulator.rs`
- Test: `crates/mullion-term/src/emulator.rs`(`mod tests`)

- [ ] **Step 1: Write the failing test**

在 `crates/mullion-term/src/emulator.rs` 的 `mod tests` 尾部追加:

```rust
    /// ⑥:两条腿都要接上 —— OSC 7 给 cwd,OSC 2 给标题(tmux 名)。
    ///
    /// **T1 不能被这次改动碰坏**:`EventSink` 现在同时收 `PtyWrite` 和
    /// `Title`,`match` 写歪一个分支就会把回写字节吞掉(现象见 T1:同步输出
    /// 探测无应答、全屏 TUI 闪、鼠标全废)。所以这条测试同时验一遍回写。
    ///
    /// 自证会变红:把 `feed` 里的 `osc7.feed(bytes)` 那段删掉(cwd 断言红);
    /// 把 `EventSink` 的 `Event::Title` 分支删掉(tmux 断言红)。
    #[test]
    fn osc_reports_land_in_the_remote_state() {
        let mut emu = Emulator::new(80, 24);
        assert_eq!(emu.take_remote_state(), None, "什么都没收到时不该有状态");

        emu.feed(b"\x1b]7;file://h/home/dev/Mullion\x07");
        emu.feed(b"\x1b]2;main:0:bash\x07");
        let st = emu.take_remote_state().expect("该收到远端状态");
        assert_eq!(st.cwd.as_deref(), Some(&b"/home/dev/Mullion"[..]));
        assert_eq!(st.tmux.as_deref(), Some("main"));
        assert!(st.title_seen);

        assert_eq!(emu.take_remote_state(), None, "take 之后应清空");
    }

    /// **OSC 7 压过标题里的路径**:OSC 7 是路径本身,标题里那个是给人看的
    /// (带 `~` 缩写、可能被 shell 截断)。反过来的话 ② 会拿一个 `~/x` 去
    /// 当 SFTP 起始目录,而 sftp-server 不展开 `~`。
    ///
    /// 自证会变红:把 `take_remote_state` 里的覆盖顺序倒过来。
    #[test]
    fn osc7_wins_over_the_path_inside_the_title() {
        let mut emu = Emulator::new(80, 24);
        emu.feed(b"\x1b]2;dev@h: ~/Mullion\x07");
        emu.feed(b"\x1b]7;file://h/home/dev/Mullion\x07");
        let st = emu.take_remote_state().expect("该收到远端状态");
        assert_eq!(st.cwd.as_deref(), Some(&b"/home/dev/Mullion"[..]));
    }

    /// 只来了 OSC 7、没来标题时 `title_seen == false` —— 调用方靠它决定
    /// 「要不要按这批数据重置 tmux 名」。恒 `true` 的话每次目录变化都会把
    /// tmux 会话名清掉(用户在 tmux 里 `cd` 一下,会话名就消失了)。
    ///
    /// 自证会变红:把 `take_remote_state` 里的 `title_seen` 写成恒 `true`。
    #[test]
    fn a_cwd_only_batch_does_not_claim_a_title_was_seen() {
        let mut emu = Emulator::new(80, 24);
        emu.feed(b"\x1b]7;file://h/tmp\x07");
        let st = emu.take_remote_state().expect("该收到远端状态");
        assert!(!st.title_seen, "没收到标题却说收到了");
        assert_eq!(st.tmux, None);
    }
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p mullion-term emulator::tests::osc 2>&1 | tail -20
```

Expected: 编译失败 `no method named 'take_remote_state'`。

- [ ] **Step 3: Implement**

在 `crates/mullion-term/src/emulator.rs`:

(a) 顶部 `use` 区加:

```rust
use crate::remote_state::{Osc7Sniffer, RemoteState};
```

(b) 把 `PtyWriteCollector` 改名为 `EventSink` 并加标题槽位(它现在收两种事件,名字得跟上):

```rust
/// 收集 `Term` 发出的事件。共享缓冲,`Term` 持一份克隆。
///
/// 两件事:
/// - `Event::PtyWrite` —— 需要回写对端的字节(**T1 红线**,漏了就是同步输出
///   探测无应答、全屏 TUI 闪、鼠标全废)。
/// - `Event::Title` —— OSC 0/2 的窗口标题(⑥ 认 tmux 会话名那条腿)。
#[derive(Clone, Default)]
struct EventSink {
    buf: Arc<Mutex<Vec<u8>>>,
    /// **只留最后一条**:标题是「当前值」,不是流水。
    title: Arc<Mutex<Option<String>>>,
}

impl EventListener for EventSink {
    fn send_event(&self, event: Event) {
        match event {
            Event::PtyWrite(text) => self
                .buf
                .lock()
                .expect("pty-write buffer poisoned")
                .extend_from_slice(text.as_bytes()),
            Event::Title(t) => *self.title.lock().expect("title slot poisoned") = Some(t),
            // 其余事件(响铃、剪贴板……)本项目还不用。
            _ => {}
        }
    }
}
```

(c) `struct Emulator` 里的 `term: Term<PtyWriteCollector>` → `Term<EventSink>`,`collector: PtyWriteCollector` → `sink: EventSink`,并追加两个字段:

```rust
    /// ⑥:OSC 7 嗅探器。alacritty 不解析 OSC 7,这条腿是我们自己的。
    osc7: Osc7Sniffer,
    /// ⑥:嗅探到的最新 cwd,等 `take_remote_state` 取走。
    cwd: Option<Vec<u8>>,
```

(d) `with_history` 里 `let collector = PtyWriteCollector::default();` → `let sink = EventSink::default();`,`Term::new(config, &dims, collector.clone())` → `sink.clone()`,构造体里 `collector` → `sink,` 并补 `osc7: Osc7Sniffer::default(), cwd: None,`。

(e) `take_pty_writes` 里 `self.collector.buf` → `self.sink.buf`。

(f) `feed` 改成:

```rust
    /// 喂入一段来自对端的字节,推进 VT 状态机(不节流,VT 状态机很快)。
    pub fn feed(&mut self, bytes: &[u8]) {
        // ⑥:OSC 7 alacritty 不解析,我们自己扫一遍。两条腿互不影响,
        // 放在 `advance` 之前只是让「先看一眼再交出去」读起来顺。
        if let Some(cwd) = self.osc7.feed(bytes) {
            self.cwd = Some(cwd);
        }
        self.parser.advance(&mut self.term, bytes);
    }
```

(g) 在 `take_pty_writes` 之后插入:

```rust
    /// ⑥:自上次调用以来远端报出的状态。`None` = 什么新东西都没有。
    ///
    /// **cwd 以 OSC 7 为准**,标题里的路径只在没有 OSC 7 时兜底:OSC 7 是
    /// 路径本身,标题里那个是给人看的(带 `~` 缩写、可能被 shell 截断)。
    ///
    /// 「取走」语义(拿完清空)而不是「读」:调用方是每帧跑一次的
    /// `Workspace::pump`,留着的话每帧都要把同一个值再算一遍(T3 那一类
    /// 白烧 CPU)。
    pub fn take_remote_state(&mut self) -> Option<RemoteState> {
        let title = self.sink.title.lock().expect("title slot poisoned").take();
        let cwd = self.cwd.take();
        if title.is_none() && cwd.is_none() {
            return None;
        }
        let mut out = title
            .as_deref()
            .map(crate::remote_state::parse_title)
            .unwrap_or_default();
        if cwd.is_some() {
            out.cwd = cwd;
        }
        Some(out)
    }
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p mullion-term 2>&1 | tail -5
cargo test -p mullion-term emulator::tests::pty_write_is_collected -- --exact 2>&1 | tail -5
```

Expected: 两条都 `test result: ok.` —— **第二条是 T1 的守护测试,必须专门确认它绿**。

- [ ] **Step 5: Commit**

```bash
git add crates/mullion-term/src/emulator.rs
git commit -m "feat(term): Emulator 收集远端 cwd/tmux 名 (F30)

PtyWriteCollector 改名 EventSink(现在同时收 PtyWrite 和 Title),
feed 里顺路跑 Osc7Sniffer,take_remote_state 取走一次采集结果。
cwd 以 OSC 7 为准,标题里的路径只兜底。

T1:EventSink 的 match 同时管着回写字节,跑了
emulator::tests::pty_write_is_collected 确认没被碰坏。
守护测试:emulator::tests::osc_reports_land_in_the_remote_state
          emulator::tests::osc7_wins_over_the_path_inside_the_title
          emulator::tests::a_cwd_only_batch_does_not_claim_a_title_was_seen"
```

---

## Task 8: ⑥ PaneState 落地 + 标题条右区

**Files:**
- Modify: `crates/mullion-app/src/shell/workspace/mod.rs`、`crates/mullion-app/src/ui/pane_title.rs`、`crates/mullion-app/src/app.rs`
- Test: 同上三处

### 8a: `PaneState.cwd` / `.tmux`

- [ ] **Step 1: Write the failing test**

在 `crates/mullion-app/src/shell/workspace/mod.rs` 的 `mod tests` 尾部追加:

```rust
    /// ⑥:远端报出来的 cwd / tmux 名要落到 `PaneState` 上。
    ///
    /// **两个字段的更新策略不同**,一条测试同时钉住:
    /// - `cwd` 只增不清:拿不到新值时保留上一个已知值(比闪成「未知」有用)。
    /// - `tmux` 收到新标题就整体重置,**包括重置成 `None`** —— 用户退出 tmux
    ///   之后 bash 会发自己的标题,这时必须把会话名清掉,否则标题条上会永久
    ///   挂着一个已经不存在的会话名。
    ///
    /// 自证会变红:把 `pump` 里 `if st.title_seen { p.tmux = st.tmux }` 改成
    /// `if let Some(t) = st.tmux { p.tmux = Some(t) }`(第三段红);把
    /// `if let Some(cwd) = st.cwd` 改成无条件 `p.cwd = st.cwd`(第二段红)。
    #[test]
    fn remote_state_lands_on_the_pane_with_the_right_reset_policy() {
        let mut ws = Workspace::new(PaneId(1));
        ws.attach_pane(test_pane(1));
        let id = PaneId(1);

        // 1. tmux 里,报了目录
        ws.pane_mut(id).unwrap().emulator.feed(b"\x1b]2;work:0:bash\x07");
        ws.pane_mut(id)
            .unwrap()
            .emulator
            .feed(b"\x1b]7;file://h/home/dev/Mullion\x07");
        ws.pump(0);
        assert_eq!(
            ws.pane(id).unwrap().cwd.as_deref(),
            Some(&b"/home/dev/Mullion"[..])
        );
        assert_eq!(ws.pane(id).unwrap().tmux.as_deref(), Some("work"));

        // 2. 只报了新目录 —— tmux 名不该被这一批清掉
        ws.pane_mut(id).unwrap().emulator.feed(b"\x1b]7;file://h/tmp\x07");
        ws.pump(0);
        assert_eq!(ws.pane(id).unwrap().cwd.as_deref(), Some(&b"/tmp"[..]));
        assert_eq!(
            ws.pane(id).unwrap().tmux.as_deref(),
            Some("work"),
            "只报目录不该把 tmux 名清掉"
        );

        // 3. 退出 tmux,bash 发自己的标题 —— 会话名必须清掉,cwd 留着
        ws.pane_mut(id)
            .unwrap()
            .emulator
            .feed(b"\x1b]2;dev@h: /tmp\x07");
        ws.pump(0);
        assert_eq!(
            ws.pane(id).unwrap().tmux,
            None,
            "退出 tmux 之后会话名还挂着"
        );
        assert_eq!(
            ws.pane(id).unwrap().cwd.as_deref(),
            Some(&b"/tmp"[..]),
            "cwd 不该被标题批次清掉"
        );
    }
```

> `Workspace::new` / `attach_pane` / `test_pane` / `pump` 的具体写法照抄同文件既有测试(例如现有的 pump 相关用例);若同文件没有 `test_pane` 辅助函数,照 `app.rs:8500` 的 `fn test_pane(id: u32)` 复制一份到 `workspace/mod.rs` 的 `mod tests` 里。

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p mullion-app --lib shell::workspace::tests::remote_state_lands 2>&1 | tail -20
```

Expected: 编译失败 `no field 'cwd' on type '&PaneState'`。

- [ ] **Step 3: Implement**

在 `crates/mullion-app/src/shell/workspace/mod.rs` 的 `pub struct PaneState` 里追加:

```rust
    /// ⑥:远端报出来的当前目录(字节,见 `RemoteState::cwd`)。
    /// `None` = 远端没报过(裸 shell 不设标题、或 tmux 没开 `set-titles`)。
    /// **只增不清**:拿不到新值时保留上一个已知值,比闪成「未知」有用。
    pub cwd: Option<Vec<u8>>,
    /// ⑥:tmux 会话名。`None` = 不在 tmux 里(或远端没开 `set-titles`)。
    /// 与 `cwd` 不同,**收到新标题就整体重置** —— 用户退出 tmux 之后 bash 会
    /// 发自己的标题,这时必须把会话名清掉,否则标题条上会永久挂着一个已经
    /// 不存在的会话名。
    pub tmux: Option<String>,
```

`mod.rs:437` 那处构造 `PaneState { … }` 补 `cwd: None, tmux: None,`。

在 `pump` 里 `session_pump::pump(&mut p.emulator, &inbound)` **之后**插入:

```rust
            // ⑥:远端状态。放在喂完字节之后 —— OSC 就在这一批字节里。
            // 两个字段的重置策略不同,见 `PaneState::cwd` / `::tmux` 的文档。
            if let Some(st) = p.emulator.take_remote_state() {
                if let Some(cwd) = st.cwd {
                    p.cwd = Some(cwd);
                }
                if st.title_seen {
                    p.tmux = st.tmux;
                }
            }
```

补齐其余构造点:

```bash
cd /data/Mullion
grep -rn "PaneState {" crates/mullion-app/src/app.rs crates/mullion-app/src/shell
```

`app.rs:4858`、`:5001`、`:8502`、`:10490`、`:10573` 各补 `cwd: None, tmux: None,`。

- [ ] **Step 4: Run tests**

```bash
cargo test -p mullion-app --lib shell::workspace:: 2>&1 | tail -5
```

Expected: `test result: ok.`。

### 8b: 标题条右区

- [ ] **Step 5: Write the failing test**

在 `crates/mullion-app/src/ui/pane_title.rs` 的 `mod tests` 尾部追加:

```rust
    /// ⑥:目录名取最后一级。
    #[test]
    fn dir_leaf_takes_the_last_component() {
        assert_eq!(dir_leaf(b"/home/dev/Mullion").as_deref(), Some("Mullion"));
        assert_eq!(dir_leaf(b"/home/dev/Mullion/").as_deref(), Some("Mullion"));
        assert_eq!(dir_leaf(b"/").as_deref(), Some("/"), "根目录自己就是最后一级");
        assert_eq!(dir_leaf(b"~").as_deref(), Some("~"));
        assert_eq!(dir_leaf(b"~/Mullion").as_deref(), Some("Mullion"));
        assert_eq!(dir_leaf(b""), None, "空路径没有最后一级,别显示一个空标签");
    }

    /// 非 UTF-8 的远端路径不能让标题条整段消失 —— 显示层宁可出一个 `�`
    /// 也要把目录名摆出来(用户至少知道自己在哪个目录)。
    ///
    /// 自证会变红:把 `dir_leaf` 里的 `to_string_lossy` 换成
    /// `std::str::from_utf8(..).ok()?`。
    #[test]
    fn a_non_utf8_dir_name_is_shown_lossily_rather_than_dropped() {
        let leaf = dir_leaf(b"/tmp/\xff\xfe").expect("非 UTF-8 也要给出个东西来");
        assert!(!leaf.is_empty());
    }

    /// ⑥:右区文字。两者都没有时**整段不画** —— 不留一个孤零零的分隔符。
    ///
    /// 自证会变红:把 `side_text` 的 `(None, None)` 分支改成
    /// `Some(String::new())`。
    #[test]
    fn side_text_shows_what_it_has_and_nothing_when_it_has_neither() {
        assert_eq!(side_text(Some("Mullion"), Some("work")).as_deref(), Some("work · Mullion"));
        assert_eq!(side_text(Some("Mullion"), None).as_deref(), Some("Mullion"));
        assert_eq!(side_text(None, Some("work")).as_deref(), Some("work"));
        assert_eq!(side_text(None, None), None);
    }

    /// **右区不许把 `Area` 撑出 `title_px`**(本文件顶部越界坑第 1 条):
    /// 撑出去就会横向侵入右边邻居、纵向盖住终端第一行,吃掉本该属于终端的
    /// 指针事件(T8 变体)。长目录名 + 长 tmux 名 + 高 DPI 一起上。
    ///
    /// 自证会变红:把右区那段的 `allocate_exact_size` 换成
    /// `ui.add(egui::Label::new(s))`(不截断,`horizontal` 布局被撑宽)。
    #[test]
    fn a_long_cwd_and_tmux_name_do_not_push_the_area_past_title_px() {
        for ppp in [1.0_f32, 1.5, 2.0] {
            let id = PaneId(9);
            let g = geom_800x600_title32(id, ppp);
            let views = [TitleView {
                geom: g,
                index: 1,
                host: Some("dev@a-very-long-hostname-that-eats-the-row"),
                status: PaneStatus::Live,
                focused: true,
                appearance: None,
                cwd_leaf: Some("a-directory-name-that-is-also-absurdly-long".to_string()),
                tmux: Some("a-tmux-session-name-that-is-long-too"),
            }];

            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(ppp);
            let t = crate::theme::MULLION_DARK;
            for _ in 0..2 {
                let _ = ctx.run(Default::default(), |ctx| {
                    show(ctx, &t, &views);
                });
            }
            let rect = ctx
                .memory(|m| m.area_rect(area_id(id)))
                .expect("标题条的 Area 该存在");
            let want_w = g.title_px.w as f32 / ppp;
            let want_h = g.title_px.h as f32 / ppp;
            assert!(
                (rect.width() - want_w).abs() < 0.5 && (rect.height() - want_h).abs() < 0.5,
                "ppp={ppp}: Area 是 {:?},该是 {want_w}×{want_h}",
                rect.size()
            );
        }
    }

    /// 左右分区各自截断:**长主机名不该把目录名挤掉**。分屏一多的时候,
    /// 主机名往往全都一样,目录名恰恰是唯一能区分的那一项。
    ///
    /// 判据是两段文字都出现在形状里(egui 的 `Shape::Text` 带 galley,
    /// galley 里的 `text()` 是截断后的实际内容)。
    ///
    /// 自证会变红:把右区那段挪进左边那个 `left_to_right` 布局里(主机名
    /// 先占满,目录名被挤成 0 宽)。
    #[test]
    fn a_long_host_name_does_not_squeeze_out_the_directory_name() {
        let id = PaneId(11);
        let views = [TitleView {
            geom: geom_800x600_title32(id, 1.0),
            index: 1,
            host: Some("dev@a-very-long-hostname-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            status: PaneStatus::Live,
            focused: true,
            appearance: None,
            cwd_leaf: Some("Mullion".to_string()),
            tmux: Some("work"),
        }];

        let ctx = egui::Context::default();
        let t = crate::theme::MULLION_DARK;
        let mut texts = Vec::new();
        for _ in 0..2 {
            let out = ctx.run(Default::default(), |ctx| {
                show(ctx, &t, &views);
            });
            texts.clear();
            for cs in &out.shapes {
                collect_texts(&cs.shape, &mut texts);
            }
        }
        let joined = texts.join(" | ");
        assert!(
            joined.contains("Mullion"),
            "目录名被长主机名挤掉了,画出来的文字是:{joined}"
        );
        assert!(
            joined.contains("work"),
            "tmux 名没画出来,画出来的文字是:{joined}"
        );
        assert!(
            joined.contains("dev@"),
            "主机名整段没了 —— 右区抢了全部宽度,画出来的文字是:{joined}"
        );
    }

    fn collect_texts(s: &egui::Shape, out: &mut Vec<String>) {
        match s {
            egui::Shape::Text(t) => out.push(t.galley.text().to_string()),
            egui::Shape::Vec(v) => {
                for x in v {
                    collect_texts(x, out);
                }
            }
            _ => {}
        }
    }
```

- [ ] **Step 6: Run test to verify it fails**

```bash
cargo test -p mullion-app --lib ui::pane_title:: 2>&1 | tail -20
```

Expected: 编译失败 `cannot find function 'dir_leaf'` / `'side_text'`。

- [ ] **Step 7: Implement**

在 `crates/mullion-app/src/ui/pane_title.rs` 的 `TitleView` 里追加(Task 3 已加过就跳过,只补文档):

```rust
    /// ⑥:远端当前目录的最后一级(已由 [`dir_leaf`] 取好)。`None` = 不知道。
    ///
    /// **拿 `String` 而不是 `&str`**:源头 `PaneState.cwd` 是字节,取最后一级
    /// 要经过 `to_string_lossy`,借不出一个活到本帧结束的 `&str`。每 pane
    /// 每帧一次短字符串分配,相对每帧几千个 glyph 可以忽略。
    pub cwd_leaf: Option<String>,
    /// ⑥:tmux 会话名。`None` = 不在 tmux 里 / 远端没开 `set-titles`。
    pub tmux: Option<&'a str>,
```

在 `icon_side` 之后插入:

```rust
/// ⑥:右区最多占标题条的多少宽度。0.45 —— 一半以内,保证主机名(标识)
/// 永远比目录名(上下文)有优先权。
const SIDE_MAX_FRAC: f32 = 0.45;

/// ⑥:目录路径的最后一级,给标题条显示用。
///
/// `/` 与 `~` 原样返回(它们本身就是「最后一级」);尾部斜杠忽略;空路径
/// 给 `None`(别在标题条上摆一个空标签)。
///
/// 非 UTF-8 走 `to_string_lossy`:显示层宁可出一个 `�` 也要把目录名摆出来 ——
/// 整段消失的话用户完全不知道自己在哪。**② 那边不吃这个宽容度**,它只接受
/// 绝对路径的原始字节。
pub fn dir_leaf(cwd: &[u8]) -> Option<String> {
    let trimmed = match cwd {
        [] => return None,
        [b'/'] => return Some("/".to_string()),
        _ => cwd.strip_suffix(b"/").unwrap_or(cwd),
    };
    let leaf = match trimmed.iter().rposition(|&b| b == b'/') {
        Some(i) => &trimmed[i + 1..],
        None => trimmed,
    };
    if leaf.is_empty() {
        return None;
    }
    Some(String::from_utf8_lossy(leaf).into_owned())
}

/// ⑥:标题条右区的文字。两者都没有时 `None`(整段不画,不留孤零零的 `·`)。
///
/// tmux 名在前:它是「我在哪个工作区」,比目录更外层。
pub fn side_text(cwd_leaf: Option<&str>, tmux: Option<&str>) -> Option<String> {
    match (tmux, cwd_leaf) {
        (Some(s), Some(d)) => Some(format!("{s} · {d}")),
        (Some(s), None) => Some(s.to_string()),
        (None, Some(d)) => Some(d.to_string()),
        (None, None) => None,
    }
}
```

在 `show` 的 `right_to_left` 布局里,**`⇆` 按钮之后、`ui.with_layout(left_to_right …)` 之前**插入:

```rust
                    // ⑥:右区 —— tmux 会话名 + 当前目录最后一级。摆在 `⇆`
                    // 左边,颜色比主机名弱:它是上下文,不是标识。
                    //
                    // **手动 `layout_no_wrap` + `allocate_exact_size`,不用
                    // `Label::truncate()`**:`right_to_left` 里一个会截断的
                    // Label 会先claim 掉全部剩余宽度,左区主机名就一点都不剩了。
                    // 手动摆能把宽度夹在 `SIDE_MAX_FRAC` 以内,两区各自截断。
                    if let Some(s) = side_text(v.cwd_leaf.as_deref(), v.tmux) {
                        let color = theme::c32(t.fg_muted);
                        let galley = ui.painter().layout_no_wrap(
                            s,
                            egui::FontId::proportional(12.0),
                            color,
                        );
                        let w = galley.size().x.min(full.width() * SIDE_MAX_FRAC);
                        let (r, _) = ui.allocate_exact_size(
                            egui::vec2(w, galley.size().y),
                            egui::Sense::hover(),
                        );
                        // 再按自己那块矩形裁一次:外层 `set_clip_rect(inner)`
                        // 只挡住溢出标题条,挡不住右区的字往左压到主机名上。
                        ui.painter()
                            .with_clip_rect(r)
                            .galley(r.min, galley, color);
                    }
```

- [ ] **Step 8: Run tests**

```bash
cargo test -p mullion-app --lib ui::pane_title:: 2>&1 | tail -5
```

Expected: `test result: ok.`。

### 8c: app.rs 填 `TitleView`

- [ ] **Step 9: Fill the fields**

在 `crates/mullion-app/src/app.rs` 构造 `TitleView` 的那处(`appearance:` 字段之后)追加:

```rust
                                                // ⑥:远端报出来的目录 / tmux 名。
                                                // 拿不到就是 `None` —— 不显示,
                                                // 不猜(见 `docs/remote-state-setup.md`
                                                // 里远端要怎么配)。
                                                cwd_leaf: ws
                                                    .pane(g.id)
                                                    .and_then(|p| p.cwd.as_deref())
                                                    .and_then(
                                                        crate::ui::pane_title::dir_leaf,
                                                    ),
                                                tmux: ws
                                                    .pane(g.id)
                                                    .and_then(|p| p.tmux.as_deref()),
```

编译并补齐 app.rs / ui/mod.rs 测试里其它构造 `TitleView` 的地方(全填 `cwd_leaf: None, tmux: None,`):

```bash
cd /data/Mullion
grep -rn "TitleView {" crates/mullion-app/src
cargo test -p mullion-app --lib 2>&1 | tail -5
```

Expected: `test result: ok.`。

- [ ] **Step 10: Commit**

```bash
git add crates/mullion-app/src
git commit -m "feat(app): 标题条显示远端目录名与 tmux 会话名 (F30)

PaneState 新增 cwd/tmux,Workspace::pump 从 Emulator::take_remote_state
落地;两个字段重置策略不同(cwd 只增不清、tmux 收到新标题就整体重置,
否则退出 tmux 之后会话名会永久挂着)。标题条右区手动摆 galley 并夹在
45% 宽以内 —— right_to_left 里会截断的 Label 会claim 掉全部剩余宽度,
长主机名会把目录名挤掉,而分屏多时目录名才是唯一能区分的那一项。

守护测试:shell::workspace::tests::remote_state_lands_on_the_pane_with_the_right_reset_policy
          ui::pane_title::tests::a_long_cwd_and_tmux_name_do_not_push_the_area_past_title_px
          ui::pane_title::tests::a_long_host_name_does_not_squeeze_out_the_directory_name"
```

---

## Task 9: ② SFTP 面板继承终端目录

**Files:**
- Modify: `crates/mullion-app/src/app.rs`
- Test: `crates/mullion-app/src/app.rs`

- [ ] **Step 1: Write the failing test**

在 `crates/mullion-app/src/app.rs` 的 `mod tests` 尾部追加:

```rust
    /// ②:文件面板远端栏该开在哪。优先级:焦点 pane 报出来的当前目录 >
    /// F120 配置的默认远端目录 > `None`(交给 `spawn_sftp_open` 里的
    /// `canonicalize(".")` 落回登录目录)。
    ///
    /// **只接受绝对路径**:标题里拿到的可能是 `~/Mullion`,而 openssh 的
    /// `sftp-server` **不展开 `~`** —— 直接拿去 `canonicalize` 会失败,
    /// 面板会停在「取不到登录目录」,比不继承更糟。`~` 那种只用来在标题条上
    /// 显示目录名。
    ///
    /// 自证会变红:把 `files_start_dir` 里 `starts_with('/')` 那个判断删掉
    /// (`~` 用例红);把 `pane_cwd` 那条优先级去掉(第一条红)。
    #[test]
    fn files_start_dir_prefers_the_panes_cwd_but_only_if_absolute() {
        assert_eq!(
            files_start_dir(Some(b"/home/dev/Mullion"), Some("/srv")).as_deref(),
            Some("/home/dev/Mullion"),
            "pane 报的目录该压过配置的默认目录"
        );
        assert_eq!(
            files_start_dir(Some(b"~/Mullion"), Some("/srv")).as_deref(),
            Some("/srv"),
            "~ 不是绝对路径,sftp-server 不展开它,该落回配置值"
        );
        assert_eq!(
            files_start_dir(None, Some("/srv")).as_deref(),
            Some("/srv"),
            "没有 pane 目录时用配置值"
        );
        assert_eq!(files_start_dir(None, None), None);
        // 非 UTF-8 的远端路径落回配置值:`spawn_sftp_open` 收 `Option<String>`,
        // 到不了这条路。标题条那边仍会 lossy 显示出来(`dir_leaf`)。
        assert_eq!(
            files_start_dir(Some(b"/tmp/\xff"), Some("/srv")).as_deref(),
            Some("/srv")
        );
    }

    /// ② 的接线守护:`trigger_sftp_open` 必须把「焦点 pane 的 cwd」和
    /// 「配置的默认远端目录」一起交给 `files_start_dir`,再把结果传下去。
    ///
    /// **扎的是源码结构**,理由同
    /// `trigger_sftp_open_passes_the_tabs_default_remote_into_spawn_sftp_open`:
    /// 真正验它要一条活 sftp 连接。验证边界:挡得住「忘了继承」和「把
    /// default_remote 直接传下去」,挡不住 `files_start_dir` 自己写错(那由
    /// 上面那条纯函数测试守)。
    ///
    /// 自证会变红:把 `trigger_sftp_open` 里的 `files_start_dir(...)` 换回
    /// 直接传 `default_remote`。
    #[test]
    fn trigger_sftp_open_inherits_the_focused_panes_directory() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn trigger_sftp_open(&mut self, generation: u64) {")
            .nth(1)
            .expect("找不到 trigger_sftp_open 的定义");
        let body = &after[..after
            .find("\n    }\n")
            .expect("找不到 trigger_sftp_open 的函数结尾")];

        assert!(
            body.contains("focused_pane_cwd()"),
            "trigger_sftp_open 没读焦点 pane 的当前目录 —— ② 的继承会静默失效"
        );
        let call_after = body
            .split("spawn_sftp_open(")
            .nth(1)
            .expect("找不到 spawn_sftp_open 调用");
        let call_args = &call_after[..call_after
            .find(");")
            .expect("找不到 spawn_sftp_open 调用的结尾")];
        assert!(
            call_args.contains("start_dir"),
            "spawn_sftp_open 收到的不是 files_start_dir 的结果"
        );
    }
```

同时把既有的 `trigger_sftp_open_passes_the_tabs_default_remote_into_spawn_sftp_open` 改成新形态 —— **不删断言,改成断言这条链的新形状**:

```rust
        assert!(
            body.contains("let default_remote = tab.content.sftp_default_remote();"),
            "trigger_sftp_open 没有从 tab 读 default_remote"
        );
        // …(`spawn_sftp_open` 的参数断言改成)…
        assert!(
            call_args.contains("start_dir"),
            "spawn_sftp_open 的调用没有把起始目录传下去——配置的默认远端目录\
             和 ② 的目录继承都会在这一步被静默丢弃,验收清单第 7 条会失效"
        );
```

并在该测试的文档注释里补一行:

```rust
    /// ②:`default_remote` 现在不再直接传给 `spawn_sftp_open`,而是先跟焦点
    /// pane 的 cwd 一起过 `files_start_dir`。这条守的仍是「配置值不许在这一步
    /// 丢掉」,只是落点从参数名变成了 `start_dir` 这条链。
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p mullion-app --lib tests::files_start_dir 2>&1 | tail -20
```

Expected: 编译失败 `cannot find function 'files_start_dir'`。

- [ ] **Step 3: Implement**

(a) 在 `crates/mullion-app/src/app.rs` 里 `fn configured_remote_dir` 的**上方**插入:

```rust
/// ② 文件面板远端栏该开在哪。
///
/// 优先级:焦点 pane 报出来的当前目录 > F120 配置的默认远端目录 > `None`
/// (交给 [`configured_remote_dir`] 落回 `"."`,也就是登录目录)。
///
/// **只接受绝对路径**:标题里拿到的可能是 `~/Mullion`,而 openssh 的
/// `sftp-server` **不展开 `~`** —— 直接拿去 `canonicalize` 会失败,面板会停在
/// 「取不到登录目录」,比不继承更糟。非 UTF-8 的远端路径同样落回配置值
/// (`spawn_sftp_open` 收 `Option<String>`);标题条那边仍会 lossy 显示,
/// 见 `pane_title::dir_leaf`。
fn files_start_dir(pane_cwd: Option<&[u8]>, default_remote: Option<&str>) -> Option<String> {
    let from_pane = pane_cwd
        .filter(|c| c.starts_with(b"/"))
        .and_then(|c| String::from_utf8(c.to_vec()).ok());
    from_pane.or_else(|| default_remote.map(str::to_string))
}
```

(b) 在 `impl TabContent` 里(`fn sftp_default_remote` 旁边)加:

```rust
    /// ②:这个标签焦点 pane 报出来的当前目录。SFTP 节点标签/占位标签没有
    /// 终端,恒 `None`。
    fn focused_pane_cwd(&self) -> Option<Vec<u8>> {
        self.as_terminal()
            .and_then(|t| t.ws.focused())
            .and_then(|p| p.cwd.clone())
    }
```

(c) `trigger_sftp_open` 里,把

```rust
        let default_remote = tab.content.sftp_default_remote();
```

之后接上、并把 `spawn_sftp_open` 的实参换掉:

```rust
        let default_remote = tab.content.sftp_default_remote();
        // ②:优先开在这个标签焦点 pane 报出来的目录。
        let pane_cwd = tab.content.focused_pane_cwd();
        let start_dir = files_start_dir(pane_cwd.as_deref(), default_remote.as_deref());
        if let Some(files) = tab.content.files_panel_mut() {
            files.remote.load = crate::files::state::Load::Loading;
        }
        let task = spawn_sftp_open(&self._runtime, &self.proxy, generation, conn, start_dir);
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p mullion-app --lib tests::files_start_dir tests::trigger_sftp_open 2>&1 | tail -8
```

Expected: 全部 `ok`(含改过的那条既有测试)。

- [ ] **Step 5: Write the failing test (已开着的侧栏也要跟)**

在 `crates/mullion-app/src/app.rs` 的 `mod tests` 尾部追加:

```rust
    /// ② 的第二种情形:侧栏**已经**开着 sftp 时,「关→开」这一次跃迁也要
    /// 把远端栏带过去 —— `trigger_sftp_open` 只在第一次开连接时跑,之后就
    /// 短路返回了。
    ///
    /// **判据是「只在跃迁上同步」**:一直跟着焦点 pane 走的话,用户在面板里
    /// 点开的目录会被反复拽回终端所在目录,完全没法浏览。
    ///
    /// **扎的是源码结构**:真正验它要一条活 sftp 连接 + 一个 winit 事件循环。
    /// 验证边界:挡得住「忘了接同步」和「每帧都同步」,挡不住
    /// `sync_files_to_focused_pane` 内部写错(那由 `files_start_dir` 那条纯
    /// 函数测试和 `apply_remote_file_action` 的既有测试守)。
    ///
    /// 自证会变红:删掉那个 `if self.ui.files_sidebar_open && !files_was_open`
    /// 判断(第二条红);把它改成 `if self.ui.files_sidebar_open`(第三条红)。
    #[test]
    fn the_files_sidebar_syncs_to_the_terminal_only_on_the_closed_to_open_edge() {
        let src = include_str!("app.rs");
        assert!(
            src.contains("fn sync_files_to_focused_pane(&mut self)"),
            "缺 sync_files_to_focused_pane —— ② 在侧栏已开着时会静默不生效"
        );
        assert!(
            src.contains("let files_was_open = self.ui.files_sidebar_open;"),
            "没有记录侧栏这一帧之前的开合状态,判不出「关→开」跃迁"
        );
        assert!(
            src.contains("if self.ui.files_sidebar_open && !files_was_open {"),
            "同步的判据不是「关→开」跃迁 —— 每帧都同步会把用户在面板里点开的\
             目录反复拽回终端所在目录"
        );
    }
```

- [ ] **Step 6: Run test to verify it fails**

```bash
cargo test -p mullion-app --lib tests::the_files_sidebar_syncs 2>&1 | tail -10
```

Expected: FAIL `缺 sync_files_to_focused_pane`。

- [ ] **Step 7: Implement**

(a) 在 `crates/mullion-app/src/app.rs` 的 `fn files_hotkey_event` **之后**插入:

```rust
    /// ②:把远端栏带到焦点 pane 报出来的目录。
    ///
    /// 两种情形:
    /// - sftp 还没开(`sftp_client()` 是 `None`):什么都不用做 ——
    ///   `trigger_sftp_open` 稍后自己会读焦点 pane 的 cwd 定起始目录。
    /// - 已经开着:走一次普通的 `Goto`,与用户手点目录**同一条路径** ——
    ///   不新开第二条加载/错误处理逻辑。
    ///
    /// 拿不到绝对路径就什么都不做(面板停在原处),不猜 —— 见
    /// [`files_start_dir`]。
    fn sync_files_to_focused_pane(&mut self) {
        let Some(gen) = self.files_owner_generation() else {
            return;
        };
        let Some(tab) = self.tabs.by_generation(gen) else {
            return;
        };
        if tab.content.sftp_client().is_none() {
            return;
        }
        let Some(dir) = files_start_dir(tab.content.focused_pane_cwd().as_deref(), None) else {
            return;
        };
        let target = mullion_ssh::sftp::RemotePath::from_bytes(dir.into_bytes());
        self.apply_remote_file_action(gen, crate::ui::files_panel::FileAction::Goto(target));
    }
```

> `files_owner_generation()` 的返回类型以 `app.rs:2153` 附近的定义为准;若它返回 `Option<u64>` 直接照抄上面的写法。`RemotePath::from_bytes` 的入参形态以 `crates/mullion-ssh/src/sftp.rs` 里的签名为准。

(b) 在 `render_frame` 调用之前(`let a = self.active.as_mut()...` 的**上方**)插入:

```rust
                            // ②:侧栏「关→开」跃迁的判据。菜单项(`chrome.rs`)
                            // 直接改 `self.ui`,拿不到 `&mut App`;热键在
                            // `files_hotkey_event` 里改。两条路都汇到这一帧,
                            // 所以判据只放这一处,不在每个开关旁边各写一遍。
                            let files_was_open = self.ui.files_sidebar_open;
```

(c) 在写回 `taken_files` 的那个 `if let (Some(gen), Some(pf)) = ... { ... }` 块**之后**、`self.limiter.record_present(now);` 之前插入:

```rust
                            if self.ui.files_sidebar_open && !files_was_open {
                                self.sync_files_to_focused_pane();
                            }
```

- [ ] **Step 8: Run tests**

```bash
cargo test -p mullion-app --lib 2>&1 | tail -5
```

Expected: `test result: ok.`。

- [ ] **Step 9: Commit**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): 文件面板远端栏继承终端当前目录 (F50/F120)

files_start_dir 定优先级:焦点 pane 的 cwd > 配置的默认远端目录 > 登录目录。
只接受绝对路径 —— 标题里可能是 ~/x,而 openssh 的 sftp-server 不展开 ~,
拿去 canonicalize 会失败、面板停在「取不到登录目录」,比不继承更糟。
sftp 已开着时走一次普通 Goto;判据是侧栏「关→开」跃迁而不是每帧,
否则用户在面板里点开的目录会被反复拽回终端所在目录。

守护测试:tests::files_start_dir_prefers_the_panes_cwd_but_only_if_absolute
          tests::trigger_sftp_open_inherits_the_focused_panes_directory
          tests::the_files_sidebar_syncs_to_the_terminal_only_on_the_closed_to_open_edge"
```

---

## Task 10: 文档

**Files:**
- Create: `docs/remote-state-setup.md`
- Modify: `docs/gui-render-gotchas.md`

- [ ] **Step 1: 写远端配置手册**

新建 `docs/remote-state-setup.md`:

```markdown
# 远端状态上报(标题条目录名 / tmux 名 / SFTP 目录继承)

分屏标题条右边那一小段「tmux 名 · 目录名」,以及 `Ctrl+Shift+B` 打开文件面板时
远端栏落在哪个目录,数据**全部来自远端自己发过来的 OSC 转义序列**。
远端不发,这两个功能就静默降级(标题条不显示那一段、文件面板落回登录目录)——
不是 bug,是拿不到。

## 为什么不能旁路问一句

adr-009:一条 SSH 连接承载所有分屏。旁路开一条 exec channel 跑
`tmux display-message -p '#{pane_current_path}'` 拿回来的是**某个** pane 的路径,
而 `$SSH_CONNECTION` 四元组在所有分屏之间完全相同,没有任何办法把它对上是哪一块。
`channel.set_env` 注入自己的标识也不行:sshd 的 `AcceptEnv` 默认只放 `LANG`/`LC_*`。

所以只剩「远端主动报、按 channel 收」这一条路。

## 远端要怎么配

### tmux(推荐,一行)

`~/.tmux.conf`:

```
set -g set-titles on
```

tmux 会按默认的 `set-titles-string`(`#S:#I:#W`)发 OSC 2,我们从第一段认出会话名。
想连目录一起报:

```
set -g set-titles-string '#S:#I:#W #{pane_current_path}'
```

改完 `tmux kill-server` 或 `tmux source-file ~/.tmux.conf`。

### shell 报 cwd(OSC 7,更准)

Ubuntu 的 bash 默认**不发** OSC 7。加到 `~/.bashrc`:

```bash
osc7_cwd() { printf '\033]7;file://%s%s\033\\' "$HOSTNAME" "$PWD"; }
PROMPT_COMMAND="osc7_cwd${PROMPT_COMMAND:+; $PROMPT_COMMAND}"
```

zsh 用户装了 `oh-my-zsh` 的话通常已经在发了。fish 从 3.x 起默认发。

**OSC 7 压过标题里的路径**:它是路径本身,标题里那个是给人看的(带 `~` 缩写、
可能被 shell 截断)。

## 降级行为

| 远端配置 | 标题条右区 | 文件面板起始目录 |
|---|---|---|
| 都没配 | 不显示 | F120 配置的默认远端目录 → 登录目录 |
| 只开 `set-titles on` | `会话名` | 同上(标题里没有路径) |
| `set-titles-string` 带路径 | `会话名 · 目录名` | 该目录(若是绝对路径) |
| 加了 OSC 7 | `会话名 · 目录名` | 该目录 |

标题里的路径常常是 `~/Mullion` 这种缩写形式。**`~` 只用来在标题条上显示目录名**,
不拿去当 SFTP 起始目录 —— openssh 的 `sftp-server` 不展开 `~`,`canonicalize("~/x")`
会失败,面板会停在「取不到登录目录」,比不继承更糟。

## 相关代码

- 解析:`crates/mullion-term/src/remote_state.rs`(纯函数,11 条测试)
- 采集:`Emulator::feed` 里跑 `Osc7Sniffer`,`take_remote_state` 取走
- 落地:`Workspace::pump` → `PaneState.cwd` / `.tmux`
- 显示:`ui/pane_title.rs` 的 `dir_leaf` / `side_text`
- 继承:`app.rs` 的 `files_start_dir`
```

- [ ] **Step 2: 补 gotchas**

在 `docs/gui-render-gotchas.md` 末尾追加两条(格式照该文件既有条目的「症状/规则/守护」三段式):

```markdown
## 网格排版的字号必须与量 `cell_w` 的字号同源

**症状**:终端里后面的字压到前面的字上(`.md` 和 `12 条` 重叠),而且只在部分
「字号 × 缩放」组合下出现 —— 换个 DPI 就好了,极难归因。

**规则**:`cell_w` 是用某个 `Metrics` 量出来的,排版就必须用**同一个** `Metrics`。
曾经排版用 `Metrics::new(cell_h * 0.8, cell_h)`、量宽用 `Metrics::new(font_px, cell_h)`;
`cell_h = ceil(font_px * 1.25)`,两者只在 `font_px * 1.25` 恰好是整数时相等
(10pt@150% 相等,10pt@100% 差 2%,一行 60 列漂出 1.2 格)。收成
`text.rs` 的 `grid_metrics`,唯一来源。

**守护**:`text::tests::the_font_size_used_for_layout_is_the_one_cell_w_was_measured_with`
(遍历 pt × scale 九组,判据是 60 个 `M` 的实际 advance == 60 × `cell_w`,容差 0.5px)。

## 盖在终端上的装饰用 `ctx.layer_painter`,不要用 `egui::Area`

**症状**:加了个分界线/描边之后,终端划选失效(鼠标按下没反应),但键盘还好。

**规则**:`Area`(以及任何 `allocate_*`)会占一块**可交互**矩形。T8 的指针路由
是「先喂 egui 后判」,所以盖在终端上的 `Area` 会把指针事件吃掉。纯装饰走
`ctx.layer_painter(LayerId::new(Order::Background, id))` —— 只画不占。
层序取 `Order::Background` 就够:egui 整层 composite 在 wgpu 自绘的终端之上,
同时又在面板/弹窗之下(不会盖住模态框)。

另一半:**装饰不许改几何**。给焦点 pane 缩 `term_px` 让出描边空间会改 `grid`,
每切一次焦点就发一次 `window_change`(T4),远端 TUI 每点一下重排一次。
分界线画进 `layout_geometry` 已经让出的 `GAP_PX` 缝里。

**守护**:`ui::pane_edges::tests`(6 条,含分界线不压 `term_px`、无焦点不画环)。
```

- [ ] **Step 3: Commit**

```bash
git add docs/remote-state-setup.md docs/gui-render-gotchas.md
git commit -m "docs: 远端状态上报运行手册 + 两条渲染坑(字号同源 / layer_painter)"
```

---

## Task 11: 跑绿 + 交付

- [ ] **Step 1: 全绿**

```bash
cd /data/Mullion
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log | tail -20
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20
cargo fmt --check 2>&1 | tail -5
```

Expected: 所有 `test result: ok.`,clippy 无输出,fmt 无输出。

- [ ] **Step 2: 升版本**

`Cargo.toml` 的 `workspace.package.version` 第三位 +1(`0.1.48` → `0.1.49`)。

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: 版本 0.1.49(修文字重叠/图标截断 + 分屏分界线与焦点提示 + 标题条远端状态 + SFTP 目录继承 + 隐藏标签栏)"
```

- [ ] **Step 3: 交叉编译 + objdump 验收**

```bash
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app 2>&1 | tail -5
objdump -p target/x86_64-pc-windows-gnu/release/mullion-app.exe | grep "DLL Name" | sort -u
```

Expected: **不出现** `libgcc_s_seh-1.dll` 与 `libwinpthread-1.dll`(出现即不合格,按
`docs/cross-compile-windows.md` 修)。

- [ ] **Step 4: 发 Release**

```bash
cd /data/Mullion
cp target/x86_64-pc-windows-gnu/release/mullion-app.exe /tmp/mullion.exe
cd /tmp && sha256sum mullion.exe > mullion.exe.sha256 && cat mullion.exe.sha256
# 先 push,否则 gh 会把 tag 建在远端旧 HEAD 上
cd /data/Mullion && git push origin main
HTTPS_PROXY=http://127.0.0.1:7890 gh release create v0.1.49 \
  /tmp/mullion.exe /tmp/mullion.exe.sha256 -t "v0.1.49" -F /tmp/notes.md --repo kilobitcy/Mullion
```

`/tmp/notes.md` 必须包含「修了什么」+ sha256 + 首次运行提示(`Unblock-File .\mullion.exe`)+
下面这份**人工验收清单**(无头容器验不了的部分):

1. **① 文字不再重叠** —— 在 tmux 里跑 Claude Code,输出长行中英混排,确认相邻字不叠;
   把设置里的字号在 10 / 11 / 13pt 之间各切一遍,Windows 缩放 100% / 125% / 150% 各试一遍。
2. **⑤ 图标完整** —— 给会话设个 `.ico` 图标,分屏后看标题条上的图标底部有没有被切;
   同样过一遍三档缩放。
3. **③ 分界线** —— 左右 + 上下分屏,确认缝里有一条细线,既看得见又不抢眼。
4. **④ 焦点分屏** —— 开 4 块分屏,点来点去,确认一眼能看出焦点在哪(accent 描边 +
   标题条微亮);把标题条关掉(F83)再试一遍,描边应该还在。
5. **⑥ 标题条右区** —— 按 `docs/remote-state-setup.md` 配好远端,确认标题条右边出现
   `会话名 · 目录名`;`cd` 一下确认目录名跟着变;`exit` 退出 tmux 确认会话名消失;
   什么都不配的机器上确认那一段整段不显示(不是显示空白或占位符)。
6. **② SFTP 继承目录** —— 在终端里 `cd` 到某个深目录,`Ctrl+Shift+B` 打开文件面板,
   确认远端栏直接落在那个目录;再在面板里点开别的目录,回终端再 `cd`,确认面板**不会**
   被拽回去;关掉侧栏再开一次,确认这次跟到新目录。
7. **⑦ 标签栏没了** —— 确认窗口顶部不再有标签条,而且中央区把那点高度吃回去了
   (终端多出一行,不是留一条空白);`Ctrl+Tab` 切标签仍然能用。

- [ ] **Step 5: 报给用户**

Release 链接 + sha256 + 上面那份验收清单。

---

## Self-Review

**Spec coverage**:①→Task 1;②→Task 9;③→Task 4 + Task 5;④→Task 5;⑤→Task 3;
⑥→Task 6 + 7 + 8 + 10;⑦→Task 2。spec 的 D1–D11 全部落在具体 Task 上;
spec 的「不动的东西」(不缩 `term_px`、不开旁路 exec 通道、不动 `GAP_PX` 大小、
不删标签栏代码)分别由 Task 5 的模块文档、Task 10 的手册、Task 5 的 `divider_lines_of`、
Task 2 的 `tab_bar_inner` 兜住。

**已知的跨 Task 依赖**(执行时不要打乱顺序):
- Task 3 必须先把 `TitleView` 的 `cwd_leaf` / `tmux` 两个字段加上(值先填 `None`),
  Task 5 与 Task 8 的测试都要构造它。
- Task 5 的 `pane_edges` 测试用 `theme::MULLION_DARK.divider`,所以 Task 4 必须在它之前。
- Task 8a 依赖 Task 7 的 `Emulator::take_remote_state`。
- Task 9 依赖 Task 8a 的 `PaneState.cwd`。

**类型一致性**:`RemoteState.cwd: Option<Vec<u8>>`(term)→ `PaneState.cwd: Option<Vec<u8>>`
(app)→ `dir_leaf(&[u8]) -> Option<String>` → `TitleView.cwd_leaf: Option<String>`;
另一条 → `files_start_dir(Option<&[u8]>, Option<&str>) -> Option<String>` →
`spawn_sftp_open(.., Option<String>)`,与既有签名一致。
`title_bar_px(f32) -> u32` 与 `PxRect` 的 `u32` 字段一致。
