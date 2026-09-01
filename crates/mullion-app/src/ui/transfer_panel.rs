//! F55/F59:底部传输队列面板。折叠时一行摘要,展开是逐条列表。
//!
//! **队列空时整个面板不画** —— 常驻一条空条只是在偷终端的行数,
//! 而终端行数正是这个项目最不该浪费的东西。

use crate::files::queue::{Direction, Job, JobState, Queue, Summary};
use crate::theme::{self, Theme};

/// 面板上按下的东西。`app.rs` 拿去改队列 —— 这里不碰队列本身
/// (egui 闭包借不到 `&mut App`,而且队列改动要跟取消旗标一起做)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferUiAction {
    Cancel(u64),
    CancelAll,
    ClearFinished,
}

/// 画面板。`expanded` 由调用方持有(`UiState`),跨帧记住折叠状态。
/// 用「展开」而不是「折叠」表述,是为了 `Default`(`false`)正好等于
/// 默认折叠 —— 见 `UiState::transfer_expanded` 的说明。
pub fn show(
    ctx: &egui::Context,
    t: &Theme,
    queue: &Queue,
    expanded: &mut bool,
) -> Option<TransferUiAction> {
    if queue.jobs().is_empty() {
        return None;
    }
    let s = queue.summary();
    let mut action = None;
    egui::TopBottomPanel::bottom("transfer-queue")
        .frame(
            egui::Frame::none()
                .fill(theme::c32(t.panel_bg))
                .inner_margin(egui::Margin::symmetric(8.0, 4.0))
                .stroke(theme::stroke(t)),
        )
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                // F143:原先那两个三角字符都不在 GBK,画出来是豆腐。三角
                // 改自绘,文字仍走普通按钮 —— 两个控件并排,点哪个都翻折叠。
                let g = if *expanded {
                    crate::ui::icon::Glyph::TriangleDown
                } else {
                    crate::ui::icon::Glyph::TriangleRight
                };
                let tip = if *expanded {
                    "折起传输队列"
                } else {
                    "展开传输队列"
                };
                if crate::ui::icon::icon_button(ui, g, true, tip) {
                    *expanded = !*expanded;
                }
                if ui.button("传输").clicked() {
                    *expanded = !*expanded;
                }
                ui.colored_label(theme::c32(t.fg_mid), summary_line(&s, queue.rate_bps()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("清除已完成").clicked() {
                        action = Some(TransferUiAction::ClearFinished);
                    }
                    if s.busy && ui.button("全部取消").clicked() {
                        action = Some(TransferUiAction::CancelAll);
                    }
                });
            });
            if !*expanded {
                return;
            }
            ui.separator();
            egui::ScrollArea::vertical()
                .max_height(160.0)
                .show(ui, |ui| {
                    for j in queue.jobs() {
                        ui.horizontal(|ui| {
                            ui.colored_label(
                                theme::c32(t.fg_dim),
                                match j.dir {
                                    Direction::Upload => "↑",
                                    Direction::Download => "↓",
                                },
                            );
                            ui.label(&j.label);
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if !j.state.is_finished() && ui.button("取消").clicked() {
                                        action = Some(TransferUiAction::Cancel(j.id));
                                    }
                                    ui.colored_label(state_color(t, j), state_text(j));
                                },
                            );
                        });
                    }
                });
        });
    action
}

fn summary_line(s: &Summary, bps: f64) -> String {
    if !s.busy {
        return "全部完成".into();
    }
    let rate = crate::files::human_size(bps as u64);
    format!("↑{} ↓{} · {rate}/s · {}", s.up, s.down, eta(s, bps))
}

/// `剩余 00:41`。速率还没估出来时**不瞎猜** —— 一个跳来跳去的 ETA
/// 比没有 ETA 更糟,用户会照着它安排事情。
fn eta(s: &Summary, bps: f64) -> String {
    if bps <= 1.0 || s.bytes_total <= s.bytes_done {
        return "剩余 --:--".into();
    }
    let secs = ((s.bytes_total - s.bytes_done) as f64 / bps) as u64;
    format!("剩余 {:02}:{:02}", secs / 60, secs % 60)
}

