# F160–F163 设计：叶子级现场恢复（pane → 节点 → tmux）

- 日期：2026-08-25
- 状态：已批准（grilling 会话逐条确认）
- 关联：F37（布局持久化）、F148（多实例现场历史）、F153（单击恢复 + 自动串行拨号）、
  F123/F124（远端状态上报 + tmux 自举）、F128（断线自动重连）、F141（重连接回原 tmux 会话）、
  F40~F44（登录后自动化）、B2-b（换节点）
- 目标版本：待定（落到 `mullion-app`，按交付约定走 release-windows）

---

## 1. 背景

「新开 exe 恢复现场」今天只恢复**骨架**：标签数、每标签的分屏树形状、焦点叶子、
窗口几何、Terminal/Files 类型。用户报的是它**没达到设计要求**——恢复回来的不是
上次那个现场。

拆开是四条症状，根因只有一个。

### 1.1 四条症状

**① 分屏 pane 恢复后全是裸 shell。**
恢复走 `apply_saved_tree`（`workspace/mod.rs:327`）摆回形状，新长出来的叶子交给
`spawn_fresh_panes`（`app.rs:5705`），它们走 `pending_for_extra_pane`
（`automation.rs:114`）→ `build_plan_without_tmux`（`store/automation.rs:266`），
**刻意跳过 tmux**。恢复一个 4 屏标签，只有第 1 块可能回到 tmux，其余 3 块停在登录目录。

**② 换过节点的 pane 恢复后连错机器。**
`SavedTab.session_id`（`store/layout.rs:116`）是**标签级唯一**的一个值；
`SavedNodeEntry`（`store/layout.rs:64`）只有 `dir`/`ratio`，叶子不带任何身份。
用户用「换节点」把某块 pane 搬到第二台机器之后，`ws.hosts` 里有两台，
而磁盘上只记得第一台——恢复时所有 pane 一起拨向那一个会话。

**③ tmux 名接回旧的。**
`build_plan` / `build_plan_reattach` 的会话名判据是 `tmux_session_name(配置, 会话名)`
（`store/automation.rs:217`），取自 F40 的会话配置，**不是关 exe 那一刻 pane 实际所在的会话**。

**④ 根本没 attach，一块都没回。**
这条不是接线断了——恢复路径确实设了 `connect_request_last`（`app.rs:2581`）
并走完整的 `spawn_connect` → `pending_for`。是**用户的 tmux 从来不在配置里**：
他靠 `.bashrc` 里的 alias `tt <名>`（= `tmux attach`）和 `tmux new -s <名>` 手敲进去。
`ResolvedAutomation.tmux` 是 `None`，`tmux_session_name` 返回 `None`，
`build_plan` 走无 tmux 分支——**一条 tmux 命令都不会发**。

### 1.2 唯一的根因

> 客户端**一直知道**每块 pane 在哪台机器的哪个 tmux，但**既不落盘、也不用它去 attach**。
> 落盘的身份只到标签级，attach 的依据只认会话配置。

「一直知道」不是推测：F124 自举给每条连接发
`tmux set -g set-titles on && tmux set -g set-titles-string '#S:#I:#W #{pane_current_path}'`
（`remote_bootstrap.rs:17`），用户手敲 `tt web01` 之后 tmux 就按这个格式发标题，
`parse_title` 取第一段得到会话名，写进 `PaneState.tmux`（`workspace/mod.rs:188`）。
**已在真机确认**：pane 标题条上看得到会话名且对得上。

`PaneState.host_ix` 同理——它精确指向 `ws.hosts` 里那台机器，只是从不落盘。

### 1.3 顺带修好的既有 bug

同一个判据坑了 F128。`build_plan_reattach`（`store/automation.rs:201`）的
`tmux_session_name` 返回 `None` 时给出**空计划**，文档明写「不静默回落」。
所以对手敲 tmux 的用法，**今天断线自动重连也接不回 tmux**——不只是新开 exe。
本设计把真值源改掉，两条路径一并覆盖。

---

## 2. 决策

每条都在 grilling 会话里逐条拍板。「为什么不是别的」是这份文档的主要价值。

### D1 — tmux 名的真值源改成运行时实测

落盘的是 `PaneState.tmux`（远端标题上报的实际会话名），不是配置里写的那个。
配置**只在实测为空时回落**。

为什么不是配置：配置表达的是「意图」，实测是「事实」。用户手敲进的会话配置里
根本没有；即使配了，他在远端 `tmux switch-client` 切过之后配置也是错的。
而恢复要的是**关 exe 那一刻的现场**。

### D2 — 只发 `attach`，绝不 `new-session`

