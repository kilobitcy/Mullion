//! 运行期自诊断:阶段打点 + 看门狗 + 内存/指标采样 + 启动环境快照。
//!
//! 目标只有一个:**卡死之后,只看 `mullion.log` 就能说出卡在哪一步**。
//! 起因是一次 Windows 11 真机卡死——日志停在 `Resized(0x0)` 之后什么都没有,
//! 而 Windows 事件日志里连 mullion 的名字都没有(GUI 进程挂起它不记录)。
//! 光凭「日志断了」只能知道事件循环不动了,说不出是卡在 acquire、egui 还是驱动。
//!
//! 做法:主线程每换一个阶段就写一次 `AtomicU8` + 时间戳(开销 = 两条 relaxed 原子写,
//! 常开无妨);独立看门狗线程每秒检查一次,发现「非 Idle 阶段超过阈值没推进」就落一条
//! WARN,带上卡住的阶段名与内存快照,并按 2 倍间隔持续复报。
//!
//! `Stage::Idle` 表示「事件循环正常阻塞等事件」——那是常态,看门狗必须忽略,
//! 否则空闲一分钟就误报。

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// 主线程当前所处的阶段。顺序即 `as u8` 编码,勿随意插入中间项。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Stage {
    /// 阻塞等待事件(正常空闲)。看门狗忽略这个阶段。
    Idle = 0,
    /// 窗口创建 / adapter 枚举 / device 请求。AMD 驱动出问题时常卡在这里。
    Startup,
    WindowEvent,
    UserEvent,
    /// 排空 rx → feed emulator → 回写 PtyWrite(T1)。
    Pump,
    /// surface configure + grid 传播。
    Resize,
    EguiRun,
    TextPrepare,
    /// `surface.get_current_texture()`。
    Acquire,
    /// 录制命令 + submit。
    Encode,
    /// `frame.present()`。
    Present,
    /// 会话库读写(keyring / TOML)。
    StoreIo,
}

/// `Stage` 一共有几个变体。`STAGE_US` 与后续的剖面快照都按它定长。
pub const STAGE_COUNT: usize = 12;

const STAGE_NAMES: [&str; STAGE_COUNT] = [
    "idle",
    "startup",
    "window_event",
    "user_event",
    "pump",
    "resize",
    "egui_run",
    "text_prepare",
    "acquire",
    "encode",
    "present",
    "store_io",
];

pub fn stage_name(raw: u8) -> &'static str {
    STAGE_NAMES.get(raw as usize).copied().unwrap_or("?")
}

/// 阶段时钟:当前阶段 + 上次换阶段的时刻 + 每阶段的驻留时长分布。
///
/// 抽成结构体(而不是三个裸 `static`)只为一件事:**测试能自己造一个**。
/// 进程级 static 是全 crate 共享的 —— `host_key::tests` 之类的测试会间接
/// 调到 `mark()`,在并行 runner 下偷走 prev/since 这一对,让本文件的断言
/// 概率性假红。给测试一个自己的实例,这类干扰整类消失,顺带连 `sleep`
/// 都不需要了(时刻由调用方传进来)。
struct StageClock {
    stage: AtomicU8,
    beat_us: AtomicU64,
    /// 每个阶段的驻留时长分布。索引 = `Stage as usize`。
    ///
    /// **白得的**:`mark()` 本来就铺满了事件循环,离开一个阶段时顺手把这一趟
    /// 的时长记进去,就有了「pump 慢还是 present 慢」的答案,不需要任何新插桩点。
    hist: [crate::profile::Histogram; STAGE_COUNT],
}

impl StageClock {
    const fn new() -> Self {
        Self {
            stage: AtomicU8::new(Stage::Idle as u8),
            beat_us: AtomicU64::new(0),
            hist: [const { crate::profile::Histogram::new() }; STAGE_COUNT],
        }
    }

    /// 进入 `stage`,并把**刚刚结束的那个阶段**的驻留时长记进它自己的桶。
    ///
    /// `now_us` 由调用方给:生产里是 `elapsed_us()`,测试里是常量 —— 于是
    /// 这段逻辑测起来既不依赖时钟也不依赖 sleep。
    ///
    /// **单写者**:生产环境里只有主线程(事件循环)调用,看门狗线程只读。
    /// 两次 `swap` 因此不需要凑成一次原子快照 —— 多个线程并发调用会互相
    /// 偷走 prev/since 这一对。
    fn mark(&self, stage: Stage, now_us: u64) {
        let prev = self.stage.swap(stage as u8, Ordering::Relaxed);
        let since = self.beat_us.swap(now_us, Ordering::Relaxed);
        // `Idle` 不计时:阻塞等事件本来就可以很久,算进去会让直方图被一个
        // 几十秒的样本主导,其余全淹掉。
        if prev != Stage::Idle as u8 {
            if let Some(h) = self.hist.get(prev as usize) {
                h.record_us(now_us.saturating_sub(since));
            }
        }
    }
}

static CLOCK: StageClock = StageClock::new();
static ORIGIN: OnceLock<Instant> = OnceLock::new();
static STARTED: AtomicBool = AtomicBool::new(false);

// 指标(既是性能基线,也是心跳:卡死后能看出最后一次成功 present 是几秒前)。
static FRAMES: AtomicU64 = AtomicU64::new(0);
static PRESENTS: AtomicU64 = AtomicU64::new(0);
static SKIPPED: AtomicU64 = AtomicU64::new(0);
static INBOUND_BYTES: AtomicU64 = AtomicU64::new(0);

// F155 剖面用的计数器。采集端只做原子加法 —— 帧路径上做别的就是 T3。
static REDRAW_TERMINAL: AtomicU64 = AtomicU64::new(0);
static REDRAW_UI: AtomicU64 = AtomicU64::new(0);
static REDRAW_BOTH: AtomicU64 = AtomicU64::new(0);
static THROTTLED: AtomicU64 = AtomicU64::new(0);
static KEYS: AtomicU64 = AtomicU64::new(0);
static CONNECTS_OK: AtomicU64 = AtomicU64::new(0);
static CONNECTS_ERR: AtomicU64 = AtomicU64::new(0);
static RECONNECTS: AtomicU64 = AtomicU64::new(0);
static SFTP_OPS: AtomicU64 = AtomicU64::new(0);
/// F155/T2 剖面:各 pane 攒帧状态机报上来的同步块计数(领域陷阱 T2 —— 历史上
/// 「打字慢一拍」的真根因就是这里的超时收口)。
static SYNC_BLOCKS: AtomicU64 = AtomicU64::new(0);
static SYNC_TIMEOUTS: AtomicU64 = AtomicU64::new(0);
/// F12 剖面:整形缓存这一窗口命中/未命中了多少行。
static RESHAPE_HIT: AtomicU64 = AtomicU64::new(0);
static RESHAPE_MISS: AtomicU64 = AtomicU64::new(0);

