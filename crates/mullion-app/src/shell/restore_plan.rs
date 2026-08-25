//! F161/F162:恢复一个标签时,每个叶子该怎么处理 —— 主叶子选谁、哪些叶子要
//! 另拨一台机器、谁的 `attach` 该带 `-d`。**纯函数,零 egui/winit/tokio/store IO**。
//!
//! 这些判据全是「错了也看不出来、直到某天接到别人的会话上」的那一类,所以
//! 一条都不留在 `app.rs` 的事件分支里(那里要一个真的 `App` + 真的 SSH 连接
//! 才跑得起来,等于只能靠人眼验)。

use mullion_store::SessionId;

use crate::shell::layout_snapshot::LeafIdentity;

/// 一个叶子在恢复时该走哪条路(F162,设计 5.2)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeafPlan {
    /// 跟主叶子同一条会话:在**已有**的那条 SSH 连接上另开一条 channel
    /// (今天 `spawn_fresh_panes` 走的路)。
    SameHost,
    /// 另一台机器:排进串行拨号队列,走「换节点」那条链路(D10)。
    Dial(SessionId),
    /// 会话已经不在库里(用户把它删了)。摆出形状、挂一句说明,**不拨号**(D3)。
    ///
    /// 不丢掉这个叶子:丢了分屏比例会静默变形 —— 存的是 2×2,恢复回来变三块,
    /// 而没有任何提示。
    Orphan,
}

/// F162:哪个叶子当「主叶子」—— **前序第一个身份还连得上**的叶子。
///
/// 它决定标签用哪条会话去 `spawn_connect`(今天那条路,零改动),连上之后
/// 已有的那块 pane 就落在这个叶子位上(见 `Workspace::apply_saved_tree` 的
/// `main_leaf` 参数,设计 5.2①)。
///
/// `known` 回答「这条会话现在还在库里吗」。传闭包而不是 `&SessionStore`:
/// store 打不开时(keyring 不可用)照样要能跑这段判据,而且这样才测得动
/// (同 `layout_snapshot::usable`)。
///
/// `None` = 一个能连的叶子都没有 —— 整个标签保持占位态。
pub fn main_leaf(
    identities: &[LeafIdentity],
    known: &dyn Fn(SessionId) -> bool,
) -> Option<(usize, SessionId)> {
    identities.iter().enumerate().find_map(|(ix, i)| {
        let s = i.session_id?;
        known(s).then_some((ix, s))
    })
}

/// F162:每个叶子的路由。`main` = [`main_leaf`] 选出来的那条会话。
pub fn plan_leaves(
    identities: &[LeafIdentity],
    main: SessionId,
    known: &dyn Fn(SessionId) -> bool,
) -> Vec<LeafPlan> {
    identities
        .iter()
        .map(|i| match i.session_id {
            Some(s) if s == main => LeafPlan::SameHost,
            Some(s) if known(s) => LeafPlan::Dial(s),
            // 会话被删了 / 压根没有身份:摆出来,不拨号。
            _ => LeafPlan::Orphan,
        })
        .collect()
}

