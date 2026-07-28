//! winit 键盘事件 → term keymap 的 (Key, Mods)。纯映射,可脱离窗口单测。
//! 编码本身(含 T6 Shift+Enter)在 `mullion_term::keymap::encode_key`,这里只做翻译。

use mullion_term::keymap::{Key, Mods};
use winit::event::{KeyEvent, MouseScrollDelta};
use winit::keyboard::{Key as WKey, ModifiersState, NamedKey};

/// 把一次 winit 按键事件翻译成 term 的 (Key, Mods);无法映射的键返回 None。
///
/// `KeyEvent::platform_specific` 字段对外部 crate 是 `pub(crate)`,测试里无法
/// 直接构造 `KeyEvent`,因此可测逻辑抽到 [`translate_logical`],这里只做转调。
pub fn translate_key(event: &KeyEvent, mods: ModifiersState) -> Option<(Key, Mods)> {
    translate_logical(&event.logical_key, mods)
}

/// 纯翻译逻辑:接收 `logical_key` 与修饰键状态,返回 term 的 (Key, Mods)。
pub fn translate_logical(logical: &WKey, mods: ModifiersState) -> Option<(Key, Mods)> {
    let m = Mods {
        shift: mods.shift_key(),
        ctrl: mods.control_key(),
        alt: mods.alt_key(),
        sup: mods.super_key(),
    };
    let key = match logical {
        WKey::Named(NamedKey::Enter) => Key::Enter,
        // 空格/常用控制键都作为 NamedKey 送达,不走 Character——早期漏映射导致
        // 空格等「很多键没反应」。
        WKey::Named(NamedKey::Space) => Key::Space,
        WKey::Named(NamedKey::Tab) => Key::Tab,
        WKey::Named(NamedKey::Backspace) => Key::Backspace,
        WKey::Named(NamedKey::Escape) => Key::Escape,
        WKey::Named(NamedKey::Delete) => Key::Delete,
        WKey::Named(NamedKey::ArrowUp) => Key::Up,
        WKey::Named(NamedKey::ArrowDown) => Key::Down,
        WKey::Named(NamedKey::ArrowLeft) => Key::Left,
        WKey::Named(NamedKey::ArrowRight) => Key::Right,
        WKey::Named(NamedKey::PageUp) => Key::PageUp,
        WKey::Named(NamedKey::PageDown) => Key::PageDown,
        WKey::Character(s) => {
            let mut chars = s.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None; // 多字符(IME 合成)MVP 先不处理
            }
            Key::Char(c)
        }
        _ => return None,
    };
    Some((key, m))
}

/// 一次滚轮增量 → 行数(正数 = 向上 / 往历史)。
///
/// `LineDelta` 一格按 3 行(与主流终端一致)。`PixelDelta`(触控板/精密滚轮)按
/// 行高换算,**不足一行也至少给 ±1**——直接截断的话触控板小幅滚动永远无反应。
pub fn wheel_lines(delta: MouseScrollDelta, cell_h: f32) -> i32 {
    match delta {
        MouseScrollDelta::LineDelta(_, y) => (y * 3.0).round() as i32,
        MouseScrollDelta::PixelDelta(p) => {
            let h = if cell_h > 0.0 { cell_h } else { 1.0 };
            let raw = p.y as f32 / h;
            let n = raw.trunc() as i32;
            if n != 0 {
                n
            } else if raw > 0.0 {
                1
            } else if raw < 0.0 {
                -1
            } else {
                0
            }
        }
    }
}

