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
    let data = t
        .get(b"/home/testuser".as_slice())
        .expect("父目录该在")
        .iter()
        .find(|n| n.name == b"notes.txt")
        .expect("notes.txt 该在")
        .data
        .clone();
    assert!(data.is_empty(), "新建的文件不该带内容,实际:{data:?}");
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

/// F220/B2:`common::parse_copy_or_move` 要认得出**真实 `shell_quote`** 拼出来
/// 的命令行 —— 这是这个假服务端唯一有价值的地方。B3 的 `try_exec` 会拼
/// `cp -a[f] -- <quoted src> <quoted dst>` / `mv [-f] -- …`,多对用 ` && ` 串。
/// 这里不等 B3 落地,直接照它的拼法自己拼一遍,拿真实 `shell_quote` 喂给
/// 解析器 —— 不这样测的话,解析器认不出真实输出这件事只会在 B3 落地后
/// 表现为「静默走了 SFTP 回退,exec 快路径其实一次都没被验过」,测试
/// 还是绿的。
#[test]
fn parse_copy_or_move_understands_what_shell_quote_actually_produces() {
    use mullion_ssh::exec::shell_quote;

    fn cmd_for(head: &[u8], pairs: &[(&[u8], &[u8])]) -> Vec<u8> {
        let mut cmd = Vec::new();
        for (from, to) in pairs {
            if !cmd.is_empty() {
                cmd.extend_from_slice(b" && ");
            }
            cmd.extend_from_slice(head);
            cmd.extend_from_slice(&shell_quote(from));
            cmd.push(b' ');
            cmd.extend_from_slice(&shell_quote(to));
        }
        cmd
    }

    // 四种 head:cp(非覆盖/覆盖)、mv(非覆盖/覆盖)。
    let cases: &[(&[u8], bool)] = &[
        (b"cp -a -- ", false),
        (b"cp -af -- ", false),
        (b"mv -- ", true),
        (b"mv -f -- ", true),
    ];
    for (head, expect_move) in cases {
        let cmd = cmd_for(head, &[(b"/home/testuser/box", b"/home/testuser/box-copy")]);
        let (is_move, pairs) = common::parse_copy_or_move(&cmd)
            .unwrap_or_else(|| panic!("解不出 head={:?} 拼出来的命令", head));
        assert_eq!(is_move, *expect_move, "head={:?} 的移动标志判错了", head);
        assert_eq!(
            pairs,
            vec![(
                b"/home/testuser/box".to_vec(),
                b"/home/testuser/box-copy".to_vec()
            )],
            "head={:?} 解出来的路径对不上",
            head
        );
    }

    // 多对用 ` && ` 串:B3 一次粘贴多个条目走的是这条形状。
    let multi = cmd_for(
        b"cp -a -- ",
        &[
            (b"/home/testuser/a", b"/home/testuser/dst/a"),
            (b"/home/testuser/b", b"/home/testuser/dst/b"),
        ],
    );
    let (is_move, pairs) = common::parse_copy_or_move(&multi).expect("多对该能解出来");
    assert!(!is_move, "cp 不该被认成移动");
    assert_eq!(
        pairs,
        vec![
            (
                b"/home/testuser/a".to_vec(),
                b"/home/testuser/dst/a".to_vec()
            ),
            (
                b"/home/testuser/b".to_vec(),
                b"/home/testuser/dst/b".to_vec()
            ),
        ],
        "多对的顺序或路径解错了"
    );

    // 脏名字(单引号 + `$` + 空格):`shell_quote` 是唯一的转义来源,
    // 解析器要能把它原样解回来。
    let nasty: &[u8] = b"it's a $(x) file";
    let dirty = cmd_for(b"mv -- ", &[(nasty, b"/home/testuser/dst")]);
    let (is_move, pairs) = common::parse_copy_or_move(&dirty).expect("脏名字也该解出来");
    assert!(is_move);
    assert_eq!(
        pairs,
        vec![(nasty.to_vec(), b"/home/testuser/dst".to_vec())]
    );
}

