# 切片 D0：标签页最小版 实施计划（F36）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `App` 从「一个 workspace」变成「一列标签」。能开多个标签、切换不断连接、关标签干净收口。**发版**，实机验「两个会话各占一个标签，来回切，tmux 里的 Claude Code 不掉线、不重排」。

**Architecture:** 标签是 `mullion-app` 内部概念，`mullion-core` / `term` / `ssh` / `store` **零改动**。纯逻辑（活动下标、关闭后焦点转移、快捷键映射、按世代号定位标签）落在新文件 `shell/tabs.rs`，可脱离 GUI 纯测；`app.rs` 只做接线；标签栏是 `ui/chrome.rs` 里再加一个 `TopBottomPanel`。

**Spec:** `docs/superpowers/specs/2026-08-12-sftp-browser-design.md`（D3；D2 的中央区收缩链路）

**本切片不做**：任何 SFTP 代码（`TabContent` 只有终端一种变体，SFTP 变体归 D1）、拖拽重排、重启恢复（F37）、分离窗口、标签右键菜单。

---

## 本切片新定死的四条设计决策

**S1 —— 世代号 `generation` 升级为「标签路由键」。**
现在 `PaneOpened` / 自动化结论 / `ConnectErr` 三条迟到事件都靠 `generation_matches(ws, gen)`
判「还是不是当前那个 workspace」。多标签下这个判据**必须**从「等于唯一 ws 的世代」改成
「在全部标签里找世代号等于它的那个」——`next_ws_generation` 全局单调递增，世代号天然全局
唯一，正好当路由键用，不需要再引一个 TabId 进事件负载。
反过来说：**任何迟到事件都不许用「活动标签」去接**。用户在标签 A 发起连接、切到标签 B 的
几百毫秒里 `PaneOpened` 抵达，用活动标签接就会把 A 的 pane 挂到 B 上（adr-009 记的四条失效
模式之一，本切片把它从「一个 ws 内」放大到「跨标签」）。

**S2 —— per-connection 状态整体搬进 `Tab`。**
`ws` / `current_preset` / `last_cfg` / `automation` / `automation_status` 五项都是「属于这条
连接」的，留在 `App` 上就会串味：在标签 A 跑着自动化，去标签 B 连一个新会话，`ConnectOk` 里
那句 `self.automation.take().abort()` 会把 A 的自动化掐了。
留在 `App` 的是「在途/全局」状态：`pending_automation`、`pending_skip_automation`（一次只连
一个）、`tunnels`（隧道独占连接，与标签无关，adr-010）、`known_hosts`、`store`、`appearance`。

**S3 —— 标签栏是第三个 `TopBottomPanel`，不改中央区取值方式。**
`ui/mod.rs:413` 那句「必须在菜单栏和状态栏都 show 完之后取 `available_rect`」的纪律不变，
只是多一个面板参与收缩。标签栏出现/消失 → 中央区高度变 → 下一帧 `apply_geometry` 算出新
grid → 发 `window_change`（T4）。**不新开任何一条尺寸传播路径**。
只有一个标签时**仍然显示**标签栏（不做「单标签自动隐藏」）：自动隐藏会让「开第二个标签」
这一下发生一次高度跳变 + 全终端重排，远端 TUI 会闪一下——正是本项目存在的理由要避免的。

**S4 —— 标签快捷键在键盘路由里**先判后喂**，且不转发给远端。**
`Ctrl+Tab` / `Ctrl+Shift+Tab` / `Ctrl+W` / `Ctrl+1..9` 在终端态被我们吃掉，不编码进 PTY。
按 T8 的纪律：判给标签动作的键**绝不先喂 `egui_state.on_window_event`**，否则 egui 焦点系统
吞掉 Tab、`wants_keyboard_input()` 恒真，终端永久收不到键。
选这四组是因为它们在终端里不产生控制字符（`Ctrl+W` 例外——它是 `^W`，bash 的删词；**这一条
要在 Release notes 的人工验收里点名**，若你实测觉得夺键太狠，改成 `Ctrl+Shift+W`）。

