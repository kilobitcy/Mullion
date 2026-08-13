//! F5 跳板引用图解析(设计 §4.1)。纯函数,零 IO。
//!
//! 输出是**扁平化后的会话 id 序列**,按拨号顺序:`[0]` 最先连,最后一个是离目标最近的跳板。
//! 目标会话本身**不在**返回值里。

use std::collections::BTreeMap;

use crate::error::StoreError;
use crate::inherit::{resolve, PrefsLayer};
use crate::model::{SessionId, SessionRecord};

/// 跳板链最大深度。超过即报错——现实里没人串 8 台以上,超了几乎必是配置错误,
/// 且每多一跳都乘上一次延迟。
pub const MAX_JUMP_DEPTH: usize = 8;

/// 展开 `target` 的完整跳板链,返回按拨号顺序排列的会话 id。
///
/// `sessions` / `groups` 是全量索引:展开过程要读每个跳板会话**自身**的
/// 跳板设置(含它从分组继承来的),所以不能只传目标那一条。
pub fn expand_chain(
    target: SessionId,
    sessions: &BTreeMap<SessionId, SessionRecord>,
    groups: &BTreeMap<crate::model::GroupId, crate::group::GroupRecord>,
) -> Result<Vec<SessionId>, StoreError> {
    let rec = sessions
        .get(&target)
        .ok_or(StoreError::JumpDangling(target))?;
    let chain = resolve(&layers_for(rec, groups)).jump;
    expand_from(Some(target), &chain, sessions, groups)
}

/// 从一条**已给定**的跳板链展开(F92)。发起方是尚未保存的草稿:它还没有
/// id,不可能被任何已存记录引用,因此不参与环检测。
///
/// 除入口外,与 `expand_chain` 共用同一个内核 —— 草稿路径和已保存路径
/// 的展开语义(递归、去重、环/悬空/超深判定)必须完全一致,否则「测试通过
/// 但保存后连不上」。
pub fn expand_chain_of(
    chain: &[crate::network::JumpRef],
    sessions: &BTreeMap<SessionId, SessionRecord>,
    groups: &BTreeMap<crate::model::GroupId, crate::group::GroupRecord>,
) -> Result<Vec<SessionId>, StoreError> {
    expand_from(None, chain, sessions, groups)
}

/// 两个入口共用的内核。`origin` 只用于环检测入栈与错误定位;
/// `None` = 发起方尚未入库。
fn expand_from(
    origin: Option<SessionId>,
    chain: &[crate::network::JumpRef],
    sessions: &BTreeMap<SessionId, SessionRecord>,
    groups: &BTreeMap<crate::model::GroupId, crate::group::GroupRecord>,
) -> Result<Vec<SessionId>, StoreError> {
    let mut out = Vec::new();
    let mut on_stack: Vec<SessionId> = origin.into_iter().collect();
    for hop in chain {
        visit(hop.0, sessions, groups, &mut out, &mut on_stack)?;
        if !out.contains(&hop.0) {
            out.push(hop.0);
        }
    }
    if out.len() > MAX_JUMP_DEPTH {
        // out 非空(len > MAX),`last()` 必有值。
        return Err(StoreError::JumpTooDeep(origin.unwrap_or_else(|| {
            *out.last().expect("out.len() > MAX_JUMP_DEPTH 时必非空")
        })));
    }
    Ok(out)
}

/// 后序 DFS:先把 `id` 的每个跳板(及其自身的跳板)压进 `out`,`id` 自己由调用方负责。
///
/// `on_stack` 是当前递归路径,用于环检测;`out` 兼作去重集合(菱形引用只连一次)。
fn visit(
    id: SessionId,
    sessions: &BTreeMap<SessionId, SessionRecord>,
    groups: &BTreeMap<crate::model::GroupId, crate::group::GroupRecord>,
    out: &mut Vec<SessionId>,
    on_stack: &mut Vec<SessionId>,
) -> Result<(), StoreError> {
    if on_stack.contains(&id) {
        return Err(StoreError::JumpCycle(id));
    }
    if on_stack.len() > MAX_JUMP_DEPTH {
        return Err(StoreError::JumpTooDeep(id));
    }
    let rec = sessions.get(&id).ok_or(StoreError::JumpDangling(id))?;

    // 跳板会话自身的跳板链也要走继承(它可能属于某个配了统一代理/跳板的分组)。
    let layers = layers_for(rec, groups);
    let chain = resolve(&layers).jump;

    on_stack.push(id);
    for hop in chain {
        visit(hop.0, sessions, groups, out, on_stack)?;
        if !out.contains(&hop.0) {
            out.push(hop.0);
        }
    }
    on_stack.pop();

    if out.len() > MAX_JUMP_DEPTH {
        return Err(StoreError::JumpTooDeep(id));
    }
    Ok(())
}

