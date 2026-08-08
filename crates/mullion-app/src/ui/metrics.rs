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

/// `TextEdit` 默认内边距的 x 方向合计。
///
/// egui 0.30 的 `TextEdit` 默认 `margin: Margin::symmetric(4.0, 2.0)`
/// (`widgets/text_edit/builder.rs:129`),而 `desired_width` 只圈内容区 ——
/// 实际画出来的外框会再宽 `margin.sum().x = 8.0`。给同一行后续控件算
/// `reserve` 时必须把这 8px 补上,否则后面的按钮/输入框会被顶出去 8px:
/// 肉眼几乎看不出,但确实被裁(走查 P0-1 的余数)。
///
/// 硬编码是当前能做到的最好:该字段私有、只有 setter 没有 getter,
/// `Style` 里也没有对应项,运行时读不到。
/// **不要**用 `SP_S` 顶替 —— 它俩数值恰好都是 8.0 但语义无关,
/// 调间距刻度时会静默带崩这里。
pub const TEXT_EDIT_MARGIN_X: f32 = 8.0;

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
    // 两条断言比较的都是编译期常量,clippy 建议挪进 const block ——
    // 但这里要的正是「跑测试时会被检查」,挪进 const block 会在编译期就
    // 断言,失败时报的是编译错误而不是测试失败,达不到「常量改坏了
    // 有测试兜底」的目的,故显式放行这条 lint。
    #[allow(clippy::assertions_on_constants)]
    fn short_field_is_meaningfully_narrower_than_medium() {
        assert!(FIELD_W_S * 2.0 < FIELD_W_M);
        assert!(FIELD_W_MIN <= FIELD_W_S);
    }

    /// `reserve` 必须跟着真实字体走,不能是手写常量 —— 否则换字号/换
    /// 缩放后 P0-1 原样复发,且没有任何编译错误提示。
    #[test]
    fn button_reserve_tracks_the_actual_label_width() {
        let ctx = egui::Context::default();
        let mut narrow = 0.0f32;
        let mut wide = 0.0f32;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
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
}
