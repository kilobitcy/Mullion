//! F71:启动解锁框 —— `secrets.enc` 由主密码加密时,先问密码再打开会话库。
//!
//! **零 IO**:这里只收一串密码并回报一个 [`UnlockOut`],真的开库
//! (`Vault::open_with`)在 `app.rs`。`mullion-store` 永远不会主动索要密码
//! (零 UI 是架构不变量,设计 D10)。

use crate::theme::{self, Theme};
use crate::ui::annotate;
use crate::ui::metrics::{field_w, FIELD_W_M, SP_L, SP_M, SP_S, SP_XS};

/// 解锁框自己的那点状态。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UnlockDraft {
    /// 输入框里的密码。
    pub password: String,
    /// 已经错过一次。红字只在**试过之后**出现 —— 一打开就红着等于在指责
    /// 用户还没做的事。
    pub failed: bool,
}

/// 这一帧用户干了什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnlockOut {
    /// 还在打字。
    None,
    /// 提交这串密码(点「解锁」或在输入框里按回车)。
    Submit,
    /// 「退出」:关窗口退出进程。
    Quit,
}

/// 画解锁框。返回这一帧的结论。
pub fn show(ctx: &egui::Context, t: &Theme, draft: &mut UnlockDraft) -> UnlockOut {
    let mut out = UnlockOut::None;
    egui::Window::new("需要主密码")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            annotate::mark(ui.ctx(), "解锁弹窗", ui.max_rect());
            let avail = ui.available_width();
            ui.label(theme::hint_text(
                t,
                "这台机器上的 secrets.enc 由主密码加密,解开之后才能用会话库。",
            ));
            ui.add_space(SP_M);
            ui.horizontal(|ui| {
                ui.label("主密码");
                ui.add_space(SP_S);
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut draft.password)
                        .password(true)
                        .desired_width(field_w(avail, FIELD_W_M, 0.0)),
                );
                // 回车 = 解锁。`lost_focus()` 单独一个条件不够 —— 点到别处也会
                // 失焦,那种情况下不该当成提交。
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    out = UnlockOut::Submit;
                }
                // 弹窗一出现就把焦点放进输入框:用户开机第一件事就是打密码,
                // 还要先点一下框才有反应是纯粹的摩擦。判据是「这一帧谁都没有
                // 焦点」而不是「第一帧」——后者要多存一个帧计数,而且用户主动
                // 点到「退出」按钮上之后会被抢回来。
                if ui.ctx().memory(|m| m.focused().is_none()) {
                    resp.request_focus();
                }
            });
            // 内联红字紧贴它解释的那个字段(表单规范 #5)。
            if draft.failed {
                ui.add_space(SP_XS);
                ui.label(
                    egui::RichText::new("密码不对,再试一次")
                        .size(11.0)
                        .color(theme::c32(t.danger)),
                );
            }
            ui.add_space(SP_L);
            ui.horizontal(|ui| {
                // 空密码点不动:提交空串只会换来同一句「密码不对」,
                // 而那句话会让用户以为自己记错了密码。
                let can = !draft.password.is_empty();
                if ui
                    .add_enabled(can, egui::Button::new("解锁"))
                    .on_disabled_hover_text("先输入主密码")
                    .clicked()
                {
                    out = UnlockOut::Submit;
                }
                ui.add_space(SP_S);
                // **不提供「跳过」**:跳过意味着带着一个解不开的库继续跑,
                // 然后在用户双击会话时才炸 —— 那时他早忘了自己跳过了解锁。
                if ui.button("退出").clicked() {
                    out = UnlockOut::Quit;
                }
            });
        });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 跑两帧并收本帧画出来的文字。两帧的理由同 `ui/settings.rs` 的 `run`。
    fn run(d: &mut UnlockDraft) -> (Vec<String>, UnlockOut) {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut out = UnlockOut::None;
        let mut shapes = Vec::new();
        for _ in 0..2 {
            let full = ctx.run(egui::RawInput::default(), |ctx| {
                out = show(ctx, &t, d);
            });
            shapes = full.shapes;
        }
        let mut texts = Vec::new();
        fn walk(shape: &egui::Shape, acc: &mut Vec<String>) {
            match shape {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, acc)),
                egui::Shape::Text(ts) => acc.push(ts.galley.text().to_string()),
                _ => {}
            }
        }
        for cs in &shapes {
            walk(&cs.shape, &mut texts);
        }
        (texts, out)
    }

    /// 点一下写着 `label` 的部件,返回那一帧的结论。
    fn click(d: &mut UnlockDraft, label: &str) -> UnlockOut {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut shapes = Vec::new();
        for _ in 0..2 {
            let full = ctx.run(egui::RawInput::default(), |ctx| {
                show(ctx, &t, d);
            });
            shapes = full.shapes;
        }
        fn find(shape: &egui::Shape, label: &str) -> Option<egui::Pos2> {
            match shape {
                egui::Shape::Vec(v) => v.iter().find_map(|s| find(s, label)),
                egui::Shape::Text(ts) if ts.galley.text() == label => {
                    Some(ts.pos + ts.galley.size() / 2.0)
                }
                _ => None,
            }
        }
        let pos = shapes
            .iter()
            .find_map(|cs| find(&cs.shape, label))
            .unwrap_or_else(|| panic!("解锁框里没有写着「{label}」的部件"));
        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::PointerMoved(pos));
        for pressed in [true, false] {
            input.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            });
        }
        let mut out = UnlockOut::None;
        let _ = ctx.run(input, |ctx| {
            out = show(ctx, &t, d);
        });
        out
    }

    fn filled() -> UnlockDraft {
        UnlockDraft {
            password: "hunter2".into(),
            failed: false,
        }
    }

    /// 光画一帧不该产生任何动作 —— 否则弹窗一出现就拿空密码去试,
    /// 于是第一眼看到的就是「密码不对」。
    #[test]
    fn merely_showing_the_dialog_changes_nothing() {
        let mut d = filled();
        let before = d.clone();
        let (_, out) = run(&mut d);
        assert_eq!(out, UnlockOut::None);
        assert_eq!(d, before);
    }

    #[test]
    fn the_unlock_button_submits() {
        assert_eq!(click(&mut filled(), "解锁"), UnlockOut::Submit);
    }

    /// 「退出」必须与「提交」分开回报:调用方拿它去关窗口。混成一个的话,
    /// 用户点退出的结果是又试一次密码。
    #[test]
    fn quit_reports_quit_so_the_caller_can_close_the_window() {
        assert_eq!(click(&mut filled(), "退出"), UnlockOut::Quit);
    }

    /// 红字只在**试过之后**出现。一打开就红着等于在指责用户还没做的事,
    /// 而且真错过一次时那行字就不再是新信息了。
    #[test]
    fn the_error_line_only_shows_after_a_failed_try() {
        let (before, _) = run(&mut filled());
        assert!(
            !before.iter().any(|s| s.contains("密码不对")),
            "还没试过就先报错:{before:?}"
        );
        let mut d = UnlockDraft {
            password: String::new(),
            failed: true,
        };
        let (after, _) = run(&mut d);
        assert!(
            after.iter().any(|s| s.contains("密码不对")),
            "试错了却什么都不说:{after:?}"
        );
    }

    /// 空密码提交不了 —— 提交空串只会换来同一句「密码不对」,而那句话
    /// 会让用户以为自己记错了密码。
    #[test]
    fn an_empty_password_cannot_be_submitted() {
        let mut d = UnlockDraft::default();
        let (_, out) = run(&mut d);
        assert_eq!(out, UnlockOut::None);
        assert_eq!(click(&mut d, "解锁"), UnlockOut::None, "空密码不该能提交");
    }

    /// 不提供「跳过」(设计 §3.1):跳过 = 带着一个解不开的库继续跑,
    /// 在用户双击会话时才炸。这条钉住的是「以后别顺手加一个」。
    #[test]
    fn there_is_no_skip_escape_hatch() {
        let (texts, _) = run(&mut filled());
        assert!(
            !texts.iter().any(|s| s.contains("跳过")),
            "解锁框里出现了「跳过」:{texts:?}"
        );
    }
}
