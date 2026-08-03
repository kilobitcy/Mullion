//! 会话编辑表单的**纯逻辑**:表单缓冲、表单 → `SessionDraft` 的转换、凭据三态合成。
//!
//! **本文件不许 `use egui`**。会话表单的 bug(端口解析、代理三态、凭据被静默清除)
//! 全部能在没有窗口的情况下单测复现——这是把它从 UI 代码里切出来的全部理由。

use std::path::PathBuf;

use mullion_store::{
    AppearancePrefs, Auth, AuthKind, Connection, GroupId, Identity, NetworkPrefs, Protocol,
    SecretEntry, SessionDraft, SessionId, SessionRecord, TerminalPrefs,
};

/// 编辑表单里认证方式的选择。不复用 `AuthKind` 本身,因为 UI 在密码/公钥两种模式
/// 间切换时要各自保留自己的缓冲(密码框内容、私钥路径都不该因切换选项就丢)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthKindUi {
    Password,
    PublicKey,
}

/// 编辑表单里的代理选择。**四态**,不是三态:
/// 「跟随分组」与「不使用代理」必须分开,前者是不设置(继承),
/// 后者是显式 `Direct`(覆盖分组)。合并二者会让用户无法在有分组代理时单独直连。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxyModeUi {
    Inherit,
    Direct,
    Socks5,
    HttpConnect,
}

/// 编辑表单的跨帧字段缓冲。端口用 `String`(保存时才 `parse`),密码/口令是明文
/// 缓冲(仅存在于本进程内存,保存后随 `SaveIntent` 一次性转移给 store 加密)。
///
/// **不 derive Debug**:`password`/`passphrase`/`proxy_password` 三个字段是明文,
/// derive 会让 `{:?}` 把它们打印出来。目前全仓没有调用点,但属休眠风险
/// (对照 `mullion_ssh::hop::Hop` 同样的手写打码 Debug)。见下方手写实现。
#[derive(Clone)]
pub struct EditorBuffer {
    pub name: String,
    pub host: String,
    pub port: String,
    pub protocol: Protocol,
    pub user: String,
    pub note: String,
    pub auth_kind: AuthKindUi,
    pub password: String,
    pub key_path: String,
    pub passphrase: String,

    pub proxy_mode: ProxyModeUi,
    pub proxy_host: String,
    pub proxy_port: String,
    pub proxy_user: String,
    pub proxy_password: String,
    /// 跳板链,按拨号顺序。UI 用下拉逐个添加/删除。
    pub jump_chain: Vec<SessionId>,
    /// 跳板链是否被用户显式设过。`false` = 沿用继承(写回 `None`)。
    pub jump_set: bool,

    // ↓↓↓ 透传字段:UI 目前没有编辑标签/终端偏好/外观偏好的入口(分组自
    // P0-b 起已可编辑,见下方 preserved_group_id 的注),但
    // `Vault::update` 对 `identity`/`terminal`/`appearance` 是整体字段替换
    // 而非合并(见 vault.rs)。所以编辑表单必须把这些字段原样存下来再原样写回,
    // 否则「编辑会话」会静默清空它们。新建会话时没有 `SessionRecord` 可读,
    // 保持默认值(未分组/无标签/默认偏好)。
    // (`network` 分节曾经也在这份透传名单里,现在有了 proxy_mode/jump_chain
    // 等真正的编辑字段,不再需要盲目透传。)
    // 注:`preserved_group_id` 自 P0-b 起可由编辑器下拉修改,名字沿用未改以免波及守护测试。
    pub preserved_group_id: Option<GroupId>,
    pub preserved_tags: Vec<String>,
    pub preserved_terminal: TerminalPrefs,
    pub preserved_appearance: AppearancePrefs,
}

impl Default for EditorBuffer {
    fn default() -> Self {
        Self {
            name: String::new(),
            host: String::new(),
            port: "22".to_string(),
            protocol: Protocol::Ssh,
            user: String::new(),
            note: String::new(),
            auth_kind: AuthKindUi::Password,
            password: String::new(),
            key_path: String::new(),
            passphrase: String::new(),
            proxy_mode: ProxyModeUi::Inherit,
            proxy_host: String::new(),
            proxy_port: "1080".to_string(),
            proxy_user: String::new(),
            proxy_password: String::new(),
            jump_chain: Vec::new(),
            jump_set: false,
            preserved_group_id: None,
            preserved_tags: Vec::new(),
            preserved_terminal: TerminalPrefs::default(),
            preserved_appearance: AppearancePrefs::default(),
        }
    }
}

