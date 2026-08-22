//! 渲染侧:攒帧状态机(T2)+ 渲染接口占位(ADR-001)。
//!
//! 攒帧(T2/F11):同步输出(DEC 2026)在 BSU(`CSI ? 2026 h`)与 ESU(`CSI ? 2026 l`)
//! 之间攒住不画,收到 ESU 才提交一帧。这是消灭闪烁的根治手段。

const BSU: &[u8] = b"\x1b[?2026h";
const ESU: &[u8] = b"\x1b[?2026l";
/// BSU/ESU 只差末字节,共享这段前缀;跨 feed 边界时最多需要留住它。
const SYNC_PREFIX_MAX: usize = BSU.len() - 1;

/// 同步块最长攒帧时长。超过就强行出帧,不再等 ESU。
///
/// 攒帧的前提是对端会把 ESU 发完。它不发(TUI 被 kill / 链路截断 / 对端 bug)时,
/// 没有这道闸就是**画面永久冻结**——用户看到的是「键盘没有任何反应」,而字节其实
/// 一直在正常收发。宁可闪一下,也不能停在死画面上。DEC 2026 规范本身也要求终端
/// 侧设超时;150ms 与 alacritty/contour 同量级。
const SYNC_TIMEOUT_MS: u64 = 150;

/// 攒帧状态机:跟踪是否处于同步块,以及是否有待提交的脏数据。
#[derive(Debug, Default)]
pub struct SyncFramePacer {
    in_sync: bool,
    dirty: bool,
    /// 进入同步块的时刻,用于 [`SYNC_TIMEOUT_MS`] 逃生。
    sync_since_ms: u64,
    /// 上一段末尾那截「可能是被切开的 BSU/ESU 前缀」,与下一段拼接后再扫。
    /// 最长 [`SYNC_PREFIX_MAX`] 字节。
    tail: Vec<u8>,
    /// F155/T2 剖面:进入过多少次同步块(每个 BSU→ESU/超时算一次)。
    blocks: u32,
    /// F155/T2 剖面:有多少个同步块是靠 [`SYNC_TIMEOUT_MS`] 逃生门硬挤出来的
    /// (对端没把 ESU 发完)。
    timeouts: u32,
    /// 当前这个同步块的超时是否已经计过数——闩住,避免连续多帧重复计。
    timeout_counted: bool,
}

impl SyncFramePacer {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂入一段来自对端的字节:标脏,并探测 BSU/ESU 切换攒帧状态。
    ///
    /// `now_ms` 用于同步块超时(见 [`SYNC_TIMEOUT_MS`])。
    ///
    /// **必须跨 feed 边界匹配**:一段字节就是一次 SSH `ChannelMsg::Data`,TCP 完全
    /// 可以把 `\x1b[?2026l` 切成 `\x1b[?2026` + `l` 两段。只在单段内 `starts_with`
    /// 的话这个 ESU 就检测不到,`in_sync` 永远为真,`should_present()` 恒 false,
    /// 画面永久冻结。故把上段残留的前缀留在 `tail` 里接着扫。
    pub fn feed(&mut self, bytes: &[u8], now_ms: u64) {
        if !bytes.is_empty() {
            self.dirty = true;
        }
        // tail 里只可能是 BSU/ESU 的不完整前缀(完整的上一轮就消费掉了),
        // 重扫一遍不会重复触发状态切换。
        let mut buf = std::mem::take(&mut self.tail);
        buf.extend_from_slice(bytes);
        let mut i = 0;
        while i < buf.len() {
            if buf[i..].starts_with(BSU) {
                if !self.in_sync {
                    self.in_sync = true;
                    self.sync_since_ms = now_ms;
                    self.blocks = self.blocks.saturating_add(1);
                    self.timeout_counted = false;
                }
                i += BSU.len();
            } else if buf[i..].starts_with(ESU) {
                self.in_sync = false;
                i += ESU.len();
            } else {
                i += 1;
            }
        }
        // 末尾若是 BSU/ESU 的真前缀就留到下次(k<=SYNC_PREFIX_MAX 时两者相同,查 BSU 即可)。
        let keep = (1..=SYNC_PREFIX_MAX.min(buf.len()))
            .rev()
            .find(|&k| buf[buf.len() - k..] == BSU[..k])
            .unwrap_or(0);
        self.tail = buf[buf.len() - keep..].to_vec();
    }

