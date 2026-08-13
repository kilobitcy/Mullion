//! F53/设计 D20:内置编辑器窗口。
//!
//! 五条边界(大小 / 二进制 / 编码 / 换行 / 写回)里,前三条在打开之前就判完了
//! (`app.rs` + `edit::text`),这里负责后两条与「别把改动弄丢」:
//! 脏了就在标题上标出来、关窗前拦一次、换行混用时逼用户明说选哪种。

use crate::edit::sessions::EditKey;
use crate::edit::text::Eol;
use crate::theme::{self, Theme};

/// 窗口里按下的东西。写回是 IO,一律交回 `app.rs`(同 `files_dialog` 的做法)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorAction {
    /// 保存 → 走与外部编辑**同一条**回传路径(含远端变更检查)。
    Save,
    /// 关窗(已经确认过丢弃了)。**必须自带 key**:`show` 返回前就把
    /// `*state` 清成 `None` 了,调用方再回头问「刚才关的是哪条」问不出来,
    /// 于是那条编辑会话和它的临时文件就永远留在那儿。
    Close(EditKey),
}

pub struct EditorState {
    pub key: EditKey,
    /// 完整远端路径,标题栏里显示。
    pub path: String,
    pub text: String,
    /// 打开(或上次保存成功)时的文本。脏检查的基准。
    pub baseline: String,
    /// 非 `None` = 只读,字符串是原因(二进制 / 非 UTF-8)。
    pub read_only: Option<&'static str>,
    /// 文件原本的换行符。`Mixed` 时保存前要用户选(D3-4)。
    pub eol: Eol,
    pub bom: bool,
    /// 用户为混用文件选定的换行符。`Mixed` 之外的文件恒 `None`(不需要选)。
    pub eol_choice: Option<Eol>,
    /// 写前在同目录留一份 `.mullion.bak`(D3-7)。默认开。
    pub backup: bool,
    /// 正在回传 —— 保存按钮置灰,免得连点两次写两遍。
    pub busy: bool,
    /// 上一次保存的结果(成功一句、失败一句原因)。
    pub notice: Option<String>,
    /// 点了关闭但内容是脏的 —— 压一条确认条,不直接关。
    pub confirm_close: bool,
}

impl EditorState {
    pub fn new(
        key: EditKey,
        path: String,
        text: String,
        read_only: Option<&'static str>,
        eol: Eol,
        bom: bool,
    ) -> Self {
        Self {
            key,
            path,
            baseline: text.clone(),
            text,
            read_only,
            eol,
            bom,
            eol_choice: None,
            backup: true,
            busy: false,
            notice: None,
            confirm_close: false,
        }
    }

    pub fn dirty(&self) -> bool {
        self.text != self.baseline
    }

    /// 真正写回时用的换行符。混用文件用用户选的那个,其余用原本的。
    pub fn effective_eol(&self) -> Eol {
        match self.eol {
            Eol::Mixed => self.eol_choice.unwrap_or(Eol::Lf),
            other => other,
        }
    }

    /// 编码成要写回远端的字节。换行按 `effective_eol` 统一,BOM 读到就带回去
    /// (D3-6)—— 编辑一个文件不该顺手改掉它的这两样。
    pub fn bytes(&self) -> Vec<u8> {
        crate::edit::text::encode(&self.text, self.effective_eol(), self.bom)
    }

    /// 一次回传收工。成功就把基线跟上(于是脏标记落下),失败把原因写在
    /// 窗口里 —— **不清空 `text`**:用户的改动是他唯一的一份。
    pub fn finish_save(&mut self, outcome: Result<(), String>) {
        self.busy = false;
        match outcome {
            Ok(()) => {
                self.baseline = self.text.clone();
                self.notice = Some("已回传".into());
            }
            Err(why) => self.notice = Some(why),
        }
    }

    /// 现在能不能按保存。**混用且没选换行符时不许保存**(D3-4)——
    /// 静默统一等于把用户没碰过的那些行也改了,而 diff 里是整片飘红。
    pub fn can_save(&self) -> bool {
        self.read_only.is_none()
            && !self.busy
            && self.dirty()
            && !(self.eol == Eol::Mixed && self.eol_choice.is_none())
    }
}

