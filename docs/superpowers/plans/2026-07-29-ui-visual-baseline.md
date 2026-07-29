# 视觉基线 F80/F81 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 egui 外壳与 glyphon/wgpu 终端层共用同一套深色 token，终端底色从纯黑改为 `#14161f`，并把状态栏改成左右两栏信息架构。

**Architecture:** 新增 `crates/mullion-app/src/theme.rs` 作为唯一色彩来源。默认前景/背景色经 `mullion-term::palette::DefaultColors` 注入进 VT 层（term 只认自己的 `Rgb`，无 UI 类型泄漏）。先修 quad 着色器的 sRGB 缺陷——不修则 F80 必然验收失败。

**Tech Stack:** Rust / wgpu 23 / egui 0.30 / glyphon 0.7 / alacritty_terminal

**Spec:** `docs/superpowers/specs/2026-07-29-ui-visual-baseline-design.md`

---

## 计划期发现：spec 的「三处同源」实为五处，且有一个色彩空间阻塞

写计划时逐文件核对，spec §3.2 列的三处不全，另有一个更严重的问题：

**追加的两处硬编码**（都是 `0xcc` 字面量，**不引用** `palette::DEFAULT_FG`，grep 常量名找不到）：

| 位置 | 内容 | 后果 |
|---|---|---|
| `gpu.rs:60` | 光标色块 `[0xcc, 0xcc, 0xcc]` | 前景改 `#e4e6f0` 后光标仍是旧灰，肉眼可见色差 |
| `text.rs:131` | glyphon `default_color: rgb(0xcc,0xcc,0xcc)` | 当前每个 span 都带显式色，实际取不到；属潜伏陷阱 |

**阻塞级：quad 着色器没做 sRGB 转换，F80 的验收标准无法达成。**

surface 格式是 sRGB（`gpu.rs:131` 的 `.find(|f| f.is_srgb())`）。写入 sRGB 目标的着色器输出会被硬件当作**线性值**再编码。三条渲染路径的现状：

| 路径 | 是否转换 | 证据 |
|---|---|---|
| egui | ✅ 正确 | `egui-wgpu-0.30.0/src/egui.wgsl:41` `linear_from_gamma_rgb` |
| glyphon（文字） | ✅ 正确 | `glyphon-0.7.0/src/shader.wgsl:35` `srgb_to_linear` |
| **我们的 quad** | ❌ **原样透传** | `gpu.rs:316` `fs_main` 直接 `return in.color` |

今天看不出来，是因为底色是纯黑——`0` 在两个空间里都是 `0`。一旦底色变成 `#14161f`，同一个 token 画出来会是**两个颜色**：egui 菜单栏正确，终端色块偏亮。F80 的人工验收第一条「没有两个世界的割裂感」会直接失败。

所以 Task 1 先修着色器。这不是顺手优化——**它是 F80 能否成立的前提**。

顺带澄清：修完后，现存的 ANSI 彩色格会变暗（回到正确值）。这是修正而非回归，人工验收时会看到红/绿字比以前"深"一点，那是对的。

**另有一处 spec 与代码不符**：spec §2.5 写终端行高 1.2，代码 `text.rs:67` 是 `font_px * 1.25`。1.25 落在 spec 自己给的合理区间（1.1~1.25）内，改它会变动行数、踩 T4/F34，收益为零。**决定：代码不动，Task 9 改 spec 记为 1.25。**

---

## 文件结构

| 文件 | 责任 | 动作 |
|---|---|---|
| `crates/mullion-app/src/theme.rs` | **新增**。全部视觉 token + egui 映射 + wgpu clear 色 + sRGB 转换 | 创建 |
| `crates/mullion-term/src/palette.rs` | 加 `DefaultColors`，穿透 `resolve` | 改 |
| `crates/mullion-term/src/emulator.rs` | 持有并注入 `DefaultColors` | 改 |
| `crates/mullion-app/src/gpu.rs` | 着色器 sRGB；`quads_for` 收 `DefaultColors`（含光标色） | 改 |
| `crates/mullion-app/src/text.rs` | `default_color` 取自主题 | 改 |
| `crates/mullion-app/src/pane.rs` | `Pane::new` 透传 `DefaultColors` | 改 |
| `crates/mullion-app/src/app.rs` | clear 色 / `quads_for` 实参 / `apply_egui` / 去掉 `status` 串 | 改 |
| `crates/mullion-app/src/ui/chrome.rs` | 两个 panel 的 Frame；F81 两栏；菜单正名 | 改 |
| `crates/mullion-app/src/ui/mod.rs` | `build_ui` 去掉 `status` 参数、加 `panes` | 改 |
| `crates/mullion-app/src/lib.rs` | 挂 `pub mod theme;` | 改 |

---

## Task 1: 修 quad 着色器的 sRGB 转换（F80 前置）

**Files:**
- Modify: `crates/mullion-app/src/gpu.rs:293-317`（`QUAD_WGSL`）
- Test: `crates/mullion-app/src/gpu.rs`（`mod tests`）

- [ ] **Step 1: 写失败的守护测试**

着色器的数值正确性无法在无头环境验证（见 CLAUDE.md「你无法验证的东西」），但**「转换有没有被删掉」可以守**。在 `crates/mullion-app/src/gpu.rs` 的 `mod tests` 里追加：

```rust
    /// 守 F80 前置:surface 是 sRGB 格式(`is_srgb()` 挑的),着色器输出会被硬件
    /// 当线性值再编码。不转换的话,同一个 token 在 egui(自己转了)和终端色块
    /// (没转)里会画成两个颜色——底色非黑之后肉眼可见。
    /// 数值正确性只能人眼验;这里守的是「转换没被后来的重构删掉」。
    #[test]
    fn quad_shader_converts_srgb_to_linear() {
        assert!(
            QUAD_WGSL.contains("fn srgb_to_linear"),
            "quad 着色器缺 sRGB→线性 转换,终端色块会比 egui 外壳亮一截"
        );
        assert!(
            QUAD_WGSL.contains("srgb_to_linear(color.rgb)"),
            "srgb_to_linear 定义了但没用在顶点色上"
        );
    }
```

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test -p mullion-app --lib gpu::tests::quad_shader_converts_srgb_to_linear`
Expected: FAIL，panic 信息 `quad 着色器缺 sRGB→线性 转换...`

- [ ] **Step 3: 改着色器**

把 `crates/mullion-app/src/gpu.rs` 的 `QUAD_WGSL` 整体替换为：

```rust
const QUAD_WGSL: &str = r#"
@group(0) @binding(0) var<uniform> resolution: vec4<f32>;

struct VsOut { @builtin(position) pos: vec4<f32>, @location(0) color: vec4<f32> };

