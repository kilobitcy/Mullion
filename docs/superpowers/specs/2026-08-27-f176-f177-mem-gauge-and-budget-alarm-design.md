# F176 / F177 内存口径收口与 N5 预算闸 —— 设计

日期：2026-08-27
起因：N5 切片（v0.1.77，空载 289→155MB）复盘

---

## 1. 动因：归因系统没瞎，是没人信它、也没人被它叫醒

N5 那轮排查里，128MB 的 wgpu 次分配器 chunk 是靠 **VMMap 三轮手工解剖**定位的。
复盘时的第一直觉是「我们的 `profile.mem` 对这类回归失明，得补 `gpu=` 桶」。
查证之后这个前提站不住，两条路都堵死：

| wgpu 23 的内存 API | 为什么救不了这次 |
|---|---|
| `Device::generate_allocator_report()` | **只有 DX12 后端实现**（`wgpu-hal-23/src/dx12/device.rs:1917`），`src/lib.rs:975` 的默认实现直接返回 `None`。本项目实机跑的是 Vulkan（日志 `GPU: … backend=Vulkan`），恒为 `None` |
| `Device::get_internal_counters()`（`counters` feature） | Vulkan 后端只累加 `block.size()`（`vulkan/device.rs:1110`），即**次分配出去的那一小块**。128MB 是 gpu_alloc free-list 的 **chunk（reserve）**，不是 allocation。counters 会如实报 `buffer_memory: 4MB`，而进程里躺着 128MB |

而实机日志翻出来的真相是：

```
profile.mem 428MB = scroll:0 xfer:0 text:5 其他:423
```

**这行躺了几百次。`其他` 占 98.8%。** 数据一直都在，问题是两件事：

- **D1 这个数没人信。** `428` 是 `PrivateUsage`（提交量），任务管理器那列是**专用工作集**，
  同一台机器同一时刻是 `289`。Linux 分支的同一字段又是 `/proc/self/statm` 的 RSS。
  三个口径共用一个名字、彼此对不上，所以排查时没人拿它当依据——**这才是那轮
  直接绕开日志上 VMMap 的根因**。
- **D2 这个数不会叫人。** 98.8% 未归因在日志里和一切正常时长得一模一样，
  没有任何东西对它报警。

本切片修 D1（**F176**）与 D2（**F177**）。拆「其他」这件事**明确不做**，理由见 §5。

---

## 2. F176 —— 内存口径收口

### 2.1 日志形态

```
Windows:  profile.mem commit=428MB(ws 289) = scroll:0 xfer:0 text:5 其他:423
Windows（老系统回落）:
          profile.mem commit=428MB(ws n/a) = scroll:0 xfer:0 text:5 其他:423
Linux:    profile.mem rss=155MB = scroll:0 xfer:0 text:5 其他:150
```

`=` 绑定在**打头那个主数**上。`ws` 括在后面，是交叉核对用的第二个数，不参与减法。

### 2.2 减法算在 commit 上，ws 只做核对（反直觉，理由要写进注释）

工作集**会被系统裁剪**——窗口最小化时 Windows 会把大半工作集甩出去。而记账块
`scroll/xfer/text` 是 Rust 堆上的 `Vec`，字节数不因页被换出而变小。拿 ws 做被减数，
用户一最小化就会刷屏 `其他:0(记账超出RSS 87MB)`：把一个正常的系统行为报成
记账模型崩了。

`PrivateUsage`（commit）不被裁剪、恒 ≥ 我们的堆量，减法才成立。

ws 的职责是另外两件：**它是你在任务管理器里看到的那个数**，以及 **F177 的判据**
（`spec.md:420` 的 N5 原文写的是「常驻内存」＝工作集，不是提交量）。

两个数各司其职，各自的理由分别写在 `diag.rs` 采样处与 `profile.rs` 渲染处。

### 2.3 取哪个字段（这里有个容易选错的坑）

`PROCESS_MEMORY_COUNTERS_EX.WorkingSetSize` 是**总**工作集，含共享页：系统 DLL、
exe 映像、mmap 进来的字体（N5 那轮 VMMap 量到 98.8MB Mapped File）。
拿它对照任务管理器照样对不上，只是差到另一个方向去。

要的是 `PROCESS_MEMORY_COUNTERS_EX2.PrivateWorkingSetSize`
（本项目锁定的 **`windows-sys 0.59.0`** 里已有：
`Windows/Win32/System/ProcessStatus/mod.rs:138`，与 `EX` 同属已开的
`Win32_System_ProcessStatus` feature，**不必新开 feature**）。

