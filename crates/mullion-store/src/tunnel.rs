//! 隧道(端口转发)数据模型。只放数据类型与纯查询,零 IO、零 async。
//!
//! 设计见 `docs/superpowers/specs/2026-08-11-tunnels-design.md`(F110~F117)。
//! 隧道是**引用会话**的一等对象:自己不存主机/认证,只存「转发什么」,
//! 拨号目标一律由 `session_id` 指向的会话提供(设计 D2,纯引用无覆盖)。

use serde::{Deserialize, Serialize};

use crate::model::{SessionId, SessionRecord};

/// 隧道稳定主键。新建时取现有 max+1(见 vault),**与 `SessionId` 各自独立编号**。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TunnelId(pub u64);

/// 转发类型及其**该类型才有**的字段。
///
/// 用带数据枚举而非「一个 kind 字段 + 一堆可选字段」,是为了让设计 D5/D12
/// (`-D` 锁死本机、`-D` 无目标)成为**编译期保证**:`Dynamic` 变体在类型上
/// 就没有 `target_*` / `expose`,想给动态转发配一个暴露地址,代码写不出来。
/// UI 层因此不需要靠 disabled 兜底。
///
/// `#[serde(tag)]` 与 `TunnelRecord` 的 `#[serde(flatten)]` 配合平铺,
/// 照抄 `Auth`/`AuthKind`(model.rs:52-60)已在 toml 下验证过的组合。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum TunnelKind {
    /// `-L`:本机侦听,连接由**远端主机**发起到 `target`(目标名也由远端解析)。
    Local {
        target_host: String,
        target_port: u16,
        /// `true` = 侦听 0.0.0.0(同网段任意主机可连)。**默认 false = 仅本机**。
        #[serde(default)]
        expose: bool,
    },
    /// `-R`:远端侦听,连接由**本机**发起到 `target`(目标名也由本机解析)。
    Remote {
        target_host: String,
        target_port: u16,
        /// `true` = 请求远端侦听 0.0.0.0。受 sshd 的 `GatewayPorts` 制约,
        /// 服务端可静默降级为仅回环 —— 协议应答只带端口,无法从本地判定(设计 D9)。
        #[serde(default)]
        expose: bool,
    },
    /// `-D`:本机 SOCKS5 代理,目标由每条连接自己携带,故无固定目标。
    /// **恒绑回环**:开放的无认证代理与「暴露一个已知目标」是两类风险(设计 D5)。
    Dynamic,
}

/// 一条隧道。自身不含主机/认证 —— 那些全部来自 `session_id` 指向的会话。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TunnelRecord {
    pub id: TunnelId,
    /// 提供拨号目标与认证的会话。悬垂时**必须报错**,见 `resolve_target`。
    pub session_id: SessionId,
    /// 侦听端口:`Local`/`Dynamic` 在本机,`Remote` 在远端。
    pub listen_port: u16,
    #[serde(default)]
    pub note: String,
    /// 已落盘但本切片无 UI、无行为(设计 D6,欠账留给 T-b)。
    #[serde(default)]
    pub autostart: bool,
    /// 平铺进 `[[tunnel]]`,不额外产生一层 table。
    #[serde(flatten)]
    pub kind: TunnelKind,
}

/// 解析隧道要拨的会话。**悬垂必须硬失败**,不返回 `None`、不跳过、不回落到
/// 任何默认会话 —— 与 `jump::expand_chain` 对悬垂跳板的处置是同一条规则。
///
/// 零 IO 纯函数:`sessions` 由调用方(app)从 vault 里索引好传进来。
pub fn resolve_target<'a>(
    t: &TunnelRecord,
    sessions: &'a std::collections::BTreeMap<SessionId, SessionRecord>,
) -> Result<&'a SessionRecord, crate::StoreError> {
    sessions
        .get(&t.session_id)
        .ok_or(crate::StoreError::TunnelDangling(t.session_id))
}

