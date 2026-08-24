# F12 差分整形：按行指纹跳过未变行的文本整形

- 日期: 2026-08-24
- 状态: 设计已定稿，待实现
- 关联: `spec.md` F12(P0) / N1 / N2、`docs/adr-001-glyph-rendering.md`、T3

## 1. 问题

`TextLayer::prepare_panes`（`crates/mullion-app/src/text.rs:337`）每一帧对**当前
workspace 里所有 pane 的每一行**无条件重新 `set_rich_text` + `shape_until_scroll`，
不管这一行这一帧变没变、也不管这个 pane 变没变。

单个 pane 收到字节时确实只有它自己被标脏（`shell/workspace/mod.rs:408` →
`render.rs:44` `SyncFramePacer::feed`），但"这一帧要不要出帧"那一步把所有 pane 的脏
标记取了 **OR**（`render.rs:169-182` `panes_ready_to_present`，注释里写明跨 pane 语义
必须是 OR，否则活跃 pane 的新内容会被静止 pane 拖住永远出不来）。这个 OR 是对的，
但"是哪个 pane 脏的"这个信息到此就丢了：一旦决定出帧，`app.rs:7819-7845` 取的是
`geoms.iter()`（`compute_geoms` 遍历整棵分屏树）里**全部** pane 的快照，
`prepare_panes` 再对它们逐行整形，present 后 `app.rs:8075-8081` 又对所有 pane
统一 `mark_presented`。

结果：**任意一个 pane 的光标闪一下、tmux 状态栏刷新一次，就会拖着全窗口所有
pane（包括完全静止的那些）陪跑一次全量文本整形**，单帧成本与 pane 总数成正比。

### 证据

1. **作者自己的量化**（`text.rs:1196-1250`，2026-08-21 加的 `shaping_cost_per_frame`
   脚手架）："`prepare_panes` 每帧对每个 run 无条件 `set_rich_text` +
   `shape_until_scroll`，屏幕内容一帧没变也照做一遍"，实测 **8 pane 分屏
   ≈ 18~23ms/帧，单这一步就超预算——N2 的头号嫌疑人**。

2. **真实场景对照实验**（2026-08-24，Windows 11 实机，AMD 780M，16 核）。
   逐步加 pane，全部处于空闲态（Claude Code 停在等待输入，没有任何对话在执行）：

   | 场景 | mullion 进程 CPU |
   |---|---|
   | 刚启动，无连接 | 1.8% |
   | 连 1 个节点 | 3.5% |
   | 该节点 `tmux attach` 后 | 5.1% |
   | 2 分屏 / 2 节点 / 2× tmux+Claude Code | 5.9% |
   | 4 分屏 / 4 节点 / **不接 tmux** | 6.2% |

   关键对比在最后一行：多加 2 个**不接 tmux** 的 pane 只多 0.3%，而前面每加一个
   tmux+Claude Code 的 pane 要跳 0.6~1.6 个点。差别不在内容量（都空闲），
   而在 tmux/Claude Code 空闲时仍会周期性吐极小的字节（状态栏时钟、光标相关序列），
   每一次都触发一轮**全窗口**整形。

3. **横向基线**：同一台机器、同样的节点、同样的 tmux + Claude Code，
   **xshell 常驻 CPU 0.2%、内存 11MB**；mullion 5~6%、300+MB。
   内存差距主要是架构性的（wgpu/Vulkan swapchain + glyph atlas，GPU 加速是本项目
   的设计目标，不在本次范围），**但 CPU 差 25~30 倍解释不到 GPU 税头上**——
   一个实现良好的 GPU 终端在无可见变化时应当收敛到接近 0。

4. **ADR-001 早就写好了触发条件**：v0.1 选 glyphon 时明确写"N2/N3 达标主要靠
   T2 攒帧 + T3 帧率封顶 + **F12 damage 差分渲染**这套『少画帧』策略"，并留下
   "当 N2/N3 埋点数据显示瓶颈落在文本布局/整形时"重新考虑的触发条件。
   上面 1~3 就是那份数据。而 `spec.md:90` 把 F12 列为 **P0**（目标 v0.3，
   `spec.md:420`），全仓库 `grep -rn "\.damage("` **零命中**——至今未实现。

