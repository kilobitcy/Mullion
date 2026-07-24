//! xterm-256 默认调色板 + 颜色解析(F10)。
//!
//! 核 alacritty 0.26 源码后确认:`Term::colors()` 返回的 `Colors([Option<Rgb>;269])`
//! 默认**全为 None**,只装 OSC-4 运行时覆盖,不含任何默认 RGB。默认 RGB 归我们所有 ——
//! 这里就是那张表。解析优先用覆盖,否则查默认表。这层是纯函数,是干净的可测面。

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor, Rgb as AnsiRgb};

use crate::snapshot::Rgb;

/// 16 个标准 ANSI 颜色(经典 xterm 默认值)。索引 0..16。
const ANSI16: [Rgb; 16] = [
    Rgb::new(0, 0, 0),       // 0  black
    Rgb::new(205, 0, 0),     // 1  red
    Rgb::new(0, 205, 0),     // 2  green
    Rgb::new(205, 205, 0),   // 3  yellow
    Rgb::new(0, 0, 238),     // 4  blue
    Rgb::new(205, 0, 205),   // 5  magenta
    Rgb::new(0, 205, 205),   // 6  cyan
    Rgb::new(229, 229, 229), // 7  white
    Rgb::new(127, 127, 127), // 8  bright black
    Rgb::new(255, 0, 0),     // 9  bright red
    Rgb::new(0, 255, 0),     // 10 bright green
    Rgb::new(255, 255, 0),   // 11 bright yellow
    Rgb::new(92, 92, 255),   // 12 bright blue
    Rgb::new(255, 0, 255),   // 13 bright magenta
    Rgb::new(0, 255, 255),   // 14 bright cyan
    Rgb::new(255, 255, 255), // 15 bright white
];

/// 默认前景 / 背景(无 OSC 覆盖时)。
pub const DEFAULT_FG: Rgb = Rgb::new(0xcc, 0xcc, 0xcc);
pub const DEFAULT_BG: Rgb = Rgb::new(0x00, 0x00, 0x00);

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

fn from_ansi(rgb: AnsiRgb) -> Rgb {
    Rgb::new(rgb.r, rgb.g, rgb.b)
}

fn named_default(named: NamedColor) -> Rgb {
    match named as usize {
        i @ 0..=15 => ANSI16[i],
        256 => DEFAULT_FG, // Foreground
        257 => DEFAULT_BG, // Background
        _ => DEFAULT_FG,   // Bright*/Dim*/Cursor 等 MVP 先落默认前景
    }
}

/// 把一个单元格颜色解析成具体 RGB:OSC 覆盖优先,否则用默认表。
pub fn resolve(color: AnsiColor, colors: &Colors) -> Rgb {
    match color {
        AnsiColor::Spec(rgb) => from_ansi(rgb),
        AnsiColor::Indexed(i) => match colors[i as usize] {
            Some(over) => from_ansi(over),
            None => indexed_default(i),
        },
        AnsiColor::Named(named) => match colors[named] {
            Some(over) => from_ansi(over),
            None => named_default(named),
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
            resolve(AnsiColor::Named(NamedColor::Red), &colors),
            Rgb::new(205, 0, 0)
        );
    }

    #[test]
    fn indexed_matches_named_for_first_16() {
        let colors = Colors::default();
        assert_eq!(resolve(AnsiColor::Indexed(1), &colors), Rgb::new(205, 0, 0));
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
            resolve(AnsiColor::Spec(AnsiRgb { r: 1, g: 2, b: 3 }), &colors),
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
            resolve(AnsiColor::Named(NamedColor::Red), &colors),
            Rgb::new(10, 20, 30)
        );
    }

    #[test]
    fn default_fg_bg_are_distinct() {
        let colors = Colors::default();
        assert_eq!(
            resolve(AnsiColor::Named(NamedColor::Background), &colors),
            DEFAULT_BG
        );
        assert_eq!(
            resolve(AnsiColor::Named(NamedColor::Foreground), &colors),
            DEFAULT_FG
        );
    }
}
