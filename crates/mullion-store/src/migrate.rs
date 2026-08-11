//! v1(扁平) → v2(分节)的一次性迁移,以及 v<5 的私钥路径抽取。
//!
//! v1 结构在此**独立定义**并冻结:不能靠 v2 的 `SessionRecord` 去读旧文件,
//! 因为旧结构除 `note` 外均无 `#[serde(default)]`,缺字段即解析失败。

use std::collections::BTreeMap;
use std::path::PathBuf;

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

/// v<5 的 `[session.auth].path`。**冻结**:只挑私钥路径,其余字段一律由 serde
/// 忽略,所以同一份结构对 v1(扁平)和 v2~v4(分节)都成立 —— 两种布局里
/// `[[session]]` 都有 `id`,私钥路径都在 `[session.auth].path`。
#[derive(Debug, Deserialize)]
struct LegacyKeyFile {
    #[serde(default)]
    session: Vec<LegacyKeyRow>,
}

#[derive(Debug, Deserialize)]
struct LegacyKeyRow {
    id: SessionId,
    #[serde(default)]
    auth: LegacyKeyAuth,
}

#[derive(Debug, Default, Deserialize)]
struct LegacyKeyAuth {
    #[serde(default)]
    path: Option<PathBuf>,
}

/// 从 v<5 的 sessions.toml 文本里挑出「会话 id → 私钥路径」。
///
/// 单独抽一个**最小**结构去读,而不是在 `AuthKind` 上挂一个只写不读的遗留字段:
/// 后者会让 v5 之后每一处构造 `AuthKind::PublicKey` 的代码都得写一个恒为 `None`
/// 的字段,而这个字段的唯一用途在打开库的那一瞬间就结束了。
///
/// 解析失败(文件本就坏)时返回空表而不是报错:真正的解析错误由主路径
/// (`migrate_v1` / `SessionsFile`)报出,这里再报一次只会盖掉更准的那条。
pub fn legacy_key_paths(text: &str) -> BTreeMap<SessionId, PathBuf> {
    let Ok(f) = toml::from_str::<LegacyKeyFile>(text) else {
        return BTreeMap::new();
    };
    f.session
        .into_iter()
        .filter_map(|r| r.auth.path.map(|p| (r.id, p)))
        .collect()
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
            automation: crate::automation::AutomationPrefs::default(),
        })
        .collect();
    Ok(SessionsFile {
        schema_version: CURRENT_SCHEMA,
        group: Vec::new(),
        session,
        // v1 文件里不存在隧道概念,空数组是**永久**正确的。
        tunnel: Vec::new(),
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
        assert_eq!(
            s.automation,
            crate::automation::AutomationPrefs::default(),
            "迁移不得凭空写入自动化配置"
        );
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

    /// 版本号是**故意**钉死的:动它就意味着用户的库要迁移一次,不该被
    /// 顺手改掉。v7 的理由见 `CURRENT_SCHEMA` 文档 —— 新增 `[[tunnel]]`,
    /// 旧客户端读 v7 会把整个隧道数组丢掉再写回,拒绝比静默吃掉好。
    #[test]
    fn current_schema_is_seven() {
        assert_eq!(crate::model::CURRENT_SCHEMA, 7);
    }

    /// v5 的库里存的 emoji 图标**必须原样读得出来**。
    ///
    /// v6 的 UI 不再产出 emoji(epaint 画不了彩色字形,64px 纯图标档下认不出
    /// 是哪台机器),但那是「不再提供这个功能」,不是「可以把用户已经存下的
    /// 数据吃掉」。`IconKind::Emoji` 变体因此必须留在类型里 —— 删掉它,这份
    /// TOML 会整片反序列化失败,用户丢的是**整个会话库**而不是一个图标。
    ///
    /// 自证会变红:把 `IconKind` 里的 `Emoji` 变体删掉,这条编译不过。
    #[test]
    fn a_v5_library_with_emoji_icons_still_loads_on_v6() {
        let v5 = r#"
schema_version = 5

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

[session.appearance.icon]
kind = "emoji"
value = "🔥"
"#;
        let file: crate::model::SessionsFile = toml::from_str(v5).unwrap();
        let icon = file.session[0]
            .appearance
            .icon
            .as_ref()
            .expect("图标不该丢");
        assert_eq!(icon.kind, crate::model::IconKind::Emoji);
        assert_eq!(icon.value, "🔥");
        assert_eq!(icon.bg, None, "v5 没有底色这个键,应落 None 而不是解析失败");
    }

    /// v6 的库升 v7 **不需要任何迁移代码** —— 这条是来证明它确实自动成立的,
    /// 不是在测某个迁移函数。
    ///
    /// 机制:`SessionsFile.tunnel` 带 `#[serde(default)]`,缺键补空数组;
    /// `load_sessions` 见 `probe.schema_version < CURRENT_SCHEMA` 会先备份
    /// `.toml.bak` 再按新结构解析。风险不在「隧道读不出来」(v6 本就没有),
    /// 而在**既有字段被顺手弄丢** —— 所以这里逐一断言。
    #[test]
    fn v6_file_without_tunnel_key_loads_as_empty_and_keeps_everything_else() {
        let v6 = r#"
schema_version = 6

[[group]]
id = 2
name = "生产"

[[session]]
id = 7
modified_at = "2026-08-01T00:00:00Z"

[session.identity]
name = "db"
note = "主库"
group_id = 2
tags = ["prod"]

[session.connection]
host = "192.0.2.10"
port = 2222
protocol = "ssh"

[session.auth]
user = "ops"
kind = "public_key"
has_passphrase = true

[session.terminal]
scrollback = 5000

[session.network.proxy]
kind = "socks5"
host = "127.0.0.1"
port = 7891

[session.automation.tmux]
kind = "attach"
session_name = "claude"
"#;
        let file: crate::model::SessionsFile = toml::from_str(v6).unwrap();

        assert!(
            file.tunnel.is_empty(),
            "v6 没有隧道,应补成空数组而非解析失败"
        );

        let s = &file.session[0];
        assert_eq!(s.id, crate::model::SessionId(7));
        assert_eq!(s.modified_at, "2026-08-01T00:00:00Z");
        assert_eq!(s.identity.name, "db");
        assert_eq!(s.identity.note, "主库");
        assert_eq!(s.identity.group_id, Some(crate::model::GroupId(2)));
        assert_eq!(s.identity.tags, vec!["prod".to_string()]);
        assert_eq!(s.connection.host, "192.0.2.10");
        assert_eq!(s.connection.port, 2222);
        assert_eq!(s.auth.user, "ops");
        assert_eq!(
            s.auth.kind,
            crate::model::AuthKind::PublicKey {
                has_passphrase: true
            }
        );
        assert_eq!(s.terminal.scrollback, Some(5000));
        assert_eq!(
            s.network.proxy,
            Some(crate::network::ProxyChoice::Socks5(
                crate::network::ProxyEndpoint {
                    host: "127.0.0.1".into(),
                    port: 7891,
                    user: None,
                }
            )),
            "代理分节不该在升版本时丢掉"
        );
        assert_eq!(
            s.automation.tmux,
            Some(crate::automation::TmuxChoice::Attach {
                session_name: Some("claude".into())
            }),
            "自动化分节不该在升版本时丢掉"
        );
        assert_eq!(file.group.len(), 1, "分组不该在升版本时丢掉");
        assert_eq!(file.group[0].name, "生产");
    }

    /// 反过来:v6 存的 .ico 图标要能完整往返,底色跟着走。
    #[test]
    fn an_ico_icon_and_its_background_colour_round_trip() {
        let spec = crate::model::IconSpec {
            kind: crate::model::IconKind::Ico,
            value: "QUJD".into(),
            bg: Some("#1e88e5".into()),
        };
        let text = toml::to_string(&spec).unwrap();
        assert_eq!(
            toml::from_str::<crate::model::IconSpec>(&text).unwrap(),
            spec
        );

        // 没垫底色时不该往 TOML 里写一行空的 `bg`。
        let plain = crate::model::IconSpec {
            bg: None,
            ..spec.clone()
        };
        assert!(
            !toml::to_string(&plain).unwrap().contains("bg"),
            "没设底色就不该写这个键"
        );
    }

    /// 私钥路径必须能从 **v1(扁平)和 v2~v4(分节)两种布局**里都挑出来 ——
    /// 抽取发生在解析成 v5 结构**之前**,而 v1 文件走的是另一条(`migrate_v1`)
    /// 路径。只覆盖一种布局的话,另一种布局的用户升级后私钥会静默丢失。
    #[test]
    fn legacy_key_paths_reads_both_the_flat_v1_and_the_nested_v2_layouts() {
        let v1 = legacy_key_paths(V1_TEXT);
        assert_eq!(
            v1.get(&SessionId(7)).map(|p| p.display().to_string()),
            Some("/path/to/key.pem".to_string()),
            "v1 扁平布局的私钥路径没被挑出来"
        );

        let v4 = r#"
schema_version = 4

[[session]]
id = 3
modified_at = "t"

[session.identity]
name = "a"

[session.connection]
host = "h"
port = 22
protocol = "ssh"

[session.auth]
user = "u"
kind = "public_key"
path = "/home/u/.ssh/id_ed25519"
has_passphrase = true
"#;
        assert_eq!(
            legacy_key_paths(v4)
                .get(&SessionId(3))
                .map(|p| p.display().to_string()),
            Some("/home/u/.ssh/id_ed25519".to_string()),
            "v2~v4 分节布局的私钥路径没被挑出来"
        );
    }

    /// 密码会话没有 `path`,不该在表里凭空多出一条 —— 多出来会让
    /// `Vault::open` 去读一个不存在的路径,并给密码会话建一条空 secret。
    #[test]
    fn legacy_key_paths_skips_sessions_without_a_key_path() {
        let text = r#"
schema_version = 4

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
        assert!(
            legacy_key_paths(text).is_empty(),
            "密码会话不该产生私钥路径条目"
        );
    }
}
