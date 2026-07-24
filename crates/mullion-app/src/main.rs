//! mullion 入口:解析 CLI → 先建 EventLoop 拿 proxy → block_on 连接 → run_app。
//! 顺序关键:connect 需要 wake=proxy.send_event,proxy 来自 EventLoop,故必须先建循环。

use std::sync::{Arc, Mutex};

use mullion_app::app::{App, UserEvent};
use mullion_app::cli;
use mullion_ssh::known_hosts::{KnownHosts, TofuAccept};
use mullion_ssh::session::connect;
use winit::event_loop::EventLoop;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg = match cli::parse_args(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("参数错误: {e}\n用法: mullion user@host [-p PORT] [-i KEYPATH]");
            std::process::exit(2);
        }
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("建 tokio 运行时");

    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .expect("建事件循环");
    let proxy = event_loop.create_proxy();
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let _ = proxy.send_event(UserEvent::Wake);
    });

    let policy = Arc::new(TofuAccept::new(Arc::new(Mutex::new(KnownHosts::new()))));
    let (ssh, rx) = match runtime.block_on(connect(&cfg, policy, wake)) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("连接失败: {e}");
            std::process::exit(1);
        }
    };

    let mut app = App::new(runtime, ssh, rx);
    event_loop.run_app(&mut app).expect("run_app");
}
