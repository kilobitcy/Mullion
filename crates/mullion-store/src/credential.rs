//! 凭据实体(F74)。一份凭据可被多条会话引用,换密钥改一处。
//!
//! 设计见 `docs/superpowers/specs/2026-08-13-credentials-design.md`。
//! 只放数据类型与编码,零 IO —— 增删改查、引用完整性、解析都在 `vault`。

use serde::{Deserialize, Serialize};

use crate::model::AuthKind;

/// 凭据稳定主键。新建时取现有 max+1(见 vault),**与 `SessionId` / `TunnelId`
/// 各自独立编号**。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CredentialId(pub u64);

/// 一份可被多条会话引用的凭据(用户名 + 认证方式 + 侧车里的密码/私钥/口令)。
///
/// **不含主机、不含代理**:那些是「连到哪」,凭据只回答「以谁的身份」。
/// 代理口令留在会话自己的密文里(设计 D4)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialRecord {
    pub id: CredentialId,
    /// 显示名。**不要求唯一** —— 主键是 `id`,拿名字当主键会让改名变成换身份。
    pub name: String,
    pub user: String,
    /// 平铺进 `[[credential]]`,与 `[session.auth]` 的写法一致。
    #[serde(flatten)]
    pub kind: AuthKind,
}

/// 会话自带的那份认证(F74 之前 `Auth` 就是这个形状)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineAuth {
    pub user: String,
    pub kind: AuthKind,
}

/// 认证来源:**严格二选一**(设计 D1)。
///
/// 不做「引用 + 局部覆盖」:两个真值同时存在,「这台机器到底用的哪个用户名」
/// 就得靠追查,而那正是本功能要消灭的东西。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Auth {
    /// 本会话独有。
    Inline(InlineAuth),
    /// 引用共享凭据。悬空时**必须报错**,见 `Vault::resolve_auth`。
    Ref(CredentialId),
}

impl Auth {
    /// 便捷构造:本会话独有。测试与迁移里到处要用。
    pub fn inline(user: impl Into<String>, kind: AuthKind) -> Self {
        Auth::Inline(InlineAuth {
            user: user.into(),
            kind,
        })
    }

    /// 本会话独有的那份认证;引用共享凭据时为 `None`。
    ///
    /// **不要拿它当「取用户名」的通用入口** —— 引用凭据的会话在这里拿到的是
    /// `None`,真正的用户名要走 `Vault::resolve_auth`(那里才查得到凭据表)。
    pub fn as_inline(&self) -> Option<&InlineAuth> {
        match self {
            Auth::Inline(i) => Some(i),
            Auth::Ref(_) => None,
        }
    }

    /// 本会话独有的那份认证的可变引用;引用共享凭据时为 `None`。
    pub fn as_inline_mut(&mut self) -> Option<&mut InlineAuth> {
        match self {
            Auth::Inline(i) => Some(i),
            Auth::Ref(_) => None,
        }
    }

    /// 引用的凭据 id;本会话独有时为 `None`。
    pub fn credential_id(&self) -> Option<CredentialId> {
        match self {
            Auth::Inline(_) => None,
            Auth::Ref(id) => Some(*id),
        }
    }
}

/// 列表副标题、下拉项这类**只读显示**里该写的用户名。
///
/// 与 `Vault::resolve_auth` 分开是刻意的:那条路径要能失败(悬空引用绝不能
/// 拿去连接),而画一行字不该因为数据有问题就整片报错。悬空在这里退化成一个
/// **明说有问题**的占位符 —— 连接时仍会硬失败,用户不会因为看见占位符就以为
/// 还能连。
pub fn display_user<'a>(auth: &'a Auth, credentials: &'a [CredentialRecord]) -> &'a str {
    match auth {
        Auth::Inline(i) => &i.user,
        Auth::Ref(id) => credentials
            .iter()
            .find(|c| c.id == *id)
            .map_or("(凭据已删)", |c| c.user.as_str()),
    }
}

/// `[session.auth]` 的线上形状(设计 D2)。
///
/// **全是标量,没有嵌套 flatten**:`AuthKind` 是内部标签枚举且要平铺进同一张
/// table,再套一层内部标签枚举就撞上 toml 对该组合的已知限制(spec F74 验收 ①);
/// 而且内部标签枚举没有默认变体,v8 文件的 auth 分节里没有 `source` 键,加了标签
/// 就一律解析失败。所以这里把三种键手工摊平,`TryFrom` 双向转换。
#[derive(Serialize, Deserialize)]
struct AuthRepr {
    /// 缺省 = `inline`(v8 文件就是这样)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user: Option<String>,
    /// `AuthKind` 的标签:`"password"` / `"public_key"`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    has_passphrase: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    credential_id: Option<CredentialId>,
}

const SOURCE_REF: &str = "ref";
const SOURCE_INLINE: &str = "inline";

impl From<&Auth> for AuthRepr {
    fn from(a: &Auth) -> Self {
        match a {
            // Inline **不写 `source`**:auth 分节的字节与 v8 完全相同,
            // 升级后 diff 里只有真正改过的会话。
            Auth::Inline(i) => {
                let (kind, has_passphrase) = match i.kind {
                    AuthKind::Password => ("password", None),
                    AuthKind::PublicKey { has_passphrase } => ("public_key", Some(has_passphrase)),
                };
                AuthRepr {
                    source: None,
                    user: Some(i.user.clone()),
                    kind: Some(kind.to_string()),
                    has_passphrase,
                    credential_id: None,
                }
            }
            Auth::Ref(id) => AuthRepr {
                source: Some(SOURCE_REF.to_string()),
                user: None,
                kind: None,
                has_passphrase: None,
                credential_id: Some(*id),
            },
        }
    }
}

