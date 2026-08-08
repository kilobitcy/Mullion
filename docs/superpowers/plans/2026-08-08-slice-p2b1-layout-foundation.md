# 会话管理器 UI 走查 · 阶段 1「布局与视觉地基」实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修掉会话管理器右栏「输入框吃光整行 → 右侧附属控件被裁到面板外」这个 P0 缺陷，并建立后三阶段共用的尺寸/间距/图标地基。

**Architecture:** 新增两个零依赖的纯工具模块 —— `ui/metrics.rs`（尺寸刻度 + 宽度计算纯函数）与 `ui/icon.rs`（epaint 自绘控制图标，`shapes()` 为纯函数以便几何断言）；然后把 `session_manager` 里所有 `desired_width(f32::INFINITY)` 与硬编码 `80.0` 换成 `metrics` 提供的语义档位，把跳板链那三个字符按钮换成自绘图标，并把「高级」页合并进「连接」页（代理属于连接方式）。不动 SSH / 跳板 / 存储逻辑，不动 `sessions.toml` 结构。

**Tech Stack:** Rust 2021 / egui 0.30 / epaint 0.30 / winit 0.30 / wgpu 23。全部改动落在 `crates/mullion-app`。

---

## 前置约定（执行者必读）

1. **不升版本号、不发 Release。** 走查的 4 个阶段共用一个 `v0.1.25`，版本 bump 与交叉编译放在阶段 4 完成后一次性做（CLAUDE.md「交付约定」）。阶段 1 结束时只要求 `cargo test --workspace` 全绿 + `clippy -D warnings` 无输出 + `fmt --check` 通过。
2. **提交粒度。** 本计划每个 Task 末尾各自提交（TDD 纪律 + CLAUDE.md「一次提交只做一件事」）。阶段全部完成后，按项目惯例把本阶段 squash 成一个 `feat(ui): 会话管理器布局与视觉地基（走查 1/2/5/7/8/17）` 提交再入 main。
3. **架构不变量。** 本阶段所有新代码都在 `mullion-app`，不向 `core/term/ssh/store` 添加任何东西。`ui/metrics.rs` 与 `ui/icon.rs` 不得 `use crate::shell::*` 或任何 store 类型 —— 它们必须是能脱离窗口单测的纯模块。
4. **无头环境的边界。** 「125%/150% DPI 下截图无错位」「字形是否对齐」属于 CLAUDE.md 里「你无法验证的东西」。本计划写的 DPI 测试只能守住 egui 的**布局矩形**不越界，不能证明渲染观感。交付时必须如实标注为待人工验收项。

---

## 现状锚点（改之前先确认这些行还在）

| 位置 | 现状 | 本阶段动作 |
|---|---|---|
| `session_manager/fields.rs:13-19` | `grid()`，`min_col_width(88.0)` | 换成 `metrics::LABEL_COL_W` |
| `session_manager/fields.rs:23-31` | `section()`，`add_space(10.0)` / `add_space(4.0)` | 换成刻度值 + 细分隔线 |
| `session_manager/fields.rs:133,137` | 名称 / 主机 `desired_width(f32::INFINITY)` | `FIELD_W_M` |
| `session_manager/fields.rs:141` | 端口 `desired_width(80.0)` | `FIELD_W_S` |
| `session_manager/fields.rs:176-180` | 备注 multiline，标签垂直居中错位 | `FIELD_W_L` + 标签顶对齐 |
| `session_manager/fields.rs:234-237` | 「emoji 显示为黑白剪影(egui 不支持彩色字形)」 | 中性文案 |
| `session_manager/fields.rs:438-451` | 跳板链 `✕ ↓ ↑` 三个字符按钮，无 tooltip | 自绘图标 + tooltip |
| `session_manager/fields.rs:503` | 用户名 `INFINITY` | `FIELD_W_M` |
| `session_manager/fields.rs:640-682` | `network()`，「高级」页唯一内容 | 并入 `basic()` |
| `session_manager/fields.rs:659,661,666` | 代理地址 / 端口 / 用户 | `FIELD_W_M` / `FIELD_W_S` / `FIELD_W_M` |
| `session_manager/mod.rs:69` | `TAB_ADVANCED: usize = 2` | 删除，`TAB_AUTOMATION`→2、`TAB_APPEARANCE`→3 |
| `session_manager/mod.rs:559-600` | `secret_edit()`，三个分支全 `INFINITY` | 预留右侧宽度 |
| `session_manager/editor.rs:15` 附近 | `TABS: [&str; 5]` | `[&str; 4]` |
| `session_manager/editor.rs:386-397` | tab 分派 | 去掉 `TAB_ADVANCED` 分支 |
| `session_manager/list.rs:214` | 搜索框 `hint_text` | 走 `theme::hint_text()` |
| `theme.rs:229` `apply_egui` | 没设过 `weak_text_color` 相关项 | 不改 `apply_egui`，新增 `hint_text()` 包装 |

---

## 文件结构

**新建**

- `crates/mullion-app/src/ui/metrics.rs` —— 表单尺寸档位、间距刻度、宽度计算纯函数 `field_w()` / `button_reserve()`。零 egui 布局副作用（`field_w` 完全纯，`button_reserve` 只读 `Ui` 的字体与 spacing）。
- `crates/mullion-app/src/ui/icon.rs` —— 控制图标。`shapes(rect, glyph, stroke) -> Vec<egui::Shape>` 为纯函数；`icon_button(ui, glyph, enabled, tooltip) -> bool` 是它的 `Ui` 包装。

**修改**

- `crates/mullion-app/src/ui/mod.rs` —— 加两行 `pub mod`。
- `crates/mullion-app/src/theme.rs` —— 新增 `hint_text()` + 两个对比度测试。
- `crates/mullion-app/src/ui/session_manager/mod.rs` —— Tab 常量表、`secret_edit()`。
- `crates/mullion-app/src/ui/session_manager/fields.rs` —— `grid()`/`section()`/`basic()`/`network()`/`chain_editor()`/`auth()`/`appearance()` 的宽度与图标。
- `crates/mullion-app/src/ui/session_manager/editor.rs` —— `TABS`、tab 分派、常量表测试。
- `crates/mullion-app/src/ui/session_manager/list.rs` —— 搜索框 hint 走 `theme::hint_text()`。

---

## Task 1: `ui/metrics.rs` —— 尺寸刻度与宽度计算

**为什么先做这个：** 走查 P0-1（附属控件被裁）与 P0-2（输入框宽度失控）本质是同一个缺陷 —— 全项目没有任何宽度语义，只有 `f32::INFINITY` 和散落的 `80.0`。先把计算逻辑做成纯函数，后面三个 Task 才有东西可用，且这块唯一的真逻辑（「先扣预留、再取上限、再夹下界」）能在没有窗口的情况下被测死。

**关键设计：`field_w` 是「上限」不是「定宽」。** 右栏宽度随分隔条可拖：默认 880 宽窗口下右栏内容宽约 440px，分隔条拖到 `LIST_MAX_W=440` 时只剩约 300px。写死 480px（走查原文的建议值）在拖窄后必然溢出，等于把 P0-1 换个地方复发。

**Files:**
- Create: `crates/mullion-app/src/ui/metrics.rs`
- Modify: `crates/mullion-app/src/ui/mod.rs:2-9`

- [ ] **Step 1: 先写失败的测试**

新建 `crates/mullion-app/src/ui/metrics.rs`，**只写测试模块**（实现留到 Step 3）：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// 走查 P0-1 的根治点。老写法是 `desired_width(f32::INFINITY)`:输入框
    /// 吃光整行,跟在它后面的「撤销」「已设置」被推到面板外只露半个字。
    /// `reserve` 必须先从可用宽里扣掉,附属控件才有地方站。
    #[test]
    fn reserve_is_subtracted_before_the_cap_so_a_trailing_button_always_fits() {
        // 可用 440,预留 60 给「撤销」,上限 320 → 380 被上限压到 320,
        // 剩 120 > 60,按钮站得下。
        assert_eq!(field_w(440.0, FIELD_W_M, 60.0), FIELD_W_M);
        // 可用 300(分隔条拖到 LIST_MAX_W 时的真实值),同样预留 60
        // → 240,没到上限,按上限走就会溢出 60px。
        assert_eq!(field_w(300.0, FIELD_W_M, 60.0), 240.0);
    }

    /// 上限是**上限**,不是定宽。写死一个 480 的定宽,右栏拖窄后一样溢出。
    #[test]
    fn field_w_never_exceeds_available_so_a_dragged_narrow_pane_cannot_clip() {
        for avail in [120.0f32, 200.0, 300.0, 440.0, 900.0] {
            for max in [FIELD_W_S, FIELD_W_M, FIELD_W_L] {
                let w = field_w(avail, max, 0.0);
                assert!(
                    w <= avail,
                    "avail={avail} max={max} 算出 {w},比可用宽还大 —— 必被裁"
                );
                assert!(w.is_finite(), "FIELD_W_L 是 INFINITY,不能原样漏出去");
            }
        }
    }

    /// 极窄时不能算出 0 或负数:`TextEdit` 收到 0 宽会缩成一条缝,
    /// 用户看到的是「输入框不见了」,比溢出更难排查。
    #[test]
    fn field_w_clamps_to_a_usable_floor_instead_of_collapsing_to_zero() {
        assert_eq!(field_w(40.0, FIELD_W_M, 60.0), FIELD_W_MIN);
        assert_eq!(field_w(0.0, FIELD_W_M, 0.0), FIELD_W_MIN);
    }

    /// 间距刻度必须严格递增且互不相等 —— 这套值的全部用处就是让
    /// 「16 比 12 大一档」在视觉上成立,写重了等于没分档。
    #[test]
    fn spacing_scale_is_strictly_increasing() {
        let scale = [SP_XS, SP_S, SP_M, SP_L, SP_XL];
        for w in scale.windows(2) {
            assert!(w[0] < w[1], "间距刻度 {:?} 不是严格递增", scale);
        }
    }

    /// 短值档必须真的比中值档窄一大截,否则「端口框和主机框一样长」
    /// 这个走查 P0-2 的原始症状根本没被修掉。
    #[test]
    fn short_field_is_meaningfully_narrower_than_medium() {
        assert!(FIELD_W_S * 2.0 < FIELD_W_M);
        assert!(FIELD_W_MIN <= FIELD_W_S);
    }
}
```

- [ ] **Step 2: 跑测试确认它失败**

先在 `crates/mullion-app/src/ui/mod.rs` 的模块声明区（第 2 行 `pub mod badge;` 之前）加一行，否则测试根本不会被编译：

```rust
pub mod icon;
pub mod metrics;
```

（`icon.rs` 在 Task 2 才建，本步先只加 `pub mod metrics;`，Task 2 再补 `pub mod icon;`。）

Run: `cargo test -p mullion-app metrics:: 2>&1 | tail -20`
Expected: FAIL，编译错误 `cannot find function 'field_w' in this scope` / `cannot find value 'FIELD_W_M' in this scope`。

- [ ] **Step 3: 写最小实现**

在 `crates/mullion-app/src/ui/metrics.rs` 的**测试模块之前**插入：

```rust
//! 表单尺寸档位与间距刻度。
//!
//! 存在的理由:走查 P0-1/P0-2。改之前全项目没有任何宽度语义 ——
//! 输入框要么 `desired_width(f32::INFINITY)`(吃光整行,把右边的附属控件
//! 顶出面板),要么散落的硬编码 `80.0`。两者都无法在右栏被拖宽/拖窄时
//! 保持正确。
//!
//! **本模块不得依赖 `crate::shell` 或任何 store 类型**:它的价值就在于
//! `field_w` 是个能脱离窗口单测的纯函数。

