//! 连接参数与认证方式(F1)。只是数据,不含 UI/pane 概念。

/// F1 三种认证。
///
/// **不 derive Debug**:`Password` 与 `PublicKey.key_data` 都是明文凭据,
/// `{:?}` 一打就会把它们写进 ADR-008 的 mullion.log。手写打码实现
/// (与 `hop::Hop` 同一模式);`Hop::SshJump` 的 Debug 直接委托到这里。
#[derive(Clone)]
pub enum AuthMethod {
    /// 密码认证。
    Password(String),
    /// 公钥认证:**私钥内容本身**(PEM / OpenSSH 文本)+ 可选 passphrase。
    ///
    /// 收内容而不是路径:本 crate 不该去碰文件系统,而且凭据的来源
    /// (加密侧车 / 命令行 `-i` 给的文件)是调用方的事。
    PublicKey {
        key_data: String,
        passphrase: Option<String>,
    },
    /// ssh-agent 认证(从 SSH_AUTH_SOCK 取身份)。
    Agent,
}

impl std::fmt::Debug for AuthMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fn redacted(present: bool) -> &'static str {
            if present {
                "<已设置>"
            } else {
                "<无>"
            }
        }
        match self {
            Self::Password(_) => write!(f, "Password(<已设置>)"),
            Self::PublicKey {
                key_data,
                passphrase,
            } => write!(
                f,
                "PublicKey {{ key_data: {}, passphrase: {} }}",
                redacted(!key_data.is_empty()),
                redacted(passphrase.is_some())
            ),
            Self::Agent => write!(f, "Agent"),
        }
    }
}

/// 一次连接所需的全部参数。app 构造后交给 `session::connect`。
#[derive(Debug, Clone)]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: AuthMethod,
    /// 初始 PTY 尺寸;reflow 后由 `SshSession::resize` 同步(F34)。
    pub cols: u16,
    pub rows: u16,
    /// TERM 名,固定 "xterm-256color"。
    pub term: String,
    /// 拨号链(F4/F5)。**空 = 直连**。顺序即拨号顺序:`hops[0]` 最先建立。
    /// 由 app 从会话配置物化而来;本 crate 不认识「会话」「分组」。
    pub hops: Vec<crate::hop::Hop>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 红线:`SshConfig` derive 了 Debug,连接失败时整个 config 会被 `{:?}` 打进
    /// mullion.log。`AuthMethod` 若跟着 derive,密码和私钥正文就直接落盘了。
    #[test]
    fn debug_never_leaks_password_or_key_material() {
        let cfg = SshConfig {
            host: "h".into(),
            port: 22,
            user: "u".into(),
            auth: AuthMethod::Password("hunter2".into()),
            cols: 80,
            rows: 24,
            term: "xterm-256color".into(),
            hops: Vec::new(),
        };
        let s = format!("{cfg:?}");
        assert!(!s.contains("hunter2"), "密码不能出现在 Debug 里: {s}");
        assert!(s.contains('h'), "非敏感字段应保留以便排障: {s}");

        let key = AuthMethod::PublicKey {
            key_data: "-----BEGIN OPENSSH PRIVATE KEY-----\nKEYBODY\n".into(),
            passphrase: Some("keypw".into()),
        };
        let s = format!("{key:?}");
        assert!(!s.contains("KEYBODY"), "私钥正文不能出现在 Debug 里: {s}");
        assert!(!s.contains("keypw"), "私钥口令不能出现在 Debug 里: {s}");
    }

    /// 「有没有设」这一位要如实反映:全打成 `<已设置>` 的话,排障时看不出
    /// 究竟是钥匙没导进来还是口令给错了。
    #[test]
    fn debug_distinguishes_set_from_unset() {
        let s = format!(
            "{:?}",
            AuthMethod::PublicKey {
                key_data: String::new(),
                passphrase: None,
            }
        );
        assert!(!s.contains("已设置"), "空私钥/无口令不该显示为已设置: {s}");
    }
}