    /// 是否有新数据要画。多 pane 聚合时这是「任一」(OR)条件 —— 只要某个 pane
    /// 有新内容,整帧就值得画;不能因为另一个 pane 静止(长期不脏)就永远不出帧。
    ///
    /// 目前只在本文件内被 [`panes_ready_to_present`] 调用,`pub` 是为将来按
    /// pane 粒度查询留的口子(例如给 pane 标题条加同步中/高负载徽标),
    /// 不是已有外部调用方。
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// 是否卡在**未超时**的同步块内(T2/F11)。多 pane 聚合时这是「任一」(AND
    /// 取反,即"没有一个"pane 在按住)条件 —— 只要某个 pane 还在攒帧,画出来
    /// 就是半更新的同步块,也就是撕裂,不能因为它本身不脏就放过。
    ///
    /// 之所以要跟 [`is_dirty`](Self::is_dirty) 分开查询:同步门控与"有没有新数据"
    /// 是两件独立的事 —— 一个 pane 可以在同步区间内被上一帧 present 过(脏标记
    /// 已清)但仍未收到 ESU,这时它依然在按住整帧,即使自己不脏。
    ///
    /// 同 [`is_dirty`](Self::is_dirty):目前只在本文件内被调用(`should_present`、
    /// `panes_ready_to_present`、`holding_deadline_ms`),`pub` 是预留的按 pane
    /// 查询口子,不是已有外部调用方。
    pub fn is_holding_frame(&self, now_ms: u64) -> bool {
        self.in_sync && now_ms.saturating_sub(self.sync_since_ms) < SYNC_TIMEOUT_MS
    }

    /// 若正卡在未超时的同步块内,返回它的超时时刻(ms,与传入的 `now_ms` 同一
    /// 时钟基准)。事件循环拿它去主动排一次 `WaitUntil`——`SYNC_TIMEOUT_MS`
    /// 本身只是「过了这个点该出帧」的判定阈值,没有代码在那个时刻真的把事件
    /// 循环叫醒的话,冻住的画面只能等下一个不相关事件(鼠标移动/别的 pane
    /// 来字节)顺带救回来,不是保证 150ms 左右自愈。
    fn holding_deadline_ms(&self, now_ms: u64) -> Option<u64> {
        self.is_holding_frame(now_ms)
            .then_some(self.sync_since_ms + SYNC_TIMEOUT_MS)
    }

    /// 是否应立即提交一帧:有脏数据,且不在同步块内(T2/F11)或同步块已超时。
    pub fn should_present(&self, now_ms: u64) -> bool {
        self.is_dirty() && !self.is_holding_frame(now_ms)
    }

    /// 记录已提交一帧,清除脏标记。
    ///
    /// `now_ms` 用于判定这一帧是否是靠 [`SYNC_TIMEOUT_MS`] 逃生门硬挤出来的
    /// (F155/T2 剖面)。
    pub fn mark_presented(&mut self, now_ms: u64) {
        // 出帧那一刻仍在同步块里、且已经过了 SYNC_TIMEOUT_MS —— 这一帧只可能是靠
        // 逃生门硬挤出来的(对端没把 ESU 发完)。闩住,同一个块只算一次;下一个 BSU
        // 进来才复位。判据自带 150ms 检查,所以即使某帧是被 egui 的 ui_dirty 推出来
        // 的(那时可能有 pane 正在合法攒帧),也不会误计。
        if self.in_sync
            && !self.timeout_counted
            && now_ms.saturating_sub(self.sync_since_ms) >= SYNC_TIMEOUT_MS
        {
            self.timeouts = self.timeouts.saturating_add(1);
            self.timeout_counted = true;
        }
        self.dirty = false;
    }

    /// 取走这一窗口攒下的同步块计数并清零(F155/T2 剖面)。
    ///
    /// 必须在这里清零而不是让调用方自减:计数器的语义是「这一窗口发生了几次」,
    /// 让调用方各自减一遍在多 pane 场景下极易漏减、重复上报。
    pub fn take_counts(&mut self) -> (u32, u32) {
        (
            std::mem::take(&mut self.blocks),
            std::mem::take(&mut self.timeouts),
        )
    }
}