/// 输入框的绝对下界。低于这个宽度 `TextEdit` 会缩成一条缝,
/// 用户看到的是「输入框不见了」——比溢出更难排查。
pub const FIELD_W_MIN: f32 = 72.0;

/// 短值:端口、超时、延时。
pub const FIELD_W_S: f32 = 96.0;

/// 中值:名称、主机、用户名、密码、代理地址。
///
/// 320 而不是走查建议的 480:默认 880 宽窗口下右栏内容宽约 440px
/// (880 − 12 窗口边距 − 300 列表宽 − 28 CentralPanel 内边距 = 540,
/// 再减 88 标签列 − 12 列间距 = 440)。480 在默认尺寸下就已经溢出,
/// 分隔条拖到 `LIST_MAX_W` 后右栏只剩约 300px,溢得更狠。
pub const FIELD_W_M: f32 = 320.0;

/// 长文本:备注。撑满可用宽(仍受 `field_w` 的 `reserve` 与下界约束)。
pub const FIELD_W_L: f32 = f32::INFINITY;

/// 两列表单左侧标签列的固定宽度。定宽是为了让各分区的输入框左边缘对齐 ——
/// `Grid::min_col_width` 只是下界,标签变长会把整列推宽,分区之间就错开了。
pub const LABEL_COL_W: f32 = 88.0;

/// 间距刻度。除这五个值外不得在 UI 里出现新的裸间距数字。
pub const SP_XS: f32 = 4.0;
pub const SP_S: f32 = 8.0;
pub const SP_M: f32 = 12.0;
pub const SP_L: f32 = 16.0;
pub const SP_XL: f32 = 24.0;

/// 算一个输入框该有多宽。
///
/// - `available`:`ui.available_width()`,随分隔条拖动实时变。
/// - `max`:语义档位上限(`FIELD_W_S/M/L`)。**是上限不是定宽。**
/// - `reserve`:同一行里跟在输入框后面的附属控件需要的宽度。
///
/// 顺序是「先扣预留 → 再取上限 → 再夹下界」。这三步缺一不可:
/// 不扣预留 = 走查 P0-1(按钮被裁);不取上限 = 走查 P0-2(「LEG」填在
/// 800px 的框里);不夹下界 = 极窄时框塌成缝。
pub fn field_w(available: f32, max: f32, reserve: f32) -> f32 {
    (available - reserve).clamp(FIELD_W_MIN, max.max(FIELD_W_MIN))
}
```

注意 `max.max(FIELD_W_MIN)`：`clamp` 在 `min > max` 时会 panic，而 `FIELD_W_L` 是 `INFINITY`、`FIELD_W_S` 是 96 都大于 72，但这层保护让将来有人把档位调到 72 以下时不会炸。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app metrics:: 2>&1 | tail -20`
Expected: PASS，`test result: ok. 5 passed`。

- [ ] **Step 5: 加 `button_reserve()` 的失败测试**

`reserve` 不能是手写常量：字号、DPI 缩放、按钮内边距一变，手算值就失同步，而且没有任何编译错误会提示。加一个跟着实际字体量的估算器。在 `metrics.rs` 的 `mod tests` 里追加：

```rust
    /// `reserve` 必须跟着真实字体走,不能是手写常量 —— 否则换字号/换
    /// 缩放后 P0-1 原样复发,且没有任何编译错误提示。
    #[test]
    fn button_reserve_tracks_the_actual_label_width() {
        let ctx = egui::Context::default();
        let mut narrow = 0.0f32;
        let mut wide = 0.0f32;
        ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                narrow = button_reserve(ui, "撤销");
                wide = button_reserve(ui, "撤销撤销撤销撤销");
            });
        });
        assert!(narrow > 0.0, "预留宽不能是 0,那等于没预留");
        assert!(
            wide > narrow * 2.0,
            "标签长 4 倍,预留宽只有 {wide} vs {narrow} —— 没在量真实文字"
        );
    }
```

- [ ] **Step 6: 跑测试确认它失败**

Run: `cargo test -p mullion-app metrics::tests::button_reserve 2>&1 | tail -20`
Expected: FAIL，`cannot find function 'button_reserve' in this scope`。

- [ ] **Step 7: 实现 `button_reserve()`**

在 `metrics.rs` 里 `field_w` 之后追加：

```rust
/// 估算一个文字按钮在当前样式下占多宽,给 `field_w` 的 `reserve` 用。
///
/// 手写常量在这里是错的:按钮宽 = 文字宽 + 2×`button_padding.x`,而文字宽
/// 随字体、字号、DPI 缩放变。egui 自己就是这么算的,这里照抄一遍。
/// 末尾再加一份 `item_spacing.x`,那是输入框与按钮之间的间隙。
pub fn button_reserve(ui: &egui::Ui, label: &str) -> f32 {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let text_w = ui.fonts(|f| {
        f.layout_no_wrap(label.to_owned(), font, egui::Color32::PLACEHOLDER)
            .size()
            .x
    });
    text_w + 2.0 * ui.spacing().button_padding.x + ui.spacing().item_spacing.x
}
```

`Color32::PLACEHOLDER` 在 ecolor 0.30 是 `from_rgba_premultiplied(64, 254, 0, 128)`，只用于排版、不落到屏幕上，`layout_no_wrap` 的返回值只取 `.size().x`。

- [ ] **Step 8: 跑测试确认通过**

Run: `cargo test -p mullion-app metrics:: 2>&1 | tail -20`
Expected: PASS，`test result: ok. 6 passed`。

- [ ] **Step 9: 提交**

```bash
cargo fmt
cargo clippy -p mullion-app --all-targets -- -D warnings
git add crates/mullion-app/src/ui/metrics.rs crates/mullion-app/src/ui/mod.rs
git commit -m "feat(ui): 表单尺寸档位与宽度计算纯函数 (走查 P0-1/P0-2)

新增 ui/metrics.rs:FIELD_W_S/M/L + 间距刻度 + field_w()/button_reserve()。
field_w 的语义是「先扣附属控件预留 → 再取档位上限 → 再夹下界」,三步缺一
就分别复发「按钮被裁」「LEG 填在 800px 框里」「极窄时框塌成缝」。
上限取 320 而非走查建议的 480:默认窗口右栏内容宽仅约 440px,拖窄后约 300px。
本提交只加纯函数,不改任何现有 UI。"
```

---

## Task 2: `ui/icon.rs` —— 自绘控制图标

**为什么需要：** 走查 P0-5 说跳板链的 `□` 看不出是删除。读代码发现代码里写的其实是 `✕`（U+2715）—— **`□` 是缺字形的豆腐块**。`install_cjk_font`（`ui/mod.rs`）只装了 egui 内置拉丁字体 + 微软雅黑，U+2715 两边都没有。按走查建议改成 `✕` 或 `🗑` 会原样重现同一个 bug。唯一稳的做法是不依赖字体，用 epaint 直接画。

**Files:**
- Create: `crates/mullion-app/src/ui/icon.rs`
- Modify: `crates/mullion-app/src/ui/mod.rs`（补 `pub mod icon;`）

- [ ] **Step 1: 先写失败的测试**