按实测名发的命令是 `tmux attach -t <名>`。会话已不存在（远端重启过 / 用户自己
kill 了）就命令失败、停在裸 shell。

这**突破了现有红线**：`build_plan_reattach` 的文档写着「配置没走 tmux 就返回空，
不静默回落」，其精神是「不基于猜测发 tmux 命令」。突破的正当性在于实测名不是猜测——
它是远端自己报上来的。但仍守住更硬的那一半：**永不替用户在远端造东西**。

为什么不复用 `tmux_command`（`store/automation.rs:233`）：它内建了
`has-session && exec attach || exec new-session` 的回落，正是我们不要的那一半。
需要一条新的命令串。**只复用 `shell_quote`**（`store/automation.rs:108`，已 pub，
有 4 条单测覆盖命令替换/单引号），不新写转义。

新命令串**必须保留 `has-session` 守门**，砍掉的只是 `||` 后半段：

```
tmux has-session -t <名> 2>/dev/null && exec tmux attach[-d] -t <名>
```

这不是风格问题：裸的 `exec tmux attach -t X` 在会话不存在时，`exec` 已经把
shell 替换成 tmux 进程，tmux 报错退出 → channel 关闭 → **pane 直接死掉**。
D4 的「挂提示」和 D8 的「停在裸 shell」全部落空——shell 都没了。守门之后
`&&` 短路，shell 原地活着，才有裸 shell 可停。探测与 attach 之间的竞态窗口
沿用 `tmux_command` 文档里「已知且接受」的结论。

### D3 — 无 SessionId 的 pane：摆出形状但不拨号

快速连接 / CLI 直连（`mullion user@host -i key`）的 `HostConn.session_id` 是
`None`（`workspace/mod.rs:132`），没有可存的身份。那块叶子恢复成一个空 pane，
里面写明「当初是快速连接，无法自动恢复」。

为什么不丢掉那个叶子：分屏比例会静默变形——存的是 2×2，恢复回来变三块，
而没有任何提示。为什么不把完整 `SshConfig` 落盘：那会让 `layouts/*.toml`
多出一份连接参数副本，且要保留 10 份。

承载机制：没有 `PaneState` 的 pane 画不出文字（树里有 id 而无状态是 F35 的
「空窗期」约定，只是短暂空白）。空态 pane 要一个**带哑 pty 的 `PaneState`**
——F128 的 `Disconnected` pane（emulator + 死 channel）已是先例，沿用同一套，
不发明新的渲染路径。D6 的失败 pane、5.2 的排队 pane 同理。

### D4 — attach 失败必须可见

`automation::Outcome::Completed` 的语义只是「字节发出去了」（`automation.rs:20`），
远端 `tmux attach -t X` 返回什么客户端根本不看——**默认情况下 attach 失败完全静默**。

做法：发完 attach 等一段时间，比对 `PaneState.tmux` 是否变成期望的那个名字；
不符就在那块 pane 上挂一条提示（「当初的会话 web01 已不存在」）。
**不弹窗**——多块 pane 都失败时会连弹好几次。

这是实测那条腿的第二个用途：attach 成功后 tmux 必然按 F124 配的
`set-titles-string` 发标题，所以「接回来了没有」本来就是可观测的。

边界：这个判据**依赖 F124 在跑**。用户把 `tmux_bootstrap` 开关关掉时，
attach 成功也不会有标题上报，校验会恒误报「没接上」——开关关着就跳过校验
（attach 照发，只是不许下失败结论）。

### D5 — 同名冲突：首块带 `-d`，其余不带；**判据键是（机器, 会话名）二元组**

同一个标签里两块 pane 指向**同一台机器上的同一个**会话名时（用户在两块里都
敲了 `tt a`），按叶子前序，第一块发 `attach -d`，后续同名的发不带 `-d` 的
`attach`。

键**不能只是会话名**：pane A 在机器 X 的会话 `a`、pane B 在机器 Y 的会话 `a`
是两台 tmux 服务器上的两个互不相干的会话，都该带 `-d`（各踢各的残骸）。
按名字去重会让 B 白白不踢。机器一侧用叶子的 `session_id`（同一会话记录 =
同一台机器；恢复场景里没有比它更细的机器身份）。

`-d` 不能省：exe 崩溃/强杀后远端 tmux client 会残留到 TCP 超时，不踢的话两个
client 同时挂着、`window-size` 反复 reflow（F141 的原始理由）。
`-d` 也不能全加：第二块会把第一块踢成 detached，恢复出来一块死屏。

