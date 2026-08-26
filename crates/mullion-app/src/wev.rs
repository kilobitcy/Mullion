//! F171:窗口事件类型归因 —— 「这一次重绘请求是**哪一类**窗口事件带来的」。
//!
//! F157 的 `dirty=行号:次数` 只能答到 `app.rs` 里 `egui_state.on_window_event`
//! 返回 `repaint: true` 的那一行,再往上一级就断了:实机日志里那一行每 5 秒
//! 响 158 次,而用户**根本没碰键鼠**(`in=0B/s key=0x`,`present=1`)。要接着
//! 往上查,必须知道是哪类事件让 egui 说「我要重绘」。
//!
//! # 为什么 match 里不许写 `_`
//!
//! `winit::event::WindowEvent`(0.30.13)**不是** `#[non_exhaustive]`,所以
//! 穷尽 match 在 winit 升版加档时是**编译错误**。这正是本项目反复踩过的
//! 「列举式门控在加档时必然漏」的根治手段 —— 写了 `_` 就退化成静默漏档:
//! 新事件全落进一个笼统的桶,而这张表存在的意义恰恰是「别把人带去改错
//! 地方」。加档时该做的是给它一个码和一个名字,不是让它落进兜底。
//!
//! 因此 `wev=` 里出现的 `other:N` **只有一个含义**:活跃事件类型超过了
//! `diag::TABLE_SLOTS` 个槽位,与「有档没登记」无关。

use winit::event::WindowEvent;

/// 事件类型码 → 日志短名。
///
/// **第 0 位是占位,永不作为任何事件的码**:`diag::KeyTable` 拿 `key == 0`
/// 当空槽标记,若某一类事件编码成 0,它的计数会被后来者反复当成空槽顶掉
/// —— 静默丢数,且日志上与「这类事件一次没来」长得一模一样。
///
/// 名字全 ASCII 且尽量短:这一段要塞进已经很长的剖面概览行。
const NAMES: [&str; 29] = [
    "",            // 0:占位,见上
    "activation",  // 1
    "resize",      // 2
    "moved",       // 3
    "close",       // 4
    "destroyed",   // 5
    "dropfile",    // 6
    "hoverfile",   // 7
    "hovercancel", // 8
    "focus",       // 9
    "kbd",         // 10
    "mods",        // 11
    "ime",         // 12
    "cursor",      // 13
    "curenter",    // 14
    "curleave",    // 15
    "wheel",       // 16
    "click",       // 17
    "pinch",       // 18
    "pan",         // 19
    "dbltap",      // 20
    "rotate",      // 21
    "tppressure",  // 22
    "axis",        // 23
    "touch",       // 24
    "scale",       // 25
    "theme",       // 26
    "occluded",    // 27
    "redraw",      // 28
];

const ACTIVATION: u32 = 1;
const RESIZED: u32 = 2;
const MOVED: u32 = 3;
const CLOSE_REQUESTED: u32 = 4;
const DESTROYED: u32 = 5;
const DROPPED_FILE: u32 = 6;
const HOVERED_FILE: u32 = 7;
const HOVERED_FILE_CANCELLED: u32 = 8;
const FOCUSED: u32 = 9;
const KEYBOARD_INPUT: u32 = 10;
const MODIFIERS_CHANGED: u32 = 11;
const IME: u32 = 12;
const CURSOR_MOVED: u32 = 13;
const CURSOR_ENTERED: u32 = 14;
const CURSOR_LEFT: u32 = 15;
const MOUSE_WHEEL: u32 = 16;
const MOUSE_INPUT: u32 = 17;
const PINCH_GESTURE: u32 = 18;
const PAN_GESTURE: u32 = 19;
const DOUBLE_TAP_GESTURE: u32 = 20;
const ROTATION_GESTURE: u32 = 21;
const TOUCHPAD_PRESSURE: u32 = 22;
const AXIS_MOTION: u32 = 23;
const TOUCH: u32 = 24;
const SCALE_FACTOR_CHANGED: u32 = 25;
const THEME_CHANGED: u32 = 26;
const OCCLUDED: u32 = 27;
const REDRAW_REQUESTED: u32 = 28;

