# IME 寿命修复 + 文件面板多选可见性 实现计划(F149~F151)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让窗口的 IME 常开(修「用着用着中文就打不出来」)、让 egui 输入框里的中文不再漏进远端 shell、让文件面板的多选看得见并且拖得出反馈。

**Architecture:** 三块互不依赖。① `app.rs` 在 `handle_platform_output` 之前按住 egui 的 IME 账本,并给 `WindowEvent::Ime` 补上输入分流;② `theme.rs` + `ui/files_panel.rs` 的 `row()` 换选中高亮,`show()` 末尾加栏底状态行;③ `files/drag.rs` 出文案、`show()` 画拖拽预览。判据一律抽成纯函数放在能单测的层(`input.rs` / `files/state.rs` / `files/drag.rs` / `app.rs` 的 `_of` 自由函数),渲染层只接线。

**Tech Stack:** Rust / winit 0.30.13 / egui + egui-winit 0.30.0 / wgpu / glyphon。测试用既有的 `egui::Context::run` 双帧模式(不是 kittest)。

**设计依据:** `docs/superpowers/specs/2026-08-21-ime-lifetime-and-multiselect-design.md`

---

## 文件结构

| 文件 | 职责 | 动作 |
|---|---|---|
| `crates/mullion-app/src/input.rs` | 新增 `ime_ledger_clamp` —— 这一帧该往 egui 的 IME 账本写什么 | 修改(在 `ime_commit_bytes` 之后) |
| `crates/mullion-app/src/app.rs` | ① 账本按压的接线点(`handle_platform_output` 之前)② `ime_goes_to_terminal_of` 自由函数 + `WindowEvent::Ime` 分支补分流 | 修改 |
| `crates/mullion-app/src/theme.rs` | 新增 `sel_alpha` 字段 + `selection_fill()` | 修改 |
| `crates/mullion-app/src/ui/files_panel.rs` | `row()` 换高亮、`show()` 加栏底状态行与拖拽预览 | 修改 |
| `crates/mullion-app/src/files/state.rs` | `PaneState::status_text()` | 修改 |
| `crates/mullion-app/src/files/drag.rs` | `preview_label()` | 修改 |
| `spec.md` | 登记 F149 / F150 / F151 | 修改 |
| `CLAUDE.md` | 陷阱表加 T10(egui 会关掉宿主的 IME) | 修改 |

---

## Task 1:F149 —— 把 egui 的 IME 账本按住

**背景(实现者必读):** `egui-winit 0.30.0` `src/lib.rs:848` 的去抖长这样:

```rust
let allow_ime = ime.is_some();          // 目标值:egui 里有没有文本框在组字
if self.allow_ime != allow_ime {        // 账本 ≠ 目标 → 发调用
    self.allow_ime = allow_ime;
    window.set_ime_allowed(allow_ime);
}
```

账本初值 `false`,而我们在 `resumed`(`app.rs:5689`)里调过一次
`set_ime_allowed(true)` —— 两边从一开始就不一致。用户点一次任意 egui 输入框
再离开,egui 就会发 `set_ime_allowed(false)`,把**整个窗口**的 IME 关掉,
终端从此收不到 `WindowEvent::Ime`,中文永久打不出来,只能重启。

**要短路那次禁用调用,得把账本写成和目标值相同的 `false`**(骗它「已经关过了」)。
写 `true` 是反的:那会制造 `true != false`,禁用调用每帧必发,bug 从「用过输入框
才触发」恶化成「从第一帧起就没有中文」。

**Files:**
- Modify: `crates/mullion-app/src/input.rs`(在 `ime_commit_bytes` 函数之后)
- Modify: `crates/mullion-app/src/app.rs:9259`(`handle_platform_output` 调用之前)
- Test: `crates/mullion-app/src/input.rs` 的 `mod tests`

- [ ] **Step 1: 写失败的测试**

加到 `crates/mullion-app/src/input.rs` 的 `mod tests` 里:

```rust
/// F149:egui 不要 IME 的那些帧,账本必须被写成 **false**。
///
/// 写 `true` 是反的 —— egui 的去抖是「账本 ≠ 目标值才发调用」,目标值这时
/// 正是 `false`,写 true 反而每帧都触发一次 `set_ime_allowed(false)`,
/// 窗口从第一帧起就没有 IME。这条断言钉的就是这个方向。
#[test]
fn the_ime_ledger_is_clamped_to_false_so_egui_never_disables_the_window_ime() {
    assert_eq!(
        ime_ledger_clamp(false),
        Some(false),
        "egui 不要 IME 时,账本要写成与目标值相同的 false,去抖才会短路"
    );
}

/// egui 自己要 IME 的帧不许动账本:它这时要发的是 `set_ime_allowed(true)`,
/// 对一个本就开着的窗口无害,插手只会让两边的账再次对不上。
#[test]
fn the_ime_ledger_is_left_alone_while_egui_is_composing() {
    assert_eq!(ime_ledger_clamp(true), None);
}
```

- [ ] **Step 2: 跑测试确认它失败**

```bash
cargo test -p mullion-app --lib input::tests::the_ime_ledger 2>&1 | tail -20
```

Expected: 编译失败,`cannot find function `ime_ledger_clamp` in this scope`

- [ ] **Step 3: 写实现**

加到 `crates/mullion-app/src/input.rs`,紧接在 `ime_commit_bytes` 之后:

```rust
/// F149:这一帧该往 `egui_winit::State` 的 IME 账本里写什么。`None` = 别动它。
///
/// **窗口的 IME 归宿主所有,egui 不许关它。** egui-winit 的去抖是
/// 「账本 ≠ 目标值才发 `set_ime_allowed`」(`lib.rs:849`)。egui 里没有文本框
/// 在组字的帧,目标值是 `false`;把账本预先写成同一个 `false`,那次调用就
/// 发不出去,窗口保持 `resumed` 里设的常开。
///
/// 终端不是 egui 部件,egui 永远不会知道它也需要 IME —— 不按住账本的话,
/// 用户点过一次任意输入框(换节点搜索框、路径条、标签改名、会话管理器字段)
/// 再点回终端,中文输入就永久没了,且**没有自愈路径**,只能重启。
///
/// 返回 `Some(true)` 是**反的**:那会制造 `true != false`,禁用调用每帧必发。
/// 这是复核阶段真的写反过一次的地方,`the_ime_ledger_is_clamped_to_false_...`
/// 钉着方向。
pub fn ime_ledger_clamp(egui_wants_ime: bool) -> Option<bool> {
    if egui_wants_ime {
        None
    } else {
        Some(false)
    }
}
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p mullion-app --lib input::tests::the_ime_ledger 2>&1 | tail -5
```

Expected: `test result: ok. 2 passed`

- [ ] **Step 5: 接线到 app.rs**

打开 `crates/mullion-app/src/app.rs`,找到这一段(约 9256~9261 行):

```rust
    crate::ui::annotate::ingest_accesskit(
        &a.egui_ctx,
        full_output.platform_output.accesskit_update.take(),
    );
    a.egui_state
        .handle_platform_output(&a.window, full_output.platform_output);
```

