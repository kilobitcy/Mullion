# F192–F196 文字层 Buffer 记账与 run 合并 —— 设计

日期：2026-08-29
起因：v0.1.81 实机日志（PID 22508，14:42–15:20，97 条 `profile.mem`）

---

## 1. 实测数据

```
14:42:16  commit=297MB  ws=198MB  堆=57MB  scroll:0 xfer:0 text:2
14:44:21  commit=346MB  ws=244MB  堆=98MB  scroll:1 xfer:0 text:26
15:20:09  commit=346MB  ws=244MB  堆=98MB  scroll:1 xfer:0 text:26   ← 之后 36 分钟一动不动
负载：tabs=1 panes=3 hosts=2 scroll=436行 xfer=0
profile.gpu：vram=258/15621MB（AMD 780M，核显 UMA）
```

从这份数据出发有三条结论，**其中一条不成立**。三条都记在这里，因为下一次
从同一份日志立项的人会重新推一遍。

### 1.1 没有时间型泄漏 —— 成立，但样本有盲区

涨幅全部发生在开头 2 分钟，之后 36 分钟纹丝不动。v0.1.80 那份 52 分钟日志里
`其他:` 从 233 单调涨到 319MB 的「棘轮」，台阶落在 `WindowEvent::Resized` 上
（F190 已定位，F191 量化后否掉了埋点）。

**但这次的 36 分钟样本里一次 resize 都没有。** 所以准确的说法是：
**没有时间驱动的泄漏；resize 驱动的棘轮在这份样本里未被触发，仍未被证伪。**
本切片按「不是泄漏」立项，这条盲区是明知的取舍，不是遗漏。

### 1.2 「大头在 GPU 侧」—— 未证实（三处不成立）

原推理：堆外 = commit(346) − 堆(98) = 248MB，而 DXGI 报 258MB，两个独立口径吻合。

| # | 反驳 |
|---|---|
| R1 | **`ws=244 < vram=258`。** GPU 驱动为进程分配的内存是常驻锁页的。若这 258MB 全额计进本进程，光它一项就超过整个工作集了。这个不等式本身否掉「258 全在账内」 |
| R2 | **两把尺子量的不是一个东西。** `commit`（`PrivateUsage`）不含映射文件；而 `cosmic-text` 0.12.1 的 `std` feature 打开了 `fontdb/memmap`（`cosmic-text-0.12.1/Cargo.toml:136`），系统字体是 mmap 的——**在 ws 里、不在 commit 里**。拿 commit 减堆得到的「堆外」，与按 ws 口径谈的 vram 相减，数值相近是巧合 |
| R3 | **DXGI 是驱动口径。** `sysprobe.rs:873` 的注释自己写了「DXGI 在驱动层统计」。`DXGI_MEMORY_SEGMENT_GROUP_LOCAL` 在 UMA 上是 BIOS carveout + 动态共享的混合（budget 报 15621MB ⇒ 动态），与 Windows 的进程提交量不是同一个记账体系 |

**用户已拍板：不做前置验证，按性价比排序直接动手。** 这一节留作证据——
若 F193/F194 落地后体感无改善，回到这里，而不是重新从「两个口径对上了」推一遍。

唯一能钉死 GPU 侧的实验是零代码成本的：**实机把窗口从最大化拖到最小再拖回，
看 `vram=` 与 `ws=` 各自怎么动**。留给将来。

### 1.3 71MB 未归因在 Rust 堆里 —— 成立，且定量自洽

`堆=98MB`，已记账 `scroll:1 + text:26 = 27MB`，余 71MB。

反推 Buffer 数：`26MB / 4096 = 6656` 个记账单位。而可见行撑死一两百 ——
**说明 `pool` 里躺着约 6500 个空闲 `Buffer`**（`cache` 那百来行只按 1 行 1 个计）。
再加上 `cache` 里被漏记的 runs（下节），总 Buffer 数 2 万量级，按每个 2–4KB 估
正好落在 71MB 这个洞里。**这个假设定量自洽，1.2 那条不自洽。**

---

## 2. F192 —— 把尺子修对（计数与单价必须一起改）

### 2.1 病

```rust
// text.rs:387
(self.cache.len() + self.pool.len() + self.temp.len()) * BUFFER_EST_BYTES
```

