# F164~F166 周期 CPU/GPU 剖面 + 一实例一日志 — 设计

> 2026-08-26。来源：`mullion.log` 的 profile 行只有内存没有 CPU/GPU，
> F158 那类「空闲烧满一个核」只能靠 wake=/rr= 间接推认；且多实例共写一个
> 日志文件，per-process 指标混流后会读出错误结论（此缺口 F160~F163 设计
> 文档里已记过一笔）。

## 编号

| # | 内容 | 优先级 |
|---|---|---|
| F164 | 周期 profile 行加进程 CPU% + 主线程 CPU%；CPU 超阈值强制打破空闲门 | P1 |
| F165 | GPU 三口径：PDH 引擎占用率 % + DXGI 本进程显存 + timestamp query GPU 帧耗时 | P1 |
| F166 | 一实例一日志文件 + 行内 pid + 运行期轮转 + 陈旧文件配额清理 | P1 |

F166 是 F164/F165 的前置：没有归属，per-process 的 CPU/GPU 数字在多实例下
是误导（两个各自稳定的进程混流成「一个进程在 6% 和 94% 之间抽风」）。
实现顺序：F166 → F164 → F165，一次发版。

## 1. 数据流与归属

```
watchdog 线程（5s 周期，主线程零成本）      渲染路径（主线程，每帧）
  sysprobe::CpuProbe.sample()               gpu.rs: QuerySet 双时间戳（抽样）
  sysprobe::GpuProbe.sample()   ← PDH          ↓ resolve → staging → map_async
  sysprobe::vram()              ← DXGI       diag::note_gpu_frame_us(µs) → 直方图
        ↓                                          ↓
   take_snapshot() 填 Snapshot ←──────────────── drain()
        ↓
   profile::render_line() → mullion-<instance>.log 单行
```

- 新模块 `crates/mullion-app/src/sysprobe.rs` 收纳三套平台探针。
  `diag.rs` 已 730 行，再塞三套 FFI 必爆；差分/百分比换算抽成纯函数单测，
  平台 FFI 只留薄壳。GPU 帧耗时不在 sysprobe——它长在渲染路径上，归
  `gpu.rs` + `app.rs`。
- 对外只暴露平台无关类型：`CpuSample { process_pct, main_thread_pct }`、
  `GpuSample { engines: Vec<(String, u8)> }`、`VramSample { used_mb, budget_mb }`。
  非 Windows / 探针不可用 / 无基线一律 `None`，渲染成 `n/a`。
- **探针有状态**：PDH 是速率型计数器，句柄常驻（`PdhOpenQuery` 一次 +
  每周期 `PdhCollectQueryData`，首次只作基线）；CPU 差分要留上一窗口的
  tick；DXGI adapter 枚举一次常驻。所以 `watchdog_loop` 持有
  `CpuProbe` / `GpuProbe` 两个 struct，不是自由函数。

## 2. F164 CPU

- Windows：`GetProcessTimes`（user+kernel）差分；主线程 `GetThreadTimes`。
  **主线程句柄必须在主线程上 `DuplicateHandle(GetCurrentThread())` 拿真句柄**
  （伪句柄传给 watchdog 线程会指向 watchdog 自己——静默错值，不报错）。
  取点：`start_watchdog`（main.rs 里跑在主线程上）。
- Linux：`/proc/self/stat` 字段 14/15；主线程 tid == pid，
  读 `/proc/self/task/<pid>/stat`。
- 换算：`pct = Δcpu_ns / (window_ns × N) × 100`。**进程 CPU% 以核数归一
  （满载 = 100%），主线程 CPU% 不归一（一个核跑满 = 100%）**——口径不同
  是有意的：F158 的症状原文是「烧满一个核」，归一化会把它压成 12% 而看不出来。
- 日志格式：`cpu=8%/主线程:96%`；采不到 `cpu=n/a`。
- **空闲门**（本功能的动机场景）：`Snapshot` 加
  `cpu_pct: Option<u8>` / `main_cpu_pct: Option<u8>`，`is_idle()` 追加
  `cpu_pct < IDLE_CPU_PCT(5) && main_cpu_pct < IDLE_MAIN_CPU_PCT(20)`。
  真空闲（CPU ≈ 0）照旧不写盘、硬盘能睡；「看着空闲实则烧核」自己跳出来。
  **`None` 不打破空闲门**——采样失败不能变成每 5 秒写一次盘。
  阈值是具名常量 + 单测，真机验收后可调。

