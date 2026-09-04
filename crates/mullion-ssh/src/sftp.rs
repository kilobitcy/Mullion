//! SFTP v3 客户端(F50)。协议实现来自 `russh-sftp 2.4.0`。
//!
//! **本模块不向外暴露任何 `russh_sftp` 类型。** 架构不变量是 `app → ssh`,
//! 让第三方的 `FileType`/`Metadata` 漏进 `mullion-app` 等于把依赖方向变成
//! `app → russh_sftp`,以后换协议库要改的就不止这一个 crate。

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
///
/// `Ord` 是**字节序**,只配 `BTreeMap`/去重这类用途。列表排序另有一套
/// (按显示名、不分大小写,见 `mullion_app::files::sort`)——直接 `.sort()`
/// 会让 `Zeta` 排到 `alpha` 前面,用户眼里就是「排序坏了」。
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

    /// 这个路径发不发得出去 —— 与 [`RemotePath::as_wire`] 同一条判据。
    /// 界面拿它决定「这一行的操作入口要不要禁用」。
    pub fn is_operable(&self) -> bool {
        self.as_wire().is_ok()
    }

    /// 唯一的 wire 出口。发不出去的两类路径在这里被挡住,调用方拿不到
    /// wire 串也就发不出请求。
    ///
    /// 1. **字节本身不是 UTF-8**(用户手输、或将来换了能透传字节的协议库)。
    /// 2. **串里含 `U+FFFD`**。这一类是从线上收回来的:`russh-sftp 2.4.0`
    ///    的 `buf.rs:25` 在**收包方向也**过 `from_utf8_lossy`,远端一个 GBK
    ///    文件名到我们手里时早已是合法 UTF-8、只是带着替换字符。**原始字节
    ///    已经丢了**,拿这串替换字符去请求必然打不中那个文件 —— 与其发一条
    ///    让用户看不懂的 `NoSuchFile`,不如在这里就说清「本版动不了它」
    ///    (设计 D16 的 2026-08-12 修订 + 实施期补丁)。
    ///
    /// 代价:名字里**真的**写了个 `U+FFFD` 的文件本版也只能看不能动。
    /// 这种文件极罕见,而误伤的后果只是「不可操作」,反过来放行的后果是
    /// 「操作打空、原因说不清」——两害相权。
    pub fn as_wire(&self) -> Result<String, NonUtf8Path> {
        let s = String::from_utf8(self.0.clone()).map_err(|_| NonUtf8Path)?;
        if s.contains('\u{fffd}') {
            return Err(NonUtf8Path);
        }
        Ok(s)
    }

    /// 拼一段名字。分隔符恒为 `/`(SFTP 线上永远是 POSIX 路径,
    /// 哪怕客户端跑在 Windows 上)。
    ///
    /// `name` 是**一段名字**,不是路径:`readdir` 给的文件名本就不含 `/`。
    /// 传含 `/` 的进来只会拼出 `/data//abs` 这种东西 —— 这里不做归一化,
    /// 因为悄悄「修好」一个本不该出现的输入,只会把上游的 bug 藏起来。
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
    /// F142 起界面显示名字,靠的是列完目录后单独 exec 一次 `getent`
    /// (`mullion_app::files::owners`),不是这里能给出的信息。
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

/// [`SftpClient::write_if_unchanged`] 的结果(F53)。
///
/// 两条分支都带回**远端当前的** `(mtime, size)`:调用方无论走哪条都要拿它
/// 刷快照 —— 冲突之后不刷的话,下一次保存还会撞上同一个冲突,处置框永远关不掉。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    /// 写下去了。戳是写完之后回读的。
    Written { mtime: u32, size: u64 },
    /// 远端变过,**一个字节都没写**。戳是远端当前的。
    Conflict { mtime: u32, size: u64 },
}

#[derive(Debug)]
pub enum SftpError {
    /// 开 channel / 请求 subsystem 失败。
    Subsystem,
    /// 协议层报错(含 NoSuchFile / PermissionDenied 等)。
    Protocol(String),
    /// 路径含非 UTF-8 字节 —— **请求根本没发出去**(设计 D16 修订)。
    NonUtf8Name,
    /// F53:文件超过了调用方给的上限,**读到一半就停了**(见 `read_all`)。
    /// 单独一个变体而不是塞进 `Protocol`:界面要据此给出「用下载功能」这条
    /// 出路,而不是把一句协议错误原样甩给用户。
    TooLarge(u64),
}

