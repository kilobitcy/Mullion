# F164~F166 周期 CPU/GPU 剖面 + 一实例一日志 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 `mullion.log` 每 5 秒的 profile 行带上进程/主线程 CPU%、GPU 引擎占用率、显存与 GPU 帧耗时；同时把日志改成一实例一文件，使这些 per-process 数字在多开时归属明确。

**Architecture:** 新模块 `sysprobe.rs` 收纳三套平台探针（CPU 时间差分、PDH GPU%、DXGI 显存），由 `diag.rs` 的看门狗线程每 5 秒调用，主线程零成本；GPU 帧耗时长在渲染路径上（`gpu.rs` 建 QuerySet、`app.rs` 挂 `timestamp_writes`），抽样回读后喂进 `diag` 现有直方图。`logx.rs` 改为按 F148 的 instance id 命名日志文件，轮转从「启动时」改为「运行期」，并在启动时按心跳判活回收陈旧文件。

**Tech Stack:** Rust / windows-sys 0.59（PDH）/ windows 0.59（DXGI COM）/ wgpu 23.0.1（TIMESTAMP_QUERY）/ mullion-store 的 F148 心跳 API

**设计文档:** `docs/superpowers/specs/2026-08-26-f164-f166-cpu-gpu-profile-per-instance-log-design.md`

---

## 背景：读这些再动手

**必读**（每条都对应下面某个任务里会踩的坑）：

- `CLAUDE.md` 的「领域陷阱」表，尤其 **T3**（喂数据和重绘解耦：任何进热路径的系统调用都是违规）。
- `docs/gui-render-gotchas.md`：动 `gpu.rs` / `app.rs` 前扫一遍。
- `crates/mullion-store/src/history.rs`：F148 的 `new_instance_id` / `is_alive` / `alive_path` / `ALIVE_EXT` / `now_secs`，本计划全程复用，**不要重写一套**。

**已核实的 API 签名**（按锁定版本从本地 registry 读出，不要凭记忆改）：

| API | 签名 |
|---|---|
| `wgpu::RenderPassTimestampWrites` | `{ query_set: &QuerySet, beginning_of_pass_write_index: Option<u32>, end_of_pass_write_index: Option<u32> }` |
| `CommandEncoder::resolve_query_set` | `(&mut self, &QuerySet, Range<u32>, &Buffer, BufferAddress)` |
| `Queue::get_timestamp_period` | `(&self) -> f32` |
| `PdhOpenQueryW` | `(PCWSTR, usize, *mut isize) -> u32` |
| `PdhAddEnglishCounterW` | `(isize, PCWSTR, usize, *mut isize) -> u32` |
| `PdhCollectQueryData` | `(isize) -> u32` |
| `PdhGetFormattedCounterArrayW` | `(isize, PDH_FMT, *mut u32, *mut u32, *mut PDH_FMT_COUNTERVALUE_ITEM_W) -> u32` |
| `IDXGIAdapter3::QueryVideoMemoryInfo` | `(&self, u32, DXGI_MEMORY_SEGMENT_GROUP, *mut DXGI_QUERY_VIDEO_MEMORY_INFO) -> Result<()>` |
| `DXGI_QUERY_VIDEO_MEMORY_INFO` | `{ Budget: u64, CurrentUsage: u64, AvailableForReservation: u64, CurrentReservation: u64 }` |

**关键约束**：`#[cfg(windows)]` 的代码在 Linux 上 `cargo test` / `cargo clippy` **完全看不到**。每个碰 Windows 代码的任务，提交前必须跑一次交叉编译（见 Task 15），否则「本机全绿、真机编不过」。

---

## 文件结构

| 文件 | 责任 | 动作 |
|---|---|---|
| `crates/mullion-app/src/sysprobe.rs` | CPU/GPU%/显存三套平台探针。纯函数 + 薄 FFI 壳。**不认识 Snapshot、不写日志** | 新建 |
| `crates/mullion-app/src/logx.rs` | 日志文件命名/轮转/清理 + 行格式 | 改 |
| `crates/mullion-app/src/diag.rs` | 看门狗周期调用探针，填 Snapshot | 改 |
| `crates/mullion-app/src/profile.rs` | Snapshot 新字段 + 渲染 + 空闲门 | 改 |
| `crates/mullion-app/src/gpu.rs` | TIMESTAMP_QUERY feature、QuerySet、staging buffer、回读状态机 | 改 |
| `crates/mullion-app/src/app.rs` | render pass 挂 `timestamp_writes`；`instance_id` 改读 logx；F155 导出改名 | 改 |
| `crates/mullion-app/src/lib.rs` | 挂 `mod sysprobe` | 改 |
| `crates/mullion-app/Cargo.toml` | 加 PDH / DXGI feature | 改 |

---

# 阶段一：F166 一实例一日志（F164/F165 的前置）

## Task 1: instance id 上移到 logx

**为什么先做这个**：`logx::init`（main.rs:41）跑在 `App::new`（main.rs:98）之前，日志文件名要用 id，而 id 现在生成在 `App::new` 里。上移之后日志文件 ⇄ F148 现场历史记录一一对应。

**Files:**
- Modify: `crates/mullion-app/src/logx.rs`
- Modify: `crates/mullion-app/src/app.rs:2245-2248`

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-app/src/logx.rs` 的 `mod tests` 末尾（`}` 之前）加：

```rust
    /// 同一进程里 `instance_id()` 必须每次返回同一个值。
    ///
    /// 它同时决定日志文件名和 F148 现场历史的记录名 —— 两次调用拿到不同的
    /// id,症状是「日志文件里写着 A,历史记录叫 B」,排障时根本对不上号,
    /// 而且没有任何报错。
    ///
    /// 自证会变红:把 `instance_id` 里的 `get_or_init` 换成每次现算
    /// `new_instance_id(now_ms(), process::id())`。
    #[test]
    fn the_instance_id_is_stable_within_one_process() {
        let a = instance_id();
        let b = instance_id();
        assert_eq!(a, b, "同一进程两次拿到不同的 instance id");
        assert!(!a.is_empty(), "instance id 是空的");
    }

    /// id 的形状必须是 F148 的 `{毫秒}-{pid}`。
    ///
    /// 形状是硬约定:Task 3 的文件名解析器按「两段纯数字」严格校验,
    /// 形状一变,自己的日志会被自己的清理逻辑判成不认识的文件。
    ///
    /// 自证会变红:把 `instance_id` 改成 `format!("mullion-{}", process::id())`。
    #[test]
    fn the_instance_id_is_two_numeric_parts() {
        let id = instance_id();
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(parts.len(), 2, "id 不是两段:{id}");
        for p in parts {
            assert!(
                !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()),
                "id 里有非数字段:{id}"
            );
        }
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app --lib logx:: 2>&1 | tail -20
```

Expected: FAIL，`cannot find function 'instance_id' in this scope`

- [ ] **Step 3: 实现**

在 `logx.rs` 的 `log_path()`（第 71 行）**之前**插入：

```rust
/// 本实例的身份,`{毫秒}-{pid}`(F148 的 `new_instance_id`)。
///
/// **在 `logx` 而不是 `App::new` 里生成**:日志文件名要用它,而 `logx::init`
/// 跑在 `App::new` 之前。共用同一个 id 之后,日志文件与 F148 的现场历史
/// 记录一一对应 —— 排障时「崩的是哪个实例、它当时恢复的是哪个现场」不用猜。
///
/// `get_or_init` 而非 `init` 里 `set`:集成测试不会走 `init`,懒生成让
/// 调用顺序无关紧要。
pub fn instance_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        mullion_store::new_instance_id(mullion_store::now_ms(), std::process::id())
    })
}
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p mullion-app --lib logx:: 2>&1 | grep -E "test result|FAILED"
```

Expected: `test result: ok.`

- [ ] **Step 5: App::new 改读它**

把 `crates/mullion-app/src/app.rs:2245-2248` 的

```rust
            instance_id: mullion_store::new_instance_id(
                mullion_store::now_ms(),
                std::process::id(),
            ),
```

替换为

```rust
            // 与日志文件名同源(logx::instance_id):两边共用一个 id,
            // 日志与现场历史记录才对得上号。
            instance_id: crate::logx::instance_id().to_string(),
```

- [ ] **Step 6: 跑全量确认没打断别处**

```bash
cargo test --workspace > /tmp/t1.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/t1.log | grep -v "ok\." | head
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
```

Expected: 没有 FAILED / panicked；clippy 无输出

- [ ] **Step 7: 提交**

```bash
git add crates/mullion-app/src/logx.rs crates/mullion-app/src/app.rs
git commit -m "refactor(logx): instance id 生成点上移到 logx,与现场历史共用 (F166)

日志文件名要用 id,而 logx::init 跑在 App::new 之前。共用同一个 id 之后,
日志文件与 F148 现场历史记录一一对应。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 2: 行前缀带 pid

**Files:**
- Modify: `crates/mullion-app/src/logx.rs`（`write_line_at`，第 256-267 行）

- [ ] **Step 1: 写失败的测试**

在 `logx.rs` 的 `mod tests` 里加：

```rust
    /// 每一行都必须带 pid。
    ///
    /// 一实例一文件之后 pid 看似冗余,但它是**双保险**:日志被改名、被
    /// 拼接、被贴进 issue 之后,文件名那层归属就没了,而排障时最常见的
    /// 动作恰恰是把几个文件拼起来按时间排。
    ///
    /// 自证会变红:把 `format_line` 里的 `[{pid}] ` 去掉。
    #[test]
    fn every_line_carries_the_pid() {
        let line = format_line("2026-08-26T00:00:00Z", 4242, "INFO  mullion: 你好");
        assert!(line.contains("[4242]"), "行里没有 pid:{line}");
        assert!(line.starts_with("[2026-08-26T00:00:00Z]"), "时间戳不在最前:{line}");
        assert!(line.ends_with('\n'), "行尾没有换行:{line:?}");
    }

    /// pid 必须排在时间戳**之后**、正文之前。
    ///
    /// 位置不是审美问题:现有的排障习惯是 `findstr profile` 之后按列读,
    /// pid 插进正文中间会把 profile 行的字段位置整体推移。
    ///
    /// 自证会变红:把 `format_line` 改成 `format!("[{pid}] [{ts}] {msg}\n")`。
    #[test]
    fn the_pid_sits_between_the_timestamp_and_the_message() {
        let line = format_line("TS", 7, "MSG");
        assert_eq!(line, "[TS] [7] MSG\n");
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app --lib logx::tests::every_line_carries_the_pid 2>&1 | tail -10
```

Expected: FAIL，`cannot find function 'format_line'`

- [ ] **Step 3: 实现**

把 `logx.rs` 第 256-267 行整个 `write_line_at` 替换为：

```rust
/// 一行日志的最终形状。**抽成纯函数只为可测**:行格式是多实例排障时
/// 唯一的归属线索,内联在 `format!` 里就只能靠人眼看。
pub fn format_line(ts: &str, pid: u32, msg: &str) -> String {
    format!("[{ts}] [{pid}] {msg}\n")
}

/// 真正落盘:带 UTC 时间戳 + pid,写文件 + stderr。`level` 决定要不要立刻
/// flush(见 [`flush_immediately`])。失败静默(日志绝不能反过来拖垮程序)。
fn write_line_at(msg: &str, level: log::Level) {
    let ts = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default();
    let full = format_line(&ts, std::process::id(), msg);
    let _ = write!(std::io::stderr(), "{full}");
    if let Some(Some(m)) = SINK.get() {
        if let Ok(mut f) = m.lock() {
            emit(&mut *f, &full, level);
        }
    }
}
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p mullion-app --lib logx:: 2>&1 | grep -E "test result|FAILED"
```

Expected: `test result: ok.`

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/logx.rs
git commit -m "feat(logx): 每行日志带 pid,多实例归属双保险 (F166)

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 3: per-instance 文件名 + 严格解析器

**关键坑**：解析器必须严格校验「两段纯数字」。宽松匹配会把 F155 导出的
`mullion-redacted.log` 当成 instance id 为 `redacted` 的陈旧日志，
Task 5 的清理会**把它删掉**。

**Files:**
- Modify: `crates/mullion-app/src/logx.rs`

- [ ] **Step 1: 写失败的测试**

```rust
    /// 文件名 ⇄ instance id 的往返。
    ///
    /// 自证会变红:把 `log_file_name` 里的 `mullion-` 前缀去掉。
    #[test]
    fn a_log_file_name_round_trips_to_its_instance_id() {
        let name = log_file_name("1755000000123-4242");
        assert_eq!(name, "mullion-1755000000123-4242.log");
        assert_eq!(parse_log_name(&name), Some("1755000000123-4242"));
        assert_eq!(
            parse_log_name("mullion-1755000000123-4242.log.1"),
            Some("1755000000123-4242"),
            "轮转出来的 .log.1 必须认得出属于哪个实例,否则它成孤儿"
        );
    }

    /// **解析器必须严格**:只认 F148 的 `{纯数字}-{纯数字}`。
    ///
    /// 宽松匹配会把 F155 导出的 `mullion-redacted.log` 认成 instance id
    /// 为 `redacted` 的日志 —— 它没有心跳,会被判死,然后被清理逻辑
    /// **删掉用户刚导出准备发给我们的那个文件**。
    ///
    /// 老的 `mullion.log`(无 id)也必须返回 None:那是上一版留下的,
    /// 用户可能正开着看,不归我们管。
    ///
    /// 自证会变红:把 `parse_log_name` 里的 `is_instance_id(id)` 判断删掉。
    #[test]
    fn only_a_real_instance_id_is_recognised_so_other_files_are_never_touched() {
        for bad in [
            "mullion-redacted.log",          // F155 导出的脱敏副本
            "mullion-redacted-1-2.log",      // 带 id 的脱敏副本
            "mullion.log",                   // 上一版的遗留日志
            "mullion.log.1",                 // 上一版的遗留轮转
            "mullion-.log",                  // 空 id
            "mullion-abc-def.log",           // 非数字
            "mullion-1-2-3.log",             // 三段
            "notes.txt",                     // 完全无关
        ] {
            assert_eq!(parse_log_name(bad), None, "{bad} 不该被当成实例日志");
        }
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app --lib logx::tests::only_a_real_instance 2>&1 | tail -10
```

