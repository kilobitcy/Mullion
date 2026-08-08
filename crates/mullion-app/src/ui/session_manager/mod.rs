//! 会话管理弹窗:列表 + CRUD + 编辑表单(Task 6,§4.3/§1.2)。
//!
//! 关键约束:这里渲染在 `app.rs` 的 `egui_ctx.run(|ctx| ...)` 闭包内,只能拿到
//! `&mut UiState`,拿不到 `&mut SessionStore`(否则借用检查器过不了)。所以任何会
//! 改 store / 发起连接的动作,这里只写「意图」到 `UiState`,由 `app.rs` 在
//! `render_frame` 返回、借用释放之后统一施加——与既有 `request_disconnect`/
//! `request_quit` 完全同构。

mod buffer;
mod dedupe;
mod editor;
mod env_hint;
mod fields;
mod highlight;
mod inherit_row;
mod jump_preview;
mod keys;
mod keyscan;
pub(crate) mod list;
mod tab_badge;
mod tags;
pub(crate) mod validate;

pub(crate) use buffer::{
    build_draft, clear_key, connect_string, import_icon_file, import_key_file, is_dirty,
    merge_secret, secret_fields, set_color_target, sync_has_passphrase,
};
pub(crate) use buffer::{AuthKindUi, JumpModeUi, ProxyModeUi};
pub use buffer::{EditorBuffer, SaveIntent, SecretField, SecretPresence, SwitchTarget};

use egui::NumExt as _;
use mullion_store::{GroupId, GroupRecord, SessionId, SessionRecord};

use crate::theme::{self, Theme};

use super::UiState;

/// 设计稿 §3:880×560 单窗,左栏定宽 300。
pub(crate) const WINDOW_W: f32 = 880.0;
pub(crate) const WINDOW_H: f32 = 560.0;
pub(crate) const LIST_W: f32 = 300.0;
/// 左栏拖拽下限。再窄「user@host」副文本就没法读了。
pub(crate) const LIST_MIN_W: f32 = 220.0;
/// 左栏拖拽上限。与 `WINDOW_W` 联立,但**别信纸面公式**——两栏
/// `inner_margin` 各 14、两侧共 28,「880 - 440 - 28 = 412」看着像右栏
/// 内容宽,但 egui 的 `Window` 内容区实际取 `default_size`(880),不是
/// 「`WINDOW_W` 减两侧 `window_margin`」,公式口径本就对不上实现。
/// 唯一可信的是实测:用真实指针事件把分隔条拖到 `LIST_MAX_W`=440,读回
/// `editor_root_id()` 矩形,右栏内容宽约 412px,相对 400px 的最小可用宽
/// 只有 12px 余量——不是纸面算出来的 16px。
/// 改这几个常量任意一个,都必须重跑
/// `dragging_the_split_does_not_widen_the_window`(它的判定表达式比这里更
/// 保守,仍然安全,但别再据这段注释的算术去调整 `LIST_MAX_W`)。
pub(crate) const LIST_MAX_W: f32 = 440.0;
/// 内容区高度。egui 的 `Window` 高度默认跟内容走,不撑到 `default_size` 给的
/// 高度;靠这个值把双栏撑满,否则会话少时窗口会缩成一条。见 §3 的待验证假设。
/// `show()` 里会先跟实际可用高度(减去 `window_chrome_reserve`)取较小值,
/// 算出地板 `content_min_height` 喂给 `ui.set_min_height`——**光有地板不够**:
/// `SidePanel` 会把自己的内容高度报回给外层 `ui`(`ui.expand_to_include_rect` +
/// 只能变大的棘轮 `ui.set_min_height(ui.max_rect().height())`,见
/// `egui-0.30.0/src/containers/panel.rs::SidePanel::show_inside_dyn`),只设地板
/// 会让这个「报告更多→外层被撑大→SidePanel 下一帧报告更多」的环没有上限地跑
/// 下去。天花板怎么钉见 `show()` 内部的注释——**不能用 `ui.set_height`/无条件
/// `ui.set_max_height`**:那等于每帧无条件覆写 `content_ui` 的 `max_rect`,会
/// 连带丢弃 `Resize` 当帧从用户拖拽算出的候选尺寸,直接弄死垂直 resize
/// (F90 复核发现的回归,`dragging_the_resize_handle_grows_window_and_stops_
/// only_at_screen_edge` 实测坐实)。现在的天花板只在
/// `ui.max_rect().height()` 真的超过可用高度那一帧才出手。
pub(crate) const CONTENT_MIN_HEIGHT: f32 = 480.0;

/// 右栏 Tab 的下标。`editor_tab: usize` 是既有技术债(换 enum 会波及
/// 所有 Tab 相关代码,不在本切片范围),但至少让「哪个数字是哪个 Tab」
/// 只有一处真源 —— 否则重排 `editor::TABS` 时编译器不会报错,
/// `validate::tab()` 会静默把用户导向错误的 Tab。
pub(crate) const TAB_CONNECT: usize = 0;
pub(crate) const TAB_AUTH: usize = 1;
pub(crate) const TAB_AUTOMATION: usize = 2;
pub(crate) const TAB_APPEARANCE: usize = 3;

/// 计算给 `Window` 自身 chrome(标题栏 + `Frame::window` 的 `inner_margin`)留的
/// 余量——**不是硬编码常量,是按 egui-0.30.0 实际渲染公式当场算出来的**。
///
/// 复核 F90 时发现:早先这里是一个硬编码 `64.0`。用
/// `cargo test -p mullion-app --lib session_manager::tests -- --nocapture` 实测
/// 追出来,`egui-0.30.0/src/containers/window.rs`(`Window::show_dyn`)里有两处
/// 独立算「标题栏占多高」,口径不一致:
///
/// 公式 A——`resize.max_size.y` 的 clamp(egui 自己「不超过可用区域」的保护)
/// 用的是 `title_bar_height = font_height(title) + window_margin.top +
/// window_margin.bottom`,没有 `.max(interact_size.y)`。
///
/// 公式 B——`TitleBar::new` 实际渲染标题栏用的是
/// `height = font_height(title).max(style.spacing.interact_size.y)`,放大无障碍
/// 字号/缩放(`interact_size.y` 变大)时这个值会比公式 A 大得多,公式 A 的保护
/// 对此不生效。
///
/// 而外层 `Window` 帧到我们自己 content ui 之间实际吃掉的高度,是公式 B 的
/// `title_bar_height` 加上 `window_margin.top+bottom` **算两次**(一次是
/// `Frame::window` 的 `inner_margin` 包住「标题栏+内容」整体,一次是
/// `title_content_spacing` 作为标题栏与内容之间的 item spacing,两处都等于
/// `window_margin.top+bottom`,见 `window.rs::show_dyn` 里 `margins` 变量的计算
/// (`outer_margin.sum() + inner_margin.sum() + vec2(0.0, title_bar_height)`)
/// 以及紧邻的 `frame.content_ui.spacing_mut().item_spacing.y =
/// title_content_spacing`)。
/// 这里按公式 B 现算,用实际 `ctx.style()`/`ctx.fonts()` 里当前的字号/间距,不
/// 猜一个宁可算多的常量——`interact_size.y=100` 时实测算出 124(`24 + 100`),
/// 跟旧硬编码 64 差出近一倍;这正是复核 F90 时发现旧值不够导致按钮底边真实
/// 溢出 14px(694 vs 屏幕高 680)的根因,见下面
/// `new_button_stays_within_screen_rect_when_real_bottom_bar_content_is_taller_than_the_old_hardcoded_estimate`
/// 那条测试的提交历史。
///
/// `SLACK`:公式 A/B 之外,实测(拖到贴近屏幕边缘那一帧,细分成 30 步、每步
/// 10px,见测试 `dragging_the_resize_handle_grows_window_and_stops_only_at_
/// screen_edge`)还是存在几 px 的残余偏差(约 2.7px)——`Resize` 自己
/// (`containers/resize.rs::Resize::begin`)还有一层独立的
/// `content_clip_rect = inner_rect.expand(ui.visuals().clip_rect_margin)`
/// (默认 3.0)之类的裁剪/取整开销,没有直接进公式 A/B,读源码不容易完全追全。
/// 不精确复刻这些内部细节,按项目既有约定「宁可算多、不算少」补一个 8px 安全
/// 余量(实测残余的 3 倍余裕)——这不是放弃现算、退回硬编码:主项(标题栏高度、
/// `window_margin` 双计)仍然完全动态跟字号/缩放走,`SLACK` 只覆盖公式之外量
/// 不到的零头。
const SLACK: f32 = 8.0;

/// 「测试连接」(F92)的四态。存在 `UiState` 里跨帧;真正的拨测在 app.rs
/// 的 tokio 运行时上跑,靠 `App::probe_epoch` 世代号丢弃过期结果。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ProbeState {
    #[default]
    Idle,
    Running,
    Ok,
    Err(String),
}

fn window_chrome_reserve(ctx: &egui::Context) -> f32 {
    let style = ctx.style();
    let title_font_height =
        ctx.fonts(|fonts| egui::RichText::new(WINDOW_TITLE).font_height(fonts, &style));
    let title_bar_height = title_font_height.max(style.spacing.interact_size.y);
    let window_margin = style.spacing.window_margin;
    2.0 * (window_margin.top + window_margin.bottom) + title_bar_height + SLACK
}

/// `egui::Window::new` 的标题文本。抽成常量是因为 `window_chrome_reserve` 要用
/// **同一段文字**现算标题栏真实高度——标题变了这两处必须一起改,不能各写一份。
const WINDOW_TITLE: &str = "会话管理器";

