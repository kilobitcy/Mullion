# 切片 P1-b 设计：登录后自动化的运行时接线与配置 UI（F40~F44）

> 前置：`2026-08-05-slice-p1-login-automation-design.md`（P1 总设计，下称「总设计」）
> 与 `2026-08-06-slice-p1a-login-automation-data-and-schedule.md`（P1-a 实现计划，已完工，
> squash 成 `e808597` 入 main）。
>
> 本文只写 **app 层**：把 P1-a 已就绪的 `build_plan`（store，纯函数）与
> `write_scheduled`（ssh，async 调度）接起来，并给用户可编辑的入口。
> 数据模型、字节生成规则、引号层数、继承语义全部沿用总设计 §3，本文不重复。

---

## 1. 对总设计的两处修订

写实现计划前必须先解决的冲突——留着两份说法相反的文档，后人一定会读到错的那份。

### 修订一：超时与延时改由 tokio task 自己管，不进 `ControlFlow`

总设计 §5 写「超时检测依赖 `ControlFlow::WaitUntil(deadline)`……事件循环三个分支都要
复位 control_flow，否则就是 T7」。**本切片不这么做。**

理由：T7（首次节流后永久 100% CPU 忙转）是本项目已知最恶的坑，它的现场就是
`ControlFlow` 三分支的复位。为一个**与渲染完全无关**的业务 deadline 再去改那三个
分支，是在已经踩过雷的地方重新布雷，而收益为零——`write_scheduled` 已经跑在 tokio
上，`ready_timeout` 用同一个 `select!` 管即可，一行 `ControlFlow` 都不必动。

改法：winit 线程只负责发两个信号（「首字节到了」「用户接管了」），其余全在 task 内。
代价是要新增一个 `UserEvent` 变体回送结束状态——而这正是本项目已有的成熟模式
（`ConnectOk` / `PaneOpened` / `ProbeOk` 都这么干）。

附带好处：整条链路可用 `#[tokio::test(start_paused = true)]` 假时钟零网络单测，
而帧循环里的 deadline 只能靠构造时钟注入间接测。

### 修订二：`Arc<SshSession>` 不是架构改动

P1-a 实现计划留了一条前置提醒：「`SshSession` 目前在 app 里是被直接拥有的，P1-b 需要
把 pane 持有的 `SshSession` 改成 `Arc<SshSession>`」，并建议单独过一轮设计。

**实际不需要。** `PaneState.pty` 的类型是 `Box<dyn PtyWriter>`
（`shell/workspace/mod.rs:67`），`SshSession` 只是被装进去的实现之一；而 `SshSession`
内部只有一个 `mpsc::Sender<SshCmd>`，本身就是 `Send + Sync`。

所以：新增 `impl PtyWriter for Arc<SshSession>`（转发 `write`/`resize` 两个方法），
`ConnectOk` 里 `let ssh = Arc::new(ssh)`，`Box::new(ssh.clone())` 进 pane，`ssh` 本体
作 `Arc<dyn ByteSink>` 交给 task。`PaneState` 字段类型不变，既有调用点零改动。

### 未修订、逐条沿用的部分

总设计 §2 的核心约束（一步不变式）、§3 数据层、§6 安全边界（env 明文）、§7 两条勘误
（只有第一个 pane 跑自动化 / 只做 `export` 行）全部照旧，本切片不得触碰。

---

## 2. 架构与新增单元

新增 `crates/mullion-app/src/automation.rs`。`app.rs` 已 2684 行，状态机不往里塞——
总设计 §8 也明确要求「本期新增逻辑放独立模块，只在事件循环里留触发点」。

模块内两部分，边界清晰：

| 部分 | 内容 | 可测性 |
|---|---|---|
| 纯决策 | `decide_start`、`status_text`、`split_pasted_commands` | 裸单测，无 tokio、无 egui |
| async 运行 | `run(sink, steps, ready_rx, cancel_rx, timeout) -> Outcome` | `start_paused` 假时钟 + FakeSink，零网络 |

依赖方向不变：`app → {store, ssh}`。store 出 `ResolvedAutomation` 与 `Vec<Step>`，
ssh 出 `write_scheduled`，app 是唯一同时认识两者的地方。