/// 多 pane 的攒帧聚合(T2/§7.2):**任一** pane 有新数据要画(OR),**且没有任何**
/// pane 卡在未超时的同步区间内(AND)。
///
/// 这两个子条件不能像单 pane 的 [`SyncFramePacer::should_present`] 那样合并成一个
/// `all(should_present)`:`dirty` 描述的是"这个 pane 自己有没有东西要画",跨 pane
/// 语义天然是 OR —— 一个长期静止的 pane(没人在敲的 shell)`dirty` 恒为 `false`,
/// 若跟着一起被 AND 进去,活跃 pane 的新内容就永远出不来,而且不会随时间自愈
/// (静止 pane 根本没进同步区,`SYNC_TIMEOUT_MS` 逃生门救不了它)。反过来,同步
/// 门控描述的是"是不是有人正在半更新一块画面",跨 pane 语义必须是 AND —— 只要
/// 有一个 pane 还在攒帧,把其它 pane 的新内容画出来也会让这个 pane 露出半张
/// 撕裂的画面,所以哪怕这个 pane 自己不脏也必须按住整帧。
///
/// **空集合返回 `true`** —— 没有 pane 的时候(launcher 界面)整个 UI 靠 egui 画,
/// 聚合若返回 false,窗口一帧都出不来。
pub fn panes_ready_to_present<'a>(
    pacers: impl Iterator<Item = &'a SyncFramePacer>,
    now_ms: u64,
) -> bool {
    let mut saw_any_pane = false;
    let mut any_dirty = false;
    let mut none_holding = true;
    for p in pacers {
        saw_any_pane = true;
        any_dirty |= p.is_dirty();
        none_holding &= !p.is_holding_frame(now_ms);
    }
    !saw_any_pane || (any_dirty && none_holding)
}

/// 多 pane 里最早会因同步块超时而需要重新判定的时刻(ms)。`None` = 没有 pane
/// 卡在同步块里,事件循环这一轮不需要为此特意安排唤醒。
///
/// 事件循环据此在「本帧不出帧」的分支上主动排一次 `WaitUntil`(而不是无脑
/// `ControlFlow::Wait` 一直等下一个不相关事件)——错峰超时(不同 pane 各自
/// 何时进入同步块不同)靠的不是排 N 个定时器,而是每次都只盯最早到期的那个,
/// 到点重新判定后自然级联到下一个。
pub fn earliest_sync_timeout_ms<'a>(
    pacers: impl Iterator<Item = &'a SyncFramePacer>,
    now_ms: u64,
) -> Option<u64> {
    pacers.filter_map(|p| p.holding_deadline_ms(now_ms)).min()
}

/// 渲染接口(ADR-001):上层只依赖它,glyphon 只是一个实现。
///
/// 骨架不实现真实渲染——GPU 字形位置/颜色/光标形状/CJK 宽字符占两格
/// 都无法在无头容器自动验证,需人工确认。真实签名 `draw(grid, damage, surface)`
/// 随 glyphon 接入再细化。
pub trait Renderer {
    fn present(&mut self);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_update_defers_present() {
        let mut pacer = SyncFramePacer::new();
        // 进入同步块:BSU + 流式内容,期间攒住不画(否则撕裂/抖)。
        pacer.feed(BSU, 0);
        pacer.feed(b"streaming content...", 0);
        assert!(!pacer.should_present(0), "同步块内不应 present(T2/F11)");
        // 收到 ESU,提交一帧。
        pacer.feed(ESU, 0);
        assert!(pacer.should_present(0), "收到 ESU 后应提交一帧");
        pacer.mark_presented(0);
        assert!(!pacer.should_present(0), "提交后脏标记应清除");
    }

    #[test]
    fn present_immediately_without_sync_block() {
        let mut pacer = SyncFramePacer::new();
        pacer.feed(b"plain output", 0);
        assert!(pacer.should_present(0), "无同步块时正常帧应立即可提交");
    }

    #[test]
    fn esu_split_across_feeds_is_still_detected() {
        // T2:一次 feed = 一个 SSH Data 包,TCP 可以把 ESU 切在任意位置。切开后若
        // 检测不到,in_sync 永远为真 → 画面永久冻结(表现为「键盘没反应」)。
        for cut in 1..ESU.len() {
            let mut pacer = SyncFramePacer::new();
            pacer.feed(BSU, 0);
            pacer.feed(&ESU[..cut], 0);
            assert!(!pacer.should_present(0), "cut={cut}:ESU 还没喂完,不该出帧");
            pacer.feed(&ESU[cut..], 0);
            assert!(
                pacer.should_present(0),
                "cut={cut}:被切开的 ESU 未被识别,in_sync 卡死"
            );
        }
    }

