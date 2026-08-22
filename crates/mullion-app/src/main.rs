//! mullion 入口:解析 CLI → 建 EventLoop 拿 proxy → 交给 App 统一异步 connect → run_app。
//! CLI 直连(路径①,`mullion user@host`)与无参启动 launcher(路径②)共用同一条
//! `session::connect` 通道,结果经 `UserEvent` 回送(App::spawn_connect,§5)。
//!
//! Windows 用 GUI 子系统,双击/启动时不附带黑控制台窗口;从终端启动时再附着父
//! 控制台,让 CLI 直连的诊断(eprintln)与退出码仍可见。
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::sync::{Arc, Mutex};

use mullion_app::app::{App, UserEvent};
use mullion_app::cli;
use mullion_store::KnownHostsFile;
use winit::event_loop::EventLoop;

fn main() {
    // GUI 子系统下从终端启动时,附着父进程控制台;双击(无父控制台)则静默失败,
    // 不弹黑框。须在任何 stdout/stderr 写入前调用。
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
        AttachConsole(ATTACH_PARENT_PROCESS);
    }

    // F155:日志档位来自 settings.toml。**必须在 logx::init 之前读** ——
    // 档位决定了 facade 的 max_level,init 之后再改就晚了。
    //
    // settings.toml 是明文 TOML、不经 keyring/主密码(见 store 的 settings.rs),
    // 所以这一步不需要会话库打开,也不会弹解锁框。读不出来就按默认档位跑,
    // 那句降级说明等 init 之后再补记 —— 此刻还没有日志可写。
    let (log_level, settings_note) = match mullion_app::shell::store::config_dir() {
        Some(dir) => {
            let loaded = mullion_store::settings::load(&dir);
            (loaded.settings.log_level, loaded.note)
        }
        None => (mullion_store::LogLevel::Info, None),
    };

    // 文件日志 + panic 钩子:GUI 子系统下崩溃/卡死无声消失,靠这个取证。
    // init 同时接管 `log` facade,wgpu/winit/russh 的内部诊断一并落进同一个文件。
    mullion_app::logx::init(env!("CARGO_PKG_VERSION"), log_level);
    if let Some(note) = settings_note {
        mullion_app::logx::line(&format!("settings.toml:{note}"));
    }
    // 环境快照 + 看门狗:卡死时靠它说出「卡在哪个阶段、卡了多久、内存多少」,
    // 不必去翻 Windows 事件日志(它根本不记录 GUI 进程挂起)。
    mullion_app::diag::log_startup_env(env!("CARGO_PKG_VERSION"));
    mullion_app::diag::start_watchdog(mullion_app::diag::DEFAULT_STALL_MS);

    let args: Vec<String> = std::env::args().skip(1).collect();
    mullion_app::logx::line(&format!(
        "启动模式: {}",
        if args.is_empty() {
            "无参 launcher"
        } else {
            "CLI 直连"
        }
    ));

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("建 tokio 运行时");

    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .expect("建事件循环");
    let proxy = event_loop.create_proxy();
    // F3:主机密钥指纹表,跨进程持久化。拿不到配置目录(极罕见,如无 HOME)时
    // 退化成纯内存表——每次启动都会重新问一遍,但绝不静默放行。
    let known_hosts = Arc::new(Mutex::new(match mullion_app::shell::store::config_dir() {
        Some(dir) => KnownHostsFile::load(&dir),
        None => KnownHostsFile::default(),
    }));
    if known_hosts
        .lock()
        .expect("known-hosts poisoned")
        .is_corrupt()
    {
        // 不是致命错误(当空表继续跑),但必须留痕:用户会奇怪「为什么又让我确认指纹」。
        mullion_app::logx::line("known_hosts.toml 解析失败,当空表处理;首次保存时会备份为 .bak");
    }

    // 待定 F:CLI 直连(路径①)解析成功即视为「直连」,连接失败时 App 保留
    // stderr + exit(1)(可脚本化);无参(路径②)进 launcher,失败走窗口内提示。
    let (initial, cli_direct) = if args.is_empty() {
        (None, false)
    } else {
        match cli::parse_args(&args) {
            Ok(cfg) => (Some(cfg), true),
            Err(e) => {
                eprintln!("参数错误: {e}\n用法: mullion user@host [-p PORT] [-i KEYPATH]");
                std::process::exit(2);
            }
        }
    };

    let mut app = App::new(runtime, proxy, known_hosts, initial, cli_direct);
    event_loop.run_app(&mut app).expect("run_app");
}
