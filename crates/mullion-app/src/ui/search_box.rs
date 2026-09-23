//! F287:带「清空」叉的搜索框 —— 全库六个筛选输入框的唯一实现。
//!
//! **为什么抽成构件**:这六个框(启动页、项目管理器左栏、换节点、选项目、
//! 会话管理器列表、文件面板远端栏递归搜索)长得一样、行为该一样,但散在六个
//! 文件里。上一次「给搜索框加个东西」的改动只落到其中一个,另外五个静默留在
//! 旧样子 —— 这种缺口编译器看不见、测试也看不见,只有用户切到另一个页面才
//! 发现。抽成构件之后「六个全一致」由**类型系统**保证,不靠自觉。

//! **叉的语义是「把字一个个删光」,不是别的**。差别都在看不见的地方:
//! 光 `String::clear()` 而不动 `TextEditState`,游标会留在原来的字符位置上,
//! 下一帧 egui 按旧游标去索引一个空串;`Response::changed()` 不标的话,凡是靠
//! `changed()` 做增量反应的调用方(F278 的搜索作废就是)会完全错过这次清空,
//! 而画面上字确实没了 —— 典型的「看着好了、实际没有」。

use crate::ui::icon::{self, Glyph};
use crate::ui::metrics::FIELD_W_MIN;

/// 叉占的横向宽度(含它与右边框之间的余量)。
///
/// 这个值有**两处**用途,必须同源:画叉的位置,以及把 `TextEdit` 的右内边距
/// 撑开同样多。少撑这一下,长搜索词会一直延伸到叉底下 —— 字与叉互相叠印,
/// 而两者都还在、谁也不遮住谁,看上去就是一团糊。
pub const CLEAR_W: f32 = 20.0;

/// 叉本身的边长。比 [`CLEAR_W`] 小,差出来的是它与右边框之间的呼吸。
const CROSS_SIZE: f32 = 13.0;

/// `TextEdit` 内边距的左侧与上下,与 egui 的默认值一致(`Margin::symmetric(4,2)`)。
/// 只有右侧被我们撑开成 `M_X + CLEAR_W`。
const M_X: f32 = 4.0;
const M_Y: f32 = 2.0;

/// 叉画在哪 —— 由 `TextEdit` 的 `Response::rect` 反推。
///
/// **`resp.rect` 是内容区,不是外框**:egui 0.30 在返回前做了
/// `output.response.rect = outer_rect - margin`(builder.rs:432)。撑开的那
/// 一段右内边距整个落在 `resp.rect` **外面**,叉就画在那里 —— 照着
/// `resp.rect.right()` 往**里**退一个叉宽的话,叉正好压在文字上,而「撑开
/// 内边距」这一步等于白做,两件事一起失效,画面上只看得出「有点挤」。
fn cross_rect(inner: egui::Rect) -> egui::Rect {
    egui::Rect::from_center_size(
        egui::pos2(inner.right() + CLEAR_W / 2.0, inner.center().y),
        egui::Vec2::splat(CROSS_SIZE),
    )
}

