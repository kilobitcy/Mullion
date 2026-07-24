//! 只验证测试 server 自身:用 russh 原生 client 连上并密码认证成功。
//! (不依赖我们尚未写的 session::connect,避免循环依赖。)

mod common;

use std::sync::Arc;

struct AcceptAll;
impl russh::client::Handler for AcceptAll {
    type Error = russh::Error;
    async fn check_server_key(
        &mut self,
        _key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_server_accepts_password() {
    let addr = common::spawn_echo_server().await;
    let config = Arc::new(russh::client::Config::default());
    let mut handle = russh::client::connect(config, (addr.ip(), addr.port()), AcceptAll)
        .await
        .expect("connect");
    let res = handle
        .authenticate_password(common::TEST_USER, common::TEST_PASSWORD)
        .await
        .expect("auth call");
    assert!(
        matches!(res, russh::client::AuthResult::Success),
        "密码认证应成功"
    );
}
