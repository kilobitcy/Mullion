# 切片 P1 设计：登录后自动化（F40~F44）

> 日期：2026-08-05 · 状态：设计已确认，待写实现计划
> 上游：`docs/superpowers/specs/2026-07-30-session-management-roadmap-design.md` §5
> 本设计**推翻该 §5 的两条既定前提**，见 §7「路线图勘误」。

---

## 1. 目标与边界

一句话：**连上去就落在远端 tmux 里的 Claude Code 前，不用每次手敲 `tmux attach`。**

在范围内：
- F40 tmux 自动 attach（不存在则建）
- F41 登录后命令列表（逐条可设延时）
- F42 初始工作目录
- F43 环境变量注入
- F44 自动化总开关

明确不在范围内：
- 不做脚本引擎（`spec.md` 4.10 §11 已登记拒绝理由）
- 不解析远端输出、不做提示符匹配、不做 expect 式交互
- 不做 tmux grouped session（见 §7）

---

## 2. 核心约束：第一个字节之后，屏幕上是什么就不由我们说了算

先澄清一个容易搞反的事实：**每次 SSH 连接都会拿到一个干净的 login shell**。断线重连
时新 channel 请求 pty，sshd fork 一个新 shell；那个还在跑的 Claude Code 活在 tmux
server 里，隔着一层，我们的第一个字节不可能直接打进它的输入框。

危险的不是第一个字节，是**第二个**。我们自己发出的 `tmux attach` 一旦生效，屏幕上就
从 shell 变成了那个正在跑的 Claude Code TUI；此后再发 `cd /srv` 或任何命令行，字符
就进了它的输入框，回车被当成一条提问发出去——**这是数据破坏，不是体验瑕疵**。

而客户端无法可靠判断 attach 有没有生效、生效后面对的是什么（提示符匹配形态千差万别，
路线图 §5 前提①已否掉）。所以规则是：

> **自动化只在「确定还是干净 shell」的那一个窗口期内发字节。一旦发出可能改变
> 屏幕归属的东西（attach / 启动 TUI），就不再发第二个字节。**

落地成：把整套自动化编码成**一行** shell 条件表达式，由远端自己原子判断分支，
客户端不解析任何远端输出，也不发第二步。

```sh
tmux has-session -t <NAME> 2>/dev/null && exec tmux attach -t <NAME> \
  || exec tmux new-session -s <NAME> -c <DIR> '<export…; cmd…; exec $SHELL>'
```

- 会话已存在 → 走 attach 分支，**一个字节的命令都不会进已有会话**
- 会话不存在 → 走 new-session 分支，命令作为新会话的启动命令由 tmux 执行，
  不经过我们的 pty
- 两条分支都 `exec`，避免留一层多余的 shell

用 `has-session || new-session` 而不是路线图原文的 `new-session -A`，正是因为
`-A` 无法区分「新建」与「附着」，命令列表和工作目录就没有安全的落点。

**同一条规则约束无 tmux 分支**（见 §3）：那条路径同样不许无条件拆多步。用户的
`.bashrc` 里写一句自动 attach tmux 是极常见的配置，届时第二步开始就打进 TUI，
与上面是同一个坑。

---

## 3. 数据层（mullion-store，零 IO、零 async）

新文件 `crates/mullion-store/src/automation.rs`：

```rust
/// 登录后自动化（可继承分节）。F40~F44。
#[derive(Default, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationPrefs {
    pub enabled: Option<bool>,                      // F44，None=继承
    pub tmux: Option<TmuxChoice>,                   // F40
    pub commands: Option<Vec<AutomationCommand>>,   // F41，Override 语义
    pub work_dir: Option<String>,                   // F42
    pub env: Option<Vec<EnvVar>>,                   // F43，Override 语义
    pub initial_delay_ms: Option<u32>,              // 首次输出后再等多久
    pub inter_delay_ms: Option<u32>,                // 行间延时（仅无 tmux 分支用得上）
    pub ready_timeout_ms: Option<u32>,              // 多久没收到字节就判定登录失败
}

#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TmuxChoice {
    Off,                                     // 显式不用 tmux（≠ None 的「继承」）
    Attach { session_name: Option<String> }, // None = 由 identity.name 推导
}

pub struct AutomationCommand { pub text: String, pub delay_ms: Option<u32> }
pub struct EnvVar { pub key: String, pub value: String }
```

