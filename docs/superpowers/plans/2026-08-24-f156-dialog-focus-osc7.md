# F156 实现计划：恢复弹窗关闭入口 · 换节点后焦点跟随 · 非 tmux 的 shell OSC 7 自举

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 给 F148 恢复弹窗补一个关闭入口、让换节点成功后焦点落到那块 pane、并在非 tmux 场景下往远端 shell 注入一次 OSC 7 上报,让 `Ctrl+Shift+B` 的目录继承有数据可用。

**Architecture:** 三件事互相独立,共用一次发版。F156-a 只动 `ui/history.rs`(egui 自带 close button + Esc,复用既有的 `HistoryOut::Dismiss`)。F156-b 只在自由函数 `rehost_pane` 末尾加一句 `ws.set_focus(id)`,并用一条对照测试钉住 `reattach_pane` **不许**跟着改。F156-c 新增纯逻辑模块 `shell_bootstrap.rs`(注入串常量 + 生成函数,零 IO、零 async),把 `ConnectOk`/`PaneOpened`/`PaneRehosted` 三处「pane 挂上了、拿到写口」收成一个 `App::on_pane_ready`,在里面先注入、再起登录后自动化。

**Tech Stack:** Rust workspace(`mullion-app` / `mullion-store` / `mullion-term`)、egui 0.30、accesskit(测试里用来定位无文字控件)、真 bash(live 测试)。

**设计出处:** `docs/superpowers/specs/2026-08-24-f156-dialog-focus-osc7-design.md`。**动手前先读一遍那份 spec 的「关键取舍」三节** —— 这里的每个决定都在那里有理由,不要在实现时自行改口。

**通读一遍再动手的领域约束:**
- 依赖方向 `app → {core, term, ssh, store}`,反向绝对不允许。
- T9(字形白名单):本片新增的所有 UI 字符串只能是 ASCII + 汉字 + 中文标点。`×` **不自绘**,用 egui 自带的 close button(它是 `line_segment` 画的,不进白名单)。
- 「绿」= `cargo test --workspace` 全过 **且** `cargo clippy --workspace --all-targets -- -D warnings` 无输出。只跑单个 crate 不算绿。
- 大输出先落盘再 grep:`cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log`。

---

## 文件清单

| 文件 | 责任 | 动作 |
|---|---|---|
| `crates/mullion-app/src/ui/history.rs` | F148 恢复弹窗的绘制与出口 | 改 `show()` + 加 2 条测试 + 1 个测试脚手架 |
| `crates/mullion-app/src/app.rs` | 事件循环、pane 生命周期 | `rehost_pane` 加一句;新增 `on_pane_ready`;改 3 处调用点;`take_settings_draft` 加一行;加 4 条测试 |
| `crates/mullion-app/src/shell_bootstrap.rs` | **新建**。OSC 7 注入串(纯逻辑) | 新建 |
| `crates/mullion-app/src/lib.rs` | 模块登记 | 加一行 `pub mod shell_bootstrap;` |
| `crates/mullion-app/tests/shell_osc7_live.rs` | **新建**。拿真 bash 验注入串 | 新建 |
| `crates/mullion-store/src/settings.rs` | 落盘设置 | 加 `shell_osc7_bootstrap` 字段 + 2 条测试 |
| `crates/mullion-app/src/ui/settings.rs` | 设置弹窗 | 草稿加字段、远端分节加第二个勾选框 + 2 条测试 |
| `docs/remote-state-setup.md` | 远端状态上报的运行手册 | 改掉过时的一句 + 补 F156-c |
| `spec.md` | 需求编号表 | 加 F156-a/b/c 三行 |
| `Cargo.toml` | 版本号 | `0.1.65` → `0.1.66` |

**任务顺序即依赖顺序**:Task 1(F156-a)、Task 2(F156-b)彼此独立且不依赖任何新类型,先做完先提交。Task 3~8 是 F156-c 的一条链:模块 → live 验证 → 落盘字段 → 设置 UI → 接线。Task 9 收尾文档,Task 10 发版。

---

## Task 1:F156-a —— 弹窗标题栏加 × 与 Esc

**Files:**
- Modify: `crates/mullion-app/src/ui/history.rs:116-211`(`show`)
- Test: `crates/mullion-app/src/ui/history.rs` 的 `mod tests`(同文件)

- [ ] **Step 1:先写会失败的测试(点 ×)**

在 `crates/mullion-app/src/ui/history.rs` 的 `mod tests` 里,紧跟在既有的 `fn click_row(...)` 之后,加这个脚手架和第一条测试:

```rust
    /// 点标题栏右上角的 ×。
    ///
    /// **不能用上面的 `click(label)`** —— 那个靠找 `Shape::Text` 定位,而 egui
    /// 的 close button 是两条 `line_segment` 画出来的,树里根本没有文字。
    /// 改从本帧的 accesskit 树里按 egui 给它登记的 label 取 rect
    /// (`egui/src/containers/window.rs` 的 `close_button`:
    /// `WidgetInfo::labeled(WidgetType::Button, .., "Close window")`)。
    ///
    /// 取不到就 panic 并把树里所有 label 打出来:egui 换版本改了这个 label
    /// 的话,这条测试要**当场报出来**,而不是静默点到别处、变成一条恒绿。
    fn click_close_x(draft: &mut Option<HistoryDraft>) -> Option<HistoryOut> {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        // 开了才构树(egui 的 `accesskit` feature 已由 `mullion-app` 打开)。
        ctx.enable_accesskit();
        // 两帧:`egui::Window` 首帧 `fade_in`,几何还没落定(同 `texts` 的说明)。
        // 开着 accesskit 时**每帧都会**产出一棵完整的树,所以取最后一帧那棵。
        let mut update = None;
        for _ in 0..2 {
            let mut full = ctx.run(egui::RawInput::default(), |ctx| {
                show(ctx, &t, draft);
            });
            update = full.platform_output.accesskit_update.take();
        }
        let nodes = update.expect("开了 accesskit 却没有产出树").nodes;
        let labels: Vec<String> = nodes
            .iter()
            .filter_map(|(_, n)| n.label().map(str::to_string))
            .collect();
        let b = nodes
            .iter()
            .find(|(_, n)| n.label() == Some("Close window"))
            .and_then(|(_, n)| n.bounds())
            .unwrap_or_else(|| {
                panic!("accesskit 树里没有关闭按钮;树里现有的 label:{labels:?}")
            });
        let pos = egui::pos2(
            ((b.x0 + b.x1) / 2.0) as f32,
            ((b.y0 + b.y1) / 2.0) as f32,
        );
        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::PointerMoved(pos));
        for pressed in [true, false] {
            input.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            });
        }
        let mut out = None;
        let _ = ctx.run(input, |ctx| {
            out = show(ctx, &t, draft);
        });
        out
    }

    /// F156-a:用户报的是「这个弹窗看起来关不掉」—— 底部虽然有「不恢复」,
    /// 但弹窗右上角没有 ×,而那是所有人找出口的第一个地方。
    ///
    /// 回报的是**既有的** `Dismiss`,不新增出口变体:`app.rs` 那侧
    /// 「无论恢复还是不恢复都把弹窗收掉」的处置一行都不用动。
    ///
    /// 自证会变红:把 `show` 里的 `.open(&mut open)` 去掉
    /// (树里就没有那个按钮了,脚手架的 panic 会打出实际的 label 列表)。
    #[test]
    fn closing_the_window_with_the_title_bar_x_reports_dismiss() {
        let mut draft = Some(HistoryDraft::new(rows()));
        assert_eq!(click_close_x(&mut draft), Some(HistoryOut::Dismiss));
    }
```

- [ ] **Step 2:跑它,确认是红的**

```bash
cargo test -p mullion-app --lib ui::history::tests::closing_the_window_with_the_title_bar_x_reports_dismiss 2>&1 | tail -20
```
预期:FAIL,panic 信息里是「accesskit 树里没有关闭按钮;树里现有的 label:[…]」(此刻还没挂 `.open()`)。

- [ ] **Step 3:改 `show`,让 × 出现并回报 `Dismiss`**

把 `crates/mullion-app/src/ui/history.rs:121-126` 这一段:

```rust
    let d = draft.as_mut()?;
    let mut out = None;
    egui::Window::new("恢复上次的现场")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
```

改成:

```rust
    let d = draft.as_mut()?;
    let mut out = None;
    // F156-a:`.open()` 就是标题栏右上角那个 ×。**用 egui 自带的、不自绘一个
    // `×` 字符** —— 它是 `line_segment` 画的,不碰 T9 的字形白名单
    // (`tests/glyph_whitelist.rs`);自绘的话得往 `ui::glyphs::VERIFIED` 里
    // 登记一个系统本来就提供的控件,不划算。
    //
    // × 与底部的「不恢复」并存:后者是键盘路径的出口(Tab 够得到),
    // 前者是鼠标路径的直觉位置。删掉任一个都会让某一类用户找不到出口。
    let mut open = true;
    egui::Window::new("恢复上次的现场")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
```

再把函数末尾的 `crates/mullion-app/src/ui/history.rs:210` 这一行:

```rust
    out
}
```

改成:

```rust
    // F156-a:× 和 Esc 同一个出口,都回报既有的 `Dismiss`。
    //
    // `get_or_insert` 而不是直接赋值:同一帧里既点了某一行、又把窗关掉,在
    // 物理上不可能,但让「先发生的结论优先」是显式的,比依赖那个不可能性稳。
    //
    // Esc 直接读 `ctx`:这个弹窗里没有文本框,不需要
    // `session_manager::keys::scan` 那套 `typing` 让位逻辑。
    if !open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        out.get_or_insert(HistoryOut::Dismiss);
    }
    out
}
```

- [ ] **Step 4:跑,确认变绿**

```bash
cargo test -p mullion-app --lib ui::history::tests::closing_the_window_with_the_title_bar_x_reports_dismiss 2>&1 | tail -5
```
预期:`test result: ok. 1 passed`。

- [ ] **Step 5:写 Esc 的测试**

在刚才那条测试之后加:

```rust
    /// F156-a:Esc 也是出口。× 只照顾鼠标,而这个弹窗是**启动时**弹的 ——
    /// 用户此刻手还在键盘上。
    ///
    /// 自证会变红:把 `show` 末尾那个 `key_pressed(Escape)` 分支删掉。
    #[test]
    fn pressing_escape_closes_the_dialog() {
        let mut draft = Some(HistoryDraft::new(rows()));
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        // 两帧预热,理由同 `texts`。
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                show(ctx, &t, &mut draft);
            });
        }
        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        });
        let mut out = None;
        let _ = ctx.run(input, |ctx| {
            out = show(ctx, &t, &mut draft);
        });
        assert_eq!(out, Some(HistoryOut::Dismiss));
    }
```