`ShapedCache::len()` 返回的是 `rows.len()`（`shaped_cache.rs:186`），即**行数**；
而每个 `CachedRow` 装的是 `runs: Vec<CachedRun<T>>`（`shaped_cache.rs:61`），
**一行 N 个 `glyphon::Buffer`**。

N 有多大取决于 `row_to_runs` 的切法（`text.rs:80`）：`advance_is_cell_wide` 为真的
连续格并成一个 run，**其余每格自成一个 run**。而 `advance_is_cell_wide`
（`text.rs:48`）要求 `cell.width <= 1 && cell.ch.is_ascii() && !is_ascii_control()`
—— **每一个中文字、每一个框线符 `─│┌└`、每一个 emoji 都单独占一个 Buffer**。

在 tmux 里跑 Claude Code 的画面上，一行 120 列的框线 = 120 个 Buffer，记账只算 1。

### 2.2 只改计数会让日志出现物理上不可能的读数 —— 所以①拆不开

**（这一节初稿写错过一次，机制不是「触发兜底分支」，留档以免再错。）**

`mem_parts`（`profile.rs:801`）的兜底判据是 `accounted > primary_mb`，
而被减数 `primary_mb` 在 Windows 上是 **commit（346MB）**，不是 `堆=`。
把 `cache.len()` 换成 runs 求和后 `text:` 约 82MB，`accounted ≈ 83 < 346`
—— **那条分支根本不触发**。

真实症状更隐蔽：

```
commit=346MB(ws 244) 堆=98MB = scroll:1 xfer:0 text:82 其他:263
```

`text:82` 与 `堆=98` 几乎打平。三笔记账是堆的**子集**（都是 Rust 堆上的
`Vec`/`Buffer`），子集占到全集 84% 已经很可疑；`BUFFER_EST_BYTES` 只要再偏大
一点，`text:` 就会**反超 `堆=`——一个物理上不可能的读数**。而现有渲染没有任何
分支会指出这件事，读者只能靠自己起疑。

**这恰好说明 §2.4 那条新守护补的正是这个洞**：兜底分支守的是「记账 vs commit」，
而尺子坏掉先撞穿的是「记账 vs 堆」这条更紧的界。

根因是 `BUFFER_EST_BYTES = 4096`：它当初按「一行 ~200 格」拍的（`text.rs:214`
的注释原话），而修完计数之后被计数的单位从「一行」变成了「一个 run」，
而一个 run 大多数时候只装一个汉字。**同一个常数不可能同时对这两种东西成立。**

### 2.3 标定方法（不能照抄 F190 的手法）

`heapgauge::Counters` 被刻意做成可多实例（`heapgauge.rs:41` 写明理由是给测试用），
**但全局分配器只喂 `GLOBAL`** —— 要量「一个 Buffer 实际吃多少堆」，只能读
`GLOBAL.live()` 的差值，私有计数器挂不上分配器。

而 F190 自己的教训正是冲着这个来的（`spec.md:195`）：

> 1600+ 条测试并行跑，进程级全局计数上的绝对增量测不准，四条数值守护各自用
> `Box::leak` 的私有计数器断言**精确相等**而非容差。

**照抄 F190 的私有计数器手法在这里行不通；直接读 `GLOBAL` 又会被邻居测试的
分配噪声推着走。** 三条对策一起上：

1. **把信号做大到噪声之上**：一次整形 **10,000** 个单字 run 并**保持它们存活**
   到第二次读数之后（`Vec<Buffer>` 持有）。信号量级 20–40MB，而并行邻居在
   同一个测量窗口里的**净**漂移是 MB 级。
2. **多轮取中位数**（3 轮），削掉偶发的大块分配。
3. **只断言量级**：`[EST/4, EST×4]`。要打红一个 4 倍带宽的中位数断言，
   邻居得在窗口内净漂几十 MB —— 实际不会。

流程：建 `FontSystem`（纯 CPU，不需要 GPU；`cosmic_text::Buffer` 同）→
记 `live()` → 整形并持有 10,000 个单字 run（最坏形态，也是修完计数后的主要
计数单位）→ 记 `live()` → 相除。

平台漂是明知的：Linux 开发机的回退字体与 Windows 不同，实测值会差。
断言只钉量级正是为此——常数漂出一个量级时逼人回来重标，日常波动不红。

