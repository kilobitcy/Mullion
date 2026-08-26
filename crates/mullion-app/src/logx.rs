//! 文件日志(分级 + 轮转)+ 接管 `log` facade + panic 钩子。
//!
//! GUI 子系统下双击启动没有控制台,`eprintln!` 全丢;进程挂起时 Windows 事件日志
//! **根本不记录**(实测:一次真机卡死在 Application 通道里连 mullion 的名字都搜不到)。
//! 所以取证只能靠自己这个文件。
//!
//! 为什么接 `log` facade:wgpu / wgpu-core / wgpu-hal / naga / winit / glyphon / russh
//! 内部全用 `log` 打诊断(adapter 选择、surface 重建、设备丢失、SSH 协商失败…)。
//! 接上之后这些信息与我们自己的日志落进同一个文件、同一条时间线——这是「不依赖
//! Windows 端日志」的最大收益。见 `docs/adr-008-*`。
//!
//! 级别开关(环境变量,取值 `off|error|warn|info|debug|trace`):
//! - `MULLION_LOG` —— 自家 crate(`mullion*`),默认 `info`
//! - `MULLION_LOG_DEPS` —— 第三方 crate,默认 `warn`(分开是因为 wgpu 一开 debug 就刷屏)

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use log::{LevelFilter, Log, Metadata, Record};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// 单个日志文件的大小上限;超过则在启动时轮转一代(`mullion.log.1`)。
/// debug/trace 级别下 wgpu + 自家心跳的量不小,不设上限会把盘写满。
const ROTATE_AT_BYTES: u64 = 8 * 1024 * 1024;

/// debug/trace 档的单文件上限。8MB 在 debug 下几十秒就满 —— 出问题那一刻的
/// 记录会被自己后面的日志冲掉,而那正是唯一有价值的一段。
const ROTATE_AT_BYTES_DEBUG: u64 = 64 * 1024 * 1024;

/// 这个档位下,单个日志文件多大就轮转。
pub fn rotate_bytes_for(app: LevelFilter) -> u64 {
    if app >= LevelFilter::Debug {
        ROTATE_AT_BYTES_DEBUG
    } else {
        ROTATE_AT_BYTES
    }
}

/// 这一条要不要立刻落盘。
///
/// 「逐行落盘」是本文件原本的意图(卡死时「最后一行停在哪」是硬证据),
/// 但 debug 档下每帧几条日志、每条一次 write 系统调用,写盘就进了帧预算,
/// 测出来的不再是原来的程序(T3)。折中:错误/警告立刻 flush(稀少),
/// 其余走缓冲、由 `diag` 的周期线程每秒 flush 一次,最坏丢一秒。
pub fn flush_immediately(level: log::Level) -> bool {
    level <= log::Level::Warn
}

/// 写一行到 sink,按级别决定要不要立刻 flush。
///
/// 抽成泛型只为一件事:**测试能拿一个假 sink 数 flush 次数**。`SINK` 是
/// 进程级 `OnceLock`,测试碰不了它,而「判断做对了」与「判断真的落到 sink 上」
/// 是两回事 —— 中间断一根线,纯函数的测试照样全绿。
fn emit<W: Write>(w: &mut W, full: &str, level: log::Level) {
    let _ = w.write_all(full.as_bytes());
    if flush_immediately(level) {
        let _ = w.flush();
    }
}

/// **`Option` 在锁内**是为了运行期轮转:换文件要在持锁时把旧 writer
/// `take()` 出来 drop 掉再放新的,`Option` 在锁外就换不了。
static SINK: OnceLock<Mutex<Option<std::io::BufWriter<std::fs::File>>>> = OnceLock::new();

/// 本实例日志文件的路径,轮转时要用。`init` 之后才有。
static LOG_FILE: OnceLock<PathBuf> = OnceLock::new();

/// 运行期够到 logger 的句柄(设置弹窗点「确定」时换档要用)。`init` 之前是空的。
static LOGGER: OnceLock<&'static FileLogger> = OnceLock::new();

/// 本实例的身份,`{毫秒}-{pid}`(F148 的 `new_instance_id`)。
///
/// **在 `logx` 而不是 `App::new` 里生成**:日志文件名要用它,而 `logx::init`
/// 跑在 `App::new` 之前。共用同一个 id 之后,日志文件与 F148 的现场历史
/// 记录一一对应 —— 排障时「崩的是哪个实例、它当时恢复的是哪个现场」不用猜。
///
/// `get_or_init` 而非 `init` 里 `set`:集成测试不会走 `init`,懒生成让
/// 调用顺序无关紧要。
pub fn instance_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        mullion_store::new_instance_id(mullion_store::now_ms(), std::process::id())
    })
}