在 `a.egui_state.handle_platform_output(...)` **之前**插入:

```rust
    // F149:把 egui 的 IME 账本按住。egui-winit 的去抖是「账本 ≠ 目标值才发
    // `set_ime_allowed`」——egui 里没有文本框在组字的帧,它会发一次
    // `set_ime_allowed(false)`,关掉的是**整个窗口**的 IME。终端不是 egui
    // 部件,egui 永远不知道它也需要 IME,于是用户点过一次任意输入框再点回
    // 终端,中文输入就永久没了(没有自愈路径,只能重启 exe)。
    //
    // 把账本预先写成它这一帧本来要写的 `false`,去抖短路,那次调用发不出去,
    // 窗口保持 `resumed` 里设的常开。**必须排在 `handle_platform_output`
    // 之前** —— 排在后面等于什么都没做(它读的是调用当时的账本),而且照样
    // 编译、照样静默失灵,守护见 `the_ime_ledger_is_clamped_before_egui_...`。
    if let Some(v) = input::ime_ledger_clamp(full_output.platform_output.ime.is_some()) {
        a.egui_state.set_allow_ime(v);
    }
    a.egui_state
        .handle_platform_output(&a.window, full_output.platform_output);
```

- [ ] **Step 6: 写「调用顺序」的源码级守护**

加到 `crates/mullion-app/src/app.rs` 的 `mod tests` 里:

```rust
/// F149:账本必须在 `handle_platform_output` **之前**按住。
///
/// 顺序错了这个修复完全失效 —— 那个函数读的是调用当时的账本,之后再改
/// 一点用都没有 —— 而且照样编译、测试照样能跑,只有实机打不出中文才暴露。
///
/// 锚点**必须带行首换行 + 缩进**:不带的话会匹配到本测试自己那一行
/// (`include_str!("app.rs")` 读的就是这个文件),`find` 拿到测试的位置,
/// 断言变成拿测试自己跟实现比,恒绿。这里的字面量里 `\n` 是转义序列,
/// 测试自身那一行含的是反斜杠加 n 两个字符,匹配不上真换行,是安全的。
#[test]
fn the_ime_ledger_is_clamped_before_egui_gets_a_chance_to_disable_it() {
    let src = include_str!("app.rs");
    let clamp = src
        .find("\n    if let Some(v) = input::ime_ledger_clamp(")
        .expect("找不到 F149 的账本按压接线");
    let hpo = src
        .find("\n        .handle_platform_output(")
        .expect("找不到 handle_platform_output 的调用");
    assert!(
        clamp < hpo,
        "ime_ledger_clamp 必须排在 handle_platform_output 之前,否则账本改了也没人读,\
         中文输入照样会被 egui 关掉"
    );
}
```

- [ ] **Step 7: 跑测试确认通过**

```bash
cargo test -p mullion-app --lib the_ime_ledger 2>&1 | tail -5
```

Expected: `test result: ok. 3 passed`

- [ ] **Step 8: 自证守护会变红**

把 `app.rs` 里刚插入的那三行 `if let Some(v) = ...` 整段**剪切**到
`a.egui_state.handle_platform_output(...)` 那两行**之后**,重跑:

```bash
cargo test -p mullion-app --lib the_ime_ledger_is_clamped_before 2>&1 | tail -8
```

Expected: FAIL(`ime_ledger_clamp 必须排在 handle_platform_output 之前…`)

确认变红后**改回来**,重跑确认恢复绿。

- [ ] **Step 9: 提交**

```bash
git add crates/mullion-app/src/input.rs crates/mullion-app/src/app.rs
git commit -m "$(cat <<'EOF'
fix(app): 按住 egui 的 IME 账本,窗口 IME 不再被关掉 (F149)

egui-winit 0.30 在没有文本框组字的帧会 set_ime_allowed(false),关的是整个
窗口的 IME。终端不是 egui 部件,egui 不知道它也需要 —— 用户点过一次任意
输入框再回终端,中文永久打不出来,只能重启。把账本预先写成它要写的 false,
去抖短路。跑的守护:the_ime_ledger_is_clamped_to_false_so_egui_never_...
与 the_ime_ledger_is_clamped_before_egui_gets_a_chance_to_disable_it。
EOF
)"
```

---

## Task 2:F149 —— `WindowEvent::Ime` 补上输入分流

**背景:** `app.rs:6666` 的分流里 `is_kbd` 只匹配 `WindowEvent::KeyboardInput`。
`Ime` 事件落进 else 分支被喂给 egui,**然后**又走到 `app.rs:6923` 的
`WindowEvent::Ime` 分支无条件写进焦点 pane 的 PTY。后果:在会话管理器 /
标签改名 / 路径条里打的中文**同时**上屏和被发到远端 shell。

**Files:**
- Modify: `crates/mullion-app/src/app.rs`(自由函数放在 `effective_focus_of` 附近;分支改在 `WindowEvent::Ime` 处)
- Test: `crates/mullion-app/src/app.rs` 的 `mod tests`

- [ ] **Step 1: 写失败的测试**

加到 `crates/mullion-app/src/app.rs` 的 `mod tests` 里:

```rust
/// F149:IME 事件该不该落到终端,判据与普通键盘完全一致 —— 它就是键盘输入
/// 的一种,只是走了另一条 winit 事件。四种组合都断言:只测一种的话,一份
/// 「恒 true」或「恒 false」的实现有一半概率蒙混过去。
#[test]
fn ime_reaches_the_terminal_only_when_the_keyboard_would() {
    use crate::shell::input_route::Focus;
    assert!(
        ime_goes_to_terminal_of(Focus::Terminal, false, false),
        "终端聚焦、无模态、egui 不要键盘 → 中文该进终端"
    );
    assert!(
        !ime_goes_to_terminal_of(Focus::Terminal, false, true),
        "egui 文本框正拿着键盘焦点 → 中文是打给它的,不能同时发到远端 shell"
    );
    assert!(
        !ime_goes_to_terminal_of(Focus::FilesPanel, false, false),
        "焦点在文件面板 → 不该往终端写"
    );
    assert!(
        !ime_goes_to_terminal_of(Focus::Terminal, true, false),
        "模态弹窗开着 → 一切归 egui"
    );
}
```

- [ ] **Step 2: 跑测试确认它失败**

```bash
cargo test -p mullion-app --lib ime_reaches_the_terminal 2>&1 | tail -20
```

Expected: 编译失败,`cannot find function `ime_goes_to_terminal_of``

- [ ] **Step 3: 写实现(自由函数)**

加到 `crates/mullion-app/src/app.rs`,紧跟在 `effective_focus_of` 函数之后:

```rust
/// F149:这次 IME 事件该不该落到终端。
///
/// **判据直接复用 `route_focused`,不另起一套。** IME 就是键盘输入的一种,
/// 只是走了 `WindowEvent::Ime` 这条另外的路;两套判据迟早会在维护里分叉,
/// 而分叉的后果是「某些情况下中文又漏进远端 shell」这种间歇性、极难查的故障。
///
/// 抽成不依赖 `App` 的自由函数是为了能单测 —— `App` 在无头环境里造不出来。
fn ime_goes_to_terminal_of(
    focus: shell::input_route::Focus,
    modal_open: bool,
    egui_wants_keyboard: bool,
) -> bool {
    matches!(
        shell::input_route::route_focused(
            focus,
            modal_open,
            egui_wants_keyboard,
            false,
            shell::input_route::InputKind::Keyboard,
        ),
        shell::input_route::Route::Terminal
    )
}
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p mullion-app --lib ime_reaches_the_terminal 2>&1 | tail -5
```

Expected: `test result: ok. 1 passed`

- [ ] **Step 5: 接线 —— 改 `WindowEvent::Ime` 分支**

打开 `crates/mullion-app/src/app.rs`,找到 `WindowEvent::Ime(ime) => {`(约 6923 行)。
把整个分支替换成下面这段(原有的 `match &ime {...}` 内容一字不改地搬进
`if to_terminal` 里):

```rust
            // 输入法(F21):中文/日文的字是从这条路进来的,不是 `KeyboardInput`。
            // `set_ime_allowed(true)` 在 `resumed` 里开,egui 想关掉它的那次调用
            // 被 F149 的账本按压拦住了(见 `input::ime_ledger_clamp`)。
            WindowEvent::Ime(ime) => {
                // F149:这条事件此前**没有**过输入分流 —— 上面的 `is_kbd` 只匹配
                // `KeyboardInput`,于是 Ime 一路喂给了 egui、又一路写进焦点 pane
                // 的 PTY:在会话名 / 标签改名 / 路径条里打的中文会同时上屏和发到
                // 远端 shell(混在命令行里不显眼,所以一直没人报)。
                let to_terminal = ime_goes_to_terminal_of(
                    self.effective_focus(),
                    self.modal_open(),
                    self.active
                        .as_ref()
                        .is_some_and(|a| a.egui_ctx.wants_keyboard_input()),
                );
                if to_terminal {
                    match &ime {
                        winit::event::Ime::Preedit(text, _) => {
                            self.ime.on_preedit(text);
                            // F125:拼音现在内联上屏,组字中敲字母也算「输入」——
                            // 不重置的话光标可能闪到暗周期,用户敲拼音时观感是丢帧。
                            self.last_input_at = Instant::now();
                        }
                        winit::event::Ime::Commit(text) => {
                            self.ime.on_commit();
                            // F125:输入法提交也是输入,重置闪烁相位。
                            self.last_input_at = Instant::now();
                            // 组字结果按用户输入对待:先回底部(否则「打了但看不到」,
                            // 与按键/粘贴同一条口径),再写焦点 pane。
                            if let Some(bytes) = input::ime_commit_bytes(text) {
                                if let Some(pane) =
                                    self.active_ws_mut().and_then(Workspace::focused_mut)
                                {
                                    pane.emulator.selection_clear();
                                    pane.emulator.scroll_to_bottom();
                                    let _ = pane.pty.write(bytes);
                                    // F40:用户接管,自动化让位(借用已释放)。
                                    self.user_took_over();
                                }
                            }
                        }
                        winit::event::Ime::Enabled => {}
                        winit::event::Ime::Disabled => self.ime.on_disabled(),
                    }
                    // F126:preedit 串变了,候选框该跟去拼音串末尾——不补这一句,
                    // 候选框位置要等下一次别的事件才更新,组字时肉眼可见地滞后一拍。
                    self.apply_ime_cursor_area();
                } else {
                    // 这串拼音是打给 egui 的。终端侧的组字状态**必须清掉**:
                    // 留着的话 F126 会把它内联画在终端光标处(用户在会话名框里
                    // 打字,终端上跟着显示拼音),而且 `swallows_key()` 恒 true
                    // 会让终端永久吞键 —— 与 `ImeState` 少认一条结束边同一类故障。
                    self.ime.on_disabled();
                    // 候选框位置的记账作废:egui 自己会调 `set_ime_cursor_area`
                    // 把框摆到它的文本框那儿(egui-winit `lib.rs:855`),而我们的
                    // `ime_cursor_area` 没跟着变。不作废的话,回到终端组字时若算出
                    // 的 area 与记账值相同,`apply_ime_cursor_area` 会在第一行早退,
                    // 候选框一直停在那个文本框原来的位置。
                    self.ime_cursor_area = None;
                }
                self.request_ui_redraw();
            }
```

- [ ] **Step 6: 写「不走终端时必须清组字状态」的守护**

加到 `crates/mullion-app/src/app.rs` 的 `mod tests` 里:

```rust
/// F149:IME 不归终端的那条分支,必须清掉组字状态并作废候选框记账。
///
/// 少了 `on_disabled()`,终端会永久吞键(`swallows_key()` 恒 true)且把别人的
/// 拼音内联画在自己光标处;少了 `ime_cursor_area = None`,回到终端组字时候选框
/// 会停在 egui 文本框原来的位置。两条都编译得过、都只有人眼能发现。
///
/// `App` 在无头环境造不出来,只能扎源码结构。按 `} else {` 切出那条分支的体,
/// 并断言切出来的确实比整段短(否则退化成扫全文件,恒绿)。
#[test]
fn ime_that_is_not_for_the_terminal_clears_composition_and_invalidates_the_candidate_box() {
    let src = include_str!("app.rs");
    let after = src
        .split("                let to_terminal = ime_goes_to_terminal_of(")
        .nth(1)
        .expect("找不到 F149 的 IME 分流接线");
    let else_body = after
        .split("                } else {")
        .nth(1)
        .expect("找不到 IME 不归终端的那条分支");
    let else_body = &else_body[..else_body
        .find("\n                }")
        .expect("找不到 else 分支的结尾")];
    assert!(
        else_body.len() < after.len(),
        "没切出分支体,断言会退化成扫全文件"
    );
    assert!(
        else_body.contains("self.ime.on_disabled();"),
        "IME 不归终端时必须清组字状态,否则终端永久吞键:{else_body}"
    );
    assert!(
        else_body.contains("self.ime_cursor_area = None;"),
        "IME 不归终端时必须作废候选框记账,否则候选框停在 egui 文本框那儿:{else_body}"
    );
}
```

- [ ] **Step 7: 跑测试确认通过**

```bash
cargo test -p mullion-app --lib ime_ 2>&1 | tail -8
```

Expected: 全部 pass

- [ ] **Step 8: 自证守护会变红**

把接线里的 `self.ime.on_disabled();` 那一行注释掉,重跑:

```bash
cargo test -p mullion-app --lib ime_that_is_not_for_the_terminal 2>&1 | tail -8
```

Expected: FAIL(`IME 不归终端时必须清组字状态…`)。确认后**改回来**。

- [ ] **Step 9: 提交**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "$(cat <<'EOF'
fix(app): IME 事件补上输入分流,中文不再漏进远端 shell (F149)

is_kbd 只匹配 KeyboardInput,Ime 事件绕过了分流:喂给 egui 之后又无条件写
进焦点 pane 的 PTY —— 在会话名/标签改名/路径条里打的中文会同时上屏和发到
远端。判据复用既有的 route_focused,不另起一套。不归终端时清组字状态并作废
候选框记账。跑的守护:ime_reaches_the_terminal_only_when_the_keyboard_would
与 ime_that_is_not_for_the_terminal_clears_composition_and_...
EOF
)"
```

---

## Task 3:F150 —— 选中高亮换成 accent 半透明 + 左侧色条

**背景:** `row()` 现在画的选中底色是 `t.sunken_bg`(`#0e1018`),而面板底
`t.panel_bg` 是 `#14161f` —— 选中行**比背景还暗 6 个亮度单位**,人眼分辨不出来。
用户因此以为文件面板根本没有多选(实际上 `click_row` 的 Ctrl/Shift 语义早就有,
且有单测)。