### 2.4 新守护：记账合计不得超过 `堆=`

```
scroll_b + xfer_b + text_b  ≤  heap_b
```

三笔记账全部在 Rust 堆上（`Vec` / `Buffer`），天然是 `堆=` 的子集。一旦超出，
唯一可能就是尺子坏了 —— **这正是 F192 要根治的病，而这条断言对将来再一次
改错同样有效**，不是一次性修补。

**落地形态：`mem_parts` 的第三条渲染分支**，与现有「记账超出commit」那条同族：

```
commit=346MB(ws 244) 堆=98MB = scroll:1 xfer:0 text:120 其他:225(!记账 121 > 堆 98)
```

**打标记而不是硬断言**（`assert!`/`debug_assert!` 都不行）：`堆=` 是分配器计数器的
**瞬时**读数，三桶是同一窗口另一时刻采的，差一帧就有几 MB 噪声。真实的坏尺子
会超出几十 MB，噪声只有几 MB —— 但把噪声写成 panic 是拿渲染路径赌采样时序。

纯渲染层（`mem_parts` 的入参上），不需要 GPU，可纯单测。

---

## 3. F193 —— quad 实例缓冲常驻（理由不是省内存）

### 3.1 它动不了那 250MB

`quads_for`（`gpu.rs:113-119`）只在两种情况产出 quad：

```rust
let color = if cell.selected { cell.fg }
            else if cell.bg == defaults.bg { continue; }   // ← 默认底色一个都不画
            else { cell.bg };
```

`QuadInstance` = `[f32;4] + [f32;4]` = **32 字节**。实际画面（tmux + Claude Code，
绝大多数格子是默认底）：

| 场景 | quad 数 | 缓冲大小 |
|---|---|---|
| 典型帧 | ~200 | **6.4 KB** |
| 病态满彩（3 pane × 50 行 × 200 列全非默认底） | 30,000 | **0.96 MB** |

**每帧 6KB 的 create/drop 撑不出 250MB 的 free-list**，相差四到五个数量级。
gpu_alloc 起手 chunk 是 8MB（`wgpu-hal-23.0.1/src/vulkan/adapter.rs:1937`），
这个 buffer 一辈子都在同一个 chunk 里被反复复用。

### 3.2 真正的理由是 T3

```rust
// gpu.rs:688
let data: Vec<QuadInstance> = quads.iter().map(...).collect();  // 帧路径上分配
self.device.create_buffer_init(...)                             // 每帧一次 map/unmap + staging copy
```

这是 **T3「帧路径上不分配」**的违规。成本在 CPU 侧的分配 churn 和每帧一次
map/unmap，不在显存。

### 3.3 改法与借用结构

- `Gpu` 上加常驻 `quad_buf: wgpu::Buffer`（`VERTEX | COPY_DST`）+ 常驻
  `quad_staging: Vec<QuadInstance>`
- `upload_quads(&mut self, quads: &[Quad]) -> u32`：`staging.clear()` → 填充 →
  容量不足则按 **2 倍**重建 buffer，否则 `queue.write_buffer`
- 调用点 `app.rs:11159` 的 `terminal_draw` 只留 `quads.len()`，画的时候引
  `a.gpu.quad_buf`（`draw_quads` 本来就是 `&'a self`）

借用已核过：`panes` 是函数入参、不从 `a` 借，所以 `&mut a.gpu` 不与
`a.text.prepare_panes(&a.gpu.device, …)` 打架。约 20–30 行。

### 3.4 验收判据必须换

**判据是帧路径分配量 / `堆=` 的空闲增长，不是 `vram=`。**

不换判据的后果是确定的：改完看 `vram=` 纹丝不动，然后得出「这一刀没用」的
错误结论，而它在自己该管的那一栏是有效的。**commit body 里明写「不预期
`vram=` 有变化」。**

---

## 4. F194 —— GPU 块大小做成旋钮，默认不动

### 4.1 `MemoryHints::Manual` 的基底是 `Performance`，不是 `MemoryUsage`

锁定版本 `wgpu 23.0.1`，`wgpu-hal-23.0.1/src/vulkan/adapter.rs:1948`：

