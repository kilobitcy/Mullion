//! F205:一次拨号一张票。
//!
//! 背景(用户实报,反复复发三轮):「这次连上的是哪条会话」原先记在 `App` 的
//! **单个字段**里(`ui.connect_request_last`),而 `ConnectOk` 事件本身不带
//! `SessionId` —— 它是从 tokio task 发回来的,回来时只有一个 `SshConnection`。
//! 同类的单槽还有三个:`pending_cfg`、`pending_automation.plan/template/
//! session_name`。
//!
//! `spawn_connect` 没有任何「在途只许一条」的闸,而本项目的主场景恰恰是
//! **高延迟代理链路**:一次连接要好几秒。用户在第一条还没连上时点了第二条,
//! 四个单槽就被后者整体盖掉。第一条的 `ConnectOk` 抵达时,拿到的是第二条
//! 会话的身份 ——
//!
//! - 标签 A 的文件面板填的是会话 B 的书签(A 的看起来「消失了」);
//! - **更糟**:此后在 A 上点 ☆ 收藏,按 `tab.session_id` 落盘,写进了 B 的记录;
//! - A 上分屏时按 B 的 `cfg` 开 pty(term/尺寸取自另一台机器);
//! - **最糟**:给 A 配的登录后命令,在 B 的终端里执行。
//!
//! 全程没有任何报错。前两轮修复(F187/F189)都修在 store 那一层 —— 而错的
//! 是**身份归属**这一层,和 F188/T11 是同一族:判据放错了层。
//!
//! 这里只做台账,**不认识 payload 是什么**:随行数据的类型(`SshConfig`、
//! 自动化计划)住在 `app.rs`,而 `shell` 这一层不许认识 UI/事件循环的类型。
//! 泛型换来的额外好处是测试可以拿 `&str` 当 payload,不必为了测台账去构造
//! 一个真的 `SshConfig`。

/// 一次拨号的票号。发出去时随任务走,`ConnectOk`/`ConnectErr` 原样带回来。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DialId(pub u64);

/// 在途拨号台账。
///
/// 用 `Vec` 而不是 `HashMap`:在途拨号是个位数(用户手点出来的),线性扫比
/// 哈希快,而且顺序稳定 —— 调试日志里「现在台账上还挂着谁」读起来是有序的。
#[derive(Debug)]
pub struct DialLedger<T> {
    next: u64,
    open: Vec<(DialId, T)>,
}

impl<T> Default for DialLedger<T> {
    fn default() -> Self {
        Self {
            next: 1,
            open: Vec::new(),
        }
    }
}

impl<T> DialLedger<T> {
    /// 发一张票,把这次拨号的随行数据存进来。
    pub fn issue(&mut self, payload: T) -> DialId {
        let id = DialId(self.next);
        self.next += 1;
        self.open.push((id, payload));
        id
    }

    /// 认领。**取出即消费**:同一张票认第二次拿不到东西 —— 一次拨号只会有
    /// 一个结局(`ConnectOk` 或 `ConnectErr`),认两次说明接线接错了,让它
    /// 返回 `None` 好过让两个标签共用一份随行数据。
    pub fn claim(&mut self, id: DialId) -> Option<T> {
        let ix = self.open.iter().position(|(k, _)| *k == id)?;
        Some(self.open.remove(ix).1)
    }

    /// 台账上还挂着几张票。只给日志和测试用。
    pub fn len(&self) -> usize {
        self.open.len()
    }

    pub fn is_empty(&self) -> bool {
        self.open.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **这条就是那个 bug 本身。**
    ///
    /// 两条拨号同时在途(高延迟链路上再平常不过),先发起的先连上 —— 它必须
    /// 拿回**自己**那份随行数据,而不是后发起那条的。
    ///
    /// 自证会变红:把 `open: Vec<..>` 换成 `last: Option<(DialId, T)>`,
    /// `issue` 只记最后一张 —— 那正是 `ui.connect_request_last` 今天的样子。
    #[test]
    fn two_dials_in_flight_each_get_their_own_payload_back() {
        let mut led: DialLedger<&str> = DialLedger::default();
        let a = led.issue("会话A");
        let b = led.issue("会话B");
        assert_ne!(a, b, "两次拨号必须拿到不同票号,否则后一张会顶掉前一张");
        assert_eq!(
            led.claim(a),
            Some("会话A"),
            "先发起的那条认领到了别人的身份"
        );
        assert_eq!(led.claim(b), Some("会话B"), "后发起的那条也要认得回自己");
    }

    /// 一次拨号只有一个结局。同一张票被认两次 = 接线接错了(比如 `ConnectOk`
    /// 和 `ConnectErr` 都认了一遍),这时**宁可什么都不给**——给了的话两个
    /// 标签会共用同一份 cfg / 自动化计划,症状比现在这个 bug 更难查。
    #[test]
    fn a_ticket_can_only_be_claimed_once() {
        let mut led: DialLedger<u32> = DialLedger::default();
        let a = led.issue(7);
        assert_eq!(led.claim(a), Some(7));
        assert_eq!(led.claim(a), None, "同一张票认了第二次还给东西");
    }

    /// 认一张不存在的票不许 panic —— 事件是从 tokio task 发回来的,标签早被
    /// 关掉、任务被 abort 之后仍可能有迟到的事件抵达。
    #[test]
    fn claiming_an_unknown_ticket_is_not_a_panic() {
        let mut led: DialLedger<u32> = DialLedger::default();
        assert_eq!(led.claim(DialId(999)), None);
    }

    /// 台账必须能空回去。每条拨号最终都会被 `ConnectOk` 或 `ConnectErr` 认领,
    /// 认领即摘除 —— 只涨不落的话,长时间跑下来台账里全是死票,而票里装着
    /// `SshConfig`(含主机名/端口/认证方式),等于一直不释放。
    #[test]
    fn a_claimed_ticket_leaves_the_ledger() {
        let mut led: DialLedger<u32> = DialLedger::default();
        let a = led.issue(1);
        let b = led.issue(2);
        assert_eq!(led.len(), 2);
        led.claim(a);
        led.claim(b);
        assert!(led.is_empty(), "认领完台账没空 —— 随行数据一直不释放");
    }
}
