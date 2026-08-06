//! 把 store 的「拨号声明」物化成 ssh 的 `Hop` 列表(设计 §4)。
//!
//! **红线 2 的枢纽**:store 类型只出现在入参,`Hop` 只出现在出参。
//! `mullion-ssh` 因此永远不需要认识「会话」「分组」。纯函数,零 IO。

use mullion_ssh::config::AuthMethod;
use mullion_ssh::hop::Hop;
use mullion_store::{AuthKind, ProxyChoice, SecretEntry, SessionRecord};

/// 物化拨号链。`secret_of` 用于查**每一跳自己那条会话**的凭据。
///
/// `proxy_secret` 是目标会话的 secret(代理口令只存会话级,见设计 §3.3)。
pub fn build_hops_with_proxy_secret(
    proxy: Option<&ProxyChoice>,
    jumps: &[SessionRecord],
    secret_of: &dyn Fn(mullion_store::SessionId) -> Option<SecretEntry>,
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

    for rec in jumps {
        hops.push(Hop::SshJump {
            host: rec.connection.host.clone(),
            port: rec.connection.port,
            user: rec.auth.user.clone(),
            auth: jump_auth(rec, secret_of(rec.id)),
        });
    }

    hops
}

/// 无代理口令的便捷入口(测试与「目标会话无 secret」时用)。
pub fn build_hops(
    proxy: Option<&ProxyChoice>,
    jumps: &[SessionRecord],
    secret_of: &dyn Fn(mullion_store::SessionId) -> Option<SecretEntry>,
) -> Vec<Hop> {
    build_hops_with_proxy_secret(proxy, jumps, secret_of, None)
}

/// 用户名与口令**必须成对**才发认证:只有用户名就拿空口令去谈,几乎必被拒,
/// 且会把「没配口令」误报成「口令错」。
fn pair(user: Option<&str>, pw: Option<String>) -> Option<(String, String)> {
    match (user, pw) {
        (Some(u), Some(p)) => Some((u.to_string(), p)),
        _ => None,
    }
}

/// 跳板的认证方式。凭据取自**跳板自己那条会话**。
fn jump_auth(rec: &SessionRecord, secret: Option<SecretEntry>) -> AuthMethod {
    match &rec.auth.kind {
        AuthKind::Password => match secret.and_then(|s| s.password) {
            Some(p) => AuthMethod::Password(p),
            // 没存密码就退回 agent:拿空串去认证只会得到一条误导性的 AuthFailed。
            None => AuthMethod::Agent,
        },
        AuthKind::PublicKey { path, .. } => AuthMethod::PublicKey {
            path: path.clone(),
            passphrase: secret.and_then(|s| s.passphrase),
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
            auth: Auth {
                user: "ops".into(),
                kind: AuthKind::Password,
            },
            terminal: Default::default(),
            appearance: Default::default(),
            network: NetworkPrefs::default(),
            automation: Default::default(),
        }
    }

    fn pw(p: &str) -> SecretEntry {
        SecretEntry {
            password: Some(p.into()),
            passphrase: None,
            proxy_password: None,
        }
    }

    #[test]
    fn direct_session_produces_no_hops() {
        let hops = build_hops(None, &[], &|_| None);
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
        let jumps = vec![rec(2, "bastion")];
        let hops = build_hops(Some(&proxy), &jumps, &|_| Some(pw("bp")));
        assert_eq!(hops.len(), 2);
        assert!(matches!(hops[0], Hop::Socks5 { .. }), "代理必须在最前");
        assert!(matches!(hops[1], Hop::SshJump { .. }));
    }

    /// `Direct` 是显式直连,不该物化出任何代理跳。
    #[test]
    fn explicit_direct_produces_no_proxy_hop() {
        let hops = build_hops(Some(&ProxyChoice::Direct), &[], &|_| None);
        assert!(hops.is_empty(), "Direct 不是一跳,不该出现在链上");
    }

    #[test]
    fn jump_order_is_preserved_as_dial_order() {
        let jumps = vec![rec(2, "b1"), rec(3, "b2")];
        let hops = build_hops(None, &jumps, &|_| Some(pw("x")));
        match (&hops[0], &hops[1]) {
            (Hop::SshJump { host: a, .. }, Hop::SshJump { host: b, .. }) => {
                assert_eq!((a.as_str(), b.as_str()), ("b1", "b2"));
            }
            other => panic!("应为两个 SshJump,实际: {other:?}"),
        }
    }

    /// 跳板的凭据取自**跳板自己那条会话**的 secret,不是目标会话的。
    #[test]
    fn jump_credentials_come_from_the_jump_session_not_the_target() {
        let jumps = vec![rec(2, "bastion")];
        let hops = build_hops(None, &jumps, &|id| {
            assert_eq!(id, SessionId(2), "应查跳板会话的 secret");
            Some(pw("bastion-pw"))
        });
        match &hops[0] {
            Hop::SshJump { auth, .. } => {
                assert!(matches!(auth, AuthMethod::Password(p) if p == "bastion-pw"));
            }
            other => panic!("实际: {other:?}"),
        }
    }

    /// 跳板会话没存密码 → 退回 agent,而不是拿空串去认证(那必然 AuthFailed 且信息误导)。
    #[test]
    fn jump_without_stored_password_falls_back_to_agent() {
        let jumps = vec![rec(2, "bastion")];
        let hops = build_hops(None, &jumps, &|_| None);
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
        };
        let hops = build_hops_with_proxy_secret(Some(&proxy), &[], &|_| None, Some(&secret));
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
        let hops = build_hops_with_proxy_secret(Some(&proxy), &[], &|_| None, None);
        match &hops[0] {
            Hop::HttpConnect { auth, .. } => assert!(auth.is_none()),
            other => panic!("实际: {other:?}"),
        }
    }
}