/// F220/B2:`common::copy_recursively` 要镜像 `remove_recursively` 的行为 ——
/// 拷整棵目录树,但**遇到符号链接原样复制成叶子,不跟进去**(与 B3 的
/// `copy_one` 承诺的不变量一致)。不等 B3 落地就直接对内存树验这一条,
/// 免得「跟进符号链接」这个错要等到 B3 的集成测试才可能被撞见。
#[test]
fn copy_recursively_mirrors_the_tree_but_never_follows_a_symlink() {
    let mut t = nested_tree();

    common::copy_recursively(&mut t, b"/home/testuser/box", b"/home/testuser/box-copy");

    // 目标目录本身、以及它的直接孩子(文件/子目录/链接)都该出现。
    assert!(exists(&t, b"/home/testuser/box-copy"), "目标目录没建出来");
    let copied_names: std::collections::BTreeSet<_> = names_in(&t, b"/home/testuser/box-copy")
        .into_iter()
        .collect();
    assert_eq!(
        copied_names,
        [b"f1".to_vec(), b"sub".to_vec(), b"lnk".to_vec()]
            .into_iter()
            .collect(),
        "拷贝出来的直接孩子名字不对"
    );

    // 子目录要递归拷:sub/deep.txt 也该在。
    assert!(
        exists(&t, b"/home/testuser/box-copy/sub"),
        "子目录没有递归拷"
    );
    assert!(
        names_in(&t, b"/home/testuser/box-copy/sub").contains(&b"deep.txt".to_vec()),
        "子目录里的文件没跟着拷"
    );

    // 核心不变量:lnk 是符号链接,复制后必须仍是叶子 —— 不能在树上
    // 长出 `/home/testuser/box-copy/lnk` 这个目录键(那就是跟进去了)。
    assert!(
        !t.contains_key(b"/home/testuser/box-copy/lnk".as_slice()),
        "符号链接被当成目录跟进去了 —— 整个链接目标被复制了一遍"
    );
    let lnk = t
        .get(b"/home/testuser/box-copy".as_slice())
        .unwrap()
        .iter()
        .find(|n| n.name == b"lnk")
        .expect("lnk 节点该在");
    assert_eq!(
        lnk.kind,
        common::sftp_server::NodeKind::Symlink(b"/home/testuser/victim".to_vec()),
        "lnk 复制后该仍是指向原目标的符号链接"
    );

    // 复制不该动源:box 和它的孩子都该原封不动还在。
    assert!(exists(&t, b"/home/testuser/box"), "复制不该动源目录");
    assert!(
        names_in(&t, b"/home/testuser/box").contains(&b"lnk".to_vec()),
        "源目录的链接不该被复制操作带走"
    );
}

// ---- 远端内复制/移动(F220/B3)---------------------------------------------

use mullion_ssh::copy_tree::{transfer_into, CopyMode, TransferReport};

/// F220 快路径:exec 可用时走一条 `cp -a`,**一条 SFTP 写请求都不发**。
#[tokio::test]
async fn a_paste_uses_the_exec_fast_path_when_it_is_allowed() {
    let (addr, probe, tree_h) = common::spawn_sftp_server(nested_tree()).await;
    let (conn, sftp) = (conn_of(addr).await, client(addr).await);
    let pairs = vec![(rp("/home/testuser/box"), rp("/home/testuser/box-copy"))];
    let report = transfer_into(&sftp, &conn, &pairs, CopyMode::Copy, false)
        .await
        .expect("复制该成功");
    assert_eq!(report, TransferReport::Exec);

    let t = tree_h.lock().unwrap();
    assert!(exists(&t, b"/home/testuser/box-copy"), "目标没建出来");
    assert!(exists(&t, b"/home/testuser/box"), "复制不该动源");
    let p = probe.lock().unwrap();
    assert!(
        p.paths_for("write").is_empty() && p.paths_for("mkdir").is_empty(),
        "走了 exec 就不该再发逐文件的 SFTP 写请求:{:?}",
        p.seen
    );
}

/// F220 的核心:**exec 被拒时回退到 SFTP 逐文件递归**,而不是报错收场
/// (sftp-only 账号上功能不能残缺)。
#[tokio::test]
async fn a_paste_falls_back_to_sftp_when_exec_is_refused() {
    let (addr, _probe, tree_h) = common::spawn_sftp_server_without_exec(nested_tree()).await;
    let (conn, sftp) = (conn_of(addr).await, client(addr).await);
    let pairs = vec![(rp("/home/testuser/box"), rp("/home/testuser/box-copy"))];
    let report = transfer_into(&sftp, &conn, &pairs, CopyMode::Copy, false)
        .await
        .expect("回退该成功");
    assert_eq!(report, TransferReport::Sftp);
    let t = tree_h.lock().unwrap();
    assert!(exists(&t, b"/home/testuser/box-copy"), "回退没把树建出来");
}

