# 切片 D2-a：SFTP 远端写操作 实现计划（F54 部分 / F57 / D17 / D21）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让远端栏能**改远端数据**——多选、新建文件夹、重命名、删除（含递归）、改权限——每一步都有确认与守护，绝不静默对错文件下手。

**Architecture:** `mullion-ssh` 侧给 `SftpClient` 补写方法（全部经 `RemotePath::as_wire()` 这道唯一出口），并新增一个 `exec` 能力用于 F57 的 `rm -rf` 快路径；`mullion-app` 侧把「选中」从单条升级成选择集 + 光标 + 锚点的纯状态机，右键菜单与四个对话框走 `UiActions` 回到 `app.rs` 落成异步任务。写操作**只在远端栏**（设计 D5：本地文件管理外包给资源管理器）。

**Tech Stack:** `russh 0.54.5`、`russh-sftp 2.4.0`、`egui 0.30`、`tokio`。

**这一片故意不做**（留给 D2-b）：上传 / 下载 / 传输队列 F55 / 并发 F56 / `.part` 语义 / 同名冲突 / Windows 非法文件名 / 断线退避重连。
**这一片故意不发版**：D2-a 与 D2-b 合并成一次 Release，用户已明确「完成后一并验收」。

---

## 背景：读之前必须知道的五件事

1. **`RemotePath` 的唯一 wire 出口是 `as_wire() -> Result<String, NonUtf8Path>`**（`crates/mullion-ssh/src/sftp.rs:77`）。含非 UTF-8 字节**或含 `U+FFFD`** 的路径拿不到 wire 串，于是在类型层就发不出请求。**所有新写方法都必须走它**，不许 `String::from_utf8_lossy` 绕过去——绕过去就是「静默打错文件」，这正是 D16 要挡的。

2. **界面判「这条能不能操作」用 `is_operable()`，不是 `is_utf8()`。** `russh-sftp 2.4.0` 的 `src/buf.rs:25` 在**收包方向也**过 `from_utf8_lossy`，远端一个 GBK 文件名到我们手里已经是合法 UTF-8、只带 `U+FFFD`。`is_utf8()` 对这类条目恒 `true`。

3. **假服务端现在的树是只读的**（`Arc<Tree>`），且 `remove` 恒返回 `PermissionDenied`（D1 是只读切片，服务端替我们把关）。Task 1 要把它改成可写。

4. **`mullion-ssh` 现在没有 exec 能力**（`grep -rn "exec" crates/mullion-ssh/src/` 只有一条注释）。F57 的快路径要新写。

5. **`russh` 的 `Channel::exec` 签名是 `pub async fn exec<A: Into<Vec<u8>>>(&self, want_reply: bool, command: A)`**（`russh-0.54.5/src/channels/mod.rs:234`）——**收字节**，不是 `&str`。这对 F57 的路径转义很重要：命令行可以是任意字节。

---

## 文件结构

| 文件 | 责任 | 动作 |
|---|---|---|
| `crates/mullion-ssh/tests/common/sftp_server.rs` | 假 SFTP 服务端 + 探针 | 改：树可写、实现 mkdir/rmdir/remove/rename/setstat |
| `crates/mullion-ssh/tests/common/mod.rs` | 假 sshd | 改：`spawn_sftp_server` 返回树句柄；加 `exec_request` |
| `crates/mullion-ssh/src/sftp.rs` | SFTP 协议封装 | 改：加五个写方法 + `stat` |
| `crates/mullion-ssh/src/exec.rs` | **新建**：一次性 exec 命令 + shell 单引号转义 | 新建 |
| `crates/mullion-ssh/src/lib.rs` | crate 根 | 改：`pub mod exec;` |
| `crates/mullion-ssh/src/remove_tree.rs` | **新建**：递归删除（先 exec 后回退） | 新建 |
| `crates/mullion-ssh/tests/sftp_write.rs` | **新建**：写操作端到端 | 新建 |
| `crates/mullion-app/src/files/state.rs` | 一栏的运行态 | 改：选择集 + 光标 + 锚点 |
| `crates/mullion-app/src/ui/files_panel.rs` | 面板渲染 | 改：多选高亮、右键菜单、`FileAction` 扩展 |
| `crates/mullion-app/src/ui/files_dialog.rs` | **新建**：四个对话框（新建 / 重命名 / 删除确认 / 权限） | 新建 |
| `crates/mullion-app/src/ui/mod.rs` | 每帧构建 UI | 改：`UiState` 加对话框状态、`UiActions` 加 `files_op`、`build_ui` 调对话框 |
| `crates/mullion-app/src/app.rs` | 事件循环 + 接线 | 改：`FileOp` 落成异步任务、新 `UserEvent`、Delete/F2 键 |

---

## Task 1：假服务端可写化（测试基建）

**Files:**
- Modify: `crates/mullion-ssh/tests/common/sftp_server.rs`
- Modify: `crates/mullion-ssh/tests/common/mod.rs:119-227`
- Modify: `crates/mullion-ssh/tests/sftp_browse.rs`（7 处 `spawn_sftp_server` 调用点）

**为什么先做这个：** 没有可写的服务端，后面每一个写方法都只能断言「没 panic」，那种测试一律恒绿。

- [ ] **Step 1：把树改成可写，并让 `spawn_sftp_server` 把句柄交出来**

`crates/mullion-ssh/tests/common/sftp_server.rs`——把 `SftpHandler` 的 `tree` 字段从 `Arc<Tree>` 换成 `Arc<Mutex<Tree>>`：

```rust
pub struct SftpHandler {
    tree: Arc<Mutex<Tree>>,
    probe: Arc<Mutex<Probe>>,
    dirs: HashMap<String, (Vec<u8>, bool)>,
    next_handle: u64,
}

impl SftpHandler {
    pub fn new(tree: Arc<Mutex<Tree>>, probe: Arc<Mutex<Probe>>) -> Self {
        Self { tree, probe, dirs: HashMap::new(), next_handle: 0 }
    }
```

`attrs_of` / `opendir` / `readdir` / `readlink` 里所有 `self.tree.get(..)` 改成先 `let tree = self.tree.lock().unwrap();` 再 `tree.get(..)`。**注意 `readdir` 里现在有 `self.dirs.get_mut(&handle)` 与 `self.tree` 的双重借用**——先把 `dir` clone 出来结束对 `self.dirs` 的借用，再锁 `tree`（现有代码已经这么写了，保持）。

`crates/mullion-ssh/tests/common/mod.rs`：

```rust
pub struct SftpSshHandler {
    channels: std::collections::HashMap<ChannelId, Channel<Msg>>,
    tree: Arc<std::sync::Mutex<sftp_server::Tree>>,
    probe: Arc<std::sync::Mutex<sftp_server::Probe>>,
}
```

`spawn_sftp_server` 改签名，**第三个返回值是树句柄**（测试要拿它断言「文件真的没了 / 真的建出来了」）：

```rust
#[allow(dead_code)]
pub async fn spawn_sftp_server(
    tree: sftp_server::Tree,
) -> (
    std::net::SocketAddr,
    Arc<std::sync::Mutex<sftp_server::Probe>>,
    Arc<std::sync::Mutex<sftp_server::Tree>>,
) {
    let host_key =
        russh::keys::load_secret_key("tests/fixtures/server_hostkey", None).expect("load hostkey");
    let mut config = russh::server::Config::default();
    config.keys.push(host_key);
    let config = Arc::new(config);
    let tree = Arc::new(std::sync::Mutex::new(tree));
    let probe = Arc::new(std::sync::Mutex::new(sftp_server::Probe::default()));

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind");
    let addr = listener.local_addr().unwrap();
    let (t, p) = (tree.clone(), probe.clone());
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let config = config.clone();
            let handler = SftpSshHandler {
                channels: std::collections::HashMap::new(),
                tree: t.clone(),
                probe: p.clone(),
            };
            tokio::spawn(async move {
                let _ = russh::server::run_stream(config, stream, handler).await;
            });
        }
    });
    (addr, probe, tree)
}
```

`crates/mullion-ssh/tests/sftp_browse.rs` 的 7 处调用点全部改成三元组解构，用不到树的写 `_tree`：

```rust
let (addr, _probe, _tree) = common::spawn_sftp_server(tree()).await;
```
```rust
let (addr, probe, _tree) = common::spawn_sftp_server(tree()).await;
```

- [ ] **Step 2：跑一遍确认只读路径没被改坏**

Run: `cargo test -p mullion-ssh --test sftp_browse 2>&1 | tail -20`
Expected: 7 passed（与改动前一致）。

- [ ] **Step 3：给树加两个测试用的查询辅助**

`sftp_server.rs` 末尾追加（**给测试断言用**，不是协议的一部分）：

```rust
/// 树里有没有这一条(目录键或某目录下的一个节点名)。测试断言「删掉了没」
/// 「建出来了没」用它 —— 直接翻 `HashMap` 每个测试都要抄一遍 `split_last`。
pub fn exists(tree: &Tree, path: &[u8]) -> bool {
    if tree.contains_key(path) {
        return true;
    }
    let (dir, name) = split_last(path);
    tree.get(&dir).is_some_and(|v| v.iter().any(|n| n.name == name))
}

/// 某个目录下现有的节点名。断言「目录里剩了什么」用。
pub fn names_in(tree: &Tree, dir: &[u8]) -> Vec<Vec<u8>> {
    tree.get(dir).map(|v| v.iter().map(|n| n.name.clone()).collect()).unwrap_or_default()
}
```

`split_last` 目前是私有 `fn`，把它改成 `pub(crate) fn` 让上面两个辅助能用（同文件内其实不用改，保持 `fn` 即可——两个辅助就在同一文件里）。

- [ ] **Step 4：实现五个写操作协议方法**

替换 `sftp_server.rs` 里现有的 `remove`（那条恒 `PermissionDenied` 的），并新增四个。放在 `impl russh_sftp::server::Handler for SftpHandler` 里：

```rust
    /// 删一个**文件或链接**。目录要走 `rmdir`(与真实 sshd 一致)。
    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
        self.note("remove", &filename);
        let (dir, name) = split_last(filename.as_bytes());
        let mut tree = self.tree.lock().unwrap();
        let Some(v) = tree.get_mut(&dir) else {
            return Err(StatusCode::NoSuchFile);
        };
        let Some(ix) = v.iter().position(|n| n.name == name) else {
            return Err(StatusCode::NoSuchFile);
        };
        if v[ix].kind == NodeKind::Dir {
            // 真实 sshd 对目录发 REMOVE 会回 Failure。测试要能扎住
            // 「客户端把目录当文件删」这个错。
            return Err(StatusCode::Failure);
        }
        v.remove(ix);
        Ok(ok_status(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
        self.note("rmdir", &path);
        let key = path.clone().into_bytes();
        let mut tree = self.tree.lock().unwrap();
        // 非空目录必须拒 —— 递归删除的正确性全靠这一条兜底:实现要是漏删了
        // 里面的东西,这里会回 Failure 而不是静默成功。
        if tree.get(&key).is_some_and(|v| !v.is_empty()) {
            return Err(StatusCode::Failure);
        }
        tree.remove(&key);
        let (dir, name) = split_last(&key);
        if let Some(v) = tree.get_mut(&dir) {
            v.retain(|n| !(n.name == name && n.kind == NodeKind::Dir));
        }
        Ok(ok_status(id))
    }

    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        self.note("mkdir", &path);
        let key = path.clone().into_bytes();
        let mut tree = self.tree.lock().unwrap();
        if tree.contains_key(&key) {
            return Err(StatusCode::Failure);
        }
        let (dir, name) = split_last(&key);
        if !tree.contains_key(&dir) {
            return Err(StatusCode::NoSuchFile);
        }
        tree.entry(dir).or_default().push(Node::dir(&name));
        tree.insert(key, Vec::new());
        Ok(ok_status(id))
    }

    async fn rename(
        &mut self,
        id: u32,
        oldpath: String,
        newpath: String,
    ) -> Result<Status, Self::Error> {
        self.note("rename", &format!("{oldpath} -> {newpath}"));
        let (od, on) = split_last(oldpath.as_bytes());
        let (nd, nn) = split_last(newpath.as_bytes());
        let mut tree = self.tree.lock().unwrap();
        let Some(v) = tree.get_mut(&od) else {
            return Err(StatusCode::NoSuchFile);
        };
        let Some(ix) = v.iter().position(|n| n.name == on) else {
            return Err(StatusCode::NoSuchFile);
        };
        let mut node = v.remove(ix);
        let was_dir = node.kind == NodeKind::Dir;
        node.name = nn.clone();
        tree.entry(nd).or_default().push(node);
        if was_dir {
            if let Some(children) = tree.remove(oldpath.as_bytes()) {
                tree.insert(newpath.into_bytes(), children);
            }
        }
        Ok(ok_status(id))
    }

    async fn setstat(
        &mut self,
        id: u32,
        path: String,
        attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        self.note("setstat", &path);
        let (dir, name) = split_last(path.as_bytes());
        let mut tree = self.tree.lock().unwrap();
        // 目录自身的 mode 存在父目录那条记录上(与 attrs_of 的查法一致)。
        let Some(v) = tree.get_mut(&dir) else {
            return Err(StatusCode::NoSuchFile);
        };
        let Some(n) = v.iter_mut().find(|n| n.name == name) else {
            return Err(StatusCode::NoSuchFile);
        };
        if let Some(m) = attrs.permissions {
            n.mode = m & 0o7777;
        }
        Ok(ok_status(id))
    }
```

在文件里加这个小工具（`close` 里那段重复的 `Status` 构造也改成用它）：

```rust
fn ok_status(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: "Ok".into(),
        language_tag: "en-US".into(),
    }
}
```

`Node` 需要 `PartialEq` 才能写 `n.kind == NodeKind::Dir` —— `NodeKind` 已经 `derive(PartialEq, Eq)`，够了。

- [ ] **Step 5：全量跑绿**

Run: `cargo test -p mullion-ssh 2>&1 | grep -E "test result|FAILED|error\["`
Expected: 全 ok，无 FAILED。