新建 `crates/mullion-app/src/ui/icon.rs`，只写测试模块：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, Color32, Rect, Stroke};

    fn r() -> Rect {
        Rect::from_min_max(pos2(10.0, 20.0), pos2(26.0, 36.0))
    }
    fn s() -> Stroke {
        Stroke::new(1.5, Color32::WHITE)
    }

    /// 所有端点都得落在给定矩形内。越界的图标会画到邻居按钮的地盘上,
    /// 而 egui 不会因此报任何错 —— 只有人眼能看出来。
    #[test]
    fn every_glyph_stays_inside_its_rect() {
        for g in [Glyph::Cross, Glyph::ArrowUp, Glyph::ArrowDown] {
            for p in points_of(&shapes(r(), g, s())) {
                assert!(
                    r().contains(p),
                    "{g:?} 的端点 {p:?} 跑出了 {:?}",
                    r()
                );
            }
        }
    }

    /// ↑ 和 ↓ 画反了不会有任何编译错误,也不会有任何 panic ——
    /// 用户点「上移」结果条目往下跑。这是本模块唯一真正会出的 bug。
    #[test]
    fn arrow_up_points_up_and_arrow_down_points_down() {
        let up = points_of(&shapes(r(), Glyph::ArrowUp, s()));
        let down = points_of(&shapes(r(), Glyph::ArrowDown, s()));
        let cy = r().center().y;
        // 尖端:离中心竖直方向最远的那个点。
        let apex_up = up
            .iter()
            .copied()
            .min_by(|a, b| a.y.total_cmp(&b.y))
            .unwrap();
        let apex_down = down
            .iter()
            .copied()
            .max_by(|a, b| a.y.total_cmp(&b.y))
            .unwrap();
        assert!(apex_up.y < cy, "ArrowUp 的尖端在中心线下方,画反了");
        assert!(apex_down.y > cy, "ArrowDown 的尖端在中心线上方,画反了");
        // 尖端必须在水平中线附近,否则画出来是个斜杠不是箭头。
        assert!((apex_up.x - r().center().x).abs() < 1.0);
        assert!((apex_down.x - r().center().x).abs() < 1.0);
    }

    /// 叉必须是两条**相交**的线,不是两条平行线也不是一条。
    #[test]
    fn cross_is_two_segments_that_actually_cross() {
        let sh = shapes(r(), Glyph::Cross, s());
        assert_eq!(sh.len(), 2, "叉是两笔");
        let pts = points_of(&sh);
        assert_eq!(pts.len(), 4);
        // 两条线的中点都应落在矩形中心。
        let m0 = (pts[0] + pts[1].to_vec2()) / 2.0;
        let m1 = (pts[2] + pts[3].to_vec2()) / 2.0;
        let c = r().center();
        assert!((m0 - c).length() < 0.01, "第一笔不过中心");
        assert!((m1 - c).length() < 0.01, "第二笔不过中心");
        // 斜率必须一正一负,否则是两条平行线。
        let k0 = (pts[1].y - pts[0].y) / (pts[1].x - pts[0].x);
        let k1 = (pts[3].y - pts[2].y) / (pts[3].x - pts[2].x);
        assert!(k0 * k1 < 0.0, "两笔同向,画出来是个等号不是叉");
    }

    /// 图标随 rect 缩放,不能写死像素 —— 按钮高度跟字号走,
    /// 字号一变图标就该跟着变。
    #[test]
    fn glyphs_scale_with_the_rect() {
        let big = Rect::from_min_max(pos2(0.0, 0.0), pos2(64.0, 64.0));
        let small = Rect::from_min_max(pos2(0.0, 0.0), pos2(16.0, 16.0));
        let span = |rc: Rect| {
            let p = points_of(&shapes(rc, Glyph::Cross, s()));
            let xs: Vec<f32> = p.iter().map(|q| q.x).collect();
            xs.iter().cloned().fold(f32::MIN, f32::max)
                - xs.iter().cloned().fold(f32::MAX, f32::min)
            };
        assert!(span(big) > span(small) * 3.0, "图标没跟着 rect 缩放");
    }

    /// 从形状里抠出所有端点,给上面几个测试用。
    fn points_of(shapes: &[egui::Shape]) -> Vec<egui::Pos2> {
        let mut out = Vec::new();
        for s in shapes {
            match s {
                egui::Shape::LineSegment { points, .. } => out.extend_from_slice(points),
                egui::Shape::Path(p) => out.extend_from_slice(&p.points),
                other => panic!("图标里出现了没预期的形状:{other:?}"),
            }
        }
        out
    }
}
```

- [ ] **Step 2: 跑测试确认它失败**

在 `crates/mullion-app/src/ui/mod.rs` 补上 `pub mod icon;`（放在 `pub mod badge;` 之前，保持字母序）。

Run: `cargo test -p mullion-app icon:: 2>&1 | tail -20`
Expected: FAIL，`cannot find type 'Glyph' in this scope` / `cannot find function 'shapes'`。

- [ ] **Step 3: 写最小实现**

在 `icon.rs` 测试模块之前插入：

```rust
//! 控制图标。**用 epaint 直接画,不走字体。**
//!
//! 走查 P0-5 报的「跳板链上那个 □ 看不出是删除」，读代码发现源码里写的
//! 其实是 `✕`(U+2715)——`□` 是**缺字形的豆腐块**。`ui::install_cjk_font`
//! 只装了 egui 内置拉丁字体 + 微软雅黑,U+2715 两边都没有。换成 `🗑`
//! 只会把豆腐换个位置。自绘是唯一不受字体覆盖面影响的做法。
//!
//! `shapes()` 拆成纯函数是为了让「↑ 画成了 ↓」这类 bug 能被单测抓到 ——
//! 它不会引发编译错误、不会 panic,只会让用户点「上移」时条目往下跑。

use egui::{pos2, Rect, Response, Shape, Stroke, Ui, Vec2};

/// 本项目用到的控制图标。加新图标时同步给 `shapes()` 补分支 ——
/// `match` 是穷尽的,漏了会编译不过。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    /// 叉:移除/删除。
    Cross,
    /// 上移。
    ArrowUp,
    /// 下移。
    ArrowDown,
}

/// 图标笔画占 rect 的比例。留边是为了让图标在按钮里不顶着边框。
const INSET: f32 = 0.28;

/// 把一个图标摊成 epaint 形状。纯函数,不碰 `Ui`。
///
/// `rect` 是图标的**外框**(通常是按钮内容区的正方形部分),所有端点保证
/// 落在框内 —— 越界的笔画会画进邻居按钮的地盘,而 egui 对此毫无怨言。
pub fn shapes(rect: Rect, glyph: Glyph, stroke: Stroke) -> Vec<Shape> {
    // 取正方形内接区,再按 INSET 收边。非正方 rect 下箭头不会被拉扁。
    let side = rect.width().min(rect.height());
    let c = rect.center();
    let h = side * (0.5 - INSET);
    match glyph {
        Glyph::Cross => vec![
            Shape::LineSegment {
                points: [pos2(c.x - h, c.y - h), pos2(c.x + h, c.y + h)],
                stroke: stroke.into(),
            },
            Shape::LineSegment {
                points: [pos2(c.x + h, c.y - h), pos2(c.x - h, c.y + h)],
                stroke: stroke.into(),
            },
        ],
        // 人字形(chevron),不画箭杆:16px 见方里画带杆的箭头笔画会糊成一团。
        Glyph::ArrowUp => vec![Shape::line(
            vec![
                pos2(c.x - h, c.y + h * 0.5),
                pos2(c.x, c.y - h * 0.7),
                pos2(c.x + h, c.y + h * 0.5),
            ],
            stroke,
        )],
        Glyph::ArrowDown => vec![Shape::line(
            vec![
                pos2(c.x - h, c.y - h * 0.5),
                pos2(c.x, c.y + h * 0.7),
                pos2(c.x + h, c.y - h * 0.5),
            ],
            stroke,
        )],
    }
}

/// 图标按钮。返回是否被点击。
///
/// `tooltip` 是**必填**参数而不是 `Option`:走查 P0-5 的另一半是
/// 「所有图标按钮都要有 hover tooltip」。做成必填,新加图标按钮时
/// 就不可能忘 —— 编译器会要求你传。
pub fn icon_button(ui: &mut Ui, glyph: Glyph, enabled: bool, tooltip: &str) -> bool {
    let size = Vec2::splat(ui.spacing().interact_size.y);
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    // tooltip 无条件挂:禁用态(第一跳的「上移」)更需要说明它是什么,
    // 而 `on_disabled_hover_text` 只对 `add_enabled` 造出来的 Response 生效,
    // 这里的 Response 来自 `allocate_exact_size`,它永远算「启用」。
    let resp: Response = resp.on_hover_text(tooltip);

    if ui.is_rect_visible(rect) {
        let vis = if enabled {
            ui.style().interact(&resp)
        } else {
            &ui.visuals().widgets.noninteractive
        };
        let (rounding, weak_bg, bg_stroke) = (vis.rounding, vis.weak_bg_fill, vis.bg_stroke);
        let fg = if enabled {
            vis.fg_stroke.color
        } else {
            // 禁用态压到 fg_faint:noninteractive 的前景色跟正常态一样,
            // 光靠它分不出「能点」和「不能点」。
            ui.visuals().weak_text_color()
        };
        ui.painter().rect(rect, rounding, weak_bg, bg_stroke);
        ui.painter().extend(shapes(rect, glyph, Stroke::new(1.5, fg)));
    }
    enabled && resp.clicked()
}
```

**egui 0.30 签名核对提示（写之前先确认，别凭记忆）：**
- `Shape::LineSegment { points: [Pos2; 2], stroke: PathStroke }` —— `stroke` 字段是 `PathStroke`，所以要 `.into()`。
- `Shape::line(points: Vec<Pos2>, stroke: impl Into<PathStroke>) -> Shape` 返回 `Shape::Path`。
- `Ui::allocate_exact_size(desired: Vec2, sense: Sense) -> (Rect, Response)`。
- `Style::interact(&Response) -> &WidgetVisuals`。
- `Painter::rect(rect, rounding, fill, stroke)`。
如果任一签名对不上，**看编译器报的实际签名改，不要猜**（CLAUDE.md「API 漂移」）。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app icon:: 2>&1 | tail -20`
Expected: PASS，`test result: ok. 4 passed`。