/// F220:文件内容要真的一样。只看「路径存在」是恒绿的 —— 一个建空文件的
/// 实现照样通过。`nested_tree()` 里的文件节点都是 `Node::file`(标称
/// 大小、无真实内容),这里现改一个真有内容的进去。
#[tokio::test]
async fn the_sftp_fallback_copies_the_bytes_not_just_the_names() {
    let mut t0 = nested_tree();
    let sub = t0
        .get_mut(b"/home/testuser/box/sub".as_slice())
        .expect("nested_tree() 该有 box/sub");
    let deep = sub
        .iter_mut()
        .find(|n| n.name == b"deep.txt")
        .expect("nested_tree() 的 box/sub 该有 deep.txt");
    *deep = Node::file_with(b"deep.txt", b"deep content, not just a name");

    let (addr, _probe, tree_h) = common::spawn_sftp_server_without_exec(t0).await;
    let (conn, sftp) = (conn_of(addr).await, client(addr).await);
    let src = rp("/home/testuser/box/sub/deep.txt");
    let dst = rp("/home/testuser/deep-copy.txt");
    let before = {
        let t = tree_h.lock().unwrap();
        t.get(b"/home/testuser/box/sub".as_slice())
            .expect("源目录该在")
            .iter()
            .find(|n| n.name == b"deep.txt")
            .expect("源文件该在")
            .data
            .clone()
    };
    assert!(!before.is_empty(), "前提:源文件有内容");
    transfer_into(&sftp, &conn, &[(src, dst)], CopyMode::Copy, false)
        .await
        .expect("复制该成功");
    let t = tree_h.lock().unwrap();
    let after = t
        .get(b"/home/testuser".as_slice())
        .expect("目标目录该在")
        .iter()
        .find(|n| n.name == b"deep-copy.txt")
        .expect("目标文件该在")
        .data
        .clone();
    assert_eq!(after, before, "拷过去的字节不一样");
}

/// F220:**绝不跟随符号链接** —— 跟进去等于把链接指向的整个目录复制一遍。
/// 两条路都要验(同 F57 的那条守护)。
///
/// 只断言「没有把链接当目录跟进去」是不够的:「跟进去了」和「压根什么都
/// 没建」都能让那条断言通过(B3 复核挖出的一个恒绿)。所以这里还要断言
/// 「确实建出来一条符号链接,且目标与源链接一致」——一个把 `Symlink` 分支
/// 改成空操作的实现会在这一条上真的红。
#[tokio::test]
async fn a_paste_never_follows_a_symlink_on_either_path() {
    for without_exec in [true, false] {
        let (addr, _probe, tree_h) = if without_exec {
            common::spawn_sftp_server_without_exec(nested_tree()).await
        } else {
            common::spawn_sftp_server(nested_tree()).await
        };
        let (conn, sftp) = (conn_of(addr).await, client(addr).await);
        // nested_tree() 里 box 下那条指向 victim 的链接叫 `lnk`(不是 `link`)。
        let link = rp("/home/testuser/box/lnk");
        let report = transfer_into(
            &sftp,
            &conn,
            &[(link, rp("/home/testuser/link-copy"))],
            CopyMode::Copy,
            false,
        )
        .await
        .expect("复制链接本身该成功");
        assert_eq!(
            report,
            if without_exec {
                TransferReport::Sftp
            } else {
                TransferReport::Exec
            },
            "without_exec={without_exec} 时走的路不对,后面两条分支的判据就对不上号了"
        );

        let t = tree_h.lock().unwrap();
        assert!(
            !t.contains_key(&b"/home/testuser/link-copy".to_vec()),
            "链接被当目录跟进去了(without_exec={without_exec}) —— 整个目标目录被复制了一遍"
        );
        let copied = t
            .get(b"/home/testuser".as_slice())
            .expect("父目录该在")
            .iter()
            .find(|n| n.name == b"link-copy")
            .unwrap_or_else(|| {
                panic!("目标该真的建出来,而不是什么都没做(without_exec={without_exec})")
            });
        assert_eq!(
            copied.kind,
            common::sftp_server::NodeKind::Symlink(b"/home/testuser/victim".to_vec()),
            "复制出来的该是一条指向原目标的符号链接(without_exec={without_exec})"
        );
    }
}

