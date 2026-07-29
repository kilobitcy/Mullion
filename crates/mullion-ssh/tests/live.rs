//! 真机 live 冒烟。默认 ignore;设 MULLION_LIVE=1 且真机可达时手动跑:
//!   MULLION_LIVE=1 cargo test -p mullion-ssh --test live -- --ignored --nocapture
//! 目标主机/用户/私钥由环境变量提供(未设时用占位,连不通):
//!   MULLION_LIVE_HOST / MULLION_LIVE_USER / MULLION_LIVE_KEY。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use mullion_ssh::config::{AuthMethod, SshConfig};
use mullion_ssh::known_hosts::{KnownHosts, TofuAccept};
use mullion_ssh::session::{connect, establish, open_pty};

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

/// 在 rx 上等到出现 `needle` 为止(10s 超时)。多 pane 场景下每条 channel 都要
/// 单独等一次,不能复用 `run_echo`(它自带 connect,这里要的是共享 handle)。
async fn wait_for(rx: &mut tokio::sync::mpsc::Receiver<Vec<u8>>, needle: &[u8]) -> bool {
    let mut seen = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(chunk) = rx.recv().await {
            seen.extend_from_slice(&chunk);
            if seen.windows(needle.len()).any(|w| w == needle) {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false)
}

/// F35 真机验证:一次 `establish` + 四次 `open_pty`,四条 channel 各跑各的 shell;
/// 再 drop 掉一条,断言其余三条仍能收发(§6.1 的 `Arc` 保活语义)。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "需真机(MULLION_LIVE_HOST 等)+ MULLION_LIVE=1"]
async fn multi_pty_live_f35() {
    if !live_enabled() {
        eprintln!("跳过:未设 MULLION_LIVE=1");
        return;
    }
    let cfg = base(AuthMethod::PublicKey {
        path: std::env::var("MULLION_LIVE_KEY")
            .unwrap_or_else(|_| "/path/to/key.pem".into())
            .into(),
        passphrase: None,
    });
    let policy = Arc::new(TofuAccept::new(Arc::new(Mutex::new(KnownHosts::new()))));
    let handle = Arc::new(establish(&cfg, policy).await.expect("真机握手"));

    let mut panes = Vec::new();
    for _ in 0..4 {
        panes.push(
            open_pty(handle.clone(), &cfg, Arc::new(|| {}))
                .await
                .expect("open_pty"),
        );
    }

    // 每条 channel 打一个不同的标记:串台了断言就会失败。
    for (i, (session, rx)) in panes.iter_mut().enumerate() {
        session
            .write(format!("echo MULLION_PANE_{i}\n").into_bytes())
            .expect("write");
        let needle = format!("MULLION_PANE_{i}");
        assert!(
            wait_for(rx, needle.as_bytes()).await,
            "第 {i} 条 channel 没回显自己的标记"
        );
    }

    // §6.1:关掉一个 pane 不能拖垮别的 pane。
    panes.remove(0);
    for (i, (session, rx)) in panes.iter_mut().enumerate() {
        session
            .write(b"echo MULLION_ALIVE\n".to_vec())
            .expect("write");
        assert!(
            wait_for(rx, b"MULLION_ALIVE").await,
            "drop 掉一条 channel 把幸存的第 {i} 条也带走了"
        );
    }
}