/// 画一个搜索框,非空时在框内右端叠一个清空叉。
///
/// - `id`:**调用方给**,不让构件自己编。F278 的远端栏搜索框把它的
///   `TextEdit` id 当作键盘路由的锚(`find_edit_id(pane, generation)`),
///   构件自作主张换一个 id,那条路由会静默失灵。
/// - `desired_width`:与裸 `TextEdit::desired_width` 同义(内容区宽度)。
///
/// 返回输入框自己的 `Response`。点叉清空也会让它 `changed()` 为真。
pub fn search_box(
    ui: &mut egui::Ui,
    text: &mut String,
    id: egui::Id,
    hint: impl Into<egui::WidgetText>,
    desired_width: f32,
) -> egui::Response {
    let mut resp = ui.add(
        egui::TextEdit::singleline(text)
            .id(id)
            .hint_text(hint)
            // 右内边距撑开一个叉那么宽:这是「长词不被叉盖住」的**唯一**机制。
            // `desired_width` 只圈内容区、不含内边距,所以下面把它也扣掉同样
            // 一段,外框总宽与改造前保持一致 —— 六处调用的既有布局不用跟着调。
            .margin(egui::Margin {
                left: M_X,
                right: M_X + CLEAR_W,
                top: M_Y,
                bottom: M_Y,
            })
            .desired_width((desired_width - CLEAR_W).max(FIELD_W_MIN)),
    );

    // 空框不画叉:没东西可清的时候摆一个按钮是纯噪音,而且 hint 文字正占着
    // 那一片,叉压在提示语上更难认。
    if text.is_empty() {
        return resp;
    }

    let r = resp.rect;
    // 外框 = 内容区 + 内边距。热区要按外框夹,**不能按 `r` 夹** —— 叉整个
    // 画在 `r` 外面,拿 `r` 去 `intersect` 会把热区裁成空矩形,叉画得出来
    // 却永远点不中(零报错)。
    let outer = egui::Rect::from_min_max(
        r.min - egui::vec2(M_X, M_Y),
        r.max + egui::vec2(M_X + CLEAR_W, M_Y),
    );
    let cross = cross_rect(r);
    // 热区按 `interact_size.y` 放大到点得中(十来像素的方块在高 DPI 小框里
    // 要对准),但**不许越出输入框的外边** —— 越出去会盖住同一行紧挨着的下
    // 一个控件,那个控件从此点不中,而画面上什么都看不出来(「手写 clickable
    // 行的右半边点不中」那条坑的反面)。
    let hit = egui::Rect::from_center_size(
        cross.center(),
        egui::Vec2::splat(ui.spacing().interact_size.y.max(CROSS_SIZE)),
    )
    .intersect(outer);
    let click = ui.interact(hit, id.with("clear"), egui::Sense::click());
    let click = click.on_hover_text("清空搜索");
    // 图标按钮一个字都不画 —— 不报的话它在 accesskit 树里是个没名字的空节点,
    // F100 的自动候选认不出它是谁(同 `icon::icon_button` 的理由)。
    click.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "清空搜索"));

    if ui.is_rect_visible(cross) {
        let color = if click.hovered() {
            ui.visuals().text_color()
        } else {
            ui.visuals().weak_text_color()
        };
        ui.painter().extend(icon::shapes(
            cross,
            Glyph::Cross,
            egui::Stroke::new(1.3, color),
        ));
    }

    if click.clicked() {
        clear_in_place(ui.ctx(), id, text);
        resp.mark_changed();
    }
    resp
}