Expected: FAIL，`cannot find function 'parse_log_name'`

- [ ] **Step 3: 实现**

在 `logx.rs` 里把 `log_path()`（第 70-73 行）替换为：

```rust
/// 日志文件所在目录。给清理逻辑用。
pub fn log_dir() -> Option<PathBuf> {
    crate::shell::store::config_dir()
}

/// 某个实例的日志文件名。
pub fn log_file_name(instance_id: &str) -> String {
    format!("mullion-{instance_id}.log")
}

/// 这个字符串是不是一个 F148 形状的 instance id(`{纯数字}-{纯数字}`)。
///
/// **严格校验不是洁癖**:配置目录里还躺着 F155 导出的
/// `mullion-redacted.log`。宽松匹配会把它认成 id 为 `redacted` 的实例日志,
/// 判死之后由清理逻辑删掉 —— 删的正是用户刚导出准备发过来的那份。
fn is_instance_id(s: &str) -> bool {
    let mut parts = s.split('-');
    let (Some(a), Some(b), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let numeric = |x: &str| !x.is_empty() && x.bytes().all(|c| c.is_ascii_digit());
    numeric(a) && numeric(b)
}

/// 文件名 → instance id。认 `mullion-<id>.log` 与轮转出来的
/// `mullion-<id>.log.1`;其余(含上一版的 `mullion.log`)一律 `None`。
pub fn parse_log_name(name: &str) -> Option<&str> {
    let rest = name.strip_prefix("mullion-")?;
    let id = rest
        .strip_suffix(".log.1")
        .or_else(|| rest.strip_suffix(".log"))?;
    is_instance_id(id).then_some(id)
}

/// 本实例的日志文件路径:`<config_dir>/mullion-<instance_id>.log`
/// (Windows `%APPDATA%\mullion\config\mullion-<id>.log`)。
///
/// **一实例一文件**:多开时所有实例 append 进同一个文件的话,profile 行里
/// 的 CPU%/GPU%/显存全是 per-process 数字,混流之后会读成「一个进程在
/// 6% 和 94% 之间抽风」—— 比没有日志更糟。
pub fn log_path() -> Option<PathBuf> {
    log_dir().map(|d| d.join(log_file_name(instance_id())))
}
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p mullion-app --lib logx:: 2>&1 | grep -E "test result|FAILED"
cargo clippy -p mullion-app --all-targets -- -D warnings 2>&1 | tail -5
```

Expected: `test result: ok.`；clippy 无输出

- [ ] **Step 5: 启动横幅写明新路径**

`logx.rs` 的 `init` 里，把「启动」那条 `line(&format!(...))`（第 213-216 行）改成：

```rust
        Some(p) => line(&format!(
            "==== mullion {version} 启动;日志: {} (app={app} deps={deps}) ====\n\
             (一实例一文件;上一版的 mullion.log 若还在,已不再写入)",
            p.display()
        )),
```

- [ ] **Step 6: 提交**