// surface 格式是 sRGB(见 Gpu::new 的 is_srgb 挑选),硬件会把着色器输出当**线性**
// 值再编码成 sRGB。所以这里必须先把 sRGB 分量转成线性,否则画出来比实际亮一截。
// egui(egui.wgsl 的 linear_from_gamma_rgb)与 glyphon(shader.wgsl 的
// srgb_to_linear)都这么做;我们不做就会和它们对不上。放顶点着色器:每个
// quad 4 次,比逐像素便宜。
fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let cutoff = c < vec3<f32>(0.04045);
    let lower = c / vec3<f32>(12.92);
    let higher = pow((c + vec3<f32>(0.055)) / vec3<f32>(1.055), vec3<f32>(2.4));
    return select(higher, lower, cutoff);
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32,
           @location(0) rect: vec4<f32>,
           @location(1) color: vec4<f32>) -> VsOut {
    // TriangleStrip 四角:(0,0)(1,0)(0,1)(1,1)
    let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
    let px = rect.xy + corner * rect.zw;        // 像素坐标(左上原点)
    let ndc = vec2<f32>(
        px.x / resolution.x * 2.0 - 1.0,
        1.0 - px.y / resolution.y * 2.0,        // y 翻转
    );
    var out: VsOut;
    out.pos = vec4<f32>(ndc, 0.0, 1.0);
    out.color = vec4<f32>(srgb_to_linear(color.rgb), color.a);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> { return in.color; }
"#;
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib gpu::tests`
Expected: PASS，含 `quad_shader_converts_srgb_to_linear`、`default_bg_cell_makes_no_quad`、`origin_shifts_every_quad_so_first_row_clears_the_menu_bar`

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/gpu.rs
git commit -m "fix(app): quad 着色器补 sRGB→线性 转换 (F80 前置)

surface 用 is_srgb() 挑格式,着色器输出会被硬件当线性值再编码。
egui(egui.wgsl linear_from_gamma_rgb)与 glyphon(shader.wgsl
srgb_to_linear)都转了,只有我们的 quad 原样透传——底色是纯黑时
看不出(0 在两个空间都是 0),底色一旦非黑,同一 token 在外壳和
终端里就是两个颜色,F80 直接验收失败。

守护测试 gpu::tests::quad_shader_converts_srgb_to_linear
(数值正确性只能人眼验,这里守转换不被重构删掉)"
```

---

## Task 2: `DefaultColors` 穿透 `palette::resolve`

**Files:**
- Modify: `crates/mullion-term/src/palette.rs:32-84`（常量 / `named_default` / `resolve`）
- Modify: `crates/mullion-term/src/palette.rs:86-150`（现有 6 个测试补实参）
- Test: `crates/mullion-term/src/palette.rs`（`mod tests`）

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-term/src/palette.rs` 的 `mod tests` 末尾追加：

```rust
    #[test]
    fn injected_defaults_replace_factory_values() {
        let colors = Colors::default();
        let d = DefaultColors {
            fg: Rgb::new(0xe4, 0xe6, 0xf0),
            bg: Rgb::new(0x14, 0x16, 0x1f),
        };
        assert_eq!(
            resolve(AnsiColor::Named(NamedColor::Background), &colors, d),
            Rgb::new(0x14, 0x16, 0x1f)
        );
        assert_eq!(
            resolve(AnsiColor::Named(NamedColor::Foreground), &colors, d),
            Rgb::new(0xe4, 0xe6, 0xf0)
        );
    }

    /// 注入只该动默认前景/背景,不该动 ANSI 16 色(那是另一套,F84 才可配)。
    #[test]
    fn injection_does_not_touch_ansi16() {
        let colors = Colors::default();
        let d = DefaultColors {
            fg: Rgb::new(0xe4, 0xe6, 0xf0),
            bg: Rgb::new(0x14, 0x16, 0x1f),
        };
        assert_eq!(
            resolve(AnsiColor::Named(NamedColor::Red), &colors, d),
            Rgb::new(205, 0, 0)
        );
    }

    /// OSC 覆盖(将来的 OSC 10/11)优先级仍高于注入的默认色。
    #[test]
    fn osc_override_still_wins_over_injected_defaults() {
        let mut colors = Colors::default();
        colors[NamedColor::Background] = Some(AnsiRgb { r: 1, g: 2, b: 3 });
        let d = DefaultColors {
            fg: Rgb::new(0xe4, 0xe6, 0xf0),
            bg: Rgb::new(0x14, 0x16, 0x1f),
        };
        assert_eq!(
            resolve(AnsiColor::Named(NamedColor::Background), &colors, d),
            Rgb::new(1, 2, 3)
        );
    }
```

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test -p mullion-term --lib palette`
Expected: FAIL，编译错误 `cannot find struct DefaultColors` / `resolve takes 2 arguments but 3 were supplied`

- [ ] **Step 3: 实现**

把 `crates/mullion-term/src/palette.rs:32-34` 替换为：

```rust
/// 默认前景 / 背景的**出厂值**(无注入、无 OSC 覆盖时)。
pub const DEFAULT_FG: Rgb = Rgb::new(0xcc, 0xcc, 0xcc);
pub const DEFAULT_BG: Rgb = Rgb::new(0x00, 0x00, 0x00);

/// 一对可注入的默认前景/背景色(F80)。
///
/// 「默认前景/背景」本就是 VT 协议概念——SGR 39/49 说的是它,OSC 10/11 改的也是它,
/// 所以它归 term 所有。app 层的主题只是**注入**一组值进来,方向仍是 app → term:
/// 这里只出现 term 自己的 `Rgb`,没有任何 UI 类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DefaultColors {
    pub fg: Rgb,
    pub bg: Rgb,
}

impl Default for DefaultColors {
    fn default() -> Self {
        Self {
            fg: DEFAULT_FG,
            bg: DEFAULT_BG,
        }
    }
}
```

把 `named_default` 与 `resolve`（原 62-84 行）替换为：

```rust
fn named_default(named: NamedColor, d: DefaultColors) -> Rgb {
    match named as usize {
        i @ 0..=15 => ANSI16[i],
        256 => d.fg, // Foreground
        257 => d.bg, // Background
        // Bright*/Dim*/Cursor 等 MVP 先落默认前景。注意:注入之后这些会**跟着**
        // 变成主题前景色(本轮正是想要的,光标与文本同色系)。将来若要单独调
        // 光标色,源头是这一行,不是 Theme。
        _ => d.fg,
    }
}

/// 把一个单元格颜色解析成具体 RGB:OSC 覆盖优先,否则用默认表。
///
/// `d` 是可注入的默认前景/背景(F80 主题色)。不传主题时用 `DefaultColors::default()`。
pub fn resolve(color: AnsiColor, colors: &Colors, d: DefaultColors) -> Rgb {
    match color {
        AnsiColor::Spec(rgb) => from_ansi(rgb),
        AnsiColor::Indexed(i) => match colors[i as usize] {
            Some(over) => from_ansi(over),
            None => indexed_default(i),
        },
        AnsiColor::Named(named) => match colors[named] {
            Some(over) => from_ansi(over),
            None => named_default(named, d),
        },
    }
}
```

现有 6 个测试里所有 `resolve(x, &colors)` 调用改成 `resolve(x, &colors, DefaultColors::default())`——共 5 处（`named_red_resolves_to_ansi_red`、`indexed_matches_named_for_first_16`、`spec_passes_through`、`osc_override_wins_over_default`、`default_fg_bg_are_distinct` 里两处）。断言一律不动。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-term --lib palette`
Expected: PASS，9 个测试全过（原 6 + 新 3）

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-term/src/palette.rs
git commit -m "feat(term): 默认前景/背景色改为可注入 DefaultColors (F80)

const 原本在 resolve → named_default 两跳外被吃掉,Emulator 光存字段
传不进去,必须改 resolve 签名。DEFAULT_FG/BG 保留为出厂值。
注入类型是 term 自己的 Rgb,无 UI 类型泄漏,依赖方向不变。

现有 6 个测试补第三个实参,断言未动。
新增 injected_defaults_replace_factory_values /
injection_does_not_touch_ansi16 /
osc_override_still_wins_over_injected_defaults"
```

---

## Task 3: `Emulator` 持有并使用 `DefaultColors`

**Files:**
- Modify: `crates/mullion-term/src/emulator.rs:62-66`（struct）、`with_history`、`snapshot`
- Test: `crates/mullion-term/src/emulator.rs`（`mod tests`）

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-term/src/emulator.rs` 的 `mod tests` 末尾追加：

```rust
    /// F80:注入的默认色必须真的穿透 resolve 到达 snapshot。
    /// 只测 palette 层不够——中间隔着 Emulator 的字段和 snapshot 的调用。
    #[test]
    fn injected_default_colors_reach_snapshot() {
        let mut emu = Emulator::new(4, 2);
        emu.set_default_colors(Rgb::new(0xe4, 0xe6, 0xf0), Rgb::new(0x14, 0x16, 0x1f));
        emu.feed(b"x");
        let snap = emu.snapshot();
        let cell = &snap.row(0)[0];
        assert_eq!(cell.ch, 'x');
        assert_eq!(cell.fg, Rgb::new(0xe4, 0xe6, 0xf0), "前景应是注入值");
        assert_eq!(cell.bg, Rgb::new(0x14, 0x16, 0x1f), "背景应是注入值");
    }

    /// 不注入时保持出厂值,老行为不变。
    #[test]
    fn without_injection_snapshot_uses_factory_defaults() {
        let mut emu = Emulator::new(4, 2);
        emu.feed(b"x");
        let snap = emu.snapshot();
        let cell = &snap.row(0)[0];
        assert_eq!(cell.fg, crate::palette::DEFAULT_FG);
        assert_eq!(cell.bg, crate::palette::DEFAULT_BG);
    }
```

若 `mod tests` 里还没有 `Rgb` 的引入，在该 mod 顶部加 `use crate::snapshot::Rgb;`。

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test -p mullion-term --lib emulator::tests::injected_default_colors_reach_snapshot`
Expected: FAIL，编译错误 `no method named set_default_colors`

- [ ] **Step 3: 实现**

`crates/mullion-term/src/emulator.rs:62-66` 的 struct 改为：

```rust
pub struct Emulator {
    term: Term<PtyWriteCollector>,
    parser: Processor,
    collector: PtyWriteCollector,
    /// F80:可注入的默认前景/背景色。默认为出厂值,app 层挂主题后覆盖。
    defaults: palette::DefaultColors,
}
```

在 `with_history` 构造 `Self { ... }` 的字段列表里追加 `defaults: palette::DefaultColors::default(),`。

在 `impl Emulator` 里，`take_pty_writes` 之后追加：

```rust
    /// 注入默认前景/背景色(F80 主题)。影响所有「未显式指定颜色」的格子,
    /// 即 SGR 39/49 语义下的默认色。OSC 10/11 运行时覆盖仍优先于此。
    pub fn set_default_colors(&mut self, fg: Rgb, bg: Rgb) {
        self.defaults = palette::DefaultColors { fg, bg };
    }
```

`snapshot()` 里两行 `palette::resolve` 改为：

```rust
                    fg: palette::resolve(cell.fg, colors, self.defaults),
                    bg: palette::resolve(cell.bg, colors, self.defaults),
```

确认文件顶部已 `use crate::snapshot::Rgb;`（`snapshot()` 已在构造 `SnapCell`，`Rgb` 若未直接引入则给 `set_default_colors` 签名补上）。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-term`
Expected: PASS，全部通过（含 `pty_write_is_collected` — T1 守护）

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-term/src/emulator.rs
git commit -m "feat(term): Emulator 持有 DefaultColors + set_default_colors (F80)

snapshot() 的两处 resolve 改用字段而非出厂常量。
守护测试 emulator::tests::injected_default_colors_reach_snapshot
(注入穿透 resolve 到 snapshot)、without_injection_snapshot_uses_factory_defaults
(不注入时老行为不变);T1 的 pty_write_is_collected 一并跑过"
```

---

## Task 4: 新增 `theme.rs`

**Files:**
- Create: `crates/mullion-app/src/theme.rs`
- Modify: `crates/mullion-app/src/lib.rs`（挂模块）
- Test: `crates/mullion-app/src/theme.rs`（`mod tests`）

- [ ] **Step 1: 挂模块并写出完整的 theme.rs**

先在 `crates/mullion-app/src/lib.rs` 里，按既有 `pub mod` 的字母序位置追加一行：

```rust
pub mod theme;
```

创建 `crates/mullion-app/src/theme.rs`：

```rust
//! F80:全局视觉 token。egui 外壳与 glyphon/wgpu 终端层共用同一套色板。
//!
//! **不放 `ui/` 下**:终端渲染层(`gpu.rs`/`text.rs`/`app.rs` 的 clear 色)也要用它,
//! 不只是 egui 外壳。
//!
//! 依赖方向:本模块属于 `mullion-app`(该 crate 本就依赖 egui/wgpu),跨 crate 方向上
//! 只向下用到 `mullion_term` 的 `Rgb` / `DefaultColors`。**不得**把 `Theme` 或任何
//! egui/wgpu 类型漏进 `mullion-term`。
//!
//! 色板全表见 `docs/superpowers/specs/2026-07-29-ui-visual-baseline-design.md` §2,
//! 改色前先查表,不要重新调色。

use mullion_term::palette::DefaultColors;
use mullion_term::snapshot::Rgb;

/// 一套完整的视觉 token。const 构造,零运行时开销。
pub struct Theme {
    // --- 结构色(§2.1) ---
    pub window_bg: Rgb,
    /// 标题栏色。F85 自绘标题栏已否决,保留 token 备将来重提。
    pub bar_title: Rgb,
    pub bar_menu: Rgb,
    /// 工具栏(F82,随分屏切片)。
    pub bar_tool: Rgb,
    pub bar_status: Rgb,
    pub panel_bg: Rgb,
    /// pane 标题条(F83,随分屏切片)。
    pub panel_head: Rgb,
    /// 凹槽:分段控件底、快捷键徽标、滑轨。
    pub sunken_bg: Rgb,
    /// 描边不透明度。描边色恒为白,只调 alpha(§2.1 的 rgba(255,255,255,0.06))。
    pub stroke_alpha: u8,

    // --- 前景灰阶(§2.2) ---
    pub fg: Rgb,
    pub fg_strong: Rgb,
    pub fg_mid: Rgb,
    pub fg_muted: Rgb,
    pub fg_dim: Rgb,
    pub fg_dimmer: Rgb,
    pub fg_faint: Rgb,
    pub fg_ghost: Rgb,

    // --- 语义色(§2.3) ---
    pub accent: Rgb,
    pub accent_fg: Rgb,
    pub ok: Rgb,
    pub warn: Rgb,
    pub info: Rgb,
    pub danger: Rgb,

    // --- 终端色(§2.4) ---
    pub term_bg: Rgb,
    pub term_fg: Rgb,
}

/// 出厂主题。F84 做主题切换时,这里会多出同类型的兄弟常量。
pub const MULLION_DARK: Theme = Theme {
    window_bg: Rgb::new(0x12, 0x14, 0x1c),
    bar_title: Rgb::new(0x1e, 0x20, 0x28),
    bar_menu: Rgb::new(0x18, 0x1a, 0x22),
    bar_tool: Rgb::new(0x15, 0x18, 0x22),
    bar_status: Rgb::new(0x18, 0x1b, 0x26),
    panel_bg: Rgb::new(0x14, 0x16, 0x1f),
    panel_head: Rgb::new(0x19, 0x1c, 0x27),
    sunken_bg: Rgb::new(0x0e, 0x10, 0x18),
    stroke_alpha: 15, // 0.06 × 255 ≈ 15

    fg: Rgb::new(0xe4, 0xe6, 0xf0),
    fg_strong: Rgb::new(0xd3, 0xd6, 0xea),
    fg_mid: Rgb::new(0xc7, 0xca, 0xe0),
    fg_muted: Rgb::new(0xa9, 0xae, 0xc2),
    fg_dim: Rgb::new(0x9a, 0xa0, 0xb8),
    fg_dimmer: Rgb::new(0x8a, 0x90, 0xa8),
    fg_faint: Rgb::new(0x56, 0x5b, 0x70),
    fg_ghost: Rgb::new(0x4b, 0x50, 0x66),

    accent: Rgb::new(0x8b, 0x95, 0xff),
    accent_fg: Rgb::new(0x0d, 0x0f, 0x16),
    ok: Rgb::new(0x7f, 0xd9, 0x9b),
    warn: Rgb::new(0xe0, 0xb7, 0x67),
    info: Rgb::new(0x7c, 0x9e, 0xff),
    danger: Rgb::new(0xe8, 0x11, 0x23),

    // 与 panel_bg 同值:终端就是最大的那块 panel。
    term_bg: Rgb::new(0x14, 0x16, 0x1f),
    term_fg: Rgb::new(0xe4, 0xe6, 0xf0),
};

/// **三处同源的唯一出口**(设计文档 §3.2/§6)。
///
/// 终端底色散落在三处:wgpu 的 clear 色、`gpu::quads_for` 的 `default_bg` 短路、
/// `Emulator` 注入的默认背景。`quads_for` 对「背景 == 默认背景」的格子跳过不画
/// quad(有意的性能优化,`gpu::tests::default_bg_cell_makes_no_quad` 守着),
/// 让 clear 色直接透出来——三者一旦失配,满屏空白格显示的是 clear 色而非主题色。
///
/// 所以 app 层这三处**一律**从本函数取值,**禁止**再直接引用
/// `mullion_term::palette::DEFAULT_FG/BG`。
pub fn term_default_colors(t: &Theme) -> DefaultColors {
    DefaultColors {
        fg: t.term_fg,
        bg: t.term_bg,
    }
}

/// 单个 sRGB 分量(0..=255)转线性(0.0..=1.0)。
///
/// surface 格式是 sRGB(`Gpu::new` 用 `is_srgb()` 挑的),`LoadOp::Clear` 给的值会被
/// 当作**线性**值再编码成 sRGB。要让清屏色在屏幕上正好是 `#14161f`,这里必须先转。
/// 公式与 egui(`egui.wgsl` 的 `linear_from_gamma_rgb`)、glyphon(`shader.wgsl` 的
/// `srgb_to_linear`)、我们的 `QUAD_WGSL` 完全一致——四条路径同一套换算,才谈得上同色。
pub fn srgb_to_linear(c: u8) -> f64 {
    let c = c as f64 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// wgpu 清屏色,与 `t.term_bg` 同源。
pub fn clear_color(t: &Theme) -> wgpu::Color {
    let bg = t.term_bg;
    wgpu::Color {
        r: srgb_to_linear(bg.r),
        g: srgb_to_linear(bg.g),
        b: srgb_to_linear(bg.b),
        a: 1.0,
    }
}

/// `Rgb` → egui 颜色。
pub fn c32(c: Rgb) -> egui::Color32 {
    egui::Color32::from_rgb(c.r, c.g, c.b)
}

/// 主题描边(白 + 低 alpha)。
pub fn stroke(t: &Theme) -> egui::Stroke {
    egui::Stroke::new(1.0, egui::Color32::from_white_alpha(t.stroke_alpha))
}

/// 把主题写进 egui 的 `Visuals`。启动时对 egui ctx 调一次。
///
/// 只设 `Visuals`,不碰 `Spacing`——栏高由各 panel 自己的 `Frame` 内边距决定
/// (见 `ui::chrome`),混在一起改会让两边互相打架。
pub fn apply_egui(ctx: &egui::Context, t: &Theme) {
    let mut v = egui::Visuals::dark();

    v.panel_fill = c32(t.bar_menu);
    v.window_fill = c32(t.bar_status);
    v.extreme_bg_color = c32(t.sunken_bg);
    v.faint_bg_color = c32(t.panel_head);
    v.window_stroke = stroke(t);
    v.hyperlink_color = c32(t.info);
    v.selection.bg_fill = c32(t.accent).gamma_multiply(0.35);
    v.selection.stroke = egui::Stroke::new(1.0, c32(t.fg));

    // 不用 override_text_color:那会把所有文字压成一个色,连带盖掉分级灰阶。
    // 逐状态设 fg_stroke,让常态/悬停/按下有层次。
    let round = egui::Rounding::same(7.0);
    v.widgets.noninteractive.bg_fill = c32(t.panel_bg);
    v.widgets.noninteractive.weak_bg_fill = c32(t.panel_bg);
    v.widgets.noninteractive.bg_stroke = stroke(t);
    v.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, c32(t.fg_muted));
    v.widgets.noninteractive.rounding = round;

    v.widgets.inactive.bg_fill = c32(t.sunken_bg);
    v.widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
    v.widgets.inactive.bg_stroke = stroke(t);
    v.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, c32(t.fg_muted));
    v.widgets.inactive.rounding = round;

    v.widgets.hovered.bg_fill = c32(t.panel_head);
    v.widgets.hovered.weak_bg_fill = c32(t.panel_head);
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, c32(t.accent));
    v.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, c32(t.fg));
    v.widgets.hovered.rounding = round;

    v.widgets.active.bg_fill = c32(t.accent);
    v.widgets.active.weak_bg_fill = c32(t.accent);
    v.widgets.active.bg_stroke = egui::Stroke::new(1.0, c32(t.accent));
    v.widgets.active.fg_stroke = egui::Stroke::new(1.0, c32(t.accent_fg));
    v.widgets.active.rounding = round;

    v.widgets.open.bg_fill = c32(t.sunken_bg);
    v.widgets.open.weak_bg_fill = c32(t.sunken_bg);
    v.widgets.open.bg_stroke = stroke(t);
    v.widgets.open.fg_stroke = egui::Stroke::new(1.0, c32(t.fg));
    v.widgets.open.rounding = round;

    ctx.set_visuals(v);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §3.2 三处同源之一:clear 色必须由 term_bg 推出,不能是另写的常量。
    #[test]
    fn clear_color_matches_term_bg() {
        let t = &MULLION_DARK;
        let c = clear_color(t);
        assert_eq!(c.r, srgb_to_linear(t.term_bg.r));
        assert_eq!(c.g, srgb_to_linear(t.term_bg.g));
        assert_eq!(c.b, srgb_to_linear(t.term_bg.b));
        assert_eq!(c.a, 1.0);
    }

    /// §3.2 三处同源之二/三:注入给 Emulator 与传给 quads_for 的值同出一源。
    #[test]
    fn term_defaults_match_theme() {
        let t = &MULLION_DARK;
        let d = term_default_colors(t);
        assert_eq!(d.fg, t.term_fg);
        assert_eq!(d.bg, t.term_bg);
    }

    /// clear 色是**线性**值:直接用 c/255 会比 egui 面板亮一截(两个世界)。
    /// #14161f 的 0x14 = 20,20/255 ≈ 0.0784,线性约 0.007——差了十倍,不是舍入误差。
    #[test]
    fn clear_color_is_linear_not_raw_srgb() {
        let c = clear_color(&MULLION_DARK);
        let raw = 0x14 as f64 / 255.0;
        assert!(
            c.r < raw / 5.0,
            "clear 色看着像原始 sRGB 分量({raw})而非线性值({}),终端底色会比外壳亮",
            c.r
        );
        assert!((c.r - 0.00699).abs() < 1e-4, "sRGB 0x14 的线性值应约 0.00699,实为 {}", c.r);
    }

    #[test]
    fn srgb_to_linear_endpoints_and_cutoff() {
        assert_eq!(srgb_to_linear(0), 0.0);
        assert!((srgb_to_linear(255) - 1.0).abs() < 1e-9);
        // 低端走线性段(c <= 0.04045,即 u8 <= 10)
        assert!((srgb_to_linear(10) - (10.0 / 255.0 / 12.92)).abs() < 1e-12);
    }

    /// 终端底色与 pane 底色同值——终端就是最大的那块 panel,不同值就是两个世界。
    #[test]
    fn term_bg_equals_panel_bg() {
        assert_eq!(MULLION_DARK.term_bg, MULLION_DARK.panel_bg);
    }
}
```

- [ ] **Step 2: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib theme`
Expected: PASS，5 个测试全过

