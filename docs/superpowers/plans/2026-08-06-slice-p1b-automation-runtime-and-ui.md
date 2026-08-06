# 切片 P1-b 实现计划：登录后自动化的运行时接线与配置 UI（F40~F44）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 P1-a 已就绪的 `build_plan`（store，纯函数）与 `write_scheduled`（ssh，async 调度）在 app 层接起来，让用户连上远端后自动落进 tmux 里的 Claude Code，并给出可编辑的配置入口与可跳过的逃生门。

**Architecture:** 新增 `crates/mullion-app/src/automation.rs`，分纯决策（可裸单测）与 async 运行（`start_paused` 假时钟 + FakeSink，零网络）两部分。winit 线程只发三个一次性信号（就绪 / 用户接管 / 断线），超时与延时全在 spawn 出去的 tokio task 的 `select!` 里闭环，**一行 `ControlFlow` 都不动**（T7）。配置 UI 是会话编辑器的第四个 tab。

**Tech Stack:** Rust 2021 / tokio（`oneshot` + `select!` + `time::pause`）/ winit 0.30 `ApplicationHandler` / egui 0.30 / 既有 `mullion-store::automation`、`mullion-ssh::schedule`。

**输入 spec：** `docs/superpowers/specs/2026-08-06-slice-p1b-automation-runtime-and-ui-design.md`（下称「spec」）。spec 的 §1 两处修订、§3 的陷阱清单、§7 人工验收清单在本计划中逐条落地。

---

## 相对 spec 的四处实现决策（写代码前先读，避免以为是漏做）

1. **`decide_start` 改名为 `take_pending` 并合并跳过标志的消费。** spec §3 把「要不要起」和「一次性跳过标志的消费」写成两件事，但「跳过恰好只生效一次」这条性质如果不落在同一个函数里就没法单测（消费点会散进 `app.rs`，而 `App` 在无头环境构造不出来）。合并后 Task 3 的三条测试全部扎在真实决策点上。

2. **`status_text` 返回 `String` 而非 `Option<String>`。** 六种 `Outcome` 每一种都有文案，没有「无话可说」的情形。「进行中」不是 `Outcome`，单独一个常量 + `status_line` 组合函数。

3. **`AutomationHandle` 不存 `steps: usize`。** `Outcome::Completed(n)` 自己带着步数回来，handle 再存一份就是两个真源。

4. **`EditorBuffer.preserved_automation` 保持字段名不改。** spec §5 说「直接变成可编辑的 `automation`」——语义照做（本切片起它可编辑），但**名字不动**：`buffer.rs:76` 已有先例注明「`preserved_group_id` 自 P0-b 起可由编辑器下拉修改，名字沿用未改以免波及守护测试」。改名会波及 `buffer.rs` 的透传守护测试（`draft.automation == rec.automation` 那条），收益为零。

**断线为什么要单独一条 oneshot（spec §3 表格第四行的实现细节）：** 若断线时复用 `cancel`，`write_scheduled` 会返回 `Cancelled`，状态栏就会显示「自动化已中止：检测到你的输入」——用户根本没输入。所以 handle 上是 **三条** 一次性通道：`ready` / `cancel` / `disconnect`。

---

## 文件结构

| 文件 | 责任 | 本切片动作 |
|---|---|---|
| `crates/mullion-app/src/automation.rs` | 自动化状态机：纯决策 + async 运行 + 文案 | **新建**（Task 2~5） |
| `crates/mullion-app/src/lib.rs` | 模块清单 | 加一行 `pub mod automation;` |
| `crates/mullion-app/Cargo.toml` | 依赖 | dev-deps 加 tokio `test-util` |
| `crates/mullion-app/src/shell/workspace/mod.rs` | pane 状态机 | `PtyWriter for Arc<SshSession>`、`PaneState.saw_first_byte` |
| `crates/mullion-app/src/app.rs` | 事件循环 | 四条触发边 + `UserEvent::AutomationDone` + 计划计算 |
| `crates/mullion-app/src/ui/mod.rs` | `UiState` / `UiFrame` / `build_ui` | 跳过意图字段 + 状态栏接线 |
| `crates/mullion-app/src/ui/chrome.rs` | 状态栏 | 多一个自动化文案槽位 |
| `crates/mullion-app/src/ui/session_manager/list.rs` | 会话列表 | 右键菜单加「连接（跳过自动化）」 |
| `crates/mullion-app/src/ui/session_manager/mod.rs` | Tab 常量 | 加 `TAB_ADVANCED` / `TAB_AUTOMATION` |
| `crates/mullion-app/src/ui/session_manager/editor.rs` | 右栏骨架 | `TABS` 扩成四个 + 分派 |
| `crates/mullion-app/src/ui/session_manager/fields.rs` | 字段布局 | **新增 `automation()`**（Task 14~16） |

`crates/mullion-store/**` 与 `crates/mullion-ssh/**` **一行不改**——P1-a 已经把数据层和调度做完了。任何想改它们的冲动都说明接线接错了，停下来。

---

## Task 1: 前置改动 —— `Arc<SshSession>` 可当 PTY 写口

**Files:**
- Modify: `crates/mullion-app/src/shell/workspace/mod.rs:40-47`
- Modify: `crates/mullion-app/Cargo.toml`（dev-dependencies）

`write_scheduled` 要 `Arc<dyn ByteSink>`，而 pane 也要一个写口。两者指向同一个 `SshSession`，所以 `SshSession` 必须能被共享持有。`PaneState.pty` 的类型本来就是 `Box<dyn PtyWriter>`，`SshSession` 内部只有一个 `mpsc::Sender<SshCmd>`（本身 `Send + Sync`），所以只要多一个转发实现即可，**既有调用点零改动**。

- [ ] **Step 1: 加 `PtyWriter for Arc<SshSession>`**

在 `crates/mullion-app/src/shell/workspace/mod.rs` 中 `impl PtyWriter for SshSession { ... }` 那一块（`:40-47`）**之后**插入：

```rust
/// 同一条 SSH channel 同时被 pane（同步写按键）和自动化 task（异步按时间表写）
/// 持有，所以要有一个可共享的写口。`SshSession` 内部只有一个
/// `mpsc::Sender<SshCmd>`，本身就是 `Send + Sync`，`Arc` 只是共享所有权，
/// 不引入任何锁。
///
/// 转发而不是 `impl<T: PtyWriter> PtyWriter for Arc<T>`：后者会跟
/// `Box<dyn PtyWriter>` 的既有用法产生一堆 trait 求解歧义，而本项目只需要
/// `Arc<SshSession>` 这一种。
impl PtyWriter for Arc<SshSession> {
    fn write(&self, bytes: Vec<u8>) -> Result<(), TrySendErr> {
        SshSession::write(self, bytes)
    }
    fn resize(&self, cols: u16, rows: u16) -> Result<(), TrySendErr> {
        SshSession::resize(self, cols, rows)
    }
}
```

- [ ] **Step 2: 给 dev-dependencies 加 tokio `test-util`**

后面的假时钟测试要 `tokio::time::pause()`，它由 `test-util` feature 提供。`mullion-ssh` 已经这么加过（见其 `Cargo.toml` 的 dev-dependencies），照抄。

把 `crates/mullion-app/Cargo.toml` 末尾的

```toml
[dev-dependencies]
tempfile = "3"
```

改成

```toml
[dev-dependencies]
tempfile = "3"
# 自动化的假时钟测试（`#[tokio::test(start_paused = true)]`）要 test-util。
# feature 是并集，这里只是在 test target 上多开一个，不影响正式构建。
tokio = { workspace = true, features = ["test-util"] }
```

- [ ] **Step 3: 编译并跑既有测试，确认零回归**

Run: `cargo test -p mullion-app --lib > /tmp/t1.log 2>&1; grep -nE "test result|FAILED|panicked|error\[" /tmp/t1.log`
Expected: `test result: ok.`，无 `FAILED` / `error[`。

- [ ] **Step 4: 提交**

```bash
git add crates/mullion-app/src/shell/workspace/mod.rs crates/mullion-app/Cargo.toml
git commit -m "feat(app): Arc<SshSession> 可当 PTY 写口 + 测试用假时钟 (F40~F44)

pane 与自动化 task 要同时持有同一条 SSH channel。PaneState.pty 本来就是
Box<dyn PtyWriter>，只需多一个转发实现，既有调用点零改动。"
```

---

## Task 2: `automation.rs` 骨架 —— `Outcome` 与状态栏文案

**Files:**
- Create: `crates/mullion-app/src/automation.rs`
- Modify: `crates/mullion-app/src/lib.rs`

- [ ] **Step 1: 写模块 + 失败的文案测试**

新建 `crates/mullion-app/src/automation.rs`：

```rust
//! F40~F44 登录后自动化的 **app 侧**状态机。
//!
//! 分两部分，边界是刻意的：
//! - **纯决策**（`pending_for` / `take_pending` / `status_text` /
//!   `split_pasted_commands`）：裸单测，无 tokio、无 egui、无 store。
//! - **async 运行**（`run`）：`#[tokio::test(start_paused = true)]` 假时钟 +
//!   假 sink，零网络。
//!
//! 超时与延时**全部**在这里的 `select!` 里闭环，**绝不进 `ControlFlow`**。
//! 理由见设计 spec §1 修订一：T7（首次节流后永久 100% CPU 忙转）的现场就是
//! 事件循环那三个分支的 `control_flow` 复位，为一个与渲染无关的业务 deadline
//! 去改那里，是在踩过雷的地方重新布雷，而收益为零。

/// 自动化正在跑时状态栏显示的文案。不是 `Outcome`——它描述的是「还没结束」。
pub const RUNNING_TEXT: &str = "自动化:进行中";

/// 一次自动化的最终结局。回送给 UI **只为了给用户一句话**，不驱动任何后续动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// 全部步骤都发完了。带步数：用户看不见发生了什么，就无法判断自动化是
    /// 没跑还是跑了没效果。
    Completed(usize),
    /// 用户接管（敲了键/滚了轮/粘贴了）。
    Aborted,
    /// 等首字节超时。带毫秒数：文案要说清等了多久，否则用户不知道该调哪个值。
    SkippedTimeout { after_ms: u32 },
    /// 链路断了。
    Disconnected,
    /// 出站队列持续满。
    Congested,
}

/// 结局 → 状态栏文案。纯函数。
pub fn status_text(o: Outcome) -> String {
    match o {
        Outcome::Completed(n) => format!("自动化已完成({n} 步)"),
        Outcome::Aborted => "自动化已中止:检测到你的输入".to_string(),
        // 向上取整而不是截断:配了 500ms 超时的用户不该看到「0s 未收到输出」。
        Outcome::SkippedTimeout { after_ms } => {
            format!("自动化已跳过:{}s 未收到远端输出", after_ms.div_ceil(1000))
        }
        Outcome::Disconnected => "自动化已中止:连接已断开".to_string(),
        Outcome::Congested => "自动化已中止:出站队列拥塞".to_string(),
    }
}

