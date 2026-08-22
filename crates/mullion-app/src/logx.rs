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
use std::sync::{Mutex, OnceLock};

use log::{LevelFilter, Log, Metadata, Record};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// 单个日志文件的大小上限;超过则在启动时轮转一代(`mullion.log.1`)。
/// debug/trace 级别下 wgpu + 自家心跳的量不小,不设上限会把盘写满。
const ROTATE_AT_BYTES: u64 = 8 * 1024 * 1024;

static SINK: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();

/// 日志文件路径:`<config_dir>/mullion.log`(Windows `%APPDATA%\mullion\config\mullion.log`)。
pub fn log_path() -> Option<PathBuf> {
    crate::shell::store::config_dir().map(|d| d.join("mullion.log"))
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

struct FileLogger {
    app: LevelFilter,
    deps: LevelFilter,
}

impl Log for FileLogger {
    fn enabled(&self, md: &Metadata) -> bool {
        let limit = if is_own_crate(md.target()) {
            self.app
        } else {
            self.deps
        };
        md.level() <= limit
    }

    fn log(&self, r: &Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        write_line(&format!("{:<5} {}: {}", r.level(), r.target(), r.args()));
    }

    fn flush(&self) {}
}

/// 打开日志文件(必要时先轮转)+ 接管 `log` facade + 安装 panic 钩子。`main` 最早调用一次。
///
/// `stored` 来自 `settings.toml`(`main` 在调用本函数**之前**读好)。
/// 读不到设置时传 `LogLevel::Info`。
pub fn init(version: &str, stored: mullion_store::LogLevel) {
    let path = log_path();
    let file = path.as_ref().and_then(|p| {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        rotate_if_large(p);
        OpenOptions::new().create(true).append(true).open(p).ok()
    });
    let _ = SINK.set(file.map(Mutex::new));

    let env_app = std::env::var("MULLION_LOG").ok();
    let env_deps = std::env::var("MULLION_LOG_DEPS").ok();
    let (app, deps) = resolve_levels(stored, env_app.as_deref(), env_deps.as_deref());
    // set_boxed_logger 只可能成功一次(集成测试里重复调会 Err)——失败静默,
    // 日志绝不能反过来拖垮程序。
    if log::set_boxed_logger(Box::new(FileLogger { app, deps })).is_ok() {
        log::set_max_level(app.max(deps));
    }

    match path {
        Some(p) => line(&format!(
            "==== mullion {version} 启动;日志: {} (app={app} deps={deps}) ====",
            p.display()
        )),
        None => line(&format!(
            "==== mullion {version} 启动(无法定位配置目录,仅 stderr;app={app} deps={deps})===="
        )),
    }

    // panic 钩子:把 panic 信息 + backtrace 落盘,避免 GUI 子系统下无声退出。
    std::panic::set_hook(Box::new(|info| {
        let bt = std::backtrace::Backtrace::force_capture();
        let loc = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "?".into());
        write_line(&format!(
            "PANIC mullion: @ {loc}: {info}\n--- backtrace ---\n{bt}\n--- end ---"
        ));
    }));
}

/// 超过上限就把旧日志挪到 `.1`(只留一代)。失败静默:轮转不成也要能继续写。
fn rotate_if_large(p: &Path) {
    if let Ok(md) = std::fs::metadata(p) {
        if md.len() > ROTATE_AT_BYTES {
            let _ = std::fs::rename(p, p.with_extension("log.1"));
        }
    }
}

/// 关键生命周期事件(info 级,target 固定为 `mullion`)。
/// 保留这个窄接口是为了让事件循环里的取证打点写起来短,行为等价于 `log::info!`。
pub fn line(msg: &str) {
    log::info!(target: "mullion", "{msg}");
}

/// 真正落盘:带 UTC 时间戳,写文件 + stderr,逐行 flush。
///
/// 逐行 flush 是刻意的:卡死/被强杀时进程没有机会 flush 缓冲区,而「日志最后一行
/// 停在哪」正是判断卡在哪一步的硬证据。失败静默(日志绝不能反过来拖垮程序)。
fn write_line(msg: &str) {
    let ts = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default();
    let full = format!("[{ts}] {msg}\n");
    let _ = write!(std::io::stderr(), "{full}");
    if let Some(Some(m)) = SINK.get() {
        if let Ok(mut f) = m.lock() {
            let _ = f.write_all(full.as_bytes());
            let _ = f.flush();
        }
    }
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
}