```rust
/// 一次自动化的最终结局。回送给 UI 只为了给用户一句话，不驱动任何后续动作。
pub enum Outcome {
    Completed(usize),   // 携带步数,状态栏要显示
    Aborted,            // 用户接管
    SkippedTimeout,
    Disconnected,
    Congested,
}
```

`App` 上挂一个 `Option<AutomationHandle>`：

```rust
struct AutomationHandle {
    pane: PaneId,                        // 只认这一个 pane 的首字节
    generation: u64,                     // C1:跨重连世代过滤,同 PaneOpened
    steps: usize,                        // 供 Completed 文案
    ready: Option<oneshot::Sender<()>>,  // take() 后 send
    cancel: Option<oneshot::Sender<()>>, // take() 后 drop 即取消
    task: JoinHandle<()>,
}
```

`ready`/`cancel` 是 `Option` 而非直接持有：两者都是**一次性**边，`take()` 天然保证
不会重复触发，也省掉一个「是否已触发」的布尔标志。

---

## 3. 数据流与四条触发边

```
spawn_connect(cfg) 时(winit 线程,此刻才确定是哪条会话)
 └ session_id = ui.connect_request_last
   Some(id) ⟹ store.resolved(id)?.automation + store 里那条记录的 name
              build_plan(&automation, name) → Vec<Step>
   None     ⟹ 空计划(CLI 直连,见下)
   App.pending_automation = Some(PendingAutomation{ steps, ready_timeout_ms })

ConnectOk 抵达
 └ 取走 pending_automation;decide_start(&steps, skip_flag) == true ⟹
     ssh = Arc::new(ssh);  pane 拿 Box::new(ssh.clone())
     建 ready/cancel 两条 oneshot
     spawn automation::run(ssh as Arc<dyn ByteSink>, steps, ready_rx, cancel_rx, timeout)
     App.automation = Some(AutomationHandle{..})

run 内:
  select!{ biased;
           cancel → Aborted
         ; ready  → write_scheduled(sink, steps, cancel).into()
         ; sleep(ready_timeout_ms) → SkippedTimeout }
  → proxy.send_event(UserEvent::AutomationDone(generation, outcome))
```

### 为什么在 `spawn_connect` 算，而不是 `ConnectOk` 里算

两个理由。其一，`ConnectOk` 事件不携带 `SessionId`（见 `UserEvent` 定义），到那时只能
读 `ui.connect_request_last`，而连接在途期间用户完全可能在会话管理器里改了配置甚至
删了这条会话——发出去的字节就跟用户当初点「连接」时看到的配置对不上了。其二，
`store.resolved()` 是同步 IO 边界内的查库，放在 `spawn_connect`（用户点击的那一帧）
比放在 `ConnectOk`（可能几秒后、代理链路下更久）语义清楚。

### CLI 直连没有会话记录

`mullion user@host -p 22 -i key` 这条路径（`App.initial`）压根没有 `SessionId`，
`connect_request_last` 为 `None`。**此时计划为空，不跑自动化**——没有配置来源，
凭空猜一个 tmux 会话名去 attach 是最坏的行为。这不是退化：CLI 直连本来就是调试
入口，要自动化就存成会话。配一条守护测试。

### 数据入口用 `SessionStore::resolved`，不是 `Vault::resolve_for`

App 持有的是 `crate::shell::store::SessionStore`（`app.rs:145`），它已经封好了
`resolved(id) -> Result<ResolvedConfig, StoreError>`（`shell/store.rs:119`），内部就是
`Vault::resolve_for`。app 不该越过这层门面直接够 `Vault`。

`build_plan` 还需要会话名做 tmux 的 `fallback_name`，而 `ResolvedConfig` 不含它——
从 `store.list()` 里按 id 找那条记录取 `identity.name`。若要新增一个
`SessionStore::automation_plan_for(id)` 把两步合起来，属实现计划的自由。

四条边：

