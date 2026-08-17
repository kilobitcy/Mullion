//! F121:左栏拖拽排序的判定层。**零 egui**,可纯单测。
//!
//! UI 只负责「指针在哪一行的哪半边」,落点算什么意图全在这里 ——
//! 判据散在渲染代码里的话,「拖到组内最后一行的下半」这类边界只能靠手点。

use mullion_store::{GroupId, SessionId};

/// 一次拖拽的结论。字段与 `Vault::move_session` 的参数一一对应,
/// 中间不做二次翻译。
///
/// **必须是 `pub`**(不是 `pub(crate)`):挂在 `UiState::reorder_request`
/// 这个 `pub` 字段上,字段可见性不能超过类型本身,否则是
/// `private_interfaces` 编译警告(`-D warnings` 下会直接编译失败)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReorderIntent {
    pub id: SessionId,
    pub group: Option<GroupId>,
    /// 插在谁前面。`None` = 该组末尾。
    pub before: Option<SessionId>,
}

/// 松手落在某一行上。`next_in_group` = 被悬停行在**该组内**的下一条
/// (组内最后一行传 `None`)。
///
/// 上半 → 插在它前面;下半 → 插在它后面(= 它的下一条前面)。
/// 拖到自己身上 → `None`,什么都不做。
pub(crate) fn drop_on_row(
    dragged: SessionId,
    over: SessionId,
    over_group: Option<GroupId>,
    next_in_group: Option<SessionId>,
    upper_half: bool,
) -> Option<ReorderIntent> {
    if dragged == over {
        return None;
    }
    Some(ReorderIntent {
        id: dragged,
        group: over_group,
        before: if upper_half {
            Some(over)
        } else {
            next_in_group
        },
    })
}

/// 松手落在分组头上 → 插到该组末尾。折叠的组、空组都只能从这里进。
pub(crate) fn drop_on_group(dragged: SessionId, group: Option<GroupId>) -> ReorderIntent {
    ReorderIntent {
        id: dragged,
        group,
        before: None,
    }
}

/// 被悬停行在**该组内**的下一条(组内最后一行 → `None`)。
///
/// 从 `list.rs::show` 的渲染循环里抽出来,单独钉住这一条算术:调用方必须传
/// **这一帧真正可见的那一组成员**(过滤过搜索词、过滤过 `Icons` 档隐藏行
/// 之后的 `members`),传错成 `matched` 或 `sessions` 的话,组内最后一行的
/// 下半区落点会算错——`drop_on_row` 会把它接在一条实际不存在于本组可见
/// 顺序里的行前面。
pub(crate) fn next_in_group(members: &[SessionId], i: usize) -> Option<SessionId> {
    members.get(i + 1).copied()
}

/// 这一帧准不准拖(设计 D9)。
///
/// 搜索中、或 `Icons` 档下都有行被藏起来,此时**可见顺序 ≠ 真实顺序**:
/// 松手落在两行之间,到底插在哪一条隐藏行的前后是歧义的。与其猜一个,
/// 不如不让拖 —— 猜错的代价是用户的顺序被悄悄改成他没要的样子。
///
/// `Density` 三档里只有 `Icons` 会整条隐藏行(见 `list.rs` 里
/// `d == Density::Icons` 那段过滤);`Compact` 只是收窄文字排版,
/// 行数不变,可见顺序仍是真实顺序,允许拖。
pub(crate) fn drag_enabled(query: &str, density: super::list::Density) -> bool {
    query.trim().is_empty() && density != super::list::Density::Icons
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::session_manager::list::Density;

    fn sid(n: u64) -> SessionId {
        SessionId(n)
    }

    #[test]
    fn upper_half_inserts_before_the_hovered_row() {
        let i = drop_on_row(sid(1), sid(2), None, Some(sid(3)), true).unwrap();
        assert_eq!(i.before, Some(sid(2)));
    }

    /// 自证会变红:把 `upper_half` 的两个分支对调。
    #[test]
    fn lower_half_inserts_before_the_next_row() {
        let i = drop_on_row(sid(1), sid(2), None, Some(sid(3)), false).unwrap();
        assert_eq!(i.before, Some(sid(3)));
    }

    /// 组内最后一行的下半区 = 组末尾。没有这条,拖到列表最下面会没反应。
    #[test]
    fn lower_half_of_the_last_row_means_the_end_of_the_group() {
        let i = drop_on_row(sid(1), sid(2), None, None, false).unwrap();
        assert_eq!(i.before, None);
    }

    #[test]
    fn dropping_a_row_onto_itself_does_nothing() {
        assert!(drop_on_row(sid(1), sid(1), None, None, true).is_none());
    }

    /// 跨组:目标行所在的组就是新组。
    #[test]
    fn the_hovered_rows_group_becomes_the_new_group() {
        let g = GroupId(9);
        let i = drop_on_row(sid(1), sid(2), Some(g), None, true).unwrap();
        assert_eq!(i.group, Some(g));
    }

    #[test]
    fn dropping_on_a_group_header_appends_to_that_group() {
        let g = GroupId(9);
        let i = drop_on_group(sid(1), Some(g));
        assert_eq!((i.group, i.before), (Some(g), None));
    }

    /// D9 的门控。自证会变红:把 `drag_enabled` 改成恒 `true`。
    #[test]
    fn dragging_is_off_while_filtering_because_the_visible_order_is_not_the_real_one() {
        assert!(drag_enabled("", Density::Full));
        assert!(!drag_enabled("web", Density::Full), "搜索中不许拖");
        assert!(!drag_enabled("  ", Density::Icons), "图标档藏了行,不许拖");
        assert!(!drag_enabled("", Density::Icons));
    }

    /// 覆盖缺口检查:三档只测了两档的话,中间那档「藏不藏行」就是没验证过
    /// 的假设。`Compact` 不隐藏行(只收窄文字排版),可见顺序仍是真实顺序,
    /// 必须允许拖 —— 依据见 `list.rs` 里 `d == Density::Icons` 才触发的
    /// 隐藏过滤,`Compact` 落进 `else` 分支走 `matched.clone()` 全量保留。
    #[test]
    fn compact_density_does_not_hide_rows_so_dragging_stays_allowed() {
        assert!(drag_enabled("", Density::Compact));
    }

    #[test]
    fn next_in_group_returns_the_following_member() {
        let members = [sid(1), sid(2), sid(3)];
        assert_eq!(next_in_group(&members, 0), Some(sid(2)));
        assert_eq!(next_in_group(&members, 1), Some(sid(3)));
    }

    /// 组内最后一行没有下一条。没有这条,拖到组尾会被算成插到某个不存在的
    /// 越界位置而不是「组末尾」。
    #[test]
    fn next_in_group_is_none_for_the_last_member() {
        let members = [sid(1), sid(2)];
        assert_eq!(next_in_group(&members, 1), None);
    }

    /// 单成员组:唯一一行既是第一条也是最后一条。
    #[test]
    fn next_in_group_is_none_for_a_single_member_group() {
        assert_eq!(next_in_group(&[sid(1)], 0), None);
    }
}
