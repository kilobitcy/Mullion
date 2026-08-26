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

/// `repaint_delay` 落在哪个桶（F157）。
///
/// 三分而不是二分:`Duration::MAX` 与「很大的有限值」**必须分开**。前者是
/// 「egui 不需要重绘」,后者是「egui 要重绘,只是可以等」——归成一类的话,
/// 剖面里「空闲时 egui 一次都没要过重绘」和「空闲时 egui 每帧都要」长得一样,
/// 而分辨这两者正是 F157 存在的全部理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepaintBucket {
    /// `Duration::ZERO` —— egui 要求**立刻**再来一帧。空闲时出现这一档,
    /// 说明 `InputState::wants_repaint_after` 的第一条判据成立
    /// (有未处理的指针动作 / 滚动增量 / 输入事件)。
    Zero,
    /// 有限非零 —— 动画、tooltip 延时之类,可以等。
    Finite,
    /// `Duration::MAX` —— egui 不需要重绘。**真空闲时本该恒是这一档**。
    Max,
}

/// 把一个 `repaint_delay` 归进 [`RepaintBucket`]。纯函数,可单测。
pub fn repaint_bucket(d: std::time::Duration) -> RepaintBucket {
    // `MAX` 先判:它不是零,所以顺序对结果无影响,但先写它是为了让
    // 「MAX 是独立一档」这件事在代码里一眼可见。
    if d == std::time::Duration::MAX {
        RepaintBucket::Max
    } else if d.is_zero() {
        RepaintBucket::Zero
    } else {
        RepaintBucket::Finite
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
    /// T2(领域陷阱):各 pane 攒帧状态机进入过多少次同步块。
    pub sync_blocks: u64,
    /// T2:其中有多少个块是靠 150ms 逃生门硬挤出来的(对端没发完 ESU)。
    /// 历史上「打字慢一拍」的真根因就是这里。
    pub sync_timeouts: u64,
    /// F12:本窗口内整形缓存命中的行数。
    pub reshape_hit: u64,
    /// F12:未命中(真的跑了一次 shape)的行数。
    ///
    /// **这一对是差分整形唯一的运行期守护**:判据写错导致永远 miss 时,
    /// 画面完全正确、日志一切正常,只有这里的比值会掉下去。
    pub reshape_miss: u64,
    /// F159:本窗口内整帧指纹命中(没提交 GPU)的帧数。
    pub fp_hit: u64,
    /// F159:未命中(真的画了一帧)的帧数。
    ///
    /// **这一对是整帧指纹唯一的运行期守护**:判据写错导致永远 miss 时,
    /// 画面完全正确、日志一切正常、性能悄悄回到改之前,只有这里的比值会掉。
    pub fp_miss: u64,
    /// F157:本窗口收到了多少次 `RedrawRequested`(唤醒率的直接读数)。
    ///
    /// **提成一等指标是有意的**:整帧指纹会把 CPU 压下去,从而**掩盖**
    /// 「唤醒 400/s」这个真根因,让下一版失去动力。
    pub wakes: u64,
    /// F157:我们**主动**调 `request_redraw` 的次数——`about_to_wait` 到点补画。
    pub rr_sched: u64,
    /// F157:我们主动调 `request_redraw` 的次数——因窗口事件 / 后台唤醒 / UI 请求。
    ///
    /// `wakes` 与 `rr_sched + rr_evt` 的差值不为零是**正常**的:多次
    /// `request_redraw` 会被 winit 合并成一次 `RedrawRequested`,OS 也会主动发
    /// (窗口被遮挡后暴露)。差值本身就是信息,**不试图把它归成精确的三类**
    /// ——那需要一个「最近一次请求来源」的单槽标记,而合并会让它系统性失真。
    /// 宁可报两个诚实的数,不报一个精确但错的分类。
    pub rr_evt: u64,
    /// F157:`ui_dirty` 被置真的来源(源码行号, 次数)。全部置脏点都在 `app.rs`,
    /// 所以只带行号不带文件名(见 `diag::note_ui_dirty`)。
    pub dirty_sites: Vec<(u32, u64)>,
    /// F157:归因表的 8 个槽位用完之后,落在外面的置脏次数。
    pub dirty_other: u64,
    /// F157:这一窗口喂给 egui 的输入事件总数。
    pub egui_events: u64,
    /// F157:其中有多少**帧**带着至少一个事件。
    ///
    /// 与 `egui_events` 分开报:egui 的 `wants_repaint_after` 只要本趟
    /// `events` 非空就返回 `Duration::ZERO`,所以「有几帧带着事件」才是
    /// 跟 `rdelay_zero` 直接对得上的那个数。
    pub egui_event_frames: u64,
    /// F157:`repaint_delay == Duration::ZERO` 的帧数。
    pub rdelay_zero: u64,
    /// F157:`repaint_delay` 有限非零的帧数。
    pub rdelay_finite: u64,
    /// F157:`repaint_delay == Duration::MAX`(egui 不需要重绘)的帧数。
    /// **真空闲时本该是全部**。
    pub rdelay_max: u64,
    pub tabs: u64,
    pub panes: u64,
    pub hosts: u64,
    pub mem_process_mb: u64,
    /// F164:整个进程的 CPU 占用,**按核数归一**。`None` = 采不到。
    pub cpu_pct: Option<u8>,
    /// F164:主线程的 CPU 占用,**不归一**(一个核跑满 = 100)。
    ///
    /// 与 `cpu_pct` 口径不同是有意的:F158 那次的症状是「烧满一个核」,
    /// 在多核机上归一化之后只有个位数,会淹没在噪声里。
    pub main_cpu_pct: Option<u8>,
    /// F165:GPU 引擎占用,按类型聚合的前两名。空 = 采不到或全零。
    pub gpu_engines: Vec<(String, u8)>,
    /// F165:GPU 探针可用吗。区分「可用但为 0」与「采不到」。
    pub gpu_available: bool,
    /// F165:本进程显存 (已用 MB, 预算 MB)。`None` = 采不到。
    pub vram_mb: Option<(u64, u64)>,
    /// F165:GPU 帧耗时分布。样本数为 0 = 不支持或本窗口没采到。
    pub gpu_frame_us: Counts,
    /// F167:本窗口的用户滚动事件数(滚轮/翻页键/拖拽自动滚,计次量)。
    pub scroll_events: u64,
    /// F167/F169:传输队列此刻未收尾条数(状态量,读而不清)。
    pub xfer_jobs: u64,
    /// F169:未传完的字节(total - done,状态量)。
    pub xfer_bytes_left: u64,
    /// F169:在跑的传输条数(在途缓冲 = running × 64KiB chunk)。
    pub xfer_running: u64,
    /// F169:全部 pane 的 scrollback 记账字节(gauge,主线程每帧更新)。
    pub mem_scroll_bytes: u64,
    /// F167/F169:全部 pane 的回溯总行数(profile.load 的分母)。来自 F169
    /// 内存记账那次遍历,与 `scroll_events`(用户滚动计次)不是一回事。
    pub scroll_lines: u64,
    /// F169:TextLayer 的 Buffer 估算字节(gauge)。
    pub mem_text_bytes: u64,
    /// F168:线程组 CPU(组名, 不归一不封顶百分比)。固定顺序,见 `group_threads`。
    pub thread_groups: Vec<(&'static str, u32)>,
    /// F168:没进分组表的线程原名(Debug 档打出来,防列举式漏项)。
    pub thread_unmapped: Vec<(String, u32)>,
    /// F168:线程枚举这一窗口成功过。false → profile.cpu 的分组段渲染 n/a。
    pub thread_available: bool,
    /// F170:终端趟 GPU 耗时分布(槽1-槽0)。
    pub gpu_term_us: Counts,
    /// F170:egui 趟 GPU 耗时分布(槽2-槽1)。
    pub gpu_egui_us: Counts,
    /// F170:INSIDE_PASSES 拿到了。false → 分层渲染 `分层:n/a`。
    pub gpu_split_supported: bool,
}

/// 逐阶段计数。长度与 `diag::Stage` 的变体数一致。
pub type StageCounts = [Counts; crate::diag::STAGE_COUNT];

/// F164:进程 CPU 超过这个百分比(按核数归一)就不算空闲。
const IDLE_CPU_PCT: u8 = 5;

/// F164:主线程 CPU 超过这个百分比(不归一)就不算空闲。
///
/// 比进程阈值高:主线程本来就承担事件循环,偶尔的一次唤醒会打到十几。
/// 20 以上意味着事件循环在真忙 —— 那正是要抓的。
const IDLE_MAIN_CPU_PCT: u8 = 20;

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
            sync_blocks: 0,
            sync_timeouts: 0,
            reshape_hit: 0,
            reshape_miss: 0,
            fp_hit: 0,
            fp_miss: 0,
            wakes: 0,
            rr_sched: 0,
            rr_evt: 0,
            dirty_sites: Vec::new(),
            dirty_other: 0,
            egui_events: 0,
            egui_event_frames: 0,
            rdelay_zero: 0,
            rdelay_finite: 0,
            rdelay_max: 0,
            tabs: 0,
            panes: 0,
            hosts: 0,
            mem_process_mb: 0,
            cpu_pct: None,
            main_cpu_pct: None,
            gpu_engines: Vec::new(),
            gpu_available: false,
            vram_mb: None,
            gpu_frame_us: [0; BUCKETS],
            scroll_events: 0,
            xfer_jobs: 0,
            xfer_bytes_left: 0,
            xfer_running: 0,
            mem_scroll_bytes: 0,
            scroll_lines: 0,
            mem_text_bytes: 0,
            thread_groups: Vec::new(),
            thread_unmapped: Vec::new(),
            thread_available: false,
            gpu_term_us: [0; BUCKETS],
            gpu_egui_us: [0; BUCKETS],
            gpu_split_supported: false,
        }
    }

    /// 这个窗口里什么都没发生。
    ///
    /// 判据只看「有没有画过帧 / 有没有收过字节 / 有没有按过键 / 有没有连接
    /// 或 SFTP 动作」—— 标签数、内存这类**状态量**在空闲时也非零，拿它们
    /// 判活会让空闲的 mullion 每 5 秒写一次盘，笔记本硬盘永远睡不下去。
    ///
    /// **`stage_us` 有意不算活动**：事件循环被无关的定时器唤一下（重连退避、
    /// 心跳）也会留下阶段样本，把它算成活动就等于「进程活着就每 5 秒写一次盘」，
    /// 又回到这条判据要防的那件事上。有帧/有字节/有按键/有节流才算真在干活。
    ///
    /// **`sync_timeouts` 不能只靠 `sync_blocks` 兜住**：一个同步块可以在上一个
    /// 窗口进入、这个窗口才超时逃生——那种窗口 `sync_blocks` 为 0 但
    /// `sync_timeouts` 非零，恰恰是最需要落盘的一种（T2 故障正在发生）。
    ///
    /// **CPU 是唯一一个例外的状态量**:`tabs`/`mem` 那些空闲时也非零,拿它们
    /// 判活会让空闲的 mullion 每 5 秒写一次盘。但 CPU 不同 —— 空闲时它本该
    /// 接近零,非零恰恰说明「看着空闲、实则在烧」(F158),那正是最需要落盘的
    /// 一种窗口。阈值把两者分开。
    pub fn is_idle(&self) -> bool {
        self.frames == 0
            && self.throttled == 0
            && self.redraw_terminal == 0
            && self.redraw_ui == 0
            && self.redraw_both == 0
            && self.inbound_bytes == 0
            && self.keys == 0
            && self.connects_ok == 0
            && self.connects_err == 0
            && self.reconnects == 0
            && self.sftp_ops == 0
            && self.sync_blocks == 0
            && self.sync_timeouts == 0
            && !self.cpu_is_busy()
    }

    /// F164:CPU 读数说明这一窗口其实在干活。
    ///
    /// **`is_some_and` 不是 `is_none_or`**:探针采不到(`None`)时必须算
    /// 「不忙」。反过来的话,任何一台读不到 CPU 的机器上,空闲的 mullion
    /// 会每 5 秒写一次盘 —— 正是 `is_idle` 这条判据当初要防的事。
    fn cpu_is_busy(&self) -> bool {
        self.cpu_pct.is_some_and(|p| p >= IDLE_CPU_PCT)
            || self.main_cpu_pct.is_some_and(|p| p >= IDLE_MAIN_CPU_PCT)
    }
}

