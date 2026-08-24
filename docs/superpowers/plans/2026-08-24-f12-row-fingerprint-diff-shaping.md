# F12 差分整形（按行指纹）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 `TextLayer::prepare_panes` 只对"这一帧真的变了"的行做文本整形，消除
"任意一个 pane 闪一下就拖着全窗口所有 pane 重新整形一遍"的浪费（F12 / N1 / N2）。

**Architecture:** `Emulator::snapshot()` 在建每行 cells 之后顺手算一个覆盖
`SnapCell` 全部六个字段的 FNV-1a 行指纹；`mullion-app` 新增一个纯数据的
`ShapedCache<T>`（按 `(PaneId, row)` 分槽、跨帧存活、帧末按帧序号逐出），
`prepare_panes` 逐行比对指纹与 pane 像素宽，命中就复用已 shape 好的 `glyphon::Buffer`。
判据是"结果变没变"（构造式），不是"列举所有会让它变的原因"（枚举式）——
详见设计文档 `docs/superpowers/specs/2026-08-24-f12-row-fingerprint-diff-shaping-design.md`。

**Tech Stack:** Rust 2021 / workspace 五 crate；`glyphon` + `cosmic-text`（文本整形）；
`alacritty_terminal`（VT 仿真）；`mullion-core::layout::PaneId`（缓存键）。

---

## 背景：读这份计划前必须知道的五件事

1. **依赖方向是硬约束**：`app → {core, term, ssh, store}`，其余互不依赖。本计划
   只在 `mullion-term` 和 `mullion-app` 里改东西，`mullion-term` 不会开始认识
   `PaneId`（缓存键完全在 app 侧）。
2. **"绿"的定义**：`cargo test --workspace` 全过 **且**
   `cargo clippy --workspace --all-targets -- -D warnings` 无输出。只跑单个 crate
   不叫绿。
3. **T3（领域陷阱）**：帧路径上不许每帧新分配大量对象。本计划里的 `pool` 就是
   为这条服务的，不是可选优化。
4. **不得为通过测试而削弱测试**。每条测试的 doc 注释里都写了"自证会变红"——
   实现完成后应当能按那句话把测试改红，改不红说明这条测试是恒绿的假守护。
5. **`prepare_panes` 本身跑不了单测**（要真实 wgpu `Device`/`Queue`）。所以本计划
   把每一处能抽的判断都抽成了自由函数/纯模块。剩下的机械接线由 Task 6 的运行期
   计数器 + Task 8 的人工验收清单兜底，**不要假装它被测到了**。

## 文件结构

| 文件 | 动作 | 职责 |
|---|---|---|
| `crates/mullion-term/src/snapshot.rs` | 改 | 新增 `hash_row()` 纯函数 + `GridSnapshot::new()` 唯一构造入口；`row_hash` 字段私有 + 访问器 |
| `crates/mullion-term/src/emulator.rs` | 改 | `snapshot()` 改走 `GridSnapshot::new()` |
| `crates/mullion-app/src/shaped_cache.rs` | **建** | `ShapedCache<T>` / `CachedRow<T>` / `CachedRun<T>` / `RowPlan` / `plan_row()`。纯数据，零 GPU，可单测 |
| `crates/mullion-app/src/lib.rs` | 改 | 挂 `pub mod shaped_cache;` |
| `crates/mullion-app/src/text.rs` | 改 | `TextLayer` 换字段；`prepare_panes` 改成查缓存；`set_font` 清缓存；抽 `shape_run()` |
| `crates/mullion-app/src/gpu.rs` | 改 | 三处测试用 `GridSnapshot` 构造改走 `new()` |
| `crates/mullion-app/src/diag.rs` | 改 | `RESHAPE_HIT` / `RESHAPE_MISS` 计数器 + `count_reshape()` |
| `crates/mullion-app/src/profile.rs` | 改 | `Snapshot` 两个新字段 + 剖面行 `reshape=hit:/miss:` |
| `spec.md` | 改 | F12 措辞 |
| `docs/adr-011-row-fingerprint-vs-term-damage.md` | **建** | 记"手上有 `Term::damage()` 为什么不用" |
| `docs/gui-render-gotchas.md` | 改 | 补一条"六字段同源"的坑 |

`shaped_cache.rs` 单独成文件而不是塞进 `text.rs`：`text.rs` 已经 1349 行，且
`ShapedCache` 与 glyphon 完全无关（它对载荷泛型），混进去就等于把一块本可以
纯单测的逻辑拖进"要 GPU 才能跑"的文件里——那正是 `hidden_span_for_row` 上一轮
踩过的坑（见 `text.rs:191-199` 的注释）。

---

## Task 1: `hash_row` 行指纹纯函数（mullion-term）

**Files:**
- Modify: `crates/mullion-term/src/snapshot.rs`（当前 81 行，末尾追加）

`snapshot.rs` 目前**没有** `#[cfg(test)] mod tests`，这一步会建第一个。

- [ ] **Step 1: 先写会失败的测试**

在 `crates/mullion-term/src/snapshot.rs` **文件末尾**追加：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// 一个基准格。各测试只动其中一个字段,用来证明"那个字段进了哈希"。
    fn base() -> SnapCell {
        SnapCell {
            ch: 'a',
            fg: Rgb::new(0xcc, 0xcc, 0xcc),
            bg: Rgb::new(0x10, 0x10, 0x10),
            width: 1,
            spacer: false,
            selected: false,
        }
    }

    /// 改一个字段,断言整行指纹跟着变。
    fn changing(f: impl FnOnce(&mut SnapCell)) -> (u64, u64) {
        let before = [base(), base()];
        let mut after = before;
        f(&mut after[1]);
        (hash_row(&before), hash_row(&after))
    }

    /// 同样的一行,算两次必须一样。**没有这条,下面六条全是恒绿的**
    /// —— 一个每次返回随机数的 `hash_row` 能让"变了就不等"全部通过。
    ///
    /// 自证会变红:让 `hash_row` 的 `h` 初值改成 `cells.len() as u64`
    /// 之外的任何随每次调用变化的东西。
    #[test]
    fn the_same_row_hashes_the_same_every_time() {
        let row = [base(), base()];
        assert_eq!(hash_row(&row), hash_row(&row));
    }

    /// 内容变(SGR 之外最常见的一种)。
    ///
    /// 自证会变红:从 `hash_row` 里删掉喂 `ch` 的那几行。
    #[test]
    fn a_changed_char_changes_the_row_hash() {
        let (a, b) = changing(|c| c.ch = 'b');
        assert_ne!(a, b, "改了字符,指纹没变 —— 屏幕会留着旧字");
    }

    /// SGR 前景色 / 主题换色 / bold 提亮,最终都落在 `fg` 上。
    ///
    /// 自证会变红:从 `hash_row` 里删掉 `fg.r`/`fg.g`/`fg.b`。
    #[test]
    fn a_changed_fg_changes_the_row_hash() {
        let (a, b) = changing(|c| c.fg = Rgb::new(0xff, 0x00, 0x00));
        assert_ne!(a, b, "改了前景色,指纹没变");
    }

    /// 背景色。选区反色会把它读成文字色(`text.rs::row_to_spans`),
    /// 所以它同样影响整形结果,不是"只影响 quad 层"。
    ///
    /// 自证会变红:从 `hash_row` 里删掉 `bg.r`/`bg.g`/`bg.b`。
    #[test]
    fn a_changed_bg_changes_the_row_hash() {
        let (a, b) = changing(|c| c.bg = Rgb::new(0x00, 0xff, 0x00));
        assert_ne!(a, b, "改了背景色,指纹没变 —— 选区反色会用陈旧字色");
    }

    /// 宽度决定 `row_to_runs` 怎么切 run(F16)。
    ///
    /// 自证会变红:从 `hash_row` 里删掉 `width`。
    #[test]
    fn a_changed_width_changes_the_row_hash() {
        let (a, b) = changing(|c| c.width = 2);
        assert_ne!(a, b, "改了显示宽度,指纹没变");
    }

    /// spacer 决定这一格跳不跳过。
    ///
    /// 自证会变红:从 `hash_row` 里删掉 `spacer`。
    #[test]
    fn a_changed_spacer_changes_the_row_hash() {
        let (a, b) = changing(|c| c.spacer = true);
        assert_ne!(a, b, "改了 spacer 标记,指纹没变");
    }

    /// F18 选区。alacritty 的 `Term::damage()` **不含**选区变化,
    /// 这正是本设计不用 damage 的头号理由 —— 指纹必须自己覆盖它。
    ///
    /// 自证会变红:从 `hash_row` 里删掉 `selected`。
    #[test]
    fn a_changed_selection_changes_the_row_hash() {
        let (a, b) = changing(|c| c.selected = true);
        assert_ne!(a, b, "改了选中标记,指纹没变 —— 划选后文字不反色");
    }

    /// 行变长/变短必须换指纹。定长逐格喂字节天然覆盖,写下来是防止
    /// 日后有人"优化"成只喂非空白格。
    ///
    /// 自证会变红:在 `hash_row` 开头加 `let cells = &cells[..1.min(cells.len())];`。
    #[test]
    fn a_longer_row_hashes_differently() {
        assert_ne!(hash_row(&[base()]), hash_row(&[base(), base()]));
    }
}
```

- [ ] **Step 2: 跑测试,确认它因为编译不过而失败**

```bash
cargo test -p mullion-term hash_row 2>&1 | tail -20
```

Expected: 编译错误 `cannot find function 'hash_row' in this scope`。

- [ ] **Step 3: 写最小实现**

在 `crates/mullion-term/src/snapshot.rs` 里，`impl Rgb { ... }` 之后、
`pub struct SnapCell` 之前插入：

```rust
/// FNV-1a(64 位)的偏移基与质数。
///
/// 为什么手写而不是 `DefaultHasher`:`std` 的 `RandomState` 带随机种子,
/// 同一份内容在同一进程的两帧之间都可能算出不同的值,拿它做跨帧比对是
/// 直接坏掉的;`DefaultHasher::new()` 虽然确定,但标准库**明确不保证**
/// 跨版本稳定。FNV-1a 只有十行,零依赖,可直接单测。
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

