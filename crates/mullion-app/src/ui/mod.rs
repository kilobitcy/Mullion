//! egui UI 构建,与 app 事件循环解耦。build_ui 每帧在 egui ctx.run 闭包里调。
pub mod badge;
pub mod chrome;
pub mod group_manager;
pub mod host_key;
pub mod ico;
pub mod icon;
pub mod metrics;
pub mod pane_title;
pub mod paste;
pub mod session_manager;
pub mod toast;
pub mod toolbar;

use std::sync::Arc;

use mullion_store::{GroupId, GroupRecord, SessionId, SessionRecord};

/// 给 egui 挂上系统 CJK 字体作回退。egui 只内嵌拉丁字体,中文菜单/状态栏否则
/// 全渲染成 tofu 方框。按存在顺序取第一个系统字体(Windows 一等公民);非 Windows
/// 或都找不到就静默返回,egui 用默认字体(不崩)。启动时对 egui_ctx 调一次即可。
pub fn install_cjk_font(ctx: &egui::Context) {
    // .ttc 用 FontData 默认 index 0(如 msyh.ttc face 0 = 微软雅黑 Regular)。
    const CANDIDATES: &[&str] = &[
        r"C:\Windows\Fonts\msyh.ttc",   // 微软雅黑
        r"C:\Windows\Fonts\simhei.ttf", // 黑体
        r"C:\Windows\Fonts\Deng.ttf",   // 等线
        r"C:\Windows\Fonts\simsun.ttc", // 宋体
    ];
    let Some(bytes) = CANDIDATES.iter().find_map(|p| std::fs::read(p).ok()) else {
        return;
    };
    // 从 default 出发:保留内嵌拉丁字体作主字体,只把 CJK 追加为末位回退。
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "system-cjk".to_owned(),
        Arc::new(egui::FontData::from_owned(bytes)),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("system-cjk".to_owned());
    }
    ctx.set_fonts(fonts);
}

/// App 的 UI 侧状态(与渲染/连接解耦)。
#[derive(Default)]
pub struct UiState {
    pub session_manager_open: bool,
    pub about_open: bool,
    pub last_error: Option<String>,
    /// 用户是否关掉了当前这条错误卡片。**只该由 `set_error` 复位** ——
    /// 各处直接写 `last_error` 会绕过复位,导致关掉一次后再也看不到错误。
    pub error_dismissed: bool,
    /// 走查 13:错误卡片的「详情」是不是展开着。同 `error_dismissed`,
    /// **只该由 `set_error` 复位** —— 上一条错误展开着,不代表下一条也要
    /// 劈头甩一屏堆栈。
    pub error_expanded: bool,
    /// 中央区可用像素(egui 布局后写入,喂 `App::compute_geoms` → `shell::workspace::geom::layout_geometry`,
    /// 按像素切分布局树)。
    pub central_px: (u32, u32),
    /// 中央区左上角像素(egui 布局后写入)。终端自绘层必须**整体平移**到这里,
    /// 否则第 0 行画在窗口顶端、被顶部菜单栏盖住(用户看不到首行输出)。
    /// 鼠标坐标换算要用同一个原点,见 `App::cursor_in_grid`。
    pub central_origin_px: (f32, f32),
    pub request_disconnect: bool,
    pub request_quit: bool,

    // --- Task 6:会话管理弹窗。egui 闭包只有 `&mut UiState`,借不到 `&mut store`,
    // 所以下面这些字段只承载「意图」,由 app.rs 在 render_frame 返回、借用释放后
    // 统一施加(与既有 request_disconnect/request_quit 同构)。---
    /// 点了「删除」但还没二次确认;确认后转成 `delete_request`。
    pub pending_delete: Option<SessionId>,
    /// 双击行 / 点「连接」→ app 事后据此 `ssh_config_for` + `spawn_connect`。
    pub connect_request: Option<SessionId>,
    /// 最后一次发起连接的会话 id。`UserEvent::ConnectOk` 不带 SessionId,
    /// 状态点要知道「是哪条连上了」,只能在发起时记下来。
    pub connect_request_last: Option<SessionId>,
    /// 正在拨号中的会话(走查 4)。`spawn_connect` 置位,`ConnectOk`/`ConnectErr`
    /// 清位。**不能从 `connect_request_last` 反推**:那个字段在连接成功之后
    /// 仍然留着(状态点靠它认「是哪条连上了」),反推的话每条连上的会话都会
    /// 永远显示「连接中」。
    pub connecting: Option<SessionId>,
    /// 最近一次拨号失败的会话(走查 4)。下一次对**任何**会话发起连接时清空 ——
    /// 一条陈旧的失败标记挂上几个小时,用户会以为那条会话现在是坏的。
    pub connect_failed: Option<SessionId>,
    /// F44:本次连接一次性跳过自动化(右键菜单)。app.rs 消费后立即清零 ——
    /// 右键跳过一次之后,普通双击连接若还静默跳过,用户会以为自动化坏了。
    pub connect_skip_automation: bool,
    /// 二次确认后的删除意图 → app 事后据此调 `store.delete`。
    pub delete_request: Option<SessionId>,
    /// 右键「移动到分组」(走查 3)。`None` 的 `GroupId` = 移到「未分组」。
    ///
    /// 不复用 `save_request`:那条通道要一份完整的 draft(整个表单的所有字段),
    /// 右键菜单手上只有一个 id,为了改一个 `group_id` 去凭空造一份 draft,一旦
    /// 哪个字段填漏就是静默地把用户的配置改掉。
    pub move_to_group: Option<(SessionId, Option<GroupId>)>,
    /// 编辑表单点「保存」→ app 事后据此调 `store.add`/`store.update`。
    pub save_request: Option<session_manager::SaveIntent>,
    /// 正在编辑的会话 id;`None` = 新建。
    pub editor_id: Option<SessionId>,
    /// 编辑表单的跨帧字段缓冲。`None` = 右栏未在编辑任何会话(画空态提示)。
    pub editor: Option<session_manager::EditorBuffer>,
    /// 编辑区当前内容的基线快照,用于脏检查(见 `session_manager::is_dirty`)。
    /// 与 `editor` 同时设置、同时清空。
    pub editor_baseline: Option<session_manager::EditorBuffer>,
    /// 待切换目标。表单脏时先弹确认,用户选「丢弃」才消费它。
    pub pending_switch: Option<session_manager::SwitchTarget>,
    /// 切换时表单是脏的 → 正在等用户确认。为真时右栏顶部压一条确认横幅。
    pub confirm_switch: bool,
    /// 用户在确认横幅上点了「丢弃并切换」。中转一层是因为
    /// `session_manager::editor::show` 正持着 `ui_state.editor` 的 `&mut`,
    /// 不能在那里直接调 `apply_switch`(它要重设 `ui_state.editor`);真正的
    /// 施加挪到 `session_manager::show` 里、`Window::show` 借用释放之后。
    pub discard_and_switch: bool,
    /// 左栏搜索框内容。
    pub search: String,
    /// 走查 21:刚点了「+ 新建」,下一帧把光标放进「名称」框。
    ///
    /// **一次性**:`fields::basic` 消费后立即复位。不复位的话每帧都抢焦点,
    /// 用户按 Tab 走到别的字段会被当场弹回来。
    pub focus_name_request: bool,
    /// 走查 15:哪些必填框已经被用户碰过 —— 只有碰过的才亮红字。
    /// 放这儿而不是 `EditorBuffer` 里的理由见 `validate::Touched`。
    /// 切换会话时随表单一起重置。
    pub touched: session_manager::validate::Touched,
    /// 右栏当前 Tab(0=基础 1=认证 2=网络)。
    pub editor_tab: usize,
    /// 点了「选择…」私钥文件 → app 事后另起线程开系统文件对话框(不能在
    /// egui 闭包里同步阻塞,那是在 winit 事件回调中间停掉整个事件循环)。
    pub pick_key_request: bool,
    /// 右栏「保存」被点了。`Some(true)` = 「保存并连接」。
    /// 中转一层是因为 `build_draft` 要读整个 `EditorBuffer`,而这里正持着
    /// 它的 `&mut`。
    pub save_click: Option<bool>,