若 `clear_color_is_linear_not_raw_srgb` 的 `0.00699` 断言差在 1e-4 之外，用实际打印值修正常量到 5 位小数——公式是权威，常量只是记录。

- [ ] **Step 3: 提交**

```bash
git add crates/mullion-app/src/theme.rs crates/mullion-app/src/lib.rs
git commit -m "feat(app): 新增 theme.rs,全局视觉 token 单一来源 (F80)

色板全表见设计文档 §2。term_default_colors 是三处同源的唯一出口,
app 层此后禁止直接引用 palette::DEFAULT_FG/BG。

clear_color 走 sRGB→线性 转换:surface 是 sRGB 格式,LoadOp::Clear
的值会被当线性值再编码,直接用 c/255 会比 egui 面板亮十倍。

测试:clear_color_matches_term_bg / term_defaults_match_theme /
clear_color_is_linear_not_raw_srgb / srgb_to_linear_endpoints_and_cutoff /
term_bg_equals_panel_bg"
```

---

## Task 5: `quads_for` 改收 `DefaultColors`（含光标色）

**Files:**
- Modify: `crates/mullion-app/src/gpu.rs:22-62`（签名 + 光标色）
- Modify: `crates/mullion-app/src/gpu.rs:340-425`（测试实参）
- Test: `crates/mullion-app/src/gpu.rs`（`mod tests`）

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-app/src/gpu.rs` 的 `mod tests` 末尾追加：

```rust
    /// 计划期发现:光标色原本硬编码 [0xcc,0xcc,0xcc],不引用任何常量,
    /// 改主题前景后会留一块旧灰。必须跟着 DefaultColors.fg 走。
    #[test]
    fn cursor_uses_injected_default_fg() {
        let fg = Rgb::new(0xe4, 0xe6, 0xf0);
        let bg = Rgb::new(0x14, 0x16, 0x1f);
        let mut snap = snap_1x1(bg);
        snap.cursor = Cursor {
            row: 0,
            col: 0,
            visible: true,
        };
        let quads = quads_for(&snap, (0.0, 0.0), 10.0, 20.0, DefaultColors { fg, bg });
        let cursor = quads.last().expect("光标可见时应有一个 quad");
        assert_eq!(cursor.color, [0xe4, 0xe6, 0xf0], "光标色应取注入的默认前景");
    }

    /// 非黑主题底色下,默认背景格同样不画 quad(靠 clear 色透出)。
    #[test]
    fn default_bg_cell_makes_no_quad_on_themed_bg() {
        let bg = Rgb::new(0x14, 0x16, 0x1f);
        let snap = snap_1x1(bg);
        let quads = quads_for(
            &snap,
            (0.0, 0.0),
            10.0,
            20.0,
            DefaultColors {
                fg: Rgb::new(0xe4, 0xe6, 0xf0),
                bg,
            },
        );
        assert!(
            quads.is_empty(),
            "背景 == 主题默认背景 的格子不该画 quad,否则白扔一块画面"
        );
    }
