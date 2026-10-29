# 任务栏可见性(F337-a / F337 / F336)Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 窗口不可见时,远端 BEL(Claude Code 跑完)能让任务栏变橙、窗口标题带 `[!]`、对应 pane/标签挂待办圆点;标题随焦点 pane 显示项目名,多开可区分。

**Architecture:** 先让 `pump_io` 泵全部标签(F337-a),否则后台标签的 BEL 根本产生不了。BEL 由 `Emulator` 的 `EventSink` 记成一个 `AtomicBool`;`Workspace::settle_attention(viewer)` 在 `flush_expired_vt_sync` **之后**收 bell 并按「是否被看着」结算 `PaneState.attention`;App 层只决定 `viewer`(窗口焦点 × 活动标签)与要不要 `request_user_attention`。窗口标题是纯函数 `compose_window_title`,在 `about_to_wait` 现算、不同才 `set_title`。

**Tech Stack:** Rust;alacritty_terminal 0.26(`Event::Bell`)、winit 0.30.13(`Window::request_user_attention` / `has_focus` / `set_title`)、egui 0.30。

**设计依据:** `docs/superpowers/specs/2026-10-29-taskbar-attention-design.md`(含复核修订)。

**通用约定(每个 Task 都适用):**
- 注释、提交信息一律简体中文;提交摘要带 spec 编号;结尾加
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`。
- 大输出落盘再 grep:`cargo test -p mullion-app > /tmp/t.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/t.log`。
- 「绿」= `cargo test --workspace` 全过 **且** `cargo clippy --workspace --all-targets -- -D warnings` 无输出,
  最后一个 Task 统一核;中间 Task 跑到指定 crate 即可,但不许据此说「测试通过」。
- `app.rs` 约 3.6 万行,用 `grep -n` 定位,不要整文件读。下文给的行号是 2026-10-29 `main@14494df` 的,
  漂移了就按引用的代码片段重新定位。

---

## 文件结构

| 文件 | 职责 / 本次改动 |
|---|---|
| `crates/mullion-term/src/emulator.rs` | `EventSink.bell` + `Emulator::take_bell()` |
| `crates/mullion-term/tests/vt_fixtures.rs` + `tests/fixtures/tmux-client-bell.bin` | tmux 客户端流里的 BEL 真实字节 |
| `crates/mullion-app/src/shell/workspace/mod.rs` | `PaneState.attention`;`pane_is_seen` 纯函数;`Workspace::settle_attention` |
| `crates/mullion-app/src/app.rs` | `pump_all_tabs` / `background_vt_deadline` / `settle_attention_of` / `should_flash` / `pane_label` / `window_title_of` / `compose_window_title`;`pump_io`、`sync_timeout_wake`、`Focused` 臂、`about_to_wait`、窗口创建处、`TitleView`/`TabView` 构造点接线 |
| `crates/mullion-app/src/ui/pane_title.rs` | `TitleView.attention` + 圆点 |
| `crates/mullion-app/src/ui/chrome.rs` | `TabView.attention` + 圆点 |

---

### Task 1: F337-a 后台标签照常泵

**Files:**
- Modify: `crates/mullion-app/src/app.rs`(`fn pump_io` ≈10370;`fn sync_timeout_wake` ≈5011;新增两个自由函数放在 `fn sync_timeout_wake_at` ≈17726 旁边)
- Test: `crates/mullion-app/src/app.rs` 的 `mod tests`(放在 `fn second_terminal_tab` ≈27023 附近)

- [ ] **Step 1: 跑一遍守护基线(T1/T3/T7)**

```bash
cargo test -p mullion-term pty_write_is_collected > /tmp/t1.log 2>&1; grep -nE "test result|FAILED" /tmp/t1.log
cargo test -p mullion-app redraw_is_frame_capped > /tmp/t3.log 2>&1; grep -nE "test result|FAILED" /tmp/t3.log
cargo test -p mullion-app frame::tests > /tmp/t7.log 2>&1; grep -nE "test result|FAILED" /tmp/t7.log
```
Expected: 三处都 `test result: ok`,且各自至少跑了 1 条(`0 passed` 说明过滤串失效,先修过滤串)。

- [ ] **Step 2: 写失败测试**

在 `mod tests` 里 `fn second_terminal_tab` 之后加:

```rust
    /// 同 `second_terminal_tab`,但用调用方给的 `Workspace` —— 要拿
    /// `tests_support::ws_with` 的 `Probe` 观测 PtyWrite 有没有回写。
    fn terminal_tab_of(ws: Workspace) -> TabContent {
        TabContent::Terminal(Box::new(TerminalTab {
            ws,
            last_cfg: None,
            automation: Vec::new(),
            automation_template: None,
            tmux_attach: None,
            automation_status: None,
            files: Default::default(),
            sftp: None,
            sftp_host_ix: None,
            sftp_tasks: Vec::new(),
            sftp_default_remote: None,
            sftp_screenshot_dir: None,
            sftp_home: None,
            reconnect_tasks: Vec::new(),
            leaf_wanted: Vec::new(),
            leaf_detach: Vec::new(),
            pending_reveal: None,
        }))
    }

    /// **F337-a / T1**:后台标签的 pane 也要排空 rx、回写 PtyWrite。
    ///
    /// 原先 `pump_io` 只泵活动标签:后台标签里 TUI 发的光标位置查询
    /// (`CSI 6n`)永远等不到应答,BEL 也永远到不了 `Term`。
    ///
    /// 自证会变红:把 `pump_all_tabs` 的循环改回只处理 `tabs.active_mut()`。
    #[test]
    fn a_background_tab_is_pumped_and_answers_its_pty_queries() {
        use crate::shell::workspace::tests_support::ws_with;
        let (bg_ws, probes) = ws_with(1);
        let mut tabs: Tabs<TabContent> = Tabs::default();
        tabs.open("bg".into(), None, terminal_tab_of(bg_ws));
        tabs.open("fg".into(), None, second_terminal_tab(2));
        assert_eq!(tabs.active_index(), 1, "前提:后台标签不是活动标签");
        probes[0].tx.try_send(b"\x1b[6n".to_vec()).unwrap();

        pump_all_tabs(&mut tabs, 0, Instant::now());

        let writes = probes[0].writes.lock().unwrap();
        assert!(
            writes.iter().any(|w| w.starts_with(b"\x1b[")),
            "后台标签的光标查询没有被应答:{writes:?}"
        );
    }

    /// **F337-a / T3**:后台标签的同步块收口**不**让本帧标脏 —— 后台不出帧,
    /// 标脏等于为一块看不见的画面白画一帧。
    #[test]
    fn a_background_sync_flush_does_not_ask_for_a_frame() {
        use crate::shell::workspace::tests_support::ws_with;
        let (mut bg_ws, _p) = ws_with(1);
        bg_ws
            .pane_mut(PaneId(1))
            .unwrap()
            .emulator
            .feed(b"\x1b[?2026hX");
        let mut tabs: Tabs<TabContent> = Tabs::default();
        tabs.open("bg".into(), None, terminal_tab_of(bg_ws));
        tabs.open("fg".into(), None, second_terminal_tab(2));

        let later = Instant::now() + std::time::Duration::from_secs(5);
        assert!(!pump_all_tabs(&mut tabs, 0, later));
        // 但它确实收口了(否则 C2:同步块里的 BEL 永远出不来)。
        let bg = tabs.get(0).unwrap().content.as_terminal().unwrap();
        assert!(bg.ws.vt_sync_deadline().is_none(), "后台同步块没有被收口");
    }

    /// **F337-a / T2**:后台标签卡在同步块里时,`WaitUntil` 也要排到它的超时点,
    /// 否则「没有下一个入站字节」时它的 BEL 永远出不来。
    #[test]
    fn the_background_vt_deadline_is_scheduled_but_the_active_one_is_not_counted_twice() {
        use crate::shell::workspace::tests_support::ws_with;
        let (mut bg_ws, _p) = ws_with(1);
        bg_ws.pane_mut(PaneId(1)).unwrap().emulator.feed(b"\x1b[?2026h");
        let mut tabs: Tabs<TabContent> = Tabs::default();
        tabs.open("bg".into(), None, terminal_tab_of(bg_ws));
        tabs.open("fg".into(), None, second_terminal_tab(2));
        assert!(background_vt_deadline(&tabs).is_some());

        // 只有活动标签在同步块里:那份由 `sync_timeout_wake_at` 管,这里不重复算。
        let mut only_fg: Tabs<TabContent> = Tabs::default();
        let (mut fg_ws, _p2) = ws_with(1);
        fg_ws.pane_mut(PaneId(1)).unwrap().emulator.feed(b"\x1b[?2026h");
        only_fg.open("fg".into(), None, terminal_tab_of(fg_ws));
        assert!(background_vt_deadline(&only_fg).is_none());
    }

    /// **接线守护(F337-a)**:`pump_io` 走 `pump_all_tabs`,不再只泵活动标签。
    #[test]
    fn pump_io_pumps_every_tab() {
        let src = include_str!("app.rs");
        let body = src
            .split("    fn pump_io(&mut self) {")
            .nth(1)
            .expect("找不到 pump_io")
            .split("\n    }\n")
            .next()
            .unwrap();
        assert!(body.contains("pump_all_tabs(&mut self.tabs"), "{body}");
        assert!(!body.contains("ws.pump(now)"), "pump_io 又只泵活动标签了:{body}");
    }
