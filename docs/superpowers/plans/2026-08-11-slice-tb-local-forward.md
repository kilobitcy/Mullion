# 切片 T-b：`-L` 本地转发 + 重连 + 状态可见性 实施计划（F111/F114/F115）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** T-a 配好的隧道**真的能转发**。`-L` 起本机 `TcpListener`，每个入站连接经 `direct-tcpip` 打到远端目标；连接断了自行有界退避重连；状态在隧道列表与状态栏常驻可见，掉进失败态弹一次 toast。**发版**，实机验「DBeaver 连本地 3306」。

**Architecture:** 转发实现全部落 `mullion-ssh/src/tunnel.rs`（设计 D10：`SshConnection::handle()` 是 `pub(crate)`，只有同 crate 够得着；且这块逻辑能脱离 GUI 用真 `TcpListener` 纯测）。`mullion-app` 只做三件事：把 `TunnelRecord` 翻成 `SshConfig`、持有运行时状态表、把状态画出来。**依赖方向不变**。

**Spec:** `docs/superpowers/specs/2026-08-11-tunnels-design.md`（D1/D5/D7/D8/D10/D13）、`docs/adr-010-tunnels-as-first-class-objects.md`

**本切片不做**：`-R`（F112）、`-D`（F113）—— 归 T-c，启动按钮对这两类保持真禁用，hover 文案改成指向 T-c。`autostart` 仍无 UI 无行为。

---

## 本切片新定死的六条设计决策

写在最前面，因为它们决定了下面每个 Task 的形状。

**S1 —— 先占端口，再建 SSH；listener 跨重连不释放。**
`bind` 在 `establish` **之前**。理由有两条：端口被占用是配置错误，`bind` 0ms 就能发现，不该先烧一次完整建链（高延迟代理链路上要几秒）才告诉用户「端口 3306 已被占用」；重连期间若把 listener 放掉，端口会被别的进程抢走，隧道恢复时再也绑不回来。重连期间进来的 TCP 连接**立即关闭**，不静默挂起 —— 挂起的连接在 DBeaver 那边表现为「转圈直到超时」，比干脆的 connection reset 难排查得多。

**S2 —— `bind` 失败是致命错误，不进退避。**
端口占用重试 8 次不会变成不占用。直接落 `Failed`，文案点名端口号。

**S3 —— 重连用「只信已知主机」策略，首连才允许弹窗。**
首次启动用现成的 `PromptingPolicy`（用户刚点完「启动」，人在场）；**重连一律换成新的 `TrustedOnlyPolicy`**：`known_hosts` 里有记录且指纹一致才放行，未知/变更一律拒绝。
这一手把设计 D7 的两条硬约束变成**结构性保证**而不是 if 判断：后台重连不可能弹出模态框（策略压根不发弹窗事件），指纹变了也不可能被自动接受（策略只认已记录的那一份）。

**S4 —— 致命错误分类是纯函数。**
`is_fatal(&ConnectError)`：`HostKeyChanged`（疑似中间人）/ `HostKeyUnknown`（需要交互）/ `AuthFailed`（凭据不对，重试 8 次只会把账号锁掉）→ 致命，立刻停、不重试；其余（网络、代理、跳板）→ 可退避。

**S5 —— 状态回传走注入回调，`mullion-ssh` 不认识 `TunnelId`。**
`Arc<dyn Fn(TunnelState) + Send + Sync>`，与 `session.rs` 里 `wake` 的既有做法同构。谁是第几条隧道由 app 在闭包里捕获。

**S6 —— 连接死亡靠 2s 轮询 `Handle::is_closed()` 发现。**
一条空闲隧道（没人连 3306）的 SSH 断掉时，accept 循环不会有任何动静。不主动探，用户要等到下次用 DBeaver 才发现「已经断了半小时」。`russh 0.54.5` 的 `Handle::is_closed()`（`client/mod.rs:269`）= 内部 mpsc sender 是否已关，传输任务一死即为真。

---

## 文件结构

