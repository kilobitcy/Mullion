//! F294:可配置的本地热键 —— 动作表、默认键、键口径、合法性、撞键。
//!
//! # 没有「Bindings」影子状态
//!
//! 真值只有一份:`Settings.hotkeys`(稀疏覆盖)+ [`Action::default_chord`]。
//! 每次按键 [`resolve`] 直接从 settings 算,7 条比对、零分配。「先拷一份
//! 到 App 字段、改设置时记得同步」这种影子状态本项目踩过 N 次。
//!
//! # 键口径
//!
//! 捕获与匹配两边都走 winit 的 `key_without_modifiers()`:`logical_key` 对
//! `` Ctrl+Shift+` `` 给的是 `~`(布局把 Shift 算进去了),两边会漂;
//! `physical_key` 又不认布局。取不到(死键等)再退到 `logical_key`。

use mullion_store::{Chord, KeyName, Settings};
use std::collections::BTreeMap;
use winit::keyboard::{Key as WKey, ModifiersState, NamedKey};

/// 可配的 7 条动作。**Ctrl+1…9 不在这里**(`shell::tabs::digit_hotkey`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Action {
    NextTab,
    PrevTab,
    CloseTab,
    ToggleFiles,
    ToggleFocus,
    NewProject,
    ToggleDrawer,
}

impl Action {
    pub const ALL: [Action; 7] = [
        Action::NextTab,
        Action::PrevTab,
        Action::CloseTab,
        Action::ToggleFiles,
        Action::ToggleFocus,
        Action::NewProject,
        Action::ToggleDrawer,
    ];

    /// `settings.toml` 里 `[hotkeys]` 下的键名。**改了就是 schema 变更**。
    pub fn name(self) -> &'static str {
        match self {
            Action::NextTab => "next_tab",
            Action::PrevTab => "prev_tab",
            Action::CloseTab => "close_tab",
            Action::ToggleFiles => "toggle_files",
            Action::ToggleFocus => "toggle_focus",
            Action::NewProject => "new_project",
            Action::ToggleDrawer => "toggle_drawer",
        }
    }

    /// 默认键。抽屉是 `` Ctrl+Shift+` ``(F294 实报:原来的 `` Ctrl+` ``
    /// 与某些远端工具撞);文件侧栏必须带 Shift —— `Ctrl+B` 是 tmux 的
    /// prefix,抢了它用户在远端 tmux 里寸步难行。
    pub fn default_chord(self) -> Chord {
        let ctrl = |shift: bool, key: KeyName| Chord {
            ctrl: true,
            shift,
            alt: false,
            sup: false,
            key,
        };
        match self {
            Action::NextTab => ctrl(false, KeyName::Tab),
            Action::PrevTab => ctrl(true, KeyName::Tab),
            Action::CloseTab => ctrl(false, KeyName::Char('w')),
            Action::ToggleFiles => ctrl(true, KeyName::Char('b')),
            Action::ToggleFocus => Chord::plain(KeyName::F(6)),
            Action::NewProject => ctrl(true, KeyName::Char('n')),
            Action::ToggleDrawer => ctrl(true, KeyName::Char('`')),
        }
    }
}

/// 某个动作当前绑的键:覆盖优先,没有就默认。
pub fn chord_for(settings: &Settings, action: Action) -> Chord {
    settings
        .hotkeys
        .get(action.name())
        .copied()
        .unwrap_or_else(|| action.default_chord())
}

/// 这个组合键绑了哪个动作。手改 TOML 造出两个动作同键时,按 [`Action::ALL`]
/// 的顺序取第一个 —— 设置 UI 走 [`vet`],不会造出这种表。
pub fn resolve(settings: &Settings, chord: &Chord) -> Option<Action> {
    Action::ALL
        .into_iter()
        .find(|a| chord_for(settings, *a) == *chord)
}

/// 7 条全量(设置弹窗的草稿用)。
pub fn all_chords(settings: &Settings) -> BTreeMap<Action, Chord> {
    Action::ALL
        .into_iter()
        .map(|a| (a, chord_for(settings, a)))
        .collect()
}

/// 全量 → 稀疏:只留与默认不同的,键换成 TOML 里的名字。写回 settings 用。
pub fn overrides(bound: &BTreeMap<Action, Chord>) -> BTreeMap<String, Chord> {
    bound
        .iter()
        .filter(|(a, c)| **c != a.default_chord())
        .map(|(a, c)| (a.name().to_string(), *c))
        .collect()
}