/// 组装继承层序:`[会话, 分组]`(优先级从高到低)。悬空 `group_id` 沿用
/// P0-a 既有的静默降级——**仅限分组**,跳板悬空是另一回事,必须硬失败。
fn layers_for<'a>(
    rec: &'a SessionRecord,
    groups: &'a BTreeMap<crate::model::GroupId, crate::group::GroupRecord>,
) -> Vec<&'a dyn PrefsLayer> {
    let mut layers: Vec<&dyn PrefsLayer> = vec![rec];
    if let Some(g) = rec.identity.group_id.and_then(|gid| groups.get(&gid)) {
        layers.push(g);
    }
    layers
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::group::GroupRecord;
    use crate::model::{
        AppearancePrefs, Auth, AuthKind, Connection, GroupId, Identity, Protocol, TerminalPrefs,
    };
    use crate::network::{JumpRef, NetworkPrefs};

    fn rec(id: u64, jump: Vec<u64>) -> SessionRecord {
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
                host: format!("h{id}"),
                port: 22,
                protocol: Protocol::Ssh,
            },
            auth: Auth {
                user: "u".into(),
                kind: AuthKind::Password,
            },
            terminal: TerminalPrefs::default(),
            appearance: AppearancePrefs::default(),
            network: NetworkPrefs {
                proxy: None,
                jump: Some(jump.into_iter().map(|i| JumpRef(SessionId(i))).collect()),
            },
            automation: crate::automation::AutomationPrefs::default(),
            sftp: crate::sftp::SftpPrefs::default(),
        }
    }

    fn index(records: Vec<SessionRecord>) -> BTreeMap<SessionId, SessionRecord> {
        records.into_iter().map(|r| (r.id, r)).collect()
    }

    fn no_groups() -> BTreeMap<GroupId, GroupRecord> {
        BTreeMap::new()
    }

    #[test]
    fn direct_session_has_empty_chain() {
        let idx = index(vec![rec(1, vec![])]);
        let got = expand_chain(SessionId(1), &idx, &no_groups()).unwrap();
        assert!(got.is_empty(), "无跳板会话应展开成空链");
    }

    #[test]
    fn single_hop_returns_that_hop() {
        let idx = index(vec![rec(1, vec![2]), rec(2, vec![])]);
        let got = expand_chain(SessionId(1), &idx, &no_groups()).unwrap();
        assert_eq!(got, vec![SessionId(2)]);
    }

    /// 展开规则 2:跳板自身的跳板要递归展开,插在它之前。
    #[test]
    fn nested_jump_expands_transitively_in_dial_order() {
        // 目标 1 → 经 2;而 2 自己又要经 3。拨号顺序必须是 3 → 2 → 1。
        let idx = index(vec![rec(1, vec![2]), rec(2, vec![3]), rec(3, vec![])]);
        let got = expand_chain(SessionId(1), &idx, &no_groups()).unwrap();
        assert_eq!(
            got,
            vec![SessionId(3), SessionId(2)],
            "递归展开的跳板必须排在引用它的那一跳之前"
        );
    }

    #[test]
    fn multi_hop_preserves_declared_order() {
        let idx = index(vec![rec(1, vec![2, 3]), rec(2, vec![]), rec(3, vec![])]);
        let got = expand_chain(SessionId(1), &idx, &no_groups()).unwrap();
        assert_eq!(got, vec![SessionId(2), SessionId(3)], "声明顺序即拨号顺序");
    }

    #[test]
    fn cycle_is_rejected_not_silently_truncated() {
        let idx = index(vec![rec(1, vec![2]), rec(2, vec![1])]);
        let err = expand_chain(SessionId(1), &idx, &no_groups()).unwrap_err();
        assert!(
            matches!(err, StoreError::JumpCycle(_)),
            "环必须报错,实际: {err:?}"
        );
    }

    #[test]
    fn self_reference_is_a_cycle() {
        let idx = index(vec![rec(1, vec![1])]);
        let err = expand_chain(SessionId(1), &idx, &no_groups()).unwrap_err();
        assert!(matches!(err, StoreError::JumpCycle(_)));
    }

    #[test]
    fn dangling_reference_is_rejected_never_degraded_to_direct() {
        // 安全属性:静默降级会让用户以为流量过了堡垒机,实际直连。
        let idx = index(vec![rec(1, vec![42])]);
        let err = expand_chain(SessionId(1), &idx, &no_groups()).unwrap_err();
        assert!(
            matches!(err, StoreError::JumpDangling(SessionId(42))),
            "悬空引用必须报错,实际: {err:?}"
        );
    }

    #[test]
    fn chain_longer_than_max_depth_is_rejected() {
        // 1 → 2 → 3 → ... → 11,展开后 10 跳,超过 MAX_JUMP_DEPTH(8)。
        let mut records = Vec::new();
        for id in 1..=10u64 {
            records.push(rec(id, vec![id + 1]));
        }
        records.push(rec(11, vec![]));
        let idx = index(records);
        let err = expand_chain(SessionId(1), &idx, &no_groups()).unwrap_err();
        assert!(
            matches!(err, StoreError::JumpTooDeep(_)),
            "超深必须报错,实际: {err:?}"
        );
    }

    /// 构造一条长度为 `hops` 的线性跳板链:1 → 2 → … → hops+1,最后一个无跳板。
    /// `expand_chain(SessionId(1), ...)` 应展开出恰好 `hops` 个会话 id。
    fn linear_chain(hops: u64) -> BTreeMap<SessionId, SessionRecord> {
        let mut records = Vec::new();
        for id in 1..=hops {
            records.push(rec(id, vec![id + 1]));
        }
        records.push(rec(hops + 1, vec![]));
        index(records)
    }

    // 下面两条钉住深度上限的 off-by-one 边界:`on_stack.len() > MAX_JUMP_DEPTH` 若被
    // 误改成 `>=`,现有测试(用远超上限的 10 跳链)大概率仍全绿,因为没有测试卡在
    // 边界正中央。必须各写一条「恰好等于上限」和「刚好超一跳」,且用 `MAX_JUMP_DEPTH`
    // 常量算链长而非硬编码数字,否则将来调常量这两条测试也会假绿。

    /// 恰好 `MAX_JUMP_DEPTH` 跳的线性链必须成功,且返回长度精确等于 `MAX_JUMP_DEPTH`——
    /// 只断言 `is_ok()` 抓不住「多算/少算一跳」这类错误,必须钉住长度。
    #[test]
    fn chain_of_exactly_max_depth_succeeds_with_full_length() {
        let idx = linear_chain(MAX_JUMP_DEPTH as u64);
        let got = expand_chain(SessionId(1), &idx, &no_groups()).unwrap();
        assert_eq!(
            got.len(),
            MAX_JUMP_DEPTH,
            "恰好等于上限的链应该成功,且长度精确等于 MAX_JUMP_DEPTH"
        );
    }

    /// 比上限多一跳(`MAX_JUMP_DEPTH + 1`)的线性链必须报错。
    #[test]
    fn chain_of_max_depth_plus_one_is_rejected() {
        let idx = linear_chain(MAX_JUMP_DEPTH as u64 + 1);
        let err = expand_chain(SessionId(1), &idx, &no_groups()).unwrap_err();
        assert!(
            matches!(err, StoreError::JumpTooDeep(_)),
            "刚好超一跳必须报错,实际: {err:?}"
        );
    }

    /// 同一台跳板被两条支路引用不算环,但只连一次。
    #[test]
    fn diamond_reference_dedups_without_reporting_cycle() {
        // 1 → [2, 3];2 → 4;3 → 4。4 只该出现一次,且在 2 之前。
        let idx = index(vec![
            rec(1, vec![2, 3]),
            rec(2, vec![4]),
            rec(3, vec![4]),
            rec(4, vec![]),
        ]);
        let got = expand_chain(SessionId(1), &idx, &no_groups()).unwrap();
        assert_eq!(got, vec![SessionId(4), SessionId(2), SessionId(3)]);
    }

    /// 展开中间跳板时也要走继承:B 自己没配链但它所在分组配了,
    /// 展开 A 时必须把 B 继承来的那一跳也带上。
    /// 若这里直接读 `rec.network.jump` 而不经 `resolve`,组级堡垒机会被静默跳过——
    /// 用户以为多了一层防护,实际没有。
    #[test]
    fn intermediate_jump_inherits_its_own_group_chain() {
        let mut b = rec(2, vec![]);
        b.network.jump = None; // 未设置 = 继承
        b.identity.group_id = Some(GroupId(7));
        let idx = index(vec![rec(1, vec![2]), b, rec(3, vec![])]);

        let mut g = GroupRecord {
            id: GroupId(7),
            name: "生产".into(),
            tags: Vec::new(),
            terminal: TerminalPrefs::default(),
            appearance: AppearancePrefs::default(),
            network: NetworkPrefs::default(),
            automation: crate::automation::AutomationPrefs::default(),
        };
        g.network.jump = Some(vec![JumpRef(SessionId(3))]);
        let groups: BTreeMap<GroupId, GroupRecord> = [(GroupId(7), g)].into_iter().collect();

        let got = expand_chain(SessionId(1), &idx, &groups).unwrap();
        assert_eq!(
            got,
            vec![SessionId(3), SessionId(2)],
            "B 继承来的跳板 3 必须排在 B 之前"
        );
    }

    /// 中间跳板自己配的 proxy 与本次拨号无关:代理只在**第一跳出本机**时用一次,
    /// 后续跳都在隧道里。`expand_chain` 只返回会话 id,不该也不能把 proxy 带出来——
    /// 这个测试钉住「返回值里没有代理」这个契约。
    #[test]
    fn intermediate_jump_proxy_is_not_part_of_the_chain() {
        let mut b = rec(2, vec![]);
        b.network.proxy = Some(crate::network::ProxyChoice::Socks5(
            crate::network::ProxyEndpoint {
                host: "should-be-ignored".into(),
                port: 1080,
                user: None,
            },
        ));
        let idx = index(vec![rec(1, vec![2]), b]);
        let got = expand_chain(SessionId(1), &idx, &no_groups()).unwrap();
        assert_eq!(got, vec![SessionId(2)], "链里只有会话 id,代理不参与");
    }

    /// F92:从一条**给定的**跳板链展开,发起方不必存在于索引里 ——
    /// 「测试连接」拨的是还没保存、还没有 id 的草稿。
    ///
    /// 自证变红的方式:把 `expand_chain_of` 改成忽略 `chain` 参数、
    /// 直接返回空 vec。
    #[test]
    fn chain_of_expands_without_an_existing_origin_session() {
        // 草稿要经 2,而 2 自己要经 3 —— 拨号顺序必须是 3 → 2。
        let idx = index(vec![rec(2, vec![3]), rec(3, vec![])]);
        let chain = vec![JumpRef(SessionId(2))];
        let got = expand_chain_of(&chain, &idx, &no_groups()).unwrap();
        assert_eq!(got, vec![SessionId(3), SessionId(2)]);
    }

    /// F92:草稿路径的安全属性一个都不能少 —— 悬空跳板照样硬失败,
    /// 绝不静默降级成直连(用户会以为流量过了堡垒机)。
    ///
    /// 自证变红的方式:把 `visit` 里的 `.ok_or(StoreError::JumpDangling(id))?`
    /// 改成 `else { return Ok(()) }`。
    #[test]
    fn chain_of_still_rejects_dangling_and_cyclic_hops() {
        let idx = index(vec![rec(2, vec![])]);
        let err = expand_chain_of(&[JumpRef(SessionId(42))], &idx, &no_groups()).unwrap_err();
        assert!(
            matches!(err, StoreError::JumpDangling(SessionId(42))),
            "悬空引用必须报错,实际: {err:?}"
        );

        let cyc = index(vec![rec(1, vec![2]), rec(2, vec![1])]);
        let err = expand_chain_of(&[JumpRef(SessionId(1))], &cyc, &no_groups()).unwrap_err();
        assert!(
            matches!(err, StoreError::JumpCycle(_)),
            "环必须报错,实际: {err:?}"
        );
    }
}
