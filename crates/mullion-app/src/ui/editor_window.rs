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
    /// F208:上一帧实测的「外壳」尺寸(外框减内容区)= 标题栏 + 四圈边距。
    /// `Window::max_size` 管内容区,而尺寸预算说的是外框,差的就是这一截。
    /// 首帧还没量到,按零算,第二帧收敛。见 `max_size`。
    pub chrome: egui::Vec2,
    /// F204:还要摆正几帧。开窗时是 2,每帧减一,归零后位置归用户拖。
    ///
    /// **不能只靠 `Window::default_pos`**:egui 把窗口位置记在 `Memory` 里,
    /// 同一个窗口 id 第二次打开时 `default_pos` 早就不生效了 —— 用户在副屏
    /// 上把它拖走过一次,之后拔掉副屏,这个窗口就再也见不到了。
    ///
    /// **为什么是 2 帧而不是 1**:第一帧还不知道窗口实际有多高(内容撑出来
    /// 的高度和 `DEFAULT_SIZE` 不一样),只能先按默认尺寸估着摆;第二帧拿
    /// 上一帧量到的真实尺寸重摆一次才准。
    pub centre_frames: u8,
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
            chrome: egui::Vec2::ZERO,
            centre_frames: 2,
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

/// 窗口的默认大小。`centred_rect` 与 `Window::default_size` 共用一份 ——
/// 两处各写一遍的话,首帧钉的框和 egui 自己算的框会差一点,窗口开出来抖一下。
///
/// F208:从 720×480 提到 1100×760。480 点高的窗口里,减掉标题行/只读提示/
/// 换行选择/两条分隔线/底部按钮行之后正文只剩十来行,编辑一个配置文件都要
/// 一直滚。宽度提到 1100 是为了让 100 列以内的行不折行(远端配置和日志的
/// 常见宽度)。
const DEFAULT_SIZE: egui::Vec2 = egui::vec2(1100.0, 760.0);

/// F208:窗口上限占主窗口客户区的比例。
///
/// 用户实报「编辑器底部被 Windows 11 任务栏遮挡」。根因是 egui 的 `Resize`
/// 每帧都做 `desired_size = desired_size.max(last_content_size)`(0.30 的
/// `containers/resize.rs:258`),而本窗口的正文高度又是从窗口可用高度反推的
/// —— 两者互为因果,一帧涨一点,直到撞上 egui 的约束框(= 主窗口客户区,
/// 它本身可能就压在任务栏底下)。`default_size` 治不了:它只在这个窗口 id
/// **第一次出现**那一帧生效,之后 `Resize` 从 `Memory` 里读老尺寸。
///
/// 留 15% 的余量,窗口四周始终看得见底下的终端,「这是个浮窗、可以关掉」
/// 一眼就成立。
const MAX_SIZE_RATIO: f32 = 0.85;

/// F208:这一帧交给 `Window::max_size` 的上限。
///
/// **`max_size` 管的是内容区,而挡住任务栏的是外框** —— 两者差着标题栏加
/// 四圈边距(默认样式下约 50 点,随字号/DPI 变)。所以预算要先把外壳那一截
/// 扣掉;`chrome` 由上一帧实测(外框尺寸减内容区尺寸)得来,首帧没有,按
/// 零估,下一帧就收敛。**不照抄 egui 内部那套 `title_bar_height + margins`
/// 的算法**:那是私有实现,版本一变就静默算错,而实测的差值永远是对的。
fn max_size(screen: egui::Rect, chrome: egui::Vec2) -> egui::Vec2 {
    // 夹一个下界,免得极小窗口 + 大 chrome 算出负数(egui 会拿它当 min 用)。
    (screen.size() * MAX_SIZE_RATIO - chrome).max(egui::vec2(320.0, 240.0))
}

