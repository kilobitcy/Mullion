//! SFTP 写操作的端到端测试(F54/F57):真握手 → 开 sftp subsystem → 改远端。
//! 服务端是同进程的可写假 SFTP(见 `common/sftp_server.rs`)。
//!
//! 判据一律是**服务端的树变成了什么样** + **探针里到底发了哪些请求**。
//! 只断言「客户端返回 Ok」是恒绿的:一个什么都不做的实现也返回 Ok。

mod common;

use std::sync::Arc;

use common::sftp_server::{exists, names_in, Node, Tree};
use mullion_ssh::config::{AuthMethod, SshConfig};
use mullion_ssh::known_hosts::{Fingerprint, HostKeyDecision, HostKeyFuture, HostKeyPolicy};
use mullion_ssh::session::{establish, SshConnection};
use mullion_ssh::sftp::{RemotePath, SftpClient, SftpError};

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
            Node::file("说明.md".as_bytes(), 34),
        ],
    );
    t.insert(
        b"/home/testuser/docs".to_vec(),
        vec![Node::file(b"inner.txt", 3)],
    );
    t
}

async fn conn_of(addr: std::net::SocketAddr) -> Arc<SshConnection> {
    Arc::new(
        establish(&cfg(addr), Arc::new(AcceptAll))
            .await
            .expect("connect"),
    )
}

async fn client(addr: std::net::SocketAddr) -> SftpClient {
    SftpClient::open(conn_of(addr).await)
        .await
        .expect("open sftp")
}

fn rp(s: &str) -> RemotePath {
    RemotePath::from_bytes(s.as_bytes().to_vec())
}

#[tokio::test]
async fn creating_a_directory_makes_it_appear_on_the_server() {
    let (addr, _probe, tree_h) = common::spawn_sftp_server(tree()).await;
    let sftp = client(addr).await;

    sftp.create_dir(&rp("/home/testuser/new"))
        .await
        .expect("mkdir");

    let t = tree_h.lock().unwrap();
    assert!(exists(&t, b"/home/testuser/new"), "新目录该出现在服务端");
}

/// 中文名逐字节往返 —— 服务端收到的必须是同一串 UTF-8 字节,
/// 不是被谁在中途 lossy 过一遍的近似串。
#[tokio::test]
async fn creating_a_chinese_directory_sends_the_exact_bytes() {
    let (addr, probe, tree_h) = common::spawn_sftp_server(tree()).await;
    let sftp = client(addr).await;

    sftp.create_dir(&rp("/home/testuser/新建文件夹"))
        .await
        .expect("mkdir 中文");

    let seen = probe.lock().unwrap().paths_for("mkdir");
    assert!(
        seen.iter()
            .any(|s| s.as_bytes() == "/home/testuser/新建文件夹".as_bytes()),
        "服务端收到的字节必须与请求的一致: {seen:?}"
    );
    assert!(exists(
        &tree_h.lock().unwrap(),
        "/home/testuser/新建文件夹".as_bytes()
    ));
}

#[tokio::test]
async fn renaming_moves_the_entry() {
    let (addr, _probe, tree_h) = common::spawn_sftp_server(tree()).await;
    let sftp = client(addr).await;

    sftp.rename(&rp("/home/testuser/a.txt"), &rp("/home/testuser/b.txt"))
        .await
        .expect("rename");

    let t = tree_h.lock().unwrap();
    assert!(!exists(&t, b"/home/testuser/a.txt"), "旧名该没了");
    assert!(exists(&t, b"/home/testuser/b.txt"), "新名该出现");
}

#[tokio::test]
async fn removing_a_file_takes_it_off_the_server() {
    let (addr, _probe, tree_h) = common::spawn_sftp_server(tree()).await;
    let sftp = client(addr).await;

    sftp.remove_file(&rp("/home/testuser/a.txt"))
        .await
        .expect("remove");

    assert!(!exists(&tree_h.lock().unwrap(), b"/home/testuser/a.txt"));
}