`Option<T>` 表达继承是既有约定（`None` = 继承上游）。`TmuxChoice::Off` 与
`None` 必须可区分——这与 `ProxyChoice::Direct` 是同一个坑：会话要能显式地
「就是不要用分组配的那个 tmux」。

`commands` / `env` 取 **Override** 而非拼接（路线图 §4.2 已定），因为拼接会产生
「为什么多跑了一条命令」这类极难排查的问题。

**继承接线**：`PrefsLayer` 加第五个方法 `fn automation(&self) -> &AutomationPrefs`，
三个实现（`SessionRecord` / `GroupRecord` / `SessionDraft`）同步跟进；`ResolvedConfig`
加 `automation: ResolvedAutomation` 字段，逐字段走既有的 `resolve_override`。

**schema v3 → v4**：一次性迁移 + `.bak` 备份（沿用 `migrate.rs` 既有形状）。
v3 记录读出来 `automation` 全为 `None`，语义等价于「不做自动化」，迁移无损。

### 纯函数（同文件）

```rust
pub struct Step { pub delay: Duration, pub bytes: Vec<u8> }
pub fn build_plan(a: &ResolvedAutomation, fallback_name: &str) -> Vec<Step>;
pub fn sanitize_tmux_name(raw: &str) -> String;  // tmux 会话名不能含 '.' 与 ':'
pub fn shell_quote(raw: &str) -> String;         // 单引号包裹 + '\'' 替换
```

`build_plan` 两条分支：

- **有 tmux**：恰好一个 `Step`，`delay = initial_delay`，`bytes` = §2 那一行 + `\r`
- **无 tmux**：
  - **默认（没有任何一条命令设了 `delay_ms`）→ 也是恰好一个 `Step`**：
    `export A=…; cd <DIR>; cmd1; cmd2` 用 `;` 连成一行，一次发完。这是 §2 那条规则的
    直接推论——多发一步就多一次「屏幕已经不归我们了」的机会。
  - **仅当用户显式给某条命令配了 `delay_ms` 才拆多步**：首步用 `initial_delay`，
    其后每步 `delay_ms.unwrap_or(inter_delay_ms)`，顺序为 `export` 行 → `cd` 行 →
    命令列表。UI 在这个字段旁必须写明：**拆多步意味着后续命令会发给当时屏幕上的
    任何东西**，包括你的 `.bashrc` 自动 attach 进去的 tmux。

tmux 会话名默认由 `identity.name` 经 `sanitize_tmux_name` 生成，可在会话里显式覆盖。

### 引号层数（定死，实现必踩）

只做**一层**转义，各自负责：

- 会话名、工作目录、每个 env 的**值**：各自经 `shell_quote` 单引号包裹一次
- **命令文本原样拼接，不再 quote**——它本来就是 shell 语法，用户写 `echo 'hi'`
  就该原样跑，再包一层直接炸
- 有 tmux 分支里最外层那对包住 `<export…; cmd…; exec $SHELL>` 的单引号，由
  `build_plan` 对**拼好的整串**统一转义一次，不由各片段自己带

### 空启动命令的边界

用户既没配命令也没配 env 时，`new-session` 的启动命令串为空——此时**整个 `'…'`
参数省略**，只发 `tmux new-session -s X -c DIR`。生成 `new-session -s X ''` 会让
tmux 去跑一个空命令，行为随版本而异。同理 `-c DIR` 在 `work_dir` 为空时省略。

**行终止符一律 `\r`**，复用 `keymap.rs` 里 Enter 键的既有约定。待 F25（回车 CR/CRLF）
落地后两条路径统一改走同一份配置——不允许「人手敲回车」与「自动化发送」用不同约定。

---

## 4. 调度（mullion-ssh）

新文件 `crates/mullion-ssh/src/schedule.rs`：

```rust
pub trait ByteSink: Send + Sync {
    fn write(&self, bytes: Vec<u8>) -> Result<(), TrySendErr>;
}
impl ByteSink for SshSession { /* 转调既有 try_send */ }

pub enum ScheduleOutcome { Completed, Cancelled, Disconnected, Congested }

/// 只认延时与字节，不认识 tmux / 自动化 / 会话。
pub async fn write_scheduled(
    sink: Arc<dyn ByteSink>,
    steps: Vec<(Duration, Vec<u8>)>,
    cancel: oneshot::Receiver<()>,
) -> ScheduleOutcome;
```

