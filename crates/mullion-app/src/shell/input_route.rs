//! 输入分流决策(spec §4.5)。egui 的 `consumed` 在「无控件聚焦」时不足以保证方向键/
//! 快捷键回到终端——顶栏/菜单可能间歇抢键(踩 T5/T6)。故显式按这张真值表决定:
//! 有模态→全给 egui;否则按事件类型看 egui 是否真的要这类输入,不要就回终端原路。
//! A2b 用 `egui_ctx.wants_keyboard_input()` / `wants_pointer_input()` 填这两个布尔。

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum InputKind {
    Keyboard,
    Pointer,
}

use crate::shell::workspace::PaneStatus;
use mullion_term::keymap::{Key, Mods};

/// F129:`Ctrl+D` 这一下该干什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CtrlD {
    /// 照常送 `0x04`(EOF)。
    SendEof,
    /// 关掉这块分屏。
    ClosePane,
    /// 关掉整个标签(这块是最后一块分屏)。
    CloseTab,
}

/// F129:这一下按键是不是「裸 Ctrl+D」—— 只有它才轮得到 `ctrl_d_action` 说话。
///
/// **必须排掉 `alt`**:Windows 把 AltGr 合成成 Left-Ctrl + Right-Alt,不排的话
/// 用户在断开的 pane 上用 AltGr 打字就会莫名其妙丢一块分屏(最后一块时是整个
/// 标签,连自动化和 sftp task 一起收走)。Windows 11 是本项目唯一的一等公民,
/// 这条必踩。`sup` 一并排掉:Win+D 是系统级「显示桌面」,不该被我们截。
pub fn is_bare_ctrl_d(key: Key, mods: Mods) -> bool {
    mods.ctrl
        && !mods.shift
        && !mods.alt
        && !mods.sup
        && matches!(key, Key::Char(c) if c.eq_ignore_ascii_case(&'d'))
}

