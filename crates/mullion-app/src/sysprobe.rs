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
//! 由 `profile::render_lines` 渲染成 `n/a`。**不许编一个 0 出来**:
//! 「采不到」和「真的是 0」在排障时是两回事。

#[cfg(windows)]
use windows::core::Interface as _; // `cast::<IDXGIAdapter3>()`

/// 一次 CPU 采样。两个读数都是[单核万分比](cpu_bp)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuSample {
    /// 整个进程的 CPU 占用。**不归一、不封顶**:16 核全跑满 = 160_000。
    pub process_bp: u32,
    /// 主线程的 CPU 占用。同口径(一个线程占不满两个核,天然 ≤ 10_000)。
    pub main_thread_bp: u32,
}

/// CPU 时间差 → **单核万分比**(一个核跑满一整个窗口 = 10_000)。
///
/// **F179:口径和分辨率都是判据的一部分。** 旧版按核数归一成整数百分点,
/// 于是 16 核机器上 1 个显示百分点 = 0.16 个核 —— 单核 16% 以下**全部显示
/// 0%**,而 N1 要求的「空闲 < 1% 单核」比那个最小刻度还小 16 倍。实机 34
/// 分钟 286 个窗口的 p50/p95/max 全是 0%,**达标与超标十五倍长得一模一样**。
/// 万分比让 N1 的阈值(= 100)有两位有效数字,而单核口径让它不必知道核数
/// 就能直接读:`total=0.63%` 达标,`total=4.10%` 不达标,一眼可判。
///
/// 归一口径没有丢:`profile.cpu` 行同时报 `cores=`,除一下即可。
///
/// 饱和而非截断(同 [`crate::profile::thread_group_pct`]):病态的小窗口下
/// `as u32` 会绕回成一个像模像样的小数字,爆表成 `u32::MAX` 才一眼看得出
/// 该查上游。
///
/// `window_ns` 为 0(时钟没走 / 首次采样无基线)返回 `None`,不是 0 ——
/// 「采不到」和「真的是 0」在排障时是两回事,而且 `None` 不会打破空闲门。
pub fn cpu_bp(delta_ns: u64, window_ns: u64) -> Option<u32> {
    if window_ns == 0 {
        return None;
    }
    let bp = (delta_ns as u128) * 10_000 / (window_ns as u128);
    Some(bp.min(u32::MAX as u128) as u32)
}

/// CPU 探针。**有状态**:百分比是两次采样的差分,必须记住上一窗口。
///
/// 由看门狗线程持有。`main_thread` 句柄必须由**主线程**在 `new_on_main_thread`
/// 里取好传进来。
pub struct CpuProbe {
    prev_process_ns: Option<u64>,
    prev_main_ns: Option<u64>,
    cores: u32,
    #[cfg(windows)]
    main_thread: Option<MainThreadHandle>,
    #[cfg(target_os = "linux")]
    main_tid: u32,
}

/// 主线程句柄的自有拷贝。
///
/// **不能存 `GetCurrentThread()`**:那是个伪句柄(常量 `-2`),含义是
/// 「调用它的那个线程」—— 存进结构体传给看门狗线程之后,它指的是**看门狗
/// 线程自己**。症状是主线程 CPU% 恒等于零点几,而事件循环正忙转。
/// 静默错值,没有任何报错。
#[cfg(windows)]
struct MainThreadHandle(windows_sys::Win32::Foundation::HANDLE);

/// FILETIME(单位 100 纳秒)→ 纳秒。
///
/// 一份而不是三份:`read_cpu_ns`、线程枚举、缓存句柄读数都要它,各抄一遍
/// 的话「乘 100」这个换算就有三个改点。
#[cfg(windows)]
fn filetime_ns(ft: windows_sys::Win32::Foundation::FILETIME) -> u64 {
    (((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64) * 100
}

// SAFETY: HANDLE 是个内核对象句柄,跨线程使用是 Win32 的正常用法;
// 这里只读(GetThreadTimes),不改状态。
#[cfg(windows)]
unsafe impl Send for MainThreadHandle {}

impl CpuProbe {
    /// **必须在主线程上调用**(`main` / `start_watchdog` 里),之后把
    /// 整个 probe move 进看门狗线程。
    pub fn new_on_main_thread() -> Self {
        Self {
            prev_process_ns: None,
            prev_main_ns: None,
            cores: std::thread::available_parallelism().map_or(1, |n| n.get() as u32),
            #[cfg(windows)]
            main_thread: dup_current_thread(),
            #[cfg(target_os = "linux")]
            main_tid: linux_current_tid(),
        }
    }

    /// 这台机器的逻辑核数。**不参与百分比换算**(F179 之后两个读数都是单核
    /// 口径),只是渲染层要把它印进 `profile.cpu` 行,好让读日志的人能换算回
    /// 归一口径 —— 不印的话「180% 单核」在 4 核机和 32 核机上意味着完全不同
    /// 的两件事,而日志里看不出是哪台。
    pub fn cores(&self) -> u32 {
        self.cores
    }

    /// 采一次。首次调用没有基线,返回 `None`。
    pub fn sample(&mut self, window_ns: u64) -> Option<CpuSample> {
        let (proc_ns, main_ns) = read_cpu_ns(self)?;
        let d_proc = self.prev_process_ns.map(|p| proc_ns.saturating_sub(p));
        let d_main = self.prev_main_ns.map(|p| main_ns.saturating_sub(p));
        self.prev_process_ns = Some(proc_ns);
        self.prev_main_ns = Some(main_ns);
        Some(CpuSample {
            process_bp: cpu_bp(d_proc?, window_ns)?,
            main_thread_bp: cpu_bp(d_main?, window_ns)?,
        })
    }
}

#[cfg(windows)]
fn dup_current_thread() -> Option<MainThreadHandle> {
    use windows_sys::Win32::Foundation::{DuplicateHandle, DUPLICATE_SAME_ACCESS};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentThread};
    let mut out = std::ptr::null_mut();
    // SAFETY: 全部实参都是当前进程/线程的伪句柄与本地栈变量。
    let ok = unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            GetCurrentThread(),
            GetCurrentProcess(),
            &mut out,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    };
    (ok != 0).then_some(MainThreadHandle(out))
}

/// 返回 (进程累计 CPU 纳秒, 主线程累计 CPU 纳秒)。
#[cfg(windows)]
fn read_cpu_ns(p: &CpuProbe) -> Option<(u64, u64)> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetProcessTimes, GetThreadTimes,
    };

    let ns = filetime_ns;

    let mut c = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut e = c;
    let mut k = c;
    let mut u = c;
    // SAFETY: 四个 out 参数都是本地栈上的 FILETIME。
    let ok = unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) };
    if ok == 0 {
        return None;
    }
    let proc_ns = ns(k) + ns(u);

    let main_ns = match &p.main_thread {
        Some(h) => {
            let mut tk = c;
            let mut tu = c;
            // SAFETY: `h.0` 是 DuplicateHandle 给的自有句柄,四个 out 参数在栈上。
            let ok = unsafe { GetThreadTimes(h.0, &mut c, &mut e, &mut tk, &mut tu) };
            if ok == 0 {
                return None;
            }
            ns(tk) + ns(tu)
        }
        None => return None,
    };
    Some((proc_ns, main_ns))
}