impl AuthRepr {
    /// **两个真值同时出现 = 数据坏了,直接失败**(设计 D2)。
    /// 静默取其中一个的后果是「用户以为在用 A 身份、实际在用 B」。
    fn into_auth(self) -> Result<Auth, String> {
        let is_ref = match self.source.as_deref() {
            None | Some(SOURCE_INLINE) => false,
            Some(SOURCE_REF) => true,
            Some(other) => return Err(format!("auth.source 只能是 inline 或 ref,实际 {other:?}")),
        };
        if is_ref {
            if self.user.is_some() || self.kind.is_some() {
                return Err(
                    "auth 同时给了 credential_id 与 user/kind —— 引用与自带认证只能二选一".into(),
                );
            }
            let id = self
                .credential_id
                .ok_or("auth.source = \"ref\" 却没有 credential_id")?;
            return Ok(Auth::Ref(id));
        }
        if self.credential_id.is_some() {
            return Err(
                "auth 带了 credential_id 却没写 source = \"ref\" —— 引用与自带认证只能二选一"
                    .into(),
            );
        }
        let user = self.user.ok_or("auth 缺少 user")?;
        let kind = match self.kind.as_deref() {
            Some("password") | None => AuthKind::Password,
            Some("public_key") => AuthKind::PublicKey {
                has_passphrase: self.has_passphrase.unwrap_or(false),
            },
            Some(other) => return Err(format!("auth.kind 不认识:{other:?}")),
        };
        Ok(Auth::Inline(InlineAuth { user, kind }))
    }
}

impl Serialize for Auth {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        AuthRepr::from(self).serialize(s)
    }
}

impl<'de> Deserialize<'de> for Auth {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        AuthRepr::deserialize(d)?
            .into_auth()
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// v8 的 auth 分节长这样(密码)。inline 编码**必须与它逐字节一致** ——
    /// 多写一个 `source = "inline"` 不影响功能,但会让升级后每条会话都出现
    /// 一行无意义的 diff,也让手改配置的人以为这是个必填键。
    #[test]
    fn inline_auth_serializes_exactly_like_v8() {
        let a = Auth::inline("ops", AuthKind::Password);
        let s = toml::to_string_pretty(&a).unwrap();
        assert_eq!(s.trim(), "user = \"ops\"\nkind = \"password\"", "实际:{s}");
    }

    #[test]
    fn inline_pubkey_round_trips_with_the_passphrase_flag() {
        let a = Auth::inline(
            "ops",
            AuthKind::PublicKey {
                has_passphrase: true,
            },
        );
        let s = toml::to_string_pretty(&a).unwrap();
        assert!(s.contains("has_passphrase = true"), "实际:{s}");
        assert_eq!(toml::from_str::<Auth>(&s).unwrap(), a);
    }

    /// v8 文件的 auth 分节里没有 `source` 键,必须读成 inline。
    /// 读不了就等于「新版本读不了旧文件」。
    #[test]
    fn a_v8_auth_section_without_source_reads_as_inline() {
        let a: Auth = toml::from_str("user = \"u\"\nkind = \"password\"").unwrap();
        assert_eq!(a, Auth::inline("u", AuthKind::Password));
    }

    #[test]
    fn ref_auth_round_trips() {
        let a = Auth::Ref(CredentialId(3));
        let s = toml::to_string_pretty(&a).unwrap();
        assert!(s.contains("source = \"ref\""), "实际:{s}");
        assert!(s.contains("credential_id = 3"), "实际:{s}");
        assert_eq!(toml::from_str::<Auth>(&s).unwrap(), a);
        assert_eq!(a.credential_id(), Some(CredentialId(3)));
    }

    /// 失效模式 5:两个真值并存 → 解析失败,不许静默取一个。
    /// 静默取一个的症状是「用户以为在用 A 身份、实际在用 B」。
    #[test]
    fn an_auth_section_with_both_shapes_is_rejected() {
        let both = "source = \"ref\"\ncredential_id = 1\nuser = \"u\"\nkind = \"password\"";
        assert!(
            toml::from_str::<Auth>(both).is_err(),
            "引用 + 自带认证并存必须报错"
        );
        // 反向:带了 credential_id 却没写 source,同样是自相矛盾。
        let sneaky = "credential_id = 1\nuser = \"u\"\nkind = \"password\"";
        assert!(toml::from_str::<Auth>(sneaky).is_err(), "实际被接受了");
    }

    #[test]
    fn a_ref_without_a_credential_id_is_rejected() {
        assert!(toml::from_str::<Auth>("source = \"ref\"").is_err());
    }

    #[test]
    fn an_unknown_source_is_rejected() {
        assert!(toml::from_str::<Auth>("source = \"whatever\"").is_err());
    }

    #[test]
    fn credential_record_flattens_its_kind() {
        let c = CredentialRecord {
            id: CredentialId(1),
            name: "运维私钥".into(),
            user: "ops".into(),
            kind: AuthKind::PublicKey {
                has_passphrase: true,
            },
        };
        let s = toml::to_string_pretty(&c).unwrap();
        assert!(s.contains("kind = \"public_key\""), "实际:{s}");
        assert!(!s.contains("[kind]"), "不该多出一层 table:{s}");
        assert_eq!(toml::from_str::<CredentialRecord>(&s).unwrap(), c);
    }
}