#[inline]
fn eat(h: u64, b: u8) -> u64 {
    (h ^ u64::from(b)).wrapping_mul(FNV_PRIME)
}

/// 一行的内容指纹(F12)。**渲染层判"这一行要不要重新整形"的唯一判据。**
///
/// # 最关键的不变量
///
/// 喂进去的字段必须**恰好等于** `mullion-app` 的 `text::row_to_runs` /
/// `row_to_spans` 真正读到的字段。少喂一个,那一类变化就**静默**不重画 ——
/// 症状是屏幕上留着一行陈旧的字,编译不报错、测试不报错、日志不报错,
/// 只有人眼能发现。
///
/// 两层机械守护,缺一不可:
///
/// 1. **存量字段**:本模块 `tests` 里的六条 `a_changed_*_changes_the_row_hash`,
///    一条对一个字段,一条都不能省。
/// 2. **增量字段**:下面那句**穷尽解构**。给 `SnapCell` 加字段(比如
///    underline)时,这里会当场编译报错,强迫作者对"进不进哈希"表态,
///    而不是静默漏掉。**不要**把它改成 `cell.ch` 那种点号取字段的写法 ——
///    那样加字段就没有任何提示了。
///
/// SGR bold 不必单列:`Emulator::snapshot` 已经用 `palette::bold_brighten`
/// 把它烘进了 `fg`。
pub fn hash_row(cells: &[SnapCell]) -> u64 {
    let mut h = FNV_OFFSET;
    for cell in cells {
        // 穷尽解构 —— 见上面的文档,这一行是增量字段的唯一守护。
        let SnapCell {
            ch,
            fg,
            bg,
            width,
            spacer,
            selected,
        } = *cell;
        for b in (ch as u32).to_le_bytes() {
            h = eat(h, b);
        }
        for b in [
            fg.r,
            fg.g,
            fg.b,
            bg.r,
            bg.g,
            bg.b,
            width,
            u8::from(spacer),
            u8::from(selected),
        ] {
            h = eat(h, b);
        }
    }
    h
}
```

- [ ] **Step 4: 跑测试,确认全过**

```bash
cargo test -p mullion-term 2>&1 | grep -E "test result|FAILED|panicked"
```

Expected: `test result: ok.`，其中包含新增的 8 条。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-term/src/snapshot.rs
git commit -m "feat(term): 行内容指纹 hash_row,六字段逐条守护 + 穷尽解构 (F12)

差分整形的判据基础。哈希输入恰好覆盖 row_to_runs/row_to_spans 读到的
六个字段(= SnapCell 全部字段);存量字段由六条逐字段测试守护,增量字段
由函数体内的穷尽解构守护(加字段即编译报错)。"
```

---

## Task 2: `GridSnapshot` 带上 `row_hash`（mullion-term）

**Files:**
- Modify: `crates/mullion-term/src/snapshot.rs:68-81`
- Modify: `crates/mullion-term/src/emulator.rs:373`（`snapshot()` 的返回构造）

`row_hash` **做成私有字段 + 访问器**，构造只能走 `GridSnapshot::new()`。这样
crate 外任何"手搓一个 GridSnapshot"的地方都无法绕过指纹计算——编译器拦得住，
不必靠人记得。

- [ ] **Step 1: 先写会失败的测试**

在 `crates/mullion-term/src/snapshot.rs` 的 `mod tests` 里追加：

```rust
    fn cursor_at_origin() -> Cursor {
        Cursor {
            row: 0,
            col: 0,
            visible: false,
            shape: CursorShape::Beam,
            blinking: false,
        }
    }

    /// `new()` 给每一行都算好指纹,长度 == rows。
    ///
    /// 自证会变红:让 `GridSnapshot::new` 的 `row_hash` 收成 `Vec::new()`。
    #[test]
    fn new_fills_one_hash_per_row() {
        let s = GridSnapshot::new(3, 2, vec![base(); 6], cursor_at_origin());
        assert_eq!(s.row_hash(0), hash_row(s.row(0)));
        assert_eq!(s.row_hash(1), hash_row(s.row(1)));
    }

    /// **F12 的验收标准**(`spec.md`:"只改一行后,脏行集合只含那一行")。
    ///
    /// 自证会变红:让 `GridSnapshot::new` 把每一行的指纹都算成
    /// `hash_row(&cells)`(整份 cells 而不是本行切片)—— 那样改一行会让
    /// 所有行的指纹一起变,差分就退化成全量。
    #[test]
    fn changing_one_row_moves_only_that_rows_hash() {
        let before = GridSnapshot::new(3, 3, vec![base(); 9], cursor_at_origin());
        let mut cells = vec![base(); 9];
        cells[3 + 1].ch = 'Z'; // 第 1 行、第 1 列
        let after = GridSnapshot::new(3, 3, cells, cursor_at_origin());

        assert_eq!(before.row_hash(0), after.row_hash(0), "第 0 行不该变");
        assert_ne!(before.row_hash(1), after.row_hash(1), "第 1 行该变");
        assert_eq!(before.row_hash(2), after.row_hash(2), "第 2 行不该变");
    }

    /// 越界行号返回 0 而不是 panic。渲染层拿到的 `rows` 与快照的 `rows`
    /// 在 resize 那一帧可能短暂不一致,不能让它把进程带走。
    ///
    /// 自证会变红:把 `row_hash()` 的实现改成 `self.row_hash[row as usize]`。
    #[test]
    fn an_out_of_range_row_hash_is_zero_not_a_panic() {
        let s = GridSnapshot::new(3, 2, vec![base(); 6], cursor_at_origin());
        assert_eq!(s.row_hash(9), 0);
    }
```

- [ ] **Step 2: 跑测试,确认它因为编译不过而失败**

```bash
cargo test -p mullion-term new_fills_one_hash 2>&1 | tail -20
```

Expected: 编译错误 `no function or associated item named 'new' found for struct 'GridSnapshot'`。

- [ ] **Step 3: 改 `GridSnapshot`**

把 `crates/mullion-term/src/snapshot.rs:68-81` 整段替换为：

