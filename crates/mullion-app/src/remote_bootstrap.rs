//! F124:远端状态上报自举 —— 纯逻辑。零 IO、零 async,真正发 exec 在 `app.rs`。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// 塞给 tmux 的 `set-titles-string`。
///
/// - `#S` 会话名、`#I` 窗口序号、`#W` 窗口名 —— 前两段正是 `parse_title`
///   认会话名的判据(第一段是名字、第二段必须是纯数字)。
/// - `#{pane_current_path}` 是 tmux **自己**从 OS 拿的 pane 当前目录,不依赖
///   shell 发 OSC 7(tmux 会吃掉内层 OSC 7 不转发,那是 F51 被否的理由之一)。
///
/// **刻意覆写而不是追加到用户现值**:tmux 默认值里的 `#T`(pane 标题)可能
/// 先吐出一个像路径的 token,`parse_title` 取「第一个 `/` 开头的空白分隔
/// token」会拿错。也刻意不做「只在现值是 tmux 默认时才写」——那样在改过
/// 这个选项的机器上目录继承会**静默**失效,用户看不出为什么。
pub const TMUX_TITLES_STRING: &str = "#S:#I:#W #{pane_current_path}";

/// 两次尝试之间至少隔多久(毫秒)。
///
/// **由帧驱动,不排 `WaitUntil`**:事件循环空闲时会 `ControlFlow::Wait`,
/// 再加一个定时唤醒源就要在三个分支里都复位 control_flow(T7),不值得。
/// 而且用户真去开 tmux 的时候必然在敲键盘 —— 敲键盘就有帧,判据自然会跑到。
/// 完全空闲时不重试是对的:那时也没人在开 tmux。
pub const RETRY_MS: u64 = 30_000;

/// 生产用的自举命令。
pub fn bootstrap_command() -> Vec<u8> {
    bootstrap_command_with("tmux")
}

/// 同上,但可以换 tmux 可执行名/参数 —— live 测试用 `"tmux -L mullion-test"`
/// 打到一个隔离的 socket 上,不去动开发机上真在用的那个 tmux 服务器。
///
/// 两条用 `&&` 串:退出码 0 当且仅当两条都成功。用 `;` 的话第一条失败也可能
/// 返回 0,我们会把「没配上」记成「已成功」然后**永不再试**。
///
/// `tmux` **原样拼进 shell 命令串,不转义** —— 它只接受受信的字面量
/// (生产是 `"tmux"`,live 测试是 `"tmux -L mullion-test"`)。别把用户输入
/// 或远端回来的东西传进来。
pub fn bootstrap_command_with(tmux: &str) -> Vec<u8> {
    format!("{tmux} set -g set-titles on && {tmux} set -g set-titles-string '{TMUX_TITLES_STRING}'")
        .into_bytes()
}

/// 这一帧该不该发起一次自举。
///
/// `since_last_ms` = 距上次**发起**多久;`None` = 这条连接上还没试过。
pub fn should_attempt(enabled: bool, done: bool, busy: bool, since_last_ms: Option<u64>) -> bool {
    if !enabled || done || busy {
        return false;
    }
    match since_last_ms {
        None => true,
        Some(ms) => ms >= RETRY_MS,
    }
}

/// 一条连接上自举的共享状态。
///
/// **发起侧只有事件循环这一个线程**:「判到该发 → `mark_busy`」在同一次 tick
/// 里同步走完,所以 `busy` 不需要 CAS,两次 tick 之间也不会重叠发起。后台
/// task 只写 `finish`。多线程发起会破坏这个前提。
///
/// `Arc<AtomicBool>` 而不是裸 `bool`:结论在 tokio task 里产生、判据在事件
/// 循环里读,两边必须看到同一份。用 `UserEvent` 回送也行,但那要多一条按世代
/// 路由的事件变体 —— 这里的结论只有一个 bit,不值得。
#[derive(Debug, Clone, Default)]
pub struct BootstrapFlags {
    done: Arc<AtomicBool>,
    busy: Arc<AtomicBool>,
}

