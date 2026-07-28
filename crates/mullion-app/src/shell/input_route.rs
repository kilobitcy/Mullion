//! 输入分流决策(spec §4.5)。egui 的 `consumed` 在「无控件聚焦」时不足以保证方向键/
//! 快捷键回到终端——顶栏/菜单可能间歇抢键(踩 T5/T6)。故显式按这张真值表决定:
//! 有模态→全给 egui;否则按事件类型看 egui 是否真的要这类输入,不要就回终端原路。
//! A2b 用 `egui_ctx.wants_keyboard_input()` / `wants_pointer_input()` 填这两个布尔。

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum InputKind {
    Keyboard,
    Pointer,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Route {
    /// 交给 egui(菜单/状态栏/模态弹窗/表单)。
    Egui,
    /// 交给终端原路:键盘走 keymap+PtyWrite、鼠标走 SGR 上报(Shift 逃生门 T5)。
    Terminal,
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
}