Run: `cargo clippy -p mullion-ssh --all-targets -- -D warnings`
Expected: 无输出。

- [ ] **Step 6：提交**

```bash
git add crates/mullion-ssh/tests/
git commit -m "test(ssh): 假 SFTP 服务端改成可写内存树,补 mkdir/rmdir/remove/rename/setstat (F54/F57)"
```

---

## Task 2：`SftpClient` 的五个写方法 + `stat`

**Files:**
- Modify: `crates/mullion-ssh/src/sftp.rs`
- Create: `crates/mullion-ssh/tests/sftp_write.rs`

- [ ] **Step 1：先写失败的端到端测试**

新建 `crates/mullion-ssh/tests/sftp_write.rs`：

```rust
//! SFTP 写操作的端到端测试:真握手 → 开 sftp subsystem → 改远端。
//! 服务端是同进程的可写假 SFTP(见 common/sftp_server.rs)。

mod common;

use std::sync::Arc;

use common::sftp_server::{exists, names_in, Node, Tree};
use mullion_ssh::config::{AuthMethod, SshConfig};
use mullion_ssh::known_hosts::{Fingerprint, HostKeyDecision, HostKeyFuture, HostKeyPolicy};
use mullion_ssh::session::establish;
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
    t.insert(b"/home/testuser/docs".to_vec(), vec![Node::file(b"inner.txt", 3)]);
    t
}

async fn client(addr: std::net::SocketAddr) -> SftpClient {
    let conn = Arc::new(establish(&cfg(addr), Arc::new(AcceptAll)).await.expect("connect"));
    SftpClient::open(conn).await.expect("open sftp")
}

#[tokio::test]
async fn creating_a_directory_makes_it_appear_on_the_server() {
    let (addr, _probe, tree_h) = common::spawn_sftp_server(tree()).await;
    let sftp = client(addr).await;

    sftp.create_dir(&RemotePath::from_bytes(b"/home/testuser/new".to_vec()))
        .await
        .expect("mkdir");

    let t = tree_h.lock().unwrap();
    assert!(exists(&t, b"/home/testuser/new"), "新目录该出现在服务端");
}

/// 中文名逐字节往返 —— 建一个中文目录,服务端收到的必须是同一串 UTF-8 字节。
#[tokio::test]
async fn creating_a_chinese_directory_sends_the_exact_bytes() {
    let (addr, probe, tree_h) = common::spawn_sftp_server(tree()).await;
    let sftp = client(addr).await;

    let p = RemotePath::from_bytes("/home/testuser/新建文件夹".as_bytes().to_vec());
    sftp.create_dir(&p).await.expect("mkdir 中文");

    let seen = probe.lock().unwrap().paths_for("mkdir");
    assert!(
        seen.iter().any(|s| s.as_bytes() == "/home/testuser/新建文件夹".as_bytes()),
        "服务端收到的字节必须与请求的一致: {seen:?}"
    );
    assert!(exists(&tree_h.lock().unwrap(), "/home/testuser/新建文件夹".as_bytes()));
}

#[tokio::test]
async fn renaming_moves_the_entry() {
    let (addr, _probe, tree_h) = common::spawn_sftp_server(tree()).await;
    let sftp = client(addr).await;

    sftp.rename(
        &RemotePath::from_bytes(b"/home/testuser/a.txt".to_vec()),
        &RemotePath::from_bytes(b"/home/testuser/b.txt".to_vec()),
    )
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

    sftp.remove_file(&RemotePath::from_bytes(b"/home/testuser/a.txt".to_vec()))
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

    let docs = RemotePath::from_bytes(b"/home/testuser/docs".to_vec());
    sftp.remove_dir(&docs).await.expect_err("非空目录不该删得掉");

    sftp.remove_file(&RemotePath::from_bytes(b"/home/testuser/docs/inner.txt".to_vec()))
        .await
        .expect("先删里面的文件");
    sftp.remove_dir(&docs).await.expect("空了之后该删得掉");

    assert!(!exists(&tree_h.lock().unwrap(), b"/home/testuser/docs"));
}

#[tokio::test]
async fn changing_permissions_is_visible_in_a_later_listing() {
    let (addr, _probe, _tree) = common::spawn_sftp_server(tree()).await;
    let sftp = client(addr).await;

    let p = RemotePath::from_bytes(b"/home/testuser/a.txt".to_vec());
    sftp.set_permissions(&p, 0o600).await.expect("setstat");

    let got = sftp
        .list_dir(&RemotePath::from_bytes(b"/home/testuser".to_vec()))
        .await
        .expect("list");
    let a = got.iter().find(|e| e.name.as_bytes() == b"a.txt").unwrap();
    assert_eq!(a.mode & 0o777, 0o600, "改完权限,再列一次要看得见新值");
}

/// D16 的核心不变量在写方向也成立:**发不出去的路径一个请求都不发**。
/// 只测「返回 Err」不够 —— 那对「先发了请求再失败」也成立。要连探针一起验。
#[tokio::test]
async fn a_non_operable_path_never_reaches_the_wire_for_any_write() {
    let (addr, probe, tree_h) = common::spawn_sftp_server(tree()).await;
    let sftp = client(addr).await;

    let bad = RemotePath::from_bytes(vec![b'/', 0xff, 0xfe, b'x']);
    let lossy = RemotePath::from_bytes("/home/testuser/\u{fffd}\u{fffd}.txt".as_bytes().to_vec());
    let ok = RemotePath::from_bytes(b"/home/testuser/z".to_vec());

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
            sftp.set_permissions(p, 0o644).await.expect_err("setstat 不该发得出去"),
            SftpError::NonUtf8Name
        ));
        // rename 两个参数都要挡:任一端发不出去就整条不发。
        assert!(matches!(
            sftp.rename(p, &ok).await.expect_err("rename 源不该发得出去"),
            SftpError::NonUtf8Name
        ));
        assert!(matches!(
            sftp.rename(&ok, p).await.expect_err("rename 目标不该发得出去"),
            SftpError::NonUtf8Name
        ));
    }

    let pr = probe.lock().unwrap();
    for op in ["mkdir", "remove", "rmdir", "setstat", "rename"] {
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
```

- [ ] **Step 2：跑，确认编译失败（方法还不存在）**

Run: `cargo test -p mullion-ssh --test sftp_write 2>&1 | head -20`
Expected: 编译错误 `no method named 'create_dir' found for struct 'SftpClient'` 等。

- [ ] **Step 3：实现五个写方法 + `stat`**

`crates/mullion-ssh/src/sftp.rs` 的 `impl SftpClient` 里，`canonicalize` 之后追加：

```rust
    /// 新建目录。父目录不存在会失败(不做 `mkdir -p` —— 界面上用户是在
    /// 某个具体目录里按的「新建文件夹」,父目录必然存在;悄悄创建一串
    /// 中间目录只会把打错的路径变成一堆垃圾目录)。
    pub async fn create_dir(&self, path: &RemotePath) -> Result<(), SftpError> {
        let wire = path.as_wire()?;
        self.inner
            .create_dir(wire)
            .await
            .map_err(|e| SftpError::Protocol(e.to_string()))
    }

    /// 改名 / 移动。**两个路径都要能发得出去**:任一端被 `as_wire` 挡下就
    /// 整条不发 —— 只挡一端的话,另一端会被拿去跟一个 lossy 串配对,
    /// 结果是把文件改成一个谁也打不开的名字。
    pub async fn rename(&self, from: &RemotePath, to: &RemotePath) -> Result<(), SftpError> {
        let (f, t) = (from.as_wire()?, to.as_wire()?);
        self.inner
            .rename(f, t)
            .await
            .map_err(|e| SftpError::Protocol(e.to_string()))
    }

    /// 删一个**文件或符号链接**。SFTP 的 REMOVE 对链接删的是链接本身,
    /// **不跟随**(设计 D17)——搞错了就是把远端整个目标目录删了。
    /// 目录要走 [`SftpClient::remove_dir`]。
    pub async fn remove_file(&self, path: &RemotePath) -> Result<(), SftpError> {
        let wire = path.as_wire()?;
        self.inner
            .remove_file(wire)
            .await
            .map_err(|e| SftpError::Protocol(e.to_string()))
    }

    /// 删一个**空目录**。非空会被服务端拒 —— 递归删除见
    /// `crate::remove_tree::remove_tree`。
    pub async fn remove_dir(&self, path: &RemotePath) -> Result<(), SftpError> {
        let wire = path.as_wire()?;
        self.inner
            .remove_dir(wire)
            .await
            .map_err(|e| SftpError::Protocol(e.to_string()))
    }

    /// 改权限位(设计 D21)。**只送 permissions 一个字段**:`FileAttributes`
    /// 的其余字段留 `None`,SFTP v3 的 attrs 带 flags 位图,没设的字段不会
    /// 被写过去 —— 顺手把 uid/gid/mtime 一起送出去等于拿本地的猜测覆盖
    /// 远端的真值。
    pub async fn set_permissions(&self, path: &RemotePath, mode: u32) -> Result<(), SftpError> {
        let wire = path.as_wire()?;
        let attrs = russh_sftp::protocol::FileAttributes {
            permissions: Some(mode & 0o7777),
            ..Default::default()
        };
        self.inner
            .set_metadata(wire, attrs)
            .await
            .map_err(|e| SftpError::Protocol(e.to_string()))
    }

    /// 取一条的属性。**用 lstat 语义(不跟随链接)**,与 `list_dir` 的
    /// readdir 对齐 —— 跟随了的话,一条指向大文件的链接会报出目标的大小,
    /// 删除确认框里的「共 N 字节」就是错的。
    pub async fn stat(&self, path: &RemotePath) -> Result<Entry, SftpError> {
        let wire = path.as_wire()?;
        let md = self
            .inner
            .symlink_metadata(wire)
            .await
            .map_err(|e| SftpError::Protocol(e.to_string()))?;
        let kind = if md.is_symlink() {
            EntryKind::Symlink
        } else if md.is_dir() {
            EntryKind::Dir
        } else if md.is_regular() {
            EntryKind::File
        } else {
            EntryKind::Other
        };
        // 名字只取最后一段,与 `Entry::name` 的语义("只是名字,不含目录部分")一致。
        let name = RemotePath::from_bytes(last_segment(path.as_bytes()).to_vec());
        Ok(Entry {
            name,
            kind,
            size: md.size.unwrap_or(0),
            mtime: md.mtime.unwrap_or(0),
            mode: md.permissions.unwrap_or(0) & 0o7777,
            uid: md.uid.unwrap_or(0),
            gid: md.gid.unwrap_or(0),
            link_target: None,
        })
    }
```

在 `impl SftpClient` 外面（文件末尾 `#[cfg(test)]` 之前）加：

```rust
/// `/a/b/c` → `c`。根本身(`/`)与不含 `/` 的相对名原样返回。
fn last_segment(path: &[u8]) -> &[u8] {
    match path.iter().rposition(|b| *b == b'/') {
        Some(ix) if ix + 1 < path.len() => &path[ix + 1..],
        _ => path,
    }
}
```

- [ ] **Step 4：跑，确认通过**

Run: `cargo test -p mullion-ssh --test sftp_write 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok. 7 passed`

- [ ] **Step 5：变异验收（必做，自己重做一遍，不信实现者报告）**

对每一条做一次变异，确认对应测试**真的变红**。改之前先备份，改完还原——**不要用 `git checkout` 还原，那会把同一文件里还没提交的其他改动一起冲掉**：

```bash
cp crates/mullion-ssh/src/sftp.rs /tmp/sftp.rs.bak
# 变异 1:把 create_dir 里的 `path.as_wire()?` 换成 `Ok::<_, NonUtf8Path>(path.display().into_owned())?`
#   → a_non_operable_path_never_reaches_the_wire_for_any_write 必须变红
# 变异 2:把 rename 的 `to.as_wire()?` 改成 `to.display().into_owned()`
#   → 同上必须变红(只挡一端是不够的)
# 变异 3:把 stat 里的 symlink_metadata 换成 metadata(跟随链接)
#   → 见下面 Task 4 的链接测试
cp /tmp/sftp.rs.bak crates/mullion-ssh/src/sftp.rs
```

每次变异后跑 `cargo test -p mullion-ssh --test sftp_write`，**记录哪一条测试变红**。若某个变异全绿，说明该处零覆盖，**补测试再继续**。

- [ ] **Step 6：提交**

```bash
git add crates/mullion-ssh/src/sftp.rs crates/mullion-ssh/tests/sftp_write.rs
git commit -m "feat(ssh): SftpClient 补写方法(mkdir/rename/remove/rmdir/setstat/stat) (F54)"
```

---

## Task 3：exec 能力 + shell 单引号转义

**Files:**
- Create: `crates/mullion-ssh/src/exec.rs`
- Modify: `crates/mullion-ssh/src/lib.rs`
- Modify: `crates/mullion-ssh/tests/common/mod.rs`（假 sshd 加 `exec_request`）
- Modify: `crates/mullion-ssh/tests/sftp_write.rs`（加 exec 测试）

- [ ] **Step 1：先写转义的失败单测**

新建 `crates/mullion-ssh/src/exec.rs`，先只写测试模块与空壳：