```bash
cargo test -p mullion-app --lib logx:: 2>&1 | grep -E "test result"
git add crates/mullion-app/src/logx.rs
git commit -m "feat(logx): 日志改为一实例一文件 mullion-<id>.log (F166)

解析器按 F148 的 id 形状严格校验(两段纯数字),否则 F155 导出的
mullion-redacted.log 会被后续清理逻辑当成陈旧实例日志删掉。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 4: SINK 结构改造 + 运行期轮转

**为什么必须改结构**：文件名唯一之后，「启动时看上次的文件多大」永远不会触发，
长跑实例会无限涨（debug 档那 64MB 上限直接作废）。运行期换文件要求
`SINK` 里的 `Option` 在锁**内**，现在它在锁外，换不了。

**Files:**
- Modify: `crates/mullion-app/src/logx.rs`
- Modify: `crates/mullion-app/src/diag.rs`（`watchdog_loop`，第 642 行附近）

- [ ] **Step 1: 写失败的测试**

```rust
    /// 轮转判据。
    ///
    /// 自证会变红:把 `should_rotate` 改成恒 `false`。
    #[test]
    fn a_file_past_the_limit_wants_to_rotate() {
        assert!(!should_rotate(0, 100));
        assert!(!should_rotate(100, 100), "刚好等于上限不该转");
        assert!(should_rotate(101, 100));
    }

    /// 轮转必须**先关后挪**,而不是对开着的文件 rename。
    ///
    /// 对一个正在写的文件 rename:句柄跟着 inode 走,本进程会继续往
    /// 改名后的 `.log.1` 里写,而新建的主文件永远是空的 —— 症状是
    /// 「日志停在某个时刻不动了」,且完全静默。这正是本切片要修的那个
    /// 多实例老 bug,不能在轮转里以另一种形式重现。
    ///
    /// 这里扎的是**源码结构**:真流程要碰进程唯一的 `SINK` 和真实文件系统,
    /// 单测里跑不动。
    ///
    /// 自证会变红:把 `rotate_now` 里的 `guard.take()` 那行删掉。
    #[test]
    fn rotation_closes_the_file_before_renaming_it() {
        let src = include_str!("logx.rs");
        let body = src
            .split("fn rotate_now(")
            .nth(1)
            .expect("rotate_now 没了?这条测试的锚点失效了")
            .split("\n}\n")
            .next()
            .expect("rotate_now 的函数体没有闭合?");
        let close_at = body.find("guard.take()").expect("轮转没有先关文件");
        let rename_at = body.find("rename").expect("轮转没有 rename");
        assert!(
            close_at < rename_at,
            "先 rename 后关文件 —— 句柄会跟着 inode 走,之后所有日志都写进 .log.1"
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app --lib logx::tests::a_file_past_the_limit 2>&1 | tail -10
```

Expected: FAIL，`cannot find function 'should_rotate'`

- [ ] **Step 3: 改 SINK 结构**

把 `logx.rs` 第 65 行

```rust
static SINK: OnceLock<Option<Mutex<std::io::BufWriter<std::fs::File>>>> = OnceLock::new();
```

替换为

```rust
/// **`Option` 在锁内**是为了运行期轮转:换文件要在持锁时把旧 writer
/// `take()` 出来 drop 掉再放新的,`Option` 在锁外就换不了。
static SINK: OnceLock<Mutex<Option<std::io::BufWriter<std::fs::File>>>> = OnceLock::new();

/// 本实例日志文件的路径,轮转时要用。`init` 之后才有。
static LOG_FILE: OnceLock<PathBuf> = OnceLock::new();
```

- [ ] **Step 4: 改三处 SINK 用法**

`write_line_at` 里（Task 2 刚写的那段）：

```rust
    if let Some(m) = SINK.get() {
        if let Ok(mut g) = m.lock() {
            if let Some(w) = g.as_mut() {
                emit(w, &full, level);
            }
        }
    }
```

`flush_now`（第 283-289 行）整个替换为：

```rust
/// 把缓冲里的日志刷到盘上。`diag` 的周期线程每秒调一次 —— 没有它,
/// info/debug 档下卡死时最后几秒的记录会随进程一起消失。
pub fn flush_now() {
    if let Some(m) = SINK.get() {
        if let Ok(mut g) = m.lock() {
            if let Some(w) = g.as_mut() {
                let _ = w.flush();
            }
        }
    }
}
```

`init` 里（第 187-200 行）整个替换为：

```rust
    let path = log_path();
    let file = path.as_ref().and_then(|p| {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .ok()
            .map(std::io::BufWriter::new)
    });
    let _ = SINK.set(Mutex::new(file));
    if let Some(p) = path.as_ref() {
        let _ = LOG_FILE.set(p.clone());
    }
```

注意：`rotate_if_large` 那次启动时轮转的调用**删掉**（文件名唯一，它永远不触发），
函数本体也删掉——它是本次改动导致变得无用的死代码。

- [ ] **Step 5: 实现运行期轮转**

在 `flush_now` 之后追加：

```rust
/// 这个大小该不该轮转。纯函数,可单测。
pub fn should_rotate(len: u64, limit: u64) -> bool {
    len > limit
}

/// 当前档位对应的轮转上限。`init` 之前按最保守的档算。
fn current_rotate_bytes() -> u64 {
    let app = LOGGER
        .get()
        .map_or(LevelFilter::Info, |l| filter_from_usize(l.app.load(Ordering::Relaxed)));
    rotate_bytes_for(app)
}

/// 日志超限就转一代并重开。**由 `diag` 的看门狗线程每秒调一次**。
///
/// 为什么不在 `write_line_at` 里判:那是帧路径(每帧几条 debug 日志),
/// 一次 `metadata` 系统调用就进了帧预算 —— T3 红线。看门狗线程本来就
/// 每秒醒一次做 flush,顺带查一次大小是免费的。
///
/// 为什么不在启动时判:一实例一文件之后文件名唯一,启动时那个文件永远
/// 是空的,判据永远不成立,64MB 上限形同虚设。
pub fn rotate_if_needed() {
    let Some(path) = LOG_FILE.get() else { return };
    let Some(m) = SINK.get() else { return };
    let Ok(mut guard) = m.lock() else { return };
    if guard.is_none() {
        return;
    }
    let len = std::fs::metadata(path).map_or(0, |md| md.len());
    if !should_rotate(len, current_rotate_bytes()) {
        return;
    }
    rotate_now(&mut guard, path);
}

/// 轮转本体:**先关后挪**。
///
/// 顺序是全部要点。对一个正开着的文件 rename,句柄会跟着 inode 走 ——
/// 本进程继续往改名后的 `.log.1` 里写,新建的主文件永远是空的,症状是
/// 「日志某一刻起停住不动」且完全静默。`take()` 让 `BufWriter` 走 Drop
/// (flush + close),之后 rename 的是一个没人开着的文件。
fn rotate_now(guard: &mut Option<std::io::BufWriter<std::fs::File>>, path: &Path) {
    drop(guard.take());
    let _ = std::fs::rename(path, path.with_extension("log.1"));
    *guard = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
        .map(std::io::BufWriter::new);
}
```

- [ ] **Step 6: 看门狗每秒调它**

`crates/mullion-app/src/diag.rs` 的 `watchdog_loop` 里，把第 642 行

```rust
        crate::logx::flush_now();
```

替换为

```rust
        crate::logx::flush_now();
        // 一实例一文件之后,轮转判据从「启动时」搬到这里 —— 文件名唯一,
        // 启动时那个文件永远是空的。放看门狗而不是写日志的热路径:
        // 一次 metadata 系统调用不能进帧预算(T3)。
        crate::logx::rotate_if_needed();
```

- [ ] **Step 7: 跑测试确认通过**

```bash
cargo test -p mullion-app --lib logx:: 2>&1 | grep -E "test result|FAILED"
cargo clippy -p mullion-app --all-targets -- -D warnings 2>&1 | tail -5
```

Expected: `test result: ok.`；clippy 无输出。若 clippy 报 `Path` 未导入，
`logx.rs` 顶部已有 `use std::path::{Path, PathBuf};`（第 18 行），无需改。

- [ ] **Step 8: 提交**

```bash
git add crates/mullion-app/src/logx.rs crates/mullion-app/src/diag.rs
git commit -m "feat(logx): 轮转从启动时改为运行期,先关后挪 (F166)

文件名唯一后启动时轮转永不触发,长跑实例会无限涨。判据搬进看门狗
每秒的 flush 旁边,不进日志热路径(T3)。轮转本体先 take() 关掉
writer 再 rename —— 对开着的文件 rename 会让句柄跟着 inode 走。

守护测试:logx::tests::rotation_closes_the_file_before_renaming_it

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 5: 陈旧日志清理

**Files:**
- Modify: `crates/mullion-app/src/logx.rs`

- [ ] **Step 1: 写失败的测试**

```rust
    fn lf(name: &str, id: &str, mtime: i64) -> LogFile {
        LogFile {
            path: PathBuf::from(name),
            instance_id: id.to_string(),
            mtime_secs: mtime,
        }
    }

    /// **活着的实例永远不被清理**。
    ///
    /// 这是整个清理逻辑唯一不能错的一条:删掉一个正在被写的日志,用户
    /// 排障时看到的是一个从中间断掉的文件,而断掉的原因不在文件里。
    /// F148 的 `live_instances_are_never_pruned_and_do_not_eat_the_quota`
    /// 是同一条判据。
    ///
    /// 自证会变红:把 `prune_plan` 里 `alive_ids.contains` 那道过滤删掉。
    #[test]
    fn a_live_instance_log_is_never_pruned() {
        let files = vec![
            lf("a.log", "100-1", 1_000),
            lf("b.log", "200-2", 2_000),
            lf("c.log", "300-3", 3_000),
        ];
        let alive = vec!["100-1".to_string()];
        let plan = prune_plan(&files, &alive, "999-9", 9_000, 1);
        assert!(
            !plan.contains(&PathBuf::from("a.log")),
            "活着的实例的日志被列进删除计划了"
        );
    }

    /// **自己的文件按文件名硬排除**,不靠心跳。
    ///
    /// 清理跑在 `logx::init` 里,而 F148 的第一次心跳要等 `App` 跑起来
    /// 之后 —— 此刻自己的 `.alive` 文件还不存在,靠心跳判活会把自己判死。
    ///
    /// 自证会变红:把 `prune_plan` 里 `f.instance_id != self_id` 那道过滤删掉。
    #[test]
    fn our_own_log_is_excluded_by_name_because_our_heartbeat_does_not_exist_yet() {
        let files = vec![lf("me.log", "999-9", 100), lf("old.log", "100-1", 100)];
        let plan = prune_plan(&files, &[], "999-9", 9_000, 0);
        assert!(!plan.contains(&PathBuf::from("me.log")), "把自己的日志删了");
        assert!(plan.contains(&PathBuf::from("old.log")));
    }

    /// **刚动过的文件(60 秒内)一律不删**,作为心跳竞态的第二道保险。
    ///
    /// 另一个实例可能刚启动、还没写第一次心跳;或者刚崩溃 —— 后者的日志
    /// 恰恰是最该留的证据。
    ///
    /// 自证会变红:把 `prune_plan` 里 `now_secs - f.mtime > FRESH_SECS` 改成恒 true。
    #[test]
    fn a_freshly_written_log_is_kept_even_without_a_heartbeat() {
        let files = vec![lf("fresh.log", "100-1", 9_000 - 10)];
        let plan = prune_plan(&files, &[], "999-9", 9_000, 0);
        assert!(plan.is_empty(), "60 秒内动过的日志被删了:{plan:?}");
    }

    /// 主文件与它轮转出来的 `.log.1` **同进退**。
    ///
    /// 分开处理的话,死实例的 `.1` 永远没人删(清理只按主文件名认实例),
    /// 慢性写满盘 —— 而且完全无声。
    ///
    /// 自证会变红:把 `prune_plan` 里的分组改成按 path 而不是按 instance_id。
    #[test]
    fn a_rotated_sibling_goes_with_its_main_file() {
        let files = vec![
            lf("m.log", "100-1", 100),
            lf("m.log.1", "100-1", 90),
            lf("keep.log", "200-2", 5_000),
        ];
        let plan = prune_plan(&files, &[], "999-9", 9_000, 1);
        assert!(plan.contains(&PathBuf::from("m.log")));
        assert!(
            plan.contains(&PathBuf::from("m.log.1")),
            "轮转出来的 .log.1 成了孤儿,永远没人删"
        );
        assert!(!plan.contains(&PathBuf::from("keep.log")), "配额内的被删了");
    }

    /// 配额按**实例**算而不是按文件算,且留最近的。
    ///
    /// 自证会变红:把 `prune_plan` 里的 `sort_by_key` 去掉 Reverse。
    #[test]
    fn the_quota_keeps_the_most_recent_instances() {
        let files = vec![
            lf("old.log", "100-1", 100),
            lf("mid.log", "200-2", 200),
            lf("new.log", "300-3", 300),
        ];
        let plan = prune_plan(&files, &[], "999-9", 9_000, 2);
        assert_eq!(plan, vec![PathBuf::from("old.log")], "留的不是最近两个");
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app --lib logx::tests::a_live_instance 2>&1 | tail -10
```

Expected: FAIL，`cannot find type 'LogFile'`

- [ ] **Step 3: 实现纯函数**

在 `logx.rs` 的 `rotate_now` 之后追加：

```rust
/// 死实例的日志最多留几组。「组」= 一个实例的主文件 + 它轮转出来的 `.log.1`。
const KEEP_DEAD_LOGS: usize = 5;

/// 多新的文件算「刚动过」,一律不删。见 [`prune_plan`] 的竞态说明。
const FRESH_SECS: i64 = 60;

/// 扫出来的一个日志文件。
#[derive(Debug, Clone)]
pub struct LogFile {
    pub path: PathBuf,
    pub instance_id: String,
    pub mtime_secs: i64,
}

/// 算出该删哪些日志文件。**纯函数**:真删盘的那一步只负责按计划执行。
///
/// 三道保险,顺序不能少:
/// 1. `self_id` 按**文件名**硬排除 —— 清理跑在 `init` 里,此刻本实例的
///    F148 心跳文件还不存在(第一次心跳要等 `App` 起来),靠心跳会判死自己。
/// 2. `alive_ids` 里的一律不动 —— 删掉正在被写的日志,用户看到的是一个
///    从中间断掉的文件,而断掉的原因不在文件里。
/// 3. `FRESH_SECS` 内动过的不删 —— 另一个实例可能刚启动还没写心跳,
///    或者刚崩溃(后者的日志恰恰最该留)。
///
/// 分组按 `instance_id` 而非路径:主文件与 `.log.1` 必须同进退,否则死
/// 实例的 `.1` 永远没人认领,慢性写满盘且无声。
pub fn prune_plan(
    files: &[LogFile],
    alive_ids: &[String],
    self_id: &str,
    now_secs: i64,
    keep: usize,
) -> Vec<PathBuf> {
    let mut groups: std::collections::BTreeMap<&str, (i64, Vec<&PathBuf>)> =
        std::collections::BTreeMap::new();
    for f in files {
        if f.instance_id == self_id || alive_ids.iter().any(|a| a == &f.instance_id) {
            continue;
        }
        let e = groups
            .entry(&f.instance_id)
            .or_insert((i64::MIN, Vec::new()));
        e.0 = e.0.max(f.mtime_secs);
        e.1.push(&f.path);
    }
    // 组里**任何**一个文件刚动过,整组都留 —— 主文件新、`.log.1` 旧是常态。
    groups.retain(|_, (mtime, _)| now_secs.saturating_sub(*mtime) > FRESH_SECS);

    let mut ordered: Vec<(&str, (i64, Vec<&PathBuf>))> = groups.into_iter().collect();
    ordered.sort_by_key(|(id, (mtime, _))| (std::cmp::Reverse(*mtime), *id));
    ordered
        .into_iter()
        .skip(keep)
        .flat_map(|(_, (_, paths))| paths.into_iter().cloned())
        .collect()
}
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p mullion-app --lib logx::tests 2>&1 | grep -E "test result|FAILED"
```

Expected: `test result: ok.`

- [ ] **Step 5: 接上真实文件系统**

在 `prune_plan` 之后追加：

```rust
/// 扫目录 + 读 F148 心跳 + 执行清理计划。`init` 里调一次,失败全静默。
fn prune_stale_logs(dir: &Path) {
    let now = mullion_store::now_secs();

    // F148 的心跳文件:`<dir>/layouts/<id>.alive`,内容是一个 Unix 秒。
    let mut alive: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(mullion_store::history_dir(dir)) {
        for e in rd.flatten() {
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(id) = name.strip_suffix(&format!(".{}", mullion_store::ALIVE_EXT)) else {
                continue;
            };
            let hb = std::fs::read_to_string(e.path())
                .ok()
                .and_then(|s| s.trim().parse::<i64>().ok())
                .unwrap_or(i64::MIN);
            if mullion_store::is_alive(now, hb) {
                alive.push(id.to_string());
            }
        }
    }

    let mut files: Vec<LogFile> = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(id) = parse_log_name(name) else { continue };
        let mtime = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs() as i64);
        files.push(LogFile {
            path: e.path(),
            instance_id: id.to_string(),
            mtime_secs: mtime,
        });
    }

    for p in prune_plan(&files, &alive, instance_id(), now, KEEP_DEAD_LOGS) {
        let _ = std::fs::remove_file(p);
    }
}
```

在 `init` 里，`SINK.set(...)` 之后、启动横幅 `match path` 之前插入：

```rust
    // 陈旧日志回收。放在 SINK 建好之后:万一清理本身出事,那条日志还写得出去。
    if let Some(d) = log_dir() {
        prune_stale_logs(&d);
    }
```

- [ ] **Step 6: 跑绿 + clippy**

```bash
cargo test --workspace > /tmp/t5.log 2>&1; grep -nE "FAILED|panicked" /tmp/t5.log | head
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
```

Expected: 无 FAILED / panicked；clippy 无输出

- [ ] **Step 7: 提交**

```bash
git add crates/mullion-app/src/logx.rs
git commit -m "feat(logx): 陈旧实例日志按心跳判活 + 配额回收 (F166)

三道保险:自己按文件名硬排除(init 时心跳还没写)、活实例不动、
60 秒内动过的不删。主文件与 .log.1 按 instance_id 分组同进退,
否则死实例的 .1 成孤儿慢性写满盘。

守护测试:logx::tests::a_live_instance_log_is_never_pruned
          logx::tests::a_rotated_sibling_goes_with_its_main_file

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 6: F155 导出改名 + 文档同步

**Files:**
- Modify: `crates/mullion-app/src/app.rs:2555, 2578`
- Modify: `docs/adr-008-diagnostics.md`

- [ ] **Step 1: 导出副本带 instance id**

`app.rs` 第 2578 行

```rust
                let dst = src.with_file_name("mullion-redacted.log");
```

替换为

```rust
                // 带上 instance id:多开时两个实例都点「导出」的话,固定
                // 文件名会让后点的那个悄悄覆盖前一个。
                let dst = src.with_file_name(format!(
                    "mullion-redacted-{}.log",
                    crate::logx::instance_id()
                ));
```

同时把第 2555 行的文档注释

```rust
    /// F155:把 `mullion.log` 脱敏后另存一份,并把路径告诉用户。
```

改成

```rust
    /// F155:把**本实例的**日志脱敏后另存一份,并把路径告诉用户。
    ///
    /// 只导本实例(F166 之后一实例一文件)。多开时别的实例的日志各自
    /// 独立,需要的话在那边点。
```

- [ ] **Step 2: 提示里说明还有别的实例**

`app.rs` 的 `Ok(dst) => {` 分支里，把

```rust
                let msg = format!("已导出脱敏日志:{}", dst.display());
```

替换为

```rust
                let others = crate::logx::log_dir()
                    .and_then(|d| std::fs::read_dir(d).ok())
                    .map_or(0, |rd| {
                        rd.flatten()
                            .filter(|e| {
                                // `file_name()` 返回的是 OsString(自有值),
                                // 必须先绑定再借 —— 链式写 `e.file_name().to_str()`
                                // 会让 `&str` 借在一个当场析构的临时值上,编译失败。
                                let name = e.file_name();
                                name.to_str()
                                    .and_then(crate::logx::parse_log_name)
                                    .is_some_and(|id| id != crate::logx::instance_id())
                            })
                            .count()
                    });
                let msg = if others > 0 {
                    format!(
                        "已导出脱敏日志:{}(本机还有 {others} 份其他实例的日志)",
                        dst.display()
                    )
                } else {
                    format!("已导出脱敏日志:{}", dst.display())
                };
```

- [ ] **Step 3: 确认脱敏规则没被绕过**

```bash
cargo test -p mullion-app --lib redact 2>&1 | grep -E "test result|FAILED"
```

Expected: `test result: ok.`（新增的全是数字字段，脱敏规则不需要改）

- [ ] **Step 4: 更新 ADR-008**

`docs/adr-008-diagnostics.md` 第 32 行附近，把日志路径那句改成：

```markdown
`<config_dir>/mullion-<instance_id>.log`(F166:一实例一文件;`instance_id`
与 F148 现场历史同源)。轮转在**运行期**由看门狗每秒判一次,超限转一代到
`.log.1`;启动时按心跳判活回收已关实例的日志(留最近 5 组)。
```

第 48 行「启动时若日志超过 8 MB 轮转一代到 `mullion.log.1`」删掉（已被上面那句取代）。

- [ ] **Step 5: 跑绿 + 提交**

```bash
cargo test --workspace > /tmp/t6.log 2>&1; grep -nE "FAILED|panicked" /tmp/t6.log | head
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
git add crates/mullion-app/src/app.rs docs/adr-008-diagnostics.md
git commit -m "feat(app): F155 导出副本带 instance id + 提示其他实例 (F166)

固定文件名会让多开时后点导出的实例悄悄覆盖前一个。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

# 阶段二：F164 CPU

## Task 7: sysprobe 骨架 + cpu_pct 纯函数

**Files:**
- Create: `crates/mullion-app/src/sysprobe.rs`
- Modify: `crates/mullion-app/src/lib.rs`

- [ ] **Step 1: 建模块并挂上**

创建 `crates/mullion-app/src/sysprobe.rs`：

```rust
//! 进程级资源探针:CPU 时间、GPU 引擎占用率、显存。
//!
//! **为什么不在 `diag.rs` 里**:那个文件已经 730 行,再塞三套平台 FFI
//! 会失控。这里的分工是「平台相关的采集」+「平台无关的换算」,换算部分
//! 是纯函数、能单测,FFI 只留薄壳。
//!
//! **调用方只有看门狗线程**(`diag::watchdog_loop`,每 5 秒一次),
//! 所以这里的一切都不在帧路径上,可以放心做系统调用。
//!
//! 非 Windows / 探针不可用 / 首次采样无基线 —— 一律返回 `None`,
//! 由 `profile::render_line` 渲染成 `n/a`。**不许编一个 0 出来**:
//! 「采不到」和「真的是 0」在排障时是两回事。

/// 一次 CPU 采样。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuSample {
    /// 整个进程的 CPU 占用,**按核数归一**(所有核跑满 = 100)。
    pub process_pct: u8,
    /// 主线程的 CPU 占用,**不归一**(一个核跑满 = 100)。
    pub main_thread_pct: u8,
}

/// CPU 时间差 → 百分比。
///
/// `cores` 是归一化的除数:进程口径传真实核数,主线程口径传 1。
///
/// **两个口径故意不同**。F158 那次故障的症状原文是「空闲不再烧满一个核」,
/// 在 16 核机器上按核数归一之后它只有 6% —— 淹没在噪声里,而这个功能存在
/// 的全部理由就是让它跳出来。主线程不归一,一个核跑满就是 100%。
///
/// `window_ns` 为 0(时钟没走 / 首次采样无基线)返回 `None`,不是 0 ——
/// 「采不到」和「真的是 0」在排障时是两回事,而且 `None` 不会打破空闲门。
pub fn cpu_pct(delta_ns: u64, window_ns: u64, cores: u32) -> Option<u8> {
    if window_ns == 0 || cores == 0 {
        return None;
    }
    let denom = (window_ns as u128) * (cores as u128);
    let pct = (delta_ns as u128) * 100 / denom;
    Some(pct.min(100) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 进程口径按核数归一,主线程口径不归一。
    ///
    /// 这是本模块唯一一条「写错了也全绿、只有真机看得出」的判据:
    /// 两个口径混用的话,「烧满一个核」在多核机上会被压成个位数百分比。
    ///
    /// 自证会变红:把 `cpu_pct` 里的 `* (cores as u128)` 去掉。
    #[test]
    fn the_process_is_normalised_by_cores_while_the_main_thread_is_not() {
        // 一个核被跑满一整个窗口。
        let window = 5_000_000_000u64; // 5s
        let one_core = 5_000_000_000u64;
        assert_eq!(
            cpu_pct(one_core, window, 16),
            Some(6),
            "16 核机上跑满一个核 ≈ 6%(进程口径)"
        );
        assert_eq!(
            cpu_pct(one_core, window, 1),
            Some(100),
            "主线程口径下跑满一个核就是 100%"
        );
    }

    /// 超出 100 要夹紧,不能溢出成小数字。
    ///
    /// `GetProcessTimes` 在多核上很容易给出 > window 的累计值(多线程并行),
    /// 不夹紧的话 u8 转换会回绕 —— 200% 变成一个看起来正常的数。
    ///
    /// 自证会变红:把 `.min(100)` 删掉。
    #[test]
    fn a_multi_core_burst_is_clamped_instead_of_wrapping() {
        assert_eq!(cpu_pct(40_000_000_000, 5_000_000_000, 1), Some(100));
    }

    /// 采不到时是 `None` 而不是 0。
    ///
    /// 0 会被空闲门读成「真空闲」,而 `None` 不打破空闲门也不冒充数据。
    ///
    /// 自证会变红:把 `cpu_pct` 的两处 `return None` 改成 `return Some(0)`。
    #[test]
    fn an_unusable_window_yields_nothing_rather_than_a_fake_zero() {
        assert_eq!(cpu_pct(1_000, 0, 4), None);
        assert_eq!(cpu_pct(1_000, 5_000_000_000, 0), None);
    }
}
```

在 `crates/mullion-app/src/lib.rs` 里，按字母序把 `pub mod sysprobe;` 加进模块列表
（找到 `pub mod store;` 或相邻的 `pub mod` 块，插在 `pub mod redact;` 之后一类的位置）。

- [ ] **Step 2: 跑测试**

```bash
cargo test -p mullion-app --lib sysprobe:: 2>&1 | grep -E "test result|FAILED"
cargo clippy -p mullion-app --all-targets -- -D warnings 2>&1 | tail -5
```

Expected: `test result: ok. 3 passed`；clippy 无输出

- [ ] **Step 3: 提交**

```bash
git add crates/mullion-app/src/sysprobe.rs crates/mullion-app/src/lib.rs
git commit -m "feat(sysprobe): CPU 百分比换算纯函数,进程归一/主线程不归一 (F164)

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 8: 平台 CPU 采样

**关键坑**：`GetCurrentThread()` 返回的是**伪句柄**，传给看门狗线程后会指向
看门狗自己 —— 静默错值，不报错。必须在主线程上 `DuplicateHandle` 拿真句柄。

**Files:**
- Modify: `crates/mullion-app/src/sysprobe.rs`
- Modify: `crates/mullion-app/Cargo.toml`

- [ ] **Step 1: 加 Windows feature**

`crates/mullion-app/Cargo.toml` 的 `windows-sys` features 里追加两项（`Win32_System_Threading` 已有）：

```toml
windows-sys = { version = "0.59", features = [
    "Win32_System_Console",
    "Win32_System_ProcessStatus",
    "Win32_System_SystemInformation",
    "Win32_System_Threading",
    # F165:PDH 性能计数器,读 GPU 引擎占用率。
    "Win32_System_Performance",
    # F164:DuplicateHandle 拿主线程真句柄(GetCurrentThread 是伪句柄)。
    "Win32_Foundation",
] }
```

- [ ] **Step 2: 实现探针**

在 `sysprobe.rs` 的 `cpu_pct` 之后、`mod tests` 之前插入：

```rust
/// CPU 探针。**有状态**:百分比是两次采样的差分,必须记住上一窗口。
///
/// 由看门狗线程持有。`main_thread` 句柄必须由**主线程**在 `new_on_main_thread`
/// 里取好传进来。
pub struct CpuProbe {
    prev_process_ns: Option<u64>,
    prev_main_ns: Option<u64>,
    cores: u32,
    #[cfg(windows)]
    main_thread: Option<MainThreadHandle>,
    #[cfg(target_os = "linux")]
    main_tid: u32,
}

/// 主线程句柄的自有拷贝。
///
/// **不能存 `GetCurrentThread()`**:那是个伪句柄(常量 `-2`),含义是
/// 「调用它的那个线程」—— 存进结构体传给看门狗线程之后,它指的是**看门狗
/// 线程自己**。症状是主线程 CPU% 恒等于零点几,而事件循环正忙转。
/// 静默错值,没有任何报错。
#[cfg(windows)]
struct MainThreadHandle(windows_sys::Win32::Foundation::HANDLE);

// SAFETY: HANDLE 是个内核对象句柄,跨线程使用是 Win32 的正常用法;
// 这里只读(GetThreadTimes),不改状态。
#[cfg(windows)]
unsafe impl Send for MainThreadHandle {}

impl CpuProbe {
    /// **必须在主线程上调用**(`main` / `start_watchdog` 里),之后把
    /// 整个 probe move 进看门狗线程。
    pub fn new_on_main_thread() -> Self {
        Self {
            prev_process_ns: None,
            prev_main_ns: None,
            cores: std::thread::available_parallelism().map_or(1, |n| n.get() as u32),
            #[cfg(windows)]
            main_thread: dup_current_thread(),
            #[cfg(target_os = "linux")]
            main_tid: std::process::id(),
        }
    }

    /// 采一次。首次调用没有基线,返回 `None`。
    pub fn sample(&mut self, window_ns: u64) -> Option<CpuSample> {
        let (proc_ns, main_ns) = read_cpu_ns(self)?;
        let d_proc = self.prev_process_ns.map(|p| proc_ns.saturating_sub(p));
        let d_main = self.prev_main_ns.map(|p| main_ns.saturating_sub(p));
        self.prev_process_ns = Some(proc_ns);
        self.prev_main_ns = Some(main_ns);
        Some(CpuSample {
            process_pct: cpu_pct(d_proc?, window_ns, self.cores)?,
            main_thread_pct: cpu_pct(d_main?, window_ns, 1)?,
        })
    }
}

#[cfg(windows)]
fn dup_current_thread() -> Option<MainThreadHandle> {
    use windows_sys::Win32::Foundation::{DuplicateHandle, DUPLICATE_SAME_ACCESS};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentThread};
    let mut out = std::ptr::null_mut();
    // SAFETY: 全部实参都是当前进程/线程的伪句柄与本地栈变量。
    let ok = unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            GetCurrentThread(),
            GetCurrentProcess(),
            &mut out,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    };
    (ok != 0).then_some(MainThreadHandle(out))
}

/// 返回 (进程累计 CPU 纳秒, 主线程累计 CPU 纳秒)。
#[cfg(windows)]
fn read_cpu_ns(p: &CpuProbe) -> Option<(u64, u64)> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetProcessTimes, GetThreadTimes,
    };

    // FILETIME 的单位是 100 纳秒。
    fn ns(ft: FILETIME) -> u64 {
        (((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64) * 100
    }

    let mut c = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let mut e = c;
    let mut k = c;
    let mut u = c;
    // SAFETY: 四个 out 参数都是本地栈上的 FILETIME。
    let ok = unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) };
    if ok == 0 {
        return None;
    }
    let proc_ns = ns(k) + ns(u);

    let main_ns = match &p.main_thread {
        Some(h) => {
            let mut tk = c;
            let mut tu = c;
            // SAFETY: `h.0` 是 DuplicateHandle 给的自有句柄,四个 out 参数在栈上。
            let ok = unsafe { GetThreadTimes(h.0, &mut c, &mut e, &mut tk, &mut tu) };
            if ok == 0 {
                return None;
            }
            ns(tk) + ns(tu)
        }
        None => return None,
    };
    Some((proc_ns, main_ns))
}

