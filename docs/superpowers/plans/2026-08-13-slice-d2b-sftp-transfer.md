# 切片 D2-b：SFTP 传输（上传 / 下载 / 队列 / 并发） Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 D2-a 建好的「远端单次写操作」扩成**双向文件传输**：右键上传 / 下载、后台队列、多 sftp channel 并发、`.part` 语义、冲突询问，界面上有一条常驻的队列面板。

**Architecture:**
`mullion-ssh` 只加**流式读写原语**（`RemoteFile`：分块读 / 分块写 / finish），本地磁盘 IO 一律留在 `mullion-app/src/files/local.rs` —— 依赖方向不变，`mullion-ssh` 仍然「只认字节流」。
队列是**纯逻辑状态机**（`files/queue.rs`，零 egui / 零 tokio），调度由 `app.rs` 每帧驱动：`queue.take_runnable()` 给出可以起的 job id，app 为每个 job spawn 一条**独立的 sftp channel**（F56 并发的实现方式，见设计 D8），进度经 `UserEvent` 回流。
冲突不靠 worker 持 channel 等用户——worker 探到目标已存在就**返回 `Conflict` 结束**，job 落到 `JobState::Conflict`，UI 弹窗，用户选完 `queue.resolve_conflict()` 把 job 放回 Pending 重新调度。这样 worker 无状态、可随时 abort，也天然支持「全部应用」。

**Tech Stack:** Rust / russh 0.54.5 / russh-sftp 2.4.0（`client::fs::File` 实现 `AsyncRead`+`AsyncWrite`，写侧内部有 pipeline）/ tokio / egui 0.30

**spec 编号：** F52（传输）、F55（队列）、F56（并发）、F59（进度）；设计决策 D8 / D16 / D19。

---

## 本切片**不做**（明确欠账，留 D2-c）

- **D22 标签宿主退避重连**——那是连接韧性，与传输正交；本切片断线时 job 直接 `Failed`，文案给「连接已断开」。
- **F56 并发度的设置项 UI**——本切片并发度是常量 `DEFAULT_CONCURRENCY = 4`，设计说的「1~8 可配」等 D2-c 接进会话档。
- **拖拽上传 / 拖出下载**（F52 的拖拽形态）——归 D3/D4，本切片只做右键菜单入口。
- **断点续传**——设计 F55 明确不做。

---

## File Structure

| 文件 | 职责 | 新建/改 |
|---|---|---|
| `crates/mullion-ssh/tests/common/sftp_server.rs` | 假服务端补 `open`/`read`/`write`/`close` 的**文件**分支 + `Node.data` | 改 |
| `crates/mullion-ssh/src/sftp.rs` | `RemoteFile` 流式读写原语 | 改 |
| `crates/mullion-ssh/tests/sftp_transfer.rs` | 传输原语的集成测试（打自家假服务端） | 新建 |
| `crates/mullion-app/src/files/transfer.rs` | 纯逻辑：`.part` 命名、Windows 非法名、冲突改名 | 新建 |
| `crates/mullion-app/src/files/queue.rs` | 纯逻辑：队列状态机、并发闸门、汇总/速率 | 新建 |
| `crates/mullion-app/src/files/local.rs` | 本地端流式读写 + 递归枚举 | 改 |
| `crates/mullion-app/src/ui/transfer_panel.rs` | 底部队列面板（折叠一行 / 展开列表） | 新建 |
| `crates/mullion-app/src/ui/files_dialog.rs` | 冲突对话框（覆盖/跳过/重命名/全部应用） | 改 |
| `crates/mullion-app/src/ui/files_panel.rs` | 右键菜单加「下载 / 上传」 | 改 |
| `crates/mullion-app/src/ui/mod.rs` | 接线 `transfer_panel` + `UiActions` 新字段 | 改 |
| `crates/mullion-app/src/app.rs` | 调度器、worker spawn、进度事件、节流重绘 | 改 |

---

## Task 1：假服务端支持文件内容读写

**Files:** Modify `crates/mullion-ssh/tests/common/sftp_server.rs`

- [ ] **Step 1：给 `Node` 加内容字段**

`Node` 加 `pub data: Vec<u8>`，`Node::dir` / `Node::file` 里填 `data: Vec::new()`，新增：

```rust
    /// 带内容的文件。`size` 由内容长度算 —— 传输测试断言「收到的字节
    /// 与树上的一致」，两者对不上时永远说不清是谁错了。
    pub fn file_with(name: &[u8], data: &[u8]) -> Self {
        let mut n = Self::file(name, data.len() as u64);
        n.data = data.to_vec();
        n
    }
```

- [ ] **Step 2：Handler 加文件句柄表**

`SftpHandler` 加字段 `files: HashMap<String, Vec<u8>>`（handle → 该 handle 对应的**完整路径**），`new()` 里 `files: HashMap::new()`。

- [ ] **Step 3：实现 `open` / `read` / `write` / `close`（文件分支）**

```rust
    async fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: russh_sftp::protocol::OpenFlags,
        _attrs: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        self.note("open", &filename);
        let path = filename.as_bytes().to_vec();
        let (dir, name) = split_last(&path);
        let mut tree = self.tree.lock().unwrap();
        let existed = tree
            .get(&dir)
            .is_some_and(|v| v.iter().any(|n| n.name == name));
        if pflags.contains(russh_sftp::protocol::OpenFlags::WRITE) {
            if !tree.contains_key(&dir) {
                return Err(StatusCode::NoSuchFile);
            }
            if existed && pflags.contains(russh_sftp::protocol::OpenFlags::EXCLUDE) {
                return Err(StatusCode::Failure);
            }
            if !existed {
                tree.entry(dir.clone())
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
        drop(tree);
        let handle = format!("f{}", self.next_handle);
        self.next_handle += 1;
        self.files.insert(handle.clone(), filename);
        Ok(Handle { id, handle })
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<russh_sftp::protocol::Data, Self::Error> {
        let path = self.files.get(&handle).ok_or(StatusCode::Failure)?.clone();
        let (dir, name) = split_last(path.as_bytes());
        let tree = self.tree.lock().unwrap();
        let node = tree
            .get(&dir)
            .and_then(|v| v.iter().find(|n| n.name == name))
            .ok_or(StatusCode::NoSuchFile)?;
        let start = offset.min(node.data.len() as u64) as usize;
        if start >= node.data.len() {
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
        let end = offset as usize + data.len();
        if node.data.len() < end {
            node.data.resize(end, 0);
        }
        node.data[offset as usize..end].copy_from_slice(&data);
        node.size = node.data.len() as u64;
        Ok(ok_status(id))
    }
```

`close` 已有目录分支，改成先摘 `self.files.remove(&handle)`，摘到就返回 ok；摘不到再走原来的 `dirs` 分支。

- [ ] **Step 4：跑 `cargo test -p mullion-ssh` 确认既有 SFTP 测试仍绿**（这一步只加能力，不该动到任何既有断言）

- [ ] **Step 5：提交**

```bash
git add crates/mullion-ssh/tests/common/sftp_server.rs
git commit -m "test(ssh): 假 SFTP 服务端支持文件内容读写 (F52)"
```

---

## Task 2：`mullion-ssh` 流式传输原语

**Files:** Modify `crates/mullion-ssh/src/sftp.rs`; Create `crates/mullion-ssh/tests/sftp_transfer.rs`

- [ ] **Step 1：写失败的集成测试**

`crates/mullion-ssh/tests/sftp_transfer.rs`：

```rust
//! F52:传输原语打自家假 SFTP 服务端。判据一律是**服务端内存树上的字节**,
//! 不是客户端自己报的成功 —— 「写请求发出去了」和「远端真的变成这样了」
//! 是两回事,只信前者的测试在协议写错时照样绿。
mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use common::sftp_server::{Node, Probe, Tree};
use mullion_ssh::sftp::{RemotePath, SftpClient};

fn tree_with(files: Vec<Node>) -> Tree {
    let mut t: Tree = HashMap::new();
    t.insert(b"/".to_vec(), vec![Node::dir(b"home")]);
    t.insert(b"/home".to_vec(), files);
    t
}

async fn connect(tree: Tree) -> (SftpClient, Arc<Mutex<Tree>>, Arc<Mutex<Probe>>) {
    let tree = Arc::new(Mutex::new(tree));
    let probe = Arc::new(Mutex::new(Probe::default()));
    let conn = common::spawn_sftp_server(tree.clone(), probe.clone()).await;
    let client = SftpClient::open(conn).await.expect("开 sftp");
    (client, tree, probe)
}

#[tokio::test]
async fn reading_a_remote_file_yields_every_byte_even_when_it_spans_many_chunks() {
    // 比一次 read 能拿的多得多:分块循环少写一轮的话,尾巴会静默丢失。
    let payload: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let (client, _tree, _probe) = tree_with(vec![Node::file_with(b"big.bin", &payload)])
        .pipe_connect()
        .await;
    let mut f = client
        .open_read(&RemotePath::from_bytes(b"/home/big.bin".to_vec()))
        .await
        .expect("打开");
    let mut got = Vec::new();
    let mut buf = vec![0u8; 32 * 1024];
    loop {
        let n = f.read_chunk(&mut buf).await.expect("读");
        if n == 0 {
            break;
        }
        got.extend_from_slice(&buf[..n]);
    }
    assert_eq!(got.len(), payload.len(), "读回来的长度不对");
    assert_eq!(got, payload, "读回来的内容不对");
}

#[tokio::test]
async fn writing_a_remote_file_lands_every_byte_on_the_server() {
    let payload: Vec<u8> = (0..150_000u32).map(|i| (i % 97) as u8).collect();
    let (client, tree, _probe) = tree_with(vec![]).pipe_connect().await;
    let dst = RemotePath::from_bytes(b"/home/out.bin".to_vec());
    let mut f = client.open_write(&dst, true).await.expect("打开写");
    for chunk in payload.chunks(30_000) {
        f.write_chunk(chunk).await.expect("写");
    }
    f.finish().await.expect("收尾");

    let t = tree.lock().unwrap();
    let node = t[&b"/home".to_vec()]
        .iter()
        .find(|n| n.name == b"out.bin")
        .expect("服务端上没有这个文件");
    assert_eq!(node.data, payload, "服务端上的字节与写入的不一致");
}

#[tokio::test]
async fn a_non_utf8_path_never_reaches_the_wire() {
    // D16:非 UTF-8 名不发请求。判据是探针里一条 open 都没有。
    let (client, _tree, probe) = tree_with(vec![]).pipe_connect().await;
    let bad = RemotePath::from_bytes(vec![b'/', b'h', 0xff, b'x']);
    assert!(client.open_read(&bad).await.is_err(), "非 UTF-8 名不该成功");
    assert!(
        client.open_write(&bad, true).await.is_err(),
        "非 UTF-8 名不该成功"
    );
    assert!(
        probe.lock().unwrap().paths_for("open").is_empty(),
        "非 UTF-8 名竟然发出了 open:{:?}",
        probe.lock().unwrap().paths_for("open")
    );
}

/// 小工具:让上面三个测试都能写成 `tree.pipe_connect().await`。
trait PipeConnect {
    async fn pipe_connect(self) -> (SftpClient, Arc<Mutex<Tree>>, Arc<Mutex<Probe>>);
}
impl PipeConnect for Tree {
    async fn pipe_connect(self) -> (SftpClient, Arc<Mutex<Tree>>, Arc<Mutex<Probe>>) {
        connect(self).await
    }
}
```

