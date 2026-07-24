//! 输入编码:按键 → 字节,鼠标是否上报。纯逻辑,可脱离窗口单测。
//!
//! 守护陷阱:
//! - T5/F15:按住 Shift 强制走本地划选(鼠标不上报),否则 `/tui fullscreen` 下无法复制。
//! - T6/F14:Shift+Enter 两套编码(Kitty → CSI-u;否则 → `ESC CR`);Ctrl+J 恒 `\n`。

/// 修饰键状态。骨架只覆盖编码用得到的四个。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    /// super / win / cmd。
    pub sup: bool,
}

/// 可编码的按键。覆盖 shell/tmux/Claude Code 日常所需的常用键;
/// 功能键(F1..)、Home/End/PageUp/Down 等后续再扩。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Enter,
    Char(char),
    /// 空格。winit 作为 `NamedKey::Space` 送达,不是 `Character(" ")`,
    /// 早期只认 Char 时空格无反应——这里单列。
    Space,
    Tab,
    /// 退格:发 DEL(0x7f),现代终端/bash/Claude Code 的约定(非 0x08)。
    Backspace,
    Escape,
    Delete,
    Up,
    Down,
    Left,
    Right,
}

/// 把一次按键编码成发往对端的字节。
///
/// `kitty` 表示对端是否启用了 Kitty 键盘协议(F13);不支持时优雅退化。
pub fn encode_key(key: Key, mods: Mods, kitty: bool) -> Vec<u8> {
    // Ctrl+J 恒为 `\n`,官方保底方案(F14),优先于一切且不受 Kitty 影响。
    if mods.ctrl && matches!(key, Key::Char('j' | 'J')) {
        return vec![b'\n'];
    }
    match key {
        Key::Enter => encode_enter(mods, kitty),
        Key::Char(c) => encode_char(c, mods),
        Key::Space => vec![b' '],
        Key::Tab => vec![b'\t'],
        Key::Backspace => vec![0x7f],
        Key::Escape => vec![0x1b],
        // 方向键/Delete 用普通光标键序列(CSI)。应用光标键模式(DECCKM,
        // 部分全屏 TUI 会开)下应发 ESC O A 等;当前不追踪该模式,后续补。
        Key::Delete => b"\x1b[3~".to_vec(),
        Key::Up => b"\x1b[A".to_vec(),
        Key::Down => b"\x1b[B".to_vec(),
        Key::Right => b"\x1b[C".to_vec(),
        Key::Left => b"\x1b[D".to_vec(),
    }
}

fn encode_enter(mods: Mods, kitty: bool) -> Vec<u8> {
    if mods.shift {
        if kitty {
            // CSI 13 ; 2 u —— Enter 键码 13 + Shift 修饰(1 + 1)。
            b"\x1b[13;2u".to_vec()
        } else {
            // ESC CR —— Claude Code `/terminal-setup` 写入的约定(F14)。
            vec![0x1b, b'\r']
        }
    } else {
        vec![b'\r']
    }
}

fn encode_char(c: char, mods: Mods) -> Vec<u8> {
    if mods.ctrl {
        if let Some(b) = ctrl_byte(c) {
            return vec![b];
        }
    }
    // 骨架:普通可打印字符按 UTF-8 发出(alt/super 前缀等后续再补)。
    let mut buf = [0u8; 4];
    c.encode_utf8(&mut buf).as_bytes().to_vec()
}

/// Ctrl+字母 → C0 控制码(A→0x01 … Z→0x1a),其中 Ctrl+J = 0x0a(`\n`)。
fn ctrl_byte(c: char) -> Option<u8> {
    let up = c.to_ascii_uppercase();
    if up.is_ascii_uppercase() {
        Some((up as u8) & 0x1f)
    } else {
        None
    }
}

/// 鼠标事件是否上报给对端。
///
/// 按住 Shift 时恒 `false`——强制走本地划选,让用户在 `/tui fullscreen`
/// 鼠标捕获下仍能复制(F15,守护陷阱 T5)。这是唯一的逃生门,优先于捕获状态。
pub fn mouse_should_report(mods: Mods, capture_on: bool) -> bool {
    if mods.shift {
        return false;
    }
    capture_on
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shift() -> Mods {
        Mods {
            shift: true,
            ..Default::default()
        }
    }
    fn ctrl() -> Mods {
        Mods {
            ctrl: true,
            ..Default::default()
        }
    }

    #[test]
    fn shift_enter_without_kitty_is_esc_cr() {
        // T6/F14:非 Kitty 下 Shift+Enter 必须是 ESC CR。
        assert_eq!(encode_key(Key::Enter, shift(), false), vec![0x1b, b'\r']);
    }

    #[test]
    fn shift_enter_with_kitty_is_csi_u() {
        // T6/F14:Kitty 下 Shift+Enter 走 CSI-u(13;2u)。
        assert_eq!(
            encode_key(Key::Enter, shift(), true),
            b"\x1b[13;2u".to_vec()
        );
    }

    #[test]
    fn ctrl_j_is_always_newline() {
        // F14:Ctrl+J 恒 `\n`,无论是否 Kitty。
        assert_eq!(encode_key(Key::Char('j'), ctrl(), false), vec![b'\n']);
        assert_eq!(encode_key(Key::Char('j'), ctrl(), true), vec![b'\n']);
        assert_eq!(encode_key(Key::Char('J'), ctrl(), false), vec![b'\n']);
    }

    #[test]
    fn plain_enter_is_cr() {
        assert_eq!(encode_key(Key::Enter, Mods::default(), false), vec![b'\r']);
    }

    #[test]
    fn plain_char_is_utf8() {
        assert_eq!(
            encode_key(Key::Char('a'), Mods::default(), false),
            vec![b'a']
        );
    }

    #[test]
    fn space_is_0x20() {
        // 回归:空格是 NamedKey::Space,早期只认 Char 时无反应。必须发 0x20。
        assert_eq!(encode_key(Key::Space, Mods::default(), false), vec![b' ']);
    }

    #[test]
    fn common_control_keys_encode_to_expected_bytes() {
        let m = Mods::default();
        assert_eq!(encode_key(Key::Tab, m, false), vec![b'\t']); // 0x09
        assert_eq!(encode_key(Key::Backspace, m, false), vec![0x7f]); // DEL
        assert_eq!(encode_key(Key::Escape, m, false), vec![0x1b]);
        assert_eq!(encode_key(Key::Delete, m, false), b"\x1b[3~".to_vec());
    }

    #[test]
    fn arrows_are_csi_cursor_sequences() {
        let m = Mods::default();
        assert_eq!(encode_key(Key::Up, m, false), b"\x1b[A".to_vec());
        assert_eq!(encode_key(Key::Down, m, false), b"\x1b[B".to_vec());
        assert_eq!(encode_key(Key::Right, m, false), b"\x1b[C".to_vec());
        assert_eq!(encode_key(Key::Left, m, false), b"\x1b[D".to_vec());
    }

    #[test]
    fn shift_blocks_mouse_report_so_user_can_copy() {
        // T5/F15:捕获开启时按住 Shift 也不上报,用户才能划选复制。
        assert!(!mouse_should_report(shift(), true));
    }

    #[test]
    fn mouse_reports_when_captured_without_shift() {
        assert!(mouse_should_report(Mods::default(), true));
    }

    #[test]
    fn mouse_silent_when_capture_off() {
        assert!(!mouse_should_report(Mods::default(), false));
    }
}
