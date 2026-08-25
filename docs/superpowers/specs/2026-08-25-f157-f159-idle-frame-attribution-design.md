# F157–F159 设计：空闲帧归因 + 整帧指纹

- 日期：2026-08-25
- 状态：已批准（grilling 会话逐条确认）
- 关联：N3（重绘频率 ≤ 刷新率）、领域陷阱 T3/T7、[ADR-011](../../adr-011-row-fingerprint-vs-term-damage.md)、F12、F125、F155
- 目标版本：v0.1.68

---

## 1. 背景

F12（v0.1.67）把「每帧对每个 pane 的每一行重做文本整形」改成按行指纹跳过未变行，
实机 CPU 明显下降。但仍高于同机 xshell 的 0.2%，更高于 pwsh + ssh 的 0%。

对 `/tmp/mullion.log`（v0.1.66）的分析给出了原因，**它不在单帧成本上**。

挑一条**完全空闲**的剖面行（2 标签 1 pane、`in=0B/s` 远端一个字节没来、无键鼠输入）：

```
frame=300x  present=300  skip=0  throttle=1689
redraw=term:0/ui:1989/both:0    window_event=1989x    acquire=300x/p95=16.4ms
```

三条互相印证的事实：

1. **`present=300 / 5s` = 雷打不动 60 fps**，画的是一模一样的一屏。
2. **`throttle=1689 / 5s` ≈ 每秒醒来 340 次**被帧闸打回。合计唤醒率 ≈ 398/s。
3. **`window_event` 与 `redraw ui` 每一行都精确相等**（1989 vs 1989、1471 vs 1471、
   839 vs 839；鼠标一动就不等了，59407 vs 40536）。空闲时窗口收到的每一个事件
   都是它自己发出去的 `RedrawRequested` —— 闭环自激，不是 OS 推的，也不是网络推的。

F12 治的是「每帧多贵」，剩下的问题是「凭什么还在出帧」。
**xshell / pwsh 空闲时出 0 帧，我们出 60 帧**；单帧成本再削也跨不过 60-vs-0。

### 1.1 已定位的两条自激回路

**① launcher 态无条件出帧**（`app.rs:8353`）：

```rust
let dirty = match self.active_ws() {
    Some(_) => frame_is_dirty(terminal_dirty, self.ui_dirty),
    None => true,     // ← 没连任何东西时，永远脏
};
```

日志坐实：`tabs=0 panes=0` 时照样 `frame=300x/present=300`。
旁边注释的理由（「`ControlFlow::Wait` 下 winit 不会凭空生成 `RedrawRequested`」）
在同一函数别处会排 `WaitUntil` 的前提下不成立。

**② 终端态 `ui_dirty` 清了又立刻被置回**：`app.rs:8068` present 后清零 →
`app.rs:8365` 因 egui 返回有限 `repaint_delay` 立刻置回 `true`。

算术对得上：`ui_dirty` 每 5 秒为真 1989 次 ≈ 398/s，与总唤醒率
（`throttle 1689 + present 300` = 398/s）**完全相等**；而它每帧只在 present 后
被清一次（300 次）。唯一自洽的解释是——**`ui_dirty` 每帧被置真恰好一次，
就在 8365**，然后跨越那 5.6 次被节流的唤醒（`Throttle` 分支不清脏），
直到下一次 present 清掉。

也就是说：**egui 每一帧都返回了有限的 `repaint_delay`**。

### 1.2 还不知道的那一个

egui 0.30 的 `context.rs:2384-2399`，每趟 pass 结束时：

```rust
if self.memory.options.repaint_on_widget_change {   // 默认 false，我们没开
    if viewport.prev_pass.widgets != viewport.this_pass.widgets { repaint_needed = true; }
}
if repaint_needed {
    self.request_repaint(ended_viewport_id, RepaintCause::new());
} else if let Some(delay) = viewport.input.wants_repaint_after() {
    self.request_repaint_after(delay, ended_viewport_id, RepaintCause::new());
}
```

而 `wants_repaint_after()`（`input_state/mod.rs:495`）第一条判据是：

