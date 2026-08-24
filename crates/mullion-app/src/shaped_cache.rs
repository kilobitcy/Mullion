//! F12 差分整形:跨帧缓存"已经 shape 好的一行",靠行指纹判脏。
//!
//! **零 GPU、零 glyphon**:载荷是泛型 `T`。生产时 `T = glyphon::Buffer`,
//! 测试时 `T = ()`。泛型不是过度抽象,是**可测性的前提** —— `Buffer` 必须
//! 有一个 `FontSystem` 才能构造,不泛型就没有任何一条断言能在无头机器上跑。

use mullion_core::layout::PaneId;
use std::collections::HashMap;

/// 缓存里的一段已整形 run。`col` 是它在这一行里的起始列(渲染时
/// `left = term_px.x + col × cell_w`,与 `gpu::quads_for` 同一个式子)。
pub struct CachedRun<T> {
    pub col: u16,
    pub payload: T,
}

/// 缓存里的一行。
pub struct CachedRow<T> {
    /// 上次整形时这一行的内容指纹(`mullion_term::snapshot::hash_row`)。
    pub hash: u64,
    /// 上次整形时这个 pane 的终端区像素宽。它进的是 `Buffer::set_size`
    /// 的 `avail`,**快照里没有这个量**,指纹覆盖不到,必须单独比。
    pub term_w: u32,
    /// 最后一次被访问的帧序号。逐出判据,见 [`ShapedCache::end_frame`]。
    last_seen: u64,
    pub runs: Vec<CachedRun<T>>,
}

impl<T> CachedRow<T> {
    /// 只给单测用的构造:`plan_row` 的四条分支不需要真的 runs。
    #[cfg(test)]
    fn for_test(hash: u64, term_w: u32) -> Self {
        Self {
            hash,
            term_w,
            last_seen: 0,
            runs: Vec::new(),
        }
    }
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

/// 这一行这一帧该怎么办。**纯函数**,四条分支各有一条测试。
///
/// # 为什么组字行必须绕开缓存
///
/// 组字期间正文行带着"给拼音让路"的空洞(`text::hidden_span_for_row`)。
/// 若把这份结果写回缓存,用户按 Esc 取消组字后 cells 没变、指纹相同 →
/// 下一帧命中 → 复用**带空洞的** buffer → 被拼音盖住的那几个字永久消失。
/// 这正是本设计要根除的"静默陈旧"。
///
/// 组字行的旧条目会因为本帧没被访问而在帧末逐出,组字结束后的第一帧
/// 按"无条目"miss 一次即恢复。
pub fn plan_row<T>(
    cached: Option<&CachedRow<T>>,
    hash: u64,
    term_w: u32,
    is_preedit_row: bool,
) -> RowPlan {
    if is_preedit_row {
        return RowPlan::Temporary;
    }
    match cached {
        Some(c) if c.hash == hash && c.term_w == term_w => RowPlan::Reuse,
        _ => RowPlan::Reshape,
    }
}

/// 按 `(PaneId, row)` 分槽的跨帧整形缓存。
///
/// **键必须是 `PaneId` 这种稳定身份**,不能是 `panes.iter().enumerate()`
/// 的当帧下标:关掉中间一块 pane 会让其后所有 pane 的下标挪位,拿下标
/// 当键会张冠李戴地把 A 的缓存当成 B 用。
pub struct ShapedCache<T> {
    rows: HashMap<(PaneId, u16), CachedRow<T>>,
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

    pub fn get(&self, key: (PaneId, u16)) -> Option<&CachedRow<T>> {
        self.rows.get(&key)
    }

    /// 命中路径:标记这一行本帧用过,免得被帧末逐出。
    pub fn touch(&mut self, key: (PaneId, u16)) {
        let f = self.frame;
        if let Some(r) = self.rows.get_mut(&key) {
            r.last_seen = f;
        }
    }

    /// 未命中路径:写入刚整形好的一行。
    ///
    /// **`runs` 为空也要写。** `row_to_runs` 会把整行空白直接丢掉,空行的
    /// 整形产物就是空集;不写条目的话空行永远 miss,而空行恰是空闲画面的
    /// 大头 —— 差分就白做了。
    pub fn insert(&mut self, key: (PaneId, u16), hash: u64, term_w: u32, runs: Vec<CachedRun<T>>) {
        let last_seen = self.frame;
        self.rows.insert(
            key,
            CachedRow {
                hash,
                term_w,
                last_seen,
                runs,
            },
        );
    }

    /// 重整形前把这一行的旧载荷摘出来推进 `recycle`,条目本身删掉。
    ///
    /// 不回收的话,滚动的日志(每帧每行都变)会每帧新建上千个
    /// `glyphon::Buffer` —— 那是陷阱 T3,且**比改之前更慢**。
    pub fn recycle_row(&mut self, key: (PaneId, u16), recycle: &mut Vec<T>) {
        if let Some(mut r) = self.rows.remove(&key) {
            recycle.extend(r.runs.drain(..).map(|x| x.payload));
        }
    }