/// 把草稿里的 7 条写回落盘用的稀疏表。**不是整表替换**:先按动作名把这 7 个
/// 键清掉,再把非默认的那几条放回去 —— `settings.toml` 是明文、用户会手改,
/// 里面可能有打错的键名或将来版本才认识的行,整表替换会让「打开设置点一次
/// 确定」把它们永久抹掉,而且零报错(本仓登记过的「整份覆盖」缺陷族)。
///
/// 改回默认的那一条随之从文件里消失(`overrides` 不会把它放回去)。
pub fn write_back(into: &mut BTreeMap<String, Chord>, bound: &BTreeMap<Action, Chord>) {
    into.retain(|k, _| !Action::ALL.iter().any(|a| a.name() == k));
    into.extend(overrides(bound));
}

/// 修饰键本身。捕获态下按到它们 = 用户还在按组合,不算一次输入。
pub fn is_modifier(key: &WKey) -> bool {
    matches!(
        key,
        WKey::Named(
            NamedKey::Shift
                | NamedKey::Control
                | NamedKey::Alt
                | NamedKey::AltGraph
                | NamedKey::Super
                | NamedKey::Meta
                | NamedKey::Hyper
                | NamedKey::CapsLock
                | NamedKey::NumLock
                | NamedKey::ScrollLock
                | NamedKey::Fn
                | NamedKey::FnLock
                | NamedKey::Symbol
                | NamedKey::SymbolLock
        )
    )
}

/// 把一个 winit 键 + 修饰键状态翻成 [`Chord`]。不在键域返回 `None`。
///
/// 字符键与 F 键都**走 store 侧的智能构造器**([`KeyName::char`] /
/// [`KeyName::f`]):归一(小写)与拒收(非 ASCII 可见字符、F 键越界)只有
/// 一份判据,这里再抄一遍就会与落盘那一侧漂开。
pub fn chord_of_key(key: &WKey, mods: ModifiersState) -> Option<Chord> {
    let name = match key {
        WKey::Character(s) => {
            let mut it = s.chars();
            let c = it.next()?;
            if it.next().is_some() {
                return None;
            }
            KeyName::char(c)?
        }
        WKey::Named(n) => match n {
            NamedKey::Tab => KeyName::Tab,
            NamedKey::PageUp => KeyName::PageUp,
            NamedKey::PageDown => KeyName::PageDown,
            NamedKey::Home => KeyName::Home,
            NamedKey::End => KeyName::End,
            NamedKey::ArrowUp => KeyName::Up,
            NamedKey::ArrowDown => KeyName::Down,
            NamedKey::ArrowLeft => KeyName::Left,
            NamedKey::ArrowRight => KeyName::Right,
            NamedKey::F1 => KeyName::f(1)?,
            NamedKey::F2 => KeyName::f(2)?,
            NamedKey::F3 => KeyName::f(3)?,
            NamedKey::F4 => KeyName::f(4)?,
            NamedKey::F5 => KeyName::f(5)?,
            NamedKey::F6 => KeyName::f(6)?,
            NamedKey::F7 => KeyName::f(7)?,
            NamedKey::F8 => KeyName::f(8)?,
            NamedKey::F9 => KeyName::f(9)?,
            NamedKey::F10 => KeyName::f(10)?,
            NamedKey::F11 => KeyName::f(11)?,
            NamedKey::F12 => KeyName::f(12)?,
            _ => return None,
        },
        _ => return None,
    };
    Some(Chord {
        ctrl: mods.control_key(),
        shift: mods.shift_key(),
        alt: mods.alt_key(),
        sup: mods.super_key(),
        key: name,
    })
}

/// 一次按键事件 → [`Chord`]。**主口径是 `key_without_modifiers()`**(理由见
/// 模块文档),取不到再退到 `logical_key`。
pub fn chord_of_event(ke: &winit::event::KeyEvent, mods: ModifiersState) -> Option<Chord> {
    use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
    chord_of_key(&ke.key_without_modifiers(), mods).or_else(|| chord_of_key(&ke.logical_key, mods))
}

