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
    /// F203:模态弹窗(全部 `egui::Window`)的底色。
    ///
    /// **F80 冻结色表里没有这一项**,和 `divider`/`sel_alpha` 一样是新增项。
    /// 原先弹窗蹭的是 `bar_status`(#181b26),与 `term_bg`(#14161f)只差
    /// 7/255 —— 删除确认框弹在终端上,用户实报「淹没在 pane 的黑色背景里,
    /// 不容易找到」。
    ///
    /// **不复用 `bar_status` 并把它调亮**:状态栏与会话管理器内框也用它,
    /// 那两处贴在 `panel_bg` 上、本来就分得清,跟着变灰纯属误伤。
    /// 守护:`dialogs_get_their_own_fill_so_brightening_them_does_not_drag_the_status_bar_along`。
    pub modal_bg: Rgb,
    pub panel_bg: Rgb,
    /// pane 标题条(F83,随分屏切片)。
    pub panel_head: Rgb,
    /// 凹槽:分段控件底、快捷键徽标、滑轨。
    pub sunken_bg: Rgb,
    /// 描边不透明度。描边色恒为白,只调 alpha(§2.1 的 rgba(255,255,255,0.06))。
    pub stroke_alpha: u8,
    /// ③ F80:分屏之间那 1 物理像素分界线的颜色。
    ///
    /// **F80 冻结色表里没有这一项**,因为当时根本没画分界线 —— 这是新增项,
    /// 不是改既有值。为什么不复用 `stroke`(白 6%):`stroke` 是画在
    /// `panel_bg` 上的边框,分界线画在 `term_bg` 上、只有 1 物理像素宽,
    /// 6% 的白在那个条件下看不见。守护:
    /// `the_divider_is_visible_but_not_loud_against_the_terminal_background`。
    pub divider: Rgb,
    /// ④ F150:多选高亮填充的 alpha(画在 `accent` 上)。
    ///
    /// **F80 冻结色表里没有这一项**,和 `divider` 一样是新增项、不是改既有值。
    /// 原来选中行画的是 `sunken_bg`(#0e1018),比 `panel_bg`(#14161f)还暗
    /// 6 个亮度单位,笔记本屏上人眼分辨不出来 —— 用户因此以为文件面板没有
    /// 多选功能。不新造色相,只给既有的 `accent` 配一个 alpha。
    /// 守护:`a_selected_row_is_painted_with_the_accent_fill_not_the_sunken_bg`。
    pub sel_alpha: u8,

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
    /// F213:危险语义的**文字**色。
    ///
    /// `danger`(#e81123)是 Windows 系统红,亮度很低:在 `modal_bg`(#3f3f3f)
    /// 上只有 **2.27:1**,在 `bar_status`(#181b26)上 3.71:1 —— 两处都读不到
    /// 4.5:1。而它承载的恰恰是全 app 后果最重的几句话(「删除 3 个目录(连同
    /// 其中全部内容)」「有未保存的修改,关掉就没了」),看不清的代价是误操作。
    ///
    /// 不去动 `danger` 本身:它还要当**填充/描边**用(非文字判据 3:1,#e81123
    /// 在深色面板上够),而且要把一个纯红提到 4.5:1 只能提到接近粉,填充语义
    /// 就散了。分成两个 token,各自守各自的门槛。
    pub danger_text: Rgb,

    // --- F127 文件类型图标色(§2.3 的延伸) ---
    /// 目录:蓝。与 `accent`(偏紫)刻意拉开,不然选中行的强调色和图标糊在一起。
    pub icon_dir: Rgb,
    /// 归档:橙。
    pub icon_archive: Rgb,
    /// 图片:绿。
    pub icon_image: Rgb,
    /// 代码:紫。
    pub icon_code: Rgb,
    /// 文档:灰蓝(最"安静"的一类,因为它最多)。
    pub icon_doc: Rgb,
    /// 可执行:黄。
    pub icon_exec: Rgb,
    /// 符号链接:青。
    pub icon_link: Rgb,
    /// 其他:中性灰。
    pub icon_other: Rgb,
    /// F133:PDF。红,对齐 Windows 上的既有心智。
    pub icon_pdf: Rgb,
    /// F133:Word 文档(doc/docx)。蓝。
    pub icon_word: Rgb,
    /// F133:表格(xls/xlsx/csv)。绿。
    pub icon_excel: Rgb,
    /// F133:演示文稿(ppt/pptx)。橙。
    pub icon_slides: Rgb,
    /// F134:普通文件的默认图标色。中性,比 `icon_other`(设备/socket)亮 ——
    /// 「不认识的普通文件」是绝大多数,不该跟极少数特殊类型抢注意力。
    pub icon_file: Rgb,

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
    // F203:用户指定的 rgb(63,63,63)。中性灰,与色板的冷调蓝紫不同源 ——
    // 这一点是明知的取舍:实报的问题是「找不到」,而醒目度优先。
    modal_bg: Rgb::new(0x3f, 0x3f, 0x3f),
    panel_bg: Rgb::new(0x14, 0x16, 0x1f),
    panel_head: Rgb::new(0x19, 0x1c, 0x27),
    sunken_bg: Rgb::new(0x0e, 0x10, 0x18),
    stroke_alpha: 15, // 0.06 × 255 ≈ 15
    divider: Rgb::new(0x28, 0x2b, 0x38),
    sel_alpha: 51, // 0.2 × 255 ≈ 51

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
    // modal_bg 上 4.84:1、panel_bg 上 8.28:1。同色相(0°),只是把明度抬到
    // 文字判据之上 —— 见 `Theme::danger_text`。
    danger_text: Rgb::new(0xff, 0x90, 0x90),

    icon_dir: Rgb::new(0x6f, 0xa8, 0xff),
    icon_archive: Rgb::new(0xe0, 0x9a, 0x4a),
    icon_image: Rgb::new(0x5f, 0xc2, 0x8a),
    icon_code: Rgb::new(0xb0, 0x8c, 0xff),
    icon_doc: Rgb::new(0x9a, 0xa8, 0xc4),
    icon_exec: Rgb::new(0xd8, 0xc0, 0x52),
    icon_link: Rgb::new(0x58, 0xc0, 0xc8),
    // 比 `fg_dimmer`(0x8a90a8)亮一档:两者同值的话,「可操作的 other」
    // 和「不可操作的 other」在屏上一模一样,`color_for` 的灰化就失效了。
    icon_other: Rgb::new(0xa8, 0xae, 0xc4),
    icon_pdf: Rgb::new(0xe2, 0x6d, 0x6d),
    // 原定 #5b9cf0 与既有 icon_dir(#6fa8ff)RGB 欧氏距离仅 ≈28,同屏易混
    // (目录和 docx 是常见组合);往更深更饱和的蓝调开,拉到 ≈91。
    icon_word: Rgb::new(0x2f, 0x6f, 0xe0),
    // 原定 #4fb372 与既有 icon_image(#5fc28a)距离仅 ≈33;调深调纯,拉到 ≈105。
    icon_excel: Rgb::new(0x1f, 0x8a, 0x4c),
    // 原定 #e88b3d 与既有 icon_archive(#e09a4a)距离仅 ≈21(zip 和 pptx 常见
    // 同屏);往更红更饱和的橙调开,拉到 ≈68。
    icon_slides: Rgb::new(0xf2, 0x69, 0x1f),
    icon_file: Rgb::new(0xc2, 0xc8, 0xd8),

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

