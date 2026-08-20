# 文件面板内边距 + 拖出判据 + 路径收藏 + channel 收口 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修掉「关分屏不发 CHANNEL_CLOSE 给远端留垃圾」与「窗口最大化时拖出到资源管理器永远不触发」两个缺陷，并给文件面板加统一内边距、给路径条加收藏/书签下拉。

**Architecture:** 四项互不依赖。F140 在 `mullion-ssh` 加一条显式 `close` 命令、在 `mullion-app` 的 `PtyWriter` 上加一个 `close()` 并接到关分屏/换 channel/关标签三条路径；F138 给两个宿主 `Frame` 加 `inner_margin`；F59 把交接判据的第二个参数从「指针在窗口内」换成「指针在文件面板矩形内」，矩形取上一帧的值经 `UiState` 传递；F139 在路径条右端加 ☆/▾ 两个按钮、删掉旧的横排书签栏，收藏落到 `SessionRecord.sftp.bookmarks`。

**Tech Stack:** Rust 2021 / russh 0.54.5 / egui 0.30 / winit 0.30 / tokio。

**设计文档:** `docs/superpowers/specs/2026-08-20-files-panel-padding-bookmarks-dragout-channel-close-design.md`

**执行顺序:** Task 1 → 8。F140（Task 1-2）排在最前：它是协议层缺陷，与 UI 三项零耦合，先合掉可以独立验证。

---

## 文件清单

| 文件 | 责任 | 本次改动 |
|---|---|---|
| `crates/mullion-ssh/src/session.rs` | SSH channel 的 io 循环与句柄 | 新增 `SshCmd::Close` / `SshSession::close()`，`io_task` 两条退出路径都发 `close()` |
| `crates/mullion-app/src/shell/workspace/mod.rs` | 布局树 + pane 运行态 | `PtyWriter::close()`；`Workspace::close_pane` / 新增 `close_all_panes`；`FakePty` 计数 |
| `crates/mullion-app/src/app.rs` | 事件循环与接线 | `swap_pane_channel` 关旧 channel；`wind_down` 关全部 pane；F139 的动作落盘；`NullPty` 补 `close` |
| `crates/mullion-app/src/ui/files_panel.rs` | 文件面板绘制 | `inner_margin`；`BookmarkView` 参数；☆/▾ 按钮；删旧书签栏；两宿主回填面板矩形 |
| `crates/mullion-app/src/ui/mod.rs` | 帧装配与动作路由 | `UiState::files_panel_rect`；`pointer_inside_panel`；交接判据接线 |
| `crates/mullion-app/src/dragout/mod.rs` | 拖出判据（零平台代码） | `should_hand_off` 第二参改语义；两条新单测 |
| `spec.md` | 需求台账 | 新增 F138/F139/F140 三行，F59 行补记修复 |

---

### Task 1: F140-a — `mullion-ssh` 显式 channel 收口

**Files:**
- Modify: `crates/mullion-ssh/src/session.rs:408-411`（`SshCmd`）、`:430-448`（`SshSession`）、`:540-550`（`io_task` 的 `cmd` 分支）

**背景（实现者必读）：** russh 0.54.5 的 `ChannelReadHalf` / `ChannelWriteHalf` **没有 `Drop` 实现**，drop 它们不会发 `SSH_MSG_CHANNEL_CLOSE`。当前 `io_task` 在句柄全部 drop 时只发 `eof()`，远端 shell 因此收不到挂断。`ChannelWriteHalf::close()` 确实存在（`russh-0.54.5/src/channels/mod.rs:356`，在 190 行那个 impl 块内）。

- [ ] **Step 1: 写失败的测试**

追加到 `crates/mullion-ssh/src/session.rs` 的 `mod tests` 里：

```rust
    /// F140:`SshSession::close()` 必须把一条 `Close` 命令送进 io 队列 ——
    /// 那是 `io_task` 发出 `SSH_MSG_CHANNEL_CLOSE` 的唯一触发点。
    ///
    /// 为什么要有这条:关分屏时只 drop 句柄的话,`io_task` 走的是 `None`
    /// 分支;而 russh 0.54.5 对 `ChannelWriteHalf` 没有 `Drop` 实现,
    /// 远端 shell 一直挂着,channel slot 累积到 sshd 的 `MaxSessions`
    /// 之后同一条连接再也开不出新分屏。
    #[tokio::test]
    async fn close_sends_a_close_command_down_the_io_queue() {
        let (cmd_tx, mut cmd_rx) = mpsc::channel::<SshCmd>(4);
        let s = SshSession { cmd_tx };
        s.close();
        assert!(
            matches!(cmd_rx.recv().await, Some(SshCmd::Close)),
            "close() 没有把 Close 命令发出去 —— io_task 不会发 CHANNEL_CLOSE"
        );
    }
```

- [ ] **Step 2: 跑测试确认它红**

```bash
cargo test -p mullion-ssh close_sends_a_close_command 2>&1 | tail -20
```

预期：编译失败，`no variant named `Close`` / `no method named `close``。

- [ ] **Step 3: 实现**

`SshCmd` 加变体（`session.rs:408`）：

```rust
/// 交给 io task 的命令。
enum SshCmd {
    Write(Vec<u8>),
    Resize(u16, u16),
    /// F140:显式收口。让 io_task 发出 `SSH_MSG_CHANNEL_CLOSE` 之后退出 ——
    /// 光靠 drop 句柄不行,russh 0.54.5 的 `ChannelWriteHalf` 没有 `Drop`。
    Close,
}
```

`SshSession` 加方法（`session.rs:448`，`resize` 之后）：

```rust
    /// F140:显式关掉这条 channel。**非阻塞、非 async** —— 调用点全在 UI
    /// 同步路径上(关分屏 / 换节点 / 关标签)。
    ///
    /// 失败一律忽略:队列满或 io_task 已经退出,两种情况都意味着"这条
    /// channel 已经在收口或已经没了",没有可补救的动作,更不该让 UI 卡住。
    pub fn close(&self) {
        let _ = self.cmd_tx.try_send(SshCmd::Close);
    }
```

`io_task` 的 `cmd` 分支（`session.rs:540`）：

```rust
            cmd = cmd_rx.recv() => match cmd {
                Some(SshCmd::Write(b)) => {
                    let _ = write.data(&b[..]).await;
                }
                Some(SshCmd::Resize(c, r)) => {
                    let _ = write.window_change(c as u32, r as u32, 0, 0).await;
                }
                Some(SshCmd::Close) => {
                    // F140:先 eof 再 close —— 顺序照 RFC 4254 §5.3 的常规,
                    // 让远端知道"我不再发数据了"再拆 channel。
                    let _ = write.eof().await;
                    let _ = write.close().await;
                    break;
                }
                None => {
                    // 所有句柄已 drop。**这里也要 close**:`Arc<SshSession>`
                    // 可能被自动化 task 多处持有,显式 `close()` 走不到的
                    // 路径由这条兜底。russh 0.54.5 的 `ChannelWriteHalf`
                    // 没有 `Drop`,不发就是泄漏一个 channel slot。
                    let _ = write.eof().await;
                    let _ = write.close().await;
                    break;
                }
            },
```

- [ ] **Step 4: 跑测试确认绿**

```bash
cargo test -p mullion-ssh 2>&1 | tail -5
```

