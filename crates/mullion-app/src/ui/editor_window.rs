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
    /// C 组:最大化状态。`true` 时窗口铺满主窗口客户区。
    /// 纯 UI 运行态,不持久化 —— 关掉编辑器就没了(与 F37 的布局持久化
    /// 无关,那存的是标签与分屏形状)。
    pub maximized: bool,
    /// C 组:最大化之前(非最大化状态下)最近一次渲染出的窗口矩形。
    /// 每个非最大化帧都更新 —— 「还原」要钉回这里。
    pub last_rect: Option<egui::Rect>,
    /// C 组:一次性信号 ——「下一帧把窗口钉回这个矩形」,用完即清。
    /// 不这样做的话,停止钉满屏几何后 egui 会把上一帧的全屏尺寸当成窗口
    /// 当前状态一直留着,「还原」等于没按。
    pub restore_to: Option<egui::Rect>,
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
            maximized: false,
            last_rect: None,
            restore_to: None,
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

/// C 组:窗口这一帧要不要钉几何、钉到哪。
/// - 最大化:每帧钉满屏(不每帧钉的话,用户在最大化状态下拖边缘,egui 会
///   记住那个尺寸,再按「还原」就还原不回去了)。
/// - 刚点了还原:钉回最大化之前那一帧记下的矩形**一帧**,然后放手 ——
///   不钉的话 egui 会把全屏那个尺寸当成窗口当前状态一直留着,「还原」
///   等于没按。
/// - 其余情况:不钉,窗口归用户拖。
fn pinned_rect(
    maximized: bool,
    restore_to: Option<egui::Rect>,
    screen: egui::Rect,
) -> Option<egui::Rect> {
    if maximized {
        Some(screen)
    } else {
        restore_to
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

    let screen = ctx.screen_rect();
    // 用「进入这一帧时」的状态决定钉不钉、钉到哪 —— 本帧里用户点击
    // 最大化/还原会改 `s.maximized`/`s.restore_to`,但那个改动到下一帧
    // 才生效,不然「点还原的这一帧」会把刚被点掉的满屏矩形误记成
    // last_rect(回到 1 的老问题)。
    let was_maximized = s.maximized;
    let pin = pinned_rect(was_maximized, s.restore_to, screen);
    let mut win = egui::Window::new("编辑文件")
        .collapsible(false)
        .resizable(true)
        .default_size(egui::vec2(720.0, 480.0));
    if let Some(r) = pin {
        win = win.current_pos(r.min).fixed_size(r.size());
    }
    if !was_maximized {
        // 还原信号只用这一帧。
        s.restore_to = None;
    }

    let resp = win.show(ctx, |ui| {
        crate::ui::annotate::mark(ui.ctx(), "内置编辑器".to_string(), ui.max_rect());
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&title).color(theme::c32(t.fg_mid)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = if s.maximized { "还原" } else { "最大化" };
                if ui.small_button(label).clicked() {
                    if s.maximized {
                        // 还原:钉回最大化之前记下的矩形。
                        s.maximized = false;
                        s.restore_to = s.last_rect;
                    } else {
                        s.maximized = true;
                    }
                }
            });
        });

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
        // C 组:高度跟着窗口走。**减去底部按钮行的预算** —— 不减的话
        // `ScrollArea` 会把可用高度吃光,保存/关闭那一行被挤出窗口。
        let reserve = ui.spacing().interact_size.y + ui.spacing().item_spacing.y * 2.0;
        let h = (ui.available_height() - reserve).max(80.0);
        egui::ScrollArea::vertical().max_height(h).show(ui, |ui| {
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

    if !was_maximized {
        // 非最大化的每一帧都记下渲染出的矩形 —— 「还原」要钉回这里。
        // 用 `was_maximized`(帧首状态)而不是这一帧点击后的
        // `s.maximized`:点了「还原」的这一帧,窗口渲染出来的还是满屏
        // 矩形,拿它去更新 `last_rect` 会把刚要还原回去的目标覆盖掉。
        if let Some(r) = &resp {
            s.last_rect = Some(r.response.rect);
        }
    }

    if close {
        *state = None;
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    /// C 组:编辑器窗口不许锚死、不许写死编辑区高度。
    ///
    /// `anchor` 会让 `egui::Window` 完全无法拖动(egui 0.30:设了 anchor 就
    /// 忽略用户拖拽的位移);编辑区高度写死会让窗口放大后编辑区仍停在
    /// 原高度 —— 两者合起来就是「没法全屏」。
    ///
    /// 扎源码而不是造窗口:这两条都是**代码里有没有这一行**的事实,
    /// 而窗口的实际可拖性要真人拖一下才知道。
    ///
    /// 自证会变红:把 `.anchor(egui::Align2::CENTER_CENTER, ..)` 加回去。
    #[test]
    fn the_editor_window_is_neither_anchored_nor_height_locked() {
        let src = include_str!("editor_window.rs");
        let prod = src.split("#[cfg(test)]").next().expect("源码切歪了");
        assert!(
            !prod.contains(".anchor("),
            "编辑器窗口还锚着 —— 锚死的 egui::Window 拖不动"
        );
        assert!(
            !prod.contains("max_height(360.0)"),
            "编辑区高度还写死在 360 —— 窗口放大了它也不跟着长"
        );
    }

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

    /// C 组:标题行的最大化/还原按钮真的能点、真的会翻转状态,
    /// **而且按钮上的文字真的跟着变**。
    ///
    /// 单靠后面这行 `texts()` 断言之前,「文案跟着变」只是被
    /// `click()` 内部的 `find_button_pos` 间接测住:文案不对的话它就找不到
    /// 叫「还原」的按钮而直接 panic —— 失败信息说的是「找不到按钮」,
    /// 不是「文案不对」,诊断性差。这里显式断言画面上出现的文字。
    #[test]
    fn the_maximize_button_toggles_state_and_flips_its_own_label() {
        let mut s = editable();
        assert!(!s.as_ref().unwrap().maximized, "前提:默认不是最大化");
        assert!(
            texts(&mut s).iter().any(|x| x == "最大化"),
            "初始态按钮该写着「最大化」"
        );

        assert_eq!(click(&mut s, "最大化"), None, "最大化不产生 EditorAction");
        assert!(s.as_ref().unwrap().maximized, "点了最大化,状态该翻转");
        assert!(
            texts(&mut s).iter().any(|x| x == "还原"),
            "最大化之后按钮该改写成「还原」"
        );

        assert_eq!(click(&mut s, "还原"), None);
        assert!(!s.as_ref().unwrap().maximized, "再点一次该还原");
        assert!(
            texts(&mut s).iter().any(|x| x == "最大化"),
            "还原之后按钮该改回「最大化」"
        );
    }

    /// C 组:`pinned_rect` 是几何决策的唯一出口,纯函数,单独锁住三种输入。
    /// 用具体数字断言具体数字 —— 不在测试里重抄一遍函数体,否则「改坏了
    /// 函数,测试跟着错」这种变异测不出来。
    #[test]
    fn pinned_rect_covers_its_three_cases_with_concrete_numbers() {
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1920.0, 1080.0));
        let restore = egui::Rect::from_min_size(egui::pos2(100.0, 80.0), egui::vec2(720.0, 480.0));

        // 最大化:不管 restore_to 是什么,钉满屏。
        assert_eq!(pinned_rect(true, None, screen), Some(screen));
        assert_eq!(pinned_rect(true, Some(restore), screen), Some(screen));

        // 非最大化 + 有还原信号:钉回那个矩形。
        assert_eq!(pinned_rect(false, Some(restore), screen), Some(restore));

        // 非最大化 + 没有还原信号:不钉,窗口归用户拖。
        assert_eq!(pinned_rect(false, None, screen), None);
    }

    /// C 组:点「还原」记下的 `restore_to`,必须是最大化**之前**那个窗口
    /// 大小的矩形,而不是最大化期间那个满屏矩形 —— 否则「还原」等于
    /// 把窗口重新摆回满屏,跟没按一样。
    ///
    /// 如果这条在无头环境下因窗口几何预热不稳而失败,应如实报告,不得
    /// 改造成恒绿断言。
    #[test]
    fn restoring_records_the_pre_maximize_rect_not_the_full_screen_one() {
        let mut s = editable();
        // 热身,让非最大化状态下的 last_rect 先落定。
        let _ = texts(&mut s);
        let before_max = s.as_ref().unwrap().last_rect;
        assert!(before_max.is_some(), "热身之后 last_rect 该有值了");

        assert_eq!(click(&mut s, "最大化"), None);
        assert!(s.as_ref().unwrap().maximized);
        // 最大化期间不该再更新 last_rect —— 它应该还是最大化前记的那个。
        assert_eq!(
            s.as_ref().unwrap().last_rect,
            before_max,
            "最大化期间 last_rect 不该被满屏矩形覆盖"
        );

        assert_eq!(click(&mut s, "还原"), None);
        assert!(!s.as_ref().unwrap().maximized);
        assert_eq!(
            s.as_ref().unwrap().restore_to,
            before_max,
            "还原信号该钉回最大化之前的矩形,而不是满屏矩形"
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
