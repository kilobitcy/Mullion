//! 隧道运行时(F111/F114/F115):谁在跑、跑成什么样,以及状态栏那一格该写什么。
//!
//! **不碰网络** —— 真正的转发在 `mullion_ssh::tunnel`。这里只持有句柄、
//! 记住每条隧道最后一次上报的状态,并把「N 条里有几条在跑、最坏的那条什么样」
//! 算成纯函数(设计 D13)。

use std::collections::BTreeMap;

use mullion_ssh::tunnel::{TunnelHandle, TunnelState};
use mullion_store::{SessionId, TunnelId, TunnelRecord};

/// 一条已经起来(或起过)的隧道。
///
/// `handle` 一旦 Drop 隧道就停(`TunnelHandle` 的 `Drop` 会发停止信号),
/// 所以「从表里删掉」和「停止」是同一件事 —— 不会出现「表里没了但端口还占着」。
struct Live {
    handle: TunnelHandle,
    state: TunnelState,
}

/// 当前所有在跑/跑挂了的隧道。
#[derive(Default)]
pub struct TunnelRuntime {
    live: BTreeMap<TunnelId, Live>,
}

impl TunnelRuntime {
    /// 登记一条刚起来的隧道。同 id 已存在时**先停旧的**再换 —— 否则旧那条
    /// 的 listener 还占着端口,新的根本 bind 不上。
    pub fn insert(&mut self, id: TunnelId, handle: TunnelHandle) {
        self.live.insert(
            id,
            Live {
                handle,
                state: TunnelState::Connecting,
            },
        );
    }

    /// 收到一次状态上报。返回**上一个**状态,供调用方判断跃迁(失败 toast
    /// 靠它只弹一次)。
    ///
    /// id 不在表里 = 用户已经停止或删除了这条隧道,而这是一条在途的旧消息,
    /// 直接丢弃。隧道不属于 `Workspace`,用不上 `PaneOpened` 那套世代号;
    /// 「还在不在表里」本身就是这里唯一需要的判据。
    pub fn set_state(&mut self, id: TunnelId, state: TunnelState) -> Option<TunnelState> {
        let live = self.live.get_mut(&id)?;
        Some(std::mem::replace(&mut live.state, state))
    }

    pub fn state(&self, id: TunnelId) -> Option<&TunnelState> {
        self.live.get(&id).map(|l| &l.state)
    }

    /// 停止并从表里摘掉。摘掉即释放 `TunnelHandle`,端口随之放开。
    pub fn stop(&mut self, id: TunnelId) {
        if let Some(live) = self.live.remove(&id) {
            live.handle.stop();
        }
    }

    /// 每帧交给 UI 的快照。
    pub fn snapshot(&self) -> Vec<(TunnelId, TunnelState)> {
        self.live
            .iter()
            .map(|(id, l)| (*id, l.state.clone()))
            .collect()
    }

    fn states(&self) -> Vec<TunnelState> {
        self.live.values().map(|l| l.state.clone()).collect()
    }

    /// 状态栏那一格(F115)。
    pub fn indicator(&self, configured: usize) -> Option<Indicator> {
        indicator(&self.states(), configured)
    }
}

/// 删掉某条会话时要顺手停掉的隧道。
///
/// **这是安全属性,不是体验属性**(设计 D3):一条指向已删除会话的本机端口
/// 继续 listen,意味着用户以为已经关掉的通路还开着 —— 而且他再也没法从
/// 界面上找到它并关掉,因为那条会话没了。
/// `running` 用 `TunnelRuntime::snapshot()` 的形状而不是 `&TunnelRuntime`:
/// 后者只能靠真起一条隧道(要 tokio runtime、要 listener、要 `SshConfig`)才
/// 填得出来,而这是条**安全属性**,必须有一个能随手写死输入的测试钉着。
pub fn tunnels_to_stop_on_session_delete(
    session: SessionId,
    tunnels: &[TunnelRecord],
    running: &[(TunnelId, TunnelState)],
) -> Vec<TunnelId> {
    mullion_store::tunnel::tunnels_referencing(session, tunnels)
        .iter()
        .map(|t| t.id)
        .filter(|id| running.iter().any(|(rid, _)| rid == id))
        .collect()
}