fn redacted(s: &str) -> &'static str {
    if s.is_empty() {
        "<空>"
    } else {
        "<已设置>"
    }
}

impl std::fmt::Debug for EditorBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditorBuffer")
            .field("name", &self.name)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("protocol", &self.protocol)
            .field("user", &self.user)
            .field("note", &self.note)
            .field("auth_kind", &self.auth_kind)
            .field("password", &redacted(&self.password))
            .field("key_path", &self.key_path)
            .field("passphrase", &redacted(&self.passphrase))
            .field("proxy_mode", &self.proxy_mode)
            .field("proxy_host", &self.proxy_host)
            .field("proxy_port", &self.proxy_port)
            .field("proxy_user", &self.proxy_user)
            .field("proxy_password", &redacted(&self.proxy_password))
            .field("jump_chain", &self.jump_chain)
            .field("jump_set", &self.jump_set)
            .field("preserved_group_id", &self.preserved_group_id)
            .field("preserved_tags", &self.preserved_tags)
            .field("preserved_terminal", &self.preserved_terminal)
            .field("preserved_appearance", &self.preserved_appearance)
            .finish()
    }
}

impl EditorBuffer {
    /// 把已有会话的非敏感字段填入表单(密码/口令 store 不会明文回吐,留空 ——
    /// 编辑时留空 = 不改;见 `build_draft` 的说明)。
    pub(crate) fn from_record(rec: &SessionRecord) -> Self {
        let mut buf = Self {
            name: rec.identity.name.clone(),
            host: rec.connection.host.clone(),
            port: rec.connection.port.to_string(),
            protocol: rec.connection.protocol,
            user: rec.auth.user.clone(),
            note: rec.identity.note.clone(),
            preserved_group_id: rec.identity.group_id,
            preserved_tags: rec.identity.tags.clone(),
            preserved_terminal: rec.terminal.clone(),
            preserved_appearance: rec.appearance.clone(),
            ..Self::default()
        };
        match &rec.network.proxy {
            None => buf.proxy_mode = ProxyModeUi::Inherit,
            Some(mullion_store::ProxyChoice::Direct) => buf.proxy_mode = ProxyModeUi::Direct,
            Some(mullion_store::ProxyChoice::Socks5(ep)) => {
                buf.proxy_mode = ProxyModeUi::Socks5;
                buf.proxy_host = ep.host.clone();
                buf.proxy_port = ep.port.to_string();
                buf.proxy_user = ep.user.clone().unwrap_or_default();
            }
            Some(mullion_store::ProxyChoice::HttpConnect(ep)) => {
                buf.proxy_mode = ProxyModeUi::HttpConnect;
                buf.proxy_host = ep.host.clone();
                buf.proxy_port = ep.port.to_string();
                buf.proxy_user = ep.user.clone().unwrap_or_default();
            }
        }
        if let Some(chain) = &rec.network.jump {
            buf.jump_set = true;
            buf.jump_chain = chain.iter().map(|j| j.0).collect();
        }
        match &rec.auth.kind {
            AuthKind::Password => buf.auth_kind = AuthKindUi::Password,
            AuthKind::PublicKey { path, .. } => {
                buf.auth_kind = AuthKindUi::PublicKey;
                buf.key_path = path.display().to_string();
            }
        }
        buf
    }
}

/// 一次「保存」的意图:app 事后据此调用 `store.add`(`editing_id=None`)或
/// `store.update`(`Some(id)`)。
pub struct SaveIntent {
    pub editing_id: Option<SessionId>,
    pub draft: SessionDraft,
}