**仍是同一个 `K32GetProcessMemoryInfo` 调用**，只把 `cb` 换成 `EX2` 的尺寸——
零额外系统调用。代价：`EX2` 需要 Windows 11 / Server 2022。

**取数顺序**：先按 `EX2` 尺寸调一次；失败则按 `EX` 尺寸重调一次，此时
`ws_bytes = None`、日志印 `(ws n/a)`。Windows 11 是本项目一等公民
（`CLAUDE.md`），回落路径只为老机器兜底，不是主路。

**`PrivateWorkingSetSize == 0` 一律当采不到**：结构体是 `zeroed()` 出来的，
若某个系统上 `EX2` 返回成功却没填这个字段，读到的就是 0；而一个跑着的进程
专用工作集不可能真为 0。这是本项目「采不到不许编成 0」规矩的**反向**用法——
这里 0 不是伪造出来的读数，而是「没被填写」的唯一可辨识痕迹。

### 2.4 数据层改动

```rust
// diag.rs
pub enum MemKind { Commit, Rss }          // 主数量的是什么，随采样一起下沉

pub struct MemSample {
    pub process_bytes: u64,
    pub kind: MemKind,                    // 新增
    pub ws_bytes: Option<u64>,            // 新增：专用工作集，Linux 恒 None
    pub sys_avail_bytes: u64,
    pub sys_total_bytes: u64,
}
```

**平台差异全部下沉到采样处，渲染层不带 `#[cfg]`。** 这样「Windows 那种输出」
能在 Linux 开发机上直接单测——符合本项目「没有窗口也能测」的架构约束
（`CLAUDE.md` 架构不变量）。Linux 分支填 `kind: Rss, ws_bytes: None`。

```rust
// profile.rs
pub struct Snapshot {
    pub mem_process_mb: u64,              // 不动（既有 0 哨兵债照旧）
    pub mem_kind: MemKind,                // 新增
    pub mem_ws_mb: Option<u64>,           // 新增
    …
}

pub fn mem_parts(
    kind: MemKind,
    primary_mb: u64,
    ws_mb: Option<u64>,
    scroll_b: u64, xfer_b: u64, text_b: u64,
) -> String
```

`mem_ws_mb` **用 `Option` 而不是沿用 `mem_process_mb` 那个 0 哨兵**：0 哨兵是
F155 的既有债（`profile.rs:908` 的注释自己承认了），不再复制第二份。

`mem_parts` 现有的两条分支（正常 / `其他:0(记账超出RSS NMB)`）**原样保留**。
超出分支是 F169 明确要的「记账模型错了要被发现」，不因为换了口径就砍掉。

### 2.5 F176 的守护测试

| 测试 | 钉住什么 | 变异 |
|---|---|---|
| `mem_parts_renders_commit_and_ws_on_windows` | 双数格式 | 删 `(ws …)` 段 |
| `mem_parts_renders_a_single_number_when_there_is_no_working_set` | Linux 形态 `rss=…MB`，**不印** `(ws n/a)` | 让 `None` 也印括号 |
| `mem_parts_says_n_a_when_the_working_set_could_not_be_sampled` | Windows + `None` → `(ws n/a)` | 回落时静默印 0 |
| `the_remainder_is_computed_against_commit_not_the_working_set` | 喂 `ws < scroll+xfer+text < commit`，断言**不**走超出分支 | 把被减数换成 `ws_mb` |
| 既有 `mem_parts_reports_the_remainder_honestly` | 含 `accounted == process_mb` 的相等边界（F169 自查挖出的恒绿点） | 保留不动 |

第四条是重点：「顺手把两个数统一成一个」正是这段代码日后最可能被重构掉的方式，
而那么改之后日志照写、数字照有，只在最小化窗口时才暴露。

---

## 3. F177 —— N5 预算闸

### 3.1 判据必须是绝对阈值，百分比那条路是死的

第一直觉是「`其他` 占比超过 X% 就报警」。不成立：我们只记 `scroll/xfer/text`
三种**内容**缓冲，基线的 GPU 显存、字体、代码段、egui 从来不在账上。空载时
`其他` 天然接近 100%，**健康版本也一样**。按百分比报警等于常亮。

```rust
/// spec.md:420 的 N5：常驻内存（8 pane，10000 行回溯）< 300MB。
///
/// **这个数在核显机器上含义更严**：UMA 没有独立显存，wgpu 的每一份分配都
/// 计进工作集（N5 切片实测 165MB WriteCombine 全在账内）。独显机器上同样的
/// 代码会低一大截。看到读数逼近上限时，先问「核显还是独显」再下结论。
pub const N5_BUDGET_MB: u64 = 300;

/// 报告步长：越界后，比上次报告值又高这么多才再报一次（§3.3）。
pub const MEM_REPORT_STEP_MB: u64 = 64;

/// 回落滞回带。**删掉它不会有任何测试以外的报错**，但空闲进程会因为
/// ws 在阈值附近抖动而每几个窗口写两行日志——硬盘永不休眠（§3.3）。
pub const MEM_HYSTERESIS_MB: u64 = 16;
```

