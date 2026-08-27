# F176/F177 内存口径收口与 N5 预算闸 —— 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 `profile.mem` 报的数与任务管理器对得上（F176），并在进程越过 N5 预算时自己在日志里喊出来（F177）。

**Architecture:** 采样层（`diag.rs`）多取一个 `PrivateWorkingSetSize` 并把「主数量的是什么」下沉成 `MemKind`，使渲染层（`profile.rs`）**不带 `#[cfg]`**、Windows 形态能在 Linux 开发机上直接单测。预算闸是一对纯函数（判定 `budget_verdict` + 渲染 `over_budget_line`/`recovered_line`），由看门狗线程持有 `reported_mb` 状态，**调用点在 Info 日志门之外**，以便 warn 档也能响、且穿透空闲门。

**Tech Stack:** Rust，`windows-sys 0.59`（`Win32_System_ProcessStatus`，已开，不新增 feature），`log`。零新依赖、零新 crate feature。

**设计来源：** `docs/superpowers/specs/2026-08-27-f176-f177-mem-gauge-and-budget-alarm-design.md`

---

## 文件结构

| 文件 | 职责 | 本计划里的改动 |
|---|---|---|
| `crates/mullion-app/src/diag.rs` | 采样 + 看门狗驱动 | 新增 `MemKind`；`MemSample` 加两字段；`sample_memory` 三个平台分支；`take_snapshot` 填两个新字段；`watchdog_loop` 接预算闸 |
| `crates/mullion-app/src/profile.rs` | **纯渲染 + 纯判定**，零 IO | `Snapshot` 加两字段；`mem_parts` 换签名；新增 `mem_accounted_mb` / `Snapshot::mem_other_mb` / 三个常量 / `BudgetVerdict` / `budget_verdict` / `over_budget_line` / `recovered_line` |
| `spec.md` | 需求表 | 追加 F176、F177 两行 |

**为什么判定放 `profile.rs` 而不是 `diag.rs`**：`profile.rs` 是本 crate 里「纯函数 + 可单测」的那一半，`diag.rs` 带 `#[cfg(windows)]` 和进程级 static。判定逻辑进 `diag.rs` 就没法在 Linux 上覆盖 Windows 形态。

---

## Task 1：`MemKind` 与 `MemSample` 扩字段（采样层）

**Files:**
- Modify: `crates/mullion-app/src/diag.rs:828-911`

- [ ] **Step 1：加 `MemKind` 与两个新字段**

在 `crates/mullion-app/src/diag.rs` 里，把现有的 `MemSample` 定义（第 828~835 行）替换为：

```rust
/// F176:`MemSample::process_bytes` 那个主数**量的是什么**。
///
/// 这个枚举存在的唯一理由是让渲染层不带 `#[cfg]`:平台差异在采样处就
/// 消化掉,`profile.rs` 只认这个标签,于是 Windows 那种输出能在 Linux
/// 开发机上直接单测(架构不变量:布局/渲染 bug 要能脱离窗口复现)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemKind {
    /// Windows `PrivateUsage` —— **提交量**,含已保留未驻留的页。
    Commit,
    /// Linux `/proc/self/statm` 的常驻集。
    Rss,
}

impl MemKind {
    pub fn label(self) -> &'static str {
        match self {
            MemKind::Commit => "commit",
            MemKind::Rss => "rss",
        }
    }
}

/// 内存快照。判断卡死时是否伴随内存压力(reflow 爆内存 / 泄漏 / 系统整体吃紧)。
#[derive(Debug, Clone, Copy)]
pub struct MemSample {
    /// 本进程主数(Windows PrivateUsage / Linux RSS),口径见 `kind`。
    pub process_bytes: u64,
    /// F176:`process_bytes` 的口径。
    pub kind: MemKind,
    /// F176:**专用**工作集 —— 任务管理器进程页「内存」列的那个数。
    /// `None` = 这台机器采不到(Linux;或 Windows 老系统回落到了 `EX`)。
    ///
    /// **它不参与 `mem_parts` 的减法**:工作集会被系统裁剪(窗口最小化时
    /// 尤其激进),而记账块是 Rust 堆上的 `Vec`、字节数不因页被换出而变小。
    /// 拿它做被减数,用户一最小化就会刷屏「记账超出」—— 把正常的系统行为
    /// 报成记账模型崩了。减法算在 `process_bytes` 上,它才不被裁剪。
    pub ws_bytes: Option<u64>,
    pub sys_avail_bytes: u64,
    pub sys_total_bytes: u64,
}
```

`Display for MemSample`（第 837~848 行）**保持不动**——它服务的是事件循环停滞报警行，那行不需要 ws。

- [ ] **Step 2：改 Windows 分支**

把 `#[cfg(windows)] pub fn sample_memory()`（第 850~883 行）整体替换为：

```rust
#[cfg(windows)]
pub fn sample_memory() -> Option<MemSample> {
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
        PROCESS_MEMORY_COUNTERS_EX2,
    };
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    // SAFETY:三个调用都只写入我们自己栈上的、已按 API 要求填好 cb/dwLength
    // 的结构体。K32GetProcessMemoryInfo 按 cb 判断实际结构体大小,传哪个
    // 尺寸就填到哪。失败一律回落,不 panic。
    unsafe {
        // F176:**先按 EX2 要**。它比 EX 多一个 `PrivateWorkingSetSize`,
        // 那正是任务管理器进程页「内存」列的数。
        //
        // **别用 `EX.WorkingSetSize`**:那是**总**工作集,含共享页(系统 DLL、
        // exe 映像、mmap 进来的字体 —— N5 那轮 VMMap 量到 98.8MB Mapped File)。
        // 拿它对照任务管理器照样对不上,只是差到另一个方向去。
        //
        // EX2 要 Windows 11 / Server 2022;老系统上这次调用会失败,回落 EX
        // (此时 ws 报 n/a)。Windows 11 是本项目一等公民,回落只为兜底。
        let mut ex2 = std::mem::zeroed::<PROCESS_MEMORY_COUNTERS_EX2>();
        ex2.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32;
        let ok2 = K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            std::ptr::addr_of_mut!(ex2).cast::<PROCESS_MEMORY_COUNTERS>(),
            ex2.cb,
        ) != 0;

        let (private_usage, ws_bytes, ok_proc) = if ok2 {
            // **`PrivateWorkingSetSize == 0` 一律当采不到。** 结构体是
            // `zeroed()` 出来的,若某个系统上 EX2 返回成功却没填这个字段,
            // 读到的就是 0;而一个跑着的进程专用工作集不可能真为 0。
            // 这是本项目「采不到不许编成 0」规矩的**反向**用法 —— 这里的 0
            // 不是我们伪造的读数,而是「没被填写」的唯一可辨识痕迹。
            let ws = ex2.PrivateWorkingSetSize as u64;
            (ex2.PrivateUsage as u64, (ws != 0).then_some(ws), true)
        } else {
            let mut ex = std::mem::zeroed::<PROCESS_MEMORY_COUNTERS_EX>();
            ex.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
            let ok = K32GetProcessMemoryInfo(
                GetCurrentProcess(),
                std::ptr::addr_of_mut!(ex).cast::<PROCESS_MEMORY_COUNTERS>(),
                ex.cb,
            ) != 0;
            (ex.PrivateUsage as u64, None, ok)
        };

        let mut ms = std::mem::zeroed::<MEMORYSTATUSEX>();
        ms.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        let ok_sys = GlobalMemoryStatusEx(std::ptr::addr_of_mut!(ms)) != 0;

        if !ok_proc && !ok_sys {
            return None;
        }
        Some(MemSample {
            process_bytes: private_usage,
            kind: MemKind::Commit,
            ws_bytes,
            sys_avail_bytes: ms.ullAvailPhys,
            sys_total_bytes: ms.ullTotalPhys,
        })
    }
}
```

