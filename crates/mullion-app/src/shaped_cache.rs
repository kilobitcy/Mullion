//! F12 差分整形:跨帧缓存"已经 shape 好的一行",靠行指纹判脏。
//!
//! **零 GPU、零 glyphon**:载荷是泛型 `T`。生产时 `T = glyphon::Buffer`,
//! 测试时 `T = ()`。泛型不是过度抽象,是**可测性的前提** —— `Buffer` 必须
//! 有一个 `FontSystem` 才能构造,不泛型就没有任何一条断言能在无头机器上跑。
//!
//! # F174:键从位置寻址改成内容寻址
//!
//! F12 落地时键是 `(PaneId, row)`,命中判据是 `hash && term_w`。终审已经挖出
//! 「键实为内容寻址,`PaneId` 只是分桶」这层——**但查表方式没跟上**:整形结果
//! 只取决于(内容, term_w, 字体),而查表却按行号去查。滚一行,每一行的内容
//! 都挪了行号,查到的是那个行号上的旧条目 → 指纹不等 → 整块 pane 全部重整形,
//! 尽管 `Buffer` 一个都没真变。实机剖面上是 `reshape=hit:5970/miss:1254`、
//! `text_prepare p95=16.4ms/max=65.5ms`(滚轮档)。
//!
//! F174 把键换成 [`ShapeKey`](内容 + `term_w`),查表方式与命中判据从此同源:
//!
//! - 滚 k 行只 miss 新露出的那 k 行。
//! - 同内容的行(**空行是空闲画面的大头**)跨 pane 自动合并成一条 —— 同一个
//!   `Buffer` 挂在多个 `TextArea` 上是 glyphon 支持的用法:`prepare` 里
//!   `text_area.buffer.layout_runs()` 只读,位置由 `glyph.physical((left, top))`
//!   在取字形时才施加。
//!
//! 代价:「哪些行的内容变了」这个**诊断**问题从此问不到这张表(滚动时它只
//! 知道「这份内容没见过」),搬去了 [`crate::row_fp`]。

use std::collections::HashMap;

/// 整形缓存的键:**内容寻址**。
///
/// 整形结果只取决于(内容, `term_w`, 字体)。字体不进键 —— 换字体族/字号/DPI
/// 走 `TextLayer::set_font` 里那一次显式 `clear`,是这张表**唯一的**显式失效
/// hook(见该函数文档)。内容与 `term_w` 则由这个键自动覆盖。
///
/// `PaneId` 刻意**不**在键里:同一份内容在哪块 pane 上整形出来都一样,分桶
/// 只会白白拆散命中。位置相关的量(`left`/`top`)是建 `TextArea` 时才施加的。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShapeKey {
    /// 行内容指纹(`mullion_term::snapshot::hash_row`)。覆盖字符、前景色、
    /// 背景色、宽度、spacer、选中态全六个字段。
    pub hash: u64,
    /// 这个 pane 的终端区像素宽。它进的是 `Buffer::set_size` 的 `avail`,
    /// **快照里没有这个量**,指纹覆盖不到,必须自己占一格。
    pub term_w: u32,
}

/// 缓存里的一段已整形 run。`col` 是它在这一行里的起始列(渲染时
/// `left = term_px.x + col × cell_w`,与 `gpu::quads_for` 同一个式子)。
pub struct CachedRun<T> {
    pub col: u16,
    pub payload: T,
    /// F192:这个载荷里排了多少个字形。**只喂内存记账**,渲染一路不看它。
    ///
    /// 存下来而不是用时现算,是因为唯一算得准的地方是整形完那一刻
    /// (`Buffer::layout_runs`),而记账发生在几秒后的 gauge 采样里,那时
    /// 再遍历上千个 `Buffer` 就跑到帧路径上去了(T3)。
    pub glyphs: u32,
}

/// 缓存里的一行。
///
/// F174 起 `hash`/`term_w` 不再作为字段存在这里 —— 它们**就是键**
/// ([`ShapeKey`])。存成两份会漂移,而漂移的症状是画面陈旧且完全静默。
pub struct CachedRow<T> {
    /// 最后一次被访问的帧序号。逐出判据,见 [`ShapedCache::end_frame`]。
    last_seen: u64,
    pub runs: Vec<CachedRun<T>>,
}