/// 日志文件所在目录。给清理逻辑用。
pub fn log_dir() -> Option<PathBuf> {
    crate::shell::store::config_dir()
}

/// 某个实例的日志文件名。
pub fn log_file_name(instance_id: &str) -> String {
    format!("mullion-{instance_id}.log")
}

/// 这个字符串是不是一个 F148 形状的 instance id(`{纯数字}-{纯数字}`)。
///
/// **严格校验不是洁癖**:配置目录里还躺着 F155 导出的
/// `mullion-redacted.log`。宽松匹配会把它认成 id 为 `redacted` 的实例日志,
/// 判死之后由清理逻辑删掉 —— 删的正是用户刚导出准备发过来的那份。
fn is_instance_id(s: &str) -> bool {
    let mut parts = s.split('-');
    let (Some(a), Some(b), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let numeric = |x: &str| !x.is_empty() && x.bytes().all(|c| c.is_ascii_digit());
    numeric(a) && numeric(b)
}

/// 文件名 → instance id。认 `mullion-<id>.log` 与轮转出来的
/// `mullion-<id>.log.1`;其余(含上一版的 `mullion.log`)一律 `None`。
pub fn parse_log_name(name: &str) -> Option<&str> {
    let rest = name.strip_prefix("mullion-")?;
    let id = rest
        .strip_suffix(".log.1")
        .or_else(|| rest.strip_suffix(".log"))?;
    is_instance_id(id).then_some(id)
}

/// 本实例的日志文件路径:`<config_dir>/mullion-<instance_id>.log`
/// (Windows `%APPDATA%\mullion\config\mullion-<id>.log`)。
///
/// **一实例一文件**:多开时所有实例 append 进同一个文件的话,profile 行里
/// 的 CPU%/GPU%/显存全是 per-process 数字,混流之后会读成「一个进程在
/// 6% 和 94% 之间抽风」—— 比没有日志更糟。
pub fn log_path() -> Option<PathBuf> {
    log_dir().map(|d| d.join(log_file_name(instance_id())))
}

/// 解析级别字符串。无法识别或缺省时回落到 `default`。纯函数,可单测。
pub fn parse_level(raw: Option<&str>, default: LevelFilter) -> LevelFilter {
    match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("off") => LevelFilter::Off,
        Some("error") => LevelFilter::Error,
        Some("warn") => LevelFilter::Warn,
        Some("info") => LevelFilter::Info,
        Some("debug") => LevelFilter::Debug,
        Some("trace") => LevelFilter::Trace,
        _ => default,
    }
}

/// 设置里的三档 → (自家 crate 档位, 第三方 crate 档位)。
///
/// **第三方永远比自家低一档**:wgpu/naga/winit 一开 debug 就刷屏(见模块
/// 顶部说明),跟着自家一起提上去的话,每 5 秒一行的剖面会被淹没在几万行
/// adapter 日志里 —— 那等于这个功能没做。
pub fn levels_for(level: mullion_store::LogLevel) -> (LevelFilter, LevelFilter) {
    match level {
        mullion_store::LogLevel::Error => (LevelFilter::Error, LevelFilter::Error),
        mullion_store::LogLevel::Info => (LevelFilter::Info, LevelFilter::Warn),
        mullion_store::LogLevel::Debug => (LevelFilter::Debug, LevelFilter::Info),
    }
}

/// 设置 + 两个环境变量 → 最终档位。**环境变量覆盖设置**。
///
/// 纯函数(环境变量由调用方读好传进来),这样「谁覆盖谁」这条规则测得动 ——
/// 直接在测试里 `set_var` 的话,并行跑的测试会互相偷环境。
pub fn resolve_levels(
    stored: mullion_store::LogLevel,
    env_app: Option<&str>,
    env_deps: Option<&str>,
) -> (LevelFilter, LevelFilter) {
    let (app, deps) = levels_for(stored);
    (parse_level(env_app, app), parse_level(env_deps, deps))
}