| 文件 | 本次的职责 | 改动性质 |
|---|---|---|
| `crates/mullion-ssh/src/tunnel.rs` | 退避/致命分类/绑定地址纯函数 + `-L` 转发 + 监管循环 | **新建** |
| `crates/mullion-ssh/src/session.rs` | `SshConnection` 暴露 `is_closed()` 与开 direct-tcpip 的入口 | 加两个方法 |
| `crates/mullion-ssh/src/lib.rs` | `pub mod tunnel` | 一行 |
| `crates/mullion-app/src/host_key.rs` | `TrustedOnlyPolicy`（S3） | 加类型 |
| `crates/mullion-app/src/tunnels.rs` | 运行时状态表 + 指示器纯函数 | **新建** |
| `crates/mullion-app/src/app.rs` | `UserEvent::TunnelState`、启停施加、删会话联动 | 加分支 |
| `crates/mullion-app/src/ui/mod.rs` | `UiState` 加启停意图；`UiFrame` 加运行态视图 | 加字段 |
| `crates/mullion-app/src/ui/session_manager/tunnel_list.rs` | 启动/停止按钮 + 行状态 | 主战场 |
| `crates/mullion-app/src/ui/chrome.rs` | F115 状态栏指示器 | 加参数 |
| `crates/mullion-app/src/ui/session_manager/list.rs` | 删会话确认框补「其中 N 条正在运行」 | 局部 |

任务顺序由编译约束决定：Task 1 的类型是 2/3 的前提；Task 4 的状态表是 5/6/7 的前提。

---

### Task 1: `mullion-ssh` 的纯函数地基

**Files:** Create `crates/mullion-ssh/src/tunnel.rs`；Modify `crates/mullion-ssh/src/lib.rs`

- [ ] **Step 1: 先写失败测试 —— 退避序列**

`backoff_is_bounded_exponential_and_gives_up`：
`backoff_delay(1..=8)` 依次为 1/2/4/8/16/30/30/30 秒（第 6 次起被 30s 封顶），`backoff_delay(9)` 为 `None`。

三条断言各自有用：**指数**保证掉线瞬间恢复快，**封顶**保证长时间断网时不会退化成一小时试一次，**放弃**保证不会永远在后台重拨一台已经报废的机器。缺任何一条都是真实故障模式。

- [ ] **Step 2: 先写失败测试 —— 致命分类（S4）**

`fingerprint_change_is_fatal_and_never_retried`：`is_fatal(&HostKeyChanged{..})` 为真。
`auth_failure_is_fatal_so_we_do_not_lock_the_account`：`is_fatal(&AuthFailed)` 为真。
`transient_network_errors_are_retryable`：`ConnectionRefused` / `Io` / `JumpFailed` / `ProxyUnreachable` 全为假。

第一条是安全断言，注释要写明：指纹变了可能是中间人，自动重连 = 自动把凭据往可疑主机上送。

- [ ] **Step 3: 先写失败测试 —— 绑定地址（D5/F117）**

`expose_false_binds_loopback_only`：`bind_addr(false, 3306)` 的 IP 是 `127.0.0.1`。
`expose_true_binds_all_interfaces`：`bind_addr(true, 3306)` 的 IP 是 `0.0.0.0`。
两条必须成对 —— 只测 `false` 那条，把函数写成恒返回 loopback 也是绿的。

- [ ] **Step 4: 实现，跑绿**

```rust
pub const MAX_ATTEMPTS: u32 = 8;
pub const BACKOFF_CAP: Duration = Duration::from_secs(30);
pub fn backoff_delay(attempt: u32) -> Option<Duration>;
pub fn is_fatal(e: &ConnectError) -> bool;
pub fn bind_addr(expose: bool, port: u16) -> SocketAddr;
```

`cargo test -p mullion-ssh`。

---

### Task 2: `-L` 转发本体（真 `TcpListener`，无需远端）

**Files:** Modify `crates/mullion-ssh/src/tunnel.rs`、`crates/mullion-ssh/src/session.rs`

- [ ] **Step 1: `SshConnection` 开两个口子**

```rust
pub fn is_closed(&self) -> bool                     // S6 用
pub(crate) async fn open_direct_tcpip(&self, host: &str, port: u16) -> Result<Channel<Msg>, ConnectError>
```

`open_direct_tcpip` 保持 `pub(crate)`：外部拿不到 channel 就不会误 Drop，与 `handle()` 同一条理由。`originator` 传 `"127.0.0.1", 0`（本机发起，端口无意义）。

- [ ] **Step 2: 先写失败测试 —— 双向字节搬运**