- [ ] **Step 6:跑整个模块,确认既有测试没被 × 挤坏**

```bash
cargo test -p mullion-app --lib ui::history 2>&1 | tail -5
```
预期:全绿(含既有的 `dismissing_reports_dismiss`、`clicking_a_row_restores_that_record_right_away`、`merely_showing_the_dialog_restores_nothing`)。

**特别注意 `merely_showing_the_dialog_restores_nothing`**:它跑了两帧空输入。`.open()` 挂上之后,只要没人点 × 也没人按 Esc,`open` 恒为 `true`,`out` 必须仍是 `None`。这条要是红了,说明 `if !open` 的判据写反了。

- [ ] **Step 7:变异验证(两条,逐条做)**

**先提交暂存区之外的东西之前不要跑这一步** —— 变异验证要改源码再改回来,历史上两次被 `git checkout` 吞掉未提交的编辑。**先 `git add -A` 把本任务的改动 stage 住**,再做变异:

```bash
git add -A
```

变异 1:把 `.open(&mut open)` 那行注释掉 → 跑 `cargo test -p mullion-app --lib ui::history`,预期 `closing_the_window_with_the_title_bar_x_reports_dismiss` 红。改回来。

变异 2:把 `if !open || ctx.input(...)` 改成 `if !open`  → 预期 `pressing_escape_closes_the_dialog` 红。改回来。

改回来之后跑一次 `git diff` 确认工作区干净(与暂存区一致)。

- [ ] **Step 8:提交**

```bash
git add crates/mullion-app/src/ui/history.rs
git commit -m "feat(app): 恢复现场弹窗加标题栏 × 与 Esc 出口 (F156-a)

× 用 egui 自带的 close button,不自绘 —— 它是 line_segment 画的,
不进 T9 字形白名单。两个入口都回报既有的 HistoryOut::Dismiss,
app.rs 侧的处置零改动。

守护:closing_the_window_with_the_title_bar_x_reports_dismiss /
pressing_escape_closes_the_dialog"
```

---

## Task 2:F156-b —— 换节点成功后焦点跟到那块 pane

**Files:**
- Modify: `crates/mullion-app/src/app.rs:8881-8914`(`rehost_pane`)
- Test: `crates/mullion-app/src/app.rs` 的 `mod tests`(同文件,放在既有的 `reattach_keeps_the_screen_but_rehost_wipes_it` 之后)

- [ ] **Step 1:先写会失败的对照测试**

在 `crates/mullion-app/src/app.rs` 的 `mod tests` 里,紧跟在 `fn reattach_keeps_the_screen_but_rehost_wipes_it` 之后加:

```rust
    /// F156-b:换节点成功后,分屏焦点跟到**那块 pane**;断线重连**绝不**跟。
    ///
    /// 两个函数长得很像,但语义相反:
    /// - 换节点是用户**刚刚**在标题条上亲手指定的,下一步必然是往新节点里
    ///   敲东西,焦点不跟过去他得再点一下。
    /// - 断线重连是后台自愈,可能发生在用户正在**另一块** pane 里打字的任意
    ///   时刻。抢焦点等于把他正在打的字发到另一台机器上去。
    ///
    /// 这条差异只写注释拦不住下一次「顺手把这两个函数统一一下」的重构,
    /// 所以拿一条**对照**测试钉住,而不是两条各测各的。
    ///
    /// 自证会变红:
    /// - 删掉 `rehost_pane` 里的 `ws.set_focus(id);` → 第 3 条断言红
    /// - 往 `reattach_pane` 里也加一句 `ws.set_focus(id);` → 第 2 条断言红
    #[test]
    fn rehosting_moves_the_focus_to_that_pane_but_reattaching_never_does() {
        use crate::shell::workspace::tests_support::{fresh_pipe, ws_with};
        let (mut ws, _probes) = ws_with(2);
        let generation = ws.generation();
        ws.set_focus(PaneId(1));
        assert_eq!(
            ws.focus(),
            PaneId(1),
            "脚手架的起始焦点就不在 1 号,下面两条断言分不出对错"
        );

        // 断线重连 2 号:焦点不动。
        let (pty, rx) = fresh_pipe();
        assert!(reattach_pane(&mut ws, PaneId(2), generation, 0, pty, rx));
        assert_eq!(
            ws.focus(),
            PaneId(1),
            "后台自愈把焦点从用户正在用的 pane 抢走了"
        );

        // 换节点 2 号:焦点跟过去。
        let (pty, rx) = fresh_pipe();
        assert!(rehost_pane(
            &mut ws,
            PaneId(2),
            generation,
            0,
            pty,
            rx,
            mullion_term::emulator::Emulator::DEFAULT_HISTORY,
        ));
        assert_eq!(
            ws.focus(),
            PaneId(2),
            "换完节点焦点还留在原来那块 pane 上,用户得再点一下才能打字"
        );
    }
```

- [ ] **Step 2:跑它,确认是红的**

```bash
cargo test -p mullion-app --lib app::tests::rehosting_moves_the_focus_to_that_pane_but_reattaching_never_does 2>&1 | tail -20
```
预期:FAIL,最后一条断言 `left: PaneId(1), right: PaneId(2)`,消息是「换完节点焦点还留在原来那块 pane 上」。

- [ ] **Step 3:改 `rehost_pane`**

在 `crates/mullion-app/src/app.rs` 的 `rehost_pane` 里,把:

```rust
    swap_pane_channel(p, host_ix, pty, rx);
    true
}
```

改成:

```rust
    swap_pane_channel(p, host_ix, pty, rx);
    // F156-b:焦点跟到这块 pane。用户刚在标题条上亲手指定了新节点,下一步
    // 必然是往它里面敲东西。
    //
    // **放在这个自由函数里、不放事件分支里**:这里能拿真实构造的 `Workspace`
    // 直接断言 `ws.focus()`;放事件分支只能写「读 `app.rs` 源码找字符串」式的
    // 断言,那是本项目反复踩到的恒绿模式。
    //
    // `reattach_pane`(F128 断线自动重连)**刻意不跟着改**,理由见
    // `rehosting_moves_the_focus_to_that_pane_but_reattaching_never_does`。
    //
    // 只动分屏焦点,不动 egui 的输入焦点:此刻输入焦点若在文件侧栏,本片不
    // 把它抢回终端(那是另一类语义,用户没要)。
    ws.set_focus(id);
    true
}
```

同时在 `rehost_pane` 的文档注释末尾(`/// 纯函数(只碰 `&mut Workspace`)…` 那句之前)补一句:

```rust
/// 成功时顺带把**分屏焦点**设到这块 pane(F156-b)。失败路径不设 —— 开头的
/// `pane_still_wanted` 早退挡在前面(`set_focus` 自己也有成员校验,但让早退
/// 先挡住,语义更清楚)。
```

- [ ] **Step 4:跑,确认变绿,而且既有的 rehost 测试没被带坏**

```bash
cargo test -p mullion-app --lib app::tests::rehost 2>&1 | tail -10
cargo test -p mullion-app --lib app::tests::reattach 2>&1 | tail -10
```
预期:两条命令都全绿,含既有的 `rehosting_a_pane_repoints_it_and_wipes_the_old_hosts_screen`、`rehosting_a_pane_that_is_gone_is_refused`、`reattach_keeps_the_screen_but_rehost_wipes_it`。

- [ ] **Step 5:变异验证**

```bash
git add -A
```

变异 1:删掉 `ws.set_focus(id);` → 预期新测试的第 3 条断言红。改回来。
变异 2:在 `reattach_pane` 的 `swap_pane_channel(...)` 之后加一句 `ws.set_focus(id);` → 预期新测试的第 2 条断言红。**改回来**(这条一定要改回来,留着就是把 F128 的语义破坏掉)。

改完 `git diff` 确认工作区与暂存区一致。

- [ ] **Step 6:提交**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): 换节点成功后分屏焦点跟到那块 pane (F156-b)

改在自由函数 rehost_pane 里(能直接断言 ws.focus(),不必写读源码的恒绿
断言)。reattach_pane 刻意不跟着改 —— 断线重连是后台自愈,抢焦点等于把
用户正在打的字发到另一台机器上。

守护:rehosting_moves_the_focus_to_that_pane_but_reattaching_never_does
(对照测试,两个方向各有一条变异命中)"
```

---

## Task 3:F156-c(1/6)—— 新建 `shell_bootstrap.rs` 纯逻辑模块

**Files:**
- Create: `crates/mullion-app/src/shell_bootstrap.rs`
- Modify: `crates/mullion-app/src/lib.rs`(模块登记)

- [ ] **Step 1:建文件(含实现与测试一起写)**

这个模块只有一个常量和一个函数,没有分支逻辑可以「先红后绿」—— 直接连测试一起落,下一步跑测试确认判据成立,再靠 Step 4 的变异验证证明测试不是恒绿。

新建 `crates/mullion-app/src/shell_bootstrap.rs`:

```rust
//! F156-c:非 tmux 场景的 shell OSC 7 自举 —— 纯逻辑。零 IO、零 async,
//! 真正往 PTY 写在 `app.rs` 的 `App::on_pane_ready`。
//!
//! 为什么需要:非 tmux 时 `PaneState.cwd` 一条腿都没有 ——
//! Ubuntu 的 bash **默认不发 OSC 7**,而「窗口标题」那条腿只要 PS1 被
//! starship / oh-my-bash / 自定义 rc 接管就断。用户报的
//! 「`Ctrl+Shift+B` 经常留在 `~`」就是这个。tmux 场景能跟住,是因为 F124
//! 把 `#{pane_current_path}` 塞进了 tmux **自己**发的标题,绕开了 shell。
//!
//! 这是 F124 的 shell 版:pane 的 shell channel 一建立就往 PTY 写一次,
//! 让远端 shell 从此每个提示符发一次 OSC 7。**不写远端任何文件** ——
//! 这条命令只活在这条 shell 的内存里,断开即消失。那正是它能默认开启、
//! 而「往 `~/.bashrc` 追加」不能的原因。
//!
//! 与 `remote_bootstrap`(F124)同构,但两者是**两个独立开关**:那个改的是
//! 远端 tmux 服务器内存里的全局选项,这个往用户当前这条 shell 里写命令
//! 并清屏。副作用不同,想只关掉其中一件是合理诉求。