- [ ] **Step 5: 变异验证（确认测试不是恒绿）**

把 `Glyph::ArrowUp` 分支里的 `c.y - h * 0.7` 临时改成 `c.y + h * 0.7`，再跑：

Run: `cargo test -p mullion-app icon::tests::arrow_up_points_up 2>&1 | tail -20`
Expected: FAIL，`ArrowUp 的尖端在中心线下方,画反了`。

确认变红后**把改动改回来**再跑一次确认变绿。（这一步来自项目既有教训：纯加测试的改动必须自证能变红，见 memory `subagent-driven-review-lessons`。）

- [ ] **Step 6: 提交**

```bash
cargo fmt
cargo clippy -p mullion-app --all-targets -- -D warnings
git add crates/mullion-app/src/ui/icon.rs crates/mullion-app/src/ui/mod.rs
git commit -m "feat(ui): epaint 自绘控制图标模块 (走查 P0-5)

跳板链上那个 □ 不是代码写错,源码里是 ✕(U+2715);□ 是缺字形的豆腐块 ——
install_cjk_font 只装了 egui 内置拉丁 + 微软雅黑,U+2715 两边都没有。
按走查建议改成 ✕ 或 🗑 会原样复现同一个 bug,所以改成不走字体的自绘。
shapes() 做成纯函数,让「↑ 画成 ↓」这类不报错不 panic 的 bug 能被单测抓住。
本提交只加模块,调用点在下一提交。"
```

---

## Task 3: `theme::hint_text()` —— 占位符文字提到 AA

**为什么：** 走查 P2-17 说占位符对比度偏低。查 egui 0.30 源码确认了根因：`TextEdit` 画 hint 时用 `ui.visuals().weak_text_color()`（`widgets/text_edit/builder.rs:693`），而 `weak_text_color()` = `tint_color_towards(fg_muted, widgets.noninteractive.weak_bg_fill)`（`style.rs:990,1013,1020`）—— 也就是 `fg_muted` 和 `panel_bg` 的中间色，深色面板上远达不到 4.5:1。

egui **没有** 设置 `weak_text_color` 的入口（它是派生量）。但 `hint_text()` 收的是 `impl Into<WidgetText>`，而 `Painter::galley(pos, galley, fallback_color)` 的第三参只是 **fallback** —— galley 里自带颜色时不生效。所以传一个 `RichText::new(s).color(...)` 就能盖掉。

**Files:**
- Modify: `crates/mullion-app/src/theme.rs`（新增 `hint_text()` + 两个测试）

- [ ] **Step 1: 先写失败的测试**

在 `crates/mullion-app/src/theme.rs` 的 `mod tests` 里追加：

```rust
    /// 走查 P2-17:占位符文字要达 WCAG AA 的 4.5:1。
    /// 底色取 `sunken_bg`(`extreme_bg_color`,即输入框内部的底),
    /// 不是 `panel_bg` —— 占位符画在输入框里,不是画在面板上。
    #[test]
    fn hint_text_color_meets_aa_against_the_input_background() {
        let ratio = contrast_ratio(MULLION_DARK.fg_dimmer, MULLION_DARK.sunken_bg);
        assert!(
            ratio >= 4.5,
            "占位符 fg_dimmer on sunken_bg 只有 {ratio:.2}:1,达不到 AA"
        );
    }

    /// 这条测试的作用是**记录 `hint_text()` 这层包装为什么存在**:
    /// egui 自己算的 hint 颜色达不到 AA。哪天 egui 改了默认取色、
    /// 这条测试变红,说明包装可以删了 —— 那时应当删掉它,而不是
    /// 放宽这里的断言。
    #[test]
    fn egui_default_hint_color_would_fail_aa_which_is_why_hint_text_exists() {
        let ctx = egui::Context::default();
        apply_egui(&ctx, &MULLION_DARK);
        let weak = ctx.style().visuals.weak_text_color();
        let ratio = contrast_ratio(
            Rgb::new(weak.r(), weak.g(), weak.b()),
            MULLION_DARK.sunken_bg,
        );
        assert!(
            ratio < 4.5,
            "egui 的默认 hint 颜色现在有 {ratio:.2}:1,已经够用了 —— \
             请删掉 theme::hint_text() 这层包装及本测试"
        );
    }

    /// `hint_text()` 必须真的把颜色写进 `RichText`,否则
    /// `Painter::galley` 会回落到 egui 的 `weak_text_color()`,包装白做。
    #[test]
    fn hint_text_carries_an_explicit_color_so_the_fallback_never_applies() {
        let rt = hint_text(&MULLION_DARK, "留空 = 继承(远端默认)");
        assert_eq!(
            rt.color(),
            Some(c32(MULLION_DARK.fg_dimmer)),
            "RichText 没带颜色 = 走 egui fallback = 对比度原样偏低"
        );
    }
```

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test -p mullion-app theme::tests::hint 2>&1 | tail -20`
Expected: FAIL，`cannot find function 'hint_text' in this scope`。

（前两条测试可能已经通过 —— 它们只用现有 API。第三条必定编译失败，导致整个 crate 测试编译不过，这就是「红」。）

- [ ] **Step 3: 写最小实现**

在 `theme.rs` 的 `pub fn contrast_ratio` 之后追加：

```rust
/// 输入框占位符文字。**所有 `hint_text` 都必须走这里。**
///
/// egui 画 hint 用的是 `Visuals::weak_text_color()`
/// (`widgets/text_edit/builder.rs:693`),而它 =
/// `tint_color_towards(fg_muted, widgets.noninteractive.weak_bg_fill)`
/// —— 也就是 `fg_muted` 与 `panel_bg` 的中间色,在深色面板上达不到
/// WCAG AA 的 4.5:1(走查 P2-17)。
///
/// egui 没有设置 `weak_text_color` 的入口(它是派生量),但
/// `Painter::galley` 的第三参只是 **fallback** ——galley 自带颜色时
/// 不生效。所以给 `hint_text()` 传一个带显式颜色的 `RichText` 就能盖掉。
pub fn hint_text(t: &Theme, s: impl Into<String>) -> egui::RichText {
    egui::RichText::new(s.into()).color(c32(t.fg_dimmer))
}
```

若 `theme.rs` 的 `mod tests` 里还没 `use` 到 `Rgb`，在测试模块顶部补 `use mullion_term::snapshot::Rgb;`（文件顶部第 14 行已经有同名 `use`，测试模块里写 `use super::*;` 时可能已覆盖 —— 编译报错再补）。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app theme:: 2>&1 | tail -20`
Expected: PASS，三条新测试全绿。

- [ ] **Step 5: 把所有 `hint_text` 调用点换过来**

逐点替换（共 8 处）：

`crates/mullion-app/src/ui/session_manager/list.rs:214`
```rust
            .hint_text(crate::theme::hint_text(t, "搜索名称 / 主机 / 标签"))
```
（`list.rs` 里若当前作用域没有 `t: &Theme`，用该函数已有的主题参数名；若确实拿不到，说明这处得先把 `t` 传进来 —— 照做，别绕过。）

`crates/mullion-app/src/ui/session_manager/mod.rs:593`
```rust
                    .hint_text(theme::hint_text(t, empty_hint))
```

`crates/mullion-app/src/ui/session_manager/fields.rs:215`
```rust
                            .hint_text(crate::theme::hint_text(t, "🔥")),
```

`crates/mullion-app/src/ui/session_manager/fields.rs:308`
```rust
                            .hint_text(crate::theme::hint_text(t, "#rrggbb")),
```

`crates/mullion-app/src/ui/session_manager/fields.rs:750`（原本是一个 `if/else` 表达式产出 `String`，把整个表达式的结果喂进 `hint_text()`）
```rust
                    .hint_text(crate::theme::hint_text(
                        t,
                        if derived.is_empty() {
                            /* 保持原有的 else 分支文案原样 */
                        } else {
                            /* 保持原有的 then 分支文案原样 */
                        },
                    ))
```
> 执行时**照抄该处现有的两支文案，一个字都不要改** —— 本 Task 只换颜色，不改文案。

`crates/mullion-app/src/ui/session_manager/fields.rs:770`
```rust
                .hint_text(crate::theme::hint_text(t, "留空 = 继承(远端默认)"))
```

`crates/mullion-app/src/ui/session_manager/fields.rs:933`
```rust
                                .hint_text(crate::theme::hint_text(t, "KEY"))
```

`crates/mullion-app/src/ui/session_manager/fields.rs:939`
```rust
                                .hint_text(crate::theme::hint_text(t, "值(明文)"))
```

- [ ] **Step 6: 确认没有漏网的裸 `hint_text`**

Run: `grep -rn 'hint_text("' crates/mullion-app/src/`
Expected: 无输出（所有调用点都改成了 `hint_text(crate::theme::hint_text(t, ...))` 的形式，裸字符串字面量只剩在 `theme::hint_text` 的实现和测试里）。

- [ ] **Step 7: 跑全量测试**

Run: `cargo test -p mullion-app 2>&1 | tail -20`
Expected: PASS。若 `fields.rs:750` 那处因为 `derived` 的借用顺序编译不过，把 `hint_text()` 的结果先绑到局部变量再喂给 `TextEdit`。

- [ ] **Step 8: 提交**