| 边 | 触发点 | 动作 |
|---|---|---|
| 首字节到达 | `PaneState` 新增 `saw_first_byte: bool`，`Workspace::pump` 收到非空入站字节时置位 | App 每帧查 handle.pane 的这个标志 ⟹ `ready.take().send(())` |
| ready 超时 | `run` 内 `sleep` | `Outcome::SkippedTimeout` |
| 用户接管 | app.rs 中**所有用户意图的 PTY 写入点**（当前四处：粘贴 `:474`、滚轮上报 `:1123`/`:1134`、键盘 `:1221`） | `cancel.take()` 后 drop |
| 断线 | `Workspace::pump` 把 pane 置 `PaneStatus::Disconnected`，App 每帧查 | 同上，`cancel.take()` 后 drop |
| 关窗 / 重连 | 新 `ConnectOk`、`suspended` | 整个 handle 置 `None`，drop 即取消 |

**首字节检测不要让 `pump` 返回 `Vec<PaneId>`**：`pump` 每帧必调，返回 `Vec` 等于每帧
一次堆分配，而本项目对帧路径上的无谓开销一向敏感（T3）。挂个 `bool` 在 `PaneState`
上零分配，而且「这个 pane 收到过字节没有」本来就是 pane 自己的状态。

**四处写入点的行号会漂**，实现时以 `grep 'pty.write'` 为准：`app.rs` 里全部命中都是
用户意图，`shell/workspace/mod.rs` 里那一处是 T1 应答（见下），唯一要排除的就是它。
将来新增用户输入路径（如鼠标按钮上报）也必须一并接上 cancel。

### 必须点名的陷阱：cancel 绝不能挂在 `PtyWriter::write` 上

`Workspace::pump` 内部也调 `p.pty.write(out)`（`shell/workspace/mod.rs:252`）——那是
**T1 的 PtyWrite 应答**（DSR 光标查询、同步输出探测的回应），不是用户输入。若把
「有人往 PTY 写字节」当成用户接管的判据，远端一发同步输出探测，自动化立刻自杀，
而且现象是「有时候能跑有时候跑不了」，极难定位。

判据只能是 app.rs 那四个**用户意图**写入点。配一条守护测试直接扎在这上面。

### 只有第一个 pane 跑自动化

`AutomationHandle.pane` 在 `ConnectOk` 里固定为首个 `PaneId`；`PaneOpened` 分支一行
不加。总设计 §7 前提②：分屏新开的 pane 是干净 shell，所有 pane attach 同一 tmux
session 会内容镜像，且 `window-size` 取 `latest` 会反复 reflow、取 `smallest` 会留白。

### `initial_delay_ms` 已经在计划里，不要再等一次

`build_plan` 生成的**第一个 `Step` 的 `delay` 就是 `initial_delay_ms`**（P1-a
`automation.rs` 已如此实现）。`run` 在收到 ready 信号后应**立即**把 steps 交给
`write_scheduled`，不得在中间再 sleep 一次 `initial_delay_ms`——那会让实际延时翻倍，
而且这种 bug 在真机上只表现为「感觉慢了一点」，没人会发现。

### 超时起点的已知偏差

总设计要求「从 `open_pty` 返回起算」。实际起点是 task 被 spawn 的时刻，比 `open_pty`
返回晚一次 winit 事件投递（毫秒级）。差值远小于 15s 默认值，本切片不做补偿，记录在案。

---

## 4. 状态栏与跳过入口

`status_text(Outcome) -> Option<String>`，纯函数：

| 状态 | 文案 |
|---|---|
| 运行中（等就绪 / 执行中） | `自动化：进行中` |
| `Completed(n)` | `自动化已完成（{n} 步）` |
| `Aborted` | `自动化已中止：检测到你的输入` |
| `SkippedTimeout` | `自动化已跳过：{n}s 未收到远端输出` |
| `Disconnected` | `自动化已中止：连接已断开` |
| `Congested` | `自动化已中止：出站队列拥塞` |

`Completed` 也显示，沿总设计 §8「已完成 N 步」——用户看不见发生了什么，就无法判断
自动化是没跑还是跑了没效果。

**文案的生命周期**：`AutomationDone` 抵达时写进 `App.automation_status: Option<String>`，
**一直显示到下一次 `spawn_connect`**（那时清空）。不做定时淡出——状态栏本来就是常驻
信息区，而定时清除需要再引一个 deadline 进帧循环，正是修订一要避免的东西。