/// 这一行这一帧该怎么办。
///
/// 三态而不是 `bool`:`Temporary` 与 `Reshape` 的**整形动作完全相同**,
/// 但缓存副作用正好相反(一个必须写回、一个绝对不能写回)。用 `bool`
/// 表达不了这个差别,用枚举则调用方必须穷尽 `match`,日后加档时编译器
/// 会拦住漏掉的分支。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowPlan {
    /// 复用缓存,跳过整形。**收益的全部来源。**
    Reuse,
    /// 重新整形并写回缓存。
    Reshape,
    /// 整形一份临时的,**不查也不写缓存**。只有 IME 组字行走这一档。
    Temporary,
}

/// 这一行这一帧该怎么办。**纯函数**,三条分支各有一条测试。
///
/// F174 起「内容变了」「pane 宽度变了」两档不在这里判 —— 它们**就是键的组成
/// 部分**([`ShapeKey`]),查得到就说明两者都对得上。留着这个函数是为了组字
/// 那一档:调用方必须穷尽 `match RowPlan`,日后加档时编译器会拦住漏掉的分支,
/// 写成 `Option::is_some()` 就没有这层保护了。
///
/// # 为什么组字行必须绕开缓存
///
/// 组字期间正文行带着"给拼音让路"的空洞(`text::hidden_span_for_row`)。
/// 若把这份结果写回缓存,用户按 Esc 取消组字后 cells 没变、指纹相同 →
/// 下一帧命中 → 复用**带空洞的** buffer → 被拼音盖住的那几个字永久消失。
/// 这正是本设计要根除的"静默陈旧"。
///
/// **F174 之后这一档更要紧**:内容寻址会让别的 pane 里内容相同的行命中同一
/// 条目,组字产物一旦写进去,污染面从「这一行」扩大到「全窗口所有同内容的行」。
///
/// 组字行的旧条目会因为本帧没被访问而在帧末逐出,组字结束后的第一帧
/// 按"无条目"miss 一次即恢复。
pub fn plan_row<T>(cached: Option<&CachedRow<T>>, is_preedit_row: bool) -> RowPlan {
    if is_preedit_row {
        return RowPlan::Temporary;
    }
    match cached {
        Some(_) => RowPlan::Reuse,
        None => RowPlan::Reshape,
    }
}

/// 按 [`ShapeKey`](内容 + `term_w`)分槽的跨帧整形缓存。
///
/// **键是内容不是位置**,理由见模块文档。行号与 `PaneId` 都不在键里:整形
/// 结果与它出自哪块 pane 的第几行无关,位置是建 `TextArea` 时才施加的。
pub struct ShapedCache<T> {
    rows: HashMap<ShapeKey, CachedRow<T>>,
    /// 帧序号。用它而不是"每帧新建一个 `HashSet` 记访问集",是因为后者
    /// 每帧都要在帧路径上分配(陷阱 T3)。
    frame: u64,
}

impl<T> ShapedCache<T> {
    pub fn new() -> Self {
        Self {
            rows: HashMap::new(),
            frame: 0,
        }
    }