/// 捕获态下一个按键的四种去向。
///
/// **判定剥在这里,不留在 `app.rs`**:`hotkey_capture_event` 要真 `App`
/// (`EventLoopProxy`)才跑得起来,四路判定写在那里等于零覆盖 —— 复核实测把
/// 那里的 `is_modifier` 取反,全库 2489 条一条不红,而真实后果是捕获功能整体
/// 变死(按住 Ctrl 就报「这个键不能用」,真正的键反而被忽略)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureStep {
    /// Esc:退出捕获,什么都不绑。
    Cancel,
    /// 修饰键本身:用户还在按组合,等下一个键。
    Ignore,
    /// 交给草稿裁决(`SettingsDraft::capture`)。
    Take(Chord),
    /// 不在键域(Enter / Space / 死键…):报「这个键不能用作快捷键」。
    Unbindable,
}

/// 捕获态下这一个按键该怎么处置。`key` 给 `logical_key`(Esc 与修饰键按它认),
/// `chord` 给 [`chord_of_event`] 的结果(键口径与匹配那一侧同源)。
pub fn capture_step(key: &WKey, chord: Option<Chord>) -> CaptureStep {
    if matches!(key, WKey::Named(NamedKey::Escape)) {
        CaptureStep::Cancel
    } else if is_modifier(key) {
        CaptureStep::Ignore
    } else if let Some(c) = chord {
        CaptureStep::Take(c)
    } else {
        CaptureStep::Unbindable
    }
}

/// 合法绑定:带 Ctrl / Alt / Super 的任意键,或 F1…F12(可裸按)。裸字母 /
/// 数字 / 符号、仅 Shift、裸 Tab 一律拒 —— 会吃掉打字。
pub fn is_legal(c: &Chord) -> Result<(), &'static str> {
    if matches!(c.key, KeyName::F(_)) || c.ctrl || c.alt || c.sup {
        Ok(())
    } else {
        Err("要带 Ctrl / Alt / Super,或者用 F1…F12")
    }
}