    #[test]
    fn bsu_split_across_feeds_is_still_detected() {
        for cut in 1..BSU.len() {
            let mut pacer = SyncFramePacer::new();
            pacer.feed(&BSU[..cut], 0);
            pacer.feed(&BSU[cut..], 0);
            pacer.feed(b"streaming", 0);
            assert!(
                !pacer.should_present(0),
                "cut={cut}:被切开的 BSU 未被识别,攒帧失效 → 撕裂"
            );
        }
    }

    #[test]
    fn tail_only_keeps_real_prefixes() {
        // 末尾不是 BSU/ESU 前缀时不许留残渣,否则下一段会被错误拼接。
        let mut pacer = SyncFramePacer::new();
        pacer.feed(b"ends with esc-bracket \x1b[", 0);
        pacer.feed(b"?2026h", 0);
        assert!(!pacer.should_present(0), "跨段拼出的 BSU 应生效");

        let mut pacer = SyncFramePacer::new();
        pacer.feed(b"plain text", 0);
        pacer.feed(b"?2026h", 0);
        assert!(pacer.should_present(0), "不成前缀的尾巴不该拼出假 BSU");
    }

    #[test]
    fn unterminated_sync_block_times_out() {
        // 对端发了 BSU 却再没发 ESU(TUI 被 kill / 链路截断):没有超时闸就是画面
        // 永久冻结。宁可闪一下也要出帧。
        let mut pacer = SyncFramePacer::new();
        pacer.feed(BSU, 1_000);
        pacer.feed(b"half a frame", 1_000);
        assert!(!pacer.should_present(1_100), "超时之前仍应攒帧");
        assert!(
            pacer.should_present(1_000 + SYNC_TIMEOUT_MS),
            "同步块超时后必须强行出帧,否则永久冻结"
        );
    }

    #[test]
    fn sync_timeout_measures_from_block_start_not_last_byte() {
        // 流式输出会在同步块内持续喂字节;若每次 feed 都重置计时,超时永远不到。
        let mut pacer = SyncFramePacer::new();
        pacer.feed(BSU, 0);
        for t in 0..10 {
            pacer.feed(b"chunk", t * 20);
        }
        assert!(pacer.should_present(SYNC_TIMEOUT_MS), "计时须从 BSU 起算");
    }

    /// T2/§7.2:任一 pane 处在 DEC 2026 同步区间内,整帧都不 present。
    ///
    /// 只看焦点 pane 的话,后台 pane 会被画成半张更新过的画面 —— 撕裂正是
    /// 这个项目要消灭的东西,不能因为"它不是焦点"就放过去。
    #[test]
    fn any_pane_in_sync_defers_present() {
        let mut a = SyncFramePacer::new();
        let mut b = SyncFramePacer::new();
        a.feed(b"hello", 0);
        b.feed(b"\x1b[?2026h", 0); // b 进入同步区间
        assert!(a.should_present(0), "单看 a 是可以出帧的");
        assert!(
            !panes_ready_to_present([&a, &b].into_iter(), 0),
            "b 还在攒帧,整帧就该等"
        );
        b.feed(b"\x1b[?2026l", 0); // b 退出
        assert!(panes_ready_to_present([&a, &b].into_iter(), 0));
    }

    /// 空集合必须返回 true。launcher 界面(还没连上、一个 pane 都没有)靠 egui
    /// 画,聚合返回 false 的话它一帧都出不来 —— 表现为启动后白屏/黑屏。
    #[test]
    fn no_panes_is_ready_so_the_launcher_can_draw() {
        let empty: [&SyncFramePacer; 0] = [];
        assert!(panes_ready_to_present(empty.into_iter(), 0));
    }

    /// 卡死保护也要能穿透聚合:超时后即使还没收到 ESU 也必须出帧,
    /// 否则一个坏掉的远端能把整个窗口冻住。
    #[test]
    fn timeout_releases_the_aggregate_too() {
        let mut a = SyncFramePacer::new();
        a.feed(b"\x1b[?2026h", 0);
        assert!(!panes_ready_to_present([&a].into_iter(), 0));
        assert!(
            panes_ready_to_present([&a].into_iter(), SYNC_TIMEOUT_MS + 1),
            "超时后必须放行"
        );
    }