/// F220:剪切 = 移动。源没了、目标有了。
#[tokio::test]
async fn a_cut_paste_moves_the_entry_instead_of_copying_it() {
    for without_exec in [true, false] {
        let (addr, _probe, tree_h) = if without_exec {
            common::spawn_sftp_server_without_exec(nested_tree()).await
        } else {
            common::spawn_sftp_server(nested_tree()).await
        };
        let (conn, sftp) = (conn_of(addr).await, client(addr).await);
        let report = transfer_into(
            &sftp,
            &conn,
            &[(rp("/home/testuser/box"), rp("/home/testuser/moved"))],
            CopyMode::Move,
            false,
        )
        .await
        .expect("移动该成功");
        if !without_exec {
            assert_eq!(report, TransferReport::Exec, "exec 可用时该走快路径");
        }
        let t = tree_h.lock().unwrap();
        assert!(exists(&t, b"/home/testuser/moved"), "目标没出现");
        assert!(
            !exists(&t, b"/home/testuser/box"),
            "源还在(without_exec={without_exec}) —— 剪切变成了复制"
        );
    }
}

/// F220:脏名字(空格 / 单引号 / `$`)必须原样打到那条路径上 ——
/// 引号漏一个就是远端任意命令执行。这条走的是 exec 快路径:显式断言
/// `report == Exec`,否则「引号错了 → 假服务端解析失败 → 回 127 → 静默
/// 退到 SFTP」也能让这条测试通过,而 exec 路径的转义从没被真正验过。
#[tokio::test]
async fn the_paste_fast_path_quotes_nasty_names_correctly() {
    let mut t0 = nested_tree();
    let key = b"/home/testuser/it's a $(x) file".to_vec();
    t0.entry(b"/home/testuser".to_vec())
        .or_default()
        .push(common::sftp_server::Node::file(b"it's a $(x) file", 2));
    let (addr, probe, tree_h) = common::spawn_sftp_server(t0).await;
    let (conn, sftp) = (conn_of(addr).await, client(addr).await);
    let report = transfer_into(
        &sftp,
        &conn,
        &[(RemotePath::from_bytes(key), rp("/home/testuser/copied"))],
        CopyMode::Copy,
        false,
    )
    .await
    .expect("脏名字也该复制成功");
    assert_eq!(report, TransferReport::Exec, "这条该走 exec 快路径");
    let t = tree_h.lock().unwrap();
    assert!(exists(&t, b"/home/testuser/copied"), "脏名字的引号处理错了");
    let p = probe.lock().unwrap();
    assert!(
        p.paths_for("write").is_empty() && p.paths_for("mkdir").is_empty(),
        "断言走了 exec 就不该有 SFTP 写请求,否则上面 report==Exec 的判据本身就没验实:{:?}",
        p.seen
    );
}

/// F220/B3 缺陷 2:`overwrite == true` 时,`cp -a`/`mv` 撞上一个**已存在的
/// 非空目录**要整个替换掉它,而不是把源嵌进目标目录里
/// (`cp -a src dst` 在 `dst` 已存在且是目录时的默认行为是拷成
/// `dst/basename(src)`,`-f` 救不了这一种)。exec 快路径与 SFTP 回退
/// 两条路的覆盖语义必须一致 —— 都验。
#[tokio::test]
async fn overwriting_an_existing_directory_replaces_it_instead_of_nesting_into_it() {
    for without_exec in [true, false] {
        let mut t0 = nested_tree();
        // 目标已存在,且里面有一个源里没有的文件:覆盖后这个文件必须消失,
        // 不然就是「嵌进去」而不是「替换」。
        t0.entry(b"/home/testuser".to_vec())
            .or_default()
            .push(Node::dir(b"box-copy"));
        t0.insert(
            b"/home/testuser/box-copy".to_vec(),
            vec![Node::file(b"stale.txt", 1)],
        );

        let (addr, probe, tree_h) = if without_exec {
            common::spawn_sftp_server_without_exec(t0).await
        } else {
            common::spawn_sftp_server(t0).await
        };
        let (conn, sftp) = (conn_of(addr).await, client(addr).await);

        let report = transfer_into(
            &sftp,
            &conn,
            &[(rp("/home/testuser/box"), rp("/home/testuser/box-copy"))],
            CopyMode::Copy,
            true,
        )
        .await
        .expect("覆盖复制该成功");
        assert_eq!(
            report,
            if without_exec {
                TransferReport::Sftp
            } else {
                TransferReport::Exec
            },
            "without_exec={without_exec} 时走的路不对"
        );

        let t = tree_h.lock().unwrap();
        let names: std::collections::BTreeSet<_> = names_in(&t, b"/home/testuser/box-copy")
            .into_iter()
            .collect();
        assert_eq!(
            names,
            [b"f1".to_vec(), b"sub".to_vec(), b"lnk".to_vec()]
                .into_iter()
                .collect(),
            "覆盖后目标目录该只剩源的内容(without_exec={without_exec}):实际 {names:?}"
        );
        assert!(
            !names.contains(b"stale.txt".as_slice()),
            "旧内容没被清掉,说明是嵌进去了而不是替换(without_exec={without_exec})"
        );
        drop(t);
        if !without_exec {
            let p = probe.lock().unwrap();
            assert!(
                p.paths_for("write").is_empty() && p.paths_for("mkdir").is_empty(),
                "覆盖走了 exec 就不该再发 SFTP 写请求:{:?}",
                p.seen
            );
            // 缺口 1:上面那堆断言只看服务端的树变成了什么样 —— 假服务端的
            // `parse_overwriting_copy_or_move` 分支自己会先 `remove_recursively`
            // 清空目标再拷,所以哪怕 `try_exec` 发出去的是裸 `cp -af -- `(没有
            // `rm -rf --` 前缀,落到假服务端会走 `parse_copy_or_move` 那条
            // 「非覆盖」解析分支,而它同样会 `tree.insert(to, Vec::new())`
            // 清空重建),最终树状态也会长得一样,树状态断言测不出命令串本身
            // 错没错。这里直接核对发出去的命令串。
            assert_eq!(p.execs.len(), 1, "覆盖粘贴该只发一条命令");
            let cmd = &p.execs[0];
            assert!(
                cmd.starts_with(b"rm -rf -- "),
                "覆盖命令必须先 rm -rf 清场,不能是裸 cp -af/mv -f(cp -a 撞上已存在的\
                 目录是嵌进去,不是替换): {}",
                String::from_utf8_lossy(cmd)
            );
            let needle: &[u8] = b" && cp -a -- ";
            assert!(
                cmd.windows(needle.len()).any(|w| w == needle),
                "覆盖命令必须是 rm -rf -- <dst> && cp -a -- <src> <dst> 这个形状: {}",
                String::from_utf8_lossy(cmd)
            );
        }
    }
}