```rust
//! 在**已建立**的连接上跑一条一次性命令(F57 的 `rm -rf` 快路径)。
//!
//! 与 `session::open_pty`/`sftp::SftpClient::open` 同一条防呆:签名里没有
//! 任何网络参数,想在这里偷偷重连一次都做不到。
//!
//! **不请求 PTY**:这是批处理命令,不是交互 shell。请求了的后果是远端白白
//! 起一个伪终端、`who` 里多一行幽灵会话,而且 `PermitTTY no` 下会直接被拒。

use std::sync::Arc;

use crate::session::SshConnection;

/// 一条命令跑完的结果。
#[derive(Debug)]
pub struct ExecOutcome {
    /// 远端的退出码。**`None` 表示对端没送 `exit-status`** —— 那不等于成功,
    /// 调用方(F57 的回退判定)必须把它当失败处理。
    pub exit_status: Option<u32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl ExecOutcome {
    /// 命令是不是干净地成功了。**`exit_status == None` 算失败**(见该字段文档)。
    pub fn succeeded(&self) -> bool {
        self.exit_status == Some(0)
    }
}

#[derive(Debug)]
pub enum ExecError {
    /// 开 channel 失败(连接已断)。
    Channel,
    /// 对端**拒绝**执行命令。sftp-only 账号(`ForceCommand internal-sftp` +
    /// `ChrootDirectory`)就是这一类 —— F57 靠它决定回退到逐文件递归删除。
    Rejected,
}

impl std::fmt::Display for ExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecError::Channel => write!(f, "无法开启命令通道,连接可能已断开"),
            ExecError::Rejected => write!(f, "远端拒绝执行命令(sftp-only 账号会这样)"),
        }
    }
}

impl std::error::Error for ExecError {}

/// 把一段**字节**包成 shell 单引号字面量。
///
/// 规则只有一条:用 `'` 包住,内部的每个 `'` 换成 `'\''`(闭合、转义一个
/// 单引号、再开启)。单引号内 POSIX shell **不做任何解释** —— `$`、反引号、
/// `\`、换行、空格、`*` 全是字面量。这是唯一不需要枚举元字符的正确写法;
/// 任何「把危险字符列出来逐个转义」的实现,漏一个就是远端任意命令执行。
///
/// 返回字节而不是 `String`:`russh` 的 `Channel::exec` 收 `Into<Vec<u8>>`,
/// 而路径本来就是字节。中间过一趟 `String` 等于给非 UTF-8 路径设一道
/// 本不必要的门槛。
pub fn shell_quote(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 2);
    out.push(b'\'');
    for b in bytes {
        if *b == b'\'' {
            out.extend_from_slice(b"'\\''");
        } else {
            out.push(*b);
        }
    }
    out.push(b'\'');
    out
}

/// 在已建立的连接上跑一条命令,读完 stdout/stderr 与退出码再返回。
///
/// **只用于短命令**(`rm -rf` 这类):全部输出攒在内存里,不做流式。
pub async fn exec(conn: &Arc<SshConnection>, command: Vec<u8>) -> Result<ExecOutcome, ExecError> {
    use russh::ChannelMsg;

    let mut channel = conn
        .handle()
        .channel_open_session()
        .await
        .map_err(|_| ExecError::Channel)?;
    // `want_reply = true`:对端拒绝时 `exec` 直接返回 Err,这正是 F57
    // 判定「该回退了」的信号。设 false 的话拒绝是静默的,我们会误以为
    // 命令跑了而且成功了。
    if channel.exec(true, command).await.is_err() {
        // `Channel<Msg>` 没有自动发 CHANNEL_CLOSE 的 Drop,不显式关就是
        // 泄漏一个 channel slot(同 `SftpClient::open` 那条注释)。
        let _ = channel.close().await;
        return Err(ExecError::Rejected);
    }

    let mut out = ExecOutcome { exit_status: None, stdout: Vec::new(), stderr: Vec::new() };
    while let Some(msg) = channel.wait().await {
        match msg {
            ChannelMsg::Data { data } => out.stdout.extend_from_slice(&data),
            ChannelMsg::ExtendedData { data, .. } => out.stderr.extend_from_slice(&data),
            ChannelMsg::ExitStatus { exit_status } => out.exit_status = Some(exit_status),
            ChannelMsg::Close | ChannelMsg::Eof => {}
            _ => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// F57 的路径转义。逐条覆盖 spec 点名的五类:空格、引号、换行、`$`、反引号。
    ///
    /// 判据是「包起来之后,shell 解出来的还是原串」。这里不能真起一个 shell,
    /// 所以退一步验结构:外层一对单引号,内部除了 `'` 的转义序列之外**逐字节
    /// 原样**。
    #[test]
    fn quoting_neutralises_every_shell_metacharacter() {
        for raw in [
            &b"a b.txt"[..],
            &b"it's here"[..],
            &b"line1\nline2"[..],
            &b"$HOME"[..],
            &b"`whoami`"[..],
            &b"a*b?c[d]"[..],
            &b"back\\slash"[..],
            "中文 名.txt".as_bytes(),
        ] {
            let q = shell_quote(raw);
            assert_eq!(q.first(), Some(&b'\''), "必须以单引号开头: {:?}", q);
            assert_eq!(q.last(), Some(&b'\''), "必须以单引号结尾: {:?}", q);
            // 反解:去掉外层引号,把 `'\''` 还原成 `'`,应当逐字节等于原串。
            let inner = &q[1..q.len() - 1];
            let restored = String::from_utf8_lossy(inner).replace("'\\''", "'");
            assert_eq!(
                restored.as_bytes(),
                String::from_utf8_lossy(raw).as_bytes(),
                "反解回来的串必须与原串逐字节相同"
            );
        }
    }

    /// 单引号是**唯一**需要特殊处理的字符。这条钉死的是「别去枚举元字符」:
    /// 换成一个「把 `$`/反引号/`\` 挨个加反斜杠」的实现,这条必然变红 ——
    /// 那种实现在单引号内会把 `\$` 原样留下,反解就对不上了。
    #[test]
    fn a_single_quote_is_the_only_character_that_gets_rewritten() {
        assert_eq!(shell_quote(b"$`\\*"), b"'$`\\*'".to_vec());
        assert_eq!(shell_quote(b"a'b"), b"'a'\\''b'".to_vec());
    }

    /// 空串也要包成一对引号 —— 裸的空串在命令行里等于「这个参数不存在」,
    /// `rm -rf` 的参数凭空少一个,后果是删错东西。
    #[test]
    fn an_empty_path_still_produces_a_quoted_empty_argument() {
        assert_eq!(shell_quote(b""), b"''".to_vec());
    }

    /// `exit_status == None`(对端没送退出码)**不算成功**。这条守的是
    /// F57 的回退判定:算成功的话,一条根本没跑起来的 `rm -rf` 会被当成
    /// 「删干净了」,界面刷新后文件还在,用户完全不知道发生了什么。
    #[test]
    fn a_missing_exit_status_is_not_success() {
        let o = ExecOutcome { exit_status: None, stdout: Vec::new(), stderr: Vec::new() };
        assert!(!o.succeeded());
        let ok = ExecOutcome { exit_status: Some(0), stdout: Vec::new(), stderr: Vec::new() };
        assert!(ok.succeeded());
        let bad = ExecOutcome { exit_status: Some(1), stdout: Vec::new(), stderr: Vec::new() };
        assert!(!bad.succeeded());
    }
}
```

`crates/mullion-ssh/src/lib.rs` 加一行（放在既有 `pub mod` 列表里，按字母序）：

```rust
pub mod exec;
```

- [ ] **Step 2：跑单测**

Run: `cargo test -p mullion-ssh exec:: 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok. 4 passed`

- [ ] **Step 3：假 sshd 支持 exec（带「可配置拒绝」）**

`crates/mullion-ssh/tests/common/sftp_server.rs` 的 `Probe` 加一个字段：

```rust
#[derive(Default)]
pub struct Probe {
    pub seen: Vec<(&'static str, String)>,
    pub pty_requests: usize,
    /// exec 收到的命令行(原始字节)。F57 的转义守护靠它。
    pub execs: Vec<Vec<u8>>,
}
```

`crates/mullion-ssh/tests/common/mod.rs` 的 `SftpSshHandler` 加一个开关字段与 `exec_request`：

```rust
pub struct SftpSshHandler {
    channels: std::collections::HashMap<ChannelId, Channel<Msg>>,
    tree: Arc<std::sync::Mutex<sftp_server::Tree>>,
    probe: Arc<std::sync::Mutex<sftp_server::Probe>>,
    /// `false` = 像 sftp-only 账号那样**拒绝** exec(F57 回退路径的测试用)。
    allow_exec: bool,
}
```

在 `impl Handler for SftpSshHandler` 里加：

```rust
    /// 记下命令行;`allow_exec == false` 时像 `ForceCommand internal-sftp`
    /// 的账号那样直接拒(F57 的回退分支靠这条触发)。
    ///
    /// 允许执行时**只认 `rm -rf -- <路径…>` 这一种**,并真的在内存树上删掉。
    /// 起一个真 shell 来解析命令行既不可能也没必要 —— 我们要验的是
    /// 「转义对不对 + 回退判定对不对」,不是 shell 的实现。
    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.probe.lock().unwrap().execs.push(data.to_vec());
        if !self.allow_exec {
            session.channel_failure(channel)?;
            return Ok(());
        }
        session.channel_success(channel)?;
        let code = match parse_rm_rf(data) {
            Some(paths) => {
                let mut tree = self.tree.lock().unwrap();
                for p in paths {
                    remove_recursively(&mut tree, &p);
                }
                0
            }
            None => 127, // 命令认不出来 —— 与真 shell 的 "command not found" 同码
        };
        session.exit_status_request(channel, code)?;
        session.close(channel)?;
        Ok(())
    }
```

同文件末尾加两个辅助（**故意写得笨但正确**——测试基建出 bug 比生产代码更难查）：

```rust
/// 认 `rm -rf -- '<路径>' '<路径>'…`,把单引号字面量解回原始字节。
/// 认不出来返回 `None`(测试里就是「命令没跑成」)。
#[allow(dead_code)]
fn parse_rm_rf(cmd: &[u8]) -> Option<Vec<Vec<u8>>> {
    let prefix = b"rm -rf -- ";
    if !cmd.starts_with(prefix) {
        return None;
    }
    let mut rest = &cmd[prefix.len()..];
    let mut out = Vec::new();
    while !rest.is_empty() {
        if rest[0] == b' ' {
            rest = &rest[1..];
            continue;
        }
        if rest[0] != b'\'' {
            return None; // 没被引号包住 —— 转义漏了,这正是我们要抓的
        }
        let mut arg = Vec::new();
        let mut i = 1;
        loop {
            if i >= rest.len() {
                return None; // 引号没闭合
            }
            if rest[i] == b'\'' {
                // `'\''` 是「闭合 + 转义的单引号 + 重新开启」
                if rest[i..].starts_with(b"'\\''") {
                    arg.push(b'\'');
                    i += 4;
                    continue;
                }
                i += 1;
                break;
            }
            arg.push(rest[i]);
            i += 1;
        }
        out.push(arg);
        rest = &rest[i..];
    }
    Some(out)
}

/// 在内存树里递归删掉一条(目录连同子树)。
#[allow(dead_code)]
fn remove_recursively(tree: &mut sftp_server::Tree, path: &[u8]) {
    let children: Vec<Vec<u8>> = tree
        .get(path)
        .map(|v| v.iter().map(|n| n.name.clone()).collect())
        .unwrap_or_default();
    for name in children {
        let mut child = path.to_vec();
        if !child.ends_with(b"/") {
            child.push(b'/');
        }
        child.extend_from_slice(&name);
        remove_recursively(tree, &child);
    }
    tree.remove(path);
    let (dir, name) = sftp_server::split_last_pub(path);
    if let Some(v) = tree.get_mut(&dir) {
        v.retain(|n| n.name != name);
    }
}
```

`sftp_server.rs` 里把 `split_last` 暴露一个 pub 包装（`common/mod.rs` 是同 crate 的另一个模块，用得着）：

```rust
/// `split_last` 的对外包装 —— `common/mod.rs` 的 exec 模拟要用。
pub fn split_last_pub(path: &[u8]) -> (Vec<u8>, Vec<u8>) {
    split_last(path)
}
```

`spawn_sftp_server` 保持 `allow_exec: true`，再加一个显式拒绝的变体：

```rust
/// 起一个带 SFTP 的假 sshd。返回监听地址、探针、可写的内存树。
#[allow(dead_code)]
pub async fn spawn_sftp_server(tree: sftp_server::Tree) -> (/* 同前三元组 */) {
    spawn_sftp_server_with(tree, true).await
}

/// 像 sftp-only 账号那样**拒绝 exec** 的变体(F57 回退分支的测试用)。
#[allow(dead_code)]
pub async fn spawn_sftp_server_without_exec(tree: sftp_server::Tree) -> (/* 同前三元组 */) {
    spawn_sftp_server_with(tree, false).await
}
```

把原 `spawn_sftp_server` 的函数体整体挪进 `spawn_sftp_server_with(tree, allow_exec)`，构造 handler 时带上 `allow_exec`。

- [ ] **Step 4：跑，确认基建没坏**

Run: `cargo test -p mullion-ssh 2>&1 | grep -E "test result|FAILED|error\["`
Expected: 全 ok。

- [ ] **Step 5：提交**

```bash
git add crates/mullion-ssh/src/exec.rs crates/mullion-ssh/src/lib.rs crates/mullion-ssh/tests/common/
git commit -m "feat(ssh): 一次性 exec 通道 + shell 单引号转义,假 sshd 支持 exec (F57)"
```

---

## Task 4：递归删除（先 exec 后回退，链接不跟随）

**Files:**
- Create: `crates/mullion-ssh/src/remove_tree.rs`
- Modify: `crates/mullion-ssh/src/lib.rs`
- Modify: `crates/mullion-ssh/tests/sftp_write.rs`

- [ ] **Step 1：先写端到端测试**

追加到 `crates/mullion-ssh/tests/sftp_write.rs`：

```rust
use mullion_ssh::remove_tree::{remove_tree, RemoveReport};

fn nested_tree() -> Tree {
    let mut t = Tree::new();
    t.insert(b"/home/testuser".to_vec(), vec![Node::dir(b"box"), Node::dir(b"victim")]);
    t.insert(
        b"/home/testuser/box".to_vec(),
        vec![Node::file(b"f1", 1), Node::dir(b"sub"), Node::link(b"lnk", b"/home/testuser/victim")],
    );
    t.insert(b"/home/testuser/box/sub".to_vec(), vec![Node::file(b"deep.txt", 2)]);
    t.insert(b"/home/testuser/victim".to_vec(), vec![Node::file(b"precious.txt", 9)]);
    t
}

async fn conn_of(addr: std::net::SocketAddr) -> Arc<mullion_ssh::session::SshConnection> {
    Arc::new(establish(&cfg(addr), Arc::new(AcceptAll)).await.expect("connect"))
}