#[cfg(target_os = "linux")]
fn read_cpu_ns(p: &CpuProbe) -> Option<(u64, u64)> {
    let hz = 100u64; // Linux 上 USER_HZ 恒为 100(内核 ABI,不随 CONFIG_HZ 变)
    let read = |path: &str| -> Option<u64> {
        let s = std::fs::read_to_string(path).ok()?;
        // comm 字段可能含空格和括号,从最后一个 ')' 之后开始切。
        let rest = &s[s.rfind(')')? + 1..];
        let f: Vec<&str> = rest.split_whitespace().collect();
        // 从 ')' 之后数:索引 0 = state(第 3 字段),故 utime(14) = 索引 11。
        let utime: u64 = f.get(11)?.parse().ok()?;
        let stime: u64 = f.get(12)?.parse().ok()?;
        Some((utime + stime) * 1_000_000_000 / hz)
    };
    let proc_ns = read("/proc/self/stat")?;
    let main_ns = read(&format!("/proc/self/task/{}/stat", p.main_tid))?;
    Some((proc_ns, main_ns))
}

/// 调用它的那个线程的真实 tid。
///
/// **不用 `std::process::id()`**:那只在「调用者恰好是进程的第一个线程」时
/// 才等于 tid —— 生产代码里成立(`new_on_main_thread` 从主线程调),但单测
/// 里 `#[test]` 函数跑在测试框架的工作线程上,`process::id()` 量的是别的
/// 线程,烧多少 CPU 都测不出来(静默恒零,不报错)。`/proc/thread-self` 是
/// 内核维护的「指向调用者自己」的符号链接,任何线程读它都得到自己的 tid,
/// 这正好和 Windows 分支里 `GetCurrentThread()` 伪句柄「指调用者自己」的
/// 语义对齐,两边同一份「必须在目标线程上调用」的契约。
#[cfg(target_os = "linux")]
fn linux_current_tid() -> u32 {
    std::fs::read_link("/proc/thread-self")
        .ok()
        .and_then(|p| p.file_name().and_then(|s| s.to_str()?.parse().ok()))
        .unwrap_or_else(std::process::id)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn read_cpu_ns(_p: &CpuProbe) -> Option<(u64, u64)> {
    None
}

/// 调用者自己的 tid。**必须在主线程上调**才能拿到主线程的
/// (同 T12 的教训:谁调谁的语义)。给 [`ThreadCpuProbe::new`] 喂 main_tid 用。
///
/// Windows / Linux 之外一律返回 `0`:那些平台上 [`ThreadCpuProbe::sample`]
/// 本来就恒返回 `None`,`main_tid` 传什么都不影响任何行为。
pub(crate) fn current_tid() -> u32 {
    #[cfg(windows)]
    {
        // SAFETY: 无参数,返回调用者自己的 tid。
        unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() }
    }
    #[cfg(target_os = "linux")]
    {
        linux_current_tid()
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        0
    }
}

/// F168:每线程 CPU 时间探针。有状态(差分),看门狗线程持有。
///
/// 固有缺陷(如实写在这,别试图修):
///
/// 1. 两次采样之间生灭的短命线程漏账。5 秒窗口 + 本项目线程全是长命线程,
///    可接受;真要精确得 hook 线程生命周期,超出日志的本分。
/// 2. **tid 复用**:线程退出后 tid 被新线程拿走,新线程的累计值从 0 起,
///    比 `prev` 里那个记录小,`saturating_sub` 把这一窗口的 delta 静默夹成 0
///    ——一个「看着正常」的假零值。Windows 回收 tid 比 Linux 积极,而
///    Windows 是本项目唯一的一等公民,所以这条不是纯理论。判为可接受:
///    只影响复用发生的那**一个**窗口,下一窗口差分就自愈了,而要根治得按
///    (tid, 线程创建时间)做键,那是给日志加一整套线程身份追踪。
/// 3. **F182,只在 Windows 上**:线程清单每 [`RESCAN_EVERY_WINDOWS`] 个窗口
///    才重扫一次,所以新线程最长要一分钟才进表,而且它进表的第一个窗口只
///    建基线、不报账(见 [`NewThread`])。代价是新线程头一分钟的 CPU 记不到,
///    换来的是不必每 5 秒快照一次全系统线程 —— 后者实测占了写盘窗口全部
///    CPU 的一大块。本项目的线程全是长命线程(tokio 池、看门狗、对话框、
///    拖出 STA),一分钟的发现延迟对「谁在烧」这个问题没有影响。
pub struct ThreadCpuProbe {
    /// tid → 上一窗口的累计 CPU ns。`None` = 还没建过基线(首次采样)。
    prev: Option<std::collections::HashMap<u32, u64>>,
    main_tid: u32,
    /// F182:缓存下来的线程清单(tid + 自有句柄 + 名字)。
    #[cfg(windows)]
    cached: Vec<CachedThread>,
    /// F182:距上次重扫过了几个窗口。`None` = 从没扫过。
    ///
    /// 用 `Option` 而不是「`u32::MAX` 当哨兵」:哨兵值迟早会被某个
    /// `>= RESCAN_EVERY_WINDOWS` 的比较悄悄吃掉,而 `None` 是编译器盯着的。
    #[cfg(windows)]
    since_rescan: Option<u32>,
    /// F182:上一窗口有线程读数失败(多半是句柄失效),下一窗口强制重扫。
    #[cfg(windows)]
    read_failed: bool,
}

/// F182:重扫线程清单的间隔(窗口数)。5 秒一个窗口 → 一分钟一次。
///
/// 这个数是**发现延迟**与**采样开销**的兑换比:调小则新线程更快进表,
/// 调大则全系统快照更少。一分钟的依据是「本项目的线程都是长命线程,
/// 且它们全在启动/连接的头几秒里创建完」。
pub const RESCAN_EVERY_WINDOWS: u32 = 12;

/// F182:重扫线程清单的判据。**纯函数,不 gate 在 `#[cfg(windows)]` 里**
/// —— 只有这样它才在开发机(Linux)上测得动,而这正是唯一容易写错的一段。
///
/// `read_failed` 优先于计数:句柄失效说明清单已经和现实对不上了,
/// 等满一分钟等于明知有错还继续报一分钟的错数。
pub fn needs_rescan(since_rescan: Option<u32>, read_failed: bool) -> bool {
    match since_rescan {
        None => true,
        Some(n) => read_failed || n >= RESCAN_EVERY_WINDOWS,
    }
}

/// F182:一个缓存下来的线程。
///
/// **名字只在重扫时取一次**:`GetThreadDescription` 每次都要让系统分配一段
/// 宽串再 `LocalFree`,而线程名从创建到退出不会变 —— 每 5 秒重取一遍是纯浪费。
#[cfg(windows)]
struct CachedThread {
    tid: u32,
    handle: windows_sys::Win32::Foundation::HANDLE,
    name: String,
}