/// F220/B3 缺口 3(复核靶向变异追加):上面那条覆盖测试只走了
/// `CopyMode::Copy`。全套测试里 `CopyMode::Move` 只出现两处
/// (`a_cut_paste_moves_the_entry_instead_of_copying_it` 和
/// `a_cut_falls_back_to_copy_and_delete_when_rename_reports_exdev`),两处
/// 传的都是 `overwrite=false` —— `Move + overwrite=true` 这个组合此前零
/// 覆盖:实测把 `try_exec` 的条件改成
/// `if overwrite && matches!(mode, CopyMode::Copy)`(让「漏加 rm -rf 前缀」
/// 只在 Move 分支退化)→ 25/25 全绿,没有任何测试变红。这条组合有明确的
/// 产品路径:B7 剪切一批文件、粘贴到有同名条目的目录、批量冲突框里选覆盖,
/// 走的正是 `Move + overwrite=true`。
///
/// 结构照抄 `overwriting_an_existing_directory_replaces_it_instead_of_nesting_into_it`,
/// 只把 `CopyMode::Copy` 换成 `CopyMode::Move`:判据同构 ——
/// 树状态是「替换」不是「嵌套」,exec 命令串必须 `rm -rf -- ` 开头 + 含
/// ` && mv -- `,外加剪切语义本身(源没了)。
#[tokio::test]
async fn overwriting_an_existing_directory_with_a_cut_replaces_it_instead_of_nesting_into_it() {
    for without_exec in [true, false] {
        let mut t0 = nested_tree();
        // 目标已存在,且里面有一个源里没有的文件:覆盖后这个文件必须消失,
        // 不然就是「嵌进去」而不是「替换」。
        t0.entry(b"/home/testuser".to_vec())
            .or_default()
            .push(Node::dir(b"box-copy"));
        t0.insert(
            b"/home/testuser/box-copy".to_vec(),
            vec![Node::file(b"stale.txt", 1)],
        );

        let (addr, probe, tree_h) = if without_exec {
            common::spawn_sftp_server_without_exec(t0).await
        } else {
            common::spawn_sftp_server(t0).await
        };
        let (conn, sftp) = (conn_of(addr).await, client(addr).await);

        let report = transfer_into(
            &sftp,
            &conn,
            &[(rp("/home/testuser/box"), rp("/home/testuser/box-copy"))],
            CopyMode::Move,
            true,
        )
        .await
        .expect("覆盖剪切该成功");
        assert_eq!(
            report,
            if without_exec {
                TransferReport::Sftp
            } else {
                TransferReport::Exec
            },
            "without_exec={without_exec} 时走的路不对"
        );

        let t = tree_h.lock().unwrap();
        assert!(
            !exists(&t, b"/home/testuser/box"),
            "源还在(without_exec={without_exec}) —— 剪切变成了复制"
        );
        let names: std::collections::BTreeSet<_> = names_in(&t, b"/home/testuser/box-copy")
            .into_iter()
            .collect();
        assert_eq!(
            names,
            [b"f1".to_vec(), b"sub".to_vec(), b"lnk".to_vec()]
                .into_iter()
                .collect(),
            "覆盖后目标目录该只剩源的内容(without_exec={without_exec}):实际 {names:?}"
        );
        assert!(
            !names.contains(b"stale.txt".as_slice()),
            "旧内容没被清掉,说明是嵌进去了而不是替换(without_exec={without_exec})"
        );
        drop(t);
        if !without_exec {
            let p = probe.lock().unwrap();
            assert!(
                p.paths_for("write").is_empty() && p.paths_for("mkdir").is_empty(),
                "覆盖走了 exec 就不该再发 SFTP 写请求:{:?}",
                p.seen
            );
            assert_eq!(p.execs.len(), 1, "覆盖剪切该只发一条命令");
            let cmd = &p.execs[0];
            assert!(
                cmd.starts_with(b"rm -rf -- "),
                "覆盖命令必须先 rm -rf 清场,不能是裸 mv -f(mv 撞上已存在的目录是嵌进去,\
                 不是替换): {}",
                String::from_utf8_lossy(cmd)
            );
            let needle: &[u8] = b" && mv -- ";
            assert!(
                cmd.windows(needle.len()).any(|w| w == needle),
                "覆盖命令必须是 rm -rf -- <dst> && mv -- <src> <dst> 这个形状: {}",
                String::from_utf8_lossy(cmd)
            );
        }
    }
}

