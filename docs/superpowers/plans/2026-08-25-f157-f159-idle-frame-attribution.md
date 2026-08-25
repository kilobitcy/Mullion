# F157–F159 空闲帧归因 + 整帧指纹 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让空闲的 mullion 不再每秒无条件提交 60 帧 GPU，并在剖面行里一次性问出「到底是谁在每帧置脏」。

**Architecture:** 三件事同版落地。F157 在**我们这一侧**埋归因（egui 的 `repaint_causes()` 对自动重绘无效，理由见设计文档 §1.3）；F158 摘掉 launcher 态「没连东西就永远脏」的列举式兜底；F159 补上一层构造式兜底——整帧指纹跟上一帧一样就不提交 GPU。F158 单独上会让 `ui_dirty`（80 个置脏点 : 1 个清脏点）成为唯一判据，漏标一处就是「点了没反应」且全程静默，所以**必须**和 F159 一起。

**Tech Stack:** Rust / winit 0.30 / wgpu 23 / egui 0.30 / glyphon 0.7。全部改动落在 `mullion-app`。

**设计文档：** `docs/superpowers/specs/2026-08-25-f157-f159-idle-frame-attribution-design.md`（已批准）

---

## 读之前必须知道的四件事

1. **领域陷阱 T3**：帧路径上不许分配、不许加锁、不许格式化。本计划新增的所有采集点都只做 relaxed 原子加法。
2. **领域陷阱 T7**：事件循环的三个分支都必须显式复位 `ControlFlow`。本计划**不动**那段代码。
3. **`MEMORY.md` 记录的恒绿模式**：本仓大量守护测试用「读自己的源码切片」实现，历史上出过多次恒绿。本计划里每一条源码切片测试都**必须**先断言「切干净了」（`prod.len() < src.len()`），并且每条测试的文档注释都要写出**自证会变红的那个变异**。
4. **`f32` 一律走 `to_bits()`**，绝不用 `==` 比较，也绝不把 `f32` 喂进 `derive(PartialEq)`。

---

## 文件结构

| 文件 | 责任 | 本计划里做什么 |
|---|---|---|
| `crates/mullion-app/src/profile.rs` | 剖面的**纯数据结构与纯函数**（零 IO / 零 UI） | 新增 `RepaintBucket` + `repaint_bucket()`；`Snapshot` 加 12 个字段；`render_line` 加四段 |
| `crates/mullion-app/src/diag.rs` | 运行期采集（原子计数器 + 5 秒周期线程） | 新增 8 个计数器 + `ui_dirty` 无锁归因表 + 采集函数 |
| `crates/mullion-app/src/frame_fp.rs` | **新建**。整帧指纹，纯函数、零 GPU、零 IO | 全部内容 |
| `crates/mullion-app/src/text.rs` | 文字层 | 新增 `TextLayer::style_key()` 一个方法 |
| `crates/mullion-app/src/app.rs` | 事件循环 + `render_frame`（唯一允许知道其余各层的地方） | `mark_ui_dirty!` 宏 + 80 处置脏点替换 + 摘 launcher 兜底 + `user_event_marks_dirty` + 指纹接线 |
| `crates/mullion-app/src/lib.rs` | 模块表 | 加一行 `pub mod frame_fp;` |
| `spec.md` | 需求编号 | 新增 F157 / F158 / F159 三条 |
| `Cargo.toml` | 版本 | 0.1.67 → 0.1.68 |

**不新建 crate、不动依赖方向。** `frame_fp` 吃的是 `egui::ClippedPrimitive` 与 `crate::gpu::PaneRender`，两者都在 `mullion-app` 内，所以它属于 app，不下沉。

---

## Task 1: `repaint_bucket` —— `repaint_delay` 的三分桶

**Files:**
- Modify: `crates/mullion-app/src/profile.rs`（在 `fmt_us` 之后、`Snapshot` 定义之前插入）
- Test: `crates/mullion-app/src/profile.rs` 的 `mod tests`

**为什么单独一个 Task：** 这是本切片唯一一个可以完全脱离一切上下文单测的判据，而它写错的症状是「剖面里『egui 一次都没要过重绘』和『egui 每帧都要、只是可以等 10 秒』长得一模一样」——归因错了比没有更糟，会把人带去改错地方。

- [ ] **Step 1: 写会失败的测试**

在 `crates/mullion-app/src/profile.rs` 的 `mod tests` 末尾（最后一个 `}` 之前）加：

```rust
    /// F157:`repaint_delay` 的三分桶。**`MAX` 必须与「很大的有限值」分开**。
    ///
    /// `Duration::MAX` 的语义是「egui 不需要重绘」,一个很大的有限值的语义是
    /// 「egui 要重绘,只是可以等」。归成一类的话,剖面里「空闲时 egui 一次都
    /// 没要过重绘」(健康)和「空闲时 egui 每帧都要重绘,只是可以等很久」
    /// (本切片要查的那个病)长得一模一样。
    ///
    /// 自证会变红:把 `repaint_bucket` 的判据改成
    /// `if d >= std::time::Duration::from_secs(1) { Max }`。
    #[test]
    fn a_huge_finite_repaint_delay_is_not_the_same_as_never() {
        use std::time::Duration;
        assert_eq!(repaint_bucket(Duration::ZERO), RepaintBucket::Zero);
        assert_eq!(repaint_bucket(Duration::from_nanos(1)), RepaintBucket::Finite);
        assert_eq!(repaint_bucket(Duration::from_millis(16)), RepaintBucket::Finite);
        assert_eq!(
            repaint_bucket(Duration::from_secs(86_400)),
            RepaintBucket::Finite,
            "一天之后要重绘 ≠ 永远不需要重绘"
        );
        assert_eq!(repaint_bucket(Duration::MAX), RepaintBucket::Max);
    }
```

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test -p mullion-app --lib profile::tests::a_huge_finite 2>&1 | tail -20`
Expected: 编译失败，`cannot find function repaint_bucket in this scope`

- [ ] **Step 3: 写实现**

在 `crates/mullion-app/src/profile.rs` 里 `pub fn fmt_us` 的定义之后、`/// 一个 5 秒窗口里采到的全部东西。` 之前插入：

```rust
/// `repaint_delay` 落在哪个桶（F157）。
///
/// 三分而不是二分:`Duration::MAX` 与「很大的有限值」**必须分开**。前者是
/// 「egui 不需要重绘」,后者是「egui 要重绘,只是可以等」——归成一类的话,
/// 剖面里「空闲时 egui 一次都没要过重绘」和「空闲时 egui 每帧都要」长得一样,
/// 而分辨这两者正是 F157 存在的全部理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepaintBucket {
    /// `Duration::ZERO` —— egui 要求**立刻**再来一帧。空闲时出现这一档,
    /// 说明 `InputState::wants_repaint_after` 的第一条判据成立
    /// (有未处理的指针动作 / 滚动增量 / 输入事件)。
    Zero,
    /// 有限非零 —— 动画、tooltip 延时之类,可以等。
    Finite,
    /// `Duration::MAX` —— egui 不需要重绘。**真空闲时本该恒是这一档**。
    Max,
}

/// 把一个 `repaint_delay` 归进 [`RepaintBucket`]。纯函数,可单测。
pub fn repaint_bucket(d: std::time::Duration) -> RepaintBucket {
    // `MAX` 先判:它不是零,所以顺序对结果无影响,但先写它是为了让
    // 「MAX 是独立一档」这件事在代码里一眼可见。
    if d == std::time::Duration::MAX {
        RepaintBucket::Max
    } else if d.is_zero() {
        RepaintBucket::Zero
    } else {
        RepaintBucket::Finite
    }
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib profile:: 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok.`

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/profile.rs
git commit -m "feat(profile): repaint_delay 三分桶,MAX 与有限值不许混为一谈 (F157)"
```

---

## Task 2: 剖面新增四段（wake / rr / dirty / egui_ev / rdelay / fp）

**Files:**
- Modify: `crates/mullion-app/src/profile.rs`（`Snapshot` 结构体、`Snapshot::empty`、`render_line`）
- Test: `crates/mullion-app/src/profile.rs` 的 `mod tests`

**注意：** 本 Task 只做「纯数据层 + 渲染」。计数器与采集函数在 Task 3/4，接线在 Task 4/8。先把要打印的东西定死，后面的 Task 才有可断言的目标。

**`is_idle()` 一个字都不许动。** 新增的这些字段在空闲时恰恰非零（那正是要查的东西），进了 `is_idle` 判据就等于「空闲的 mullion 每 5 秒写一次盘」，笔记本硬盘永远睡不下去——这是布局落盘已经踩过一次的坑。

- [ ] **Step 1: 写会失败的测试**

在 `crates/mullion-app/src/profile.rs` 的 `mod tests` 里，把现有的 `fn busy_snapshot()` 整个替换成（新增六行字段）：

```rust
    fn busy_snapshot() -> Snapshot {
        let mut s = Snapshot {
            window_ms: 5_000,
            frames: 300,
            presents: 298,
            skipped: 2,
            inbound_bytes: 1_024_000,
            redraw_terminal: 200,
            redraw_ui: 80,
            redraw_both: 20,
            throttled: 40,
            keys: 12,
            connects_ok: 1,
            sftp_ops: 3,
            sync_blocks: 12,
            sync_timeouts: 3,
            reshape_hit: 900,
            reshape_miss: 100,
            fp_hit: 250,
            fp_miss: 50,
            wakes: 340,
            rr_sched: 40,
            rr_evt: 7,
            dirty_sites: vec![(8365, 300), (7191, 2)],
            dirty_other: 5,
            egui_events: 9,
            egui_event_frames: 4,
            rdelay_zero: 300,
            rdelay_finite: 0,
            rdelay_max: 0,
            tabs: 2,
            panes: 3,
            hosts: 2,
            mem_process_mb: 180,
            ..Snapshot::empty()
        };
        s.frame_us[bucket_of(8_000)] = 300;
        s
    }
```

再在 `mod tests` 末尾加四条测试：

```rust
    /// F157:四段归因必须原样进剖面行。
    ///
    /// 这一行是本切片**唯一**的产出——下一版改什么完全取决于它。段名或
    /// 分隔符写错了,人还是能读,但 `findstr` 拉不出时间序列。
    ///
    /// 自证会变红:把 `render_line` 里 `wake=` 那一整段删掉。
    #[test]
    fn the_frame_loop_attribution_reaches_the_line() {
        let line = render_line(&busy_snapshot()).expect("忙窗口该有一行");
        assert!(line.contains("wake=340x"), "没报唤醒次数:{line}");
        assert!(line.contains("rr=sched:40,evt:7"), "没报主动请求重绘的来源:{line}");
        assert!(line.contains("egui_ev=9x/f:4"), "没报喂给 egui 的事件数:{line}");
        assert!(
            line.contains("rdelay=z:300/f:0/m:0"),
            "没报 repaint_delay 分桶:{line}"
        );
        assert!(line.contains("fp=hit:250/miss:50"), "没报整帧指纹命中率:{line}");
    }

    /// 置脏点按次数**倒序**取前三,溢出的槽位报成 `other:N`。
    ///
    /// 倒序是重点:空闲时只有一处每帧都在置脏,它必须排在第一位;按行号
    /// 排序的话它会被一堆各来过一次的启动期置脏点埋掉。
    ///
    /// 自证会变红:把 `render_line` 里的 `Reverse` 去掉。
    #[test]
    fn the_dirty_sites_are_ranked_by_how_often_they_fire() {
        let mut s = busy_snapshot();
        s.dirty_sites = vec![(100, 1), (8365, 300), (200, 2), (300, 3)];
        s.dirty_other = 0;
        let line = render_line(&s).expect("忙窗口该有一行");
        assert!(
            line.contains("dirty=8365:300,300:3,200:2"),
            "置脏点没按次数倒序取前三:{line}"
        );
        assert!(!line.contains("100:1"), "第四名不该出现在行里:{line}");
    }

    /// 归因表溢出必须**说出来**。8 个槽位用完之后落进 `other`,不报的话
    /// 「这几处就是全部」与「还有一堆没槽位」在日志里长得一样。
    ///
    /// 自证会变红:把 `render_line` 里那句 `if s.dirty_other > 0` 的分支删掉。
    #[test]
    fn an_overflowing_attribution_table_says_so() {
        let mut s = busy_snapshot();
        s.dirty_sites = vec![(8365, 300)];
        s.dirty_other = 17;
        let line = render_line(&s).expect("忙窗口该有一行");
        assert!(line.contains("dirty=8365:300,other:17"), "溢出没报:{line}");
    }

    /// 一次置脏都没采到时报 `dirty=-`,**不是空字符串**。
    ///
    /// 人工验收清单第 3 条专门看这个:`dirty=` 后面什么都没有 = 埋点根本
    /// 没接上(F12 同款陷阱:埋点白埋时画面完全正常)。留成空串的话它跟
    /// 「这个窗口确实一次没置脏」长得一样,一整趟实机往返就白跑了。
    ///
    /// 自证会变红:把那个 `-` 占位改成空字符串。
    #[test]
    fn an_empty_attribution_table_is_told_apart_from_a_missing_one() {
        let mut s = busy_snapshot();
        s.dirty_sites = Vec::new();
        s.dirty_other = 0;
        let line = render_line(&s).expect("忙窗口该有一行");
        assert!(line.contains("dirty=- "), "空归因表没有占位符:{line}");
    }

    /// F157 的四段**一律不进 `is_idle`**。
    ///
    /// 它们在空闲时恰恰非零(那正是要查的东西),进了判据就等于「空闲的
    /// mullion 每 5 秒写一次盘」,笔记本硬盘永远睡不下去——这是布局落盘
    /// 已经踩过一次的坑,与 F12 的 `reshape_*` 处理一致。
    ///
    /// 自证会变红:往 `is_idle` 的条件里加上 `&& self.wakes == 0`。
    #[test]
    fn the_new_attribution_counters_do_not_count_as_activity() {
        let parked = Snapshot {
            window_ms: 5_000,
            wakes: 2_000,
            rr_sched: 1_700,
            rr_evt: 3,
            dirty_sites: vec![(8365, 300)],
            dirty_other: 1,
            egui_events: 5,
            egui_event_frames: 2,
            rdelay_zero: 300,
            fp_hit: 300,
            fp_miss: 1,
            ..Snapshot::empty()
        };
        assert!(
            parked.is_idle(),
            "光有归因数据就被算成了活动 —— 硬盘会永远醒着"
        );
    }