- [ ] **Step 3：改 Linux 与兜底分支**

在 `#[cfg(target_os = "linux")]` 分支（第 885~906 行）的 `Some(MemSample { … })` 里加两个字段：

```rust
    Some(MemSample {
        process_bytes: rss_pages * page,
        kind: MemKind::Rss,
        // Linux 的主数本身就是常驻量,再单开一个 ws 是同义反复。
        ws_bytes: None,
        sys_avail_bytes: kb("MemAvailable:"),
        sys_total_bytes: kb("MemTotal:"),
    })
```

`#[cfg(not(any(windows, target_os = "linux")))]` 分支返回 `None`，**不动**。

- [ ] **Step 4：确认能编过**

Run: `cargo check -p mullion-app 2>&1 | tail -20`
Expected: 只应报 `mem_parts`/`Snapshot` 相关的下游错误（还没改），或者干净通过。`MemSample`/`MemKind` 本身不该有错。

Windows 分支在这台 Linux 机上不编译，**交叉编译时才检得到**：

Run: `cargo check -p mullion-app --target x86_64-pc-windows-gnu 2>&1 | tail -30`
Expected: 无 error（`PROCESS_MEMORY_COUNTERS_EX2` 与 `PrivateWorkingSetSize` 已核实存在于 `windows-sys 0.59.0` 的 `Windows/Win32/System/ProcessStatus/mod.rs:126-139`，属已开的 `Win32_System_ProcessStatus` feature）

- [ ] **Step 5：提交**

```bash
git add crates/mullion-app/src/diag.rs
git commit -m "feat(app): 内存采样带上专用工作集与口径标签 (F176)

EX2.PrivateWorkingSetSize 才是任务管理器那列;EX.WorkingSetSize 是含共享页
的总工作集,拿它对照照样对不上。老系统回落 EX、ws 报 n/a。

MemKind 让平台差异在采样处消化,渲染层不带 cfg —— Windows 形态能在
Linux 开发机上单测。"
```

---

## Task 2：`mem_parts` 换签名 + 记账合计抽成共用函数（纯渲染）

**Files:**
- Modify: `crates/mullion-app/src/profile.rs:580-623`
- Test: `crates/mullion-app/src/profile.rs`（`mod tests` 内）

- [ ] **Step 1：先写失败的测试**

在 `crates/mullion-app/src/profile.rs` 的 `mod tests` 末尾（现有 `mem_parts_reports_the_remainder_honestly` 之后、`}` 之前）加入：

```rust
    /// F176:Windows 形态 —— 主数是 commit,ws 括在后面做交叉核对。
    ///
    /// 数字取自 N5 切片的实机日志(428MB commit / 289MB 专用工作集,
    /// 同一时刻),不是编的。
    #[test]
    fn mem_parts_renders_commit_and_ws_on_windows() {
        assert_eq!(
            mem_parts(
                crate::diag::MemKind::Commit,
                428,
                Some(289),
                0,
                0,
                5 << 20
            ),
            "commit=428MB(ws 289) = scroll:0 xfer:0 text:5 其他:423"
        );
    }

    /// F176:Linux 形态 —— 主数是 rss,**不印** `(ws …)`。
    ///
    /// 自证会变红:让 `MemKind::Rss` 也走 `(ws n/a)` 那条分支。
    #[test]
    fn mem_parts_renders_a_single_number_when_there_is_no_working_set() {
        assert_eq!(
            mem_parts(crate::diag::MemKind::Rss, 155, None, 0, 0, 5 << 20),
            "rss=155MB = scroll:0 xfer:0 text:5 其他:150"
        );
    }

    /// F176:Windows 老系统回落到 `EX` 之后 ws 采不到 —— 印 `n/a`,
    /// 不许静默印 0(印 0 会被读成「专用工作集真的是 0」)。
    #[test]
    fn mem_parts_says_n_a_when_the_working_set_could_not_be_sampled() {
        assert_eq!(
            mem_parts(crate::diag::MemKind::Commit, 428, None, 0, 0, 5 << 20),
            "commit=428MB(ws n/a) = scroll:0 xfer:0 text:5 其他:423"
        );
    }

    /// F176 的承重条:**减法算在主数(commit)上,不算在 ws 上**。
    ///
    /// 喂一个 ws(100) < 记账(168) < commit(400) 的组合:算 commit 时余量
    /// 232、正常分支;算 ws 时会走「记账超出」。断言必须落在正常分支上。
    ///
    /// 自证会变红:把 `mem_parts` 里的被减数换成 `ws_mb`。这正是这段代码
    /// 日后最可能被「顺手统一成一个数」重构掉的方式,而那么改之后日志照写、
    /// 数字照有,只在用户最小化窗口时才暴露。
    #[test]
    fn the_remainder_is_computed_against_commit_not_the_working_set() {
        assert_eq!(
            mem_parts(
                crate::diag::MemKind::Commit,
                400,
                Some(100),
                128 << 20,
                24 << 20,
                16 << 20
            ),
            "commit=400MB(ws 100) = scroll:128 xfer:24 text:16 其他:232"
        );
    }
```