预期：`test result: ok`。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-ssh/src/session.rs
git commit -m "feat(ssh): channel 显式收口,关闭时发 CHANNEL_CLOSE (F140)"
```

---

### Task 2: F140-b — 三条路径都走显式 close

**Files:**
- Modify: `crates/mullion-app/src/shell/workspace/mod.rs:78-107`（trait 与两个 impl）、`:340-350`（`close_pane`）、`:516-533`（`FakePty`/`Probe`/`fake_pane`）
- Modify: `crates/mullion-app/src/app.rs:1274`（`wind_down`）、`:7922`（`swap_pane_channel`）、`NullPty`（app 测试模块内）

- [ ] **Step 1: 写失败的测试（workspace 侧）**

先给测试替身加计数能力。`FakePty` 加字段、`Probe` 加出口、`fake_pane` 接线（`shell/workspace/mod.rs` 的 `mod tests`）：

```rust
    #[derive(Default)]
    struct FakePty {
        writes: Arc<Mutex<Vec<Vec<u8>>>>,
        resizes: Arc<Mutex<Vec<(u16, u16)>>>,
        resize_fails: Arc<Mutex<bool>>,
        /// F140:`close()` 被调了几次。**计数而不是布尔** —— 重复关是无害的
        /// (io_task 已退出时 try_send 失败即忽略),但一次都没有就是泄漏。
        closes: Arc<Mutex<usize>>,
    }
```

`impl PtyWriter for FakePty` 里补：

```rust
        fn close(&self) {
            *self.closes.lock().unwrap() += 1;
        }
```

`Probe` 加字段 `pub closes: Arc<Mutex<usize>>,`，`fake_pane` 里 `let probe_closes = pty.closes.clone();` 并放进返回的 `Probe`。

然后写守护测试（同 `mod tests`）：

```rust
    /// F140:关分屏必须**显式**关掉那条 SSH channel。
    ///
    /// 光丢弃 `PaneState` 不够:那条路径最终走到 `io_task` 的 `None` 分支,
    /// 而 russh 0.54.5 的 `ChannelWriteHalf` 没有 `Drop` —— 远端 shell
    /// 一直挂着,channel slot 泄漏到 sshd 的 `MaxSessions` 之后同一条连接
    /// 再也开不出新分屏。
    ///
    /// 自证会变红:把 `close_pane` 里那句 `p.pty.close()` 删掉。
    #[test]
    fn closing_a_pane_closes_its_channel() {
        let (p1, _probe1) = fake_pane(1);
        let mut ws = Workspace::new(p1, 0);
        let new_id = ws.split_focused(Dir::Horizontal).expect("分不出第二块");
        let (mut p2, probe2) = fake_pane(new_id.0);
        p2.id = new_id;
        ws.attach_pane(p2);

        assert_eq!(*probe2.closes.lock().unwrap(), 0, "还没关就已经调过 close");
        assert!(ws.close_pane(new_id));
        assert_eq!(
            *probe2.closes.lock().unwrap(),
            1,
            "关分屏没有关掉它的 channel —— 远端 shell 会挂着不死(F140)"
        );
    }

    /// F140:关标签要关掉**每一块** pane 的 channel,不是只关焦点那块。
    ///
    /// 自证会变红:把 `close_all_panes` 的循环改成只关 `self.focus`。
    #[test]
    fn closing_every_pane_closes_every_channel() {
        let (p1, probe1) = fake_pane(1);
        let mut ws = Workspace::new(p1, 0);
        let new_id = ws.split_focused(Dir::Horizontal).expect("分不出第二块");
        let (mut p2, probe2) = fake_pane(new_id.0);
        p2.id = new_id;
        ws.attach_pane(p2);

        ws.close_all_panes();
        assert_eq!(*probe1.closes.lock().unwrap(), 1, "第一块 pane 的 channel 没关");
        assert_eq!(*probe2.closes.lock().unwrap(), 1, "第二块 pane 的 channel 没关");
    }
```

- [ ] **Step 2: 跑测试确认它红**

```bash
cargo test -p mullion-app closing_a_pane_closes_its_channel 2>&1 | tail -20
```

预期：编译失败，`no method named `close` found for ...`。

- [ ] **Step 3: 实现 trait 与两条路径**

`shell/workspace/mod.rs:78`：

```rust
pub trait PtyWriter: Send {
    fn write(&self, bytes: Vec<u8>) -> Result<(), TrySendErr>;
    fn resize(&self, cols: u16, rows: u16) -> Result<(), TrySendErr>;
    /// F140:显式关掉这条 channel(发 `SSH_MSG_CHANNEL_CLOSE`)。
    ///
    /// **没有默认实现**:漏实现就该编译不过。给个空默认的话,将来新加的
    /// 写口会静默地不收口,而这正是 F140 要修的那个 bug 的形状。
    ///
    /// 无返回值:调用点都在"反正要丢弃这个 pane 了"的路径上,失败没有
    /// 补救动作(见 `SshSession::close` 的文档)。
    fn close(&self);
}
```

两个 impl 各补一行转发：

```rust
impl PtyWriter for SshSession {
    // ... write / resize 不动
    fn close(&self) {
        SshSession::close(self);
    }
}

impl PtyWriter for Arc<SshSession> {
    // ... write / resize 不动
    fn close(&self) {
        SshSession::close(self);
    }
}
```

`Workspace::close_pane`（`:342`），注释里那句「channel 随之关闭」是错的，一并改：

```rust
    /// 关闭一个 pane(F31):树上兄弟顶替,`PaneState` 一并丢弃。
    /// 最后一个 pane 不可关,返回 `false` 且什么都不动。
    ///
    /// F140:**丢弃 `PaneState` 不等于关掉 channel**。russh 0.54.5 的
    /// `ChannelWriteHalf` 没有 `Drop`,不显式 `close()` 的话远端 shell
    /// 会一直挂着,channel slot 泄漏到 sshd 的 `MaxSessions` 上限。
    pub fn close_pane(&mut self, id: PaneId) -> bool {
        if !close_pane(&mut self.tree, id) {
            return false;
        }
        if let Some(p) = self.panes.iter().find(|p| p.id == id) {
            p.pty.close();
        }
        self.panes.retain(|p| p.id != id);
        self.focus = next_focus(self.focus, &leaves(&self.tree));
        true
    }

    /// F140:关掉**所有** pane 的 channel。关标签时用(见 `app.rs::wind_down`)——
    /// 那条路径不经过 `close_pane`,整个 `Workspace` 被直接 drop。
    ///
    /// 不改任何状态:调用方紧接着就要丢弃整个 `Workspace`,清空 `panes`
    /// 只是多一步没人观察得到的写。
    pub fn close_all_panes(&self) {
        for p in &self.panes {
            p.pty.close();
        }
    }
```

- [ ] **Step 4: 跑测试确认绿**

```bash
cargo test -p mullion-app -- closing_a_pane_closes_its_channel closing_every_pane_closes_every_channel 2>&1 | tail -10
```

预期：2 passed。（此时 `app.rs` 的 `NullPty` 还没实现 `close`，若编译报错先做 Step 5 再跑。）

- [ ] **Step 5: 接上 app 侧三条路径**

`app.rs` 的 `NullPty`（测试模块里）补：

```rust
        fn close(&self) {}
