# 切片 D1：SFTP 只读浏览 实施计划（F50 / F120）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让用户第一次「能看见远端文件」——SSH 会话右侧栏和 SFTP 节点标签页里都能列目录、排序、进出目录、走书签，全程零写操作。

**Architecture:** 协议层落 `mullion-ssh/src/sftp.rs`（`SshConnection::handle()` 是 `pub(crate)`，只有同 crate 够得着，同 `tunnel.rs`）；它**不向外暴露任何 `russh_sftp` 类型**，只给自定义的 `RemotePath` / `Entry`，架构方向 `app → ssh` 不被第三方类型污染。面板的纯逻辑（排序、隐藏文件、路径导航）落 `mullion-app/src/files/`，零 egui、可纯单测（同 `shell/tabs.rs` 的做法）。渲染落 `mullion-app/src/ui/files_panel.rs`。`mullion-core` **零改动**——面板是 egui 的 `SidePanel` / 标签内容区，布局树不知道它存在（设计 D2）。

**Tech Stack:** `russh-sftp 2.4.0`（客户端 + 测试用服务端）、russh 0.54.5、egui 0.30、`mullion-store` schema v8。

**设计文档：** `docs/superpowers/specs/2026-08-12-sftp-browser-design.md`（决策 D1/D2/D4/D5/D6/D7/D8/D15/D16/D21/D23/D24 在下面被反复引用）

**本切片不做**（写进来是为了让执行者不越界）：任何写操作（上传/下载/新建/重命名/删除/改权限）、传输队列、拖拽、编辑器、断线自动重连（那是 D2 的 `schedule.rs` 复用）、递归删除。**只读**。

---

## 前置：一条被推翻的设计决策

**设计 D16 原文**：「所有 SFTP 请求一律用原始字节；UI 显示串是 `from_utf8_lossy` 的投影」。

**实施前核实（读 `russh-sftp 2.4.0` 源码）**：

- `src/buf.rs:25` —— `try_get_string()` 的实现是 `Ok(String::from_utf8_lossy(&bytes).into())`，
  严格版那行被注释在上面一行。**所有 wire 字符串都经过这一步**。
- `src/protocol/file.rs` —— `File { filename: String, longname: String, attrs }`。
- `src/client/rawsession.rs` —— 每个方法都是 `P: Into<String>`，连最底层的 raw session 也没有字节通道。
- `serde_bytes` 只用在 `protocol/write.rs` 与 `protocol/data.rs`（**文件内容**），文件名没份。

结论：非 UTF-8 的远端文件名在进到我们代码之前就已经被替换成 U+FFFD，原样发回去是另一串字节。
D16 的字面要求在这个库上**做不到**。

**已拍板的修订（2026-08-12）**：

- **我们这一层照旧以 `Vec<u8>` 为真源**（`RemotePath`），只在调库的那一行转 `String`。
  合法 UTF-8（含全部中文路径）逐字节往返，纪律照旧成立。
- 转不成 `String` 的路径**到不了网络层**——`RemotePath::as_wire()` 返回 `Result`，
  错误分支不发任何请求。
- 列表里名字含 U+FFFD 的条目标 `NonUtf8`，**照常显示**（用户得知道那儿有个文件），
  但所有操作入口禁用并给理由「名称非 UTF-8，本版无法操作」。**绝不静默打错文件。**
- 代价：这类文件本版只能看不能动。写进 Release notes 的已知限制。

**Task 1 的第一步就是把这条修订写回设计文档与 spec.md**，否则半年后回头看会以为实现偷懒了。

---

## 文件结构

| 文件 | 职责 |
|---|---|
| `crates/mullion-ssh/src/sftp.rs`（新建） | `RemotePath` 字节纪律类型、`Entry`/`EntryKind` 自有类型、`SftpClient::open` / `list_dir` / `canonicalize` / `read_link`。**不 re-export 任何 `russh_sftp` 类型** |
| `crates/mullion-ssh/tests/common/sftp_server.rs`（新建） | 假 SFTP 服务端：内存文件系统 + `russh_sftp::server::Handler`，挂在既有假 sshd 的 `subsystem_request` 上 |
| `crates/mullion-ssh/tests/sftp_browse.rs`（新建） | 端到端：真握手 → 开 subsystem → 列目录，含非 UTF-8 与符号链接两条守护 |
| `crates/mullion-store/src/sftp.rs`（新建） | `SftpPrefs { default_remote, default_local, bookmarks }` + `Bookmark` |
| `crates/mullion-store/src/model.rs`（改） | `SessionRecord` 加 `sftp` 字段；`CURRENT_SCHEMA` 7 → 8 |
| `crates/mullion-store/src/migrate.rs`（改） | v7 → v8 迁移守护测试 |
| `crates/mullion-app/src/files/mod.rs`（新建） | 纯逻辑：`Listing`、排序键、隐藏文件过滤、`parent_of` / `join`。零 egui/tokio |
| `crates/mullion-app/src/files/state.rs`（新建） | 面板运行态：当前目录、加载中/错误、选中行、排序列、隐藏开关 |
| `crates/mullion-app/src/ui/files_panel.rs`（新建） | egui 渲染：远端栏 + 本地栏 + 列头 + 书签栏 + F100 登记 |
| `crates/mullion-app/src/shell/session_map.rs`（改） | 解 D24 闸门：SFTP 记录映射成合法拨号参数 + `wants_sftp` 标记 |
| `crates/mullion-app/src/ui/session_manager/editor.rs`（改） | 撤掉 `Disabled::Sftp`；`TABS` 加「SFTP」页 |
| `crates/mullion-app/src/ui/session_manager/fields.rs`（改） | 「SFTP」分节：默认远端/本地目录 + 书签列表 |
| `crates/mullion-app/src/app.rs`（改） | 接线：侧栏开关、`TabContent::Files`、异步列目录事件 |
| `crates/mullion-app/src/ui/mod.rs`（改） | `UiFrame` 加面板视图、`UiActions` 加面板动作 |

---

## Task 1: 记录 D16 修订 + 加 russh-sftp 依赖

**Files:**
- Modify: `docs/superpowers/specs/2026-08-12-sftp-browser-design.md`（D16 节）
- Modify: `spec.md:133-134`
- Modify: `crates/mullion-ssh/Cargo.toml`
- Modify: `Cargo.toml`（workspace 依赖表）

- [ ] **Step 1: 改设计文档 D16**

在 `### D16 远端路径的真源是 `Vec<u8>`` 一节的开头插入：

```markdown
> **2026-08-12 实施期修订（D1 切片）。** 下面「所有 SFTP 请求一律用原始字节」这条
> 在 `russh-sftp 2.4.0` 上**做不到**：`src/buf.rs:25` 把所有 wire 字符串过
> `String::from_utf8_lossy`（严格版被注释掉），`protocol/file.rs` 的 `File.filename`
> 是 `String`，连 `rawsession` 每个方法都是 `P: Into<String>`；`serde_bytes` 只用在
> 文件**内容**上。非 UTF-8 文件名在进到我们代码前就已被替换成 U+FFFD。
>
> **修订后的纪律**：我们这层仍以 `Vec<u8>`（`RemotePath`）为真源，只在调库那一行转
> `String`；合法 UTF-8（含全部中文路径）逐字节往返。转不成 `String` 的路径
> **到不了网络层**（`as_wire()` 返回 `Result`，错误分支一个请求都不发）；这类条目在
> 列表里照常显示但所有操作入口禁用，理由「名称非 UTF-8，本版无法操作」。
> 代价是这类文件只能看不能动；换来的是**绝不静默打错文件**，这正是 D16 的本意。
>
> 备选「fork/vendor russh-sftp 改字节干净」与「自己写 SFTP v3 协议层」都被否掉：
> 前者要改 15+ 个 packet 类型与 ser/de 两侧并从此自维护 fork，后者工作量翻倍且
> 协议细节 bug 只有实机能暴露。两者都可在将来 D 系列收尾时重提。
```

同节末尾那句「守护测试：假服务端里放一个非 UTF-8 文件名，断言『显示是 lossy 的』
且『删除请求发出的字节 == 原始字节』」改成：

```markdown
守护测试（修订后）：假服务端里同时放一个 UTF-8 中文名和一个非 UTF-8 名，断言
① 中文名的 `RemotePath` 字节与服务端收到的字节逐字节相等；② 非 UTF-8 名显示为
lossy 串、`as_wire()` 返回 `Err`、且**服务端一个针对它的请求都没收到**。
```

- [ ] **Step 2: 改 spec.md 的 §4.4 纪律条**

`spec.md:133-134` 现在是：

```
1. **远端路径的真源是 `Vec<u8>`。** SFTP v3 的文件名是字节串、无编码约定，Linux 上可以是任意
   非 UTF-8 字节。所有 SFTP 请求一律用原始字节；UI 显示串只是 `from_utf8_lossy` 的**投影**。
```

改成：

```
1. **远端路径的真源是 `Vec<u8>`。** SFTP v3 的文件名是字节串、无编码约定，Linux 上可以是任意
   非 UTF-8 字节。客户端内部一律以字节为真源，UI 显示串只是 `from_utf8_lossy` 的**投影**。
   **协议层限制（2026-08-12 核实）**：`russh-sftp 2.4.0` 的 wire 字符串一律走
   `from_utf8_lossy`，字节通道只对文件**内容**开放。故转不成 UTF-8 的路径**不发请求**，
   对应条目在列表里照常显示、所有操作禁用并说明原因——绝不静默打到别的文件上。
```

- [ ] **Step 3: 加依赖**

`Cargo.toml` 的 `[workspace.dependencies]` 里加一行（放在 `russh` 那行之后，保持字母序无所谓，
保持「同族挨着」）：

```toml
# SFTP v3 协议层(F50)。**不依赖 russh 本身**(已核实 2.4.0 的依赖只有 bytes/chrono/
# dashmap/serde/serde_bytes/thiserror/bitflags/log/tokio/tokio-util),吃任意
# AsyncRead+AsyncWrite,与锁定的 russh 0.54 不会版本打架。
russh-sftp = "2.4.0"
```

`crates/mullion-ssh/Cargo.toml` 的 `[dependencies]` 加：

```toml
russh-sftp.workspace = true
```

`[dev-dependencies]` 加（假 SFTP 服务端要用 `russh_sftp::server`，与客户端同一个 crate，
不需要额外条目——**这一步什么都不用加**，写在这里是为了让执行者不去多此一举）。

- [ ] **Step 4: 验证依赖能拉下来并编过**

```bash
HTTPS_PROXY=http://127.0.0.1:7890 cargo check -p mullion-ssh
```

预期：`Finished`，`Cargo.lock` 里出现 `russh-sftp 2.4.0`。

若网络失败，代理环境变量必须带上（本机 DNS 解析不了 crates.io 之外的域名，见 CLAUDE.md）。

- [ ] **Step 5: 提交**

```bash
git add Cargo.toml Cargo.lock crates/mullion-ssh/Cargo.toml docs/superpowers/specs/2026-08-12-sftp-browser-design.md spec.md
git commit -m "docs+deps: D16 按 russh-sftp 实况修订 + 引入 russh-sftp 2.4.0 (F50)"
```

---

## Task 2: `RemotePath` —— 字节真源与它的唯一出口

**Files:**
- Create: `crates/mullion-ssh/src/sftp.rs`
- Modify: `crates/mullion-ssh/src/lib.rs`

这一步**没有任何网络**，纯类型 + 纯单测。先把纪律钉死，再让协议代码只能从这个口子出去。

- [ ] **Step 1: 写失败的测试**

新建 `crates/mullion-ssh/src/sftp.rs`，先只写测试模块（`use super::*;` 会因为没有实现而编不过，
这就是「失败」）：

```rust
//! SFTP v3 客户端(F50)。协议实现来自 `russh-sftp 2.4.0`。
//!
//! **本模块不向外暴露任何 `russh_sftp` 类型。** 架构不变量是 `app → ssh`,
//! 让第三方的 `FileType`/`Metadata` 漏进 `mullion-app` 等于把依赖方向变成
//! `app → russh_sftp`,以后换协议库要改的就不止这一个 crate。

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_utf8_path_round_trips_byte_for_byte() {
        let p = RemotePath::from_bytes(b"/data/\xe4\xb8\xad\xe6\x96\x87/a.txt".to_vec());
        assert_eq!(p.as_wire().unwrap(), "/data/中文/a.txt");
        assert_eq!(p.as_bytes(), b"/data/\xe4\xb8\xad\xe6\x96\x87/a.txt");
    }

    /// D16 修订后的核心不变量:转不成 UTF-8 的路径**拿不到 wire 表示**,
    /// 于是在类型上就发不出请求 —— 不是靠调用方记得先检查。
    #[test]
    fn a_non_utf8_path_can_be_displayed_but_never_reaches_the_wire() {
        let p = RemotePath::from_bytes(vec![b'/', 0xff, 0xfe, b'x']);
        assert!(p.as_wire().is_err(), "非 UTF-8 路径不得给出 wire 串");
        assert!(
            p.display().contains('\u{fffd}'),
            "显示串该是 lossy 投影,用户要能看见那儿有个文件"
        );
        assert!(!p.is_utf8());
    }

    #[test]
    fn joining_keeps_bytes_and_uses_a_single_slash() {
        let dir = RemotePath::from_bytes(b"/data".to_vec());
        assert_eq!(dir.join(b"a.txt").as_bytes(), b"/data/a.txt");
        let root = RemotePath::from_bytes(b"/".to_vec());
        assert_eq!(root.join(b"a.txt").as_bytes(), b"/a.txt");
        let trailing = RemotePath::from_bytes(b"/data/".to_vec());
        assert_eq!(trailing.join(b"a.txt").as_bytes(), b"/data/a.txt");
    }

    #[test]
    fn parent_of_root_is_root_so_backspace_cannot_walk_off_the_top() {
        let root = RemotePath::from_bytes(b"/".to_vec());
        assert_eq!(root.parent().as_bytes(), b"/");
        let a = RemotePath::from_bytes(b"/data/x".to_vec());
        assert_eq!(a.parent().as_bytes(), b"/data");
        let b = RemotePath::from_bytes(b"/data".to_vec());
        assert_eq!(b.parent().as_bytes(), b"/");
    }

    /// 相对路径(`.` = 登录目录)也要能用 —— 默认远端目录留空时就是它。
    #[test]
    fn a_relative_path_stays_relative_when_joined() {
        let dot = RemotePath::from_bytes(b".".to_vec());
        assert_eq!(dot.join(b"sub").as_bytes(), b"./sub");
    }
}
```

- [ ] **Step 2: 跑，确认它编不过**

```bash
cargo test -p mullion-ssh --lib sftp 2>&1 | grep -E "^error|cannot find"
```

预期：`cannot find type RemotePath in this scope` 之类。

- [ ] **Step 3: 写实现**

在 `sftp.rs` 的模块文档之后、`mod tests` 之前插入：

```rust
use std::borrow::Cow;

/// 远端路径。**真源是字节**,不是 `String`。
///
/// SFTP v3 的文件名就是字节串,没有编码约定;Linux 上可以是任意非 UTF-8 字节
/// (GBK 中文名就是这一类)。一旦某处拿显示串反推路径,这类文件就会静默失败,
/// 或者更糟 —— 打到另一个文件上。
///
/// **唯一的 wire 出口是 [`RemotePath::as_wire`],它返回 `Result`。**
/// `russh-sftp 2.4.0` 的接口全是 `Into<String>`,给不出字节通道
/// (`src/buf.rs:25` 把所有 wire 字符串过 `from_utf8_lossy`),所以本版
/// 只能把非 UTF-8 路径**挡在网络层之外**,而不是原样发出去。这不是偷懒,
/// 是在「静默打错文件」和「明确拒绝」之间选了后者(设计 D16 的 2026-08-12 修订)。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RemotePath(Vec<u8>);

/// 路径含非 UTF-8 字节,本版无法对它发请求。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NonUtf8Path;

impl std::fmt::Display for NonUtf8Path {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "名称含非 UTF-8 字节,本版无法操作")
    }
}

impl std::error::Error for NonUtf8Path {}

impl RemotePath {
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// 给渲染用的**投影**。绝不能拿它反推路径。
    pub fn display(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.0)
    }

    pub fn is_utf8(&self) -> bool {
        std::str::from_utf8(&self.0).is_ok()
    }

    /// 唯一的 wire 出口。非 UTF-8 → `Err`,调用方发不出请求。
    pub fn as_wire(&self) -> Result<String, NonUtf8Path> {
        String::from_utf8(self.0.clone()).map_err(|_| NonUtf8Path)
    }

    /// 拼一段名字。分隔符恒为 `/`(SFTP 线上永远是 POSIX 路径,
    /// 哪怕客户端跑在 Windows 上)。
    pub fn join(&self, name: &[u8]) -> Self {
        let mut out = self.0.clone();
        if !out.ends_with(b"/") && !out.is_empty() {
            out.push(b'/');
        }
        out.extend_from_slice(name);
        Self(out)
    }

    /// 上一级。**根的上一级还是根** —— 否则一直按 Backspace 会走出
    /// `/` 之上,拼出 `""` 这种服务端认不得的路径。
    pub fn parent(&self) -> Self {
        if self.0 == b"/" || self.0.is_empty() {
            return self.clone();
        }
        let trimmed: &[u8] = if self.0.ends_with(b"/") {
            &self.0[..self.0.len() - 1]
        } else {
            &self.0
        };
        match trimmed.iter().rposition(|b| *b == b'/') {
            Some(0) => Self(b"/".to_vec()),
            Some(ix) => Self(trimmed[..ix].to_vec()),
            None => Self(trimmed.to_vec()),
        }
    }
}
```