```bash
cargo fmt
cargo clippy -p mullion-app --all-targets -- -D warnings
git add crates/mullion-app/src/theme.rs crates/mullion-app/src/ui/session_manager/
git commit -m "fix(ui): 占位符文字提到 WCAG AA (走查 P2-17)

egui 画 hint 用 Visuals::weak_text_color(),那是 fg_muted 与 panel_bg 的
中间色,深色面板上达不到 4.5:1,且 egui 没有设置它的入口。
改法:hint_text() 收 impl Into<WidgetText>,而 Painter::galley 的第三参
只是 fallback —— 传带显式颜色的 RichText 就能盖掉。
新增 theme::hint_text() 统一入口,8 处调用点全部改走它。
附带一条「egui 默认色确实不够用」的反证测试,哪天它变红就该删掉这层包装。"
```

---

## Task 4: 表单宽度全面换档

**这是走查 P0-1 + P0-2 的正式修复。** 前三个 Task 只是备料。

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/fields.rs:13-19,131-182,501-518,640-682`
- Modify: `crates/mullion-app/src/ui/session_manager/mod.rs:559-600`

- [ ] **Step 1: 先写失败的测试**

在 `crates/mullion-app/src/ui/session_manager/fields.rs` 的 `mod tests` 里追加。先加一个「测量某个宽度下所有形状是否越界」的工具函数，Task 8 的 DPI 测试还要复用它：

```rust
    /// 所有形状的最右边界。无穷/NaN 的(整屏底色之类)跳过。
    ///
    /// 用它而不是「找某个控件的 Response」:走查 P0-1 的症状是**画出去了**
    /// 被 clip_rect 裁掉,`Response.rect` 反而看不出问题 —— 形状边界才看得出。
    fn max_right(shapes: &[egui::epaint::ClippedShape]) -> f32 {
        fn walk(s: &egui::Shape, acc: &mut f32) {
            if let egui::Shape::Vec(v) = s {
                v.iter().for_each(|x| walk(x, acc));
                return;
            }
            let r = s.visual_bounding_rect();
            if r.is_finite() && r.right() > *acc {
                *acc = r.right();
            }
        }
        let mut acc = f32::MIN;
        shapes.iter().for_each(|cs| walk(&cs.shape, &mut acc));
        acc
    }

    /// 在给定的面板宽度下跑两帧「认证」页,返回第二帧输出。
    /// 跑两帧的理由同 `run_appearance`:第一帧的布局是估的。
    fn run_auth_at(width: f32, presence: SecretPresence, buf: &mut EditorBuffer) -> egui::FullOutput {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, 600.0),
            )),
            ..Default::default()
        };
        let mut run = |buf: &mut EditorBuffer| {
            ctx.run(input(), |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::none())
                    .show(ctx, |ui| {
                        auth(ui, &t, buf, presence, &[]);
                    });
            })
        };
        let _ = run(buf);
        run(buf)
    }

    /// **走查 P0-1 的守护测试。**
    ///
    /// 老写法:`TextEdit::singleline(value).desired_width(f32::INFINITY)`
    /// 吃光整行,后面的「已设置(不修改则保持不变)」被推到面板外,
    /// 只露出半个字。右栏被分隔条拖窄时更狠。
    ///
    /// 300.0 不是随手挑的:分隔条拖到 `LIST_MAX_W = 440` 时右栏内容宽
    /// 实测约 300px,是本项目真实可达的最窄值。
    #[test]
    fn password_row_never_paints_past_the_panel_even_at_the_narrowest_pane() {
        for width in [300.0f32, 440.0, 900.0] {
            let mut buf = EditorBuffer {
                auth_kind: AuthKindUi::Password,
                ..Default::default()
            };
            let presence = SecretPresence {
                password: true,
                ..Default::default()
            };
            let out = run_auth_at(width, presence, &mut buf);
            let right = max_right(&out.shapes);
            assert!(
                right <= width + 0.5,
                "面板宽 {width},却画到了 x={right} —— 密码框右边的附属控件被裁了"
            );
        }
    }

    /// 同上,但走「已改过」分支(touched = true) —— 那一支右边挂的是
    /// 「撤销」按钮 + 「留空 = 清除已存凭据」,加起来比另外两支都长。
    #[test]
    fn touched_password_row_fits_the_revert_button_at_the_narrowest_pane() {
        for width in [300.0f32, 440.0] {
            let mut buf = EditorBuffer {
                auth_kind: AuthKindUi::Password,
                password_touched: true,
                ..Default::default()
            };
            let out = run_auth_at(width, SecretPresence::default(), &mut buf);
            let right = max_right(&out.shapes);
            assert!(
                right <= width + 0.5,
                "面板宽 {width},却画到了 x={right} —— 「撤销」被裁了"
            );
        }
    }

    /// **走查 P0-2 的守护测试。** 名称框不该横跨整行。
    #[test]
    fn medium_fields_are_capped_and_do_not_span_the_whole_row() {
        let mut buf = EditorBuffer::default();
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut widths = Vec::new();
        let mut run = |buf: &mut EditorBuffer, widths: &mut Vec<f32>| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(900.0, 600.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::none())
                        .show(ctx, |ui| {
                            widths.clear();
                            widths.push(crate::ui::metrics::field_w(
                                ui.available_width(),
                                crate::ui::metrics::FIELD_W_M,
                                0.0,
                            ));
                            basic(ui, &t, buf, &[], &[], None);
                        });
                },
            )
        };
        let _ = run(&mut buf, &mut widths);
        let _ = run(&mut buf, &mut widths);
        assert_eq!(
            widths[0],
            crate::ui::metrics::FIELD_W_M,
            "900px 宽的面板里,中值字段必须被 FIELD_W_M 压住,而不是撑满"
        );
    }
```

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test -p mullion-app session_manager::fields::tests::password_row session_manager::fields::tests::touched_password session_manager::fields::tests::medium_fields 2>&1 | tail -30`
Expected: FAIL —— 前两条报「画到了 x=…」（数值大于面板宽），第三条报断言不等。

若前两条意外通过，**先别改实现**：把 `width` 降到 240.0 再试，确认这套测量手法确实能抓到越界；抓不到就说明 `max_right` 的写法有问题（比如 clip 后的形状不进 `shapes`），必须先修好测量再往下走。

- [ ] **Step 3: 改 `secret_edit`**

`crates/mullion-app/src/ui/session_manager/mod.rs:559-600` 整体替换为：

```rust
    ui.horizontal(|ui| {
        use crate::ui::metrics::{button_reserve, field_w, FIELD_W_M};
        if *touched {
            // 走查 P0-1:老写法 `desired_width(f32::INFINITY)` 让输入框吃光
            // 整行,「撤销」和后面的警告被推出面板只露半个字。改成先量出
            // 「撤销」实际要多宽、从可用宽里扣掉,再取 FIELD_W_M 上限。
            // 量而不是写常量:按钮宽随字号/DPI 变,写死的值会悄悄失同步。
            let reserve = button_reserve(ui, "撤销");
            let w = field_w(ui.available_width(), FIELD_W_M, reserve);
            ui.add(
                egui::TextEdit::singleline(value)
                    .id_salt(id)
                    .password(true)
                    .desired_width(w),
            );
            if ui.button("撤销").clicked() {
                *touched = false;
                value.clear();
            }
        } else if has_stored {
            let w = field_w(ui.available_width(), FIELD_W_M, 0.0);
            let resp = ui.add(
                egui::TextEdit::singleline(&mut "******".to_string())
                    .id_salt(id)
                    .password(true)
                    .desired_width(w),
            );
            if resp.gained_focus() {
                // 一聚焦就翻面:框清空、进入可编辑态。占位符本身从不外流。
                *touched = true;
                value.clear();
            }
        } else {
            let w = field_w(ui.available_width(), FIELD_W_M, 0.0);
            let resp = ui.add(
                egui::TextEdit::singleline(value)
                    .id_salt(id)
                    .password(true)
                    .hint_text(theme::hint_text(t, empty_hint))
                    .desired_width(w),
            );
            if resp.gained_focus() {
                *touched = true;
            }
        }
    });
    // 说明文字另起一行。它们是**句子**,挤在输入框右边时无论怎么算宽度
    // 都放不下(「留空 = 清除已存凭据」在 14px 下就要约 180px,而最窄
    // 右栏扣掉输入框下界后只剩不到 100px)。这是走查 P0-1 的另一半。
    if *touched && value.is_empty() {
        ui.colored_label(theme::c32(t.warn), "留空 = 清除已存凭据");
    } else if !*touched && has_stored {
        ui.colored_label(theme::c32(t.fg_dimmer), "已设置(不修改则保持不变)");
    }
```

**两处必须留意：**
1. 上面把 `secret_edit` 从「一个 `ui.horizontal`」变成「一个 `horizontal` + 后面跟一行 label」。它被 `grid()` 的单元格调用（`fields.rs:524`、`fields.rs:670`），Grid 单元格里放两行是允许的（单元格会长高），但**行高会变**。这是有意的：说明文字必须能完整显示。
2. `has_stored` 分支原本把 `"******"` 绑到局部 `let mut placeholder`。上面写成 `&mut "******".to_string()` 会创建临时值，Rust 借用检查不通过。**保留原来的 `let mut placeholder = "******".to_string();` 写法**，只把 `desired_width` 换掉。

- [ ] **Step 4: 改 `fields.rs` 的宽度**

`fields.rs` 文件顶部 `use` 区追加：
```rust
use crate::ui::metrics::{field_w, FIELD_W_L, FIELD_W_M, FIELD_W_S, LABEL_COL_W};
```

