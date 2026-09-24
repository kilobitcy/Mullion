//! F84 / F295:快捷键一览的数据源;F294 起也是**撞键判定**的数据源。
//!
//! # 这张表仍然是手抄的,但键是结构化的
//!
//! 快捷键的实现散在几处,没有统一注册中心:
//!
//! - `mullion_term::keymap` —— 编码给远端的键(T5/T6)
//! - `app.rs` 的 `KeyboardInput` 分支 / 各 `*_hotkey_event` / `handle_panel_key`
//!   + `shell::tabs` + `ui::annotate::hotkey` + `ui::session_manager::keys::scan`
//!     —— 本地动作
//!
//! 为一张表去把整条输入链路重构成注册中心,代价远大于收益。诚实的做法是承认
//! 它是手抄的、把真源写在这里,然后守住手抄最容易出的错:撞键、漏节、Esc
//! 写两遍([`tests`])。F295 把每行的键从字符串改成 [`Keys`]:**显示文本由
//! 结构生成**,撞键按结构比 —— 字符串比对会被「Ctrl+Shift+C」vs「Ctrl+Shift+c」
//! 这种漂移骗过。
//!
//! # 分节
//!
//! 行按 [`Shortcut::section`] 分组,顺序 = 表里首次出现的顺序。**只有 Esc
//! 合并**进「通用」一节:其余跨节重名(Ctrl+Shift+N / Ctrl+C / Ctrl+1…)各留
//! 各的行 —— 它们在不同焦点下是不同的功能,合并反而藏掉了「谁压过谁」。

use mullion_store::{Chord, KeyName};