// SAFETY: 同 `MainThreadHandle` —— 线程句柄跨线程只读使用是 Win32 的正常
// 用法,这里只 `GetThreadTimes`,不改状态。整个 probe 由看门狗线程独占。
#[cfg(windows)]
unsafe impl Send for CachedThread {}

/// F182:重扫时**新出现**的 tid 该怎么记账。
///
/// 两个平台的答案不一样,而这正是最容易抄错的地方:
///
/// - Linux 每个窗口都重新 readdir(`/proc/self/task` 只列自己进程,本来就
///   便宜),「新出现」= 这 5 秒里刚创建 —— 它的累计时间就是这一窗口的账,
///   [`ChargeFull`](NewThread::ChargeFull)。
/// - Windows 一分钟才重扫一次,「新出现」可能是 59 秒前创建的。照 Linux
///   那样全额算,会把一分钟的 CPU 塞进一个 5 秒窗口,报出 `1180%` 这种
///   **看着像着火了的假值**。所以只建基线、这一窗口记 0,
///   [`BaselineOnly`](NewThread::BaselineOnly)。
enum NewThread {
    ChargeFull,
    /// **只有 Windows 分支构造它。** 开发机(Linux)上 `cargo build` 会把它
    /// 判成 dead_code,而 `-D warnings` 把 dead_code 当错 —— 测试里的构造点
    /// 只在 `--all-targets` 下算数,救不了普通 build。
    #[cfg_attr(not(windows), allow(dead_code))]
    BaselineOnly,
}

impl ThreadCpuProbe {
    /// `main_tid` 必须是主线程的 tid —— 在主线程上调
    /// [`linux_current_tid`](自身,Linux)/`GetCurrentThreadId`(Windows)取好
    /// 传进来(同 T12 的教训:谁调谁的语义,存下来跨线程用就错了)。主线程
    /// 口径已有 `CpuProbe`(F164)更准地覆盖,这里的清单**排除**它,不重复计。
    pub fn new(main_tid: u32) -> Self {
        Self {
            prev: None,
            main_tid,
            #[cfg(windows)]
            cached: Vec::new(),
            #[cfg(windows)]
            since_rescan: None,
            #[cfg(windows)]
            read_failed: false,
        }
    }

    /// 拿这一窗口的读数与 `prev` 做差,然后整表换基线。
    ///
    /// **整表替换,不是增量 merge**:退出线程的旧 tid 就此从 `prev` 里消失,
    /// 不然 HashMap 会随线程生灭无限涨。
    ///
    /// 没有基线(首次调用)返回 `None`,不是 `Some(空表)`:空表到了分组层
    /// 就是「各组 0%」,一个凭空编出来的 0 —— 本文件头部明令禁止的那种。
    /// 同文件 `CpuProbe::sample` 也是这个约定。
    fn diff_and_rebase(
        &mut self,
        cur: std::collections::HashMap<u32, (String, u64)>,
        new_thread: NewThread,
    ) -> Option<Vec<(String, u64)>> {
        let out = self.prev.as_ref().map(|prev| {
            cur.iter()
                .map(|(tid, (name, ns))| {
                    let delta = match (prev.get(tid), &new_thread) {
                        (Some(p), _) => ns.saturating_sub(*p),
                        (None, NewThread::ChargeFull) => *ns,
                        (None, NewThread::BaselineOnly) => 0,
                    };
                    (name.clone(), delta)
                })
                .collect()
        });
        self.prev = Some(cur.into_iter().map(|(tid, (_, ns))| (tid, ns)).collect());
        out
    }