## 3. F165 GPU 占用率（PDH，Windows 专属）

- 计数器 `\GPU Engine(*)\Utilization Percentage`，**必须用
  `PdhAddEnglishCounterW`**——`PdhAddCounterW` 吃本地化计数器名，中文
  Windows 上运行期找不到（不是编译期失败）。
- 读取：`PdhGetFormattedCounterArrayW`，实例名形如
  `pid_1234_luid_…_engtype_3D`；按本进程 `pid_<pid>_` 前缀过滤，按
  `engtype_*` 聚合求和。
- 输出取非零引擎前两名：`gpu=3D:14%/Copy:3%`；全零 `gpu=0%`；不可用 `gpu=n/a`。
- 实例名解析（pid 过滤 + engtype 提取）是纯函数，喂假字符串单测——
  这是最易写错又最难在无头机上发现的一段。
- 依赖：`windows-sys` 加 feature `Win32_System_Performance`。不引新 crate。

## 4. F165 显存（DXGI，Windows 专属）

- `CreateDXGIFactory1` → `EnumAdapters1`，用 `gpu.rs` 已有的
  `adapter.get_info()` 的 vendor/device 匹配 `DXGI_ADAPTER_DESC1` →
  `cast::<IDXGIAdapter3>()` → `QueryVideoMemoryInfo(0, LOCAL)`。
  报的是**本进程**的 `CurrentUsage/Budget`，与 wgpu 实际选了 D3D12 还是
  Vulkan 无关。DXGI free-threaded，watchdog 线程上用没有 COM 单元问题。
- 格式 `vram=312/8064MB`；匹配不上或非 Windows → `n/a`。
- 已知限制：两块同型号 GPU 时按 vendor/device 匹配取第一块。
- 依赖：`windows` crate 加 feature `Win32_Graphics_Dxgi`。

## 5. F165 GPU 帧耗时（timestamp query，跨平台）

- `request_device` 按 `adapter.features()` 条件申请
  `Features::TIMESTAMP_QUERY`；不支持整块降级 `gpu_us=n/a`，**不 panic**。
- **抽样而非每帧**：staging buffer 空闲时才给唯一那个 render pass
  （app.rs `begin_render_pass` 处）传 `timestamp_writes`（QuerySet 两条：
  beginning/end of pass），非采样帧传 `None`（零开销）。
  `resolve_query_set` → COPY 到 MAP_READ staging → `map_async`；回来后按
  `queue.get_timestamp_period()` 换 µs，喂 `diag` 直方图（与 `frame_us`
  同一套桶）。输出 `gpu_us=Nx/p50=…/p95=…`。
- **在途样本会悬着**：`map_async` 的回调要等后续 `poll`/`submit` 才触发，
  长空闲时样本停在半路——这不是泄漏，staging busy 的判定必须容忍它，
  下一帧渲染时自然收割。

## 6. F166 一实例一日志

- **文件名** `mullion-<instance_id>.log`，`instance_id` 复用 F148 的
  `{now_ms}-{pid}`（光用 pid 会撞上 OS 回收复用——上一次同 pid 实例的
  日志会被接着写）。F148 已测过该 id 可安全作文件名。
- **生成点上移**：从 `App::new`（app.rs:2245）挪进 `logx`，
  `OnceLock::get_or_init` 懒生成；`App::new` 改读 `logx::instance_id()`。
  日志文件 ⇄ F148 现场历史记录一一对应，排障时不用猜「崩的是哪个实例」。
- **行内加 pid 双保险**：`[ts] [1234] INFO mullion: …`——文件被改名、
  拼接、贴进 issue 后归属仍在。只加 pid 不加完整 id，前缀要短。
  行格式抽成纯函数 `format_line(ts, pid, msg)`（现有代码里是内联
  `format!`，不抽出来测不动）。
- **轮转改运行期语义**：文件名唯一后「启动时看上次文件多大」永远不触发，
  长跑实例无限涨。改为 watchdog 每秒 flush 时顺带 `metadata` 查大小，
  超限即轮转。放 watchdog 而非 `write_line_at`：热路径不塞系统调用（T3）。
  - **轮转步骤钉死**：锁内 `take()` → drop（flush + close）→ rename 成
    `.log.1` → 重开新文件。**不 rename 开着的文件**——那正是本设计要修的
    「句柄跟着 inode 走」陷阱，关了再挪是唯一确定性的做法。
  - 结构调整：`SINK` 从 `OnceLock<Option<Mutex<BufWriter>>>` 改为
    `OnceLock<Mutex<Option<BufWriter>>>`（Option 挪进锁内），否则运行期
    换不了文件。这是本次唯一一处非外科手术式改动，没有它运行期轮转做不了。