/// 空目录用 `remove_dir`。**非空目录必须失败** —— 服务端替我们把关,
/// 递归删除的正确性(先删干净里面)才有据可依。
#[tokio::test]
async fn removing_a_non_empty_directory_fails_but_an_empty_one_succeeds() {
    let (addr, _probe, tree_h) = common::spawn_sftp_server(tree()).await;
    let sftp = client(addr).await;

    let docs = rp("/home/testuser/docs");
    sftp.remove_dir(&docs)
        .await
        .expect_err("非空目录不该删得掉");

    sftp.remove_file(&rp("/home/testuser/docs/inner.txt"))
        .await
        .expect("先删里面的文件");
    sftp.remove_dir(&docs).await.expect("空了之后该删得掉");

    assert!(!exists(&tree_h.lock().unwrap(), b"/home/testuser/docs"));
}

#[tokio::test]
async fn changing_permissions_is_visible_in_a_later_listing() {
    let (addr, _probe, _tree) = common::spawn_sftp_server(tree()).await;
    let sftp = client(addr).await;

    sftp.set_permissions(&rp("/home/testuser/a.txt"), 0o600)
        .await
        .expect("setstat");

    let got = sftp.list_dir(&rp("/home/testuser")).await.expect("list");
    let a = got
        .iter()
        .find(|e| e.name.as_bytes() == b"a.txt")
        .expect("a.txt 还在");
    assert_eq!(a.mode & 0o777, 0o600, "改完权限,再列一次要看得见新值");
    // **只该动 permissions**。SFTP v3 的 attrs 是带 flags 位图的:顺手把
    // uid/gid/mtime 一起送出去,等于拿本地的猜测覆盖远端的真值 ——
    // 在共享目录上这会把文件的属主改掉,而界面上一点提示都没有。
    assert_eq!(a.uid, 1000, "属主不该被这次改权限带走");
    assert_eq!(a.gid, 1000, "属组不该被这次改权限带走");
    assert_eq!(a.mtime, 1_700_000_100, "修改时间不该被这次改权限带走");
}

/// `stat` 用 **lstat 语义**:一条指向目录的链接必须报成 `Symlink`,
/// 不是它目标的类型。跟随了的话,递归删除会把链接当目录走进去 ——
/// 那正是 D17 要挡的事故。
#[tokio::test]
async fn stat_reports_a_symlink_as_a_link_and_not_as_its_target() {
    let mut t = tree();
    t.get_mut(b"/home/testuser".as_slice())
        .unwrap()
        .push(Node::link(b"lnk", b"/home/testuser/docs"));
    let (addr, probe, _tree) = common::spawn_sftp_server(t).await;
    let sftp = client(addr).await;

    let e = sftp.stat(&rp("/home/testuser/lnk")).await.expect("stat");
    assert_eq!(
        e.kind,
        mullion_ssh::sftp::EntryKind::Symlink,
        "链接必须报成 Symlink"
    );
    assert_eq!(e.name.as_bytes(), b"lnk", "name 只该是最后一段");

    // 判据不止在返回值上:发出去的必须是 LSTAT 而不是 STAT。
    let p = probe.lock().unwrap();
    assert!(
        !p.paths_for("lstat").is_empty(),
        "该发 lstat: seen={:?}",
        p.seen
    );
    assert!(
        p.paths_for("stat").is_empty(),
        "不该发跟随链接的 stat: {:?}",
        p.paths_for("stat")
    );
}

