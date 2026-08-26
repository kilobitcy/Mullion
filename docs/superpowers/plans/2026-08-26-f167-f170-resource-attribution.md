# 资源用量归因（F167~F170）Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 v0.1.71 的五个纯总量口径（cpu/mem/gpu/vram/gpu_us）升级成带归因的多行输出：场景标签（F167）、CPU 按线程分组（F168）、内存分块+显式余量（F169）、GPU term/egui 分层（F170）。

**Architecture:** 决策逻辑全部是 `cfg`-free 纯函数（`scene_of`/线程分组/余量计算/多行渲染，都在 `profile.rs`，Linux 上可测）；平台 FFI 只有 `sysprobe.rs` 的线程枚举薄壳；数据流沿用既有模式——主线程往 diag 的原子 gauge 里写、看门狗 5 秒采一次拼 `Snapshot`。

**Tech Stack:** Rust；wgpu 23.0.1（`TIMESTAMP_QUERY_INSIDE_PASSES`）；Windows FFI（Toolhelp32 + `GetThreadTimes` + `GetThreadDescription`）；Linux `/proc/self/task`。

**Spec:** `docs/superpowers/specs/2026-08-26-resource-attribution-design.md`（用户决策已锁定，实现不得自行更改）。

---

## 全局纪律（每个任务都适用，不再重复）

1. **每个守护测试必须变异验证**：按注释里写的变异改一处 → 跑测试贴红色输出 → 改回。恒绿测试比没测试更坏。**变异验证前先 commit**（历史上两次被 `git checkout` 吞掉未提交编辑）。
2. 每个任务结束跑 `cargo fmt --check`，不干净就 `cargo fmt`（v0.1.71 踩过：计划里的代码片段没按 rustfmt 排版，攒到最后一起炸）。
3. 「绿」= `cargo test --workspace` 全过 **且** `cargo clippy --workspace --all-targets -- -D warnings` 无输出。大输出先落盘：`cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log`。
4. 提交信息中文、带 spec 编号。
5. `#[cfg(windows)]` 代码在本机**完全不编译也不跑 clippy**——Windows 分支写完靠 Task 14 的交叉编译验证编译通过。
6. 日志格式串里的中文（"个/剩/行/其他/主线程"）不受 T9 字形白名单约束（那是 egui UI 字符串的事，日志不走 egui 字体链），但**别在日志里发明生僻字**。

## 文件结构（改哪些、各自负责什么）

| 文件 | 动作 | 职责 |
|---|---|---|
| `crates/mullion-term/src/emulator.rs` | 加 `scrollback_bytes()` | scrollback 记账，与 `clamp_history` 同一预算模型 |
| `crates/mullion-app/src/files/queue.rs` | `Summary` 加 `active` | 未收尾 job 数（场景判据 + 分母） |
| `crates/mullion-app/src/profile.rs` | Snapshot 新字段；`Scene`/`scene_of`；线程分组；内存行；`render_lines` | **全部归因决策逻辑，纯函数** |
| `crates/mullion-app/src/diag.rs` | 新计数器/gauge + 看门狗接线 + 多条记录写盘 | 数据集散地 |
| `crates/mullion-app/src/sysprobe.rs` | `ThreadCpuProbe`（两套 `#[cfg]`） | 线程 CPU 时间采集 |
| `crates/mullion-app/src/gpu.rs` | INSIDE_PASSES 条件申请；GpuTimer 2→3 槽 | GPU 分层采样 |
| `crates/mullion-app/src/text.rs` | 加 `bytes_estimate()` | text 记账块 |
| `crates/mullion-app/src/app.rs` | `count_scroll` 埋点 ×3；gauge 计算；pass 内槽 1 | 接线 |
| `spec.md` / 发版 | F167~F170 行；v0.1.72 | 收尾 |

---

### Task 1: `mullion-term` 的 `scrollback_bytes()`

**Files:**
- Modify: `crates/mullion-term/src/emulator.rs`（`history_lines` 在 194 行附近，测试 mod 在文件尾部）

- [ ] **Step 1: 写失败测试**（加进 `emulator.rs` 已有的 `mod tests`）

```rust
    /// F169:记账与预算必须同一个模型。
    ///
    /// 自证会变红:把 `scrollback_bytes` 里的 `BYTES_PER_CELL` 换成常量 `16`,
    /// 或把 `cols()` 换成写死的 `80`。
    #[test]
    fn scrollback_bytes_shares_the_budget_model_with_clamp_history() {
        let mut emu = Emulator::with_history(10, 2, 3);
        for i in 0..10 {
            emu.feed(format!("L{i}\r\n").as_bytes());
        }
        let lines = emu.history_lines();
        assert!(lines > 0, "喂了 10 行,历史不该是空的");
        assert_eq!(emu.scrollback_bytes(), lines * 10 * BYTES_PER_CELL);
        // 空历史 = 0 字节,不是「每行保底」——空 pane 不该被记账。
        let fresh = Emulator::with_history(10, 2, 3);
        assert_eq!(fresh.scrollback_bytes(), 0);
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-term scrollback_bytes -- --nocapture`
Expected: FAIL，`no method named scrollback_bytes`

- [ ] **Step 3: 实现**（放在 `history_lines()` 旁边）

```rust
    /// F169:scrollback 当前占用的字节数(按满行估算的上界)。
    ///
    /// 与 [`clamp_history`] 用同一个 `BYTES_PER_CELL` —— 预算和记账必须
    /// 同源,改常量时两边一起动(守护测试盯着)。
    pub fn scrollback_bytes(&self) -> usize {
        self.history_lines() * usize::from(self.cols()) * BYTES_PER_CELL
    }
```

- [ ] **Step 4: 跑通**：`cargo test -p mullion-term` 全绿
- [ ] **Step 5: commit**，然后变异验证（改 `BYTES_PER_CELL` 为 16 → 红 → 改回）

```bash
git add -A && git commit -m "feat(term): scrollback_bytes 记账查询,与 clamp_history 同预算模型 (F169)"
```

---

### Task 2: `Queue::Summary` 加 `active`

**Files:**
- Modify: `crates/mullion-app/src/files/queue.rs`（`Summary` 在 91 行，`summary()` 在 275 行，测试 mod 345 行起，fixture `fn job(dir)` 已有）

- [ ] **Step 1: 失败测试**（加进已有 `mod tests`）

```rust
    /// F167:场景判据「传输队列非空」用的是 active(未收尾条数),不是
    /// running —— pending 的 job 也说明用户正等着传输。
    ///
    /// 自证会变红:把 `summary` 里 `s.active += 1` 挪进 `Running` 分支。
    #[test]
    fn active_counts_pending_and_running_but_not_finished() {
        let mut q = Queue::new(1);
        let a = q.push(job(Direction::Download));
        let _b = q.push(job(Direction::Download)); // 并发 1,这条留在 Pending
        assert_eq!(q.take_runnable(), vec![a]);
        assert_eq!(q.summary().active, 2, "1 running + 1 pending");
        q.progress(a, 100);
        q.finish(a, Ok(()));
        assert_eq!(q.summary().active, 1, "收尾的不算");
    }
```

（`progress`/`finish` 的实际签名以 `queue.rs` 里已有测试的调用方式为准——写之前先看同文件测试里怎么调的，签名不同就照真实签名改这两行，断言不变。）

- [ ] **Step 2: 确认失败**：`cargo test -p mullion-app active_counts -- --nocapture` → `no field active`
- [ ] **Step 3: 实现**：`Summary` 加字段 + `summary()` 循环里补一行

```rust
    /// F167:没收尾的条数(Pending + Running)。场景判据与 profile.load 的分母。
    pub active: usize,
```