/// 每一条右侧那一格文字。**失败要写原因**,不是一个红点 —— 传输失败的
/// 原因(没权限 / 目录不存在 / 连接断了)决定用户下一步怎么办。
fn state_text(j: &Job) -> String {
    match &j.state {
        JobState::Pending => "排队中".into(),
        JobState::Running => {
            let pct = (j.done * 100).checked_div(j.total).unwrap_or(0).min(100);
            format!("{pct}%")
        }
        JobState::Conflict => "等待处置".into(),
        JobState::Done => "完成".into(),
        JobState::Skipped => "已跳过".into(),
        JobState::Canceled => "已取消".into(),
        JobState::Failed(m) => m.clone(),
    }
}

fn state_color(t: &Theme, j: &Job) -> egui::Color32 {
    theme::c32(match j.state {
        JobState::Failed(_) => t.danger_text,
        JobState::Conflict => t.warn,
        JobState::Done => t.ok,
        _ => t.fg_dim,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::queue::{Direction, NewJob, Queue};

    fn push(q: &mut Queue, dir: Direction, label: &str) -> u64 {
        q.push(NewJob {
            dir,
            generation: 1,
            label: label.into(),
            total: 100,
        })
    }

    /// 面板画出来的**全部文字**。`Panel` 首帧 `fade_in` 只记 `Shape::Noop`,
    /// 所以必须跑两帧才看得到内容(见 `docs/gui-render-gotchas.md`)。
    fn texts(q: &Queue, expanded: &mut bool) -> Vec<String> {
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
                    show(ctx, &t, q, expanded);
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
    fn an_empty_queue_draws_nothing_so_it_does_not_eat_terminal_rows() {
        let q = Queue::new(4);
        let mut expanded = false;
        assert!(
            texts(&q, &mut expanded).is_empty(),
            "队列空时不该占地方:{:?}",
            texts(&q, &mut expanded)
        );
    }

    #[test]
    fn the_collapsed_summary_shows_both_directions_and_the_rate() {
        let mut q = Queue::new(4);
        push(&mut q, Direction::Upload, "a");
        push(&mut q, Direction::Download, "b");
        q.take_runnable();
        let mut expanded = false;
        let ts = texts(&q, &mut expanded).join(" ");
        assert!(ts.contains("↑1"), "少了上行条数:{ts}");
        assert!(ts.contains("↓1"), "少了下行条数:{ts}");
        assert!(ts.contains("/s"), "少了速率:{ts}");
    }

    /// 折叠时**不该**逐条画出来 —— 折叠的全部意义就是只占一行。
    #[test]
    fn the_collapsed_panel_hides_the_per_job_rows() {
        let mut q = Queue::new(4);
        push(&mut q, Direction::Upload, "报告.pdf");
        let mut expanded = false;
        let ts = texts(&q, &mut expanded).join(" ");
        assert!(!ts.contains("报告.pdf"), "折叠时不该画出每一条:{ts}");
    }

    #[test]
    fn the_expanded_list_names_every_job_so_a_failure_can_be_traced_to_a_file() {
        let mut q = Queue::new(4);
        push(&mut q, Direction::Upload, "报告.pdf");
        let mut expanded = true;
        let ts = texts(&q, &mut expanded).join(" ");
        assert!(ts.contains("报告.pdf"), "展开后应当看得到文件名:{ts}");
    }

    #[test]
    fn a_failed_job_shows_its_reason_instead_of_just_a_red_dot() {
        let mut q = Queue::new(4);
        let id = push(&mut q, Direction::Upload, "a");
        q.take_runnable();
        q.finish(id, Err("没权限".into()));
        let mut expanded = true;
        let ts = texts(&q, &mut expanded).join(" ");
        assert!(ts.contains("没权限"), "失败原因得写出来:{ts}");
    }

    /// 速率没估出来时不给一个瞎猜的 ETA。
    #[test]
    fn the_eta_stays_blank_until_the_rate_is_known() {
        let s = Summary {
            up: 1,
            down: 0,
            bytes_done: 0,
            bytes_total: 1000,
            busy: true,
            active: 1,
        };
        assert!(eta(&s, 0.0).contains("--:--"), "速率未知时不该给数字");
        assert_eq!(eta(&s, 100.0), "剩余 00:10");
    }
}