```

`RecordingPty`（`app.rs:12120` 与 `:12205` 两处）同样各补一行 `fn close(&self) {}`。

`swap_pane_channel`（`app.rs:7922`）—— 换 channel 的两条路径（`rehost_pane` 换节点 / `reattach_pane` 重连）共用这一个函数，只需在这里关一次；原注释里「Drop 即关掉上一条 channel」也是错的：

```rust
fn swap_pane_channel(
    p: &mut crate::shell::workspace::PaneState,
    host_ix: usize,
    pty: Box<dyn crate::shell::workspace::PtyWriter>,
    rx: Receiver<Vec<u8>>,
) {
    p.host_ix = host_ix;
    // F140:**先显式关掉旧 channel**。旧的 `pty`/`rx` 会在下面两句赋值里
    // 被 Drop,但 Drop 关不掉 channel —— russh 0.54.5 的 `ChannelWriteHalf`
    // 没有 `Drop` 实现。不关的话换一次节点就在远端留一个挂着的 shell。
    p.pty.close();
    p.pty = pty;
    p.rx = rx;
    // ... 其余不动
```

`wind_down`（`app.rs:1274`）的 `Terminal` 分支，在 `t.ws` 被 drop 之前：

```rust
        TabContent::Terminal(t) => {
            for h in t.automation {
                h.task.abort();
            }
            for task in t.sftp_tasks {
                task.abort();
            }
            for task in t.reconnect_tasks {
                task.abort();
            }
            // F140:**显式**关掉每块 pane 的 channel,然后 `t.ws` 才 drop。
            // 光靠 drop 关不掉 —— russh 0.54.5 的 `ChannelWriteHalf` 没有
            // `Drop`(这一行原本的注释写反了)。
            t.ws.close_all_panes();
        }
```

- [ ] **Step 6: 写关标签那条路径的守护测试**

`app.rs` 的 `mod tests` 里（复用既有的 `Tab {...}` 构造写法，见 `app.rs:12671` 那条测试）：

```rust
    /// F140:关标签要把它名下**每一块** pane 的 channel 都关掉。
    ///
    /// `wind_down` 原本只 abort 后台任务、然后让 `Workspace` 自然 drop ——
    /// 而 drop 关不掉 channel(russh 0.54.5 的 `ChannelWriteHalf` 没有
    /// `Drop`)。用户关掉一个开了 4 块分屏的标签,远端就多 4 个挂着的 shell。
    ///
    /// 自证会变红:把 `wind_down` 里那句 `t.ws.close_all_panes()` 删掉。
    #[test]
    fn winding_down_a_terminal_tab_closes_every_pane_channel() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CountingPty(Arc<AtomicUsize>);
        impl crate::shell::workspace::PtyWriter for CountingPty {
            fn write(&self, _b: Vec<u8>) -> Result<(), mullion_ssh::TrySendErr> {
                Ok(())
            }
            fn resize(&self, _c: u16, _r: u16) -> Result<(), mullion_ssh::TrySendErr> {
                Ok(())
            }
            fn close(&self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        let closes = Arc::new(AtomicUsize::new(0));
        let mut p1 = test_pane(1);
        p1.pty = Box::new(CountingPty(closes.clone()));
        let mut ws = Workspace::new(p1, 0);
        let id2 = ws
            .split_focused(mullion_core::layout::Dir::Horizontal)
            .expect("分不出第二块");
        let mut p2 = test_pane(id2.0);
        p2.id = id2;
        p2.pty = Box::new(CountingPty(closes.clone()));
        ws.attach_pane(p2);

        let tab = Tab {
            id: TabId(1),
            title: "test".into(),
            session_id: None,
            title_override: None,
            color_override: None,
            content: TabContent::Terminal(Box::new(TerminalTab {
                ws,
                current_preset: None,
                last_cfg: None,
                automation: Vec::new(),
                automation_template: None,
                automation_status: None,
                files: Default::default(),
                sftp: None,
                sftp_host_ix: None,
                sftp_tasks: Vec::new(),
                sftp_default_remote: None,
                sftp_home: None,
                reconnect_tasks: Vec::new(),
            })),
        };
        wind_down(tab);

        assert_eq!(
            closes.load(Ordering::SeqCst),
            2,
            "关标签没关掉全部 pane 的 channel —— 远端会留下挂着的 shell(F140)"
        );
    }
```

若 `mullion_core::layout::Dir` 在 `app.rs` 里没 import，用全路径即可（上面已用全路径）；`TrySendErr` 的实际引用路径以 `app.rs` 顶部既有 `use` 为准。

- [ ] **Step 7: 跑测试确认绿**

```bash
cargo test -p mullion-app 2>&1 > /tmp/t2.log; grep -nE "test result|FAILED|panicked" /tmp/t2.log | tail -5
```

预期：`test result: ok`，无 FAILED。

- [ ] **Step 8: 提交**

```bash
git add crates/mullion-app/src/shell/workspace/mod.rs crates/mullion-app/src/app.rs
git commit -m "fix(app): 关分屏/换节点/关标签显式关闭 SSH channel (F140)

关分屏此前只丢弃 PaneState,而 russh 0.54.5 的 ChannelWriteHalf 没有 Drop
实现,io_task 只发 eof 不发 close —— 远端 shell 挂着不死,channel slot 泄漏
到 sshd MaxSessions 后同一条连接开不出新分屏(adr-009 列出的失效模式)。
tmux 语义:client 收 SIGHUP = detach,tmux server/session 不受影响。
守护测试:closing_a_pane_closes_its_channel /
closing_every_pane_closes_every_channel /
winding_down_a_terminal_tab_closes_every_pane_channel"
```

---

### Task 3: F138 — 文件面板统一内边距

**Files:**
- Modify: `crates/mullion-app/src/ui/files_panel.rs:1243-1247`（`sidebar` 的 `Frame`）、`:1329-1333`（`content` 的 `Frame`）
- Test: 同文件 `mod tests`

- [ ] **Step 1: 写失败的测试**

追加到 `files_panel.rs` 的 `mod tests`：

```rust
    /// F138:面板内容不能贴着外框画。判据取**真值**——「↑」按钮的实际
    /// rect 与面板外框 rect 相比,左边至少留出 `SP_S`。
    ///
    /// 不拿常量断言常量(那是重言式、恒绿):`Frame::inner_margin` 删掉之后
    /// 按钮 rect 会左移到面板左缘,这条必须变红。
    #[test]
    fn the_panel_does_not_draw_its_contents_flush_against_its_own_edge() {
        let t = crate::theme::MULLION_DARK;
        let mut frame = two_columns();
        let ctx = egui::Context::default();
        let mut cols = ColWidths::default();
        // 三帧:egui Panel 首帧是 sizing pass,rect 还没稳定;最后一帧的
        // 输出用来同时取面板外框和「↑」的位置,两者必须来自同一帧。
        let mut out = ctx.run(egui::RawInput::default(), |ctx| {
            content(ctx, &t, 1, false, &mut frame, 0, &mut cols, &mut None);
        });
        for _ in 0..2 {
            out = ctx.run(egui::RawInput::default(), |ctx| {
                content(ctx, &t, 1, false, &mut frame, 0, &mut cols, &mut None);
            });
        }
        // 面板外框 = 本帧最靠左的那个矩形(`CentralPanel` 的 `Frame` 背景)。
        let panel_left = out
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::epaint::Shape::Rect(r) => Some(r.rect.left()),
                _ => None,
            })
            .fold(f32::INFINITY, f32::min);
        assert!(panel_left.is_finite(), "一个矩形都没画出来 —— 脚手架本身有问题");
        let arrow = find_text_pos(&out.shapes, "↑").expect("路径条的「↑」没画出来");
        assert!(
            arrow.x >= panel_left + crate::ui::metrics::SP_S,
            "「↑」贴着面板边缘画(x={}, 面板左缘={}),F138 要求至少留 {} 点内边距",
            arrow.x,
            panel_left,
            crate::ui::metrics::SP_S
        );
    }