/// F338:把本实例的运行表转成要对外发布的形状。`Stopped` 不发布 —— 它表示
/// 已经不占端口。按 id 升序,保证「没变就不重写」的比较稳定。
pub fn publishable(
    snapshot: &[(TunnelId, TunnelState)],
) -> Vec<(TunnelId, mullion_store::RemoteTunnelState)> {
    use mullion_store::RemoteTunnelState as R;
    let mut out: Vec<_> = snapshot
        .iter()
        .filter_map(|(id, s)| {
            let r = match s {
                TunnelState::Connecting => R::Connecting,
                TunnelState::Running => R::Running,
                TunnelState::Reconnecting { .. } => R::Reconnecting,
                TunnelState::Failed(_) => R::Failed,
                TunnelState::Stopped => return None,
            };
            Some((*id, r))
        })
        .collect();
    out.sort_by_key(|(id, _)| *id);
    out
}

/// F338:读别的实例隧道状态的间隔。隧道页看得见时 2 秒(用户正盯着它),
/// 否则跟心跳 15 秒。**不是每帧读** —— 每帧 `read_dir` 是 T3 违规。
pub fn others_poll_interval(page_visible: bool) -> std::time::Duration {
    if page_visible {
        std::time::Duration::from_secs(2)
    } else {
        std::time::Duration::from_secs(mullion_store::HEARTBEAT_INTERVAL_SECS)
    }
}

/// F338:下一次该读的时刻。只在隧道页看得见时才主动唤醒事件循环
/// (其余时候搭心跳的车,不新增忙转的唤醒路径,T3/T7)。
pub fn others_wake(
    page_visible: bool,
    last_read: std::time::Instant,
) -> Option<std::time::Instant> {
    page_visible.then(|| last_read + others_poll_interval(true))
}

/// 这次状态变化该不该播报。返回 `Some(根因)` = 播一条。
///
/// 判据是**跃迁**,不是当前状态:健康探测 2 秒一次,每次都会走一遍状态上报,
/// 写成「当前是 `Failed` 就播」等于每 2 秒弹一条 toast,界面直接没法用。
pub fn failure_announcement(prev: &TunnelState, next: &TunnelState) -> Option<String> {
    match (prev, next) {
        (TunnelState::Failed(_), _) => None,
        (_, TunnelState::Failed(msg)) => Some(msg.clone()),
        _ => None,
    }
}

/// 状态栏隧道指示器的三档。按**最坏**状态取,不按第一条。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// 全部正常。
    Calm,
    /// 有在重连/在建链的。
    Warn,
    /// 有停下等人工的。
    Danger,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Indicator {
    pub text: String,
    pub severity: Severity,
}

/// `states` 是**已启动**的那些隧道的状态,`configured` 是库里配了多少条。
///
/// 一条都没启动时返回 `None` —— 状态栏不该为「你配了但没开」的东西常驻占一格。
pub fn indicator(states: &[TunnelState], configured: usize) -> Option<Indicator> {
    if configured == 0 || states.is_empty() {
        return None;
    }
    let running = states
        .iter()
        .filter(|s| matches!(s, TunnelState::Running))
        .count();
    // 取最坏:一条挂了、九条好着,用户需要知道的是那一条。
    // 写成「取第一条」的话,同一组状态换个顺序会给出不同答案。
    let severity = if states
        .iter()
        .any(|s| matches!(s, TunnelState::Failed(_) | TunnelState::Stopped))
    {
        Severity::Danger
    } else if states.iter().any(|s| {
        matches!(
            s,
            TunnelState::Reconnecting { .. } | TunnelState::Connecting
        )
    }) {
        Severity::Warn
    } else {
        Severity::Calm
    };
    // F143:原先那两个符号(U+21BB / U+2717)都不在 GBK,是豆腐块。这里是
    // **纯字符串**(进 `Indicator::text`,由状态栏当普通文本画),没有自绘的
    // 余地 —— 只能换成白名单里的字符。`…` 表「还在路上」,`×` 表「断了」。
    let mark = match severity {
        Severity::Calm => "",
        Severity::Warn => "…",
        Severity::Danger => " ×",
    };
    Some(Indicator {
        text: format!("隧道 {running}/{configured}{mark}"),
        severity,
    })
}

