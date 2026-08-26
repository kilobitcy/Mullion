# 资源用量归因（F167~F170）设计

> 状态：已与用户确认，待实现计划
> 前置：F155（日志三档 + 性能剖面）、F157（帧循环归因）、F164~F166（CPU/GPU 三口径 + 一实例一日志）

## 0. 要解决的问题

v0.1.71 的 profile 行已经记了 `cpu=` / `mem=` / `gpu=` / `vram=` / `gpu_us=`，
但这五个口径是**纯总量、零归因**。看到 `cpu=80%/主线程:15%` 时，剩下 65% 完全是黑的；
看到 `mem=340MB` 时不知道是 scrollback 还是传输缓冲。

而"用量是否合理"这个判断，**必须**有三样东西才成立：

1. **分子的构成** —— 340MB 是由哪几块组成的
2. **分母** —— 当时开了几个 pane、回溯多少行、传输队列多深
3. **场景** —— 这 5 秒里程序在干什么

现有日志三样都缺。本切片补齐。

### 已经有的归因（不重复造）

| 段 | 回答的问题 | 来源 |
|---|---|---|
| `pump=Nx/p95=…` 等 12 档阶段计时 | 主线程时间花在哪个阶段 | F155 |
| `dirty=行号:次数` | 谁在置脏触发重绘 | F157 |
| `wake=` / `rr=sched:N,evt:N` / `rdelay=` | 为什么醒来、为什么重绘 | F157 |
| `reshape=hit/miss`、`fp=hit/miss` | 两级缓存命中率 | F12 / F159 |

本切片补的是**资源侧**的归因，与上面的**帧循环侧**归因互补，不替代。

## 1. 用户决策（已锁定，实现时不得自行更改）

| 决策点 | 选定 | 理由 |
|---|---|---|
| 用途 | 全部四项：排障指到功能 / 优化决策 / 回归比对 / 用量分母 | 用户全选 |
| 成本边界 | **分两层**：Info 常开便宜的，Debug 开贵的 | 主场景是长跑挂机，Info 档写盘要克制 |
| 内存归因手段 | **手工记账已知大块 + 显式余量**，不做自定义分配器 | 分配器的标签传播在 async 里做不对，错数据比没数据坏 |
| GPU 分层手段 | **申请 `TIMESTAMP_QUERY_INSIDE_PASSES`，pass 内插点** | 不动单 pass 结构 |
| 输出格式 | **拆成多行，按主题分组** | 单行已 500+ 字符，人眼扫不动 |

## 2. 输出结构（F167 的一部分）

同一个 `Snapshot` 渲染成**一组行**，共享同一时刻——时间对齐天然不会错：

```
profile      5.0s frame=120x/p50=1.2ms/p95=4.5ms present=118 skip=2 fp=hit:80/miss:40 …
profile.load scene=sftp-transfer tabs=2 panes=5 hosts=3 scroll=12.4k行 xfer=2个/48MB剩 key=0x in=1.2MB/s
profile.cpu  total=68% main=14% | tokio:31% watchdog:0% 其他:9%
profile.mem  340MB = scroll:128 xfer:24 text:16 其他:172
profile.gpu  util=3D:22%/Copy:3% vram=210/8192MB frame=42x/p50=1.2ms/p95=3.4ms | term:0.8ms egui:0.4ms
```

（`profile.cpu` 的示例组名以 §4 的分组表为准——不存在 `render` 组，
渲染跑在主线程上，归 `main`。`frame=` 保留 F165 已有的 p50/p95 两个分位，
迁移不降精度。）

**规则**：

- `grep "profile\."` 捞全组，`grep "profile.mem"` 只看内存
- **每行是一条独立的日志记录**：各自带 `[时间] [pid]` 前缀（F166 的"每行带 pid"
  不许破坏），**禁止**一条记录嵌 `\n`——嵌了的话，续行没有时间戳和 pid，
  grep 出来对不上号。`render_line` 现有 doc 注释里"单行是硬要求"要连着改写成
  "每条记录单行"，理由不变