impl std::fmt::Display for SftpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SftpError::Subsystem => write!(f, "远端没有开启 SFTP 子系统,或连接已断开"),
            SftpError::Protocol(m) => write!(f, "{m}"),
            SftpError::NonUtf8Name => write!(f, "{}", NonUtf8Path),
            SftpError::TooLarge(limit) => write!(
                f,
                "文件超过 {} MB,编辑器打不开;用「下载到本地」取回来再处理",
                limit / (1024 * 1024)
            ),
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
            // 链接目标要多一个 RTT,只对链接发。名字发不出去的就跳过 ——
            // 它的完整路径本来就到不了网络层(D16 修订)。
            let link_target = if kind == EntryKind::Symlink && name.is_operable() {
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

    /// 新建目录。父目录不存在会失败(**不做 `mkdir -p`** —— 界面上用户是在
    /// 某个具体目录里按的「新建文件夹」,父目录必然存在;悄悄创建一串中间
    /// 目录只会把打错的路径变成一堆垃圾目录)。
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
    /// **不跟随**(设计 D17)—— 搞错了就是把远端整个目标目录删了。
    /// 目录要走 [`SftpClient::remove_dir`]。
    pub async fn remove_file(&self, path: &RemotePath) -> Result<(), SftpError> {
        let wire = path.as_wire()?;
        self.inner
            .remove_file(wire)
            .await
            .map_err(|e| SftpError::Protocol(e.to_string()))
    }

    /// 删一个**空目录**。非空会被服务端拒 —— 递归删除见
    /// [`crate::remove_tree::remove_tree`]。
    pub async fn remove_dir(&self, path: &RemotePath) -> Result<(), SftpError> {
        let wire = path.as_wire()?;
        self.inner
            .remove_dir(wire)
            .await
            .map_err(|e| SftpError::Protocol(e.to_string()))
    }

    /// 改权限位(设计 D21)。
    ///
    /// **只送 permissions 一个字段**:SFTP v3 的 attrs 带 flags 位图,没设的
    /// 字段不会被写过去。顺手把 uid/gid/mtime 一起送出去等于拿本地的猜测
    /// 覆盖远端的真值。
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
    /// readdir 对齐 —— 跟随了的话,递归删除会把「指向目录的链接」当目录
    /// 走进去,那正是 D17 要挡的事故。
    pub async fn stat(&self, path: &RemotePath) -> Result<Entry, SftpError> {
        let wire = path.as_wire()?;
        let md = self
            .inner
            .symlink_metadata(wire)
            .await
            .map_err(|e| SftpError::Protocol(e.to_string()))?;
        // 判定顺序与 `list_dir` 一致:先 symlink —— 一些 sshd 会把 symlink
        // 和 dir 两个类型位一起报回来,先判 dir 就会把链接误认成目录。
        let kind = if md.is_symlink() {
            EntryKind::Symlink
        } else if md.is_dir() {
            EntryKind::Dir
        } else if md.is_regular() {
            EntryKind::File
        } else {
            EntryKind::Other
        };
        Ok(Entry {
            // 名字只取最后一段,与 `Entry::name`「只是名字,不含目录部分」
            // 的语义一致。
            name: RemotePath::from_bytes(last_segment(path.as_bytes()).to_vec()),
            kind,
            size: md.size.unwrap_or(0),
            mtime: md.mtime.unwrap_or(0),
            mode: md.permissions.unwrap_or(0) & 0o7777,
            uid: md.uid.unwrap_or(0),
            gid: md.gid.unwrap_or(0),
            link_target: None,
        })
    }

    /// 打开一个远端文件**读**(F52)。
    ///
    /// 返回的 [`RemoteFile`] 分块读,进度由调用方按 `read_chunk` 的返回值
    /// 累计 —— 传输层要在每块之后更新界面,一次性 `read_to_end` 会让 2GB
    /// 的文件在进度条上一动不动,还把整个文件读进内存。
    pub async fn open_read(&self, path: &RemotePath) -> Result<RemoteFile, SftpError> {
        let wire = path.as_wire()?;
        let file = self
            .inner
            .open(wire)
            .await
            .map_err(|e| SftpError::Protocol(e.to_string()))?;
        Ok(RemoteFile { file })
    }

    /// 打开一个远端文件**写**(F52)。`truncate` 为真时截断已有内容。
    ///
    /// 刻意**不带** `EXCLUDE`:能不能覆盖由上层的冲突策略决定(设计 D19),
    /// 协议层再挡一道的话,「用户明确选了覆盖」也会失败。
    pub async fn open_write(
        &self,
        path: &RemotePath,
        truncate: bool,
    ) -> Result<RemoteFile, SftpError> {
        let wire = path.as_wire()?;
        let mut flags =
            russh_sftp::protocol::OpenFlags::WRITE | russh_sftp::protocol::OpenFlags::CREATE;
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

    /// 新建一个**空文件**。已存在就失败(F219)。
    ///
    /// flags 带 `EXCLUDE` —— 与 `open_write` 刻意不带的理由正好相反:
    /// 传输通路要不要覆盖由上层的冲突策略决定(设计 D19),而「新建」撞上
    /// 已存在必须当场失败。这里没带 `TRUNCATE`,所以不带 `EXCLUDE` 并不会
    /// 把已有内容清空 —— 真实后果是 open **静默成功**,拿到的句柄指向那份
    /// **既存文件**:用户在一个已有 `config.yaml` 的目录里手滑建了个同名
    /// 文件,界面会以为「新建成功」并把光标落上去,用户点开编辑、改的其实
    /// 是别人的文件,保存时才会覆盖对方原有内容 —— 同样不可逆,但触发点
    /// 在后续的编辑/保存,不是这次 open 本身。
    pub async fn create_file(&self, path: &RemotePath) -> Result<(), SftpError> {
        let wire = path.as_wire()?;
        let flags = russh_sftp::protocol::OpenFlags::WRITE
            | russh_sftp::protocol::OpenFlags::CREATE
            | russh_sftp::protocol::OpenFlags::EXCLUDE;
        let file = self
            .inner
            .open_with_flags(wire, flags)
            .await
            .map_err(|e| SftpError::Protocol(e.to_string()))?;
        // finish() 里做 flush + close;句柄不收尾的话服务端那边会一直
        // 挂着一个打开的文件(见 `RemoteFile` 文档)。
        RemoteFile { file }.finish().await
    }

    /// 一次把整个远端文件读进内存(F53 编辑用)。**带上限**。
    ///
    /// 编辑通路的两条大小闸门(内置 1 MB / 外部 64 MB)最终都落在这里。
    /// 上限**必须边读边判**,不能读完再看长度 —— 那样一个 8 GB 的 core dump
    /// 会在「拒绝」之前先把进程 OOM 掉,而用户看到的是程序直接消失。
    ///
    /// 与传输通路的分块循环刻意分开:那条要报进度、要能取消、要落盘;
    /// 这条只服务「打开来改一改」,全量在内存里反而让调用方简单一个数量级。
    ///
    /// # F214:往返数才是成本
    ///
    /// SFTP 的 READ 在 `russh_sftp` 里是**串行**的(没有 `max_concurrent_reads`,
    /// 对照 WRITE 有 `max_concurrent_writes` —— 这正是设计 D8 说「下载受串行
    /// READ 拖累」的由来)。本项目的主场景是高延迟代理链路,于是「打开一个
    /// 文件要等多久」几乎完全等于**发了几次 READ**,与文件大小基本无关。
    /// 两处各省一大截:
    ///
    /// - **缓冲区 64 KiB → 256 KiB**。`File::poll_read`
    ///   (`russh-sftp-2.4.0/src/client/fs/file.rs:179`)取
    ///   `min(buf.remaining(), max_read_len)`,而 `max_read_len` 默认是
    ///   `max_packet_len - 9` = 262135(OpenSSH 通过 `limits@openssh.com` 报
    ///   255 KiB)。缓冲区给成 64 KiB 等于自愿把每次往返的收获砍掉四分之三 ——
    ///   一个 1 MB 的文件要 16 次串行 READ。
    /// - **`expect` 省掉「只为问一句 EOF」的那次空往返**。不给它,循环只能一直
    ///   读到服务端回 `EOF` 才知道到头了,而那次什么都没读到的 READ 在慢链路上
    ///   和一次真读一样贵。几十 KB 的配置文件因此从 2 次 READ 降到 1 次。
    ///
    /// `expect` 是调用方从目录列表里已经知道的大小,**可以过期**。所以只在读到
    /// 的字节数与它**严丝合缝**时才提前收手:列目录之后文件被改大的话,
    /// `out.len()` 会停在某次读满的位置、对不上 `expect`,于是照常读到 EOF。
    /// 判据写成 `>=` 就会在「文件变大了、而第一次读恰好读满」时静默截断 ——
    /// 用户拿到的是一份「打开成功」的残文件,存回去就把尾巴削掉了。
    pub async fn read_all(
        &self,
        path: &RemotePath,
        limit: u64,
        expect: Option<u64>,
    ) -> Result<(Vec<u8>, ReadTiming), SftpError> {
        let t_open = std::time::Instant::now();
        let mut file = self.open_read(path).await?;
        let mut timing = ReadTiming {
            open_us: t_open.elapsed().as_micros() as u64,
            ..Default::default()
        };
        let mut out = Vec::new();
        let mut buf = vec![0u8; 256 * 1024];
        let t_read = std::time::Instant::now();
        loop {
            let n = file.read_chunk(&mut buf).await?;
            timing.reads += 1;
            timing.read_us = t_read.elapsed().as_micros() as u64;
            if n == 0 {
                return Ok((out, timing));
            }
            if out.len() as u64 + n as u64 > limit {
                return Err(SftpError::TooLarge(limit));
            }
            out.extend_from_slice(&buf[..n]);
            if n < buf.len() && expect == Some(out.len() as u64) {
                return Ok((out, timing));
            }
        }
    }

    /// 覆盖写回(F53)。**TRUNC 直接写目标**,不走临时文件 + rename。
    ///
    /// 理由同设计 D19:rename 会换掉 inode,原文件的属主 / 权限 / ACL / 硬链接
    /// 全部丢失 —— 对 `/etc/nginx/nginx.conf` 这类被编辑的文件就是实打实的破坏。
    /// 代价是「写到一半断线会留下截断文件」,由上层的 `.mullion.bak` 兜(D3-7)。
    pub async fn write_all_truncate(
        &self,
        path: &RemotePath,
        bytes: &[u8],
    ) -> Result<(), SftpError> {
        let mut file = self.open_write(path, true).await?;
        for chunk in bytes.chunks(64 * 1024) {
            file.write_chunk(chunk).await?;
        }
        // 空文件也要走到这里:`open_write` 带 TRUNC 已经把内容清掉了,
        // 但不 `finish()` 就没人等那条 close 的应答(见 `RemoteFile` 文档)。
        file.finish().await
    }

    /// 先比对、再覆盖(F53/设计 D3-8)。**远端在我们编辑期间变过就一个字节都不写。**
    ///
    /// `expected` 是打开时记下的 `(mtime, size)`。判断放在这一层而不是调用方,
    /// 是因为「比对」和「写」之间不能插进别的 await 点,而调用方有三条路径
    /// (本地存盘轮询 / 内置编辑器保存 / 冲突框选了覆盖)会用到它 ——
    /// 判据散成三份,迟早只剩一份是对的。
    ///
    /// `backup` 给的话,**只在确认没冲突、且真要写之前**落一份(D3-7)。
    /// 放在这里同理:冲突时提前写出去的备份是纯垃圾文件。
    pub async fn write_if_unchanged(
        &self,
        path: &RemotePath,
        bytes: &[u8],
        expected: (u32, u64),
        backup: Option<(&RemotePath, &[u8])>,
    ) -> Result<WriteOutcome, SftpError> {
        let before = self.stat(path).await?;
        if (before.mtime, before.size) != expected {
            return Ok(WriteOutcome::Conflict {
                mtime: before.mtime,
                size: before.size,
            });
        }
        if let Some((bak_path, bak_bytes)) = backup {
            self.write_all_truncate(bak_path, bak_bytes).await?;
        }
        self.write_all_truncate(path, bytes).await?;
        // 回读新戳:下一次比对要拿它当基准。用本地算出来的长度顶替是不行的,
        // mtime 只有服务端知道,猜一个的话下一次保存必然误判成冲突。
        let after = self.stat(path).await?;
        Ok(WriteOutcome::Written {
            mtime: after.mtime,
            size: after.size,
        })
    }

    /// 目标在不在。冲突探测用。
    ///
    /// 与 [`SftpClient::stat`] 的区别:那条把「不存在」和「没权限」都变成
    /// `Err`,而冲突判断必须把两者分开 —— 分不开就只能一律当冲突,
    /// 每传一个文件都弹一次窗。
    pub async fn exists(&self, path: &RemotePath) -> Result<bool, SftpError> {
        let wire = path.as_wire()?;
        self.inner
            .try_exists(wire)
            .await
            .map_err(|e| SftpError::Protocol(e.to_string()))
    }
}

/// 一个打开着的远端文件(F52)。分块读 / 分块写。
///
/// **必须 `finish()` 收尾**:`russh_sftp` 的 `File` 有 Drop 兜底
/// (`close_nowait`),但那条关闭请求没人等应答 —— 上传完立刻去 rename
/// 会撞上「文件还开着」,而失败信息只会说「改名失败」,查不到根上。
/// 一次 [`SftpClient::read_all`] 的分段耗时(F214)。
///
/// 存在的理由是「打开一个远端文件要好几秒」这类实报**在本机永远复现不了**:
/// 慢的是往返,而无头容器里往返是零。把段落时间和**往返次数**一起报出来,
/// 用户在真机上跑一次就能指认是哪一段 —— 而不是我们隔空猜。
///
/// `reads` 是这里面最要紧的那个数:READ 串行,次数即往返数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReadTiming {
    /// OPEN 那一次往返。
    pub open_us: u64,
    /// 全部 READ 合计。
    pub read_us: u64,
    /// 发出去的 READ 次数。
    pub reads: u32,
}

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
    /// 这正是设计 D8 说「下载受串行 READ 拖累」的由来。
    pub async fn write_chunk(&mut self, buf: &[u8]) -> Result<(), SftpError> {
        use tokio::io::AsyncWriteExt;
        self.file
            .write_all(buf)
            .await
            .map_err(|e| SftpError::Protocol(e.to_string()))
    }

    /// 冲干净并关闭,**等应答**。理由见结构体文档。
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

