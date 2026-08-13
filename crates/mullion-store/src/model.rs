//! 会话数据模型。只放数据类型,零 IO。非敏感字段落明文 TOML;密码/口令走加密侧车(vault)。

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
    /// 公钥认证。**私钥内容本身**和口令都在加密侧车(`SecretEntry::private_key` /
    /// `passphrase`),这里只留「要不要口令」这一位。
    ///
    /// v5 起不再存私钥**路径**:路径既不是凭据也不是身份,却让会话跟一台机器上的
    /// 一个文件绑死 —— 换机器、挪目录、导出配置全部失效。旧文件里的路径由
    /// `Vault::open` 的 v<5 迁移读成内容后丢弃(见 `migrate::legacy_key_paths`)。
    PublicKey { has_passphrase: bool },
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
    /// F120:SFTP 书签与默认目录。v8 新增,旧文件没有这个键 → `default`
    /// 补空,无需迁移代码。
    #[serde(default)]
    pub sftp: crate::sftp::SftpPrefs,
}

/// 一条会话的**敏感**部分,加密后存 secrets.enc。
///
/// **不 derive Debug**:四个字段全是明文凭据,`{:?}` 一打就把它们写进日志/panic
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
    /// 私钥**内容**(PEM / OpenSSH 文本),v5 起取代原来的明文路径。
    /// 未加密的私钥即等同于密码,必须与口令一样走加密侧车。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub private_key: Option<String>,
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
            .field("private_key", &redacted(&self.private_key))
            .finish()
    }
}

/// 分组稳定主键。新建时取现有 max+1(见 vault)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct GroupId(pub u64);

/// 图标来源。
///
/// **只有 `Ico` 是活的**,另外三个变体一律是历史数据的容身之处 —— 删掉任何
/// 一个,含该 `kind` 的老 `sessions.toml` 会直接反序列化失败,用户丢的不是
/// 一个图标而是整个会话库。UI 上不再产出它们,不等于可以从类型里抹掉。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IconKind {
    /// 内置图标库中的名字(如 "ubuntu")。**历史遗留**,UI 早已撤掉。
    Builtin,
    /// 单个 emoji 字符。**历史遗留**:epaint 不支持 COLR/CPAL 彩色字形,
    /// emoji 在界面上只能画成黑白剪影,在 64px 的纯图标档下根本认不出是哪台机器
    /// (见 `mullion-app` 的 `badge.rs`)。v6 起改用 `Ico`。
    Emoji,
    /// 用户提供的图片路径。**历史遗留**,从未有 UI 产出过。
    Custom,
    /// 用户导入的 `.ico`,`value` 是**归一化后的文件正文的 base64**
    /// (只含 32×32 与 64×64 两帧,见 `mullion-app` 的 `ui::ico`)。
    ///
    /// 存正文而不是路径:图标要跟着配置走。用户把配置拷到另一台机器、或者
    /// 把当初那个 .ico 删了,图标都不该跟着消失 —— 这与私钥「路径不入库」
    /// 是同一条思路(v5)。
    Ico,
}

/// 图标规格。**复合对象:只能整体继承或整体覆盖**(设计 §4.1)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IconSpec {
    pub kind: IconKind,
    pub value: String,
    /// **已停用(v0.1.28)。** 图标底色改为跟随会话的节点色(`ColorSpec`),
    /// 不再单独配置;绘制侧 `badge::paint_icon` 不再读这个字段。
    ///
    /// 字段保留而非删除:v6 的文件里可能存着值,读到不该崩、也不该丢用户数据。
    /// 不做迁移 —— 迁移要动 `SCHEMA_VERSION`,而这里没有任何东西需要转换。
    ///
    /// 带 `default` + `skip_serializing_if`:没垫底色的图标不该往 TOML 里
    /// 写一行 `bg = ""`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg: Option<String>,
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
/// v5 = v4 - `[session.auth].path`:私钥改存**内容**到加密侧车。
/// v6 = v5 + `IconKind::Ico` 与 `IconSpec.bg`:会话图标改成用户导入的 .ico
///      (正文 base64 内嵌),并可配底色。
/// v7 = v6 + `[[tunnel]]`:隧道成为引用会话的一等对象(F110~F117)。
/// v8 = v7 + `session.sftp`:SFTP 书签与默认远端/本地目录(F120)。
///
/// 结构上新版本能直接读旧版本(新字段全带 `serde(default)`),升版本号是为了让
/// **旧客户端明确拒绝**,而不是静默丢弃新分节再写回。v5 还多一层理由:旧客户端
/// 读 v5 会发现 `path` 没了,公钥会话直接连不上,拒绝比装作能用好。新增
/// `session.sftp` 是同一条理由:旧客户端读 v8 会把整个分节丢掉再写回,拒绝
/// 比静默吃掉好。
///
/// **号段归属**:F74(凭据实体)原定 v3→v4,被 F40~F44 先落地拿走了 4,再被本次
/// 「私钥入库」拿走了 5(规则「谁先落地谁拿号」,见 `spec.md` F74)。
pub const CURRENT_SCHEMA: u32 = 8;

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
    /// v7 新增。旧文件没有这个键 → `default` 补空数组,无需迁移代码。
    #[serde(default)]
    pub tunnel: Vec<crate::tunnel::TunnelRecord>,
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
                    has_passphrase: false,
                },
            },
            terminal: TerminalPrefs {
                scrollback: Some(5000),
            },
            appearance: AppearancePrefs::default(),
            network: crate::network::NetworkPrefs::default(),
            automation: crate::automation::AutomationPrefs::default(),
            sftp: crate::sftp::SftpPrefs::default(),
        };
        let file = SessionsFile {
            schema_version: CURRENT_SCHEMA,
            group: Vec::new(),
            session: vec![rec.clone()],
            tunnel: Vec::new(),
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
            sftp: crate::sftp::SftpPrefs::default(),
        };
        let file = SessionsFile {
            schema_version: CURRENT_SCHEMA,
            group: Vec::new(),
            session: vec![rec],
            tunnel: Vec::new(),
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
                bg: None,
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
            private_key: None,
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
            private_key: None,
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