- **空闲门不变**：`is_idle()` 为真时整组一行都不写（F157/F164 的判据原样沿用，
  含 CPU 超阈值破空闲门）
- 概览行 `profile` 保留现有全部段，只把 `mem=/cpu=/gpu=/vram=/gpu_us=` 五段**移出**到各自的行
- `profile.load` 的 `scroll=`（回溯总行数）**不是**计数器：它来自 F169 内存记账
  同一次遍历的 `history_lines` 汇总（gauge），与场景判据用的 `scroll_events`
  （事件计数）是两个东西，别混
- 接口：`render_line(&Snapshot) -> Option<String>` 改为 `render_lines(&Snapshot) -> Vec<String>`，
  空闲返回空 `Vec`

**为什么不是"概览行 + Debug 才展开"**：`profile.mem` 那行本身很短，长的是概览行。
把便宜的分块信息藏进 Debug 档，等于偶发问题现场拿不到数据（开档时已经晚了）。

## 3. F167 场景标签 —— 直接回答"为什么在用"

纯函数 `scene_of(&Snapshot) -> Scene`，按优先级取**单个**主场景，命中即停：

| 优先级 | Scene | 判据 | 数据来源 |
|---|---|---|---|
| 1 | `sftp-transfer` | 传输队列非空 | **新增** `xfer_jobs` |
| 2 | `scrollback` | 本窗口有滚动事件 | **新增** `scroll_events` |
| 3 | `resize` | 本窗口有窗口 resize | 已有：`total(&stage_us[Stage::Resize]) > 0` |
| 4 | `typing` | `keys > 0` | 已有 |
| 5 | `remote-output` | 入站速率 ≥ `REMOTE_OUTPUT_BPS` | 已有：`inbound_bytes / 窗口秒数` |
| 6 | `connecting` | `connects_ok + connects_err + reconnects > 0` | 已有 |
| 7 | `ui-only` | `frames > 0`，其余皆零（动画/hover） | 已有 |
| 8 | `idle` | 都为零 | — |

**场景名是 `resize` 不是 `reshape`**（复核时改的）：`reshape` 在概览行里已经被
F12 的整形缓存 `reshape=hit/miss` 占用，同组日志一词两义，grep 会把两者混出来。
且已核实 `Stage::Resize` 只在窗口 resize 处打点（`app.rs` 唯一调用点），
语义就是窗口 resize——分屏拖分界线**不**触发这一档，它走 `ui-only`。

**需要两个新计数器**（`scroll_events`、`xfer_jobs` + `xfer_bytes_left`）。
这两个本来就得加——`profile.load` 那行的分母 `xfer=2个/48MB剩` 就是它们。
其余六档全部从已有字段派生。

`REMOTE_OUTPUT_BPS = 1024`（1 KB/s）。**为什么这个数**：比它高优先级的
`typing`/`scrollback`/`sftp-transfer` 已经把用户主动操作那几类领走了，
这一档只需要把"远端在主动推输出"和"提示符心跳级的涓流"（OSC 7 每个提示符发一次，
量级几十字节）分开。1 KB/s 在两者之间有两个数量级的余量。定为具名常量，调起来方便。

**为什么是单值而非多标签**：多标签下每行都是 `sftp,typing,remote-output`，等于没分类。

**并发信息不丢**：`profile.load` 那行把判定依据的原始数据（`xfer=` / `key=` / `in=` /
`tabs=` / `panes=`）全部打出来。scene 只是**快速索引**，原始数据才是真相。
读日志的人对 scene 有疑问时，同一行就能自己复核。

`idle` 这一档在实际日志里**永远不会出现**（空闲门会先把整组拦掉），
但它必须存在于枚举里——`scene_of` 是纯函数，要能对任意 `Snapshot` 有定义，
且单测要能覆盖"全零"这个输入。

## 4. F168 CPU 按线程分组（Info 常开）

### 采样点与成本

在**看门狗线程**上 5 秒采一次，与 F164 的进程/主线程 CPU 同一次唤醒。**不碰热路径**（T3）。

