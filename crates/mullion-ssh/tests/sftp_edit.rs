//! 编辑通路的全量读写(F53):真握手 → 开 sftp subsystem → `read_all` /
//! `write_all_truncate`。服务端是同进程的可写假 SFTP(见 `common/sftp_server.rs`)。
//!
//! 判据一律是**字节**。只断言「返回 Ok」是恒绿的 —— 一个把尾巴丢掉、
//! 把上限判反的实现同样返回 Ok。

mod common;

use std::sync::Arc;

use common::sftp_server::{Node, Tree};
use mullion_ssh::config::{AuthMethod, SshConfig};
use mullion_ssh::known_hosts::{Fingerprint, HostKeyDecision, HostKeyFuture, HostKeyPolicy};
use mullion_ssh::session::{establish, SshConnection};
use mullion_ssh::sftp::{RemotePath, SftpClient, SftpError, WriteOutcome};

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

fn payload(len: usize, modulo: u32) -> Vec<u8> {
    (0..len as u32).map(|i| (i % modulo) as u8).collect()
}

/// 服务端树上那个文件此刻的字节。判据只认这个 —— 客户端返回 `Ok`
/// 说明不了远端到底变成了什么样。
fn stored(tree: &Arc<std::sync::Mutex<Tree>>, name: &[u8]) -> Vec<u8> {
    let t = tree.lock().unwrap();
    t[&b"/home/testuser".to_vec()]
        .iter()
        .find(|n| n.name == name)
        .expect("服务端上没有这个文件")
        .data
        .clone()
}

/// 全量读要跨很多次 READ 才读得完。少走一轮 / 偏移不推进,尾巴都会静默丢失。
#[tokio::test]
async fn reading_a_whole_file_yields_every_byte_across_many_chunks() {
    let want = payload(300_000, 251);
    let (addr, _probe, _tree) =
        common::spawn_sftp_server(tree_with(vec![Node::file_with(b"conf.txt", &want)])).await;
    let sftp = client(addr).await;

    let (got, timing) = sftp
        .read_all(&rp("/home/testuser/conf.txt"), 1024 * 1024, None)
        .await
        .expect("read_all");
    assert_eq!(got.len(), want.len(), "长度对不上说明分块循环漏了一段");
    assert_eq!(got, want);
    assert!(
        timing.reads >= 2,
        "300 KB 一次 READ 读不完,分块循环没跑起来"
    );
}

/// 上限必须**边读边判**:读完再看长度的话,一个几 GB 的文件会在「拒绝」
/// 之前先把进程 OOM 掉,用户看到的是程序凭空消失。
#[tokio::test]
async fn a_file_over_the_limit_is_refused_with_a_reason_instead_of_filling_memory() {
    let big = payload(200_000, 251);
    let (addr, _probe, _tree) =
        common::spawn_sftp_server(tree_with(vec![Node::file_with(b"huge.bin", &big)])).await;
    let sftp = client(addr).await;

    let err = sftp
        .read_all(&rp("/home/testuser/huge.bin"), 64 * 1024, None)
        .await
        .expect_err("超限该拒绝");
    assert!(
        matches!(err, SftpError::TooLarge(n) if n == 64 * 1024),
        "该给出可操作的超限原因,实际:{err:?}"
    );
    // 反面:同一个文件在上限之内必须读得回来 —— 判据写反了(比如恒拒绝)
    // 上面那条照样绿。
    let (ok, _) = sftp
        .read_all(&rp("/home/testuser/huge.bin"), 1024 * 1024, None)
        .await
        .expect("上限之内该读得回来");
    assert_eq!(ok, big);
}

/// 写回是**覆盖**语义:原文件比新内容长时,尾巴必须被截掉。
/// 不带 TRUNC 的话,把一个 100 行的配置改成 3 行,远端会剩下 97 行旧内容 ——
/// 而且文件看起来「保存成功了」。
#[tokio::test]
async fn writing_back_truncates_so_a_shorter_file_leaves_no_tail() {
    let old = b"########## old and long ##########".to_vec();
    let (addr, _probe, tree) =
        common::spawn_sftp_server(tree_with(vec![Node::file_with(b"nginx.conf", &old)])).await;
    let sftp = client(addr).await;

    let new = b"short".to_vec();
    sftp.write_all_truncate(&rp("/home/testuser/nginx.conf"), &new)
        .await
        .expect("write_all_truncate");

    let got = stored(&tree, b"nginx.conf");
    assert_eq!(got, new, "尾巴没截干净:{}", String::from_utf8_lossy(&got));
}

/// 绕过客户端,直接把服务端树上那个文件换掉 —— 模拟「我们编辑期间别人改了它」。
fn overwrite_on_server(tree: &Arc<std::sync::Mutex<Tree>>, name: &[u8], data: &[u8]) {
    let mut t = tree.lock().unwrap();
    let n = t
        .get_mut(&b"/home/testuser".to_vec())
        .expect("目录不在")
        .iter_mut()
        .find(|n| n.name == name)
        .expect("服务端上没有这个文件");
    n.data = data.to_vec();
    n.size = data.len() as u64;
}