代价（已接受）：两块 pane 镜像同一会话时，tmux 的 `window-size` 会在两块尺寸
不同时取小/反复 reflow。那是用户当初主动开的镜像，忠实还原。

### D6 — 部分失败：pane 级降级，不是标签级

跨机器的标签恢复时某台连不上（关机 / 认证失败），**只有那块 pane** 置成
`PaneStatus::Disconnected` + 重连入口，其余 pane 照常用。

为什么不是全或无：一台机器关机就让另外两台也连不成，不成比例。
为什么不接 F128 的指数退避自动重试：认证失败类错误会反复重试到退避封顶，
远端多出几条登录失败记录。

### D7 — 有实测名就只发 attach，跳过配置的登录后命令

`automation.rs` 开篇就写着：attach 一旦生效，屏幕归那个 TUI，之后发任何字节
都是打进 TUI。所以实测 attach 与配置的 `cd`/`export`/启动命令**不能叠加**。

规则：
- 该 pane 有实测 tmux 名 → **只发 attach**，配置计划整个跳过
- 没有实测名 → 跑配置计划（= 今天的行为，零改动）

正当性：那些命令当初已经在那个 tmux 会话里跑过了，会话还在就意味着效果还在。
语义与 F141 `pending_for_reattach` 完全一致（它就是单步 attach）。

反例说明为什么必须这么定：用户某会话配了 `cd /srv && npm run dev`，
先 attach 进 tmux（dev 正跑着）再发这一串，就是往正在跑的进程里打字节。

### D8 — attach 失败后不补跑配置命令

失败检测发生在「发完等几秒看标题」之后，那时用户很可能已经在那块 pane 里
敲东西了。**延迟补发字节是本项目最危险的一类行为**（同 F156-c 只在 pane
刚建立时注入 OSC 7 的理由）。停在裸 shell，pane 上挂提示，下一步交给用户。

### D9 — `CURRENT_LAYOUT_SCHEMA` 不升版

新增字段全是 `Option` + `#[serde(default)]`，纯增量，字段含义没变。

不升的硬理由：`list_records` 对 `schema_version > CURRENT` 是**整条跳过**
（`store/history.rs:189`）。升到 2 之后一旦回滚到旧版 exe，整个恢复列表变空。
不升的话旧 exe 静默忽略叶子字段、回落到今天的标签级行为——降级仍能用。

### D10 — 按既有纪律定（未逐条询问，已告知用户）

- **跨机器恢复串行拨号**：并发会同时弹多个密码框 / 主机指纹确认。F153 已有的
  串行队列是**标签**粒度，这里要的是（标签, 叶子）粒度——是把它的机制**扩展**
  到叶子级，不是原样复用；实现 plan 里要明写队列结构怎么改。
- **跨机器复用「换节点」路径**：`rehost_pane`（`app.rs:8988`）已能把新连接 push 进
  `ws.hosts`，不新写第二条拨号链路（第二条一定会漏掉 `pending_restore` 那道防连点的闸）。
- **落盘频率不额外设防**：`parse_title` 只取 `#S` 会话名，用户在 tmux 里切 window
  （`#I`/`#W` 变）不会引起变化，脏比对不会被打爆。

---

## 3. 功能编号

| 编号 | 内容 |
|---|---|
| **F160** | 叶子级身份落盘：`SavedNodeEntry` 的叶子带 `session_id` + `tmux`（实测会话名），恢复时按叶子读回 |
| **F161** | 按实测名 attach：新命令串（只 attach、不建）+ D5 的 `-d` 规则 + D7 的跳过规则；**恢复与 F128 断线重连共用** |
| **F162** | 跨机器恢复：一个标签内多台机器的串行拨号 + D6 的 pane 级失败降级 + D3 的无身份 pane 空态 |
| **F163** | attach 结果校验：比对 `PaneState.tmux`，不符时在该 pane 上挂提示 |

---

## 4. 数据格式

`SavedNodeEntry`（`store/layout.rs:64`）的叶子加两个字段。**分割节点上恒为 `None`**：

```toml
[[tab]]
kind = "terminal"
session_id = 3        # 保留：兼容旧 exe + 作为叶子缺字段时的回落
title = "prod"
focus_leaf = 1

  [[tab.tree]]        # 分割节点：dir/ratio 有值，身份字段无
  dir = "horizontal"
  ratio = 0.4

  [[tab.tree]]        # 叶子：连 3 号会话，当初在 tmux 会话 web01 里
  session_id = 3
  tmux = "web01"

  [[tab.tree]]        # 叶子：换过节点，连的是 7 号会话；不在 tmux 里
  session_id = 7
```

