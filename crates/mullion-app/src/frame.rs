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
}