```rust
pub struct GridSnapshot {
    pub cols: u16,
    pub rows: u16,
    pub cells: Vec<SnapCell>,
    pub cursor: Cursor,
    /// 每行的内容指纹(F12),长度 == `rows`。
    ///
    /// **私有是有意的**:它必须与 `cells` 严格同步,而字段公开就意味着
    /// crate 外可以手搓一个 `GridSnapshot { .. }` 却把指纹填成 `vec![0; n]`
    /// —— 那样渲染层会认为"这一行永远没变",屏幕永久停在第一帧。
    /// 私有之后唯一的构造入口是 [`GridSnapshot::new`],编译器保证同步。
    row_hash: Vec<u64>,
}

impl GridSnapshot {
    /// **唯一的构造入口**,顺手把每行指纹算好(F12)。
    ///
    /// `cells.len()` 应当等于 `cols × rows`;不足的行按 0 补指纹而不是
    /// panic —— 快照是渲染路径上的东西,宁可这一帧多整形一次,也不能崩。
    pub fn new(cols: u16, rows: u16, cells: Vec<SnapCell>, cursor: Cursor) -> Self {
        let w = cols as usize;
        let row_hash = (0..rows as usize)
            .map(|r| {
                let start = r * w;
                cells
                    .get(start..start + w)
                    .map_or(0, hash_row)
            })
            .collect();
        Self {
            cols,
            rows,
            cells,
            cursor,
            row_hash,
        }
    }

    /// 第 `row` 行的单元格切片(长度 == cols)。
    pub fn row(&self, row: u16) -> &[SnapCell] {
        let start = row as usize * self.cols as usize;
        &self.cells[start..start + self.cols as usize]
    }

    /// 第 `row` 行的内容指纹(F12)。越界返回 0。
    ///
    /// 0 是"未知"而不是"某个具体内容":调用方拿它跟缓存里的值比,
    /// 比不上就重新整形 —— 越界那一帧多整形一次,而不是漏画。
    pub fn row_hash(&self, row: u16) -> u64 {
        self.row_hash.get(row as usize).copied().unwrap_or(0)
    }
}
```

- [ ] **Step 4: 改 `Emulator::snapshot()` 的返回构造**

在 `crates/mullion-term/src/emulator.rs`，把 `snapshot()` 结尾那段
（`GridSnapshot {` 开始到函数体结束的 `}`）替换为：

```rust
        GridSnapshot::new(
            cols as u16,
            rows as u16,
            cells,
            Cursor {
                row: cursor_row.max(0) as u16,
                col: p.column.0 as u16,
                // MVP 未接 DECTCEM(`\x1b[?25l`/`\x1b[?25h`)光标隐藏/显示;
                // 这里只处理「滚出可视区」这一种不可见(F17)。
                visible: cursor_row >= 0 && (cursor_row as usize) < rows,
                shape: map_shape(style.shape),
                blinking: style.blinking,
            },
        )
```

- [ ] **Step 5: 跑 mullion-term 全测**

```bash
cargo test -p mullion-term 2>&1 | grep -E "test result|FAILED|panicked"
```

Expected: 全 ok。**注意** `mullion-app` 此时编译不过（四处测试用的结构体字面量
少了 `row_hash`，且字段私有）——那是 Task 3 的事，本步只跑 `-p mullion-term`。

- [ ] **Step 6: 补一条回溯滚动的守护测试**

在 `crates/mullion-term/src/emulator.rs` 的 `mod tests` 末尾追加（F17 陷阱：
行号换算必须与 `snapshot()` 的 `display_offset` 同源）：

```rust
    /// F17:回溯滚动后,指纹跟着**内容**走而不是跟着屏幕位置走。
    ///
    /// 喂满两屏 + 一行,然后往回滚一行:滚动前的第 0 行内容,滚动后应当
    /// 出现在第 1 行,两者指纹必须相等。
    ///
    /// 自证会变红:把 `GridSnapshot::new` 里的行切片换成固定的第 0 行。
    #[test]
    fn row_hashes_follow_the_content_when_scrolled_back() {
        let mut e = Emulator::with_history(10, 4, 100);
        for i in 0..8 {
            e.feed(format!("line{i}\r\n").as_bytes());
        }
        let before = e.snapshot();
        e.scroll_display(1);
        let after = e.snapshot();
        assert_eq!(
            before.row_hash(0),
            after.row_hash(1),
            "回溯一行后,原第 0 行的内容该落在第 1 行且指纹不变"
        );
    }
```

**实现者注意**：`Emulator::with_history` 与 `scroll_display` 的确切签名请先
`grep -n "pub fn with_history\|pub fn scroll_display" crates/mullion-term/src/emulator.rs`
核对，按实际签名调整这两行调用（本项目 API 漂移条款：不要凭记忆写）。

- [ ] **Step 7: 跑测试**

```bash
cargo test -p mullion-term 2>&1 | grep -E "test result|FAILED|panicked"
```

Expected: 全 ok。

- [ ] **Step 8: 提交**

```bash
git add crates/mullion-term/src/snapshot.rs crates/mullion-term/src/emulator.rs
git commit -m "feat(term): GridSnapshot 带每行指纹,构造收口到 new() (F12)

row_hash 做成私有字段 + 访问器:字段公开就意味着 crate 外能手搓一份
把指纹填成全 0 的快照,渲染层会据此认为「永远没变」、屏幕永久停在第
一帧。私有之后唯一入口是 new(),编译器保证指纹与 cells 同步。

守护:changing_one_row_moves_only_that_rows_hash(F12 验收标准)、
row_hashes_follow_the_content_when_scrolled_back(F17)。"
```

---

## Task 3: 修好 `mullion-app` 侧四处快照构造（编译修复）

**Files:**
- Modify: `crates/mullion-app/src/gpu.rs`（`snap_1x1` / `snap_selected_1x1` / `snap_with_cursor` 三处测试辅助）
- Modify: `crates/mullion-app/src/text.rs`（`snapshot_for_hidden_span_tests` 一处测试辅助）

这四处都是 `#[cfg(test)]` 里的辅助函数，Task 2 把 `row_hash` 私有化之后它们
用不了结构体字面量了。这正是私有化的目的：**编译器逼着每个构造点走 `new()`**。

- [ ] **Step 1: 先确认当前是红的**

```bash
cargo test -p mullion-app --no-run 2>&1 | grep -E "^error" | head
```

Expected: 四处 `error[E0451]: field 'row_hash' of struct 'GridSnapshot' is private`
（或 `E0063: missing field`）。

- [ ] **Step 2: 改 `gpu.rs` 的 `snap_1x1`**

把 `crates/mullion-app/src/gpu.rs` 里 `fn snap_1x1` 的函数体替换为：

```rust
    fn snap_1x1(bg: Rgb) -> GridSnapshot {
        GridSnapshot::new(
            1,
            1,
            vec![SnapCell {
                ch: ' ',
                fg: Rgb::new(0xcc, 0xcc, 0xcc),
                bg,
                width: 1,
                spacer: false,
                selected: false,
            }],
            Cursor {
                row: 0,
                col: 0,
                visible: false,
                shape: CursorShape::Beam,
                blinking: true,
            },
        )
    }
```

- [ ] **Step 3: 改 `gpu.rs` 的 `snap_selected_1x1`**

```rust
    fn snap_selected_1x1(fg: Rgb, bg: Rgb) -> GridSnapshot {
        GridSnapshot::new(
            1,
            1,
            vec![SnapCell {
                ch: 'a',
                fg,
                bg,
                width: 1,
                spacer: false,
                selected: true,
            }],
            Cursor {
                row: 0,
                col: 0,
                visible: false,
                shape: CursorShape::Beam,
                blinking: true,
            },
        )
    }
```

- [ ] **Step 4: 改 `gpu.rs` 的 `snap_with_cursor`**

```rust
    /// 造一个 cols=4 rows=2、光标在 (row, col) 且形状为 `shape` 的快照(F125)。
    fn snap_with_cursor(col: u16, row: u16, shape: CursorShape) -> GridSnapshot {
        let blank = SnapCell {
            ch: ' ',
            fg: Rgb::new(0xcc, 0xcc, 0xcc),
            bg: Rgb::new(0x10, 0x10, 0x10),
            width: 1,
            spacer: false,
            selected: false,
        };
        GridSnapshot::new(
            4,
            2,
            vec![blank; 8],
            mullion_term::snapshot::Cursor {
                row,
                col,
                visible: true,
                shape,
                blinking: true,
            },
        )
    }
```

- [ ] **Step 5: 改 `text.rs` 的 `snapshot_for_hidden_span_tests`**

```rust
    fn snapshot_for_hidden_span_tests(
        cursor_row: u16,
        cursor_col: u16,
        visible: bool,
    ) -> GridSnapshot {
        let blank = SnapCell {
            ch: ' ',
            fg: Rgb::new(0xcc, 0xcc, 0xcc),
            bg: Rgb::new(0x10, 0x10, 0x10),
            width: 1,
            spacer: false,
            selected: false,
        };
        GridSnapshot::new(
            20,
            4,
            vec![blank; 20 * 4],
            mullion_term::snapshot::Cursor {
                row: cursor_row,
                col: cursor_col,
                visible,
                shape: mullion_term::snapshot::CursorShape::Block,
                blinking: true,
            },
        )
    }
```

- [ ] **Step 6: 跑全 workspace 测试**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
```

Expected: 全 ok，无 FAILED。

- [ ] **Step 7: 提交**

```bash
git add crates/mullion-app/src/gpu.rs crates/mullion-app/src/text.rs
git commit -m "refactor(app): 测试侧快照构造改走 GridSnapshot::new (F12)

row_hash 私有化的直接后果 —— 编译器把四处结构体字面量逼到唯一构造
入口上。纯机械改动,不动任何断言。"
```

---

## Task 4: `ShapedCache` 纯模块（mullion-app）

**Files:**
- Create: `crates/mullion-app/src/shaped_cache.rs`
- Modify: `crates/mullion-app/src/lib.rs:32`（在 `pub mod shell;` 之前按字母序插入）

- [ ] **Step 1: 建文件,先只写测试**

创建 `crates/mullion-app/src/shaped_cache.rs`，内容为：

```rust
//! F12 差分整形:跨帧缓存"已经 shape 好的一行",靠行指纹判脏。
//!
//! **零 GPU、零 glyphon**:载荷是泛型 `T`。生产时 `T = glyphon::Buffer`,
//! 测试时 `T = ()`。泛型不是过度抽象,是**可测性的前提** —— `Buffer` 必须
//! 有一个 `FontSystem` 才能构造,不泛型就没有任何一条断言能在无头机器上跑。

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
        c.insert((P0, 0), 1, 800, vec![CachedRun { col: 0, payload: () }]);
        c.insert((P1, 0), 2, 800, vec![CachedRun { col: 0, payload: () }]);
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
        assert_eq!(c.get((p2, 0)).map(|r| r.hash), Some(12), "P2 拿到了别人的缓存");
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
                CachedRun { col: 0, payload: () },
                CachedRun { col: 3, payload: () },
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
        c.insert((P1, 0), 2, 800, vec![CachedRun { col: 0, payload: () }]);
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
        c.insert((P0, 0), 1, 800, vec![CachedRun { col: 0, payload: () }]);
        c.clear(&mut pool);
        assert!(c.is_empty());
        assert_eq!(pool.len(), 1);
    }
}
```

- [ ] **Step 2: 挂上模块,跑测试确认失败**

在 `crates/mullion-app/src/lib.rs` 的 `pub mod session_pump;` 与 `pub mod shell;`
之间插入一行：

```rust
pub mod shaped_cache;
```

然后：

```bash
cargo test -p mullion-app shaped_cache 2>&1 | grep -E "^error" | head
```

Expected: `cannot find type 'ShapedCache'`、`cannot find function 'plan_row'` 等。

- [ ] **Step 3: 写实现**

把实现插入 `crates/mullion-app/src/shaped_cache.rs` 的模块文档之后、
`#[cfg(test)]` 之前：