```

**实现者注意：** `two_columns()` 与 `find_text_pos` 都是本模块已有的 helper。路径条画在 `Load` 匹配之前，所以两栏是不是 `Load::Ready` 都不影响本测试。`content` 的第八个参数 `&mut None` 是 Task 4 引入的面板矩形出参——若 Task 4 尚未做，先去掉这个参数，Task 4 时再补上。

- [ ] **Step 2: 跑测试确认它红**

```bash
cargo test -p mullion-app the_panel_does_not_draw_its_contents_flush 2>&1 | tail -15
```

预期：FAILED，消息里 `x` 与「面板左缘」相差不足 8。

- [ ] **Step 3: 实现**

`sidebar`（`:1243`）：

```rust
        .frame(
            egui::Frame::none()
                .fill(theme::c32(t.panel_bg))
                .stroke(theme::stroke(t))
                // F138:内容不贴边。左右比上下宽 —— 横向是「内容 vs 边框」,
                // 纵向相邻的是别的面板,那边本就有分隔线垫着。取值只从
                // `metrics` 的间距五档里选,不写裸数字。
                .inner_margin(egui::Margin::symmetric(
                    crate::ui::metrics::SP_S,
                    crate::ui::metrics::SP_XS,
                )),
        )
```

`content`（`:1329`）加同样的 `.inner_margin(...)`。

- [ ] **Step 4: 跑测试确认绿 + 回归 F135/F136/F137**

```bash
cargo test -p mullion-app files_panel 2>&1 > /tmp/t3.log; grep -nE "test result|FAILED" /tmp/t3.log | tail -5
```

预期：`test result: ok`。**若 F135/F136/F137 那批列宽/横滚/截断测试里有红的**，先看它是不是因为可用宽度少了 16 点而落到别的分支 —— 那种情况调测试里的面板宽度常量，**不要**去动被测的截断/滚动逻辑。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/ui/files_panel.rs
git commit -m "feat(app): 文件面板两个宿主统一加内边距 (F138)"
```

---

### Task 4: F59 修复 — 交接判据改成「离开文件面板矩形」

**Files:**
- Modify: `crates/mullion-app/src/dragout/mod.rs:82-98`（判据）、`:239-246`（旧测试改写）
- Modify: `crates/mullion-app/src/ui/mod.rs`（`UiState` 新字段、`pointer_inside_window` 旁边加 `pointer_inside_panel`、交接接线）
- Modify: `crates/mullion-app/src/ui/files_panel.rs`（`sidebar`/`content` 回填面板矩形）

**背景：** 旧判据是「指针出了窗口」。用户窗口一直最大化 → 永远不成立 → `start_drag_out` 一次都没被调用过（它每条失败路径都会 `set_error`，而用户报的是「完全无提示」）。

- [ ] **Step 1: 写失败的测试（纯函数）**

`dragout/mod.rs` 的 `mod tests`：把旧的 `a_drag_that_is_still_inside_the_window_is_not_handed_to_the_os` 整条替换成下面两条：

```rust
    #[test]
    fn a_drag_that_is_still_inside_the_files_panel_is_not_handed_to_the_os() {
        // **F58 的命根子**:远端栏起拖的手势同时属于「拖到本地栏 = 下载」。
        // 两栏都在面板矩形里,面板内就交给 OS 的话 `DoDragDrop` 接管鼠标
        // 捕获,F58 的远端→本地方向当场失效。
        assert!(!should_hand_off(Some(PanelColumn::Remote), true, false));
    }

    #[test]
    fn a_drag_that_left_the_panel_but_not_the_window_is_handed_to_the_os() {
        // **这条就是 v0.1.54 的真 bug**:旧判据问的是「指针出没出**窗口**」,
        // 而用户的窗口一直最大化 —— 指针无处可去,拖出永远触发不了。
        // 把参数换回「指针在窗口内」的语义,这条必红。
        assert!(should_hand_off(Some(PanelColumn::Remote), false, false));
    }
```

- [ ] **Step 2: 跑测试确认它红**

```bash
cargo test -p mullion-app a_drag_that_left_the_panel 2>&1 | tail -10
```

预期：编译通过但语义未改时**这条会绿**（参数位置一样）。这是本任务唯一一处「测试先行挡不住」的地方 —— 真正的判据在接线层（Step 5 的那条测试）。仍要先改名和文档，让判据语义与调用方一致。

- [ ] **Step 3: 改判据的签名与文档**

`dragout/mod.rs:82`：

```rust
/// 什么时候把这一拖交给操作系统(设计 N1,2026-08-20 修正)。
///
/// - `from`:egui 拖拽载荷说这一拖是从哪一栏起的。`None` = 现在没在拖。
/// - `pointer_inside_panel`:指针还在**文件面板矩形**里没有。
/// - `already_running`:已经有一条拖出线程在跑了。
///
/// **判据是「出没出文件面板」,不是「出没出窗口」**:后者是 v0.1.37 的
/// 原始实现,而窗口最大化时它永远不成立 —— 拖出功能从发布起就没触发过一次。
///
/// `!pointer_inside_panel` 这一条仍是 **F58 的命根子**:远端栏起拖的手势
/// 同时属于 F58(拖到本地栏 = 下载),而两栏都在面板矩形**内**,所以那半个
/// 手势全程不会触发 OLE 交接。面板内 = 内部传输,面板外 = 交给系统。
pub fn should_hand_off(
    from: Option<PanelColumn>,
    pointer_inside_panel: bool,
    already_running: bool,
) -> bool {
    from == Some(PanelColumn::Remote) && !pointer_inside_panel && !already_running
}
```

- [ ] **Step 4: 让两个宿主把自己的矩形回填给 `UiState`**

`ui/mod.rs` 的 `UiState`（`:309` 的 `files_cols` 之后）加字段：

```rust
    /// F59:文件面板**上一帧**的外框矩形。`None` = 上一帧没画文件面板。
    ///
    /// 为什么是上一帧:拖出交接的判定发生在 `build_ui` 的前段(要在动作
    /// 路由之前算出 `files_drag_out`),而两个宿主(`SidePanel`/`CentralPanel`)
    /// 都在那之后才 show。拖拽持续几十帧,差一帧的矩形对判据没有影响。
    pub files_panel_rect: Option<egui::Rect>,
```

`files_panel::sidebar` 在读回宽度那一句旁边补写（`files_panel.rs:1289` 附近）：

```rust
    ui_state.files_sidebar_w = resp.response.rect.width();
    // F59:把外框矩形留给下一帧的拖出交接判据用(见 `UiState::files_panel_rect`)。
    ui_state.files_panel_rect = Some(resp.response.rect);
```

`files_panel::content` 加一个出参（签名末尾，`cols` 之后）：

```rust
pub fn content(
    ctx: &egui::Context,
    t: &Theme,
    generation: u64,
    panel_focused: bool,
    frame: &mut PanelFrame,
    drop_in: usize,
    cols: &mut ColWidths,
    /// F59:把本帧的外框矩形写回去,给下一帧的拖出交接判据用。
    /// 独立出参而不是收整个 `UiState` —— 标签宿主刻意不认识 `UiState`
    /// (见本函数上面的文档)。
    panel_rect: &mut Option<egui::Rect>,
) -> (Option<FileAction>, Option<FileAction>) {
```

函数体里 `CentralPanel::default()...show(ctx, |ui| { ... })` 的**闭包第一行**写入：

```rust
            *panel_rect = Some(ui.max_rect());
```

`ui/mod.rs:824` 的调用点补上 `&mut ui_state.files_panel_rect`。`files_panel.rs` 里既有的 `content(...)` 测试调用点全部补 `&mut None`。

- [ ] **Step 5: 接线 + 写接线层的守护测试**

`ui/mod.rs` 的 `pointer_inside_window` 旁边加：