`summary()` 的 for 循环里、`if j.state.is_finished()…continue` **之后**加：

```rust
            if !j.state.is_finished() {
                s.active += 1;
            }
```

（注意放在 `continue` 之后与 `busy` 同一段落，逻辑与 `busy` 同判据。）

- [ ] **Step 4: 跑通** + 检查既有 `Summary::default()` 相关测试没被 `PartialEq` 破坏（`Default` derive 会自动带上 `active: 0`）
- [ ] **Step 5: commit + 变异验证**

```bash
git add -A && git commit -m "feat(app): 传输队列 Summary 加 active 未收尾计数 (F167)"
```

---

### Task 3: diag 新计数器/gauge + Snapshot 新字段 + `take_snapshot` 装配

**Files:**
- Modify: `crates/mullion-app/src/diag.rs`（statics 集中在 119~160 行，`count_*` 函数 315 行起，`take_snapshot` 690 行起）
- Modify: `crates/mullion-app/src/profile.rs`（`Snapshot` 162 行起，`empty()` 在 315 行附近）

- [ ] **Step 1: Snapshot 加字段**（`profile.rs`，放在 `gpu_frame_us` 之后；`empty()` 里全部补零值）

```rust
    /// F167:本窗口的用户滚动事件数(滚轮/翻页键/拖拽自动滚,计次量)。
    pub scroll_events: u64,
    /// F167/F169:传输队列此刻未收尾条数(状态量,读而不清)。
    pub xfer_jobs: u64,
    /// F169:未传完的字节(total - done,状态量)。
    pub xfer_bytes_left: u64,
    /// F169:在跑的传输条数(在途缓冲 = running × 64KiB chunk)。
    pub xfer_running: u64,
    /// F169:全部 pane 的 scrollback 记账字节(gauge,主线程每帧更新)。
    pub mem_scroll_bytes: u64,
    /// F167:全部 pane 的回溯总行数(profile.load 的分母)。
    pub scroll_lines: u64,
    /// F169:TextLayer 的 Buffer 估算字节(gauge)。
    pub mem_text_bytes: u64,
    /// F168:线程组 CPU(组名, 不归一不封顶百分比)。固定顺序,见 group_threads。
    pub thread_groups: Vec<(&'static str, u32)>,
    /// F168:没进分组表的线程原名(Debug 档打出来,防列举式漏项)。
    pub thread_unmapped: Vec<(String, u32)>,
    /// F168:线程枚举这一窗口成功过。false → profile.cpu 的分组段渲染 n/a。
    pub thread_available: bool,
    /// F170:终端趟 GPU 耗时分布(槽1-槽0)。
    pub gpu_term_us: Counts,
    /// F170:egui 趟 GPU 耗时分布(槽2-槽1)。
    pub gpu_egui_us: Counts,
    /// F170:INSIDE_PASSES 拿到了。false → 分层渲染 `分层:n/a`。
    pub gpu_split_supported: bool,
```

- [ ] **Step 2: diag.rs 加 statics 与写入函数**（statics 挨着 267 行的 `TABS/PANES/HOSTS`；函数挨着 `set_scale`）

```rust
// F167:滚动事件(计次,swap 清零)。
static SCROLL_EVENTS: AtomicU64 = AtomicU64::new(0);
// F167/F169:传输与内存 gauge(状态量,load 读)。
static XFER_JOBS: AtomicU64 = AtomicU64::new(0);
static XFER_RUNNING: AtomicU64 = AtomicU64::new(0);
static XFER_BYTES_LEFT: AtomicU64 = AtomicU64::new(0);
static MEM_SCROLL_BYTES: AtomicU64 = AtomicU64::new(0);
static SCROLL_LINES: AtomicU64 = AtomicU64::new(0);
static MEM_TEXT_BYTES: AtomicU64 = AtomicU64::new(0);
// F170:GPU 分层直方图与支持标志(仿照 GPU_FRAME_US 的既有写法)。
```

```rust
/// F167:用户滚了一下(滚轮一档/一次翻页/拖拽自滚一帧都算一次)。
pub fn count_scroll() {
    SCROLL_EVENTS.fetch_add(1, Ordering::Relaxed);
}

/// F167/F169:传输队列规模。relaxed 原子存,帧路径可调(与 set_scale 同款)。
pub fn set_xfer_gauges(active: u64, running: u64, bytes_left: u64) {
    XFER_JOBS.store(active, Ordering::Relaxed);
    XFER_RUNNING.store(running, Ordering::Relaxed);
    XFER_BYTES_LEFT.store(bytes_left, Ordering::Relaxed);
}

/// F169:内存记账 gauge。
pub fn set_mem_gauges(scroll_bytes: u64, scroll_lines: u64, text_bytes: u64) {
    MEM_SCROLL_BYTES.store(scroll_bytes, Ordering::Relaxed);
    SCROLL_LINES.store(scroll_lines, Ordering::Relaxed);
    MEM_TEXT_BYTES.store(text_bytes, Ordering::Relaxed);
}

/// F170:一次分层采样(µs)。由 GpuTimer 回读回调调用(wgpu 内部线程)。
pub fn record_gpu_split_us(term_us: u64, egui_us: u64) { /* 仿 record_gpu_frame_us,两个直方图 */ }
/// F170:GPU 初始化时报告 INSIDE_PASSES 是否拿到。
pub fn set_gpu_split_supported(v: bool) { /* AtomicBool store */ }
```

`take_snapshot` 里装配（**计次量 swap、状态量 load**，与既有注释的语义一致）：

```rust
    s.scroll_events = SCROLL_EVENTS.swap(0, Ordering::Relaxed);
    s.xfer_jobs = XFER_JOBS.load(Ordering::Relaxed);
    s.xfer_running = XFER_RUNNING.load(Ordering::Relaxed);
    s.xfer_bytes_left = XFER_BYTES_LEFT.load(Ordering::Relaxed);
    s.mem_scroll_bytes = MEM_SCROLL_BYTES.load(Ordering::Relaxed);
    s.scroll_lines = SCROLL_LINES.load(Ordering::Relaxed);
    s.mem_text_bytes = MEM_TEXT_BYTES.load(Ordering::Relaxed);
    // gpu_term_us/gpu_egui_us:drain 两个直方图(仿 gpu_frame_us 现状)
    // gpu_split_supported:load AtomicBool
```

- [ ] **Step 3: 守护测试**（diag.rs 或 profile.rs 已有快照测试模式，如果 diag 没有测试 mod 就放 profile.rs，用 pub 接口喂）——**关键语义**：`scroll_events` 是计次量必须清零，`xfer_jobs` 是状态量必须保留。

```rust
    /// F167:计次量与状态量的清零语义不能弄反。
    ///
    /// 自证会变红:把 take_snapshot 里 SCROLL_EVENTS 的 `swap(0,..)` 改成
    /// `load(..)`,或把 XFER_JOBS 的 `load` 改成 `swap(0,..)`。
    #[test]
    fn scroll_is_drained_but_xfer_gauge_survives_the_snapshot() {
        count_scroll();
        set_xfer_gauges(2, 1, 48 << 20);
        let a = take_snapshot(5000);
        assert_eq!(a.scroll_events, 1);
        assert_eq!(a.xfer_jobs, 2);
        let b = take_snapshot(5000);
        assert_eq!(b.scroll_events, 0, "计次量必须随窗口清零");
        assert_eq!(b.xfer_jobs, 2, "状态量描述此刻,不许被清");
    }
```

注意：diag 的 statics 是进程级全局，测试之间会串——看同文件/同工程已有测试怎么防的（若已有串扰约定就照做；没有就在断言里只比较差值）。`take_snapshot` 目前是私有 `fn`，测试同模块可见；若放 profile.rs 则把 `take_snapshot` 改 `pub(crate)`。

