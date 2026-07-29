//! 输入编码:按键 → 字节,鼠标是否上报。纯逻辑,可脱离窗口单测。
//!
//! 守护陷阱:
//! - T5/F15:按住 Shift 强制走本地划选(鼠标不上报),否则 `/tui fullscreen` 下无法复制。
//! - T6/F14:Shift+Enter 两套编码(Kitty → CSI-u;否则 → `ESC CR`);Ctrl+J 恒 `\n`。

use alacritty_terminal::term::TermMode;

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
/// 功能键(F1..)、Home/End 等后续再扩。
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
    /// 翻页键。裸键转发对端;Shift+PageUp/Down 由 app 截住做本地回溯(F17)。
    PageUp,
    PageDown,
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
        Key::PageUp => b"\x1b[5~".to_vec(),
        Key::PageDown => b"\x1b[6~".to_vec(),
    }
}

/// bracketed paste 的起止标记(DEC 2004)。
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &str = "\x1b[201~";

/// 把一段粘贴文本编码成发往对端的字节(F18)。
///
/// `bracketed` = 远端置了 `TermMode::BRACKETED_PASTE`。开启时用
/// `ESC[200~` / `ESC[201~` 包裹,远端(bash/zsh/Claude Code)据此知道这是
/// 粘贴而非逐键输入,不会把多行内容逐行执行。
///
/// 两件净化:
/// 1. **剔除所有裸 `ESC`(0x1b) 与 `ETX`(0x03) 字节**。只剔完整的
///    `ESC[201~` 挡不住:`str::replace` 是单趟非重叠扫描,剔掉中间那个之后
///    左右残片会贴合成新的完整标记溜出去(见
///    `paste_cannot_reassemble_end_marker_from_leftovers`)。剔光 ESC 之后
///    这个序列根本无从构成。`ETX` 一并剔是因为有些 shell 收到它会错误地
///    终止 bracketed paste(alacritty 上游同此)。
/// 2. **`\r\n` 与 `\n` 统一成 `\r`**。终端里 Enter 是 CR 不是 LF。
///
/// 两步顺序无关:第 1 步只动 ESC/ETX 字节,第 2 步只动 `\r`/`\n` 字节,
/// 字符集不相交,谁先谁后结果一样。
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    let body = text
        .replace("\r\n", "\r")
        .replace('\n', "\r")
        .replace(['\x1b', '\x03'], "");
    if !bracketed {
        return body.into_bytes();
    }
    let mut out = Vec::with_capacity(body.len() + PASTE_START.len() + PASTE_END.len());
    out.extend_from_slice(PASTE_START);
    out.extend_from_slice(body.as_bytes());
    out.extend_from_slice(PASTE_END.as_bytes());
    out
}

/// 这段粘贴内容会在远端产生几次「回车执行」(F18)。
///
/// 必须与 [`encode_paste`] 同源:那边把 `\r\n` 与 `\n` 都归一成 `\r`,而裸 `\r`
/// 原样保留——三种写法在远端都是一次回车。用 `str::lines()` 数会漏掉裸 `\r`,
/// 让确认弹窗**低估**风险,而这个弹窗存在的唯一理由就是准确告知风险。
///
/// 尾随的那一个换行不计:`"ls -la\n"` 只执行一条命令。不这样处理的话,
/// 从浏览器/IDE 复制的单行命令(普遍带尾随换行)每次都会触发多行确认。
pub fn paste_line_count(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    // 与 encode_paste 一致地归一,再剥掉**最后一个**换行(不是全部尾随换行:
    // 中间和末尾的空行都是实打实的回车)。
    let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
    let body = normalized.strip_suffix('\r').unwrap_or(&normalized);
    body.split('\r').count()
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

/// 一次滚轮滚动的处置(F17 §3.2 三档分流)。
///
/// 之所以要分档:alt screen(tmux/vim)在 alacritty 里恒 0 行本地历史,
/// 本地回溯拿不到任何东西,只能把滚轮转成对端认识的东西。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WheelAction {
    /// 本地回溯 scrollback。`lines` 正数 = 往历史(向上)。
    LocalScroll { lines: i32 },
    /// 上报给对端。`col`/`row` 是 1-based 单元格坐标。
    Report {
        button: u8,
        col: u16,
        row: u16,
        sgr: bool,
        /// 连发次数(一次滚轮可能等于多行)。
        count: u16,
    },
    /// 退化成方向键连发(DECSET 1007 / ALTERNATE_SCROLL)。
    ArrowKeys { up: bool, count: u16 },
    /// 什么都不做。
    None,
}