/// 铺在 `bg` 上的文字该用深色还是浅色。
///
/// **实算对比度取胜者**,不按亮度阈值拍脑袋:阈值法在中间调(#808080 一带)
/// 两边都勉强,而 F122 的标签底色是用户自选的任意 hex,没有色板纪律兜底。
///
/// 两个候选取纯黑/纯白:任何底色上,这两个里至少有一个 ≥ 4.5:1。
pub fn readable_fg(bg: Rgb) -> Rgb {
    let dark = Rgb::new(0, 0, 0);
    let light = Rgb::new(0xff, 0xff, 0xff);
    if contrast_ratio(dark, bg) >= contrast_ratio(light, bg) {
        dark
    } else {
        light
    }
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
///
/// `const`:`host_key.rs` 的 `DANGER` 要在常量位置由色板推出来,不能另写字面量
/// (F213 —— 原来那个写死的 #c82828 在弹窗底色上只有 1.9:1)。
pub const fn c32(c: Rgb) -> egui::Color32 {
    egui::Color32::from_rgb(c.r, c.g, c.b)
}

/// F206:焦点描边的宽度(逻辑点)。1 点 —— 再粗就从「提示」变成「装饰」。
pub const FOCUS_RING_W: f32 = 1.0;

/// F206:焦点描边的圆角。**直角**。
///
/// 用户实报:pane 的焦点线是直角细线,文件面板却是 2 点圆角框,同一个语义
/// (「键盘现在归我」)在屏幕上有两种长相,眼睛得学两遍。
pub const FOCUS_RING_ROUNDING: egui::Rounding = egui::Rounding::ZERO;

/// F206:焦点描边。**三处(pane 边框 / 文件面板 / 内置编辑器窗口)共用这一个
/// 出口**,`FOCUS_RING_ROUNDING` 一起用。
///
/// 各处自己 `Stroke::new(w, c32(t.accent))` 的写法必然漂移 —— 走查前它已经
/// 漂成了三套(1.0 直角 / 2.0 圆角 4 / egui 默认的白 6% 圆角 6)。机械守护见
/// `tests::focus_rings_all_come_from_one_place`。
pub fn focus_ring(t: &Theme) -> egui::Stroke {
    egui::Stroke::new(FOCUS_RING_W, c32(t.accent))
}

/// 主题描边(白 + 低 alpha)。
pub fn stroke(t: &Theme) -> egui::Stroke {
    egui::Stroke::new(1.0, egui::Color32::from_white_alpha(t.stroke_alpha))
}

/// F150:多选高亮的行底色(`accent` + 低 alpha)。
///
/// 用 `from_rgba_unmultiplied`:`sel_alpha` 是「相对于底下那层的不透明度」,
/// 不是预乘值。写成 `Color32::from_rgba_premultiplied` 会得到一个几乎全黑的
/// 颜色 —— 编译过、画得出、就是看不见,和它要修的那个 bug 一模一样。
pub fn selection_fill(t: &Theme) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(t.accent.r, t.accent.g, t.accent.b, t.sel_alpha)
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
    // F203:弹窗有自己的底色,不再蹭 `bar_status` —— 见 `Theme::modal_bg`。
    v.window_fill = c32(t.modal_bg);
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
    ctx.style_mut(|s| {
        s.spacing.scroll = scroll_style();
        s.interaction.resize_grab_radius_corner = RESIZE_CORNER_GRAB;
    });
}