`fields.rs:13-19` 的 `grid()`：
```rust
fn grid(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    egui::Grid::new(id)
        .num_columns(2)
        .spacing([crate::ui::metrics::SP_M, 10.0])
        .min_col_width(LABEL_COL_W)
        .max_col_width(f32::INFINITY)
        .show(ui, add);
}
```
> 若 egui 0.30 的 `Grid` 没有 `max_col_width`（**先查再写**），去掉那一行。

`fields.rs:133`（名称）：
```rust
        ui.add(
            egui::TextEdit::singleline(&mut buf.name)
                .desired_width(field_w(ui.available_width(), FIELD_W_M, 0.0)),
        );
```

`fields.rs:137`（主机）：同上，把 `buf.name` 换成 `buf.host`。

`fields.rs:141`（端口）：
```rust
        ui.add(
            egui::TextEdit::singleline(&mut buf.port)
                .desired_width(field_w(ui.available_width(), FIELD_W_S, 0.0)),
        );
```

`fields.rs:175-181`（备注，同时修走查 P2-17 提到的「「备注」标签垂直位置和别的行不一致」）：
```rust
        // 标签顶对齐:Grid 每行默认 `Align::Center`,3 行高的 multiline
        // 旁边的短标签会被垂直居中,跟上面几行的标签对不齐(走查 P2-17)。
        ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
            ui.label("备注");
        });
        ui.add(
            egui::TextEdit::multiline(&mut buf.note)
                .desired_rows(3)
                .desired_width(field_w(ui.available_width(), FIELD_W_L, 0.0)),
        );
        ui.end_row();
```

`fields.rs:503`（用户名）：
```rust
        ui.add(
            egui::TextEdit::singleline(&mut buf.user)
                .desired_width(field_w(ui.available_width(), FIELD_W_M, 0.0)),
        );
```

`fields.rs:656-667`（代理地址 / 端口 / 用户）：
```rust
            ui.label("代理地址");
            ui.horizontal(|ui| {
                // 端口跟在同一行,先给它留位置再算主机框 —— 否则主机框
                // 吃光整行,端口框被顶出去(走查 P0-1 的同型缺陷)。
                let reserve = FIELD_W_S + ui.spacing().item_spacing.x;
                ui.add(
                    egui::TextEdit::singleline(&mut buf.proxy_host)
                        .desired_width(field_w(ui.available_width(), FIELD_W_M, reserve)),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut buf.proxy_port)
                        .desired_width(FIELD_W_S),
                );
            });
            ui.end_row();

            ui.label("代理用户");
            ui.add(
                egui::TextEdit::singleline(&mut buf.proxy_user)
                    .desired_width(field_w(ui.available_width(), FIELD_W_M, 0.0)),
            );
            ui.end_row();
```

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p mullion-app 2>&1 | tail -30`
Expected: PASS。既有测试里若有依赖旧布局的（例如按坐标点击的测试算错了位置），**修测试的坐标算法、不要放宽断言**。

- [ ] **Step 6: 确认没有漏网的 `INFINITY`**

Run: `grep -rn 'desired_width(f32::INFINITY)' crates/mullion-app/src/ui/session_manager/`
Expected: 只剩 `list.rs:215`（左栏搜索框，它**应该**撑满窄列，是正确用法）。其余全部消失。

- [ ] **Step 7: 提交**

```bash
cargo fmt
cargo clippy -p mullion-app --all-targets -- -D warnings
git add crates/mullion-app/src/ui/session_manager/
git commit -m "fix(ui): 表单字段按语义分档定上限,附属控件不再被裁 (走查 P0-1/P0-2)

P0-1 的真凶不是「显示/隐藏密码的眼睛按钮」(那个按钮不存在),是
secret_edit 三个分支全用 desired_width(INFINITY):输入框吃光整行,
「撤销」/「已设置(不修改则保持不变)」被推出面板只露半个字。
改法两条:①「撤销」的宽度用 button_reserve 实测后从可用宽里扣掉;
② 说明文字挪到下一行 —— 它们是句子,右栏最窄时无论怎么算都放不下。
P0-2:名称/主机/用户名/代理地址走 FIELD_W_M,端口走 FIELD_W_S,备注走
FIELD_W_L。顺带修掉「备注」标签因 Grid 默认居中而与上面几行错位。
守护测试按 300/440/900 三档面板宽比对形状最右边界。"
```

---

## Task 5: 跳板链按钮换自绘图标 + tooltip

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/fields.rs:438-451`

- [ ] **Step 1: 先写失败的测试**

在 `fields.rs` 的 `mod tests` 里追加：

```rust
    /// 走查 P0-5。老写法用 `ui.button("✕")` —— U+2715 不在 egui 内置
    /// 拉丁字体里,也不在微软雅黑里,实机渲染成豆腐块 □,用户完全看不出
    /// 是「删除」。改成自绘后,页面上不该再有任何这三个字符的文字形状。
    #[test]
    fn jump_row_buttons_are_drawn_not_typed_so_they_cannot_render_as_tofu() {
        let sessions = vec![
            session_named(1, "GoFish"),
            session_named(2, "LEG"),
            session_named(3, "Brain"),
        ];
        let mut buf = EditorBuffer {
            jump_mode: JumpModeUi::Custom,
            jump_chain: vec![SessionId(1), SessionId(2)],
            ..Default::default()
        };
        let out = run_basic_with(&mut buf, &sessions);
        for ch in ["✕", "↑", "↓"] {
            assert!(
                find_text_pos(&out.shapes, ch).is_none(),
                "页面上还有文字形状 {ch:?} —— 它在真机上是豆腐块,必须改成自绘"
            );
        }
    }
```

`session_named` / `run_basic_with` 若 `fields.rs` 的测试模块里已有等价工具（现有 `run_basic` 只收 `sessions: &[SessionRecord]`），直接复用现成的；没有就照现有 `run_basic`（`fields.rs:1556`）的写法补一个能传 `sessions` 的版本，并用该模块里已有的 `SessionRecord` 构造工具生成会话。**不要新造 `SessionRecord` 的构造方式**，照抄同模块里已有的。

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test -p mullion-app jump_row_buttons_are_drawn 2>&1 | tail -20`
Expected: FAIL，`页面上还有文字形状 "✕"`。

- [ ] **Step 3: 改实现**

`fields.rs:438-451` 替换为：

```rust
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    use crate::ui::icon::{icon_button, Glyph};
                    // 自绘而不是打字:U+2715 / U+2191 / U+2193 都不在 egui
                    // 内置拉丁字体和微软雅黑里,实机上全渲染成豆腐块 □
                    // (走查 P0-5 报的「□ 完全看不出是删除」)。
                    // tooltip 是 `icon_button` 的必填参数,忘不了。
                    if icon_button(ui, Glyph::Cross, true, "移除此跳板") {
                        remove = Some(i);
                    }
                    if icon_button(ui, Glyph::ArrowDown, i + 1 < len, "下移") {
                        swap = Some((i, i + 1));
                    }
                    if icon_button(ui, Glyph::ArrowUp, i > 0, "上移") {
                        swap = Some((i - 1, i));
                    }
                });
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app -- session_manager::fields 2>&1 | tail -30`
Expected: PASS。若既有的「点某一行的 ↑ 会上移」类测试是靠 `find_all_text_pos(&out.shapes, "↑")` 定位的，它们现在找不到文字了 —— **必须改成按 `Response.rect` 或按行 y 坐标定位，不能删掉那些测试**（它们守的是「salt 用 (位置, 会话 id)」那个真 bug）。改法：在 `chain_editor` 里给每个 `icon_button` 之前 `ui.push_id` 已有的 salt 下再取一个稳定 id，测试用 `ctx.read_response(id)` 拿矩形 —— 这与 `mod.rs` 里 `save_button_id()` / `probe_button_id()` 的既有做法一致。

- [ ] **Step 5: 提交**

```bash
cargo fmt
cargo clippy -p mullion-app --all-targets -- -D warnings
git add crates/mullion-app/src/ui/session_manager/fields.rs
git commit -m "fix(ui): 跳板链按钮改自绘图标并补 tooltip (走查 P0-5)

□ 不是代码写错,源码里是 ✕(U+2715);豆腐块来自字体缺字形 ——
install_cjk_font 只装 egui 内置拉丁 + 微软雅黑,U+2715/↑/↓ 都不在其中。
按走查建议换成 ✕ 或 🗑 会原样复现,所以改成 ui::icon 的自绘图标。
tooltip(上移/下移/移除此跳板)是 icon_button 的必填参数,新加按钮忘不了。
守护测试断言页面上不再有这三个字符的文字形状。"
```

---

## Task 6: 「高级」页合并进「连接」页

**为什么：** 走查 P1-8 说右侧大面积留白 ——「高级」页只有一行「代理」。用户选了方案 B（保留标签页 + 合并高级进连接 + 保持 880×560 默认尺寸）。代理本来就属于「怎么连上去」，跟主机/端口/跳板是同一个决策。

**代理放在跳板之前**：连接路径是 `本机 →(代理)→ 第一跳跳板 →…→ 目标主机`，页面自上而下按这个顺序排，阶段 2 要加的「连接路径预览」才读得通。

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/mod.rs:67-71`
- Modify: `crates/mullion-app/src/ui/session_manager/editor.rs`（`TABS`、tab 分派、常量表测试）
- Modify: `crates/mullion-app/src/ui/session_manager/fields.rs:122-185,640`

- [ ] **Step 1: 先写失败的测试**

在 `editor.rs` 的 `mod tests` 里，把现有的 `TABS` 常量表测试（`editor.rs:564-570`）**替换**为：