```

- [ ] **Step 3: 跑测试确认失败**

Run: `cargo test -p mullion-app background > /tmp/t.log 2>&1; grep -nE "error\[|test result|FAILED" /tmp/t.log`
Expected: 编译错误 `cannot find function `pump_all_tabs`` / `background_vt_deadline`。

- [ ] **Step 4: 实现**

在 `fn sync_timeout_wake_at` 之前加:

```rust
/// F337-a:排空**全部**标签的 rx、喂 emulator、回写 PtyWrite(T1),并收口到点的
/// 同步块(T2)。返回**活动标签**有没有收口 —— 只有它要出帧。
///
/// 原先只泵活动标签:后台标签的 TUI 查询永远等不到应答、有界 rx 灌满堵住
/// io_task,BEL 也永远到不了 `Term`(切回去那一刻才喂进去,那时它恰好正被
/// 看着,待办直接蒸发)。后台标签的字节切回时本来也要解析,总工作量不变。
///
/// 自由函数而不是 `&mut self` 方法:`App` 在测试容器里构造不出来。
fn pump_all_tabs(tabs: &mut Tabs<TabContent>, now_ms: u64, now: Instant) -> bool {
    let active = tabs.active_index();
    let mut active_flushed = false;
    for (ix, tab) in tabs.iter_mut().enumerate() {
        let Some(t) = tab.content.as_terminal_mut() else {
            continue;
        };
        t.ws.pump(now_ms);
        // T2:超时的同步块要就地收口 —— 字节还压在 vte 的 `Processor` 里,
        // `Term` 根本没见过。见 `Emulator::flush_expired_sync`。
        let flushed = t.ws.flush_expired_vt_sync(now);
        if ix == active {
            active_flushed = flushed;
        }
    }
    active_flushed
}

/// F337-a:**非活动**标签里最早的 vte 同步块超时点。活动标签那份(连同
/// `SyncFramePacer` 的超时)由 `sync_timeout_wake_at` 管;后台不出帧,只需要
/// 到点来收口一次,否则它同步块里的 BEL 要等下一个不相关事件才出来。
fn background_vt_deadline(tabs: &Tabs<TabContent>) -> Option<Instant> {
    let active = tabs.active_index();
    tabs.iter()
        .enumerate()
        .filter(|(ix, _)| *ix != active)
        .filter_map(|(_, tab)| tab.content.as_terminal())
        .filter_map(|t| t.ws.vt_sync_deadline())
        .min()
}
```

`fn pump_io` 里把

```rust
        let now = self.now_ms();
        let mut flushed = false;
        if let Some(ws) = self.active_ws_mut() {
            ws.pump(now);
            // T2:超时的同步块要就地收口 —— 字节还压在 vte 的 `Processor` 里,
            // `Term` 根本没见过,再怎么出帧也只是旧画面。
            // 见 `Emulator::flush_expired_sync`。
            flushed = ws.flush_expired_vt_sync(Instant::now());
        }
```

换成

```rust
        let now = self.now_ms();
        // F337-a:全部标签都泵,只有活动标签的收口决定要不要出帧。
        let flushed = pump_all_tabs(&mut self.tabs, now, Instant::now());
