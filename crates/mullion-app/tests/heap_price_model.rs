//! F192 的两项定价模型标定,**独占一个测试进程**。
//!
//! # 为什么这条不能待在 `src/text.rs` 的 `#[cfg(test)]` 里
//!
//! 它量的是「一个已整形 `Buffer` 实际吃掉多少堆」,唯一量得到的尺子是进程
//! 全局的 `heapgauge::GLOBAL`(F190 那套私有计数器对分配器不可见)。而
//! `cargo test --lib` 把 2400+ 条用例并发跑在同一个进程里 —— 读数窗口里
//! 邻居的分配与释放同样记在这把尺子上。
//!
//! 原先的对策是「三轮取中位数 + 只断言量级(4 倍带)」,**不够**:邻居在窗口
//! 里**净释放**时差值为负,`u64::saturating_sub` 把它静默压成 `0`,而 `0` 是
//! 个合法数值,照样参与排序、照样能当上中位数。2026-09-23 实测 5 轮复现 1 次,
//! 打出来的三轮是 `[0, 0, 2825]` —— 两轮被压成了 0。加宽带宽治不了这个:
//! 0 落在任何以预测值为中心的区间之外。
//!
//! 搬进独立的集成测试二进制就没有邻居了:`cargo test` 逐个运行各测试 target,
//! 而这个 target 里只有下面这一条用例。断言一条没放宽,反而**收紧**了 ——
//! 净减少不再被压成 0,而是当场炸出来(独占进程里那只可能是尺子坏了)。
//!
//! 相关:`sysprobe` 里那条 CPU 用例是同一族毛病的另一种形态(判据假设本线程
//! 能独占半个核),两条一起修。

use glyphon::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping};
use mullion_app::heapgauge;
use mullion_app::text::{BUFFER_FIXED_BYTES, DEFAULT_FONT_FAMILY, GLYPH_EST_BYTES};

/// F192:两项定价模型的实测标定。**在长短两端各量一次。**
///
/// 为什么必须两端都量:一开始只量了单字 run(2371 字节),照它标一个单常数
/// 看着挺准 —— 直到把 run 拉长才发现 200 格的 ASCII 行值 55920 字节。
/// 24 倍跨度,单常数在另一端必错一个数量级。两项模型是从这四个实测点
/// (1→2371、20→7580、60→16900、200→55920)最小二乘拟出来的。
///
/// 两端合起来才钉得住两个常数:短端主要约束 `BUFFER_FIXED_BYTES`,
/// 长端主要约束 `GLYPH_EST_BYTES`。**少任何一端,另一个常数就自由了。**
///
/// 三轮取中位数仍然保留:第一轮含 `FontSystem` 的一次性增长(字体数据、
/// shape cache),是必然的离群值,中位数把它削掉。**多轮的理由是预热,
/// 不是抗噪** —— 抗噪已经由「独占进程」解决(见模块文档)。
///
/// 断言只钉量级(`[预测/4, 预测×4]`):平台漂是明知的(Linux 开发机的回退
/// 字体与 Windows 不同),常数漂出一个量级时逼人回来重标,日常波动不红。
///
/// 自证会变红:把 `GLYPH_EST_BYTES` 改成 1(长端立刻红,短端仍绿 ——
/// 这正是「少一端就钉不住」的现场)。
#[test]
fn the_shaped_buffer_price_model_matches_what_it_actually_costs() {
    // 尺子本身先自检:`#[global_allocator]` 挂在 lib.rs 上,如果它没被链进
    // 这个测试二进制,下面每一轮的差值都会是 0 —— 那和「Buffer 一个字节都
    // 不占」长得一模一样。先把这两种情况分开。
    assert!(
        heapgauge::live_bytes() > 0,
        "堆账为 0:CountingAlloc 没有链进这个测试二进制,下面量到的都是假数"
    );

    // (一个 run 里的字形数, 持有多少个 run)。乘积按总量 ~25MB 选。
    const POINTS: [(usize, usize); 2] = [(1, 10_000), (200, 500)];
    let mut fs = FontSystem::new();
    let metrics = Metrics::new(16.0, 20.0);

    for (glyphs, n) in POINTS {
        let text = "x".repeat(glyphs);
        let mut rounds = [0usize; 3];
        for slot in &mut rounds {
            let before = heapgauge::GLOBAL.live();
            // `held` 必须活到第二次读数之后,否则量到的是 0。
            let mut held: Vec<Buffer> = Vec::with_capacity(n);
            for _ in 0..n {
                let mut b = Buffer::new(&mut fs, metrics);
                b.set_text(
                    &mut fs,
                    &text,
                    Attrs::new().family(Family::Name(DEFAULT_FONT_FAMILY)),
                    Shaping::Advanced,
                );
                b.shape_until_scroll(&mut fs, false);
                held.push(b);
            }
            let after = heapgauge::GLOBAL.live();
            // **不用 `saturating_sub`。** 它会把「账净减少了」压成一个合法的
            // `0`,而 0 混进中位数就是原来那条 flaky 的全部病因。独占进程里
            // 账不该减少,真减少了说明尺子坏了 —— 当场说清楚,别悄悄记 0。
            let delta = after.checked_sub(before).unwrap_or_else(|| {
                panic!("堆账在这一轮里净减少了({before} → {after})——本用例独占进程,不该有人在这个窗口里释放")
            });
            *slot = delta as usize / n;
            drop(held);
        }
        rounds.sort_unstable();
        let measured = rounds[1];
        let predicted = BUFFER_FIXED_BYTES + glyphs * GLYPH_EST_BYTES;
        println!("{glyphs} 字形/run:实测中位数 {measured} 字节,模型 {predicted}(三轮 {rounds:?})");
        assert!(
            measured >= predicted / 4 && measured <= predicted * 4,
            "{glyphs} 字形的 run:模型报 {predicted}、实测 {measured},差了一个量级\
             以上,该重新标定 BUFFER_FIXED_BYTES/GLYPH_EST_BYTES 了(三轮 {rounds:?})"
        );
    }
}