/// 快路径:exec 可用时走 `rm -rf`,**一条 SFTP 删除请求都不该发**。
#[tokio::test]
async fn a_recursive_delete_uses_the_exec_fast_path_when_it_is_allowed() {
    let (addr, probe, tree_h) = common::spawn_sftp_server(nested_tree()).await;
    let conn = conn_of(addr).await;
    let sftp = SftpClient::open(conn.clone()).await.expect("open sftp");

    let report = remove_tree(&sftp, &conn, &RemotePath::from_bytes(b"/home/testuser/box".to_vec()))
        .await
        .expect("递归删除");
    assert_eq!(report, RemoveReport::Exec, "exec 可用时该走快路径");

    let t = tree_h.lock().unwrap();
    assert!(!exists(&t, b"/home/testuser/box"), "整棵子树该没了");
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

    let report = remove_tree(&sftp, &conn, &RemotePath::from_bytes(b"/home/testuser/box".to_vec()))
        .await
        .expect("递归删除该回退成功");
    assert_eq!(report, RemoveReport::Sftp, "exec 被拒时该回退");

    let t = tree_h.lock().unwrap();
    assert!(!exists(&t, b"/home/testuser/box"), "回退路径也要真的删干净");
    assert!(!exists(&t, b"/home/testuser/box/sub"), "子目录也要删掉");
    let p = probe.lock().unwrap();
    assert!(!p.paths_for("rmdir").is_empty(), "回退路径必然发过 rmdir");
}

/// D17 最要命的一条:**删除绝不跟随符号链接**。搞错了就是把远端整个
/// 目标目录删了 —— 这条测试的存在本身就是它的理由。
///
/// 两条路径都要验:回退的 SFTP 递归(下面这条)与 exec 的 `rm -rf`
/// (`rm -rf` 对链接删的是链接本身,由假服务端的 `remove_recursively` 复现)。
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

        remove_tree(&sftp, &conn, &RemotePath::from_bytes(b"/home/testuser/box".to_vec()))
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

/// 转义:名字里带空格/引号/换行/`$`/反引号的目录也要删得掉,
/// 且服务端收到的命令行**解回来逐字节等于原路径**。
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

    assert!(!exists(&tree_h.lock().unwrap(), &key), "带怪名字的目录该真的没了");
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

    let bad = RemotePath::from_bytes("/home/testuser/\u{fffd}\u{fffd}".as_bytes().to_vec());
    let err = remove_tree(&sftp, &conn, &bad).await.expect_err("不该发得出去");
    assert!(matches!(err, SftpError::NonUtf8Name));

    let p = probe.lock().unwrap();
    assert!(p.execs.is_empty(), "一条 exec 都不该发: {:?}", p.execs);
    assert!(p.paths_for("remove").is_empty() && p.paths_for("rmdir").is_empty());
}
```

- [ ] **Step 2：跑，确认编译失败**

Run: `cargo test -p mullion-ssh --test sftp_write 2>&1 | head -10`
Expected: `unresolved import 'mullion_ssh::remove_tree'`

- [ ] **Step 3：实现**

新建 `crates/mullion-ssh/src/remove_tree.rs`：

```rust
//! 递归删除(F57)。**先试 `exec rm -rf`,被拒或失败则回退 SFTP 逐文件递归。**
//!
//! 两条路都要有的理由(设计 D17):
//! - 一律走 exec:sftp-only 账号(`ForceCommand internal-sftp` +
//!   `ChrootDirectory`)会拒绝 exec,功能在那种账号上直接残缺。
//! - 一律逐文件:删一个 `node_modules` 要等到天荒地老(每文件一个 RTT,
//!   高延迟代理链路上这是几十分钟对几秒的差别)。
//!
//! **绝不跟随符号链接**:列举用 `list_dir`(readdir = lstat 语义),
//! 遇到 `EntryKind::Symlink` 一律当叶子删掉,不进去。搞错了就是把远端
//! 整个目标目录删了 —— 这是本模块最重要的一条不变量。

use std::sync::Arc;

use crate::exec::{exec, shell_quote, ExecError};
use crate::session::SshConnection;
use crate::sftp::{EntryKind, RemotePath, SftpClient, SftpError};

/// 这一次删除实际走了哪条路。调用方用它写日志 / 断言,不影响正确性。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveReport {
    /// 走了 `exec rm -rf` 快路径。
    Exec,
    /// 回退到 SFTP 逐文件递归。
    Sftp,
}

/// 递归删掉一条(文件、链接或整棵目录树)。
///
/// `path` 发不出去(`as_wire` 被挡)时**一个请求都不发**,直接
/// `Err(SftpError::NonUtf8Name)` —— 拿一串替换字符去 `rm -rf` 是本项目
/// 能犯的最严重的错。
pub async fn remove_tree(
    sftp: &SftpClient,
    conn: &Arc<SshConnection>,
    path: &RemotePath,
) -> Result<RemoveReport, SftpError> {
    // **先过这道门**:后面两条路都不许再各自检查一遍(检查两遍就有
    // 一遍会被人改漏)。
    let _ = path.as_wire()?;

    match try_exec_rm(conn, path).await {
        Ok(true) => return Ok(RemoveReport::Exec),
        // 命令跑了但没成功(权限不足、路径不存在…):**不回退**,回退只会
        // 把同一个错误再犯一遍,而且是慢一千倍地犯。
        Ok(false) => {
            return Err(SftpError::Protocol(
                "远端 rm 命令执行失败(可能是权限不足)".into(),
            ))
        }
        // 对端拒绝执行命令 / 开不出 channel —— 这才是该回退的信号。
        Err(_) => {}
    }
    remove_via_sftp(sftp, path).await?;
    Ok(RemoveReport::Sftp)
}

/// 试 `rm -rf`。`Ok(true)` = 跑了且成功;`Ok(false)` = 跑了但失败;
/// `Err` = 压根没跑起来(该回退)。
async fn try_exec_rm(conn: &Arc<SshConnection>, path: &RemotePath) -> Result<bool, ExecError> {
    // `--` 终止选项解析:名字以 `-` 开头的目录(`-rf` 这种真的存在)
    // 不加它会被 rm 当成选项。
    let mut cmd = b"rm -rf -- ".to_vec();
    cmd.extend_from_slice(&shell_quote(path.as_bytes()));
    let out = exec(conn, cmd).await?;
    Ok(out.succeeded())
}

/// 回退路径:SFTP 逐文件递归。
///
/// 用显式栈而不是递归 `async fn` —— async 函数递归要装箱
/// (`Box::pin`),而且深目录会把栈上的 future 堆得很大。
async fn remove_via_sftp(sftp: &SftpClient, root: &RemotePath) -> Result<(), SftpError> {
    // 先看这一条是什么。**lstat 语义**:指向目录的链接必须当叶子处理。
    let root_kind = sftp.stat(root).await?.kind;
    if root_kind != EntryKind::Dir {
        return sftp.remove_file(root).await;
    }

    // 后序遍历:先把所有目录按「从深到浅」排好,叶子文件边走边删。
    let mut dirs_in_order: Vec<RemotePath> = Vec::new();
    let mut pending: Vec<RemotePath> = vec![root.clone()];
    while let Some(dir) = pending.pop() {
        let entries = sftp.list_dir(&dir).await?;
        dirs_in_order.push(dir.clone());
        for e in entries {
            // 名字发不出去的条目:整棵树都删不干净了,老实报错,
            // 不装作成功(rmdir 到时候会因为非空而失败,那个错误
            // 用户读不懂)。
            if !e.name.is_operable() {
                return Err(SftpError::NonUtf8Name);
            }
            let child = dir.join(e.name.as_bytes());
            match e.kind {
                // **链接一律当叶子** —— 不 `list_dir` 它,不进去(D17)。
                EntryKind::Dir => pending.push(child),
                _ => sftp.remove_file(&child).await?,
            }
        }
    }
    // 从深到浅删空目录:`dirs_in_order` 是入栈顺序(浅→深),倒过来即可。
    for dir in dirs_in_order.iter().rev() {
        sftp.remove_dir(dir).await?;
    }
    Ok(())
}
```

`crates/mullion-ssh/src/lib.rs` 加：

```rust
pub mod remove_tree;
```

- [ ] **Step 4：跑，确认通过**

Run: `cargo test -p mullion-ssh --test sftp_write 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok. 12 passed`

- [ ] **Step 5：变异验收（五个变异，逐个确认变红）**

```bash
cp crates/mullion-ssh/src/remove_tree.rs /tmp/rt.bak
```

| # | 变异 | 该变红的测试 |
|---|---|---|
| 1 | `EntryKind::Dir => pending.push(child)` 改成 `EntryKind::Dir \| EntryKind::Symlink => pending.push(child)`（跟随链接） | `a_recursive_delete_never_follows_a_symlink_into_the_target_directory` |
| 2 | `remove_tree` 开头的 `let _ = path.as_wire()?;` 删掉 | `a_non_operable_path_is_refused_by_recursive_delete_without_any_request` |
| 3 | `try_exec_rm` 里的 `shell_quote(...)` 换成 `path.as_bytes().to_vec()`（不转义） | `the_exec_fast_path_quotes_nasty_names_correctly` |
| 4 | `Err(_) => {}` 改成 `Err(e) => return Err(SftpError::Protocol(e.to_string()))`（不回退） | `a_recursive_delete_falls_back_to_sftp_when_exec_is_refused` |
| 5 | `remove_tree` 里删掉 `try_exec_rm` 那一整段，只留 SFTP 路径 | `a_recursive_delete_uses_the_exec_fast_path_when_it_is_allowed` |

每改一次跑 `cargo test -p mullion-ssh --test sftp_write`，确认**恰好**那一条（或那几条）变红。全绿说明零覆盖，补测试。改完还原：

```bash
cp /tmp/rt.bak crates/mullion-ssh/src/remove_tree.rs
```

- [ ] **Step 6：提交**

```bash
git add crates/mullion-ssh/src/remove_tree.rs crates/mullion-ssh/src/lib.rs crates/mullion-ssh/tests/sftp_write.rs
git commit -m "feat(ssh): 递归删除先 exec rm -rf 后回退 SFTP,链接一律不跟随 (F57/D17)"
```

---

## Task 5：多选模型（F54）

**Files:**
- Modify: `crates/mullion-app/src/files/state.rs`

**当前状态**：`PaneState.selected: Option<RemotePath>`（单选，存身份不存下标）。
**目标**：选择集 + 光标 + 锚点。三者的分工不能混：
- `cursor`：`↑`/`↓` 移动的那一行，也是「重命名 / 改权限」这类**单目标**操作的对象。
- `selected`：删除 / 将来的批量下载的对象。
- `anchor`：`Shift` 范围选择的起点。

- [ ] **Step 1：先写失败的测试**

在 `crates/mullion-app/src/files/state.rs` 的 `mod tests` 里追加：

```rust
    fn ready(names: &[&str]) -> PaneState {
        let mut s = state();
        s.entries = names.iter().map(|n| e(n, EntryKind::File)).collect();
        s.load = Load::Ready;
        s
    }

    fn rp(name: &str) -> RemotePath {
        RemotePath::from_bytes(name.as_bytes().to_vec())
    }

    /// 平点一行:清空原有选择,只留这一条,光标与锚点都落到它上面。
    #[test]
    fn a_plain_click_selects_exactly_one_row() {
        let mut s = ready(&["a", "b", "c"]);
        s.click_row(&rp("a"), false, false);
        s.click_row(&rp("c"), false, false);
        assert_eq!(s.selected_paths(), vec![rp("c")], "平点该只剩最后点的那条");
        assert_eq!(s.cursor.as_ref(), Some(&rp("c")));
        assert_eq!(s.anchor.as_ref(), Some(&rp("c")));
    }

    /// Ctrl 点:切换那一条的选中态,其余不动。
    #[test]
    fn a_ctrl_click_toggles_one_row_without_clearing_the_rest() {
        let mut s = ready(&["a", "b", "c"]);
        s.click_row(&rp("a"), false, false);
        s.click_row(&rp("c"), true, false);
        let mut got = s.selected_paths();
        got.sort_by(|x, y| x.as_bytes().cmp(y.as_bytes()));
        assert_eq!(got, vec![rp("a"), rp("c")]);
        // 再 Ctrl 点一次 c 应当取消它
        s.click_row(&rp("c"), true, false);
        assert_eq!(s.selected_paths(), vec![rp("a")]);
    }

    /// Shift 点:从锚点到这一条**闭区间**全选,按当前**可见行序**算 ——
    /// 不是按 `entries` 的存储顺序,也不是按字节序。点过列头重排之后
    /// 「从这儿到那儿」指的是用户眼里看到的那一段。
    #[test]
    fn a_shift_click_selects_the_inclusive_visible_range() {
        let mut s = ready(&["a", "b", "c", "d"]);
        s.click_row(&rp("b"), false, false); // 锚点 = b
        s.click_row(&rp("d"), false, true);
        let mut got = s.selected_paths();
        got.sort_by(|x, y| x.as_bytes().cmp(y.as_bytes()));
        assert_eq!(got, vec![rp("b"), rp("c"), rp("d")]);
        assert_eq!(s.anchor.as_ref(), Some(&rp("b")), "Shift 不该挪动锚点");
        assert_eq!(s.cursor.as_ref(), Some(&rp("d")), "光标跟着走");
    }

    /// 反向 Shift(从下往上点)同样是闭区间。
    #[test]
    fn a_backwards_shift_click_selects_the_same_range() {
        let mut s = ready(&["a", "b", "c", "d"]);
        s.click_row(&rp("d"), false, false);
        s.click_row(&rp("b"), false, true);
        let mut got = s.selected_paths();
        got.sort_by(|x, y| x.as_bytes().cmp(y.as_bytes()));
        assert_eq!(got, vec![rp("b"), rp("c"), rp("d")]);
    }

    /// 隐藏文件被过滤掉时,Shift 范围**不该把看不见的那条也选上** ——
    /// 用户选的是他看得见的那一段,删除确认框里冒出一条他从没见过的
    /// `.env` 是最坏的一种意外。
    #[test]
    fn a_shift_range_never_picks_up_rows_that_are_filtered_out() {
        let mut s = state();
        s.entries = vec![
            e("a", EntryKind::File),
            e(".secret", EntryKind::File),
            e("c", EntryKind::File),
        ];
        s.load = Load::Ready;
        s.show_hidden = false;
        s.click_row(&rp("a"), false, false);
        s.click_row(&rp("c"), false, true);
        let got = s.selected_paths();
        assert!(
            !got.contains(&rp(".secret")),
            "被过滤掉的隐藏项不该混进范围选择: {got:?}"
        );
        assert_eq!(got.len(), 2);
    }

    /// 换目录(`begin_load`)必须把选择集、光标、锚点一起清干净 ——
    /// 留着的话,新目录里恰好同名的文件会凭空「已选中」,而删除是不可逆的。
    #[test]
    fn navigating_away_clears_the_whole_selection() {
        let mut s = ready(&["a", "b"]);
        s.click_row(&rp("a"), false, false);
        s.click_row(&rp("b"), true, false);
        s.begin_load(rp("/elsewhere"));
        assert!(s.selected.is_empty(), "换目录该清空选择集");
        assert!(s.cursor.is_none());
        assert!(s.anchor.is_none());
    }

    /// 刷新(`accept`)之后,**已经不在列表里的选中项要被丢掉**。
    /// 留着的话,删除确认框会列出一个远端已经没有的路径,用户点确认后
    /// 收到一条 NoSuchFile,完全不知道自己删的是什么。
    #[test]
    fn a_refresh_drops_selections_that_no_longer_exist() {
        let mut s = ready(&["a", "b"]);
        s.click_row(&rp("a"), false, false);
        s.click_row(&rp("b"), true, false);
        let seq = s.begin_load(rp("/home/u"));
        // begin_load 已经清了,这里手工放回去,模拟「刷新当前目录」的语义
        s.selected.insert(rp("a"));
        s.selected.insert(rp("b"));
        s.cursor = Some(rp("b"));
        assert!(s.accept(seq, Ok(vec![e("a", EntryKind::File)])));
        assert_eq!(s.selected_paths(), vec![rp("a")], "没了的那条该被丢掉");
        assert!(s.cursor.is_none(), "光标指着的那条没了,光标也该清掉");
    }