```rust
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
```

- [ ] **Step 4: 跑测试**

```bash
cargo test -p mullion-app shaped_cache 2>&1 | grep -E "test result|FAILED|panicked"
```

Expected: `test result: ok.`，10 条全过。

- [ ] **Step 5: 跑 clippy**

```bash
cargo clippy -p mullion-app --all-targets -- -D warnings 2>&1 | tail -20
```

Expected: 无输出。（`Default` 与 `is_empty` 已经补上，clippy 的
`new_without_default` / `len_without_is_empty` 不会响。）

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-app/src/shaped_cache.rs crates/mullion-app/src/lib.rs
git commit -m "feat(app): ShapedCache —— 按 (PaneId,row) 分槽的跨帧整形缓存 (F12)

纯数据、对载荷泛型(测试 T=()、生产 T=Buffer),因此判据能在无 GPU 的
机器上单测。判据函数 plan_row 返回三态枚举而不是 bool:Temporary 与
Reshape 动作相同但缓存副作用相反。

三条容易写错、写错了只有人眼能发现的地方各配一条守护:
- 组字行绝不入缓存(否则 Esc 取消后复用带空洞的 buffer,字永久消失)
- 空行也要写条目(否则空行永远 miss,差分白做)
- 逐出/重整形的旧载荷回池子(否则流式场景每帧新建上千 Buffer,T3)"
```

---

## Task 5: 接进 `TextLayer::prepare_panes`

**Files:**
- Modify: `crates/mullion-app/src/text.rs:213-231`（`TextLayer` 字段）
- Modify: `crates/mullion-app/src/text.rs:263-276`（`TextLayer::new`）
- Modify: `crates/mullion-app/src/text.rs:289-297`（`set_font`）
- Modify: `crates/mullion-app/src/text.rs:333-477`（`prepare_panes`）

这一步没有新单测（`prepare_panes` 要真实 wgpu `Device`/`Queue`）。守护靠
Task 4 的纯模块测试 + Task 6 的运行期计数器 + Task 8 的人工验收。**不要**为了
"看起来有测试"而写一条断言不了任何东西的空壳。

- [ ] **Step 1: 换 `TextLayer` 的字段**

把 `crates/mullion-app/src/text.rs:220` 那一行
（`    buffers: Vec<Buffer>, // 每屏面行一个`）替换为：

```rust
    /// F12:跨帧的整形缓存,按 `(PaneId, row)` 分槽。取代了原来那个
    /// "每帧从头填一遍"的 `Vec<Buffer>`。
    cache: crate::shaped_cache::ShapedCache<Buffer>,
    /// 空闲的 `Buffer` 回收池。缓存逐出、重整形、清空时的旧 buffer 都进这里,
    /// 整形时优先从这里取 —— 每帧新建上千个 `Buffer` 就是陷阱 T3,而且滚动
    /// 场景(每帧每行都变)不回收会比改之前更慢。
    pool: Vec<Buffer>,
    /// 临时槽:IME 组字行的正文 + 拼音串 overlay。**它们绝不进 `cache`**
    /// (理由见 `shaped_cache::plan_row` 的文档)。当池子用,不每帧清空。
    temp: Vec<Buffer>,
```

在 `crates/mullion-app/src/text.rs` 的 `TextLayer::new` 里，把
`            buffers: Vec::new(),` 替换为：

```rust
            cache: crate::shaped_cache::ShapedCache::new(),
            pool: Vec::new(),
            temp: Vec::new(),
```

- [ ] **Step 2: `set_font` 清缓存**

把 `set_font` 的函数体末尾（`self.cell_w = measure_cell_w(...)` 那一行之后）
加上：

```rust
        // F12:换字体族/字号/DPI 会让所有已 shape 的 buffer 的 metrics 整体
        // 作废。这是缓存**唯一的显式失效 hook** —— 别的失效源(内容、SGR、
        // 选区、主题、pane 宽度)全部由行指纹与 `term_w` 自动覆盖,不需要
        // 也不应该在各自的入口处再加 hook。
        //
        // `pool` / `temp` 不必清:整形路径每次都调 `set_metrics`,池里的
        // buffer 不会带着陈旧 metrics 上屏。
        self.cache.clear(&mut self.pool);
```

- [ ] **Step 3: 抽出整形函数**

