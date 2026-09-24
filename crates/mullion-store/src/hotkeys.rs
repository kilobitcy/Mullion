//! F294:本地热键的**组合键表示**。只认结构,不认动作 —— 「哪个动作绑了它」
//! 是 app 的事,这里只负责它长什么样、怎么落盘、怎么显示。
//!
//! # TOML 里是一行字符串
//!
//! 内存里是结构体(四个修饰键 + 键枚举,撞键要按结构比),文件里写成
//! `"ctrl+shift+`"`:修饰键小写、`+` 连接、**末段是键名**。键本身是 `+`
//! 时写 `"ctrl++"` —— 解析先剥修饰键前缀再看剩下的整段,不按 `+` 切分,
//! 所以不会歧义。`settings.toml` 是明文、用户会手改,给人读的形态比嵌套表
//! 值钱。

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// 可绑定的键。**Enter / Esc / Backspace / Space / Delete 不在这里**:它们裸按
/// 有终端语义,带修饰又是终端控制键,没有安全的绑法;需要写在一览表里的
/// 那几处走纯文字。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyName {
    /// 字符键,**只收 ASCII 可见字符且存小写**。
    ///
    /// 「不收非 ASCII 布局键(ö / 西里尔 / 假名)」是决定,不是偷懒,两条理由:
    /// 一是显示文本要过 app 侧的 GBK 字形白名单,链外字形画成豆腐块且静默;
    /// 二是 [`KeyName::Left`] 等的显示文本是 `←`,若 `Char` 收非 ASCII,
    /// `Ctrl+←` 就会反解成 `Char('←')` —— 两个不同的键有同一个显示文本。
    /// 存小写是因为 `Ctrl+Shift+N` 与 `Ctrl+N` 的区别在 `shift` 位,
    /// 不在字母大小写上。
    ///
    /// **直接构造这个变体会绕过归一与校验**,请走 [`KeyName::char`]。
    Char(char),
    /// F1…F12。**直接构造这个变体会绕过范围校验**(`F(99)` 写得出来、读不回),
    /// 请走 [`KeyName::f`]。
    F(u8),
    Tab,
    PageUp,
    PageDown,
    Home,
    End,
    Up,
    Down,
    Left,
    Right,
}

impl KeyName {
    /// 字符键的唯一正路:ASCII 可见字符转小写,其余一律 `None`。
    pub const fn char(c: char) -> Option<Self> {
        if c.is_ascii_graphic() {
            Some(Self::Char(c.to_ascii_lowercase()))
        } else {
            None
        }
    }

    /// 功能键的唯一正路:1..=12,越界一律 `None`。
    pub const fn f(n: u8) -> Option<Self> {
        if matches!(n, 1..=12) {
            Some(Self::F(n))
        } else {
            None
        }
    }

    /// 从 TOML 里的键名段解析。传进来的是已经整体小写过的那一段。
    fn from_token(tok: &str) -> Option<Self> {
        let named = match tok {
            "tab" => Some(Self::Tab),
            "pageup" => Some(Self::PageUp),
            "pagedown" => Some(Self::PageDown),
            "home" => Some(Self::Home),
            "end" => Some(Self::End),
            "up" => Some(Self::Up),
            "down" => Some(Self::Down),
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            _ => None,
        };
        if named.is_some() {
            return named;
        }
        if let Some(n) = tok.strip_prefix('f') {
            if let Ok(n) = n.parse::<u8>() {
                return Self::f(n);
            }
        }
        let mut it = tok.chars();
        let c = it.next()?;
        if it.next().is_some() {
            return None;
        }
        Self::char(c)
    }

    /// TOML 里的键名段。
    fn token(self) -> String {
        match self {
            Self::Char(c) => c.to_string(),
            Self::F(n) => format!("f{n}"),
            Self::Tab => "tab".into(),
            Self::PageUp => "pageup".into(),
            Self::PageDown => "pagedown".into(),
            Self::Home => "home".into(),
            Self::End => "end".into(),
            Self::Up => "up".into(),
            Self::Down => "down".into(),
            Self::Left => "left".into(),
            Self::Right => "right".into(),
        }
    }

    /// 给人看的键名。方向键用箭头(四个都在 GBK 内,app 侧字形白名单已登记,
    /// 守护在 `ui::glyphs::tests::every_key_name_label_only_uses_verified_glyphs`)。
    fn label(self) -> String {
        match self {
            Self::Char(c) => c.to_uppercase().to_string(),
            Self::F(n) => format!("F{n}"),
            Self::Tab => "Tab".into(),
            Self::PageUp => "PageUp".into(),
            Self::PageDown => "PageDown".into(),
            Self::Home => "Home".into(),
            Self::End => "End".into(),
            Self::Up => "↑".into(),
            Self::Down => "↓".into(),
            Self::Left => "←".into(),
            Self::Right => "→".into(),
        }
    }
}

/// 一个组合键。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord {
    /// Ctrl。
    pub ctrl: bool,
    /// Shift。
    pub shift: bool,
    /// Alt(macOS 上是 Option)。
    pub alt: bool,
    /// Windows 键 / macOS 的 Cmd 键。TOML 里写 `super`。
    pub sup: bool,
    /// 主键。
    pub key: KeyName,
}