/// 注入给远端 shell 的那一行(**不含结尾换行**,由 [`osc7_setup_line`] 补)。
///
/// 逐处的理由:
/// - **前导一个空格**:Ubuntu 默认 `HISTCONTROL=ignoreboth`(含 `ignorespace`),
///   这条就不进 shell history。不是所有发行版都这么配,所以它是**尽力而为**,
///   不是保证。
/// - **`printf '...%s...' "$PWD"` 而不是把 `$PWD` 拼进格式串**:目录名含 `%`
///   时会被 printf 当格式符吃掉,吐出一条**错的绝对路径** —— 而错的绝对路径
///   骗得过下游所有「是不是绝对路径」的校验,会把 SFTP 面板带到一个不存在的
///   目录去。这一条由 `tests/shell_osc7_live.rs` 拿真 bash 钉住。
/// - **主机名段留空**(`file:///path`):`parse_osc7` 本来就忽略主机名段
///   (它在 tmux/容器里经常是错的)。留空省掉 `$HOSTNAME`(bash)与 `$HOST`(zsh)
///   的差异,注入串短一截、少一处能出错的地方。
/// - **`${PROMPT_COMMAND:+;$PROMPT_COMMAND}` 保留用户原有的**:直接覆盖的话
///   会把用户自己那条(很可能正是发窗口标题的那条,也就是 F123 的另一条腿)
///   一起干掉,换成净负收益。
/// - **函数名带 `__mullion_` 前缀**:双下划线开头 + 项目名,撞用户已有函数的
///   概率可以忽略;真撞了也是覆盖我们自己的,不会破坏用户的 shell。
/// - **末尾 `clear`**:用户拍板要清屏。代价是 motd / 登录横幅一起被清掉。
///
/// 已知限制(进人工验收清单):fish / csh 下这一行是语法错误,屏幕上会打一行
/// 报错(fish 3.x 起本来就默认发 OSC 7,不做兼容,用户可以关掉开关);tmux
/// 场景无效但无害(tmux 吃掉内层 OSC 7 不转发,那个场景走 F124 那条腿);
/// 注入只发生在 pane 建立那一刻,用户之后在 pane 里 `ssh` 到第三台机器,
/// `PROMPT_COMMAND` 不会跟过去。
pub const OSC7_SETUP: &str = r#" __mullion_osc7() { printf '\033]7;file://%s\033\\' "$PWD"; }; if [ -n "$BASH_VERSION" ]; then PROMPT_COMMAND="__mullion_osc7${PROMPT_COMMAND:+;$PROMPT_COMMAND}"; elif [ -n "$ZSH_VERSION" ]; then precmd_functions+=(__mullion_osc7); fi; clear"#;

/// 真正写进 PTY 的字节:注入串 + 一个换行。
///
/// **换行不能省** —— 少了它这条命令只是躺在提示符上没有回车,用户敲的下一个
/// 字符会直接接在它后面,屏幕上出现一行莫名其妙的长命令。
pub fn osc7_setup_line() -> Vec<u8> {
    format!("{OSC7_SETUP}\n").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 没有换行 = 这条命令永远不会被执行,只是躺在提示符上,然后跟用户敲的
    /// 下一个字符拼成一行乱码。
    ///
    /// 自证会变红:把 `format!("{OSC7_SETUP}\n")` 里的 `\n` 去掉。
    #[test]
    fn the_line_ends_with_a_newline_so_the_shell_actually_runs_it() {
        let line = String::from_utf8(osc7_setup_line()).expect("注入串是 ASCII");
        assert!(line.ends_with('\n'), "没有换行,这条命令不会被执行:{line:?}");
        assert_eq!(line.matches('\n').count(), 1, "多了换行会多跑一个空提示符");
    }

    /// 前导空格是 `HISTCONTROL=ignorespace` 的钩子 —— 没有它,用户按一下 ↑
    /// 就是我们塞进去的这一长串,而他自己上一条命令被挤到第二格。
    ///
    /// 自证会变红:把 `OSC7_SETUP` 开头那个空格删掉。
    #[test]
    fn the_line_starts_with_a_space_so_it_stays_out_of_shell_history() {
        assert!(
            OSC7_SETUP.starts_with(' '),
            "少了前导空格,这条会进用户的 shell history:{OSC7_SETUP:?}"
        );
    }

    /// `$PWD` 必须当**参数**传给 printf,不能拼进格式串。
    ///
    /// 拼进去的话,目录名里的 `%` 会被 printf 当格式符吃掉,吐出一条错的
    /// **绝对路径** —— 而错的绝对路径骗得过下游所有「是不是绝对路径」的校验,
    /// SFTP 面板会被带到一个不存在的目录去(比不继承更糟)。
    ///
    /// 整条链路(Rust 字面量 → shell 单引号 → printf 格式串)由
    /// `tests/shell_osc7_live.rs` 拿真 bash 验;这里只钉形状。
    ///
    /// 自证会变红:把 `'...%s...' "$PWD"` 改成 `"...$PWD..."`。
    #[test]
    fn the_pwd_is_an_argument_not_spliced_into_the_format_string() {
        assert!(
            OSC7_SETUP.contains(r#"printf '\033]7;file://%s\033\\' "$PWD""#),
            "printf 的写法变了,目录名含 % 时会吐出一条错的绝对路径:{OSC7_SETUP:?}"
        );
    }

    /// 主机名段留空(`file://` 紧跟 `%s`,而 `%s` 展开出来的绝对路径自带
    /// 开头的 `/`,凑成 `file:///path`)。`parse_osc7` 忽略主机名段,拿
    /// `$HOSTNAME`/`$HOST` 去填只会多一处 bash/zsh 的差异。
    ///
    /// 自证会变红:把 `file://%s` 改成 `file://$HOSTNAME%s`。
    #[test]
    fn the_hostname_segment_is_left_empty() {
        assert!(OSC7_SETUP.contains("file://%s"), "{OSC7_SETUP:?}");
        assert!(
            !OSC7_SETUP.contains("HOSTNAME") && !OSC7_SETUP.contains("$HOST"),
            "别去填主机名段,那是 bash/zsh 变量名不一样的又一处坑:{OSC7_SETUP:?}"
        );
    }

    /// 用户原有的 `PROMPT_COMMAND` 必须保留。直接覆盖的话,会把他自己那条
    /// (很可能正是发窗口标题的那条,也就是 F123 的另一条腿)一起干掉 ——
    /// 那样我们补上一条腿、砍掉另一条,净收益可能是负的。
    ///
    /// 自证会变红:把 `"__mullion_osc7${PROMPT_COMMAND:+;$PROMPT_COMMAND}"`
    /// 改成 `"__mullion_osc7"`。
    #[test]
    fn the_users_own_prompt_command_is_kept() {
        assert!(
            OSC7_SETUP.contains("${PROMPT_COMMAND:+;$PROMPT_COMMAND}"),
            "会把用户自己的 PROMPT_COMMAND 覆盖掉:{OSC7_SETUP:?}"
        );
    }

    /// bash 与 zsh 各有一条分支 —— zsh 里 `PROMPT_COMMAND` 不是钩子,
    /// 只走 bash 那条的话 zsh 用户什么都收不到,而且不报错。
    ///
    /// 自证会变红:把 `elif [ -n "$ZSH_VERSION" ]` 那一整段删掉。
    #[test]
    fn both_bash_and_zsh_get_a_branch() {
        assert!(OSC7_SETUP.contains("$BASH_VERSION"), "{OSC7_SETUP:?}");
        assert!(OSC7_SETUP.contains("$ZSH_VERSION"), "{OSC7_SETUP:?}");
        assert!(
            OSC7_SETUP.contains("precmd_functions+=(__mullion_osc7)"),
            "zsh 那条没挂进 precmd_functions:{OSC7_SETUP:?}"
        );
    }

    /// 末尾清屏(用户拍板)。少了它,屏幕上会永久留着我们塞进去的这一长串。
    ///
    /// 自证会变红:把结尾的 `; clear` 删掉。
    #[test]
    fn the_line_clears_the_screen_when_it_is_done() {
        assert!(OSC7_SETUP.ends_with("; clear"), "{OSC7_SETUP:?}");
    }

    /// 整条注入串必须是 ASCII:它要穿过 PTY 直接进远端 shell,而远端的
    /// locale 我们不知道。非 ASCII 在 `LANG=C` 的机器上会变成一串问号,
    /// 而那时它已经是一条**语法不同**的命令了。
    #[test]
    fn the_whole_line_is_ascii() {
        assert!(OSC7_SETUP.is_ascii(), "{OSC7_SETUP:?}");
    }
}
```

- [ ] **Step 2:登记模块**

在 `crates/mullion-app/src/lib.rs` 里,`pub mod session_pump;` 与 `pub mod shell;` 之间(按字母序)插入:

```rust
pub mod shell_bootstrap;
```

注意:`shell` 排在 `shell_bootstrap` 前面(`shell` 是 `shell_bootstrap` 的前缀)。最终顺序是 `session_pump` → `shell` → `shell_bootstrap` → `text`。

- [ ] **Step 3:跑测试**

```bash
cargo test -p mullion-app --lib shell_bootstrap 2>&1 | tail -5
```
预期:`test result: ok. 8 passed`。

- [ ] **Step 4:变异验证(挑三条最容易被「顺手改好看」的)**

```bash
git add -A
```

变异 1:把 `printf '\033]7;file://%s\033\\' "$PWD"` 改成 `printf "\033]7;file://$PWD\033\\\\"` → 预期 `the_pwd_is_an_argument_not_spliced_into_the_format_string` 红。改回来。
变异 2:把 `${PROMPT_COMMAND:+;$PROMPT_COMMAND}` 删掉 → 预期 `the_users_own_prompt_command_is_kept` 红。改回来。
变异 3:去掉 `format!` 里的 `\n` → 预期 `the_line_ends_with_a_newline_so_the_shell_actually_runs_it` 红。改回来。

- [ ] **Step 5:提交**

```bash
git add crates/mullion-app/src/shell_bootstrap.rs crates/mullion-app/src/lib.rs
git commit -m "feat(app): OSC 7 注入串(纯逻辑模块) (F156-c)

非 tmux 场景下 PaneState.cwd 一条腿都没有 —— bash 默认不发 OSC 7,
窗口标题那条腿只要 PS1 被接管就断。这个模块只放命令串与生成函数,
零 IO、零 async;往 PTY 写在下一个任务里接。

不写远端任何文件:命令只活在这条 shell 的内存里,断开即消失。"
```

---

## Task 4:F156-c(2/6)—— 拿真 bash 验注入串(live 测试)

这一步验的是整条链路**最容易错的一环**:转义要在「Rust 字面量 → shell 单引号 → printf 格式串」三层之间穿过去,任何一层漏一个反斜杠,远端都只会安静地什么都不发 —— 没有报错、没有日志,只有 SFTP 面板停在 `~`。上一个任务的单元测试只钉了字符串的形状,证明不了它在真 shell 里跑得通。

**Files:**
- Create: `crates/mullion-app/tests/shell_osc7_live.rs`

- [ ] **Step 1:确认开发机上有 bash**

```bash
bash --version | head -1
```
预期:打印出 GNU bash 的版本。有,所以这个测试**不加 `#[ignore]`**,进常规 `cargo test --workspace`。

- [ ] **Step 2:写测试**

新建 `crates/mullion-app/tests/shell_osc7_live.rs`:

```rust
//! F156-c:拿**真的 bash** 跑一遍注入串,再把它吐出来的字节喂给
//! `mullion_term` 的 OSC 7 解析,断言解出来的路径就是那个 `$PWD`。
//!
//! 这条测试验的是整条链路最容易错的一环:转义要在「Rust 字面量 → shell
//! 单引号 → printf 格式串」三层之间穿过去。任何一层漏一个反斜杠,远端都
//! 只会**安静地什么都不发** —— 没有报错、没有日志,只有 SFTP 面板停在 `~`。
//! `shell_bootstrap` 的单元测试只钉了字符串的形状,证明不了这件事。
//!
//! 走本机 `bash -c`,不走 SSH:注入串是共享的,SSH 那一段由 mullion-ssh 的
//! live 测试覆盖,这里没必要再要一台真机。开发机上有 bash,所以**不加
//! `#[ignore]`**,进常规 `cargo test --workspace`。
#![cfg(unix)]

use std::os::unix::ffi::OsStrExt;
use std::process::Command;

/// 测试目录名里同时放一个空格和一个 `%s`:
/// - `%s` 钉住「`$PWD` 走 printf 的**参数**,不是拼进格式串」。拼进去的话
///   这个 `%s` 会被当成格式符、吃掉一个不存在的参数、展开成空 —— 吐出来的
///   是一条**错的绝对路径**,而错的绝对路径骗得过下游所有校验。
/// - 空格钉住 `"$PWD"` 外面那对双引号没被漏掉(漏了就在空格处断成两段)。
const DIR_NAME: &str = "mullion osc7 100%s";

/// 收尾:断言失败(panic)那条路径也要把目录删掉。
struct RmOnDrop(std::path::PathBuf);

impl Drop for RmOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.0);
    }
}