```rust
wgt::MemoryHints::Manual { suballocated_device_memory_block_size } => gpu_alloc::Config {
    starting_free_list_chunk:     range.start,
    final_free_list_chunk:        range.end,
    initial_buddy_dedicated_size: range.start,
    ..perf_cfg                                  // ←←← 基底是 Performance
},
```

于是写 `Manual { 4MB..16MB }` 拿到的是：

| 参数 | 现在 `MemoryUsage` | `Manual{4..16MB}` | |
|---|---|---|---|
| `starting_free_list_chunk` | 8MB | **4MB** | ✅ |
| `final_free_list_chunk` | 64MB | **16MB** | ✅ |
| `dedicated_threshold` | 8MB | **32MB** | ❌ 静默放宽 4 倍 |
| `transient_dedicated_threshold` | 16MB | **128MB** | ❌ 静默放宽 8 倍 |

后两个是「多大的资源才配拥有独占内存块」。放宽之后，一个 20MB 的纹理不再拿
一块正好 20MB、释放即归还的独占块，而是去 free-list 里挤 —— 而 free-list 的块
上限被同时压到 16MB、根本装不下，净效果是白留一块在 free-list 里永不归还。

**`Manual{4..16MB}` 不是「MemoryUsage 再狠一点」，是「Performance 的阈值 +
更小的块」，方向未知。** wgpu 自己的注释还写着 "the parameters here are not set
in stone nor where they picked with strong confidence"（`adapter.rs:1911`）。

### 4.2 改法

默认值**保持 `MemoryUsage`**（零回归风险），加环境变量旋钮：

```
MULLION_GPU_BLOCK=4,16      # 单位 MB，start,end；解析失败/未设 → MemoryUsage
```

先例：`MULLION_LOG` / `MULLION_LOG_DEPS`（`logx.rs:240`）。日常使用时顺手带一个
环境变量就能扫出真值，不需要专门编排验证流程（见
`memory/field-capture-lessons.md`：先榨日志的自陈能力）。

选中的档位要**打进启动日志**，否则日后对比两份日志时分不出是哪一档。

### 4.3 守护测试怎么改

`gpu.rs:1647` 现在按整行字面量断言：

```rust
assert_eq!(hits, ["memory_hints: wgpu::MemoryHints::MemoryUsage,"]);
```

它守的不变量是 **「不许退回默认的 `Performance`」**（不写 = Performance =
Vulkan 起手 128MB chunk = 空载 289MB 的主犯）。这条不能丢。

改法：值提成具名常量，`DeviceDescriptor` 那行仍是单行、仍可整行匹配：

```rust
memory_hints: gpu_memory_hints(),     // ← 断言这一行必须是它
```

再补一条：`gpu_memory_hints()` 的默认分支必须是 `MemoryHints::MemoryUsage`
（改成 `Performance` 或 `default()` 当场红）。**两件事分开守**：「有没有显式写」
与「默认档是哪一档」。

---

## 5. F195 —— run 合并判据改成实测 advance

### 5.1 「同宽度 + 同回退字体」推不出「对得上格子」

`row_to_runs` 的设计前提写在 `text.rs:52-70`：cosmic-text 在一个 run 内部
**按 advance 累加**摆字（第 k 个字形的 x = 前 k 个 advance 之和），而底色 /
光标 / 选区由 `gpu::quads_for` 按 **col × cell_w** 精确画。两套定位只在
「每个字形的 advance 恰好等于 cell_w」时重合。

设某 CJK 字形在回退字体（微软雅黑）里的 advance 是 `a`，而 `cell_w` 是按
Google Sans Code 的 `'M'` 量的（`text.rs:measure_cell_w`）。合并 N 个中文之后：

```
drift(k) = k × (a − 2·cell_w)
```

每字差 0.2px，一行 60 个中文到行尾漂 12px ≈ 一整格。**症状就是用户当初实报的
「粘贴的内容和光标之间有空白」** —— run 被切碎正是为了它。

「advance 彼此一致」与「advance 等于 `width × cell_w`」是两回事，需要的是后者。
按「同宽度 + 同回退字体」合并，等于把那个 bug 按 O(N) 放大后请回来，
且只在中文长行才显形。

### 5.2 新判据：直接量那个性质本身

判据本身是：**「这个字符在当前字体链下量出来的 advance，是不是恰好
`width × cell_w`」**，取代「是不是 ASCII」（后者只是前者的一个保守代理）。