```rust
/// F59:指针在不在文件面板矩形里。
///
/// `rect` 为 `None`(面板刚开、还没画过一帧)时返回 `true` —— 保守当作
/// 「在面板内」,宁可少交接一帧,也不要在面板第一帧就把手势甩给 OS。
/// 指针位置取不到(`latest_pos` 为 `None`)时返回 `false`:指针都不知道
/// 在哪了,当然不在面板里,这是旧「出了窗口」行为的超集。
fn pointer_inside_panel(ctx: &egui::Context, rect: Option<egui::Rect>) -> bool {
    let Some(rect) = rect else { return true };
    ctx.input(|i| i.pointer.latest_pos())
        .is_some_and(|p| rect.contains(p))
}
```

交接处（`ui/mod.rs:620` 那段）改成：

```rust
    // F59:远端栏起的那一拖,指针一旦**离开文件面板**就交给操作系统
    // (设计 N1,2026-08-20 修正)。原来的判据是「出了窗口」——用户窗口
    // 最大化时永远不成立,这个功能从 v0.1.37 起就没触发过一次。
    // 面板内不交 —— 那一半手势是 F58(拖到本地栏 = 下载),`DoDragDrop`
    // 一接管鼠标捕获,F58 的远端→本地方向当场失效。
    let dragging_from =
        egui::DragAndDrop::payload::<crate::files::drag::DragFrom>(ctx).map(|p| p.0);
    let inside_panel = pointer_inside_panel(ctx, ui_state.files_panel_rect);
    if files_open
        && crate::dragout::should_hand_off(
            dragging_from,
            inside_panel,
            crate::dragout::is_running(),
        )
    {
        log::debug!(
            target: crate::dragout::LOG,
            "交给 OS:指针 {:?} 已离开面板 {:?}",
            ctx.input(|i| i.pointer.latest_pos()),
            ui_state.files_panel_rect
        );
        egui::DragAndDrop::clear_payload(ctx);
        actions.files_drag_out = true;
    }
```

若 `pointer_inside_window` 在改完之后没有别的调用方，删掉它（Scope Discipline：这次改动导致它变得无用）。

守护测试（`ui/mod.rs` 的 `mod tests`）：

```rust
    /// F59 的真 bug:窗口最大化时,指针离开文件面板但仍在窗口内 ——
    /// 旧判据(`pointer_inside_window`)恒为 `true`,拖出永远不触发。
    ///
    /// 自证会变红:把 `pointer_inside_panel(ctx, ui_state.files_panel_rect)`
    /// 换回 `pointer_inside_window(ctx)`。
    #[test]
    fn a_pointer_outside_the_panel_is_not_inside_it_even_when_the_window_is_maximized() {
        let ctx = egui::Context::default();
        let panel = egui::Rect::from_min_size(egui::pos2(1000.0, 0.0), egui::vec2(400.0, 800.0));
        let mut input = egui::RawInput::default();
        // 指针落在窗口内、面板外(终端区)。
        input.events.push(egui::Event::PointerMoved(egui::pos2(200.0, 400.0)));
        let _ = ctx.run(input, |_| {});
        assert!(
            !pointer_inside_panel(&ctx, Some(panel)),
            "指针在终端区却被判成「在文件面板里」—— 拖出永远触发不了(F59)"
        );
        assert!(
            pointer_inside_panel(&ctx, None),
            "还没画过面板时应保守当作「在面板内」,不该在第一帧就甩给 OS"
        );
    }
```

- [ ] **Step 6: 跑测试确认绿**

```bash
cargo test -p mullion-app 2>&1 > /tmp/t4.log; grep -nE "test result|FAILED|panicked" /tmp/t4.log | tail -5
```

预期：无 FAILED。

- [ ] **Step 7: 提交**

```bash
git add crates/mullion-app/src/dragout/mod.rs crates/mullion-app/src/ui/mod.rs crates/mullion-app/src/ui/files_panel.rs
git commit -m "fix(app): 拖出交接判据改成「指针离开文件面板」 (F59)

原判据是「指针出了窗口」,窗口最大化时永远不成立 —— 拖出到资源管理器
从 v0.1.37 起一次都没触发过(start_drag_out 的每条失败路径都会 set_error,
而用户报的是完全无提示)。面板矩形取上一帧的值经 UiState 传递:交接判定
在 build_ui 前段,两个宿主都在那之后才 show。
守护测试:a_pointer_outside_the_panel_is_not_inside_it_even_when_the_window_is_maximized"
```

---

### Task 5: F139-a — 路径条的 ☆ 与 ▾，删掉旧书签栏

**Files:**
- Modify: `crates/mullion-app/src/ui/files_panel.rs:18-61`（`FileAction`）、`:386-397`（`show` 签名）、`:466-528`（路径条）、`:530-552`（删旧书签栏）、`:1129-1180`（`PanelFrame`）、`:1255-1280` 与 `:1357-1390`（两处调用点）
- Test: 同文件 `mod tests`

- [ ] **Step 1: 定义参数类型与动作**

`files_panel.rs` 的 `FileAction` 枚举里加两条：

```rust
    /// F139:把当前目录收进书签。`name` 由 UI 取路径末段算好 —— app 侧
    /// 只负责落盘,不重复一遍命名规则。
    BookmarkAdd { path: String, name: String },
    /// F139:取消收藏。按 `path` 相等匹配(书签的身份就是路径,重名允许)。
    BookmarkRemove { path: String },
```

`show` 的 `bookmarks: &[mullion_store::Bookmark]` 参数换成：

```rust
/// F139:书签视图。`list` 是该会话已配的书签,`can_edit` = 这个标签绑着
/// 一条会话记录(有 `SessionId`),收藏才有地方落盘。本地栏恒传
/// `BookmarkView::none()`。
#[derive(Clone, Copy)]
pub struct BookmarkView<'a> {
    pub list: &'a [mullion_store::Bookmark],
    pub can_edit: bool,
}

impl BookmarkView<'_> {
    /// 本地栏用:没有书签、也不能收藏。
    pub fn none() -> Self {
        Self {
            list: &[],
            can_edit: false,
        }
    }
}
```

`PanelFrame` 加一个字段（`Default` 里给 `false`）：

```rust
    /// F139:这个标签绑着会话记录没有(`Tab::session_id.is_some()`)。
    /// 没绑就没地方存书签,☆ 按钮置灰。默认 `false` —— 与 `Default` 的
    /// 双重语境约定一致(见上面的说明),不知道时按"不能写"处理。
    pub session_bound: bool,
```

- [ ] **Step 2: 写失败的测试**

`files_panel.rs` 的 `mod tests`，先加一个 helper（放在 `find_text_pos` 附近）：

```rust
    /// F139:跑一帧远端栏,把 shapes 与 ctx 一起交出去。书签相关的几条
    /// 测试都要「点一下 → 下一帧看结果」,统一走这个。
    fn run_remote(
        ctx: &egui::Context,
        state: &mut PaneState,
        bm: BookmarkView<'_>,
        cols: &mut ColWidths,
        input: egui::RawInput,
    ) -> (egui::FullOutput, Option<FileAction>) {
        let t = crate::theme::MULLION_DARK;
        let mut act = None;
        let out = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                act = show(ui, &t, "远端", 1, PanelColumn::Remote, state, false, bm, 0, cols);
            });
        });
        (out, act)
    }

    /// 在 `pos` 处点一下的 `RawInput`(按下 + 松开同一帧,egui 认这个)。
    fn click_at(pos: egui::Pos2) -> egui::RawInput {
        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::PointerMoved(pos));
        input.events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Default::default(),
        });
        input.events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        });
        input
    }
```

再写三条测试：