/// 一行的键。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keys {
    /// 单个组合键。
    Chord(Chord),
    /// 同一功能的几个键,显示用「 / 」连。
    Chords(&'static [Chord]),
    /// `Ctrl+1 … Ctrl+N`。**n ≤ 9** —— `chords()` 是拿 `b'0' + d` 算键名的,
    /// 10 会算成 `:`(由 `every_row_is_filled_in` 守着)。
    CtrlDigits(u8),
    /// 不在 [`KeyName`] 键域里的键(Esc / Enter / Backspace …)或鼠标手势。
    /// **不参与撞键**:它们本来就绑不了。
    Text(&'static str),
}

impl Keys {
    /// 显示文本。
    pub fn display(&self) -> String {
        match self {
            Self::Chord(c) => c.display(),
            Self::Chords(cs) => cs
                .iter()
                .map(|c| c.display())
                .collect::<Vec<_>>()
                .join(" / "),
            Self::CtrlDigits(n) => format!("Ctrl+1 … Ctrl+{n}"),
            Self::Text(s) => (*s).to_string(),
        }
    }

    /// 这一行占了哪些组合键(撞键用)。`Text` 为空。
    pub fn chords(&self) -> Vec<Chord> {
        match self {
            Self::Chord(c) => vec![*c],
            Self::Chords(cs) => cs.to_vec(),
            // `min(9)`:`b'0' + d` 只在 1..=9 里算得出数字键,10 会算成 `:`。
            // 表里有 `every_row_is_filled_in` 守着 n ≤ 9,这里再夹一道 ——
            // 撞键判定宁可少认一个键,也不能凭空认出一个根本按不出来的键。
            Self::CtrlDigits(n) => (1..=(*n).min(9))
                .map(|d| ctrl(KeyName::Char(char::from(b'0' + d))))
                .collect(),
            Self::Text(_) => Vec::new(),
        }
    }

    /// 这一行是否占了 `c`。
    pub fn covers(&self, c: &Chord) -> bool {
        self.chords().contains(c)
    }
}

/// 一览表里的一行。
#[derive(Debug, Clone, Copy)]
pub struct Shortcut {
    pub keys: Keys,
    /// 小节名。**撞键只在同一节内才算撞**(见 `no_two_rows_claim_the_same_chord`);
    /// 「会话管理器」一节在 F294 的改键撞键判定里整体排除 —— 弹窗开着时本地
    /// 热键整体让位(`modal_open`),不会真撞。
    pub section: &'static str,
    /// 干什么。
    pub what: &'static str,
    /// F294 撞键判定的**唯一白名单**:这一行与「项目」的建项目热键共享同一个
    /// 组合键,靠焦点分辨(焦点在文件面板时建项目那条让位)。只有文件面板的
    /// 「新建文件夹」那一行是 `true`。
    pub shared_with_new_project: bool,
}

pub const fn plain(k: KeyName) -> Chord {
    Chord::plain(k)
}

pub const fn ctrl(k: KeyName) -> Chord {
    Chord {
        ctrl: true,
        shift: false,
        alt: false,
        sup: false,
        key: k,
    }
}

pub const fn ctrl_shift(k: KeyName) -> Chord {
    Chord {
        ctrl: true,
        shift: true,
        alt: false,
        sup: false,
        key: k,
    }
}

pub const fn shift(k: KeyName) -> Chord {
    Chord {
        ctrl: false,
        shift: true,
        alt: false,
        sup: false,
        key: k,
    }
}

const fn row(keys: Keys, section: &'static str, what: &'static str) -> Shortcut {
    Shortcut {
        keys,
        section,
        what,
        shared_with_new_project: false,
    }
}

pub const SECTION_GENERAL: &str = "通用";
pub const SECTION_TABS: &str = "标签";
pub const SECTION_TERMINAL: &str = "终端";
pub const SECTION_FILES: &str = "文件面板";
pub const SECTION_SESSION_MANAGER: &str = "会话管理器";
pub const SECTION_ANNOTATE: &str = "标注模式";
pub const SECTION_PROJECT: &str = "项目";
pub const SECTION_DRAWER: &str = "命令抽屉";

/// 全部快捷键。逐条从实现处核对过,改实现时**必须同步这里**。
/// 键名一律写**小写字面量**(`KeyName::Char('n')`),表里的每个 chord 都有
/// `every_table_chord_round_trips` 守着「能写进 TOML 也能读回来」。
pub const SHORTCUTS: &[Shortcut] = &[
    // —— 通用 —— 只有 Esc 合并到这里。文案按事实写:设置 / 项目管理 / 分组管理 /
    // 导入 / 迁移包 / 解锁 / 标签属性 / 编辑器 / 传输面板 **不认 Esc**。
    row(
        Keys::Text("Esc"),
        SECTION_GENERAL,
        "退出标注模式;关掉会话管理器 / 恢复现场 / 换节点 / 选项目 / 文件对话框 / 粘贴确认 / 远端栏搜索条;放弃就地重命名",
    ),
    // —— 标签(app.rs tab_hotkey_event / shell::tabs)——
    row(
        Keys::Chord(ctrl(KeyName::Tab)),
        SECTION_TABS,
        "切到下一个标签",
    ),
    row(
        Keys::Chord(ctrl_shift(KeyName::Tab)),
        SECTION_TABS,
        "切到上一个标签",
    ),
    row(
        Keys::Chord(ctrl(KeyName::Char('w'))),
        SECTION_TABS,
        "关闭当前标签(抢了 bash 的 ^W 删词)",
    ),
    row(Keys::CtrlDigits(9), SECTION_TABS, "切到第 N 个标签"),
    // —— 终端(app.rs 的 KeyboardInput 分支)——
    row(
        Keys::Chord(ctrl_shift(KeyName::Char('c'))),
        SECTION_TERMINAL,
        "复制选区(裸 Ctrl+C 照旧发给远端)",
    ),
    row(
        Keys::Chord(ctrl_shift(KeyName::Char('v'))),
        SECTION_TERMINAL,
        "粘贴(走 bracketed paste)",
    ),
    row(
        Keys::Chords(&[shift(KeyName::PageUp), shift(KeyName::PageDown)]),
        SECTION_TERMINAL,
        "本地翻页回溯(裸 PageUp / PageDown 照旧发给远端)",
    ),
    row(
        Keys::Text("Shift+拖动"),
        SECTION_TERMINAL,
        "强制本地划选(全屏 TUI 开着鼠标上报时的逃生门)",
    ),
    row(
        Keys::Text("Shift+Enter"),
        SECTION_TERMINAL,
        "插入换行而不提交",
    ),
    // —— 文件面板(app.rs files/focus_hotkey_event + handle_panel_key)——
    // 侧栏开关与换焦点全局生效;其余只在**焦点落在文件面板**时生效,且多数只认远端栏。
    row(
        Keys::Chord(ctrl_shift(KeyName::Char('b'))),
        SECTION_FILES,
        "开关文件侧栏(有选区时先跳到选区里那条路径)",
    ),
    row(
        Keys::Chord(plain(KeyName::F(6))),
        SECTION_FILES,
        "终端与文件面板之间换焦点(面板不在场时这个键照旧发给远端)",
    ),
    row(
        Keys::Chord(ctrl(KeyName::Char('h'))),
        SECTION_FILES,
        "显示 / 隐藏点文件",
    ),
    row(
        Keys::Chord(ctrl(KeyName::Char('n'))),
        SECTION_FILES,
        "在远端栏就地新建文件",
    ),
    Shortcut {
        keys: Keys::Chord(ctrl_shift(KeyName::Char('n'))),
        section: SECTION_FILES,
        what: "在远端栏就地新建文件夹(焦点在面板时压过「项目」那条)",
        shared_with_new_project: true,
    },
    row(
        Keys::Chords(&[
            ctrl(KeyName::Char('c')),
            ctrl(KeyName::Char('x')),
            ctrl(KeyName::Char('v')),
        ]),
        SECTION_FILES,
        "远端栏内的复制 / 剪切 / 粘贴",
    ),
    row(Keys::Text("Enter"), SECTION_FILES, "进入目录 / 打开文件"),
    row(Keys::Text("Backspace"), SECTION_FILES, "回到上一级目录"),
    row(
        Keys::Chord(plain(KeyName::F(5))),
        SECTION_FILES,
        "刷新当前栏",
    ),
    row(
        Keys::Chord(plain(KeyName::Tab)),
        SECTION_FILES,
        "在本地栏与远端栏之间切换",
    ),
    row(
        Keys::Chords(&[plain(KeyName::Up), plain(KeyName::Down)]),
        SECTION_FILES,
        "上 / 下移动选中",
    ),
    row(
        Keys::Text("Delete"),
        SECTION_FILES,
        "删除选中(远端栏,先确认)",
    ),
    row(
        Keys::Text("Shift+Delete"),
        SECTION_FILES,
        "删除选中,跳过确认(远端栏)",
    ),
    row(
        Keys::Chord(plain(KeyName::F(2))),
        SECTION_FILES,
        "重命名(远端栏)",
    ),
    row(
        Keys::Text("字母 / 数字"),
        SECTION_FILES,
        "跳到以它开头的条目(1 秒内连按累积前缀,同一字母重按在同首字母间循环)",
    ),
    // —— 会话管理器(ui::session_manager::keys::scan)——
    row(
        Keys::Chords(&[plain(KeyName::Up), plain(KeyName::Down)]),
        SECTION_SESSION_MANAGER,
        "上 / 下一条会话(编辑文本时让位)",
    ),
    row(
        Keys::Text("Enter"),
        SECTION_SESSION_MANAGER,
        "连接选中的会话",
    ),
    row(
        Keys::CtrlDigits(4),
        SECTION_SESSION_MANAGER,
        "切换右栏的编辑器分页",
    ),
    // —— 标注模式(ui::annotate::hotkey)——
    row(
        Keys::Chord(ctrl_shift(KeyName::Char('f'))),
        SECTION_ANNOTATE,
        "进入 / 退出标注模式",
    ),
    row(
        Keys::Chord(ctrl_shift(KeyName::Char('e'))),
        SECTION_ANNOTATE,
        "把标注导出成 Markdown 到剪贴板",
    ),
    row(
        Keys::Chord(ctrl_shift(KeyName::Char('d'))),
        SECTION_ANNOTATE,
        "在紧凑 / 标准 / 详细三档间循环",
    ),
    // —— 项目(app.rs project_hotkey_event)——
    row(
        Keys::Chord(ctrl_shift(KeyName::Char('n'))),
        SECTION_PROJECT,
        "把当前分屏的目录和 tmux 会话收成一个新项目(焦点在文件面板时让位)",
    ),
    // —— 命令抽屉(app.rs drawer_hotkey_event)——
    row(
        Keys::Chord(ctrl(KeyName::Char('`'))),
        SECTION_DRAWER,
        "在焦点分屏底下开 / 关命令抽屉",
    ),
];

/// 按小节分组,顺序 = 表里首次出现的顺序。设置弹窗按这个画。
pub fn sections() -> Vec<(&'static str, Vec<&'static Shortcut>)> {
    let mut out: Vec<(&'static str, Vec<&'static Shortcut>)> = Vec::new();
    for s in SHORTCUTS {
        match out.iter_mut().find(|(name, _)| *name == s.section) {
            Some((_, rows)) => rows.push(s),
            None => out.push((s.section, vec![s])),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **同一节里一个组合键只能有一个含义。**
    ///
    /// 撞键在实现处看不出来(各模块各写各的),只有在一览表里并排放着时才
    /// 暴露。按**结构**比:`Chords` / `CtrlDigits` 展开成单个组合键;`Text`
    /// 按字面比。
    ///
    /// 自证会变红:把任意一行的键改成同节里另一行的值。
    #[test]
    fn no_two_rows_claim_the_same_chord() {
        let mut seen_chords: Vec<(&str, Chord)> = Vec::new();
        let mut seen_text: Vec<(&str, &str)> = Vec::new();
        for s in SHORTCUTS {
            for c in s.keys.chords() {
                assert!(
                    !seen_chords.contains(&(s.section, c)),
                    "「{}」在「{}」里出现了两次 —— 同一个组合键不能有两个含义",
                    c.display(),
                    s.section
                );
                seen_chords.push((s.section, c));
            }
            if let Keys::Text(t) = s.keys {
                assert!(
                    !seen_text.contains(&(s.section, t)),
                    "「{t}」在「{}」里出现了两次",
                    s.section
                );
                seen_text.push((s.section, t));
            }
        }
    }

    /// F295 的实报:**Esc 整张表只许出现一次**,在「通用」节。
    ///
    /// 自证会变红:往「标注模式」加回一行 `Keys::Text("Esc")`。
    #[test]
    fn escape_is_listed_exactly_once() {
        let esc: Vec<&Shortcut> = SHORTCUTS
            .iter()
            .filter(|s| matches!(s.keys, Keys::Text(t) if t.starts_with("Esc")))
            .collect();
        assert_eq!(esc.len(), 1, "Esc 出现了 {} 次", esc.len());
        assert_eq!(esc[0].section, SECTION_GENERAL);
    }

    /// 两个文字字段都不许空:空的那一格在表格里就是一行看不懂的东西。
    #[test]
    fn every_row_is_filled_in() {
        for s in SHORTCUTS {
            assert!(!s.keys.display().trim().is_empty(), "有一行没写组合键");
            assert!(
                !s.section.trim().is_empty(),
                "「{}」没写小节",
                s.keys.display()
            );
            assert!(
                !s.what.trim().is_empty(),
                "「{}」没写作用",
                s.keys.display()
            );
            if let Keys::CtrlDigits(n) = s.keys {
                assert!(n <= 9, "CtrlDigits({n}) 越界 —— 10 会被算成 `Ctrl+:`");
            }
        }
    }

    /// 表非空,且覆盖到了每一个有快捷键的模块 —— 漏掉一整节是这张手抄表的
    /// 另一种失效方式(撞键测试对它无感)。
    ///
    /// 自证会变红:删掉某一节的全部行。
    #[test]
    fn every_module_that_has_shortcuts_is_represented() {
        let names: Vec<&str> = sections().into_iter().map(|(n, _)| n).collect();
        assert_eq!(
            names,
            [
                SECTION_GENERAL,
                SECTION_TABS,
                SECTION_TERMINAL,
                SECTION_FILES,
                SECTION_SESSION_MANAGER,
                SECTION_ANNOTATE,
                SECTION_PROJECT,
                SECTION_DRAWER,
            ],
            "小节顺序 / 覆盖面变了"
        );
    }

    /// 显示文本是生成的:`Chords` 用「 / 」连,`CtrlDigits` 是区间写法。
    #[test]
    fn display_is_generated_from_keys() {
        // 绑成 `const` 而不是写成临时数组:`Keys::Chords` 要 `&'static [Chord]`,
        // 而函数体里的 `&[shift(..)]` 不走常量提升(带 const fn 调用),借用活不到 'static。
        const PAGES: [Chord; 2] = [shift(KeyName::PageUp), shift(KeyName::PageDown)];
        assert_eq!(
            Keys::Chords(&PAGES).display(),
            "Shift+PageUp / Shift+PageDown"
        );
        assert_eq!(Keys::CtrlDigits(9).display(), "Ctrl+1 … Ctrl+9");
        assert!(Keys::CtrlDigits(9).covers(&ctrl(KeyName::Char('5'))));
        assert!(!Keys::CtrlDigits(4).covers(&ctrl(KeyName::Char('5'))));
        assert!(!Keys::CtrlDigits(9).covers(&ctrl_shift(KeyName::Char('5'))));
    }

    /// 白名单只有一对。
    ///
    /// 自证会变红:给「项目」那一行也标上 `shared_with_new_project: true`。
    #[test]
    fn the_only_shared_chord_is_the_files_panel_new_folder() {
        let shared: Vec<&Shortcut> = SHORTCUTS
            .iter()
            .filter(|s| s.shared_with_new_project)
            .collect();
        assert_eq!(shared.len(), 1);
        assert_eq!(shared[0].section, SECTION_FILES);
        assert_eq!(shared[0].keys, Keys::Chord(ctrl_shift(KeyName::Char('n'))));
    }

    /// 表里每个 chord 都是「能写进 TOML 也能读回来」的:裸 `KeyName::Char(..)`
    /// 字面量绕过了 `KeyName::char` 的归一,写成大写 / 非 ASCII 这里就红。
    ///
    /// 自证会变红:把表里任一 `KeyName::Char('c')` 改成 `'C'`。
    #[test]
    fn every_table_chord_round_trips() {
        for s in SHORTCUTS {
            for c in s.keys.chords() {
                assert_eq!(
                    Chord::parse(&c.canonical()),
                    Ok(c),
                    "「{}」({})写进 TOML 读不回来",
                    c.display(),
                    s.what
                );
            }
        }
    }
}
