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

/// 身份与组织(分节)。`name` 不可继承(设计 §4.2)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub name: String,
    #[serde(default)]
    pub note: String,
    /// 所属分组;`None` = 未分组(不参与继承)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<GroupId>,
    /// 标签。继承策略为 **Merge**(设计 §4.1)。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// 连接目标(分节)。**永不可继承**——连接目标是会话身份本身。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    pub host: String,
    pub port: u16,
    pub protocol: Protocol,
}

/// 认证(分节)。**永不可继承**。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Auth {
    pub user: String,
    /// 平铺进 `[session.auth]`,不额外产生一层 table。
    #[serde(flatten)]
    pub kind: AuthKind,
}

/// 一条会话(非敏感字段)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: SessionId,
    /// RFC3339;由调用方(app)注入,store 不持有时钟。
    pub modified_at: String,
    pub identity: Identity,
    pub connection: Connection,
    pub auth: Auth,
    #[serde(default)]
    pub terminal: TerminalPrefs,
    #[serde(default)]
    pub appearance: AppearancePrefs,
    #[serde(default)]
    pub network: crate::network::NetworkPrefs,
    #[serde(default)]
    pub automation: crate::automation::AutomationPrefs,
}

/// 一条会话的**敏感**部分,加密后存 secrets.enc。
///
/// **不 derive Debug**:三个字段全是明文口令,`{:?}` 一打就把它们写进日志/panic
/// 消息,加密存储的意义当场归零。手写打码实现(与 `mullion_ssh::hop` 同一模式),
/// 只报告「有没有设置」,连长度都不泄漏。
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretEntry {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub password: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub passphrase: Option<String>,
    /// F4:代理认证口令。与 SSH 口令分开存,避免误用。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub proxy_password: Option<String>,
}

impl std::fmt::Debug for SecretEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fn redacted(v: &Option<String>) -> &'static str {
            if v.is_some() {
                "<已设置>"
            } else {
                "<无>"
            }
        }
        f.debug_struct("SecretEntry")
            .field("password", &redacted(&self.password))
            .field("passphrase", &redacted(&self.passphrase))
            .field("proxy_password", &redacted(&self.proxy_password))
            .finish()
    }
}

/// 分组稳定主键。新建时取现有 max+1(见 vault)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct GroupId(pub u64);

/// 图标来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IconKind {
    /// 内置图标库中的名字(如 "ubuntu")。
    Builtin,
    /// 单个 emoji 字符。
    Emoji,
    /// 用户提供的图片路径。
    Custom,
}

/// 图标规格。**复合对象:只能整体继承或整体覆盖**(设计 §4.1)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IconSpec {
    pub kind: IconKind,
    pub value: String,
}

/// 颜色的作用范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorTarget {
    Tab,
    ListItem,
    PaneTitle,
    StatusBar,
}

/// 颜色规格。**复合对象:只能整体继承或整体覆盖**(设计 §4.1)——
/// 明确不支持「只覆盖 hex、沿用上游的 apply_to」这类字段级部分覆盖。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColorSpec {
    pub hex: String,
    #[serde(default)]
    pub apply_to: Vec<ColorTarget>,
}

/// 终端偏好(可继承分节)。字段一律 `Option`,`None` 即继承上游。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalPrefs {
    /// 滚动回溯行数(F17)。内置默认见 `inherit::DEFAULT_SCROLLBACK`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scrollback: Option<u32>,
}

/// 外观偏好(可继承分节)。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppearancePrefs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<IconSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<ColorSpec>,
}

/// 当前 TOML 结构版本。缺失该键的文件视为 v1(见 `migrate`)。
///
/// v3 = v2 + `[session.network]` / `[group.network]`。
/// v4 = v3 + `[session.automation]` / `[group.automation]`(F40~F44)。
///
/// 结构上新版本能直接读旧版本(新字段全带 `serde(default)`),升版本号是为了让
/// **旧客户端明确拒绝**,而不是静默丢弃新分节再写回。
///
/// **号段归属**:F74(凭据实体)原定 v3→v4,被本切片先落地拿走了 4,顺延为
/// v4→v5(规则「谁先落地谁拿号」,见 `spec.md` F74)。
pub const CURRENT_SCHEMA: u32 = 4;

fn schema_v1() -> u32 {
    1
}