> 注：`common::spawn_sftp_server` 若在既有 `common/mod.rs` 里叫别的名字，按实际名字改；不要新造一套服务端启动路径。

- [ ] **Step 2：跑测试确认失败**

```bash
cargo test -p mullion-ssh --test sftp_transfer 2>&1 | tail -20
```
预期：编译失败，`no method named open_read`。

- [ ] **Step 3：实现原语**

`crates/mullion-ssh/src/sftp.rs` 末尾（`impl SftpClient` 内）加：

```rust
    /// 打开一个远端文件**读**。返回的 `RemoteFile` 分块读,进度由调用方
    /// 按 `read_chunk` 的返回值累计 —— 传输层要在每块之后更新 UI,
    /// 一次性 `read_to_end` 会让 2GB 的文件在进度条上一动不动、
    /// 还把整个文件读进内存。
    pub async fn open_read(&self, path: &RemotePath) -> Result<RemoteFile, SftpError> {
        let wire = path.as_wire()?;
        let file = self
            .inner
            .open(wire)
            .await
            .map_err(|e| SftpError::Protocol(e.to_string()))?;
        Ok(RemoteFile { file })
    }

    /// 打开一个远端文件**写**。`truncate` 为真时截断已有内容。
    ///
    /// 刻意**不带** `EXCLUDE`:是否允许覆盖由上层的冲突策略决定
    /// (设计 D19),协议层再挡一道的话「用户明确选了覆盖」也会失败。
    pub async fn open_write(
        &self,
        path: &RemotePath,
        truncate: bool,
    ) -> Result<RemoteFile, SftpError> {
        let wire = path.as_wire()?;
        let mut flags = russh_sftp::protocol::OpenFlags::WRITE
            | russh_sftp::protocol::OpenFlags::CREATE;
        if truncate {
            flags |= russh_sftp::protocol::OpenFlags::TRUNCATE;
        }
        let file = self
            .inner
            .open_with_flags(wire, flags)
            .await
            .map_err(|e| SftpError::Protocol(e.to_string()))?;
        Ok(RemoteFile { file })
    }

    /// 目标在不在。冲突探测用 —— `stat` 也能做,但那条会把「不存在」
    /// 和「没权限」都变成 `Err`,冲突判断需要把两者分开。
    pub async fn exists(&self, path: &RemotePath) -> Result<bool, SftpError> {
        let wire = path.as_wire()?;
        Ok(self.inner.try_exists(wire).await.unwrap_or(false))
    }
```

文件末尾（`impl SftpClient` 之外）加：

```rust
/// 一个打开着的远端文件。分块读 / 分块写,**必须 `finish()` 收尾** ——
/// `russh_sftp` 的 `File` 有 Drop 兜底(`close_nowait`),但 Drop 里发出去的
/// 关闭请求没人等应答,上传完立刻去 rename 会撞上「文件还开着」。
pub struct RemoteFile {
    file: russh_sftp::client::fs::File,
}

impl RemoteFile {
    /// 读一块。返回 0 表示到文件尾。
    pub async fn read_chunk(&mut self, buf: &mut [u8]) -> Result<usize, SftpError> {
        use tokio::io::AsyncReadExt;
        self.file
            .read(buf)
            .await
            .map_err(|e| SftpError::Protocol(e.to_string()))
    }

    /// 写一块。内部会 pipeline 多个 WRITE 请求(见 `russh_sftp` 的
    /// `max_concurrent_writes`),所以高延迟链路上写比读快得多 ——
    /// 这也是设计 D8 说「下载比上传慢」的由来。
    pub async fn write_chunk(&mut self, buf: &[u8]) -> Result<(), SftpError> {
        use tokio::io::AsyncWriteExt;
        self.file
            .write_all(buf)
            .await
            .map_err(|e| SftpError::Protocol(e.to_string()))
    }

    /// 冲干净并关闭。**等应答**,见结构体文档。
    pub async fn finish(mut self) -> Result<(), SftpError> {
        use tokio::io::AsyncWriteExt;
        self.file
            .flush()
            .await
            .map_err(|e| SftpError::Protocol(e.to_string()))?;
        self.file
            .close()
            .await
            .map_err(|e| SftpError::Protocol(e.to_string()))
    }
}
```

- [ ] **Step 4：跑测试确认通过**

```bash
cargo test -p mullion-ssh --test sftp_transfer 2>&1 | tail -20
```

- [ ] **Step 5：变异验收**

至少三处，每处单独跑、跑完还原（`cp` 备份还原，**不要** `git checkout`）：
1. `read_chunk` 里把 `buf` 换成 `&mut buf[..1]` 之外的短路——改成「读满一次就返回 0」→ 长度断言必须红。
2. `open_write` 去掉 `TRUNCATE` → 覆盖测试若加了残留断言必须红（本任务先记着，Task 5 补）。
3. `open_read`/`open_write` 去掉 `as_wire()?` 改成 `String::from_utf8_lossy` → 非 UTF-8 探针断言必须红。

- [ ] **Step 6：提交**

```bash
git add crates/mullion-ssh/src/sftp.rs crates/mullion-ssh/tests/sftp_transfer.rs
git commit -m "feat(ssh): SFTP 流式读写原语 RemoteFile,本地磁盘 IO 不进本 crate (F52)"
```

---

## Task 3：传输纯逻辑（`.part` / Windows 非法名 / 改名）

**Files:** Create `crates/mullion-app/src/files/transfer.rs`; Modify `crates/mullion-app/src/files/mod.rs`

- [ ] **Step 1：写测试（与实现同文件的 `#[cfg(test)] mod tests`）**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_target_is_written_through_a_part_file_so_a_crash_leaves_no_half_file() {
        assert_eq!(staging_name("a.bin", false), "a.bin.mullion-part");
    }

    #[test]
    fn overwriting_an_existing_target_writes_in_place_to_keep_inode_and_permissions() {
        // D19:覆盖不能走 .part+rename —— rename 会换 inode,
        // 属主/权限/ACL/硬链接全丢,而用户以为只是「换了内容」。
        assert_eq!(staging_name("a.bin", true), "a.bin");
    }

    #[test]
    fn windows_reserved_characters_are_reported_instead_of_being_silently_rewritten() {
        // D16:静默改写会让「下下来的文件叫什么」无法预测,
        // 再上传回去就是另一个文件了。
        let bad = illegal_on_windows("a:b?.txt");
        assert!(bad.is_some(), "冒号和问号应当被判非法");
        assert_eq!(bad.unwrap(), "a_b_.txt", "建议名应把非法字符换成下划线");
        assert!(illegal_on_windows("普通名字.txt").is_none());
    }

    #[test]
    fn windows_reserved_device_names_are_reported_too() {
        assert!(illegal_on_windows("CON").is_some(), "CON 是设备名");
        assert!(illegal_on_windows("nul.txt").is_some(), "带扩展名也仍是设备名");
        assert!(illegal_on_windows("console.txt").is_none(), "只是前缀相同,合法");
    }

    #[test]
    fn renaming_on_conflict_inserts_the_counter_before_the_extension() {
        let taken = |n: &str| ["a.tar.gz", "a (1).tar.gz"].contains(&n);
        assert_eq!(dedup_name("a.tar.gz", taken), "a (2).tar.gz");
    }

    #[test]
    fn renaming_a_dotfile_does_not_treat_the_leading_dot_as_an_extension() {
        // `.bashrc` 拆成 ""+".bashrc" 的话会生成 " (1).bashrc",很难看且
        // 不再是隐藏文件。
        let taken = |n: &str| n == ".bashrc";
        assert_eq!(dedup_name(".bashrc", taken), ".bashrc (1)");
    }
}
```

- [ ] **Step 2：跑测试确认失败**

```bash
cargo test -p mullion-app files::transfer 2>&1 | tail -20
```
预期：`cannot find function staging_name`。

- [ ] **Step 3：实现**

`crates/mullion-app/src/files/transfer.rs`：

```rust
//! 传输的**纯逻辑**:落盘用的临时名、Windows 文件名合法性、冲突改名。
//! 零 egui / 零 tokio / 零 IO —— 这三件事全是「算错了才发现」的类型,
//! 必须能在没有网络、没有窗口的情况下单测。

/// 写入时实际用的名字。
///
/// 设计 D19:**新建**目标先写 `<name>.mullion-part` 再 rename,断线留下的
/// 半截文件一眼能认出来、也不会被误当成完整文件;**覆盖**已存在的目标
/// 则直接写,不走 rename —— rename 会换掉 inode,属主 / 权限 / ACL /
/// 硬链接全部丢失,而用户的心智模型只是「换了内容」。
pub const PART_SUFFIX: &str = ".mullion-part";

pub fn staging_name(final_name: &str, overwriting: bool) -> String {
    if overwriting {
        final_name.to_string()
    } else {
        format!("{final_name}{PART_SUFFIX}")
    }
}