**Files:**
- Modify: `crates/mullion-app/src/theme.rs`
- Modify: `crates/mullion-app/src/ui/files_panel.rs:1003-1007`(`row()` 的选中绘制)
- Test: `crates/mullion-app/src/ui/files_panel.rs` 的 `mod tests`

- [ ] **Step 1: 加 theme token(无测试步,纯数据)**

在 `crates/mullion-app/src/theme.rs` 的 `Theme` 结构里,紧跟 `stroke_alpha` /
`divider` 那一组之后加字段:

```rust
    /// ④ F150:多选高亮填充的 alpha(画在 `accent` 上)。
    ///
    /// **F80 冻结色表里没有这一项**,和 `divider` 一样是新增项、不是改既有值。
    /// 原来选中行画的是 `sunken_bg`(#0e1018),比 `panel_bg`(#14161f)还暗
    /// 6 个亮度单位,笔记本屏上人眼分辨不出来 —— 用户因此以为文件面板没有
    /// 多选功能。不新造色相,只给既有的 `accent` 配一个 alpha。
    /// 守护:`a_selected_row_is_painted_with_the_accent_fill_not_the_sunken_bg`。
    pub sel_alpha: u8,
```

在 `MULLION_DARK` 常量里,紧跟 `divider: ...` 之后加:

```rust
    sel_alpha: 51, // 0.2 × 255 ≈ 51
```

在 `pub fn stroke(t: &Theme)` 之后加辅助函数:

```rust
/// F150:多选高亮的行底色(`accent` + 低 alpha)。
///
/// 用 `from_rgba_unmultiplied`:`sel_alpha` 是「相对于底下那层的不透明度」,
/// 不是预乘值。写成 `Color32::from_rgba_premultiplied` 会得到一个几乎全黑的
/// 颜色 —— 编译过、画得出、就是看不见,和它要修的那个 bug 一模一样。
pub fn selection_fill(t: &Theme) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(t.accent.r, t.accent.g, t.accent.b, t.sel_alpha)
}
```

- [ ] **Step 2: 确认编译过**

```bash
cargo build -p mullion-app 2>&1 | tail -5
```

Expected: 编译成功(如报 `MULLION_DARK` 缺字段,说明常量里那行没加上)

- [ ] **Step 3: 写失败的测试**

加到 `crates/mullion-app/src/ui/files_panel.rs` 的 `mod tests` 里。
先加一个 helper(放在既有 `count_stroked_rects` 旁边):

```rust
    /// 数一数有多少个指定填充色的矩形。`min_w` 用来把宽窄不同的两种矩形
    /// 分开(整行底色 vs 2pt 的左侧色条)——只判颜色的话,色条和底色都算
    /// 进去,「色条没画」这种退化验不出来。
    fn count_filled_rects(
        shapes: &[egui::epaint::ClippedShape],
        color: egui::Color32,
        w_range: std::ops::RangeInclusive<f32>,
    ) -> usize {
        shapes
            .iter()
            .filter(|s| match &s.shape {
                egui::epaint::Shape::Rect(r) => {
                    r.fill == color && w_range.contains(&r.rect.width())
                }
                _ => false,
            })
            .count()
    }
```

再加测试:

```rust
    /// F150:选中行必须画成 accent 半透明 + 左侧 2pt 实色色条。
    ///
    /// 原来画的是 `sunken_bg`(#0e1018),比 `panel_bg`(#14161f)还暗 —— 用户
    /// 报「按 Ctrl 点,屏幕上完全没变化」,根因就在这儿。这条测试拿颜色本身
    /// 当判据,换回任何一个比背景暗的 token 都会红。
    #[test]
    fn a_selected_row_is_painted_with_the_accent_fill_not_the_sunken_bg() {
        let t = crate::theme::MULLION_DARK;
        let mut frame = two_columns();
        frame
            .remote
            .selected
            .insert(RemotePath::from_bytes(b"b.txt".to_vec()));
        let ctx = egui::Context::default();
        let mut cols = ColWidths::default();
        let mut render = |frame: &mut PanelFrame| {
            let mut out = None;
            let o = ctx.run(raw(None), |ctx| {
                out = Some(content(ctx, &t, 1, true, frame, 0, &mut cols, &mut None));
            });
            let _ = out;
            o
        };
        let _ = render(&mut frame);
        let out = render(&mut frame);

        assert_eq!(
            count_filled_rects(&out.shapes, crate::theme::selection_fill(&t), 20.0..=f32::MAX),
            1,
            "选中的那一行该有一块 accent 半透明的整行底色"
        );
        assert_eq!(
            count_filled_rects(&out.shapes, crate::theme::c32(t.accent), 1.0..=3.0),
            1,
            "选中的那一行该有一条 2pt 宽的 accent 实色左侧色条"
        );
        assert_eq!(
            count_filled_rects(&out.shapes, crate::theme::c32(t.sunken_bg), 20.0..=f32::MAX),
            0,
            "不该再用 sunken_bg 画选中行 —— 它比 panel_bg 还暗,等于没画"
        );
    }
```

- [ ] **Step 4: 跑测试确认它失败**

```bash
cargo test -p mullion-app --lib a_selected_row_is_painted 2>&1 | tail -12
```

Expected: FAIL(`选中的那一行该有一块 accent 半透明的整行底色`,左值 0)

- [ ] **Step 5: 写实现**

在 `crates/mullion-app/src/ui/files_panel.rs` 的常量区(`const ROW_H: f32 = 22.0;`
那一行附近)加:

```rust
/// F150:选中行左侧色条的宽度(逻辑点)。
const SEL_BAR_W: f32 = 2.0;
```

把 `row()` 里这一段(约 1003~1007 行):

```rust
    if selected {
        ui.painter().rect_filled(rect, 2.0, theme::c32(t.sunken_bg));
    }
```

替换成:

```rust
    if selected {
        // F150:accent 半透明铺满整行 + 行首一条实色。原来画 `sunken_bg`,
        // 比 `panel_bg` 还暗 6 个亮度单位,肉眼分辨不出来 —— 用户因此以为
        // 文件面板根本没有多选(`click_row` 的 Ctrl/Shift 语义一直都在)。
        // 色条不是装饰:底色再淡也可能被行内容夺走注意力,一条实色边给出
        // 「这一段是选中的」的轮廓,连选多行时一眼能看出范围。
        ui.painter()
            .rect_filled(rect, 2.0, theme::selection_fill(t));
        ui.painter().rect_filled(
            egui::Rect::from_min_size(rect.left_top(), egui::vec2(SEL_BAR_W, rect.height())),
            0.0,
            theme::c32(t.accent),
        );
    }
```