    /// 枚举全部线程,返回 (线程名, 这一窗口的 CPU ns 增量),**排除主线程**
    /// (F164 已有更准的主线程口径)。首次调用(只建基线,无差分可算)和
    /// 平台枚举失败一律返回 `None`,渲染层显示 n/a —— **不冒充 0**。
    #[cfg(target_os = "linux")]
    pub fn sample(&mut self) -> Option<Vec<(String, u64)>> {
        let hz = 100u64; // 同 `read_cpu_ns`:USER_HZ 恒为 100,内核 ABI,不随 CONFIG_HZ 变。
        let dir = std::fs::read_dir("/proc/self/task").ok()?;
        let mut cur: std::collections::HashMap<u32, (String, u64)> =
            std::collections::HashMap::new();
        for entry in dir.flatten() {
            let Some(tid) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.parse::<u32>().ok())
            else {
                continue;
            };
            if tid == self.main_tid {
                continue;
            }
            // 线程可能在枚举途中退出,读不到就跳过它,不让整次采样失败。
            let Some(ns) = std::fs::read_to_string(format!("/proc/self/task/{tid}/stat"))
                .ok()
                .and_then(|s| {
                    // comm 字段可能含空格和括号,从最后一个 ')' 之后开始切,
                    // 手法照抄 `read_cpu_ns` 的 Linux 分支。
                    let rest = &s[s.rfind(')')? + 1..];
                    let f: Vec<&str> = rest.split_whitespace().collect();
                    // 从 ')' 之后数:索引 0 = state(第 3 字段),故 utime(14) = 索引 11。
                    let utime: u64 = f.get(11)?.parse().ok()?;
                    let stime: u64 = f.get(12)?.parse().ok()?;
                    Some((utime + stime) * 1_000_000_000 / hz)
                })
            else {
                continue;
            };
            let name = std::fs::read_to_string(format!("/proc/self/task/{tid}/comm"))
                .map(|s| s.trim_end().to_string())
                .unwrap_or_default();
            cur.insert(tid, (name, ns));
        }
        // Linux 每个窗口都重新列一遍,所以「新出现」就是这 5 秒里刚创建的,
        // 全额算(理由见 `NewThread`)。
        self.diff_and_rebase(cur, NewThread::ChargeFull)
    }

    /// 同上,Windows 分支。**已在 Windows 11 实机验证过**(v0.1.79,2026-08-28
    /// 的 331 个窗口):分组读数合理(`tokio:0.00% watchdog:0.92%`,空闲时
    /// 只有看门狗在动),FFI 调用序列成立。
    ///
    /// **F182:每个窗口只读缓存句柄,不再重新枚举。** 原实现每 5 秒调一次
    /// `CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD)` —— 那**快照的是全系统
    /// 所有线程**(见 `rescan` 里的注释),典型 Windows 桌面上几千个,而我们
    /// 只要自己进程的十几个。线程句柄和线程名都不随窗口变,存下来即可;
    /// 只有「发现新线程」需要重扫,而那按 [`RESCAN_EVERY_WINDOWS`] 的节奏走。
    #[cfg(windows)]
    pub fn sample(&mut self) -> Option<Vec<(String, u64)>> {
        if needs_rescan(self.since_rescan, self.read_failed) {
            self.rescan();
        }
        // **空表只可能是枚举失败**:本进程恒有 tokio 池这些长命线程,
        // 「一个别的线程都没有」不是真实状态。走到 `diff_and_rebase` 的话
        // 它会给出 `Some(空表)`,到分组层就是「各组 0.00%」—— 本文件头部
        // 明令禁止的那种凭空的 0,而且它比 `n/a` 更难发现:一整屏 0.00%
        // 看着就像「确实没占用」。
        if self.cached.is_empty() {
            return None;
        }

        let mut cur: std::collections::HashMap<u32, (String, u64)> =
            std::collections::HashMap::new();
        let mut failed = false;
        for t in &self.cached {
            match thread_cpu_ns(t.handle) {
                Some(ns) => {
                    cur.insert(t.tid, (t.name.clone(), ns));
                }
                // 句柄失效(理论上不该发生:句柄本身让线程对象活着,线程
                // 退出后 GetThreadTimes 照样返回最终值)。真发生了就下一窗口
                // 重扫,而不是把这个线程静默从表里漏掉。
                None => failed = true,
            }
        }
        self.read_failed = failed;
        self.since_rescan = Some(self.since_rescan.map_or(1, |n| n.saturating_add(1)));

        // Windows 一分钟才重扫一次,新 tid 只建基线不报账(理由见 `NewThread`)。
        self.diff_and_rebase(cur, NewThread::BaselineOnly)
    }

    /// F182:重建线程清单。**这是唯一贵的一步**,按 [`RESCAN_EVERY_WINDOWS`]
    /// 的节奏调用。
    #[cfg(windows)]
    fn rescan(&mut self) {
        use windows_sys::Win32::Foundation::{
            CloseHandle, LocalFree, HANDLE, HLOCAL, INVALID_HANDLE_VALUE,
        };
        use windows_sys::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
        };
        use windows_sys::Win32::System::Threading::{
            GetCurrentProcessId, GetThreadDescription, OpenThread, THREAD_QUERY_LIMITED_INFORMATION,
        };

        // 先把上一轮的句柄还回去,再建新表 —— 不还的话每分钟泄漏一批
        // 内核对象,一天下来是几千个。
        self.close_cached();

        // SAFETY: `TH32CS_SNAPTHREAD` 下第二个参数被忽略,传 0。
        //
        // **它快照的是「全系统所有线程」,不是本进程的。** MSDN 明写着要靠
        // `THREADENTRY32::th32OwnerProcessID` 自己筛(下面那个 `== pid` 就是),
        // 而这一条正是 F182 要躲开的开销:典型 Windows 桌面几千个线程,
        // 内核要遍历每个进程的线程链表,系统越忙越慢。别把这行挪回每窗口。
        let snap: HANDLE = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snap == INVALID_HANDLE_VALUE {
            // 扫不到就保持空表 —— 下一窗口 `needs_rescan` 会因为
            // `since_rescan` 归零而**不**立刻重试,一分钟后再试一次。
            // 这里不 return 前先记账,免得变成每窗口重试(那就是改回原样了)。
            self.since_rescan = Some(0);
            return;
        }
        // SAFETY: 无参数,返回调用者自己的进程 id。
        let pid = unsafe { GetCurrentProcessId() };
        let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
        // 必须先填 dwSize,否则 Thread32First 直接失败。
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;

        // SAFETY: `entry.dwSize` 已按结构体大小填好,`snap` 刚创建有效。
        let mut ok = unsafe { Thread32First(snap, &mut entry) };
        while ok != 0 {
            if entry.th32OwnerProcessID == pid && entry.th32ThreadID != self.main_tid {
                let tid = entry.th32ThreadID;
                // SAFETY: `tid` 来自本进程的快照。句柄**存下来**跨窗口用 ——
                // 这与 T12 那条不冲突:T12 禁的是伪句柄,这里是 OpenThread
                // 给的自有句柄,指名道姓地指着那个 tid。
                let h: HANDLE = unsafe { OpenThread(THREAD_QUERY_LIMITED_INFORMATION, 0, tid) };
                if !h.is_null() {
                    let mut desc: *mut u16 = std::ptr::null_mut();
                    // SAFETY: `h` 有效,`desc` 是本地栈变量。
                    let hr = unsafe { GetThreadDescription(h, &mut desc) };
                    let name = if hr >= 0 && !desc.is_null() {
                        // SAFETY: `desc` 是 GetThreadDescription 成功时给出的
                        // NUL 结尾宽串,读完立刻用 LocalFree 释放。
                        let s = unsafe {
                            let mut len = 0usize;
                            while *desc.add(len) != 0 {
                                len += 1;
                            }
                            String::from_utf16_lossy(std::slice::from_raw_parts(desc, len))
                        };
                        // SAFETY: `desc` 是 GetThreadDescription 分配的,用完释放。
                        unsafe { LocalFree(desc as HLOCAL) };
                        s
                    } else {
                        String::new() // 空名原样返回,分组层管占位。
                    };
                    self.cached.push(CachedThread {
                        tid,
                        handle: h,
                        name,
                    });
                }
            }
            // SAFETY: `snap` 仍有效,`entry` 复用同一块栈内存,由 Thread32Next 重填。
            ok = unsafe { Thread32Next(snap, &mut entry) };
        }
        // SAFETY: `snap` 是 CreateToolhelp32Snapshot 给的自有句柄。
        unsafe { CloseHandle(snap) };

        self.since_rescan = Some(0);
        self.read_failed = false;
    }

    #[cfg(windows)]
    fn close_cached(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        for t in self.cached.drain(..) {
            // SAFETY: `t.handle` 是本结构体从 OpenThread 拿到的自有句柄,
            // 只在这里关,关完立刻从 `cached` 里移走(drain)。
            unsafe { CloseHandle(t.handle) };
        }
    }

    #[cfg(not(any(windows, target_os = "linux")))]
    pub fn sample(&mut self) -> Option<Vec<(String, u64)>> {
        None
    }
}

// F182:句柄要跟着 probe 一起还回去。看门狗线程活到进程结束,所以这个 Drop
// 实际上极少跑 —— 写它是因为单测里 probe 会反复构造析构,而泄漏的句柄
// 在测试进程里同样是泄漏。
#[cfg(windows)]
impl Drop for ThreadCpuProbe {
    fn drop(&mut self) {
        self.close_cached();
    }
}

/// F182:读一个已缓存句柄的累计 CPU 纳秒。
///
/// 线程退出后这里**照样成功**并返回最终值(句柄让线程对象活着),于是它的
/// delta 恒为 0、在表里显示 0% —— 直到下次重扫把它清掉。这是有意的:
/// 报一个不再变化的 0,好过让它从表里静默消失。
#[cfg(windows)]
fn thread_cpu_ns(h: windows_sys::Win32::Foundation::HANDLE) -> Option<u64> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::GetThreadTimes;
    let mut c = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut e = c;
    let mut k = c;
    let mut u = c;
    // SAFETY: `h` 是 `rescan` 里 OpenThread 得到的自有句柄,四个 out 参数在栈上。
    let ok = unsafe { GetThreadTimes(h, &mut c, &mut e, &mut k, &mut u) };
    (ok != 0).then(|| filetime_ns(k) + filetime_ns(u))
}

/// 一次 GPU 引擎占用采样。`engines` 已按占用倒序,最多两项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuSample {
    pub engines: Vec<(String, u8)>,
}

