# 同赛道项目调研：meatshell

> 调研对象：<https://github.com/yituorou/meatshell>，快照 `7a4e241`（v0.7.3，2026-09-14）。
> 本文只回答一个问题：**它有哪些做法值得 Mullion 拿来优化。**
> 这一轮**不改任何产品代码**，产出是下面的排序清单 + 各条的验证方案。
> 结论落到 spec 的只有 F264 / N10（见文末）。

## 它是什么

Rust + [Slint](https://slint.dev) 写的 FinalShell 风格 SSH 客户端。34.5k 行、
CHANGELOG 2031 行、MIT/Apache 双许可。SSH 层同样用 `russh`，VT 仿真用 `vt100`
（我们用 `alacritty_terminal`），UI 用 Slint 的 femtovg/软件渲染器（我们用
wgpu + glyphon + egui）。功能面比我们宽：ZMODEM、串口/Telnet、MCP+CLI、
远端资源与进程监控、PPK、FinalShell 连接文件导入、壁纸主题。

**为什么值得看**：同一个问题域、同一个 SSH 库、同样把「低内存、原生」当卖点，
但渲染栈完全不同。凡是它在「协议层 / 调度层」上的做法，对我们多半可移植；凡是
落在「Slint 渲染器」上的，多半不可比——它的性能基线（整窗重绘、软件光栅）比我们
差得多，所以**它自报的收益倍数不能直接搬到我们身上**。

## 证据分档

| 标记 | 含义 |
|---|---|
| **[源码]** | 读了它的实现，给了 `文件:行`。结论可信。 |
| **[自陈]** | 只有 CHANGELOG / README 的说法，**未核实**。它的收益数字基于它自己的基线。 |

我们这边的现状一律读源码核实，给 `文件:行`。

---

## 排序清单

排序判据是**可验收性**：改完之后能不能被我们自己的日志（`profile.cpu` /
`profile.mem` / `ReadTiming`）直接量到。量不到的一律压底——我们没有做专项 A/B
的人力预算（见 `docs/field-capture.md` 的方法论）。

| 位次 | 条目 | 可验收判据 | 结论 |
|---|---|---|---|
| 1 | **SFTP 下载流水线** | 下载埋点的 `reads` 不变、`read_us` 应降到约 1/N | **做**（已登记 F264 / N10），但**第一步是补埋点**，现在量不出来 |
| 2 | ~~遮挡 / 最小化时冻结渲染~~ | — | **否掉**（2026-09-14 复核）：最小化我们早有，遮挡在 Windows 上收不到事件 |
| 3 | **回滚态降频** | 滚动阅读时的 `frame` 计数 | **值得做**，未登记编号 |
| 4 | ~~dev 构建提速~~ | 增量循环墙钟 | **改判：提速收益≈0**（实测 7.15s 编译 / 15.01s 跑测试）。留下的是**磁盘**问题，见正文 |
| 5 | **RUSTSEC 记录** | 无（工程卫生） | **改判：不是抄个文件**——meatshell 的「不可达」判据对我们**不成立** |
| 6 | **换全局分配器（mimalloc / jemalloc）** | 先看 `heapgauge` 的堆占用是否随时长单调上涨 | **备选**，现在不做 |
| 7 | **积压追赶 + 按字节预算提交** | 无现成量具 | **存疑**，压底 |

> **2026-09-14 复核**：在决定是否按本文开工前，逐条核实了「我们的现状」那一栏。
> 第 2、4、5 条都被推翻或改判，第 1 条挖出三个前置条件。**原始判断保留在各节里划掉的
> 段落中，不删**——记住「当时为什么判错」比记住结论有用。

下面逐条展开。

---

### 1. SFTP 下载流水线（第 1 位）

**它怎么做的** **[源码]**：`src/sftp/impls/sftp.rs:1968` 的 `download_impl`——

- 每次下载**开一条专用 SFTP channel**（`channel_open_session` +
  `request_subsystem("sftp")` + `RawSftpSession::new`，`:1982-1991`）；
- `CHUNK = 32 * 1024`、`MAX_INFLIGHT = 32`，注释原文
  `~1 MB outstanding hides the RTT`（`:1979-1980`）；
- 用 `FuturesUnordered` 攒在途 READ，每个 future 自带**绝对偏移** `off`，
  内部还有一圈 short-read 补齐循环（`:2018-2046`）；
- 完成后按 `seek(SeekFrom::Start(off))` + `write_all` **乱序落盘**（`:2055-2060`）；
- 进度**按已完成字节累加**（`done += data.len()`），不是按最大偏移，节流 150ms 上报；
- 取消 / 出错时 `drop(local_file)` + `remove_file`，**不留半截文件**（`:2110-2130`）；
- `total == 0`（大小未知）时回退到串行读到 EOF（`:2075` 起），保正确性。

**我们的现状** **[源码]**：

- **上传已经是流水线的，这条不用改**。`crates/mullion-ssh/src/sftp.rs:669`
  的 `write_chunk` 走 `write_all` → `russh-sftp` 的 `poll_write` 在
  `max_concurrent_writes` 未满时直接 `Poll::Ready` 入队、不等 ack
  （`russh-sftp-2.4.0/src/client/fs/file.rs:272-305`）。源码里那条注释没骗人。
- **下载是串行的**。`read_chunk`（`sftp.rs:658`）走 `AsyncRead::poll_read`，
  每次只发一个 READ 再 await（`file.rs` 的 `poll_read`）。
  `sftp.rs:641` 我们自己写着：**「`reads` 是这里面最要紧的那个数：READ 串行，
  次数即往返数」**——D8 当时就知道，只是没收口。
- 一个 64 MiB 的文件按 32 KiB 分块是 2048 个 RTT。200ms RTT 下光是往返就 400 秒。

**为什么排第 1**：高延迟代理链路是本项目的主场景（`spec.md` G1），而这条的
收益是数量级的；更关键的是**我们已经有现成的量具**——`ReadTiming { read_us, reads }`
就是为这件事埋的。

**移植风险（方案里必须写清）**：

1. **SSH channel window 的封顶**。russh 0.54 的 `Config::window_size` 默认
   **2 MiB**、`maximum_packet_size` 默认 **32768**
   （`russh-0.54.5/src/client/mod.rs:1691-1692`）。1 MiB 在途装得下 2 MiB 窗口，
   不会被流控劈回串行；`CHUNK = 32 KiB` 也正好与 `maximum_packet_size` 对齐
   ——meatshell 那两个常数**不是随手取的**。若将来想调大在途量，4 MiB 会顶到
   window 上，那时必须同时抬 `window_size`，否则改了白改。
   SSH 的流控窗口是**每 channel 一份**（RFC 4254），所以专用 channel 拿到的是
   自己的 2 MiB，与 ADR-009「一条连接多条 channel」不冲突——**我们不需要像
   meatshell 那样为 SFTP 另开一条 SSH 连接**。
2. **取消与世代回收**。T11 那一族的前科：`wind_down` 对在途 task 直接 `abort()`，
   完成事件永远不抵达；F132/F128 还踩过「混合任务池无差别 abort 腰斩传输，
   并把 load 永久卡在 Loading」。在途 READ 变成 32 个之后，abort 的后果从
   「腰斩一个请求」变成「腰斩 32 个」，本地文件必然是带空洞的。
   **落盘前就要定好：取消 = 删文件，不存在「断点续传」这个中间态**（与 meatshell
   同）。
3. **乱序落盘的保序**。每块带绝对偏移 + `seek` 是对的；但进度必须按**已完成字节**
   累加而不是最大偏移，否则进度条会在乱序完成时跳着走、并且在取消时报出比实际
   写入更多的字节。
4. **内存上限**。32 × 32 KiB = 1 MiB / 传输。多个传输并发时是 N × 1 MiB，
   要么限制并发传输数，要么把在途预算做成全局的。N5 盯着这个。

**复核补充的三个前置条件（2026-09-14）**：

5. **「我们已经有现成的量具」这句话是错的。** `ReadTiming` 只挂在
   `SftpClient::read_all`（`crates/mullion-ssh/src/sftp.rs:529`）上，而 `read_all`
   的调用方**只有「编辑打开」那一条**（`crates/mullion-app/src/app.rs:14815` 的
   F214 埋点）。真正的文件下载是 `app.rs:1699` 起**手写的 `read_chunk` 循环**，
   `copy_tree.rs:249`（远端→远端复制）和 `dragout/win.rs:141`（拖出）也是——
   **三条路径都没有任何计时或次数埋点**。
   所以 F264 的第一步不是改流水线，是**给下载循环补一条 F214 同款日志行**。
   补完就能立刻回答「你的下载到底慢不慢、慢在哪一段」，而这一步本身零风险。
6. **我们的块是 64 KiB，不是笔记假设的 32 KiB**，而且**实测每次都读满**。
   `crates/mullion-app/src/profile.rs:19` 的 `XFER_CHUNK = 64 * 1024` 大于 russh
   默认的 `maximum_packet_size`（32768），本来怀疑会被劈成两次往返——
   **2026-09-14 的真机实测否掉了这个怀疑**：`reads=1201`，正好是
   `78643200 / 65536 = 1200` 加一次 EOF，一次 short read 都没有。
   反过来说：**块还能往上加**（`read_all` 用的就是 256 KiB buf），而往返数与块
   大小成反比。这是一刀比流水线便宜得多的改动，而且 `read_chunk` 的循环本来就
   正确处理 short read，试大块是安全的。
7. **在途缓冲的内存记账不会自动跟走。** `profile.rs:19` 的注释写着
   「在途缓冲记账按它算（running × 此值），**改这里记账自动跟走**」——那条自动
   只对「一块在途」成立。加了 N 块之后 `xfer_running * XFER_CHUNK`
   （`profile.rs:1172`、`Snapshot::mem_other_mb`）会**静默少算 N 倍**，
   多出来的内存全记到 `profile.mem` 的「其他」栏上。改在途的同一笔提交必须改记账。

**真机基线（2026-09-14，v0.1.109，75 MiB 随机数据，代理链路）**：

```
下载:open=58ms read=153518ms×1201 write=96ms total=153714ms bytes=78643200 chunk=65536
```

| 段落 | 占比 |
|---|---|
| `read`（1201 次往返） | **99.87%**（153.5 s） |
| `write`（本地写盘） | 0.06%（96 ms） |
| `open`（一次往返） | 0.04%（58 ms） |

均值 **512 KB/s**，每次往返 **127.8 ms**。埋点的第一个用途就兑现了：
**瓶颈 100% 在往返，本地盘和 OPEN 都不是问题**，这一点不用再猜。

**但还有一问没回答，而它决定 F264 值不值得做**：那 127.8 ms 里，多少是 RTT、
多少是 64 KiB 走完链路的时间？`open` 那次几乎不带数据、只花 58 ms，若 RTT 就是
58 ms 左右，则剩下的约 70 ms 是在传 64 KiB——那意味着**带宽约 0.9 MB/s 封顶**，
流水线最多把 512 KB/s 提到 900 KB/s，**不到 2 倍**，不是 32 倍。

**便宜的判法**：把 `XFER_CHUNK` 从 64 KiB 提到 256 KiB 再测一次（一行常量，
记账基数自动跟走，`read_chunk` 本来就正确处理 short read）。

- `reads` 掉到约 301 **且** `total` 掉到约 40 s → **往返主导**，F264 该做，
  而且这一刀本身已经白赚 4 倍；
- `reads` 掉到约 301 **但** `total` 仍在 150 s 上下 → **带宽封顶**，
  F264 收益有限，优先级要大降；
- `reads` 明显多于 301 → 服务端的单次 READ 上限落在 64 KiB 和 256 KiB 之间，
  取中间值再试。

**一次实验同时是优化和诊断**，比直接动流水线便宜一个数量级。

**已经对的一件事**：取消语义不用新定。`app.rs:1690` 起下载本来就写到 staging 文件、
取消时 `remove_file` 再返回——与 meatshell 的「不留半截」同构。F264 要做的是让它
在 32 块在途下仍然成立，而不是从头设计。

**验收判据**（写进 F264 / N10）：同一个文件、同一条链路，下载前后对比**下载埋点**
（补完之后）——`reads` **不应变化**（总字节数没变，块大小没变），`read_us`
应降到约 `1/MAX_INFLIGHT`。`reads` 变了说明块大小被动过，两次测量不可比。

---

### 2. 遮挡 / 最小化时冻结渲染（第 2 位）→ **否掉（2026-09-14 复核）**

**结论先写：这一条两半都不成立，不要开工。**

| 原文的说法 | 复核结果 |
|---|---|
| 「最小化时不画——我们目前没有」 | **错，早就有。** `crates/mullion-app/src/shell/window_state.rs` 的 `Visibility::Minimized` + `RedrawScope::PumpOnly` **整帧跳过渲染**，两个调用点在 `app.rs:11244` 和 `app.rs:12491`；还带自愈（`recover_from_minimized`，防「一次异常的 `Resized(0,0)` 让窗口永久停在 PumpOnly」）和 4 条单测，其中 `minimized_still_pumps_io` 明文守着 T1 红线「最小化只省渲染，IO 泵必须继续」 |
| 「靠 `WindowEvent::Occluded` 触发」 | **这个事件在 Windows 上根本不发。** winit 0.30.13 `src/event.rs:421` 明写 `Android / Wayland / **Windows** / Orbital: Unsupported`；在 `platform_impl/windows/` 下 grep `Occluded` **零命中**。我们 `app.rs:12086` 那个分支在唯一的一等公民平台上是**死代码** |

剩下的只有「被别的窗口完全盖住」这半条，而 Windows 不提供这个事件——要自己写
Chrome 式的遮挡跟踪（枚举 z-order 上层窗口、算覆盖区、处理 DWM 合成与
`DWMWA_CLOAKED`）。代价远超收益，且改动点正好是 T3 / T7 / F158 三次事故的同一处。
**判定：否掉。** 顺带一笔技术债——`app.rs:12086` 那个 `Occluded` 分支可以留着
（macOS 上仍有效），但别再把任何 Windows 侧的行为挂上去。

<details>
<summary>原始判断（保留，用于对照「当时为什么判错」）</summary>

**它怎么做的** **[自陈]**：CHANGELOG #127「空闲降耗：失焦停光标闪烁 + 后台暂停/
降频系统采样」，自报「后台空闲 CPU 从约 10% 降到接近 0」；#340 补了「收起运行
状态面板或开启专注模式后暂停 `sysinfo` 采样与远端监控」。

**这条要拆成三块看，只有一块对我们成立**：

| 它的做法 | 对我们 | 依据 |
|---|---|---|
| 失焦停光标闪烁 | **已具备** | F125 明文「窗口失焦停闪（同时停掉周期性重绘，守 T3/T7）」；实现在 `crates/mullion-app/src/app.rs:4165` 的 `blink_on_at(self.window_focused, elapsed)` |
| 后台暂停系统采样 | **不适用** | 它的「采样」是资源监控侧栏（用户功能）；我们对应的是 watchdog / profile 埋点，那是实机观测的**唯一**手段，`docs/field-capture.md` 的整套方法论建在上面。F181~F183 之后绝对开销也已压下去了 |
| 遮挡 / 最小化时不画 | **值得做** | 我们目前没有。`wev.rs:118` 已经在认 `WindowEvent::Occluded`，但只用于归因统计 |

**为什么这一块值钱**：你的典型用法是把 Mullion 丢到后台、让远端 tmux 里的
Claude Code 刷输出。这时窗口**被完全遮挡**，但我们仍在逐帧走 tessellate +
`text_prepare` + present。F178 的分场景聚合显示 `remote-output` 场景
203 个窗口 / 13559 帧 / 帧 CPU 25.1 秒、`main` 均值 4.4%——**那是 CPU 花得最多的
场景**，而其中有多大比例发生在没人看的时候，目前没人量过。

**注意与 F178 的关系**：F178 判定「白跑帧收口不值得做」，理由是白跑帧集中在
`ui-only` 场景（CPU 只有 0.4 秒）。本条**不是 F178 的翻案**——F178 谈的是
「指纹命中但仍跑完 CPU 侧」，本条谈的是「窗口根本不可见时连 egui pass 都不必跑」，
判据是 `Occluded` 事件而不是指纹连续命中。但 F178 留下的约束仍然适用：
**不许只掐调度路径**（帧闸正在吸收自激，掐一半会让帧数不降反升），
T7 三分支必须显式复位 `control_flow`。

**还原路径**：字节照收、终端状态照更新，只跳过 present。还原时整帧指纹
（F159）天然会判 miss 并重画，不需要额外的「脏全屏」逻辑。

**验收判据**：`profile` 里遮挡期窗口的 `frame` / `present` 计数应掉到接近 0，
而 `in=` 的字节数不受影响（收字节没停）。

</details>

**这一条错在哪**：我查了 `wev.rs`（它**认识** `Occluded`，作为归因枚举的第 27 号），
就当成「这条路走得通」；又查了 `frame.rs`，没查 `shell/window_state.rs`。
**「代码里出现过这个词」不等于「这条路在目标平台上会被走到」**，
「归因层认识它」更不等于「行为层依赖它」。

---

### 3. 回滚态降频（第 3 位）

**它怎么做的** **[源码]**：`src/app.rs:70-78` 三档常量 + `:84-99` 的
`tab_render_interval` 按状态选档——

```rust
RENDER_MIN_INTERVAL             = 33ms   // 常态，30Hz
INTERACTIVE_RENDER_MIN_INTERVAL =  8ms   // 按键后 180ms 窗口内，120Hz
SCROLLED_RENDER_MIN_INTERVAL    = 100ms  // view_offset > 0，即用户滚上去了
```

选档逻辑（`:88-97`）：滚动态 → 100ms；打字窗口内 → 8ms；否则 33ms；
**ingest 锁被占住时也归到 100ms**（注释：「锁忙本身就是 firehose 信号，
推迟这次快照能避免 UI 线程加入争用」——这个判法挺聪明）。

**对我们的两个方向，只有一个成立**：

- **打字提频 → 不适用**。我们常态就是 16ms（60Hz），比它的「交互档」8ms
  只差一档，且 N3 明确要求「≤ 显示器刷新率，不超过」。提到 8ms 直接违反 N3。
  它需要这一档是因为它的常态只有 30Hz。
- **回滚降频 → 值得做**。用户滚到历史位置看时（F17 的滚动回溯），视口是
  **内容锚定**的，远端继续刷输出并不改变用户正在看的那几行——只有滚动条元信息在变。
  我们现在仍按 16ms 全速重绘。这是纯赚的：滚动阅读时既不影响观感，又能砍掉
  `remote-output` 场景里的一部分帧。

**风险**：降频只能降**重绘**，不能降**取字节 / 喂 VT**——回滚态下新来的输出仍要
进 scrollback，否则滚回底部时内容是断的。另外滚动条的位置元信息要跟着更新，
不然用户会以为卡住了（所以是 100ms 而不是「完全不画」）。

**验收判据**：滚动阅读期间的 `frame` 计数应掉到约 1/6（16ms → 100ms）。

---

### 4. dev 构建提速（第 4 位）

**它怎么做的** **[源码]** `Cargo.toml:170-175`：

```toml
[profile.dev]
opt-level = 0
debug = 1                       # 不是默认的 2

[profile.dev.package."*"]
opt-level = 1                   # 依赖轻度优化
```

配套 **[自陈]**（CHANGELOG:629）：Windows MSVC 下用 `rust-lld.exe`。

**我们的现状** **[源码]**：`Cargo.toml` 的 `[profile.release]` 只有 `lto` /
`codegen-units` 两行，**没有 `[profile.dev]` 段**；`.cargo/config.toml` 只配了
`x86_64-pc-windows-gnu` 的 mingw 链接器，**本机 target 没配任何链接器**（走默认 `cc`/bfd）。

**复核实测（2026-09-14，本机 16 核）——提速这个动机不成立**：

```
touch crates/mullion-app/src/app.rs
cargo test --workspace --no-run   →  7.15 s   （只重编 mullion-app + 链接 35 个测试二进制）
cargo test --workspace            → 15.01 s   （2205 条测试全跑，35 个二进制全绿）
```

增量循环总共 22 秒，其中链接不是瓶颈。三件事各自的收益因此都塌了：
`opt-level = 1` 只影响依赖的**一次性**编译和测试**运行**速度（15 秒里绝大多数是
纯逻辑断言，不是计算密集）；`debug = 1` 和换链接器治的是链接时间，而链接已经
淹没在 7 秒里。**为了这个去触发一次全量重编，不划算。**

**但复核挖出了一个真问题：磁盘。**

```
/data/Mullion/target        555 GB
  └ debug                   539 GB  =  deps 288 GB + incremental 222 GB + examples 28 GB
根分区                      914 GB，已用 723 GB（83%），只剩 153 GB
```

`deps` 那 288 GB 的大头正是完整调试信息（`debug = 2` 是 dev 的默认值），
`debug = 1`（line-tables-only）能砍掉其中很大一块，**代价是 dev 下只剩行号、
没有变量级调试信息**——对「靠日志和测试、不挂 gdb」的用法够用。
`incremental` 那 222 GB 是历史累积，cargo **不会自己清**。

所以第 4 条的正确形态不是「提速」，是**磁盘卫生**，而且有个顺序陷阱：
改 `[profile.dev]` 会让整棵树的 fingerprint 失效、全量重编，新旧产物在
`cargo clean` 之前**并存**——在只剩 153 GB 的情况下先改后清有塞满的风险。
**要做就是「先清、后改、再重编」，而清掉 539 GB 意味着下一次构建是全量的。**

**已执行（2026-09-14）**：只加了 `[profile.dev] debug = 1`，**没加**
`opt-level = 1`（提速收益已实测≈0，加它只会让一次性编译更慢）。
`cargo clean` 回收 **593.6 GiB**，根分区 83% → 20%。重编后：

| | 改前 | 改后 |
|---|---|---|
| `target` 总体积 | 563 GB | **5.4 GB** |
| 从零全量构建 + 跑 2205 条测试 | — | 73.31 s |
| 增量循环（`touch app.rs`） | 7.15 s 编译 / 15.01 s 测试 | 5.49 s / 14.31 s |

那 500 多 GB 绝大部分是**历史累积**（不同 fingerprint 的旧产物 + incremental
快照），不是单次构建的必需品——`incremental` cargo 不会自己清，攒够了仍要手动
`cargo clean`。代价是 dev 下只剩行号、没有变量级调试信息（要挂调试器时临时
`RUSTFLAGS="-C debuginfo=2"`）。

<details>
<summary>原始判断（保留）：可做的三件</summary>

**可做的三件**（都不影响产品行为，只影响你的开发循环）：

1. `[profile.dev.package."*"] opt-level = 1`——依赖只编一次，之后每次增量都受益；
   我们依赖不少（wgpu / naga / alacritty / syntect / russh），测试跑得更快。
2. `debug = 1`（只要行号不要完整调试信息）——显著缩小 dev 产物、缩短链接时间。
   代价：dev 下的 backtrace 仍有行号，够用。
3. 本机 target 换 `lld` 或 `mold`——链接是 2205 条测试的增量循环里的大头。

**注意**：`[profile.dev]` 不影响 `--release`，因此与 N6（exe 体积）、
`logx.rs` 的 panic backtrace（我们刻意不 strip、不 panic=abort 的那两条理由）
完全无关，不必重新论证。

**验收判据**：改前改后各跑一次 `cargo test --workspace` 的墙钟时间。

</details>

**另一件核实到的事**：本机**没有** `mold`，也没有系统 `lld`／`clang`。
toolchain 自带 `rust-lld`（`~/.rustup/toolchains/*/lib/rustlib/x86_64-unknown-linux-gnu/bin/`
下有 `rust-lld` 和 `gcc-ld/`），但在 stable 上把它接进本机 target 要么需要
`clang -fuse-ld=lld`，要么需要往 `.cargo/config.toml` 里写**带用户名的绝对路径**
——后者不该进被跟踪文件。换链接器这一件因此需要先 `apt install lld`（要 sudo），
在链接本来就不是瓶颈的前提下，**不建议做**。

---

### 5. `audit.toml` 式的不可达记录（第 5 位）

**它怎么做的** **[源码]** `Cargo.toml:51-56` 把「为什么不升 russh」写成一整段
注释，并有独立的 `audit.toml`（1.3 KB）登记 RUSTSEC-2026-0154：
它 pin 在 russh 0.49，而修复在 ≥0.60.3，升上去会拉进一堆 pre-release 加密 crate；
**理由是「我们从不用 ssh-agent，该 advisory 在本项目不可达」**，并留了复查条件
（等依赖离开 -rc 通道）。

**我们的现状**：没有 `audit.toml`，也没有跑 `cargo audit`。依赖版本锁定的理由
写在 `Cargo.toml` 的注释里（写得比它详细），但**「已知漏洞 + 为什么不修」这一类
没有落脚点**。

**值得抄的不是文件格式，是那个纪律**：把「知道有这个 advisory、判定不可达、
复查条件是什么」写下来。否则下一个人（或下一个我）看到 `cargo audit` 报红，
要么盲目升版本捅穿交叉编译，要么直接无视。

**复核改判（2026-09-14）：这不是「抄个文件」，因为它的不可达判据对我们不成立。**

meatshell 判定 RUSTSEC-2026-0154 不可达的理由是「**我们从不用 ssh-agent**」。
我们**用**：`crates/mullion-ssh/src/session.rs:397` 的 `authenticate_agent` 走
`russh::keys::agent::client::AgentClient::connect_env()`，`AuthMethod::Agent` 在
`dial.rs:284/314`、`hop.rs:155`、`session.rs:830` 都接着。我们 pin 的是
**russh 0.54.5**，比它的 0.49 新，但仍低于它记的修复版本 0.60.3。

两点必须如实标注：

- **那条 advisory 的内容我没能核实**——编号和「修复在 ≥0.60.3」都是从 meatshell 的
  `audit.toml` 读来的**[自陈]**，不是我查证的。本机装不了 `cargo audit`
  （`cargo audit --version` → `no such command`），而它拉的 advisory-db 在
  **github.com 上，本机 DNS 不通**，要走代理。
- **减轻因素**：`session.rs:408` 写着 ssh-agent 认证**仅支持 Unix，Windows 走不到**
  （「Windows 请用 -i 指定私钥」）。Windows 是唯一的一等公民，所以主场景暴露面为零；
  但 Linux/macOS 上走 agent 认证的用户是暴露的。

**已执行（2026-09-14）：`.cargo/audit.toml` 已建立，判据逐条写在里面。**

装了 `cargo-audit` 0.22.2、带 `HTTPS_PROXY` 拉到 advisory-db，跑出来
**6 条 vulnerability + 9 条 informational**。逐条核实后：

| advisory | 判定 |
|---|---|
| RUSTSEC-2026-0195 / 0194 `quick-xml` 0.30 | **不可达**：引入链是 `accesskit_unix ← accesskit_winit ← egui-winit`，`cargo tree -i quick-xml@0.30.0 --target x86_64-pc-windows-gnu` → "nothing to print" |
| RUSTSEC-2026-0257 `webbrowser` 1.2.1 | **不可达**：漏洞在 Unix 的 `BROWSER` 模板解析上，Windows 走 ShellExecute |
| RUSTSEC-2023-0071 `rsa` Marvin | **无修复版本**；我们只拿 RSA 做认证签名，不做解密 |
| RUSTSEC-2026-0154 `russh` agent 帧 | **低**：advisory 限定在 agent 帧，而 agent 认证**仅 Unix**（`session.rs:408`），且需本地 agent 已被控 |
| **RUSTSEC-2026-0153 `russh-cryptovec`** | **可达，故意不 ignore** |

**最后那条是这一趟真正的收获**，而且**恰恰是照抄 meatshell 抄不到的**：
advisory 原文说 0.58.0 之前的 russh 把 `CryptoVec` 用在 **transport packet reads
和 zlib 解压输出**上，「remote compressed payload expansion 导致分配失败时可以让
进程 abort」。我们是 **russh 0.54.5**，而且 **显式开着 `flate2`**——压缩路径是开的。
影响面只有 DoS（advisory 明写无 RCE / 完整性 / 机密性影响），但**不是不可达**，
所以让它继续报红，直到做出决定。三条出路都有代价（升 russh 会拉 -rc 加密 crate；
关 `flate2` 会丢掉高延迟链路上最值钱的压缩；或者接受现状等依赖出 -rc），
够格单开一个 ADR。

**顺带印证了「别照抄结论」**：meatshell 对 RUSTSEC-2026-0154 的判词是「我们从不用
ssh-agent」，我们**用**——同一条 advisory、不同的判据，结论只是碰巧都落在低风险。

**成本**：一次带代理的 `cargo audit`（需先 `cargo install cargo-audit`）+ 一个文件。
**重跑记得带代理**，本机 DNS 解析不了 github，advisory-db 拉不下来。

---

### 6. 换全局分配器（第 6 位，**备选，现在不做**）

**它怎么做的** **[源码]** `src/allocator/mod.rs`：Windows → `mimalloc::MiMalloc`，
Unix 系 → `jemallocator::Jemalloc`，其余 → `System`；配一个
`allocator_name()` 和按 `cfg` 断言的单测。
**[自陈]**（CHANGELOG:48，#410）：「终端关闭后会及时释放缓存及关联状态，并按平台
选择合适的全局内存分配器，**降低长时间、多会话使用时的内存占用**」。

**为什么判它是备选而不是候选**：

1. **它治的病和我们的 N5 不是同一个**。原话说的是「长时间、多会话使用时」——
   那是**碎片化累积**；我们的 N5 是**空载常驻**（8 pane / 10000 行回溯 < 300MB）。
2. **收益上限被 GPU 侧封死**。N5 解剖（v0.1.77）把 289MB 压到 155MB 之后，
   F176/F177 查明剩下那 **128MB 连 wgpu 自己的两个内存 API 都看不见**，
   在 GPU 驱动侧。分配器够不着它。堆那部分本来就是小头。
3. **三个具体代价**：要叠在 `heapgauge` 外层（`crates/mullion-app/src/lib.rs:56`
   的 `#[global_allocator]` 现在指着我们自己的计数分配器，F184~F191 那一轮
   专门论证过「没有它就答不出是谁在吃内存」，不能为了换分配器把它摘掉）；
   mimalloc 是 C 库，给 mingw 交叉编译链再加一个 C 依赖（本机有
   `x86_64-w64-mingw32-gcc`，能不能过**未验证**）；exe 体积吃进 N6 预算。

**改成先让现有日志回答**：翻 `profile.mem` 与 `heapgauge` 的历史日志，找
**「堆占用随使用时长单调上涨、且关掉标签后不回落」**的证据。
- 找到了 → 这条升级成候选，再谈换分配器；
- 没找到 → 判它对我们无用，写进结论别再提。

这一步零成本、零风险，而且**不需要你做任何专项测试**——日志已经在了。

---

### 7. 积压追赶 + 按字节预算提交（第 7 位，**存疑**）

**它怎么做的**：**[源码]** `src/terminal/impls/render_gate.rs` 是一个
基于**代次票据**的闸门：`request()` 发票并返回 `should_schedule`（只有
Idle→Scheduled 那一次为真，天然合并）、`begin_flush()` 捕获「这次快照覆盖到哪张票」、
`finish_flush(through, visible)` 结算并判断是否需要补一轮，`wait_for(ticket)` 用
condvar 等票。有 5 条单测，其中
`hidden_flushes_settle_without_throttling_the_next_request` 说明**隐藏标签的
flush 不消耗可见帧预算**。
**[自陈]**（CHANGELOG:188，#311）另有「事件积压过大或夹有连接状态事件时优先追赶队列」，
配 `PACED_LOCAL_BACKLOG_LIMIT = 1 MiB` / `PACED_QUEUE_EVENT_LIMIT = 256`
（`src/app.rs:66-67`）。

**为什么对我们存疑**：它这套的存在前提是**每个标签一个后台线程在等 UI 快照**
（`wait_for` + condvar），而我们是**单事件循环、每帧 drain**
（`crates/mullion-app/src/session_pump.rs:7`：「app 每帧：先 drain SSH 接收端得到
`inbound`，调 `pump`，再把返回值交 `SshSession::write`」）。
我们**没有「丢失通知」这个问题**，也没有「后台线程空等」这个问题——那正是它这套
机制要解决的。合并也是天然的：每帧 drain 一次等于把这一帧内的所有输出合成一次解析。

**唯一还新鲜的半条**：「积压很大时先追赶、不做节奏控制」。我们的帧闸是时间驱动的
（16ms），不看积压量。理论上高延迟链路突发大输出时，先追赶再节流可能更快到达
稳态。但**我们没有量具能证明这是个问题**，而这块代码正是 T3 / T7 / F158 三次事故
的同一处。按 F178 立下的规矩——**离群猜想不立项**——这条压底存档，等出现实报再说。

---

## 已核实「我们已具备或更优」的

写在这里是为了**防止下次有人重新立项**。

| 它的条目 | 我们的对应物 |
|---|---|
| 失焦停光标闪烁（#127） | F125 已实现，`app.rs:4165` `blink_on_at(self.window_focused, ..)` |
| 最小化时不渲染（#127 的另一半） | **已实现**，`shell/window_state.rs` 的 `Visibility::Minimized` → `RedrawScope::PumpOnly` 整帧跳过，带自愈与 4 条单测（含 T1 红线「泵不能停」）。见第 2 条 |
| SFTP 取消不留半截文件 | 已具备：下载写 staging 文件，取消时 `remove_file`（`app.rs:1690` 起），与它同构 |
| 合并输出事件修 `tail -f` 假死（#171） | `session_pump.rs:7` 每帧 drain 即等价；我们另有帧闸（T3）与整帧指纹（F159） |
| scrollback 改双端队列（#290） | v0.1.62 已做同类优化 |
| 拖选只刷轻量选区图层 | F172（行带顶点差分）+ F174/F175（内容寻址整形缓存）是更强形态 |
| SFTP 上传流水线（#16） | 已具备（russh-sftp 的 `max_concurrent_writes`，见第 1 条） |
| `set_nodelay` | **我们更周到**：`crates/mullion-ssh/src/dial.rs:68` 显式设了，还注释说明「手搓 `connect_stream` 绕过了 `client::connect` 对 `Config.nodelay` 的应用」。meatshell 的 SSH **没设**（`russh` 默认 `nodelay: false`，Nagle 开着），只有 Telnet 设了（`src/terminal/impls/telnet.rs:137`） |
| `feed_batched` 分批喂 VT（`term_buffer.rs:509`） | **不适用**：那是为了在行滚出屏幕前捕获 scrollback，`vt100` 没有自带回溯；`alacritty_terminal` 有 |

另外两条它的经验对我们**结构上不可比**：它的 UI 渲染器可切换（CPU / femtovg /
Skia，macOS 默认改回 CPU 渲染），我们是 wgpu 单一路径（ADR-001）；它的多窗口是
Chrome 式单进程（`src/app/single_instance.rs`），我们是多进程 + 现场历史（F148）。

---

## 功能缺口（只登记，本轮不评估）

按本轮口径（只收性能与工程）不展开，仅记录「它有、我们没有」，供以后按
`spec.md` 的边界自行判断：

| 它有的 | 备注 |
|---|---|
| 终端内 ZMODEM（`sz` 下载 / `rz` 多文件上传） | `src/terminal/impls/zmodem.rs` + `zmodem_send.rs`，约 900 行 |
| 串口 / Telnet 会话 | 超出我们 spec 的边界 |
| MCP server + CLI（复用 GUI 的会话与凭据） | `meatshell mcp serve` / `meatshell cli exec`；与我们「用 Mullion 驱动 Claude Code」的场景有潜在关系 |
| PuTTY PPK v2/v3 私钥 | `src/ssh/impls/ppk.rs`，551 行，纯内存转换不落临时文件 |
| FinalShell 连接文件导入 | `src/config/impls/finalshell.rs`（DES + MD5 + `java.util.Random`） |
| 彩色 emoji（Twemoji PNG 顶替单色字形） | 按 grapheme 切 ZWJ / 肤色 / 旗帜，图像严格占原单元格。与我们的 T9 是同一个问题的另一条解法（我们走 GBK 白名单 + `ui::icon` 自绘，**终端里的 emoji 没有兜底**） |
| 远端资源 / 进程监控侧栏 | 含结束进程（带 sudo 二次确认） |
| 出站代理、跳板、-L/-R/-D | 我们都有 |
| 壁纸 / 沉浸式主题 | 与 G1「零可见闪烁」冲突，`spec.md §8` 已明确否掉 |

---

## 方法论备注

三条值得记住的：

1. **它自报的收益不能直接搬。** #127 说「后台空闲 CPU 10% → 接近 0」，听着像是
   我们也能省 10%——但它的基线是「周期性定时器触发**整窗重绘**」，而我们近空闲
   窗口实测是 `frame=1 present=1 wake=1`（每 5 秒醒一次画一帧，F178）。
   同一个改动在两边的收益差着两个数量级。**凡是自陈收益，先问「它的基线是什么」。**
2. **同一条改动在两个项目里可能治不同的病。** 分配器那条最典型：它治碎片化，
   我们的痛点是空载常驻，而空载常驻的大头在 GPU 侧。不先对齐病因就抄药方，
   最好的结果是白做。
3. **抄之前先查我们有没有。** 这一轮我起初把「失焦停光标闪烁」列成候选，
   读到 `frame.rs` 那层发现纯函数不看焦点就下了结论——**错了**，上一层
   `app.rs:4165` 早就传了 `window_focused`，F125 明文写着。
   **纯函数那一层不是判据，接线那一层才是**（这正好是记忆里反复出现的
   「App 的方法测不了 → 交付判据整体恒绿」同一个形状）。
4. **「抄之前先查我们有没有」这条纪律，写下来不等于做到了。** 上一条是这份笔记
   自己的方法论，结果同一份笔记的第 2 条又踩了一模一样的坑（最小化冻结早就有）。
   复核时才发现的原因是：**第一轮我只查了「有没有出现这个词」，没查「行为挂在
   哪个开关上」**。可操作的版本是——查三处而不是一处：①归因/日志层认不认识它
   （`wev.rs`）②纯函数层（`frame.rs`）③**行为开关层**（`shell/window_state.rs`）。
   前两处命中都可能是假信号。
5. **移植一个做法之前，先查它依赖的平台事件在我们的目标平台上存不存在。**
   `WindowEvent::Occluded` 在 winit 里是一等公民的枚举成员，编译、匹配、测试
   全都正常——只是 Windows 后端从不发它。**这类「编译过、跑起来静默什么也不发生」
   的坑，只有读 winit 的平台说明 + grep 平台后端才拦得住**，与
   `docs/gui-render-gotchas.md` 里那一批同源。
6. **「我们已经有现成的量具」是最值得怀疑的一句话。** 第 1 条原文这么写，但
   `ReadTiming` 只覆盖了「编辑打开」一条路径，真正要量的下载路径没有埋点。
   **量具存在 ≠ 量具接在你要量的那条路上。**

---

## 落到 spec 的

- **F264**（只登记不做）：SFTP 下载流水线。
- **N10**（新增 NFR）：高延迟链路下的 SFTP 下载吞吐。

其余条目（回滚降频、dev 构建、`audit.toml`）**有意不给编号**——
它们要么不是用户可见功能、要么是工程卫生，按本项目的编号惯例不该占 F 号。
真要开工时从本文取方案即可。遮挡冻结已在复核中否掉，不必再登记。

## 复核后的开工建议（2026-09-14）

按「能不能现在就动手」分三档：

| 动作 | 状态 |
|---|---|
| **给下载路径补计时埋点**（F264 第一步） | **可以直接做**：零风险、纯加日志行，做完立刻能回答「下载到底慢不慢」。这是 F264 / N10 变得可验收的前提 |
| **F264 主体**（32 块在途） | 等埋点跑过一次真机、确认确实是痛点之后再开工。连带要改 `XFER_CHUNK` 的记账公式 |
| **回滚态降频**（第 3 条） | 可做，排在 F264 之后 |
| 清 `target` + 改 `[profile.dev]`（第 4 条） | **需要你点头**：539 GB vs 一次全量重编 |
| 带代理跑 `cargo audit`（第 5 条） | **需要你点头**：要装工具 + 配代理 |
| 分配器（第 6 条）、积压追赶（第 7 条） | 维持原判，不动 |