/// 目标是否属于本项目自己的 crate(`mullion-app` 的 target 形如 `mullion_app::app`)。
fn is_own_crate(target: &str) -> bool {
    target.starts_with("mullion")
}

/// `usize` → `LevelFilter`。`log` 自己就是拿判别值当序号用的(`Off` 最小、
/// `Trace` 最大),这里手写一张表而不是 `transmute` —— 越界时回落到 `Trace`
/// (放行多于该放行的)比 UB 安全。
fn filter_from_usize(v: usize) -> LevelFilter {
    match v {
        0 => LevelFilter::Off,
        1 => LevelFilter::Error,
        2 => LevelFilter::Warn,
        3 => LevelFilter::Info,
        4 => LevelFilter::Debug,
        _ => LevelFilter::Trace,
    }
}

struct FileLogger {
    app: AtomicUsize,
    deps: AtomicUsize,
}

impl FileLogger {
    fn new(app: LevelFilter, deps: LevelFilter) -> Self {
        Self {
            app: AtomicUsize::new(app as usize),
            deps: AtomicUsize::new(deps as usize),
        }
    }

    /// 运行期换档。设置弹窗点「确定」时走这里。
    fn set(&self, app: LevelFilter, deps: LevelFilter) {
        self.app.store(app as usize, Ordering::Relaxed);
        self.deps.store(deps as usize, Ordering::Relaxed);
    }
}

impl Log for FileLogger {
    fn enabled(&self, md: &Metadata) -> bool {
        let limit = if is_own_crate(md.target()) {
            filter_from_usize(self.app.load(Ordering::Relaxed))
        } else {
            filter_from_usize(self.deps.load(Ordering::Relaxed))
        };
        md.level() <= limit
    }

    fn log(&self, r: &Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        write_line_at(
            &format!("{:<5} {}: {}", r.level(), r.target(), r.args()),
            r.level(),
        );
    }

    fn flush(&self) {
        flush_now();
    }
}

/// 打开日志文件(必要时先轮转)+ 接管 `log` facade + 安装 panic 钩子。`main` 最早调用一次。
///
/// `stored` 来自 `settings.toml`(`main` 在调用本函数**之前**读好)。
/// 读不到设置时传 `LogLevel::Info`。
pub fn init(version: &str, stored: mullion_store::LogLevel) {
    let env_app = std::env::var("MULLION_LOG").ok();
    let env_deps = std::env::var("MULLION_LOG_DEPS").ok();
    let (app, deps) = resolve_levels(stored, env_app.as_deref(), env_deps.as_deref());

    let path = log_path();
    let file = path.as_ref().and_then(|p| {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .ok()
            .map(std::io::BufWriter::new)
    });
    let _ = SINK.set(Mutex::new(file));
    if let Some(p) = path.as_ref() {
        let _ = LOG_FILE.set(p.clone());
    }

    // 泄漏成 `&'static`:`set_logger` 要求 `&'static dyn Log`,而我们还要
    // 留一个句柄给运行期换档。进程一生只泄漏这一个,不是问题。
    let logger: &'static FileLogger = Box::leak(Box::new(FileLogger::new(app, deps)));
    let _ = LOGGER.set(logger);
    // set_logger 只可能成功一次(集成测试里重复调会 Err)——失败静默,
    // 日志绝不能反过来拖垮程序。
    if log::set_logger(logger).is_ok() {
        log::set_max_level(app.max(deps));
    }

    match path {
        Some(p) => line(&format!(
            "==== mullion {version} 启动;日志: {} (app={app} deps={deps}) ====\n\
             (一实例一文件;上一版的 mullion.log 若还在,已不再写入)",
            p.display()
        )),
        None => line(&format!(
            "==== mullion {version} 启动(无法定位配置目录,仅 stderr;app={app} deps={deps})===="
        )),
    }
    // 启动横幅走的是 info 级(缓冲写)。刚起来就崩的话缓冲还没刷过 ——
    // 而「有没有走到 init」恰恰是这种崩溃唯一的线索。
    flush_now();

    // panic 钩子:把 panic 信息 + backtrace 落盘,避免 GUI 子系统下无声退出。
    std::panic::set_hook(Box::new(|info| {
        let bt = std::backtrace::Backtrace::force_capture();
        let loc = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "?".into());
        write_line_at(
            &format!("PANIC mullion: @ {loc}: {info}\n--- backtrace ---\n{bt}\n--- end ---"),
            log::Level::Error,
        );
    }));
}