同时把现有的 `mem_parts_reports_the_remainder_honestly`（第 2248~2271 行）四个调用改成新签名、并把「超出」文案里的 `RSS` 改成随口径走的标签：

```rust
    #[test]
    fn mem_parts_reports_the_remainder_honestly() {
        use crate::diag::MemKind;
        // 正常:340 = 128 + 0 + 16 + 196。
        assert_eq!(
            mem_parts(MemKind::Rss, 340, None, 128 << 20, 0, 16 << 20),
            "rss=340MB = scroll:128 xfer:0 text:16 其他:196"
        );
        // 全零记账:全进其他。
        assert_eq!(
            mem_parts(MemKind::Rss, 50, None, 0, 0, 0),
            "rss=50MB = scroll:0 xfer:0 text:0 其他:50"
        );
        // 负余量:记账 168MB > 主数 100MB,超出 68 要显式打出来。
        assert_eq!(
            mem_parts(MemKind::Rss, 100, None, 128 << 20, 24 << 20, 16 << 20),
            "rss=100MB = scroll:128 xfer:24 text:16 其他:0(记账超出rss 68MB)"
        );
        // 分支边界:记账恰好等于主数。余量 0 是**如实的 0**,不是「超出 0MB」
        // —— 少了这条,把 `<=` 写成 `<` 三条断言全不变红(恒绿缺口)。
        assert_eq!(
            mem_parts(MemKind::Rss, 144, None, 128 << 20, 0, 16 << 20),
            "rss=144MB = scroll:128 xfer:0 text:16 其他:0"
        );
    }
```

- [ ] **Step 2：跑测试确认它们失败**

Run: `cargo test -p mullion-app --lib mem_parts 2>&1 | tail -20`
Expected: 编译失败，`error[E0061]: this function takes 4 arguments but 6 arguments were supplied`

- [ ] **Step 3：实现**

把 `crates/mullion-app/src/profile.rs` 第 599~623 行的 `mem_parts` 替换为（**上方第 580~598 行的文档注释保留**，只在末尾追加一段，见下）：

```rust
/// F169/F176:记账合计(MB)。三块各自 `>> 20` 向下取整再相加。
///
/// **抽出来是为了让 `mem_parts` 与 `Snapshot::mem_other_mb` 同源** ——
/// 两处各算一遍的话,「日志里的其他」与「预算闸报的其他」迟早对不上,
/// 而那种不一致没有任何东西会报错。
pub fn mem_accounted_mb(scroll_b: u64, xfer_b: u64, text_b: u64) -> u64 {
    (scroll_b >> 20) + (xfer_b >> 20) + (text_b >> 20)
}

pub fn mem_parts(
    kind: crate::diag::MemKind,
    primary_mb: u64,
    ws_mb: Option<u64>,
    scroll_b: u64,
    xfer_b: u64,
    text_b: u64,
) -> String {
    let scroll = scroll_b >> 20;
    let xfer = xfer_b >> 20;
    let text = text_b >> 20;
    let accounted = mem_accounted_mb(scroll_b, xfer_b, text_b);
    // F176:ws 段只有 Commit 口径(Windows)才印 —— Linux 的 rss 本身就是
    // 常驻量,再括一个 ws 是同义反复。采不到印 `n/a`,不许静默印 0。
    let ws = match (kind, ws_mb) {
        (crate::diag::MemKind::Commit, Some(mb)) => format!("(ws {mb})"),
        (crate::diag::MemKind::Commit, None) => "(ws n/a)".to_string(),
        (crate::diag::MemKind::Rss, _) => String::new(),
    };
    let label = kind.label();
    if accounted <= primary_mb {
        format!(
            "{label}={primary_mb}MB{ws} = scroll:{scroll} xfer:{xfer} text:{text} 其他:{}",
            primary_mb - accounted
        )
    } else {
        format!(
            "{label}={primary_mb}MB{ws} = scroll:{scroll} xfer:{xfer} text:{text} \
             其他:0(记账超出{label} {}MB)",
            accounted - primary_mb
        )
    }
}
```

并在第 580~598 行那段文档注释的**末尾**追加：

```rust
/// **F176:`ws_mb` 不参与减法。** 工作集会被系统裁剪(窗口最小化时尤其
/// 激进),而三个记账块是 Rust 堆上的 `Vec`、字节数不因页被换出而变小。
/// 拿 ws 做被减数,用户一最小化就会刷屏「记账超出」——把一个正常的系统
/// 行为报成记账模型崩了。`primary_mb`(Windows 是 commit)不被裁剪、恒 ≥
/// 我们的堆量,减法才成立。ws 的职责是另外两件:它是任务管理器里那个数,
/// 以及 F177 预算闸的判据。守护:
/// `tests::the_remainder_is_computed_against_commit_not_the_working_set`。
```

- [ ] **Step 4：跑测试确认通过**

Run: `cargo test -p mullion-app --lib mem_parts 2>&1 | tail -20`
Expected: 5 个 `mem_parts*` 用例全 `ok`（此时 `render_lines` 那个调用点还没改，编译会失败——若失败信息只指向 `profile.rs:918` 附近的 `mem_parts(` 调用，属预期，Task 3 修）

若因 `render_lines` 编译不过而跑不了测试，先做 Task 3 Step 3 的那一处调用点修改，再回来跑本步。

- [ ] **Step 5：提交**（与 Task 3 合并提交，见 Task 3 Step 5——本任务单独不可编译，不单独提交）

---

## Task 3：`Snapshot` 接线（两个新字段 + `mem_other_mb` + 渲染行）

**Files:**
- Modify: `crates/mullion-app/src/profile.rs:302`（结构体）、`:412`（`Default`）、`:908-925`（`render_lines` 的 mem 行）
- Modify: `crates/mullion-app/src/diag.rs:1103`（`take_snapshot`）

- [ ] **Step 1：写失败的接线测试**

在 `crates/mullion-app/src/profile.rs` 的 `mod tests` 末尾加入：

