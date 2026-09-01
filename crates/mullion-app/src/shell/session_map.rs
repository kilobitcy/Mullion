//! 把 store 的 `SessionRecord`(+ 解密后的 `SecretEntry`)映射成 `mullion_ssh` 的连接参数。
//! SFTP 节点在 D1 起也走这里:映射出**同样的**拨号参数,只是多带一位
//! `wants_sftp` —— 连上之后开 sftp subsystem 而不是 PTY(设计 D24)。

use std::fmt;

use mullion_ssh::config::{AuthMethod, SshConfig};
use mullion_store::{AuthKind, Protocol, ResolvedAuth, SessionRecord};

/// 映射失败原因。
#[derive(Debug, PartialEq, Eq)]
pub enum MapError {
    /// 需要密码/口令但没提供(会话配置不完整或解密缺失)。
    MissingSecret,
    /// 公钥会话但侧车里没有私钥正文。**与 `MissingSecret` 分开**:v4→v5 迁移
    /// 时若旧路径指向的文件已被删/挪走,会话就落在这个状态,用户要做的是
    /// 「重新导入私钥」而不是「填口令」,文案指错地方等于没提示。
    MissingPrivateKey,
}

impl fmt::Display for MapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MapError::MissingSecret => write!(f, "缺少密码/私钥口令 —— 请在会话里重新配置认证"),
            MapError::MissingPrivateKey => {
                write!(f, "私钥未导入 —— 请在会话的「认证」页重新导入私钥文件")
            }
        }
    }
}

impl std::error::Error for MapError {}

/// `SessionRecord` + **已解析的身份** → `SshConfig`。cols/rows 先给占位默认,
/// 窗口出来后由 window_change 校正到真实尺寸(与既有 `cli::parse_args` 一致)。
///
/// 身份走 `ResolvedAuth` 而不是直接读 `rec.auth`(F74):会话可能引用共享凭据,
/// 那时用户名/认证方式/密文全在凭据那边,而查凭据是可失败的(悬空必须硬失败,
/// 设计 D6),不该混进这层「只做映射」的纯函数。
///
/// **模块私有,对外只留 [`to_dial_plan`]。** 公开两个入口的话,后来者顺手
/// 拿了这个就悄悄丢掉 `wants_sftp` —— 那正是 D24 要堵的坑,没道理自己再
/// 挖一遍。
fn to_ssh_config(rec: &SessionRecord, resolved: &ResolvedAuth) -> Result<SshConfig, MapError> {
    let secret = resolved.secret.as_ref();
    let auth = match &resolved.kind {
        AuthKind::Password => {
            let pw = secret
                .and_then(|s| s.password.clone())
                .ok_or(MapError::MissingSecret)?;
            AuthMethod::Password(pw)
        }
        AuthKind::PublicKey { has_passphrase } => {
            let passphrase = if *has_passphrase {
                Some(
                    secret
                        .and_then(|s| s.passphrase.clone())
                        .ok_or(MapError::MissingSecret)?,
                )
            } else {
                None
            };
            // v5 起私钥正文在侧车里,不再是磁盘路径。
            let key_data = secret
                .and_then(|s| s.private_key.clone())
                .ok_or(MapError::MissingPrivateKey)?;
            AuthMethod::PublicKey {
                key_data,
                passphrase,
            }
        }
    };
    Ok(SshConfig {
        host: rec.connection.host.clone(),
        port: rec.connection.port,
        user: resolved.user.clone(),
        auth,
        cols: 80,
        rows: 24,
        term: "xterm-256color".to_string(),
        hops: Vec::new(),
    })
}

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