/// 关键生命周期事件(info 级,target 固定为 `mullion`)。
/// 保留这个窄接口是为了让事件循环里的取证打点写起来短,行为等价于 `log::info!`。
pub fn line(msg: &str) {
    log::info!(target: "mullion", "{msg}");
}

/// 一行日志的最终形状。**抽成纯函数只为可测**:行格式是多实例排障时
/// 唯一的归属线索,内联在 `format!` 里就只能靠人眼看。
pub fn format_line(ts: &str, pid: u32, msg: &str) -> String {
    format!("[{ts}] [{pid}] {msg}\n")
}

/// 真正落盘:带 UTC 时间戳 + pid,写文件 + stderr。`level` 决定要不要立刻
/// flush(见 [`flush_immediately`])。失败静默(日志绝不能反过来拖垮程序)。
fn write_line_at(msg: &str, level: log::Level) {
    let ts = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default();
    let full = format_line(&ts, std::process::id(), msg);
    let _ = write!(std::io::stderr(), "{full}");
    if let Some(m) = SINK.get() {
        if let Ok(mut g) = m.lock() {
            if let Some(w) = g.as_mut() {
                emit(w, &full, level);
            }
        }
    }
}

/// 运行期改档位(设置弹窗点了确定)。`init` 之前调用无效果。
///
/// 同时要更新 `log::set_max_level`:facade 那一层的粗过滤在
/// `FileLogger::enabled` **之前**,不抬上去的话,自家档位提到 debug
/// 也一条都到不了我们手里。
pub fn set_levels(app: LevelFilter, deps: LevelFilter) {
    if let Some(l) = LOGGER.get() {
        l.set(app, deps);
        log::set_max_level(app.max(deps));
    }
}

/// 把缓冲里的日志刷到盘上。`diag` 的周期线程每秒调一次 —— 没有它,
/// info/debug 档下卡死时最后几秒的记录会随进程一起消失。
pub fn flush_now() {
    if let Some(m) = SINK.get() {
        if let Ok(mut g) = m.lock() {
            if let Some(w) = g.as_mut() {
                let _ = w.flush();
            }
        }
    }
}

/// 这个大小该不该轮转。纯函数,可单测。
pub fn should_rotate(len: u64, limit: u64) -> bool {
    len > limit
}

/// 当前档位对应的轮转上限。`init` 之前按最保守的档算。
fn current_rotate_bytes() -> u64 {
    let app = LOGGER
        .get()
        .map_or(LevelFilter::Info, |l| filter_from_usize(l.app.load(Ordering::Relaxed)));
    rotate_bytes_for(app)
}

/// 日志超限就转一代并重开。**由 `diag` 的看门狗线程每秒调一次**。
///
/// 为什么不在 `write_line_at` 里判:那是帧路径(每帧几条 debug 日志),
/// 一次 `metadata` 系统调用就进了帧预算 —— T3 红线。看门狗线程本来就
/// 每秒醒一次做 flush,顺带查一次大小是免费的。
///
/// 为什么不在启动时判:一实例一文件之后文件名唯一,启动时那个文件永远
/// 是空的,判据永远不成立,64MB 上限形同虚设。
pub fn rotate_if_needed() {
    let Some(path) = LOG_FILE.get() else { return };
    let Some(m) = SINK.get() else { return };
    let Ok(mut guard) = m.lock() else { return };
    if guard.is_none() {
        return;
    }
    let len = std::fs::metadata(path).map_or(0, |md| md.len());
    if !should_rotate(len, current_rotate_bytes()) {
        return;
    }
    rotate_now(&mut guard, path);
}