#### 谓词必须注入，不能把 `row_to_runs` 改成方法

初稿写成 `fn advance_is_cell_wide(&mut self, …)`（TextLayer 的方法，持有
`FontSystem` + memo）。**那会拆掉 `row_to_runs` 的可测性** —— 它现在是纯函数，
`text.rs` 里有 **12 处**直接构造 `SnapCell` 调它的单测，全都不碰 GPU、不碰
`FontSystem`。改成方法之后这批测试全要挂上一个 `FontSystem`。

正确形态是**谓词注入**：

```rust
pub fn row_to_runs(
    cells: &[SnapCell],
    hidden: Option<(u16, u16)>,
    advance_ok: &mut impl FnMut(&SnapCell) -> bool,   // ← 注入
) -> Vec<RowRun>
```

- **生产调用点**（`TextLayer::prepare_panes`）传一个闭包：查 memo，未命中就
  `measure_advance` 一次再写回。`FontSystem` 与 memo 都留在 TextLayer 里，
  `row_to_runs` 一如既往不认识它们。
- **单测**传常闭包（`&mut |c: &SnapCell| c.ch.is_ascii()` 之类），
  12 处现有用例只需加一个参数，零 GPU、零 FontSystem 的性质保住。
- 依赖方向不变：`row_to_runs` 仍然零 IO、零 async、可纯单测。

#### 谓词内部

- 复用 `measure_advance`（`text.rs:897`），`font_pick::is_monospace_advance` 已在用
- `HashMap<char, bool>` memo 化；**跟 `set_font` 一起清**（`text.rs:401` 那里是
  缓存唯一的显式失效 hook，不新增第二处）
- 浮点比较带 ε
- **`width >= 1` 是硬护栏**：组合附加符号 `unicode-width` 判为 0，会满足
  `advance == 0 × cell_w` 白送进合并

#### memo 按 `char` 单键成立 —— 但这依赖一个必须登记的前提

单键的前提是「advance 只由字符决定」。我 grep 了 `text.rs` 全部 `Attrs` 构造点
（`text.rs:510` / `897` / `1262` / `1859`）：**整形路径一律是
`Attrs::new().family(…)`，没有 `.weight()`、没有 `.style()`** —— bold 在本项目里
只走调色板加亮（F128 那条），不进 shaping。所以同一个 `char` 的 advance
与 SGR 无关，单键正确。

**但哪天有人给整形 attrs 加上真粗体/斜体，memo 就静默失效**（粗体字形的
advance 与常规不同，却共用同一个缓存项 → 错位回来，且没有任何东西会报错）。
这是 D1 那条「复用函数要连隐含前提一起复用」的同族。

守护：一条源码切片测试，断言整形路径的 `Attrs` 构造不含 `.weight(` / `.style(`
—— 要加就必须先把 memo 键从 `char` 扩成 `(char, weight, style)`。

这个判据**严格优于**现有启发式：ASCII 之所以能合并，正是因为它 advance 对得上，
不是因为它是 ASCII —— ASCII 成为新判据的真子集。

### 5.3 收益的诚实说明

| 字符类 | 预期 | 说明 |
|---|---|---|
| 框线符 `─│┌└` | **多半放行** | Google Sans Code 这类编程字体自带框线字形、advance 就是 cell_w。而框线是 Claude Code TUI 满屏的东西 —— **F195 的主要奖金，且是安全拿到的** |
| CJK | **多半拒绝** | 回退字体的 advance 通常不是 `2 × cell_w`。**这是正确行为，不是遗憾** |

**已与用户确认接受**：原分析承诺的「中文行 Buffer 数降一到两个数量级」多半拿不到。

### 5.4 守护

1. **无头漂移守护**（新增，本条是 F195 的核心）：拿真 `FontSystem`（不需要 GPU）
   把 `row_to_runs` 产出的每个 run 整形一遍，断言每个字形的 `x` 与
   `(col − run_col) × cell_w` 的偏差 < ε。
   **在 Linux 与 Windows 上输入不同**（回退字体不同 ⇒ 合并集合不同）
   **但不变量相同** —— 这正是能真守住 CJK 对齐的那种测试。
   自证会变红：把判据改回「同宽度就合并」。