    /// bug 复现:一个 pane 有新内容(a),另一个 pane 从未收到过任何字节、长期静止
    /// (b,典型场景是普通 shell 没人在敲)。旧实现对 `should_present` 整体取 `all()`,
    /// 把"有没有新数据"也 AND 了进去 —— b.dirty 恒为 false 导致 all() 恒 false,
    /// 活跃 pane a 的新内容永远出不来,且不会随时间自愈(b 根本没进同步区,
    /// SYNC_TIMEOUT_MS 逃生门救不了)。
    #[test]
    fn idle_pane_never_dirty_must_not_starve_active_pane() {
        let mut a = SyncFramePacer::new();
        a.feed(b"new output", 0);
        let b = SyncFramePacer::new();
        assert!(
            panes_ready_to_present([&a, &b].into_iter(), 0),
            "a 有新数据、b 只是静止,应该出帧"
        );
        assert!(
            panes_ready_to_present([&a, &b].into_iter(), 10_000),
            "过多久都不该自愈成 false —— 应该从一开始就是 true"
        );
    }

    /// 两个交替活跃的 pane 不该互相饿死:这一帧 A 脏、B 不脏 → 出帧 → A 被
    /// `mark_presented` 清脏;下一帧只有 B 脏 → 仍应出帧。旧的 `all()` 语义下,
    /// 每一帧都恰好有一个 pane 不脏,永远出不了帧。
    #[test]
    fn alternating_dirty_panes_do_not_starve_each_other() {
        let mut a = SyncFramePacer::new();
        let mut b = SyncFramePacer::new();
        a.feed(b"a output", 0);
        assert!(panes_ready_to_present([&a, &b].into_iter(), 0));
        a.mark_presented(0);
        b.mark_presented(0);

        b.feed(b"b output", 1);
        assert!(
            panes_ready_to_present([&a, &b].into_iter(), 1),
            "A 已清脏、B 刚变脏,仍应出帧,不能被 A 拖死"
        );
    }

    /// 同步门控不能依赖 dirty:即使卡在同步区间的那个 pane 本身并不脏
    /// (例如 BSU 之后立刻被上一帧 present 过、`dirty` 已被 `mark_presented` 清掉,
    /// 但仍未收到 ESU),只要别的 pane 有新数据,整帧也必须继续等,否则会把
    /// 半更新的同步块画出来 —— 撕裂。
    #[test]
    fn sync_gate_holds_even_when_the_syncing_pane_is_not_dirty() {
        let mut a = SyncFramePacer::new();
        let mut b = SyncFramePacer::new();
        b.feed(BSU, 0); // b 进入同步区间(顺带标脏)
        b.mark_presented(0); // 清掉 b 的 dirty,但 in_sync 仍为 true(未超时,不计入 timeouts)
        assert!(!b.should_present(0), "b 自己没有新数据,理应不出帧");

        a.feed(b"content", 0); // a 有新数据要画
        assert!(
            !panes_ready_to_present([&a, &b].into_iter(), 0),
            "b 仍卡在未超时的同步区间,即使 b 不脏,整帧也该等"
        );
    }

    /// 所有 pane 都不脏(没有新内容)时不该出帧 —— 这是 T3「喂数据和重绘解耦」
    /// 的要求,不能退化成无条件每帧都出。
    #[test]
    fn no_dirty_panes_means_no_present() {
        let a = SyncFramePacer::new();
        let b = SyncFramePacer::new();
        assert!(
            !panes_ready_to_present([&a, &b].into_iter(), 0),
            "两个 pane 都没有新数据,不该出帧"
        );
    }