/// 每个分组桶对应的 `CollapsingHeader` 构造。抽成独立函数**只为了能在测试里
/// 直接调它**:`CollapsingHeader::new` 默认把标题文本本身当 id 源(见 egui 0.30
/// `collapsing_header.rs`)。两个分组恰好同名(且当前会话数也一样,标题完全
/// 一致)时会撞 id、共享同一份展开/收起状态——编译不报错,跑起来才会看到
/// "点开 A,B 也跟着变"。`.id_salt(gid)` 用分组主键(`None`=未分组桶)强制
/// 区分,彻底绕开标题文本。守护测试
/// `collapsing_header_id_salt_disambiguates_same_titled_groups` 直接调这个函数
/// (不是重抄一遍表达式),删掉 `.id_salt(gid)` 这行测试就会红。
fn group_header(title: &str, gid: Option<GroupId>, count: usize) -> egui::CollapsingHeader {
    egui::CollapsingHeader::new(format!("{title}({count})"))
        .id_salt(gid)
        .default_open(true)
}

/// 右栏编辑器根 `Ui` 的显式 id。跟 `list::new_button_id()` 同一个理由:
/// egui 的自动 id 由 `next_auto_id_salt` 计数器派生,外部测试代码算不出来,
/// 只能挂一个不依赖父 id 栈的全局 id,测试侧用 `Context::read_response`
/// 读回真实 `Response::rect` 来判定溢出。全程序只出现一次,不会撞 id。
pub(crate) fn editor_root_id() -> egui::Id {
    egui::Id::new("mullion_sm_editor_root")
}

/// 「保存」按钮的显式 id,理由同 `editor_root_id()`:测试要读回它的
/// `Response::enabled()`,自动 id 外部算不出来。
pub(crate) fn save_button_id() -> egui::Id {
    egui::Id::new("mullion_sm_save_button")
}

/// 「测试连接」按钮的显式 id,理由同 `save_button_id()`。
pub(crate) fn probe_button_id() -> egui::Id {
    egui::Id::new("mullion_sm_probe_button")
}

/// 「复制连接串」按钮的显式 id,理由同 `save_button_id()`。
pub(crate) fn copy_button_id() -> egui::Id {
    egui::Id::new("mullion_sm_copy_button")
}

/// 画一个挂着显式 id 的按钮,并在禁用时附 tooltip。
///
/// egui 0.30 的 `Button` 不支持自定义 id,而守护测试必须能
/// `Context::read_response(id)` 读回 `enabled()` —— 只能自己分配空间、
/// 用显式 id `interact`、再手绘。这跟 `list.rs::new_button` 是**同一类问题、
/// 同一套解法**——绘制内核直接照抄它,不要另起一套算法:两者以后要跟
/// `Button::ui()`(egui-0.30.0 `widgets/button.rs`)的视觉规则保持一致,
/// 抄同一份实现能保证改一处两边一起改,不会像本函数复核前那样,字号/高度
/// 地板/底色/圆角/hover 扩张五处悄悄跟原生按钮和 `new_button` 分道扬镳。
///
/// 具体对齐点(均见 `button.rs::ui`,行号按锁定版本 egui-0.30.0):
/// - 字号用 `into_galley(.., TextStyle::Button)` 而非硬编码字号——项目
///   `theme.rs` 没覆写 `text_styles`,`TextStyle::Button` 是 egui 默认的
///   12.5,跟旁边原生 `Button`(内部同样用 `TextStyle::Button`)才能对上。
/// - 高度地板 `.at_least(vec2(0.0, interact_size.y))`(对应 `button.rs:279`
///   `desired_size.y.at_least(interact_size.y)`),否则矮按钮跟原生按钮
///   高度对不上。
/// - 底色用 `visuals.weak_bg_fill`(对应 `button.rs:308`),不是
///   `visuals.bg_fill`——原生按钮实际就是拿这个字段画底色。
/// - 圆角用 `visuals.rounding`(当前样式实际值),不硬编码常量——硬编码值
///   今天凑巧等于 `theme.rs` 里设的值,以后调 theme 这里不会跟着变。
/// - `rect.expand(visuals.expansion)`:hover/active 时原生按钮会微微扩张,
///   照抄才有同样的反馈。
/// - 用 `ui.allocate_space` 占位、`interact` 时才挂显式 id 注册交互,不用
///   `allocate_exact_size`——后者会顺带用自动 id 注册一次
///   `Sense::hover` 部件,同一块矩形被注册成两个部件(理由同
///   `list.rs::new_button` 文档注释,那里已经写过一次,这里不重复整段)。
///
/// 外面套 `add_enabled_ui`:`Response::enabled` 取的正是这一层的
/// `Ui::is_enabled`,不套的话读回来永远是 true——这是 `labeled_button`
/// 相对 `new_button` 独有的一层,`new_button` 不需要(它没有「禁用」态)。
pub(super) fn labeled_button(
    ui: &mut egui::Ui,
    id: egui::Id,
    text: &str,
    enabled: bool,
    on_disabled: Option<&str>,
) -> bool {
    let mut clicked = false;
    ui.add_enabled_ui(enabled, |ui| {
        let galley = egui::WidgetText::from(text).into_galley(
            ui,
            None,
            ui.available_width(),
            egui::TextStyle::Button,
        );
        let padding = ui.spacing().button_padding;
        let size =
            (galley.size() + padding * 2.0).at_least(egui::vec2(0.0, ui.spacing().interact_size.y));
        let (_auto_id, rect) = ui.allocate_space(size);
        let resp = ui.interact(rect, id, egui::Sense::click());
        if ui.is_rect_visible(rect) {
            let visuals = ui.style().interact(&resp);
            ui.painter().rect(
                rect.expand(visuals.expansion),
                visuals.rounding,
                visuals.weak_bg_fill,
                visuals.bg_stroke,
            );
            let text_pos = ui
                .layout()
                .align_size_within_rect(galley.size(), rect.shrink2(padding))
                .min;
            ui.painter().galley(text_pos, galley, visuals.text_color());
        }
        if enabled {
            clicked = resp.clicked();
        } else if let Some(msg) = on_disabled {
            resp.on_disabled_hover_text(msg.to_owned());
        }
    });
    clicked
}

