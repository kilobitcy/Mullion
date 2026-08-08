//! 继承槽(走查 10 / 11)。
//!
//! 走查报的是两件事:选了「继承」之后**不知道继承到了什么**,以及六处继承
//! 字段各写各的说明文字。这里统一的是「继承槽这个部件」——**不是**把六处
//! 都压成同一种控件形态。态数各字段自己定:代理是四态(继承/直连/SOCKS5/
//! HTTP)、跳板是三态、总开关是三态、时序是「勾/不勾」。硬要统一成二段开关
//! 会毁掉「继承」与「显式关闭」的区分,那是 P0-b 和 P1-b 各踩过一次的坑。
//!
//! `effective_line` 是纯函数:文案里少写一个来源、把「内置默认」和「来自
//! 分组」搞反,都不会有编译错误也不会 panic,只会让用户按着错误的心智模型
//! 去改分组配置。

use egui::Ui;
use mullion_store::{GroupId, GroupRecord};

use crate::theme::Theme;

/// 继承链上**真正生效的那一层**。
///
/// 分组是单层的,所以上游只有一级 —— 这里能把结果算准,
/// 不必含糊地说「跟随上游」。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Source<'a> {
    /// 上游分组配了值。
    Group(&'a str),
    /// 全链路都没配,落到内置默认。
    Builtin,
    /// 当前会话未分组(或分组已被删),没有上游可继承。
    NoUpstream,
}

/// 「实际生效:X(来源)」这一行灰字的文本。
///
/// 三种来源都要在文本里点名。只说「实际生效:开」的话,用户无法判断
/// 「去分组里改一下能不能影响到这条会话」—— 走查 10 报的正是这个。
pub(super) fn effective_line(value: &str, source: Source<'_>) -> String {
    match source {
        Source::Group(name) => format!("实际生效:{value}(来自分组「{name}」)"),
        Source::Builtin => format!("实际生效:{value}(内置默认)"),
        Source::NoUpstream => format!("实际生效:{value}(未分组,没有上游可继承)"),
    }
}

/// 当前会话的上游分组。悬空 `group_id` 静默当未分组 —— 与
/// `inherit::layers` 对分组的既有降级一致。
///
/// (悬空**跳板**是另一回事,那里必须硬失败:用户会以为流量过了堡垒机
/// 而实际没有。分组只影响偏好取值,降级不产生安全后果。)
pub(super) fn upstream(group_id: Option<GroupId>, groups: &[GroupRecord]) -> Option<&GroupRecord> {
    group_id.and_then(|gid| groups.iter().find(|g| g.id == gid))
}

/// 继承槽的统一排版:左边是本字段自己的控件,右边跟一行「实际生效…」灰字。
///
/// `line` 为 `None` 时只画控件 —— 用户显式填了值的时候再解释一遍
/// 「实际生效 X」是噪音,他填的就是 X。判据由调用方给,这里不猜。
///
/// 用 `horizontal_wrapped` 而不是 `horizontal`:灰字比控件长得多
/// (「实际生效:SOCKS5 127.0.0.1:7891(来自分组「生产」)」将近 30 个字),
/// 不换行会把这一格撑宽、顶出面板 —— 走查 P0-1 的同族缺陷,阶段 1 已经
/// 在代理模式按钮和命令行上各踩过一次。
pub(super) fn slot(ui: &mut Ui, t: &Theme, control: impl FnOnce(&mut Ui), line: Option<String>) {
    ui.horizontal_wrapped(|ui| {
        control(ui);
        if let Some(s) = line {
            ui.colored_label(crate::theme::c32(t.fg_dimmer), s);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_store::{GroupId, GroupRecord};

    fn group(id: u64, name: &str) -> GroupRecord {
        GroupRecord {
            id: GroupId(id),
            name: name.to_string(),
            tags: Vec::new(),
            terminal: Default::default(),
            appearance: Default::default(),
            network: Default::default(),
            automation: Default::default(),
        }
    }

    /// 三种来源必须能从文本上分辨。用户看到「实际生效:开」而不知道这个
    /// 「开」是分组配的还是内置默认,他就没法判断「改分组能不能影响到这里」——
    /// 这正是走查 10 报的缺陷。
    #[test]
    fn every_source_is_named_in_the_line() {
        assert_eq!(
            effective_line("自动 attach", Source::Group("生产")),
            "实际生效:自动 attach(来自分组「生产」)"
        );
        assert_eq!(
            effective_line("300 ms", Source::Builtin),
            "实际生效:300 ms(内置默认)"
        );
        assert_eq!(
            effective_line("不走跳板", Source::NoUpstream),
            "实际生效:不走跳板(未分组,没有上游可继承)"
        );
    }

    /// 分组名要原样出现在文本里 —— 用户得能照着这个名字去分组管理器里找。
    #[test]
    fn group_name_is_not_truncated_or_escaped() {
        let line = effective_line("SOCKS5 127.0.0.1:7891", Source::Group("代理走这台"));
        assert!(line.contains("代理走这台"), "分组名丢了: {line}");
        assert!(line.contains("SOCKS5 127.0.0.1:7891"), "生效值丢了: {line}");
    }

    /// 未分组、悬空分组 id 都必须落到 `None`。悬空 id 静默当未分组处理,
    /// 与继承层对分组的既有降级一致(悬空分组不是安全属性,悬空**跳板**才是)。
    #[test]
    fn upstream_resolves_none_for_missing_and_dangling_group() {
        let gs = vec![group(1, "生产"), group(2, "测试")];
        assert_eq!(upstream(None, &gs).map(|g| g.name.as_str()), None);
        assert_eq!(
            upstream(Some(GroupId(9)), &gs).map(|g| g.name.as_str()),
            None,
            "悬空分组 id 应静默当未分组"
        );
        assert_eq!(
            upstream(Some(GroupId(2)), &gs).map(|g| g.name.as_str()),
            Some("测试")
        );
    }
}