约束：
- 两个字段都是 `Option` + `#[serde(default)]` + `skip_serializing_if`。
  `SavedNodeEntry` 内全是标量，TOML 的「值在表之前」规则对同级标量无影响；
  `tab.tree` 作为表数组仍必须是 `SavedTab` 的最后一个字段（既有约束不变）。
- 叶子的 `session_id` 缺失 → 回落到 `SavedTab.session_id`（旧 exe 写的记录走这条）。
  两者都缺 → D3 的空态 pane。
- `tmux` 缺失 → 该 pane 不发 attach，走 D7 的「跑配置计划」分支。

---

## 5. 执行流

### 5.1 落盘侧

现有的布局快照构造（`shell/layout_snapshot.rs::to_entries`）只吃 `&Node`，
拿不到 `PaneState`。要把 `Workspace` 的 `panes` / `hosts` 一起传进去，按叶子的
`PaneId` 查出 `(hosts[p.host_ix].session_id, p.tmux.clone())`。

**依赖方向不变**：`layout_snapshot` 已在 app 侧，`mullion-store` 仍不认识
`mullion-core`（架构不变量，`store/layout.rs` 开篇那条）。

### 5.2 恢复侧

```
用户在恢复列表点一条 / F153 自动串行拨号
  ↓
对每个标签，取「主叶子」= 前序第一个有 session_id 的叶子
  ↓
spawn_connect(主叶子的会话)  ← 今天这条路，零改动
  ↓ ConnectOk
建标签 → apply_saved_tree 摆回形状 → 得到 fresh 叶子列表
  ↓
对每个叶子按前序：
  ├ session_id == 主叶子的 → 在 hosts[0] 上开 channel（今天的 spawn_fresh_panes）
  ├ session_id 不同        → 排进串行拨号队列，走 rehost_pane 那条路
  └ session_id 缺失        → D3 空态 pane，不拨号
  ↓ 每块 pane 的 on_pane_ready（app.rs:5192，三条建立路径已收口于此）
  ├ 该叶子有 tmux 名 → 发 attach（D5 决定带不带 -d），跳过配置计划（D7）
  └ 无                → 跑配置计划（今天的行为）
  ↓ 发完 attach 的那些 pane
F163 校验：等待窗口内比对 PaneState.tmux，不符则挂提示（D4）
```

`on_pane_ready` 是唯一的收口点，已有守护测试
`every_pane_ready_path_goes_through_on_pane_ready`（F156-c 建的，
要求 `self.start_automation(` 全文件只有 1 个调用点）——新逻辑必须挂在它里面，
不能另开分支。

两个不改就必错的接口细节：

**① 已连 pane 的落位。** `apply_saved_tree`（`workspace/mod.rs:340`）把已有
pane 恒填**第一个叶子位**（`ids = keep + fresh`，前序映射）。而主叶子是
「前序第一个**有 session_id** 的叶子」——若叶子 0 是 D3 的无身份占位，
已连的 pane 会被摆到叶子 0 上，恰好占了本该空着的那格，主叶子反而空着。
`apply_saved_tree` 要加一个参数：已连 pane 落在第几个叶子（把 `ids` 里
kept 那一项挪到对应下标），旧调用点传 0 行为不变。

**② 恢复途中的落盘不能冲掉身份。** `save_layout_if_changed` 每 2 秒从
**运行时状态**现算快照。串行拨号进行中，未连上的叶子还没有真的
`HostConn`/实测 tmux 名——快照若按「查 `hosts[host_ix]`」现算，写出的叶子
就没有 `session_id`/`tmux`；半路 kill 掉 exe，这条记录里未恢复的身份
**永久丢失**。所以 D3/D6/排队三种 pane 状态必须**自带身份**（目标
`session_id` + 当初的 tmux 名），快照优先读它，连上之后才切回实测值。

### 5.3 F128 重连侧

`TmuxAttach`（`app.rs:366`）今天记的是「哪块 pane / 哪台 host / 哪个会话名」，
但 `session_name` 来自 `tmux_attach_for_connect`（`app.rs:398`）→
`tmux_session_name(配置)`，且 `pane` 硬编 `PaneId(1)`、`host_ix` 硬编 0。

改动：真值源换成实测（每块 pane 各有各的），并让 `reattach_pane`（`app.rs:9073`）
按该 pane 自己的实测名 attach。这一改**同时**修好 1.3 里那条既有 bug。

---

## 6. 守护测试清单

纯逻辑（可单测，每条要配一个「自证会变红」的变异）：

