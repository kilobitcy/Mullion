//! 跳板链的路径预览与前置校验(走查 12)。**纯函数,零 egui。**
//!
//! 环 / 自引用 / 悬空 / 超深四种检测在 `mullion_store::jump` 里早就做全了
//! (那里有四条守护测试)。这里**不重新实现**任何一条 —— 只是把同一个内核
//! 的结果提前到编辑时,并把 `StoreError` 翻成用户看得懂的话。重新实现一遍
//! 的话,两套判据迟早漂移,结果就是「编辑时说没问题、拨号时连不上」。

use std::collections::BTreeMap;

use mullion_store::{GroupId, GroupRecord, JumpRef, SessionId, SessionRecord, StoreError};

/// 一行连接路径:`本机 →(SOCKS5)→ 堡垒 → 网关 → web01`。
///
/// `proxy` 是代理的短标签(`"SOCKS5"` / `"HTTP"` / `None`)。代理排在第一跳
/// **之前**:连接路径就是 `本机 →(代理)→ 第一跳 →…→ 目标`,页面上「代理」
/// 分区也排在「跳板」之前,顺序一致才读得通。
///
/// 悬空跳原地标出来而不是跳过:光说「有一跳不存在」的话,用户得挨个点开
/// 会话去找是哪一跳。
pub(super) fn preview(
    chain: &[SessionId],
    sessions: &[SessionRecord],
    proxy: Option<&str>,
    target: &str,
) -> String {
    let mut out = "本机".to_string();
    // 第一个箭头带代理标注,其余都是光杆箭头。
    let mut arrow = match proxy {
        Some(p) => format!("→({p})→"),
        None => "→".to_string(),
    };
    for id in chain {
        let name = match sessions.iter().find(|s| s.id == *id) {
            Some(s) => s.identity.name.clone(),
            None => format!("#{} 已删除", id.0),
        };
        out.push_str(&format!(" {arrow} {name}"));
        arrow = "→".to_string();
    }
    out.push_str(&format!(" {arrow} {target}"));
    out
}

