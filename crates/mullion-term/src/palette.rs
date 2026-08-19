//! xterm-256 默认调色板 + 颜色解析(F10)。
//!
//! 核 alacritty 0.26 源码后确认:`Term::colors()` 返回的 `Colors([Option<Rgb>;269])`
//! 默认**全为 None**,只装 OSC-4 运行时覆盖,不含任何默认 RGB。默认 RGB 归我们所有 ——
//! 这里就是那张表。解析优先用覆盖,否则查默认表。这层是纯函数,是干净的可测面。

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor, Rgb as AnsiRgb};

use crate::snapshot::Rgb;

/// 16 个标准 ANSI 颜色。索引 0..16。
///
/// 取值 = **Campbell**,Windows Terminal / pwsh 的出厂配色。原来用的是经典
/// xterm 默认值,它的 blue `#0000EE` 在深色终端底上对比度只有 1.9:1 —— 深色屏
/// 上是一团几乎读不出来的墨块,这正是用户报的「深色底 + 深色字看不清」。
///
/// 值抄自 `microsoft/terminal` 的 `TerminalSettingsModel/defaults.json`
/// (2026-08-19 拉取核实,不是凭记忆写的)。
///
/// **换这张表本身修不好那个问题**:Campbell 的 blue `#0037DA` 对比度也才 2.2:1。
/// 真正让 pwsh 里 `ls` 的目录名可读的是 `bold_brighten` 那条规则 —— 两件事
/// 必须一起做,见那个函数的文档。
///
/// 取舍:Campbell 比 xterm 默认更柔和,少数**非 bold** 的裸基色反而更暗
/// (magenta 从 3.9:1 降到 2.3:1,red 3.1→3.0)。接受,因为用户点名的参照物就是
/// pwsh,而实际会用到裸 magenta 当正文的场景极少 —— 常见的高亮用法(PS1、
/// `ls`、grep)都带 bold,走的是提亮那条路。
const ANSI16: [Rgb; 16] = [
    Rgb::new(0x0c, 0x0c, 0x0c), // 0  black
    Rgb::new(0xc5, 0x0f, 0x1f), // 1  red
    Rgb::new(0x13, 0xa1, 0x0e), // 2  green
    Rgb::new(0xc1, 0x9c, 0x00), // 3  yellow
    Rgb::new(0x00, 0x37, 0xda), // 4  blue
    Rgb::new(0x88, 0x17, 0x98), // 5  magenta
    Rgb::new(0x3a, 0x96, 0xdd), // 6  cyan
    Rgb::new(0xcc, 0xcc, 0xcc), // 7  white
    Rgb::new(0x76, 0x76, 0x76), // 8  bright black
    Rgb::new(0xe7, 0x48, 0x56), // 9  bright red
    Rgb::new(0x16, 0xc6, 0x0c), // 10 bright green
    Rgb::new(0xf9, 0xf1, 0xa5), // 11 bright yellow
    Rgb::new(0x3b, 0x78, 0xff), // 12 bright blue
    Rgb::new(0xb4, 0x00, 0x9e), // 13 bright magenta
    Rgb::new(0x61, 0xd6, 0xd6), // 14 bright cyan
    Rgb::new(0xf2, 0xf2, 0xf2), // 15 bright white
];

/// 默认前景 / 背景的**出厂值**(无注入、无 OSC 覆盖时)。
pub const DEFAULT_FG: Rgb = Rgb::new(0xcc, 0xcc, 0xcc);
pub const DEFAULT_BG: Rgb = Rgb::new(0x00, 0x00, 0x00);

/// 一对可注入的默认前景/背景色(F80)。
///
/// 「默认前景/背景」本就是 VT 协议概念——SGR 39/49 说的是它,OSC 10/11 改的也是它,
/// 所以它归 term 所有。app 层的主题只是**注入**一组值进来,方向仍是 app → term:
/// 这里只出现 term 自己的 `Rgb`,没有任何 UI 类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DefaultColors {
    pub fg: Rgb,
    pub bg: Rgb,
}