```

- [ ] **Step 2：跑，确认编译失败**

Run: `cargo test -p mullion-app files::state 2>&1 | head -20`
Expected: `no method named 'click_row'` / `no field 'cursor'` 等。

- [ ] **Step 3：改 `PaneState`**

`crates/mullion-app/src/files/state.rs`——把 `selected: Option<RemotePath>` 换成三个字段：

```rust
    /// **选中集**(F54 多选)。存的是身份(`RemotePath`)不是下标 ——
    /// 存下标会错行:点一次列头 `entries` 就重排了,`show_hidden` 一切
    /// 过滤结果也变了,下标却纹丝不动。本切片开始,操作真的接到选中项上,
    /// 错行就是**对错文件下手**,而删除不可逆。
    ///
    /// 用 `BTreeSet` 不是 `Vec`:去重是硬需求(Ctrl 点两次同一条),
    /// 而 `RemotePath` 的 `Ord` 是字节序、正好够当集合键。
    /// **顺序无意义** —— 要按可见行序拿,用 [`PaneState::selected_paths`]。
    pub selected: std::collections::BTreeSet<RemotePath>,
    /// 光标行:`↑`/`↓` 移动的那一条,也是**单目标**操作(重命名、改权限)
    /// 的对象。与 `selected` 分开是必须的 —— 多选了 5 条时「重命名」该改
    /// 哪一条没有答案,界面上就得有一条明确的「当前行」。
    pub cursor: Option<RemotePath>,
    /// `Shift` 范围选择的起点。平点 / Ctrl 点会把它挪到点中那条;
    /// Shift 点**不挪**(否则连续 Shift 点会变成一段一段接龙)。
    pub anchor: Option<RemotePath>,
```

`PaneState::new` 里对应改成：

```rust
            selected: std::collections::BTreeSet::new(),
            cursor: None,
            anchor: None,
```

`begin_load` 里 `self.selected = None;` 改成：

```rust
        self.clear_selection();
```

`accept` 的 `Ok` 分支在 `self.entries = v;` **之后**加一句剪枝：

```rust
                self.entries = v;
                self.load = Load::Ready;
                // 刷新后已经不存在的条目要从选择集里剔掉 —— 留着的话,
                // 删除确认框会列出远端已经没有的路径。
                self.prune_selection();
```

在 `impl PaneState` 里追加：

```rust
    /// 清空选择集、光标与锚点。换目录时必须整套清 —— 只清一部分的话,
    /// 新目录里同名的文件会凭空带着上一个目录的选中态。
    pub fn clear_selection(&mut self) {
        self.selected.clear();
        self.cursor = None;
        self.anchor = None;
    }

    /// 把已经不在 `entries` 里的选中项 / 光标 / 锚点剔掉。
    fn prune_selection(&mut self) {
        let alive: std::collections::BTreeSet<&RemotePath> =
            self.entries.iter().map(|e| &e.name).collect();
        self.selected.retain(|p| alive.contains(p));
        if self.cursor.as_ref().is_some_and(|p| !alive.contains(p)) {
            self.cursor = None;
        }
        if self.anchor.as_ref().is_some_and(|p| !alive.contains(p)) {
            self.anchor = None;
        }
    }

    /// 某一行是不是选中的(渲染每行都要问一次,所以是集合查找不是线性扫)。
    pub fn is_selected(&self, name: &RemotePath) -> bool {
        self.selected.contains(name)
    }

    /// 选中项,**按当前可见行序**给出。删除确认框列路径、将来的批量下载
    /// 排队都用它 —— 用户看到的顺序和我们处理的顺序一致,对账才对得上。
    pub fn selected_paths(&self) -> Vec<RemotePath> {
        self.rows()
            .into_iter()
            .filter(|e| self.selected.contains(&e.name))
            .map(|e| e.name.clone())
            .collect()
    }

    /// 点了一行。`ctrl` = 切换单条,`shift` = 从锚点到这里的闭区间,
    /// 都不按 = 只选这一条。
    ///
    /// **范围按可见行序算**(`rows()`),不是 `entries` 的存储序:点过列头
    /// 重排、或关着隐藏文件时,用户说的「从这儿到那儿」指的是他眼里看到的
    /// 那一段。按存储序算会把一条他从没见过的 `.env` 选进删除列表。
    pub fn click_row(&mut self, name: &RemotePath, ctrl: bool, shift: bool) {
        if shift {
            if let Some(anchor) = self.anchor.clone() {
                let order: Vec<RemotePath> =
                    self.rows().into_iter().map(|e| e.name.clone()).collect();
                let (Some(a), Some(b)) = (
                    order.iter().position(|p| *p == anchor),
                    order.iter().position(|p| p == name),
                ) else {
                    // 锚点已经不在可见行里(被过滤掉 / 刷新后没了)——
                    // 退化成平点,不猜用户想选哪一段。
                    self.select_only(name);
                    return;
                };
                let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
                self.selected = order[lo..=hi].iter().cloned().collect();
                self.cursor = Some(name.clone());
                // 锚点**不动** —— 动了的话连续 Shift 点会变成一段接一段。
                return;
            }
            self.select_only(name);
            return;
        }
        if ctrl {
            if !self.selected.remove(name) {
                self.selected.insert(name.clone());
            }
            self.cursor = Some(name.clone());
            self.anchor = Some(name.clone());
            return;
        }
        self.select_only(name);
    }

    /// 只选这一条,光标与锚点都落到它上面。
    pub fn select_only(&mut self, name: &RemotePath) {
        self.selected.clear();
        self.selected.insert(name.clone());
        self.cursor = Some(name.clone());
        self.anchor = Some(name.clone());
    }
```

- [ ] **Step 4：修 D1 留下的两处调用点**

`crates/mullion-app/src/ui/files_panel.rs` 的 `show()` 里：

```rust
    let selected = state.selected.clone();
```
改成不再需要克隆整个集合的写法——`rows` 借着 `&state.entries`，闭包里不能再借 `&mut state`，所以仍要先把要用的信息取出来。改成：

```rust
    // `rows` 借着 `&state.entries` 不放,闭包里不能再借一次 `&mut state`——
    // 把这一帧要用到的选中集先克隆出来(条数是可见行级别,不是两万项)。
    let selected = state.selected.clone();
    let mut clicked: Option<(mullion_ssh::sftp::RemotePath, bool, bool)> = None;
```

行渲染处：

```rust
                let resp = row(ui, t, e, show_owner, selected.contains(&e.name));
                if resp.clicked() {
                    let m = ui.input(|i| i.modifiers);
                    clicked = Some((e.name.clone(), m.command, m.shift));
                }
```

闭包之后：

```rust
    if let Some((name, ctrl, shift)) = clicked {
        state.click_row(&name, ctrl, shift);
    }
```

`crates/mullion-app/src/app.rs` 的 `move_panel_selection` —— 把 `state.selected` 的读写改成 `state.cursor`，并在移动后同步成单选：

```rust
        let Some(next) = next_panel_selection_index(&rows, state.cursor.as_ref(), delta) else {
            return;
        };
        let name = rows[next].name.clone();
        drop(rows);
        state.select_only(&name);
```

（`next_panel_selection_index` 的签名不变——它收的就是 `Option<&RemotePath>`。）

`crates/mullion-app/src/app.rs` 的 `handle_panel_key` 里 Enter 分支取「当前行」的地方，把 `state.selected` 换成 `state.cursor`：

```rust
                let (column, state) = tab.content.files_panel().active_state();
                let Some(cur) = state.cursor.as_ref() else {
                    return;
                };
```
（保持原有结构，只把字段名从 `selected` 换成 `cursor`。）

`crates/mullion-app/src/files/state.rs` 里 D1 的 `re_sorting_keeps_the_selection_on_the_same_file` 测试改成用新 API：

```rust
        s.select_only(&picked);
        ...
        assert!(s.is_selected(&picked), "选中跟着文件走,不跟着行号走");
