//! F6:连接失败给可操作错误。每类一个变体,红线是不许统一 "connection failed"。

use std::fmt;

use crate::known_hosts::Fingerprint;

/// 连接期的可操作错误(F6)。每个变体对应一类可区分的失败。
#[derive(Debug)]
pub enum ConnectError {
    /// 域名解析失败(区别于「解析成功但连不上」)。
    DnsResolution(String),
    /// TCP 连接被拒绝(对端无监听 / 防火墙 RST)。
    ConnectionRefused(String),
    /// 认证失败(凭据不对,区别于连接失败)。
    AuthFailed,
    /// 主机密钥变更 —— 疑似 MITM,已拦截(F3)。
    HostKeyChanged {
        host: String,
        expected: Fingerprint,
        got: Fingerprint,
    },
    /// 首次连接此主机,指纹未记录,需 TOFU 确认(F3)。
    /// 当前仅未来的 app 弹窗策略会产生;`TofuAccept` 自动记录未知主机,不产生此变体。
    HostKeyUnknown { host: String, got: Fingerprint },
    /// 其余 IO 错误(网络 / 读私钥 / agent socket)。
    Io(String),
    /// 开 channel / request_pty 失败。
    PtyRequest,
}

/// 把 TCP 连接阶段的 io 错误分类到精确变体(F6)。
pub(crate) fn classify_tcp(e: std::io::Error) -> ConnectError {
    match e.kind() {
        std::io::ErrorKind::ConnectionRefused => ConnectError::ConnectionRefused(e.to_string()),
        _ => ConnectError::Io(e.to_string()),
    }
}

fn hex(fp: &Fingerprint) -> String {
    fp.0.iter().map(|b| format!("{b:02x}")).collect()
}

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectError::DnsResolution(h) => write!(f, "域名解析失败:{h} —— 检查主机名/DNS"),
            ConnectError::ConnectionRefused(a) => {
                write!(f, "连接被拒绝:{a} —— 检查端口/sshd 是否在跑")
            }
            ConnectError::AuthFailed => write!(f, "认证失败 —— 检查用户名/密钥/密码"),
            ConnectError::HostKeyChanged {
                host,
                expected,
                got,
            } => write!(
                f,
                "主机 {host} 的密钥已变更(疑似中间人,已拦截):记录 {} → 收到 {}",
                hex(expected),
                hex(got)
            ),
            ConnectError::HostKeyUnknown { host, got } => {
                write!(f, "首次连接 {host},指纹 {} 未记录,需确认(TOFU)", hex(got))
            }
            ConnectError::Io(e) => write!(f, "网络 IO 错误:{e}"),
            ConnectError::PtyRequest => write!(f, "开 PTY 失败 —— 对端可能不允许 PTY"),
        }
    }
}

impl std::error::Error for ConnectError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refused_is_distinct_from_generic_io() {
        let refused = classify_tcp(std::io::Error::from(std::io::ErrorKind::ConnectionRefused));
        assert!(matches!(refused, ConnectError::ConnectionRefused(_)));
        let other = classify_tcp(std::io::Error::from(std::io::ErrorKind::TimedOut));
        assert!(
            matches!(other, ConnectError::Io(_)),
            "非 refused 应落 Io,不得混为一类"
        );
    }

    #[test]
    fn every_variant_has_distinct_actionable_message() {
        // F6 红线:每类错误消息互不相同且非空,不许统一 "connection failed"。
        let variants = [
            ConnectError::DnsResolution("h".into()),
            ConnectError::ConnectionRefused("1.2.3.4:22".into()),
            ConnectError::AuthFailed,
            ConnectError::HostKeyChanged {
                host: "h".into(),
                expected: Fingerprint(vec![1]),
                got: Fingerprint(vec![2]),
            },
            ConnectError::HostKeyUnknown {
                host: "h".into(),
                got: Fingerprint(vec![3]),
            },
            ConnectError::Io("io".into()),
            ConnectError::PtyRequest,
        ];
        let msgs: Vec<String> = variants.iter().map(|e| e.to_string()).collect();
        for m in &msgs {
            assert!(!m.is_empty());
        }
        let mut uniq = msgs.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(uniq.len(), msgs.len(), "错误消息必须两两不同(F6)");
    }
}