---

## 文件结构

| 文件 | 本次的职责 | 改动性质 |
|---|---|---|
| `crates/mullion-app/src/shell/tabs.rs` | `TabId` / `Tab` / `Tabs` 容器 + 关闭后焦点转移 + 世代路由 + 快捷键映射，全纯函数 | **新建** |
| `crates/mullion-app/src/shell/mod.rs` | `pub mod tabs` | 一行 |
| `crates/mullion-app/src/app.rs` | 34 处 `self.ws` → `self.tabs.active_ws()`；世代路由改查全表；连接/断开语义 | 主战场 |
| `crates/mullion-app/src/ui/chrome.rs` | `tab_bar()`：第三个 `TopBottomPanel` | 加函数 |
| `crates/mullion-app/src/ui/mod.rs` | `UiState` 加标签意图（切换/关闭/新建）；`build_ui` 调 `tab_bar` | 加字段 |
| `crates/mullion-app/src/input.rs` / `shell/input_route.rs` | 标签快捷键的判定（S4） | 加分支 |
| `crates/mullion-app/src/ui/annotate.rs` | 标签栏与每个标签登记进 F100 | 加登记 |

任务顺序由编译约束决定：Task 1 的类型是其余全部的前提；Task 2 做完必须**行为等价**（单标签
时与今天逐帧一致）才能往下走。

---

### Task 1: `shell/tabs.rs` —— 纯逻辑地基

- [ ] 定义类型：
  ```rust
  pub struct TabId(pub u32);
  pub enum TabContent { Terminal(Workspace) }   // D1 会加 Files 变体
  pub struct Tab {
      pub id: TabId,
      pub title: String,                 // 会话名，无会话时 "user@host"
      pub color: Option<egui::Color32>,  // F62 节点色
      pub icon: Option<...>,             // F61，复用 appearance 缓存的解析结果
      pub content: TabContent,
      pub current_preset: Option<Preset>,
      pub last_cfg: Option<SshConfig>,
      pub automation: Option<AutomationHandle>,
      pub automation_status: Option<String>,
  }
  pub struct Tabs { tabs: Vec<Tab>, active: usize, next_id: u32 }
  ```
  `Tabs` 空 = launcher 态（取代今天的 `ws: None`）。
- [ ] `push(tab) -> TabId`（新标签成为活动标签）、`active()` / `active_mut()`、
      `active_ws()` / `active_ws_mut()`（`Option<&Workspace>`，让 Task 2 的替换是机械的）。
- [ ] `close(ix) -> Option<Tab>`：**焦点转移规则** —— 关的不是活动标签则活动下标只做位移修正；
      关的是活动标签则取**右邻**，没有右邻取左邻；关空则回 launcher 态。返回被关的 `Tab`
      交给调用方收口（Task 6）。
- [ ] `by_generation_mut(gen) -> Option<&mut Tab>`（S1 路由键）。
- [ ] `switch_next()` / `switch_prev()`（环绕）、`switch_to(n)`（`Ctrl+1..9`，`n` 越界即 no-op；
      **`Ctrl+9` = 最后一个标签**，与浏览器一致，不是"第 9 个"）。
- [ ] 测试（纯，不需要 GPU）：
  - `close_active_moves_focus_to_the_right_neighbour`
  - `close_active_falls_back_to_left_when_it_was_last`
  - `close_non_active_keeps_the_same_tab_focused`（关左边的标签后，活动的还是同一个 `TabId`，
    不是同一个下标——这条能扎住"只改下标忘了修正"的 bug）
  - `close_last_returns_to_launcher`
  - `switch_to_9_means_last_tab`
  - `by_generation_finds_the_owner_tab_not_the_active_one`（S1 的核心：造两个标签，
    用非活动标签的世代号查，必须查到它自己）

### Task 2: `App.ws` → `App.tabs`，**行为等价**的机械迁移

- [ ] `App` 删 `ws` / `current_preset` / `last_cfg` / `automation` / `automation_status` 五个字段，
      加 `tabs: Tabs`（S2）。