fn exists_on_server(tree: &Arc<std::sync::Mutex<Tree>>, name: &[u8]) -> bool {
    tree.lock().unwrap()[&b"/home/testuser".to_vec()]
        .iter()
        .any(|n| n.name == name)
}

/// D3-8 的**核心守护**:打开之后远端被别人改了,回传必须走冲突分支,
/// 而且远端**一个字节都不能被覆盖**。
///
/// 只断言「返回了 Conflict」是不够的 —— 一个「先写下去再报冲突」的实现
/// 照样满足。判据必须落在服务端树上的字节。
#[tokio::test]
async fn a_remote_changed_behind_our_back_is_refused_without_touching_a_single_byte() {
    let orig = b"listen 80;\n".to_vec();
    let (addr, _probe, tree) =
        common::spawn_sftp_server(tree_with(vec![Node::file_with(b"nginx.conf", &orig)])).await;
    let sftp = client(addr).await;
    let path = rp("/home/testuser/nginx.conf");

    // 打开:内容 + 那一刻的戳。
    let (got, _) = sftp
        .read_all(&path, 1024 * 1024, None)
        .await
        .expect("read_all");
    assert_eq!(got, orig);
    let st = sftp.stat(&path).await.expect("stat");
    let snapshot = (st.mtime, st.size);

    // 我们在改的期间,别人也改了同一个文件。
    let theirs = b"listen 8080; # someone else was here\n".to_vec();
    overwrite_on_server(&tree, b"nginx.conf", &theirs);

    let bak = rp("/home/testuser/nginx.conf.mullion.bak");
    let outcome = sftp
        .write_if_unchanged(&path, b"listen 443;\n", snapshot, Some((&bak, &orig)))
        .await
        .expect("write_if_unchanged");

    assert!(
        matches!(outcome, WriteOutcome::Conflict { size, .. } if size == theirs.len() as u64),
        "该报冲突并带回远端当前的戳,实际:{outcome:?}"
    );
    assert_eq!(
        stored(&tree, b"nginx.conf"),
        theirs,
        "冲突时远端被动过了 —— 别人的改动就是这么没的"
    );
    assert!(
        !exists_on_server(&tree, b"nginx.conf.mullion.bak"),
        "冲突时不该留下备份文件:一份没人要的垃圾,还会让用户以为已经写过一轮了"
    );
}

/// 反面:没人动过时必须**真的写下去**、留下备份、并带回**回读的**新戳。
/// 缺了这条,上面那个测试对一个「恒报冲突、从不写」的实现同样是绿的。
#[tokio::test]
async fn an_untouched_remote_is_written_with_a_backup_and_a_freshly_read_stamp() {
    let orig = b"listen 80;\n".to_vec();
    let (addr, _probe, tree) =
        common::spawn_sftp_server(tree_with(vec![Node::file_with(b"nginx.conf", &orig)])).await;
    let sftp = client(addr).await;
    let path = rp("/home/testuser/nginx.conf");

    let st = sftp.stat(&path).await.expect("stat");
    let snapshot = (st.mtime, st.size);
    let mine = b"listen 443;\nssl on;\n".to_vec();
    let bak = rp("/home/testuser/nginx.conf.mullion.bak");
    let outcome = sftp
        .write_if_unchanged(&path, &mine, snapshot, Some((&bak, &orig)))
        .await
        .expect("write_if_unchanged");

    assert_eq!(stored(&tree, b"nginx.conf"), mine, "内容没写进去");
    assert_eq!(
        stored(&tree, b"nginx.conf.mullion.bak"),
        orig,
        "备份里该是**改之前**那一份,而不是我们刚写的这份"
    );
    // 新戳要能当下一轮的比对基准:拿本地长度顶替是不行的,所以断言它
    // 确实等于服务端此刻的 size。
    assert_eq!(
        outcome,
        WriteOutcome::Written {
            mtime: st.mtime,
            size: mine.len() as u64,
        },
        "写完该回读一次属性再带回来"
    );

    // 拿这个新戳再存一次必须仍然是「没冲突」——不成立的话用户在 vim 里
    // 存第二次就会被冤枉成冲突。
    let WriteOutcome::Written { mtime, size } = outcome else {
        unreachable!()
    };
    let again = sftp
        .write_if_unchanged(&path, b"listen 8443;\n", (mtime, size), None)
        .await
        .expect("write_if_unchanged");
    assert!(
        matches!(again, WriteOutcome::Written { .. }),
        "第二次保存被误判成冲突了:{again:?}"
    );
}

/// 大内容要分很多块写出去,一块都不能漏。
#[tokio::test]
async fn writing_back_a_large_buffer_stores_every_byte() {
    let (addr, _probe, tree) =
        common::spawn_sftp_server(tree_with(vec![Node::file_with(b"data.bin", b"x")])).await;
    let sftp = client(addr).await;

    let want = payload(300_000, 241);
    sftp.write_all_truncate(&rp("/home/testuser/data.bin"), &want)
        .await
        .expect("write_all_truncate");

    let got = stored(&tree, b"data.bin");
    assert_eq!(got.len(), want.len());
    assert_eq!(got, want);
}