/// 百分比渲染。`None` → `n/a`(不是 0:「采不到」和「真的是 0」是两回事)。
fn fmt_pct(v: Option<u8>) -> String {
    v.map_or_else(|| "n/a".to_string(), |p| format!("{p}%"))
}

/// F165:GPU 引擎占用渲染成 `3D:14%/Copy:3%`。
///
/// 三种状态必须长得不一样:探针不可用 `n/a`、可用但全零 `0%`、有值列出来。
/// 把前两种混成一个的话,「这台机器读不到 GPU」和「这台机器没在用 GPU」
/// 在日志里无法区分。
fn fmt_engines(engines: &[(String, u8)], available: bool) -> String {
    if !available {
        return "n/a".to_string();
    }
    if engines.is_empty() {
        return "0%".to_string();
    }
    engines
        .iter()
        .map(|(k, v)| format!("{k}:{v}%"))
        .collect::<Vec<_>>()
        .join("/")
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

    // F157:置脏点按次数倒序取前三。倒序是重点——空闲时只有一处每帧都在
    // 置脏,按行号排的话它会被一堆各来过一次的启动期置脏点埋掉。
    let mut sites = s.dirty_sites.clone();
    sites.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    sites.truncate(3);
    let mut dirty_parts: Vec<String> = sites.iter().map(|(l, n)| format!("{l}:{n}")).collect();
    if s.dirty_other > 0 {
        dirty_parts.push(format!("other:{}", s.dirty_other));
    }
    // 空表报 `-` 而不是空串:人工验收清单第 3 条专门看这个,`dirty=` 后面
    // 什么都没有 = 埋点根本没接上,而那和「这窗口确实一次没置脏」必须
    // 在日志里长得不一样。
    let dirty_part = if dirty_parts.is_empty() {
        "-".to_string()
    } else {
        dirty_parts.join(",")
    };

    // GPU 帧耗时:样本数为 0 时报 n/a 而不是 p50=0 —— adapter 不支持
    // TIMESTAMP_QUERY 与「GPU 一帧只用了 0µs」必须在日志里长得不一样。
    let gpu_us_part = {
        let n = total(&s.gpu_frame_us);
        if n == 0 {
            "n/a".to_string()
        } else {
            format!(
                "{n}x/p50={}/p95={}",
                fmt_us(quantile_us(&s.gpu_frame_us, 0.5)),
                fmt_us(quantile_us(&s.gpu_frame_us, 0.95))
            )
        }
    };

    Some(format!(
        "profile {:.1}s frame={}x/p50={}/p95={}/max={} present={} skip={} throttle={} \
         redraw=term:{}/ui:{}/both:{} 同步块={}x/超时={}x in={} key={}x/echo={}x/p95={} {} \
         reshape=hit:{}/miss:{} fp=hit:{}/miss:{} wake={}x/rr=sched:{},evt:{} dirty={} \
         egui_ev={}x/f:{} rdelay=z:{}/f:{}/m:{} \
         conn=ok:{}/err:{}/re:{} sftp={} tabs={} panes={} hosts={} mem={}MB cpu={} gpu={} vram={} gpu_us={}",
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
        s.sync_blocks,
        s.sync_timeouts,
        rate,
        s.keys,
        total(&s.echo_us),
        fmt_us(quantile_us(&s.echo_us, 0.95)),
        stage_part,
        s.reshape_hit,
        s.reshape_miss,
        s.fp_hit,
        s.fp_miss,
        s.wakes,
        s.rr_sched,
        s.rr_evt,
        dirty_part,
        s.egui_events,
        s.egui_event_frames,
        s.rdelay_zero,
        s.rdelay_finite,
        s.rdelay_max,
        s.connects_ok,
        s.connects_err,
        s.reconnects,
        s.sftp_ops,
        s.tabs,
        s.panes,
        s.hosts,
        s.mem_process_mb,
        match (s.cpu_pct, s.main_cpu_pct) {
            (None, None) => "n/a".to_string(),
            (a, b) => format!("{}/主线程:{}", fmt_pct(a), fmt_pct(b)),
        },
        fmt_engines(&s.gpu_engines, s.gpu_available),
        s.vram_mb
            .map_or_else(|| "n/a".to_string(), |(u, b)| format!("{u}/{b}MB")),
        gpu_us_part,
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
            sync_blocks: 12,
            sync_timeouts: 3,
            reshape_hit: 900,
            reshape_miss: 100,
            fp_hit: 250,
            fp_miss: 50,
            wakes: 340,
            rr_sched: 40,
            rr_evt: 7,
            dirty_sites: vec![(8365, 300), (7191, 2)],
            dirty_other: 5,
            egui_events: 9,
            egui_event_frames: 4,
            rdelay_zero: 300,
            rdelay_finite: 0,
            rdelay_max: 0,
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
    /// 「这个版本忘了统计跳帧」在日志里长得一模一样。同理覆盖 T2 的
    /// `sync_timeouts`：「这个窗口没超时」与「忘了统计超时」不能长得一样。
    #[test]
    fn a_zero_count_is_printed_rather_than_omitted() {
        let mut s = busy_snapshot();
        s.skipped = 0;
        s.sync_timeouts = 0;
        let line = render_line(&s).expect("忙窗口该有一行");
        assert!(line.contains("skip=0"), "零值被省略了：{line}");
        assert!(line.contains("超时=0x"), "同步块超时的零值被省略了：{line}");
    }

    /// T2(领域陷阱)：同步块与超时次数必须原样进得了剖面行，这是本项目
    /// 最有价值的一组指标——历史上「打字慢一拍」的真根因就是这里的超时收口。
    #[test]
    fn the_sync_block_counts_reach_the_line() {
        let line = render_line(&busy_snapshot()).expect("忙窗口该有一行");
        assert!(line.contains("同步块=12x"), "没报同步块次数：{line}");
        assert!(line.contains("超时=3x"), "没报同步块超时次数：{line}");
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

    /// 六个「有事发生但没画成帧」的字段,**每一个都必须单独**把窗口顶成非空闲。
    ///
    /// 「重绘全被帧闸挡下、一帧没画成」正是领域陷阱 T3 发作时的样子;判成
    /// 空闲就等于把要查的那个指标 drain 掉又不打印,数据静默丢失。`sync_blocks`/
    /// `sync_timeouts`(T2)同理:一个同步块可以在上一个窗口进入、这个窗口才
    /// 超时逃生,那种窗口 `sync_blocks` 恰好是 0,只能靠 `sync_timeouts` 自己
    /// 把窗口顶成非空闲。
    ///
    /// 逐字段循环而不是一份「几个字段一起非零」的 fixture:后者删掉任一条
    /// 判据,剩下的都能替它兜住,测试照绿 —— 只能证明「六条里至少还剩一条」。
    ///
    /// 自证会变红:删掉 `is_idle` 里 `throttled`/`redraw_terminal`/`redraw_ui`/
    /// `redraw_both`/`sync_blocks`/`sync_timeouts` 中的**任意一条**。
    #[test]
    #[allow(clippy::type_complexity)]
    fn each_no_frame_activity_counter_alone_keeps_the_window_from_being_idle() {
        let fields: [(&str, fn(&mut Snapshot)); 6] = [
            ("throttled", |s| s.throttled = 400),
            ("redraw_terminal", |s| s.redraw_terminal = 400),
            ("redraw_ui", |s| s.redraw_ui = 400),
            ("redraw_both", |s| s.redraw_both = 400),
            ("sync_blocks", |s| s.sync_blocks = 400),
            ("sync_timeouts", |s| s.sync_timeouts = 400),
        ];
        for (name, set) in fields {
            let mut s = Snapshot {
                window_ms: 5_000,
                ..Snapshot::empty()
            };
            set(&mut s);
            assert!(
                !s.is_idle(),
                "只有 {name} 非零时窗口被当成了空闲 —— T3 的指标会被静默丢掉"
            );
            assert!(render_line(&s).is_some(), "只有 {name} 非零时没出剖面行");
        }
    }

    /// 节流次数要如实报进剖面行(零值也报,见
    /// `a_zero_count_is_printed_rather_than_omitted`)。
    #[test]
    fn the_throttle_count_reaches_the_line() {
        let s = Snapshot {
            window_ms: 5_000,
            throttled: 400,
            ..Snapshot::empty()
        };
        let line = render_line(&s).expect("这种窗口必须出一行");
        assert!(line.contains("throttle=400"), "没报节流次数:{line}");
    }

    /// 「没量到一次回显」和「量到的回显是 0µs」必须在日志里长得不一样。
    ///
    /// 只打 p95 的话两者都是 `0us`;看日志的人会把「这条链路根本没采到样本」
    /// 读成「回显快到 0 微秒」,方向完全反了。
    ///
    /// 自证会变红:把 `render_line` 里的 `total(&s.echo_us)` 换成常量 `0`。
    #[test]
    fn an_unmeasured_echo_is_told_apart_from_a_fast_one() {
        let no_samples = render_line(&busy_snapshot()).expect("有一行");
        assert!(
            no_samples.contains("echo=0x"),
            "没报回显样本数:{no_samples}"
        );

        let mut measured = busy_snapshot();
        measured.echo_us[bucket_of(30_000)] = 7;
        let line = render_line(&measured).expect("有一行");
        assert!(line.contains("echo=7x"), "回显样本数不对:{line}");
        assert!(!line.contains("p95=0us"), "量到了却报 0:{line}");
    }

    /// F12:整形缓存的命中/未命中必须进剖面行。
    ///
    /// 这是"差分整形悄悄退化回全量"的**唯一**运行期守护 —— 判据写错时
    /// 画面完全正确,只有 miss 数会暴露它。没有这一列,退化是静默的。
    ///
    /// 自证会变红:把 `render_line` 里 `reshape=` 那一段删掉。
    #[test]
    fn the_reshape_cache_counts_reach_the_line() {
        let line = render_line(&busy_snapshot()).expect("忙窗口该有一行");
        assert!(
            line.contains("reshape=hit:900/miss:100"),
            "没报整形缓存命中率:{line}"
        );
    }

    /// 零命中同样要**显式写出来**:"这个窗口一次都没命中"与"这个版本
    /// 忘了统计"在日志里不能长得一样(与 `skip=0` 同一条纪律)。
    ///
    /// 自证会变红:给 `render_line` 里的 `reshape=` 段加上
    /// `if s.reshape_hit > 0` 之类的条件。
    #[test]
    fn a_zero_reshape_hit_is_printed_rather_than_omitted() {
        let mut s = busy_snapshot();
        s.reshape_hit = 0;
        let line = render_line(&s).expect("忙窗口该有一行");
        assert!(line.contains("reshape=hit:0/"), "零命中被省略了:{line}");
    }

    /// F157:`repaint_delay` 的三分桶。**`MAX` 必须与「很大的有限值」分开**。
    ///
    /// `Duration::MAX` 的语义是「egui 不需要重绘」,一个很大的有限值的语义是
    /// 「egui 要重绘,只是可以等」。归成一类的话,剖面里「空闲时 egui 一次都
    /// 没要过重绘」(健康)和「空闲时 egui 每帧都要重绘,只是可以等很久」
    /// (本切片要查的那个病)长得一模一样。
    ///
    /// 自证会变红:把 `repaint_bucket` 的判据改成
    /// `if d >= std::time::Duration::from_secs(1) { Max }`。
    #[test]
    fn a_huge_finite_repaint_delay_is_not_the_same_as_never() {
        use std::time::Duration;
        assert_eq!(repaint_bucket(Duration::ZERO), RepaintBucket::Zero);
        assert_eq!(
            repaint_bucket(Duration::from_nanos(1)),
            RepaintBucket::Finite
        );
        assert_eq!(
            repaint_bucket(Duration::from_millis(16)),
            RepaintBucket::Finite
        );
        assert_eq!(
            repaint_bucket(Duration::from_secs(86_400)),
            RepaintBucket::Finite,
            "一天之后要重绘 ≠ 永远不需要重绘"
        );
        assert_eq!(repaint_bucket(Duration::MAX), RepaintBucket::Max);
    }

    /// F157:四段归因必须原样进剖面行。
    ///
    /// 这一行是本切片**唯一**的产出——下一版改什么完全取决于它。段名或
    /// 分隔符写错了,人还是能读,但 `findstr` 拉不出时间序列。
    ///
    /// 自证会变红:把 `render_line` 里 `wake=` 那一整段删掉。
    #[test]
    fn the_frame_loop_attribution_reaches_the_line() {
        let line = render_line(&busy_snapshot()).expect("忙窗口该有一行");
        assert!(line.contains("wake=340x"), "没报唤醒次数:{line}");
        assert!(
            line.contains("rr=sched:40,evt:7"),
            "没报主动请求重绘的来源:{line}"
        );
        assert!(
            line.contains("egui_ev=9x/f:4"),
            "没报喂给 egui 的事件数:{line}"
        );
        assert!(
            line.contains("rdelay=z:300/f:0/m:0"),
            "没报 repaint_delay 分桶:{line}"
        );
        assert!(
            line.contains("fp=hit:250/miss:50"),
            "没报整帧指纹命中率:{line}"
        );
    }

    /// 置脏点按次数**倒序**取前三,溢出的槽位报成 `other:N`。
    ///
    /// 倒序是重点:空闲时只有一处每帧都在置脏,它必须排在第一位;按行号
    /// 排序的话它会被一堆各来过一次的启动期置脏点埋掉。
    ///
    /// 自证会变红:把 `render_line` 里的 `Reverse` 去掉。
    #[test]
    fn the_dirty_sites_are_ranked_by_how_often_they_fire() {
        let mut s = busy_snapshot();
        s.dirty_sites = vec![(100, 1), (8365, 300), (200, 2), (300, 3)];
        s.dirty_other = 0;
        let line = render_line(&s).expect("忙窗口该有一行");
        assert!(
            line.contains("dirty=8365:300,300:3,200:2"),
            "置脏点没按次数倒序取前三:{line}"
        );
        assert!(!line.contains("100:1"), "第四名不该出现在行里:{line}");
    }

    /// 归因表溢出必须**说出来**。8 个槽位用完之后落进 `other`,不报的话
    /// 「这几处就是全部」与「还有一堆没槽位」在日志里长得一样。
    ///
    /// 自证会变红:把 `render_line` 里那句 `if s.dirty_other > 0` 的分支删掉。
    #[test]
    fn an_overflowing_attribution_table_says_so() {
        let mut s = busy_snapshot();
        s.dirty_sites = vec![(8365, 300)];
        s.dirty_other = 17;
        let line = render_line(&s).expect("忙窗口该有一行");
        assert!(line.contains("dirty=8365:300,other:17"), "溢出没报:{line}");
    }

    /// 一次置脏都没采到时报 `dirty=-`,**不是空字符串**。
    ///
    /// 人工验收清单第 3 条专门看这个:`dirty=` 后面什么都没有 = 埋点根本
    /// 没接上(F12 同款陷阱:埋点白埋时画面完全正常)。留成空串的话它跟
    /// 「这个窗口确实一次没置脏」长得一样,一整趟实机往返就白跑了。
    ///
    /// 自证会变红:把那个 `-` 占位改成空字符串。
    #[test]
    fn an_empty_attribution_table_is_told_apart_from_a_missing_one() {
        let mut s = busy_snapshot();
        s.dirty_sites = Vec::new();
        s.dirty_other = 0;
        let line = render_line(&s).expect("忙窗口该有一行");
        assert!(line.contains("dirty=- "), "空归因表没有占位符:{line}");
    }

    /// F157 的四段**一律不进 `is_idle`**。
    ///
    /// 它们在空闲时恰恰非零(那正是要查的东西),进了判据就等于「空闲的
    /// mullion 每 5 秒写一次盘」,笔记本硬盘永远睡不下去——这是布局落盘
    /// 已经踩过一次的坑,与 F12 的 `reshape_*` 处理一致。
    ///
    /// 自证会变红:往 `is_idle` 的条件里加上 `&& self.wakes == 0`。
    #[test]
    fn the_new_attribution_counters_do_not_count_as_activity() {
        let parked = Snapshot {
            window_ms: 5_000,
            wakes: 2_000,
            rr_sched: 1_700,
            rr_evt: 3,
            dirty_sites: vec![(8365, 300)],
            dirty_other: 1,
            egui_events: 5,
            egui_event_frames: 2,
            rdelay_zero: 300,
            fp_hit: 300,
            fp_miss: 1,
            ..Snapshot::empty()
        };
        assert!(
            parked.is_idle(),
            "光有归因数据就被算成了活动 —— 硬盘会永远醒着"
        );
    }

    /// **CPU 超阈值必须打破空闲门**。
    ///
    /// 这是 F164 存在的理由。F158 那次是「看着空闲、实则烧满一个核」——
    /// 旧的 `is_idle` 只看帧/字节/按键,那种窗口一行都不写,故障在日志里
    /// 完全不存在。
    ///
    /// 自证会变红:把 `is_idle` 里 CPU 那两条判断删掉。
    #[test]
    fn a_window_that_looks_idle_but_burns_cpu_still_gets_written() {
        let mut s = Snapshot::empty();
        s.window_ms = 5_000;
        assert!(s.is_idle(), "全零快照该算空闲");

        s.main_cpu_pct = Some(96);
        assert!(
            !s.is_idle(),
            "主线程烧满一个核却仍判空闲 —— 这一行不会写盘,故障在日志里不存在"
        );

        s.main_cpu_pct = None;
        s.cpu_pct = Some(40);
        assert!(!s.is_idle(), "进程 CPU 40% 却仍判空闲");
    }

    /// **采不到(None)不打破空闲门**。
    ///
    /// 探针不可用时如果算作「忙」,空闲的 mullion 会每 5 秒写一次盘 ——
    /// 正是 `is_idle` 这条判据当初要防的事(笔记本硬盘永远睡不下去)。
    ///
    /// 自证会变红:把 `is_some_and` 改成 `is_none_or`。
    #[test]
    fn a_cpu_probe_that_reports_nothing_does_not_wake_the_disk() {
        let mut s = Snapshot::empty();
        s.window_ms = 5_000;
        s.cpu_pct = None;
        s.main_cpu_pct = None;
        assert!(s.is_idle(), "采不到 CPU 被当成了忙");
    }

    /// 真空闲(CPU 接近 0)照旧不写盘。
    ///
    /// 自证会变红:把 `IDLE_CPU_PCT` 改成 0。
    #[test]
    fn a_genuinely_idle_window_is_still_skipped() {
        let mut s = Snapshot::empty();
        s.window_ms = 5_000;
        s.cpu_pct = Some(0);
        s.main_cpu_pct = Some(1);
        assert!(s.is_idle(), "真空闲也写盘了,硬盘睡不下去");
    }

    /// 渲染行里带 CPU,采不到时是 `n/a` 而不是 0。
    ///
    /// 自证会变红:把 `fmt_pct` 的 `None` 分支改成返回 `"0%"`。
    #[test]
    fn the_line_shows_cpu_and_says_n_a_when_it_could_not_be_read() {
        let mut s = Snapshot::empty();
        s.window_ms = 5_000;
        s.frames = 10;
        s.cpu_pct = Some(8);
        s.main_cpu_pct = Some(96);
        let line = render_line(&s).expect("非空闲窗口该出行");
        assert!(line.contains("cpu=8%/主线程:96%"), "行里没有 CPU:{line}");

        s.cpu_pct = None;
        s.main_cpu_pct = None;
        let line = render_line(&s).expect("非空闲窗口该出行");
        assert!(
            line.contains("cpu=n/a"),
            "采不到时该报 n/a 而不是编一个 0:{line}"
        );
    }

    /// GPU 的三种状态在日志里必须长得不一样。
    ///
    /// 「读不到 GPU」和「GPU 空着」混成同一个字符串的话,排障时没法判断
    /// 是探针坏了还是真的没在渲染。
    ///
    /// 自证会变红:把 `fmt_engines` 的 `!available` 分支改成返回 `"0%"`。
    #[test]
    fn an_unavailable_gpu_probe_reads_differently_from_an_idle_gpu() {
        assert_eq!(fmt_engines(&[], false), "n/a");
        assert_eq!(fmt_engines(&[], true), "0%");
        assert_eq!(
            fmt_engines(&[("3D".to_string(), 14), ("Copy".to_string(), 3)], true),
            "3D:14%/Copy:3%"
        );
    }

    /// 显存采不到时报 `n/a`,不是 `0/0MB`。
    ///
    /// 「这台机器读不到显存」和「这个进程一点显存都没占」是两回事,
    /// 后者在一个 GPU 渲染的终端里根本不可能发生,混起来会误导排障。
    ///
    /// 自证会变红:把 `render_line` 里 `vram` 那段的 `map_or_else`
    /// 换成 `map_or("0/0MB".to_string(), ..)`。
    #[test]
    fn vram_that_could_not_be_read_says_n_a_instead_of_zero() {
        let mut s = Snapshot::empty();
        s.window_ms = 5_000;
        s.frames = 10;
        let line = render_line(&s).expect("非空闲窗口该出行");
        assert!(line.contains("vram=n/a"), "采不到却报了数字:{line}");

        s.vram_mb = Some((123, 4096));
        let line = render_line(&s).expect("非空闲窗口该出行");
        assert!(line.contains("vram=123/4096MB"), "显存没渲染出来:{line}");
    }

    /// 没采到 GPU 帧耗时时报 `n/a`,不是 `p50=0`。
    ///
    /// adapter 不支持 TIMESTAMP_QUERY 与「GPU 一帧只用了 0µs」是两回事,
    /// 后者还会让人以为渲染是免费的。
    ///
    /// 自证会变红:把 `gpu_us_part` 的 `n == 0` 分支删掉。
    #[test]
    fn a_gpu_timer_that_never_reported_says_n_a_instead_of_zero() {
        let mut s = Snapshot::empty();
        s.window_ms = 5_000;
        s.frames = 10;
        let line = render_line(&s).expect("非空闲窗口该出行");
        assert!(line.contains("gpu_us=n/a"), "没采到却报了数字:{line}");

        // `bucket_of` 是本模块私有的,`mod tests` 里有 `use super::*` 直接可用。
        s.gpu_frame_us[bucket_of(2_000)] = 3;
        let line = render_line(&s).expect("非空闲窗口该出行");
        assert!(line.contains("gpu_us=3x/"), "采到了却没报:{line}");
    }
}
