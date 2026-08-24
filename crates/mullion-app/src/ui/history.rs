//! F148:「恢复上次的现场」弹窗(设计 D9/D10)。
//!
//! **零 IO**:这里只画一份已经准备好的行、回报用户选了哪条,真的读盘/摆标签
//! 在 `app.rs`。
//!
//! 时间显示用**相对时间**(设计 X3):`time` 0.3 只开了 `formatting` feature,
//! 拿不到本地时区偏移(`now_local` 要 `local-offset`,而且在多线程进程里按
//! soundness 规则通常返回 `Err`)。绝对时间在中国时区会差 8 小时 —— 相对时间
//! 既规避了这个坑,对「认出哪条记录」也更好用。

use crate::theme::{self, Theme};
use crate::ui::annotate;
use crate::ui::metrics::{SP_L, SP_M, SP_S, SP_XS};

/// 摘要里最多列几个会话名,超出的折成 `+N`。
const SUMMARY_MAX: usize = 3;

/// 列表里的一行。**已经算好的字符串**,画的时候不做任何计算 ——
/// 这些文本的判据(几天前、摘要怎么折)全是纯函数,单独测。
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryRow {
    /// 实例 id,回报给 `app.rs` 用来认领槽位(D12)。
    pub id: String,
    /// 第一行:`3 小时前 · 4 个标签 · 7 块分屏`。
    pub head: String,
    /// 第二行:`prod-web-01 · nas · db-01 · +1`。
    pub summary: String,
}

/// 弹窗自己的那点状态。
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryDraft {
    pub rows: Vec<HistoryRow>,
    /// 选中第几行。恒有值 —— 列表非空才会建这个草稿(见 `new`)。
    pub selected: usize,
}

impl HistoryDraft {
    /// **只在 `rows` 非空时调**:空列表的弹窗等于让用户点一下才能开始干活
    /// (D9:没有任何记录时不弹)。
    pub fn new(rows: Vec<HistoryRow>) -> Self {
        Self { rows, selected: 0 }
    }
}

/// 这一帧用户干了什么。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryOut {
    /// 恢复这个实例 id 的现场。
    Restore(String),
    /// 「不恢复」/ 关掉弹窗。菜单里还有常驻入口能再打开(D9)。
    Dismiss,
}

/// 相对时间(设计 X3)。`now` 与 `updated_at` 都是 Unix 秒(UTC)。
///
/// 超过 7 天退回 `MM-DD`(UTC 日期,最多差一天)—— 到那个尺度上,「11 天前」
/// 不如一个日期好定位。
pub fn when_text(now: i64, updated_at: i64) -> String {
    // 未来的时刻(时钟往回跳过)按「刚刚」算 —— 显示「-3 小时前」更莫名其妙。
    let d = now.saturating_sub(updated_at);
    if d < 60 {
        return "刚刚".into();
    }
    if d < 3600 {
        return format!("{} 分钟前", d / 60);
    }
    if d < 86_400 {
        return format!("{} 小时前", d / 3600);
    }
    if d < 7 * 86_400 {
        return format!("{} 天前", d / 86_400);
    }
    match time::OffsetDateTime::from_unix_timestamp(updated_at) {
        Ok(dt) => format!("{:02}-{:02}", dt.month() as u8, dt.day()),
        // 时刻本身是垃圾(手改过的文件)。用破折号而不是编一个日期出来。
        Err(_) => "—".into(),
    }
}

/// 会话名摘要。超过 `SUMMARY_MAX` 个折成 `+N`。
///
/// 空列表给一句话而不是空字符串:第二行空着的话,那一行的高度还在,看着像
/// 渲染出了 bug。
pub fn summary_text(titles: &[String]) -> String {
    if titles.is_empty() {
        return "(没有可恢复的标签)".into();
    }
    if titles.len() <= SUMMARY_MAX {
        return titles.join(" · ");
    }
    format!(
        "{} · +{}",
        titles[..SUMMARY_MAX].join(" · "),
        titles.len() - SUMMARY_MAX
    )
}

/// 第一行。`panes` 是所有标签的分屏数之和。
///
/// 单标签单分屏时不啰嗦「1 个标签 · 1 块分屏」—— 那两句话没有信息量,
/// 只是把真正有用的时间挤到一边。
pub fn head_text(when: &str, tabs: usize, panes: usize) -> String {
    if tabs <= 1 && panes <= 1 {
        return when.to_string();
    }
    if panes <= tabs {
        return format!("{when} · {tabs} 个标签");
    }
    format!("{when} · {tabs} 个标签 · {panes} 块分屏")
}