/// 单次滚轮事件允许上报/退化成方向键的最大连发次数。
///
/// `WheelAction::Report`/`ArrowKeys` 的 `count` 字段是 `u16`;修改前这里直接用
/// `lines.unsigned_abs().min(u16::MAX as u32)` 夹到 `u16::MAX`(65535)——类型上限
/// 够不出错,但语义上离谱:一次物理滚轮/触控板动作正常也就是个位数到几十行,
/// 高延迟链路上一次异常大的增量(如触控板惯性滚动被系统放大,或未来接入更高
/// 精度设备)若真顶到 6 万多,app 层会据此重复发送同一段字节六万多遍,瞬间把
/// 对端刷爆。这个常量把上限收紧到贴近真实物理动作的量级。`LocalScroll` 不受
/// 此限——本地回溯不发字节,无害,而且用户可能用惯性滚动一次翻很远,不该被夹。
const MAX_WHEEL_REPORTS: u16 = 64;

/// 滚轮分流决策(F17)。纯函数,可脱离窗口单测。
///
/// `lines` 正数 = 向上(往历史);`cell` 是鼠标所在的 1-based 单元格 `(col, row)`。
///
/// 顺序不能换:Shift 恒优先(T5 同源逃生门,用户必须永远能读历史),
/// 然后才轮到 alt screen 判定。
///
/// 与上游 alacritty 的两处刻意偏离(读代码前先看,别拿"对齐上游"当理由改掉):
///
/// - **Shift 逃生门是本项目自己扩的,上游没有。** 上游 `scroll_terminal`
///   (`alacritty/src/input/mod.rs:760-825`)在进入鼠标上报分支之前完全不看 Shift,
///   `shift_key()` 只出现在更下面的方向键退化分支里——也就是说真实 alacritty 的
///   滚轮上报不受 Shift 影响。这里的 `shift ||` 是我们为 T5 刻意加的:让用户只需
///   记住一条规则「按住 Shift 就能读历史」。**不要**以「和 alacritty 对齐」为由
///   删掉它,那会让全屏 TUI 下永远读不到历史。
/// - **Ctrl/Alt 修饰位不透传,是有意简化,不是漏写。** 上游 `mouse_report`
///   (`alacritty/src/input/mod.rs:541-568`)会把 `shift→+4 / alt→+8 / ctrl→+16`
///   叠加到 button 上;这里的 `Report.button` 永远不含修饰位(签名只收
///   `shift: bool`,且 Shift 已被逃生门吃掉,不会走到这一步)。设计文档 §3.2
///   的签名就是这样定的,后果是 Ctrl+滚轮加速翻页这类用法在对端不可见。
pub fn wheel_action(mode: TermMode, shift: bool, lines: i32, cell: (u16, u16)) -> WheelAction {
    if lines == 0 {
        return WheelAction::None;
    }
    if shift || !mode.contains(TermMode::ALT_SCREEN) {
        return WheelAction::LocalScroll { lines };
    }
    let up = lines > 0;
    let count = lines.unsigned_abs().min(MAX_WHEEL_REPORTS as u32) as u16;
    if mode.intersects(TermMode::MOUSE_MODE) {
        return WheelAction::Report {
            // SGR/X10 通用:滚轮上=64,下=65。
            button: if up { 64 } else { 65 },
            col: cell.0,
            row: cell.1,
            sgr: mode.contains(TermMode::SGR_MOUSE),
            count,
        };
    }
    if mode.contains(TermMode::ALTERNATE_SCROLL) {
        return WheelAction::ArrowKeys { up, count };
    }
    WheelAction::None
}

/// 把一次滚轮上报编码成字节。
///
/// SGR(DECSET 1006):`CSI < b ; col ; row M`,坐标无上限。
/// X10(传统):`CSI M (b+32) (col+32) (row+32)`,每字段一字节,故最大 223;
/// 超出必须夹紧,否则加 32 后溢出成完全不同的坐标。
///
/// 与上游不同的取舍:上游 `normal_mouse_report`
/// (`alacritty/src/input/mod.rs:572-580`)在坐标 `>= 223` 时是 `return;`——
/// 整帧丢弃,什么都不发。这里选择夹紧到边界继续发,是有意取舍:宁可给对端一个
/// 近似(错误)的位置,也不让这次滚动完全消失。代价是远处的滚轮事件会被
/// 对端误判成落在边界单元格上。
pub fn encode_wheel_report(button: u8, col: u16, row: u16, sgr: bool) -> Vec<u8> {
    if sgr {
        format!("\x1b[<{button};{col};{row}M").into_bytes()
    } else {
        let clamp = |v: u16| (v.min(223) as u8) + 32;
        vec![
            0x1b,
            b'[',
            b'M',
            button.saturating_add(32),
            clamp(col),
            clamp(row),
        ]
    }
}

