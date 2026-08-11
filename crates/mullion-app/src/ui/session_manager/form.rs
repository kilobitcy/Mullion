//! 表单骨架构件:分节标题、两列 Grid、必填星号、字段下的内联红字。
//!
//! 2026-08-11(F119)从 `fields.rs` 切出来。理由不是「文件太长」,而是
//! 隧道表单(`tunnel_editor.rs`)要用**同一套**骨架:没有共享构件,
//! 「标签列 88px」「分节前一条细线」这类规则就只能靠人看,下个切片照样漂。
//! `docs/ui-form-guidelines.md` 是这套构件的文字版,两者必须同时改。
//!
//! **搬迁时逻辑一字未改**,四个函数连同文档注释原样搬过来。

use egui::Ui;

use crate::theme::Theme;
use crate::ui::annotate;
use crate::ui::metrics::LABEL_COL_W;

/// 两列表单的统一样式:左列标签定宽,右列输入撑满。
pub(super) fn grid(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    egui::Grid::new(id)
        .num_columns(2)
        .spacing([crate::ui::metrics::SP_M, crate::ui::metrics::SP_S])
        .min_col_width(LABEL_COL_W)
        .show(ui, add);
}

/// 分区小标题。11px + fg_muted,上面留一档大间距 + 一条细分隔线 ——
/// 表单一路平铺下来没有任何视觉锚点,眼睛找不到「这几行是一组」
/// (走查 P2-17)。
///
/// 首个分区不画分隔线:页面顶上来一条横线看着像误画的。
///
/// **不要**改用 `ui.min_rect().height() > 0.0` 这类容器状态推断:实测过,
/// `egui::CentralPanel` 一进去 `min_rect` 就等于整个 `max_rect`(非零),
/// 只有生产环境实际包着的 `egui::ScrollArea` 内层 ui 才会从零开始——
/// 判据会不会画线因此取决于「外面拿什么容器包这一页」这种调用方看不见
/// 的细节,坏起来是「所有分区顶上都多一条线」或者「所有分区都没有线」,
/// 取决于测试用什么容器渲染。
///
/// `first` 是**页面级游标**,不是每次调用现填的字面量:`&mut bool`,
/// 用一次就在函数内部自翻成 `false`。**必须由页面级函数**(`basic` /
/// `appearance` / `auth` / `automation`)**在最外层持有并声明
/// `let mut first = true;`**,那一页内所有 `section(...)` 调用都传
/// `&mut first`。这样「这一页第几个分区」永远由调用序列自动决定,不
/// 依赖任何人手工在每个调用点上把 `true`/`false` 敲对——旧版直接传
/// `bool` 字面量时,忘了把新插入分区之前那个改成 `false`,或者忘了把
/// 原来的首个分区改成 `false`,编译器和测试都拦不住(两个"first"或者
/// 顺序错位)。
///
/// `network()` / `jump()` 是被 `basic()` 调用的**子函数**,不是页面级
/// 函数——它们必须原样接住调用方传来的游标继续往下传,**不许**在函数
/// 内部自己 `let mut first = true`:那等于把硬编码换了个地方,子函数被
/// 单独渲染成一页时(如某些测试)看起来「恰好」对,一旦真被插进别的页面
/// 中间就会在不该有线的地方多画一条。
pub(super) fn section(ui: &mut Ui, t: &Theme, title: &str, first: &mut bool) {
    use crate::ui::metrics::{SP_L, SP_XS};
    if !*first {
        ui.add_space(SP_L);
        ui.separator();
    }
    *first = false;
    ui.add_space(SP_XS);
    let head = ui.label(
        egui::RichText::new(title)
            .size(11.0)
            .color(crate::theme::c32(t.fg_muted)),
    );
    // F100:标的是**分节标题这一行**,不是分节整块 —— `section` 只画标题、不
    // 持有内容闭包,拿不到整块的 rect。要标整块得把这个函数改成包住内容的
    // 容器,那是另一回事。指标题够用:人说的是「『代理』这一节太挤」。
    annotate::mark(
        ui.ctx(),
        format!("会话管理器/右栏/分节「{title}」"),
        head.rect,
    );
    ui.add_space(SP_XS);
}

/// 必填项标签:名字后跟一个 danger 色的星号。
pub(super) fn required(ui: &mut Ui, t: &Theme, text: &str) {
    ui.horizontal(|ui| {
        ui.label(text);
        ui.colored_label(crate::theme::c32(t.danger), "*");
    });
}

/// 走查 15:字段下方的一行内联红字。占一整行 grid,左格留空让文字跟输入框
/// 左对齐 —— 挂在标签那一列会被误读成「这一行的标签」。
///
/// 只在 `show` 为真时才占行:恒占位会让表单在没有错误时凭空多出三段空白。
pub(super) fn field_error(ui: &mut Ui, t: &Theme, show: bool, msg: &str) {
    if !show {
        return;
    }
    ui.label("");
    ui.label(
        egui::RichText::new(msg)
            .size(11.0)
            .color(crate::theme::c32(t.danger)),
    );
    ui.end_row();
}