| 测试 | 扎住什么 | 变异 |
|---|---|---|
| `a_leaf_carries_the_host_it_was_actually_on_not_the_tab_default` | F160 落盘按叶子取 `hosts[host_ix].session_id` | 改成读 `hosts[0]` |
| `an_old_record_without_leaf_fields_falls_back_to_the_tab_session` | D9 降级兼容 | 删掉回落分支 |
| `the_attach_command_never_creates_a_session` | D2 红线 | 命令串里加回 `|| exec tmux new-session` |
| `a_failed_attach_leaves_the_shell_alive` | D2 的 `has-session` 守门（删掉它 = 失败即杀 pane） | 把守门段删掉、直接 `exec tmux attach` |
| `the_session_name_is_shell_quoted` | 复用 `shell_quote` | 换成裸拼接（会话名含 `'`/`$()` 时） |
| `the_detach_flag_is_keyed_per_host_and_session_name` | D5：键是（机器, 名）二元组——两台机器上的同名会话都要带 `-d` | 键退化成只按名字 |
| `only_the_first_pane_on_the_same_host_session_gets_the_detach_flag` | D5 同机同名首块规则 | 全加 `-d` / 全不加，两个方向各扎一条 |
| `a_pane_with_a_measured_tmux_name_skips_the_configured_plan` | D7 | 让两者叠加 |
| `a_pane_without_a_measured_name_still_runs_the_configured_plan` | D7 的另一半（防止把今天的行为一起删掉） | 改成恒跳过 |
| `a_failed_attach_does_not_replay_the_configured_plan` | D8 | 加补跑分支 |
| `a_leaf_without_any_session_id_is_kept_as_a_placeholder_not_dropped` | D3 | 改成丢弃叶子 |
| `restoring_a_two_host_tab_dials_serially` | D10 串行 | 改成并发 |
| `one_unreachable_host_only_disconnects_its_own_pane` | D6 | 改成整标签退回占位 |
| `the_connected_pane_lands_on_the_main_leaf_not_the_first_leaf` | 5.2①：叶子 0 是无身份占位时已连 pane 不能占错格 | 恒传 0 |
| `a_snapshot_taken_mid_restore_keeps_the_pending_leaf_identities` | 5.2②：恢复途中落盘不冲掉未连叶子的身份 | 快照改成只查 `hosts[host_ix]` |

接线守护（本项目反复踩过「纯函数写对了没接线」）：

- `the_leaf_identity_actually_reaches_the_snapshot`——`to_entries` 的调用点真的传了
  `panes`/`hosts`，不是传了个空表
- `every_attach_goes_through_on_pane_ready`——复用 F156-c 已有的那条，确认新逻辑
  没另开调用点
- `the_reattach_path_reads_the_measured_name_not_the_configured_one`——1.3 那条既有
  bug 的回归守护

**恒绿风险提示**（见 `subagent-driven-review-lessons` 记忆）：
`tmux` 字段是 `Option<String>`，「回落」与「跳过」两条分支在 `None` 时行为可能
碰巧相同——写测试时两条必须各喂一种输入，只喂 `None` 会让两道防御互相掩护。

---

## 7. 我验证不了的（人工验收清单）

- **attach 字节的发送时机**：pane 建立 → 首字节 → 发 attach，中间那段裸 shell 会不会
  被用户抢先输入。`Outcome::Aborted` 会中止（行为正确），但手感要实测。
- **F163 的等待时长**：几秒算「没接上」。太短会误报，太长提示来得毫无意义。真机调。
- **跨机器串行恢复的实际体验**：连续几次密码框 / 指纹确认，是否难以忍受。
- **D7 对已配 tmux 的会话是否也合适**：用户当前所有会话都没配，但规则要对两类都成立。
- **同名镜像时的 `window-size` 行为**（D5 的已接受代价）：两块尺寸不同的 pane
  attach 同一会话，实际是留白还是反复 reflow。
- 一如既往：是否不闪 / 字形对齐 / 输入法。

---

## 8. 明确不做

- **生命周期事件流 / 用量时长统计**（本轮调查的原始问法）：用户已明确重点不在那儿。
  若将来要做，缺口清单在上一轮调查里：`mullion.log` 多实例共用一个文件且行内无 pid、
  开关 pane 与断连全程无日志、退出时不逐个记时刻。
- **tmux 内部状态**（window 序号、tmux 自己的分屏、当前 pane）：attach 之后由 tmux
  自己还原，客户端不管。
- **scrollback 恢复**、**SFTP 标签的远端目录**（F120 明确「不记忆上次打开的目录」）。
- **启动即自动摆回**：F37 那部分已被 F148 取代，摆什么由用户在恢复列表里选。本设计
  不动这个入口。