- [ ] 34 处 `self.ws` 逐处改写。绝大多数是 `self.ws.as_ref()` → `self.tabs.active_ws()`。
      **逐处确认语义**：读活动标签的用 `active_*`；三条迟到事件（`PaneOpened` / 自动化结论 /
      `ConnectErr`）改用 `by_generation_mut`（S1）。
- [ ] `generation_matches(ws, gen)` 这个自由函数**删掉**，由 `by_generation_mut` 取代——留着它
      就会有人继续拿活动 ws 去比。
- [ ] 本任务**不加任何新功能**：连接仍替换活动标签（等价于今天）、无标签栏、无快捷键。
- [ ] 验收：`cargo test -p mullion-app` 全绿；**T1/T3/T4/T7/T8 五条守护测试逐条复跑并在提交
      信息里点名**（`emulator::tests::pty_write_is_collected`、`app::tests::redraw_is_frame_capped`、
      `app::tests::reflow_emits_resize`、`frame::tests`、
      `input_route::tests::terminal_keyboard_is_never_fed_to_egui_so_tab_cannot_steal_focus`）。
- [ ] **单独一个 commit**，信息写明「纯重构，行为等价」。后面出问题时这是二分的锚点。

### Task 3: 标签栏 UI

- [ ] `chrome::tab_bar(ctx, tabs, &mut UiState) `：`TopBottomPanel::top("tabs")`，**放在
      `menu` 之后、`status` 之前 show**（S3）。
- [ ] 每个标签：图标（F61，32px 那套的小号）+ 标题 + 关闭 ×；活动标签用节点色（F62）做底/下划线，
      非活动用主题的次级色。色板一律取 `theme.rs`，**不新引任何色值**（F80 纪律）。
- [ ] 标题过长截断 + tooltip 显示全名；标签数多到排不下时**横向滚动**（`ScrollArea::horizontal`），
      不做溢出下拉菜单。
- [ ] 右侧一个 `+` 按钮 = 打开会话管理器（不是"新建空标签"——空标签在本项目里没有意义，
      没连接的标签什么都画不了）。
- [ ] F100 登记：标签栏容器、每个标签、关闭按钮、`+` 按钮。
- [ ] 测试（走既有的 egui 离屏 harness）：
  - `tab_bar_is_shown_even_with_a_single_tab`（S3）
  - `active_tab_uses_the_session_color`
  - `tab_bar_shrinks_the_central_rect`（标签栏出现后 `central_px` 变小——这是 T4 链路的入口）

### Task 4: 快捷键与路由（S4）

- [ ] 在键盘路由的**判定**阶段识别 `Ctrl+Tab` / `Ctrl+Shift+Tab` / `Ctrl+W` / `Ctrl+1..9`，
      命中则记进 `UiState` 的标签意图并**吞掉**（既不喂 egui、也不编码进 PTY）。
- [ ] 弹窗/会话管理器打开时（`modal` 为真）这些键**不生效**——此时键盘归 egui（T8）。
- [ ] 测试：
  - `tab_shortcuts_are_swallowed_and_never_encoded_to_pty`（喂 `Ctrl+Tab`，断言 PTY 写入为空）
  - `tab_shortcuts_do_not_reach_egui`（T8 同构）
  - `tab_shortcuts_are_inert_while_a_modal_is_open`

### Task 5: 连接语义 —— 连接开新标签

- [ ] 会话管理器发起连接：**新开一个标签**；若当前是 launcher 态（`tabs` 为空）则开第一个。
- [ ] CLI 直连（`mullion user@host`）：开第一个标签，`cli_direct` 语义不变。
- [ ] 同一条会话可以开多个标签（不做去重）——标题相同时**不加序号**，靠节点色和 hover 的
      `user@host:port` 区分；序号会让"关掉中间那个"之后编号跳变，反而更乱。
