//! F40~F44 登录后自动化的 **app 侧**状态机。
//!
//! 分两部分，边界是刻意的：
//! - **纯决策**（`pending_for` / `take_pending` / `status_text` /
//!   `split_pasted_commands`）：裸单测，无 tokio、无 egui、无 store。
//! - **async 运行**（`run`）：`#[tokio::test(start_paused = true)]` 假时钟 +
//!   假 sink，零网络。
//!
//! 超时与延时**全部**在这里的 `select!` 里闭环，**绝不进 `ControlFlow`**。
//! 理由见设计 spec §1 修订一：T7（首次节流后永久 100% CPU 忙转）的现场就是
//! 事件循环那三个分支的 `control_flow` 复位，为一个与渲染无关的业务 deadline
//! 去改那里，是在踩过雷的地方重新布雷，而收益为零。

/// 自动化正在跑时状态栏显示的文案。不是 `Outcome`——它描述的是「还没结束」。
pub const RUNNING_TEXT: &str = "自动化:进行中";

/// 一次自动化的最终结局。回送给 UI **只为了给用户一句话**，不驱动任何后续动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// 全部步骤都发完了。带步数：用户看不见发生了什么，就无法判断自动化是
    /// 没跑还是跑了没效果。
    Completed(usize),
    /// 用户接管（敲了键/滚了轮/粘贴了）。
    Aborted,
    /// 等首字节超时。带毫秒数：文案要说清等了多久，否则用户不知道该调哪个值。
    SkippedTimeout { after_ms: u32 },
    /// 链路断了。
    Disconnected,
    /// 出站队列持续满。
    Congested,
}

/// 结局 → 状态栏文案。纯函数。
pub fn status_text(o: Outcome) -> String {
    match o {
        Outcome::Completed(n) => format!("自动化已完成({n} 步)"),
        Outcome::Aborted => "自动化已中止:检测到你的输入".to_string(),
        // 向上取整而不是截断:配了 500ms 超时的用户不该看到「0s 未收到输出」。
        Outcome::SkippedTimeout { after_ms } => {
            format!("自动化已跳过:{}s 未收到远端输出", after_ms.div_ceil(1000))
        }
        Outcome::Disconnected => "自动化已中止:连接已断开".to_string(),
        Outcome::Congested => "自动化已中止:出站队列拥塞".to_string(),
    }
}

/// 状态栏这一帧该显示哪一句。`running` = 当前还挂着 handle。
///
/// 抽出来是因为「跑着的时候盖住上一次的结论」这条规则很容易在 UI 代码里
/// 写反（先画 last、再判 running），而写反的现象是「新连接的状态栏还挂着
/// 上一条连接的结论」——一条与当前连接毫不相干的误导信息。
pub fn status_line(running: bool, last: Option<&str>) -> Option<&str> {
    if running {
        Some(RUNNING_TEXT)
    } else {
        last
    }
}

use mullion_store::{build_plan, ResolvedAutomation, SessionId, Step};

/// 已经算好、等着 `ConnectOk` 抵达时启用的计划。
///
/// **在 `spawn_connect`（用户点「连接」的那一帧）算，不在 `ConnectOk` 里算。**
/// 两个理由：其一，`UserEvent::ConnectOk` 不携带 `SessionId`，到那时只能读
/// `ui.connect_request_last`，而连接在途期间用户完全可能改了配置甚至删了这条
/// 会话——发出去的字节就跟他当初点「连接」时看到的配置对不上了。其二，
/// `store.resolved()` 是同步查库，放在用户点击的那一帧语义清楚。
pub struct PendingAutomation {
    pub steps: Vec<Step>,
    pub ready_timeout_ms: u32,
}

/// 算这条会话的待办计划。`None` = 不跑自动化。
///
/// `lookup` 由调用方注入（返回「解析后的自动化配置」+「会话名」），而不是在
/// 这里直接够 `SessionStore`——注入之后本函数可以脱离会话库单测，
/// 「没有 SessionId 就绝不查库」这条性质才验得动（见
/// `cli_direct_connect_has_no_plan`）。同样的注入手法在
/// `session_manager::editor::ensure_key_candidates_scanned` 已有先例。
///
/// `id` 为 `None` 即 CLI 直连（`mullion user@host -p 22 -i key`）：它压根没有
/// 会话记录，没有配置来源，**凭空猜一个 tmux 会话名去 attach 是最坏的行为**。
/// 这不是退化：CLI 直连本来就是调试入口，要自动化就存成会话。
pub fn pending_for(
    id: Option<SessionId>,
    lookup: impl FnOnce(SessionId) -> Option<(ResolvedAutomation, String)>,
) -> Option<PendingAutomation> {
    let id = id?;
    let (resolved, name) = lookup(id)?;
    let steps = build_plan(&resolved, &name);
    if steps.is_empty() {
        return None;
    }
    Some(PendingAutomation {
        steps,
        ready_timeout_ms: resolved.ready_timeout_ms,
    })
}