- **Windows**：`CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD)` 枚举 →
  筛 `th32OwnerProcessID == GetCurrentProcessId()` →
  `OpenThread(THREAD_QUERY_LIMITED_INFORMATION)` → `GetThreadTimes` + `GetThreadDescription` 取名
- **Linux**：遍历 `/proc/self/task/*/`，读 `stat`（utime/stime，字段 14/15）+ `comm` 取名

约 20 个线程 × 2 次调用 / 5 秒 ≈ 8 次调用/秒，可忽略。

CPU% 的算法**复用 F164 已有的 `cpu_pct(delta_ns, window_ns, cores)`**，
线程组一律走**不归一**口径（与 `main` 一致：100% = 烧满一个核），
因为"这一组烧了几个核"比"占整机百分之几"更有用。

### 分组

"线程名前缀 → 组名"的映射表。未匹配的进 `其他`。

| 前缀 | 组名 |
|---|---|
| 主线程（tid == 主线程 tid） | `main`（与 F164 的主线程口径同源，不重复采） |
| `tokio-runtime-worker` | `tokio` |
| `mullion-watchdog` | `watchdog` |
| `mullion-file-dialog` | `dialog` |
| `mullion-dragout`（F59 的 STA 线程） | `dragout` |
| 空名（没设过名的线程） | `其他`，Debug 档 unmapped 清单里以 `unnamed` 显示 |
| 其余 | `其他` |

空名那行不是假设：Windows 上 `GetThreadDescription` 对没调过
`SetThreadDescription` 的线程（驱动、DXGI、PDH 内部线程）返回**空串**；
不定义去向的话，Debug 档 unmapped 清单里会出现 `:2.1%` 这种没头的怪行。
（Linux 的 `comm` 总有值，不受影响。）

**防列举式漏项**（本项目踩过三次的模式）：未匹配的线程名在 **Debug 档原样列出**，
形如 `profile.cpu.unmapped wgpu-poll:2.1% dxgi-worker:0.3%`。漏了哪一类，
下次读日志就能看见，而不是永远静默归进"其他"。

前缀匹配必须**防串号**（与 F165 的 PDH `pid_1234` vs `pid_12345` 同族）：
`mullion-watchdog` 不得匹配上假想的 `mullion-watchdog2`。判据是"前缀 + 边界"，
不是裸 `starts_with`。

### 一条必须诚实标注的限制

**SSH 与 SFTP 的 task 都跑在同一批 tokio worker 上，按线程分不开**，只能合报 `tokio:31%`。

要真分开，得给 SFTP 单独一个 runtime，或者做 task 级打点（需要 tracing 基础设施）——
那是**改架构**，不是加日志，超出本切片范围（见 §9 非目标）。

配合 `profile.load` 那行的 `xfer=` / `in=` 可以推断（传输队列非空时 tokio 的涨幅大概率
来自 SFTP），但这是**推断不是实测**。设计上的处置：组名就叫 `tokio` 而不是
`ssh` 或 `network`——名字不许暗示它区分了内部用途。

## 5. F169 内存分块 + 显式余量（Info 常开）

### 记账的三块（RSS 侧）

| 块 | 来源 | 精度 |
|---|---|---|
| `scroll` | 各 pane：`history_lines() × cols × BYTES_PER_CELL` | **精确**（复用 `emulator.rs` 已有的 `BYTES_PER_CELL = 24` 与 `clamp_history` 同一个模型） |
| `xfer` | SFTP 传输队列在途缓冲字节数 | 精确（我们自己的结构） |
| `text` | `TextLayer` 的 `pool` / `temp` 两个 `Vec<Buffer>`（cosmic-text 的 layout 结果） | 估算 |

`其他` = `mem_process_mb` − 上面之和。

### 字形图集（atlas）**不记账** —— 设计原稿的修正

原稿把 `atlas` 列为第四块。**核实后删除**，两个独立理由：

1. glyphon 0.7 的 `TextAtlas` 字段全是 `pub(crate)`，`InnerAtlas` 本身也是 `pub(crate)`，
   外部**拿不到**纹理尺寸。公开 API 只有 `num_channels()` 和 `trim()`。
