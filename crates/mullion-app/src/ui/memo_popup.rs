//! F367:pane 里的项目备忘浮层(Ctrl+Shift+M)。
//!
//! 原先改备忘要走「会话 → 项目 → 选项目 → 备忘分节」,而备忘恰恰是边干活边
//! 随手记的东西。浮层**锚定焦点 pane**、在 pane 内居中(F373):上半部
//! 「新增备忘」输入框打开即聚焦、回车即加一条并立即落盘,下面倒序列出已有
//! 备忘、每条可删;下半部长期备注(`note`)默认折叠,点开可编辑、点「保存」落盘。
//!
//! 这里只画、只出结论([`MemoOut`]);落盘(字段级 mutator)与落盘后刷新快照
//! 在 `app.rs`。**要登记进 `app.rs::modal_open`**:里面有输入框,不算模态的话
//! 打的字同时漏给远端 shell(T8)。

use mullion_core::layout::PaneId;
use mullion_store::{Memo, ProjectId, ProjectRecord};

use crate::theme::{self, Theme};

/// 浮层那块 `Area` 的 id。按 pane 分,同 `project_pick::area_id`。
pub(crate) fn area_id(pane: PaneId) -> egui::Id {
    egui::Id::new(("memo_popup_area", pane.0))
}

/// 「新增备忘」输入框的 id。手写才能在测试里取回、也才能 `request_focus`。
pub(crate) fn input_id() -> egui::Id {
    egui::Id::new("memo_popup_input")
}

/// 浮层离 pane 边缘的留白。
const INSET: f32 = 8.0;

/// 固定部分(标题 + 输入框 + 备注折叠头 + 内边距)的高度预算,同
/// `project_pick::CHROME_H` 的用法:宁可估大,别把底部出口顶出 pane。
const CHROME_H: f32 = 140.0;

/// 浮层的状态。`Some` = 开着。
pub struct MemoDraft {
    pub pane: PaneId,
    pub project: ProjectId,
    /// 标题「项目名 · 备忘」。
    pub title: String,
    /// 打开那一刻(及每次落盘后)的备忘快照。
    pub memos: Vec<Memo>,
    /// 「新增备忘」输入框里的字。
    pub input: String,
    /// 备注正在编辑的值。
    pub note: String,
    /// 备注在库里的值 —— 脏判据的基线。
    pub note_baseline: String,
    /// 备注分节展开了没有。默认折叠。
    pub note_open: bool,
    /// 还欠一次「把焦点给输入框」,同 `ProjectPickDraft::focus_pending`。
    pub focus_pending: bool,
    /// 落盘失败的原因,浮层里内联红字。
    pub error: Option<String>,
    /// 备注有未保存改动时按了 Esc —— 不关,改为提示。
    pub dirty_hint: bool,
}

impl MemoDraft {
    pub fn new(pane: PaneId, p: &ProjectRecord) -> Self {
        Self {
            pane,
            project: p.id,
            title: format!("{} · 备忘", p.name),
            memos: p.memos.clone(),
            input: String::new(),
            note: p.note.clone(),
            note_baseline: p.note.clone(),
            note_open: false,
            focus_pending: true,
            error: None,
            dirty_hint: false,
        }
    }

    /// 备注相对库里的值改过没有。点外 / Esc 关浮层前看它(`ui/dismiss.rs` 约定)。
    pub fn note_dirty(&self) -> bool {
        self.note != self.note_baseline
    }

    /// 落盘成功后按库里的新值刷新快照。备注只在**刚保存的就是它**时才整段
    /// 覆盖编辑值;否则(加/删备忘时)保留用户正在改的半段备注。
    pub fn refresh(&mut self, p: &ProjectRecord) {
        let was_clean = !self.note_dirty();
        self.memos = p.memos.clone();
        self.note_baseline = p.note.clone();
        if was_clean {
            self.note = p.note.clone();
        }
        self.error = None;
    }
}