/// D16 的核心不变量在写方向也成立:**发不出去的路径一个请求都不发**。
/// 只测「返回 Err」不够 —— 那对「先发了请求再失败」也成立。要连探针一起验。
#[tokio::test]
async fn a_non_operable_path_never_reaches_the_wire_for_any_write() {
    let (addr, probe, tree_h) = common::spawn_sftp_server(tree()).await;
    let sftp = client(addr).await;

    let bad = RemotePath::from_bytes(vec![b'/', 0xff, 0xfe, b'x']);
    let lossy = rp("/home/testuser/\u{fffd}\u{fffd}.txt");
    let ok = rp("/home/testuser/z");

    for p in [&bad, &lossy] {
        assert!(matches!(
            sftp.create_dir(p).await.expect_err("mkdir 不该发得出去"),
            SftpError::NonUtf8Name
        ));
        assert!(matches!(
            sftp.remove_file(p).await.expect_err("remove 不该发得出去"),
            SftpError::NonUtf8Name
        ));
        assert!(matches!(
            sftp.remove_dir(p).await.expect_err("rmdir 不该发得出去"),
            SftpError::NonUtf8Name
        ));
        assert!(matches!(
            sftp.set_permissions(p, 0o644)
                .await
                .expect_err("setstat 不该发得出去"),
            SftpError::NonUtf8Name
        ));
        assert!(matches!(
            sftp.stat(p).await.expect_err("stat 不该发得出去"),
            SftpError::NonUtf8Name
        ));
        // rename 两个参数都要挡:任一端发不出去就整条不发 —— 只挡一端的话,
        // 另一端会被拿去跟一个 lossy 串配对,把文件改成谁也打不开的名字。
        assert!(matches!(
            sftp.rename(p, &ok)
                .await
                .expect_err("rename 源不该发得出去"),
            SftpError::NonUtf8Name
        ));
        assert!(matches!(
            sftp.rename(&ok, p)
                .await
                .expect_err("rename 目标不该发得出去"),
            SftpError::NonUtf8Name
        ));
    }

    let pr = probe.lock().unwrap();
    for op in ["mkdir", "remove", "rmdir", "setstat", "rename", "lstat"] {
        assert!(
            pr.paths_for(op).is_empty(),
            "被挡下的路径不该产生任何 {op} 请求: {:?}",
            pr.paths_for(op)
        );
    }
    // 反面自检:服务端的树一点没变 —— 否则上面那堆断言可能只是「探针没记」。
    assert_eq!(
        names_in(&tree_h.lock().unwrap(), b"/home/testuser").len(),
        3,
        "一条都不该被改动"
    );
}

// ---- 递归删除(F57 / D17)-------------------------------------------------

use mullion_ssh::remove_tree::{remove_tree, RemoveReport};

/// `box` 里有文件、子目录、以及一条**指向 `victim` 的符号链接**。
/// `victim` 里那个 `precious.txt` 是「删除跟随了链接」的报警器。
fn nested_tree() -> Tree {
    let mut t = Tree::new();
    t.insert(
        b"/home/testuser".to_vec(),
        vec![Node::dir(b"box"), Node::dir(b"victim")],
    );
    t.insert(
        b"/home/testuser/box".to_vec(),
        vec![
            Node::file(b"f1", 1),
            Node::dir(b"sub"),
            Node::link(b"lnk", b"/home/testuser/victim"),
        ],
    );
    t.insert(
        b"/home/testuser/box/sub".to_vec(),
        vec![Node::file(b"deep.txt", 2)],
    );
    t.insert(
        b"/home/testuser/victim".to_vec(),
        vec![Node::file(b"precious.txt", 9)],
    );
    t
}

/// 快路径:exec 可用时走 `rm -rf`,**一条 SFTP 删除请求都不该发**。
#[tokio::test]
async fn a_recursive_delete_uses_the_exec_fast_path_when_it_is_allowed() {
    let (addr, probe, tree_h) = common::spawn_sftp_server(nested_tree()).await;
    let conn = conn_of(addr).await;
    let sftp = SftpClient::open(conn.clone()).await.expect("open sftp");

    let report = remove_tree(&sftp, &conn, &rp("/home/testuser/box"))
        .await
        .expect("递归删除");
    assert_eq!(report, RemoveReport::Exec, "exec 可用时该走快路径");

    assert!(
        !exists(&tree_h.lock().unwrap(), b"/home/testuser/box"),
        "整棵子树该没了"
    );
    let p = probe.lock().unwrap();
    assert!(
        p.paths_for("remove").is_empty() && p.paths_for("rmdir").is_empty(),
        "走了 exec 就不该再发逐文件的 SFTP 删除: remove={:?} rmdir={:?}",
        p.paths_for("remove"),
        p.paths_for("rmdir")
    );
}

