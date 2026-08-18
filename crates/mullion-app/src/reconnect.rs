//! F128:断线自动重连的**判据层**。
//!
//! 这里只有纯函数 —— 拨号本身要真网络,但「拨不拨、拨哪条、等多久、
//! 什么时候放弃」全都能在无头容器里单测。

use std::time::Duration;

use mullion_core::layout::PaneId;

use crate::shell::workspace::PaneStatus;

/// 这一帧要为哪些 host 发起重拨。
///
/// **按 host 去重**:adr-009 下一条连接承载多个 pane,4 块分屏一起断时
/// 拨 4 次等于 4 次认证 + 远端 4 条登录记录。
/// `in_flight` 是已经在拨的 host,重复发起会让帧循环每秒拨几十次。
pub fn hosts_to_redial(panes: &[(PaneId, usize, PaneStatus)], in_flight: &[usize]) -> Vec<usize> {
    let mut seen: Vec<usize> = Vec::new();
    for (_, host_ix, status) in panes {
        if *status != PaneStatus::Reconnecting {
            continue;
        }
        if in_flight.contains(host_ix) || seen.contains(host_ix) {
            continue;
        }
        seen.push(*host_ix);
    }
    seen
}

/// 第 `attempt` 次重试前该等多久(`attempt` 从 1 开始,同
/// `mullion_ssh::tunnel::backoff_delay` 的约定)。`None` = 到顶,放弃。
///
/// 直接转发隧道那套表(`mullion_ssh::tunnel::backoff_delay`):两套退避会在
/// 同一条链路抖动时互相打架,用户看到的"多久重试一次"也会因为断的是隧道还是
/// 终端而不同。
pub fn delay_for(attempt: u32) -> Option<Duration> {
    mullion_ssh::tunnel::backoff_delay(attempt)
}

/// 第 `attempt` 次重拨失败之后,pane 该是什么状态。
/// 退避到顶就落回 `Disconnected`,由用户自己决定重连还是关掉 —— 一直重试
/// 到天荒地老的话,一台已经拆掉的服务器会让客户端永远挂着一个后台任务。
pub fn status_after_failure(attempt: u32) -> PaneStatus {
    match delay_for(attempt) {
        Some(_) => PaneStatus::Reconnecting,
        None => PaneStatus::Disconnected,
    }
}

/// 喂进 emulator 的那一行屏内提示(§7.3)。
///
/// **不做倒计时**:那要在帧循环里再引一个 deadline,正是 spec §1 修订一要
/// 避免的东西(同 `automation_status` 不做定时淡出)。前后各一个 `\r\n`:
/// 前面那个保证不覆盖远端最后一行输出,后面那个让重连成功后的新输出从行首开始。
pub fn notice_bytes(attempt: u32, delay: Duration) -> Vec<u8> {
    format!(
        "\r\n[Mullion] 连接已断开,第 {attempt} 次重连将在 {} 秒后开始…\r\n",
        delay.as_secs().max(1)
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// F128:**按 host 分组**。adr-009 说一条 SSH 连接承载多个 pane,
    /// 4 块分屏挂在同一台机器上时,链路一死是 4 块一起报 `Reconnecting` ——
    /// 每块各拨一次就是 4 条连接、4 次认证,对高延迟代理链路是纯浪费,
    /// 而且远端会看到 4 次登录。
    ///
    /// 自证会变红:把 `hosts_to_redial` 里的去重(`seen`)删掉。
    #[test]
    fn all_panes_of_one_host_share_a_single_redial() {
        let panes = [
            (PaneId(1), 0usize, PaneStatus::Reconnecting),
            (PaneId(2), 0, PaneStatus::Reconnecting),
            (PaneId(3), 1, PaneStatus::Reconnecting),
            (PaneId(4), 0, PaneStatus::Live),
        ];
        assert_eq!(hosts_to_redial(&panes, &[]), vec![0, 1]);
    }

    /// F128:已经在途的那条不再发起 —— 不然每一帧都会再拨一次
    /// (帧循环 60fps,一秒六十条连接)。
    ///
    /// 自证会变红:把 `hosts_to_redial` 里 `in_flight.contains` 那一句删掉。
    #[test]
    fn a_redial_already_in_flight_is_not_started_again() {
        let panes = [(PaneId(1), 0usize, PaneStatus::Reconnecting)];
        assert!(hosts_to_redial(&panes, &[0]).is_empty());
    }

    /// F128:只有 `Reconnecting` 才拨。`Disconnected`(用户敲了 `exit`)
    /// 去拨的话,用户永远退不出登录。
    #[test]
    fn disconnected_panes_are_never_redialed() {
        let panes = [
            (PaneId(1), 0usize, PaneStatus::Disconnected),
            (PaneId(2), 1, PaneStatus::Live),
        ];
        assert!(hosts_to_redial(&panes, &[]).is_empty());
    }

    /// F128:退避表直接用隧道那套(`mullion_ssh::tunnel::backoff_delay`),
    /// 不另写一份 —— 两套退避策略会在同一条链路抖动时互相打架,而且用户
    /// 看到的"多久重试一次"会因为断的是隧道还是终端而不同。
    ///
    /// 自证会变红:把 `delay_for` 改成返回一个常量 `Duration`。
    #[test]
    fn backoff_is_the_same_table_as_tunnels() {
        for attempt in 0..8 {
            assert_eq!(
                delay_for(attempt),
                mullion_ssh::tunnel::backoff_delay(attempt),
                "第 {attempt} 次的退避不一致"
            );
        }
    }

    /// F128:退避到顶(`backoff_delay` 返回 `None`)= 放弃,pane 落到
    /// `Disconnected`,由用户自己决定重连还是关掉。一直重试到天荒地老的话,
    /// 一台已经拆机的服务器会让客户端永远有一个后台任务在跑。
    ///
    /// `backoff_delay` 的 `attempt` 从 1 开始计数(`attempt == 0` 本身就是
    /// 非法输入,直接返回 `None`,见 `tunnel.rs` 文档),所以这里从 1 开始找
    /// 「到顶」的那一次,不能从 0 开始 —— 从 0 开始会立刻把 0 当成「到顶」,
    /// 跟下面「第 1 次该是 Reconnecting」的断言自相矛盾。
    #[test]
    fn giving_up_turns_into_a_plain_disconnect() {
        let last = (1..).find(|a| delay_for(*a).is_none()).expect("总会有上限");
        assert!(delay_for(last).is_none());
        assert_eq!(status_after_failure(last), PaneStatus::Disconnected);
        assert_eq!(status_after_failure(1), PaneStatus::Reconnecting);
    }

    /// F128:屏内提示是**一行**,喂进 emulator 当普通输出。做倒计时的话要在
    /// 帧循环里再引一个 deadline,正是 spec §1 修订一要避免的东西
    /// (同 `automation_status` 不做定时淡出)。
    #[test]
    fn the_in_screen_notice_is_one_line_of_plain_output() {
        let s = String::from_utf8(notice_bytes(2, std::time::Duration::from_secs(4))).unwrap();
        assert!(s.starts_with("\r\n"), "另起一行,不覆盖远端最后那行输出");
        assert!(s.ends_with("\r\n"));
        assert_eq!(s.matches('\n').count(), 2, "只有一行正文");
        assert!(s.contains("第 2 次"), "实际:{s:?}");
        assert!(s.contains("4"), "要告诉用户等多久,实际:{s:?}");
    }
}