/// F204:一打开就摆在屏幕正中 —— 窗口左上角该落在哪。
///
/// `measured` 是上一帧量到的窗口实际尺寸,第一帧还没有,按 `DEFAULT_SIZE` 估。
/// **只钉位置不钉尺寸**:钉了尺寸的话,量到的就是被钉的那个值,等放手那一帧
/// 窗口缩回内容自然高度,又偏了一半的差额。
fn centred_pos(screen: egui::Rect, measured: Option<egui::Vec2>) -> egui::Pos2 {
    screen.center() - measured.unwrap_or(DEFAULT_SIZE) / 2.0
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

    // F204:Ctrl+S = 「保存到远端」那颗按钮,**包括它按不动的时候**。
    // `consume_key` 会把这次按键从事件流里取走,正文那个 `TextEdit` 就
    // 看不见它了(否则 S 会被当成一个字符打进去)。
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::S)) {
        if s.can_save() {
            action = Some(EditorAction::Save);
        } else if let Some(why) = s.read_only {
            // 「只读」「换行没选」是用户**必须先做点别的**才能存 ——
            // 沉默的话他只会一直按,以为程序坏了。
            s.notice = Some(format!("只读,存不了:{why}"));
        } else if s.eol == Eol::Mixed && s.eol_choice.is_none() {
            s.notice = Some("换行符混用,先在上面选一种再保存。".into());
        }
        // 剩下两种(没改过 / 正在回传)是按早了或按重了,静默 —— 弹话只会吵。
    }

    let screen = ctx.screen_rect();
    // F204:开窗头两帧把窗口摆到屏幕正中。见 `EditorState::centre_frames` 里
    // 为什么不能只靠 `default_pos`、以及为什么要两帧。
    let centre = if s.centre_frames > 0 {
        s.centre_frames -= 1;
        Some(centred_pos(screen, s.last_rect.map(|r| r.size())))
    } else {
        None
    };
    // 用「进入这一帧时」的状态决定钉不钉、钉到哪 —— 本帧里用户点击
    // 最大化/还原会改 `s.maximized`/`s.restore_to`,但那个改动到下一帧
    // 才生效,不然「点还原的这一帧」会把刚被点掉的满屏矩形误记成
    // last_rect(回到 1 的老问题)。
    let was_maximized = s.maximized;
    let pin = pinned_rect(was_maximized, s.restore_to, screen);
    let mut win = egui::Window::new("编辑文件")
        .collapsible(false)
        .resizable(true)
        .default_size(DEFAULT_SIZE)
        // F208:上限 —— 这一条才是「不再撑到任务栏底下」的判据,见 `max_size`。
        .max_size(max_size(screen, s.chrome))
        // F206:焦点描边三处同源。编辑器是模态(`Modal::Editor`),永远持有
        // 键盘,所以这里**不做条件判断** —— 条件恒真的边框写成 `if` 只会
        // 让人以为它会灭。egui 默认的窗口边框是白 6%、圆角 6,与 pane 的
        // 焦点线不是一回事。
        .frame(
            egui::Frame::window(&ctx.style())
                .stroke(theme::focus_ring(t))
                .rounding(theme::FOCUS_RING_ROUNDING),
        );
    if let Some(r) = pin {
        win = win.current_pos(r.min).fixed_size(r.size());
    } else if let Some(p) = centre {
        // 最大化/还原优先:那两个也在钉几何,同一帧里抢起来会打架。
        //
        // F208:开窗那两帧**连尺寸一起钉**,把 `Resize` 存在 `Memory` 里的
        // 老尺寸冲掉 —— 上一次被撑大的那个值否则会一直传下去,用户每次
        // 打开都得手动拖回来。钉完就放手,拖拽/最大化照旧。
        win = win.current_pos(p).fixed_size(DEFAULT_SIZE);
    }
    if !was_maximized {
        // 还原信号只用这一帧。
        s.restore_to = None;
    }

    // F208:量这一帧的内容区,配着外框算出「外壳」有多厚(见 `max_size`)。
    let mut content = egui::Vec2::ZERO;
    let resp = win.show(ctx, |ui| {
        crate::ui::annotate::mark(ui.ctx(), "内置编辑器".to_string(), ui.max_rect());
        content = ui.max_rect().size();
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&title).color(theme::c32(t.fg_mid)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                use crate::ui::icon::{icon_button, Glyph};
                // F204:`□` 和 `✕` 都是 T9 的豆腐块风险,一律自绘。
                // right_to_left 布局里先加的在最右边 —— ✕ 要在最外侧。
                if icon_button(ui, Glyph::Cross, true, "关闭") {
                    if s.dirty() {
                        s.confirm_close = true;
                    } else {
                        action = Some(EditorAction::Close(s.key));
                        close = true;
                    }
                }
                let (g, tip) = if s.maximized {
                    (Glyph::Restore, "还原")
                } else {
                    (Glyph::Maximize, "最大化")
                };
                if icon_button(ui, g, true, tip) {
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
            ui.colored_label(theme::c32(t.fg_muted), n);
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
                    // F207:正文区底色 = 终端底色 `term_bg`。用户看的是远端
                    // 文件,底色跟终端一致才连得上「这就是那台机器上的东西」;
                    // 而窗口壳仍是 `modal_bg`(#3f3f3f),两层色差本身就是
                    // 「哪块能打字」的边界。
                    //
                    // 走 `background_color` 而不是改 `Visuals::extreme_bg_color`
                    // ——后者是全局量,一改所有 `TextEdit`(会话表单、路径条、
                    // 改名框)跟着变,而那些贴在 `panel_bg` 上、本来就配好了。
                    .background_color(theme::c32(t.term_bg))
                    .desired_width(f32::INFINITY)
                    .desired_rows(20)
                    // 只读一律靠这一条落地。靠「保存按钮置灰」是不够的:
                    // 用户改了半天才发现存不了,那些改动全白费。
                    .interactive(s.read_only.is_none()),
            );
        });
        ui.separator();

        if s.confirm_close {
            ui.colored_label(theme::c32(t.danger_text), "有未保存的修改,关掉就没了。");
            ui.horizontal(|ui| {
                if ui
                    .button(egui::RichText::new("丢弃并关闭").color(theme::c32(t.danger_text)))
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
            // F204:关闭挪到标题栏的 ✕ 上了 —— 底下不再重复一颗。
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
    // F208:外框减内容区 = 外壳。跟 `last_rect` 分开更新 —— 最大化的那些帧
    // 内容区照样量得准,而 `last_rect` 是故意不更新的(它是「还原」的目标)。
    if let Some(r) = &resp {
        s.chrome = (r.response.rect.size() - content).max(egui::Vec2::ZERO);
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
        let act = click_icon(&mut s, "关闭");
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
        assert_eq!(click_icon(&mut s, "关闭"), Some(EditorAction::Close(1)));
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
        let act = click_icon(&mut s, "关闭");
        assert!(s.is_none(), "前提:show 已经把状态清掉了");
        assert_eq!(
            act,
            Some(EditorAction::Close(77)),
            "关的是哪一条必须写在动作里"
        );
    }

    /// C 组 / F204:标题行的最大化按钮真的能点、真的会翻转状态,
    /// **而且它的自述真的跟着变**。
    ///
    /// F204 把它从写字的 `small_button` 换成了自绘图标(`□`/`✕` 都是 T9
    /// 的豆腐块字符,自绘这条路不问字体)。图标不画文字,于是判据从
    /// 「画面上出现哪个词」改成「accesskit 里那颗按钮报的名字是什么」——
    /// 后者同时也是屏幕阅读器听到的东西。
    ///
    /// 自证会变红:把 `label` 那一行改成恒为 `"最大化"`。
    #[test]
    fn the_maximize_icon_toggles_state_and_flips_what_it_calls_itself() {
        let mut s = editable();
        assert!(!s.as_ref().unwrap().maximized, "前提:默认不是最大化");

        assert_eq!(
            click_icon(&mut s, "最大化"),
            None,
            "最大化不产生 EditorAction"
        );
        assert!(s.as_ref().unwrap().maximized, "点了最大化,状态该翻转");

        assert_eq!(click_icon(&mut s, "还原"), None);
        assert!(!s.as_ref().unwrap().maximized, "再点一次该还原");
    }

    /// F204:关闭挪到标题栏的 ✕ 上之后,底下那一行**不能再留一颗**
    /// 「关闭」—— 两颗做同一件事的按钮,用户会以为它们不一样。
    #[test]
    fn the_bottom_row_no_longer_repeats_the_close_button() {
        let mut s = editable();
        let seen = texts(&mut s);
        assert!(
            !seen.iter().any(|x| x == "关闭"),
            "底部还画着「关闭」按钮:{seen:?}"
        );
        // 反面:保存按钮必须还在,否则上面那条断言在一个空窗口上也成立。
        assert!(
            seen.iter().any(|x| x == "保存到远端"),
            "连保存按钮都没画出来 —— 上一条断言什么也没守住:{seen:?}"
        );
    }

    /// F204:窗口每次打开都摆在屏幕正中。
    ///
    /// egui 把窗口位置记在 `Memory` 里,不主动摆的话第二次打开还停在上次
    /// 拖走的地方 —— 用户在副屏上拖过一次、之后拔掉副屏,这个窗口就再也
    /// 见不到了,而它是模态的,等于程序卡死。
    #[test]
    fn the_editor_opens_centred_on_screen() {
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1920.0, 1080.0));
        let mut s = editable();
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        for _ in 0..3 {
            let mut input = egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            };
            input
                .viewports
                .values_mut()
                .for_each(|v| v.inner_rect = Some(screen));
            let _ = ctx.run(input, |ctx| {
                show(ctx, &t, &mut s);
            });
        }
        let r = s.as_ref().unwrap().last_rect.expect("窗口几何没落定");
        let d = (r.center() - screen.center()).abs();
        assert!(
            d.x < 4.0 && d.y < 4.0,
            "窗口中心 {:?} 离屏幕中心 {:?} 太远 —— 没摆正",
            r.center(),
            screen.center()
        );
    }

    /// F208:装一个很长的文件也不许把窗口撑出屏幕。
    ///
    /// 用户实报「编辑器底部被 Windows 11 任务栏遮挡」。egui 的 `Resize` 每帧
    /// `desired_size = desired_size.max(last_content_size)`,而本窗口的正文
    /// 高度又是从窗口可用高度反推的 —— 两者互为因果,一帧涨一点。跑够帧数
    /// 才看得见:只跑三帧的话涨幅还没超过默认尺寸,断言会空过。
    ///
    /// 自证会变红:把 `.max_size(max_size(screen, s.chrome))` 那一行删掉
    /// (实测 1038.7 点 > 918 点预算)。
    #[test]
    fn a_long_file_cannot_ratchet_the_window_past_the_screen_budget() {
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1920.0, 1080.0));
        let mut s = editable();
        // 两千行 —— 远超任何窗口高度,正文的自然高度稳稳撑满可用空间。
        s.as_mut().unwrap().text = "x\n".repeat(2000);
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        for _ in 0..40 {
            let mut input = egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            };
            input
                .viewports
                .values_mut()
                .for_each(|v| v.inner_rect = Some(screen));
            let _ = ctx.run(input, |ctx| {
                show(ctx, &t, &mut s);
            });
        }
        let r = s.as_ref().unwrap().last_rect.expect("窗口几何没落定");
        // 判据直接写「外框不超过屏幕的 85%」,不拿 `max_size` 反推 ——
        // 反推等于把被测函数抄一遍,它算错了这里也跟着错。
        let cap = screen.size() * MAX_SIZE_RATIO;
        assert!(
            r.height() <= cap.y + 1.0,
            "窗口高 {} 超了预算 {} —— 底边会压到任务栏底下",
            r.height(),
            cap.y
        );
        assert!(
            r.width() <= cap.x + 1.0,
            "窗口宽 {} 超了预算 {}",
            r.width(),
            cap.x
        );
        // 反面:窗口得真的画出来了,否则上面两条在一个零尺寸的框上也成立。
        assert!(
            r.height() > 100.0 && r.width() > 100.0,
            "窗口压根没铺开({r:?})—— 上面两条断言什么也没守住"
        );
    }

    /// F204:`centred_pos` 是「摆哪儿」的唯一出口,纯函数,用具体数字锁死。
    #[test]
    fn centred_pos_puts_the_window_in_the_middle_using_the_measured_size() {
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1920.0, 1080.0));
        // 第一帧没量到尺寸 —— 按 DEFAULT_SIZE(F208 起 1100×760)估。
        assert_eq!(centred_pos(screen, None), egui::pos2(410.0, 160.0));
        // 第二帧拿真实尺寸重摆:窗口比默认矮,左上角就该往下挪。
        assert_eq!(
            centred_pos(screen, Some(egui::vec2(732.0, 388.0))),
            egui::pos2(594.0, 346.0)
        );
        // 屏幕原点非零时也得跟着走(多显示器 / 客户区有偏移)。
        let off = egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(1920.0, 1080.0));
        assert_eq!(centred_pos(off, None) + DEFAULT_SIZE / 2.0, off.center());
    }

    /// F204:Ctrl+S 与「保存到远端」是同一件事 —— 包括**不能存的时候
    /// 也一样不存**。热键绕过 `can_save()` 的话,一个换行混用的文件会被
    /// 静默统一,而用户根本没做那个选择。
    #[test]
    fn ctrl_s_saves_exactly_when_the_save_button_would() {
        let mut s = editable();
        assert!(!s.as_ref().unwrap().can_save(), "前提:没改过,存不了");
        assert_eq!(press_ctrl_s(&mut s), None, "没改过就按 Ctrl+S,不该回传");

        s.as_mut().unwrap().text = "changed\n".into();
        assert!(s.as_ref().unwrap().can_save());
        assert_eq!(
            press_ctrl_s(&mut s),
            Some(EditorAction::Save),
            "改过之后 Ctrl+S 该回传"
        );
    }

    /// F204:按了 Ctrl+S 却存不了,得**说出为什么**。
    ///
    /// 「不脏 / 正忙」两种情况静默忽略:那是用户按早了或按重了,弹话反而吵。
    /// 「只读 / 换行没选」则是他改不掉现状、必须先做点别的 —— 沉默的话
    /// 他只会一直按,以为程序坏了。
    #[test]
    fn ctrl_s_on_a_file_that_cannot_be_saved_says_why() {
        let mut ro = editable();
        {
            let st = ro.as_mut().unwrap();
            st.read_only = Some("内容不是 UTF-8");
        }
        assert_eq!(press_ctrl_s(&mut ro), None);
        let n = ro.as_ref().unwrap().notice.clone().unwrap_or_default();
        assert!(n.contains("只读"), "只读文件按 Ctrl+S 没说明原因:{n:?}");

        let mut mixed = editable();
        {
            let st = mixed.as_mut().unwrap();
            st.eol = Eol::Mixed;
            st.text = "a\r\nb\n".into();
        }
        assert_eq!(press_ctrl_s(&mut mixed), None);
        let n = mixed.as_ref().unwrap().notice.clone().unwrap_or_default();
        assert!(n.contains("换行"), "换行没选就按 Ctrl+S 没说明原因:{n:?}");

        // 不脏的那种必须**保持沉默** —— 否则上面两条只是「总会写点什么」。
        let mut clean = editable();
        assert_eq!(press_ctrl_s(&mut clean), None);
        assert_eq!(
            clean.as_ref().unwrap().notice,
            None,
            "没改过就按 Ctrl+S,不该弹话"
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
        // 全程共用一个 ctx:窗口几何记在它的 Memory 里,换 ctx 等于重开窗口
        // (F204 的居中也只在开窗头两帧生效,换 ctx 后位置会跳)。
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let t = crate::theme::MULLION_DARK;
        // 热身,让非最大化状态下的 last_rect 先落定。
        for _ in 0..3 {
            let _ = ctx.run(egui::RawInput::default(), |c| {
                show(c, &t, &mut s);
            });
        }
        let before_max = s.as_ref().unwrap().last_rect;
        assert!(before_max.is_some(), "热身之后 last_rect 该有值了");

        assert_eq!(click_icon_in(&ctx, &mut s, "最大化"), None);
        assert!(s.as_ref().unwrap().maximized);
        // 最大化期间不该再更新 last_rect —— 它应该还是最大化前记的那个。
        assert_eq!(
            s.as_ref().unwrap().last_rect,
            before_max,
            "最大化期间 last_rect 不该被满屏矩形覆盖"
        );

        assert_eq!(click_icon_in(&ctx, &mut s, "还原"), None);
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

    /// F204:点标题栏上那两颗自绘图标。它们不画文字,只能靠 accesskit 定位 ——
    /// 这同时也验了「屏幕阅读器听得见它叫什么」。
    fn click_icon(state: &mut Option<EditorState>, tooltip: &str) -> Option<EditorAction> {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        click_icon_in(&ctx, state, tooltip)
    }

    /// 同上,但在**调用方给的**那个 `Context` 里点。
    ///
    /// 窗口几何(位置、尺寸、居中用掉的帧数)全都记在 `Context` 的 `Memory`
    /// 里。一串动作要连起来看几何怎么变时,必须共用同一个 ctx —— 每次新建
    /// 一个等于把窗口重新开了一遍。
    fn click_icon_in(
        ctx: &egui::Context,
        state: &mut Option<EditorState>,
        tooltip: &str,
    ) -> Option<EditorAction> {
        let t = crate::theme::MULLION_DARK;
        let mut update = None;
        // 两帧:egui `Window` 首帧只记 `Shape::Noop`,几何也还没落定。
        for _ in 0..2 {
            update = ctx
                .run(egui::RawInput::default(), |ctx| {
                    show(ctx, &t, state);
                })
                .platform_output
                .accesskit_update;
        }
        let b = update
            .expect("没有 accesskit 输出")
            .nodes
            .iter()
            .find(|(_, n)| n.label() == Some(tooltip))
            .and_then(|(_, n)| n.bounds())
            .unwrap_or_else(|| panic!("标题栏上找不到自述为「{tooltip}」的图标按钮"));
        let pos = egui::pos2(
            (b.x0 as f32 + b.x1 as f32) / 2.0,
            (b.y0 as f32 + b.y1 as f32) / 2.0,
        );
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

    /// F204:敲一次 Ctrl+S,返回这一帧的动作。
    fn press_ctrl_s(state: &mut Option<EditorState>) -> Option<EditorAction> {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                show(ctx, &t, state);
            });
        }
        let m = egui::Modifiers::COMMAND;
        let mut input = egui::RawInput {
            modifiers: m,
            ..Default::default()
        };
        input.events.push(egui::Event::Key {
            key: egui::Key::S,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: m,
        });
        let mut act = None;
        let _ = ctx.run(input, |ctx| {
            act = show(ctx, &t, state);
        });
        act
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