```rust
    /// F176:`profile.mem` 行按快照的口径渲染,两个新字段确实接到了行上。
    ///
    /// 自证会变红:把 `render_lines` 里传给 `mem_parts` 的 `s.mem_ws_mb`
    /// 写死成 `None` —— 行里会变成 `(ws n/a)`,断言当场抓住。
    #[test]
    fn the_mem_line_carries_the_working_set_from_the_snapshot() {
        let mut s = busy_snapshot();
        s.mem_process_mb = 428;
        s.mem_kind = crate::diag::MemKind::Commit;
        s.mem_ws_mb = Some(289);
        s.mem_scroll_bytes = 0;
        s.xfer_running = 0;
        s.mem_text_bytes = 5 << 20;
        let lines = render_lines(&s, false);
        let mem = lines
            .iter()
            .find(|l| l.starts_with("profile.mem "))
            .expect("应有 profile.mem 行");
        assert_eq!(
            mem,
            "profile.mem commit=428MB(ws 289) = scroll:0 xfer:0 text:5 其他:423"
        );
    }

    /// F177 的分母:`Snapshot::mem_other_mb` 与 `profile.mem` 行里的
    /// `其他:` **必须同源**。两处各算一遍的话,预算闸报的数和日志上一行
    /// 报的数会对不上,而没有任何东西会报错。
    ///
    /// 自证会变红:把 `mem_other_mb` 改成直接返回 `mem_process_mb`。
    #[test]
    fn the_other_bucket_is_the_same_number_in_the_line_and_in_the_alarm() {
        let mut s = busy_snapshot();
        s.mem_process_mb = 428;
        s.mem_kind = crate::diag::MemKind::Commit;
        s.mem_ws_mb = Some(289);
        s.mem_scroll_bytes = 0;
        s.xfer_running = 0;
        s.mem_text_bytes = 5 << 20;
        assert_eq!(s.mem_other_mb(), 423);
        let lines = render_lines(&s, false);
        assert!(lines
            .iter()
            .any(|l| l.starts_with("profile.mem ") && l.ends_with("其他:423")));
    }
```

- [ ] **Step 2：跑测试确认失败**

Run: `cargo test -p mullion-app --lib the_mem_line_carries 2>&1 | tail -20`
Expected: 编译失败，`error[E0609]: no field 'mem_kind' on type 'Snapshot'`

- [ ] **Step 3：实现**

**3a.** `crates/mullion-app/src/profile.rs` 第 302 行 `pub mem_process_mb: u64,` 之后插入：

```rust
    /// F176:`mem_process_mb` 的口径(Windows commit / Linux rss)。
    pub mem_kind: crate::diag::MemKind,
    /// F176:专用工作集(MB)。`None` = 采不到。**不参与记账减法**,
    /// 理由见 `mem_parts` 的文档注释;它是 F177 预算闸的判据。
    ///
    /// 用 `Option` 而不是沿用 `mem_process_mb` 那个 0 哨兵:0 哨兵是 F155
    /// 的既有债(见本文件 `render_lines` 里 mem 行上方的注释),不再复制第二份。
    pub mem_ws_mb: Option<u64>,
```

**3b.** 第 412 行 `mem_process_mb: 0,` 之后插入：

```rust
            mem_kind: crate::diag::MemKind::Rss,
            mem_ws_mb: None,
```

（`Default` 取 `Rss` 是因为它是「不印 ws 段」的那一档——默认值不该凭空造出一个 `(ws n/a)`。）

**3c.** 在 `impl Snapshot`（`is_idle`/`cpu_is_busy` 所在的那个 impl，第 480 行的 `}` 之前）加入：

```rust
    /// F177:这一窗口「其他」栏的 MB 数,与 `profile.mem` 行同源
    /// (共用 [`mem_accounted_mb`])。预算闸的告警行要带上它。
    pub fn mem_other_mb(&self) -> u64 {
        self.mem_process_mb.saturating_sub(mem_accounted_mb(
            self.mem_scroll_bytes,
            self.xfer_running * XFER_CHUNK,
            self.mem_text_bytes,
        ))
    }
```

**3d.** 第 916~924 行 `render_lines` 里的 `mem_parts` 调用改为：

```rust
        lines.push(format!(
            "profile.mem {}",
            mem_parts(
                s.mem_kind,
                s.mem_process_mb,
                s.mem_ws_mb,
                s.mem_scroll_bytes,
                xfer_buf_bytes,
                s.mem_text_bytes
            )
        ));
```

**3e.** `crates/mullion-app/src/diag.rs` 第 1103 行替换为：

```rust
    // F176:一次采样喂三个字段。**分三次调 `sample_memory()` 会让三个数
    // 来自不同时刻**,而它们随后要一起做减法。
    if let Some(m) = sample_memory() {
        s.mem_process_mb = m.process_bytes / (1024 * 1024);
        s.mem_kind = m.kind;
        s.mem_ws_mb = m.ws_bytes.map(|b| b / (1024 * 1024));
    }
```

（采不到时三个字段留在 `Default`：`mem_process_mb == 0` 触发 `render_lines` 里既有的 `profile.mem n/a(RSS 采不到)` 分支，行为不变。）

- [ ] **Step 4：跑测试确认通过**

Run: `cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: `test result: ok.`（`mem_parts` 五条 + 新的两条接线条全过）

- [ ] **Step 5：提交**

```bash
git add crates/mullion-app/src/profile.rs crates/mullion-app/src/diag.rs
git commit -m "feat(app): profile.mem 报双口径,减法仍算在 commit 上 (F176)

日志里的 428(PrivateUsage 提交量)与任务管理器的 289(专用工作集)对不上,
是上一轮排查绕开日志直接上 VMMap 的根因。现在一行同时给两个数。