## 2. 目标与非目标

**目标**：一行内容这一帧没变，就不重新整形它。空闲的 tmux/Claude Code pane 在
光标闪烁/状态栏刷新时不再拖着全窗口陪跑。

**非目标（本次明确不做）**：

- **不做 GPU 层的局部重绘**。每帧仍然把已整形好的 buffer 全量交给 glyphon
  `prepare`/`render`。真正的"只提交脏区域"是更大的改动，不在 F12 范围内，
  也不是当前有数据支撑的瓶颈。
- **不做快照构建的差分**。`Emulator::snapshot()` 仍然每帧重建整份 `cols×rows`
  网格。若日后剖面显示它成为新瓶颈，再单独立项（见 §8 后续）。
- 不换渲染器。ADR-001 里"换等宽网格专用 wgpu pipeline"那条路依然不走——
  本次证明瓶颈在"整形了多少次"，而不是"每次整形多快"。

## 3. 决策：行指纹，而不是 `Term::damage()`

alacritty_terminal 0.26.0 确有现成的 `Term::damage() -> TermDamage`
（`Full` / `Partial(iter of LineDamageBounds{line,left,right})`）+ `reset_damage()`，
`spec.md` 的 F12 原文也点名了它。**本设计不用它。**

理由：`Term::damage()` 只知道 alacritty 自己改过的格子。能改变"一行最终长什么样"
的来源至少有七个：

1. `Term` 内容变化（damage 知道）
2. 选区反色——`text.rs:25` `row_to_spans` 里选中格把喂给整形的颜色从 `fg` 换成 `bg`
   （alacritty 文档 `term/mod.rs:450-452` **明说** selection 不在 damage 里）
3. IME preedit（`hidden_span_for_row`，正文要为拼音串让路）
4. **主题换色**——`Emulator::set_default_colors` 改的是快照里 `palette::resolve`
   解析出的 fg/bg，alacritty 完全不知道
5. 字体族 / 字号变化（`TextLayer::set_font` 改 metrics）
6. DPI 缩放变化（同上）
7. pane 像素宽度变化（喂给 `Buffer::set_size` 的 `avail` 依赖 `geom.term_px.w`）

以 damage 为基础就必须**逐个枚举**这些来源去并集。漏掉任何一个，症状是**屏幕上
留着一行陈旧的字**——编译不报错、测试不报错、日志不报错，只有人眼能发现，正落在
`CLAUDE.md` §「你无法验证的东西」那一类。而"列举式门控在加档时必然漏"在本项目
已经踩中过三次。

**行指纹**把判据从"列举所有会变的原因"翻转成"直接看结果变没变"：在
`Emulator::snapshot()` 已有的逐格循环里顺手算一个每行的 `u64`，覆盖渲染真正读到的
全部字段。上面 1/2/4 全部已经烘进快照字段，自动覆盖；5/6 由一次整体清空覆盖；
7 由缓存条目自带的宽度比对覆盖；3 单独处理（只有一行）。

失败方向也反过来了：指纹方案的最坏情况是**多整形一次**（画面永远正确），
damage 方案的最坏情况是**少画**（静默陈旧）。

代价：每帧多算约 0.1~0.3ms 的哈希（对照要省掉的 18~23ms），且需要修订
`spec.md:90` F12 的措辞。这笔账划算。

**被否的备选**：
- **A. damage 驱动**（spec 字面）——风险如上。
- **C. 两者都做**（damage 决定重建哪些快照行 + 指纹决定重整形）——收益叠加（还能
  省掉每帧那份 `cols×rows` 的 Vec 分配），但复杂度翻倍，且 A 的风险原样保留。
  留作后续，见 §8。

## 4. 架构

依赖方向不变（`app → term`），改动落在两个 crate。

### 4.1 `mullion-term`：`GridSnapshot.row_hash`

`GridSnapshot`（`snapshot.rs:68`）增加 `row_hash: Vec<u64>`，长度 = `rows`，在
`Emulator::snapshot()`（`emulator.rs:311`）**已有的那趟逐格循环里**算出来，不新开
一趟遍历。

- 算法：手写 **FNV-1a**。零依赖、进程内与跨版本都确定、可直接单测。
  （`std::collections::hash_map::DefaultHasher` 不保证跨版本稳定，`RandomState`
  带随机种子，都不合适。）