/// 捕获那一刻的裁决:合法 + 不撞。撞键比对对象是另外 6 条可配动作的**当前值**
/// (`bound`)与一览表里的硬名单(`ui::shortcuts::occupant`)。`Ok` 才许写进草稿。
pub fn vet(bound: &BTreeMap<Action, Chord>, action: Action, chord: Chord) -> Result<(), String> {
    is_legal(&chord).map_err(str::to_string)?;
    if let Some((other, _)) = bound.iter().find(|(a, c)| **a != action && **c == chord) {
        return Err(format!(
            "已被「{}」占用",
            crate::ui::shortcuts::what_of(*other)
        ));
    }
    if let Some(row) = crate::ui::shortcuts::occupant(&chord, action) {
        return Err(format!("已被「{}」({})占用", row.what, row.section));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::shortcuts::{ctrl, ctrl_shift, plain, shift};

    fn defaults() -> BTreeMap<Action, Chord> {
        all_chords(&Settings::default())
    }

    /// F294 实报:抽屉默认 `` Ctrl+Shift+` ``,旧的 `` Ctrl+` `` **彻底**不再是抽屉键。
    ///
    /// 自证会变红:把 `default_chord` 里 ToggleDrawer 的 `true` 改回 `false`。
    #[test]
    fn the_drawer_default_is_ctrl_shift_backtick_and_the_old_key_is_free() {
        let s = Settings::default();
        assert_eq!(
            resolve(&s, &ctrl_shift(KeyName::Char('`'))),
            Some(Action::ToggleDrawer)
        );
        assert_eq!(
            resolve(&s, &ctrl(KeyName::Char('`'))),
            None,
            "旧键还在开抽屉"
        );
    }

    /// `Ctrl+B` 是 tmux 的 prefix,文件侧栏必须带 Shift。原来是 `app.rs` 里的
    /// 源码切片守护(`!mods.shift`),现在真值在这张表里,直接测表。
    #[test]
    fn ctrl_b_is_left_to_tmux() {
        let s = Settings::default();
        assert_eq!(resolve(&s, &ctrl(KeyName::Char('b'))), None);
        assert_eq!(
            resolve(&s, &ctrl_shift(KeyName::Char('b'))),
            Some(Action::ToggleFiles)
        );
    }

    /// 覆盖优先于默认;覆盖之后旧键让出来。
    #[test]
    fn an_override_replaces_the_default_and_frees_it() {
        let mut s = Settings::default();
        s.hotkeys
            .insert("close_tab".into(), Chord::parse("ctrl+shift+w").unwrap());
        assert_eq!(
            resolve(&s, &ctrl_shift(KeyName::Char('w'))),
            Some(Action::CloseTab)
        );
        assert_eq!(
            resolve(&s, &ctrl(KeyName::Char('w'))),
            None,
            "改掉之后 Ctrl+W 还在关标签"
        );
        // 不认识的键名不影响别的
        s.hotkeys
            .insert("no_such_action".into(), Chord::parse("ctrl+q").unwrap());
        assert_eq!(resolve(&s, &ctrl(KeyName::Char('q'))), None);
    }

    /// 稀疏写回:等于默认的一条都不写。
    ///
    /// 自证会变红:把 `overrides` 里的 `filter` 删掉。
    #[test]
    fn overrides_only_keep_what_differs_from_the_defaults() {
        let mut b = defaults();
        assert!(overrides(&b).is_empty());
        b.insert(Action::ToggleDrawer, Chord::parse("ctrl+alt+d").unwrap());
        let o = overrides(&b);
        assert_eq!(o.len(), 1);
        assert_eq!(o["toggle_drawer"].canonical(), "ctrl+alt+d");
        b.insert(Action::ToggleDrawer, Action::ToggleDrawer.default_chord());
        assert!(overrides(&b).is_empty(), "改回默认之后应当删掉那一条");
    }

    /// 写回**只动这 7 个键名**:用户手写在 `[hotkeys]` 里的别的键名(打错的、
    /// 将来版本的)留着不动。整表替换的话,用新版客户端打开一次设置点确定,
    /// 老版本 / 新版本才认识的那些行就永久没了,零报错。
    ///
    /// 自证会变红:把 `write_back` 换成 `*into = overrides(bound);`。
    #[test]
    fn writing_back_replaces_the_known_actions_and_keeps_the_unknown_ones() {
        let mut into: BTreeMap<String, Chord> = BTreeMap::new();
        into.insert("no_such_action".into(), Chord::parse("ctrl+q").unwrap());
        // 上一轮写下的一条覆盖,这一轮改回了默认 —— 必须消失。
        into.insert("close_tab".into(), Chord::parse("ctrl+shift+w").unwrap());
        let mut b = defaults();
        b.insert(Action::ToggleDrawer, Chord::parse("ctrl+alt+d").unwrap());
        write_back(&mut into, &b);
        assert_eq!(
            into.get("no_such_action").copied(),
            Some(Chord::parse("ctrl+q").unwrap()),
            "不认识的键名被抹掉了"
        );
        assert!(
            !into.contains_key("close_tab"),
            "改回默认的那条没从文件里消失"
        );
        assert_eq!(
            into.get("toggle_drawer").copied(),
            Some(Chord::parse("ctrl+alt+d").unwrap()),
            "改过的那条没写进去"
        );
    }

    /// 键口径:字母归小写、Named 键映射、不在键域的返回 None。
    #[test]
    fn chord_of_key_lowercases_letters_and_maps_named_keys() {
        let none = ModifiersState::empty();
        let cs = ModifiersState::CONTROL | ModifiersState::SHIFT;
        assert_eq!(
            chord_of_key(&WKey::Character("N".into()), cs),
            Some(ctrl_shift(KeyName::Char('n')))
        );
        assert_eq!(
            chord_of_key(&WKey::Named(NamedKey::F6), none),
            Some(plain(KeyName::F(6)))
        );
        assert_eq!(
            chord_of_key(&WKey::Named(NamedKey::ArrowLeft), ModifiersState::ALT),
            Some(Chord {
                alt: true,
                ..plain(KeyName::Left)
            })
        );
        for k in [
            WKey::Named(NamedKey::Enter),
            WKey::Named(NamedKey::Escape),
            WKey::Named(NamedKey::Backspace),
            WKey::Named(NamedKey::Space),
            WKey::Named(NamedKey::Shift),
            WKey::Character("ab".into()),
            WKey::Character(" ".into()),
            // 非 ASCII 布局键:`KeyName::char` 拒收(显示文本会与方向键撞)。
            WKey::Character("ö".into()),
        ] {
            assert_eq!(chord_of_key(&k, cs), None, "{k:?} 不该进键域");
        }
        assert!(is_modifier(&WKey::Named(NamedKey::Control)));
        assert!(!is_modifier(&WKey::Named(NamedKey::F6)));
    }

    /// 修饰键名单必须**覆盖 winit 0.30.13 的全部 14 个修饰键变体**。
    ///
    /// 这里的名单是**独立硬编码**的(照 `winit::keyboard::NamedKey` 里
    /// `Alt` 到 `Super` 那一段逐个抄下来),不从 `is_modifier` 反推 —— 判据
    /// 与被测量同源就是恒绿。漏一个的后果是静默的:捕获态下按住那个键会被
    /// 当成「一次输入」,`chord_of_event` 对它返回 `None`,用户看到的是
    /// 「按了个 CapsLock 就报这个键不能用」。
    ///
    /// 自证会变红:删掉 `is_modifier` 里的 `NamedKey::FnLock` 那一项。
    #[test]
    fn every_winit_modifier_key_counts_as_a_modifier() {
        for n in [
            NamedKey::Alt,
            NamedKey::AltGraph,
            NamedKey::CapsLock,
            NamedKey::Control,
            NamedKey::Fn,
            NamedKey::FnLock,
            NamedKey::NumLock,
            NamedKey::ScrollLock,
            NamedKey::Shift,
            NamedKey::Symbol,
            NamedKey::SymbolLock,
            NamedKey::Meta,
            NamedKey::Hyper,
            NamedKey::Super,
        ] {
            assert!(is_modifier(&WKey::Named(n)), "{n:?} 没算成修饰键");
        }
        for n in [
            NamedKey::Enter,
            NamedKey::Escape,
            NamedKey::Tab,
            NamedKey::Space,
            NamedKey::F6,
            NamedKey::ArrowUp,
        ] {
            assert!(!is_modifier(&WKey::Named(n)), "{n:?} 被当成了修饰键");
        }
        assert!(!is_modifier(&WKey::Character("a".into())));
    }

    /// 捕获态四路判定各走各的。**这四路原来长在 `app.rs` 的
    /// `hotkey_capture_event` 里**(造不出 `App`,零覆盖):复核实测把那里的
    /// `is_modifier` 取反,2489 条测试一条都不红,而功能整体变死。
    ///
    /// 自证会变红:把 `capture_step` 里的 `is_modifier(key)` 取反(第二段红);
    /// 把 Esc 与修饰键两条分支对调(第一段红)。
    #[test]
    fn capture_step_routes_escape_modifiers_and_dead_keys_apart() {
        assert_eq!(
            capture_step(&WKey::Named(NamedKey::Escape), None),
            CaptureStep::Cancel,
            "Esc 不取消 —— 点进捕获态就再也退不出来"
        );
        for m in [
            NamedKey::Control,
            NamedKey::Shift,
            NamedKey::Alt,
            NamedKey::Super,
            NamedKey::CapsLock,
        ] {
            assert_eq!(
                capture_step(&WKey::Named(m), None),
                CaptureStep::Ignore,
                "{m:?} 被当成了一次输入 —— 用户先按住 Ctrl 就会被判「这个键不能用」"
            );
        }
        let f6 = plain(KeyName::F(6));
        assert_eq!(
            capture_step(&WKey::Named(NamedKey::F6), Some(f6)),
            CaptureStep::Take(f6)
        );
        let cq = ctrl(KeyName::Char('q'));
        assert_eq!(
            capture_step(&WKey::Character("q".into()), Some(cq)),
            CaptureStep::Take(cq)
        );
        for k in [
            WKey::Named(NamedKey::Enter),
            WKey::Named(NamedKey::Space),
            WKey::Named(NamedKey::Backspace),
            WKey::Named(NamedKey::Insert),
        ] {
            assert_eq!(
                capture_step(&k, None),
                CaptureStep::Unbindable,
                "{k:?} 该报「不能用作快捷键」"
            );
        }
    }

    /// 合法性:F 键可裸;别的要带 Ctrl / Alt / Super;仅 Shift 不算。
    ///
    /// 自证会变红:把 `is_legal` 里 `c.ctrl || c.alt || c.sup` 改成 `true`。
    #[test]
    fn legality_requires_a_real_modifier_unless_it_is_an_f_key() {
        assert!(is_legal(&plain(KeyName::F(3))).is_ok());
        assert!(is_legal(&ctrl(KeyName::Char('q'))).is_ok());
        assert!(is_legal(&Chord {
            alt: true,
            ..plain(KeyName::Char('q'))
        })
        .is_ok());
        assert!(is_legal(&Chord {
            sup: true,
            ..plain(KeyName::Char('q'))
        })
        .is_ok());
        for bad in [
            plain(KeyName::Char('x')),
            plain(KeyName::Char('1')),
            plain(KeyName::Char('`')),
            shift(KeyName::Char('x')),
            plain(KeyName::Tab),
            shift(KeyName::Tab),
            plain(KeyName::Up),
        ] {
            assert!(is_legal(&bad).is_err(), "{} 不该合法", bad.display());
        }
    }

    /// 撞键:与另一条可配动作撞、与终端 / 文件面板 / 标签 Ctrl+1…9 的硬名单撞
    /// 都拒,并且报得出占用者;「会话管理器」一节不算。
    ///
    /// 自证会变红:把 `vet` 里 `occupant` 那段删掉(第二、三、四条红)。
    #[test]
    fn vet_rejects_collisions_and_names_the_occupant() {
        let b = defaults();
        let err = vet(&b, Action::ToggleDrawer, ctrl(KeyName::Char('w'))).unwrap_err();
        assert!(err.contains("关闭当前标签"), "{err}");
        let err = vet(&b, Action::ToggleDrawer, ctrl_shift(KeyName::Char('c'))).unwrap_err();
        assert!(err.contains("复制选区") && err.contains("终端"), "{err}");
        let err = vet(&b, Action::ToggleDrawer, ctrl(KeyName::Char('h'))).unwrap_err();
        assert!(err.contains("点文件"), "{err}");
        let err = vet(&b, Action::ToggleDrawer, ctrl(KeyName::Char('5'))).unwrap_err();
        assert!(err.contains("第 N 个标签"), "{err}");
        // 会话管理器的 Ctrl+1…4 已被标签的 Ctrl+1…9 盖住;单独验「会话管理器不算」
        // 要找一个只有那一节有的键 —— 它那节只有 ↑/↓/Enter/Ctrl+数字,前三个
        // 本来就不合法,所以这里验的是:`occupant` 对该节返回 None。
        assert!(
            crate::ui::shortcuts::occupant(&plain(KeyName::Up), Action::ToggleDrawer)
                .map(|r| r.section)
                != Some(crate::ui::shortcuts::SECTION_SESSION_MANAGER)
        );
        assert!(vet(
            &b,
            Action::ToggleDrawer,
            Chord::parse("ctrl+alt+d").unwrap()
        )
        .is_ok());
        // 改成自己当前的键不算撞
        assert!(vet(&b, Action::CloseTab, ctrl(KeyName::Char('w'))).is_ok());
    }

    /// 白名单:`Ctrl+Shift+N` 对 NewProject 放行(现状默认值),对别的动作拒绝。
    ///
    /// 自证会变红:去掉 `occupant` 里 `shared_with` 那个判断。
    #[test]
    fn vet_whitelists_ctrl_shift_n_only_for_new_project() {
        let mut b = defaults();
        // 先把 NewProject 挪开,再验证它能绑回来
        b.insert(Action::NewProject, Chord::parse("ctrl+alt+p").unwrap());
        assert!(vet(&b, Action::NewProject, ctrl_shift(KeyName::Char('n'))).is_ok());
        let err = vet(&b, Action::ToggleDrawer, ctrl_shift(KeyName::Char('n'))).unwrap_err();
        assert!(err.contains("新建文件夹"), "{err}");
    }

    /// 拒绝发生在写入之前:`vet` 是纯函数,不碰 `bound`。
    #[test]
    fn vet_rejects_before_anything_is_written() {
        let b = defaults();
        let before = b.clone();
        let _ = vet(&b, Action::ToggleDrawer, plain(KeyName::Char('x')));
        assert_eq!(b, before);
    }

    /// 键口径守护:`chord_of_event` 主口径必须是 `key_without_modifiers`。
    /// 造不出 `KeyEvent`(字段私有),只能切源码。
    ///
    /// 自证会变红:把 `chord_of_event` 里 `key_without_modifiers()` 换成 `logical_key`。
    #[test]
    fn the_hotkey_chord_is_read_without_modifiers() {
        let src = include_str!("hotkeys.rs");
        let body = src
            .split("pub fn chord_of_event(")
            .nth(1)
            .expect("找不到 chord_of_event");
        let body: String = body[..body.find("\n}\n").expect("找不到函数结尾")]
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect();
        let primary = body
            .find("key_without_modifiers()")
            .expect("没用 key_without_modifiers");
        let fallback = body.find("logical_key").expect("没有 logical_key 兜底");
        assert!(primary < fallback, "主口径不是 key_without_modifiers");
    }
}