/// F220/B3 缺口 2:SFTP 回退路径里「`rename` 失败(EXDEV)→ 拷贝 + 删源」
/// 这条分支。真实 sshd 上跨设备重命名会失败,`transfer_into` 那时才退成
/// 「`copy_one` 之后 `remove_tree(from)`」——假服务端的 `rename` 平时从不
/// 失败,这条分支原本零覆盖。用 `spawn_sftp_server_without_exec_and_rename`
/// (`reject_rename` 开关,仿 `allow_exec` 的写法)逼 `sftp.rename` 报错,
/// 断言剪切仍然成功,且源真的没了、目标真的建出来了。
#[tokio::test]
async fn a_cut_falls_back_to_copy_and_delete_when_rename_reports_exdev() {
    let (addr, _probe, tree_h) =
        common::spawn_sftp_server_without_exec_and_rename(nested_tree()).await;
    let (conn, sftp) = (conn_of(addr).await, client(addr).await);

    let report = transfer_into(
        &sftp,
        &conn,
        &[(rp("/home/testuser/box"), rp("/home/testuser/moved"))],
        CopyMode::Move,
        false,
    )
    .await
    .expect("rename 失败时该退成拷贝+删源,而不是直接报错收场");
    assert_eq!(report, TransferReport::Sftp, "exec 被拒时该走 SFTP 回退");

    let t = tree_h.lock().unwrap();
    assert!(
        exists(&t, b"/home/testuser/moved"),
        "目标没出现 —— EXDEV 回退里的拷贝那一步没做"
    );
    assert!(
        !exists(&t, b"/home/testuser/box"),
        "源还在 —— EXDEV 回退里的删源那一步没做,剪切退化成了复制"
    );
}

// ---- 代码质量复核追加(2026-09):C1 数据安全 + I3 挂起 ---------------------