/// 轮转本体:**先关后挪**。
///
/// 顺序是全部要点。对一个正开着的文件 rename,句柄会跟着 inode 走 ——
/// 本进程继续往改名后的 `.log.1` 里写,新建的主文件永远是空的,症状是
/// 「日志某一刻起停住不动」且完全静默。`take()` 让 `BufWriter` 走 Drop
/// (flush + close),之后 rename 的是一个没人开着的文件。
fn rotate_now(guard: &mut Option<std::io::BufWriter<std::fs::File>>, path: &Path) {
    drop(guard.take());
    let _ = std::fs::rename(path, path.with_extension("log.1"));
    *guard = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
        .map(std::io::BufWriter::new);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_levels_case_insensitively() {
        assert_eq!(
            parse_level(Some("DEBUG"), LevelFilter::Info),
            LevelFilter::Debug
        );
        assert_eq!(
            parse_level(Some(" trace "), LevelFilter::Info),
            LevelFilter::Trace
        );
        assert_eq!(
            parse_level(Some("off"), LevelFilter::Info),
            LevelFilter::Off
        );
    }

    #[test]
    fn unknown_or_missing_falls_back_to_default() {
        // 环境变量写错不该让日志静音,回落默认档。
        assert_eq!(
            parse_level(Some("verbose"), LevelFilter::Info),
            LevelFilter::Info
        );
        assert_eq!(parse_level(None, LevelFilter::Warn), LevelFilter::Warn);
    }

    #[test]
    fn own_crate_targets_are_separated_from_deps() {
        // 自家与第三方分档的前提:target 前缀能区分开。
        assert!(is_own_crate("mullion_app::app"));
        assert!(is_own_crate("mullion"));
        assert!(!is_own_crate("wgpu_core::device"));
        assert!(!is_own_crate("winit::platform_impl"));
    }

    use mullion_store::LogLevel;

    /// 设置里的档位映射成两个 `LevelFilter`:自家 crate 一个、第三方一个。
    ///
    /// **第三方档位不等于自家档位**:wgpu/naga 一开 debug 就刷屏(本文件
    /// 顶部注释里记着),把它们跟自家一起提上去,每 5 秒一行的剖面会被淹没
    /// 在几万行 adapter 日志里 —— 那等于这个功能没做。
    ///
    /// 自证会变红:把 `levels_for` 里 Debug 档的 deps 从 `Info` 改成 `Debug`。
    #[test]
    fn the_dependency_level_never_follows_our_own_all_the_way_up() {
        assert_eq!(
            levels_for(LogLevel::Debug),
            (LevelFilter::Debug, LevelFilter::Info)
        );
        assert_eq!(
            levels_for(LogLevel::Info),
            (LevelFilter::Info, LevelFilter::Warn)
        );
        assert_eq!(
            levels_for(LogLevel::Error),
            (LevelFilter::Error, LevelFilter::Error)
        );
    }

    /// 环境变量**覆盖**设置:排障时不必先进 GUI 改设置再重启。
    ///
    /// 反过来(设置覆盖环境变量)的话,「我明明设了 MULLION_LOG=debug
    /// 怎么还是没有」会变成一个查无可查的问题。
    ///
    /// 自证会变红:把 `resolve_levels` 里两个 `parse_level` 的 default
    /// 参数换成写死的 `LevelFilter::Info`/`Warn`。
    #[test]
    fn an_environment_variable_wins_over_the_stored_setting() {
        // 设置说 error,环境变量说 debug → 用 debug。
        assert_eq!(
            resolve_levels(LogLevel::Error, Some("debug"), None),
            (LevelFilter::Debug, LevelFilter::Error),
            "MULLION_LOG 没能覆盖设置里的档位"
        );
        // 环境变量缺席 → 完全按设置。
        assert_eq!(
            resolve_levels(LogLevel::Debug, None, None),
            (LevelFilter::Debug, LevelFilter::Info)
        );
        // 依赖档有自己的环境变量,同样覆盖。
        assert_eq!(
            resolve_levels(LogLevel::Info, None, Some("off")),
            (LevelFilter::Info, LevelFilter::Off)
        );
    }

    /// 环境变量写错(`verbose`)时回落到**设置里的档位**,而不是回落到
    /// 硬编码的默认。用户在设置里选了 debug、又在环境变量里打错一个词,
    /// 结果日志静默降回 info —— 那比直接忽略更难查。
    ///
    /// 自证会变红:把 `resolve_levels` 里 `parse_level(env_app, app)` 的
    /// 第二个参数改成 `LevelFilter::Info`。
    #[test]
    fn a_typo_in_the_environment_falls_back_to_the_stored_level_not_to_a_hardcoded_one() {
        assert_eq!(
            resolve_levels(LogLevel::Debug, Some("verbose"), None),
            (LevelFilter::Debug, LevelFilter::Info)
        );
    }

    /// debug 档写得多,8MB 一代几十秒就冲掉了 —— 真正出问题那一刻的记录
    /// 已经被自己后面的日志刷没了。档位高时上限跟着抬。
    ///
    /// 自证会变红:把 `rotate_bytes_for` 改成恒返回 `ROTATE_AT_BYTES`。
    #[test]
    fn a_chattier_level_gets_a_bigger_file_before_rotating() {
        let info = rotate_bytes_for(LevelFilter::Info);
        let debug = rotate_bytes_for(LevelFilter::Debug);
        assert_eq!(info, ROTATE_AT_BYTES);
        assert!(
            debug >= info * 4,
            "debug 档的上限只有 {debug},几十秒就把出问题那一刻冲掉了"
        );
        assert_eq!(
            rotate_bytes_for(LevelFilter::Trace),
            debug,
            "trace 比 debug 还吵,不该反而回到小上限"
        );
    }

    /// 哪些级别必须**立刻**落盘。
    ///
    /// 本文件存在的全部理由是「卡死/被强杀时最后一行停在哪」—— 缓冲写会把
    /// 这个能力削掉。折中:错误与警告立刻 flush(它们稀少,代价可忽略),
    /// info/debug 走缓冲、由周期线程每秒 flush 一次,最坏丢一秒。
    ///
    /// 自证会变红:把 `flush_immediately` 改成恒 `false`。
    #[test]
    fn errors_are_flushed_at_once_while_chatter_may_wait_a_second() {
        assert!(flush_immediately(log::Level::Error));
        assert!(flush_immediately(log::Level::Warn));
        assert!(!flush_immediately(log::Level::Info));
        assert!(!flush_immediately(log::Level::Debug));
    }

    /// 记录 flush 次数的假 sink。
    struct Spy {
        bytes: Vec<u8>,
        flushes: usize,
    }

    impl std::io::Write for Spy {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    /// `flush_immediately` 这个判断必须真的落到 sink 上。
    ///
    /// 光测那个纯函数是不够的:它返回什么与「写的时候有没有照做」是两回事,
    /// 中间断一根线,测试照绿而日志在卡死时全丢 —— 那是这个文件唯一的用途。
    ///
    /// 自证会变红:把 `emit` 里的 `if flush_immediately(level)` 改成 `if true`
    /// 或 `if false`,两种改法各扎一条断言。
    #[test]
    fn the_flush_decision_actually_reaches_the_sink() {
        let mut spy = Spy {
            bytes: Vec::new(),
            flushes: 0,
        };
        emit(&mut spy, "chatter\n", log::Level::Info);
        assert_eq!(spy.flushes, 0, "info 级也立刻 flush,缓冲等于没做");
        assert!(
            !spy.bytes.is_empty(),
            "不 flush 不等于不写 —— 字节必须进缓冲"
        );

        emit(&mut spy, "boom\n", log::Level::Error);
        assert_eq!(spy.flushes, 1, "错误没有立刻落盘,卡死时最后一行会丢");
    }

    /// 运行期换档必须真的改变「这条日志放不放行」。
    ///
    /// 只测 `set_levels` 存进去了是不够的:存了但 `enabled` 没读它,
    /// 症状是「设置里选了详细档,日志一行没多」,而设置文件里存的确实对 ——
    /// 重启又是好的,这种最难自查。
    ///
    /// 自证会变红:把 `enabled` 里读原子那两行换回读固定字段。
    #[test]
    fn changing_the_level_at_runtime_changes_what_gets_through() {
        let lg = FileLogger::new(LevelFilter::Warn, LevelFilter::Error);
        let own = log::Metadata::builder()
            .target("mullion_app::app")
            .level(log::Level::Info)
            .build();
        assert!(!lg.enabled(&own), "warn 档不该放行 info");

        lg.set(LevelFilter::Debug, LevelFilter::Error);
        assert!(
            lg.enabled(&own),
            "换到 debug 档后 info 仍被挡 —— 换档没生效"
        );
    }

    /// 自家 crate 与第三方各走各的档。混成一个的话,把自家提到 debug
    /// 会连 wgpu 一起提上去,每 5 秒一行的剖面被淹没在几万行 adapter 日志里。
    ///
    /// 自证会变红:把 `enabled` 里的 `is_own_crate` 分支去掉,两边都用 `app`。
    #[test]
    fn the_two_level_dials_stay_independent_at_runtime() {
        let lg = FileLogger::new(LevelFilter::Debug, LevelFilter::Error);
        let theirs = log::Metadata::builder()
            .target("wgpu_core::device")
            .level(log::Level::Info)
            .build();
        let ours = log::Metadata::builder()
            .target("mullion_app::app")
            .level(log::Level::Info)
            .build();
        assert!(lg.enabled(&ours), "自家 info 被挡了");
        assert!(!lg.enabled(&theirs), "第三方跟着自家一起被提上去了");
    }

    /// `LevelFilter` ↔ `usize` 的往返必须无损。错一档的症状是静默的:
    /// 日志少了或多了,没有任何报错。
    ///
    /// 自证会变红:把 `filter_from_usize` 里 `3 => Info` 改成 `3 => Warn`。
    #[test]
    fn the_level_survives_the_round_trip_through_an_atomic() {
        for f in [
            LevelFilter::Off,
            LevelFilter::Error,
            LevelFilter::Warn,
            LevelFilter::Info,
            LevelFilter::Debug,
            LevelFilter::Trace,
        ] {
            assert_eq!(filter_from_usize(f as usize), f, "{f} 走一圈变了");
        }
    }

    /// 运行期换档必须**同时**抬 `log::set_max_level`。
    ///
    /// facade 那一层的粗过滤在 `FileLogger::enabled` **之前**:不抬上去的话,
    /// 自家档位提到 debug 也一条都到不了我们手里 —— 用户在设置里选了详细档、
    /// 日志一行没多,而设置文件里存的确实是他选的那个值。
    ///
    /// 这里扎的是**源码结构**:`set_levels` 只在 `init` 之后才有效果,而
    /// `init` 会接管进程唯一的 `log` facade,单测里跑不了真流程。
    ///
    /// 自证会变红:删掉 `set_levels` 里那句 `log::set_max_level(..)`。
    #[test]
    fn changing_the_level_also_raises_the_facade_filter() {
        let src = include_str!("logx.rs");
        let body = src
            .split("pub fn set_levels(")
            .nth(1)
            .expect("set_levels 没了？这条测试的锚点失效了")
            .split("\n}\n")
            .next()
            .expect("set_levels 的函数体没有闭合？");
        assert!(
            body.contains("set_max_level"),
            "换档没抬 facade 的粗过滤 —— 提到 debug 档也一条都到不了 enabled"
        );
    }

    /// 同一进程里 `instance_id()` 必须每次返回同一个值。
    ///
    /// 它同时决定日志文件名和 F148 现场历史的记录名 —— 两次调用拿到不同的
    /// id,症状是「日志文件里写着 A,历史记录叫 B」,排障时根本对不上号,
    /// 而且没有任何报错。
    ///
    /// 自证会变红:把 `instance_id` 里的 `get_or_init` 换成每次现算
    /// `new_instance_id(now_ms(), process::id())`。
    #[test]
    fn the_instance_id_is_stable_within_one_process() {
        let a = instance_id();
        let b = instance_id();
        assert_eq!(a, b, "同一进程两次拿到不同的 instance id");
        assert!(!a.is_empty(), "instance id 是空的");
    }

    /// id 的形状必须是 F148 的 `{毫秒}-{pid}`。
    ///
    /// 形状是硬约定:Task 3 的文件名解析器按「两段纯数字」严格校验,
    /// 形状一变,自己的日志会被自己的清理逻辑判成不认识的文件。
    ///
    /// 自证会变红:把 `instance_id` 改成 `format!("mullion-{}", process::id())`。
    #[test]
    fn the_instance_id_is_two_numeric_parts() {
        let id = instance_id();
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(parts.len(), 2, "id 不是两段:{id}");
        for p in parts {
            assert!(
                !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()),
                "id 里有非数字段:{id}"
            );
        }
    }

    /// 每一行都必须带 pid。
    ///
    /// 一实例一文件之后 pid 看似冗余,但它是**双保险**:日志被改名、被
    /// 拼接、被贴进 issue 之后,文件名那层归属就没了,而排障时最常见的
    /// 动作恰恰是把几个文件拼起来按时间排。
    ///
    /// 自证会变红:把 `format_line` 里的 `[{pid}] ` 去掉。
    #[test]
    fn every_line_carries_the_pid() {
        let line = format_line("2026-08-26T00:00:00Z", 4242, "INFO  mullion: 你好");
        assert!(line.contains("[4242]"), "行里没有 pid:{line}");
        assert!(line.starts_with("[2026-08-26T00:00:00Z]"), "时间戳不在最前:{line}");
        assert!(line.ends_with('\n'), "行尾没有换行:{line:?}");
    }

    /// pid 必须排在时间戳**之后**、正文之前。
    ///
    /// 位置不是审美问题:现有的排障习惯是 `findstr profile` 之后按列读,
    /// pid 插进正文中间会把 profile 行的字段位置整体推移。
    ///
    /// 自证会变红:把 `format_line` 改成 `format!("[{pid}] [{ts}] {msg}\n")`。
    #[test]
    fn the_pid_sits_between_the_timestamp_and_the_message() {
        let line = format_line("TS", 7, "MSG");
        assert_eq!(line, "[TS] [7] MSG\n");
    }

    /// 文件名 ⇄ instance id 的往返。
    ///
    /// 自证会变红:把 `log_file_name` 里的 `mullion-` 前缀去掉。
    #[test]
    fn a_log_file_name_round_trips_to_its_instance_id() {
        let name = log_file_name("1755000000123-4242");
        assert_eq!(name, "mullion-1755000000123-4242.log");
        assert_eq!(parse_log_name(&name), Some("1755000000123-4242"));
        assert_eq!(
            parse_log_name("mullion-1755000000123-4242.log.1"),
            Some("1755000000123-4242"),
            "轮转出来的 .log.1 必须认得出属于哪个实例,否则它成孤儿"
        );
    }

    /// 轮转判据。
    ///
    /// 自证会变红:把 `should_rotate` 改成恒 `false`。
    #[test]
    fn a_file_past_the_limit_wants_to_rotate() {
        assert!(!should_rotate(0, 100));
        assert!(!should_rotate(100, 100), "刚好等于上限不该转");
        assert!(should_rotate(101, 100));
    }

    /// 轮转必须**先关后挪**,而不是对开着的文件 rename。
    ///
    /// 对一个正在写的文件 rename:句柄跟着 inode 走,本进程会继续往
    /// 改名后的 `.log.1` 里写,而新建的主文件永远是空的 —— 症状是
    /// 「日志停在某个时刻不动了」,且完全静默。这正是本切片要修的那个
    /// 多实例老 bug,不能在轮转里以另一种形式重现。
    ///
    /// 这里扎的是**源码结构**:真流程要碰进程唯一的 `SINK` 和真实文件系统,
    /// 单测里跑不动。
    ///
    /// 自证会变红:把 `rotate_now` 里的 `guard.take()` 那行删掉。
    #[test]
    fn rotation_closes_the_file_before_renaming_it() {
        let src = include_str!("logx.rs");
        let body = src
            .split("fn rotate_now(")
            .nth(1)
            .expect("rotate_now 没了?这条测试的锚点失效了")
            .split("\n}\n")
            .next()
            .expect("rotate_now 的函数体没有闭合?");
        let close_at = body.find("guard.take()").expect("轮转没有先关文件");
        let rename_at = body.find("rename").expect("轮转没有 rename");
        assert!(
            close_at < rename_at,
            "先 rename 后关文件 —— 句柄会跟着 inode 走,之后所有日志都写进 .log.1"
        );
    }

    /// **解析器必须严格**:只认 F148 的 `{纯数字}-{纯数字}`。
    ///
    /// 宽松匹配会把 F155 导出的 `mullion-redacted.log` 认成 instance id
    /// 为 `redacted` 的日志 —— 它没有心跳,会被判死,然后被清理逻辑
    /// **删掉用户刚导出准备发给我们的那个文件**。
    ///
    /// 老的 `mullion.log`(无 id)也必须返回 None:那是上一版留下的,
    /// 用户可能正开着看,不归我们管。
    ///
    /// 自证会变红:把 `parse_log_name` 里的 `is_instance_id(id)` 判断删掉。
    #[test]
    fn only_a_real_instance_id_is_recognised_so_other_files_are_never_touched() {
        for bad in [
            "mullion-redacted.log",          // F155 导出的脱敏副本
            "mullion-redacted-1-2.log",      // 带 id 的脱敏副本
            "mullion.log",                   // 上一版的遗留日志
            "mullion.log.1",                 // 上一版的遗留轮转
            "mullion-.log",                  // 空 id
            "mullion-abc-def.log",           // 非数字
            "mullion-1-2-3.log",             // 三段
            "notes.txt",                     // 完全无关
        ] {
            assert_eq!(parse_log_name(bad), None, "{bad} 不该被当成实例日志");
        }
    }
}