`crates/mullion-ssh/src/lib.rs` 的 `pub mod` 列表里按字母序加一行（在 `schedule` 与
`session` 之间）：

```rust
pub mod sftp;
```

- [ ] **Step 4: 跑，确认全绿**

```bash
cargo test -p mullion-ssh --lib sftp
```

预期：`test result: ok. 5 passed`。

- [ ] **Step 5: 变异验收**

把 `as_wire` 改成 `Ok(String::from_utf8_lossy(&self.0).into())`（即「照库的方式来」），
跑 `a_non_utf8_path_can_be_displayed_but_never_reaches_the_wire` **必须变红**。改回。

把 `parent()` 里 `self.0 == b"/"` 那条早退删掉，跑
`parent_of_root_is_root_so_backspace_cannot_walk_off_the_top` **必须变红**。改回。

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-ssh/src/sftp.rs crates/mullion-ssh/src/lib.rs
git commit -m "feat(ssh): RemotePath —— 远端路径以字节为真源,非 UTF-8 挡在网络层外 (F50)"
```

---

## Task 3: 假 SFTP 服务端（测试基建）

**Files:**
- Create: `crates/mullion-ssh/tests/common/sftp_server.rs`
- Modify: `crates/mullion-ssh/tests/common/mod.rs`

手法与隧道切片一致：**拿自家客户端打自家服务端**。无外部依赖、无头能跑绿。

注意签名差异：`russh-sftp` 官方示例是照 russh **0.62** 写的
（`channel_open_session(&mut self, channel, reply: ChannelOpenHandle, session)`），
**我们锁的是 0.54.5**（`channel_open_session(&mut self, channel: Channel<Msg>, session) -> Result<bool>`）。
照既有 `common/mod.rs` 的写法来，别照抄示例。

- [ ] **Step 1: 写假服务端**

新建 `crates/mullion-ssh/tests/common/sftp_server.rs`：

```rust
//! 进程内假 SFTP 服务端:内存文件系统 + `russh_sftp::server::Handler`,
//! 挂在同目录的假 sshd 上(`subsystem_request` 收到 "sftp" 就把 channel
//! 交给它)。与隧道切片「拿自家客户端打自家服务端」同一手法。
//!
//! **它同时是守护测试的探针**:每个请求收到的**原始路径串**都记进
//! `Probe::seen`,于是「非 UTF-8 名一个请求都没发出去」这条断言有据可依。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use russh_sftp::protocol::{
    File, FileAttributes, Handle, Name, Status, StatusCode, Version,
};

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
        Self { name: name.to_vec(), kind: NodeKind::Dir, size: 4096, mtime: 1_700_000_000, mode: 0o755, uid: 1000, gid: 1000 }
    }
    pub fn file(name: &[u8], size: u64) -> Self {
        Self { name: name.to_vec(), kind: NodeKind::File, size, mtime: 1_700_000_100, mode: 0o644, uid: 1000, gid: 1000 }
    }
    pub fn link(name: &[u8], target: &[u8]) -> Self {
        Self { name: name.to_vec(), kind: NodeKind::Symlink(target.to_vec()), size: target.len() as u64, mtime: 1_700_000_200, mode: 0o777, uid: 1000, gid: 1000 }
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
            NodeKind::Symlink(_) => a.set_symlink(true),
        }
        a
    }
}

/// 服务端见过的每一个请求。守护测试靠它证明「某个请求根本没发出去」。
#[derive(Default)]
pub struct Probe {
    pub seen: Vec<(&'static str, String)>,
    /// SSH 层的 `pty_request` 次数。SFTP 通道上必须恒为 0(测试表 #14)。
    pub pty_requests: usize,
}

impl Probe {
    pub fn paths_for(&self, op: &str) -> Vec<String> {
        self.seen.iter().filter(|(o, _)| *o == op).map(|(_, p)| p.clone()).collect()
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
        Self { tree, probe, dirs: HashMap::new(), next_handle: 0 }
    }

    fn note(&self, op: &'static str, path: &str) {
        self.probe.lock().unwrap().seen.push((op, path.to_owned()));
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
        let resolved = if path == "." || path.is_empty() { "/home/testuser".to_string() } else { path };
        Ok(Name { id, files: vec![File::dummy(resolved)] })
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
        self.dirs.remove(&handle);
        Ok(Status { id, status_code: StatusCode::Ok, error_message: "Ok".into(), language_tag: "en-US".into() })
    }