/// `/a/b/c` → `c`。根本身(`/`)与不含 `/` 的相对名原样返回。
fn last_segment(path: &[u8]) -> &[u8] {
    match path.iter().rposition(|b| *b == b'/') {
        Some(ix) if ix + 1 < path.len() => &path[ix + 1..],
        _ => path,
    }
}

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
        assert!(!p.is_operable());
    }

    /// 实施期补丁:从线上收回来的名字**永远是合法 UTF-8**
    /// (`russh-sftp 2.4.0` 收包时就 lossy 过了),非 UTF-8 的原始字节
    /// 只留下一个 `U+FFFD`。只认 `is_utf8()` 的话这类条目会被当成好路径
    /// 发出去,换回一条用户看不懂的 `NoSuchFile`。
    #[test]
    fn a_lossy_name_from_the_wire_is_utf8_but_still_never_reaches_the_wire() {
        let p = RemotePath::from_bytes("\u{fffd}\u{fffd}.txt".as_bytes().to_vec());
        assert!(p.is_utf8(), "lossy 串本身是合法 UTF-8 —— 这正是坑所在");
        assert!(p.as_wire().is_err(), "含替换字符的路径不得给出 wire 串");
        assert!(!p.is_operable());
    }

    /// 反面:正常的中文路径**必须**照发不误。上面那条判据一旦写宽
    /// (比如「含非 ASCII 就拒」),中文目录会整个用不了。
    #[test]
    fn an_ordinary_chinese_path_stays_operable() {
        let p = RemotePath::from_bytes("/data/中文/说明.md".as_bytes().to_vec());
        assert!(p.is_operable());
        assert_eq!(p.as_wire().unwrap(), "/data/中文/说明.md");
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