/// Windows 上非法的文件名 → `Some(建议名)`;合法 → `None`。
///
/// 设计 D16:**打断并给建议名**,不静默改写。静默改写的后果是
/// 「下下来的文件到底叫什么」无法预测,再传回去就成了另一个文件。
pub fn illegal_on_windows(name: &str) -> Option<String> {
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
        "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    let stem = name.split('.').next().unwrap_or("");
    let reserved = RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem));
    let bad_char = |c: char| matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
        || (c as u32) < 0x20;
    // 结尾的点和空格 Windows 会静默吃掉,等于换了个名字。
    let bad_tail = name.ends_with('.') || name.ends_with(' ');
    if !reserved && !name.chars().any(bad_char) && !bad_tail {
        return None;
    }
    let mut fixed: String = name
        .chars()
        .map(|c| if bad_char(c) { '_' } else { c })
        .collect();
    while fixed.ends_with('.') || fixed.ends_with(' ') {
        fixed.pop();
    }
    if reserved {
        fixed.insert(0, '_');
    }
    if fixed.is_empty() {
        fixed.push('_');
    }
    Some(fixed)
}

/// 冲突选「重命名」时生成的新名字:`a.tar.gz` → `a (1).tar.gz`。
///
/// `taken` 由调用方提供(远端要发 stat、本地查磁盘),这里只管算名字。
pub fn dedup_name(name: &str, taken: impl Fn(&str) -> bool) -> String {
    let (stem, ext) = split_ext(name);
    for i in 1..10_000 {
        let cand = if ext.is_empty() {
            format!("{stem} ({i})")
        } else {
            format!("{stem} ({i}).{ext}")
        };
        if !taken(&cand) {
            return cand;
        }
    }
    // 一万个重名是病态输入;给一个必然不同的兜底,别 panic。
    format!("{name}.dup")
}

/// 拆成「主干 + 扩展名」。`.bashrc` 整个算主干(开头的点不是扩展名分隔),
/// `a.tar.gz` 只把最后一段当扩展名。
fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(0) | None => (name, ""),
        Some(i) => (&name[..i], &name[i + 1..]),
    }
}
```

`crates/mullion-app/src/files/mod.rs` 加 `pub mod transfer;`（跟着已有的 `pub mod local; pub mod state;`）。

- [ ] **Step 4：跑测试确认通过**

```bash
cargo test -p mullion-app files::transfer 2>&1 | tail -20
```

- [ ] **Step 5：变异验收（至少 4 处）**

1. `staging_name` 的 `overwriting` 分支反过来 → 两条 `.part` 测试必须红。
2. `illegal_on_windows` 去掉 `reserved` 判断 → 设备名测试必须红。
3. `illegal_on_windows` 去掉 `bad_tail` → 需补一条 `"a."` 的断言（若测试没覆盖，补上再跑）。
4. `split_ext` 的 `Some(0)` 分支删掉 → dotfile 测试必须红。

- [ ] **Step 6：提交**

```bash
git add crates/mullion-app/src/files/transfer.rs crates/mullion-app/src/files/mod.rs
git commit -m "feat(app): 传输纯逻辑——.part 命名、Windows 非法名、冲突改名 (F52/D16/D19)"
```

---

## Task 4：传输队列状态机

**Files:** Create `crates/mullion-app/src/files/queue.rs`; Modify `crates/mullion-app/src/files/mod.rs`

- [ ] **Step 1：写测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn q() -> Queue {
        Queue::new(2)
    }
    fn job(dir: Direction) -> NewJob {
        NewJob {
            dir,
            generation: 7,
            label: "x".into(),
            total: 100,
        }
    }

    #[test]
    fn the_concurrency_limit_caps_how_many_jobs_run_at_once() {
        // F56:闸门开太大就是同时开 N 条 sftp channel,高延迟链路上
        // 每条都在抢同一个 TCP 窗口,总吞吐反而掉。
        let mut q = q();
        for _ in 0..5 {
            q.push(job(Direction::Download));
        }
        assert_eq!(q.take_runnable().len(), 2, "第一轮只该放行 2 个");
        assert!(q.take_runnable().is_empty(), "闸门满了不该再放行");
    }

    #[test]
    fn finishing_a_job_frees_a_slot_for_the_next_one() {
        let mut q = q();
        for _ in 0..3 {
            q.push(job(Direction::Download));
        }
        let first = q.take_runnable();
        q.finish(first[0], Ok(()));
        assert_eq!(q.take_runnable().len(), 1, "腾出一个槽就该放行一个");
    }

    #[test]
    fn a_single_failure_does_not_stop_the_rest_of_the_queue() {
        // F55 明确要求:一条失败不掀桌子。
        let mut q = q();
        let a = q.push(job(Direction::Upload));
        let b = q.push(job(Direction::Upload));
        let running = q.take_runnable();
        assert_eq!(running.len(), 2);
        q.finish(a, Err("炸了".into()));
        assert!(
            matches!(q.get(b).unwrap().state, JobState::Running),
            "另一条不该被连坐"
        );
        assert!(matches!(q.get(a).unwrap().state, JobState::Failed(_)));
    }

    #[test]
    fn a_conflicted_job_waits_for_the_user_and_reruns_after_the_choice() {
        let mut q = q();
        let a = q.push(job(Direction::Download));
        q.take_runnable();
        q.finish(a, Err(JobError::Conflict.into()));
        assert!(matches!(q.get(a).unwrap().state, JobState::Conflict));
        assert!(
            q.take_runnable().is_empty(),
            "等用户拿主意期间不许自己重跑"
        );
        q.resolve_conflict(a, Conflict::Overwrite, false);
        assert_eq!(q.take_runnable(), vec![a], "选完了该重新排上");
        assert_eq!(q.get(a).unwrap().resolved, Some(Conflict::Overwrite));
    }

    #[test]
    fn apply_to_all_answers_later_conflicts_without_asking_again() {
        let mut q = Queue::new(1);
        let a = q.push(job(Direction::Download));
        let b = q.push(job(Direction::Download));
        q.take_runnable();
        q.finish(a, Err(JobError::Conflict.into()));
        q.resolve_conflict(a, Conflict::Skip, true);
        q.take_runnable();
        q.finish(a, Ok(()));
        q.take_runnable();
        q.finish(b, Err(JobError::Conflict.into()));
        assert!(
            !matches!(q.get(b).unwrap().state, JobState::Conflict),
            "已经选过「全部应用」就不该再拦一次:{:?}",
            q.get(b).unwrap().state
        );
    }

    #[test]
    fn skipping_a_conflict_finishes_the_job_instead_of_transferring_it() {
        let mut q = Queue::new(1);
        let a = q.push(job(Direction::Download));
        q.take_runnable();
        q.finish(a, Err(JobError::Conflict.into()));
        q.resolve_conflict(a, Conflict::Skip, false);
        assert!(
            matches!(q.get(a).unwrap().state, JobState::Skipped),
            "选了跳过就该直接收尾,而不是排队重跑"
        );
        assert!(q.take_runnable().is_empty());
    }

    #[test]
    fn canceling_a_pending_job_never_lets_it_start() {
        let mut q = Queue::new(1);
        let a = q.push(job(Direction::Upload));
        let b = q.push(job(Direction::Upload));
        q.take_runnable();
        q.cancel(b);
        q.finish(a, Ok(()));
        assert!(q.take_runnable().is_empty(), "取消掉的不该被调度");
    }

    #[test]
    fn the_summary_counts_only_unfinished_bytes_so_the_bar_does_not_jump_backwards() {
        let mut q = Queue::new(4);
        let a = q.push(job(Direction::Upload));
        let b = q.push(job(Direction::Download));
        q.take_runnable();
        q.progress(a, 40);
        let s = q.summary();
        assert_eq!((s.up, s.down), (1, 1), "上下行各一条在跑");
        assert_eq!(s.bytes_total, 200);
        assert_eq!(s.bytes_done, 40);
        q.finish(b, Ok(()));
        let s = q.summary();
        assert_eq!(s.bytes_done, 140, "完成的那条按 total 计入,不能倒退");
    }

    #[test]
    fn clearing_finished_jobs_keeps_the_ones_still_in_flight() {
        let mut q = Queue::new(4);
        let a = q.push(job(Direction::Upload));
        let b = q.push(job(Direction::Upload));
        q.take_runnable();
        q.finish(a, Ok(()));
        q.clear_finished();
        assert_eq!(q.jobs().len(), 1);
        assert_eq!(q.jobs()[0].id, b);
    }

    #[test]
    fn the_rate_meter_reports_bytes_per_second_from_two_samples() {
        let mut m = RateMeter::default();
        assert_eq!(m.sample(0.0, 0), 0.0, "第一次采样没有区间,给 0");
        assert_eq!(m.sample(2.0, 2_000), 1_000.0);
    }

    #[test]
    fn the_rate_meter_ignores_a_zero_length_interval_instead_of_dividing_by_zero() {
        let mut m = RateMeter::default();
        m.sample(1.0, 100);
        let r = m.sample(1.0, 500);
        assert!(r.is_finite(), "同一时刻两次采样不能算出 inf:{r}");
    }
}
```

- [ ] **Step 2：跑测试确认失败**

```bash
cargo test -p mullion-app files::queue 2>&1 | tail -20
```

- [ ] **Step 3：实现**

`crates/mullion-app/src/files/queue.rs`：

