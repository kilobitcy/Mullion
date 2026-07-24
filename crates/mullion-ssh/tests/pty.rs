mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use mullion_ssh::config::{AuthMethod, SshConfig};
use mullion_ssh::known_hosts::{KnownHosts, TofuAccept};
use mullion_ssh::session::connect;

fn cfg(addr: std::net::SocketAddr) -> SshConfig {
    SshConfig {
        host: addr.ip().to_string(),
        port: addr.port(),
        user: common::TEST_USER.to_string(),
        auth: AuthMethod::Password(common::TEST_PASSWORD.into()),
        cols: 80,
        rows: 24,
        term: "xterm-256color".to_string(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn pty_echo_roundtrip() {
    let addr = common::spawn_echo_server().await;
    let policy = Arc::new(TofuAccept::new(Arc::new(Mutex::new(KnownHosts::new()))));
    let wake = Arc::new(|| {});
    let (session, mut rx) = connect(&cfg(addr), policy, wake).await.expect("connect");

    session.write(b"ping".to_vec()).expect("write");
    let got = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("未超时")
        .expect("收到回显");
    assert_eq!(&got, b"ping", "PTY 往返:发 ping 应收 ping");
}

#[tokio::test(flavor = "multi_thread")]
async fn resize_then_still_pumps() {
    // F34:resize 后 io_task 应仍在正常收发(不因 window_change 卡死/退出)。
    let addr = common::spawn_echo_server().await;
    let policy = Arc::new(TofuAccept::new(Arc::new(Mutex::new(KnownHosts::new()))));
    let (session, mut rx) = connect(&cfg(addr), policy, Arc::new(|| {}))
        .await
        .expect("connect");
    session.resize(100, 40).expect("resize 应入队成功");
    // resize 之后仍能 echo 往返 → 证明 io_task 处理完 window_change 后仍在泵。
    session.write(b"pong".to_vec()).expect("write");
    let got = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("未超时")
        .expect("收到回显");
    assert_eq!(&got, b"pong", "resize 后仍应能 echo 往返");
}