#[test]
fn a_real_bash_reports_the_directory_the_injection_asks_for() {
    let dir = std::env::temp_dir().join(DIR_NAME);
    std::fs::create_dir_all(&dir).expect("建测试目录");
    let _cleanup = RmOnDrop(dir.clone());
    // 规范化:`temp_dir()` 可能含软链,而 bash 的 `$PWD` 报的是 `getcwd()`
    // 的结果(见下面 `env_remove("PWD")`)。两边不走同一条路的话,断言会在
    // 一个跟本功能毫无关系的地方假红。
    let dir = std::fs::canonicalize(&dir).expect("规范化测试目录");

    let line = String::from_utf8(mullion_app::shell_bootstrap::osc7_setup_line())
        .expect("注入串是 ASCII");

    // `--noprofile --norc`:不读开发机上这个用户的 rc,免得他自己的
    // `PROMPT_COMMAND` 把结论搅浑。
    // `env_remove("PWD")`:继承来的 PWD 是 cargo 的工作目录,跟
    // `current_dir` 不一致。清掉之后 bash 自己从 `getcwd()` 填,与上面的
    // `canonicalize` 对得上。
    // `TERM=dumb`:`clear` 在这里可能失败,无所谓 —— 它不是最后一条命令。
    // 非交互 bash 不会自己跑 `PROMPT_COMMAND`,所以显式再调一次那个函数。
    let out = Command::new("bash")
        .args(["--noprofile", "--norc", "-c"])
        .arg(format!("{line}__mullion_osc7\n"))
        .current_dir(&dir)
        .env_remove("PWD")
        .env("TERM", "dumb")
        .output()
        .expect("跑 bash");
    assert!(
        out.status.success(),
        "注入串在真 bash 上跑不通:stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    // 喂给生产用的那个嗅探器,而不是自己写一个正则 —— 这条测试要验的正是
    // 「我们发出去的东西,我们自己解得回来」。
    let mut sniffer = mullion_term::remote_state::Osc7Sniffer::default();
    let got = sniffer.feed(&out.stdout).unwrap_or_else(|| {
        panic!(
            "bash 跑完了,却没解出一条 OSC 7 —— 转义在某一层被吃掉了。\
             stdout={:?} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    assert_eq!(
        String::from_utf8_lossy(&got),
        String::from_utf8_lossy(dir.as_os_str().as_bytes()),
        "解出来的目录不是 bash 当时所在的那个"
    );
}
```

- [ ] **Step 3:跑**

```bash
cargo test -p mullion-app --test shell_osc7_live 2>&1 | tail -20
```
预期:`test result: ok. 1 passed`。

**如果红了,先看 panic 里打出来的 `stdout`**:
- 完全没有 `\x1b]7;` → Rust 侧的 `r#"..."#` 里反斜杠被吃掉了。
- 有 `\x1b]7;file:///tmp/mullion osc7 100` 但缺尾巴 → `%s` 被 printf 当格式符吃了,说明 `$PWD` 被拼进了格式串。
- 有序列但 `parse_osc7` 返回 `None` → 终止符不是 `ESC \` 或 `BEL`,检查 `\033\\` 那一段。

- [ ] **Step 4:变异验证(这一条最重要 —— 它证明 live 测试真的能抓到转义错)**

```bash
git add -A
```

把 `shell_bootstrap.rs` 里的 `OSC7_SETUP` 中 `printf '\033]7;file://%s\033\\' "$PWD"` 改成
`printf "\033]7;file://$PWD\033\\\\"`(即把 `$PWD` 拼进格式串),跑:

```bash
cargo test -p mullion-app --test shell_osc7_live 2>&1 | tail -20
```
预期:红,而且 panic 信息里能看到路径在 `100` 处被截断(`%s` 被吃掉了)。**改回来**,再跑一次确认绿。

- [ ] **Step 5:提交**

```bash
git add crates/mullion-app/tests/shell_osc7_live.rs
git commit -m "test(app): 拿真 bash 验 OSC 7 注入串能被自己解回来 (F156-c)

转义要穿过「Rust 字面量 → shell 单引号 → printf 格式串」三层,漏一个
反斜杠远端就安静地什么都不发。目录名里放 % 和空格,钉住 \$PWD 必须当
printf 的参数传 —— 拼进格式串会吐出一条错的绝对路径,而错的绝对路径
骗得过下游所有校验。"
```

---

## Task 5:F156-c(3/6)—— `Settings` 加 `shell_osc7_bootstrap` 字段

**Files:**
- Modify: `crates/mullion-store/src/settings.rs:60-114`(结构体 + 默认值 + `Default` impl)
- Test: `crates/mullion-store/src/settings.rs` 的 `mod tests`

- [ ] **Step 1:先写会失败的测试**

在 `crates/mullion-store/src/settings.rs` 的 `mod tests` 里,紧跟在 `fn tmux_bootstrap_survives_a_round_trip_when_turned_off` 之后加:

```rust
    /// F156-c:老的 `settings.toml` 里没有这个字段,读出来必须**默认开** ——
    /// 关着的话,所有已经在用的用户升级上来之后,非 tmux 场景仍然跟不住目录,
    /// 而他们不会知道设置里多了一个开关。
    ///
    /// 自证会变红:把 `default_shell_osc7_bootstrap` 的返回值改成 `false`。
    #[test]
    fn shell_osc7_bootstrap_defaults_to_on_for_files_written_before_it_existed() {
        let dir = tmp();
        std::fs::write(
            dir.path().join(SETTINGS_FILE),
            "schema_version = 1\nfont_pt = 10.0\n",
        )
        .expect("写老格式文件");
        let back = load(dir.path());
        assert!(back.note.is_none(), "老文件不该有 note:{:?}", back.note);
        assert!(
            back.settings.shell_osc7_bootstrap,
            "老文件缺这个字段时该默认开"
        );
    }

    /// 关掉之后要真的留得住 —— 这条命令是往用户当前这条 shell 里写东西并
    /// 清屏,「关了下次又自己开回来」是不能接受的。
    #[test]
    fn shell_osc7_bootstrap_survives_a_round_trip_when_turned_off() {
        let dir = tmp();
        let s = Settings {
            shell_osc7_bootstrap: false,
            ..Settings::default()
        };
        save(dir.path(), &s).expect("写盘");
        assert!(!load(dir.path()).settings.shell_osc7_bootstrap);
    }
```

(上面两条是照着同一个 `mod tests` 里既有的 `tmux_bootstrap_defaults_to_on_for_files_written_before_it_existed` / `tmux_bootstrap_survives_a_round_trip_when_turned_off` 写的,`tmp()` 与 `SETTINGS_FILE` 都是那里已有的东西。若实际有出入,以文件里既有的为准。)

- [ ] **Step 2:跑,确认编译不过(字段不存在)**

```bash
cargo test -p mullion-store 2>&1 | grep -E "^error|no field" | head -5
```
预期:`error[E0560]: struct `Settings` has no field named `shell_osc7_bootstrap``。

- [ ] **Step 3:加字段**

在 `crates/mullion-store/src/settings.rs` 里,`pub tmux_bootstrap: bool,` 之后、`/// F155:日志详细档位。` 之前插入:

```rust
    /// F156-c:pane 的 shell channel 一建立,就往 PTY 注入一行,让远端 shell
    /// 从此每个提示符发一次 OSC 7(当前目录)。
    ///
    /// **默认开**:非 tmux 场景下 `PaneState.cwd` 本来一条腿都没有 ——
    /// Ubuntu 的 bash 默认不发 OSC 7,而窗口标题那条腿只要 PS1 被
    /// starship / oh-my-bash 接管就断。用户报的「`Ctrl+Shift+B` 经常留在 `~`」
    /// 就是这个。
    ///
    /// 与 [`Settings::tmux_bootstrap`] **分开两个开关**:那个改的是远端 tmux
    /// 服务器内存里的选项,这个往用户**当前这条 shell** 里写一行命令并清屏。
    /// 副作用完全不同,想只关掉其中一件是合理诉求,一个开关做不到。
    ///
    /// 不写远端任何文件 —— 只活在这条 shell 的内存里,断开即消失。命令串与
    /// 逐处理由见 `mullion_app::shell_bootstrap::OSC7_SETUP`。
    #[serde(default = "default_shell_osc7_bootstrap")]
    pub shell_osc7_bootstrap: bool,
```

在 `fn default_tmux_bootstrap() -> bool { true }` 之后加:

```rust
fn default_shell_osc7_bootstrap() -> bool {
    true
}
```

在 `impl Default for Settings` 的 `tmux_bootstrap: true,` 之后加:

```rust
            shell_osc7_bootstrap: true,
```

- [ ] **Step 4:跑,确认绿**

```bash
cargo test -p mullion-store 2>&1 | tail -5
```
预期:全绿。若有别处的 `Settings { .. }` 字面量报缺字段,编译器会点名 —— 逐个补 `shell_osc7_bootstrap: true,`(截至写计划时,库内所有 `Settings` 字面量都带 `..Settings::default()`,应当一个都不用改)。

- [ ] **Step 5:变异验证**

```bash
git add -A
```
把 `default_shell_osc7_bootstrap` 的返回值改成 `false` → 预期 `shell_osc7_bootstrap_defaults_to_on_for_files_written_before_it_existed` 红。改回来。

- [ ] **Step 6:提交**

```bash
git add crates/mullion-store/src/settings.rs
git commit -m "feat(store): 设置加 shell_osc7_bootstrap 开关,默认开 (F156-c)

serde(default) 保证老 settings.toml 读出来是开的 —— 关着的话已在用的
用户升级后非 tmux 场景仍然跟不住目录,而他们不会知道多了这个开关。

与 F124 的 tmux_bootstrap 分成两个开关:副作用不同(那个改 tmux 服务器
内存里的选项,这个往当前 shell 写命令并清屏)。"
```

---

## Task 6:F156-c(4/6)—— 设置弹窗「远端」分节加第二个勾选框

**Files:**
- Modify: `crates/mullion-app/src/ui/settings.rs`(标签常量、`SettingsDraft`、`from_settings`、`remote()`、3 处测试字面量)
- Test: 同文件的 `mod tests`

- [ ] **Step 1:先写会失败的测试**

在 `crates/mullion-app/src/ui/settings.rs` 的 `mod tests` 里,紧跟在 `fn the_draft_starts_from_the_stored_switch_not_from_a_hardcoded_default` 之后加:

```rust
    // ---- F156-c 远端分节第二个开关 ----

    /// F156-c:点这个开关要当场回报 `Preview`(草稿变了、要重画),
    /// 「确定」时才落盘。回报 `None` 的话用户点了没反应。
    ///
    /// 用文件里既有的 `interact` 脚手架(跑满 `FRAMES` 帧预热,再按标签文字
    /// 找部件中心点下去;复选框是 `Sense::click()`,要同帧松手)。
    ///
    /// 自证会变红:把 `remote()` 里这个复选框的 `.changed()` 分支删掉。
    #[test]
    fn toggling_the_shell_osc7_checkbox_reports_a_preview() {
        let mut d = draft();
        assert!(d.shell_osc7_bootstrap, "脚手架的初值该是开着的");
        let out = interact(&mut d, OSC7_LABEL, egui::Vec2::ZERO, true);
        assert!(
            !d.shell_osc7_bootstrap,
            "复选框没被真的点到,这条测试测了个寂寞"
        );
        assert_eq!(out, SettingsOut::Preview);
    }

    /// F156-c:两个开关是**独立**的 —— 点了这个,F124 那个不许跟着动。
    /// 它们的副作用完全不同(一个改远端 tmux 服务器的内存选项,一个往用户
    /// 当前这条 shell 里写命令并清屏),串在一起等于把「只关掉其中一件」
    /// 这个合理诉求堵死。
    ///
    /// 自证会变红:把 `remote()` 里第二个 `checkbox` 的第一个参数写成
    /// `&mut draft.tmux_bootstrap`(**这正是复制粘贴最容易出的错**,
    /// 而且它不报错、只是两个开关联动)。
    #[test]
    fn the_two_remote_switches_are_independent() {
        let mut d = draft();
        let _ = interact(&mut d, OSC7_LABEL, egui::Vec2::ZERO, true);
        assert!(!d.shell_osc7_bootstrap, "点的是 OSC 7 那个");
        assert!(d.tmux_bootstrap, "点 OSC 7 那个把 F124 的开关也带翻了");

        let mut d = draft();
        let _ = interact(&mut d, BOOTSTRAP_LABEL, egui::Vec2::ZERO, true);
        assert!(!d.tmux_bootstrap, "点的是 tmux 那个");
        assert!(d.shell_osc7_bootstrap, "点 tmux 那个把 OSC 7 的开关也带翻了");
    }

    /// F156-c:草稿从**落盘的真值**起。起错了的症状是「用户关掉过,再打开
    /// 设置弹窗又显示开着」—— 而只要他这时点了确定,关掉的选择就被覆盖回去。
    ///
    /// 自证会变红:把 `from_settings` 里那行改成 `shell_osc7_bootstrap: true,`。
    #[test]
    fn the_osc7_draft_starts_from_the_stored_switch() {
        let s = mullion_store::Settings {
            shell_osc7_bootstrap: false,
            ..Default::default()
        };
        assert!(!SettingsDraft::from_settings(&s).shell_osc7_bootstrap);
        assert!(
            SettingsDraft::from_settings(&mullion_store::Settings::default())
                .shell_osc7_bootstrap
        );
    }
```

- [ ] **Step 2:跑,确认编译不过**

```bash
cargo test -p mullion-app --lib ui::settings 2>&1 | grep -E "^error" | head -5
```
预期:`cannot find value `OSC7_LABEL``、`no field `shell_osc7_bootstrap``。

- [ ] **Step 3:加标签常量**

在 `crates/mullion-app/src/ui/settings.rs` 的 `const BOOTSTRAP_LABEL: &str = "自动配置远端 tmux 的状态上报";` 之后加:

```rust
/// F156-c 那个开关的标签。同上,实现与测试**共用这一份**。
const OSC7_LABEL: &str = "让远端 shell 报出当前目录(非 tmux 场景)";
```

- [ ] **Step 4:草稿加字段**

在 `SettingsDraft` 的 `pub tmux_bootstrap: bool,` 之后加:

```rust
    /// F156-c:往远端 shell 注入一次 OSC 7 上报。
    pub shell_osc7_bootstrap: bool,
```

在 `from_settings` 的 `tmux_bootstrap: s.tmux_bootstrap,` 之后加:

```rust
            shell_osc7_bootstrap: s.shell_osc7_bootstrap,
```

- [ ] **Step 5:远端分节加第二个勾选框**

把 `fn remote(...)` 的文档注释第一行

```rust
/// 远端分节:自动配置 tmux 状态上报(F124)。
```

改成

```rust
/// 远端分节:自动配置 tmux 状态上报(F124)+ 让远端 shell 报出当前目录(F156-c)。
///
/// **两个独立开关**,不是一个。副作用完全不同:F124 改的是远端 tmux 服务器
/// 内存里的全局选项,F156-c 往用户**当前这条 shell** 里写一行命令并清屏。
/// 想只关掉其中一件是合理诉求,一个开关做不到。
```

再在 `remote()` 内部、F124 那段说明文字的 `ui.end_row();` 之后、`});` 之前插入:

```rust
        ui.label("");
        if ui
            .checkbox(&mut draft.shell_osc7_bootstrap, OSC7_LABEL)
            .changed()
        {
            *out = SettingsOut::Preview;
        }
        ui.end_row();

        ui.label("");
        ui.label(
            egui::RichText::new(
                "分屏刚连上时往远端 shell 发一行命令,让它此后每个提示符都报一次当前目录。\
                 上面那条只在远端开着 tmux 时管用,这条管的是**不经过 tmux** 的场景 ——\
                 文件面板继承终端所在目录靠它。\
                 只改这条 shell 内存里的 PROMPT_COMMAND(不写远端任何文件,断开即消失),\
                 发完会清一次屏,所以登录横幅会被一起清掉。\
                 远端 shell 不是 bash / zsh(比如 fish)时,屏幕上会打出一行报错,\
                 那种情况请关掉这个开关。",
            )
            .size(11.0)
            .color(theme::c32(t.fg_dim)),
        );
        ui.end_row();
```

**注意**:上面这段说明里的 `**不经过 tmux**` 在 egui 里不会被渲染成粗体(`RichText` 不解析 markdown),会原样显示两颗星。**把那两对 `**` 去掉**,写成「这条管的是不经过 tmux 的场景」。写进来是为了提醒你别照抄 markdown 语法进 UI 字符串。

- [ ] **Step 6:补齐三处 `SettingsDraft` 字面量**

编译器会点名。逐个在 `tmux_bootstrap: ...` 之后加 `shell_osc7_bootstrap: true,`:

1. `crates/mullion-app/src/ui/settings.rs` 的 `fn draft()`(测试脚手架)
2. `crates/mullion-app/src/ui/settings.rs` 的 `fn a_font_that_is_not_installed_is_called_out` 里那个字面量
3. `crates/mullion-app/src/app.rs` 的 `fn a_password_change_always_clears_the_two_boxes` 里那个字面量

- [ ] **Step 7:跑**

```bash
cargo test -p mullion-app --lib ui::settings 2>&1 | tail -5
```
预期:全绿。

**如果 `toggling_the_shell_osc7_checkbox_reports_a_preview` 因为「设置弹窗里没有写着「…」的部件」panic**,那是 `FRAMES` 不够了 —— 弹窗又长了两行,宽度收敛可能要多一两帧。把 `const FRAMES` 从 8 提到 10 并在它的注释里补一句「F156-c 加了两行之后实测需要 N 帧」。**不要**为了让它过而改测试的断言。

- [ ] **Step 8:跑字形白名单(T9)**

```bash
cargo test -p mullion-app --test glyph_whitelist 2>&1 | tail -5
```
预期:绿。新加的 UI 字符串只有汉字、中文标点和 ASCII;若它红了,报出的那个字符就是刚写进去的,换成 GBK 内的写法(别去 `VERIFIED` 里登记 —— 那个闸门的意思是「你已经去 Windows 实机看过一眼」)。

- [ ] **Step 9:跑表单规范守护**

```bash
cargo test -p mullion-app --test form_guidelines 2>&1 | tail -5
```
预期:绿(复选框和灰字说明都挂在输入列、标签列留空,与上面那条同构)。

- [ ] **Step 10:变异验证**

```bash
git add -A
```
把第二个 `checkbox` 的第一个参数改成 `&mut draft.tmux_bootstrap` → 预期 `the_two_remote_switches_are_independent` 红。改回来。

- [ ] **Step 11:提交**

```bash
git add crates/mullion-app/src/ui/settings.rs crates/mullion-app/src/app.rs
git commit -m "feat(app): 设置弹窗远端分节加「让远端 shell 报出当前目录」 (F156-c)

独立开关,不复用 F124 那个 —— 副作用不同(那个改 tmux 服务器内存选项,
这个往当前 shell 写命令并清屏)。说明文字点名了清屏与 fish 报错两个代价。

守护:toggling_the_shell_osc7_checkbox_reports_a_preview /
the_two_remote_switches_are_independent(复制粘贴写错开关名会红) /
the_osc7_draft_starts_from_the_stored_switch"
```

---

## Task 7:F156-c(5/6)—— `on_pane_ready` 收口三处调用点并注入

这一步是本片唯一有回归风险的地方:三处「pane 挂上了、拿到写口」各写一遍,正是本项目已经踩中**三次**的「列举式门控在加档时必然漏」。收成一个方法,加第四种 pane 建立方式时不会再漏。

**Files:**
- Modify: `crates/mullion-app/src/app.rs`
  - 新增 `fn on_pane_ready`(放在 `fn start_automation` 之前)
  - `accept_connect_ok` 尾部(`crates/mullion-app/src/app.rs:5584-5606` 一带)
  - `UserEvent::PaneOpened` 分支(`:6499-6509`)
  - `UserEvent::PaneRehosted` 分支(`:6593-6602`)
  - `fn take_settings_draft`(`:2286` 一带)
- Test: 同文件的 `mod tests`

- [ ] **Step 1:先写会失败的接线守护**

在 `crates/mullion-app/src/app.rs` 的 `mod tests` 里(和其它 `include_str!("app.rs")` 那批源码级守护放在一起),加:

```rust
    /// F156-c:**每一条 pane 建立路径都必须走 `on_pane_ready`。**
    ///
    /// 三处调用点各写一遍注入,正是「列举式门控在加档时必然漏」——本项目
    /// 已经踩中三次。漏一处的症状是:那种方式开出来的 pane 永远跟不住目录,
    /// 而且完全静默(没有报错、没有日志,只是 `Ctrl+Shift+B` 停在 `~`)。
    ///
    /// 判据是「`self.start_automation(` 在整个文件里**只有一个**调用点」——
    /// 加第四种 pane 建立方式的人只要照着现有的写一句 `self.start_automation`,
    /// 这条就红,他会被逼着去看 `on_pane_ready`。
    ///
    /// 顺序也钉:注入串自带 `clear`,排在自动化**之后**会把用户登录后命令的
    /// 输出清掉一半。
    ///
    /// 自证会变红:
    /// - 把任意一处调用点改回直接调 `self.start_automation` → 计数变 2,第 1 条红
    /// - 把 `on_pane_ready` 里的注入删掉 → 第 3 条红
    /// - 把注入挪到 `start_automation` 之后 → 第 4 条红
    #[test]
    fn every_pane_ready_path_goes_through_on_pane_ready() {
        let src = include_str!("app.rs");
        let calls = src.matches("self.start_automation(").count();
        assert_eq!(
            calls,
            1,
            "`self.start_automation(` 有 {calls} 个调用点。新的 pane 建立方式\
             必须改走 `on_pane_ready`,否则那条路径不会注入 OSC 7(静默失效)"
        );
        // 切片键带上换行和缩进,钉住切的是**方法定义**而不是文档注释里的提及。
        let body = src
            .split("\n    fn on_pane_ready(")
            .nth(1)
            .expect("找不到 on_pane_ready 的定义");
        let body = &body[..body
            .find("\n    }\n")
            .expect("找不到 on_pane_ready 的函数结尾")];
        assert!(
            body.len() > 120,
            "on_pane_ready 的函数体切歪了(切出来 {} 字节)",
            body.len()
        );
        let inject = body
            .find("shell_bootstrap::osc7_setup_line()")
            .expect("on_pane_ready 里没有注入 OSC 7 —— 这个方法存在的理由就是它");
        let automate = body
            .find("self.start_automation(")
            .expect("on_pane_ready 里没起自动化 —— 三处调用点的另一半功能丢了");
        assert!(
            inject < automate,
            "注入排在了自动化之后 —— 注入串自带 clear,会把登录后命令的输出清掉"
        );
        assert!(
            body.contains("self.settings.shell_osc7_bootstrap"),
            "注入没读开关,用户关不掉"
        );
    }

    /// F156-c:设置弹窗「确定」时,新开关要真的搬进 `self.settings` ——
    /// 不搬的话用户改了、点了确定、也落了盘,但**本次运行**仍按旧值走,
    /// 而他不会知道要重启。
    ///
    /// 与既有的 `tmux_bootstrap`/`log_level` 两条切的是同一个函数体
    /// (`take_settings_draft`),写法照抄它们。
    ///
    /// 自证会变红:把 `self.settings.shell_osc7_bootstrap = d.shell_osc7_bootstrap;`
    /// 删掉。
    #[test]
    fn committing_the_settings_carries_the_shell_osc7_switch() {
        let src = include_str!("app.rs");
        let body = src
            .split("\n    fn take_settings_draft(&mut self) {")
            .nth(1)
            .expect("找不到 take_settings_draft 的定义");
        let body = &body[..body
            .find("\n    }\n")
            .expect("找不到 take_settings_draft 的函数结尾")];
        assert!(
            body.contains("self.settings.shell_osc7_bootstrap = d.shell_osc7_bootstrap;"),
            "「确定」没把 F156-c 的开关搬进 settings:{body}"
        );
    }
```

- [ ] **Step 2:跑,确认是红的**

```bash
cargo test -p mullion-app --lib app::tests::every_pane_ready_path_goes_through_on_pane_ready app::tests::committing_the_settings_carries_the_shell_osc7_switch 2>&1 | tail -20
```
预期:两条都 FAIL(此刻 `start_automation` 有 3 个调用点、`on_pane_ready` 还不存在)。

- [ ] **Step 3:写 `on_pane_ready`**

在 `crates/mullion-app/src/app.rs` 的 `fn start_automation(` **定义之前**(它那段文档注释之前)插入:

```rust
    /// 一块 pane 挂上了、写口拿到手了 —— 三条路径(首次连接 / 分屏新开 /
    /// 换节点)共用的落地动作。
    ///
    /// **必须是唯一入口。** 三处各写一遍,正是「列举式门控在加档时必然漏」,
    /// 本项目已经踩中三次;漏一处的症状是那种方式开出来的 pane 永远跟不住
    /// 目录,而且完全静默。守护:`every_pane_ready_path_goes_through_on_pane_ready`。
    ///
    /// 做两件事,**顺序不能反**:
    /// 1. F156-c:注入一次 OSC 7 上报(见 `shell_bootstrap::OSC7_SETUP`)。
    /// 2. 有计划的话,起 F40~F44 的登录后自动化。
    ///
    /// 注入串自带 `clear`,排在自动化之后会把用户登录后命令的输出清掉一半。
    ///
    /// **只在 pane 刚建立、shell 还没跑任何程序时注入。** 这是唯一安全的窗口
    /// —— 换到 `Ctrl+Shift+B` 那一刻现写的话,pane 里可能正跑着 Claude Code
    /// 之类的全屏 TUI,写进去的字节会变成那个 TUI 的按键输入。
    ///
    /// `ByteSink::write` 是**同步**的(`try_send` 语义),不需要起 task。
    /// 写失败(出站队列满 / channel 已死)只记一行日志:这是锦上添花的功能,
    /// 拿不到目录就退回 F120 配置的默认远端目录,不该把连接本身搅黄。
    fn on_pane_ready(
        &mut self,
        generation: u64,
        pane: PaneId,
        sink: Arc<mullion_ssh::session::SshSession>,
        plan: Option<crate::automation::PendingAutomation>,
    ) {
        if self.settings.shell_osc7_bootstrap {
            use mullion_ssh::schedule::ByteSink;
            if let Err(e) = sink.write(crate::shell_bootstrap::osc7_setup_line()) {
                log::warn!(
                    target: "mullion",
                    "pane {} 的 OSC 7 自举没发出去({e:?}),这条 shell 不会报当前目录",
                    pane.0
                );
            }
        }
        if let Some(plan) = plan {
            self.start_automation(generation, pane, plan, sink);
        }
    }

```

**`ByteSink` 的引入方式**:`impl ByteSink for SshSession` 在 `crates/mullion-ssh/src/schedule.rs:25`,所以 `sink`(`Arc<SshSession>`)通过 `Deref` 就能调 `write`。若 `use` 在方法内报 unused 或与 `PtyWriter::write` 撞名,改成完全限定调用:

```rust
            let bytes = crate::shell_bootstrap::osc7_setup_line();
            if let Err(e) = mullion_ssh::schedule::ByteSink::write(sink.as_ref(), bytes) {
```

**`Arc<SshSession>` 同时实现了 `PtyWriter`(`shell/workspace/mod.rs:111`),两个 trait 都有 `write`** —— 编译器报 ambiguity 时用上面的完全限定写法,不要去改任何一个 trait。

- [ ] **Step 4:改调用点 1(`accept_connect_ok`,首次连接)**

把 `crates/mullion-app/src/app.rs` 里这一段:

```rust
        if let Some(plan) = crate::automation::take_pending(
            &mut self.pending_automation.plan,
            &mut self.pending_automation.skip,
        ) {
```

一直到

```rust
            // 建标签的这个 pane 照配置**全套**跑,含 tmux
            // (`PaneId(1)` 见 `Workspace::new`)。
            self.start_automation(generation, PaneId(1), plan, ssh);
        }
        self.request_ui_redraw();
```

改成:

```rust
        let plan = crate::automation::take_pending(
            &mut self.pending_automation.plan,
            &mut self.pending_automation.skip,
        );
        if plan.is_some() {
            // S1:挂回**属主标签**(按世代号查),不用「活动标签」——
            // `open` 刚把新标签设为活动,今天两者等价,但那是巧合:
            // 哪天连接成功不再顺带切换焦点,这里就会把 handle 挂错标签。
            if let Some(t) = self
                .tabs
                .by_generation_mut(generation)
                .and_then(|tab| tab.content.as_terminal_mut())
            {
                // F141:这次全套跑里到底 attach 了哪个 tmux 会话 ——
                // 断线重连要照着它把用户接回去。
                t.tmux_attach = tmux_attach_for_connect(tpl.as_ref(), tmux_name.as_deref());
                t.automation_template = tpl;
            }
        }
        // 建标签的这个 pane 照配置**全套**跑,含 tmux
        // (`PaneId(1)` 见 `Workspace::new`)。F156-c:注入也在这里发,
        // 见 `on_pane_ready`。
        self.on_pane_ready(generation, PaneId(1), ssh, plan);
        self.request_ui_redraw();
```

**注意**:原来 `t.tmux_attach = ...` / `t.automation_template = tpl;` 那两行在 `if let Some(plan)` 里面,`tpl` 被移动进去。改成 `if plan.is_some()` 之后 `tpl` 仍在作用域里,移动语义不变(仍然只在这一支里被移走)。**`tpl` / `tmux_name` 的两行 `take()` 不要动位置** —— 它们必须在 `take_pending` 之前跑完,那是「模板跟计划同进同退」的既有语义。

**`ssh` 的所有权**:原来只在有 plan 时才被移动进 `start_automation`;现在无条件移进 `on_pane_ready`。确认 `ssh` 在这行之后没有别的用途(`request_ui_redraw()` 不碰它),编译器会替你验。

- [ ] **Step 5:改调用点 2(`UserEvent::PaneOpened`,分屏新开)**

把:

```rust
                if let Some(sink) = attached {
                    let plan = self
                        .tabs
                        .by_generation(generation)
                        .and_then(|tab| tab.content.as_terminal())
                        .and_then(|t| t.automation_template.as_ref())
                        .and_then(crate::automation::pending_for_extra_pane);
                    if let Some(plan) = plan {
                        self.start_automation(generation, id, plan, sink);
                    }
                }
```

改成:

```rust
                if let Some(sink) = attached {
                    let plan = self
                        .tabs
                        .by_generation(generation)
                        .and_then(|tab| tab.content.as_terminal())
                        .and_then(|t| t.automation_template.as_ref())
                        .and_then(crate::automation::pending_for_extra_pane);
                    self.on_pane_ready(generation, id, sink, plan);
                }
```

- [ ] **Step 6:改调用点 3(`UserEvent::PaneRehosted`,换节点)**

把:

```rust
                if let Some(sink) = attached {
                    // 用户拍板:换过节点的 pane 要跑**新节点**的登录后命令,
                    // 规则同分屏新开的那些 —— 跳过 tmux,其余照跑。
                    if let Some(plan) = pending.plan {
                        self.start_automation(generation, pane, plan, sink);
                    }
                    self.ui.set_toast("已换节点");
```

改成:

```rust
                if let Some(sink) = attached {
                    // 用户拍板:换过节点的 pane 要跑**新节点**的登录后命令,
                    // 规则同分屏新开的那些 —— 跳过 tmux,其余照跑。
                    self.on_pane_ready(generation, pane, sink, pending.plan);
                    self.ui.set_toast("已换节点");
```

- [ ] **Step 7:改 `take_settings_draft`**

在 `crates/mullion-app/src/app.rs` 的 `fn take_settings_draft` 里,`self.settings.tmux_bootstrap = d.tmux_bootstrap;` 之后加:

```rust
            self.settings.shell_osc7_bootstrap = d.shell_osc7_bootstrap;
```

- [ ] **Step 8:跑两条新守护 + 整个 crate**

```bash
cargo test -p mullion-app 2>&1 > /tmp/f156.log; grep -nE "test result|FAILED|panicked" /tmp/f156.log
```
预期:全绿。

**如果 `every_pane_ready_path_goes_through_on_pane_ready` 报「计数 2」**,说明还有一处调用点没改完 —— `grep -n "self.start_automation(" crates/mullion-app/src/app.rs` 找出来。

- [ ] **Step 9:变异验证(三条)**

```bash
git add -A
```

变异 1:把 `PaneRehosted` 那处改回 `if let Some(plan) = pending.plan { self.start_automation(generation, pane, plan, sink); }` → 预期第 1 条断言红(计数 2)。改回来。
变异 2:删掉 `on_pane_ready` 里 `if self.settings.shell_osc7_bootstrap { ... }` 整块 → 预期 `expect("on_pane_ready 里没有注入 OSC 7 …")` 红。改回来。
变异 3:把 `on_pane_ready` 里的注入块整块挪到 `if let Some(plan)` 之后 → 预期「注入排在了自动化之后」红。改回来。

**注意**:变异 1 与变异 3 都涉及删/挪同一片代码,分两轮做,每轮做完 `git diff` 确认工作区回到暂存区状态再做下一轮(历史上「变异锚点用裸前缀会两轮删同一处」)。

- [ ] **Step 10:提交**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): pane 建立时注入 OSC 7 上报,三处调用点收成 on_pane_ready (F156-c)

非 tmux 场景下 PaneState.cwd 没有任何数据源,Ctrl+Shift+B 只能停在 ~。
pane 刚建立、shell 还没跑任何程序,是唯一安全的注入窗口 —— 换到按
Ctrl+Shift+B 那一刻现写的话,pane 里可能正跑着全屏 TUI,字节会变成按键。

三处(ConnectOk / PaneOpened / PaneRehosted)收成一个方法:各写一遍正是
「列举式门控在加档时必然漏」,本项目已踩中三次。注入排在 start_automation
之前 —— 注入串自带 clear。

守护:every_pane_ready_path_goes_through_on_pane_ready(钉调用点唯一 +
注入在前)/ committing_the_settings_carries_the_shell_osc7_switch"
```

---

## Task 8:F156-c(6/6)—— 全量跑绿

- [ ] **Step 1:全量测试**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
```
预期:每个 target 都 `test result: ok`,没有 FAILED / panicked。

- [ ] **Step 2:clippy**

```bash
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20
```
预期:无输出(除了 `Finished` 那行)。

常见的两个:`on_pane_ready` 参数多于 7 个会触发 `too_many_arguments`(本方法只有 5 个,不会);`sink.write(...)` 若被判 ambiguity 已在 Task 7 Step 3 给了完全限定写法。

- [ ] **Step 3:fmt**

```bash
cargo fmt --all && cargo fmt --check
```
预期:第二条无输出。若 `cargo fmt` 改动了文件,`git add -A` 后并进上一个提交或单独提交。

- [ ] **Step 4:如果有 fmt 改动就提交**

```bash
git status --short
# 有改动才跑下面这条
git add -A && git commit -m "style: cargo fmt"
```

---

## Task 9:文档与 spec 收尾

**Files:**
- Modify: `docs/remote-state-setup.md`
- Modify: `spec.md`

- [ ] **Step 1:改掉 `docs/remote-state-setup.md` 里过时的那句**

先定位:

```bash
grep -n "files_start_dir\` 只接受" docs/remote-state-setup.md
```
预期:命中第 130 行一带。

把这一整段(以「标题里的路径常常是」开头、到那个右括号加句号为止):

```
标题里的路径常常是 `~/Mullion` 这种缩写形式。**`~` 只用来在标题条上显示目录名**,
不拿去当 SFTP 起始目录 —— openssh 的 `sftp-server` 不展开 `~`,直接拿 `~/Mullion`
去 `canonicalize` 会失败,面板会停在「取不到登录目录」,比不继承更糟
(`files_start_dir` 只接受以 `/` 开头的绝对路径,`~/...` 一律落回配置的默认远端目录)。
```

换成:

```
标题里的路径常常是 `~/Mullion` 这种缩写形式,而 openssh 的 `sftp-server`
**不展开 `~`** —— 直接拿 `~/Mullion` 去 `canonicalize` 会失败,面板会停在
「取不到登录目录」,比不继承更糟。

所以 `files_start_dir` 分两档处理:已经是绝对路径的直接用;`~` / `~/x` 在
**登录目录已知**时(sftp 已经开过、拿到过 home)由 `expand_tilde` 展开成绝对
路径,登录目录未知时才落回配置的默认远端目录。`~user` **不展开** —— 那要查
远端的 passwd,猜错会把用户带到别人的家目录去。
```

> 这句原文写的是「`~/...` 一律落回配置的默认远端目录」,那是 F123 补 `expand_tilde`
> 之前的行为,已经过时。本次定位 F156-c 的根因时差点被它带偏,所以随片改掉。

- [ ] **Step 2:在同一份文档里补上 F156-c**

在「## 降级行为」那张表之后、「## 文件面板已经开着时,不会跟着终端 cd 跑」之前,插入一节:

```markdown
## 不经过 tmux 时:客户端自己注入(F156-c)

上面整节讲的都是「远端配好了会怎样」。**非 tmux 场景的问题在于绝大多数远端
根本没配**:Ubuntu 的 bash 默认不发 OSC 7,而窗口标题那条腿只要 PS1 被
starship / oh-my-bash / 自定义 rc 接管就断 —— 两条腿同时断,`PaneState.cwd`
一个字节都收不到,`Ctrl+Shift+B` 只能停在登录目录。

F156-c 的做法是:**pane 的 shell channel 一建立就往 PTY 写一行**,让这条
shell 从此每个提示符发一次 OSC 7。命令串与逐处理由见
`crates/mullion-app/src/shell_bootstrap.rs` 的 `OSC7_SETUP`;发它的地方是
`App::on_pane_ready`(三条 pane 建立路径共用的唯一入口)。

- **不写远端任何文件**,只改这条 shell 内存里的 `PROMPT_COMMAND`,断开即消失。
  这是它能默认开启、而「往 `~/.bashrc` 追加」不能的原因。
- 用户原有的 `PROMPT_COMMAND` 保留(拼在我们这条后面),不覆盖。
- 末尾带一次 `clear`,所以 motd / 登录横幅会被清掉。
- 开关在设置弹窗「远端」分节,与 F124 那个**分开**(`shell_osc7_bootstrap`)。

**已知限制:**
- **tmux 场景无效但无害** —— tmux 吃掉内层 OSC 7 不转发(F51 被否的同一个
  事实),那个场景走 F124 那条腿。
- **fish / csh 下这一行是语法错误**,屏幕上会打一行报错。fish 3.x 起本来就
  默认发 OSC 7,不做兼容;请关掉开关。
- **注入只发生在 pane 建立那一刻**。用户之后在 pane 里 `ssh` 到第三台机器,
  `PROMPT_COMMAND` 不会跟过去。
- 远端 sshd 配了 `ForceCommand`、或用户的登录 shell 直接就是 tmux 时,写进去
  的字节会变成那个程序的输入。这是注入方案的固有代价。

**为什么不在按 `Ctrl+Shift+B` 那一刻现写:** 那时 pane 里可能正跑着 Claude Code
之类的全屏 TUI,写进去的字节会变成 TUI 的按键输入。**pane 刚建立、shell 还没跑
任何程序**是唯一安全的注入窗口。
```

- [ ] **Step 3:在「相关代码」小节补两条**

在该小节的「自举:…」那条之后加:

```markdown
- 非 tmux 自举:`crates/mullion-app/src/shell_bootstrap.rs`(注入串)+
  `App::on_pane_ready`(三条 pane 建立路径共用的注入点)+
  `crates/mullion-app/tests/shell_osc7_live.rs`(拿真 bash 验转义)
```

- [ ] **Step 4:给 `spec.md` 加三行**

在 `| F152 | …` 那一行之后追加(**注意 `spec.md` 用的是全角标点 `：（）—`,与 `docs/` 下的半角风格不同,照抄表里既有的**):

```markdown
| F156-a | **「恢复上次的现场」弹窗加关闭入口**：标题栏右上角 ×（`egui::Window::open()`，`line_segment` 画的，不进 T9 字形白名单）+ Esc，两者都回报**既有的** `HistoryOut::Dismiss`，`app.rs` 侧处置零改动。底部的「不恢复」保留——那是键盘路径的出口，× 是鼠标路径的直觉位置，删掉任一个都会让某一类用户找不到出口 | P2 | `closing_the_window_with_the_title_bar_x_reports_dismiss`（× 是线段画的，找 `Shape::Text` 的老脚手架点不中，改从 accesskit 树按 `"Close window"` 取 rect；取不到就 panic 并打出树里所有 label，egui 换版本改了标签要**当场报出来**而不是静默恒绿）/ `pressing_escape_closes_the_dialog` |
| F156-b | **换节点成功后分屏焦点跟到那块 pane**：`ws.set_focus(id)` 加在**自由函数 `rehost_pane`** 末尾（那里能拿真实 `Workspace` 直接断言 `ws.focus()`；放事件分支只能写「读 `app.rs` 源码找字符串」式的恒绿断言）。`reattach_pane`（F128 断线自动重连）**刻意不跟着改**——换节点是用户刚刚亲手发起的，断线重连是后台自愈，可能发生在用户正在另一块 pane 里打字的任意时刻，抢焦点等于把按键发到另一台机器上。只动分屏焦点，不动 egui 输入焦点 | P2 | `rehosting_moves_the_focus_to_that_pane_but_reattaching_never_does`（**对照**测试，两个方向各有一条变异命中：删 `set_focus` / 往 `reattach_pane` 里也加一句） |
| F156-c | **非 tmux 场景的 shell OSC 7 自举**：pane 的 shell channel 一建立就往 PTY 写一行，让远端 shell 此后每个提示符发一次 OSC 7。这是 F124 的 shell 版——非 tmux 时 `PaneState.cwd` 两条腿同时断（bash 默认不发 OSC 7；窗口标题那条只要 PS1 被 starship/oh-my-bash 接管就断），`Ctrl+Shift+B` 只能停在登录目录。**不写远端任何文件**（只改这条 shell 内存里的 `PROMPT_COMMAND`，断开即消失，这是能默认开启的前提）；保留用户原有的 `PROMPT_COMMAND`；`$PWD` 走 `printf '…%s…' "$PWD"` 的**参数**而非拼进格式串（拼进去时目录名含 `%` 会吐出一条**错的绝对路径**，而错的绝对路径骗得过下游所有校验）；前导空格走 `HISTCONTROL=ignorespace`（尽力而为）；末尾 `clear`（代价是 motd 被清）。开关 `shell_osc7_bootstrap` 默认开，与 F124 那个**分开**（副作用不同）。三处 pane 建立路径收成 `App::on_pane_ready`，注入排在 `start_automation` **之前**（注入串自带 `clear`）。**只在 pane 刚建立时注入**——按 `Ctrl+Shift+B` 那一刻 pane 里可能正跑着全屏 TUI，字节会变成按键 | P1 | `shell_bootstrap` 八条纯逻辑（换行/前导空格/`$PWD` 当参数/主机名段留空/保留用户的 `PROMPT_COMMAND`/bash+zsh 两条分支/末尾 clear/全 ASCII）；**`tests/shell_osc7_live.rs` 拿真 bash 跑一遍再喂给自家 `Osc7Sniffer`**——转义要穿过「Rust 字面量 → shell 单引号 → printf 格式串」三层，漏一个反斜杠远端就安静地什么都不发，测试目录名里放 `%s` 和空格钉住这一条；接线守护 `every_pane_ready_path_goes_through_on_pane_ready`（`self.start_automation(` 全文件**只许有 1 个**调用点 + 注入必须排在它前面）/ `committing_the_settings_carries_the_shell_osc7_switch`；UI 侧 `toggling_the_shell_osc7_checkbox_reports_a_preview` / `the_two_remote_switches_are_independent`（复制粘贴写错开关名会红）/ `the_osc7_draft_starts_from_the_stored_switch`。**验不了**：注入时机是否真落在 pty 缓冲窗口里、`clear` 之后的观感、高延迟代理链路下注入与登录后自动化的先后、非 bash/zsh 远端上那行报错的实际样子——全部进人工验收清单 |
```

- [ ] **Step 5:提交**

```bash
git add docs/remote-state-setup.md spec.md
git commit -m "docs(f156): 补 F156-a/b/c 需求条目与非 tmux 自举一节

顺带修正 remote-state-setup.md 里过时的一句 —— 那句说「~/... 一律落回
配置的默认远端目录」,是 F123 补 expand_tilde 之前的行为,本次定位 F156-c
根因时差点被它带偏。"
```

---

## Task 10:发版(按 CLAUDE.md 的「交付约定」一条龙)

本轮改动落到了 `mullion-app`,用户要拿去 Windows 实机验,所以**不要停下来问「要不要发版」**。

- [ ] **Step 1:升版本号**

`Cargo.toml` 第 12 行 `version = "0.1.65"` → `version = "0.1.66"`。

- [ ] **Step 2:确认还是绿的**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
```

- [ ] **Step 3:提交版本号**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: 版本 0.1.66(恢复弹窗关闭入口、换节点焦点跟随、非 tmux 的 shell OSC 7 自举)"
```

- [ ] **Step 4:走发版技能**

调用 `.claude/skills/release-windows/SKILL.md`(说「发版」时自动加载)。**别凭记忆做** —— 交叉编译、objdump 验收、签名、`gh` 走 socks 代理(本机 DNS 解析不了 github)每一步都有漏了也不报错的坑。

- [ ] **Step 5:人工验收清单(写进 Release notes)**

这些在无头容器里**验不了**,必须由人在 Windows 11 + 真实远端上确认:

**F156-a**
- [ ] 启动时弹出「恢复上次的现场」,标题栏右上角有一个 ×,点它弹窗关掉、且**不恢复任何标签**。
- [ ] 按 Esc 同样关掉、且不恢复。
- [ ] 底部的「恢复」「不恢复」照旧;单击某一行仍然当场恢复那一条(F153-b 没被挤坏)。

**F156-b**
- [ ] 分屏 ≥2 块,焦点在 A;在 B 的标题条上点「换节点」选一台 → 连上后光标/焦点边框在 **B**,直接敲字进的是 B。
- [ ] 拔网线/断链让 B 断线自动重连(F128)时,焦点**不动**(仍在你当时用的那块)。

**F156-c**(用一台**没开 tmux**、bash 的远端)
- [ ] 新开一个终端标签,登录后屏幕被清了一次(看不到 motd),没有多余的命令回显残留在屏幕上。
- [ ] `history 5` 里**没有** `__mullion_osc7` 那一长串(`HISTCONTROL` 含 `ignorespace` 的机器上)。
- [ ] `cd /var/log` 之后按 `Ctrl+Shift+B`,SFTP 面板开在 `/var/log`,**不是** `/home/<你>`。
- [ ] 分屏出来的 pane、以及换过节点的 pane,同样跟得住(三条路径都走 `on_pane_ready`)。
- [ ] 配了登录后命令(F40~F44)的会话:命令的输出**没有**被清屏吃掉(注入排在自动化之前)。
- [ ] 设置 → 远端:两个勾选框各自独立;把新的这个关掉、重启 exe,它仍然是关着的,且不再注入(屏幕不清屏了)。
- [ ] 高延迟代理链路下重复一次上面第一条 —— 注入与登录后命令的先后顺序是否还稳。
- [ ] (可选)找一台 fish 的远端:确认那行报错的样子可以接受,关掉开关后消失。

---

## 自查记录(写计划时跑过的)

- **Spec 覆盖**:spec 的 a/b/c 三节各对应 Task 1 / Task 2 / Task 3~8;「不做的事」四条没有任何任务去碰;「顺带修正」对应 Task 9 Step 1。
- **占位符**:全文无 TBD / TODO / 「类似 Task N」。每一步该改的代码都给了字面量。
- **类型一致**:`HistoryOut::Dismiss`(既有)、`SettingsDraft::shell_osc7_bootstrap`、`Settings::shell_osc7_bootstrap`、`shell_bootstrap::{OSC7_SETUP, osc7_setup_line}`、`App::on_pane_ready` 五个名字在全文各处拼写一致。
- **已知的脆点**(实现时若与实际不符,以代码为准并在提交信息里说明):
  1. Task 1 的 accesskit label `"Close window"` 来自 egui 0.30.0 的 `containers/window.rs`;脚手架在取不到时 panic 并打印全部 label,不会静默。
  2. Task 6 的 `FRAMES = 8` 在弹窗又长两行后可能不够,Step 7 给了处置(提帧数,**不**改断言)。
  3. Task 7 的 `sink.write` 可能与 `PtyWriter::write` 撞名,Step 3 给了完全限定写法。