/// 解析失败:原串原样带回,报错时给用户看。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChordParseError(pub String);

impl fmt::Display for ChordParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "不是合法的组合键:「{}」", self.0)
    }
}

impl std::error::Error for ChordParseError {}

impl Chord {
    /// 不带任何修饰键。
    pub const fn plain(key: KeyName) -> Self {
        Self {
            ctrl: false,
            shift: false,
            alt: false,
            sup: false,
            key,
        }
    }

    /// 解析 TOML 里那一行。首尾空白忽略;大小写不敏感;修饰键顺序随意、
    /// 重复写也只归一成一个布尔位(`"ctrl+ctrl+n"` 等于 `"ctrl+n"`);剥完
    /// 修饰键前缀剩下的**整段**是键名(所以 `"ctrl++"` 的键是 `+`)。
    pub fn parse(s: &str) -> Result<Self, ChordParseError> {
        let lower = s.trim().to_lowercase();
        let mut rest = lower.as_str();
        let (mut ctrl, mut shift, mut alt, mut sup) = (false, false, false, false);
        loop {
            if let Some(r) = rest.strip_prefix("ctrl+") {
                ctrl = true;
                rest = r;
            } else if let Some(r) = rest.strip_prefix("shift+") {
                shift = true;
                rest = r;
            } else if let Some(r) = rest.strip_prefix("alt+") {
                alt = true;
                rest = r;
            } else if let Some(r) = rest.strip_prefix("super+") {
                sup = true;
                rest = r;
            } else {
                break;
            }
        }
        let key = KeyName::from_token(rest).ok_or_else(|| ChordParseError(s.to_string()))?;
        Ok(Self {
            ctrl,
            shift,
            alt,
            sup,
            key,
        })
    }

    /// TOML 里的写法。与 [`Self::parse`] 互逆。
    pub fn canonical(self) -> String {
        let mut out = String::new();
        if self.ctrl {
            out.push_str("ctrl+");
        }
        if self.shift {
            out.push_str("shift+");
        }
        if self.alt {
            out.push_str("alt+");
        }
        if self.sup {
            out.push_str("super+");
        }
        out.push_str(&self.key.token());
        out
    }

    /// 给人看的写法:`Ctrl+Shift+N` / `F6` / `Ctrl+←`。一览表与设置里的
    /// 显示文本**全部由这里生成**,不再手抄。
    pub fn display(self) -> String {
        let mut out = String::new();
        if self.ctrl {
            out.push_str("Ctrl+");
        }
        if self.shift {
            out.push_str("Shift+");
        }
        if self.alt {
            out.push_str("Alt+");
        }
        if self.sup {
            out.push_str("Super+");
        }
        out.push_str(&self.key.label());
        out
    }
}

impl Serialize for Chord {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.canonical())
    }
}

impl<'de> Deserialize<'de> for Chord {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctrl_shift(key: KeyName) -> Chord {
        Chord {
            ctrl: true,
            shift: true,
            alt: false,
            sup: false,
            key,
        }
    }

    /// 九个具名键,给遍历用。
    const NAMED: [KeyName; 9] = [
        KeyName::Tab,
        KeyName::PageUp,
        KeyName::PageDown,
        KeyName::Home,
        KeyName::End,
        KeyName::Up,
        KeyName::Down,
        KeyName::Left,
        KeyName::Right,
    ];

    /// 解析 ↔ canonical 互逆,且大小写 / 顺序 / 首尾空白都归一。
    ///
    /// 自证会变红:把 `parse` 里 `to_lowercase()` 去掉(第二条红)。
    #[test]
    fn parse_and_canonical_are_inverse_and_case_insensitive() {
        for (text, want) in [
            ("ctrl+shift+`", ctrl_shift(KeyName::Char('`'))),
            ("CTRL+SHIFT+N", ctrl_shift(KeyName::Char('n'))),
            ("shift+ctrl+n", ctrl_shift(KeyName::Char('n'))),
            (
                "  ctrl+n  ",
                Chord {
                    ctrl: true,
                    ..Chord::plain(KeyName::Char('n'))
                },
            ),
            ("f6", Chord::plain(KeyName::F(6))),
            (
                "ctrl+tab",
                Chord {
                    ctrl: true,
                    ..Chord::plain(KeyName::Tab)
                },
            ),
            (
                "alt+left",
                Chord {
                    alt: true,
                    ..Chord::plain(KeyName::Left)
                },
            ),
            (
                "super+pageup",
                Chord {
                    sup: true,
                    ..Chord::plain(KeyName::PageUp)
                },
            ),
        ] {
            let got = Chord::parse(text).unwrap_or_else(|e| panic!("{text}:{e}"));
            assert_eq!(got, want, "{text}");
            assert_eq!(
                Chord::parse(&got.canonical()).unwrap(),
                got,
                "canonical 不可逆:{text}"
            );
        }
    }

