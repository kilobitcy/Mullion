//! 一次性操作反馈(走查 13):保存 / 删除 / 移动分组成功后,底部飘一条
//! 三秒自动消失的短提示。
//!
//! **为什么不是错误卡片**:那张卡片是「出事了,你得处理」,要占位置、要等
//! 用户关掉;这里要的是「你刚才那一下生效了」,看一眼就够,不该留在界面上。
//!
//! **时间源是 `ctx.input(|i| i.time)`,不是 `Instant::now()`**:后者在无头
//! 测试里没法拨快,一条三秒的 toast 就得真等三秒;帧时间可以直接在
//! `RawInput::time` 里给,测试里想跳到第几秒就跳到第几秒。

use crate::theme::{self, Theme};

/// 一条 toast 的存活时长(秒)。
pub const TTL: f64 = 3.0;

/// F263:一条 toast 最宽多少(逻辑点)。
///
/// 480 ≈ 一行 60 个汉字,再宽眼睛要横扫,而这东西只在屏幕上停三秒。
pub const MAX_W: f32 = 480.0;

/// F263:这一帧 toast 的宽度预算。
///
/// # 为什么必须显式给,而不是让它自己长
///
/// 用户实报「toast 总是很竖长」。根因是 `egui::Area` 的**尺寸棘轮**:
/// `Area::end` 每帧把 `state.size` 记成这一帧内容的 `min_size`,下一帧的
/// `max_rect` 就从那个尺寸来(`egui-0.30.0/src/containers/area.rs`)。于是
/// 上一条 toast 是「已保存」,这一条一百来字的失败原因就被压在那三个字的
/// 宽度里换行 —— 十几行的一根竖条。`Id` 是固定的 `"mullion_toast"`,
/// 这份状态在两条 toast 之间一直留着。
///
/// `Ui::set_max_width` 直接改 `max_rect.max.x`(`placer.set_max_width`),
/// **可以把预算改大**,所以它切得断这根棘轮;光设一个上限是不够的。
///
/// 半屏那一档:窄窗口里 480 会顶到两边;下限 200 是防着极窄窗口把文字挤成
/// 每行一个字 —— 那种窗口下 toast 宽过半屏也无所谓,它本来就是临时浮层。
pub fn max_width(screen_w: f32) -> f32 {
    MAX_W.min(screen_w * 0.5).max(200.0)
}

/// F213:这一条说的是哪一档事。
///
/// 原来边框无论内容一律画 `ok` 绿 —— 「正在上传截图…」(还没落地)和
/// 「隧道已停止:连接被拒」(降级了)都镶一圈成功色,颜色在说谎。
/// 三档都是**非文字**元素(1px 边框),判据是 3:1,不是 4.5:1。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// 进行中,结果未知(「正在打开…」)。
    Busy,
    /// 干成了。
    Ok,
    /// 做完了,但有失败或降级(「3 条已连接,1 条失败」)。
    ///
    /// 注意与 `set_error` 的分工:要用户**处理**的失败走错误卡片,飘一下就
    /// 没了的这里只管「结果打了折,你知道一下」。
    Warn,
}