- [ ] **Step 4: 全绿 + fmt + commit + 变异验证**

```bash
git add -A && git commit -m "feat(app): 归因数据层 —— 滚动计数/传输与内存 gauge/GPU 分层槽位入快照 (F167~F170)"
```

---

### Task 4: app.rs 埋点接线

**Files:**
- Modify: `crates/mullion-app/src/app.rs`
  - `count_scroll`：8127（滚轮 `WheelAction::LocalScroll`）、8310（翻页键 `pane.emulator.scroll(scroll)`）、9245（拖拽自滚）三处，各自紧跟 `emulator.scroll(...)` 之后加 `diag::count_scroll();`
  - gauge 计算：`diag::set_scale(` 调用点（8479 附近）之后
  - `XFER_CHUNK`：两处 `vec![0u8; 64 * 1024]`（73 行偏移的传输 worker 与 1390 行）
- Modify: `crates/mullion-app/src/text.rs`：加 `bytes_estimate()`

- [ ] **Step 1: text.rs 加估算接口**（`TextLayer` impl 里）

```rust
/// F169:一个整形完的 Buffer(一行终端文字)的估算驻留字节。
/// 粗估:一行 ~200 格,每字形的 layout/shaping 结果按几十字节算。
/// 这是**估算**(spec §5),精度要求是量级正确,守护测试只钉「与 Buffer
/// 数成正比」这一层。
pub const BUFFER_EST_BYTES: usize = 4096;
```

```rust
    /// F169:文字层驻留内存估算 = (缓存 + 池 + 临时槽)的 Buffer 数 × 单价。
    pub fn bytes_estimate(&self) -> usize {
        (self.cache.len() + self.pool.len() + self.temp.len()) * BUFFER_EST_BYTES
    }
```

- [ ] **Step 2: app.rs 顶层加常量并统一两处 chunk**

```rust
/// 传输 worker 的读写 chunk。**F169 的在途缓冲记账按它算**(running × 此值),
/// 改这里记账自动跟走。
const XFER_CHUNK: usize = 64 * 1024;
```

两处 `let mut buf = vec![0u8; 64 * 1024];` 改成 `let mut buf = vec![0u8; XFER_CHUNK];`。

- [ ] **Step 3: gauge 计算**（紧跟 `diag::set_scale(...)` 之后；遍历**全部**标签，不是只有活动 workspace——记忆里的教训「drive_* 每帧驱动函数必须遍历全部标签」在这里同样适用）

```rust
                // F169:内存记账 gauge。遍历全部标签(不是只有活动 ws):
                // 后台标签的 scrollback 也占内存。几十个 pane 的整数乘加,帧预算内。
                let mut scroll_bytes = 0u64;
                let mut scroll_lines = 0u64;
                for tab in self.tabs.iter() {
                    if let Some(t) = tab.content.as_terminal() {
                        for p in t.ws.panes() {
                            scroll_bytes += p.emulator.scrollback_bytes() as u64;
                            scroll_lines += p.emulator.history_lines() as u64;
                        }
                    }
                }
                diag::set_mem_gauges(scroll_bytes, scroll_lines, self.text.bytes_estimate() as u64);
                let xs = self.transfer.queue.summary();
                diag::set_xfer_gauges(
                    xs.active as u64,
                    (xs.up + xs.down) as u64,
                    xs.bytes_total.saturating_sub(xs.bytes_done),
                );
```

（`self.text` 的字段名以 app.rs 实际为准——渲染处用 `a.text.render(...)`，App 字段名照着找；`TerminalTab` 的 workspace 字段是 `ws`。）

- [ ] **Step 4: 守护测试**（app.rs 已有「源码切片断言」模式，18777 行附近有现成的 `diag::set_scale(` 检查——照同一模式，**注意记忆里的教训：锚点用带上下文的串，别用裸前缀**）

```rust
    /// F167/F169:埋点接线守护。三处滚动调用点每处都要跟 count_scroll,
    /// gauge 计算必须遍历 self.tabs(全部标签)而不是 active_ws。
    ///
    /// 自证会变红:删掉任意一处 `diag::count_scroll();`,或把遍历改成
    /// `self.active_ws()`。
    #[test]
    fn scroll_and_gauge_wiring_is_present_in_source() {
        let src = include_str!("app.rs");
        assert_eq!(
            src.matches("diag::count_scroll();").count(),
            3,
            "滚动埋点必须恰好三处(滚轮/翻页/拖拽自滚)"
        );
        assert!(src.contains("diag::set_mem_gauges(scroll_bytes"));
        assert!(
            src.contains("for tab in self.tabs.iter() {\n                    if let Some(t) = tab.content.as_terminal()"),
            "内存记账必须遍历全部标签"
        );
    }
```

（源码切片测试的已知弱点：改签名会恒绿。这里断言的是**调用次数**与**遍历对象**，两者都不是签名。）

- [ ] **Step 5: 全绿 + fmt + commit + 变异验证**（删一处 `count_scroll` → 红 → 恢复）

```bash
git add -A && git commit -m "feat(app): 滚动/传输/内存 gauge 埋点接线,chunk 提成 XFER_CHUNK 常量 (F167/F169)"
```

---

### Task 5: `Scene` 与 `scene_of`

**Files:**
- Modify: `crates/mullion-app/src/profile.rs`

- [ ] **Step 1: 失败测试**

```rust
    /// F167:场景优先级。并发时取优先级最高的单值;涓流不算 remote-output。
    ///
    /// 自证会变红:把 scene_of 里 sftp 与 scrollback 两个 if 对调,或把
    /// `>= REMOTE_OUTPUT_BPS` 改成 `>`(边界值那条会抓住)。
    #[test]
    fn scene_priority_and_the_trickle_threshold() {
        let mut s = Snapshot::empty();
        s.window_ms = 5000;
        assert_eq!(scene_of(&s), Scene::Idle);
        s.frames = 10;
        assert_eq!(scene_of(&s), Scene::UiOnly);
        s.connects_ok = 1;
        assert_eq!(scene_of(&s), Scene::Connecting);
        // 阈值两侧:5 秒窗口,1024 B/s 阈值 → 5120 字节是分界。
        s.inbound_bytes = 5119;
        assert_eq!(scene_of(&s), Scene::Connecting, "涓流不算远端刷屏");
        s.inbound_bytes = 5120;
        assert_eq!(scene_of(&s), Scene::RemoteOutput);
        s.keys = 1;
        assert_eq!(scene_of(&s), Scene::Typing);
        s.stage_us[crate::diag::Stage::Resize as usize] = one_sample();
        assert_eq!(scene_of(&s), Scene::Resize);
        s.scroll_events = 1;
        assert_eq!(scene_of(&s), Scene::Scrollback);
        s.xfer_jobs = 1;
        assert_eq!(scene_of(&s), Scene::SftpTransfer, "传输+打字+滚动并发时传输最优先");
    }
```

（`one_sample()`：构造一个只有一条样本的 `Counts`——看 `Histogram::record_us`/`Counts` 在同文件测试里怎么造的，照抄；没有现成的就写 helper：新建空 `Counts` 后把桶 0 计 1。）

- [ ] **Step 2: 确认失败**：`cargo test -p mullion-app scene_priority -- --nocapture` → 编译错误 `cannot find scene_of`
- [ ] **Step 3: 实现**