```rust
//! 传输队列的**纯逻辑**(F55/F56)。零 egui / 零 tokio / 零 IO ——
//! 「并发闸门放行几个」「一条失败会不会连坐」「冲突选完有没有重排」
//! 这些全是状态机 bug,得能在没有网络的情况下复现。
//!
//! 调度的形状:`app.rs` 每帧调一次 [`Queue::take_runnable`],拿到的 id
//! 各起一条 sftp channel 跑;worker 只回报结果,**不碰队列** ——
//! 队列的所有权在 UI 线程,不需要锁。

/// 传输方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Upload,
    Download,
}

/// 冲突时的处置。**没有「静默覆盖」** —— F55 的硬要求。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conflict {
    Overwrite,
    Skip,
    Rename,
}

/// worker 回报的失败原因。`Conflict` 单独一档:它不是错误,是「需要
/// 用户拿主意」,混进 `Failed` 的话队列会把它当成永久失败扔掉。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobError {
    Conflict,
    Other(String),
}

impl From<JobError> for String {
    fn from(e: JobError) -> String {
        match e {
            JobError::Conflict => "目标已存在".into(),
            JobError::Other(m) => m,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobState {
    Pending,
    Running,
    /// 等用户在冲突对话框里拿主意。
    Conflict,
    Done,
    Skipped,
    Canceled,
    Failed(String),
}

impl JobState {
    pub fn is_finished(&self) -> bool {
        matches!(
            self,
            JobState::Done | JobState::Skipped | JobState::Canceled | JobState::Failed(_)
        )
    }
}

/// 入队时要填的东西。
pub struct NewJob {
    pub dir: Direction,
    /// S1:属主标签的世代。异步结果按它路由,永远不投给活动标签。
    pub generation: u64,
    /// 界面上显示的一行(通常是文件名)。
    pub label: String,
    pub total: u64,
}

pub struct Job {
    pub id: u64,
    pub dir: Direction,
    pub generation: u64,
    pub label: String,
    pub total: u64,
    pub done: u64,
    pub state: JobState,
    /// 用户对这一条的冲突处置。worker 起跑时读它决定覆盖 / 改名。
    pub resolved: Option<Conflict>,
}

#[derive(Debug, Default, PartialEq)]
pub struct Summary {
    /// 在跑的上行 / 下行条数。
    pub up: usize,
    pub down: usize,
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// 还有没有没收尾的活。折叠面板要不要显示看它。
    pub busy: bool,
}

pub struct Queue {
    jobs: Vec<Job>,
    next_id: u64,
    concurrency: usize,
    /// 「全部应用」选过之后的默认处置。
    blanket: Option<Conflict>,
}

impl Queue {
    pub fn new(concurrency: usize) -> Self {
        Self {
            jobs: Vec::new(),
            next_id: 1,
            concurrency: concurrency.max(1),
            blanket: None,
        }
    }

    pub fn jobs(&self) -> &[Job] {
        &self.jobs
    }

    pub fn get(&self, id: u64) -> Option<&Job> {
        self.jobs.iter().find(|j| j.id == id)
    }

    fn get_mut(&mut self, id: u64) -> Option<&mut Job> {
        self.jobs.iter_mut().find(|j| j.id == id)
    }

    pub fn push(&mut self, n: NewJob) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.jobs.push(Job {
            id,
            dir: n.dir,
            generation: n.generation,
            label: n.label,
            total: n.total,
            done: 0,
            state: JobState::Pending,
            resolved: None,
        });
        id
    }

    /// 放行一批可以起跑的 job,并把它们置成 `Running`。
    /// **同一个 id 不会被放行两次** —— 状态就地改掉了。
    pub fn take_runnable(&mut self) -> Vec<u64> {
        let running = self
            .jobs
            .iter()
            .filter(|j| j.state == JobState::Running)
            .count();
        let mut slots = self.concurrency.saturating_sub(running);
        let mut out = Vec::new();
        for j in self.jobs.iter_mut() {
            if slots == 0 {
                break;
            }
            if j.state == JobState::Pending {
                j.state = JobState::Running;
                out.push(j.id);
                slots -= 1;
            }
        }
        out
    }

    pub fn progress(&mut self, id: u64, done: u64) {
        if let Some(j) = self.get_mut(id) {
            j.done = done;
        }
    }

    /// worker 收工。`Err(Conflict)` 会落到 `JobState::Conflict` 等用户;
    /// 已经有「全部应用」的话直接按那个处置走,不再打扰。
    pub fn finish(&mut self, id: u64, result: Result<(), String>) {
        let blanket = self.blanket;
        let Some(j) = self.get_mut(id) else { return };
        match result {
            Ok(()) => {
                j.done = j.total;
                j.state = JobState::Done;
            }
            Err(msg) if msg == String::from(JobError::Conflict) => {
                j.state = JobState::Conflict;
            }
            Err(msg) => j.state = JobState::Failed(msg),
        }
        if j.state == JobState::Conflict {
            if let Some(c) = blanket {
                self.apply_conflict(id, c);
            }
        }
    }

    /// 用户在冲突对话框里选完了。`apply_all` 会把这个处置记成默认。
    pub fn resolve_conflict(&mut self, id: u64, choice: Conflict, apply_all: bool) {
        if apply_all {
            self.blanket = Some(choice);
        }
        self.apply_conflict(id, choice);
    }

    fn apply_conflict(&mut self, id: u64, choice: Conflict) {
        let Some(j) = self.get_mut(id) else { return };
        if j.state != JobState::Conflict {
            return;
        }
        match choice {
            // 跳过不需要再跑一趟网络,直接收尾。
            Conflict::Skip => j.state = JobState::Skipped,
            Conflict::Overwrite | Conflict::Rename => {
                j.resolved = Some(choice);
                j.state = JobState::Pending;
            }
        }
    }

    /// 还在等用户处置的第一条(对话框一次只问一个)。
    pub fn first_conflict(&self) -> Option<&Job> {
        self.jobs.iter().find(|j| j.state == JobState::Conflict)
    }

    pub fn cancel(&mut self, id: u64) {
        if let Some(j) = self.get_mut(id) {
            if !j.state.is_finished() {
                j.state = JobState::Canceled;
            }
        }
    }

    pub fn cancel_all(&mut self) {
        let ids: Vec<u64> = self
            .jobs
            .iter()
            .filter(|j| !j.state.is_finished())
            .map(|j| j.id)
            .collect();
        for id in ids {
            self.cancel(id);
        }
    }

    pub fn clear_finished(&mut self) {
        self.jobs.retain(|j| !j.state.is_finished());
    }

    /// 属主标签关掉时把它的 job 全部作废 —— 留着会往一个已经没了的
    /// 世代上派活,worker 起跑时找不到 sftp client 只能干等。
    pub fn cancel_generation(&mut self, generation: u64) {
        let ids: Vec<u64> = self
            .jobs
            .iter()
            .filter(|j| j.generation == generation && !j.state.is_finished())
            .map(|j| j.id)
            .collect();
        for id in ids {
            self.cancel(id);
        }
    }

    pub fn summary(&self) -> Summary {
        let mut s = Summary::default();
        for j in &self.jobs {
            match j.state {
                JobState::Running => match j.dir {
                    Direction::Upload => s.up += 1,
                    Direction::Download => s.down += 1,
                },
                _ => {}
            }
            if j.state.is_finished() && j.state != JobState::Done {
                // 跳过 / 取消 / 失败的不该把分母撑大,否则进度条永远到不了头。
                continue;
            }
            s.bytes_total += j.total;
            // 完成的按 total 计 —— 用 done 的话最后一次进度上报要是丢了,
            // 进度条会卡在 99%。
            s.bytes_done += if j.state == JobState::Done {
                j.total
            } else {
                j.done.min(j.total)
            };
            if !j.state.is_finished() {
                s.busy = true;
            }
        }
        s
    }
}

/// 速率估计。`now` 用**秒**传进来(调用方拿 `Instant`),于是这里
/// 不碰时钟、可纯单测。
#[derive(Default)]
pub struct RateMeter {
    last: Option<(f64, u64)>,
    bps: f64,
}

impl RateMeter {
    pub fn sample(&mut self, now_secs: f64, total_done: u64) -> f64 {
        if let Some((t0, b0)) = self.last {
            let dt = now_secs - t0;
            if dt > 0.0 {
                let db = total_done.saturating_sub(b0) as f64;
                // 指数平滑:不平滑的话数字每帧乱跳,读都读不出来。
                let inst = db / dt;
                self.bps = if self.bps == 0.0 {
                    inst
                } else {
                    self.bps * 0.7 + inst * 0.3
                };
                self.last = Some((now_secs, total_done));
            }
        } else {
            self.last = Some((now_secs, total_done));
        }
        self.bps
    }

    pub fn bps(&self) -> f64 {
        self.bps
    }
}
```

`files/mod.rs` 加 `pub mod queue;`。

- [ ] **Step 4：跑测试确认通过**

- [ ] **Step 5：变异验收（至少 5 处）**

1. `take_runnable` 的 `slots` 改成 `self.concurrency`（忽略在跑数）→ 闸门测试红。
2. `finish` 的 `Conflict` 分支改成 `Failed` → 冲突测试红。
3. `apply_conflict` 的 `Skip` 改成也置 `Pending` → 跳过测试红。
4. `summary` 里完成的按 `j.done` 计 → 「不能倒退」测试红。
5. `RateMeter::sample` 去掉 `dt > 0.0` 判断 → 除零测试红。

- [ ] **Step 6：提交**

```bash
git add crates/mullion-app/src/files/queue.rs crates/mullion-app/src/files/mod.rs
git commit -m "feat(app): 传输队列状态机——并发闸门、冲突挂起、失败不连坐 (F55/F56)"
```

---

## Task 5：本地端流式读写 + 递归枚举

**Files:** Modify `crates/mullion-app/src/files/local.rs`

- [ ] **Step 1：写测试（追加到既有 `mod tests`）**

```rust
    #[test]
    fn walking_a_directory_yields_files_with_paths_relative_to_the_root() {
        // 递归传输靠这个把「一个目录」摊成「一串文件 + 它们在目标端的
        // 相对位置」。相对路径算错 = 文件全糊到根目录下。
        let tmp = tempdir();
        std::fs::create_dir_all(tmp.join("a/b")).unwrap();
        std::fs::write(tmp.join("a/one.txt"), b"1").unwrap();
        std::fs::write(tmp.join("a/b/two.txt"), b"22").unwrap();
        let mut got: Vec<(String, u64)> = walk_dir(&tmp.join("a"))
            .expect("枚举")
            .into_iter()
            .map(|w| (w.rel.join("/"), w.size))
            .collect();
        got.sort();
        assert_eq!(
            got,
            vec![("b/two.txt".to_string(), 2), ("one.txt".to_string(), 1)]
        );
    }

    #[test]
    fn walking_does_not_follow_symlinks_out_of_the_tree() {
        // D17 同款理由:跟随链接会把 /etc 整个拖进传输队列。
        let tmp = tempdir();
        std::fs::create_dir_all(tmp.join("a")).unwrap();
        std::fs::write(tmp.join("target.txt"), b"x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(tmp.join("target.txt"), tmp.join("a/link")).unwrap();
        let got = walk_dir(&tmp.join("a")).expect("枚举");
        assert!(
            got.iter().all(|w| w.rel != vec!["link".to_string()]),
            "符号链接不该进传输列表:{:?}",
            got.iter().map(|w| w.rel.clone()).collect::<Vec<_>>()
        );
    }
```

