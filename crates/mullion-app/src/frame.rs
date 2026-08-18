//! 帧率节流:把重绘频率压到 ≤ 显示器刷新率(N3,守护陷阱 T3)。
//!
//! 「收到数据就重绘」在 tmux 流式输出下每秒数千次 → GPU 空转、风扇起飞。
//! 这里用可注入时钟(调用方传入当前时刻)做节流,便于单测、不依赖真实时间。

/// 帧率节流器:两次提交之间至少间隔 `min_interval_ms`。
#[derive(Debug)]
pub struct FrameLimiter {
    min_interval_ms: u64,
    last_present_ms: Option<u64>,
}

impl FrameLimiter {
    /// `min_interval_ms`:两帧最小间隔。16ms ≈ 60fps。
    pub fn new(min_interval_ms: u64) -> Self {
        Self {
            min_interval_ms,
            last_present_ms: None,
        }
    }

    /// 在时刻 `now_ms` 是否允许提交一帧(距上次 >= 最小间隔;首帧恒允许)。
    pub fn should_present(&self, now_ms: u64) -> bool {
        match self.last_present_ms {
            None => true,
            Some(last) => now_ms.saturating_sub(last) >= self.min_interval_ms,
        }
    }

    /// 记录在 `now_ms` 提交了一帧。
    pub fn record_present(&mut self, now_ms: u64) {
        self.last_present_ms = Some(now_ms);
    }

    /// 给定「是否有脏帧」与当前时刻,决定这次 redraw 该 present / 节流 / 空闲。
    ///
    /// 纯决策:不改变自身状态,不碰 winit `ControlFlow`——调用方据返回值决定
    /// 是否 present、是否安排 `WaitUntil`,从而避免 T3 陈旧 deadline 导致的忙转。
    /// present/throttle 的边界判断复用 [`Self::should_present`],不另起一套等价阈值
    /// 比较,避免以后只改一处导致两者分歧。
    pub fn plan(&self, dirty: bool, now_ms: u64) -> RedrawAction {
        if !dirty {
            return RedrawAction::Idle;
        }
        if self.should_present(now_ms) {
            return RedrawAction::Present;
        }
        // 到这说明有 last_present 且太快(should_present=false 只有此情形会发生,
        // None 分支恒真已在上面提前返回)。
        let last = self
            .last_present_ms
            .expect("should_present=false 时必有 last");
        RedrawAction::Throttle {
            wait_ms: self.min_interval_ms - now_ms.saturating_sub(last),
        }
    }
}

/// 本帧有没有内容要画,即交给 [`FrameLimiter::plan`] 的 `dirty`。
///
/// **两个脏源必须都算上**。`terminal_dirty` 只反映「远端来了新字节」;egui 自己也会
/// 要重绘(菜单展开、hover 高亮、弹窗动画、错误提示)。只看前者的话,终端态下远端
/// 一安静,egui 的交互就全被 [`RedrawAction::Idle`] 吞掉——菜单点不开、弹窗不出现,
/// 而 launcher 态(没有终端字节源)反倒正常,这个不一致极易被误判成 egui 的问题。
pub fn frame_is_dirty(terminal_dirty: bool, ui_dirty: bool) -> bool {
    terminal_dirty || ui_dirty
}

/// 光标闪烁半周期(毫秒)。530ms 是 Windows 的系统默认光标闪烁周期,
/// 与用户在别处(记事本、cmd)看到的节奏一致。
pub const BLINK_HALF_MS: u64 = 530;

/// F125:这一刻光标该不该画出来。
///
/// 相位从**最后一次键盘输入**起算:刚敲完一个字符,光标一定是亮的。不这么做的话
/// 连续打字时光标会随机隐没,观感像丢帧。
///
/// 纯函数,不碰时钟也不碰 winit —— 闪烁是本项目里唯一一个「必须周期性重绘」的
/// 特性,把判据留在能单测的地方,是它不退化成 T3(每帧重绘)的前提。
pub fn blink_visible(now_ms: u64, last_input_ms: u64) -> bool {
    let elapsed = now_ms.saturating_sub(last_input_ms);
    (elapsed / BLINK_HALF_MS).is_multiple_of(2)
}

