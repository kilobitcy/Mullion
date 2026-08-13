//! F53/设计 D13:底部「编辑中」列表。
//!
//! 监视是**显式结束**的(不猜编辑器进程退没退),所以必须有一处看得见
//! 「现在有哪些文件挂在监视里」,否则用户根本不知道自己还欠着几个回传。
//!
//! 与传输面板同一条纪律:**列表空时整个面板不画** —— 常驻一条空条只是在
//! 偷终端的行数。

use crate::edit::sessions::{EditEntry, EditKind, EditSessions, EditState};
use crate::theme::{self, Theme};

/// 面板上按下的东西。真正的动作(删临时文件、发写回)一律回 `app.rs`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditUiAction {
    /// 结束监视这一条(临时文件一并删掉)。
    End(u64),
    /// 冲突那一条:重新把处置框打开(用户可能上次点了取消)。
    Resolve(u64),
}

/// 退出确认框上的选择(D3-12)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitChoice {
    /// 丢掉这些没传上去的改动,继续退出。
    Anyway,
    /// 回去处理。
    Cancel,
}

/// F53/D3-12:有「改了没传上去」的编辑时,第一次点关闭要拦一下。
///
/// **必须逐条列出是哪些文件**。只说「还有未保存的编辑」的话,用户没法判断
/// 那是他刚才随手打开看一眼的东西,还是一个改了半小时的配置 —— 于是他要么
/// 盲目丢弃,要么盲目取消再挨个翻。
pub fn show_exit_confirm(
    ctx: &egui::Context,
    t: &Theme,
    edits: &EditSessions,
) -> Option<ExitChoice> {
    let mut choice = None;
    egui::Window::new("还有改动没传回远端")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            crate::ui::annotate::mark(ui.ctx(), "退出确认框".to_string(), ui.max_rect());
            for e in edits.iter().filter(|e| e.has_unsent_changes()) {
                ui.colored_label(
                    state_color(t, e),
                    format!("{} —— {}", e.label, state_text(e)),
                );
            }
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(
                    "现在退出,这些改动就只剩本地临时文件那一份,而临时目录会被一并清掉。",
                )
                .color(theme::c32(t.fg_dim)),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("回去处理").clicked() {
                    choice = Some(ExitChoice::Cancel);
                }
                if ui
                    .button(
                        egui::RichText::new("仍然退出(丢弃这些改动)").color(theme::c32(t.danger)),
                    )
                    .clicked()
                {
                    choice = Some(ExitChoice::Anyway);
                }
            });
        });
    choice
}

pub fn show(
    ctx: &egui::Context,
    t: &Theme,
    edits: &EditSessions,
    expanded: &mut bool,
) -> Option<EditUiAction> {
    if edits.is_empty() {
        return None;
    }
    let mut action = None;
    let total = edits.iter().count();
    let pending = edits.iter().filter(|e| e.has_unsent_changes()).count();
    egui::TopBottomPanel::bottom("edit-list")
        .frame(
            egui::Frame::none()
                .fill(theme::c32(t.panel_bg))
                .inner_margin(egui::Margin::symmetric(8.0, 4.0))
                .stroke(theme::stroke(t)),
        )
        .show(ctx, |ui| {
            crate::ui::annotate::mark(ui.ctx(), "编辑中列表".to_string(), ui.max_rect());
            ui.horizontal(|ui| {
                let arrow = if *expanded { "▾" } else { "▸" };
                if ui.button(format!("{arrow} 编辑中")).clicked() {
                    *expanded = !*expanded;
                }
                ui.colored_label(theme::c32(t.fg_mid), format!("{total} 个文件"));
                // 有「改了没传上去」的必须在**折叠态**也看得见 —— 折叠是常态,
                // 只在展开时才提示等于没提示。
                if pending > 0 {
                    ui.colored_label(theme::c32(t.warn), format!("{pending} 个待处置"));
                }
            });
            if !*expanded {
                return;
            }
            ui.separator();
            egui::ScrollArea::vertical()
                .max_height(140.0)
                .show(ui, |ui| {
                    for e in edits.iter() {
                        ui.horizontal(|ui| {
                            ui.colored_label(
                                theme::c32(t.fg_dim),
                                match e.kind {
                                    EditKind::External => "外部",
                                    EditKind::Inline => "内置",
                                },
                            );
                            ui.label(&e.label);
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button("结束编辑").clicked() {
                                        action = Some(EditUiAction::End(e.key));
                                    }
                                    if e.state == EditState::Conflict
                                        && ui.button("处置…").clicked()
                                    {
                                        action = Some(EditUiAction::Resolve(e.key));
                                    }
                                    ui.colored_label(state_color(t, e), state_text(e));
                                },
                            );
                        });
                    }
                });
        });
    action
}