/// F339:标题条上「这块 pane 的会话在远端开着哪些端口」的一格。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePort {
    pub port: u16,
    pub hover: String,
}

/// 属于 `session` 的、**正在运行**的远程转发(-R),按远端端口升序。
/// 本窗口的状态优先于别的窗口的上报(同 F338 列表行);同一条隧道只出一次。
pub fn pane_remote_ports(
    session: SessionId,
    records: &[TunnelRecord],
    local: &[(TunnelId, TunnelState)],
    others: &[mullion_store::OtherTunnel],
) -> Vec<RemotePort> {
    use mullion_store::{RemoteTunnelState, TunnelKind};
    let mut out: Vec<RemotePort> = Vec::new();
    for r in records.iter().filter(|r| r.session_id == session) {
        let TunnelKind::Remote {
            target_host,
            target_port,
            ..
        } = &r.kind
        else {
            continue;
        };
        let owner = if let Some((_, st)) = local.iter().find(|(id, _)| *id == r.id) {
            // 本窗口有这条:以本窗口为准,别的窗口的上报不再参与。
            if !matches!(st, TunnelState::Running) {
                continue;
            }
            "本窗口".to_string()
        } else if let Some(o) = others
            .iter()
            .find(|o| o.tunnel == r.id && o.state == RemoteTunnelState::Running)
        {
            format!("另一窗口 PID {}", o.pid)
        } else {
            continue;
        };
        let name = if r.name.is_empty() {
            format!("远程 {}", r.listen_port)
        } else {
            r.name.clone()
        };
        out.push(RemotePort {
            port: r.listen_port,
            hover: format!(
                "{name}:远端 {} → {target_host}:{target_port}({owner})",
                r.listen_port
            ),
        });
    }
    out.sort_by_key(|p| p.port);
    out
}