/// 拨号前把 `mullion_store::jump` 的四种失败翻成人话。干净时返回 `None`。
///
/// **不自己判环**:调的就是拨号时用的同一个 `expand_chain_of`。
pub(super) fn check(chain: &[SessionId], sessions: &[SessionRecord]) -> Option<String> {
    if chain.is_empty() {
        return None;
    }
    // 只在链非空时才建索引:每帧给几十条会话建 `BTreeMap` 是白烧 CPU
    // (本项目陷阱 T3)。`chain_editor` 只在「自定义」模式下渲染,
    // 这里再加一道空链短路。
    let by_id: BTreeMap<SessionId, SessionRecord> =
        sessions.iter().map(|s| (s.id, s.clone())).collect();
    // 分组只影响跳板会话**自身**继承来的跳板设置。编辑器手上没有分组的
    // 全量索引,传空表 = 「跳板会话自己不从分组继承跳板」。这会让一种
    // 罕见情况漏报(A 的跳板由分组配、且构成环),但不会误报 —— 宁可漏,
    // 不可在干净的链上弹红字。拨号时 `expand_chain` 用的是全量索引,
    // 真有环仍然拦得住。
    let groups: BTreeMap<GroupId, GroupRecord> = BTreeMap::new();
    let refs: Vec<JumpRef> = chain.iter().map(|id| JumpRef(*id)).collect();
    match mullion_store::jump::expand_chain_of(&refs, &by_id, &groups) {
        Ok(_) => None,
        Err(StoreError::JumpCycle(id)) => Some(format!(
            "跳板链存在环,经过会话 #{} —— 拨号时会直接失败,请检查该会话自己的跳板设置",
            id.0
        )),
        Err(StoreError::JumpDangling(id)) => Some(format!(
            "第 #{} 跳指向的会话已被删除 —— 拨号会硬失败(不会悄悄改走直连)",
            id.0
        )),
        Err(StoreError::JumpTooDeep(_)) => Some(format!(
            "展开后超过 {} 跳 —— 每多一跳都乘一次延迟,几乎必是配错了",
            mullion_store::jump::MAX_JUMP_DEPTH
        )),
        Err(e) => Some(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_store::{Auth, AuthKind, Connection, Identity, NetworkPrefs, Protocol};

    fn sess(id: u64, name: &str, host: &str, jump: Option<Vec<u64>>) -> SessionRecord {
        SessionRecord {
            id: SessionId(id),
            modified_at: "t".into(),
            identity: Identity {
                name: name.into(),
                note: String::new(),
                group_id: None,
                tags: Vec::new(),
            },
            connection: Connection {
                host: host.into(),
                port: 22,
                protocol: Protocol::Ssh,
            },
            auth: Auth {
                user: "root".into(),
                kind: AuthKind::Password,
            },
            terminal: Default::default(),
            appearance: Default::default(),
            network: NetworkPrefs {
                proxy: None,
                jump: jump.map(|v| v.into_iter().map(|i| JumpRef(SessionId(i))).collect()),
            },
            automation: Default::default(),
        }
    }

    /// 预览要按**拨号顺序**读得通,并且把代理放在第一跳之前 ——
    /// 连接路径是 `本机 →(代理)→ 第一跳 →…→ 目标`。
    #[test]
    fn preview_reads_in_dial_order_with_proxy_first() {
        let sessions = vec![
            sess(1, "堡垒", "bastion.example", None),
            sess(2, "网关", "gw.internal", None),
        ];
        let line = preview(
            &[SessionId(1), SessionId(2)],
            &sessions,
            Some("SOCKS5"),
            "web01",
        );
        assert_eq!(line, "本机 →(SOCKS5)→ 堡垒 → 网关 → web01");
    }

    /// 没配代理时不该凭空冒出一个 `→()→`。
    #[test]
    fn preview_omits_the_proxy_hop_when_there_is_none() {
        let sessions = vec![sess(1, "堡垒", "bastion.example", None)];
        assert_eq!(
            preview(&[SessionId(1)], &sessions, None, "web01"),
            "本机 → 堡垒 → web01"
        );
    }

    /// 悬空引用在预览里也要看得见是**哪一跳**没了 —— 光说「有一跳不存在」
    /// 用户得挨个点开会话去找。
    #[test]
    fn preview_marks_a_dangling_hop_inline() {
        let sessions = vec![sess(1, "堡垒", "bastion.example", None)];
        let line = preview(&[SessionId(1), SessionId(42)], &sessions, None, "web01");
        assert!(line.contains("#42"), "悬空跳要带出 id:{line}");
        assert!(line.contains("已删除"), "悬空跳要说明原因:{line}");
    }

    /// 自引用是环的一种。数据层早就能测出来,走查 12 要的只是把它提前到
    /// 编辑时。
    #[test]
    fn check_catches_a_self_referencing_hop_before_dialing() {
        // #1 的跳板是它自己。
        let sessions = vec![sess(1, "堡垒", "bastion.example", Some(vec![1]))];
        let msg = check(&[SessionId(1)], &sessions).expect("自引用必须被拦下");
        assert!(msg.contains("环"), "错误要说人话:{msg}");
    }

    /// 两条会话互为跳板 —— 拨号时会无限递归,必须在编辑时就拦。
    #[test]
    fn check_catches_a_two_node_cycle() {
        let sessions = vec![
            sess(1, "A", "a.example", Some(vec![2])),
            sess(2, "B", "b.example", Some(vec![1])),
        ];
        assert!(
            check(&[SessionId(1)], &sessions).is_some(),
            "互引用必须被拦下"
        );
    }

    /// 一条干净的链不该报任何错 —— 误报比不报更烦人,用户会学会无视它。
    #[test]
    fn check_stays_quiet_on_a_healthy_chain() {
        let sessions = vec![
            sess(1, "堡垒", "bastion.example", None),
            sess(2, "网关", "gw.internal", Some(vec![1])),
        ];
        assert_eq!(check(&[SessionId(2)], &sessions), None);
    }
}