```rust
/// F167:remote-output 与「OSC 7 提示符心跳涓流」的分界。
/// 涓流每提示符几十字节,真刷屏至少 KB/s 级 —— 1 KB/s 在两者之间有两个
/// 数量级余量。具名常量,好调。
pub const REMOTE_OUTPUT_BPS: u64 = 1024;

/// F167:这 5 秒程序主要在干什么。单值,优先级命中即停(spec §3)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scene {
    SftpTransfer,
    Scrollback,
    Resize,
    Typing,
    RemoteOutput,
    Connecting,
    UiOnly,
    /// 空闲门在这之前就把整组拦掉了,实际日志里永不出现;
    /// 留着是因为纯函数要对任意输入有定义(spec §3)。
    Idle,
}

impl Scene {
    pub fn label(self) -> &'static str {
        match self {
            Scene::SftpTransfer => "sftp-transfer",
            Scene::Scrollback => "scrollback",
            Scene::Resize => "resize",
            Scene::Typing => "typing",
            Scene::RemoteOutput => "remote-output",
            Scene::Connecting => "connecting",
            Scene::UiOnly => "ui-only",
            Scene::Idle => "idle",
        }
    }
}

pub fn scene_of(s: &Snapshot) -> Scene {
    if s.xfer_jobs > 0 {
        return Scene::SftpTransfer;
    }
    if s.scroll_events > 0 {
        return Scene::Scrollback;
    }
    if total(&s.stage_us[crate::diag::Stage::Resize as usize]) > 0 {
        return Scene::Resize;
    }
    if s.keys > 0 {
        return Scene::Typing;
    }
    // 速率按毫秒换算,窗口为 0 时当 0 处理(不除零)。
    let bps = if s.window_ms == 0 {
        0
    } else {
        s.inbound_bytes.saturating_mul(1000) / s.window_ms
    };
    if bps >= REMOTE_OUTPUT_BPS {
        return Scene::RemoteOutput;
    }
    if s.connects_ok + s.connects_err + s.reconnects > 0 {
        return Scene::Connecting;
    }
    if s.frames > 0 {
        return Scene::UiOnly;
    }
    Scene::Idle
}
```

- [ ] **Step 4: 跑通 + fmt + commit + 变异验证**（对调前两个 if → 红 → 恢复）

```bash
git add -A && git commit -m "feat(app): scene_of 场景标签纯函数,八档优先级 + 涓流阈值 (F167)"
```

---

### Task 6: 线程分组纯函数

**Files:**
- Modify: `crates/mullion-app/src/profile.rs`

- [ ] **Step 1: 失败测试**

```rust
    /// F168:分组表 + 三个坑:前缀串号 / 空名 / 未匹配进其他但 Debug 可见。
    ///
    /// 自证会变红:把 prefix_matches 改成裸 starts_with(串号那条),或把
    /// 空名分支删掉(unnamed 那条),或把 thread_group_pct 加 .min(100)
    /// (超 100% 那条 —— 组内多线程烧多核是常态,封顶就看不见了)。
    #[test]
    fn thread_grouping_boundaries_and_the_uncapped_pct() {
        let threads = vec![
            ("tokio-runtime-worker".to_string(), 150u32),
            ("tokio-runtime-worker".to_string(), 90u32),
            ("mullion-watchdog".to_string(), 1u32),
            ("mullion-watchdog2".to_string(), 7u32), // 串号陷阱:不是 watchdog
            ("".to_string(), 3u32),                  // 空名(Windows 未命名线程)
            ("wgpu-poll".to_string(), 5u32),
        ];
        let g = group_threads(&threads);
        let get = |name: &str| g.groups.iter().find(|(n, _)| *n == name).unwrap().1;
        assert_eq!(get("tokio"), 240, "同组求和,且允许超 100%");
        assert_eq!(get("watchdog"), 1, "watchdog2 不许被前缀串进来");
        assert_eq!(get("其他"), 7 + 3 + 5);
        let unmapped: Vec<&str> = g.unmapped.iter().map(|(n, _)| n.as_str()).collect();
        assert!(unmapped.contains(&"mullion-watchdog2"));
        assert!(unmapped.contains(&"unnamed"), "空名要有占位标识");
        assert!(unmapped.contains(&"wgpu-poll"));
        // 不封顶换算:5 秒窗口烧了 12 秒 CPU(多线程组)= 240%。
        assert_eq!(thread_group_pct(12_000_000_000, 5_000_000_000), Some(240));
        assert_eq!(thread_group_pct(1, 0), None, "窗口为 0 = 采不到,不是 0%");
    }
```

- [ ] **Step 2: 确认失败** → 编译错误
- [ ] **Step 3: 实现**

```rust
/// F168:线程 CPU 百分比,不归一(100 = 一个核)**且不封顶**(组内多线程
/// 烧多核是常态,封顶等于把最该看见的读数削掉)。这就是它不复用
/// [`crate::sysprobe::cpu_pct`] 的原因 —— 那个按口径约定 `.min(100)`。
pub fn thread_group_pct(delta_ns: u64, window_ns: u64) -> Option<u32> {
    if window_ns == 0 {
        return None;
    }
    Some(((delta_ns as u128) * 100 / (window_ns as u128)) as u32)
}

/// F168:前缀 → 组名。顺序即输出顺序。main 不在表里(由 F164 的主线程
/// 口径另源,采样层按 tid 排除)。
const THREAD_GROUPS: &[(&str, &str)] = &[
    ("tokio-runtime-worker", "tokio"),
    ("mullion-watchdog", "watchdog"),
    ("mullion-file-dialog", "dialog"),
    ("mullion-dragout", "dragout"),
];

/// 前缀 + 边界:`mullion-watchdog` 不匹配 `mullion-watchdog2`。
/// (与 F165 PDH 的 `pid_1234` vs `pid_12345` 同族陷阱。)
fn prefix_matches(name: &str, prefix: &str) -> bool {
    match name.strip_prefix(prefix) {
        None => false,
        Some(rest) => !rest.starts_with(|c: char| c.is_ascii_alphanumeric()),
    }
}

pub struct ThreadGroups {
    /// 固定顺序:表内各组 + 末尾「其他」。0 也在列 —— watchdog:0% 是信息。
    pub groups: Vec<(&'static str, u32)>,
    /// 落进「其他」的原名(空名记作 `unnamed`),Debug 档打出来。
    pub unmapped: Vec<(String, u32)>,
}

pub fn group_threads(threads: &[(String, u32)]) -> ThreadGroups {
    let mut sums = vec![0u32; THREAD_GROUPS.len()];
    let mut other = 0u32;
    let mut unmapped = Vec::new();
    for (name, pct) in threads {
        match THREAD_GROUPS
            .iter()
            .position(|(p, _)| prefix_matches(name, p))
        {
            Some(i) => sums[i] = sums[i].saturating_add(*pct),
            None => {
                other = other.saturating_add(*pct);
                let shown = if name.is_empty() { "unnamed" } else { name };
                unmapped.push((shown.to_string(), *pct));
            }
        }
    }
    let mut groups: Vec<(&'static str, u32)> = THREAD_GROUPS
        .iter()
        .zip(sums)
        .map(|((_, g), v)| (*g, v))
        .collect();
    groups.push(("其他", other));
    ThreadGroups { groups, unmapped }
}
```

（注意 `strip_prefix("")` 对空前缀恒 Some——表里没有空前缀，无此问题。空名 `""` 对任何前缀 `strip_prefix` 都是 None → 进其他，正确。）

- [ ] **Step 4: 跑通 + fmt + commit + 变异验证**（`prefix_matches` 改裸 `starts_with` → 红 → 恢复）

```bash
git add -A && git commit -m "feat(app): 线程分组纯函数 —— 前缀带边界/空名占位/不封顶百分比 (F168)"
```

---

### Task 7: `sysprobe::ThreadCpuProbe`（Linux 实现 + Windows 实现）