```rust
    /// F139:当前目录没被收藏时画空心 ☆,点一下发出 `BookmarkAdd`,
    /// 名字取路径末段。
    #[test]
    fn clicking_the_hollow_star_bookmarks_the_current_directory() {
        let ctx = egui::Context::default();
        let mut state = PaneState::new(RemotePath::from_bytes(b"/var/log".to_vec()));
        state.load = Load::Ready;
        let mut cols = ColWidths::default();
        let bm = BookmarkView { list: &[], can_edit: true };
        let (out, _) = run_remote(&ctx, &mut state, bm, &mut cols, egui::RawInput::default());
        let star = find_text_pos(&out.shapes, "☆").expect("空心 ☆ 没画出来");
        let (_, act) = run_remote(&ctx, &mut state, bm, &mut cols, click_at(star));
        match act {
            Some(FileAction::BookmarkAdd { path, name }) => {
                assert_eq!(path, "/var/log");
                assert_eq!(name, "log", "书签名默认取路径末段");
            }
            other => panic!("点 ☆ 没发出 BookmarkAdd,实际:{other:?}"),
        }
    }

    /// F139:已收藏的目录画实心 ★,再点一下 = 取消收藏。
    #[test]
    fn clicking_the_filled_star_removes_that_bookmark() {
        let ctx = egui::Context::default();
        let mut state = PaneState::new(RemotePath::from_bytes(b"/var/log".to_vec()));
        state.load = Load::Ready;
        let mut cols = ColWidths::default();
        let list = vec![mullion_store::Bookmark {
            name: "日志".into(),
            path: "/var/log".into(),
        }];
        let bm = BookmarkView { list: &list, can_edit: true };
        let (out, _) = run_remote(&ctx, &mut state, bm, &mut cols, egui::RawInput::default());
        let star = find_text_pos(&out.shapes, "★").expect("当前目录已收藏,该画实心 ★");
        let (_, act) = run_remote(&ctx, &mut state, bm, &mut cols, click_at(star));
        assert!(
            matches!(act, Some(FileAction::BookmarkRemove { ref path }) if path == "/var/log"),
            "再点实心 ★ 应发出 BookmarkRemove,实际:{act:?}"
        );
    }

    /// F139:标签没绑会话记录时收藏无处可落,☆ 必须点不动 ——
    /// 能点但静默丢弃是最坏的一档(用户以为收藏了)。
    ///
    /// 自证会变红:把 `add_enabled_ui(bm.can_edit, ...)` 的条件写死成 `true`。
    #[test]
    fn the_star_is_disabled_when_the_tab_is_not_bound_to_a_session() {
        let ctx = egui::Context::default();
        let mut state = PaneState::new(RemotePath::from_bytes(b"/var/log".to_vec()));
        state.load = Load::Ready;
        let mut cols = ColWidths::default();
        let bm = BookmarkView { list: &[], can_edit: false };
        let (out, _) = run_remote(&ctx, &mut state, bm, &mut cols, egui::RawInput::default());
        let star = find_text_pos(&out.shapes, "☆").expect("☆ 该画出来(置灰也要画)");
        let (_, act) = run_remote(&ctx, &mut state, bm, &mut cols, click_at(star));
        assert!(act.is_none(), "没有会话记录时点 ☆ 不该发出任何动作,实际:{act:?}");
    }

    /// F139:▾ 里列出全部书签,点一条跳过去。菜单是弹出层 ——
    /// 点开要一帧、菜单内容画出来在下一帧,所以这里跑三帧。
    #[test]
    fn picking_a_bookmark_from_the_dropdown_emits_goto() {
        let ctx = egui::Context::default();
        let mut state = PaneState::new(RemotePath::from_bytes(b"/".to_vec()));
        state.load = Load::Ready;
        let mut cols = ColWidths::default();
        let list = vec![mullion_store::Bookmark {
            name: "日志".into(),
            path: "/var/log".into(),
        }];
        let bm = BookmarkView { list: &list, can_edit: true };
        let (out, _) = run_remote(&ctx, &mut state, bm, &mut cols, egui::RawInput::default());
        let caret = find_text_pos(&out.shapes, "▾").expect("▾ 没画出来");
        let (out, _) = run_remote(&ctx, &mut state, bm, &mut cols, click_at(caret));
        let item = find_text_pos(&out.shapes, "日志")
            .or_else(|| {
                // 菜单内容可能晚一帧才画出来。
                let (out2, _) =
                    run_remote(&ctx, &mut state, bm, &mut cols, egui::RawInput::default());
                find_text_pos(&out2.shapes, "日志")
            })
            .expect("菜单里没有「日志」这条书签");
        let (_, act) = run_remote(&ctx, &mut state, bm, &mut cols, click_at(item));
        assert!(
            matches!(act, Some(FileAction::Goto(ref p)) if p.display() == "/var/log"),
            "点菜单里的书签没跳过去,实际:{act:?}"
        );
    }
```

同时**删掉**旧的 `clicking_a_bookmark_dispatches_goto_to_its_path` 与 `only_the_remote_column_gets_a_bookmarks_bar` 两条（它们守的是被删掉的横排书签栏）；后者的意图由新的 `the_star_is_disabled_when_the_tab_is_not_bound_to_a_session` 与「本地栏传 `BookmarkView::none()`」共同承接。

- [ ] **Step 3: 跑测试确认它红**

```bash
cargo test -p mullion-app -- clicking_the_hollow_star clicking_the_filled_star picking_a_bookmark_from 2>&1 | tail -15
```

预期：编译失败（`BookmarkView` / `BookmarkAdd` 尚未接进 `show`）或断言失败（☆ 没画出来）。

- [ ] **Step 4: 实现路径条按钮**

`show` 的路径条 `ui.horizontal(...)` 里，**在 `⟳` 之后、路径标签之前**插入（放前面而不是行尾：路径 `Label` 用 `available_width` 吃掉整行剩余宽度，排在它后面的按钮会被挤出可视区）：

```rust
        // F139:收藏当前目录。★/☆ 由「当前 cwd 在不在书签列表里」现算 ——
        // 不存标志位,列表就是唯一真值。
        let here = state.cwd.display().to_string();
        let starred = bookmarks.list.iter().any(|b| b.path == here);
        ui.add_enabled_ui(bookmarks.can_edit, |ui| {
            let btn = ui
                .small_button(if starred { "★" } else { "☆" })
                .on_hover_text(if starred {
                    "取消收藏这个目录"
                } else {
                    "收藏这个目录"
                })
                // 置灰时 `on_hover_text` 不生效(egui 对 disabled 部件不显示
                // 普通 tooltip),用 `on_disabled_hover_text` 说明原因 ——
                // 一个点不动又不说为什么的按钮比没有更糟。
                .on_disabled_hover_text("这个标签不来自已保存的会话,书签无处存放");
            if btn.clicked() {
                action = Some(if starred {
                    FileAction::BookmarkRemove { path: here.clone() }
                } else {
                    FileAction::BookmarkAdd {
                        name: bookmark_default_name(&here),
                        path: here.clone(),
                    }
                });
            }
        });
        // F139:书签下拉。没有书签时置灰 —— 点开一个空菜单是纯噪音。
        ui.add_enabled_ui(!bookmarks.list.is_empty(), |ui| {
            ui.menu_button("▾", |ui| {
                for b in bookmarks.list {
                    // 空名字是 store 明确允许的合法状态(`Bookmark::name`
                    // 文档),界面回退显示路径本身,不能画一条没有文字的项。
                    let label = if b.name.is_empty() {
                        b.path.as_str()
                    } else {
                        b.name.as_str()
                    };
                    if ui.button(label).on_hover_text(&b.path).clicked() {
                        action = Some(FileAction::Goto(
                            mullion_ssh::sftp::RemotePath::from_bytes(b.path.as_bytes().to_vec()),
                        ));
                        ui.close_menu();
                    }
                }
            })
            .response
            .on_disabled_hover_text("还没有收藏任何路径");
        });
```