```

`mod tests` 顶部的 `use` 补上 `DefaultColors`：

```rust
    use mullion_term::palette::DefaultColors;
```

若 `snap_1x1` 返回的 `GridSnapshot` 里 `cursor` 字段不是 `visible: false`，`cursor_uses_injected_default_fg` 里的显式赋值仍然覆盖它，无需改 helper。

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test -p mullion-app --lib gpu::tests::cursor_uses_injected_default_fg`
Expected: FAIL，编译错误 `expected Rgb, found DefaultColors`

- [ ] **Step 3: 实现**

`crates/mullion-app/src/gpu.rs` 顶部 `use` 追加：

```rust
use mullion_term::palette::DefaultColors;
```

`quads_for` 的签名与函数体改为（只列改动的部分）：

```rust
/// 从快照生成需要画的色块:bg ≠ 默认 的格 + 选中格(反色,F18)+ 可见光标(块状)。
/// 纯函数,可单测。
///
/// `origin` 是终端区左上角的窗口像素坐标(egui 菜单栏/状态栏之间的中央区)。
/// 网格坐标一律相对该原点:传 `(0.0, 0.0)` 得到纯网格坐标(测试用),实际渲染
/// 传中央区原点,否则第 0 行画在窗口顶端、被菜单栏盖住。文字层
/// (`text::TextLayer::prepare`)必须用**同一个** origin,不然底色和字会错位。
///
/// `defaults` 必须来自 `theme::term_default_colors`(F80 三处同源),不要直接传
/// `palette::DEFAULT_*`——那样主题一换就和 clear 色失配。
pub fn quads_for(
    snap: &GridSnapshot,
    origin: (f32, f32),
    cell_w: f32,
    cell_h: f32,
    defaults: DefaultColors,
) -> Vec<Quad> {
```