/// `SessionRecord` + 已解析的身份 → 一次完整的连接意图。
pub fn to_dial_plan(rec: &SessionRecord, resolved: &ResolvedAuth) -> Result<DialPlan, MapError> {
    Ok(DialPlan {
        cfg: to_ssh_config(rec, resolved)?,
        wants_sftp: rec.connection.protocol == Protocol::Sftp,
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
            auth: Auth::inline("u", auth),
            terminal: Default::default(),
            appearance: Default::default(),
            network: Default::default(),
            automation: Default::default(),
            sftp: Default::default(),
        }
    }

    /// 上游(`shell::store`)解析完之后交给映射层的形状:自带认证的会话
    /// 就是「照抄自己那份 + 自己的侧车」。
    fn resolved(r: &SessionRecord, sec: Option<&SecretEntry>) -> ResolvedAuth {
        let i = r.auth.as_inline().expect("测试里都是自带认证");
        ResolvedAuth {
            user: i.user.clone(),
            kind: i.kind.clone(),
            secret: sec.cloned(),
        }
    }

    #[test]
    fn password_maps_with_secret() {
        let r = rec(AuthKind::Password, Protocol::Ssh);
        let sec = SecretEntry {
            password: Some("pw".into()),
            passphrase: None,
            proxy_password: None,
            private_key: None,
        };
        let cfg = to_ssh_config(&r, &resolved(&r, Some(&sec))).unwrap();
        assert_eq!(cfg.host, "h");
        assert_eq!(cfg.port, 2222);
        assert_eq!(cfg.user, "u");
        assert_eq!(cfg.term, "xterm-256color");
        assert!(matches!(cfg.auth, AuthMethod::Password(p) if p == "pw"));
    }

    /// F211:登记 tmux `sync` 特性时报的 TERM,必须与我们真的向 pty 请求的那个
    /// 一字不差。
    ///
    /// tmux 按 client 的 TERM 做模式匹配来决定要不要转发 DEC 2026 同步块。对不
    /// 上的后果是**静默不生效**:没有任何报错,只是同步块永远不来、T2 的攒帧
    /// 继续空转,而这只有人眼在真机上才看得出来。`mullion-store` 不依赖
    /// `mullion-ssh`(架构不变量),两边只能各写一份字面量 —— 这条跨 crate 断言
    /// 就是那两份字面量之间唯一的锁。
    ///
    /// 自证会变红:把 `SshConfig.term` 改成 `"xterm-256color-truecolor"`。
    #[test]
    fn the_term_we_ask_the_pty_for_is_the_one_we_register_tmux_sync_against() {
        let r = rec(AuthKind::Password, Protocol::Ssh);
        let sec = SecretEntry {
            password: Some("pw".into()),
            passphrase: None,
            proxy_password: None,
            private_key: None,
        };
        let cfg = to_ssh_config(&r, &resolved(&r, Some(&sec))).unwrap();
        assert_eq!(
            cfg.term,
            mullion_store::automation::SYNC_TERM,
            "TERM 与 tmux sync 登记的模式对不上,同步块会静默不来"
        );
    }

    #[test]
    fn password_without_secret_errors() {
        let r = rec(AuthKind::Password, Protocol::Ssh);
        assert!(matches!(
            to_ssh_config(&r, &resolved(&r, None)),
            Err(MapError::MissingSecret)
        ));
    }

    /// 私钥正文取自侧车而不是磁盘 —— 这正是 v5 的改动点。
    #[test]
    fn pubkey_takes_the_key_body_from_the_secret_sidecar() {
        let r = rec(
            AuthKind::PublicKey {
                has_passphrase: true,
            },
            Protocol::Ssh,
        );
        let sec = SecretEntry {
            password: None,
            passphrase: Some("ph".into()),
            proxy_password: None,
            private_key: Some("KEYBODY".into()),
        };
        let cfg = to_ssh_config(&r, &resolved(&r, Some(&sec))).unwrap();
        match cfg.auth {
            AuthMethod::PublicKey {
                key_data,
                passphrase,
            } => {
                assert_eq!(key_data, "KEYBODY");
                assert_eq!(passphrase.as_deref(), Some("ph"));
            }
            _ => panic!("应为 PublicKey"),
        }
    }

    #[test]
    fn pubkey_no_passphrase_maps_none() {
        let r = rec(
            AuthKind::PublicKey {
                has_passphrase: false,
            },
            Protocol::Ssh,
        );
        let sec = SecretEntry {
            password: None,
            passphrase: None,
            proxy_password: None,
            private_key: Some("KEYBODY".into()),
        };
        let cfg = to_ssh_config(&r, &resolved(&r, Some(&sec))).unwrap();
        assert!(matches!(
            cfg.auth,
            AuthMethod::PublicKey {
                passphrase: None,
                ..
            }
        ));
    }

    /// 迁移时私钥文件读不到的会话会落在「没有私钥正文」上。这时报的必须是
    /// 「重新导入私钥」而不是「缺少密码/口令」—— 后者会把用户引到口令输入框,
    /// 而那里怎么填都连不上。
    #[test]
    fn pubkey_without_key_body_says_reimport_not_missing_password() {
        let r = rec(
            AuthKind::PublicKey {
                has_passphrase: false,
            },
            Protocol::Ssh,
        );
        assert!(matches!(
            to_ssh_config(&r, &resolved(&r, None)),
            Err(MapError::MissingPrivateKey)
        ));
        assert!(
            MapError::MissingPrivateKey.to_string().contains("导入"),
            "文案要指向重新导入私钥"
        );
    }

    /// D24:SFTP 记录不再被映射层拒绝,而是映射成**合法的拨号参数**。
    /// 判据是「参数对得上」+「`wants_sftp` 为真」——后者是 app 侧
    /// 「连上后开 sftp subsystem 而不是 PTY」的开关。
    ///
    /// 这条替代了 F118 时期的 `SftpNotSupported` 断言:那时拒绝是对的
    /// (无处可去),D1 给了 SFTP 节点自己的标签页,再拒绝就是功能残缺。
    #[test]
    fn an_sftp_record_maps_to_real_dial_parameters_and_asks_for_the_sftp_subsystem() {
        let r = rec(AuthKind::Password, Protocol::Sftp);
        let sec = SecretEntry {
            password: Some("pw".into()),
            passphrase: None,
            proxy_password: None,
            private_key: None,
        };

        let plan =
            to_dial_plan(&r, &resolved(&r, Some(&sec))).expect("SFTP 节点必须能映射出拨号参数");
        assert_eq!(plan.cfg.host, "h");
        assert_eq!(plan.cfg.port, 2222);
        assert!(plan.wants_sftp, "SFTP 节点连上后开的是 sftp subsystem");
    }

    /// 反面:SSH 会话不得被标成要 sftp。搞反了的症状是「双击普通会话
    /// 开出一个文件面板、终端再也出不来」。
    #[test]
    fn an_ssh_record_does_not_ask_for_the_sftp_subsystem() {
        let r = rec(AuthKind::Password, Protocol::Ssh);
        let sec = SecretEntry {
            password: Some("pw".into()),
            passphrase: None,
            proxy_password: None,
            private_key: None,
        };
        let plan = to_dial_plan(&r, &resolved(&r, Some(&sec))).expect("SSH 会话映射");
        assert!(!plan.wants_sftp);
    }

    #[test]
    fn pubkey_with_passphrase_but_no_secret_errors() {
        // has_passphrase=true 但没提供 secret → MissingSecret(与 Password 路径对称)
        let r = rec(
            AuthKind::PublicKey {
                has_passphrase: true,
            },
            Protocol::Ssh,
        );
        assert!(matches!(
            to_ssh_config(&r, &resolved(&r, None)),
            Err(MapError::MissingSecret)
        ));
    }
}