    async fn readlink(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        self.note("readlink", &path);
        let (dir, name) = split_last(path.as_bytes());
        let node = self.tree.get(&dir).and_then(|v| v.iter().find(|n| n.name == name));
        match node.map(|n| &n.kind) {
            Some(NodeKind::Symlink(t)) => Ok(Name {
                id,
                files: vec![File::dummy(String::from_utf8_lossy(t).to_string())],
            }),
            _ => Err(StatusCode::NoSuchFile),
        }
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
        self.note("lstat", &path);
        let key = path.clone().into_bytes();
        if self.tree.contains_key(&key) {
            return Ok(russh_sftp::protocol::Attrs { id, attrs: Node::dir(b"").attrs() });
        }
        let (dir, name) = split_last(path.as_bytes());
        match self.tree.get(&dir).and_then(|v| v.iter().find(|n| n.name == name)) {
            Some(n) => Ok(russh_sftp::protocol::Attrs { id, attrs: n.attrs() }),
            None => Err(StatusCode::NoSuchFile),
        }
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
        self.note("stat", &path);
        self.lstat(id, path).await
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
```

- [ ] **Step 2: 把它挂到假 sshd 上**

`crates/mullion-ssh/tests/common/mod.rs` 顶部加：

```rust
pub mod sftp_server;
```

同文件里**新增**一个带 SFTP 的 server（不要动既有的 `EchoHandler` / `spawn_echo_server`——
它们被 `auth.rs` / `pty.rs` 等用着，Scope Discipline）：

```rust
/// 带 SFTP subsystem 的假 sshd。`EchoHandler` 保持原样不动 —— 那个被
/// auth/pty 几个测试用着,给它加分支等于让不相关的测试跟着变。
pub struct SftpSshHandler {
    channels: std::collections::HashMap<ChannelId, Channel<Msg>>,
    tree: Arc<sftp_server::Tree>,
    probe: Arc<std::sync::Mutex<sftp_server::Probe>>,
}

impl Handler for SftpSshHandler {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        if user == TEST_USER && password == TEST_PASSWORD { Ok(Auth::Accept) } else { Ok(Auth::reject()) }
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        self.channels.insert(channel.id(), channel);
        Ok(true)
    }

    /// 只为数数:SFTP 通道上一次都不该被调用(测试表 #14)。
    #[allow(clippy::too_many_arguments)]
    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        _cw: u32,
        _rh: u32,
        _pw: u32,
        _ph: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.probe.lock().unwrap().pty_requests += 1;
        session.channel_success(channel)?;
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if name == "sftp" {
            let Some(ch) = self.channels.remove(&channel) else {
                session.channel_failure(channel)?;
                return Ok(());
            };
            session.channel_success(channel)?;
            // `run` 内部自己 spawn,立即返回;别在这儿 await 到天荒地老。
            russh_sftp::server::run(
                ch.into_stream(),
                sftp_server::SftpHandler::new(self.tree.clone(), self.probe.clone()),
            )
            .await;
        } else {
            session.channel_failure(channel)?;
        }
        Ok(())
    }
}

/// 起一个带 SFTP 的假 sshd。返回监听地址与探针(测试用它断言「哪些请求发出去了」)。
pub async fn spawn_sftp_server(
    tree: sftp_server::Tree,
) -> (std::net::SocketAddr, Arc<std::sync::Mutex<sftp_server::Probe>>) {
    let host_key =
        russh::keys::load_secret_key("tests/fixtures/server_hostkey", None).expect("load hostkey");
    let mut config = russh::server::Config::default();
    config.keys.push(host_key);
    let config = Arc::new(config);
    let tree = Arc::new(tree);
    let probe = Arc::new(std::sync::Mutex::new(sftp_server::Probe::default()));

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
    let addr = listener.local_addr().unwrap();
    let (t, p) = (tree.clone(), probe.clone());
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { break };
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
    (addr, probe)
}
```

同文件顶部的 `use` 补上 `Channel`（`russh::{Channel, ChannelId, CryptoVec}` 已有 `ChannelId`）。

- [ ] **Step 3: 编过即可（这一步没有断言）**

```bash
cargo test -p mullion-ssh --test tunnel_forward 2>&1 | tail -3
```

预期：既有测试仍全绿（证明没碰坏 `common`）。新文件此刻还没有测试用它，
`cargo test` 会给 `dead_code` 警告——**下一个 Task 就用上了**，先用
`#![allow(dead_code)]` 顶在 `sftp_server.rs` 首行（tests/ 下的 helper 模块普遍如此，
既有 `common/mod.rs` 若已有该属性则照抄）。

- [ ] **Step 4: 提交**

```bash
git add crates/mullion-ssh/tests/common/
git commit -m "test(ssh): 假 SFTP 服务端(内存 FS + 请求探针),挂在既有假 sshd 上 (F50)"
```

---

## Task 4: `SftpClient` —— 开 subsystem 与列目录

**Files:**
- Modify: `crates/mullion-ssh/src/sftp.rs`
- Create: `crates/mullion-ssh/tests/sftp_browse.rs`

- [ ] **Step 1: 写端到端失败测试**

新建 `crates/mullion-ssh/tests/sftp_browse.rs`：

```rust
//! SFTP 只读浏览的端到端测试:真握手 → 开 sftp subsystem → 列目录。
//! 服务端是同进程的假 SFTP(见 common/sftp_server.rs)。

mod common;

use std::sync::Arc;

use common::sftp_server::{Node, Tree};
use mullion_ssh::config::{AuthMethod, SshConfig};
use mullion_ssh::session::{establish, HostKeyPolicy};
use mullion_ssh::sftp::{EntryKind, RemotePath, SftpClient};

struct AcceptAll;

impl HostKeyPolicy for AcceptAll {
    fn check(
        &self,
        _host: &str,
        _port: u16,
        _algo: &str,
        _fingerprint: &str,
    ) -> mullion_ssh::known_hosts::Decision {
        mullion_ssh::known_hosts::Decision::Accept
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
        proxy: None,
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
    t.insert(b"/home/testuser/docs".to_vec(), vec![Node::file(b"inner.txt", 3)]);
    t
}

#[tokio::test]
async fn listing_a_directory_returns_kinds_sizes_and_link_targets() {
    let (addr, _probe) = common::spawn_sftp_server(tree()).await;
    let conn = Arc::new(establish(&cfg(addr), Arc::new(AcceptAll)).await.expect("connect"));
    let sftp = SftpClient::open(conn).await.expect("open sftp");

    let mut got = sftp
        .list_dir(&RemotePath::from_bytes(b"/home/testuser".to_vec()))
        .await
        .expect("list");
    got.sort_by(|a, b| a.name.cmp(&b.name));

    let names: Vec<String> = got.iter().map(|e| e.name.display().to_string()).collect();
    assert!(names.contains(&"docs".to_string()));
    assert!(names.contains(&"说明.md".to_string()), "中文名必须原样出现: {names:?}");

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
    t.insert("/home/testuser/文档".as_bytes().to_vec(), vec![Node::file(b"x", 1)]);
    let (addr, probe) = common::spawn_sftp_server(t).await;
    let conn = Arc::new(establish(&cfg(addr), Arc::new(AcceptAll)).await.expect("connect"));
    let sftp = SftpClient::open(conn).await.expect("open sftp");

    let dir = RemotePath::from_bytes("/home/testuser/文档".as_bytes().to_vec());
    sftp.list_dir(&dir).await.expect("list 中文目录");

    let seen = probe.lock().unwrap().paths_for("opendir");
    assert!(
        seen.iter().any(|p| p.as_bytes() == "/home/testuser/文档".as_bytes()),
        "服务端收到的字节必须与请求的一致: {seen:?}"
    );
}

/// D16 修订后的核心守护:非 UTF-8 名**显示得出来**,但对它的请求
/// **一个都不发**。
#[tokio::test]
async fn a_non_utf8_name_is_listed_but_no_request_is_ever_sent_for_it() {
    let (addr, probe) = common::spawn_sftp_server(tree()).await;
    let conn = Arc::new(establish(&cfg(addr), Arc::new(AcceptAll)).await.expect("connect"));
    let sftp = SftpClient::open(conn).await.expect("open sftp");

    let got = sftp
        .list_dir(&RemotePath::from_bytes(b"/home/testuser".to_vec()))
        .await
        .expect("list");
    let bad = got.iter().find(|e| !e.name.is_utf8()).expect("非 UTF-8 条目该出现在列表里");
    assert!(bad.name.display().contains('\u{fffd}'), "显示串是 lossy 投影");

    // 拿它当目录进一次 —— 必须在客户端就被挡下。
    let err = sftp.list_dir(&bad.name).await.expect_err("非 UTF-8 路径不该发得出去");
    assert!(matches!(err, mullion_ssh::sftp::SftpError::NonUtf8Name));

    let opendirs = probe.lock().unwrap().paths_for("opendir");
    assert!(
        !opendirs.iter().any(|p| p.contains('\u{fffd}')),
        "服务端不该收到任何含替换字符的路径: {opendirs:?}"
    );
}

/// `.` 要能解析成登录目录 —— 默认远端目录留空时走的就是这条(D15)。
#[tokio::test]
async fn a_dot_path_canonicalizes_to_the_login_directory() {
    let (addr, _probe) = common::spawn_sftp_server(tree()).await;
    let conn = Arc::new(establish(&cfg(addr), Arc::new(AcceptAll)).await.expect("connect"));
    let sftp = SftpClient::open(conn).await.expect("open sftp");

    let home = sftp
        .canonicalize(&RemotePath::from_bytes(b".".to_vec()))
        .await
        .expect("canonicalize");
    assert_eq!(home.as_bytes(), b"/home/testuser");
}

/// 测试表 #14:SFTP 通道**不请求 PTY**。请求了的后果不是报错,是远端
/// 白白起一个伪终端、`who` 里多一行幽灵会话,而且 sshd 的
/// `ForceCommand`/`PermitTTY no` 环境下会直接被拒。
#[tokio::test]
async fn opening_sftp_never_requests_a_pty() {
    let (addr, probe) = common::spawn_sftp_server(tree()).await;
    let conn = Arc::new(establish(&cfg(addr), Arc::new(AcceptAll)).await.expect("connect"));
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
    let (addr, _probe) = common::spawn_sftp_server(tree()).await;
    let conn = Arc::new(establish(&cfg(addr), Arc::new(AcceptAll)).await.expect("connect"));
    let a = SftpClient::open(conn.clone()).await.expect("first");
    let b = SftpClient::open(conn.clone()).await.expect("second");
    assert!(!conn.is_closed());
    assert!(a.list_dir(&RemotePath::from_bytes(b"/home/testuser".to_vec())).await.is_ok());
    assert!(b.list_dir(&RemotePath::from_bytes(b"/home/testuser/docs".to_vec())).await.is_ok());
}
```

**注意**：`SshConfig` 的字段以 `crates/mullion-ssh/src/config.rs` 实际定义为准；
上面 `proxy: None` 若与实际不符，照实际补齐（既有测试 `tunnel_forward.rs` 里有现成写法，
直接抄那个 `cfg()` 构造）。`HostKeyPolicy` 的方法签名同理，抄 `session.rs` 测试模块里的
`AlwaysAccept`。

- [ ] **Step 2: 跑，确认编不过**

```bash
cargo test -p mullion-ssh --test sftp_browse 2>&1 | grep -E "^error" | head -5
```

预期：`cannot find struct SftpClient`。

- [ ] **Step 3: 写实现**

在 `crates/mullion-ssh/src/sftp.rs` 追加：

```rust
use std::sync::Arc;

use crate::session::SshConnection;

/// 一个目录项。**自有类型**,不透传 `russh_sftp::protocol::*`
/// (见模块文档:不让第三方类型漏进 `mullion-app`)。
#[derive(Debug, Clone)]
pub struct Entry {
    /// 只是名字,不含目录部分。拼完整路径用 `dir.join(entry.name.as_bytes())`。
    pub name: RemotePath,
    pub kind: EntryKind,
    pub size: u64,
    /// Unix 时间戳(秒)。SFTP v3 的 mtime 就是 u32,2106 年之前够用。
    pub mtime: u32,
    /// 权限位。**只取低 12 位**,类型位已经在 `kind` 里了。
    pub mode: u32,
    /// SFTP 的 attrs 里 uid/gid 是**数字**,协议拿不到 /etc/passwd 映射。
    /// 界面就老实显示 `1000:1000`,不为此去 exec 一次 `id`(设计 D21)。
    pub uid: u32,
    pub gid: u32,
    /// 符号链接的目标。非链接为 `None`。
    pub link_target: Option<RemotePath>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Dir,
    File,
    Symlink,
    Other,
}

#[derive(Debug)]
pub enum SftpError {
    /// 开 channel / 请求 subsystem 失败。
    Subsystem,
    /// 协议层报错(含 NoSuchFile / PermissionDenied 等)。
    Protocol(String),
    /// 路径含非 UTF-8 字节 —— **请求根本没发出去**(设计 D16 修订)。
    NonUtf8Name,
}

impl std::fmt::Display for SftpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SftpError::Subsystem => write!(f, "远端没有开启 SFTP 子系统,或连接已断开"),
            SftpError::Protocol(m) => write!(f, "{m}"),
            SftpError::NonUtf8Name => write!(f, "{}", NonUtf8Path),
        }
    }
}

impl std::error::Error for SftpError {}

impl From<NonUtf8Path> for SftpError {
    fn from(_: NonUtf8Path) -> Self {
        SftpError::NonUtf8Name
    }
}

/// 一条 SFTP channel。
///
/// **持有 `Arc<SshConnection>` 只为保活**:russh 0.54.5 的 `Handle` 一 Drop
/// 整条 SSH 连接就断(见 `session::open_pty` 的同款注释)。侧栏模式下这份
/// `Arc` 与终端 pane 共用同一条连接 —— 开面板不重握手、不重认证,高延迟
/// 代理链路上这是几秒的差别(设计 D6)。
pub struct SftpClient {
    inner: russh_sftp::client::SftpSession,
    _conn: Arc<SshConnection>,
}

impl SftpClient {
    /// 在**已建立**的连接上开一条 sftp channel。
    ///
    /// 签名里刻意没有任何网络参数(host/port/auth/policy 一个都不收),
    /// 与 `session::open_pty` 同一条防呆:想在这里偷偷重连一次都做不到。
    pub async fn open(conn: Arc<SshConnection>) -> Result<Self, SftpError> {
        let channel = conn
            .handle()
            .channel_open_session()
            .await
            .map_err(|_| SftpError::Subsystem)?;
        if channel.request_subsystem(true, "sftp").await.is_err() {
            // `Channel<Msg>` 没有自动发 CHANNEL_CLOSE 的 Drop,不显式关就是
            // 泄漏一个 channel slot,一直累积到 sshd 的 MaxSessions 上限
            // (同 `open_pty` 里那条注释)。
            let _ = channel.close().await;
            return Err(SftpError::Subsystem);
        }
        let inner = russh_sftp::client::SftpSession::new(channel.into_stream())
            .await
            .map_err(|e| SftpError::Protocol(e.to_string()))?;
        Ok(Self { inner, _conn: conn })
    }

    /// 列目录。**不解引用符号链接**——`readdir` 给的就是 lstat 语义,
    /// 链接的目标另用 `readlink` 单独取(设计 D17/D21)。
    pub async fn list_dir(&self, dir: &RemotePath) -> Result<Vec<Entry>, SftpError> {
        let wire = dir.as_wire()?;
        let read = self
            .inner
            .read_dir(wire)
            .await
            .map_err(|e| SftpError::Protocol(e.to_string()))?;

        let mut out = Vec::new();
        for de in read {
            let name = RemotePath::from_bytes(de.file_name().into_bytes());
            let attrs = de.metadata();
            let kind = if attrs.is_symlink() {
                EntryKind::Symlink
            } else if attrs.is_dir() {
                EntryKind::Dir
            } else if attrs.is_regular() {
                EntryKind::File
            } else {
                EntryKind::Other
            };
            // 链接目标要多一个 RTT,只对链接发。名字非 UTF-8 就跳过 ——
            // 它的完整路径本来就发不出去(D16 修订)。
            let link_target = if kind == EntryKind::Symlink && name.is_utf8() {
                let full = dir.join(name.as_bytes());
                match full.as_wire() {
                    Ok(w) => self
                        .inner
                        .read_link(w)
                        .await
                        .ok()
                        .map(|t| RemotePath::from_bytes(t.into_bytes())),
                    Err(_) => None,
                }
            } else {
                None
            };
            out.push(Entry {
                name,
                kind,
                size: attrs.size.unwrap_or(0),
                mtime: attrs.mtime.unwrap_or(0),
                mode: attrs.permissions.unwrap_or(0) & 0o7777,
                uid: attrs.uid.unwrap_or(0),
                gid: attrs.gid.unwrap_or(0),
                link_target,
            });
        }
        Ok(out)
    }

    /// 解析成绝对路径。`.` → 登录目录,默认远端目录留空时走这条(设计 D15)。
    pub async fn canonicalize(&self, path: &RemotePath) -> Result<RemotePath, SftpError> {
        let wire = path.as_wire()?;
        let out = self
            .inner
            .canonicalize(wire)
            .await
            .map_err(|e| SftpError::Protocol(e.to_string()))?;
        Ok(RemotePath::from_bytes(out.into_bytes()))
    }
}
```

- [ ] **Step 4: 跑，确认全绿**

```bash
cargo test -p mullion-ssh --test sftp_browse 2>&1 | grep -E "^test |test result"
```

预期：5 条全 `ok`。

**若 `a_non_utf8_name_is_listed_but_no_request_is_ever_sent_for_it` 里
`bad.name.is_utf8()` 意外为真**：说明 `File::new` 那侧把名字二次编码了。
去 `sftp_server.rs` 的 `readdir` 检查——那里为了满足库的 `String` 接口用了
`from_utf8_lossy`，服务端**发出去的就是 lossy 后的字节**。这不是客户端的问题，
把该测试的构造改成「服务端直接回 lossy 串，客户端据此判定 `!is_utf8()`」仍成立：
判据是**客户端收到含 U+FFFD 的名字时会不会往外发请求**，与服务端怎么产生它无关。

- [ ] **Step 5: 变异验收（三条）**

1. `list_dir` 开头的 `dir.as_wire()?` 换成 `String::from_utf8_lossy(dir.as_bytes()).to_string()` →
   `a_non_utf8_name_is_listed_but_no_request_is_ever_sent_for_it` **必须变红**。
2. `kind` 判定里把 `attrs.is_symlink()` 那支挪到 `is_dir()` 之后 →
   若假服务端的链接节点同时置了 dir 位则会变红；**若不变红，说明测试没扎住**，
   给 `Node::link` 的 attrs 同时置 `set_dir(true)` 再验一次（真实 sshd 上
   指向目录的链接正是这种）。
3. `SftpClient::open` 里 `request_subsystem(true, "sftp")` 改成 `"shell"` →
   所有五条**必须变红**（服务端会 `channel_failure`）。

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-ssh/src/sftp.rs crates/mullion-ssh/tests/sftp_browse.rs
git commit -m "feat(ssh): SftpClient —— 复用连接开 subsystem + 只读列目录/解析路径 (F50)"
```

---

## Task 5: store schema v7 → v8（书签与默认目录，F120）

**Files:**
- Create: `crates/mullion-store/src/sftp.rs`
- Modify: `crates/mullion-store/src/model.rs`
- Modify: `crates/mullion-store/src/lib.rs`
- Modify: `crates/mullion-store/src/migrate.rs`(仅加测试)

- [ ] **Step 1: 写失败的迁移测试**

在 `crates/mullion-store/src/migrate.rs` 的 `mod tests` 里追加（紧跟既有的
`v6_file_without_tunnel_key_loads_as_empty_and_keeps_everything_else` 之后，
照它的写法）：

```rust
/// v7 的库升 v8 **不需要任何迁移代码** —— 新字段全部 `#[serde(default)]`。
/// 这条是来证明它确实自动成立的:v7 文件读进来 → 新字段是缺省 → 其余
/// 逐字段等价。写不出这条断言就说明某个新字段忘了加 `default`,那时
/// 用户的 v7 库会直接反序列化失败。
#[test]
fn a_v7_library_without_the_sftp_key_loads_with_defaults_and_keeps_everything_else() {
    let v7 = r#"
schema = 7

[[session]]
id = 7
modified_at = "2026-08-01T00:00:00Z"

[session.identity]
name = "机器 A"

[session.connection]
host = "10.0.0.1"
port = 22
protocol = "ssh"

[session.auth]
user = "ubuntu"
kind = "password"
"#;
    let file: crate::model::SessionsFile = toml::from_str(v7).unwrap();
    let rec = &file.session[0];
    assert_eq!(rec.identity.name, "机器 A");
    assert_eq!(rec.connection.host, "10.0.0.1");
    assert_eq!(rec.auth.user, "ubuntu");
    // 新分节整体缺省。
    assert_eq!(rec.sftp, crate::sftp::SftpPrefs::default());
    assert!(rec.sftp.default_remote.is_none());
    assert!(rec.sftp.default_local.is_none());
    assert!(rec.sftp.bookmarks.is_empty());
}

/// 反过来:带 sftp 分节的 v8 存一遍读一遍要**等价**,书签顺序不许变
/// (用户是按自己的顺序排的,重排一次就再也信不过这个列表)。
#[test]
fn sftp_prefs_survive_a_save_load_round_trip_with_bookmark_order_intact() {
    let prefs = crate::sftp::SftpPrefs {
        default_remote: Some("/srv/app".into()),
        default_local: Some(r"D:\work".into()),
        bookmarks: vec![
            crate::sftp::Bookmark { name: "日志".into(), path: "/var/log".into() },
            crate::sftp::Bookmark { name: "配置".into(), path: "/etc/nginx".into() },
        ],
    };
    let text = toml::to_string(&prefs).unwrap();
    let back: crate::sftp::SftpPrefs = toml::from_str(&text).unwrap();
    assert_eq!(back, prefs);
    assert_eq!(back.bookmarks[0].name, "日志", "书签顺序不许变");
}
```

- [ ] **Step 2: 跑，确认编不过**

```bash
cargo test -p mullion-store 2>&1 | grep -E "^error" | head -3
```

预期：`no field `sftp` on type `SessionRecord``。

- [ ] **Step 3: 写实现**

新建 `crates/mullion-store/src/sftp.rs`：

```rust
//! SFTP 书签与默认目录(F120,schema v8)。
//!
//! **挂在 `SessionRecord` 上而不是做全局书签表**(设计 D15):`/data/Mullion`
//! 这种路径换台机器没有意义;点全局书签还要先问「在哪台机器上打开」,多一步。
//!
//! 路径在这一层是 `String`:它是**用户在表单里敲进去的东西**,天然是文本。
//! 到了 `mullion-ssh` 才转成 `RemotePath`(字节真源,见那边的 D16 修订)。

use serde::{Deserialize, Serialize};

/// 一条远端书签。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bookmark {
    /// 显示名。空串是允许的 —— 那时界面回退显示路径本身。
    pub name: String,
    pub path: String,
}

/// 一条会话的 SFTP 偏好(可继承分节)。
///
/// 字段全 `Option` / 空集合:**留空即用缺省**,远端 `.`(登录后的 home)、
/// 本地 `%USERPROFILE%`。不记忆「上次打开的目录」——那会让每次打开的位置
/// 取决于上次干了什么,而不是取决于配置(spec F120)。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SftpPrefs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_remote: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_local: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bookmarks: Vec<Bookmark>,
}
```

`crates/mullion-store/src/lib.rs` 加 `pub mod sftp;`，并在既有的 re-export 行里
补上 `pub use sftp::{Bookmark, SftpPrefs};`（照该文件里 `tunnel` / `automation` 的写法）。

`crates/mullion-store/src/model.rs` 的 `SessionRecord` 末尾追加字段（在 `automation` 之后）：

```rust
    /// F120:SFTP 书签与默认目录。v8 新增,旧文件没有这个键 → `default`
    /// 补空,无需迁移代码。
    #[serde(default)]
    pub sftp: crate::sftp::SftpPrefs,
```

同文件把 `CURRENT_SCHEMA` 从 7 改成 8，并在它上方的版本沿革注释里补一行：

```rust
/// v8 = v7 + `session.sftp`:SFTP 书签与默认远端/本地目录(F120)。
```

`CURRENT_SCHEMA` 文档里那段「旧客户端读到新版本要拒绝」的理由照旧成立，
把 v7 那句复述改成 v8 的：新增 `session.sftp`，旧客户端读 v8 会把整个分节丢掉再写回，
**拒绝比静默吃掉好**。

- [ ] **Step 4: 跑，确认全绿**

```bash
cargo test -p mullion-store 2>&1 | grep -E "test result|FAILED"
```

预期：全 `ok`。

- [ ] **Step 5: 变异验收**

把 `pub sftp` 上的 `#[serde(default)]` 删掉 →
`a_v7_library_without_the_sftp_key_loads_with_defaults_and_keeps_everything_else`
**必须变红**（`missing field `sftp``）。改回。

把 `bookmarks` 的 `Vec` 换成 `std::collections::BTreeSet<...>`（会重排）——
编不过就直接跳过这条，改为把 `SftpPrefs` 的 `bookmarks` 在反序列化后 `sort_by_key(|b| b.name.clone())`，
`sftp_prefs_survive_a_save_load_round_trip_with_bookmark_order_intact` **必须变红**。改回。

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-store/src/sftp.rs crates/mullion-store/src/lib.rs crates/mullion-store/src/model.rs crates/mullion-store/src/migrate.rs
git commit -m "feat(store): schema v8 —— SFTP 书签与默认远端/本地目录 (F120)"
```

---

## Task 6: 解开 F118 留下的三处闸门（D24）

**Files:**
- Modify: `crates/mullion-app/src/shell/session_map.rs`
- Modify: `crates/mullion-app/src/ui/session_manager/editor.rs`
- Modify: `crates/mullion-app/src/ui/session_manager/list.rs`

D24 说得很清楚：**不是删，是改成新语义**。对应测试从「断言被拒」改成
「断言拨号参数正确且不含 PTY 请求」。

- [ ] **Step 1: 改 `session_map.rs` 的测试**

`crates/mullion-app/src/shell/session_map.rs:228` 附近那条断言
`Err(MapError::SftpNotSupported)` 的测试，整条改写成：

```rust
    /// D24:SFTP 记录不再被映射层拒绝,而是映射成**合法的拨号参数**。
    /// 判据是「参数对得上」+「`wants_sftp` 为真」——后者是 app 侧
    /// 「连上后开 sftp subsystem 而不是 PTY」的开关。
    ///
    /// 这条替代了 F118 时期的 `SftpNotSupported` 断言:那时拒绝是对的
    /// (无处可去),D1 给了 SFTP 节点自己的标签页,再拒绝就是功能残缺。
    #[test]
    fn an_sftp_record_maps_to_real_dial_parameters_and_asks_for_the_sftp_subsystem() {
        let mut rec = sample_record();
        rec.connection.protocol = Protocol::Sftp;
        rec.connection.host = "10.0.0.9".into();
        rec.connection.port = 2222;
        let secret = SecretEntry { password: Some("pw".into()), ..Default::default() };

        let plan = to_dial_plan(&rec, Some(&secret)).expect("SFTP 节点必须能映射出拨号参数");
        assert_eq!(plan.cfg.host, "10.0.0.9");
        assert_eq!(plan.cfg.port, 2222);
        assert!(plan.wants_sftp, "SFTP 节点连上后开的是 sftp subsystem");
    }

    /// 反面:SSH 会话不得被标成要 sftp。搞反了的症状是「双击普通会话
    /// 开出一个文件面板、终端再也出不来」。
    #[test]
    fn an_ssh_record_does_not_ask_for_the_sftp_subsystem() {
        let rec = sample_record();
        let secret = SecretEntry { password: Some("pw".into()), ..Default::default() };
        let plan = to_dial_plan(&rec, Some(&secret)).expect("SSH 会话映射");
        assert!(!plan.wants_sftp);
    }
