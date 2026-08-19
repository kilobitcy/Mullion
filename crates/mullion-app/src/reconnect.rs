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

/// 一次重连成功之后,这条 host 上**没换到新 channel** 的那些 pane 里,哪些要
/// 补一次重拨(置回 `Reconnecting`,下一帧由 `hosts_to_redial` 收走)。
///
/// 为什么会有"漏网的":`spawn_reconnect` 是按**发起那一刻**已经翻成
/// `Reconnecting` 的 pane 去开 channel 的。而 adr-009 下一条连接承载多块分屏,
/// 各 pane 的 `rx` 不保证同一帧关闭(reader task 各自独立,缓冲里积压的字节还要
/// 先排空)。于是 A 先翻、带着 A 去拨号,B 慢了几帧才翻 —— 拨回来的 channel 里
/// 没有 B 的份,B 手里攥的还是那条已经死掉的旧 channel:输入静默丢失(按项目
/// 惯例 `let _ = pty.write(..)`),而标题条上它看起来一切正常。
///
/// **只捞 `Live`**:`Reconnecting` 的下一帧本来就会被 `hosts_to_redial` 收走,
/// 不用在这里动;`Disconnected` 一律不碰 —— 那是用户敲 `exit` 或者重试到顶
/// 放弃的终态,把它拉回来等于替用户决定"你还想连着"。
///
/// 判据成立的前提:重连只由 `rx_closed_action(transport_alive == false)` 触发,
/// 也就是**整条传输层**死了。传输层死了,这条 host 上的每一条 channel 都死了,
/// 所以"没换到新 channel 的 `Live` pane"必然是攥着死 channel 的。
pub fn strays_after_reconnect(
    panes: &[(PaneId, usize, PaneStatus)],
    host_ix: usize,
    attached: &[PaneId],
) -> Vec<PaneId> {
    panes
        .iter()
        .filter(|(id, ix, status)| {
            *ix == host_ix && *status == PaneStatus::Live && !attached.contains(id)
        })
        .map(|(id, _, _)| *id)
        .collect()
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

/// 退避到顶、放弃重连时喂进 emulator 的那一行提示。同 `notice_bytes`:纯文本
/// + 前后各一个 `\r\n`,不做倒计时。
///
/// **必须是普通字符串字面量**,不是手工 hex 转义的 `b"..."`——那种写法全文件
/// 只有一处,内容靠旁边注释反推,字节和注释谁也不校验谁;日后改文案的人
/// 很可能只改了注释、忘了转 hex,现象是「注释说一套、屏幕显示另一套」。
pub fn give_up_notice_bytes() -> Vec<u8> {
    "\r\n[Mullion] 重连失败次数过多,已停止重试。\r\n"
        .to_owned()
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

    /// F128 竞态:重连拨回来的 channel 里没份的那些 pane 要补一次重拨。
    ///
    /// adr-009 下一条连接承载多块分屏,各 pane 的 `rx` 不保证同一帧关闭 ——
    /// A 先翻 `Reconnecting`、带着 A 去拨号,B 慢几帧才翻,拨回来的 channel 里
    /// 就没有 B 的份。不补的话 B 攥着一条死 channel:输入静默丢失(项目惯例是
    /// `let _ = pty.write(..)`),而标题条上它看起来一切正常。
    ///
    /// 自证会变红:把 `strays_after_reconnect` 的函数体改成 `Vec::new()`。
    #[test]
    fn a_pane_that_missed_the_redial_gets_picked_up_again() {
        let panes = [
            (PaneId(1), 0usize, PaneStatus::Live), // 换到新 channel 上了
            (PaneId(2), 0, PaneStatus::Live),      // 慢了一步,没赶上这次拨号
            (PaneId(3), 1, PaneStatus::Live),      // 另一台机器,不关这次的事
        ];
        assert_eq!(
            strays_after_reconnect(&panes, 0, &[PaneId(1)]),
            vec![PaneId(2)]
        );
    }

    /// F128:`Disconnected` 一律不碰 —— 那是用户敲了 `exit`(或者重试到顶
    /// 放弃)的终态。捞回来等于替用户决定「你还想连着」,他就永远退不出登录,
    /// 正是 `rx_closed_action` 那条红线要防的现象。
    ///
    /// `Reconnecting` 也不用捞:下一帧 `hosts_to_redial` 本来就会收走它,
    /// 在这里重复置位只是无用功。
    ///
    /// 自证会变红:把 `strays_after_reconnect` 里的 `== PaneStatus::Live`
    /// 换成 `!= PaneStatus::Disconnected`(会多捞 `Reconnecting`),
    /// 或者换成 `true`(连用户 exit 的那块也捞)。
    #[test]
    fn a_pane_the_user_exited_is_never_dragged_back_into_reconnecting() {
        let panes = [
            (PaneId(1), 0usize, PaneStatus::Disconnected),
            (PaneId(2), 0, PaneStatus::Reconnecting),
        ];
        assert!(
            strays_after_reconnect(&panes, 0, &[]).is_empty(),
            "只该捞 Live:Disconnected 是用户的决定,Reconnecting 下一帧自会被收走"
        );
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

    /// F128:退避到顶、放弃重连时的屏内提示,同样是**一行**纯文本输出
    /// (同 `notice_bytes`)。
    #[test]
    fn the_give_up_notice_is_one_line_of_plain_output() {
        let s = String::from_utf8(give_up_notice_bytes()).unwrap();
        assert!(s.starts_with("\r\n"), "另起一行,不覆盖远端最后那行输出");
        assert!(s.ends_with("\r\n"));
        assert_eq!(s.matches('\n').count(), 2, "只有一行正文");
        assert!(
            s.contains("重连失败"),
            "要让用户知道已经放弃重试,实际:{s:?}"
        );
    }
}
