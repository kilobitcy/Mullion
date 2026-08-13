//! F84:快捷键一览的数据源。**只读**——本片不做改键。
//!
//! # 这张表是手抄的
//!
//! 快捷键的实现散在三处,没有统一注册中心:
//!
//! - `mullion_term::keymap` —— 编码给远端的键(T5/T6)
//! - `shell::tabs::hotkey` —— 标签切换
//! - `app.rs` 的 `KeyboardInput` 分支 + `ui::annotate::hotkey` +
//!   `ui::session_manager::keys::scan` —— 本地动作
//!
//! 为一张只读表格去把整条输入链路重构成注册中心,代价远大于收益。诚实的做法
//! 是承认它是手抄的、把真源写在这里,然后守住手抄**最容易出的那个错**:
//! 撞键([`no_two_rows_claim_the_same_chord`])。spec F50 里那句「不能用
//! `Ctrl+Shift+F`,已被 F100 占用」就是撞键的现场记录——有了这条测试,下次
//! 撞车会在**把新键加进一览表**的那一刻变红,而不是在用户手上。
//!
//! [`no_two_rows_claim_the_same_chord`]: tests::no_two_rows_claim_the_same_chord

/// 一览表里的一行。
pub struct Shortcut {
    /// 组合键的显示文本。
    pub chord: &'static str,
    /// 在哪儿生效。**撞键只在同一个 scope 内才算撞**——`Ctrl+1…9`
    /// (切标签)与 `Ctrl+1…4`(会话管理器切编辑器 Tab)不冲突,因为
    /// `tabs::hotkey` 在弹窗开着时(`modal_open`)整个让位。
    pub scope: &'static str,
    /// 干什么。
    pub what: &'static str,
}

/// 全部快捷键。逐条从实现处核对过,改实现时**必须同步这里**。
pub const SHORTCUTS: &[Shortcut] = &[
    // —— 标签(shell::tabs::hotkey)——
    Shortcut {
        chord: "Ctrl+Tab",
        scope: "标签",
        what: "切到下一个标签",
    },
    Shortcut {
        chord: "Ctrl+Shift+Tab",
        scope: "标签",
        what: "切到上一个标签",
    },
    Shortcut {
        chord: "Ctrl+W",
        scope: "标签",
        what: "关闭当前标签(抢了 bash 的 ^W 删词)",
    },
    Shortcut {
        chord: "Ctrl+1 … Ctrl+9",
        scope: "标签",
        what: "切到第 N 个标签",
    },
    // —— 终端(app.rs 的 KeyboardInput 分支)——
    Shortcut {
        chord: "Ctrl+Shift+C",
        scope: "终端",
        what: "复制选区(裸 Ctrl+C 照旧发给远端)",
    },
    Shortcut {
        chord: "Ctrl+Shift+V",
        scope: "终端",
        what: "粘贴(走 bracketed paste)",
    },
    Shortcut {
        chord: "Shift+PageUp / PageDown",
        scope: "终端",
        what: "本地翻页回溯(裸 PageUp/PageDown 照旧发给远端)",
    },
    Shortcut {
        chord: "Shift+拖动",
        scope: "终端",
        what: "强制本地划选(全屏 TUI 开着鼠标上报时的逃生门)",
    },
    Shortcut {
        chord: "Shift+Enter",
        scope: "终端",
        what: "插入换行而不提交",
    },
    // —— 文件面板(app.rs::files_hotkey_event)——
    Shortcut {
        chord: "Ctrl+Shift+B",
        scope: "文件",
        what: "开关文件侧栏",
    },
    // —— 会话管理器(ui::session_manager::keys::scan)——
    Shortcut {
        chord: "↑ / ↓",
        scope: "会话管理器",
        what: "上/下一条会话(编辑文本时让位)",
    },
    Shortcut {
        chord: "Enter",
        scope: "会话管理器",
        what: "连接选中的会话",
    },
    Shortcut {
        chord: "Ctrl+1 … Ctrl+4",
        scope: "会话管理器",
        what: "切换右栏的编辑器分页",
    },
    Shortcut {
        chord: "Esc",
        scope: "会话管理器",
        what: "关闭窗口(焦点在输入框时先退出编辑)",
    },
    // —— 标注模式(ui::annotate::hotkey)——
    Shortcut {
        chord: "Ctrl+Shift+F",
        scope: "标注模式",
        what: "进入 / 退出标注模式",
    },
    Shortcut {
        chord: "Ctrl+Shift+E",
        scope: "标注模式",
        what: "把标注导出成 Markdown 到剪贴板",
    },
    Shortcut {
        chord: "Ctrl+Shift+D",
        scope: "标注模式",
        what: "在紧凑 / 标准 / 详细三档间循环",
    },
    Shortcut {
        chord: "Esc",
        scope: "标注模式",
        what: "退出标注模式",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    /// **同一个 scope 里一个组合键只能有一个含义。**
    ///
    /// 这是这张手抄表唯一守得住、也最值得守的东西:撞键在实现处看不出来
    /// (三个模块各写各的),只有在一览表里并排放着时才暴露。
    ///
    /// 自证会变红:把任意一行的 `chord` 改成同 scope 里另一行的值。
    #[test]
    fn no_two_rows_claim_the_same_chord() {
        let mut seen: Vec<(&str, &str)> = Vec::new();
        for s in SHORTCUTS {
            let key = (s.scope, s.chord);
            assert!(
                !seen.contains(&key),
                "「{}」在「{}」里出现了两次 —— 同一个组合键不能有两个含义",
                s.chord,
                s.scope
            );
            seen.push(key);
        }
    }

    /// 三个字段都不许空:空的那一格在表格里就是一行看不懂的东西。
    #[test]
    fn every_row_is_filled_in() {
        for s in SHORTCUTS {
            assert!(!s.chord.trim().is_empty(), "有一行没写组合键");
            assert!(!s.scope.trim().is_empty(), "「{}」没写生效范围", s.chord);
            assert!(!s.what.trim().is_empty(), "「{}」没写作用", s.chord);
        }
    }

    /// 表非空,且覆盖到了每一个有快捷键的模块 —— 漏掉一整个 scope 是这张
    /// 手抄表的另一种失效方式(撞键测试对它无感)。
    ///
    /// 自证会变红:删掉某个 scope 的全部行。
    #[test]
    fn every_module_that_has_shortcuts_is_represented() {
        for want in ["标签", "终端", "文件", "会话管理器", "标注模式"] {
            assert!(
                SHORTCUTS.iter().any(|s| s.scope == want),
                "一览表里没有「{want}」这一档的任何快捷键"
            );
        }
    }
}