2. 更根本的：它是 **GPU 纹理，属于显存，本来就不在 RSS 里**。放进 `profile.mem` 是量纲错误。

显存侧同理**不做分块**（glyphon 不给数据），`profile.gpu` 只有 `vram=` 总量。
这一点要在文档里写明，免得后来人以为是漏了。

### 余量为负的处置

余量可能为负——记账是估算，且 RSS 不含未 touch 的页。

**处置**：`其他:0(记账超出RSS 12MB)`。不报负数、不 panic、不静默夹成 0。

"静默夹成 0"是最坏的选择：它会让"记账模型错了"这件事永远不被发现。
把超出量显式打出来，模型偏了就能立刻看见。

### 为什么余量必须显式打出来

`其他` 天然防住了"列举式漏项"：漏了哪一块，就体现为 `其他` 变大。
如果反过来只打三块、不打余量，读日志的人会以为 `128+24+16=168MB` 就是全部，
而真实 RSS 是 340MB。

**人工验收里有一条专门盯这个**：如果 `其他` 在真实长跑下常年占 60% 以上，
说明记账选错了块，模型要重做（见 §8）。

## 6. F170 GPU 分层

条件申请 `wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES`，
与 F165 已有的 `TIMESTAMP_QUERY` **各自独立降级**（两个 feature 分别判断，不捆绑）。

query set 从 2 槽扩到 3 槽：

| 槽 | 位置 |
|---|---|
| 0 | pass 开始（`beginning_of_pass_write_index`） |
| 1 | 终端趟结束（`pass.write_timestamp(&set, 1)`，在 `forget_lifetime` 之前） |
| 2 | pass 结束（`end_of_pass_write_index`） |

`term = t1 − t0`，`egui = t2 − t1`。

**不动单 pass 结构**——`app.rs` 里有专门注释解释为什么终端趟必须写在
`forget_lifetime` 之前、两趟为什么画进同一个 pass。拿渲染正确性换一个诊断指标，
正好反了（这也是 F165 已确立的原则："诊断指标不该反过来影响被诊断的对象"）。

**降级矩阵**（两个 feature 独立，四种组合都要有定义）：

| TIMESTAMP_QUERY | INSIDE_PASSES | 输出 |
|---|---|---|
| 有 | 有 | `frame=42x/p50=1.2ms/p95=3.4ms \| term:0.8ms egui:0.4ms` |
| 有 | 无 | `frame=42x/p50=1.2ms/p95=3.4ms \| 分层:n/a` |
| 无 | 有 | `frame=n/a`（防御性定义，见下） |
| 无 | 无 | `frame=n/a` |

第三行按 wgpu 合约**不会真实出现**：wgpu-types 23.0.0 的文档写明
`TIMESTAMP_QUERY_INSIDE_PASSES` "Implies `Features::TIMESTAMP_QUERY` …
is supported"（`lib.rs:532`）。矩阵仍给它定义（纯函数要对任意输入有定义，
且这是 wgpu 的合约不是我们的），但申请逻辑可以据此写成：
**仅当 adapter 同时报告两者时才申请 INSIDE_PASSES**，单独出现按"都不支持"处理。

连带改动别漏：query set 2 槽 → 3 槽时，`resolve` / `staging` 两个 buffer
从 16 字节扩到 24 字节（3 × 8），解算端按 3 个 u64 读。

GPU 分层归**便宜类，Info 常开**：`write_timestamp` 是录进 command buffer 的 GPU 侧命令，
不是系统调用，微秒级。

## 7. Debug 档才开的

按 §1 的两层决策，以下只在 `LogLevel::Debug` 输出：

| 项 | 行 | 为什么归 Debug |
|---|---|---|
| 未匹配线程名清单 | `profile.cpu.unmapped` | 只在扩分组表时才需要 |
| 每个 pane 的 scrollback 明细 | `profile.mem.panes` | pane 多时行会很长 |
| 传输队列逐 job 缓冲 | `profile.mem.xfer` | 队列深时行会很长 |
| 记账与 RSS 的差额详情 | `profile.mem.delta` | 排查记账模型用 |

