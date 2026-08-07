mod common;

use std::sync::{Arc, Mutex};

use mullion_ssh::config::{AuthMethod, SshConfig};
use mullion_ssh::error::ConnectError;
use mullion_ssh::known_hosts::{Fingerprint, KnownHosts, TofuAccept};
use mullion_ssh::session::establish;

fn cfg(addr: std::net::SocketAddr, auth: AuthMethod) -> SshConfig {
    SshConfig {
        host: addr.ip().to_string(),
        port: addr.port(),
        user: common::TEST_USER.to_string(),
        auth,
        cols: 80,
        rows: 24,
        term: "xterm-256color".to_string(),
        hops: Vec::new(),
    }
}

fn tofu() -> Arc<TofuAccept> {
    Arc::new(TofuAccept::new(Arc::new(Mutex::new(KnownHosts::new()))))
}

#[tokio::test(flavor = "multi_thread")]
async fn password_auth_succeeds() {
    let addr = common::spawn_echo_server().await;
    let c = cfg(addr, AuthMethod::Password(common::TEST_PASSWORD.into()));
    assert!(establish(&c, tofu()).await.is_ok(), "正确密码应连上");
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_password_is_auth_failed() {
    let addr = common::spawn_echo_server().await;
    let c = cfg(addr, AuthMethod::Password("nope".into()));
    match establish(&c, tofu()).await {
        Err(ConnectError::AuthFailed) => {}
        Err(e) => panic!("错密码应 AuthFailed,实际 {e:?}"),
        Ok(_) => panic!("错密码应 AuthFailed,实际却连上了"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn pubkey_auth_succeeds() {
    let addr = common::spawn_echo_server().await;
    let c = cfg(
        addr,
        AuthMethod::PublicKey {
            // v5 起 `AuthMethod` 收私钥**正文**,读文件是调用方的事。
            key_data: std::fs::read_to_string("tests/fixtures/client_key").unwrap(),
            passphrase: None,
        },
    );
    assert!(establish(&c, tofu()).await.is_ok(), "pubkey 应连上");
}

#[tokio::test(flavor = "multi_thread")]
async fn changed_host_key_is_rejected() {
    // F3:预置一个不同指纹 → 连接必须 HostKeyChanged。
    let addr = common::spawn_echo_server().await;
    let known = Arc::new(Mutex::new(KnownHosts::new()));
    known
        .lock()
        .unwrap()
        .record(&addr.ip().to_string(), Fingerprint(vec![0xde, 0xad]));
    let policy = Arc::new(TofuAccept::new(known));
    let c = cfg(addr, AuthMethod::Password(common::TEST_PASSWORD.into()));
    match establish(&c, policy).await {
        Err(ConnectError::HostKeyChanged { .. }) => {}
        Err(e) => panic!("主机密钥变更应 HostKeyChanged,实际 {e:?}"),
        Ok(_) => panic!("主机密钥变更应 HostKeyChanged,实际却连上了"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn refused_port_is_connection_refused() {
    // F6:连一个刚释放、无人监听的端口 → ConnectionRefused(区别于 DNS/auth/hostkey)。
    // 端到端走 establish 的 DNS→TCP 分类,不只是 classify_tcp 单测。
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener); // 释放端口:回环上再连极大概率被 RST 拒
    let c = cfg(addr, AuthMethod::Password("x".into()));
    match establish(&c, tofu()).await {
        Err(ConnectError::ConnectionRefused(_)) => {}
        Err(e) => panic!("无人监听的端口应 ConnectionRefused,实际 {e:?}"),
        Ok(_) => panic!("连一个无人监听的端口不该成功"),
    }
}