/// 会话管理器弹窗:双栏(左列表 300px + 右编辑表单)合成单窗(F90)。
/// `store_available=false` 时(待定 G:keyring/库打开失败)不崩,只展示兜底提示。
///
/// `presence` 把参数个数从 7 顶到 8,超过 clippy 默认阈值——跟 `ui/mod.rs::UiFrame`
/// 那次「9 参就该聚成结构体」的取舍不同:那里是给外壳级入口,新增字段是常态;
/// 这里已经是内部叶子函数,再引入一个专用结构体纯粹为了压参数数没有实际收益。
#[allow(clippy::too_many_arguments)]
pub fn show(
    ctx: &egui::Context,
    t: &Theme,
    ui_state: &mut UiState,
    sessions: &[SessionRecord],
    groups: &[GroupRecord],
    store_available: bool,
    connected: Option<SessionId>,
    presence: SecretPresence,
    appearance: &crate::ui::badge::AppearanceCache,
) -> Option<egui::Rect> {
    if !ui_state.session_manager_open {
        return None;
    }

    let mut open = true;
    // 走查 16:键盘快捷键。在画任何东西**之前**解:`Action::Close` 要能在这一帧
    // 就把窗口关掉,而 `egui::Window::open(&mut open)` 只在 show 之前读一次
    // `open`。判定本身是纯函数,见 `keys::scan` 的文档(以及它为什么不碰
    // `app.rs` 的键盘路由 —— 陷阱 T8)。
    let typing = ctx.memory(|m| m.focused().is_some());
    for action in ctx.input(|i| keys::scan(i, typing)) {
        match action {
            keys::Action::Close => {
                // 有确认框挂着时,Esc 先撤确认框 —— 那一下十有八九是「我不想
                // 删了 / 我不想丢改动」,连窗口一起关掉会顺手把编辑丢了。
                if ui_state.confirm_switch {
                    ui_state.confirm_switch = false;
                    ui_state.pending_switch = None;
                } else if ui_state.pending_delete.is_some() {
                    ui_state.pending_delete = None;
                } else {
                    open = false;
                }
            }
            keys::Action::Prev | keys::Action::Next => {
                let order = list::visible_order(sessions, groups, &ui_state.search);
                let forward = action == keys::Action::Next;
                if let Some(id) = keys::step(&order, ui_state.editor_id, forward) {
                    // 走 `pending_switch` 而不是直接换 `editor`:表单脏的时候
                    // 要弹确认,这套机制已经在那条路上了(见本文件下方的消费点)。
                    ui_state.pending_switch = Some(SwitchTarget::Session(id));
                }
            }
            keys::Action::Open => {
                if let Some(id) = ui_state.editor_id {
                    ui_state.connect_request = Some(id);
                }
            }
            keys::Action::Tab(n) => {
                // 没在编辑任何会话时右栏是空态,切页没有意义。
                if ui_state.editor.is_some() {
                    ui_state.editor_tab = n;
                }
            }
        }
    }
    // 修复(F90 初版):`ui.set_min_height(CONTENT_MIN_HEIGHT)` 曾经是硬地板——
    // 主窗口可用高度小于它时 `Window` 不收缩,而是整体溢出可见区,底部的分隔线/
    // 「+ 新建」按钮被顶到屏幕外(Windows 11 实机截图确认)。这里先改成
    // 「`CONTENT_MIN_HEIGHT` 与实际可用高度的较小值」,可用高度不足时地板
    // 跟着降,窗口自己收缩而不是溢出。
    //
    // 用 `ctx.available_rect()` 而不是 `ctx.screen_rect()`:前者是「菜单栏/
    // 状态栏两个 `TopBottomPanel` 已经 show 完、让出之后剩下的区域」——
    // `ui/mod.rs::build_ui` 算 `central_px` 时用的就是同一个概念,同一帧里
    // 那两栏也已经在 `session_manager::show` 之前 show 过了;后者是整个物理
    // 屏幕,会把菜单栏/状态栏占的高度也算进「可用」,地板还是算多了。再减去
    // `window_chrome_reserve(ctx)` 给 `Window` 自己的标题栏/边距留余量
    // (按 egui 实际渲染公式现算,见该函数文档)。
    let chrome_reserve = window_chrome_reserve(ctx);
    let content_min_height =
        CONTENT_MIN_HEIGHT.min((ctx.available_rect().height() - chrome_reserve).max(0.0));

    let window_resp = egui::Window::new(WINDOW_TITLE)
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size([WINDOW_W, WINDOW_H])
        .min_width(WINDOW_W)
        .frame(
            egui::Frame::window(&ctx.style())
                .fill(theme::c32(t.bar_status))
                .rounding(12.0),
        )
        .show(ctx, |ui| {
            // 复核修错(F90):这里原来只 `set_min_height`——只给地板、不给天花板。
            // 用 `cargo test -p mullion-app --lib session_manager::tests -- --nocapture`
            // 实测追出来:`egui::SidePanel::show_inside_dyn`
            // (`egui-0.30.0/src/containers/panel.rs`)会把自己的内容矩形通过
            // `ui.expand_to_include_rect` 报给外层 `ui`,还带一个「只能变大不能
            // 变小」的棘轮 `ui.set_min_height(ui.max_rect().height())`——SidePanel
            // 拿到的 `max_rect` 本身就继承自外层这个只设了地板、没设天花板的
            // content ui,于是「地板 480 → SidePanel 报告需要更多 → 外层被撑大 →
            // 下一次 SidePanel 拿到更大的 max_rect → 报告需要更多」这个环从第一帧
            // 就已经跑起来,content ui 最终稳定在远超 480 的高度(实测:字号/
            // 间距调大后稳定在 586),而这个高度加上 `Window` 自己的标题栏/边距
            // 就会顶穿屏幕——`Window` 自身的「不超过 constrain_rect」保护
            // (`window.rs::show_dyn` 里 `resize.max_size.y = ...min(max_height)`,
            // `max_height = constrain_rect.height() - title_bar_height`)对此不管用:
            // 标题栏的真实高度是 `TitleBar::new`
            // (`window.rs`)里 `font_height.max(interact_size.y)` 算出来的,会跟着
            // 无障碍字号/缩放一起变大,但算 `max_height` 这行用的是更早、更粗糙的
            // 纯 `font_height` 估计,没有 `.max(interact_size.y)`——这两处口径不一致,
            // 是 egui 自身的既有偏差,不是我们能从 `Window` 调用方修的。
            //
            // 第一版复核修复(已废弃)：`ui.set_height(content_min_height)`
            // ——`set_height` = `set_min_height` + `set_max_height` 一起钉死。这个
            // 天花板确实能堵住上面的棘轮环,但代价是每一帧都无条件把
            // `content_ui` 的 `max_rect` 高度**强制改写**成固定的
            // `content_min_height`——用一个模拟真实指针拖拽的诊断探针(排查过程中
            // 临时写的,后来被下面永久保留的
            // `dragging_the_resize_handle_grows_window_and_stops_only_at_screen_edge`
            // 取代)实测追出来:
            // `egui::Placer::set_max_height`(`placer.rs`)不是「设一个上限,超了才
            // 削」,而是直接 `region.max_rect.max.y = ...`,**无条件覆写**成传入的
            // 值——不管 `Window` 内部 `Resize::begin()` 当时已经从用户拖拽算出的
            // `state.desired_size` 是多少,统统被这一行覆盖掉。也就是说这一帧的
            // 拖拽结果被我们自己的天花板每帧作废,窗口高度被钉死成常量,垂直
            // 拖拽 resize 整个失效——协调者实测复现:同一条连接把窗口从初始
            // 高度往下拖,`outer.rect` 完全不跟手,永远等于那个常量。
            //
            // 现在的修复:天花板只在**真的会溢出屏幕**的那一帧才出手,平时完全
            // 不碰 `max_rect`——保留 `Resize`/拖拽给的原始值,不去无条件覆写它。
            // `avail` 是「刨去 chrome 之后屏幕真正能装下的高度」,跟 `content_min_height`
            // 算地板用的是同一个 `avail`,只是这里不取 `.min`,单独留一份给天花板判断。
            // 只有当 `ui.max_rect().height()`(这一帧 `Resize` 给到的、已经反映了
            // 拖拽/棘轮的候选高度)已经超过 `avail` 时才调用 `set_max_height(avail)`
            // 把它削回去;没超过就什么都不做,拖拽因此完全不手,能跟手。
            // 用同一个诊断探针实测确认过:平时(不逼近屏幕边缘)拖拽窗口变大变小,
            // `outer.rect` 逐步跟随每一步拖拽;只有拖到会顶穿屏幕的那一步,高度才被
            // 钉在 `avail`,不再继续跟手往外长——这正是「不溢出」该有的样子,不是
            // 「resize 整个失效」。守护测试见下面
            // `dragging_the_resize_handle_grows_window_and_stops_only_at_screen_edge`。
            let avail = (ctx.available_rect().height() - chrome_reserve).max(0.0);
            ui.set_min_height(content_min_height);
            if ui.max_rect().height() > avail {
                ui.set_max_height(avail);
            }

            // 地板:让 Window 至少给出容得下「左栏 + 右栏」的宽度。
            // 不能靠 `Window::min_width` —— 它只约束 Resize 的下限,
            // 约束不到 CentralPanel 的绘制。
            let wm = ctx.style().spacing.window_margin;
            ui.set_min_width(WINDOW_W - (wm.left + wm.right));

            // 天花板:横向可用量另算 —— `window_chrome_reserve` 是纵向量
            // (标题栏高 + 上下 margin),横向套用是量纲错误。
            let avail_w = (ctx.available_rect().width() - (wm.left + wm.right) - SLACK).max(0.0);
            // 必须条件式。`Placer::set_max_width` 是无条件覆写 region.max_rect,
            // 无脑设会作废 Resize 当帧从拖拽算出的候选尺寸,resize 手柄就拖不动了。
            if ui.max_rect().width() > avail_w {
                ui.set_max_width(avail_w);
            }

            // F94:把这一帧的实际内容宽度报告回外层,否则窗口外框不跟手。
            // `Window` 内部 `resize.resizable(false)`(「We resize it manually」),
            // 于是 `Resize::end` 走「Probably a window」分支,用 `last_content_size`
            // (= content ui 的 `min_rect`)去 `advance_cursor_after_rect`,外框
            // `outer_rect` 由它算出;而拖拽结果只写进 `desired_size`,只体现在
            // `max_rect` 上。高度方向靠 `SidePanel` 那条「只增不减」棘轮把两者
            // 对上了,宽度方向没有任何对应机制——不补这一句,外框宽度就永远停在
            // 上面那个常量地板,而 `CentralPanel` 按 `max_rect` 铺满画到框外。
            // 必须放在天花板 clamp **之后**:要报告的是被削过的宽度,不是削之前的。
            ui.set_min_width(ui.max_rect().width());

            // §3.1 降级:没有会话库时不画双栏,只给一句话,避免用户对着空表单填半天。
            if !store_available {
                ui.colored_label(
                    theme::c32(t.danger),
                    "会话库不可用,无法读写会话(详见状态栏错误)。",
                );
                return;
            }

            egui::SidePanel::left(ui.id().with("sm_list"))
                .resizable(true)
                .default_width(LIST_W)
                .width_range(LIST_MIN_W..=LIST_MAX_W)
                .frame(
                    egui::Frame::none()
                        .fill(theme::c32(t.panel_bg))
                        .inner_margin(14.0),
                )
                .show_inside(ui, |ui| {
                    list::show(ui, t, ui_state, sessions, groups, connected, appearance)
                });

            egui::CentralPanel::default()
                .frame(
                    egui::Frame::none()
                        .fill(theme::c32(t.bar_status))
                        .inner_margin(14.0),
                )
                .show_inside(ui, |ui| {
                    // 挂显式 id 供守护测试读回真实矩形,见 `editor_root_id()`。
                    let rect = ui.max_rect();
                    ui.interact(rect, editor_root_id(), egui::Sense::hover());
                    editor::show(ui, t, ui_state, groups, sessions, presence)
                });
        });

    // 借用已随 `Window::show` 闭包结束而释放,这里才能安全地整体读写
    // `ui_state.editor`。`build_draft` 要读整个 `EditorBuffer`,`editor::show`
    // 内部正持着它的 `&mut`,所以「保存」只在那边置一个 `save_click` 标志,
    // 真正的施加挪到这里。
    // 「认证」Tab 里导入/清除私钥留下的一行提示 → 转成编辑器顶部那条通知。
    // 必须在下面判脏**之前**抽走:它是瞬态的,留在缓冲里会让「导入失败」也
    // 把表单判成脏,切会话时凭空弹一个确认。
    if let Some(buf) = ui_state.editor.as_mut() {
        if let Some(note) = buf.key_note.take() {
            ui_state.key_drop_note = Some(note);
        }
    }

    if let Some(then_connect) = ui_state.save_click.take() {
        if let Some(buf) = ui_state.editor.as_ref() {
            match build_draft(buf) {
                Ok(draft) => {
                    let (password, passphrase, proxy_password, private_key) = secret_fields(buf);
                    ui_state.save_request = Some(SaveIntent {
                        editing_id: ui_state.editor_id,
                        draft,
                        password,
                        passphrase,
                        proxy_password,
                        private_key,
                        then_connect,
                    });
                    // 保存成功后基线要跟上,否则刚存完就被判成脏。
                    ui_state.editor_baseline = ui_state.editor.clone();
                }
                Err(msg) => ui_state.set_error(msg),
            }
        }
    }

    // 切换目标的消费:表单脏就先挂起等确认,不静默丢弃用户刚打的字。
    // 借用已释放,才能安全整体读写 `ui_state.editor`/`editor_baseline`。
    if ui_state.pending_switch.is_some() {
        let dirty = match (ui_state.editor.as_ref(), ui_state.editor_baseline.as_ref()) {
            (Some(b), Some(base)) => is_dirty(b, base),
            _ => false,
        };
        if dirty {
            ui_state.confirm_switch = true;
        } else {
            apply_switch(ui_state, sessions);
        }
    }

    // 确认横幅上点了「丢弃并切换」→ `editor.rs` 只中转一个 bool(它正持着
    // `ui_state.editor` 的 `&mut`,不能在那边直接调 `apply_switch`)。
    if std::mem::take(&mut ui_state.discard_and_switch) {
        apply_switch(ui_state, sessions);
    }

    // 「认证」Tab 里「浏览…」私钥文件按钮同一帧被点了 → 转成
    // `UiState::pick_key_request`(`app.rs` 事后另起线程开系统文件对话框,
    // 不能在 egui 闭包里同步阻塞)。同样借用已释放,才能安全整体读写
    // `ui_state.editor`。
    if let Some(buf) = ui_state.editor.as_mut() {
        if std::mem::take(&mut buf.pick_key_clicked) {
            ui_state.pick_key_request = true;
        }
        if std::mem::take(&mut buf.pick_icon_clicked) {
            ui_state.pick_icon_request = true;
        }
    }

    if !open {
        ui_state.close_session_manager();
    }
    window_resp.map(|r| r.response.rect)
}

