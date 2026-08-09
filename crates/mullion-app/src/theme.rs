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
    /// 独立工具栏底色(设计文档 §4.1 的 48px 栏)。F82 的布局按钮改画在菜单栏
    /// 同一行、按钮组底用 `sunken_bg`,那一栏没了,故当前零引用;保留 token 备
    /// 将来真需要一条工具栏(如 F50 SFTP 的操作栏)时用。
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
    /// 会话管理器里的副文本(F80/F90:列表 user@host、跳板提示、「已设置」、
    /// 空态占位)。
    /// 选它而不是 `fg_faint`,是因为 `fg_faint` on `panel_bg` 只有 2.69:1,
    /// 达不到 WCAG AA 的 4.5:1;本色是 5.71:1。
    /// 也仍预留给 F84 设置弹窗的快捷键位徽标(设计文档 §4.4)。
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
    /// 错误**卡片/横幅**底纹用的柔和红(F90 会话管理器 §5.2)。`danger` 是
    /// Windows 系统红 #e81123,用作大面积底色太刺眼;两者不互相替代。
    pub danger_soft: Rgb,

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
    danger_soft: Rgb::new(0xe0, 0x67, 0x67),

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

/// F62 会话语义色的 8 个预设:`(显示名, hex, 建议用途)`。
///
/// **按颜色命名而不是按环境命名**(不叫「生产色」「测试色」):环境语义是 F64
/// 的地盘,两处都定义「什么是生产」必然会漂移。第三列只作为 tooltip 出现,
/// 是建议,不产生任何语义。
///
/// **不放进 `Theme`**:`Theme` 的字段是「UI 自己用的语义色」,F84 主题切换时
/// 整套换;而这 8 个是用户挑选的标识色,存进 `ColorSpec.hex` 后就与主题脱钩了。
/// 换个主题不该让用户标的红变成另一种红。
///
/// 红/黄/绿/蓝/灰刻意复用 `danger_soft`/`warn`/`ok`/`info`/`fg_dimmer` 的同一组
/// 色值(同一套调色逻辑,不引入第二种审美);**紫故意不取 `accent` 的 #8b95ff**,
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

/// `#rrggbb` → `Rgb`。**解析失败返回 `None`,不报错**——`ColorSpec.hex` 是自由
/// 文本(用户可自定义 hex,配置文件也可能被手改),一个坏值不该让整张会话列表
/// 画不出来。调用方一律把 `None` 当作「没设色」。
///
/// 只认 6 位十六进制、必须带 `#`、大小写不敏感。
pub fn parse_hex(s: &str) -> Option<Rgb> {
    let h = s.strip_prefix('#')?;
    // ASCII 判定必须挡在切片之前:`h.len()` 是**字节**数,"中文" 是 6 字节但只有
    // 2 个 char,直接 `&h[0..2]` 会在字符边界内切开而 panic。
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
/// (同 `clear_color` 那个坑,见 `clear_color_is_linear_not_raw_srgb`)。
fn relative_luminance(c: Rgb) -> f64 {
    0.2126 * srgb_to_linear(c.r) + 0.7152 * srgb_to_linear(c.g) + 0.0722 * srgb_to_linear(c.b)
}

/// WCAG 对比度,1.0(同色)~ 21.0(纯黑对纯白)。
///
/// 用来在测试里**实算**预设色的可见性,而不是靠眼睛和感觉调色。
/// 文本要 4.5:1(WCAG 1.4.3),非文本图形要 3:1(WCAG 1.4.11)——
/// 3px 竖条属后者。
pub fn contrast_ratio(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

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

/// `Rgb` → egui 颜色。
pub fn c32(c: Rgb) -> egui::Color32 {
    egui::Color32::from_rgb(c.r, c.g, c.b)
}

/// 主题描边(白 + 低 alpha)。
pub fn stroke(t: &Theme) -> egui::Stroke {
    egui::Stroke::new(1.0, egui::Color32::from_white_alpha(t.stroke_alpha))
}

/// 把主题写进 egui 的 `Visuals`(外加一处 `Spacing::scroll`,见下)。启动时对
/// egui ctx 调一次。
///
/// **不碰 `Spacing` 里跟尺寸/间距有关的任何一项**——栏高由各 panel 自己的
/// `Frame` 内边距决定(见 `ui::chrome`),混在一起改会让两边互相打架。
/// 唯一的例外是 `Spacing::scroll`:它不参与任何布局尺寸的推导,且 egui 的默认
/// 值有个必须纠正的行为,见 `scroll_style` 的注释。
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
    ctx.style_mut(|s| s.spacing.scroll = scroll_style());
}

/// 滚动条样式。**唯一改动是让静止状态的滑块可见**。
///
/// egui 的默认是 `ScrollStyle::floating()`,其中 `dormant_handle_opacity = 0.0`
/// ——指针不在滚动区里时,滑块的 alpha 是 **0**,整条滚动条彻底不存在
/// (见 egui-0.30.0 `containers/scroll_area.rs` 里 `handle_opacity` 的
/// `lerp(dormant..=active, is_hovering_outer_rect_t)`)。后果:会话列表底下还压着
/// 几条会话时,屏幕上**没有任何提示**——用户以为列表就这么长。离屏截图 harness
/// (`examples/ui_shot.rs`)的第一批产出就是这么发现的:12 条会话只看得见 9 条,
/// 而滚动本身是好的。
///
/// 为什么不换成 `ScrollStyle::solid()`:solid 的 `foreground_color = false`,滑块色取
/// `widgets.inactive.bg_fill` = `sunken_bg`,而轨道底色取 `extreme_bg_color`
/// ——本主题里这两个 token **同值**(都是 `sunken_bg`),滑块会跟轨道糊成一片,
/// 换了个方式继续隐形。而且 solid 恒占 10px 宽,列表行跟着变窄,已冻结的尺寸
/// 规格(F80/F81)和三档密度阈值都得跟着重算。
///
/// 所以保留 floating(不占宽度、悬停自动变粗),只把静止态的透明度从 0 抬起来。
/// 0.45 是「看得出有一条,但不抢视线」——具体数值只有人眼能定,harness 出的图
/// 只能证明它非零可见。
fn scroll_style() -> egui::style::ScrollStyle {
    egui::style::ScrollStyle {
        dormant_handle_opacity: 0.45,
        ..egui::style::ScrollStyle::floating()
    }
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
            let c =
                parse_hex(hex).unwrap_or_else(|| panic!("预设色「{name}」的 hex {hex} 解析不了"));
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
    ///
    /// 用 `append_to` 把它摊成 `LayoutJob` 再看 section 的颜色:egui 内部
    /// 就是这么做的 —— `RichText` 没带颜色时,`into_text_and_format` 会填
    /// `Color32::PLACEHOLDER`,而 `PLACEHOLDER` 正是「让 `Painter::galley`
    /// 用 fallback 色」的哨兵值。断言它不是 PLACEHOLDER,才真的守住了行为。
    #[test]
    fn hint_text_carries_an_explicit_color_so_the_fallback_never_applies() {
        let mut job = egui::text::LayoutJob::default();
        hint_text(&MULLION_DARK, "留空 = 继承(远端默认)").append_to(
            &mut job,
            &egui::Style::default(),
            egui::FontSelection::Default,
            egui::Align::Center,
        );
        let color = job.sections[0].format.color;
        assert_ne!(
            color,
            egui::Color32::PLACEHOLDER,
            "RichText 没带颜色 = 走 egui fallback = 对比度原样偏低"
        );
        assert_eq!(color, c32(MULLION_DARK.fg_dimmer));
    }
}