/// F57 的核心:**exec 被拒时回退到 SFTP 逐文件递归**,而不是报错收场。
/// sftp-only 账号(`ForceCommand internal-sftp`)就是这种环境。
#[tokio::test]
async fn a_recursive_delete_falls_back_to_sftp_when_exec_is_refused() {
    let (addr, probe, tree_h) = common::spawn_sftp_server_without_exec(nested_tree()).await;
    let conn = conn_of(addr).await;
    let sftp = SftpClient::open(conn.clone()).await.expect("open sftp");

    let report = remove_tree(&sftp, &conn, &rp("/home/testuser/box"))
        .await
        .expect("递归删除该回退成功");
    assert_eq!(report, RemoveReport::Sftp, "exec 被拒时该回退");

    let t = tree_h.lock().unwrap();
    assert!(!exists(&t, b"/home/testuser/box"), "回退路径也要真的删干净");
    assert!(!exists(&t, b"/home/testuser/box/sub"), "子目录也要删掉");
    drop(t);
    let p = probe.lock().unwrap();
    assert!(!p.paths_for("rmdir").is_empty(), "回退路径必然发过 rmdir");
}

/// D17 最要命的一条:**删除绝不跟随符号链接**。搞错了就是把远端整个
/// 目标目录删了 —— 这条测试的存在本身就是它的理由。
///
/// 两条路径都要验:回退的 SFTP 递归,与 exec 的 `rm -rf`。
#[tokio::test]
async fn a_recursive_delete_never_follows_a_symlink_into_the_target_directory() {
    for without_exec in [true, false] {
        let (addr, probe, tree_h) = if without_exec {
            common::spawn_sftp_server_without_exec(nested_tree()).await
        } else {
            common::spawn_sftp_server(nested_tree()).await
        };
        let conn = conn_of(addr).await;
        let sftp = SftpClient::open(conn.clone()).await.expect("open sftp");

        remove_tree(&sftp, &conn, &rp("/home/testuser/box"))
            .await
            .expect("递归删除");

        let t = tree_h.lock().unwrap();
        assert!(
            exists(&t, b"/home/testuser/victim/precious.txt"),
            "链接指向的目录被跟进去删了(without_exec={without_exec}) —— 这是 D17 要挡的那类事故"
        );
        assert!(exists(&t, b"/home/testuser/victim"));
        drop(t);

        if without_exec {
            let p = probe.lock().unwrap();
            assert!(
                !p.paths_for("opendir").iter().any(|s| s.contains("victim")),
                "一次都不该 opendir 到链接目标里去: {:?}",
                p.paths_for("opendir")
            );
        }
    }
}

/// 转义:名字里带空格/引号/换行/`$`/反引号的目录也要删得掉。
/// 假服务端会把命令行**解回原始字节**再动手 —— 转义错了它就认不出这条
/// 命令,树上那个目录会原封不动地留着。
#[tokio::test]
async fn the_exec_fast_path_quotes_nasty_names_correctly() {
    let nasty: &[u8] = b"it's a $(dir) `x` \n name";
    let mut t = Tree::new();
    t.insert(b"/home/testuser".to_vec(), vec![Node::dir(nasty)]);
    let mut key = b"/home/testuser/".to_vec();
    key.extend_from_slice(nasty);
    t.insert(key.clone(), vec![Node::file(b"inner", 1)]);

    let (addr, probe, tree_h) = common::spawn_sftp_server(t).await;
    let conn = conn_of(addr).await;
    let sftp = SftpClient::open(conn.clone()).await.expect("open sftp");

    remove_tree(&sftp, &conn, &RemotePath::from_bytes(key.clone()))
        .await
        .expect("删带怪名字的目录");

    assert!(
        !exists(&tree_h.lock().unwrap(), &key),
        "带怪名字的目录该真的没了 —— 还在就说明命令行被 shell 拆错了"
    );
    let execs = probe.lock().unwrap().execs.clone();
    assert_eq!(execs.len(), 1, "该只发一条命令");
    assert!(
        execs[0].starts_with(b"rm -rf -- '"),
        "命令必须是 rm -rf -- 加单引号包住的路径: {}",
        String::from_utf8_lossy(&execs[0])
    );
}