**阈值不随 pane 数浮动**（曾考虑 `160 + 18 × panes`）：那会凭空引入一个拍脑袋的
系数，并把「预算」这个 spec 概念稀释成公式。按 0.1.77 的 155MB 基线，8 pane
满载离 300 还有距离；真响了就值得看一眼。

### 3.2 输出

```
WARN  profile.mem.over ws=428MB > N5 300MB (commit 512, 其他 507)
INFO  profile.mem.over 回落 ws=180MB
```

行前缀沿用 `profile.mem.delta` 的命名法，可 grep。

**用 `warn!` 有附带好处**：`logx.rs:50` 那条 `level <= Warn` 让这类记录**立即落盘**、
不跟着缓冲走——进程随后崩了/被杀了，现场才留得住。

**回落那一行必须有。** 否则读日志的人看到一条越界警告，无从判断它是「当时闪了一下」
还是「从那以后一直这样」。

### 3.3 报告节奏

**首次越界报一次；之后只在比上次报告值又高 ≥ `MEM_REPORT_STEP_MB`（64）时再报；
跌回阈值以下报一次 `回落` 并复位。**

**边界写死**：`Cross` 判 `ws > N5_BUDGET_MB`（严格大于，恰好 300 不算超）。

**`Recover` 判 `ws <= N5_BUDGET_MB - MEM_HYSTERESIS_MB`（16），不是简单的
「跌回阈值以下」——不加这条滞回带，§3.4 拿来绕开空闲门的那个理由当场作废。**

ws 在阈值附近抖动（299↔301）是常态：Windows 会主动裁剪工作集，进程自己
一次分配释放也能跨过去。没有滞回时每一次穿越都产出一对 `Cross`/`Recover`，
两行日志每几个窗口来一遍——**一个空闲进程被这条警告永远吵醒**，正是
§3.4 承诺「O(1) 不是 O(每 5 秒)」时排除掉的那件事。

加了 16MB 滞回后：抖在 284~301 之间时首次 `Cross` 一次，之后保持越界态、
全程 `Quiet`；真回落（428→180 那种）照常报 `回落`。

**代价要说清**：ws 长期停在 290 时不会出「回落」行，日志上停留在
「越界中」。可接受——之前那条 WARN 报的数就是 301，读者不会被带偏多少；
而反过来（不加滞回）的代价是硬盘永不休眠，量级完全不同。

不复用 `diag::should_report`（`diag.rs:821`）那套翻倍：内存从 300 翻到 600 才吭
第二声，中间一条几百 MB 的慢泄漏全程静默。固定步长对内存更合适。

```rust
/// 两个变体携带的都是**触发本次判定的那个 `ws_mb`**。
/// `Cross(mb)` 时调用方把 `reported_mb` 置为 `Some(mb)`；
/// `Recover(mb)` 时置回 `None`（复位后能再次 `Cross`）。
pub enum BudgetVerdict { Quiet, Cross(u64), Recover(u64) }

/// 纯函数。调用方持有 `reported_mb: Option<u64>` 状态
/// （语义：上一次报告 `Cross` 时的 ws；`None` = 当前不在越界态）。
pub fn budget_verdict(ws_mb: Option<u64>, reported_mb: Option<u64>) -> BudgetVerdict
```

**`ws_mb == None`（采不到）一律 `Quiet`。** 同 `cpu_is_busy` 里那条
`is_some_and`（`profile.rs:477`）的道理：读不到的机器上不能凭空报警。

### 3.4 这条警告必须穿透空闲门

`render_lines` 开头 `is_idle()` 直接返回空（`profile.rs:723`），而**空载正是它拦掉的**——
N5 那次要查的场景一行都不写。

空闲门存在的理由是「别每 5 秒吵醒笔记本硬盘」（`profile.rs:440` 的注释）。
这条警告**每次越界只写一次**，是 O(1) 不是 O(每 5 秒)，那个理由套不上它。
这也正好补回本次讨论最初选定的方向：空载基线终于能自己留痕。

**实现上不在 `render_lines` 里开后门**，而是拆成独立一步：驱动处先问
`budget_verdict`（不看 `is_idle`），再照旧走 `render_lines`。

### 3.5 F177 的守护测试