    /// 主机密钥弹窗的回答(F3)。`Some(true)` = 接受;`Some(false)` = 取消连接。
    /// 同样只承载意图:record + save + 回送 oneshot 都在 app.rs 施加点做。
    pub host_key_reply: Option<bool>,

    /// 多行粘贴确认弹窗的回答(F18)。`Some(true)` = 粘贴;`Some(false)` = 取消。
    /// 同样只承载意图:取出 `pending_paste` 并发送在 app.rs 施加点做。
    pub paste_reply: Option<bool>,

    /// 「分屏 → 显示/隐藏 pane 标题条」被点了(F83)。app.rs 消费后复位,
    /// 翻转 `Workspace::title_bars` 并重算几何(会改行数 → 必发 window_change)。
    pub toggle_title_bars: bool,

    // --- Task 16:分组管理弹窗(F60)。与会话管理弹窗同构:只写意图,
    // app.rs 在借用释放后统一施加。---
    /// 分组管理弹窗是否展示。
    pub group_manager_open: bool,
    /// 「新建分组」输入框的跨帧缓冲。
    pub group_name_buf: String,
    /// 分组管理弹窗里点了新建/改名/删除 → app 事后据此调 `store` 对应方法。
    pub group_intent: Option<crate::ui::group_manager::GroupIntent>,

    // --- P1-b:测试连接(F92)。与 save_click 同构 —— UI 只写意图,
    // 拨测在 app.rs 的施加点起 tokio 任务。---
    /// 「测试连接」被点了。app.rs 消费后复位,按当前表单起一次拨测。
    pub probe_click: bool,
    /// 拨测的四态,由 app.rs 在收到 `UserEvent::ProbeOk/ProbeErr` 时写。
    pub probe: crate::ui::session_manager::ProbeState,
    /// 请求取消在途拨测(切会话 / 关编辑器 / 关会话管理器)。app.rs 消费后
    /// 自增 `probe_epoch` 并 abort 任务。放意图标志而非直接持有世代号:
    /// 世代号和 JoinHandle 都在 App 上,UiState 够不着。
    pub probe_cancel: bool,
    /// 发起拨测那一刻的表单快照。结论(`Ok`/`Err`)只对这份表单有效 ——
    /// 一改字段就不再可信,清掉。
    ///
    /// **不能拿 `editor_baseline` 当这个基线**:那是「上次保存」的基线,
    /// 表达的是「相对保存是否脏」这个持久状态。新建会话时表单天然是脏的,
    /// 用它判定会让拨测结论在产生的下一帧就被清掉,成功卡片一帧都看不见 ——
    /// 而「新建会话、填完、先测一下再保存」正是「测试连接」的主场景。
    ///
    /// **持有的是整份表单的克隆,里面含 `password`/`passphrase`/
    /// `proxy_password` 三个明文字段**——凡是「编辑这条会话结束」的地方
    /// (切会话/新建、关闭会话管理器、点「取消」)都必须把它清成 `None`,
    /// 否则这份明文副本会一直留在进程堆内存里,直到用户对**另一份**表单
    /// 再点一次「测试连接」才被覆盖,驻留窗口可能横跨整个应用生命周期。
    pub probe_form: Option<session_manager::EditorBuffer>,

