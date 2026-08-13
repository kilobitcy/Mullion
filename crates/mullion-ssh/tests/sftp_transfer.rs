//! SFTP 流式传输原语的端到端测试(F52):真握手 → 开 sftp subsystem →
//! 分块读 / 分块写。服务端是同进程的可写假 SFTP(见 `common/sftp_server.rs`)。
//!
//! 判据一律是**字节**:读回来的和树上的一模一样、写下去的和树上的一模一样。
//! 只断言「返回 Ok」是恒绿的——一个把偏移算错、把尾巴丢掉的实现同样返回 Ok。

mod common;

use std::sync::Arc;

use common::sftp_server::{Node, Tree};
use mullion_ssh::config::{AuthMethod, SshConfig};
use mullion_ssh::known_hosts::{Fingerprint, HostKeyDecision, HostKeyFuture, HostKeyPolicy};
use mullion_ssh::session::{establish, SshConnection};
use mullion_ssh::sftp::{RemotePath, SftpClient};

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

async fn client(addr: std::net::SocketAddr) -> SftpClient {
    let conn: Arc<SshConnection> = Arc::new(
        establish(&cfg(addr), Arc::new(AcceptAll))
            .await
            .expect("connect"),
    );
    SftpClient::open(conn).await.expect("open sftp")
}

fn rp(s: &str) -> RemotePath {
    RemotePath::from_bytes(s.as_bytes().to_vec())
}

fn tree_with(files: Vec<Node>) -> Tree {
    let mut t = Tree::new();
    t.insert(b"/home/testuser".to_vec(), files);
    t
}

/// 比一次 READ 拿得到的多得多。分块循环少走一轮 / 偏移不推进,
/// 尾巴都会静默丢失,而长度断言会当场揪出来。
fn payload(len: usize, modulo: u32) -> Vec<u8> {
    (0..len as u32).map(|i| (i % modulo) as u8).collect()
}

#[tokio::test]
async fn reading_a_remote_file_yields_every_byte_even_when_it_spans_many_chunks() {
    let want = payload(200_000, 251);
    let (addr, _probe, _tree) =
        common::spawn_sftp_server(tree_with(vec![Node::file_with(b"big.bin", &want)])).await;
    let sftp = client(addr).await;

    let mut f = sftp
        .open_read(&rp("/home/testuser/big.bin"))
        .await
        .expect("打开读");
    let mut got = Vec::new();
    let mut buf = vec![0u8; 32 * 1024];
    loop {
        let n = f.read_chunk(&mut buf).await.expect("读一块");
        if n == 0 {
            break;
        }
        got.extend_from_slice(&buf[..n]);
    }

    assert_eq!(got.len(), want.len(), "读回来的长度不对");
    assert_eq!(got, want, "读回来的内容不对");
}

#[tokio::test]
async fn writing_a_remote_file_lands_every_byte_on_the_server() {
    let want = payload(150_000, 97);
    let (addr, _probe, tree_h) = common::spawn_sftp_server(tree_with(Vec::new())).await;
    let sftp = client(addr).await;

    let mut f = sftp
        .open_write(&rp("/home/testuser/out.bin"), true)
        .await
        .expect("打开写");
    for chunk in want.chunks(30_000) {
        f.write_chunk(chunk).await.expect("写一块");
    }
    f.finish().await.expect("收尾");

    let t = tree_h.lock().unwrap();
    let node = t[&b"/home/testuser".to_vec()]
        .iter()
        .find(|n| n.name == b"out.bin")
        .expect("服务端上没有这个文件");
    assert_eq!(node.data.len(), want.len(), "服务端上的长度不对");
    assert_eq!(node.data, want, "服务端上的字节与写入的不一致");
}

/// 覆盖写必须先截断:不截的话短内容盖长内容会留下前一版的尾巴,
/// 而文件看起来「传成功了」。
#[tokio::test]
async fn overwriting_with_a_shorter_file_leaves_no_tail_from_the_previous_content() {
    let (addr, _probe, tree_h) =
        common::spawn_sftp_server(tree_with(vec![Node::file_with(b"a.bin", b"AAAAAAAAAAAA")]))
            .await;
    let sftp = client(addr).await;

    let mut f = sftp
        .open_write(&rp("/home/testuser/a.bin"), true)
        .await
        .expect("打开写");
    f.write_chunk(b"BB").await.expect("写");
    f.finish().await.expect("收尾");

    let t = tree_h.lock().unwrap();
    let node = t[&b"/home/testuser".to_vec()]
        .iter()
        .find(|n| n.name == b"a.bin")
        .expect("文件没了");
    assert_eq!(node.data, b"BB", "旧内容的尾巴还在:{:?}", node.data);
}

#[tokio::test]
async fn a_non_utf8_path_never_reaches_the_wire() {
    // D16:非 UTF-8 名一个请求都不发。判据是探针里一条 open 都没有 ——
    // 只断言「返回 Err」的话,一个先发请求再报错的实现照样绿。
    let (addr, probe, _tree) = common::spawn_sftp_server(tree_with(Vec::new())).await;
    let sftp = client(addr).await;
    let bad = RemotePath::from_bytes(vec![b'/', b'h', 0xff, b'x']);

    assert!(sftp.open_read(&bad).await.is_err(), "非 UTF-8 名不该成功");
    assert!(
        sftp.open_write(&bad, true).await.is_err(),
        "非 UTF-8 名不该成功"
    );

    let seen = probe.lock().unwrap().paths_for("open");
    assert!(seen.is_empty(), "非 UTF-8 名竟然发出了 open:{seen:?}");
}

#[tokio::test]
async fn exists_tells_a_missing_target_apart_from_an_existing_one() {
    // 冲突探测的地基:分不清就只能一律当成冲突,每传一个文件都弹一次窗。
    let (addr, _probe, _tree) =
        common::spawn_sftp_server(tree_with(vec![Node::file_with(b"here.bin", b"x")])).await;
    let sftp = client(addr).await;

    assert!(
        sftp.exists(&rp("/home/testuser/here.bin"))
            .await
            .expect("查在不在"),
        "已有的文件该报存在"
    );
    assert!(
        !sftp
            .exists(&rp("/home/testuser/nope.bin"))
            .await
            .expect("查在不在"),
        "没有的文件该报不存在"
    );
}
