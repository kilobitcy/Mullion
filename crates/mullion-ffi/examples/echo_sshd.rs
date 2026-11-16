//! 宿主机上的 echo sshd,给模拟器插桩测试连(模拟器里 `10.0.2.2` = 宿主机回环)。
//! 用户 `testuser` / 密码 `test-password`,只绑回环。
//! `端口` 上是纯 echo;`端口+1` 上开 shell 先吐提示符 `$ `(F349 attach 计划要等首字节)。
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
    let (addr, _rec) =
        common::spawn_echo_with_banner(&format!("127.0.0.1:{}", port + 1), b"$ ").await;
    println!("echo sshd (banner) listening on {addr}");
    std::future::pending::<()>().await;
}