/// D5:一批要 attach 的叶子里,哪几块该带 `-d`。
///
/// **键是(机器, 会话名)二元组**,不是会话名。pane A 在机器 X 的会话 `a`、
/// pane B 在机器 Y 的会话 `a` 是两台 tmux 服务器上两个互不相干的会话,
/// **都**该带 `-d`(各踢各的残骸);只按名字去重会让 B 白白不踢。
/// 机器一侧用叶子的 `session_id` —— 同一条会话记录 = 同一台机器,恢复场景里
/// 没有比它更细的机器身份。
///
/// 为什么第一块要带 `-d`:exe 崩溃/强杀之后远端 tmux client 会残留到 TCP 超时,
/// 不踢的话两个 client 同时挂着,tmux 的 `window-size` 会跟着两边尺寸反复
/// reflow(F141 的原始理由)。
/// 为什么其余不能带:第二块会把第一块踢成 detached,恢复出来一块死屏。
pub fn detach_flags(leaves: &[LeafIdentity]) -> Vec<bool> {
    let mut seen: Vec<(Option<SessionId>, &str)> = Vec::new();
    leaves
        .iter()
        .map(|i| {
            let Some(name) = i.tmux.as_deref() else {
                // 不发 attach 的叶子不占同名那一格,否则真正要 attach 的
                // 那块就拿不到 `-d` 了。
                return false;
            };
            let key = (i.session_id, name);
            if seen.contains(&key) {
                false
            } else {
                seen.push(key);
                true
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(session: u64, tmux: Option<&str>) -> LeafIdentity {
        LeafIdentity {
            session_id: Some(SessionId(session)),
            tmux: tmux.map(str::to_string),
        }
    }

    fn nothing() -> LeafIdentity {
        LeafIdentity::default()
    }

    /// 主叶子是前序第一个**连得上**的,不是恒第 0 个。
    ///
    /// 自证会变红:把 `main_leaf` 改成恒返回 `Some((0, ..))`。
    #[test]
    fn the_main_leaf_is_the_first_one_that_can_still_be_dialed() {
        let ids = [leaf(7, None), leaf(3, Some("a"))];
        // 会话 7 被用户删了 → 主叶子该是第 1 个。
        let got = main_leaf(&ids, &|s| s == SessionId(3));
        assert_eq!(got, Some((1, SessionId(3))));
    }

    #[test]
    fn a_tab_with_no_dialable_leaf_has_no_main_leaf() {
        assert_eq!(main_leaf(&[leaf(7, None)], &|_| false), None);
        assert_eq!(main_leaf(&[nothing()], &|_| true), None);
        assert_eq!(main_leaf(&[], &|_| true), None);
    }

    /// F162:同一条会话的叶子复用已有连接,别的会话排队另拨。
    ///
    /// 自证会变红:把 `plan_leaves` 里的 `s == main` 判断去掉(全变 `Dial`)——
    /// 那样恢复一个普通的 2×2 单机标签会凭空拨 3 次号,每次一个密码框。
    #[test]
    fn leaves_on_the_main_session_reuse_the_connection_and_the_rest_get_queued() {
        let ids = [leaf(3, Some("a")), leaf(3, None), leaf(7, Some("b"))];
        let got = plan_leaves(&ids, SessionId(3), &|_| true);
        assert_eq!(
            got,
            vec![
                LeafPlan::SameHost,
                LeafPlan::SameHost,
                LeafPlan::Dial(SessionId(7))
            ]
        );
    }

    /// D3(按 plan 开头「与 spec 的偏差①」调整过入口):会话被删掉的叶子
    /// **摆出来**,不丢掉。丢掉的话分屏比例会静默变形 —— 存的是 2×2,
    /// 恢复回来变三块,而没有任何提示。
    ///
    /// 自证会变红:把 `Orphan` 那条分支删掉、改成不产出这个叶子。
    #[test]
    fn a_leaf_whose_session_is_gone_is_kept_as_a_placeholder_not_dropped() {
        let ids = [leaf(3, Some("a")), leaf(7, Some("b")), leaf(3, None)];
        let got = plan_leaves(&ids, SessionId(3), &|s| s == SessionId(3));
        assert_eq!(
            got.len(),
            3,
            "叶子数必须与树的叶子数一一对应,少一个就是变形"
        );
        assert_eq!(got[1], LeafPlan::Orphan);
    }

    /// 身份完全缺失的叶子(理论上不可达,见 plan 开头的偏差①)同样按占位处理,
    /// **不许 panic、不许丢**。
    #[test]
    fn a_leaf_with_no_identity_at_all_is_also_a_placeholder() {
        let got = plan_leaves(&[nothing()], SessionId(3), &|_| true);
        assert_eq!(got, vec![LeafPlan::Orphan]);
    }

    /// D5 的核心:键是(机器, 会话名)。**两台机器上的同名会话都要带 `-d`**。
    ///
    /// 自证会变红:把去重键退化成只按会话名。
    #[test]
    fn the_detach_flag_is_keyed_per_host_and_session_name() {
        let ids = [leaf(3, Some("a")), leaf(7, Some("a"))];
        assert_eq!(
            detach_flags(&ids),
            vec![true, true],
            "两台机器上各自的会话 a 互不相干,都该各踢各的残骸"
        );
    }

    /// D5 的另一半:**同机同名**只有第一块带 `-d`。第二块带的话会把第一块
    /// 踢成 detached,恢复出来一块死屏。
    ///
    /// 自证会变红:全加 `-d`(第二个断言红)/ 全不加(第一个断言红)——
    /// 两个方向各扎一条,不许互相掩护。
    #[test]
    fn only_the_first_pane_on_the_same_host_session_gets_the_detach_flag() {
        let ids = [leaf(3, Some("a")), leaf(3, Some("a")), leaf(3, Some("b"))];
        assert_eq!(detach_flags(&ids), vec![true, false, true]);
    }

    /// 没有实测名的叶子根本不发 attach —— 它的标志位是什么无所谓,
    /// 但**绝不能占掉同名那一格**,否则真正要 attach 的那块就不带 `-d` 了。
    #[test]
    fn a_leaf_without_a_measured_name_does_not_consume_the_detach_slot() {
        let ids = [leaf(3, None), leaf(3, Some("a"))];
        assert_eq!(detach_flags(&ids), vec![false, true]);
    }
}
