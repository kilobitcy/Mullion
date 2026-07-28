//! 窗口可见性(最小化判定)+「这一帧该做到哪一步」的纯决策。零 winit/wgpu,可单测。
//!
//! 为什么需要:Windows 最小化时会送一次 `Resized(0,0)`。若照单全收:
//!
//! 1. `grid_size_for(0,0)` 钳成 1×1 → `emulator.resize(1,1)`。tmux 处于 alt screen 时
//!    alacritty 仍会对带 scrollback 的 primary grid 启用 reflow,按 1 列重排后再
//!    `truncate(max_scroll_limit + lines)`——**终端历史被永久碾平,还原也回不来**。
//! 2. `ssh.resize(1,1)` 把 `window_change 1×1` 发给远端,tmux / Claude Code TUI 按
//!    1 列重排版(T4 的反面用法)。
//! 3. 继续对 0 面积表面 configure / acquire / present。
//!
//! 正确做法是整帧跳过,**但 IO 泵必须照跑**(见 [`RedrawScope::PumpOnly`])。

/// 窗口当前能否呈现。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Visibility {
    #[default]
    Visible,
    /// 最小化(或表面面积为 0):不可呈现。
    Minimized,
}

/// 由窗口物理像素尺寸判定可见性。任一维为 0 即视为最小化。
pub fn visibility_for(width: u32, height: u32) -> Visibility {
    if width == 0 || height == 0 {
        Visibility::Minimized
    } else {
        Visibility::Visible
    }
}

/// 收到「窗口本该看得见」的信号(重回前台 / 取消遮挡 / 收到重绘请求)时,拿实测的
/// `window.inner_size()` 判断是否该从 [`Visibility::Minimized`] 自愈。
/// 返回 `Some(新状态)` 表示要切换,`None` 表示保持不变。
///
/// **为什么不能只信 `Resized`**:进 Minimized 只需要一次 `Resized(0,0)`,而出来却
/// 完全指望对方补发一次非零 `Resized`。0×0 不总是「用户最小化了」——驱动/合成器抖动、
/// 显卡崩溃重启外壳(v0.1.4 真机日志最后一行正是 `Resized(0x0)`)也会送这一条,那些
/// 情况下不会有还原事件。缺了这道自愈,窗口就永久停在「只泵 IO 不绘制」:字节照收照发,
/// 屏幕再也不更新,用户看到的是「键盘没有任何反应」。
pub fn recover_from_minimized(current: Visibility, width: u32, height: u32) -> Option<Visibility> {
    if !matches!(current, Visibility::Minimized) {
        return None;
    }
    match visibility_for(width, height) {
        Visibility::Visible => Some(Visibility::Visible),
        Visibility::Minimized => None, // 真的还是 0×0,老老实实继续只泵 IO
    }
}

/// 一次 `Resized` 该产生哪些副作用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResizePlan {
    /// 是否重新 configure wgpu 表面。
    pub reconfigure_surface: bool,
    /// 是否把新列/行数推给 emulator 与远端(`window_change`,T4)。
    pub propagate_grid: bool,
    /// 是否请求一次重绘。
    pub request_redraw: bool,
}

/// 最小化时三件事全不做,只记状态;还原时按真实尺寸全量恢复。
pub fn plan_resize(vis: Visibility) -> ResizePlan {
    let visible = matches!(vis, Visibility::Visible);
    ResizePlan {
        reconfigure_surface: visible,
        propagate_grid: visible,
        request_redraw: visible,
    }
}

/// 一次 `RedrawRequested` 该做到哪一步。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedrawScope {
    /// 整帧:IO 泵 + egui + GPU present。
    Full,
    /// 只跑 IO 泵(排空 rx → feed emulator → 回写 `PtyWrite`),不碰 GPU。
    ///
    /// **绝不能连泵一起停**:rx 是有界通道(256),灌满后 io_task 阻塞;而且远端的
    /// 同步输出探测 / 光标位置查询等不到应答会永久卡死(T1 红线)。最小化只该省掉
    /// 渲染,不该切断与远端的对话。
    PumpOnly,
}

pub fn redraw_scope(vis: Visibility) -> RedrawScope {
    match vis {
        Visibility::Visible => RedrawScope::Full,
        Visibility::Minimized => RedrawScope::PumpOnly,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_size_is_minimized() {
        // Windows 最小化送的就是这一条。
        assert_eq!(visibility_for(0, 0), Visibility::Minimized);
        assert_eq!(visibility_for(800, 0), Visibility::Minimized);
        assert_eq!(visibility_for(0, 600), Visibility::Minimized);
        assert_eq!(visibility_for(800, 600), Visibility::Visible);
    }

    #[test]
    fn minimized_resize_touches_neither_gpu_nor_remote() {
        // 最小化不得 configure 表面、不得 emulator.resize(否则 scrollback 被 1 列
        // reflow 碾平)、不得发 window_change 1×1(否则远端 tmux 按 1 列排版)。
        let plan = plan_resize(visibility_for(0, 0));
        assert!(!plan.reconfigure_surface);
        assert!(!plan.propagate_grid);
        assert!(!plan.request_redraw);
    }

    #[test]
    fn restore_reconfigures_and_propagates() {
        let plan = plan_resize(visibility_for(1920, 1080));
        assert!(plan.reconfigure_surface);
        assert!(plan.propagate_grid);
        assert!(plan.request_redraw);
    }

    #[test]
    fn minimized_recovers_when_real_size_is_nonzero() {
        // 没有这条自愈:一次异常的 Resized(0,0)(驱动抖动/外壳重启)就能让窗口永久
        // 停在 PumpOnly,屏幕再不更新 —— 表现为「键盘没有任何反应」。
        assert_eq!(
            recover_from_minimized(Visibility::Minimized, 1920, 1080),
            Some(Visibility::Visible)
        );
    }

    #[test]
    fn recovery_does_not_fire_when_truly_minimized_or_already_visible() {
        // 真最小化时尺寸仍是 0×0,不能自愈回去,否则又开始按 0 列碾平 scrollback。
        assert_eq!(recover_from_minimized(Visibility::Minimized, 0, 0), None);
        // 已经可见就别多此一举地重走恢复路径。
        assert_eq!(
            recover_from_minimized(Visibility::Visible, 1920, 1080),
            None
        );
    }

    #[test]
    fn minimized_still_pumps_io() {
        // T1 红线:最小化只省渲染,IO 泵必须继续,否则有界 rx 灌满 + 远端探测无应答。
        assert_eq!(redraw_scope(Visibility::Minimized), RedrawScope::PumpOnly);
        assert_eq!(redraw_scope(Visibility::Visible), RedrawScope::Full);
    }
}