    /// 一帧结束:本帧没访问过的键全删,载荷推进 `recycle`。
    ///
    /// 这一条**统一覆盖** pane 关闭、行数缩小、切标签。刻意不在
    /// `close_pane` 之类的地方各加一处清理 hook —— 那种列举式门控在
    /// 加档时必然漏,本项目已经踩中过三次。
    pub fn end_frame(&mut self, recycle: &mut Vec<T>) {
        let f = self.frame;
        self.rows.retain(|_, r| {
            if r.last_seen == f {
                return true;
            }
            recycle.extend(r.runs.drain(..).map(|x| x.payload));
            false
        });
    }

    /// 全清(换字体族/字号/DPI)。载荷同样回收。
    pub fn clear(&mut self, recycle: &mut Vec<T>) {
        for (_, mut r) in self.rows.drain() {
            recycle.extend(r.runs.drain(..).map(|x| x.payload));
        }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
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
    use mullion_core::layout::PaneId;

    const P0: PaneId = PaneId(0);
    const P1: PaneId = PaneId(1);

    /// 首帧:没有条目 → 整形。
    ///
    /// 自证会变红:把 `plan_row` 的 `None` 分支改成 `RowPlan::Reuse`。
    #[test]
    fn a_row_with_no_entry_is_reshaped() {
        assert_eq!(
            plan_row(None::<&CachedRow<()>>, 7, 800, false),
            RowPlan::Reshape
        );
    }

    /// 指纹变了 → 重整形。这条覆盖内容、SGR、选区反色、主题换色**四类**
    /// 失效源(它们全都烘在快照字段里,见 hash_row 的文档)。
    ///
    /// 自证会变红:把 `plan_row` 里的 `c.hash == hash` 删掉。
    #[test]
    fn a_changed_hash_is_reshaped() {
        let c = CachedRow::<()>::for_test(7, 800);
        assert_eq!(plan_row(Some(&c), 8, 800, false), RowPlan::Reshape);
    }

    /// pane 像素宽变了 → 重整形。宽度进的是 `Buffer::set_size` 的 `avail`,
    /// 快照里没有它,指纹覆盖不到,必须单独比。
    ///
    /// 自证会变红:把 `plan_row` 里的 `c.term_w == term_w` 删掉。
    #[test]
    fn a_changed_pane_width_is_reshaped() {
        let c = CachedRow::<()>::for_test(7, 800);
        assert_eq!(plan_row(Some(&c), 7, 640, false), RowPlan::Reshape);
    }

    /// 都没变 → 复用。这是整个改动的收益来源。
    ///
    /// 自证会变红:让 `plan_row` 无条件返回 `RowPlan::Reshape`
    /// —— 画面依旧全对,性能悄悄回到改之前。这正是 §7 那个运行期
    /// `reshape=hit:/miss:` 计数器存在的理由。
    #[test]
    fn an_unchanged_row_is_reused() {
        let c = CachedRow::<()>::for_test(7, 800);
        assert_eq!(plan_row(Some(&c), 7, 800, false), RowPlan::Reuse);
    }

    /// 组字行走临时槽 —— **哪怕缓存里有一条完全匹配的条目**。
    ///
    /// 这条是防"看起来更聪明"的写法:若组字行也允许命中缓存,用户按 Esc
    /// 取消组字后 cells 没变、指纹相同,会复用**带拼音空洞的**那份 buffer,
    /// 被盖住的几个字永久消失。
    ///
    /// 自证会变红:把 `plan_row` 里 `is_preedit_row` 那条提前返回删掉。
    #[test]
    fn a_preedit_row_is_temporary_even_when_the_cache_matches() {
        let c = CachedRow::<()>::for_test(7, 800);
        assert_eq!(plan_row(Some(&c), 7, 800, true), RowPlan::Temporary);
    }

    /// 逐出:本帧没访问过的键在帧末被删。这一条同时覆盖 pane 关闭、
    /// 行数缩小、切标签 —— 不需要在 `close_pane` 之类的地方各加清理 hook。
    ///
    /// 自证会变红:把 `end_frame` 的 `retain` 谓词改成恒 `true`。
    #[test]
    fn rows_not_touched_this_frame_are_evicted() {
        let mut c: ShapedCache<()> = ShapedCache::new();
        let mut pool = Vec::new();

        c.begin_frame();
        c.insert(
            (P0, 0),
            1,
            800,
            vec![CachedRun {
                col: 0,
                payload: (),
            }],
        );
        c.insert(
            (P1, 0),
            2,
            800,
            vec![CachedRun {
                col: 0,
                payload: (),
            }],
        );
        c.end_frame(&mut pool);
        assert_eq!(c.len(), 2);

        // 第二帧只碰 P0 —— P1 那块 pane 被关掉了。
        c.begin_frame();
        c.touch((P0, 0));
        c.end_frame(&mut pool);
        assert_eq!(c.len(), 1, "没访问过的 pane 该被逐出");
        assert!(c.get((P0, 0)).is_some(), "还在的 pane 不该被误伤");
    }

    /// 关掉**中间**一块 pane 之后,剩下的 pane 仍然拿到自己的缓存。
    ///
    /// 缓存键必须是 `PaneId` 这种稳定身份。用 `panes.iter().enumerate()`
    /// 的当帧下标当键的话,关掉中间一块会让其后所有 pane 的下标挪位 ——
    /// A 的缓存被当成 B 的用,屏幕上两块 pane 的内容互换。
    ///
    /// 自证会变红:把 `ShapedCache` 的键类型从 `(PaneId, u16)` 换成
    /// `(usize, u16)` 并在这里按下标存取。
    #[test]
    fn the_cache_is_keyed_by_pane_id_not_by_frame_index() {
        let mut c: ShapedCache<()> = ShapedCache::new();
        let mut pool = Vec::new();
        let p2 = PaneId(2);

        // 第一帧:三块 pane,下标 0/1/2。
        c.begin_frame();
        c.insert((P0, 0), 10, 800, Vec::new());
        c.insert((P1, 0), 11, 800, Vec::new());
        c.insert((p2, 0), 12, 800, Vec::new());
        c.end_frame(&mut pool);

        // 第二帧:中间那块(P1)关了,P2 的当帧下标从 2 挪到了 1。
        c.begin_frame();
        c.touch((P0, 0));
        c.touch((p2, 0));
        assert_eq!(
            c.get((p2, 0)).map(|r| r.hash),
            Some(12),
            "P2 拿到了别人的缓存"
        );
        c.end_frame(&mut pool);
        assert!(c.get((P1, 0)).is_none(), "关掉的 pane 该被逐出");
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
        c.insert((P0, 0), 5, 800, Vec::new());
        c.end_frame(&mut pool);

        c.begin_frame();
        assert_eq!(plan_row(c.get((P0, 0)), 5, 800, false), RowPlan::Reuse);
    }

    /// 逐出的载荷进回收池,不是直接丢掉。
    ///
    /// `glyphon::Buffer` 每帧重新分配就是陷阱 T3。滚动的日志每帧每行都变,
    /// 若重整形时丢弃旧 buffer、新建一批,流式场景(N2 要保的那一档)会
    /// **比改之前更慢**。
    ///
    /// 自证会变红:把 `end_frame` / `recycle_row` 里 `recycle.extend(..)`
    /// 那句删掉。
    #[test]
    fn evicted_and_reshaped_payloads_go_back_to_the_pool() {
        let mut c: ShapedCache<()> = ShapedCache::new();
        let mut pool = Vec::new();

        c.begin_frame();
        c.insert(
            (P0, 0),
            1,
            800,
            vec![
                CachedRun {
                    col: 0,
                    payload: (),
                },
                CachedRun {
                    col: 3,
                    payload: (),
                },
            ],
        );
        c.end_frame(&mut pool);
        assert_eq!(pool.len(), 0, "还在用的行不该被回收");

        // 重整形前先把旧载荷收回来。
        c.begin_frame();
        c.recycle_row((P0, 0), &mut pool);
        assert_eq!(pool.len(), 2, "重整形时旧 buffer 该回池子");
        assert!(c.get((P0, 0)).is_none(), "recycle_row 该把条目一起摘掉");

        // 逐出路径同样回收。
        c.insert(
            (P1, 0),
            2,
            800,
            vec![CachedRun {
                col: 0,
                payload: (),
            }],
        );
        c.end_frame(&mut pool);
        c.begin_frame();
        c.end_frame(&mut pool);
        assert_eq!(pool.len(), 3, "逐出时旧 buffer 也该回池子");
    }

    /// `clear` 同样回收 —— 换字体时清空缓存,那一批 buffer 不该白扔。
    ///
    /// 自证会变红:把 `clear` 改成只 `self.rows.clear()`。
    #[test]
    fn clearing_recycles_too() {
        let mut c: ShapedCache<()> = ShapedCache::new();
        let mut pool = Vec::new();
        c.begin_frame();
        c.insert(
            (P0, 0),
            1,
            800,
            vec![CachedRun {
                col: 0,
                payload: (),
            }],
        );
        c.clear(&mut pool);
        assert!(c.is_empty());
        assert_eq!(pool.len(), 1);
    }
}