/// 把框清空成「用户一路退格删到底」的那个状态。
///
/// 三件事缺一不可,而缺了哪一件画面上都看不出来:
/// 1. 正文清空 —— 唯一肉眼可见的一步。
/// 2. **游标归零**。`TextEditState` 独立于这个 `String` 存在 memory 里,
///    留着旧游标的话下一帧 egui 拿它去索引一个空串。
/// 3. **焦点留在框里**。用户点叉是为了接着重打,不是为了退出搜索;焦点跑掉
///    的话他得再点一次框(在 F278 那种靠模态托管键盘的地方,焦点丢了等于
///    整条搜索路径断掉)。
fn clear_in_place(ctx: &egui::Context, id: egui::Id, text: &mut String) {
    text.clear();
    if let Some(mut st) = egui::TextEdit::load_state(ctx, id) {
        st.cursor
            .set_char_range(Some(egui::text::CCursorRange::default()));
        egui::TextEdit::store_state(ctx, id, st);
    }
    ctx.memory_mut(|m| m.request_focus(id));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eid() -> egui::Id {
        egui::Id::new("被测搜索框")
    }

    /// 一次完整的左键点击(按下 + 抬起)。`PointerMoved` 不能省:egui 的交互
    /// 靠指针位置,只发按钮事件时 hover 判定拿不到坐标。
    fn click_at(pos: egui::Pos2) -> egui::RawInput {
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
        input
    }

    /// 跑一帧,返回 (输入框 rect, 这一帧 `changed()`, 画出来的全部图形)。
    fn frame(
        ctx: &egui::Context,
        text: &mut String,
        input: egui::RawInput,
    ) -> (egui::Rect, bool, Vec<egui::epaint::ClippedShape>) {
        let mut rect = egui::Rect::NOTHING;
        let mut changed = false;
        let out = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let r = search_box(ui, text, eid(), "搜点什么", 200.0);
                rect = r.rect;
                changed = r.changed();
            });
        });
        (rect, changed, out.shapes)
    }

    /// 把一串字符敲进框里(走真的键盘事件,不是直接改 `String`)——
    /// `TextEditState` 因此是 egui 自己建的,跟用户手打出来的一模一样。
    fn type_text(ctx: &egui::Context, text: &mut String, s: &str) {
        ctx.memory_mut(|m| m.request_focus(eid()));
        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::Text(s.to_owned()));
        frame(ctx, text, input);
    }

    /// 数**落在叉那一小块里**的线段 —— `Glyph::Cross` 是两条对角线。
    ///
    /// 用「有没有线段」而不是「有没有某个文字」:叉是自绘的,白名单里那个
    /// `×` 字符根本没进过绘制流(T9 的纪律)。
    ///
    /// 限定在叉的矩形内、而不是数整帧:`TextEdit` 的闪烁光标**也是**一条
    /// `LineSegment`,数整帧会把它一起算进来,判据就跟着焦点状态飘。
    fn crosses_drawn(shapes: &[egui::epaint::ClippedShape], rect: egui::Rect) -> usize {
        let zone = cross_rect(rect).expand(2.0);
        fn walk(s: &egui::Shape, zone: egui::Rect, n: &mut usize) {
            match s {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, zone, n)),
                egui::Shape::LineSegment { points, .. }
                    if points.iter().all(|p| zone.contains(*p)) =>
                {
                    *n += 1;
                }
                _ => {}
            }
        }
        let mut n = 0;
        shapes.iter().for_each(|c| walk(&c.shape, zone, &mut n));
        n
    }

    /// 空框不画叉:没东西可清的时候摆一个按钮是纯噪音,而且 hint 文字正占着
    /// 那一片,叉压在提示语上更难认。
    #[test]
    fn an_empty_box_draws_no_clear_cross() {
        let ctx = egui::Context::default();
        let mut text = String::new();
        let (rect, _, shapes) = frame(&ctx, &mut text, egui::RawInput::default());
        assert_eq!(
            crosses_drawn(&shapes, rect),
            0,
            "空搜索框不该画叉(叉是两条对角线段)"
        );
    }

    /// 框里有字就得有叉 —— 这是 F287 的整条需求。
    #[test]
    fn a_box_with_text_draws_the_clear_cross() {
        let ctx = egui::Context::default();
        let mut text = String::new();
        type_text(&ctx, &mut text, "日志");
        let (rect, _, shapes) = frame(&ctx, &mut text, egui::RawInput::default());
        assert_eq!(
            crosses_drawn(&shapes, rect),
            2,
            "框里有字时右端必须有一个叉(两条对角线段)"
        );
    }

    /// **F287 的核心判据**:点叉之后的状态,必须与「一路退格删到底」之后的
    /// 状态**完全一致** —— 正文、`TextEditState` 里的游标、键盘焦点,三样
    /// 都算。
    ///
    /// 为什么要比到游标这一层:`TextEditState` 独立于这个 `String` 存在
    /// memory 里。只 `String::clear()` 的话游标还停在第 N 个字符上,下一帧
    /// egui 拿它去索引一个空串 —— 轻则 hint 不出现,重则接着打的字落在
    /// 奇怪的位置。而这**在画面上和正常情况分辨不出来**,只有比状态才看得见。
    #[test]
    fn clicking_the_cross_lands_where_backspacing_it_all_would() {
        // 路径 A:一路退格删光。
        let a_ctx = egui::Context::default();
        let mut a = String::new();
        type_text(&a_ctx, &mut a, "日志");
        for _ in 0..2 {
            let mut input = egui::RawInput::default();
            input.events.push(egui::Event::Key {
                key: egui::Key::Backspace,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            });
            frame(&a_ctx, &mut a, input);
        }
        frame(&a_ctx, &mut a, egui::RawInput::default());

        // 路径 B:点叉。
        let b_ctx = egui::Context::default();
        let mut b = String::new();
        type_text(&b_ctx, &mut b, "日志");
        let (rect, ..) = frame(&b_ctx, &mut b, egui::RawInput::default());
        let (_, changed, _) = frame(&b_ctx, &mut b, click_at(cross_rect(rect).center()));
        frame(&b_ctx, &mut b, egui::RawInput::default());

        assert_eq!(a, "", "前提:退格确实把字删光了");
        assert_eq!(b, "", "点叉没把正文清空");
        assert!(
            changed,
            "点叉清空没有标 changed() —— 靠 changed() 做增量反应的调用方\
             (F278 的搜索作废)会完全错过这次清空,而画面上字确实没了"
        );

        let cur = |ctx: &egui::Context| {
            egui::TextEdit::load_state(ctx, eid()).and_then(|s| s.cursor.char_range())
        };
        assert_eq!(
            cur(&b_ctx),
            cur(&a_ctx),
            "点叉之后的游标和一路退格之后的不一样 —— 下一帧 egui 会拿旧游标\
             去索引一个空串"
        );
        assert_eq!(
            b_ctx.memory(|m| m.focused()),
            Some(eid()),
            "点叉把键盘焦点弄丢了 —— 用户点叉是为了接着重打,不是为了退出搜索"
        );
    }

    /// 长搜索词不许延伸到叉底下。
    ///
    /// 唯一的机制是把 `TextEdit` 的右内边距撑开一个叉那么宽;少撑这一下,
    /// 字与叉互相叠印(两者都还在、谁也不遮住谁),看上去就是一团糊 ——
    /// 而这种糊**只有人眼能发现**。
    #[test]
    fn a_long_query_never_runs_under_the_cross() {
        let ctx = egui::Context::default();
        let mut text = String::new();
        type_text(
            &ctx,
            &mut text,
            "一条长得装不下的搜索词一条长得装不下的搜索词",
        );
        let (rect, _, shapes) = frame(&ctx, &mut text, egui::RawInput::default());

        // **必须与 `clip_rect` 求交**:超长文本的 galley 整体远比框宽,
        // `TextShape` 的 `visual_bounding_rect()` 给的是那整条 galley 的
        // 范围,而真正上屏的只有 `clip_rect` 圈住的那一段。拿未裁剪的范围
        // 当判据,这条守护永远红,且红得跟实现对不对无关。
        fn rightmost_text(s: &egui::Shape, clip: egui::Rect, acc: &mut f32) {
            match s {
                egui::Shape::Vec(v) => v.iter().for_each(|s| rightmost_text(s, clip, acc)),
                egui::Shape::Text(t) => {
                    let painted = t.visual_bounding_rect().intersect(clip);
                    if painted.is_positive() {
                        *acc = acc.max(painted.right());
                    }
                }
                _ => {}
            }
        }
        let mut right = f32::NEG_INFINITY;
        shapes
            .iter()
            .for_each(|c| rightmost_text(&c.shape, c.clip_rect, &mut right));
        assert!(right.is_finite(), "一个字都没画出来,这条守护量不到东西");

        let cross_left = cross_rect(rect).left();
        assert!(
            right <= cross_left,
            "搜索词一直画到 {right:.0},越过了叉的左边界 {cross_left:.0}\
             (框右边 {:.0})—— 字和叉会叠印成一团",
            rect.right()
        );
    }
}
