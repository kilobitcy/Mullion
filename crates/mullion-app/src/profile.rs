//! 性能剖面的**数据结构与纯函数**（F155）：对数桶直方图 + 分位数 + 剖面行渲染。
//!
//! **零 IO、零 UI、零 async**，可纯单测。采集端（`diag.rs`）只往这里做原子加法，
//! 格式化只发生在 5 秒周期线程里 —— 帧路径上做格式化就是 T3。
//!
//! 为什么是对数桶而不是存原始样本：一秒 60 帧、跑一整天是 500 万个样本，
//! 存下来算精确分位数要几十 MB 内存和一次排序。对数桶用 24 个 `AtomicU64`
//! （192 字节）换「相对误差不超过 2 倍」的分位数 —— 而找性能瓶颈要的是
//! 「p95 是 3ms 还是 300ms」，不是「是 3.1ms 还是 3.2ms」。

use std::sync::atomic::{AtomicU64, Ordering};

/// 桶数。桶 0 = 0µs；桶 k(k≥1) 覆盖 [2^(k-1), 2^k) µs。
/// 桶 23 = [4.2s, ∞)，比看门狗的停滞阈值（3s）还大一档，够用。
pub const BUCKETS: usize = 24;

/// 一个样本落进哪个桶。
///
/// 纯函数：桶边界是要被分位数换算反过来用的，两处各写一遍必然漂。
pub fn bucket_of(us: u64) -> usize {
    if us == 0 {
        return 0;
    }
    // 1 → 1, 2..3 → 2, 4..7 → 3 ...
    let b = 64 - us.leading_zeros() as usize;
    b.min(BUCKETS - 1)
}

/// 桶 `k` 的上界（µs）。分位数报的是这个 —— **报上界而不是下界**：
/// 「p95 不超过 X」是个能直接拿去做判断的说法，「p95 至少是 X」不是。
pub fn bucket_upper_us(k: usize) -> u64 {
    if k == 0 {
        0
    } else {
        1u64 << k.min(BUCKETS - 1)
    }
}

/// 一份取下来的计数快照。
pub type Counts = [u64; BUCKETS];

/// 分位数（µs）。`q` 取 0.0..=1.0。空直方图返回 0。
///
/// 用「向上取整的目标序号」而不是线性插值：桶本身就有 2 倍误差，
/// 在桶内插值是给精度加戏。
pub fn quantile_us(counts: &Counts, q: f64) -> u64 {
    let total: u64 = counts.iter().sum();
    if total == 0 {
        return 0;
    }
    let target = ((total as f64) * q).ceil().max(1.0) as u64;
    let mut acc = 0u64;
    for (k, c) in counts.iter().enumerate() {
        acc += c;
        if acc >= target {
            return bucket_upper_us(k);
        }
    }
    bucket_upper_us(BUCKETS - 1)
}

/// 直方图里一共记了多少个样本。
pub fn total(counts: &Counts) -> u64 {
    counts.iter().sum()
}

/// 可并发写入的直方图。写入端只做一次原子加法。
#[derive(Debug)]
pub struct Histogram {
    counts: [AtomicU64; BUCKETS],
}

impl Default for Histogram {
    fn default() -> Self {
        Self::new()
    }
}

impl Histogram {
    /// `const fn`：要能直接写成 `static`。
    pub const fn new() -> Self {
        // 内联 const（Rust 1.79+ 稳定）：数组的每个元素各自求值一次。
        // 不用 `const ZERO: AtomicU64 = ..;` 那种写法 —— 具名的内部可变常量
        // 会被 `clippy::declare_interior_mutable_const` 拦下，而那条 lint 是对的：
        // 具名 const 每次使用都会复制一份，很容易写出「以为在改同一个原子、
        // 其实各改各的副本」的 bug。内联 const 没有这个歧义。
        Self {
            counts: [const { AtomicU64::new(0) }; BUCKETS],
        }
    }

    /// 记一个样本。**只有一条 relaxed 原子加**，可以放在帧路径上。
    pub fn record_us(&self, us: u64) {
        self.counts[bucket_of(us)].fetch_add(1, Ordering::Relaxed);
    }