函数体里 `} else if cell.bg == default_bg {` 改为 `} else if cell.bg == defaults.bg {`。

光标那段（原 54-62 行）改为：

```rust
    if snap.cursor.visible {
        quads.push(Quad {
            x: origin.0 + snap.cursor.col as f32 * cell_w,
            y: origin.1 + snap.cursor.row as f32 * cell_h,
            w: cell_w,
            h: cell_h,
            // MVP 块状光标用默认前景色。原本硬编码 0xcc,主题化后必须跟着走,
            // 否则新前景下光标是一块突兀的旧灰。
            color: [defaults.fg.r, defaults.fg.g, defaults.fg.b],
        });
    }
```

现有 5 处 `quads_for(..., Rgb::new(0, 0, 0))` 调用改为 `quads_for(..., DefaultColors::default())`（`gpu.rs` 测试里的 350/362/369/387/418 行）。这些测试断言的是出厂值行为，改成 `default()` 语义不变。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib gpu`
Expected: PASS，含 `cursor_uses_injected_default_fg`、`default_bg_cell_makes_no_quad`、`default_bg_cell_makes_no_quad_on_themed_bg`、`origin_shifts_every_quad_so_first_row_clears_the_menu_bar`、`quad_shader_converts_srgb_to_linear`

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/gpu.rs
git commit -m "refactor(app): quads_for 收 DefaultColors,光标色跟着主题走 (F80)

计划期发现 spec 漏了一处硬编码:光标色 [0xcc,0xcc,0xcc] 不引用任何
常量(grep DEFAULT_FG 找不到),改主题前景后会留一块旧灰。
签名从 default_bg: Rgb 换成 defaults: DefaultColors,背景短路与光标
同取一源。

测试 cursor_uses_injected_default_fg /
default_bg_cell_makes_no_quad_on_themed_bg(断言非黑底色下短路仍成立)"
```

---

## Task 6: app 侧接线（clear 色 / quads_for / Emulator 注入 / 文字默认色）