**Files:**
- Modify: `crates/mullion-app/src/sysprobe.rs`

**背景（给零上下文工程师）**：文件里已有 `CpuProbe`（进程+主线程两口径），模式是「主线程上 `new`、看门狗线程上 `sample`、差分算 delta」。`ThreadCpuProbe` 照同一模式，但按 tid 记上一窗口的每线程累计值。Linux 的换算方式**照抄同文件 `CpuProbe` 的 Linux 分支**（utime/stime 是 ticks，怎么乘到 ns 那里已经写对了——`_SC_CLK_TCK` 或写死 100 以现状为准，别自己发明）。

- [ ] **Step 1: 失败测试**（Linux 分支在本机可真测）

```rust
    /// F168:线程枚举必须真的按线程分账。在一条命名线程里烧 CPU,
    /// 采样结果里该线程的 delta 必须显著大于零,且**主线程不在清单里**。
    ///
    /// 自证会变红:把 sample 里「跳过 main_tid」的判断删掉(主线程混入),
    /// 或把 delta 计算的新旧值弄反(全是 0)。
    #[cfg(not(windows))]
    #[test]
    fn a_burning_named_thread_shows_up_with_its_name_and_main_is_excluded() {
        let mut probe = ThreadCpuProbe::new(linux_current_tid());
        let _ = probe.sample(); // 建基线
        let t = std::thread::Builder::new()
            .name("mullion-burner".into())
            .spawn(|| {
                let start = std::time::Instant::now();
                let mut x = 0u64;
                while start.elapsed() < std::time::Duration::from_millis(300) {
                    x = x.wrapping_mul(31).wrapping_add(7);
                }
                std::hint::black_box(x)
            })
            .unwrap();
        let _ = t.join();
        let s = probe.sample().expect("Linux 上枚举不该失败");
        let burner: u64 = s
            .iter()
            .filter(|(n, _)| n == "mullion-burner")
            .map(|(_, d)| *d)
            .sum();
        assert!(
            burner > 100_000_000,
            "烧了 300ms,记到的却只有 {burner}ns"
        );
        assert!(
            !s.iter().any(|(n, _)| n == "已退出线程还在清单里"),
            "(占位断言,保留)"
        );
    }
```

（**已退出线程**：burner join 完后 `/proc/self/task/<tid>` 目录消失，它的 CPU 时间在**第二次 sample** 里还能读到吗？读不到——这是采样法的固有缺陷：两次采样之间生灭的线程会漏账。测试里 burner 在 join 前后各留 delta 的问题：上面测试 burner 在 sample 时已退出，读不到就会 fail。**修正测试**：把 `probe.sample()` 移到 `join` 之前、烧的循环结束之后——用一个 channel 让 burner 烧完后阻塞等待，主测试线程先 sample 再放行 join。实现者按此意图写，断言不变。）

- [ ] **Step 2: 确认失败** → 编译错误
- [ ] **Step 3: 实现骨架**

```rust
/// F168:每线程 CPU 时间探针。有状态(差分),看门狗线程持有。
///
/// 固有缺陷(如实写在这,别试图修):两次采样之间生灭的短命线程漏账。
/// 5 秒窗口 + 本项目线程全是长命线程,可接受;真要精确得 hook 线程生命周期,
/// 超出日志的本分。
pub struct ThreadCpuProbe {
    /// tid → 上一窗口的累计 CPU ns。
    prev: std::collections::HashMap<u32, u64>,
    main_tid: u32,
}

impl ThreadCpuProbe {
    /// `main_tid` 必须是主线程的 —— 在主线程上调 [`linux_current_tid`]
    /// (Linux)或 `GetCurrentThreadId`(Windows)取好传进来(同 T12 的教训:
    /// 谁调谁的语义,存下来跨线程用就错了)。
    pub fn new(main_tid: u32) -> Self { /* prev: HashMap::new() */ }

    /// 枚举全部线程,返回 (线程名, 这一窗口的 CPU ns 增量),**排除主线程**
    /// (F164 已有更准的主线程口径)。首次调用建基线返回 Some(空表)。
    /// 平台枚举失败返回 None(渲染层显示 n/a,不冒充 0)。
    pub fn sample(&mut self) -> Option<Vec<(String, u64)>> { /* 两套 #[cfg] */ }
}
```

Linux 分支要点（可测）：`std::fs::read_dir("/proc/self/task")` → 每个 tid：`comm` 文件读名（trim 换行）、`stat` 文件按**最后一个 `)` 之后**切字段取 utime/stime（索引 11/12，F164 已建立的手法，照抄它的换算）→ ticks → ns。新 tid（不在 prev 里）delta 按全额算。**采样后 `self.prev` 整表替换成本次的**（不是增量 merge——退出线程的旧 tid 留着会让 HashMap 无限涨）。

Windows 分支要点（本机不编译，Task 14 交叉编译验证）：`CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0)` → `Thread32First/Next` 过滤 `th32OwnerProcessID == GetCurrentProcessId()` → `OpenThread(THREAD_QUERY_LIMITED_INFORMATION, FALSE, tid)` → `GetThreadTimes`（FILETIME 100ns 单位 ×100 = ns）+ `GetThreadDescription`（返回 PWSTR，`CoTaskMemFree`/`LocalFree` 按文档释放——**windows-sys 与 windows crate 的释放函数不同，写之前查本仓库 Cargo.lock 里实际用的是哪个 crate 的哪个版本**，`sysprobe.rs` 头部 import 能看到）→ `CloseHandle`。快照句柄最后 `CloseHandle`。空名（PWSTR 空串）原样返回 `String::new()`（分组层管占位）。

- [ ] **Step 4: 跑通**（Linux 分支真跑）`cargo test -p mullion-app a_burning_named_thread -- --nocapture`
- [ ] **Step 5: fmt + commit + 变异验证**（删「跳过 main_tid」→ 红 → 恢复）

```bash
git add -A && git commit -m "feat(app): ThreadCpuProbe 每线程 CPU 采集,两套平台后端 (F168)"
```

---

### Task 8: 看门狗接线线程采样

**Files:**
- Modify: `crates/mullion-app/src/diag.rs`（`start_watchdog` 600 行附近建探针；`watchdog_loop` 的采样点 650~668 行）

- [ ] **Step 1: `start_watchdog` 里建**（与 `CpuProbe::new_on_main_thread()` 同段——那里的注释解释了为什么必须在主线程建，`ThreadCpuProbe` 的 main_tid 同理）

```rust
    #[cfg(windows)]
    let main_tid = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
    #[cfg(not(windows))]
    let main_tid = crate::sysprobe::linux_current_tid();
    let mut threads = crate::sysprobe::ThreadCpuProbe::new(main_tid);
```

（`GetCurrentThreadId` 的实际路径按 `sysprobe.rs` 用的 crate 对齐；若 `linux_current_tid` 目前是私有 `fn` 就改 `pub(crate)`。探针 move 进 `watchdog_loop` 闭包——`spawn(move || watchdog_loop(stall_ms, cpu, gpu, threads))`。）

- [ ] **Step 2: 采样点接线**（`take_snapshot` 之后、渲染之前，与 cpu/gpu 采样同段）

```rust
            match threads.sample() {
                Some(list) => {
                    snap.thread_available = true;
                    let window_ns = window_ms.saturating_mul(1_000_000);
                    let pcts: Vec<(String, u32)> = list
                        .into_iter()
                        .filter_map(|(name, delta)| {
                            crate::profile::thread_group_pct(delta, window_ns)
                                .map(|p| (name, p))
                        })
                        .collect();
                    let g = crate::profile::group_threads(&pcts);
                    snap.thread_groups = g.groups;
                    snap.thread_unmapped = g.unmapped;
                }
                None => snap.thread_available = false,
            }
```