#[cfg(target_os = "linux")]
fn read_cpu_ns(p: &CpuProbe) -> Option<(u64, u64)> {
    let hz = 100u64; // Linux 上 USER_HZ 恒为 100(内核 ABI,不随 CONFIG_HZ 变)
    let read = |path: &str| -> Option<u64> {
        let s = std::fs::read_to_string(path).ok()?;
        // comm 字段可能含空格和括号,从最后一个 ')' 之后开始切。
        let rest = &s[s.rfind(')')? + 1..];
        let f: Vec<&str> = rest.split_whitespace().collect();
        // 从 ')' 之后数:索引 0 = state(第 3 字段),故 utime(14) = 索引 11。
        let utime: u64 = f.get(11)?.parse().ok()?;
        let stime: u64 = f.get(12)?.parse().ok()?;
        Some((utime + stime) * 1_000_000_000 / hz)
    };
    let proc_ns = read("/proc/self/stat")?;
    // Linux 上主线程的 tid == pid。
    let main_ns = read(&format!("/proc/self/task/{}/stat", p.main_tid))?;
    Some((proc_ns, main_ns))
}

#[cfg(not(any(windows, target_os = "linux")))]
fn read_cpu_ns(_p: &CpuProbe) -> Option<(u64, u64)> {
    None
}
```

- [ ] **Step 3: 加一条本平台的合理性测试**

在 `sysprobe.rs` 的 `mod tests` 里追加：

```rust
    /// 本平台真的采得到 CPU 时间,且第二次采样能算出百分比。
    ///
    /// 只测纯函数是不够的:`read_cpu_ns` 的字段下标错一位(Linux 的
    /// `/proc/self/stat` 尤其容易,comm 里带空格会把 split 打乱)会让
    /// 数字变成一个看起来正常的错值。
    ///
    /// 自证会变红:把 Linux 分支的 `f.get(11)` 改成 `f.get(10)`
    /// (那是 cminflt,只会在 fork 时变,烧 CPU 也不涨)。
    #[test]
    #[cfg(any(windows, target_os = "linux"))]
    fn this_platform_reports_cpu_time_that_actually_grows_when_we_burn_cpu() {
        let mut p = CpuProbe::new_on_main_thread();
        assert_eq!(p.sample(1_000_000_000), None, "首次采样没有基线,该是 None");
        // 在主线程上烧掉一小段真实 CPU。
        let start = std::time::Instant::now();
        let mut x = 0u64;
        while start.elapsed() < std::time::Duration::from_millis(150) {
            x = x.wrapping_add(1);
        }
        std::hint::black_box(x);
        let window_ns = start.elapsed().as_nanos() as u64;
        let s = p.sample(window_ns).expect("第二次采样该有值");
        assert!(
            s.main_thread_pct > 50,
            "刚把主线程跑满 150ms,主线程口径只报了 {}%",
            s.main_thread_pct
        );
    }
```

- [ ] **Step 4: 跑测试**

```bash
cargo test -p mullion-app --lib sysprobe:: 2>&1 | grep -E "test result|FAILED"
cargo clippy -p mullion-app --all-targets -- -D warnings 2>&1 | tail -5
```

Expected: `test result: ok. 4 passed`；clippy 无输出

- [ ] **Step 5: 交叉编译验 Windows 分支**

```bash
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app 2>&1 | tail -20
```

Expected: `Finished`。**这一步不能跳** —— `#[cfg(windows)]` 的代码在
Linux 上 `cargo test`/`clippy` 完全看不到，编译错误只有这里能暴露。

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-app/src/sysprobe.rs crates/mullion-app/Cargo.toml
git commit -m "feat(sysprobe): 进程与主线程 CPU 时间采样 (F164)

