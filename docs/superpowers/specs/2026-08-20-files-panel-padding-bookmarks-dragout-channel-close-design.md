# 设计：文件面板内边距 + 拖出判据修复 + 路径收藏 + channel 收口

日期：2026-08-20
编号：**F138**（面板内边距）、**F59 修复**（拖出交接判据，不另给编号）、**F139**（路径收藏与书签下拉）、**F140**（SSH channel 显式收口）
基线：v0.1.54（`234bda2`）

四项一起做、发一个版。前三项是 UI，第四项是协议层缺陷；四者互不依赖，可并行实现、独立测试。

---

## 背景

用户在 v0.1.54 实机使用中提了四条，其中两条经排查是**功能从未真正生效**，而非观感问题：

| 报告 | 排查结论 |
|---|---|
| 路径条「↑」紧贴面板左边缘 | `files_panel::sidebar()` 的 `Frame::none()` 没有 `inner_margin`，整栏内容全部贴边 |
| 拖文件到 Windows 资源管理器没反应 | F59 的交接判据是「指针出了窗口」；用户窗口**一直最大化**，该条件永远不成立，`start_drag_out` 一次都没被调用过 |
| 想要路径收藏按钮 | 现有 F120 书签只能在会话编辑页里配，浏览时无法收藏；且书签栏只在已配过书签时才出现 |
| 关分屏是否给服务器留垃圾 | 留。关分屏不发 `SSH_MSG_CHANNEL_CLOSE`，远端 shell 不退、channel slot 泄漏 |

---

## ① F138 — 文件面板统一内边距

### 现状与根因

`files_panel.rs:1224` 的 `sidebar()` 用

```rust
egui::Frame::none()
    .fill(theme::c32(t.panel_bg))
    .stroke(theme::stroke(t))
```

没有 `inner_margin`，于是路径条、列头、行体全部从面板外框的第 0 点开始画。标签宿主 `content()`（1318 行）同样。

### 方案

两个宿主各加 `inner_margin(egui::Margin::symmetric(SP_S, SP_XS))` —— 左右 8pt、上下 4pt，取自 `ui/metrics.rs` 既有的间距五档，不引入裸数字。

左右比上下宽：横向是「内容与边框」的关系，纵向是「面板与相邻面板」的关系，后者本就有分隔线和外部间距垫着。

### 连带影响（必须回归）

F136 的横向滚动阈值、F135 的列宽算术都基于 `ui.available_width()`。加 margin 后可用宽度少 16pt，滚动条出现得更早——这是 egui 自动跟随的，但 F135/F136/F137 那批测试里凡是按具体宽度断言的，都要重跑确认没红。

### 守护测试

不能拿常量断言常量（本项目已记录的第三类恒绿模式）。判据取真值：跑一帧，取「↑」按钮的 `Response::rect` 与面板外框 `rect`，断言

```
btn.rect.left() >= panel.rect.left() + SP_S
```

把 `inner_margin` 那行删掉，这条必须变红。

---

## ② F59 修复 — 交接判据改成「离开文件面板矩形」

### 现状与根因

`dragout::should_hand_off` 的判据：

```rust
from == Some(PanelColumn::Remote) && !pointer_inside_window && !already_running
```

`pointer_inside_window` 由 `ui/mod.rs:614` 的 `pointer_inside_window()` 算，用 `ctx.screen_rect()`。窗口最大化时 screen_rect 就是整个屏幕，指针无处可去，条件恒 false。

这同时解释了三件事：为什么用户「完全无提示」（`start_drag_out` 的每条失败路径都会 `set_error`，无提示 = 根本没被调到）；为什么 F58 拖到本地栏照常能用（那条路径不看这个判据）；为什么 v0.1.37 当初能验收通过（多半是在非最大化窗口下试的）。

### 方案

判据的第二个参数从 `pointer_inside_window` 换成 `pointer_inside_panel`：

```rust
pub fn should_hand_off(
    from: Option<PanelColumn>,
    pointer_inside_panel: bool,
    already_running: bool,
) -> bool {
    from == Some(PanelColumn::Remote) && !pointer_inside_panel && !already_running
}
```

面板矩形由 `sidebar()` / `content()` 把宿主的整块 `rect` 回传给 `build_ui`（两种宿主各回各的，不是单栏矩形）。`latest_pos()` 为 `None` 时算「已离开」——旧行为的超集，指针真出窗口那条老路径继续成立。

`should_hand_off` 保持零 egui 依赖的纯函数，判据本身仍可纯单测。

### 为什么不破 F58