在 `crates/mullion-app/src/text.rs` 里，`impl TextLayer` **之前**（紧跟
`pane_bounds_ltrb` 之后）插入：

```rust
/// 把一段 run 整形进 `buf`。**整个渲染路径上唯一调用 `shape_until_scroll`
/// 的地方** —— 缓存命中时根本不会走到这里,那正是 F12 的收益。
///
/// `avail`(交给 `Buffer::set_size` 的可用宽度)按「从这一列到 pane 右缘」
/// 给,不是整个 pane 宽度:给多了 cosmic-text 不会截断(我们本来就不靠它
/// 换行,行尾由 `TextBounds` 裁),给少了才会误折行。
///
/// 复用来的 buffer 带着上一次的 metrics,所以每次都要 `set_metrics`
/// (换字号/换 DPI 那一帧不重设,字会按旧行高排)。
#[allow(clippy::too_many_arguments)]
fn shape_run(
    buf: &mut Buffer,
    fs: &mut FontSystem,
    metrics: Metrics,
    spans: &[(String, Color)],
    col: u16,
    term_w: u32,
    cell_w: f32,
    cell_h: f32,
    attrs: Attrs<'_>,
) {
    buf.set_metrics(fs, metrics);
    let avail = term_w
        .saturating_sub((f32::from(col) * cell_w) as u32)
        .max(1) as f32;
    buf.set_size(fs, Some(avail), Some(cell_h));
    let iter = spans.iter().map(|(s, c)| (s.as_str(), attrs.color(*c)));
    buf.set_rich_text(fs, iter, attrs, Shaping::Advanced);
    buf.shape_until_scroll(fs, false);
}

/// 本帧某个 `TextArea` 的 buffer 存在哪。
#[derive(Clone, Copy)]
enum BufSrc {
    /// 在跨帧缓存里:`(键, 这一行第几个 run)`。
    Cached((mullion_core::layout::PaneId, u16), usize),
    /// 在临时槽里(IME 组字)。
    Temp(usize),
}

/// 本帧要画的一段文字:画在哪个 pane 的哪一行哪一列,buffer 在哪。
struct Placement {
    pane_ix: usize,
    row: u16,
    col: u16,
    src: BufSrc,
}
```

- [ ] **Step 4: 重写 `prepare_panes`**

把 `crates/mullion-app/src/text.rs` 里整个 `pub fn prepare_panes(...)`（从
`    /// 为所有 pane 准备文字。` 的文档注释开始，到 `    }` 结束，即原
333–477 行）替换为：

```rust
    /// 为所有 pane 准备文字。每个 pane 用自己的 `term_px` 作原点**和**裁剪框。
    ///
    /// # F12 差分整形
    ///
    /// 第一遍逐 `(PaneId, row)` 查 [`crate::shaped_cache::ShapedCache`]:
    /// 行指纹与 pane 像素宽都没变就**直接复用上一帧 shape 好的 buffer**,
    /// 连 `row_to_runs` 都不调(它每行要建一批 `String`)。
    ///
    /// 改这里之前先读 `shaped_cache::plan_row` 的文档 —— 尤其是"组字行为
    /// 什么绝不能进缓存"。
    ///
    /// 第二遍建 `TextArea`:glyphon 的 `prepare` 要求 buffer 借用活到
    /// `render`,所以两遍不能合成一遍。`left`/`top`/`bounds` 每帧现算,
    /// 因此**拖动分屏、移动 pane 不需要重整形**,只有宽度变了才需要。
    pub fn prepare_panes(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        panes: &[PaneRender<'_>],
        res: Resolution,
        preedit_fg: Rgb,
    ) -> Result<(), glyphon::PrepareError> {
        use crate::shaped_cache::{CachedRun, RowPlan};

        self.viewport.update(queue, res);
        let metrics = grid_metrics(self.font_px, self.cell_h);
        // 族名先克隆到局部:`Attrs` 借的是 `&str`,直接借 `self.family` 的话
        // 下面就没法再 `&mut self.font_system` 了(E0502)。每帧一次短字符串
        // 克隆,相对整形开销可以忽略。
        let family_owned = self.family_name().to_string();
        let attrs = Attrs::new().family(Family::Name(&family_owned));
        // 字段级借用分割:这几个是 `TextLayer` 的不同字段,分别可变借用
        // 合法;写成 `self.xxx` 穿插调用就借不出来了。
        let fs = &mut self.font_system;
        let cache = &mut self.cache;
        let pool = &mut self.pool;
        let temp = &mut self.temp;
        let (cell_w, cell_h) = (self.cell_w, self.cell_h);

        cache.begin_frame();
        let mut plan: Vec<Placement> = Vec::new();
        let mut temp_n = 0usize;
        let (mut hits, mut misses) = (0u64, 0u64);

        for (pane_ix, p) in panes.iter().enumerate() {
            // 缓存键必须是稳定身份。`pane_ix` 是当帧下标,关掉中间一块
            // pane 会让它挪位 —— 拿它当键会张冠李戴。
            let pane_id = p.geom.id;
            let term_w = p.geom.term_px.w;
            for row in 0..p.snap.rows {
                // F126:组字中的拼音串占的列区间只在光标行生效——正文 run 要
                // 在这个区间让路(见 `row_to_runs` 的 `hidden` 参数文档),
                // 不然背景 quad 盖不住排在它后面的文字层,拼音会和原字符的
                // 字形叠在一起。
                let hidden = hidden_span_for_row(p, row);
                let key = (pane_id, row);
                let hash = p.snap.row_hash(row);
                match crate::shaped_cache::plan_row(cache.get(key), hash, term_w, hidden.is_some())
                {
                    RowPlan::Reuse => {
                        hits += 1;
                        cache.touch(key);
                        let cols: Vec<u16> = cache
                            .get(key)
                            .map(|r| r.runs.iter().map(|x| x.col).collect())
                            .unwrap_or_default();
                        for (ix, col) in cols.into_iter().enumerate() {
                            plan.push(Placement {
                                pane_ix,
                                row,
                                col,
                                src: BufSrc::Cached(key, ix),
                            });
                        }
                    }
                    RowPlan::Reshape => {
                        misses += 1;
                        // 先把旧载荷收回池子再整形,否则滚动场景每帧新建
                        // 上千个 `Buffer`(T3),比改之前还慢。
                        cache.recycle_row(key, pool);
                        let mut runs: Vec<CachedRun<Buffer>> = Vec::new();
                        for run in row_to_runs(p.snap.row(row), hidden) {
                            let mut buf = pool.pop().unwrap_or_else(|| Buffer::new(fs, metrics));
                            shape_run(
                                &mut buf, fs, metrics, &run.spans, run.col, term_w, cell_w, cell_h,
                                attrs,
                            );
                            plan.push(Placement {
                                pane_ix,
                                row,
                                col: run.col,
                                src: BufSrc::Cached(key, runs.len()),
                            });
                            runs.push(CachedRun {
                                col: run.col,
                                payload: buf,
                            });
                        }
                        // 空 `runs` 也要写:整行空白的产物就是空集,不写条目
                        // 的话空行永远 miss,而空行是空闲画面的大头。
                        cache.insert(key, hash, term_w, runs);
                    }
                    RowPlan::Temporary => {
                        for run in row_to_runs(p.snap.row(row), hidden) {
                            if temp_n == temp.len() {
                                temp.push(Buffer::new(fs, metrics));
                            }
                            shape_run(
                                &mut temp[temp_n],
                                fs,
                                metrics,
                                &run.spans,
                                run.col,
                                term_w,
                                cell_w,
                                cell_h,
                                attrs,
                            );
                            plan.push(Placement {
                                pane_ix,
                                row,
                                col: run.col,
                                src: BufSrc::Temp(temp_n),
                            });
                            temp_n += 1;
                        }
                    }
                }
            }

            // F126:组字中的拼音串本身。走临时槽,颜色取默认前景色(它盖在
            // 自己铺的默认背景上,不跟随底下那格原本的 SGR 颜色 —— 那格
            // 颜色可能恰好等于背景色,拼音就隐形了)。守卫与
            // `hidden_span_for_row` 内部判据同源(非空 + 光标可见)。
            let preedit_cells = if !p.preedit.is_empty() && p.snap.cursor.visible {
                preedit_layout(p.snap.cols, p.snap.cursor.col, p.preedit)
            } else {
                Vec::new()
            };
            for c in &preedit_cells {
                if temp_n == temp.len() {
                    temp.push(Buffer::new(fs, metrics));
                }
                let spans = [(c.ch.to_string(), to_color(preedit_fg))];
                shape_run(
                    &mut temp[temp_n],
                    fs,
                    metrics,
                    &spans,
                    c.col,
                    term_w,
                    cell_w,
                    cell_h,
                    attrs,
                );
                plan.push(Placement {
                    pane_ix,
                    row: p.snap.cursor.row,
                    col: c.col,
                    src: BufSrc::Temp(temp_n),
                });
                temp_n += 1;
            }
        }

        // 帧末逐出:本帧没访问过的键(pane 关了、行数缩了、切了标签)全删,
        // 载荷回池子。刻意不在 `close_pane` 之类的地方各加清理 hook。
        cache.end_frame(pool);
        crate::diag::count_reshape(hits, misses);

        // 第二遍:建 TextArea,bounds 用**该 pane 的**矩形而不是整窗。
        // `left` 加上 `col × cell_w` —— 这一项就是 CJK 对齐的落点:与
        // `gpu::quads_for` 画底色/光标用的是同一个式子。
        let mut areas: Vec<TextArea> = Vec::with_capacity(plan.len());
        for pl in &plan {
            let Some(p) = panes.get(pl.pane_ix) else {
                continue;
            };
            let buffer = match pl.src {
                BufSrc::Cached(key, ix) => match self.cache.get(key).and_then(|r| r.runs.get(ix)) {
                    Some(run) => &run.payload,
                    None => continue,
                },
                BufSrc::Temp(i) => match self.temp.get(i) {
                    Some(b) => b,
                    None => continue,
                },
            };
            let (left, top, right, bottom) = pane_bounds_ltrb(p.geom.term_px);
            areas.push(TextArea {
                buffer,
                left: p.geom.term_px.x as f32 + f32::from(pl.col) * self.cell_w,
                top: p.geom.term_px.y as f32 + f32::from(pl.row) * self.cell_h,
                scale: 1.0,
                bounds: TextBounds {
                    left,
                    top,
                    right,
                    bottom,
                },
                default_color: glyphon::Color::rgb(
                    self.default_fg.r,
                    self.default_fg.g,
                    self.default_fg.b,
                ),
                custom_glyphs: &[],
            });
        }

        self.renderer.prepare(
            device,
            queue,
            &mut self.font_system,
            &mut self.atlas,
            &self.viewport,
            areas,
            &mut self.swash,
        )
    }
```