/// F157:本窗口收到多少次 `RedrawRequested`。唤醒率的直接读数。
static WAKES: AtomicU64 = AtomicU64::new(0);
/// F157:我们主动 `request_redraw` 的次数,按来源分。
static RR_SCHED: AtomicU64 = AtomicU64::new(0);
static RR_EVT: AtomicU64 = AtomicU64::new(0);
/// F157:喂给 egui 的事件总数 / 其中有事件的帧数。
static EGUI_EVENTS: AtomicU64 = AtomicU64::new(0);
static EGUI_EVENT_FRAMES: AtomicU64 = AtomicU64::new(0);
/// F157:`repaint_delay` 三分桶(见 `profile::repaint_bucket`)。
static RDELAY_ZERO: AtomicU64 = AtomicU64::new(0);
static RDELAY_FINITE: AtomicU64 = AtomicU64::new(0);
static RDELAY_MAX: AtomicU64 = AtomicU64::new(0);
/// F159:整帧指纹命中/未命中的帧数。
static FP_HIT: AtomicU64 = AtomicU64::new(0);
static FP_MISS: AtomicU64 = AtomicU64::new(0);

/// F157:归因表的槽位数。`ui_dirty` 置脏点表与 F171 的窗口事件类型表共用。
///
/// 8 而不是 75(置脏点总数):空闲时真正在响的只有个位数处,而这张表要待在
/// 帧路径上——`note()` 最坏两轮有界扫描(至多 24 次 relaxed load),加一次
/// store 或 fetch_add 收尾,与 `diag::mark` 同量级。
pub const TABLE_SLOTS: usize = 8;

/// 「某个键响了多少次」的归因表:定长、无锁、不分配、不格式化(T3)。
///
/// 两个实例:`DIRTY`(键 = `app.rs` 的源码行号,F157)与 `WEV`(键 = 窗口事件
/// 类型码,F171)。键的语义由使用方定,这里只管计数与槽位回收 —— 复制一份
/// 结构出来的话,「槽位归还」这条微妙的时序约定就要维护两遍。
///
/// 抽成结构体(而不是一组裸 `static`)只为一件事:**测试能自己造一个**。
/// 进程级 static 是全 crate 共享的 —— 并行 runner 下别的测试会间接调到
/// `note_ui_dirty`,共享状态会让本文件的断言概率性假红(与 `StageClock`
/// 同一条理由)。
struct KeyTable {
    /// 槽位认领的键。**0 = 空槽**,所以任何键空间都不许把 0 用作真实键
    /// (F171 的 `wev::kind_of` 因此从 1 起编,`line!()` 天然不会是 0)。
    key: [AtomicU32; TABLE_SLOTS],
    hits: [AtomicU64; TABLE_SLOTS],
    /// 槽位用完之后落到这里,报成 `dirty=...,other:N`。**必须报**:
    /// 不报的话「这几处就是全部」与「还有一堆没槽位」在日志里长得一样。
    other: AtomicU64,
}

impl KeyTable {
    const fn new() -> Self {
        Self {
            key: [const { AtomicU32::new(0) }; TABLE_SLOTS],
            hits: [const { AtomicU64::new(0) }; TABLE_SLOTS],
            other: AtomicU64::new(0),
        }
    }