/// F214:**知道大小就少发一次 READ。**
///
/// SFTP 的 READ 在 `russh_sftp` 里是串行的(对照 WRITE 有
/// `max_concurrent_writes`,READ 没有对应项)。本项目的主场景是高延迟代理
/// 链路,于是「打开一个几十 KB 的配置文件要等好几秒」几乎完全等于**发了几次
/// READ** —— 与文件大小无关。而其中有一次是纯浪费:不告诉读循环文件多大,
/// 它只能一直读到服务端回 `EOF` 才知道到头了,那次什么都没读到的往返和一次
/// 真读一样贵。
///
/// 判据是**服务端见过几次 READ**,不是返回值。只断言「内容对」是恒绿的:
/// 多问一次 EOF 的实现同样把每个字节都读回来了。
///
/// 自证会变红:把 `read_all` 里 `expect == Some(out.len() as u64)` 那条提前
/// 收手的分支删掉 —— 带 hint 那一轮立刻变回 2 次。
#[tokio::test]
async fn knowing_the_size_up_front_saves_the_round_trip_that_only_asks_for_eof() {
    let want = payload(30_000, 251);
    let (addr, probe, _tree) =
        common::spawn_sftp_server(tree_with(vec![Node::file_with(b"app.conf", &want)])).await;
    let sftp = client(addr).await;
    let path = rp("/home/testuser/app.conf");

    // 不给 hint:一次读回全部,再多问一次才知道是 EOF。
    let (blind, t_blind) = sftp
        .read_all(&path, 1024 * 1024, None)
        .await
        .expect("read_all");
    assert_eq!(blind, want);
    assert_eq!(
        t_blind.reads, 2,
        "没有 hint 时必须多问一次 EOF,不然下面没得比"
    );
    let blind_reads = probe.lock().unwrap().paths_for("read").len();

    // 给 hint:读满就收手。
    let (hinted, t_hinted) = sftp
        .read_all(&path, 1024 * 1024, Some(want.len() as u64))
        .await
        .expect("read_all");
    assert_eq!(hinted, want, "省掉那次往返不能少读一个字节");
    assert_eq!(t_hinted.reads, 1, "知道大小之后一次 READ 就该够");
    let total_reads = probe.lock().unwrap().paths_for("read").len();
    assert_eq!(
        total_reads - blind_reads,
        1,
        "服务端侧也要少收一次 READ —— 只信客户端自报的计数,埋点错了看不出来"
    );
}

/// F214 的兜底:`expect` 会过期。列目录之后文件被改**大**了,提前收手就会
/// 静默截断 —— 判据写成 `>=` 而不是 `==` 时正是这样炸的(第一次读满就收手,
/// 后面的字节永远拿不到,而用户看到的是一份「打开成功」的残文件)。
#[tokio::test]
async fn a_stale_size_hint_never_truncates_because_it_must_match_exactly() {
    let actual = payload(300_000, 251);
    let (addr, _probe, _tree) =
        common::spawn_sftp_server(tree_with(vec![Node::file_with(b"grown.bin", &actual)])).await;
    let sftp = client(addr).await;

    // 列目录时它还只有 100 KB。
    let (got, _) = sftp
        .read_all(&rp("/home/testuser/grown.bin"), 1024 * 1024, Some(100_000))
        .await
        .expect("read_all");
    assert_eq!(got.len(), actual.len(), "hint 过期时必须照常读到 EOF");
    assert_eq!(got, actual);
}

/// F214:缓冲区必须够大,否则每次往返只捞回四分之一。
///
/// `File::poll_read` 取 `min(buf.remaining(), max_read_len)`,而 `max_read_len`
/// 默认是 `max_packet_len - 9` = 262135。缓冲区留在 64 KiB 的话,一个 300 KB
/// 的文件要 5 次串行 READ;给到 256 KiB 只要 2 次。
///
/// 自证会变红:把 `read_all` 里的 `256 * 1024` 改回 `64 * 1024`。
#[tokio::test]
async fn one_round_trip_hauls_a_whole_packet_not_a_quarter_of_one() {
    let want = payload(300_000, 251);
    let (addr, _probe, _tree) =
        common::spawn_sftp_server(tree_with(vec![Node::file_with(b"big.conf", &want)])).await;
    let sftp = client(addr).await;

    let (got, timing) = sftp
        .read_all(
            &rp("/home/testuser/big.conf"),
            1024 * 1024,
            Some(want.len() as u64),
        )
        .await
        .expect("read_all");
    assert_eq!(got, want);
    assert_eq!(
        timing.reads, 2,
        "300 KB 该是「读满一包 + 读完尾巴」两次往返;变多说明缓冲区又被调小了"
    );
}