/// F217:窗口四角的抓取半径。**这个值是按 egui 的命中判定倒推出来的,
/// 不是审美取舍**。
///
/// 症状:在窗口正角上按住拖,只改得动宽或高中的一个,拖不出对角。
///
/// 根因在 `hit_test.rs:416` 的 `should_prioritizie_hits_on_back` —— 距离打平时
/// egui 会「优先选细的那个命中目标」(为了让细长的 resize 边带好抓),判据是
/// `back <= 0.5 * front && back <= 16`,量的是各自的短边。而边带的短边是
/// `2 × resize_grab_radius_side` = **10**,角的短边是
/// `2 × resize_grab_radius_corner` = **20** —— `10 <= 0.5 × 20` 正好取等,
/// 于是正角上永远是边带赢,角本身一次也轮不到。egui 在那儿自己也写着
/// 「hard-coded heuristics that could surely be improved」。
///
/// 把角的半径压到 9(短边 18)就打破了这个取等:`10 <= 9` 为假,角赢。
/// 抓取区从 20×20 缩到 18×18,仍比画出来的角标(`resize_corner_size` = 12)大。
///
/// **不去动 `resize_grab_radius_side`**:那个值 `TopBottomPanel`/`SidePanel`
/// 的分隔条也在读(`panel.rs:309`/`801`),动它会连带改掉文件侧栏分界线的
/// 手感,而那跟本次问题无关。
///
/// 守护:`ui::editor_window::tests::dragging_a_corner_changes_both_width_and_height`
/// (egui 哪天改了那套启发式,这条会红)。
const RESIZE_CORNER_GRAB: f32 = 9.0;

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

    /// 「bold 提亮」必须**真的**提高可读性:每个亮色在终端底色上都得比它的
    /// 基色更亮。
    ///
    /// 判据落在 app 层,因为**只有这一层同时知道两样东西** —— ANSI 表归
    /// `mullion-term`,终端底色 `term_bg` 归主题。term 层测得了「bold 走没走
    /// 提亮那条路」,测不了「提完之后到底看不看得清」。
    ///
    /// 这条对任何一套调色板都成立,不是照着 Campbell 的数字反填的阈值:
    /// 一套 bright 比 base 还暗的调色板,`bold_brighten` 就成了帮倒忙。
    ///
    /// 自证会变红:把 ANSI 表里 bright blue 和 blue 的值对调。
    #[test]
    fn every_bright_color_outreads_its_base_on_the_terminal_background() {
        use mullion_term::palette::indexed_default;
        const NAMES: [&str; 8] = [
            "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
        ];
        for (i, name) in NAMES.iter().enumerate() {
            let base = contrast_ratio(indexed_default(i as u8), MULLION_DARK.term_bg);
            let bright = contrast_ratio(indexed_default(i as u8 + 8), MULLION_DARK.term_bg);
            assert!(
                bright > base,
                "bright {name} 在终端底色上比 {name} 还暗({bright:.2} ≤ {base:.2}),\
                 bold 提亮成了帮倒忙"
            );
        }
    }

    /// 用户实机报的那个症状的**数值判据**:`ls` 的目录名(`01;34` → bright
    /// blue)必须过 WCAG 1.4.3 的正文线 4.5:1。
    ///
    /// 目录名是终端里最常读的一类文字。原来那套 xterm 默认值在这里是
    /// 1.9:1(蓝 `#0000EE` 直接落上去,还没有提亮这一步)—— 图上就是一团
    /// 读不出来的墨块。
    ///
    /// 自证会变红:把 ANSI 表里 bright blue 换回 `#5C5CFF`(3.80,读得出来但
    /// 不过线)或 `#0000EE`。
    #[test]
    fn a_directory_name_clears_the_wcag_body_text_line() {
        use mullion_term::palette::indexed_default;
        let ratio = contrast_ratio(indexed_default(12), MULLION_DARK.term_bg);
        assert!(
            ratio >= 4.5,
            "bright blue 对终端底色只有 {ratio:.2}:1,ls 的目录名在深色屏上读不清"
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

    /// F122:整块上色后,标题文字的对比度不能再靠色板纪律 —— 用户配的是任意
    /// hex。`readable_fg` 必须在任何底色上都给出 ≥ 4.5:1(WCAG 1.4.3 文本阈值)。
    ///
    /// 自证会变红:把 `readable_fg` 改成恒返回浅色。
    #[test]
    fn readable_fg_clears_the_text_threshold_on_any_background() {
        // 8 个预设直接取 `LABEL_PALETTE`,不手抄一份 —— 抄的那份改了色板不会跟着变。
        let presets = LABEL_PALETTE.iter().map(|(_, hex, _)| *hex);
        for hex in presets.chain(["#000000", "#ffffff", "#808080"]) {
            let bg = parse_hex(hex).unwrap();
            let fg = readable_fg(bg);
            assert!(
                contrast_ratio(fg, bg) >= 4.5,
                "{hex} 上的前景色只有 {:.2}:1",
                contrast_ratio(fg, bg)
            );
        }
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

    /// F127:8 类图标色在面板底色上必须达到 WCAG 1.4.11 的非文本对比度 3:1,
    /// 否则「颜色区分类型」这件事对暗色主题下的细线条图标不成立。
    /// 阈值与写法同 F62 的会话语义色。
    #[test]
    fn file_icon_colors_are_visible_on_the_panel() {
        let t = MULLION_DARK;
        for (name, c) in [
            ("dir", t.icon_dir),
            ("archive", t.icon_archive),
            ("image", t.icon_image),
            ("code", t.icon_code),
            ("doc", t.icon_doc),
            ("exec", t.icon_exec),
            ("link", t.icon_link),
            ("other", t.icon_other),
            ("pdf", t.icon_pdf),
            ("word", t.icon_word),
            ("excel", t.icon_excel),
            ("slides", t.icon_slides),
            ("file", t.icon_file),
        ] {
            let ratio = contrast_ratio(c, t.panel_bg);
            assert!(ratio >= 3.0, "{name} 在面板底上只有 {ratio:.2}:1");
        }
    }

    /// F127:13 类颜色必须两两不同 —— 两类同色等于少一类。
    ///
    /// **这条只保证「不完全相等」,保证不了「肉眼可区分」**:已知既有色里
    /// `icon_doc`(#9aa8c4)与 `icon_other`(#a8aec4)的 RGB 欧氏距离仅
    /// ≈15.2,同屏出现(普通文件混在文档列表里)时其实很难分辨,是已知技术债。
    /// 没在这里加距离阈值断言 —— 阈值只能设成这个既有最小值,加了不产生新约束;
    /// 设更大的值会反过来要求改既有色,而 UI 视觉规格(色板全表)是冻结的,
    /// 既有色不许动。F133/F134 新加的三色(word/excel/slides)已人工核实
    /// 各自与最近既有色的距离 ≥ 40(分别 ≈91/≈105/≈68),不会加重这个债。
    #[test]
    fn file_icon_colors_are_all_distinct() {
        let t = MULLION_DARK;
        let all = [
            t.icon_dir,
            t.icon_archive,
            t.icon_image,
            t.icon_code,
            t.icon_doc,
            t.icon_exec,
            t.icon_link,
            t.icon_other,
            t.icon_pdf,
            t.icon_word,
            t.icon_excel,
            t.icon_slides,
            t.icon_file,
        ];
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                assert_ne!(all[i], all[j], "第 {i} 类和第 {j} 类同色");
            }
        }
    }

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

    /// F203:弹窗底色走**自己的 token**,不再蹭 `bar_status`。
    ///
    /// 用户实报:删除/权限确认框弹在近黑的终端上「淹没在 pane 的黑色背景里,
    /// 不容易找到」。根因是所有 `egui::Window` 共用
    /// `window_fill = bar_status`(#181b26),与 `term_bg`(#14161f)只差
    /// 几个亮度单位。
    ///
    /// **不能直接把 `bar_status` 调亮**:状态栏(`ui::chrome`)和会话管理器
    /// 的内框也用它,改了那两处会跟着一起变灰 —— 而它们本来就贴在
    /// `panel_bg` 上,不存在「找不到」的问题。所以必须是一个新 token。
    ///
    /// 自证会变红:把 `v.window_fill` 改回 `c32(t.bar_status)`。
    #[test]
    fn dialogs_get_their_own_fill_so_brightening_them_does_not_drag_the_status_bar_along() {
        let t = MULLION_DARK;
        assert_ne!(
            t.modal_bg, t.bar_status,
            "modal_bg 与 bar_status 同值 —— 那这个 token 什么也没分开"
        );
        let ctx = egui::Context::default();
        apply_egui(&ctx, &t);
        assert_eq!(
            ctx.style().visuals.window_fill,
            c32(t.modal_bg),
            "弹窗底色没接到 modal_bg 上"
        );
    }

    /// F203:弹窗底色必须比终端底色亮得**足以一眼分辨**。
    ///
    /// 判据用亮度差而不是「等于某个具体色值」:后者只是把常量抄两遍,
    /// 改坏了两边一起错。下界 32 是实报那条 bug 的量化 —— 原先
    /// `bar_status`(#181b26)对 `term_bg`(#14161f)最大只抬 7/255,
    /// 用户看不出来。
    ///
    /// 自证会变红:把 `modal_bg` 改回 `#181b26`。
    #[test]
    fn the_dialog_fill_stands_off_the_terminal_background_by_more_than_the_old_one_did() {
        let t = MULLION_DARK;
        let lift = i32::from(t.modal_bg.r) - i32::from(t.term_bg.r);
        assert!(
            lift >= 32,
            "modal_bg 只比 term_bg 抬了 {lift}/255 —— 用户实报「淹没在黑背景里」\
             时的差值是 7,抬得不够等于没修"
        );
        // 反面:亮到接近前景灰阶就没法在上面写字了。
        assert!(
            t.modal_bg.r < t.fg_faint.r,
            "modal_bg 比 fg_faint 还亮 —— 弹窗里的次要文字将彻底不可读"
        );
    }

    /// F203 的连带账:底色抬亮,**贴在它上面的次要文字对比度是往下走的**。
    ///
    /// 原先弹窗里的说明文字用 `fg_dim`(#9aa0b8),在 `bar_status` 上约
    /// 5.5:1 过 AA;搬到 `modal_bg`(#3f3f3f)上掉到约 4.1:1,不过了。
    /// 提一档到 `fg_muted`(#a9aec2)才回到 5.0:1。
    ///
    /// 两条断言缺一不可:只断言 `fg_muted` 达标的话,把调用点改回 `fg_dim`
    /// 这条测试照样绿(它压根没看调用点用的是哪个)。第二条断言钉的是
    /// 「这次换档确有必要」—— 哪天有人把 `modal_bg` 调暗回去,它会红,
    /// 提醒把文字档一起调回来。
    ///
    /// 自证会变红:把 `modal_bg` 改回 `#181b26`(第二条红)。
    #[test]
    fn the_secondary_text_token_used_in_dialogs_still_clears_aa_on_the_new_fill() {
        let t = MULLION_DARK;
        let muted = contrast_ratio(t.fg_muted, t.modal_bg);
        assert!(
            muted >= 4.5,
            "fg_muted 在 modal_bg 上只有 {muted:.2}:1,不到 WCAG AA 的 4.5:1"
        );
        let dim = contrast_ratio(t.fg_dim, t.modal_bg);
        assert!(
            dim < 4.5,
            "fg_dim 在 modal_bg 上是 {dim:.2}:1、已经达标 —— 那么弹窗里换档这件事\
             失去了理由,`tests/dialog_contrast.rs` 那条闸门该一并撤掉"
        );
    }

    /// F213:危险语义的**文字**在弹窗底色上也要过 4.5:1。
    ///
    /// `danger`(Windows 系统红 #e81123)只有 2.27:1 —— 而它承载的是「删除 3 个
    /// 目录(连同其中全部内容)」这种后果最重的句子。两件事一起钉:新 token
    /// 达标,且**老 token 确实不达标**(否则加它没有理由,该退回去)。
    ///
    /// 自证会变红:把 `danger_text` 改回 `Rgb::new(0xe8, 0x11, 0x23)`。
    #[test]
    fn the_danger_text_token_is_readable_on_the_dialog_fill_but_the_fill_token_is_not() {
        let t = MULLION_DARK;
        for (name, bg) in [
            ("modal_bg", t.modal_bg),
            ("panel_bg", t.panel_bg),
            ("bar_status", t.bar_status),
        ] {
            let r = contrast_ratio(t.danger_text, bg);
            assert!(r >= 4.5, "danger_text 在 {name} 上只有 {r:.2}:1");
        }
        let old = contrast_ratio(t.danger, t.modal_bg);
        assert!(
            old < 4.5,
            "danger 在 modal_bg 上是 {old:.2}:1、已经达标 —— 那 `danger_text` 没有\
             存在理由,该合并回去"
        );
        // 填充/描边判据是 3:1(WCAG 1.4.11)。`danger` 留着干这个,别顺手删掉。
        assert!(
            contrast_ratio(t.danger, t.panel_bg) >= 3.0,
            "danger 连非文字的 3:1 都不到了,那它当填充也不成立"
        );
    }

    /// F213:toast 的底色必须与弹窗同源,不能蹭 `sunken_bg`。
    ///
    /// 它是飘在终端正文之上的浮层。原来用 `sunken_bg`(#0e1018),与终端底色
    /// (#14161f)只有 **1.05:1** —— 边界几乎看不出来,整块提示像是印在正文里的。
    /// F203 把弹窗从这个坑里捞出来时漏掉了它。
    ///
    /// 自证会变红:把 `toast.rs` 里的 `t.modal_bg` 改回 `t.sunken_bg`。
    #[test]
    fn the_toast_floats_on_the_dialog_fill_so_it_stands_off_the_terminal_behind_it() {
        let t = MULLION_DARK;
        let src = include_str!("ui/toast.rs");
        let prod = src.split("#[cfg(test)]").next().expect("源码切歪了");
        assert!(
            prod.contains(".fill(theme::c32(t.modal_bg))"),
            "toast 的底色不是 modal_bg 了"
        );
        let stands_off = contrast_ratio(t.modal_bg, t.term_bg);
        let old = contrast_ratio(t.sunken_bg, t.term_bg);
        assert!(
            stands_off > old * 1.5,
            "换过之后 toast 与终端底的对比只有 {stands_off:.2}:1(原来 {old:.2}:1),\
             等于白换"
        );
        // 三档边框都是**非文字**元素,判据 3:1。
        for (name, c) in [("info", t.info), ("ok", t.ok), ("warn", t.warn)] {
            let r = contrast_ratio(c, t.modal_bg);
            assert!(r >= 3.0, "toast 的 {name} 边框在 modal_bg 上只有 {r:.2}:1");
        }
    }
}