主线程句柄用 DuplicateHandle 在主线程上取真句柄 —— GetCurrentThread()
是伪句柄,存进结构体给看门狗线程用会指向看门狗自己(静默错值)。

守护测试:sysprobe::tests::this_platform_reports_cpu_time_that_actually_grows_when_we_burn_cpu

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 9: CPU 接进 Snapshot + 空闲门

**Files:**
- Modify: `crates/mullion-app/src/profile.rs`
- Modify: `crates/mullion-app/src/diag.rs`

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-app/src/profile.rs` 的 `mod tests` 里加：

```rust
    /// **CPU 超阈值必须打破空闲门**。
    ///
    /// 这是 F164 存在的理由。F158 那次是「看着空闲、实则烧满一个核」——
    /// 旧的 `is_idle` 只看帧/字节/按键,那种窗口一行都不写,故障在日志里
    /// 完全不存在。
    ///
    /// 自证会变红:把 `is_idle` 里 CPU 那两条判断删掉。
    #[test]
    fn a_window_that_looks_idle_but_burns_cpu_still_gets_written() {
        let mut s = Snapshot::empty();
        s.window_ms = 5_000;
        assert!(s.is_idle(), "全零快照该算空闲");

        s.main_cpu_pct = Some(96);
        assert!(
            !s.is_idle(),
            "主线程烧满一个核却仍判空闲 —— 这一行不会写盘,故障在日志里不存在"
        );

        s.main_cpu_pct = None;
        s.cpu_pct = Some(40);
        assert!(!s.is_idle(), "进程 CPU 40% 却仍判空闲");
    }

    /// **采不到(None)不打破空闲门**。
    ///
    /// 探针不可用时如果算作「忙」,空闲的 mullion 会每 5 秒写一次盘 ——
    /// 正是 `is_idle` 这条判据当初要防的事(笔记本硬盘永远睡不下去)。
    ///
    /// 自证会变红:把 `is_some_and` 改成 `is_none_or`。
    #[test]
    fn a_cpu_probe_that_reports_nothing_does_not_wake_the_disk() {
        let mut s = Snapshot::empty();
        s.window_ms = 5_000;
        s.cpu_pct = None;
        s.main_cpu_pct = None;
        assert!(s.is_idle(), "采不到 CPU 被当成了忙");
    }

    /// 真空闲(CPU 接近 0)照旧不写盘。
    ///
    /// 自证会变红:把 `IDLE_CPU_PCT` 改成 0。
    #[test]
    fn a_genuinely_idle_window_is_still_skipped() {
        let mut s = Snapshot::empty();
        s.window_ms = 5_000;
        s.cpu_pct = Some(0);
        s.main_cpu_pct = Some(1);
        assert!(s.is_idle(), "真空闲也写盘了,硬盘睡不下去");
    }

    /// 渲染行里带 CPU,采不到时是 `n/a` 而不是 0。
    ///
    /// 自证会变红:把 `fmt_pct` 的 `None` 分支改成返回 `"0%"`。
    #[test]
    fn the_line_shows_cpu_and_says_n_a_when_it_could_not_be_read() {
        let mut s = Snapshot::empty();
        s.window_ms = 5_000;
        s.frames = 10;
        s.cpu_pct = Some(8);
        s.main_cpu_pct = Some(96);
        let line = render_line(&s).expect("非空闲窗口该出行");
        assert!(line.contains("cpu=8%/主线程:96%"), "行里没有 CPU:{line}");

        s.cpu_pct = None;
        s.main_cpu_pct = None;
        let line = render_line(&s).expect("非空闲窗口该出行");
        assert!(
            line.contains("cpu=n/a"),
            "采不到时该报 n/a 而不是编一个 0:{line}"
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app --lib profile::tests::a_window_that_looks_idle 2>&1 | tail -10
```

Expected: FAIL，`no field 'main_cpu_pct' on type 'Snapshot'`

- [ ] **Step 3: Snapshot 加字段**

`profile.rs` 里，在 `pub mem_process_mb: u64,`（第 245 行）之后加：

```rust
    /// F164:整个进程的 CPU 占用,**按核数归一**。`None` = 采不到。
    pub cpu_pct: Option<u8>,
    /// F164:主线程的 CPU 占用,**不归一**(一个核跑满 = 100)。
    ///
    /// 与 `cpu_pct` 口径不同是有意的:F158 那次的症状是「烧满一个核」,
    /// 在多核机上归一化之后只有个位数,会淹没在噪声里。
    pub main_cpu_pct: Option<u8>,
```

`Snapshot::empty()` 里，在 `mem_process_mb: 0,` 之后加：

```rust
            cpu_pct: None,
            main_cpu_pct: None,
```

- [ ] **Step 4: 加阈值常量与空闲门**

在 `impl Snapshot` **之前**加：

```rust
/// F164:进程 CPU 超过这个百分比(按核数归一)就不算空闲。
const IDLE_CPU_PCT: u8 = 5;

/// F164:主线程 CPU 超过这个百分比(不归一)就不算空闲。
///
/// 比进程阈值高:主线程本来就承担事件循环,偶尔的一次唤醒会打到十几。
/// 20 以上意味着事件循环在真忙 —— 那正是要抓的。
const IDLE_MAIN_CPU_PCT: u8 = 20;
```

把 `is_idle` 的函数体最后一行 `&& self.sync_timeouts == 0` 改成：

```rust
            && self.sync_timeouts == 0
            && !self.cpu_is_busy()
```

并在 `is_idle` 之后加：

```rust
    /// F164:CPU 读数说明这一窗口其实在干活。
    ///
    /// **`is_some_and` 不是 `is_none_or`**:探针采不到(`None`)时必须算
    /// 「不忙」。反过来的话,任何一台读不到 CPU 的机器上,空闲的 mullion
    /// 会每 5 秒写一次盘 —— 正是 `is_idle` 这条判据当初要防的事。
    fn cpu_is_busy(&self) -> bool {
        self.cpu_pct.is_some_and(|p| p >= IDLE_CPU_PCT)
            || self.main_cpu_pct.is_some_and(|p| p >= IDLE_MAIN_CPU_PCT)
    }
```

同时把 `is_idle` 的文档注释里追加一段：

```rust
    /// **CPU 是唯一一个例外的状态量**：`tabs`/`mem` 那些空闲时也非零,拿它们
    /// 判活会让空闲的 mullion 每 5 秒写一次盘。但 CPU 不同 —— 空闲时它本该
    /// 接近零,非零恰恰说明「看着空闲、实则在烧」(F158),那正是最需要落盘的
    /// 一种窗口。阈值把两者分开。
```

- [ ] **Step 5: 渲染**

在 `render_line` 之前加：

```rust
/// 百分比渲染。`None` → `n/a`(不是 0:「采不到」和「真的是 0」是两回事)。
fn fmt_pct(v: Option<u8>) -> String {
    v.map_or_else(|| "n/a".to_string(), |p| format!("{p}%"))
}
```

把 `render_line` 的格式串里 `mem={}MB` 改成 `mem={}MB cpu={}`，
并在参数列表 `s.mem_process_mb,` 之后加：

```rust
        match (s.cpu_pct, s.main_cpu_pct) {
            (None, None) => "n/a".to_string(),
            (a, b) => format!("{}/主线程:{}", fmt_pct(a), fmt_pct(b)),
        },
```

- [ ] **Step 6: 跑测试确认通过**

```bash
cargo test -p mullion-app --lib profile:: 2>&1 | grep -E "test result|FAILED"
```

Expected: `test result: ok.`

- [ ] **Step 7: 看门狗接线**

`diag.rs` 的 `start_watchdog`（第 578 行附近），在 `let spawned = std::thread::Builder::new()` **之前**加：

```rust
    // **必须在这里建**:`start_watchdog` 由 `main` 在主线程上调用,而
    // `CpuProbe` 要在主线程上 DuplicateHandle 拿主线程的真句柄。
    // 搬进 watchdog_loop 里建的话,拿到的是看门狗线程自己(静默错值)。
    let cpu = crate::sysprobe::CpuProbe::new_on_main_thread();
```

把闭包改成 move 进去：

```rust
        .spawn(move || watchdog_loop(stall_ms, cpu));
```

`watchdog_loop` 签名改为：

```rust
fn watchdog_loop(stall_ms: u64, mut cpu: crate::sysprobe::CpuProbe) {
```

把周期指标那段（第 625-638 行）改成：

```rust
        if now_us.saturating_sub(last_metrics) >= METRICS_EVERY_MS * 1000 {
            let window_ms = now_us.saturating_sub(last_metrics) / 1000;
            last_metrics = now_us;
            let cpu_sample = cpu.sample(window_ms.saturating_mul(1_000_000));
            // **无条件** drain:计数器的语义必须是「这一窗口」,不能取决于日志
            // 档位。挂在门里的话,error 档下计数器一路累积,而停滞报警行读的
            // 是同一批 static —— 同一个数字在不同档位下含义不同,是排障时
            // 最坏的一类坑。渲染(格式化)才是贵的那步,只有它需要关在门里。
            let mut snap = take_snapshot(window_ms);
            snap.cpu_pct = cpu_sample.map(|c| c.process_pct);
            snap.main_cpu_pct = cpu_sample.map(|c| c.main_thread_pct);
            if log::log_enabled!(target: "mullion", log::Level::Info) {
                if let Some(line) = crate::profile::render_line(&snap) {
                    log::info!(target: "mullion", "{line}");
                }
            }
        }
```

- [ ] **Step 8: 跑绿 + 交叉编译 + 提交**

```bash
cargo test --workspace > /tmp/t9.log 2>&1; grep -nE "FAILED|panicked" /tmp/t9.log | head
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app 2>&1 | tail -5
git add crates/mullion-app/src/profile.rs crates/mullion-app/src/diag.rs
git commit -m "feat(profile): profile 行带进程/主线程 CPU%,超阈值打破空闲门 (F164)

空闲门是这个功能的理由:F158 那种「看着空闲、实则烧满一个核」的窗口
在旧判据下一行都不写。采不到时是 None,既不冒充数据也不唤醒硬盘。

守护测试:profile::tests::a_window_that_looks_idle_but_burns_cpu_still_gets_written
          profile::tests::a_cpu_probe_that_reports_nothing_does_not_wake_the_disk

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

# 阶段三：F165 GPU

## Task 10: PDH 实例名解析纯函数

**Files:**
- Modify: `crates/mullion-app/src/sysprobe.rs`

- [ ] **Step 1: 写失败的测试**

在 `sysprobe.rs` 的 `mod tests` 里加：

```rust
    /// PDH 的 GPU Engine 实例名解析。
    ///
    /// 真实形状(Windows 11,任务管理器读的是同一批计数器):
    /// `pid_1234_luid_0x00000000_0x0000C4C1_phys_0_eng_0_engtype_3D`
    ///
    /// 这一段是本切片里最容易写错、又最难发现的:解析错了 `gpu=` 恒为
    /// `0%` 或 `n/a`,而那和「这台机器真的没在用 GPU」长得一模一样。
    ///
    /// 自证会变红:把 `engine_of` 里的 `_engtype_` 改成 `_eng_`。
    #[test]
    fn a_gpu_engine_instance_name_yields_its_engine_type() {
        let n = "pid_1234_luid_0x00000000_0x0000C4C1_phys_0_eng_0_engtype_3D";
        assert_eq!(engine_of(n, 1234), Some("3D"));
        assert_eq!(
            engine_of("pid_1234_luid_0x0_0x1_phys_0_eng_2_engtype_VideoDecode", 1234),
            Some("VideoDecode")
        );
    }

    /// **别的进程的实例必须被滤掉**。
    ///
    /// 不滤的话报出来的是整机 GPU 占用 —— 排障时会把「另一个程序在渲染」
    /// 读成「mullion 在烧 GPU」。
    ///
    /// `pid_12345` 不能被 `pid_1234` 前缀匹配上:那是 10 倍的邻居 pid,
    /// 这种串号比不匹配更难查。
    ///
    /// 自证会变红:把 `engine_of` 里的前缀改成 `format!("pid_{pid}")`(少个下划线)。
    #[test]
    fn another_process_engine_is_filtered_out_including_the_prefix_neighbour() {
        let other = "pid_9999_luid_0x0_0x1_phys_0_eng_0_engtype_3D";
        assert_eq!(engine_of(other, 1234), None);
        let neighbour = "pid_12345_luid_0x0_0x1_phys_0_eng_0_engtype_3D";
        assert_eq!(
            engine_of(neighbour, 1234),
            None,
            "pid_12345 被 pid_1234 前缀匹配上了 —— 串号比不匹配更难查"
        );
    }

    /// 按引擎类型聚合求和,倒序取前两名。
    ///
    /// 求和而非取最大:同一个 engtype 在多个 `eng_N` 实例上各报一部分,
    /// 取最大会系统性低报。
    ///
    /// 自证会变红:把 `aggregate_engines` 里的 `+=` 改成 `=`。
    #[test]
    fn engines_of_the_same_type_are_summed_and_the_top_two_win() {
        let items = vec![
            ("pid_7_luid_a_b_phys_0_eng_0_engtype_3D".to_string(), 8.0),
            ("pid_7_luid_a_b_phys_0_eng_1_engtype_3D".to_string(), 6.0),
            ("pid_7_luid_a_b_phys_0_eng_2_engtype_Copy".to_string(), 3.0),
            ("pid_7_luid_a_b_phys_0_eng_3_engtype_VideoDecode".to_string(), 0.0),
            ("pid_8_luid_a_b_phys_0_eng_0_engtype_3D".to_string(), 90.0),
        ];
        let got = aggregate_engines(&items, 7);
        assert_eq!(
            got,
            vec![("3D".to_string(), 14), ("Copy".to_string(), 3)],
            "同类型没求和 / 没倒序 / 零值没滤掉 / 别的 pid 混进来了"
        );
    }

    /// 百分比要夹紧到 100:多引擎求和很容易超。
    ///
    /// 自证会变红:把 `aggregate_engines` 里的 `.min(100.0)` 删掉。
    #[test]
    fn a_summed_utilisation_over_one_hundred_is_clamped() {
        let items = vec![
            ("pid_7_luid_a_b_phys_0_eng_0_engtype_3D".to_string(), 80.0),
            ("pid_7_luid_a_b_phys_0_eng_1_engtype_3D".to_string(), 70.0),
        ];
        assert_eq!(aggregate_engines(&items, 7), vec![("3D".to_string(), 100)]);
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app --lib sysprobe::tests::a_gpu_engine_instance 2>&1 | tail -10
```

Expected: FAIL，`cannot find function 'engine_of'`

- [ ] **Step 3: 实现（纯函数，非 Windows 也编译，才测得动）**

在 `sysprobe.rs` 的 `CpuProbe` 之后插入：

```rust
/// 一次 GPU 引擎占用采样。`engines` 已按占用倒序,最多两项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuSample {
    pub engines: Vec<(String, u8)>,
}

/// PDH 的 `\GPU Engine(*)` 实例名 → 本进程的引擎类型。
///
/// 实例名形如
/// `pid_1234_luid_0x00000000_0x0000C4C1_phys_0_eng_0_engtype_3D`。
///
/// 前缀带尾随下划线是必须的:`pid_1234` 会前缀匹配上 `pid_12345`,
/// 把邻居进程的 GPU 占用算到自己头上 —— 串号比不匹配难查得多。
///
/// **纯函数,且不在 `#[cfg(windows)]` 里**:Linux 上也编译,这样解析
/// 逻辑在开发机上就测得动。真正碰 PDH 的部分才 gate。
pub fn engine_of(instance: &str, pid: u32) -> Option<&str> {
    let rest = instance.strip_prefix(&format!("pid_{pid}_"))?;
    let at = rest.rfind("_engtype_")?;
    let ty = &rest[at + "_engtype_".len()..];
    (!ty.is_empty()).then_some(ty)
}

/// 一批 (实例名, 占用率) → 本进程按引擎类型聚合的前两名。
///
/// **求和而非取最大**:同一个 engtype 会在多个 `eng_N` 实例上各报一部分,
/// 取最大会系统性低报。求和之后可能超 100(多引擎并行),夹紧。
///
/// 只取前两名:全列出来一行放不下,而排在后面的恒定是零。
pub fn aggregate_engines(items: &[(String, f64)], pid: u32) -> Vec<(String, u8)> {
    let mut by_type: std::collections::BTreeMap<&str, f64> = std::collections::BTreeMap::new();
    for (name, v) in items {
        if let Some(ty) = engine_of(name, pid) {
            *by_type.entry(ty).or_insert(0.0) += v;
        }
    }
    let mut out: Vec<(String, u8)> = by_type
        .into_iter()
        .filter(|(_, v)| *v >= 0.5)
        .map(|(k, v)| (k.to_string(), v.clamp(0.0, 100.0).round() as u8))
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out.truncate(2);
    out
}
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p mullion-app --lib sysprobe:: 2>&1 | grep -E "test result|FAILED"
cargo clippy -p mullion-app --all-targets -- -D warnings 2>&1 | tail -5
```

Expected: `test result: ok. 8 passed`；clippy 无输出

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/sysprobe.rs
git commit -m "feat(sysprobe): PDH GPU Engine 实例名解析与聚合 (F165)

解析逻辑不 gate 在 cfg(windows) 里,这样在 Linux 开发机上就测得动 ——
解析错了 gpu= 恒为 0%,跟「真的没用 GPU」长得一模一样。

守护测试:sysprobe::tests::another_process_engine_is_filtered_out_including_the_prefix_neighbour

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 11: PDH 探针接线

**Files:**
- Modify: `crates/mullion-app/src/sysprobe.rs`
- Modify: `crates/mullion-app/src/diag.rs`
- Modify: `crates/mullion-app/src/profile.rs`

- [ ] **Step 1: 实现 GpuProbe**

在 `sysprobe.rs` 的 `aggregate_engines` 之后插入：

```rust
/// GPU 引擎占用探针。**有状态**:PDH 是速率型计数器,查询句柄必须常驻,
/// 且第一次 `PdhCollectQueryData` 只作基线、不出数。
pub struct GpuProbe {
    #[cfg(windows)]
    inner: Option<PdhQuery>,
    #[cfg(windows)]
    primed: bool,
}

#[cfg(windows)]
struct PdhQuery {
    query: isize,
    counter: isize,
}

// SAFETY: PDH 句柄是进程级的不透明整数,跨线程使用是 PDH 的正常用法;
// 本结构体只被看门狗线程独占持有。
#[cfg(windows)]
unsafe impl Send for PdhQuery {}

impl GpuProbe {
    pub fn new() -> Self {
        Self {
            #[cfg(windows)]
            inner: open_pdh(),
            #[cfg(windows)]
            primed: false,
        }
    }

    /// 采一次。首次调用只作基线,返回 `None`。
    #[cfg(windows)]
    pub fn sample(&mut self) -> Option<GpuSample> {
        let q = self.inner.as_ref()?;
        // SAFETY: `q.query` 由 PdhOpenQueryW 得到,本结构体存活期间有效。
        if unsafe { windows_sys::Win32::System::Performance::PdhCollectQueryData(q.query) } != 0 {
            return None;
        }
        if !self.primed {
            // 速率型计数器要两次采集才有值,第一次只是基线。
            self.primed = true;
            return None;
        }
        let items = read_counter_array(q.counter)?;
        Some(GpuSample {
            engines: aggregate_engines(&items, std::process::id()),
        })
    }

    #[cfg(not(windows))]
    pub fn sample(&mut self) -> Option<GpuSample> {
        None
    }
}

impl Default for GpuProbe {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn open_pdh() -> Option<PdhQuery> {
    use windows_sys::Win32::System::Performance::{PdhAddEnglishCounterW, PdhOpenQueryW};
    let mut query = 0isize;
    // SAFETY: 两个 out 参数在栈上;`wide` 给的是 NUL 结尾的 UTF-16。
    if unsafe { PdhOpenQueryW(std::ptr::null(), 0, &mut query) } != 0 {
        return None;
    }
    let mut counter = 0isize;
    // **必须是 `PdhAddEnglishCounterW`**:`PdhAddCounterW` 吃的是**本地化**
    // 计数器名,中文 Windows 上这条路径根本找不到 —— 而且是运行期静默失败,
    // 编译和本机测试全绿。
    let path = wide(r"\GPU Engine(*)\Utilization Percentage");
    if unsafe { PdhAddEnglishCounterW(query, path.as_ptr(), 0, &mut counter) } != 0 {
        return None;
    }
    Some(PdhQuery { query, counter })
}

/// 读一次计数器数组。两趟调用:先问要多大缓冲,再取数据。
#[cfg(windows)]
fn read_counter_array(counter: isize) -> Option<Vec<(String, f64)>> {
    use windows_sys::Win32::System::Performance::{
        PdhGetFormattedCounterArrayW, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE,
    };
    let mut size = 0u32;
    let mut count = 0u32;
    // SAFETY: 第一趟传空缓冲,PDH 用 `size` 回报所需字节数(返回
    // PDH_MORE_DATA,非 0,所以这里不检查返回值,只看 size)。
    unsafe {
        PdhGetFormattedCounterArrayW(
            counter,
            PDH_FMT_DOUBLE,
            &mut size,
            &mut count,
            std::ptr::null_mut(),
        )
    };
    if size == 0 {
        return None;
    }
    let n = (size as usize).div_ceil(std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>()) + 1;
    let mut buf: Vec<PDH_FMT_COUNTERVALUE_ITEM_W> = Vec::with_capacity(n);
    // SAFETY: 容量已按 PDH 回报的字节数算好并多留一项;PDH 负责填充,
    // 之后只读前 `count` 项。
    let ok = unsafe {
        PdhGetFormattedCounterArrayW(
            counter,
            PDH_FMT_DOUBLE,
            &mut size,
            &mut count,
            buf.as_mut_ptr(),
        )
    };
    if ok != 0 {
        return None;
    }
    // SAFETY: PDH 成功返回,前 `count` 项已初始化。
    unsafe { buf.set_len(count as usize) };

    let mut out = Vec::with_capacity(buf.len());
    for it in &buf {
        if it.szName.is_null() {
            continue;
        }
        // SAFETY: `szName` 指向 PDH 填在同一块缓冲尾部的 NUL 结尾宽串。
        let name = unsafe {
            let mut len = 0usize;
            while *it.szName.add(len) != 0 {
                len += 1;
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(it.szName, len))
        };
        // SAFETY: 用 PDH_FMT_DOUBLE 取的数,联合体里有效的是 doubleValue。
        let v = unsafe { it.FmtValue.Anonymous.doubleValue };
        if v.is_finite() {
            out.push((name, v));
        }
    }
    Some(out)
}
```

- [ ] **Step 2: Snapshot 加字段 + 渲染**

`profile.rs` 的 `Snapshot` 里，在 `main_cpu_pct` 之后加：

```rust
    /// F165:GPU 引擎占用,按类型聚合的前两名。空 = 采不到或全零。
    pub gpu_engines: Vec<(String, u8)>,
    /// F165:GPU 探针可用吗。区分「可用但为 0」与「采不到」。
    pub gpu_available: bool,
```

`empty()` 里加：

```rust
            gpu_engines: Vec::new(),
            gpu_available: false,
```

在 `fmt_pct` 之后加：

```rust
/// F165:GPU 引擎占用渲染成 `3D:14%/Copy:3%`。
///
/// 三种状态必须长得不一样:探针不可用 `n/a`、可用但全零 `0%`、有值列出来。
/// 把前两种混成一个的话,「这台机器读不到 GPU」和「这台机器没在用 GPU」
/// 在日志里无法区分。
fn fmt_engines(engines: &[(String, u8)], available: bool) -> String {
    if !available {
        return "n/a".to_string();
    }
    if engines.is_empty() {
        return "0%".to_string();
    }
    engines
        .iter()
        .map(|(k, v)| format!("{k}:{v}%"))
        .collect::<Vec<_>>()
        .join("/")
}
```

`render_line` 的格式串里 `cpu={}` 之后加 ` gpu={}`，参数里加：

```rust
        fmt_engines(&s.gpu_engines, s.gpu_available),
```

- [ ] **Step 3: 测试三态**

`profile.rs` 的 `mod tests` 里加：

```rust
    /// GPU 的三种状态在日志里必须长得不一样。
    ///
    /// 「读不到 GPU」和「GPU 空着」混成同一个字符串的话,排障时没法判断
    /// 是探针坏了还是真的没在渲染。
    ///
    /// 自证会变红:把 `fmt_engines` 的 `!available` 分支改成返回 `"0%"`。
    #[test]
    fn an_unavailable_gpu_probe_reads_differently_from_an_idle_gpu() {
        assert_eq!(fmt_engines(&[], false), "n/a");
        assert_eq!(fmt_engines(&[], true), "0%");
        assert_eq!(
            fmt_engines(&[("3D".to_string(), 14), ("Copy".to_string(), 3)], true),
            "3D:14%/Copy:3%"
        );
    }
```

- [ ] **Step 4: 看门狗接线**

`diag.rs` 的 `start_watchdog` 里，在 `let cpu = ...` 之后加：

```rust
    let gpu = crate::sysprobe::GpuProbe::new();
```

`spawn` 改成 `.spawn(move || watchdog_loop(stall_ms, cpu, gpu));`，
签名改成 `fn watchdog_loop(stall_ms: u64, mut cpu: crate::sysprobe::CpuProbe, mut gpu: crate::sysprobe::GpuProbe) {`。

周期指标那段里，`snap.main_cpu_pct = ...` 之后加：

```rust
            if let Some(g) = gpu.sample() {
                snap.gpu_available = true;
                snap.gpu_engines = g.engines;
            }
```

- [ ] **Step 5: 跑绿 + 交叉编译 + 提交**

```bash
cargo test --workspace > /tmp/t11.log 2>&1; grep -nE "FAILED|panicked" /tmp/t11.log | head
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app 2>&1 | tail -20
git add crates/mullion-app/src/sysprobe.rs crates/mullion-app/src/diag.rs crates/mullion-app/src/profile.rs
git commit -m "feat(sysprobe): PDH 读本进程 GPU 引擎占用率 (F165)

必须用 PdhAddEnglishCounterW —— PdhAddCounterW 吃本地化计数器名,
中文 Windows 上运行期静默找不到,编译和本机测试全绿。

守护测试:profile::tests::an_unavailable_gpu_probe_reads_differently_from_an_idle_gpu

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 12: DXGI 显存

**Files:**
- Modify: `crates/mullion-app/Cargo.toml`
- Modify: `crates/mullion-app/src/sysprobe.rs`
- Modify: `crates/mullion-app/src/profile.rs`
- Modify: `crates/mullion-app/src/diag.rs`
- Modify: `crates/mullion-app/src/gpu.rs`

- [ ] **Step 1: 加 DXGI feature**

`crates/mullion-app/Cargo.toml` 的 `windows` features 列表里追加：

```toml
    # F165:IDXGIAdapter3::QueryVideoMemoryInfo,读本进程显存占用。
    "Win32_Graphics_Dxgi",
```

- [ ] **Step 2: 实现**

在 `sysprobe.rs` 的 `GpuProbe` 之后插入：

```rust
/// 一次显存采样(本进程的本地显存)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VramSample {
    pub used_mb: u64,
    pub budget_mb: u64,
}

/// 显存探针。DXGI adapter **枚举一次常驻** —— 每 5 秒重新
/// `CreateDXGIFactory1` + `EnumAdapters1` 是白花的系统调用。
pub struct VramProbe {
    #[cfg(windows)]
    adapter: Option<windows::Win32::Graphics::Dxgi::IDXGIAdapter3>,
}

impl VramProbe {
    /// `vendor`/`device` 来自 `wgpu::AdapterInfo`,用来在多显卡机器上
    /// 认出 wgpu 实际在用的那一块。
    #[cfg(windows)]
    pub fn new(vendor: u32, device: u32) -> Self {
        Self {
            adapter: find_adapter(vendor, device),
        }
    }

    /// 非 Windows 上这个结构体**没有字段**,所以构造器也得分开写 ——
    /// 把 `adapter: None` 写在 `#[cfg(not(windows))]` 属性下是编不过的
    /// (属性只能去掉字段初始化,去不掉「结构体没有这个字段」)。
    #[cfg(not(windows))]
    pub fn new(_vendor: u32, _device: u32) -> Self {
        Self {}
    }

    #[cfg(windows)]
    pub fn sample(&self) -> Option<VramSample> {
        use windows::Win32::Graphics::Dxgi::{
            DXGI_MEMORY_SEGMENT_GROUP_LOCAL, DXGI_QUERY_VIDEO_MEMORY_INFO,
        };
        let a = self.adapter.as_ref()?;
        let mut info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
        // SAFETY: `a` 是活着的 COM 接口;out 参数在栈上。
        unsafe { a.QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &mut info) }.ok()?;
        const MB: u64 = 1024 * 1024;
        Some(VramSample {
            used_mb: info.CurrentUsage / MB,
            budget_mb: info.Budget / MB,
        })
    }

    #[cfg(not(windows))]
    pub fn sample(&self) -> Option<VramSample> {
        None
    }
}

/// 按 vendor/device 找出 wgpu 在用的那块 adapter。
///
/// `QueryVideoMemoryInfo` 报的是**本进程**的用量,与 wgpu 实际选了 D3D12
/// 还是 Vulkan 无关 —— DXGI 在驱动层统计,不看是哪个 API 申请的。
///
/// 已知限制:两块同型号 GPU 时取枚举到的第一块。
#[cfg(windows)]
fn find_adapter(vendor: u32, device: u32) -> Option<windows::Win32::Graphics::Dxgi::IDXGIAdapter3> {
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter3, IDXGIFactory1};
    // SAFETY: CreateDXGIFactory1 是 free-threaded 的,不需要先 CoInitialize。
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.ok()?;
    for i in 0..16u32 {
        // SAFETY: 索引越界时返回 DXGI_ERROR_NOT_FOUND,由 `ok()?` 收掉。
        let Ok(a1) = (unsafe { factory.EnumAdapters1(i) }) else {
            break;
        };
        // SAFETY: `a1` 刚由 EnumAdapters1 返回,有效。
        let Ok(desc) = (unsafe { a1.GetDesc1() }) else {
            continue;
        };
        if desc.VendorId == vendor && desc.DeviceId == device {
            return a1.cast::<IDXGIAdapter3>().ok();
        }
    }
    None
}
```

`sysprobe.rs` 顶部（模块文档之后）加：

```rust
#[cfg(windows)]
use windows::core::Interface as _; // `cast::<IDXGIAdapter3>()`
```

- [ ] **Step 3: Snapshot 加字段 + 渲染**

`profile.rs` 的 `Snapshot` 里，`gpu_available` 之后加：

```rust
    /// F165:本进程显存 (已用 MB, 预算 MB)。`None` = 采不到。
    pub vram_mb: Option<(u64, u64)>,
```

`empty()` 加 `vram_mb: None,`。

`render_line` 格式串 `gpu={}` 之后加 ` vram={}`，参数加：

```rust
        s.vram_mb
            .map_or_else(|| "n/a".to_string(), |(u, b)| format!("{u}/{b}MB")),
```

- [ ] **Step 4: 接线（探针要 adapter info，得从 gpu.rs 拿）**

`gpu.rs` 的 `Gpu::new` 里，`let info = adapter.get_info();` 那段日志之后加：

```rust
        // F165:显存探针要按 vendor/device 在 DXGI 里认出同一块卡。
        // adapter 枚举一次常驻,交给 diag 存着。
        crate::diag::set_vram_probe(crate::sysprobe::VramProbe::new(info.vendor, info.device));
```

`diag.rs` 里，在 `static FRAME_US`（第 271 行）附近加：

```rust
/// F165:显存探针。`Gpu::new` 建好后放进来 —— 看门狗线程比 GPU 早启动,
/// 拿不到 adapter info,只能反过来由 GPU 那边推给它。
static VRAM_PROBE: std::sync::OnceLock<crate::sysprobe::VramProbe> = std::sync::OnceLock::new();

/// F165:`Gpu::new` 调一次。重复调用忽略(只有一个窗口)。
pub fn set_vram_probe(p: crate::sysprobe::VramProbe) {
    let _ = VRAM_PROBE.set(p);
}
```

`take_snapshot` 里，`s.mem_process_mb = ...` 之后加：

```rust
    s.vram_mb = VRAM_PROBE
        .get()
        .and_then(|p| p.sample())
        .map(|v| (v.used_mb, v.budget_mb));
```

**注意**：`VramProbe` 要能进 `OnceLock`（需要 `Send + Sync`）。在 `sysprobe.rs` 的
`VramProbe` 定义之后加：

```rust
// SAFETY: `IDXGIAdapter3` 是 free-threaded 的(DXGI 对象不属于 COM 单元),
// 且这里只做只读查询。放进 `OnceLock` 需要这两个约束。
#[cfg(windows)]
unsafe impl Send for VramProbe {}
#[cfg(windows)]
unsafe impl Sync for VramProbe {}
```

- [ ] **Step 5: 跑绿 + 交叉编译 + 提交**

```bash
cargo test --workspace > /tmp/t12.log 2>&1; grep -nE "FAILED|panicked" /tmp/t12.log | head
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app 2>&1 | tail -20
git add crates/mullion-app/
git commit -m "feat(sysprobe): DXGI 读本进程显存占用 (F165)

QueryVideoMemoryInfo 报的是本进程用量,与 wgpu 选了 D3D12 还是 Vulkan
无关。adapter 按 vendor/device 匹配 wgpu 在用的那块,枚举一次常驻。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 13: GPU 帧耗时（timestamp query）

**Files:**
- Modify: `crates/mullion-app/src/gpu.rs`
- Modify: `crates/mullion-app/src/app.rs:10968`
- Modify: `crates/mullion-app/src/diag.rs`
- Modify: `crates/mullion-app/src/profile.rs`

- [ ] **Step 1: 条件申请 feature**

`gpu.rs` 的 `Gpu::new` 里，把

```rust
        let (device, queue) = handle
            .block_on(adapter.request_device(&wgpu::DeviceDescriptor::default(), None))
            .expect("request_device");
```

替换为

```rust
        // F165:GPU 帧耗时要 TIMESTAMP_QUERY。**条件申请**:请求 adapter
        // 不支持的 feature,`request_device` 会直接失败 —— 那等于为了一个
        // 诊断指标让整个程序在老驱动上起不来。不支持就整块降级成 n/a。
        let has_ts = adapter
            .features()
            .contains(wgpu::Features::TIMESTAMP_QUERY);
        if !has_ts {
            log::info!(target: "mullion", "adapter 不支持 TIMESTAMP_QUERY,GPU 帧耗时降级为 n/a");
        }
        let (device, queue) = handle
            .block_on(adapter.request_device(
                &wgpu::DeviceDescriptor {
                    required_features: if has_ts {
                        wgpu::Features::TIMESTAMP_QUERY
                    } else {
                        wgpu::Features::empty()
                    },
                    ..Default::default()
                },
                None,
            ))
            .expect("request_device");
```

- [ ] **Step 2: 建 QuerySet + staging，加进 Gpu**

`gpu.rs` 的 `pub struct Gpu`（第 330 行）里加字段：

```rust
    /// F165:GPU 帧耗时采样。adapter 不支持 TIMESTAMP_QUERY 时是 `None`。
    pub gpu_timer: Option<GpuTimer>,
```

在 `struct Gpu` 之后加：

```rust
/// F165:一帧 GPU 耗时的采样器。
///
/// **抽样而非每帧**:回读要走 `map_async`,一个 staging buffer 同时只能
/// 服务一次采样。忙的时候本帧就不挂 `timestamp_writes`(传 `None`,零开销),
/// 等上一次回读完再采下一次。诊断指标不该反过来影响被诊断的对象。
pub struct GpuTimer {
    set: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    /// **必须是 `Arc`**:`map_async` 的回调是 `'static` 的,要把 buffer
    /// move 进去才能在里面 `get_mapped_range` / `unmap`。而 wgpu 23 的
    /// `wgpu::Buffer` **没有实现 `Clone`**(只 derive 了 `Debug`),
    /// 直接 `.clone()` 编不过。
    staging: std::sync::Arc<wgpu::Buffer>,
    period_ns: f32,
    /// 上一次采样还没回来。
    busy: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl GpuTimer {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let set = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("frame-timestamps"),
            ty: wgpu::QueryType::Timestamp,
            count: 2,
        });
        let size = 2 * std::mem::size_of::<u64>() as wgpu::BufferAddress;
        Self {
            set,
            resolve: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("ts-resolve"),
                size,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }),
            staging: std::sync::Arc::new(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("ts-staging"),
                size,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            })),
            period_ns: queue.get_timestamp_period(),
            busy: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// 这一帧要不要采。`None` = 上一次还没回来,本帧不挂时间戳。
    pub fn writes(&self) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        if self.busy.load(std::sync::atomic::Ordering::Relaxed) {
            return None;
        }
        Some(wgpu::RenderPassTimestampWrites {
            query_set: &self.set,
            beginning_of_pass_write_index: Some(0),
            end_of_pass_write_index: Some(1),
        })
    }

    /// 在 `submit` **之前**录进 encoder。只在本帧真的挂了时间戳时调。
    pub fn resolve(&self, enc: &mut wgpu::CommandEncoder) {
        enc.resolve_query_set(&self.set, 0..2, &self.resolve, 0);
        enc.copy_buffer_to_buffer(&self.resolve, 0, &self.staging, 0, self.staging.size());
    }

    /// `submit` **之后**发起回读。
    ///
    /// 回调要等后续 `poll`/`submit` 才触发 —— 长空闲时样本会停在半路。
    /// **那不是泄漏**:`busy` 一直是 true,下一次渲染自然收割,只是这段
    /// 时间里不再采新样本。
    pub fn read_back(&self) {
        self.busy
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let busy = self.busy.clone();
        let staging = self.staging.clone();
        let period = self.period_ns;
        self.staging
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |res| {
                if res.is_ok() {
                    let view = staging.slice(..).get_mapped_range();
                    let a = u64::from_le_bytes(view[0..8].try_into().unwrap_or_default());
                    let b = u64::from_le_bytes(view[8..16].try_into().unwrap_or_default());
                    drop(view);
                    let ticks = b.saturating_sub(a);
                    let us = (ticks as f64 * period as f64 / 1000.0) as u64;
                    crate::diag::record_gpu_frame_us(us);
                }
                staging.unmap();
                busy.store(false, std::sync::atomic::Ordering::Relaxed);
            });
    }
}
```

`Gpu::new` 的返回结构体里加：

```rust
            gpu_timer: has_ts.then(|| GpuTimer::new(&device, &queue)),
