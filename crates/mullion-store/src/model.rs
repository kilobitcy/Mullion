//! 会话数据模型。只放数据类型,零 IO。非敏感字段落明文 TOML;密码/口令走加密侧车(vault)。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// 会话稳定主键。新建时取现有 max+1(见 vault)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SessionId(pub u64);

/// 会话协议。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Ssh,
    Sftp,
}

/// 认证方式的**非敏感**部分。真正的密码/口令在 `SecretEntry`(加密)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthKind {
    /// 密码认证:密码串存加密侧车。
    Password,
    /// 公钥认证:私钥 path 明文;口令(若有)存加密侧车。
    PublicKey { path: PathBuf, has_passphrase: bool },
}

/// 一条会话(非敏感字段)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: SessionId,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub protocol: Protocol,
    pub user: String,
    #[serde(default)]
    pub note: String,
    /// RFC3339;由调用方(app)注入,store 不持有时钟。
    pub modified_at: String,
    pub auth: AuthKind,
}

/// 一条会话的**敏感**部分,加密后存 secrets.enc。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretEntry {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub password: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub passphrase: Option<String>,
}

/// sessions.toml 的顶层结构:产生 `[[session]]` 数组。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionsFile {
    #[serde(default)]
    pub session: Vec<SessionRecord>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_toml_round_trips() {
        let rec = SessionRecord {
            id: SessionId(7),
            name: "dev".into(),
            host: "192.0.2.10".into(),
            port: 22,
            protocol: Protocol::Ssh,
            user: "user".into(),
            note: "跳板后".into(),
            modified_at: "2026-07-25T00:00:00Z".into(),
            auth: AuthKind::PublicKey {
                path: "/path/to/key.pem".into(),
                has_passphrase: false,
            },
        };
        let file = SessionsFile {
            session: vec![rec.clone()],
        };
        let s = toml::to_string_pretty(&file).unwrap();
        let back: SessionsFile = toml::from_str(&s).unwrap();
        assert_eq!(back.session, vec![rec]);
    }

    #[test]
    fn empty_toml_parses_to_no_sessions() {
        let back: SessionsFile = toml::from_str("").unwrap();
        assert!(back.session.is_empty(), "空文件应解析为零会话,不报错");
    }
}
