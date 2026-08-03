//! F4 网络代理 / F5 跳板链的数据模型(设计 §3.1)。零 IO,可纯单测。

use serde::{Deserialize, Serialize};

use crate::model::SessionId;

/// 网络路径偏好(可继承分节)。
///
/// **与 `Connection` 分开**:`Connection` 是「会话身份本身」永不可继承,
/// 而「怎么走到它」恰恰最该按分组继承(设计 §3.1 的自审修正)。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkPrefs {
    /// F4 网络代理。`None` = 继承上游;`Some(Direct)` = 显式不走代理。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyChoice>,
    /// F5 跳板链,按拨号顺序:`[0]` 最先连。
    /// `None` = 继承上游;`Some(vec![])` = 显式直连。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jump: Option<Vec<JumpRef>>,
}

/// 代理选择。`Direct` 是**显式**不走代理,用来覆盖分组的代理设置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProxyChoice {
    Direct,
    Socks5(ProxyEndpoint),
    HttpConnect(ProxyEndpoint),
}

/// 代理端点的**非敏感**部分。口令在 `SecretEntry::proxy_password`(加密)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyEndpoint {
    pub host: String,
    pub port: u16,
    /// 认证用户名;`None` = 免认证代理。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}

/// 对另一条会话的引用,用作跳板。
///
/// 跳板复用「会话」这一实体(设计 D2):跳板机本身也是一台要维护凭据的机器,
/// 没必要另起一套只有主机/用户/密钥的半吊子模型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct JumpRef(pub SessionId);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_prefs_round_trips() {
        let n = NetworkPrefs {
            proxy: Some(ProxyChoice::Socks5(ProxyEndpoint {
                host: "127.0.0.1".into(),
                port: 7891,
                user: Some("u".into()),
            })),
            jump: Some(vec![JumpRef(SessionId(3)), JumpRef(SessionId(5))]),
        };
        let s = toml::to_string_pretty(&n).unwrap();
        let back: NetworkPrefs = toml::from_str(&s).unwrap();
        assert_eq!(back, n);
    }

    #[test]
    fn unset_fields_are_not_written() {
        let s = toml::to_string_pretty(&NetworkPrefs::default()).unwrap();
        assert_eq!(s.trim(), "", "全未设的分节不应写出任何键");
    }

    /// 设计 §3.2 的核心区分:`None` = 继承上游,`Some(Direct)` / `Some(vec![])` = 显式覆盖成「不走」。
    #[test]
    fn explicit_empty_is_distinguishable_from_inherit() {
        let inherit = NetworkPrefs::default();
        let explicit = NetworkPrefs {
            proxy: Some(ProxyChoice::Direct),
            jump: Some(Vec::new()),
        };
        assert_ne!(inherit, explicit, "「继承」与「显式直连」不是同一个值");

        let s = toml::to_string_pretty(&explicit).unwrap();
        let back: NetworkPrefs = toml::from_str(&s).unwrap();
        assert_eq!(back, explicit, "显式直连必须能 round-trip,不能被写没");
        assert_eq!(back.jump, Some(Vec::new()));
    }

    #[test]
    fn proxy_kind_is_tagged_not_positional() {
        let p = ProxyChoice::HttpConnect(ProxyEndpoint {
            host: "proxy.local".into(),
            port: 8080,
            user: None,
        });
        let s = toml::to_string_pretty(&p).unwrap();
        assert!(
            s.contains(r#"kind = "http_connect""#),
            "应有 kind 标签: {s}"
        );
        assert!(!s.contains("user"), "None 的 user 不应写出: {s}");
    }
}
