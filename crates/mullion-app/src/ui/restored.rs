//! F37:恢复出来的**占位标签**的中央区。
//!
//! 上次关窗时开着的标签,这次启动只摆回骨架、一条连接都不建(设计 §1)。
//! 这里画的就是那个骨架:会话名 + 上次的分屏数 + 一个「重连」按钮。
//!
//! 零 IO、零连接 —— 按钮只回报一个 `TabId`,真的拨号由 `app.rs` 走
//! `spawn_connect`(与会话管理器双击连接同一条路径,不分叉)。

use super::annotate;
use crate::shell::tabs::TabId;
use crate::theme::{self, Theme};

/// 一个占位标签这一帧要显示的东西。
///
/// `Copy`:它是 [`super::UiFrame`] 的字段,而 `UiFrame` 必须整体 `Copy`
/// (见其文档注释)。
#[derive(Clone, Copy)]
pub struct RestoredView<'a> {
    /// 哪个标签。按钮点下去回报的就是它 —— **不用「活动标签」反推**:
    /// 调用方本来就是按活动标签填的这个字段,显式带上 id 才能让 `app.rs`
    /// 那侧的处置与「当时画的是谁」严格对上(同 S1 用世代号路由的理由)。
    pub tab_id: TabId,
    /// 标签标题(会话名)。
    pub title: &'a str,
    /// 上次这个标签分了几屏。1 就不显示分屏那半句。
    pub panes: usize,
    /// 已经点过重连、正在拨号。按钮禁用并改文案 —— 不禁用的话连点会拉起
    /// N 条连接,而本片选手动重连的理由之一正是「别让高延迟链路上同时挤
    /// 一堆握手」(设计 §1 理由 2)。
    pub dialing: bool,
}

/// 画占位中央区。返回 `Some(tab_id)` = 用户按了「重连」。
///
/// **必须是本帧最后一个 panel 类部件**(同 `files_panel::content`):
/// `CentralPanel` 铺满剩余空间,排在别的 panel 之前会把它们挤没。
pub fn show(ctx: &egui::Context, t: &Theme, v: RestoredView<'_>) -> Option<TabId> {
    let mut clicked = None;
    let panel = egui::CentralPanel::default()
        .frame(egui::Frame::none().fill(theme::c32(t.window_bg)))
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                // 上半留白多于下半:整块内容视觉重心略高于正中,正中会显得往下坠。
                ui.add_space(ui.available_height() * 0.32);
                ui.label(
                    egui::RichText::new(v.title)
                        .size(18.0)
                        .color(theme::c32(t.fg_strong)),
                );
                ui.add_space(6.0);
                ui.label(theme::hint_text(t, subtitle(v.panes)));
                ui.add_space(14.0);
                let label = if v.dialing { "连接中…" } else { "重连" };
                if ui
                    .add_enabled(
                        !v.dialing,
                        egui::Button::new(label).min_size([96.0, 28.0].into()),
                    )
                    .clicked()
                {
                    clicked = Some(v.tab_id);
                }
            });
        });
    annotate::mark(ctx, "占位标签(未连接)", panel.response.rect);
    clicked
}

/// 副标题文案。抽成纯函数是为了能脱离 egui 断言 —— 「上次是几分屏」这句话
/// 是本视图唯一一处有分支的逻辑,靠跑帧去测它等于拿最贵的手段测最便宜的东西。
fn subtitle(panes: usize) -> String {
    if panes <= 1 {
        "未连接".to_string()
    } else {
        format!("未连接 · 上次是 {panes} 分屏")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_pane_tab_does_not_brag_about_its_split_layout() {
        assert_eq!(subtitle(1), "未连接");
        assert_eq!(subtitle(0), "未连接");
    }

    #[test]
    fn a_split_tab_says_how_many_panes_it_had() {
        assert_eq!(subtitle(3), "未连接 · 上次是 3 分屏");
    }

    fn view(dialing: bool) -> RestoredView<'static> {
        RestoredView {
            tab_id: TabId(7),
            title: "生产机",
            panes: 3,
            dialing,
        }
    }

    /// 跑两帧,收本帧画出来的所有文字。**两帧**:`CentralPanel` 首帧
    /// `fade_in` 只记 `Shape::Noop`(与 `files_panel` / `edit_panel` 那边
    /// 同款,见其注释)。
    fn texts(dialing: bool) -> Vec<String> {
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
                    show(ctx, &t, view(dialing));
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
    fn click(dialing: bool, label: &str) -> Option<TabId> {
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
                    show(ctx, &t, view(dialing));
                })
                .shapes;
        }
        let pos = shapes
            .iter()
            .find_map(|cs| find(&cs.shape, label))
            .unwrap_or_else(|| panic!("占位中央区里没有写着「{label}」的按钮"));
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
            out = show(ctx, &t, view(dialing));
        });
        out
    }

    /// 占位标签必须自报家门:光一句「未连接」的话,恢复出四个标签时用户
    /// 分不清哪个是哪个(标签栏上标题被截断是常态)。
    #[test]
    fn the_placeholder_names_the_session_and_the_old_split_count() {
        let joined = texts(false).join(" ");
        assert!(joined.contains("生产机"), "没画会话名:{joined}");
        assert!(joined.contains("上次是 3 分屏"), "没画分屏数:{joined}");
    }

    /// 按钮回报的是**画它时那个 `tab_id`**,不是「当前活动标签」——
    /// 两者在同一帧里恒相等,但让 `app.rs` 拿到显式的 id 才能在处置那侧
    /// 对上账(同 S1 用世代号路由的理由)。
    #[test]
    fn clicking_reconnect_reports_the_tab_it_was_drawn_for() {
        assert_eq!(click(false, "重连"), Some(TabId(7)));
    }

    /// 没按任何按钮的那一帧必须返回 `None` —— 否则占位标签一画出来就自己
    /// 开始拨号,本片「手动重连」这条设计当场作废。
    #[test]
    fn merely_showing_the_placeholder_dials_nothing() {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut out = None;
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                out = show(ctx, &t, view(false));
            });
        }
        assert_eq!(out, None);
    }

    /// 拨号中必须禁用按钮:不禁用的话用户连点会拉起 N 条连接,而选手动重连
    /// 的理由之一正是「别让高延迟链路上同时挤一堆握手」(设计 §1 理由 2)。
    ///
    /// 自证会变红:把 `add_enabled(!v.dialing, ..)` 的第一个参数改成 `true`。
    #[test]
    fn the_reconnect_button_is_dead_while_already_dialing() {
        assert_eq!(
            click(true, "连接中…"),
            None,
            "拨号中按钮仍可点 —— 连点会拉起 N 条连接"
        );
    }
}
