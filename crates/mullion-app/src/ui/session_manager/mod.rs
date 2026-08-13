//! 会话管理弹窗:列表 + CRUD + 编辑表单(Task 6,§4.3/§1.2)。
//!
//! 关键约束:这里渲染在 `app.rs` 的 `egui_ctx.run(|ctx| ...)` 闭包内,只能拿到
//! `&mut UiState`,拿不到 `&mut SessionStore`(否则借用检查器过不了)。所以任何会
//! 改 store / 发起连接的动作,这里只写「意图」到 `UiState`,由 `app.rs` 在
//! `render_frame` 返回、借用释放之后统一施加——与既有 `request_disconnect`/
//! `request_quit` 完全同构。

mod buffer;
pub(crate) mod credential_editor;
pub(crate) mod credential_list;
mod dedupe;
mod editor;
mod env_hint;
mod fields;
pub(crate) mod form;
mod highlight;
mod inherit_row;
mod jump_preview;
mod keys;
pub(crate) mod keyscan;
pub(crate) mod list;
mod tab_badge;
mod tags;
pub(crate) mod tunnel_editor;
pub(crate) mod tunnel_list;
pub(crate) mod validate;

pub(crate) use buffer::{
    build_draft, clear_key, connect_string, import_icon_file, import_key_file, is_dirty,
    merge_secret, secret_fields, set_color_target, sync_has_passphrase,
};
pub(crate) use buffer::{AuthKindUi, JumpModeUi, ProxyModeUi};
pub use buffer::{
    CredSourceUi, EditorBuffer, SaveIntent, SecretField, SecretPresence, SwitchTarget,
};
pub use credential_editor::{
    build_credential_draft, credential_secret_fields, import_credential_key_file, in_use_message,
    users_of, CredentialEditorBuffer, CredentialSaveIntent,
};
pub use tunnel_editor::{TunnelEditorBuffer, TunnelKindUi, TunnelSaveIntent};

/// 会话管理器的顶层模式(F116/F118)。三类对象左右栏整体切换,不共用列表 ——
/// 混在一个列表里会让「这一行是什么」需要读图标才知道。
///
/// `Sftp` 档列的是 `protocol == Sftp` 的**会话记录**(D1:数据层不分家,
/// schema 仍是 v7)。放在会话与隧道中间:它跟会话是同一类东西的两种协议,
/// 跟隧道不是。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ManagerMode {
    #[default]
    Sessions,
    Sftp,
    /// F74 共享凭据。排在 SFTP 与隧道之间:它跟会话/SFTP 一样是「登录用的东西」,
    /// 隧道是「连上之后转发什么」,顺序照着用户的心智走一遍。
    Credentials,
    Tunnels,
}

/// 这一档该列哪种协议的会话记录。`Tunnels` 不列会话,故为 `None`。
///
/// **这是分流的唯一真源。** 列表渲染(`list::show`)与键盘顺序
/// (`list::visible_order`)必须都走它 —— 只过滤其中一侧,方向键会把选中态
/// 跳到当前页根本看不见的记录上。
pub(crate) fn protocol_of(mode: ManagerMode) -> Option<mullion_store::Protocol> {
    use mullion_store::Protocol;
    match mode {
        ManagerMode::Sessions => Some(Protocol::Ssh),
        ManagerMode::Sftp => Some(Protocol::Sftp),
        ManagerMode::Credentials | ManagerMode::Tunnels => None,
    }
}

use egui::NumExt as _;
use mullion_store::{GroupId, GroupRecord, SessionRecord, TunnelId, TunnelRecord};

use crate::theme::{self, Theme};
use crate::ui::annotate;

use super::UiState;

/// 第 `i` 个编辑器 Tab 的标题(F100 导出要写「当下在看哪一页」)。
/// 名字只有 `editor::TABS` 一处真源,别在调用方重起一份。
pub(crate) fn tab_title(i: usize) -> &'static str {
    editor::TABS.get(i).copied().unwrap_or("?")
}

/// 设计稿 §3:880×560 单窗,左栏定宽 300。
pub(crate) const WINDOW_W: f32 = 880.0;
pub(crate) const WINDOW_H: f32 = 560.0;
pub(crate) const LIST_W: f32 = 300.0;
/// 左栏能拖到的最窄宽度。= 32px 图标 + 左右各 12px 呼吸。必须**严格小于**
/// `list::ICONS_BELOW`(88),否则纯图标档永远拖不出来。
pub(crate) const LIST_MIN_W: f32 = 56.0;
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
/// F120:SFTP 默认目录 + 书签。加在最后一个下标之后 —— 别的下标(尤其
/// `TAB_AUTH`,见 `editor.rs` 的门控/校验)不能因为插了新页而漂移。
pub(crate) const TAB_SFTP: usize = 4;

/// 这个模式下右栏画哪几页,元素是 `editor::TABS` 的**原始下标**。
///
/// SFTP 节点不画「登录后」(F40~F44 是发 shell 命令与 tmux 附着,对 SFTP
/// 没有落点)。隧道档不走这里 —— 它的右栏根本不是 Tab 编辑器。
///
/// **必须返回原始下标。** `TAB_CONNECT..TAB_APPEARANCE` 是下标常量
/// (`editor.rs` 头上写明这是既有技术债),隐藏中间一页之后若用
/// `TABS.iter().enumerate()` 的位置序号去写 `editor_tab`,第三个画出来的是
/// 「图标」但下标是 2,点下去打开的是「登录后」。
pub(crate) fn visible_tabs(mode: ManagerMode) -> &'static [usize] {
    match mode {
        ManagerMode::Sftp => &[TAB_CONNECT, TAB_AUTH, TAB_SFTP, TAB_APPEARANCE],
        _ => &[
            TAB_CONNECT,
            TAB_AUTH,
            TAB_AUTOMATION,
            TAB_SFTP,
            TAB_APPEARANCE,
        ],
    }
}

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

/// F116 模式条。返回自身矩形供 F100 登记。
///
/// **只写 `manager_mode`,绝不碰任何 editor/baseline 字段**:两个模式各持一套
/// 表单缓冲,在这里顺手清一下就等于「切一次页,另一边填到一半的东西没了」。
/// 守护测试 `switching_manager_mode_does_not_clobber_the_other_editors_dirty_state`
/// 正是从这个函数驱动的。
fn mode_bar(ui: &mut egui::Ui, ui_state: &mut UiState) -> egui::Rect {
    let mut rect = egui::Rect::NOTHING;
    ui.horizontal(|ui| {
        for (mode, label) in [
            (ManagerMode::Sessions, "会话"),
            (ManagerMode::Sftp, "SFTP"),
            (ManagerMode::Credentials, "凭据"),
            (ManagerMode::Tunnels, "隧道"),
        ] {
            let on = ui_state.manager_mode == mode;
            if ui.add(egui::SelectableLabel::new(on, label)).clicked() {
                ui_state.manager_mode = mode;
            }
        }
        rect = ui.min_rect();
    });
    rect
}

/// 删隧道的二次确认。独立小窗:隧道的删除按钮在右栏表单上,把确认内联进表单
/// 会把下面的字段整体顶下去,用户点「取消」后视线还得再找一遍原来的位置。
/// 删凭据的二次确认。理由同 `tunnel_delete_confirm`:删除按钮在右栏表单上,
/// 把确认内联进表单会把下面的字段整体顶下去。
///
/// **能走到这里的凭据一定没人引用** —— 有引用者时按钮是真禁用的
/// (`credential_editor::show`),这个框根本弹不出来。
fn credential_delete_confirm(
    ctx: &egui::Context,
    t: &Theme,
    ui_state: &mut UiState,
    id: mullion_store::CredentialId,
    name: &str,
) {
    egui::Window::new("删除凭据")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            ui.colored_label(theme::c32(t.danger_soft), format!("删除「{name}」?"));
            ui.horizontal(|ui| {
                if ui.button("删除").clicked() {
                    ui_state.credential_delete_request = Some(id);
                    ui_state.pending_credential_delete = None;
                }
                if ui.button("取消").clicked() {
                    ui_state.pending_credential_delete = None;
                }
            });
        });
}