2. **CJK + 框线混排的 VT fixture**（`docs/vt-fixtures.md` 的流程）。
   项目规定 VT 相关新功能必须配 fixture。
3. `advance_ok` memo 必须跟 `set_font` 清 —— 自证会变红：删掉那行，换字号后
   断言合并集合已刷新。

---

## 6. F196 —— `pool` 加 cap

`pool: Vec<Buffer>`（`text.rs:272`）无上限、从不收缩，只进不出地卡在历史峰值。
按 §1.3 反推，里面躺着约 **6500 个**空闲 Buffer，每个还攥着上一次整形留下的
`Vec` 容量。

- **cap 单位是 Buffer 数，不是行数**。用的就是 F192 为修尺子新加的那个方法
  （`ShapedCache::payload_count()`，runs 求和）—— **一处实现两处用**，
  免得日后一个改了另一个没改。原分析写「2 × cache 行数」，
  但 `pool` 装 Buffer、`cache` 数行 —— 那正是 F192 要修的单位错配，
  别在新代码里再犯一次。F192 之后 cache 的 Buffer 数是现成的。
- cap = `2 × cache 中的 Buffer 数`，下界给一个常数（避免启动瞬间 cache 为空时
  把池清空）。
- 执行点：`ShapedCache::end_frame` 回收完之后 `truncate` 一次。
- **安全性无虞**：`glyphon::Buffer` 就是 `cosmic_text::Buffer`，不持有 GPU 资源，
  drop 是纯 CPU free。
- 2 倍余量是为了不 thrash：稳态下每帧回池数 ≈ 每帧取用数（滚动时 = 滚动行数），
  2 倍留够了。

F195 之后 pool 的稳态大小会变，但 cap 是自适应的，不需要重标。

---

## 7. 明确不做的

### 7.1 `bands` 改「N 帧未见才回收」—— 拒（安全不变量）

原分析建议把 `slots.retain(|_, s| s.last_seen == frame)`（`text.rs:821`）
改成 300 帧未见才回收，以减少切标签时的 `TextRenderer` 重建。两条理由否掉：

1. **量级不成立。** `BAND_ROWS = 16`（`bands.rs:51`），3 pane × 约 50 行
   ⇒ 每 pane 约 4 带，全窗口约 **12 个** `TextRenderer`。谈不上「一批 churn」。
2. **那一行不只是回收策略，是安全不变量。** `text.rs:819` 与 `app.rs:11120`
   的注释是配套的：`trim` 清空 `glyphs_in_use`，**只有本帧真的 prepare 过的带**
   才会把自己用的字形标回去。当前每帧逐出保证了「没参加本帧 prepare 的带
   不可能活着」。

   改成 300 帧存活之后：切到别的标签 → 那些带休眠但活着 → 期间发生 `trim`
   → 它们的字形失去保护被淘汰 → 切回来时 `fp` 比对**相同**、判定干净、跳过
   prepare → **顶点指着旧图集坐标，屏幕上画出别的字**。
   正是那两处注释里明写的「不报错、不 panic、编译测试日志全静默」。

   要做也得付「唤醒即强制重建」（休眠带回来时 `fp = None`）的代价，
   那样省下的 churn 又还回去了，净收益归零。

3. 方向也相反：这是内存切片，⑤b 是「多留 300 帧 GPU 顶点缓冲换一点 churn」，
   是往回加内存。

**动作**：把「每帧逐出是安全不变量，不是可调的回收策略」写进 `text.rs:819`
上方注释，免得下次再提。

### 7.2 scrollback —— 不碰

实测 `scroll:1MB`，`HISTORY_BUDGET_BYTES = 32MB/pane`（`emulator.rs:145`），
差两个数量级。

补一条口径说明：`scroll_bytes` 只累加 `scrollback_bytes()`（`app.rs:8625`），
**不含可见网格**。但 3 pane × 50 行 × 200 列的可见网格也就 1MB 上下，
不影响结论。

### 7.3 `quads_for` 里的 `Vec::new()` —— 不动

改它要把纯函数改成吃 `&mut Vec`，`gpu.rs` 里一批直接构造 `PaneRender` 的现有
测试全部要跟着改。收益（每帧一次 Vec 分配）不值这个代价。
**Scope Discipline：F193 只动上传路径。**

### 7.4 N5 从 300 收到 150 —— 拒（改的是量法，不是阈值）

