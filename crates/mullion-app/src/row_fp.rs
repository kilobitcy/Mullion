//! F174:位置寻址的行指纹台账。**只做诊断,不参与任何渲染判据。**
//!
//! # 为什么要单独一张表
//!
//! F12 的整形缓存自 F174 起改成**内容寻址**(`shaped_cache::ShapeKey`),它回答
//! 的是「这份内容整形过吗」。而 `seg=`(见 [`crate::bands::segments`])问的是
//! 另一件事:「屏幕上**哪些行**的内容变了、变化散不散」——那个数字的用途是
//! 判断 [`crate::bands::BAND_ROWS`] 选得对不对。
//!
//! 两者在滚动场景下会分道扬镳:往下滚一行,整形缓存只 miss 新露出的那一行
//! (上面每一行的内容上一帧都整形过,只是挪了行号),可屏幕上**每一行显示的
//! 内容都换了**。若拿 miss 集合当「变了的行」,`seg=` 会报出接近 1 的段数,
//! 而实际整屏在动 —— **编译、测试、画面全静默,只有这个数字在骗人**。
//!
//! 所以职责切开:内容寻址的载荷表答「整形过吗」,这张位置寻址的指纹表答
//! 「这一行变了吗」。代价是每行一个 `u64`。
//!
//! 零 GPU、零 glyphon、纯逻辑,可无头单测。

use mullion_core::layout::PaneId;
use std::collections::HashMap;

struct Entry {
    hash: u64,
    /// 最后一次被记账的帧序号。逐出判据,见 [`RowFingerprints::end_frame`]。
    last_seen: u64,
}

/// `(PaneId, 行号) → 上一帧的行指纹`。
pub struct RowFingerprints {
    rows: HashMap<(PaneId, u16), Entry>,
    /// 帧序号。用它而不是每帧新建一个访问集,是因为后者每帧都要在帧路径上
    /// 分配(陷阱 T3)。与 [`crate::shaped_cache::ShapedCache`] 同款。
    frame: u64,
}

impl RowFingerprints {
    pub fn new() -> Self {
        Self {
            rows: HashMap::new(),
            frame: 0,
        }
    }

