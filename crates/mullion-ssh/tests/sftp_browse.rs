//! SFTP 只读浏览的端到端测试:真握手 → 开 sftp subsystem → 列目录。
//! 服务端是同进程的假 SFTP(见 common/sftp_server.rs)。

mod common;

use std::sync::Arc;

use common::sftp_server::{Node, Tree};
use mullion_ssh::config::{AuthMethod, SshConfig};
use mullion_ssh::known_hosts::{Fingerprint, HostKeyDecision, HostKeyFuture, HostKeyPolicy};
use mullion_ssh::session::establish;
use mullion_ssh::sftp::{EntryKind, RemotePath, SftpClient};

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

fn tree() -> Tree {
    let mut t = Tree::new();
    t.insert(
        b"/home/testuser".to_vec(),
        vec![
            Node::dir(b"docs"),
            Node::file(b"a.txt", 12),
            // UTF-8 中文名:必须逐字节往返。
            Node::file("说明.md".as_bytes(), 34),
            // 非 UTF-8 名(GBK 的「中文」):库会把它 lossy 掉,我们要认出来。
            Node::file(&[0xd6, 0xd0, 0xce, 0xc4, b'.', b't', b'x', b't'], 7),
            Node::link(b"link", b"/etc"),
            Node::file(b".hidden", 1),
        ],
    );
    t.insert(
        b"/home/testuser/docs".to_vec(),
        vec![Node::file(b"inner.txt", 3)],
    );
    t
}

#[tokio::test]
async fn listing_a_directory_returns_kinds_sizes_and_link_targets() {
    let (addr, _probe, _tree) = common::spawn_sftp_server(tree()).await;
    let conn = Arc::new(
        establish(&cfg(addr), Arc::new(AcceptAll))
            .await
            .expect("connect"),
    );
    let sftp = SftpClient::open(conn).await.expect("open sftp");

    let mut got = sftp
        .list_dir(&RemotePath::from_bytes(b"/home/testuser".to_vec()))
        .await
        .expect("list");
    got.sort_by(|a, b| a.name.cmp(&b.name));

    let names: Vec<String> = got.iter().map(|e| e.name.display().to_string()).collect();
    assert!(names.contains(&"docs".to_string()));
    assert!(
        names.contains(&"说明.md".to_string()),
        "中文名必须原样出现: {names:?}"
    );

    let docs = got.iter().find(|e| e.name.as_bytes() == b"docs").unwrap();
    assert_eq!(docs.kind, EntryKind::Dir);

    let a = got.iter().find(|e| e.name.as_bytes() == b"a.txt").unwrap();
    assert_eq!(a.kind, EntryKind::File);
    assert_eq!(a.size, 12);
    assert_eq!(a.mode & 0o777, 0o644);
    assert_eq!((a.uid, a.gid), (1000, 1000));

    let link = got.iter().find(|e| e.name.as_bytes() == b"link").unwrap();
    assert_eq!(link.kind, EntryKind::Symlink);
    assert_eq!(
        link.link_target.as_ref().map(|t| t.as_bytes()),
        Some(&b"/etc"[..]),
        "符号链接要显示 name → target(D21)"
    );
}

/// 中文路径逐字节往返:进子目录时发出去的**就是**那串 UTF-8 字节。
#[tokio::test]
async fn a_utf8_chinese_directory_is_requested_byte_for_byte() {
    let mut t = tree();
    t.insert(
        "/home/testuser/文档".as_bytes().to_vec(),
        vec![Node::file(b"x", 1)],
    );
    let (addr, probe, _tree) = common::spawn_sftp_server(t).await;
    let conn = Arc::new(
        establish(&cfg(addr), Arc::new(AcceptAll))
            .await
            .expect("connect"),
    );
    let sftp = SftpClient::open(conn).await.expect("open sftp");

    let dir = RemotePath::from_bytes("/home/testuser/文档".as_bytes().to_vec());
    sftp.list_dir(&dir).await.expect("list 中文目录");

    let seen = probe.lock().unwrap().paths_for("opendir");
    assert!(
        seen.iter()
            .any(|p| p.as_bytes() == "/home/testuser/文档".as_bytes()),
        "服务端收到的字节必须与请求的一致: {seen:?}"
    );
}