    // --- P1-b:~/.ssh 私钥候选(F93)。---
    /// 扫描 `~/.ssh` 得到的私钥候选路径。打开编辑器 / 切到认证 Tab 时刷一次,
    /// 之后缓存 —— `read_dir` 是同步 IO,不能每帧调(陷阱 T3)。
    pub key_candidates: Vec<std::path::PathBuf>,
    /// 候选是否已扫过。扫描动作在 `session_manager::editor::
    /// ensure_key_candidates_scanned`——渲染编辑器时调用,`false` 就扫一次
    /// 并把这里置 `true`;`close_session_manager` 关闭会话管理器时会把它
    /// 复位回 `false`,下次打开重新扫(用户可能刚 `ssh-keygen` 生成了新密钥)。
    pub key_candidates_ready: bool,
    /// 拖拽私钥文件时给用户的一次性提示(如「已忽略其余 2 个文件」)。
    pub key_drop_note: Option<String>,
    /// 走查 13:等着进场的 toast 文本。生产端(`app.rs` 施加意图那一段)拿不到
    /// `egui::Context`,也就拿不到帧时间,所以只放文本,时间戳由
    /// `toast::show` 盖。
    pub pending_toast: Option<String>,
    /// 当前正在飘着的那条 toast。
    pub toast: Option<toast::Toast>,
}

impl UiState {
    /// 报告一条错误。**所有**错误写入都必须走这里,不要直接赋值 `last_error`。
    /// 报一条「刚才那一下生效了」的短提示(走查 13)。三秒自散,不占位置。
    /// **失败一律走 `set_error`**:那是要用户处理的,不能飘一下就没了。
    pub fn set_toast(&mut self, msg: impl Into<String>) {
        self.pending_toast = Some(msg.into());
    }

    pub fn set_error(&mut self, msg: String) {
        self.last_error = Some(msg);
        self.error_dismissed = false;
        self.error_expanded = false;
    }

    /// 关闭会话管理器。**所有**关闭点都必须走这里,不要直接赋值
    /// `session_manager_open = false`:关闭时要顺带清空只属于它的、可能残留
    /// 的临时状态(目前是 `pending_delete`)——否则下次打开时,待确认删除的
    /// 确认框会带着上次的意图凭空重新出现(复核 F90 Task 10 发现的 bug)。
    ///
    /// F92:在途拨测也必须一并取消 —— 否则窗口关了,20 秒后回来的
    /// ProbeOk 还会把结果卡片写回一个已经不属于任何表单的状态上。
    pub fn close_session_manager(&mut self) {
        self.session_manager_open = false;
        self.pending_delete = None;
        self.probe = crate::ui::session_manager::ProbeState::Idle;
        // `probe_form` 揣着三个明文凭据字段的表单副本(见字段文档注释),
        // 窗口一关,编辑这件事就结束了,不该让它继续留在内存里。
        self.probe_form = None;
        self.probe_cancel = true;
        self.key_candidates_ready = false;
        // F93:上一次打开时留下的拖拽提示(如「已取第一个文件,忽略其余
        // 2 个」)不能跟着漂到下次打开、甚至下一条完全不同的会话上——
        // 用户会以为刚才对当前表单做了什么。
        self.key_drop_note = None;
    }
}

/// 一帧 UI 的全部输入。聚成结构体是为了让新增 UI 元素(F82 工具栏、F83 标题条)
/// 不再推高参数个数 —— B1 时这里已经 9 参并挂着 `too_many_arguments` 豁免。
///
/// 全部字段要么是引用要么是 `Copy` 类型(`&[T]` 恒 `Copy`,与 `T` 是否 `Copy`
/// 无关;`Preset`/`HostKeyView`/`PasteView` 都显式 `derive(Copy)`),故整体
/// `derive(Copy)`——`render_frame` 里 `egui_ctx.run` 的闭包要按值收它,而
/// `egui::Context::run` 的实现是个 loop(见 `render_frame` 内注释),按值
/// 移动一次性数据进 `FnMut` 编译不过,`Copy` 是唯一干净的解法。
#[derive(Clone, Copy)]
pub struct UiFrame<'a> {
    pub sessions: &'a [SessionRecord],
    /// 分组列表(F60)。列表分组折叠 + 编辑器分组下拉都读这个。store 不可用时传 `&[]`。
    pub groups: &'a [GroupRecord],
    pub store_available: bool,
    pub connected: bool,
    /// 状态栏左栏的屏数。必须来自 `Workspace::pane_count()`。
    pub panes: usize,
    /// 当前生效的布局预设(工具栏画选中态)。`None` = 不对应任何预设。
    pub preset: Option<crate::shell::workspace::Preset>,
    /// 每个 pane 的标题条(F83)。空 = 标题条关闭或 launcher 态。
    pub titles: &'a [pane_title::TitleView<'a>],
    pub host_key: Option<host_key::HostKeyView<'a>>,
    pub paste: Option<paste::PasteView<'a>>,
    /// 当前被编辑会话的三个凭据槽位「有没有值」。**只有 bool,无明文**。
    pub secret_presence: session_manager::SecretPresence,
    /// 当前已连接的会话(状态点用)。`UserEvent::ConnectOk` 不带 SessionId,
    /// 所以这里追踪的是「最后一次成功连上的那条」,不是全量连接集合。
    pub connected_session: Option<SessionId>,
    /// F40~F44:自动化状态一句话。`None` = 这条连接没跑过自动化。
    /// 生命周期由 `App` 管:一直显示到下一次 `spawn_connect`(那时清空)。
    pub automation: Option<&'a str>,
    /// F61/F62:已解析的会话外观。**必须是缓存**——`inherit::resolve` 不得进
    /// 渲染热路径(陷阱 T3),见 `badge::AppearanceCache` 的文档注释。
    pub appearance: &'a badge::AppearanceCache,
}

