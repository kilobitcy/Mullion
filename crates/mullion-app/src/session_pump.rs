//! 字节泵纯件:app 每帧把远端字节喂进仿真器,并取回需回写对端的字节(T1)。
//! 不碰网络/GPU,可无窗口单测。真实事件循环在 main.rs 接线(未验证,需人工确认)。

use mullion_term::emulator::Emulator;

/// 喂入若干段远端字节,推进仿真器,返回需回写 SSH channel 的出站字节(T1)。
/// app 每帧:先 drain SSH 接收端得到 `inbound`,调 `pump`,再把返回值交 `SshSession::write`。
pub fn pump(emu: &mut Emulator, inbound: &[Vec<u8>]) -> Vec<u8> {
    // F155:吞吐 + 回显往返。这是 `pump` 在生产代码里唯一的调用点
    // (`Workspace::pump`),且调用方已经判过 `inbound` 非空才会走到这里,
    // 所以挂在这里既不重复计数也不漏路径。
    crate::diag::count_inbound(inbound.iter().map(Vec::len).sum());
    crate::diag::note_inbound_for_echo();
    for chunk in inbound {
        emu.feed(chunk);
    }
    emu.take_pty_writes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pump_feeds_and_collects_pty_writes() {
        // T1:喂入光标位置查询(DSR 6),pump 必须回收 CPR 应答 —— 这些字节要回写 SSH channel。
        // 漏了 → 同步输出探测无应答 → 全屏 TUI 闪。
        let mut emu = Emulator::new(80, 24);
        let out = pump(&mut emu, &[b"\x1b[6n".to_vec()]);
        assert_eq!(out, b"\x1b[1;1R", "pump 未回收 PtyWrite(T1)");
    }

    #[test]
    fn pump_without_query_yields_nothing() {
        let mut emu = Emulator::new(80, 24);
        let out = pump(&mut emu, &[b"hello".to_vec()]);
        assert!(out.is_empty(), "普通输出不应产生回写");
    }
}