`tempdir()` 用既有测试里的那个 helper；若没有，加：

```rust
    fn tempdir() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "mullion-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }
```

- [ ] **Step 2：跑测试确认失败**

- [ ] **Step 3：实现（追加到 `local.rs`，放在 `#[cfg(test)]` 之前）**

```rust
/// 递归枚举出来的一个文件。
pub struct Walked {
    /// 相对根目录的路径段。目标端按同样的层级重建。
    pub rel: Vec<String>,
    pub size: u64,
}

/// 递归枚举一个本地目录下的**普通文件**。
///
/// **不跟随符号链接**(设计 D17 同款理由:跟随会把链接指向的整棵树
/// 拖进队列,`/home/x/link -> /` 就是把根目录传一遍)。
pub fn walk_dir(root: &std::path::Path) -> Result<Vec<Walked>, String> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), Vec::<String>::new())];
    while let Some((dir, rel)) = stack.pop() {
        let rd = std::fs::read_dir(&dir).map_err(|e| format!("读不了 {}:{e}", dir.display()))?;
        for de in rd {
            let de = de.map_err(|e| format!("读不了 {}:{e}", dir.display()))?;
            // `symlink_metadata` 而不是 `metadata`:后者会解引用链接。
            let md = de
                .symlink_metadata()
                .map_err(|e| format!("读不了 {}:{e}", de.path().display()))?;
            let name = de.file_name().to_string_lossy().into_owned();
            let mut child_rel = rel.clone();
            child_rel.push(name);
            if md.is_symlink() {
                continue;
            }
            if md.is_dir() {
                stack.push((de.path(), child_rel));
            } else if md.is_file() {
                out.push(Walked {
                    rel: child_rel,
                    size: md.len(),
                });
            }
        }
    }
    Ok(out)
}
```

- [ ] **Step 4：跑测试确认通过**

- [ ] **Step 5：变异验收（2 处）**：把 `symlink_metadata` 换成 `metadata` → 链接测试红；把 `child_rel` 换成只有 `name` → 相对路径测试红。

- [ ] **Step 6：提交**

```bash
git add crates/mullion-app/src/files/local.rs
git commit -m "feat(app): 本地目录递归枚举,不跟随符号链接 (F52/D17)"
```

---

## Task 6：冲突对话框

**Files:** Modify `crates/mullion-app/src/ui/files_dialog.rs`

- [ ] **Step 1：写测试**

追加到既有 `mod tests`：

```rust
    #[test]
    fn the_conflict_dialog_offers_four_choices_and_never_overwrites_by_default() {
        // F55 硬要求:绝不静默覆盖。四个按钮少一个都不行。
        let mut d = Some(FilesDialog::Conflict {
            name: "a.bin".into(),
            job: 3,
            apply_all: false,
        });
        let (_out, texts) = run_dialog(&mut d);
        for label in ["覆盖", "跳过", "重命名", "取消"] {
            assert!(
                texts.iter().any(|t| t == label),
                "冲突对话框少了「{label}」:{texts:?}"
            );
        }
    }

    #[test]
    fn choosing_overwrite_reports_the_job_id_and_the_apply_all_flag() {
        let mut d = Some(FilesDialog::Conflict {
            name: "a.bin".into(),
            job: 3,
            apply_all: true,
        });
        let out = click_dialog_button(&mut d, "覆盖");
        assert_eq!(
            out,
            Some(FileOp::Resolve {
                job: 3,
                choice: crate::files::queue::Conflict::Overwrite,
                apply_all: true,
            })
        );
        assert!(d.is_none(), "选完该关掉");
    }
```

`run_dialog` / `click_dialog_button` 用本文件既有的 `find_button_pos` / `click_button` helper 包一层；若既有 helper 名字不同，按实际的写，不要另造一套。

- [ ] **Step 2：跑测试确认失败**

- [ ] **Step 3：实现**

`FilesDialog` 加变体：

```rust
    /// F55:目标已存在。`job` 是队列里那一条的 id;`apply_all` 是那个
    /// 勾选框的状态(勾上之后同一批后续冲突不再问)。
    Conflict {
        name: String,
        job: u64,
        apply_all: bool,
    },
```

`FileOp` 加变体：

```rust
    /// 冲突处置。**不带路径** —— 具体怎么落盘由 worker 按 `choice` 决定,
    /// UI 只负责把用户的选择传回去。
    Resolve {
        job: u64,
        choice: crate::files::queue::Conflict,
        apply_all: bool,
    },
```

`show()` 的 match 加分支：

```rust
        FilesDialog::Conflict {
            name,
            job,
            apply_all,
        } => {
            let job = *job;
            modal(ctx, t, "文件已存在", |ui| {
                ui.label(format!("目标位置已有「{name}」。"));
                ui.add_space(8.0);
                ui.checkbox(apply_all, "对本批后续冲突都这么办");
                ui.add_space(8.0);
                let all = *apply_all;
                ui.horizontal(|ui| {
                    for (label, choice) in [
                        ("覆盖", crate::files::queue::Conflict::Overwrite),
                        ("跳过", crate::files::queue::Conflict::Skip),
                        ("重命名", crate::files::queue::Conflict::Rename),
                    ] {
                        if ui.button(label).clicked() {
                            op = Some(FileOp::Resolve {
                                job,
                                choice,
                                apply_all: all,
                            });
                            close = true;
                        }
                    }
                    if ui.button("取消").clicked() {
                        op = Some(FileOp::Resolve {
                            job,
                            choice: crate::files::queue::Conflict::Skip,
                            apply_all: false,
                        });
                        close = true;
                    }
                });
            });
        }
```

> 「取消」也走 `Skip`：对话框关掉而 job 永远挂在 `Conflict` 上，队列就再也走不动了。

- [ ] **Step 4：跑测试确认通过**

- [ ] **Step 5：变异验收（2 处）**：删掉「重命名」按钮 → 四选项测试红；把「取消」改成不发 op → 需补一条断言（补上再跑）。

- [ ] **Step 6：提交**

```bash
git add crates/mullion-app/src/ui/files_dialog.rs
git commit -m "feat(ui): 冲突对话框(覆盖/跳过/重命名/全部应用),绝不静默覆盖 (F55)"
```

---

## Task 7：队列面板

**Files:** Create `crates/mullion-app/src/ui/transfer_panel.rs`; Modify `crates/mullion-app/src/ui/mod.rs`

- [ ] **Step 1：写测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::queue::{Direction, NewJob, Queue};

    fn texts(q: &mut Queue, collapsed: &mut bool) -> Vec<String> {
        let ctx = egui::Context::default();
        let t = crate::theme::Theme::default();
        // Panel 的 fade_in 会让第一帧全是 Noop,必须跑两帧。
        let mut out = Vec::new();
        for _ in 0..2 {
            let o = ctx.run(Default::default(), |ctx| {
                show(ctx, &t, q, collapsed);
            });
            out = o
                .shapes
                .iter()
                .filter_map(|c| match &c.shape {
                    egui::Shape::Text(ts) => Some(ts.galley.text().to_string()),
                    _ => None,
                })
                .collect();
        }
        out
    }

    #[test]
    fn an_empty_queue_draws_nothing_so_it_does_not_eat_terminal_rows() {
        let mut q = Queue::new(4);
        let mut collapsed = true;
        assert!(
            texts(&mut q, &mut collapsed).is_empty(),
            "队列空时不该占地方"
        );
    }

    #[test]
    fn the_collapsed_summary_shows_both_directions_and_the_rate() {
        let mut q = Queue::new(4);
        q.push(NewJob {
            dir: Direction::Upload,
            generation: 1,
            label: "a".into(),
            total: 100,
        });
        q.push(NewJob {
            dir: Direction::Download,
            generation: 1,
            label: "b".into(),
            total: 100,
        });
        q.take_runnable();
        let mut collapsed = true;
        let ts = texts(&mut q, &mut collapsed).join(" ");
        assert!(ts.contains("↑1"), "少了上行条数:{ts}");
        assert!(ts.contains("↓1"), "少了下行条数:{ts}");
    }

    #[test]
    fn the_expanded_list_names_every_job_so_a_failure_can_be_traced_to_a_file() {
        let mut q = Queue::new(4);
        q.push(NewJob {
            dir: Direction::Upload,
            generation: 1,
            label: "报告.pdf".into(),
            total: 100,
        });
        let mut collapsed = false;
        let ts = texts(&mut q, &mut collapsed).join(" ");
        assert!(ts.contains("报告.pdf"), "展开后应当看得到文件名:{ts}");
    }

    #[test]
    fn a_failed_job_shows_its_reason_instead_of_just_a_red_dot() {
        let mut q = Queue::new(4);
        let id = q.push(NewJob {
            dir: Direction::Upload,
            generation: 1,
            label: "a".into(),
            total: 100,
        });
        q.take_runnable();
        q.finish(id, Err("没权限".into()));
        let mut collapsed = false;
        let ts = texts(&mut q, &mut collapsed).join(" ");
        assert!(ts.contains("没权限"), "失败原因得写出来:{ts}");
    }
}
```

- [ ] **Step 2：跑测试确认失败**

- [ ] **Step 3：实现**

`crates/mullion-app/src/ui/transfer_panel.rs`：

```rust
//! F55:底部传输队列面板。折叠时一行摘要,展开是列表。
//!
//! **队列空时整个面板不画** —— 常驻一条空条只是在偷终端的行数。

use crate::files::queue::{Direction, JobState, Queue, Summary};
use crate::theme::Theme;

/// 面板上按下的东西。`app.rs` 拿去改队列。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferUiAction {
    Cancel(u64),
    CancelAll,
    ClearFinished,
}