/// F220/B3 复核 C1(Critical):`to` 是 `from` 的祖先时,「覆盖前先删目标」
/// 会把源所在的整棵树一起摧毁。复现与复核者一致:`src=/home/testuser/p/p`,
/// `dst=/home/testuser/p`,`overwrite=true`——UI 上完全可达(复制嵌套同名
/// 目录 `p/p`,粘到上一级,目标 `p` 已存在,选覆盖)。真实 `cp -a p/p p`
/// 自己会因为「不能把目录拷进它自身」拒绝;是我们加的「先 rm -rf 清目标」
/// 把一个本该被拒绝的操作变成了不可逆的数据摧毁。
///
/// 判据:返回 `Err`;服务端的树**完好无损**(目标、源、源里的文件都还在);
/// 探针**一条请求都没收到**——证明是在发出任何 exec/SFTP 请求之前就被挡下,
/// 不是「先删了一半才发现不对」。
#[tokio::test]
async fn pasting_into_an_ancestor_of_the_source_is_refused_before_any_request() {
    let mut t = Tree::new();
    t.insert(b"/home/testuser".to_vec(), vec![Node::dir(b"p")]);
    t.insert(b"/home/testuser/p".to_vec(), vec![Node::dir(b"p")]);
    t.insert(
        b"/home/testuser/p/p".to_vec(),
        vec![Node::file_with(b"payload.txt", b"precious")],
    );

    let (addr, probe, tree_h) = common::spawn_sftp_server(t).await;
    let (conn, sftp) = (conn_of(addr).await, client(addr).await);

    let result = transfer_into(
        &sftp,
        &conn,
        &[(rp("/home/testuser/p/p"), rp("/home/testuser/p"))],
        CopyMode::Copy,
        true,
    )
    .await;
    assert!(
        result.is_err(),
        "dst 是 src 的祖先,这个覆盖粘贴必须被拒绝,而不是执行"
    );

    let t = tree_h.lock().unwrap();
    assert!(
        exists(&t, b"/home/testuser/p"),
        "目标(同时也是源的祖先)不该被删"
    );
    assert!(exists(&t, b"/home/testuser/p/p"), "源不该被删");
    assert!(
        exists(&t, b"/home/testuser/p/p/payload.txt"),
        "源里的内容不该被删掉 —— 这正是复核挖出的那个数据摧毁"
    );
    drop(t);
    let p = probe.lock().unwrap();
    assert!(
        p.execs.is_empty() && p.seen.is_empty(),
        "挡下来的操作不该发出任何请求(exec 或 SFTP):execs={:?} seen={:?}",
        p.execs,
        p.seen
    );
}

/// F220/B3 复核 C1 的反面:`box_sibling` 与 `box` 只是共享字节前缀,不是
/// 它的子孙(`box_sibling` 与 `box` 之间隔着 `_`,不是 `/`)——合法的
/// 「拷贝 box → box_sibling」不该被这道新闸冤枉挡掉。这条测试专门用来
/// 杀「把 `/` 边界判断去掉、改成裸前缀比较」那一类变异。
#[tokio::test]
async fn pasting_to_a_sibling_that_merely_shares_a_byte_prefix_is_not_blocked() {
    let (addr, _probe, tree_h) = common::spawn_sftp_server(nested_tree()).await;
    let (conn, sftp) = (conn_of(addr).await, client(addr).await);

    let report = transfer_into(
        &sftp,
        &conn,
        &[(rp("/home/testuser/box"), rp("/home/testuser/box_sibling"))],
        CopyMode::Copy,
        false,
    )
    .await
    .expect("box_sibling 只是共享字节前缀,不是 box 的子孙,不该被 C1 那道闸挡下");
    assert_eq!(report, TransferReport::Exec, "exec 可用时该走快路径");

    let t = tree_h.lock().unwrap();
    assert!(
        exists(&t, b"/home/testuser/box_sibling"),
        "目标没建出来 —— 说明合法操作被误伤了"
    );
}

/// F220/B3 复核 I3:目录里混进一条 FIFO/设备/socket(`EntryKind::Other`)时,
/// `copy_one` 必须显式报错,不能把它当成普通文件悄悄拷一份。
///
/// 诚实说明测试能力的边界:真实远端上 `open_read` 对一个没有写端的具名
/// 管道会永久阻塞(`read_chunk` 那个 await 没有超时),但这个假服务端是
/// 纯内存实现,`read_chunk` 不会真的阻塞——这里测不出「挂起」本身,能测
/// 的只是「代码是不是走了显式拒绝这条分支,而不是把它当空文件拷走」。
#[tokio::test]
async fn a_paste_refuses_a_named_pipe_instead_of_silently_copying_it_as_an_empty_file() {
    let mut t = nested_tree();
    t.entry(b"/home/testuser/box".to_vec())
        .or_default()
        .push(Node::fifo(b"a-fifo"));

    let (addr, _probe, tree_h) = common::spawn_sftp_server_without_exec(t).await;
    let (conn, sftp) = (conn_of(addr).await, client(addr).await);

    let outcome = transfer_into(
        &sftp,
        &conn,
        &[(rp("/home/testuser/box"), rp("/home/testuser/box-copy"))],
        CopyMode::Copy,
        false,
    )
    .await;
    assert!(
        outcome.is_err(),
        "目录里有一条 FIFO,粘贴该报错,而不是悄悄跳过或悄悄成功"
    );

    let t = tree_h.lock().unwrap();
    assert!(
        !exists(&t, b"/home/testuser/box-copy/a-fifo"),
        "FIFO 不该被当成普通文件拷出一份(内容为空的)副本"
    );
}