    /// 一帧开始。之后所有 [`note`](Self::note) 都记在这一帧名下。
    pub fn begin_frame(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    /// 记下这一行本帧的指纹,返回「跟上一帧比,变了吗」。
    ///
    /// **首次见到算「变了」**(新开的 pane、行数涨了、上一帧被逐出后又回来)。
    /// 这个方向与 F12 的整体取向一致:指纹类判据的最坏情况必须是**多报一次**
    /// 变化,不能是漏报 —— 漏报的症状是诊断数字看起来很漂亮而实际在退化。
    pub fn note(&mut self, key: (PaneId, u16), hash: u64) -> bool {
        let f = self.frame;
        match self.rows.get_mut(&key) {
            Some(e) => {
                let changed = e.hash != hash;
                e.hash = hash;
                e.last_seen = f;
                changed
            }
            None => {
                self.rows.insert(key, Entry { hash, last_seen: f });
                true
            }
        }
    }

    /// 一帧结束:本帧没记过账的键全删。
    ///
    /// 与 [`crate::shaped_cache::ShapedCache::end_frame`] 同一条判据,**统一
    /// 覆盖** pane 关闭、行数缩小、切标签。刻意不在 `close_pane` 之类的地方
    /// 各加清理 hook —— 那种列举式门控在加档时必然漏。
    pub fn end_frame(&mut self) {
        let f = self.frame;
        self.rows.retain(|_, e| e.last_seen == f);
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

impl Default for RowFingerprints {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P0: PaneId = PaneId(0);
    const P1: PaneId = PaneId(1);

    /// 头一回见到这一行 → 算「变了」。
    ///
    /// 自证会变红:把 `note` 的 `None` 分支改成返回 `false`。
    #[test]
    fn a_row_seen_for_the_first_time_counts_as_changed() {
        let mut t = RowFingerprints::new();
        t.begin_frame();
        assert!(t.note((P0, 0), 7));
    }

    /// 指纹没变 → 不算变。
    ///
    /// 自证会变红:让 `note` 无条件返回 `true` —— `seg=` 会恒等于「整屏在动」,
    /// 带差分的调参依据当场作废。
    #[test]
    fn an_unchanged_row_does_not_count_as_changed() {
        let mut t = RowFingerprints::new();
        t.begin_frame();
        t.note((P0, 0), 7);
        t.end_frame();

        t.begin_frame();
        assert!(!t.note((P0, 0), 7));
    }

    /// 指纹变了 → 算变了。
    ///
    /// 自证会变红:把 `note` 里的 `e.hash != hash` 改成 `false`。
    #[test]
    fn a_changed_hash_counts_as_changed() {
        let mut t = RowFingerprints::new();
        t.begin_frame();
        t.note((P0, 0), 7);
        t.end_frame();

        t.begin_frame();
        assert!(t.note((P0, 0), 8));
    }

    /// **本模块存在的全部理由**:滚动一行时,每一行都算变了。
    ///
    /// 整形缓存(内容寻址)在这一帧只会 miss 一行 —— 上面那些内容上一帧都
    /// 整形过,只是挪了行号。若 `seg=` 拿 miss 集合当「变了的行」,这里会报
    /// 「只有 1 行变了、局部性极好」,而屏幕上整整 N 行全换了内容。
    ///
    /// 自证会变红:把 `note` 的返回值改成「查内容有没有见过」的语义
    /// (例如用一张 `hash → ()` 的全局集合去重)。
    #[test]
    fn scrolling_one_line_marks_every_row_changed() {
        let mut t = RowFingerprints::new();
        const N: u16 = 8;

        // 第一帧:行 r 放内容 r。
        t.begin_frame();
        for r in 0..N {
            t.note((P0, r), u64::from(r));
        }
        t.end_frame();

        // 第二帧:整体上移一行(行 r 现在放内容 r+1)。
        t.begin_frame();
        let changed = (0..N)
            .filter(|&r| t.note((P0, r), u64::from(r) + 1))
            .count();
        t.end_frame();

        assert_eq!(
            changed,
            usize::from(N),
            "滚动一行后只有 {changed}/{N} 行被判为变了 —— `seg=` 会报出\
             「局部性极好」而实际整屏在动,带差分的调参依据静默失真"
        );
    }

    /// 本帧没记过账的键在帧末被逐出。这一条同时覆盖 pane 关闭、行数缩小、
    /// 切标签。
    ///
    /// 自证会变红:把 `end_frame` 的 `retain` 谓词改成恒 `true`。
    #[test]
    fn rows_not_noted_this_frame_are_evicted() {
        let mut t = RowFingerprints::new();

        t.begin_frame();
        t.note((P0, 0), 1);
        t.note((P1, 0), 2);
        t.end_frame();
        assert_eq!(t.len(), 2);

        // 第二帧只记 P0 —— P1 那块 pane 被关掉了。
        t.begin_frame();
        t.note((P0, 0), 1);
        t.end_frame();
        assert_eq!(t.len(), 1, "没记过账的 pane 该被逐出");
    }

    /// 被逐出之后又回来的行,算「变了」——哪怕指纹跟被逐出前一样。
    ///
    /// 这是「首次见到算变了」的直接推论,单列一条是因为它才是实际会发生的
    /// 场景(切走标签页再切回来)。保守方向:多报一次,不漏报。
    #[test]
    fn a_row_that_came_back_after_eviction_counts_as_changed() {
        let mut t = RowFingerprints::new();

        t.begin_frame();
        t.note((P0, 0), 7);
        t.end_frame();

        // 切走:这一帧一行都没记。
        t.begin_frame();
        t.end_frame();
        assert!(t.is_empty());

        // 切回来,内容一模一样。
        t.begin_frame();
        assert!(t.note((P0, 0), 7));
    }

    /// 台账按 `PaneId` 这种稳定身份分槽,不按当帧下标。
    ///
    /// 关掉中间一块 pane 会让其后所有 pane 的当帧下标挪位 —— 拿下标当键的话
    /// A 的指纹会被当成 B 的用,`seg=` 在分屏变动那一帧彻底乱掉。
    ///
    /// 自证会变红:把键类型从 `(PaneId, u16)` 换成 `(usize, u16)` 并按下标存取。
    #[test]
    fn the_ledger_is_keyed_by_pane_id_not_by_frame_index() {
        let mut t = RowFingerprints::new();
        let p2 = PaneId(2);

        // 第一帧:三块 pane,当帧下标 0/1/2。
        t.begin_frame();
        t.note((P0, 0), 10);
        t.note((P1, 0), 11);
        t.note((p2, 0), 12);
        t.end_frame();

        // 第二帧:中间那块(P1)关了,P2 的当帧下标从 2 挪到了 1。
        t.begin_frame();
        assert!(!t.note((P0, 0), 10), "P0 的指纹没变,不该判为变了");
        assert!(!t.note((p2, 0), 12), "P2 拿到了别人(P1)的指纹");
        t.end_frame();
        assert_eq!(t.len(), 2, "关掉的 pane 该被逐出");
    }
}