    /// 取走并清零。周期线程每 5 秒调一次 —— 剖面报的是**这 5 秒的窗口**，
    /// 不是自启动以来的累计：累计值会被开头那几秒的启动尖峰永久污染，
    /// 跑了一小时之后 p95 反映的是一小时前的事。
    ///
    /// 逐桶 swap 不是一次原子快照：中间可能插进来一次 `record_us`，
    /// 那个样本会落到下一个窗口。对统计无影响，不值得为它上锁。
    pub fn drain(&self) -> Counts {
        let mut out = [0u64; BUCKETS];
        for (k, c) in self.counts.iter().enumerate() {
            out[k] = c.swap(0, Ordering::Relaxed);
        }
        out
    }
}

/// µs → 便于阅读的字符串。剖面行里到处要用，统一在这里。
///
/// 1000µs 以下报 µs，以上报 ms（一位小数）—— 一行里混着两种量纲的裸数字
/// 是看不出问题的主要原因。
pub fn fmt_us(us: u64) -> String {
    if us < 1000 {
        format!("{us}us")
    } else {
        format!("{:.1}ms", us as f64 / 1000.0)
    }
}

/// 一个 5 秒窗口里采到的全部东西。
///
/// **纯数据**：由 `diag.rs` 的周期线程从各个原子计数器 drain 出来填好，
/// 再交给 [`render_line`]。分成两步是为了让「这一行长什么样」可以脱离
/// 线程、时钟和全局状态单测 —— 剖面行本身是要被人读的产物，格式错了
/// （单位漏了、零值省略了、跨行了）编译器一句话都不会说。
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// 这个窗口有多长（ms）。周期线程受调度影响不会正好 5000。
    pub window_ms: u64,
    pub frames: u64,
    pub presents: u64,
    pub skipped: u64,
    pub inbound_bytes: u64,
    /// 重绘触发原因：只有终端来了字节 / 只有 egui 要重绘 / 两者都有。
    pub redraw_terminal: u64,
    pub redraw_ui: u64,
    pub redraw_both: u64,
    /// 被帧闸挡下的重绘次数（T3 的直接体感指标）。
    pub throttled: u64,
    /// 按键次数。
    pub keys: u64,
    /// 「按键 → 下一段入站字节抵达」的间隔分布。**是回显往返的近似**，
    /// 见 `diag::note_key` 的说明。
    pub echo_us: Counts,
    /// 整帧耗时分布。
    pub frame_us: Counts,
    /// 逐阶段驻留时长分布，索引 = `diag::Stage as usize`。
    pub stage_us: StageCounts,
    pub connects_ok: u64,
    pub connects_err: u64,
    pub reconnects: u64,
    pub sftp_ops: u64,
    pub tabs: u64,
    pub panes: u64,
    pub hosts: u64,
    pub mem_process_mb: u64,
}

/// 逐阶段计数。长度与 `diag::Stage` 的变体数一致。
pub type StageCounts = [Counts; crate::diag::STAGE_COUNT];

impl Snapshot {
    /// 一份全零的快照。
    pub fn empty() -> Self {
        Self {
            window_ms: 0,
            frames: 0,
            presents: 0,
            skipped: 0,
            inbound_bytes: 0,
            redraw_terminal: 0,
            redraw_ui: 0,
            redraw_both: 0,
            throttled: 0,
            keys: 0,
            echo_us: [0; BUCKETS],
            frame_us: [0; BUCKETS],
            stage_us: [[0; BUCKETS]; crate::diag::STAGE_COUNT],
            connects_ok: 0,
            connects_err: 0,
            reconnects: 0,
            sftp_ops: 0,
            tabs: 0,
            panes: 0,
            hosts: 0,
            mem_process_mb: 0,
        }
    }

    /// 这个窗口里什么都没发生。
    ///
    /// 判据只看「有没有画过帧 / 有没有收过字节 / 有没有按过键 / 有没有连接
    /// 或 SFTP 动作」—— 标签数、内存这类**状态量**在空闲时也非零，拿它们
    /// 判活会让空闲的 mullion 每 5 秒写一次盘，笔记本硬盘永远睡不下去。
    pub fn is_idle(&self) -> bool {
        self.frames == 0
            && self.inbound_bytes == 0
            && self.keys == 0
            && self.connects_ok == 0
            && self.connects_err == 0
            && self.reconnects == 0
            && self.sftp_ops == 0
    }
}