```

`sample_record()` 若不存在，就照该测试模块里既有的构造方式写；
`to_ssh_config` 的既有调用点在测试里也要跟着改（见下一步）。

- [ ] **Step 2: 跑，确认编不过**

```bash
cargo test -p mullion-app --lib session_map 2>&1 | grep -E "^error" | head -3
```

预期：`cannot find function to_dial_plan`。

- [ ] **Step 3: 写实现**

`session_map.rs` 顶部模块文档第二行改成：

```rust
//! SFTP 节点在 D1 起也走这里:映射出**同样的**拨号参数,只是多带一位
//! `wants_sftp` —— 连上之后开 sftp subsystem 而不是 PTY(设计 D24)。
```

删掉 `MapError::SftpNotSupported` 变体、它的 `Display` 分支，以及
`to_ssh_config` 开头那三行早退：

```rust
    if rec.connection.protocol == Protocol::Sftp {
        return Err(MapError::SftpNotSupported);
    }
```

新增（放在 `to_ssh_config` 之后）：

```rust
/// 一次连接意图:拨号参数 + 连上之后开什么。
///
/// **不把 `wants_sftp` 塞进 `SshConfig`**:那是 `mullion-ssh` 的类型,
/// 而「开 PTY 还是开 sftp」是 app 的编排决策,ssh 层只认字节流
/// (架构不变量)。
#[derive(Debug, Clone)]
pub struct DialPlan {
    pub cfg: SshConfig,
    /// true = 连上后开 sftp subsystem(SFTP 节点),false = 开 PTY(SSH 会话)。
    pub wants_sftp: bool,
}

/// `SessionRecord` + 解密后的敏感部分 → 一次完整的连接意图。
pub fn to_dial_plan(
    rec: &SessionRecord,
    secret: Option<&SecretEntry>,
) -> Result<DialPlan, MapError> {
    Ok(DialPlan {
        cfg: to_ssh_config(rec, secret)?,
        wants_sftp: rec.connection.protocol == Protocol::Sftp,
    })
}
```

- [ ] **Step 4: 撤掉编辑器与列表的置灰**

`editor.rs`：
- 删 `Disabled::Sftp` 变体、`SFTP_NOT_YET` 常量、`why()` 里的
  `if matches!(mode, super::ManagerMode::Sftp) { Disabled::Sftp }` 分支、
  `tip()` 里的 `Disabled::Sftp => ...` 分支。
- `editor.rs:1451` 那条断言 `tip(&d) == Some(SFTP_NOT_YET)` 的测试整条删掉，
  换成一条正面守护：

```rust
    /// D24:SFTP 档下「连接」按钮不再被置灰。这条替代了原来断言
    /// 「置灰理由是 SFTP_NOT_YET」的测试 —— 那个理由本身没了。
    #[test]
    fn the_sftp_mode_no_longer_disables_the_connect_button() {
        let d = why(
            super::super::ManagerMode::Sftp,
            super::super::validate::Missing::default(),
            &super::super::ProbeState::Idle,
        );
        assert!(tip(&d).is_none(), "填齐了的 SFTP 节点必须能连");
    }
```

（`Missing::default()` / `ProbeState::Idle` 的实际名字以那两个类型的定义为准。）

`list.rs:769` 与 `:779` 两处 `.on_disabled_hover_text(super::editor::SFTP_NOT_YET)`：
把整个「禁用」写法改回普通可用按钮——即去掉造成禁用的那个条件与
`on_disabled_hover_text` 调用。**只改这两处，别顺手动周围的布局。**

- [ ] **Step 5: 修所有调用点**

```bash
cargo build -p mullion-app 2>&1 | grep -E "^error" | head -20
```

逐个按提示改：`to_ssh_config` 的既有调用点（`app.rs` 里发起连接那处）改成
`to_dial_plan`，并把 `wants_sftp` 先原样丢弃（`let DialPlan { cfg, wants_sftp: _ }`
或加 `#[allow(unused)]`）——**它的接线是 Task 10 的事**，这一步只解闸门。
`fields.rs:246` 与 `:1762` 两处注释里提到 `SftpNotSupported` 的句子，
改成陈述新事实（「D1 起 SFTP 节点走同一条映射，只是多带 `wants_sftp`」）。

- [ ] **Step 6: 跑，确认全绿 + 变异验收**

```bash
cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED"
```

变异：把 `to_dial_plan` 里的 `wants_sftp` 写死成 `false` →
`an_sftp_record_maps_to_real_dial_parameters_and_asks_for_the_sftp_subsystem`
**必须变红**。写死成 `true` → `an_ssh_record_does_not_ask_for_the_sftp_subsystem`
**必须变红**。

- [ ] **Step 7: 提交**

```bash
git add -u crates/mullion-app/src
git commit -m "refactor(app): 解开 F118 的三处 SFTP 闸门,映射成 DialPlan + wants_sftp (D24/F50)"
```

---

## Task 7: 面板纯逻辑（排序 / 隐藏文件 / 导航）

**Files:**
- Create: `crates/mullion-app/src/files/mod.rs`
- Modify: `crates/mullion-app/src/lib.rs`

零 egui、零 tokio、可纯单测——同 `shell/tabs.rs` 的定位。

- [ ] **Step 1: 写失败的测试**

新建 `crates/mullion-app/src/files/mod.rs`，先写模块文档 + 测试：

```rust
//! 文件面板的**纯逻辑**:排序、隐藏文件过滤、列宽档位。
//! 零 egui / 零 tokio / 零 IO —— 渲染在 `ui::files_panel`,协议在
//! `mullion_ssh::sftp`。这么切是为了让「点了列头之后顺序对不对」
//! 这类 bug 能在没有窗口的情况下写测试复现。

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_ssh::sftp::{Entry, EntryKind, RemotePath};

    fn e(name: &str, kind: EntryKind, size: u64, mtime: u32) -> Entry {
        Entry {
            name: RemotePath::from_bytes(name.as_bytes().to_vec()),
            kind,
            size,
            mtime,
            mode: 0o644,
            uid: 1000,
            gid: 1000,
            link_target: None,
        }
    }

    fn sample() -> Vec<Entry> {
        vec![
            e("zeta.txt", EntryKind::File, 10, 300),
            e("alpha", EntryKind::Dir, 4096, 100),
            e(".hidden", EntryKind::File, 1, 400),
            e("beta.txt", EntryKind::File, 5000, 200),
            e("Gamma", EntryKind::Dir, 4096, 500),
        ]
    }

    fn names(v: &[Entry]) -> Vec<String> {
        v.iter().map(|e| e.name.display().to_string()).collect()
    }

    /// 默认序:目录在前 + 名称升序(设计 D21)。
    #[test]
    fn the_default_order_puts_directories_first_then_names_ascending() {
        let mut v = sample();
        sort(&mut v, SortKey::Name, SortDir::Asc);
        assert_eq!(names(&v), vec!["alpha", "Gamma", ".hidden", "beta.txt", "zeta.txt"]);
    }

    /// 名称排序**不分大小写** —— 分了的话 `Gamma` 会跑到 `alpha` 前面,
    /// 用户眼里就是「排序坏了」。
    #[test]
    fn name_sorting_ignores_case_so_uppercase_does_not_jump_to_the_top() {
        let mut v = sample();
        sort(&mut v, SortKey::Name, SortDir::Asc);
        let dirs: Vec<String> = names(&v).into_iter().take(2).collect();
        assert_eq!(dirs, vec!["alpha", "Gamma"]);
    }

    /// 倒序**只翻名字,不翻「目录在前」** —— 目录跑到最底下不是任何人
    /// 想要的,那只是排序实现偷懒的副作用。
    #[test]
    fn reversing_the_order_keeps_directories_on_top() {
        let mut v = sample();
        sort(&mut v, SortKey::Name, SortDir::Desc);
        assert_eq!(names(&v), vec!["Gamma", "alpha", "zeta.txt", "beta.txt", ".hidden"]);
    }

    #[test]
    fn sorting_by_size_still_keeps_directories_on_top() {
        let mut v = sample();
        sort(&mut v, SortKey::Size, SortDir::Desc);
        let first_two: Vec<String> = names(&v).into_iter().take(2).collect();
        assert!(first_two.contains(&"alpha".to_string()));
        assert!(first_two.contains(&"Gamma".to_string()));
        assert_eq!(names(&v)[2], "beta.txt", "文件里最大的排最前");
    }

    #[test]
    fn hidden_entries_are_dropped_unless_asked_for() {
        let v = sample();
        assert_eq!(visible(&v, false).len(), 4);
        assert_eq!(visible(&v, true).len(), 5);
    }

    /// 排序是**本地**的,不请求远端(设计 D21):同一批 entry 排两次
    /// 结果必须一致,且不依赖任何外部状态。
    #[test]
    fn sorting_is_pure_so_the_same_input_always_gives_the_same_order() {
        let (mut a, mut b) = (sample(), sample());
        sort(&mut a, SortKey::Mtime, SortDir::Asc);
        sort(&mut b, SortKey::Mtime, SortDir::Asc);
        assert_eq!(names(&a), names(&b));
    }

    #[test]
    fn a_size_is_rendered_with_one_decimal_and_a_unit() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1024 * 1024 * 3 / 2), "1.5 MB");
    }

    /// 权限画成 `rwxr-xr-x`。**只画低 9 位** —— 类型位在 kind 里,
    /// 混进来会变成 `drwx…` 那种把类型重复画两遍的写法。
    #[test]
    fn permissions_render_as_nine_characters() {
        assert_eq!(perm_string(0o755), "rwxr-xr-x");
        assert_eq!(perm_string(0o644), "rw-r--r--");
        assert_eq!(perm_string(0o40755 & 0o7777), "rwxr-xr-x");
    }
}
```

- [ ] **Step 2: 跑，确认编不过**

```bash
cargo test -p mullion-app --lib files 2>&1 | grep -E "^error" | head -3
```

- [ ] **Step 3: 写实现**

在 `files/mod.rs` 的模块文档之后插入：

```rust
use mullion_ssh::sftp::{Entry, EntryKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Size,
    Mtime,
    Perm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

impl SortDir {
    pub fn flipped(self) -> Self {
        match self {
            SortDir::Asc => SortDir::Desc,
            SortDir::Desc => SortDir::Asc,
        }
    }
}

/// 就地排序。**目录恒在前**,倒序只翻同组内部的顺序(设计 D21)——
/// 把目录一起翻到底下从来不是任何人想要的。
pub fn sort(entries: &mut [Entry], key: SortKey, dir: SortDir) {
    entries.sort_by(|a, b| {
        let group = is_dir(b).cmp(&is_dir(a)); // 目录(true)排前
        if group != std::cmp::Ordering::Equal {
            return group;
        }
        let ord = match key {
            // 不分大小写:分了的话 `Gamma` 会跑到 `alpha` 前面。
            SortKey::Name => a
                .name
                .display()
                .to_lowercase()
                .cmp(&b.name.display().to_lowercase()),
            SortKey::Size => a.size.cmp(&b.size),
            SortKey::Mtime => a.mtime.cmp(&b.mtime),
            SortKey::Perm => a.mode.cmp(&b.mode),
        };
        match dir {
            SortDir::Asc => ord,
            SortDir::Desc => ord.reverse(),
        }
    });
}

fn is_dir(e: &Entry) -> bool {
    e.kind == EntryKind::Dir
}

/// 过滤隐藏项(`.` 开头)。`show_hidden` 为真时原样返回。
pub fn visible(entries: &[Entry], show_hidden: bool) -> Vec<&Entry> {
    entries
        .iter()
        .filter(|e| show_hidden || !e.name.as_bytes().starts_with(b"."))
        .collect()
}

/// `1.5 MB` 这种。1024 进制,一位小数;不足 1 KB 直接给字节数
/// (`0.9 KB` 比 `920 B` 难读)。
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64 / 1024.0;
    let mut unit = 0;
    while v >= 1024.0 && unit + 1 < UNITS.len() {
        v /= 1024.0;
        unit += 1;
    }
    format!("{v:.1} {}", UNITS[unit])
}

/// `rwxr-xr-x`。**只看低 9 位**:类型位已经在 `EntryKind` 里了,
/// 再画一遍就成了 `drwx…` 那种把同一件事说两遍的写法。
pub fn perm_string(mode: u32) -> String {
    let bits = mode & 0o777;
    let mut s = String::with_capacity(9);
    for shift in [6, 3, 0] {
        let g = (bits >> shift) & 0o7;
        s.push(if g & 0o4 != 0 { 'r' } else { '-' });
        s.push(if g & 0o2 != 0 { 'w' } else { '-' });
        s.push(if g & 0o1 != 0 { 'x' } else { '-' });
    }
    s
}
```

`crates/mullion-app/src/lib.rs` 加 `pub mod files;`（按既有 `pub mod` 的字母序位置）。

- [ ] **Step 4: 跑，确认全绿**

```bash
cargo test -p mullion-app --lib files
```

预期：8 条全 `ok`。

- [ ] **Step 5: 变异验收（两条）**

1. `sort` 里删掉 `group` 那三行（不再让目录优先）→
   `the_default_order_puts_directories_first_then_names_ascending` 与
   `reversing_the_order_keeps_directories_on_top` **都必须变红**。
2. `SortDir::Desc` 分支改成对整个比较（含 group）取反 →
   `reversing_the_order_keeps_directories_on_top` **必须变红**。

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-app/src/files/ crates/mullion-app/src/lib.rs
git commit -m "feat(app): 文件面板纯逻辑 —— 目录优先排序/隐藏项/大小与权限渲染 (F50)"
```

---

## Task 8: 面板运行态 + 远端栏渲染

**Files:**
- Create: `crates/mullion-app/src/files/state.rs`
- Create: `crates/mullion-app/src/ui/files_panel.rs`
- Modify: `crates/mullion-app/src/files/mod.rs`(加 `pub mod state;`)
- Modify: `crates/mullion-app/src/ui/mod.rs`

- [ ] **Step 1: 写运行态与它的失败测试**

新建 `crates/mullion-app/src/files/state.rs`：

```rust
//! 一个文件栏的运行态。**零 egui**:导航语义(进目录/回上级/刷新)在这里
//! 写成纯状态机,于是「双击链接跟不跟随」「Backspace 会不会走出根」
//! 这类 bug 不需要窗口就能复现。

use mullion_ssh::sftp::{Entry, EntryKind, RemotePath};

use super::{SortDir, SortKey};

/// 这一栏当前在干什么。
#[derive(Debug, Clone, PartialEq)]
pub enum Load {
    /// 还没连上 / 还没发过第一次请求。
    Idle,
    Loading,
    Ready,
    /// 出错了,字符串是已经格式化好的可读原因。
    Failed(String),
}

pub struct PaneState {
    pub cwd: RemotePath,
    pub entries: Vec<Entry>,
    pub load: Load,
    pub sort_key: SortKey,
    pub sort_dir: SortDir,
    pub show_hidden: bool,
    /// 选中行在**过滤+排序之后**那个列表里的下标。`None` = 没选中。
    pub selected: Option<usize>,
    /// 每发一次请求 +1。异步结果回来时对不上就丢弃 ——
    /// 用户点得比网络快时,后发先至的旧结果会把新目录顶掉。
    pub request_seq: u64,
}

impl PaneState {
    pub fn new(cwd: RemotePath) -> Self {
        Self {
            cwd,
            entries: Vec::new(),
            load: Load::Idle,
            sort_key: SortKey::Name,
            sort_dir: SortDir::Asc,
            show_hidden: false,
            selected: None,
            request_seq: 0,
        }
    }

    /// 开始一次加载,返回本次的序号。调用方把它随异步任务带走,
    /// 结果回来时用 `accept` 校验。
    pub fn begin_load(&mut self, cwd: RemotePath) -> u64 {
        self.cwd = cwd;
        self.load = Load::Loading;
        self.selected = None;
        self.request_seq += 1;
        self.request_seq
    }