/// 画面板。`collapsed` 由调用方持有(跨帧记住折叠状态)。
pub fn show(
    ctx: &egui::Context,
    t: &Theme,
    queue: &Queue,
    collapsed: &mut bool,
) -> Option<TransferUiAction> {
    if queue.jobs().is_empty() {
        return None;
    }
    let s = queue.summary();
    let mut action = None;
    egui::TopBottomPanel::bottom("transfer-queue")
        .frame(egui::Frame::none().fill(t.panel_bg).inner_margin(6.0))
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                let arrow = if *collapsed { "▸" } else { "▾" };
                if ui.button(format!("{arrow} 传输")).clicked() {
                    *collapsed = !*collapsed;
                }
                ui.label(summary_line(&s, queue.rate_bps()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("清除已完成").clicked() {
                        action = Some(TransferUiAction::ClearFinished);
                    }
                    if s.busy && ui.button("全部取消").clicked() {
                        action = Some(TransferUiAction::CancelAll);
                    }
                });
            });
            if *collapsed {
                return;
            }
            ui.separator();
            egui::ScrollArea::vertical()
                .max_height(160.0)
                .show(ui, |ui| {
                    for j in queue.jobs() {
                        ui.horizontal(|ui| {
                            ui.label(match j.dir {
                                Direction::Upload => "↑",
                                Direction::Download => "↓",
                            });
                            ui.label(&j.label);
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if !j.state.is_finished() && ui.button("取消").clicked() {
                                        action = Some(TransferUiAction::Cancel(j.id));
                                    }
                                    ui.label(state_text(j));
                                },
                            );
                        });
                    }
                });
        });
    action
}

fn summary_line(s: &Summary, bps: f64) -> String {
    if !s.busy {
        return "全部完成".into();
    }
    let rate = crate::files::human_size(bps as u64);
    format!("↑{} ↓{} · {rate}/s · {}", s.up, s.down, eta(s, bps))
}

/// `剩余 00:41`。速率还没估出来时不瞎猜 —— 一个跳来跳去的 ETA 比没有更糟。
fn eta(s: &Summary, bps: f64) -> String {
    if bps <= 1.0 || s.bytes_total <= s.bytes_done {
        return "剩余 --:--".into();
    }
    let secs = ((s.bytes_total - s.bytes_done) as f64 / bps) as u64;
    format!("剩余 {:02}:{:02}", secs / 60, secs % 60)
}

fn state_text(j: &crate::files::queue::Job) -> String {
    match &j.state {
        JobState::Pending => "排队中".into(),
        JobState::Running => {
            let pct = if j.total == 0 {
                0
            } else {
                (j.done * 100 / j.total).min(100)
            };
            format!("{pct}%")
        }
        JobState::Conflict => "等待处置".into(),
        JobState::Done => "完成".into(),
        JobState::Skipped => "已跳过".into(),
        JobState::Canceled => "已取消".into(),
        JobState::Failed(m) => m.clone(),
    }
}
```

`Queue` 加一个 `rate_bps()`：`RateMeter` 存在 `Queue` 里，由 `app.rs` 每帧 `queue.tick(now_secs)` 驱动。给 `Queue` 加：

```rust
    /// 每帧调一次,更新速率估计。`now_secs` 由调用方给(单调时钟的秒数),
    /// 队列自己不碰时钟 —— 碰了就没法纯单测。
    pub fn tick(&mut self, now_secs: f64) {
        let done = self.summary().bytes_done;
        self.rate.sample(now_secs, done);
    }

    pub fn rate_bps(&self) -> f64 {
        self.rate.bps()
    }
```

（`Queue` 加字段 `rate: RateMeter`，`new()` 里 `rate: RateMeter::default()`。）

`ui/mod.rs`：加 `pub mod transfer_panel;`，`UiState` 加 `pub transfer_collapsed: bool`（`new()` 里 `true`），`UiActions` 加 `pub transfer: Option<transfer_panel::TransferUiAction>`。`build_ui` 的签名要能拿到 `&Queue`——按现有 `build_ui` 的参数风格加一个 `queue: &crate::files::queue::Queue` 参数，在 `files_dialog::show` 之后调用：

```rust
    actions.transfer = transfer_panel::show(ctx, t, queue, &mut ui_state.transfer_collapsed);
```

- [ ] **Step 4：跑测试确认通过**

- [ ] **Step 5：变异验收（3 处）**

1. 去掉 `queue.jobs().is_empty()` 早退 → 空队列测试红。
2. `summary_line` 里删掉 `↓{}` → 摘要测试红。
3. `state_text` 的 `Failed` 分支改成固定字符串 → 失败原因测试红。

- [ ] **Step 6：提交**

```bash
git add crates/mullion-app/src/ui/transfer_panel.rs crates/mullion-app/src/ui/mod.rs \
        crates/mullion-app/src/files/queue.rs
git commit -m "feat(ui): 底部传输队列面板,折叠一行摘要/展开逐条 (F55/F59)"
```

---

## Task 8：接线 —— 菜单入口、调度器、worker、节流重绘

**Files:** Modify `crates/mullion-app/src/ui/files_panel.rs`, `crates/mullion-app/src/app.rs`

- [ ] **Step 1：菜单加两个入口**

`files_panel.rs` 的 `MenuItem` 加 `Transfer`，`menu_items_for` 里：远端栏在有 cursor 时加 `("下载到本地", MenuItem::Transfer)`；本地栏在有 cursor 时加 `("上传到远端", MenuItem::Transfer)`。`FileAction` 加 `Transfer`，`into_action` 映射过去。

对应测试（追加到 `files_panel.rs` 的 `mod tests`）：

```rust
    #[test]
    fn both_columns_offer_a_transfer_entry_but_only_the_remote_one_can_write() {
        let remote = menu_items_for(PanelColumn::Remote, true);
        let local = menu_items_for(PanelColumn::Local, true);
        assert!(
            remote.iter().any(|(l, _)| *l == "下载到本地"),
            "远端栏该有下载:{remote:?}"
        );
        assert!(
            local.iter().any(|(l, _)| *l == "上传到远端"),
            "本地栏该有上传:{local:?}"
        );
        // D5:本地栏永远不出现远端写操作。
        assert!(
            !local.iter().any(|(_, m)| matches!(m, MenuItem::Ask(_))),
            "本地栏冒出了写操作:{local:?}"
        );
    }
```

- [ ] **Step 2：`app.rs` —— 事件与状态**

`UserEvent` 加：

```rust
    /// F59:一条传输的进度。**高频** —— 接它的地方绝不能每条都
    /// `request_redraw`(T3),只更队列数据,重绘交给帧节流。
    TransferProgress { job: u64, done: u64 },
    /// F55:一条传输收工。`Err` 里的 `"目标已存在"` 会被队列翻译成
    /// `JobState::Conflict`(见 `queue::JobError`)。
    TransferDone { job: u64, result: Result<(), String> },
```

`App` 加字段：

```rust
    /// F55:跨标签的传输队列。放 App 上而不是标签上 —— 设计里它是全局的,
    /// 切标签不该看见另一份队列。
    transfer_queue: crate::files::queue::Queue,
    /// 每条 job 的取消旗标。worker 每块之后看一眼;取消要能在 2GB 传到
    /// 一半时立刻生效,不能等整个文件传完。
    transfer_cancels: std::collections::HashMap<u64, Arc<std::sync::atomic::AtomicBool>>,
    /// 传输起跑时的完整参数,重跑(冲突处置后)要用同一份。
    transfer_specs: std::collections::HashMap<u64, TransferSpec>,
```

```rust
/// 一条传输的全部输入。冲突处置后重跑读同一份 —— 重新算一遍的话
/// 「用户当时选的是哪个文件」和「现在光标在哪」会对不上。
#[derive(Clone)]
struct TransferSpec {
    dir: crate::files::queue::Direction,
    generation: u64,
    local: std::path::PathBuf,
    remote: mullion_ssh::sftp::RemotePath,
    total: u64,
}
```

`new()` 里 `transfer_queue: crate::files::queue::Queue::new(4)`（F56 的默认并发；可配 UI 是 D2-c 的欠账）。

- [ ] **Step 3：发起传输（`FileAction::Transfer` 的处理）**

远端栏 → 下载：对选中集里的每一项，目录先 `walk` 不做（**本切片远端目录递归留给 worker 内部**：worker 拿到 `is_dir` 时先 `list_dir` 再逐个建 job 太绕），采用更简单也更可解释的做法 —— **发起时就在 UI 线程外 spawn 一次「展开」任务**：

```rust
    /// F52:把选中项摊成一批 job。目录要走网络展开(远端)或走磁盘展开
    /// (本地),所以整件事在后台做,回来经 `UserEvent::TransferPlanned`
    /// 一次性入队 —— 边展开边入队的话,队列会在用户眼前长半天。
    fn start_transfer(&mut self, generation: u64, dir: Direction) { /* 见下 */ }
```

再加一个事件：

```rust
    /// F52:展开完成,一批 job 可以入队了。`Err` 是展开阶段就失败
    /// (例如目录读不了),这时一条 job 都不建。
    TransferPlanned { generation: u64, result: Result<Vec<PlannedJob>, String> },
```

```rust
/// 展开后的一条:已经算好两端完整路径和大小,入队即可跑。
#[derive(Clone)]
pub struct PlannedJob {
    dir: crate::files::queue::Direction,
    local: std::path::PathBuf,
    remote: mullion_ssh::sftp::RemotePath,
    total: u64,
    label: String,
}
```

`start_transfer` 的实现：

```rust
    fn start_transfer(&mut self, generation: u64, dir: crate::files::queue::Direction) {
        let Some(tab) = self.tabs.by_generation(generation) else {
            return;
        };
        let files = tab.content.files_panel();
        let (remote_cwd, local_cwd) = (files.remote.cwd.clone(), files.local.cwd.clone());
        // 源栏的选中项:下载看远端栏,上传看本地栏。
        let picked: Vec<(mullion_ssh::sftp::RemotePath, bool, u64)> =
            match dir {
                crate::files::queue::Direction::Download => &files.remote,
                crate::files::queue::Direction::Upload => &files.local,
            }
            .selected_or_cursor_entries()
            .into_iter()
            .map(|e| (e.name.clone(), e.kind == mullion_ssh::sftp::EntryKind::Dir, e.size))
            .collect();
        if picked.is_empty() {
            return;
        }
        let Some(client) = tab.content.sftp_client() else {
            self.ui.set_error("SFTP 还没就绪".into());
            self.ui_dirty = true;
            return;
        };
        let proxy = self.proxy.clone();
        let task = self._runtime.spawn(async move {
            let result =
                plan_transfer(&client, dir, &picked, &remote_cwd, &local_cwd).await;
            let _ = proxy.send_event(UserEvent::TransferPlanned { generation, result });
        });
        self.track_sftp_task(generation, task);
    }
