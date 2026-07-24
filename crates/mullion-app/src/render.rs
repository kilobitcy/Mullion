//! 渲染侧:攒帧状态机(T2)+ 渲染接口占位(ADR-001)。
//!
//! 攒帧(T2/F11):同步输出(DEC 2026)在 BSU(`CSI ? 2026 h`)与 ESU(`CSI ? 2026 l`)
//! 之间攒住不画,收到 ESU 才提交一帧。这是消灭闪烁的根治手段。

const BSU: &[u8] = b"\x1b[?2026h";
const ESU: &[u8] = b"\x1b[?2026l";

/// 攒帧状态机:跟踪是否处于同步块,以及是否有待提交的脏数据。
#[derive(Debug, Default)]
pub struct SyncFramePacer {
    in_sync: bool,
    dirty: bool,
}

impl SyncFramePacer {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂入一段来自对端的字节:标脏,并探测 BSU/ESU 切换攒帧状态。
    ///
    /// 骨架简化:直接扫描精确的 DEC 2026 序列。真实实现应从 VT 解析器的
    /// 同步状态取(更鲁棒:能处理跨 feed 边界、参数变体)。
    pub fn feed(&mut self, bytes: &[u8]) {
        if !bytes.is_empty() {
            self.dirty = true;
        }
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i..].starts_with(BSU) {
                self.in_sync = true;
                i += BSU.len();
            } else if bytes[i..].starts_with(ESU) {
                self.in_sync = false;
                i += ESU.len();
            } else {
                i += 1;
            }
        }
    }

    /// 是否应立即提交一帧:有脏数据且不在同步块内(T2/F11)。
    pub fn should_present(&self) -> bool {
        self.dirty && !self.in_sync
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
        pacer.feed(BSU);
        pacer.feed(b"streaming content...");
        assert!(!pacer.should_present(), "同步块内不应 present(T2/F11)");
        // 收到 ESU,提交一帧。
        pacer.feed(ESU);
        assert!(pacer.should_present(), "收到 ESU 后应提交一帧");
        pacer.mark_presented();
        assert!(!pacer.should_present(), "提交后脏标记应清除");
    }

    #[test]
    fn present_immediately_without_sync_block() {
        let mut pacer = SyncFramePacer::new();
        pacer.feed(b"plain output");
        assert!(pacer.should_present(), "无同步块时正常帧应立即可提交");
    }
}