/// 用户在浮层里的结论。除 `Close` 外浮层都保持开着(app 落盘后刷新快照)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoOut {
    Add(String),
    Remove(Memo),
    SaveNote(String),
    Close,
}

/// 列表顺序:新的在上。按 `created_at`(RFC3339,同时区可按字典序比)倒序,
/// 同一时刻的按原顺序倒着来。
pub fn ordered(memos: &[Memo]) -> Vec<&Memo> {
    let mut v: Vec<(usize, &Memo)> = memos.iter().enumerate().collect();
    v.sort_by(|(ia, a), (ib, b)| b.created_at.cmp(&a.created_at).then(ib.cmp(ia)));
    v.into_iter().map(|(_, m)| m).collect()
}

/// 画浮层。返回本帧的结论;`Close` 由调用方把 draft 置 `None`。
pub fn show(
    ctx: &egui::Context,
    t: &Theme,
    draft: &mut Option<MemoDraft>,
    pane_rect: Option<egui::Rect>,
) -> Option<MemoOut> {
    let d = draft.as_mut()?;
    let mut out = None;
    // Esc:备注脏时不关,改为提示(同点外的脏改动约定)。
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        if d.note_dirty() {
            d.dirty_hint = true;
        } else {
            out = Some(MemoOut::Close);
        }
    }
    if !d.note_dirty() {
        d.dirty_hint = false;
    }
    let host = pane_rect.unwrap_or_else(|| ctx.screen_rect());
    let avail = host.shrink(INSET);
    let field_w = crate::ui::metrics::field_w(
        avail.width(),
        crate::ui::metrics::FIELD_W_M,
        2.0 * crate::ui::metrics::SP_M,
    );
    let list_h = (avail.height() - CHROME_H).max(48.0);
    egui::Area::new(area_id(d.pane))
        .order(egui::Order::Foreground)
        // F373:pane 中心对齐,内容变高向上下两头长。`pivot` 用上一帧的尺寸,
        // 首帧没有尺寸时 egui 自己走一遍不可见的 sizing pass,不会先在左上闪一下。
        .pivot(egui::Align2::CENTER_CENTER)
        .fixed_pos(host.center())
        .constrain_to(host)
        .show(ctx, |ui| {
            ui.set_max_width(field_w + 2.0 * crate::ui::metrics::SP_M);
            ui.set_max_height(avail.height());
            egui::Frame::popup(&ctx.style())
                .fill(theme::c32(t.panel_bg))
                .stroke(theme::stroke(t))
                .show(ui, |ui| {
                    crate::ui::annotate::mark(ui.ctx(), "项目备忘".to_string(), ui.max_rect());
                    ui.label(egui::RichText::new(&d.title).color(theme::c32(t.fg_strong)));
                    ui.add_space(crate::ui::metrics::SP_S);
                    // 回车闸在画输入框**之前**取:单行 TextEdit 自己吃 Enter
                    // (失焦)。组字确认的那一下回车不算(F314/F316)。
                    let focused = ctx.memory(|m| m.has_focus(input_id()));
                    let enter = focused && ctx.input_mut(crate::ui::project_nav::take_enter);
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut d.input)
                            .id(input_id())
                            .hint_text("新增备忘,回车保存")
                            .desired_width(field_w),
                    );
                    if std::mem::take(&mut d.focus_pending) {
                        r.request_focus();
                    }
                    if enter {
                        let text = d.input.trim().to_string();
                        if !text.is_empty() {
                            d.input.clear();
                            out = Some(MemoOut::Add(text));
                        }
                    }
                    if let Some(e) = &d.error {
                        ui.label(egui::RichText::new(e).color(theme::c32(t.danger_text)));
                    }
                    ui.add_space(crate::ui::metrics::SP_S);
                    if d.memos.is_empty() {
                        ui.label(
                            egui::RichText::new("没有备忘。临时要记的事写在这里,做完就删。")
                                .color(theme::c32(t.fg_muted)),
                        );
                    }
                    egui::ScrollArea::vertical()
                        .max_height(list_h)
                        .show(ui, |ui| {
                            ui.set_min_width(field_w);
                            for m in ordered(&d.memos) {
                                ui.horizontal(|ui| {
                                    if crate::ui::icon::icon_button(
                                        ui,
                                        crate::ui::icon::Glyph::Cross,
                                        true,
                                        "删除这条备忘",
                                    ) {
                                        out = Some(MemoOut::Remove(m.clone()));
                                    }
                                    ui.add(egui::Label::new(&m.text).wrap());
                                });
                            }
                        });
                    ui.separator();
                    let glyph = if d.note_open {
                        crate::ui::icon::Glyph::TriangleDown
                    } else {
                        crate::ui::icon::Glyph::TriangleRight
                    };
                    let head = ui.horizontal(|ui| {
                        crate::ui::icon::icon_inline(ui, glyph, theme::c32(t.fg_muted));
                        ui.add(egui::Label::new("长期备注").sense(egui::Sense::click()))
                    });
                    if head.inner.clicked() {
                        d.note_open = !d.note_open;
                    }
                    if d.note_open {
                        ui.add(
                            egui::TextEdit::multiline(&mut d.note)
                                .desired_rows(4)
                                .desired_width(field_w),
                        );
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(d.note_dirty(), egui::Button::new("保存备注"))
                                .clicked()
                            {
                                out = Some(MemoOut::SaveNote(d.note.clone()));
                            }
                        });
                    }
                    if d.dirty_hint {
                        ui.label(
                            egui::RichText::new("备注有未保存的改动,先保存或改回原样再关")
                                .color(theme::c32(t.warn)),
                        );
                    }
                });
        });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> ProjectRecord {
        ProjectRecord {
            id: ProjectId(7),
            name: "web".into(),
            note: String::new(),
            nodes: Vec::new(),
            preferred: None,
            dir: "/srv/web".into(),
            tmux_name: None,
            created_at: "2026-11-01T00:00:00Z".into(),
            last_accessed_at: None,
            archived_at: None,
            icon: Default::default(),
            memos: vec![
                Memo {
                    text: "旧的".into(),
                    created_at: "2026-11-01T00:00:00Z".into(),
                },
                Memo {
                    text: "新的".into(),
                    created_at: "2026-11-17T00:00:00Z".into(),
                },
            ],
        }
    }

    fn key(k: egui::Key) -> egui::Event {
        egui::Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    /// 跑一帧,返回结论。
    fn frame(
        ctx: &egui::Context,
        d: &mut Option<MemoDraft>,
        events: Vec<egui::Event>,
    ) -> Option<MemoOut> {
        let t = crate::theme::MULLION_DARK;
        let input = egui::RawInput {
            events,
            ..Default::default()
        };
        let mut out = None;
        let _ = ctx.run(input, |ctx| {
            out = show(ctx, &t, d, None);
        });
        out
    }

    /// F373:浮层在 pane 内水平垂直居中(pane 不在屏幕中央,两种中心才分得开)。
    ///
    /// 自证会变红:把 `.pivot(..).fixed_pos(host.center())` 换回 `fixed_pos(avail.min)`。
    #[test]
    fn the_popup_is_centered_in_its_pane() {
        let ctx = egui::Context::default();
        let t = crate::theme::MULLION_DARK;
        let pane = egui::Rect::from_min_size(egui::pos2(600.0, 300.0), egui::vec2(600.0, 500.0));
        let mut d = Some(MemoDraft::new(PaneId(1), &project()));
        for _ in 0..3 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1400.0, 900.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                show(ctx, &t, &mut d, Some(pane));
            });
        }
        let r = ctx
            .memory(|m| m.area_rect(area_id(PaneId(1))))
            .expect("浮层没画出来");
        let c = pane.center();
        assert!(
            (r.center().x - c.x).abs() <= 1.0 && (r.center().y - c.y).abs() <= 1.0,
            "浮层中心 {:?} 不在 pane 中心 {c:?}(浮层 {r:?})",
            r.center()
        );
        assert!(pane.contains_rect(r), "浮层出了 pane:{r:?}");
    }

    /// F367:打开即聚焦输入框;打字 + 回车 = 加一条,输入框清空、浮层不关。
    #[test]
    fn the_input_has_focus_on_open_and_enter_adds_a_memo() {
        let ctx = egui::Context::default();
        let mut d = Some(MemoDraft::new(PaneId(1), &project()));
        assert_eq!(frame(&ctx, &mut d, vec![]), None);
        assert!(
            ctx.memory(|m| m.has_focus(input_id())),
            "打开后焦点不在输入框"
        );
        let out = frame(&ctx, &mut d, vec![egui::Event::Text("abc".into())]);
        assert_eq!(out, None);
        let out = frame(&ctx, &mut d, vec![key(egui::Key::Enter)]);
        assert_eq!(out, Some(MemoOut::Add("abc".into())));
        assert_eq!(d.as_ref().unwrap().input, "", "输入框没清空");
        assert!(
            ctx.memory(|m| m.has_focus(input_id())),
            "回车后输入框失焦了 —— 连记两条得再点一次"
        );
    }

    /// F367:组字确认的那一下回车不加备忘(F314/F316 两次翻车)。
    #[test]
    fn enter_while_composing_does_not_add() {
        let ctx = egui::Context::default();
        let mut d = Some(MemoDraft::new(PaneId(1), &project()));
        frame(&ctx, &mut d, vec![]);
        frame(&ctx, &mut d, vec![egui::Event::Text("abc".into())]);
        let out = frame(
            &ctx,
            &mut d,
            vec![
                egui::Event::Ime(egui::ImeEvent::Commit("我".into())),
                key(egui::Key::Enter),
            ],
        );
        assert_eq!(out, None, "组字回车被当成了提交");
    }

    /// F367:空白输入回车什么都不加。
    #[test]
    fn blank_input_adds_nothing() {
        let ctx = egui::Context::default();
        let mut d = Some(MemoDraft::new(PaneId(1), &project()));
        frame(&ctx, &mut d, vec![]);
        frame(&ctx, &mut d, vec![egui::Event::Text("   ".into())]);
        assert_eq!(frame(&ctx, &mut d, vec![key(egui::Key::Enter)]), None);
    }

    /// F367:新的在上。
    #[test]
    fn memos_are_listed_newest_first() {
        let p = project();
        let texts: Vec<&str> = ordered(&p.memos).iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, ["新的", "旧的"]);
    }

    /// F367:备注默认折叠;脏判据跟着编辑值走;加备忘后刷新快照不冲掉
    /// 用户正在改的半段备注。
    #[test]
    fn the_note_is_folded_by_default_and_its_dirtiness_is_tracked() {
        let mut p = project();
        let mut d = MemoDraft::new(PaneId(1), &p);
        assert!(!d.note_open);
        assert!(!d.note_dirty());
        d.note = "半段".into();
        assert!(d.note_dirty());
        p.memos.push(Memo {
            text: "又一条".into(),
            created_at: "2026-11-18T00:00:00Z".into(),
        });
        d.refresh(&p);
        assert_eq!(d.memos.len(), 3);
        assert_eq!(d.note, "半段", "刷新冲掉了正在改的备注");
        p.note = "半段".into();
        d.refresh(&p);
        assert!(!d.note_dirty());
    }

    /// F367:备注脏时 Esc 不关,改为提示;干净时 Esc = 关。
    #[test]
    fn escape_closes_only_when_the_note_is_clean() {
        let ctx = egui::Context::default();
        let mut d = Some(MemoDraft::new(PaneId(1), &project()));
        frame(&ctx, &mut d, vec![]);
        d.as_mut().unwrap().note = "改了".into();
        assert_eq!(frame(&ctx, &mut d, vec![key(egui::Key::Escape)]), None);
        assert!(d.as_ref().unwrap().dirty_hint);
        d.as_mut().unwrap().note.clear();
        assert_eq!(
            frame(&ctx, &mut d, vec![key(egui::Key::Escape)]),
            Some(MemoOut::Close)
        );
    }
}
