//! 把 store 的 `SessionRecord`(+ 解密后的 `SecretEntry`)映射成 `mullion_ssh` 的连接参数。
//! SFTP 连接是切片 D,本片双击 sftp 会话在映射层直接拒绝(app 侧应更早禁用,这里兜底)。

use std::fmt;

use mullion_ssh::config::{AuthMethod, SshConfig};
use mullion_store::{AuthKind, Protocol, SecretEntry, SessionRecord};

/// 映射失败原因。
#[derive(Debug, PartialEq, Eq)]
pub enum MapError {
    /// 需要密码/口令但没提供(会话配置不完整或解密缺失)。
    MissingSecret,
    /// SFTP 连接属切片 D,本片不支持。
    SftpNotSupported,
}

impl fmt::Display for MapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MapError::MissingSecret => write!(f, "缺少密码/私钥口令 —— 请在会话里重新配置认证"),
            MapError::SftpNotSupported => write!(f, "SFTP 连接尚未实现(切片 D)"),
        }
    }
}

impl std::error::Error for MapError {}

/// `SessionRecord` + 解密后的敏感部分 → `SshConfig`。cols/rows 先给占位默认,
/// 窗口出来后由 window_change 校正到真实尺寸(与既有 `cli::parse_args` 一致)。
pub fn to_ssh_config(
    rec: &SessionRecord,
    secret: Option<&SecretEntry>,
) -> Result<SshConfig, MapError> {
    if rec.connection.protocol == Protocol::Sftp {
        return Err(MapError::SftpNotSupported);
    }
    let auth = match &rec.auth.kind {
        AuthKind::Password => {
            let pw = secret
                .and_then(|s| s.password.clone())
                .ok_or(MapError::MissingSecret)?;
            AuthMethod::Password(pw)
        }
        AuthKind::PublicKey {
            path,
            has_passphrase,
        } => {
            let passphrase = if *has_passphrase {
                Some(
                    secret
                        .and_then(|s| s.passphrase.clone())
                        .ok_or(MapError::MissingSecret)?,
                )
            } else {
                None
            };
            AuthMethod::PublicKey {
                path: path.clone(),
                passphrase,
            }
        }
    };
    Ok(SshConfig {
        host: rec.connection.host.clone(),
        port: rec.connection.port,
        user: rec.auth.user.clone(),
        auth,
        cols: 80,
        rows: 24,
        term: "xterm-256color".to_string(),
        hops: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_ssh::config::AuthMethod;
    use mullion_store::{
        Auth, AuthKind, Connection, Identity, Protocol, SecretEntry, SessionId, SessionRecord,
    };

    fn rec(auth: AuthKind, proto: Protocol) -> SessionRecord {
        SessionRecord {
            id: SessionId(1),
            modified_at: "t".into(),
            identity: Identity {
                name: "s".into(),
                note: String::new(),
                group_id: None,
                tags: Vec::new(),
            },
            connection: Connection {
                host: "h".into(),
                port: 2222,
                protocol: proto,
            },
            auth: Auth {
                user: "u".into(),
                kind: auth,
            },
            terminal: Default::default(),
            appearance: Default::default(),
            network: Default::default(),
        }
    }

    #[test]
    fn password_maps_with_secret() {
        let r = rec(AuthKind::Password, Protocol::Ssh);
        let sec = SecretEntry {
            password: Some("pw".into()),
            passphrase: None,
            proxy_password: None,
        };
        let cfg = to_ssh_config(&r, Some(&sec)).unwrap();
        assert_eq!(cfg.host, "h");
        assert_eq!(cfg.port, 2222);
        assert_eq!(cfg.user, "u");
        assert_eq!(cfg.term, "xterm-256color");
        assert!(matches!(cfg.auth, AuthMethod::Password(p) if p == "pw"));
    }

    #[test]
    fn password_without_secret_errors() {
        let r = rec(AuthKind::Password, Protocol::Ssh);
        assert!(matches!(
            to_ssh_config(&r, None),
            Err(MapError::MissingSecret)
        ));
    }

    #[test]
    fn pubkey_with_passphrase_maps() {
        let r = rec(
            AuthKind::PublicKey {
                path: "/k".into(),
                has_passphrase: true,
            },
            Protocol::Ssh,
        );
        let sec = SecretEntry {
            password: None,
            passphrase: Some("ph".into()),
            proxy_password: None,
        };
        let cfg = to_ssh_config(&r, Some(&sec)).unwrap();
        match cfg.auth {
            AuthMethod::PublicKey { path, passphrase } => {
                assert_eq!(path, std::path::PathBuf::from("/k"));
                assert_eq!(passphrase.as_deref(), Some("ph"));
            }
            _ => panic!("应为 PublicKey"),
        }
    }

    #[test]
    fn pubkey_no_passphrase_maps_none() {
        let r = rec(
            AuthKind::PublicKey {
                path: "/k".into(),
                has_passphrase: false,
            },
            Protocol::Ssh,
        );
        let cfg = to_ssh_config(&r, None).unwrap();
        assert!(matches!(
            cfg.auth,
            AuthMethod::PublicKey {
                passphrase: None,
                ..
            }
        ));
    }

    #[test]
    fn sftp_is_rejected_in_a2() {
        let r = rec(AuthKind::Password, Protocol::Sftp);
        let sec = SecretEntry {
            password: Some("pw".into()),
            passphrase: None,
            proxy_password: None,
        };
        assert!(matches!(
            to_ssh_config(&r, Some(&sec)),
            Err(MapError::SftpNotSupported)
        ));
    }

    #[test]
    fn pubkey_with_passphrase_but_no_secret_errors() {
        // has_passphrase=true 但没提供 secret → MissingSecret(与 Password 路径对称)
        let r = rec(
            AuthKind::PublicKey {
                path: "/k".into(),
                has_passphrase: true,
            },
            Protocol::Ssh,
        );
        assert!(matches!(
            to_ssh_config(&r, None),
            Err(MapError::MissingSecret)
        ));
    }
}