```

- [ ] **Step 5：跑，确认通过**

Run: `cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|error\["`
Expected: 全 ok。

- [ ] **Step 6：变异验收**

| # | 变异 | 该变红的测试 |
|---|---|---|
| 1 | `click_row` 的 shift 分支里把 `order` 换成 `self.entries.iter().map(...)`（按存储序而非可见行序） | `a_shift_range_never_picks_up_rows_that_are_filtered_out` |
| 2 | shift 分支末尾加 `self.anchor = Some(name.clone());` | `a_shift_click_selects_the_inclusive_visible_range` |
| 3 | `begin_load` 里的 `clear_selection()` 改成只 `self.selected.clear()` | `navigating_away_clears_the_whole_selection` |
| 4 | `accept` 里的 `prune_selection()` 删掉 | `a_refresh_drops_selections_that_no_longer_exist` |
| 5 | `click_row` 的 ctrl 分支改成 `self.select_only(name)` | `a_ctrl_click_toggles_one_row_without_clearing_the_rest` |

- [ ] **Step 7：提交**

```bash
git add crates/mullion-app/src/files/state.rs crates/mullion-app/src/ui/files_panel.rs crates/mullion-app/src/app.rs
git commit -m "feat(app): 文件面板多选(选中集+光标+锚点),范围按可见行序 (F54)"
```

---

## Task 6：右键菜单 + `FileAction` 扩展

**Files:**
- Modify: `crates/mullion-app/src/ui/files_panel.rs`

**设计约束**：写操作**只在远端栏**（D5：本地栏只导航与传输端点，不提供本地删除/重命名/新建）。本地栏的右键菜单只有「在资源管理器中打开」。

- [ ] **Step 1：先写失败的测试**

在 `files_panel.rs` 的 `mod tests` 里追加：

```rust
    /// 在渲染结果里找所有文本(两帧,egui Panel 首帧 fade_in 只记 Noop)。
    fn texts_of(run: impl Fn(&egui::Context)) -> Vec<String> {
        let ctx = egui::Context::default();
        let mut texts = Vec::new();
        for _ in 0..2 {
            texts.clear();
            let out = ctx.run(egui::RawInput::default(), |ctx| run(ctx));
            for shape in out.shapes.iter() {
                if let egui::epaint::Shape::Text(ts) = &shape.shape {
                    texts.push(ts.galley.text().to_owned());
                }
            }
        }
        texts
    }

    /// D5:**本地栏没有写操作入口**。菜单项的存在与否是纯结构的事,
    /// 用 `menu_items_for` 这个纯函数验,不必真去点开右键菜单
    /// (egui 的 `context_menu` 要一次右键 + 一帧才展开,测起来又脆又慢)。
    #[test]
    fn the_local_column_never_offers_a_write_operation() {
        let remote = menu_items_for(PanelColumn::Remote, true);
        let local = menu_items_for(PanelColumn::Local, true);
        for ask in [FileAsk::NewDir, FileAsk::Rename, FileAsk::Delete, FileAsk::Chmod] {
            assert!(
                remote.iter().any(|(_, a)| *a == MenuItem::Ask(ask)),
                "远端栏该有 {ask:?}"
            );
            assert!(
                !local.iter().any(|(_, a)| *a == MenuItem::Ask(ask)),
                "本地栏不该出现 {ask:?}(D5:本地文件管理外包给资源管理器)"
            );
        }
        assert!(
            local.iter().any(|(_, a)| *a == MenuItem::OpenInExplorer),
            "本地栏该有「在资源管理器中打开」"
        );
    }

    /// 没有光标行时,单目标操作(重命名 / 改权限)必须不可用 ——
    /// 给一个「点了没反应」的菜单项比不给更让人困惑。
    #[test]
    fn single_target_operations_are_absent_without_a_cursor_row() {
        let items = menu_items_for(PanelColumn::Remote, false);
        assert!(!items.iter().any(|(_, a)| *a == MenuItem::Ask(FileAsk::Rename)));
        assert!(!items.iter().any(|(_, a)| *a == MenuItem::Ask(FileAsk::Chmod)));
        assert!(!items.iter().any(|(_, a)| *a == MenuItem::Ask(FileAsk::Delete)));
        // 「新建文件夹」不需要选中任何东西 —— 空目录里也得能建。
        assert!(items.iter().any(|(_, a)| *a == MenuItem::Ask(FileAsk::NewDir)));
    }
```

- [ ] **Step 2：跑，确认编译失败**

Run: `cargo test -p mullion-app files_panel 2>&1 | head -10`
Expected: `cannot find function 'menu_items_for'`

- [ ] **Step 3：实现**

`files_panel.rs` 顶部的 `FileAction` 扩展（**加变体，不改既有的**）：

```rust
/// 用户在这一栏里做了什么。app 侧据此发异步请求。
#[derive(Debug, Clone, PartialEq)]
pub enum FileAction {
    /// 进这个目录(双击目录 / 跟随链接 / 点书签 / 点路径面包屑)。
    Goto(mullion_ssh::sftp::RemotePath),
    /// 回上一级。
    Up,
    /// 刷新当前目录。
    Refresh,
    /// 切隐藏文件显示。
    ToggleHidden,
    /// D2:请求**打开一个对话框**。真正的写操作要等用户在对话框里确认之后,
    /// 由 `UiActions::files_op` 发出 —— 右键点一下就把远端文件删了这种事
    /// 不该存在。
    Ask(FileAsk),
    /// D5:本地栏专属 —— 用系统文件管理器打开当前目录。
    OpenInExplorer,
}

/// 要打开哪个对话框。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAsk {
    /// 在当前目录里新建文件夹。**不需要选中任何东西**。
    NewDir,
    /// 重命名**光标行**(单目标)。
    Rename,
    /// 删除**选中集**(可多条)。
    Delete,
    /// 改**光标行**的权限(单目标)。
    Chmod,
}

/// 右键菜单里的一项。抽成枚举(而不是直接在渲染里写按钮)是为了让
/// 「哪些项该出现」能脱离 egui 单测 —— egui 的 `context_menu` 要一次
/// 右键 + 一帧才展开,在测试里驱动它又脆又慢,而「本地栏不许出现删除」
/// 恰恰是这一片最不能出错的一条。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuItem {
    Ask(FileAsk),
    OpenInExplorer,
    Refresh,
}

/// 这一栏此刻该有哪些右键菜单项。
///
/// - `column`:远端栏才有写操作(设计 D5)。
/// - `has_cursor`:有没有光标行。没有就不给单目标操作 ——
///   给一个点了没反应的菜单项比不给更让人困惑。
pub fn menu_items_for(column: PanelColumn, has_cursor: bool) -> Vec<(&'static str, MenuItem)> {
    let mut out: Vec<(&'static str, MenuItem)> = Vec::new();
    if column == PanelColumn::Remote {
        out.push(("新建文件夹…", MenuItem::Ask(FileAsk::NewDir)));
        if has_cursor {
            out.push(("重命名…", MenuItem::Ask(FileAsk::Rename)));
            out.push(("属性(权限)…", MenuItem::Ask(FileAsk::Chmod)));
            out.push(("删除…", MenuItem::Ask(FileAsk::Delete)));
        }
    } else {
        out.push(("在资源管理器中打开", MenuItem::OpenInExplorer));
    }
    out.push(("刷新", MenuItem::Refresh));
    out
}
```

`show()` 的签名加一个 `column: PanelColumn` 参数（`sidebar`/`content` 两个调用点各传自己那一栏），并在 `ScrollArea` 之后挂右键菜单：

```rust
    // 右键菜单挂在整栏的响应上(而不是每一行):在空白处右键也要能
    // 「新建文件夹」/「刷新」,那是用户在一个空目录里唯一的入口。
    let area = ui.interact(
        ui.max_rect(),
        ui.id().with(("files-menu", id)),
        egui::Sense::click(),
    );
    let mut menu_hit = None;
    area.context_menu(|ui| {
        annotate::mark(ui.ctx(), format!("文件面板/{id}/右键菜单"), ui.max_rect());
        for (label, item) in menu_items_for(column, state.cursor.is_some()) {
            if ui.button(label).clicked() {
                menu_hit = Some(item);
                ui.close_menu();
            }
        }
    });
    if let Some(item) = menu_hit {
        action = Some(match item {
            MenuItem::Ask(a) => FileAction::Ask(a),
            MenuItem::OpenInExplorer => FileAction::OpenInExplorer,
            MenuItem::Refresh => FileAction::Refresh,
        });
    }
```

`sidebar()` / `content()` 里四个 `show(...)` 调用点各加实参：远端两处传 `PanelColumn::Remote`，本地两处传 `PanelColumn::Local`。

- [ ] **Step 4：跑，确认通过**

Run: `cargo test -p mullion-app files_panel 2>&1 | grep -E "test result|FAILED"`
Expected: 全 ok。

- [ ] **Step 5：提交**

```bash
git add crates/mullion-app/src/ui/files_panel.rs
git commit -m "feat(ui): 文件面板右键菜单,写操作仅远端栏 (F54/D5)"
```

---

## Task 7：四个对话框

**Files:**
- Create: `crates/mullion-app/src/ui/files_dialog.rs`
- Modify: `crates/mullion-app/src/ui/mod.rs`

- [ ] **Step 1：先写失败的测试（放在新文件的 `mod tests` 里）**

新建 `crates/mullion-app/src/ui/files_dialog.rs`：

```rust
//! 远端写操作的四个对话框(F54/D17/D21):新建文件夹、重命名、删除确认、权限。
//!
//! **一律要用户确认之后才动远端**。右键点一下就删文件这种事不该存在,
//! 而删除在 SFTP 上**没有回收站、不可逆**(设计 D17)。
//!
//! 危险措辞按 F119 表单规范:列出「将删除 N 个文件 / M 个目录」+ 完整远端
//! 路径,按钮写「删除」不写「确定」——「确定」在一个列着 40 条路径的框里
//! 说明不了用户到底确定了什么。

use mullion_ssh::sftp::{EntryKind, RemotePath};

use crate::theme::{self, Theme};

/// 当前开着哪个对话框。`None` = 没开。挂在 `UiState` 上(与 `pending_delete`
/// 同一套做法):egui 闭包借不到 `&mut App`,意图必须落在状态里。
#[derive(Debug, Clone, PartialEq)]
pub enum FilesDialog {
    NewDir {
        /// 在哪个目录里建。
        parent: RemotePath,
        /// 输入框内容。
        name: String,
    },
    Rename {
        /// 完整的原路径。
        from: RemotePath,
        /// 输入框内容(只是**名字**,不含目录)。
        name: String,
    },
    Delete {
        /// 要删的完整路径 + 它是不是目录(确认文案要分开数)。
        targets: Vec<(RemotePath, bool)>,
    },
    Chmod {
        path: RemotePath,
        /// 九宫格当前值(低 9 位)。
        mode: u32,
    },
}

/// 用户在对话框里**确认**之后要执行的写操作。到这一步就没有回头路了 ——
/// `app.rs` 收到它就直接发请求。
#[derive(Debug, Clone, PartialEq)]
pub enum FileOp {
    /// 完整的目标路径(已经拼好,`app.rs` 不再拼一次 —— 拼两遍就有一遍会错)。
    NewDir(RemotePath),
    Rename { from: RemotePath, to: RemotePath },
    /// 逐条删。目录走递归删除,文件走 remove。**顺序即用户看到的顺序**。
    Delete { targets: Vec<(RemotePath, bool)> },
    Chmod { path: RemotePath, mode: u32 },
}

/// 删除确认框的那句话。抽成纯函数是因为它是**这一片唯一一句会直接导致
/// 数据丢失的文案** —— 数错了(比如把目录也算进"文件")用户就会低估后果。
pub fn delete_summary(targets: &[(RemotePath, bool)]) -> String {
    let dirs = targets.iter().filter(|(_, is_dir)| *is_dir).count();
    let files = targets.len() - dirs;
    match (files, dirs) {
        (0, 0) => "没有选中任何条目".to_string(),
        (f, 0) => format!("将删除 {f} 个文件"),
        (0, d) => format!("将删除 {d} 个目录(连同其中全部内容)"),
        (f, d) => format!("将删除 {f} 个文件、{d} 个目录(连同其中全部内容)"),
    }
}

/// 校验一个用户输入的**名字段**(新建 / 重命名共用)。
/// 返回 `Err` 时对话框的确认按钮置灰并显示这条原因。
///
/// 这里挡的是**会打到别的路径上**的输入,不是「不好看的名字」:
/// - 空 / 全空白:拼出来是父目录本身。
/// - 含 `/`:`RemotePath::join` 不做归一化(见其文档),`a/../../etc` 会
///   真的打到 `/etc` 上去。这是本函数存在的**全部理由**。
/// - `.` 与 `..`:同上,而且服务端多半直接回 Failure。
pub fn validate_name(name: &str) -> Result<(), &'static str> {
    let t = name.trim();
    if t.is_empty() {
        return Err("名字不能为空");
    }
    if name.contains('/') {
        return Err("名字里不能有 /(那会跳到另一个目录去)");
    }
    if t == "." || t == ".." {
        return Err("不能用 . 或 .. 当名字");
    }
    Ok(())
}

/// 九宫格 → 八进制。位序与 `files::perm_string` 画出来的一致:
/// 属主 rwx、属组 rwx、其他 rwx,从高位到低位。
pub fn mode_from_bits(bits: [bool; 9]) -> u32 {
    let mut m = 0;
    for (i, on) in bits.iter().enumerate() {
        if *on {
            m |= 1 << (8 - i);
        }
    }
    m
}

/// 八进制 → 九宫格。`mode_from_bits` 的逆。
pub fn bits_from_mode(mode: u32) -> [bool; 9] {
    let mut out = [false; 9];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = mode & (1 << (8 - i)) != 0;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rp(s: &str) -> RemotePath {
        RemotePath::from_bytes(s.as_bytes().to_vec())
    }

    /// 文件和目录要**分开数**。混着数的话,「将删除 3 个文件」后面跟着
    /// 一个装了两千个文件的目录,用户完全低估了后果 —— 而这一步不可逆。
    #[test]
    fn the_delete_summary_counts_files_and_directories_separately() {
        let s = delete_summary(&[(rp("/a"), false), (rp("/b"), true), (rp("/c"), false)]);
        assert!(s.contains("2 个文件"), "实际: {s}");
        assert!(s.contains("1 个目录"), "实际: {s}");
        assert!(
            s.contains("连同其中全部内容"),
            "有目录时必须说清是连内容一起删: {s}"
        );
    }

    /// 只有文件时不该冒出「0 个目录」这种话。
    #[test]
    fn the_delete_summary_does_not_mention_a_category_that_is_empty() {
        let s = delete_summary(&[(rp("/a"), false)]);
        assert_eq!(s, "将删除 1 个文件");
        let d = delete_summary(&[(rp("/a"), true)]);
        assert!(!d.contains("文件"), "只有目录时不该提文件: {d}");
    }

    /// 名字里有 `/` 必须挡下。`RemotePath::join` **不做归一化**(见其文档),
    /// 放过去的话 `../../etc/passwd` 会真的打到 `/etc/passwd` 上 ——
    /// 在「重命名」上这等于把远端系统文件改名。
    #[test]
    fn a_name_containing_a_slash_is_refused() {
        assert!(validate_name("a/b").is_err());
        assert!(validate_name("../../etc").is_err());
        assert!(validate_name("..").is_err());
        assert!(validate_name(".").is_err());
        assert!(validate_name("   ").is_err());
        assert!(validate_name("").is_err());
    }

    /// 正常名字(含中文、空格、点)必须放行 —— 判据写宽了会让
    /// 「我的 文档.txt」这种再普通不过的名字建不出来。
    #[test]
    fn ordinary_names_including_chinese_and_spaces_are_accepted() {
        assert!(validate_name("说明.md").is_ok());
        assert!(validate_name("my file.txt").is_ok());
        assert!(validate_name(".hidden").is_ok());
        assert!(validate_name("a.b.c").is_ok());
    }

    /// 九宫格与八进制必须互为逆运算,且位序与 `perm_string` 画出来的一致。
    /// 位序反了的症状是:用户勾「属主可写」,实际改的是「其他人可写」——
    /// 一个安全事故,而且界面上看不出来。
    #[test]
    fn the_permission_grid_round_trips_and_matches_the_rendered_order() {
        for mode in [0o000, 0o644, 0o755, 0o600, 0o777, 0o111] {
            assert_eq!(mode_from_bits(bits_from_mode(mode)), mode, "mode={mode:o}");
        }
        // 第 0 格 = 属主 r,对应 0o400。
        let mut bits = [false; 9];
        bits[0] = true;
        assert_eq!(mode_from_bits(bits), 0o400);
        // 第 8 格 = 其他 x,对应 0o001。
        let mut bits = [false; 9];
        bits[8] = true;
        assert_eq!(mode_from_bits(bits), 0o001);
        // 与渲染出来的字符串对齐:0o640 该画成 rw-r-----
        assert_eq!(crate::files::perm_string(0o640), "rw-r-----");
        let b = bits_from_mode(0o640);
        assert_eq!(
            (b[0], b[1], b[2], b[3], b[4], b[5]),
            (true, true, false, true, false, false)
        );
    }
}
```

- [ ] **Step 2：跑，确认通过（纯函数先绿）**

Run: `cargo test -p mullion-app files_dialog 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok. 5 passed`（先只跑纯函数部分；`show` 还没写）

`crates/mullion-app/src/ui/mod.rs` 加模块声明（与既有 `pub mod files_panel;` 并列）：

```rust
pub mod files_dialog;
```

- [ ] **Step 3：写渲染函数**

在 `files_dialog.rs` 的 `#[cfg(test)]` **之前**追加：

```rust
/// 画当前开着的那个对话框。返回用户确认的写操作(没确认就是 `None`)。
///
/// `dialog` 传 `&mut Option<..>`:用户点「取消」或确认之后要把它清成
/// `None`,这一步必须在这里做 —— 交给调用方做的话,总有一条分支会漏。
pub fn show(ctx: &egui::Context, t: &Theme, dialog: &mut Option<FilesDialog>) -> Option<FileOp> {
    let Some(d) = dialog.as_mut() else {
        return None;
    };
    let mut op = None;
    let mut close = false;

    match d {
        FilesDialog::NewDir { parent, name } => {
            let parent_disp = parent.display().to_string();
            egui::Window::new("新建文件夹")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    crate::ui::annotate::mark(ui.ctx(), "新建文件夹对话框", ui.max_rect());
                    ui.label(
                        egui::RichText::new(format!("位置:{parent_disp}"))
                            .color(theme::c32(t.fg_dim)),
                    );
                    ui.add(egui::TextEdit::singleline(name).desired_width(260.0));
                    let valid = validate_name(name);
                    if let Err(why) = valid {
                        ui.colored_label(theme::c32(t.danger), why);
                    }
                    ui.horizontal(|ui| {
                        if ui.add_enabled(valid.is_ok(), egui::Button::new("新建")).clicked() {
                            op = Some(FileOp::NewDir(parent.join(name.trim().as_bytes())));
                            close = true;
                        }
                        if ui.button("取消").clicked() {
                            close = true;
                        }
                    });
                });
        }
        FilesDialog::Rename { from, name } => {
            let from_disp = from.display().to_string();
            egui::Window::new("重命名")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    crate::ui::annotate::mark(ui.ctx(), "重命名对话框", ui.max_rect());
                    ui.label(
                        egui::RichText::new(format!("原名:{from_disp}"))
                            .color(theme::c32(t.fg_dim)),
                    );
                    ui.add(egui::TextEdit::singleline(name).desired_width(260.0));
                    let valid = validate_name(name);
                    if let Err(why) = valid {
                        ui.colored_label(theme::c32(t.danger), why);
                    }
                    ui.horizontal(|ui| {
                        if ui.add_enabled(valid.is_ok(), egui::Button::new("重命名")).clicked() {
                            let to = from.parent().join(name.trim().as_bytes());
                            op = Some(FileOp::Rename { from: from.clone(), to });
                            close = true;
                        }
                        if ui.button("取消").clicked() {
                            close = true;
                        }
                    });
                });
        }
        FilesDialog::Delete { targets } => {
            egui::Window::new("删除")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    crate::ui::annotate::mark(ui.ctx(), "删除确认对话框", ui.max_rect());
                    ui.colored_label(theme::c32(t.danger), delete_summary(targets));
                    // 「没有回收站」必须写在框里。用户在本地删东西是有后悔药的,
                    // 那个心智模型会被原样带到这儿来(设计 D17)。
                    ui.label(
                        egui::RichText::new("远端删除不可逆,没有回收站。")
                            .color(theme::c32(t.fg_dim)),
                    );
                    egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                        for (p, is_dir) in targets.iter() {
                            let mark = if *is_dir { "[目录] " } else { "" };
                            ui.label(format!("{mark}{}", p.display()));
                        }
                    });
                    ui.horizontal(|ui| {
                        // 按钮写「删除」不写「确定」(F119 危险措辞)——
                        // 「确定」在一个列着 40 条路径的框里说明不了用户到底
                        // 确定了什么。
                        if ui.button(egui::RichText::new("删除").color(theme::c32(t.danger))).clicked() {
                            op = Some(FileOp::Delete { targets: targets.clone() });
                            close = true;
                        }
                        if ui.button("取消").clicked() {
                            close = true;
                        }
                    });
                });
        }
        FilesDialog::Chmod { path, mode } => {
            let path_disp = path.display().to_string();
            egui::Window::new("属性")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    crate::ui::annotate::mark(ui.ctx(), "权限对话框", ui.max_rect());
                    ui.label(egui::RichText::new(&path_disp).color(theme::c32(t.fg_dim)));
                    let mut bits = bits_from_mode(*mode);
                    egui::Grid::new("chmod-grid").show(ui, |ui| {
                        ui.label("");
                        ui.label("读");
                        ui.label("写");
                        ui.label("执行");
                        ui.end_row();
                        for (row, who) in ["属主", "属组", "其他"].iter().enumerate() {
                            ui.label(*who);
                            for col in 0..3 {
                                ui.checkbox(&mut bits[row * 3 + col], "");
                            }
                            ui.end_row();
                        }
                    });
                    *mode = mode_from_bits(bits);
                    ui.label(format!("八进制:{:04o}", *mode));
                    ui.horizontal(|ui| {
                        if ui.button("应用").clicked() {
                            op = Some(FileOp::Chmod { path: path.clone(), mode: *mode });
                            close = true;
                        }
                        if ui.button("取消").clicked() {
                            close = true;
                        }
                    });
                });
        }
    }

    if close {
        *dialog = None;
    }
    op
}
```

- [ ] **Step 4：接进 `build_ui`**

`crates/mullion-app/src/ui/mod.rs`：

`UiState` 加字段（放在 `files_sidebar_w` 之后）：

```rust
    /// D2:远端写操作的对话框状态。`None` = 没开。
    ///
    /// 挂在 `UiState` 而不是 `PanelFrame` 上:对话框是**全局模态**,同一时刻
    /// 只该有一个,而 `PanelFrame` 每个标签一份。放进 `PanelFrame` 的话,
    /// 切个标签就会冒出另一个删除确认框。
    pub files_dialog: Option<files_dialog::FilesDialog>,
```

`UiActions` 加字段：

```rust
    /// D2:用户在对话框里**确认**了的远端写操作。到这一步没有回头路,
    /// `app.rs` 收到就直接发请求。
    ///
    /// 加字段时记得同步 `app.rs::has_real_action` —— 漏了的话这个动作会在
    /// egui 的 discard 趟被静默吃掉,而且默认没有任何测试会变红。
    pub files_op: Option<files_dialog::FileOp>,
```

`build_ui` 里在 `session_manager::show` 之后、`toast::show` 之前调用（对话框该盖在会话管理器之上、toast 之下）：

```rust
    // D2:远端写操作确认框。排在会话管理器之后 —— 它是从文件面板发起的模态,
    // 该盖在别的窗口上;排在 toast 之前 —— 操作反馈永远在最上面(走查 13)。
    actions.files_op = files_dialog::show(ctx, t, &mut ui_state.files_dialog);
```

- [ ] **Step 5：跑绿**

Run: `cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|error\["`
Expected: 全 ok。

Run: `cargo clippy -p mullion-app --all-targets -- -D warnings`
Expected: 无输出。

- [ ] **Step 6：提交**

```bash
git add crates/mullion-app/src/ui/files_dialog.rs crates/mullion-app/src/ui/mod.rs
git commit -m "feat(ui): 新建/重命名/删除确认/权限四个对话框,危险措辞按 F119 (F54/D17/D21)"
```

---

## Task 8：`app.rs` 接线（`Ask` → 对话框，`FileOp` → 异步任务）

**Files:**
- Modify: `crates/mullion-app/src/app.rs`

- [ ] **Step 1：加 `UserEvent` 变体**

在 `enum UserEvent` 里（`SftpListed` 之后）加：

```rust
    /// D2/F54:一次远端写操作跑完了。`Ok(())` = 成功,`Err` = 已经格式化好的
    /// 可读原因。**按世代路由**(S1):用户在一次网络往返期间切了标签,结果
    /// 也要回到发起它的那个标签,不是当前活动标签。
    ///
    /// 成功之后由接收方发起一次刷新 —— 写操作不带回新的目录内容,不刷新的话
    /// 界面上那个文件"还在",用户会以为删除没生效然后再删一次。
    SftpOpDone {
        generation: u64,
        result: Result<(), String>,
    },
```

- [ ] **Step 2：把 `Ask` 变成对话框状态**

`apply_remote_file_action` 的 `match action` 里加分支（在既有四个之后）。**注意**：`Ask` 不发网络请求，它只是开对话框，所以要在算 `target` 之前就 return：

```rust
            // D2:开对话框,不发请求。真正的写操作等用户确认之后从
            // `UiActions::files_op` 回来(见 `apply_file_op`)。
            FileAction::Ask(ask) => {
                self.open_files_dialog(generation, ask);
                return;
            }
            // 本地栏专属,远端栏收到就是接线接错了 —— 老实记一条,不静默吞。
            FileAction::OpenInExplorer => {
                log::warn!(target: "mullion", "远端栏收到 OpenInExplorer,忽略");
                return;
            }
```

新增方法（放在 `apply_remote_file_action` 之后）：

```rust
    /// D2:把一个「打开对话框」的意图落成 `UiState::files_dialog`。
    ///
    /// **对话框的内容在这里一次性算好**(要删哪些、原名是什么、当前权限是
    /// 多少),不是等渲染时再回头查面板状态:对话框开着的时候用户可能已经
    /// 切了标签、目录已经刷新过,那时再查就是另一份数据了。
    fn open_files_dialog(&mut self, generation: u64, ask: crate::ui::files_panel::FileAsk) {
        use crate::ui::files_dialog::FilesDialog;
        use crate::ui::files_panel::FileAsk;

        let Some(tab) = self.tabs.iter().find(|t| t.content.generation() == generation) else {
            return;
        };
        let state = &tab.content.files_panel().remote;
        let dialog = match ask {
            FileAsk::NewDir => Some(FilesDialog::NewDir {
                parent: state.cwd.clone(),
                name: String::new(),
            }),
            FileAsk::Rename => state.cursor.as_ref().map(|cur| FilesDialog::Rename {
                from: state.cwd.join(cur.as_bytes()),
                name: cur.display().to_string(),
            }),
            FileAsk::Chmod => state.cursor.as_ref().and_then(|cur| {
                let e = state.entries.iter().find(|e| &e.name == cur)?;
                Some(FilesDialog::Chmod {
                    path: state.cwd.join(cur.as_bytes()),
                    mode: e.mode & 0o777,
                })
            }),
            FileAsk::Delete => {
                // 选中集为空时退化成「删光标那一条」—— 用户按 Delete 时
                // 多半就是想删高亮那条,弹一个「没有选中任何条目」的空框
                // 只会让人以为程序坏了。
                let picked = if state.selected.is_empty() {
                    state.cursor.iter().cloned().collect::<Vec<_>>()
                } else {
                    state.selected_paths()
                };
                let targets: Vec<(mullion_ssh::sftp::RemotePath, bool)> = picked
                    .iter()
                    .filter_map(|name| {
                        let e = state.entries.iter().find(|e| &e.name == name)?;
                        // 发不出去的名字不许进删除列表 —— 请求打不中那个文件,
                        // 而它会在确认框里让用户以为「删了 5 条」。
                        if !name.is_operable() {
                            return None;
                        }
                        Some((
                            state.cwd.join(name.as_bytes()),
                            e.kind == mullion_ssh::sftp::EntryKind::Dir,
                        ))
                    })
                    .collect();
                if targets.is_empty() {
                    None
                } else {
                    Some(FilesDialog::Delete { targets })
                }
            }
        };
        if dialog.is_some() {
            self.ui.files_dialog = dialog;
            // 对话框是新出现的窗口,不请求重绘的话键盘发起的那条路径
            // (Delete / F2)要等鼠标动一下才画得出来(D1 复核挖出的同款 bug)。
            self.request_ui_redraw();
        }
    }
```

- [ ] **Step 3：把 `FileOp` 落成异步任务**

新增方法：

```rust
    /// D2/F54:执行一次已确认的远端写操作。
    ///
    /// 全部走后台 task + `UserEvent::SftpOpDone` 回流,**不在 UI 线程上等**:
    /// 一次递归删除在高延迟链路上可能跑几十秒,阻塞窗口线程等于整个程序卡死。
    fn apply_file_op(&mut self, generation: u64, op: crate::ui::files_dialog::FileOp) {
        use crate::ui::files_dialog::FileOp;

        let Some(tab) = self.tabs.iter().find(|t| t.content.generation() == generation) else {
            return;
        };
        let Some(client) = tab.content.sftp_client() else {
            self.ui.set_error("SFTP 通道还没建立,请先等目录加载完".into());
            return;
        };
        let conn = tab.content.sftp_connection();
        let proxy = self.proxy.clone();
        let task = self._runtime.spawn(async move {
            let result = match op {
                FileOp::NewDir(p) => client.create_dir(&p).await.map_err(|e| e.to_string()),
                FileOp::Rename { from, to } => {
                    client.rename(&from, &to).await.map_err(|e| e.to_string())
                }
                FileOp::Chmod { path, mode } => client
                    .set_permissions(&path, mode)
                    .await
                    .map_err(|e| e.to_string()),
                FileOp::Delete { targets } => delete_all(&client, conn.as_ref(), &targets).await,
            };
            let _ = proxy.send_event(UserEvent::SftpOpDone { generation, result });
        });
        self.track_sftp_task(generation, task);
    }
```

在 `app.rs` 的自由函数区（`next_panel_selection_index` 附近）加：

```rust
/// 逐条删。目录走递归删除(F57:先 exec 后回退),文件与链接走 remove。
///
/// **一条失败就停**,并把已经删掉的条数报进错误里:继续删下去的话,用户
/// 看到一条「权限不足」却不知道前面几条到底删没删,而这一步不可逆。
async fn delete_all(
    client: &std::sync::Arc<mullion_ssh::sftp::SftpClient>,
    conn: Option<&std::sync::Arc<SshConnection>>,
    targets: &[(mullion_ssh::sftp::RemotePath, bool)],
) -> Result<(), String> {
    for (ix, (path, is_dir)) in targets.iter().enumerate() {
        let r = if *is_dir {
            match conn {
                // 递归删除要 exec 快路径,而 exec 要连接句柄。拿不到就
                // 退化成纯 SFTP 递归 —— 慢,但不能因此不给删。
                Some(c) => mullion_ssh::remove_tree::remove_tree(client, c, path)
                    .await
                    .map(|_| ()),
                None => client.remove_dir(path).await,
            }
        } else {
            // **链接走 remove_file** —— SFTP 的 REMOVE 删的是链接本身,
            // 不跟随(设计 D17)。
            client.remove_file(path).await
        };
        if let Err(e) = r {
            return Err(format!(
                "删除 {} 失败:{e}(前面 {ix} 条已删除)",
                path.display()
            ));
        }
    }
    Ok(())
}
```

- [ ] **Step 4：接收 `SftpOpDone` 并刷新**

在 `user_event` 的 `match` 里加：

```rust
            UserEvent::SftpOpDone { generation, result } => {
                match result {
                    Ok(()) => {
                        self.ui.set_toast("已完成");
                        // 写操作不带回新的目录内容 —— 不刷新的话界面上那个
                        // 文件"还在",用户会以为没生效然后再删一次。
                        self.dispatch_panel_action_for(
                            generation,
                            crate::ui::files_panel::PanelColumn::Remote,
                            crate::ui::files_panel::FileAction::Refresh,
                        );
                    }
                    Err(msg) => self.ui.set_error(msg),
                }
                self.request_ui_redraw();
            }
```

- [ ] **Step 5：把 `UiActions::files_op` 接进 present 分支**

在 `render_frame` 返回 `actions` 之后处理 `files_remote`/`files_local` 的那一段旁边加：

```rust
                            // D2:对话框里确认了的写操作。按侧栏/标签属主的
                            // 世代路由(S1),与 `files_remote` 同一条判据 ——
                            // 不重新算一遍。
                            if let (Some(gen), Some(op)) =
                                (files_owner_generation, actions.files_op.take())
                            {
                                self.apply_file_op(gen, op);
                            }
```

`has_real_action` 补一行：

```rust
        || a.files_op.is_some()
```

- [ ] **Step 6：加守护测试**

在 `app.rs` 的 `mod tests` 里追加：

```rust
    /// `UiActions` 加了字段却漏改 `has_real_action` 的话,新动作会在 egui 的
    /// discard 趟被静默吃掉 —— 症状是「点了确认,什么也没发生,也不报错」。
    /// 这条把**每一个** Option 字段都点一遍。
    ///
    /// 破坏性验证:把 `has_real_action` 里的 `a.files_op.is_some()` 删掉,
    /// 这条必须变红。
    #[test]
    fn every_ui_action_field_counts_as_a_real_action() {
        use crate::ui::files_dialog::FileOp;
        use crate::ui::UiActions;

        let mut a = UiActions::default();
        assert!(!has_real_action(&a), "全空时不该算有动作");

        a.files_op = Some(FileOp::NewDir(mullion_ssh::sftp::RemotePath::from_bytes(
            b"/x".to_vec(),
        )));
        assert!(has_real_action(&a), "files_op 没被算进 has_real_action");
    }

    /// 写操作完成后**必须刷新**。不刷新的症状是:删了一个文件,列表里
    /// 那一行还在,用户以为没生效再删一次,收到一条 NoSuchFile。
    ///
    /// 这是结构守护(`user_event` 要 `&mut App`,无头造不出来):断言
    /// `SftpOpDone` 的 `Ok` 分支里确实发了一次 Refresh。
    #[test]
    fn a_successful_write_triggers_a_refresh_so_the_list_is_not_stale() {
        let src = include_str!("app.rs");
        let (production, _) = src
            .split_once("#[cfg(test)]")
            .expect("找不到 #[cfg(test)] 边界");
        let at = production
            .find("UserEvent::SftpOpDone")
            .expect("找不到 SftpOpDone 的处理分支");
        // 取这个分支往后一小段(下一个 UserEvent:: 之前)。
        let rest = &production[at + "UserEvent::SftpOpDone".len()..];
        let end = rest.find("UserEvent::").unwrap_or(rest.len());
        let arm = &rest[..end];
        assert!(
            arm.contains("FileAction::Refresh"),
            "写操作成功后没有刷新目录 —— 界面会一直显示已经删掉的那一行"
        );
    }
```

- [ ] **Step 7：跑绿**

Run: `cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|error\["`
Expected: 全 ok。

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: 无输出。

- [ ] **Step 8：提交**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): 远端写操作接线——对话框意图、异步执行、完成后刷新 (F54/F57)"
```

---

## Task 9：键盘 `Delete`/`F2` + 「在资源管理器中打开」

**Files:**
- Modify: `crates/mullion-app/src/app.rs`
- Modify: `crates/mullion-app/src/files/local.rs`

**背景**：D1 的 `handle_panel_key` **故意没接** `Delete`/`F2`（那时没有写操作）。现在补上。

- [ ] **Step 1：接两个键**

`handle_panel_key` 的 `match key` 里加：

```rust
            // D2:仅远端栏 —— 本地栏不提供删除/重命名(设计 D5)。
            WinitKey::Named(NamedKey::Delete) => {
                self.dispatch_panel_action_for(
                    generation,
                    crate::ui::files_panel::PanelColumn::Remote,
                    FileAction::Ask(crate::ui::files_panel::FileAsk::Delete),
                );
            }
            WinitKey::Named(NamedKey::F2) => {
                self.dispatch_panel_action_for(
                    generation,
                    crate::ui::files_panel::PanelColumn::Remote,
                    FileAction::Ask(crate::ui::files_panel::FileAsk::Rename),
                );
            }