- [ ] **Step 6: 跑测试确认通过**

```bash
cargo test -p mullion-app --lib a_selected_row_is_painted 2>&1 | tail -5
```

Expected: `test result: ok. 1 passed`

- [ ] **Step 7: 跑整个面板的测试,确认没打破既有断言**

```bash
cargo test -p mullion-app --lib files_panel 2>&1 | tail -8
```

Expected: 全绿

- [ ] **Step 8: 提交**

```bash
git add crates/mullion-app/src/theme.rs crates/mullion-app/src/ui/files_panel.rs
git commit -m "$(cat <<'EOF'
feat(app): 选中行改 accent 半透明 + 左侧色条 (F150)

原来画 sunken_bg(#0e1018),比 panel_bg(#14161f)还暗 6 个亮度单位,人眼
分辨不出来 —— 用户报「按 Ctrl 点屏幕完全没变化」,根因是看不见而不是没选上。
新增 sel_alpha token(不新造色相,给 accent 配 alpha)。跑的守护:
a_selected_row_is_painted_with_the_accent_fill_not_the_sunken_bg。
EOF
)"
```

---

## Task 4:F150 —— 栏底状态行的文案(纯函数)

**Files:**
- Modify: `crates/mullion-app/src/files/state.rs`(`PaneState` 的 impl 块,放在 `picked_entries` 之后)
- Test: `crates/mullion-app/src/files/state.rs` 的 `mod tests`

- [ ] **Step 1: 写失败的测试**

加到 `crates/mullion-app/src/files/state.rs` 的 `mod tests` 里:

```rust
    /// 没选中时状态行报的是**可见行数**,不是 `entries.len()` ——
    /// 关着隐藏文件时两者不一样,报存储数就跟用户眼睛看到的对不上。
    #[test]
    fn the_status_line_counts_visible_rows_when_nothing_is_selected() {
        let mut s = PaneState::new(rp("/"));
        s.entries = vec![
            e("a", EntryKind::File),
            e("b", EntryKind::File),
            e(".hidden", EntryKind::File),
        ];
        s.load = Load::Ready;
        assert_eq!(s.status_text(), "2 项", "隐藏文件不该计进去");
        s.show_hidden = true;
        assert_eq!(s.status_text(), "3 项");
    }

    /// 选中时报条数 + 体积。体积只算文件 —— 目录的 `size` 在 SFTP 里是元数据
    /// 大小(常见 4096),加进去给出的是一个没有意义的数。
    #[test]
    fn the_status_line_reports_the_selection_size_counting_files_only() {
        let mut s = PaneState::new(rp("/"));
        let mut big = e("big.bin", EntryKind::File);
        big.size = 2048;
        let mut small = e("small.txt", EntryKind::File);
        small.size = 1024;
        let mut dir = e("logs", EntryKind::Dir);
        dir.size = 4096;
        s.entries = vec![big, small, dir];
        s.load = Load::Ready;
        s.selected.insert(rp("big.bin"));
        s.selected.insert(rp("small.txt"));
        s.selected.insert(rp("logs"));
        assert_eq!(
            s.status_text(),
            "已选 3 项 · 3.0 KB",
            "3 条(含一个目录),体积只算两个文件的 2048+1024"
        );
    }

    /// 只选了目录 → **不拼体积**。拼出来是「已选 1 项 · 0 B」,而目录当然
    /// 不是 0 字节,那行字是在撒谎。
    #[test]
    fn a_directory_only_selection_omits_the_size_instead_of_claiming_zero_bytes() {
        let mut s = PaneState::new(rp("/"));
        let mut dir = e("logs", EntryKind::Dir);
        dir.size = 4096;
        s.entries = vec![dir];
        s.load = Load::Ready;
        s.selected.insert(rp("logs"));
        assert_eq!(s.status_text(), "已选 1 项");
    }
```

- [ ] **Step 2: 跑测试确认它失败**

```bash
cargo test -p mullion-app --lib files::state::tests::the_status_line 2>&1 | tail -12
```

Expected: 编译失败,`no method named `status_text``

- [ ] **Step 3: 写实现**

加到 `crates/mullion-app/src/files/state.rs` 的 `impl PaneState` 里,
紧跟 `picked_entries` 之后:

```rust
    /// F150:栏底状态行的文案。
    ///
    /// 这一行同时是**用户唯一能看见的选中证据** —— 用户报过「按 Ctrl 点,
    /// 屏幕上完全没变化」,当时高亮画得比背景还暗,除了这行字之外没有任何
    /// 途径能分辨「没选上」和「选上了但看不见」。
    ///
    /// 两条口径:
    /// - 计数按**可见行**(`rows()`),不是 `entries.len()` —— 关着隐藏文件时
    ///   两者不一样,报存储数跟用户眼睛看到的对不上。
    /// - 体积只算文件。目录的 `size` 在 SFTP 里是元数据大小(常见 4096),
    ///   加进去给出的是个没有意义的数;而全选目录时干脆不拼体积,拼出来
    ///   「· 0 B」是在撒谎。
    pub fn status_text(&self) -> String {
        let rows = self.rows();
        let picked: Vec<&Entry> = rows
            .iter()
            .copied()
            .filter(|e| self.selected.contains(&e.name))
            .collect();
        if picked.is_empty() {
            return format!("{} 项", rows.len());
        }
        let bytes: u64 = picked
            .iter()
            .filter(|e| e.kind != EntryKind::Dir)
            .map(|e| e.size)
            .sum();
        let has_file = picked.iter().any(|e| e.kind != EntryKind::Dir);
        if has_file {
            format!("已选 {} 项 · {}", picked.len(), super::human_size(bytes))
        } else {
            format!("已选 {} 项", picked.len())
        }
    }
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p mullion-app --lib files::state::tests 2>&1 | tail -5
```

Expected: 全绿(含既有的选择集测试)

- [ ] **Step 5: 确认字形白名单没被踩(T9)**

```bash
cargo test -p mullion-app --test glyph_whitelist 2>&1 | tail -5
```

Expected: PASS(`·` U+00B7 已在 `ui::glyphs::VERIFIED` 里登记过)

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-app/src/files/state.rs
git commit -m "$(cat <<'EOF'
feat(app): 栏底状态行的文案纯函数 (F150)

