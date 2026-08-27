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

/// F169:传输 worker 的读写 chunk 字节数。**在途缓冲记账按它算**
/// (running × 此值),改这里记账自动跟走。app.rs 的传输 buffer 引用它。
pub const XFER_CHUNK: u64 = 64 * 1024;

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

/// F173：一个 pane 在这一窗口里的归因读数。
///
/// 存在的理由是全局 `in=`／`bands=脏/总` 都是**跨 pane 拍平的标量**：
/// 实机静置日志里每 60 秒来一次 `in=3.9KB/s bands=4/72 seg=3`，看得见有
/// 东西在动，指不出是谁在动、动在屏幕的哪一块。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneDetail {
    /// `PaneId.0`。0 是真实 pane 号，不是空槽哨兵（见 `diag::PaneTable`）。
    pub id: u32,
    /// 这一窗口该 pane 收到的远端字节。
    pub in_bytes: u64,
    /// 这一窗口该 pane 重建过的行带**并集**，bit n = 第 n 带（F172 的带号）。
    ///
    /// 并集而非计次：要答的是「屏幕的哪一块在动」。超过 64 带的折进最高位
    /// （64 带 × 16 行 = 1024 行，比任何真实窗口都高）。
    pub dirty_bands: u64,
    /// 该 pane 最近一帧的总带数。没有它，`@b23` 说不出「末带」还是中间某带，
    /// 而 tmux status-line 恰恰只动末带。
    pub band_total: u32,
    /// 该 pane 参与顶点重建的帧数。主行 `frame=` 是整窗口的，分不出
    /// 「三个 pane 各画一帧」与「一个 pane 画了三帧」。
    pub frames: u64,
}

/// 一个 5 秒窗口里采到的全部东西。
///
/// **纯数据**：由 `diag.rs` 的周期线程从各个原子计数器 drain 出来填好，
/// 再交给 [`render_lines`]。分成两步是为了让「这些行长什么样」可以脱离
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
    /// F172:本窗口内**重建了顶点**的行带数。
    pub band_dirty: u64,
    /// F172:本窗口内一共有多少带(所有画过的帧累加)。
    ///
    /// **这一对是行带差分唯一的运行期守护**:判据写错导致每帧全带重建时,
    /// 画面完全正确、日志一切正常、性能悄悄回到改之前,只有这个比值会顶到 1。
    pub band_total: u64,
    /// F172:变化行分成几个连通段。判「带宽 16 选得对不对」用 ——
    /// 段数远小于脏带数说明变化集中(可以放大带),逼近脏带数说明变化分散。
    pub band_segments: u64,
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
    /// F171:让 egui 说「要重绘」的窗口事件类型(`wev::kind_of` 的码, 次数)。
    ///
    /// `dirty_sites` 只能答到「`app.rs` 的哪一行置了脏」,再往上一级就断了。
    /// 这一段接着往上答「那一行凭什么响」——实机日志里它每 5 秒响 158 次,
    /// 而用户根本没碰键鼠。
    pub wev_kinds: Vec<(u32, u64)>,
    /// F171:事件类型多于槽位时的溢出。**与「有档没登记」无关**:
    /// `wev::kind_of` 是不带 `_` 的穷尽 match,加档是编译错误。
    pub wev_other: u64,
    /// F171:`CursorMoved` 与上一次**同坐标**的次数。
    ///
    /// 与 `wev_kinds` 里的 `cursor:N` 并排读:两者接近说明是坐标恒定的幽灵
    /// 事件(可按位置掐掉),差得远说明真有东西在动指针 —— 修法完全不同。
    pub cursor_dup: u64,
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
    /// F173:per-pane 归因。全局 `in=` 只是聚合速率,答不了「三个 pane 各说
    /// 一点」与「一个 pane 说了全部」的区别,而这两种的根因完全不同。
    pub pane_detail: Vec<PaneDetail>,
    /// F173:槽位用完之后落在外面的入站字节(不是 pane 数)。
    pub pane_other_bytes: u64,
    pub mem_process_mb: u64,
    /// F176:`mem_process_mb` 的口径(Windows commit / Linux rss)。
    pub mem_kind: crate::diag::MemKind,
    /// F176:专用工作集(MB)。`None` = 采不到。**不参与记账减法**,
    /// 理由见 `mem_parts` 的文档注释;它是 F177 预算闸的判据。
    ///
    /// 用 `Option` 而不是沿用 `mem_process_mb` 那个 0 哨兵:0 哨兵是 F155
    /// 的既有债(见本文件 `render_lines` 里 mem 行上方的注释),不再复制第二份。
    pub mem_ws_mb: Option<u64>,
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
    /// F175:`egui_state.on_window_event` 单独一趟的耗时分布。
    ///
    /// 与 `stage_us[window_event]` 是包含关系(后者含前者),两者一起看才能
    /// 判断「窗口事件贵」贵在喂 egui 还是贵在别的地方。
    pub egui_feed_us: Counts,
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
            pane_detail: Vec::new(),
            pane_other_bytes: 0,
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
            band_dirty: 0,
            band_total: 0,
            band_segments: 0,
            fp_hit: 0,
            fp_miss: 0,
            wakes: 0,
            rr_sched: 0,
            rr_evt: 0,
            dirty_sites: Vec::new(),
            dirty_other: 0,
            wev_kinds: Vec::new(),
            wev_other: 0,
            cursor_dup: 0,
            egui_events: 0,
            egui_event_frames: 0,
            rdelay_zero: 0,
            rdelay_finite: 0,
            rdelay_max: 0,
            tabs: 0,
            panes: 0,
            hosts: 0,
            mem_process_mb: 0,
            mem_kind: crate::diag::MemKind::Rss,
            mem_ws_mb: None,
            cpu_pct: None,
            main_cpu_pct: None,
            gpu_engines: Vec::new(),
            gpu_available: false,
            vram_mb: None,
            gpu_frame_us: [0; BUCKETS],
            egui_feed_us: [0; BUCKETS],
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

    /// F177:这一窗口「其他」栏的 MB 数,与 `profile.mem` 行同源
    /// (共用 [`mem_accounted_mb`])。预算闸的告警行要带上它。
    pub fn mem_other_mb(&self) -> u64 {
        self.mem_process_mb.saturating_sub(mem_accounted_mb(
            self.mem_scroll_bytes,
            self.xfer_running * XFER_CHUNK,
            self.mem_text_bytes,
        ))
    }
}

/// F157/F171:把一张 `diag::KeyTable` 的采样渲染成 `键:次数,键:次数,other:N`。
///
/// **倒序是重点** —— 空闲时只有一两个键每帧都在响,按键值排的话它们会被一堆
/// 各来过一次的启动期键埋掉,而恰恰是「每帧都响的那个」才是要找的东西。
///
/// **空表报 `-` 而不是空串**:`dirty=`/`wev=` 后面什么都没有 = 埋点根本没接上,
/// 那和「这窗口确实一次没响」必须在日志里长得不一样。
///
/// `label` 负责键→显示名:置脏点直接印行号,窗口事件要翻成短名。
fn render_key_table(items: &[(u32, u64)], other: u64, label: impl Fn(u32) -> String) -> String {
    let mut top = items.to_vec();
    top.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    top.truncate(3);
    let mut parts: Vec<String> = top
        .iter()
        .map(|(k, n)| format!("{}:{n}", label(*k)))
        .collect();
    if other > 0 {
        parts.push(format!("other:{other}"));
    }
    if parts.is_empty() {
        "-".to_string()
    } else {
        parts.join(",")
    }
}

/// 百分比渲染。`None` → `n/a`(不是 0:「采不到」和「真的是 0」是两回事)。
/// 一个绝对字节数。**不是速率** —— `profile.load` 的 `in=` 带 `/s`,这里
/// 的是这一窗口的实收量,两者相邻出现,单位必须自己把话说清。
fn fmt_bytes(b: u64) -> String {
    if b >= 1 << 20 {
        format!("{:.1}MB", b as f64 / (1 << 20) as f64)
    } else if b >= 1024 {
        format!("{:.1}KB", b as f64 / 1024.0)
    } else {
        format!("{b}B")
    }
}

/// 一个 pane 报几条脏带号。再多就读不动了,超出的折成 `+N`。
const PANE_BANDS_SHOWN: usize = 4;

