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
    /// 文件内容。D2-b 的传输测试判据是「服务端上的字节」——只看
    /// `size` 的话,一个把偏移算错、把同一块写两遍的实现照样绿。
    pub data: Vec<u8>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
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
            data: Vec::new(),
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
            data: Vec::new(),
        }
    }

    /// 带内容的文件。`size` 由内容长度算 —— 传输测试断言「收到的字节
    /// 与树上的一致」,两者对不上时永远说不清是谁错了。
    pub fn file_with(name: &[u8], data: &[u8]) -> Self {
        let mut n = Self::file(name, data.len() as u64);
        n.data = data.to_vec();
        n
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
            data: Vec::new(),
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
    /// `exec_request` 收到的命令行(原始字节)。F57 的转义守护靠它 ——
    /// 记 `String` 的话,一条含非 UTF-8 字节的路径会被有损转换,
    /// 「发出去的到底是哪串字节」就永远查不清了。
    pub execs: Vec<Vec<u8>>,
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

/// 树里有没有这一条(目录键,或某目录下的一个节点名)。
/// 测试断言「删掉了没」「建出来了没」用它 —— 不给的话每个测试都要
/// 自己抄一遍 `split_last` 那套查法,抄错了就是测试在说谎。
pub fn exists(tree: &Tree, path: &[u8]) -> bool {
    if tree.contains_key(path) {
        return true;
    }
    let (dir, name) = split_last(path);
    tree.get(&dir)
        .is_some_and(|v| v.iter().any(|n| n.name == name))
}

/// 某个目录下现有的节点名。断言「目录里还剩什么」用。
pub fn names_in(tree: &Tree, dir: &[u8]) -> Vec<Vec<u8>> {
    tree.get(dir)
        .map(|v| v.iter().map(|n| n.name.clone()).collect())
        .unwrap_or_default()
}

/// `split_last` 的对外包装 —— `common/mod.rs` 里的 exec 模拟要用同一套
/// 父目录/名字的切法,自己再写一遍就会两边不一致。
pub fn split_last_pub(path: &[u8]) -> (Vec<u8>, Vec<u8>) {
    split_last(path)
}

pub struct SftpHandler {
    /// **可写**的内存树。D1 时它是 `Arc<Tree>`(只读切片,服务端替我们把关);
    /// D2 有了写操作,守护测试的判据从「服务端拒了没」变成
    /// 「树上到底变成什么样」,于是必须能改。
    tree: Arc<Mutex<Tree>>,
    probe: Arc<Mutex<Probe>>,
    /// opendir 发出的 handle → 它对应的目录路径;readdir 第二次调用要返回 EOF。
    dirs: HashMap<String, (Vec<u8>, bool)>,
    /// `open` 发出的 handle → 它对应的**完整路径**。读写按路径回到树上找,
    /// 不在 handle 里缓存内容 —— 缓存的话「服务端上到底变成什么样」
    /// 要等 close 才成立,而传输测试恰恰想在中途看。
    files: HashMap<String, String>,
    next_handle: u64,
    /// F220/B3 缺口 2:`true` = `rename` 一律失败,模拟真实 EXDEV(跨设备
    /// 重命名)。`copy_tree.rs` 的 `sftp.rename(..).is_err()` 那条「退成拷贝
    /// +删源」的分支——假服务端的 `rename` 平时从不失败,那个分支原本零
    /// 覆盖。照 `allow_exec`(见 `common/mod.rs`)那个既有开关的写法来。
    reject_rename: bool,
}

impl SftpHandler {
    pub fn new(tree: Arc<Mutex<Tree>>, probe: Arc<Mutex<Probe>>) -> Self {
        Self::new_with_rename_policy(tree, probe, false)
    }

    /// `reject_rename = true` 时 `rename` 一律报错,见该字段的文档。
    pub fn new_with_rename_policy(
        tree: Arc<Mutex<Tree>>,
        probe: Arc<Mutex<Probe>>,
        reject_rename: bool,
    ) -> Self {
        Self {
            tree,
            probe,
            dirs: HashMap::new(),
            files: HashMap::new(),
            next_handle: 0,
            reject_rename,
        }
    }

    fn note(&self, op: &'static str, path: &str) {
        self.probe.lock().unwrap().seen.push((op, path.to_owned()));
    }

    /// 查一个路径的属性。**不记探针** —— 记不记由调用它的那个协议方法决定,
    /// 否则 `stat` 与 `lstat` 共用实现时会把对方的名字也记一遍。
    fn attrs_of(&self, id: u32, path: &str) -> Result<russh_sftp::protocol::Attrs, StatusCode> {
        let tree = self.tree.lock().unwrap();
        if tree.contains_key(path.as_bytes()) {
            return Ok(russh_sftp::protocol::Attrs {
                id,
                attrs: Node::dir(b"").attrs(),
            });
        }
        let (dir, name) = split_last(path.as_bytes());
        match tree
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
        if !self.tree.lock().unwrap().contains_key(&key) {
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
        // 先把 dir clone 出来结束对 `self.dirs` 的可变借用,再锁 tree ——
        // 顺序反了借用检查器直接拦下。
        let dir = dir.clone();
        let files = self
            .tree
            .lock()
            .unwrap()
            .get(&dir)
            .cloned()
            .unwrap_or_default();
        Ok(Name {
            id,
            files: files
                .iter()
                .map(|n| File::new(String::from_utf8_lossy(&n.name).to_string(), n.attrs()))
                .collect(),
        })
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: russh_sftp::protocol::OpenFlags,
        _attrs: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        self.note("open", &filename);
        let (dir, name) = split_last(filename.as_bytes());
        {
            let mut tree = self.tree.lock().unwrap();
            let existed = tree
                .get(&dir)
                .is_some_and(|v| v.iter().any(|n| n.name == name));
            if pflags.contains(russh_sftp::protocol::OpenFlags::WRITE) {
                // 父目录不存在就拒 —— 真 sshd 不会替你 mkdir -p,
                // 服务端替客户端兜底的话「上传前建目录」那条逻辑就测不出来。
                if !tree.contains_key(&dir) {
                    return Err(StatusCode::NoSuchFile);
                }
                if existed && pflags.contains(russh_sftp::protocol::OpenFlags::EXCLUDE) {
                    return Err(StatusCode::Failure);
                }
                if !existed {
                    tree.entry(dir)
                        .or_default()
                        .push(Node::file_with(&name, b""));
                } else if pflags.contains(russh_sftp::protocol::OpenFlags::TRUNCATE) {
                    if let Some(n) = tree
                        .get_mut(&dir)
                        .and_then(|v| v.iter_mut().find(|n| n.name == name))
                    {
                        n.data.clear();
                        n.size = 0;
                    }
                }
            } else if !existed {
                return Err(StatusCode::NoSuchFile);
            }
        }
        self.next_handle += 1;
        let h = format!("file-{}", self.next_handle);
        self.files.insert(h.clone(), filename);
        Ok(Handle { id, handle: h })
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<russh_sftp::protocol::Data, Self::Error> {
        let path = self.files.get(&handle).ok_or(StatusCode::Failure)?.clone();
        // F214:READ 也记探针。READ 在客户端是**串行**的,次数就是往返数 ——
        // 「打开一个文件要好几秒」这类实报只能从这个数上被证伪或坐实,
        // 而本机往返为零,不数次数就永远量不到。
        self.note("read", &path);
        let (dir, name) = split_last(path.as_bytes());
        let tree = self.tree.lock().unwrap();
        let node = tree
            .get(&dir)
            .and_then(|v| v.iter().find(|n| n.name == name))
            .ok_or(StatusCode::NoSuchFile)?;
        let start = offset.min(node.data.len() as u64) as usize;
        if start >= node.data.len() {
            // 到尾巴要报 Eof,不是空 Data:客户端拿空 Data 会当成
            // 「这一次没读到,再试一次」,变成死循环。
            return Err(StatusCode::Eof);
        }
        let end = (start + len as usize).min(node.data.len());
        Ok(russh_sftp::protocol::Data {
            id,
            data: node.data[start..end].to_vec(),
        })
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, Self::Error> {
        let path = self.files.get(&handle).ok_or(StatusCode::Failure)?.clone();
        let (dir, name) = split_last(path.as_bytes());
        let mut tree = self.tree.lock().unwrap();
        let node = tree
            .get_mut(&dir)
            .and_then(|v| v.iter_mut().find(|n| n.name == name))
            .ok_or(StatusCode::NoSuchFile)?;
        // 按 offset 落位而不是 append:客户端会 pipeline 多个 WRITE,
        // 应答顺序不保证,append 的话字节会乱序。
        let end = offset as usize + data.len();
        if node.data.len() < end {
            node.data.resize(end, 0);
        }
        node.data[offset as usize..end].copy_from_slice(&data);
        node.size = node.data.len() as u64;
        Ok(ok_status(id))
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        // 也要记:「开了目录有没有关」是 channel/handle 泄漏那类 bug 的唯一
        // 探针,漏记这一条 Probe 就查不出来(模块文档承诺了每个请求都记)。
        self.note("close", &handle);
        self.dirs.remove(&handle);
        self.files.remove(&handle);
        Ok(ok_status(id))
    }

    async fn readlink(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        self.note("readlink", &path);
        let (dir, name) = split_last(path.as_bytes());
        let tree = self.tree.lock().unwrap();
        let node = tree
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

    /// F220 的 SFTP 回退路径要用它重建链接:`linkpath` 处新建一条指向
    /// `targetpath` 的链接。父目录不存在就拒,与 `mkdir`/`open` 一致。
    async fn symlink(
        &mut self,
        id: u32,
        linkpath: String,
        targetpath: String,
    ) -> Result<Status, Self::Error> {
        self.note("symlink", &linkpath);
        let (dir, name) = split_last(linkpath.as_bytes());
        let mut tree = self.tree.lock().unwrap();
        if !tree.contains_key(&dir) {
            return Err(StatusCode::NoSuchFile);
        }
        tree.entry(dir)
            .or_default()
            .push(Node::link(&name, targetpath.as_bytes()));
        Ok(ok_status(id))
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

    /// 删一个**文件或符号链接**。目录要走 `rmdir`(与真实 sshd 一致)——
    /// 这里对目录回 `Failure`,好让「客户端把目录当文件删」这个错扎得住。
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
            return Err(StatusCode::Failure);
        }
        v.remove(ix);
        Ok(ok_status(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
        self.note("rmdir", &path);
        let key = path.clone().into_bytes();
        let mut tree = self.tree.lock().unwrap();
        // **非空目录必须拒。** 递归删除的正确性全靠这一条兜底:实现要是
        // 漏删了里面的东西,这里会回 Failure 而不是静默成功。
        if tree.get(&key).is_some_and(|v| !v.is_empty()) {
            return Err(StatusCode::Failure);
        }
        if !tree.contains_key(&key) && !exists(&tree, &key) {
            return Err(StatusCode::NoSuchFile);
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
        // 不做 `mkdir -p`:父目录不存在就是失败,与真实 sshd 一致。
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
        if self.reject_rename {
            // 模拟 EXDEV(跨设备重命名失败):不动树,直接报错,逼调用方
            // 走「拷贝 + 删源」回退(`reject_rename` 字段文档)。
            return Err(StatusCode::Failure);
        }
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
        node.name = nn;
        tree.entry(nd).or_default().push(node);
        // 目录改名要把它那条子目录记录一起搬走,否则子树就成了孤儿。
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
        // 一条的 mode 存在**父目录**那条记录里(与 `attrs_of` 的查法一致)。
        let Some(v) = tree.get_mut(&dir) else {
            return Err(StatusCode::NoSuchFile);
        };
        let Some(n) = v.iter_mut().find(|n| n.name == name) else {
            return Err(StatusCode::NoSuchFile);
        };
        // **像真 sshd 一样照单全收**:客户端送了哪个字段就改哪个。
        // 服务端要是只认 permissions,「客户端顺手把 uid 一起送出去」
        // 这个 bug 在树上留不下任何痕迹,守护测试就恒绿了。
        if let Some(m) = attrs.permissions {
            n.mode = m & 0o7777;
        }
        if let Some(u) = attrs.uid {
            n.uid = u;
        }
        if let Some(g) = attrs.gid {
            n.gid = g;
        }
        if let Some(t) = attrs.mtime {
            n.mtime = t;
        }
        Ok(ok_status(id))
    }
}

fn ok_status(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: "Ok".into(),
        language_tag: "en-US".into(),
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