/// 真正切到 `pending_switch` 指向的目标,同时重置基线与 Tab。
///
/// `SwitchTarget::Session` 若目标会话在这一帧之间被删了(左栏的删除也走
/// 意图,可能先落地),直接放弃切换、原样保留当前表单——`confirm_switch`
/// 已经在上面被置 `false`,但 `editor`/`editor_baseline` 都没动,所以表单
/// 是否仍然脏由下一次真正触发的切换意图重新判定,不会静默丢弃用户的编辑。
fn apply_switch(ui_state: &mut UiState, sessions: &[SessionRecord]) {
    let Some(target) = ui_state.pending_switch.take() else {
        return;
    };
    ui_state.confirm_switch = false;
    match target {
        SwitchTarget::NewDraft => {
            // 走查 21:用户名预填成当前系统账号,光标落到「名称」上。
            ui_state.editor = Some(EditorBuffer::new_draft());
            ui_state.editor_id = None;
            ui_state.focus_name_request = true;
        }
        SwitchTarget::Session(id) => {
            let Some(rec) = sessions.iter().find(|r| r.id == id) else {
                return;
            };
            ui_state.editor = Some(EditorBuffer::from_record(rec));
            ui_state.editor_id = Some(id);
        }
    }
    // 基线必须在这里同步设置:漏了它,刚打开的会话立刻被判成脏,
    // 下一次切换就会弹一个莫名其妙的确认。
    ui_state.editor_baseline = ui_state.editor.clone();
    ui_state.editor_tab = 0;
    // 走查 15:触碰位跟着表单一起归零。上一条会话上留下的「碰过」会让新表单
    // 一打开就带红字 —— 用户根本没碰过这几个框。
    ui_state.touched = Default::default();

    // F92:换了会话,上一条的拨测结果不再有意义;快照(`probe_form`)里
    // 还揣着三个明文凭据字段,一并清掉以缩短明文在内存里的驻留窗口。
    ui_state.probe = ProbeState::Idle;
    ui_state.probe_form = None;
    ui_state.probe_cancel = true;
    // F93:上一条会话的拖拽提示(如「已取第一个文件,忽略其余 2 个」)不能
    // 跟着漂到下一条表单上 —— 用户会以为刚才对新会话做了什么。
    ui_state.key_drop_note = None;
}