    /// 一帧开始。之后所有 `touch` / `insert` 都记在这一帧名下。
    pub fn begin_frame(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    pub fn get(&self, key: ShapeKey) -> Option<&CachedRow<T>> {
        self.rows.get(&key)
    }

    /// 命中路径:标记这份内容本帧用过,免得被帧末逐出。
    pub fn touch(&mut self, key: ShapeKey) {
        let f = self.frame;
        if let Some(r) = self.rows.get_mut(&key) {
            r.last_seen = f;
        }
    }

    /// 未命中路径:写入刚整形好的一行。
    ///
    /// **`runs` 为空也要写。** `row_to_runs` 会把整行空白直接丢掉,空行的
    /// 整形产物就是空集;不写条目的话空行永远 miss,而空行恰是空闲画面的
    /// 大头 —— 差分就白做了。F174 内容寻址之后更是如此:全窗口所有空行
    /// 共用这**一条**条目。
    pub fn insert(&mut self, key: ShapeKey, runs: Vec<CachedRun<T>>) {
        let last_seen = self.frame;
        self.rows.insert(key, CachedRow { last_seen, runs });
    }

    /// 一帧结束:本帧没访问过的键全删,载荷推进 `recycle`。
    ///
    /// 这一条**统一覆盖** pane 关闭、行数缩小、切标签、滚出视野。刻意不在
    /// `close_pane` 之类的地方各加一处清理 hook —— 那种列举式门控在
    /// 加档时必然漏,本项目已经踩中过三次。
    ///
    /// **载荷必须回 `recycle`,不能直接丢。** F174 之后这里是**唯一**的回收
    /// 点(内容寻址下 `Reshape` 只在「这份内容没见过」时发生,不存在同键旧
    /// 载荷可摘,原先那个 `recycle_row` 因此成了空操作,已删)。丢掉的话滚动
    /// 时会每帧新建上千个 `glyphon::Buffer` —— 那是陷阱 T3,且**比不做差分
    /// 还慢**。稳态是平的:第 N 帧滚出去的行在帧末回池,第 N+1 帧新露出的行
    /// 从池里取(滚动起步那一帧会多分配一次,之后收敛)。
    /// F192:回收出去的是 `(载荷, 字形数)`。字形数必须一起走 —— 池子里躺的
    /// 多半是长行退下来的 buffer,按固定价计会低报一个数量级(见
    /// `text::bytes_estimate_of` 的两项模型)。
    pub fn end_frame(&mut self, recycle: &mut Vec<(T, u32)>) {
        let f = self.frame;
        self.rows.retain(|_, r| {
            if r.last_seen == f {
                return true;
            }
            recycle.extend(r.runs.drain(..).map(|x| (x.payload, x.glyphs)));
            false
        });
    }

    /// 全清(换字体族/字号/DPI)。载荷同样回收。
    pub fn clear(&mut self, recycle: &mut Vec<(T, u32)>) {
        for (_, mut r) in self.rows.drain() {
            recycle.extend(r.runs.drain(..).map(|x| (x.payload, x.glyphs)));
        }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// F192:缓存里**载荷**的个数(各行 `runs` 之和),不是行数。
    ///
    /// **记账与容量决策一律用这个,不要用 [`len`](Self::len)。** 一行装的是
    /// `Vec<CachedRun<T>>`,而 `text::row_to_runs` 会把每个非 ASCII 字符单独
    /// 切成一个 run —— 满屏框线的 TUI 下一行 120 列就是 120 个载荷,拿行数
    /// 当口径低报一个数量级(F192 修的正是这个)。
    ///
    /// 两处用它:`TextLayer::bytes_estimate` 的记账,和 F196 的 `pool` cap。
    /// **一处实现两处用**是有意的 —— 各写一遍的话,改了一个忘另一个,
    /// 而两者不一致没有任何东西会报错。
    pub fn payload_count(&self) -> usize {
        self.rows.values().map(|r| r.runs.len()).sum()
    }

    /// F192:缓存里**字形**的总数。与 [`payload_count`](Self::payload_count)
    /// 是两个维度,记账两项都要(`buffers × 固定价 + glyphs × 边际价`)。
    ///
    /// 一个中文字的 run 值 2.4KB、一个 200 格的 ASCII 行值 56KB —— 24 倍的
    /// 跨度,任何单常数模型在其中一端必错一个数量级。
    pub fn glyph_count(&self) -> usize {
        self.rows
            .values()
            .map(|r| r.runs.iter().map(|x| x.glyphs as usize).sum::<usize>())
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

impl<T> Default for ShapedCache<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(hash: u64, term_w: u32) -> ShapeKey {
        ShapeKey { hash, term_w }
    }

    fn run() -> CachedRun<()> {
        run_of(1)
    }

    fn run_of(glyphs: u32) -> CachedRun<()> {
        CachedRun {
            col: 0,
            payload: (),
            glyphs,
        }
    }

    /// 首帧:没有条目 → 整形。
    ///
    /// 自证会变红:把 `plan_row` 的 `None` 分支改成 `RowPlan::Reuse`。
    #[test]
    fn a_row_with_no_entry_is_reshaped() {
        assert_eq!(plan_row(None::<&CachedRow<()>>, false), RowPlan::Reshape);
    }

    /// 查得到 → 复用。这是整个改动的收益来源。
    ///
    /// 自证会变红:让 `plan_row` 无条件返回 `RowPlan::Reshape` —— 画面依旧
    /// 全对,性能悄悄回到改之前。这正是运行期 `reshape=hit:/miss:` 计数器
    /// 存在的理由。
    #[test]
    fn an_entry_that_is_found_is_reused() {
        let c = CachedRow::<()> {
            last_seen: 0,
            runs: Vec::new(),
        };
        assert_eq!(plan_row(Some(&c), false), RowPlan::Reuse);
    }

    /// 组字行走临时槽 —— **哪怕缓存里有一条完全匹配的条目**。
    ///
    /// 这条是防"看起来更聪明"的写法:若组字行也允许命中缓存,用户按 Esc
    /// 取消组字后 cells 没变、指纹相同,会复用**带拼音空洞的**那份 buffer,
    /// 被盖住的几个字永久消失。F174 内容寻址之后污染面更大:全窗口所有
    /// 同内容的行都会跟着一起缺字。
    ///
    /// 自证会变红:把 `plan_row` 里 `is_preedit_row` 那条提前返回删掉。
    #[test]
    fn a_preedit_row_is_temporary_even_when_the_cache_matches() {
        let c = CachedRow::<()> {
            last_seen: 0,
            runs: Vec::new(),
        };
        assert_eq!(plan_row(Some(&c), true), RowPlan::Temporary);
    }

    /// F192:`payload_count` 数的是**载荷个数**,不是行数。
    ///
    /// 这两个数在本项目里差一个数量级:`row_to_runs` 把每个非 ASCII 字符
    /// 单独切成一个 run,满屏框线的 TUI 下一行 120 列就是 120 个载荷。
    /// `len()`(行数)当记账口径用会低报同一个数量级 —— 那正是 F192 要修的病。
    ///
    /// 自证会变红:让 `payload_count` 返回 `self.rows.len()`。
    #[test]
    fn payload_count_counts_runs_not_rows() {
        let mut c = ShapedCache::<()>::new();
        c.begin_frame();
        c.insert(k(1, 80), vec![run(), run(), run()]);
        c.insert(k(2, 80), vec![run(), run()]);
        assert_eq!(c.len(), 2, "行数");
        assert_eq!(c.payload_count(), 5, "载荷数 = 各行 runs 之和");
    }

    /// F192:`glyph_count` 数的是**字形**,与载荷个数是两个维度。
    ///
    /// 实测(`text.rs` 的校准测试)一个整形完的 `Buffer` 值
    /// `1770 + 269 × 字形数` 字节 —— 一个中文字的 run 2.4KB,一个 200 格的
    /// ASCII 行 56KB,差 24 倍。**只数 buffer 个数的记账在这两种画面上会各错
    /// 一个方向**,所以两个维度都得报。
    ///
    /// 自证会变红:让 `glyph_count` 返回 `payload_count()`。
    #[test]
    fn glyph_count_sums_glyphs_not_buffers() {
        let mut c = ShapedCache::<()>::new();
        c.begin_frame();
        c.insert(k(1, 80), vec![run_of(200), run_of(1)]);
        c.insert(k(2, 80), vec![run_of(1)]);
        assert_eq!(c.payload_count(), 3, "载荷数");
        assert_eq!(c.glyph_count(), 202, "字形数 = 各 run 的字形之和");
    }

    /// F192:逐出时字形数**跟着载荷一起**进回收池。
    ///
    /// 池子里躺的多半是长 ASCII 行退下来的 buffer(单个值 56KB),丢了字形数
    /// 就只能按固定价 1.8KB 计 —— 池子越大低报越狠,而池子恰恰是没有上限的
    /// 那一个(F196 才给它加 cap)。
    ///
    /// 自证会变红:把 `end_frame` 里 `map` 的 `(x.payload, x.glyphs)` 改成
    /// `(x.payload, 0)`。
    #[test]
    fn an_evicted_run_carries_its_glyph_count_to_the_pool() {
        let mut c = ShapedCache::<()>::new();
        let mut pool: Vec<((), u32)> = Vec::new();

        c.begin_frame();
        c.insert(k(1, 80), vec![run_of(200)]);
        c.end_frame(&mut pool);
        c.begin_frame();
        c.end_frame(&mut pool);

        assert_eq!(pool.len(), 1);
        assert_eq!(
            pool[0].1, 200,
            "回池的 buffer 丢了字形数 → 之后按固定价低报"
        );
    }

    /// 内容变了 → 查不到 → 整形。
    ///
    /// F174 之前这一档由 `plan_row` 里的 `c.hash == hash` 判;之后它是**键的
    /// 一部分**,所以判据搬到了「查得到吗」这一层,断言也跟着搬。
    ///
    /// 自证会变红:把 `ShapeKey` 的 `hash` 字段从 `#[derive(Hash, PartialEq)]
    /// ` 的参与范围里摘出去(例如手写 `Hash`/`PartialEq` 只看 `term_w`)。
    #[test]
    fn a_changed_hash_misses() {
        let mut c: ShapedCache<()> = ShapedCache::new();
        c.begin_frame();
        c.insert(k(7, 800), Vec::new());
        assert_eq!(plan_row(c.get(k(8, 800)), false), RowPlan::Reshape);
    }

    /// pane 像素宽变了 → 查不到 → 整形。
    ///
    /// 宽度进的是 `Buffer::set_size` 的 `avail`,**快照里没有这个量**,行指纹
    /// 覆盖不到,必须自己在键里占一格。漏了它的症状是分屏拖宽之后那一侧的
    /// 字仍按旧宽度折行,而内容没变所以永远不会自己恢复。
    ///
    /// 自证会变红:把 `ShapeKey` 的 `term_w` 字段删掉(键只留 `hash`)。
    #[test]
    fn a_changed_pane_width_misses() {
        let mut c: ShapedCache<()> = ShapedCache::new();
        c.begin_frame();
        c.insert(k(7, 800), Vec::new());
        assert_eq!(plan_row(c.get(k(7, 640)), false), RowPlan::Reshape);
    }

    /// **本次改动的全部收益所在**:整体滚动一行,只 miss 新露出的那一行。
    ///
    /// F174 之前键是 `(PaneId, row)`:滚一行,每一行的内容都挪了行号,查到的
    /// 是那个行号上的旧条目 → 指纹不等 → 整块 pane 全部重整形,尽管 `Buffer`
    /// 一个都没真变。实机剖面上就是 `reshape=miss:~30/帧`、`text_prepare`
    /// 尖到 65ms。
    ///
    /// 这条测试直接钉住那个比值:退化了会变红。**画面在两种键下都完全正确**,
    /// 没有这条断言,退化是彻底静默的。
    ///
    /// 自证会变红:把 `ShapeKey` 换回 `(PaneId, row)` 那种位置寻址的键。
    #[test]
    fn scrolling_one_line_only_misses_the_newly_revealed_row() {
        let mut c: ShapedCache<()> = ShapedCache::new();
        let mut pool = Vec::new();
        const N: u64 = 8;
        const W: u32 = 800;

        // 第一帧:行 r 放内容 r,全部 miss(首帧本来就该全 miss)。
        c.begin_frame();
        for r in 0..N {
            assert_eq!(plan_row(c.get(k(r, W)), false), RowPlan::Reshape);
            c.insert(k(r, W), vec![run()]);
        }
        c.end_frame(&mut pool);

        // 第二帧:整体上移一行 —— 行 r 现在放内容 r+1,底部新露出内容 N。
        c.begin_frame();
        let mut misses = 0;
        for r in 0..N {
            let content = r + 1;
            match plan_row(c.get(k(content, W)), false) {
                RowPlan::Reuse => c.touch(k(content, W)),
                RowPlan::Reshape => {
                    misses += 1;
                    c.insert(k(content, W), vec![run()]);
                }
                RowPlan::Temporary => unreachable!("这条测试里没有组字行"),
            }
        }
        c.end_frame(&mut pool);

        assert_eq!(
            misses, 1,
            "滚一行 miss 了 {misses} 次,应该只有新露出的那一行 —— 键退回\
             位置寻址了,滚动时整块 pane 都在白白重整形"
        );
    }

    /// 同一帧里内容相同的多行只整形一次 —— **哪怕它们在不同的 pane 里**。
    ///
    /// 空行是空闲画面的大头(见 `insert` 的文档)。整形结果只取决于
    /// (内容, `term_w`, 字体),与出自哪块 pane 的第几行无关,所以共享是
    /// **构造上正确**而非侥幸:同一个 `Buffer` 挂在多个 `TextArea` 上时,
    /// 位置由 `TextArea::left/top` 在取字形时才施加。
    ///
    /// 自证会变红:把 `PaneId` 或行号加回 `ShapeKey`。
    #[test]
    fn identical_rows_across_panes_share_one_entry() {
        let mut c: ShapedCache<()> = ShapedCache::new();
        let mut pool = Vec::new();
        const BLANK: u64 = 0;
        const W: u32 = 800;

        c.begin_frame();
        let mut shaped = 0;
        // 两块 pane 各 5 行全空。
        for _pane in 0..2 {
            for _row in 0..5 {
                match plan_row(c.get(k(BLANK, W)), false) {
                    RowPlan::Reuse => c.touch(k(BLANK, W)),
                    RowPlan::Reshape => {
                        shaped += 1;
                        c.insert(k(BLANK, W), Vec::new());
                    }
                    RowPlan::Temporary => unreachable!(),
                }
            }
        }
        c.end_frame(&mut pool);

        assert_eq!(shaped, 1, "十个同内容的空行整形了 {shaped} 次,应该只有一次");
        assert_eq!(c.len(), 1, "同内容的行没合并成一条条目");
    }

    /// 逐出:本帧没访问过的键在帧末被删。这一条同时覆盖 pane 关闭、
    /// 行数缩小、切标签、滚出视野 —— 不需要在 `close_pane` 之类的地方
    /// 各加清理 hook。
    ///
    /// 自证会变红:把 `end_frame` 的 `retain` 谓词改成恒 `true`。
    #[test]
    fn rows_not_touched_this_frame_are_evicted() {
        let mut c: ShapedCache<()> = ShapedCache::new();
        let mut pool = Vec::new();

        c.begin_frame();
        c.insert(k(1, 800), vec![run()]);
        c.insert(k(2, 800), vec![run()]);
        c.end_frame(&mut pool);
        assert_eq!(c.len(), 2);

        // 第二帧只碰头一条 —— 另一条那份内容已经滚出视野。
        c.begin_frame();
        c.touch(k(1, 800));
        c.end_frame(&mut pool);
        assert_eq!(c.len(), 1, "没访问过的内容该被逐出");
        assert!(c.get(k(1, 800)).is_some(), "还在用的内容不该被误伤");
    }

    /// 零 run 的行(整行空白)也要写条目。
    ///
    /// 空行恰是空闲画面的大头。不写条目的话它永远落在"无条目 → 整形"
    /// 分支,miss 率居高不下,差分等于白做。
    ///
    /// 自证会变红:在 `insert` 开头加 `if runs.is_empty() { return; }`。
    #[test]
    fn an_empty_row_still_gets_an_entry_so_it_can_hit_next_frame() {
        let mut c: ShapedCache<()> = ShapedCache::new();
        let mut pool = Vec::new();

        c.begin_frame();
        c.insert(k(5, 800), Vec::new());
        c.end_frame(&mut pool);

        c.begin_frame();
        assert_eq!(plan_row(c.get(k(5, 800)), false), RowPlan::Reuse);
    }

    /// 逐出的载荷进回收池,不是直接丢掉。
    ///
    /// `glyphon::Buffer` 每帧重新分配就是陷阱 T3。F174 之后 `end_frame` 是
    /// **唯一**的回收点(内容寻址下不存在同键旧载荷可摘),漏了这里滚动场景
    /// 会每帧新建上千个 buffer,**比不做差分还慢**。
    ///
    /// 自证会变红:把 `end_frame` 里 `recycle.extend(..)` 那句删掉。
    #[test]
    fn evicted_payloads_go_back_to_the_pool() {
        let mut c: ShapedCache<()> = ShapedCache::new();
        let mut pool = Vec::new();

        c.begin_frame();
        c.insert(k(1, 800), vec![run(), run()]);
        c.end_frame(&mut pool);
        assert_eq!(pool.len(), 0, "还在用的行不该被回收");

        c.begin_frame();
        c.end_frame(&mut pool);
        assert_eq!(pool.len(), 2, "逐出时旧 buffer 该回池子");
        assert!(c.is_empty());
    }

    /// 滚动稳态下池子是平的:滚出去的回池,新露出的从池里取,不新建。
    ///
    /// 这条盯的是「F174 删掉 `recycle_row` 之后回收还够不够用」——`end_frame`
    /// 排在帧末,新露出的行在**帧中**就要取池,中间差一帧。答案是够:第 N 帧
    /// 滚出去的那一行在第 N 帧末回池,第 N+1 帧新露出的行正好取到它。
    ///
    /// 自证会变红:把 `end_frame` 的回收删掉,池子会一直是空的。
    #[test]
    fn the_pool_stays_flat_while_scrolling() {
        // 载荷用 `u32` 而不是 `()`:要能区分「取到的是回收来的那一个」和
        // 「又新建了一个」,单位类型区分不了。这正是载荷做成泛型的用处。
        let mut c: ShapedCache<u32> = ShapedCache::new();
        let mut pool: Vec<(u32, u32)> = Vec::new();
        const N: u64 = 8;
        const W: u32 = 800;
        let mut allocated = 0u32;

        let frame =
            |c: &mut ShapedCache<u32>, pool: &mut Vec<(u32, u32)>, top: u64, alloc: &mut u32| {
                c.begin_frame();
                for r in 0..N {
                    let content = top + r;
                    match plan_row(c.get(k(content, W)), false) {
                        RowPlan::Reuse => c.touch(k(content, W)),
                        RowPlan::Reshape => {
                            // 整形路径:优先从池里取,取不到才新建。
                            let payload = pool.pop().map(|(p, _)| p).unwrap_or_else(|| {
                                *alloc += 1;
                                *alloc
                            });
                            c.insert(
                                k(content, W),
                                vec![CachedRun {
                                    col: 0,
                                    payload,
                                    glyphs: 1,
                                }],
                            );
                        }
                        RowPlan::Temporary => unreachable!(),
                    }
                }
                c.end_frame(pool);
            };

        // 首帧:池子空,N 行全新建。
        frame(&mut c, &mut pool, 0, &mut allocated);
        assert_eq!(allocated, u32::try_from(N).unwrap(), "首帧该新建 N 个");

        // 起步那一帧还会多分配一次(滚出去的行要到本帧末才回池)。
        frame(&mut c, &mut pool, 1, &mut allocated);
        let after_first_scroll = allocated;

        // 之后连滚十帧,一个都不该再新建。
        for top in 2..12 {
            frame(&mut c, &mut pool, top, &mut allocated);
        }
        assert_eq!(
            allocated,
            after_first_scroll,
            "滚动稳态下又新建了 {} 个 Buffer —— 回收没接上,踩 T3",
            allocated - after_first_scroll
        );
    }

    /// `clear` 同样回收 —— 换字体时清空缓存,那一批 buffer 不该白扔。
    ///
    /// 自证会变红:把 `clear` 改成只 `self.rows.clear()`。
    #[test]
    fn clearing_recycles_too() {
        let mut c: ShapedCache<()> = ShapedCache::new();
        let mut pool = Vec::new();
        c.begin_frame();
        c.insert(k(1, 800), vec![run()]);
        c.clear(&mut pool);
        assert!(c.is_empty());
        assert_eq!(pool.len(), 1);
    }
}