impl Kind {
    /// 边框色。三档在 `modal_bg`(#3f3f3f)上分别是 4.11 / 6.17 / 5.59,
    /// 都过了非文字 3:1 的门槛。
    pub fn stroke(self, t: &Theme) -> mullion_term::snapshot::Rgb {
        match self {
            Kind::Busy => t.info,
            Kind::Ok => t.ok,
            Kind::Warn => t.warn,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    pub text: String,
    pub kind: Kind,
    /// 进场时的帧时间。
    pub born: f64,
}

/// 还能活多久(秒)。`<= 0` 表示该消失了。
pub fn remaining(toast: &Toast, now: f64) -> f64 {
    TTL - (now - toast.born)
}

/// 画当前这条 toast,顺便负责它的生老病死。
///
/// `pending` 是生产端(`app.rs` 施加意图那一段)扔进来的文本 —— 生产端拿不到
/// `egui::Context`,也就拿不到帧时间,所以「文本」和「什么时候进的场」分成两步:
/// 这里 `take()` 出来时才盖时间戳。
pub fn show(
    ctx: &egui::Context,
    t: &Theme,
    pending: &mut Option<(Kind, String)>,
    live: &mut Option<Toast>,
) {
    let now = ctx.input(|i| i.time);
    if let Some((kind, text)) = pending.take() {
        *live = Some(Toast {
            text,
            kind,
            born: now,
        });
    }
    let Some(toast) = live.as_ref() else {
        return;
    };
    let left = remaining(toast, now);
    if left <= 0.0 {
        *live = None;
        return;
    }
    // 到点了得自己醒过来把自己抹掉:事件循环是帧率节流 + `WaitUntil` 的
    // (陷阱 T3/T7),没有输入就没有下一帧 —— 不主动排一帧,这条 toast 会
    // 一直挂在屏幕上直到用户碰一下鼠标。
    ctx.request_repaint_after(std::time::Duration::from_secs_f64(left));

    egui::Area::new(egui::Id::new("mullion_toast"))
        .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -56.0])
        // 不可交互:它飘在内容之上,可交互会把底下按钮的点击吃掉。
        .interactable(false)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            // F263:宽度预算每帧显式给一份,不继承上一帧(见 `max_width`)。
            ui.set_max_width(max_width(ctx.screen_rect().width()));
            // F213:底色与弹窗同源。原来蹭的是 `sunken_bg`(#0e1018),在
            // 终端底色(#14161f)上只有 1.05:1 —— 一块飘在正文之上、却几乎
            // 看不出边界的浮层。F203 把弹窗从这个坑里捞出来时漏了 toast。
            egui::Frame::none()
                .fill(theme::c32(t.modal_bg))
                .stroke(egui::Stroke::new(1.0, theme::c32(toast.kind.stroke(t))))
                .rounding(8.0)
                .inner_margin(egui::vec2(14.0, 8.0))
                .show(ui, |ui| {
                    ui.colored_label(theme::c32(t.fg), &toast.text);
                });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(
        ctx: &egui::Context,
        time: f64,
        pending: &mut Option<(Kind, String)>,
        live: &mut Option<Toast>,
    ) {
        let t = crate::theme::MULLION_DARK;
        let _ = ctx.run(
            egui::RawInput {
                time: Some(time),
                ..Default::default()
            },
            |ctx| show(ctx, &t, pending, live),
        );
    }

    /// 走查 13:toast 进场时盖的是**帧时间**,过了 TTL 自己消失。
    ///
    /// 自证会变红:把 `remaining` 里的减号改成加号,第三段(超时后该没了)
    /// 立刻炸。
    #[test]
    fn a_toast_stamps_the_frame_clock_and_expires_on_its_own() {
        let ctx = egui::Context::default();
        let mut pending = Some((Kind::Ok, "已保存".to_string()));
        let mut live = None;

        run(&ctx, 10.0, &mut pending, &mut live);
        assert_eq!(
            live.as_ref().map(|x| x.born),
            Some(10.0),
            "进场时间该取当帧的 `input.time`,不是 `Instant::now()`"
        );
        assert!(pending.is_none(), "待发文本必须被消费掉,否则每帧重新进场");

        run(&ctx, 10.0 + TTL - 0.5, &mut pending, &mut live);
        assert!(live.is_some(), "还没到点不该提前消失");

        run(&ctx, 10.0 + TTL + 0.1, &mut pending, &mut live);
        assert!(live.is_none(), "过了 TTL 该自己消失,不用等用户动鼠标");
    }