impl Default for DefaultColors {
    fn default() -> Self {
        Self {
            fg: DEFAULT_FG,
            bg: DEFAULT_BG,
        }
    }
}

/// 256 色索引 → 默认 RGB(0..16 ANSI;16..232 6×6×6 立方;232..256 灰阶)。
pub fn indexed_default(i: u8) -> Rgb {
    match i {
        0..=15 => ANSI16[i as usize],
        16..=231 => {
            let n = i - 16;
            let comp = |c: u8| -> u8 {
                if c == 0 {
                    0
                } else {
                    c * 40 + 55
                }
            };
            Rgb::new(comp(n / 36), comp((n / 6) % 6), comp(n % 6))
        }
        232..=255 => {
            let v = (i - 232) * 10 + 8;
            Rgb::new(v, v, v)
        }
    }
}

/// SGR 1(bold)对**前景基色**的效果:提到对应的亮色。
///
/// 这是 xterm 的老规矩,也是 Windows Terminal / pwsh 的出厂默认
/// (`intenseTextStyle: "bright"`) —— 用户点名要参照的就是它。
///
/// **为什么非做不可**:`ls` 的目录色和绝大多数 PS1 用的是 `01;34`
/// (bold + blue)。不映射的话它落到 ANSI blue,在深色终端底上对比度 2.2:1,
/// 基本读不出来;提到 bright blue 之后是 4.6:1,过 WCAG AA 正文线。换调色板
/// 治不了这一条(两套调色板的 blue 都在 2:1 上下),只有这条规则能治。
///
/// **只作用于 `Named`**:
/// - `Indexed`(256 色)和 `Spec`(truecolor)是程序精确点名的颜色,擅自提亮
///   等于篡改 —— TUI 自己配的主题会被我们改花。
/// - `Named(Foreground)`(SGR 39 的默认前景)经 `to_bright` 变成
///   `BrightForeground`,而它在 `named_default` 里仍然落 `d.fg`。所以「默认
///   白字加粗」不会变色,只有真的点了名的 8 个基色会提亮。
/// - 背景色**不过这个函数**:`\e[1;44m` 里的 bold 说的是前景,把背景一起提亮
///   会让反色块整体发光。
pub fn bold_brighten(color: AnsiColor) -> AnsiColor {
    match color {
        AnsiColor::Named(n) => AnsiColor::Named(n.to_bright()),
        other => other,
    }
}

fn from_ansi(rgb: AnsiRgb) -> Rgb {
    Rgb::new(rgb.r, rgb.g, rgb.b)
}

fn named_default(named: NamedColor, d: DefaultColors) -> Rgb {
    match named as usize {
        i @ 0..=15 => ANSI16[i],
        256 => d.fg, // Foreground
        257 => d.bg, // Background
        // Cursor/Dim*/BrightForeground/DimForeground 等 MVP 先落默认前景
        // (SGR 90-97 的 8 个 Bright 基色走的是上面 0..=15 的 ANSI16 分支,
        // 不受这里影响)。注意:注入之后这些会**跟着**变成主题前景色
        // (本轮正是想要的,光标与文本同色系)。将来若要单独调光标色,
        // 源头是这一行,不是 Theme。
        _ => d.fg,
    }
}