三条设计理由：

1. **调度归 ssh 而非 store**：这是真实 async 行为，进 store 就破了「零 async」红线。
2. **也不进 app 事件循环**：定时靠 `tokio::time::sleep`，不靠帧循环——否则会与
   T3/T7 的帧率节流打架（路线图 §5 前提③要挡的正是这个）。
3. **`ByteSink` 抽象存在的唯一理由是可测**：有了它就能用假 sink +
   `tokio::time::pause()` **零网络**验证「顺序 / 延时 / 取消 / 断线即停」四条。

取消用 tokio 自带的 `oneshot`，不引入 `tokio-util` 的 `CancellationToken`——
每个 pane 的自动化只会被取消一次，一次性语义正好，且不加新依赖。

**`Step` 与 `(Duration, Vec<u8>)` 的关系**：`write_scheduled` 刻意只收元组，
不认识 store 的 `Step` 类型（否则 ssh 就依赖 store 了，违反单向依赖）。转换是
**app 侧一行 `map`**：`plan.into_iter().map(|s| (s.delay, s.bytes)).collect()`。
不在 store 里给 `Step` 加 `From`，也不在 ssh 里定义第二份 `Step`。

---

## 5. 接线（mullion-app）

```rust
enum AutomationState {
    Pending { plan: Vec<Step>, deadline: Instant },  // 等首字节
    Running { cancel: Option<oneshot::Sender<()>> },
    Skipped(SkipReason),                             // Timeout | UserInput | Disabled
    Done,
}
```

四条触发边：

| 事件 | 动作 |
|---|---|
| 该 pane 首次收到**非空**字节 | `Pending` → spawn task 调 `write_scheduled` → `Running` |
| 帧循环发现 `now > deadline` 且仍 `Pending` | → `Skipped(Timeout)`，状态栏提示，**绝不补发** |
| 任何用户输入（键盘 / 粘贴 / 鼠标上报） | `Pending` → `Skipped(UserInput)`；`Running` → `cancel()` |
| pane 关闭 / 链路断开 | `write()` 返回 `Closed`，任务自行结束 |

**「首次收到 PTY 输出」由 app 侧检测**——字节本来就流经 app 的帧循环，
`mullion-ssh` 不该认识「自动化」这个概念。

**`ready_timeout` 从 `open_pty` 返回时起算**（不是从连接建立、也不是从 TCP 握手）。
前面那段的耗时归代理 / 跳板 / 认证管，把它算进「登录后多久没输出」只会让代理链路下
的用户莫名其妙地被跳过自动化。

**用户接管优先**：一旦收到任何用户输入，立即中止**剩余全部**自动化命令，
并在状态栏给一句「自动化已取消（用户输入）」。用户已经开始打字，说明他不需要
自动化了，此时继续插入字节就是抢输入。

**必须点名的接线陷阱**：超时检测依赖 `ControlFlow::WaitUntil(deadline)`。远端一个
字节都不发时帧循环本来就该睡着（T3），若不为这个 deadline 显式设唤醒点，超时永远
不会被发现；而设了之后**事件循环三个分支都要复位 control_flow**，否则就是 T7 那个
「首次节流后 100% CPU 忙转」。实现计划里单列一步，并跑 `frame::tests`。

**只有连接的第一个 pane 跑自动化**（见 §7 前提②勘误）。代码里正好落在
`spawn_connect` 与 `spawn_fresh_panes` 两条独立路径上，分屏新开的 pane 走后者，
天然不带自动化，不需要额外判断。

### 错误与日志

- `TrySendErr::Full`（出站队列满）：退避重试 3 次，仍失败 → `Congested`，提示后放弃，
  不无限重试
- 日志走 adr-008 的 `log` facade，**只记步数、字节长度与结束原因，不记命令原文与
  env 值**——用户不该把口令写进命令列表，但一定会有人写，日志不该成为泄漏点

### 用户可跳过（两层）

1. 会话字段 F44 `enabled`（持久，可继承）
2. 会话列表右键「连接（跳过自动化）」（一次性，排障用）

---

## 6. 安全边界：环境变量不是存密码的地方

F43 的环境变量**存明文进 `sessions.toml`，不进 `secrets.enc`**。UI 必须写明这一点。