**Files:**
- Modify: `crates/mullion-app/src/text.rs:54-80`（`TextLayer::new`）、`text.rs:131`
- Modify: `crates/mullion-app/src/pane.rs:13-20`
- Modify: `crates/mullion-app/src/app.rs:1222-1228`、`app.rs:1293-1298`、`app.rs:598`、`TextLayer::new` 调用点
- Test: `crates/mullion-app/src/pane.rs`（新增 `mod tests`）

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-app/src/pane.rs` 末尾追加：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{term_default_colors, MULLION_DARK};

    /// §3.2 三处同源的第三处:Emulator 的注入值。前两处是编译期常量、好守,
    /// 这处发生在运行时接线,恰恰最容易漏改——所以单独守一条。
    #[test]
    fn terminal_defaults_come_from_theme() {
        let d = term_default_colors(&MULLION_DARK);
        let pane = Pane::new(PaneId(1), 4, 2, d);
        let snap = pane.emulator.snapshot();
        let cell = &snap.row(0)[0];
        assert_eq!(cell.bg, MULLION_DARK.term_bg, "空格背景应是主题底色");
        assert_eq!(cell.fg, MULLION_DARK.term_fg, "空格前景应是主题前景");
    }
}
```

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test -p mullion-app --lib pane`
Expected: FAIL，编译错误 `Pane::new takes 3 arguments but 4 were supplied`

- [ ] **Step 3: 实现**

**(a) `crates/mullion-app/src/pane.rs`** 的 `impl Pane` 改为：

```rust
impl Pane {
    /// `defaults` 来自 `theme::term_default_colors`(F80 三处同源),不要传
    /// `palette::DEFAULT_*`——那样终端底色会和 clear 色失配。
    pub fn new(
        id: PaneId,
        cols: u16,
        rows: u16,
        defaults: mullion_term::palette::DefaultColors,
    ) -> Self {
        let mut emulator = Emulator::new(cols, rows);
        emulator.set_default_colors(defaults.fg, defaults.bg);
        Self { id, emulator }
    }
}
```

**(b) `crates/mullion-app/src/app.rs:598`**：

```rust
                let pane = Pane::new(
                    PaneId(1),
                    cols,
                    rows,
                    crate::theme::term_default_colors(&crate::theme::MULLION_DARK),
                );
```

**(c) `crates/mullion-app/src/app.rs:1222-1228`** 的 `quads_for` 调用：

```rust
            let quads = quads_for(
                &snap,
                origin,
                a.text.cell_w,
                a.text.cell_h,
                crate::theme::term_default_colors(&crate::theme::MULLION_DARK),
            );
```

**(d) `crates/mullion-app/src/app.rs:1293-1298`** 的 clear：

```rust
                    load: wgpu::LoadOp::Clear(crate::theme::clear_color(
                        &crate::theme::MULLION_DARK,
                    )),
```

**(e) `crates/mullion-app/src/text.rs`** —— `TextLayer` struct 追加字段：

```rust
    /// F80:glyphon 的兜底文字色(span 未带显式色时用)。当前每个 span 都带色,
    /// 取不到;留着是为了主题一换就整体跟走,不留一处旧灰的潜伏陷阱。
    default_fg: Rgb,
```

`TextLayer::new` 签名追加参数并在构造里赋值：

```rust
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        font_px: f32,
        default_fg: Rgb,
    ) -> Self {
```

构造 `Self { ... }` 的字段列表末尾加 `default_fg,`。

`text.rs:131` 的 `default_color` 改为：

```rust
                default_color: glyphon::Color::rgb(
                    self_default_fg.r,
                    self_default_fg.g,
                    self_default_fg.b,
                ),
```

注意：该处在 `self.buffers.iter().enumerate().map(...)` 闭包内，`self` 已被 `&self.buffers` 借出，不能再借 `self.default_fg`。在 `let cell_h = self.cell_h;` 那一行**旁边**先取出副本：

```rust
        let cell_h = self.cell_h;
        let self_default_fg = self.default_fg;
```

`text.rs` 顶部 `use` 确认已有 `mullion_term::snapshot::Rgb`（`row_to_spans` 已用 `SnapCell`，若 `Rgb` 未引入则补 `use mullion_term::snapshot::{GridSnapshot, Rgb};`）。

**(f) `TextLayer::new` 的调用点**（在 `app.rs` 里，grep `TextLayer::new` 定位）追加第五个实参：

```rust
            crate::theme::MULLION_DARK.term_fg,
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app`
Expected: PASS，含 `pane::tests::terminal_defaults_come_from_theme`

- [ ] **Step 5: 确认没有漏网的直接引用**

Run: `grep -rn "palette::DEFAULT_FG\|palette::DEFAULT_BG\|0xcc, 0xcc, 0xcc" crates/mullion-app/src/`
Expected: 只剩 `mod tests` 里的出现（测试用出厂值做基准是合理的）。**非测试代码里若还有，就是漏改。**

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-app/src/pane.rs crates/mullion-app/src/app.rs crates/mullion-app/src/text.rs
git commit -m "feat(app): 终端默认色/clear 色/文字兜底色全部改从 Theme 取 (F80)

§3.2 的三处同源在 app 侧接线完成,另补两处计划期发现的硬编码
(gpu 光标色见上一提交、text.rs 的 glyphon default_color)。
非测试代码里不再有 palette::DEFAULT_* 与 0xcc 字面量。

守护测试 pane::tests::terminal_defaults_come_from_theme —— 三处同源里
运行时接线那一处,前两处是编译期常量、theme::tests 已守"
```

---

## Task 7: egui 外壳挂主题 + 两个 panel 的 Frame

**Files:**
- Modify: `crates/mullion-app/src/app.rs:518-521`（挂 `apply_egui`）
- Modify: `crates/mullion-app/src/ui/chrome.rs:5`、`chrome.rs:45`（panel Frame）

- [ ] **Step 1: 挂主题**

`crates/mullion-app/src/app.rs:520` 的 `install_cjk_font` 之后追加一行：

```rust
        crate::ui::install_cjk_font(&egui_ctx);
        crate::theme::apply_egui(&egui_ctx, &crate::theme::MULLION_DARK);
```

- [ ] **Step 2: 给菜单栏与状态栏各自的 Frame**

`Visuals::panel_fill` 只有一个值，但菜单栏 `#181a22` 与状态栏 `#181b26` 不同色，必须各自设 `Frame`。

`crates/mullion-app/src/ui/chrome.rs` 顶部 `use` 改为：

```rust
use super::UiState;
use crate::theme::{self, Theme};
```

`top_menu` 的签名与 panel 构造改为：

```rust
pub fn top_menu(ctx: &egui::Context, t: &Theme, ui_state: &mut UiState, connected: bool) {
    // 菜单栏与状态栏底色不同(§2.1),Visuals::panel_fill 只有一个值,
    // 所以各自带 Frame。栏高由 inner_margin 决定(目标 30px),精确值人眼验。
    egui::TopBottomPanel::top("menu")
        .frame(
            egui::Frame::none()
                .fill(theme::c32(t.bar_menu))
                .inner_margin(egui::Margin::symmetric(6.0, 3.0))
                .stroke(theme::stroke(t)),
        )
        .show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
```

（其余菜单项内容不动，闭包结尾照旧。）

- [ ] **Step 3: 编译确认**

Run: `cargo build -p mullion-app`
Expected: 报 `top_menu` 调用点实参不足 —— 下一个 Task 一起补。先确认错误只出在 `ui/mod.rs:112`。

- [ ] **Step 4: 暂不提交**

状态栏的 Frame 与 F81 改造是同一处代码，合并进 Task 8 一次改完，避免中间态编译不过的提交。

---

## Task 8: F81 状态栏两栏信息架构

**Files:**
- Modify: `crates/mullion-app/src/ui/chrome.rs:44-54`（`status_bar`）
- Modify: `crates/mullion-app/src/ui/mod.rs:95-113`（`build_ui` 参数）
- Modify: `crates/mullion-app/src/app.rs:915-919`（删掉 status 串）、`app.rs:949`、`app.rs:1165`、`app.rs:1180`
- Test: `crates/mullion-app/src/ui/chrome.rs`（新增 `mod tests`）

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-app/src/ui/chrome.rs` 末尾追加：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_text_connected_single_pane() {
        let (left, right) = status_text(1, true);
        assert_eq!(left, "1 屏 · 已连接");
        assert_eq!(right, "UTF-8");
    }

    #[test]
    fn status_text_disconnected() {
        let (left, _) = status_text(1, false);
        assert_eq!(left, "1 屏 · 未连接");
    }

    /// 分屏(F30)落地后 N 会变;格式化提前按多屏写好,免得那时再动状态栏。
    #[test]
    fn status_text_multi_pane() {
        let (left, _) = status_text(4, true);
        assert_eq!(left, "4 屏 · 已连接");
    }

    /// 色点不进字符串:它要按连接态分别用 ok / fg_faint 上色,
    /// 混在文本里就只能是一个颜色。
    #[test]
    fn status_text_carries_no_dot_glyph() {
        let (left, right) = status_text(1, true);
        assert!(!left.contains('●'), "色点应由调用方单独上色绘制");
        assert!(!right.contains('●'));
    }
}
```

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test -p mullion-app --lib ui::chrome`
Expected: FAIL，编译错误 `cannot find function status_text`

- [ ] **Step 3: 实现**

`crates/mullion-app/src/ui/chrome.rs` 的 `status_bar` 整块（原 40-54 行）替换为：

```rust
/// F81 状态栏两栏文案。纯函数,可单测。
///
/// 左栏 `{N} 屏 · {连接态}`,右栏编码。**色点不进字符串**——它要按连接态分别用
/// `ok` / `fg_faint` 上色,塞进文本就只能是一个颜色。
///
/// 「远端 SSH 版本」本来在设计里,复核后砍掉:russh 的 `remote_sshid()` 只在带
/// `session` 参数的 Handler 回调里够得着,F3 用的 `check_server_key` 拿不到,
/// 要做是跨 crate 事件接线,不该混进纯视觉改动(见设计文档 §3.4)。
pub fn status_text(panes: usize, connected: bool) -> (String, String) {
    let left = format!(
        "{} 屏 · {}",
        panes,
        if connected { "已连接" } else { "未连接" }
    );
    (left, "UTF-8".to_string())
}

/// `last_error`(F3 落盘失败等)必须总有个展示位:它可能是在会话管理器/编辑器
/// 都已关闭之后才产生的(如主机密钥确认后 `ConnectOk` 顺手关掉了会话管理器),
/// 那两处的 `last_error` 渲染此时根本不会被调用到(复核 A4)。状态栏常驻,
/// 不受那两个弹窗开关状态影响,兜底展示。
pub fn status_bar(
    ctx: &egui::Context,
    t: &Theme,
    panes: usize,
    connected: bool,
    last_error: Option<&str>,
) {
    let (left, right) = status_text(panes, connected);
    egui::TopBottomPanel::bottom("status")
        .frame(
            egui::Frame::none()
                .fill(theme::c32(t.bar_status))
                .inner_margin(egui::Margin::symmetric(8.0, 2.0))
                .stroke(theme::stroke(t)),
        )
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                let dot = if connected { t.ok } else { t.fg_faint };
                ui.colored_label(theme::c32(dot), "●");
                ui.colored_label(theme::c32(t.fg_faint), left);
                // last_error 必须可见:右对齐区先画它,再画常规右栏。
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(err) = last_error {
                        ui.colored_label(theme::c32(t.danger), err);
                        ui.separator();
                    }
                    ui.colored_label(theme::c32(t.fg_faint), right);
                });
            });
        });
}
```

**(b) `crates/mullion-app/src/ui/mod.rs`** —— `build_ui` 的 `status: &str` 参数换成 `panes: usize`（参数位置不变），两行调用改为：

```rust
    chrome::top_menu(ctx, t, ui_state, connected);
    chrome::status_bar(ctx, t, panes, connected, ui_state.last_error.as_deref());
