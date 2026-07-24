//! 真机 live 冒烟。默认 ignore;设 MULLION_LIVE=1 且真机可达时手动跑:
//!   MULLION_LIVE=1 cargo test -p mullion-ssh --test live -- --ignored --nocapture
//! 目标主机/用户/私钥由环境变量提供(未设时用占位,连不通):
//!   MULLION_LIVE_HOST / MULLION_LIVE_USER / MULLION_LIVE_KEY。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use mullion_ssh::config::{AuthMethod, SshConfig};
use mullion_ssh::known_hosts::{KnownHosts, TofuAccept};
use mullion_ssh::session::connect;

fn live_enabled() -> bool {
    std::env::var("MULLION_LIVE").as_deref() == Ok("1")
}

fn base(auth: AuthMethod) -> SshConfig {
    SshConfig {
        host: std::env::var("MULLION_LIVE_HOST").unwrap_or_else(|_| "example.com".into()),
        port: 22,
        user: std::env::var("MULLION_LIVE_USER").unwrap_or_else(|_| "user".into()),
        auth,
        cols: 80,
        rows: 24,
        term: "xterm-256color".into(),
    }
}

async fn run_echo(auth: AuthMethod) {
    let policy = Arc::new(TofuAccept::new(Arc::new(Mutex::new(KnownHosts::new()))));
    let (session, mut rx) = connect(&base(auth), policy, Arc::new(|| {}))
        .await
        .expect("真机连接");
    // 真机是真 shell:发一条命令,断言输出里出现标记。
    session.write(b"echo MULLION_OK\n".to_vec()).expect("write");
    let deadline = Duration::from_secs(10);
    let mut seen = Vec::new();
    let ok = tokio::time::timeout(deadline, async {
        while let Some(chunk) = rx.recv().await {
            seen.extend_from_slice(&chunk);
            if seen.windows(10).any(|w| w == b"MULLION_OK") {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    assert!(
        ok,
        "真机 shell 应回显 MULLION_OK;收到: {:?}",
        String::from_utf8_lossy(&seen)
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "需真机(MULLION_LIVE_HOST 等)+ MULLION_LIVE=1"]
async fn pubkey_live() {
    if !live_enabled() {
        eprintln!("跳过:未设 MULLION_LIVE=1");
        return;
    }
    run_echo(AuthMethod::PublicKey {
        path: std::env::var("MULLION_LIVE_KEY")
            .unwrap_or_else(|_| "/path/to/key.pem".into())
            .into(),
        passphrase: None,
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "需真机 + agent 已加载对应私钥 + MULLION_LIVE=1"]
async fn agent_live() {
    if !live_enabled() {
        eprintln!("跳过:未设 MULLION_LIVE=1");
        return;
    }
    run_echo(AuthMethod::Agent).await;
}
