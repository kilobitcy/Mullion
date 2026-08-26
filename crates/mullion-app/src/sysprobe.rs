//! 进程级资源探针:CPU 时间、GPU 引擎占用率、显存。
//!
//! **为什么不在 `diag.rs` 里**:那个文件已经 730 行,再塞三套平台 FFI
//! 会失控。这里的分工是「平台相关的采集」+「平台无关的换算」,换算部分
//! 是纯函数、能单测,FFI 只留薄壳。
//!
//! **调用方只有看门狗线程**(`diag::watchdog_loop`,每 5 秒一次),
//! 所以这里的一切都不在帧路径上,可以放心做系统调用。
//!
//! 非 Windows / 探针不可用 / 首次采样无基线 —— 一律返回 `None`,
//! 由 `profile::render_line` 渲染成 `n/a`。**不许编一个 0 出来**:
//! 「采不到」和「真的是 0」在排障时是两回事。

/// 一次 CPU 采样。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuSample {
    /// 整个进程的 CPU 占用,**按核数归一**(所有核跑满 = 100)。
    pub process_pct: u8,
    /// 主线程的 CPU 占用,**不归一**(一个核跑满 = 100)。
    pub main_thread_pct: u8,
}

/// CPU 时间差 → 百分比。
///
/// `cores` 是归一化的除数:进程口径传真实核数,主线程口径传 1。
///
/// **两个口径故意不同**。F158 那次故障的症状原文是「空闲不再烧满一个核」,
/// 在 16 核机器上按核数归一之后它只有 6% —— 淹没在噪声里,而这个功能存在
/// 的全部理由就是让它跳出来。主线程不归一,一个核跑满就是 100%。
///
/// `window_ns` 为 0(时钟没走 / 首次采样无基线)返回 `None`,不是 0 ——
/// 「采不到」和「真的是 0」在排障时是两回事,而且 `None` 不会打破空闲门。
pub fn cpu_pct(delta_ns: u64, window_ns: u64, cores: u32) -> Option<u8> {
    if window_ns == 0 || cores == 0 {
        return None;
    }
    let denom = (window_ns as u128) * (cores as u128);
    let pct = (delta_ns as u128) * 100 / denom;
    Some(pct.min(100) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 进程口径按核数归一,主线程口径不归一。
    ///
    /// 这是本模块唯一一条「写错了也全绿、只有真机看得出」的判据:
    /// 两个口径混用的话,「烧满一个核」在多核机上会被压成个位数百分比。
    ///
    /// 自证会变红:把 `cpu_pct` 里的 `* (cores as u128)` 去掉。
    #[test]
    fn the_process_is_normalised_by_cores_while_the_main_thread_is_not() {
        // 一个核被跑满一整个窗口。
        let window = 5_000_000_000u64; // 5s
        let one_core = 5_000_000_000u64;
        assert_eq!(
            cpu_pct(one_core, window, 16),
            Some(6),
            "16 核机上跑满一个核 ≈ 6%(进程口径)"
        );
        assert_eq!(
            cpu_pct(one_core, window, 1),
            Some(100),
            "主线程口径下跑满一个核就是 100%"
        );
    }

    /// 超出 100 要夹紧,不能溢出成小数字。
    ///
    /// `GetProcessTimes` 在多核上很容易给出 > window 的累计值(多线程并行),
    /// 不夹紧的话 u8 转换会回绕 —— 200% 变成一个看起来正常的数。
    ///
    /// 自证会变红:把 `.min(100)` 删掉。
    #[test]
    fn a_multi_core_burst_is_clamped_instead_of_wrapping() {
        assert_eq!(cpu_pct(40_000_000_000, 5_000_000_000, 1), Some(100));
    }

    /// 采不到时是 `None` 而不是 0。
    ///
    /// 0 会被空闲门读成「真空闲」,而 `None` 不打破空闲门也不冒充数据。
    ///
    /// 自证会变红:把 `cpu_pct` 的两处 `return None` 改成 `return Some(0)`。
    #[test]
    fn an_unusable_window_yields_nothing_rather_than_a_fake_zero() {
        assert_eq!(cpu_pct(1_000, 0, 4), None);
        assert_eq!(cpu_pct(1_000, 5_000_000_000, 0), None);
    }
}