```rust
    /// Tab 常量必须与 `TABS` 一一对应。合并「高级」后只剩 4 页。
    ///
    /// `TAB_AUTH` 的下标不能变:`editor.rs` 里拖入私钥文件的门控写的是
    /// `editor_tab == TAB_AUTH && auth_kind == PublicKey`,而
    /// `validate::Missing::tab()` 也返回 `TAB_AUTH`。下标一漂,这两处
    /// 全部悄悄失准,没有任何编译错误。
    #[test]
    fn tab_constants_match_the_table_and_auth_keeps_index_one() {
        use crate::ui::session_manager::{TAB_APPEARANCE, TAB_AUTOMATION};
        assert_eq!(super::TABS.len(), 4, "「高级」已并入「连接」,不该还有 5 页");
        assert_eq!(super::TABS[TAB_CONNECT], "连接");
        assert_eq!(super::TABS[TAB_AUTH], "认证");
        assert_eq!(super::TABS[TAB_AUTOMATION], "登录后");
        assert_eq!(super::TABS[TAB_APPEARANCE], "图标");
        assert_eq!(TAB_AUTH, 1, "私钥拖入门控与必填校验都钉在这个下标上");
    }
```

在 `fields.rs` 的 `mod tests` 里追加：

```rust
    /// 走查 P1-8:「高级」页只有一行代理,右侧 70% 全是空白。
    /// 合并后代理必须出现在「连接」页上。
    #[test]
    fn proxy_settings_render_on_the_connect_page_after_the_merge() {
        let mut buf = EditorBuffer {
            proxy_mode: ProxyModeUi::Socks5,
            ..Default::default()
        };
        let out = run_basic(&mut buf, &[]);
        assert!(
            find_text_pos(&out.shapes, "代理地址").is_some(),
            "选了 SOCKS5 却在「连接」页上找不到代理地址 —— 合并没做完"
        );
    }

    /// 代理排在跳板之前:连接路径是 本机 →(代理)→ 第一跳 →…→ 目标,
    /// 页面自上而下得按这个顺序,阶段 2 的「连接路径预览」才读得通。
    #[test]
    fn proxy_section_comes_before_the_jump_section() {
        let mut buf = EditorBuffer {
            proxy_mode: ProxyModeUi::Socks5,
            jump_mode: JumpModeUi::Custom,
            ..Default::default()
        };
        let out = run_basic(&mut buf, &[]);
        let proxy = find_text_pos(&out.shapes, "代理").expect("找不到代理分区");
        let jump = find_text_pos(&out.shapes, "跳板").expect("找不到跳板分区");
        assert!(
            proxy.y < jump.y,
            "代理 y={} 排在跳板 y={} 下面了",
            proxy.y,
            jump.y
        );
    }
```

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test -p mullion-app -- tab_constants_match proxy_settings_render proxy_section_comes 2>&1 | tail -30`
Expected: FAIL —— 第一条报 `TABS.len()` 是 5，后两条报找不到「代理地址」。

- [ ] **Step 3: 改常量表**

`mod.rs:67-71`：
```rust
pub(crate) const TAB_CONNECT: usize = 0;
pub(crate) const TAB_AUTH: usize = 1;
pub(crate) const TAB_AUTOMATION: usize = 2;
pub(crate) const TAB_APPEARANCE: usize = 3;
```
（删掉 `TAB_ADVANCED`。）

`editor.rs` 的 `TABS`：
```rust
const TABS: [&str; 4] = ["连接", "认证", "登录后", "图标"];
```

- [ ] **Step 4: 改 tab 分派**

`editor.rs:386-397`：
```rust
        .show(ui, |ui| match ui_state.editor_tab {
            super::TAB_AUTH => super::fields::auth(ui, t, buf, presence, &ui_state.key_candidates),
            super::TAB_AUTOMATION => super::fields::automation(ui, t, buf),
            super::TAB_APPEARANCE => super::fields::appearance(ui, t, buf),
            // `TAB_CONNECT` 与「越界值兜底」合并成同一个分支:`editor_tab`
            // 是既有的裸 usize 技术债,越界值落回首页比 panic 好。
            _ => super::fields::basic(
                ui,
                t,
                buf,
                groups,
                sessions,
                ui_state.editor_id,
                presence,
            ),
        });
```

- [ ] **Step 5: 把 `network()` 并进 `basic()`**

`fields.rs:122-185` 的 `basic()`：签名末尾加 `presence: SecretPresence`，函数体末尾把 `jump(...)` 那一行换成：

```rust
    // 代理排在跳板之前:连接路径是 本机 →(代理)→ 第一跳 →…→ 目标主机,
    // 页面自上而下按这个顺序读得通。走查 P1-8 把原「高级」页并到这里 ——
    // 那一页只有一行代理,右侧 70% 是空白。
    network(ui, t, buf, presence);

    jump(ui, t, buf, groups, sessions, editing);
```

`network()` 保持独立函数（`pub(super)` 可以降成私有 `fn`，clippy 会提示未使用的可见性时再降）。它的 `section(ui, t, "代理")` 原样保留。

- [ ] **Step 6: 跑测试确认通过**

Run: `cargo test -p mullion-app 2>&1 | tail -30`
Expected: PASS。既有测试里凡是构造 `run_basic` 的地方都要补 `presence` 实参 —— 用 `SecretPresence::default()`。

- [ ] **Step 7: 确认没有残留引用**

Run: `grep -rn "TAB_ADVANCED" crates/`
Expected: 无输出。

- [ ] **Step 8: 提交**

```bash
cargo fmt
cargo clippy -p mullion-app --all-targets -- -D warnings
git add crates/mullion-app/src/ui/session_manager/
git commit -m "feat(ui): 「高级」页并入「连接」页,五页收成四页 (走查 P1-8)

「高级」页只有一行代理,右侧 70% 是空白;而代理本来就属于「怎么连上去」,
跟主机/端口/跳板同一个决策。用户选的是走查方案 B(保留标签页 + 合并,
不改 880×560 默认尺寸)。
代理排在跳板之前:连接路径是 本机 →(代理)→ 第一跳 →…→ 目标主机。
TAB_AUTH 的下标刻意保持为 1 —— 私钥拖入门控(editor.rs:205)与
validate::Missing::tab() 都钉在这个下标上,漂了不会有编译错误。"
```

---

## Task 7: 分区节奏与 emoji 文案

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/fields.rs:23-31,234-237`
- Modify: `crates/mullion-app/src/ui/session_manager/fields.rs:1590-1615`（测试锚点）

- [ ] **Step 1: 先改测试（这次是「改」不是「加」）**

`fields.rs:1590-1615` 的 `emoji_mode_survives_the_next_frame_with_an_empty_buffer` 用 `"黑白剪影"` 当页面锚点。文案一改这条测试就红 —— **这是设计好的**：它逼你在改文案时同时确认输入区还在。把断言里的锚点换成新文案的特征词，并把注释说清楚锚点的作用：

```rust
    #[test]
    fn emoji_mode_survives_the_next_frame_with_an_empty_buffer() {
        let mut buf = EditorBuffer {
            icon_emoji_mode: true,
            ..Default::default()
        };
        let out = run_appearance(&mut buf);
        // 「单色显示」是这一页 emoji 分支的锚点文字 —— 它在,说明输入区
        // 还在页面上。改这句文案时必须同步改这里(这正是本断言的用处)。
        assert!(
            find_text_pos(&out.shapes, "单色显示").is_some(),
            "选了 emoji 模式但还没填内容时,输入区必须留在页面上;\
             消失了就是用户报的「点 emoji 没有内容」"
        );
        assert!(buf.icon_emoji_mode, "模式位不该被自己的写回逻辑抹掉");
        assert!(
            buf.preserved_appearance.icon.is_none(),
            "缓冲是空的,不该凭空造出一个空 emoji 图标"
        );
    }
```

再追加一条守查 P2-7 的测试：

```rust
    /// 走查 P2-7:界面上不该出现实现细节。用户不需要知道 egui 是什么。
    #[test]
    fn the_ui_never_mentions_egui_or_its_limitations() {
        let mut buf = EditorBuffer {
            icon_emoji_mode: true,
            ..Default::default()
        };
        let out = run_appearance(&mut buf);
        for leak in ["egui", "剪影", "不支持"] {
            assert!(
                find_text_pos(&out.shapes, leak).is_none(),
                "界面上出现了实现细节 {leak:?}"
            );
        }
    }
```

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test -p mullion-app -- emoji_mode_survives the_ui_never_mentions 2>&1 | tail -20`
Expected: FAIL —— 第一条找不到「单色显示」，第二条找到了「egui」。

- [ ] **Step 3: 改文案**

`fields.rs:234-237`：
```rust
                    ui.colored_label(crate::theme::c32(t.fg_dimmer), "图标以单色显示");
```

> 走查 P2-7 列了三档方案（内置矢量图标 / Twemoji 贴图 / 中性一句话）。用户选的是**第三档**：本轮只中性化文案，不做内置图标集、不做纹理贴图（那是 F61 的后续，不在本次走查范围内）。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app -- emoji_mode_survives the_ui_never_mentions 2>&1 | tail -20`
Expected: PASS。

- [ ] **Step 5: 统一分区节奏**