把 env 值当凭据加密存储会给用户一个错误的安全承诺：值终归要以 `export` 行的形式
发进远端 shell，会落进 shell 历史、`/proc/<pid>/environ`、以及任何抓终端的东西。
加密只是让用户以为它安全了。要存密码就用凭据（F70/F71/F74）。

---

## 7. 路线图勘误（推翻 §5 的两条前提）

### 前提② 分屏语义：改为「只有第一个 pane 跑自动化」

原文规定「每个 PTY channel 各跑一次自动化，tmux 会话名倾向不附 pane 序号」。
这条走不通：不附序号意味着所有 pane attach 同一个 tmux session，于是

- 所有 pane **显示完全相同的内容**（tmux 多客户端 attach 的本义），分屏的意义没了
- 尺寸互压：`window-size latest` 会让每个新 pane attach 都触发全局 reflow（正在跑的
  TUI 反复重排版）；`window-size smallest` 则所有 pane 被压到最小的那个尺寸并留白

附序号（每 pane 一个独立 tmux session）也不对——那是四个互不相干的会话，
用户想要的「四格看同一台机器的不同工作」用普通 shell 就够了，不需要四个 tmux server 会话。

**结论**：只有连接建立时的第一个 pane 跑自动化，分屏新开的 pane 是干净 shell。
tmux grouped session（`tmux new-session -t <src>`，共享窗口集合但各自独立当前窗口
与尺寸）是这个问题的真解，**登记进 `spec.md` 4.10「已登记未排期」**，本期不做。

### 「优先 SSH env 请求」：删掉，只做 `export` 行

原文 F43 写「优先 SSH env 请求，服务端拒绝则回退到 `export` 行」。SSH env 请求在
本项目主场景里**结构性无效**：

1. sshd 的 `AcceptEnv` 默认只放行 `LANG` / `LC_*`，用户自定义变量会被静默丢弃
2. 更硬的一条：tmux attach 之后面对的 shell 是 **tmux server 早先 fork 出来的**，
   它不继承本次 channel 的环境，env 请求即使被接受也到不了那个 shell

**结论**：只做 `export` 行，且必须在 attach 之后发（在 new-session 分支里作为启动
命令的一部分）。省掉一条永远走不通的分支和它的回退逻辑。

---

## 8. 切片边界

### P1-a（数据层 + 调度，零 GUI）

- `mullion-store/src/automation.rs`：类型 + `build_plan` / `sanitize_tmux_name` / `shell_quote`
- `PrefsLayer` 加 `automation()`，三实现跟进；`ResolvedConfig` 加字段
- schema v3 → v4 迁移 + `.bak`
- `mullion-ssh/src/schedule.rs`：`ByteSink` + `write_scheduled`

### P1-b（GUI 接线 + 交付）

- 会话/分组编辑器新增「登录后」分节（F40~F44 五项）
- `app.rs` 的 `AutomationState` 状态机与四条触发边
- 会话列表右键「连接（跳过自动化）」
- 状态栏三条提示：已取消（用户输入）/ 已跳过（登录超时）/ 已完成 N 步
- 版本号 bump + 交叉编译 + Release（CLAUDE.md 交付约定一条龙）

`app.rs` 已 2684 行，本期新增逻辑放独立模块，只在事件循环里留状态机的触发点。

---

## 9. 测试矩阵

| 层 | 测试 | 验什么 |
|---|---|---|
| store 纯函数 | `build_plan_tmux_branch_is_single_atomic_line` | 有 tmux 时**恰好一个 Step**，且含 `has-session && exec attach \|\| exec new-session` |
| store 纯函数 | `build_plan_no_tmux_is_single_step_unless_per_command_delay` | 无 tmux 且无逐条延时时**也只有一个 Step**（§2 规则的推论） |
| store 纯函数 | `build_plan_no_tmux_orders_export_cd_then_commands` | 配了逐条延时才拆多步，且顺序与延时正确 |
| store 纯函数 | `shell_quote_escapes_single_quote` | 注入面：会话名/目录/env 值含引号、空格 |
| store 纯函数 | `command_text_is_not_quoted_so_user_shell_syntax_survives` | 命令文本原样拼接：`echo 'hi'` 不被二次转义 |
| store 纯函数 | `empty_start_command_omits_quoted_arg` | 没命令没 env 时不生成 `new-session -s X ''` |
| store 纯函数 | `sanitize_tmux_name_strips_dot_and_colon` | tmux 会话名不能含 `.` 与 `:` |
| store 纯函数 | `disabled_automation_builds_empty_plan` | F44 关闭时计划为空（配合 app 侧不构造状态机） |
| store 继承 | `automation_none_inherits_group` | `None` = 继承上游 |
| store 继承 | `tmux_off_is_not_inherit` | 「显式 Off」≠「继承」（`ProxyChoice::Direct` 同款坑） |
| store 迁移 | `migrate_v3_to_v4_adds_empty_automation` | 无损 + `.bak` 存在 |
| ssh 调度 | `write_scheduled_respects_delays` | 顺序 / 延时（`tokio::time::pause()` + 假 sink） |
| ssh 调度 | `write_scheduled_stops_on_cancel` | 取消后**不再写任何字节** |
| ssh 调度 | `write_scheduled_stops_when_sink_closed` | 断线即停 |
| app | `frame::tests` 既有用例 + deadline 唤醒不破坏节流 | T7 |
| app | `app::tests::reflow_emits_resize` | T4，新 pane 建立后仍须发 `window_change` |