ws 刻意不参与减法:工作集会被系统裁剪(最小化窗口时尤甚),拿它做被减数
会把正常的系统行为报成「记账超出」。守护
the_remainder_is_computed_against_commit_not_the_working_set。"
```

---

## Task 4：预算闸的判定与渲染（纯函数）

**Files:**
- Modify: `crates/mullion-app/src/profile.rs`（`mem_accounted_mb` 之后）
- Test: `crates/mullion-app/src/profile.rs`（`mod tests` 内）

- [ ] **Step 1：写失败的测试**

在 `crates/mullion-app/src/profile.rs` 的 `mod tests` 末尾加入：

```rust
    /// F177:预算闸状态机 —— 首次越界报、同值静默、涨够步长再报、
    /// 跌到滞回带下沿才算回落且复位后能再报。
    ///
    /// 自证会变红:去掉步长判据(改成越界态每窗口都 `Cross`),第二条断言抓住。
    #[test]
    fn budget_verdict_reports_once_then_only_on_a_real_climb() {
        // 首次越界。
        assert_eq!(budget_verdict(Some(428), None), BudgetVerdict::Cross(428));
        // 同值再来:安静。
        assert_eq!(budget_verdict(Some(428), Some(428)), BudgetVerdict::Quiet);
        // 涨了 63:还不够一个步长(64),安静。
        assert_eq!(budget_verdict(Some(491), Some(428)), BudgetVerdict::Quiet);
        // 涨够 64:再报一次。
        assert_eq!(
            budget_verdict(Some(492), Some(428)),
            BudgetVerdict::Cross(492)
        );
        // 跌到滞回带下沿(300-16=284)以下:回落。
        assert_eq!(
            budget_verdict(Some(180), Some(428)),
            BudgetVerdict::Recover(180)
        );
        // 复位之后还能再报。
        assert_eq!(budget_verdict(Some(500), None), BudgetVerdict::Cross(500));
    }

    /// F177 的承重条:阈值附近抖动(299↔301)**一行都不许写**。
    ///
    /// 没有滞回带时,每一次穿越都产出一对 Cross/Recover,两行日志每几个
    /// 窗口来一遍 —— 一个空闲进程被这条警告永远吵醒,正是「穿透空闲门」
    /// 时拿「O(1) 不是 O(每5秒)」当理由所排除掉的那件事。
    ///
    /// 自证会变红:把 Recover 判据的 `- MEM_HYSTERESIS_MB` 去掉。
    #[test]
    fn jitter_around_the_threshold_does_not_wake_the_disk() {
        let mut reported: Option<u64> = None;
        let mut lines = 0;
        for ws in [301u64, 299, 301, 299, 300, 301] {
            match budget_verdict(Some(ws), reported) {
                BudgetVerdict::Quiet => {}
                BudgetVerdict::Cross(mb) => {
                    lines += 1;
                    reported = Some(mb);
                }
                BudgetVerdict::Recover(_) => {
                    lines += 1;
                    reported = None;
                }
            }
        }
        assert_eq!(lines, 1, "阈值附近抖动只该在第一次越界时写一行");
    }

    /// F177:恰好等于预算不算超 —— 判据是严格大于。
    ///
    /// 自证会变红:把 `>` 写成 `>=`。
    #[test]
    fn the_threshold_is_strictly_greater_than() {
        assert_eq!(
            budget_verdict(Some(N5_BUDGET_MB), None),
            BudgetVerdict::Quiet
        );
        assert_eq!(
            budget_verdict(Some(N5_BUDGET_MB + 1), None),
            BudgetVerdict::Cross(N5_BUDGET_MB + 1)
        );
    }

    /// F177:采不到读数的机器上不许凭空报警。
    ///
    /// 同 `cpu_is_busy` 那条 `is_some_and` 的道理。
    /// 自证会变红:把 `let Some(ws) = ws_mb else { … }` 换成
    /// `ws_mb.unwrap_or(u64::MAX)`。
    #[test]
    fn no_reading_means_no_alarm() {
        assert_eq!(budget_verdict(None, None), BudgetVerdict::Quiet);
        assert_eq!(budget_verdict(None, Some(428)), BudgetVerdict::Quiet);
    }

    /// F177:两种告警行的正文。
    #[test]
    fn the_alarm_lines_say_which_number_tripped_and_what_else_is_on_the_books() {
        assert_eq!(
            over_budget_line(428, 512, 507),
            "profile.mem.over ws=428MB > N5 300MB (commit 512, 其他 507)"
        );
        assert_eq!(recovered_line(180), "profile.mem.over 回落 ws=180MB");
    }
```

- [ ] **Step 2：跑测试确认失败**

Run: `cargo test -p mullion-app --lib budget 2>&1 | tail -20`
Expected: 编译失败，`error[E0425]: cannot find function 'budget_verdict' in this scope`

- [ ] **Step 3：实现**

在 `crates/mullion-app/src/profile.rs` 里 `mem_accounted_mb` 之后加入：

```rust
/// F177:spec.md 的 N5 —— 常驻内存(8 pane,10000 行回溯)< 300MB。
///
/// **这个数在核显机器上含义更严**:UMA 没有独立显存,wgpu 的每一份分配都
/// 计进工作集(N5 切片实测 165MB WriteCombine 全在账内)。独显机器上同样的
/// 代码会低一大截。看到读数逼近上限时,先问「核显还是独显」再下结论。
pub const N5_BUDGET_MB: u64 = 300;

/// F177:报告步长。越界之后,比上次报告值又高这么多才再报一次。
///
/// **刻意不用 `diag::should_report` 那套翻倍**:内存从 300 翻到 600 才吭
/// 第二声,中间一条几百 MB 的慢泄漏全程静默。固定步长对内存更合适。
pub const MEM_REPORT_STEP_MB: u64 = 64;

/// F177:回落滞回带。
///
/// **删掉它不会有任何测试以外的报错**,但 ws 在阈值附近抖动(299↔301,
/// Windows 主动裁剪工作集时很常见)会让每一次穿越都产出一对
/// `Cross`/`Recover` —— 空闲进程每几个窗口写两行日志,硬盘永不休眠,
/// 正是「这条警告可以穿透空闲门」所依据的前提被推翻。
/// 守护:`tests::jitter_around_the_threshold_does_not_wake_the_disk`。
pub const MEM_HYSTERESIS_MB: u64 = 16;

/// F177:这一窗口预算闸该不该出声。
///
/// 两个变体携带的都是**触发本次判定的那个 `ws_mb`**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetVerdict {
    Quiet,
    /// 越界(首次,或比上次报告值又涨了一个步长)。调用方置
    /// `reported_mb = Some(mb)`。
    Cross(u64),
    /// 跌回滞回带下沿以下。调用方置 `reported_mb = None`。
    Recover(u64),
}

/// F177:纯判定。调用方持有 `reported_mb: Option<u64>`
/// (语义:上一次报 `Cross` 时的 ws;`None` = 当前不在越界态)。
///
/// **`ws_mb == None` 一律 `Quiet`**:读不到工作集的机器上不能凭空报警,
/// 同 `Snapshot::cpu_is_busy` 里那条 `is_some_and` 的道理(那是私有方法,
/// 这里刻意用普通代码体而非 intra-doc 链接,免得 `cargo doc` 报私有链接)。
pub fn budget_verdict(ws_mb: Option<u64>, reported_mb: Option<u64>) -> BudgetVerdict {
    let Some(ws) = ws_mb else {
        return BudgetVerdict::Quiet;
    };
    match reported_mb {
        // 不在越界态:严格大于才算越界(恰好等于预算不算超)。
        None => {
            if ws > N5_BUDGET_MB {
                BudgetVerdict::Cross(ws)
            } else {
                BudgetVerdict::Quiet
            }
        }
        // 已在越界态:先看回落(带滞回),再看有没有涨够一个步长。
        Some(prev) => {
            if ws <= N5_BUDGET_MB.saturating_sub(MEM_HYSTERESIS_MB) {
                BudgetVerdict::Recover(ws)
            } else if ws >= prev.saturating_add(MEM_REPORT_STEP_MB) {
                BudgetVerdict::Cross(ws)
            } else {
                BudgetVerdict::Quiet
            }
        }
    }
}

