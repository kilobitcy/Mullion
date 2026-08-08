//! 会话管理器的键盘快捷键(走查 16)。
//!
//! **判定全部留在会话管理器这一层,不碰 `app.rs` 的键盘路由**(陷阱 T8):
//! 那条路上「键盘先判后喂」的顺序是终端能不能收到按键的命门,为一个弹窗去
//! 动它是拿整个终端的可用性冒险。会话管理器开着的时候,egui 的
//! `wants_keyboard_input()` 本来就把键盘判给了 UI,这里读 `InputState` 就够。
//!
//! 判定抽成纯函数(`scan`)而不是散在 `show()` 里的一堆 `if`:快捷键的坑全在
//! 「什么时候**不该**响应」上 —— 用户正在文本框里打字时按 ↑ 是移动光标,
//! 不是切会话。这个条件写在渲染代码里就再也测不动了。

/// 这一帧解出来的快捷键动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Action {
    /// Esc:关掉会话管理器(有确认框时先撤确认框)。
    Close,
    /// ↑:选上一条会话。
    Prev,
    /// ↓:选下一条会话。
    Next,
    /// Enter:连接当前选中的会话。
    Open,
    /// Ctrl+1..4:切到第 n 个 Tab。
    Tab(usize),
}

/// 从这一帧的输入里解出该响应哪些快捷键。
///
/// `typing` = 此刻有控件握着键盘焦点(多半是某个文本框)。此时 ↑↓ / Enter
/// **一律让位**:那是光标移动和换行,抢过来会让人没法编辑。
/// `Ctrl+数字` 和 `Esc` 不受影响 —— 前者文本框不认,后者在 egui 里本来就是
/// 「退出编辑」,焦点还在时这一下先被 egui 用掉,用户再按一次才关窗,
/// 正好是想要的两段式。
pub(super) fn scan(i: &egui::InputState, typing: bool) -> Vec<Action> {
    let mut out = Vec::new();
    if i.key_pressed(egui::Key::Escape) && !typing {
        out.push(Action::Close);
    }
    if !typing {
        if i.key_pressed(egui::Key::ArrowUp) {
            out.push(Action::Prev);
        }
        if i.key_pressed(egui::Key::ArrowDown) {
            out.push(Action::Next);
        }
        if i.key_pressed(egui::Key::Enter) {
            out.push(Action::Open);
        }
    }
    // `command` 而不是 `ctrl`:在 macOS 上它是 ⌘。Windows 是一等公民,
    // 那里两者等价,顺手把 mac 也照顾到不额外花钱。
    if i.modifiers.command {
        for (n, key) in [
            egui::Key::Num1,
            egui::Key::Num2,
            egui::Key::Num3,
            egui::Key::Num4,
        ]
        .into_iter()
        .enumerate()
        {
            if i.key_pressed(key) {
                out.push(Action::Tab(n));
            }
        }
    }
    out
}

