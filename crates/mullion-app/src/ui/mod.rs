//! egui UI 构建,与 app 事件循环解耦。build_ui 每帧在 egui ctx.run 闭包里调。
pub mod chrome;
pub mod host_key;
pub mod pane_title;
pub mod paste;
pub mod session_manager;
pub mod toolbar;

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

    /// 「分屏 → 显示/隐藏 pane 标题条」被点了(F83)。app.rs 消费后复位,
    /// 翻转 `Workspace::title_bars` 并重算几何(会改行数 → 必发 window_change)。
    pub toggle_title_bars: bool,
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

/// 每帧构建 UI:菜单栏(顶)+ 工具栏(F82)+ 状态栏(底)+ 各 pane 标题条(F83)
/// + 弹窗,之后把中央区剩余尺寸写回 `central_px`。返回本帧的布局动作。
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
    chrome::top_menu(ctx, t, ui_state, frame.connected);
    // 工具栏在菜单栏之下、状态栏之上:三个 Panel 的 show 顺序决定它们
    // 从窗口边缘往里堆的次序,换顺序会让工具栏跑到状态栏上面去。
    if frame.connected {
        actions.preset = toolbar::show(ctx, t, frame.preset);
    }
    chrome::status_bar(
        ctx,
        t,
        frame.panes,
        frame.connected,
        ui_state.last_error.as_deref(),
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
    if ui_state.session_manager_open || ui_state.editor_open {
        session_manager::show(ctx, ui_state, frame.sessions, frame.store_available);
    }
    // 中央区剩余像素:available_rect 是 point,× pixels_per_point 换像素。
    // 必须在所有 TopBottomPanel 都 show 完之后取(现在多了工具栏),拿到的才是
    // 扣掉菜单栏+工具栏+状态栏的中央区。原点与尺寸一起记:尺寸决定几行几列,
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

    /// 一个空 `UiFrame`,测试各自按需覆盖需要的字段。
    fn base_frame() -> UiFrame<'static> {
        UiFrame {
            sessions: &[],
            store_available: false,
            connected: true,
            panes: 1,
            preset: None,
            titles: &[],
            host_key: None,
            paste: None,
        }
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
        let mut actions = UiActions::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            let _ = build_ui(ctx, &crate::theme::MULLION_DARK, &mut ui_state, frame);
        });
        let out = ctx.run(egui::RawInput::default(), |ctx| {
            actions = build_ui(ctx, &crate::theme::MULLION_DARK, &mut ui_state, frame);
        });
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
        (text, actions)
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
        }
    }

    /// F82:工具栏只在已连接时露出(launcher 态没有 pane 可切布局)。
    /// 破坏性验证:把 `build_ui` 里 `if frame.connected` 改成 `if !frame.connected`,
    /// 本测试的 `connected: true` 分支断言(应该出现 Single 预设的按钮文案)会红。
    #[test]
    fn build_ui_toolbar_shows_only_when_connected_f82() {
        let (connected_text, _) = rendered_text(UiFrame {
            connected: true,
            ..base_frame()
        });
        assert!(
            connected_text.contains(crate::shell::workspace::Preset::Single.label()),
            "已连接时工具栏应该画出预设按钮,实际文本: {connected_text:?}"
        );

        let (disconnected_text, _) = rendered_text(UiFrame {
            connected: false,
            ..base_frame()
        });
        assert!(
            !disconnected_text.contains(crate::shell::workspace::Preset::Single.label()),
            "未连接(launcher 态)不该画工具栏,实际文本: {disconnected_text:?}"
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

    /// 已知盲区:`actions.preset`/`actions.close_pane` 是否真的来自
    /// `toolbar::show`/`pane_title::show` 的返回值(而非被硬编码成恒 `None`),
    /// 在无头环境下无法验证——不模拟点击时,真实调用链和硬编码 `None` 从外部
    /// 观察不出区别(两者结果都是 `None`)。这里只能断言"没点击时确实是
    /// `None`",不能证明"点击后会变成 Some"这条链路真的接通。要补全这段需要
    /// 模拟指针点击(需要拿到按钮的精确布局矩形,当前 egui 版本没有稳定的
    /// 无头手段拿到未显式命名 id 的按钮矩形),留给后续切片按需处理。
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
}