/// **不是**这条连接第一个 pane 的那些 pane（分屏新开的、换过节点的）该跑什么。
/// `None` = 无事可做，连 oneshot 都不建。
///
/// 与 `pending_for` 的差别只有一处：**跳过 tmux**（见
/// `build_plan_without_tmux`）。用户拍板的规则是「只有建标签的那个 pane 全套
/// 跑，其余跳过 tmux、其余照跑」——两块 pane attach 同一个 session 会内容镜像，
/// 而 `cd` / `export` / 启动命令对每块新 shell 都仍然成立。
///
/// 收 `&ResolvedAutomation` 而不是 `SessionId`+lookup：模板是 `ConnectOk` 那一刻
/// 就随标签存下来的（`TerminalTab::automation_template`），此后分屏多少次都用
/// 同一份。**刻意不回头再查库**——与 `pending_for` 同一个理由：连上之后用户
/// 完全可能改了配置甚至删了会话，那时候分屏发出去的字节就跟他当初连接时看到的
/// 配置对不上了。
pub fn pending_for_extra_pane(tpl: &ResolvedAutomation) -> Option<PendingAutomation> {
    let steps = mullion_store::build_plan_without_tmux(tpl);
    if steps.is_empty() {
        return None;
    }
    Some(PendingAutomation {
        steps,
        ready_timeout_ms: tpl.ready_timeout_ms,
    })
}

/// 这块 pane 的连接状态是否意味着「在跑的那份自动化该当场取消」。
///
/// 判据是**这条 channel 还能不能把字节送出去**，不是「pane 会不会回来」。
/// `Reconnecting` 也要取消：F128 之前链路一死就直接是 `Disconnected`，
/// 「死了就立刻取消」这条语义靠 `== Disconnected` 表达是对的；F128 插进
/// `Reconnecting` 这个中间态之后，同一条判据就漏了 —— 等 ready 的那份自动化
/// 得一直挂到自己的 `ready_timeout_ms` 才收场，而它等的那条 channel 早就没了。
/// 重连成功后 `PaneReconnected` 会另起一份（跳过 tmux 那套），旧的这份留着
/// 只会往一条死 channel 上写。
///
/// 抽成纯函数而不是在 `drive_automation` 里写 `matches!`：判据本身能脱离
/// `App`/事件循环单测，而 `drive_automation` 不能（同 F129 的 Ctrl+D 判据）。
pub fn should_cancel_on_status(status: crate::shell::workspace::PaneStatus) -> bool {
    use crate::shell::workspace::PaneStatus;
    match status {
        PaneStatus::Live => false,
        // 穷尽列出而不是 `_ => true`：以后再加状态（比如「挂起」）时，
        // 编译器会逼着人回来想一遍该不该取消，而不是静默归到"取消"这一档。
        PaneStatus::Reconnecting | PaneStatus::Disconnected => true,
    }
}

/// `ConnectOk` 抵达时：取走待办计划与一次性跳过标志，回答「这次到底起不起」。
///
/// 两个 `&mut` 都是**取走**语义，这正是本函数存在的理由：计划和跳过标志都必须
/// 恰好生效一次。分成「判断」和「消费」两个函数的话，消费点会散进 `app.rs`，
/// 而 `App` 在无头环境构造不出来——「跳过只生效一次」这条性质就再也测不动了。
pub fn take_pending(
    pending: &mut Option<PendingAutomation>,
    skip: &mut bool,
) -> Option<PendingAutomation> {
    let plan = pending.take();
    if std::mem::take(skip) {
        return None;
    }
    plan
}

