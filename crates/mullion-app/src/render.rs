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

    /// 是否应立即提交一帧:有脏数据,且不在同步块内(T2/F11)或同步块已超时。
    pub fn should_present(&self, now_ms: u64) -> bool {
        self.dirty
            && (!self.in_sync || now_ms.saturating_sub(self.sync_since_ms) >= SYNC_TIMEOUT_MS)
    }

    /// 记录已提交一帧,清除脏标记。
    pub fn mark_presented(&mut self) {
        self.dirty = false;
    }
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
        pacer.mark_presented();
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
}
