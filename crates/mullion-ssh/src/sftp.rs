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