/// 画弹窗。返回 `Some` = 这一帧有结论(由 `app.rs` 负责把 `draft` 置 `None`)。
///
/// `draft` 为 `None` = 弹窗关着,什么都不画。
pub fn show(
    ctx: &egui::Context,
    t: &Theme,
    draft: &mut Option<HistoryDraft>,
) -> Option<HistoryOut> {
    let d = draft.as_mut()?;
    let mut out = None;
    // F156-a:`.open()` 就是标题栏右上角那个 ×。**用 egui 自带的、不自绘一个
    // `×` 字符** —— 它是 `line_segment` 画的,不碰 T9 的字形白名单
    // (`tests/glyph_whitelist.rs`);自绘的话得往 `ui::glyphs::VERIFIED` 里
    // 登记一个系统本来就提供的控件,不划算。
    //
    // × 与底部的「不恢复」并存:后者是键盘路径的出口(Tab 够得到),
    // 前者是鼠标路径的直觉位置。删掉任一个都会让某一类用户找不到出口。
    let mut open = true;
    egui::Window::new("恢复上次的现场")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            annotate::mark(ui.ctx(), "恢复现场弹窗", ui.max_rect());
            ui.label(theme::hint_text(
                t,
                "选一条摆回标签栏,会一条接一条自动重连。",
            ));
            ui.add_space(SP_M);
            egui::ScrollArea::vertical()
                .max_height(320.0)
                .show(ui, |ui| {
                    for i in 0..d.rows.len() {
                        let selected = i == d.selected;
                        let row = &d.rows[i];
                        // 整行可点:只让文字可点的话,行末的空白点不中,而用户
                        // 会去点行的任何地方(J 片「标签宿主一行都点不中」的
                        // 同一个教训)。
                        let resp = ui.allocate_response(
                            egui::vec2(ui.available_width(), 44.0),
                            egui::Sense::click(),
                        );
                        // 选中 / 悬停的底色与会话列表同源:那边是
                        // `session_manager::list::row_bg(selected, hovered, None, t)`
                        // 的两条常量臂。**照抄取值而不是调那个函数** ——
                        // 它是 `pub(crate)` 且第三个参数是节点色(本列表没有
                        // 节点概念),为两个常量拉一条跨模块依赖不划算。
                        // 色板已冻结,**不许**往 `Theme` 里加新字段。
                        if selected {
                            ui.painter().rect_filled(
                                resp.rect,
                                egui::Rounding::same(4.0),
                                theme::c32(t.sunken_bg),
                            );
                        } else if resp.hovered() {
                            ui.painter().rect_filled(
                                resp.rect,
                                egui::Rounding::same(4.0),
                                theme::c32(t.panel_head),
                            );
                        }
                        let mut p = resp.rect.min + egui::vec2(SP_S, SP_XS);
                        ui.painter().text(
                            p,
                            egui::Align2::LEFT_TOP,
                            &row.head,
                            egui::FontId::proportional(14.0),
                            theme::c32(t.fg_strong),
                        );
                        p.y += 20.0;
                        ui.painter().text(
                            p,
                            egui::Align2::LEFT_TOP,
                            &row.summary,
                            egui::FontId::proportional(12.0),
                            theme::c32(t.fg_dim),
                        );
                        // F153-b:**单击即恢复**。原来是「单击选中 + 双击恢复」,
                        // 用户报的是「点了没反应」—— 双击在高延迟远程桌面/触控板
                        // 上本来就不好按,而这个弹窗只有一件事可做。
                        //
                        // `selected` 与「恢复」按钮都留着:那是键盘路径的出口。
                        //
                        // 不再单列 `double_clicked()` 分支:双击的第一次点击已经
                        // 置过 `clicked()`,那条是死代码。
                        if resp.clicked() {
                            d.selected = i;
                            out = Some(HistoryOut::Restore(row.id.clone()));
                        }
                    }
                });
            ui.add_space(SP_L);
            ui.horizontal(|ui| {
                if ui
                    .add(egui::Button::new("恢复").min_size([96.0, 28.0].into()))
                    .clicked()
                {
                    out = Some(HistoryOut::Restore(d.rows[d.selected].id.clone()));
                }
                ui.add_space(SP_S);
                if ui.button("不恢复").clicked() {
                    out = Some(HistoryOut::Dismiss);
                }
            });
        });
    // F156-a:× 和 Esc 同一个出口,都回报既有的 `Dismiss`。
    //
    // `get_or_insert` 而不是直接赋值:同一帧里既点了某一行、又把窗关掉,在
    // 物理上不可能,但让「先发生的结论优先」是显式的,比依赖那个不可能性稳。
    //
    // Esc 直接读 `ctx`:这个弹窗里没有文本框,不需要
    // `session_manager::keys::scan` 那套 `typing` 让位逻辑。
    if !open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        out.get_or_insert(HistoryOut::Dismiss);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_record_says_just_now() {
        assert_eq!(when_text(1000, 1000), "刚刚");
        assert_eq!(when_text(1000, 941), "刚刚");
    }

    #[test]
    fn relative_time_walks_up_the_units() {
        assert_eq!(when_text(1_000_000, 1_000_000 - 120), "2 分钟前");
        assert_eq!(when_text(1_000_000, 1_000_000 - 3 * 3600), "3 小时前");
        assert_eq!(when_text(1_000_000, 1_000_000 - 2 * 86_400), "2 天前");
    }

    /// 超过一周退回日期 —— 「11 天前」在那个尺度上不如一个日期好定位。
    #[test]
    fn anything_older_than_a_week_falls_back_to_a_date() {
        // 1_755_000_000 = 2025-08-12T12:00:00Z,减 30 天 = 2025-07-13T12:00:00Z。
        assert_eq!(
            when_text(1_755_000_000, 1_755_000_000 - 30 * 86_400),
            "07-13"
        );
    }

    /// 时钟往回跳过(记录的时刻在未来)时不显示负数 —— 「-3 小时前」比
    /// 「刚刚」更让人以为程序坏了。
    ///
    /// 自证会变红:把 `when_text` 里的 `if d < 60` 改成 `if (0..60).contains(&d)`
    /// —— 兜住负数的是这条分支,**不是** `saturating_sub`(那个防的是另一件事,
    /// 见下一条测试)。
    #[test]
    fn a_record_stamped_in_the_future_reads_as_just_now() {
        assert_eq!(when_text(1000, 9999), "刚刚");
    }

    /// 记录文件是手能改的,`updated_at` 能塞进**任何** `i64`,包括 `i64::MIN`。
    /// `now - i64::MIN` 在 debug 构建下溢出 panic —— 一个被手改过的记录就能让
    /// 客户端起不来,所以 `when_text` 里的 `saturating_sub` 不是装饰。
    ///
    /// 饱和之后 `d` 变成 `i64::MAX`,落进「超过一周」那一支,而
    /// `from_unix_timestamp(i64::MIN)` 给 `Err` —— 于是显示破折号,而不是编一个
    /// 日期出来。
    ///
    /// 自证会变红:把 `saturating_sub` 换成裸减法(那会 panic,panic 也是红)。
    #[test]
    fn a_hand_edited_timestamp_at_the_low_extreme_does_not_crash_the_client() {
        assert_eq!(when_text(1000, i64::MIN), "—");
    }

    #[test]
    fn a_short_summary_lists_every_session() {
        let t = vec!["a".to_string(), "b".to_string()];
        assert_eq!(summary_text(&t), "a · b");
    }

    /// 长摘要折成 `+N` —— 不折的话第二行会把弹窗撑得比屏幕还宽。
    #[test]
    fn a_long_summary_is_folded() {
        let t: Vec<String> = (1..=6).map(|i| format!("s{i}")).collect();
        assert_eq!(summary_text(&t), "s1 · s2 · s3 · +3");
    }

    /// 空摘要给一句话,不是空字符串:那一行的高度还在,空着看着像渲染坏了。
    #[test]
    fn an_empty_summary_says_so_instead_of_going_blank() {
        assert_eq!(summary_text(&[]), "(没有可恢复的标签)");
    }

    /// 单标签单分屏不啰嗦 —— 「1 个标签 · 1 块分屏」没有信息量。
    #[test]
    fn a_single_pane_record_does_not_brag_about_its_counts() {
        assert_eq!(head_text("刚刚", 1, 1), "刚刚");
    }

    #[test]
    fn a_record_with_splits_reports_both_counts() {
        assert_eq!(
            head_text("3 小时前", 4, 7),
            "3 小时前 · 4 个标签 · 7 块分屏"
        );
    }

    /// 标签数等于分屏数(每个标签都只有一块)时不重复报同一个数。
    #[test]
    fn a_record_without_splits_only_reports_the_tab_count() {
        assert_eq!(head_text("2 天前", 3, 3), "2 天前 · 3 个标签");
    }

    fn rows() -> Vec<HistoryRow> {
        vec![
            HistoryRow {
                id: "a".into(),
                head: "刚刚 · 2 个标签".into(),
                summary: "prod · nas".into(),
            },
            HistoryRow {
                id: "b".into(),
                head: "3 小时前".into(),
                summary: "db-01".into(),
            },
        ]
    }

    /// 跑两帧,收本帧画出来的所有文字。**两帧**:`egui::Window` 首帧
    /// `fade_in` 只记 `Shape::Noop`(同 `ui/restored.rs` 的说明)。
    fn texts(draft: &mut Option<HistoryDraft>) -> Vec<String> {
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
                    show(ctx, &t, draft);
                })
                .shapes;
        }
        let mut out = Vec::new();
        for cs in &shapes {
            walk(&cs.shape, &mut out);
        }
        out
    }

    /// 点一下写着 `label` 的那颗按钮,返回这一帧 `show` 的结论。
    fn click(draft: &mut Option<HistoryDraft>, label: &str) -> Option<HistoryOut> {
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
                    show(ctx, &t, draft);
                })
                .shapes;
        }
        let pos = shapes
            .iter()
            .find_map(|cs| find(&cs.shape, label))
            .unwrap_or_else(|| panic!("弹窗里没有写着「{label}」的控件"));
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
            out = show(ctx, &t, draft);
        });
        out
    }

    /// 点第 `i` 行的中央。拿那一行的 `head` 文字当锚点 —— 它就画在那一行的
    /// rect 里,不必自己算行高(行高改了这里也不会跟着坏)。
    fn click_row(draft: &mut Option<HistoryDraft>, i: usize) -> Option<HistoryOut> {
        let head = draft.as_ref().expect("草稿").rows[i].head.clone();
        click(draft, &head)
    }

    /// 点标题栏右上角的 ×。
    ///
    /// **不能用上面的 `click(label)`** —— 那个靠找 `Shape::Text` 定位,而 egui
    /// 的 close button 是两条 `line_segment` 画出来的,树里根本没有文字。
    /// 改从本帧的 accesskit 树里按 egui 给它登记的 label 取 rect
    /// (`egui/src/containers/window.rs` 的 `close_button`:
    /// `WidgetInfo::labeled(WidgetType::Button, .., "Close window")`)。
    ///
    /// 取不到就 panic 并把树里所有 label 打出来:egui 换版本改了这个 label
    /// 的话,这条测试要**当场报出来**,而不是静默点到别处、变成一条恒绿。
    fn click_close_x(draft: &mut Option<HistoryDraft>) -> Option<HistoryOut> {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        // 开了才构树(egui 的 `accesskit` feature 已由 `mullion-app` 打开)。
        ctx.enable_accesskit();
        // 两帧:`egui::Window` 首帧 `fade_in`,几何还没落定(同 `texts` 的说明)。
        // 开着 accesskit 时**每帧都会**产出一棵完整的树,所以取最后一帧那棵。
        let mut update = None;
        for _ in 0..2 {
            let mut full = ctx.run(egui::RawInput::default(), |ctx| {
                show(ctx, &t, draft);
            });
            update = full.platform_output.accesskit_update.take();
        }
        let nodes = update.expect("开了 accesskit 却没有产出树").nodes;
        let labels: Vec<String> = nodes
            .iter()
            .filter_map(|(_, n)| n.label().map(str::to_string))
            .collect();
        let b = nodes
            .iter()
            .find(|(_, n)| n.label() == Some("Close window"))
            .and_then(|(_, n)| n.bounds())
            .unwrap_or_else(|| panic!("accesskit 树里没有关闭按钮;树里现有的 label:{labels:?}"));
        let pos = egui::pos2(((b.x0 + b.x1) / 2.0) as f32, ((b.y0 + b.y1) / 2.0) as f32);
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
        let mut out = None;
        let _ = ctx.run(input, |ctx| {
            out = show(ctx, &t, draft);
        });
        out
    }

    /// F156-a:用户报的是「这个弹窗看起来关不掉」—— 底部虽然有「不恢复」,
    /// 但弹窗右上角没有 ×,而那是所有人找出口的第一个地方。
    ///
    /// 回报的是**既有的** `Dismiss`,不新增出口变体:`app.rs` 那侧
    /// 「无论恢复还是不恢复都把弹窗收掉」的处置一行都不用动。
    ///
    /// 自证会变红:把 `show` 里的 `.open(&mut open)` 去掉
    /// (树里就没有那个按钮了,脚手架的 panic 会打出实际的 label 列表)。
    #[test]
    fn closing_the_window_with_the_title_bar_x_reports_dismiss() {
        let mut draft = Some(HistoryDraft::new(rows()));
        assert_eq!(click_close_x(&mut draft), Some(HistoryOut::Dismiss));
    }

    /// F156-a:Esc 也是出口。× 只照顾鼠标,而这个弹窗是**启动时**弹的 ——
    /// 用户此刻手还在键盘上。
    ///
    /// 自证会变红:把 `show` 末尾那个 `key_pressed(Escape)` 分支删掉。
    #[test]
    fn pressing_escape_closes_the_dialog() {
        let mut draft = Some(HistoryDraft::new(rows()));
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        // 两帧预热,理由同 `texts`。
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                show(ctx, &t, &mut draft);
            });
        }
        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        });
        let mut out = None;
        let _ = ctx.run(input, |ctx| {
            out = show(ctx, &t, &mut draft);
        });
        assert_eq!(out, Some(HistoryOut::Dismiss));
    }

    /// `None` 的草稿什么都不画 —— 弹窗关着就是关着。
    #[test]
    fn a_closed_dialog_draws_nothing() {
        let mut draft = None;
        assert!(texts(&mut draft).is_empty());
    }

    /// 每一条记录的两行都要画出来:只画第一行的话,多开场景下两条记录的
    /// 时间可能很接近,用户分不出哪条是哪个窗口(D10)。
    #[test]
    fn every_row_shows_both_its_lines() {
        let mut draft = Some(HistoryDraft::new(rows()));
        let joined = texts(&mut draft).join(" ");
        assert!(joined.contains("刚刚 · 2 个标签"), "第一行没画:{joined}");
        assert!(joined.contains("prod · nas"), "第二行没画:{joined}");
        assert!(joined.contains("3 小时前"), "第二条记录没画:{joined}");
    }

    /// 「恢复」回报的是**当前选中那一条的 id**,不是恒第一条。
    ///
    /// 自证会变红:把 `d.rows[d.selected].id` 改成 `d.rows[0].id`。
    #[test]
    fn restoring_reports_the_selected_record_not_the_first_one() {
        let mut draft = Some(HistoryDraft::new(rows()));
        draft.as_mut().unwrap().selected = 1;
        assert_eq!(
            click(&mut draft, "恢复"),
            Some(HistoryOut::Restore("b".into()))
        );
    }

    #[test]
    fn dismissing_reports_dismiss() {
        let mut draft = Some(HistoryDraft::new(rows()));
        assert_eq!(click(&mut draft, "不恢复"), Some(HistoryOut::Dismiss));
    }

    /// 光把弹窗画出来不等于选了什么 —— 否则它一出现就自己恢复了,
    /// 「可以选择恢复」这条需求当场作废。
    #[test]
    fn merely_showing_the_dialog_restores_nothing() {
        let mut draft = Some(HistoryDraft::new(rows()));
        assert!(!texts(&mut draft).is_empty());
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut out = None;
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                out = show(ctx, &t, &mut draft);
            });
        }
        assert_eq!(out, None);
    }

    /// F153-b:单击一行就恢复那一条,不用再去够「恢复」按钮。用户报的原话是
    /// 「点了没反应」—— 原来要双击。
    ///
    /// 自证会变红:把 `clicked()` 分支改回只写 `d.selected = i`。
    #[test]
    fn clicking_a_row_restores_that_record_right_away() {
        let mut draft = Some(HistoryDraft::new(rows()));
        assert_eq!(
            click_row(&mut draft, 1),
            Some(HistoryOut::Restore("b".into()))
        );
    }

    /// F153:提示语必须跟行为一致。自动重连之后,「点「重连」才拨号」是假话
    /// —— 用户照着它等,以为程序没反应。
    ///
    /// 自证会变红:把那句文案改回去。
    #[test]
    fn the_hint_no_longer_promises_a_manual_reconnect() {
        let mut draft = Some(HistoryDraft::new(rows()));
        let joined = texts(&mut draft).join(" ");
        assert!(
            !joined.contains("点「重连」才拨号"),
            "提示语还在说要手动重连:{joined}"
        );
        assert!(joined.contains("自动重连"), "提示语没说会自动重连:{joined}");
    }
}