- **陈旧文件清理**（`init` 时执行）：扫 `mullion-*.log*`，
  **以 instance id 为单位配对**——主文件与它的 `.log.1` 同进退（否则死
  实例的 `.1` 成孤儿，慢性写满盘）。判活用 F148 `is_alive` 心跳；
  **活实例一律不动**；判死的按 mtime 倒序保留最近 5 组，其余删。
  - **竞态双保险**：①自己的文件按文件名（含自己的 instance_id）硬排除；
    ② mtime 在 60 秒内的一律不删。刚启动的实例头 15 秒可能还没写过第一
    次心跳（app.rs:2251 的负偏移只保证第一次 `about_to_wait` 写，扫盘
    可能发生在那之前），会被误判死；刚崩溃的实例日志恰恰是最该留的证据，
    也靠 mtime 最新 + keep-5 兜住。
- **旧文件不动**：既有 `mullion.log` / `.log.1` 不删不改名（用户可能开着
  看），只是没人再写。启动横幅写明新路径。
- **F155 脱敏导出**：导本实例当前文件，提示带文件名并说明「本机还有 N 份
  其他实例的日志」。脱敏规则不改（新字段全是数字），但给 redact 测试补
  一条断言确认新字段不被误伤。
- stderr 仍共用（行内有 pid 了，够用，不改）。

## 7. 测试与人工验收

自动测试（纯函数 + 假 sink/假字符串，不碰真盘真 GPU）：

| 对象 | 断言要点 |
|---|---|
| `cpu_pct(delta, window, cores)` | 归一口径：进程除核数、主线程不除 |
| PDH 实例名解析 | pid 前缀过滤 + engtype 提取 + 聚合，喂假字符串 |
| `is_idle()` 新判据 | CPU 超阈值打破空闲；`None` 不打破 |
| `render_line` | 新字段渲染 + `n/a` 分支 |
| `log_file_name` / `format_line` | 文件名含 id；行前缀含 pid |
| `should_rotate(len, limit)` | 运行期轮转判据 |
| `prune_plan(files, alive, self_id, now)` | **活实例不在删除计划**；自己硬排除；60s 内不删；`.1` 与主文件配对；keep-5 |
| 轮转真的换了 sink | 扩展现有 `Spy` 假 sink 手法 |
| redact | 新字段不被脱敏误伤 |

守护测试按既有纪律自证会变红（变异点写进测试注释）；派复核前过一遍
恒绿清单（memory: subagent-driven-review-lessons）。

真机验收清单（无头机验不了，进发版 notes）：

1. 开两个 exe：`%APPDATA%\mullion\config\` 下两个 `mullion-*.log`，
   各自 `cpu=`/`gpu=`/`vram=` 独立且合理，行前缀带各自 pid。
2. `gpu=` 与任务管理器 GPU 列对得上量级；`vram=` 与「专用 GPU 内存」对得上。
3. 空转 10 分钟：`cpu=` 接近 0 且**不写新行**（空闲门未被打破）。
4. 关一个实例、开第三个：死实例按配额回收（连 `.1` 一起），活实例文件没被删。
5. adapter 不支持 TIMESTAMP_QUERY（改注册表或换核显验）→ `gpu_us=n/a` 不崩。
6. debug 档跑到超限：运行期轮转出 `.log.1`，主文件重新从小开始。

## 8. 波及面清单

- `docs/adr-008-diagnostics.md`：补记路径变更（一实例一文件）。
- `.claude/skills/release-windows/SKILL.md` 验收清单里「打开 mullion.log」的措辞。
- `app.rs:2555` F155 导出、`logx::log_path()` 全部调用方。
- `Cargo.toml`：`windows-sys` + `Win32_System_Performance`；
  `windows` + `Win32_Graphics_Dxgi`。无新 crate。

## 9. 非目标

- 不做日志聚合查看器/合并工具（grep 多文件够用）。
- 不做 GPU 温度/频率/功耗（PDH 没有，要 NVML/ADL 这类厂商库——新依赖，越界）。
- 不做 Linux 的 GPU 占用/显存（主场景 Windows；`n/a` 即可）。
- 不改 stderr 通道。