/// 用户这一帧在 UI 上做的、需要 app 事后施加的布局动作。
/// 与 `UiState` 里那些"意图字段"同构:egui 闭包借不到 `&mut Workspace`。
///
/// **没有 derive `PartialEq`**:`app.rs::render_frame` 里判断"这一趟 egui pass 是否
/// 产出了真实动作"(discard 趟兜底,见该处注释)是手写的 `xxx.is_some() || yyy.is_some()`,
/// 逐字段枚举的。新增字段时**必须**同步那处判断,否则新动作会在 discard 趟被静默丢弃。
#[derive(Default)]
pub struct UiActions {
    /// 点了工具栏上的某个布局预设。
    pub preset: Option<crate::shell::workspace::Preset>,
    /// 点了某个 pane 标题条上的 ×。
    pub close_pane: Option<mullion_core::layout::PaneId>,
}

/// 每帧构建 UI:菜单栏(顶,布局按钮 F82 画在同一行居中)、状态栏(底)、
/// 各 pane 标题条(F83)、弹窗,之后把中央区剩余尺寸写回 `central_px`。
/// 返回本帧的布局动作。
pub fn build_ui(
    ctx: &egui::Context,
    t: &crate::theme::Theme,
    ui_state: &mut UiState,
    frame: UiFrame<'_>,
) -> UiActions {
    let mut actions = UiActions::default();
    // 主机密钥确认最先画:它是安全关口,任何时候都该盖在最上层(F3)。
    if let Some(view) = &frame.host_key {
        host_key::show(ctx, view, &mut ui_state.host_key_reply);
    }
    // 粘贴确认排在主机密钥之后:安全关口优先级最高,粘贴其次。
    if let Some(view) = &frame.paste {
        paste::show(ctx, view, &mut ui_state.paste_reply);
    }
    // 布局按钮组画在菜单栏那一行里(F82),所以点中的预设由 top_menu 返回。
    actions.preset = chrome::top_menu(ctx, t, ui_state, frame.connected, frame.preset);
    chrome::status_bar(
        ctx,
        t,
        frame.panes,
        frame.connected,
        ui_state.last_error.as_deref(),
        frame.automation,
        // F62:状态栏取**当前聚焦 pane**所属会话的色。多 pane 时状态栏该显示
        // 谁的色没有确定答案,所以这个落点默认不勾;勾了就按聚焦那个走 ——
        // 焦点是用户当下正在操作的那个 pane,这是唯一有意义的选择。
        frame
            .titles
            .iter()
            .find(|v| v.focused)
            .and_then(|v| v.appearance)
            .and_then(|a| badge::should_paint(a, mullion_store::ColorTarget::StatusBar)),
    );
    // 关于弹窗(§2:名称/版本/定位/仓库)。
    if ui_state.about_open {
        let mut open = ui_state.about_open;
        egui::Window::new("关于")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.heading("Mullion");
                ui.label(format!("版本 {}", env!("CARGO_PKG_VERSION")));
                ui.label("原生 GPU 加速 SSH 客户端");
                ui.hyperlink_to("GitHub", "https://github.com/kilobitcy/Mullion");
            });
        ui_state.about_open = open;
    }
    if ui_state.session_manager_open {
        session_manager::show(
            ctx,
            t,
            ui_state,
            frame.sessions,
            frame.groups,
            frame.store_available,
            frame.connected_session,
            frame.secret_presence,
            frame.appearance,
        );
    }
    if ui_state.group_manager_open {
        group_manager::show(ctx, ui_state, frame.groups);
    }
    // 走查 13:操作反馈飘在所有弹窗之上 —— 保存成功的那条 toast 要在会话
    // 管理器还开着的时候就能看见,不然用户根本不知道刚才那一下有没有生效。
    toast::show(ctx, t, &mut ui_state.pending_toast, &mut ui_state.toast);
    // 中央区剩余像素:available_rect 是 point,× pixels_per_point 换像素。
    // 必须在菜单栏和状态栏两个 TopBottomPanel 都 show 完之后取,拿到的才是
    // 扣掉这两栏的中央区。原点与尺寸一起记:尺寸决定几行几列,
    // 原点决定这几行画在哪儿——只记尺寸就是 B0 那次遮挡 bug 的成因。
    let ppp = ctx.pixels_per_point();
    let rect = ctx.available_rect();
    ui_state.central_px = (
        (rect.width() * ppp).max(0.0) as u32,
        (rect.height() * ppp).max(0.0) as u32,
    );
    ui_state.central_origin_px = ((rect.min.x * ppp).max(0.0), (rect.min.y * ppp).max(0.0));

    // 标题条最后画:它用绝对坐标,而坐标依赖上面几个 Panel 定完的中央区。
    // Area 不参与 Panel 的空间分配,所以放在 available_rect 之后不影响换算。
    actions.close_pane = pane_title::show(ctx, t, frame.titles);
    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::workspace::{PaneStatus, PxRect, TITLE_BAR_PX};
    use mullion_core::layout::PaneId;

    /// 用户关掉错误卡片后,**下一个**错误必须重新弹出来。
    /// 自证会变红:删掉 `set_error` 里的 `self.error_dismissed = false;`
    /// 这一行(这正是漏写时会发生的事),断言 2 立刻红。
    #[test]
    fn set_error_reopens_the_card_after_the_user_dismissed_the_previous_one() {
        let mut st = UiState::default();
        st.set_error("第一个错误".into());
        assert!(!st.error_dismissed);

        st.error_dismissed = true; // 用户点了 ×

        st.set_error("第二个错误".into());
        assert!(
            !st.error_dismissed,
            "新错误必须重新展开卡片,否则用户再也看不到任何错误"
        );
        assert_eq!(st.last_error.as_deref(), Some("第二个错误"));
    }

    /// 复核坑:关闭会话管理器时若只清 `session_manager_open`、不清
    /// `pending_delete`,下次打开(或搜索词恢复匹配)时,上次待确认删除的
    /// 确认框会带着旧意图凭空重新出现,用户可能在不知情下点到「删除」。
    /// `close_session_manager` 是**所有**关闭点(窗口 X / 连接成功后自动关闭)
    /// 唯一允许调的入口,必须把这类残留状态一并清空。
    ///
    /// 自证会变红:把 `close_session_manager` 里 `self.pending_delete = None;`
    /// 这一行删掉,断言立刻报 `pending_delete` 仍是 `Some(SessionId(1))`。
    #[test]
    fn close_session_manager_clears_pending_delete_so_it_cannot_resurface() {
        let mut st = UiState {
            session_manager_open: true,
            pending_delete: Some(SessionId(1)),
            ..Default::default()
        };
        st.close_session_manager();
        assert!(!st.session_manager_open);
        assert_eq!(
            st.pending_delete, None,
            "关闭会话管理器必须清空待确认删除,否则下次打开会凭空复现确认框"
        );
    }

    /// 复核 Important 1:`probe_form` 是发起拨测那一刻整份表单的克隆,里面
    /// 揣着 `password`/`passphrase`/`proxy_password` 三个明文字段(见字段
    /// 文档注释)。原先只在点「测试连接」时被覆盖写入,从来没有任何地方
    /// 清空过——用户点一次后,这份明文副本会一直留在进程堆内存里,直到
    /// 对**另一份**表单再点一次「测试连接」才被覆盖,驻留窗口可能横跨
    /// 整个应用生命周期。`close_session_manager` 是**所有**关闭点的唯一
    /// 入口,必须把它一并清空,缩短明文驻留窗口。
    ///
    /// 断言刻意不用 `assert_eq!`/`{:?}` 直接比对整个 `EditorBuffer`——它
    /// 刻意没有 `derive(Debug)`(`buffer.rs` 顶部注释:避免 `{:?}` 把三个
    /// 明文字段打印出来),只能用 `is_none()` 这种不需要 `Debug` 的判定。
    ///
    /// 自证会变红:把 `close_session_manager` 里 `self.probe_form = None;`
    /// 这一行删掉,断言立刻报 `probe_form` 仍是 `Some(..)`。
    #[test]
    fn close_session_manager_clears_probe_form_so_plaintext_credentials_do_not_linger() {
        let mut st = UiState {
            session_manager_open: true,
            probe_form: Some(session_manager::EditorBuffer::default()),
            ..Default::default()
        };
        st.close_session_manager();
        assert!(
            st.probe_form.is_none(),
            "关闭会话管理器必须清空 probe_form,否则含明文凭据的表单副本会滞留内存"
        );
    }

    /// F93:关闭会话管理器必须把私钥候选相关的两处临时状态一并清空——
    /// `key_candidates_ready` 复位成 `false`,下次打开时 `editor.rs` 才会
    /// 重新扫 `~/.ssh`(用户可能刚 `ssh-keygen` 生成了新密钥,缓存的候选
    /// 已经过时);`key_drop_note` 清空,否则上一次打开(甚至上一条完全
    /// 不同的会话)留下的拖拽提示会凭空出现在下一次打开的表单上,让用户
    /// 误以为刚才对当前表单做了什么。
    ///
    /// 前一半(`key_candidates_ready = false`)在 Task 5 就已经加了,这条
    /// 测试同时钉住这两个属性,避免以后有人只改了其中一个。
    ///
    /// 自证会变红:把 `close_session_manager` 里新加的
    /// `self.key_drop_note = None;` 这一行删掉,`key_drop_note` 断言会报
    /// 仍是 `Some("已忽略其余 2 个文件".into())`。
    #[test]
    fn close_session_manager_resets_key_candidates_ready_and_clears_drop_note() {
        let mut st = UiState {
            session_manager_open: true,
            key_candidates_ready: true,
            key_candidates: vec![std::path::PathBuf::from("/home/u/.ssh/id_ed25519")],
            key_drop_note: Some("已忽略其余 2 个文件".to_owned()),
            ..Default::default()
        };
        st.close_session_manager();
        assert!(
            !st.key_candidates_ready,
            "关闭后必须复位 ready,否则下次打开不会重新扫描,漏掉刚生成的新密钥"
        );
        assert_eq!(
            st.key_drop_note, None,
            "关闭后必须清空拖拽提示,否则会凭空出现在下一次打开的表单上"
        );
    }

    /// 一个空 `UiFrame`,测试各自按需覆盖需要的字段。
    fn base_frame() -> UiFrame<'static> {
        UiFrame {
            sessions: &[],
            groups: &[],
            store_available: false,
            connected: true,
            panes: 1,
            preset: None,
            titles: &[],
            host_key: None,
            paste: None,
            secret_presence: session_manager::SecretPresence::default(),
            connected_session: None,
            automation: None,
            // 测试专用:`AppearanceCache` 没有 const 构造,借 `Box::leak` 换一个
            // `'static` 引用——只在测试进程里泄漏一次,不是生产路径。
            appearance: Box::leak(Box::new(badge::AppearanceCache::default())),
        }
    }

    /// 真跑一帧 `build_ui`,复用调用方给的 `ctx`(才能跨帧读上一帧的 widget)
    /// 和 `input`(才能塞指针事件模拟点击)。
    fn run_frame(
        ctx: &egui::Context,
        ui_state: &mut UiState,
        frame: UiFrame<'_>,
        input: egui::RawInput,
    ) -> (egui::FullOutput, UiActions) {
        let mut actions = UiActions::default();
        let out = ctx.run(input, |ctx| {
            actions = build_ui(ctx, &crate::theme::MULLION_DARK, ui_state, frame);
        });
        (out, actions)
    }

    /// 真跑一帧 `build_ui`,把返回的形状树递归展平成纯文本,用来断言某段文案
    /// 确实被画了出来(而不是像上一版那样只构造结构体、从不调用 `build_ui`)。
    ///
    /// 跑两遍同一个 `ctx`:`egui::Area`(`pane_title.rs` 用它画标题条)在
    /// **第一次**遇到某个 id 时会先做一趟不可见的 sizing pass(只记 `area_rect`
    /// 到 memory,不产生任何 Shape,靠 `request_repaint` 排到下一帧才真正画出
    /// 内容——见 `egui-0.30.0/src/containers/area.rs:549`
    /// `ui_builder.sizing_pass().invisible()`)。只跑一遍会漏掉标题条的所有
    /// Shape,不是 `build_ui` 没接线,是 egui 自身的首帧行为。第二遍复用同一个
    /// `ctx`(memory 里已有上一遍存的 `AreaState`),`sizing_pass` 不再触发,
    /// 才能看到真实绘制内容。
    fn rendered_text(frame: UiFrame<'_>) -> (String, UiActions) {
        let ctx = egui::Context::default();
        let mut ui_state = UiState::default();
        let _ = run_frame(&ctx, &mut ui_state, frame, egui::RawInput::default());
        let (out, actions) = run_frame(&ctx, &mut ui_state, frame, egui::RawInput::default());
        (collect_text(&out), actions)
    }

    /// 把 `FullOutput` 的形状树递归展平成纯文本,用来断言某段文案确实被画了
    /// 出来。从 `rendered_text` 里抽出来,供需要自己控制帧数/`ui_state`(比如
    /// Task 15 的空态/降级/脏切换测试要在跑帧之间读回 `ui_state` 字段)的用例
    /// 复用,不重复抄一份 `walk`。
    fn collect_text(out: &egui::FullOutput) -> String {
        fn walk(shape: &egui::Shape, out: &mut String) {
            match shape {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                egui::Shape::Text(t) => {
                    out.push_str(&t.galley.job.text);
                    out.push('\n');
                }
                _ => {}
            }
        }
        let mut text = String::new();
        for cs in &out.shapes {
            walk(&cs.shape, &mut text);
        }
        text
    }

    fn title_view(host: &str) -> crate::ui::pane_title::TitleView<'_> {
        crate::ui::pane_title::TitleView {
            geom: crate::shell::workspace::PaneGeom {
                id: PaneId(1),
                px: PxRect {
                    x: 0,
                    y: 0,
                    w: 800,
                    h: 600,
                },
                title_px: PxRect {
                    x: 0,
                    y: 0,
                    w: 800,
                    h: TITLE_BAR_PX,
                },
                term_px: PxRect {
                    x: 0,
                    y: TITLE_BAR_PX,
                    w: 800,
                    h: 600 - TITLE_BAR_PX,
                },
                grid: (80, 28),
            },
            index: 1,
            host: Some(host),
            status: PaneStatus::Live,
            focused: true,
            appearance: None,
        }
    }

    /// 跑两帧(`rendered_text` 里解释了为什么要两帧)后,数一数工具栏上到底
    /// 画出了几个布局按钮。按钮是**纯图标**,没有任何文字可查(F82),所以探针
    /// 从「文案出现在 shapes 里」换成「按 `toolbar::button_id(i)` 能读到
    /// widget」—— 这也是 `button_id` 存在的理由。
    fn drawn_preset_buttons(frame: UiFrame<'_>) -> usize {
        let ctx = egui::Context::default();
        let mut ui_state = UiState::default();
        let _ = run_frame(&ctx, &mut ui_state, frame, egui::RawInput::default());
        let _ = run_frame(&ctx, &mut ui_state, frame, egui::RawInput::default());
        (0..crate::shell::workspace::Preset::ALL.len())
            .filter(|&i| ctx.read_response(toolbar::button_id(i)).is_some())
            .count()
    }

    /// F82:布局按钮只在已连接时露出(launcher 态没有 pane 可切布局),
    /// 且一次画出全部 7 个(一排平铺、始终全可见)。
    ///
    /// 破坏性验证:把 `chrome::top_menu` 里 `if connected` 改成 `if !connected`,
    /// 两条断言都红。
    #[test]
    fn build_ui_preset_buttons_show_only_when_connected_f82() {
        let n = crate::shell::workspace::Preset::ALL.len();
        assert_eq!(
            drawn_preset_buttons(UiFrame {
                connected: true,
                ..base_frame()
            }),
            n,
            "已连接时应画出全部 {n} 个布局按钮"
        );
        assert_eq!(
            drawn_preset_buttons(UiFrame {
                connected: false,
                ..base_frame()
            }),
            0,
            "未连接(launcher 态)不该画布局按钮"
        );
    }

    /// 菜单栏顶层只剩「会话 / 配置 / 关于」——「分屏」菜单已撤(布局改由同一行
    /// 的按钮组控制),F83 的标题条开关搬进了「配置」。
    ///
    /// 破坏性验证:把 `chrome.rs` 里的 `ui.menu_button("分屏", …)` 加回去,
    /// 最后一条断言红。
    #[test]
    fn menu_bar_no_longer_has_a_split_menu() {
        let (text, _) = rendered_text(UiFrame {
            connected: true,
            ..base_frame()
        });
        for item in ["会话", "配置", "关于"] {
            assert!(text.contains(item), "菜单栏缺了「{item}」: {text:?}");
        }
        assert!(
            !text.contains("分屏"),
            "菜单栏不该再有「分屏」菜单: {text:?}"
        );
    }

    /// F81/技术债 2:状态栏的屏数必须来自 `frame.panes`,不是硬编码。
    /// 破坏性验证:把 `build_ui` 里传给 `status_bar` 的 `frame.panes` 改回
    /// 硬编码 `1`,下面两次调用会得到同一段文本("1 屏"),两条断言至少一条红。
    #[test]
    fn build_ui_status_bar_pane_count_is_wired_not_hardcoded_f81() {
        let (text4, _) = rendered_text(UiFrame {
            panes: 4,
            ..base_frame()
        });
        assert!(
            text4.contains("4 屏"),
            "panes=4 时状态栏应显示 4 屏,实际文本: {text4:?}"
        );

        let (text3, _) = rendered_text(UiFrame {
            panes: 3,
            ..base_frame()
        });
        assert!(
            text3.contains("3 屏") && !text3.contains("4 屏"),
            "panes=3 时状态栏应显示 3 屏(不是残留的 4 屏),实际文本: {text3:?}"
        );
    }

    /// F40~F44:自动化状态必须真的流到状态栏,不能被 `build_ui` 吃掉。
    ///
    /// 破坏性验证:把 `build_ui` 里传给 `status_bar` 的 `frame.automation`
    /// 改成硬编码 `None`,第一条断言会红。
    #[test]
    fn build_ui_status_bar_shows_automation_status() {
        let (with_status, _) = rendered_text(UiFrame {
            automation: Some("自动化:进行中"),
            ..base_frame()
        });
        assert!(
            with_status.contains("自动化:进行中"),
            "自动化状态没画进状态栏,实际文本: {with_status:?}"
        );

        let (without, _) = rendered_text(UiFrame {
            automation: None,
            ..base_frame()
        });
        assert!(
            !without.contains("自动化"),
            "没有自动化状态时不该凭空出现文案,实际文本: {without:?}"
        );
    }

    /// F83:`frame.titles` 必须真的流到 `pane_title::show`,不能被忽略。
    /// 破坏性验证:把 `build_ui` 里传给 `pane_title::show` 的 `frame.titles`
    /// 改成硬编码 `&[]`,本测试的 "有标题条" 断言会红(标题条文案再也画不出来)。
    #[test]
    fn build_ui_titles_flow_into_pane_title_show_f83() {
        let view = title_view("uniquehostmarker");
        let (with_title, _) = rendered_text(UiFrame {
            titles: std::slice::from_ref(&view),
            ..base_frame()
        });
        assert!(
            with_title.contains("uniquehostmarker"),
            "titles 非空时应画出标题条文案,实际文本: {with_title:?}"
        );

        let (without_title, _) = rendered_text(UiFrame {
            titles: &[],
            ..base_frame()
        });
        assert!(
            !without_title.contains("uniquehostmarker"),
            "titles 为空时不该出现任何标题条文案,实际文本: {without_title:?}"
        );
    }

    /// 没点任何东西时两个动作都必须是 `None` —— 否则 app 会在每一帧被动重排
    /// 布局(每次重排都发 window_change,T4)。
    #[test]
    fn build_ui_actions_are_none_when_nothing_clicked() {
        let view = title_view("h");
        let (_, actions) = rendered_text(UiFrame {
            connected: true,
            titles: std::slice::from_ref(&view),
            preset: Some(crate::shell::workspace::Preset::Single),
            ..base_frame()
        });
        assert_eq!(actions.preset, None);
        assert_eq!(actions.close_pane, None);
    }

    /// F82 接线:点第 i 个按钮,`actions.preset` 就得是 `Preset::ALL[i]`。
    ///
    /// 这条关掉了上一版留下的盲区(「无头环境拿不到按钮矩形,证不了点击链路
    /// 真的接通」)—— `toolbar::button_id` 给了显式 id,`Context::read_response`
    /// 就能拿到精确矩形,往它中心发一次真实的指针按下/抬起即可。
    ///
    /// 破坏性验证:把 `toolbar::show_in` 结尾的 `clicked` 改成恒 `None`,
    /// 或把 `build_ui` 里 `actions.preset = chrome::top_menu(…)` 的返回值丢掉,
    /// 本测试红。**逐个索引都点一遍**是必须的:只点第一个的话,
    /// `button_rect`/`button_id` 的索引错位(比如全都返回第 0 个)不会被发现。
    #[test]
    fn build_ui_clicking_a_preset_button_wires_through_to_actions_f82() {
        let frame = UiFrame {
            connected: true,
            ..base_frame()
        };
        let ctx = egui::Context::default();
        let mut ui_state = UiState::default();
        let _ = run_frame(&ctx, &mut ui_state, frame, egui::RawInput::default());
        let _ = run_frame(&ctx, &mut ui_state, frame, egui::RawInput::default());

        for (i, expected) in crate::shell::workspace::Preset::ALL.into_iter().enumerate() {
            let pos = ctx
                .read_response(toolbar::button_id(i))
                .unwrap_or_else(|| panic!("第 {i} 个布局按钮没画出来"))
                .rect
                .center();
            let click = |pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            };
            let input = egui::RawInput {
                events: vec![egui::Event::PointerMoved(pos), click(true), click(false)],
                ..Default::default()
            };
            let (_, actions) = run_frame(&ctx, &mut ui_state, frame, input);
            assert_eq!(
                actions.preset,
                Some(expected),
                "点第 {i} 个按钮(位置 {pos:?})应得到 {expected:?}"
            );
        }
    }

    /// F90:会话管理器必须是**单窗**。设计稿把「列表 / 编辑器 / 删除确认」三个弹窗
    /// 合成一个 880×560 双栏窗口,再冒出第二个顶层 `egui::Window` 就是回归。
    ///
    /// 计数机制:`Areas::order()` 是 `pub(crate)`,拿不到;改用公开的
    /// `Areas::visible_layer_ids()`(`egui-0.30.0/src/memory/mod.rs`),同样不带
    /// `UiKind` 标签,没法按「是不是 Window」过滤。但 `egui::Window` 默认落在
    /// `Order::Middle`,而 `ComboBox` / `Popup` / tooltip 走 `Order::Foreground`,
    /// 菜单栏与状态栏是 `TopBottomPanel`(`Order::Background`)——按
    /// `Order::Middle` 过滤即等价于数窗口。
    ///
    /// 自证会变红:把 `session_manager::editor::show` 的内容重新包一层
    /// `egui::Window::new("编辑会话").show(ctx, ..)`(即本切片要消灭的那个窗口),
    /// 这条断言立刻报 `2 != 1`。
    #[test]
    fn session_manager_is_a_single_window_so_the_editor_cannot_pop_out_again() {
        let ctx = egui::Context::default();
        crate::theme::apply_egui(&ctx, &crate::theme::MULLION_DARK);
        let mut st = UiState {
            session_manager_open: true,
            ..Default::default()
        };
        let frame = UiFrame {
            store_available: true,
            ..base_frame()
        };
        // 跑两遍:egui 的 Area 首帧是不可见的 sizing pass,第二帧才落进 areas order。
        for _ in 0..2 {
            run_frame(&ctx, &mut st, frame, egui::RawInput::default());
        }

        let windows = ctx.memory(|m| {
            m.areas()
                .visible_layer_ids()
                .iter()
                .filter(|l| l.order == egui::Order::Middle)
                .count()
        });
        assert_eq!(
            windows, 1,
            "会话管理器必须是单窗;新增任何顶层 egui::Window 都会让这条变红"
        );
    }

    /// 打开一条会话后,基线必须同步设置好 —— 否则刚打开就被判成脏,
    /// 用户什么都没改也会挨一次「有未保存的更改」。
    /// 自证会变红:删掉 `apply_switch` 末尾的
    /// `ui_state.editor_baseline = ui_state.editor.clone();`,这条报「不该脏」。
    #[test]
    fn opening_a_session_sets_the_baseline_so_it_is_not_immediately_dirty() {
        let mut st = UiState {
            session_manager_open: true,
            pending_switch: Some(crate::ui::session_manager::SwitchTarget::NewDraft),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        crate::theme::apply_egui(&ctx, &crate::theme::MULLION_DARK);
        let frame = UiFrame {
            store_available: true,
            ..base_frame()
        };
        for _ in 0..2 {
            run_frame(&ctx, &mut st, frame, egui::RawInput::default());
        }

        assert!(st.editor.is_some(), "新建草稿应已切入编辑区");
        assert_eq!(
            st.editor, st.editor_baseline,
            "基线必须与刚打开的表单一致,否则会立刻被判成脏"
        );
        assert!(!st.confirm_switch, "刚打开不该弹未保存确认");
    }

    /// 没选中会话时,右栏给空态提示而不是一张填不进去的空表单。这里 `sessions`
    /// 是空的,所以走的是走查 21 的首次使用引导那一支(两支文案的分流由
    /// `session_manager::editor` 自己的测试守)。
    #[test]
    fn editor_shows_empty_state_when_nothing_is_selected() {
        let frame = UiFrame {
            store_available: true,
            ..base_frame()
        };
        let mut st = UiState {
            session_manager_open: true,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        crate::theme::apply_egui(&ctx, &crate::theme::MULLION_DARK);
        let mut text = String::new();
        for _ in 0..2 {
            let (out, _) = run_frame(&ctx, &mut st, frame, egui::RawInput::default());
            text = collect_text(&out);
        }
        assert!(
            text.contains("还没有任何会话"),
            "应显示空态提示,实得:{text}"
        );
    }

    /// §3.1 降级:会话库打不开时不画双栏,只给一句话 —— 否则用户对着一张
    /// 永远存不下去的表单填半天。
    #[test]
    fn store_unavailable_degrades_to_a_single_line_instead_of_a_dead_form() {
        let frame = UiFrame {
            store_available: false,
            ..base_frame()
        };
        let mut st = UiState {
            session_manager_open: true,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        crate::theme::apply_egui(&ctx, &crate::theme::MULLION_DARK);
        let mut text = String::new();
        for _ in 0..2 {
            let (out, _) = run_frame(&ctx, &mut st, frame, egui::RawInput::default());
            text = collect_text(&out);
        }
        assert!(text.contains("会话库不可用"), "应给降级提示,实得:{text}");
        assert!(!text.contains("从左侧选一条会话"), "降级时不该画双栏");
    }

    /// 表单脏时切走,必须先弹确认、且用户刚打的字一个都不能丢。
    /// 静默丢弃是这个窗口最伤人的失败模式 —— 用户填了半张表,点错一行就没了。
    /// 自证会变红:把 `session_manager::show` 里那段
    /// `if dirty { confirm_switch = true } else { apply_switch(..) }`
    /// 改成无条件 `apply_switch(..)`,断言 2/3 立刻红。
    #[test]
    fn switching_away_from_a_dirty_form_asks_before_discarding_the_users_typing() {
        let baseline = session_manager::EditorBuffer::default();
        let mut edited = baseline.clone();
        edited.name = "用户刚打的名字".to_string();

        let mut st = UiState {
            session_manager_open: true,
            editor: Some(edited.clone()),
            editor_baseline: Some(baseline),
            pending_switch: Some(session_manager::SwitchTarget::NewDraft),
            ..Default::default()
        };
        let frame = UiFrame {
            store_available: true,
            ..base_frame()
        };
        let ctx = egui::Context::default();
        crate::theme::apply_egui(&ctx, &crate::theme::MULLION_DARK);

        let mut text = String::new();
        for _ in 0..2 {
            let (out, _) = run_frame(&ctx, &mut st, frame, egui::RawInput::default());
            text = collect_text(&out);
        }

        assert!(st.confirm_switch, "表单脏时切走必须先弹确认");
        assert!(
            text.contains("有未保存的更改"),
            "确认横幅文案应画出来,实得:{text}"
        );
        assert_eq!(
            st.editor.as_ref().map(|b| b.name.as_str()),
            Some(edited.name.as_str()),
            "确认之前不能静默应用切换,用户刚打的字必须还在"
        );
    }
}
