# 切片 P2-a：会话图标与语义色 实现计划（F61 / F62）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 给会话一个一眼可辨的视觉身份（一个图标 + 一种颜色），显示在会话列表行、pane 标题条、状态栏三处，降低「我以为我在测试机上」这类误操作。

**Architecture:** 数据层（`AppearancePrefs`/`IconSpec`/`ColorSpec`/`ColorTarget` 与继承解析）在 v0.1.14 已就绪，本切片**零 schema 改动**，是纯 egui 侧接线。新增 `ui/badge.rs` 作为三处落点共用的绘制原语与解析缓存；8 色预设与 hex 解析进 `theme.rs`；编辑器控件按既有约定进 `fields.rs`。

**Tech Stack:** Rust 2021 / egui 0.30 / epaint / `mullion-store`（`inherit::resolve`）

**设计文档：** `docs/superpowers/specs/2026-08-07-slice-p2a-icons-colors-design.md`

---

## 给实现者的前置说明

**读之前必须知道的三件事：**

1. **`inherit::resolve` 绝不能进渲染热路径。** 它的文档注释点名了本项目陷阱 T3（喂数据和重绘没解耦 → 每秒几千次重绘、GPU 空转）。会话列表每帧画几十行，逐行 resolve 就是直接踩。所有落点只接受**已解析**的 `&Appearance`，解析统一由 `AppearanceCache::rebuild` 在记录变更时做。

2. **`pane_title.rs` 有两个越界坑**（该文件顶部注释详述）：`Frame` 的 `min_rect + margin` 会撑破 `Area`；`set_min_size` 只设下限。本切片在 pane 标题条画竖条，**必须用 painter 直接画在已 `allocate_rect` 的 `full` 矩形里**，不新增任何 widget、不参与布局计算。守护测试 `area_rect_matches_title_px_exactly_across_dpi_scales` 必须全程绿。

3. **`chrome.rs::status_text` 是纯函数返回字符串，字形不进字符串。** 已有 `status_text_carries_no_dot_glyph` 守着。状态栏的会话色**必须画出来**，绝不能拼进文本。

**「绿」的定义**（CLAUDE.md）：`cargo test --workspace` 全过 **且** `cargo clippy --workspace --all-targets -- -D warnings` 无输出。只跑单个 crate 不叫绿。

**大输出先落盘再 grep：**
```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
```

---

## 文件结构

**新建**

| 文件 | 职责 |
|---|---|
| `crates/mullion-app/src/ui/badge.rs` | 会话外观的全部：`Appearance` 数据、`AppearanceCache` 解析缓存、`should_paint` 落点判定、`builtin_shape` 形状表、`paint_icon` / `paint_edge_bar` 绘制原语 |

**修改**

| 文件 | 改什么 |
|---|---|
| `crates/mullion-app/src/theme.rs` | 加 `LABEL_PALETTE`（8 色）、`parse_hex`、`contrast_ratio` |
| `crates/mullion-app/src/ui/mod.rs` | 加 `pub mod badge;`；`UiFrame` 加 `appearance` 字段 |
| `crates/mullion-app/src/ui/session_manager/list.rs` | `session_row` 画右侧竞色条 + 图标槽位 |
| `crates/mullion-app/src/ui/session_manager/mod.rs` | `show` 多收一个缓存参数往下传 |
| `crates/mullion-app/src/ui/session_manager/fields.rs` | `basic()` 里新开「外观」section |
| `crates/mullion-app/src/ui/session_manager/buffer.rs` | `EditorBuffer` 加两个图标缓冲字段；`set_color_target` 纯函数 |
| `crates/mullion-app/src/ui/pane_title.rs` | `TitleView` 加 `appearance`；画左侧竖条 + 图标 |
| `crates/mullion-app/src/ui/chrome.rs` | `status_bar` 加 `session_color` 参数，画小色块 |
| `crates/mullion-app/src/shell/workspace/mod.rs` | `HostConn` 加 `session_id` |
| `crates/mullion-app/src/app.rs` | 维护 `AppearanceCache`；构造 `HostConn` / `TitleView` 时填新字段 |

**与设计文档的一处偏离（更简单，行为等价）**

设计文档 §2 写「pane 打开时算一次，存进 pane 自身状态，保存时同步刷新所有已打开 pane」。实现改为：**pane 只存 `session_id`，画的时候从 `AppearanceCache` 查**。这样从结构上就不存在「pane 那份缓存忘了刷」的陈旧态——不是靠记得刷新，是靠根本没有第二份。

---

## Task 1: 色板与 hex 解析（`theme.rs`）