/// F177:越界告警的正文。带上 commit 与「其他」,让读日志的人不必再去
/// 翻同一窗口的 `profile.mem` 行。
pub fn over_budget_line(ws_mb: u64, commit_mb: u64, other_mb: u64) -> String {
    format!(
        "profile.mem.over ws={ws_mb}MB > N5 {N5_BUDGET_MB}MB \
         (commit {commit_mb}, 其他 {other_mb})"
    )
}

/// F177:回落行。
///
/// **这行必须存在**:否则读日志的人看到一条越界警告,无从判断它是
/// 「当时闪了一下」还是「从那以后一直这样」。
pub fn recovered_line(ws_mb: u64) -> String {
    format!("profile.mem.over 回落 ws={ws_mb}MB")
}
```

- [ ] **Step 4：跑测试确认通过**

Run: `cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: `test result: ok.`

- [ ] **Step 5：提交**

```bash
git add crates/mullion-app/src/profile.rs
git commit -m "feat(app): N5 预算闸的判定与告警正文 (F177)

绝对阈值不是百分比:我们只记 scroll/xfer/text 三种内容缓冲,基线的
GPU/字体/代码从来不在账上,空载时「其他」天然接近 100%,健康版本也一样。

回落判据带 16MB 滞回带 —— 没有它,ws 在 299↔301 抖动会让空闲进程
每几个窗口写两行日志。守护 jitter_around_the_threshold_does_not_wake_the_disk。"
```

---

## Task 5：驱动接线（穿透空闲门 + 穿透 Info 日志门）

**Files:**
- Modify: `crates/mullion-app/src/diag.rs:964`（局部状态）、`:1022-1033`（调用点）
- Test: `crates/mullion-app/src/diag.rs`（`mod tests` 内，源码切片守护）

- [ ] **Step 1：写失败的测试**

在 `crates/mullion-app/src/diag.rs` 的 `mod tests` 末尾加入：