    /// 键本身是 `+` 的写法:`"ctrl++"`。这是「不按 `+` 切分」的全部理由。
    #[test]
    fn a_plus_key_is_written_as_a_trailing_plus() {
        let c = Chord::parse("ctrl++").unwrap();
        assert_eq!(c.key, KeyName::Char('+'));
        assert!(c.ctrl);
        assert_eq!(c.canonical(), "ctrl++");
    }

    /// 残缺 / 不在键域的串一律拒绝,而不是猜一个。
    #[test]
    fn junk_is_rejected() {
        for bad in [
            "",
            "ctrl+",
            "ctrl",
            "ctrl+enter",
            "esc",
            "f13",
            "f0",
            "ctrl+ab",
            // 控制字符 / 非 ASCII 布局键:`Char` 只收 ASCII 可见字符。
            "ctrl+\u{7}",
            "ctrl+←",
            "ctrl+ö",
            "ctrl+\u{200b}",
        ] {
            assert!(Chord::parse(bad).is_err(), "{bad:?} 不该解析成功");
        }
    }

    /// 加了新变体这里编不过 —— 提醒你同步 `from_token` 的具名表、`token`/`label`,
    /// 以及 app 侧 `glyphs::tests::every_key_name_label_only_uses_verified_glyphs` 的列表
    /// (那两张都是列举式的,新变体的字形会静默绕过 T9 闸门)。
    #[test]
    fn the_variant_list_is_exhaustive() {
        match KeyName::Tab {
            KeyName::Char(_)
            | KeyName::F(_)
            | KeyName::Tab
            | KeyName::PageUp
            | KeyName::PageDown
            | KeyName::Home
            | KeyName::End
            | KeyName::Up
            | KeyName::Down
            | KeyName::Left
            | KeyName::Right => {}
        }
    }

    /// 智能构造器是归一与校验的唯一入口。
    ///
    /// 自证会变红:把 `KeyName::char` 里的 `is_ascii_graphic` 判断去掉(第二组红);
    /// 把 `KeyName::f` 的范围改成 `0..=13`(第三组红)。
    #[test]
    fn smart_constructors_normalise_and_reject() {
        assert_eq!(KeyName::char('N'), Some(KeyName::Char('n')));
        assert_eq!(KeyName::char('n'), Some(KeyName::Char('n')));
        assert_eq!(KeyName::char('`'), Some(KeyName::Char('`')));
        for bad in ['←', ' ', '\u{7}', 'ö', '\u{200b}', '中'] {
            assert_eq!(KeyName::char(bad), None, "{bad:?} 不该被收下");
        }
        assert_eq!(KeyName::f(12), Some(KeyName::F(12)));
        assert_eq!(KeyName::f(1), Some(KeyName::F(1)));
        assert_eq!(KeyName::f(0), None);
        assert_eq!(KeyName::f(13), None);
    }

    /// 凡是构造器造得出来的组合键,写下去都读得回来 —— 这条钉的是
    /// 「键域」与「解析域」不许错位(`F(99)` 那类「写得出读不回」)。
    #[test]
    fn every_chord_built_from_the_constructors_round_trips() {
        let mut keys: Vec<KeyName> = NAMED.to_vec();
        keys.extend((0x21u8..=0x7e).filter_map(|b| KeyName::char(b as char)));
        keys.extend((1u8..=12).filter_map(KeyName::f));
        for key in keys {
            for (ctrl, alt, sup) in [
                (false, false, false),
                (true, false, false),
                (false, true, false),
                (false, false, true),
                (true, true, true),
            ] {
                let chord = Chord {
                    ctrl,
                    shift: false,
                    alt,
                    sup,
                    key,
                };
                let text = chord.canonical();
                assert_eq!(Chord::parse(&text), Ok(chord), "{text:?} 读不回来");
            }
        }
    }

    /// 显示文本由结构生成。
    ///
    /// 自证会变红:把 `label` 里 `to_uppercase` 去掉。
    #[test]
    fn display_is_generated_from_the_structure() {
        assert_eq!(ctrl_shift(KeyName::Char('c')).display(), "Ctrl+Shift+C");
        assert_eq!(Chord::plain(KeyName::F(6)).display(), "F6");
        assert_eq!(
            Chord {
                ctrl: true,
                ..Chord::plain(KeyName::Left)
            }
            .display(),
            "Ctrl+←"
        );
        assert_eq!(ctrl_shift(KeyName::Char('`')).display(), "Ctrl+Shift+`");
    }

    /// 走 serde 进 TOML 是一行字符串,读回相等;读不回来时错误里带原串,
    /// 用户才找得到 `settings.toml` 里是哪一行写错了。
    #[test]
    fn serde_writes_a_single_string_and_reads_it_back() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Wrap {
            k: Chord,
        }
        let w = Wrap {
            k: ctrl_shift(KeyName::Char('`')),
        };
        let text = toml::to_string(&w).unwrap();
        assert!(text.contains("k = \"ctrl+shift+`\""), "{text}");
        assert_eq!(toml::from_str::<Wrap>(&text).unwrap(), w);
        let err = toml::from_str::<Wrap>("k = \"ctrl+enter\"")
            .unwrap_err()
            .to_string();
        assert!(err.contains("ctrl+enter"), "错误里没带原串:{err}");
    }
}