远端栏与本地栏都在宿主矩形**内**，远端→本地的下载手势全程 `pointer_inside_panel == true`，不会误触发 OLE 交接。

### 操作方式的变化（需人工验收）

改完后手势是：从远端栏拖到终端区 → OLE 拖拽起来（光标变文件图标）→ 按 Windows 惯例拖到任务栏图标悬停切窗口 → 放进目标目录。COM 那一层无头环境验不了，进人工清单。

### 守护测试

- `a_drag_that_is_still_inside_the_panel_is_not_handed_to_the_os`（改写自旧的 `..._inside_the_window_...`）
- `a_drag_that_left_the_panel_but_not_the_window_is_handed_to_the_os` —— **这条正是本次的真 bug**，把判据改回旧的它就变红

顺带补 `mullion::sftp::drag_out` target 上的交接日志（payload 有无 / 指针坐标 / 面板矩形 / 结论），下次再出问题不用靠推理。

---

## ③ F139 — 路径收藏与书签下拉

### 交互

路径条右端（「⟳」之后）加两个 `small_button`：

| 按钮 | 行为 |
|---|---|
| **☆ / ★** | 当前 `cwd` 与某条 `Bookmark.path` 相等 → 显示实心 ★，点击 = 取消收藏；否则空心 ☆，点击 = 收藏 |
| **▾** | 弹菜单列出全部书签，点一条发 `FileAction::Goto`；无书签时置灰，悬停「还没有收藏任何路径」 |

新收藏的书签名默认取路径末段（`/var/log` → `log`，根目录 → `/`）。菜单里空名的书签回退显示完整路径，与 F120 现有约定一致。

置灰条件之二：标签的 `session_id` 为 `None`（命令行直连、没有会话记录可写）时 ☆ 置灰，悬停「此连接不来自已保存的会话，无处存书签」。

### 去掉现有书签栏

`files_panel.rs:534-556` 那条 `if !bookmarks.is_empty()` 的横排书签栏删除，书签全部走 ▾ 下拉。理由：它只在已配过书签时才出现，用户根本不知道它存在；且占一整行高度。相关的三条测试改写到下拉上。

### 数据

书签仍存 `SessionRecord.sftp.bookmarks`（`mullion-store/src/sftp.rs` 的 `SftpPrefs`），**与会话管理器编辑页共用同一份**，无 schema 改动。

新增两条动作：

```rust
FileAction::BookmarkAdd { path: String, name: String }
FileAction::BookmarkRemove { path: String }
```

`app.rs` 侧改 `Vault` 里对应会话的 `sftp.bookmarks` 并存盘——**必须同时打 `touched_store` 标记**（切片 I 踩过一次：只改内存不打标记，重启后收藏消失）。

### 范围

只给远端栏。本地栏继续收 `&[]` 且 `session_id` 传 `None`，两个按钮都不画——本地目录收藏不在本轮范围。

### 守护测试

- `clicking_the_star_records_the_current_directory_as_a_bookmark`
- `clicking_a_filled_star_removes_that_bookmark`（★/☆ 状态由 `cwd` 与书签列表比对得出，不另存标志位）
- `the_star_is_disabled_when_the_tab_has_no_session_id`
- `picking_a_bookmark_from_the_dropdown_emits_goto`
- app 侧：`bookmark_add_marks_the_store_dirty`（变异掉 `touched_store` 那行必须变红）

---

## ④ F140 — SSH channel 显式收口

### 现状与根因

证据链完整：

1. `Workspace::close_pane`（`shell/workspace/mod.rs:342`）从 `self.panes` 里 `retain` 掉 `PaneState`
2. `PaneState` drop → 它持有的 `SshSession` drop → `cmd_tx` drop
3. `io_task` 里 `cmd_rx.recv()` 返回 `None`，走这条分支：

```rust
None => {
    let _ = write.eof().await;
    break; // 所有句柄已 drop
}
```

4. **只发 `SSH_MSG_CHANNEL_EOF`，不发 `SSH_MSG_CHANNEL_CLOSE`**。`break` 之后 `ChannelWriteHalf` / `ChannelReadHalf` 被 drop——而 russh 0.54.5 对这两个类型**没有 `Drop` 实现**（已在 registry 源码里核过，全仓仅 4 处 `impl Drop`，均不适用；只有 `into_stream()` 的 `ChannelCloseOnDrop` 会发 CLOSE，我们没走那条路）

后果两条：远端 shell 收不到挂断、一直挂着；channel slot 在 sshd 侧不释放，累积到 `MaxSessions`（默认 10）后同一条连接再也开不出新分屏——正是 adr-009 已经列出的失效模式。

