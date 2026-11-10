//! F343:项目列表搜索框的键盘导航(启动页 / 切换项目弹窗共用)。
//!
//! 搜索框握着焦点时:↓/↑ 在**当前显示的行**里挪高亮,Enter 打开高亮项。
//! 项目管理器左栏不在此列(点行只是「选中去右栏编辑」,不是打开)。
//!
//! 判定拆成纯函数([`step`] / [`open_target`]),egui 只在 [`scan`] 里碰。

use egui::{Event, Key, Modifiers};

/// 在 `len` 行里把高亮挪一格。
///
/// 不绕回(同会话管理器的 `keys::step`):按住 ↓ 时莫名跳回开头,人会
/// 看不出自己在哪。没有高亮时 ↓ 落第一行、↑ 落最后一行。
pub fn step(cur: Option<usize>, len: usize, forward: bool) -> Option<usize> {
    if len == 0 {
        return None;
    }
    match cur {
        None => Some(if forward { 0 } else { len - 1 }),
        Some(c) if forward => Some((c + 1).min(len - 1)),
        Some(c) => Some(c.saturating_sub(1).min(len - 1)),
    }
}

/// Enter 该打开第几行。
///
/// - 按过方向键(有高亮):就是高亮那行。
/// - 没按过方向键:**只有搜索词非空时**才默认第一行 —— 搜完直接回车是
///   最常见的手势;而空搜索框里敲回车就打开第一个项目,是误触。
pub fn open_target(cur: Option<usize>, len: usize, searching: bool) -> Option<usize> {
    match cur {
        Some(c) if c < len => Some(c),
        _ if searching && len > 0 => Some(0),
        _ => None,
    }
}

/// 一帧扫出来的结论。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Nav {
    /// 高亮这一帧被方向键挪动过 —— 调用方据此把那一行滚进视野
    /// (每帧都滚会跟用户的滚轮打架)。
    pub moved: bool,
    /// 回车要打开的行。
    pub open: Option<usize>,
}

/// 扫这一帧的键盘。**必须在画搜索框之前调用**:单行 `TextEdit` 自己也吃
/// ↑↓/Enter(挪光标、失焦),先消费掉它才拿不到。
///
/// 只在搜索框握着焦点时响应 —— 焦点在别处(别的输入框、按钮)时这几个键
/// 不是我们的。`sel` 会被夹到 `len` 之内(搜索词变了、行变少了)。
pub fn scan(
    ctx: &egui::Context,
    search_id: egui::Id,
    sel: &mut Option<usize>,
    len: usize,
    searching: bool,
) -> Nav {
    if sel.is_some_and(|s| s >= len) {
        *sel = None;
    }
    if !ctx.memory(|m| m.has_focus(search_id)) {
        return Nav::default();
    }
    let mut nav = Nav::default();
    let (down, up, enter) = ctx.input_mut(|i| {
        // 组字提交与回车同帧到达时,这个回车是「确认候选」的那一下,
        // 不是「打开项目」—— 不消费、不响应。
        let committing = i
            .events
            .iter()
            .any(|e| matches!(e, Event::Ime(egui::ImeEvent::Commit(_))));
        (
            i.consume_key(Modifiers::NONE, Key::ArrowDown),
            i.consume_key(Modifiers::NONE, Key::ArrowUp),
            !committing && i.consume_key(Modifiers::NONE, Key::Enter),
        )
    });
    if down || up {
        // 同帧按了两个方向键(按住重复)时,以最后到的为准没法分辨,按 ↓ 算。
        *sel = step(*sel, len, down);
        nav.moved = true;
    }
    if enter {
        nav.open = open_target(*sel, len, searching);
    }
    nav
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stepping_stops_at_both_ends_instead_of_wrapping() {
        assert_eq!(step(None, 0, true), None);
        assert_eq!(step(None, 3, true), Some(0));
        assert_eq!(step(None, 3, false), Some(2));
        assert_eq!(step(Some(1), 3, true), Some(2));
        assert_eq!(step(Some(2), 3, true), Some(2));
        assert_eq!(step(Some(0), 3, false), Some(0));
        // 行变少之后高亮还指着越界的位置:夹回去,不 panic。
        assert_eq!(step(Some(9), 3, false), Some(2));
    }

    #[test]
    fn enter_opens_the_highlight_else_the_first_row_only_when_searching() {
        assert_eq!(open_target(Some(1), 3, false), Some(1));
        assert_eq!(open_target(None, 3, true), Some(0));
        assert_eq!(
            open_target(None, 3, false),
            None,
            "空搜索框回车不该打开东西"
        );
        assert_eq!(open_target(None, 0, true), None);
        assert_eq!(open_target(Some(5), 3, true), Some(0), "越界的高亮视同没有");
    }

    /// 跑 `frames` 帧:第一帧让搜索框拿到焦点,之后按给定事件喂键。
    fn drive(
        keys: &[Key],
        focused: bool,
        sel: &mut Option<usize>,
        len: usize,
        searching: bool,
    ) -> Nav {
        let ctx = egui::Context::default();
        let id = egui::Id::new("nav_test_search");
        let mut text = String::from("x");
        let mut out = Nav::default();
        for frame in 0..3 {
            let events = if frame == 2 {
                keys.iter()
                    .map(|k| Event::Key {
                        key: *k,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: Modifiers::NONE,
                    })
                    .collect()
            } else {
                vec![]
            };
            let input = egui::RawInput {
                events,
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                if frame == 2 {
                    out = scan(ctx, id, sel, len, searching);
                }
                egui::CentralPanel::default().show(ctx, |ui| {
                    let r = ui.add(egui::TextEdit::singleline(&mut text).id(id));
                    if focused && frame == 0 {
                        r.request_focus();
                    }
                });
            });
        }
        out
    }

    /// 自证会变红:把 `scan` 里的 `has_focus` 判断删掉 —— 第一条断言之外的
    /// 「没焦点」那条红。
    #[test]
    fn keys_only_count_while_the_search_box_has_focus() {
        let mut sel = None;
        let nav = drive(&[Key::ArrowDown], true, &mut sel, 3, true);
        assert!(nav.moved);
        assert_eq!(sel, Some(0));

        let mut sel = None;
        let nav = drive(&[Key::ArrowDown, Key::Enter], false, &mut sel, 3, true);
        assert_eq!(nav, Nav::default(), "焦点不在搜索框:方向键/回车不是我们的");
        assert_eq!(sel, None);
    }

    #[test]
    fn enter_after_arrowing_opens_that_row_and_bare_enter_opens_the_first() {
        let mut sel = Some(1);
        let nav = drive(&[Key::Enter], true, &mut sel, 3, true);
        assert_eq!(nav.open, Some(1));

        let mut sel = None;
        let nav = drive(&[Key::Enter], true, &mut sel, 3, true);
        assert_eq!(nav.open, Some(0));

        let mut sel = None;
        let nav = drive(&[Key::Enter], true, &mut sel, 3, false);
        assert_eq!(nav.open, None);
    }
}