- [ ] **Step 3: 全绿**（这段没有独立单测——决策逻辑已在 Task 6/7 测过，这里是纯接线；渲染层 Task 10 的测试会覆盖 `thread_available=false → n/a` 的语义）
- [ ] **Step 4: fmt + commit**

```bash
git add -A && git commit -m "feat(app): 看门狗接入线程 CPU 采样,分组结果进快照 (F168)"
```

---### Task 9: 内存行纯函数（分块 + 显式余量）

**Files:**
- Modify: `crates/mullion-app/src/profile.rs`

- [ ] **Step 1: 失败测试**

```rust
    /// F169:余量三态 —— 正常 / 全零 / 负余量报超出而不是负数。
    ///
    /// 自证会变红:把负余量分支改成静默夹 0(`saturating_sub` 一把梭),
    /// 「超出」字样那条断言会抓住。
    #[test]
    fn mem_parts_reports_the_remainder_honestly() {
        // 正常:340 = 128 + 0 + 16 + 196。
        assert_eq!(
            mem_parts(340, 128 << 20, 0, 16 << 20),
            "340MB = scroll:128 xfer:0 text:16 其他:196"
        );
        // 全零记账:全进其他。
        assert_eq!(mem_parts(50, 0, 0, 0), "50MB = scroll:0 xfer:0 text:0 其他:50");
        // 负余量:记账 168MB > RSS 100MB,超出 68 要显式打出来。
        assert_eq!(
            mem_parts(100, 128 << 20, 24 << 20, 16 << 20),
            "100MB = scroll:128 xfer:24 text:16 其他:0(记账超出RSS 68MB)"
        );
    }
```

- [ ] **Step 2: 确认失败** → 编译错误
- [ ] **Step 3: 实现**

```rust
/// F169:`profile.mem` 的正文。块单位 MB(向下取整,小块显示 0 是如实)。
///
/// 余量为负时**显式报超出量**:静默夹 0 会让「记账模型错了」永远不被
/// 发现(spec §5)。
pub fn mem_parts(process_mb: u64, scroll_b: u64, xfer_b: u64, text_b: u64) -> String {
    let scroll = scroll_b >> 20;
    let xfer = xfer_b >> 20;
    let text = text_b >> 20;
    let accounted = scroll + xfer + text;
    if accounted <= process_mb {
        format!(
            "{}MB = scroll:{} xfer:{} text:{} 其他:{}",
            process_mb,
            scroll,
            xfer,
            text,
            process_mb - accounted
        )
    } else {
        format!(
            "{}MB = scroll:{} xfer:{} text:{} 其他:0(记账超出RSS {}MB)",
            process_mb,
            scroll,
            xfer,
            text,
            accounted - process_mb
        )
    }
}
```

（调用方（Task 10）把 xfer 块算成 `xfer_running × XFER_CHUNK`——在途缓冲，不是未传字节；`XFER_CHUNK` 在 app.rs，profile 不 import 它，由 render_lines 内联 `s.xfer_running * 64 * 1024`?——**不行，两处硬编码会漂**。把常量挪到 profile.rs：`pub const XFER_CHUNK: u64 = 64 * 1024;`，app.rs 引用 `crate::profile::XFER_CHUNK as usize`。Task 4 若已在 app.rs 定义，此处**挪走**并改引用——一个常量只许有一个家。）

- [ ] **Step 4: 跑通 + fmt + commit + 变异验证**

```bash
git add -A && git commit -m "feat(app): mem_parts 内存分块渲染,负余量显式报超出 (F169)"
```

---

### Task 10: `render_lines` 多行改造

**Files:**
- Modify: `crates/mullion-app/src/profile.rs`（`render_line` 399~521 行整体改造；既有引用它的测试全部跟着改）

**这是本切片最大的一个任务。** 目标输出（spec §2，行内容以下面代码为准）：

```
profile      5.0s frame=… (现有全部段,去掉 mem=/cpu=/gpu=/vram=/gpu_us= 五段)
profile.load scene=… tabs=… panes=… hosts=… scroll=…行 xfer=…个/…剩 key=…x in=…
profile.cpu  total=… main=… | tokio:…% watchdog:…% dialog:…% dragout:…% 其他:…%
profile.mem  (mem_parts 的输出)
profile.gpu  util=… vram=… frame=… | term:… egui:…
(debug 时追加) profile.cpu.unmapped … / profile.mem.delta …
```

- [ ] **Step 1: 失败测试**（先写新契约的测试；既有 render_line 测试这一步**先不动**）

```rust
    /// F167:多行契约 —— 空闲零行/前缀/五段移出概览/无内嵌换行/debug 行开关。
    ///
    /// 自证会变红:概览行忘删 `mem=` 段(移出那条),或 render_lines 用
    /// `\n`.join 拼成单串(无换行那条),或 debug 行忘了 gate(开关那条)。
    #[test]
    fn render_lines_contract() {
        assert!(render_lines(&Snapshot::empty(), false).is_empty(), "空闲必须零行");
        let mut s = busy_snapshot();
        s.thread_available = true;
        s.thread_groups = vec![("tokio", 31), ("其他", 9)];
        s.thread_unmapped = vec![("wgpu-poll".to_string(), 5)];
        let lines = render_lines(&s, false);
        assert!(lines.len() >= 5, "概览+load+cpu+mem+gpu 至少五行");
        for l in &lines {
            assert!(!l.contains('\n'), "多行 = 多条独立记录,单行内禁止换行: {l}");
        }
        let overview = &lines[0];
        assert!(overview.starts_with("profile "), "概览行前缀");
        for gone in ["mem=", "cpu=", "gpu=", "vram=", "gpu_us="] {
            assert!(!overview.contains(gone), "{gone} 该移去专属行了");
        }
        assert!(lines.iter().any(|l| l.starts_with("profile.load scene=")));
        assert!(lines.iter().any(|l| l.starts_with("profile.cpu ") && l.contains("tokio:31%")));
        assert!(lines.iter().any(|l| l.starts_with("profile.mem ")));
        assert!(lines.iter().any(|l| l.starts_with("profile.gpu ")));
        assert!(
            !lines.iter().any(|l| l.starts_with("profile.cpu.unmapped")),
            "info 档不出 unmapped"
        );
        let dbg = render_lines(&s, true);
        assert!(
            dbg.iter().any(|l| l.starts_with("profile.cpu.unmapped") && l.contains("wgpu-poll:5%")),
            "debug 档要能看见没进分组表的线程"
        );
    }

    /// F168:采不到线程 ≠ 各组为 0。
    /// 自证会变红:把 cpu 行渲染里 thread_available 的分支删掉。
    #[test]
    fn an_unavailable_thread_probe_renders_na_not_zeros() {
        let mut s = busy_snapshot();
        s.thread_available = false;
        let lines = render_lines(&s, false);
        let cpu = lines.iter().find(|l| l.starts_with("profile.cpu ")).unwrap();
        assert!(cpu.ends_with("| n/a"), "采不到必须是 n/a: {cpu}");
        assert!(!cpu.contains("tokio:"));
    }
```

- [ ] **Step 2: 确认失败** → 编译错误
- [ ] **Step 3: 实现**。把 `render_line` 改名 `render_lines(s: &Snapshot, debug: bool) -> Vec<String>`：

1. 空闲判断不动：`if s.is_idle() { return Vec::new(); }`
2. **概览行**：现有 format 串去掉尾部 `mem={}MB cpu={} gpu={} vram={} gpu_us={}` 五段及对应实参（`tabs= panes= hosts=` **保留**，spec 规则）。
3. **load 行**：