在 `show` 之外加纯函数（放在 `finish_path_edit` 附近）：

```rust
/// F139:新书签的默认名 = 路径末段。根目录没有末段,回退成 `/` ——
/// 空名字虽然 store 允许,但菜单里会退化成显示整条路径,不如直接给个 `/`。
fn bookmark_default_name(path: &str) -> String {
    match path.trim_end_matches('/').rsplit('/').next() {
        Some(seg) if !seg.is_empty() => seg.to_owned(),
        _ => "/".to_owned(),
    }
}
```

**删掉** `files_panel.rs:530-552` 的整段旧书签栏（`if !bookmarks.is_empty() { ui.horizontal_wrapped(...) }`）。

- [ ] **Step 5: 更新两处调用点**

`sidebar`（`:1263`）本地栏传 `BookmarkView::none()`，远端栏（`:1277`）传：

```rust
                BookmarkView {
                    list: &frame.bookmarks,
                    can_edit: frame.session_bound,
                },
```

`content` 的两处同样改。`files_panel.rs` 与 `ui/mod.rs` 里所有 `show(...)` 的测试调用点，`&bookmarks` / `&[]` 一律换成对应的 `BookmarkView`。

- [ ] **Step 6: 跑测试确认绿**

```bash
cargo test -p mullion-app 2>&1 > /tmp/t5.log; grep -nE "test result|FAILED|panicked" /tmp/t5.log | tail -5
```

- [ ] **Step 7: 提交**

```bash
git add crates/mullion-app/src/ui/files_panel.rs crates/mullion-app/src/ui/mod.rs
git commit -m "feat(app): 路径条收藏按钮与书签下拉,去掉横排书签栏 (F139)"
```

---

### Task 6: F139-b — 收藏落到会话配置

**Files:**
- Modify: `crates/mullion-app/src/app.rs`（`apply_remote_file_action` 加两条分支、`PanelFrame` 构造处设 `session_bound`、`touched_store`）
- Test: `crates/mullion-app/src/app.rs` 的 `mod tests`

- [ ] **Step 1: 实现动作分支**

`apply_remote_file_action`（`app.rs:2838` 那个 `match &action`）——这两条**不产生 `target`**，在函数早段（`FileAction::Ask(_)` 那批分流的地方）处理，因为它们要借 `self.store`，而后面那段已经借着 `tab`：

```rust
        // F139:收藏/取消收藏。**不走 `target` 那条路** —— 它们不改当前目录,
        // 只改会话配置;而且要借 `self.store`,不能夹在借着 `tab` 的那段里。
        if let FileAction::BookmarkAdd { path, name } = &action {
            self.add_bookmark(generation, path.clone(), name.clone());
            return;
        }
        if let FileAction::BookmarkRemove { path } = &action {
            self.remove_bookmark(generation, path.clone());
            return;
        }
```

新增两个方法（放在 `apply_remote_file_action` 附近）：

```rust
    /// F139:把一条书签写进会话配置,并同步这个标签的内存副本。
    ///
    /// 两处都要写:`PanelFrame::bookmarks` 是这一帧画 ★/▾ 用的,
    /// store 那份是重启之后还在的。只写一处的话,要么按钮不变亮、
    /// 要么关掉客户端收藏就没了。
    fn add_bookmark(&mut self, generation: u64, path: String, name: String) {
        let Some(tab) = self.tabs.by_generation(generation) else {
            return;
        };
        let Some(sid) = tab.session_id else {
            // UI 已经把按钮置灰了(`BookmarkView::can_edit`),走到这儿说明
            // 接线被改坏了 —— 不静默吞。
            log::warn!("收到 BookmarkAdd 但标签没有 SessionId,已忽略");
            return;
        };
        let bm = mullion_store::Bookmark { name, path };
        if let Some(store) = self.store.as_mut() {
            store.update_session(sid, |rec| {
                if !rec.sftp.bookmarks.iter().any(|b| b.path == bm.path) {
                    rec.sftp.bookmarks.push(bm.clone());
                }
            });
        }
        if let Some(tab) = self.tabs.by_generation_mut(generation) {
            if let Some(files) = tab.content.files_panel_mut() {
                if !files.bookmarks.iter().any(|b| b.path == bm.path) {
                    files.bookmarks.push(bm);
                }
            }
        }
        self.ui_dirty = true;
    }

    /// F139:取消收藏。按路径相等匹配 —— 书签的身份就是路径。
    fn remove_bookmark(&mut self, generation: u64, path: String) {
        let Some(tab) = self.tabs.by_generation(generation) else {
            return;
        };
        let Some(sid) = tab.session_id else {
            log::warn!("收到 BookmarkRemove 但标签没有 SessionId,已忽略");
            return;
        };
        if let Some(store) = self.store.as_mut() {
            store.update_session(sid, |rec| rec.sftp.bookmarks.retain(|b| b.path != path));
        }
        if let Some(tab) = self.tabs.by_generation_mut(generation) {
            if let Some(files) = tab.content.files_panel_mut() {
                files.bookmarks.retain(|b| b.path != path);
            }
        }
        self.ui_dirty = true;
    }
```

**实现者注意：** `self.store` 的实际类型与「改一条会话记录并存盘」的既有写法以 `app.rs` 里现成的调用为准（搜 `update_session` / `save`；若没有 `update_session` 这个方法，照该文件里改会话记录的既有姿势写，**不要**新造一套存盘路径）。存盘失败的处理照既有姿势（多半是 `set_error`）。

- [ ] **Step 2: 设置 `session_bound`**

每处构造 `PanelFrame::new(...)` 或 `PanelFrame { ... }` 的地方（`app.rs:5437`、`:5534` 及终端标签那处），紧接着写：

```rust
                    files.session_bound = session_id.is_some();
```

若构造是内联在结构体字面量里的，就在 `PanelFrame::new` 里加第三个参数 `session_bound: bool` 并更新全部调用点 —— 两种做法二选一，**选后者**（编译器会逼着每个调用点表态，不会漏）。

- [ ] **Step 3: 把书签动作算进 `touched_store`**

`app.rs:7415` 的 `let touched_store = ...` 表达式里补上这两条动作。判据表达式形如 `self.ui.delete_request.is_some() || ...`，按同样的形式加：

```rust
                    || matches!(
                        actions.files_remote,
                        Some(crate::ui::files_panel::FileAction::BookmarkAdd { .. })
                            | Some(crate::ui::files_panel::FileAction::BookmarkRemove { .. })
                    )
```

**实现者注意：** `touched_store` 必须算在那批 `take()` 之前（源码里已有注释说明）；`actions.files_remote` 在这一行之后才被消费，读它是安全的 —— 若发现顺序不对，把这条判据挪到 `take` 之前，不要改 `take` 的顺序。

- [ ] **Step 4: 写守护测试**

```rust
    /// F139:收藏必须进 `touched_store`,否则外观缓存/存盘链路收不到通知,
    /// 用户重启客户端后收藏消失(切片 I 踩过同一个坑)。
    ///
    /// 与既有的 `import_request` 那条同款,扎源码 —— `touched_store` 要一个
    /// 真的 `App` 才能求值,而 `App` 在无头环境里造不出来。
    ///
    /// 自证会变红:把上面那条 `BookmarkAdd` 从表达式里删掉。
    #[test]
    fn bookmarking_is_counted_as_touching_the_store() {
        let src = include_str!("app.rs");
        let expr = src
            .split("let touched_store = ")
            .nth(1)
            .expect("找不到 touched_store 的赋值")
            .split(";\n")
            .next()
            .expect("touched_store 的表达式切歪了 —— 下面那条断言会空过");
        assert!(
            expr.contains("BookmarkAdd"),
            "收藏没算进 touched_store:书签不会存盘(F139)"
        );
        assert!(
            expr.contains("BookmarkRemove"),
            "取消收藏没算进 touched_store:删掉的书签重启后会回来(F139)"
        );
    }
```

