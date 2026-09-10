//! F254:「此刻持有几条会话 channel」的账本。
//!
//! ## 为什么需要它
//!
//! 分屏从 2 变 3 时报「开 PTY 失败 —— 对端可能不允许 PTY」,那句话把四件
//! 完全不同的事说成了同一件(见 `error::ConnectError::SessionChannel` 的
//! `stage`),而其中最可能的一件是**撞上了 sshd 的 `MaxSessions`**(默认 10)。
//! 判断是不是它,唯一的客观依据就是「失败这一刻我们手上有几条」。
//!
//! ## 这个数的口径(**别拿它跟 MaxSessions 对账**)
//!
//! 它数的是「**我们**认为自己持有的会话 channel」,不是服务端认为的。两者
//! 在几个地方必然不同:
//!
//! - CHANNEL_CLOSE 有一个来回。我们这边 guard 一 drop 就减了,服务端要等收到
//!   报文才减 —— 关掉一个 pane 后立刻开一个新的,服务端眼里可能仍是满的。
//! - `direct-tcpip`(F110 系列的端口转发)**不进这本账**:`MaxSessions` 只
//!   数 session 类型的 channel,转发不占。把它们算进来会让「9 条」这个数字
//!   在开着隧道时凭空虚高,反而误导。
//! - 一条连接上的 sftp channel(F50 文件面板)和 exec channel(F57 等)都
//!   **是** session 类型,所以它们在账上 —— 用户开着文件面板时分屏上限确实
//!   会少一条,这不是账错了。
//!
//! 所以这个数只能用来**给人看**(「持有 9 条」+ 服务端回了 `ResourceShortage`
//! 基本就定案了),不许拿它做「≥10 就不发请求」这类判断:算错一格就变成
//! 「明明还能开却拒绝开」,而且用户没有任何办法绕过。
//!
//! ## 两本账
//!
//! 每个 guard 同时记两本:自己那条连接的,和一本**进程级**的
//! ([`in_process`])。进程级那本给性能剖面行用(F155),它回答的是
//! 「这个 exe 一共挂着多少条」—— 多标签多连接时那才是资源总量。
//!
//! 两本账绑在同一个 guard 上是刻意的:分开两次登记的话,「加了连接级、忘了
//! 加进程级」不会有任何症状,只是剖面行里的数字长期偏小,而没人能发现。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// 进程级的那本账。见模块文档「两本账」。
static IN_PROCESS: AtomicUsize = AtomicUsize::new(0);

/// 这个进程此刻持有多少条会话 channel(所有连接加总)。
///
/// 给性能剖面行用。**同 [`ChannelLedger::held`] 的口径警告** —— 这是我们
/// 自己的账,不是服务端的。
pub fn in_process() -> usize {
    IN_PROCESS.load(Ordering::Relaxed)
}

/// 一条连接的账本。克隆出来的副本共享同一份计数(内部 `Arc`)。
#[derive(Debug, Clone, Default)]
pub struct ChannelLedger {
    held: Arc<AtomicUsize>,
}

impl ChannelLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// 此刻这条连接持有几条会话 channel。口径见模块文档。
    pub fn held(&self) -> usize {
        self.held.load(Ordering::Relaxed)
    }

    /// 记上一条。返回的 [`ChannelGuard`] 一 `Drop` 就把两本账都减回去。
    ///
    /// **必须在 `channel_open_session()` 成功之后才记**:开都没开出来的
    /// channel 不占服务端任何槽位,提前记会让报错里那个数字虚高一条,
    /// 而排查的人恰恰是在拿它跟 `MaxSessions` 比。
    pub fn check_out(&self) -> ChannelGuard {
        self.held.fetch_add(1, Ordering::Relaxed);
        IN_PROCESS.fetch_add(1, Ordering::Relaxed);
        ChannelGuard {
            held: Arc::clone(&self.held),
        }
    }
}

