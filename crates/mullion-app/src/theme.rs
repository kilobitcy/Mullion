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
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    // --- 结构色(§2.1) ---
    /// 窗口最外层背景。设计文档 §2.1 标为「窗口底」,但当前代码没有路径绘它——
    /// F85 自绘标题栏已否决,窗口装饰交给 OS 原生标题栏,没有自绘的窗口底可暴露。
    /// 零引用;若 F85 未来带 `WM_NCHITTEST` 方案重提,会用在这里。
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
    /// pane 标题条聚焦态的主机名颜色(F83,`ui/pane_title.rs::show`)。
    pub fg_strong: Rgb,
    /// 预留给 F50 SFTP 侧栏头文字 / F84 设置弹窗快捷键动作名
    /// (设计文档 §4.3/§4.4)。零引用。
    pub fg_mid: Rgb,
    pub fg_muted: Rgb,
    /// pane 标题条断开状态的圆点颜色(F83,`ui/pane_title.rs::show`)。
    pub fg_dim: Rgb,
    /// 预留给 F84 设置弹窗的快捷键位徽标(设计文档 §4.4)。零引用。
    pub fg_dimmer: Rgb,
    pub fg_faint: Rgb,
    /// 预留给 F50 SFTP 侧栏列表表头(设计文档 §4.3)。零引用。
    pub fg_ghost: Rgb,

    // --- 语义色(§2.3) ---
    pub accent: Rgb,
    pub accent_fg: Rgb,
    pub ok: Rgb,
    /// 预留给「高负载」状态指示:F83 pane 标题条冻结规格(设计文档 §4.2)原定
    /// 「已连接/已断开/高负载」三态圆点,本切片只落地了前两态;F81 状态栏
    /// (§3.4)同样只留 ok/fg_faint 两态。高负载判定条件待定。零引用。
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
    // 0.35 无 spec 对应值,是 egui 选中态观感的调参,不是色板 token。
    v.selection.bg_fill = c32(t.accent).gamma_multiply(0.35);
    v.selection.stroke = egui::Stroke::new(1.0, c32(t.fg));

    // 不用 override_text_color:那会把所有文字压成一个色,连带盖掉分级灰阶。
    // 逐状态设 fg_stroke,让常态/悬停/按下有层次。
    // 7px 是全局兜底圆角;设计文档 §2.5 按场景分了 pill 6 / 按钮 7 / 控件组 8 /
    // 模态 12,调用方需要不同值时用 `Frame::rounding` 覆盖,不改这里。
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
        assert!(
            (c.r - 0.00699).abs() < 1e-4,
            "sRGB 0x14 的线性值应约 0.00699,实为 {}",
            c.r
        );
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

    /// apply_egui 真的把 token 写进了 egui Visuals,不是摆设:挑 panel_fill(结构色)
    /// 和 widgets.active.bg_fill(语义色)两个代表性字段验证写入生效。
    #[test]
    fn apply_egui_writes_theme_tokens_into_visuals() {
        let ctx = egui::Context::default();
        apply_egui(&ctx, &MULLION_DARK);
        let v = ctx.style().visuals.clone();
        assert_eq!(
            v.panel_fill,
            c32(MULLION_DARK.bar_menu),
            "panel_fill 应取自 theme.bar_menu"
        );
        assert_eq!(
            v.widgets.active.bg_fill,
            c32(MULLION_DARK.accent),
            "按下态背景应取自 theme.accent"
        );
    }
}
