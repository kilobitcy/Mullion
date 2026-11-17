//! F366:下载覆盖失败的告警框。
//!
//! 方向是远端 → 本机:用户在冲突框里明确选了「覆盖」,而本机目标被 Excel/WPS
//! 等独占打开(Windows os error 32)或只读 —— 原先只有传输面板一行红字,面板
//! 收起 / F308 自动清掉之后就完全看不见了。
//!
//! 只管「选了覆盖的下载」;其它下载失败维持面板红字。一批里先后失败的几条
//! **合并进同一个框**(框开着时再来的追加进去),多了折成「等另外 N 个」。
//!
//! 重试要用的那份传输输入(`TransferSpec`)由 app 侧按 [`Item::job`] 另存一份:
//! `TransferDone` 里凡收尾即回收 spec,不另存的话点「重试」时路径已经丢了。
//! (`TransferSpec` 是 `app.rs` 私有类型,`ui/` 这一层不认识它。)

/// 告警框里的一条。
pub struct Item {
    /// 文件名(传输面板那一行的 label)。
    pub label: String,
    /// 翻译后的原因(`files::fail::overwrite_reason`)。
    pub reason: String,
    /// 旧 job 的 id —— 重试时要把它从队列里摘掉(F308)。
    pub job: u64,
    /// 旧 job 的总字节数,重试入队时照抄。
    pub total: u64,
}

/// 告警框的全部状态。`None` = 关着。
pub struct OverwriteAlert {
    pub items: Vec<Item>,
}

/// 用户在框里点了什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertOut {
    /// 只重跑失败的那几个,仍按覆盖。
    Retry,
    Close,
}

/// 窗口标题。`app.rs` 不登记 area(豁免「点外面关」),但标题仍集中在这里。
pub(crate) const WINDOW_TITLE: &str = "覆盖失败";

/// 正文宽度上界,同 `pack_dialog` 的档位。
const MAX_W: f32 = crate::ui::metrics::FIELD_W_M
    + crate::ui::metrics::LABEL_COL_W
    + crate::ui::metrics::SP_L * 2.0;

/// 记一条失败。框开着就追加(同一批合并),没开就开一个。
pub fn push_failure(
    alert: &mut Option<OverwriteAlert>,
    label: String,
    raw: &str,
    job: u64,
    total: u64,
) {
    let item = Item {
        label,
        reason: crate::files::fail::overwrite_reason(raw),
        job,
        total,
    };
    alert
        .get_or_insert_with(|| OverwriteAlert { items: Vec::new() })
        .items
        .push(item);
}

/// 框里列出的那几行:「文件名:原因」,多了折成「等另外 N 个」(同 F330 冲突框)。
pub fn visible_lines(items: &[Item]) -> Vec<String> {
    let all: Vec<String> = items
        .iter()
        .map(|i| format!("{}:{}", i.label, i.reason))
        .collect();
    crate::paste_files::conflict_lines(&all)
}

/// 画告警框。返回本帧的出口;`Retry`/`Close` 时由调用方施加并关框
/// (重试要按 `items` 里的 job 取回 app 侧另存的 spec)。
pub fn show(
    ctx: &egui::Context,
    t: &crate::theme::Theme,
    alert: &Option<OverwriteAlert>,
) -> Option<AlertOut> {
    let a = alert.as_ref()?;
    let mut out = None;
    egui::Window::new(WINDOW_TITLE)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            crate::ui::annotate::mark(ui.ctx(), "覆盖失败告警".to_string(), ui.max_rect());
            // 文件名可以很长:夹住宽度让每行折行,不让它把窗口撑出屏幕(F285)。
            ui.set_max_width(MAX_W);
            ui.label(
                egui::RichText::new(format!("{} 个文件没能覆盖到本机:", a.items.len()))
                    .color(crate::theme::c32(t.warn)),
            );
            ui.add_space(crate::ui::metrics::SP_S);
            for line in visible_lines(&a.items) {
                ui.label(line);
            }
            ui.add_space(crate::ui::metrics::SP_M);
            ui.horizontal(|ui| {
                if ui.button("重试").clicked() {
                    out = Some(AlertOut::Retry);
                }
                if ui.button("关闭").clicked() {
                    out = Some(AlertOut::Close);
                }
            });
        });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// F366:同一批先后失败的几条合并进一个框,不是一条一个框排队弹。
    #[test]
    fn failures_in_one_batch_merge_into_one_alert() {
        let mut a: Option<OverwriteAlert> = None;
        push_failure(&mut a, "a.xlsx".into(), "x (os error 32)", 1, 10);
        push_failure(&mut a, "b.xlsx".into(), "x (os error 5)", 2, 20);
        let a = a.unwrap();
        assert_eq!(a.items.len(), 2);
        assert!(a.items[0].reason.contains("占用"), "原因没翻译");
        assert!(a.items[1].reason.contains("没有写入权限"));
    }

    /// F366:框里每行带原因;多了折叠,框不会被几百行撑出屏幕。
    #[test]
    fn each_line_carries_its_reason_and_the_list_folds_after_the_limit() {
        let mut a: Option<OverwriteAlert> = None;
        for i in 0..12 {
            push_failure(&mut a, format!("f{i}"), "x (os error 32)", i, 0);
        }
        let lines = visible_lines(&a.unwrap().items);
        assert!(lines[0].starts_with("f0:") && lines[0].contains("占用"));
        assert_eq!(lines.last().unwrap(), "等另外 4 个");
    }

    /// 点一下写着 `label` 的那颗按钮,返回这一帧 `show` 的结论(同 `history::tests::click`)。
    fn click(alert: &Option<OverwriteAlert>, label: &str) -> Option<AlertOut> {
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
                    show(ctx, &t, alert);
                })
                .shapes;
        }
        let pos = shapes
            .iter()
            .find_map(|cs| find(&cs.shape, label))
            .unwrap_or_else(|| panic!("框里没有写着「{label}」的控件"));
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
            out = show(ctx, &t, alert);
        });
        out
    }

    /// F366:两颗按钮各自带出自己的出口(施加由 app 侧做)。
    #[test]
    fn the_two_buttons_report_retry_and_close() {
        let mut a: Option<OverwriteAlert> = None;
        push_failure(&mut a, "a.xlsx".into(), "x (os error 32)", 1, 0);
        assert_eq!(click(&a, "重试"), Some(AlertOut::Retry));
        assert_eq!(click(&a, "关闭"), Some(AlertOut::Close));
    }
}