**`AutomationDone` 必须按世代过滤**（`AutomationHandle.generation` 就是为此存在）：
高延迟链路下，用户完全可能在自动化还在跑的时候断开重连，旧世代的「自动化已中止：
连接已断开」落到新连接的状态栏上，是一条与当前连接毫不相干的误导信息。判据同
`PaneOpenErr` 的 `generation_matches`。

### F44 的一次性跳过入口

会话列表右键菜单加「连接（跳过自动化）」（`list.rs:323` 已有 `context_menu`）。

`UiState` 加 `connect_skip_automation` 意图标志，`spawn_connect` 时转存进
`App.pending_skip_automation`，`ConnectOk` 消费后**立即清零**。

必须是一次性的：右键跳过一次之后，普通双击连接若还静默跳过，用户会以为自动化坏了。
配守护测试。

### 日志

沿总设计 §5：只记步数、字节长度与结束原因，**不记命令原文与 env 值**。

---

## 5. 编辑器第四个 tab「登录后」

`TABS` 由 `["连接","认证","高级"]` 扩为 `["连接","认证","高级","登录后"]`，
新增 `fields::automation()`。

放第四个 tab 而不是塞进「高级」：「高级」现在是代理 + 跳板链，再加 tmux + 命令表 +
env 表 + 三个延时会长到必须滚动才能找到东西。自动化也是唯一会**主动往远端发字节**
的配置，值得独立一页。

### 编辑缓冲

`EditorBuffer.preserved_automation: AutomationPrefs` 直接变成可编辑的 `automation`。
不另造 `*Ui` 枚举——`AutomationPrefs` 字段本来就全是 `Option`，类型自身表达三态
（`None` = 继承 / `Some(_)` = 显式）。`ProxyModeUi` 当年要造，是因为 `ProxyChoice`
枚举里没有 Inherit 变体，这里没有那个问题。

### 控件

| 字段 | 控件 |
|---|---|
| F44 `enabled` | 三态下拉 `继承 / 开 / 关` |
| F40 `tmux` | 三态下拉 `继承 / 不用 / 自动 attach` + 会话名输入；留空时 placeholder 实时显示 `sanitize_tmux_name(会话名)` 的推导结果 |
| F41 `commands` | 动态行：文本 + 「设延时」勾选 + 毫秒数 + 上移 / 下移 / 删除 |
| F42 `work_dir` | 单行输入 |
| F43 `env` | 动态 key/value 行 + 固定警告文案 |
| 三个延时 | 勾选框 + `DragValue`；未勾选时显示内置默认值作提示 |

### 两条硬性文案（spec 直接要求，不是可选润色）

- env 区固定显示：「环境变量不是存密码的地方——值以明文存进 `sessions.toml`，并会以
  `export` 行发到远端，落进 shell 历史与 `/proc/<pid>/environ`。要存密码请用凭据。」（F43）
- 只要有任一条命令配了延时，页顶出现：「配了延时的命令会拆成多步发送。第二步起，
  字符会进入当时屏幕上的任何程序——如果远端已经 attach 上 TUI，它们会被打进那个
  程序的输入框。」（F41）

### F41 多行粘贴

粘贴含换行的文本 ⟹ 自动拆成多条命令，空行丢弃；手敲回车等同「新增一条」。

数据层已经会静默丢弃含换行的命令（`command_containing_newline_is_dropped`），UI 必须
在进库前拦住，否则用户配了一条会凭空消失的命令。拆条而非剥换行：`a && b` 与 `a; b`
语义不同，静默拼接会改变行为；拆条则结果仍符合数据层语义——默认仍合成一行用 `;`
连接发出，不破「恰好一步」不变式。

纯函数 `split_pasted_commands(&str) -> Vec<String>` 单测。

### 连带改动

`Missing::tab()`（F91 缺项红点）与 `TAB_*` 常量随之扩展。沿用现有 `usize` 索引，
不重构成 enum——`validate.rs:22` 已注明换 enum 会波及所有 Tab 相关代码，不在本切片范围。

---

## 6. 测试矩阵