    /// 事件循环靠这个决定「本帧不出帧」时要不要主动排一次 `WaitUntil`——错峰
    /// 超时(不同 pane 各自何时进同步块不同)必须拿**最早**到期的那个,拿错
    /// (比如拿最晚的,或者干脆固定拿第一个 pane)就会让先到期的 pane 多等,
    /// 150ms 逃生门名不副实。
    #[test]
    fn earliest_sync_timeout_picks_the_soonest_deadline_across_panes() {
        let mut a = SyncFramePacer::new();
        a.feed(BSU, 0); // 进入同步块于 t=0,超时于 150
        let mut b = SyncFramePacer::new();
        b.feed(BSU, 50); // 进入同步块于 t=50,超时于 200

        assert_eq!(
            earliest_sync_timeout_ms([&a, &b].into_iter(), 60),
            Some(150),
            "两个都还没超时:必须报最早到期的 a(150),不是 b(200)或任意一个"
        );
        assert_eq!(
            earliest_sync_timeout_ms([&a, &b].into_iter(), 150),
            Some(200),
            "a 已超时(不再算「卡住」),该改口报仍在卡的 b"
        );
        assert_eq!(
            earliest_sync_timeout_ms([&a, &b].into_iter(), 300),
            None,
            "两个都超时/都不在同步块内:没有 pane 需要为此被特意唤醒"
        );

        let empty = SyncFramePacer::new();
        assert_eq!(
            earliest_sync_timeout_ms(std::iter::once(&empty), 0),
            None,
            "从没进过同步块的 pane 不该产生一个假的唤醒时刻"
        );
    }

    /// F155/T2 剖面:进入一次同步块只算一个块,哪怕 BSU 在块内(比如流式输出
    /// 时对端重复发)重复出现——`in_sync` 已经是 true 时不该重新计数。
    #[test]
    fn re_entering_bsu_inside_a_block_counts_once() {
        let mut pacer = SyncFramePacer::new();
        pacer.feed(BSU, 0);
        pacer.feed(BSU, 10); // 同一个块内又来一次 BSU(不应发生,但要防御)
        pacer.feed(BSU, 20);
        assert_eq!(pacer.take_counts(), (1, 0), "块内重复 BSU 不该多算");
    }

    /// F155/T2 剖面:ESU 正常收口的块不算超时——那是本项目的正常路径,
    /// 不能让每一次流式输出都在剖面里报一次「超时」。
    #[test]
    fn a_block_closed_by_esu_is_not_a_timeout() {
        let mut pacer = SyncFramePacer::new();
        pacer.feed(BSU, 0);
        pacer.feed(b"streaming", 10);
        pacer.feed(ESU, 20); // 远早于 SYNC_TIMEOUT_MS
        pacer.mark_presented(20);
        assert_eq!(pacer.take_counts(), (1, 0), "正常收口的块不该计进 timeouts");
    }

    /// F155/T2 剖面:对端不发 ESU、过了 150ms 才靠逃生门出帧的块算一次超时,
    /// 且同一个块哪怕连续好几帧都卡在超时状态,也只计一次——不然剖面会把
    /// 一次真实故障夸大成几十次。
    #[test]
    fn an_unterminated_block_counts_one_timeout_even_across_repeated_frames() {
        let mut pacer = SyncFramePacer::new();
        pacer.feed(BSU, 0);
        pacer.feed(b"half a frame", 0);
        // 逃生门出帧的那一帧:should_present 应为 true(见 unterminated_sync_block_times_out)。
        assert!(pacer.should_present(SYNC_TIMEOUT_MS));
        pacer.mark_presented(SYNC_TIMEOUT_MS);
        // ESU 仍未到,后续几帧继续卡在同一个(未超时判定意义上已放行、但
        // in_sync 仍为 true 的)块里,每帧都会再次 mark_presented。
        pacer.mark_presented(SYNC_TIMEOUT_MS + 16);
        pacer.mark_presented(SYNC_TIMEOUT_MS + 32);
        assert_eq!(
            pacer.take_counts(),
            (1, 1),
            "同一个未收口的块连续多帧只该计一次超时"
        );
    }

    /// F155/T2 剖面:`take_counts` 取走后必须归零,否则下一个 5 秒窗口会
    /// 把上一窗口的计数重新报一遍——同一次故障在日志里出现两次。
    #[test]
    fn take_counts_drains_so_the_next_window_does_not_re_report() {
        let mut pacer = SyncFramePacer::new();
        pacer.feed(BSU, 0);
        pacer.feed(b"stuck", 0);
        pacer.mark_presented(SYNC_TIMEOUT_MS);
        assert_eq!(pacer.take_counts(), (1, 1));
        assert_eq!(
            pacer.take_counts(),
            (0, 0),
            "取走之后应归零,不能被下一个窗口重复上报"
        );
    }
}