/// PDH 的 `\GPU Engine(*)` 实例名 → 本进程的引擎类型。
///
/// 实例名形如
/// `pid_1234_luid_0x00000000_0x0000C4C1_phys_0_eng_0_engtype_3D`。
///
/// 前缀带尾随下划线是必须的:`pid_1234` 会前缀匹配上 `pid_12345`,
/// 把邻居进程的 GPU 占用算到自己头上 —— 串号比不匹配难查得多。
///
/// **纯函数,且不在 `#[cfg(windows)]` 里**:Linux 上也编译,这样解析
/// 逻辑在开发机上就测得动。真正碰 PDH 的部分才 gate。
pub fn engine_of(instance: &str, pid: u32) -> Option<&str> {
    let rest = instance.strip_prefix(&format!("pid_{pid}_"))?;
    let at = rest.rfind("_engtype_")?;
    let ty = &rest[at + "_engtype_".len()..];
    (!ty.is_empty()).then_some(ty)
}

/// 一批 (实例名, 占用率) → 本进程按引擎类型聚合的前两名。
///
/// **求和而非取最大**:同一个 engtype 会在多个 `eng_N` 实例上各报一部分,
/// 取最大会系统性低报。求和之后可能超 100(多引擎并行),夹紧。
///
/// 只取前两名:全列出来一行放不下,而排在后面的恒定是零。
pub fn aggregate_engines(items: &[(String, f64)], pid: u32) -> Vec<(String, u8)> {
    let mut by_type: std::collections::BTreeMap<&str, f64> = std::collections::BTreeMap::new();
    for (name, v) in items {
        if let Some(ty) = engine_of(name, pid) {
            *by_type.entry(ty).or_insert(0.0) += v;
        }
    }
    let mut out: Vec<(String, u8)> = by_type
        .into_iter()
        .filter(|(_, v)| *v >= 0.5)
        .map(|(k, v)| (k.to_string(), v.clamp(0.0, 100.0).round() as u8))
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out.truncate(2);
    out
}

/// GPU 引擎占用探针。**有状态**:PDH 是速率型计数器,查询句柄必须常驻,
/// 且第一次 `PdhCollectQueryData` 只作基线、不出数。
pub struct GpuProbe {
    #[cfg(windows)]
    inner: Option<PdhQuery>,
    #[cfg(windows)]
    primed: bool,
}

#[cfg(windows)]
struct PdhQuery {
    query: isize,
    counter: isize,
}

// SAFETY: PDH 句柄是进程级的不透明整数,跨线程使用是 PDH 的正常用法;
// 本结构体只被看门狗线程独占持有。
#[cfg(windows)]
unsafe impl Send for PdhQuery {}

impl GpuProbe {
    pub fn new() -> Self {
        Self {
            #[cfg(windows)]
            inner: open_pdh(),
            #[cfg(windows)]
            primed: false,
        }
    }

    /// 采一次。首次调用只作基线,返回 `None`。
    #[cfg(windows)]
    pub fn sample(&mut self) -> Option<GpuSample> {
        let q = self.inner.as_ref()?;
        // SAFETY: `q.query` 由 PdhOpenQueryW 得到,本结构体存活期间有效。
        if unsafe { windows_sys::Win32::System::Performance::PdhCollectQueryData(q.query) } != 0 {
            return None;
        }
        if !self.primed {
            // 速率型计数器要两次采集才有值,第一次只是基线。
            self.primed = true;
            return None;
        }
        let items = read_counter_array(q.counter)?;
        Some(GpuSample {
            engines: aggregate_engines(&items, std::process::id()),
        })
    }

    #[cfg(not(windows))]
    pub fn sample(&mut self) -> Option<GpuSample> {
        None
    }
}

impl Default for GpuProbe {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn open_pdh() -> Option<PdhQuery> {
    use windows_sys::Win32::System::Performance::{PdhAddEnglishCounterW, PdhOpenQueryW};
    let mut query = 0isize;
    // SAFETY: 两个 out 参数在栈上;`wide` 给的是 NUL 结尾的 UTF-16。
    if unsafe { PdhOpenQueryW(std::ptr::null(), 0, &mut query) } != 0 {
        return None;
    }
    let mut counter = 0isize;
    // **必须是 `PdhAddEnglishCounterW`**:`PdhAddCounterW` 吃的是**本地化**
    // 计数器名,中文 Windows 上这条路径根本找不到 —— 而且是运行期静默失败,
    // 编译和本机测试全绿。
    let path = wide(r"\GPU Engine(*)\Utilization Percentage");
    if unsafe { PdhAddEnglishCounterW(query, path.as_ptr(), 0, &mut counter) } != 0 {
        return None;
    }
    Some(PdhQuery { query, counter })
}

/// 读一次计数器数组。两趟调用:先问要多大缓冲,再取数据。
#[cfg(windows)]
fn read_counter_array(counter: isize) -> Option<Vec<(String, f64)>> {
    use windows_sys::Win32::System::Performance::{
        PdhGetFormattedCounterArrayW, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE,
    };
    let mut size = 0u32;
    let mut count = 0u32;
    // SAFETY: 第一趟传空缓冲,PDH 用 `size` 回报所需字节数(返回
    // PDH_MORE_DATA,非 0,所以这里不检查返回值,只看 size)。
    unsafe {
        PdhGetFormattedCounterArrayW(
            counter,
            PDH_FMT_DOUBLE,
            &mut size,
            &mut count,
            std::ptr::null_mut(),
        )
    };
    if size == 0 {
        return None;
    }
    let n = (size as usize).div_ceil(std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>()) + 1;
    let mut buf: Vec<PDH_FMT_COUNTERVALUE_ITEM_W> = Vec::with_capacity(n);
    // 告诉 PDH 缓冲实际有多大(字节数),而不是沿用第一趟回报的 `size`:
    // `with_capacity(n)` 多留了一项余量,实际分配的字节数 ≥ 第一趟的回报值。
    // 用小的那个值当入参是安全的(PDH 不会写超出它所知道的范围),但如果
    // PDH 在两趟之间因为并发的另一次 `PdhCollectQueryData` 让实例数变多,
    // 传一个偏小的 `size` 会让第二趟又返回 PDH_MORE_DATA、永远读不到数。
    // 按实际分配的字节数回填,消除这个缝隙。
    size = (n * std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>()) as u32;
    // SAFETY: 容量已按 PDH 回报的字节数算好并多留一项;PDH 负责填充,
    // 之后只读前 `count` 项。
    let ok = unsafe {
        PdhGetFormattedCounterArrayW(
            counter,
            PDH_FMT_DOUBLE,
            &mut size,
            &mut count,
            buf.as_mut_ptr(),
        )
    };
    if ok != 0 {
        return None;
    }
    // SAFETY: PDH 成功返回,前 `count` 项已初始化。
    unsafe { buf.set_len(count as usize) };

    let mut out = Vec::with_capacity(buf.len());
    for it in &buf {
        if it.szName.is_null() {
            continue;
        }
        // SAFETY: `szName` 指向 PDH 填在同一块缓冲尾部的 NUL 结尾宽串。
        let name = unsafe {
            let mut len = 0usize;
            while *it.szName.add(len) != 0 {
                len += 1;
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(it.szName, len))
        };
        // SAFETY: 用 PDH_FMT_DOUBLE 取的数,联合体里有效的是 doubleValue。
        let v = unsafe { it.FmtValue.Anonymous.doubleValue };
        if v.is_finite() {
            out.push((name, v));
        }
    }
    Some(out)
}

/// 一次显存采样(本进程的本地显存)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VramSample {
    pub used_mb: u64,
    pub budget_mb: u64,
}

