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

const STAGE_NAMES: [&str; 12] = [
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

fn stage_name(raw: u8) -> &'static str {
    STAGE_NAMES.get(raw as usize).copied().unwrap_or("?")
}

static STAGE: AtomicU8 = AtomicU8::new(Stage::Idle as u8);
static BEAT_MS: AtomicU64 = AtomicU64::new(0);
static ORIGIN: OnceLock<Instant> = OnceLock::new();
static STARTED: AtomicBool = AtomicBool::new(false);

// 指标(既是性能基线,也是心跳:卡死后能看出最后一次成功 present 是几秒前)。
static FRAMES: AtomicU64 = AtomicU64::new(0);
static PRESENTS: AtomicU64 = AtomicU64::new(0);
static SKIPPED: AtomicU64 = AtomicU64::new(0);
static INBOUND_BYTES: AtomicU64 = AtomicU64::new(0);

fn elapsed_ms() -> u64 {
    ORIGIN.get().map_or(0, |o| o.elapsed().as_millis() as u64)
}

/// 进入某个阶段。主线程在事件循环各处调用;开销 = 两条 relaxed 原子写。
pub fn mark(stage: Stage) {
    STAGE.store(stage as u8, Ordering::Relaxed);
    BEAT_MS.store(elapsed_ms(), Ordering::Relaxed);
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
    BEAT_MS.store(0, Ordering::Relaxed);
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
        let now = elapsed_ms();
        let stage = STAGE.load(Ordering::Relaxed);
        let stuck = now.saturating_sub(BEAT_MS.load(Ordering::Relaxed));

        if stage == Stage::Idle as u8 {
            // 正常空闲:阻塞等事件本来就可以很久,不是卡死。
            reported_ms = 0;
        } else if should_report(stuck, stall_ms, reported_ms) {
            let mem = sample_memory()
                .map(|m| m.to_string())
                .unwrap_or_else(|| "内存采样不可用".into());
            log::warn!(
                target: "mullion",
                "事件循环停滞 {:.1}s;最后阶段={} 帧={} present={} 跳帧={} 入站={}KB {mem}",
                stuck as f64 / 1000.0,
                stage_name(stage),
                FRAMES.load(Ordering::Relaxed),
                PRESENTS.load(Ordering::Relaxed),
                SKIPPED.load(Ordering::Relaxed),
                INBOUND_BYTES.load(Ordering::Relaxed) / 1024,
            );
            reported_ms = stuck;
        }

        if now.saturating_sub(last_metrics) >= METRICS_EVERY_MS {
            last_metrics = now;
            // debug 级:默认不开,排查时 `MULLION_LOG=debug` 就能拿到完整时间线。
            if log::log_enabled!(target: "mullion", log::Level::Debug) {
                let mem = sample_memory()
                    .map(|m| m.to_string())
                    .unwrap_or_else(|| "内存采样不可用".into());
                log::debug!(
                    target: "mullion",
                    "心跳 t={}s 阶段={} 帧={} present={} 跳帧={} 入站={}KB {mem}",
                    now / 1000,
                    stage_name(stage),
                    FRAMES.load(Ordering::Relaxed),
                    PRESENTS.load(Ordering::Relaxed),
                    SKIPPED.load(Ordering::Relaxed),
                    INBOUND_BYTES.load(Ordering::Relaxed) / 1024,
                );
            }
        }
    }
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
}