**Files:**
- Modify: `crates/mullion-app/src/theme.rs`

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-app/src/theme.rs` 的 `mod tests` 里，`apply_egui_writes_theme_tokens_into_visuals` 之后追加：

```rust
    /// F62:`#rrggbb` 是唯一认的写法。**解析失败返回 `None` 而不是报错**——
    /// 配置文件被手改坏不该让会话列表画不出来(设计 §3)。
    ///
    /// 不认 3 位缩写 / 8 位带 alpha / 不带 `#` 的裸串:多一种写法就多一种
    /// 「存进去是这个、读出来是那个」的可能。
    #[test]
    fn parse_hex_accepts_six_digits_and_rejects_everything_else() {
        assert_eq!(parse_hex("#e06767"), Some(Rgb::new(0xe0, 0x67, 0x67)));
        assert_eq!(
            parse_hex("#E06767"),
            Some(Rgb::new(0xe0, 0x67, 0x67)),
            "大小写不敏感"
        );
        assert_eq!(parse_hex("e06767"), None, "缺 # 不认");
        assert_eq!(parse_hex("#e067"), None, "位数不足不认");
        assert_eq!(parse_hex("#e0676777"), None, "8 位带 alpha 不认");
        assert_eq!(parse_hex("#gghhii"), None, "非十六进制不认");
        assert_eq!(parse_hex(""), None);
        // 「中文」= 6 个**字节**但只有 2 个 char。先按字节长度过滤再切片会 panic,
        // 这条钉死实现必须靠 ASCII 判定挡在切片之前。
        assert_eq!(parse_hex("#中文"), None, "多字节字符不能让切片 panic");
    }

    /// F62:8 个预设色是画在 `panel_bg` 上的 3px 竖条。竖条是**非文本**元素,
    /// WCAG 1.4.11 的阈值是 3:1(不是文字的 4.5:1)。低于这个数,用户在真实
    /// 显示器上根本分不出这条会话有没有标色 —— 整个特性就白做了。
    #[test]
    fn label_palette_contrasts_at_least_3_to_1_against_panel_bg() {
        for (name, hex, _) in LABEL_PALETTE {
            let c = parse_hex(hex)
                .unwrap_or_else(|| panic!("预设色「{name}」的 hex {hex} 解析不了"));
            let ratio = contrast_ratio(c, MULLION_DARK.panel_bg);
            assert!(
                ratio >= 3.0,
                "预设色「{name}」({hex})与 panel_bg 的对比度只有 {ratio:.2}:1,\
                 达不到 WCAG 1.4.11 非文本元素要求的 3:1"
            );
        }
    }

    /// 紫**必须**跟 `accent` 不同色。会话列表行的左 3px 是选中态 accent 条、
    /// 右 3px 是这里的语义色条 —— 两者同色时,选中一条标了紫的会话,左右两条
    /// 边看起来一模一样,用户分不出哪条是「选中」哪条是「标色」。
    ///
    /// 这条专门防「顺手统一一下」:accent(#8b95ff)和紫(#b98bff)长得很像,
    /// 后来的人很容易觉得是重复定义而把它们合并。
    #[test]
    fn palette_purple_differs_from_accent_so_the_two_edge_bars_stay_distinguishable() {
        let (_, hex, _) = LABEL_PALETTE
            .iter()
            .find(|(name, _, _)| *name == "紫")
            .expect("色板里应该有「紫」");
        let purple = parse_hex(hex).expect("紫的 hex 应可解析");
        assert_ne!(
            purple, MULLION_DARK.accent,
            "紫与 accent 同色会让列表行左右两条边分不出「选中」和「标色」"
        );
    }

    /// 对比度公式自身的锚点:纯黑对纯白必须正好是 21:1(WCAG 定义的上界),
    /// 同色必须是 1:1。没有这条,上面那条 3:1 断言可能是在用一个算错的公式
    /// 「验证通过」。
    #[test]
    fn contrast_ratio_matches_wcag_endpoints() {
        let black = Rgb::new(0, 0, 0);
        let white = Rgb::new(255, 255, 255);
        assert!((contrast_ratio(black, white) - 21.0).abs() < 0.01);
        assert!((contrast_ratio(white, black) - 21.0).abs() < 0.01, "对称");
        assert!((contrast_ratio(white, white) - 1.0).abs() < 1e-9);
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app --lib theme:: 2>&1 | tail -20
```
Expected: 编译失败，`cannot find function parse_hex` / `cannot find value LABEL_PALETTE` / `cannot find function contrast_ratio`

- [ ] **Step 3: 写实现**

在 `crates/mullion-app/src/theme.rs` 的 `pub fn c32` 定义**之前**（即 `clear_color` 之后）插入：

```rust
/// F62 会话语义色的 8 个预设：`(显示名, hex, 建议用途)`。
///
/// **按颜色命名而不是按环境命名**（不叫「生产色」「测试色」）：环境语义是 F64
/// 的地盘，两处都定义「什么是生产」必然会漂移。第三列只作为 tooltip 出现，
/// 是建议，不产生任何语义。
///
/// **不放进 `Theme`**：`Theme` 的字段是「UI 自己用的语义色」，F84 主题切换时
/// 整套换；而这 8 个是用户挑选的标识色，存进 `ColorSpec.hex` 后就与主题脱钩了。
/// 换个主题不该让用户标的红变成另一种红。
///
/// 红/黄/绿/蓝/灰刻意复用 `danger_soft`/`warn`/`ok`/`info`/`fg_dimmer` 的同一组
/// 色值（同一套调色逻辑，不引入第二种审美）；**紫故意不取 `accent` 的 #8b95ff**，
/// 理由见 `palette_purple_differs_from_accent_so_the_two_edge_bars_stay_distinguishable`。
pub const LABEL_PALETTE: [(&str, &str, &str); 8] = [
    ("红", "#e06767", "生产 / 高危"),
    ("橙", "#e0955f", "预发 / 待处理"),
    ("黄", "#e0b767", "测试"),
    ("绿", "#7fd99b", "开发 / 安全"),
    ("青", "#67d0d9", "数据库 / 存储"),
    ("蓝", "#7c9eff", "内网 / 常用"),
    ("紫", "#b98bff", "个人 / 实验"),
    ("灰", "#8a90a8", "归档 / 弃用"),
];

/// `#rrggbb` → `Rgb`。**解析失败返回 `None`，不报错**——`ColorSpec.hex` 是自由
/// 文本（用户可自定义 hex，配置文件也可能被手改），一个坏值不该让整张会话列表
/// 画不出来。调用方一律把 `None` 当作「没设色」。
///
/// 只认 6 位十六进制、必须带 `#`、大小写不敏感。
pub fn parse_hex(s: &str) -> Option<Rgb> {
    let h = s.strip_prefix('#')?;
    // ASCII 判定必须挡在切片之前：`h.len()` 是**字节**数，"中文" 是 6 字节但只有
    // 2 个 char，直接 `&h[0..2]` 会在字符边界内切开而 panic。
    // `&&` 短路 + `is_ascii_hexdigit` 保证走到切片时每个字节都是单字节字符。
    if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(Rgb::new(
        u8::from_str_radix(&h[0..2], 16).ok()?,
        u8::from_str_radix(&h[2..4], 16).ok()?,
        u8::from_str_radix(&h[4..6], 16).ok()?,
    ))
}

/// WCAG 相对亮度。分量先转线性再加权——直接拿 sRGB 分量算会得到偏亮的结果
/// （同 `clear_color` 那个坑，见 `clear_color_is_linear_not_raw_srgb`）。
fn relative_luminance(c: Rgb) -> f64 {
    0.2126 * srgb_to_linear(c.r) + 0.7152 * srgb_to_linear(c.g) + 0.0722 * srgb_to_linear(c.b)
}

/// WCAG 对比度，1.0（同色）~ 21.0（纯黑对纯白）。
///
/// 用来在测试里**实算**预设色的可见性，而不是靠眼睛和感觉调色。
/// 文本要 4.5:1（WCAG 1.4.3），非文本图形要 3:1（WCAG 1.4.11）——
/// 3px 竖条属后者。
pub fn contrast_ratio(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p mullion-app --lib theme:: 2>&1 | tail -15
```
Expected: `test result: ok.`，其中包含 `parse_hex_accepts_six_digits_and_rejects_everything_else`、`label_palette_contrasts_at_least_3_to_1_against_panel_bg`、`palette_purple_differs_from_accent_so_the_two_edge_bars_stay_distinguishable`、`contrast_ratio_matches_wcag_endpoints` 四条。

- [ ] **Step 5: 提交**

```bash
cd /data/Mullion
git add crates/mullion-app/src/theme.rs
git commit -m "feat(app): 8 色语义色板 + hex 解析 (F62)

色板按颜色命名而非按环境命名,避免与将来的 F64 环境等级撞语义;
用途只作 tooltip 建议。紫故意避开 accent(#8b95ff),否则列表行左右
两条边(选中态 accent / 语义色)会分不出来。

对比度用 WCAG 公式实算进测试:3px 竖条是非文本元素,阈值 3:1
(1.4.11),不是文字的 4.5:1。parse_hex 坏值降级为 None 不报错。"
```

---

## Task 2: `Appearance` 数据与落点判定（新建 `ui/badge.rs`）

**Files:**
- Create: `crates/mullion-app/src/ui/badge.rs`
- Modify: `crates/mullion-app/src/ui/mod.rs`（加模块声明）

- [ ] **Step 1: 建文件并写失败的测试**

创建 `crates/mullion-app/src/ui/badge.rs`，**只写文件头 + 测试**（实现下一步补）：

```rust
//! 会话外观(F61 图标 / F62 语义色):数据、解析缓存、落点判定、绘制原语。
//!
//! 三处落点 —— 会话列表行(`session_manager/list.rs`)、pane 标题条
//! (`pane_title.rs`)、状态栏(`chrome.rs`)—— 共用这里,不各画各的。
//!
//! **本模块不调 `inherit::resolve`,除了 `AppearanceCache::rebuild`。**
//! 那个函数的文档注释点名了陷阱 T3(喂数据和重绘没解耦):会话列表每帧要画
//! 几十行,逐行解析继承就是每秒几千次的无谓计算。绘制侧一律只收已解析好的
//! `&Appearance`。

use mullion_store::model::{ColorSpec, ColorTarget, IconSpec};

use crate::theme;

/// 从 `ResolvedConfig` 摘出来的外观部分。
///
/// 单独立一个类型而不是直接传 `ResolvedConfig`:后者还揣着 scrollback、
/// 代理、跳板、自动化 —— 绘制层不该看见那些,也不该因为它们变了就重画。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Appearance {
    pub icon: Option<IconSpec>,
    pub color: Option<ColorSpec>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_store::model::{ColorSpec, ColorTarget};

    fn colored(hex: &str, targets: &[ColorTarget]) -> Appearance {
        Appearance {
            icon: None,
            color: Some(ColorSpec {
                hex: hex.to_string(),
                apply_to: targets.to_vec(),
            }),
        }
    }

    /// F62 的核心判定:颜色画在哪由 `apply_to` 说了算,不由落点自己决定。
    /// 三处落点共用这一个函数,所以过滤逻辑只有一份、只能错一次。
    #[test]
    fn should_paint_only_where_apply_to_says_so() {
        let a = colored("#e06767", &[ColorTarget::ListItem, ColorTarget::PaneTitle]);
        assert_eq!(
            should_paint(&a, ColorTarget::ListItem),
            Some(egui::Color32::from_rgb(0xe0, 0x67, 0x67))
        );
        assert_eq!(
            should_paint(&a, ColorTarget::PaneTitle),
            Some(egui::Color32::from_rgb(0xe0, 0x67, 0x67))
        );
        assert_eq!(
            should_paint(&a, ColorTarget::StatusBar),
            None,
            "没勾状态栏就不该在状态栏上色"
        );
        assert_eq!(should_paint(&a, ColorTarget::Tab), None);
    }

    /// 没设色的会话在任何落点都不画。
    #[test]
    fn should_paint_returns_none_when_no_color_is_set() {
        let a = Appearance::default();
        for target in [
            ColorTarget::Tab,
            ColorTarget::ListItem,
            ColorTarget::PaneTitle,
            ColorTarget::StatusBar,
        ] {
            assert_eq!(should_paint(&a, target), None);
        }
    }

    /// `apply_to: []` 是**合法状态** =「色留着,暂时哪都不显示」。
    /// 编辑器里取消勾选所有落点不清除颜色(与跳板「切到无/继承时链条缓冲
    /// 不清空」同一条原则:用户切走再切回,配的东西还在),所以这个组合会
    /// 真实存在于配置里,不能当成坏数据。
    #[test]
    fn empty_apply_to_paints_nowhere_but_is_not_an_error() {
        let a = colored("#e06767", &[]);
        assert_eq!(should_paint(&a, ColorTarget::ListItem), None);
        assert!(a.color.is_some(), "颜色本身必须留着");
    }

    /// 坏 hex 降级成「没设色」,不 panic、不报错。配置文件被手改坏
    /// (或将来引入新写法的旧版本读到)不该让整张会话列表画不出来。
    #[test]
    fn unparseable_hex_degrades_to_no_color_instead_of_panicking() {
        let a = colored("not-a-color", &[ColorTarget::ListItem]);
        assert_eq!(should_paint(&a, ColorTarget::ListItem), None);
    }
}
```

在 `crates/mullion-app/src/ui/mod.rs` 顶部模块声明区（第 2 行 `pub mod chrome;` 之前）加一行，保持字母序：

```rust
pub mod badge;
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app --lib badge:: 2>&1 | tail -20
```
Expected: 编译失败，`cannot find function should_paint in this scope`

- [ ] **Step 3: 写实现**

在 `crates/mullion-app/src/ui/badge.rs` 的 `Appearance` 定义之后、`mod tests` 之前插入：

```rust
/// 这个落点该用什么颜色画。`None` = 不画。
///
/// 三处落点共用,所以「`apply_to` 过滤 + hex 解析失败降级」这两件事
/// 只有一份实现、只能错一次。
pub fn should_paint(a: &Appearance, target: ColorTarget) -> Option<egui::Color32> {
    let c = a.color.as_ref()?;
    if !c.apply_to.contains(&target) {
        return None;
    }
    theme::parse_hex(&c.hex).map(theme::c32)
}
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p mullion-app --lib badge:: 2>&1 | tail -15
```
Expected: `test result: ok.` 含 4 条 badge 测试

- [ ] **Step 5: 提交**

```bash
cd /data/Mullion
git add crates/mullion-app/src/ui/badge.rs crates/mullion-app/src/ui/mod.rs
git commit -m "feat(app): 会话外观数据与落点判定 should_paint (F62)

三处落点(会话列表/pane 标题条/状态栏)共用同一个判定函数,apply_to
过滤与 hex 坏值降级只有一份实现。

apply_to 为空是合法状态(色留着、暂时不显示),不是坏数据 —— 编辑器
取消勾选所有落点时不清除颜色,与跳板缓冲不清空同一条原则。"
```

---

## Task 3: 内置形状库与绘制原语（`ui/badge.rs`）

**Files:**
- Modify: `crates/mullion-app/src/ui/badge.rs`

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-app/src/ui/badge.rs` 的 `mod tests` 里追加（`use` 行也要补）：

```rust
    use mullion_store::model::{IconKind, IconSpec};

    fn icon(kind: IconKind, value: &str) -> IconSpec {
        IconSpec {
            kind,
            value: value.to_string(),
        }
    }

    /// 数一帧里画出来的图形总数(递归展开 `Shape::Vec`)。
    ///
    /// 这是本模块唯一能自动验证「到底画没画」的手段:形状是 painter 直接
    /// 画的,没有 widget、没有 Response、没有文字锚点可以反查。
    fn count_shapes(shapes: &[egui::epaint::ClippedShape]) -> usize {
        fn walk(s: &egui::Shape) -> usize {
            match s {
                egui::Shape::Vec(v) => v.iter().map(walk).sum(),
                egui::Shape::Noop => 0,
                _ => 1,
            }
        }
        shapes.iter().map(|cs| walk(&cs.shape)).sum()
    }

    /// 跑一帧,返回「画了 `icon` 时的图形数」。传 `None` 得到不画任何图标的
    /// 基线 —— `CentralPanel` 自己也会画背景,不能拿绝对数字当断言。
    fn shapes_with(icon: Option<&IconSpec>) -> usize {
        let ctx = egui::Context::default();
        let out = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                if let Some(i) = icon {
                    let rect = egui::Rect::from_min_size(
                        egui::pos2(0.0, 0.0),
                        egui::vec2(16.0, 16.0),
                    );
                    paint_icon(
                        ui.painter(),
                        rect,
                        i,
                        Some(egui::Color32::RED),
                        &crate::theme::MULLION_DARK,
                    );
                }
            });
        });
        count_shapes(&out.shapes)
    }

    /// 8 个内置形状每一个都必须真画出东西来。
    ///
    /// 光断言 `builtin_shape(name).is_some()` 不够 —— 那只证明查表命中,
    /// 证不了 `paint_icon` 真往 painter 里放了图形(比如 `Polys` 分支忘了
    /// 遍历、或顶点表是空的)。
    #[test]
    fn every_builtin_shape_actually_paints_something() {
        let base = shapes_with(None);
        for name in BUILTIN_SHAPES {
            let n = shapes_with(Some(&icon(IconKind::Builtin, name)));
            assert!(
                n > base,
                "内置形状「{name}」没画出任何图形(基线 {base},实际 {n})"
            );
        }
    }

    /// 设计 §4.3 规则 2:认不出的值一律**不画**,与「没设图标」表现一致。
    /// 四种情况共用这一条降级路径,向前向后都不会崩:
    /// 旧配置手改坏、`IconKind::Custom`(本期不做)、将来新增的形状名在
    /// 旧版本上、emoji 超过 8 个 char。
    #[test]
    fn unrecognized_icons_paint_nothing() {
        let base = shapes_with(None);
        for bad in [
            icon(IconKind::Builtin, "no-such-shape"),
            icon(IconKind::Builtin, ""),
            icon(IconKind::Custom, "/path/to/some.png"),
            icon(IconKind::Emoji, ""),
            // 9 个 char > MAX_EMOJI_CHARS:用户把一整段文字粘进来会撑爆行高
            icon(IconKind::Emoji, "一二三四五六七八九"),
        ] {
            assert_eq!(
                shapes_with(Some(&bad)),
                base,
                "认不出的图标 {bad:?} 不该画任何东西"
            );
        }
    }

    /// emoji 长度上限:ZWJ 家庭序列(👨‍👩‍👧 是 5 个 char)和旗帜要放得下,
    /// 同时挡住把一整段文字粘进来。刻意不引 `unicode-segmentation` 做真
    /// 字素分割 —— 为一个上限校验加依赖不划算。
    #[test]
    fn emoji_length_limit_admits_zwj_sequences_and_rejects_prose() {
        assert!(emoji_is_paintable("🔥"));
        assert_eq!("👨‍👩‍👧".chars().count(), 5, "ZWJ 家庭序列确实是 5 个 char");
        assert!(emoji_is_paintable("👨‍👩‍👧"), "ZWJ 家庭序列必须放得下");
        assert!(!emoji_is_paintable(""), "空值不画");
        assert!(
            !emoji_is_paintable("这是一整段被粘贴进来的说明文字"),
            "超过上限的长文本必须挡住,否则会撑爆列表行高"
        );
    }

    /// 竖条画在指定的那一边,且宽度恒为 `EDGE_BAR_W`。
    ///
    /// 会话列表行的左 3px 已经被选中态 accent 条占了(见 `list.rs::session_row`),
    /// 语义色条必须走右边;pane 标题条没有这个占用,走左边。画反了两条会重叠,
    /// 选中态和标色在视觉上就合并了。
    #[test]
    fn edge_bar_sits_on_the_requested_side() {
        let rect = egui::Rect::from_min_size(egui::pos2(100.0, 200.0), egui::vec2(50.0, 44.0));
        assert_eq!(
            edge_bar_rect(rect, Side::Left),
            egui::Rect::from_min_size(egui::pos2(100.0, 200.0), egui::vec2(EDGE_BAR_W, 44.0))
        );
        assert_eq!(
            edge_bar_rect(rect, Side::Right),
            egui::Rect::from_min_size(
                egui::pos2(150.0 - EDGE_BAR_W, 200.0),
                egui::vec2(EDGE_BAR_W, 44.0)
            )
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app --lib badge:: 2>&1 | tail -20
```
Expected: 编译失败，`cannot find value BUILTIN_SHAPES` / `cannot find function paint_icon` / `cannot find function emoji_is_paintable` / `cannot find function edge_bar_rect` / `cannot find type Side`

- [ ] **Step 3: 写实现**

在 `crates/mullion-app/src/ui/badge.rs` 的 `should_paint` 之后、`mod tests` 之前插入。同时把文件顶部的 `use` 补成：

```rust
use mullion_store::model::{ColorSpec, ColorTarget, IconKind, IconSpec};

use crate::theme::{self, Theme};
```

实现：

```rust
/// emoji 值的 `char` 上限。ZWJ 家庭序列（👨‍👩‍👧 是 5 个 char）和旗帜要放得下，
/// 同时挡住用户把一整段文字粘进来撑爆行高。
///
/// 刻意不引 `unicode-segmentation` 做真字素分割：为一个上限校验加一个依赖
/// 不划算，而这个上限本来就是个粗筛。
pub const MAX_EMOJI_CHARS: usize = 8;

/// 边缘竖条宽度（逻辑点）。
pub const EDGE_BAR_W: f32 = 3.0;

/// 内置形状库（F61）。**这些名字进 `IconSpec::value`，是持久化数据的一部分，
/// 不可改名**——改一个名字，等于把所有用户已经存下的那个图标变成不认识的值
/// （然后按 §4.3 规则 2 降级为不画）。只能往后追加。
pub const BUILTIN_SHAPES: [&str; 8] = [
    "circle", "ring", "square", "diamond", "triangle", "hexagon", "star", "bar",
];

/// 一个内置形状的画法。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Shape {
    /// 实心圆。
    Circle,
    /// 空心圆环。
    Ring,
    /// 一组**凸**多边形，顶点归一化在单位方框（0.0..=1.0）内，逐个填充画出。
    ///
    /// 拆成多个是因为 epaint 的 `convex_polygon` 只对凸多边形正确：六芒星
    /// 这类凹形状直接喂进去会画成一坨错误的扇形，必须拆成两个凸三角形叠加。
    Polys(&'static [&'static [(f32, f32)]]),
}

const SQUARE: &[&[(f32, f32)]] = &[&[(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]];
const DIAMOND: &[&[(f32, f32)]] = &[&[(0.5, 0.0), (1.0, 0.5), (0.5, 1.0), (0.0, 0.5)]];
const TRIANGLE: &[&[(f32, f32)]] = &[&[(0.5, 0.0), (1.0, 1.0), (0.0, 1.0)]];
const HEXAGON: &[&[(f32, f32)]] = &[&[
    (0.25, 0.0),
    (0.75, 0.0),
    (1.0, 0.5),
    (0.75, 1.0),
    (0.25, 1.0),
    (0.0, 0.5),
]];
/// 六芒星 = 两个反向的正三角形叠加。**必须拆成两个凸多边形**，理由见
/// `Shape::Polys` 的注释。
const STAR: &[&[(f32, f32)]] = &[
    &[(0.5, 0.0), (0.933, 0.75), (0.067, 0.75)],
    &[(0.5, 1.0), (0.067, 0.25), (0.933, 0.25)],
];
/// 竖条：窄长方形，跟边缘竖条不是一回事（这个是图标，那个是行边缘装饰）。
const BAR: &[&[(f32, f32)]] = &[&[(0.32, 0.0), (0.68, 0.0), (0.68, 1.0), (0.32, 1.0)]];

/// 形状名 → 画法。**认不出的名字返回 `None`**（设计 §4.3 规则 2）。
pub fn builtin_shape(name: &str) -> Option<Shape> {
    Some(match name {
        "circle" => Shape::Circle,
        "ring" => Shape::Ring,
        "square" => Shape::Polys(SQUARE),
        "diamond" => Shape::Polys(DIAMOND),
        "triangle" => Shape::Polys(TRIANGLE),
        "hexagon" => Shape::Polys(HEXAGON),
        "star" => Shape::Polys(STAR),
        "bar" => Shape::Polys(BAR),
        _ => return None,
    })
}

/// emoji 值能不能画。空值和超长值都不画（走 §4.3 规则 2 那条降级路径）。
pub fn emoji_is_paintable(v: &str) -> bool {
    !v.is_empty() && v.chars().count() <= MAX_EMOJI_CHARS
}

/// 竖条画在 `rect` 的哪一边。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// 边缘竖条的矩形。抽成纯函数是因为「画在哪一边」是唯一能自动验的部分——
/// 画出来好不好看只有人眼能判定，但画反了边可以测。
pub fn edge_bar_rect(rect: egui::Rect, side: Side) -> egui::Rect {
    match side {
        Side::Left => egui::Rect::from_min_size(rect.min, egui::vec2(EDGE_BAR_W, rect.height())),
        Side::Right => egui::Rect::from_min_size(
            egui::pos2(rect.max.x - EDGE_BAR_W, rect.min.y),
            egui::vec2(EDGE_BAR_W, rect.height()),
        ),
    }
}

/// 画一条边缘竖条（F62）。
pub fn paint_edge_bar(p: &egui::Painter, rect: egui::Rect, side: Side, color: egui::Color32) {
    p.rect_filled(
        edge_bar_rect(rect, side),
        egui::Rounding::same(2.0),
        color,
    );
}

/// 画一个图标（F61）。`tint` 是会话的语义色，`None` = 未设色。
///
/// 三条规则（设计 §4.3）：
/// 1. **形状染色，emoji 不染** —— 形状用 `tint`（未设色时 `fg_muted`）；emoji
///    保持 `fg` 原色，黑白 🐧 染成红色就失去辨识度了。
/// 2. **认不出的一律不画** —— `IconKind::Custom`、坏形状名、超长/空 emoji
///    共用这一条降级路径。
/// 3. epaint **不支持 COLR/CPAL 彩色字形**，emoji 在界面上是**黑白剪影**。
///    这不是 bug，是 egui 的既有限制（内置字体 `NotoEmoji-Regular` /
///    `emoji-icon-font` 全是黑白轮廓，即使系统装了 Segoe UI Emoji 也一样）。
pub fn paint_icon(
    p: &egui::Painter,
    rect: egui::Rect,
    icon: &IconSpec,
    tint: Option<egui::Color32>,
    t: &Theme,
) {
    match icon.kind {
        IconKind::Builtin => {
            let Some(shape) = builtin_shape(&icon.value) else {
                return;
            };
            paint_shape(
                p,
                rect,
                shape,
                tint.unwrap_or_else(|| theme::c32(t.fg_muted)),
            );
        }
        IconKind::Emoji => {
            if !emoji_is_paintable(&icon.value) {
                return;
            }
            p.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                &icon.value,
                egui::FontId::proportional(rect.height().min(rect.width()) * 0.85),
                theme::c32(t.fg),
            );
        }
        // 自定义图片本期不做：要引 image 解码器，顶爆 N6 的 25MB 体积线。
        // 枚举变体保留（旧配置/将来），走同一条降级路径：不画。
        IconKind::Custom => {}
    }
}

fn paint_shape(p: &egui::Painter, rect: egui::Rect, shape: Shape, color: egui::Color32) {
    // 取内切正方形，免得图标在长条形 rect 里被拉扁；再内缩一成留呼吸空间。
    let side = rect.width().min(rect.height());
    let b = egui::Rect::from_center_size(rect.center(), egui::vec2(side, side)).shrink(side * 0.1);
    let s = b.width();
    match shape {
        Shape::Circle => p.circle_filled(b.center(), s * 0.5, color),
        Shape::Ring => p.circle_stroke(
            b.center(),
            s * 0.38,
            egui::Stroke::new(s * 0.16, color),
        ),
        Shape::Polys(polys) => {
            for poly in polys {
                let pts: Vec<egui::Pos2> = poly
                    .iter()
                    .map(|(x, y)| egui::pos2(b.min.x + x * s, b.min.y + y * s))
                    .collect();
                p.add(egui::Shape::convex_polygon(
                    pts,
                    color,
                    egui::Stroke::NONE,
                ));
            }
        }
    }
}
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p mullion-app --lib badge:: 2>&1 | tail -20
```
Expected: `test result: ok.` 含 8 条 badge 测试（4 条来自 Task 2 + 4 条新增）

- [ ] **Step 5: 提交**

```bash
cd /data/Mullion
git add crates/mullion-app/src/ui/badge.rs
git commit -m "feat(app): 内置形状库 + 图标/竖条绘制原语 (F61)

8 个形状全部 painter 自绘,零字体依赖、零体积,且能被语义色染色 ——
这是它相对 emoji 的价值。六芒星拆成两个凸三角形:epaint 的
convex_polygon 只对凸多边形正确。

形状名是持久化数据的一部分,不可改名。认不出的值一律不画,
Custom/坏形状名/超长 emoji 共用同一条降级路径。

emoji 呈现为黑白剪影 —— epaint 不支持 COLR/CPAL 彩色字形,
装了 Segoe UI Emoji 也一样,已写进验收清单。"
```

---

## Task 4: 解析缓存 `AppearanceCache`（`ui/badge.rs`）

**Files:**
- Modify: `crates/mullion-app/src/ui/badge.rs`

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-app/src/ui/badge.rs` 的 `mod tests` 里追加：

```rust
    use mullion_store::model::{
        Auth, AuthKind, Connection, Identity, Protocol, SessionRecord,
    };
    use mullion_store::{AppearancePrefs, GroupId, GroupRecord, SessionId};

    fn rec(id: u64, group: Option<GroupId>, appearance: AppearancePrefs) -> SessionRecord {
        SessionRecord {
            id: SessionId(id),
            modified_at: "2026-08-07T00:00:00Z".into(),
            identity: Identity {
                name: format!("s{id}"),
                note: String::new(),
                group_id: group,
                tags: Vec::new(),
            },
            connection: Connection {
                host: "192.0.2.1".into(),
                port: 22,
                protocol: Protocol::Ssh,
            },
            auth: Auth {
                user: "u".into(),
                kind: AuthKind::Password,
            },
            terminal: Default::default(),
            appearance,
            network: Default::default(),
            automation: Default::default(),
        }
    }

    fn appearance_with_color(hex: &str) -> AppearancePrefs {
        AppearancePrefs {
            icon: None,
            color: Some(ColorSpec {
                hex: hex.to_string(),
                apply_to: vec![ColorTarget::ListItem],
            }),
        }
    }

    /// 会话自己设了色就用自己的。
    #[test]
    fn cache_resolves_session_own_appearance() {
        let sessions = vec![rec(1, None, appearance_with_color("#e06767"))];
        let mut c = AppearanceCache::default();
        c.rebuild(&sessions, &[]);
        assert_eq!(
            should_paint(c.get(SessionId(1)).unwrap(), ColorTarget::ListItem),
            Some(egui::Color32::from_rgb(0xe0, 0x67, 0x67))
        );
    }

    /// 会话没设、分组设了 → 继承分组。
    ///
    /// 本切片分组管理器里**没有**外观编辑入口（`GroupRecord.appearance` 恒空），
    /// 但解析照走继承链。成本为零，而将来给分组接上外观时三处落点一行都不用改；
    /// 反过来若现在图省事直接读 `rec.appearance`，将来就得**记得**改三处——
    /// 那种「记得」正是漏掉的来源。这条测试就是那个「将来」的预演。
    #[test]
    fn cache_falls_back_to_group_appearance() {
        let gid = GroupId(7);
        let sessions = vec![rec(1, Some(gid), AppearancePrefs::default())];
        let groups = vec![GroupRecord {
            id: gid,
            name: "g".into(),
            tags: Vec::new(),
            terminal: Default::default(),
            appearance: appearance_with_color("#7fd99b"),
            network: Default::default(),
            automation: Default::default(),
        }];
        let mut c = AppearanceCache::default();
        c.rebuild(&sessions, &groups);
        assert_eq!(
            should_paint(c.get(SessionId(1)).unwrap(), ColorTarget::ListItem),
            Some(egui::Color32::from_rgb(0x7f, 0xd9, 0x9b)),
            "会话没设外观时应继承分组的"
        );
    }

    /// 会话设了就覆盖分组。
    #[test]
    fn session_appearance_overrides_group() {
        let gid = GroupId(7);
        let sessions = vec![rec(1, Some(gid), appearance_with_color("#e06767"))];
        let groups = vec![GroupRecord {
            id: gid,
            name: "g".into(),
            tags: Vec::new(),
            terminal: Default::default(),
            appearance: appearance_with_color("#7fd99b"),
            network: Default::default(),
            automation: Default::default(),
        }];
        let mut c = AppearanceCache::default();
        c.rebuild(&sessions, &groups);
        assert_eq!(
            should_paint(c.get(SessionId(1)).unwrap(), ColorTarget::ListItem),
            Some(egui::Color32::from_rgb(0xe0, 0x67, 0x67))
        );
    }

    /// **本切片最重要的一条结构性守护**：`get` 返回的是缓存住的值，不是当场
    /// 重算的。`inherit::resolve` 的文档注释点名了陷阱 T3——会话列表每帧要画
    /// 几十行，逐行解析继承就是每秒几千次无谓计算。
    ///
    /// 构造方法：`rebuild` 之后把源数据改掉，`get` 必须仍返回旧值。这不是在
    /// 鼓励用陈旧数据（调用方负责在记录变更后调 `rebuild`），而是证明 `get`
    /// 没有在背地里重算——重算入口只有 `rebuild` 一个，调用方才控制得住它
    /// 不落进渲染热路径。
    ///
    /// 自证会变红：把 `get` 改成每次现算（比如内部调 `resolve` 返回克隆值），
    /// 这条立刻报颜色变成了 `#7fd99b`。
    #[test]
    fn get_returns_the_cached_value_not_a_fresh_resolve() {
        let mut sessions = vec![rec(1, None, appearance_with_color("#e06767"))];
        let mut c = AppearanceCache::default();
        c.rebuild(&sessions, &[]);
        sessions[0].appearance = appearance_with_color("#7fd99b");
        assert_eq!(
            should_paint(c.get(SessionId(1)).unwrap(), ColorTarget::ListItem),
            Some(egui::Color32::from_rgb(0xe0, 0x67, 0x67)),
            "get 必须返回 rebuild 时缓存的值；返回新值说明它在渲染时现算，\
             那就是把 resolve 放进了每帧热路径(T3)"
        );
    }

    /// 缓存里没有的会话（比如刚被删掉、或 store 不可用）返回 `None`，
    /// 调用方按「没设外观」处理，不 panic。
    #[test]
    fn unknown_session_id_returns_none() {
        let c = AppearanceCache::default();
        assert!(c.get(SessionId(999)).is_none());
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app --lib badge:: 2>&1 | tail -20
```
Expected: 编译失败，`cannot find type AppearanceCache in this scope`

- [ ] **Step 3: 写实现**

在 `crates/mullion-app/src/ui/badge.rs` 的 `should_paint` 之后插入（`use` 补上 `GroupRecord` / `SessionId` / `SessionRecord`）：

顶部 `use` 改成：

```rust
use std::collections::HashMap;

// 全部走 `mullion_store` 顶层再导出（`lib.rs:26-29` 把 model 的这些类型都
// 摆到了顶层）。不混用 `mullion_store::model::X` 和 `mullion_store::X` 两条
// 路径引同一批类型——那会让人以为是两组不同的东西。
use mullion_store::{
    ColorSpec, ColorTarget, GroupRecord, IconKind, IconSpec, PrefsLayer, SessionId, SessionRecord,
};

use crate::theme::{self, Theme};
```

> 相应地，Task 2 / Task 3 里写的 `use mullion_store::model::{...}` 到这一步就被这条完整的 `use` 取代了；测试模块里的 `use mullion_store::model::{...}` 也统一改成 `use mullion_store::{...}`。落点文件（`list.rs` / `pane_title.rs` / `chrome.rs` / `fields.rs`）里那些 `mullion_store::model::ColorTarget` 的全限定写法同理，改成 `mullion_store::ColorTarget`。

实现：

```rust
/// 会话外观的解析缓存。
///
/// **存在的唯一理由是陷阱 T3。** `inherit::resolve` 的文档注释明确写着「结果
/// 应由调用方缓存，**不要在渲染热路径 / 每帧里重新调用**」。会话列表每帧要画
/// 几十行，pane 标题条每帧一条，逐个解析继承链就是每秒几千次无谓计算。
///
/// **重算入口只有 `rebuild` 一个**：保存 / 删除 / 分组变更后各调一次。
/// `get` 取 `&self` 且返回引用，类型上就不可能在内部现算再返回——这条约束
/// 是编译器保证的，不靠自觉。
#[derive(Debug, Default)]
pub struct AppearanceCache {
    map: HashMap<SessionId, Appearance>,
}

impl AppearanceCache {
    /// 按当前会话与分组重算全表。
    ///
    /// **层序必须是 `[会话, 分组]`**（`inherit::resolve` 的文档：「调用方负责
    /// 组装层序，当前为 `[会话, 分组]`」）。`cache_falls_back_to_group_appearance`
    /// 和 `session_appearance_overrides_group` 两条测试钉死这个顺序——写反了
    /// 会变成「分组盖掉会话」，用户改会话自己的颜色会不生效。
    ///
    /// `shell::store::SessionStore::resolved(id)` 做的是同一件事，这里**故意
    /// 不用**：那会让 `badge` 模块依赖 `SessionStore`，测试就得构造真 store
    /// （牵扯 keyring 和文件系统）。收纯数据切片换来纯单测，代价是层序组装
    /// 重复了一遍——上面那两条测试就是防这份重复漂移的。
    pub fn rebuild(&mut self, sessions: &[SessionRecord], groups: &[GroupRecord]) {
        self.map.clear();
        for rec in sessions {
            // 分组不存在（悬空 group_id）时只用会话自己这一层，跟
            // `group_manager::group_sessions` 把这类会话归进「未分组」是同一个
            // 姿态：一条坏引用不该让这条会话的外观整个消失。
            let group = rec
                .identity
                .group_id
                .and_then(|gid| groups.iter().find(|g| g.id == gid));
            let cfg = match group {
                Some(g) => mullion_store::resolve(&[rec as &dyn PrefsLayer, g as &dyn PrefsLayer]),
                None => mullion_store::resolve(&[rec as &dyn PrefsLayer]),
            };
            self.map.insert(
                rec.id,
                Appearance {
                    icon: cfg.icon,
                    color: cfg.color,
                },
            );
        }
    }

    /// 取一条会话的已解析外观。缓存里没有 → `None`（调用方按「没设外观」处理）。
    pub fn get(&self, id: SessionId) -> Option<&Appearance> {
        self.map.get(&id)
    }
}
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p mullion-app --lib badge:: 2>&1 | tail -20
```
Expected: `test result: ok.` 含 13 条 badge 测试

- [ ] **Step 5: 提交**

```bash
cd /data/Mullion
git add crates/mullion-app/src/ui/badge.rs
git commit -m "feat(app): 外观解析缓存 AppearanceCache (F61/F62)

存在的唯一理由是陷阱 T3:inherit::resolve 的文档注释明确警告结果必须
由调用方缓存、不得在渲染热路径每帧调用。会话列表每帧几十行,逐行解析
继承就是每秒几千次无谓计算。

重算入口只有 rebuild 一个;get 取 &self 且返回引用,类型上就不可能
内部现算再返回 —— 这条约束是编译器保证的。守护测试用「改源数据后
get 仍返回旧值」证明它真的缓存了。

分组本期恒空但解析照走继承链:将来给分组接外观时三处落点零改动。"
```

---

## Task 5: 会话列表落点（`session_manager/list.rs`）

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/list.rs`
- Modify: `crates/mullion-app/src/ui/session_manager/mod.rs`
- Modify: `crates/mullion-app/src/ui/mod.rs`
- Modify: `crates/mullion-app/src/app.rs`

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-app/src/ui/session_manager/list.rs` 的 `mod tests` 末尾追加：

```rust
    /// 数一帧里画出来的图形总数。同 `badge.rs::tests::count_shapes` 的手法：
    /// 竖条和图标都是 painter 直接画的，没有 widget、没有 Response 可以反查。
    fn count_shapes(shapes: &[egui::epaint::ClippedShape]) -> usize {
        fn walk(s: &egui::Shape) -> usize {
            match s {
                egui::Shape::Vec(v) => v.iter().map(walk).sum(),
                egui::Shape::Noop => 0,
                _ => 1,
            }
        }
        shapes.iter().map(|cs| walk(&cs.shape)).sum()
    }

    fn run_list(appearance: &crate::ui::badge::AppearanceCache) -> usize {
        let t = crate::theme::MULLION_DARK;
        let sessions = vec![rec(1, "dev-box", "192.0.2.10", &[])];
        let groups: Vec<GroupRecord> = Vec::new();
        let mut ui_state = UiState::default();
        let ctx = egui::Context::default();
        let out = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                show(
                    ui,
                    &t,
                    &mut ui_state,
                    &sessions,
                    &groups,
                    None,
                    appearance,
                );
            });
        });
        count_shapes(&out.shapes)
    }

    fn cache_with(color: Option<(&str, Vec<mullion_store::model::ColorTarget>)>)
        -> crate::ui::badge::AppearanceCache
    {
        let mut sessions = vec![rec(1, "dev-box", "192.0.2.10", &[])];
        if let Some((hex, apply_to)) = color {
            sessions[0].appearance = mullion_store::AppearancePrefs {
                icon: None,
                color: Some(mullion_store::model::ColorSpec {
                    hex: hex.to_string(),
                    apply_to,
                }),
            };
        }
        let mut c = crate::ui::badge::AppearanceCache::default();
        c.rebuild(&sessions, &[]);
        c
    }

    /// F62：勾了「会话列表」的会话，行上要多画一条竞色条。
    ///
    /// 自证会变红：把 `session_row` 里那段 `should_paint(.., ListItem)` 的
    /// 绘制注释掉，这条立刻报两者图形数相等。
    #[test]
    fn list_row_paints_an_edge_bar_when_apply_to_includes_list_item() {
        use mullion_store::model::ColorTarget;
        let none = run_list(&cache_with(None));
        let with = run_list(&cache_with(Some(("#e06767", vec![ColorTarget::ListItem]))));
        assert!(
            with > none,
            "勾了「会话列表」的会话应该多画一条竞色条(无色 {none} 个图形，有色 {with} 个)"
        );
    }

    /// 没勾「会话列表」就不画——`apply_to` 说了算，不是「设了色就到处画」。
    #[test]
    fn list_row_paints_nothing_when_apply_to_excludes_list_item() {
        use mullion_store::model::ColorTarget;
        let none = run_list(&cache_with(None));
        let other = run_list(&cache_with(Some((
            "#e06767",
            vec![ColorTarget::PaneTitle, ColorTarget::StatusBar],
        ))));
        assert_eq!(
            other, none,
            "只勾了 pane 标题条/状态栏的会话，不该在列表行上画竞色条"
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app --lib session_manager::list 2>&1 | tail -20
```
Expected: 编译失败，`this function takes 6 arguments but 7 arguments were supplied`（`show` 还没加参数）

- [ ] **Step 3: 写实现**

**3a.** `crates/mullion-app/src/ui/session_manager/list.rs` —— 改 `session_row`。

把第 33-41 行的函数签名与文档注释替换为：

```rust
/// 手绘一行会话。不用 `selectable_label`:设计稿要「状态点 + 名称 + user@host
/// 两行 + 选中态左侧强调条」,`selectable_label` 只画得出单行文本。
///
/// F61/F62 加了两样东西:**右**边缘的语义色竖条(左 3px 已被选中态 accent 占了,
/// 两者各占一边才不打架)、状态点与文字之间的 16px 图标槽位。
/// **槽位恒占**——没设图标的行也留白,否则有图标和没图标的行文字左边界参差。
fn session_row(
    ui: &mut Ui,
    t: &Theme,
    rec: &SessionRecord,
    selected: bool,
    connected: bool,
    appearance: &crate::ui::badge::Appearance,
) -> egui::Response {
```

在第 62 行（选中态左条那个 `if selected { ... }` 块的右花括号）**之后**插入：

```rust
    // F62:语义色竖条走**右**边缘 —— 左 3px 归选中态 accent,两者各占一边。
    if let Some(c) = crate::ui::badge::should_paint(
        appearance,
        mullion_store::model::ColorTarget::ListItem,
    ) {
        crate::ui::badge::paint_edge_bar(p, rect, crate::ui::badge::Side::Right, c);
    }
```

把画名字和 user@host 的两处 `p.text(...)` 的 x 坐标从 `rect.left() + 30.0` 改为 `rect.left() + ICON_SLOT_R`，并在图标槽位里画图标。具体：在 `ui.interact(dot_rect, ...)` 那一段之后、第一个 `p.text` 之前插入：

```rust
    // F61:图标槽位。**恒占**,画不画都留这 16px —— 有图标的行和没图标的行
    // 文字左边界必须对齐,否则列表看起来像坏了。
    if let Some(icon) = &appearance.icon {
        crate::ui::badge::paint_icon(
            p,
            egui::Rect::from_center_size(
                egui::pos2(rect.left() + ICON_SLOT_X, rect.center().y),
                egui::vec2(ICON_PX, ICON_PX),
            ),
            icon,
            crate::ui::badge::should_paint(
                appearance,
                mullion_store::model::ColorTarget::ListItem,
            ),
            t,
        );
    }
```

在文件顶部 `const ROW_H` 之后加三个常量：

```rust
/// 图标槽位中心距行左边缘（逻辑点）。状态点在 16，图标紧随其后。
const ICON_SLOT_X: f32 = 38.0;
/// 图标边长。
const ICON_PX: f32 = 16.0;
/// 文字左边界。= 图标槽位右沿 + 8px 间距，**恒定**（见 `session_row` 注释）。
const TEXT_X: f32 = ICON_SLOT_X + ICON_PX / 2.0 + 8.0;
```

把两处 `rect.left() + 30.0` 改为 `rect.left() + TEXT_X`。

**3b.** 同文件 `show` 与 `row` 加参数透传。

`show` 签名（第 169-176 行）改为：

```rust
pub(super) fn show(
    ui: &mut Ui,
    t: &Theme,
    ui_state: &mut UiState,
    sessions: &[SessionRecord],
    groups: &[GroupRecord],
    connected: Option<SessionId>,
    appearance: &crate::ui::badge::AppearanceCache,
) {
```

`show` 内部调 `row(...)` 处补一个参数 `appearance,`（放在 `connected,` 之后）。

`row` 签名改为：

```rust
fn row(
    ui: &mut Ui,
    t: &Theme,
    ui_state: &mut UiState,
    rec: &SessionRecord,
    connected: Option<SessionId>,
    appearance: &crate::ui::badge::AppearanceCache,
    pending_delete_target: Option<SessionId>,
    pending_delete_rendered: &mut bool,
) {
```

`row` 里调 `session_row` 处改为：

```rust
    let selected = ui_state.editor_id == Some(rec.id);
    // 缓存里没有这条(store 刚删掉、或还没 rebuild)就按「没设外观」画。
    let default_appearance = crate::ui::badge::Appearance::default();
    let a = appearance.get(rec.id).unwrap_or(&default_appearance);
    let resp = session_row(ui, t, rec, selected, connected == Some(rec.id), a);
```

**3c.** `crates/mullion-app/src/ui/session_manager/mod.rs` —— `show` 加参数（第 259-268 行签名）：

在 `presence: SecretPresence,` 之后加一行 `appearance: &crate::ui::badge::AppearanceCache,`，并在内部调用 `list::show(...)` 处把它传下去。

**3d.** `crates/mullion-app/src/ui/mod.rs` —— `UiFrame` 加字段。

在 `pub automation: Option<&'a str>,`（第 227 行）之后加：

```rust
    /// F61/F62:已解析的会话外观。**必须是缓存**——`inherit::resolve` 不得进
    /// 渲染热路径(陷阱 T3),见 `badge::AppearanceCache` 的文档注释。
    pub appearance: &'a badge::AppearanceCache,
```

在 `build_ui` 里调 `session_manager::show(...)` 处（第 288-297 行）末尾补 `frame.appearance,`。

**3e.** `crates/mullion-app/src/app.rs` —— 维护缓存。

在 `App` 结构体里 `connected_session` 字段（第 192 行）之后加：

```rust
    /// F61/F62:会话外观的解析缓存。**只在会话/分组变更后 rebuild**,
    /// 绝不在渲染里现算(陷阱 T3,见 `ui::badge::AppearanceCache`)。
    appearance: crate::ui::badge::AppearanceCache,
```

在构造 `App` 处（第 257 行 `connected_session: None,` 附近）加：

```rust
            appearance: Default::default(),
```

在构造 `UiFrame` 处（第 1539 行起）补一行：

```rust
                                appearance: &self.appearance,
```

加一个私有方法（放在 `apply_save` 那个自由函数**之前**、`impl App` 块内任意位置皆可，建议紧挨着 `spawn_connect`）：

```rust
    /// 重算会话外观缓存（F61/F62）。
    ///
    /// **每一处改动了会话或分组的地方都必须调它。** 漏掉一处的症状是：用户改了
    /// 颜色、保存，列表和 pane 标题条却还是旧色，直到重启才更新——一个没有报错、
    /// 只是「看起来没生效」的 bug，最难查。
    ///
    /// 反过来也不能图省事每帧调：`inherit::resolve` 的文档注释点名了陷阱 T3
    /// （喂数据和重绘没解耦），会话列表每帧几十行，逐行解析就是每秒几千次。
    fn refresh_appearance(&mut self) {
        match self.store.as_ref() {
            Some(s) => {
                // `list()` 而不是 `sessions()`——store 的会话访问器叫 `list`。
                let (sessions, groups) = (s.list().to_vec(), s.groups().to_vec());
                self.appearance.rebuild(&sessions, &groups);
            }
            None => self.appearance.rebuild(&[], &[]),
        }
    }
```

> `to_vec()` 是为了断开对 `self.store` 的不可变借用——`rebuild` 要 `&mut self.appearance`，两者同属 `self`，跨方法边界的借用会退化成整体借用。会话数量是几十条量级、只在保存/删除时走一次，这份拷贝不在任何热路径上。

**调用点恰好两处**（行号已核实）：

**① store 首次加载后** —— 第 929 行 `};`（`self.store = match dir { ... };` 这条语句的分号）之后、`// CLI 直连(路径①)` 注释之前，插入：

```rust
        // 启动时先算一次,否则第一次打开会话管理器全是无色。
        self.refresh_appearance();
```

**② 会话/分组变更施加完之后** —— `handle_deferred` 里那一段意图施加代码。改法分两步：

先把第 1697-1702 行那个 diag 判断改为**同时**记下「本帧是否碰了 store」：

```rust
                // self.active/self.ws/self.ui 的借用都已释放,才能拿 `&mut
                // self.store`(egui 闭包里借不到它,只能在这里事后统一施加)。
                //
                // `touched_store` 一并在这里算:F61/F62 的外观缓存要在会话/分组
                // 变更后重算,而三个 `take()` 之后就问不出「刚才有没有意图」了。
                let touched_store = self.ui.delete_request.is_some()
                    || self.ui.save_request.is_some()
                    || self.ui.group_intent.is_some();
                if self.ui.delete_request.is_some() || self.ui.save_request.is_some() {
                    // keyring/TOML 是同步 IO,在事件回调里可能阻塞(Windows 凭据管理器
                    // 偶发几百 ms),打点让看门狗能指认。
                    diag::mark(diag::Stage::StoreIo);
                }
```

再在第 1748 行（`if let Some(intent) = self.ui.group_intent.take() { ... }` 这个 `if let` 块的右花括号）之后、`// 「选择…」私钥文件` 注释之前，插入：

```rust
                // F61/F62:会话增删改、分组增删改名都可能改变外观继承链
                // (删掉分组 → 会话回落到自己那一层),缓存跟着重算。
                //
                // **必须门控在 `touched_store` 后面**:这段每帧都跑,无条件调
                // `refresh_appearance` 就是每帧对所有会话重跑 `inherit::resolve`
                // —— 正是这个缓存要防的陷阱 T3。
                //
                // 不管成功还是失败都重算:失败路径上 store 可能已经改了一半
                // (比如 `delete` 成功但 `save` 失败),按实际状态重算才是对的。
                if touched_store {
                    self.refresh_appearance();
                }
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p mullion-app --lib session_manager::list 2>&1 | tail -20
```
Expected: `test result: ok.`，含 `list_row_paints_an_edge_bar_when_apply_to_includes_list_item` 与 `list_row_paints_nothing_when_apply_to_excludes_list_item`，且原有 6 条 list 测试仍全绿。

```bash
cargo test -p mullion-app 2>&1 | tail -5
```
Expected: 全绿（编译错误在这一步会全部暴露：`UiFrame` 新字段会让所有构造点报错）

- [ ] **Step 5: 提交**

```bash
cd /data/Mullion
git add crates/mullion-app/src/ui/session_manager/list.rs \
        crates/mullion-app/src/ui/session_manager/mod.rs \
        crates/mullion-app/src/ui/mod.rs \
        crates/mullion-app/src/app.rs
git commit -m "feat(app): 会话列表行的语义色竞色条 + 图标槽位 (F61/F62)

竖条走**右**边缘:左 3px 已经是选中态 accent 条,两者各占一边才不会
在选中一条标了色的会话时分不出哪条是哪条。

图标槽位恒占 16px,没设图标的行也留白 —— 否则有图标和没图标的行
文字左边界参差,列表看起来像坏了。

外观缓存在会话/分组变更后 rebuild,不进渲染热路径(T3)。"
```

---

## Task 6: pane 标题条落点（`pane_title.rs`）

**Files:**
- Modify: `crates/mullion-app/src/shell/workspace/mod.rs`
- Modify: `crates/mullion-app/src/ui/pane_title.rs`
- Modify: `crates/mullion-app/src/app.rs`

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-app/src/ui/pane_title.rs` 的 `mod tests` 末尾追加：

```rust
    fn count_shapes(shapes: &[egui::epaint::ClippedShape]) -> usize {
        fn walk(s: &egui::Shape) -> usize {
            match s {
                egui::Shape::Vec(v) => v.iter().map(walk).sum(),
                egui::Shape::Noop => 0,
                _ => 1,
            }
        }
        shapes.iter().map(|cs| walk(&cs.shape)).sum()
    }

    fn appearance_with(targets: Vec<mullion_store::model::ColorTarget>)
        -> crate::ui::badge::Appearance
    {
        crate::ui::badge::Appearance {
            icon: None,
            color: Some(mullion_store::model::ColorSpec {
                hex: "#e06767".into(),
                apply_to: targets,
            }),
        }
    }

    fn run_title(appearance: Option<&crate::ui::badge::Appearance>) -> usize {
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(1.0);
        let views = [TitleView {
            geom: geom_800x600_title32(1),
            index: 1,
            host: Some("dev@build-01"),
            status: PaneStatus::Live,
            focused: true,
            appearance,
        }];
        let out = ctx.run(Default::default(), |ctx| {
            show(ctx, &crate::theme::MULLION_DARK, &views);
        });
        count_shapes(&out.shapes)
    }

    /// F62:勾了「pane 标题条」的会话,标题条左边缘要多一条竖条。
    ///
    /// 自证会变红:把 `show()` 里那段 `should_paint(.., PaneTitle)` 的绘制
    /// 注释掉,这条立刻报两者图形数相等。
    #[test]
    fn pane_title_paints_an_edge_bar_when_apply_to_includes_pane_title() {
        use mullion_store::model::ColorTarget;
        let none = run_title(None);
        let with = run_title(Some(&appearance_with(vec![ColorTarget::PaneTitle])));
        assert!(
            with > none,
            "勾了「pane 标题条」的会话应该多画一条竖条(无 {none} 个图形，有 {with} 个)"
        );
    }

    /// 没勾就不画。
    #[test]
    fn pane_title_paints_nothing_when_apply_to_excludes_pane_title() {
        use mullion_store::model::ColorTarget;
        let none = run_title(None);
        let other = run_title(Some(&appearance_with(vec![ColorTarget::ListItem])));
        assert_eq!(other, none, "只勾了会话列表的会话不该在 pane 标题条上画");
    }

    /// **本任务最关键的回归**:加了竖条和图标之后,`Area` 的几何承诺不能变。
    ///
    /// `pane_title.rs` 顶部注释警告过两个越界坑(`Frame` 的 `min_rect + margin`
    /// 撑破 `Area`、`set_min_size` 只设下限)。竖条用 painter 直接画在已经
    /// `allocate_rect(full, ..)` 的矩形里、不新增任何 widget,就是为了绕开
    /// 它们 —— 这条测试钉死这个前提在有外观的情况下依然成立。
    ///
    /// 自证会变红:把竖条改成用 `ui.allocate_exact_size` 之类参与布局的方式
    /// 画,`Area` 就会被撑宽,这条立刻报宽度对不上。
    #[test]
    fn area_rect_stays_exact_even_with_appearance_bar_and_icon() {
        use crate::shell::workspace::TITLE_BAR_PX;
        use mullion_store::model::{ColorTarget, IconKind, IconSpec};
        let a = crate::ui::badge::Appearance {
            icon: Some(IconSpec {
                kind: IconKind::Builtin,
                value: "hexagon".into(),
            }),
            color: Some(mullion_store::model::ColorSpec {
                hex: "#e06767".into(),
                apply_to: vec![ColorTarget::PaneTitle],
            }),
        };
        for ppp in [1.0f32, 1.25, 1.5] {
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(ppp);
            let views = [TitleView {
                geom: geom_800x600_title32(1),
                index: 1,
                host: Some("dev@build-01"),
                status: PaneStatus::Live,
                focused: true,
                appearance: Some(&a),
            }];
            let _ = ctx.run(Default::default(), |ctx| {
                show(ctx, &crate::theme::MULLION_DARK, &views);
            });
            let rect = ctx
                .memory(|m| m.area_rect(area_id(PaneId(1))))
                .unwrap_or_else(|| panic!("ppp={ppp}: 标题条没画出任何 Area"));
            assert!(
                (rect.width() - 800.0 / ppp).abs() < 0.5,
                "ppp={ppp}: 加了外观后 Area 宽 {} 撑出了 title_px",
                rect.width()
            );
            assert!(
                (rect.height() - TITLE_BAR_PX as f32 / ppp).abs() < 0.5,
                "ppp={ppp}: 加了外观后 Area 高 {} 撑出了 title_px",
                rect.height()
            );
        }
    }
```

同时把 `mod tests` 里已有的三处 `TitleView { ... }` 构造（`area_rect_matches_title_px_exactly_across_dpi_scales`、`long_host_name_does_not_push_area_past_title_px`、`title_bar_off_draws_no_area`）各补一行 `appearance: None,`。

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app --lib pane_title 2>&1 | tail -20
```
Expected: 编译失败，`struct TitleView has no field named appearance`

- [ ] **Step 3: 写实现**

**3a.** `crates/mullion-app/src/ui/pane_title.rs` —— `TitleView` 加字段（第 13-21 行）：

```rust
pub struct TitleView<'a> {
    pub geom: PaneGeom,
    /// 该 pane 在几何顺序中的序号,从 1 起。
    pub index: usize,
    /// 主机标签(会话名或 user@host)。尚未连上时给 `None`。
    pub host: Option<&'a str>,
    pub status: PaneStatus,
    pub focused: bool,
    /// F61/F62:这个 pane 所属会话的已解析外观。`None` = 没有对应会话记录
    /// (快速连接、或 store 不可用)。**必须来自 `badge::AppearanceCache`**,
    /// 不许在这里现解析(陷阱 T3)。
    pub appearance: Option<&'a crate::ui::badge::Appearance>,
}
```

**3b.** 同文件 `show` 里画竖条与图标。

在 `ui.painter().rect(full, 0.0, ...)` 那一段之后（`let inner = full.shrink2(...)` 之前）插入：

```rust
                // F62:语义色竖条走左边缘。**用 painter 直接画在 `full` 里**,
                // 不新增任何 widget、不参与布局计算 —— 这是绕开本文件顶部
                // 那两个越界坑的做法(`Frame` 的 min_rect+margin 撑破 Area、
                // `set_min_size` 只设下限)。守护:
                // `area_rect_stays_exact_even_with_appearance_bar_and_icon`。
                let bar_color = v.appearance.and_then(|a| {
                    crate::ui::badge::should_paint(
                        a,
                        mullion_store::model::ColorTarget::PaneTitle,
                    )
                });
                if let Some(c) = bar_color {
                    crate::ui::badge::paint_edge_bar(
                        ui.painter(),
                        full,
                        crate::ui::badge::Side::Left,
                        c,
                    );
                }
```

在 `ui.colored_label(theme::c32(dot), "●");` **之前**插入图标（同样不参与外层几何——它在 `content` 这个 `new_child` 里，已被 `set_clip_rect(inner)` 裁住）：

```rust
                        // F61:图标画在状态点之前。`content` 是个
                        // `new_child` + `set_clip_rect`,画多了只会被裁掉,
                        // 不会把 `Area` 撑大(见函数文档注释)。
                        if let Some(icon) = v.appearance.and_then(|a| a.icon.as_ref()) {
                            let (r, _) = ui.allocate_exact_size(
                                egui::vec2(14.0, 14.0),
                                egui::Sense::hover(),
                            );
                            crate::ui::badge::paint_icon(ui.painter(), r, icon, bar_color, t);
                        }
```

**3c.** `crates/mullion-app/src/shell/workspace/mod.rs` —— `HostConn` 加字段（第 68-76 行）：

在 `pub addr: String,` 之后加：

```rust
    /// 这条连接来自哪条会话记录（F61/F62 外观要按它查缓存）。
    /// `None` = 快速连接或 store 不可用。
    pub session_id: Option<mullion_store::SessionId>,
```

**3d.** `crates/mullion-app/src/app.rs` —— 填两处。

构造 `HostConn` 处（第 990-1000 行）在 `handle,` 之前加：

```rust
                    // 与下面 `self.connected_session` 同源：都取发起这次连接时
                    // 记下的那条会话（`ConnectOk` 不带 SessionId，见 UserEvent）。
                    session_id: self.ui.connect_request_last,
```

构造 `TitleView` 处（第 1522-1533 行）在 `focused: ...,` 之后加：

```rust
                                            // 一条连接一个会话（ADR-009：多 pane
                                            // 共用一条 SSH 连接，`host_ix` 目前恒 0）。
                                            // 将来 F36 多标签页带来多 host 时，这里
                                            // 按 host 各查各的，不用改结构。
                                            appearance: ws
                                                .pane(g.id)
                                                .and_then(|p| ws.hosts.get(p.host_ix))
                                                .and_then(|h| h.session_id)
                                                .and_then(|sid| self.appearance.get(sid)),
```

> **借用检查提示**：`self.appearance.get(...)` 借了 `&self.appearance`，而 `titles` 随后被放进 `UiFrame` 一起交给 `render_frame`（它要 `&mut self.ui`）。`self.ui` 与 `self.appearance` 是不同字段，同一函数体内的字段级借用是允许的。若编译器仍报错（跨方法调用会退化成整体借用），把 `render_frame` 的 `&mut self` 改为显式传入 `&mut self.ui` 与 `&self.appearance` 两个参数。

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p mullion-app --lib pane_title 2>&1 | tail -20
```
Expected: `test result: ok.`，含 3 条新测试 + 原有 6 条（尤其 `area_rect_matches_title_px_exactly_across_dpi_scales` 必须仍绿）

- [ ] **Step 5: 提交**

```bash
cd /data/Mullion
git add crates/mullion-app/src/ui/pane_title.rs \
        crates/mullion-app/src/shell/workspace/mod.rs \
        crates/mullion-app/src/app.rs
git commit -m "feat(app): pane 标题条的语义色竖条 + 图标 (F61/F62)

竖条用 painter 直接画在已 allocate_rect 的 full 矩形里,不新增 widget、
不参与布局计算 —— 绕开本文件顶部警告的两个越界坑(Frame 的
min_rect+margin 撑破 Area、set_min_size 只设下限)。

守护测试 area_rect_stays_exact_even_with_appearance_bar_and_icon 在
100/125/150% 三档 DPI 下钉死几何承诺不变;原有
area_rect_matches_title_px_exactly_across_dpi_scales 仍绿。

HostConn 加 session_id:pane 要按会话查外观缓存,而 ConnectOk 不带
SessionId,取的是发起连接时记下的那条(与 connected_session 同源)。"
```

---

## Task 7: 状态栏落点（`chrome.rs`）

**Files:**
- Modify: `crates/mullion-app/src/ui/chrome.rs`
- Modify: `crates/mullion-app/src/ui/mod.rs`

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-app/src/ui/chrome.rs` 的 `mod tests` 末尾追加：

```rust
    fn count_shapes(shapes: &[egui::epaint::ClippedShape]) -> usize {
        fn walk(s: &egui::Shape) -> usize {
            match s {
                egui::Shape::Vec(v) => v.iter().map(walk).sum(),
                egui::Shape::Noop => 0,
                _ => 1,
            }
        }
        shapes.iter().map(|cs| walk(&cs.shape)).sum()
    }

    fn run_status(session_color: Option<egui::Color32>) -> usize {
        let ctx = egui::Context::default();
        let out = ctx.run(Default::default(), |ctx| {
            status_bar(
                ctx,
                &crate::theme::MULLION_DARK,
                1,
                true,
                None,
                None,
                session_color,
            );
        });
        count_shapes(&out.shapes)
    }

    /// F62：状态栏的会话色是**画**出来的一个小色块，不是拼进文本的字形。
    ///
    /// 自证会变红：把 `status_bar` 里那段色块绘制删掉，这条立刻报两者相等。
    #[test]
    fn status_bar_paints_a_session_color_block_when_given_one() {
        let none = run_status(None);
        let with = run_status(Some(egui::Color32::from_rgb(0xe0, 0x67, 0x67)));
        assert!(
            with > none,
            "给了会话色就该多画一个色块(无 {none} 个图形，有 {with} 个)"
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app --lib chrome 2>&1 | tail -20
```
Expected: 编译失败，`this function takes 6 arguments but 7 arguments were supplied`

- [ ] **Step 3: 写实现**

`crates/mullion-app/src/ui/chrome.rs` —— `status_bar` 加参数（第 112-119 行签名末尾）：

```rust
    automation: Option<&str>,
    /// F62:当前聚焦 pane 所属会话的语义色。**只有勾了「状态栏」落点才是
    /// `Some`**(过滤在 `badge::should_paint` 里做,这里只负责画)。
    session_color: Option<egui::Color32>,
) {
```

在 `let dot = if connected { t.ok } else { t.fg_faint };` **之前**插入：

```rust
                // F62:会话语义色是**画**出来的一个小竖块,不是拼进 `status_text`
                // 的字形 —— 那条纯函数有 `status_text_carries_no_dot_glyph` 守着
                // 「字形不进字符串」,而它是对的:塞进文本就只能是一个颜色。
                if let Some(c) = session_color {
                    let (r, _) = ui.allocate_exact_size(
                        egui::vec2(crate::ui::badge::EDGE_BAR_W, 12.0),
                        egui::Sense::hover(),
                    );
                    ui.painter().rect_filled(r, egui::Rounding::same(1.5), c);
                }
```

`crates/mullion-app/src/ui/mod.rs` —— `build_ui` 里调 `chrome::status_bar(...)`（第 264-271 行）末尾补一个参数：

```rust
        // F62:状态栏取**当前聚焦 pane**所属会话的色。多 pane 时状态栏该显示
        // 谁的色没有确定答案,所以这个落点默认不勾(见设计 §5),勾了就按聚焦
        // 那个走 —— 焦点是用户当下正在操作的那个 pane,这是唯一有意义的选择。
        frame
            .titles
            .iter()
            .find(|v| v.focused)
            .and_then(|v| v.appearance)
            .and_then(|a| {
                badge::should_paint(a, mullion_store::model::ColorTarget::StatusBar)
            }),
    );
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p mullion-app --lib chrome 2>&1 | tail -20
```
Expected: `test result: ok.`，5 条 chrome 测试全绿，尤其 `status_text_carries_no_dot_glyph`。

- [ ] **Step 5: 提交**

```bash
cd /data/Mullion
git add crates/mullion-app/src/ui/chrome.rs crates/mullion-app/src/ui/mod.rs
git commit -m "feat(app): 状态栏的会话语义色块 (F62)

色块是画出来的,不拼进 status_text —— 那条纯函数有
status_text_carries_no_dot_glyph 守着「字形不进字符串」,塞进文本
就只能是一个颜色。回归测试仍绿。

多 pane 时取当前聚焦 pane 所属会话的色:焦点是用户当下在操作的那个,
这是唯一有意义的选择。该落点默认不勾。"
```

---

## Task 8: 编辑器「外观」section（`fields.rs` + `buffer.rs`）

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/buffer.rs`
- Modify: `crates/mullion-app/src/ui/session_manager/fields.rs`

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-app/src/ui/session_manager/buffer.rs` 的 `mod tests` 末尾追加（若该文件没有 `mod tests`，在文件末尾新建一个）：

```rust
    use mullion_store::model::{ColorSpec, ColorTarget};

    /// F62:勾选框只增删**指定的那一个**落点。
    ///
    /// 编辑器只展示会话列表 / pane 标题条 / 状态栏三个勾选框,但 `apply_to`
    /// 里可能还有 `ColorTarget::Tab`(F36 标签页,排在 v0.5,UI 上没有对应
    /// 勾选框)。如果按「勾了什么存什么」重建整个 `apply_to`,用户随便改一下
    /// 勾选、保存,那个 `Tab` 就被静默剥掉了 —— 违背设计 §2「读到旧值不报错、
    /// 不清除」,而且用户完全看不出发生了什么。
    ///
    /// 自证会变红:把 `set_color_target` 改成「清空后按当前勾选重建」的写法,
    /// 这条立刻报 `Tab` 不见了。
    #[test]
    fn set_color_target_preserves_targets_the_ui_does_not_show() {
        let mut spec = ColorSpec {
            hex: "#e06767".into(),
            apply_to: vec![ColorTarget::Tab, ColorTarget::ListItem],
        };
        // 用户取消勾选「会话列表」、勾上「状态栏」
        set_color_target(&mut spec, ColorTarget::ListItem, false);
        set_color_target(&mut spec, ColorTarget::StatusBar, true);
        assert!(
            spec.apply_to.contains(&ColorTarget::Tab),
            "UI 上没有勾选框的 Tab 必须原样保留,不能被静默剥掉"
        );
        assert!(!spec.apply_to.contains(&ColorTarget::ListItem));
        assert!(spec.apply_to.contains(&ColorTarget::StatusBar));
    }

    /// 重复勾选不产生重复项 —— `apply_to` 是集合语义,存成 `Vec` 只是因为
    /// toml 没有集合类型。
    #[test]
    fn set_color_target_is_idempotent() {
        let mut spec = ColorSpec {
            hex: "#e06767".into(),
            apply_to: vec![],
        };
        set_color_target(&mut spec, ColorTarget::ListItem, true);
        set_color_target(&mut spec, ColorTarget::ListItem, true);
        assert_eq!(spec.apply_to, vec![ColorTarget::ListItem]);
        set_color_target(&mut spec, ColorTarget::ListItem, false);
        set_color_target(&mut spec, ColorTarget::ListItem, false);
        assert!(spec.apply_to.is_empty());
    }

    /// 取消勾选所有落点**不清除颜色**。`ColorSpec { hex, apply_to: [] }` 是
    /// 合法状态 =「色留着,暂时哪都不显示」——与跳板「切到无/继承时链条缓冲
    /// 不清空」同一条原则:用户切走再切回,配的东西还在。
    #[test]
    fn clearing_all_targets_keeps_the_color_itself() {
        let mut spec = ColorSpec {
            hex: "#e06767".into(),
            apply_to: vec![ColorTarget::ListItem],
        };
        set_color_target(&mut spec, ColorTarget::ListItem, false);
        assert!(spec.apply_to.is_empty());
        assert_eq!(spec.hex, "#e06767", "颜色本身必须留着");
    }

    /// 编辑外观必须让表单变脏 —— 否则用户改完颜色直接切到别的会话,改动
    /// 被静默丢弃,连确认框都不弹。`EditorBuffer` derive 了 `PartialEq`,
    /// `preserved_appearance` 是它的字段,所以这是白拿的;这条测试钉死
    /// 「白拿」这件事不会在将来某次重构里被拿走(比如把 appearance 挪进
    /// 一个不参与比对的旁路结构)。
    #[test]
    fn editing_appearance_makes_the_form_dirty() {
        let baseline = EditorBuffer::default();
        let mut buf = baseline.clone();
        buf.preserved_appearance.color = Some(ColorSpec {
            hex: "#e06767".into(),
            apply_to: vec![ColorTarget::ListItem],
        });
        assert!(
            is_dirty(&buf, &baseline),
            "改了外观表单必须判脏,否则切换会话时改动被静默丢弃"
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app --lib session_manager::buffer 2>&1 | tail -20
```
Expected: 编译失败，`cannot find function set_color_target in this scope`

- [ ] **Step 3: 写实现**

**3a.** `crates/mullion-app/src/ui/session_manager/buffer.rs` —— 加纯函数与两个缓冲字段。

在 `is_dirty` 之后插入：

```rust
/// 勾选 / 取消一个颜色落点。
///
/// **只增删指定的那一个**：编辑器只展示会话列表 / pane 标题条 / 状态栏三个
/// 勾选框，而 `apply_to` 里可能还有 `ColorTarget::Tab`（F36 标签页排在 v0.5，
/// UI 上没有对应勾选框）。按「勾了什么存什么」重建整个列表会把它静默剥掉，
/// 违背设计 §2「读到旧值不报错、不清除」。
pub(crate) fn set_color_target(
    spec: &mut mullion_store::model::ColorSpec,
    target: mullion_store::model::ColorTarget,
    on: bool,
) {
    let has = spec.apply_to.contains(&target);
    if on && !has {
        spec.apply_to.push(target);
    } else if !on && has {
        spec.apply_to.retain(|t| *t != target);
    }
}
```

在 `EditorBuffer` 里 `pub preserved_appearance: AppearancePrefs,` 之后加两个缓冲字段：

```rust
    /// 「形状」模式下选中的形状名。**切到别的图标模式时不清空**——同
    /// `jump_chain` 的缓冲逻辑：用户切回来应该看到自己刚才选的，而不是从头再点。
    pub icon_shape_buf: String,
    /// 「emoji」模式下输入的 emoji。切走不清空，理由同上。
    pub icon_emoji_buf: String,
```

`Default` impl 里加：

```rust
            icon_shape_buf: String::new(),
            icon_emoji_buf: String::new(),
```

手写 `Debug` impl 里加（两个都不是敏感字段，直接打）：

```rust
            .field("icon_shape_buf", &self.icon_shape_buf)
            .field("icon_emoji_buf", &self.icon_emoji_buf)
```

`from_record` 里按已存的图标回填缓冲（在 `preserved_appearance: rec.appearance.clone(),` 所在的构造之后、返回之前）：

```rust
        // 把已存的图标回填进对应模式的缓冲，用户切换模式时不会看到空框。
        if let Some(icon) = &rec.appearance.icon {
            match icon.kind {
                mullion_store::model::IconKind::Builtin => {
                    buf.icon_shape_buf = icon.value.clone();
                }
                mullion_store::model::IconKind::Emoji => {
                    buf.icon_emoji_buf = icon.value.clone();
                }
                // Custom 本期不支持编辑（要引 image 解码器），保持缓冲为空；
                // `preserved_appearance` 原样透传，不会因为编辑而丢失。
                mullion_store::model::IconKind::Custom => {}
            }
        }
```

同时把 `preserved_appearance` 上方那段注释里的「UI 目前没有编辑标签/终端偏好/外观偏好的入口」改为「UI 目前没有编辑标签/终端偏好的入口（外观自 P2-a 起可编辑，见 `fields.rs::appearance`）」。

**3b.** `crates/mullion-app/src/ui/session_manager/fields.rs` —— 新开 section。

在 `basic()` 里 `jump(ui, t, buf, groups, sessions, editing);` **之前**插入一行：

```rust
    appearance(ui, t, buf);
```

在 `jump()` 函数之前插入：

```rust
/// F61/F62 外观。放在**「连接」页**「归类」之后:图标和颜色回答的是「这条会话
/// 是谁」,跟名称 / 分组 / 备注同类;埋进「高级」里用户根本找不到,也不值得为它
/// 新开第五个 Tab。
fn appearance(ui: &mut Ui, t: &Theme, buf: &mut EditorBuffer) {
    use mullion_store::model::{ColorSpec, ColorTarget, IconKind, IconSpec};

    section(ui, t, "外观");
    grid(ui, "sm_basic_appearance", |ui| {
        ui.label("图标");
        ui.vertical(|ui| {
            // 三态模式由 `preserved_appearance.icon` 反推,不另存一个 UI 枚举:
            // 多一份状态就多一处会跟真值不同步的地方。两个 `*_buf` 只是切换
            // 模式时不丢内容的缓冲,不是真值来源。
            let mode = match buf.preserved_appearance.icon.as_ref().map(|i| i.kind) {
                None => 0u8,
                Some(IconKind::Builtin) => 1,
                // Custom 本期不支持编辑,在 UI 上落到「无」——但
                // `preserved_appearance` 原样透传,不碰就不会丢。
                Some(IconKind::Emoji) => 2,
                Some(IconKind::Custom) => 0,
            };
            let mut next = mode;
            ui.horizontal(|ui| {
                let vis = &mut ui.visuals_mut().selection;
                vis.bg_fill = crate::theme::c32(t.accent).linear_multiply(0.35);
                ui.selectable_value(&mut next, 0, "无");
                ui.selectable_value(&mut next, 1, "形状");
                ui.selectable_value(&mut next, 2, "emoji");
            });

            match next {
                1 => {
                    // 第一次切到「形状」且没有缓冲时给个默认,免得选了模式却
                    // 什么都没有、看起来像坏了。
                    if buf.icon_shape_buf.is_empty() {
                        buf.icon_shape_buf = crate::ui::badge::BUILTIN_SHAPES[0].to_string();
                    }
                    ui.horizontal_wrapped(|ui| {
                        for name in crate::ui::badge::BUILTIN_SHAPES {
                            let selected = buf.icon_shape_buf == name;
                            let (rect, resp) = ui.allocate_exact_size(
                                egui::vec2(24.0, 24.0),
                                egui::Sense::click(),
                            );
                            if selected {
                                ui.painter().rect_filled(
                                    rect,
                                    egui::Rounding::same(6.0),
                                    crate::theme::c32(t.accent).linear_multiply(0.35),
                                );
                            }
                            crate::ui::badge::paint_icon(
                                ui.painter(),
                                rect.shrink(4.0),
                                &IconSpec {
                                    kind: IconKind::Builtin,
                                    value: name.to_string(),
                                },
                                None,
                                t,
                            );
                            if resp.clicked() {
                                buf.icon_shape_buf = name.to_string();
                            }
                        }
                    });
                }
                2 => {
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut buf.icon_emoji_buf)
                                .desired_width(60.0)
                                .hint_text("🔥"),
                        );
                        for e in ["🔥", "🐧", "🗄", "⚙", "🌐", "🔒", "🧪", "📦"] {
                            if ui.small_button(e).clicked() {
                                buf.icon_emoji_buf = e.to_string();
                            }
                        }
                    });
                    if !buf.icon_emoji_buf.is_empty()
                        && !crate::ui::badge::emoji_is_paintable(&buf.icon_emoji_buf)
                    {
                        ui.colored_label(
                            crate::theme::c32(t.warn),
                            format!(
                                "太长了（最多 {} 个字符），这样不会显示",
                                crate::ui::badge::MAX_EMOJI_CHARS
                            ),
                        );
                    }
                    ui.colored_label(
                        crate::theme::c32(t.fg_dimmer),
                        "emoji 显示为黑白剪影（egui 不支持彩色字形）",
                    );
                }
                _ => {}
            }

            // 每帧按当前模式 + 缓冲写回真值。写回而不是只在点击时改,是因为
            // 模式切换、缓冲编辑、预设点击三条路径都会改状态,集中一处写回
            // 才不会漏。
            buf.preserved_appearance.icon = match next {
                1 if !buf.icon_shape_buf.is_empty() => Some(IconSpec {
                    kind: IconKind::Builtin,
                    value: buf.icon_shape_buf.clone(),
                }),
                2 if !buf.icon_emoji_buf.is_empty() => Some(IconSpec {
                    kind: IconKind::Emoji,
                    value: buf.icon_emoji_buf.clone(),
                }),
                // 模式没变且原本是 Custom 时保持原样,不把它抹成 None ——
                // 本期不支持编辑不等于允许静默删除用户的数据。
                0 if mode == 0 => buf.preserved_appearance.icon.take(),
                _ => None,
            };
        });
        ui.end_row();

        ui.label("颜色");
        ui.vertical(|ui| {
            ui.horizontal_wrapped(|ui| {
                for (name, hex, usage) in crate::theme::LABEL_PALETTE {
                    let selected = buf
                        .preserved_appearance
                        .color
                        .as_ref()
                        .is_some_and(|c| c.hex.eq_ignore_ascii_case(hex));
                    let (rect, resp) =
                        ui.allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::click());
                    if selected {
                        ui.painter().rect_stroke(
                            rect,
                            egui::Rounding::same(6.0),
                            egui::Stroke::new(2.0, crate::theme::c32(t.fg)),
                        );
                    }
                    if let Some(rgb) = crate::theme::parse_hex(hex) {
                        ui.painter().circle_filled(
                            rect.center(),
                            7.0,
                            crate::theme::c32(rgb),
                        );
                    }
                    resp.on_hover_text(format!("{name} · {usage}"));
                    if ui
                        .interact(rect, ui.id().with(("palette", hex)), egui::Sense::click())
                        .clicked()
                    {
                        match &mut buf.preserved_appearance.color {
                            Some(c) => c.hex = hex.to_string(),
                            None => {
                                // 新设色时的默认落点:会话列表 + pane 标题条。
                                // 状态栏不默认勾 —— 多 pane 时它该显示谁的色
                                // 没有确定答案(设计 §5)。
                                buf.preserved_appearance.color = Some(ColorSpec {
                                    hex: hex.to_string(),
                                    apply_to: vec![
                                        ColorTarget::ListItem,
                                        ColorTarget::PaneTitle,
                                    ],
                                })
                            }
                        }
                    }
                }
                if ui.button("清除").clicked() {
                    buf.preserved_appearance.color = None;
                }
            });

            if let Some(c) = &mut buf.preserved_appearance.color {
                ui.horizontal(|ui| {
                    ui.label("自定义");
                    ui.add(
                        egui::TextEdit::singleline(&mut c.hex)
                            .desired_width(90.0)
                            .hint_text("#rrggbb"),
                    );
                    if crate::theme::parse_hex(&c.hex).is_none() {
                        ui.colored_label(crate::theme::c32(t.warn), "不是 #rrggbb，不会显示");
                    }
                });
            }
        });
        ui.end_row();

        ui.label("作用于");
        ui.vertical(|ui| {
            match &mut buf.preserved_appearance.color {
                Some(spec) => {
                    for (target, label) in [
                        (ColorTarget::ListItem, "会话列表"),
                        (ColorTarget::PaneTitle, "pane 标题条"),
                        (ColorTarget::StatusBar, "状态栏"),
                    ] {
                        let mut on = spec.apply_to.contains(&target);
                        if ui.checkbox(&mut on, label).changed() {
                            crate::ui::session_manager::set_color_target(spec, target, on);
                        }
                    }
                }
                None => {
                    ui.colored_label(crate::theme::c32(t.fg_dimmer), "先选一个颜色");
                }
            }
        });
        ui.end_row();
    });
}
```

**3c.** `crates/mullion-app/src/ui/session_manager/mod.rs` —— 导出 `set_color_target`。

第 16-19 行的再导出块按字母序插入一项（`secret_fields` 之后）：

```rust
pub(crate) use buffer::{
    build_draft, clear_key, connect_string, import_key_file, is_dirty, merge_secret, secret_fields,
    set_color_target, sync_has_passphrase,
};
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p mullion-app 2>&1 | tail -8
```
Expected: 全绿，含 4 条新 buffer 测试

- [ ] **Step 5: 提交**

```bash
cd /data/Mullion
git add crates/mullion-app/src/ui/session_manager/buffer.rs \
        crates/mullion-app/src/ui/session_manager/fields.rs \
        crates/mullion-app/src/ui/session_manager/mod.rs
git commit -m "feat(app): 会话编辑器「外观」section (F61/F62)

放在「连接」页「归类」之后:图标和颜色回答的是「这条会话是谁」,
与名称/分组/备注同类,不值得新开第五个 Tab。

set_color_target 只增删指定的那一个落点 —— 编辑器只展示三个勾选框,
按「勾了什么存什么」重建会把旧记录里的 ColorTarget::Tab(F36,UI 上
没有勾选框)静默剥掉。守护测试钉死这条。

取消勾选所有落点不清除颜色;图标两个模式各自留缓冲、切换不丢 ——
都与跳板缓冲不清空同一条原则。脏判定白拿(EditorBuffer 已 derive
PartialEq),另加一条测试钉死它不会在将来重构里被拿走。"
```

---

## Task 9: 变异验红 + 全量绿

**Files:** 无（只跑验证）

- [ ] **Step 1: 变异验红 —— `should_paint` 的 `apply_to` 过滤**

临时把 `crates/mullion-app/src/ui/badge.rs` 的 `should_paint` 改成忽略过滤：

```rust
pub fn should_paint(a: &Appearance, _target: ColorTarget) -> Option<egui::Color32> {
    let c = a.color.as_ref()?;
    theme::parse_hex(&c.hex).map(theme::c32)
}
```

```bash
cargo test -p mullion-app --lib badge:: 2>&1 | grep -E "^test |result"
```
Expected: `should_paint_only_where_apply_to_says_so` 与 `empty_apply_to_paints_nowhere_but_is_not_an_error` **FAILED**

**改回去**，重跑确认恢复绿。

- [ ] **Step 2: 变异验红 —— `builtin_shape` 的未知名降级**

临时把 `_ => return None,` 改成 `_ => Shape::Circle,`：

```bash
cargo test -p mullion-app --lib badge:: 2>&1 | grep -E "^test |result"
```
Expected: `unrecognized_icons_paint_nothing` **FAILED**

**改回去**，重跑确认恢复绿。

- [ ] **Step 3: 变异验红 —— 缓存真的存住了**

临时在 `AppearanceCache::rebuild` 的**末尾**加一行：

```rust
        self.map.clear();
```

```bash
cargo test -p mullion-app --lib badge:: 2>&1 | grep -E "^test |result"
```
Expected: `cache_resolves_session_own_appearance`、`cache_falls_back_to_group_appearance`、`session_appearance_overrides_group`、`get_returns_the_cached_value_not_a_fresh_resolve` 四条 **FAILED**（`unwrap()` panic 在空表上）

**改回去**，重跑确认恢复绿。

> 这条验的是「缓存里确实有东西」。而「`get` 不在内部现算」由**类型**保证——`get(&self) -> Option<&Appearance>` 返回的是借用，不可能来自函数内新建的临时值，编译器不让。`get_returns_the_cached_value_not_a_fresh_resolve` 用「改源数据后 `get` 仍返回旧值」把这件事写成可执行的断言，即使将来有人把签名改成返回克隆值，它也会立刻变红。

- [ ] **Step 4: 变异验红 —— `set_color_target` 不剥 Tab**

临时在 `set_color_target` 的**开头**加一行（模拟「清空后按当前勾选重建」那种写法的后果）：

```rust
    spec.apply_to.clear();
```

```bash
cargo test -p mullion-app --lib session_manager::buffer 2>&1 | grep -E "^test |result"
```
Expected: `set_color_target_preserves_targets_the_ui_does_not_show` **FAILED**（断言 `Tab` 仍在时报错）

**改回去**，重跑确认恢复绿。

- [ ] **Step 5: 变异验红 —— pane 标题条几何**

临时把 `pane_title.rs` 里画竖条的 `crate::ui::badge::paint_edge_bar(ui.painter(), full, ...)` 换成参与布局的方式：

```rust
                if let Some(c) = bar_color {
                    let (r, _) = ui.allocate_exact_size(
                        egui::vec2(crate::ui::badge::EDGE_BAR_W, 600.0),
                        egui::Sense::hover(),
                    );
                    ui.painter().rect_filled(r, egui::Rounding::same(2.0), c);
                }
```

```bash
cargo test -p mullion-app --lib pane_title 2>&1 | grep -E "^test |result"
```
Expected: `area_rect_stays_exact_even_with_appearance_bar_and_icon` **FAILED**（Area 被撑高）

**改回去**，重跑确认恢复绿。

- [ ] **Step 6: 全量绿**

```bash
cd /data/Mullion
cargo fmt --all
cargo test --workspace > /tmp/test.log 2>&1; echo "exit=$?"; grep -nE "test result|FAILED|panicked" /tmp/test.log | tail -20
cargo clippy --workspace --all-targets -- -D warnings > /tmp/clippy.log 2>&1; echo "clippy exit=$?"; grep -c "^error" /tmp/clippy.log
cargo fmt --check && echo "fmt ok"
```
Expected: 测试 `exit=0` 且无 FAILED；`clippy exit=0` 且 `error` 计数为 0；`fmt ok`

> **注意**：clippy 退出码不能接 grep 管道读——grep 会吃掉它。必须像上面那样先重定向再单独 `echo $?`。

- [ ] **Step 7: 提交**

```bash
cd /data/Mullion
git add -A
git commit -m "test(app): P2-a 变异验红 5 处 + 全量绿 (F61/F62)

按 subagent-driven-review-lessons 的教训,逐条确认守护测试真的会变红:
apply_to 过滤失效 / 未知形状名不降级 / 缓存被清空 / set_color_target
剥掉 Tab / pane 竖条参与布局撑破 Area。只看测试是绿的不算数。"
```

---

## Task 10: 版本号、交付与发布

> 这是 CLAUDE.md 的「交付约定」默认执行项，不用再问。

**Files:**
- Modify: `Cargo.toml`
- Create: `notes.md`（覆盖上一版内容）

- [ ] **Step 1: 升 patch 版本**

`Cargo.toml` 第 12 行 `version = "0.1.22"` → `version = "0.1.23"`

```bash
cd /data/Mullion
cargo check -p mullion-app 2>&1 | tail -3   # 让 Cargo.lock 跟着更新
git add Cargo.toml Cargo.lock
git commit -m "chore: 版本 0.1.23(会话图标与语义色 F61/F62)"
```

- [ ] **Step 2: 重跑绿（版本改动后再确认一次）**

```bash
cargo test --workspace > /tmp/test.log 2>&1; echo "exit=$?"; grep -nE "test result: FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings > /tmp/clippy.log 2>&1; echo "clippy exit=$?"
```
Expected: 两个 exit 都是 0

- [ ] **Step 3: 交叉编译 + 依赖验收**

```bash
cd /data/Mullion
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app 2>&1 | tail -5
objdump -p target/x86_64-pc-windows-gnu/release/mullion.exe | grep "DLL Name"
```
Expected: **不得**出现 `libgcc_s_seh-1.dll` 或 `libwinpthread-1.dll`（出现即为不合格，按 `docs/cross-compile-windows.md` 修）

- [ ] **Step 4: 写 notes.md**

覆盖 `/data/Mullion/notes.md`，内容包含：

1. 本版做了什么（会话图标 + 语义色，三处落点，编辑器「外观」section）
2. **人工验收清单**（无头容器验不了的）：
   - emoji 显示为**黑白剪影**是否可接受（epaint 不支持彩色字形，不是 bug）
   - 3px 竖条在 125% / 150% DPI 缩放下是否可见
   - CJK 字体回退装上后 emoji 是否被挤掉（`install_cjk_font` 把系统中文字体放在末位回退，emoji 字形的选择顺序需实机确认）
   - 8 个预设色在真实显示器上是否互相可区分（尤其橙/黄、蓝/紫）
   - 选中一条标了紫色的会话，左（accent）右（紫）两条边是否分得出来
   - 8 个内置形状是否画得正确（**凸多边形绕向错会画成空心或消失**，无头测不出来）
   - 多 pane 各配不同颜色时，标题条竖条是否真的帮到辨识
   - 改了颜色保存后，**已经开着的 pane** 标题条是否立刻跟着变
3. sha256
4. 首次运行提示（`Unblock-File .\mullion.exe`）

- [ ] **Step 5: 发布**

```bash
cd /data/Mullion
cp target/x86_64-pc-windows-gnu/release/mullion.exe .
sha256sum mullion.exe > mullion.exe.sha256
cat mullion.exe.sha256   # 把这个值填进 notes.md 的「校验」段
HTTPS_PROXY=http://127.0.0.1:7890 gh release create v0.1.23 \
  mullion.exe mullion.exe.sha256 -t "v0.1.23" -F notes.md --repo kilobitcy/Mullion
```

> **Release 标题只能是纯版本号 `v0.1.23`**，不带破折号、不带摘要、不带 emoji。想说的话全写进 notes 正文。
> 本机 DNS 解析不了 github，`gh` 必须带 `HTTPS_PROXY`。

- [ ] **Step 6: 报给用户**

Release 链接 + sha256 + 人工验收清单。

---

## 附：spec 编号对照

| 编号 | 在哪实现 | 守护测试 |
|---|---|---|
| F61 图标 | `badge.rs::builtin_shape` / `paint_icon`，`fields.rs::appearance` | `every_builtin_shape_actually_paints_something`、`unrecognized_icons_paint_nothing`、`emoji_length_limit_admits_zwj_sequences_and_rejects_prose` |
| F62 语义色 | `theme.rs::LABEL_PALETTE` / `parse_hex`，`badge.rs::should_paint` / `paint_edge_bar`，三处落点 | `label_palette_contrasts_at_least_3_to_1_against_panel_bg`、`palette_purple_differs_from_accent_...`、`should_paint_only_where_apply_to_says_so`、`list_row_paints_an_edge_bar_...`、`pane_title_paints_an_edge_bar_...`、`status_bar_paints_a_session_color_block_...` |
| 陷阱 T3（缓存不进热路径） | `badge.rs::AppearanceCache` | `get_returns_the_cached_value_not_a_fresh_resolve` |
| `pane_title.rs` 两个越界坑 | `paint_edge_bar` 走 painter | `area_rect_stays_exact_even_with_appearance_bar_and_icon`、`area_rect_matches_title_px_exactly_across_dpi_scales`（回归） |
| 「字形不进字符串」 | `chrome.rs` 色块用 painter | `status_text_carries_no_dot_glyph`（回归） |