**实现者注意（第五类恒绿模式）：** 这条测试自己的源码里就含字符串 `"BookmarkAdd"`，而 `include_str!("app.rs")` 读的正是同一个文件。切片必须先 `split("let touched_store = ")` 把范围缩到那一条表达式内（上面已经这么做），**不能**直接在整份源码上 `contains`——那样它永远绿。写完后按自证说明变异一次确认它真的会红。

- [ ] **Step 5: 跑测试确认绿**

```bash
cargo test -p mullion-app 2>&1 > /tmp/t6.log; grep -nE "test result|FAILED|panicked" /tmp/t6.log | tail -5
```

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): 书签收藏落到会话配置并标记 store 脏 (F139)"
```

---

### Task 7: spec.md 台账

**Files:**
- Modify: `spec.md:128`（F59 行补记）、`:146` 之后（新增三行）

- [ ] **Step 1: 补 F59 行**

在 F59 那一行的「验收/守护」列末尾追加：

```
；**2026-08-20 修正**:交接判据原为「指针出了窗口」,窗口最大化时永远不成立 —— 这个功能从 v0.1.37 起一次都没触发过。改为「指针离开文件面板矩形」,守护 `a_pointer_outside_the_panel_is_not_inside_it_even_when_the_window_is_maximized`
```

- [ ] **Step 2: 新增三行**

在 F137 那一行之后插入（保持表格三列格式：编号 | 描述 | 优先级 | 守护）：

```markdown
| F138 | **文件面板统一内边距**:两个宿主(侧栏 `SidePanel` / 标签 `CentralPanel`)的 `Frame` 加 `inner_margin`,左右 `SP_S`、上下 `SP_XS`,内容不再贴着外框画 | P3 | `the_panel_does_not_draw_its_contents_flush_against_its_own_edge` —— 判据取真值(「↑」按钮 rect 与面板外框 rect 相比),不拿常量断言常量;F135/F136/F137 那批列宽/横滚测试须一并回归(可用宽度少了 16 点) |
| F139 | **路径条收藏与书签下拉**:☆/★ 切换收藏当前目录(名字默认取路径末段),▾ 列出全部书签点击即跳。**去掉原来的横排书签栏**(只在已配过书签时才出现,用户根本不知道它存在)。书签仍存 `SessionRecord.sftp.bookmarks`,与会话编辑页共用一份,无 schema 改动。标签没有 `SessionId` 时 ☆ 置灰并悬停说明原因;只给远端栏 | P2 | `clicking_the_hollow_star_bookmarks_the_current_directory` / `clicking_the_filled_star_removes_that_bookmark`(★/☆ 由 cwd 与列表现算,不存标志位)/ `the_star_is_disabled_when_the_tab_is_not_bound_to_a_session` / `picking_a_bookmark_from_the_dropdown_emits_goto`;`bookmarking_is_counted_as_touching_the_store` 守住存盘链路 |
| F140 | **SSH channel 显式收口**:关分屏 / 换节点 / 关标签三条路径都发 `SSH_MSG_CHANNEL_CLOSE`。此前只丢弃 `PaneState`,而 **russh 0.54.5 的 `ChannelWriteHalf` 没有 `Drop` 实现**,`io_task` 只发 `eof` —— 远端 shell 挂着不死,channel slot 泄漏到 sshd `MaxSessions`(默认 10)后同一条连接开不出新分屏(adr-009 已列的失效模式)。tmux 语义:client 收 SIGHUP = detach,tmux server/session 不受影响;裸前台命令会被 SIGHUP 杀掉,与关掉 PuTTY 窗口一致 | P1 | `close_sends_a_close_command_down_the_io_queue`(ssh 侧)/ `closing_a_pane_closes_its_channel` / `closing_every_pane_closes_every_channel` / `winding_down_a_terminal_tab_closes_every_pane_channel`;**「真的发出了 CHANNEL_CLOSE 报文」无头验不了**,进人工清单(远端 `ps -ef \| grep sshd` 看子进程回收) |
```

- [ ] **Step 3: 提交**

```bash
git add spec.md
git commit -m "docs(spec): 登记 F138/F139/F140,F59 补记判据修正"
```

---

### Task 8: 交付一条龙

按 `CLAUDE.md` 的交付约定执行，**不要停下来问**。

- [ ] **Step 1: bump 版本**

`Cargo.toml` 的 `workspace.package.version` 第三位 +1（`0.1.54` → `0.1.55`）。

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: 版本 0.1.55(面板内边距 + 拖出判据 + 路径收藏 + channel 收口)"
```

- [ ] **Step 2: 跑绿**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log | tail -20
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
cargo fmt --check
```

预期：无 FAILED、clippy 无输出、fmt 无输出。**不绿不发。**

- [ ] **Step 3: 交叉编译 + 依赖验收**

```bash
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app
x86_64-w64-mingw32-objdump -p target/x86_64-pc-windows-gnu/release/mullion-app.exe | grep 'DLL Name'
```

出现 `libgcc_s_seh-1.dll` 或 `libwinpthread-1.dll` 即为不合格，必须修（见 `docs/cross-compile-windows.md`）。

- [ ] **Step 4: 签名（必须在算 sha256 之前）**

```bash
scripts/sign-windows.sh target/x86_64-pc-windows-gnu/release/mullion-app.exe
```

**这是唯一漏了也不报错的一步。**

- [ ] **Step 5: 发 Release**

先 push 再发版（`gh release create` 会把 tag 建在远端当前 HEAD 上）。命令与代理设置见 `.claude/skills/release-windows/SKILL.md`。Release 标题**只能**是纯版本号 `v0.1.55`。

- [ ] **Step 6: 报给用户**

Release 链接 + sha256 + 下面这份人工验收清单：

1. 文件面板左右两侧有可见留白，「↑」不再贴边；拖列宽、横向滚动照常
2. **窗口最大化状态下**从远端栏拖文件到终端区，光标变成拖拽图标；拖到任务栏资源管理器图标上悬停切窗口，放进目录能落文件
3. 远端栏 ☆ 收藏当前目录 → 变 ★ → ▾ 里能看到并跳回去；重启客户端后收藏还在；快速连接（无会话记录）时 ☆ 置灰且悬停有说明
4. 开 3~4 个分屏后逐个关掉，远端 `ps -ef | grep sshd` 看子进程数回落；tmux 里的会话 `tmux ls` 仍在、`tmux attach` 回去内容完好

---

## 自审记录

- **spec 覆盖**：设计文档四节 ↔ Task 3（①F138）、Task 4（②F59）、Task 5+6（③F139）、Task 1+2（④F140），全部有对应任务；人工验收清单已搬进 Task 8。
- **类型一致性**：`BookmarkView` 在 Task 5 定义、Task 5 Step 5 的调用点使用；`FileAction::BookmarkAdd/Remove` 在 Task 5 定义、Task 6 消费；`PtyWriter::close` 在 Task 2 定义，`FakePty`/`NullPty`/`RecordingPty`/`CountingPty` 四个替身都补了实现；`Workspace::close_all_panes` 在 Task 2 定义并在同一 Task 的 `wind_down` 处使用。
- **已知的实现者判断点**（不是占位符，是必须现场核对的既有 API）：`self.store` 改会话记录的既有姿势（Task 6 Step 1）、`PanelFrame` 构造点的形态（Task 6 Step 2）、`touched_store` 表达式的确切形状（Task 6 Step 3）。三处都写明了"照既有姿势、不新造路径"的约束。