计数按可见行(不是 entries.len(),关着隐藏文件时两者不同);体积只算文件
(目录的 size 是元数据大小);全是目录时不拼体积(「· 0 B」是在撒谎)。
跑的守护:the_status_line_counts_visible_rows_when_nothing_is_selected /
the_status_line_reports_the_selection_size_counting_files_only /
a_directory_only_selection_omits_the_size_instead_of_claiming_zero_bytes。
EOF
)"
```

---

## Task 5:F150 —— 状态行接线 + Ctrl 多选的端到端守护

**Files:**
- Modify: `crates/mullion-app/src/ui/files_panel.rs`(`show()` 里 ScrollArea 的高度 + 函数末尾)
- Test: `crates/mullion-app/src/ui/files_panel.rs` 的 `mod tests`

- [ ] **Step 1: 写失败的测试**

先加一个能带修饰键的 helper(放在既有 `press` / `moved` 旁边):

```rust
    /// 带修饰键的点击。**修饰键必须写进 `RawInput::modifiers`**,不是只写进
    /// 事件里的那份 —— `files_panel` 读的是 `ui.input(|i| i.modifiers)`,
    /// 即全局状态。只设事件里那份的话,Ctrl 位根本传不到 `click_row`,
    /// 多选静默失效而所有断言照样绿。
    fn press_mod(
        pos: egui::Pos2,
        time: f64,
        pressed: bool,
        modifiers: egui::Modifiers,
    ) -> egui::RawInput {
        let mut input = raw(Some(time));
        input.modifiers = modifiers;
        input.events.push(egui::Event::PointerMoved(pos));
        input.events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers,
        });
        input
    }

    /// Windows/Linux 上 egui 把 Ctrl 归一化到 `command` 位,`files_panel`
    /// 读的正是 `command`(写 `ctrl` 会让 macOS 用户点不出多选)。两位都置上,
    /// 与真实平台一致。
    fn ctrl() -> egui::Modifiers {
        egui::Modifiers {
            command: true,
            ctrl: true,
            ..Default::default()
        }
    }
```

再加测试:

```rust
    /// F150:**这是「Ctrl 多选」唯一的端到端守护。**
    ///
    /// `click_row` 的 Ctrl 语义在 `files::state` 里有单测,但从来没有一条
    /// 测试证明「点击真的把 ctrl 位带进去了」—— 中间隔着
    /// `ui.input(|i| i.modifiers)` 这一步,读错来源(比如读事件里那份而不是
    /// 全局状态)会让多选整个不成立,而 `files::state` 那些单测全绿。
    ///
    /// 断言打在**状态行文字**上:那是用户唯一看得见的选中证据。
    #[test]
    fn ctrl_clicking_a_second_row_adds_it_to_the_selection() {
        let t = crate::theme::MULLION_DARK;
        let mut frame = two_columns();
        let ctx = egui::Context::default();
        let mut cols = ColWidths::default();
        let mut render = |input: egui::RawInput, frame: &mut PanelFrame| {
            let o = ctx.run(input, |ctx| {
                content(ctx, &t, 1, true, frame, 0, &mut cols, &mut None);
            });
            o
        };
        let _ = render(raw(None), &mut frame);
        let out = render(raw(None), &mut frame);
        let b = find_text_pos(&out.shapes, "b.txt").expect("远端栏该画出 b.txt");
        let logs = find_text_pos(&out.shapes, "logs").expect("远端栏该画出 logs");

        // 平点 b.txt。
        let _ = render(press(b, 1.0, true), &mut frame);
        let _ = render(press(b, 1.1, false), &mut frame);
        // Ctrl 点 logs —— 该是「再加一条」,不是「换成这一条」。
        let _ = render(press_mod(logs, 1.2, true, ctrl()), &mut frame);
        let _ = render(press_mod(logs, 1.3, false, ctrl()), &mut frame);

        let out = render(raw(None), &mut frame);
        let texts: Vec<String> = out
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::epaint::Shape::Text(ts) => Some(ts.galley.text().to_owned()),
                _ => None,
            })
            .collect();
        assert!(
            texts.iter().any(|s| s == "已选 2 项"),
            "Ctrl 点第二行该把它加进选择集,状态行该显示「已选 2 项」;实际画出来的是 {texts:?}"
        );
    }
```

- [ ] **Step 2: 跑测试确认它失败**

```bash
cargo test -p mullion-app --lib ctrl_clicking_a_second_row 2>&1 | tail -12
```

Expected: FAIL(状态行还没接线,`texts` 里没有「已选 2 项」)

- [ ] **Step 3: 写实现 —— 给状态行腾高度**

在 `crates/mullion-app/src/ui/files_panel.rs` 的 `show()` 里,找到分配列头横带
那一段(约 693 行 `let (header_band, _) = ui.allocate_exact_size(`)。
在它**之前**插入:

```rust
    // F150:栏底状态行要占一行,先从可用高度里扣掉。**必须在 ScrollArea
    // 之前算** —— 它开着 `auto_shrink([false, false])`,会把当时的可用高度
    // 全部吃光,状态行排在后面就被挤到面板之外了(egui 不报错,那行字就是
    // 不见了)。`max(ROW_H)` 兜住面板被拖到极窄时的负数。
    let body_h = (ui.available_height() - ROW_H * 2.0).max(ROW_H);
```

> `ROW_H * 2.0`:一份给列头横带(紧接着就分配),一份给状态行。

- [ ] **Step 4: 写实现 —— 限制滚动区高度**

在同一个函数里找到:

```rust
    egui::ScrollArea::both()
        .id_salt(scroll_id_salt(id, generation))
```

在 `.id_salt(...)` 之后插入一行:

```rust
        .max_height(body_h)
```

- [ ] **Step 5: 写实现 —— 画状态行**

在 `show()` 里找到落点收口那一段之后、函数返回 `action` 之前的位置。
具体地,在这一段:

```rust
    if let Some(name) = drag_start {
        state.select_only(&name);
    }
    if let Some((name, ctrl, shift)) = clicked {
        state.click_row(&name, ctrl, shift);
    }
```

之后插入:

```rust
    // F150:栏底状态行。**画在 `click_row` 之后** —— 点击的效果要在同一帧
    // 就反映到这行字上,否则用户点一下看到的还是上一帧的数,像是没生效。
    ui.add_space(crate::ui::metrics::SP_XS);
    ui.colored_label(theme::c32(t.fg_dim), state.status_text());
```

- [ ] **Step 6: 跑测试确认通过**

```bash
cargo test -p mullion-app --lib ctrl_clicking_a_second_row 2>&1 | tail -8
```

Expected: `test result: ok. 1 passed`

如果 FAIL 且 `texts` 里出现的是「已选 1 项」,说明 Ctrl 位没传到 —— 检查
`press_mod` 是否设了 `input.modifiers`(而不只是事件里那份)。

- [ ] **Step 7: 自证这条端到端守护会变红**

把 `files/state.rs` 里 `click_row` 的 ctrl 分支临时改成 `self.select_only(name);`,重跑:

```bash
cargo test -p mullion-app --lib ctrl_clicking_a_second_row 2>&1 | tail -8
```

Expected: FAIL(状态行显示「已选 1 项」)。确认后**改回来**。

- [ ] **Step 8: 跑整个面板测试,确认高度改动没打破既有断言**

```bash
cargo test -p mullion-app --lib files_panel 2>&1 | tail -10
```

Expected: 全绿。若某条拖拽测试红了,多半是行的屏幕坐标随高度变了 ——
那些测试用 `find_text_pos` 现取坐标,不该受影响;真红了要看具体断言。

- [ ] **Step 9: 提交**

```bash
git add crates/mullion-app/src/ui/files_panel.rs
git commit -m "$(cat <<'EOF'
feat(app): 文件面板加栏底状态行,并给 Ctrl 多选补端到端守护 (F150)

状态行是用户唯一看得见的选中证据(用户报「Ctrl 点没反应」时,除了它没有
任何途径分辨「没选上」和「选上了看不见」)。ScrollArea 必须先扣掉这一行的
高度 —— 它 auto_shrink([false,false]) 会吃光可用高度,状态行会被挤出面板。
跑的守护:ctrl_clicking_a_second_row_adds_it_to_the_selection(端到端,
证明点击真的把 ctrl 位带进了 click_row)。
EOF
)"
```

---

## Task 6:F151 —— 拖拽跟随预览

**Files:**
- Modify: `crates/mullion-app/src/files/drag.rs`
- Modify: `crates/mullion-app/src/ui/files_panel.rs`(`show()` 里 `incoming` 判定附近)
- Test: `crates/mullion-app/src/files/drag.rs` 的 `mod tests`

- [ ] **Step 1: 写失败的测试**

加到 `crates/mullion-app/src/files/drag.rs` 的 `mod tests` 里:

```rust
    /// 拖一条时显名字、拖多条时显条数。
    ///
    /// `Response::dnd_set_drag_payload` 只挂载荷、**不画任何预览** ——
    /// 在此之前拖起来指针底下空空如也,用户分不清「拖没拖着」和「拖了几项」。
    #[test]
    fn the_drag_preview_names_a_single_file_but_counts_a_multi_selection() {
        assert_eq!(preview_label(1, "a.txt"), "a.txt");
        assert_eq!(preview_label(3, "a.txt"), "拖动 3 项");
    }

    /// 0 项是拖不起来的(起拖时没选中会先把那条选上),真走到这儿也不能
    /// 印成「拖动 0 项」——那是在报告一个不存在的动作。
    #[test]
    fn an_empty_drag_falls_back_to_the_name_instead_of_claiming_zero_items() {
        assert_eq!(preview_label(0, "a.txt"), "a.txt");
    }
