//! F3-a:TOFU 的键必须带端口 —— 同一个 host 上的两台主机不能互相判为
//! 「主机密钥已变更」。
//!
//! 这两条测试扎的是**注入点**而不是 `host_key_id` 本身(那个有自己的单测):
//! 键是在 `session::handshake_and_auth` 里组出来的,`establish` 与 `dial`
//! 各自要把自己那一段的端口传进去。少传一处,那条路径就退回按裸 host 记,
//! 而单测照样全绿。

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use mullion_ssh::config::{AuthMethod, SshConfig};
use mullion_ssh::error::ConnectError;
use mullion_ssh::hop::Hop;
use mullion_ssh::known_hosts::{
    Fingerprint, HostKeyDecision, HostKeyFuture, HostKeyPolicy, KnownHosts, TofuAccept,
};
use mullion_ssh::session::establish;

fn cfg(addr: std::net::SocketAddr) -> SshConfig {
    SshConfig {
        host: addr.ip().to_string(),
        port: addr.port(),
        user: common::TEST_USER.to_string(),
        auth: AuthMethod::Password(common::TEST_PASSWORD.into()),
        cols: 80,
        rows: 24,
        term: "xterm-256color".to_string(),
        hops: Vec::new(),
    }
}

/// 本 bug 的直接复现:两台**指纹不同**的主机摆在同一个 127.0.0.1 上
/// (端口由内核随机分配,必然互不相同,也必然不是 22),共用一张 TOFU 表。
///
/// 键不含端口时:第一台记下指纹 → 第二台命中同一条记录、指纹对不上 →
/// `Reject(Changed)` → `establish` 报 `HostKeyChanged`,也就是用户看到的
/// 那个「主机密钥已变更(警告)」弹窗。
#[tokio::test(flavor = "multi_thread")]
async fn two_hosts_behind_the_same_address_do_not_shadow_each_other() {
    let first = common::spawn_echo_server_with_hostkey("tests/fixtures/server_hostkey").await;
    let second = common::spawn_echo_server_with_hostkey("tests/fixtures/other_key").await;
    assert_ne!(first.port(), second.port(), "两台必须落在不同端口");

    // 同一张表 = 真实 App 的形态(main.rs 全局只建一份)。
    let known = Arc::new(Mutex::new(KnownHosts::new()));

    for (label, addr) in [("第一台", first), ("第二台", second)] {
        let policy = Arc::new(TofuAccept::new(known.clone()));
        let r = tokio::time::timeout(Duration::from_secs(10), establish(&cfg(addr), policy))
            .await
            .unwrap_or_else(|_| panic!("{label} establish 不应挂起"));
        match r {
            Ok(_) => {}
            Err(ConnectError::HostKeyChanged { host, .. }) => panic!(
                "{label}被判成主机密钥变更(键 `{host}` 被另一台占着)—— \
                 TOFU 键没带端口,两台机器互相当中间人"
            ),
            Err(e) => panic!("{label}连接失败: {e}"),
        }
    }
}

/// 记下策略实际收到的每一个键,顺序即校验顺序(先跳板,后目标)。
struct RecordIds(Arc<Mutex<Vec<String>>>);
impl HostKeyPolicy for RecordIds {
    fn decide<'a>(
        &'a self,
        host_id: &'a str,
        _algo: &'a str,
        _fp: &'a Fingerprint,
    ) -> HostKeyFuture<'a> {
        self.0.lock().unwrap().push(host_id.to_owned());
        Box::pin(std::future::ready(HostKeyDecision::Accept))
    }
}

/// 跳板自己的键同样要带端口(F3 对跳板一视同仁,设计 §5.3)。
///
/// 这里的 `EchoHandler` 不会开 `direct-tcpip`,所以整条 `establish` 注定失败
/// —— 但**跳板的 `check_server_key` 在那之前已经跑过了**,这正是要断言的东西。
/// 用它当跳板,省得把 `two_hop_jump.rs` 里那个转发 server 搬进 `common`。
#[tokio::test(flavor = "multi_thread")]
async fn the_jump_host_key_is_also_recorded_per_port() {
    let jump = common::spawn_echo_server().await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let policy: Arc<dyn HostKeyPolicy> = Arc::new(RecordIds(seen.clone()));

    let mut cfg = cfg(jump);
    cfg.host = "10.0.0.9".to_string(); // 目标随便填:走不到那一步。
    cfg.port = 22;
    cfg.hops = vec![Hop::SshJump {
        host: jump.ip().to_string(),
        port: jump.port(),
        user: common::TEST_USER.to_string(),
        auth: AuthMethod::Password(common::TEST_PASSWORD.into()),
    }];

    let _ = tokio::time::timeout(Duration::from_secs(10), establish(&cfg, policy))
        .await
        .expect("establish 不应挂起");

    assert_eq!(
        seen.lock().unwrap().as_slice(),
        [format!("[{}]:{}", jump.ip(), jump.port())],
        "跳板的 TOFU 键必须带上它自己的端口"
    );
}