/// F129:判据只有两条 —— 这块 pane 还活着吗、它是不是最后一块。
///
/// **活着就一定是 EOF**:Ctrl+D 是 shell 里退出登录、给 `cat` 收尾的标准键,
/// 改掉的话用户在正常会话里会莫名其妙丢一块分屏。断了之后 EOF 反正也送不出去,
/// 这个键在那儿本来就是废的,拿来关分屏不抢任何东西。
pub fn ctrl_d_action(status: PaneStatus, is_last_pane: bool) -> CtrlD {
    match status {
        PaneStatus::Live => CtrlD::SendEof,
        // 重连中的也算:用户按这个键的意思就是「别等了,收掉」。
        PaneStatus::Reconnecting | PaneStatus::Disconnected => {
            if is_last_pane {
                CtrlD::CloseTab
            } else {
                CtrlD::ClosePane
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Route {
    /// 交给 egui(菜单/状态栏/模态弹窗/表单)。
    Egui,
    /// 交给终端原路:键盘走 keymap+PtyWrite、鼠标走 SGR 上报(Shift 逃生门 T5)。
    Terminal,
    /// 交给文件面板(F50)。只有键盘会走到这里,指针照旧先喂 egui 后判。
    FilesPanel,
}

/// 键盘焦点在哪一侧(F6 / 设计 D23)。文件面板不存在 / 没开时恒为 `Terminal`——
/// 见 `crate::app::App::effective_focus` 按上下文夹紧的说明。
#[derive(Debug, PartialEq, Eq, Clone, Copy, Default)]
pub enum Focus {
    #[default]
    Terminal,
    FilesPanel,
}

impl Focus {
    /// F6 在两侧之间来回(设计 D23)。**不用 `Ctrl+Tab`**——D0 已经把它给了
    /// 标签切换;面板内的 `Tab` 另有用处(远端栏↔本地栏)。
    pub fn toggled(self) -> Self {
        match self {
            Focus::Terminal => Focus::FilesPanel,
            Focus::FilesPanel => Focus::Terminal,
        }
    }
}

/// egui 是否应该看到这个事件(即是否喂给 `egui_winit::State::on_window_event`)。
///
/// **T8**:键盘事件在判给终端时**绝不能**先喂 egui。egui 的焦点系统在 `begin_pass`
/// 里扫原始事件,看到 Tab 就把焦点给菜单栏第一个按钮
/// (egui 0.30 `memory/mod.rs`:"nothing has focus and the user pressed tab")。
/// 一旦如此,`wants_keyboard_input()` 恒 true,[`route`] 此后把每个按键都判给 egui,
/// 终端永久收不到任何键——症状是「Tab 补全成功后键盘全废,回车/退格都没反应」。
///
/// 指针相反,恒 true:egui 要靠 `CursorMoved` 维护 hover,不喂就没有
/// `wants_pointer_input()` 可言,菜单/弹窗再也点不动。指针的分流仍是「先喂后判」。
pub fn egui_should_see(kind: InputKind, modal_open: bool, egui_wants_keyboard: bool) -> bool {
    match kind {
        InputKind::Pointer => true,
        InputKind::Keyboard => matches!(
            route(modal_open, egui_wants_keyboard, false, kind),
            Route::Egui
        ),
    }
}

/// 这个窗口事件该不该喂给 `egui_winit::State::on_window_event`。
///
/// **T7 变种**:`egui-winit` 把 `RedrawRequested` 归进「Things that may require
/// repaint」,**恒返回 `repaint: true`**(0.30 `lib.rs`)。而我们收到 `repaint`
/// 就 `mark_ui_dirty` + `window.request_redraw()` —— 后者立刻再生成一个
/// `RedrawRequested`,闭环自激:事件循环永远有 pending redraw,`WaitUntil`/`Wait`
/// 一次也等不到。帧闸(`FrameLimiter`)只挡得住**出帧**,挡不住这一圈空转。
/// v0.1.68 的真机日志坐实:完全空闲(`in=0B/s egui_ev=0x present=0`)时
/// `window_event` = `dirty` = `rr evt` = 26 万次/5 秒 ≈ 4.8 万次/秒,一整个
/// 单核烧在这上面;`ui_dirty` 也因此恒真,F158 的「空闲不出帧」被完全架空。
///
/// `RedrawRequested` 不携带任何输入信息,egui 也不需要它 —— 帧是我们自己在
/// `WindowEvent::RedrawRequested` 分支里跑的。其余「may require repaint」的事件
/// (`Resized`/`Focused`/`Occluded`/`CloseRequested`…)必须照喂:它们是真的状态
/// 变化,而且各来一次,不构成回环。
pub fn egui_should_see_window_event(event: &winit::event::WindowEvent) -> bool {
    !matches!(event, winit::event::WindowEvent::RedrawRequested)
}

/// `modal_open`:有模态弹窗时吞掉一切;`egui_wants_keyboard`/`egui_wants_pointer`:
/// 来自 egui 上下文的 `wants_*_input()`;`kind`:本次事件类型。
pub fn route(
    modal_open: bool,
    egui_wants_keyboard: bool,
    egui_wants_pointer: bool,
    kind: InputKind,
) -> Route {
    if modal_open {
        return Route::Egui;
    }
    let egui_wants = match kind {
        InputKind::Keyboard => egui_wants_keyboard,
        InputKind::Pointer => egui_wants_pointer,
    };
    if egui_wants {
        Route::Egui
    } else {
        Route::Terminal
    }
}

/// [`route`] 的带焦点版本。面板不存在时传 `Focus::Terminal`,行为与 [`route`]
/// 完全一致。
///
/// 优先级:模态 > 面板焦点 > egui 想不想要。模态排最前是因为会话管理器开着时
/// 按 Enter 必须是「保存」而不是「进目录」。
pub fn route_focused(
    focus: Focus,
    modal_open: bool,
    egui_wants_keyboard: bool,
    egui_wants_pointer: bool,
    kind: InputKind,
) -> Route {
    if modal_open {
        return Route::Egui;
    }
    if kind == InputKind::Keyboard && focus == Focus::FilesPanel {
        return Route::FilesPanel;
    }
    route(modal_open, egui_wants_keyboard, egui_wants_pointer, kind)
}

/// F246:**指针**事件的分流。键盘照旧走 [`route_focused`],两者在这里正式分家。
///
/// 与 [`route`] 的差别只有两条,每条都对应一个具体的坏掉的姿态:
///
/// **① 编辑器开着时不吃指针。** 内置编辑器是 `Modal::Editor`(它得算模态,
/// 否则里面一个字都打不出来,T8),而 [`route`] 一见模态就把指针全判给 egui ——
/// 于是「编辑远端文件」窗口一开,背后 pane 里的文字**一个字都划不动**。而用户
/// 要往文件里写的内容,十有八九正是从那个 pane 里复制的。只放指针、不放键盘:
/// 键盘仍然全归 egui,所以不会出现「以为在编辑文件、字却打进了远端 shell」。
///
/// 其余模态照旧全吃 —— 它们要么盖住整块屏(会话管理器),要么是必须显式回答的
/// 安全判断(TOFU),让指针漏到背后的终端没有任何好处。
///
/// **② 划选中的指针被终端捕获。** 用户按住左键从 pane 里往外拖,指针会经过
/// 编辑器窗口,那一瞬 `egui_wants_pointer` 变真 —— 不捕获的话选区当场冻住
/// (`CursorMoved` 被 egui 收走,`update_selection_endpoint` 再也不被调),
/// 而且松开那一下也判给 egui,`dragging` 与 F212 的 hold **永远还不回来**
/// (T13:挂住之后这个 pane 的选区再也擦不掉)。捕获还顺带覆盖了「拖到一半
/// 弹出个模态」这种情况,理由相同。
///
/// 捕获排在模态判断**之前**:反过来的话,拖拽中途冒出来的模态会截胡那一下
/// `Released`,hold 照样挂死。
pub fn pointer_route(
    modal_open: bool,
    editor_open: bool,
    dragging: bool,
    egui_wants_pointer: bool,
) -> Route {
    if dragging {
        return Route::Terminal;
    }
    if modal_open && !editor_open {
        return Route::Egui;
    }
    if egui_wants_pointer {
        Route::Egui
    } else {
        Route::Terminal
    }
}

/// F246:一次划选结束之后,该把 egui 的键盘焦点还给谁。`None` = 谁都不还。
///
/// 只在**编辑器开着**时还。用户点进 pane 划一段字,egui 会因为这一下点在
/// 文本框外面而交出焦点 —— 复制完回头想按 `Ctrl+V`,得先再点一次编辑器,而
/// 「点回去」本身可能又把刚划的选区弄没。还回去省掉这一步。
///
/// **编辑器没开就绝不还**:那等于凭空把键盘焦点塞给一个 egui 部件,
/// `wants_keyboard_input()` 从此恒真,终端**永久**收不到任何键 —— T8 那条
/// 「Tab 补全后键盘全废」的复发形态,只是触发源换成了鼠标。
pub fn focus_to_restore<T>(editor_open: bool, saved: Option<T>) -> Option<T> {
    if editor_open {
        saved
    } else {
        None
    }
}

/// [`egui_should_see`] 的带焦点版本。**T8 的注入点就是这个函数**——判给面板
/// 的键在这里返回 `false`,于是它根本进不了 `egui_state.on_window_event`,
/// egui 的焦点系统也就无从吞掉 Tab。
pub fn egui_should_see_focused(
    focus: Focus,
    kind: InputKind,
    modal_open: bool,
    egui_wants_keyboard: bool,
) -> bool {
    match kind {
        InputKind::Pointer => true,
        InputKind::Keyboard => matches!(
            route_focused(focus, modal_open, egui_wants_keyboard, false, kind),
            Route::Egui
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modal_captures_everything() {
        assert_eq!(route(true, false, false, InputKind::Keyboard), Route::Egui);
        assert_eq!(route(true, false, false, InputKind::Pointer), Route::Egui);
    }

    #[test]
    fn terminal_gets_keyboard_when_egui_doesnt_want_it() {
        // 关键:无模态、egui 不要键盘 → 方向键/快捷键回到终端 keymap(守 T5/T6)
        assert_eq!(
            route(false, false, false, InputKind::Keyboard),
            Route::Terminal
        );
    }

    #[test]
    fn egui_widget_focus_takes_keyboard() {
        assert_eq!(route(false, true, false, InputKind::Keyboard), Route::Egui);
    }

    #[test]
    fn terminal_keyboard_is_never_fed_to_egui_so_tab_cannot_steal_focus() {
        // T8:终端聚焦时的按键(尤其 Tab)不许先经 egui —— 否则 egui 焦点系统把焦点
        // 抓到菜单栏,wants_keyboard_input 从此恒 true,终端永久收不到键。
        assert!(!egui_should_see(InputKind::Keyboard, false, false));
    }

    #[test]
    fn egui_still_sees_keyboard_when_it_owns_it() {
        assert!(egui_should_see(InputKind::Keyboard, true, false)); // 模态弹窗
        assert!(egui_should_see(InputKind::Keyboard, false, true)); // 表单聚焦
    }

    #[test]
    fn egui_always_sees_pointer() {
        // 指针必须恒喂:egui 靠 CursorMoved 维护 hover / wants_pointer_input,
        // 不喂则菜单/弹窗点不动。
        assert!(egui_should_see(InputKind::Pointer, false, false));
    }

    #[test]
    fn pointer_follows_egui_want() {
        assert_eq!(route(false, false, true, InputKind::Pointer), Route::Egui);
        assert_eq!(
            route(false, false, false, InputKind::Pointer),
            Route::Terminal
        );
    }

    /// T8 / 设计 D23:文件面板拿到焦点时,键盘事件走面板这条路,
    /// **绝不先喂 egui**。喂了的后果与 T8 原案一模一样:egui 的焦点系统
    /// 在 `begin_pass` 里看到 Tab 就把焦点给菜单栏,`wants_keyboard_input()`
    /// 从此恒真,终端和面板**双双**收不到任何键。
    ///
    /// 注意第三个参数给 `true`(假装 egui 想要键盘)—— 这正是坏掉时的现场,
    /// 给 `false` 的话实现写错了也能蒙对。
    #[test]
    fn panel_keyboard_is_never_fed_to_egui_so_tab_cannot_steal_focus() {
        assert!(!egui_should_see_focused(
            Focus::FilesPanel,
            InputKind::Keyboard,
            false,
            true
        ));
        assert_eq!(
            route_focused(Focus::FilesPanel, false, true, false, InputKind::Keyboard),
            Route::FilesPanel
        );
    }

    /// 模态弹窗压过面板焦点 —— 会话管理器开着的时候按 Enter 必须是
    /// 「保存」,不是「进目录」。
    #[test]
    fn a_modal_outranks_panel_focus() {
        assert_eq!(
            route_focused(Focus::FilesPanel, true, false, false, InputKind::Keyboard),
            Route::Egui
        );
        assert!(egui_should_see_focused(
            Focus::FilesPanel,
            InputKind::Keyboard,
            true,
            false
        ));
    }

    /// 焦点在终端时,面板一个键都不截 —— 否则在 tmux 里按 F5 会莫名其妙
    /// 刷新文件列表。这条是上面那条的反面,少了它「恒返回 FilesPanel」
    /// 的实现也能全绿。
    #[test]
    fn terminal_focus_leaves_every_key_to_the_terminal() {
        assert_eq!(
            route_focused(Focus::Terminal, false, false, false, InputKind::Keyboard),
            Route::Terminal
        );
    }

    /// 指针不受面板焦点影响:仍是「先喂后判」,否则菜单/弹窗点不动。
    #[test]
    fn pointer_events_still_reach_egui_regardless_of_panel_focus() {
        assert!(egui_should_see_focused(
            Focus::FilesPanel,
            InputKind::Pointer,
            false,
            false
        ));
    }

    /// F6 换焦点。**不用 `Ctrl+Tab`** —— 那个是标签页的(D0 已占)。
    #[test]
    fn f6_toggles_focus_between_terminal_and_panel() {
        assert_eq!(Focus::Terminal.toggled(), Focus::FilesPanel);
        assert_eq!(Focus::FilesPanel.toggled(), Focus::Terminal);
    }

    /// F129:**AltGr 不许被当成 Ctrl+D**。Windows 把 AltGr 合成成
    /// Left-Ctrl + Right-Alt,所以「按住 Ctrl 且键是 d」这条判据在 Windows 上
    /// 会被 AltGr+D 命中 —— 而 Windows 11 是本项目唯一的一等公民。
    /// 后果不是丢一个字符,是丢一块分屏(最后一块时连整个标签一起收走)。
    ///
    /// 自证会变红:把 `is_bare_ctrl_d` 里的 `!mods.alt` 删掉。
    #[test]
    fn altgr_is_not_ctrl_d_because_windows_synthesizes_it_as_ctrl_plus_alt() {
        let altgr = Mods {
            ctrl: true,
            alt: true,
            ..Default::default()
        };
        assert!(
            !is_bare_ctrl_d(Key::Char('d'), altgr),
            "AltGr+D 被当成了 Ctrl+D —— 用户在断开的 pane 上打字会丢分屏"
        );
        let win = Mods {
            ctrl: true,
            sup: true,
            ..Default::default()
        };
        assert!(!is_bare_ctrl_d(Key::Char('d'), win));
    }

    /// F129:裸 Ctrl+D(含大写)要认得出来,别把 `is_bare_ctrl_d` 收得太紧
    /// 以至于谁都不匹配 —— 那样 F129 整个功能静默失效,上面那条排除测试
    /// 还是绿的。
    ///
    /// 自证会变红:把 `is_bare_ctrl_d` 的函数体改成 `false`。
    #[test]
    fn bare_ctrl_d_is_recognized_in_either_case() {
        let ctrl = Mods {
            ctrl: true,
            ..Default::default()
        };
        assert!(is_bare_ctrl_d(Key::Char('d'), ctrl));
        assert!(is_bare_ctrl_d(Key::Char('D'), ctrl));
        // Ctrl+Shift+D 不是它 —— 那一带是 F18 的划选/粘贴热键区。
        assert!(!is_bare_ctrl_d(
            Key::Char('d'),
            Mods {
                ctrl: true,
                shift: true,
                ..Default::default()
            }
        ));
        // 别的键、以及没按 Ctrl 的 d,都不是。
        assert!(!is_bare_ctrl_d(Key::Char('c'), ctrl));
        assert!(!is_bare_ctrl_d(Key::Char('d'), Mods::default()));
    }

    /// F129:**活着的 pane 上 Ctrl+D 永远是 EOF**。这个语义不能动 ——
    /// 它是 shell 退出登录、给 `cat`/`ssh` 收尾的标准键,改掉的话用户
    /// 在正常会话里按 Ctrl+D 会莫名其妙丢一块分屏。
    ///
    /// 自证会变红:把 `ctrl_d_action` 里 `PaneStatus::Live` 那一支删掉。
    #[test]
    fn ctrl_d_on_a_live_pane_is_always_eof() {
        assert_eq!(
            ctrl_d_action(PaneStatus::Live, true),
            CtrlD::SendEof,
            "最后一块活 pane 上也是 EOF"
        );
        assert_eq!(ctrl_d_action(PaneStatus::Live, false), CtrlD::SendEof);
    }

    /// F129:断开的 pane 上 Ctrl+D 关掉这块分屏 —— 断了之后 EOF 送不出去,
    /// 这个键在那儿本来就是废的。重连中的也算「断开」:用户按这个键的意思
    /// 就是「别等了,收掉」。
    #[test]
    fn ctrl_d_on_a_dead_pane_closes_it() {
        assert_eq!(
            ctrl_d_action(PaneStatus::Disconnected, false),
            CtrlD::ClosePane
        );
        assert_eq!(
            ctrl_d_action(PaneStatus::Reconnecting, false),
            CtrlD::ClosePane
        );
    }

    /// T7 变种:`RedrawRequested` **绝不能**喂给 egui。egui-winit 对它恒返回
    /// `repaint: true`,而我们收到 `repaint` 就 `request_redraw()` —— 下一个
    /// `RedrawRequested` 立刻又来,事件循环永远有 pending redraw,空闲时一整个
    /// 单核烧在空转上(v0.1.68 实机:4.8 万次/秒)。
    ///
    /// 自证会变红:把 `egui_should_see_window_event` 的函数体改成 `true`。
    #[test]
    fn egui_never_sees_redraw_requested_or_every_frame_asks_for_the_next_one() {
        assert!(
            !egui_should_see_window_event(&winit::event::WindowEvent::RedrawRequested),
            "RedrawRequested 喂进了 egui —— 它恒返回 repaint:true,自激回环"
        );
    }

    /// 反向:其余「可能需要重绘」的窗口事件必须照喂 —— 它们是真的状态变化,
    /// 各来一次,不构成回环。少喂一种的症状各不相同且全是静默的:`Resized`
    /// 不喂 egui 的布局停在旧尺寸,`Focused` 不喂则失焦后控件仍显示为激活。
    ///
    /// 自证会变红:把 `egui_should_see_window_event` 的函数体改成 `false`。
    #[test]
    fn the_other_repaint_worthy_window_events_still_reach_egui() {
        use winit::event::WindowEvent as WE;
        for ev in [
            WE::Resized(winit::dpi::PhysicalSize::new(1920, 1080)),
            WE::Focused(true),
            WE::Occluded(true),
            WE::CloseRequested,
        ] {
            assert!(
                egui_should_see_window_event(&ev),
                "{ev:?} 没喂给 egui —— egui 的状态会静默停在旧值"
            );
        }
    }

    /// F246:编辑器开着时,指针仍然落到终端 —— 否则背后 pane 里的文字
    /// 一个字都划不动,而要往文件里写的内容多半正是从那儿复制的。
    ///
    /// 第一个参数给 `true`(编辑器确实是模态,T8 要求的)才是坏掉时的现场;
    /// 给 `false` 的话实现写错了也能蒙对。
    ///
    /// 自证会变红:把 `pointer_route` 里的 `&& !editor_open` 删掉。
    #[test]
    fn an_open_editor_does_not_swallow_pointer_events_meant_for_the_pane() {
        assert_eq!(
            pointer_route(true, true, false, false),
            Route::Terminal,
            "编辑器开着就划不动 pane 了(F246)"
        );
        // 指针落在编辑器窗口自己身上时照旧归 egui。
        assert_eq!(pointer_route(true, true, false, true), Route::Egui);
    }

    /// F246 的反面:**其余模态照旧全吃指针**。少了这一条,「编辑器豁免」
    /// 写成「所有模态都豁免」也全绿 —— 而那意味着会话管理器开着时点一下
    /// 背后的终端就开始划选,右键还会直接粘贴到远端。
    ///
    /// 自证会变红:把 `pointer_route` 里的 `if modal_open && !editor_open`
    /// 整段删掉。
    #[test]
    fn every_other_modal_still_swallows_the_pointer() {
        assert_eq!(pointer_route(true, false, false, false), Route::Egui);
    }

    /// F246/T13:按住左键划选期间,指针被终端**捕获**。拖过编辑器窗口时
    /// `egui_wants_pointer` 会变真,不捕获的话选区当场冻住,而且松开那一下
    /// 也判给 egui —— `dragging` 和 F212 的 hold 永远还不回来,这个 pane
    /// 的选区从此再也擦不掉。
    ///
    /// 捕获必须压过模态:拖到一半冒出来的模态截胡 `Released` 是同一个死法。
    ///
    /// 自证会变红:把 `pointer_route` 开头的 `if dragging` 那段删掉。
    #[test]
    fn a_drag_in_progress_keeps_the_pointer_no_matter_who_wants_it() {
        assert_eq!(pointer_route(false, true, true, true), Route::Terminal);
        assert_eq!(
            pointer_route(true, false, true, true),
            Route::Terminal,
            "拖拽中途冒出的模态截胡了 Released —— F212 的 hold 会挂死(T13)"
        );
    }

    /// F246:没有任何模态时,指针分流与 [`route`] 逐字一致 —— 这条防的是
    /// 「顺手把 `pointer_route` 写成恒返回 Terminal」,那样菜单和状态栏
    /// 全都点不动。
    #[test]
    fn with_no_modal_the_pointer_still_follows_what_egui_wants() {
        assert_eq!(pointer_route(false, false, false, true), Route::Egui);
        assert_eq!(pointer_route(false, false, false, false), Route::Terminal);
    }

    /// F246/T8:划选结束后**只在编辑器开着时**把 egui 焦点还回去。
    /// 编辑器没开还的话,等于凭空把键盘焦点塞给一个 egui 部件,
    /// `wants_keyboard_input()` 从此恒真,终端永久收不到任何键。
    ///
    /// 自证会变红:把 `focus_to_restore` 的函数体改成 `saved`。
    #[test]
    fn focus_is_only_handed_back_while_the_editor_is_open() {
        assert_eq!(focus_to_restore(true, Some(7)), Some(7));
        assert_eq!(
            focus_to_restore(false, Some(7)),
            None,
            "编辑器没开也还焦点 —— 终端会永久收不到键(T8)"
        );
    }

    /// F129:断开的 pane 是标签里最后一块时,关掉整个标签。
    /// `Workspace::close_pane` 本来就拒绝关最后一块(F31),不特判的话
    /// 用户按下去**什么都不会发生**,只能去菜单里找「断开」。
    ///
    /// 自证会变红:把 `is_last` 那个参数在函数体里忽略掉。
    #[test]
    fn ctrl_d_on_the_last_dead_pane_closes_the_whole_tab() {
        assert_eq!(
            ctrl_d_action(PaneStatus::Disconnected, true),
            CtrlD::CloseTab
        );
    }
}