/// 画编辑器窗口。返回本帧的动作。
///
/// `state` 传 `&mut Option<..>`:关窗要把它清成 `None`,这一步在这里做 ——
/// 交给调用方的话总有一条分支会漏(同 `files_dialog::show`)。
pub fn show(
    ctx: &egui::Context,
    t: &Theme,
    state: &mut Option<EditorState>,
) -> Option<EditorAction> {
    let s = state.as_mut()?;
    let mut action = None;
    let mut close = false;
    let title = format!("{}{}", if s.dirty() { "● " } else { "" }, s.path);

    egui::Window::new("编辑文件")
        .collapsible(false)
        .resizable(true)
        .default_size(egui::vec2(720.0, 480.0))
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            crate::ui::annotate::mark(ui.ctx(), "内置编辑器".to_string(), ui.max_rect());
            ui.label(egui::RichText::new(&title).color(theme::c32(t.fg_mid)));

            if let Some(why) = s.read_only {
                ui.colored_label(theme::c32(t.warn), format!("只读:{why}"));
            }
            if s.eol == Eol::Mixed {
                ui.horizontal(|ui| {
                    ui.colored_label(theme::c32(t.warn), "这个文件换行符混用,保存会把全文统一成:");
                    ui.selectable_value(&mut s.eol_choice, Some(Eol::Lf), "LF");
                    ui.selectable_value(&mut s.eol_choice, Some(Eol::Crlf), "CRLF");
                });
            }
            if let Some(n) = &s.notice {
                ui.colored_label(theme::c32(t.fg_dim), n);
            }

            ui.separator();
            egui::ScrollArea::vertical()
                .max_height(360.0)
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut s.text)
                            .code_editor()
                            .desired_width(f32::INFINITY)
                            .desired_rows(20)
                            // 只读一律靠这一条落地。靠「保存按钮置灰」是不够的:
                            // 用户改了半天才发现存不了,那些改动全白费。
                            .interactive(s.read_only.is_none()),
                    );
                });
            ui.separator();

            if s.confirm_close {
                ui.colored_label(theme::c32(t.danger), "有未保存的修改,关掉就没了。");
                ui.horizontal(|ui| {
                    if ui
                        .button(egui::RichText::new("丢弃并关闭").color(theme::c32(t.danger)))
                        .clicked()
                    {
                        action = Some(EditorAction::Close(s.key));
                        close = true;
                    }
                    if ui.button("继续编辑").clicked() {
                        s.confirm_close = false;
                    }
                });
                return;
            }

            ui.horizontal(|ui| {
                if ui
                    .add_enabled(s.can_save(), egui::Button::new("保存到远端"))
                    .clicked()
                {
                    action = Some(EditorAction::Save);
                }
                if ui.button("关闭").clicked() {
                    if s.dirty() {
                        s.confirm_close = true;
                    } else {
                        action = Some(EditorAction::Close(s.key));
                        close = true;
                    }
                }
                ui.checkbox(&mut s.backup, "写前留一份 .mullion.bak");
            });
        });

    if close {
        *state = None;
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editable() -> Option<EditorState> {
        Some(EditorState::new(
            1,
            "/etc/nginx/nginx.conf".into(),
            "server {}\n".into(),
            None,
            Eol::Lf,
            false,
        ))
    }

    /// 两帧渲染取全部文字(egui `Window` 首帧 fade_in 只记 `Shape::Noop`)。
    fn texts(state: &mut Option<EditorState>) -> Vec<String> {
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
                    show(ctx, &t, state);
                })
                .shapes;
        }
        let mut out = Vec::new();
        for cs in &shapes {
            walk(&cs.shape, &mut out);
        }
        out
    }

    /// 只读要**说明白为什么**。一个存不了又不解释的编辑器,用户只会觉得
    /// 程序坏了,而真实原因(二进制 / 编码)决定他下一步该干什么。
    #[test]
    fn a_read_only_file_says_why_instead_of_silently_dropping_edits() {
        let mut s = editable();
        s.as_mut().unwrap().read_only = Some("内容不是 UTF-8");
        let joined = texts(&mut s).join(" ");
        assert!(joined.contains("只读"), "没标出只读:{joined}");
        assert!(joined.contains("UTF-8"), "没说明原因:{joined}");
        assert!(!s.as_ref().unwrap().can_save(), "只读文件不许保存");
    }

    /// 只读**必须落在正文上**,不能只是把保存按钮置灰。
    ///
    /// 这条测试的由来:变异「`.interactive(s.read_only.is_none())` → 恒
    /// `true`」时,原先那批断言(标题上写着「只读」、`can_save()` 为假)
    /// 全绿 —— 用户照样能在一个非 UTF-8 文件里敲上半天,最后发现存不了。
    #[test]
    fn a_read_only_editor_ignores_typing_not_just_the_save_button() {
        let mut s = editable();
        s.as_mut().unwrap().read_only = Some("内容不是 UTF-8");
        type_into_body(&mut s, "X");
        assert_eq!(
            s.as_ref().unwrap().text,
            "server {}\n",
            "只读文件的正文被改动了 —— 光把保存按钮置灰是不够的"
        );

        // 反面:可编辑的文件必须真的打得进去,否则上面那条恒绿
        // (一个永远收不到键的编辑器同样满足它)。
        let mut ok = editable();
        type_into_body(&mut ok, "X");
        assert_ne!(
            ok.as_ref().unwrap().text,
            "server {}\n",
            "可编辑的文件没能接到键盘输入 —— 前一条断言于是什么也没守住"
        );
    }

    /// 脏了要在标题上看得见。看不见的话,用户关窗时才被拦一次,
    /// 而那时他已经以为自己保存过了。
    #[test]
    fn a_dirty_buffer_marks_the_title_so_the_user_can_see_unsaved_work() {
        let mut s = editable();
        assert!(!texts(&mut s).iter().any(|x| x.starts_with("● ")));
        s.as_mut().unwrap().text = "server { listen 80; }\n".into();
        assert!(s.as_ref().unwrap().dirty());
        assert!(
            texts(&mut s).iter().any(|x| x.starts_with("● ")),
            "脏了标题该带标记"
        );
    }

    /// D3-4:换行混用时,**没选换行符就不许保存**,而且界面要写明会统一。
    /// 静默统一的后果是一次「只改了一行」的提交在 diff 里整片飘红。
    #[test]
    fn a_mixed_eol_file_forces_an_explicit_choice_before_saving() {
        let mut s = editable();
        {
            let st = s.as_mut().unwrap();
            st.eol = Eol::Mixed;
            st.text = "a\nb\n".into(); // 弄脏,排除「不脏所以不能存」的干扰
        }
        assert!(s.as_ref().unwrap().dirty(), "前提:内容确实是脏的");
        assert!(
            !s.as_ref().unwrap().can_save(),
            "混用且没选换行符时不该允许保存"
        );
        let joined = texts(&mut s).join(" ");
        assert!(joined.contains("混用"), "界面要说明这件事:{joined}");

        s.as_mut().unwrap().eol_choice = Some(Eol::Crlf);
        assert!(s.as_ref().unwrap().can_save(), "选完之后就该能存了");
        assert_eq!(s.as_ref().unwrap().effective_eol(), Eol::Crlf);
    }

    /// 普通(非混用)文件不该被这条规则误伤 —— 误伤的话所有 LF 文件都存不了。
    #[test]
    fn an_ordinary_file_needs_no_eol_choice() {
        let mut s = editable();
        s.as_mut().unwrap().text = "changed\n".into();
        assert!(s.as_ref().unwrap().can_save());
        assert_eq!(s.as_ref().unwrap().effective_eol(), Eol::Lf);
    }

    /// 没改过就不必存 —— 允许的话,一次「打开看看」也会把远端 mtime 改掉,
    /// 而那正是别人判断「文件动没动」的依据。
    #[test]
    fn an_unchanged_buffer_cannot_be_saved() {
        let s = editable().unwrap();
        assert!(!s.dirty());
        assert!(!s.can_save());
    }

    /// 正在回传时保存按钮要锁住,否则连点两次会写两遍同一个文件。
    #[test]
    fn a_busy_editor_refuses_a_second_save() {
        let mut s = editable().unwrap();
        s.text = "changed\n".into();
        assert!(s.can_save());
        s.busy = true;
        assert!(!s.can_save());
    }

    /// 脏着点关闭要拦一次。直接关掉的话,用户的修改连一句提示都没有就没了。
    #[test]
    fn closing_a_dirty_editor_asks_before_throwing_the_changes_away() {
        let mut s = editable();
        s.as_mut().unwrap().text = "changed\n".into();
        let act = click(&mut s, "关闭");
        assert_eq!(act, None, "脏着关不该直接关掉");
        assert!(s.is_some(), "窗口该还在");
        assert!(s.as_ref().unwrap().confirm_close);
        let joined = texts(&mut s).join(" ");
        assert!(joined.contains("未保存"), "该说清后果:{joined}");

        assert_eq!(click(&mut s, "丢弃并关闭"), Some(EditorAction::Close(1)));
        assert!(s.is_none(), "确认之后窗口该关掉");
    }

    /// 干净的编辑器关起来不该拦 —— 拦了的话每次看一眼都要多点一下。
    #[test]
    fn closing_a_clean_editor_needs_no_confirmation() {
        let mut s = editable();
        assert_eq!(click(&mut s, "关闭"), Some(EditorAction::Close(1)));
        assert!(s.is_none());
    }

    /// 关窗动作**必须自带 key**。`show` 返回前 `*state` 已经是 `None`,
    /// 调用方要靠这个 key 才能收掉编辑会话和临时文件;不带的话临时目录
    /// 会一直涨,而「编辑中」列表里那条永远消不掉。
    #[test]
    fn the_close_action_carries_the_key_because_the_state_is_already_gone() {
        let mut s = Some(EditorState::new(
            77,
            "/tmp/a.conf".into(),
            "x\n".into(),
            None,
            Eol::Lf,
            false,
        ));
        let act = click(&mut s, "关闭");
        assert!(s.is_none(), "前提:show 已经把状态清掉了");
        assert_eq!(
            act,
            Some(EditorAction::Close(77)),
            "关的是哪一条必须写在动作里"
        );
    }

    fn find_button_pos(shapes: &[egui::epaint::ClippedShape], label: &str) -> Option<egui::Pos2> {
        fn walk(shape: &egui::Shape, label: &str) -> Option<egui::Pos2> {
            match shape {
                egui::Shape::Vec(v) => v.iter().find_map(|s| walk(s, label)),
                egui::Shape::Text(ts) if ts.galley.text() == label => {
                    Some(ts.pos + ts.galley.size() / 2.0)
                }
                _ => None,
            }
        }
        shapes.iter().find_map(|cs| walk(&cs.shape, label))
    }

    /// 在形状树里找**包含** `needle` 的那一处文字的中心点。正文那一块
    /// galley 里带着换行,拿它当按钮标签全等匹配是找不到的。
    fn find_text_pos(shapes: &[egui::epaint::ClippedShape], needle: &str) -> Option<egui::Pos2> {
        fn walk(shape: &egui::Shape, needle: &str) -> Option<egui::Pos2> {
            match shape {
                egui::Shape::Vec(v) => v.iter().find_map(|s| walk(s, needle)),
                egui::Shape::Text(ts) if ts.galley.text().contains(needle) => {
                    Some(ts.pos + ts.galley.size() / 2.0)
                }
                _ => None,
            }
        }
        shapes.iter().find_map(|cs| walk(&cs.shape, needle))
    }

    /// 点进正文取焦点,再敲一个字符进去。
    fn type_into_body(state: &mut Option<EditorState>, ch: &str) {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut out = None;
        for _ in 0..2 {
            out = Some(ctx.run(egui::RawInput::default(), |ctx| {
                show(ctx, &t, state);
            }));
        }
        let pos = find_text_pos(&out.unwrap().shapes, "server")
            .expect("找不到正文 —— 测试的定位写坏了,不是被测代码的问题");
        let mut click = egui::RawInput::default();
        for pressed in [true, false] {
            click.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            });
        }
        let _ = ctx.run(click, |ctx| {
            show(ctx, &t, state);
        });
        let mut typing = egui::RawInput::default();
        typing.events.push(egui::Event::Text(ch.to_string()));
        let _ = ctx.run(typing, |ctx| {
            show(ctx, &t, state);
        });
    }

    /// 点一下写着 `label` 的按钮,返回这一帧的动作。
    fn click(state: &mut Option<EditorState>, label: &str) -> Option<EditorAction> {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut out = None;
        for _ in 0..2 {
            out = Some(ctx.run(egui::RawInput::default(), |ctx| {
                show(ctx, &t, state);
            }));
        }
        let pos = find_button_pos(&out.unwrap().shapes, label)
            .unwrap_or_else(|| panic!("窗口里没有写着「{label}」的按钮"));
        let mut input = egui::RawInput::default();
        for pressed in [true, false] {
            input.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            });
        }
        let mut act = None;
        let _ = ctx.run(input, |ctx| {
            act = show(ctx, &t, state);
        });
        act
    }
}