/// 一条会话 channel 的持有凭证。**靠 `Drop` 记账**。
///
/// 为什么是 RAII 而不是「用完调一下 `release()`」:归还点有五处以上
/// (`open_pty` 三段失败路径、`io_task` 正常结束、`exec` 的每个 `return`、
/// `SftpClient` 被丢弃),漏掉任何一处的表现是**这本账只增不减** —— 而它
/// 只出现在错误文案与剖面行里,不会有任何报错,等到有人发现「持有 47 条」
/// 明显不可能时,早就误导过好几轮排查了。
///
/// 所以 guard 必须被**移动进那条 channel 的所有者**(`io_task` 的参数 /
/// `SftpClient` 的字段 / `exec` 的栈帧),让所有权本身保证归还。
#[derive(Debug)]
pub struct ChannelGuard {
    held: Arc<AtomicUsize>,
}

impl Drop for ChannelGuard {
    fn drop(&mut self) {
        // `fetch_sub` 在 0 上会绕回 `usize::MAX`。理论上到不了(guard 只能
        // 由 `check_out` 造出来,一造一减配平),但一旦真的到了,绕回的
        // 那个天文数字会出现在用户的错误文案里 —— 夹一下,宁可少记一条。
        let _ = self
            .held
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                Some(n.saturating_sub(1))
            });
        let _ = IN_PROCESS.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
            Some(n.saturating_sub(1))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **本模块的每条测试都要先拿这把锁**。
    ///
    /// [`IN_PROCESS`] 是进程级的,而 `cargo test` 默认多线程并行:两条测试
    /// 同时各持一个 guard 时,「差值 +1」这种判据会被对方的 `check_out` /
    /// `Drop` 打乱 —— 症状是**偶发**假红(第一次写这几条时就撞上了:一条
    /// 测试的 guard 在另一条量完 `before` 之后才 drop)。
    ///
    /// 不改判据去迁就并行:差值判据正是唯一能杀掉「忘了记进程级那本账」的
    /// 写法(见 `a_guard_is_also_counted_in_the_process_wide_book`)。
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// 拿锁。`unwrap_or_else` 而不是 `unwrap`:别的测试 panic 会让锁中毒,
    /// 那之后**所有**这几条都变红,真正的失败原因就被埋了。
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 一条 guard 记一条,`Drop` 还一条。
    ///
    /// 自证会变红:把 `check_out` 里的 `fetch_add` 删掉,或把 `Drop` 删掉。
    #[test]
    fn a_guard_holds_one_slot_and_hands_it_back_when_dropped() {
        let _serial = serial();
        let l = ChannelLedger::new();
        assert_eq!(l.held(), 0);
        let g = l.check_out();
        assert_eq!(l.held(), 1);
        let g2 = l.check_out();
        assert_eq!(l.held(), 2);
        drop(g);
        assert_eq!(l.held(), 1, "drop 一条没还回来");
        drop(g2);
        assert_eq!(l.held(), 0, "两条都 drop 了账上还有");
    }

    /// 克隆出来的账本看的是**同一份**计数。
    ///
    /// `SshConnection` 会把账本 clone 一份给 `open_pty`/`exec`/`SftpClient`,
    /// 各看一份独立计数的话,错误文案里永远是「持有 1 条」—— 那个数字就
    /// 彻底没用了,而且看不出是坏的。
    ///
    /// 自证会变红:把 `held` 的类型从 `Arc<AtomicUsize>` 改成 `AtomicUsize`
    /// 并让 `Clone` 复制它的值。
    #[test]
    fn a_cloned_ledger_shares_the_same_count() {
        let _serial = serial();
        let a = ChannelLedger::new();
        let b = a.clone();
        let _g = a.check_out();
        assert_eq!(b.held(), 1, "克隆出来的账本各记各的");
    }

    /// guard 同时记**进程级**那本账。
    ///
    /// 判据只能是「差值」,不能是绝对值:进程级那本是全局的,拿不到一个
    /// 「本该是多少」的基准。而差值判据要求量 `before` 与 drop 之间没有别人
    /// 动这本账 —— 所以本模块全部测试串行(见 `LOCK`)。真连接的路径都要
    /// 网络,同 crate 里不会有别的测试 `check_out`。
    ///
    /// 自证会变红:把 `check_out` 里那句 `IN_PROCESS.fetch_add` 删掉
    /// (**这正是最容易漏的一处**:连接级那本有测试盯着,进程级那本只出现在
    /// 剖面行里,漏了只是数字长期偏小)。
    #[test]
    fn a_guard_is_also_counted_in_the_process_wide_book() {
        let _serial = serial();
        let before = in_process();
        let l = ChannelLedger::new();
        let g = l.check_out();
        assert_eq!(in_process(), before + 1, "进程级那本没记上");
        drop(g);
        assert_eq!(in_process(), before, "进程级那本没还回来");
    }

    /// `Drop` 在 0 上不许绕回 `usize::MAX`。
    ///
    /// 造不出「多还一次」的 guard(`check_out` 是唯一入口),所以直接对着
    /// 那个夹紧逻辑本身验:拿一本手动置 0 的账本 drop 一个 guard。
    ///
    /// **每一处开会话通道的地方都得记账。**
    ///
    /// 上面四条测的是账本自己,而账本再对也挡不住「某条路径压根没调
    /// `check_out`」—— 那条路径开出来的通道不上账,错误文案里的数字就偏小,
    /// 而看的人正拿它跟 `MaxSessions` 比。这一族缺陷(纯逻辑测得扎实、接线
    /// 没人看着)在本仓库反复出现过。
    ///
    /// 判据用**成对计数**而不是「逐个文件点名」:点名式的清单在**加**一处
    /// 开点时必然漏(本仓库的「列举式门控」踩过多次),而成对计数对新增
    /// 天然生效 —— 新开一处不记账,这条就红。
    ///
    /// 只数**会话**类型:`tunnel.rs` 走的是 `channel_open_direct_tcpip`,
    /// sshd 的 `MaxSessions` 不算转发,故意不在这本账上(见本模块开头)。
    ///
    /// 自证会变红:删掉 `exec.rs` 里那句 `check_out()`(实测过)。
    #[test]
    fn every_place_that_opens_a_session_channel_also_puts_it_on_the_books() {
        // 扫的是三个**兄弟文件**,不含本文件 —— 本测试自己写了这两个串,
        // 连自己一起扫的话数字必然对不上(自匹配陷阱,本仓库单独立过项)。
        for (name, src) in [
            ("exec.rs", include_str!("exec.rs")),
            ("session.rs", include_str!("session.rs")),
            ("sftp.rs", include_str!("sftp.rs")),
        ] {
            let opens = src.matches("channel_open_session()").count();
            let books = src.matches("ledger().check_out()").count();
            assert!(opens > 0, "{name} 里一处会话通道都没开?锚点该更新了");
            assert_eq!(
                opens, books,
                "{name}:开了 {opens} 处会话通道,只记了 {books} 处的账 —— \
                 漏记的那处开出来的通道不上账,报错里的「持有 N 条」会偏小"
            );
        }
    }

    /// 自证会变红:把 `Drop` 里的 `saturating_sub` 换成 `n - 1`(那会
    /// panic)或 `fetch_sub`(那会绕回)。
    #[test]
    fn handing_back_a_slot_that_is_not_on_the_books_does_not_wrap_around() {
        let _serial = serial();
        let l = ChannelLedger::new();
        let g = l.check_out();
        // 模拟「账被别处清零了」——绕回的话下面就是 usize::MAX。
        l.held.store(0, Ordering::Relaxed);
        drop(g);
        assert_eq!(l.held(), 0, "在 0 上还一条把账绕回去了");
    }
}