/// 标题条上那一格的文字与悬停。没有端口就是 `None`(不占位)。
/// 多个折成 `:9222 +1`,悬停列全部。
pub fn port_badge(ports: &[RemotePort]) -> Option<(String, String)> {
    let first = ports.first()?;
    let text = match ports.len() {
        1 => format!("CDP:{}", first.port),
        n => format!("CDP:{} +{}", first.port, n - 1),
    };
    let hover = ports
        .iter()
        .map(|p| p.hover.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    Some((text, hover))
}

#[cfg(test)]
mod tests {

    /// F339:只认本会话、-R、Running;本窗口优先;别窗口的 Running 也算。
    #[test]
    fn pane_ports_collect_running_remote_forwards_of_this_session() {
        use mullion_store::{OtherTunnel, RemoteTunnelState as R, TunnelKind};
        let remote = |id: u64, sess: u64, port: u16| TunnelRecord {
            id: TunnelId(id),
            session_id: SessionId(sess),
            listen_port: port,
            name: String::new(),
            note: String::new(),
            autostart: false,
            kind: TunnelKind::Remote {
                target_host: "127.0.0.1".into(),
                target_port: 9222,
                expose: false,
            },
        };
        let mut local_fwd = remote(5, 1, 7000);
        local_fwd.kind = TunnelKind::Dynamic;
        let recs = vec![
            remote(1, 1, 9322),
            remote(2, 1, 9222),
            remote(3, 2, 9111), // 别的会话
            remote(4, 1, 9400), // 本窗口里是 Connecting
            local_fwd,          // 非 -R
            remote(6, 1, 9500), // 只有别的窗口在跑
            remote(7, 1, 9600), // 别的窗口里是 Failed
        ];
        let local = vec![
            (TunnelId(1), TunnelState::Running),
            (TunnelId(2), TunnelState::Running),
            (TunnelId(3), TunnelState::Running),
            (TunnelId(4), TunnelState::Connecting),
            (TunnelId(5), TunnelState::Running),
        ];
        let other = |id: u64, state| OtherTunnel {
            tunnel: TunnelId(id),
            state,
            pid: 42,
            started_at: 0,
            instance: "x".into(),
        };
        let others = vec![
            other(4, R::Running), // 本窗口有记录,不能被别窗口翻盘
            other(6, R::Running),
            other(7, R::Failed),
        ];
        let got = pane_remote_ports(SessionId(1), &recs, &local, &others);
        let ports: Vec<u16> = got.iter().map(|p| p.port).collect();
        assert_eq!(ports, vec![9222, 9322, 9500]);
        assert!(got[2].hover.contains("另一窗口 PID 42"));
        assert!(got[0].hover.contains("本窗口"));
    }

    /// F339:徽标文字——无/单个/多个。
    #[test]
    fn port_badge_folds_extra_ports() {
        let p = |port: u16| RemotePort {
            port,
            hover: format!("h{port}"),
        };
        assert_eq!(port_badge(&[]), None);
        assert_eq!(port_badge(&[p(9222)]).unwrap().0, "CDP:9222");
        let (t, h) = port_badge(&[p(9222), p(9322)]).unwrap();
        assert_eq!(t, "CDP:9222 +1");
        assert_eq!(h, "h9222\nh9322");
    }

    /// F338:已停止的不发布;其余四态各映射一档;顺序稳定。
    #[test]
    fn publishable_skips_stopped_and_orders_by_id() {
        use mullion_store::RemoteTunnelState as R;
        let snap = vec![
            (TunnelId(9), TunnelState::Running),
            (TunnelId(2), TunnelState::Stopped),
            (
                TunnelId(4),
                TunnelState::Reconnecting {
                    attempt: 1,
                    retry_in: std::time::Duration::from_secs(1),
                },
            ),
            (TunnelId(1), TunnelState::Failed("x".into())),
            (TunnelId(3), TunnelState::Connecting),
        ];
        assert_eq!(
            publishable(&snap),
            vec![
                (TunnelId(1), R::Failed),
                (TunnelId(3), R::Connecting),
                (TunnelId(4), R::Reconnecting),
                (TunnelId(9), R::Running),
            ]
        );
    }

    /// F338:隧道页看得见才 2 秒一读并主动唤醒;看不见时不排唤醒、间隔回到心跳。
    /// 自证会变红:把 `others_wake` 改成恒 `Some`(空闲窗口 2 秒醒一次,T3)。
    #[test]
    fn the_fast_poll_and_its_wakeup_exist_only_while_the_tunnel_page_is_visible() {
        let t0 = std::time::Instant::now();
        assert_eq!(
            others_poll_interval(true),
            std::time::Duration::from_secs(2)
        );
        assert_eq!(
            others_poll_interval(false),
            std::time::Duration::from_secs(mullion_store::HEARTBEAT_INTERVAL_SECS)
        );
        assert_eq!(
            others_wake(true, t0),
            Some(t0 + std::time::Duration::from_secs(2))
        );
        assert_eq!(others_wake(false, t0), None);
    }
    use super::*;
    use std::time::Duration;

    fn reconnecting() -> TunnelState {
        TunnelState::Reconnecting {
            attempt: 2,
            retry_in: Duration::from_secs(2),
        }
    }

    /// D13 的守护。**同一组状态换个顺序必须给同一个答案** —— 写成
    /// 「取第一条」时这条会红,而「取第一条」的实际后果是:两条隧道
    /// 挂了一条,状态栏照样是安静的灰色,用户永远不知道。
    #[test]
    fn indicator_takes_the_worst_state_not_the_first() {
        let a = indicator(&[TunnelState::Running, TunnelState::Failed("x".into())], 2).unwrap();
        let b = indicator(&[TunnelState::Failed("x".into()), TunnelState::Running], 2).unwrap();
        assert_eq!(a, b, "顺序不该影响结论");
        assert_eq!(a.severity, Severity::Danger);

        // 重连中比正常坏、比失败好。
        let w = indicator(&[TunnelState::Running, reconnecting()], 2).unwrap();
        assert_eq!(w.severity, Severity::Warn);
        assert_eq!(
            indicator(&[TunnelState::Running], 1).unwrap().severity,
            Severity::Calm
        );
        // 表里还留着但已经 `Stopped` 的,也算需要注意 —— 正常停止是
        // 「从表里摘掉」,还挂在表里说明是意外停的。
        assert_eq!(
            indicator(&[TunnelState::Running, TunnelState::Stopped], 2)
                .unwrap()
                .severity,
            Severity::Danger
        );
    }

    #[test]
    fn indicator_counts_running_over_total() {
        let i = indicator(&[TunnelState::Running, reconnecting()], 2).unwrap();
        assert!(i.text.contains("1/2"), "实际: {}", i.text);
        // 配了 5 条只起了 2 条时,分母是**配置数**不是已起数 ——
        // 否则「隧道 2/2」会让人以为全都在跑。
        let i = indicator(&[TunnelState::Running, TunnelState::Running], 5).unwrap();
        assert!(i.text.contains("2/5"), "实际: {}", i.text);
    }

    #[test]
    fn indicator_is_none_when_nothing_is_configured() {
        assert!(indicator(&[], 0).is_none());
        assert!(
            indicator(&[], 3).is_none(),
            "配了但一条都没启动时,状态栏不该常驻占一格"
        );
    }

    /// 跌进失败态播一次,之后连报三次都不再播。
    ///
    /// 自证会变红:把 `failure_announcement` 改成「只看 `next` 是不是
    /// `Failed`」,下面那三次就都会返回 `Some`。
    #[test]
    fn entering_failed_state_announces_once_not_every_report() {
        let failed = TunnelState::Failed("端口被占".into());
        assert_eq!(
            failure_announcement(&TunnelState::Running, &failed).as_deref(),
            Some("端口被占"),
            "跌进失败态必须播报,且带上根因"
        );
        for _ in 0..3 {
            assert_eq!(
                failure_announcement(&failed, &failed),
                None,
                "已经在失败态了就别再播 —— 健康探测每 2 秒就会来一次"
            );
        }
        // 换了一条失败原因也不重播:用户已经知道这条隧道挂了,
        // 详细根因在列表行里写着。
        assert_eq!(
            failure_announcement(&failed, &TunnelState::Failed("别的原因".into())),
            None
        );
        assert_eq!(
            failure_announcement(&reconnecting(), &TunnelState::Running),
            None
        );
    }

    /// D3 的守护。删会话必须停掉它名下**正在跑**的隧道,且**只**停这些。
    ///
    /// 这是安全属性:会话一删,那些隧道在界面上就再也找不到了,而本机端口还
    /// listen 着 —— 用户以为关掉的通路仍然开着,且没有任何办法关掉它。
    ///
    /// 自证会变红:把实现里的 `filter` 去掉(会连没启动的一起「停」,返回值
    /// 多出 `TunnelId(2)`),或把 `tunnels_referencing` 换成全表(会多出
    /// 别的会话的 `TunnelId(3)`)。
    #[test]
    fn deleting_a_session_stops_exactly_its_running_tunnels() {
        fn rec(id: u64, session: u64) -> TunnelRecord {
            TunnelRecord {
                id: TunnelId(id),
                session_id: SessionId(session),
                listen_port: 3306,
                name: String::new(),
                note: String::new(),
                autostart: false,
                kind: mullion_store::TunnelKind::Local {
                    target_host: "db".into(),
                    target_port: 3306,
                    expose: false,
                },
            }
        }
        let tunnels = vec![rec(1, 7), rec(2, 7), rec(3, 8)];
        let running = vec![
            (TunnelId(1), TunnelState::Running),
            (TunnelId(3), TunnelState::Running),
        ];
        assert_eq!(
            tunnels_to_stop_on_session_delete(SessionId(7), &tunnels, &running),
            vec![TunnelId(1)],
            "只停这条会话名下、且确实在跑的那些"
        );
        assert!(tunnels_to_stop_on_session_delete(SessionId(9), &tunnels, &running).is_empty());
    }

    #[test]
    fn stale_state_for_a_removed_tunnel_is_dropped() {
        let mut rt = TunnelRuntime::default();
        assert!(
            rt.set_state(TunnelId(1), TunnelState::Running).is_none(),
            "已停止/已删除的隧道,在途状态必须丢弃而不是凭空建一条"
        );
        assert!(rt.state(TunnelId(1)).is_none());
        assert!(rt.indicator(1).is_none());
    }
}