| 测试 | 钉住什么 | 变异 |
|---|---|---|
| `budget_verdict` 状态机四条 | 首次越界 `Cross`；同值再来 `Quiet`；+64MB 再 `Cross`；跌回滞回带下沿后 `Recover` 且复位后能再 `Cross` | 去掉步长、改成每窗口都报 |
| `jitter_around_the_threshold_does_not_wake_the_disk` | 喂 `301 → 299 → 301 → 299`，断言只有第一个出 `Cross`、其余全 `Quiet`（**一条 `Recover` 都不许有**） | `Recover` 的边界去掉滞回、改回 `ws <= N5_BUDGET_MB` |
| `the_threshold_is_strictly_greater_than` | 恰好 `ws == 300` 判 `Quiet` | `>` 改 `>=` |
| `no_reading_means_no_alarm` | `ws_mb: None` 恒 `Quiet` | `is_some_and` 改 `is_none_or` |
| `an_idle_process_that_is_over_budget_still_gets_a_warning` | **接线**：空闲快照 + 超预算 ws → 仍出警告行 | 把 `budget_verdict` 挪进 `render_lines` 的 `is_idle` 之后 |

**最后一条（`an_idle_process_…`）是整个 F177 最容易在日后重构中被悄悄埋掉的一点**：
把 `budget_verdict` 挪进 `is_idle` 之后，编译过、测试若只测纯函数也全绿，
只有实机空载时才发现它不响。

---

## 4. 依赖方向与架构约束

改动全部落在 `mullion-app`（`diag.rs` / `profile.rs` / 事件循环驱动处）。
不动 `core` / `term` / `ssh` / `store`，不新增依赖，不新增 crate feature。
`windows-sys` 已是既有依赖，`PROCESS_MEMORY_COUNTERS_EX2` 在锁定版本里已存在。

---

## 5. 明确不做的

- **拆「其他」桶（原 ③ / VirtualQuery 自扫地址空间）。** 记在账上。
  等 F176/F177 落地后，若警告响了却仍定位不了，再回头做。届时的方案是
  Windows 上 `VirtualQuery` 遍历自身地址空间，按 Protection/Type 分桶
  （WriteCombine≈GPU、Private=堆、Image、Stack）——即 VMMap 干的事的精简版。
  它会带来平台分支和一个启发式判据，现在为一次已修好的故障建专用仪表不划算。
- **接 wgpu `counters` feature。** 见 §1 的表：Vulkan 后端下它拆不动 chunk，
  开了也白开，还多一个 feature 和一堆原子加减。
- **mmap 字体方案。** N5 切片已否决（省 18.8MB 但运行期锁死
  `C:\Windows\Fonts\msyh.ttc`，挡系统字体更新）。
- **补 F174/F175 的 spec.md 表行。** 已发现 `spec.md` 的 F 表停在 F173、
  F174/F175 只在提交信息里，属既有漂移；本切片不顺手修（Scope Discipline），
  单独记一笔。

---

## 6. 你无法验证的东西（人工验收）

无头容器里能验的只有纯函数与接线。以下要实机：

- [ ] `profile.mem` 的 `ws` 数与**任务管理器进程页「内存」列**在同一时刻一致
      （这是 F176 存在的全部理由，也是唯一的真判据）
- [ ] 最小化窗口再恢复，`ws` 明显掉下去又涨回来，而 `其他` **不**出现
      `记账超出RSS`（§2.2 那条设计的实证）
- [ ] 老 Windows（非 11）上回落路径生效、印 `(ws n/a)` 而不是崩或印 0
      —— 手边没有这种机器的话，此条如实标注为未验证
- [ ] 人为把 `N5_BUDGET_MB` 临时调到低于当前占用，确认 `profile.mem.over`
      按「首次一次 / +64MB 再一次 / 回落一次」的节奏出现
- [ ] 空载静置两分钟，确认 `profile.mem.over` 该响时响得到（穿透空闲门）、
      不该响时全程安静（不吵醒硬盘）

---

## 7. 风险

- **`PROCESS_MEMORY_COUNTERS_EX2` 的实际行为**只在文档层面核实过（字段存在、
  MSDN 标 Windows 11+）。「不支持时是返回 FALSE 还是成功但不填字段」两种可能
  都做了处置（回落重调 + 0 当采不到），但**哪一种真的发生**要实机才知道。
- **`N5_BUDGET_MB = 300` 对核显机器可能偏紧**。0.1.77 的 155MB 基线给了余量，
  但 8 pane + 10000 行回溯的满载数从未实测过。若实机满载稳定在 280~300，
  该调的是这个常量而不是关掉警告——常量改动要连同 §3.1 的注释一起更新。
