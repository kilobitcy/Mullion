mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use mullion_ssh::config::{AuthMethod, SshConfig};
use mullion_ssh::known_hosts::{KnownHosts, TofuAccept};
use mullion_ssh::session::{connect, establish, open_pty};

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

/// F35:一次握手、多条 channel。分屏的全部价值都压在这条上 —— 每开一个 pane
/// 就重新 TCP + 认证一次的话,高延迟代理链路下开 4 屏要等好几秒。
#[tokio::test(flavor = "multi_thread")]
async fn one_handshake_serves_many_ptys_f35() {
    let addr = common::spawn_echo_server().await;
    let policy = Arc::new(TofuAccept::new(Arc::new(Mutex::new(KnownHosts::new()))));
    let handle = Arc::new(establish(&cfg(addr), policy).await.expect("establish"));

    let mut sessions = Vec::new();
    for _ in 0..4 {
        let (s, rx) = open_pty(handle.clone(), &cfg(addr), Arc::new(|| {}))
            .await
            .expect("open_pty");
        sessions.push((s, rx));
    }

    for (i, (s, rx)) in sessions.iter_mut().enumerate() {
        let msg = format!("pane{i}");
        s.write(msg.as_bytes().to_vec()).expect("write");
        let got = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("未超时")
            .expect("收到回显");
        assert_eq!(
            got,
            msg.as_bytes(),
            "第 {i} 条 channel 的回显串到别的 channel 了"
        );
    }
}

/// 关掉一个 pane 不能拖垮别的 pane:`Handle` 用 Arc 共享,最后一个引用释放才断连。
#[tokio::test(flavor = "multi_thread")]
async fn dropping_one_pty_keeps_the_others_alive_f35() {
    let addr = common::spawn_echo_server().await;
    let policy = Arc::new(TofuAccept::new(Arc::new(Mutex::new(KnownHosts::new()))));
    let handle = Arc::new(establish(&cfg(addr), policy).await.expect("establish"));

    let (doomed, doomed_rx) = open_pty(handle.clone(), &cfg(addr), Arc::new(|| {}))
        .await
        .expect("open_pty 1");
    let (survivor, mut survivor_rx) = open_pty(handle.clone(), &cfg(addr), Arc::new(|| {}))
        .await
        .expect("open_pty 2");
    drop(doomed);
    drop(doomed_rx);

    survivor.write(b"still here".to_vec()).expect("write");
    let got = tokio::time::timeout(Duration::from_secs(5), survivor_rx.recv())
        .await
        .expect("未超时")
        .expect("收到回显");
    assert_eq!(&got, b"still here", "关掉一个 pane 把整条连接带走了");
}