/// 显存探针。DXGI adapter **枚举一次常驻** —— 每 5 秒重新
/// `CreateDXGIFactory1` + `EnumAdapters1` 是白花的系统调用。
pub struct VramProbe {
    #[cfg(windows)]
    adapter: Option<windows::Win32::Graphics::Dxgi::IDXGIAdapter3>,
}

// SAFETY: `IDXGIAdapter3` 是 free-threaded 的(DXGI 对象不属于 COM 单元),
// 且这里只做只读查询。放进 `OnceLock` 需要这两个约束。
#[cfg(windows)]
unsafe impl Send for VramProbe {}
#[cfg(windows)]
unsafe impl Sync for VramProbe {}

impl VramProbe {
    /// `vendor`/`device` 来自 `wgpu::AdapterInfo`,用来在多显卡机器上
    /// 认出 wgpu 实际在用的那一块。
    #[cfg(windows)]
    pub fn new(vendor: u32, device: u32) -> Self {
        Self {
            adapter: find_adapter(vendor, device),
        }
    }

    /// 非 Windows 上这个结构体**没有字段**,所以构造器也得分开写 ——
    /// 把 `adapter: None` 写在 `#[cfg(not(windows))]` 属性下是编不过的
    /// (属性只能去掉字段初始化,去不掉「结构体没有这个字段」)。
    #[cfg(not(windows))]
    pub fn new(_vendor: u32, _device: u32) -> Self {
        Self {}
    }

    #[cfg(windows)]
    pub fn sample(&self) -> Option<VramSample> {
        use windows::Win32::Graphics::Dxgi::{
            DXGI_MEMORY_SEGMENT_GROUP_LOCAL, DXGI_QUERY_VIDEO_MEMORY_INFO,
        };
        let a = self.adapter.as_ref()?;
        let mut info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
        // SAFETY: `a` 是活着的 COM 接口;out 参数在栈上。
        unsafe { a.QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &mut info) }.ok()?;
        const MB: u64 = 1024 * 1024;
        Some(VramSample {
            used_mb: info.CurrentUsage / MB,
            budget_mb: info.Budget / MB,
        })
    }

    #[cfg(not(windows))]
    pub fn sample(&self) -> Option<VramSample> {
        None
    }
}