```

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test -p mullion-app --lib profile:: 2>&1 | tail -25`
Expected: 编译失败，`struct Snapshot has no field named fp_hit`

- [ ] **Step 3: 写实现（三处）**

**3a.** `crates/mullion-app/src/profile.rs`，在 `Snapshot` 结构体里 `pub reshape_miss: u64,` 之后、`pub tabs: u64,` 之前插入：

```rust
    /// F159:本窗口内整帧指纹命中(没提交 GPU)的帧数。
    pub fp_hit: u64,
    /// F159:未命中(真的画了一帧)的帧数。
    ///
    /// **这一对是整帧指纹唯一的运行期守护**:判据写错导致永远 miss 时,
    /// 画面完全正确、日志一切正常、性能悄悄回到改之前,只有这里的比值会掉。
    pub fp_miss: u64,
    /// F157:本窗口收到了多少次 `RedrawRequested`(唤醒率的直接读数)。
    ///
    /// **提成一等指标是有意的**:整帧指纹会把 CPU 压下去,从而**掩盖**
    /// 「唤醒 400/s」这个真根因,让下一版失去动力。
    pub wakes: u64,
    /// F157:我们**主动**调 `request_redraw` 的次数——`about_to_wait` 到点补画。
    pub rr_sched: u64,
    /// F157:我们主动调 `request_redraw` 的次数——因窗口事件 / 后台唤醒 / UI 请求。
    ///
    /// `wakes` 与 `rr_sched + rr_evt` 的差值不为零是**正常**的:多次
    /// `request_redraw` 会被 winit 合并成一次 `RedrawRequested`,OS 也会主动发
    /// (窗口被遮挡后暴露)。差值本身就是信息,**不试图把它归成精确的三类**
    /// ——那需要一个「最近一次请求来源」的单槽标记,而合并会让它系统性失真。
    /// 宁可报两个诚实的数,不报一个精确但错的分类。
    pub rr_evt: u64,
    /// F157:`ui_dirty` 被置真的来源(源码行号, 次数)。全部置脏点都在 `app.rs`,
    /// 所以只带行号不带文件名(见 `diag::note_ui_dirty`)。
    pub dirty_sites: Vec<(u32, u64)>,
    /// F157:归因表的 8 个槽位用完之后,落在外面的置脏次数。
    pub dirty_other: u64,
    /// F157:这一窗口喂给 egui 的输入事件总数。
    pub egui_events: u64,
    /// F157:其中有多少**帧**带着至少一个事件。
    ///
    /// 与 `egui_events` 分开报:egui 的 `wants_repaint_after` 只要本趟
    /// `events` 非空就返回 `Duration::ZERO`,所以「有几帧带着事件」才是
    /// 跟 `rdelay_zero` 直接对得上的那个数。
    pub egui_event_frames: u64,
    /// F157:`repaint_delay == Duration::ZERO` 的帧数。
    pub rdelay_zero: u64,
    /// F157:`repaint_delay` 有限非零的帧数。
    pub rdelay_finite: u64,
    /// F157:`repaint_delay == Duration::MAX`(egui 不需要重绘)的帧数。
    /// **真空闲时本该是全部**。
    pub rdelay_max: u64,
```

**3b.** 同文件 `impl Snapshot` 的 `empty()` 里，在 `reshape_miss: 0,` 之后、`tabs: 0,` 之前插入：

```rust
            fp_hit: 0,
            fp_miss: 0,
            wakes: 0,
            rr_sched: 0,
            rr_evt: 0,
            dirty_sites: Vec::new(),
            dirty_other: 0,
            egui_events: 0,
            egui_event_frames: 0,
            rdelay_zero: 0,
            rdelay_finite: 0,
            rdelay_max: 0,
```

**3c.** 同文件 `render_line`，在 `let stage_part = ...;` 之后、`Some(format!(` 之前插入置脏段的组装：

```rust
    // F157:置脏点按次数倒序取前三。倒序是重点——空闲时只有一处每帧都在
    // 置脏,按行号排的话它会被一堆各来过一次的启动期置脏点埋掉。
    let mut sites = s.dirty_sites.clone();
    sites.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    sites.truncate(3);
    let mut dirty_parts: Vec<String> = sites.iter().map(|(l, n)| format!("{l}:{n}")).collect();
    if s.dirty_other > 0 {
        dirty_parts.push(format!("other:{}", s.dirty_other));
    }
    // 空表报 `-` 而不是空串:人工验收清单第 3 条专门看这个,`dirty=` 后面
    // 什么都没有 = 埋点根本没接上,而那和「这窗口确实一次没置脏」必须
    // 在日志里长得不一样。
    let dirty_part = if dirty_parts.is_empty() {
        "-".to_string()
    } else {
        dirty_parts.join(",")
    };
```

然后把 `Some(format!(` 的格式串与参数表整体替换成（**只在 `reshape=` 段之后插入新的一整段，其余原样**）：

```rust
    Some(format!(
        "profile {:.1}s frame={}x/p50={}/p95={}/max={} present={} skip={} throttle={} \
         redraw=term:{}/ui:{}/both:{} 同步块={}x/超时={}x in={} key={}x/echo={}x/p95={} {} \
         reshape=hit:{}/miss:{} fp=hit:{}/miss:{} wake={}x/rr=sched:{},evt:{} dirty={} \
         egui_ev={}x/f:{} rdelay=z:{}/f:{}/m:{} \
         conn=ok:{}/err:{}/re:{} sftp={} tabs={} panes={} hosts={} mem={}MB",
        secs,
        s.frames,
        fmt_us(quantile_us(&s.frame_us, 0.5)),
        fmt_us(quantile_us(&s.frame_us, 0.95)),
        fmt_us(quantile_us(&s.frame_us, 1.0)),
        s.presents,
        s.skipped,
        s.throttled,
        s.redraw_terminal,
        s.redraw_ui,
        s.redraw_both,
        s.sync_blocks,
        s.sync_timeouts,
        rate,
        s.keys,
        total(&s.echo_us),
        fmt_us(quantile_us(&s.echo_us, 0.95)),
        stage_part,
        s.reshape_hit,
        s.reshape_miss,
        s.fp_hit,
        s.fp_miss,
        s.wakes,
        s.rr_sched,
        s.rr_evt,
        dirty_part,
        s.egui_events,
        s.egui_event_frames,
        s.rdelay_zero,
        s.rdelay_finite,
        s.rdelay_max,
        s.connects_ok,
        s.connects_err,
        s.reconnects,
        s.sftp_ops,
        s.tabs,
        s.panes,
        s.hosts,
        s.mem_process_mb,
    ))
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib profile:: 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: `test result: ok.`（含新增的 5 条）

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/profile.rs
git commit -m "feat(profile): 剖面加 wake/rr/dirty/egui_ev/rdelay/fp 六段 (F157/F159)"
```

---

## Task 3: `mark_ui_dirty!` 宏 + `ui_dirty` 无锁归因表

**Files:**
- Modify: `crates/mullion-app/src/diag.rs`（计数器 + `note_ui_dirty` + `take_snapshot`）
- Modify: `crates/mullion-app/src/app.rs`（宏定义 + 80 处机械替换）
- Test: `crates/mullion-app/src/diag.rs` 的 `mod tests`、`crates/mullion-app/src/app.rs` 的 `mod tests`

**为什么是宏而不是 `App` 的方法：** 有些置脏点位于 `self.active` 的可变借用作用域里（`app.rs:7188` 一带的 egui 事件分流），那里调不了任何 `&mut self` 方法（E0499）。宏展开成一句普通的字段赋值，两种上下文都能用；而且 `line!()` 在 `macro_rules!` 体内展开成**调用点**的行号，正是归因要的东西。

- [ ] **Step 1: 写会失败的测试（两处）**

**1a.** `crates/mullion-app/src/diag.rs` 的 `mod tests` 末尾加：

```rust
    /// F157:归因表在槽位够用时逐行分开计,用完之后落进 `other`。
    ///
    /// 表写错的症状是「剖面里有一行数字,但它指的不是那个地方」——归因错了
    /// 比没有更糟,会把人带去改错地方。
    ///
    /// 用**自己的**表实例而不是进程级 static:并行 runner 下别的测试会间接
    /// 调到 `note_ui_dirty`,共享 static 会让断言概率性假红(与本文件
    /// `StageClock` 给测试单开实例是同一条理由)。
    ///
    /// 自证会变红:把 `DirtyTable::note` 里的线性扫描去掉,永远走 `other`。
    #[test]
    fn the_attribution_table_keeps_each_line_apart_until_it_runs_out_of_slots() {
        let t = DirtyTable::new();
        for _ in 0..300 {
            t.note(8365);
        }
        t.note(7191);
        t.note(7191);
        // 再来 DIRTY_SITES 个各不相同的行号,把槽位撑满并溢出。
        for line in 1000..(1000 + DIRTY_SITES as u32) {
            t.note(line);
        }
        let (sites, other) = t.drain();
        assert!(
            sites.contains(&(8365, 300)),
            "每帧都在置脏的那一处没被单独计出来:{sites:?}"
        );
        assert!(sites.contains(&(7191, 2)), "第二处没被计出来:{sites:?}");
        assert_eq!(sites.len(), DIRTY_SITES, "槽位没被填满:{sites:?}");
        assert!(other > 0, "槽位用完之后的置脏必须落进 other,否则会静默消失");
    }

    /// 取走之后,**这一窗口没再响过**的槽位要还回去。
    ///
    /// 不还的话,启动那几秒里各来一次的一次性置脏点会把 8 个槽位永久占死,
    /// 而真正每帧都在置脏的那一处只能落进 `other` —— 一整趟实机往返白跑,
    /// 且剖面看起来一切正常。
    ///
    /// 自证会变红:把 `drain` 里那句「hits == 0 就把槽位清零」删掉。
    #[test]
    fn a_slot_that_went_quiet_is_handed_back_for_the_next_window() {
        let t = DirtyTable::new();
        for line in 1..=(DIRTY_SITES as u32) {
            t.note(line); // 启动期的一次性置脏点,各来一次
        }
        let _ = t.drain();
        for _ in 0..50 {
            t.note(9999); // 下一个窗口里真正每帧都在置脏的那一处
        }
        let (sites, other) = t.drain();
        assert!(
            sites.contains(&(9999, 50)),
            "槽位没还回来,常驻置脏点被挤进了 other:sites={sites:?} other={other}"
        );
    }

    /// 计次量是**取走**,不是读取。剖面报的是「这 5 秒」而不是自启动累计。
    ///
    /// 自证会变红:把 `drain` 里 `DIRTY_HITS` 的 `swap(0, ..)` 改成 `load(..)`。
    #[test]
    fn draining_the_attribution_table_resets_the_window() {
        let t = DirtyTable::new();
        t.note(42);
        assert_eq!(t.drain().0, vec![(42, 1)]);
        assert!(t.drain().0.is_empty(), "取走之后窗口该是空的");
    }
```

**1b.** `crates/mullion-app/src/app.rs` 的 `mod tests` 末尾（最后一个 `}` 之前）加：

```rust
    /// F157:**每一处**置脏都必须走 `mark_ui_dirty!`,不许直接写字段赋值。
    ///
    /// 直接赋值等于在归因表上开一个洞,而洞的症状是「剖面里少了一行、
    /// 看起来一切正常」—— 这类静默失效在本项目已经踩中过多次(F12 的
    /// 埋点、F155 的接线),只能靠机械守护。
    ///
    /// 顺带钉住清脏点还在:一起被替换掉的话 `ui_dirty` 再也不会归零,
    /// 每帧都脏,直接重演 T3。
    ///
    /// 自证会变红:把任意一处 `mark_ui_dirty!(self.ui_dirty);` 改回
    /// `self.ui_dirty = true;`。
    #[test]
    fn every_ui_dirty_set_site_goes_through_the_attribution_macro() {
        let src = include_str!("app.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .unwrap_or(src);
        // `split` 找不到模式时会把整份 haystack 原样返回,`.next()` 永远是
        // `Some` —— 切不干净的话下面搜到的会是测试自己的文本,断言恒绿。
        assert!(
            prod.len() < src.len(),
            "没能切掉测试模块 —— 会搜到测试自己的文本,断言恒绿"
        );
        assert!(
            prod.contains("macro_rules! mark_ui_dirty"),
            "置脏宏不见了 —— 归因整个失效"
        );
        assert!(
            prod.contains("mark_ui_dirty!(self.ui_dirty);"),
            "一处宏调用都没有 —— 替换没做"
        );
        assert_eq!(
            prod.matches("ui_dirty = true").count(),
            0,
            "有置脏点绕开了 mark_ui_dirty! —— F157 的归因表会漏掉它"
        );
        assert!(
            prod.contains("self.ui_dirty = false;"),
            "清脏点被一起改掉了 —— ui_dirty 再也不会归零,直接重演 T3"
        );
    }
```

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test -p mullion-app --lib 2>&1 | tail -25`
Expected: 编译失败，`cannot find type DirtyTable in this scope`

- [ ] **Step 3: 写实现（三处）**

**3a.** `crates/mullion-app/src/diag.rs`：把顶部的 `use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};` 改成：

```rust
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
```

在 `static RESHAPE_MISS: AtomicU64 = AtomicU64::new(0);` 之后插入：

```rust
/// F157:`ui_dirty` 归因表的槽位数。
///
/// 8 而不是 80(置脏点总数):空闲时真正在响的只有个位数处,而这张表要待在
/// 帧路径上——最坏 8 次 relaxed load + 1 次 CAS,与 `diag::mark` 同量级。
pub const DIRTY_SITES: usize = 8;