用 `tokio::io::duplex` 造一对假「channel 流」，起真的 `TcpListener` 绑 `127.0.0.1:0`（端口交给内核，测试不能跟别的进程抢固定端口）：

1. `bytes_flow_from_local_client_to_remote_end`
2. `bytes_flow_back_from_remote_end_to_local_client`
3. `closing_the_remote_end_closes_the_local_client`：远端 EOF 必须传播回本地，否则 DBeaver 会一直挂着一个死连接。
4. `closing_the_local_client_closes_the_remote_end`：反向同理，否则远端 sshd 那侧堆半开 channel。

搬运函数因此不能直接吃 `Channel<Msg>`，签名要泛型到 `AsyncRead + AsyncWrite`：

```rust
pub(crate) async fn pump_bidirectional<A, B>(local: A, remote: B) -> std::io::Result<(u64, u64)>
where A: AsyncRead + AsyncWrite + Unpin, B: AsyncRead + AsyncWrite + Unpin;
```

这正是「能脱离 GUI 纯测」的兑现点（设计 D10 理由 3）。实现用 `tokio::io::copy_bidirectional`。

- [ ] **Step 3: 先写失败测试 —— 端口占用要说人话（S2）**

`binding_an_occupied_port_names_the_port_instead_of_a_generic_io_error`：先自己占住一个端口，再 `bind_listener` 同一个端口，断言错误消息里**含该端口号**且含「占用」。
理由：`AddrInUse` 原文是英文 os error，用户看不出该去关哪个程序。

- [ ] **Step 4: 实现 accept 循环**

```rust
async fn serve_local(listener: &TcpListener, conn: &Arc<SshConnection>, host: &str, port: u16) -> ConnectError
```

每个入站连接 `tokio::spawn` 一份：开 channel → `into_stream()` → `pump_bidirectional`。
`into_stream()` 内部包的是 `ChannelCloseOnDrop`（`russh` `channels/mod.rs:589`），流一 Drop 就发 CHANNEL_CLOSE —— 这是本项目 ADR-009 不变量 3「channel 泄漏」在这里的解法，**不要**改成手工 `channel.data()` 搬运，那条路要自己管 close。
开 channel 失败时：若 `conn.is_closed()` 则返回，交给上层重连；否则只关掉这一条本地连接、继续 accept（目标端口没开着不该拖垮整条隧道）。

---

### Task 3: 监管循环（生命周期 + 重连，F114）

**Files:** Modify `crates/mullion-ssh/src/tunnel.rs`

- [ ] **Step 1: 定状态与句柄**

```rust
pub enum TunnelState {
    Connecting,
    Running,
    Reconnecting { attempt: u32, retry_in: Duration },
    Failed(String),
    Stopped,
}
pub struct TunnelHandle { /* stop 通道 + JoinHandle */ }
impl TunnelHandle { pub fn stop(&self); }
```

`Failed` 带**已格式化的原因**：app 侧要把它塞进 toast 和行状态，不该在 UI 层再做一次错误分类。

- [ ] **Step 2: 先写失败测试 —— 状态序列**

用一个「永远失败」的假拨号器（把 `establish` 抽成注入的 `dialer: Arc<dyn Fn() -> BoxFuture<Result<SshConnection, ConnectError>>>`？—— **不要**，`SshConnection` 造不出假的）。改用可测的切法：把**决策**从**执行**里剥出来，写成纯函数：

```rust
pub fn next_step(attempt: u32, err: &ConnectError) -> Step   // Step::Retry(Duration) | Step::GiveUp(String)
```

测三条：
1. `fatal_error_gives_up_on_the_first_attempt`：第 1 次就遇到 `HostKeyChanged` → `GiveUp`，且理由里含「指纹」。**自证变红**：把 `is_fatal` 那个分支删掉，这条会变成 `Retry`。
2. `transient_error_retries_with_growing_delay`：`Io` 错误第 1/2/3 次分别 `Retry(1s/2s/4s)`。
3. `giving_up_after_the_last_attempt_says_how_many_times_it_tried`：第 9 次 → `GiveUp`，理由里含「8 次」。

监管循环本身（真 `establish`、真网络）无头验不了，但它现在只剩「照 `next_step` 的判决执行」这一层壳。

- [ ] **Step 3: 实现 `spawn_tunnel`**