/// 状态栏这一帧该显示哪一句。`running` = 当前还挂着 handle。
///
/// 抽出来是因为「跑着的时候盖住上一次的结论」这条规则很容易在 UI 代码里
/// 写反（先画 last、再判 running），而写反的现象是「新连接的状态栏还挂着
/// 上一条连接的结论」——一条与当前连接毫不相干的误导信息。
pub fn status_line(running: bool, last: Option<&str>) -> Option<&str> {
    if running {
        Some(RUNNING_TEXT)
    } else {
        last
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 六种结局各一条文案，字面量硬编码——不能拿生产代码里的同一个
    /// `format!` 当预期值，那是重言式。
    #[test]
    fn status_text_covers_every_outcome() {
        assert_eq!(status_text(Outcome::Completed(3)), "自动化已完成(3 步)");
        assert_eq!(status_text(Outcome::Aborted), "自动化已中止:检测到你的输入");
        assert_eq!(
            status_text(Outcome::SkippedTimeout { after_ms: 15_000 }),
            "自动化已跳过:15s 未收到远端输出"
        );
        assert_eq!(
            status_text(Outcome::Disconnected),
            "自动化已中止:连接已断开"
        );
        assert_eq!(status_text(Outcome::Congested), "自动化已中止:出站队列拥塞");
    }

    /// 亚秒超时不能显示成「0s」——那句话读起来像「一瞬间就放弃了」。
    ///
    /// 自证会变红:把 `div_ceil(1000)` 改成 `/ 1000`。
    #[test]
    fn sub_second_timeout_rounds_up_so_it_never_reads_zero() {
        assert_eq!(
            status_text(Outcome::SkippedTimeout { after_ms: 500 }),
            "自动化已跳过:1s 未收到远端输出"
        );
    }

    /// 跑着的时候必须盖住上一次的结论,否则新连接的状态栏挂着旧连接的话。
    ///
    /// 自证会变红:把 `status_line` 的两个分支对调。
    #[test]
    fn running_text_wins_over_the_previous_verdict() {
        assert_eq!(status_line(true, Some("旧的结论")), Some(RUNNING_TEXT));
        assert_eq!(status_line(false, Some("旧的结论")), Some("旧的结论"));
        assert_eq!(status_line(false, None), None);
    }
}
```

- [ ] **Step 2: 挂进模块树**

`crates/mullion-app/src/lib.rs` 的模块清单里，在 `pub mod app;` **之后**加一行（按字母序，`app` 后紧跟 `automation`）：

```rust
pub mod automation;
```

- [ ] **Step 3: 跑测试**

Run: `cargo test -p mullion-app --lib automation:: > /tmp/t2.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/t2.log`
Expected: `test result: ok. 3 passed`

- [ ] **Step 4: 提交**

```bash
git add crates/mullion-app/src/automation.rs crates/mullion-app/src/lib.rs
git commit -m "feat(app): 自动化模块骨架 —— Outcome 与状态栏文案 (F40~F44)"
```

---

## Task 3: 计划的计算与消费（纯决策）

**Files:**
- Modify: `crates/mullion-app/src/automation.rs`

这是「要不要跑自动化」的**唯一**决策点。四条真实规则全在这里，`app.rs` 只负责把 store 和标志位递进来。

- [ ] **Step 1: 写失败的测试**

在 `automation.rs` 的 `mod tests` 里追加：

```rust
    // 注意路径:四个 `DEFAULT_*` 常量**没有**在 `mullion_store` 顶层 re-export
    // （lib.rs:17-19 只导出了 build_plan / AutomationCommand / AutomationPrefs /
    // EnvVar / ResolvedAutomation / Step / TmuxChoice），必须走 `automation::`。
    use mullion_store::automation::{
        DEFAULT_INITIAL_DELAY_MS, DEFAULT_INTER_DELAY_MS, DEFAULT_READY_TIMEOUT_MS,
    };
    use mullion_store::{AutomationCommand, ResolvedAutomation, SessionId, TmuxChoice};

    /// 造一份「开着、有一条命令」的解析结果。
    fn enabled_automation() -> ResolvedAutomation {
        ResolvedAutomation {
            enabled: true,
            tmux: None,
            commands: vec![AutomationCommand {
                text: "echo hi".into(),
                delay_ms: None,
            }],
            work_dir: None,
            env: Vec::new(),
            initial_delay_ms: DEFAULT_INITIAL_DELAY_MS,
            inter_delay_ms: DEFAULT_INTER_DELAY_MS,
            ready_timeout_ms: DEFAULT_READY_TIMEOUT_MS,
        }
    }

    /// F44 关掉 ⟹ `build_plan` 给空计划 ⟹ 连 oneshot 都不建、task 也不 spawn。
    ///
    /// 自证会变红:把 `pending_for` 里 `if steps.is_empty() { return None; }` 删掉。
    #[test]
    fn disabled_automation_does_not_start() {
        let mut a = enabled_automation();
        a.enabled = false;
        let p = pending_for(Some(SessionId(7)), |_| Some((a.clone(), "web01".into())));
        assert!(p.is_none(), "F44 关闭时不该产出任何待办计划");

        // 反面:开着就该有计划,否则上一条断言是恒真的。
        let on = pending_for(Some(SessionId(7)), |_| {
            Some((enabled_automation(), "web01".into()))
        });
        assert!(on.is_some());
    }

    /// CLI 直连(`mullion user@host -p 22 -i key`)没有 SessionId,不该跑自动化。
    /// 凭空猜一个 tmux 会话名去 attach 是最坏的行为。
    ///
    /// `lookup` 里直接 panic:它一旦被调用就说明有人在没有会话 id 的情况下
    /// 去查了库,那正是这条测试要挡的。
    ///
    /// 自证会变红:把 `pending_for` 开头的 `let id = id?;` 换成
    /// `let id = id.unwrap_or(SessionId(1));`。
    #[test]
    fn cli_direct_connect_has_no_plan() {
        let p = pending_for(None, |_| panic!("没有 SessionId 时不该去查会话库"));
        assert!(p.is_none());
    }

    /// 库里查不到这条会话(刚被删了、连接还在途)⟹ 不跑。
    #[test]
    fn missing_session_record_has_no_plan() {
        let p = pending_for(Some(SessionId(7)), |_| None);
        assert!(p.is_none());
    }

    /// tmux 会话名留空时,`build_plan` 拿会话名做 fallback —— 名字必须真的
    /// 流进去,否则 attach 的是别人的会话。
    ///
    /// 自证会变红:把 `pending_for` 里 `build_plan(&resolved, &name)` 的
    /// `&name` 换成 `""`。
    #[test]
    fn session_name_flows_into_the_plan_as_the_tmux_fallback() {
        let mut a = enabled_automation();
        a.tmux = Some(TmuxChoice::Attach { session_name: None });
        a.commands.clear();
        let p = pending_for(Some(SessionId(7)), |_| Some((a.clone(), "web01".into())))
            .expect("配了 tmux attach 就该有计划");
        let bytes = String::from_utf8(p.steps[0].bytes.clone()).unwrap();
        assert!(
            bytes.contains("'web01'"),
            "会话名没进 tmux 命令,实际字节: {bytes:?}"
        );
    }

    /// F44 一次性跳过:计划不空也不许起。
    ///
    /// 自证会变红:把 `take_pending` 里 `if std::mem::take(skip) { return None; }`
    /// 整段删掉。
    #[test]
    fn skip_flag_suppresses_start_even_when_plan_is_not_empty() {
        let mut pending = pending_for(Some(SessionId(7)), |_| {
            Some((enabled_automation(), "web01".into()))
        });
        assert!(pending.is_some(), "前提:计划非空");
        let mut skip = true;
        assert!(take_pending(&mut pending, &mut skip).is_none());
    }

    /// 跳过**只**生效一次。右键跳过一次之后,普通双击连接若还静默跳过,
    /// 用户会以为自动化坏了。
    ///
    /// 自证会变红:把 `std::mem::take(skip)` 换成 `*skip`。
    #[test]
    fn skip_flag_is_consumed_after_one_connect() {
        let mut skip = true;

        let mut first = pending_for(Some(SessionId(7)), |_| {
            Some((enabled_automation(), "web01".into()))
        });
        assert!(take_pending(&mut first, &mut skip).is_none(), "第一次应跳过");
        assert!(!skip, "跳过标志必须被消费掉");

        let mut second = pending_for(Some(SessionId(7)), |_| {
            Some((enabled_automation(), "web01".into()))
        });
        assert!(
            take_pending(&mut second, &mut skip).is_some(),
            "第二次连接不该再跳过"
        );
    }

    /// 待办计划也是一次性的:`ConnectOk` 取走之后不许留在 App 上,
    /// 否则下一次连接会拿到上一条会话的计划。
    #[test]
    fn pending_plan_is_taken_not_copied() {
        let mut pending = pending_for(Some(SessionId(7)), |_| {
            Some((enabled_automation(), "web01".into()))
        });
        let mut skip = false;
        assert!(take_pending(&mut pending, &mut skip).is_some());
        assert!(pending.is_none(), "计划必须被取走");
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib automation:: 2>&1 | grep -E "^error|cannot find"| head`
Expected: `cannot find function `pending_for` in this scope` 之类的编译错误。

- [ ] **Step 3: 写实现**

在 `automation.rs` 的 `status_line` **之后**、`mod tests` **之前**插入：

```rust
use mullion_store::{build_plan, ResolvedAutomation, SessionId, Step};

/// 已经算好、等着 `ConnectOk` 抵达时启用的计划。
///
/// **在 `spawn_connect`（用户点「连接」的那一帧）算，不在 `ConnectOk` 里算。**
/// 两个理由：其一，`UserEvent::ConnectOk` 不携带 `SessionId`，到那时只能读
/// `ui.connect_request_last`，而连接在途期间用户完全可能改了配置甚至删了这条
/// 会话——发出去的字节就跟他当初点「连接」时看到的配置对不上了。其二，
/// `store.resolved()` 是同步查库，放在用户点击的那一帧语义清楚。
pub struct PendingAutomation {
    pub steps: Vec<Step>,
    pub ready_timeout_ms: u32,
}

/// 算这条会话的待办计划。`None` = 不跑自动化。
///
/// `lookup` 由调用方注入（返回「解析后的自动化配置」+「会话名」），而不是在
/// 这里直接够 `SessionStore`——注入之后本函数可以脱离会话库单测，
/// 「没有 SessionId 就绝不查库」这条性质才验得动（见
/// `cli_direct_connect_has_no_plan`）。同样的注入手法在
/// `session_manager::editor::ensure_key_candidates_scanned` 已有先例。
///
/// `id` 为 `None` 即 CLI 直连（`mullion user@host -p 22 -i key`）：它压根没有
/// 会话记录，没有配置来源，**凭空猜一个 tmux 会话名去 attach 是最坏的行为**。
/// 这不是退化：CLI 直连本来就是调试入口，要自动化就存成会话。
pub fn pending_for(
    id: Option<SessionId>,
    lookup: impl FnOnce(SessionId) -> Option<(ResolvedAutomation, String)>,
) -> Option<PendingAutomation> {
    let id = id?;
    let (resolved, name) = lookup(id)?;
    let steps = build_plan(&resolved, &name);
    if steps.is_empty() {
        return None;
    }
    Some(PendingAutomation {
        steps,
        ready_timeout_ms: resolved.ready_timeout_ms,
    })
}

/// `ConnectOk` 抵达时：取走待办计划与一次性跳过标志，回答「这次到底起不起」。
///
/// 两个 `&mut` 都是**取走**语义，这正是本函数存在的理由：计划和跳过标志都必须
/// 恰好生效一次。分成「判断」和「消费」两个函数的话，消费点会散进 `app.rs`，
/// 而 `App` 在无头环境构造不出来——「跳过只生效一次」这条性质就再也测不动了。
pub fn take_pending(
    pending: &mut Option<PendingAutomation>,
    skip: &mut bool,
) -> Option<PendingAutomation> {
    let plan = pending.take();
    if std::mem::take(skip) {
        return None;
    }
    plan
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib automation:: > /tmp/t3.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/t3.log`
Expected: `test result: ok. 10 passed`

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/automation.rs
git commit -m "feat(app): 自动化计划的计算与一次性消费 (F40/F44)

pending_for 注入 lookup 而不是直接够 SessionStore —— 「没有 SessionId 就
绝不查库」(CLI 直连)这条性质才验得动。take_pending 把「取计划」与「消费
跳过标志」合成一个函数,「跳过只生效一次」不至于散进构造不出来的 App。"
```

---

## Task 4: 多行粘贴拆条（纯函数）

**Files:**
- Modify: `crates/mullion-app/src/automation.rs`

数据层已经会静默丢弃含控制字符的命令（`automation.rs::command_texts` 的 `filter`），UI 必须在进库前把多行拆开，否则用户配了一条会凭空消失的命令。

- [ ] **Step 1: 写失败的测试**

在 `mod tests` 里追加：

```rust
    /// F41:粘贴多行 ⟹ 拆成多条,空行丢弃,每条 trim。
    ///
    /// 拆条而不是剥换行:`a && b` 与 `a; b` 语义不同,静默拼接会改变行为;
    /// 拆条则结果仍符合数据层语义(默认仍合成一行用 `;` 连接发出)。
    ///
    /// 自证会变红:把 `.filter(|s| !s.is_empty())` 删掉(第二条断言红)。
    #[test]
    fn split_pasted_commands_splits_and_drops_blank_lines() {
        assert_eq!(
            split_pasted_commands("cd /srv\nls -la"),
            vec!["cd /srv".to_string(), "ls -la".to_string()]
        );
        assert_eq!(
            split_pasted_commands("cd /srv\n\n  ls -la  \n"),
            vec!["cd /srv".to_string(), "ls -la".to_string()]
        );
    }

    /// CRLF 与裸 CR 都要拆。裸 `\r` 在「一行模型」里就是一次额外回车,
    /// 漏掉它整条命令会被数据层丢弃。
    ///
    /// 自证会变红:把 `split(['\n', '\r'])` 改成 `split('\n')`。
    #[test]
    fn split_pasted_commands_handles_crlf_and_bare_cr() {
        assert_eq!(
            split_pasted_commands("a\r\nb\rc"),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    /// 单行原样(只 trim),全空白得空表。
    #[test]
    fn split_pasted_commands_leaves_a_single_line_alone() {
        assert_eq!(
            split_pasted_commands("  tmux ls  "),
            vec!["tmux ls".to_string()]
        );
        assert!(split_pasted_commands(" \r\n\t ").is_empty());
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib automation:: 2>&1 | grep -E "cannot find" | head -1`
Expected: `cannot find function `split_pasted_commands` in this scope`

- [ ] **Step 3: 写实现**

在 `take_pending` 之后插入：

```rust
/// F41:把一段可能含换行的文本拆成若干条命令。
///
/// 空行丢弃、逐条 `trim`。`\r` 与 `\n` 都算行分隔——裸 `\r` 在「整个计划只有
/// 一行」的模型里就是一次额外回车（正是设计 §2 要挡的东西），漏掉它这条命令
/// 会被数据层静默丢弃，用户配了个会凭空消失的命令。
pub fn split_pasted_commands(raw: &str) -> Vec<String> {
    raw.split(['\n', '\r'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib automation:: > /tmp/t4.log 2>&1; grep -nE "test result|FAILED" /tmp/t4.log`
Expected: `test result: ok. 13 passed`

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/automation.rs
git commit -m "feat(app): 多行粘贴拆成多条命令的纯函数 (F41)"
```

---

## Task 5: `run()` —— 假时钟下的完整调度闭环

**Files:**
- Modify: `crates/mullion-app/src/automation.rs`

这是整个切片的核心。三条一次性通道 + 一个超时，全在 tokio 里闭环。

- [ ] **Step 1: 写失败的测试**

在 `mod tests` 里追加：

```rust
    use mullion_ssh::schedule::ByteSink;
    use mullion_ssh::session::TrySendErr;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// 假 sink:记录收到的字节。**零网络**。
    #[derive(Default)]
    struct FakeSink {
        written: Mutex<Vec<Vec<u8>>>,
        /// 置真后一律返回 Closed(模拟 channel 已关)。
        closed: bool,
    }

    impl FakeSink {
        fn written(&self) -> Vec<Vec<u8>> {
            self.written.lock().unwrap().clone()
        }
    }

    impl ByteSink for FakeSink {
        fn write(&self, bytes: Vec<u8>) -> Result<(), TrySendErr> {
            if self.closed {
                return Err(TrySendErr::Closed);
            }
            self.written.lock().unwrap().push(bytes);
            Ok(())
        }
    }

    /// 两步计划:300ms 后 `a\r`,再 200ms 后 `b\r`。
    fn two_steps() -> Vec<Step> {
        vec![
            Step {
                delay: Duration::from_millis(300),
                bytes: b"a\r".to_vec(),
            },
            Step {
                delay: Duration::from_millis(200),
                bytes: b"b\r".to_vec(),
            },
        ]
    }

    /// 三条一次性通道 + sink,一把造好。
    fn harness() -> (
        Arc<FakeSink>,
        tokio::sync::oneshot::Sender<()>,
        tokio::sync::oneshot::Sender<()>,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<Outcome>,
    ) {
        let fake = Arc::new(FakeSink::default());
        let sink: Arc<dyn ByteSink> = fake.clone();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        let (disc_tx, disc_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(run(
            sink,
            two_steps(),
            ready_rx,
            cancel_rx,
            disc_rx,
            15_000,
        ));
        (fake, ready_tx, cancel_tx, disc_tx, task)
    }

    /// 首字节到达之前一个字节都不许发 —— 核心约束:没确认对端已经在说话,
    /// 就没法确认那是不是一个干净 shell。
    ///
    /// 自证会变红:把 `run` 里第一段 `select!` 整个删掉,直接跑 `write_scheduled`。
    #[tokio::test(start_paused = true)]
    async fn ready_signal_starts_the_plan() {
        let (fake, ready_tx, _cancel_tx, _disc_tx, task) = harness();

        // 假时钟推 10 秒:没收到 ready,一个字节都不该发。
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert!(
            fake.written().is_empty(),
            "还没收到首字节就发了字节:{:?}",
            fake.written()
        );

        ready_tx.send(()).unwrap();
        let out = task.await.unwrap();
        assert_eq!(out, Outcome::Completed(2));
        assert_eq!(fake.written(), vec![b"a\r".to_vec(), b"b\r".to_vec()]);
    }

    /// 超时:一个字节都不发,**绝不补发**。
    ///
    /// 自证会变红:把 `sleep(timeout)` 那条分支的返回值改成继续跑计划。
    #[tokio::test(start_paused = true)]
    async fn no_first_byte_within_timeout_is_skipped() {
        let fake = Arc::new(FakeSink::default());
        let sink: Arc<dyn ByteSink> = fake.clone();
        // 注意:三个 sender 必须用具名绑定持有。写成裸 `_` 会当场 drop,
        // 接收端立刻就绪 → 走成取消/断线分支,测试会莫名其妙变红。
        let (_ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (_cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        let (_disc_tx, disc_rx) = tokio::sync::oneshot::channel();

        let out = run(sink, two_steps(), ready_rx, cancel_rx, disc_rx, 15_000).await;

        assert_eq!(out, Outcome::SkippedTimeout { after_ms: 15_000 });
        assert!(fake.written().is_empty(), "超时后不许补发任何字节");
    }

    /// 等待期用户就接管了(连上就急着敲键盘)。
    #[tokio::test(start_paused = true)]
    async fn cancel_before_ready_aborts() {
        let (fake, _ready_tx, cancel_tx, _disc_tx, task) = harness();
        cancel_tx.send(()).unwrap();
        assert_eq!(task.await.unwrap(), Outcome::Aborted);
        assert!(fake.written().is_empty());
    }

    /// 执行到一半用户接管:**剩余步骤一个字节都不再发**。
    ///
    /// 自证会变红:`run` 里把 `cancel` 换成一条新建的、永不就绪的接收端
    /// 再交给 `write_scheduled`。
    #[tokio::test(start_paused = true)]
    async fn cancel_during_run_stops_remaining_steps() {
        let (fake, ready_tx, cancel_tx, _disc_tx, task) = harness();
        ready_tx.send(()).unwrap();
        // 推进到第一步已发、第二步还在等的时刻。
        tokio::time::sleep(Duration::from_millis(350)).await;
        cancel_tx.send(()).unwrap();

        assert_eq!(task.await.unwrap(), Outcome::Aborted);
        assert_eq!(
            fake.written(),
            vec![b"a\r".to_vec()],
            "取消后剩余步骤一个字节都不许发(用户接管优先)"
        );
    }

    /// 等待期断线:结局必须是 `Disconnected` 而不是 `Aborted`。
    ///
    /// 这正是断线要独占一条 oneshot 的理由:复用 `cancel` 的话,
    /// `write_scheduled` 会返回 `Cancelled`,状态栏就显示「检测到你的输入」——
    /// 用户根本没输入。
    ///
    /// 自证会变红:把 `run` 第一段 `select!` 里 `disconnect` 那条分支的返回值
    /// 改成 `Outcome::Aborted`。
    #[tokio::test(start_paused = true)]
    async fn disconnect_before_ready_reports_disconnected_not_aborted() {
        let (fake, _ready_tx, _cancel_tx, disc_tx, task) = harness();
        disc_tx.send(()).unwrap();
        assert_eq!(task.await.unwrap(), Outcome::Disconnected);
        assert!(fake.written().is_empty());
    }

    /// 执行期断线同样报 `Disconnected`。
    #[tokio::test(start_paused = true)]
    async fn disconnect_during_run_reports_disconnected() {
        let (_fake, ready_tx, _cancel_tx, disc_tx, task) = harness();
        ready_tx.send(()).unwrap();
        tokio::time::sleep(Duration::from_millis(350)).await;
        disc_tx.send(()).unwrap();
        assert_eq!(task.await.unwrap(), Outcome::Disconnected);
    }

    /// sink 已关 ⟹ `write_scheduled` 自己就会报 `Disconnected`,
    /// 这条路径不依赖 app 侧的断线信号。
    #[tokio::test(start_paused = true)]
    async fn closed_sink_reports_disconnected() {
        let fake = Arc::new(FakeSink {
            closed: true,
            ..Default::default()
        });
        let sink: Arc<dyn ByteSink> = fake.clone();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (_cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        let (_disc_tx, disc_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(run(sink, two_steps(), ready_rx, cancel_rx, disc_rx, 15_000));
        ready_tx.send(()).unwrap();
        assert_eq!(task.await.unwrap(), Outcome::Disconnected);
    }

    /// `initial_delay_ms` 已经是第一个 Step 的 delay,`run` 收到 ready 后
    /// **不许再等一次** —— 那会让实际延时翻倍,而这种 bug 在真机上只表现为
    /// 「感觉慢了一点」,没人会发现。
    ///
    /// 自证会变红:在 `run` 收到 ready 之后、`write_scheduled` 之前插一条
    /// `tokio::time::sleep(Duration::from_millis(300)).await;`。
    #[tokio::test(start_paused = true)]
    async fn ready_does_not_add_an_extra_initial_delay() {
        let fake = Arc::new(FakeSink::default());
        let sink: Arc<dyn ByteSink> = fake.clone();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (_cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        let (_disc_tx, disc_rx) = tokio::sync::oneshot::channel();
        let steps = vec![Step {
            delay: Duration::from_millis(300),
            bytes: b"a\r".to_vec(),
        }];
        let task = tokio::spawn(run(sink, steps, ready_rx, cancel_rx, disc_rx, 15_000));

        ready_tx.send(()).unwrap();
        let start = tokio::time::Instant::now();
        assert_eq!(task.await.unwrap(), Outcome::Completed(1));
        // 硬编码 300:不能拿 `DEFAULT_INITIAL_DELAY_MS` 当预期值(重言式)。
        assert_eq!(
            start.elapsed(),
            Duration::from_millis(300),
            "ready 之后只该等计划自己的 300ms,多等一次就是延时翻倍"
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib automation:: 2>&1 | grep -E "cannot find function .run." | head -1`
Expected: 报 `run` 未定义。

- [ ] **Step 3: 写实现**

在 `split_pasted_commands` 之后插入：

```rust
use std::sync::Arc;
use std::time::Duration;

use mullion_ssh::schedule::{write_scheduled, ByteSink, ScheduleOutcome};
use tokio::sync::oneshot;

/// 跑一次自动化。**整条链路在这里闭环，`ControlFlow` 一行都不动**（spec §1 修订一）。
///
/// 四条边，三条是一次性通道、一条是自带的超时：
/// - `ready`：winit 线程发现该 pane 收到了第一个入站字节。
/// - `cancel`：用户接管（敲键 / 滚轮上报 / 粘贴）。**必须只由 app.rs 里的用户
///   意图写入点触发**，见 `App::user_took_over` 的文档。
/// - `disconnect`：pane 被 `Workspace::pump` 标成 `Disconnected`。独占一条通道
///   而不是复用 `cancel`：复用的话结局会变成 `Aborted`，状态栏显示「检测到你的
///   输入」——用户根本没输入。
/// - `ready_timeout_ms`：从本 task 被 spawn 起算。
///
/// 已知偏差：设计要求「从 `open_pty` 返回起算」，实际起点是 task 被 spawn 的
/// 时刻，晚一次 winit 事件投递（毫秒级）。远小于 15s 默认值，不做补偿。
pub async fn run(
    sink: Arc<dyn ByteSink>,
    steps: Vec<Step>,
    ready: oneshot::Receiver<()>,
    mut cancel: oneshot::Receiver<()>,
    mut disconnect: oneshot::Receiver<()>,
    ready_timeout_ms: u32,
) -> Outcome {
    let steps_len = steps.len();
    let timeout = Duration::from_millis(u64::from(ready_timeout_ms));

    // 第一段：等首字节。`biased` = 取消/断线优先，同时就绪时不会「刚被取消却
    // 又起了计划」。
    tokio::select! {
        biased;
        _ = &mut cancel => return Outcome::Aborted,
        _ = &mut disconnect => return Outcome::Disconnected,
        r = ready => {
            // Err = 发送端被 drop（handle 没了 / App 退出）。理论上 `cancel`
            // 会先于它就绪（同时 drop + biased），这里是防御性兜底。
            if r.is_err() {
                return Outcome::Aborted;
            }
        }
        _ = tokio::time::sleep(timeout) => {
            return Outcome::SkippedTimeout { after_ms: ready_timeout_ms };
        }
    }

    // 第二段：按时间表发。**不在这里再 sleep 一次 `initial_delay_ms`** ——
    // `build_plan` 生成的第一个 Step 的 delay 就是它，多等一次就是延时翻倍。
    let plan: Vec<(Duration, Vec<u8>)> =
        steps.into_iter().map(|s| (s.delay, s.bytes)).collect();
    let sched = write_scheduled(sink, plan, cancel);
    tokio::pin!(sched);
    tokio::select! {
        biased;
        _ = &mut disconnect => Outcome::Disconnected,
        out = &mut sched => match out {
            ScheduleOutcome::Completed => Outcome::Completed(steps_len),
            ScheduleOutcome::Cancelled => Outcome::Aborted,
            ScheduleOutcome::Disconnected => Outcome::Disconnected,
            ScheduleOutcome::Congested => Outcome::Congested,
        },
    }
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib automation:: > /tmp/t5.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/t5.log`
Expected: `test result: ok. 21 passed`

- [ ] **Step 5: clippy**

Run: `cargo clippy -p mullion-app --all-targets -- -D warnings 2>&1 | tail -5`
Expected: 无输出（或仅 `Finished`）。

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-app/src/automation.rs
git commit -m "feat(app): 自动化调度闭环 —— 首字节/超时/接管/断线四条边 (F40~F44)

超时与延时全在 tokio 的 select! 里,不碰 ControlFlow(T7:那三个分支的
control_flow 复位是本项目已知最恶的坑,为一个与渲染无关的业务 deadline
去改它是重新布雷)。断线独占一条 oneshot 而不是复用 cancel —— 复用会让
状态栏对着一个没输入过的用户说「检测到你的输入」。

假时钟(start_paused)+ 假 sink 零网络覆盖 9 条路径。"
```

---

## Task 6: pane 的首字节标志

**Files:**
- Modify: `crates/mullion-app/src/shell/workspace/mod.rs`（`PaneState`、`Workspace::pump`、测试里的 `fake_pane`）

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-app/src/shell/workspace/mod.rs` 的 `mod tests` 里，紧跟在 `pty_write_goes_to_its_own_pane_channel_t1` 之后追加：

```rust
    /// 首字节检测:只有真的收到过入站字节的 pane 才置位。
    ///
    /// 不让 `pump` 返回 `Vec<PaneId>`:`pump` 每帧必调,返回 Vec 等于每帧一次
    /// 堆分配,本项目对帧路径上的无谓开销一向敏感(T3)。挂个 bool 在
    /// `PaneState` 上零分配,而且「这个 pane 收到过字节没有」本来就是
    /// pane 自己的状态。
    ///
    /// 自证会变红:把 `pump` 里 `p.saw_first_byte = true;` 挪到
    /// `if inbound.is_empty() { continue; }` **之前**(1 号 pane 会被误置位)。
    #[tokio::test]
    async fn pump_marks_saw_first_byte_only_for_panes_with_inbound() {
        let (mut ws, probes) = ws_with(2);
        assert!(!ws.pane(PaneId(1)).unwrap().saw_first_byte, "初值必须是 false");
        assert!(!ws.pane(PaneId(2)).unwrap().saw_first_byte);

        // 只给 2 号 pane 喂字节。
        probes[1].tx.send(b"hello".to_vec()).await.unwrap();
        tokio::task::yield_now().await;
        ws.pump(0);

        assert!(
            !ws.pane(PaneId(1)).unwrap().saw_first_byte,
            "1 号 pane 什么都没收到,不该置位 —— 置位了就会让自动化在一条还没
             说话的 channel 上开跑"
        );
        assert!(
            ws.pane(PaneId(2)).unwrap().saw_first_byte,
            "2 号 pane 收到了字节却没置位 —— 自动化会一直等到超时"
        );
    }

    /// 置位之后不会被后续的空帧清掉:它是「历史上收到过」而不是「这一帧收到了」。
    ///
    /// 自证会变红:把 `p.saw_first_byte = true;` 改成
    /// `p.saw_first_byte = !inbound.is_empty();` 并挪到 `continue` 之前。
    #[tokio::test]
    async fn saw_first_byte_is_sticky_across_idle_frames() {
        let (mut ws, probes) = ws_with(1);
        probes[0].tx.send(b"hello".to_vec()).await.unwrap();
        tokio::task::yield_now().await;
        ws.pump(0);
        assert!(ws.pane(PaneId(1)).unwrap().saw_first_byte);

        ws.pump(16); // 空帧
        assert!(
            ws.pane(PaneId(1)).unwrap().saw_first_byte,
            "首字节标志必须是粘的,否则空帧会把它清掉、自动化永远等不到就绪"
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib workspace 2>&1 | grep -E "no field .saw_first_byte" | head -1`
Expected: 报 `PaneState` 没有 `saw_first_byte` 字段。

- [ ] **Step 3: 加字段并在 pump 里置位**

3a. `PaneState`（`shell/workspace/mod.rs:62-74`）在 `pub status: PaneStatus,` **之后**插入：

```rust
    /// 这个 pane 收到过任何入站字节没有。**粘性**：一旦为真就不再变回假。
    ///
    /// 自动化的「就绪」判据（设计 §2）：远端已经在说话，才说明我们拿到的是
    /// 一个活着的 login shell。`App` 每帧查它，不让 `pump` 返回 `Vec<PaneId>`
    /// ——`pump` 每帧必调，返回 Vec 等于每帧一次堆分配（T3）。
    pub saw_first_byte: bool,
```

3b. `Workspace::pump`（`:230-255`）里，把

```rust
            if inbound.is_empty() {
                continue;
            }
            let out = session_pump::pump(&mut p.emulator, &inbound);
```

改成

```rust
            if inbound.is_empty() {
                continue;
            }
            // 粘性置位。必须在 `continue` **之后** —— 放前面的话每个 pane 每帧
            // 都会被置位，自动化会在一条还没说过话的 channel 上开跑。
            p.saw_first_byte = true;
            let out = session_pump::pump(&mut p.emulator, &inbound);
```

3c. 测试脚手架 `fake_pane`（`shell/workspace/mod.rs` 的 `mod tests` 内）里，`status: PaneStatus::Live,` 之后加一行：

```rust
                saw_first_byte: false,
```

- [ ] **Step 4: 补齐 app.rs 的两处构造点**

`crates/mullion-app/src/app.rs` 里有两处 `PaneState { ... }` 字面量：`ConnectOk` 分支（约 `:846`）与 `PaneOpened` 分支（约 `:900`）。两处都在 `status: crate::shell::workspace::PaneStatus::Live,` 之后加：

```rust
                        saw_first_byte: false,
```

（缩进跟随各自上下文。`grep -n "PaneStatus::Live," crates/mullion-app/src/app.rs` 找准这两处。）

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib > /tmp/t6.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/t6.log`
Expected: `test result: ok.`，新增 2 条通过。

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-app/src/shell/workspace/mod.rs crates/mullion-app/src/app.rs
git commit -m "feat(app): pane 的首字节标志 —— 自动化的就绪判据 (F40)

粘性 bool 挂在 PaneState 上,不让 pump 返回 Vec<PaneId>:pump 每帧必调,
返回 Vec 等于每帧一次堆分配(T3)。置位必须在 `inbound.is_empty()` 的
continue 之后,否则每个 pane 每帧都置位。"
```

---

## Task 7: App 字段与「点连接那一帧算计划」

**Files:**
- Modify: `crates/mullion-app/src/app.rs`（`App` 结构体、`App::new`、`spawn_connect`）

- [ ] **Step 1: 加 App 字段**

`crates/mullion-app/src/app.rs` 的 `pub struct App {` 里，在最后一个字段 `probe_task: Option<tokio::task::JoinHandle<()>>,` **之后**插入：

```rust
    /// F40~F44:正在跑的那一次自动化。`None` = 没在跑。
    automation: Option<AutomationHandle>,
    /// `spawn_connect` 算好、等 `ConnectOk` 抵达时启用的计划。
    ///
    /// 在 `spawn_connect`（用户点击那一帧）算而不是 `ConnectOk` 里算：
    /// `ConnectOk` 不携带 `SessionId`，到那时只能读 `ui.connect_request_last`，
    /// 而连接在途期间用户完全可能改了配置甚至删了这条会话。
    pending_automation: Option<crate::automation::PendingAutomation>,
    /// F44 右键「连接（跳过自动化）」的一次性标志。`ConnectOk` 消费后立即清零。
    pending_skip_automation: bool,
    /// 上一次自动化的结论文案。一直显示到下一次 `spawn_connect` 才清空 ——
    /// 不做定时淡出：状态栏本来就是常驻信息区，而定时清除需要再引一个
    /// deadline 进帧循环，正是 spec §1 修订一要避免的东西。
    automation_status: Option<String>,
```

`App::new` 的 `Self { ... }` 里，`probe_task: None,` 之后插入：

```rust
            automation: None,
            pending_automation: None,
            pending_skip_automation: false,
            automation_status: None,
```

- [ ] **Step 2: 定义 `AutomationHandle`**

在 `app.rs` 里 `struct Active { ... }` 定义 **之前**（紧跟 `UserEvent` 枚举之后）插入：

```rust
/// 一次在途自动化的把手。三条通道都是 `Option`，因为每一条都是**一次性边**：
/// `take()` 天然保证不会重复触发，也省掉一个「是否已触发」的布尔标志。
struct AutomationHandle {
    /// 只认这一个 pane 的首字节。总设计 §7 前提②：分屏新开的 pane 是干净
    /// shell，不重复跑自动化（所有 pane attach 同一个 tmux session 会内容
    /// 镜像，且 `window-size` 取 `latest` 会反复 reflow、取 `smallest` 会留白）。
    pane: PaneId,
    /// C1：跨「断开→重连」世代过滤，同 `PaneOpened`。高延迟链路下，用户完全
    /// 可能在自动化还在跑的时候断开重连，旧世代的结论落到新连接的状态栏上，
    /// 是一条与当前连接毫不相干的误导信息。
    generation: u64,
    ready: Option<tokio::sync::oneshot::Sender<()>>,
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
    disconnect: Option<tokio::sync::oneshot::Sender<()>>,
    /// 换新连接时 abort：旧那次的结论对新连接没有意义。
    task: tokio::task::JoinHandle<()>,
}
```

- [ ] **Step 3: 在 `spawn_connect` 里算计划**

`fn spawn_connect(&mut self, cfg: SshConfig)` 开头，`self.last_cfg = Some(cfg.clone());` **之前**插入：

```rust
        // F40~F44:此刻才确定「是哪条会话」。连接在途期间用户可能改配置甚至
        // 删会话，所以计划必须在用户点击的这一帧定死。
        // 上一次的结论到此为止：新连接开始了，旧结论就是误导信息。
        self.automation_status = None;
        self.pending_automation = crate::automation::pending_for(
            self.ui.connect_request_last,
            |id| {
                let store = self.store.as_ref()?;
                let resolved = store.resolved(id).ok()?;
                // `ResolvedConfig` 不含会话名，而 `build_plan` 要它做 tmux 的
                // fallback_name（用户没填 tmux 会话名时按会话名推导）。
                let name = store
                    .list()
                    .iter()
                    .find(|r| r.id == id)?
                    .identity
                    .name
                    .clone();
                Some((resolved.automation, name))
            },
        );
```

- [ ] **Step 4: 编译**

Run: `cargo build -p mullion-app 2>&1 | grep -E "^error" | head`
Expected: 无 `error`。

**本 Task 故意不跑 clippy。** `automation` / `pending_skip_automation` / `AutomationHandle` 这一刻还没有任何读取点，`-D warnings` 必然报 `field is never read` / `struct is never constructed`。这不是缺陷，是 Task 切分的必然结果——Task 8 立刻把它们用上。**绝不要为此加 `#[allow(dead_code)]`**：那个 `allow` 加上去就不会有人再摘掉，将来真的死掉的字段就永远没人发现。clippy 在 Task 9 结束时统一跑。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): 自动化的 App 状态位与「点连接那一帧算计划」 (F40~F44)

计划在 spawn_connect 算而不是 ConnectOk:ConnectOk 不带 SessionId,到那时
只能读 connect_request_last,而连接在途期间用户可能已经改了配置甚至删了
这条会话 —— 发出去的字节就跟他点「连接」时看到的配置对不上了。"
```

---

## Task 8: `ConnectOk` 里启用自动化

**Files:**
- Modify: `crates/mullion-app/src/app.rs`（`UserEvent::ConnectOk` 分支）

- [ ] **Step 1: 改 `ConnectOk` 分支**

把 `UserEvent::ConnectOk { ssh, rx, handle } => {` 分支里这一段：

```rust
                let mut ws = crate::shell::workspace::Workspace::new(
                    PaneState {
                        id: PaneId(1),
                        host_ix: 0,
                        emulator,
                        pty: Box::new(ssh),
```

改成：

```rust
                // pane 和自动化 task 要共享同一条 channel（spec §1 修订二）：
                // `PaneState.pty` 本来就是 `Box<dyn PtyWriter>`，`SshSession`
                // 内部只有一个 mpsc Sender、本身 Send+Sync，`Arc` 只是共享
                // 所有权，不引入锁。既有调用点零改动。
                let ssh = Arc::new(ssh);
                let mut ws = crate::shell::workspace::Workspace::new(
                    PaneState {
                        id: PaneId(1),
                        host_ix: 0,
                        emulator,
                        pty: Box::new(ssh.clone()),
```

然后在该分支末尾、`self.request_ui_redraw();` **之前**插入：

```rust
                // F40~F44:起自动化。旧那次(如果有)的结论对新连接没有意义。
                if let Some(old) = self.automation.take() {
                    old.task.abort();
                }
                if let Some(plan) = crate::automation::take_pending(
                    &mut self.pending_automation,
                    &mut self.pending_skip_automation,
                ) {
                    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
                    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
                    let (disc_tx, disc_rx) = tokio::sync::oneshot::channel();
                    let sink: Arc<dyn mullion_ssh::schedule::ByteSink> = ssh;
                    let proxy = self.proxy.clone();
                    let steps = plan.steps.len();
                    let timeout_ms = plan.ready_timeout_ms;
                    log::info!(
                        target: "mullion",
                        "自动化:{steps} 步待发,就绪超时 {timeout_ms}ms"
                    );
                    let task = self._runtime.spawn(async move {
                        let outcome = crate::automation::run(
                            sink,
                            plan.steps,
                            ready_rx,
                            cancel_rx,
                            disc_rx,
                            timeout_ms,
                        )
                        .await;
                        let _ = proxy.send_event(UserEvent::AutomationDone(generation, outcome));
                    });
                    self.automation = Some(AutomationHandle {
                        // 只有第一个 pane 跑自动化(总设计 §7 前提②)。
                        pane: PaneId(1),
                        generation,
                        ready: Some(ready_tx),
                        cancel: Some(cancel_tx),
                        disconnect: Some(disc_tx),
                        task,
                    });
                }
```

> **注意**：`generation` 是本分支上面已经算好的局部变量（`let generation = self.next_ws_generation;`），直接用；不要再读 `ws.generation()`——`ws` 此时已经 move 进 `self.ws`。

- [ ] **Step 2: 加 `UserEvent::AutomationDone` 变体**

在 `pub enum UserEvent { ... }` 里，`ProbeErr(u64, String),` **之后**插入：

```rust
    /// F40~F44:一次自动化结束。`u64` 是发起时的 `Workspace` 世代号，
    /// 过期的直接丢（同 `PaneOpenErr::generation`）。
    AutomationDone(u64, crate::automation::Outcome),
```

（`user_event` 的 match 分支在 Task 11 加；本步先加变体会让 match 不穷尽而编译失败——所以 **Task 8 与 Task 11 的 match 分支要一起编译通过**。为保持每个 Task 可独立编译，本步同时加一个最小分支：）

在 `user_event` 的 `UserEvent::ProbeErr(epoch, msg) => { ... }` 之后插入：

```rust
            UserEvent::AutomationDone(generation, outcome) => {
                self.accept_automation_done(generation, outcome);
            }
```

并在 `impl App` 里（`fn spawn_key_picker` 之后）加最小实现，Task 11 会把状态栏接线补全：

```rust
    /// 自动化结束。**必须按世代过滤**：高延迟链路下用户完全可能在自动化还在
    /// 跑的时候断开重连，旧世代的「自动化已中止：连接已断开」落到新连接的
    /// 状态栏上，是一条与当前连接毫不相干的误导信息（判据同 `PaneOpenErr`）。
    fn accept_automation_done(&mut self, generation: u64, outcome: crate::automation::Outcome) {
        if !self
            .ws
            .as_ref()
            .is_some_and(|ws| generation_matches(ws, generation))
        {
            log::debug!(target: "mullion", "丢弃过期世代 {generation} 的自动化结论");
            return;
        }
        log::info!(target: "mullion", "自动化结束: {outcome:?}");
        self.automation_status = Some(crate::automation::status_text(outcome));
        self.automation = None;
        self.ui_dirty = true;
        self.request_ui_redraw();
    }
```

- [ ] **Step 3: 编译并跑全量测试**

Run: `cargo test -p mullion-app --lib > /tmp/t8.log 2>&1; grep -nE "test result|FAILED|error\[" /tmp/t8.log`
Expected: `test result: ok.`

- [ ] **Step 4: 提交**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): ConnectOk 起自动化 + 结论按世代过滤 (F40~F44)

pane 与自动化 task 共享 Arc<SshSession>。AutomationDone 必须按世代过滤:
高延迟链路下用户可能在自动化还在跑时断开重连,旧世代的「连接已断开」
落到新连接的状态栏上是一条毫不相干的误导信息(判据同 PaneOpenErr)。"
```

---

## Task 9: 首字节与断线两条边（每帧查）

**Files:**
- Modify: `crates/mullion-app/src/app.rs`（`pump_io`）

- [ ] **Step 1: 在 `pump_io` 后面驱动自动化**

把 `fn pump_io(&mut self)`（约 `:529`）改成：

```rust
    fn pump_io(&mut self) {
        let now = self.now_ms();
        if let Some(ws) = self.ws.as_mut() {
            ws.pump(now);
        }
        self.drive_automation();
    }

    /// 首字节 / 断线两条边。挂在 `pump_io` 上而不是重绘上：最小化期间窗口
    /// 未必还会被重绘，但 `Wake` 仍会驱动 `pump_io`——否则用户最小化着连上，
    /// 自动化会一直等到超时。
    ///
    /// 每帧调，所以**零分配**：只读两个 bool、`take()` 两个 `Option`。
    fn drive_automation(&mut self) {
        let Some(h) = self.automation.as_mut() else {
            return;
        };
        let Some(ws) = self.ws.as_ref() else {
            return;
        };
        // pane 不在了（被关掉/换世代）：让 task 自然结束，别让它挂到超时。
        let Some(pane) = ws.pane(h.pane) else {
            h.disconnect.take();
            return;
        };
        if pane.status == crate::shell::workspace::PaneStatus::Disconnected {
            // send 的 Err（接收端已走）无所谓：task 已经结束了。
            if let Some(tx) = h.disconnect.take() {
                let _ = tx.send(());
            }
            return;
        }
        if pane.saw_first_byte {
            if let Some(tx) = h.ready.take() {
                let _ = tx.send(());
            }
        }
    }
```

- [ ] **Step 2: 编译 + 全量测试**

Run: `cargo test -p mullion-app --lib > /tmp/t9.log 2>&1; grep -nE "test result|FAILED|error\[" /tmp/t9.log`
Expected: `test result: ok.`

- [ ] **Step 3: clippy**

Run: `cargo clippy -p mullion-app --all-targets -- -D warnings 2>&1 | tail -3`
Expected: 无 warning。

- [ ] **Step 4: 提交**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): 首字节与断线两条边挂在 pump_io 上 (F40)

挂 pump_io 而不是重绘:最小化期间窗口未必再被重绘,但 Wake 仍驱动
pump_io —— 否则用户最小化着连上,自动化会一直等到超时。"
```

---

## Task 10: 用户接管 —— 四个写入点接 cancel（含 T1 守护）

**Files:**
- Modify: `crates/mullion-app/src/app.rs`（`send_paste`、`MouseWheel`、`KeyboardInput`、新增 `user_took_over` 与守护测试）

**这个 Task 是本切片最容易埋雷的地方。** 判据只能是 app.rs 里的**用户意图**写入点；`Workspace::pump` 里的 `p.pty.write(out)`（`shell/workspace/mod.rs:252`）是 **T1 的 PtyWrite 应答**（DSR 光标查询、同步输出探测的回应），挂上去的话远端一发探测自动化就自杀，现象是「有时候能跑有时候跑不了」，极难定位。

- [ ] **Step 1: 加 `user_took_over`**

在 `impl App` 里（`fn send_paste` 之前）插入：

```rust
    /// 用户开始输入 ⟹ 自动化让位（设计 §2 的「用户接管优先」）。
    ///
    /// **只能从 `app.rs` 里用户意图的 PTY 写入点调。** `Workspace::pump` 里
    /// 那处 `p.pty.write(out)` 是 T1 的 PtyWrite 应答（DSR 光标查询、同步输出
    /// 探测的回应），不是用户输入——把取消挂在 `PtyWriter::write` 上，远端一发
    /// 同步输出探测自动化就自杀，而且现象是「有时候能跑有时候跑不了」。
    ///
    /// 将来新增用户输入路径（如鼠标按钮上报 F15）也必须一并接上这里。
    /// 当前的四处以 `grep -n "pty.write" crates/mullion-app/src/app.rs` 为准
    /// （行号会漂，别钉死）。
    fn user_took_over(&mut self) {
        if let Some(h) = self.automation.as_mut() {
            // drop 发送端即取消（`write_scheduled` 的 doc：收到值**或**发送端
            // 被 drop 都算取消）。
            h.cancel.take();
        }
    }
```

- [ ] **Step 2: 接第一处 —— 粘贴（`send_paste`）**

`fn send_paste(&mut self, text: &str)` 末尾，把

```rust
        pane.emulator.scroll_to_bottom();
        let _ = pane.pty.write(bytes);
    }
```

改成

```rust
        pane.emulator.scroll_to_bottom();
        let _ = pane.pty.write(bytes);
        // `pane` 的借用到此结束，才能再借 `&mut self`。
        self.user_took_over();
    }
```

- [ ] **Step 3: 接第二、三处 —— 滚轮上报（`WindowEvent::MouseWheel`）**

在 `WindowEvent::MouseWheel { delta, .. } => {` 分支里，`let local = self.cursor_in_grid();` **之前**插入：

```rust
                // 滚轮上报是发给远端的字节 = 用户意图。本地回溯
                // （`WheelAction::LocalScroll`）不发字节，不算接管。
                let mut took_over = false;
```

然后把 `WheelAction::Report { .. }` 分支里的

```rust
                            let _ = pane.pty.write(bytes);
                        }
```

改成

```rust
                            let _ = pane.pty.write(bytes);
                            took_over = true;
                        }
```

把 `WheelAction::ArrowKeys { up, count }` 分支里的

```rust
                            let _ = pane.pty.write(bytes);
                        }
                        WheelAction::None => {}
```

改成

```rust
                            let _ = pane.pty.write(bytes);
                            took_over = true;
                        }
                        WheelAction::None => {}
```

最后，在该分支末尾把

```rust
                // 本地回溯不产生新的终端字节,不标脏这一帧会被 frame_is_dirty 判 Idle
                // 丢掉——滚了但画面不动。
                self.request_ui_redraw();
```

改成

```rust
                if took_over {
                    self.user_took_over();
                }
                // 本地回溯不产生新的终端字节,不标脏这一帧会被 frame_is_dirty 判 Idle
                // 丢掉——滚了但画面不动。
                self.request_ui_redraw();
```

- [ ] **Step 4: 接第四处 —— 键盘（`WindowEvent::KeyboardInput`）**

把

```rust
                        if let Some(pane) = self.ws.as_mut().and_then(Workspace::focused_mut) {
                            // F18:一按普通键就清选区。留着的话高亮会挂在屏幕上,
                            // 而底下的内容早被新输出冲掉了——高亮的是别的字。
                            pane.emulator.selection_clear();
                            // F17:一按普通键就贴回底部,否则「打字了但看不到自己输入」。
                            pane.emulator.scroll_to_bottom();
                            let _ = pane.pty.write(bytes);
                        }
```

改成

```rust
                        if let Some(pane) = self.ws.as_mut().and_then(Workspace::focused_mut) {
                            // F18:一按普通键就清选区。留着的话高亮会挂在屏幕上,
                            // 而底下的内容早被新输出冲掉了——高亮的是别的字。
                            pane.emulator.selection_clear();
                            // F17:一按普通键就贴回底部,否则「打字了但看不到自己输入」。
                            pane.emulator.scroll_to_bottom();
                            let _ = pane.pty.write(bytes);
                            // F40:用户接管,自动化让位(借用已释放)。
                            self.user_took_over();
                        }
```

> `pane` 的最后一次使用是 `pane.pty.write(bytes)`，NLL 下借用到此结束，紧跟着借 `&mut self` 可以编过。若编译器报借用冲突，改成在 `if let` 块外用 `took_over` 标志的写法（同滚轮那处）。

- [ ] **Step 5: 写 T1 守护测试**

在 `crates/mullion-app/src/app.rs` 的 `mod tests` 里追加：

```rust
    /// **T1 守护**:取消边绝不能挂在 `PtyWriter::write` 上。
    ///
    /// `Workspace::pump` 内部也调 `p.pty.write(out)` —— 那是 T1 的 PtyWrite
    /// 应答(DSR 光标位置查询、同步输出探测的回应),**不是用户输入**。若把
    /// 「有人往 PTY 写字节」当成用户接管的判据,远端一发同步输出探测,自动化
    /// 立刻自杀,而且现象是「有时候能跑有时候跑不了」,极难定位。
    ///
    /// **这条测试扎的是源码结构而不是运行时行为**,这是刻意的:取消边是
    /// `App` 的私有方法,`Workspace` 根本够不着它,运行时构造不出「pump 触发
    /// 取消」的场景;真正的风险是将来有人为了图省事把取消挪进
    /// `PtyWriter::write` 或 `Workspace::pump`。验证边界:它只挡得住「调用了
    /// `user_took_over`」这一种写法,挡不住有人另起一个同义的新函数。
    ///
    /// 自证会变红:在 `shell/workspace/mod.rs` 里随便加一行注释提到
    /// `user_took_over`。
    #[test]
    fn pty_write_echo_does_not_cancel_automation() {
        let src = include_str!("shell/workspace/mod.rs");
        assert!(
            !src.contains("user_took_over"),
            "workspace 里出现了 user_took_over —— `Workspace::pump` 的 pty.write \
             是 T1 的 PtyWrite 应答(DSR/同步输出探测的回应),不是用户输入。\
             把取消挂上去,远端一发探测自动化就自杀,现象是「有时能跑有时不能」。\
             取消边只能挂在 app.rs 里的用户意图写入点上。"
        );
    }
```

- [ ] **Step 6: 自证测试能变红**

```bash
printf '\n// user_took_over\n' >> crates/mullion-app/src/shell/workspace/mod.rs
cargo test -p mullion-app --lib pty_write_echo 2>&1 | grep -E "test result|FAILED"
```
Expected: `FAILED`（1 failed）。然后还原：
```bash
git checkout crates/mullion-app/src/shell/workspace/mod.rs
git status --short crates/mullion-app/src/shell/workspace/mod.rs
```
Expected: `git status` 对该文件无输出（已还原干净）。

> 注意：Step 3 那一步 `git checkout` 会把 Task 6 里对 workspace 的改动一并丢掉——所以**必须先确认 Task 6 已提交**（`git log --oneline -3` 里能看到「pane 的首字节标志」那条）。若尚未提交，改用手动删掉刚追加的两行。

- [ ] **Step 7: 跑全量测试 + clippy**

Run: `cargo test -p mullion-app --lib > /tmp/t10.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/t10.log`
Expected: `test result: ok.`

Run: `cargo clippy -p mullion-app --all-targets -- -D warnings 2>&1 | tail -3`
Expected: 无 warning。

- [ ] **Step 8: 提交**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): 用户接管时自动化让位 —— 四个用户意图写入点 (F40)

判据只能是 app.rs 里的用户意图写入点(粘贴/滚轮上报×2/键盘)。
Workspace::pump 里那处 pty.write 是 T1 的 PtyWrite 应答(DSR/同步输出探测
的回应),挂上去的话远端一发探测自动化就自杀,现象是「有时能跑有时不能」。
配了源码级守护测试 pty_write_echo_does_not_cancel_automation,已自证变红。"
```

---

## Task 11: 状态栏显示自动化状态

**Files:**
- Modify: `crates/mullion-app/src/ui/chrome.rs`（`status_bar` 多一个槽位）
- Modify: `crates/mullion-app/src/ui/mod.rs`（`UiFrame` 加字段 + `build_ui` 接线 + 测试）
- Modify: `crates/mullion-app/src/app.rs`（`render_frame` 填字段）

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-app/src/ui/mod.rs` 的 `mod tests` 里，紧跟
`build_ui_status_bar_pane_count_is_wired_not_hardcoded_f81` 之后追加：

```rust
    /// F40~F44:自动化状态必须真的流到状态栏,不能被 `build_ui` 吃掉。
    ///
    /// 破坏性验证:把 `build_ui` 里传给 `status_bar` 的 `frame.automation`
    /// 改成硬编码 `None`,第一条断言会红。
    #[test]
    fn build_ui_status_bar_shows_automation_status() {
        let (with_status, _) = rendered_text(UiFrame {
            automation: Some("自动化:进行中"),
            ..base_frame()
        });
        assert!(
            with_status.contains("自动化:进行中"),
            "自动化状态没画进状态栏,实际文本: {with_status:?}"
        );

        let (without, _) = rendered_text(UiFrame {
            automation: None,
            ..base_frame()
        });
        assert!(
            !without.contains("自动化"),
            "没有自动化状态时不该凭空出现文案,实际文本: {without:?}"
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib ui::tests 2>&1 | grep -E "no field .automation" | head -1`
Expected: 报 `UiFrame` 没有 `automation` 字段。

- [ ] **Step 3: `UiFrame` 加字段**

`crates/mullion-app/src/ui/mod.rs` 的 `pub struct UiFrame<'a> { ... }` 里，最后一个字段
`pub connected_session: Option<SessionId>,` **之后**插入：

```rust
    /// F40~F44:自动化状态一句话。`None` = 这条连接没跑过自动化。
    /// 生命周期由 `App` 管：一直显示到下一次 `spawn_connect`（那时清空）。
    pub automation: Option<&'a str>,
```

同文件 `mod tests` 里的 `fn base_frame()` 也要补这个字段（`automation: None,`）——`grep -n "fn base_frame" crates/mullion-app/src/ui/mod.rs` 找到它，在 `connected_session: None,` 后加一行。

- [ ] **Step 4: `build_ui` 接线**

把 `build_ui` 里的

```rust
    chrome::status_bar(
        ctx,
        t,
        frame.panes,
        frame.connected,
        ui_state.last_error.as_deref(),
    );
```

改成

```rust
    chrome::status_bar(
        ctx,
        t,
        frame.panes,
        frame.connected,
        ui_state.last_error.as_deref(),
        frame.automation,
    );
```

- [ ] **Step 5: `status_bar` 加槽位**

`crates/mullion-app/src/ui/chrome.rs`：把

```rust
pub fn status_bar(
    ctx: &egui::Context,
    t: &Theme,
    panes: usize,
    connected: bool,
    last_error: Option<&str>,
) {
```

改成

```rust
pub fn status_bar(
    ctx: &egui::Context,
    t: &Theme,
    panes: usize,
    connected: bool,
    last_error: Option<&str>,
    automation: Option<&str>,
) {
```

并把内部的右对齐区

```rust
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(err) = last_error {
                        ui.colored_label(theme::c32(t.danger), err);
                        ui.separator();
                    }
                    ui.colored_label(theme::c32(t.fg_faint), right);
                });
```

改成

```rust
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(err) = last_error {
                        ui.colored_label(theme::c32(t.danger), err);
                        ui.separator();
                    }
                    // F40~F44:自动化状态排在错误之后、常规右栏之前。
                    // 它是「这次连接发生了什么」的唯一可见证据——用户看不见,
                    // 就无法判断自动化是没跑还是跑了没效果。
                    if let Some(a) = automation {
                        ui.colored_label(theme::c32(t.fg_muted), a);
                        ui.separator();
                    }
                    ui.colored_label(theme::c32(t.fg_faint), right);
                });
```

- [ ] **Step 6: 填 `UiFrame` 的新字段**

`render_frame` 的签名**不用动**——`UiFrame` 是在调用点就地构造的（`RedrawRequested` 分支内，`grep -n "connected_session: self.connected_session" crates/mullion-app/src/app.rs` 找到它）。

把

```rust
                                connected_session: self.connected_session,
                            };
```

改成

```rust
                                connected_session: self.connected_session,
                                // 「跑着的时候盖住上一次的结论」这条规则放在
                                // `automation::status_line` 里，不在这儿手写
                                // if/else —— 写反的现象是新连接的状态栏挂着上
                                // 一条连接的结论，而它有单测钉着。
                                automation: crate::automation::status_line(
                                    self.automation.is_some(),
                                    self.automation_status.as_deref(),
                                ),
                            };
```

> 借用说明：这两处读的是 `self.automation` / `self.automation_status`，而紧随其后的 `self.active.as_mut()`（约 `:1369`）借的是另一个字段。字段级借用分辨在这里已被现有代码验证过——同一个 `UiFrame` 字面量里的 `groups: self.store.as_ref()...` 就留着对 `self.store` 的不可变借用，与那个 `as_mut()` 共存。

- [ ] **Step 7: 跑测试**

Run: `cargo test -p mullion-app --lib > /tmp/t11.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/t11.log`
Expected: `test result: ok.`

- [ ] **Step 8: 提交**

```bash
git add crates/mullion-app/src/ui/chrome.rs crates/mullion-app/src/ui/mod.rs crates/mullion-app/src/app.rs
git commit -m "feat(app): 状态栏显示自动化状态 (F40~F44)

文案一直显示到下一次 spawn_connect,不做定时淡出 —— 定时清除要再引一个
deadline 进帧循环,正是本切片要避免的东西(T7)。"
```

---

## Task 12: F44 右键「连接（跳过自动化）」

**Files:**
- Modify: `crates/mullion-app/src/ui/mod.rs`（`UiState` 加意图字段）
- Modify: `crates/mullion-app/src/ui/session_manager/list.rs`（右键菜单）
- Modify: `crates/mullion-app/src/app.rs`（意图施加点）

- [ ] **Step 1: `UiState` 加字段**

`crates/mullion-app/src/ui/mod.rs` 的 `pub struct UiState { ... }` 里，紧跟
`pub connect_request_last: Option<SessionId>,` 之后插入：

```rust
    /// F44:本次连接一次性跳过自动化（右键菜单）。app.rs 消费后立即清零 ——
    /// 右键跳过一次之后，普通双击连接若还静默跳过，用户会以为自动化坏了。
    pub connect_skip_automation: bool,
```

- [ ] **Step 2: 右键菜单加入口**

`crates/mullion-app/src/ui/session_manager/list.rs` 里，把

```rust
    resp.context_menu(|ui| {
        if ui.button("删除").clicked() {
            ui_state.pending_delete = Some(rec.id);
            ui.close_menu();
        }
    });
```

改成

```rust
    resp.context_menu(|ui| {
        // F44:一次性逃生门。远端 tmux 里正跑着 Claude Code 时,用户可能只想
        // 连上去看一眼,不想让自动化再发一遍 attach。
        if ui.button("连接(跳过自动化)").clicked() {
            ui_state.connect_request = Some(rec.id);
            ui_state.connect_skip_automation = true;
            ui.close_menu();
        }
        if ui.button("删除").clicked() {
            ui_state.pending_delete = Some(rec.id);
            ui.close_menu();
        }
    });
```

- [ ] **Step 3: 意图施加点消费标志**

`crates/mullion-app/src/app.rs` 里把

```rust
                if let Some(id) = self.ui.connect_request.take() {
                    self.ui.connect_request_last = Some(id);
```

改成

```rust
                // F44:**无条件**取走跳过标志 —— 哪怕这一帧没有连接意图(右键
                // 点了又关掉菜单),也不能让它漂到下一次连接上。
                let skip_automation = std::mem::take(&mut self.ui.connect_skip_automation);
                if let Some(id) = self.ui.connect_request.take() {
                    self.ui.connect_request_last = Some(id);
                    self.pending_skip_automation = skip_automation;
```

- [ ] **Step 4: 编译 + 全量测试**

Run: `cargo test -p mullion-app --lib > /tmp/t12.log 2>&1; grep -nE "test result|FAILED|error\[" /tmp/t12.log`
Expected: `test result: ok.`

> 「跳过恰好生效一次」的守护测试已经在 Task 3 的 `skip_flag_is_consumed_after_one_connect` 里，扎在真正的决策点 `take_pending` 上；这里的接线只是把标志递进去。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/ui/mod.rs crates/mullion-app/src/ui/session_manager/list.rs crates/mullion-app/src/app.rs
git commit -m "feat(ui): 会话右键「连接(跳过自动化)」 (F44)

跳过标志无条件取走:右键点开菜单又关掉也要清,否则会漂到下一次连接上。
「恰好生效一次」由 automation::take_pending 的守护测试钉死。"
```

---

## Task 13: 第四个 tab「登录后」的骨架

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/mod.rs`（Tab 常量）
- Modify: `crates/mullion-app/src/ui/session_manager/editor.rs`（`TABS` + 分派）
- Modify: `crates/mullion-app/src/ui/session_manager/fields.rs`（空的 `automation()`）

放第四个 tab 而不是塞进「高级」：「高级」现在是代理 + 跳板链，再加 tmux + 命令表 + env 表 + 三个延时会长到必须滚动才能找到东西。自动化也是唯一会**主动往远端发字节**的配置，值得独立一页。

- [ ] **Step 1: 加 Tab 常量**

`crates/mullion-app/src/ui/session_manager/mod.rs` 里把

```rust
pub(crate) const TAB_CONNECT: usize = 0;
pub(crate) const TAB_AUTH: usize = 1;
```

改成

```rust
pub(crate) const TAB_CONNECT: usize = 0;
pub(crate) const TAB_AUTH: usize = 1;
pub(crate) const TAB_ADVANCED: usize = 2;
pub(crate) const TAB_AUTOMATION: usize = 3;
```

> `validate::Missing::tab()` **不动**：「登录后」这一页没有任何必填项，缺项永远不会指向它。

- [ ] **Step 2: `TABS` 扩成四个 + 显式分派**

`crates/mullion-app/src/ui/session_manager/editor.rs`：

把

```rust
/// 三个 Tab 的标题。索引即 `UiState::editor_tab`。
const TABS: [&str; 3] = ["连接", "认证", "高级"];
```

改成

```rust
/// 四个 Tab 的标题。索引即 `UiState::editor_tab`,与 `super::TAB_*` 一一对应。
const TABS: [&str; 4] = ["连接", "认证", "高级", "登录后"];
```

把

```rust
        .show(ui, |ui| match ui_state.editor_tab {
            super::TAB_CONNECT => super::fields::basic(ui, t, buf, groups),
            super::TAB_AUTH => super::fields::auth(ui, t, buf, presence, &ui_state.key_candidates),
            _ => super::fields::network(ui, t, buf, presence),
        });
```

改成

```rust
        .show(ui, |ui| match ui_state.editor_tab {
            super::TAB_CONNECT => super::fields::basic(ui, t, buf, groups),
            super::TAB_AUTH => super::fields::auth(ui, t, buf, presence, &ui_state.key_candidates),
            super::TAB_AUTOMATION => super::fields::automation(ui, t, buf),
            // 「高级」是兜底:`editor_tab` 是既有的裸 usize 技术债,越界值
            // 落到这里比 panic 好。
            _ => super::fields::network(ui, t, buf, presence),
        });
```

- [ ] **Step 3: 加空的 `fields::automation`**

`crates/mullion-app/src/ui/session_manager/fields.rs` 末尾（`#[cfg(test)] mod tests` **之前**）插入：

```rust
/// F40~F44「登录后」页。字段全部落在 `buf.preserved_automation` 上。
///
/// 字段名沿用 `preserved_*` 前缀而**没有**改成 `automation`:与
/// `preserved_group_id`（自 P0-b 起可编辑，名字未改）同一个理由 —— 改名会波及
/// `buffer.rs` 的透传守护测试，收益为零。
pub(super) fn automation(ui: &mut Ui, t: &Theme, buf: &mut EditorBuffer) {
    section(ui, t, "总开关");
    let _ = (ui, t, buf);
}
```

> 这一步只求编过；真正的控件在 Task 14~16。

- [ ] **Step 4: 编译 + 全量测试**

Run: `cargo test -p mullion-app --lib > /tmp/t13.log 2>&1; grep -nE "test result|FAILED|error\[" /tmp/t13.log`
Expected: `test result: ok.`

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/ui/session_manager/
git commit -m "feat(ui): 会话编辑器加第四个 tab「登录后」骨架 (F40~F44)

不塞进「高级」:那页已经是代理+跳板链,再加 tmux/命令表/env 表/三个延时
会长到必须滚动才找得到东西;自动化也是唯一会主动往远端发字节的配置。"
```

---

## Task 14: 「登录后」页 —— 总开关 / tmux / 工作目录

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/fields.rs`

- [ ] **Step 1: 加三态下拉辅助函数**

在 `fields.rs` 的 `fn required(...)` 之后插入：

```rust
/// 三态下拉：继承（`None`）/ 开 / 关。
///
/// 「继承」与「显式关闭」必须可区分（同 `ProxyModeUi` 那个坑）：合并二者会让
/// 用户无法在分组开了自动化时单独关掉某一条会话。`Option<bool>` 类型自身就
/// 表达了三态，不需要再造一个 `*Ui` 枚举。
fn tri_state(ui: &mut Ui, id: &str, v: &mut Option<bool>, on: &str, off: &str) {
    let text = match *v {
        None => "继承",
        Some(true) => on,
        Some(false) => off,
    };
    egui::ComboBox::from_id_salt(id)
        .selected_text(text)
        .show_ui(ui, |ui| {
            ui.selectable_value(v, None, "继承");
            ui.selectable_value(v, Some(true), on);
            ui.selectable_value(v, Some(false), off);
        });
}

/// 可选毫秒数：勾选框 + `DragValue`。未勾选时显示内置默认值作提示 ——
/// 光给一个空框，用户不知道不填会发生什么。
fn opt_ms(ui: &mut Ui, t: &Theme, id: &str, v: &mut Option<u32>, default: u32, max: u32) {
    ui.horizontal(|ui| {
        let mut on = v.is_some();
        if ui.checkbox(&mut on, "").changed() {
            *v = if on { Some(default) } else { None };
        }
        match v {
            Some(ms) => {
                ui.add(egui::DragValue::new(ms).range(0..=max).suffix(" ms"));
            }
            None => {
                ui.colored_label(
                    crate::theme::c32(t.fg_dimmer),
                    format!("继承(内置默认 {default} ms)"),
                );
            }
        }
        let _ = id;
    });
}
```

- [ ] **Step 2: 写「登录后」页的前三节**

把 Task 13 里那个占位的 `automation()` 整个替换成：

```rust
pub(super) fn automation(ui: &mut Ui, t: &Theme, buf: &mut EditorBuffer) {
    // tmux 会话名留空时的推导结果，作 placeholder 实时显示。必须在借
    // `buf.preserved_automation` 之前算好。
    let derived = mullion_store::automation::sanitize_tmux_name(&buf.name);
    let a = &mut buf.preserved_automation;

    section(ui, t, "总开关");
    grid(ui, "sm_auto_enabled", |ui| {
        ui.label("登录后自动化");
        tri_state(ui, "sm_auto_enabled_combo", &mut a.enabled, "开", "关");
        ui.end_row();
    });

    section(ui, t, "tmux");
    grid(ui, "sm_auto_tmux", |ui| {
        ui.label("连上后");
        let text = match &a.tmux {
            None => "继承",
            Some(TmuxChoice::Off) => "不用 tmux",
            Some(TmuxChoice::Attach { .. }) => "自动 attach",
        };
        egui::ComboBox::from_id_salt("sm_auto_tmux_combo")
            .selected_text(text)
            .show_ui(ui, |ui| {
                if ui.selectable_label(a.tmux.is_none(), "继承").clicked() {
                    a.tmux = None;
                }
                if ui
                    .selectable_label(matches!(a.tmux, Some(TmuxChoice::Off)), "不用 tmux")
                    .clicked()
                {
                    a.tmux = Some(TmuxChoice::Off);
                }
                // 已经是 Attach 时不要重建 —— 会把用户填好的会话名清掉。
                if ui
                    .selectable_label(
                        matches!(a.tmux, Some(TmuxChoice::Attach { .. })),
                        "自动 attach",
                    )
                    .clicked()
                    && !matches!(a.tmux, Some(TmuxChoice::Attach { .. }))
                {
                    a.tmux = Some(TmuxChoice::Attach { session_name: None });
                }
            });
        ui.end_row();

        if let Some(TmuxChoice::Attach { session_name }) = &mut a.tmux {
            ui.label("会话名");
            // 不给 EditorBuffer 另开一个 String 字段:两个真源迟早漂移。
            // 每帧从 Option<String> 展开成临时 String,改动时写回 ——
            // 清空 = 回到「由会话名推导」,正是要的语义。
            let mut s = session_name.clone().unwrap_or_default();
            let resp = ui.add(
                egui::TextEdit::singleline(&mut s)
                    .hint_text(if derived.is_empty() {
                        "会话名为空,无法推导 —— 必须手填".to_string()
                    } else {
                        format!("留空则用「{derived}」")
                    })
                    .desired_width(f32::INFINITY),
            );
            if resp.changed() {
                *session_name = if s.trim().is_empty() { None } else { Some(s) };
            }
            ui.end_row();
        }
    });

    section(ui, t, "工作目录");
    grid(ui, "sm_auto_dir", |ui| {
        ui.label("初始目录");
        let mut s = a.work_dir.clone().unwrap_or_default();
        let resp = ui.add(
            egui::TextEdit::singleline(&mut s)
                .hint_text("留空 = 继承(远端默认)")
                .desired_width(f32::INFINITY),
        );
        if resp.changed() {
            a.work_dir = if s.trim().is_empty() { None } else { Some(s) };
        }
        ui.end_row();
    });
}
```

- [ ] **Step 3: 补 import**

`fields.rs` 顶部把

```rust
use mullion_store::{GroupRecord, Protocol};
```

改成

```rust
use mullion_store::{GroupRecord, Protocol, TmuxChoice};
```

- [ ] **Step 4: 编译 + 全量测试**

Run: `cargo test -p mullion-app --lib > /tmp/t14.log 2>&1; grep -nE "test result|FAILED|error\[" /tmp/t14.log`
Expected: `test result: ok.`

Run: `cargo clippy -p mullion-app --all-targets -- -D warnings 2>&1 | tail -3`
Expected: 无 warning。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/ui/session_manager/fields.rs
git commit -m "feat(ui): 登录后页的总开关/tmux/工作目录 (F40/F42/F44)

三态一律用 Option 自身表达,不另造 *Ui 枚举 —— AutomationPrefs 的字段本来
就全是 Option;ProxyModeUi 当年要造是因为 ProxyChoice 没有 Inherit 变体。
tmux 会话名直接编辑 Option<String>,不另开 String 缓冲(两个真源会漂移)。"
```

---

## Task 15: 「登录后」页 —— F41 命令列表与多行拆条

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/fields.rs`

- [ ] **Step 1: 加警告横幅辅助函数**

在 `fields.rs` 的 `fn opt_ms(...)` 之后插入：

```rust
/// 固定警告横幅。用 `warn` 描边而不是纯文字：这两条说的都是「会把明文/字符
/// 发到远端」，混在普通说明里读者会滑过去。
fn warn_banner(ui: &mut Ui, t: &Theme, text: &str) {
    egui::Frame::none()
        .fill(crate::theme::c32(t.sunken_bg))
        .stroke(egui::Stroke::new(1.0, crate::theme::c32(t.warn)))
        .rounding(6.0)
        .inner_margin(8.0)
        .show(ui, |ui| {
            ui.colored_label(crate::theme::c32(t.warn), text);
        });
}

/// F41：配了逐条延时时置顶的硬性警告。**不是可选润色**——这是设计 §2
/// 核心约束的用户可见面：一旦拆成多步，第二步起就无法保证屏幕还归我们。
const DELAY_WARNING: &str = "配了延时的命令会拆成多步发送。第二步起,字符会进入\
当时屏幕上的任何程序 —— 如果远端已经 attach 上 TUI,它们会被打进那个程序的输入框。";
```

- [ ] **Step 2: 在 `automation()` 里加命令列表**

在 `automation()` 函数体开头，`let a = &mut buf.preserved_automation;` **之后**、
`section(ui, t, "总开关");` **之前**插入：

```rust
    // 警告置顶:滚到页面底部才看到「会打进 TUI 输入框」就太晚了。
    if a.commands
        .iter()
        .flatten()
        .any(|c| c.delay_ms.is_some())
    {
        warn_banner(ui, t, DELAY_WARNING);
        ui.add_space(6.0);
    }
```

在 `automation()` 末尾（「工作目录」那一节之后）追加：

```rust
    section(ui, t, "登录后命令");
    // `None`(继承)与 `Some(vec![])`(显式空覆盖)必须可区分 —— 所以**绝不能**
    // 用 `get_or_insert_with(Vec::new)`:那样光是打开这一页就会把「继承」
    // 悄悄翻成「显式覆盖成空」,分组里配的命令全部失效。
    let mut reset_commands = false;
    match a.commands.as_mut() {
        None => {
            ui.horizontal(|ui| {
                ui.colored_label(crate::theme::c32(t.fg_dimmer), "继承上游的命令列表");
                if ui.button("改为自定义").clicked() {
                    reset_commands = true; // 见下方,这里借着 a.commands 不能直接赋值
                }
            });
        }
        Some(cmds) => {
            let len = cmds.len();
            let mut remove: Option<usize> = None;
            let mut swap: Option<(usize, usize)> = None;
            // 多行拆条:(在第几行之后, 插哪些)。
            let mut insert_after: Option<(usize, Vec<String>)> = None;

            for (i, c) in cmds.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    // 用 multiline 而不是 singleline:singleline 是否把粘贴进来
                    // 的换行原样交给我们,取决于 egui 内部实现;multiline 一定
                    // 会,拆条逻辑才有输入可拆。`desired_rows(1)` 让它看起来
                    // 仍是一行。
                    let resp = ui.add(
                        egui::TextEdit::multiline(&mut c.text)
                            .desired_rows(1)
                            .desired_width(240.0),
                    );
                    if resp.changed() && c.text.contains(['\n', '\r']) {
                        // 手敲回车 = 新增一条空行(spec §5)。
                        let trailing = c.text.ends_with('\n') || c.text.ends_with('\r');
                        let mut parts = crate::automation::split_pasted_commands(&c.text);
                        if trailing {
                            parts.push(String::new());
                        }
                        c.text = if parts.is_empty() {
                            String::new()
                        } else {
                            parts.remove(0)
                        };
                        if !parts.is_empty() {
                            insert_after = Some((i, parts));
                        }
                    }

                    let mut has_delay = c.delay_ms.is_some();
                    if ui.checkbox(&mut has_delay, "延时").changed() {
                        c.delay_ms = if has_delay {
                            Some(mullion_store::automation::DEFAULT_INTER_DELAY_MS)
                        } else {
                            None
                        };
                    }
                    if let Some(ms) = c.delay_ms.as_mut() {
                        ui.add(egui::DragValue::new(ms).range(0..=60_000u32).suffix(" ms"));
                    }

                    if ui.add_enabled(i > 0, egui::Button::new("↑")).clicked() {
                        swap = Some((i, i - 1));
                    }
                    if ui.add_enabled(i + 1 < len, egui::Button::new("↓")).clicked() {
                        swap = Some((i, i + 1));
                    }
                    if ui.button("✕").clicked() {
                        remove = Some(i);
                    }
                });
            }

            // 变更统一在遍历结束后施加 —— 边遍历边改会让索引失效。
            if let Some((i, parts)) = insert_after {
                for (k, text) in parts.into_iter().enumerate() {
                    cmds.insert(
                        i + 1 + k,
                        mullion_store::AutomationCommand {
                            text,
                            delay_ms: None,
                        },
                    );
                }
            }
            if let Some((x, y)) = swap {
                cmds.swap(x, y);
            }
            if let Some(i) = remove {
                cmds.remove(i);
            }

            ui.horizontal(|ui| {
                if ui.button("+ 添加命令").clicked() {
                    cmds.push(mullion_store::AutomationCommand {
                        text: String::new(),
                        delay_ms: None,
                    });
                }
                if ui.button("恢复继承").clicked() {
                    reset_commands = true;
                }
            });
        }
    }
    // `a.commands` 的可变借用到这里才结束,现在才能整体换掉它。
    if reset_commands {
        a.commands = match a.commands {
            None => Some(Vec::new()),
            Some(_) => None,
        };
    }
```

> `reset_commands` 在两个分支里含义相反（`None` 分支是「改为自定义」、`Some` 分支是「恢复继承」），末尾那个 `match` 把它翻译成实际动作。这是为了绕开「在 `match a.commands.as_mut()` 内部改 `a.commands` 本身」的借用冲突。

- [ ] **Step 3: 编译 + 全量测试 + clippy**

Run: `cargo test -p mullion-app --lib > /tmp/t15.log 2>&1; grep -nE "test result|FAILED|error\[" /tmp/t15.log`
Expected: `test result: ok.`

Run: `cargo clippy -p mullion-app --all-targets -- -D warnings 2>&1 | tail -3`
Expected: 无 warning。

- [ ] **Step 4: 记下无法自动验证的部分**

在本 Task 的提交信息里明确写出：**「粘贴多行文本是否真的拆成多条」需要人工在 Windows 实机验证**。拆条的纯函数（`split_pasted_commands`）已单测覆盖，但「egui 的 `TextEdit::multiline` 在真实粘贴事件下把换行原样交给我们」这一环无头环境验不了。这一条会进 Task 17 的 Release notes 验收清单。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/ui/session_manager/fields.rs
git commit -m "feat(ui): 登录后命令列表 + 多行粘贴拆条 (F41)

绝不用 get_or_insert_with:那样光打开这一页就会把「继承」翻成「显式覆盖成
空」,分组里配的命令全部失效。用 multiline 而不是 singleline —— singleline
是否把粘贴的换行原样交出来取决于 egui 内部实现,拆条逻辑就没有输入可拆。

未验证:粘贴多行是否真的拆成多条,需人工在实机确认(拆条纯函数已单测)。"
```

---

## Task 16: 「登录后」页 —— F43 环境变量与三个延时

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/fields.rs`

- [ ] **Step 1: 加 env 警告常量**

在 `DELAY_WARNING` 之后插入：

```rust
/// F43：env 区的固定警告。**不是可选润色**——用户会拿它存密码，而这里
/// 存不住密码：值以明文进 `sessions.toml`（不进 `secrets.enc`），且终归要以
/// `export` 行发到远端。
const ENV_WARNING: &str = "环境变量不是存密码的地方 —— 值以明文存进 sessions.toml,\
并会以 export 行发到远端,落进 shell 历史与 /proc/<pid>/environ。要存密码请用凭据。";
```

- [ ] **Step 2: 在 `automation()` 末尾追加两节**

```rust
    section(ui, t, "环境变量");
    warn_banner(ui, t, ENV_WARNING);
    ui.add_space(6.0);
    // 同 commands:`None`(继承)与 `Some(vec![])`(显式空覆盖)必须可区分。
    let mut reset_env = false;
    match a.env.as_mut() {
        None => {
            ui.horizontal(|ui| {
                ui.colored_label(crate::theme::c32(t.fg_dimmer), "继承上游的环境变量");
                if ui.button("改为自定义").clicked() {
                    reset_env = true;
                }
            });
        }
        Some(vars) => {
            let mut remove: Option<usize> = None;
            for (i, v) in vars.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut v.key)
                            .hint_text("KEY")
                            .desired_width(140.0),
                    );
                    ui.label("=");
                    ui.add(
                        egui::TextEdit::singleline(&mut v.value)
                            .hint_text("值(明文)")
                            .desired_width(220.0),
                    );
                    if ui.button("✕").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                vars.remove(i);
            }
            ui.horizontal(|ui| {
                if ui.button("+ 添加变量").clicked() {
                    vars.push(mullion_store::EnvVar {
                        key: String::new(),
                        value: String::new(),
                    });
                }
                if ui.button("恢复继承").clicked() {
                    reset_env = true;
                }
            });
        }
    }
    if reset_env {
        a.env = match a.env {
            None => Some(Vec::new()),
            Some(_) => None,
        };
    }

    section(ui, t, "时序");
    grid(ui, "sm_auto_timing", |ui| {
        ui.label("首字节后再等");
        opt_ms(
            ui,
            t,
            "sm_auto_initial",
            &mut a.initial_delay_ms,
            mullion_store::automation::DEFAULT_INITIAL_DELAY_MS,
            10_000,
        );
        ui.end_row();

        ui.label("行间延时");
        opt_ms(
            ui,
            t,
            "sm_auto_inter",
            &mut a.inter_delay_ms,
            mullion_store::automation::DEFAULT_INTER_DELAY_MS,
            10_000,
        );
        ui.end_row();

        ui.label("就绪超时");
        opt_ms(
            ui,
            t,
            "sm_auto_ready",
            &mut a.ready_timeout_ms,
            mullion_store::automation::DEFAULT_READY_TIMEOUT_MS,
            120_000,
        );
        ui.end_row();
    });
```

- [ ] **Step 3: 编译 + 全量测试 + clippy + fmt**

Run: `cargo test --workspace > /tmp/t16.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/t16.log`
Expected: 每个 crate 都 `test result: ok.`，无 `FAILED`。

Run: `cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3`
Expected: 无 warning。

Run: `cargo fmt --check`
Expected: 无输出。

- [ ] **Step 4: 提交**

```bash
git add crates/mullion-app/src/ui/session_manager/fields.rs
git commit -m "feat(ui): 登录后页的环境变量与三个延时 (F43)

env 区固定警告是硬性要求不是润色:值明文进 sessions.toml、以 export 行发到
远端、落进 shell 历史与 /proc/<pid>/environ —— 用户会拿它存密码。"
```

---

## Task 17: 交付（CLAUDE.md 一条龙）

**Files:**
- Modify: `Cargo.toml`（`workspace.package.version`）
- Create: `notes.md`（Release notes 正文，不入库）

本切片有用户可见行为，触发 CLAUDE.md 的一条龙交付约定。**不要停下来问「要不要 bump / 要不要发版」。**

- [ ] **Step 1: 升 patch 版本号**

读 `Cargo.toml` 的 `workspace.package.version`，第三位 +1（当前是 `0.1.N` → `0.1.N+1`）。单独一个提交：

```bash
git add Cargo.toml
git commit -m "chore: 版本 0.1.N(登录后自动化:连上即落进远端 tmux 里的 Claude Code)"
```

- [ ] **Step 2: 跑绿**

```bash
cargo test --workspace > /tmp/final-test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/final-test.log
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```
Expected: 三条全干净。**不绿不发。**

- [ ] **Step 3: 交叉编译 + objdump 依赖验收**

```bash
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app
```
按 `docs/cross-compile-windows.md` 做 objdump 依赖验收。**出现 `libgcc_s_seh-1.dll` / `libwinpthread-1.dll` 即为不合格，必须修。**

- [ ] **Step 4: 写 notes.md**

内容包含：改了什么 + 下面这份**人工验收清单** + sha256 + 首次运行提示（未签名 exe 每版都会被 SmartScreen 拦，`Unblock-File .\mullion.exe`）。

人工验收清单（沿 spec §7，本切片是它们第一次真正可验）：

1. Windows 实机连上即落在远端 tmux 里的 Claude Code
2. **断线重连回一个正在跑的 Claude Code**：确认没有任何字符被打进输入框（这是整个设计的核心约束）
3. 关掉自动化（F44）后行为与上一版逐字一致
4. 自动化跑到一半时敲键盘：剩余命令不再发出，状态栏给出「已中止：检测到你的输入」
5. 分屏新开的 pane 是干净 shell，不重复跑自动化
6. **等待期按一下回车**（「催一下」是很常见的动作）：确认状态栏说清了自动化被取消，用户不会以为「连上了但什么都没发生」
7. 远端 `.bashrc` 里配了自动 attach tmux 时，关掉 F40 只用命令列表：确认命令没有打进 TUI
8. 中文会话名 sanitize 后 tmux 能正常 attach
9. 高延迟代理链路下 15s 默认超时是否够用
10. **命令框里粘贴多行文本，确认拆成多条**（拆条纯函数已单测，但「egui 把粘贴的换行原样交给我们」这一环无头验不了）
11. 会话右键菜单里「连接（跳过自动化）」跳过一次后，再普通双击连接时自动化**恢复**执行

- [ ] **Step 5: 发 GitHub Release**

```bash
sha256sum mullion.exe > mullion.exe.sha256
HTTPS_PROXY=http://127.0.0.1:7890 gh release create v0.1.N \
  mullion.exe mullion.exe.sha256 -t "v0.1.N" -F notes.md --repo kilobitcy/Mullion
```
**标题只能是纯版本号 `v0.1.N`**，不带破折号、不带一句话摘要、不带 emoji。想说的话全部写进 notes 正文。

- [ ] **Step 6: 报给用户**

Release 链接 + sha256 + 上面那份验收清单。

---

## 附：本切片明确不做的事

- 分组编辑器的「登录后」分节。数据层（`GroupRecord.automation`）与继承 P1-a 已就绪，但分组编辑器是另一套 UI 代码；会话级先跑通、实机验过再说。
- 自动化的「立即重跑」按钮。总设计 §2 的核心约束是「一旦屏幕归属可能变了就不再发」，重跑按钮与它正面冲突，要做得先定清楚语义。
- 任何为自动化改 `ControlFlow` 的东西。`frame::tests` 在本切片里是**回归**而非新增：修订一的直接后果就是那批测试不该有任何改动。**若实现中发现需要动它们，说明又把 deadline 塞进帧循环了，停下来。**