- **哈希输入必须恰好覆盖 `row_to_runs` / `row_to_spans` 真正读到的六个字段**：
  `ch`、`fg`、`bg`、`width`、`spacer`、`selected`。这六个恰好是 `SnapCell` 的
  **全部**字段（`snapshot.rs:19`）——哈希覆盖整个结构体。SGR bold 不必单列：
  `snapshot()` 已经用 `palette::bold_brighten` 把它烘进 `fg`。

  > **这是整个改动最关键的不变量。** 少哈希一个字段，那一类变化就静默不重画。
  > 运行时守护见 §6；此外哈希函数体内**穷尽解构**
  > `let SnapCell { ch, fg, bg, width, spacer, selected } = *cell;`——日后给
  > `SnapCell` 加字段（如 underline）会在这里编译报错，强制作者对"进不进哈希"
  > 表态，而不是静默漏掉。逐字段测试只护得住存量字段，护增量的是这条编译期守护。

- 行号**不进**哈希：缓存按 `(PaneId, row)` 分槽，各行只跟自己的上一帧比。两行内容
  相同则哈希相同，这是正确且无害的。

### 4.2 `mullion-app`：`ShapedCache`

`TextLayer`（`text.rs:214`）里那个按帧序号平铺、每帧从头填的
`buffers: Vec<Buffer>`（`text.rs:220`）换成跨帧缓存：

```
ShapedCache<T> {                     // 对载荷泛型，见 §6 的理由
    rows: HashMap<(PaneId, u16), CachedRow<T>>,
    frame: u64,                      // 帧序号，逐出用
}
CachedRow<T> { hash: u64, term_w: u32, last_seen: u64, runs: Vec<CachedRun<T>> }
CachedRun<T> { col: u16, payload: T }   // 生产: T = glyphon::Buffer
```

判据抽成一个纯函数，返回三态枚举而不是 `bool`——`Temporary` 与 `Reshape` 的
"整形"动作相同但缓存副作用相反（一个写、一个绝不写），用 `bool` 表达不了，
用枚举则调用方必须穷尽 `match`：

```
enum RowPlan { Reuse, Reshape, Temporary }
fn plan_row<T>(cached: Option<&CachedRow<T>>, hash: u64, term_w: u32,
               is_preedit_row: bool) -> RowPlan
```

每帧对每个 `(pane, row)`：

| 判据 | 行为 | 覆盖的失效源 |
|---|---|---|
| 无缓存条目 | 整形并写入 | 首帧、新建 pane、行数变多 |
| `hash` 不同 | 重新整形并写回 | 内容、SGR、选区反色、主题换色 |
| `term_w` 不同 | 重新整形并写回 | pane 像素宽变化（`avail`） |
| 是光标行且 preedit 非空 | 临时整形，**不查也不写缓存** | IME 组字（只一行，不做精细判定） |
| 以上都不成立 | **复用缓存，跳过整形** | —— |

**preedit 行绝不进缓存。** 组字中的正文行带着让路空洞（`hidden_span_for_row`，
它还额外要求 `cursor.visible`——表里的判据是它的超集，只会多整形不会漏），拼音串
overlay 同样是临时内容。若把这份结果写回缓存：用户按 Esc 取消组字后 cells 未变
→ hash 相同 → 会**复用带空洞的缓存**，被拼音盖住的那几个字永久消失——正是本设计
要根除的"静默陈旧"。所以该行走临时槽：不查缓存、不写缓存；其旧条目因本帧未被
访问而在帧末被逐出，组字结束后的第一帧按"无缓存条目"miss 一次即恢复。

**零 run 的行也要写条目**（`runs` 为空）。`row_to_runs` 会把全空白未选中的 run
整个丢掉，空行的整形产物就是空集——但空行恰是空闲画面的大头，不写条目的话这类行
永远落在"无缓存条目"分支，miss 率居高不下，正是 §7 要防的那种静默退化。

**逐出**：每帧开头 `frame += 1`，访问过的条目记下 `last_seen = frame`，帧末一次
`retain(|_, r| r.last_seen == frame)`。用帧序号而不是每帧新建一个 `HashSet` 记
访问集，是为了不在帧路径上分配（T3）。这一条统一覆盖 pane 关闭、行数缩小、
切标签，**不需要在 `close_pane` 等处各加一处清理 hook**——那正是列举式门控会漏
的地方。

