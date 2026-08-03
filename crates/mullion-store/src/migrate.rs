//! v1(扁平) → v2(分节)的一次性迁移。
//!
//! v1 结构在此**独立定义**并冻结:不能靠 v2 的 `SessionRecord` 去读旧文件,
//! 因为旧结构除 `note` 外均无 `#[serde(default)]`,缺字段即解析失败。

use serde::Deserialize;

use crate::error::StoreError;
use crate::model::{
    AppearancePrefs, Auth, AuthKind, Connection, Identity, Protocol, SessionId, SessionRecord,
    SessionsFile, TerminalPrefs, CURRENT_SCHEMA,
};

/// v1 的一条会话。**冻结,不要再改**。
#[derive(Debug, Deserialize)]
struct V1Record {
    id: SessionId,
    name: String,
    host: String,
    port: u16,
    protocol: Protocol,
    user: String,
    #[serde(default)]
    note: String,
    modified_at: String,
    auth: AuthKind,
}

/// v1 的顶层文件。
#[derive(Debug, Deserialize)]
struct V1File {
    #[serde(default)]
    session: Vec<V1Record>,
}

/// 只探测版本号,不解析其余内容(未知字段被 serde 忽略)。
#[derive(Debug, Deserialize)]
pub struct SchemaProbe {
    #[serde(default = "one")]
    pub schema_version: u32,
}

fn one() -> u32 {
    1
}

/// 把 v1 文本迁移成 v2 结构。分组为空(v1 没有分组概念),
/// 可继承分节全部留 `None` —— 即「继承/未设置」,行为与迁移前一致。
pub fn migrate_v1(text: &str) -> Result<SessionsFile, StoreError> {
    // 显式转成 Migration,不借 `?`/`From<toml::de::Error>` 自动变成 TomlDe——
    // 后者的文案「文件可能被手改坏」会把用户引去查语法,而这里失败大多是
    // 版本/结构不兼容(缺字段等),不是语法问题。
    let old: V1File = toml::from_str(text).map_err(|e| StoreError::Migration(e.to_string()))?;
    let session = old
        .session
        .into_iter()
        .map(|r| SessionRecord {
            id: r.id,
            modified_at: r.modified_at,
            identity: Identity {
                name: r.name,
                note: r.note,
                group_id: None,
                tags: Vec::new(),
            },
            connection: Connection {
                host: r.host,
                port: r.port,
                protocol: r.protocol,
            },
            auth: Auth {
                user: r.user,
                kind: r.auth,
            },
            terminal: TerminalPrefs::default(),
            appearance: AppearancePrefs::default(),
            network: crate::network::NetworkPrefs::default(),
        })
        .collect();
    Ok(SessionsFile {
        schema_version: CURRENT_SCHEMA,
        group: Vec::new(),
        session,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// v0.1.12 真实写出的格式。
    const V1_TEXT: &str = r#"
[[session]]
id = 7
name = "dev"
host = "192.0.2.10"
port = 22
protocol = "ssh"
user = "user"
note = "跳板后"
modified_at = "2026-07-25T00:00:00Z"

[session.auth]
kind = "public_key"
path = "/path/to/key.pem"
has_passphrase = false
"#;

    #[test]
    fn migrate_preserves_every_v1_field() {
        let out = migrate_v1(V1_TEXT).unwrap();
        assert_eq!(out.schema_version, CURRENT_SCHEMA);
        assert_eq!(out.session.len(), 1);
        let s = &out.session[0];
        assert_eq!(s.id, SessionId(7));
        assert_eq!(s.identity.name, "dev");
        assert_eq!(s.identity.note, "跳板后");
        assert_eq!(s.connection.host, "192.0.2.10");
        assert_eq!(s.connection.port, 22);
        assert_eq!(s.connection.protocol, Protocol::Ssh);
        assert_eq!(s.auth.user, "user");
        assert_eq!(s.modified_at, "2026-07-25T00:00:00Z");
        assert!(matches!(
            &s.auth.kind,
            AuthKind::PublicKey {
                has_passphrase: false,
                ..
            }
        ));
    }

    #[test]
    fn migrated_prefs_are_all_unset_so_behavior_is_unchanged() {
        let out = migrate_v1(V1_TEXT).unwrap();
        let s = &out.session[0];
        assert_eq!(
            s.terminal,
            TerminalPrefs::default(),
            "迁移不得凭空写入偏好值"
        );
        assert_eq!(s.appearance, AppearancePrefs::default());
        assert!(s.identity.group_id.is_none());
        assert!(s.identity.tags.is_empty());
        assert!(out.group.is_empty());
    }

    #[test]
    fn migrated_file_round_trips_as_v2() {
        let out = migrate_v1(V1_TEXT).unwrap();
        let text = toml::to_string_pretty(&out).unwrap();
        let back: SessionsFile = toml::from_str(&text).unwrap();
        assert_eq!(back, out, "迁移产物必须能按 v2 原样读回");
    }

    #[test]
    fn probe_reads_version_without_full_parse() {
        let p: SchemaProbe = toml::from_str(V1_TEXT).unwrap();
        assert_eq!(p.schema_version, 1, "缺键视为 v1");
        let p2: SchemaProbe = toml::from_str("schema_version = 2").unwrap();
        assert_eq!(p2.schema_version, 2);
    }

    #[test]
    fn empty_v1_file_migrates_to_empty_v2() {
        let out = migrate_v1("").unwrap();
        assert!(out.session.is_empty());
        assert_eq!(out.schema_version, CURRENT_SCHEMA);
    }

    /// v2 文件不含 network 分节,新字段全带 `#[serde(default)]`,应能直接读成 v3 结构。
    #[test]
    fn v2_file_reads_into_current_structs_without_network_section() {
        let text = r#"
schema_version = 2

[[session]]
id = 1
modified_at = "t"

[session.identity]
name = "a"

[session.connection]
host = "h"
port = 22
protocol = "ssh"

[session.auth]
user = "u"
kind = "password"
"#;
        let file: crate::model::SessionsFile = toml::from_str(text).unwrap();
        assert_eq!(file.session.len(), 1);
        assert_eq!(
            file.session[0].network,
            crate::network::NetworkPrefs::default(),
            "缺 network 分节应落默认(全继承)"
        );
    }

    /// 升 v3 的真正理由不是迁移,而是让 v0.1.14 那样的旧客户端**明确拒绝**——
    /// 否则旧客户端读到 `[session.network]` 会静默丢弃再写回,用户的代理配置无声消失。
    #[test]
    fn current_schema_is_three() {
        assert_eq!(crate::model::CURRENT_SCHEMA, 3);
    }
}
