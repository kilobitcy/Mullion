//! winit 键盘事件 → term keymap 的 (Key, Mods)。纯映射,可脱离窗口单测。
//! 编码本身(含 T6 Shift+Enter)在 `mullion_term::keymap::encode_key`,这里只做翻译。

use mullion_term::keymap::{Key, Mods};
use winit::event::KeyEvent;
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

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
