//! egui UI 构建,与 app 事件循环解耦。build_ui 每帧在 egui ctx.run 闭包里调。
pub mod chrome;
pub mod host_key;
pub mod paste;
pub mod session_manager;

use std::sync::Arc;

use mullion_store::{SessionId, SessionRecord};

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
    /// 中央区可用像素(egui 布局后写入,喂 shell::viewport::grid_dims)。
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
    /// 列表当前选中的会话。
    pub selected: Option<SessionId>,
    /// 点了「删除」但还没二次确认;确认后转成 `delete_request`。
    pub pending_delete: Option<SessionId>,
    /// 双击行 / 点「连接」→ app 事后据此 `ssh_config_for` + `spawn_connect`。
    pub connect_request: Option<SessionId>,
    /// 二次确认后的删除意图 → app 事后据此调 `store.delete`。
    pub delete_request: Option<SessionId>,
    /// 编辑表单点「保存」→ app 事后据此调 `store.add`/`store.update`。
    pub save_request: Option<session_manager::SaveIntent>,
    /// 编辑子表单是否展示。
    pub editor_open: bool,
    /// 正在编辑的会话 id;`None` = 新建。
    pub editor_id: Option<SessionId>,
    /// 编辑表单的跨帧字段缓冲。
    pub editor: session_manager::EditorBuffer,
    /// 点了「选择…」私钥文件 → app 事后另起线程开系统文件对话框(不能在
    /// egui 闭包里同步阻塞,那是在 winit 事件回调中间停掉整个事件循环)。
    pub pick_key_request: bool,

    /// 主机密钥弹窗的回答(F3)。`Some(true)` = 接受;`Some(false)` = 取消连接。
    /// 同样只承载意图:record + save + 回送 oneshot 都在 app.rs 施加点做。
    pub host_key_reply: Option<bool>,

    /// 多行粘贴确认弹窗的回答(F18)。`Some(true)` = 粘贴;`Some(false)` = 取消。
    /// 同样只承载意图:取出 `pending_paste` 并发送在 app.rs 施加点做。
    pub paste_reply: Option<bool>,
}

/// 每帧构建 UI:菜单栏(顶)+ 状态栏(底)+ 会话管理弹窗,之后把中央区剩余尺寸写回
/// central_px。`connected`:是否已有连接;`panes`:状态栏左栏的屏数;`sessions`/
/// `store_available`:会话列表快照与 store 是否成功打开(待定 G:打不开时优雅禁用)。
#[allow(clippy::too_many_arguments)] // 与 render_frame 同理(F18 新增 paste 参数);拆结构体属于范围外重构。
pub fn build_ui(
    ctx: &egui::Context,
    t: &crate::theme::Theme,
    ui_state: &mut UiState,
    sessions: &[SessionRecord],
    store_available: bool,
    connected: bool,
    panes: usize,
    host_key: Option<host_key::HostKeyView<'_>>,
    paste: Option<paste::PasteView<'_>>,
) {
    // 主机密钥确认最先画:它是安全关口,任何时候都该盖在最上层(F3)。
    if let Some(view) = &host_key {
        host_key::show(ctx, view, &mut ui_state.host_key_reply);
    }
    // 粘贴确认排在主机密钥之后:安全关口优先级最高,粘贴其次。
    // 两者同时出现的可能性极低(握手期间还没有终端可粘),但顺序要写死,
    // 别留给 egui 的绘制顺序去决定谁盖谁。
    if let Some(view) = &paste {
        paste::show(ctx, view, &mut ui_state.paste_reply);
    }
    chrome::top_menu(ctx, t, ui_state, connected);
    chrome::status_bar(ctx, t, panes, connected, ui_state.last_error.as_deref());
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
    if ui_state.session_manager_open || ui_state.editor_open {
        session_manager::show(ctx, ui_state, sessions, store_available);
    }
    // 中央区剩余像素:available_rect 是 point,× pixels_per_point 换像素。
    // 必须在两个 TopBottomPanel 都 show 完之后取,拿到的才是扣掉菜单栏+状态栏
    // 的中央区。原点(rect.min)与尺寸一起记:尺寸决定终端有几行几列,原点决定
    // 这几行画在哪儿——只记尺寸就是本次遮挡 bug 的成因。
    let ppp = ctx.pixels_per_point();
    let rect = ctx.available_rect();
    ui_state.central_px = (
        (rect.width() * ppp).max(0.0) as u32,
        (rect.height() * ppp).max(0.0) as u32,
    );
    ui_state.central_origin_px = ((rect.min.x * ppp).max(0.0), (rect.min.y * ppp).max(0.0));
}