/// `ui_dirty` 置真的归因表:定长、无锁、不分配、不格式化(T3)。
///
/// 抽成结构体(而不是一组裸 `static`)只为一件事:**测试能自己造一个**。
/// 进程级 static 是全 crate 共享的 —— 并行 runner 下别的测试会间接调到
/// `note_ui_dirty`,共享状态会让本文件的断言概率性假红(与 `StageClock`
/// 同一条理由)。
struct DirtyTable {
    /// 槽位认领的源码行号。0 = 空槽。
    line: [AtomicU32; DIRTY_SITES],
    hits: [AtomicU64; DIRTY_SITES],
    /// 槽位用完之后落到这里,报成 `dirty=...,other:N`。**必须报**:
    /// 不报的话「这几处就是全部」与「还有一堆没槽位」在日志里长得一样。
    other: AtomicU64,
}

impl DirtyTable {
    const fn new() -> Self {
        Self {
            line: [const { AtomicU32::new(0) }; DIRTY_SITES],
            hits: [const { AtomicU64::new(0) }; DIRTY_SITES],
            other: AtomicU64::new(0),
        }
    }

    /// 记一次「第 `line` 行把 `ui_dirty` 置真了」。
    ///
    /// 线性扫:命中就加一;遇到空槽就 CAS 抢占。CAS 失败且抢它的人写的
    /// 正是同一个行号时也算命中 —— 不特判的话同一处会在两个槽位里各计
    /// 一半,剖面上那一处的次数凭空少一截。
    fn note(&self, line: u32) {
        for i in 0..DIRTY_SITES {
            let cur = self.line[i].load(Ordering::Relaxed);
            if cur == line {
                self.hits[i].fetch_add(1, Ordering::Relaxed);
                return;
            }
            if cur != 0 {
                continue;
            }
            match self.line[i].compare_exchange(
                0,
                line,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    self.hits[i].fetch_add(1, Ordering::Relaxed);
                    return;
                }
                Err(taken) if taken == line => {
                    self.hits[i].fetch_add(1, Ordering::Relaxed);
                    return;
                }
                Err(_) => continue,
            }
        }
        self.other.fetch_add(1, Ordering::Relaxed);
    }

    /// 取走这一窗口的内容。
    ///
    /// **这一窗口一次没响过的槽位要还回去**:不还的话,启动那几秒里各来
    /// 一次的一次性置脏点会把 8 个槽位永久占死,而真正每帧都在置脏的那
    /// 一处只能落进 `other` —— 一整趟实机往返白跑,且剖面看起来一切正常。
    fn drain(&self) -> (Vec<(u32, u64)>, u64) {
        let mut out = Vec::new();
        for i in 0..DIRTY_SITES {
            let hits = self.hits[i].swap(0, Ordering::Relaxed);
            if hits > 0 {
                out.push((self.line[i].load(Ordering::Relaxed), hits));
            } else {
                self.line[i].store(0, Ordering::Relaxed);
            }
        }
        (out, self.other.swap(0, Ordering::Relaxed))
    }
}

static DIRTY: DirtyTable = DirtyTable::new();

/// F157:第 `line` 行把 `ui_dirty` 置真了。**只由 `mark_ui_dirty!` 宏调用。**
///
/// 只收行号、不收文件名:所有置脏点都在 `app.rs`(由
/// `app::tests::every_ui_dirty_set_site_goes_through_the_attribution_macro`
/// 钉死)。存文件名要么存 `&'static str` 的裸指针(不安全还原),要么加锁
/// ——帧路径上两者都不行(T3)。
pub fn note_ui_dirty(line: u32) {
    DIRTY.note(line);
}
```

在 `take_snapshot` 里，`s.reshape_miss = RESHAPE_MISS.swap(0, Ordering::Relaxed);` 之后插入：

```rust
    let (dirty_sites, dirty_other) = DIRTY.drain();
    s.dirty_sites = dirty_sites;
    s.dirty_other = dirty_other;
```

**3b.** `crates/mullion-app/src/app.rs`：在 `use crate::{diag, input, shell};`（约 line 33）之后、`/// app 与「连接建立」异步任务之间的事件` 之前插入宏定义：

```rust
/// F157:把 `ui_dirty` 置真,并把**调用点的行号**记进归因表。
///
/// **唯一的置脏入口**。直接写 `self.ui_dirty = true` 等于在归因表上开一个洞,
/// 而洞的症状是「剖面里少了一行、看起来一切正常」——守护测试
/// `tests::every_ui_dirty_set_site_goes_through_the_attribution_macro` 钉死这一条。
///
/// **是宏而不是 `App` 的方法**:有些置脏点位于 `self.active` 的可变借用作用域里
/// (egui 事件分流那一段),那里调不了任何 `&mut self` 方法(E0499);宏展开成
/// 一句普通的字段赋值,两种上下文都能用。`line!()` 在 `macro_rules!` 体内
/// 展开成**调用点**的行号,正是归因要的东西。
///
/// 开销:一句赋值 + 最坏 8 次 relaxed load + 1 次 CAS,与 `diag::mark` 同量级(T3)。
macro_rules! mark_ui_dirty {
    ($slot:expr) => {{
        $slot = true;
        crate::diag::note_ui_dirty(line!());
    }};
}
```

**3c.** 机械替换 80 处：

```bash
sed -i 's/self\.ui_dirty = true;/mark_ui_dirty!(self.ui_dirty);/g' crates/mullion-app/src/app.rs
```

替换命中的两处关键点值得单独说一句：`about_to_wait` 那条链里的两处 `self.ui_dirty = true`（`repaint_delay < MAX` 排期分支、`transfer.queue.summary().busy` 分支）也会被换掉——**前者正是设计文档 §1.2 认定的自激回路②**，它被归因表点名是本切片最想看到的结果。

替换后**必须**核对（四条都要对上）：

```bash
grep -c "mark_ui_dirty!(self.ui_dirty);" crates/mullion-app/src/app.rs   # 期望 80
grep -c "ui_dirty = true" crates/mullion-app/src/app.rs                  # 期望 0
grep -c "self.ui_dirty = false;" crates/mullion-app/src/app.rs           # 期望 1
grep -n "ui_dirty: true" crates/mullion-app/src/app.rs                   # 期望 1 处(App::new 的初始值,不属于运行期置脏)
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: 全部 `ok.`

若出现 E0499/E0502 一类借用错误：那是宏展开成 `self.ui_dirty = true` 之后**本来就该能过**的（部分借用），报错说明该处原本写的就不是 `self.ui_dirty`——回去看那一处的上下文，不要改宏。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/diag.rs crates/mullion-app/src/app.rs
git commit -m "feat(diag): ui_dirty 置脏点归因表 + mark_ui_dirty! 收口 80 处 (F157)"
```

---

## Task 4: F157 接线（wake / rr / egui_ev / rdelay）

**Files:**
- Modify: `crates/mullion-app/src/diag.rs`（4 个采集函数 + `take_snapshot`）
- Modify: `crates/mullion-app/src/app.rs:7610`（`count_wake`）、`:4770`/`:6464`/`:7193`/`:8826`（`count_request_redraw`）、`:9805`（`note_egui_events`）、`:9880`（`note_repaint_delay`）
- Test: `crates/mullion-app/src/app.rs` 的 `mod tests`

**行号是撰写本计划时的现状，Task 3 的替换会让它们整体下移。按锚点文本定位，不要按行号。**

- [ ] **Step 1: 写会失败的测试**

在 `crates/mullion-app/src/app.rs` 的 `mod tests` 末尾加：