```

**注意**：这两个键**不看 `active_column`** —— 焦点在本地栏时按 Delete 也只对远端栏生效是不对的。改成：焦点在本地栏时**什么都不做**：

```rust
            WinitKey::Named(NamedKey::Delete) | WinitKey::Named(NamedKey::F2) => {
                // 设计 D5:本地栏不提供删除 / 重命名。焦点在本地栏时这两个键
                // **静默不动**,不是转投远端栏 —— 用户看着本地栏按 Delete,
                // 结果删了远端文件,是这一片能造成的最坏后果。
                let column = self
                    .tabs
                    .iter()
                    .find(|t| t.content.generation() == generation)
                    .map(|t| t.content.files_panel().active_column);
                if column != Some(crate::ui::files_panel::PanelColumn::Remote) {
                    return;
                }
                let ask = if matches!(key, WinitKey::Named(NamedKey::Delete)) {
                    crate::ui::files_panel::FileAsk::Delete
                } else {
                    crate::ui::files_panel::FileAsk::Rename
                };
                self.dispatch_panel_action_for(
                    generation,
                    crate::ui::files_panel::PanelColumn::Remote,
                    FileAction::Ask(ask),
                );
            }
```

- [ ] **Step 2：加守护测试**

`app.rs` 的 `mod tests`：

```rust
    /// 设计 D5 最要命的一条:焦点在**本地栏**时按 `Delete`,绝不能去删远端
    /// 文件。转投远端栏是一个看着"体贴"、后果不可逆的实现。
    ///
    /// 结构守护(`handle_panel_key` 要 `&mut App`):断言那一段里确实有
    /// 「不是远端栏就 return」这道闸。
    ///
    /// 破坏性验证:把那两行闸删掉 —— 这条必须变红。
    #[test]
    fn delete_and_rename_keys_do_nothing_while_the_local_column_has_focus() {
        let src = include_str!("app.rs");
        let (production, _) = src
            .split_once("#[cfg(test)]")
            .expect("找不到 #[cfg(test)] 边界");
        let at = production
            .find("WinitKey::Named(NamedKey::Delete)")
            .expect("找不到 Delete 键的处理");
        let arm = &production[at..at + 900.min(production.len() - at)];
        assert!(
            arm.contains("PanelColumn::Remote)") && arm.contains("return;"),
            "Delete/F2 的处理里没有「焦点不在远端栏就不动」这道闸 ——\
             用户看着本地栏按 Delete 会删掉远端文件"
        );
    }