```rust
if self.pointer.wants_repaint()                  // pointer_events 非空 || delta != ZERO
    || self.unprocessed_scroll_delta.abs().max_elem() > 0.2
    || self.unprocessed_scroll_delta_for_zoom.abs() > 0.2
    || !self.events.is_empty()                   // ← 本趟收到任何输入事件
{ return Some(Duration::ZERO); }
```

我们这侧的 `raw_input` 全部来自 `egui_state.on_window_event`（`app.rs:7188`），
没有任何合成注入。所以真空闲时 `events` 本该为空、`repaint_delay` 本该是 `MAX`。
**日志说不是。剩下的唯一未知量就是：这两条判据里哪一条每帧都成立。**

### 1.3 为什么不能用社区标准答案 `repaint_causes()`

社区（[egui #1261](https://github.com/emilk/egui/discussions/1261)）给的定位工具是
`Context::repaint_causes()`。**它对这个成因无效。**

`RepaintCause::new()` 带 `#[track_caller]`，记的是 `Location::caller()`；
而上面两处调用的 caller 就是 **egui 自己的 `context.rs:2396/2398`**。
所以 `repaint_causes()` 只认得出**我们主动**调 `request_repaint` 的三处
（`ui/toast.rs:54`、`ui/host_key.rs:74`、`ui/annotate.rs:707`，空闲态一个都不成立），
对自动重绘只会吐一行 egui 内部行号。

**归因必须埋在我们这一侧的边界上。**

---

## 2. 目标与非目标

### 目标

- 空闲时（终端态、远端安静、无输入）的 present 频率降到 **~2/s**。
- 一次实机往返就能点名「谁在每帧置脏」，不留未知量。
- 判脏漏标不再能造成静默丢帧。

### 非目标（明确不做）

- **不追求空闲 0 帧。** F125 的光标闪烁 `BLINK_HALF_MS = 530`，聚焦的 pane
  每秒必须翻转 ~2 次相位。真正的天花板是 60 → ~2 present/s（约 30 倍），不是 0。
  xshell 也闪光标，但它走 GDI/D2D 局部失效、只重画光标那一格，不出整帧；
  在 wgpu 上做局部呈现是另一个量级的工程，本轮不碰。
- **本轮治不好终端态的唤醒率。** 见 §7 的预期表。这是设计上就知道的。
- 不改 `PresentMode`、不改 `desired_maximum_frame_latency`。见 §8 被否的备选。

---

## 3. 三个特性

| 编号 | 名称 | 一句话 |
|---|---|---|
| **F157** | 帧循环归因 | 剖面行报「谁置的脏 / egui 收了几个事件 / `repaint_delay` 是什么 / 醒了几次」 |
| **F158** | launcher 无条件出帧下线 | 判脏统一走 `frame_is_dirty`，不再有「没连东西就永远脏」的兜底 |
| **F159** | 整帧指纹 | 画面跟上一帧一模一样就不提交 GPU |

F158 摘掉的是一层**列举式**兜底，F159 补上的是一层**构造式**兜底。两者必须同版落地
——只做 F158 会让 `ui_dirty` 成为唯一判据，而它现在是 **80 个置脏点 : 1 个清脏点**
的结构，漏标一处的症状是「点了没反应 / 连上了画面不动」，编译测试日志全静默。
按 `MEMORY.md` 的记录，「列举式门控在加档时必然漏」在本项目已踩中三次。

ADR-011 三周前刚为 F12 写下过同一条推理：**判据要放在结果上，不放在原因上。**

---

## 4. F157：帧循环归因

### 4.1 剖面行新增四段

追加在现有 `reshape=hit:/miss:` 之后、`conn=` 之前：

```
wake=1990x/rr=sched:1689,evt:2  dirty=8365:300,7191:2  egui_ev=0x/f:0  rdelay=z:300/f:0/m:0
```

| 段 | 含义 |
|---|---|
| `wake` | 本窗口收到多少次 `RedrawRequested`（唤醒率的直接读数） |
| `rr` | 我们**主动**调了多少次 `request_redraw`，按来源分：`sched` = `about_to_wait` 到点补画；`evt` = 因窗口事件请求（`app.rs:4770/6464/7191`） |
| `dirty` | `ui_dirty` 被置真的来源行号 × 次数，倒序取前三 |
| `egui_ev` | 喂给 egui 的事件总数 `x` / 其中有事件的帧数 `f` |
| `rdelay` | `repaint_delay` 分桶：`z` = `Duration::ZERO`，`f` = 有限非零，`m` = `MAX` |

`wake` 与 `rr` 的差值不为零是**正常**的：多次 `request_redraw` 会被 winit 合并成一次
`RedrawRequested`，OS 也会主动发（窗口被遮挡后暴露）。差值本身就是信息，
**不试图把它归成精确的三类**——那需要一个「最近一次请求来源」的单槽标记，
而合并会让它系统性失真。宁可报两个诚实的数，不报一个精确但错的分类。

### 4.2 `ui_dirty` 收成方法

80 处 `self.ui_dirty = true;`（`grep -c` 实测，全部在 `app.rs`）机械替换为
`self.mark_ui_dirty();`：

```rust
/// 标记 egui 侧需要重绘。**唯一的置脏入口。**
///
/// `#[track_caller]`:F157 的归因靠它拿到调用点行号。直接写字段赋值等于
/// 在归因表上开一个洞,而洞的症状是「剖面里少了一行、看起来一切正常」。
#[track_caller]
fn mark_ui_dirty(&mut self) {
    self.ui_dirty = true;
    crate::diag::note_ui_dirty(std::panic::Location::caller().line());
}
```

`note_ui_dirty` 的帧路径开销必须可忽略（T3）：**不分配、不加锁、不格式化**。

实现：固定 8 槽的无锁表。

```rust
const DIRTY_SITES: usize = 8;
static DIRTY_LINE: [AtomicU32; DIRTY_SITES] = [const { AtomicU32::new(0) }; DIRTY_SITES];
static DIRTY_HITS: [AtomicU64; DIRTY_SITES] = [const { AtomicU64::new(0) }; DIRTY_SITES];
/// 槽位用完之后落到这里,报为 `dirty=...,other:N`。
static DIRTY_OTHER: AtomicU64 = AtomicU64::new(0);
```

`note_ui_dirty(line)`：线性扫 8 个 `AtomicU32`，命中就 `DIRTY_HITS[i].fetch_add(1)`；
没命中就对空槽（值为 0）做一次 `compare_exchange` 抢占；全满则 `DIRTY_OTHER` 加一。
最坏 8 次 relaxed load + 1 次 CAS，与现有 `diag::mark` 同量级。

**只报行号、不报文件名**：所有置脏点都在 `app.rs`。这一条由守护测试钉死
（扫源码断言 `mark_ui_dirty(` 只出现在 `app.rs` 与本测试自身）。
存文件名要么存 `&'static str` 的裸指针（不安全还原），要么加锁（帧路径上不行）。

### 4.3 egui 侧两段

在 `render_frame`（`app.rs:9804` 一带）：

```rust
let raw_input = a.egui_state.take_egui_input(&a.window);
crate::diag::note_egui_events(raw_input.events.len());
```

在 `repaint_delay` 取出之后（`app.rs:9876`）：

```rust
crate::diag::note_repaint_delay(repaint_delay);
```

分桶判据写成纯函数以便单测：

```rust
/// `repaint_delay` 落在哪个桶。0 / 有限非零 / MAX 三分。
///
/// **`MAX` 必须与「很大的有限值」分开**:前者是「egui 不需要重绘」,
/// 后者是「egui 要重绘只是可以等」——归成一类的话,剖面里
/// 「egui 一次都没要过重绘」和「egui 每帧都要但可以等 10 秒」长得一样。
pub fn repaint_bucket(d: std::time::Duration) -> RepaintBucket
```

### 4.4 `is_idle` 不动

`profile.rs` 的 `is_idle()` 判据只看「有没有画过帧 / 收过字节 / 按过键 / 连接或 SFTP 动作」。
新增的四段**一律不进 `is_idle`**——`wake`/`dirty` 在空闲时恰恰非零（这正是要查的东西），
进了判据会让空闲的 mullion 每 5 秒写一次盘。这与 F12 的 `reshape_*` 处理一致。

---

## 5. F158：launcher 无条件出帧下线

### 5.1 改动

**不新增函数**。`app.rs:8352-8355` 那个 `match` 整个删掉：

```rust
// 上面已经有:launcher 态 terminal_dirty 恒 false
let terminal_dirty = match self.active_ws() {
    Some(ws) => crate::render::panes_ready_to_present(...),
    None => false,
};
...
// 改动前:launcher 态 (`None`) 无条件 true
// 改动后:两态同一条判据
let dirty = crate::frame::frame_is_dirty(terminal_dirty, self.ui_dirty);
```

`terminal_dirty` 在 launcher 态本来就恒为 `false`，所以两态判据天然统一，
**不需要**再包一层带 `has_workspace` 参数的函数——那个参数会被忽略，
是个纯粹的坏味道。

**守护测试**照抄本仓已有的 `count_redraw` 接线守护
（`app.rs:17098-17123`）的形制：源码切片断言 `dirty` 的绑定式恰好是
`frame::frame_is_dirty(terminal_dirty, self.ui_dirty)`，
配上 `frame::tests::egui_repaint_alone_is_dirty_enough`
里已有的 `assert!(!frame_is_dirty(false, false))`。

源码切片测试在本项目有已知的恒绿模式（见 `MEMORY.md`），
所以这条测试**必须写出自证会变红的变异**：把绑定式改回
`match self.active_ws() { Some(_) => ..., None => true }` 应当当场变红。

### 5.2 风险与缓解

摘掉之后 `ui_dirty` 成为 launcher 态的唯一判据。风险来源不是窗口事件
（`7191` 覆盖了），而是**后台线程送进来的 `UserEvent`**：连接结果、
`HostKeyPrompt`、传输进度、SFTP 完成。漏标一处 = 「连上了画面不动」。

三层缓解，按可靠性排序：

1. **F159 的整帧指纹**（构造式，见下）—— 漏标脏最多只是晚一帧，不会永久卡住。
   这是主要缓解手段，也是 F158/F159 必须同版的原因。
2. `UserEvent` 的处理入口收敛到一处集中置脏，而不是每个分支各自记得标。
3. 人工验收清单（§9）第 2、3 条。

---

## 6. F159：整帧指纹

### 6.1 判据

```
fp = FNV-1a( egui 的 tessellate 产物
           ⊕ 各 pane 的 F12 行指纹
           ⊕ 光标状态
           ⊕ IME preedit 状态
           ⊕ 几何 )
```

与上一帧相同 **且 `textures_delta` 为空**（见 6.2a）→ **不提交 GPU**：
跳过 `text.trim()`、终端趟的 `quads_for_panes`/`prepare_panes`、
`get_current_texture`、`encode`、`present`。

**记账全部照做，且这不是额外工作，是继承来的**：`limiter.record_present(now)`、
`ui_dirty = false`、`pacer.mark_presented(now)`、同步块收口、几何施加——
这些全在**调用方**（`app.rs:8068-8080`）`render_frame` 返回之后无条件执行，
现有的 surface Timeout / AtlasFull 提前 return 也是被同一段兜住的。
所以命中时在 `render_frame` 内部提前 return 即可，什么都不用补。

**硬约束：跳帧判断必须留在 `render_frame` 内部，不得挪到调用方侧。**
挪出去就得手工重做上面每一笔记账；漏掉 `pacer.mark_presented` 一笔，
`panes_ready_to_present` 恒真 → `terminal_dirty` 恒真 → 每帧醒来算指纹，
退化回 60fps 空转且剖面里 `present` 反而是 0，症状极具迷惑性。

### 6.2a `textures_delta` 非空必须强制 miss

`render_frame` 在 encode 段上传 `full_output.textures_delta.set`
（egui 字体图集的新字形栅格），present 之后释放 `.free`（`app.rs:9958/10006`）。
这两个 delta 是 egui **每帧 drain 出来、只交付一次**的——指纹命中就跳的话
delta 被静默丢弃，之后某帧引用一张从未上传的纹理，花屏或 panic，
且只在「先命中、后未命中」的序列里发作，无头测试完全够不到。

所以跳帧条件是 `fp 相同 && textures_delta.set.is_empty() && textures_delta.free.is_empty()`。
delta 非空的帧计入 `miss`（真实频率极低：字形首次栅格化、纹理回收）。
这条分支必须有守护测试：构造 `set` 非空的 `TexturesDelta`，断言判 miss。

### 6.2 截断点

指纹算在 `tessellate` 之后、终端趟之前（`app.rs` 的 `a.text.trim()` 一带）。
理由：终端侧的输入（行指纹、几何、光标）在那个位置**已经全部就绪**
（行指纹来自快照、几何来自 `compute_geoms`、光标来自 `blink_on`），
不需要先付 `text_prepare` 的 4.1ms 才知道结果没变。

egui pass **照跑**，不跳过。它是指纹的真值来源，
也是 tooltip / 菜单动画能继续推进的前提（动画在推进 → 顶点变了 → 指纹不同 → 照常出帧）。

**`text.trim()` 被跳过是安全的**，但要在代码里写明理由：`trim` 存在的意义是
让下一次 `prepare` 能淘汰旧字形；本帧既然不 `prepare`，也就不会有新字形进图集，
图集不会增长。（原注释强调 `trim` 必须在 `AtlasFull` 的提前 return 之前——
那是因为那条路径**已经 prepare 过了**。此处不同。）

### 6.3 各分量怎么算

**egui**：`Vec<ClippedPrimitive>`。逐项吃 `clip_rect` 的四个 `f32`
（`to_bits()`，不用 `==`）与 `Primitive::Mesh` 的 `texture_id` / `indices` / `vertices`。
`epaint::Vertex` 是 `#[repr(C)]` 的 POD（`pos`/`uv`/`color`）。

`Primitive::Callback` **一律判为「变了」**（保守方向：多画一帧，永不少画）。
我们目前不用 paint callback，但这条分支必须写，且要有测试——
将来有人加了 callback，静默失效是不可接受的。

**终端**：每个 pane 吃 `(PaneId, term_px, display_offset, 全部可见行的 row_hash,
光标行列/形状/是否可见)`。行指纹已由 F12 的六条逐字段测试守住，
且 `SnapCell.selected` 在内，选区反色自动覆盖。

**光标**：`blink_on`（F125 的相位）单独入指纹——它不在行指纹里
（光标由 `gpu::quads_for_panes` 画成独立色块）。

**IME preedit**：组字串、光标截段、落点 pane/行列，逐一入指纹。
**这一分量不能省**：preedit 画在终端文字层（F126，复用 `SnapCell::width`
的宽度判据），不在 egui 的 paint_jobs 里；而组字过程中 cells 不变、
行指纹不变——漏掉它，指纹在整个组字过程中恒命中，
**打拼音屏幕纹丝不动**，正是 T10 那一族「只有人眼能发现」的坑。
守护测试：仅改 preedit 内容，断言指纹变。

**几何**：`compute_geoms` 的产物。

### 6.4 穷尽解构护栏

与 F12 同款：指纹函数体内对每个输入结构做穷尽解构
（`let Foo { a, b, c } = *x;`），加字段即编译报错。
这是唯一能防住「新加了一个影响画面的字段却没进指纹」的机械手段——
症状是屏幕留着陈旧的一帧，编译测试日志全静默。

### 6.5 运行期守护

新增剖面段 `fp=hit:N/miss:M`（沿用 F12 `reshape=` 的形制与理由）。

**这是差分类优化唯一的运行期守护**：判据写错导致永远 miss 时，
画面完全正确、日志一切正常、性能悄悄回到改之前，没有任何人看得出来。
零值也显式打印（「这窗口一次没命中」和「这版本忘了统计」在日志里不能长得一样）。

同时把 `wake`（§4.1）提成一等指标。风险很实在：
**指纹会把 CPU 压下去，从而掩盖「唤醒 400/s」这个真根因**，让下一版失去动力。

---

## 7. 本版的预期（明确不到位的部分）

| | 本版后 | |
|---|---|---|
| launcher 态 present | 60 → ~0 /s | ✓ F158 |
| 终端态 present | 指纹拦掉大部分 | ✓ F159 |
| 终端态**唤醒** | 仍 ~400/s | ✗ 等 F157 的诊断结果 |
| 终端态 `egui_run` | 仍按唤醒率跑 | ✗ 同上 |

**CPU 会降但降不到 0.2%，这是设计上就知道的，不是失手。**
真正的开关在 `wants_repaint_after()` 的哪一条判据每帧成立，只能实机拿。
拿到日志后另开一版（F160）。

---

## 8. 被否的备选

- **A. 只摘 launcher 兜底，靠 40 个置脏点。** 否掉：把「列举式门控」变成唯一判据，
  且守不住「以后新加的那一档」。
- **B. 不摘兜底，只把 launcher 态限到 2fps。** 否掉：零风险但只治 launcher
  （用户真正在意的是终端态），且把一个错误判据固化成「特性」。
- **C. 用 `ctx.repaint_causes()` 定位。** 否掉：见 §1.3，egui 0.30 上对自动重绘无效。
- **D. 本版带一个 `MULLION_IGNORE_EGUI_REPAINT=1` 的实验开关**，实机一开一关
  当场读 CPU 差值。否掉（用户决定）：老实等诊断，不在没有证据时改 T3/T7 高压区的行为。
- **E. 改 `PresentMode` / `desired_maximum_frame_latency`。** 否掉：
  日志里 `acquire p95=16.4ms` 是完整的一个 vsync 间隔，按
  [wgpu 文档](https://docs.rs/wgpu/latest/wgpu/enum.PresentMode.html) `Fifo` 就该阻塞在这里。
  搜下来**没有** DX12/Windows 上 Fifo 忙等（自旋烧 CPU）的立案记录
  （[wgpu #1218](https://github.com/gfx-rs/wgpu/issues/1218) 那条是 Vulkan 的驱动怪癖）。
  **这是假设不是结论**——若 F157 的日志显示 `acquire` 才是大头，再回来重开。
- **F. wgpu 局部呈现 / 只重画光标那一格。** 否掉：wgpu 没有部分呈现的一等支持，
  工程量与本轮不匹配。这是「空闲 0 帧」的唯一通路，留作将来。

---

## 9. 人工验收清单（实机）

无头环境验不了的，全在这里。第 1、2、5 条最重要。

1. **launcher 态**：启动后不连任何会话，静置 1 分钟。
   `%APPDATA%\mullion\config\mullion.log` 里 `findstr profile`，
   `present=` 应当**接近 0**（改动前是 300/5s）。
2. **终端态**：4 分屏各挂 tmux + Claude Code 停在等待输入，静置 1 分钟。
   读 `wake=` 与 `present=`。**预期 `present` 明显下降、`wake` 基本不变**——
   `wake` 不降是本版已知的、写在 §7 里的。
3. **归因四段必须有非零内容**。`dirty=` 全空 = 埋点没接上（F12 同款陷阱：
   埋点白埋时画面完全正常）。把这一行原样贴回来，它决定下一版改什么。
4. **交互没丢**：launcher 里点会话、开弹窗、hover 高亮、键盘上下选择，全部要有反应。
   这是 F158 最可能坏的地方。
5. **后台事件能顶起一帧**：在 launcher 态发起一个连接，
   **不碰鼠标键盘**，看画面会不会自己更新到「已连接」。
   这条专验 §5.2 的风险；坏了的症状是画面卡在旧状态直到你动一下鼠标。
6. **切主题 / 换字体 / 换字号 / 拖到不同 DPI**：整屏立刻跟走，不能有陈旧帧。
7. **划选**：拖鼠标划选，选区实时反色，松开不残留。
8. **中文输入法**：组字时拼音串要**逐键跟着变**（这条专验 §6.3 的 preedit
   分量——漏了它的症状就是打拼音屏幕纹丝不动）；上屏正常；
   按 Esc 取消组字后被盖住的字要回来。
9. **拖动分屏分界线**：宽度变化后重排正确。
10. **`fp=hit:/miss:`**：静止时命中率应接近 100%；
    常年 miss 高 = 指纹判据写错，差分白做，但画面正常，只有这里看得出来。

---

## 10. 我不能验证的

- `acquire p95=16.4ms` 到底是真睡眠还是驱动层自旋（见 §8 的 E）。
- 那 5.6 次/帧被节流的唤醒里，超出 `about_to_wait` 推演的部分从哪来。
  F157 的 `wake`/`rr` 两段就是为这个准备的。
- 手上那份日志是 **v0.1.66（F12 之前）**。所有关于 present / 唤醒的结论都成立
  （F12 没碰帧闸，只改了单帧成本），但**新的绝对数没有**。
- 任何「是否不闪 / 手感 / 跟手」类指标。