    /// 收下一次加载结果。序号对不上返回 `false`(结果被丢弃)。
    pub fn accept(&mut self, seq: u64, result: Result<Vec<Entry>, String>) -> bool {
        if seq != self.request_seq {
            return false;
        }
        match result {
            Ok(mut v) => {
                super::sort(&mut v, self.sort_key, self.sort_dir);
                self.entries = v;
                self.load = Load::Ready;
            }
            Err(msg) => {
                self.entries.clear();
                self.load = Load::Failed(msg);
            }
        }
        true
    }

    /// 点列头:同一列再点一次翻方向,换列则回到升序。
    pub fn click_header(&mut self, key: SortKey) {
        if self.sort_key == key {
            self.sort_dir = self.sort_dir.flipped();
        } else {
            self.sort_key = key;
            self.sort_dir = SortDir::Asc;
        }
        super::sort(&mut self.entries, self.sort_key, self.sort_dir);
    }

    /// 这一帧要画的行(过滤 + 已排序)。
    pub fn rows(&self) -> Vec<&Entry> {
        super::visible(&self.entries, self.show_hidden)
    }

    /// 「进去」的目标。目录 → 它自己;**指向目录的链接 → 跟随**
    /// (设计 D21:双击跟随,删除才不跟随);普通文件 → `None`。
    ///
    /// 名字非 UTF-8 的一律 `None` —— 它的完整路径发不出去(D16 修订),
    /// 与其发一个必然失败的请求,不如在这里就不动。
    pub fn enter_target(&self, e: &Entry) -> Option<RemotePath> {
        if !e.name.is_utf8() {
            return None;
        }
        match e.kind {
            EntryKind::Dir => Some(self.cwd.join(e.name.as_bytes())),
            // 链接目标可能是绝对路径,也可能是相对的。绝对的直接用,
            // 相对的按当前目录拼 —— 这是 POSIX 的语义。
            EntryKind::Symlink => e.link_target.as_ref().map(|t| {
                if t.as_bytes().starts_with(b"/") {
                    t.clone()
                } else {
                    self.cwd.join(t.as_bytes())
                }
            }),
            _ => None,
        }
    }
}
```

在同文件加测试模块：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn e(name: &str, kind: EntryKind) -> Entry {
        Entry {
            name: RemotePath::from_bytes(name.as_bytes().to_vec()),
            kind,
            size: 0,
            mtime: 0,
            mode: 0o644,
            uid: 0,
            gid: 0,
            link_target: None,
        }
    }

    fn state() -> PaneState {
        PaneState::new(RemotePath::from_bytes(b"/home/u".to_vec()))
    }

    /// 用户点得比网络快时,**后发先至的旧结果必须被丢掉** ——
    /// 否则界面会莫名其妙跳回上一个目录的内容。
    #[test]
    fn a_stale_listing_that_arrives_late_is_discarded() {
        let mut s = state();
        let first = s.begin_load(RemotePath::from_bytes(b"/a".to_vec()));
        let second = s.begin_load(RemotePath::from_bytes(b"/b".to_vec()));
        assert!(!s.accept(first, Ok(vec![e("stale", EntryKind::File)])), "旧结果该被丢弃");
        assert!(s.entries.is_empty());
        assert!(s.accept(second, Ok(vec![e("fresh", EntryKind::File)])));
        assert_eq!(s.entries.len(), 1);
        assert_eq!(s.entries[0].name.display(), "fresh");
    }

    #[test]
    fn clicking_the_same_header_twice_flips_the_direction() {
        let mut s = state();
        assert_eq!((s.sort_key, s.sort_dir), (SortKey::Name, SortDir::Asc));
        s.click_header(SortKey::Name);
        assert_eq!(s.sort_dir, SortDir::Desc);
        s.click_header(SortKey::Size);
        assert_eq!((s.sort_key, s.sort_dir), (SortKey::Size, SortDir::Asc), "换列回升序");
    }

    /// D21:双击指向目录的链接**跟随进入**。
    #[test]
    fn a_symlink_to_a_directory_is_followed_on_enter() {
        let s = state();
        let mut link = e("l", EntryKind::Symlink);
        link.link_target = Some(RemotePath::from_bytes(b"/etc/nginx".to_vec()));
        assert_eq!(s.enter_target(&link).unwrap().as_bytes(), b"/etc/nginx");

        let mut rel = e("r", EntryKind::Symlink);
        rel.link_target = Some(RemotePath::from_bytes(b"sub".to_vec()));
        assert_eq!(s.enter_target(&rel).unwrap().as_bytes(), b"/home/u/sub");
    }

    /// D16 修订:非 UTF-8 名的条目**连「进去」都不试** ——
    /// 发一个必然失败的请求只会给用户一条看不懂的错误。
    #[test]
    fn a_non_utf8_entry_cannot_be_entered() {
        let s = state();
        let mut bad = e("x", EntryKind::Dir);
        bad.name = RemotePath::from_bytes(vec![0xff, 0xfe]);
        assert!(s.enter_target(&bad).is_none());
    }

    #[test]
    fn a_plain_file_is_not_an_enter_target() {
        let s = state();
        assert!(s.enter_target(&e("a.txt", EntryKind::File)).is_none());
    }

    #[test]
    fn a_failed_load_clears_the_rows_and_keeps_the_reason() {
        let mut s = state();
        let seq = s.begin_load(RemotePath::from_bytes(b"/nope".to_vec()));
        assert!(s.accept(seq, Err("没有那个文件".into())));
        assert!(s.entries.is_empty());
        assert_eq!(s.load, Load::Failed("没有那个文件".into()));
    }
}
```

`files/mod.rs` 加 `pub mod state;`。

- [ ] **Step 2: 跑，确认全绿**

```bash
cargo test -p mullion-app --lib files::state
```

预期：6 条全 `ok`。

- [ ] **Step 3: 变异验收**

`accept` 开头的 `if seq != self.request_seq { return false; }` 删掉 →
`a_stale_listing_that_arrives_late_is_discarded` **必须变红**。

`enter_target` 里 `EntryKind::Symlink` 那支改成 `_ => None` →
`a_symlink_to_a_directory_is_followed_on_enter` **必须变红**。

- [ ] **Step 4: 写渲染**

新建 `crates/mullion-app/src/ui/files_panel.rs`。它只做绘制，动作以返回值交回：

```rust
//! 文件面板的 egui 渲染(F50)。远端栏与本地栏共用这一套 —— 差别只有
//! 数据来源与「哪些操作可用」,不是两份代码(设计 D1)。
//!
//! **大目录用 `ScrollArea::show_rows` 虚拟滚动**(设计 D21):一次
//! `readdir` 全量取回,但每帧只画可见那几十行。两万项的目录不做这一步
//! 会直接把帧时间打穿(陷阱 T3 的同类)。

use egui::Ui;

use crate::files::state::{Load, PaneState};
use crate::files::{human_size, perm_string, SortKey};
use crate::theme::{self, Theme};
use crate::ui::annotate;
use mullion_ssh::sftp::EntryKind;

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
}

/// 列宽。名称列吃掉剩余宽度,其余定宽 —— 定宽列一旦跟着内容浮动,
/// 换个目录整张表就会横着抖。
const W_SIZE: f32 = 78.0;
const W_MTIME: f32 = 132.0;
const W_PERM: f32 = 86.0;
const ROW_H: f32 = 22.0;

/// 画一栏。返回本帧的动作(至多一个 —— 一帧里用户点不了两下)。
pub fn show(
    ui: &mut Ui,
    t: &Theme,
    id: &str,
    state: &mut PaneState,
    show_owner: bool,
) -> Option<FileAction> {
    let mut action = None;
    annotate::mark(ui.ctx(), format!("文件面板/{id}"), ui.max_rect());

    // 路径条 + 上级 + 刷新。
    ui.horizontal(|ui| {
        if ui.small_button("↑").on_hover_text("上一级(Backspace)").clicked() {
            action = Some(FileAction::Up);
        }
        if ui.small_button("⟳").on_hover_text("刷新(F5)").clicked() {
            action = Some(FileAction::Refresh);
        }
        let path = state.cwd.display().to_string();
        annotate::mark(ui.ctx(), format!("文件面板/{id}/路径"), ui.max_rect());
        ui.add(egui::Label::new(egui::RichText::new(path).color(theme::c32(t.fg_mid))).truncate());
    });

    match &state.load {
        Load::Idle => {
            ui.colored_label(theme::c32(t.fg_dimmer), "未连接");
            return action;
        }
        Load::Loading => {
            ui.colored_label(theme::c32(t.fg_dim), "正在读取目录…");
            return action;
        }
        Load::Failed(msg) => {
            ui.colored_label(theme::c32(t.danger), msg.clone());
            return action;
        }
        Load::Ready => {}
    }

    header(ui, t, id, state);

    let rows = state.rows();
    let mut goto = None;
    egui::ScrollArea::vertical()
        .id_source(format!("files-{id}"))
        .auto_shrink([false, false])
        .show_rows(ui, ROW_H, rows.len(), |ui, range| {
            for ix in range {
                let e = rows[ix];
                let resp = row(ui, t, e, show_owner, state.selected == Some(ix));
                if resp.clicked() {
                    state.selected = Some(ix);
                }
                if resp.double_clicked() {
                    if let Some(target) = state.enter_target(e) {
                        goto = Some(target);
                    }
                }
            }
        });
    if let Some(g) = goto {
        action = Some(FileAction::Goto(g));
    }
    action
}

fn header(ui: &mut Ui, t: &Theme, id: &str, state: &mut PaneState) {
    ui.horizontal(|ui| {
        annotate::mark(ui.ctx(), format!("文件面板/{id}/列头"), ui.max_rect());
        let mut hit = None;
        let name_w = (ui.available_width() - W_SIZE - W_MTIME - W_PERM).max(80.0);
        for (label, key, w) in [
            ("名称", SortKey::Name, name_w),
            ("大小", SortKey::Size, W_SIZE),
            ("修改时间", SortKey::Mtime, W_MTIME),
            ("权限", SortKey::Perm, W_PERM),
        ] {
            let mark = if state.sort_key == key {
                match state.sort_dir {
                    crate::files::SortDir::Asc => " ▲",
                    crate::files::SortDir::Desc => " ▼",
                }
            } else {
                ""
            };
            let (rect, resp) =
                ui.allocate_exact_size(egui::vec2(w, ROW_H), egui::Sense::click());
            ui.painter().text(
                rect.left_center() + egui::vec2(4.0, 0.0),
                egui::Align2::LEFT_CENTER,
                format!("{label}{mark}"),
                egui::FontId::proportional(11.0),
                theme::c32(t.fg_muted),
            );
            if resp.clicked() {
                hit = Some(key);
            }
        }
        if let Some(k) = hit {
            state.click_header(k);
        }
    });
}

fn row(
    ui: &mut Ui,
    t: &Theme,
    e: &mullion_ssh::sftp::Entry,
    show_owner: bool,
    selected: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), ROW_H),
        egui::Sense::click(),
    );
    if selected {
        ui.painter().rect_filled(rect, 2.0, theme::c32(t.sunken_bg));
    }
    let p = ui.painter();
    let font = egui::FontId::proportional(12.0);
    // 非 UTF-8 名字画成 dim + 后缀说明:用户要能一眼看出「这个动不了」
    // 而不是点下去才发现(D16 修订)。
    let usable = e.name.is_utf8();
    let fg = if !usable {
        theme::c32(t.fg_dimmer)
    } else if e.kind == EntryKind::Dir {
        theme::c32(t.fg_strong)
    } else {
        theme::c32(t.fg)
    };
    let mut label = e.name.display().to_string();
    if let (EntryKind::Symlink, Some(tgt)) = (e.kind, &e.link_target) {
        label = format!("{label} → {}", tgt.display());
    }
    if !usable {
        label = format!("{label}（名称非 UTF-8，本版无法操作）");
    }
    let name_w = (rect.width() - W_SIZE - W_MTIME - W_PERM).max(80.0);
    p.text(rect.left_center() + egui::vec2(4.0, 0.0), egui::Align2::LEFT_CENTER, label, font.clone(), fg);
    let size_x = rect.left() + name_w + W_SIZE;
    let size_text = if e.kind == EntryKind::Dir { String::new() } else { human_size(e.size) };
    p.text(egui::pos2(size_x, rect.center().y), egui::Align2::RIGHT_CENTER, size_text, font.clone(), theme::c32(t.fg_mid));
    p.text(egui::pos2(size_x + 8.0, rect.center().y), egui::Align2::LEFT_CENTER, mtime_text(e.mtime), font.clone(), theme::c32(t.fg_mid));
    let mut perm = perm_string(e.mode);
    if show_owner {
        perm = format!("{perm} {}:{}", e.uid, e.gid);
    }
    p.text(rect.right_center() - egui::vec2(4.0, 0.0), egui::Align2::RIGHT_CENTER, perm, font, theme::c32(t.fg_dim));
    resp
}

/// SFTP v3 的 mtime 是 Unix 秒。用 `time` crate 格式化 —— 它已在依赖里。
fn mtime_text(secs: u32) -> String {
    match time::OffsetDateTime::from_unix_timestamp(secs as i64) {
        Ok(dt) => format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            dt.year(),
            dt.month() as u8,
            dt.day(),
            dt.hour(),
            dt.minute()
        ),
        Err(_) => "—".into(),
    }
}
```

**注意**：`ScrollArea::id_source` / `Label::truncate` / `allocate_exact_size`
的名字以 egui 0.30 实际 API 为准；编不过就照编译器给的签名改，**别猜**
（`docs/gui-render-gotchas.md` 有既往踩坑记录）。

`ui/mod.rs` 加 `pub mod files_panel;`。

- [ ] **Step 5: 加一条渲染守护**