/// 每一条右侧那一格。**冲突和失败要写清下一步怎么办**,不是一个红点 ——
/// 「远端被别人改过」和「没权限」对用户意味着完全不同的两件事。
fn state_text(e: &EditEntry) -> String {
    match &e.state {
        EditState::Watching if e.saved_once => "已回传,继续监视".into(),
        EditState::Watching => "监视中".into(),
        EditState::Uploading => "回传中…".into(),
        EditState::Conflict => "远端已被改动,等你决定".into(),
        EditState::Failed(why) => format!("回传失败:{why}"),
    }
}

fn state_color(t: &Theme, e: &EditEntry) -> egui::Color32 {
    theme::c32(match e.state {
        EditState::Failed(_) => t.danger,
        EditState::Conflict => t.warn,
        EditState::Uploading => t.fg_mid,
        EditState::Watching => t.fg_dim,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_ssh::sftp::RemotePath;
    use std::path::PathBuf;

    fn one(state: EditState) -> EditSessions {
        let mut s = EditSessions::new();
        let k = s.add(
            1,
            EditKind::External,
            RemotePath::from_bytes(b"/etc/nginx/nginx.conf".to_vec()),
            PathBuf::from("/tmp/nginx.conf"),
            (1, 1),
        );
        s.get_mut(k).unwrap().state = state;
        s
    }

    fn texts(edits: &EditSessions, expanded: &mut bool) -> Vec<String> {
        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                egui::Shape::Text(ts) => out.push(ts.galley.text().to_string()),
                _ => {}
            }
        }
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut shapes = Vec::new();
        // 两帧:`Panel` 首帧 fade_in 只记 `Shape::Noop`。
        for _ in 0..2 {
            shapes = ctx
                .run(egui::RawInput::default(), |ctx| {
                    show(ctx, &t, edits, expanded);
                })
                .shapes;
        }
        let mut out = Vec::new();
        for cs in &shapes {
            walk(&cs.shape, &mut out);
        }
        out
    }

    #[test]
    fn an_empty_edit_list_draws_nothing_so_it_does_not_eat_terminal_rows() {
        let s = EditSessions::new();
        let mut expanded = false;
        assert!(texts(&s, &mut expanded).is_empty());
    }

    /// 冲突要说清**下一步怎么办**。一个红点只会让用户去重启程序。
    #[test]
    fn a_conflicted_entry_says_what_happened_instead_of_just_turning_red() {
        let s = one(EditState::Conflict);
        let mut expanded = true;
        let joined = texts(&s, &mut expanded).join(" ");
        assert!(joined.contains("nginx.conf"), "该列出文件名:{joined}");
        assert!(joined.contains("远端已被改动"), "该说清原因:{joined}");
        assert!(joined.contains("处置"), "该给出口:{joined}");
    }

    /// 「改了没传上去」在**折叠态**就要看得见 —— 折叠是常态。
    #[test]
    fn a_pending_change_is_visible_even_while_the_panel_is_collapsed() {
        let s = one(EditState::Failed("没权限".into()));
        let mut expanded = false;
        let joined = texts(&s, &mut expanded).join(" ");
        assert!(joined.contains("待处置"), "折叠态也得提示:{joined}");
    }

    /// 退出确认框要**逐条点名**,而且只点有未传改动的那些。
    ///
    /// 只说一句「还有未保存的编辑」的话,用户没法判断那是随手打开看一眼的
    /// 东西还是改了半小时的配置 —— 于是要么盲目丢弃,要么盲目取消再挨个翻。
    #[test]
    fn the_exit_confirmation_names_the_files_that_would_lose_work() {
        let mut s = one(EditState::Failed("没权限".into()));
        // 一条干净的:不该被点名,否则「有几件事要处理」这个数就是错的。
        s.add(
            1,
            EditKind::External,
            RemotePath::from_bytes(b"/etc/hosts".to_vec()),
            PathBuf::from("/tmp/hosts"),
            (1, 1),
        );
        let joined = exit_texts(&s).join(" ");
        assert!(joined.contains("nginx.conf"), "该点名:{joined}");
        assert!(joined.contains("没权限"), "该说清是哪种没传上去:{joined}");
        assert!(
            !joined.contains("hosts"),
            "只是打开看着的那条不该被算进来:{joined}"
        );
    }

    #[test]
    fn the_exit_confirmation_offers_both_ways_out() {
        let s = one(EditState::Conflict);
        assert_eq!(
            exit_click(&s, "仍然退出(丢弃这些改动)"),
            Some(ExitChoice::Anyway)
        );
        assert_eq!(exit_click(&s, "回去处理"), Some(ExitChoice::Cancel));
    }

    /// 没按任何按钮的那一帧必须返回 `None`。返回别的东西的话,确认框
    /// 一开出来程序就自己退了。
    #[test]
    fn merely_showing_the_exit_confirmation_decides_nothing() {
        let s = one(EditState::Conflict);
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut out = None;
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                out = show_exit_confirm(ctx, &t, &s);
            });
        }
        assert_eq!(out, None);
    }

    fn exit_texts(edits: &EditSessions) -> Vec<String> {
        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                egui::Shape::Text(ts) => out.push(ts.galley.text().to_string()),
                _ => {}
            }
        }
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut shapes = Vec::new();
        for _ in 0..2 {
            shapes = ctx
                .run(egui::RawInput::default(), |ctx| {
                    show_exit_confirm(ctx, &t, edits);
                })
                .shapes;
        }
        let mut out = Vec::new();
        for cs in &shapes {
            walk(&cs.shape, &mut out);
        }
        out
    }

    /// 点一下退出确认框上写着 `label` 的按钮。
    fn exit_click(edits: &EditSessions, label: &str) -> Option<ExitChoice> {
        fn find(shape: &egui::Shape, label: &str) -> Option<egui::Pos2> {
            match shape {
                egui::Shape::Vec(v) => v.iter().find_map(|s| find(s, label)),
                egui::Shape::Text(ts) if ts.galley.text() == label => {
                    Some(ts.pos + ts.galley.size() / 2.0)
                }
                _ => None,
            }
        }
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut shapes = Vec::new();
        for _ in 0..2 {
            shapes = ctx
                .run(egui::RawInput::default(), |ctx| {
                    show_exit_confirm(ctx, &t, edits);
                })
                .shapes;
        }
        let pos = shapes
            .iter()
            .find_map(|cs| find(&cs.shape, label))
            .unwrap_or_else(|| panic!("确认框里没有写着「{label}」的按钮"));
        let mut input = egui::RawInput::default();
        for pressed in [true, false] {
            input.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            });
        }
        let mut out = None;
        let _ = ctx.run(input, |ctx| {
            out = show_exit_confirm(ctx, &t, edits);
        });
        out
    }

    /// 反面:一切正常时不该冒出「待处置」——狼来了喊多了就没人看了。
    #[test]
    fn a_healthy_entry_shows_no_warning() {
        let s = one(EditState::Watching);
        let mut expanded = false;
        let joined = texts(&s, &mut expanded).join(" ");
        assert!(!joined.contains("待处置"), "不该无端报警:{joined}");
    }
}