/// 这个窗口事件属于哪一类。**不许加 `_` 分支**,理由见模块文档。
pub fn kind_of(e: &WindowEvent) -> u32 {
    match e {
        WindowEvent::ActivationTokenDone { .. } => ACTIVATION,
        WindowEvent::Resized(_) => RESIZED,
        WindowEvent::Moved(_) => MOVED,
        WindowEvent::CloseRequested => CLOSE_REQUESTED,
        WindowEvent::Destroyed => DESTROYED,
        WindowEvent::DroppedFile(_) => DROPPED_FILE,
        WindowEvent::HoveredFile(_) => HOVERED_FILE,
        WindowEvent::HoveredFileCancelled => HOVERED_FILE_CANCELLED,
        WindowEvent::Focused(_) => FOCUSED,
        WindowEvent::KeyboardInput { .. } => KEYBOARD_INPUT,
        WindowEvent::ModifiersChanged(_) => MODIFIERS_CHANGED,
        WindowEvent::Ime(_) => IME,
        WindowEvent::CursorMoved { .. } => CURSOR_MOVED,
        WindowEvent::CursorEntered { .. } => CURSOR_ENTERED,
        WindowEvent::CursorLeft { .. } => CURSOR_LEFT,
        WindowEvent::MouseWheel { .. } => MOUSE_WHEEL,
        WindowEvent::MouseInput { .. } => MOUSE_INPUT,
        WindowEvent::PinchGesture { .. } => PINCH_GESTURE,
        WindowEvent::PanGesture { .. } => PAN_GESTURE,
        WindowEvent::DoubleTapGesture { .. } => DOUBLE_TAP_GESTURE,
        WindowEvent::RotationGesture { .. } => ROTATION_GESTURE,
        WindowEvent::TouchpadPressure { .. } => TOUCHPAD_PRESSURE,
        WindowEvent::AxisMotion { .. } => AXIS_MOTION,
        WindowEvent::Touch(_) => TOUCH,
        WindowEvent::ScaleFactorChanged { .. } => SCALE_FACTOR_CHANGED,
        WindowEvent::ThemeChanged(_) => THEME_CHANGED,
        WindowEvent::Occluded(_) => OCCLUDED,
        WindowEvent::RedrawRequested => REDRAW_REQUESTED,
    }
}