```

（`selected_or_cursor_entries()` 给 `PaneState` 加：选中集非空就返回选中集对应的 `Entry`（按 `rows()` 序），否则返回 cursor 那一条；没有就空。这是「右键单条没选中也能操作」的既有语义，D2-a 里 `secondary_clicked` 已经把光标行选上了，但键盘触发时仍需要它。）

`plan_transfer` 自由函数（放在 `delete_all` 旁边）：

```rust
/// 把「用户点中的一批条目」摊成文件级 job。目录递归展开:远端走
/// `list_dir`,本地走 `walk_dir`。**不跟随符号链接** —— 两侧都跳过,
/// 理由与 `remove_tree` 一样(设计 D17)。
async fn plan_transfer(
    client: &Arc<mullion_ssh::sftp::SftpClient>,
    dir: crate::files::queue::Direction,
    picked: &[(mullion_ssh::sftp::RemotePath, bool, u64)],
    remote_cwd: &mullion_ssh::sftp::RemotePath,
    local_cwd: &mullion_ssh::sftp::RemotePath,
) -> Result<Vec<PlannedJob>, String> {
    let mut out = Vec::new();
    for (name, is_dir, size) in picked {
        match dir {
            crate::files::queue::Direction::Download => {
                let remote = remote_cwd.join(name.as_bytes());
                if *is_dir {
                    plan_download_dir(client, &remote, &[], &mut out, local_cwd).await?;
                } else {
                    out.push(download_job(&remote, &[], local_cwd, *size)?);
                }
            }
            crate::files::queue::Direction::Upload => {
                let local = crate::files::local::to_path(&local_cwd.join(name.as_bytes()));
                if *is_dir {
                    for w in crate::files::local::walk_dir(&local)? {
                        out.push(upload_job(&local, &w.rel, remote_cwd, name, w.size)?);
                    }
                } else {
                    out.push(upload_job(&local, &[], remote_cwd, name, *size)?);
                }
            }
        }
    }
    Ok(out)
}
```

`download_job` / `upload_job` / `plan_download_dir` 的完整实现见 Task 8 Step 4。

- [ ] **Step 4：job 构造与远端递归**

```rust
/// 一条下载 job:远端绝对路径 + 相对段 → 本地落点。
/// D16:落点名在 Windows 上非法就**整条拒掉并给建议名**,不静默改写。
fn download_job(
    remote_root: &mullion_ssh::sftp::RemotePath,
    rel: &[Vec<u8>],
    local_dir: &mullion_ssh::sftp::RemotePath,
    size: u64,
) -> Result<PlannedJob, String> {
    let mut remote = remote_root.clone();
    let mut local = crate::files::local::to_path(local_dir);
    let root_name = last_segment(remote_root);
    check_windows_name(&root_name)?;
    local.push(&root_name);
    for seg in rel {
        remote = remote.join(seg);
        let s = String::from_utf8_lossy(seg).into_owned();
        check_windows_name(&s)?;
        local.push(&s);
    }
    let label = local
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(PlannedJob {
        dir: crate::files::queue::Direction::Download,
        local,
        remote,
        total: size,
        label,
    })
}

fn check_windows_name(name: &str) -> Result<(), String> {
    match crate::files::transfer::illegal_on_windows(name) {
        None => Ok(()),
        Some(sug) => Err(format!(
            "「{name}」在 Windows 上不是合法文件名。改成「{sug}」再传。"
        )),
    }
}

fn last_segment(p: &mullion_ssh::sftp::RemotePath) -> String {
    let b = p.as_bytes();
    let seg = b.rsplit(|c| *c == b'/').next().unwrap_or(b);
    String::from_utf8_lossy(seg).into_owned()
}

/// 一条上传 job。
fn upload_job(
    local_root: &std::path::Path,
    rel: &[String],
    remote_dir: &mullion_ssh::sftp::RemotePath,
    root_name: &mullion_ssh::sftp::RemotePath,
    size: u64,
) -> Result<PlannedJob, String> {
    let mut local = local_root.to_path_buf();
    let mut remote = remote_dir.join(root_name.as_bytes());
    for seg in rel {
        local.push(seg);
        remote = remote.join(seg.as_bytes());
    }
    let label = rel
        .last()
        .cloned()
        .unwrap_or_else(|| root_name.display().into_owned());
    Ok(PlannedJob {
        dir: crate::files::queue::Direction::Upload,
        local,
        remote,
        total: size,
        label,
    })
}

/// 远端目录递归。**不跟随符号链接**(D17)。
async fn plan_download_dir(
    client: &Arc<mullion_ssh::sftp::SftpClient>,
    root: &mullion_ssh::sftp::RemotePath,
    rel: &[Vec<u8>],
    out: &mut Vec<PlannedJob>,
    local_dir: &mullion_ssh::sftp::RemotePath,
) -> Result<(), String> {
    // 手写栈,不用递归 async fn —— `async fn` 递归要 Box,还得处理生命周期。
    let mut stack = vec![rel.to_vec()];
    while let Some(cur) = stack.pop() {
        let mut dir = root.clone();
        for seg in &cur {
            dir = dir.join(seg);
        }
        let entries = client.list_dir(&dir).await.map_err(|e| e.to_string())?;
        for e in entries {
            if e.kind == mullion_ssh::sftp::EntryKind::Symlink
                || e.kind == mullion_ssh::sftp::EntryKind::Other
            {
                continue;
            }
            let mut next = cur.clone();
            next.push(e.name.as_bytes().to_vec());
            if e.kind == mullion_ssh::sftp::EntryKind::Dir {
                stack.push(next);
            } else {
                out.push(download_job(root, &next, local_dir, e.size)?);
            }
        }
    }
    Ok(())
}
```

- [ ] **Step 5：调度 + worker**

```rust
    /// 每帧调:队列放行几条就起几条 worker。
    ///
    /// **每条 job 一条独立的 sftp channel**(F56)——共用一条的话请求
    /// 在同一个 session 上串行,并发度等于 1,设计 D8 说的吞吐问题原样还在。
    fn pump_transfers(&mut self) {
        for id in self.transfer_queue.take_runnable() {
            let Some(spec) = self.transfer_specs.get(&id).cloned() else {
                self.transfer_queue.finish(id, Err("任务参数丢了".into()));
                continue;
            };
            let Some(tab) = self.tabs.by_generation(spec.generation) else {
                self.transfer_queue.cancel(id);
                continue;
            };
            let Some(conn) = tab.content.sftp_connection() else {
                self.transfer_queue.finish(id, Err("连接已断开".into()));
                continue;
            };
            let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
            self.transfer_cancels.insert(id, cancel.clone());
            let resolved = self.transfer_queue.get(id).and_then(|j| j.resolved);
            let proxy = self.proxy.clone();
            let generation = spec.generation;
            let task = self._runtime.spawn(async move {
                let result = run_transfer(conn, spec, resolved, id, &proxy, &cancel).await;
                let _ = proxy.send_event(UserEvent::TransferDone { job: id, result });
            });
            self.track_sftp_task(generation, task);
        }
    }