/// 在一串 id 里往前/往后挪一格。
///
/// 三条边界都不绕回:
/// - 列表空 → `None`(没得选)
/// - 当前没选中 → 落到第一条(`Next`)/ 最后一条(`Prev`),这是用户按方向键
///   时唯一说得通的起点
/// - 已经在头/尾 → 停住。绕回会让人「按住↓」时莫名其妙跳回列表开头,
///   而列表是有分组的,跳回去连自己在哪一组都看不出来。
pub(super) fn step(
    order: &[mullion_store::SessionId],
    cur: Option<mullion_store::SessionId>,
    forward: bool,
) -> Option<mullion_store::SessionId> {
    if order.is_empty() {
        return None;
    }
    let Some(cur) = cur.and_then(|c| order.iter().position(|x| *x == c)) else {
        return Some(if forward {
            order[0]
        } else {
            order[order.len() - 1]
        });
    };
    if forward {
        order.get(cur + 1).copied()
    } else {
        cur.checked_sub(1).map(|i| order[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_store::SessionId;

    /// `InputState::modifiers` 取自 `RawInput::modifiers`,**不是**从
    /// `Event::Key` 里那份读出来的 —— 只在事件上带 `command: true`,
    /// `i.modifiers.command` 仍是 `false`(第一版这条测试就栽在这儿)。
    fn input(events: Vec<egui::Event>, modifiers: egui::Modifiers) -> egui::InputState {
        let ctx = egui::Context::default();
        let mut got = None;
        let _ = ctx.run(
            egui::RawInput {
                events,
                modifiers,
                ..Default::default()
            },
            |ctx| got = Some(ctx.input(|i| i.clone())),
        );
        got.expect("闭包必须跑到底")
    }

    fn key(k: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    /// 走查 16:正在文本框里打字时,↑↓ / Enter 必须让位 —— 那是光标移动和
    /// 换行。抢过来的话,用户在「备注」里按个回车就跳去连另一台机器了。
    ///
    /// 自证会变红:把 `scan` 里的 `if !typing` 去掉,第二段断言炸。
    #[test]
    fn arrows_and_enter_stand_down_while_you_are_typing() {
        let none = egui::Modifiers::default();
        let ev = || {
            vec![
                key(egui::Key::ArrowDown, none),
                key(egui::Key::Enter, none),
                key(egui::Key::Escape, none),
            ]
        };

        let free = scan(&input(ev(), none), false);
        assert!(free.contains(&Action::Next));
        assert!(free.contains(&Action::Open));
        assert!(free.contains(&Action::Close));

        let typing = scan(&input(ev(), none), true);
        assert!(
            typing.is_empty(),
            "焦点在文本框里时这三个键都归文本框:{typing:?}"
        );
    }

    /// 走查 16:Ctrl+1..4 切 Tab,**不看**有没有在打字 —— 文本框不认这个组合,
    /// 让位没有意义,反而让「填着表想去看看别的页」这条路走不通。
    ///
    /// 自证会变红:把 `i.modifiers.command` 去掉,第三段(光按 2 不该切页)炸。
    #[test]
    fn ctrl_digits_switch_tabs_even_mid_edit() {
        let cmd = egui::Modifiers {
            command: true,
            ..Default::default()
        };
        assert_eq!(
            scan(&input(vec![key(egui::Key::Num1, cmd)], cmd), true),
            vec![Action::Tab(0)]
        );
        assert_eq!(
            scan(&input(vec![key(egui::Key::Num4, cmd)], cmd), false),
            vec![Action::Tab(3)]
        );
        assert!(
            scan(
                &input(
                    vec![key(egui::Key::Num2, egui::Modifiers::default())],
                    egui::Modifiers::default()
                ),
                false
            )
            .is_empty(),
            "光按数字键不该切页 —— 那是在往框里打字"
        );
    }

    /// 走查 16:方向键在列表两端**停住,不绕回**。绕回会让「按住 ↓」的人
    /// 莫名其妙回到列表开头,而列表是按分组分段的,跳回去连自己在哪一段都
    /// 看不出来。没选中时按方向键要有个说得通的起点。
    ///
    /// 自证会变红:把 `order.get(cur + 1)` 改成 `Some(order[(cur + 1) % len])`,
    /// 第四段(到底了该停住)炸。
    #[test]
    fn stepping_stops_at_both_ends_instead_of_wrapping_around() {
        let order = [SessionId(1), SessionId(2), SessionId(3)];

        assert_eq!(
            step(&order, None, true),
            Some(SessionId(1)),
            "没选中时↓落到第一条"
        );
        assert_eq!(
            step(&order, None, false),
            Some(SessionId(3)),
            "没选中时↑落到最后一条"
        );
        assert_eq!(step(&order, Some(SessionId(2)), true), Some(SessionId(3)));
        assert_eq!(step(&order, Some(SessionId(3)), true), None, "到底了停住");
        assert_eq!(step(&order, Some(SessionId(1)), false), None, "到顶了停住");
        assert_eq!(step(&[], Some(SessionId(1)), true), None, "空列表没得选");

        // 选中的那条已经被搜索过滤掉了(不在 order 里)→ 按方向键从头开始,
        // 而不是原地不动让用户以为键盘坏了。
        assert_eq!(step(&order, Some(SessionId(9)), true), Some(SessionId(1)));
    }
}
