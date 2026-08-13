#![allow(dead_code)]
//! 进程内假 SFTP 服务端:内存文件系统 + `russh_sftp::server::Handler`,
//! 挂在同目录的假 sshd 上(`subsystem_request` 收到 "sftp" 就把 channel
//! 交给它)。与隧道切片「拿自家客户端打自家服务端」同一手法。
//!
//! **它同时是守护测试的探针**:每个请求收到的**原始路径串**都记进
//! `Probe::seen`,于是「非 UTF-8 名一个请求都没发出去」这条断言有据可依。
//!
//! `dead_code` 是**长期**要开的:`mod common` 在每个测试二进制里各编译一份,
//! 不跑 SFTP 的那几个(pty / tunnel_forward / smoke_server …)用不到这里的符号。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use russh_sftp::protocol::{File, FileAttributes, Handle, Name, Status, StatusCode, Version};

/// 内存里的一个节点。
#[derive(Clone)]
pub struct Node {
    pub name: Vec<u8>,
    pub kind: NodeKind,
    pub size: u64,
    pub mtime: u32,
    /// 八进制权限位的低 12 位(不含类型位,类型位由 `kind` 决定)。
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
}

#[derive(Clone, PartialEq, Eq)]
pub enum NodeKind {
    Dir,
    File,
    /// 符号链接及其目标(用于「删除不跟随」类测试;D1 只用来显示)。
    Symlink(Vec<u8>),
}

impl Node {
    pub fn dir(name: &[u8]) -> Self {
        Self {
            name: name.to_vec(),
            kind: NodeKind::Dir,
            size: 4096,
            mtime: 1_700_000_000,
            mode: 0o755,
            uid: 1000,
            gid: 1000,
        }
    }
    pub fn file(name: &[u8], size: u64) -> Self {
        Self {
            name: name.to_vec(),
            kind: NodeKind::File,
            size,
            mtime: 1_700_000_100,
            mode: 0o644,
            uid: 1000,
            gid: 1000,
        }
    }
    pub fn link(name: &[u8], target: &[u8]) -> Self {
        Self {
            name: name.to_vec(),
            kind: NodeKind::Symlink(target.to_vec()),
            size: target.len() as u64,
            mtime: 1_700_000_200,
            mode: 0o777,
            uid: 1000,
            gid: 1000,
        }
    }

    fn attrs(&self) -> FileAttributes {
        let mut a = FileAttributes {
            size: Some(self.size),
            uid: Some(self.uid),
            user: None,
            gid: Some(self.gid),
            group: None,
            permissions: Some(self.mode),
            atime: Some(self.mtime),
            mtime: Some(self.mtime),
        };
        match self.kind {
            NodeKind::Dir => a.set_dir(true),
            NodeKind::File => a.set_regular(true),
            // 同时置 dir 位:一些真实 sshd 对「指向目录的符号链接」在 readdir
            // 里会把两个类型位一起报回来。这也顺带扎住了客户端 kind 判定的
            // 分支顺序——先判 dir 会把它误认成目录(见 sftp_browse.rs 的
            // mutation 验收注释)。
            NodeKind::Symlink(_) => {
                a.set_symlink(true);
                a.set_dir(true);
            }
        }
        a
    }
}

/// 服务端见过的每一个请求。守护测试靠它证明「某个请求根本没发出去」。
#[derive(Default)]
pub struct Probe {
    pub seen: Vec<(&'static str, String)>,
    /// SSH 层的 `pty_request` 次数。SFTP 通道上必须恒为 0。
    pub pty_requests: usize,
}

impl Probe {
    pub fn paths_for(&self, op: &str) -> Vec<String> {
        self.seen
            .iter()
            .filter(|(o, _)| *o == op)
            .map(|(_, p)| p.clone())
            .collect()
    }
}

/// 内存文件系统:目录路径(字节) → 该目录下的节点。
pub type Tree = HashMap<Vec<u8>, Vec<Node>>;

pub struct SftpHandler {
    tree: Arc<Tree>,
    probe: Arc<Mutex<Probe>>,
    /// opendir 发出的 handle → 它对应的目录路径;readdir 第二次调用要返回 EOF。
    dirs: HashMap<String, (Vec<u8>, bool)>,
    next_handle: u64,
}

impl SftpHandler {
    pub fn new(tree: Arc<Tree>, probe: Arc<Mutex<Probe>>) -> Self {
        Self {
            tree,
            probe,
            dirs: HashMap::new(),
            next_handle: 0,
        }
    }

    fn note(&self, op: &'static str, path: &str) {
        self.probe.lock().unwrap().seen.push((op, path.to_owned()));
    }