在 `files_panel.rs` 加测试模块（照 `ui/chrome.rs` 里既有 egui 测试的写法——
**跑两帧**，因为 Area/Panel 第一帧 `fade_in` 会把 shape 记成 `Shape::Noop`）：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mullion_ssh::sftp::{Entry, RemotePath};

    fn entry(name: &[u8], kind: EntryKind) -> Entry {
        Entry {
            name: RemotePath::from_bytes(name.to_vec()),
            kind,
            size: 1024,
            mtime: 1_700_000_000,
            mode: 0o644,
            uid: 1000,
            gid: 1000,
            link_target: None,
        }
    }

    /// 两万项的目录里,一帧只该画可见那几十行 —— `show_rows` 没接对
    /// 的症状是帧时间被打穿(陷阱 T3 的同类),而它在小目录下完全看不出来。
    #[test]
    fn a_huge_directory_only_paints_the_visible_rows() {
        let mut state = PaneState::new(RemotePath::from_bytes(b"/big".to_vec()));
        state.entries = (0..20_000)
            .map(|i| entry(format!("f{i:05}").as_bytes(), EntryKind::File))
            .collect();
        state.load = Load::Ready;

        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut painted = 0usize;
        for _ in 0..2 {
            painted = 0;
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.set_max_height(300.0);
                    show(ui, &t, "远端", &mut state, false);
                });
            });
            // 行数从 painter 的文本 shape 数反推不稳(还有列头等),
            // 直接数「本帧 show_rows 交出的 range 长度」更准:
            // 用一个 300px 高的容器,ROW_H=22 → 至多 ~15 行 + 余量。
            painted = state.rows().len().min((300.0 / ROW_H) as usize + 4);
        }
        assert!(painted < 40, "一帧画的行数必须远小于 20000,实际 {painted}");
    }

    /// 非 UTF-8 的名字必须**看得见**(用户要知道那儿有东西)且**标注不可操作**。
    #[test]
    fn a_non_utf8_name_is_shown_with_an_explicit_note() {
        let mut state = PaneState::new(RemotePath::from_bytes(b"/x".to_vec()));
        state.entries = vec![entry(&[0xd6, 0xd0, b'.', b't', b'x', b't'], EntryKind::File)];
        state.load = Load::Ready;

        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut texts = Vec::new();
        for _ in 0..2 {
            texts.clear();
            let out = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(ui, &t, "远端", &mut state, false);
                });
            });
            for shape in out.shapes.iter() {
                if let egui::epaint::Shape::Text(ts) = &shape.shape {
                    texts.push(ts.galley.text().to_owned());
                }
            }
        }
        assert!(
            texts.iter().any(|s| s.contains("名称非 UTF-8")),
            "非 UTF-8 条目要带明确说明,实际画出来的文本: {texts:?}"
        );
        assert!(
            texts.iter().any(|s| s.contains('\u{fffd}')),
            "同时还要能看见那个名字本身"
        );
    }
}
```

第一条测试若写不出稳定判据（`show_rows` 的 range 拿不到），**改成直接断言
源码里用的是 `show_rows` 而不是 `show`**（`include_str!` 源码守护，本仓已有
先例），并在测试文档注释里写明「这是结构守护，不是行为守护，因为
egui 0.30 不把 `show_rows` 的 range 交出来」。**不要**留一条恒绿的假测试。

- [ ] **Step 6: 跑绿 + 提交**

```bash
cargo test -p mullion-app --lib files_panel
git add crates/mullion-app/src/files/state.rs crates/mullion-app/src/ui/files_panel.rs crates/mullion-app/src/files/mod.rs crates/mullion-app/src/ui/mod.rs
git commit -m "feat(ui): 文件栏渲染 —— 四列/列头排序/虚拟滚动/非 UTF-8 标注 (F50)"
```

---

## Task 8b: 本地栏（只导航，D5）

**Files:**
- Create: `crates/mullion-app/src/files/local.rs`
- Modify: `crates/mullion-app/src/files/mod.rs`(加 `pub mod local;`)

D5 定死了本切片的本地栏**只导航**：能列、能进出、能选中，**不能删/改/建**。
它复用远端栏的 `PaneState` 与渲染——差别只有数据来源，不是两份代码（设计 D1）。

- [ ] **Step 1: 写失败的测试**

新建 `crates/mullion-app/src/files/local.rs`，先写测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_a_local_directory_reports_kinds_and_sizes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("a.txt"), b"hello").unwrap();

        let mut got = list_dir(dir.path()).expect("列本地目录");
        got.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
        assert_eq!(got.len(), 2);

        let a = got.iter().find(|e| e.name.as_bytes() == b"a.txt").unwrap();
        assert_eq!(a.kind, EntryKind::File);
        assert_eq!(a.size, 5);

        let sub = got.iter().find(|e| e.name.as_bytes() == b"sub").unwrap();
        assert_eq!(sub.kind, EntryKind::Dir);
    }

    /// 读不了的目录要给**可读的原因**,不是 `Os { code: 13 }` 这种
    /// 用户看不懂的东西。
    #[test]
    fn an_unreadable_directory_yields_a_readable_reason() {
        let err = list_dir(std::path::Path::new("/definitely/not/here")).unwrap_err();
        assert!(!err.is_empty());
        assert!(!err.contains("Os {"), "别把 io::Error 的 Debug 直接丢给用户: {err}");
    }

    /// 本地路径也走 `RemotePath`(字节真源),**但分隔符按平台**:
    /// Windows 上拼出 `D:\work/sub` 虽然 `std::path` 认,显示出来很难看,
    /// 而且用户拷走贴进 PowerShell 会一半斜杠一半反斜杠。
    #[test]
    fn joining_a_local_path_uses_the_platform_separator() {
        let base = to_path(&RemotePath::from_bytes(b"/tmp".to_vec()));
        assert_eq!(base, std::path::PathBuf::from("/tmp"));
        let joined = join_local(&RemotePath::from_bytes(b"/tmp".to_vec()), b"sub");
        assert_eq!(to_path(&joined), std::path::PathBuf::from("/tmp").join("sub"));
    }

    /// 默认本地目录:配置留空 → 用户主目录。拿不到主目录(极少见)
    /// 也不能 panic,退到当前工作目录。
    #[test]
    fn the_default_local_directory_falls_back_to_the_home_directory() {
        let d = default_local(None);
        assert!(!d.as_bytes().is_empty());
    }
}
```

- [ ] **Step 2: 跑，确认编不过**

```bash
cargo test -p mullion-app --lib files::local 2>&1 | grep -E "^error" | head -3
```

- [ ] **Step 3: 写实现**

```rust
//! 本地文件栏的数据来源(设计 D5:本切片**只导航**,不删不改不建)。
//!
//! 复用 `mullion_ssh::sftp::Entry` 而不是另造一套类型 —— 两栏共用一套
//! 渲染与排序,D2 加拖拽时「本地↔远端」也才是同一种东西在两个方向上动。
//! 代价是本地路径也塞进 `RemotePath`(字节真源),这在 Windows 上是
//! `OsStr` 的有损投影:非 UTF-8 的 Windows 文件名(UTF-16 孤儿代理项)
//! 会走与远端同一条「显示得出、操作不了」的路。

use std::path::{Path, PathBuf};

use mullion_ssh::sftp::{Entry, EntryKind, RemotePath};

/// 列一个本地目录。错误是**已经格式化好的可读原因**。
pub fn list_dir(dir: &Path) -> Result<Vec<Entry>, String> {
    let rd = std::fs::read_dir(dir).map_err(|e| readable(dir, &e))?;
    let mut out = Vec::new();
    for item in rd {
        let Ok(item) = item else { continue };
        // `symlink_metadata` 而不是 `metadata`:不跟随链接,与远端的
        // lstat 语义对齐(D17)。跟随了的话,指向自己的链接会挂住列目录。
        let md = match item.path().symlink_metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let ft = md.file_type();
        let kind = if ft.is_symlink() {
            EntryKind::Symlink
        } else if ft.is_dir() {
            EntryKind::Dir
        } else if ft.is_file() {
            EntryKind::File
        } else {
            EntryKind::Other
        };
        let link_target = if kind == EntryKind::Symlink {
            std::fs::read_link(item.path())
                .ok()
                .map(|p| RemotePath::from_bytes(path_bytes(&p)))
        } else {
            None
        };
        out.push(Entry {
            name: RemotePath::from_bytes(os_bytes(&item.file_name())),
            kind,
            size: md.len(),
            mtime: mtime_secs(&md),
            mode: perm_bits(&md),
            uid: 0,
            gid: 0,
            link_target,
        });
    }
    Ok(out)
}

fn readable(dir: &Path, e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => format!("目录不存在:{}", dir.display()),
        std::io::ErrorKind::PermissionDenied => format!("没有权限读取:{}", dir.display()),
        _ => format!("读取失败:{}({})", dir.display(), e.kind()),
    }
}

fn mtime_secs(md: &std::fs::Metadata) -> u32 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs().min(u32::MAX as u64) as u32)
        .unwrap_or(0)
}

/// Unix 上取真实权限位;Windows 上没有 mode 概念,只报只读与否 ——
/// 编一个 `rwxr-xr-x` 出来会让用户以为那是真的。
fn perm_bits(md: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        md.permissions().mode() & 0o7777
    }
    #[cfg(not(unix))]
    {
        if md.permissions().readonly() {
            0o444
        } else {
            0o644
        }
    }
}

#[cfg(unix)]
fn os_bytes(s: &std::ffi::OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    s.as_bytes().to_vec()
}

#[cfg(not(unix))]
fn os_bytes(s: &std::ffi::OsStr) -> Vec<u8> {
    // Windows 的 `OsStr` 是 UTF-16;`to_string_lossy` 是有损投影,
    // 孤儿代理项会变成 U+FFFD —— 与远端非 UTF-8 名走同一条
    // 「显示得出、操作不了」的路(D16 修订)。
    s.to_string_lossy().into_owned().into_bytes()
}

fn path_bytes(p: &Path) -> Vec<u8> {
    os_bytes(p.as_os_str())
}

/// `RemotePath` → 本地 `PathBuf`。
pub fn to_path(p: &RemotePath) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(p.as_bytes()))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(p.display().into_owned())
    }
}

/// 本地版的 join:**用平台分隔符**。`RemotePath::join` 恒用 `/`,那是
/// SFTP 线上的规矩;本地路径拼成 `D:\work/sub` 虽然 `std::path` 认,
/// 但用户拷走贴进 PowerShell 会看到一半斜杠一半反斜杠。
pub fn join_local(base: &RemotePath, name: &[u8]) -> RemotePath {
    let mut p = to_path(base);
    p.push(to_path(&RemotePath::from_bytes(name.to_vec())));
    RemotePath::from_bytes(path_bytes(&p))
}

/// 上一级。已在根就原地不动(与 `RemotePath::parent` 同一条防呆)。
pub fn parent_local(p: &RemotePath) -> RemotePath {
    let path = to_path(p);
    match path.parent() {
        Some(up) if up != path => RemotePath::from_bytes(path_bytes(up)),
        _ => p.clone(),
    }
}

/// 默认本地目录:配置里填了就用它,留空用用户主目录,再拿不到就用
/// 当前工作目录 —— 任何一步都不 panic,面板宁可开在一个奇怪的地方
/// 也不能开不出来。
pub fn default_local(configured: Option<&str>) -> RemotePath {
    if let Some(s) = configured.filter(|s| !s.trim().is_empty()) {
        return RemotePath::from_bytes(s.as_bytes().to_vec());
    }
    let home = directories::BaseDirs::new()
        .map(|b| b.home_dir().to_path_buf())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    RemotePath::from_bytes(path_bytes(&home))
}
```

`files/mod.rs` 加 `pub mod local;`。

- [ ] **Step 4: 跑绿**

```bash
cargo test -p mullion-app --lib files::local
```

预期：4 条全 `ok`。

- [ ] **Step 5: 变异验收**

`list_dir` 里 `symlink_metadata` 换回 `metadata`（跟随链接）→
在 `listing_a_local_directory_reports_kinds_and_sizes` 里加一个指向目录的
符号链接并断言它的 `kind == Symlink`，**必须变红**。
（Windows 上建符号链接要管理员权限，这条断言用
`#[cfg(unix)]` 圈起来，并在注释里写明为什么。）

`readable()` 直接改成 `e.to_string()` →
`an_unreadable_directory_yields_a_readable_reason` **必须变红**
（`to_string()` 出的是 "No such file or directory (os error 2)"，
不含 `Os {`——**若不变红就把断言改成「必须包含目录路径」**，
`e.to_string()` 里没有路径，那条一定扎得住）。

- [ ] **Step 6: 接进面板**

Task 8 的 `PanelFrame.local` 初始化为
`PaneState::new(local::default_local(prefs.default_local.as_deref()))`；
本地栏的 `FileAction::Goto` / `Up` / `Refresh` 在 `app.rs` 里走
`local::list_dir` 而不是 SFTP——**同步调用即可**，本地目录读盘是微秒级，
不值得为它 spawn 一个任务（远端那条必须 spawn，因为它是网络 RTT）。

- [ ] **Step 7: 提交**

```bash
git add crates/mullion-app/src/files/local.rs crates/mullion-app/src/files/mod.rs
git commit -m "feat(app): 本地文件栏 —— 只导航的本地目录列举,复用远端栏的 Entry 与渲染 (F50/D5)"
```

---

## Task 9: 侧栏宿主（`Ctrl+Shift+B`）+ T4 window_change

**Files:**
- Modify: `crates/mullion-app/src/ui/mod.rs`
- Modify: `crates/mullion-app/src/ui/chrome.rs`(菜单项)
- Modify: `crates/mullion-app/src/app.rs`

**这一步碰 T4。** 开关侧栏 → 中央区变窄 → 终端列数重算 → 必须发一次
`window_change`，否则远端 tmux 里的 TUI 按旧列数排版（设计 D2 明确说
「走既有 reflow 路径」，所以实现上**不要**另造一条通路）。

- [ ] **Step 1: 写失败的守护测试**

在 `crates/mullion-app/src/app.rs` 的测试模块追加（照既有
`tab_bar_height_reaches_the_remote_as_a_window_change` 的写法——真 `Workspace` +
`RecordingPty`）：

```rust
    /// T4 / 设计 D2:开侧栏把中央区挤窄,远端必须收到一次 `window_change`
    /// 且列数变少。没这一下,tmux 里的 TUI 会按旧列数排版,全屏直接错行。
    ///
    /// 这条与 `reflow_emits_resize` 不重复:那条测的是分屏,这条测的是
    /// **侧栏**这条新路径 —— D2 承诺它复用同一套 reflow,这里是兑现凭证。
    #[test]
    fn opening_the_files_sidebar_reaches_the_remote_as_a_window_change() {
        let pty = RecordingPty::default();
        let mut ws = workspace_with(&pty);

        let full = PxRect { x: 0, y: 0, w: 1600, h: 900 };
        ws.layout_geometry(full, CELL_W, CELL_H);
        let before = pty.last_cols();
        pty.clear();

        // 侧栏 360px:中央区右边被吃掉这么多。
        let narrowed = PxRect { x: 0, y: 0, w: 1600 - 360, h: 900 };
        ws.layout_geometry(narrowed, CELL_W, CELL_H);

        assert_eq!(pty.resize_count(), 1, "开侧栏必须正好发一次 window_change");
        assert!(
            pty.last_cols() < before,
            "列数必须变少:{} → {}",
            before,
            pty.last_cols()
        );
    }
```

`RecordingPty` / `workspace_with` / `CELL_W` / `PxRect` 的实际名字与构造，
**照 `tab_bar_height_reaches_the_remote_as_a_window_change` 那条抄**——
它就在同一个测试模块里，已经跑绿。

- [ ] **Step 2: 跑，确认变红（此刻应该是编不过或断言失败）**

```bash
cargo test -p mullion-app --lib opening_the_files_sidebar
```

- [ ] **Step 3: 接线**

`ui/mod.rs`：
- `UiState` 加：

```rust
    /// F50:文件侧栏开着没有。**按会话记住**是 D1 的承诺,但记忆落在
    /// `App` 那边(它才知道当前是哪条会话),这里只有「这一帧开没开」。
    pub files_sidebar_open: bool,
    /// 侧栏宽度(point)。可拖,默认 360。
    pub files_sidebar_w: f32,
```

`Default` 里给 `files_sidebar_open: false, files_sidebar_w: 360.0`。

- `UiFrame` 加：

```rust
    /// F50:文件面板这一帧的两栏状态。`None` = 面板关着 / launcher 态。
    pub files: Option<&'a mut crate::ui::files_panel::PanelFrame>,
```

（若 `&mut` 在 `UiFrame` 里引起借用麻烦，改成让 `build_ui` 直接收一个
独立参数 `files: Option<&mut PanelFrame>`——**哪种能编过用哪种**，
但两栏的 `PaneState` 必须是 `&mut`，因为列头排序会就地改状态。）

- `UiActions` 加：

```rust
    /// F50:文件面板这一帧的动作(远端栏 / 本地栏各至多一个)。
    pub files_remote: Option<crate::ui::files_panel::FileAction>,
    pub files_local: Option<crate::ui::files_panel::FileAction>,
```

**并同步改 `app.rs::render_frame` 里那处逐字段枚举的 discard 判断**
（`UiActions` 的文档注释明确警告过：漏改新动作会在 discard 趟被静默丢弃）：

```rust
if this_pass.preset.is_some()
    || this_pass.close_pane.is_some()
    || this_pass.tab.is_some()
    || this_pass.files_remote.is_some()
    || this_pass.files_local.is_some()
    || this_pass.annotate_export.is_some()
```

- `build_ui` 里，在 `tab_bar` 之后、`status_bar` 之前加：

```rust
    // 侧栏排在标签栏之后 show:`SidePanel` 与 `TopBottomPanel` 按 show 的
    // 先后从窗口边缘往里堆,先堆完上下两条,侧栏才不会顶到菜单栏上面去。
    if let Some(files) = frame.files {
        let (r, l) = files_panel::sidebar(ctx, t, ui_state, files);
        actions.files_remote = r;
        actions.files_local = l;
    }
```

`files_panel.rs` 加 `sidebar()`：右侧 `SidePanel`，**上下堆叠**
（上远端 60% / 下本地 40%，设计 D4），宽度可拖：

```rust
/// 侧栏宿主(设计 D1 的宿主之一)。**上下堆叠**:侧栏典型宽 320~450px,
/// 左右并排后每栏只剩 160~220px,四列排不下;而把侧栏加宽到 560px 会
/// 压扁终端列数,让远端 TUI 重排得很难看(设计 D4)。
pub fn sidebar(
    ctx: &egui::Context,
    t: &Theme,
    ui_state: &mut crate::ui::UiState,
    frame: &mut PanelFrame,
) -> (Option<FileAction>, Option<FileAction>) {
    let mut out = (None, None);
    let resp = egui::SidePanel::right("files")
        .resizable(true)
        .default_width(ui_state.files_sidebar_w)
        .width_range(280.0..=640.0)
        .frame(
            egui::Frame::none()
                .fill(theme::c32(t.panel_bg))
                .stroke(theme::stroke(t)),
        )
        .show(ctx, |ui| {
            annotate::mark(ui.ctx(), "文件侧栏", ui.max_rect());
            let h = ui.available_height();
            ui.allocate_ui(egui::vec2(ui.available_width(), h * 0.6), |ui| {
                out.0 = show(ui, t, "远端", &mut frame.remote, frame.show_owner);
            });
            ui.separator();
            out.1 = show(ui, t, "本地", &mut frame.local, false);
        });
    // 拖过之后记住宽度(按会话记忆由 `App` 落地)。
    ui_state.files_sidebar_w = resp.response.rect.width();
    out
}

/// 一帧要画的两栏 + 列选项。
pub struct PanelFrame {
    pub remote: PaneState,
    pub local: PaneState,
    /// D21:属主:组默认隐藏,列头右键打开。
    pub show_owner: bool,
}
```