/// sessions.toml 的顶层结构:产生 `[[group]]` 与 `[[session]]` 数组。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionsFile {
    /// 旧文件没有这个键 → 解析为 1 → 触发迁移。
    #[serde(default = "schema_v1")]
    pub schema_version: u32,
    #[serde(default)]
    pub group: Vec<crate::group::GroupRecord>,
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
            modified_at: "2026-07-25T00:00:00Z".into(),
            identity: Identity {
                name: "dev".into(),
                note: "跳板后".into(),
                group_id: Some(GroupId(2)),
                tags: vec!["prod".into()],
            },
            connection: Connection {
                host: "192.0.2.10".into(),
                port: 22,
                protocol: Protocol::Ssh,
            },
            auth: Auth {
                user: "user".into(),
                kind: AuthKind::PublicKey {
                    path: "/path/to/key.pem".into(),
                    has_passphrase: false,
                },
            },
            terminal: TerminalPrefs {
                scrollback: Some(5000),
            },
            appearance: AppearancePrefs::default(),
            network: crate::network::NetworkPrefs::default(),
            automation: crate::automation::AutomationPrefs::default(),
        };
        let file = SessionsFile {
            schema_version: CURRENT_SCHEMA,
            group: Vec::new(),
            session: vec![rec.clone()],
        };
        let s = toml::to_string_pretty(&file).unwrap();
        let back: SessionsFile = toml::from_str(&s).unwrap();
        assert_eq!(back.session, vec![rec]);
    }

    #[test]
    fn auth_kind_flattens_into_auth_section() {
        let rec = SessionRecord {
            id: SessionId(1),
            modified_at: "t".into(),
            identity: Identity {
                name: "a".into(),
                note: String::new(),
                group_id: None,
                tags: Vec::new(),
            },
            connection: Connection {
                host: "h".into(),
                port: 22,
                protocol: Protocol::Ssh,
            },
            auth: Auth {
                user: "u".into(),
                kind: AuthKind::Password,
            },
            terminal: TerminalPrefs::default(),
            appearance: AppearancePrefs::default(),
            network: crate::network::NetworkPrefs::default(),
            automation: crate::automation::AutomationPrefs::default(),
        };
        let file = SessionsFile {
            schema_version: CURRENT_SCHEMA,
            group: Vec::new(),
            session: vec![rec],
        };
        let s = toml::to_string_pretty(&file).unwrap();
        assert!(s.contains("[session.auth]"), "应有 auth 分节: {s}");
        assert!(
            s.contains(r#"kind = "password""#),
            "kind 应平铺进 auth 分节: {s}"
        );
        assert!(
            !s.contains("[session.auth.kind]"),
            "不应多出一层 table: {s}"
        );
    }

    #[test]
    fn empty_toml_parses_to_no_sessions() {
        let back: SessionsFile = toml::from_str("").unwrap();
        assert!(back.session.is_empty(), "空文件应解析为零会话,不报错");
        assert!(back.group.is_empty());
        assert_eq!(back.schema_version, 1, "缺 schema_version 的文件视为 v1");
    }

    #[test]
    fn prefs_sections_skip_none_fields() {
        let t = TerminalPrefs { scrollback: None };
        let s = toml::to_string_pretty(&t).unwrap();
        assert_eq!(s.trim(), "", "全 None 的分节不应写出任何键");

        let a = AppearancePrefs {
            icon: Some(IconSpec {
                kind: IconKind::Emoji,
                value: "🐧".into(),
            }),
            color: None,
        };
        let s = toml::to_string_pretty(&a).unwrap();
        assert!(s.contains("emoji"), "icon 应写出: {s}");
        assert!(!s.contains("color"), "None 的 color 不应写出: {s}");
        let back: AppearancePrefs = toml::from_str(&s).unwrap();
        assert_eq!(back, a);
    }

    #[test]
    fn color_spec_round_trips_with_targets() {
        let c = ColorSpec {
            hex: "#E5484D".into(),
            apply_to: vec![ColorTarget::Tab, ColorTarget::StatusBar],
        };
        let s = toml::to_string_pretty(&c).unwrap();
        let back: ColorSpec = toml::from_str(&s).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn secret_entry_carries_proxy_password() {
        let s = SecretEntry {
            password: None,
            passphrase: None,
            proxy_password: Some("p".into()),
        };
        let text = toml::to_string_pretty(&s).unwrap();
        let back: SecretEntry = toml::from_str(&text).unwrap();
        assert_eq!(back, s);

        let empty = toml::to_string_pretty(&SecretEntry::default()).unwrap();
        assert_eq!(empty.trim(), "", "全 None 的 SecretEntry 不写出任何键");
    }

    /// 加密存储的口令绝不能被 `{:?}` 顺手打进日志/panic 消息。
    #[test]
    fn debug_never_leaks_secret_entry_plaintext() {
        let e = SecretEntry {
            password: Some("hunter2".into()),
            passphrase: Some("keyphrase".into()),
            proxy_password: Some("proxypw".into()),
        };
        let s = format!("{e:?}");
        for leaked in ["hunter2", "keyphrase", "proxypw"] {
            assert!(!s.contains(leaked), "Debug 泄漏了明文口令: {s}");
        }
        assert!(s.contains("<已设置>"), "应报告字段已设置: {s}");
        assert!(
            format!("{:?}", SecretEntry::default()).contains("<无>"),
            "未设置的字段应报告 <无>"
        );
    }
}