/// 把 `WheelAction::ArrowKeys` 的一次方向退化编码成字节。
///
/// **故意是 SS3(`ESC O A`/`ESC O B`),不是 `encode_key(Key::Up/Down, ..)` 的
/// CSI(`ESC [ A`/`ESC [ B`)。** 依据三点:
/// 1. 上游 alacritty 的同一分支(`alacritty/src/input/mod.rs` `scroll_terminal`,
///    `ALT_SCREEN | ALTERNATE_SCROLL` 且无鼠标模式时的退化)无条件写
///    `0x1b, b'O', line_cmd`,不查 `APP_CURSOR`/DECCKM——`fn scroll_terminal`
///    在第 760 行,`ALTERNATE_SCROLL` 分支在 799-819 行,`push(b'O')` 在 810/816 行
///    (2026-07 对照 alacritty/alacritty master 分支核实)。
/// 2. 标准 xterm terminfo 的 `kcuu1=\EOA`(应用光标键 SS3),`less`/`man` 等按
///    terminfo 认键;发 CSI 形式它们不认得,滚轮在这些程序里会静默失效。
/// 3. 与 `encode_key` 里普通方向键的 CSI 编码**刻意不共用**——那条路径服务
///    「裸方向键转发给对端自己处理」,这里服务「滚轮退化成方向键给不认鼠标协议
///    的全屏程序」,语义不同,场景不同,不应该因为字节像就合并。
///
/// 和 `encode_key` 一样,这条路径也不追踪 DECCKM(与上游一致,上游本就不查)。
pub fn encode_wheel_arrow(up: bool) -> Vec<u8> {
    vec![0x1b, b'O', if up { b'A' } else { b'B' }]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alt(extra: TermMode) -> TermMode {
        TermMode::ALT_SCREEN | extra
    }

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
    fn page_keys_are_csi_tilde_sequences() {
        // F17:裸 PageUp/PageDown 照旧转发给对端(tmux/less 自己有翻页);
        // Shift+PageUp 由 app 层截住做本地回溯,不走编码。
        let m = Mods::default();
        assert_eq!(encode_key(Key::PageUp, m, false), b"\x1b[5~".to_vec());
        assert_eq!(encode_key(Key::PageDown, m, false), b"\x1b[6~".to_vec());
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

    #[test]
    fn shift_forces_local_scroll_so_user_can_read_history() {
        // T5 同源逃生门:即便对端开了鼠标上报,按住 Shift 也必须走本地回溯,
        // 否则 tmux/Claude Code 全屏下用户永远看不到刷过去的历史。
        let m = alt(TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE);
        assert_eq!(
            wheel_action(m, true, 3, (10, 5)),
            WheelAction::LocalScroll { lines: 3 }
        );
    }

    #[test]
    fn primary_screen_wheel_scrolls_locally() {
        // 非 alt screen(普通 shell):滚轮永远是本地回溯。
        assert_eq!(
            wheel_action(TermMode::default(), false, -3, (1, 1)),
            WheelAction::LocalScroll { lines: -3 }
        );
    }

    #[test]
    fn alt_screen_with_mouse_mode_reports_sgr() {
        let m = alt(TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE);
        assert_eq!(
            wheel_action(m, false, 2, (12, 34)),
            WheelAction::Report {
                button: 64,
                col: 12,
                row: 34,
                sgr: true,
                count: 2
            }
        );
        // 向下滚 → button 65。
        assert_eq!(
            wheel_action(m, false, -1, (12, 34)),
            WheelAction::Report {
                button: 65,
                col: 12,
                row: 34,
                sgr: true,
                count: 1
            }
        );
    }

    #[test]
    fn alt_screen_without_mouse_falls_back_to_arrow_keys() {
        // tmux 不开鼠标模式时的常见场景:DECSET 1007 允许把滚轮当方向键,
        // 这样 less/man 之类还能翻页,而不是滚轮完全没反应。
        let m = alt(TermMode::ALTERNATE_SCROLL);
        assert_eq!(
            wheel_action(m, false, 3, (1, 1)),
            WheelAction::ArrowKeys { up: true, count: 3 }
        );
    }

    #[test]
    fn alt_screen_without_alternate_scroll_does_nothing() {
        // 对端明确关了 1007 且不收鼠标 → 什么都别发,乱发方向键会误操作。
        assert_eq!(
            wheel_action(alt(TermMode::NONE), false, 3, (1, 1)),
            WheelAction::None
        );
        // 零增量恒 None。
        assert_eq!(
            wheel_action(TermMode::default(), false, 0, (1, 1)),
            WheelAction::None
        );
    }

    #[test]
    fn wheel_report_encoding_matches_sgr_and_x10() {
        assert_eq!(
            encode_wheel_report(64, 12, 34, true),
            b"\x1b[<64;12;34M".to_vec()
        );
        assert_eq!(
            encode_wheel_report(64, 12, 34, false),
            vec![0x1b, b'[', b'M', 96, 44, 66]
        );
        // X10 每个字段最多 223(255-32),超出必须夹紧,否则字节溢出成乱码。
        assert_eq!(
            encode_wheel_report(64, 500, 500, false),
            vec![0x1b, b'[', b'M', 96, 255, 255]
        );
    }

    #[test]
    fn alt_screen_without_sgr_reports_x10() {
        // 只开 MOUSE_REPORT_CLICK、不开 SGR_MOUSE 是真实存在的组合(老式终端/
        // 未协商 1006 的对端)。sgr 字段必须如实置 false,否则调用方会按 SGR
        // 语法编码,发出的字节协议对端根本认不出来。
        let m = alt(TermMode::MOUSE_REPORT_CLICK);
        assert_eq!(
            wheel_action(m, false, 2, (12, 34)),
            WheelAction::Report {
                button: 64,
                col: 12,
                row: 34,
                sgr: false,
                count: 2
            }
        );
    }

    #[test]
    fn huge_wheel_delta_is_clamped_to_max_reports() {
        // 异常大的 lines(如系统放大过的触控板增量)不能原样透传成 count——
        // 那会让 app 层把同一段字节重复几万遍,瞬间把对端刷爆(附加项复核)。
        let m = alt(TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE);
        assert_eq!(
            wheel_action(m, false, 50_000, (1, 1)),
            WheelAction::Report {
                button: 64,
                col: 1,
                row: 1,
                sgr: true,
                count: MAX_WHEEL_REPORTS,
            }
        );
        let arrow_mode = alt(TermMode::ALTERNATE_SCROLL);
        assert_eq!(
            wheel_action(arrow_mode, false, -50_000, (1, 1)),
            WheelAction::ArrowKeys {
                up: false,
                count: MAX_WHEEL_REPORTS,
            }
        );
    }

    #[test]
    fn max_wheel_reports_is_a_sane_small_number() {
        // 这个常量的用途是防止「一次异常大的滚轮增量」把对端刷爆——它必须钉在
        // 「一次物理滚轮/触控板动作的合理量级」(个位数到几十行),而不能被悄悄
        // 改成一个形同虚设的大数(比如把 64 改成 640,上面 clamp 机制的测试
        // 照样绿,但防刷爆的实际效果就没了)。clippy 认为对 const 值断言是
        // 「值恒定,断言无意义」,用 const 块把它变成编译期检查,意图不变。
        const { assert!(MAX_WHEEL_REPORTS <= 100) };
    }

    #[test]
    fn encode_wheel_arrow_is_ss3_not_csi() {
        // SS3(ESC O A/B),不是 encode_key(Key::Up/Down,..) 的 CSI(ESC [ A/B)——
        // 依据见 encode_wheel_arrow 文档注释(上游 alacritty scroll_terminal 无条件
        // 写 0x1b,b'O',line_cmd;xterm terminfo kcuu1=\EOA;less/man 只认 SS3)。
        // 期望值直接从这两条协议依据推导,不是跑一遍实现拿回填值。
        assert_eq!(encode_wheel_arrow(true), vec![0x1b, b'O', b'A']);
        assert_eq!(encode_wheel_arrow(false), vec![0x1b, b'O', b'B']);
    }

    #[test]
    fn paste_is_bracketed_when_remote_enabled_it() {
        // F18 验收口径(spec.md):开启 bracketed paste 时内容被 ESC[200~ 包裹。
        assert_eq!(encode_paste("ls", true), b"\x1b[200~ls\x1b[201~".to_vec());
    }

    #[test]
    fn paste_is_raw_when_remote_did_not_enable_bracketed() {
        assert_eq!(encode_paste("ls", false), b"ls".to_vec());
    }

    #[test]
    fn paste_strips_embedded_end_marker_so_it_cannot_break_out() {
        // 剔除的是裸 ESC(和 ETX),而不是剔除完整标记——只剔完整标记的话
        // 残片会重新拼出一个(见 paste_cannot_reassemble_end_marker_from_leftovers)。
        // 剔光 ESC 后 "\x1b[201~" 里的 ESC 没了,残留的 "[201~" 是纯文本,
        // 没有前导 ESC,终端不会当 CSI 解析,无害。
        let evil = "safe\x1b[201~rm -rf /";
        assert_eq!(
            encode_paste(evil, true),
            b"\x1b[200~safe[201~rm -rf /\x1b[201~".to_vec()
        );
        // 未开 bracketed 时同样剔除:裸 ESC 留在流里对远端也是危险字节。
        assert_eq!(encode_paste(evil, false), b"safe[201~rm -rf /".to_vec());
    }

    #[test]
    fn paste_cannot_reassemble_end_marker_from_leftovers() {
        // 只剔除完整的 ESC[201~ 是不够的:str::replace 单趟非重叠扫描,
        // 剔掉中间那个之后左右残片会贴合成一个新的完整标记溜出去。
        // 剔掉所有裸 ESC 才是真正封死的做法(alacritty 上游同此)。
        let out = encode_paste("\x1b[20\x1b[201~1~", true);
        let body = &out[PASTE_START.len()..out.len() - PASTE_END.len()];
        assert!(
            !body
                .windows(PASTE_END.len())
                .any(|w| w == PASTE_END.as_bytes()),
            "净化后的正文里重新出现了结束标记 → 粘贴内容可提前脱离 paste 模式"
        );
    }

    #[test]
    fn paste_strips_etx_because_some_shells_mistake_it_for_paste_end() {
        // 上游注释:有些 shell 收到 \x03(ETX / Ctrl-C)会错误地终止 bracketed paste。
        assert_eq!(encode_paste("a\x03b", false), b"ab".to_vec());
    }

    #[test]
    fn paste_normalizes_newlines_to_cr() {
        // 终端里 Enter 是 CR 不是 LF;发 LF 远端 readline 行为会怪
        // (多出空行 / 不执行)。CRLF 也要折成单个 CR,否则每行执行两次。
        assert_eq!(encode_paste("a\r\nb\nc", false), b"a\rb\rc".to_vec());
    }

    #[test]
    fn paste_line_count_ignores_trailing_newline() {
        // 从浏览器/IDE 复制一条命令通常带尾随换行。它只执行一条命令,
        // 不该被当成「多行粘贴」报警——这是 F18 弹窗最高频的误报来源。
        assert_eq!(paste_line_count("ls -la"), 1);
        assert_eq!(paste_line_count("ls -la\n"), 1);
        assert_eq!(paste_line_count("ls -la\r\n"), 1);
    }

    #[test]
    fn paste_line_count_counts_bare_cr_like_encode_paste_does() {
        // `encode_paste` 把 \r\n / \n 都归一成 \r,而裸 \r 原样保留——
        // 三者在远端都是一次回车。用 `str::lines()` 数会漏掉裸 \r,
        // 让弹窗**低估**风险(显示 2 行、实际执行 3 次回车)。
        assert_eq!("a\rb\rc\nd".lines().count(), 2, "lines() 的口径(反例基线)");
        assert_eq!(paste_line_count("a\rb\rc\nd"), 4);
    }

    #[test]
    fn paste_line_count_counts_blank_lines_in_the_middle() {
        // 中间的空行是实打实的一次回车,不能跟尾随换行一样被吃掉。
        assert_eq!(paste_line_count("a\n\nb"), 3);
        // 末尾连续换行只吃掉最后一个:"a\n\n" = 贴完 a、回车、再回车一次空行。
        assert_eq!(paste_line_count("a\n\n"), 2);
    }

    #[test]
    fn paste_line_count_of_empty_is_zero() {
        assert_eq!(paste_line_count(""), 0);
    }

    #[test]
    fn alt_screen_arrow_keys_handles_scroll_down() {
        // 上面的 alt_screen_without_mouse_falls_back_to_arrow_keys 只测了向上
        // (up: true);向下滚是同样常见的真实路径,若 up 字段的取值逻辑被
        // 悄悄改坏(例如符号判断反了),这条分支之前完全没有测试能抓到。
        let m = alt(TermMode::ALTERNATE_SCROLL);
        assert_eq!(
            wheel_action(m, false, -2, (1, 1)),
            WheelAction::ArrowKeys {
                up: false,
                count: 2
            }
        );
    }
}