/// 删除会话前的影响面:哪些隧道引用了它。
///
/// 按 `TunnelId` 升序返回 —— 确认框里的清单顺序必须稳定(不能沿用
/// `tunnels` 的存储顺序,那个会随增删漂移)。
pub fn tunnels_referencing(id: SessionId, tunnels: &[TunnelRecord]) -> Vec<&TunnelRecord> {
    let mut hit: Vec<&TunnelRecord> = tunnels.iter().filter(|t| t.session_id == id).collect();
    hit.sort_by_key(|t| t.id);
    hit
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Auth, AuthKind, Connection, Identity, Protocol, SessionRecord, TerminalPrefs,
    };
    use std::collections::BTreeMap;

    /// 只为让测试能产出真实的 `[[tunnel]]` 数组形态,不进生产代码。
    #[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    struct Wrap {
        tunnel: Vec<TunnelRecord>,
    }

    fn local() -> TunnelRecord {
        TunnelRecord {
            id: TunnelId(1),
            session_id: SessionId(7),
            listen_port: 3306,
            note: "本地连库".into(),
            autostart: false,
            kind: TunnelKind::Local {
                target_host: "db.internal".into(),
                target_port: 3306,
                expose: false,
            },
        }
    }

    #[test]
    fn local_tunnel_round_trips_through_toml() {
        let file = Wrap {
            tunnel: vec![local()],
        };
        let text = toml::to_string(&file).unwrap();

        // `kind` 必须平铺在 `[[tunnel]]` 下,不额外产生一层 table
        // ——照抄 `Auth`/`AuthKind`(model.rs:52-60)已验证过的组合。
        assert!(text.contains("[[tunnel]]"), "实际:\n{text}");
        assert!(text.contains(r#"kind = "local""#), "实际:\n{text}");
        assert!(
            !text.contains("[tunnel.kind]"),
            "kind 不能变成子 table,实际:\n{text}"
        );

        let back: Wrap = toml::from_str(&text).unwrap();
        assert_eq!(back, file);
    }

    #[test]
    fn dynamic_tunnel_has_no_target_or_expose_fields() {
        // D5/D12 的守护:动态转发(-D)在**类型上**就没有目标与暴露字段。
        // 有人把 `Dynamic` 改成带字段的变体时这条会红 —— 那意味着
        // 「给 SOCKS5 代理配一个全网卡绑定」重新变得可表示。
        let file = Wrap {
            tunnel: vec![TunnelRecord {
                id: TunnelId(2),
                session_id: SessionId(7),
                listen_port: 1080,
                note: String::new(),
                autostart: false,
                kind: TunnelKind::Dynamic,
            }],
        };
        let text = toml::to_string(&file).unwrap();

        for forbidden in ["target_host", "target_port", "expose"] {
            assert!(
                !text.contains(forbidden),
                "动态转发不该产出 {forbidden},实际:\n{text}"
            );
        }
    }

    #[test]
    fn expose_defaults_to_false_when_key_absent() {
        // serde 对 `bool` 的 `#[serde(default)]` 是 `false`,而 `false` 正是**安全值**
        // (仅本机可连)。若字段取名 `local_only`,缺键会默认成 `false` = 全网卡暴露
        // ——手改文件漏一个键就静默开放端口。字段命名方向本身就是一道安全闸门。
        let text = r#"
[[tunnel]]
id = 1
session_id = 7
listen_port = 3306
kind = "local"
target_host = "db.internal"
target_port = 3306
"#;
        let back: Wrap = toml::from_str(text).unwrap();
        let TunnelKind::Local { expose, .. } = back.tunnel[0].kind else {
            panic!("应解析为 Local");
        };
        assert!(!expose, "缺 expose 键必须默认为仅本机");
    }

    fn session(id: u64) -> SessionRecord {
        SessionRecord {
            id: SessionId(id),
            modified_at: "t".into(),
            identity: Identity {
                name: format!("s{id}"),
                note: String::new(),
                group_id: None,
                tags: Vec::new(),
            },
            connection: Connection {
                host: format!("host{id}"),
                port: 22,
                protocol: Protocol::Ssh,
            },
            auth: Auth {
                user: "u".into(),
                kind: AuthKind::Password,
            },
            terminal: TerminalPrefs::default(),
            appearance: Default::default(),
            network: Default::default(),
            automation: Default::default(),
        }
    }

    fn index(ids: &[u64]) -> BTreeMap<SessionId, SessionRecord> {
        ids.iter().map(|&i| (SessionId(i), session(i))).collect()
    }

    fn t(id: u64, session: u64) -> TunnelRecord {
        TunnelRecord {
            id: TunnelId(id),
            session_id: SessionId(session),
            listen_port: 3306,
            note: String::new(),
            autostart: false,
            kind: TunnelKind::Dynamic,
        }
    }

    /// 命名刻意对齐 `jump.rs` 的 `dangling_reference_is_rejected_never_degraded_to_direct`
    /// ——两处是**同一条规则**:引用不存在的会话必须硬失败。
    ///
    /// 隧道这边的降级后果更隐蔽:静默跳到「第一个会话」会把一条本机端口
    /// 悄悄接到另一台机器上,用户以为自己在连测试库,实际连的是生产库。
    #[test]
    fn dangling_tunnel_reference_is_rejected_never_silently_dropped() {
        let sessions = index(&[1, 2]);
        let err = resolve_target(&t(1, 42), &sessions).unwrap_err();
        assert!(
            matches!(err, crate::StoreError::TunnelDangling(SessionId(42))),
            "悬垂引用必须报错,实际: {err:?}"
        );
        // 反面:存在的引用要能解析到**那一条**,不是别的。
        let got = resolve_target(&t(1, 2), &sessions).unwrap();
        assert_eq!(got.id, SessionId(2));
    }

    #[test]
    fn tunnels_referencing_finds_all_and_only_the_affected() {
        let tunnels = vec![t(1, 7), t(2, 8), t(3, 7)];
        let got = tunnels_referencing(SessionId(7), &tunnels);
        assert_eq!(
            got.iter().map(|x| x.id).collect::<Vec<_>>(),
            vec![TunnelId(1), TunnelId(3)]
        );
        assert!(tunnels_referencing(SessionId(99), &tunnels).is_empty());
    }

    /// 删除确认框里列出的顺序不能每次刷新都变 —— 用户在读一份「即将失效的
    /// 清单」,顺序跳动会让人怀疑自己看错了。
    #[test]
    fn tunnels_referencing_is_stable_ordered() {
        let tunnels = vec![t(9, 7), t(2, 7), t(5, 7)];
        let got = tunnels_referencing(SessionId(7), &tunnels);
        assert_eq!(
            got.iter().map(|x| x.id).collect::<Vec<_>>(),
            vec![TunnelId(2), TunnelId(5), TunnelId(9)],
            "必须按 TunnelId 升序,不能沿用输入顺序"
        );
    }
}