/// 事件类型码 → 短名。码越界报 `?N` 而不是 panic:剖面渲染跑在看门狗线程上,
/// 为一个诊断字段炸掉整个日志线程不划算,而 `?N` 一眼就能看出是码表脱钩。
pub fn name_of(kind: u32) -> String {
    NAMES
        .get(kind as usize)
        .filter(|n| !n.is_empty())
        .map_or_else(|| format!("?{kind}"), |n| (*n).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::dpi::{PhysicalPosition, PhysicalSize};
    use winit::event::DeviceId;

    fn dev() -> DeviceId {
        DeviceId::dummy()
    }

    /// 码表与名字表脱钩时,归因会**指向另一类事件** —— 比没有归因更糟,
    /// 会把人带去改错地方。这条把每个 variant 的码和名字端到端对起来。
    ///
    /// `DeviceId::dummy()` 是 winit 明确为单测提供的构造口,所以指针一族
    /// 也能在无头环境里造出来,不必只测「能廉价构造的那几个」。
    ///
    /// 自证会变红:把 `NAMES` 里任意两项对调。
    #[test]
    fn every_variant_maps_to_its_own_name() {
        let cases: Vec<(WindowEvent, &str)> = vec![
            (WindowEvent::Resized(PhysicalSize::new(1, 1)), "resize"),
            (WindowEvent::Moved(PhysicalPosition::new(1, 1)), "moved"),
            (WindowEvent::CloseRequested, "close"),
            (WindowEvent::Destroyed, "destroyed"),
            (WindowEvent::DroppedFile("/tmp/a".into()), "dropfile"),
            (WindowEvent::HoveredFile("/tmp/a".into()), "hoverfile"),
            (WindowEvent::HoveredFileCancelled, "hovercancel"),
            (WindowEvent::Focused(true), "focus"),
            (WindowEvent::ModifiersChanged(Default::default()), "mods"),
            (
                WindowEvent::CursorMoved {
                    device_id: dev(),
                    position: PhysicalPosition::new(1.0, 2.0),
                },
                "cursor",
            ),
            (WindowEvent::CursorEntered { device_id: dev() }, "curenter"),
            (WindowEvent::CursorLeft { device_id: dev() }, "curleave"),
            (
                WindowEvent::AxisMotion {
                    device_id: dev(),
                    axis: 0,
                    value: 0.0,
                },
                "axis",
            ),
            (
                WindowEvent::TouchpadPressure {
                    device_id: dev(),
                    pressure: 0.0,
                    stage: 0,
                },
                "tppressure",
            ),
            (WindowEvent::DoubleTapGesture { device_id: dev() }, "dbltap"),
            (
                WindowEvent::ThemeChanged(winit::window::Theme::Dark),
                "theme",
            ),
            (WindowEvent::Occluded(true), "occluded"),
            (WindowEvent::RedrawRequested, "redraw"),
        ];
        for (e, want) in cases {
            let got = name_of(kind_of(&e));
            assert_eq!(got, want, "{e:?} 归错类了");
        }
    }

    /// 码 0 是 `KeyTable` 的空槽标记,不许被任何事件占用 —— 占用了那一类的
    /// 计数会被后来者当成空槽反复顶掉,静默丢数。
    ///
    /// 自证会变红:把 `kind_of` 里任意一个 arm 改成返回 0。
    #[test]
    fn no_event_kind_is_zero_because_zero_means_empty_slot() {
        let events = [
            WindowEvent::CloseRequested,
            WindowEvent::Destroyed,
            WindowEvent::HoveredFileCancelled,
            WindowEvent::RedrawRequested,
            WindowEvent::Focused(true),
            WindowEvent::Occluded(false),
            WindowEvent::Resized(PhysicalSize::new(1, 1)),
            WindowEvent::CursorMoved {
                device_id: dev(),
                position: PhysicalPosition::new(0.0, 0.0),
            },
        ];
        for e in events {
            assert_ne!(kind_of(&e), 0, "{e:?} 编成了空槽标记");
        }
        assert!(NAMES[0].is_empty(), "第 0 位必须留空作占位");
    }

    /// 名字必须互不相同:重名的两类在日志里会被读成同一类,而这张表的
    /// 全部价值就是把它们分开。
    #[test]
    fn the_short_names_are_all_distinct() {
        let mut seen = std::collections::HashSet::new();
        for (i, n) in NAMES.iter().enumerate().skip(1) {
            assert!(!n.is_empty(), "第 {i} 位没有名字");
            assert!(seen.insert(*n), "短名 {n} 重复了");
        }
    }

    /// 码表脱钩时报 `?N` 而不是 panic —— 剖面渲染跑在看门狗线程上。
    #[test]
    fn an_unknown_kind_is_reported_rather_than_panicking() {
        assert_eq!(name_of(999), "?999");
        assert_eq!(name_of(0), "?0");
    }

    /// **接线守护**:`kind_of` 的 match 里不许有 `_` 分支。
    ///
    /// 有了 `_`,winit 升版加档就从「编译错误」退化成「静默归进兜底桶」,
    /// 而这正是本项目踩过三次的那一类失效 —— 代码照跑、测试全绿、日志
    /// 看起来一切正常,只有归因悄悄指错了地方。
    ///
    /// 自证会变红:在 `kind_of` 末尾加一句 `_ => 0,`。
    #[test]
    fn the_match_has_no_catch_all_so_a_new_winit_variant_breaks_the_build() {
        let src = include_str!("wev.rs");
        let body = src
            .split("pub fn kind_of(")
            .nth(1)
            .expect("找不到 kind_of 的定义")
            .split("\n}\n")
            .next()
            .expect("找不到 kind_of 的函数体");
        assert!(
            !body.contains("_ =>"),
            "kind_of 里出现了兜底分支,winit 加档将不再编译报错:\n{body}"
        );
    }
}