`Workspace::close_pane` 的注释「`PaneState` 一并丢弃（channel 随之关闭）」与 `wind_down` 的注释「`t.ws` 在这里 drop —— 每个 `PaneState` 随之 drop，关掉它那条 SSH channel」都是**错的**，一并修正。

### 方案

`mullion-ssh/src/session.rs`：

```rust
enum SshCmd { Write(Vec<u8>), Resize(u16, u16), Close }

impl SshSession {
    /// 显式收口：让 `io_task` 发出 `SSH_MSG_CHANNEL_CLOSE` 后退出。
    /// 非 async —— 调用点全在 UI 同步路径上；用 `try_send`，
    /// 队列满或 io_task 已死都当作"已经在收口了"，忽略。
    pub fn close(&self) { let _ = self.cmd_tx.try_send(SshCmd::Close); }
}
```

`io_task`：

```rust
Some(SshCmd::Close) => {
    let _ = write.eof().await;
    let _ = write.close().await;
    break;
}
None => {
    // 双保险：句柄全 drop 而没人调过 close() 时也要收口
    let _ = write.eof().await;
    let _ = write.close().await;
    break;
}
```

`None` 分支同样补 `close()`：`Arc<SshSession>` 可能被 automation task 多处持有，显式 `close()` 走不到的路径由它兜底。

`PtyWriter` trait（`shell/workspace/mod.rs:78`）加 `fn close(&self)`，两个 impl 转发，`FakePty` 计数。

### 三条调用路径

| 路径 | 位置 | 时机 |
|---|---|---|
| 关分屏 | `Workspace::close_pane` | `panes.retain` **之前**，对被关的 pane 调 |
| 换节点 | `app.rs:7887 rehost_pane` / `7953 reattach_pane` | 换掉 `pty` **之前**，对旧的调 |
| 关标签 | `app.rs:1274 wind_down` 的 `Terminal` 分支 | `t.ws` drop 之前，对**每块 pane** 调 |

关标签那条要在 `Workspace` 上新增 `close_all_panes(&self)`，遍历调 `close()`——不能只关焦点那块。

### tmux 语义（用户已确认的硬约束）

CHANNEL_CLOSE → sshd 关 pty → pty 上的前台进程组收 SIGHUP。分屏里跑的是 tmux client 时，client 收 SIGHUP 退出 = **detach**；tmux server 与 session 是独立 daemon，完好保留，重连 `tmux attach` 一切照旧。这正是"关分屏不能影响 tmux"要的语义。

副作用（用户已明确接受）：分屏里跑的不是 tmux 而是裸前台命令时，该命令会被 SIGHUP 杀掉——与关掉 PuTTY 窗口的行为一致，是标准 SSH 语义。

### 守护测试

- `closing_a_pane_closes_its_channel` / `rehosting_a_pane_closes_the_old_channel` / `winding_down_a_tab_closes_every_pane_channel` —— 用 `FakePty` 的调用计数
- `close_sends_a_close_command_down_the_channel` —— `SshSession::close()` 确实把 `SshCmd::Close` 送进队列

**测不到的那一层**：`write.close()` 是否真的发出 `SSH_MSG_CHANNEL_CLOSE` 报文，无头环境验不了（要假 sshd 才测得到）。进人工验收清单，验法是：开几个分屏再关掉，在远端 `ps -ef | grep sshd` 看子进程有没有回收、`who` 看登录记录有没有减少。

---

## 交付

一并发一个 patch 版本（v0.1.55），走 `CLAUDE.md` 的交付约定一条龙：bump → 全绿（`cargo test --workspace` + `clippy -D warnings` + `fmt --check`）→ 交叉编译 + objdump 依赖验收 → **签名** → 发 Release（标题纯 `v0.1.55`）→ 报链接 + sha256 + 人工清单。

### 人工验收清单（无头验不了的）

1. 文件面板左右两侧有可见留白，「↑」不再贴边；拖列宽、横向滚动照常
2. **窗口最大化状态下**从远端栏拖文件到终端区，光标变成拖拽图标；拖到任务栏资源管理器图标上悬停切窗口，放进目录能落文件
3. 远端栏 ☆ 收藏当前目录 → 变 ★ → ▾ 里能看到并跳回去；重启客户端后收藏还在
4. 开 3~4 个分屏后逐个关掉，远端 `ps -ef | grep sshd` 看子进程数回落；tmux 里的会话 `tmux ls` 仍在、`attach` 回去内容完好