/// D16 在递归删除上同样成立:发不出去的路径**一条请求都不发**,
/// 也不许被塞进 `rm -rf` 的命令行(那等于拿一串替换字符去删东西)。
#[tokio::test]
async fn a_non_operable_path_is_refused_by_recursive_delete_without_any_request() {
    let (addr, probe, _tree) = common::spawn_sftp_server(nested_tree()).await;
    let conn = conn_of(addr).await;
    let sftp = SftpClient::open(conn.clone()).await.expect("open sftp");

    let bad = rp("/home/testuser/\u{fffd}\u{fffd}");
    let err = remove_tree(&sftp, &conn, &bad)
        .await
        .expect_err("不该发得出去");
    assert!(matches!(err, SftpError::NonUtf8Name));

    let p = probe.lock().unwrap();
    assert!(p.execs.is_empty(), "一条 exec 都不该发: {:?}", p.execs);
    assert!(p.paths_for("remove").is_empty() && p.paths_for("rmdir").is_empty());
}

// ---- 新建空文件(F219)------------------------------------------------------

/// `tree()` 里 `a.txt` 是 `Node::file`,`data` 字段是空的(`size` 只是标称值)
/// —— 检验「EXCLUDE 撞上已存在不许改动内容」时需要一个**真有内容**的既存
/// 文件,否则「内容没被动过」这条断言在改坏之前就已经是真的,测试就是恒绿的。
fn tree_with_real_content() -> Tree {
    let mut t = tree();
    let entries = t.get_mut(b"/home/testuser".as_slice()).unwrap();
    if let Some(n) = entries.iter_mut().find(|n| n.name == b"a.txt") {
        *n = Node::file_with(b"a.txt", b"hello, this is real content");
    }
    t
}

/// F219:新建一个空文件 —— 服务端上真的多出这一条,且大小为 0。
///
/// 判据是**服务端的树**,不是「客户端返回了 Ok」:后者恒绿,一个什么都
/// 不做的实现照样通过。
#[tokio::test]
async fn creating_a_file_makes_an_empty_one_appear_on_the_server() {
    let (addr, _probe, tree_h) = common::spawn_sftp_server(tree()).await;
    let sftp = client(addr).await;
    sftp.create_file(&rp("/home/testuser/notes.txt"))
        .await
        .expect("新建文件该成功");
    let t = tree_h.lock().unwrap();
    assert!(
        exists(&t, b"/home/testuser/notes.txt"),
        "服务端上没有这个文件,实际:{:?}",
        names_in(&t, b"/home/testuser")
    );
}

/// F219 的核心闸门:**撞上已存在必须失败**,不能把别人的文件截断成 0 字节。
///
/// 自证会变红:把 `create_file` 里的 `OpenFlags::EXCLUDE` 去掉。
#[tokio::test]
async fn creating_a_file_that_already_exists_fails_instead_of_truncating_it() {
    let (addr, _probe, tree_h) = common::spawn_sftp_server(tree_with_real_content()).await;
    let sftp = client(addr).await;
    let before = {
        let t = tree_h.lock().unwrap();
        t.get(&b"/home/testuser".to_vec())
            .expect("父目录该在")
            .iter()
            .find(|n| n.name == b"a.txt")
            .expect("a.txt 该在")
            .data
            .clone()
    };
    assert!(!before.is_empty(), "前提:这个文件本来有内容");

    let err = sftp.create_file(&rp("/home/testuser/a.txt")).await;
    assert!(err.is_err(), "撞上已存在的文件该失败,而不是悄悄覆盖");

    let t = tree_h.lock().unwrap();
    let after = t
        .get(&b"/home/testuser".to_vec())
        .expect("父目录该在")
        .iter()
        .find(|n| n.name == b"a.txt")
        .expect("a.txt 该在")
        .data
        .clone();
    assert_eq!(after, before, "文件内容被动过了 —— EXCLUDE 没生效");
}