```

（放在构造 `Gpu { ... }` 的字段列表里，`device`/`queue` 已在作用域内；
若所有权已被 move，改为在 move 之前先算好一个局部变量再放进去。）

- [ ] **Step 3: diag 收样本**

`diag.rs` 里，`static FRAME_US` 旁边加：

```rust
/// F165:GPU 帧耗时(微秒)。与 `FRAME_US` 共用一套桶,好横向比。
static GPU_FRAME_US: crate::profile::Histogram = crate::profile::Histogram::new();

/// F165:记一次 GPU 帧耗时。由 wgpu 的 map 回调调用(不在主线程上)。
pub fn record_gpu_frame_us(us: u64) {
    GPU_FRAME_US.record_us(us);
}
```

`take_snapshot` 里加：

```rust
    s.gpu_frame_us = GPU_FRAME_US.drain();
```

- [ ] **Step 4: Snapshot 字段 + 渲染**

`profile.rs` 的 `Snapshot` 里加：

```rust
    /// F165:GPU 帧耗时分布。样本数为 0 = 不支持或本窗口没采到。
    pub gpu_frame_us: Counts,
```

`empty()` 加 `gpu_frame_us: [0; BUCKETS],`。

`render_line` 里，在 `let dirty_part = ...` 之后加：

```rust
    // GPU 帧耗时:样本数为 0 时报 n/a 而不是 p50=0 —— adapter 不支持
    // TIMESTAMP_QUERY 与「GPU 一帧只用了 0µs」必须在日志里长得不一样。
    let gpu_us_part = {
        let n = total(&s.gpu_frame_us);
        if n == 0 {
            "n/a".to_string()
        } else {
            format!(
                "{n}x/p50={}/p95={}",
                fmt_us(quantile_us(&s.gpu_frame_us, 0.5)),
                fmt_us(quantile_us(&s.gpu_frame_us, 0.95))
            )
        }
    };