```

- [ ] **Step 2: 跑测试确认它失败**

```bash
cargo test -p mullion-app --lib files::drag::tests::the_drag_preview 2>&1 | tail -10
```

Expected: 编译失败,`cannot find function `preview_label``

- [ ] **Step 3: 写实现**

加到 `crates/mullion-app/src/files/drag.rs`,紧跟 `drop_in_hint` 之后:

```rust
/// F151:拖拽途中跟着指针走的那个小胶囊上写什么。
///
/// `Response::dnd_set_drag_payload` 只挂载荷、不画预览 —— 没有这一条的话
/// 拖起来指针底下什么都没有,用户分不清「拖没拖着」「拖了几项」。
///
/// 单项显名字(用户要确认拖的是哪一个),多项显条数(名字列表在指针边上
/// 铺不下,而且这时用户关心的是「有没有把该选的都带上」)。
pub fn preview_label(n: usize, first: &str) -> String {
    if n <= 1 {
        first.to_owned()
    } else {
        format!("拖动 {n} 项")
    }
}
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p mullion-app --lib files::drag::tests 2>&1 | tail -5
```

Expected: 全绿

- [ ] **Step 5: 接线 —— 在 `show()` 里画预览**

在 `crates/mullion-app/src/ui/files_panel.rs` 的 `show()` 里,找到 `incoming`
的定义(约 486 行):

```rust
    let incoming = egui::DragAndDrop::payload::<crate::files::drag::DragFrom>(ui.ctx())
        .filter(|f| f.0 != column)
        .is_some();
```

在它**之后**插入:

```rust
    // F151:本栏正被拖 —— 在指针旁画一个跟随的小胶囊。
    //
    // 判据是「载荷来自**本栏**」:两栏都会走到这里,不区分的话同一次拖拽
    // 会被画两遍(两个胶囊叠在一起,边缘毛糙)。
    //
    // 画在 `Order::Tooltip` 层:那是 egui 里唯一保证压在所有 panel 之上的
    // 常规层,画在当前 `ui` 的 painter 上会被另一栏的背景盖掉。
    let outgoing = egui::DragAndDrop::payload::<crate::files::drag::DragFrom>(ui.ctx())
        .is_some_and(|f| f.0 == column);
    if outgoing {
        if let Some(p) = ui.ctx().pointer_latest_pos() {
            let first = state
                .selected_paths()
                .first()
                .map(|n| n.display().into_owned())
                .unwrap_or_default();
            let label = crate::files::drag::preview_label(state.selected.len(), &first);
            let painter = ui.ctx().layer_painter(egui::LayerId::new(
                egui::Order::Tooltip,
                egui::Id::new(("files-drag-preview", id)),
            ));
            let font = egui::FontId::proportional(12.0);
            let galley = painter.layout_no_wrap(label, font, theme::c32(t.accent_fg));
            // 偏移一点,别让胶囊压在指针尖底下(挡住落点行的高亮)。
            let at = p + egui::vec2(crate::ui::metrics::SP_M, crate::ui::metrics::SP_M);
            let pad = egui::vec2(crate::ui::metrics::SP_S, crate::ui::metrics::SP_XS);
            let bg = egui::Rect::from_min_size(at, galley.size() + pad * 2.0);
            painter.rect_filled(bg, 4.0, theme::c32(t.accent));
            painter.galley(at + pad, galley, theme::c32(t.accent_fg));
        }
    }
```

- [ ] **Step 6: 确认编译过 + 面板测试仍全绿**

```bash
cargo build -p mullion-app 2>&1 | tail -5 && cargo test -p mullion-app --lib files_panel 2>&1 | tail -8
```

Expected: 编译成功,面板测试全绿

- [ ] **Step 7: 提交**

```bash
git add crates/mullion-app/src/files/drag.rs crates/mullion-app/src/ui/files_panel.rs
git commit -m "$(cat <<'EOF'
feat(app): 拖拽途中在指针旁画「拖动 N 项」 (F151)