```rust
    /// F177 的承重条:预算闸的调用点必须在 **`log_enabled!(Info)` 那个门
    /// 之外**,也就是在 `render_lines` 之前。
    ///
    /// 两件事同时挂在这个位置上:
    /// 1. **穿透空闲门** —— `render_lines` 开头 `is_idle()` 直接返回空,
    ///    而空载正是 N5 那次要查的场景。挪进去之后编译过、纯函数测试
    ///    全绿,只有实机空载时才发现它不响。
    /// 2. **穿透 Info 门** —— 这条是 `warn!`,warn 档下也该响;关进
    ///    Info 门里等于「把日志调低就听不见警报」。
    ///
    /// 判据用**行序**而不是「文件里包含 budget_verdict」:后者对「整段
    /// 挪进门里」恒绿。
    ///
    /// 自证会变红:把 `budget_verdict(` 那一段剪切到
    /// `if log::log_enabled!(target: "mullion", log::Level::Info) {` 之后。
    #[test]
    fn the_budget_gate_is_asked_before_the_info_log_gate() {
        let src = include_str!("diag.rs");
        let src = &src[..src.find("#[cfg(test)]").expect("diag.rs 应有测试模块")];
        let gate = src
            .find("if log::log_enabled!(target: \"mullion\", log::Level::Info) {")
            .expect("应有 Info 日志门");
        let call = src
            .find("crate::profile::budget_verdict(")
            .expect("应有预算闸调用点");
        assert!(
            call < gate,
            "预算闸必须问在 Info 日志门之前:关在门里的话,warn 档听不见警报,\
             且它会连带被 render_lines 的空闲门挡掉 —— 而空载正是要查的场景"
        );
    }
```

- [ ] **Step 2：跑测试确认失败**

Run: `cargo test -p mullion-app --lib the_budget_gate_is_asked_before 2>&1 | tail -20`
Expected: FAIL，`应有预算闸调用点`（panic on `expect`）

- [ ] **Step 3：实现**

**3a.** `crates/mullion-app/src/diag.rs` 第 964 行 `let mut reported_ms = 0u64;` 之后插入：

```rust
    // F177:预算闸的状态 —— 上一次报 `Cross` 时的 ws(MB)。
    // `None` = 当前不在越界态。与 `reported_ms` 一样是本线程私有的,
    // 不需要原子量。
    let mut reported_mb: Option<u64> = None;
```

**3b.** 第 1022 行 `}`（`match threads.sample()` 的收尾）与第 1023 行 `if log::log_enabled!(…Info) {` 之间插入：

```rust
            // F177:预算闸问在 Info 日志门**之外**,两个理由:
            // 这是 `warn!`(warn 档下也该响),且它必须绕开 `render_lines`
            // 开头那道空闲门 —— 空载正是 N5 那次要查的场景,而空闲门会
            // 让那种窗口一行都不写。每次越界只写一行,是 O(1) 不是
            // O(每5秒),不违反「别吵醒笔记本硬盘」那条初衷。
            // 守护:tests::the_budget_gate_is_asked_before_the_info_log_gate。
            match crate::profile::budget_verdict(snap.mem_ws_mb, reported_mb) {
                crate::profile::BudgetVerdict::Quiet => {}
                crate::profile::BudgetVerdict::Cross(mb) => {
                    log::warn!(
                        target: "mullion",
                        "{}",
                        crate::profile::over_budget_line(
                            mb,
                            snap.mem_process_mb,
                            snap.mem_other_mb()
                        )
                    );
                    reported_mb = Some(mb);
                }
                crate::profile::BudgetVerdict::Recover(mb) => {
                    log::info!(target: "mullion", "{}", crate::profile::recovered_line(mb));
                    reported_mb = None;
                }
            }
```

- [ ] **Step 4：跑测试确认通过**

Run: `cargo test -p mullion-app --lib the_budget_gate_is_asked_before 2>&1 | tail -10`
Expected: `test result: ok. 1 passed`

- [ ] **Step 5：全量绿**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```
Expected: 全部 `test result: ok.`；clippy 无输出；fmt 无输出

- [ ] **Step 6：提交**

```bash
git add crates/mullion-app/src/diag.rs
git commit -m "feat(app): 预算闸接进看门狗,问在 Info 日志门之外 (F177)

位置本身是承重的:关进 Info 门里则 warn 档听不见警报,且会连带被
render_lines 的空闲门挡掉 —— 而空载(N5 那次要查的场景)恰恰是空闲门
拦掉的那一类。守护 the_budget_gate_is_asked_before_the_info_log_gate
用行序判据,「文件里包含 budget_verdict」对整段挪进门里恒绿。"
```

---

## Task 6：变异自证

**Files:** 无（只跑不改）

> 项目纪律：变异验证**在提交之后**做，避免 `git checkout` 吞掉未提交的编辑。
> `sed` **必须行锚定**（`^\( *\)…$`），否则会连带改掉守护测试里的期望字符串，
> 变异失效而测试「通过」——那是假绿，不是「变异没被杀死」。

- [ ] **Step 1：变异 ①——减法换成 ws**

```bash
sed -i 's/^\( *\)let accounted = mem_accounted_mb(scroll_b, xfer_b, text_b);$/\1let accounted = ws_mb.unwrap_or(primary_mb);/' crates/mullion-app/src/profile.rs
grep -c "the_remainder_is_computed_against_commit" crates/mullion-app/src/profile.rs   # 应为 1,确认测试期望完好
cargo test -p mullion-app --lib the_remainder_is_computed_against_commit 2>&1 | tail -5
git checkout crates/mullion-app/src/profile.rs
```
Expected: `FAILED`

- [ ] **Step 2：变异 ②——去掉滞回带**

```bash
sed -i 's/^\( *\)if ws <= N5_BUDGET_MB.saturating_sub(MEM_HYSTERESIS_MB) {$/\1if ws <= N5_BUDGET_MB {/' crates/mullion-app/src/profile.rs
cargo test -p mullion-app --lib jitter_around_the_threshold 2>&1 | tail -5
git checkout crates/mullion-app/src/profile.rs
```
Expected: `FAILED`

- [ ] **Step 3：变异 ③——阈值改成 `>=`**

```bash
sed -i 's/^\( *\)if ws > N5_BUDGET_MB {$/\1if ws >= N5_BUDGET_MB {/' crates/mullion-app/src/profile.rs
cargo test -p mullion-app --lib the_threshold_is_strictly_greater_than 2>&1 | tail -5
git checkout crates/mullion-app/src/profile.rs
```
Expected: `FAILED`

- [ ] **Step 4：变异 ④——采不到时凭空报警**

```bash
sed -i 's/^\( *\)let Some(ws) = ws_mb else {$/\1let Some(ws) = ws_mb.or(Some(u64::MAX)) else {/' crates/mullion-app/src/profile.rs
cargo test -p mullion-app --lib no_reading_means_no_alarm 2>&1 | tail -5
git checkout crates/mullion-app/src/profile.rs
```
Expected: `FAILED`

- [ ] **Step 5：变异 ⑤——ws 段写死 None（接线断开）**

```bash
sed -i 's/^\( *\)s.mem_ws_mb,$/\1None,/' crates/mullion-app/src/profile.rs
cargo test -p mullion-app --lib the_mem_line_carries_the_working_set 2>&1 | tail -5
git checkout crates/mullion-app/src/profile.rs
```
Expected: `FAILED`

- [ ] **Step 6：变异 ⑥——预算闸挪进 Info 门**

手工把 `diag.rs` 里那整段 `match crate::profile::budget_verdict(…) { … }`
剪切到 `if log::log_enabled!(target: "mullion", log::Level::Info) {` 的**下一行**。

```bash
cargo test -p mullion-app --lib the_budget_gate_is_asked_before 2>&1 | tail -5
git checkout crates/mullion-app/src/diag.rs
```
Expected: `FAILED`

- [ ] **Step 7：确认工作区干净**

Run: `git status --porcelain`
Expected: 无输出（六次 `git checkout` 都还原干净了）

若某条变异**没有**变红，说明对应的守护测试是恒绿的——**修测试，不要跳过**
（`~/.claude` 的恒绿清单：源码切片用「文件里包含某串」当判据，对「整段挪位置」
和「改签名」两类变异恒绿）。

---

## Task 7：spec.md 补两行

**Files:**
- Modify: `spec.md`（F 表末尾，现停在 F173）

- [ ] **Step 1：追加两行**

在 `spec.md` 第 178 行（F173 那一行）之后插入：

```markdown
| F176 | **内存口径收口**：`profile.mem` 改报 `commit=428MB(ws 289) = scroll:0 xfer:0 text:5 其他:423`。原先只报 `PrivateUsage`（提交量），与任务管理器进程页「内存」列（**专用**工作集）同一时刻差着一百多 MB，对不上所以没人信，N5 那轮排查因此绕开日志直接上 VMMap——**这才是这条要修的东西** | P2 | ws 取 `PROCESS_MEMORY_COUNTERS_EX2.PrivateWorkingSetSize`，**不是 `EX.WorkingSetSize`**（后者是含共享页的总工作集，对照照样对不上、只是差到另一个方向）；同一次 `K32GetProcessMemoryInfo`，零额外系统调用；`EX2` 要 Win11/Server2022，老系统回落 `EX` 并报 `(ws n/a)`。`PrivateWorkingSetSize == 0` 当采不到——结构体是 `zeroed()` 出来的，0 是「没被填写」的唯一痕迹，而活进程的专用工作集不可能真为 0。**ws 刻意不参与减法**：工作集会被系统裁剪（最小化窗口尤甚）而记账块是堆上的 `Vec`、不随换出变小，拿它做被减数会把正常系统行为报成「记账超出」刷屏；减法算在 commit 上。平台差异下沉成 `diag::MemKind`，**渲染层不带 `#[cfg]`**，Windows 形态能在 Linux 开发机上单测。守护：`profile::tests` 五条（Windows 双数／Linux 单数不印 `(ws …)`／回落印 `n/a`／`the_remainder_is_computed_against_commit_not_the_working_set`／既有的余量四态）+ `the_mem_line_carries_the_working_set_from_the_snapshot`。**验不了**：ws 与任务管理器是否真的一致、老 Windows 的回落路径——交人工验收 |
| F177 | **N5 预算闸**：ws 越过 `N5_BUDGET_MB`（300）时单独写一行 `WARN profile.mem.over ws=428MB > N5 300MB (commit 512, 其他 507)`，跌回后写一行 `回落`。补的是「`其他:423` 在日志里躺了几百次、和一切正常时长得一模一样」——数据一直都在，缺的是它自己会叫 | P2 | **判据必须是绝对阈值，百分比那条路是死的**：只记 `scroll/xfer/text` 三种内容缓冲，基线的 GPU／字体／代码从不在账上，空载时 `其他` 天然接近 100%、健康版本也一样，按占比报警等于常亮。越界判 `>`（恰好 300 不算超）；越界后只在比上次报告值又高 ≥64MB 时再报（**不用 `diag::should_report` 那套翻倍**——300→600 才吭第二声，中间几百 MB 的慢泄漏全程静默）；**回落判 `ws <= 300-16`，滞回带不可省**：没有它，ws 在 299↔301 抖动（Windows 主动裁剪工作集时常见）会让每次穿越产出一对 `Cross`/`Recover`，空闲进程每几个窗口写两行、硬盘永不休眠，正是「这条可以穿透空闲门」所依据的前提被推翻。**调用点在 `log_enabled!(Info)` 门之外**：关进门里则 warn 档听不见警报，且会连带被 `render_lines` 的 `is_idle()` 挡掉——而空载恰恰是空闲门拦掉的那一类，也正是要查的场景；挪进去之后编译过、纯函数测试全绿，只有实机空载才发现它不响，故守护用**行序**判据（`the_budget_gate_is_asked_before_the_info_log_gate`），「文件里包含 `budget_verdict`」对这个变异恒绿。`ws_mb == None` 恒 `Quiet`（同 `cpu_is_busy` 的 `is_some_and`：读不到的机器不许凭空报警）。守护：`profile::tests` 五条（状态机／抖动不写盘／严格大于／采不到不报警／两种告警正文）。**验不了**：实机上这条警告的节奏是否如设计——交人工验收（临时调低 `N5_BUDGET_MB` 触发） |
```

> **注意**：`spec.md` 的 F 表停在 F173，F174/F175 只活在提交信息里（既有漂移）。
> 本任务**只补 F176/F177**，不顺手补 F174/F175——那是另一件事（Scope Discipline）。
> 表格里的 `|` 必须转义成 `\|`（照抄上方 F173 行的写法）。

- [ ] **Step 2：确认表格没被写坏**

Run: `grep -c "^| F17[67] |" spec.md`
Expected: `2`

- [ ] **Step 3：提交**

```bash
git add spec.md
git commit -m "docs: spec 补 F176/F177 两行(内存口径收口 + N5 预算闸)"
```

---

## Task 8：发版（`release-windows` 一条龙）

改动落在 `mullion-app`，按项目交付约定**不再问、直接做**。

- [ ] **Step 1：升版本**

`Cargo.toml` 的 `workspace.package.version` 第三位 +1（`0.1.77` → `0.1.78`），单独提交：

```bash
git commit -am "chore: 版本 0.1.78(profile.mem 报双口径 + N5 预算闸)"
```

- [ ] **Step 2：走 `release-windows` skill 的全部步骤**

跑绿 → 交叉编译 → objdump 验收 → 签名 → 先 push 再 `gh release create`。
命令、代理设置、notes 模板见 `.claude/skills/release-windows/SKILL.md`，
**别凭记忆做**。

- [ ] **Step 3：notes 里的人工验收清单**（照抄设计文档 §6）

```markdown
## 人工验收清单

- [ ] `profile.mem` 那行的 `ws` 数与**任务管理器进程页「内存」列**在同一时刻一致
      —— 这是 F176 存在的全部理由，也是唯一的真判据
      `Select-String -Path "$env:APPDATA\mullion\config\mullion-*.log" -Pattern "profile\.mem " | Select-Object -Last 3`
- [ ] 最小化窗口再恢复：`ws` 明显掉下去又涨回来，而 `其他` **不**出现
      `记账超出`（这是 ws 不参与减法那条设计的实证）
- [ ] 老 Windows（非 11）上印 `(ws n/a)` 而不是崩或印 0
      —— 手边没有这种机器就如实标注「未验证」
- [ ] 空载静置两分钟，全程**没有** `profile.mem.over`（0.1.77 基线 ~155MB，
      离 300 还远）；把 `N5_BUDGET_MB` 临时调低重跑一次，确认按
      「首次一次 / +64MB 再一次 / 回落一次」的节奏出现
- [ ] 连接真机 tmux + Claude Code 正常（本版未动 SSH/输入/渲染路径）
```

---

## 自查（写完计划后过的一遍）

**Spec 覆盖**：§2.1 形态→Task 2/3；§2.2 减法口径→Task 2 + 变异①；§2.3 字段选择与回落→Task 1；§2.4 数据层→Task 1/3；§2.5 五条守护→Task 2/3；§3.1 阈值与三常量→Task 4；§3.2 输出→Task 4；§3.3 节奏与滞回→Task 4 + 变异②；§3.4 穿透空闲门→Task 5 + 变异⑥；§3.5 守护表→Task 4/5；§4 依赖方向→无新依赖（Task 1 已核实 `windows-sys 0.59` 自带 `EX2`）；§5 不做的→计划里没有对应任务（正确）；§6 人工验收→Task 8 Step 3；§7 风险→Task 8 的验收清单第 3 条与第 4 条。**无缺口。**

**类型一致性**：`MemKind`（`diag.rs` 定义，`profile.rs` 以 `crate::diag::MemKind` 引用）、`MemSample.ws_bytes: Option<u64>`（字节）→ `Snapshot.mem_ws_mb: Option<u64>`（MB，`take_snapshot` 里 `/ (1024*1024)`）→ `budget_verdict(ws_mb: Option<u64>, …)`，单位在 Task 3 Step 3e 换算一次、全程 MB。`mem_accounted_mb` 被 `mem_parts` 与 `Snapshot::mem_other_mb` 共用。`over_budget_line(ws_mb, commit_mb, other_mb)` 三个参数与 Task 5 的调用点 `(mb, snap.mem_process_mb, snap.mem_other_mb())` 一一对应。

**已知的顺序依赖**：Task 2 单独不可编译（`render_lines` 的调用点在 Task 3 才改），故两者合并在 Task 3 Step 5 提交——计划里已显式写明，不是遗漏。