`budget_verdict`（`profile.rs:727`）的状态机：越界后要再涨
`MEM_REPORT_STEP_MB = 64` 才报第二声。所以 300→150 之后：

- 当前 ws=244 → 立刻 `Cross(244)`，报一行，然后**永久停在越界态**
- 下一次出声要等 ws ≥ 308
- **244 到 308 之间的任何回归从此报不出来** —— 而这正是最可能发生回归的区间

换来的不是更灵敏的告警，是**一个恒亮的灯 + 一个 64MB 宽的盲区**。

更要紧的是这条线**从没被按定义量过**。`profile.rs:685` 写的是
「N5 —— 常驻内存（**8 pane，10000 行回溯**）< 300MB」，而这份日志的载荷是
`panes=3 scroll=436行`。**3 个 pane、436 行回溯就 244MB。**

**结论反过来：N5=300 八成早就不达标，只是尺子从没伸到该量的地方。**

处置（**B + D**）：

- **B（本轮）**：数字不动，补一次**按定义载荷**（8 pane / 10000 行）的实机采样。
  不需要专门编排流程 —— 开 8 个 pane 正常用一会儿，日志自陈。
- **D（本轮之后）**：F192–F196 落地后拿新读数，把线定到「实测 + 余量」，
  棘轮式收紧。有了 8 pane 与 3 pane 两个点，将来若要做「预算随载荷缩放
  （`base + k × panes`）」，`k` 才算量出来的而不是猜的。

---

## 8. 依赖方向与架构约束

全部改动落在 `mullion-app`，不碰 core/term/ssh/store，依赖方向不变。

可纯单测（无 GPU）的部分：

| 改动 | 可单测的那一层 |
|---|---|
| F192 计数 + 标定 | `bytes_estimate` 是纯算术；标定用 `FontSystem`（纯 CPU） |
| F192 记账 ≤ 堆 | `profile.rs` 纯渲染层入参断言 |
| F193 | 借用/接线由编译器保证；判据量在 `heapgauge` 上 |
| F194 | 环境变量解析是纯函数；守护是源码切片 |
| F195 | `row_to_runs` 纯数据；漂移守护用 `FontSystem`（纯 CPU） |
| F196 | `ShapedCache::end_frame` + cap 是纯数据结构操作 |

**没有一条需要窗口。** 这正是 CLAUDE.md 里那条约束的收益。

---

## 9. 你无法验证的东西（人工验收清单）

1. **F195 的 CJK / 框线对齐**——漂移守护能钉住「合并的 run 落在格子上」，
   但「屏幕上看着对不对」只有人眼能判。**实机验收必看**：满屏框线的
   Claude Code TUI + 一段长中文（≥60 字）同屏，看字与底色块有没有渐进错开。
2. **F194 各档位的真实效果**——只能靠带 `MULLION_GPU_BLOCK` 跑一次再读
   `profile.gpu` 的 `vram=`。
3. **F193 是否真的降了帧路径分配**——判据在 `堆=` 的空闲增长曲线上，
   需要一段静置日志。
4. **N5 的 B**：8 pane / 10000 行载荷下采一次 `profile.mem`。
5. **体感**——本轮的起因是「指标达标但体感超标」，而体感只有人能判。

---

## 10. 风险

| # | 风险 | 处置 |
|---|---|---|
| K1 | F195 判错导致 CJK 错位复发 | 漂移守护 + VT fixture + 人工验收 §9.1。判据比原启发式**严格更保守的方向**（实测 advance 是充分条件），出错方向是「该合并的没合并」（只损失收益不损失正确性） |
| K2 | F192 标定值在 Windows 上与 Linux 差一个量级 | 断言只钉量级；真实 Windows 值靠实机日志的 `text:` 与 `堆=` 的比值反推校正 |
| K3 | F194 旋钮被用了一次就忘了它开着 | 选中档位打进启动日志 |
| K4 | F196 cap 太紧导致每帧 alloc/free thrash | 2 倍余量 + 常数下界；判据在 `堆=` 的分配增长上（与 F193 同一条曲线） |
| K5 | 本轮全部落地后体感仍无改善 | 那说明 §1.2 的 GPU 侧归因是对的而我们没动它 —— 回到 §1.2 末尾那个零成本的 resize 实验 |
