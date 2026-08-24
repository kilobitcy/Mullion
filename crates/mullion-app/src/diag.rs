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

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
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
/// 状态量(此刻是多少),不是「这窗口发生了几次」—— 采集时读而不清。
static TABS: AtomicU64 = AtomicU64::new(0);
static PANES: AtomicU64 = AtomicU64::new(0);
static HOSTS: AtomicU64 = AtomicU64::new(0);

static FRAME_US: crate::profile::Histogram = crate::profile::Histogram::new();
static ECHO_US: crate::profile::Histogram = crate::profile::Histogram::new();
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

/// 此刻的规模。App 每帧调一次(三条 relaxed 原子存,可忽略)。
pub fn set_scale(tabs: usize, panes: usize, hosts: usize) {
    TABS.store(tabs as u64, Ordering::Relaxed);
    PANES.store(panes as u64, Ordering::Relaxed);
    HOSTS.store(hosts as u64, Ordering::Relaxed);
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
    let spawned = std::thread::Builder::new()
        .name("mullion-watchdog".into())
        .spawn(move || watchdog_loop(stall_ms));
    if let Err(e) = spawned {
        // 看门狗起不来不该拖垮程序,但必须留痕(否则日志里没有它的 WARN 会被误读成「没卡过」)。
        log::warn!(target: "mullion", "看门狗线程启动失败,自诊断降级: {e}");
    }
}

/// 周期指标行的间隔。既是性能基线,也是「主线程还活着」的心跳。
const METRICS_EVERY_MS: u64 = 5_000;

fn watchdog_loop(stall_ms: u64) {
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
            let snap = take_snapshot(window_ms);
            if log::log_enabled!(target: "mullion", log::Level::Info) {
                if let Some(line) = crate::profile::render_line(&snap) {
                    log::info!(target: "mullion", "{line}");
                }
            }
        }

        // info/debug 档走缓冲写,靠这里把最后一秒刷下去 —— 没有它,
        // 卡死时最后几秒的日志会随进程一起消失,而那正是唯一有用的一段。
        crate::logx::flush_now();
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
    s.tabs = TABS.load(Ordering::Relaxed);
    s.panes = PANES.load(Ordering::Relaxed);
    s.hosts = HOSTS.load(Ordering::Relaxed);
    s.mem_process_mb = sample_memory().map_or(0, |m| m.process_bytes / (1024 * 1024));
    s
}

// F12(Task 6 会换成真实现):这一帧 (PaneId,row) 缓存命中/未命中数。
pub fn count_reshape(_hits: u64, _misses: u64) {}

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
}