/// D16 修订(+ 实施期补丁)的核心守护:**非 UTF-8 名字列得出来、显示得出来,
/// 但一个请求都发不出去**。
///
/// 实施期发现 `russh-sftp 2.4.0` 的 `buf.rs:25` 在**两个方向**都过
/// `from_utf8_lossy`:服务端把 GBK 原始字节塞进 `readdir` 响应时就已经 lossy
/// 了一次,到客户端手里的名字**恒为合法 UTF-8**,只是带着 `U+FFFD`,原始字节
/// 已经丢了。所以判据不能是 `is_utf8()`(它对这类条目恒 `true`),而是
/// `is_operable()` —— 含 `U+FFFD` 的串拿去请求必然打不中那个文件。
///
/// 两个来源都要扎住:
/// 1. **从线上收回来的**:lossy 过的条目(本切片能碰到的唯一形态)。
/// 2. **客户端本地持有的**:真·非 UTF-8 字节(用户手输、或将来换了能透传
///    字节的协议库)。
#[tokio::test]
async fn a_non_utf8_name_is_listed_but_no_request_is_ever_sent_for_it() {
    let (addr, probe, _tree) = common::spawn_sftp_server(tree()).await;
    let conn = Arc::new(
        establish(&cfg(addr), Arc::new(AcceptAll))
            .await
            .expect("connect"),
    );
    let sftp = SftpClient::open(conn).await.expect("open sftp");

    // 来源 1:走过线的 lossy 名字 —— 列得出来、显示得出来,但不可操作。
    let got = sftp
        .list_dir(&RemotePath::from_bytes(b"/home/testuser".to_vec()))
        .await
        .expect("list");
    let lossy = got
        .iter()
        .find(|e| e.name.display().contains('\u{fffd}'))
        .expect("lossy 条目该出现在列表里");
    assert!(
        lossy.name.is_utf8(),
        "走了一趟线的 lossy 名字是合法 UTF-8(库两端都 from_utf8_lossy 过)——\
         这正是不能拿 is_utf8() 当判据的原因"
    );
    assert!(
        !lossy.name.is_operable(),
        "含 U+FFFD 的名字必须被判成不可操作,界面才有理由把它的操作入口置灰"
    );

    let before = probe.lock().unwrap().paths_for("opendir").len();
    let err = sftp
        .list_dir(&lossy.name)
        .await
        .expect_err("lossy 名字不该发得出去");
    assert!(matches!(err, mullion_ssh::sftp::SftpError::NonUtf8Name));

    // 来源 2:本地持有的真·非 UTF-8 字节(GBK「中文.txt」,没经过 lossy)。
    let raw_gbk = RemotePath::from_bytes(vec![0xd6, 0xd0, 0xce, 0xc4, b'.', b't', b'x', b't']);
    assert!(!raw_gbk.is_utf8(), "GBK 原始字节本就不是合法 UTF-8");
    let err = sftp
        .list_dir(&raw_gbk)
        .await
        .expect_err("非 UTF-8 路径不该发得出去");
    assert!(matches!(err, mullion_ssh::sftp::SftpError::NonUtf8Name));

    let seen = probe.lock().unwrap().paths_for("opendir");
    assert_eq!(
        seen.len(),
        before,
        "被挡下的路径不该多产生任何 opendir 请求: {seen:?}"
    );
    assert!(
        !seen.iter().any(|p| p.contains('\u{fffd}')),
        "带替换字符的路径一旦发出去,服务端只会回一条用户看不懂的 NoSuchFile: {seen:?}"
    );
}

/// `.` 要能解析成登录目录 —— 默认远端目录留空时走的就是这条(D15)。
#[tokio::test]
async fn a_dot_path_canonicalizes_to_the_login_directory() {
    let (addr, _probe, _tree) = common::spawn_sftp_server(tree()).await;
    let conn = Arc::new(
        establish(&cfg(addr), Arc::new(AcceptAll))
            .await
            .expect("connect"),
    );
    let sftp = SftpClient::open(conn).await.expect("open sftp");

    let home = sftp
        .canonicalize(&RemotePath::from_bytes(b".".to_vec()))
        .await
        .expect("canonicalize");
    assert_eq!(home.as_bytes(), b"/home/testuser");
}