```rust
    let scroll_disp = if s.scroll_lines >= 1000 {
        format!("{:.1}k行", s.scroll_lines as f64 / 1000.0)
    } else {
        format!("{}行", s.scroll_lines)
    };
    lines.push(format!(
        "profile.load scene={} tabs={} panes={} hosts={} scroll={} xfer={}个/{}MB剩 key={}x in={}",
        scene_of(s).label(),
        s.tabs, s.panes, s.hosts,
        scroll_disp,
        s.xfer_jobs,
        s.xfer_bytes_left >> 20,
        s.keys,
        rate, // 概览行算 rate 的那个变量,提出来共用
    ));
```

4. **cpu 行**：

```rust
    let groups = if !s.thread_available {
        "n/a".to_string()
    } else {
        s.thread_groups
            .iter()
            .map(|(n, p)| format!("{n}:{p}%"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    lines.push(format!(
        "profile.cpu total={} main={} | {}",
        fmt_pct(s.cpu_pct),
        fmt_pct(s.main_cpu_pct),
        groups
    ));
```

5. **mem 行**：`lines.push(format!("profile.mem {}", mem_parts(s.mem_process_mb, s.mem_scroll_bytes, s.xfer_running * XFER_CHUNK, s.mem_text_bytes)));`
6. **gpu 行**：`util=`（现有 `fmt_engines`）、`vram=`（现有映射）、`frame=`（现有 `gpu_us_part` 逻辑，改名）、分层段：

```rust
    let split = if !s.gpu_split_supported {
        "分层:n/a".to_string()
    } else if total(&s.gpu_term_us) == 0 {
        // 支持但这窗口没采到样(抽样忙/纯空闲),与「不支持」长得不一样。
        "分层:0x".to_string()
    } else {
        format!(
            "term:{} egui:{}",
            fmt_us(quantile_us(&s.gpu_term_us, 0.5)),
            fmt_us(quantile_us(&s.gpu_egui_us, 0.5))
        )
    };
```

7. **debug 行**（`if debug { ... }`）：
   - `profile.cpu.unmapped a:5% b:3%`（`thread_unmapped` 非空时才 push）
   - `profile.mem.delta rss={}B scroll={}B xfer={}B text={}B`（原始字节，排查记账模型用）
   - （spec §7 的 `profile.mem.panes` / `profile.mem.xfer` 逐项明细：**数据层这版不带 per-pane 明细**——gauge 只有总量。降档处理：这两行推迟到真实长跑发现「其他」占比异常需要下钻时再加，本任务在 spec §7 表格上加一句「panes/xfer 明细:数据层未接,延后」的备注并在提交信息里写明。**不许静默跳过**。）
8. **既有测试迁移**：所有 `render_line(&s)` 的调用改 `render_lines(&s, false)`，取行的断言改成在 `Vec` 里找对应前缀的行。涉及五段的断言（`cpu=`/`gpu=`/`vram=`/`gpu_us=`/`mem=`）改到对应的新行上——**断言本身一条都不许删**（那是在削弱测试）。
9. `render_line` 函数头上那段「单行是硬要求」的注释改写成「每条记录单行」（理由不变：按行 grep）。

- [ ] **Step 4: 全绿**（重点确认迁移后的既有测试一条没少：`grep -c "fn .*render" src/profile.rs` 前后对比测试数量）
- [ ] **Step 5: fmt + commit + 变异验证**（概览行故意留下 `mem=` 段 → 红 → 恢复）

```bash
git add -A && git commit -m "feat(app): profile 剖面拆成多行 —— load/cpu/mem/gpu 各归其位 (F167~F170)

spec §7 的 profile.mem.panes / profile.mem.xfer 逐项明细延后:
数据层只有总量 gauge,下钻明细等真实长跑出现异常占比再接。"
```

---

### Task 11: diag 写盘处改多条记录

**Files:**
- Modify: `crates/mullion-app/src/diag.rs`（watchdog_loop 664~668 行）

- [ ] **Step 1: 改写**

```rust
            if log::log_enabled!(target: "mullion", log::Level::Info) {
                let debug = log::log_enabled!(target: "mullion", log::Level::Debug);
                // 每行独立 log 一次:各自带时间戳/pid 前缀(F166),
                // 单条记录嵌 \n 会让续行 grep 不到时间 —— spec §2 明令禁止。
                for line in crate::profile::render_lines(&snap, debug) {
                    log::info!(target: "mullion", "{line}");
                }
            }
```

- [ ] **Step 2: 全绿 + fmt + commit**（渲染契约已在 Task 10 测过；这里是接线，靠编译器保证 `render_line` 旧名已无人引用）

```bash
git add -A && git commit -m "feat(app): 剖面写盘改多条独立记录,每行自带时间戳与 pid (F167)"
```

---

### Task 12: GPU 分层（INSIDE_PASSES + 3 槽 + term/egui 解算）

**Files:**
- Modify: `crates/mullion-app/src/gpu.rs`（feature 申请 468~496 行；`GpuTimer` 342~431 行）
- Modify: `crates/mullion-app/src/app.rs`（render pass 11001~11030 行）
- Modify: `crates/mullion-app/src/diag.rs`（Task 3 已留了 `record_gpu_split_us` / `set_gpu_split_supported` 的壳，这里补直方图实现——照 `GPU_FRAME_US` 的现状抄）

**注意（spec §6 + wgpu 合约）**：`TIMESTAMP_QUERY_INSIDE_PASSES` implies `TIMESTAMP_QUERY`（wgpu-types 23.0.0 `lib.rs:532`），所以申请逻辑是「两者都在 adapter features 里才申请 INSIDE_PASSES」。**不动单 pass 结构**——`forget_lifetime` 那段注释解释了为什么，槽 1 必须写在它之前。

- [ ] **Step 1: `Gpu::new` 的 feature 申请**（替换现有 `has_ts` 段）

```rust
        let has_ts = adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY);
        // F170:INSIDE_PASSES 按 wgpu 合约蕴含 TIMESTAMP_QUERY;两者都在才申请,
        // 单独出现按都不支持处理(spec §6 降级矩阵第三行,防御性)。
        let has_split = has_ts
            && adapter
                .features()
                .contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES);
        if !has_ts {
            log::info!(target: "mullion", "adapter 不支持 TIMESTAMP_QUERY,GPU 帧耗时降级为 n/a");
        } else if !has_split {
            log::info!(target: "mullion", "adapter 不支持 INSIDE_PASSES,GPU 分层降级为 n/a");
        }
        crate::diag::set_gpu_split_supported(has_split);
```

`required_features`：

```rust
                    required_features: if has_split {
                        wgpu::Features::TIMESTAMP_QUERY
                            | wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES
                    } else if has_ts {
                        wgpu::Features::TIMESTAMP_QUERY
                    } else {
                        wgpu::Features::empty()
                    },
```

构造：`let gpu_timer = has_ts.then(|| GpuTimer::new(&device, &queue, has_split));`

- [ ] **Step 2: `GpuTimer` 扩槽**。加字段 `split: bool`；`new(device, queue, split)` 里：

```rust
        let slots: u32 = if split { 3 } else { 2 };
        // count: slots;  size: slots as u64 * 8(resolve 与 staging 都按它)
```

`writes()` 的 `end_of_pass_write_index`：`Some(if self.split { 2 } else { 1 })`。
`resolve()`：`enc.resolve_query_set(&self.set, 0..self.slots, &self.resolve, 0);`（`slots` 存成字段）。
新增 pass 内插点：