**`Buffer` 必须回收，不能随缓存条目一起丢。** 现状那个 `Vec<Buffer>` 池子存在的
理由（`text.rs:352-354`：满屏 CJK 时 run 数是行数的几十倍，每帧重新分配近千个
`Buffer` 就是 T3）在改动后一字不变地成立——而且**更危险**：滚动的日志每帧每行都
变，若重整形时直接丢弃旧条目、新建 `Buffer`，流式场景（正是 N2 要保的那一档）
会从"每帧复用池子"退化成"每帧分配上千个 `Buffer`"，**比改之前更慢**。
因此逐出与重整形都把旧载荷推回一个 `pool: Vec<T>`，整形时优先 `pool.pop()`。

**唯一的显式 hook**：`TextLayer::set_font()` 里 `cache.clear()`。换字体族/字号/DPI
会让所有已 shape 的 buffer 的 metrics 整体作废，且这是单一入口。
（`pool` 不必清：整形路径每次都调 `set_metrics`，池里的 buffer 不带陈旧 metrics。）

### 4.3 缓存键必须是稳定身份，不是当帧下标

现在 `prepare_panes` 用的 `pi` 是 `panes.iter().enumerate()` 的**当帧下标**。关掉
中间一块 pane 会让其后所有 pane 的下标挪位——拿下标当缓存键会**张冠李戴**地把
A pane 的缓存当成 B pane 用。

**不需要改任何结构体**：`PaneRender.geom` 是按值持有的 `PaneGeom`
（`shell/workspace/geom.rs:75`），它本来就带 `pub id: PaneId`，且
`PaneId`（`mullion-core/src/layout.rs:9`）已 `derive(Hash, Eq)`，可直接做
`HashMap` 键。`prepare_panes` 里写 `p.geom.id` 即可。

> 定稿时本节曾要求给 `PaneRender` 加一个 `id` 字段，写实现计划时核对源码发现
> 稳定 id 早已在手；照原样加会得到一个与 `geom.id` 必须永远相等的冗余字段，
> 那本身就是一个新的失同步面。已删。

## 5. 数据流（每帧）

1. `Workspace::pump` 喂字节 —— **不变**。
2. 出帧判定 —— **不变**。`panes_ready_to_present` 的 OR 语义保持原样：
   "这一帧要不要出"和"这一帧重画哪些行"是两件独立的事，前者的 OR 是对的。
3. present 分支取 `emulator.snapshot()` —— 仍是整份快照，现在多带 `row_hash`。
4. `prepare_panes` 第一遍：逐 `(pane, row)` 查缓存，**miss 才**
   `set_rich_text` + `shape_until_scroll`；第二遍建 `TextArea` 时从缓存借
   `Buffer`（顺序仍按 pane × row × run 线性铺，与现状一致）。
5. `renderer.prepare` / `render` / `trim` —— **不变**。

借用：`cache` 与 `font_system` / `atlas` 是 `TextLayer` 的不同字段，沿用现有的
字段级借用分割手法（`text.rs:361-364` 已有同款注释解释这一招）。

**光标闪烁不参与判脏。** 光标由 `gpu::quads_for_panes`（`gpu.rs:210`）画成独立的
quad，与喂给 cosmic-text 整形的字符颜色没有任何交叉；闪不闪，字本身不变。
因此 F12 落地后，"聚焦但完全空闲、只有光标在跳"这一档将**完全不跑整形**，
只剩很便宜的 quad 几何计算——这正是本次实测场景里浪费的主体。

## 6. 测试策略

### `mullion-term`（纯单测，无 GPU）

- **逐字段守护（六条）**：`ch` / `fg` / `bg` / `width` / `spacer` / `selected`
  各自单独变化，都必须让该行 `row_hash` 变。这是防"漏哈希一个字段"的唯一机械
  守护，一条都不能省。
- **F12 验收标准**：只改一行 → 只有那一行的 `row_hash` 变（对应 `spec.md` 那句
  "只改一行后，脏行集合只含那一行"）。