    /// 查一个路径的属性。**不记探针** —— 记不记由调用它的那个协议方法决定,
    /// 否则 `stat` 与 `lstat` 共用实现时会把对方的名字也记一遍。
    fn attrs_of(&self, id: u32, path: &str) -> Result<russh_sftp::protocol::Attrs, StatusCode> {
        if self.tree.contains_key(path.as_bytes()) {
            return Ok(russh_sftp::protocol::Attrs {
                id,
                attrs: Node::dir(b"").attrs(),
            });
        }
        let (dir, name) = split_last(path.as_bytes());
        match self
            .tree
            .get(&dir)
            .and_then(|v| v.iter().find(|n| n.name == name))
        {
            Some(n) => Ok(russh_sftp::protocol::Attrs {
                id,
                attrs: n.attrs(),
            }),
            None => Err(StatusCode::NoSuchFile),
        }
    }
}

impl russh_sftp::server::Handler for SftpHandler {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(
        &mut self,
        _version: u32,
        _ext: HashMap<String, String>,
    ) -> Result<Version, Self::Error> {
        Ok(Version::new())
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        self.note("realpath", &path);
        // `.` = 登录目录,固定成 /home/testuser,跟真 sshd 的行为一致。
        let resolved = if path == "." || path.is_empty() {
            "/home/testuser".to_string()
        } else {
            path
        };
        Ok(Name {
            id,
            files: vec![File::dummy(resolved)],
        })
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        self.note("opendir", &path);
        let key = path.clone().into_bytes();
        if !self.tree.contains_key(&key) {
            return Err(StatusCode::NoSuchFile);
        }
        self.next_handle += 1;
        let h = format!("dir-{}", self.next_handle);
        self.dirs.insert(h.clone(), (key, false));
        Ok(Handle { id, handle: h })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        self.note("readdir", &handle);
        let Some((dir, done)) = self.dirs.get_mut(&handle) else {
            return Err(StatusCode::Failure);
        };
        if *done {
            // 协议要求读完用 EOF 收尾,否则客户端会一直问下去。
            return Err(StatusCode::Eof);
        }
        *done = true;
        let dir = dir.clone();
        let files = self.tree.get(&dir).cloned().unwrap_or_default();
        Ok(Name {
            id,
            files: files
                .iter()
                .map(|n| File::new(String::from_utf8_lossy(&n.name).to_string(), n.attrs()))
                .collect(),
        })
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        // 也要记:「开了目录有没有关」是 channel/handle 泄漏那类 bug 的唯一
        // 探针,漏记这一条 Probe 就查不出来(模块文档承诺了每个请求都记)。
        self.note("close", &handle);
        self.dirs.remove(&handle);
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: "Ok".into(),
            language_tag: "en-US".into(),
        })
    }

    async fn readlink(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        self.note("readlink", &path);
        let (dir, name) = split_last(path.as_bytes());
        let node = self
            .tree
            .get(&dir)
            .and_then(|v| v.iter().find(|n| n.name == name));
        match node.map(|n| &n.kind) {
            Some(NodeKind::Symlink(t)) => Ok(Name {
                id,
                files: vec![File::dummy(String::from_utf8_lossy(t).to_string())],
            }),
            _ => Err(StatusCode::NoSuchFile),
        }
    }

    async fn lstat(
        &mut self,
        id: u32,
        path: String,
    ) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
        self.note("lstat", &path);
        self.attrs_of(id, &path)
    }

    async fn stat(
        &mut self,
        id: u32,
        path: String,
    ) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
        self.note("stat", &path);
        // **不转发给 `lstat`**:那样一次 STAT 会在 Probe 里同时留下 "stat"
        // 和 "lstat" 两条,`paths_for("lstat")` 就再也分不清客户端到底发的
        // 是哪一种 —— 探针一旦会说谎,靠它写的守护测试全都不算数。
        self.attrs_of(id, &path)
    }

    /// **写操作一律拒绝。** D1 是只读切片,服务端在这里替我们把关:
    /// 客户端要是偷偷发了写请求,测试会看到 `PermissionDenied` 而不是静默成功。
    async fn remove(&mut self, _id: u32, filename: String) -> Result<Status, Self::Error> {
        self.note("remove", &filename);
        Err(StatusCode::PermissionDenied)
    }
}

/// `"/a/b/c"` → `("/a/b", "c")`。根下的项父目录是 `"/"`。
fn split_last(path: &[u8]) -> (Vec<u8>, Vec<u8>) {
    match path.iter().rposition(|b| *b == b'/') {
        Some(0) => (b"/".to_vec(), path[1..].to_vec()),
        Some(ix) => (path[..ix].to_vec(), path[ix + 1..].to_vec()),
        None => (b".".to_vec(), path.to_vec()),
    }
}