```rust
pub fn spawn_tunnel(
    listener: TcpListener,                 // S1：调用方先 bind 好再交进来
    target: (String, u16),
    dial: TunnelDial,                      // cfg + 首连策略 + 重连策略（S3）
    on_state: Arc<dyn Fn(TunnelState) + Send + Sync>,
) -> TunnelHandle;
```

循环骨架：`Connecting` → `establish`（首次用 `dial.first_policy`，其后 `dial.retry_policy`）→ `Running` → `select!{ serve_local(..), tick(2s) 查 is_closed(), stop 信号 }` → 断了就按 `next_step` 判决 → `Reconnecting{..}` 或 `Failed`。
`stop` 到达时先 `conn.disconnect().await` 再退出 —— `russh` 的 `Drop` 不发 disconnect（`session.rs:118` 已记），漏了就在对端堆半开连接。

---

### Task 4: app 侧运行时状态表

**Files:** Create `crates/mullion-app/src/tunnels.rs`；Modify `host_key.rs`、`app.rs`、`ui/mod.rs`

- [ ] **Step 1: `TrustedOnlyPolicy`（S3）**

放 `host_key.rs`，复用现成的纯函数 `check()`：`HostKeyCheck::Trusted` → `Accept`，其余一律 `Reject`。
测试 `trusted_only_policy_rejects_unknown_and_changed_hosts_without_prompting`：两种情况都 `Reject`，且**没有**任何 `HostKeyPrompt` 事件被发出（策略里根本没有 proxy 字段，这一点由类型保证 —— 测试注释里点明）。

- [ ] **Step 2: 状态表与指示器纯函数**

```rust
pub struct TunnelRuntime { live: BTreeMap<TunnelId, Live> }   // Live { handle, state }
pub fn indicator(states: &[TunnelState], total: usize) -> Option<Indicator>
```

先写失败测试：
1. `indicator_takes_the_worst_state_not_the_first`：`[Running, Failed]` → 危险态；把顺序反过来结果**相同**。这条守 D13「按最坏状态上色」——写成取第一条时，两个顺序会给出不同答案。
2. `indicator_counts_running_over_total`：2 条配置、1 条运行 → 文案含 `1/2`。
3. `indicator_is_none_when_nothing_is_configured`：一条隧道都没有时状态栏不占位置。

- [ ] **Step 3: `UserEvent::TunnelState { id, state }` 接线**

app.rs 收到后写进状态表并 `request_redraw`。注意 `UserEvent` 已有的世代号约定不适用（隧道不属于 `Workspace`，其生命周期独立于连接/分屏），**改用 id 存在性判断**：状态到达时若该 `TunnelId` 已不在表里（用户已停止/已删除），直接丢弃。

---

### Task 5: 启动/停止按钮与行状态（F111 UI 侧）

**Files:** Modify `tunnel_list.rs`、`ui/mod.rs`、`app.rs`

- [ ] **Step 1: 先写失败测试**

1. `local_tunnel_start_button_is_enabled_now_that_forwarding_exists`：`Local` 行的启动按钮 `enabled == true`。
   **这条是 T-a 那条 `start_button_is_really_disabled_not_just_greyed` 的正式接班**——那条要在本 Task 删掉（它守的前提已经不成立），删的同时必须由这条顶上，否则「按钮该不该能点」这件事一个测试都没有了。
   **必须在 `ctx.run` 闭包内部读 `read_response` 并跑 ≥3 帧** —— 见 T-a 记的坑：`run()` 外面读会落到首帧，那帧滚动区不可见、`Ui` 记成 `enabled=false`，断言恒绿。
2. `remote_and_dynamic_start_buttons_stay_disabled_until_t_c`：`Remote` / `Dynamic` 行仍是禁用态。防止有人顺手把三种类型一起放开，结果 `-R`/`-D` 点了没反应。
3. `a_running_tunnel_shows_stop_instead_of_start`：状态为 `Running` 时按钮文字是「停止」。
4. `a_dangling_tunnel_cannot_be_started`：引用已删除会话的隧道，启动按钮禁用，hover 说明是引用坏了。**这是 D3「删除后拒绝启动」的 UI 闸门。**

- [ ] **Step 2: 实现**

行右侧按状态出按钮；行副标题右侧加状态文字（`重连中 (3/8，4 秒后重试)` / `已停止：端口 3306 已被占用`）。
点击写 `ui_state.tunnel_start_request` / `tunnel_stop_request`（UI 只写意图），app.rs 施加：`store.ssh_config_for(session_id)` → `bind_addr` → `bind` → `spawn_tunnel`。