- 回溯滚动（`display_offset` 变化）后各行 `row_hash` 跟着**内容**走，
  与 `snapshot()` 里既有的行号换算同源（F17 陷阱）。

### `mullion-app`（纯单测，无 GPU）

- 判据纯函数 `plan_row`（§4.2）的四条分支各一条测试。
- 逐出：本帧未访问的键被删。
- 回收：被逐出的、以及重整形前的旧载荷都进了 `pool`（数一数 `pool.len()`）。
  这一条挡的是 §4.2 那个"流式场景比改之前更慢"的退化。
- 组字取消后一帧：preedit 行的旧条目已被逐出，按 miss 重整形；断言缓存里
  **从未**出现过组字期间的写入（`T = ()` 模拟两帧，验 §4.2"不查也不写"）。
- 零 run 的行（全空白）也产生缓存条目，第二帧对同内容命中而非 miss。
- **`ShapedCache` 对载荷泛型的理由**：`glyphon::Buffer` 必须有 `FontSystem` 才能
  构造，不泛型就没法在无 GPU 的单测里建缓存。测试用 `T = ()`，生产用
  `T = Buffer`。这不是过度抽象，是可测性的前提。
- 缓存键是 `PaneId` 而非下标：构造"关掉中间一块 pane"的场景，断言剩余 pane 拿到
  的仍是自己的缓存。

### 人工验收（无法自动验证，进 PR 清单）

- 满屏 CJK 下的 idle CPU（任务管理器读数）
- 切主题后整屏颜色是否**立刻**跟走（验 §3 第 4 条失效源真的被指纹覆盖）
- 选区拖动时的实时反色
- IME 组字观感
- 换字体族/字号/改 DPI 后整屏是否重排
- 4 分屏挂 tmux + Claude Code 静置时的 CPU，对照 xshell 0.2% 基线

## 7. 度量

- 复用 F155 剖面：`text_prepare` 的 p95 应显著下降。
- **新增 `reshape=hit:{}/miss:{}` 计数进 profile 行。** 没有它的话，"缓存永远
  miss"这种退化会**静默发生**——画面完全正确，性能悄悄回到改之前，没人看得出来。
  用 ASCII 而不是中文，与同一行里的 `redraw=term:/ui:/both:`、`conn=ok:/err:/re:`
  同构，且天然绕开 T9（字形白名单）。
- 复用 `text.rs:1196-1250` 已有的量化脚手架跑前后对比。
- N1（4 pane 静置 < 1% 单核）按本次的截图方法人工复测。

## 8. 风险

- **哈希碰撞**：u64 FNV 碰撞概率可忽略；即便发生，症状是某一行少画一次，且**能
  自愈**（该行下次再变就会更新）。不做二次校验。
- **缓存内存**：条目数 = 可见 pane 数 × 行数，且每帧逐出未访问键，不会随时间增长。
  回溯缓冲区的行**不进缓存**（只有可视区的行会被访问）。
- **静默退化成现状**：若判据写错导致永远 miss，画面正确但性能无改善。由 §7 的
  `reshape` 计数守护。
- **后续可选项**（不在本次范围）：若剖面显示 `snapshot()` 每帧重建整份
  `cols×rows` 成为新瓶颈，再引入 `Term::damage()` 只重建脏行——那时指纹仍是最终
  判据，damage 只作为"少建几行快照"的优化，不改变失效判定的构造式性质。

## 9. 连带要改的文档

- **`spec.md:90` F12**：措辞从"用 `Term::damage()` 只重画脏行"改为"按行指纹判脏"；
  验收标准"只改一行后，脏行集合只含那一行"**保持不变**。
- **新增 `docs/adr-011-row-fingerprint-vs-term-damage.md`**：记录"手上有现成的
  `Term::damage()` 为什么不用"——枚举式失效源 vs 构造式覆盖，两者失败方向一个是
  静默陈旧、一个是白花一次整形。这是半年后一定会被重新问起的决策，理由比结论值钱。
- **`docs/gui-render-gotchas.md`** 补一条：`row_hash` 覆盖的字段必须与整形真正读到
  的字段同源；机械守护两层——存量字段靠六条逐字段测试，增量字段靠哈希函数里的
  穷尽解构（`SnapCell` 加字段即编译报错，见 §4.1）。