```

格式串 `vram={}` 之后加 ` gpu_us={}`，参数加 `gpu_us_part,`。

- [ ] **Step 5: 测试**

`profile.rs` 的 `mod tests` 里加：

```rust
    /// 没采到 GPU 帧耗时时报 `n/a`,不是 `p50=0`。
    ///
    /// adapter 不支持 TIMESTAMP_QUERY 与「GPU 一帧只用了 0µs」是两回事,
    /// 后者还会让人以为渲染是免费的。
    ///
    /// 自证会变红:把 `gpu_us_part` 的 `n == 0` 分支删掉。
    #[test]
    fn a_gpu_timer_that_never_reported_says_n_a_instead_of_zero() {
        let mut s = Snapshot::empty();
        s.window_ms = 5_000;
        s.frames = 10;
        let line = render_line(&s).expect("非空闲窗口该出行");
        assert!(line.contains("gpu_us=n/a"), "没采到却报了数字:{line}");

        // `bucket_of` 是本模块私有的,`mod tests` 里有 `use super::*` 直接可用。
        s.gpu_frame_us[bucket_of(2_000)] = 3;
        let line = render_line(&s).expect("非空闲窗口该出行");
        assert!(line.contains("gpu_us=3x/"), "采到了却没报:{line}");
    }