/// 指针物理像素坐标 → 1-based 终端单元格 `(col, row)`,夹紧在 `dims` 内。
///
/// 不减菜单栏高度:终端文字层就是从窗口原点开始画的(`text.rs` 的
/// `top: row * cell_h`),这里必须用同一套坐标系,否则上报的行号会整体偏移。
pub fn cell_at(px: (f32, f32), cell: (f32, f32), dims: (u16, u16)) -> (u16, u16) {
    let cw = if cell.0 > 0.0 { cell.0 } else { 1.0 };
    let ch = if cell.1 > 0.0 { cell.1 } else { 1.0 };
    let col = (px.0 / cw).floor().max(0.0) as u32 + 1;
    let row = (px.1 / ch).floor().max(0.0) as u32 + 1;
    (
        col.min(dims.0.max(1) as u32) as u16,
        row.min(dims.1.max(1) as u32) as u16,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::dpi::PhysicalPosition;
    use winit::event::MouseScrollDelta;

    #[test]
    fn enter_maps_to_key_enter() {
        let (key, mods) =
            translate_logical(&WKey::Named(NamedKey::Enter), ModifiersState::SHIFT).unwrap();
        assert_eq!(key, Key::Enter);
        assert!(mods.shift);
    }

    #[test]
    fn char_maps_to_key_char() {
        let (key, _) =
            translate_logical(&WKey::Character("a".into()), ModifiersState::empty()).unwrap();
        assert_eq!(key, Key::Char('a'));
    }

    #[test]
    fn multichar_ime_is_ignored() {
        // 多字符(输入法合成)MVP 先不当按键处理,交给后续 IME 支持。
        assert!(
            translate_logical(&WKey::Character("ab".into()), ModifiersState::empty()).is_none()
        );
    }

    #[test]
    fn space_named_key_maps_to_space() {
        // 回归:空格是 NamedKey::Space,不是 Character(" ")。早期漏了这条 → 空格没反应。
        let (key, _) =
            translate_logical(&WKey::Named(NamedKey::Space), ModifiersState::empty()).unwrap();
        assert_eq!(key, Key::Space);
    }

    #[test]
    fn common_named_keys_are_mapped() {
        let e = ModifiersState::empty();
        let m = |n| translate_logical(&WKey::Named(n), e).map(|(k, _)| k);
        assert_eq!(m(NamedKey::Tab), Some(Key::Tab));
        assert_eq!(m(NamedKey::Backspace), Some(Key::Backspace));
        assert_eq!(m(NamedKey::Escape), Some(Key::Escape));
        assert_eq!(m(NamedKey::Delete), Some(Key::Delete));
        assert_eq!(m(NamedKey::ArrowUp), Some(Key::Up));
        assert_eq!(m(NamedKey::ArrowDown), Some(Key::Down));
        assert_eq!(m(NamedKey::ArrowLeft), Some(Key::Left));
        assert_eq!(m(NamedKey::ArrowRight), Some(Key::Right));
        assert_eq!(m(NamedKey::PageUp), Some(Key::PageUp));
        assert_eq!(m(NamedKey::PageDown), Some(Key::PageDown));
    }

    #[test]
    fn line_delta_is_three_lines_per_notch() {
        assert_eq!(wheel_lines(MouseScrollDelta::LineDelta(0.0, 1.0), 16.0), 3);
        assert_eq!(
            wheel_lines(MouseScrollDelta::LineDelta(0.0, -2.0), 16.0),
            -6
        );
    }

    #[test]
    fn small_pixel_delta_still_scrolls_at_least_one_line() {
        // 触控板一次只送几个像素;截断成 0 的话触控板永远滚不动。
        let tiny = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 3.0));
        assert_eq!(wheel_lines(tiny, 16.0), 1);
        let tiny_down = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -3.0));
        assert_eq!(wheel_lines(tiny_down, 16.0), -1);
        // 大增量按行高换算。
        let big = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 48.0));
        assert_eq!(wheel_lines(big, 16.0), 3);
        // 镜像:大幅负增量同样按行高换算,符号不能翻——方向搞反是这类代码
        // 最常见的 bug,且在无头环境里靠人眼滚一下才能发现,必须靠测试钉住。
        let big_down = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -48.0));
        assert_eq!(wheel_lines(big_down, 16.0), -3);
    }

    #[test]
    fn cell_at_is_one_based_and_clamped() {
        // 鼠标上报的坐标是 1-based,且必须夹在网格内——越界坐标会让对端 TUI 误判。
        assert_eq!(cell_at((0.0, 0.0), (8.0, 16.0), (80, 24)), (1, 1));
        assert_eq!(cell_at((23.0, 33.0), (8.0, 16.0), (80, 24)), (3, 3));
        assert_eq!(
            cell_at((10_000.0, 10_000.0), (8.0, 16.0), (80, 24)),
            (80, 24)
        );
        assert_eq!(cell_at((-5.0, -5.0), (8.0, 16.0), (80, 24)), (1, 1));
    }
}
