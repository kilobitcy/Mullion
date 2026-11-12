//! 宿主机上的 echo sshd,给模拟器插桩测试连(模拟器里 `10.0.2.2` = 宿主机回环)。
//! 用户 `testuser` / 密码 `test-password`,只绑回环。
//!
//!     cargo run -p mullion-ffi --example echo_sshd -- 2222
#[path = "../tests/common/mod.rs"]
mod common;

#[tokio::main]
async fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|p| p.parse().ok())
        .unwrap_or(2222);
    let (addr, _rec) = common::spawn_recording_echo(&format!("127.0.0.1:{port}")).await;
    println!("echo sshd listening on {addr}");
    std::future::pending::<()>().await;
}