```

- [ ] **Step 3：本地栏「在资源管理器中打开」**

`crates/mullion-app/src/files/local.rs` 末尾（`#[cfg(test)]` 之前）加：

```rust
/// 用系统文件管理器打开一个本地目录(设计 D5:本地文件管理外包出去)。
///
/// 平台命令抽成 [`open_command`] 是为了能在无头环境验参数 —— 真的 spawn
/// 一个资源管理器在 CI 里既跑不起来也没法断言。
pub fn open_in_file_manager(dir: &RemotePath) -> Result<(), String> {
    let (prog, arg) = open_command(dir);
    std::process::Command::new(&prog)
        .arg(&arg)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("打不开文件管理器({prog}):{e}"))
}

/// 平台对应的「打开目录」命令。**不拼 shell 命令行** —— 直接给
/// `Command::arg`,路径里的空格 / 引号 / `$` 全都不需要转义,
/// 也就没有注入面。
fn open_command(dir: &RemotePath) -> (String, std::ffi::OsString) {
    let path = to_path(dir).into_os_string();
    #[cfg(windows)]
    {
        ("explorer.exe".to_string(), path)
    }
    #[cfg(target_os = "macos")]
    {
        ("open".to_string(), path)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        ("xdg-open".to_string(), path)
    }
}
```

测试：

```rust
    /// 路径**原样**交给 `Command::arg`,不经 shell —— 名字里有空格或
    /// `$(...)` 的目录也不需要转义,更没有注入面。
    #[test]
    fn the_file_manager_command_passes_the_path_as_a_single_argument() {
        let d = RemotePath::from_bytes(b"/tmp/a b $(x)".to_vec());
        let (prog, arg) = open_command(&d);
        assert!(!prog.is_empty());
        assert_eq!(
            arg,
            std::ffi::OsString::from("/tmp/a b $(x)"),
            "路径必须原样当成一个参数,不许拼进命令行字符串"
        );
    }
```

- [ ] **Step 4：接线**

`app.rs` 的 `apply_local_file_action` 的 `match action` 里加：

```rust
            FileAction::OpenInExplorer => {
                if let Err(e) = local::open_in_file_manager(&files.local.cwd) {
                    self.ui.set_error(e);
                }
                return;
            }
            // 写操作对话框只挂在远端栏(设计 D5)。本地栏收到就是接线接错了。
            FileAction::Ask(_) => {
                log::warn!(target: "mullion", "本地栏收到写操作请求,忽略(D5)");
                return;
            }
```

- [ ] **Step 5：跑绿 + 提交**

Run: `cargo test --workspace 2>&1 | grep -E "test result|FAILED|error\["`
Run: `cargo clippy --workspace --all-targets -- -D warnings`
Run: `cargo fmt --check`

```bash
git add crates/mullion-app/src/app.rs crates/mullion-app/src/files/local.rs
git commit -m "feat(app): 面板 Delete/F2 键与「在资源管理器中打开」,本地栏无写操作 (F54/D5)"
```

---

## Task 10：绿闸门与自查（不发版）

**Files:** 无（只跑与记录）

- [ ] **Step 1：全量绿**

```bash
cargo test --workspace > /tmp/d2a-test.log 2>&1; echo "exit=$?"
grep -nE "test result|FAILED|panicked" /tmp/d2a-test.log | grep -v " 0 failed" | head
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```
Expected: `exit=0`、无 FAILED、clippy 无输出、fmt 干净。

- [ ] **Step 2：领域陷阱守护复跑**

这一片碰了 `app.rs` 的键盘分流与事件循环，T3/T7/T8 必须复跑：

```bash
cargo test -p mullion-app redraw_is_frame_capped
cargo test -p mullion-app frame::tests
cargo test -p mullion-app terminal_keyboard_is_never_fed_to_egui
cargo test -p mullion-app reflow_emits_resize
```
Expected: 全 ok。

- [ ] **Step 3：确认 D1 的「零写操作」验收条款仍然成立**

```bash
cargo test -p mullion-ssh --test sftp_browse a_full_browse_session_never_sends_a_single_write_request
```
Expected: ok。**这条不该因为 D2 而删掉或放宽** —— 它守的是「**浏览**动作不写」，与「客户端有没有写方法」是两件事。假服务端的 `remove` 从「恒拒绝」改成「真的删」之后，这条测试的区分力**变强了**（原先就算客户端误发 remove 也只会拿到 PermissionDenied）。

- [ ] **Step 4：不提交（本任务无改动）**，直接进入 D2-b 的计划

---

## 自查（写完计划后按 writing-plans 的要求跑一遍）

**1. spec 覆盖**

| spec / 设计条目 | 落在哪个 Task |
|---|---|
| F54 多选 + 批量删除 | Task 5（多选）、Task 7（确认框）、Task 8（执行） |
| F57 递归删除先 exec 后回退 + 路径转义 | Task 3（转义 + exec）、Task 4（回退） |
| D17 删链接不跟随 | Task 4（`remove_via_sftp` 的 `EntryKind::Dir` 分支 + 双路径测试） |
| D17 无回收站、危险措辞 | Task 7（`delete_summary` + 「不可逆」一行 + 按钮写「删除」） |
| D21 权限修改（八进制 + 九宫格，不递归） | Task 2（`set_permissions`）、Task 7（九宫格） |
| D5 本地栏不提供删除/重命名/新建 | Task 6（`menu_items_for`）、Task 9（Delete/F2 闸）、Task 9（`OpenInExplorer`） |
| D16 发不出去的路径一个请求都不发 | Task 2、Task 4 各有一条探针测试 |
| **不在本片**：F55 队列 / F56 并发 / `.part` / 冲突 / Windows 非法名 / 退避重连 | D2-b |

**2. 占位符扫描**：无 TBD / TODO；每个改代码的 Step 都给了完整代码块。

**3. 类型一致性**
- `spawn_sftp_server` 在 Task 1 改成三元组，Task 2/4 的测试都按三元组解构。✓
- `RemoveReport` 在 Task 4 定义，同一 Task 的测试里使用。✓
- `FileAsk` / `MenuItem` 在 Task 6 定义（`files_panel.rs`），Task 7 的 `FilesDialog` 与 Task 8 的 `open_files_dialog` 引用；`FileOp` 在 Task 7 定义（`files_dialog.rs`），Task 8 使用。✓
- `PaneState::cursor` 在 Task 5 引入，Task 6（`menu_items_for(_, state.cursor.is_some())`）与 Task 8（`open_files_dialog`）使用。✓
- `SftpClient::stat` 在 Task 2 定义，Task 4 的 `remove_via_sftp` 使用。✓
- `shell_quote` / `exec` / `ExecError` 在 Task 3 定义，Task 4 使用。✓