fn tunnel_delete_confirm(
    ctx: &egui::Context,
    t: &Theme,
    ui_state: &mut UiState,
    id: mullion_store::TunnelId,
    title: &str,
) {
    egui::Window::new("删除隧道")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            ui.colored_label(theme::c32(t.danger_soft), format!("删除「{title}」?"));
            ui.horizontal(|ui| {
                if ui.button("删除").clicked() {
                    ui_state.tunnel_delete_request = Some(id);
                    ui_state.pending_tunnel_delete = None;
                }
                if ui.button("取消").clicked() {
                    ui_state.pending_tunnel_delete = None;
                }
            });
        });
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
    credentials: &[mullion_store::CredentialRecord],
    tunnels: &[TunnelRecord],
    tunnel_states: &[(TunnelId, mullion_ssh::tunnel::TunnelState)],
    store_available: bool,
    presence: SecretPresence,
    // `credential_presence`:当前选中**凭据**的密文存在情况(F74)。与
    // `presence` 各自独立 —— 两档各画各的密文框,共用一份会让凭据表单
    // 报着会话的「已设置」。
    credential_presence: SecretPresence,
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
                // 隧道页没有会话列表可走。不判模式的话,站在隧道页按 ↑↓ 会对
                // 一条看不见的会话发 pending_switch —— 会话表单脏着时还会凭空
                // 弹出「有未保存的更改」确认框,用户完全不知道自己动了什么。
                let Some(protocol) = protocol_of(ui_state.manager_mode) else {
                    continue;
                };
                let order = list::visible_order(sessions, groups, &ui_state.search, protocol);
                let forward = action == keys::Action::Next;
                if let Some(id) = keys::step(&order, ui_state.editor_id, forward) {
                    // 走 `pending_switch` 而不是直接换 `editor`:表单脏的时候
                    // 要弹确认,这套机制已经在那条路上了(见本文件下方的消费点)。
                    ui_state.pending_switch = Some(SwitchTarget::Session(id));
                }
            }
            keys::Action::Open => {
                // 隧道档、凭据档压根没有会话可连;会话档与 SFTP 档都能连
                // (SFTP 节点连上开的是独占文件标签,不是终端)。
                //
                // 判据走 `protocol_of`(「这一档列的是哪种协议的会话记录」的
                // 唯一真源),而不是列举模式或写 `!= Tunnels`:列举式在加档时
                // 必然漏 —— 已经漏过两次,一次是 D1 让 SFTP 可连之后这条键盘
                // 路径没跟上(纯键盘用户按 Enter 完全没反应),一次是 F74 加
                // 「凭据」档时 `!= Tunnels` 会把它误判成「可以连」。
                if protocol_of(ui_state.manager_mode).is_some() {
                    if let Some(id) = ui_state.editor_id {
                        ui_state.connect_request = Some(id);
                    }
                }
            }
            keys::Action::Tab(n) => {
                // 隧道档、凭据档的右栏不是 Tab 编辑器,切页没有意义。
                // 判据同上走 `protocol_of`,理由见 `Action::Open` 那段。
                //
                // 这个模式判断**不是**跟下面
                // `editor.is_some()` 重叠的冗余条件,不能因为看起来多余就删掉:
                // `mode_bar`(上面)切模式时只写 `ui_state.manager_mode`,
                // 刻意不碰 `editor`/`editor_baseline`(保留另一档表单的脏状态,
                // 见 `mode_bar` 文档注释)。全文件搜 `ui_state.editor = ` 只有
                // `apply_switch` 里两处赋 `Some(...)`,没有任何地方把它清成
                // `None`。所以在会话页开着编辑器、切到隧道档之后,
                // `ui_state.editor` 依然是 `Some`——没有这个模式判断,站在
                // 隧道档按 Ctrl+3 会悄悄改掉背后那个看不见的会话编辑器的
                // `editor_tab`。跟 Task 4 修的是同一类漏洞。
                //
                // SFTP 档少一页(D5),`n` 是**位置序号**不是 Tab 下标,
                // 必须过 `visible_tabs` 映射 —— 直接写 `editor_tab = n`
                // 会让 Ctrl+3 打开「登录后」而 Tab 条上高亮的是「图标」。
                if protocol_of(ui_state.manager_mode).is_some() && ui_state.editor.is_some() {
                    if let Some(&tab) = visible_tabs(ui_state.manager_mode).get(n) {
                        ui_state.editor_tab = tab;
                    }
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

            // F100:整个内容区。**先标外层再标内层**只是可读性,hit test 与登记
            // 顺序无关(取面积最小者)。
            annotate::mark(ui.ctx(), "会话管理器", ui.max_rect());

            // F116 模式条:会话 / 隧道。放在双栏**之上**,因为它切的是
            // 左右栏整体,画在左栏里会让人以为只换列表。
            let mode_rect = mode_bar(ui, ui_state);
            annotate::mark(ui.ctx(), "会话管理器/模式条", mode_rect);
            ui.add_space(6.0);

            let list_panel = egui::SidePanel::left(ui.id().with("sm_list"))
                .resizable(true)
                .default_width(LIST_W)
                .width_range(LIST_MIN_W..=LIST_MAX_W)
                .frame(
                    egui::Frame::none()
                        .fill(theme::c32(t.panel_bg))
                        .inner_margin(14.0),
                )
                .show_inside(ui, |ui| match ui_state.manager_mode {
                    ManagerMode::Sessions | ManagerMode::Sftp => list::show(
                        ui,
                        t,
                        ui_state,
                        sessions,
                        groups,
                        credentials,
                        tunnels,
                        tunnel_states,
                        appearance,
                        // 上面 `match` 的两个分支都保证 `protocol_of` 有值。
                        protocol_of(ui_state.manager_mode).expect("会话/SFTP 档必有协议"),
                    ),
                    ManagerMode::Credentials => {
                        credential_list::show(ui, t, ui_state, credentials, sessions)
                    }
                    ManagerMode::Tunnels => tunnel_list::show(
                        ui,
                        t,
                        ui_state,
                        tunnels,
                        tunnel_states,
                        sessions,
                        credentials,
                    ),
                });
            annotate::mark(ui.ctx(), "会话管理器/左栏", list_panel.response.rect);

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
                    annotate::mark(ui.ctx(), "会话管理器/右栏", rect);
                    // 借用冲突:`ui_state` 下面要整个可变借给 `editor::show`,
                    // 不能在同一次调用表达式里再读 `ui_state.manager_mode`——
                    // 先拷出来(`ManagerMode` 是 `Copy`),再传这份拷贝。
                    let mode = ui_state.manager_mode;
                    match mode {
                        ManagerMode::Sessions | ManagerMode::Sftp => editor::show(
                            ui,
                            t,
                            ui_state,
                            groups,
                            sessions,
                            credentials,
                            presence,
                            mode,
                        ),
                        ManagerMode::Credentials => {
                            credential_editor::show(ui, t, ui_state, sessions, credential_presence)
                        }
                        ManagerMode::Tunnels => {
                            tunnel_editor::show(ui, t, ui_state, sessions, credentials)
                        }
                    }
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

    // 隧道侧的同一套:`tunnel_editor::show` 正持着 `tunnel_editor` 的 `&mut`,
    // 那里只置一个标志,`build_tunnel_draft` 要整体读缓冲,所以挪到这里。
    if std::mem::take(&mut ui_state.tunnel_save_click) {
        if let Some(buf) = ui_state.tunnel_editor.as_ref() {
            match tunnel_editor::build_tunnel_draft(buf) {
                Ok(draft) => {
                    ui_state.tunnel_save_request = Some(TunnelSaveIntent {
                        editing_id: ui_state.tunnel_editor_id,
                        draft,
                    });
                }
                Err(msg) => ui_state.set_error(msg),
            }
        }
    }
    // 删隧道的二次确认。会话侧的确认框内联在列表行下面(那里一行就是一个
    // 会话),隧道侧的删除按钮在右栏表单上,确认也就画在表单区域 —— 见
    // `tunnel_delete_confirm`。
    if let Some(id) = ui_state.pending_tunnel_delete {
        let title = tunnels
            .iter()
            .find(|x| x.id == id)
            .map(tunnel_list::row_title);
        match title {
            Some(title) => tunnel_delete_confirm(ctx, t, ui_state, id, &title),
            // 目标已经不在库里(别处删掉了)→ 撤掉挂着的确认,
            // 不能留一个指向空气的确认框等用户去点。
            None => ui_state.pending_tunnel_delete = None,
        }
    }

    // 凭据侧的同一套。`credential_editor::show` 正持着 `credential_editor`
    // 的 `&mut`,那里只置标志,`build_credential_draft` 要整体读缓冲。
    if std::mem::take(&mut ui_state.credential_save_click) {
        if let Some(buf) = ui_state.credential_editor.as_ref() {
            match credential_editor::build_credential_draft(buf) {
                Ok(draft) => {
                    let (password, passphrase, private_key) =
                        credential_editor::credential_secret_fields(buf);
                    ui_state.credential_save_request = Some(CredentialSaveIntent {
                        editing_id: ui_state.credential_editor_id,
                        draft,
                        password,
                        passphrase,
                        private_key,
                    });
                    // 保存成功后基线要跟上,否则刚存完就被判成脏。
                    ui_state.credential_editor_baseline = ui_state.credential_editor.clone();
                }
                Err(msg) => ui_state.set_error(msg),
            }
        }
    }
    if let Some(id) = ui_state.pending_credential_delete {
        let name = credentials
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.name.clone());
        match name {
            Some(name) => credential_delete_confirm(ctx, t, ui_state, id, &name),
            // 目标已经不在库里(别处删掉了)→ 撤掉挂着的确认,不能留一个
            // 指向空气的确认框等用户去点。
            None => ui_state.pending_credential_delete = None,
        }
    }
    // 凭据表单里导入/清除私钥留下的一行提示 → 转成编辑器顶部那条通知。
    // 与会话侧同一处置(瞬态,留在缓冲里会把表单误判成脏)。
    if let Some(buf) = ui_state.credential_editor.as_mut() {
        if let Some(note) = buf.key_note.take() {
            ui_state.key_drop_note = Some(note);
        }
        if std::mem::take(&mut buf.pick_key_clicked) {
            ui_state.pick_credential_key_request = true;
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

    // D4 统一闸门。隧道档压根没有会话可连;SFTP 节点(D1 起)是可以连的 ——
    // 连上开的是独占的文件标签,不是终端(`App::spawn_connect` 的
    // `wants_sftp` 分支)。入口有四条(左栏双击、右键菜单、右栏按钮、
    // Enter),逐条挡必然漏——这里做唯一一道兜底,各入口再各自置灰
    // (那是给人看的,这一道是保证行为)。
    // `connect_skip_automation` 必须一起清:留着它会漂到下一次真正的连接上,
    // 用户会莫名其妙地跳过一次自动化。
    if protocol_of(ui_state.manager_mode).is_none() {
        ui_state.connect_request = None;
        ui_state.connect_skip_automation = false;
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
            let mut draft = EditorBuffer::new_draft();
            // D1/D3:在哪一档按「+ 新建」,建出来的就是哪一类节点。协议此后
            // 只读,所以这是它唯一的决定点。
            if let Some(p) = protocol_of(ui_state.manager_mode) {
                draft.protocol = p;
            }
            ui_state.editor = Some(draft);
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
mod tunnel_ui_tests {
    use super::*;
    use mullion_store::{SessionId, TunnelId, TunnelKind, TunnelRecord};

    fn sess(id: u64, name: &str) -> SessionRecord {
        use mullion_store::model::{Auth, AuthKind, Connection, Identity, Protocol};
        SessionRecord {
            id: SessionId(id),
            modified_at: "t".into(),
            identity: Identity {
                name: name.into(),
                note: String::new(),
                group_id: None,
                tags: Vec::new(),
            },
            connection: Connection {
                host: format!("10.0.0.{id}"),
                port: 22,
                protocol: Protocol::Ssh,
            },
            auth: Auth::inline("root", AuthKind::Password),
            terminal: Default::default(),
            appearance: Default::default(),
            network: Default::default(),
            automation: Default::default(),
            sftp: Default::default(),
        }
    }

    fn sftp_sess(id: u64, name: &str) -> SessionRecord {
        let mut s = sess(id, name);
        s.connection.protocol = mullion_store::Protocol::Sftp;
        s
    }

    fn tun(id: u64, session: u64, kind: TunnelKind) -> TunnelRecord {
        TunnelRecord {
            id: TunnelId(id),
            session_id: SessionId(session),
            listen_port: 3306,
            note: String::new(),
            autostart: false,
            kind,
        }
    }

    fn local() -> TunnelKind {
        TunnelKind::Local {
            target_host: "db.internal".into(),
            target_port: 3306,
            expose: false,
        }
    }

    /// 跑两帧(第一帧让布局稳定),返回第二帧输出。
    fn run(
        ui_state: &mut UiState,
        sessions: &[SessionRecord],
        tunnels: &[TunnelRecord],
    ) -> (egui::Context, egui::FullOutput) {
        run_with_credentials(ui_state, sessions, tunnels, &[])
    }

    /// 同上,但能喂一份凭据库(F74 的「凭据」档要用)。
    fn run_with_credentials(
        ui_state: &mut UiState,
        sessions: &[SessionRecord],
        tunnels: &[TunnelRecord],
        credentials: &[mullion_store::CredentialRecord],
    ) -> (egui::Context, egui::FullOutput) {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let once = |st: &mut UiState| {
            ctx.run(Default::default(), |ctx| {
                show(
                    ctx,
                    &t,
                    st,
                    sessions,
                    &[],
                    credentials,
                    tunnels,
                    &[],
                    true,
                    SecretPresence::default(),
                    SecretPresence::default(),
                    &crate::ui::badge::AppearanceCache::default(),
                );
            })
        };
        once(ui_state);
        let out = once(ui_state);
        (ctx, out)
    }

    fn all_text(shapes: &[egui::epaint::ClippedShape]) -> Vec<String> {
        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                egui::Shape::Text(ts) => out.push(ts.galley.text().to_string()),
                _ => {}
            }
        }
        let mut acc = Vec::new();
        shapes.iter().for_each(|cs| walk(&cs.shape, &mut acc));
        acc
    }

    fn has(texts: &[String], needle: &str) -> bool {
        texts.iter().any(|s| s.contains(needle))
    }

    fn open(mode: ManagerMode) -> UiState {
        UiState {
            session_manager_open: true,
            manager_mode: mode,
            ..Default::default()
        }
    }

    /// 分流的唯一真源。列表渲染和键盘顺序都走它,两边分叉就会出现
    /// 「按了下箭头,右栏换了一条陌生会话,左栏却没有任何行高亮」。
    #[test]
    fn each_mode_maps_to_exactly_one_protocol_or_none() {
        use mullion_store::Protocol;
        assert_eq!(protocol_of(ManagerMode::Sessions), Some(Protocol::Ssh));
        assert_eq!(protocol_of(ManagerMode::Sftp), Some(Protocol::Sftp));
        assert_eq!(
            protocol_of(ManagerMode::Tunnels),
            None,
            "隧道档不列会话,必须是 None——给它编一个协议会让键盘顺序走进会话列表"
        );
    }

    /// SFTP 档列 SFTP 节点,会话档列 SSH 会话,互不串台。
    ///
    /// 自证会变红:把 `list::show`/`visible_order` 里的协议 filter 去掉,
    /// 两个方向的断言各红一条。
    #[test]
    fn each_page_lists_only_its_own_protocol() {
        let sessions = vec![sess(1, "生产主控"), sftp_sess(2, "文件中转")];

        let (_c, out) = run(&mut open(ManagerMode::Sessions), &sessions, &[]);
        let texts = all_text(&out.shapes);
        assert!(has(&texts, "生产主控"), "会话页没画 SSH 会话: {texts:?}");
        assert!(
            !has(&texts, "文件中转"),
            "会话页混进了 SFTP 节点: {texts:?}"
        );

        let (_c, out) = run(&mut open(ManagerMode::Sftp), &sessions, &[]);
        let texts = all_text(&out.shapes);
        assert!(has(&texts, "文件中转"), "SFTP 页没画 SFTP 节点: {texts:?}");
        assert!(
            !has(&texts, "生产主控"),
            "SFTP 页混进了 SSH 会话: {texts:?}"
        );
    }

    /// 键盘顺序与渲染必须是**同一份**集合。这条钉的是「按 ↓ 跳到看不见的行」
    /// ——症状是右栏表单换了一条陌生会话,左栏却没有任何一行高亮。
    ///
    /// 自证会变红:只给 `list::show` 加 filter、不给 `visible_order` 加。
    #[test]
    fn keyboard_order_never_contains_a_row_the_page_does_not_render() {
        use mullion_store::Protocol;
        let sessions = vec![sess(1, "生产主控"), sftp_sess(2, "文件中转")];
        let order = list::visible_order(&sessions, &[], "", Protocol::Ssh);
        assert_eq!(
            order,
            vec![SessionId(1)],
            "SSH 页的键盘顺序里不该有 SFTP 节点"
        );
        let order = list::visible_order(&sessions, &[], "", Protocol::Sftp);
        assert_eq!(order, vec![SessionId(2)]);
    }

    /// SFTP 节点不画「登录后」:那一页是发 shell 命令与 tmux 附着(F40~F44),
    /// 对 SFTP 没有落点,画出来等于让用户配一堆永远不会执行的东西。
    ///
    /// 返回的是 **`TABS` 的原始下标**而不是重新编号。`TAB_*` 是下标常量:
    /// 用 `enumerate()` 的位置序号去写 `editor_tab`,点「图标」会打开「登录后」。
    #[test]
    fn sftp_hides_the_automation_tab_and_keeps_original_indices() {
        assert_eq!(
            visible_tabs(ManagerMode::Sftp),
            &[TAB_CONNECT, TAB_AUTH, TAB_SFTP, TAB_APPEARANCE],
            "SFTP 档必须是这四页,且用的是原始下标"
        );
        assert_eq!(
            visible_tabs(ManagerMode::Sessions),
            &[
                TAB_CONNECT,
                TAB_AUTH,
                TAB_AUTOMATION,
                TAB_SFTP,
                TAB_APPEARANCE
            ]
        );
    }

    /// D5 防漂移:必填项只落在「连接」「认证」两页,两页在 SFTP 档都在。
    /// 将来有人给「登录后」加必填项时,这条会先红 —— 那时才需要处理
    /// 「校验要把用户导向一页看不见的 Tab」。
    #[test]
    fn every_reachable_validation_target_is_visible_in_sftp_mode() {
        let all_missing = super::validate::Missing {
            name: true,
            host: true,
            user: true,
            credential: true,
        };
        let visible = visible_tabs(ManagerMode::Sftp);
        for m in [
            all_missing,
            super::validate::Missing {
                name: false,
                host: false,
                user: true,
                credential: false,
            },
            // F74:共享凭据没挑 —— 缺项落在「认证」页,SFTP 档也看得见。
            super::validate::Missing {
                name: false,
                host: false,
                user: false,
                credential: true,
            },
        ] {
            if let Some(tab) = m.tab() {
                assert!(
                    visible.contains(&tab),
                    "校验会把用户导向 SFTP 档看不见的 Tab {tab}"
                );
            }
        }
    }

    /// D3:协议只读。可改的话,一条记录会在保存那一刻从当前列表消失、跑到
    /// 另一页去(用户看到的是「我点了保存,会话没了」),而引用它的隧道会
    /// 当场变成「经由一个 SFTP 节点」。要换协议就新建一条。
    ///
    /// 光看「画面上没出现『sftp(未实现)』」测不出什么 —— `ComboBox` 关着的
    /// 时候候选项本来就不画,这条断言在协议还是下拉的旧实现下也会恒绿。
    /// 所以这里**真的点一下协议控件**:旧实现是 `ComboBox`,点它的按钮部分
    /// 就会在下一帧弹出候选(`sftp(未实现)` 会出现);新实现是纯
    /// `ui.label`,点哪里都不会有任何响应。
    ///
    /// 只跑了 `ManagerMode::Sessions` 一种场景,名字不承诺覆盖 SFTP 档:
    /// `fields::basic` 根本不接收 `ManagerMode`,协议只读对两个模式是同一份
    /// 代码同一条路径,SFTP 场景不会带来新分支,补一组只是重复跑一遍同样的
    /// 断言,不构成新的覆盖。
    #[test]
    fn the_protocol_row_is_plain_text_with_no_dropdown_to_open() {
        let sessions = vec![sess(1, "生产主控")];
        let mut st = open(ManagerMode::Sessions);
        st.editor_id = Some(SessionId(1));
        st.editor = Some(EditorBuffer::from_record(&sessions[0]));
        st.editor_baseline = st.editor.clone();
        let (ctx, out) = run(&mut st, &sessions, &[]);
        let texts = all_text(&out.shapes);
        // 精确匹配,不用 `has`(子串):"ssh" 这么短的串很容易被别的文案
        // (主机名、`ssh-rsa`、`~/.ssh/config` 之类)子串误命中。协议这一行
        // 画的就是单独一个 `ssh` 文本节点,精确相等才对得上「协议值显示出来
        // 了」这件事。
        assert!(
            texts.iter().any(|s| s == "ssh"),
            "协议值该显示出来: {texts:?}"
        );

        let pos = find_text_pos(&out.shapes, "ssh").expect("协议值「ssh」没画出来");
        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Default::default(),
        });
        input.events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        });
        let t = crate::theme::MULLION_DARK;
        let mut draw = |input: egui::RawInput| {
            ctx.run(input, |c| {
                show(
                    c,
                    &t,
                    &mut st,
                    &sessions,
                    &[],
                    &[],
                    &[],
                    &[],
                    true,
                    SecretPresence::default(),
                    SecretPresence::default(),
                    &crate::ui::badge::AppearanceCache::default(),
                );
            })
        };
        let _ = draw(input);
        // `ComboBox` 点击后要再多跑一帧,弹出的候选才会画出来(同
        // `tri_state_combo_keeps_on_off_labels_and_written_values_paired_not_
        // swapped` 里的手法:点击那一帧只切换打开状态,候选内容在下一帧才
        // 真正画出来)。少这一帧,这条断言无论协议是不是只读都会恒绿。
        let clicked_out = draw(egui::RawInput::default());
        let texts_after_click = all_text(&clicked_out.shapes);
        assert!(
            !has(&texts_after_click, "sftp(未实现)"),
            "点一下协议控件就弹出了候选项,说明它还是个能改的下拉: {texts_after_click:?}"
        );
        assert_eq!(
            st.editor.as_ref().map(|b| b.protocol),
            Some(mullion_store::Protocol::Ssh),
            "点一下协议控件,值不该变"
        );
    }

    /// 在哪一档按「+ 新建」,建出来的就是哪一类节点。协议此后只读(D3),
    /// 所以这是它唯一的决定点 —— 漏了这里,SFTP 页新建的节点会是 ssh,
    /// 保存后当场从 SFTP 页消失。
    #[test]
    fn new_draft_takes_its_protocol_from_the_current_mode() {
        use mullion_store::Protocol;
        for (mode, want) in [
            (ManagerMode::Sessions, Protocol::Ssh),
            (ManagerMode::Sftp, Protocol::Sftp),
        ] {
            let mut st = open(mode);
            st.pending_switch = Some(SwitchTarget::NewDraft);
            apply_switch(&mut st, &[]);
            assert_eq!(
                st.editor.as_ref().map(|b| b.protocol),
                Some(want),
                "{mode:?} 档新建的草稿协议不对"
            );
        }
    }

    /// D5 的下标陷阱。SFTP 档 Tab 条上第四个是「图标」(F120 加了「SFTP」页后
    /// 从第三个挪到第四个),点它必须打开「图标」。
    ///
    /// 自证会变红:把 `editor.rs` 的 Tab 循环改回 `TABS.iter().enumerate()`,
    /// 这条立刻红(`editor_tab` 会变成 2 = 登录后)。
    #[test]
    fn clicking_the_fourth_tab_in_sftp_mode_opens_appearance_not_automation() {
        let sessions = vec![sftp_sess(1, "文件中转")];
        let mut st = open(ManagerMode::Sftp);
        st.editor_id = Some(SessionId(1));
        st.editor = Some(EditorBuffer::from_record(&sessions[0]));
        st.editor_baseline = st.editor.clone();

        // 先跑两帧把 Tab 画出来,再用真实指针事件点「图标」那一格
        // (位置从渲染出的文字锚点反查)。
        let (ctx, out) = run(&mut st, &sessions, &[]);
        let pos = find_text_pos(&out.shapes, "图标").expect("Tab「图标」没画出来");
        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Default::default(),
        });
        input.events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        });
        let t = crate::theme::MULLION_DARK;
        let _ = ctx.run(input, |c| {
            show(
                c,
                &t,
                &mut st,
                &sessions,
                &[],
                &[],
                &[],
                &[],
                true,
                SecretPresence::default(),
                SecretPresence::default(),
                &crate::ui::badge::AppearanceCache::default(),
            );
        });

        assert_eq!(
            st.editor_tab, TAB_APPEARANCE,
            "点 SFTP 档第三个 Tab「图标」,打开的却是下标 {} 那一页",
            st.editor_tab
        );
    }

    /// 反查一段文字画在哪儿,用来给点击事件定位。**没有用 `ctx.graphics(...)`**
    /// (egui-0.30.0 的 `GraphicLayers` 没有公开的 `iter()`,编译不过)——改用
    /// `run()` 已经拿到的 `FullOutput::shapes`,同 `editor.rs::tests` 里
    /// `find_text_pos_exact` 的做法(那边按**完整相等**,这里按**子串**——
    /// Tab 标题后面可能跟着 F91 的缺项徽标,不能要求完全相等)。
    fn find_text_pos(shapes: &[egui::epaint::ClippedShape], needle: &str) -> Option<egui::Pos2> {
        fn walk(shape: &egui::Shape, needle: &str) -> Option<egui::Pos2> {
            match shape {
                egui::Shape::Vec(v) => v.iter().find_map(|s| walk(s, needle)),
                egui::Shape::Text(ts) if ts.galley.text().contains(needle) => {
                    Some(ts.pos + ts.galley.size() / 2.0)
                }
                _ => None,
            }
        }
        shapes.iter().find_map(|cs| walk(&cs.shape, needle))
    }

    /// 切到隧道页,左栏画的必须是隧道,不是会话。
    ///
    /// 自证会变红:把 `show()` 里 `SidePanel` 的 `match manager_mode` 改回
    /// 无条件 `list::show`,这条立刻炸。
    #[test]
    fn tunnel_mode_renders_tunnel_list_not_session_list() {
        let sessions = vec![sess(1, "生产主控")];
        let tunnels = vec![tun(1, 1, local())];

        let (_c, out) = run(&mut open(ManagerMode::Tunnels), &sessions, &tunnels);
        let texts = all_text(&out.shapes);
        assert!(has(&texts, "本地 3306"), "隧道行没画出来: {texts:?}");
        assert!(
            !has(&texts, "生产主控") || has(&texts, "经 生产主控"),
            "隧道页不该画会话列表行(只允许作为「经由」出现): {texts:?}"
        );

        // 反面:会话模式下画的是会话,不是隧道。
        let (_c, out) = run(&mut open(ManagerMode::Sessions), &sessions, &tunnels);
        let texts = all_text(&out.shapes);
        assert!(has(&texts, "生产主控"), "会话行没画出来: {texts:?}");
        assert!(!has(&texts, "本地 3306"), "会话页不该画隧道行: {texts:?}");
    }

    fn cred(id: u64, name: &str, user: &str) -> mullion_store::CredentialRecord {
        mullion_store::CredentialRecord {
            id: mullion_store::CredentialId(id),
            name: name.into(),
            user: user.into(),
            kind: mullion_store::AuthKind::Password,
        }
    }

    /// F74:模式条四档,顺序定死「会话 · SFTP · 凭据 · 隧道」。
    ///
    /// 顺序不是审美问题:用户是靠位置记按钮的,插一档进来把「隧道」挤走,
    /// 每个既有用户的肌肉记忆都得重学一遍。这条钉住位置,将来再加档时
    /// 会先变红,逼人显式决定插在哪。
    ///
    /// 判据用**画出来的 x 坐标**,不是数组字面量 —— 后者是拿常量断言常量。
    #[test]
    fn the_mode_bar_has_four_tabs_in_a_fixed_order() {
        let (_c, out) = run(&mut open(ManagerMode::Sessions), &[], &[]);
        let xs: Vec<f32> = ["会话", "SFTP", "凭据", "隧道"]
            .iter()
            .map(|label| {
                find_text_pos(&out.shapes, label)
                    .unwrap_or_else(|| panic!("模式条上没有「{label}」这一档"))
                    .x
            })
            .collect();
        assert!(
            xs.windows(2).all(|w| w[0] < w[1]),
            "四档必须按「会话·SFTP·凭据·隧道」从左到右排;实得 x={xs:?}"
        );
    }

    /// F74:切到「凭据」档,左栏画的是凭据,不是会话。
    #[test]
    fn credential_mode_renders_the_credential_list() {
        let sessions = vec![sess(1, "生产主控")];
        let creds = vec![cred(1, "运维号", "ops")];

        let (_c, out) =
            run_with_credentials(&mut open(ManagerMode::Credentials), &sessions, &[], &creds);
        let texts = all_text(&out.shapes);
        assert!(has(&texts, "运维号"), "凭据行没画出来: {texts:?}");
        assert!(
            !has(&texts, "生产主控"),
            "凭据页不该画会话列表行: {texts:?}"
        );
    }

    /// F74/D7:被引用的凭据,删除按钮**当场**禁用并把引用者的名字列出来。
    ///
    /// 只说「有会话在用」等于让用户挨个点开会话去找是谁 —— 而「先去解绑谁」
    /// 正是他接下来唯一要做的事。
    ///
    /// 自证变红的方式:把 `credential_editor::show` 里那句
    /// `ui.colored_label(theme::c32(t.danger), msg)` 删掉。
    #[test]
    fn deleting_a_referenced_credential_is_blocked_and_names_the_holders() {
        use mullion_store::{Auth, CredentialId};
        let mut web = sess(1, "web01");
        web.auth = Auth::Ref(CredentialId(1));
        let creds = vec![cred(1, "运维号", "ops")];

        let mut st = open(ManagerMode::Credentials);
        st.credential_editor_id = Some(CredentialId(1));
        st.credential_editor = Some(CredentialEditorBuffer::from_record(&creds[0]));

        let (_c, out) = run_with_credentials(&mut st, &[web], &[], &creds);
        let texts = all_text(&out.shapes);
        assert!(
            texts.iter().any(|s| s.contains("web01")),
            "拦下来时必须点名是谁在用;实际画出:{texts:?}"
        );
        assert!(st.pending_credential_delete.is_none(), "光渲染不该发起删除");
    }

    /// F74:站在「凭据」档按 ↑↓ / Ctrl+2,不许去动背后那个看不见的会话编辑器。
    ///
    /// 切模式时刻意不清 `editor`/`editor_baseline`(保留另一档的脏状态,见
    /// `mode_bar` 文档),所以在凭据档上这两个字段依然是 `Some` —— 少了模式
    /// 门控,按一下方向键就会对一条用户完全看不见的会话发切换意图,表单脏时
    /// 还会凭空弹出「有未保存的更改」。这与隧道档是同一类漏洞,判据统一走
    /// `protocol_of`。
    ///
    /// 自证变红的方式:把 `Action::Tab(n)` 与 `Action::Prev|Next` 两处的
    /// `protocol_of(...)` 判断去掉。
    #[test]
    fn credential_mode_swallows_list_and_tab_shortcuts() {
        let t = crate::theme::MULLION_DARK;
        let sessions = vec![sess(1, "a"), sess(2, "b")];
        let key = |k: egui::Key, modifiers: egui::Modifiers| egui::Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        let drive = |st: &mut UiState, events: Vec<egui::Event>, modifiers| {
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
                            st,
                            &sessions,
                            &[],
                            &[],
                            &[],
                            &[],
                            true,
                            SecretPresence::default(),
                            SecretPresence::default(),
                            &crate::ui::badge::AppearanceCache::default(),
                        );
                    },
                );
            };
            run(Vec::new());
            run(events);
        };

        let cmd = egui::Modifiers {
            command: true,
            ..Default::default()
        };
        let mut st = open(ManagerMode::Credentials);
        st.editor = Some(EditorBuffer::default());
        st.editor_id = Some(SessionId(1));
        st.editor_tab = 0;

        drive(&mut st, vec![key(egui::Key::Num2, cmd)], cmd);
        assert_eq!(st.editor_tab, 0, "凭据档下 Ctrl+2 不该动会话编辑器的 Tab");

        drive(
            &mut st,
            vec![key(egui::Key::ArrowDown, egui::Modifiers::default())],
            egui::Modifiers::default(),
        );
        assert_eq!(
            st.editor_id,
            Some(SessionId(1)),
            "凭据档下 ↓ 不该切走背后那条看不见的会话"
        );
        assert!(!st.confirm_switch, "更不该凭空弹出「有未保存的更改」");
    }

    /// F100:新 UI 元素必须登记,否则标注模式导不出它 —— 用户就是靠导出的
    /// 标注来提需求的,漏登记等于这块界面在对话里没有名字。
    #[test]
    fn mode_bar_is_annotated_for_f100() {
        let t = crate::theme::MULLION_DARK;
        let sessions = vec![sess(1, "a")];
        let mut st = open(ManagerMode::Sessions);
        let ctx = egui::Context::default();
        // `mark` 在模式关着时是空操作(只读一个 bool),所以必须先打开。
        assert!(crate::ui::annotate::toggle(&ctx), "标注模式没打开,后面白测");
        for _ in 0..2 {
            let _ = ctx.run(Default::default(), |ctx| {
                show(
                    ctx,
                    &t,
                    &mut st,
                    &sessions,
                    &[],
                    &[],
                    &[],
                    &[],
                    true,
                    SecretPresence::default(),
                    SecretPresence::default(),
                    &crate::ui::badge::AppearanceCache::default(),
                );
            });
        }
        let paths = crate::ui::annotate::spot_paths(&ctx);
        assert!(
            paths.iter().any(|p| p == "会话管理器/模式条"),
            "模式条没登记进 F100,实际有: {paths:?}"
        );
    }

    /// 切模式**只能**换页,不许碰另一边的表单缓冲。
    ///
    /// 症状很隐蔽:会话表单填到一半 → 好奇点一下「隧道」→ 切回来,刚才打的
    /// 字没了,而且没有任何提示。两套 editor/baseline 共用一个清空点就会这样。
    #[test]
    fn switching_manager_mode_does_not_clobber_the_other_editors_dirty_state() {
        let sessions = vec![sess(1, "a")];
        let mut st = open(ManagerMode::Sessions);
        // 会话表单编辑到一半:缓冲与基线不同 = 脏。
        let buf = EditorBuffer {
            name: "填了一半".into(),
            ..Default::default()
        };
        st.editor_id = Some(SessionId(1));
        st.editor_baseline = Some(EditorBuffer::default());
        st.editor = Some(buf.clone());

        st.manager_mode = ManagerMode::Tunnels;
        let _ = run(&mut st, &sessions, &[]);
        st.manager_mode = ManagerMode::Sessions;
        let _ = run(&mut st, &sessions, &[]);

        assert_eq!(
            st.editor.as_ref().map(|b| b.name.clone()),
            Some("填了一半".to_string()),
            "切一趟隧道页回来,会话表单的未保存改动被丢掉了"
        );
        assert_eq!(
            st.editor_baseline,
            Some(EditorBuffer::default()),
            "基线也必须原样保留,否则脏检查会误判成「没改过」"
        );
    }

    /// 「能不能点」必须**真的**体现在 `Response::enabled` 上,不能只是画成灰的。
    /// 这条同时钉两头:三种转发都实现了 → 都能点;引用悬垂 → 真禁用。
    ///
    /// 它是 T-a 那条 `start_button_is_really_disabled_not_just_greyed` 与 T-b 那条
    /// `local_start_button_is_clickable_and_unimplemented_kinds_stay_disabled` 的
    /// 接班 —— 前两条守的前提(「转发还没实现」/「只有 `-L` 实现了」)都已经
    /// 不成立,但「按钮该不该能点」这件事必须一直有测试钉着。
    ///
    /// **必须在 `ctx.run` 的闭包内部读 response。** `run` 返回之后
    /// `read_response` 落到 `prev_pass`,拿到的是更早的一帧;而首帧
    /// 布局还没量出底部面板高度,整个滚动区不可见 → egui 把那层 `Ui`
    /// 记成 `enabled=false`。在外面读,「本地转发能点」这半条即使
    /// 实现正确也照样红、「远端禁用」那半条即使删掉 `ui.disable()`
    /// 也照样绿 —— 已实测确认过。
    #[test]
    fn every_kind_is_clickable_and_only_dangling_references_stay_disabled() {
        fn enabled_of(tunnels: &[TunnelRecord], id: TunnelId) -> bool {
            let t = crate::theme::MULLION_DARK;
            let sessions = vec![sess(1, "a")];
            let mut st = open(ManagerMode::Tunnels);
            let ctx = egui::Context::default();
            let mut seen: Option<egui::Response> = None;
            for _ in 0..3 {
                let _ = ctx.run(Default::default(), |c| {
                    show(
                        c,
                        &t,
                        &mut st,
                        &sessions,
                        &[],
                        &[],
                        tunnels,
                        &[],
                        true,
                        SecretPresence::default(),
                        SecretPresence::default(),
                        &crate::ui::badge::AppearanceCache::default(),
                    );
                    seen = c.read_response(tunnel_list::start_button_id(id));
                });
            }
            seen.expect("启动按钮该被渲染出来").enabled
        }

        for (id, kind) in [
            (1u64, local()),
            (
                2,
                TunnelKind::Remote {
                    target_host: "127.0.0.1".into(),
                    target_port: 3000,
                    expose: false,
                },
            ),
            (3, TunnelKind::Dynamic),
        ] {
            assert!(
                enabled_of(&[tun(id, 1, kind)], TunnelId(id)),
                "三种转发都实现了,启动按钮都必须可点(隧道 {id})"
            );
        }
        // 唯一还剩的禁用理由:引用的会话已经删了。必须是**真禁用** ——
        // 画成灰的但仍能点,用户点了没反应会以为是隧道配错了。
        assert!(
            !enabled_of(&[tun(9, 999, local())], TunnelId(9)),
            "引用悬垂时按钮必须是真禁用而不是画成灰的"
        );
    }

    /// 悬垂引用:列表和编辑器都要**说出来**,并且不许静默改指向。
    #[test]
    fn a_dangling_tunnel_says_so_instead_of_snapping_to_another_session() {
        let sessions = vec![sess(1, "还在的会话")];
        let tunnels = vec![tun(1, 999, local())];
        let mut st = open(ManagerMode::Tunnels);
        st.tunnel_editor_id = Some(TunnelId(1));
        st.tunnel_editor = Some(TunnelEditorBuffer::from_record(&tunnels[0]));
        st.tunnel_editor_baseline = st.tunnel_editor.clone();

        let (_c, out) = run(&mut st, &sessions, &tunnels);
        let texts = all_text(&out.shapes);
        assert!(has(&texts, "引用的会话已删除"), "没有提示悬垂: {texts:?}");
        assert!(
            has(&texts, "已删除的会话 (id=999)"),
            "下拉必须保持显示原来的 id,而不是跳到第一条: {texts:?}"
        );
        assert_eq!(
            st.tunnel_editor.as_ref().and_then(|b| b.session_id),
            Some(SessionId(999)),
            "渲染一帧不该把悬垂引用悄悄改成别的会话"
        );
    }

    /// 标签必须随类型翻转,且**不能同时出现** —— 两套方向词一起摆在屏幕上
    /// 比不写还糟。
    #[test]
    fn local_and_remote_use_different_captions_and_never_both() {
        let sessions = vec![sess(1, "a")];
        let probe = |kind: TunnelKindUi| {
            let mut st = open(ManagerMode::Tunnels);
            st.tunnel_editor = Some(TunnelEditorBuffer {
                session_id: Some(SessionId(1)),
                kind,
                listen_port: "3306".into(),
                target_host: "db.internal".into(),
                target_port: "3306".into(),
                ..Default::default()
            });
            let (_c, out) = run(&mut st, &sessions, &[]);
            all_text(&out.shapes)
        };

        let l = probe(TunnelKindUi::Local);
        assert!(has(&l, "本机侦听端口"), "{l:?}");
        assert!(!has(&l, "远端侦听端口"), "两种方向词同时出现: {l:?}");
        assert!(
            has(&l, "由远端主机解析"),
            "-L 的目标名由远端解析,这句必须写出来: {l:?}"
        );

        let r = probe(TunnelKindUi::Remote);
        assert!(has(&r, "远端侦听端口"), "{r:?}");
        assert!(!has(&r, "本机侦听端口"), "两种方向词同时出现: {r:?}");
        assert!(
            has(&r, "由本机解析"),
            "-R 的目标名由本机解析,这句必须写出来: {r:?}"
        );
    }

    /// `-D` 没有目标,也没有暴露开关 —— 后者在类型上就不存在(见
    /// `TunnelKind::Dynamic`),UI 若还画一个,就是画了个写不进数据的控件。
    #[test]
    fn dynamic_renders_neither_target_nor_expose_checkbox() {
        let sessions = vec![sess(1, "a")];
        let mut st = open(ManagerMode::Tunnels);
        st.tunnel_editor = Some(TunnelEditorBuffer {
            session_id: Some(SessionId(1)),
            kind: TunnelKindUi::Dynamic,
            listen_port: "1080".into(),
            ..Default::default()
        });
        let (_c, out) = run(&mut st, &sessions, &[]);
        let texts = all_text(&out.shapes);
        assert!(has(&texts, "本机 SOCKS5 端口"), "{texts:?}");
        assert!(!has(&texts, "目标"), "动态转发不该有目标区: {texts:?}");
        assert!(!has(&texts, "允许"), "动态转发不该有暴露勾选框: {texts:?}");
    }

    /// F117:勾上暴露要给**带具体目标**的警告。泛泛一句「有风险」没法让用户
    /// 判断该不该勾。
    #[test]
    fn checking_expose_warns_with_the_concrete_target() {
        let sessions = vec![sess(1, "a")];
        let mut st = open(ManagerMode::Tunnels);
        st.tunnel_editor = Some(TunnelEditorBuffer {
            session_id: Some(SessionId(1)),
            kind: TunnelKindUi::Local,
            listen_port: "3306".into(),
            target_host: "db.internal".into(),
            target_port: "3306".into(),
            expose: true,
            ..Default::default()
        });
        let (_c, out) = run(&mut st, &sessions, &[]);
        let texts = all_text(&out.shapes);
        assert!(
            texts
                .iter()
                .any(|s| s.contains("db.internal:3306") && s.contains("不做任何认证")),
            "暴露警告要带具体目标: {texts:?}"
        );

        // 不勾就不该有这句 —— 常驻的红字等于没有红字。
        let mut st2 = open(ManagerMode::Tunnels);
        st2.tunnel_editor = Some(TunnelEditorBuffer {
            session_id: Some(SessionId(1)),
            kind: TunnelKindUi::Local,
            listen_port: "3306".into(),
            target_host: "db.internal".into(),
            target_port: "3306".into(),
            expose: false,
            ..Default::default()
        });
        let (_c, out) = run(&mut st2, &sessions, &[]);
        assert!(!has(&all_text(&out.shapes), "不做任何认证"));
    }

    /// Task 7:删会话前列出受影响的隧道;没有引用时**不多画一块空区域**。
    #[test]
    fn deleting_a_referenced_session_lists_the_affected_tunnels() {
        let sessions = vec![sess(7, "要删的")];
        let tunnels = vec![tun(1, 7, local()), tun(2, 7, TunnelKind::Dynamic)];
        let mut st = open(ManagerMode::Sessions);
        st.pending_delete = Some(SessionId(7));
        let (_c, out) = run(&mut st, &sessions, &tunnels);
        let texts = all_text(&out.shapes);
        assert!(has(&texts, "以下隧道会失去引用"), "{texts:?}");
        assert!(has(&texts, "本地 3306"), "{texts:?}");
        assert!(has(&texts, "动态 3306"), "{texts:?}");

        // 无引用:确认框保持原样。
        let mut st2 = open(ManagerMode::Sessions);
        st2.pending_delete = Some(SessionId(7));
        let (_c, out) = run(&mut st2, &sessions, &[]);
        let texts = all_text(&out.shapes);
        assert!(has(&texts, "删除「要删的」?"), "确认框本身要在: {texts:?}");
        assert!(
            !has(&texts, "以下隧道会失去引用"),
            "没有隧道引用时不该多出一块空区域: {texts:?}"
        );
    }

    /// D3:确认框里必须把「正在跑的会被停掉」单独说出来。跟「会失去引用」
    /// 混为一句的话,用户按下删除时不知道自己此刻会切断什么。
    #[test]
    fn delete_confirm_says_how_many_of_them_are_running() {
        let t = crate::theme::MULLION_DARK;
        let sessions = vec![sess(7, "要删的")];
        let tunnels = vec![tun(1, 7, local()), tun(2, 7, TunnelKind::Dynamic)];
        let states = vec![(TunnelId(1), mullion_ssh::tunnel::TunnelState::Running)];
        let mut st = open(ManagerMode::Sessions);
        st.pending_delete = Some(SessionId(7));
        let ctx = egui::Context::default();
        let mut out = None;
        for _ in 0..2 {
            out = Some(ctx.run(Default::default(), |c| {
                show(
                    c,
                    &t,
                    &mut st,
                    &sessions,
                    &[],
                    &[],
                    &tunnels,
                    &states,
                    true,
                    SecretPresence::default(),
                    SecretPresence::default(),
                    &crate::ui::badge::AppearanceCache::default(),
                );
            }));
        }
        let texts = all_text(&out.unwrap().shapes);
        assert!(
            texts.iter().any(|s| s.contains("正在运行")),
            "没说清有几条正在跑: {texts:?}"
        );
        assert!(
            texts.iter().any(|s| s.contains("其中 1 条")),
            "两条引用里只有一条在跑,不该报 2: {texts:?}"
        );
    }

    /// 隧道页没有会话可切、可连。不加这道门的话,站在隧道页按 ↑↓ 会对一条
    /// **看不见**的会话发 `pending_switch`(会话表单脏着时还会凭空弹出
    /// 「有未保存的更改」确认框),按 Enter 会连接一条看不见的会话。
    ///
    /// 这是 F116 引入模式条时漏的门,本测试在修复前必须先红。
    #[test]
    fn tunnel_mode_ignores_session_keyboard_actions() {
        let sessions = vec![sess(1, "生产主控")];
        let t = crate::theme::MULLION_DARK;
        let key = |k: egui::Key| egui::Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        };

        // ↓:`pending_switch` 会在同一帧内被消费掉(不脏就立即
        // `apply_switch`,见该函数文档),帧结束后它本来就是 `None`——不管有
        // 没有这道门都一样,单独查它测不出漏洞。真正的症状是 `editor_id`
        // 被悄悄换成了一条隧道页上看不见的会话,所以查它。
        let mut st = open(ManagerMode::Tunnels);
        let ctx = egui::Context::default();
        let _ = ctx.run(
            egui::RawInput {
                events: vec![key(egui::Key::ArrowDown)],
                ..Default::default()
            },
            |ctx| {
                show(
                    ctx,
                    &t,
                    &mut st,
                    &sessions,
                    &[],
                    &[],
                    &[],
                    &[],
                    true,
                    SecretPresence::default(),
                    SecretPresence::default(),
                    &crate::ui::badge::AppearanceCache::default(),
                );
            },
        );
        assert!(
            st.editor_id.is_none(),
            "隧道页按 ↓ 不该把右栏换成看不见的会话,实际: {:?}",
            st.editor_id
        );

        // Enter:`editor_id` 若是从会话页切过来时残留的(`mode_bar` 明确不清
        // editor 字段,见该函数文档),隧道页按 Enter 不该拿它去发连接请求。
        let mut st2 = open(ManagerMode::Tunnels);
        st2.editor_id = Some(SessionId(1));
        let ctx2 = egui::Context::default();
        let _ = ctx2.run(
            egui::RawInput {
                events: vec![key(egui::Key::Enter)],
                ..Default::default()
            },
            |ctx| {
                show(
                    ctx,
                    &t,
                    &mut st2,
                    &sessions,
                    &[],
                    &[],
                    &[],
                    &[],
                    true,
                    SecretPresence::default(),
                    SecretPresence::default(),
                    &crate::ui::badge::AppearanceCache::default(),
                );
            },
        );
        assert!(
            st2.connect_request.is_none(),
            "隧道页按 Enter 不该发起连接,实际: {:?}",
            st2.connect_request
        );
    }

    /// D1/D4:SFTP 档按 Enter 必须能连(连上开的是独占文件标签)。
    ///
    /// 这条补的是一个真 bug:`Action::Open` 的判据一直写着
    /// `== ManagerMode::Sessions`,注释还停在「SFTP 节点连不了(F50 未实现)」。
    /// D1 让 SFTP 节点可连之后,左栏双击、右键菜单、右栏按钮三条入口都改了,
    /// **唯独键盘这条没跟上** —— 纯键盘用户(本项目的目标场景之一)在 SFTP
    /// 列表里选中一条按 Enter,完全没反应,而且不报任何错。
    ///
    /// 反面(隧道档按 Enter 不放行)由 `keyboard_navigation_is_inert_in_tunnels_mode`
    /// 和下面那条兜底闸门测试守着,这里不重复。
    #[test]
    fn enter_connects_an_sftp_node_because_d1_made_them_connectable() {
        let t = crate::theme::MULLION_DARK;
        let key = |k: egui::Key| egui::Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        };
        let sessions = vec![sess(1, "生产主控"), sftp_sess(2, "文件中转")];
        let mut st = open(ManagerMode::Sftp);
        st.editor_id = Some(SessionId(2));
        let ctx = egui::Context::default();
        let _ = ctx.run(
            egui::RawInput {
                events: vec![key(egui::Key::Enter)],
                ..Default::default()
            },
            |ctx| {
                show(
                    ctx,
                    &t,
                    &mut st,
                    &sessions,
                    &[],
                    &[],
                    &[],
                    &[],
                    true,
                    SecretPresence::default(),
                    SecretPresence::default(),
                    &crate::ui::badge::AppearanceCache::default(),
                );
            },
        );
        assert_eq!(
            st.connect_request,
            Some(SessionId(2)),
            "SFTP 档按 Enter 没发出连接意图 —— 键盘用户唯一走不通的入口"
        );
    }

    /// D4:隧道档压根没有会话可连——四条入口(左栏双击、右键菜单、右栏
    /// 按钮、Enter)都要挡住。这里测的是兜底闸门:无论哪条路把
    /// `connect_request` 写了进来,出了 `show()` 都必须是 `None`。
    ///
    /// 自证会变红:把 mod.rs 里那段闸门条件从 `== Tunnels` 改回
    /// `!= Sessions`(或者干脆删掉)。
    #[test]
    fn no_connect_request_survives_the_tunnels_mode() {
        let sessions = vec![sess(1, "生产主控"), sftp_sess(2, "文件中转")];
        let mut st = open(ManagerMode::Tunnels);
        // 模拟「某条入口已经把意图写了进来」。
        st.connect_request = Some(SessionId(2));
        st.connect_skip_automation = true;
        let _ = run(&mut st, &sessions, &[]);
        assert!(st.connect_request.is_none(), "隧道档不该放行连接意图");
        assert!(
            !st.connect_skip_automation,
            "隧道档的跳过自动化标志也要一并清掉,否则它会漂到下一次真连接上"
        );

        // 反面:会话档必须照常放行,否则这道闸门就把正常功能一起挡了。
        let mut st = open(ManagerMode::Sessions);
        st.connect_request = Some(SessionId(1));
        let _ = run(&mut st, &sessions, &[]);
        assert_eq!(
            st.connect_request,
            Some(SessionId(1)),
            "会话档的连接意图被误挡了"
        );
    }

    /// D1/D24:SFTP 节点现在**可以**连了(连上开的是独占的文件标签,见
    /// `App::spawn_connect` 的 `wants_sftp` 分支)——闸门不该再挡 SFTP 档。
    /// 这条与上面那条 `Tunnels` 分别测,不是从循环里删掉 `Sftp` 分支了事:
    /// 删掉就丢了「SFTP 档要放行」这个新增的正面断言,是在假装它无关紧要。
    #[test]
    fn connect_request_passes_through_the_sftp_mode_now() {
        let sessions = vec![sess(1, "生产主控"), sftp_sess(2, "文件中转")];
        let mut st = open(ManagerMode::Sftp);
        st.connect_request = Some(SessionId(2));
        st.connect_skip_automation = true;
        let _ = run(&mut st, &sessions, &[]);
        assert_eq!(
            st.connect_request,
            Some(SessionId(2)),
            "SFTP 档的连接意图不该再被挡(D1:连上开文件标签)"
        );
        assert!(
            st.connect_skip_automation,
            "放行路径不该动 connect_skip_automation"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_store::SessionId;

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
            auth: Auth::inline("root", AuthKind::Password),
            terminal: Default::default(),
            appearance: Default::default(),
            network: Default::default(),
            automation: Default::default(),
            sftp: Default::default(),
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
                            &[],
                            &[],
                            &[],
                            true,
                            SecretPresence::default(),
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
                &[],
                &[],
                &[],
                true,
                SecretPresence::default(),
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
                &[],
                &[],
                &[],
                true,
                SecretPresence::default(),
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
                &[],
                &[],
                &[],
                true,
                SecretPresence::default(),
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
                &[],
                &[],
                &[],
                true,
                SecretPresence::default(),
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
                &[],
                &[],
                &[],
                true,
                SecretPresence::default(),
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
                &[],
                &[],
                &[],
                true,
                SecretPresence::default(),
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
                    &[],
                    &[],
                    &[],
                    true,
                    SecretPresence::default(),
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
                    &[],
                    &[],
                    &[],
                    true,
                    SecretPresence::default(),
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
                    &[],
                    &[],
                    &[],
                    true,
                    SecretPresence::default(),
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
                &[],
                &[],
                &[],
                true,
                SecretPresence::default(),
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
                    &[],
                    &[],
                    &[],
                    true,
                    SecretPresence::default(),
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
                    &[],
                    &[],
                    &[],
                    true,
                    SecretPresence::default(),
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
                    &[],
                    &[],
                    &[],
                    true,
                    SecretPresence::default(),
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

    /// F100:标注模式开着时,会话管理器的**每一层容器**都得登记出来。
    ///
    /// 这条测试的必要性:插桩是散落在五个文件里的一行行 `annotate::mark`,
    /// 删掉任何一行都编译得过、跑得起来、界面看着一模一样 —— 唯一的症状是
    /// 用户点那个位置时描不出框,而这在无头环境里看不见。
    ///
    /// 自证会变红:注释掉任意一行被断言的 `annotate::mark`。
    #[test]
    fn annotate_mode_registers_every_container_of_the_session_manager() {
        let t = crate::theme::MULLION_DARK;
        let sessions = vec![SessionRecord {
            id: SessionId(1),
            modified_at: "2026-08-09T00:00:00Z".into(),
            identity: mullion_store::model::Identity {
                name: "web01".into(),
                note: String::new(),
                group_id: None,
                tags: Vec::new(),
            },
            connection: mullion_store::model::Connection {
                host: "10.0.0.1".into(),
                port: 22,
                protocol: mullion_store::Protocol::Ssh,
            },
            auth: mullion_store::Auth::inline("root", mullion_store::model::AuthKind::Password),
            terminal: Default::default(),
            appearance: Default::default(),
            network: Default::default(),
            automation: Default::default(),
            sftp: Default::default(),
        }];
        let groups: Vec<GroupRecord> = Vec::new();
        let mut ui_state = UiState {
            session_manager_open: true,
            // 右栏必须在「正在编辑」态,否则画的是空态提示,Tab 条/分节/底栏
            // 一个都不会出现,这条测试就只覆盖了左栏。
            editor: Some(EditorBuffer {
                name: "web01".into(),
                host: "10.0.0.1".into(),
                user: "root".into(),
                ..Default::default()
            }),
            editor_id: Some(SessionId(1)),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1600.0, 900.0),
            )),
            ..Default::default()
        };
        annotate::toggle(&ctx);
        let mut paths = Vec::new();
        // 跑两帧:面板/`CollapsingHeader` 的持久化状态要第二帧才稳定,第一帧
        // 分组可能还是折叠的、会话行不会进 body 闭包。
        for _ in 0..2 {
            let _ = ctx.run(input(), |ctx| {
                show(
                    ctx,
                    &t,
                    &mut ui_state,
                    &sessions,
                    &groups,
                    &[],
                    &[],
                    &[],
                    true,
                    SecretPresence::default(),
                    SecretPresence::default(),
                    &crate::ui::badge::AppearanceCache::default(),
                );
                paths = annotate::spot_paths(ctx);
            });
        }
        for want in [
            "会话管理器",
            "会话管理器/左栏",
            "会话管理器/左栏/搜索框",
            "会话管理器/左栏/新建按钮",
            "会话管理器/左栏/分组头「未分组」",
            "会话管理器/左栏/会话行「web01」",
            "会话管理器/右栏",
            "会话管理器/右栏/Tab 条",
            "会话管理器/右栏/Tab「连接」",
            "会话管理器/右栏/底部按钮条",
        ] {
            assert!(
                paths.iter().any(|p| p == want),
                "缺插桩:{want}\n本帧登记的是:{paths:#?}"
            );
        }
        // 分节标题是 `fields.rs` 里那一处公共插桩,「连接」页至少有一节。
        assert!(
            paths
                .iter()
                .any(|p| p.starts_with("会话管理器/右栏/分节「")),
            "「连接」页一节都没登记,`fields::section` 的插桩掉了\n{paths:#?}"
        );
    }
}