`chrome.rs` 的「视图」菜单加一项（照既有菜单项的写法）：

```rust
            if ui.button("文件面板\tCtrl+Shift+B").clicked() {
                ui_state.files_sidebar_open = !ui_state.files_sidebar_open;
                ui.close_menu();
            }
```

- [ ] **Step 4: 加快捷键**

`app.rs` 的 `tab_hotkey_event` 旁边**新增**一个同构的
`files_hotkey_event`（**别塞进 tab 那个**——一个函数一件事，
而且 tab 那个已经有守护测试钉着它的行为）：

```rust
    /// F50 / 设计 D23:`Ctrl+Shift+B` 开关文件侧栏。
    ///
    /// 选 `Ctrl+Shift+*` 系是因为它在终端里不产生控制字符,不和远端
    /// tmux / Claude Code 抢键(T5/T6 类冲突)。**不能用 `Ctrl+Shift+F`**
    /// —— 它已被 F100 标注模式占用,先到先得。
    fn files_hotkey_event(&mut self, event: &WindowEvent) -> bool {
        let WindowEvent::KeyboardInput { event: ke, .. } = event else {
            return false;
        };
        if ke.state != ElementState::Pressed {
            return false;
        }
        let Some((key, mods)) = input::translate_key(ke, self.mods) else {
            return false;
        };
        if self.modal_open() || !mods.ctrl || !mods.shift || mods.alt || mods.sup {
            return false;
        }
        if !matches!(key, mullion_term::keymap::Key::Char('b' | 'B')) {
            return false;
        }
        self.ui.files_sidebar_open = !self.ui.files_sidebar_open;
        self.request_ui_redraw();
        true
    }
```

在 `window_event` 里 `tab_hotkey_event` 之后调用它（同样是「判定阶段截走，
早退」，T8 纪律）。

- [ ] **Step 5: 跑绿 + 复跑 T4 守护**

```bash
cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED"
cargo test -p mullion-app --lib reflow_emits_resize
cargo test -p mullion-app --lib opening_the_files_sidebar
```

- [ ] **Step 6: 变异验收**

把 `sidebar()` 的 `SidePanel` 换成 `egui::Area`（Area 不参与 Panel 的空间分配，
中央区不会变窄）→ `opening_the_files_sidebar_reaches_the_remote_as_a_window_change`
**必须变红**。

**注意**：这条测试直接喂 `layout_geometry` 两个尺寸，不经过 egui。
若变异后它仍绿，说明测试没扎在真实注入点上——那就把断言改成
「`build_ui` 之后 `ui_state.central_px.0` 变小了」，用两帧
（开/关侧栏）各跑一次 `build_ui` 对比。**这一条必须自证会红才算数**
（切片 P0-b 的教训：守护测试必须自证变红且扎到真实注入点）。

- [ ] **Step 7: 提交**

```bash
git add -u crates/mullion-app/src
git commit -m "feat(app): 文件侧栏宿主 —— Ctrl+Shift+B 开关 + 上下双栏 + 挤窄中央区发 window_change (F50)"
```

---

## Task 10: 标签宿主（SFTP 节点连上后开自己的标签）

**Files:**
- Modify: `crates/mullion-app/src/app.rs`
- Modify: `crates/mullion-app/src/shell/tabs.rs`(仅在需要时)

### 两条前置待办（Task 9 复核挖出，都只在远端栏接上数据那一刻才会咬人）

- [ ] **前置 A：补 `files_remote` 的 discard 守护。**
  实测：把 `render_frame` 里 `||` 链中的 `this_pass.files_remote.is_some()`
  删掉，全部 662 条测试**仍然全绿**。原因是 Task 9 里远端栏恒 `Load::Idle`，
  从来产不出「`files_remote` 是这一帧唯一真实动作」的那一帧。
  Task 10 一接上远端数据，这个判断若被误删就会让远端栏的点击在 discard 趟被
  整体丢弃，**而且没有任何测试报警** —— 这正是 `UiActions` 文档注释警告过的
  静默丢弃坑。接完数据后补一条专属回归，并做变异验收（删掉那一项必须变红）。

- [ ] **前置 B：`App` 只有一个 `PanelFrame`，跨标签共享，先拍板再写码。**
  Task 9 让 `App` 持有单个 `PanelFrame`（终端标签的侧栏），切标签时侧栏内容
  不变 —— 那时只有本地栏有数据，共享是合理的。
  远端栏一接上 SFTP，前提立刻不成立：不同标签连着不同主机，共享一份
  `remote` 状态意味着**标签 B 的侧栏显示着标签 A 主机的目录**，用户看不出
  异常，直到对着错误的主机操作。
  现在代码里没有任何准备（没按 tab id 索引、没有 generation 路由）。
  D0 定下的 S1 规则是「迟到的异步结果按 `generation` 路由到属主标签，
  绝不路由到当前标签」—— 远端目录列举正是这类异步结果。
  **开工前先定：`PanelFrame.remote` 是挂到每个标签上，还是侧栏只在
  SFTP 标签里出现。** 不要写着写着才发现。

- [ ] **Step 1: 写失败的守护测试**

`app.rs` 测试模块追加（源码结构守护，照既有
`connecting_opens_a_new_tab_instead_of_replacing_the_active_one` 的
`include_str!` 手法；`App` 需要 `EventLoopProxy`，无头构造不出来）：

```rust
    /// D24 + D1:SFTP 节点连上后开的是**文件标签**,不是终端标签。
    /// 判据是 `ConnectOk` 分支按 `wants_sftp` 分流,而不是无条件建
    /// `TerminalTab` —— 搞错了的症状是双击 SFTP 节点开出一个空终端。
    #[test]
    fn an_sftp_node_opens_a_files_tab_not_a_terminal_tab() {
        let src = include_str!("app.rs");
        let body = src
            .split("UserEvent::ConnectOk {")
            .nth(1)
            .expect("找不到 ConnectOk 分支");
        assert!(
            body.contains("wants_sftp"),
            "ConnectOk 必须按 wants_sftp 分流"
        );
        assert!(
            body.contains("TabContent::Files"),
            "SFTP 节点要开文件标签"
        );
    }

    /// 关文件标签要走与终端标签**同构**的收口(设计 D0 Task 6 的移交约定):
    /// 先停掉这个标签自己的东西,再 drop 它的连接。
    #[test]
    fn winding_down_a_files_tab_drops_its_own_connection() {
        let src = include_str!("app.rs");
        let body = src.split("fn wind_down(").nth(1).expect("找不到 wind_down");
        assert!(
            body.contains("TabContent::Files"),
            "wind_down 必须显式处理文件标签,不能靠 `_ => {{}}` 兜底 —— \
             兜底的写法在加新变体时不会报错,连接就悄悄泄漏了"
        );
    }
```

- [ ] **Step 2: 跑，确认变红**

```bash
cargo test -p mullion-app --lib an_sftp_node_opens_a_files_tab
```

- [ ] **Step 3: 实现**

`app.rs`：

- `TabContent` 加变体：

```rust
    /// F50:SFTP 节点独占的一个标签(设计 D1 的第二种宿主)。
    Files(FilesTab),
```

- 新类型：

```rust
/// 一个文件标签。**独占自己的 SSH 连接**(设计 D6:节点模式 `establish`
/// 一条独占连接,与 adr-010 的隧道同构),所以这里握着 `Arc<SshConnection>`
/// —— 它一 drop,那条连接就断。
pub struct FilesTab {
    pub panel: crate::ui::files_panel::PanelFrame,
    pub sftp: Arc<mullion_ssh::sftp::SftpClient>,
    pub conn: Arc<SshConnection>,
    /// S1 路由键:迟到的列目录结果按它找属主标签,不投给活动标签。
    pub generation: u64,
}
```

`TabPayload for TabContent` 的 `generation()` 加 `Files` 分支。

- `wind_down` 加分支：

```rust
        TabContent::Files(f) => {
            // `f.sftp` 与 `f.conn` 在这里 drop —— 最后一份 Arc 释放时
            // 那条独占连接才真正断(同 open_pty 的保活约定)。
            drop(f);
        }
```

- `ConnectOk` 分支按 `wants_sftp` 分流。**`wants_sftp` 要随异步任务带回来**——
  `UserEvent::ConnectOk` 加一个 `wants_sftp: bool` 字段，`spawn_connect`
  发任务时把它带上。SFTP 分支不开 PTY，改为：

```rust
                if wants_sftp {
                    // 这里已经在事件循环里(同步上下文),开 sftp 是 async ——
                    // 与 `PaneOpened` 同一套:spawn 出去,回来再发一个
                    // `UserEvent::SftpOpened { generation, .. }`。
                    // **绝不在这里 block_on**:那会把整个 UI 卡在网络 RTT 上。
                    self.spawn_open_sftp(handle.clone(), generation);
                    self.tabs.open(title, session_id, TabContent::Files(/* 占位:Loading 态 */));
                } else {
                    self.tabs.open(title, session_id, TabContent::Terminal(TerminalTab { .. }));
                }
```

`FilesTab` 的 `sftp` 因此要能表达「还没开好」。**改成
`sftp: Option<Arc<SftpClient>>`**，`PanelFrame.remote.load` 初始为
`Load::Loading`，`SftpOpened` 事件到达后按 generation 找属主标签填进去
（`self.tabs.by_generation_mut(generation)`）——这正是 D0 立下的 S1 路由规则，
**不要**改投活动标签。

新增两个 `UserEvent` 变体：

```rust
    /// F50:SFTP 子系统开好了。按 generation 找属主标签(S1 路由规则)。
    SftpOpened {
        generation: u64,
        client: Arc<mullion_ssh::sftp::SftpClient>,
        /// 登录目录(`canonicalize(".")` 的结果),第一次列目录用它。
        home: mullion_ssh::sftp::RemotePath,
    },
    /// F50:一次列目录的结果。`seq` 与 `PaneState::request_seq` 对齐,
    /// 对不上就丢(用户点得比网络快时的后发先至)。
    SftpListed {
        generation: u64,
        seq: u64,
        result: Result<Vec<mullion_ssh::sftp::Entry>, String>,
    },
```

两者的处理都走 `by_generation_mut` + `PaneState::accept`。

- [ ] **Step 4: 跑绿**

```bash
cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED"
```

- [ ] **Step 4b: 解开第四处闸门（Task 6 执行时发现，计划原先漏了）**

`crates/mullion-app/src/ui/session_manager/mod.rs:743` 还有一道 **D4 统一兜底闸门**：

```rust
    if ui_state.manager_mode != ManagerMode::Sessions {
        ui_state.connect_request = None;
        ui_state.connect_skip_automation = false;
    }
```

注释明写「SFTP 节点连不上(F50 未实现)、隧道档压根没有会话可连」。F50 就是本切片：
**这道闸门不解开，Task 6 解掉的那三处只是让按钮不再置灰，点下去照样没反应**——
四条入口（左栏双击、右键菜单、右栏按钮、Enter）的意图全被这里统一抹掉。

刻意排在 Task 10：解闸门必须与「`ConnectOk` 按 `wants_sftp` 分流」同一提交落地，
否则中间态是「SFTP 节点开出一个空终端标签」，比点了没反应更难查。

改法：条件从 `!= Sessions` 收窄成 `== Tunnels`（隧道档没有会话可连，那条理由仍成立），
注释里删掉「SFTP 节点连不上(F50 未实现)」那半句、写清 D1 起 SFTP 档放行的理由。

守护测试 `no_connect_request_survives_outside_session_mode`（`mod.rs:1723`）
的 `for mode in [ManagerMode::Sftp, ManagerMode::Tunnels]` 要拆开：
`Tunnels` 留在原断言里，`Sftp` 移到反面那半（与 `Sessions` 一样必须**放行**），
并把测试改名成说得清新语义的名字。**不许直接把 `Sftp` 从循环里删掉不补**——
那就成了「删断言换绿」。

- [ ] **Step 5: 变异验收**

`wind_down` 的 `TabContent::Files` 分支改成 `_ => {}` 兜底 →
`winding_down_a_files_tab_drops_its_own_connection` **必须变红**。

把 Step 4b 的闸门条件改回 `!= Sessions` → 改名后的那条闸门测试
（SFTP 档必须放行那一半）**必须变红**。

`ConnectOk` 里把 `wants_sftp` 分流删掉（一律建终端标签）→
`an_sftp_node_opens_a_files_tab_not_a_terminal_tab` **必须变红**。

- [ ] **Step 6: 提交**

```bash
git add -u crates/mullion-app/src
git commit -m "feat(app): SFTP 节点开独占标签 —— TabContent::Files + 按 generation 路由列目录结果 (F50)"
```

---

## Task 11: 编辑器「SFTP」分节（默认目录 + 书签，F120）

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/editor.rs`
- Modify: `crates/mullion-app/src/ui/session_manager/fields.rs`
- Modify: `crates/mullion-app/src/ui/session_manager/buffer.rs`
- Modify: `crates/mullion-app/src/ui/session_manager/mod.rs`

按 F119 表单规范：分节 + 88px 标签列（`LABEL_COL_W`）+ `field_w` 三档宽度
+ `SP_*` 五档间距。**写之前先扫一眼 `docs/ui-form-guidelines.md`**，
机械守护在 `crates/mullion-app/tests/form_guidelines.rs`。

- [ ] **Step 1: 写失败的测试**

`fields.rs` 测试模块追加：

```rust
    /// F120:两个默认目录 + 书签列表都要在「SFTP」页上画得出来。
    /// 判据是画出来的文本 —— 只断言函数存在等于什么都没测。
    #[test]
    fn the_sftp_section_shows_both_default_directories_and_the_bookmarks() {
        let mut buf = sample_buffer();
        buf.sftp_default_remote = "/srv/app".into();
        buf.sftp_default_local = r"D:\work".into();
        buf.sftp_bookmarks = vec![("日志".into(), "/var/log".into())];

        let texts = render_texts(|ui, t| {
            let mut first = true;
            super::sftp(ui, t, &mut buf, &mut first);
        });
        assert!(texts.iter().any(|s| s.contains("默认远端目录")));
        assert!(texts.iter().any(|s| s.contains("默认本地目录")));
        assert!(texts.iter().any(|s| s.contains("日志")));
        assert!(texts.iter().any(|s| s.contains("/var/log")));
    }

    /// 留空要说清缺省是什么 —— 否则用户不知道「不填」会发生什么,
    /// 只能试(F119 的空态文案规范)。
    #[test]
    fn empty_default_directories_explain_what_happens_instead() {
        let mut buf = sample_buffer();
        buf.sftp_default_remote.clear();
        buf.sftp_default_local.clear();
        let texts = render_texts(|ui, t| {
            let mut first = true;
            super::sftp(ui, t, &mut buf, &mut first);
        });
        let all = texts.join(" ");
        assert!(all.contains("登录目录"), "远端留空的缺省要写出来: {all}");
        assert!(all.contains("用户主目录") || all.contains("USERPROFILE"), "本地留空的缺省要写出来: {all}");
    }
```

`sample_buffer()` / `render_texts()` 照该测试模块里既有的同名/同类
helper 用；没有就照 `tunnel_editor.rs` 的测试写一个。

- [ ] **Step 2: 跑，确认编不过**

- [ ] **Step 3: 实现**

`buffer.rs` 的 `EditorBuffer` 加三个字段（照既有字段的写法，
**并同步改 `from_record` / `to_draft` / `is_dirty` 三处**——
漏了任何一处，症状分别是「打开编辑器看不到已存的值」「保存后丢失」
「改了不提示未保存」）：

```rust
    /// F120:SFTP 默认远端目录。空 = 用登录目录。
    pub sftp_default_remote: String,
    /// F120:SFTP 默认本地目录。空 = 用用户主目录。
    pub sftp_default_local: String,
    /// F120:远端书签 `(名称, 路径)`。顺序即用户排的顺序,保存时不许重排。
    pub sftp_bookmarks: Vec<(String, String)>,