```rust
    /// F157:**每一处** `request_redraw()` 都必须同时记一笔来源。
    ///
    /// 漏一处的症状是剖面里 `wake` 与 `rr` 的差值凭空变大,而那个差值正是
    /// 用来判断「唤醒是谁推的」的唯一依据 —— 归因错了会把人带去改错地方。
    ///
    /// 自证会变红:删掉任意一处 `diag::count_request_redraw(...)`,
    /// 或者新加一处 `request_redraw()` 而不配套记账。
    #[test]
    fn every_request_redraw_records_where_it_came_from() {
        let src = include_str!("app.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .unwrap_or(src);
        assert!(
            prod.len() < src.len(),
            "没能切掉测试模块 —— 会搜到测试自己的文本,断言恒绿"
        );
        let calls = prod.matches(".request_redraw();").count();
        let notes = prod.matches("diag::count_request_redraw(").count();
        assert!(calls > 0, "一处 request_redraw 都没有 —— 切片切错了");
        assert_eq!(
            calls, notes,
            "{calls} 处 request_redraw 只有 {notes} 处记了来源"
        );
        assert!(
            prod.contains("diag::count_request_redraw(diag::RedrawSource::Scheduled)"),
            "没有任何一处按 `sched`(about_to_wait 到点补画)记账"
        );
    }

    /// F157:唤醒计数必须记在 `RedrawRequested` 分支的**最开头**。
    ///
    /// 记在后面的话,那些在帧闸之前就 return 掉的路径(最小化 / PumpOnly)
    /// 完全不计数 —— 而「窗口最小化了却还在每秒醒 400 次」恰恰是最该被
    /// 看见的一种。
    ///
    /// 自证会变红:把 `diag::count_wake();` 挪到 `self.pump_io();` 之后。
    #[test]
    fn the_wake_counter_sits_at_the_very_top_of_the_redraw_arm() {
        let src = include_str!("app.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .unwrap_or(src);
        assert!(prod.len() < src.len(), "没能切掉测试模块,断言会恒绿");
        let arm = prod
            .split("WindowEvent::RedrawRequested => {")
            .nth(1)
            .expect("找不到 RedrawRequested 分支");
        let head = &arm[..arm.len().min(200)];
        assert!(
            head.contains("diag::count_wake();"),
            "唤醒计数不在分支开头:{head}"
        );
        assert!(
            arm.split("self.pump_io();").next().unwrap_or_default().contains("count_wake"),
            "唤醒计数排在了 pump_io 之后 —— 提前 return 的路径会漏计"
        );
    }

    /// F157:喂给 egui 的事件数与 egui 吐回来的 `repaint_delay` 都要采。
    ///
    /// 这两个数是本切片**唯一**能回答「`wants_repaint_after` 的哪一条判据
    /// 每帧都成立」的东西,少任何一个都得再跑一趟实机。
    ///
    /// 自证会变红:删掉 `diag::note_egui_events(` 或 `diag::note_repaint_delay(`。
    #[test]
    fn the_egui_side_of_the_frame_loop_is_instrumented() {
        let src = include_str!("app.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .unwrap_or(src);
        assert!(prod.len() < src.len(), "没能切掉测试模块,断言会恒绿");
        assert!(
            prod.contains("diag::note_egui_events(raw_input.events.len());"),
            "没采「这一帧喂了 egui 几个事件」"
        );
        assert!(
            prod.contains("diag::note_repaint_delay(repaint_delay);"),
            "没采 egui 吐回来的 repaint_delay"
        );
    }
```

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test -p mullion-app --lib every_request_redraw the_wake_counter the_egui_side 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: 三条全 FAILED

- [ ] **Step 3: 写实现（两处文件）**

**3a.** `crates/mullion-app/src/diag.rs`：在 `static RESHAPE_MISS` 之后（Task 3 加的 `DIRTY_SITES` 之前）插入计数器：

```rust
/// F157:本窗口收到多少次 `RedrawRequested`。唤醒率的直接读数。
static WAKES: AtomicU64 = AtomicU64::new(0);
/// F157:我们主动 `request_redraw` 的次数,按来源分。
static RR_SCHED: AtomicU64 = AtomicU64::new(0);
static RR_EVT: AtomicU64 = AtomicU64::new(0);
/// F157:喂给 egui 的事件总数 / 其中有事件的帧数。
static EGUI_EVENTS: AtomicU64 = AtomicU64::new(0);
static EGUI_EVENT_FRAMES: AtomicU64 = AtomicU64::new(0);
/// F157:`repaint_delay` 三分桶(见 `profile::repaint_bucket`)。
static RDELAY_ZERO: AtomicU64 = AtomicU64::new(0);
static RDELAY_FINITE: AtomicU64 = AtomicU64::new(0);
static RDELAY_MAX: AtomicU64 = AtomicU64::new(0);
/// F159:整帧指纹命中/未命中的帧数。
static FP_HIT: AtomicU64 = AtomicU64::new(0);
static FP_MISS: AtomicU64 = AtomicU64::new(0);
```

在 `pub fn count_throttled()` 之后插入采集函数：

```rust
/// F157:窗口收到了一次 `RedrawRequested`。**记在分支最开头**——记在后面
/// 的话,最小化 / PumpOnly 那些提前 return 的路径完全不计数,而「最小化了
/// 却还在每秒醒 400 次」恰恰是最该被看见的一种。
pub fn count_wake() {
    WAKES.fetch_add(1, Ordering::Relaxed);
}

/// 我们主动调 `request_redraw` 的来源(F157)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedrawSource {
    /// `about_to_wait` 到点把被节流的那帧补画出来。
    Scheduled,
    /// 因窗口事件 / 后台唤醒 / UI 主动请求。
    Event,
}

/// F157:记一次主动请求重绘。
///
/// 与 `count_wake` 的差值不为零是**正常**的:多次请求会被 winit 合并成
/// 一次 `RedrawRequested`,OS 也会主动发。差值本身就是信息,不试图把它
/// 归成精确的三类(那需要一个「最近一次请求来源」的单槽标记,而合并会让
/// 它系统性失真)。宁可报两个诚实的数。
pub fn count_request_redraw(src: RedrawSource) {
    match src {
        RedrawSource::Scheduled => &RR_SCHED,
        RedrawSource::Event => &RR_EVT,
    }
    .fetch_add(1, Ordering::Relaxed);
}

/// F157:这一帧喂给 egui 多少个输入事件。0 时直接返回,免得静止时也在
/// relaxed 原子上打转。
///
/// egui 的 `InputState::wants_repaint_after` 只要本趟 `events` 非空就返回
/// `Duration::ZERO` —— 所以「有几帧带着事件」才是跟 `rdelay=z:` 直接对得上
/// 的那个数,两者必须分开报。
pub fn note_egui_events(n: usize) {
    if n == 0 {
        return;
    }
    EGUI_EVENTS.fetch_add(n as u64, Ordering::Relaxed);
    EGUI_EVENT_FRAMES.fetch_add(1, Ordering::Relaxed);
}

/// F157:egui 这一帧吐回来的 `repaint_delay` 落在哪个桶。
pub fn note_repaint_delay(d: Duration) {
    match crate::profile::repaint_bucket(d) {
        crate::profile::RepaintBucket::Zero => &RDELAY_ZERO,
        crate::profile::RepaintBucket::Finite => &RDELAY_FINITE,
        crate::profile::RepaintBucket::Max => &RDELAY_MAX,
    }
    .fetch_add(1, Ordering::Relaxed);
}

/// F159:整帧指纹这一帧命中(没提交 GPU)还是没命中。
///
/// **差分类优化唯一的运行期守护**:判据写错导致永远 miss 时,画面完全
/// 正确、日志一切正常、性能悄悄回到改之前,只有这里的比值会掉。
pub fn count_frame_fp(hit: bool) {
    if hit { &FP_HIT } else { &FP_MISS }.fetch_add(1, Ordering::Relaxed);
}
```

在 `take_snapshot` 里，Task 3 加的那两行之后插入：

```rust
    s.wakes = WAKES.swap(0, Ordering::Relaxed);
    s.rr_sched = RR_SCHED.swap(0, Ordering::Relaxed);
    s.rr_evt = RR_EVT.swap(0, Ordering::Relaxed);
    s.egui_events = EGUI_EVENTS.swap(0, Ordering::Relaxed);
    s.egui_event_frames = EGUI_EVENT_FRAMES.swap(0, Ordering::Relaxed);
    s.rdelay_zero = RDELAY_ZERO.swap(0, Ordering::Relaxed);
    s.rdelay_finite = RDELAY_FINITE.swap(0, Ordering::Relaxed);
    s.rdelay_max = RDELAY_MAX.swap(0, Ordering::Relaxed);
    s.fp_hit = FP_HIT.swap(0, Ordering::Relaxed);
    s.fp_miss = FP_MISS.swap(0, Ordering::Relaxed);
```

**3b.** `crates/mullion-app/src/app.rs` 五处接线：

① `WindowEvent::RedrawRequested => {` 分支的第一句（在 `let now = self.now_ms();` **之前**）：

```rust
            WindowEvent::RedrawRequested => {
                // F157:唤醒率的直接读数。**必须是这个分支的第一句** ——
                // 排在 `pump_io` 之后的话,最小化 / PumpOnly 那些提前 return
                // 的路径完全不计数,而「最小化了却还在每秒醒 400 次」恰恰是
                // 最该被看见的一种。
                diag::count_wake();
                let now = self.now_ms();
```

② `fn request_ui_redraw` 里：

```rust
    fn request_ui_redraw(&mut self) {
        mark_ui_dirty!(self.ui_dirty);
        if let Some(a) = &self.active {
            diag::count_request_redraw(diag::RedrawSource::Event);
            a.window.request_redraw();
        }
    }
```

③ `UserEvent::Wake` 分支里那句：

```rust
                } else if let Some(a) = &self.active {
                    diag::count_request_redraw(diag::RedrawSource::Event);
                    a.window.request_redraw();
                }
```

④ egui 事件分流里那句（`if resp.repaint {` 内部）：

```rust
                if resp.repaint {
                    // 标脏与请求重绘必须成对:只请求不标脏,那帧会被 frame_is_dirty
                    // 判 Idle 丢掉(终端态尤其明显:远端一安静菜单就点不开)。
                    mark_ui_dirty!(self.ui_dirty);
                    diag::count_request_redraw(diag::RedrawSource::Event);
                    active.window.request_redraw();
                }
```

⑤ `fn about_to_wait` 里那句：

```rust
                if let Some(a) = &self.active {
                    diag::count_request_redraw(diag::RedrawSource::Scheduled);
                    a.window.request_redraw();
                }
```

⑥ `render_frame` 里 `let raw_input = a.egui_state.take_egui_input(&a.window);` 之后：

```rust
    let raw_input = a.egui_state.take_egui_input(&a.window);
    // F157:这一帧喂了 egui 几个事件。egui 的 `wants_repaint_after` 只要本趟
    // `events` 非空就返回 `Duration::ZERO` —— 空闲时这个数本该是 0,不是 0
    // 就说明有人在往里灌事件,那正是「凭什么还在出帧」的答案。
    diag::note_egui_events(raw_input.events.len());
```

⑦ `render_frame` 里 `let repaint_delay = full_output...;` 之后：

```rust
        .map_or(std::time::Duration::MAX, |v| v.repaint_delay);
    // F157:egui 到底要不要下一帧。真空闲时本该恒是 `m:`(MAX);日志里
    // 每帧都是 `z:`/`f:` 就坐实了自激回路②。
    diag::note_repaint_delay(repaint_delay);
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: 全部 `ok.`

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/diag.rs crates/mullion-app/src/app.rs
git commit -m "feat(app): 帧循环归因接线——唤醒/主动请求/egui 事件/repaint_delay (F157)"
```

---

## Task 5: F158 —— 摘掉 launcher 态的无条件出帧兜底

**Files:**
- Modify: `crates/mullion-app/src/app.rs`（`let dirty = match self.active_ws() { ... }` 一段及其上方注释）
- Test: `crates/mullion-app/src/app.rs` 的 `mod tests`

**为什么不新增函数：** `terminal_dirty` 在 launcher 态本来就恒为 `false`（上面那个 `match` 的 `None => false`），所以两态判据天然统一。再包一层带 `has_workspace` 参数的函数会让那个参数被忽略，是纯粹的坏味道。

- [ ] **Step 1: 写会失败的测试**

在 `crates/mullion-app/src/app.rs` 的 `mod tests` 末尾加：

```rust
    /// F158:判脏在 launcher 态与终端态**必须是同一条判据**。
    ///
    /// 摘掉的是原来那句 `None => true`(没连任何东西时无条件判脏)。日志
    /// 坐实了它的后果:`tabs=0 panes=0` 时照样 `frame=300x/present=300`
    /// —— 对着一屏静止的占位 UI 每秒提交 60 帧 GPU。旁边注释给的理由
    /// (「`ControlFlow::Wait` 下 winit 不会凭空生成 `RedrawRequested`」)
    /// 在同一函数别处会排 `WaitUntil` 的前提下不成立。
    ///
    /// 判据本身的真值表由 `frame::tests::egui_repaint_alone_is_dirty_enough`
    /// 里的 `assert!(!frame_is_dirty(false, false))` 守着,这里只钉接线。
    ///
    /// 自证会变红:把绑定式改回
    /// `match self.active_ws() { Some(_) => crate::frame::frame_is_dirty(terminal_dirty, self.ui_dirty), None => true }`。
    #[test]
    fn the_frame_dirty_check_is_the_same_in_both_states() {
        let src = include_str!("app.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .unwrap_or(src);
        assert!(
            prod.len() < src.len(),
            "没能切掉测试模块 —— 会搜到测试自己的文本,断言恒绿"
        );
        assert_eq!(
            prod.matches("let dirty = ").count(),
            1,
            "`let dirty = ` 不止一处,下面的切片会指错地方"
        );
        let binding = prod
            .split("let dirty = ")
            .nth(1)
            .expect("找不到 dirty 的绑定式")
            .split(";\n")
            .next()
            .unwrap_or_default();
        assert_eq!(
            binding.trim(),
            "crate::frame::frame_is_dirty(terminal_dirty, self.ui_dirty)",
            "launcher 态又开了一条兜底判据:{binding}"
        );
    }
```

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test -p mullion-app --lib the_frame_dirty_check 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: FAILED，`launcher 态又开了一条兜底判据: match self.active_ws() { ... }`

- [ ] **Step 3: 写实现**

把 `crates/mullion-app/src/app.rs` 里这一段：

```rust
                // dirty:终端态取「远端来了新字节(pacer,含同步块探测)」与「egui 要
                // 重绘」的并集——只看前者的话,远端一安静菜单就点不开(见
                // `frame::frame_is_dirty`)。launcher 态本 Task 没有持续数据源,把
                // 「确实触发了一次 RedrawRequested」当作脏——这不是无条件轮询:
                // ControlFlow::Wait 下 winit 不会凭空生成 RedrawRequested,真正的重绘
                // 频率由触发它的事件(resize/connect/wake/OS 重绘)决定。
```

替换成：

```rust
                // dirty:两态**同一条判据** —— 「远端来了新字节(pacer,含同步块
                // 探测)」与「egui 要重绘」的并集(见 `frame::frame_is_dirty`)。
                //
                // F158:这里原本还有一句 `None => true`(launcher 态无条件判脏),
                // 已摘掉。当时给的理由是「`ControlFlow::Wait` 下 winit 不会凭空
                // 生成 `RedrawRequested`」,而它在同一函数别处会排 `WaitUntil`
                // 的前提下不成立:present 之后那段一旦拿到有限的 `repaint_delay`
                // 就排一次 `WaitUntil`,到点 `about_to_wait` 补一次 `request_redraw`
                // —— 闭环自激。日志坐实:`tabs=0 panes=0` 时照样
                // `frame=300x/present=300`,对着一屏静止的占位 UI 每秒提交 60 帧。
                //
                // 摘掉之后 `ui_dirty` 成为 launcher 态的唯一判据,而它是
                // 80 个置脏点 : 1 个清脏点的结构。兜底改由 F159 的整帧指纹
                // (构造式)提供:漏标脏最多晚一帧,不会永久卡住。
```

再把下面这一段：

```rust
                let dirty = match self.active_ws() {
                    Some(_) => crate::frame::frame_is_dirty(terminal_dirty, self.ui_dirty),
                    None => true,
                };
```

替换成（`terminal_dirty` 在 launcher 态由上面那个 `match` 保证恒为 `false`，两态因此天然统一）：

```rust
                let dirty = crate::frame::frame_is_dirty(terminal_dirty, self.ui_dirty);
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: 全部 `ok.`

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "fix(app): launcher 态不再无条件判脏,两态统一走 frame_is_dirty (F158)"
```

---

## Task 6: F158 缓解②——后台事件默认标脏

**Files:**
- Modify: `crates/mullion-app/src/app.rs`（新增自由函数 `user_event_marks_dirty` + `fn user_event` 开头接线）
- Test: `crates/mullion-app/src/app.rs` 的 `mod tests`

**为什么需要：** 摘掉 launcher 兜底之后，风险来源不是窗口事件（egui 分流那一处覆盖了），而是**后台线程送进来的 `UserEvent`**：连接结果、`HostKeyPrompt`、传输进度、SFTP 完成。漏标一处 = 「连上了画面不动，动一下鼠标才刷出来」。

**判据方向必须反过来**：默认标脏，只对三种显式豁免——`Wake` / `TransferProgress`（每秒几千条、靠帧内排期驱动画面，标脏就是 T3），以及 `EditTick`（**理由不同**：它的分支把 `self.ui_dirty` 当信号读，在这里预置会让那个 `if` 恒真，语义静默改掉）。将来加新变体时默认落在「多画一帧」这一侧——判 `true` 最多多画一帧，判 `false` 是画面卡住。

- [ ] **Step 1: 写会失败的测试**

在 `crates/mullion-app/src/app.rs` 的 `mod tests` 末尾加：

```rust
    /// F158:后台事件**默认标脏**,只有三种显式豁免。
    ///
    /// 方向不对称是重点:判 `true` 最多多画一帧(而且会被 F159 的整帧指纹
    /// 拦掉),判 `false` 是「连上了画面不动」。所以 `user_event_marks_dirty`
    /// 写成穷尽 `match` 而不是 `_ => false` —— 加新变体时编译报错,强迫作者
    /// 表态,而不是静默落到「不标脏」那一侧。
    ///
    /// 自证会变红:把 `user_event_marks_dirty` 里 `ConnectErr` 那一支挪进
    /// 豁免名单。
    #[test]
    fn a_background_event_marks_the_ui_dirty_unless_it_is_a_known_flood() {
        // 高频、靠帧内排期驱动的两种:标脏就是 T3(风扇起飞)。
        assert!(
            !user_event_marks_dirty(&UserEvent::Wake),
            "Wake 每秒可以来几千条,标脏等于把 T2 的攒帧闸整个绕过去"
        );
        assert!(
            !user_event_marks_dirty(&UserEvent::TransferProgress { job: 1, done: 0 }),
            "传输进度每秒几千条,靠它驱动重绘就是风扇起飞"
        );
        // `EditTick` 豁免的理由**不同**:它自己的分支把 `self.ui_dirty` 当
        // 信号读(「文件变了不一定改动界面」),在这里预先置真会让那个条件
        // 恒成立,静默改掉它的语义。
        assert!(
            !user_event_marks_dirty(&UserEvent::EditTick { key: 1, stamp: None }),
            "EditTick 自己判脏,在这里预置会让它分支里的 `if self.ui_dirty` 恒真"
        );
        // 其余一律标脏 —— 挑三种最容易漏、且漏了症状最难查的。
        assert!(
            user_event_marks_dirty(&UserEvent::ConnectErr("boom".into())),
            "连接失败不标脏 = 用户点了连接,错误提示要等他动鼠标才出现"
        );
        assert!(
            user_event_marks_dirty(&UserEvent::ProbeOk(7)),
            "探测结果不标脏 = 会话管理器里的状态永远停在「探测中」"
        );
        assert!(
            user_event_marks_dirty(&UserEvent::KeyPathPicked(None)),
            "文件对话框取消不标脏 = 按钮永远停在禁用态"
        );
    }

    /// F158:那个判据必须**真的接在** `user_event` 的开头。
    ///
    /// 纯函数写对了但没接线,是本项目反复踩过的静默失效(F12 的埋点、
    /// F155 的接线)——判据全绿,画面照样卡住。
    ///
    /// 自证会变红:把 `fn user_event` 开头那三行删掉。
    #[test]
    fn the_background_dirty_rule_is_actually_wired_into_user_event() {
        let src = include_str!("app.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .unwrap_or(src);
        assert!(prod.len() < src.len(), "没能切掉测试模块,断言会恒绿");
        let body = prod
            .split("fn user_event(&mut self")
            .nth(1)
            .expect("找不到 user_event");
        let head = &body[..body.len().min(600)];
        assert!(
            head.contains("if user_event_marks_dirty(&event)"),
            "判据没接进 user_event 的开头:{head}"
        );
    }
```

**与既有守护的关系（不要"顺手统一"）：** `app.rs` 里已有一条
`transfer_progress_events_never_request_a_redraw_so_the_fan_stays_quiet`，
断言 `UserEvent::TransferProgress` 那条 **arm** 内不含 `ui_dirty` / `request_redraw`。
本 Task 的 `mark_ui_dirty!` 加在 `user_event` 的**开头**、arm 之外，所以那条测试
文本上照样过——但它守的行为（进度事件不驱动重绘）只有在 `TransferProgress`
被列进豁免名单时才真的成立。两者必须一起看：**把 `TransferProgress` 从豁免
名单里挪出去，那条既有测试不会变红，风扇却会起飞。**

设计文档 §5.2 把"传输进度"列在风险来源里，本计划**有意偏离**：那一档由
`about_to_wait` 里 `transfer.queue.summary().busy` 那条 `WaitUntil` 排期驱动
（`TRANSFER_UI_INTERVAL_MS`），不需要事件驱动，标脏就是 T3。

**注意**：上面 `UserEvent::TransferProgress { job, done }` 与 `EditTick { key, stamp }` 的字段名已按 `app.rs` 当前定义核对过（`TransferProgress` **没有** `total` 字段）。若届时对不上，以 `app.rs` 的定义为准。

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test -p mullion-app --lib a_background_event_marks 2>&1 | tail -20`
Expected: 编译失败，`cannot find function user_event_marks_dirty`

- [ ] **Step 3: 写实现**

在 `crates/mullion-app/src/app.rs` 里 `fn blink_on_at` 定义的旁边（自由函数区，`fn render_frame` 之前）加：

```rust
/// F158:这个后台事件该不该把 egui 侧标脏。
///
/// **穷尽 `match`,不许用 `_`**:摘掉 launcher 的无条件出帧兜底之后,
/// `ui_dirty` 成了 launcher 态的唯一判据,漏标一处的症状是「连上了画面
/// 不动,动一下鼠标才刷出来」。加新变体时这里编译报错,强迫作者表态。
///
/// 方向也刻意不对称:判 `true` 最多多画一帧(而且会被 F159 的整帧指纹
/// 拦掉),判 `false` 是画面卡住。所以默认落在标脏那一侧,只对已知的
/// 高频事件显式豁免。
fn user_event_marks_dirty(e: &UserEvent) -> bool {
    match e {
        // ——— 豁免之一:每秒几千条,靠帧内排期驱动画面 ———
        //
        // `Wake` 是「远端来了字节」的通知,画面该不该更新由 `terminal_dirty`
        // (pacer,含 T2 的同步块攒帧)判;在这里标脏等于把攒帧闸整个绕过去。
        UserEvent::Wake => false,
        // 传输进度每秒几千条,靠它驱动重绘就是 T3(风扇起飞)。画面由
        // `RedrawRequested` 里那段排期推进。
        UserEvent::TransferProgress { .. } => false,
        // ——— 豁免之二(理由完全不同):自己判脏 ———
        //
        // `EditTick` 的分支把 `self.ui_dirty` **当信号读**(「看门任务只在
        // 文件真的变了时才发这条,但『变了』不一定改动界面」)。在这里预先
        // 置真会让那个 `if self.ui_dirty` 恒成立 —— 编译过、测试过、语义
        // 静默变成「每次 tick 都重绘」。它的分支自成闭环,不需要这里帮忙。
        UserEvent::EditTick { .. } => false,
        // ——— 其余一律标脏 ———
        UserEvent::ConnectOk { .. }
        | UserEvent::ConnectErr(_)
        | UserEvent::KeyPathPicked(_)
        | UserEvent::CredentialKeyPathPicked(_)
        | UserEvent::IconPathPicked(_)
        | UserEvent::SshConfigPicked(_)
        | UserEvent::HostKeyPrompt(_)
        | UserEvent::PaneOpened { .. }
        | UserEvent::PaneOpenErr { .. }
        | UserEvent::PaneRehosted { .. }
        | UserEvent::PaneRehostErr { .. }
        | UserEvent::PaneReconnected { .. }
        | UserEvent::PaneReconnectErr { .. }
        | UserEvent::ProbeOk(_)
        | UserEvent::ProbeErr(_, _)
        | UserEvent::AutomationDone(_, _, _)
        | UserEvent::TunnelState { .. }
        | UserEvent::SftpOpened { .. }
        | UserEvent::SftpListed { .. }
        | UserEvent::OwnerNames { .. }
        | UserEvent::SftpOpDone { .. }
        | UserEvent::TransferPlanned { .. }
        | UserEvent::TransferDone { .. }
        | UserEvent::EditOpened { .. }
        | UserEvent::EditSaved { .. } => true,
    }
}
```

**变体名单以 `app.rs` 的 `pub enum UserEvent` 为准**（撰写本计划时是 28 个）。少写一个会编译报错（`match` 非穷尽），那正是要的效果——照编译器的提示补齐，**不要加 `_ =>`**。

在 `fn user_event` 的开头接线：

```rust
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        diag::mark(diag::Stage::UserEvent);
        // F158:后台事件默认标脏。判据与豁免名单见 `user_event_marks_dirty`。
        if user_event_marks_dirty(&event) {
            mark_ui_dirty!(self.ui_dirty);
        }
        match event {
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: 全部 `ok.`

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "fix(app): 后台事件默认标脏,只豁免 Wake/TransferProgress/EditTick (F158)"
```

---

## Task 7: F159 —— `frame_fp` 模块（纯函数）

**Files:**
- Create: `crates/mullion-app/src/frame_fp.rs`
- Modify: `crates/mullion-app/src/lib.rs`（加 `pub mod frame_fp;`）
- Modify: `crates/mullion-app/src/text.rs`（新增 `TextLayer::style_key`）
- Test: `crates/mullion-app/src/frame_fp.rs` 的 `mod tests`

**核心设计（照抄 ADR-011 的推理）：判据放在结果上，不放在原因上。** 能改变「这一帧长什么样」的来源列举不完，漏一个的症状是屏幕留着陈旧的一帧，编译/测试/日志全静默。

**三条不能省的细节：**

1. **`FrameFp` 刻意不 `derive(PartialEq)`。** `Unhashable == Unhashable` 会被 derive 判成 `true`，而那正好是「含 paint callback 的两帧被判成一样、于是永远不再重画」这个静默故障。比较一律走 `same_as`。
2. **光标闪烁走 `gpu::style_for` 的结果，不直接哈希 `blink_on`。** 直接哈希 `blink_on` 的话，launcher 态（一块 pane 都没有）也会跟着相位每秒变 2 次指纹，白白出 2 帧；而非焦点 pane 恒画空心光标、根本不跟着闪，也会被算成「变了」。判在结果上，这两件事自动对。
3. **`f32` 一律 `to_bits()`。** 不用 `==`，也不给带 `f32` 的结构体 `derive(PartialEq)`。

- [ ] **Step 1: 建文件并写测试（先只放测试，实现留空会编译不过——本步骤连实现骨架一起写，Step 2 靠删掉分量来验证测试确实会红）**

创建 `crates/mullion-app/src/frame_fp.rs`：

```rust
//! F159:整帧指纹 —— 「这一帧画出来跟上一帧一模一样吗」。
//!
//! **零 GPU、零 IO**,可纯单测:吃的是 egui 已经 tessellate 出来的顶点、
//! 终端各 pane 的行指纹与几何,吐一个 `u64`。
//!
//! 为什么判在**结果**上而不是判在**原因**上:见
//! [ADR-011](../../../docs/adr-011-row-fingerprint-vs-term-damage.md)
//! (F12 的行指纹为什么不用 `Term::damage()`)。同一条推理 —— 能改变
//! 「这一帧长什么样」的来源列举不完,漏一个的症状是屏幕留着陈旧的一帧,
//! 编译/测试/日志全静默,只有人眼能发现。**失败方向也跟着反过来**:
//! 指纹的最坏情况是多画一帧,枚举式判据的最坏情况是少画。

use mullion_term::snapshot::{Cursor, Rgb};

use crate::gpu::{style_for, PaneRender};
use crate::shell::workspace::{PaneGeom, PxRect};

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// 增量式 FNV-1a。与 `mullion_term::snapshot::hash_row` 同一套常数
/// (那边算「一行长什么样」,这边算「一帧长什么样」)。
#[derive(Debug, Clone, Copy)]
struct Fnv(u64);

impl Fnv {
    const fn new() -> Self {
        Self(FNV_OFFSET)
    }
    fn byte(&mut self, b: u8) {
        self.0 ^= b as u64;
        self.0 = self.0.wrapping_mul(FNV_PRIME);
    }
    fn u64(&mut self, v: u64) {
        for b in v.to_le_bytes() {
            self.byte(b);
        }
    }
    fn u32(&mut self, v: u32) {
        self.u64(v as u64);
    }
    /// **位模式**,不是数值。`f32` 绝不用 `==` 比,也绝不进 `derive(PartialEq)`。
    fn f32(&mut self, v: f32) {
        self.u32(v.to_bits());
    }
    fn bool(&mut self, v: bool) {
        self.byte(v as u8);
    }
    /// 先吃长度再吃内容 —— 不吃长度的话 `("ab","c")` 与 `("a","bc")` 同哈希。
    fn bytes(&mut self, s: &[u8]) {
        self.u64(s.len() as u64);
        for &b in s {
            self.byte(b);
        }
    }
    fn rgb(&mut self, c: Rgb) {
        let Rgb { r, g, b } = c;
        self.byte(r);
        self.byte(g);
        self.byte(b);
    }
}

/// 一帧的指纹。
///
/// **刻意不 `derive(PartialEq)`**:`Unhashable == Unhashable` 会被 derive
/// 判成 `true`,而那正好是「含 paint callback 的两帧被判成一样、于是永远
/// 不再重画」这个静默故障。比较一律走 [`FrameFp::same_as`]。
#[derive(Debug, Clone, Copy)]
pub enum FrameFp {
    Hash(u64),
    /// 本帧含无法指纹化的内容(egui 的 paint callback)。与任何东西都不相同,
    /// **包括另一个 `Unhashable`** —— 回调每帧可以画出不同的东西,我们看不见。
    /// 保守方向:多画一帧,永不少画。
    Unhashable,
}

impl FrameFp {
    /// 两帧是否**确定**一模一样。
    pub fn same_as(&self, other: &FrameFp) -> bool {
        match (self, other) {
            (FrameFp::Hash(a), FrameFp::Hash(b)) => a == b,
            _ => false,
        }
    }
}

/// 影响文字层最终长相、但不进任何行指纹的样式量(F21 字体族/字号、
/// F80 兜底文字色、DPI)。
///
/// **刻意不 `derive(PartialEq)`**:里面有 `f32`,比较一律走位模式。
#[derive(Debug, Clone, Copy)]
pub struct StyleKey<'a> {
    pub family: &'a str,
    pub font_px: f32,
    pub cell_w: f32,
    pub cell_h: f32,
    pub default_fg: Rgb,
}

/// 这一帧能不能跳过 GPU 提交。
///
/// `deltas_empty`:egui 这一帧有没有待上传 / 待释放的纹理增量。**非空一律
/// 判 miss** —— 那两份 delta 是 egui 每帧 drain 出来、**只交付一次**的
/// (字体图集的新字形栅格 / 纹理回收),跳掉就永久丢了,之后某帧会引用
/// 一张从未上传的纹理:花屏或 panic,且只在「先命中、后未命中」的序列里
/// 发作,无头测试完全够不到。真实频率极低,不影响收益。
pub fn can_skip(prev: Option<&FrameFp>, cur: &FrameFp, deltas_empty: bool) -> bool {
    deltas_empty && prev.is_some_and(|p| p.same_as(cur))
}

/// 算这一帧的整帧指纹。
///
/// `surface` 是交换链的 `(width, height)` —— 窗口尺寸变了必须重画,而
/// 尺寸不进 egui 顶点也不进行指纹。
pub fn frame_fingerprint(
    paint_jobs: &[egui::ClippedPrimitive],
    panes: &[PaneRender<'_>],
    blink_on: bool,
    style: StyleKey<'_>,
    surface: (u32, u32),
) -> FrameFp {
    let mut h = Fnv::new();
    h.u32(surface.0);
    h.u32(surface.1);
    hash_style(&mut h, style);
    if !hash_paint_jobs(&mut h, paint_jobs) {
        return FrameFp::Unhashable;
    }
    h.u64(panes.len() as u64);
    for p in panes {
        hash_pane(&mut h, p, blink_on);
    }
    FrameFp::Hash(h.0)
}

fn hash_style(h: &mut Fnv, style: StyleKey<'_>) {
    // 穷尽解构 —— 加字段时这里编译报错,强迫作者对「进不进指纹」表态。
    let StyleKey {
        family,
        font_px,
        cell_w,
        cell_h,
        default_fg,
    } = style;
    h.bytes(family.as_bytes());
    h.f32(font_px);
    h.f32(cell_w);
    h.f32(cell_h);
    h.rgb(default_fg);
}

/// 返回 `false` = 本帧含 paint callback,指纹不成立。
fn hash_paint_jobs(h: &mut Fnv, jobs: &[egui::ClippedPrimitive]) -> bool {
    h.u64(jobs.len() as u64);
    for job in jobs {
        // 穷尽解构 —— 同上。
        let egui::ClippedPrimitive {
            clip_rect,
            primitive,
        } = job;
        h.f32(clip_rect.min.x);
        h.f32(clip_rect.min.y);
        h.f32(clip_rect.max.x);
        h.f32(clip_rect.max.y);
        match primitive {
            egui::epaint::Primitive::Mesh(mesh) => {
                let egui::epaint::Mesh {
                    indices,
                    vertices,
                    texture_id,
                } = mesh;
                match texture_id {
                    egui::TextureId::Managed(id) => {
                        h.byte(0);
                        h.u64(*id);
                    }
                    egui::TextureId::User(id) => {
                        h.byte(1);
                        h.u64(*id);
                    }
                }
                h.u64(indices.len() as u64);
                for i in indices {
                    h.u32(*i);
                }
                h.u64(vertices.len() as u64);
                for v in vertices {
                    let egui::epaint::Vertex { pos, uv, color } = *v;
                    h.f32(pos.x);
                    h.f32(pos.y);
                    h.f32(uv.x);
                    h.f32(uv.y);
                    for c in color.to_array() {
                        h.byte(c);
                    }
                }
            }
            // 回调每帧可以画出不同的东西,我们看不见 —— 一律判「变了」。
            // 我们目前不用 paint callback,但这条分支必须写:将来有人加了
            // 之后静默失效(屏幕停在旧的一帧)是不可接受的。
            egui::epaint::Primitive::Callback(_) => return false,
        }
    }
    true
}

fn hash_pane(h: &mut Fnv, pane: &PaneRender<'_>, blink_on: bool) {
    // 穷尽解构 —— 给 `PaneRender` 加字段时编译报错。
    let PaneRender {
        geom,
        snap,
        focused,
        preedit,
    } = pane;
    hash_geom(h, geom);
    h.bool(*focused);
    // F126 组字串。**这一分量不能省**:preedit 画在终端文字层(复用
    // `SnapCell::width` 的宽度判据),不在 egui 的 paint_jobs 里;而组字过程
    // 中 cells 不变、行指纹不变 —— 漏掉它,指纹在整个组字过程中恒命中,
    // **打拼音屏幕纹丝不动**,正是 T10 那一族「只有人眼能发现」的坑。
    h.bytes(preedit.as_bytes());
    h.u32(snap.cols as u32);
    h.u32(snap.rows as u32);
    let Cursor {
        row,
        col,
        visible,
        shape,
        blinking,
    } = snap.cursor;
    h.u32(row as u32);
    h.u32(col as u32);
    h.bool(visible);
    h.bool(blinking);
    // F125 闪烁相位:吃 `style_for` 的**结果**而不是裸的 `blink_on`。
    // 吃裸值的话,一块 pane 都没有的 launcher 态也会跟着相位每秒变 2 次
    // 指纹(白出 2 帧),而非焦点 pane 恒画空心光标、根本不跟着闪,也会
    // 被算成「变了」。判在结果上,这两件事自动对。
    h.byte(style_for(shape, *focused, blink_on) as u8);
    // F12 的行指纹。`SnapCell.selected` 在内,划选反色自动覆盖;主题换色
    // 也已经烘进快照的 fg/bg。
    for r in 0..snap.rows {
        h.u64(snap.row_hash(r));
    }
}

fn hash_geom(h: &mut Fnv, g: &PaneGeom) {
    // 穷尽解构 —— 同上。
    let PaneGeom {
        id,
        px,
        title_px,
        term_px,
        grid,
    } = *g;
    h.u32(id.0);
    for r in [px, title_px, term_px] {
        let PxRect {
            x,
            y,
            w,
            h: rect_h,
        } = r;
        h.u32(x);
        h.u32(y);
        h.u32(w);
        h.u32(rect_h);
    }
    h.u32(grid.0 as u32);
    h.u32(grid.1 as u32);
}
```

在同文件末尾加测试模块：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mullion_core::layout::PaneId;
    use mullion_term::snapshot::{CursorShape, GridSnapshot, SnapCell};

    fn cell(ch: char) -> SnapCell {
        SnapCell {
            ch,
            fg: Rgb { r: 200, g: 200, b: 200 },
            bg: Rgb { r: 0, g: 0, b: 0 },
            width: 1,
            spacer: false,
            selected: false,
        }
    }

    fn snap_of(text: &str) -> GridSnapshot {
        let cells: Vec<SnapCell> = text.chars().map(cell).collect();
        GridSnapshot::new(
            cells.len() as u16,
            1,
            cells,
            Cursor {
                row: 0,
                col: 0,
                visible: true,
                shape: CursorShape::Beam,
                blinking: true,
            },
        )
    }

    fn geom() -> PaneGeom {
        PaneGeom {
            id: PaneId(1),
            px: PxRect { x: 0, y: 0, w: 800, h: 600 },
            title_px: PxRect { x: 0, y: 0, w: 800, h: 0 },
            term_px: PxRect { x: 0, y: 0, w: 800, h: 600 },
            grid: (80, 24),
        }
    }

    fn style() -> StyleKey<'static> {
        StyleKey {
            family: "Google Sans Code",
            font_px: 16.0,
            cell_w: 9.0,
            cell_h: 20.0,
            default_fg: Rgb { r: 200, g: 200, b: 200 },
        }
    }

    fn mesh_job(x: f32) -> egui::ClippedPrimitive {
        let mut mesh = egui::epaint::Mesh::default();
        mesh.indices.push(0);
        mesh.vertices.push(egui::epaint::Vertex {
            pos: egui::pos2(x, 0.0),
            uv: egui::pos2(0.0, 0.0),
            color: egui::Color32::WHITE,
        });
        egui::ClippedPrimitive {
            clip_rect: egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(800.0, 600.0)),
            primitive: egui::epaint::Primitive::Mesh(mesh),
        }
    }

    /// 一模一样的两帧必须判「一样」。这是整个 F159 收益的前提 ——
    /// 它红了说明指纹里混进了每帧都变的东西(时间戳、地址、迭代顺序),
    /// 症状是画面完全正确、CPU 一点没降,只有 `fp=hit:0/miss:N` 看得出来。
    #[test]
    fn two_identical_frames_hash_the_same() {
        let s = snap_of("hello");
        let panes = [PaneRender { geom: geom(), snap: &s, focused: true, preedit: "" }];
        let jobs = [mesh_job(1.0)];
        let a = frame_fingerprint(&jobs, &panes, true, style(), (800, 600));
        let b = frame_fingerprint(&jobs, &panes, true, style(), (800, 600));
        assert!(a.same_as(&b), "同样的输入算出了不同的指纹");
    }

    /// 终端内容变了必须判「变了」。
    ///
    /// 自证会变红:把 `hash_pane` 里那个 `row_hash` 循环删掉。
    #[test]
    fn a_changed_row_changes_the_fingerprint() {
        let a = snap_of("hello");
        let b = snap_of("hellp");
        let jobs = [mesh_job(1.0)];
        let fa = frame_fingerprint(
            &jobs,
            &[PaneRender { geom: geom(), snap: &a, focused: true, preedit: "" }],
            true,
            style(),
            (800, 600),
        );
        let fb = frame_fingerprint(
            &jobs,
            &[PaneRender { geom: geom(), snap: &b, focused: true, preedit: "" }],
            true,
            style(),
            (800, 600),
        );
        assert!(!fa.same_as(&fb), "改了一个字,指纹却没变 —— 屏幕会留着陈旧的一行");
    }

    /// 划选反色必须判「变了」。
    ///
    /// `SnapCell.selected` 已经在 F12 的行指纹里,这条钉的是「那条链路
    /// 真的接到了整帧指纹上」——断了的症状是拖鼠标划选时选区完全不显示。
    ///
    /// 自证会变红:同上(删 `row_hash` 循环)。
    #[test]
    fn selecting_text_changes_the_fingerprint() {
        let plain = snap_of("hello");
        let mut selected_cells: Vec<SnapCell> = "hello".chars().map(cell).collect();
        selected_cells[0].selected = true;
        let selected = GridSnapshot::new(5, 1, selected_cells, plain.cursor);
        let jobs = [mesh_job(1.0)];
        let fa = frame_fingerprint(
            &jobs,
            &[PaneRender { geom: geom(), snap: &plain, focused: true, preedit: "" }],
            true,
            style(),
            (800, 600),
        );
        let fb = frame_fingerprint(
            &jobs,
            &[PaneRender { geom: geom(), snap: &selected, focused: true, preedit: "" }],
            true,
            style(),
            (800, 600),
        );
        assert!(!fa.same_as(&fb), "划选没改变指纹 —— 选区永远画不出来");
    }

    /// **只改组字串**必须判「变了」(T10 一族)。
    ///
    /// preedit 画在终端文字层(F126),不在 egui 的 paint_jobs 里;而组字
    /// 过程中 cells 不变、行指纹不变 —— 漏掉这一分量的症状是
    /// **打拼音屏幕纹丝不动**,编译/测试/日志全静默。
    ///
    /// 自证会变红:把 `hash_pane` 里那句 `h.bytes(preedit.as_bytes());` 删掉。
    #[test]
    fn typing_pinyin_changes_the_fingerprint_even_though_the_cells_do_not() {
        let s = snap_of("hello");
        let jobs = [mesh_job(1.0)];
        let fa = frame_fingerprint(
            &jobs,
            &[PaneRender { geom: geom(), snap: &s, focused: true, preedit: "ni" }],
            true,
            style(),
            (800, 600),
        );
        let fb = frame_fingerprint(
            &jobs,
            &[PaneRender { geom: geom(), snap: &s, focused: true, preedit: "nih" }],
            true,
            style(),
            (800, 600),
        );
        assert!(!fa.same_as(&fb), "组字串变了指纹没变 —— 打拼音时屏幕会纹丝不动");
    }

    /// 焦点 pane 的光标闪烁相位翻转必须判「变了」——否则光标不闪。
    /// 非焦点 pane 恒画空心光标,相位翻转对它是**不变**的,不该白出一帧。
    ///
    /// 自证会变红:把 `hash_pane` 里的 `style_for(shape, *focused, blink_on)`
    /// 换成裸的 `blink_on`(非焦点那条会红),或者整句删掉(焦点那条会红)。
    #[test]
    fn only_the_focused_pane_churns_when_the_blink_phase_flips() {
        let s = snap_of("hello");
        let jobs = [mesh_job(1.0)];
        let fp = |focused: bool, blink: bool| {
            frame_fingerprint(
                &jobs,
                &[PaneRender { geom: geom(), snap: &s, focused, preedit: "" }],
                blink,
                style(),
                (800, 600),
            )
        };
        assert!(
            !fp(true, true).same_as(&fp(true, false)),
            "焦点 pane 的相位翻转没改变指纹 —— 光标不会闪"
        );
        assert!(
            fp(false, true).same_as(&fp(false, false)),
            "非焦点 pane 恒画空心光标,却跟着相位白出帧"
        );
    }

    /// **一块 pane 都没有**(launcher 态)时,相位翻转不该改变指纹。
    ///
    /// 这条直接对应人工验收清单第 1 条:launcher 静置 `present` 要接近 0。
    /// 吃裸 `blink_on` 的话,launcher 会稳定地每秒出 2 帧。
    ///
    /// 自证会变红:在 `frame_fingerprint` 里加一句 `h.bool(blink_on);`。
    #[test]
    fn the_launcher_does_not_churn_with_the_cursor_blink() {
        let jobs = [mesh_job(1.0)];
        let a = frame_fingerprint(&jobs, &[], true, style(), (800, 600));
        let b = frame_fingerprint(&jobs, &[], false, style(), (800, 600));
        assert!(a.same_as(&b), "launcher 里没有光标,却跟着闪烁相位每秒白出 2 帧");
    }

    /// egui 侧动一个顶点就必须判「变了」——菜单高亮、tooltip、动画全靠它。
    ///
    /// 自证会变红:把 `hash_paint_jobs` 里的 `vertices` 循环删掉。
    #[test]
    fn moving_an_egui_vertex_changes_the_fingerprint() {
        let s = snap_of("hello");
        let panes = [PaneRender { geom: geom(), snap: &s, focused: true, preedit: "" }];
        let a = frame_fingerprint(&[mesh_job(1.0)], &panes, true, style(), (800, 600));
        let b = frame_fingerprint(&[mesh_job(2.0)], &panes, true, style(), (800, 600));
        assert!(!a.same_as(&b), "egui 顶点动了指纹没变 —— 菜单/悬停反馈会卡住");
    }

    /// paint callback **永远**不判命中,包括跟另一个 callback 帧比。
    ///
    /// 我们目前不用 paint callback。这条钉的是「将来有人加了之后不会静默
    /// 失效」——`derive(PartialEq)` 会让 `Unhashable == Unhashable` 成立,
    /// 那正好是「屏幕永久停在加 callback 那一帧」。
    ///
    /// 自证会变红:给 `FrameFp` 加 `derive(PartialEq)` 并把 `same_as` 改成 `self == other`。
    #[test]
    fn a_paint_callback_frame_is_never_considered_unchanged() {
        assert!(!FrameFp::Unhashable.same_as(&FrameFp::Unhashable));
        assert!(!FrameFp::Unhashable.same_as(&FrameFp::Hash(7)));
        assert!(!FrameFp::Hash(7).same_as(&FrameFp::Unhashable));
        assert!(FrameFp::Hash(7).same_as(&FrameFp::Hash(7)));
    }

    /// 换字体族 / 字号 / DPI 必须判「变了」。
    ///
    /// 这几样一个都不进行指纹(行指纹只认内容和颜色),漏掉的症状是
    /// **换完字体屏幕停在旧字体的那一帧上**。
    ///
    /// 自证会变红:把 `frame_fingerprint` 里的 `hash_style` 调用删掉。
    #[test]
    fn changing_the_font_changes_the_fingerprint() {
        let s = snap_of("hello");
        let panes = [PaneRender { geom: geom(), snap: &s, focused: true, preedit: "" }];
        let jobs = [mesh_job(1.0)];
        let base = frame_fingerprint(&jobs, &panes, true, style(), (800, 600));
        let bigger = StyleKey { font_px: 18.0, ..style() };
        let other_family = StyleKey { family: "Consolas", ..style() };
        assert!(
            !base.same_as(&frame_fingerprint(&jobs, &panes, true, bigger, (800, 600))),
            "字号变了指纹没变"
        );
        assert!(
            !base.same_as(&frame_fingerprint(&jobs, &panes, true, other_family, (800, 600))),
            "字体族变了指纹没变"
        );
    }

    /// 窗口尺寸变了必须判「变了」。尺寸不进 egui 顶点也不进行指纹。
    ///
    /// 自证会变红:把 `frame_fingerprint` 里那两句 `h.u32(surface.*)` 删掉。
    #[test]
    fn resizing_the_surface_changes_the_fingerprint() {
        let s = snap_of("hello");
        let panes = [PaneRender { geom: geom(), snap: &s, focused: true, preedit: "" }];
        let jobs = [mesh_job(1.0)];
        let a = frame_fingerprint(&jobs, &panes, true, style(), (800, 600));
        let b = frame_fingerprint(&jobs, &panes, true, style(), (801, 600));
        assert!(!a.same_as(&b), "窗口尺寸变了指纹没变");
    }

    /// 拖动分屏分界线(几何变了、内容没变)必须判「变了」。
    ///
    /// 自证会变红:把 `hash_pane` 里的 `hash_geom` 调用删掉。
    #[test]
    fn dragging_a_split_changes_the_fingerprint() {
        let s = snap_of("hello");
        let jobs = [mesh_job(1.0)];
        let wide = geom();
        let narrow = PaneGeom {
            term_px: PxRect { x: 0, y: 0, w: 400, h: 600 },
            ..geom()
        };
        let a = frame_fingerprint(
            &jobs,
            &[PaneRender { geom: wide, snap: &s, focused: true, preedit: "" }],
            true,
            style(),
            (800, 600),
        );
        let b = frame_fingerprint(
            &jobs,
            &[PaneRender { geom: narrow, snap: &s, focused: true, preedit: "" }],
            true,
            style(),
            (800, 600),
        );
        assert!(!a.same_as(&b), "pane 几何变了指纹没变 —— 拖分界线画面不跟");
    }

    /// 两块 pane 内容对调必须判「变了」——关掉中间一块时其后所有 pane
    /// 会整体挪位,判成「没变」就是张冠李戴,屏幕上两块 pane 的内容互换。
    ///
    /// 自证会变红:把 `frame_fingerprint` 里对 `panes` 的 `for` 改成先按
    /// 某种规范序排序再哈希。
    #[test]
    fn swapping_two_panes_changes_the_fingerprint() {
        let a = snap_of("aaa");
        let b = snap_of("bbb");
        let g2 = PaneGeom { id: PaneId(2), ..geom() };
        let jobs = [mesh_job(1.0)];
        let ab = frame_fingerprint(
            &jobs,
            &[
                PaneRender { geom: geom(), snap: &a, focused: true, preedit: "" },
                PaneRender { geom: g2, snap: &b, focused: false, preedit: "" },
            ],
            true,
            style(),
            (800, 600),
        );
        let ba = frame_fingerprint(
            &jobs,
            &[
                PaneRender { geom: geom(), snap: &b, focused: true, preedit: "" },
                PaneRender { geom: g2, snap: &a, focused: false, preedit: "" },
            ],
            true,
            style(),
            (800, 600),
        );
        assert!(!ab.same_as(&ba), "两块 pane 的内容对调却判成没变");
    }

    /// `textures_delta` 非空的帧**一律**判 miss。
    ///
    /// 那两份 delta 是 egui 每帧 drain 出来、只交付一次的(字体图集的新
    /// 字形栅格 / 纹理回收)。指纹命中就跳的话它被静默丢弃,之后某帧引用
    /// 一张从未上传的纹理 —— 花屏或 panic,且只在「先命中、后未命中」的
    /// 序列里发作,无头测试完全够不到。
    ///
    /// 自证会变红:把 `can_skip` 里的 `deltas_empty &&` 去掉。
    #[test]
    fn a_pending_texture_delta_forces_a_miss() {
        let fp = FrameFp::Hash(7);
        assert!(can_skip(Some(&FrameFp::Hash(7)), &fp, true));
        assert!(
            !can_skip(Some(&FrameFp::Hash(7)), &fp, false),
            "有待交付的纹理增量却跳了帧 —— 增量会被静默丢弃"
        );
    }

    /// 第一帧没有可比对的上一帧,必须画。
    ///
    /// 自证会变红:把 `can_skip` 里的 `prev.is_some_and(..)` 改成
    /// `prev.map_or(true, ..)`。
    #[test]
    fn the_first_frame_is_never_a_hit() {
        assert!(!can_skip(None, &FrameFp::Hash(7), true));
    }

    /// `GridSnapshot` 加了新字段却没进整帧指纹,症状是屏幕留着陈旧的一帧。
    ///
    /// 那个结构体有私有字段,crate 外**无法穷尽解构**,所以只能靠扫源码。
    /// 字段覆盖面本身由 `mullion-term` 那边 `hash_row` 的穷尽解构 + 六条
    /// 逐字段测试守着;这条只负责在「结构体长出新字段」时把人拦下来。
    ///
    /// 自证会变红:往 `GridSnapshot` 里加一个 `pub display_offset: usize,`。
    #[test]
    fn the_snapshot_has_not_grown_a_field_behind_the_fingerprints_back() {
        let src = include_str!("../../mullion-term/src/snapshot.rs");
        let body = src
            .split("pub struct GridSnapshot {")
            .nth(1)
            .expect("找不到 GridSnapshot 的定义 —— 切片失效,断言会恒绿")
            .split("\n}")
            .next()
            .expect("GridSnapshot 的定义没有结尾");
        let fields: Vec<&str> = body
            .lines()
            .map(str::trim)
            .filter(|l| l.ends_with(',') && !l.starts_with("///") && !l.starts_with("//"))
            .collect();
        assert_eq!(
            fields,
            vec![
                "pub cols: u16,",
                "pub rows: u16,",
                "pub cells: Vec<SnapCell>,",
                "pub cursor: Cursor,",
                "row_hash: Vec<u64>,",
            ],
            "GridSnapshot 的字段变了 —— 回 `frame_fp::hash_pane` 决定新字段进不进整帧指纹"
        );
    }
}
```

同时在 `crates/mullion-app/src/lib.rs` 的 `pub mod frame;` 之后加一行：

```rust
pub mod frame_fp;
```

- [ ] **Step 2: 跑测试确认每条断言都真的会红**

先跑一遍确认全绿：

Run: `cargo test -p mullion-app --lib frame_fp:: 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: `test result: ok.`（15 条）

再逐条验证「自证会变红」（**这一步不能跳**，源码切片与哈希类测试在本项目有已知的恒绿模式）。挑三条最关键的做变异，每次改完跑一遍确认变红、再 `git checkout` 还原：

```bash
# ① 删掉 preedit 分量 → typing_pinyin_... 必须红
# ② 把 hash_pane 的 style_for(...) 换成 blink_on → only_the_focused_pane_... 必须红
# ③ 去掉 can_skip 的 deltas_empty && → a_pending_texture_delta_forces_a_miss 必须红
cargo test -p mullion-app --lib frame_fp:: 2>&1 | grep -E "test result|FAILED"
git checkout crates/mullion-app/src/frame_fp.rs
```

**做变异之前先 `git stash` 或确保工作区已提交** —— 本项目历史上两次因为 `git checkout` 把未提交的编辑一起吞掉而返工。

- [ ] **Step 3: 给 `TextLayer` 加 `style_key`**

在 `crates/mullion-app/src/text.rs` 的 `fn family_name` 之后插入：

```rust
    /// F159:影响文字层最终长相、但不进任何行指纹的样式量。整帧指纹吃它。
    ///
    /// 字体族 / 字号 / DPI 换了而所有行的内容都没变时,行指纹一个都不会变
    /// —— 少了这一项,换完字体屏幕会停在旧字体的那一帧上,编译/测试/日志
    /// 全静默(F12 的 `set_font` 显式清缓存治的是另一半:整形结果作废)。
    pub fn style_key(&self) -> crate::frame_fp::StyleKey<'_> {
        crate::frame_fp::StyleKey {
            family: self.family_name(),
            font_px: self.font_px,
            cell_w: self.cell_w,
            cell_h: self.cell_h,
            default_fg: self.default_fg,
        }
    }
```

- [ ] **Step 4: 跑全量 + clippy**

Run:
```bash
cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED|panicked"
cargo clippy -p mullion-app --all-targets -- -D warnings 2>&1 | tail -20
```
Expected: 测试全 `ok.`，clippy 无输出

若 clippy 报 `Primitive` 的 `match` 非穷尽（`#[non_exhaustive]`），补一条 `_ => return false,` 并在旁边写明理由：保守方向，多画一帧永不少画。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/frame_fp.rs crates/mullion-app/src/lib.rs crates/mullion-app/src/text.rs
git commit -m "feat(app): 整帧指纹纯函数层——egui 顶点+行指纹+光标+preedit+几何 (F159)"
```

---

## Task 8: F159 接线进 `render_frame`

**Files:**
- Modify: `crates/mullion-app/src/app.rs`（`struct Active` 加字段、`Active` 构造处、`render_frame` 三处）
- Test: `crates/mullion-app/src/app.rs` 的 `mod tests`

**一条硬约束，写在代码注释里也写在这里：跳帧判断必须留在 `render_frame` 内部，不得挪到调用方侧。**

`limiter.record_present(now)`、`ui_dirty = false`、`pacer.mark_presented(now)`、同步块收口、几何施加——这些全在**调用方**（`App::window_event` 的 `Present` 分支，`render_frame` 返回之后）无条件执行，现有的 surface Timeout / AtlasFull 提前 return 也是被同一段兜住的。所以命中时在函数内部提前 `return` 即可，**什么都不用补**。

挪出去就得手工重做上面每一笔记账；漏掉 `pacer.mark_presented` 一笔，`panes_ready_to_present` 恒真 → `terminal_dirty` 恒真 → 每帧醒来算一次指纹，退化回 60fps 空转，而剖面里 `present` 反而是 0——症状极具迷惑性。

- [ ] **Step 1: 写会失败的测试**

在 `crates/mullion-app/src/app.rs` 的 `mod tests` 末尾加：

```rust
    /// F159:指纹命中必须在**任何 GPU 工作之前**提前 return,而基准只能由
    /// **真正提交过**的帧更新。
    ///
    /// 三条各自钉一个「编译过、跑起来才发作」的错法:
    ///
    /// - 提前 return 排在 `get_current_texture` 之后 → 每帧照样占一次交换链,
    ///   Fifo 下照样等一个 vsync,收益归零而剖面看起来一切正常。
    /// - 基准在提前 return 的路径上也更新 → prepare 失败 / acquire 失败那几帧
    ///   没画出去却成了基准,下一帧误判命中,屏幕停在更早的一帧上。
    /// - 判断挪到调用方侧 → 得手工重做 `record_present`/`mark_presented` 那几笔
    ///   记账,漏一笔就是 60fps 空转且 `present=0`。
    ///
    /// 自证会变红:把 `a.last_frame_fp = Some(fp);` 挪到 `frame.present();` 之前。
    #[test]
    fn a_fingerprint_hit_returns_before_any_gpu_work_and_only_a_presented_frame_becomes_the_baseline(
    ) {
        let src = include_str!("app.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .unwrap_or(src);
        assert!(
            prod.len() < src.len(),
            "没能切掉测试模块 —— 会搜到测试自己的文本,断言恒绿"
        );
        let at = |needle: &str| {
            assert_eq!(
                prod.matches(needle).count(),
                1,
                "`{needle}` 在生产段里不是恰好一处,下面的先后判断会指错地方"
            );
            prod.find(needle).expect("上面刚断言过存在")
        };
        let fp = at("crate::frame_fp::frame_fingerprint(");
        let skip = at("if crate::frame_fp::can_skip(");
        let acquire = at("a.gpu.surface.get_current_texture()");
        let present = at("frame.present();");
        let baseline = at("a.last_frame_fp = Some(fp);");
        assert!(fp < skip, "先判跳帧后算指纹,顺序反了");
        assert!(
            skip < acquire,
            "指纹命中的提前 return 排在 acquire 之后 —— 每帧照样占一次交换链,收益归零"
        );
        assert!(
            present < baseline,
            "没画出去的帧也成了下一帧的比对基准 —— 屏幕会停在更早的一帧上"
        );
    }

    /// F159:surface 被重新 configure 之后,交换链内容未定义,基准必须作废。
    ///
    /// 不作废的症状:窗口从遮挡/丢失中恢复之后画面全黑或残留旧内容,而
    /// 且只在触发过 `SurfaceError::Lost`/`Outdated` 的机器上出现。
    ///
    /// 自证会变红:把 Lost/Outdated 分支里那句 `a.last_frame_fp = None;` 删掉。
    #[test]
    fn reconfiguring_the_surface_drops_the_fingerprint_baseline() {
        let src = include_str!("app.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .unwrap_or(src);
        assert!(prod.len() < src.len(), "没能切掉测试模块,断言会恒绿");
        let arm = prod
            .split("a.gpu.surface.configure(&a.gpu.device, &a.gpu.config);")
            .nth(1)
            .expect("找不到 surface 重新 configure 的分支");
        let head = &arm[..arm.len().min(300)];
        assert!(
            head.contains("a.last_frame_fp = None;"),
            "重新 configure 之后没作废指纹基准:{head}"
        );
    }
```

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test -p mullion-app --lib a_fingerprint_hit reconfiguring_the_surface 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: 两条都 FAILED

- [ ] **Step 3: 写实现（四处）**

**3a.** `struct Active` 里，`egui_renderer: egui_wgpu::Renderer,` 之后加：

```rust
    /// F159:上一次**真正提交给 GPU** 的那一帧的整帧指纹。
    ///
    /// `None` = 没有可比对的上一帧(首帧,或 surface 刚被重新 configure ——
    /// 那之后交换链内容未定义,拿旧基准比会让画面停在更早的一帧上)。
    last_frame_fp: Option<crate::frame_fp::FrameFp>,
```

**3b.** `self.active = Some(Active {` 的构造里加一行：

```rust
            last_frame_fp: None,
```

**3c.** `render_frame` 里，`diag::note_repaint_delay(repaint_delay);`（Task 4 加的）之后、`// 每帧先 trim` 之前插入：

```rust
    // --- F159:整帧指纹。画出来跟上一帧一模一样就不提交 GPU。---
    //
    // **判在结果上,不判在原因上**(与 F12 的行指纹同一条推理,见 ADR-011):
    // 能改变「这一帧长什么样」的来源列举不完,漏一个的症状是屏幕留着陈旧的
    // 一帧,编译/测试/日志全静默。
    //
    // 截断点选在这里(tessellate 之后、终端趟之前):终端侧的全部输入
    // (行指纹来自快照、几何来自 `compute_geoms`、光标相位由调用方算好)
    // 在这个位置**已经全部就绪**,不需要先付 `text_prepare` 那几毫秒才知道
    // 结果没变。egui pass **照跑不跳** —— 它是指纹的真值来源,也是 tooltip /
    // 菜单动画能继续推进的前提(动画在推进 → 顶点变了 → 指纹不同 → 照常出帧)。
    let fp = crate::frame_fp::frame_fingerprint(
        &paint_jobs,
        panes,
        blink_on,
        a.text.style_key(),
        (a.gpu.config.width, a.gpu.config.height),
    );
    // egui 的纹理增量是**每帧 drain 出来、只交付一次**的,非空时一律强制
    // miss(理由见 `frame_fp::can_skip` 的文档)。
    let deltas_empty =
        full_output.textures_delta.set.is_empty() && full_output.textures_delta.free.is_empty();
    if crate::frame_fp::can_skip(a.last_frame_fp.as_ref(), &fp, deltas_empty) {
        diag::count_frame_fp(true);
        // 提前 return 即可,**什么都不用补**:`limiter.record_present` /
        // `ui_dirty = false` / `pacer.mark_presented` / 同步块收口 / 几何施加
        // 全在**调用方**(`App::window_event` 的 `Present` 分支)本函数返回
        // 之后无条件执行,现有的 surface Timeout / AtlasFull 提前 return
        // 也是被同一段兜住的。
        //
        // **这个判断不许挪到调用方侧**:挪出去就得手工重做上面每一笔记账;
        // 漏掉 `pacer.mark_presented` 一笔,`panes_ready_to_present` 恒真 →
        // `terminal_dirty` 恒真 → 每帧醒来算一次指纹,退化回 60fps 空转,
        // 而剖面里 `present` 反而是 0 —— 症状极具迷惑性。
        //
        // 返回**真实的** `repaint_delay`(不是别的提前 return 用的
        // `Duration::MAX`):egui 可能正在推进一段动画,那一路的排期不能被
        // 「这一帧画面没变」吃掉。
        //
        // `a.text.trim()` 被跳过是安全的:`trim` 存在的意义是让下一次
        // `prepare` 能淘汰旧字形,本帧既然不 `prepare`,也就不会有新字形进
        // 图集,图集不会增长。(原注释强调 `trim` 必须排在 `AtlasFull` 的
        // 提前 return 之前 —— 那是因为那条路径**已经 prepare 过了**。)
        return (repaint_delay, actions);
    }
    diag::count_frame_fp(false);
```

**3d.** `render_frame` 里，Lost/Outdated 分支加一句、函数末尾加一句：

```rust
        Err(e @ (wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated)) => {
            log::warn!(target: "mullion", "wgpu surface {e:?},重新 configure 后跳过本帧");
            a.gpu.surface.configure(&a.gpu.device, &a.gpu.config);
            // F159:重新 configure 之后交换链内容未定义,旧基准作废 ——
            // 留着的话下一帧会误判命中,画面停在更早的一帧上。
            a.last_frame_fp = None;
            diag::count_skipped();
            return (std::time::Duration::MAX, actions);
        }
```

```rust
    for id in &full_output.textures_delta.free {
        a.egui_renderer.free_texture(id);
    }
    // F159:只有**真正提交过**的帧才成为下一帧的比对基准。任何提前 return
    // (prepare 失败 / acquire 失败 / surface 重配)都不更新它 —— 那些帧
    // 没画出去,拿它们当基准会让下一帧误判命中,屏幕停在更早的一帧上。
    a.last_frame_fp = Some(fp);

    (repaint_delay, actions)
```

- [ ] **Step 4: 跑测试确认通过**

Run:
```bash
cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED|panicked"
cargo clippy -p mullion-app --all-targets -- -D warnings 2>&1 | tail -20
```
Expected: 测试全 `ok.`，clippy 无输出

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): 整帧指纹接进 render_frame,命中即不提交 GPU (F159)"
```

---

## Task 9: spec.md 条目 + 版本号 + 全绿

**Files:**
- Modify: `spec.md`（§4 功能需求表，接在 F156-c 之后）
- Modify: `Cargo.toml`（`workspace.package.version`）

- [ ] **Step 1: 往 `spec.md` 的功能需求表末尾加三行**

在 `| F156-c |` 那一行之后追加（表头是 `编号 | 需求 | 优先级 | 守护/验收`）：

```markdown
| F157 | **帧循环归因**：剖面行追加 `wake=Nx/rr=sched:N,evt:N`、`dirty=行号:次数,…`、`egui_ev=Nx/f:N`、`rdelay=z:N/f:N/m:N` 四段，一次实机往返就能点名「谁在每帧置脏 / egui 收了几个事件 / `repaint_delay` 到底是什么」。**社区标准答案 `Context::repaint_causes()` 对这个成因无效**——`RepaintCause::new()` 带 `#[track_caller]`，而 egui 自动重绘的调用点是它自己的 `context.rs:2396/2398`，吐出来的只会是 egui 的内部行号；归因必须埋在我们这一侧的边界上。80 处 `self.ui_dirty = true` 全部收口到 `mark_ui_dirty!` 宏（是宏不是方法：有些置脏点在 `self.active` 的可变借用作用域里，调不了 `&mut self` 方法），行号由 `line!()` 拿；归因表是固定 8 槽的无锁表（帧路径上不许分配/加锁/格式化，T3），一窗口没响过的槽位归还，否则启动期的一次性置脏点会把槽位永久占死。`repaint_delay` **三分**桶：`Duration::MAX`（egui 不需要重绘）必须与「很大的有限值」（要重绘只是可以等）分开，归成一类的话两种截然不同的状态在日志里长得一样 | P1 | `profile::tests::a_huge_finite_repaint_delay_is_not_the_same_as_never`；`diag::tests` 三条（槽位分辨/槽位归还/drain 清零，用自己的表实例避开并行 runner 的假红）；接线守护 `every_ui_dirty_set_site_goes_through_the_attribution_macro`（生产段里 `ui_dirty = true` 必须恰好 0 处）/ `every_request_redraw_records_where_it_came_from`（调用数与记账数必须相等）/ `the_wake_counter_sits_at_the_very_top_of_the_redraw_arm`（排在 `pump_io` 之后的话，最小化路径完全不计数）。**新增四段一律不进 `is_idle`**——它们在空闲时恰恰非零，进了判据就是「空闲的 mullion 每 5 秒写一次盘」 |
| F158 | **launcher 无条件出帧下线**：`let dirty` 那个 `match` 整个删掉，两态统一走 `frame::frame_is_dirty(terminal_dirty, self.ui_dirty)`（`terminal_dirty` 在 launcher 态本来就恒 `false`，天然统一，**不需要**再包一层带 `has_workspace` 的函数——那参数会被忽略）。原来那句 `None => true` 的理由是「`ControlFlow::Wait` 下 winit 不会凭空生成 `RedrawRequested`」，而它在同一函数别处会排 `WaitUntil` 的前提下不成立：present 后拿到有限 `repaint_delay` → 排 `WaitUntil` → `about_to_wait` 到点补 `request_redraw` → 闭环自激。日志坐实 `tabs=0 panes=0` 时照样 `frame=300x/present=300`。摘掉之后 `ui_dirty` 成为 launcher 态唯一判据，缓解②把后台事件的判据**反过来**：`user_event_marks_dirty` 是穷尽 `match`（加变体即编译报错），默认标脏，只豁免三种：`Wake` / `TransferProgress`（每秒几千条，标脏就是 T3）与 `EditTick`（理由不同——它的分支把 `self.ui_dirty` **当信号读**，在这里预置会让 `if self.ui_dirty` 恒真、语义静默改掉）。**必须与 F159 同版**——只做 F158 会让 80 个置脏点 : 1 个清脏点的列举式结构成为唯一判据 | P1 | `the_frame_dirty_check_is_the_same_in_both_states`（源码切片断言 `dirty` 的绑定式**恰好**是那一句；变异：改回 `match … None => true` 当场变红）；`a_background_event_marks_the_ui_dirty_unless_it_is_a_known_flood`；`the_background_dirty_rule_is_actually_wired_into_user_event`（纯函数写对了没接线是本项目反复踩过的静默失效）。**人工验收**：launcher 里点会话/开弹窗/hover/键盘选择全部要有反应；不碰鼠标发起一个连接，画面要自己更新到「已连接」 |
| F159 | **整帧指纹**：`FNV-1a(egui tessellate 产物 ⊕ 各 pane 的 F12 行指纹 ⊕ 光标 ⊕ IME preedit ⊕ 几何 ⊕ 字体样式 ⊕ 交换链尺寸)`，与上一帧相同**且 `textures_delta` 两个方向都为空**就不提交 GPU。截断点在 tessellate 之后、终端趟之前——终端侧输入那时已全部就绪，不必先付 `text_prepare` 才知道没变；egui pass 照跑不跳（它是指纹的真值来源，也是动画能推进的前提）。四条不能省的细节：①`textures_delta` 非空**强制 miss**（那是 egui 每帧 drain、只交付一次的字体图集增量，跳掉就永久丢，之后某帧引用从未上传的纹理 → 花屏或 panic，只在「先命中后未命中」的序列里发作，无头够不到）；②**preedit 必须入指纹**（画在终端文字层不在 egui paint_jobs 里，组字过程中 cells 不变行指纹不变，漏掉就是**打拼音屏幕纹丝不动**，T10 一族）；③光标闪烁吃 `gpu::style_for` 的**结果**而非裸 `blink_on`（吃裸值的话 launcher 也跟着相位每秒白出 2 帧，非焦点 pane 恒画空心光标也会被算成变了）；④`FrameFp` **刻意不 `derive(PartialEq)`**（`Unhashable == Unhashable` 会被 derive 判成 `true`，那正好是「加了 paint callback 之后屏幕永久停住」）。跳帧判断**必须留在 `render_frame` 内部**：记账全在调用方无条件执行，挪出去漏掉 `mark_presented` 一笔就是 60fps 空转且剖面 `present=0` | P1 | `frame_fp::tests` 15 条，每条配一个自证会变红的变异：内容/划选/组字串/焦点相位/launcher 不churn/egui 顶点/callback 永不命中/字体/尺寸/几何/pane 对调/delta 强制 miss/首帧必画/`GridSnapshot` 长新字段时源码切片变红；接线守护 `a_fingerprint_hit_returns_before_any_gpu_work_and_only_a_presented_frame_becomes_the_baseline`（三个 byte offset 的先后关系）/ `reconfiguring_the_surface_drops_the_fingerprint_baseline`。运行期守护 `fp=hit:N/miss:N`（判据写错导致永远 miss 时画面完全正确、日志一切正常，只有这个比值会掉）。**验不了**：实机 CPU、是否不闪、真实字形/CJK 对齐、输入法候选框 |
```

- [ ] **Step 2: 升版本号**

`Cargo.toml` 的 `[workspace.package]`：

```toml
version = "0.1.68"
```

- [ ] **Step 3: 跑全绿**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

Expected：所有 `test result: ok.`；clippy 与 fmt 无输出。

**「绿」的定义**：`cargo test --workspace` 全过 **且** `clippy -D warnings` 无输出。只跑单个 crate 不叫绿。

- [ ] **Step 4: 提交**

```bash
git add spec.md Cargo.toml
git commit -m "chore: 版本 0.1.68(空闲帧归因 + launcher 停止无条件出帧 + 整帧指纹)"
```

---

## 全部任务完成之后

按 `CLAUDE.md` 的交付约定一条龙走完 `.claude/skills/release-windows/SKILL.md`：交叉编译 → objdump 验收 → 签名 → 发 GitHub Release（走代理）→ 报链接与人工验收清单。

`notes.md` 的人工验收清单直接照抄设计文档 §9 的十条，**第 1、2、3、5、8 条最重要**：

1. launcher 静置 1 分钟，`present=` 应接近 0（改动前是 300/5s）。
2. 4 分屏各挂 tmux + Claude Code 静置 1 分钟，读 `wake=` 与 `present=`。**预期 `present` 明显下降、`wake` 基本不变**——`wake` 不降是本版已知的（设计文档 §7），不是失手。
3. **归因四段必须有非零内容**。`dirty=-` = 埋点没接上。把这一行原样贴回来，它决定下一版改什么。
5. launcher 态发起一个连接，**不碰鼠标键盘**，看画面会不会自己更新到「已连接」。
8. 中文输入法：组字时拼音串要**逐键跟着变**。

**本版的 CPU 会降但降不到 xshell 的 0.2%，这是设计上就知道的**（设计文档 §7）：真正的开关在 egui 的 `wants_repaint_after()` 哪一条判据每帧成立，只能靠 F157 的实机日志拿到，拿到之后另开一版。