// ---- 代码质量复核第二轮追加(2026-09):C1 闸的两个新洞 ---------------------

/// F220/B3 复核第二轮·1:`is_ancestor_or_self` 被 `from` 的末尾斜杠静默
/// 绕过。`RemotePath` 不做归一化(`joining_keeps_bytes_and_uses_a_single_slash`
/// 已证明末尾斜杠会被原样保留),带斜杠是代码库承认的合法状态——修复前,
/// `from="/home/testuser/box/"` 时 `b[a.len()]` 落的是 `to` 里子项名字的
/// 第一个字符而不是分隔符,边界判断永远不成立,C1 那道闸整个失效。
///
/// 场景与下面 `pasting_a_directory_into_its_own_subdirectory_...` 同构
/// (把目录粘贴进它自己的子目录),唯一区别是 `from` 带一个末尾斜杠。
#[tokio::test]
async fn pasting_a_source_with_a_trailing_slash_into_its_own_subdirectory_is_still_refused() {
    let (addr, probe, tree_h) = common::spawn_sftp_server(nested_tree()).await;
    let (conn, sftp) = (conn_of(addr).await, client(addr).await);

    let from = RemotePath::from_bytes(b"/home/testuser/box/".to_vec());
    let result = transfer_into(
        &sftp,
        &conn,
        &[(from, rp("/home/testuser/box/sub"))],
        CopyMode::Copy,
        true,
    )
    .await;
    assert!(
        result.is_err(),
        "from 带末尾斜杠也不该绕过 C1 那道闸 —— 这个粘贴必须被拒绝"
    );

    let t = tree_h.lock().unwrap();
    assert!(exists(&t, b"/home/testuser/box"), "源不该被动");
    assert!(
        exists(&t, b"/home/testuser/box/sub/deep.txt"),
        "目标目录里原有的内容不该被清空"
    );
    drop(t);
    let p = probe.lock().unwrap();
    assert!(
        p.execs.is_empty() && p.seen.is_empty(),
        "挡下来的操作不该发出任何请求:execs={:?} seen={:?}",
        p.execs,
        p.seen
    );
}

/// F220/B3 复核第二轮·2:`is_ancestor_or_self(f, t) || is_ancestor_or_self(t, f)`
/// 两支各防一个方向。上面的 `pasting_into_an_ancestor_of_the_source_...`
/// 测的是「`to` 是 `from` 的祖先」(`is_ancestor_or_self(t, f)` 那一支);
/// 这条补相反方向——「`from` 是 `to` 的祖先」(`is_ancestor_or_self(f, t)`
/// 那一支):把一个目录粘贴进它自己的子目录,`from=box`,`to=box/sub`。
/// 复核者用真实靶向变异实测过:把两支砍成只留 `is_ancestor_or_self(t, f)`
/// 那一支,29 条测试全绿——这个方向此前零覆盖。
#[tokio::test]
async fn pasting_a_directory_into_its_own_subdirectory_is_refused_before_any_request() {
    let (addr, probe, tree_h) = common::spawn_sftp_server(nested_tree()).await;
    let (conn, sftp) = (conn_of(addr).await, client(addr).await);

    let result = transfer_into(
        &sftp,
        &conn,
        &[(rp("/home/testuser/box"), rp("/home/testuser/box/sub"))],
        CopyMode::Copy,
        true,
    )
    .await;
    assert!(
        result.is_err(),
        "from 是 to 的祖先(把目录粘贴进它自己的子目录),这个粘贴必须被拒绝"
    );

    let t = tree_h.lock().unwrap();
    assert!(exists(&t, b"/home/testuser/box"), "源不该被动");
    assert!(
        exists(&t, b"/home/testuser/box/sub"),
        "目标(同时也是源的子孙)不该被动"
    );
    assert!(
        exists(&t, b"/home/testuser/box/sub/deep.txt"),
        "目标目录里原有的内容不该被清空"
    );
    drop(t);
    let p = probe.lock().unwrap();
    assert!(
        p.execs.is_empty() && p.seen.is_empty(),
        "挡下来的操作不该发出任何请求:execs={:?} seen={:?}",
        p.execs,
        p.seen
    );
}