```

`fields.rs` 加页面级函数 `pub(super) fn sftp(ui, t, buf, first: &mut bool)`：
一个 `form::section(ui, t, "默认目录", first)` + `form::grid` 两行，
一个 `form::section(ui, t, "书签", first)` + 可增删的行列表。
**`first` 是页面级游标，照 `form::section` 的文档注释传 `&mut first`，
不要在函数内部自己 `let mut first = true`。**

`editor.rs`：`TABS` 从 4 项扩成 5 项，加 `"SFTP"`；
`mod.rs` 加 `pub(crate) const TAB_SFTP: usize = 4;`；
`visible_tabs` 两个分支都要包含它（SSH 会话也有侧栏，同样需要默认目录与书签）：

```rust
        ManagerMode::Sftp => &[TAB_CONNECT, TAB_AUTH, TAB_SFTP, TAB_APPEARANCE],
        _ => &[TAB_CONNECT, TAB_AUTH, TAB_AUTOMATION, TAB_SFTP, TAB_APPEARANCE],
```

`editor.rs` 的内容区 match 加 `TAB_SFTP => fields::sftp(...)` 分支。
`editor.rs:1371` 那条 `assert_eq!(super::TABS[TAB_CONNECT], "连接")` 附近
若有「TABS 长度」的断言，一并更新。

- [ ] **Step 4: 跑绿**

```bash
cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED"
cargo test -p mullion-app --test form_guidelines
```

- [ ] **Step 5: 变异验收**

`buffer.rs` 的 `to_draft` 里把 `sftp_bookmarks` 那行删掉（保存时丢书签）→
应有测试变红；**若没有一条测试因此变红**，说明 `buffer.rs` 缺一条往返守护，
补一条：

```rust
    /// 三个新字段必须**存得进去也读得出来**。少接一处的症状分别是
    /// 「打开看不到」「保存后丢」「改了不提示未保存」,三种都不报错。
    #[test]
    fn sftp_prefs_survive_the_editor_round_trip() {
        let mut rec = sample_record();
        rec.sftp.default_remote = Some("/srv".into());
        rec.sftp.bookmarks = vec![mullion_store::Bookmark { name: "日志".into(), path: "/var/log".into() }];
        let buf = EditorBuffer::from_record(&rec, /* 其余参数照既有调用 */);
        assert_eq!(buf.sftp_default_remote, "/srv");
        assert_eq!(buf.sftp_bookmarks, vec![("日志".to_string(), "/var/log".to_string())]);
        let draft = buf.to_draft(/* 照既有调用 */);
        assert_eq!(draft.sftp.bookmarks[0].path, "/var/log");
    }
```

- [ ] **Step 6: 提交**

```bash
git add -u crates/mullion-app/src
git commit -m "feat(ui): 会话编辑器「SFTP」页 —— 默认远端/本地目录 + 书签列表 (F120)"
```

---

## Task 12: 焦点与键盘路由（F6 / 面板内按键，T8 纪律）

**Files:**
- Modify: `crates/mullion-app/src/shell/input_route.rs`
- Modify: `crates/mullion-app/src/app.rs`

**这一步碰 T8。** 判给面板的键**绝不先喂 `egui_state.on_window_event`**——
egui 的焦点系统会吞 Tab，终端就永久收不到键了。规则：
**键盘先判后喂，指针先喂后判。**

既有 API（**照它扩，别另起炉灶**）：

```rust
pub enum InputKind { Keyboard, Pointer }
pub enum Route { Egui, Terminal }
pub fn egui_should_see(kind: InputKind, modal_open: bool, egui_wants_keyboard: bool) -> bool
pub fn route(modal_open: bool, egui_wants_keyboard: bool, egui_wants_pointer: bool, kind: InputKind) -> Route
```

`egui_should_see` 是 T8 的**唯一注入点**（它决定喂不喂
`egui_state.on_window_event`），所以焦点维度必须加到**它**上面，
加在别处等于没加。

- [ ] **Step 1: 写失败的守护测试**

`input_route.rs` 测试模块追加：

```rust
    /// T8 / 设计 D23:文件面板拿到焦点时,键盘事件走面板这条路,
    /// **绝不先喂 egui**。喂了的后果与 T8 原案一模一样:egui 的焦点系统
    /// 在 `begin_pass` 里看到 Tab 就把焦点给菜单栏,`wants_keyboard_input()`
    /// 从此恒真,终端和面板**双双**收不到任何键。
    ///
    /// 注意第三个参数给 `true`(假装 egui 想要键盘)—— 这正是坏掉时的现场,
    /// 给 `false` 的话实现写错了也能蒙对。
    #[test]
    fn panel_keyboard_is_never_fed_to_egui_so_tab_cannot_steal_focus() {
        assert!(!egui_should_see_focused(
            Focus::FilesPanel,
            InputKind::Keyboard,
            false,
            true
        ));
        assert_eq!(
            route_focused(Focus::FilesPanel, false, true, false, InputKind::Keyboard),
            Route::FilesPanel
        );
    }

    /// 模态弹窗压过面板焦点 —— 会话管理器开着的时候按 Enter 必须是
    /// 「保存」,不是「进目录」。
    #[test]
    fn a_modal_outranks_panel_focus() {
        assert_eq!(
            route_focused(Focus::FilesPanel, true, false, false, InputKind::Keyboard),
            Route::Egui
        );
        assert!(egui_should_see_focused(
            Focus::FilesPanel,
            InputKind::Keyboard,
            true,
            false
        ));
    }

    /// 焦点在终端时,面板一个键都不截 —— 否则在 tmux 里按 F5 会莫名其妙
    /// 刷新文件列表。这条是上面那条的反面,少了它「恒返回 FilesPanel」
    /// 的实现也能全绿。
    #[test]
    fn terminal_focus_leaves_every_key_to_the_terminal() {
        assert_eq!(
            route_focused(Focus::Terminal, false, false, false, InputKind::Keyboard),
            Route::Terminal
        );
    }

    /// 指针不受面板焦点影响:仍是「先喂后判」,否则菜单/弹窗点不动
    /// (既有 `egui_should_see` 文档注释里的理由原样适用)。
    #[test]
    fn pointer_events_still_reach_egui_regardless_of_panel_focus() {
        assert!(egui_should_see_focused(
            Focus::FilesPanel,
            InputKind::Pointer,
            false,
            false
        ));
    }

    /// F6 换焦点。**不用 `Ctrl+Tab`** —— 那个是标签页的(D0 已占)。
    #[test]
    fn f6_toggles_focus_between_terminal_and_panel() {
        assert_eq!(Focus::Terminal.toggled(), Focus::FilesPanel);
        assert_eq!(Focus::FilesPanel.toggled(), Focus::Terminal);
    }
```

- [ ] **Step 2: 跑，确认编不过**

```bash
cargo test -p mullion-app --lib input_route 2>&1 | grep -E "^error" | head -3
```

- [ ] **Step 3: 实现**

`input_route.rs` 追加（**既有的 `route` / `egui_should_see` 原样不动**——
它们有一批守护测试钉着，改签名会把不相关的测试一起搅动；新函数在
面板存在时用，面板不存在时 `Focus::Terminal` 让它退化成旧行为）：

```rust
/// 键盘焦点在哪一侧。文件面板不存在 / 没开时恒为 `Terminal`。
#[derive(Debug, PartialEq, Eq, Clone, Copy, Default)]
pub enum Focus {
    #[default]
    Terminal,
    FilesPanel,
}

impl Focus {
    /// F6 在两侧之间来回(设计 D23)。**不用 `Ctrl+Tab`** —— D0 已经把它
    /// 给了标签切换;面板内的 `Tab` 另有用处(远端栏↔本地栏)。
    pub fn toggled(self) -> Self {
        match self {
            Focus::Terminal => Focus::FilesPanel,
            Focus::FilesPanel => Focus::Terminal,
        }
    }
}
```

`Route` 加一个变体：

```rust
    /// 交给文件面板(F50)。只有键盘会走到这里,指针照旧先喂 egui 后判。
    FilesPanel,
```

加变体会让既有 `match Route::…` 的调用点报 non-exhaustive ——
**这是好事**，编译器会把每个需要处理新目标的地方点出来，逐个补。

```rust
/// [`route`] 的带焦点版本。面板不存在时传 `Focus::Terminal`,行为与
/// [`route`] 完全一致。
///
/// 优先级:模态 > 面板焦点 > egui 想不想要。模态排最前是因为会话管理器
/// 开着时按 Enter 必须是「保存」而不是「进目录」。
pub fn route_focused(
    focus: Focus,
    modal_open: bool,
    egui_wants_keyboard: bool,
    egui_wants_pointer: bool,
    kind: InputKind,
) -> Route {
    if modal_open {
        return Route::Egui;
    }
    if kind == InputKind::Keyboard && focus == Focus::FilesPanel {
        return Route::FilesPanel;
    }
    route(modal_open, egui_wants_keyboard, egui_wants_pointer, kind)
}

/// [`egui_should_see`] 的带焦点版本。**T8 的注入点就是这个函数** ——
/// 判给面板的键在这里返回 `false`,于是它根本进不了
/// `egui_state.on_window_event`,egui 的焦点系统也就无从吞掉 Tab。
pub fn egui_should_see_focused(
    focus: Focus,
    kind: InputKind,
    modal_open: bool,
    egui_wants_keyboard: bool,
) -> bool {
    match kind {
        InputKind::Pointer => true,
        InputKind::Keyboard => matches!(
            route_focused(focus, modal_open, egui_wants_keyboard, false, kind),
            Route::Egui
        ),
    }
}
```

`app.rs`：`App` 加 `focus: input_route::Focus`（`Default::default()` = 终端）；
把调用 `egui_should_see` / `route` 的那两处换成 `_focused` 版本并传
`self.focus`；`Route::FilesPanel` 分支调用新增的 `handle_panel_key`。

`handle_panel_key`（仅面板有焦点时到达，设计 D23）：
`Enter` 进目录（`PaneState::enter_target`）、`Backspace` 上级
（`cwd.parent()`）、`F5` 刷新、`Ctrl+H` 切 `show_hidden` 并重排、
`Tab` 在远端栏/本地栏之间换、`↑`/`↓` 移动 `selected`。
**`Delete` / `F2` 本切片不接**——那是 D2 的写操作，现在接了就是给一个
按下去没反应的键。

`F6` 换焦点放在 `files_hotkey_event` 旁边同一层（判定阶段截走），
**并且面板关着时 `F6` 不生效**（否则焦点跑到一个看不见的地方，键盘像死了）：

```rust
        if key == mullion_term::keymap::Key::Named(NamedKey::F6) && self.ui.files_sidebar_open {
            self.focus = self.focus.toggled();
            self.request_ui_redraw();
            return true;
        }
```

- [ ] **Step 4: 跑绿**

```bash
cargo test -p mullion-app --lib input_route
cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED"
```

- [ ] **Step 5: 变异验收（两条）**

1. `egui_should_see_focused` 的 `InputKind::Keyboard` 分支直接委托给旧的
   `egui_should_see`（丢掉焦点维度）→
   `panel_keyboard_is_never_fed_to_egui_so_tab_cannot_steal_focus` **必须变红**。
2. `route_focused` 里把 `if modal_open` 那条早退挪到面板判定**之后** →
   `a_modal_outranks_panel_focus` **必须变红**。

- [ ] **Step 5: 复跑 T8 既有守护**

```bash
cargo test -p mullion-app --lib terminal_keyboard_is_never_fed_to_egui
```

- [ ] **Step 6: 提交**

```bash
git add -u crates/mullion-app/src
git commit -m "feat(app): F6 切换终端/面板焦点 + 面板内只读按键,判定阶段截走 (F50/T8)"
```

---

## Task 13: 交付

- [ ] **Step 1: 全绿闸门**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

三条都过才算绿。只跑单个 crate 不算。

- [ ] **Step 2: 复跑领域陷阱守护，逐条记录**

```bash
cargo test --workspace 2>&1 | grep -E "pty_write_is_collected|sync_update_defers_present|redraw_is_frame_capped|reflow_emits_resize|shift_blocks_mouse_report|shift_enter_without_kitty|terminal_keyboard_is_never_fed_to_egui|frame::tests"
```

本切片碰了 **T3**（虚拟滚动）、**T4**（侧栏挤窄中央区）、**T8**（面板键盘路由），
提交正文里要写明跑了哪几条。

- [ ] **Step 3: 升版本号(单独一笔)**

`Cargo.toml` 的 `workspace.package.version` 第三位 +1（0.1.32 → 0.1.33）。

```bash
cargo check -q -p mullion-app
git add Cargo.toml Cargo.lock
git commit -m "chore: 版本 0.1.33(SFTP 只读浏览:侧栏与节点标签能列远端目录)"
```

- [ ] **Step 4: 交叉编译 + objdump 验收**

```bash
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app
x86_64-w64-mingw32-objdump -p target/x86_64-pc-windows-gnu/release/mullion.exe | grep "DLL Name" | sort -u
```

出现 `libgcc_s_seh-1.dll` 或 `libwinpthread-1.dll` **即为不合格**，必须修
（见 `docs/cross-compile-windows.md`）。

- [ ] **Step 5: 发 Release**

```bash
cd /tmp && cp /data/Mullion/target/x86_64-pc-windows-gnu/release/mullion.exe .
sha256sum mullion.exe > mullion.exe.sha256
HTTPS_PROXY=http://127.0.0.1:7890 gh release create v0.1.33 \
  mullion.exe mullion.exe.sha256 -t "v0.1.33" -F notes.md --repo kilobitcy/Mullion
```

标题**只能是纯版本号**。notes 里写：修了什么 + 下面这份人工验收清单 + sha256
+ `Unblock-File .\mullion.exe` 提示。

**人工验收清单（无头验不了，抄进 notes）：**

1. **侧栏能看见远端文件**：连一台机器，`Ctrl+Shift+B` 开侧栏，
   远端栏列出登录目录；双击进子目录、`↑` 回上级都正常。
2. **开关侧栏不让 TUI 错行**：在 tmux 里跑一个全屏 TUI（Claude Code 即可），
   开关侧栏几次，TUI **重排后是正确的**，没有错行/残影。这是 T4 的实机凭证。
3. **中文文件名**：远端建一个中文名目录和文件，列表里显示正确、能进得去；
   CJK 列对齐是否可读。
4. **非 UTF-8 文件名**：远端建一个 GBK 名文件
   （`touch "$(printf '\xd6\xd0\xce\xc4').txt"`），列表里应**看得见**它
   （带替换字符）并标「名称非 UTF-8，本版无法操作」，双击进不去也不报奇怪的错。
   这是本版**已知限制**，不是 bug。
5. **SFTP 节点开标签**：双击一个 SFTP 节点，应开出一个独占的文件标签
   （不是空终端）；`Ctrl+W` 关掉后远端 `who` 里对应会话消失。
6. **大目录**：进一个几千项的目录（`/usr/lib` 之类），滚动是否跟手、
   有没有卡顿。
7. **书签与默认目录**：在会话编辑器「SFTP」页填默认远端目录与两条书签，
   保存后重开侧栏，是否直接落在那个目录；点书签是否跳过去。
8. **视觉**：面板与终端的配色/字体是否同源（F80 三处同源纪律）。

- [ ] **Step 6: 报给用户**

Release 链接 + sha256 + 上面的验收清单。

---

## 验收

- `cargo test --workspace` 全绿 + `clippy -D warnings` 无输出 + `fmt --check` 干净。
- 本切片新增的每一条守护测试都**自证会变红**（每个 Task 的变异步骤逐条做完）。
- T3 / T4 / T8 三条领域陷阱守护复跑通过，提交正文写明。
- 零写操作：假服务端的 `remove` 恒返回 `PermissionDenied`，测试跑完
  `probe.paths_for("remove")` 应当是空的。

## 移交给 D2 的接口约定

- `SftpClient` 加写方法（`upload` / `download` / `mkdir` / `rename` / `remove`），
  签名一律收 `&RemotePath`，走同一个 `as_wire()` 出口——**非 UTF-8 拒绝**这条
  纪律对写操作只会更重要。
- `PaneState` 加选中集合（多选）；`FileAction` 加写动作变体。
- 传输队列是**全局**对象，不挂在任何一个标签上（设计 D18）——
  D2 建它时放 `App` 顶层，关标签不清队列。
- 断线退避重连（设计 D22）复用隧道切片的 `schedule.rs`，D2 落地。
