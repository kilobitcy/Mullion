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
}