/// F41:把一段可能含换行的文本拆成若干条命令。
///
/// 空行丢弃、逐条 `trim`。`\r` 与 `\n` 都算行分隔——裸 `\r` 在「整个计划只有
/// 一行」的模型里就是一次额外回车（正是设计 §2 要挡的东西），漏掉它这条命令
/// 会被数据层静默丢弃，用户配了个会凭空消失的命令。
pub fn split_pasted_commands(raw: &str) -> Vec<String> {
    raw.split(['\n', '\r'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// F41：一行输入框里冒出换行之后，把它摊成「这一行的新内容 + 要插在它下面的若干行」。
///
/// 与 `split_pasted_commands` 的差别只在**两端的空段要保留**：粘贴一整段时，
/// 空行是噪音；用户手敲回车时，那个空段恰恰就是他要的新空行。只丢中间的空行、
/// 两端各补回来，「敲回车 = 新增一条空行」（spec §5）才在**空框里敲**和**行首敲**
/// 两种情形下都真的有反应——否则空段被整体过滤，按键看上去毫无效果。
pub fn split_edited_line(raw: &str) -> (String, Vec<String>) {
    let mut parts = split_pasted_commands(raw);
    if raw.starts_with(['\n', '\r']) {
        parts.insert(0, String::new());
    }
    if raw.ends_with(['\n', '\r']) {
        parts.push(String::new());
    }
    if parts.is_empty() {
        return (String::new(), Vec::new());
    }
    let head = parts.remove(0);
    (head, parts)
}

use std::sync::Arc;
use std::time::Duration;

use mullion_ssh::schedule::{write_scheduled, ByteSink, ScheduleOutcome};
use tokio::sync::oneshot;

/// 跑一次自动化。**整条链路在这里闭环，`ControlFlow` 一行都不动**（spec §1 修订一）。
///
/// 四条边，三条是一次性通道、一条是自带的超时：
/// - `ready`：winit 线程发现该 pane 收到了第一个入站字节。
/// - `cancel`：用户接管（敲键 / 滚轮上报 / 粘贴）。**必须只由 app.rs 里的用户
///   意图写入点触发**，见 `App::user_took_over` 的文档。
/// - `disconnect`：pane 被 `Workspace::pump` 标成 `Disconnected`。独占一条通道
///   而不是复用 `cancel`：复用的话结局会变成 `Aborted`，状态栏显示「检测到你的
///   输入」——用户根本没输入。
/// - `ready_timeout_ms`：从本 task 被 spawn 起算。
///
/// 已知偏差：设计要求「从 `open_pty` 返回起算」，实际起点是 task 被 spawn 的
/// 时刻，晚一次 winit 事件投递（毫秒级）。远小于 15s 默认值，不做补偿。
pub async fn run(
    sink: Arc<dyn ByteSink>,
    steps: Vec<Step>,
    ready: oneshot::Receiver<()>,
    mut cancel: oneshot::Receiver<()>,
    mut disconnect: oneshot::Receiver<()>,
    ready_timeout_ms: u32,
) -> Outcome {
    let steps_len = steps.len();
    let timeout = Duration::from_millis(u64::from(ready_timeout_ms));

    // 第一段：等首字节。`biased` = 取消/断线优先，同时就绪时不会「刚被取消却
    // 又起了计划」。
    tokio::select! {
        biased;
        _ = &mut cancel => return Outcome::Aborted,
        _ = &mut disconnect => return Outcome::Disconnected,
        r = ready => {
            // Err = 发送端被 drop（handle 没了 / App 退出）。理论上 `cancel`
            // 会先于它就绪（同时 drop + biased），这里是防御性兜底。
            if r.is_err() {
                return Outcome::Aborted;
            }
        }
        _ = tokio::time::sleep(timeout) => {
            return Outcome::SkippedTimeout { after_ms: ready_timeout_ms };
        }
    }

    // 第二段：按时间表发。**不在这里再 sleep 一次 `initial_delay_ms`** ——
    // `build_plan` 生成的第一个 Step 的 delay 就是它，多等一次就是延时翻倍。
    let plan: Vec<(Duration, Vec<u8>)> = steps.into_iter().map(|s| (s.delay, s.bytes)).collect();
    let sched = write_scheduled(sink, plan, cancel);
    tokio::pin!(sched);
    tokio::select! {
        biased;
        _ = &mut disconnect => Outcome::Disconnected,
        out = &mut sched => match out {
            ScheduleOutcome::Completed => Outcome::Completed(steps_len),
            ScheduleOutcome::Cancelled => Outcome::Aborted,
            ScheduleOutcome::Disconnected => Outcome::Disconnected,
            ScheduleOutcome::Congested => Outcome::Congested,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 注意路径:四个 `DEFAULT_*` 常量**没有**在 `mullion_store` 顶层 re-export
    // （lib.rs:17-19 只导出了 build_plan / AutomationCommand / AutomationPrefs /
    // EnvVar / ResolvedAutomation / Step / TmuxChoice），必须走 `automation::`。
    use mullion_store::automation::{
        DEFAULT_INITIAL_DELAY_MS, DEFAULT_INTER_DELAY_MS, DEFAULT_READY_TIMEOUT_MS,
    };
    use mullion_store::{AutomationCommand, ResolvedAutomation, SessionId, TmuxChoice};

    /// F128 回归:链路刚死、还在退避重连的那段时间里,在跑的自动化必须**当场**
    /// 取消。判据写成 `== Disconnected` 的话(F128 之前唯一的"死法"),
    /// `Reconnecting` 这个中间态会漏掉——等首字节的那份自动化会一直挂到自己的
    /// `ready_timeout_ms`(默认几十秒)才收场,而它等的那条 channel 早没了;
    /// 期间重连成功了的话,`PaneReconnected` 起的新一份和这份旧的会同时往
    /// 同一块 pane 上写。
    ///
    /// 自证会变红:把 `should_cancel_on_status` 里 `Reconnecting` 那一档挪回
    /// `false`(也就是回到 F128 引入中间态之前的判据)。
    #[test]
    fn a_reconnecting_pane_cancels_its_automation_just_like_a_dead_one() {
        use crate::shell::workspace::PaneStatus;
        assert!(
            should_cancel_on_status(PaneStatus::Reconnecting),
            "链路死了还在重连,这份自动化等的那条 channel 已经没了"
        );
        assert!(should_cancel_on_status(PaneStatus::Disconnected));
        assert!(
            !should_cancel_on_status(PaneStatus::Live),
            "活着的 pane 绝不能取消 —— 那会让登录后命令永远跑不起来"
        );
    }

    /// 造一份「开着、有一条命令」的解析结果。
    fn enabled_automation() -> ResolvedAutomation {
        ResolvedAutomation {
            enabled: true,
            tmux: None,
            commands: vec![AutomationCommand {
                text: "echo hi".into(),
                delay_ms: None,
            }],
            work_dir: None,
            env: Vec::new(),
            initial_delay_ms: DEFAULT_INITIAL_DELAY_MS,
            inter_delay_ms: DEFAULT_INTER_DELAY_MS,
            ready_timeout_ms: DEFAULT_READY_TIMEOUT_MS,
        }
    }

    /// F44 关掉 ⟹ `build_plan` 给空计划 ⟹ 连 oneshot 都不建、task 也不 spawn。
    ///
    /// 自证会变红:把 `pending_for` 里 `if steps.is_empty() { return None; }` 删掉。
    #[test]
    fn disabled_automation_does_not_start() {
        let mut a = enabled_automation();
        a.enabled = false;
        let p = pending_for(Some(SessionId(7)), |_| Some((a.clone(), "web01".into())));
        assert!(p.is_none(), "F44 关闭时不该产出任何待办计划");

        // 反面:开着就该有计划,否则上一条断言是恒真的。
        let on = pending_for(Some(SessionId(7)), |_| {
            Some((enabled_automation(), "web01".into()))
        });
        assert!(on.is_some());
    }

    /// 分屏新开的 pane 跑命令,但**绝不碰 tmux** —— 这是用户拍板的规则,
    /// 也是 `AutomationHandle` 当初钉死 `PaneId(1)` 的原始顾虑(两块 pane
    /// attach 同一 session 会内容镜像 + 尺寸打架)的正解。
    ///
    /// 自证会变红:把 `pending_for_extra_pane` 里的
    /// `build_plan_without_tmux(tpl)` 换成 `build_plan(tpl, "x")`。
    #[test]
    fn an_extra_pane_runs_the_commands_but_never_attaches_tmux() {
        let mut a = enabled_automation();
        a.tmux = Some(TmuxChoice::Attach {
            session_name: Some("web01".into()),
        });

        // 前提:同一份配置给第一个 pane 时确实会发 tmux,否则下面恒真。
        let first = pending_for(Some(SessionId(7)), |_| Some((a.clone(), "web01".into())))
            .expect("第一个 pane 该有计划");
        assert!(String::from_utf8(first.steps[0].bytes.clone())
            .unwrap()
            .contains("tmux"));

        let extra = pending_for_extra_pane(&a).expect("配了命令就该有计划");
        for s in &extra.steps {
            let line = String::from_utf8(s.bytes.clone()).unwrap();
            assert!(!line.contains("tmux"), "分屏 pane 不许碰 tmux: {line}");
            assert!(line.contains("echo hi"), "登录后命令要照跑: {line}");
        }
    }

    /// 只配了 tmux(没有任何命令/环境/工作目录)的会话:分屏 pane 无事可做,
    /// 必须**连 oneshot 都不建** —— 否则状态栏会挂一句"自动化:进行中",
    /// 一直挂到超时,而实际上一个字节都不会发。
    #[test]
    fn an_extra_pane_with_only_tmux_configured_starts_nothing() {
        let mut a = enabled_automation();
        a.tmux = Some(TmuxChoice::Attach { session_name: None });
        a.commands.clear();
        assert!(
            pending_for(Some(SessionId(7)), |_| Some((a.clone(), "web01".into()))).is_some(),
            "前提:第一个 pane 仍有计划(就是那句 tmux)"
        );
        assert!(pending_for_extra_pane(&a).is_none());
    }

    /// F44 总开关关掉时,分屏 pane 同样一步不发 —— 否则"关掉自动化"这个开关
    /// 在分屏后会静默失效。
    #[test]
    fn an_extra_pane_respects_the_master_switch() {
        let mut a = enabled_automation();
        assert!(pending_for_extra_pane(&a).is_some(), "前提:开着时有计划");
        a.enabled = false;
        assert!(pending_for_extra_pane(&a).is_none());
    }

    /// 超时值必须跟着模板走,不能落回内置默认 —— 用户把 ready_timeout 调到
    /// 60s 是因为这台机器登录慢,分屏 pane 登录同样慢。
    ///
    /// 自证会变红:把 `ready_timeout_ms: tpl.ready_timeout_ms` 写成
    /// `DEFAULT_READY_TIMEOUT_MS`。
    #[test]
    fn an_extra_pane_inherits_the_configured_ready_timeout() {
        let mut a = enabled_automation();
        a.ready_timeout_ms = 60_000;
        assert_eq!(pending_for_extra_pane(&a).unwrap().ready_timeout_ms, 60_000);
    }

    /// CLI 直连(`mullion user@host -p 22 -i key`)没有 SessionId,不该跑自动化。
    /// 凭空猜一个 tmux 会话名去 attach 是最坏的行为。
    ///
    /// `lookup` 里直接 panic:它一旦被调用就说明有人在没有会话 id 的情况下
    /// 去查了库,那正是这条测试要挡的。
    ///
    /// 自证会变红:把 `pending_for` 开头的 `let id = id?;` 换成
    /// `let id = id.unwrap_or(SessionId(1));`。
    #[test]
    fn cli_direct_connect_has_no_plan() {
        let p = pending_for(None, |_| panic!("没有 SessionId 时不该去查会话库"));
        assert!(p.is_none());
    }

    /// 库里查不到这条会话(刚被删了、连接还在途)⟹ 不跑。
    #[test]
    fn missing_session_record_has_no_plan() {
        let p = pending_for(Some(SessionId(7)), |_| None);
        assert!(p.is_none());
    }

    /// tmux 会话名留空时,`build_plan` 拿会话名做 fallback —— 名字必须真的
    /// 流进去,否则 attach 的是别人的会话。
    ///
    /// 自证会变红:把 `pending_for` 里 `build_plan(&resolved, &name)` 的
    /// `&name` 换成 `""`。
    #[test]
    fn session_name_flows_into_the_plan_as_the_tmux_fallback() {
        let mut a = enabled_automation();
        a.tmux = Some(TmuxChoice::Attach { session_name: None });
        a.commands.clear();
        let p = pending_for(Some(SessionId(7)), |_| Some((a.clone(), "web01".into())))
            .expect("配了 tmux attach 就该有计划");
        let bytes = String::from_utf8(p.steps[0].bytes.clone()).unwrap();
        assert!(
            bytes.contains("'web01'"),
            "会话名没进 tmux 命令,实际字节: {bytes:?}"
        );
    }

    /// F44 一次性跳过:计划不空也不许起。
    ///
    /// 自证会变红:把 `take_pending` 里 `if std::mem::take(skip) { return None; }`
    /// 整段删掉。
    #[test]
    fn skip_flag_suppresses_start_even_when_plan_is_not_empty() {
        let mut pending = pending_for(Some(SessionId(7)), |_| {
            Some((enabled_automation(), "web01".into()))
        });
        assert!(pending.is_some(), "前提:计划非空");
        let mut skip = true;
        assert!(take_pending(&mut pending, &mut skip).is_none());
        assert!(
            pending.is_none(),
            "跳过时计划也必须被取走,否则会残留给下一次连接"
        );
    }

    /// 跳过**只**生效一次。右键跳过一次之后,普通双击连接若还静默跳过,
    /// 用户会以为自动化坏了。
    ///
    /// 自证会变红:把 `std::mem::take(skip)` 换成 `*skip`。
    #[test]
    fn skip_flag_is_consumed_after_one_connect() {
        let mut skip = true;

        let mut first = pending_for(Some(SessionId(7)), |_| {
            Some((enabled_automation(), "web01".into()))
        });
        assert!(
            take_pending(&mut first, &mut skip).is_none(),
            "第一次应跳过"
        );
        assert!(!skip, "跳过标志必须被消费掉");

        let mut second = pending_for(Some(SessionId(7)), |_| {
            Some((enabled_automation(), "web01".into()))
        });
        assert!(
            take_pending(&mut second, &mut skip).is_some(),
            "第二次连接不该再跳过"
        );
    }

    /// 待办计划也是一次性的:`ConnectOk` 取走之后不许留在 App 上,
    /// 否则下一次连接会拿到上一条会话的计划。
    #[test]
    fn pending_plan_is_taken_not_copied() {
        let mut pending = pending_for(Some(SessionId(7)), |_| {
            Some((enabled_automation(), "web01".into()))
        });
        let mut skip = false;
        assert!(take_pending(&mut pending, &mut skip).is_some());
        assert!(pending.is_none(), "计划必须被取走");
    }

    /// 六种结局各一条文案，字面量硬编码——不能拿生产代码里的同一个
    /// `format!` 当预期值，那是重言式。
    #[test]
    fn status_text_covers_every_outcome() {
        assert_eq!(status_text(Outcome::Completed(3)), "自动化已完成(3 步)");
        assert_eq!(status_text(Outcome::Aborted), "自动化已中止:检测到你的输入");
        assert_eq!(
            status_text(Outcome::SkippedTimeout { after_ms: 15_000 }),
            "自动化已跳过:15s 未收到远端输出"
        );
        assert_eq!(
            status_text(Outcome::Disconnected),
            "自动化已中止:连接已断开"
        );
        assert_eq!(status_text(Outcome::Congested), "自动化已中止:出站队列拥塞");
    }

    /// 亚秒超时不能显示成「0s」——那句话读起来像「一瞬间就放弃了」。
    ///
    /// 自证会变红:把 `div_ceil(1000)` 改成 `/ 1000`。
    #[test]
    fn sub_second_timeout_rounds_up_so_it_never_reads_zero() {
        assert_eq!(
            status_text(Outcome::SkippedTimeout { after_ms: 500 }),
            "自动化已跳过:1s 未收到远端输出"
        );
    }

    /// 跑着的时候必须盖住上一次的结论,否则新连接的状态栏挂着旧连接的话。
    ///
    /// 自证会变红:把 `status_line` 的两个分支对调。
    #[test]
    fn running_text_wins_over_the_previous_verdict() {
        assert_eq!(status_line(true, Some("旧的结论")), Some(RUNNING_TEXT));
        assert_eq!(status_line(false, Some("旧的结论")), Some("旧的结论"));
        assert_eq!(status_line(false, None), None);
    }

    /// F41:粘贴多行 ⟹ 拆成多条,空行丢弃,每条 trim。
    ///
    /// 拆条而不是剥换行:`a && b` 与 `a; b` 语义不同,静默拼接会改变行为;
    /// 拆条则结果仍符合数据层语义(默认仍合成一行用 `;` 连接发出)。
    ///
    /// 自证会变红:把 `.filter(|s| !s.is_empty())` 删掉(第二条断言红)。
    #[test]
    fn split_pasted_commands_splits_and_drops_blank_lines() {
        assert_eq!(
            split_pasted_commands("cd /srv\nls -la"),
            vec!["cd /srv".to_string(), "ls -la".to_string()]
        );
        assert_eq!(
            split_pasted_commands("cd /srv\n\n  ls -la  \n"),
            vec!["cd /srv".to_string(), "ls -la".to_string()]
        );
    }

    /// CRLF 与裸 CR 都要拆。裸 `\r` 在「一行模型」里就是一次额外回车,
    /// 漏掉它整条命令会被数据层丢弃。
    ///
    /// 自证会变红:把 `split(['\n', '\r'])` 改成 `split('\n')`。
    #[test]
    fn split_pasted_commands_handles_crlf_and_bare_cr() {
        assert_eq!(
            split_pasted_commands("a\r\nb\rc"),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    /// 单行原样(只 trim),全空白得空表。
    #[test]
    fn split_pasted_commands_leaves_a_single_line_alone() {
        assert_eq!(
            split_pasted_commands("  tmux ls  "),
            vec!["tmux ls".to_string()]
        );
        assert!(split_pasted_commands(" \r\n\t ").is_empty());
    }

    /// 在**空框**里敲回车必须真的多出一条空行。
    ///
    /// 这一条是 `split_edited_line` 存在的全部理由:直接用
    /// `split_pasted_commands` 的话,`"\n"` 的两个空段都会被过滤掉,函数返回空表,
    /// 界面上按键毫无反应 —— 而 spec §5 说的是「手敲回车 = 新增一条空行」。
    #[test]
    fn split_edited_line_on_an_empty_box_yields_one_blank_row() {
        assert_eq!(
            split_edited_line("\n"),
            (String::new(), vec![String::new()])
        );
    }

    /// 在**行首**敲回车 = 把当前这条整体压下去,上面空出一行。
    #[test]
    fn split_edited_line_at_line_start_pushes_the_text_down() {
        assert_eq!(
            split_edited_line("\nfoo"),
            (String::new(), vec!["foo".to_string()])
        );
    }

    /// 在**行尾**敲回车 = 本行留着,下面空出一行。
    #[test]
    fn split_edited_line_at_line_end_adds_a_blank_row_below() {
        assert_eq!(
            split_edited_line("foo\n"),
            ("foo".to_string(), vec![String::new()])
        );
    }

    /// 行中间敲回车 = 一刀两断;整段粘贴时中间的空行仍旧丢弃(沿用拆条语义)。
    #[test]
    fn split_edited_line_splits_in_the_middle_and_still_drops_inner_blanks() {
        assert_eq!(
            split_edited_line("foo\nbar"),
            ("foo".to_string(), vec!["bar".to_string()])
        );
        assert_eq!(
            split_edited_line("a\n\nb"),
            ("a".to_string(), vec!["b".to_string()])
        );
        // CRLF 中间那个空段是编码噪音,不是用户敲的空行,不能变成幽灵条目。
        assert_eq!(
            split_edited_line("a\r\nb"),
            ("a".to_string(), vec!["b".to_string()])
        );
    }

    use mullion_ssh::schedule::ByteSink;
    use mullion_ssh::session::TrySendErr;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// 假 sink:记录收到的字节。**零网络**。
    #[derive(Default)]
    struct FakeSink {
        written: Mutex<Vec<Vec<u8>>>,
        /// 置真后一律返回 Closed(模拟 channel 已关)。
        closed: bool,
        /// 置真后一律返回 Full(模拟出站队列持续满)。
        full: bool,
    }

    impl FakeSink {
        fn written(&self) -> Vec<Vec<u8>> {
            self.written.lock().unwrap().clone()
        }
    }

    impl ByteSink for FakeSink {
        fn write(&self, bytes: Vec<u8>) -> Result<(), TrySendErr> {
            if self.closed {
                return Err(TrySendErr::Closed);
            }
            if self.full {
                return Err(TrySendErr::Full);
            }
            self.written.lock().unwrap().push(bytes);
            Ok(())
        }
    }

    /// 两步计划:300ms 后 `a\r`,再 200ms 后 `b\r`。
    fn two_steps() -> Vec<Step> {
        vec![
            Step {
                delay: Duration::from_millis(300),
                bytes: b"a\r".to_vec(),
            },
            Step {
                delay: Duration::from_millis(200),
                bytes: b"b\r".to_vec(),
            },
        ]
    }

    /// 三条一次性通道 + sink,一把造好。
    #[allow(clippy::type_complexity)]
    fn harness() -> (
        Arc<FakeSink>,
        tokio::sync::oneshot::Sender<()>,
        tokio::sync::oneshot::Sender<()>,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<Outcome>,
    ) {
        let fake = Arc::new(FakeSink::default());
        let sink: Arc<dyn ByteSink> = fake.clone();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        let (disc_tx, disc_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(run(sink, two_steps(), ready_rx, cancel_rx, disc_rx, 15_000));
        (fake, ready_tx, cancel_tx, disc_tx, task)
    }

    /// 首字节到达之前一个字节都不许发 —— 核心约束:没确认对端已经在说话,
    /// 就没法确认那是不是一个干净 shell。
    ///
    /// 自证会变红:把 `run` 里第一段 `select!` 整个删掉,直接跑 `write_scheduled`。
    #[tokio::test(start_paused = true)]
    async fn ready_signal_starts_the_plan() {
        let (fake, ready_tx, _cancel_tx, _disc_tx, task) = harness();

        // 假时钟推 10 秒:没收到 ready,一个字节都不该发。
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert!(
            fake.written().is_empty(),
            "还没收到首字节就发了字节:{:?}",
            fake.written()
        );

        ready_tx.send(()).unwrap();
        let out = task.await.unwrap();
        assert_eq!(out, Outcome::Completed(2));
        assert_eq!(fake.written(), vec![b"a\r".to_vec(), b"b\r".to_vec()]);
    }

    /// 超时:一个字节都不发,**绝不补发**。
    ///
    /// 自证会变红:把 `sleep(timeout)` 那条分支的返回值改成继续跑计划。
    #[tokio::test(start_paused = true)]
    async fn no_first_byte_within_timeout_is_skipped() {
        let fake = Arc::new(FakeSink::default());
        let sink: Arc<dyn ByteSink> = fake.clone();
        // 注意:三个 sender 必须用具名绑定持有。写成裸 `_` 会当场 drop,
        // 接收端立刻就绪 → 走成取消/断线分支,测试会莫名其妙变红。
        let (_ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (_cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        let (_disc_tx, disc_rx) = tokio::sync::oneshot::channel();

        let out = run(sink, two_steps(), ready_rx, cancel_rx, disc_rx, 15_000).await;

        assert_eq!(out, Outcome::SkippedTimeout { after_ms: 15_000 });
        assert!(fake.written().is_empty(), "超时后不许补发任何字节");
    }

    /// 出站队列持续满 → `Congested`,**不能**被译成别的结局。
    ///
    /// 四个 `ScheduleOutcome` 变体各自映射到一个 `Outcome`,其余三条都有专门的
    /// 测试,唯独这条原先没有 —— 而它译错的现象是状态栏说「连接已断开」,用户
    /// 去查网络,真因却是本地出站队列堵着。`write_scheduled` 自己会不会产出
    /// `Congested` 由 `mullion-ssh` 侧的测试保证,这里只钉住**译码**。
    #[tokio::test(start_paused = true)]
    async fn a_permanently_full_outbound_queue_is_reported_as_congested() {
        let fake = Arc::new(FakeSink {
            full: true,
            ..Default::default()
        });
        let sink: Arc<dyn ByteSink> = fake.clone();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (_cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        let (_disc_tx, disc_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(run(sink, two_steps(), ready_rx, cancel_rx, disc_rx, 15_000));

        ready_tx.send(()).unwrap();
        assert_eq!(task.await.unwrap(), Outcome::Congested);
        assert!(fake.written().is_empty(), "一直 Full 就一个字节都没写出去");
    }

    /// 等待期用户就接管了(连上就急着敲键盘)。
    #[tokio::test(start_paused = true)]
    async fn cancel_before_ready_aborts() {
        let (fake, _ready_tx, cancel_tx, _disc_tx, task) = harness();
        cancel_tx.send(()).unwrap();
        assert_eq!(task.await.unwrap(), Outcome::Aborted);
        assert!(fake.written().is_empty());
    }

    /// 执行到一半用户接管:**剩余步骤一个字节都不再发**。
    ///
    /// 自证会变红:`run` 里把 `cancel` 换成一条新建的、永不就绪的接收端
    /// 再交给 `write_scheduled`。
    #[tokio::test(start_paused = true)]
    async fn cancel_during_run_stops_remaining_steps() {
        let (fake, ready_tx, cancel_tx, _disc_tx, task) = harness();
        ready_tx.send(()).unwrap();
        // 推进到第一步已发、第二步还在等的时刻。
        tokio::time::sleep(Duration::from_millis(350)).await;
        cancel_tx.send(()).unwrap();

        assert_eq!(task.await.unwrap(), Outcome::Aborted);
        assert_eq!(
            fake.written(),
            vec![b"a\r".to_vec()],
            "取消后剩余步骤一个字节都不许发(用户接管优先)"
        );
    }

    /// 等待期断线:结局必须是 `Disconnected` 而不是 `Aborted`。
    ///
    /// 这正是断线要独占一条 oneshot 的理由:复用 `cancel` 的话,
    /// `write_scheduled` 会返回 `Cancelled`,状态栏就显示「检测到你的输入」——
    /// 用户根本没输入。
    ///
    /// 自证会变红:把 `run` 第一段 `select!` 里 `disconnect` 那条分支的返回值
    /// 改成 `Outcome::Aborted`。
    #[tokio::test(start_paused = true)]
    async fn disconnect_before_ready_reports_disconnected_not_aborted() {
        let (fake, _ready_tx, _cancel_tx, disc_tx, task) = harness();
        disc_tx.send(()).unwrap();
        assert_eq!(task.await.unwrap(), Outcome::Disconnected);
        assert!(fake.written().is_empty());
    }

    /// 执行期断线同样报 `Disconnected`。
    #[tokio::test(start_paused = true)]
    async fn disconnect_during_run_reports_disconnected() {
        let (_fake, ready_tx, _cancel_tx, disc_tx, task) = harness();
        ready_tx.send(()).unwrap();
        tokio::time::sleep(Duration::from_millis(350)).await;
        disc_tx.send(()).unwrap();
        assert_eq!(task.await.unwrap(), Outcome::Disconnected);
    }

    /// sink 已关 ⟹ `write_scheduled` 自己就会报 `Disconnected`,
    /// 这条路径不依赖 app 侧的断线信号。
    #[tokio::test(start_paused = true)]
    async fn closed_sink_reports_disconnected() {
        let fake = Arc::new(FakeSink {
            closed: true,
            ..Default::default()
        });
        let sink: Arc<dyn ByteSink> = fake.clone();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (_cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        let (_disc_tx, disc_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(run(sink, two_steps(), ready_rx, cancel_rx, disc_rx, 15_000));
        ready_tx.send(()).unwrap();
        assert_eq!(task.await.unwrap(), Outcome::Disconnected);
    }

    /// `initial_delay_ms` 已经是第一个 Step 的 delay,`run` 收到 ready 后
    /// **不许再等一次** —— 那会让实际延时翻倍,而这种 bug 在真机上只表现为
    /// 「感觉慢了一点」,没人会发现。
    ///
    /// 自证会变红:在 `run` 收到 ready 之后、`write_scheduled` 之前插一条
    /// `tokio::time::sleep(Duration::from_millis(300)).await;`。
    #[tokio::test(start_paused = true)]
    async fn ready_does_not_add_an_extra_initial_delay() {
        let fake = Arc::new(FakeSink::default());
        let sink: Arc<dyn ByteSink> = fake.clone();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (_cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        let (_disc_tx, disc_rx) = tokio::sync::oneshot::channel();
        let steps = vec![Step {
            delay: Duration::from_millis(300),
            bytes: b"a\r".to_vec(),
        }];
        let task = tokio::spawn(run(sink, steps, ready_rx, cancel_rx, disc_rx, 15_000));

        ready_tx.send(()).unwrap();
        let start = tokio::time::Instant::now();
        assert_eq!(task.await.unwrap(), Outcome::Completed(1));
        // 硬编码 300:不能拿 `DEFAULT_INITIAL_DELAY_MS` 当预期值(重言式)。
        assert_eq!(
            start.elapsed(),
            Duration::from_millis(300),
            "ready 之后只该等计划自己的 300ms,多等一次就是延时翻倍"
        );
    }
}