/// 密码框的三态控件(F73)。
///
/// 三个显示状态:
///   1. 没碰过 + 库里有值 → 6 个 `*` 占位 + `password(true)`,只读观感;
///   2. 没碰过 + 库里没值 → 空框 + hint `empty_hint`(由调用方传入,见该参数文档);
///   3. 碰过        → 正常可编辑 + 右侧「撤销」按钮。
///
/// **占位符永远不会流进 `SecretField::Set`**:状态 1/2 下 `touched` 是 false,
/// `secret_fields` 直接给 `Keep`,根本不读框里的字符串。迁移点选
/// `gained_focus()` 而不是 `changed()` —— 聚焦那一刻就把框清空,用户看到的是
/// 一个空框(而不是 6 个星号后面接着自己输的字),也就不可能把 `******` 连同
/// 新密码一起存进去。
///
/// 位数固定 6 位:黑点数量若跟真实长度走,就把密码长度泄漏给了肩窥者。
/// `empty_hint`:状态 2(没碰过 + 库里没值)下显示的占位提示文案。
///
/// 不能写死一个文案:本函数被密码、私钥口令、代理口令三处调用,语义并不相同——
/// 「留空表示无口令」只对私钥口令成立(它确实可选),对密码/代理口令这两个
/// 字段留空并不等于「无密码」,写死会误导用户,所以由调用方按各自语义传入。
pub(super) fn secret_edit(
    ui: &mut egui::Ui,
    t: &Theme,
    id: &str,
    value: &mut String,
    touched: &mut bool,
    has_stored: bool,
    empty_hint: &str,
) {
    // 走查 P0-1:`secret_edit` 被 `grid()` 的单元格调用——egui 的 `Grid` 把
    // 闭包里**每一次顶层调用**都当成「下一个单元格」放进当前行(不是
    // 「同一单元格里继续往下叠」),`ui.horizontal(..)` 后面紧跟一次
    // `ui.colored_label(..)` 会被摆成本行的第三个格子,跟输入框挤在
    // 同一行右边,不会像期望的那样另起一行(同 `jump()` 里
    // `ui.vertical(|ui| chain_editor(..))` 的写法:要让多个控件挤进
    // **同一个格子**并在格子内部纵向排布,必须用一个 `ui.vertical` 包起来
    // 作为唯一的顶层调用)。
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            use crate::ui::metrics::{button_reserve, field_w, FIELD_W_M};
            if *touched {
                // 走查 P0-1:老写法 `desired_width(f32::INFINITY)` 让输入框吃光
                // 整行,「撤销」和后面的警告被推出面板只露半个字。改成先量出
                // 「撤销」实际要多宽、从可用宽里扣掉,再取 FIELD_W_M 上限。
                // 量而不是写常量:按钮宽随字号/DPI 变,写死的值会悄悄失同步。
                // `+ TEXT_EDIT_MARGIN_X`:`TextEdit` 自己的默认内边距是
                // `Margin::symmetric(4.0, 2.0)`(egui-0.30.0
                // text_edit/builder.rs:129),`desired_width` 只圈住内容区,
                // 实际画出来的外框会再多出 `margin.sum().x = 8.0`。这个 8px
                // 只有在 `desired_width` 严格小于「真实可用宽」时才会显形
                // (即本分支这种「预留了空间给后面按钮」的情况)——不加的话,
                // 预留的空间被内边距吃掉一部分,「撤销」照样被顶出面板,只是
                // 从整行溢出变成溢出 8px,肉眼几乎看不出但仍会被裁。用专门的
                // 常量而不是借 `SP_S`:两者数值恰好都是 8.0 但语义无关,间距
                // 刻度调整时会静默带崩这里。
                let reserve = button_reserve(ui, "撤销") + crate::ui::metrics::TEXT_EDIT_MARGIN_X;
                let w = field_w(ui.available_width(), FIELD_W_M, reserve);
                ui.add(
                    egui::TextEdit::singleline(value)
                        .id_salt(id)
                        .password(true)
                        .desired_width(w),
                );
                if ui.small_button("撤销").clicked() {
                    *touched = false;
                    value.clear();
                }
            } else if has_stored {
                let mut placeholder = "******".to_string();
                let w = field_w(ui.available_width(), FIELD_W_M, 0.0);
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut placeholder)
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
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 走查 16:快捷键真的接到 `show()` 上了 —— `keys::scan` 单测只证明「按键
    /// 被解成了动作」,证不了那个动作有人消费。这条从 `show()` 驱动:Esc 关窗、
    /// ↓ 落一个切换意图、Ctrl+2 换 Tab。
    ///
    /// 自证会变红:把 `show()` 顶部那个 `for action in ...` 循环整段删掉,
    /// 三段断言全炸。
    #[test]
    fn keyboard_shortcuts_are_actually_wired_into_the_manager() {
        use mullion_store::model::{Auth, AuthKind, Connection, Identity, Protocol};
        let t = crate::theme::MULLION_DARK;
        let rec = |id: u64, name: &str| SessionRecord {
            id: SessionId(id),
            modified_at: "t".into(),
            identity: Identity {
                name: name.into(),
                note: String::new(),
                group_id: None,
                tags: Vec::new(),
            },
            connection: Connection {
                host: "10.0.0.1".into(),
                port: 22,
                protocol: Protocol::Ssh,
            },
            auth: Auth {
                user: "root".into(),
                kind: AuthKind::Password,
            },
            terminal: Default::default(),
            appearance: Default::default(),
            network: Default::default(),
            automation: Default::default(),
        };
        let sessions = vec![rec(1, "a"), rec(2, "b")];
        let key = |k: egui::Key, modifiers: egui::Modifiers| egui::Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        let drive = |ui_state: &mut UiState, events: Vec<egui::Event>, modifiers| {
            let ctx = egui::Context::default();
            let mut run = |events: Vec<egui::Event>| {
                let _ = ctx.run(
                    egui::RawInput {
                        events,
                        modifiers,
                        ..Default::default()
                    },
                    |ctx| {
                        show(
                            ctx,
                            &t,
                            ui_state,
                            &sessions,
                            &[],
                            true,
                            None,
                            SecretPresence::default(),
                            &crate::ui::badge::AppearanceCache::default(),
                        );
                    },
                );
            };
            run(Vec::new()); // 布局稳定一帧再送按键
            run(events);
        };

        // ↓ → 落一个切到下一条会话的意图。
        let mut st = UiState {
            session_manager_open: true,
            editor_id: Some(SessionId(1)),
            ..Default::default()
        };
        drive(
            &mut st,
            vec![key(egui::Key::ArrowDown, egui::Modifiers::default())],
            egui::Modifiers::default(),
        );
        assert!(
            st.editor_id == Some(SessionId(2)) || st.confirm_switch,
            "↓ 该切到下一条会话(或因表单脏而弹确认),实得 editor_id={:?}",
            st.editor_id
        );

        // Ctrl+2 → 换到第二个 Tab。
        let cmd = egui::Modifiers {
            command: true,
            ..Default::default()
        };
        let mut st = UiState {
            session_manager_open: true,
            editor: Some(EditorBuffer::default()),
            editor_tab: 0,
            ..Default::default()
        };
        drive(&mut st, vec![key(egui::Key::Num2, cmd)], cmd);
        assert_eq!(st.editor_tab, 1, "Ctrl+2 该切到第二个 Tab");

        // Esc → 关窗。
        let mut st = UiState {
            session_manager_open: true,
            ..Default::default()
        };
        drive(
            &mut st,
            vec![key(egui::Key::Escape, egui::Modifiers::default())],
            egui::Modifiers::default(),
        );
        assert!(!st.session_manager_open, "Esc 该关掉会话管理器");
    }

    /// 走查 15:切换表单时触碰位必须归零。上一条会话上留下的「碰过主机」
    /// 会让下一条表单一打开就顶着红字 —— 用户还没碰过那个框。
    ///
    /// 自证会变红:把 `apply_switch` 里的 `ui_state.touched = Default::default();`
    /// 删掉,这条报「切换后不该还留着触碰位」。(已实测确认变红。)
    #[test]
    fn switching_forms_clears_which_fields_you_have_been_in() {
        let mut ui_state = UiState {
            touched: validate::Touched {
                name: true,
                host: true,
                user: true,
                port: true,
            },
            pending_switch: Some(SwitchTarget::NewDraft),
            ..Default::default()
        };
        apply_switch(&mut ui_state, &[]);
        assert_eq!(
            ui_state.touched,
            validate::Touched::default(),
            "换了张表单,上一张上「碰过哪些框」不该跟着过来"
        );
    }

    /// 复审坑:`egui::CollapsingHeader::new(text)` 默认把标题文本本身当 id 源
    /// (`egui-0.30.0/src/containers/collapsing_header.rs::new`)。两个分组
    /// 名字相同、桶内会话数也相同时,列表里两个 header 的标题文本会完全一致
    /// ——不加 `.id_salt` 就会撞 id、共享同一份展开/收起状态,点开一个另一个
    /// 也跟着变。这条测试直接调 `group_header`(`show()` 内部实际用的同一个
    /// 函数,不是重抄一遍表达式),同一个父 `ui`、相同标题、不同 `gid`:
    /// 去掉 `group_header` 里的 `.id_salt(gid)` 这行,两个 `header_response.id`
    /// 会相等,下面的 `assert_ne!` 就会失败(已实测确认,见提交说明)。
    #[test]
    fn collapsing_header_id_salt_disambiguates_same_titled_groups() {
        let ctx = egui::Context::default();
        let mut ids: Option<(egui::Id, egui::Id)> = None;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let resp_a = group_header("生产", Some(GroupId(1)), 1).show(ui, |_| {});
                let resp_b = group_header("生产", Some(GroupId(2)), 1).show(ui, |_| {});
                ids = Some((resp_a.header_response.id, resp_b.header_response.id));
            });
        });
        let (id_a, id_b) = ids.expect("闭包必须跑到底,写回 ids");
        assert_ne!(
            id_a, id_b,
            "两个分组标题相同时,header 的持久化 id 必须靠 gid 区分,否则展开状态会互相串"
        );
    }

    /// 复核修错(F90):判定靶子必须是「+ 新建」按钮**真实渲染出的 `Response::rect`**,
    /// 不能再用文字锚点(`TextShape::pos`)。文字锚点只在按钮矮(默认字号)时约等于
    /// 按钮矩形——按钮被下面第二条测试撑高后,锚点还停在按钮内容区顶部附近,
    /// 真正该测的按钮底边早就跑到远处去了(实测过一次假阳性:按钮真实矩形底边
    /// 694,文字锚点只有 637,屏幕高 680,14px 的真实溢出被锚点判定当成了通过)。
    /// 跟 `toolbar.rs` 里同一类问题的解法一致(见 `build_ui_clicking_a_preset_button_wires_through_to_actions_f82`
    /// 的注释):`list::new_button_id()` 给了显式 id,`Context::read_response` 就能
    /// 拿到精确矩形。
    ///
    /// **必须在 `ctx.run(...)` 的闭包内部调用**,不能等 `run()` 返回之后再调——
    /// 排查这条测试时发现:`egui::Context` 每个 pass 维护 `this_pass`/`prev_pass`
    /// 两份 widget 记录,`end_pass`(`run()` 内部收尾时调用)结尾做的是
    /// `mem::swap(prev_pass, this_pass)`(见 egui-0.30.0 `context.rs::end_pass`),
    /// 而 `read_response` 优先查 `this_pass`、查不到才落到 `prev_pass`(见
    /// `context.rs::read_response`)。这意味着 `run()` **返回之后**,`this_pass`
    /// 里放的是上一次 swap 之前的内容,也就是**上上一帧**(N-2)的旧记录——
    /// 而这个按钮每帧都用同一个显式 id 注册,`this_pass` 永远命中,
    /// `.or_else(prev_pass)` 那条回退分支永远走不到,读到的就是整整慢一帧的
    /// 陈旧矩形。已实测复现:同一屏幕高度 400 场景下,在闭包内部读到收敛值
    /// `396`,`run()` 返回后在外面读到的却是收敛前一帧的 `403`——两次读的是
    /// 同一个按钮,数字却不一样,连带着让这条测试在真实高度差只有 3px 的场景下
    /// 也报了错(见下方测试历史上第一版的失败输出)。所以这里的口子只开在
    /// 闭包**内部**、`show()` 调用之后。
    fn new_button_rect(ctx: &egui::Context) -> egui::Rect {
        ctx.read_response(super::list::new_button_id())
            .expect("「+ 新建」按钮应该已经画出来了")
            .rect
    }

    /// 复核 F90 bug:主窗口可用高度不足时,`ui.set_min_height(CONTENT_MIN_HEIGHT)`
    /// 曾经是硬地板——`Window` 不收缩,而是整体溢出可见区,底部「+ 新建」按钮被
    /// 顶到屏幕外(Windows 11 实机截图确认,880×560 默认窗口、`screen_rect` 高
    /// 400 时按钮 y 坐标算出来是 480.7,远超屏幕高度 400)。
    ///
    /// 这条测试驱动真实的 `session_manager::show`,把 `RawInput::screen_rect` 设成
    /// 一个偏小的高度(400,远小于 `CONTENT_MIN_HEIGHT`=480 + chrome 余量),跑够
    /// 帧数让布局(含 `list.rs` 新增的 `TopBottomPanel` 持久化状态)稳定,再用
    /// `new_button_rect` 读回按钮的真实矩形,断言它的**底边**落在 `screen_rect`
    /// 内 —— 不是被顶到屏幕外。
    ///
    /// 自证会变红(两处独立破坏,各自单独就足以让这条测试报错,见提交说明的
    /// 实测报错原文):
    /// 1. 破坏修复 A:把这里的 `ui.set_min_height(content_min_height)` 改回
    ///    `ui.set_min_height(CONTENT_MIN_HEIGHT)`(即撤销「跟可用高度取较小值」
    ///    这层 clamp)——窗口溢出,按钮 y 坐标算出来远大于屏幕高度 400。
    /// 2. 破坏修复 B:把 `list.rs` 里的 `TopBottomPanel::bottom` 改回手算
    ///    `list_h = ui.available_height() - BOTTOM_BAR_H` 喂给
    ///    `ScrollArea::max_height`——`ui.available_height()` 在 `SidePanel` 内
    ///    返回的是 `Window` 的布局高度而不是真实可见高度,`ScrollArea` 撑满这个
    ///    偏大的高度,同样把按钮推出可见区。
    #[test]
    fn new_button_stays_within_screen_rect_when_main_window_is_short() {
        let t = crate::theme::MULLION_DARK;
        let sessions: Vec<SessionRecord> = Vec::new();
        let groups: Vec<GroupRecord> = Vec::new();
        let mut ui_state = UiState {
            session_manager_open: true,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        // 屏幕宽 1000(超过 `.min_width(WINDOW_W)`(880),不触发宽度相关的约束),
        // 高只有 400 —— 明显小于 `CONTENT_MIN_HEIGHT`(480)。
        let screen_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 400.0));
        let input = || egui::RawInput {
            screen_rect: Some(screen_rect),
            ..Default::default()
        };

        // egui 的布局(尤其是这次改动新增的 `TopBottomPanel` 持久化状态)通常要跑
        // 两帧以上才稳定,先空跑一帧,再取第二帧的输出断言。矩形要在第二帧的
        // 闭包**内部**读(`new_button_rect` 文档注释解释了为什么不能等 `run()`
        // 返回之后再读)。
        let _ = ctx.run(input(), |ctx| {
            show(
                ctx,
                &t,
                &mut ui_state,
                &sessions,
                &groups,
                true,
                None,
                SecretPresence::default(),
                &crate::ui::badge::AppearanceCache::default(),
            );
        });
        let mut rect = None;
        let _ = ctx.run(input(), |ctx| {
            show(
                ctx,
                &t,
                &mut ui_state,
                &sessions,
                &groups,
                true,
                None,
                SecretPresence::default(),
                &crate::ui::badge::AppearanceCache::default(),
            );
            rect = Some(new_button_rect(ctx));
        });
        let rect = rect.expect("闭包必须跑到底,写回按钮矩形");
        assert!(
            rect.max.y < screen_rect.max.y,
            "「+ 新建」按钮的真实矩形底边 y={} 落在屏幕高度 {} 之外,说明窗口溢出\
             或列表撑满了偏大的可用高度,把底栏顶出了可见区(rect={rect:?})",
            rect.max.y,
            screen_rect.max.y
        );
    }

    /// F90:右栏(编辑器)不许画到窗口矩形之外。
    ///
    /// 根因是 `SidePanel::show_inside` 用 `expand_to_include_rect` 只增不减地
    /// 回报尺寸,而 `CentralPanel::show_inside` 吃掉 `available_rect_before_wrap()`
    /// 却**不回报**——窗口自身没被撑宽,右栏就直接画到窗口外被裁掉。
    ///
    /// 自证变红的方式:注释掉 `show()` 里 `ui.set_min_width(...)` 那一行。
    #[test]
    fn editor_panel_stays_within_window_rect() {
        let t = crate::theme::MULLION_DARK;
        let sessions: Vec<SessionRecord> = Vec::new();
        let groups: Vec<GroupRecord> = Vec::new();
        let mut ui_state = UiState {
            session_manager_open: true,
            editor: Some(EditorBuffer::default()),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        // 屏幕给得很宽,窗口本身却只有 default_size 那么大 —— 正是溢出的场景。
        let screen_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 900.0));
        let input = || egui::RawInput {
            screen_rect: Some(screen_rect),
            ..Default::default()
        };
        let _ = ctx.run(input(), |ctx| {
            show(
                ctx,
                &t,
                &mut ui_state,
                &sessions,
                &groups,
                true,
                None,
                SecretPresence::default(),
                &crate::ui::badge::AppearanceCache::default(),
            );
        });
        let mut rects = None;
        let _ = ctx.run(input(), |ctx| {
            let window_rect = show(
                ctx,
                &t,
                &mut ui_state,
                &sessions,
                &groups,
                true,
                None,
                SecretPresence::default(),
                &crate::ui::badge::AppearanceCache::default(),
            );
            let editor_rect = ctx.read_response(editor_root_id()).map(|r| r.rect);
            rects = Some((window_rect, editor_rect));
        });
        let (window_rect, editor_rect) = rects.expect("闭包必须跑到底,写回两个矩形");
        let window_rect = window_rect.expect("会话管理器窗口应该已经画出来了");
        let editor_rect = editor_rect.expect("右栏编辑器应该已经画出来了");
        assert!(
            editor_rect.right() <= window_rect.right() + SLACK,
            "右栏溢出窗口:editor.right={} > window.right={}",
            editor_rect.right(),
            window_rect.right()
        );
    }

    /// 复核修复 B 的独立性:原 `list.rs` 里的 `BOTTOM_BAR_H = 40.0` 是一个假设
    /// 「底部分隔线 + 按钮行」固定只占 ~30px 的硬编码估计值。只要真实渲染出来
    /// 的内容比这个假设高,`ScrollArea::max_height` 留给底栏的空间就不够,按钮
    /// 会被挤出可见区——**即使修复 A(`Window` 高度地板/天花板)已经生效、
    /// 主窗口本身完全没有溢出屏幕**。这正是设计里强调的「两个独立缺陷」:
    /// 上一条测试(`..._when_main_window_is_short`)在正常字号下测不出这条,
    /// 因为默认字号下真实内容(~30px)比 40px 的估计值还小,怎么缩小屏幕都
    /// 不会露馅。
    ///
    /// 用调大 `Style::spacing.interact_size.y` / `item_spacing.y`(相当于用户
    /// 放大了界面缩放或无障碍字号——真实场景,不是编造的极端值)让真实底栏
    /// 内容变高,屏幕给足 680(设计稿默认 `WINDOW_H`,这个高度下修复 A 的
    /// `content_min_height` 会直接取满 `CONTENT_MIN_HEIGHT`=480、不受屏幕高度
    /// clamp 影响,把变量收敛到只剩修复 B 一个),断言「+ 新建」按钮仍落在
    /// 屏幕内。
    ///
    /// 复核 F90 缺陷时的两层实测记录:
    /// 1. 断言靶子从文字锚点换成真实 `Response::rect` 后,这条测试**先自证变红**
    ///    过一次——按钮真实矩形 `[[32.0 594.0]-[76.0 694.0]]`,屏幕高度只有
    ///    680,底边 694 溢出 14px(旧的文字锚点判定拿的是锚点 y≈637,比屏幕矮,
    ///    误判成通过)。追下去发现这 14px **不是** `list.rs` 内容本身算错,而是
    ///    `SidePanel` 把内容高度报回给外层 `ui`(`ui.expand_to_include_rect` +
    ///    只能变大的棘轮 `ui.set_min_height(ui.max_rect().height())`,均见
    ///    `egui-0.30.0/src/containers/panel.rs::SidePanel::show_inside_dyn`),
    ///    而外层 content ui 当时只用 `ui.set_min_height` 给了地板、没给天花板,
    ///    这个「报告更多 → 外层被撑大 → 下一帧报告更多」的环从第一帧就在跑,
    ///    最终稳定在 586(远超地板 480)。
    /// 2. 改成 `ui.set_height`(地板+天花板一起钉死,见 `show()` 里的注释)后
    ///    这条测试转绿,用同样的实测方法确认转绿不是巧合:临时把
    ///    `list.rs` 的 `TopBottomPanel::bottom` 改回手算
    ///    `list_h = ui.available_height() - BOTTOM_BAR_H` 喂给
    ///    `ScrollArea::max_height`(即撤销修复 B,`set_height` 天花板原样保留),
    ///    这条测试在同样的屏幕高度 680 下又能实测出红(按钮被挤出天花板钉住的
    ///    固定预算)——证明修复 B 仍然是必要的独立防线,不是被 `set_height`
    ///    天花板顺带盖住了。
    #[test]
    fn new_button_stays_within_screen_rect_when_real_bottom_bar_content_is_taller_than_the_old_hardcoded_estimate(
    ) {
        let t = crate::theme::MULLION_DARK;
        let sessions: Vec<SessionRecord> = Vec::new();
        let groups: Vec<GroupRecord> = Vec::new();
        let mut ui_state = UiState {
            session_manager_open: true,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        // 模拟更大的界面缩放/字号:默认 `interact_size.y` 是 18~20px 量级,
        // `item_spacing.y` 是个位数。调到 100 / 20 后,「分隔线 + 一行按钮」
        // 这块真实内容的高度会远超原 `BOTTOM_BAR_H = 40.0` 的估计。
        ctx.style_mut(|s| {
            s.spacing.interact_size.y = 100.0;
            s.spacing.item_spacing.y = 20.0;
        });
        let screen_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 680.0));
        let input = || egui::RawInput {
            screen_rect: Some(screen_rect),
            ..Default::default()
        };

        // 跟上一条测试一样,先空跑一帧让布局收敛,矩形在第二帧闭包内部读。
        let _ = ctx.run(input(), |ctx| {
            show(
                ctx,
                &t,
                &mut ui_state,
                &sessions,
                &groups,
                true,
                None,
                SecretPresence::default(),
                &crate::ui::badge::AppearanceCache::default(),
            );
        });
        let mut rect = None;
        let _ = ctx.run(input(), |ctx| {
            show(
                ctx,
                &t,
                &mut ui_state,
                &sessions,
                &groups,
                true,
                None,
                SecretPresence::default(),
                &crate::ui::badge::AppearanceCache::default(),
            );
            rect = Some(new_button_rect(ctx));
        });
        let rect = rect.expect("闭包必须跑到底,写回按钮矩形");
        assert!(
            rect.max.y < screen_rect.max.y,
            "「+ 新建」按钮的真实矩形底边 y={} 落在屏幕高度 {} 之外(rect={rect:?})",
            rect.max.y,
            screen_rect.max.y
        );
    }

    /// 复核 F90 第二版发现的回归:第一版修复(`ui.set_height(content_min_height)`,
    /// 见 `CONTENT_MIN_HEIGHT` 文档注释)把地板和天花板一起钉死成同一个常量,
    /// 代价是垂直拖拽 resize 整个失效——协调者实测复现,`cargo test -p mullion-app
    /// --lib session_manager::tests::probe_drag_resize -- --nocapture`(诊断用
    /// 探针,已在改用现在这版天花板逻辑后删除)也独立坐实:`egui::Placer::
    /// set_max_height`(`placer.rs`)不是「设一个上限,超了才削」,是每次调用都
    /// 无条件把 `content_ui.max_rect` 覆写成传入的值,不管 `Resize` 当帧从拖拽
    /// 算出的候选尺寸是多少——天花板值一旦是个固定常量,窗口高度就被钉死。
    ///
    /// 这条测试用真实指针事件模拟拖拽窗口右下角的 resize 手柄,直接在
    /// `show()` 的渲染闭包**内部**(不是 `ctx.memory(|m| m.area_rect(id))`——
    /// 协调者实测过这条路径在这里会读到陈旧值,宽度读出 312、实际 880)读回
    /// `show()` 返回的真实外层矩形(`window_resp.map(|r| r.response.rect)`),
    /// 对三个阶段分别断言:
    /// 1. 屏幕够高、天花板不会触发时,窗口高度要跟手——分 3 步、每步 10px 往下拖,
    ///    断言每一步窗口底边都恰好前进 10px(容差 1px),不是钉死不动。
    /// 2. 继续拖到会顶穿屏幕(屏幕高度故意设成 700,拖拽目标远超这个高度)那一步,
    ///    断言窗口底边确实被削到不超过屏幕高度——天花板真的在起效,不是没做限制。
    /// 3. 松开后从被削过的位置继续往回拖(缩小方向),断言窗口能继续跟手缩小——
    ///    证明天花板不是「钉死」,被削过一次之后 resize 机制没有被弄死。
    ///
    /// 自证会变红:把下面的 `apply_ceiling` 换成第一版的
    /// `ui.set_height(content_min_height)`(地板天花板一起钉死),第 1 步的
    /// 跟手断言会先炸——实测输出(见提交说明)：窗口高度全程钉在
    /// `content_min_height` 对应的常量,10px 的拖拽步进被吃掉,`assert!` 报
    /// 「未跟手」。
    #[test]
    fn dragging_the_resize_handle_grows_window_and_stops_only_at_screen_edge() {
        let t = crate::theme::MULLION_DARK;
        let sessions: Vec<SessionRecord> = Vec::new();
        let groups: Vec<GroupRecord> = Vec::new();
        let mut ui_state = UiState {
            session_manager_open: true,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        // 屏幕矮一些(700),让「往下拖」这个动作会在中途真的顶穿屏幕,逼出天花板
        // 生效的那一帧;前几步的拖拽幅度不足以触发天花板,用来验证正常 resize。
        let screen_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 700.0));
        let base_input = || egui::RawInput {
            screen_rect: Some(screen_rect),
            ..Default::default()
        };
        let mut run = |ctx: &egui::Context, input: egui::RawInput| -> egui::Rect {
            let mut outer = None;
            let _ = ctx.run(input, |ctx| {
                outer = show(
                    ctx,
                    &t,
                    &mut ui_state,
                    &sessions,
                    &groups,
                    true,
                    None,
                    SecretPresence::default(),
                    &crate::ui::badge::AppearanceCache::default(),
                );
            });
            outer.expect("会话管理器窗口应已渲染并返回外层矩形")
        };

        let mut initial_outer = egui::Rect::NAN;
        for _ in 0..3 {
            initial_outer = run(&ctx, base_input());
        }

        let corner = initial_outer.right_bottom();
        run(
            &ctx,
            egui::RawInput {
                events: vec![egui::Event::PointerMoved(corner)],
                ..base_input()
            },
        );
        run(
            &ctx,
            egui::RawInput {
                events: vec![egui::Event::PointerButton {
                    pos: corner,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::default(),
                }],
                ..base_input()
            },
        );

        // 阶段 1:分 3 步、每步往下拖 10px,屏幕够高,天花板不该触发——每一步
        // 窗口底边都该恰好跟着前进 10px。
        let mut prev_bottom = corner.y;
        for step in 1..=3 {
            let pos = corner + egui::vec2(0.0, 10.0 * step as f32);
            let outer = run(
                &ctx,
                egui::RawInput {
                    events: vec![egui::Event::PointerMoved(pos)],
                    ..base_input()
                },
            );
            let advanced = outer.max.y - prev_bottom;
            assert!(
                (advanced - 10.0).abs() < 1.0,
                "第 {step} 步:窗口底边应跟随拖拽前进约 10px,实际前进 {advanced}\
                 (前一帧底边 {prev_bottom},这一帧 outer={outer:?})——resize 没有跟手,\
                 疑似天花板把 max_rect 钉死了"
            );
            prev_bottom = outer.max.y;
        }

        // 阶段 2:继续拖到远超屏幕高度的位置(屏幕高 700,目标拖到相当于顶边+320,
        // 加上窗口自身顶边偏移,必定超屏),分步拖、多跑几帧让布局收敛。
        let drag_to = corner + egui::vec2(0.0, 300.0);
        let mut clamped = initial_outer;
        for step in 1..=30 {
            let f = step as f32 / 30.0;
            let pos = corner + (drag_to - corner) * f;
            clamped = run(
                &ctx,
                egui::RawInput {
                    events: vec![egui::Event::PointerMoved(pos)],
                    ..base_input()
                },
            );
        }
        for _ in 0..3 {
            clamped = run(&ctx, base_input());
        }
        assert!(
            clamped.max.y <= screen_rect.max.y,
            "天花板生效后窗口底边 {} 仍然超出屏幕高度 {}(outer={clamped:?})——没有堵住溢出",
            clamped.max.y,
            screen_rect.max.y
        );

        // 阶段 3:松开,从被削过的位置继续往回拖(缩小方向),验证 resize 机制
        // 没有被这次天花板削减弄死——应该还能继续跟手缩小。
        run(
            &ctx,
            egui::RawInput {
                events: vec![egui::Event::PointerButton {
                    pos: drag_to,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::default(),
                }],
                ..base_input()
            },
        );
        let shrink_corner = clamped.right_bottom();
        run(
            &ctx,
            egui::RawInput {
                events: vec![egui::Event::PointerMoved(shrink_corner)],
                ..base_input()
            },
        );
        run(
            &ctx,
            egui::RawInput {
                events: vec![egui::Event::PointerButton {
                    pos: shrink_corner,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::default(),
                }],
                ..base_input()
            },
        );
        let shrink_to = shrink_corner - egui::vec2(0.0, 150.0);
        let mut shrunk = clamped;
        for step in 1..=15 {
            let f = step as f32 / 15.0;
            let pos = shrink_corner + (shrink_to - shrink_corner) * f;
            shrunk = run(
                &ctx,
                egui::RawInput {
                    events: vec![egui::Event::PointerMoved(pos)],
                    ..base_input()
                },
            );
        }
        assert!(
            shrunk.max.y < clamped.max.y - 50.0,
            "被天花板削过之后,继续往小拖应该还能跟手缩小,但削减前底边 {},\
             缩小后底边 {}(resize 疑似被永久钉死)",
            clamped.max.y,
            shrunk.max.y
        );
    }

    /// F94:横向拖 resize 手柄时,**窗口边框/标题栏必须跟着变宽**,内容不许
    /// 画到边框外面去。
    ///
    /// 症状(Windows 11 实机录屏):往右下拖手柄,右栏内容、底部按钮条确实
    /// 跟着变宽了,但窗口的圆角边框、标题栏「会话管理器」那一条、右上角的
    /// 关闭按钮、右下角的 resize 手柄**全部停在原宽度**,内容溢出边框、直接
    /// 画在窗口外的主背景上。
    ///
    /// 根因在 egui 0.30 的两条口径差:
    /// - `Window` 内部 `resize.resizable(false)`(`window.rs:474`,「We resize
    ///   it manually」),于是 `Resize::end`(`resize.rs:317-332`)走的是
    ///   「Probably a window」那条分支——`advance_cursor_after_rect` 用的是
    ///   `last_content_size`(内容**实际**占用),不是 `desired_size`。窗口
    ///   外框 `outer_rect` 由 `frame.end()` 从这个 `min_rect` 算出来。
    /// - 而拖拽结果是写进 `desired_size` 的,它只体现在 content ui 的
    ///   `max_rect` 上。
    ///
    /// 高度方向上两者能对上,是因为 `SidePanel::show_inside_dyn` 有一条
    /// 「只增不减」的棘轮把自己的高度报回外层 `ui`(见 `CONTENT_MIN_HEIGHT`
    /// 文档注释);**宽度方向没有任何对应机制**(这一点
    /// `dragging_the_split_does_not_widen_the_window` 的文档注释里早就写下
    /// 过,只是当时是当作「分隔条撑不宽窗口」的好事来记的)。于是横向上
    /// `min_rect` 一直停在我们 `ui.set_min_width(...)` 给的那个常量地板,
    /// 外框永远 880 宽,而 `CentralPanel` 按 `max_rect` 铺满、画到 1500+。
    ///
    /// 拖的是**右边缘中点**,不是右下角:egui 的 hit-test 在这个无头环境里
    /// 会把角落的拖拽判给覆盖面更大的「bottom」边 widget(已实测:同一次
    /// 按下,`edge_drag/bottom` 的 `dragged()` 为 true,而
    /// `edge_drag/right_bottom` 与 `edge_drag/right` 全是 false),于是只有
    /// 纵向 resize 被驱动,横向那条路径根本走不到。真机上角拖两个方向都生效
    /// (录屏为证),这里换成右边缘只是为了在测试里可靠地驱动横向那条路径。
    ///
    /// 自证会变红:删掉 `show()` 里 `ui.set_min_width(ui.max_rect().width())`
    /// 那一行(把宽度报告回外层的那句),两条断言都会炸——已实测输出:外框右边
    /// 全程钉在 896,而右栏内容右边缘一路涨到 936,内容确实画到了框外。
    #[test]
    fn dragging_the_resize_handle_widens_the_window_frame_not_just_its_contents() {
        let t = crate::theme::MULLION_DARK;
        let sessions: Vec<SessionRecord> = Vec::new();
        let groups: Vec<GroupRecord> = Vec::new();
        let mut ui_state = UiState {
            session_manager_open: true,
            editor: Some(EditorBuffer::default()),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        // 屏幕给得足够宽(1600),横向天花板(`avail_w`)在这几步拖拽里不会触发,
        // 单独考察「跟手」这一件事。
        let screen_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 900.0));
        let base_input = || egui::RawInput {
            screen_rect: Some(screen_rect),
            ..Default::default()
        };
        let mut run = |ctx: &egui::Context, input: egui::RawInput| -> egui::Rect {
            let mut outer = None;
            let _ = ctx.run(input, |ctx| {
                outer = show(
                    ctx,
                    &t,
                    &mut ui_state,
                    &sessions,
                    &groups,
                    true,
                    None,
                    SecretPresence::default(),
                    &crate::ui::badge::AppearanceCache::default(),
                );
            });
            outer.expect("会话管理器窗口应已渲染并返回外层矩形")
        };

        let mut initial_outer = egui::Rect::NAN;
        for _ in 0..3 {
            initial_outer = run(&ctx, base_input());
        }

        let grab = egui::pos2(initial_outer.max.x, initial_outer.center().y);
        run(
            &ctx,
            egui::RawInput {
                events: vec![egui::Event::PointerMoved(grab)],
                ..base_input()
            },
        );
        run(
            &ctx,
            egui::RawInput {
                events: vec![egui::Event::PointerButton {
                    pos: grab,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::default(),
                }],
                ..base_input()
            },
        );

        // 阶段 1:分 3 步、每步往右拖 20px,窗口右边缘要跟手前进。
        let mut prev_right = grab.x;
        let mut outer = initial_outer;
        for step in 1..=3 {
            let pos = grab + egui::vec2(20.0 * step as f32, 0.0);
            outer = run(
                &ctx,
                egui::RawInput {
                    events: vec![egui::Event::PointerMoved(pos)],
                    ..base_input()
                },
            );
            let advanced = outer.max.x - prev_right;
            assert!(
                (advanced - 20.0).abs() < 1.0,
                "第 {step} 步:窗口右边缘应跟随拖拽前进约 20px,实际前进 {advanced}\
                 (前一帧右边缘 {prev_right},这一帧 outer={outer:?})——外框没跟手,\
                 标题栏/关闭按钮/resize 手柄会停在原宽度,内容却画到框外"
            );
            prev_right = outer.max.x;
        }

        // 阶段 2:内容必须留在外框以内 —— 这是录屏里肉眼可见的那个症状本身。
        let editor = ctx
            .read_response(editor_root_id())
            .expect("右栏编辑器根 Ui 应已注册")
            .rect;
        assert!(
            editor.max.x <= outer.max.x + 1.0,
            "右栏内容右边缘 {} 超出了窗口外框右边缘 {}(editor={editor:?}, outer={outer:?})\
             ——内容被画到窗口边框外面",
            editor.max.x,
            outer.max.x
        );
    }

    /// F90:分隔条上限(440)与窗口宽(880)的联立关系不能被改坏。
    ///
    /// 这条测试实际有两层效力,强度不一样,别混为一谈:
    ///
    /// 1. **头两条 `const { assert!(..) }` 是常量联立检查**,这是本测试真正
    ///    的效力所在,改坏 `LIST_MAX_W`/`WINDOW_W`/`LIST_W` 任何一个导致联立
    ///    关系不成立,**编译期**(不用等到跑测试)就会炸。跟
    ///    `mullion-term/src/keymap.rs::max_wheel_reports_is_a_sane_small_number`
    ///    是同一个先例:比较双方全是 `const`,clippy 的
    ///    `assertions_on_constants` 会认为普通 `assert!` 写这种断言是笔误,
    ///    用 `const { .. }` 把它变成编译期检查,不需要 `#[allow]`,而且比运行
    ///    时断言更早炸、检查随 `cargo test` 一样能跑到。代价是 `const` 块里
    ///    不能用 `format!` 风格的插值拼 panic 消息(`{LIST_MAX_W}` 这种),只能
    ///    写静态字符串——常量值本身在源码定义处就看得到,不需要 panic 消息
    ///    复述一遍。
    /// 2. **第三条渲染断言(`window_rect.width() <= screen_rect.width() +
    ///    SLACK`)维持运行时 `assert!`**,它比的不是常量(左边是渲染出的真实
    ///    矩形宽度),是粗粒度回归网,只对「`WINDOW_W` 本身被改得比屏幕还宽」
    ///    这类改动有效——已实测验证:把 `WINDOW_W` 临时改成 2000
    ///    (`LIST_MAX_W` 不动)会让这条断言真的红:
    ///    `窗口被撑得比屏幕还宽:2000 > 900`。
    ///
    /// **本测试不覆盖、也测不出「拖拽分隔条把窗口撑宽」这条路径**——这里的
    /// `RawInput` 全程没有任何指针/拖拽事件,`SidePanel` 因此全程停在
    /// `default_width`(=`LIST_W`=300),从未被拖到过 `LIST_MAX_W`。文档标题
    /// 说的「分隔条拖到上限」只是常量层面的推导前提,不是这条测试实际驱动
    /// 到的运行时状态。
    ///
    /// 即便真的补上拖拽事件把分隔条拖到 440,也复现不出「窗口被撑宽」——
    /// 已用真实指针事件实测确认过:egui 0.30 在这条路径上有双重钳制兜底,
    /// 我们的代码根本插不进去:`egui-0.30.0/src/containers/window.rs:496-501`
    /// 把 `resize.max_size.x` 钳到 `constrain_rect.width()`(屏幕宽度),在
    /// 我们的闭包代码跑之前就生效;`egui-0.30.0/src/containers/panel.rs:243`
    /// 把 `SidePanel` 自己的宽度 `.at_most(available_rect.width())`,天生不会
    /// 把外层撑宽(不同于高度方向那条「只增不减棘轮」,宽度没有对应机制)。
    /// 所以 Task 1 里那段条件式天花板(`if ui.max_rect().width() > avail_w
    /// { ui.set_max_width(avail_w); }`)删不删,对这条测试的结果毫无影响
    /// (已实测确认)——不要指望删它能自证这条测试,自证请改 `LIST_MAX_W`
    /// 到违反联立关系的值(见上面第 1 点)。
    #[test]
    fn dragging_the_split_does_not_widen_the_window() {
        const { assert!(LIST_MAX_W <= WINDOW_W - 400.0 - 24.0) };
        const { assert!(LIST_MIN_W <= LIST_W && LIST_W <= LIST_MAX_W) };

        let t = crate::theme::MULLION_DARK;
        let sessions: Vec<SessionRecord> = Vec::new();
        let groups: Vec<GroupRecord> = Vec::new();
        let mut ui_state = UiState {
            session_manager_open: true,
            editor: Some(EditorBuffer::default()),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        // 屏幕只比最窄窗口宽一点点 —— 拖到上限也不许把窗口顶出屏幕。
        let screen_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 900.0));
        let input = || egui::RawInput {
            screen_rect: Some(screen_rect),
            ..Default::default()
        };
        for _ in 0..2 {
            let _ = ctx.run(input(), |ctx| {
                show(
                    ctx,
                    &t,
                    &mut ui_state,
                    &sessions,
                    &groups,
                    true,
                    None,
                    SecretPresence::default(),
                    &crate::ui::badge::AppearanceCache::default(),
                );
            });
        }
        let mut window_rect = None;
        let _ = ctx.run(input(), |ctx| {
            window_rect = show(
                ctx,
                &t,
                &mut ui_state,
                &sessions,
                &groups,
                true,
                None,
                SecretPresence::default(),
                &crate::ui::badge::AppearanceCache::default(),
            );
        });
        let window_rect = window_rect.expect("会话管理器窗口应该已经画出来了");
        assert!(
            window_rect.width() <= screen_rect.width() + SLACK,
            "窗口被撑得比屏幕还宽:{} > {}",
            window_rect.width(),
            screen_rect.width()
        );
    }

    /// F91:必填项没填齐时,「保存」/「保存并连接」必须点不动 ——
    /// 否则存进去一条连不上的记录,用户还以为存好了。
    ///
    /// 自证变红的方式:把 `editor.rs` 里传给 `super::labeled_button(ui,
    /// super::save_button_id(), "保存", !disable_save, save_tip.as_deref())`
    /// 这一处调用的第四个实参 `!disable_save` 临时改成 `true`(即拆掉「保存」
    /// 按钮的禁用本身),而不是改测试里的 buffer 内容。`ui.add_enabled(
    /// !disable_connect, ...)` 是「保存并连接」按钮的代码,跟 `disable_save`
    /// 无关,改那里自证不了这条测试。
    #[test]
    fn save_buttons_are_disabled_when_required_fields_are_empty() {
        let t = crate::theme::MULLION_DARK;
        let sessions: Vec<SessionRecord> = Vec::new();
        let groups: Vec<GroupRecord> = Vec::new();
        // 名称/主机/用户名全空 —— 正是「新建」刚打开时的样子。
        let mut ui_state = UiState {
            session_manager_open: true,
            editor: Some(EditorBuffer::default()),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let screen_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 900.0));
        let input = || egui::RawInput {
            screen_rect: Some(screen_rect),
            ..Default::default()
        };
        let mut enabled = None;
        for _ in 0..2 {
            let _ = ctx.run(input(), |ctx| {
                show(
                    ctx,
                    &t,
                    &mut ui_state,
                    &sessions,
                    &groups,
                    true,
                    None,
                    SecretPresence::default(),
                    &crate::ui::badge::AppearanceCache::default(),
                );
                enabled = ctx.read_response(save_button_id()).map(|r| r.enabled());
            });
        }
        assert_eq!(enabled, Some(false), "必填项全空时「保存」按钮必须是禁用态");

        // 填齐后必须重新可点。
        if let Some(buf) = ui_state.editor.as_mut() {
            buf.name = "web01".into();
            buf.host = "10.0.0.1".into();
            buf.user = "root".into();
        }
        let mut enabled_after = None;
        for _ in 0..2 {
            let _ = ctx.run(input(), |ctx| {
                show(
                    ctx,
                    &t,
                    &mut ui_state,
                    &sessions,
                    &groups,
                    true,
                    None,
                    SecretPresence::default(),
                    &crate::ui::badge::AppearanceCache::default(),
                );
                enabled_after = ctx.read_response(save_button_id()).map(|r| r.enabled());
            });
        }
        assert_eq!(enabled_after, Some(true), "必填项填齐后「保存」必须可点");
    }

    /// F92:`disable_save` 与 `disable_connect` 是两个刻意不同的语义——保存
    /// 只被「必填未齐」挡(拨测只读表单、不改),保存并连接/测试连接被
    /// 「必填未齐」和「拨测进行中」两个原因都挡。上一条测试只覆盖了
    /// `disable_save` 那一半;这条覆盖 `disable_connect` 独有的那一半:
    /// 必填项已填齐、但 `ProbeState::Running` 时,「测试连接」必须是禁用态,
    /// 而同一时刻「保存」必须仍然可点——这正是两个语义的分水岭。
    ///
    /// 自证变红的方式:把 `editor.rs` 里传给「测试连接」的
    /// `super::labeled_button(ui, super::probe_button_id(), "测试连接",
    /// !disable_connect, ..)` 这一处调用的第四个实参 `!disable_connect`
    /// 临时改成 `true`,而不是改这里的 `probe` 字段。
    #[test]
    fn probe_button_is_disabled_while_probing_even_though_required_fields_are_filled() {
        let t = crate::theme::MULLION_DARK;
        let sessions: Vec<SessionRecord> = Vec::new();
        let groups: Vec<GroupRecord> = Vec::new();
        let buf = EditorBuffer {
            name: "web01".into(),
            host: "10.0.0.1".into(),
            user: "root".into(),
            ..Default::default()
        };
        let mut ui_state = UiState {
            session_manager_open: true,
            editor: Some(buf),
            probe: ProbeState::Running,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let screen_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 900.0));
        let input = || egui::RawInput {
            screen_rect: Some(screen_rect),
            ..Default::default()
        };
        let mut probe_enabled = None;
        let mut save_enabled = None;
        for _ in 0..2 {
            let _ = ctx.run(input(), |ctx| {
                show(
                    ctx,
                    &t,
                    &mut ui_state,
                    &sessions,
                    &groups,
                    true,
                    None,
                    SecretPresence::default(),
                    &crate::ui::badge::AppearanceCache::default(),
                );
                probe_enabled = ctx.read_response(probe_button_id()).map(|r| r.enabled());
                save_enabled = ctx.read_response(save_button_id()).map(|r| r.enabled());
            });
        }
        assert_eq!(
            probe_enabled,
            Some(false),
            "必填项已填齐但拨测在途时,「测试连接」必须是禁用态"
        );
        assert_eq!(
            save_enabled,
            Some(true),
            "拨测在途不该挡「保存」——它只读表单、不改"
        );
    }
}