> **`profile.mem.panes` / `profile.mem.xfer` 逐项明细：Task 10（`render_lines` 多行改造）本轮未接。**
> 数据层只到总量 gauge（`mem_scroll_bytes`/`xfer_running` 等），没有 per-pane / per-job 明细，
> 加它需要先给对应结构补逐项快照，超出本轮渲染改造的范围，延后。

## 8. 架构落点与不变量

| 文件 | 加什么 | 可测性 |
|---|---|---|
| `profile.rs` | `Scene` + `scene_of()`、线程分组纯函数、余量计算纯函数、`render_lines()` | **全部纯函数，可纯单测** |
| `sysprobe.rs` | 线程枚举 + 分组采样（两套 `#[cfg]`） | 平台 FFI，Linux 上可测 Linux 分支 |
| `diag.rs` | 接线：看门狗里采、`take_snapshot` 里装 | — |
| `gpu.rs` | INSIDE_PASSES 条件申请、3 槽 query set、两段解算 | — |
| `emulator.rs`（term） | 只读的 `scrollback_bytes()` | 纯单测 |
| 传输队列 / `text.rs` | 只读的 `bytes_estimate()` | 纯单测 |

**依赖方向不变**：`app → {core, term, ssh, store}`。各 crate 只暴露**只读的容量查询方法**，
不引入任何 UI 类型。`mullion-term` 加的 `scrollback_bytes()` 是纯计算，零 IO 零 async。

**决策逻辑一律留在纯函数里**（与 F164/F165 同一手法）：`scene_of`、分组映射、
余量计算、降级矩阵全部 `cfg`-free，能在无头 Linux 上跑测试；只有 FFI 外壳带 `#[cfg]`。

## 9. 测试

| 测什么 | 怎么测 |
|---|---|
| `scene_of` 优先级 | 每个 scene 一条；**并发输入取哪个**（传输+打字同时 → `sftp-transfer`）；`REMOTE_OUTPUT_BPS` 的两侧各一条（涓流不算 `remote-output`） |
| 线程分组 | 前缀匹配；未匹配进"其他"；**防前缀串号**；**空名进"其他"且 Debug 清单显示 `unnamed`** |
| 内存余量 | 正常；**余量为负报超出量而不是负数**；各块为 0 |
| 多行渲染 | 空闲时**零行**；每行前缀正确；五段确实从概览行移走了；**任何一行都不含 `\n`**（多行 = 多条独立记录的纯函数侧守护） |
| GPU 降级矩阵 | 四种 feature 组合各一条 |
| `scrollback_bytes` | 与 `clamp_history` 的预算模型**同源**（改了 `BYTES_PER_CELL` 两边一起动） |

每条守护测试都要**变异验证**（改一处让它变红、贴红色输出、再改回）——
这是本项目的既定纪律，恒绿测试比没测试更坏。

## 10. 验不了的（进人工验收清单，不得声称已完成）

- **线程分组数字**是否与 Process Explorer / 任务管理器的线程视图对得上
- **内存 `其他` 占比**在真实长跑下是否合理（常年 >60% = 记账选错了块，模型要重做）
- **INSIDE_PASSES** 在真实驱动上的行为（Linux 无头容器里 `#[cfg(windows)]` 那半根本不编译）
- **场景标签**是否真的对得上人的直觉（"我当时在滚屏，它说 remote-output"）
- 多行输出后日志体积的真实增长（影响 F166 的 64MB 轮转频率）

## 11. 非目标

- **不做 tokio task 级归因** —— 要 tracing 基础设施，且会改 runtime 架构
- **不做自定义分配器** —— 标签传播在 async 里做不对，错数据比没数据坏
- **不做采样剖析器 / 火焰图** —— 那是 profiler 的活，不是日志的活
- **不做显存分块** —— glyphon 不暴露 atlas 尺寸（§5）
- **日志不上传任何地方** —— 与 F155 的脱敏导出一致，用户手动交付