**注意 `use` 补齐**：`shape_run` 用到了 `Metrics` 与 `Color`。文件顶部已经
`use glyphon::{Attrs, Buffer, Cache, Family, FontSystem, Metrics, ...}` 与
`use glyphon::Color;`，无需新增；`mullion_core` 在 `BufSrc` 里用了全路径。
若编译报缺，按错误提示补，不要猜。

- [ ] **Step 5: 编译（此时 `count_reshape` 还不存在,预期红）**

```bash
cargo build -p mullion-app 2>&1 | grep -E "^error" | head
```

Expected: `cannot find function 'count_reshape' in module 'crate::diag'`。
其余错误都要在这一步解决掉——**不要**带着别的编译错误进 Task 6。

- [ ] **Step 6: 临时打桩,确认除计数器外全通**

在 `crates/mullion-app/src/diag.rs` 末尾（`#[cfg(test)]` 之前）临时加：

```rust
pub fn count_reshape(_hits: u64, _misses: u64) {}
```

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20
```

Expected: 测试全 ok，clippy 无输出。（Task 6 会把这个桩换成真实现。）

- [ ] **Step 7: 提交**

```bash
git add crates/mullion-app/src/text.rs crates/mullion-app/src/diag.rs
git commit -m "perf(app): prepare_panes 按行指纹跳过未变行的整形 (F12)

原来每帧对所有 pane 的每一行无条件 set_rich_text + shape_until_scroll,
任意一个 pane 的 tmux 状态栏刷新一次就拖着全窗口陪跑一次全量整形
(8 pane ≈18~23ms/帧)。现在逐 (PaneId,row) 查 ShapedCache,指纹与
pane 像素宽都没变就复用上一帧的 Buffer,连 row_to_runs 都不调。

三处刻意为之:
- 缓存键用 p.geom.id 而不是 enumerate 下标(关中间 pane 会挪位)
- 组字行走临时槽,不查也不写缓存(shaped_cache::plan_row 文档)
- 逐出/重整形的旧 Buffer 回 pool,整形时优先 pop(T3)

TextArea 的 left/top/bounds 仍每帧现算,所以拖动分屏不触发重整形。"
```

---

## Task 6: `reshape=hit:/miss:` 运行期计数器

**Files:**
- Modify: `crates/mullion-app/src/diag.rs`（计数器 + `count_reshape` + `take_snapshot`）
- Modify: `crates/mullion-app/src/profile.rs`（`Snapshot` 字段 + `empty()` + `render_line`）

没有它，"判据写错导致永远 miss"这种退化会**静默发生**——画面完全正确，
性能悄悄回到改之前，没有任何人看得出来。

- [ ] **Step 1: 先写会失败的测试**

在 `crates/mullion-app/src/profile.rs` 的 `mod tests` 里，先把 `busy_snapshot`
补上两个新字段（在 `sync_timeouts: 3,` 之后插入一行）：

```rust
            reshape_hit: 900,
            reshape_miss: 100,