    /// 记一次「键 `key` 响了」(F157:某一行置了脏;F171:收到某一类窗口事件)。
    ///
    /// **单写者**:生产环境里只有主线程经 `mark_ui_dirty!` / `note_window_event`
    /// 调用,看门狗线程只读 `drain()`——不需要 CAS 重试,与本文件
    /// `StageClock::mark` 同一条理由。
    ///
    /// 两轮线性扫:第一轮找精确命中,直接加计数并返回。找不到命中,第二轮
    /// 抢一个「空槽,或者这一窗口还一次没响过的槽位」——**关键在后半句**:
    /// 一个槽位挂着上一窗口的键,但这一窗口(`drain` 还没取走它)一次都
    /// 没加过,说明那一处已经安静了,原地把它让给新来的键更有用,不必等
    /// 到下一次 `drain` 才收回——否则「上一窗口才响过一次的启动尖峰」会把
    /// 槽位多占一整窗口,恰好把这一窗口真正常驻的那一处挤进 `other`
    /// (`a_slot_that_went_quiet_is_handed_back_for_the_next_window` 钉的
    /// 就是这一条)。
    fn note(&self, key: u32) {
        for i in 0..TABLE_SLOTS {
            if self.key[i].load(Ordering::Relaxed) == key {
                self.hits[i].fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
        for i in 0..TABLE_SLOTS {
            let quiet = self.key[i].load(Ordering::Relaxed) == 0
                || self.hits[i].load(Ordering::Relaxed) == 0;
            if quiet {
                self.key[i].store(key, Ordering::Relaxed);
                self.hits[i].store(1, Ordering::Relaxed);
                return;
            }
        }
        self.other.fetch_add(1, Ordering::Relaxed);
    }

    /// 取走这一窗口的内容。
    ///
    /// **这一窗口一次没响过的槽位要还回去**:不还的话,启动那几秒里各来
    /// 一次的一次性置脏点会把 8 个槽位永久占死,而真正每帧都在置脏的那
    /// 一处只能落进 `other` —— 一整趟实机往返白跑,且剖面看起来一切正常。
    ///
    /// **顺序是承重的:必须先读 `key` 再 `swap(hits)`,反过来会报错键**。
    /// `note()` 跑在主线程,`drain()` 跑在看门狗线程,两者各自只是普通
    /// relaxed 读写,没有加锁把「一个槽位的 key 和 hits」锁成一份快照。
    /// 若先 `swap(hits)`:swap 完 `hits[i]` 已经是 0,`note()` 在这个缝隙里
    /// 把这个槽位当成安静槽抢走、写了新键,drain 再去读 `key[i]` 时读到
    /// 的就是抢家的键——报出来的是「新键命中了旧键积攒的次数」,
    /// 正是这张表存在的意义要防的那种「归因指错人」。反过来先读 `key`:
    /// `note()` 的抢占分支只在看见 `hits[i]==0` 之后才会发生,而这必然发生
    /// 在 drain 的 `swap` 之后——drain 读 `key` 的那一刻,`note()` 还没有
    /// 理由去抢这个槽,读到的必然是这一批 hits 归属的那个键。唯一残留的
    /// 缝隙是「抢占落在 load 和 swap 之间,而被抢的键本来就是 0 次命中」,
    /// 此时最多错记一次命中,可接受。
    fn drain(&self) -> (Vec<(u32, u64)>, u64) {
        let mut out = Vec::new();
        for i in 0..TABLE_SLOTS {
            let key = self.key[i].load(Ordering::Relaxed);
            let hits = self.hits[i].swap(0, Ordering::Relaxed);
            if hits > 0 {
                out.push((key, hits));
            } else {
                self.key[i].store(0, Ordering::Relaxed);
            }
        }
        (out, self.other.swap(0, Ordering::Relaxed))
    }
}

static DIRTY: KeyTable = KeyTable::new();
/// F171:窗口事件类型归因表。键 = `wev::kind_of` 的码。
static WEV: KeyTable = KeyTable::new();

/// F171:指针位置去重。
///
/// 只知道「凶手是 `CursorMoved`」还不够:坐标恒定不变说明那是**幽灵事件**
/// (可以按位置直接掐掉),坐标在变说明真有东西在动指针 —— 两者修法完全
/// 不同,而这一位信息几乎零成本。
///
/// 判据用 **bit 相等**而不是数值相等:更保守,只会漏报 dup、不会误报
/// (`-0.0` 与 `0.0` 的 bits 不同,会被判成「动过」)。误报 dup 才是危险的
/// ——那会让人以为「掐掉重复坐标是安全的」。
struct CursorTracker {
    last: [AtomicU64; 2],
    /// 有没有收到过第一次。**不能拿 `last == (0, 0)` 当哨兵**:指针真的
    /// 停在窗口原点时会被当成「还没收到过」,永远记不出 dup。
    seen: AtomicBool,
    dup: AtomicU64,
}

impl CursorTracker {
    const fn new() -> Self {
        Self {
            last: [const { AtomicU64::new(0) }; 2],
            seen: AtomicBool::new(false),
            dup: AtomicU64::new(0),
        }
    }

    /// 记一次 `CursorMoved`。返回是否与上一次同坐标(便于测试直接断言)。
    fn note(&self, x: f64, y: f64) -> bool {
        let (bx, by) = (x.to_bits(), y.to_bits());
        let same = self.seen.load(Ordering::Relaxed)
            && self.last[0].load(Ordering::Relaxed) == bx
            && self.last[1].load(Ordering::Relaxed) == by;
        self.last[0].store(bx, Ordering::Relaxed);
        self.last[1].store(by, Ordering::Relaxed);
        self.seen.store(true, Ordering::Relaxed);
        if same {
            self.dup.fetch_add(1, Ordering::Relaxed);
        }
        same
    }

    /// 取走这一窗口的 dup 计数。
    ///
    /// **`last`/`seen` 刻意不重置**:指针在两个 5 秒采样窗口之间没动过,
    /// 那仍然是 dup。重置的话每个窗口的第一次 `CursorMoved` 都会被算成
    /// 「动过」,采样窗口越密、dup 率被压得越低 —— 一个随采样频率变化的
    /// 指标是没法用来下结论的。
    fn drain(&self) -> u64 {
        self.dup.swap(0, Ordering::Relaxed)
    }
}

static CURSOR: CursorTracker = CursorTracker::new();

/// F157:第 `line` 行把 `ui_dirty` 置真了。**只由 `mark_ui_dirty!` 宏调用。**
///
/// 只收行号、不收文件名:所有置脏点都在 `app.rs`(由
/// `app::tests::every_ui_dirty_set_site_goes_through_the_attribution_macro`
/// 钉死)。存文件名要么存 `&'static str` 的裸指针(不安全还原),要么加锁
/// ——帧路径上两者都不行(T3)。
pub fn note_ui_dirty(line: u32) {
    DIRTY.note(line);
}

/// F171:某一类窗口事件让 egui 说「要重绘」。`kind` 来自 `wev::kind_of`。
///
/// **埋在 `resp.repaint` 之内,不是收到事件就记**:这张表要回答的是
/// 「凭什么出帧」,不是「收到了什么」。两者的差集(收到但不触发重绘的)
/// 由既有的 `window_event` Stage 计数与 `egui_ev` 反推得到。
pub fn note_window_event(kind: u32) {
    WEV.note(kind);
}

/// F171:一次 `CursorMoved` 的物理坐标。判据与理由见 `CursorTracker`。
pub fn note_cursor_pos(x: f64, y: f64) {
    CURSOR.note(x, y);
}
/// 状态量(此刻是多少),不是「这窗口发生了几次」—— 采集时读而不清。
static TABS: AtomicU64 = AtomicU64::new(0);
static PANES: AtomicU64 = AtomicU64::new(0);
static HOSTS: AtomicU64 = AtomicU64::new(0);

// F167:滚动事件(计次,swap 清零)。
static SCROLL_EVENTS: AtomicU64 = AtomicU64::new(0);
// F167/F169:传输与内存 gauge(状态量,load 读)。
static XFER_JOBS: AtomicU64 = AtomicU64::new(0);
static XFER_RUNNING: AtomicU64 = AtomicU64::new(0);
static XFER_BYTES_LEFT: AtomicU64 = AtomicU64::new(0);
static MEM_SCROLL_BYTES: AtomicU64 = AtomicU64::new(0);
static SCROLL_LINES: AtomicU64 = AtomicU64::new(0);
static MEM_TEXT_BYTES: AtomicU64 = AtomicU64::new(0);

static FRAME_US: crate::profile::Histogram = crate::profile::Histogram::new();
static ECHO_US: crate::profile::Histogram = crate::profile::Histogram::new();
/// F165:GPU 帧耗时(微秒)。与 `FRAME_US` 共用一套桶,好横向比。
static GPU_FRAME_US: crate::profile::Histogram = crate::profile::Histogram::new();
/// F170:分层耗时(微秒)。终端趟(槽1-槽0)与 egui 趟(槽2-槽1)。
static GPU_TERM_US: crate::profile::Histogram = crate::profile::Histogram::new();
static GPU_EGUI_US: crate::profile::Histogram = crate::profile::Histogram::new();
/// F170:`INSIDE_PASSES` 时间戳查询这一路是否拿到过。
static GPU_SPLIT_SUPPORTED: AtomicBool = AtomicBool::new(false);

/// F165:记一次 GPU 帧耗时。由 wgpu 的 map 回调调用(不在主线程上)。
pub fn record_gpu_frame_us(us: u64) {
    GPU_FRAME_US.record_us(us);
}

/// F170:一次分层采样(µs)。由 GpuTimer 回读回调调用(wgpu 内部线程)。
pub fn record_gpu_split_us(term_us: u64, egui_us: u64) {
    GPU_TERM_US.record_us(term_us);
    GPU_EGUI_US.record_us(egui_us);
}

/// F170:GPU 初始化时报告 `INSIDE_PASSES` 是否拿到。
pub fn set_gpu_split_supported(v: bool) {
    GPU_SPLIT_SUPPORTED.store(v, Ordering::Relaxed);
}

/// F165:显存探针。`Gpu::new` 建好后放进来 —— 看门狗线程比 GPU 早启动,
/// 拿不到 adapter info,只能反过来由 GPU 那边推给它。
static VRAM_PROBE: std::sync::OnceLock<crate::sysprobe::VramProbe> = std::sync::OnceLock::new();

/// F165:`Gpu::new` 调一次。重复调用忽略(只有一个窗口)。
pub fn set_vram_probe(p: crate::sysprobe::VramProbe) {
    let _ = VRAM_PROBE.set(p);
}
/// 最后一次按键的时刻(µs)。0 = 还没按过 / 已被下一段入站字节消费掉。
static LAST_KEY_US: AtomicU64 = AtomicU64::new(0);

fn elapsed_us() -> u64 {
    ORIGIN.get().map_or(0, |o| o.elapsed().as_micros() as u64)
}

/// 进入某个阶段。主线程在事件循环各处调用。
///
/// 开销:一次 `Instant::elapsed` + 两条 relaxed 原子交换 + 一次原子加法。
/// Windows 上 `Instant::now` 走 QPC,约 20~30ns —— 帧路径上可以忽略,
/// 但**绝不能**在这里做格式化或加锁(T3)。
pub fn mark(stage: Stage) {
    CLOCK.mark(stage, elapsed_us());
}

/// 「上次换阶段到现在」有多少毫秒。看门狗的阈值是毫秒级的,而时基是微秒。
///
/// 抽成纯函数**只为一件事:这个 `/1000` 能被测试盯住**。埋在
/// `watchdog_loop` 里的话,漏掉它会让 `beat_us`(µs 量级,通常比 `now_us`
/// 大不了但量纲错位)把 `saturating_sub` 饱和成 0 —— 看门狗**静默永久
/// 失效**,编译器和现有测试都不会吭一声。
pub fn stuck_ms(now_us: u64, beat_us: u64) -> u64 {
    now_us.saturating_sub(beat_us) / 1000
}

pub fn count_frame() {
    FRAMES.fetch_add(1, Ordering::Relaxed);
}
pub fn count_present() {
    PRESENTS.fetch_add(1, Ordering::Relaxed);
}
/// 整帧被跳过(surface 超时/丢失、图集满、最小化…)。
pub fn count_skipped() {
    SKIPPED.fetch_add(1, Ordering::Relaxed);
}
pub fn count_inbound(bytes: usize) {
    INBOUND_BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
}

/// 整帧耗时(从 redraw 入口到 present 结束)。
pub fn record_frame_us(us: u64) {
    FRAME_US.record_us(us);
}

/// 这一帧的重绘是被谁触发的。三类分开计,才看得出「远端安静时 egui 还在
/// 每秒要几十次重绘」这种白烧 GPU 的情况。
pub fn count_redraw(terminal: bool, ui: bool) {
    match (terminal, ui) {
        (true, true) => &REDRAW_BOTH,
        (true, false) => &REDRAW_TERMINAL,
        (false, true) => &REDRAW_UI,
        // 两边都不脏时事件循环不会走到这里;真走到了也不该计进任何一类。
        (false, false) => return,
    }
    .fetch_add(1, Ordering::Relaxed);
}

/// 一次重绘被帧闸挡下(T3 的直接体感指标)。
pub fn count_throttled() {
    THROTTLED.fetch_add(1, Ordering::Relaxed);
}

/// F157:窗口收到了一次 `RedrawRequested`。**记在分支最开头**——记在后面
/// 的话,最小化 / PumpOnly 那些提前 return 的路径完全不计数,而「最小化了
/// 却还在每秒醒 400 次」恰恰是最该被看见的一种。
pub fn count_wake() {
    WAKES.fetch_add(1, Ordering::Relaxed);
}

/// 我们主动调 `request_redraw` 的来源(F157)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedrawSource {
    /// `about_to_wait` 到点把被节流的那帧补画出来。
    Scheduled,
    /// 因窗口事件 / 后台唤醒 / UI 主动请求。
    Event,
}

/// F157:记一次主动请求重绘。
///
/// 与 `count_wake` 的差值不为零是**正常**的:多次请求会被 winit 合并成
/// 一次 `RedrawRequested`,OS 也会主动发。差值本身就是信息,不试图把它
/// 归成精确的三类(那需要一个「最近一次请求来源」的单槽标记,而合并会让
/// 它系统性失真)。宁可报两个诚实的数。
pub fn count_request_redraw(src: RedrawSource) {
    match src {
        RedrawSource::Scheduled => &RR_SCHED,
        RedrawSource::Event => &RR_EVT,
    }
    .fetch_add(1, Ordering::Relaxed);
}

/// F157:这一帧喂给 egui 多少个输入事件。0 时直接返回,免得静止时也在
/// relaxed 原子上打转。
///
/// egui 的 `InputState::wants_repaint_after` 只要本趟 `events` 非空就返回
/// `Duration::ZERO` —— 所以「有几帧带着事件」才是跟 `rdelay=z:` 直接对得上
/// 的那个数,两者必须分开报。
pub fn note_egui_events(n: usize) {
    if n == 0 {
        return;
    }
    EGUI_EVENTS.fetch_add(n as u64, Ordering::Relaxed);
    EGUI_EVENT_FRAMES.fetch_add(1, Ordering::Relaxed);
}

/// F157:egui 这一帧吐回来的 `repaint_delay` 落在哪个桶。
pub fn note_repaint_delay(d: Duration) {
    match crate::profile::repaint_bucket(d) {
        crate::profile::RepaintBucket::Zero => &RDELAY_ZERO,
        crate::profile::RepaintBucket::Finite => &RDELAY_FINITE,
        crate::profile::RepaintBucket::Max => &RDELAY_MAX,
    }
    .fetch_add(1, Ordering::Relaxed);
}

/// F159:整帧指纹这一帧命中(没提交 GPU)还是没命中。
///
/// **差分类优化唯一的运行期守护**:判据写错导致永远 miss 时,画面完全
/// 正确、日志一切正常、性能悄悄回到改之前,只有这里的比值会掉。
pub fn count_frame_fp(hit: bool) {
    if hit { &FP_HIT } else { &FP_MISS }.fetch_add(1, Ordering::Relaxed);
}

/// 用户按下了一个会发往远端的键。记下时刻,等下一段入站字节来时算回显往返。
///
/// **这是近似**:连续打字时,第 N 个键的回显可能在第 N+1 个键之后才到,
/// 那样量到的是一个偏小的值;反过来,远端自己吐的输出(比如 `top` 刷新)
/// 也会被当成回显,同样偏小。它回答不了「精确延迟是多少」,能回答的是
/// 「这条链路的回显是十毫秒级还是几百毫秒级」——而后者正是高延迟代理
/// 链路上要看的量级。精确做法要给每个按键打序号并等它原样回来,那需要
/// 在 VT 层做匹配,是另一片的工作量。
pub fn note_key() {
    KEYS.fetch_add(1, Ordering::Relaxed);
    LAST_KEY_US.store(elapsed_us(), Ordering::Relaxed);
}

/// 有入站字节抵达。若在此之前有一次未被消费的按键,记一次回显往返。
pub fn note_inbound_for_echo() {
    let key_us = LAST_KEY_US.swap(0, Ordering::Relaxed);
    if key_us > 0 {
        ECHO_US.record_us(elapsed_us().saturating_sub(key_us));
    }
}

pub fn count_connect(ok: bool) {
    if ok {
        CONNECTS_OK.fetch_add(1, Ordering::Relaxed);
    } else {
        CONNECTS_ERR.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn count_reconnect() {
    RECONNECTS.fetch_add(1, Ordering::Relaxed);
}

pub fn count_sftp_op() {
    SFTP_OPS.fetch_add(1, Ordering::Relaxed);
}

/// 本帧各 pane 攒帧状态机报上来的同步块计数(T2)。两者都为 0 时直接返回,
/// 免得静止时也在 relaxed 原子上打转。
pub fn count_sync(blocks: u64, timeouts: u64) {
    if blocks == 0 && timeouts == 0 {
        return;
    }
    SYNC_BLOCKS.fetch_add(blocks, Ordering::Relaxed);
    SYNC_TIMEOUTS.fetch_add(timeouts, Ordering::Relaxed);
}

/// 本帧整形缓存的命中/未命中行数(F12)。两者都为 0(这一帧没画)时直接
/// 返回,免得静止时也在 relaxed 原子上打转。
pub fn count_reshape(hits: u64, misses: u64) {
    if hits == 0 && misses == 0 {
        return;
    }
    RESHAPE_HIT.fetch_add(hits, Ordering::Relaxed);
    RESHAPE_MISS.fetch_add(misses, Ordering::Relaxed);
}

/// 此刻的规模。App 每帧调一次(三条 relaxed 原子存,可忽略)。
pub fn set_scale(tabs: usize, panes: usize, hosts: usize) {
    TABS.store(tabs as u64, Ordering::Relaxed);
    PANES.store(panes as u64, Ordering::Relaxed);
    HOSTS.store(hosts as u64, Ordering::Relaxed);
}

/// F167:用户滚了一下(滚轮一档/一次翻页/拖拽自滚一帧都算一次)。
pub fn count_scroll() {
    SCROLL_EVENTS.fetch_add(1, Ordering::Relaxed);
}

/// F167/F169:传输队列规模。relaxed 原子存,帧路径可调(与 set_scale 同款)。
pub fn set_xfer_gauges(active: u64, running: u64, bytes_left: u64) {
    XFER_JOBS.store(active, Ordering::Relaxed);
    XFER_RUNNING.store(running, Ordering::Relaxed);
    XFER_BYTES_LEFT.store(bytes_left, Ordering::Relaxed);
}

/// F169:内存记账 gauge。
pub fn set_mem_gauges(scroll_bytes: u64, scroll_lines: u64, text_bytes: u64) {
    MEM_SCROLL_BYTES.store(scroll_bytes, Ordering::Relaxed);
    SCROLL_LINES.store(scroll_lines, Ordering::Relaxed);
    MEM_TEXT_BYTES.store(text_bytes, Ordering::Relaxed);
}

/// 卡住多久之后才第一次报警。低于这个值的都是正常的长帧。
pub const DEFAULT_STALL_MS: u64 = 3_000;

/// 是否该(再次)报警。`reported_ms` = 上次报警时的卡住时长,0 = 尚未报过。
/// 第一次越过阈值报一次,之后每翻倍再报一次(3s / 6s / 12s…),不刷屏。纯函数,可单测。
pub fn should_report(stuck_ms: u64, stall_ms: u64, reported_ms: u64) -> bool {
    if stuck_ms < stall_ms {
        return false;
    }
    reported_ms == 0 || stuck_ms >= reported_ms.saturating_mul(2)
}

/// 内存快照。判断卡死时是否伴随内存压力(reflow 爆内存 / 泄漏 / 系统整体吃紧)。
#[derive(Debug, Clone, Copy)]
pub struct MemSample {
    /// 本进程私有提交量(Windows PrivateUsage / Linux RSS)。
    pub process_bytes: u64,
    pub sys_avail_bytes: u64,
    pub sys_total_bytes: u64,
}

impl std::fmt::Display for MemSample {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        const MB: u64 = 1024 * 1024;
        write!(
            f,
            "进程={}MB 系统可用={}MB/{}MB",
            self.process_bytes / MB,
            self.sys_avail_bytes / MB,
            self.sys_total_bytes / MB
        )
    }
}

#[cfg(windows)]
pub fn sample_memory() -> Option<MemSample> {
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    // SAFETY:两个调用都只写入我们自己栈上的、已按 API 要求填好 cb/dwLength 的结构体。
    // K32GetProcessMemoryInfo 按 cb 判断实际结构体大小,传 _EX 的尺寸即可拿到 PrivateUsage
    // (这是 Win32 文档给出的标准用法)。失败一律回落 None,不 panic。
    unsafe {
        let mut pmc = std::mem::zeroed::<PROCESS_MEMORY_COUNTERS_EX>();
        pmc.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        let ok_proc = K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            std::ptr::addr_of_mut!(pmc).cast::<PROCESS_MEMORY_COUNTERS>(),
            pmc.cb,
        ) != 0;

        let mut ms = std::mem::zeroed::<MEMORYSTATUSEX>();
        ms.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        let ok_sys = GlobalMemoryStatusEx(std::ptr::addr_of_mut!(ms)) != 0;

        if !ok_proc && !ok_sys {
            return None;
        }
        Some(MemSample {
            process_bytes: pmc.PrivateUsage as u64,
            sys_avail_bytes: ms.ullAvailPhys,
            sys_total_bytes: ms.ullTotalPhys,
        })
    }
}

#[cfg(target_os = "linux")]
pub fn sample_memory() -> Option<MemSample> {
    // /proc/self/statm 第 2 字段 = 常驻页数;/proc/meminfo 取 MemTotal/MemAvailable。
    let page = 4096u64;
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let rss_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let kb = |key: &str| -> u64 {
        meminfo
            .lines()
            .find(|l| l.starts_with(key))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
            * 1024
    };
    Some(MemSample {
        process_bytes: rss_pages * page,
        sys_avail_bytes: kb("MemAvailable:"),
        sys_total_bytes: kb("MemTotal:"),
    })
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn sample_memory() -> Option<MemSample> {
    None
}

/// 启动环境快照。以前这些只能去翻 Windows 事件查看器/dxdiag,现在自己记。
/// GPU 相关(adapter 名/驱动版本/后端)由 `gpu.rs` 拿到 adapter 后另行补记。
pub fn log_startup_env(version: &str) {
    let cpus = std::thread::available_parallelism().map_or(0, |n| n.get());
    log::info!(
        target: "mullion",
        "环境: mullion={version} arch={} os={} family={} cpus={cpus}",
        std::env::consts::ARCH,
        std::env::consts::OS,
        std::env::consts::FAMILY,
    );
    if let Some(m) = sample_memory() {
        log::info!(target: "mullion", "内存: {m}");
    }
}

/// 启动看门狗 + 周期采样线程。`main` 在 `logx::init` 之后调用一次。
///
/// `stall_ms`:非 Idle 阶段停滞多久算卡(默认 [`DEFAULT_STALL_MS`])。
pub fn start_watchdog(stall_ms: u64) {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let _ = ORIGIN.set(Instant::now());
    CLOCK.beat_us.store(0, Ordering::Relaxed);
    // **必须在这里建**:`start_watchdog` 由 `main` 在主线程上调用,而
    // `CpuProbe` 要在主线程上取主线程自己的句柄/tid。搬进 watchdog_loop
    // 里建的话,拿到的是看门狗线程自己(静默错值)。`ThreadCpuProbe` 的
    // `main_tid` 同理:搬进去的话排除的是看门狗自己、主线程反而被算进
    // 「其他线程」的清单里,语义正好反了。
    let cpu = crate::sysprobe::CpuProbe::new_on_main_thread();
    let gpu = crate::sysprobe::GpuProbe::new();
    let threads = crate::sysprobe::ThreadCpuProbe::new(crate::sysprobe::current_tid());
    let spawned = std::thread::Builder::new()
        .name("mullion-watchdog".into())
        .spawn(move || watchdog_loop(stall_ms, cpu, gpu, threads));
    if let Err(e) = spawned {
        // 看门狗起不来不该拖垮程序,但必须留痕(否则日志里没有它的 WARN 会被误读成「没卡过」)。
        log::warn!(target: "mullion", "看门狗线程启动失败,自诊断降级: {e}");
    }
}

/// 周期指标行的间隔。既是性能基线,也是「主线程还活着」的心跳。
const METRICS_EVERY_MS: u64 = 5_000;

fn watchdog_loop(
    stall_ms: u64,
    mut cpu: crate::sysprobe::CpuProbe,
    mut gpu: crate::sysprobe::GpuProbe,
    mut threads: crate::sysprobe::ThreadCpuProbe,
) {
    let mut reported_ms = 0u64;
    let mut last_metrics = 0u64;
    loop {
        std::thread::sleep(Duration::from_millis(1_000));
        let now_us = elapsed_us();
        let stage = CLOCK.stage.load(Ordering::Relaxed);
        let stuck = stuck_ms(now_us, CLOCK.beat_us.load(Ordering::Relaxed));

        if stage == Stage::Idle as u8 {
            // 正常空闲:阻塞等事件本来就可以很久,不是卡死。
            reported_ms = 0;
        } else if should_report(stuck, stall_ms, reported_ms) {
            let mem = sample_memory()
                .map(|m| m.to_string())
                .unwrap_or_else(|| "内存采样不可用".into());
            log::warn!(
                target: "mullion",
                "事件循环停滞 {:.1}s;最后阶段={} 近5s内 帧={} present={} 跳帧={} 入站={}KB {mem}",
                stuck as f64 / 1000.0,
                stage_name(stage),
                FRAMES.load(Ordering::Relaxed),
                PRESENTS.load(Ordering::Relaxed),
                SKIPPED.load(Ordering::Relaxed),
                INBOUND_BYTES.load(Ordering::Relaxed) / 1024,
            );
            reported_ms = stuck;
        }

        if now_us.saturating_sub(last_metrics) >= METRICS_EVERY_MS * 1000 {
            let window_ms = now_us.saturating_sub(last_metrics) / 1000;
            last_metrics = now_us;
            // **无条件** drain:计数器的语义必须是「这一窗口」,不能取决于日志
            // 档位。挂在门里的话,error 档下计数器一路累积,而停滞报警行读的
            // 是同一批 static —— 同一个数字在不同档位下含义不同,是排障时
            // 最坏的一类坑。渲染(格式化)才是贵的那步,只有它需要关在门里。
            let cpu_sample = cpu.sample(window_ms.saturating_mul(1_000_000));
            let mut snap = take_snapshot(window_ms);
            snap.cpu_pct = cpu_sample.map(|c| c.process_pct);
            snap.main_cpu_pct = cpu_sample.map(|c| c.main_thread_pct);
            if let Some(g) = gpu.sample() {
                snap.gpu_available = true;
                snap.gpu_engines = g.engines;
            }
            match threads.sample() {
                Some(list) => {
                    snap.thread_available = true;
                    let window_ns = window_ms.saturating_mul(1_000_000);
                    let pcts: Vec<(String, u32)> = list
                        .into_iter()
                        .filter_map(|(name, delta)| {
                            crate::profile::thread_group_pct(delta, window_ns).map(|p| (name, p))
                        })
                        .collect();
                    let g = crate::profile::group_threads(&pcts);
                    snap.thread_groups = g.groups;
                    snap.thread_unmapped = g.unmapped;
                }
                None => snap.thread_available = false,
            }
            if log::log_enabled!(target: "mullion", log::Level::Info) {
                // 贵的那两行(线程未分组明细 / 记账原始字节)只在 Debug 档出,
                // 常开会把固定五行拖到没法一眼看完(用户拍板的分层:Info 常开
                // 便宜的,Debug 才开贵的)。
                let debug = log::log_enabled!(target: "mullion", log::Level::Debug);
                // **每行独立 log 一次**,各自带时间戳与 pid 前缀(F166)。
                // 单条记录里嵌 `\n` 会让续行 grep 不到时间,设计文档 §2 明令禁止。
                for line in crate::profile::render_lines(&snap, debug) {
                    log::info!(target: "mullion", "{line}");
                }
            }
        }

        // info/debug 档走缓冲写,靠这里把最后一秒刷下去 —— 没有它,
        // 卡死时最后几秒的日志会随进程一起消失,而那正是唯一有用的一段。
        crate::logx::flush_now();
        // 一实例一文件之后,轮转判据从「启动时」搬到这里 —— 文件名唯一,
        // 启动时那个文件永远是空的。放看门狗而不是写日志的热路径:
        // 一次 metadata 系统调用不能进帧预算(T3)。
        crate::logx::rotate_if_needed();
    }
}

/// 把这一窗口的所有计数器**取走**,凑成一份快照。
///
/// 计次量全部 `swap(0)` / `drain()`:剖面报的是「这 5 秒」,不是自启动以来
/// 的累计 —— 累计值会被启动那几秒的尖峰永久污染,跑了一小时之后 p95
/// 反映的还是一小时前的事。
///
/// 状态量(tabs/panes/hosts)读而不清:它们描述的是「此刻有几个标签」,
/// 不是「这 5 秒发生了几次」。
fn take_snapshot(window_ms: u64) -> crate::profile::Snapshot {
    let mut s = crate::profile::Snapshot::empty();
    s.window_ms = window_ms;
    s.frames = FRAMES.swap(0, Ordering::Relaxed);
    s.presents = PRESENTS.swap(0, Ordering::Relaxed);
    s.skipped = SKIPPED.swap(0, Ordering::Relaxed);
    s.inbound_bytes = INBOUND_BYTES.swap(0, Ordering::Relaxed);
    s.redraw_terminal = REDRAW_TERMINAL.swap(0, Ordering::Relaxed);
    s.redraw_ui = REDRAW_UI.swap(0, Ordering::Relaxed);
    s.redraw_both = REDRAW_BOTH.swap(0, Ordering::Relaxed);
    s.throttled = THROTTLED.swap(0, Ordering::Relaxed);
    s.keys = KEYS.swap(0, Ordering::Relaxed);
    s.echo_us = ECHO_US.drain();
    s.frame_us = FRAME_US.drain();
    for (k, h) in CLOCK.hist.iter().enumerate() {
        s.stage_us[k] = h.drain();
    }
    s.connects_ok = CONNECTS_OK.swap(0, Ordering::Relaxed);
    s.connects_err = CONNECTS_ERR.swap(0, Ordering::Relaxed);
    s.reconnects = RECONNECTS.swap(0, Ordering::Relaxed);
    s.sftp_ops = SFTP_OPS.swap(0, Ordering::Relaxed);
    s.sync_blocks = SYNC_BLOCKS.swap(0, Ordering::Relaxed);
    s.sync_timeouts = SYNC_TIMEOUTS.swap(0, Ordering::Relaxed);
    s.reshape_hit = RESHAPE_HIT.swap(0, Ordering::Relaxed);
    s.reshape_miss = RESHAPE_MISS.swap(0, Ordering::Relaxed);
    let (dirty_sites, dirty_other) = DIRTY.drain();
    s.dirty_sites = dirty_sites;
    s.dirty_other = dirty_other;
    let (wev_kinds, wev_other) = WEV.drain();
    s.wev_kinds = wev_kinds;
    s.wev_other = wev_other;
    s.cursor_dup = CURSOR.drain();
    s.wakes = WAKES.swap(0, Ordering::Relaxed);
    s.rr_sched = RR_SCHED.swap(0, Ordering::Relaxed);
    s.rr_evt = RR_EVT.swap(0, Ordering::Relaxed);
    s.egui_events = EGUI_EVENTS.swap(0, Ordering::Relaxed);
    s.egui_event_frames = EGUI_EVENT_FRAMES.swap(0, Ordering::Relaxed);
    s.rdelay_zero = RDELAY_ZERO.swap(0, Ordering::Relaxed);
    s.rdelay_finite = RDELAY_FINITE.swap(0, Ordering::Relaxed);
    s.rdelay_max = RDELAY_MAX.swap(0, Ordering::Relaxed);
    s.fp_hit = FP_HIT.swap(0, Ordering::Relaxed);
    s.fp_miss = FP_MISS.swap(0, Ordering::Relaxed);
    s.tabs = TABS.load(Ordering::Relaxed);
    s.panes = PANES.load(Ordering::Relaxed);
    s.hosts = HOSTS.load(Ordering::Relaxed);
    s.mem_process_mb = sample_memory().map_or(0, |m| m.process_bytes / (1024 * 1024));
    s.vram_mb = VRAM_PROBE
        .get()
        .and_then(|p| p.sample())
        .map(|v| (v.used_mb, v.budget_mb));
    s.gpu_frame_us = GPU_FRAME_US.drain();
    s.scroll_events = SCROLL_EVENTS.swap(0, Ordering::Relaxed);
    s.xfer_jobs = XFER_JOBS.load(Ordering::Relaxed);
    s.xfer_running = XFER_RUNNING.load(Ordering::Relaxed);
    s.xfer_bytes_left = XFER_BYTES_LEFT.load(Ordering::Relaxed);
    s.mem_scroll_bytes = MEM_SCROLL_BYTES.load(Ordering::Relaxed);
    s.scroll_lines = SCROLL_LINES.load(Ordering::Relaxed);
    s.mem_text_bytes = MEM_TEXT_BYTES.load(Ordering::Relaxed);
    s.gpu_term_us = GPU_TERM_US.drain();
    s.gpu_egui_us = GPU_EGUI_US.drain();
    s.gpu_split_supported = GPU_SPLIT_SUPPORTED.load(Ordering::Relaxed);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_names_cover_every_variant() {
        // 漏一个名字 → 日志里出现 "?",卡死时就白记了。
        assert_eq!(STAGE_NAMES.len(), Stage::StoreIo as usize + 1);
        assert_eq!(stage_name(Stage::Acquire as u8), "acquire");
        assert_eq!(stage_name(Stage::StoreIo as u8), "store_io");
        assert_eq!(stage_name(200), "?");
    }

    #[test]
    fn does_not_report_below_threshold() {
        assert!(!should_report(2_999, 3_000, 0));
        assert!(should_report(3_000, 3_000, 0));
    }

    #[test]
    fn repeats_only_on_doubling() {
        // 持续卡住时按 3s/6s/12s 复报,而不是每秒刷一行。
        assert!(!should_report(4_000, 3_000, 3_000));
        assert!(should_report(6_000, 3_000, 3_000));
        assert!(!should_report(9_000, 3_000, 6_000));
        assert!(should_report(12_000, 3_000, 6_000));
    }

    #[test]
    fn memory_sample_is_plausible_on_this_platform() {
        // Linux/Windows 上应能采到;采不到说明 feature/路径写错了,比静默 None 更该暴露。
        if cfg!(any(windows, target_os = "linux")) {
            let m = sample_memory().expect("本平台应能采到内存");
            assert!(m.process_bytes > 0, "进程内存不该为 0");
        }
    }

    /// `mark` 换阶段时必须把**上一个阶段**待了多久记进它自己的桶。
    ///
    /// 这是整份剖面的地基:事件循环里已经铺满 `mark()`,累计这一步做对了,
    /// 「pump 慢还是 present 慢」就不需要任何新插桩点。做错了(比如记进
    /// 新阶段而不是旧阶段)不会有任何报错,只会让剖面**一直指错人**。
    ///
    /// 用自己的 `StageClock`、时刻直接传常量:不碰进程级 static,也就不会
    /// 被别的测试(`host_key::tests` 也会间接调 `mark`)偷走 prev/since。
    ///
    /// 自证会变红:把 `StageClock::mark` 里的 `prev` 换成 `stage as u8`。
    #[test]
    fn marking_a_new_stage_charges_the_elapsed_time_to_the_stage_that_just_ended() {
        let clock = StageClock::new();
        clock.mark(Stage::Pump, 1_000);
        clock.mark(Stage::Present, 3_500);

        let pump = clock.hist[Stage::Pump as usize].drain();
        let present = clock.hist[Stage::Present as usize].drain();
        assert_eq!(
            crate::profile::total(&pump),
            1,
            "离开 Pump 时没把它这一趟的耗时记下来"
        );
        assert!(
            (2_000..=4_096).contains(&crate::profile::quantile_us(&pump, 1.0)),
            "Pump 待了 2500µs,却记成了 {}µs",
            crate::profile::quantile_us(&pump, 1.0)
        );
        assert_eq!(
            crate::profile::total(&present),
            0,
            "耗时被记到了刚进入的阶段头上 —— 剖面会一直指错人"
        );
    }

    /// `Idle` 的时长**不进直方图**。阻塞等事件本来就可以很久,把它算进去
    /// 会让直方图永远被一个几十秒的样本主导,其余全部淹没在噪声里。
    ///
    /// 自证会变红:把 `StageClock::mark` 里那句跳过 Idle 的判断删掉。
    #[test]
    fn time_spent_idle_is_not_charged_to_anything() {
        let clock = StageClock::new();
        clock.mark(Stage::Idle, 0);
        clock.mark(Stage::UserEvent, 5_000_000);
        assert_eq!(
            crate::profile::total(&clock.hist[Stage::Idle as usize].drain()),
            0,
            "空闲等待被当成了耗时"
        );
    }

    /// 时基必须是微秒。毫秒精度下单帧各阶段几乎全是 0 —— 剖面看着像
    /// 「哪里都不慢」,而实际瓶颈藏在被截断的小数里。
    ///
    /// 只读 `ORIGIN`(`OnceLock`,设过就不再变)+ 两次单调读,不碰任何
    /// 会被别的测试改写的状态,所以不需要串行化。
    ///
    /// 自证会变红:把 `elapsed_us` 的 `as_micros()` 改成 `as_millis()`。
    #[test]
    fn the_clock_counts_microseconds_not_milliseconds() {
        let _ = ORIGIN.set(Instant::now());
        let a = elapsed_us();
        std::thread::sleep(Duration::from_millis(2));
        let b = elapsed_us();
        assert!(
            b.saturating_sub(a) >= 1_000,
            "睡了 2ms 时钟只走了 {}µs —— 时基不是微秒",
            b.saturating_sub(a)
        );
    }

    /// 看门狗的阈值是毫秒,时基是微秒,中间那次换算漏掉的话看门狗会
    /// **静默永久失效**(`saturating_sub` 几乎永远饱和成 0,再也不报警),
    /// 而编译器和其余测试一句话都不会说。
    ///
    /// 自证会变红:把 `stuck_ms` 里的 `/ 1000` 去掉。
    #[test]
    fn the_watchdog_converts_microseconds_to_its_millisecond_threshold() {
        assert_eq!(stuck_ms(5_000_000, 1_000_000), 4_000);
        assert_eq!(stuck_ms(1_500, 1_000), 0, "半毫秒不该算成一毫秒");
        // 时钟异常导致的倒流不许下溢成天文数字 —— 那会让看门狗每秒刷屏。
        assert_eq!(stuck_ms(1_000, 9_999_999), 0);
        // 3 秒阈值刚好越线。
        assert!(stuck_ms(3_000_000, 0) >= DEFAULT_STALL_MS);
    }

    /// F157:归因表在槽位够用时逐行分开计,用完之后落进 `other`。
    ///
    /// 表写错的症状是「剖面里有一行数字,但它指的不是那个地方」——归因错了
    /// 比没有更糟,会把人带去改错地方。
    ///
    /// 用**自己的**表实例而不是进程级 static:并行 runner 下别的测试会间接
    /// 调到 `note_ui_dirty`,共享 static 会让断言概率性假红(与本文件
    /// `StageClock` 给测试单开实例是同一条理由)。
    ///
    /// 自证会变红:把 `KeyTable::note` 里的线性扫描去掉,永远走 `other`。
    #[test]
    fn the_attribution_table_keeps_each_line_apart_until_it_runs_out_of_slots() {
        let t = KeyTable::new();
        for _ in 0..300 {
            t.note(8365);
        }
        t.note(7191);
        t.note(7191);
        // 再来 TABLE_SLOTS 个各不相同的行号,把槽位撑满并溢出。
        for line in 1000..(1000 + TABLE_SLOTS as u32) {
            t.note(line);
        }
        let (sites, other) = t.drain();
        assert!(
            sites.contains(&(8365, 300)),
            "每帧都在置脏的那一处没被单独计出来:{sites:?}"
        );
        assert!(sites.contains(&(7191, 2)), "第二处没被计出来:{sites:?}");
        assert_eq!(sites.len(), TABLE_SLOTS, "槽位没被填满:{sites:?}");
        assert!(other > 0, "槽位用完之后的置脏必须落进 other,否则会静默消失");
    }

    /// 取走之后,**这一窗口没再响过**的槽位要还回去。
    ///
    /// 不还的话,启动那几秒里各来一次的一次性置脏点会把 8 个槽位永久占死,
    /// 而真正每帧都在置脏的那一处只能落进 `other` —— 一整趟实机往返白跑,
    /// 且剖面看起来一切正常。
    ///
    /// 自证会变红:把 `note` 第二轮判定里的 `|| self.hits[i].load(..) == 0`
    /// 去掉,只留 `line[i] == 0`——回收槽位的活其实是 `note` 自己抢着干的,
    /// `drain` 里「hits == 0 就清零」那句只在槽位彻底没再被任何行认领时
    /// 才会被走到,对这条测试是死代码。
    #[test]
    fn a_slot_that_went_quiet_is_handed_back_for_the_next_window() {
        let t = KeyTable::new();
        for line in 1..=(TABLE_SLOTS as u32) {
            t.note(line); // 启动期的一次性置脏点,各来一次
        }
        let _ = t.drain();
        for _ in 0..50 {
            t.note(9999); // 下一个窗口里真正每帧都在置脏的那一处
        }
        let (sites, other) = t.drain();
        assert!(
            sites.contains(&(9999, 50)),
            "槽位没还回来,常驻置脏点被挤进了 other:sites={sites:?} other={other}"
        );
    }

    /// 计次量是**取走**,不是读取。剖面报的是「这 5 秒」而不是自启动累计。
    ///
    /// 自证会变红:把 `drain` 里 `DIRTY_HITS` 的 `swap(0, ..)` 改成 `load(..)`。
    #[test]
    fn draining_the_attribution_table_resets_the_window() {
        let t = KeyTable::new();
        t.note(42);
        assert_eq!(t.drain().0, vec![(42, 1)]);
        assert!(t.drain().0.is_empty(), "取走之后窗口该是空的");
    }

    /// F171:同坐标算 dup,坐标一变就不算。
    ///
    /// 这一位是整个 F171 的判据分岔口:dup≈事件数 → 幽灵事件(坐标恒定,
    /// 可以按位置掐);dup 远小于事件数 → 真有东西在动指针,得先找出是谁。
    /// 判错这一位,下一轮就会往完全相反的方向改。
    ///
    /// 自证会变红:把 `note` 里的 `same` 恒置 true 或恒置 false。
    #[test]
    fn a_cursor_event_at_the_same_spot_is_a_duplicate_and_a_moved_one_is_not() {
        let c = CursorTracker::new();
        assert!(!c.note(10.0, 20.0), "第一次没有「上一次」可比,不该算 dup");
        assert!(c.note(10.0, 20.0), "同坐标该算 dup");
        assert!(c.note(10.0, 20.0), "连续同坐标该继续算 dup");
        assert!(!c.note(10.0, 21.0), "坐标变了不该算 dup");
        assert_eq!(c.drain(), 2, "dup 计数与判定不一致");
    }

    /// F171:**bit 相等,不是数值相等**。`-0.0 == 0.0` 在数值上成立,
    /// 但 bits 不同 —— 判成「动过」是保守的一侧(只漏报 dup)。
    ///
    /// 误报 dup 才危险:那会让人以为「按坐标去重是安全的」,进而掐掉真实的
    /// 指针移动。
    ///
    /// 自证会变红:把 `to_bits()` 比较改成 `x == last_x` 的浮点比较。
    #[test]
    fn the_duplicate_test_is_conservative_about_negative_zero() {
        let c = CursorTracker::new();
        assert!(!c.note(0.0, 0.0));
        assert!(!c.note(-0.0, 0.0), "-0.0 该判成动过(保守侧)");
        assert_eq!(c.drain(), 0);
    }

    /// F171:指针停在窗口原点也要能记出 dup。
    ///
    /// 拿 `last == (0, 0)` 当「还没收到过」的哨兵的话,指针真停在原点时
    /// 每一次都被当成第一次,dup 恒为 0 —— 静默错值,日志上与「指针一直在动」
    /// 长得一模一样。
    ///
    /// 自证会变红:把 `seen` 去掉、改用 `last == (0, 0)` 判首次。
    #[test]
    fn the_origin_is_a_real_position_not_a_sentinel() {
        let c = CursorTracker::new();
        c.note(0.0, 0.0);
        c.note(0.0, 0.0);
        c.note(0.0, 0.0);
        assert_eq!(c.drain(), 2, "停在原点的指针记不出 dup");
    }

    /// F171:取走 dup 计数**不重置上一次的坐标**。
    ///
    /// 指针跨过采样窗口边界仍然没动,那仍然是 dup。重置的话每个窗口的
    /// 第一次 `CursorMoved` 都算「动过」,dup 率会随采样频率变化 ——
    /// 一个随采样频率变化的指标没法用来下结论。
    ///
    /// 自证会变红:在 `drain` 里加上 `self.seen.store(false, ..)`。
    #[test]
    fn draining_keeps_the_last_position_so_stillness_survives_the_window() {
        let c = CursorTracker::new();
        c.note(7.0, 8.0);
        assert_eq!(c.drain(), 0);
        assert!(c.note(7.0, 8.0), "跨窗口的静止被误判成移动");
        assert_eq!(c.drain(), 1);
    }

    /// F167:计次量与状态量的清零语义不能弄反。
    ///
    /// `SCROLL_EVENTS`/`XFER_JOBS` 是本文件新加的 static,截至目前没有
    /// 别的测试碰它们(其余 `#[test]` 都不调 `count_scroll`/`set_xfer_gauges`/
    /// `take_snapshot`),所以不需要串行锁 —— 并行 runner 下不会有别的用例
    /// 往这两个 static 里写。若以后有测试也调用它们,需要重新评估是否要
    /// 上锁。
    ///
    /// 自证会变红:把 take_snapshot 里 SCROLL_EVENTS 的 `swap(0,..)` 改成
    /// `load(..)`,或把 XFER_JOBS 的 `load` 改成 `swap(0,..)`。
    #[test]
    fn scroll_is_drained_but_xfer_gauge_survives_the_snapshot() {
        count_scroll();
        set_xfer_gauges(2, 1, 48 << 20);
        let a = take_snapshot(5000);
        assert_eq!(a.scroll_events, 1);
        assert_eq!(a.xfer_jobs, 2);
        let b = take_snapshot(5000);
        assert_eq!(b.scroll_events, 0, "计次量必须随窗口清零");
        assert_eq!(b.xfer_jobs, 2, "状态量描述此刻,不许被清");
    }
}