/// SFTP 通道**不请求 PTY**。请求了的后果不是报错,是远端白白起一个
/// 伪终端、`who` 里多一行幽灵会话,而且 sshd 的 `ForceCommand`/
/// `PermitTTY no` 环境下会直接被拒。
#[tokio::test]
async fn opening_sftp_never_requests_a_pty() {
    let (addr, probe, _tree) = common::spawn_sftp_server(tree()).await;
    let conn = Arc::new(
        establish(&cfg(addr), Arc::new(AcceptAll))
            .await
            .expect("connect"),
    );
    let sftp = SftpClient::open(conn).await.expect("open sftp");
    sftp.list_dir(&RemotePath::from_bytes(b"/home/testuser".to_vec()))
        .await
        .expect("list");
    assert_eq!(
        probe.lock().unwrap().pty_requests,
        0,
        "SFTP 通道上不该有任何 pty_request"
    );
}

/// D6:侧栏模式蹭会话那条连接 —— 开 sftp 不重握手。
/// 判据是「同一个 `Arc<SshConnection>` 上能开出两个客户端」,
/// 开第二个不需要任何网络参数(签名里就没有)。
#[tokio::test]
async fn a_second_sftp_client_reuses_the_same_connection() {
    let (addr, _probe, _tree) = common::spawn_sftp_server(tree()).await;
    let conn = Arc::new(
        establish(&cfg(addr), Arc::new(AcceptAll))
            .await
            .expect("connect"),
    );
    let a = SftpClient::open(conn.clone()).await.expect("first");
    let b = SftpClient::open(conn.clone()).await.expect("second");
    assert!(!conn.is_closed());
    assert!(a
        .list_dir(&RemotePath::from_bytes(b"/home/testuser".to_vec()))
        .await
        .is_ok());
    assert!(b
        .list_dir(&RemotePath::from_bytes(b"/home/testuser/docs".to_vec()))
        .await
        .is_ok());
}

/// 计划的验收条款:**这一版零写操作**——跑完一整套浏览动作(开 sftp、
/// canonicalize、列几个目录、进子目录),假服务端的写操作探针必须是空的。
///
/// 这条跟「`SftpClient` 没有暴露写方法」不是同一件事,不冗余:后者是编译期
/// 保证,只覆盖我们自己写的调用;这条覆盖的是**整条链路实际发到线上的报文**,
/// 包括 `russh-sftp` 内部可能替我们发的东西。将来 D2 加写操作时,这条会
/// 变红——那是提醒改测试意图,不是提醒删掉它。
#[tokio::test]
async fn a_full_browse_session_never_sends_a_single_write_request() {
    let (addr, probe, _tree) = common::spawn_sftp_server(tree()).await;
    let conn = Arc::new(
        establish(&cfg(addr), Arc::new(AcceptAll))
            .await
            .expect("connect"),
    );
    let sftp = SftpClient::open(conn).await.expect("open sftp");

    sftp.canonicalize(&RemotePath::from_bytes(b".".to_vec()))
        .await
        .expect("canonicalize");
    sftp.list_dir(&RemotePath::from_bytes(b"/home/testuser".to_vec()))
        .await
        .expect("列登录目录");
    sftp.list_dir(&RemotePath::from_bytes(b"/home/testuser/docs".to_vec()))
        .await
        .expect("进子目录");

    let p = probe.lock().unwrap();
    assert!(
        p.paths_for("remove").is_empty(),
        "只读切片发出了删除请求: {:?}",
        p.paths_for("remove")
    );
    // 反面自检:探针本身是活的。列目录必然留下 opendir 记录,若这里也是空的,
    // 说明上面那条断言是「探针根本没在记」造成的真空成立,不是真的零写。
    assert!(
        !p.paths_for("opendir").is_empty(),
        "探针没记到任何 opendir —— 上面那条零写断言等于没测"
    );
}