/// F125:距下一次相位翻转还有多少毫秒。调用方据此排一次 `WaitUntil`
/// (**不是** `request_redraw`:那会绕开帧闸,踩 T3/T7)。
pub fn blink_next_flip_ms(now_ms: u64, last_input_ms: u64) -> u64 {
    let elapsed = now_ms.saturating_sub(last_input_ms);
    BLINK_HALF_MS - (elapsed % BLINK_HALF_MS)
}

/// [`FrameLimiter::plan`] 的决策结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedrawAction {
    /// 立即 present。
    Present,
    /// 有脏帧但太快,等 `wait_ms` 后再画(调用方据此设 `ControlFlow::WaitUntil`)。
    Throttle { wait_ms: u64 },
    /// 无脏帧,等下一个事件(调用方据此设 `ControlFlow::Wait`)。
    Idle,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn egui_repaint_alone_is_dirty_enough() {
        // 终端态远端安静时,egui 的重绘请求必须能自己撑起一帧,否则菜单/弹窗点不开。
        assert!(frame_is_dirty(false, true), "egui 要重绘就得画");
        assert!(frame_is_dirty(true, false), "终端来了字节就得画");
        assert!(!frame_is_dirty(false, false), "两边都不脏才算空闲");
    }

    #[test]
    fn not_dirty_is_idle() {
        let limiter = FrameLimiter::new(16);
        assert_eq!(limiter.plan(false, 0), RedrawAction::Idle, "无脏帧应 Idle");
    }

    #[test]
    fn dirty_first_frame_presents() {
        let limiter = FrameLimiter::new(16);
        assert_eq!(
            limiter.plan(true, 0),
            RedrawAction::Present,
            "从未 present 过时首帧应立即 Present"
        );
    }

    #[test]
    fn dirty_after_interval_presents() {
        let mut limiter = FrameLimiter::new(16);
        limiter.record_present(0);
        assert_eq!(
            limiter.plan(true, 16),
            RedrawAction::Present,
            "距上次已满 min_interval_ms 应 Present"
        );
    }

    #[test]
    fn dirty_too_soon_throttles_remaining_wait() {
        let mut limiter = FrameLimiter::new(16);
        limiter.record_present(0);
        assert_eq!(
            limiter.plan(true, 8),
            RedrawAction::Throttle { wait_ms: 8 },
            "太快的脏帧应 Throttle,且 wait_ms = 剩余间隔,不是忙等空 request_redraw"
        );
    }

    /// F125:半周期 530ms(Windows 系统默认光标闪烁周期)。
    /// 相位从**最后一次输入**起算,不是从进程启动起算。
    #[test]
    fn blink_alternates_every_half_period() {
        assert!(blink_visible(0, 0), "刚输入完必须是亮的");
        assert!(blink_visible(529, 0), "半周期内一直亮");
        assert!(!blink_visible(530, 0), "过半周期转灭");
        assert!(!blink_visible(1059, 0));
        assert!(blink_visible(1060, 0), "一个整周期后回到亮");
    }

    /// F125:打字必须重置相位 —— 不重置的话连续打字时光标会随机隐没,
    /// 观感像丢帧。判据:同一时刻,`last_input` 变了结果就该跟着变。
    ///
    /// 自证会变红:把 `blink_visible` 里的 `now_ms - last_input_ms` 换成 `now_ms`。
    #[test]
    fn typing_resets_the_phase() {
        assert!(!blink_visible(600, 0), "距上次输入 600ms:灭");
        assert!(blink_visible(600, 600), "这一刻刚敲了键:立刻回到亮");
    }

    /// 时钟倒退(不同来源的 now)不能 panic —— 用饱和减法。
    #[test]
    fn clock_going_backwards_is_not_a_panic() {
        assert!(blink_visible(0, 999));
    }

    /// F125:下一次相位翻转的时刻,给事件循环排 `WaitUntil` 用。
    /// **不返回固定的 530**:刚翻转过的那一帧要等满 530,翻转前 10ms 就只等 10ms,
    /// 否则光标会晚一整拍。
    ///
    /// 自证会变红:把 `blink_next_flip_ms` 的函数体改成 `BLINK_HALF_MS`。
    #[test]
    fn next_flip_is_the_remainder_of_the_current_half_period() {
        assert_eq!(blink_next_flip_ms(0, 0), 530);
        assert_eq!(blink_next_flip_ms(520, 0), 10);
        assert_eq!(blink_next_flip_ms(530, 0), 530);
        assert_eq!(blink_next_flip_ms(1000, 0), 60);
    }
}