```rust
    /// F170:终端趟结束的分界点。**必须在 `forget_lifetime` 之前**调
    /// (之后原 pass 就没了);只在本帧真的挂了时间戳且支持分层时调。
    pub fn mid_mark(&self, pass: &mut wgpu::RenderPass<'_>) {
        if self.split {
            pass.write_timestamp(&self.set, 1);
        }
    }
```

`read_back()` 回调解算改为：

```rust
                    // split:t0=开始 t1=终端趟完 t2=pass 完;否则 t0/t1 首尾。
                    if split {
                        let t0 = read_u64(&view, 0);
                        let t1 = read_u64(&view, 1);
                        let t2 = read_u64(&view, 2);
                        let to_us = |ticks: u64| (ticks as f64 * period as f64 / 1000.0) as u64;
                        crate::diag::record_gpu_frame_us(to_us(t2.saturating_sub(t0)));
                        crate::diag::record_gpu_split_us(
                            to_us(t1.saturating_sub(t0)),
                            to_us(t2.saturating_sub(t1)),
                        );
                    } else {
                        // 现状逻辑原样保留(两槽)。
                    }
```

（`split` 与 `slots` 要 clone/copy 进 `'static` 回调——都是 `Copy`，跟 `period` 一样直接 move。`read_u64` 是个小 helper：`u64::from_le_bytes(view[i*8..i*8+8].try_into().unwrap_or_default())`。）

- [ ] **Step 3: app.rs 接线**（pass 里、`forget_lifetime` **之前**、终端趟之后）

```rust
        // F170:终端趟/egui 趟的分界时间戳。必须在 forget_lifetime 之前。
        if sampling {
            if let Some(t) = a.gpu.gpu_timer.as_ref() {
                t.mid_mark(&mut pass);
            }
        }
        let mut static_pass = pass.forget_lifetime();
```

- [ ] **Step 4: 守护测试**（GPU 胶水无法单测——真值验证在 Task 14 实机。能测的是源码切片：分界点位置）

```rust
    /// F170:mid_mark 必须夹在终端趟之后、forget_lifetime 之前 —— 放错位置
    /// 分层就成了「全帧/0」,数字看着还挺合理,只有源码顺序能守。
    ///
    /// 自证会变红:把 mid_mark 那段挪到 forget_lifetime 之后。
    #[test]
    fn the_gpu_mid_mark_sits_between_terminal_and_egui() {
        let src = include_str!("app.rs");
        let mid = src.find("t.mid_mark(&mut pass);").expect("分界点在");
        let forget = src.find("let mut static_pass = pass.forget_lifetime();").expect("forget 在");
        let term_draw = src.find("a.text.render(&mut pass)").expect("终端文字趟在");
        assert!(term_draw < mid && mid < forget, "顺序:终端趟 < mid_mark < forget_lifetime");
    }
```

- [ ] **Step 5: 全绿 + fmt + commit + 变异验证**（把 mid_mark 段挪到 forget 之后 → 红 → 恢复）

```bash
git add -A && git commit -m "feat(app): GPU 帧耗时分层 term/egui,INSIDE_PASSES 条件申请 3 槽 (F170)"
```

---

### Task 13: spec.md 编号 + 文档收尾 + 全量绿

**Files:**
- Modify: `spec.md`（F166 行之后追加）
- Modify: `docs/gui-render-gotchas.md`（若 Task 12 实现中踩到新坑才加；没有就不动——别为凑数写条目）
- Modify: `docs/superpowers/specs/2026-08-26-resource-attribution-design.md`（§7 补 panes/xfer 明细延后的备注，Task 10 已定）

- [ ] **Step 1: spec.md 追加**（格式照 F164~F166 行的现状）

```
| F167 | 剖面场景标签:profile 拆多行,load 行给出 scene= 与分母(tabs/panes/scroll/xfer) | P2 | ✅ |
| F168 | CPU 按线程分组:tokio/watchdog/dialog/dragout/其他,Debug 档列未映射线程名 | P2 | ✅ |
| F169 | 内存分块记账:scroll/xfer/text + 显式余量,负余量报超出 | P2 | ✅ |
| F170 | GPU 帧耗时分层:term/egui 两段,INSIDE_PASSES 条件申请 | P2 | ✅ |
```

（`✅` 列的实际符号/措辞照 spec.md 里 F164 行现状抄，别发明新格式。）

- [ ] **Step 2: 全量绿**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 3: commit**

```bash
git add -A && git commit -m "docs(spec): F167~F170 资源用量归因四条入表"
```

---

### Task 14: 发版一条龙（v0.1.72）

按 `.claude/skills/release-windows/SKILL.md` 全流程执行（说「发版」时自动加载；**别凭记忆做**）。本切片特有的验收点：

- [ ] 交叉编译后 **objdump 常规验收**之外，确认 Windows 分支真的编进去了：`ThreadCpuProbe` 的 Toolhelp 导入应出现（`objdump -p target/x86_64-pc-windows-gnu/release/mullion.exe | grep -i "Thread32\|GetThreadDescription"`——搜不到 = Windows 分支被 cfg 掉了没编译，**Linux 全绿不代表它存在**）
- [ ] notes.md 的人工验收清单要含（无头环境验不了的）：
  1. `profile.cpu` 各组数字与 Process Explorer 线程视图同量级；传输大文件时 `tokio:` 明显上抬且 `profile.load` 的 `xfer=` 同步非零
  2. `profile.mem` 的 `其他` 占比在长跑下是否常年 >60%（是 → 记账选错块，模型重做，见 spec §5）
  3. `profile.gpu` 的 `term:`/`egui:` 两段在真实驱动上有数、加和 ≈ `frame=` 的 p50；核显/老驱动机器上是 `分层:n/a` 而不是崩
  4. 滚屏时 `scene=scrollback`、传输时 `scene=sftp-transfer`——场景标签对得上人的直觉
  5. 多行化后 5 秒一组 ~5 行,确认日志体积增速可接受（影响 F166 的 64MB 轮转频率）
  6. 照例：不闪 / CJK 对齐 / 输入法 / 手感无回退（本版动了渲染热路径的时间戳插点）

---

## Self-Review 记录（写完计划后跑过一遍）

- **Spec 覆盖**：§2 多行（Task 10/11）；§3 场景（Task 3/4/5）；§4 线程（Task 6/7/8）；§5 内存（Task 1/4/9/10）；§6 GPU（Task 12）；§7 Debug 档（Task 10，其中 panes/xfer 明细显式降档并回写 spec，Task 13 收口）；§8 落点与 §9 测试逐条对应。
- **已知偏离 spec 一处**：§7 的 `profile.mem.panes`/`profile.mem.xfer` 逐项明细延后（数据层只有总量 gauge；per-pane 明细要每帧构 Vec，为 Debug 档功能往热路径加分配不划算）。Task 10 提交信息与 Task 13 回写 spec 双重留痕，不静默。
- **类型一致性**：`render_lines(&Snapshot, bool) -> Vec<String>`（Task 10/11）；`ThreadCpuProbe::sample() -> Option<Vec<(String, u64)>>`（Task 7/8）；`group_threads(&[(String, u32)]) -> ThreadGroups`（Task 6/8）；`mem_parts(u64,u64,u64,u64) -> String`（Task 9/10）；`XFER_CHUNK` 唯一定义在 profile.rs（Task 9 明确从 Task 4 挪走）；`GpuTimer::new(&Device,&Queue,bool)` + `mid_mark(&mut RenderPass)`（Task 12）。
- **占位符扫描**：Task 3 的 `record_gpu_split_us` 壳在 Task 12 补实现（前向声明，两处都有代码）；Task 7 Windows 分支给的是要点清单而非成品代码——这是有意的：windows crate 的具体绑定路径要照 `sysprobe.rs` 现状对齐（计划里写了核对方法），抄错版本比留白更糟。