```

```rust
/// 跑一条传输。**自己开一条 sftp channel**(见 `pump_transfers` 的注释)。
async fn run_transfer(
    conn: Arc<SshConnection>,
    spec: TransferSpec,
    resolved: Option<crate::files::queue::Conflict>,
    job: u64,
    proxy: &EventLoopProxy<UserEvent>,
    cancel: &Arc<std::sync::atomic::AtomicBool>,
) -> Result<(), String> {
    use crate::files::queue::{Conflict, Direction, JobError};
    use crate::files::transfer::{dedup_name, staging_name};

    let client = mullion_ssh::sftp::SftpClient::open(conn)
        .await
        .map_err(|e| e.to_string())?;

    // 目标端最终落点。冲突处置为「重命名」时先换个名字。
    let (mut dst_local, mut dst_remote) = (spec.local.clone(), spec.remote.clone());
    let exists = match spec.dir {
        Direction::Download => dst_local.exists(),
        Direction::Upload => client.exists(&dst_remote).await.map_err(|e| e.to_string())?,
    };
    if exists {
        match resolved {
            // 没处置过 → 交回队列去问用户。
            None => return Err(JobError::Conflict.into()),
            Some(Conflict::Skip) => return Ok(()),
            Some(Conflict::Overwrite) => {}
            Some(Conflict::Rename) => match spec.dir {
                Direction::Download => {
                    let parent = dst_local.parent().unwrap_or(std::path::Path::new(".")).to_path_buf();
                    let base = dst_local.file_name().unwrap_or_default().to_string_lossy().into_owned();
                    let name = dedup_name(&base, |c| parent.join(c).exists());
                    dst_local = parent.join(name);
                }
                Direction::Upload => {
                    let parent = dst_remote.parent();
                    let base = last_segment(&dst_remote);
                    // 远端查重每次一个 RTT,但改名是低频操作,可接受。
                    let mut name = base.clone();
                    for i in 1..1000 {
                        let cand = dedup_name(&base, |c| c == name && i > 1);
                        let p = parent.join(cand.as_bytes());
                        if !client.exists(&p).await.map_err(|e| e.to_string())? {
                            name = cand;
                            break;
                        }
                    }
                    dst_remote = parent.join(name.as_bytes());
                }
            },
        }
    }

    // D19:新建走 `.part` 再 rename;覆盖直接写(保 inode / 权限 / 硬链接)。
    let overwriting = exists && resolved == Some(Conflict::Overwrite);
    let mut done: u64 = 0;
    let mut buf = vec![0u8; 64 * 1024];
    match spec.dir {
        Direction::Download => {
            let final_name = dst_local.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let staging = dst_local.with_file_name(staging_name(&final_name, overwriting));
            if let Some(p) = staging.parent() {
                std::fs::create_dir_all(p).map_err(|e| format!("建不了本地目录:{e}"))?;
            }
            let mut src = client.open_read(&spec.remote).await.map_err(|e| e.to_string())?;
            {
                use std::io::Write;
                let mut f = std::fs::File::create(&staging).map_err(|e| format!("写不了本地文件:{e}"))?;
                loop {
                    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                        let _ = std::fs::remove_file(&staging);
                        return Err("已取消".into());
                    }
                    let n = src.read_chunk(&mut buf).await.map_err(|e| e.to_string())?;
                    if n == 0 {
                        break;
                    }
                    f.write_all(&buf[..n]).map_err(|e| format!("写不了本地文件:{e}"))?;
                    done += n as u64;
                    let _ = proxy.send_event(UserEvent::TransferProgress { job, done });
                }
                f.flush().map_err(|e| format!("写不了本地文件:{e}"))?;
            }
            if staging != dst_local {
                std::fs::rename(&staging, &dst_local).map_err(|e| format!("改名失败:{e}"))?;
            }
        }
        Direction::Upload => {
            use std::io::Read;
            let final_name = last_segment(&dst_remote);
            let staging = dst_remote
                .parent()
                .join(staging_name(&final_name, overwriting).as_bytes());
            let mut f = std::fs::File::open(&spec.local).map_err(|e| format!("读不了本地文件:{e}"))?;
            let mut dst = client.open_write(&staging, true).await.map_err(|e| e.to_string())?;
            loop {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    let _ = dst.finish().await;
                    let _ = client.remove_file(&staging).await;
                    return Err("已取消".into());
                }
                let n = f.read(&mut buf).map_err(|e| format!("读不了本地文件:{e}"))?;
                if n == 0 {
                    break;
                }
                dst.write_chunk(&buf[..n]).await.map_err(|e| e.to_string())?;
                done += n as u64;
                let _ = proxy.send_event(UserEvent::TransferProgress { job, done });
            }
            dst.finish().await.map_err(|e| e.to_string())?;
            if staging != dst_remote {
                client
                    .rename(&staging, &dst_remote)
                    .await
                    .map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}
```

> 上传的父目录：递归上传时远端子目录可能不存在。`run_transfer` 在 `open_write` 失败时**不**自作主张 mkdir —— 改为在 `plan_transfer` 阶段就把需要的目录建出来（`plan_transfer` 里对每个 `PlannedJob` 的远端父目录去重后 `create_dir`，已存在的错误吞掉）。这一步放在 `TransferPlanned` 的 `Ok` 分支入队之前，见 Step 6。

- [ ] **Step 6：事件接线**

`match event` 里加：

```rust
            UserEvent::TransferPlanned { generation, result } => {
                match result {
                    Ok(jobs) => {
                        for p in jobs {
                            let id = self.transfer_queue.push(crate::files::queue::NewJob {
                                dir: p.dir,
                                generation,
                                label: p.label,
                                total: p.total,
                            });
                            self.transfer_specs.insert(
                                id,
                                TransferSpec {
                                    dir: p.dir,
                                    generation,
                                    local: p.local,
                                    remote: p.remote,
                                    total: p.total,
                                },
                            );
                        }
                    }
                    Err(e) => self.ui.set_error(e),
                }
                self.ui_dirty = true;
            }
            UserEvent::TransferProgress { job, done } => {
                // T3:高频事件。只更数据,**不** request_redraw ——
                // 一个 100MB 的文件会发几千条,每条都重绘就是风扇起飞。
                self.transfer_queue.progress(job, done);
            }
            UserEvent::TransferDone { job, result } => {
                self.transfer_cancels.remove(&job);
                self.transfer_queue.finish(job, result);
                // 完成后刷新目标栏 —— 不刷的话新文件不出现,用户以为没成。
                if let Some(spec) = self.transfer_specs.get(&job) {
                    let (gen, dir) = (spec.generation, spec.dir);
                    let column = match dir {
                        crate::files::queue::Direction::Download => PanelColumn::Local,
                        crate::files::queue::Direction::Upload => PanelColumn::Remote,
                    };
                    self.dispatch_panel_action_for(gen, column, FileAction::Refresh);
                }
                self.ui_dirty = true;
            }
```

`about_to_wait` / 帧节流那一段（`ui_dirty` 的驱动处）加：

```rust
        // F59:队列在跑时按帧节流刷新进度显示(T3:不靠事件驱动重绘)。
        if self.transfer_queue.summary().busy {
            self.transfer_queue
                .tick(self.start_time.elapsed().as_secs_f64());
            self.ui_dirty = true;
        }
        self.pump_transfers();
```

present 分支加：

```rust
        if let Some(a) = actions.transfer {
            match a {
                TransferUiAction::Cancel(id) => {
                    if let Some(c) = self.transfer_cancels.get(&id) {
                        c.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    self.transfer_queue.cancel(id);
                }
                TransferUiAction::CancelAll => {
                    for c in self.transfer_cancels.values() {
                        c.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    self.transfer_queue.cancel_all();
                }
                TransferUiAction::ClearFinished => self.transfer_queue.clear_finished(),
            }
            self.ui_dirty = true;
        }
```

`has_real_action` 加 `|| a.transfer.is_some()`。
`FileOp::Resolve` 在 `apply_file_op` 里：`self.transfer_queue.resolve_conflict(job, choice, apply_all);`。
每帧在 `build_ui` 之前：若 `self.ui.files_dialog.is_none()`，且 `self.transfer_queue.first_conflict()` 有值，就把冲突对话框打开。
标签关闭（`wind_down` 调用处）加 `self.transfer_queue.cancel_generation(generation);`。

- [ ] **Step 7：写守护测试（`app.rs` 的 `mod tests`）**

```rust
    #[test]
    fn transfer_progress_events_never_request_a_redraw_so_the_fan_stays_quiet() {
        // T3:进度是高频事件,每条都重绘就是每秒几千次。判据是源码里
        // 这个 arm 的块体内**没有** ui_dirty / request_redraw。
        let src = include_str!("app.rs");
        let at = src
            .find("UserEvent::TransferProgress")
            .expect("找不到 TransferProgress 的 arm");
        let rest = &src[at + src[at..].find("=> {").expect("arm 没有块体")..];
        let arm = brace_balanced_arm(rest);
        assert!(
            arm.len() < rest.len(),
            "没截到 arm 的闭合大括号,这条断言会恒绿"
        );
        assert!(
            !arm.contains("ui_dirty") && !arm.contains("request_redraw"),
            "进度事件里出现了重绘(T3):{arm}"
        );
    }

    #[test]
    fn a_finished_transfer_refreshes_the_destination_column_so_the_file_shows_up() {
        let src = include_str!("app.rs");
        let at = src
            .find("UserEvent::TransferDone")
            .expect("找不到 TransferDone 的 arm");
        let rest = &src[at + src[at..].find("=> {").expect("arm 没有块体")..];
        let arm = brace_balanced_arm(rest);
        assert!(arm.len() < rest.len(), "没截到闭合大括号,断言会恒绿");
        assert!(
            arm.contains("FileAction::Refresh"),
            "传完不刷新,新文件不会出现:{arm}"
        );
    }

    #[test]
    fn a_transfer_ui_action_counts_as_a_real_action_so_it_is_not_swallowed() {
        let mut a = UiActions::default();
        a.transfer = Some(crate::ui::transfer_panel::TransferUiAction::CancelAll);
        assert!(has_real_action(&a), "取消传输被 egui 的丢弃帧吞了");
    }
```

`brace_balanced_arm` 是既有测试里那段切法，抽成 helper 复用（D2-a 的 Task 8 已经踩过：起点必须是 `=> {`，不能是 arm 的 pattern，否则 pattern 自带的花括号会把深度立刻清零）。

- [ ] **Step 8：跑绿 + 变异验收（至少 4 处）**

1. 在 `TransferProgress` 的 arm 里加一句 `self.ui_dirty = true;` → T3 测试红。
2. 删掉 `TransferDone` 里的 `dispatch_panel_action_for(..., FileAction::Refresh)` → 刷新测试红。
3. `has_real_action` 去掉 `a.transfer` → 吞动作测试红。
4. `pump_transfers` 里改成共用 `tab.content.sftp_client()` 而不是新开 channel → 需要一条源码守护（补：断言 `pump_transfers` 的块体里出现 `SftpClient::open`）。

- [ ] **Step 9：提交**

```bash
git add crates/mullion-app/src/app.rs crates/mullion-app/src/ui/files_panel.rs
git commit -m "feat(app): 传输接线——菜单入口、逐 job 独立 channel、进度不驱动重绘 (F52/F55/F56/F59)"
```

---

## Task 9：绿门 + 交付

- [ ] **Step 1：全量绿门**

```bash
cargo fmt --check
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings
```

- [ ] **Step 2：重跑 T1–T8 守护测试**（改了 `app.rs` 事件循环，必跑）

```bash
cargo test -p mullion-term emulator::tests::pty_write_is_collected
cargo test -p mullion-app render::tests::sync_update_defers_present
cargo test -p mullion-app app::tests::redraw_is_frame_capped
cargo test -p mullion-app app::tests::reflow_emits_resize
cargo test -p mullion-term keymap::tests::shift_blocks_mouse_report_so_user_can_copy
cargo test -p mullion-term keymap::tests::shift_enter_without_kitty_is_esc_cr
cargo test -p mullion-app frame::tests
cargo test -p mullion-app input_route::tests
```

- [ ] **Step 3：版本号 + 发版**（按 `CLAUDE.md` 的交付约定，一条龙）

```bash
# workspace.package.version 第三位 +1 → 0.1.34
git commit -m "chore: 版本 0.1.34(SFTP 写操作与双向传输队列)"
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app
# objdump 依赖验收(出现 libgcc_s_seh-1.dll / libwinpthread-1.dll 即不合格)
```

- [ ] **Step 4：GitHub Release**（标题只能是 `v0.1.34`，notes 里写 D1+D2 的人工验收清单 + sha256 + `Unblock-File` 提示）