dnd_set_drag_payload 只挂载荷不画预览,拖起来指针底下什么都没有,用户分不清
拖没拖着、拖了几项。判据取「载荷来自本栏」,不然两栏各画一个叠在一起。
画在 Order::Tooltip 层——画在当前 ui 的 painter 上会被另一栏背景盖掉。
跑的守护:the_drag_preview_names_a_single_file_but_counts_a_multi_selection。
跟随渲染本身无头验不了,进人工验收清单。
EOF
)"
```

---

## Task 7:登记 F149~F151 与陷阱 T10

**Files:**
- Modify: `spec.md`
- Modify: `CLAUDE.md`

- [ ] **Step 1: 往 spec.md 的功能表加三行**

在 `spec.md` 里找到 `| F148 |` 那一行(约 157 行),在它**之后**加:

```markdown
| F149 | **窗口 IME 归宿主所有**:`egui-winit` 在没有文本框组字的帧会 `set_ime_allowed(false)`，关掉的是整个窗口的 IME —— 终端不是 egui 部件，egui 永远不知道它也需要。用户点过一次任意输入框再回终端，中文就永久打不出来（**没有自愈路径**，只能重启）。修法是在 `handle_platform_output` **之前**把 egui 的账本预写成它这一帧要写的 `false`，让它的去抖短路。同时给 `WindowEvent::Ime` 补上输入分流（此前它绕过了 `is_kbd`，喂给 egui 之后又无条件写进 PTY，在会话名/标签改名/路径条里打的中文会**同时**上屏和发到远端 shell） | P1 | `ime_ledger_clamp` 钉住写的是 **false** 不是 true（写 true 反而每帧都触发禁用调用）；源码级守护钉住它排在 `handle_platform_output` 之前（顺序错了完全失效且静默）；`ime_goes_to_terminal_of` 四种组合；不归终端的分支必须清组字状态 + 作废候选框记账 |
| F150 | **多选看得见**：选中行改 `accent` 半透明填充 + 左侧 2pt 实色色条（原来画 `sunken_bg` #0e1018，比 `panel_bg` #14161f 还暗 6 个亮度单位，人眼分辨不出来——用户因此以为文件面板根本没有多选，而 `click_row` 的 Ctrl/Shift 语义一直都在）。每栏底部加状态行：有选中显示 `已选 N 项 · 体积`（体积只算文件；全是目录时不拼，`· 0 B` 是在撒谎），无选中显示可见行数 | P2 | 拿颜色本身当判据（换回任何比背景暗的 token 都红）；状态行文案三条纯函数测试；**端到端**：平点一行 + Ctrl 点另一行 → 状态行读到「已选 2 项」（这是「点击真的把 ctrl 位带进了 `click_row`」唯一的守护，中间隔着 `ui.input(\|i\| i.modifiers)`） |
| F151 | **拖拽跟随预览**：拖动途中在指针旁画小胶囊，单项显文件名、多项显 `拖动 N 项`。`dnd_set_drag_payload` 只挂载荷不画预览，此前拖起来指针底下空空如也 | P2 | `preview_label` 单项/多项/0 项三条；跟随渲染本身无头验不了，进人工验收 |
```

- [ ] **Step 2: 往 CLAUDE.md 的陷阱表加 T10**

在 `CLAUDE.md` 的领域陷阱表里,`| T9 |` 那一行之后加:

```markdown
| T10 | 以为窗口的 IME 开关是自己说了算 | `egui-winit` 每帧按「egui 里有没有文本框在组字」调 `set_ime_allowed`，**会把整个窗口的 IME 关掉**。终端不是 egui 部件，egui 永远不知道它也需要 IME —— 用户点过一次任意输入框再回终端，中文**永久**打不出来（按 Windows 中英文切换键毫无反应），且没有自愈路径，只能重启 exe。同族的还有「`WindowEvent::Ime` 绕过输入分流」：egui 输入框里打的中文会同时发到远端 shell | `input::tests::the_ime_ledger_is_clamped_to_false_so_egui_never_disables_the_window_ime`；`app::tests::the_ime_ledger_is_clamped_before_egui_gets_a_chance_to_disable_it`（顺序错了完全失效且静默） |
```

- [ ] **Step 3: 确认字形白名单没被踩**

```bash
cargo test -p mullion-app --test glyph_whitelist 2>&1 | tail -5
```

Expected: PASS

> 注意:`spec.md` / `CLAUDE.md` 不是 `src/**/*.rs`,不在扫描范围内;
> 这一步是为了确认前面几个 Task 写进源码的中文文案没问题。

- [ ] **Step 4: 提交**

```bash
git add spec.md CLAUDE.md
git commit -m "$(cat <<'EOF'
docs: 登记 F149~F151,陷阱表加 T10 (F149/F150/F151)

T10 记的是「窗口 IME 的所有权」:egui-winit 会替你把它关掉,而症状是中文
永久打不出来、只能重启,静默且无自愈路径。
EOF
)"
```

---

## Task 8:跑绿 + 发版

- [ ] **Step 1: 全量测试**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log | tail -20
```

Expected: 所有 `test result` 行都是 `ok`,无 FAILED / panicked

- [ ] **Step 2: clippy**

```bash
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20
```

Expected: 无输出(有输出即为不绿)

- [ ] **Step 3: fmt**

```bash
cargo fmt --check 2>&1 | tail -10
```

Expected: 无输出。若有差异,跑 `cargo fmt` 后**重跑 Step 1**
—— 本项目有源码级守护测试,`fmt` 拆行会打断锚点(已踩过)。

- [ ] **Step 4: 发版**

改动落在 `mullion-app`,按交付约定一条龙走完。调用 `release-windows` skill,
它会做:升 patch 版本号(单独 `chore:` 提交)→ 跑绿 → 交叉编译 → objdump
依赖验收(出现 `libgcc_s_seh-1.dll` / `libwinpthread-1.dll` 即不合格)→
**签名**(必须在算 sha256 之前)→ push → `gh release create`(标题只能是
纯版本号 `v0.1.N`)。

- [ ] **Step 5: 交人工验收清单**

Release 说明里附上(这四条在无头容器里一条都验不了):

1. **中文输入不再失效**(F149,本轮主要交付)——
   开 exe → 打开换节点弹窗、在搜索框里点一下 → 关掉 → 回终端打中文。
   **修复前必失效**(这就是复现路径),修复后应正常。再切几次分屏、换几次节点。
2. **egui 输入框里的中文不再漏进远端**(F149)——
   双击标签改名,在框里打「测试」→ 关掉 → 看终端命令行里有没有多出这两个字。
3. **选中高亮看得清**(F150)—— Ctrl 点 3 条,一眼能数出选了几条;
   栏底状态行显示「已选 3 项 · <体积>」。
4. **拖拽预览跟手**(F151)—— 拖 3 条时指针旁显示「拖动 3 项」。

---

## 自查(写计划时已跑过)

**Spec 覆盖:** 设计文档的四个部分 —— F149 账本按压(Task 1)、F149 IME 路由
(Task 2)、F150 高亮(Task 3)+ 状态行(Task 4/5)、F151 预览(Task 6),
外加登记与发版(Task 7/8)。测试策略表里的七条守护逐条落到了具体 Task。

**类型一致性:** `ime_ledger_clamp(bool) -> Option<bool>`(Task 1 定义,
Task 1 Step 5 接线使用)、`ime_goes_to_terminal_of(Focus, bool, bool) -> bool`
(Task 2 定义并接线)、`theme::selection_fill(&Theme) -> Color32`(Task 3
定义,同 Task 使用)、`PaneState::status_text(&self) -> String`(Task 4 定义,
Task 5 接线)、`drag::preview_label(usize, &str) -> String`(Task 6 定义并接线)。
`SEL_BAR_W` / `body_h` 都在使用前定义。

**已知偏差(实现时留意):** Task 5 Step 3 扣的是 `ROW_H * 2.0`(列头 + 状态行)。
若实机看到状态行与最后一行之间空隙偏大,把 `add_space(SP_XS)` 去掉即可 ——
不要去改 `body_h` 的系数,那会让列头或状态行之一被挤掉。