/// 按 vendor/device 找出 wgpu 在用的那块 adapter。
///
/// `QueryVideoMemoryInfo` 报的是**本进程**的用量,与 wgpu 实际选了 D3D12
/// 还是 Vulkan 无关 —— DXGI 在驱动层统计,不看是哪个 API 申请的。
///
/// 已知限制:两块同型号 GPU 时取枚举到的第一块。
#[cfg(windows)]
fn find_adapter(vendor: u32, device: u32) -> Option<windows::Win32::Graphics::Dxgi::IDXGIAdapter3> {
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter3, IDXGIFactory1};
    // SAFETY: CreateDXGIFactory1 是 free-threaded 的,不需要先 CoInitialize。
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.ok()?;
    for i in 0..16u32 {
        // SAFETY: 索引越界时返回 DXGI_ERROR_NOT_FOUND,由 `ok()?` 收掉。
        let Ok(a1) = (unsafe { factory.EnumAdapters1(i) }) else {
            break;
        };
        // SAFETY: `a1` 刚由 EnumAdapters1 返回,有效。
        let Ok(desc) = (unsafe { a1.GetDesc1() }) else {
            continue;
        };
        if desc.VendorId == vendor && desc.DeviceId == device {
            return a1.cast::<IDXGIAdapter3>().ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **F179:分辨率必须容得下 N1。**
    ///
    /// 这是本模块唯一一条「写错了也全绿、只有真机看得出」的判据 —— 而且
    /// 它已经真的发生过一次:旧口径(按核数归一的整数百分点)在 16 核机上
    /// 把 N1 的整个达标区间和它十五倍的超标区间一起量化成了 `0%`,286 个
    /// 实机窗口无一例外。**指标显示 0% 和指标测不了长得一样。**
    ///
    /// 自证会变红:把 `cpu_bp` 里的 `10_000` 改成 `100`(退回整数百分点),
    /// 或给它乘回 `cores` 那个归一除数(N1 阈值会掉到 6bp,与十分之一档
    /// 无法区分)。
    #[test]
    fn the_n1_threshold_is_still_ten_ticks_above_zero() {
        let window = 5_000_000_000u64; // 5s
                                       // N1 的阈值本身:1% 单核。
        assert_eq!(cpu_bp(window / 100, window), Some(100), "1% 单核该是 100bp");
        // 十分之一个 N1 仍然不能被量化成 0 —— 否则「达标」与「测不到」同形。
        assert_eq!(cpu_bp(window / 1_000, window), Some(10));
        // 一个核跑满一整个窗口 = 10_000bp,与核数无关。
        assert_eq!(cpu_bp(window, window), Some(10_000));
    }

    /// 进程口径**不封顶**:多核并行是常态,夹到一个核就把最该看见的读数削掉了。
    ///
    /// 自证会变红:给 `cpu_bp` 加 `.min(10_000)`。
    #[test]
    fn eight_cores_of_work_reads_as_eight_hundred_percent() {
        assert_eq!(cpu_bp(40_000_000_000, 5_000_000_000), Some(80_000));
    }

    /// 荒谬输入要爆表,不许截断绕回(同 `thread_group_pct` 的理由)。
    ///
    /// 自证会变红:把 `.min(u32::MAX as u128)` 换成裸 `as u32`。
    #[test]
    fn a_pathological_window_saturates_instead_of_wrapping() {
        // 恰好整除 2^32 的一组:裸 `as u32` 会静默给出 0,一个「看着完全
        // 正常」的错值。
        assert_eq!(cpu_bp(429_496_729_600_000, 1_000_000), Some(u32::MAX));
    }

    /// 采不到时是 `None` 而不是 0。
    ///
    /// 0 会被空闲门读成「真空闲」,而 `None` 不打破空闲门也不冒充数据。
    ///
    /// 自证会变红:把 `cpu_bp` 的 `return None` 改成 `return Some(0)`。
    #[test]
    fn an_unusable_window_yields_nothing_rather_than_a_fake_zero() {
        assert_eq!(cpu_bp(1_000, 0), None);
    }

    /// 本平台真的采得到 CPU 时间,且第二次采样能算出百分比。
    ///
    /// 只测纯函数是不够的:`read_cpu_ns` 的字段下标错一位(Linux 的
    /// `/proc/self/stat` 尤其容易,comm 里带空格会把 split 打乱)会让
    /// 数字变成一个看起来正常的错值。
    ///
    /// 自证会变红:把 Linux 分支的 `f.get(11)` 改成 `f.get(10)`
    /// (那是 cminflt,只会在 fork 时变,烧 CPU 也不涨)。
    #[test]
    #[cfg(any(windows, target_os = "linux"))]
    fn this_platform_reports_cpu_time_that_actually_grows_when_we_burn_cpu() {
        let mut p = CpuProbe::new_on_main_thread();
        assert_eq!(p.sample(1_000_000_000), None, "首次采样没有基线,该是 None");
        // 在主线程上烧掉一小段真实 CPU。
        let start = std::time::Instant::now();
        let mut x = 0u64;
        while start.elapsed() < std::time::Duration::from_millis(150) {
            x = x.wrapping_add(1);
        }
        std::hint::black_box(x);
        let window_ns = start.elapsed().as_nanos() as u64;
        let s = p.sample(window_ns).expect("第二次采样该有值");
        assert!(
            s.main_thread_bp > 5_000,
            "刚把主线程跑满 150ms,主线程口径只报了 {}bp",
            s.main_thread_bp
        );
    }

    /// PDH 的 GPU Engine 实例名解析。
    ///
    /// 真实形状(Windows 11,任务管理器读的是同一批计数器):
    /// `pid_1234_luid_0x00000000_0x0000C4C1_phys_0_eng_0_engtype_3D`
    ///
    /// 这一段是本切片里最容易写错、又最难发现的:解析错了 `gpu=` 恒为
    /// `0%` 或 `n/a`,而那和「这台机器真的没在用 GPU」长得一模一样。
    ///
    /// 自证会变红:把 `engine_of` 里的 `_engtype_` 改成 `_eng_`。
    #[test]
    fn a_gpu_engine_instance_name_yields_its_engine_type() {
        let n = "pid_1234_luid_0x00000000_0x0000C4C1_phys_0_eng_0_engtype_3D";
        assert_eq!(engine_of(n, 1234), Some("3D"));
        assert_eq!(
            engine_of(
                "pid_1234_luid_0x0_0x1_phys_0_eng_2_engtype_VideoDecode",
                1234
            ),
            Some("VideoDecode")
        );
    }

    /// **别的进程的实例必须被滤掉**。
    ///
    /// 不滤的话报出来的是整机 GPU 占用 —— 排障时会把「另一个程序在渲染」
    /// 读成「mullion 在烧 GPU」。
    ///
    /// `pid_12345` 不能被 `pid_1234` 前缀匹配上:那是 10 倍的邻居 pid,
    /// 这种串号比不匹配更难查。
    ///
    /// 自证会变红:把 `engine_of` 里的前缀改成 `format!("pid_{pid}")`(少个下划线)。
    #[test]
    fn another_process_engine_is_filtered_out_including_the_prefix_neighbour() {
        let other = "pid_9999_luid_0x0_0x1_phys_0_eng_0_engtype_3D";
        assert_eq!(engine_of(other, 1234), None);
        let neighbour = "pid_12345_luid_0x0_0x1_phys_0_eng_0_engtype_3D";
        assert_eq!(
            engine_of(neighbour, 1234),
            None,
            "pid_12345 被 pid_1234 前缀匹配上了 —— 串号比不匹配更难查"
        );
    }

    /// 按引擎类型聚合求和,倒序取前两名。
    ///
    /// 求和而非取最大:同一个 engtype 在多个 `eng_N` 实例上各报一部分,
    /// 取最大会系统性低报。
    ///
    /// 自证会变红:把 `aggregate_engines` 里的 `+=` 改成 `=`。
    #[test]
    fn engines_of_the_same_type_are_summed_and_the_top_two_win() {
        let items = vec![
            ("pid_7_luid_a_b_phys_0_eng_0_engtype_3D".to_string(), 8.0),
            ("pid_7_luid_a_b_phys_0_eng_1_engtype_3D".to_string(), 6.0),
            ("pid_7_luid_a_b_phys_0_eng_2_engtype_Copy".to_string(), 3.0),
            (
                "pid_7_luid_a_b_phys_0_eng_3_engtype_VideoDecode".to_string(),
                0.0,
            ),
            ("pid_8_luid_a_b_phys_0_eng_0_engtype_3D".to_string(), 90.0),
        ];
        let got = aggregate_engines(&items, 7);
        assert_eq!(
            got,
            vec![("3D".to_string(), 14), ("Copy".to_string(), 3)],
            "同类型没求和 / 没倒序 / 零值没滤掉 / 别的 pid 混进来了"
        );
    }

    /// 百分比要夹紧到 100:多引擎求和很容易超。
    ///
    /// 自证会变红:把 `aggregate_engines` 里的 `.clamp(0.0, 100.0)` 删掉。
    #[test]
    fn a_summed_utilisation_over_one_hundred_is_clamped() {
        let items = vec![
            ("pid_7_luid_a_b_phys_0_eng_0_engtype_3D".to_string(), 80.0),
            ("pid_7_luid_a_b_phys_0_eng_1_engtype_3D".to_string(), 70.0),
        ];
        assert_eq!(aggregate_engines(&items, 7), vec![("3D".to_string(), 100)]);
    }

    /// F168:线程枚举必须真的按线程分账。在一条命名线程里烧 CPU,
    /// 采样结果里该线程的 delta 必须显著大于零,且**主线程不在清单里**。
    ///
    /// 排除主线程的证法:另起一条命名线程,把它自己的 tid 当作
    /// `main_tid` 传给一个**独立的第二个 probe**,让它也烧 CPU —— 如果
    /// 排除逻辑是错的,它必然会在自己那个 probe 的清单里看见自己。
    /// 用同一个 probe 校验「自己的 tid 不在清单里」不够直接:测试线程
    /// 的 comm 名不可控(测试框架起的),没法按名字断言。
    ///
    /// 命名限制:Linux `comm` 只有 15 个可见字符(TASK_COMM_LEN=16 含
    /// NUL),线程名必须短于它,否则 `Builder::name` 起的名字会被截断,
    /// 名字比对必然对不上。
    ///
    /// 自证会变红:把 `sample` 里「跳过 main_tid」的判断删掉(主线程混入),
    /// 或把 delta 计算的新旧值弄反(全是 0)。
    #[test]
    #[cfg(target_os = "linux")]
    fn a_burning_named_thread_shows_up_with_its_name_and_main_is_excluded() {
        use std::sync::mpsc;

        fn burn_for(dur: std::time::Duration) {
            let start = std::time::Instant::now();
            let mut x = 0u64;
            while start.elapsed() < dur {
                x = x.wrapping_add(1);
            }
            std::hint::black_box(x);
        }

        // 线程 1:待验证对象,烧 CPU 后清单里必须看得见它。
        let (burn_tid_tx, burn_tid_rx) = mpsc::channel::<u32>();
        let (burn_done_tx, burn_done_rx) = mpsc::channel::<()>();
        let (release_burn_tx, release_burn_rx) = mpsc::channel::<()>();
        let burner = std::thread::Builder::new()
            .name("mullion-burn".to_string())
            .spawn(move || {
                burn_tid_tx.send(linux_current_tid()).unwrap();
                burn_for(std::time::Duration::from_millis(300));
                burn_done_tx.send(()).unwrap();
                // 烧完之后阻塞等主线程放行,保证 sample 时它(以及
                // /proc/self/task/<tid>)还活着,不会被 join 提前收尸。
                let _ = release_burn_rx.recv();
            })
            .unwrap();
        let _burn_tid = burn_tid_rx.recv().unwrap(); // 只用来确认线程已起跑,tid 本身不需要。

        // 线程 2:排除对象,自己烧 CPU 却不该出现在以自己为 main_tid
        // 的 probe 清单里。
        let (excl_tid_tx, excl_tid_rx) = mpsc::channel::<u32>();
        let (excl_done_tx, excl_done_rx) = mpsc::channel::<()>();
        let (release_excl_tx, release_excl_rx) = mpsc::channel::<()>();
        let excluded = std::thread::Builder::new()
            .name("mullion-excl".to_string())
            .spawn(move || {
                excl_tid_tx.send(linux_current_tid()).unwrap();
                burn_for(std::time::Duration::from_millis(300));
                excl_done_tx.send(()).unwrap();
                let _ = release_excl_rx.recv();
            })
            .unwrap();
        let excl_tid = excl_tid_rx.recv().unwrap();

        let mut probe = ThreadCpuProbe::new(linux_current_tid());
        let mut probe_excl = ThreadCpuProbe::new(excl_tid);
        // 首次采样只建基线,没有差分可算 —— 必须是 n/a,不是「各组 0%」。
        assert_eq!(probe.sample(), None, "首次采样无基线,不许编 0 出来");
        assert_eq!(probe_excl.sample(), None, "首次采样无基线,不许编 0 出来");

        burn_done_rx.recv().unwrap();
        excl_done_rx.recv().unwrap();

        let list = probe.sample().expect("Linux 分支应该采得到");
        let list_excl = probe_excl.sample().expect("Linux 分支应该采得到");

        release_burn_tx.send(()).unwrap();
        release_excl_tx.send(()).unwrap();
        burner.join().unwrap();
        excluded.join().unwrap();

        let entry = list
            .iter()
            .find(|(name, _)| name == "mullion-burn")
            .unwrap_or_else(|| panic!("清单里没找到 mullion-burn,得到:{list:?}"));
        assert!(
            entry.1 > 50_000_000,
            "烧了 300ms CPU,delta 只有 {} ns",
            entry.1
        );

        assert!(
            !list_excl.iter().any(|(name, _)| name == "mullion-excl"),
            "main_tid 对应的线程不该出现在清单里,得到:{list_excl:?}"
        );
    }

    /// F182:重扫节奏的判据。**这段是 Windows 分支唯一容易写错、又刚好
    /// 平台无关的逻辑**,所以它没 gate 在 `#[cfg(windows)]` 里 —— gate 进去
    /// 的话开发机上一行都跑不到,只能靠交叉编译过不过来「验证」,而那验的
    /// 是语法不是判据。
    ///
    /// 三条各管一件事:
    /// - 从没扫过必须扫(否则清单永远是空的,整表 n/a,静默)。
    /// - 没到期就别扫(这条**就是** F182 的全部价值:每 5 秒一次全系统
    ///   线程快照,实测占了写盘窗口全部 CPU 的一大块)。
    /// - 读数失败要立刻扫,不等到期(清单已经和现实对不上了,等满一分钟
    ///   等于明知有错还继续报一分钟的错数)。
    ///
    /// 自证会变红:把 `needs_rescan` 里的 `read_failed ||` 删掉(第三条红),
    /// 或把 `n >= RESCAN_EVERY_WINDOWS` 改成 `true`(第二条红),
    /// 或把 `None => true` 改成 `None => false`(第一条红)。
    #[test]
    fn the_thread_list_is_rescanned_on_a_slow_cadence_not_every_window() {
        assert!(needs_rescan(None, false), "从没扫过必须扫");
        assert!(
            !needs_rescan(Some(0), false),
            "刚扫完不该再扫 —— 每窗口重扫就是 F182 要去掉的那件事"
        );
        assert!(
            !needs_rescan(Some(RESCAN_EVERY_WINDOWS - 1), false),
            "没到期不该扫"
        );
        assert!(needs_rescan(Some(RESCAN_EVERY_WINDOWS), false), "到期该扫");
        assert!(needs_rescan(Some(0), true), "读数失败要立刻重扫,不等到期");
    }

    /// F182:**一分钟才发现一次新线程,那它进表的第一个窗口只能建基线。**
    ///
    /// 照 Linux 那样「新 tid 全额算」的话,一条 59 秒前创建、期间烧了 3 秒
    /// CPU 的线程,会把这 3 秒塞进一个 5 秒窗口 —— 报出 `60%` 这种**看着
    /// 像着火了的假值**,而它其实只是被发现得晚。这类假值比漏账危险得多:
    /// 漏账让人以为没事,假值让人去修一个不存在的问题。
    ///
    /// 两种取法在同一份输入上必须给出不同答案,否则这条判据是空的。
    ///
    /// 自证会变红:把 `diff_and_rebase` 里 `(None, NewThread::BaselineOnly)`
    /// 那一臂的 `0` 改成 `*ns`(两种取法就此同义,第一段断言红)。
    #[test]
    fn a_thread_found_late_only_sets_a_baseline_instead_of_reporting_a_minute_at_once() {
        let cur = || {
            let mut m = std::collections::HashMap::new();
            m.insert(77u32, ("迟到的线程".to_string(), 3_000_000_000u64));
            m
        };

        // 已有基线(不是首次采样),但 77 号是这一窗口才出现的 tid。
        let mut late = ThreadCpuProbe::new(1);
        late.prev = Some(std::collections::HashMap::new());
        assert_eq!(
            late.diff_and_rebase(cur(), NewThread::BaselineOnly),
            Some(vec![("迟到的线程".to_string(), 0)]),
            "晚发现的线程该只建基线,不该把攒了一分钟的 CPU 记成这一窗口的"
        );

        let mut fresh = ThreadCpuProbe::new(1);
        fresh.prev = Some(std::collections::HashMap::new());
        assert_eq!(
            fresh.diff_and_rebase(cur(), NewThread::ChargeFull),
            Some(vec![("迟到的线程".to_string(), 3_000_000_000)]),
            "Linux 每窗口都重列,新出现就是这 5 秒里刚创建的,该全额算"
        );

        // 建完基线之后,下一窗口的差分照常 —— 只吞第一窗口,不是永远吞。
        assert_eq!(
            late.diff_and_rebase(
                {
                    let mut m = std::collections::HashMap::new();
                    m.insert(77u32, ("迟到的线程".to_string(), 3_500_000_000u64));
                    m
                },
                NewThread::BaselineOnly
            ),
            Some(vec![("迟到的线程".to_string(), 500_000_000)]),
            "建过基线之后必须正常差分,否则这条线程永远报 0"
        );
    }
}
