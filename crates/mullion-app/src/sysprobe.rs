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

    /// 采一次。首次调用没有基线,返回 `None`。
    pub fn sample(&mut self, window_ns: u64) -> Option<CpuSample> {
        let (proc_ns, main_ns) = read_cpu_ns(self)?;
        let d_proc = self.prev_process_ns.map(|p| proc_ns.saturating_sub(p));
        let d_main = self.prev_main_ns.map(|p| main_ns.saturating_sub(p));
        self.prev_process_ns = Some(proc_ns);
        self.prev_main_ns = Some(main_ns);
        Some(CpuSample {
            process_pct: cpu_pct(d_proc?, window_ns, self.cores)?,
            main_thread_pct: cpu_pct(d_main?, window_ns, 1)?,
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

    // FILETIME 的单位是 100 纳秒。
    fn ns(ft: FILETIME) -> u64 {
        (((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64) * 100
    }

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
            s.main_thread_pct > 50,
            "刚把主线程跑满 150ms,主线程口径只报了 {}%",
            s.main_thread_pct
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
}