/// 表单缓冲 → `SessionDraft`。纯函数,不碰 egui,可脱离 GUI 单测。
///
/// 密码认证:密码框留空 → `secret=None`(= 清除已存凭据,留空的语义在 UI 上有提示)。
/// 公钥认证:`has_passphrase` 由口令框是否非空决定;口令非空才带 `secret`。
pub(crate) fn build_draft(buf: &EditorBuffer) -> Result<SessionDraft, String> {
    let port: u16 = buf
        .port
        .trim()
        .parse()
        .map_err(|_| "端口非法,须为 1-65535 的整数".to_string())?;
    let (auth, secret) = match buf.auth_kind {
        AuthKindUi::Password => {
            let secret = if buf.password.is_empty() {
                None
            } else {
                Some(SecretEntry {
                    password: Some(buf.password.clone()),
                    passphrase: None,
                    proxy_password: None,
                })
            };
            (AuthKind::Password, secret)
        }
        AuthKindUi::PublicKey => {
            let has_passphrase = !buf.passphrase.is_empty();
            let secret = if has_passphrase {
                Some(SecretEntry {
                    password: None,
                    passphrase: Some(buf.passphrase.clone()),
                    proxy_password: None,
                })
            } else {
                None
            };
            (
                AuthKind::PublicKey {
                    path: PathBuf::from(buf.key_path.trim()),
                    has_passphrase,
                },
                secret,
            )
        }
    };
    // 代理口令与 SSH 凭据存在同一个 SecretEntry 里。即使 SSH 侧没有任何凭据,
    // 只要配了代理口令就得建一个 entry,否则口令无处存。
    let proxy_password = if buf.proxy_password.is_empty() {
        None
    } else {
        Some(buf.proxy_password.clone())
    };
    let secret = match (secret, proxy_password) {
        (Some(mut s), pp) => {
            s.proxy_password = pp;
            Some(s)
        }
        (None, Some(pp)) => Some(SecretEntry {
            password: None,
            passphrase: None,
            proxy_password: Some(pp),
        }),
        (None, None) => None,
    };
    let proxy = match buf.proxy_mode {
        ProxyModeUi::Inherit => None,
        ProxyModeUi::Direct => Some(mullion_store::ProxyChoice::Direct),
        ProxyModeUi::Socks5 | ProxyModeUi::HttpConnect => {
            let pport: u16 = buf
                .proxy_port
                .trim()
                .parse()
                .map_err(|_| "代理端口非法,须为 1-65535 的整数".to_string())?;
            let ep = mullion_store::ProxyEndpoint {
                host: buf.proxy_host.trim().to_string(),
                port: pport,
                user: if buf.proxy_user.trim().is_empty() {
                    None
                } else {
                    Some(buf.proxy_user.trim().to_string())
                },
            };
            Some(if buf.proxy_mode == ProxyModeUi::Socks5 {
                mullion_store::ProxyChoice::Socks5(ep)
            } else {
                mullion_store::ProxyChoice::HttpConnect(ep)
            })
        }
    };
    let jump = if buf.jump_set {
        Some(
            buf.jump_chain
                .iter()
                .map(|id| mullion_store::JumpRef(*id))
                .collect(),
        )
    } else {
        None
    };
    Ok(SessionDraft {
        identity: Identity {
            name: buf.name.trim().to_string(),
            // note 不 trim:用户备注里的前后空格属于用户数据(既有行为)。
            note: buf.note.clone(),
            group_id: buf.preserved_group_id,
            tags: buf.preserved_tags.clone(),
        },
        connection: Connection {
            host: buf.host.trim().to_string(),
            port,
            protocol: buf.protocol,
        },
        auth: Auth {
            user: buf.user.trim().to_string(),
            kind: auth,
        },
        terminal: buf.preserved_terminal.clone(),
        appearance: buf.preserved_appearance.clone(),
        network: NetworkPrefs { proxy, jump },
        secret,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_store::{
        ColorSpec, ColorTarget, IconKind, IconSpec, JumpRef, ProxyChoice, ProxyEndpoint,
    };

    /// 红线:`EditorBuffer` 携带三个明文口令缓冲。若 derive(Debug),`{:?}` 会把
    /// 它们打印出来——目前虽无调用点,但属休眠风险(对照 `mullion_ssh::hop::Hop`
    /// 的手写打码 Debug)。必须手写 Debug 并打码。
    #[test]
    fn debug_never_leaks_editor_buffer_secrets() {
        let mut b = buf();
        b.password = "hunter2".into();
        b.passphrase = "keypw".into();
        b.proxy_password = "proxypw".into();
        let s = format!("{b:?}");
        assert!(!s.contains("hunter2"), "密码绝不能出现在 Debug 里: {s}");
        assert!(!s.contains("keypw"), "私钥口令绝不能出现在 Debug 里: {s}");
        assert!(!s.contains("proxypw"), "代理口令绝不能出现在 Debug 里: {s}");
        assert!(s.contains("192.0.2.10"), "非敏感字段应保留以便排障: {s}");
    }

    fn buf() -> EditorBuffer {
        EditorBuffer {
            name: "dev".into(),
            host: "192.0.2.10".into(),
            port: "22".into(),
            protocol: Protocol::Ssh,
            user: "user".into(),
            note: "跳板后".into(),
            auth_kind: AuthKindUi::Password,
            password: String::new(),
            key_path: String::new(),
            passphrase: String::new(),
            ..EditorBuffer::default()
        }
    }

    #[test]
    fn password_session_builds_draft_with_secret() {
        let mut b = buf();
        b.password = "pw".into();
        let draft = build_draft(&b).unwrap();
        assert_eq!(draft.identity.name, "dev");
        assert_eq!(draft.connection.host, "192.0.2.10");
        assert_eq!(draft.connection.port, 22);
        assert!(matches!(draft.auth.kind, AuthKind::Password));
        assert_eq!(
            draft.secret.as_ref().and_then(|s| s.password.clone()),
            Some("pw".to_string())
        );
    }

    #[test]
    fn password_session_with_empty_password_clears_secret() {
        let b = buf(); // password 留空
        let draft = build_draft(&b).unwrap();
        assert!(draft.secret.is_none(), "留空密码应清除已存凭据");
    }

    #[test]
    fn pubkey_with_passphrase_sets_has_passphrase_and_secret() {
        let mut b = buf();
        b.auth_kind = AuthKindUi::PublicKey;
        b.key_path = "/home/me/id_ed25519".into();
        b.passphrase = "ph".into();
        let draft = build_draft(&b).unwrap();
        match draft.auth.kind {
            AuthKind::PublicKey {
                path,
                has_passphrase,
            } => {
                assert_eq!(path, PathBuf::from("/home/me/id_ed25519"));
                assert!(has_passphrase);
            }
            _ => panic!("应为 PublicKey"),
        }
        assert_eq!(
            draft.secret.as_ref().and_then(|s| s.passphrase.clone()),
            Some("ph".to_string())
        );
    }

    #[test]
    fn pubkey_without_passphrase_has_no_secret() {
        let mut b = buf();
        b.auth_kind = AuthKindUi::PublicKey;
        b.key_path = "/home/me/id_ed25519".into();
        let draft = build_draft(&b).unwrap();
        match draft.auth.kind {
            AuthKind::PublicKey { has_passphrase, .. } => assert!(!has_passphrase),
            _ => panic!("应为 PublicKey"),
        }
        assert!(draft.secret.is_none());
    }

    #[test]
    fn invalid_port_is_rejected() {
        let mut b = buf();
        b.port = "not-a-port".into();
        assert!(build_draft(&b).is_err());

        let mut b2 = buf();
        b2.port = "99999999".into(); // 超出 u16 范围
        assert!(build_draft(&b2).is_err());
    }

    /// 回归测试(critical):编辑表单编辑不到的字段(分组/标签/终端偏好/外观偏好)
    /// 在「读入表单 → 写回 draft」这趟往返里必须原样保留,不能被表单的占位默认值
    /// 悄悄清空。`Vault::update` 对这四项是整体替换而非合并,一旦 build_draft 填了
    /// 默认值,保存就会真的把用户数据清空。
    #[test]
    fn editing_a_session_preserves_fields_the_form_cannot_edit() {
        let rec = SessionRecord {
            id: SessionId(7),
            modified_at: "2026-07-25T00:00:00Z".into(),
            identity: Identity {
                name: "dev".into(),
                note: "跳板后".into(),
                group_id: Some(GroupId(1)),
                tags: vec!["web01".into()],
            },
            connection: Connection {
                host: "192.0.2.10".into(),
                port: 22,
                protocol: Protocol::Ssh,
            },
            auth: Auth {
                user: "user".into(),
                kind: AuthKind::Password,
            },
            terminal: TerminalPrefs {
                scrollback: Some(12345),
            },
            appearance: AppearancePrefs {
                icon: Some(IconSpec {
                    kind: IconKind::Emoji,
                    value: "🚀".into(),
                }),
                color: Some(ColorSpec {
                    hex: "#ff0000".into(),
                    apply_to: vec![ColorTarget::Tab],
                }),
            },
            // 非默认值:表单目前没有代理/跳板编辑控件(那是后续任务的事),
            // 但这条守护测试要能抓住「编辑时被静默清空」——值必须区别于
            // `NetworkPrefs::default()`,否则清空和保留看起来一样。
            network: NetworkPrefs {
                proxy: Some(ProxyChoice::Socks5(ProxyEndpoint {
                    host: "127.0.0.1".into(),
                    port: 7891,
                    user: None,
                })),
                jump: None,
            },
        };

        let editor_buf = EditorBuffer::from_record(&rec);
        let draft = build_draft(&editor_buf).unwrap();

        assert_eq!(
            draft.identity.group_id, rec.identity.group_id,
            "编辑不该清空 UI 编辑不到的字段:group_id"
        );
        assert_eq!(
            draft.identity.tags, rec.identity.tags,
            "编辑不该清空 UI 编辑不到的字段:tags"
        );
        assert_eq!(
            draft.terminal, rec.terminal,
            "编辑不该清空 UI 编辑不到的字段:terminal"
        );
        assert_eq!(
            draft.appearance, rec.appearance,
            "编辑不该清空 UI 编辑不到的字段:appearance"
        );
        assert_eq!(
            draft.network, rec.network,
            "编辑不该清空 UI 编辑不到的字段:network(代理/跳板)"
        );
    }

    /// 表单能编代理与跳板了,它们必须真的往返一次而不被吃掉。
    #[test]
    fn editor_round_trips_proxy_and_jump_chain() {
        let rec = SessionRecord {
            id: SessionId(7),
            modified_at: "2026-07-25T00:00:00Z".into(),
            identity: Identity {
                name: "dev".into(),
                note: "跳板后".into(),
                group_id: None,
                tags: Vec::new(),
            },
            connection: Connection {
                host: "192.0.2.10".into(),
                port: 22,
                protocol: Protocol::Ssh,
            },
            auth: Auth {
                user: "user".into(),
                kind: AuthKind::Password,
            },
            terminal: TerminalPrefs::default(),
            appearance: AppearancePrefs::default(),
            network: NetworkPrefs {
                proxy: Some(ProxyChoice::Socks5(ProxyEndpoint {
                    host: "127.0.0.1".into(),
                    port: 7891,
                    user: Some("alice".into()),
                })),
                jump: Some(vec![JumpRef(SessionId(2))]),
            },
        };
        let buf = EditorBuffer::from_record(&rec);
        let draft = build_draft(&buf).unwrap();
        assert_eq!(draft.network, rec.network, "代理与跳板必须原样往返");
    }

    /// 分组代理下,会话选「不使用代理」必须落成显式 `Direct` 而非 `None`——
    /// 落成 `None` 会继续继承分组代理,与用户所选相反。
    #[test]
    fn choosing_no_proxy_writes_explicit_direct_not_inherit() {
        let buf = EditorBuffer {
            port: "22".into(),
            proxy_mode: ProxyModeUi::Direct,
            ..EditorBuffer::default()
        };
        let draft = build_draft(&buf).unwrap();
        assert_eq!(
            draft.network.proxy,
            Some(ProxyChoice::Direct),
            "「不使用代理」是覆盖,不是不设置"
        );
    }

    #[test]
    fn choosing_inherit_leaves_proxy_unset() {
        let buf = EditorBuffer {
            port: "22".into(),
            proxy_mode: ProxyModeUi::Inherit,
            ..EditorBuffer::default()
        };
        let draft = build_draft(&buf).unwrap();
        assert_eq!(draft.network.proxy, None, "「跟随分组」= 不设置");
    }

    #[test]
    fn proxy_port_must_be_a_valid_number() {
        let buf = EditorBuffer {
            port: "22".into(),
            proxy_mode: ProxyModeUi::Socks5,
            proxy_host: "127.0.0.1".into(),
            proxy_port: "abc".into(),
            ..EditorBuffer::default()
        };
        let err = match build_draft(&buf) {
            Err(e) => e,
            Ok(_) => panic!("非法代理端口应被拒绝"),
        };
        assert!(err.contains("代理端口"), "错误消息应点名是代理端口: {err}");
    }

    /// 钉死 note 不被 trim(既有行为:旧代码是 `buf.note.clone()`,迁移不该顺手改成
    /// `trim()`——用户备注里的前后空格属于用户数据,不该被悄悄吃掉)。
    #[test]
    fn note_is_not_trimmed_when_building_draft() {
        let mut b = buf();
        b.note = "  缩进备注  ".into();
        let draft = build_draft(&b).unwrap();
        assert_eq!(
            draft.identity.note, "  缩进备注  ",
            "note 不应被 trim,前后空格属于用户数据"
        );
    }

    /// 新建会话(没有 SessionRecord 可读)时,这四项仍应是默认值——确认
    /// `EditorBuffer::default()` 的新建路径没被这次修复带偏。
    #[test]
    fn new_session_defaults_have_no_preserved_fields() {
        let b = EditorBuffer::default();
        let draft = build_draft(&b).unwrap();
        assert_eq!(draft.identity.group_id, None);
        assert_eq!(draft.identity.tags, Vec::<String>::new());
        assert_eq!(draft.terminal, TerminalPrefs::default());
        assert_eq!(draft.appearance, AppearancePrefs::default());
    }
}