impl BootstrapFlags {
    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::Relaxed)
    }
    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::Relaxed)
    }
    /// 发起前置位,防止上一次还挂在网络上时又发一次。
    pub fn mark_busy(&self) {
        self.busy.store(true, Ordering::Relaxed);
    }
    /// 收工。**只有成功才 latch `done`** —— 失败也 latch 的话,「tmux 服务器
    /// 还没起」(最常见的失败)会让这条连接从此再不重试,用户几秒后开了
    /// tmux 也配不上。
    pub fn finish(&self, ok: bool) {
        if ok {
            self.done.store(true, Ordering::Relaxed);
        }
        self.busy.store(false, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 命令必须用 `&&` 串联两条 `tmux set`:退出码 0 当且仅当两条都成功,
    /// 不需要解析 stderr。换成 `;` 的话第一条失败、第二条成功也会返回 0,
    /// 我们就会把「没配上」记成「已成功」并**永不再试**。
    ///
    /// 自证会变红:把 `&&` 换成 `;`。
    #[test]
    fn the_command_chains_both_settings_with_and_so_the_exit_code_means_both() {
        let cmd = String::from_utf8(bootstrap_command()).expect("命令是 ASCII");
        assert!(cmd.contains("set -g set-titles on"), "少了开关那条:{cmd}");
        assert!(
            cmd.contains("&& tmux set -g set-titles-string"),
            "两条不是用 && 串的:{cmd}"
        );
    }

    /// format string 必须带 `#{pane_current_path}` —— 少了它 tmux 只报会话名,
    /// 目录名和 SFTP 目录继承两个功能都静默失效。整串用**单引号**包住:
    /// `#{...}` 里的 `{`/`}` 和 `#` 在 shell 里不能裸奔。
    ///
    /// 自证会变红:把 `TMUX_TITLES_STRING` 改成 `"#S:#I:#W"`。
    #[test]
    fn the_format_string_carries_the_pane_path_and_is_single_quoted() {
        let cmd = String::from_utf8(bootstrap_command()).expect("命令是 ASCII");
        assert!(cmd.contains("'#S:#I:#W #{pane_current_path}'"), "{cmd}");
    }

    /// 换 tmux 可执行名的那条(live 测试要用 `tmux -L mullion-test`)走的是
    /// **同一份模板**,不是另抄一遍 —— 抄一遍的话 live 测试验的就不是生产命令。
    #[test]
    fn the_production_command_is_the_template_with_the_plain_tmux_binary() {
        assert_eq!(bootstrap_command(), bootstrap_command_with("tmux"));
        let alt = String::from_utf8(bootstrap_command_with("tmux -L probe")).expect("ASCII");
        assert!(
            alt.starts_with("tmux -L probe set -g set-titles on"),
            "{alt}"
        );
        assert!(
            alt.contains("&& tmux -L probe set -g set-titles-string"),
            "{alt}"
        );
    }

    /// 判据的五条分支。
    ///
    /// 自证会变红:
    /// - 去掉 `enabled` 那条 → 第 1 条红
    /// - 去掉 `done` 那条 → 第 2 条红
    /// - 去掉 `busy` 那条 → 第 3 条红
    /// - 把 `None` 当成「不试」→ 第 4 条红
    /// - 把 `>=` 写成 `>` 之外的任何放宽 → 第 5 条红
    #[test]
    fn should_attempt_covers_every_early_exit() {
        // 1. 开关关着:什么都不做。
        assert!(!should_attempt(false, false, false, None));
        // 2. 已经配成功过:永不再试(tmux 全局选项在 server 生命期内一直有效)。
        assert!(!should_attempt(true, true, false, Some(RETRY_MS * 10)));
        // 3. 上一次还在途:不重叠发起(链路黑洞时一次 exec 可能挂很久)。
        assert!(!should_attempt(true, false, true, Some(RETRY_MS * 10)));
        // 4. 从没试过:立刻试。
        assert!(should_attempt(true, false, false, None));
        // 5. 距上次不足 RETRY_MS:等着;到点了才试。
        assert!(!should_attempt(true, false, false, Some(RETRY_MS - 1)));
        assert!(should_attempt(true, false, false, Some(RETRY_MS)));
    }

    /// 标志位的状态机。失败**不置 done** —— 置了就永不再试,而「tmux 服务器
    /// 还没起」正是最常见的失败原因,用户几秒后开 tmux 就再也配不上了。
    ///
    /// 自证会变红:把 `finish(false)` 也写成 `done.store(true, ..)`。
    #[test]
    fn flags_only_latch_done_on_success() {
        let f = BootstrapFlags::default();
        assert!(!f.is_done() && !f.is_busy());

        f.mark_busy();
        assert!(f.is_busy());
        f.finish(false);
        assert!(!f.is_busy(), "失败后要解锁,否则再也不会重试");
        assert!(!f.is_done(), "失败不该被记成成功");

        f.mark_busy();
        f.finish(true);
        assert!(f.is_done() && !f.is_busy());
    }

    /// `clone` 出来的是**同一份**标志(后台 task 拿走一份、主循环留一份,
    /// 两边看到的必须是同一个状态)。
    ///
    /// 自证会变红:把 `BootstrapFlags` 的 `Arc<AtomicBool>` 换成裸 `bool`
    /// 并 `#[derive(Clone, Copy)]` —— 后台 task 置的 done 主循环永远看不见,
    /// 表现为每 30 秒重配一次,永不停止。
    #[test]
    fn cloned_flags_share_one_state() {
        let a = BootstrapFlags::default();
        let b = a.clone();
        b.finish(true);
        assert!(a.is_done(), "clone 出来的是另一份状态,后台结论传不回主循环");
    }
}