**人工验收清单**（无头环境验不了，属 CLAUDE.md「你无法验证的东西」）：

1. Windows 实机连上即落在远端 tmux 里的 Claude Code
2. **断线重连回一个正在跑的 Claude Code**：确认没有任何字符被打进输入框（§2 的核心）
3. 关掉自动化（F44）后行为与今日逐字一致
4. 自动化跑到一半时敲键盘，确认剩余命令不再发出、状态栏给出提示
5. 分屏新开的 pane 是干净 shell，不重复跑自动化
6. **等待期按一下回车**（用户"催一下"是很常见的动作）：确认自动化被取消这件事在
   状态栏说清楚了，用户不会以为「连上了但什么都没发生」
7. 远端 `.bashrc` 里配了自动 attach tmux 时，关掉 F40 只用命令列表，确认命令没有
   打进 TUI（这是 §2 那条规则在无 tmux 分支上的验证）

---

## 10. 已同步的文档改动（commit `071e002`）

1. **`spec.md` 新增 4.9「登录后自动化」**——F40~F44 此前在 spec.md 里完全不存在，
   只活在路线图 §5，与刚修掉的 §8 储备区是同一类漏账。原 4.9「已登记未排期」
   顺延为 4.10，路线图 §11.3 的两处引用同步改。
2. **`spec.md` F74 的「schema v3→v4」改为「v4→v5」**。本切片先落地就拿走 4，
   规则是「谁先落地谁拿号」。
3. **路线图追加 §12 勘误**，记录本文 §7 推翻的两条前提。
4. **`spec.md` 4.10 新增一行**：tmux grouped session（多 pane 共享窗口集合、
   各自独立尺寸），重新评估条件为「多 pane 镜像被证明是真实痛点」。

---

## 11. 复核修订（2026-08-06）

设计写完后的一轮自审，改了六处：

1. **§2 的论据被推翻重写。** 原文写「断线重连时远端是一个正在等输入的 TUI」——
   **错的**。每次 SSH 连接都会拿到 sshd fork 的新 login shell，Claude Code 活在
   tmux server 里隔着一层，第一个字节安全。真正的危险是**第二个字节**：我们自己
   发的 `attach` 让屏幕易主了。结论不变，理由必须改——理由错了比没理由危险，
   下一个人会据「反正是干净 shell」把单步拆回多步。
2. **无 tmux 分支默认也收成单步**（`;` 连成一行），只有用户显式配了逐条延时才拆。
   原设计对该分支无条件多步，而 `.bashrc` 自动 attach tmux 是极常见配置，
   与分支①同坑。
3. **引号层数定死**：会话名/目录/env 值各 quote 一次，命令文本原样不 quote，
   最外层由 `build_plan` 整串统一转义。
4. **空启动命令的边界**：不生成 `new-session -s X ''`。
5. **`Step` → 元组的转换定在 app 侧一行 `map`**，不让 ssh 依赖 store。
6. **`ready_timeout` 起算点定为 `open_pty` 返回时**，不含代理/跳板/认证耗时。

另有两条只记边界、不改设计：等待期用户按回车即取消自动化（「输入即中止」的必然
结果，靠人工验收清单第 6 条兜住观感）；`AutomationState::Skipped(Disabled)` 变体
保留但正常路径用不到——F44 关闭时 `build_plan` 直接返回空计划，压根不构造状态机。