`ssh_config_for` 失败（悬垂跳板、缺凭据）直接落 `Failed` 并显示原文 —— 它已经是可操作错误，不要再包一层。

---

### Task 6: 状态栏指示器与失败 toast（F115）

**Files:** Modify `ui/chrome.rs`、`ui/mod.rs`、`app.rs`

- [ ] **Step 1: 先写失败测试**

1. `status_bar_shows_the_tunnel_indicator_between_automation_and_encoding`：三者共存时顺序正确（`chrome.rs:125-165` 的既有约定：错误 → 自动化 → 隧道 → `UTF-8`）。
2. `status_bar_has_no_tunnel_indicator_when_none_configured`：不平白占一格。
3. `entering_failed_state_raises_a_toast_once_not_every_frame`：跌进失败态弹一次 toast，之后连跑 3 帧不再弹。
   **这条是必需的**：状态事件每次到达都会重绘，写成「状态是 Failed 就弹」会变成每帧一条 toast，界面直接不能用。

- [ ] **Step 2: 实现**

`chrome::status_bar` 加一个 `tunnel: Option<Indicator>` 参数（文本 + 色）。toast 由 app.rs 在**状态跃迁**处发（比较 old/new），不在渲染层判断。
指示器加 `annotate::mark(ctx, "状态栏/隧道指示器", rect)` —— F100 新元素必须登记。

---

### Task 7: 删会话与运行中隧道的联动（D3）

**Files:** Modify `list.rs`、`app.rs`

- [ ] **Step 1: 先写失败测试**

1. `delete_confirmation_says_how_many_affected_tunnels_are_running`：3 条受影响、1 条运行 → 确认框里出现「其中 1 条正在运行」。这是 T-a 欠账里明确移交的那半句。
2. `no_running_clause_when_none_are_running`：一条都没跑时不出现那半句（不制造「有东西在跑」的错觉）。

- [ ] **Step 2: 实现 —— 删会话立刻停掉引用它的隧道**

app.rs 施加删除会话时，先对 `tunnels_referencing(id)` 里所有**运行中**的调 `stop()`，再删会话。
注释要写明这是**安全属性不是体验属性**（设计 D3）：一条指向已删除会话的本机端口继续 listen，意味着用户以为已经关掉的通路还开着。

- [ ] **Step 3: 加一条守护测试**

`deleting_a_session_stops_its_running_tunnels`：断言停止动作真的发生了。若这条只能靠真网络验，就退化成对「决策函数」的断言（`tunnels_to_stop_on_session_delete(id, &runtime) -> Vec<TunnelId>`），并在注释里说明为什么是这个形状。

---

## 验收

- [ ] `cargo test --workspace` 全绿
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` 无输出
- [ ] `cargo fmt --check` 通过
- [ ] 三条守护测试做**变异验证**（改实现能让它们真的变红）：`local_tunnel_start_button_is_enabled_now_that_forwarding_exists`、`indicator_takes_the_worst_state_not_the_first`、`entering_failed_state_raises_a_toast_once_not_every_frame`
- [ ] 按交付约定升 patch 版本、交叉编译 + objdump 验收、发 GitHub Release

## 人工验收清单（无头环境验不了，写进 Release notes）

- [ ] `-L` 转发 3306，本地 DBeaver 能连上远端内网库
- [ ] 拔网线 / 切代理，隧道进「重连中」并自行恢复，状态栏变黄
- [ ] 目标机关机，8 次退避后停下标红，弹出 toast
- [ ] 端口被占用时报「端口 3306 已被占用」，不自动改端口
- [ ] 删除被引用的会话，确认框正确列出隧道且运行中的当场停止
- [ ] 会话管理器关闭后，状态栏隧道指示器仍然可见且计数正确
- [ ] 经跳板/代理的会话，隧道也能起来（隧道复用 `establish()` 的全链路，这条是它的实证）

## 移交给 T-c 的欠账

1. `-R`（F112）与 `-D`（F113）—— 启动按钮仍真禁用
2. `autostart` 字段已落盘，仍无 UI、无行为
3. 隧道行之间切换仍**没有脏检查确认框**（会话侧有 `pending_switch` 三件套，隧道侧没有）