/// 把一个单元格颜色解析成具体 RGB:OSC 覆盖优先,否则用默认表。
///
/// `d` 是可注入的默认前景/背景(F80 主题色)。不传主题时用 `DefaultColors::default()`。
pub fn resolve(color: AnsiColor, colors: &Colors, d: DefaultColors) -> Rgb {
    match color {
        AnsiColor::Spec(rgb) => from_ansi(rgb),
        AnsiColor::Indexed(i) => match colors[i as usize] {
            Some(over) => from_ansi(over),
            None => indexed_default(i),
        },
        AnsiColor::Named(named) => match colors[named] {
            Some(over) => from_ansi(over),
            None => named_default(named, d),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_red_resolves_to_ansi_red() {
        let colors = Colors::default();
        assert_eq!(
            resolve(
                AnsiColor::Named(NamedColor::Red),
                &colors,
                DefaultColors::default()
            ),
            Rgb::new(0xc5, 0x0f, 0x1f)
        );
    }

    #[test]
    fn indexed_matches_named_for_first_16() {
        let colors = Colors::default();
        assert_eq!(
            resolve(AnsiColor::Indexed(1), &colors, DefaultColors::default()),
            Rgb::new(0xc5, 0x0f, 0x1f)
        );
    }

    #[test]
    fn cube_pure_red_index_196() {
        assert_eq!(indexed_default(196), Rgb::new(255, 0, 0));
    }

    #[test]
    fn grayscale_index_232_is_dark_gray() {
        assert_eq!(indexed_default(232), Rgb::new(8, 8, 8));
    }

    #[test]
    fn spec_passes_through() {
        let colors = Colors::default();
        assert_eq!(
            resolve(
                AnsiColor::Spec(AnsiRgb { r: 1, g: 2, b: 3 }),
                &colors,
                DefaultColors::default()
            ),
            Rgb::new(1, 2, 3)
        );
    }

    #[test]
    fn osc_override_wins_over_default() {
        let mut colors = Colors::default();
        colors[NamedColor::Red] = Some(AnsiRgb {
            r: 10,
            g: 20,
            b: 30,
        });
        assert_eq!(
            resolve(
                AnsiColor::Named(NamedColor::Red),
                &colors,
                DefaultColors::default()
            ),
            Rgb::new(10, 20, 30)
        );
    }

    #[test]
    fn default_fg_bg_are_distinct() {
        let colors = Colors::default();
        assert_eq!(
            resolve(
                AnsiColor::Named(NamedColor::Background),
                &colors,
                DefaultColors::default()
            ),
            DEFAULT_BG
        );
        assert_eq!(
            resolve(
                AnsiColor::Named(NamedColor::Foreground),
                &colors,
                DefaultColors::default()
            ),
            DEFAULT_FG
        );
    }

    #[test]
    fn injected_defaults_replace_factory_values() {
        let colors = Colors::default();
        let d = DefaultColors {
            fg: Rgb::new(0xe4, 0xe6, 0xf0),
            bg: Rgb::new(0x14, 0x16, 0x1f),
        };
        assert_eq!(
            resolve(AnsiColor::Named(NamedColor::Background), &colors, d),
            Rgb::new(0x14, 0x16, 0x1f)
        );
        assert_eq!(
            resolve(AnsiColor::Named(NamedColor::Foreground), &colors, d),
            Rgb::new(0xe4, 0xe6, 0xf0)
        );
    }

    /// 注入只该动默认前景/背景,不该动 ANSI 16 色(那是另一套,F84 才可配)。
    #[test]
    fn injection_does_not_touch_ansi16() {
        let colors = Colors::default();
        let d = DefaultColors {
            fg: Rgb::new(0xe4, 0xe6, 0xf0),
            bg: Rgb::new(0x14, 0x16, 0x1f),
        };
        assert_eq!(
            resolve(AnsiColor::Named(NamedColor::Red), &colors, d),
            Rgb::new(0xc5, 0x0f, 0x1f)
        );
    }

    /// OSC 覆盖(将来的 OSC 10/11)优先级仍高于注入的默认色。
    #[test]
    fn osc_override_still_wins_over_injected_defaults() {
        let mut colors = Colors::default();
        colors[NamedColor::Background] = Some(AnsiRgb { r: 1, g: 2, b: 3 });
        let d = DefaultColors {
            fg: Rgb::new(0xe4, 0xe6, 0xf0),
            bg: Rgb::new(0x14, 0x16, 0x1f),
        };
        assert_eq!(
            resolve(AnsiColor::Named(NamedColor::Background), &colors, d),
            Rgb::new(1, 2, 3)
        );
    }
}