```

`build_ui` 签名还要加 `t: &crate::theme::Theme` 参数，放在 `ctx` 之后。

**(c) `crates/mullion-app/src/app.rs:915-919`** —— 删掉 `status` 字符串构造：

```rust
                            let pane = self.conn.as_ref().map(|c| &c.pane);
                            let connected = self.conn.is_some();
```

（即删掉 `let status = if connected { ... };` 整个 let 块。）

`app.rs:949` 的 `&status,` 改为 `1,`（分屏未落地，pane 数恒为 1；F30 落地后改成真实计数）。

`app.rs:1165` 的 `status: &str,` 改为 `panes: usize,`。

`app.rs:1180` 的 `status,` 改为 `panes,`，并在同一 `build_ui(` 调用的 `ctx,` 之后插入 `&crate::theme::MULLION_DARK,`。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app`
Expected: PASS，含 4 个 `ui::chrome::tests::status_text_*`

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/ui/chrome.rs crates/mullion-app/src/ui/mod.rs crates/mullion-app/src/app.rs
git commit -m "feat(app): egui 外壳挂主题 + 状态栏改两栏信息架构 (F80/F81)

菜单栏与状态栏底色不同(#181a22 / #181b26),Visuals::panel_fill 只有
一个值,所以各自带 Frame。

F81 抽出纯函数 status_text(panes, connected) -> (左, 右);色点不进
字符串,由调用方按连接态用 ok / fg_faint 分别上色。app 不再自己拼
「● 已连接」串,build_ui 的 status: &str 换成 panes: usize。
last_error 仍在右对齐区优先展示,兜底职责未动。

测试 ui::chrome::tests::status_text_{connected_single_pane,disconnected,
multi_pane,carries_no_dot_glyph}"
```

---

## Task 9: 菜单正名 + 占位接编号 + spec 勘误

**Files:**
- Modify: `crates/mullion-app/src/ui/chrome.rs:7,25,28`
- Modify: `docs/superpowers/specs/2026-07-29-ui-visual-baseline-design.md`

- [ ] **Step 1: 菜单正名与占位编号**

`crates/mullion-app/src/ui/chrome.rs` 三处字面量：

- 第 7 行 `ui.menu_button("对话", |ui| {` → `ui.menu_button("会话", |ui| {`
- 第 25 行 `egui::Button::new("(切片 B 实现)")` → `egui::Button::new("(F30 分屏 · 后续切片)")`
- 第 28 行 `egui::Button::new("(切片 C:字体等)")` → `egui::Button::new("(F84 设置 · 后续切片)")`

- [ ] **Step 2: 编译确认**

Run: `cargo build -p mullion-app`
Expected: 成功，无警告

- [ ] **Step 3: spec 勘误两处**

在设计文档 `docs/superpowers/specs/2026-07-29-ui-visual-baseline-design.md` 的 §2.5 里，把

```
终端行高：**1.2**（不是 mockup 的 1.65），后续由 F21 做成可配。
```

改为

```
终端行高：**1.25**（代码 `text.rs:67` 的现值，落在合理区间 1.1~1.25 内；
不是 mockup 的 1.65）。改它会变动行数、踩 T4/F34，收益为零，本轮不动。
后续由 F21 做成可配。
```

在 §3.2 的三处同源清单之后，追加一段（这是计划期的新发现，必须回写 spec）：

```markdown
**计划期追加：实为五处，且有一个色彩空间前置问题**

除上面三处，另有两处 `0xcc` 字面量硬编码，**不引用** `palette::DEFAULT_FG`，
grep 常量名找不到：`gpu.rs` 的光标色块、`text.rs` 的 glyphon `default_color`。

更要紧的是：surface 是 sRGB 格式（`gpu.rs` 用 `is_srgb()` 挑的），
着色器输出会被硬件当**线性**值再编码。egui（`egui.wgsl` 的
`linear_from_gamma_rgb`）与 glyphon（`shader.wgsl` 的 `srgb_to_linear`）都做了
转换，**只有我们的 quad 着色器原样透传**。底色是纯黑时看不出（0 在两个空间都是
0），底色一旦非黑，同一个 token 在外壳和终端里就是两个颜色 —— F80 的人工验收
第一条「没有两个世界的割裂感」会直接失败。

所以 `clear_color` 必须返回**线性**值，且 `QUAD_WGSL` 必须补 `srgb_to_linear`。
修完后现存 ANSI 彩色格会变暗（回到正确值），这是修正不是回归。
```

- [ ] **Step 4: 提交**

```bash
git add crates/mullion-app/src/ui/chrome.rs docs/superpowers/specs/2026-07-29-ui-visual-baseline-design.md
git commit -m "chore(app): 菜单「对话」正名「会话」+ 占位接 spec 编号 (F80)

占位从「(切片 B 实现)」改成「(F30 分屏 · 后续切片)」,零成本的欠账
可见性——每次启动都在眼前,比 todo 列表难丢。

spec 勘误两处:行高记为代码现值 1.25(改它踩 T4/F34、收益为零);
补记计划期发现的第四/五处硬编码与 quad 着色器 sRGB 前置问题"
```

---

## Task 10: 全绿 + 交付

**Files:**
- Modify: `Cargo.toml`（`workspace.package.version`）

- [ ] **Step 1: 跑全绿**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

Expected: 所有 `test result` 行为 `ok`，无 `FAILED`/`panicked`；clippy 与 fmt 无输出。

**「绿」的定义**：三条全过才算。只跑单个 crate 不叫绿。

重点确认这几个守护测试在列且通过：
- `emulator::tests::pty_write_is_collected`（T1）
- `app::tests::reflow_emits_resize`（T4）
- `app::tests::redraw_is_frame_capped`（T3）
- `frame::tests::*`（T7）
- `input_route::tests::terminal_keyboard_is_never_fed_to_egui_so_tab_cannot_steal_focus`（T8）
- `gpu::tests::origin_shifts_every_quad_so_first_row_clears_the_menu_bar`（F34）

- [ ] **Step 2: 升 patch 版本**

`Cargo.toml` 的 `workspace.package.version` 第三位 +1（当前 `0.1.10` → `0.1.11`）。

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: 版本 0.1.11(F80 视觉基线 + F81 状态栏两栏)"
```

- [ ] **Step 3: 交叉编译并做依赖验收**

```bash
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app
```

按 `docs/cross-compile-windows.md` 做 objdump 验收。出现 `libgcc_s_seh-1.dll` 或
`libwinpthread-1.dll` 即为**不合格**，必须修，不许发。

- [ ] **Step 4: 发 Release**

```bash
cd target/x86_64-pc-windows-gnu/release
sha256sum mullion.exe > mullion.exe.sha256
HTTPS_PROXY=http://127.0.0.1:7890 gh release create v0.1.11 \
  mullion.exe mullion.exe.sha256 -t "v0.1.11" -F notes.md --repo kilobitcy/Mullion
```

**Release 标题只能是纯版本号 `v0.1.11`** —— 不带破折号、不带摘要、不带 emoji。

`notes.md` 必须含下面这份人工验收清单（无头环境验不了，见 CLAUDE.md「你无法验证的东西」）：

```markdown
## 人工验收清单（本版全是视觉改动，几乎全靠人眼）

**这版改了什么**：终端底色从纯黑改为 `#14161f`，外壳与终端统一到同一套深色 token；
状态栏改成左右两栏；顺带修了一个 quad 着色器的 sRGB 缺陷。

- [ ] **外壳与终端是不是一个世界** —— 菜单栏 / 状态栏 / 终端底，三块颜色应该像同一
      套设计里出来的，没有哪一块明显偏亮或偏蓝。**这条是 F80 的核心验收，不过就是没做成。**
- [ ] **终端底色是否真的是 `#14161f`** —— 截图取色确认，不要凭感觉。偏亮说明 sRGB
      转换没生效（这版专门修了它）。
- [ ] **ANSI 彩色字比上一版暗** —— 这是**预期的修正**，不是回归。上一版所有彩色
      色块因为缺 sRGB 转换而偏亮。
- [ ] **光标块** —— 颜色应与正文前景一致（`#e4e6f0`），不是旧的 `#cccccc` 灰。
- [ ] **选中反色（F18）** —— 划选一段文字，底变前景色、字变背景色，两边都要能看清。
- [ ] **状态栏** —— 左「● 1 屏 · 已连接」（色点连接时绿、断开时灰），右「UTF-8」。
- [ ] **状态栏报错** —— 触发一次 F3 落盘失败之类的错误，红色错误文字应出现在右侧且
      不被「UTF-8」挤掉。
- [ ] **菜单** —— 首项应显示「会话」（原「对话」）；「分屏」下拉显示
      「(F30 分屏 · 后续切片)」，「配置」下拉显示「(F84 设置 · 后续切片)」。
- [ ] **首行不被菜单栏遮住**（F34 回归）—— 连上后第一行输出完整可见。
- [ ] **全屏 TUI 不闪**（T2/N3 红线）—— 远端跑 `claude` 或 `htop`，底色换了之后
      仍然不撕裂、不抖。
- [ ] **CJK 清晰度** —— 中文在新前景色下不发虚、不重叠，宽字符仍占两格。
- [ ] **深色对比度** —— 状态栏的 `fg_faint`(`#565b70`) 在 `bar_status`(`#181b26`)
      上是否还读得清；读不清就反馈，色板可调。
```

- [ ] **Step 5: 报告**

给出 Release 链接 + sha256 + 上面的验收清单。

---

## 自审

**Spec 覆盖**（逐节对照设计文档）：

| Spec 节 | 覆盖它的 Task |
|---|---|
| §2.1~§2.4 色板 token 全表 | Task 4（`Theme` + `MULLION_DARK` 逐字段） |
| §2.5 尺寸节奏 | Task 7/8 的 `inner_margin`；行高在 Task 9 勘误为 1.25 |
| §3.1 新增 `theme.rs`（不放 `ui/` 下） | Task 4 |
| §3.2 三处同源 | Task 4（出口）+ Task 5（quads_for）+ Task 6（app 接线）；**追加的两处**在 Task 5/6 |
| §3.3 注入放 Emulator + 注入链路 | Task 2（`resolve` 签名）+ Task 3（Emulator） |
| §3.3 Cursor 兜底副作用 | Task 2 的 `named_default` 注释 + Task 5 的光标测试 |
| §3.4 F81 状态栏 + 砍掉 SSH 版本 | Task 8 |
| §3.5 占位接编号 + 菜单正名 | Task 9 |
| §3.6 本轮不做 | 全程未碰渲染管线结构、输入路由、分屏、三个弹窗结构 |
| §4 冻结规格（F82/F83/F84/SFTP） | 不实现——`bar_tool`/`panel_head` token 已就位备用 |
| §5 F85 已否决 | 不实现；`bar_title` token 保留 |
| §6 测试策略 | Task 1~8 逐条落实，另加 4 条计划期新增 |

**未被 spec 预见、本计划新增的**：Task 1（quad 着色器 sRGB）。理由已写进计划开头与 Task 9 的 spec 回写——不修则 F80 的验收标准无法达成，属于「请求直接需要」而非顺手优化。

**类型一致性**：`DefaultColors { fg, bg }` 在 Task 2 定义，Task 3/5/6 使用，字段名全程一致；`term_default_colors(&Theme) -> DefaultColors` 在 Task 4 定义、Task 5/6 使用；`status_text(usize, bool) -> (String, String)` 在 Task 8 定义并测试。`Pane::new` 在 Task 6 从 3 参改 4 参，测试与调用点同批改。

**无占位符**：每个改动步骤都给了可直接粘贴的完整代码；每条命令都给了预期输出。

**已知的验证边界**（诚实标注）：
- `QUAD_WGSL` 的 sRGB 公式**数值正确性**无法在无头环境验证，Task 1 的测试只守「转换没被删掉」。真正判定靠 Release 验收清单第 2、3 条。
- 栏高 30px/24px 由 `inner_margin` 间接决定，egui 会按字体高度撑开；**精确像素值只能人眼验**，计划里没有断言它。
- 所有「观感」类结论（是否割裂、是否不闪、CJK 是否清晰）一律不在自动化范围内。
