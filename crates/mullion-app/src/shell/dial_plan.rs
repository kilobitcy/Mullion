//! 把 store 的「拨号声明」物化成 ssh 的 `Hop` 列表(设计 §4)。
//!
//! **红线 2 的枢纽**:store 类型只出现在入参,`Hop` 只出现在出参。
//! `mullion-ssh` 因此永远不需要认识「会话」「分组」。纯函数,零 IO。

use mullion_ssh::config::AuthMethod;
use mullion_ssh::hop::Hop;
use mullion_store::{AuthKind, ProxyChoice, ResolvedAuth, SecretEntry, SessionRecord};

/// 一跳跳板 + 它**已经解析好**的身份(F74)。
///
/// 身份不再由本模块去查:引用的凭据可能悬空,而悬空必须**硬失败**(设计 D6)。
/// 物化是纯函数、没有失败通道,所以解析只能发生在上游(`shell::store`),
/// 那里 `?` 得出去。把已解析的结果当入参传进来,「先解析再物化」这条顺序
/// 就成了类型层面的事实,而不是靠注释提醒。
pub struct Jump<'a> {
    pub rec: &'a SessionRecord,
    pub auth: ResolvedAuth,
}

/// 物化拨号链。
///
/// `proxy_secret` 是目标会话的 secret(代理口令只存会话级,见设计 §3.3 / D4)。
pub fn build_hops_with_proxy_secret(
    proxy: Option<&ProxyChoice>,
    jumps: &[Jump<'_>],
    proxy_secret: Option<&SecretEntry>,
) -> Vec<Hop> {
    let mut hops = Vec::new();

    // 代理排在最前:先出本机网络,才谈得上连跳板。
    if let Some(choice) = proxy {
        let pw = proxy_secret.and_then(|s| s.proxy_password.clone());
        match choice {
            // Direct 是「显式不走代理」,不是一跳。
            ProxyChoice::Direct => {}
            ProxyChoice::Socks5(ep) => hops.push(Hop::Socks5 {
                host: ep.host.clone(),
                port: ep.port,
                auth: pair(ep.user.as_deref(), pw),
            }),
            ProxyChoice::HttpConnect(ep) => hops.push(Hop::HttpConnect {
                host: ep.host.clone(),
                port: ep.port,
                auth: pair(ep.user.as_deref(), pw),
            }),
        }
    }

    for j in jumps {
        hops.push(Hop::SshJump {
            host: j.rec.connection.host.clone(),
            port: j.rec.connection.port,
            user: j.auth.user.clone(),
            auth: jump_auth(&j.auth),
        });
    }

    hops
}

/// 无代理口令的便捷入口(测试与「目标会话无 secret」时用)。
pub fn build_hops(proxy: Option<&ProxyChoice>, jumps: &[Jump<'_>]) -> Vec<Hop> {
    build_hops_with_proxy_secret(proxy, jumps, None)
}

/// 用户名与口令**必须成对**才发认证:只有用户名就拿空口令去谈,几乎必被拒,
/// 且会把「没配口令」误报成「口令错」。
fn pair(user: Option<&str>, pw: Option<String>) -> Option<(String, String)> {
    match (user, pw) {
        (Some(u), Some(p)) => Some((u.to_string(), p)),
        _ => None,
    }
}

/// 跳板的认证方式。身份取自**跳板自己那条会话**的解析结果
/// (自带认证 → 它自己的侧车;引用凭据 → 凭据的侧车)。
fn jump_auth(resolved: &ResolvedAuth) -> AuthMethod {
    let secret = resolved.secret.clone();
    match &resolved.kind {
        AuthKind::Password => match secret.and_then(|s| s.password) {
            Some(p) => AuthMethod::Password(p),
            // 没存密码就退回 agent:拿空串去认证只会得到一条误导性的 AuthFailed。
            None => AuthMethod::Agent,
        },
        // v5 起私钥正文在跳板自己那条会话的侧车里。没导入私钥就退回 agent,
        // 理由同上:拿空私钥去谈只会得到一条指不到原因的 AuthFailed。
        AuthKind::PublicKey { .. } => match secret {
            Some(s) => match s.private_key {
                Some(key_data) => AuthMethod::PublicKey {
                    key_data,
                    passphrase: s.passphrase,
                },
                None => AuthMethod::Agent,
            },
            None => AuthMethod::Agent,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_store::{
        Auth, Connection, Identity, NetworkPrefs, Protocol, ProxyEndpoint, SessionId,
    };

    fn rec(id: u64, host: &str) -> SessionRecord {
        SessionRecord {
            id: SessionId(id),
            modified_at: "t".into(),
            identity: Identity {
                name: host.into(),
                note: String::new(),
                group_id: None,
                tags: Vec::new(),
            },
            connection: Connection {
                host: host.into(),
                port: 22,
                protocol: Protocol::Ssh,
            },
            auth: Auth::inline("ops", AuthKind::Password),
            terminal: Default::default(),
            appearance: Default::default(),
            network: NetworkPrefs::default(),
            automation: Default::default(),
            sftp: Default::default(),
        }
    }

    fn pw(p: &str) -> SecretEntry {
        SecretEntry {
            password: Some(p.into()),
            passphrase: None,
            proxy_password: None,
            private_key: None,
        }
    }

    /// 上游(`shell::store`)解析完之后交给物化层的形状。
    fn jump(rec: &SessionRecord, secret: Option<SecretEntry>) -> Jump<'_> {
        let inline = rec.auth.as_inline().expect("测试里的跳板都是自带认证");
        Jump {
            rec,
            auth: ResolvedAuth {
                user: inline.user.clone(),
                kind: inline.kind.clone(),
                secret,
            },
        }
    }

    #[test]
    fn direct_session_produces_no_hops() {
        let hops = build_hops(None, &[]);
        assert!(hops.is_empty(), "无代理无跳板应产出空链");
    }

    /// 代理排在所有跳板之前:先出本机网络,再谈跳板。
    #[test]
    fn proxy_comes_before_every_jump() {
        let proxy = ProxyChoice::Socks5(ProxyEndpoint {
            host: "127.0.0.1".into(),
            port: 7891,
            user: None,
        });
        let b = rec(2, "bastion");
        let hops = build_hops(Some(&proxy), &[jump(&b, Some(pw("bp")))]);
        assert_eq!(hops.len(), 2);
        assert!(matches!(hops[0], Hop::Socks5 { .. }), "代理必须在最前");
        assert!(matches!(hops[1], Hop::SshJump { .. }));
    }

    /// `Direct` 是显式直连,不该物化出任何代理跳。
    #[test]
    fn explicit_direct_produces_no_proxy_hop() {
        let hops = build_hops(Some(&ProxyChoice::Direct), &[]);
        assert!(hops.is_empty(), "Direct 不是一跳,不该出现在链上");
    }

    #[test]
    fn jump_order_is_preserved_as_dial_order() {
        let (b1, b2) = (rec(2, "b1"), rec(3, "b2"));
        let hops = build_hops(None, &[jump(&b1, Some(pw("x"))), jump(&b2, Some(pw("x")))]);
        match (&hops[0], &hops[1]) {
            (Hop::SshJump { host: a, .. }, Hop::SshJump { host: b, .. }) => {
                assert_eq!((a.as_str(), b.as_str()), ("b1", "b2"));
            }
            other => panic!("应为两个 SshJump,实际: {other:?}"),
        }
    }

    /// 跳板的凭据取自**跳板自己那份解析结果**,不是目标会话的 secret。
    /// 搞混的症状是「跳板拿目标机的密码去认证」,报出来的是一条指错方向的
    /// AuthFailed。这里同时喂一份目标会话的 secret,确保它不会串味。
    #[test]
    fn jump_credentials_come_from_the_jump_session_not_the_target() {
        let b = rec(2, "bastion");
        let hops = build_hops_with_proxy_secret(
            None,
            &[jump(&b, Some(pw("bastion-pw")))],
            Some(&pw("target-pw")),
        );
        match &hops[0] {
            Hop::SshJump { auth, .. } => {
                assert!(matches!(auth, AuthMethod::Password(p) if p == "bastion-pw"));
            }
            other => panic!("实际: {other:?}"),
        }
    }

    /// 公钥跳板的私钥正文取自**跳板会话自己的侧车**(v5)。
    #[test]
    fn pubkey_jump_carries_the_key_body_from_its_own_secret() {
        let mut r = rec(2, "bastion");
        r.auth = Auth::inline(
            "ops",
            AuthKind::PublicKey {
                has_passphrase: true,
            },
        );
        let hops = build_hops(
            None,
            &[jump(
                &r,
                Some(SecretEntry {
                    password: None,
                    passphrase: Some("ph".into()),
                    proxy_password: None,
                    private_key: Some("KEYBODY".into()),
                }),
            )],
        );
        match &hops[0] {
            Hop::SshJump {
                auth:
                    AuthMethod::PublicKey {
                        key_data,
                        passphrase,
                    },
                ..
            } => {
                assert_eq!(key_data, "KEYBODY");
                assert_eq!(passphrase.as_deref(), Some("ph"));
            }
            other => panic!("实际: {other:?}"),
        }
    }

    /// 公钥跳板没导入私钥 → 退回 agent。理由与密码路径同:拿空私钥去谈会得到
    /// 一条指不到原因的 AuthFailed。
    #[test]
    fn pubkey_jump_without_imported_key_falls_back_to_agent() {
        let mut r = rec(2, "bastion");
        r.auth = Auth::inline(
            "ops",
            AuthKind::PublicKey {
                has_passphrase: false,
            },
        );
        let hops = build_hops(
            None,
            &[jump(
                &r,
                Some(SecretEntry {
                    password: None,
                    passphrase: None,
                    proxy_password: None,
                    private_key: None,
                }),
            )],
        );
        match &hops[0] {
            Hop::SshJump { auth, .. } => assert!(
                matches!(auth, AuthMethod::Agent),
                "没导入私钥应退回 agent,实际 {auth:?}"
            ),
            other => panic!("实际: {other:?}"),
        }
    }

    /// 跳板会话没存密码 → 退回 agent,而不是拿空串去认证(那必然 AuthFailed 且信息误导)。
    #[test]
    fn jump_without_stored_password_falls_back_to_agent() {
        let b = rec(2, "bastion");
        let hops = build_hops(None, &[jump(&b, None)]);
        match &hops[0] {
            Hop::SshJump { auth, .. } => assert!(matches!(auth, AuthMethod::Agent)),
            other => panic!("实际: {other:?}"),
        }
    }

    #[test]
    fn socks5_proxy_credentials_are_materialized() {
        let proxy = ProxyChoice::Socks5(ProxyEndpoint {
            host: "127.0.0.1".into(),
            port: 7891,
            user: Some("alice".into()),
        });
        let secret = SecretEntry {
            password: None,
            passphrase: None,
            proxy_password: Some("ppw".into()),
            private_key: None,
        };
        let hops = build_hops_with_proxy_secret(Some(&proxy), &[], Some(&secret));
        match &hops[0] {
            Hop::Socks5 { auth, .. } => {
                assert_eq!(
                    auth.as_ref().map(|(u, p)| (u.as_str(), p.as_str())),
                    Some(("alice", "ppw"))
                );
            }
            other => panic!("实际: {other:?}"),
        }
    }

    /// 代理配了用户名但没存口令 → 按免认证发起(空口令几乎必被拒,还会把
    /// 「没配口令」误报成「口令错」)。
    #[test]
    fn proxy_user_without_password_degrades_to_anonymous() {
        let proxy = ProxyChoice::HttpConnect(ProxyEndpoint {
            host: "p".into(),
            port: 8080,
            user: Some("alice".into()),
        });
        let hops = build_hops_with_proxy_secret(Some(&proxy), &[], None);
        match &hops[0] {
            Hop::HttpConnect { auth, .. } => assert!(auth.is_none()),
            other => panic!("实际: {other:?}"),
        }
    }
}