/// `p1 in=6.5KB@b11,b23/24 frames=5`。
///
/// `@` 那一段在**从没重建过顶点**(`band_total == 0`)时整段省掉:那时候
/// 分母是编不出来的,印 `@-/0` 只会让人以为窗口高度是 0。
fn render_pane_detail(p: &PaneDetail) -> String {
    let mut s = format!("p{} in={}", p.id, fmt_bytes(p.in_bytes));
    if p.band_total > 0 {
        let all: Vec<u32> = (0..64).filter(|b| p.dirty_bands >> b & 1 == 1).collect();
        let shown = if all.is_empty() {
            // 一带没脏是**好消息**(差分全命中),但必须印出来 —— 省略的话
            // 它跟「这个 pane 压根没被记上」长得一模一样。
            "-".to_string()
        } else {
            let mut t = all
                .iter()
                .take(PANE_BANDS_SHOWN)
                .map(|b| format!("b{b}"))
                .collect::<Vec<_>>()
                .join(",");
            if all.len() > PANE_BANDS_SHOWN {
                // 截断要留痕:「脏了 4 带」与「脏了 40 带」不留痕就一样长,
                // 而后者意味着差分基本失效 —— 正是这条埋点最该抓的情况。
                t.push_str(&format!("+{}", all.len() - PANE_BANDS_SHOWN));
            }
            t
        };
        s.push_str(&format!("@{shown}/{}", p.band_total));
    }
    s.push_str(&format!(" frames={}", p.frames));
    s
}

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

/// F169/F176:记账合计(MB)。三块各自 `>> 20` 向下取整再相加。
///
/// **抽出来是为了让 `mem_parts` 与 `Snapshot::mem_other_mb` 同源** ——
/// 两处各算一遍的话,「日志里的其他」与「预算闸报的其他」迟早对不上,
/// 而那种不一致没有任何东西会报错。
pub fn mem_accounted_mb(scroll_b: u64, xfer_b: u64, text_b: u64) -> u64 {
    (scroll_b >> 20) + (xfer_b >> 20) + (text_b >> 20)
}

/// F169:`profile.mem` 正文的纯渲染。手工记账三个已知大块 + 显式余量
/// （用户拍板：不做自定义分配器）。
///
/// **单位不对称，调用方容易传反**：`process_mb` 已经是 **MB**（进程 RSS，
/// 整数）；`scroll_b`/`xfer_b`/`text_b` 三个记账块是**字节**，函数内部按
/// `>> 20` 换算成 MB —— `_b` 后缀就是在提醒「这三个不是 MB」。
///
/// 三块各自 `>> 20` 向下取整再相加，会让 `accounted` 比真实值略小，从而让
/// 「其他」略偏大（三块合计最多偏差 3MB；`process_mb` 本身不取整，不参与
/// 这个偏差）。这是**如实**的偏差不是 bug：宁可「其他」栏天然带几 MB 的
/// 取整噪声，也不在这里悄悄补偿——真正要查的是「其他」占比是不是长期
/// 偏大，几 MB 噪声掩盖不了那个信号。
///
/// `process_mb == 0` 不做特殊处理，按数值老实计算——本项目「采不到不许
/// 编成 0」的规矩意味着调用方应只在采到真实 RSS 时才调用本函数（采不到
/// 时留在 `Option` 层面处理，不要把 `None` 揉成 0 传进来）。
///
/// 余量为负时**显式报超出量**：静默夹 0（`saturating_sub` 一把梭）会让
/// 「记账模型错了」永远不被发现（spec §5）。
///
/// **F176:`ws_mb` 不参与减法。** 工作集会被系统裁剪(窗口最小化时尤其
/// 激进),而三个记账块是 Rust 堆上的 `Vec`、字节数不因页被换出而变小。
/// 拿 ws 做被减数,用户一最小化就会刷屏「记账超出」——把一个正常的系统
/// 行为报成记账模型崩了。`primary_mb`(Windows 是 commit)不被裁剪、恒 ≥
/// 我们的堆量,减法才成立。ws 的职责是另外两件:它是任务管理器里那个数,
/// 以及 F177 预算闸的判据。守护:
/// `tests::the_remainder_is_computed_against_commit_not_the_working_set`。
pub fn mem_parts(
    kind: crate::diag::MemKind,
    primary_mb: u64,
    ws_mb: Option<u64>,
    scroll_b: u64,
    xfer_b: u64,
    text_b: u64,
) -> String {
    let scroll = scroll_b >> 20;
    let xfer = xfer_b >> 20;
    let text = text_b >> 20;
    let accounted = mem_accounted_mb(scroll_b, xfer_b, text_b);
    // F176:ws 段只有 Commit 口径(Windows)才印 —— Linux 的 rss 本身就是
    // 常驻量,再括一个 ws 是同义反复。采不到印 `n/a`,不许静默印 0。
    let ws = match (kind, ws_mb) {
        (crate::diag::MemKind::Commit, Some(mb)) => format!("(ws {mb})"),
        (crate::diag::MemKind::Commit, None) => "(ws n/a)".to_string(),
        (crate::diag::MemKind::Rss, _) => String::new(),
    };
    let label = kind.label();
    if accounted <= primary_mb {
        format!(
            "{label}={primary_mb}MB{ws} = scroll:{scroll} xfer:{xfer} text:{text} 其他:{}",
            primary_mb - accounted
        )
    } else {
        format!(
            "{label}={primary_mb}MB{ws} = scroll:{scroll} xfer:{xfer} text:{text} \
             其他:0(记账超出{label} {}MB)",
            accounted - primary_mb
        )
    }
}

/// F167:remote-output 与「OSC 7 提示符心跳涓流」的分界。
/// 涓流每提示符几十字节,真刷屏至少 KB/s 级 —— 1 KB/s 在两者之间有两个
/// 数量级余量。具名常量,好调。
pub const REMOTE_OUTPUT_BPS: u64 = 1024;

/// F167:这 5 秒程序主要在干什么。单值,优先级命中即停(spec §3)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scene {
    SftpTransfer,
    Scrollback,
    Resize,
    Typing,
    RemoteOutput,
    Connecting,
    UiOnly,
    /// 空闲门放行了(这一行会写盘),但七条活动判据一条都没命中 —— 帧数为零
    /// 却在烧 CPU / 重绘全被节流 / T2 同步块超时之类。**这一档本身就是异常
    /// 信号**:F158 那次「看着空闲、实则烧满一个核」如果当时有这行日志,
    /// 一眼就能认出来。
    Unattributed,
    /// `scene_of` 是纯函数、对任意输入都要有定义;空闲门拦住的窗口不写盘,
    /// 所以这一档在**正常日志**里不出现。
    Idle,
}

impl Scene {
    pub fn label(self) -> &'static str {
        match self {
            Scene::SftpTransfer => "sftp-transfer",
            Scene::Scrollback => "scrollback",
            Scene::Resize => "resize",
            Scene::Typing => "typing",
            Scene::RemoteOutput => "remote-output",
            Scene::Connecting => "connecting",
            Scene::UiOnly => "ui-only",
            Scene::Unattributed => "unattributed",
            Scene::Idle => "idle",
        }
    }
}

/// F167:这 5 秒窗口该归到哪一档,按优先级命中即停,不做多标签。
///
/// 优先级顺序不是按发生频率,是按「排障时最想知道的是哪一个」:传输
/// 和滚动会让终端在这几秒里明显变卡,哪怕同一窗口里还有零星按键,也该
/// 先说是它们在占用;resize 排在 typing 前面是因为 resize 本身会触发一
/// 整帧重排,量级远超单纯打字,和 typing 撞在同一窗口时前者才是主因。
/// remote-output 用速率而不是「有没有入站字节」判定,是为了把 OSC 7
/// 提示符心跳那种涓流排除在外(见 `REMOTE_OUTPUT_BPS`)。
pub fn scene_of(s: &Snapshot) -> Scene {
    if s.xfer_jobs > 0 {
        return Scene::SftpTransfer;
    }
    if s.scroll_events > 0 {
        return Scene::Scrollback;
    }
    if total(&s.stage_us[crate::diag::Stage::Resize as usize]) > 0 {
        return Scene::Resize;
    }
    if s.keys > 0 {
        return Scene::Typing;
    }
    // 速率按毫秒换算,窗口为 0 时当 0 处理(不除零)。
    let bps = s
        .inbound_bytes
        .saturating_mul(1000)
        .checked_div(s.window_ms)
        .unwrap_or(0);
    if bps >= REMOTE_OUTPUT_BPS {
        return Scene::RemoteOutput;
    }
    if s.connects_ok + s.connects_err + s.reconnects > 0 {
        return Scene::Connecting;
    }
    if s.frames > 0 {
        return Scene::UiOnly;
    }
    // 空闲门与这七条判据不是子集关系(is_idle 还看 throttled/sync_*/sftp_ops/
    // CPU)。写盘了却归不出因,如实说「归不出」,不冒充空闲。
    if !s.is_idle() {
        return Scene::Unattributed;
    }
    Scene::Idle
}