| 层 | 测试 | 验什么 |
|---|---|---|
| app 纯函数 | `disabled_automation_does_not_start` | F44 关闭 ⟹ plan 空 ⟹ 不建 oneshot、不 spawn |
| app 纯函数 | `skip_flag_suppresses_start_even_when_plan_is_not_empty` | 右键一次性跳过 |
| app 纯函数 | `skip_flag_is_consumed_after_one_connect` | 跳过一次后下次连接不跳过 |
| app 纯函数 | `status_text_*` | 六种 Outcome 各一条文案 |
| app 纯函数 | `split_pasted_commands_*` | 多行拆条 / 空行丢弃 / 单行不变 / 全空白得空表 |
| app 异步 | `ready_signal_starts_the_plan` | 首字节到达才发第一步（假时钟） |
| app 异步 | `no_first_byte_within_timeout_is_skipped` | 超时跳过，且**一个字节都没发** |
| app 异步 | `cancel_before_ready_aborts` | 等待期取消 |
| app 异步 | `cancel_during_run_stops_remaining_steps` | 执行期取消 |
| **app 守护** | `pty_write_echo_does_not_cancel_automation` | **T1 回写不得被当成用户输入** |
| app | `pump_marks_saw_first_byte_only_for_panes_with_inbound` | 首字节标志置位时机 |
| app | `automation_is_only_armed_for_the_first_pane` | `PaneOpened` 不带自动化 |
| app | `cli_direct_connect_has_no_plan` | 无 `SessionId` ⟹ 空计划（不猜 tmux 名） |
| app | `automation_done_from_a_stale_generation_is_ignored` | 世代过滤，同 `PaneOpenErr` |
| app | `status_is_cleared_when_a_new_connect_starts` | 文案生命周期 |
| app | `frame::tests` 既有用例 | T7 —— 本切片不动 `ControlFlow`，这些必须原样绿 |
| app | `app::tests::reflow_emits_resize` | T4 |

异步测试要用 `tokio::time::pause()`，需给 `mullion-app` 的 **dev-dependencies** 的
tokio 加 `"test-util"` feature（P1-a 已给 `mullion-ssh` 加过同一个，照抄即可）。

`frame::tests` 在矩阵里是**回归**而非新增：修订一的直接后果就是这批测试不该有任何
改动。若实现中发现需要动它们，说明又把 deadline 塞进帧循环了，停下来。

---

## 7. 人工验收清单（无头环境验不了，写进 Release notes）

沿总设计 §9，本切片是它们第一次真正可验：

1. Windows 实机连上即落在远端 tmux 里的 Claude Code
2. **断线重连回一个正在跑的 Claude Code**：确认没有任何字符被打进输入框（总设计 §2 核心）
3. 关掉自动化（F44）后行为与今日逐字一致
4. 自动化跑到一半时敲键盘：剩余命令不再发出，状态栏给出提示
5. 分屏新开的 pane 是干净 shell，不重复跑自动化
6. **等待期按一下回车**（"催一下"是很常见的动作）：确认状态栏说清了自动化被取消，
   用户不会以为「连上了但什么都没发生」
7. 远端 `.bashrc` 里配了自动 attach tmux 时，关掉 F40 只用命令列表：确认命令没有打进 TUI
8. 中文会话名 sanitize 后 tmux 能正常 attach
9. 高延迟代理链路下 15s 默认超时是否够用

---

## 8. 本切片不做的事

- 分组编辑器的「登录后」分节。数据层（`GroupRecord.automation`）与继承 P1-a 已就绪，
  但分组编辑器是另一套 UI 代码；会话级先跑通、实机验过再说。
- 自动化的「立即重跑」按钮。总设计 §2 的核心约束是「一旦屏幕归属可能变了就不再发」，
  重跑按钮与它正面冲突，要做得先定清楚语义。
- 把 `ControlFlow` 相关的任何东西改成为自动化服务（见修订一）。

---

## 9. 交付

本切片有用户可见行为，触发 CLAUDE.md 的一条龙交付约定：版本号 bump → 跑绿 →
交叉编译 + objdump 依赖验收 → 发 GitHub Release（标题纯版本号）→ 报链接与验收清单。