`fields.rs:23-31` 的 `section()`：
```rust
/// 分区小标题。11px + fg_muted,上面留一档大间距 + 一条细分隔线 ——
/// 表单一路平铺下来没有任何视觉锚点,眼睛找不到「这几行是一组」
/// (走查 P2-17)。
///
/// 首个分区不画分隔线:页面顶上来一条横线看着像误画的。
fn section(ui: &mut Ui, t: &Theme, title: &str) {
    use crate::ui::metrics::{SP_L, SP_XS};
    if ui.min_rect().height() > 0.0 {
        ui.add_space(SP_L);
        ui.separator();
    }
    ui.add_space(SP_XS);
    ui.label(
        egui::RichText::new(title)
            .size(11.0)
            .color(crate::theme::c32(t.fg_muted)),
    );
    ui.add_space(SP_XS);
}
```

> `ui.min_rect().height() > 0.0` 用来判断「这是不是本页第一个分区」。若在实测中这个判据不成立（例如 `min_rect` 一开始就非零），改用一个显式的 `first: bool` 参数，由调用方传 —— **不要**留一个判据错误的条件在那，它会导致首个分区之上多一条线，或所有分区都没有线。

- [ ] **Step 6: 跑全量测试**

Run: `cargo test -p mullion-app 2>&1 | tail -30`
Expected: PASS。分区间距变化可能让按坐标点击的既有测试偏移 —— 修坐标算法，不放宽断言。

- [ ] **Step 7: 提交**

```bash
cargo fmt
cargo clippy -p mullion-app --all-targets -- -D warnings
git add crates/mullion-app/src/ui/session_manager/fields.rs
git commit -m "fix(ui): 中性化 emoji 说明 + 统一分区节奏 (走查 P2-7/P2-17)

「emoji 显示为黑白剪影(egui 不支持彩色字形)」把实现细节泄漏给了用户,
改成「图标以单色显示」。走查列的另两档(内置矢量图标集 / Twemoji 贴图)
不在本轮范围。
分区之间改成 16px 间距 + 细分隔线,间距值全部走 metrics 刻度。
既有的 emoji 守护测试锚点同步从「黑白剪影」改到「单色显示」——
锚点跟文案绑死是有意的,它逼着改文案时确认输入区还在页面上。
另加一条断言:界面上不得出现 egui / 剪影 / 不支持 这类实现细节词。"
```

---

## Task 8: DPI 与窄栏守护测试

**诚实说明（必须写进 PR 描述）：** egui 的布局全程以 **点(point)** 为单位，`pixels_per_point` 只在光栅化时生效 —— 所以改 ppp 几乎不改变布局矩形，本测试只能守住字形栅格化的**取整漂移**不把控件顶出面板。走查验收标准里的「125%/150% DPI 下截图无错位」**仍然是纯人工验收项**（CLAUDE.md「你无法验证的东西」）。真正修掉 P0-1 的是 Task 4 的宽度上限 + 预留，不是这条测试。

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/fields.rs`（`mod tests`）

- [ ] **Step 1: 写测试**

在 `fields.rs` 的 `mod tests` 里追加：

```rust
    /// 在给定面板宽与 DPI 下跑两帧「连接」页,返回第二帧输出。
    fn run_basic_at(width: f32, ppp: f32, buf: &mut EditorBuffer) -> egui::FullOutput {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(ppp);
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, 600.0),
            )),
            ..Default::default()
        };
        let mut run = |buf: &mut EditorBuffer| {
            ctx.run(input(), |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::none())
                    .show(ctx, |ui| {
                        basic(ui, &t, buf, &[], &[], None, SecretPresence::default());
                    });
            })
        };
        let _ = run(buf);
        run(buf)
    }

    /// **走查验收标准第一条的自动化部分。**
    ///
    /// 注意边界:egui 的布局全程以「点」为单位,`pixels_per_point` 只在
    /// 光栅化时生效,所以三档 DPI 的布局矩形基本一致 —— 本测试守的是
    /// 字形栅格化的取整漂移。「125%/150% 截图无错位」仍是人工验收项。
    ///
    /// 300.0 是分隔条拖到 `LIST_MAX_W = 440` 时右栏内容宽的实测值,
    /// 是本项目真实可达的最窄面板。
    #[test]
    fn connect_page_never_paints_past_the_panel_at_any_width_or_dpi() {
        for width in [300.0f32, 440.0, 900.0] {
            for ppp in [1.0f32, 1.25, 1.5] {
                let mut buf = EditorBuffer {
                    proxy_mode: ProxyModeUi::Socks5,
                    jump_mode: JumpModeUi::Custom,
                    name: "一个相当长的会话名称用来把标签列撑开".into(),
                    host: "very-long-hostname.internal.example.com".into(),
                    ..Default::default()
                };
                let out = run_basic_at(width, ppp, &mut buf);
                let right = max_right(&out.shapes);
                assert!(
                    right <= width + 0.5,
                    "面板宽 {width} @ {ppp}x,却画到了 x={right}"
                );
            }
        }
    }
```

- [ ] **Step 2: 跑测试**

Run: `cargo test -p mullion-app connect_page_never_paints 2>&1 | tail -20`
Expected: PASS（Task 4 已经修好了宽度）。

**如果这一步一上来就绿**，先做变异验证确认它不是恒绿：把 `fields.rs` 里名称字段的 `field_w(...)` 临时改回 `f32::INFINITY`，重跑，**必须变红**。确认后改回来。恒绿的测试等于没有测试（memory `subagent-driven-review-lessons`）。

- [ ] **Step 3: 提交**

```bash
cargo fmt
cargo clippy -p mullion-app --all-targets -- -D warnings
git add crates/mullion-app/src/ui/session_manager/fields.rs
git commit -m "test(ui): 「连接」页在 3 档面板宽 × 3 档 DPI 下不越界 (走查验收 1)

按形状的可视包围盒比对面板右边界,300px 那一档是分隔条拖到
LIST_MAX_W=440 时右栏内容宽的实测值,是真实可达的最窄面板。
诚实边界:egui 布局以点为单位,pixels_per_point 只在光栅化时生效,
三档 DPI 的布局矩形基本一致 —— 本测试守的是字形取整漂移,
「125%/150% 截图无错位」仍是人工验收项。"
```

---

## 阶段收尾

- [ ] **Step 1: 全量绿**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```
Expected: 所有 `test result` 均为 `ok`；clippy 与 fmt 无输出。**只跑单个 crate 不叫绿**（CLAUDE.md）。

- [ ] **Step 2: 交叉编译自查（不发 Release）**

```bash
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app
```
Expected: 编译通过。**本阶段不做 objdump 验收、不发 Release、不升版本** —— 那些在阶段 4 结束后一次性做。

- [ ] **Step 3: 记下人工验收项**

阶段 1 落地后需要人眼确认、无头环境验不了的：
1. 「认证」页密码框右边的「撤销」按钮、下方的「已设置(不修改则保持不变)」是否完整可见（**这是 P0-1 的正主**）。
2. 把分隔条拖到最右（列表列 440px）后，上面这条是否仍然成立。
3. 名称 / 主机框是否明显变窄，端口框是否只有一小截。
4. 跳板链上的三个按钮是否画成了 ✕ / ∧ / ∨ 而不是豆腐块 □，hover 是否出 tooltip。
5. 「连接」页是否能一屏看到 基本 / 归类 / 代理 / 跳板 四个分区，Tab 条是否只剩 4 页。
6. 「图标」页是否不再出现「egui」字样。
7. 各分区之间的分隔线粗细/颜色是否可接受，首个分区上方是否**没有**多余的线。
8. 125% / 150% 缩放下截图是否有错位。

---

## 自检

**走查条目覆盖：** 本阶段认领 1 / 2 / 5 / 7 / 8 / 17 六条。

| 走查条目 | 落在哪个 Task | 备注 |
|---|---|---|
| 1 布局溢出 | Task 1（`field_w`/`button_reserve`）+ Task 4（`secret_edit` 与全部字段） | 走查原文说的「显示/隐藏密码的眼睛按钮」在代码里不存在；被裁的是「撤销」/「已设置」 |
| 2 输入框宽度失控 | Task 1（档位常量）+ Task 4（换档） | 上限取 320 而非走查建议的 480，理由见 `metrics.rs` 注释 |
| 5 图标按钮无 tooltip | Task 2（`icon.rs`）+ Task 5（接线） | 走查建议的「换成 ✕ 或 🗑」会复现豆腐块；改自绘 |
| 7 实现细节泄漏 | Task 7 | 只做走查列的第三档（中性文案） |
| 8 右侧大面积留白 | Task 6 | 走查方案 B，默认尺寸维持 880×560 不动 |
| 17 视觉规范化 | Task 1（间距刻度 + 标签列宽）+ Task 3（占位符对比度）+ Task 4（备注标签对齐）+ Task 7（分区节奏） | 「浅色主题」已剥离到 F84，不在本轮 |

**明确不在本阶段（后续阶段认领）：** 3、4、6、21、22 → 阶段 3；9、10、11、12、18、19 → 阶段 2；13、14、15、16、20 → 阶段 4。

**类型一致性检查：**
- `field_w(available: f32, max: f32, reserve: f32) -> f32` —— Task 1 定义，Task 4 / Task 8 使用，三参调用一致。
- `button_reserve(ui: &egui::Ui, label: &str) -> f32` —— Task 1 定义，Task 4 使用。
- `shapes(rect: Rect, glyph: Glyph, stroke: Stroke) -> Vec<Shape>` / `icon_button(ui, glyph, enabled, tooltip) -> bool` —— Task 2 定义，Task 5 使用。
- `theme::hint_text(t: &Theme, s: impl Into<String>) -> egui::RichText` —— Task 3 定义与使用一致。
- `basic()` 签名在 Task 6 加了第 7 个参数 `presence: SecretPresence`；Task 8 的 `run_basic_at` 按 7 参调用，与之一致。
- `TAB_ADVANCED` 在 Task 6 删除，`grep` 步骤确认无残留。
