//! F298 复核(Critical):`exec_with_timeout` 的超时分支必须真的把
//! `SSH_MSG_CHANNEL_CLOSE` 送到服务端,不能只是把整个 `exec` future 一丢
//! 就当作「释放了」——`Channel<Msg>` 没有会发 CHANNEL_CLOSE 的 `Drop`
//! (见 `exec.rs` 顶部注释、ADR-009、F138),丢 future 的后果是 sshd 的
//! `MaxSessions` 槽位永久泄漏,而客户端账本(`ledger`)却显示已经释放。
//!
//! 服务端用「exec 起了但永远不回话」的假 sshd(`hang_exec`)模拟一条挂住的
//! 远端命令,判据是服务端 `channel_close` 钩子被调用的次数——只有这个
//! 钩子被调,才证明关闭货真价实地上了线,不是单靠客户端自己的状态推断。

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::sftp_server::Tree;
use mullion_ssh::config::{AuthMethod, SshConfig};
use mullion_ssh::exec::{exec_with_timeout, ExecError};
use mullion_ssh::known_hosts::{Fingerprint, HostKeyDecision, HostKeyFuture, HostKeyPolicy};
use mullion_ssh::session::establish;

struct AcceptAll;
impl HostKeyPolicy for AcceptAll {
    fn decide<'a>(&'a self, _h: &'a str, _a: &'a str, _f: &'a Fingerprint) -> HostKeyFuture<'a> {
        Box::pin(async { HostKeyDecision::Accept })
    }
}

fn cfg(addr: std::net::SocketAddr) -> SshConfig {
    SshConfig {
        host: addr.ip().to_string(),
        port: addr.port(),
        user: common::TEST_USER.into(),
        auth: AuthMethod::Password(common::TEST_PASSWORD.into()),
        cols: 80,
        rows: 24,
        term: "xterm-256color".into(),
        hops: Vec::new(),
    }
}

/// 自证会变红:把生产代码里 `exec_with_timeout` 的实现换回
/// 「调用方在外面自己套 `tokio::time::timeout(d, exec(conn, cmd))`」
/// 这种写法(即本次复核指出的那个 Critical bug)——channel 永远不会被
/// 显式关闭,`channel_closes` 会卡在 0,这条测试就会失败。
#[tokio::test]
async fn a_timed_out_exec_still_sends_channel_close_to_the_server() {
    let (addr, probe, _tree) = common::spawn_sftp_server_with_hanging_exec(Tree::new()).await;
    let conn = Arc::new(
        establish(&cfg(addr), Arc::new(AcceptAll))
            .await
            .expect("connect"),
    );

    let r = exec_with_timeout(&conn, b"sleep 999".to_vec(), Duration::from_millis(200)).await;
    assert!(
        matches!(r, Err(ExecError::Timeout)),
        "超时必须报 ExecError::Timeout,拿到的是 {r:?}"
    );

    // CHANNEL_CLOSE 是异步送达的,给服务端的事件循环一点时间处理完这条消息。
    for _ in 0..50 {
        if probe.lock().unwrap().channel_closes >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        probe.lock().unwrap().channel_closes,
        1,
        "超时之后必须真的关掉 channel(服务端要收到 CHANNEL_CLOSE)—— \
         不然 sshd 的 MaxSessions 槽位永久占着不还,客户端却以为释放了"
    );
}

/// 对照组:没有超时预算(`exec` 本身)不该受这条新逻辑影响——命令正常跑完
/// 依然正常返回,不会把「没设超时」误判成「立刻超时」。
#[tokio::test]
async fn exec_without_a_timeout_still_completes_normally() {
    let (addr, probe, _tree) = common::spawn_sftp_server(Tree::new()).await;
    let conn = Arc::new(
        establish(&cfg(addr), Arc::new(AcceptAll))
            .await
            .expect("connect"),
    );

    let out = mullion_ssh::exec::exec(&conn, b"rm -rf -- '/nonexistent'".to_vec())
        .await
        .expect("exec 不该失败");
    assert!(out.succeeded(), "{out:?}");

    // 同上:CHANNEL_CLOSE 完成握手是异步的,给它一点时间送达。
    for _ in 0..50 {
        if probe.lock().unwrap().channel_closes >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(probe.lock().unwrap().channel_closes, 1);
}