```

`fn sync_timeout_wake` 改为:

```rust
    fn sync_timeout_wake(&self, now_ms: u64) -> Option<Instant> {
        let active = sync_timeout_wake_at(self.start, self.active_ws(), now_ms);
        // F337-a:后台标签不出帧,但同步块到点也得有人来收口。
        match (active, background_vt_deadline(&self.tabs)) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
```

- [ ] **Step 5: 跑测试确认通过,并重跑 Step 1 三组守护**

Run: `cargo test -p mullion-app background > /tmp/t.log 2>&1; grep -nE "test result|FAILED" /tmp/t.log`,
再 `cargo test -p mullion-app pump_io_pumps_every_tab`,再重跑 Step 1 三条命令。
Expected: 全部 ok。若 `sync_timeout` 相关既有测试(≈23424~23497)红了,看是不是它们断言了 `sync_timeout_wake` 的旧行为——那些测的是 `sync_timeout_wake_at`,不应受影响,红了就是改错了地方。

- [ ] **Step 6: 自证变红**

临时把 `pump_all_tabs` 循环改成只处理活动标签(`if ix != active { continue; }` 放循环第一句),跑 `a_background_tab_is_pumped_and_answers_its_pty_queries` 确认红,然后撤销。

- [ ] **Step 7: 提交**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "fix(app): 后台标签照常泵 IO,查询有应答、同步块到点收口 (F337-a)

原先 pump_io 只泵活动标签:后台标签 TUI 的光标查询无应答、有界 rx 灌满
堵住 io_task(T1 同族),BEL 切回时才喂进去、正好被看着而蒸发。
后台同步块的 vte 超时点并进 WaitUntil;后台收口不标脏(T3)。

守护:emulator::tests::pty_write_is_collected、app::tests::redraw_is_frame_capped、
frame::tests 改动前后各跑一遍均绿。

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Emulator 收 BEL(`take_bell`)

**Files:**
- Modify: `crates/mullion-term/src/emulator.rs`(`struct EventSink` ≈34;`impl EventListener` ≈41;`fn take_pty_writes` ≈378 之后加方法)
- Test: `crates/mullion-term/src/emulator.rs` 的 `mod tests`

- [ ] **Step 1: 写失败测试**

在 `emulator.rs` 的 `mod tests` 末尾加:

```rust
    /// F337:BEL 被记下,读一次即清。
    #[test]
    fn a_bell_is_taken_once() {
        let mut emu = Emulator::new(80, 24);
        assert!(!emu.take_bell());
        emu.feed(b"done\x07");
        assert!(emu.take_bell());
        assert!(!emu.take_bell(), "读完没清,同一声铃会被结算两次");
    }

    /// F337:OSC 0/2/7 的终止符也是 `\x07`,但那不是响铃 —— 每次 cd、
    /// 每次 tmux 改标题都会发一串。判据必须是 `Event::Bell`,不是扫字节。
    #[test]
    fn a_bel_that_terminates_an_osc_is_not_a_bell() {
        let mut emu = Emulator::new(80, 24);
        emu.feed(b"\x1b]2;web01:~/src\x07\x1b]7;file://h/tmp\x07");
        assert!(!emu.take_bell());
    }

    /// F337 / C2:同步块(DEC 2026)里的 BEL 被 vte 攒着,`Term` 没见过 ——
    /// 收口之前不能报,收口之后必须报。最小化态下收口是唯一能把它放出来的路。
    #[test]
    fn a_bell_inside_a_sync_block_surfaces_only_after_the_block_is_flushed() {
        let mut emu = Emulator::new(80, 24);
        emu.feed(b"\x1b[?2026h\x07");
        assert!(!emu.take_bell(), "BSU 之后的字节还在 vte 肚子里,不该已经响了");
        let later = std::time::Instant::now() + std::time::Duration::from_secs(5);
        assert!(emu.flush_expired_sync(later));
        assert!(emu.take_bell(), "同步块收口后 BEL 丢了");
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-term bell > /tmp/t.log 2>&1; grep -nE "error\[|test result|FAILED" /tmp/t.log`
Expected: `no method named `take_bell``。

- [ ] **Step 3: 实现**

`struct EventSink` 加字段(放 `title` 之后),并把文档开头「三件事」改成「四件事」、补一条:

```rust
/// - `Event::Bell` —— BEL(F337 待办)。**只认这个事件,不扫字节**:OSC 的
///   终止符也是 `\x07`,扫字节的话每次 cd / 改标题都算一声铃。
```

```rust
    /// F337:自上次 `take_bell` 以来响过铃没有。布尔而不是计数:一串铃和一声铃
    /// 对「这块 pane 有事找你」是同一个结论。
    bell: Arc<AtomicBool>,
```

文件顶部 `use std::sync::{Arc, Mutex};` 改为 `use std::sync::atomic::{AtomicBool, Ordering};` + `use std::sync::{Arc, Mutex};`(按现有 use 写法合并,别重复引入)。

`send_event` 的 match 里 `Event::ResetTitle` 臂之后、`_ => {}` 之前加:

```rust
            Event::Bell => self.bell.store(true, Ordering::Relaxed),
```

并把 `_ => {}` 上面那句注释改为 `// 其余事件(剪贴板……)本项目还不用。`

`fn take_pty_writes` 之后加:

```rust
    /// F337:自上次调用以来响过铃没有,读即清。调用方必须排在
    /// `flush_expired_sync` **之后**:同步块里的 BEL 收口时才进 `Term`。
    pub fn take_bell(&mut self) -> bool {
        self.sink.bell.swap(false, Ordering::Relaxed)
    }
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-term > /tmp/t.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/t.log`
Expected: 全部 ok(含 `pty_write_is_collected`)。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-term/src/emulator.rs
git commit -m "feat(term): 记下 BEL 供待办结算,OSC 终止符不算铃 (F337)

守护:emulator::tests::pty_write_is_collected 仍绿(T1)。

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: tmux 客户端流 BEL fixture

**Files:**
- Create: `crates/mullion-term/tests/fixtures/tmux-client-bell.bin`
- Modify: `crates/mullion-term/tests/vt_fixtures.rs`(末尾加测试)、`crates/mullion-term/tests/fixtures/README.md`(补「客户端流」录法)

- [ ] **Step 1: 录制(必须录 tmux 客户端流,pipe-pane 录不到 tmux 自己转发的 BEL)**

```bash
D=$(mktemp -d /tmp/bellrec.XXXX); S=bellrec$$
tmux -L $S -f /dev/null new-session -d -s t -x 80 -y 24 'sleep 30'
tmux -L $S new-window -d -t t: 'sleep 30'
( script -qfc "tmux -L $S attach -t t" $D/out.raw >/dev/null 2>&1 & ); sleep 1.5
P1=$(tmux -L $S display -p -t t:1 '#{pane_tty}')
printf '\a' > $P1; sleep 0.7
tmux -L $S kill-server; sleep 0.3
python3 -I -c "import sys; d=open(sys.argv[1],'rb').read(); print(len(d), d.count(b'\x07'))" $D/out.raw
cp $D/out.raw crates/mullion-term/tests/fixtures/tmux-client-bell.bin
```
Expected: 打印的 BEL 计数 ≥ 1(后台 window 的铃被 tmux 转给客户端)。
脱敏:流里只有 tmux 状态栏(会话名 `t`、主机名)。用 `grep -c "$(hostname)"` 查;命中就按 README「等长替换」把主机名换成等长的 `x`。

- [ ] **Step 2: 写测试**

`vt_fixtures.rs` 末尾加:

```rust
/// F337:tmux 把**后台 window** 的 BEL 转给客户端(`bell-action any` 默认值)。
/// 录的是 tmux **客户端**流(`script` 包 attach),不是 pipe-pane —— 后者只有应用
/// 写进 pty 的字节,看不到 tmux 自己往外发的那声铃。
///
/// 同一段流里还有 tmux 设标题的 OSC(终止符同为 `\x07`),这条同时钉住
/// 「OSC 终止符不算铃」在真实字节流上成立。
#[test]
fn tmux_forwards_a_background_window_bell_to_the_client() {
    let mut emu = Emulator::new(80, 24);
    emu.feed(&fixture_bytes("tmux-client-bell"));
    assert!(emu.take_bell(), "tmux 客户端流里的 BEL 没被认出来");
}
```

README 的录制节补一段「客户端流」:写明上面 `script -qfc "tmux attach"` 的录法,以及何时必须用它(要看 tmux 自己往外层发的字节时:BEL 转发、`CSI K` 补行等)。

- [ ] **Step 3: 跑**

Run: `cargo test -p mullion-term --test vt_fixtures > /tmp/t.log 2>&1; grep -nE "test result|FAILED" /tmp/t.log`
Expected: ok。

- [ ] **Step 4: 提交**

```bash
git add crates/mullion-term/tests
git commit -m "test(term): 录 tmux 客户端流 BEL fixture (F337)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Workspace 结算待办(`settle_attention`)

**Files:**
- Modify: `crates/mullion-app/src/shell/workspace/mod.rs`(`pub struct PaneState` ≈188 加字段;`fn flush_expired_vt_sync` ≈854 之后加方法与纯函数)
- Modify: 全部 `PaneState { .. }` 字面量(8 处:`grep -rn "history_reported: 0" crates/mullion-app/src`)
- Test: 同文件 `mod tests`(`tests_support` 下面那个测试模块;`grep -n "mod tests" shell/workspace/mod.rs` 定位)

- [ ] **Step 1: 写失败测试**

```rust
    /// F337 决策 5/7:「被看着」= 就是 viewer,或 viewer 是它的抽屉。
    #[test]
    fn a_pane_is_seen_when_it_or_its_drawer_has_the_view() {
        use super::pane_is_seen;
        let (a, b) = (PaneId(1), PaneId(2));
        assert!(pane_is_seen(a, Some(a), None));
        assert!(!pane_is_seen(b, Some(a), None));
        assert!(pane_is_seen(a, Some(b), Some(a)), "焦点在抽屉上,父 pane 上下贴着同屏");
        assert!(!pane_is_seen(a, None, None), "没有 viewer(窗口失焦/后台标签)谁都没被看着");
    }

    /// F337:没被看着的 pane 响铃 → 置待办、报 fresh;被看着的响铃 → 丢弃;
    /// 被看着时既有待办被清(不变量式清除,不挂在切焦点的入口上)。
    #[test]
    fn settle_marks_unseen_bells_drops_seen_ones_and_clears_on_view() {
        let (mut ws, _p) = ws_with(2);
        ws.pane_mut(PaneId(2)).unwrap().emulator.feed(b"\x07");
        let s = ws.settle_attention(Some(PaneId(1)));
        assert!(s.fresh && s.changed);
        assert!(ws.pane(PaneId(2)).unwrap().attention);

        ws.pane_mut(PaneId(1)).unwrap().emulator.feed(b"\x07");
        let s = ws.settle_attention(Some(PaneId(1)));
        assert!(!s.fresh && !s.changed, "正看着的 pane 响铃不算待办");
        assert!(!ws.pane(PaneId(1)).unwrap().attention);

        let s = ws.settle_attention(Some(PaneId(2)));
        assert!(!s.fresh && s.changed);
        assert!(!ws.pane(PaneId(2)).unwrap().attention, "看过去了还挂着");
    }

    /// F337 / R1:已经挂着待办的 pane 再响 → 仍报 fresh(闪烁判据是事件不是跃迁)。
    #[test]
    fn a_second_bell_on_an_already_marked_pane_is_still_fresh() {
        let (mut ws, _p) = ws_with(1);
        ws.pane_mut(PaneId(1)).unwrap().emulator.feed(b"\x07");
        assert!(ws.settle_attention(None).fresh);
        ws.pane_mut(PaneId(1)).unwrap().emulator.feed(b"\x07");
        let s = ws.settle_attention(None);
        assert!(s.fresh, "第二声铃被吞了 —— 失焦后再响就不会闪");
        assert!(!s.changed);
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app settle > /tmp/t.log 2>&1; grep -nE "error\[|test result" /tmp/t.log`
Expected: 编译错误(`pane_is_seen` / `settle_attention` / `attention` 不存在)。

- [ ] **Step 3: 实现**

`PaneState` 末尾(`notice` 之后)加:

```rust
    /// F337:这块 pane 响过铃、而你还没看过它。瞬态:不进 F37/F148 快照;
    /// 由 [`Workspace::settle_attention`] 置位与清除,别处不许写。
    pub attention: bool,
```

8 处 `PaneState { .. }` 字面量各补 `attention: false,`(`cargo build -p mullion-app --tests` 的 `missing field` 报错逐条对)。

`fn flush_expired_vt_sync` 之后加:

```rust
    /// F337:收各 pane 的 BEL 并结算待办。
    ///
    /// `viewer` 由 App 决定(窗口有焦点 **且** 这是活动标签 → `Some(焦点)`,否则 `None`)——
    /// `Workspace` 不知道窗口焦点,也不知道自己是不是活动标签(复核 R2)。
    ///
    /// **必须排在 `flush_expired_vt_sync` 之后调**:同步块里的 BEL 收口时才进 `Term`(C2)。
    /// 清除是不变量式的(每次调都按 `viewer` 重算),不挂在切焦点 / 切标签 /
    /// `Focused(true)` 三个入口上 —— 列举式门控在加入口时必然漏。
    pub fn settle_attention(&mut self, viewer: Option<PaneId>) -> Settled {
        let viewer_parent = viewer.and_then(|v| self.drawer(v)).map(|d| d.parent);
        let mut out = Settled::default();
        for p in &mut self.panes {
            let bell = p.emulator.take_bell();
            let seen = pane_is_seen(p.id, viewer, viewer_parent);
            let next = if seen { false } else { p.attention || bell };
            out.fresh |= bell && !seen;
            out.changed |= next != p.attention;
            p.attention = next;
        }
        out
    }
```

在 `impl Workspace` 之外(文件里其它自由函数旁,如 `rx_closed_action` 附近)加:

```rust
/// F337:[`Workspace::settle_attention`] 的结论。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Settled {
    /// 本轮有落在「没被看着」的 pane 上的铃(**事件**,不是待办状态的跃迁 —— 复核 R1)。
    pub fresh: bool,
    /// 有 pane 的待办状态变了(要重画标题条/标签栏)。
    pub changed: bool,
}

/// F337 决策 5/7:`id` 此刻是不是被看着。`viewer_parent` = viewer 若是抽屉,它的父 pane。
pub fn pane_is_seen(id: PaneId, viewer: Option<PaneId>, viewer_parent: Option<PaneId>) -> bool {
    viewer == Some(id) || viewer_parent == Some(id)
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app shell::workspace > /tmp/t.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/t.log`
Expected: ok。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src
git commit -m "feat(app): Workspace 按「是否被看着」结算响铃待办 (F337)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: App 接线 —— 结算、任务栏闪、焦点校准

**Files:**
- Modify: `crates/mullion-app/src/app.rs`:
  - 新自由函数放 `fn pump_all_tabs` 之后;
  - `struct App` 字段区(`window_focused: bool,` ≈2444)加 `flashed_since_blur: bool,`,构造处(`window_focused: true,` ≈3340)加 `flashed_since_blur: false,`;
  - `fn pump_io`;`WindowEvent::Focused` 臂 ≈14392;窗口创建 `self.active = Some(Active {` ≈13349 之后;
  - `use winit::window::{Window, WindowId};` 改为 `use winit::window::{UserAttentionType, Window, WindowId};`
- Test: `app.rs` `mod tests`

- [ ] **Step 1: 写失败测试**

```rust
    /// F337:只在「窗口失焦 + 这轮有没被看着的铃 + 失焦后还没闪过」时闪。
    #[test]
    fn the_taskbar_flashes_once_per_blur_for_an_unseen_bell() {
        assert!(should_flash(true, false, false));
        assert!(!should_flash(true, true, false), "前台不闪");
        assert!(!should_flash(true, false, true), "这次失焦已经闪过了");
        assert!(!should_flash(false, false, false), "没有新铃");
    }

    /// F337 决策 6:后台标签的 pane 响铃 → 置待办,即使窗口有焦点。
    /// 活动标签的焦点 pane 响铃且窗口有焦点 → 不算。
    #[test]
    fn a_bell_in_a_background_tab_is_marked_even_while_the_window_is_focused() {
        use crate::shell::workspace::tests_support::ws_with;
        let (mut bg, _p) = ws_with(1);
        bg.pane_mut(PaneId(1)).unwrap().emulator.feed(b"\x07");
        let mut tabs: Tabs<TabContent> = Tabs::default();
        tabs.open("bg".into(), None, terminal_tab_of(bg));
        tabs.open("fg".into(), None, second_terminal_tab(2));
        tabs.iter_mut()
            .nth(1)
            .unwrap()
            .content
            .as_terminal_mut()
            .unwrap()
            .ws
            .pane_mut(PaneId(1))
            .unwrap()
            .emulator
            .feed(b"\x07");

        let s = settle_attention_of(&mut tabs, true);
        assert!(s.fresh);
        assert!(tab_has_attention(tabs.get(0).unwrap()), "后台标签的铃丢了");
        assert!(!tab_has_attention(tabs.get(1).unwrap()), "正看着的 pane 被标了");
    }

    /// F337 / C2 / R9 行为式:最小化态只有 `Wake → pump_io`。同步块里的铃要在
    /// 「pump → 收口 → 结算」这一趟里出来,且窗口失焦时活动标签的焦点 pane 也算没被看着。
    #[test]
    fn a_bell_inside_an_expired_sync_block_is_settled_in_the_same_pump() {
        use crate::shell::workspace::tests_support::ws_with;
        let (mut ws, _p) = ws_with(1);
        ws.pane_mut(PaneId(1)).unwrap().emulator.feed(b"\x1b[?2026h\x07");
        let mut tabs: Tabs<TabContent> = Tabs::default();
        tabs.open("only".into(), None, terminal_tab_of(ws));

        let later = Instant::now() + std::time::Duration::from_secs(5);
        pump_all_tabs(&mut tabs, 0, later);
        let s = settle_attention_of(&mut tabs, false);
        assert!(s.fresh, "同步块收口后的铃没被结算");
        assert!(should_flash(s.fresh, false, false));
    }

    /// **接线守护(F337)**:`pump_io` 里结算排在 `pump_all_tabs` 之后(C2);
    /// 闪烁走 `should_flash`;`Focused` 臂复位 `flashed_since_blur`。
    #[test]
    fn attention_is_settled_after_the_pump_and_the_blur_flag_is_reset_on_focus() {
        let src = include_str!("app.rs");
        let src = src.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
        let body = src.split("    fn pump_io(&mut self) {").nth(1).unwrap();
        let body = body.split("\n    }\n").next().unwrap();
        let pump = body.find("pump_all_tabs(").expect("pump_io 不再调 pump_all_tabs");
        let settle = body.find("settle_attention_of(").expect("pump_io 没结算待办");
        assert!(pump < settle, "结算必须在泵(含同步块收口)之后");
        assert!(body.contains("should_flash("), "{body}");
        let arm = src.split("WindowEvent::Focused(focused) => {").nth(1).unwrap();
        let arm = arm.split("WindowEvent::Occluded").next().unwrap();
        assert!(arm.contains("self.flashed_since_blur = false;"), "{arm}");
    }
```

注:`Tabs` 没有 `get_mut`(已核实),所以用 `iter_mut().nth(1)`。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app attention > /tmp/t.log 2>&1; grep -nE "error\[|test result" /tmp/t.log`
Expected: `should_flash` / `settle_attention_of` / `tab_has_attention` 未定义。

- [ ] **Step 3: 实现自由函数**

```rust
/// F337:逐标签结算待办。`viewer` = 窗口有焦点 **且** 是活动标签 → 该标签的焦点 pane。
/// 返回各标签结论的并。
fn settle_attention_of(
    tabs: &mut Tabs<TabContent>,
    window_focused: bool,
) -> crate::shell::workspace::Settled {
    let active = tabs.active_index();
    let mut out = crate::shell::workspace::Settled::default();
    for (ix, tab) in tabs.iter_mut().enumerate() {
        let Some(t) = tab.content.as_terminal_mut() else {
            continue;
        };
        let viewer = (window_focused && ix == active).then(|| t.ws.focus());
        let s = t.ws.settle_attention(viewer);
        out.fresh |= s.fresh;
        out.changed |= s.changed;
    }
    out
}

/// F337 决策 6:标签上的待办圆点是**推导**的(标签内任一 pane 待办),不另存。
fn tab_has_attention(tab: &Tab<TabContent>) -> bool {
    tab.content
        .as_terminal()
        .is_some_and(|t| t.ws.panes().iter().any(|p| p.attention))
}

/// F337 / 复核 R1:闪烁判据是**事件**(这轮有没被看着的铃)+ 每次失焦至多一次。
/// winit 的 `Informational` = `FLASHW_TRAY | FLASHW_TIMERNOFG`,橙色持续到激活,
/// 重复调没有意义。
fn should_flash(fresh: bool, window_focused: bool, flashed_since_blur: bool) -> bool {
    fresh && !window_focused && !flashed_since_blur
}
```

(`app.rs` 顶部已 `use crate::shell::tabs::{Tab, TabPayload, Tabs};`,直接写 `Tab<TabContent>`。)

- [ ] **Step 4: 接线**

`fn pump_io` 里 `let flushed = pump_all_tabs(..);` 之后、`if flushed {` 之前加:

```rust
        // F337:结算待办。**必须在泵(含同步块收口)之后** —— 同步块里的铃
        // 收口时才进 `Term`(复核 C2)。最小化态这里是唯一会跑的地方(`Wake`)。
        let settled = settle_attention_of(&mut self.tabs, self.window_focused);
        if should_flash(settled.fresh, self.window_focused, self.flashed_since_blur) {
            if let Some(a) = &self.active {
                a.window
                    .request_user_attention(Some(UserAttentionType::Informational));
            }
            self.flashed_since_blur = true;
        }
        if settled.changed {
            // 圆点与标题要跟着变;不标脏的话这一帧被判 Idle。
            mark_ui_dirty!(self.ui_dirty);
        }
```

`WindowEvent::Focused` 臂 `if focused {` 块第一句加:

```rust
                    // F337:新的一轮失焦可以再闪一次。
                    self.flashed_since_blur = false;
```

窗口创建处 `self.active = Some(Active { .. });` 之后加:

```rust
        // F337 / 复核 R3:`window_focused` 初值写死 `true`,而多实例恢复时从未拿到
        // 焦点的窗口 winit 不补发 `Focused(false)` —— 那个实例会一直以为自己被看着。
        self.window_focused = self.active.as_ref().is_some_and(|a| a.window.has_focus());
```

- [ ] **Step 5: 跑测试确认通过 + T1/T3/T7 守护**

Run: `cargo test -p mullion-app > /tmp/t.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/t.log`,再重跑 Task 1 Step 1 三条。
Expected: 全 ok。

- [ ] **Step 6: 自证变红**

临时把 `pump_io` 里结算那段挪到 `pump_all_tabs` 之前,跑 `attention_is_settled_after_the_pump_and_the_blur_flag_is_reset_on_focus` 确认红,撤销。

- [ ] **Step 7: 提交**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): 响铃结算待办,窗口失焦时任务栏变橙 (F337)

结算排在同步块收口之后(复核 C2);闪烁按事件判、每次失焦至多一次(R1);
窗口创建后用 has_focus 校准焦点初值(R3)。
守护:emulator::tests::pty_write_is_collected、app::tests::redraw_is_frame_capped、frame::tests 均绿。

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: F336 窗口标题

**Files:**
- Modify: `crates/mullion-app/src/app.rs`(自由函数放 `fn should_flash` 之后;`struct App` 加 `last_title: String,`、构造处 `last_title: String::from("mullion"),`;`fn about_to_wait` ≈16608)
- Test: `app.rs` `mod tests`

- [ ] **Step 1: 写失败测试**

```rust
    #[test]
    fn the_window_title_names_the_focused_pane_and_counts_the_rest() {
        assert_eq!(compose_window_title(None, 0, None), "mullion");
        assert_eq!(compose_window_title(Some("项目A"), 0, None), "项目A — mullion");
        assert_eq!(compose_window_title(Some("项目A"), 2, None), "项目A (+2) — mullion");
    }

    /// 复核 R4:首个待办就是主体那块时只加 `[!] `,不重复名字。
    #[test]
    fn the_attention_prefix_names_the_pane_unless_it_is_the_main_one() {
        assert_eq!(
            compose_window_title(Some("项目A"), 2, Some(Some("项目B"))),
            "[!] 项目B · 项目A (+2) — mullion"
        );
        assert_eq!(
            compose_window_title(Some("项目A"), 2, Some(None)),
            "[!] 项目A (+2) — mullion"
        );
    }

    /// 主体 = 活动标签焦点 pane;`+N` 数整个窗口其余**真** pane(抽屉不计);
    /// 待办「首个」按标签顺序 → 树叶子顺序。
    #[test]
    fn window_title_of_reads_the_active_focus_and_the_first_marked_pane() {
        use crate::shell::workspace::tests_support::ws_with;
        let (mut bg, _p) = ws_with(2);
        bg.pane_mut(PaneId(2)).unwrap().attention = true;
        let mut tabs: Tabs<TabContent> = Tabs::default();
        tabs.open("bg".into(), None, terminal_tab_of(bg));
        tabs.open("fg".into(), None, second_terminal_tab(2));
        // 测试 pane 的 `ws.hosts` 为空、没有项目 → 主体与待办名都是 None;
        // 3 块真 pane、活动标签占 1 → (+2);待办在后台 → 只剩 `[!] `。
        assert_eq!(window_title_of(&tabs, &[]), "[!] (+2) — mullion");
    }
```

注:`Workspace::new` 的 `hosts` 为 `Vec::new()`(已核实),所以主体是 `None`。期望值写死,不要放宽成 `contains`。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app window_title > /tmp/t.log 2>&1; grep -nE "error\[|test result" /tmp/t.log`

- [ ] **Step 3: 实现**

```rust
/// F336:标题条那一套推导的窗口级版本 —— 项目名优先,否则 host 标签。
/// `host_pending` 时 `host_ix` 指着别人那台机器,不能借名字(同 `TitleView.host`)。
fn pane_label<'a>(
    ws: &'a Workspace,
    id: PaneId,
    projects: &'a [mullion_store::ProjectRecord],
) -> Option<&'a str> {
    let p = ws.pane(id)?;
    if let Some(proj) = crate::project::project_of(p.tmux.as_deref(), projects) {
        return Some(proj.name.as_str());
    }
    if p.host_pending {
        return None;
    }
    ws.hosts.get(p.host_ix).map(|h| h.label.as_str())
}

/// F336:窗口标题。`attention` = `None` 没有待办;`Some(None)` 首个待办就是主体
/// (复核 R4:不重复名字);`Some(Some(name))` 首个待办是另一块。
///
/// 只拼字符串,不碰 `App` —— 形态全在这里测。
fn compose_window_title(main: Option<&str>, others: usize, attention: Option<Option<&str>>) -> String {
    let mut s = String::new();
    match attention {
        None => {}
        Some(None) => s.push_str("[!] "),
        Some(Some(name)) => {
            s.push_str("[!] ");
            s.push_str(name);
            s.push_str(" · ");
        }
    }
    if let Some(m) = main {
        s.push_str(m);
        if others > 0 {
            s.push(' ');
        }
    }
    if others > 0 {
        s.push_str(&format!("(+{others})"));
    }
    if s.is_empty() {
        return "mullion".into();
    }
    s.push_str(" — mullion");
    s
}

/// F336:从标签表现算窗口标题。主体 = 活动标签焦点 pane;`+N` = 整窗其余真 pane
/// (抽屉不计,口径同 `Workspace::pane_count`);待办「首个」= 标签顺序 → 树叶子顺序。
fn window_title_of(tabs: &Tabs<TabContent>, projects: &[mullion_store::ProjectRecord]) -> String {
    let active = tabs.active().and_then(|t| t.content.as_terminal());
    let main_id = active.map(|t| t.ws.focus());
    let main = active.and_then(|t| pane_label(&t.ws, t.ws.focus(), projects));
    let total: usize = tabs
        .iter()
        .filter_map(|t| t.content.as_terminal())
        .map(|t| t.ws.pane_count())
        .sum();
    let others = total.saturating_sub(usize::from(active.is_some()));
    let first = tabs.iter().enumerate().find_map(|(ix, tab)| {
        let t = tab.content.as_terminal()?;
        mullion_core::layout::leaves(t.ws.tree())
            .into_iter()
            .find(|id| t.ws.pane(*id).is_some_and(|p| p.attention))
            .map(|id| (ix, id, t))
    });
    let attention = first.map(|(ix, id, t)| {
        if ix == tabs.active_index() && Some(id) == main_id {
            None
        } else {
            pane_label(&t.ws, id, projects)
        }
    });
    // 另一块待办 pane 没名字可报时(`pane_label` 为 None)是 `Some(None)`,
    // 恰好落到「只加 `[!] `」那一支 —— 不编一个名字出来。
    compose_window_title(main, others, attention)
}
```

`fn about_to_wait` 里 `self.tick_heartbeat();` 之后加:

```rust
        // F336:窗口标题。**放这里不放 `RedrawRequested`**:最小化态不出帧,
        // 而标题恰在那时最需要更新(复核 R8)。算与 `set_title` 分开、不同才调 ——
        // `set_title` 每调一次都是一次跨线程窗口消息,每次唤醒都调是 T3 那一类白烧。
        let title = window_title_of(
            &self.tabs,
            self.store.as_ref().map_or(&[], |s| s.projects()),
        );
        if title != self.last_title {
            if let Some(a) = &self.active {
                a.window.set_title(&title);
            }
            self.last_title = title;
        }
```

- [ ] **Step 4: 接线守护**

```rust
    /// **接线守护(F336)**:`about_to_wait` 里有 `window_title_of`,`set_title` 前有不等比较。
    #[test]
    fn the_window_title_is_synced_in_about_to_wait_only_when_it_changes() {
        let src = include_str!("app.rs");
        let src = src.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
        let body = src.split("    fn about_to_wait(").nth(1).unwrap();
        let body = body.split("\n    }\n").next().unwrap();
        assert!(body.contains("window_title_of("), "{body}");
        let guard = body.find("if title != self.last_title").expect("缺不等比较");
        let set = body.find(".set_title(").expect("缺 set_title");
        assert!(guard < set);
    }
```

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p mullion-app window_title > /tmp/t.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/t.log`

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): 窗口标题随焦点 pane 显示项目名与其余分屏数,待办加 [!] (F336)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: F337 UI 圆点(标题条 + 标签栏)

**Files:**
- Modify: `crates/mullion-app/src/ui/pane_title.rs`(`pub struct TitleView` 加字段;`fn show` 里 `icon_of` 之前画点 ≈741)
- Modify: `crates/mullion-app/src/ui/chrome.rs`(`pub struct TabView` 加字段;`fn one_tab` 里 `paint_icon` 之前画点 ≈418)
- Modify: `crates/mullion-app/src/app.rs`(`TitleView {` 构造点 ≈15160、`TabView {` 构造点 ≈15253)
- Modify: 测试里全部 `TitleView {`(22 处)/`TabView {`(13 处)字面量补 `attention: false,`

- [ ] **Step 1: 写失败测试(pane_title.rs `mod tests`)**

手法照抄既有的 `panes_are_markable_even_when_the_title_bar_is_hidden`(pane_title.rs ≈1078)
与 `annotate_mode_registers_the_tab_bar_and_each_tab`(chrome.rs ≈1492)。

`pane_title.rs` 的 `mod tests` 加:

```rust
    /// F337:待办圆点画出来且过 F100 登记;没有待办就不登记。
    ///
    /// 自证会变红:删掉 `show` 里 `if v.attention { .. }` 那一段。
    #[test]
    fn a_pane_with_attention_shows_and_registers_its_dot() {
        for attention in [false, true] {
            let ctx = egui::Context::default();
            crate::ui::annotate::toggle(&ctx);
            let view = TitleView {
                geom: geom_800x600_title32(1, 1.0),
                index: 1,
                project: None,
                project_icon: None,
                host: Some("dev@build-01"),
                status: PaneStatus::Live,
                focused: false,
                appearance: None,
                cwd_leaf: None,
                tmux: None,
                notice: None,
                drawer: false,
                stats: Vec::new(),
                attention,
            };
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                show(ctx, &crate::theme::MULLION_DARK, std::slice::from_ref(&view));
            });
            let paths = crate::ui::annotate::spot_paths(&ctx);
            assert_eq!(
                paths.contains(&"分屏 1/待办".to_string()),
                attention,
                "attention={attention}: {paths:?}"
            );
        }
    }
```

`chrome.rs` 的 `mod tests` 加:

```rust
    /// F337:标签上的待办圆点画出来且过 F100 登记;没有待办就不登记。
    ///
    /// 自证会变红:删掉 `one_tab` 里 `if v.attention { .. }` 那一段。
    #[test]
    fn a_tab_with_attention_shows_and_registers_its_dot() {
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(1.0);
        annotate::toggle(&ctx);
        let views = [
            TabView {
                title: "build-01",
                active: true,
                session_id: None,
                appearance: None,
                color: None,
                attention: false,
            },
            TabView {
                title: "build-02",
                active: false,
                session_id: None,
                appearance: None,
                color: None,
                attention: true,
            },
        ];
        let mut paths = Vec::new();
        // 同 `annotate_mode_registers_the_tab_bar_and_each_tab`:面板首帧 fade_in,跑两帧再取。
        for _ in 0..2 {
            let _ = ctx.run(Default::default(), |ctx| {
                tab_bar_inner(ctx, &crate::theme::MULLION_DARK, &views, true);
                paths = annotate::spot_paths(ctx);
            });
        }
        assert!(paths.iter().any(|p| p == "标签栏/标签 2/待办"), "{paths:?}");
        assert!(!paths.iter().any(|p| p == "标签栏/标签 1/待办"), "{paths:?}");
    }
```

- [ ] **Step 2: 跑测试确认失败**(`attention` 字段不存在)

- [ ] **Step 3: 实现**

`TitleView` 末尾加:

```rust
    /// F337:这块 pane 响过铃而你还没看过。画成序号前的 `warn` 色实心点。
    pub attention: bool,
```

`pane_title.rs` 的 `fn show` 里 `if let Some(icon) = icon_of(..)` 之前加:

```rust
                        // F337:待办圆点排最前 —— 它是这一条上唯一「要你做点什么」的
                        // 信号。走 `ui::icon` 自绘,不用字符(T9)。
                        if v.attention {
                            let before = ui.cursor().min;
                            crate::ui::icon::icon_inline(
                                ui,
                                crate::ui::icon::Glyph::LampLit,
                                theme::c32(t.warn),
                            );
                            let r = egui::Rect::from_min_max(before, ui.min_rect().max);
                            crate::ui::annotate::mark(ui.ctx(), format!("分屏 {}/待办", v.index), r);
                        }
```

(若 `ui.min_rect()` 量不准,改成让 `icon_inline` 返回它 allocate 的 `Rect`——那是 `icon.rs` 的签名改动,先 grep 它的调用方数量;>5 处就新开一个 `icon_inline_rect` 而不改原函数。)

`TabView` 末尾加同名字段与文档;`one_tab` 里 `if let Some(icon) = v.appearance..` 之前加同款圆点,登记名 `format!("标签栏/标签 {}/待办", ix + 1)`,颜色在有底色时用 `fg`(与图标同理:满色底上 `warn` 可能看不见),无底色时用 `t.warn`。

`app.rs` 两个构造点:

```rust
                                                    attention: ws.pane(g.id).is_some_and(|p| p.attention),
```

```rust
                                        attention: tab_has_attention(tab),
```

测试字面量逐个补 `attention: false,`(`cargo build -p mullion-app --tests` 报 `missing field` 逐条对)。

- [ ] **Step 4: 跑测试**

Run: `cargo test -p mullion-app > /tmp/t.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/t.log`
另跑 T9 守护:`cargo test -p mullion-app --test glyph_whitelist`,以及表单/标注相关:`cargo test -p mullion-app annotate`。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src
git commit -m "feat(app): 待办 pane 的标题条与所在标签挂 warn 色圆点 (F337)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 8: 全绿 + spec 收尾

- [ ] **Step 1: 全量**

```bash
cargo fmt --check
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings > /tmp/clippy.log 2>&1; tail -5 /tmp/clippy.log
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app > /tmp/win.log 2>&1; tail -3 /tmp/win.log
```
Expected: 无 FAILED;clippy 无 warning;Windows 目标编得过(`request_user_attention`/`has_focus` 在 Windows 路径真实生效,本机 Linux 编不到那段平台代码)。

- [ ] **Step 2: spec.md 标注已实现**

在 `spec.md` 的 F336 / F337 行验收标准末尾追加「**已实现(v0.1.136)**:…」一句,列出实际守护测试名(本计划里的测试名)。版本号以发版时为准。

- [ ] **Step 3: 提交,之后走 `release-windows` skill 发版**

人工验收清单(写进 Release notes,来自设计文档):
1. 远端 `~/.claude/settings.json` 已配 `"preferredNotifChannel": "terminal_bell"`。
2. 最小化 / 被遮挡时远端 `printf '\a'`:任务栏按钮变橙并保持;激活后停。
3. 前台时焦点 pane 响铃:不闪、无圆点;非焦点 pane / 后台标签响铃:出现圆点,看过去即消失。
4. 任务栏悬停缩略图 / Alt+Tab 里标题为 `项目名 (+N) — mullion`,有待办时带 `[!]`。
5. 后台标签里跑 Claude Code 跑完:切回前就能看到标签圆点(F337-a)。
6. 已知限制:Tab 补全铃在你 Alt+Tab 之后到达会闪一次;同 tmux 会话其它 window 的铃也会进这块 pane;文件对话框开着时会闪。