```

再在 `mod tests` 末尾追加：

```rust
    /// F12:整形缓存的命中/未命中必须进剖面行。
    ///
    /// 这是"差分整形悄悄退化回全量"的**唯一**运行期守护 —— 判据写错时
    /// 画面完全正确,只有 miss 数会暴露它。没有这一列,退化是静默的。
    ///
    /// 自证会变红:把 `render_line` 里 `reshape=` 那一段删掉。
    #[test]
    fn the_reshape_cache_counts_reach_the_line() {
        let line = render_line(&busy_snapshot()).expect("忙窗口该有一行");
        assert!(line.contains("reshape=hit:900/miss:100"), "没报整形缓存命中率：{line}");
    }

    /// 零命中同样要**显式写出来**:"这个窗口一次都没命中"与"这个版本
    /// 忘了统计"在日志里不能长得一样(与 `skip=0` 同一条纪律)。
    ///
    /// 自证会变红:给 `render_line` 里的 `reshape=` 段加上
    /// `if s.reshape_hit > 0` 之类的条件。
    #[test]
    fn a_zero_reshape_hit_is_printed_rather_than_omitted() {
        let mut s = busy_snapshot();
        s.reshape_hit = 0;
        let line = render_line(&s).expect("忙窗口该有一行");
        assert!(line.contains("reshape=hit:0/"), "零命中被省略了：{line}");
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app profile:: 2>&1 | grep -E "^error|test result" | head
```

Expected: 编译错误 `struct 'Snapshot' has no field named 'reshape_hit'`。

- [ ] **Step 3: 给 `Snapshot` 加字段**

在 `crates/mullion-app/src/profile.rs` 的 `pub struct Snapshot` 里，
`pub sync_timeouts: u64,` 之后插入：

```rust
    /// F12:本窗口内整形缓存命中的行数。
    pub reshape_hit: u64,
    /// F12:未命中(真的跑了一次 shape)的行数。
    ///
    /// **这一对是差分整形唯一的运行期守护**:判据写错导致永远 miss 时,
    /// 画面完全正确、日志一切正常,只有这里的比值会掉下去。
    pub reshape_miss: u64,
```

在 `Snapshot::empty()` 里，`sync_timeouts: 0,` 之后插入：

```rust
            reshape_hit: 0,
            reshape_miss: 0,
```

**不要**把它们加进 `is_idle()`：只有画了帧才会有整形计数，`frames` 已经覆盖了；
加进去等于给空闲判据引入一个恒等于零的多余条件。

- [ ] **Step 4: 加进剖面行**

在 `render_line` 的 `format!` 里，把
`         conn=ok:{}/err:{}/re:{} sftp={} tabs={} panes={} hosts={} mem={}MB",`
这一行替换为：

```rust
         reshape=hit:{}/miss:{} conn=ok:{}/err:{}/re:{} sftp={} tabs={} panes={} hosts={} mem={}MB",
```

并在参数列表里，把 `        s.connects_ok,` 之前插入：

```rust
        s.reshape_hit,
        s.reshape_miss,
```

- [ ] **Step 5: 跑 profile 测试**

```bash
cargo test -p mullion-app profile:: 2>&1 | grep -E "test result|FAILED|panicked"
```

Expected: 全 ok。

- [ ] **Step 6: 把 `diag.rs` 里的桩换成真实现**

删掉 Task 5 Step 6 加的那行空桩，改为——在
`crates/mullion-app/src/diag.rs` 的 `static SYNC_TIMEOUTS: AtomicU64 = AtomicU64::new(0);`
之后插入：

```rust
/// F12 剖面:整形缓存这一窗口命中/未命中了多少行。
static RESHAPE_HIT: AtomicU64 = AtomicU64::new(0);
static RESHAPE_MISS: AtomicU64 = AtomicU64::new(0);
```

在 `pub fn count_sync(...)` 之后插入：

```rust
/// 本帧整形缓存的命中/未命中行数(F12)。两者都为 0(这一帧没画)时直接
/// 返回,免得静止时也在 relaxed 原子上打转。
pub fn count_reshape(hits: u64, misses: u64) {
    if hits == 0 && misses == 0 {
        return;
    }
    RESHAPE_HIT.fetch_add(hits, Ordering::Relaxed);
    RESHAPE_MISS.fetch_add(misses, Ordering::Relaxed);
}
```

在 `take_snapshot` 里，`s.sync_timeouts = SYNC_TIMEOUTS.swap(0, Ordering::Relaxed);`
之后插入：

```rust
    s.reshape_hit = RESHAPE_HIT.swap(0, Ordering::Relaxed);
    s.reshape_miss = RESHAPE_MISS.swap(0, Ordering::Relaxed);
```

- [ ] **Step 7: 全绿检查**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20
cargo fmt --check
```

Expected: 测试全 ok；clippy 无输出；fmt 无输出。

- [ ] **Step 8: 提交**

```bash
git add crates/mullion-app/src/diag.rs crates/mullion-app/src/profile.rs
git commit -m "feat(app): 剖面行报整形缓存命中率 reshape=hit:/miss: (F12)

差分整形唯一的运行期守护:判据写错导致永远 miss 时,画面完全正确、
日志一切正常,只有这一对数字的比值会掉下去 —— 没有它,退化是静默的。

零值显式打印(与 skip=0 同一条纪律):「这窗口一次没命中」与「这版本
忘了统计」在日志里不能长得一样。"
```

---

## Task 7: 文档

**Files:**
- Modify: `spec.md:90`
- Create: `docs/adr-011-row-fingerprint-vs-term-damage.md`
- Modify: `docs/gui-render-gotchas.md`（`## alacritty_terminal / 快照` 一节末尾）

- [ ] **Step 1: 改 `spec.md` 的 F12**

把 `spec.md:90` 那一行：

```
| F12 | 差分渲染：用 `Term::damage()` 只重画脏行 | P0 | 单测：只改一行后，damage 只含那一行 |
```

替换为：

```
| F12 | 差分整形：按行内容指纹跳过未变行的文本整形（不用 `Term::damage()`，理由见 [adr-011](docs/adr-011-row-fingerprint-vs-term-damage.md)） | P0 | 单测：只改一行后，脏行集合只含那一行 |
```

- [ ] **Step 2: 写 ADR-011**

创建 `docs/adr-011-row-fingerprint-vs-term-damage.md`：

```markdown
# ADR-011：差分整形的判脏用行内容指纹，不用 `Term::damage()`

- 日期：2026-08-24
- 状态：已采纳
- 关联：F12（P0）、N1/N2、[adr-001](adr-001-glyph-rendering.md)、领域陷阱 T3
- 设计文档：`docs/superpowers/specs/2026-08-24-f12-row-fingerprint-diff-shaping-design.md`

## 背景

`TextLayer::prepare_panes` 原来每帧对所有 pane 的每一行无条件重新整形。
作者 2026-08-21 的量化脚手架实测 8 pane ≈ 18~23ms/帧；2026-08-24 的 Windows
实机对照实验显示，每加一个挂 tmux + Claude Code 的 pane，进程 CPU 跳 0.6~1.6
个点，而加两个不接 tmux 的 pane 只多 0.3%——差别不在内容量（都空闲），
而在 tmux 空闲时仍周期性吐极小的字节，每一次都触发一轮**全窗口**整形。
同机 xshell 连同样的节点跑同样的 tmux + Claude Code，常驻 CPU 0.2%。

`spec.md` 的 F12 原文点名了 `Term::damage()`，alacritty_terminal 0.26.0 也确实
提供 `Term::damage() -> TermDamage` + `reset_damage()`。

## 决策

**不用 `Term::damage()`。** 在 `Emulator::snapshot()` 里给每行算一个覆盖
`SnapCell` 全部字段的 FNV-1a 指纹，渲染层拿它跟上一帧比。

## 理由

`Term::damage()` 只知道 alacritty 自己改过的格子。而能改变"一行最终长什么样"
的来源至少有七个：

1. `Term` 内容变化 —— damage 知道
2. **选区反色** —— `text::row_to_spans` 把选中格的文字色从 `fg` 换成 `bg`；
   alacritty 的 `term/mod.rs:450-452` **明说** selection 不在 damage 里
3. IME preedit 让路（`hidden_span_for_row`）
4. **主题换色** —— `Emulator::set_default_colors` 改的是 `palette::resolve`
   解析出的 fg/bg，alacritty 完全不知道
5. 字体族 / 字号变化（`TextLayer::set_font` 改 metrics）
6. DPI 缩放变化
7. pane 像素宽度变化（进 `Buffer::set_size` 的 `avail`）

以 damage 为基础就必须**逐个枚举**这七个来源去求并集。漏掉任何一个，症状是
**屏幕上留着一行陈旧的字**——编译不报错、测试不报错、日志不报错，只有人眼
能发现，正落在 `CLAUDE.md` §「你无法验证的东西」那一类。而"列举式门控在加档
时必然漏"在本项目已经踩中过三次。

行指纹把判据从"列举所有会变的原因"翻转成"直接看结果变没变"：1/2/4 已经烘进
快照字段，自动覆盖；5/6 由 `set_font` 里一次整体 `clear` 覆盖；7 由缓存条目
自带的 `term_w` 比对覆盖；3 单独处理（只有一行，且**绝不写缓存**）。

**失败方向也反过来了**：指纹方案的最坏情况是多整形一次（画面永远正确），
damage 方案的最坏情况是少画（静默陈旧）。

## 代价

- 每帧多算约 0.1~0.3ms 的哈希（对照要省掉的 18~23ms）。
- 需要修订 `spec.md` 里 F12 的措辞。
- 指纹的字段覆盖面成了新的关键不变量，由两层机械守护看着：存量字段靠
  `snapshot.rs` 里六条逐字段测试，增量字段靠 `hash_row` 函数体内的穷尽解构
  （给 `SnapCell` 加字段即编译报错）。

## 被否的备选

- **A. damage 驱动**（`spec.md` 字面）：风险如上。
- **C. 两者都做**（damage 决定重建哪些**快照行** + 指纹决定重整形）：收益能叠加
  （还能省掉每帧那份 `cols×rows` 的 `Vec` 分配），但复杂度翻倍，且 A 的静默
  风险原样保留。留作后续——真要做的时候，指纹仍是最终判据，damage 只作为
  "少建几行快照"的优化，不改变失效判定的构造式性质。

## 重新考虑的触发条件

若剖面显示 `Emulator::snapshot()` 每帧重建整份 `cols×rows` 成为新的头号开销
（`pump` 阶段的 p95 压过 `text_prepare`），就该重开备选 C。
```

- [ ] **Step 3: 补 `gui-render-gotchas.md`**

在 `docs/gui-render-gotchas.md` 的 `## alacritty_terminal / 快照` 一节**末尾**
（下一个 `## ` 之前）追加：

```markdown
### 行指纹的字段覆盖面必须与整形读到的字段同源（F12）

**症状**：屏幕上留着一行陈旧的字。编译不报错、测试不报错、日志不报错，
只有人眼能发现，而且多半只在特定操作后偶发（比如"换了主题但有一行没跟着变"）。

**规则**：`snapshot::hash_row` 喂进哈希的字段，必须**恰好等于**
`text::row_to_runs` / `row_to_spans` 真正读到的字段。少喂一个，那一类变化
就静默不重画。当前是 `SnapCell` 的全部六个字段。

**守护（两层，缺一不可）**：
- 存量字段：`snapshot.rs` 的 `mod tests` 里六条 `a_changed_*_changes_the_row_hash`，
  一条对一个字段。
- 增量字段：`hash_row` 函数体内的**穷尽解构**
  `let SnapCell { ch, fg, bg, width, spacer, selected } = *cell;`。给 `SnapCell`
  加字段时这里会当场编译报错，强迫作者对"进不进哈希"表态。**不要**把它改成
  `cell.ch` 那种点号取字段的写法——那样加字段就没有任何提示了。

同一条纪律的另一半在 app 侧：组字行**绝不能**写进 `ShapedCache`
（见 `shaped_cache::plan_row` 的文档）。写进去的话，用户按 Esc 取消组字后
指纹没变、缓存命中，会复用那份**带拼音空洞的** buffer——被盖住的几个字
永久消失。
```

- [ ] **Step 4: 检查文档里的相对链接**

```bash
grep -n "adr-011" spec.md docs/gui-render-gotchas.md docs/adr-011-row-fingerprint-vs-term-damage.md
ls docs/adr-011-row-fingerprint-vs-term-damage.md
```

Expected: 文件存在，`spec.md` 里有一处引用。

- [ ] **Step 5: 提交**

```bash
git add spec.md docs/adr-011-row-fingerprint-vs-term-damage.md docs/gui-render-gotchas.md
git commit -m "docs(f12): F12 措辞改行指纹 + ADR-011 + gotchas 补六字段同源一条

ADR-011 记的是「手上有现成的 Term::damage() 为什么不用」:枚举式失效源
(至少七个,漏一个就静默陈旧)vs 构造式覆盖(最坏情况只是多整形一次)。
半年后一定会被重新问起,理由比结论值钱。"
```

---

## Task 8: 量化前后对比 + 发版

**Files:**
- Modify: `Cargo.toml:12`（版本 0.1.66 → 0.1.67）

交付约定（项目 `CLAUDE.md`）：改动落到 `mullion-app` 且要拿去实机验，
就一条龙做完，不要停下来问。

- [ ] **Step 1: 跑量化脚手架,记下改后的数字**

```bash
cargo test -p mullion-app --release shaping_cost -- --ignored --nocapture 2>&1 | tail -30
```

把输出贴进 PR/Release notes。**注意**：这个脚手架是照着**旧的**
`prepare_panes` 内层循环抄的（见它自己的文档：「那边改了这里要跟着改」），
它量的是"每帧全部重整形"的成本，也就是**改动前的基线**。改完之后它的绝对值
不该有大变化——真正的收益要看 Step 2 的 `reshape=hit:/miss:`，以及实机 CPU。
不要拿它的数字冒充"F12 生效了"。

- [ ] **Step 2: 最终全绿**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
cargo fmt --check
```

Expected: 测试全 ok；clippy 与 fmt 都无输出。

- [ ] **Step 3: 升版本号**

把 `Cargo.toml:12` 的 `version = "0.1.66"` 改成 `version = "0.1.67"`，然后：

```bash
cargo build --workspace 2>&1 | tail -3   # 让 Cargo.lock 跟上
git add Cargo.toml Cargo.lock
git commit -m "chore: 版本 0.1.67(F12 按行指纹差分整形)"
```

- [ ] **Step 4: 交叉编译 + 签名 + 发 Release**

调用 release skill——**别凭记忆做**，每一步都有漏了也不报错的坑
（代理设置、objdump 验收、tag 必须在本地 HEAD 上建）：

```
Skill: release-windows
```

- [ ] **Step 5: 人工验收清单（写进 Release notes）**

这些是**你无法自动验证**的，必须由人在 Windows 11 实机上确认：

1. **主要目标**：4 分屏 / 4 节点 / 各挂 tmux + Claude Code（停在等待输入），
   静置 1 分钟，任务管理器读 mullion 进程 CPU。**基线 6.2%，对照 xshell 0.2%。**
2. **`reshape=hit:/miss:`**：静置时打开 `%APPDATA%\mullion\config\mullion.log`，
   `grep profile`，看新那一列。空闲画面命中率应当**接近 100%**；
   miss 常年很高说明判据写错了（差分白做，但画面正常，只有这里看得出来）。
3. **切主题**：整屏颜色必须**立刻**跟走，不能有任何一行留着旧色
   （验 ADR-011 第 4 条失效源确实被指纹覆盖了）。
4. **划选**：拖动鼠标划选，选区里的文字要实时反色，松开后不残留
   （验第 2 条失效源）。
5. **输入法**：中文组字时拼音串正常显示、不与原字符字形叠在一起；
   **按 Esc 取消组字后，被拼音盖住的那几个字必须回来**（这条专门验
   `plan_row` 的 `Temporary` 分支——写错了的话字会永久消失）。
6. **换字体族 / 字号 / 拖到不同 DPI 的显示器**：整屏立刻重排，无残影。
7. **拖动分屏分界线**：宽度变化后文字重新排版正确（验 `term_w` 判据）；
   只上下拖（宽度不变）时不该有任何视觉抖动。
8. **关掉中间一块 pane**：剩下的 pane 内容不能互换、不能串行
   （验缓存键是 `PaneId` 而不是下标）。
9. **滚动回溯**（PgUp/滚轮）：内容跟着滚，不留陈旧行（验 F17 换算同源）。
10. **满屏 CJK 静置**：CPU 与 ASCII 静置应当同量级（都接近 0），
    因为两者都全命中。

---

## 自审记录

**Spec 覆盖**：设计文档九节逐条对过——
§3 决策 → ADR-011（Task 7）；§4.1 指纹 → Task 1+2；§4.2 缓存 → Task 4+5；
§4.3 稳定键 → Task 4 的 `the_cache_is_keyed_by_pane_id_not_by_frame_index`
+ Task 5 用 `p.geom.id`；§5 数据流 → Task 5；§6 测试 → Task 1/2/4 的测试
+ Task 8 Step 5 的人工清单；§7 度量 → Task 6 + Task 8 Step 1；
§8 风险 → 已分别落到 `plan_row` 文档（碰撞自愈）、`end_frame`（内存）、
`reshape=` 计数（静默退化）；§9 连带文档 → Task 7。**无遗漏。**

**类型一致性**：`hash_row(&[SnapCell]) -> u64`（Task 1）→
`GridSnapshot::new` 调用它、`row_hash(u16) -> u64` 访问器（Task 2）→
`p.snap.row_hash(row)` 供 `plan_row` 使用（Task 5）。
`plan_row<T>(Option<&CachedRow<T>>, u64, u32, bool) -> RowPlan` 在 Task 4
定义、Task 5 调用，签名一致。`ShapedCache::{begin_frame, get, touch, insert,
recycle_row, end_frame, clear, len, is_empty}` 在 Task 4 定义，Task 5 用到了
前六个 + Task 5 Step 2 用 `clear`，全部对得上。
`count_reshape(u64, u64)` 在 Task 5 打桩、Task 6 落实，签名一致。

**已知不确定项（实现时须核对，不要凭记忆写）**：
- Task 2 Step 6 里 `Emulator::with_history` / `scroll_display` 的确切签名。
- Task 5 里 glyphon `Buffer::set_size` / `set_rich_text` 的参数形状（本项目
  的 API 漂移条款：编译失败先看错误提示的实际签名）。