```

- [ ] **Step 6: app.rs 挂上**

`app.rs` 第 10968 行的 `begin_render_pass`，把

```rust
            timestamp_writes: None,
```

改成

```rust
            timestamp_writes: ts_writes,
```

在 `let mut enc = ...`（第 10950 行附近）之后、`{` 开 pass 之前加：

```rust
    // F165:GPU 帧耗时抽样。上一次回读还没回来就跳过本帧(传 None,零开销)。
    let ts_writes = a.gpu.gpu_timer.as_ref().and_then(|t| t.writes());
    let sampling = ts_writes.is_some();
```

pass 的 `}` 之后、`a.gpu.queue.submit(...)` 之前加：

```rust
    // resolve 必须在 submit 之前录进同一个 encoder。
    if sampling {
        if let Some(t) = a.gpu.gpu_timer.as_ref() {
            t.resolve(&mut enc);
        }
    }
```

`queue.submit(...)` 之后加：

```rust
    // 回读在 submit 之后发起:map_async 要等 GPU 跑完这批命令。
    if sampling {
        if let Some(t) = a.gpu.gpu_timer.as_ref() {
            t.read_back();
        }
    }
```

- [ ] **Step 7: 跑绿 + 交叉编译 + 提交**

```bash
cargo test --workspace > /tmp/t13.log 2>&1; grep -nE "FAILED|panicked" /tmp/t13.log | head
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app 2>&1 | tail -20
git add crates/mullion-app/
git commit -m "feat(gpu): timestamp query 抽样测 GPU 帧耗时 (F165)

feature 条件申请:请求 adapter 不支持的 feature 会让 request_device
直接失败,等于为一个诊断指标让程序在老驱动上起不来。
抽样而非每帧:staging 忙时本帧传 timestamp_writes: None,零开销。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

# 阶段四：收尾

## Task 14: 文档与 spec 登记

**Files:**
- Modify: `spec.md`
- Modify: `docs/gui-render-gotchas.md`
- Modify: `CLAUDE.md`

- [ ] **Step 1: spec.md 登记三条**

在 `spec.md` 的功能表末尾（F159 那一批之后）追加：

```markdown
| F164 | 周期 profile 行加进程 CPU%(按核数归一)与主线程 CPU%(不归一);CPU 超阈值强制打破空闲门 | P1 | 两个口径的归一化差异有单测(多核机上「烧满一个核」不得被压成个位数);采不到时是 `None`,既不冒充 0 也不打破空闲门 |
| F165 | GPU 三口径:PDH `\GPU Engine(*)` 本进程引擎占用率、DXGI `QueryVideoMemoryInfo` 本进程显存、wgpu TIMESTAMP_QUERY 的 GPU 帧耗时 | P1 | PDH 必须用 `PdhAddEnglishCounterW`(本地化名在中文 Windows 上静默失败);实例名 pid 过滤须防前缀串号(`pid_1234` vs `pid_12345`);TIMESTAMP_QUERY 条件申请,不支持时降级 `n/a` 而非 `request_device` 失败 |
| F166 | 一实例一日志文件 `mullion-<instance_id>.log`(与 F148 现场历史同源)+ 行内 pid + 运行期轮转 + 按心跳判活的配额清理 | P1 | 清理三道保险(自己按文件名硬排除、活实例不动、60 秒内不删),主文件与 `.log.1` 按 id 分组同进退;文件名解析须严格校验 id 形状,否则 F155 导出的 `mullion-redacted.log` 会被删掉 |
```

- [ ] **Step 2: gotchas 补两条**

`docs/gui-render-gotchas.md` 末尾追加：

```markdown
## TIMESTAMP_QUERY 必须条件申请（F165）

**症状**：`request_device` 在老驱动/核显上直接失败，程序起不来 —— 而起不来的
理由只是一个诊断指标。

**规则**：`adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY)` 为真才
放进 `required_features`，否则整块降级成 `gpu_us=n/a`。

**守护**：`profile::tests::a_gpu_timer_that_never_reported_says_n_a_instead_of_zero`
（保证「不支持」与「0µs」在日志里长得不一样）。

## GPU 帧耗时的回读会悬在半路（F165）

**症状**：长空闲时 `map_async` 的回调迟迟不来，看起来像泄漏。

**规则**：那是正常的 —— 回调要等后续 `poll`/`submit`。`busy` 标志一直为 true，
期间不再采新样本，下一次渲染自然收割。**不要**为此加超时或强制 `poll`：
强制 poll 会把空闲时的 CPU 拉起来，正是 F157~F159 刚压下去的那件事。
```

- [ ] **Step 3: CLAUDE.md 的领域陷阱表加一条**

在 T11 之后追加：

```markdown
| T12 | 把 `GetCurrentThread()` 的返回值存进结构体给别的线程用 | 它是**伪句柄**(常量 `-2`),含义是「调用它的那个线程」。存给看门狗线程之后，`GetThreadTimes` 量的是看门狗自己 —— 主线程 CPU% 恒等于零点几，而事件循环正忙转。**静默错值，没有任何报错**，且本机 Linux 上这段代码根本不编译 | `sysprobe::tests::this_platform_reports_cpu_time_that_actually_grows_when_we_burn_cpu`；必须在主线程上 `DuplicateHandle` 拿自有句柄 |
```

- [ ] **Step 4: 提交**

```bash
git add spec.md docs/gui-render-gotchas.md CLAUDE.md
git commit -m "docs: 登记 F164~F166 + 补两条 GPU gotcha 与 T12 伪句柄陷阱

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 15: 全量验收 + 发版

- [ ] **Step 1: 定义的「绿」**

```bash
cargo test --workspace > /tmp/final.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/final.log | grep -v "ok\." | head
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
cargo fmt --check
```

Expected: 三条全部无输出（`cargo test` 那条只该剩 `ok.` 被 grep 掉）

- [ ] **Step 2: 交叉编译 + objdump 验收**

```bash
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app 2>&1 | tail -5
```

Expected: `Finished`

**新增了两个 DLL 依赖，必须确认它们进了导入表且都是系统 DLL**：

```bash
x86_64-w64-mingw32-objdump -p target/x86_64-pc-windows-gnu/release/mullion-app.exe \
  | grep -i "DLL Name" | sort -u
```

Expected: 列表里出现 `PDH.dll` 与 `dxgi.dll`（或 `DXGI.dll`），且**没有**任何
非系统 DLL（如 `libgcc_s_seh-1.dll`、`libwinpthread-1.dll`）。

- [ ] **Step 3: 走发版一条龙**

按 `.claude/skills/release-windows/SKILL.md`：升 patch 版本号（0.1.70 → 0.1.71）→
跑绿 → 交叉编译 → objdump → 签名 → 发 GitHub Release（走 socks 代理）。

Release notes 里必须带下面这份**人工验收清单**（都是无头容器里验不了的）：

```markdown
### 人工验收（F164~F166）

1. **多实例归属**：开两个 mullion，确认 `%APPDATA%\mullion\config\` 下出现两个
   `mullion-<数字>-<数字>.log`，行前缀各带自己的 pid，`cpu=`/`gpu=`/`vram=`
   两边独立且数字合理。
2. **GPU 数字对得上**：任务管理器「性能 → GPU」的占用率与 `gpu=3D:N%` 大致同量级；
   「专用 GPU 内存」与 `vram=N/MMB` 的已用值对得上。
3. **真空闲不写盘**：连上一台机器后放着不动 10 分钟，确认 `cpu=` 接近 0
   **且日志不再新增 profile 行**（空闲门没被打破）。
4. **空闲烧核会被抓到**：（若能复现）出现 `cpu=` 主线程 > 20% 而 `frame=0x` 的行。
5. **清理不误伤**：关掉一个实例，再开第三个，确认死实例的 `.log` 与 `.log.1`
   成对回收，**活着那个的文件没被删**。
6. **导出不互相覆盖**：两个实例都点「导出脱敏日志」，确认产出两个
   `mullion-redacted-<id>.log`，且提示里说了「本机还有 N 份其他实例的日志」。
7. **老驱动降级**：（有核显机器可试）确认 `gpu_us=n/a` 而不是崩溃。
8. **运行期轮转**：设置里切到「详细」档跑一阵，确认超过 64MB 后出现 `.log.1`，
   主文件从头开始且**日志没有停住**（若停住 = 先 rename 后关文件的 bug 回来了）。
```

---

## 自查记录

对着设计文档逐节核过：

| 设计节 | 落在哪个任务 |
|---|---|
| §1 数据流与归属 | Task 7（模块建立）、Task 9/11/12（看门狗接线） |
| §2 F164 CPU | Task 7（纯函数）、Task 8（平台采样 + 伪句柄）、Task 9（Snapshot + 空闲门 + 阈值常量） |
| §3 F165 PDH | Task 10（解析纯函数）、Task 11（English counter + 探针） |
| §4 F165 DXGI | Task 12 |
| §5 F165 timestamp query | Task 13（条件申请 + 抽样 + 在途样本说明） |
| §6 F166 一实例一日志 | Task 1（id 上移）、Task 2（行内 pid）、Task 3（文件名 + 严格解析）、Task 4（SINK + 运行期轮转先关后挪）、Task 5（清理三道保险 + `.1` 配对）、Task 6（F155 导出） |
| §7 测试与验收 | 每个任务的 Step 1/2 + Task 15 的清单 |
| §8 波及面 | Task 6（ADR-008）、Task 14（spec/gotchas/CLAUDE.md）、Task 8/12（Cargo.toml） |
| §9 非目标 | 无任务（不做） |

**设计文档之外新增的一项**（复核 API 时发现）：F155 导出的
`mullion-redacted.log` 会被宽松的文件名解析器当成实例日志删掉 —— 已在 Task 3
用严格的 id 校验挡住，并在 Task 6 顺带修掉两个实例导出互相覆盖。

**自查时改掉的三处编译错误**（都是按锁定版本核实过的事实，不要改回去）：

1. `wgpu::Buffer` 在 wgpu 23 里**只 derive 了 `Debug`，没有 `Clone`** ——
   `map_async` 的 `'static` 回调要 move 进 buffer，只能包 `Arc<wgpu::Buffer>`。
2. `DirEntry::file_name()` 返回自有的 `OsString`，链式 `.to_str()` 会让 `&str`
   借在当场析构的临时值上。必须先 `let name = e.file_name();` 再借。
3. `#[cfg(not(windows))]` 加在**字段初始化**上去不掉「结构体没有这个字段」——
   `VramProbe::new` 必须按平台分成两个函数，不能靠属性裁剪单个构造式。
