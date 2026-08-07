//! 拨号链上的一跳(设计 §3.4)。
//!
//! **完全物化**:不含任何 store 类型(红线 2)。app 负责把「会话引用」翻译成这里的
//! 主机/端口/凭据,ssh 层拿到就能直接拨。

use crate::config::AuthMethod;

/// 拨号链上的一跳。顺序即拨号顺序:`hops[0]` 最先建立。
///
/// **不 derive Debug**:本类型携带明文口令,derive 会让 `{:?}` 把凭据
/// 打进 mullion.log(ADR-008 会记录连接阶段的诊断)。见下方手写实现。
#[derive(Clone)]
pub enum Hop {
    /// SOCKS5 代理(RFC 1928),`auth` 为 (用户名, 口令),RFC 1929。
    Socks5 {
        host: String,
        port: u16,
        auth: Option<(String, String)>,
    },
    /// HTTP CONNECT 代理,`auth` 走 `Proxy-Authorization: Basic`。
    HttpConnect {
        host: String,
        port: u16,
        auth: Option<(String, String)>,
    },
    /// SSH 跳板:在这一跳上开 direct-tcpip channel 通向下一跳。
    SshJump {
        host: String,
        port: u16,
        user: String,
        auth: AuthMethod,
    },
}

impl Hop {
    /// "host:port",用于错误消息里点名是哪一跳失败(F6)。
    pub fn endpoint(&self) -> String {
        match self {
            Hop::Socks5 { host, port, .. }
            | Hop::HttpConnect { host, port, .. }
            | Hop::SshJump { host, port, .. } => format!("{host}:{port}"),
        }
    }
}

impl std::fmt::Debug for Hop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Hop::Socks5 { host, port, auth } => f
                .debug_struct("Socks5")
                .field("host", host)
                .field("port", port)
                .field("user", &auth.as_ref().map(|(u, _)| u.as_str()))
                .field("password", &redacted(auth.is_some()))
                .finish(),
            Hop::HttpConnect { host, port, auth } => f
                .debug_struct("HttpConnect")
                .field("host", host)
                .field("port", port)
                .field("user", &auth.as_ref().map(|(u, _)| u.as_str()))
                .field("password", &redacted(auth.is_some()))
                .finish(),
            Hop::SshJump {
                host,
                port,
                user,
                auth,
            } => f
                .debug_struct("SshJump")
                .field("host", host)
                .field("port", port)
                .field("user", user)
                .field("auth", auth)
                .finish(),
        }
    }
}

fn redacted(present: bool) -> &'static str {
    if present {
        "<已设置>"
    } else {
        "<无>"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 红线:`Hop` 携带明文凭据。若 derive(Debug),被 `{:?}` 打进 ADR-008 的
    /// mullion.log 就是明文口令落盘。必须手写 Debug 并打码。
    #[test]
    fn debug_never_leaks_proxy_password() {
        let h = Hop::Socks5 {
            host: "127.0.0.1".into(),
            port: 7891,
            auth: Some(("alice".into(), "hunter2".into())),
        };
        let s = format!("{h:?}");
        assert!(!s.contains("hunter2"), "口令绝不能出现在 Debug 里: {s}");
        assert!(s.contains("127.0.0.1"), "非敏感字段应保留以便排障: {s}");
        assert!(s.contains("alice"), "用户名非敏感,保留: {s}");
    }

    #[test]
    fn debug_never_leaks_http_proxy_password() {
        let h = Hop::HttpConnect {
            host: "proxy.local".into(),
            port: 8080,
            auth: Some(("bob".into(), "s3cret".into())),
        };
        let s = format!("{h:?}");
        assert!(!s.contains("s3cret"), "口令绝不能出现在 Debug 里: {s}");
    }

    #[test]
    fn debug_never_leaks_jump_password() {
        let h = Hop::SshJump {
            host: "bastion".into(),
            port: 22,
            user: "ops".into(),
            auth: AuthMethod::Password("bastionpw".into()),
        };
        let s = format!("{h:?}");
        assert!(!s.contains("bastionpw"), "跳板口令也不能泄漏: {s}");
        assert!(s.contains("bastion"), "主机名保留以便排障: {s}");
    }

    /// 私钥内容与 passphrase 同样敏感 —— v5 起 `AuthMethod` 携带的是私钥
    /// **正文**而不是路径,一旦被 `{:?}` 打进日志就是裸钥匙落盘。
    #[test]
    fn debug_never_leaks_key_material_or_passphrase() {
        let h = Hop::SshJump {
            host: "bastion".into(),
            port: 22,
            user: "ops".into(),
            auth: AuthMethod::PublicKey {
                key_data: "-----BEGIN OPENSSH PRIVATE KEY-----\nKEYBODY\n".into(),
                passphrase: Some("keypw".into()),
            },
        };
        let s = format!("{h:?}");
        assert!(!s.contains("keypw"), "私钥口令不能泄漏: {s}");
        assert!(!s.contains("KEYBODY"), "私钥正文不能泄漏: {s}");
        assert!(s.contains("bastion"), "主机名保留以便排障: {s}");
    }

    #[test]
    fn endpoint_string_is_host_colon_port() {
        let h = Hop::SshJump {
            host: "bastion".into(),
            port: 2222,
            user: "ops".into(),
            auth: AuthMethod::Agent,
        };
        assert_eq!(h.endpoint(), "bastion:2222");
    }
}