/// 把一个窗口渲染成**一行**日志。`None` = 这个窗口空闲，不该写。
///
/// 单行是硬要求：日志按行 grep，一条记录跨行就没法用
/// `grep profile mullion.log` 拉出时间序列。
pub fn render_line(s: &Snapshot) -> Option<String> {
    if s.is_idle() {
        return None;
    }
    let secs = (s.window_ms as f64 / 1000.0).max(0.001);
    let bps = s.inbound_bytes as f64 / secs;
    let rate = if bps >= 1024.0 {
        format!("{:.1}KB/s", bps / 1024.0)
    } else {
        format!("{bps:.0}B/s")
    };
    // 阶段按「样本数 × p95」倒序取前四段 —— 全列出来一行有十二段，人眼
    // 扫不动，而排在后面的那些恒定是零。
    let mut stages: Vec<(usize, u64, u64)> = s
        .stage_us
        .iter()
        .enumerate()
        .map(|(k, c)| (k, total(c), quantile_us(c, 0.95)))
        .filter(|(_, n, _)| *n > 0)
        .collect();
    stages.sort_by_key(|(_, n, p95)| std::cmp::Reverse(n.saturating_mul(*p95)));
    stages.truncate(4);
    let stage_part = stages
        .iter()
        .map(|(k, n, p95)| {
            format!(
                "{}={}x/p95={}",
                crate::diag::stage_name(*k as u8),
                n,
                fmt_us(*p95)
            )
        })
        .collect::<Vec<_>>()
        .join(" ");

    Some(format!(
        "profile {:.1}s frame={}x/p50={}/p95={}/max={} present={} skip={} throttle={} \
         redraw=term:{}/ui:{}/both:{} in={} key={}x/echo_p95={} {} \
         conn=ok:{}/err:{}/re:{} sftp={} tabs={} panes={} hosts={} mem={}MB",
        secs,
        s.frames,
        fmt_us(quantile_us(&s.frame_us, 0.5)),
        fmt_us(quantile_us(&s.frame_us, 0.95)),
        fmt_us(quantile_us(&s.frame_us, 1.0)),
        s.presents,
        s.skipped,
        s.throttled,
        s.redraw_terminal,
        s.redraw_ui,
        s.redraw_both,
        rate,
        s.keys,
        fmt_us(quantile_us(&s.echo_us, 0.95)),
        stage_part,
        s.connects_ok,
        s.connects_err,
        s.reconnects,
        s.sftp_ops,
        s.tabs,
        s.panes,
        s.hosts,
        s.mem_process_mb,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 桶边界：每个桶覆盖上一个桶的两倍。钉死它是因为 `bucket_upper_us`
    /// 要按同一套边界反算回去，两边漂了分位数就是错的。
    #[test]
    fn buckets_double_and_the_upper_bound_agrees_with_them() {
        assert_eq!(bucket_of(0), 0);
        assert_eq!(bucket_of(1), 1);
        assert_eq!(bucket_of(2), 2);
        assert_eq!(bucket_of(3), 2);
        assert_eq!(bucket_of(4), 3);
        assert_eq!(bucket_of(7), 3);
        assert_eq!(bucket_of(8), 4);
        // 每个样本都必须落在自己桶的上界之内 —— 这条是 `quantile_us`
        // 「p95 不超过 X」这个说法能成立的全部依据。
        for us in [1u64, 2, 3, 5, 17, 999, 1000, 16_000, 999_999] {
            let k = bucket_of(us);
            assert!(
                us <= bucket_upper_us(k),
                "{us}µs 落进桶 {k}，但那个桶的上界是 {}",
                bucket_upper_us(k)
            );
        }
    }

    /// 极大值不许越界 panic —— 采样点里有 `Instant` 差值，时钟异常时会出现
    /// 荒谬的大数，而剖面绝不能反过来把程序打死。
    #[test]
    fn an_absurdly_large_sample_saturates_into_the_last_bucket() {
        assert_eq!(bucket_of(u64::MAX), BUCKETS - 1);
        assert_eq!(bucket_of(1 << 40), BUCKETS - 1);
    }

    #[test]
    fn an_empty_histogram_reports_zero_rather_than_panicking() {
        let h = Histogram::new();
        let c = h.drain();
        assert_eq!(total(&c), 0);
        assert_eq!(quantile_us(&c, 0.5), 0);
        assert_eq!(quantile_us(&c, 0.95), 0);
    }

    /// 分位数的核心：绝大多数样本很快、偶尔卡一下时，p50 要还在微秒档，
    /// 而那个慢样本必须在尾部露头 —— 这正是本切片要回答的问题形态。
    /// 只报平均值的话，那个 1 秒会被摊成 10ms，看起来毫无异常。
    ///
    /// **100 个样本里唯一的那个慢样本不出现在 p95 上,这不是 bug**:按
    /// nearest-rank 定义,「95% 的样本不超过 X」在 100 个样本里只需数到第
    /// 95 名,那一名确实是快的。孤例靠 max 兜住 —— 剖面行同时报 p50/p95/max
    /// 就是为了这个,少报 max 的话「一小时里卡了那么一下」会彻底消失。
    #[test]
    fn a_lone_slow_sample_hides_from_p95_but_never_from_max() {
        let h = Histogram::new();
        for _ in 0..99 {
            h.record_us(1);
        }
        h.record_us(1_000_000);
        let c = h.drain();
        assert_eq!(total(&c), 100);
        assert!(quantile_us(&c, 0.5) <= 2, "p50 该还在微秒档");
        assert!(
            quantile_us(&c, 0.95) <= 2,
            "百里挑一的慢样本按定义不该抬高 p95"
        );
        assert!(
            quantile_us(&c, 1.0) >= 1_000_000,
            "max 没抓到那个一秒的样本 —— 孤例卡顿就是这么在剖面里消失的"
        );
    }

    /// 目标序号**向上取整**(nearest-rank):10 个样本里有 1 个慢的,p95 的
    /// 序号是 `ceil(0.95 * 10) = 10`,必须把那个慢样本算进来。
    ///
    /// 向下取整会得到 9,正好把它排除在外 —— 于是「十次里有一次很慢」
    /// 在剖面里完全看不见,而那恰恰是高延迟链路上最该被看见的形态。
    ///
    /// 自证会变红:把 `quantile_us` 里的 `.ceil()` 改成 `.floor()`。
    #[test]
    fn the_rank_rounds_up_so_a_tenth_of_the_samples_cannot_hide_from_p95() {
        let h = Histogram::new();
        for _ in 0..9 {
            h.record_us(1);
        }
        h.record_us(1_000_000);
        let c = h.drain();
        assert_eq!(total(&c), 10);
        assert!(
            quantile_us(&c, 0.95) >= 1_000_000,
            "序号向下取整了 —— 十分之一的慢样本被排除在 p95 之外"
        );
    }

    /// `drain` 是**取走**，不是读取。不清零的话剖面报的是自启动以来的累计，
    /// 跑一小时之后 p95 反映的还是启动那几秒的尖峰。
    ///
    /// 自证会变红：把 `drain` 里的 `swap(0, ..)` 改成 `load(..)`。
    #[test]
    fn draining_resets_the_window_so_old_spikes_do_not_haunt_it() {
        let h = Histogram::new();
        h.record_us(500_000);
        assert_eq!(total(&h.drain()), 1);
        assert_eq!(total(&h.drain()), 0, "取走之后窗口该是空的");
        h.record_us(1);
        let c = h.drain();
        assert_eq!(total(&c), 1);
        assert!(quantile_us(&c, 1.0) <= 2, "上个窗口的尖峰漏到这个窗口来了");
    }

    /// 一行里混着两种量纲的裸数字是看不出问题的主要原因，所以带单位。
    #[test]
    fn durations_carry_their_unit() {
        assert_eq!(fmt_us(0), "0us");
        assert_eq!(fmt_us(999), "999us");
        assert_eq!(fmt_us(1_000), "1.0ms");
        assert_eq!(fmt_us(16_500), "16.5ms");
    }

    fn busy_snapshot() -> Snapshot {
        let mut s = Snapshot {
            window_ms: 5_000,
            frames: 300,
            presents: 298,
            skipped: 2,
            inbound_bytes: 1_024_000,
            redraw_terminal: 200,
            redraw_ui: 80,
            redraw_both: 20,
            throttled: 40,
            keys: 12,
            connects_ok: 1,
            sftp_ops: 3,
            tabs: 2,
            panes: 3,
            hosts: 2,
            mem_process_mb: 180,
            ..Snapshot::empty()
        };
        s.frame_us[bucket_of(8_000)] = 300;
        s
    }

    /// 空窗口（一帧没画、一个字节没收）**不该产出一行**。
    ///
    /// 没有这条，笔记本合盖前挂着的 mullion 会每 5 秒写一次盘，硬盘永远
    /// 睡不下去 —— 这正是布局落盘已经踩过一次的坑。
    ///
    /// 自证会变红：把 `Snapshot::is_idle` 的函数体改成 `false`。
    #[test]
    fn an_idle_window_produces_no_line_at_all() {
        let idle = Snapshot {
            window_ms: 5_000,
            ..Snapshot::empty()
        };
        assert!(idle.is_idle());
        assert!(render_line(&idle).is_none(), "空闲窗口不该写盘");
        assert!(!busy_snapshot().is_idle());
        assert!(render_line(&busy_snapshot()).is_some());
    }

    /// 「空闲」只看这个窗口里**发生过什么**，不看**此刻是什么状态**。
    ///
    /// 拿标签数/内存这类状态量判活的话，只要开着一个标签就永远不算空闲，
    /// 上一条测试守的那件事会被静默绕过。
    ///
    /// 自证会变红：在 `is_idle` 的条件里加上 `&& self.tabs == 0`。
    #[test]
    fn having_tabs_open_is_not_activity() {
        let parked = Snapshot {
            window_ms: 5_000,
            tabs: 3,
            panes: 5,
            hosts: 2,
            mem_process_mb: 200,
            ..Snapshot::empty()
        };
        assert!(parked.is_idle(), "挂着不动也被算成了活动 —— 硬盘会永远醒着");
    }

    /// 一行里必须同时有「多快」和「慢在哪一段」。只报总帧耗时的话，
    /// 看到 p95=200ms 也说不出该去优化 pump 还是 present。
    #[test]
    fn the_line_carries_both_the_frame_time_and_the_per_stage_breakdown() {
        let mut s = busy_snapshot();
        s.stage_us[crate::diag::Stage::Pump as usize][bucket_of(50_000)] = 100;
        let line = render_line(&s).expect("忙窗口该有一行");
        assert!(line.contains("frame"), "没报帧耗时：{line}");
        assert!(line.contains("p95"), "没报分位数：{line}");
        assert!(line.contains("pump="), "没报阶段耗时：{line}");
    }

    /// 剖面行是给人扫的，一行里的数字必须带单位与量纲；而且**不能换行**
    /// —— 日志按行 grep，一条记录跨行就没法用 `grep profile` 拉出时间序列。
    #[test]
    fn the_line_is_single_line_and_units_are_explicit() {
        let line = render_line(&busy_snapshot()).expect("忙窗口该有一行");
        assert!(!line.contains('\n'), "剖面行不许换行：{line}");
        assert!(
            line.contains("KB/s") || line.contains("B/s"),
            "吞吐没带单位：{line}"
        );
    }

    /// 跳帧数为 0 时也要**显式写出来**。省略掉的话，「这个窗口没跳帧」与
    /// 「这个版本忘了统计跳帧」在日志里长得一模一样。
    #[test]
    fn a_zero_count_is_printed_rather_than_omitted() {
        let mut s = busy_snapshot();
        s.skipped = 0;
        let line = render_line(&s).expect("忙窗口该有一行");
        assert!(line.contains("skip=0"), "零值被省略了：{line}");
    }

    /// 吞吐是**速率**，不是这个窗口的字节总数：窗口被调度拖长时，同样的
    /// 字节数应该报出更小的速率。不除以时长的话，「远端安静了但线程被
    /// 挂了 10 秒」会显示成吞吐翻倍。
    ///
    /// 自证会变红：把 `render_line` 里的 `s.inbound_bytes as f64 / secs`
    /// 改成 `s.inbound_bytes as f64`。
    #[test]
    fn throughput_is_a_rate_so_a_longer_window_reports_less() {
        let short = busy_snapshot();
        let long = Snapshot {
            window_ms: 20_000,
            ..busy_snapshot()
        };
        let a = render_line(&short).expect("有一行");
        let b = render_line(&long).expect("有一行");
        assert!(a.contains("200.0KB/s"), "5 秒窗口的速率不对：{a}");
        assert!(b.contains("50.0KB/s"), "20 秒窗口没按时长摊开：{b}");
    }
}