- [ ] 菜单「断开」= 关闭**当前标签**（不是回 launcher）；最后一个标签关掉后自然回 launcher。
- [ ] 标签标题/颜色/图标从 `appearance` 缓存取（F61/F62），**不在渲染里现算**（T3）。
- [ ] 测试：`connecting_opens_a_new_tab_and_keeps_the_previous_one_live`。

### Task 6: 关标签的收口

- [ ] `Tabs::close` 返回的 `Tab` 必须显式收口，顺序：
      ① `automation.abort()`（今天 `request_disconnect` 那段的理由原样适用：自动化 task 也持有
      一份 `Arc<SshSession>`，只 drop workspace 收不了口）→ ② drop `TabContent`（drop 掉全部
      `PaneState` → 关掉每条 SSH channel）→ ③ 若这是最后一个标签，回 launcher 态。
- [ ] `request_quit`：逐个走同样的收口，不靠进程退出兜底。
- [ ] 测试：
  - `closing_a_tab_aborts_only_its_own_automation`（两个标签各跑一份，关 A 后 B 的还在）
  - `closing_a_tab_drops_its_panes`（用假 PTY 断言 writer 被释放）

### Task 7: 两条跨切片守护测试

- [ ] `switching_tabs_does_not_touch_the_ssh_connections`：造两个标签（假 sshd，各一条连接），
      来回切 10 次，断言底层连接数与 channel 数不变、无重新握手。**这是 F36 写进 spec 的验收标准**。
- [ ] `tab_bar_visibility_change_emits_window_change`：标签栏出现导致中央区变矮 → 下一帧
      `apply_geometry` 必须发 `window_change`（T4 链路，S3 的落点）。
- [ ] 两条都要**自证会变红**：前者把切换实现改成"重连"必须失败；后者把标签栏高度写成 0 必须失败。

### Task 8: 交付

- [ ] `cargo test --workspace` + `clippy --workspace --all-targets -- -D warnings` + `fmt --check` 全绿。
- [ ] 升 patch 版本（`workspace.package.version` 第三位 +1），单独一个 `chore:` 提交。
- [ ] 交叉编译 `x86_64-pc-windows-gnu`，按 `docs/cross-compile-windows.md` 做 objdump 依赖验收
      （出现 `libgcc_s_seh-1.dll` / `libwinpthread-1.dll` 即不合格）。
- [ ] 发 Release（标题纯版本号），notes 含下面的人工验收清单 + sha256 + `Unblock-File` 提示。

---

## 验收

- `cargo test --workspace` 全绿 + `clippy -D warnings` 无输出。
- Task 2 之后单标签行为与今天逐帧等价（五条领域陷阱测试全过）。
- Task 7 两条守护测试均已自证变红。

---

## 人工验收清单（无头环境验不了，写进 Release notes）

1. **切标签不断连**：开两个标签各连一台机器，各自 `tmux attach` 跑着 Claude Code；来回切 20 次，
   两边**都不重连、不重排、不闪**；切回去时滚动位置和光标位置都在原处。
2. **`Ctrl+W` 夺键是否可接受**：终端里 `^W`（bash 删词）现在被标签关闭吃掉了。你实测后若觉得
   太狠，我改成 `Ctrl+Shift+W`。
3. **标签栏视觉**：节点色/图标与会话列表是否同源；中文会话名截断与 tooltip 是否正常；
   开到 10+ 标签时横向滚动是否跟手。
4. **关标签收口**：关掉一个正在跑自动化的标签，另一个标签的自动化不受影响；关掉后远端
   `who` / `ps` 里对应的会话确实消失（不留半开连接）。
5. **最后一个标签关掉**：回到 launcher 态，界面正常，能再连。

---

## 移交给 D1 的接口约定

- `TabContent` 加 `Files(FilesTab)` 变体；`Tabs` 的焦点/关闭/快捷键逻辑**不改**。
- 标签标题对 SFTP 节点取节点名，颜色同样走 F62。
- 关闭 SFTP 标签的收口顺序与 Task 6 同构（先停传输、再 drop 连接）——D1 落地时补一条对称测试。