    /// F263:长文案不许被**上一条**短 toast 的宽度压住。
    ///
    /// 用户实报「toast 总是很竖长」。判据必须是「先短后长」的两帧序列:单独
    /// 画一条长的看不出问题(首帧的预算是 egui 的 `default_area_size`,600 点,
    /// 够宽),病在 `Area` 把上一帧的尺寸留下来当下一帧的预算(见
    /// `max_width` 的文档)。所以这里刻意先飘一条「已保存」,再换成长文案。
    ///
    /// 两条断言各管一头:够宽(棘轮切断了)、不过宽(上限还在)。
    ///
    /// 自证会变红:
    /// - 删掉 `ui.set_max_width(..)` 那行 —— 「够宽」那条红(实测卡在 78 点
    ///   左右,正是「已保存」三个字的宽度);
    /// - 把 `max_width` 的 `MAX_W.min(..)` 换成一个大数(比如 4000)——
    ///   实测「够宽」那条先红(实宽 525,离 4000 差得远);把 `MAX_W` 本身
    ///   调大则由「不过宽」那条接住。
    #[test]
    fn a_long_toast_is_not_squeezed_into_the_width_of_the_short_one_before_it() {
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 900.0));
        let t = crate::theme::MULLION_DARK;
        let mut live = None;
        let frame = |time: f64, pending: &mut Option<(Kind, String)>, live: &mut Option<Toast>| {
            let _ = ctx.run(
                egui::RawInput {
                    time: Some(time),
                    screen_rect: Some(screen),
                    ..Default::default()
                },
                |ctx| show(ctx, &t, pending, live),
            );
            ctx.memory(|m| m.area_rect(egui::Id::new("mullion_toast")))
        };

        // 先飘一条很短的,让 `Area` 把那个窄尺寸记下来。
        let mut short = Some((Kind::Ok, "已保存".to_string()));
        for i in 0..3 {
            let _ = frame(1.0 + i as f64 * 0.1, &mut short, &mut live);
        }
        // 再换成一条长的(同一个 Area id,状态还在)。
        let mut long = Some((
            Kind::Warn,
            "隧道 127.0.0.1:8080 → build-01:80 已停止:连接被拒绝,\
             已退避重连 3 次,下一次在 30 秒后"
                .to_string(),
        ));
        let mut rect = None;
        for i in 0..3 {
            rect = frame(2.0 + i as f64 * 0.1, &mut long, &mut live);
        }
        let rect = rect.expect("toast 没画出来 —— 下面两条断言会空过");

        let want = max_width(screen.width());
        assert!(
            rect.width() >= want - 40.0,
            "长文案被压在 {} 点宽里换行(该有约 {want} 点)—— `Area` 把上一条\
             短 toast 的尺寸当成了这一帧的预算,画出来就是用户报的那根竖条",
            rect.width()
        );
        // 上界钉在 `MAX_W` 这个常量上,不钉 `want` —— `want` 是 `max_width`
        // 自己算的,拿它当上界等于让被测函数给自己出题。
        assert!(
            rect.width() <= MAX_W + 40.0,
            "toast 宽到了 {} 点(上限 {MAX_W})—— 一行横跨整屏,眼睛要横扫",
            rect.width()
        );
    }

    /// 走查 13:toast 活着的时候必须主动排下一帧 —— 事件循环是帧率节流 +
    /// `WaitUntil`(陷阱 T3/T7),没有输入就没有下一帧,不排的话它会一直
    /// 挂在屏幕上;反过来,没有 toast 时**不许**排,那就是白烧 GPU。
    ///
    /// 自证会变红:把 `request_repaint_after` 那行删掉,第一段断言炸。
    #[test]
    fn a_live_toast_schedules_its_own_wakeup_but_an_empty_slot_does_not() {
        let ctx = egui::Context::default();
        let mut pending = Some((Kind::Ok, "已保存".to_string()));
        let mut live = None;
        let t = crate::theme::MULLION_DARK;
        let frame = |ctx: &egui::Context,
                     time: f64,
                     pending: &mut Option<(Kind, String)>,
                     live: &mut Option<Toast>| {
            let out = ctx.run(
                egui::RawInput {
                    time: Some(time),
                    ..Default::default()
                },
                |ctx| show(ctx, &t, pending, live),
            );
            out.viewport_output[&egui::ViewportId::ROOT].repaint_delay
        };

        // **前两帧不能拿来断言**:`egui::Area` 首次遇到一个 id 时要走 sizing
        // pass,egui 自己就会请求「立刻再来一帧」(实测 frame0/1 都是 0ns),
        // 那个 0 会把「我们排没排唤醒」这个问题掩盖掉 —— 这正是第一版这条
        // 测试恒绿的原因(删掉 `request_repaint_after` 它照样通过)。
        // 跑够四帧、且帧间隔要大于 `style.animation_time`(0.083s,egui 的
        // 交互动画在跑的时候同样会请求立即重绘),布局和动画都停下来之后,
        // 拿到的才是我们自己排的那次唤醒。
        let mut delay = std::time::Duration::ZERO;
        for i in 0..4 {
            delay = frame(&ctx, 1.0 + i as f64 * 0.2, &mut pending, &mut live);
        }
        assert!(
            delay > std::time::Duration::ZERO && delay <= std::time::Duration::from_secs_f64(TTL),
            "活着的 toast 必须排一次「到点叫我」的唤醒,实得 {delay:?}"
        );

        // 让它过期消失,再跑两帧:此时槽是空的,谁也不该排唤醒。
        let _ = frame(&ctx, 1.0 + TTL + 1.0, &mut pending, &mut live);
        let delay = frame(&ctx, 1.0 + TTL + 2.0, &mut pending, &mut live);
        assert_eq!(
            delay,
            std::time::Duration::MAX,
            "没有 toast 还排唤醒 = 空转烧 GPU(陷阱 T3)"
        );
    }
}
