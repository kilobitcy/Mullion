//! F349:手机端「会话记录 → `SshConfig`」映射。
//!
//! 桌面那份在 `mullion-app/src/shell/{session_map,dial_plan}.rs`,ffi 不能依赖 app,
//! 而且手机语义本就不同:**没有 ssh-agent**。桌面在跳板缺密文时退回 Agent,
//! 手机上那等于拿一个不存在的 agent 去谈,报出来的是一条指不到原因的认证失败 ——
//! 所以这里目标与跳板一律「缺就报错、点名是哪一跳」。

use mullion_ssh::config::{AuthMethod, SshConfig};
use mullion_ssh::hop::Hop;
use mullion_store::{
    AuthKind, Protocol, ProxyChoice, ResolvedAuth, SessionId, SessionRecord, Vault,
};

/// 节点 `id` 的完整拨号参数。`Err` 是给人看的原因(已点名节点/跳板)。
pub(crate) fn dial_config(
    vault: &Vault,
    id: SessionId,
    cols: u16,
    rows: u16,
) -> Result<SshConfig, String> {
    let rec = vault
        .get(id)
        .ok_or_else(|| format!("节点 {} 不在本机数据里,请重新导入", id.0))?;
    let name = &rec.identity.name;
    if rec.connection.protocol == Protocol::Sftp {
        return Err(format!("「{name}」是 SFTP 节点,不能开终端"));
    }
    let auth = vault
        .resolve_auth(rec)
        .map_err(|e| format!("「{name}」的认证解析失败:{e}"))?;
    let method = auth_method(&auth).map_err(|why| format!("「{name}」{why}"))?;

    let resolved = vault
        .resolve_for(id)
        .map_err(|e| format!("「{name}」的网络配置解析失败:{e}"))?;
    let chain = vault
        .expand_jump_chain(id)
        .map_err(|e| format!("「{name}」的跳板链解析失败:{e}"))?;

    let mut hops = Vec::new();
    // 代理排在最前:先出本机网络,才谈得上连跳板。口令只存目标会话级(桌面设计 D4)。
    let proxy_pw = vault.secret(id).and_then(|s| s.proxy_password.clone());
    match &resolved.proxy {
        None | Some(ProxyChoice::Direct) => {}
        Some(ProxyChoice::Socks5(ep)) => hops.push(Hop::Socks5 {
            host: ep.host.clone(),
            port: ep.port,
            auth: pair(ep.user.as_deref(), proxy_pw),
        }),
        Some(ProxyChoice::HttpConnect(ep)) => hops.push(Hop::HttpConnect {
            host: ep.host.clone(),
            port: ep.port,
            auth: pair(ep.user.as_deref(), proxy_pw),
        }),
    }
    for j in &chain {
        hops.push(jump_hop(vault, j)?);
    }

    Ok(SshConfig {
        host: rec.connection.host.clone(),
        port: rec.connection.port,
        user: auth.user.clone(),
        auth: method,
        cols,
        rows,
        // F211/T2':必须与 sync 登记里按 TERM 匹配的那个名字同源。
        term: mullion_store::automation::SYNC_TERM.to_string(),
        hops,
    })
}

/// F304:这条路线经不经代理/跳板(桌面 `AppearanceCache::is_routed` 同判据)。
/// 解析失败按「经路由」处置:排到后面,轮到它时再由 [`dial_config`] 报出原因。
pub(crate) fn is_routed(vault: &Vault, id: SessionId) -> bool {
    match vault.resolve_for(id) {
        Ok(r) => !r.jump.is_empty() || !matches!(r.proxy, None | Some(ProxyChoice::Direct)),
        Err(_) => true,
    }
}

fn jump_hop(vault: &Vault, rec: &SessionRecord) -> Result<Hop, String> {
    let name = &rec.identity.name;
    let auth = vault
        .resolve_auth(rec)
        .map_err(|e| format!("跳板「{name}」的认证解析失败:{e}"))?;
    let method = auth_method(&auth).map_err(|why| format!("跳板「{name}」{why}"))?;
    Ok(Hop::SshJump {
        host: rec.connection.host.clone(),
        port: rec.connection.port,
        user: auth.user,
        auth: method,
    })
}

/// 已解析身份 → 认证方式。手机没有 agent:缺什么就说缺什么。
fn auth_method(auth: &ResolvedAuth) -> Result<AuthMethod, &'static str> {
    let secret = auth.secret.as_ref();
    match &auth.kind {
        AuthKind::Password => secret
            .and_then(|s| s.password.clone())
            .map(AuthMethod::Password)
            .ok_or("缺少密码(包里没带,请在桌面补上后重新导出)"),
        AuthKind::PublicKey { has_passphrase } => {
            let key_data = secret
                .and_then(|s| s.private_key.clone())
                .ok_or("缺少私钥(包里没带,请在桌面重新导入私钥后再导出)")?;
            let passphrase = if *has_passphrase {
                Some(
                    secret
                        .and_then(|s| s.passphrase.clone())
                        .ok_or("缺少私钥口令(包里没带)")?,
                )
            } else {
                None
            };
            Ok(AuthMethod::PublicKey {
                key_data,
                passphrase,
            })
        }
    }
}

/// 用户名与口令必须成对才发认证(桌面同规则)。
fn pair(user: Option<&str>, pw: Option<String>) -> Option<(String, String)> {
    match (user, pw) {
        (Some(u), Some(p)) => Some((u.to_string(), p)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_store::{AuthKind, SecretEntry};

    fn resolved(kind: AuthKind, secret: Option<SecretEntry>) -> ResolvedAuth {
        ResolvedAuth {
            user: "u".into(),
            kind,
            secret,
        }
    }

    #[test]
    fn password_maps_and_missing_password_is_an_error_not_agent() {
        let ok = resolved(
            AuthKind::Password,
            Some(SecretEntry {
                password: Some("p".into()),
                ..Default::default()
            }),
        );
        assert!(matches!(auth_method(&ok), Ok(AuthMethod::Password(p)) if p == "p"));
        // 手机没有 agent:桌面跳板在这里会退回 Agent,手机必须报错。
        assert!(auth_method(&resolved(AuthKind::Password, None)).is_err());
    }

    #[test]
    fn private_key_needs_its_passphrase_when_it_has_one() {
        let with_key = |pp: Option<&str>| {
            Some(SecretEntry {
                private_key: Some("KEY".into()),
                passphrase: pp.map(Into::into),
                ..Default::default()
            })
        };
        let k = AuthKind::PublicKey {
            has_passphrase: true,
        };
        assert!(matches!(
            auth_method(&resolved(k.clone(), with_key(Some("pp")))),
            Ok(AuthMethod::PublicKey { key_data, passphrase: Some(p) }) if key_data == "KEY" && p == "pp"
        ));
        assert!(auth_method(&resolved(k, with_key(None))).is_err());
        let plain = AuthKind::PublicKey {
            has_passphrase: false,
        };
        assert!(matches!(
            auth_method(&resolved(plain.clone(), with_key(None))),
            Ok(AuthMethod::PublicKey {
                passphrase: None,
                ..
            })
        ));
        assert!(auth_method(&resolved(plain, None)).is_err());
    }

    #[test]
    fn proxy_auth_is_sent_only_as_a_pair() {
        assert_eq!(
            pair(Some("u"), Some("p".into())),
            Some(("u".into(), "p".into()))
        );
        assert_eq!(pair(Some("u"), None), None);
        assert_eq!(pair(None, Some("p".into())), None);
    }
}