/// 把一个窗口渲染成**一组行**日志：`profile` 概览 + `profile.load`/`cpu`/`mem`/
/// `gpu` 各一行（`debug=true` 时再加 `profile.cpu.unmapped`/`profile.mem.delta`）。
/// 空 `Vec` = 这个窗口空闲，不该写。
///
/// **每条记录单行**是硬要求（原「单行是硬要求」的措辞已改写，理由不变）：
/// 日志按行 grep，一条记录跨行就没法用 `grep profile mullion.log` 拉出时间序列。
/// 多行本身不违反这条——**每一行都是独立的一条记录**，各自带 `[时间][pid]`
/// 前缀（F166）。拆成多行是因为单行已经 500+ 字符、人眼扫不动（用户拍板，
/// 设计文档 §1）：概览行留给帧循环侧的全部段，资源侧的
/// `mem=/cpu=/gpu=/vram=/gpu_us=` 五段移到各自的 `profile.cpu`/`profile.mem`/
/// `profile.gpu` 行，`profile.load` 补上场景标签与分母
/// （tabs/panes/hosts/scroll/xfer/key/in）。
pub fn render_lines(s: &Snapshot, debug: bool) -> Vec<String> {
    if s.is_idle() {
        return Vec::new();
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
    // p50 与 p95 一起报:桶是 log2 的,光有 p95 时「937 次 × p95=1.0ms」的总账
    // 落在 0 到 19% 之间的任何位置都说得通 —— 那个区间宽到没法据此决定该去掉
    // 帧还是该去掉这一趟处理(F175 埋点的直接动机)。
    let stage_part = stages
        .iter()
        .map(|(k, n, p95)| {
            format!(
                "{}={}x/p50={}/p95={}",
                crate::diag::stage_name(*k as u8),
                n,
                fmt_us(quantile_us(&s.stage_us[*k], 0.5)),
                fmt_us(*p95)
            )
        })
        .collect::<Vec<_>>()
        .join(" ");

    // F175:喂给 egui-winit 那一趟单独的账。它被 `window_event` 那段**包含**,
    // 两者相减才是「路由判定 + 终端分支 + 标脏」的开销。
    let egui_feed_part = {
        let n = total(&s.egui_feed_us);
        if n == 0 {
            // 采不到与「快到量不出来」必须长得不一样(同 gpu_frame 的 n/a)。
            "n/a".to_string()
        } else {
            format!(
                "{n}x/p50={}/p95={}/max={}",
                fmt_us(quantile_us(&s.egui_feed_us, 0.5)),
                fmt_us(quantile_us(&s.egui_feed_us, 0.95)),
                fmt_us(quantile_us(&s.egui_feed_us, 1.0))
            )
        }
    };

    let dirty_part = render_key_table(&s.dirty_sites, s.dirty_other, |k| k.to_string());
    // F171:事件类型码要翻成短名再上日志 —— 光有码得回源码查表,归因就废了。
    let wev_part = render_key_table(&s.wev_kinds, s.wev_other, crate::wev::name_of);

    // GPU 帧耗时:样本数为 0 时报 n/a 而不是 p50=0 —— adapter 不支持
    // TIMESTAMP_QUERY 与「GPU 一帧只用了 0µs」必须在日志里长得不一样。
    let gpu_frame_part = {
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

    let mut lines = Vec::new();

    // 概览行:留给帧循环侧的全部段。`mem=/cpu=/gpu=/vram=/gpu_us=` 五段
    // 移到各自的 profile.cpu/mem/gpu 行,tabs/panes/hosts 按 spec 规则保留。
    lines.push(format!(
        "profile {:.1}s frame={}x/p50={}/p95={}/max={} present={} skip={} throttle={} \
         redraw=term:{}/ui:{}/both:{} 同步块={}x/超时={}x in={} key={}x/echo={}x/p95={} {} \
         reshape=hit:{}/miss:{} bands={}/{} seg={} fp=hit:{}/miss:{} \
         wake={}x/rr=sched:{},evt:{} dirty={} \
         wev={} curdup={} egui_ev={}x/f:{} egui_feed={} rdelay=z:{}/f:{}/m:{} \
         conn=ok:{}/err:{}/re:{} sftp={} tabs={} panes={} hosts={}",
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
        s.band_dirty,
        s.band_total,
        s.band_segments,
        s.fp_hit,
        s.fp_miss,
        s.wakes,
        s.rr_sched,
        s.rr_evt,
        dirty_part,
        wev_part,
        s.cursor_dup,
        s.egui_events,
        s.egui_event_frames,
        egui_feed_part,
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
    ));

    // load 行:场景标签 + 分母。`scroll=` 来自 F169 记账同一次遍历的行数
    // 汇总(gauge),与场景判据用的 `scroll_events`(事件计数)不是一回事。
    // 同理,这里的 `xfer=` 是**队列还剩多少没传**,而 mem 行里的 `xfer:`
    // 是**在途缓冲占了多少内存** —— 相邻两行同一个词,两个量,别看串。
    let scroll_disp = if s.scroll_lines >= 1000 {
        format!("{:.1}k行", s.scroll_lines as f64 / 1000.0)
    } else {
        format!("{}行", s.scroll_lines)
    };
    lines.push(format!(
        "profile.load scene={} tabs={} panes={} hosts={} scroll={} xfer={}个/{}MB剩 key={}x in={}",
        scene_of(s).label(),
        s.tabs,
        s.panes,
        s.hosts,
        scroll_disp,
        s.xfer_jobs,
        s.xfer_bytes_left >> 20,
        s.keys,
        rate,
    ));

    // pane 行:per-pane 归因。**没有读数就整行不印** —— 静置日志的价值一半
    // 在于安静,每 5 秒印一条空行会把「真的没人说话」淹掉。
    if !s.pane_detail.is_empty() {
        // 按 id 排序:表里的槽位顺序是抢占出来的,不排的话相邻两个窗口的
        // pane 会换位置,肉眼对不上、diff 也没法用。
        let mut panes = s.pane_detail.clone();
        panes.sort_by_key(|p| p.id);
        let mut segs: Vec<String> = panes.iter().map(render_pane_detail).collect();
        if s.pane_other_bytes > 0 {
            segs.push(format!("其他 in={}", fmt_bytes(s.pane_other_bytes)));
        }
        lines.push(format!("profile.pane {}", segs.join(" | ")));
    }

    // cpu 行:线程枚举采不到(`thread_available == false`)必须报 n/a,
    // 不能把各组渲染成 0 —— 那会把「没采到」读成「确实没占用」。
    let groups = if !s.thread_available {
        "n/a".to_string()
    } else {
        s.thread_groups
            .iter()
            .map(|(n, p)| format!("{n}:{p}%"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    lines.push(format!(
        "profile.cpu total={} main={} | {}",
        fmt_pct(s.cpu_pct),
        fmt_pct(s.main_cpu_pct),
        groups
    ));

    // mem 行:RSS == 0 是采样失败被揉成 0(既有债,见 diag.rs),一个跑着的
    // 进程 RSS 不可能真的是 0 —— 挡在渲染层,不许编成 `0MB = … 其他:0`。
    // 一个局部变量,mem 行和 debug 档的 mem.delta 行共用:两处各写一遍的话,
    // 改了一处忘另一处就是「两行对不上」的静默错值。
    let xfer_buf_bytes = s.xfer_running * XFER_CHUNK;
    if s.mem_process_mb == 0 {
        lines.push("profile.mem n/a(RSS 采不到)".to_string());
    } else {
        lines.push(format!(
            "profile.mem {}",
            mem_parts(
                s.mem_kind,
                s.mem_process_mb,
                s.mem_ws_mb,
                s.mem_scroll_bytes,
                xfer_buf_bytes,
                s.mem_text_bytes
            )
        ));
    }

    // gpu 行:分层段三态 —— 不支持 `n/a`、支持但本窗口没采到样 `0x`、
    // 有值列出来。三者长得不一样,理由与其余「采不到 ≠ 0」的处置一致。
    let split = if !s.gpu_split_supported {
        "分层:n/a".to_string()
    } else if total(&s.gpu_term_us) == 0 {
        "分层:0x".to_string()
    } else {
        format!(
            "term:{} egui:{}",
            fmt_us(quantile_us(&s.gpu_term_us, 0.5)),
            fmt_us(quantile_us(&s.gpu_egui_us, 0.5))
        )
    };
    lines.push(format!(
        "profile.gpu util={} vram={} frame={} | {}",
        fmt_engines(&s.gpu_engines, s.gpu_available),
        s.vram_mb
            .map_or_else(|| "n/a".to_string(), |(u, b)| format!("{u}/{b}MB")),
        gpu_frame_part,
        split,
    ));

    if debug {
        // 防「列举式分组表漏项」(F168):没进分组表的线程原名 + 各自百分比,
        // 只在 Debug 档打,常开的话会把固定几行拖到没法一眼看完。
        if !s.thread_unmapped.is_empty() {
            let unmapped = s
                .thread_unmapped
                .iter()
                .map(|(n, p)| format!("{n}:{p}%"))
                .collect::<Vec<_>>()
                .join(" ");
            lines.push(format!("profile.cpu.unmapped {unmapped}"));
        }
        // 排查记账模型用的原始字节(mem 行报的是取整 MB)。`mem_process_mb`
        // 换算回字节只是为了单位一致,不代表比 MB 口径更精确。
        lines.push(format!(
            "profile.mem.delta rss={}B scroll={}B xfer={}B text={}B",
            s.mem_process_mb.saturating_mul(1024 * 1024),
            s.mem_scroll_bytes,
            xfer_buf_bytes,
            s.mem_text_bytes
        ));
    }

    lines
}

/// F168:线程 CPU 百分比,不归一(100 = 一个核)**且不封顶**(组内多线程
/// 烧多核是常态,封顶等于把最该看见的读数削掉)。这就是它不复用
/// [`crate::sysprobe::cpu_pct`] 的原因 —— 那个按口径约定 `.min(100)`。
pub fn thread_group_pct(delta_ns: u64, window_ns: u64) -> Option<u32> {
    if window_ns == 0 {
        return None;
    }
    // 饱和而非 `as u32` 截断:病态输入(上游给出荒谬的小窗口)下截断会绕回成一个
    // 像模像样的小数字(恰好整除时甚至是 0),那是 T12 那类「静默错值」;爆表成
    // u32::MAX 一眼就知道该查上游。
    Some((((delta_ns as u128) * 100 / (window_ns as u128)).min(u32::MAX as u128)) as u32)
}

/// F168:前缀 → 组名。顺序即输出顺序。main 不在表里(由 F164 的主线程
/// 口径另源,采样层按 tid 排除)。
const THREAD_GROUPS: &[(&str, &str)] = &[
    ("tokio-runtime-worker", "tokio"),
    ("mullion-watchdog", "watchdog"),
    ("mullion-file-dialog", "dialog"),
    ("mullion-dragout", "dragout"),
];

/// Linux 的 `/proc/<tid>/comm` 上限:`TASK_COMM_LEN` 16 字节含 NUL,
/// 实际能存 15 字节(本机实测:`tokio-runtime-worker` 读回来是
/// `tokio-runtime-w`)。Windows 的 `GetThreadDescription` 没这个限制。
const LINUX_COMM_MAX: usize = 15;

/// 前缀 + 边界:`mullion-watchdog` 不匹配 `mullion-watchdog2`。
/// (与 F165 PDH 的 `pid_1234` vs `pid_12345` 同族陷阱。)
///
/// 第二条分支认 Linux 的截断名:表里三个前缀超过 15 字节,不认的话
/// tokio/watchdog/dialog 三组在 Linux 上恒为 0 且完全静默。
/// **代价说清楚**:截断之后 `mullion-watchdog2` 也变成 `mullion-watchdo`,
/// Linux 上串号防护随之失效 —— 那是内核已经把信息丢了,客户端补不回来;
/// Windows(唯一一等公民)拿到的是完整名,防护完好。
fn prefix_matches(name: &str, prefix: &str) -> bool {
    if let Some(rest) = name.strip_prefix(prefix) {
        return !rest.starts_with(|c: char| c.is_ascii_alphanumeric());
    }
    // Linux 截断名:整条名字恰好是前缀的前 15 字节。
    match prefix.get(..LINUX_COMM_MAX) {
        Some(head) => name == head,
        None => false,
    }
}

/// F168:线程分组结果。求和表(常开,进剖面行)与未映射原名表(只在 Debug
/// 档打)分开返回 —— 前者是给人扫的固定几行,后者是防「列举式分组表漏了
/// 一个新线程」时能在日志里认出丢在哪,混进同一份输出会让常开那行被
/// 长度不定的原名列表拖到没法一眼看完。
pub struct ThreadGroups {
    /// 固定顺序:表内各组 + 末尾「其他」。0 也在列 —— watchdog:0% 是信息,
    /// 省略掉的话「这个窗口 watchdog 确实没占用」和「压根没这一组」分不清。
    pub groups: Vec<(&'static str, u32)>,
    /// 落进「其他」的原名(空名记作 `unnamed`),Debug 档打出来。
    pub unmapped: Vec<(String, u32)>,
}

/// F168:把逐线程 `(名字, CPU%)` 聚合成固定几组。
///
/// 输出顺序固定为 `THREAD_GROUPS` 的表内顺序 + 「其他」,不随输入顺序或
/// 命中与否变化 —— 剖面行要能直接 diff 两次运行,顺序漂了就没法看。
/// 未命中前缀表的线程(包括空名,记作 `unnamed`)计入「其他」的和,
/// 原名连同各自的百分比另存进 `unmapped` 供 Debug 档核对分组表是否漏项。
pub fn group_threads(threads: &[(String, u32)]) -> ThreadGroups {
    let mut sums = vec![0u32; THREAD_GROUPS.len()];
    let mut other = 0u32;
    let mut unmapped = Vec::new();
    for (name, pct) in threads {
        match THREAD_GROUPS
            .iter()
            .position(|(p, _)| prefix_matches(name, p))
        {
            Some(i) => sums[i] = sums[i].saturating_add(*pct),
            None => {
                other = other.saturating_add(*pct);
                let shown = if name.is_empty() { "unnamed" } else { name };
                unmapped.push((shown.to_string(), *pct));
            }
        }
    }
    let mut groups: Vec<(&'static str, u32)> = THREAD_GROUPS
        .iter()
        .zip(sums)
        .map(|((_, g), v)| (*g, v))
        .collect();
    groups.push(("其他", other));
    ThreadGroups { groups, unmapped }
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
            band_dirty: 3,
            band_total: 60,
            band_segments: 2,
            fp_hit: 250,
            fp_miss: 50,
            wakes: 340,
            rr_sched: 40,
            rr_evt: 7,
            dirty_sites: vec![(8365, 300), (7191, 2)],
            dirty_other: 5,
            wev_kinds: vec![(13, 300), (10, 2)],
            wev_other: 1,
            cursor_dup: 298,
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

    /// F175:阶段段必须同时报 p50 和 p95。
    ///
    /// 桶是 log2 的,单看 p95 定不出总账:`window_event=937x/p95=1.0ms` 意味着
    /// 这一窗口花在窗口事件上的时间在 0 到 19% 之间的**任何位置**都说得通,
    /// 而「该去掉帧还是该去掉这一趟处理」正好取决于落在区间哪一端。
    ///
    /// 自证会变红:把 `stage_part` 里的 `/p50={}` 那一截删掉。
    #[test]
    fn a_stage_reports_both_the_median_and_the_tail() {
        let mut s = busy_snapshot();
        // 绝大多数很快、偶尔卡一下 —— p50 与 p95 分家的典型形状。
        s.stage_us[crate::diag::Stage::WindowEvent as usize][bucket_of(50)] = 99;
        s.stage_us[crate::diag::Stage::WindowEvent as usize][bucket_of(1_000_000)] = 1;
        let line = render_line(&s).expect("忙窗口该有一行");
        let seg = line
            .split_whitespace()
            .find(|w| w.starts_with("window_event="))
            .unwrap_or_else(|| panic!("没报窗口事件阶段:{line}"));
        assert!(
            seg.contains("/p50=") && seg.contains("/p95="),
            "阶段段只报了一个分位数(`{seg}`)—— 光有 p95 时总账的可能区间宽到\
             没法据此决定优化方向"
        );
    }

    /// F175:喂 egui-winit 那一趟的耗时必须进剖面行,且「没采到」与「快到
    /// 量不出来」长得不一样。
    ///
    /// 这一段存在的全部理由是把 `window_event=` 那段**拆开**:它含路由判定、
    /// 终端分支、标脏,不拆就分不出窗口事件贵在哪。归成 `0us` 一类的话,
    /// 「埋点没接上」和「这一趟确实很快」在日志里没有区别 —— 而这两种情况
    /// 的下一步动作正好相反。
    ///
    /// 自证会变红:把 `egui_feed_part` 换成常量,或把 `n == 0` 那个分支删掉。
    #[test]
    fn the_egui_feed_cost_is_told_apart_from_never_measured() {
        let line = render_line(&busy_snapshot()).expect("忙窗口该有一行");
        assert!(
            line.contains("egui_feed=n/a"),
            "没采到样本却没报 n/a:{line}"
        );

        let mut measured = busy_snapshot();
        measured.egui_feed_us[bucket_of(300)] = 5;
        let line = render_line(&measured).expect("有一行");
        let seg = line
            .split_whitespace()
            .find(|w| w.starts_with("egui_feed="))
            .unwrap_or_else(|| panic!("没报喂 egui 的耗时:{line}"));
        assert!(seg.contains("5x"), "样本数不对:{seg}");
        assert!(!seg.contains("p50=0us"), "量到了却报 0:{seg}");
    }

    /// F175:`egui_feed=` 与 `egui_ev=` 是两件事,不能在日志里认串。
    ///
    /// 前者是「喂 egui 花了多久」(耗时),后者是「egui 收了几个事件」(计数)。
    /// 名字长得像,而 `line.contains("egui_ev=")` 这种前缀匹配会把两者混起来
    /// —— 本文件里就有几条守护是这么写的。
    #[test]
    fn the_egui_feed_segment_does_not_collide_with_the_egui_event_count() {
        let line = render_line(&busy_snapshot()).expect("有一行");
        let n = line
            .split_whitespace()
            .filter(|w| w.starts_with("egui_ev="))
            .count();
        assert_eq!(n, 1, "`egui_ev=` 前缀匹配到了 {n} 段,两个字段认串了:{line}");
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

    /// F172:行带差分的脏带数/总带数/连通段数必须进剖面行。
    ///
    /// 与上面那条同理,这是"顶点层差分悄悄退化回全量"的**唯一**运行期守护
    /// —— 判据写错时画面完全正确、测试全绿,只有这个比值顶到 1 才看得出来。
    /// `seg=` 则回答"16 行一带选得对不对"。
    ///
    /// 自证会变红:把 `render_line` 里 `bands=`/`seg=` 那一段删掉。
    #[test]
    fn the_band_diff_counts_reach_the_line() {
        let line = render_line(&busy_snapshot()).expect("忙窗口该有一行");
        assert!(line.contains("bands=3/60"), "没报脏带/总带数:{line}");
        assert!(line.contains("seg=2"), "没报连通段数:{line}");
    }

    /// 零脏带是**最有意义的那个读数**(全带命中、一帧顶点都没重建),
    /// 必须显式写出来 —— 省略掉的话"完美命中"与"这个版本忘了统计"在日志里
    /// 长得一模一样。
    ///
    /// 自证会变红:给 `render_line` 里的 `bands=` 段加上
    /// `if s.band_dirty > 0` 之类的条件。
    #[test]
    fn a_zero_dirty_band_count_is_printed_rather_than_omitted() {
        let mut s = busy_snapshot();
        s.band_dirty = 0;
        let line = render_line(&s).expect("忙窗口该有一行");
        assert!(line.contains("bands=0/60"), "全带命中被省略了:{line}");
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

    /// F171:事件类型必须以**短名**上日志,不是裸码。
    ///
    /// 印码的话读日志的人得回 `wev.rs` 查表才知道 13 是什么,而这一段的
    /// 全部用途就是在实机日志里一眼分出「幽灵指针事件」和「真有人在按键」
    /// —— 需要查表就等于没有归因。
    ///
    /// 自证会变红:把 `render_key_table` 的 `label` 换成 `|k| k.to_string()`。
    #[test]
    fn the_window_event_kinds_are_named_not_numbered() {
        let mut s = busy_snapshot();
        s.wev_kinds = vec![(
            crate::wev::kind_of(&winit::event::WindowEvent::CloseRequested),
            7,
        )];
        s.wev_other = 0;
        let line = render_line(&s).expect("忙窗口该有一行");
        assert!(line.contains("wev=close:7"), "事件类型没翻成短名:{line}");
    }

    /// F171:`wev=`/`curdup=` 与 `dirty=` **在同一行相邻**。
    ///
    /// 三段是一条因果链的三级(哪行置脏 ← 哪类事件 ← 是不是同一个坐标),
    /// 拆到不同行去就得跨行对齐时间窗口,实机日志里几百个窗口根本对不动。
    ///
    /// 自证会变红:把 `wev={} curdup={}` 挪到 profile.cpu 行。
    #[test]
    fn the_event_attribution_sits_next_to_the_dirty_sites() {
        let line = render_line(&busy_snapshot()).expect("忙窗口该有一行");
        let dirty = line.find("dirty=").expect("没有 dirty 段");
        let wev = line.find(" wev=").expect("没有 wev 段");
        let dup = line.find(" curdup=").expect("没有 curdup 段");
        assert!(dirty < wev && wev < dup, "三段没挨着排:{line}");
        assert!(
            line[dirty..dup].lines().count() == 1,
            "三段被拆到了不同行:{line}"
        );
    }

    /// F171:一次事件都没采到时报 `wev=-`,与 `dirty=-` 同款。
    ///
    /// 空串会跟「这窗口确实一次没响」长得一样,而这两者的下一步完全相反:
    /// 前者要回去查接线,后者说明自激已经被掐掉了。
    ///
    /// 自证会变红:把 `render_key_table` 的 `-` 占位改成空串。
    #[test]
    fn an_empty_event_table_is_told_apart_from_a_missing_one() {
        let mut s = busy_snapshot();
        s.wev_kinds = Vec::new();
        s.wev_other = 0;
        let line = render_line(&s).expect("忙窗口该有一行");
        assert!(line.contains("wev=- "), "空事件表没有占位符:{line}");
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
            wev_kinds: vec![(13, 300)],
            wev_other: 1,
            cursor_dup: 299,
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
        let lines = render_lines(&s, false);
        let cpu = lines
            .iter()
            .find(|l| l.starts_with("profile.cpu "))
            .expect("该有 cpu 行");
        assert!(cpu.contains("total=8% main=96%"), "行里没有 CPU:{cpu}");

        s.cpu_pct = None;
        s.main_cpu_pct = None;
        let lines = render_lines(&s, false);
        let cpu = lines
            .iter()
            .find(|l| l.starts_with("profile.cpu "))
            .expect("该有 cpu 行");
        assert!(
            cpu.contains("total=n/a"),
            "采不到时该报 n/a 而不是编一个 0:{cpu}"
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
        let lines = render_lines(&s, false);
        let gpu = lines
            .iter()
            .find(|l| l.starts_with("profile.gpu "))
            .expect("该有 gpu 行");
        assert!(gpu.contains("vram=n/a"), "采不到却报了数字:{gpu}");

        s.vram_mb = Some((123, 4096));
        let lines = render_lines(&s, false);
        let gpu = lines
            .iter()
            .find(|l| l.starts_with("profile.gpu "))
            .expect("该有 gpu 行");
        assert!(gpu.contains("vram=123/4096MB"), "显存没渲染出来:{gpu}");
    }

    /// 没采到 GPU 帧耗时时报 `n/a`,不是 `p50=0`。
    ///
    /// adapter 不支持 TIMESTAMP_QUERY 与「GPU 一帧只用了 0µs」是两回事,
    /// 后者还会让人以为渲染是免费的。
    ///
    /// 自证会变红:把 `gpu_frame_part` 的 `n == 0` 分支删掉。
    #[test]
    fn a_gpu_timer_that_never_reported_says_n_a_instead_of_zero() {
        let mut s = Snapshot::empty();
        s.window_ms = 5_000;
        s.frames = 10;
        let lines = render_lines(&s, false);
        let gpu = lines
            .iter()
            .find(|l| l.starts_with("profile.gpu "))
            .expect("该有 gpu 行");
        assert!(gpu.contains("frame=n/a"), "没采到却报了数字:{gpu}");

        // `bucket_of` 是本模块私有的,`mod tests` 里有 `use super::*` 直接可用。
        s.gpu_frame_us[bucket_of(2_000)] = 3;
        let lines = render_lines(&s, false);
        let gpu = lines
            .iter()
            .find(|l| l.starts_with("profile.gpu "))
            .expect("该有 gpu 行");
        assert!(gpu.contains("frame=3x/"), "采到了却没报:{gpu}");
    }

    /// F170:分层段三态必须两两不同 —— 「这台机器没有 INSIDE_PASSES」、
    /// 「有但这个窗口一帧没采到」、「有值」是三件事。
    ///
    /// 归成一类的话,老驱动机器上的日志和「分层功能坏了」长得一模一样,
    /// 排障时分不出该查驱动还是该查代码。
    ///
    /// adapter 特性探测那段(`Gpu::new`)无头环境测不了,但**这三态是纯函数**,
    /// 没理由跟着一起放过。
    ///
    /// 自证会变红:把 `!s.gpu_split_supported` 分支删掉(不支持时掉进 `0x`),
    /// 或把 `total(&s.gpu_term_us) == 0` 那条删掉(没采到时报 `term:0µs`)。
    #[test]
    fn the_gpu_split_tells_unsupported_apart_from_sampled_nothing() {
        let mut s = Snapshot::empty();
        s.window_ms = 5_000;
        s.frames = 10;
        let gpu_line = |s: &Snapshot| {
            render_lines(s, false)
                .into_iter()
                .find(|l| l.starts_with("profile.gpu "))
                .expect("该有 gpu 行")
        };

        // ① 不支持分层。
        let unsupported = gpu_line(&s);
        assert!(
            unsupported.contains("分层:n/a"),
            "不支持却没说 n/a:{unsupported}"
        );

        // ② 支持,但这个窗口一帧都没采到。
        s.gpu_split_supported = true;
        let idle = gpu_line(&s);
        assert!(idle.contains("分层:0x"), "支持但没采到该是 0x:{idle}");
        assert_ne!(unsupported, idle, "「不支持」与「没采到」长得一样了");

        // ③ 真的有值。
        s.gpu_term_us[bucket_of(800)] = 5;
        s.gpu_egui_us[bucket_of(400)] = 5;
        let sampled = gpu_line(&s);
        assert!(
            sampled.contains("term:") && sampled.contains("egui:"),
            "有样本却没列出两段:{sampled}"
        );
        assert!(
            !sampled.contains("分层:"),
            "有值时不该再出现降级占位:{sampled}"
        );
    }

    /// F167:多行契约 —— 空闲零行/前缀/五段移出概览/无内嵌换行/debug 行开关。
    ///
    /// 自证会变红:概览行忘删 `mem=` 段(移出那条),或 render_lines 用
    /// `\n`.join 拼成单串(无换行那条),或 debug 行忘了 gate(开关那条)。
    #[test]
    fn render_lines_contract() {
        assert!(
            render_lines(&Snapshot::empty(), false).is_empty(),
            "空闲必须零行"
        );
        let mut s = busy_snapshot();
        s.thread_available = true;
        s.thread_groups = vec![("tokio", 31), ("其他", 9)];
        s.thread_unmapped = vec![("wgpu-poll".to_string(), 5)];
        let lines = render_lines(&s, false);
        // 非 debug 档行数是确定的常量五行。用 `==` 而不是 `>=`:`>=` 对
        // 「意外多 push 了一行」完全失明(实测变异不会红)。
        assert_eq!(lines.len(), 5, "概览+load+cpu+mem+gpu 恰好五行:{lines:?}");
        for l in &lines {
            assert!(!l.contains('\n'), "多行 = 多条独立记录,单行内禁止换行: {l}");
        }
        let overview = &lines[0];
        assert!(overview.starts_with("profile "), "概览行前缀");
        for gone in ["mem=", "cpu=", "gpu=", "vram=", "gpu_us="] {
            assert!(!overview.contains(gone), "{gone} 该移去专属行了");
        }
        assert!(lines.iter().any(|l| l.starts_with("profile.load scene=")));
        assert!(lines
            .iter()
            .any(|l| l.starts_with("profile.cpu ") && l.contains("tokio:31%")));
        assert!(lines.iter().any(|l| l.starts_with("profile.mem ")));
        assert!(lines.iter().any(|l| l.starts_with("profile.gpu ")));
        assert!(
            !lines.iter().any(|l| l.starts_with("profile.cpu.unmapped")),
            "info 档不出 unmapped"
        );
        let dbg = render_lines(&s, true);
        assert!(
            dbg.iter()
                .any(|l| l.starts_with("profile.cpu.unmapped") && l.contains("wgpu-poll:5%")),
            "debug 档要能看见没进分组表的线程"
        );
    }

    fn detail(
        id: u32,
        in_bytes: u64,
        dirty_bands: u64,
        band_total: u32,
        frames: u64,
    ) -> PaneDetail {
        PaneDetail {
            id,
            in_bytes,
            dirty_bands,
            band_total,
            frames,
        }
    }

    /// F173：`profile.pane` 行要能指名道姓地答「静置时是谁在说话、动在哪一块」。
    ///
    /// 实机静置日志每 60 秒来一次 `in=3.9KB/s bands=4/72 seg=3`，那是三个 pane
    /// 拍平后的和。这条行拆开之后，「三个 pane 各自的 tmux status-line 在跳」
    /// 与「某一个 pane 在刷屏」才区分得开 —— 两者的修法完全不同。
    ///
    /// 自证会变红：把 `render_lines` 里 push pane 行的那一段删掉。
    #[test]
    fn the_pane_line_names_who_talked_and_which_bands_moved() {
        let mut s = busy_snapshot();
        s.pane_detail = vec![
            detail(2, 6656, 1 << 23, 24, 5),
            detail(1, 6656, (1 << 11) | (1 << 23), 24, 5),
        ];
        let line = render_lines(&s, false)
            .into_iter()
            .find(|l| l.starts_with("profile.pane "))
            .expect("没有 profile.pane 行");
        assert_eq!(
            line, "profile.pane p1 in=6.5KB@b11,b23/24 frames=5 | p2 in=6.5KB@b23/24 frames=5",
            "格式或排序不对"
        );
    }

    /// F173：一个 pane 都没动的窗口不出这行。
    ///
    /// 静置日志的价值一半在于**安静**：每 5 秒印一条 `profile.pane`（哪怕是空的）
    /// 会把「真的没人说话」淹掉，而那正是这条埋点要看的基线。
    ///
    /// 自证会变红：把渲染里的 `is_empty()` 早退删掉。
    #[test]
    fn a_window_where_no_pane_moved_has_no_pane_line() {
        let s = busy_snapshot();
        assert!(
            s.pane_detail.is_empty(),
            "前提：busy_snapshot 不带 per-pane"
        );
        assert!(
            !render_lines(&s, false)
                .iter()
                .any(|l| l.starts_with("profile.pane")),
            "没有 per-pane 读数时不许印空行"
        );
    }

    /// F173：零字节但重画过的 pane 也要出现，且脏带为空时印 `-` 而不是省略。
    ///
    /// 省略的话「这个 pane 一帧都没重建」（差分生效，好消息）与「它根本没被
    /// 记上」（埋点漏了，坏消息）在日志里长得一模一样 —— F167 踩过三次的
    /// 静默假零。
    #[test]
    fn a_pane_that_drew_nothing_dirty_still_shows_its_band_denominator() {
        let mut s = busy_snapshot();
        s.pane_detail = vec![detail(0, 0, 0, 24, 3)];
        let line = render_lines(&s, false)
            .into_iter()
            .find(|l| l.starts_with("profile.pane "))
            .expect("没有 profile.pane 行");
        assert_eq!(line, "profile.pane p0 in=0B@-/24 frames=3");
    }

    /// F173：脏带多到读不动时截断，但**必须说出截了多少**。
    ///
    /// 直接截断不留痕的话，「脏了 4 带」与「脏了 40 带」在日志里一样长 ——
    /// 而后者意味着差分基本失效，是这条埋点最该抓的那种情况。
    #[test]
    fn a_pane_with_too_many_dirty_bands_says_how_many_it_left_out() {
        let mut s = busy_snapshot();
        s.pane_detail = vec![detail(1, 0, 0b111_1111, 24, 1)];
        let line = render_lines(&s, false)
            .into_iter()
            .find(|l| l.starts_with("profile.pane "))
            .unwrap();
        assert!(
            line.contains("@b0,b1,b2,b3+3/24"),
            "截断没报剩余条数：{line}"
        );
    }

    /// F173：槽位用完之后落在外面的字节要报出来。
    ///
    /// 不报的话「表里这几个就是全部」与「还有一堆没槽位」看不出区别，
    /// 而后者说明归因表已经不可信了。
    #[test]
    fn bytes_that_missed_a_slot_are_reported_instead_of_vanishing() {
        let mut s = busy_snapshot();
        s.pane_detail = vec![detail(1, 100, 0, 24, 1)];
        s.pane_other_bytes = 2048;
        let line = render_lines(&s, false)
            .into_iter()
            .find(|l| l.starts_with("profile.pane "))
            .unwrap();
        assert!(line.ends_with("| 其他 in=2.0KB"), "溢出字节没报：{line}");
    }

    /// F168:采不到线程 ≠ 各组为 0。
    /// 自证会变红:把 cpu 行渲染里 thread_available 的分支删掉。
    #[test]
    fn an_unavailable_thread_probe_renders_na_not_zeros() {
        let mut s = busy_snapshot();
        s.thread_available = false;
        let lines = render_lines(&s, false);
        let cpu = lines
            .iter()
            .find(|l| l.starts_with("profile.cpu "))
            .unwrap();
        assert!(cpu.ends_with("| n/a"), "采不到必须是 n/a: {cpu}");
        assert!(!cpu.contains("tokio:"));
    }

    /// F169(偏离一):RSS 采不到(=0)不许编成 `0MB = … 其他:0` —— 一个跑着的
    /// 进程 RSS 不可能真的是 0,那是采样失败被揉成 0(`diag.rs` 的既有债,
    /// 本任务不改那处,只在渲染层挡住这个假零值)。必须显式报 n/a,与
    /// CPU/GPU/vram 的"采不到"处置同一套纪律。
    ///
    /// 自证会变红:把 mem 行渲染里 `s.mem_process_mb == 0` 的门删掉。
    #[test]
    fn an_unmeasured_rss_is_told_apart_from_a_process_that_uses_nothing() {
        let mut s = busy_snapshot();
        s.mem_process_mb = 0;
        let lines = render_lines(&s, false);
        let mem = lines
            .iter()
            .find(|l| l.starts_with("profile.mem "))
            .expect("该有 mem 行");
        assert!(mem.contains("n/a"), "RSS 采不到该报 n/a:{mem}");
        assert!(
            !mem.contains("0MB ="),
            "RSS 采不到不许编成 0MB 的记账:{mem}"
        );
    }

    /// 迁移垫片:概览行就是 `render_lines` 的第一行。既有那批只关心
    /// 概览行内容的测试沿用它,避免为了改签名去动一堆与本任务无关的断言。
    fn render_line(s: &Snapshot) -> Option<String> {
        render_lines(s, false).into_iter().next()
    }

    /// 一条样本的直方图(桶 0 计 1)。场景判据只看 total>0,落哪个桶无关。
    fn one_sample() -> Counts {
        let mut c = [0u64; BUCKETS];
        c[0] = 1;
        c
    }

    /// F167:场景优先级。并发时取优先级最高的单值;涓流不算 remote-output。
    ///
    /// 自证会变红:把 scene_of 里 sftp 与 scrollback 两个 if 对调,或把
    /// `>= REMOTE_OUTPUT_BPS` 改成 `>`(边界值那条会抓住)。
    #[test]
    fn scene_priority_and_the_trickle_threshold() {
        let mut s = Snapshot::empty();
        s.window_ms = 5000;
        assert_eq!(scene_of(&s), Scene::Idle);
        s.frames = 10;
        assert_eq!(scene_of(&s), Scene::UiOnly);
        s.connects_ok = 1;
        assert_eq!(scene_of(&s), Scene::Connecting);
        // 阈值两侧:5 秒窗口,1024 B/s 阈值 → 5120 字节是分界。
        s.inbound_bytes = 5119;
        assert_eq!(scene_of(&s), Scene::Connecting, "涓流不算远端刷屏");
        s.inbound_bytes = 5120;
        assert_eq!(scene_of(&s), Scene::RemoteOutput);
        s.keys = 1;
        assert_eq!(scene_of(&s), Scene::Typing);
        s.stage_us[crate::diag::Stage::Resize as usize] = one_sample();
        assert_eq!(scene_of(&s), Scene::Resize);
        s.scroll_events = 1;
        assert_eq!(scene_of(&s), Scene::Scrollback);
        s.xfer_jobs = 1;
        assert_eq!(
            scene_of(&s),
            Scene::SftpTransfer,
            "传输+打字+滚动并发时传输最优先"
        );
    }

    /// F167/F158:写盘了却归不出因 ≠ 空闲。
    ///
    /// 判据集合对不齐是这里唯一的坑:`is_idle()` 有 14 条判据,活动判据只有
    /// 七条,差集里的窗口(烧 CPU / 全被节流 / 同步块超时)必须落进
    /// `Unattributed`,而不是被冒充成 `Idle`。
    ///
    /// 自证会变红:把 `scene_of` 结尾的 `if !s.is_idle()` 那段删掉。
    #[test]
    fn a_window_that_burns_cpu_without_frames_is_not_called_idle() {
        let mut s = Snapshot::empty();
        s.window_ms = 5000;
        s.main_cpu_pct = Some(96);
        assert!(!s.is_idle(), "前提:这一行会写盘");
        assert_eq!(scene_of(&s), Scene::Unattributed);
        // 同族:重绘全被帧闸挡下、T2 同步块超时,都归不出因但都得写盘。
        let mut t = Snapshot::empty();
        t.window_ms = 5000;
        t.throttled = 7;
        assert_eq!(scene_of(&t), Scene::Unattributed);
        let mut u = Snapshot::empty();
        u.window_ms = 5000;
        u.sync_timeouts = 1;
        assert_eq!(scene_of(&u), Scene::Unattributed);
        // 真空闲仍是 Idle(纯函数对任意输入有定义)。
        let mut z = Snapshot::empty();
        z.window_ms = 5000;
        assert!(z.is_idle());
        assert_eq!(scene_of(&z), Scene::Idle);
    }

    /// F168:分组表 + 四个坑:前缀串号 / 空名 / 未匹配进其他但 Debug 可见 /
    /// Linux 15 字节截断名。
    ///
    /// 自证会变红:把 prefix_matches 改成裸 starts_with(串号那条),或把
    /// 空名分支删掉(unnamed 那条),或把 thread_group_pct 加 .min(100)
    /// (超 100% 那条 —— 组内多线程烧多核是常态,封顶就看不见了),或删掉
    /// prefix_matches 里认截断名的那个分支(截断那组断言变红),或把饱和转换
    /// 换回裸 `as u32`(爆表那条)。
    #[test]
    fn thread_grouping_boundaries_and_the_uncapped_pct() {
        let threads = vec![
            ("tokio-runtime-worker".to_string(), 150u32),
            ("tokio-runtime-worker".to_string(), 90u32),
            ("mullion-watchdog".to_string(), 1u32),
            ("mullion-watchdog2".to_string(), 7u32), // 串号陷阱:不是 watchdog
            ("".to_string(), 3u32),                  // 空名(Windows 未命名线程)
            ("wgpu-poll".to_string(), 5u32),
        ];
        let g = group_threads(&threads);
        let get = |name: &str| g.groups.iter().find(|(n, _)| *n == name).unwrap().1;
        assert_eq!(get("tokio"), 240, "同组求和,且允许超 100%");
        assert_eq!(get("watchdog"), 1, "watchdog2 不许被前缀串进来");
        assert_eq!(get("其他"), 7 + 3 + 5);
        let unmapped: Vec<&str> = g.unmapped.iter().map(|(n, _)| n.as_str()).collect();
        assert!(unmapped.contains(&"mullion-watchdog2"));
        assert!(unmapped.contains(&"unnamed"), "空名要有占位标识");
        assert!(unmapped.contains(&"wgpu-poll"));
        // 不封顶换算:5 秒窗口烧了 12 秒 CPU(多线程组)= 240%。
        assert_eq!(thread_group_pct(12_000_000_000, 5_000_000_000), Some(240));
        assert_eq!(thread_group_pct(1, 0), None, "窗口为 0 = 采不到,不是 0%");
        // 荒谬输入必须爆表,不许截断绕回:12.88 秒 / 3ns 恰好整除 2^32,
        // `as u32` 会静默给出 0 —— 一个「看着完全正常」的错值。
        assert_eq!(
            thread_group_pct(12_884_901_888, 3),
            Some(u32::MAX),
            "截断绕回会把爆表读数伪装成 0"
        );

        // Linux 内核把线程名截断到 15 字节(本机实测),表里三个前缀超长。
        // 不认截断名 = tokio/watchdog/dialog 三组在 Linux 上恒零且静默。
        let truncated = vec![
            ("tokio-runtime-w".to_string(), 40u32),
            ("mullion-watchdo".to_string(), 2u32),
            ("mullion-file-di".to_string(), 3u32),
            ("mullion-dragout".to_string(), 4u32),
        ];
        let t = group_threads(&truncated);
        let tget = |name: &str| t.groups.iter().find(|(n, _)| *n == name).unwrap().1;
        assert_eq!(tget("tokio"), 40, "Linux 截断名必须能认出来");
        assert_eq!(tget("watchdog"), 2);
        assert_eq!(tget("dialog"), 3);
        assert_eq!(tget("dragout"), 4);
        assert_eq!(tget("其他"), 0, "截断名不该漏进其他");
        assert!(t.unmapped.is_empty());
    }

    /// F169:余量三态 —— 正常 / 全零 / 负余量报超出而不是负数。
    ///
    /// 自证会变红:把负余量分支改成静默夹 0(`saturating_sub` 一把梭),
    /// 「超出」字样那条断言会抓住;把分支判据 `<=` 写成 `<`,边界那条会抓住。
    #[test]
    fn mem_parts_reports_the_remainder_honestly() {
        use crate::diag::MemKind;
        // 正常:340 = 128 + 0 + 16 + 196。
        assert_eq!(
            mem_parts(MemKind::Rss, 340, None, 128 << 20, 0, 16 << 20),
            "rss=340MB = scroll:128 xfer:0 text:16 其他:196"
        );
        // 全零记账:全进其他。
        assert_eq!(
            mem_parts(MemKind::Rss, 50, None, 0, 0, 0),
            "rss=50MB = scroll:0 xfer:0 text:0 其他:50"
        );
        // 负余量:记账 168MB > 主数 100MB,超出 68 要显式打出来。
        assert_eq!(
            mem_parts(MemKind::Rss, 100, None, 128 << 20, 24 << 20, 16 << 20),
            "rss=100MB = scroll:128 xfer:24 text:16 其他:0(记账超出rss 68MB)"
        );
        // 分支边界:记账恰好等于主数。余量 0 是**如实的 0**,不是「超出 0MB」
        // —— 少了这条,把 `<=` 写成 `<` 三条断言全不变红(恒绿缺口)。
        assert_eq!(
            mem_parts(MemKind::Rss, 144, None, 128 << 20, 0, 16 << 20),
            "rss=144MB = scroll:128 xfer:0 text:16 其他:0"
        );
    }

    /// F176:Windows 形态 —— 主数是 commit,ws 括在后面做交叉核对。
    ///
    /// 数字取自 N5 切片的实机日志(428MB commit / 289MB 专用工作集,
    /// 同一时刻),不是编的。
    #[test]
    fn mem_parts_renders_commit_and_ws_on_windows() {
        assert_eq!(
            mem_parts(crate::diag::MemKind::Commit, 428, Some(289), 0, 0, 5 << 20),
            "commit=428MB(ws 289) = scroll:0 xfer:0 text:5 其他:423"
        );
    }

    /// F176:Linux 形态 —— 主数是 rss,**不印** `(ws …)`。
    ///
    /// 自证会变红:让 `MemKind::Rss` 也走 `(ws n/a)` 那条分支。
    #[test]
    fn mem_parts_renders_a_single_number_when_there_is_no_working_set() {
        assert_eq!(
            mem_parts(crate::diag::MemKind::Rss, 155, None, 0, 0, 5 << 20),
            "rss=155MB = scroll:0 xfer:0 text:5 其他:150"
        );
    }

    /// F176:Windows 老系统回落到 `EX` 之后 ws 采不到 —— 印 `n/a`,
    /// 不许静默印 0(印 0 会被读成「专用工作集真的是 0」)。
    #[test]
    fn mem_parts_says_n_a_when_the_working_set_could_not_be_sampled() {
        assert_eq!(
            mem_parts(crate::diag::MemKind::Commit, 428, None, 0, 0, 5 << 20),
            "commit=428MB(ws n/a) = scroll:0 xfer:0 text:5 其他:423"
        );
    }

    /// F176 的承重条:**减法算在主数(commit)上,不算在 ws 上**。
    ///
    /// 喂一个 ws(100) < 记账(168) < commit(400) 的组合:算 commit 时余量
    /// 232、正常分支;算 ws 时会走「记账超出」。断言必须落在正常分支上。
    ///
    /// 自证会变红:把 `mem_parts` 里的被减数换成 `ws_mb`。这正是这段代码
    /// 日后最可能被「顺手统一成一个数」重构掉的方式,而那么改之后日志照写、
    /// 数字照有,只在用户最小化窗口时才暴露。
    #[test]
    fn the_remainder_is_computed_against_commit_not_the_working_set() {
        assert_eq!(
            mem_parts(
                crate::diag::MemKind::Commit,
                400,
                Some(100),
                128 << 20,
                24 << 20,
                16 << 20
            ),
            "commit=400MB(ws 100) = scroll:128 xfer:24 text:16 其他:232"
        );
    }

    /// F176:`profile.mem` 行按快照的口径渲染,两个新字段确实接到了行上。
    ///
    /// 自证会变红:把 `render_lines` 里传给 `mem_parts` 的 `s.mem_ws_mb`
    /// 写死成 `None` —— 行里会变成 `(ws n/a)`,断言当场抓住。
    #[test]
    fn the_mem_line_carries_the_working_set_from_the_snapshot() {
        let mut s = busy_snapshot();
        s.mem_process_mb = 428;
        s.mem_kind = crate::diag::MemKind::Commit;
        s.mem_ws_mb = Some(289);
        s.mem_scroll_bytes = 0;
        s.xfer_running = 0;
        s.mem_text_bytes = 5 << 20;
        let lines = render_lines(&s, false);
        let mem = lines
            .iter()
            .find(|l| l.starts_with("profile.mem "))
            .expect("应有 profile.mem 行");
        assert_eq!(
            mem,
            "profile.mem commit=428MB(ws 289) = scroll:0 xfer:0 text:5 其他:423"
        );
    }

    /// F177 的分母:`Snapshot::mem_other_mb` 与 `profile.mem` 行里的
    /// `其他:` **必须同源**。两处各算一遍的话,预算闸报的数和日志上一行
    /// 报的数会对不上,而没有任何东西会报错。
    ///
    /// 自证会变红:把 `mem_other_mb` 改成直接返回 `mem_process_mb`。
    #[test]
    fn the_other_bucket_is_the_same_number_in_the_line_and_in_the_alarm() {
        let mut s = busy_snapshot();
        s.mem_process_mb = 428;
        s.mem_kind = crate::diag::MemKind::Commit;
        s.mem_ws_mb = Some(289);
        s.mem_scroll_bytes = 0;
        s.xfer_running = 0;
        s.mem_text_bytes = 5 << 20;
        assert_eq!(s.mem_other_mb(), 423);
        let lines = render_lines(&s, false);
        assert!(lines
            .iter()
            .any(|l| l.starts_with("profile.mem ") && l.ends_with("其他:423")));
    }
}
