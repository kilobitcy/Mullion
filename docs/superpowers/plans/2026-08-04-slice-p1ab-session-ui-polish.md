# 切片 P1-a/P1-b 会话管理器 UI 打磨 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修掉会话管理器右栏溢出裁切的根因，把表单打磨到可用（分区/对比度/按钮主次），并加上必填校验（F91）、测试连接（F92）、`~/.ssh` 私钥扫描与拖拽（F93）。

**Architecture:** 纯逻辑抽成两个零依赖模块（`validate.rs` 判必填、`keyscan.rs` 扫私钥文件名），它们可脱离 egui 单测；UI 层只做渲染与意图写入，副作用一律在 `app.rs` 的意图施加点执行（与既有 `save_click` / `host_key_reply` 同构）。「测试连接」复用 `mullion_ssh::session::establish` 完整认证后立即 `disconnect`，靠单调递增的 `probe_epoch` 世代号丢弃过期结果；它拨的是**当前表单（含未保存改动）**，因此把 `Vault::resolve_for` / `expand_jump_chain` 各抽一个参数化内核，已保存路径改调同一内核——是重构，不是第二条解析路径。

**Tech Stack:** Rust 2021 workspace；egui 0.30.0 + egui-winit 0.30.0 + winit 0.30.13 + wgpu 23.0.1；russh 0.54.5；tokio 1.53.1；directories 5.0.1；tempfile 3.27.0（dev）。**本切片不新增任何 crate 依赖**。

**设计文档（唯一真源）：** `docs/superpowers/specs/2026-08-04-slice-p1ab-session-ui-polish-design.md`

---

## 文件结构

| 文件 | 动作 | 职责 |
|---|---|---|
| `crates/mullion-app/src/ui/session_manager/validate.rs` | 新建 | 必填项判定，纯函数、零 egui（F91） |
| `crates/mullion-app/src/ui/session_manager/keyscan.rs` | 新建 | 扫描目录下像私钥的文件名，只 `read_dir`、绝不读内容（F93） |
| `crates/mullion-app/src/ui/session_manager/mod.rs` | 修改 | 宽度地板/天花板、可拖拽分隔条、`ProbeState`、`secret_edit` 宽度与占位符、新增守护测试 |
| `crates/mullion-app/src/ui/session_manager/editor.rs` | 修改 | Tab 红点、错误/拨测卡片互斥、底部按钮条重排、拖拽落点 |
| `crates/mullion-app/src/ui/session_manager/fields.rs` | 修改 | 字段分区小标题、行距、输入框宽度、胶囊选中态、私钥行三段 |
| `crates/mullion-app/src/ui/session_manager/list.rs` | 修改 | 副文本对比度、状态点 hover tooltip |
| `crates/mullion-app/src/ui/mod.rs` | 修改 | `UiState` 新增拨测/候选/取消意图字段 |
| `crates/mullion-app/src/ui/host_key.rs` | 修改 | 「仅本次信任」文案 |
| `crates/mullion-app/src/host_key.rs` | 修改 | `PromptingPolicy::persist` + `HostKeyPrompt::persist` |
| `crates/mullion-app/src/app.rs` | 修改 | `UserEvent::ProbeOk/ProbeErr`、`spawn_probe`、`probe_epoch`/`probe_task`、指纹落盘门控 |
| `crates/mullion-app/src/shell/store.rs` | 修改 | `ssh_config_for_draft`（按草稿拨号计划） |
| `crates/mullion-store/src/vault.rs` | 修改 | 抽出 `resolve_layer` / `expand_jump_chain_of` 参数化内核 |
| `crates/mullion-store/src/jump.rs` | 修改 | 抽出 `expand_chain_of` / `expand_from` 内核 |
| `crates/mullion-store/src/inherit.rs` | 修改 | `SessionDraft` 实现 `PrefsLayer` |
| `crates/mullion-ssh/src/session.rs` | 修改 | `SshConnection::disconnect()` |
| `crates/mullion-app/Cargo.toml` | 修改 | tokio 补 `"time"` feature |
| `spec.md` | 修改 | 追加 F91/F92/F93、F3 补注 |

---

## 全局约束（每个任务都适用）

- 提交信息中文，摘要带 spec 编号，一次提交只做一件事。
- 「绿」= `cargo test --workspace` 全过 **且** `cargo clippy --workspace --all-targets -- -D warnings` 无输出 **且** `cargo fmt --check` 通过。
- 每条新增守护测试必须**自证变红**：按任务里写明的方式临时破坏被守护的属性，确认测试失败，再恢复。自证必须扎到 bug 的真实注入点，不能只改测试里的顶层参数。
- egui 测试里读 rect 必须在 `ctx.run` 闭包**内部**取值（`Context` 每 pass 会 `mem::swap` 掉 `prev_pass`）。
- `Margin` 字段在 egui 0.30 已是 `f32`，**不要写 `as f32`**（`clippy::unnecessary_cast` 会让 `-D warnings` 直接红）。

### 红线（一个字都不许动）

`const SLACK: f32 = 8.0;`、`fn window_chrome_reserve(ctx)`、`const WINDOW_TITLE`、`mod.rs:216-220` 那段高度处理、三条既有高度守护测试、测试 helper `fn new_button_rect(ctx)`、`collapsing_header_id_salt_disambiguates_same_titled_groups`。

---

## Task 1: 窗口宽度地板 + 条件式天花板（F90 根因）

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/mod.rs`
- Test: `crates/mullion-app/src/ui/session_manager/mod.rs`（`mod tests`）

- [ ] **Step 1: 写失败测试**

在 `mod.rs` 的 `mod tests` 里追加（放在 `new_button_stays_within_screen_rect_when_main_window_is_short` 之后）：

```rust
    /// F90:右栏(编辑器)不许画到窗口矩形之外。
    ///
    /// 根因是 `SidePanel::show_inside` 用 `expand_to_include_rect` 只增不减地
    /// 回报尺寸,而 `CentralPanel::show_inside` 吃掉 `available_rect_before_wrap()`
    /// 却**不回报**——窗口自身没被撑宽,右栏就直接画到窗口外被裁掉。
    ///
    /// 自证变红的方式:注释掉 `show()` 里 `ui.set_min_width(...)` 那一行。
    #[test]
    fn editor_panel_stays_within_window_rect() {
        let t = crate::theme::MULLION_DARK;
        let sessions: Vec<SessionRecord> = Vec::new();
        let groups: Vec<GroupRecord> = Vec::new();
        let mut ui_state = UiState {
            session_manager_open: true,
            editor: Some(EditorBuffer::default()),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        // 屏幕给得很宽,窗口本身却只有 default_size 那么大 —— 正是溢出的场景。
        let screen_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 900.0));
        let input = || egui::RawInput {
            screen_rect: Some(screen_rect),
            ..Default::default()
        };
        let _ = ctx.run(input(), |ctx| {
            show(
                ctx,
                &t,
                &mut ui_state,
                &sessions,
                &groups,
                true,
                None,
                SecretPresence::default(),
            );
        });
        let mut rects = None;
        let _ = ctx.run(input(), |ctx| {
            let window_rect = show(
                ctx,
                &t,
                &mut ui_state,
                &sessions,
                &groups,
                true,
                None,
                SecretPresence::default(),
            );
            let editor_rect = ctx.read_response(editor_root_id()).map(|r| r.rect);
            rects = Some((window_rect, editor_rect));
        });
        let (window_rect, editor_rect) = rects.expect("闭包必须跑到底,写回两个矩形");
        let window_rect = window_rect.expect("会话管理器窗口应该已经画出来了");
        let editor_rect = editor_rect.expect("右栏编辑器应该已经画出来了");
        assert!(
            editor_rect.right() <= window_rect.right() + SLACK,
            "右栏溢出窗口:editor.right={} > window.right={}",
            editor_rect.right(),
            window_rect.right()
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app editor_panel_stays_within_window_rect 2>&1 | tail -20`
Expected: 编译失败，`cannot find function editor_root_id in this scope`（下一步一并补上）。

- [ ] **Step 3: 实现——加 `editor_root_id` 探针 + 宽度地板与条件式天花板**

3a. 在 `mod.rs` 里 `group_header` 函数之后追加探针 id（和 `list::new_button_id` 同理由：自动 id 外部算不出来）：

```rust
/// 右栏编辑器根 `Ui` 的显式 id。跟 `list::new_button_id()` 同一个理由:
/// egui 的自动 id 由 `next_auto_id_salt` 计数器派生,外部测试代码算不出来,
/// 只能挂一个不依赖父 id 栈的全局 id,测试侧用 `Context::read_response`
/// 读回真实 `Response::rect` 来判定溢出。全程序只出现一次,不会撞 id。
pub(crate) fn editor_root_id() -> egui::Id {
    egui::Id::new("mullion_sm_editor_root")
}
```

3b. 在 `show()` 闭包内，**紧接在既有高度处理（`if ui.max_rect().height() > avail { ... }`）之后**插入宽度处理：

```rust
            // 地板:让 Window 至少给出容得下「左栏 + 右栏」的宽度。
            // 不能靠 `Window::min_width` —— 它只约束 Resize 的下限,
            // 约束不到 CentralPanel 的绘制。
            let wm = ctx.style().spacing.window_margin;
            ui.set_min_width(WINDOW_W - (wm.left + wm.right));

            // 天花板:横向可用量另算 —— `window_chrome_reserve` 是纵向量
            // (标题栏高 + 上下 margin),横向套用是量纲错误。
            let avail_w = (ctx.available_rect().width() - (wm.left + wm.right) - SLACK).max(0.0);
            // 必须条件式。`Placer::set_max_width` 是无条件覆写 region.max_rect,
            // 无脑设会作废 Resize 当帧从拖拽算出的候选尺寸,resize 手柄就拖不动了。
            if ui.max_rect().width() > avail_w {
                ui.set_max_width(avail_w);
            }
```

3c. 给右栏挂上探针 id——把 `CentralPanel` 那段改成：

```rust
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::none()
                        .fill(theme::c32(t.bar_status))
                        .inner_margin(14.0),
                )
                .show_inside(ui, |ui| {
                    // 挂显式 id 供守护测试读回真实矩形,见 `editor_root_id()`。
                    let rect = ui.max_rect();
                    ui.interact(rect, editor_root_id(), egui::Sense::hover());
                    editor::show(ui, t, ui_state, groups, presence)
                });
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib session_manager 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: `test result: ok.`，既有三条高度守护测试同样通过。

- [ ] **Step 5: 自证变红**

临时注释掉 3b 里的 `ui.set_min_width(WINDOW_W - (wm.left + wm.right));`，
Run: `cargo test -p mullion-app editor_panel_stays_within_window_rect 2>&1 | grep -E "test result|右栏溢出"`
Expected: FAILED，输出含「右栏溢出窗口」。恢复该行后重跑，Expected: `test result: ok.`

- [ ] **Step 6: Commit**

```bash
git add crates/mullion-app/src/ui/session_manager/mod.rs
git commit -m "fix(app): 会话管理器右栏不再溢出窗口被裁切 (F90)

宽度地板用 ui.set_min_width 撑开 Window;天花板必须条件式,否则
Placer::set_max_width 会无条件覆写 region.max_rect,作废 Resize 当帧的
候选尺寸导致拖不动。横向可用量另算,不复用纵向的 window_chrome_reserve。
守护测试:session_manager::tests::editor_panel_stays_within_window_rect"
```

---

## Task 2: 左右栏可拖拽分隔条（F90）

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/mod.rs`

- [ ] **Step 1: 写失败测试**

在 `mod.rs` 的 `mod tests` 里追加：

```rust
    /// F90:分隔条最宽(440)时右栏仍有 ≥400px,且窗口不会被撑宽。
    ///
    /// 常量联立:最窄窗口 880 - 左栏上限 440 - 两侧 margin 24 = 416 ≥ 400。
    /// 自证变红的方式:把 Task 1 里那段条件式天花板整段删掉。
    #[test]
    fn dragging_the_split_does_not_widen_the_window() {
        assert!(
            LIST_MAX_W <= WINDOW_W - 400.0 - 24.0,
            "左栏上限 {LIST_MAX_W} 太大:最窄窗口下右栏会不足 400px"
        );
        assert!(LIST_MIN_W <= LIST_W && LIST_W <= LIST_MAX_W, "默认宽必须落在夹紧区间内");

        let t = crate::theme::MULLION_DARK;
        let sessions: Vec<SessionRecord> = Vec::new();
        let groups: Vec<GroupRecord> = Vec::new();
        let mut ui_state = UiState {
            session_manager_open: true,
            editor: Some(EditorBuffer::default()),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        // 屏幕只比最窄窗口宽一点点 —— 拖到上限也不许把窗口顶出屏幕。
        let screen_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 900.0));
        let input = || egui::RawInput {
            screen_rect: Some(screen_rect),
            ..Default::default()
        };
        for _ in 0..2 {
            let _ = ctx.run(input(), |ctx| {
                show(
                    ctx,
                    &t,
                    &mut ui_state,
                    &sessions,
                    &groups,
                    true,
                    None,
                    SecretPresence::default(),
                );
            });
        }
        let mut window_rect = None;
        let _ = ctx.run(input(), |ctx| {
            window_rect = show(
                ctx,
                &t,
                &mut ui_state,
                &sessions,
                &groups,
                true,
                None,
                SecretPresence::default(),
            );
        });
        let window_rect = window_rect.expect("会话管理器窗口应该已经画出来了");
        assert!(
            window_rect.width() <= screen_rect.width() + SLACK,
            "窗口被撑得比屏幕还宽:{} > {}",
            window_rect.width(),
            screen_rect.width()
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app dragging_the_split 2>&1 | tail -20`
Expected: 编译失败，`cannot find value LIST_MAX_W`（下一步补）。

- [ ] **Step 3: 实现**

3a. 在 `mod.rs` 常量区 `LIST_W` 之后追加：

```rust
/// 左栏拖拽下限。再窄「user@host」副文本就没法读了。
pub(crate) const LIST_MIN_W: f32 = 220.0;
/// 左栏拖拽上限。与 `WINDOW_W` 联立:最窄窗口下右栏仍有
/// `880 - 440 - 24(两侧 inner_margin) = 416px` ≥ 400,表单不会被挤扁。
/// 改这两个常量任意一个都要回头核对 `dragging_the_split_does_not_widen_the_window`。
pub(crate) const LIST_MAX_W: f32 = 440.0;
```

3b. 把 `Window` 的 `.min_width(720.0)` 改成 `.min_width(WINDOW_W)`（Task 1 的宽度地板已按 `WINDOW_W` 撑开内容，min_width 必须与之一致，否则 Resize 下限和内容下限打架）。

3c. 把左栏 `SidePanel` 改成可拖拽：

```rust
            egui::SidePanel::left(ui.id().with("sm_list"))
                .resizable(true)
                .default_width(LIST_W)
                .width_range(LIST_MIN_W..=LIST_MAX_W)
                .frame(
                    egui::Frame::none()
                        .fill(theme::c32(t.panel_bg))
                        .inner_margin(14.0),
                )
                .show_inside(ui, |ui| {
                    list::show(ui, t, ui_state, sessions, groups, connected)
                });
```

（`inner_margin` 由 10 拉到 14，与右栏 `CentralPanel` 的 14 对齐——两栏 padding 不一致是设计文档 §3 点名的视觉毛刺。）

3d. 把既有测试 `new_button_stays_within_screen_rect_when_main_window_is_short` 里注释「超过 `.min_width(720.0)`」改成「超过 `.min_width(WINDOW_W)`(880)」。**只改注释文字，测试逻辑一行不动。**

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib session_manager 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: `test result: ok.`

- [ ] **Step 5: 自证变红**

临时删掉 Task 1 步骤 3b 里 `if ui.max_rect().width() > avail_w { ui.set_max_width(avail_w); }` 整段，
Run: `cargo test -p mullion-app dragging_the_split 2>&1 | grep -E "test result|窗口被撑"`
Expected: FAILED。恢复后重跑，Expected: `test result: ok.`

- [ ] **Step 6: Commit**

```bash
git add crates/mullion-app/src/ui/session_manager/mod.rs
git commit -m "feat(app): 会话管理器左右栏改可拖拽分隔条,220-440 夹紧 (F90)

上限 440 与 WINDOW_W(880) 联立:最窄窗口下右栏仍有 416px。
顺带把两栏 inner_margin 拉平到 14。
守护测试:session_manager::tests::dragging_the_split_does_not_widen_the_window"
```

---

## Task 3: 副文本对比度提到 WCAG AA（F80）

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/list.rs:88`
- Modify: `crates/mullion-app/src/ui/session_manager/fields.rs:162`
- Modify: `crates/mullion-app/src/ui/session_manager/mod.rs`（`secret_edit` 里的「已设置」提示）

- [ ] **Step 1: 改 list.rs 的「user@host」副文本**

把 `list.rs` 里 row 副文本那行的 `theme::c32(t.fg_faint)` 改成 `theme::c32(t.fg_dimmer)`：

```rust
    p.text(
        egui::pos2(rect.left() + 30.0, rect.top() + 25.0),
        egui::Align2::LEFT_TOP,
        format!("{}@{}", rec.auth.user, rec.connection.host),
        egui::FontId::proportional(11.0),
        // WCAG AA:fg_faint(#565b70) on panel_bg(#14161f) 只有 2.69:1,
        // fg_dimmer(#8a90a8) 是 5.71:1。不动 token 本身 —— 它在别处
        // (禁用态、装饰线)是对的。
        theme::c32(t.fg_dimmer),
    );
```

- [ ] **Step 2: 改 fields.rs 的跳板提示**

`fields.rs` 的 `network` 里跳板链提示：

```rust
            ui.colored_label(
                crate::theme::c32(t.fg_dimmer),
                format!("已配置 {} 跳(在分组管理里编辑)", buf.jump_chain.len()),
            );
```

- [ ] **Step 3: 改 mod.rs 的 `secret_edit` 「已设置」提示**

```rust
            ui.colored_label(theme::c32(t.fg_dimmer), "已设置(不修改则保持不变)");
```

- [ ] **Step 4: 修掉 `fg_dimmer` 上过时的「零引用」注释**

`theme.rs` 里 `fg_dimmer` 的 doc 注释当前是「预留给 F84 设置弹窗的快捷键位徽标
（设计文档 §4.4）。零引用。」——本任务刚给它加了三处引用，这句话就成了错的。
这是本次改动**直接导致**的注释失效，必须一起改（不是顺手优化）：

```rust
    /// 会话管理器里的副文本(F80/F90:列表 user@host、跳板提示、「已设置」)。
    /// 选它而不是 `fg_faint`,是因为 `fg_faint` on `panel_bg` 只有 2.69:1,
    /// 达不到 WCAG AA 的 4.5:1;本色是 5.71:1。
    /// 也仍预留给 F84 设置弹窗的快捷键位徽标(设计文档 §4.4)。
    pub fg_dimmer: Rgb,
```

**只改 `fg_dimmer` 这一条。** `fg_ghost` 等其他 token 的「零引用」注释同样已经
过时，但那不是本次改动造成的——不碰（Scope Discipline）。

- [ ] **Step 5: 跑绿**

Run: `cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok.`（本步无新测试；对比度属「你无法验证的东西」里的人眼项，进人工验收清单。）

- [ ] **Step 6: Commit**

```bash
git add crates/mullion-app/src/ui/session_manager/ crates/mullion-app/src/theme.rs
git commit -m "style(app): 会话管理器三处副文本对比度提到 WCAG AA (F80)

fg_faint on panel_bg 实测 2.69:1(AA 要 4.5:1),换 fg_dimmer 得 5.71:1。
不动 theme token 本身 —— 它在禁用态/装饰线上是对的;只更新 fg_dimmer
那句已被本次改动作废的「零引用」注释。"
```

---

## Task 4: `validate.rs` 必填项判定（F91，纯函数）

**Files:**
- Create: `crates/mullion-app/src/ui/session_manager/validate.rs`
- Modify: `crates/mullion-app/src/ui/session_manager/mod.rs`（加 `mod validate;` + re-export）

- [ ] **Step 1: 写完整模块（含失败测试）**

新建 `crates/mullion-app/src/ui/session_manager/validate.rs`：

```rust
//! 会话表单的必填项判定(F91)。**纯函数,零 egui、零 IO**——
//! 这里的分支全是「哪些字段为空 → 该禁哪个按钮 / 跳哪个 Tab」,
//! 放进 UI 就再也测不动了。

/// 缺哪些必填项。端口有默认值 22,不算必填。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Missing {
    pub name: bool,
    pub host: bool,
    pub user: bool,
}

impl Missing {
    pub fn any(self) -> bool {
        self.name || self.host || self.user
    }

    /// 第一个缺项所在的 Tab 索引(与 `UiState::editor_tab` 同义:
    /// 0 连接 / 1 认证 / 2 高级)。
    ///
    /// 用 `usize` 而非新枚举:`editor_tab: usize` 是既有技术债,
    /// 换 enum 会波及所有 Tab 相关代码,不在本切片范围内。
    pub fn tab(self) -> Option<usize> {
        if self.name || self.host {
            Some(0)
        } else if self.user {
            Some(1)
        } else {
            None
        }
    }

    /// 给按钮的 disabled tooltip 用,如「还缺:主机、用户名」。
    pub fn hint(self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if self.name {
            parts.push("会话名称");
        }
        if self.host {
            parts.push("主机");
        }
        if self.user {
            parts.push("用户名");
        }
        format!("还缺:{}", parts.join("、"))
    }
}

/// 判定用 `trim()`:一串空格既连不上也存不住,不能骗过校验。
pub fn check(name: &str, host: &str, user: &str) -> Missing {
    Missing {
        name: name.trim().is_empty(),
        host: host.trim().is_empty(),
        user: user.trim().is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// F91:空白字符不算填了。用户在主机框里敲了个空格就以为填好了,
    /// 存进去连的是空主机名。
    ///
    /// 自证变红的方式:把 `check` 里的 `.trim()` 去掉。
    #[test]
    fn required_fields_reject_whitespace_only() {
        let m = check("  ", "\t", " \n ");
        assert_eq!(
            m,
            Missing {
                name: true,
                host: true,
                user: true
            }
        );
        assert!(m.any());

        let ok = check("web01", "10.0.0.1", "root");
        assert_eq!(ok, Missing::default());
        assert!(!ok.any());
        assert_eq!(ok.tab(), None);
    }

    /// F91:点了禁用的按钮要能被带到第一个缺项所在的 Tab。
    /// 用户名在「认证」Tab 上,不在「连接」Tab —— 跳错就等于没跳。
    ///
    /// 自证变红的方式:把 `tab()` 里 `Some(1)` 改成 `Some(0)`。
    #[test]
    fn missing_maps_to_first_offending_tab() {
        // 只缺用户名 → 认证 Tab(1)
        assert_eq!(check("web01", "10.0.0.1", "").tab(), Some(1));
        // 缺主机(连接 Tab)优先于缺用户名(认证 Tab)
        assert_eq!(check("web01", "", "").tab(), Some(0));
        // 只缺名称 → 连接 Tab(0)
        assert_eq!(check("", "10.0.0.1", "root").tab(), Some(0));

        assert_eq!(check("web01", "", "").hint(), "还缺:主机、用户名");
        assert_eq!(check("", "10.0.0.1", "root").hint(), "还缺:会话名称");
    }
}
```

- [ ] **Step 2: 挂上模块**

`mod.rs` 的模块声明区改成：

```rust
mod buffer;
mod editor;
mod fields;
mod keyscan;
mod list;
mod validate;
```

（`keyscan` 在 Task 14 才建文件；本步先只加 `mod validate;`，`mod keyscan;` 留到 Task 14 再加。）

即本步只加一行：

```rust
mod validate;
```

- [ ] **Step 3: 跑测试**

Run: `cargo test -p mullion-app --lib validate 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok. 2 passed`

- [ ] **Step 4: 自证变红（两次）**

4a. 去掉 `check` 里三处 `.trim()` →
Run: `cargo test -p mullion-app --lib required_fields_reject_whitespace_only 2>&1 | grep -E "test result"`
Expected: FAILED。恢复。

4b. 把 `tab()` 里 `Some(1)` 改成 `Some(0)` →
Run: `cargo test -p mullion-app --lib missing_maps_to_first_offending_tab 2>&1 | grep -E "test result"`
Expected: FAILED。恢复，重跑 Expected: ok。

- [ ] **Step 5: Commit**

```bash
git add crates/mullion-app/src/ui/session_manager/validate.rs crates/mullion-app/src/ui/session_manager/mod.rs
git commit -m "feat(app): 新增 validate 模块判定表单必填项 (F91)

纯函数零 egui:判定用 trim(),防一串空格骗过校验;tab() 给出第一个
缺项所在 Tab,点禁用按钮时据此跳转。
守护测试:validate::tests::required_fields_reject_whitespace_only
        validate::tests::missing_maps_to_first_offending_tab"
```

---

## Task 5: `UiState` 新增本切片全部字段

**Files:**
- Modify: `crates/mullion-app/src/ui/mod.rs`
- Modify: `crates/mullion-app/src/ui/session_manager/mod.rs`（定义 `ProbeState` 并 re-export）

> 一次性把后续任务要用的状态字段加齐，避免每个任务都去动 `UiState` 造成反复冲突。

- [ ] **Step 1: 在 `session_manager/mod.rs` 定义 `ProbeState`**

放在常量区之后：

```rust
/// 「测试连接」(F92)的四态。存在 `UiState` 里跨帧;真正的拨测在 app.rs
/// 的 tokio 运行时上跑,靠 `App::probe_epoch` 世代号丢弃过期结果。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ProbeState {
    #[default]
    Idle,
    Running,
    Ok,
    Err(String),
}
```

`ProbeState` 就定义在本模块里，`ui/mod.rs` 直接写 `crate::ui::session_manager::ProbeState` 引用即可，**不需要额外 re-export**。

- [ ] **Step 2: 在 `UiState` 追加字段**

在 `ui/mod.rs` 的 `UiState` 结构体末尾（`group_intent` 之后）追加：

```rust
    // --- P1-b:测试连接(F92)。与 save_click 同构 —— UI 只写意图,
    // 拨测在 app.rs 的施加点起 tokio 任务。---
    /// 「测试连接」被点了。app.rs 消费后复位,按当前表单起一次拨测。
    pub probe_click: bool,
    /// 拨测的四态,由 app.rs 在收到 `UserEvent::ProbeOk/ProbeErr` 时写。
    pub probe: crate::ui::session_manager::ProbeState,
    /// 请求取消在途拨测(切会话 / 关编辑器 / 关会话管理器)。app.rs 消费后
    /// 自增 `probe_epoch` 并 abort 任务。放意图标志而非直接持有世代号:
    /// 世代号和 JoinHandle 都在 App 上,UiState 够不着。
    pub probe_cancel: bool,

    // --- P1-b:~/.ssh 私钥候选(F93)。---
    /// 扫描 `~/.ssh` 得到的私钥候选路径。打开编辑器 / 切到认证 Tab 时刷一次,
    /// 之后缓存 —— `read_dir` 是同步 IO,不能每帧调(陷阱 T3)。
    pub key_candidates: Vec<std::path::PathBuf>,
    /// 候选是否已扫过。`false` 时 app.rs 会在下一个施加点扫一次。
    pub key_candidates_ready: bool,
    /// 拖拽私钥文件时给用户的一次性提示(如「已忽略其余 2 个文件」)。
    pub key_drop_note: Option<String>,
```

- [ ] **Step 3: 在 `close_session_manager` 里取消拨测**

```rust
    /// 关闭会话管理器。**所有**关闭点都必须走这里,不要直接赋值
    /// `session_manager_open = false`:关闭时要顺带清空只属于它的、可能残留
    /// 的临时状态(目前是 `pending_delete`)——否则下次打开时,待确认删除的
    /// 确认框会带着上次的意图凭空重新出现(复核 F90 Task 10 发现的 bug)。
    ///
    /// F92:在途拨测也必须一并取消 —— 否则窗口关了,20 秒后回来的
    /// ProbeOk 还会把结果卡片写回一个已经不属于任何表单的状态上。
    pub fn close_session_manager(&mut self) {
        self.session_manager_open = false;
        self.pending_delete = None;
        self.probe = crate::ui::session_manager::ProbeState::Idle;
        self.probe_cancel = true;
        self.key_candidates_ready = false;
    }
```

- [ ] **Step 4: 在 `apply_switch` 里同样取消**

`session_manager/mod.rs::apply_switch` 末尾（`ui_state.editor_tab = 0;` 之后）追加：

```rust
    // F92:换了会话,上一条的拨测结果不再有意义。
    ui_state.probe = ProbeState::Idle;
    ui_state.probe_cancel = true;
    ui_state.key_candidates_ready = false;
```

- [ ] **Step 5: 编译**

Run: `cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED|error\["`
Expected: `test result: ok.`（`UiState` 是 `#[derive(Default)]`，新字段全有默认值，不会打断既有构造点。）

- [ ] **Step 6: Commit**

```bash
git add crates/mullion-app/src/ui/
git commit -m "feat(app): UiState 增加拨测/私钥候选状态位 (F92/F93)

ProbeState 定义在 session_manager;probe_cancel 走意图标志而非直接持
世代号 —— probe_epoch 与 JoinHandle 都在 App 上,UiState 够不着。
关闭会话管理器 / 切换会话都要取消在途拨测,否则 20 秒后回来的结果会
写到一个已不存在的表单上。"
```

---

## Task 6: 必填校验接入 UI（F91）

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/editor.rs`
- Modify: `crates/mullion-app/src/ui/session_manager/fields.rs`
- Test: `crates/mullion-app/src/ui/session_manager/mod.rs`

- [ ] **Step 1: 写失败测试**

在 `mod.rs` 的 `mod tests` 追加：

```rust
    /// F91:必填项没填齐时,「保存」/「保存并连接」必须点不动 ——
    /// 否则存进去一条连不上的记录,用户还以为存好了。
    ///
    /// 自证变红的方式:把 editor.rs 里 `ui.add_enabled(!disable_save, ...)`
    /// 的 `!disable_save` 改回 `true`(即拆掉禁用本身),而不是改测试里的
    /// buffer 内容。
    #[test]
    fn save_buttons_are_disabled_when_required_fields_are_empty() {
        let t = crate::theme::MULLION_DARK;
        let sessions: Vec<SessionRecord> = Vec::new();
        let groups: Vec<GroupRecord> = Vec::new();
        // 名称/主机/用户名全空 —— 正是「新建」刚打开时的样子。
        let mut ui_state = UiState {
            session_manager_open: true,
            editor: Some(EditorBuffer::default()),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let screen_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 900.0));
        let input = || egui::RawInput {
            screen_rect: Some(screen_rect),
            ..Default::default()
        };
        let mut enabled = None;
        for _ in 0..2 {
            let _ = ctx.run(input(), |ctx| {
                show(
                    ctx,
                    &t,
                    &mut ui_state,
                    &sessions,
                    &groups,
                    true,
                    None,
                    SecretPresence::default(),
                );
                enabled = ctx.read_response(save_button_id()).map(|r| r.enabled());
            });
        }
        assert_eq!(
            enabled,
            Some(false),
            "必填项全空时「保存」按钮必须是禁用态"
        );

        // 填齐后必须重新可点。
        if let Some(buf) = ui_state.editor.as_mut() {
            buf.name = "web01".into();
            buf.host = "10.0.0.1".into();
            buf.user = "root".into();
        }
        let mut enabled_after = None;
        for _ in 0..2 {
            let _ = ctx.run(input(), |ctx| {
                show(
                    ctx,
                    &t,
                    &mut ui_state,
                    &sessions,
                    &groups,
                    true,
                    None,
                    SecretPresence::default(),
                );
                enabled_after = ctx.read_response(save_button_id()).map(|r| r.enabled());
            });
        }
        assert_eq!(enabled_after, Some(true), "必填项填齐后「保存」必须可点");
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app save_buttons_are_disabled 2>&1 | tail -20`
Expected: 编译失败，`cannot find function save_button_id`。

- [ ] **Step 3: 实现**

3a. `mod.rs` 里 `editor_root_id()` 旁边追加：

```rust
/// 「保存」按钮的显式 id,理由同 `editor_root_id()`:测试要读回它的
/// `Response::enabled()`,自动 id 外部算不出来。
pub(crate) fn save_button_id() -> egui::Id {
    egui::Id::new("mullion_sm_save_button")
}
```

3b. `editor.rs` 顶部加禁用原因判定（放在 `const TABS` 之后）：

```rust
/// 底部按钮为什么点不动。两个原因是并集,`Missing` 优先 ——
/// 表单都没填齐,就没必要提「测试连接进行中」。
enum Disabled {
    No,
    Missing(String),
    Probing,
}

fn why(missing: super::validate::Missing, probe: &super::ProbeState) -> Disabled {
    if missing.any() {
        Disabled::Missing(missing.hint())
    } else if matches!(probe, super::ProbeState::Running) {
        Disabled::Probing
    } else {
        Disabled::No
    }
}

/// 把禁用原因摊成 tooltip 文本。可用时 `None`。
fn tip(d: &Disabled) -> Option<String> {
    match d {
        Disabled::No => None,
        Disabled::Missing(h) => Some(h.clone()),
        Disabled::Probing => Some("测试连接进行中…".to_owned()),
    }
}
```

3c. `editor.rs::show` 里，在取到 `buf` 之后、画 Tab 条之前算出校验结果：

```rust
    let missing = super::validate::check(&buf.name, &buf.host, &buf.user);
    let reason = why(missing, &ui_state.probe);
    // 保存只被「必填未齐」挡;拨测在途时仍可保存(它只读表单,不改)。
    let disable_save = missing.any();
    // 保存并连接 / 测试连接 两个原因都挡。
    let disable_connect = !matches!(reason, Disabled::No);
```

3d. Tab 条加红点：

```rust
    ui.horizontal(|ui| {
        for (i, name) in TABS.iter().enumerate() {
            // F91:缺项所在的 Tab 标一个红点,否则用户看到按钮灰着
            // 却不知道该翻哪一页。
            let label = if missing.tab() == Some(i) {
                format!("{name} ●")
            } else {
                (*name).to_string()
            };
            if ui.selectable_label(ui_state.editor_tab == i, label).clicked() {
                ui_state.editor_tab = i;
            }
        }
    });
    ui.separator();
```

3e. `fields.rs` 给三个必填标签加红星。在 `fields.rs` 顶部加一个小助手：

```rust
/// 必填项标签:名字后跟一个 danger 色的星号。
fn required(ui: &mut Ui, t: &Theme, text: &str) {
    ui.horizontal(|ui| {
        ui.label(text);
        ui.colored_label(crate::theme::c32(t.danger), "*");
    });
}
```

把 `basic` 里的 `ui.label("名称");` → `required(ui, t, "名称");`，`ui.label("主机");` → `required(ui, t, "主机");`；把 `auth` 里的 `ui.label("用户名");` → `required(ui, t, "用户名");`。

- [ ] **Step 4: 让「保存」挂上显式 id**

egui 0.30 的 `Button` 不支持自定义 id，而守护测试必须能
`Context::read_response(id)` 读回 `enabled()`。做法与 `list.rs::new_button_id`
完全同构：自己分配空间、用显式 id `interact`、再手绘。

4a. 在 `mod.rs` 里 `save_button_id()` 之后追加 helper：

```rust
/// 画一个挂着显式 id 的按钮,并在禁用时附 tooltip。
///
/// egui 0.30 的 `Button` 不支持自定义 id,而守护测试必须能
/// `Context::read_response(id)` 读回 `enabled()` —— 只能自己分配空间、
/// 用显式 id `interact`、再手绘(同 `list.rs::new_button_id` 的理由)。
/// 外面套 `add_enabled_ui`:`Response::enabled` 取的正是这一层的
/// `Ui::is_enabled`,不套的话读回来永远是 true。
pub(super) fn labeled_button(
    ui: &mut egui::Ui,
    id: egui::Id,
    text: &str,
    enabled: bool,
    on_disabled: Option<&str>,
) -> bool {
    let mut clicked = false;
    ui.add_enabled_ui(enabled, |ui| {
        let galley = ui.painter().layout_no_wrap(
            text.to_owned(),
            egui::FontId::proportional(14.0),
            ui.visuals().text_color(),
        );
        let size = galley.size() + 2.0 * ui.spacing().button_padding;
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        let resp = ui.interact(rect, id, egui::Sense::click());
        let visuals = *ui.style().interact(&resp);
        ui.painter().rect(
            rect,
            egui::Rounding::same(7.0),
            visuals.bg_fill,
            visuals.bg_stroke,
        );
        ui.painter()
            .galley(rect.center() - 0.5 * galley.size(), galley, visuals.text_color());
        if enabled {
            clicked = resp.clicked();
        } else if let Some(msg) = on_disabled {
            resp.on_disabled_hover_text(msg.to_owned());
        }
    });
    clicked
}
```

4b. 把 `editor.rs` 底部按钮条里的 `let save = ui.button("保存").clicked();` 换成：

```rust
            let save_tip = if disable_save { Some(missing.hint()) } else { None };
            let save = super::labeled_button(
                ui,
                super::save_button_id(),
                "保存",
                !disable_save,
                save_tip.as_deref(),
            );
```

4c. 把同处的 `let save_connect = ui.button("保存并连接").clicked();` 换成：

```rust
            let connect_resp = ui.add_enabled(!disable_connect, egui::Button::new("保存并连接"));
            if let Some(msg) = tip(&reason) {
                connect_resp.clone().on_disabled_hover_text(msg);
            }
            let save_connect = connect_resp.clicked();
```

（按钮条的完整重排在 Task 7 做，本步只保证禁用逻辑先跑通、测试能读到 id。）

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib session_manager 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: `test result: ok.`

- [ ] **Step 6: 自证变红**

把 Step 4b 里 `labeled_button(..., !disable_save, ...)` 的 `!disable_save` 临时改成 `true`（即拆掉禁用本身），
Run: `cargo test -p mullion-app save_buttons_are_disabled 2>&1 | grep -E "test result|必须是禁用态"`
Expected: FAILED。恢复后重跑 Expected: ok。

- [ ] **Step 7: Commit**

```bash
git add crates/mullion-app/src/ui/session_manager/
git commit -m "feat(app): 表单必填校验 —— 红星 + 禁用按钮 + Tab 红点 (F91)

保存只被「必填未齐」挡,拨测在途仍可保存(它只读表单);保存并连接
两个原因都挡。「保存」挂显式 id 手绘,因为 egui 0.30 的 Button 不支持
自定义 id 而守护测试要 read_response 读回 enabled()。
守护测试:session_manager::tests::save_buttons_are_disabled_when_required_fields_are_empty"
```

---

## Task 7: 底部按钮条重排 + 「复制连接串」/「测试连接」归位（F90）

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/editor.rs`

- [ ] **Step 1: 删掉标题条里的「复制连接串」**

`editor.rs` 标题条那段（`ui.with_layout(right_to_left, |ui| { if ui.small_button("复制连接串")... })`）整段删除。

- [ ] **Step 2: 重排底部按钮条**

把 `TopBottomPanel::bottom` 的内容改成：

```rust
    let mut cancel = false;
    egui::TopBottomPanel::bottom(ui.id().with("sm_editor_bottom"))
        .frame(egui::Frame::none())
        .show_separator_line(false)
        .show_inside(ui, |ui| {
            ui.separator();
            // 布局:[测试连接] [复制连接串] ……… [取消] [保存] [保存并连接]
            // 唯一的实心主按钮是最右的「保存并连接」——按钮全一个样,
            // 用户就只能靠读字来找主操作。
            //
            // 不许写成 `let bottom = 44.0; let body_h = available_height() - bottom;`
            // 再喂给 ScrollArea::max_height(c4eb7f1 踩过的坑):按钮条真实高度
            // 随字号/缩放变,手算的估值一旦偏小,滚动区就把按钮压出面板外。
            ui.horizontal(|ui| {
                let probe_tip = tip(&reason);
                if super::labeled_button(
                    ui,
                    super::probe_button_id(),
                    "测试连接",
                    !disable_connect,
                    probe_tip.as_deref(),
                ) {
                    ui_state.probe_click = true;
                }
                let copy_tip = if disable_save { Some(missing.hint()) } else { None };
                if super::labeled_button(
                    ui,
                    super::copy_button_id(),
                    "复制连接串",
                    !disable_save,
                    copy_tip.as_deref(),
                ) {
                    ui.ctx().copy_text(super::connect_string(buf));
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // 主按钮:accent 底 + accent_fg 字。全场唯一一个实心。
                    let primary = egui::Button::new(
                        egui::RichText::new("保存并连接").color(theme::c32(t.accent_fg)),
                    )
                    .fill(theme::c32(t.accent))
                    .stroke(egui::Stroke::NONE)
                    .rounding(7.0);
                    let mut save_connect = false;
                    let resp = ui.add_enabled(!disable_connect, primary);
                    if let Some(msg) = tip(&reason) {
                        resp.clone().on_disabled_hover_text(msg);
                    }
                    save_connect |= resp.clicked();

                    let save_tip = if disable_save { Some(missing.hint()) } else { None };
                    let save = super::labeled_button(
                        ui,
                        super::save_button_id(),
                        "保存",
                        !disable_save,
                        save_tip.as_deref(),
                    );

                    cancel |= ui.button("取消").clicked();

                    if save || save_connect {
                        ui_state.save_click = Some(save_connect);
                    }
                });
            });
        });
```

- [ ] **Step 3: 补两个显式 id**

`mod.rs` 里 `save_button_id()` 旁追加：

```rust
/// 「测试连接」按钮的显式 id,理由同 `save_button_id()`。
pub(crate) fn probe_button_id() -> egui::Id {
    egui::Id::new("mullion_sm_probe_button")
}

/// 「复制连接串」按钮的显式 id,理由同 `save_button_id()`。
pub(crate) fn copy_button_id() -> egui::Id {
    egui::Id::new("mullion_sm_copy_button")
}
```

- [ ] **Step 4: 跑绿**

Run: `cargo test -p mullion-app --lib session_manager 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: `test result: ok.`

Run: `cargo clippy -p mullion-app --all-targets -- -D warnings 2>&1 | tail -5`
Expected: 无输出（只有 `Finished` 行）。

- [ ] **Step 5: Commit**

```bash
git add crates/mullion-app/src/ui/session_manager/
git commit -m "feat(app): 底部按钮条重排,唯一实心主按钮移到最右 (F90)

次要操作(测试连接/复制连接串)靠左,取消/保存/保存并连接靠右,
「保存并连接」用 accent 底做全场唯一实心按钮。
「复制连接串」从标题条移进按钮条。
按钮条高度仍交给 TopBottomPanel 自量,不手算(c4eb7f1 的坑)。"
```

---

## Task 8: 字段分区 + 行距 + 输入框宽度 + 胶囊选中态 + 占位符（F90）

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/fields.rs`
- Modify: `crates/mullion-app/src/ui/session_manager/mod.rs`（`secret_edit`）
- Modify: `crates/mullion-app/src/ui/session_manager/list.rs`（状态点 tooltip）

- [ ] **Step 1: 分区小标题助手**

`fields.rs` 顶部加：

```rust
/// 分区小标题。11px + fg_muted,上面留 10px —— 表单一路平铺下来
/// 没有任何视觉锚点,眼睛找不到「这几行是一组」。
fn section(ui: &mut Ui, t: &Theme, title: &str) {
    ui.add_space(10.0);
    ui.label(
        egui::RichText::new(title)
            .size(11.0)
            .color(crate::theme::c32(t.fg_muted)),
    );
    ui.add_space(4.0);
}
```

- [ ] **Step 2: 行距**

把 `grid` 的 `.spacing([12.0, 8.0])` 改成 `.spacing([12.0, 10.0])`。

- [ ] **Step 3: 按 §6.1 分区**

- `basic`：`section(ui, t, "基本")` → grid(名称/主机/端口/协议) → `section(ui, t, "归类")` → grid(分组/备注)。两个 grid 的 id 分别用 `"sm_basic"` 与 `"sm_basic_group"`（Grid id 必须唯一，否则列宽互相污染）。
- `auth`：`section(ui, t, "身份")` → grid(用户名/认证方式) → `section(ui, t, "凭据")` → grid(密码 或 私钥+口令)，id 用 `"sm_auth"` 与 `"sm_auth_secret"`。
- `network`：`section(ui, t, "代理")` → grid(代理四项) → `section(ui, t, "跳板")` → grid(跳板链)，id 用 `"sm_net_proxy"` 与 `"sm_net_jump"`。

- [ ] **Step 4: 输入框宽度统一**

- 单值长文本（名称/主机/代理主机/代理用户/备注）：`.desired_width(f32::INFINITY)`
- 数值短字段：端口 `80.0`，代理端口 `80.0`（原 70，与端口拉齐）
- `mod.rs::secret_edit` 三处 `.desired_width(200.0)` → `.desired_width(f32::INFINITY)`

- [ ] **Step 5: 认证方式胶囊选中态填充**

`auth` 里两个 `selectable_value` 之前插入：

```rust
        // 选中态没有填充,只有一层几乎看不见的描边 —— 加 accent 弱底,
        // 让「现在是密码还是公钥」一眼可辨。
        let vis = &mut ui.visuals_mut().selection;
        vis.bg_fill = crate::theme::c32(t.accent).linear_multiply(0.35);
```

（放在 `ui.horizontal(|ui| { ... })` 闭包内、两个 `selectable_value` 之前，作用域随闭包结束自动恢复。）

- [ ] **Step 6: 口令占位符**

`mod.rs::secret_edit` 里 `.hint_text("未设置")` → `.hint_text("留空表示无口令")`。

- [ ] **Step 7: 状态点 hover tooltip**

`list.rs` 里画状态点那段之后追加：

```rust
    // §6.3:状态点加 tooltip。它是手绘的,没有 Response,只能补一次
    // interact —— 否则用户只能靠猜「这个绿点是什么意思」。
    let dot_rect = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 16.0, rect.center().y),
        egui::vec2(12.0, 12.0),
    );
    ui.interact(dot_rect, resp.id.with("dot"), egui::Sense::hover())
        .on_hover_text(if connected { "已连接" } else { "未连接" });
```

（`resp` 是行本身的 `Response`，`row` 函数末尾返回它；`ui` 在 `row` 里可用。若 `row` 当前没有 `ui` 参数，用它已有的 `ui` 变量名——本步实现时以文件实际签名为准，不要新增参数。）

- [ ] **Step 8: 跑绿**

Run: `cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: `test result: ok.`

Run: `cargo clippy -p mullion-app --all-targets -- -D warnings 2>&1 | tail -3`
Expected: 无 warning。

- [ ] **Step 9: Commit**

```bash
git add crates/mullion-app/src/ui/session_manager/
git commit -m "style(app): 表单分区留白、输入框宽度统一、胶囊选中态、状态点 tooltip (F90)

三个 Tab 各切成两个分区并加 11px 小标题;行距 8→10;单值长文本一律
撑满、数值字段统一 80;认证方式选中态补 accent 弱底;口令占位符改
「留空表示无口令」;手绘状态点补一次 interact 挂 hover 文案。"
```

---

## Task 9: `SshConnection::disconnect()`（F92 前置）

**Files:**
- Modify: `crates/mullion-ssh/src/session.rs`

- [ ] **Step 1: 实现**

在 `impl SshConnection` 里追加：

```rust
    /// 主动断开:先断目标主机,再逐个断跳板。
    ///
    /// 不能只靠 Drop —— russh 0.54.5 的 `impl Drop for Handle` 只
    /// `debug!("drop handle")`,既不发 disconnect 也不 abort 后台任务。
    /// 拨测(F92)一秒钟能点好几次,漏断就是在对端堆半开连接。
    pub async fn disconnect(&self) {
        let _ = self
            .handle
            .disconnect(russh::Disconnect::ByApplication, "", "")
            .await;
        for h in &self._jumps {
            let _ = h
                .disconnect(russh::Disconnect::ByApplication, "", "")
                .await;
        }
    }
```

- [ ] **Step 2: 编译 + clippy**

Run: `cargo clippy -p mullion-ssh --all-targets -- -D warnings 2>&1 | tail -5`
Expected: 无 warning。若 `russh::Disconnect` 路径不对，按编译器提示的实际路径修（**不要凭记忆改**，russh 版本锁在 0.54.5，`cargo doc -p russh --open` 或直接 grep `~/.cargo/registry/src/**/russh-0.54.5/src/lib.rs` 里的 `pub enum Disconnect`）。

Run: `cargo test -p mullion-ssh 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok.`

- [ ] **Step 3: Commit**

```bash
git add crates/mullion-ssh/src/session.rs
git commit -m "feat(ssh): SshConnection 增加主动 disconnect (F92)

russh 0.54.5 的 impl Drop for Handle 只打日志,不发 disconnect、不
abort 后台任务 —— 拨测一秒能点好几次,漏断就在对端堆半开连接。
先断目标主机再逐个断跳板。"
```

---

## Task 10: 指纹「仅本次信任」不落盘（F3 修订）

**Files:**
- Modify: `crates/mullion-app/src/host_key.rs`
- Modify: `crates/mullion-app/src/app.rs`
- Modify: `crates/mullion-app/src/ui/host_key.rs`
- Test: `crates/mullion-app/src/app.rs`（`mod tests`）

- [ ] **Step 1: 写失败测试**

在 `app.rs` 的 `mod tests` 追加（若无 `mod tests`，在文件末尾新建）：

```rust
    /// F3 修订:「测试连接」触发的 TOFU 确认只信任本次,绝不写 known_hosts。
    /// 拨测是探路,不是承诺 —— 探一次就把陌生指纹钉死,等于把 TOFU 的
    /// 保护降级成「第一次见到谁都认」。
    ///
    /// 自证变红的方式:把 app.rs 施加点的 `if accept && prompt.persist`
    /// 改回 `if accept`(即拆掉门控本身),而不是把构造 prompt 时传的
    /// persist 改成 true。
    #[test]
    fn probe_prompt_does_not_persist_host_key() {
        use crate::host_key::HostKeyPrompt;
        let dir = tempfile::tempdir().expect("建临时目录");
        let path = dir.path().join("known_hosts.toml");
        let mut known = mullion_ssh::known_hosts::KnownHostsFile::load(path.clone())
            .expect("空文件应能加载");

        // 模拟施加点:persist=false 的 prompt 被接受。
        let prompt = HostKeyPrompt {
            host: "example.invalid".to_owned(),
            algo: "ssh-ed25519".to_owned(),
            fingerprint: "SHA256:probe-only".to_owned(),
            previous: None,
            persist: false,
            reply: tokio::sync::oneshot::channel().0,
        };
        let accept = true;
        if accept && prompt.persist {
            known.record(
                &prompt.host,
                mullion_ssh::known_hosts::HostKeyEntry {
                    algo: prompt.algo.clone(),
                    fingerprint: prompt.fingerprint.clone(),
                },
            );
            known.save().expect("落盘");
        }
        assert!(
            known.get("example.invalid").is_none(),
            "拨测接受的指纹不许进 known_hosts"
        );
        assert!(!path.exists(), "拨测不该产生 known_hosts 文件");
    }
```

> **注意**：这条测试复刻施加点的条件表达式，因为 `app.rs` 的施加点在 `render_frame` 深处、无法在无窗口环境下调用。自证时必须**同时**改施加点和测试里的 `if accept && prompt.persist`——若只改测试而不改施加点，说明门控没真正落到代码里；实现者必须在 Step 5 亲自确认施加点那行文本与测试一致。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app probe_prompt_does_not_persist 2>&1 | tail -20`
Expected: 编译失败，`struct HostKeyPrompt has no field named persist`。

- [ ] **Step 3: 实现**

3a. `crates/mullion-app/src/host_key.rs`：

```rust
pub struct HostKeyPrompt {
    pub host: String,
    /// 形如 `ssh-ed25519`,供用户核对时对上 `ssh-keygen -lf` 的输出。
    pub algo: String,
    /// `SHA256:<base64>`。
    pub fingerprint: String,
    /// 存档里的旧记录;`Some` = 指纹变更(高危,UI 走警告态)。
    pub previous: Option<HostKeyEntry>,
    /// 用户接受后是否写 known_hosts。正式连接 `true`;
    /// 「测试连接」(F92)一律 `false` —— 探路不是承诺。
    pub persist: bool,
    pub reply: oneshot::Sender<bool>,
}
```

```rust
pub struct PromptingPolicy {
    known: Arc<Mutex<KnownHostsFile>>,
    proxy: Mutex<EventLoopProxy<UserEvent>>,
    /// 透传给 `HostKeyPrompt::persist`,决定用户接受后是否落盘。
    persist: bool,
}

impl PromptingPolicy {
    pub fn new(
        known: Arc<Mutex<KnownHostsFile>>,
        proxy: EventLoopProxy<UserEvent>,
        persist: bool,
    ) -> Self {
        Self {
            known,
            proxy: Mutex::new(proxy),
            persist,
        }
    }
}
```

在 `decide` 里构造 `HostKeyPrompt` 的地方补上 `persist: self.persist,`。

3b. `app.rs::spawn_connect` 里的构造改成 `PromptingPolicy::new(self.known_hosts.clone(), self.proxy.clone(), true)`。全局 grep 确认没有第三处构造点：

```bash
grep -rn "PromptingPolicy::new" crates/
```

3c. `app.rs` 意图施加点：`if accept {` → `if accept && prompt.persist {`。

3d. `crates/mullion-app/src/ui/host_key.rs`：`HostKeyView` 加字段并在未知态文案后追加一行。

```rust
pub struct HostKeyView<'a> {
    pub host: &'a str,
    pub algo: &'a str,
    pub fingerprint: &'a str,
    /// 存档里的旧指纹;`Some` = 变更(高危)。
    pub previous: Option<&'a str>,
    /// 弹窗已开的秒数。
    pub elapsed_secs: u64,
    /// `false` = 这次接受只对本次连接有效,不写 known_hosts(F92 拨测)。
    pub persist: bool,
}
```

在弹窗正文里，接受按钮上方追加：

```rust
        if !view.persist {
            ui.add_space(6.0);
            ui.colored_label(
                crate::theme::c32(t.fg_dim),
                "本次测试不会记住此指纹,正式连接时会再次询问。",
            );
        }
```

既有 6 条测试的 `HostKeyView` 构造若用了字段全列，补 `persist: true,`；若用了 `..changed` 展开语法则无需改。**测试断言一行不动。**

3e. `app.rs` 的 `HostKeyView` 构造点补 `persist: p.persist,`。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: `test result: ok.`，`host_key` 既有 6 条全过。

- [ ] **Step 5: 自证变红**

把 `app.rs` 施加点的 `if accept && prompt.persist {` 改回 `if accept {`，**同时**把测试里同一行改回 `if accept {`（测试复刻的是施加点逻辑），
Run: `cargo test -p mullion-app probe_prompt_does_not_persist 2>&1 | grep -E "test result|不许进"`
Expected: FAILED。两处都恢复后重跑 Expected: ok。

- [ ] **Step 6: Commit**

```bash
git add crates/mullion-app/src/
git commit -m "feat(app): 主机指纹确认支持「仅本次信任」不落盘 (F3/F92)

PromptingPolicy 加 persist 参数透传到 HostKeyPrompt,施加点改成
accept && prompt.persist。拨测是探路不是承诺,探一次就把陌生指纹钉死
等于把 TOFU 降级成「第一次见到谁都认」。弹窗补一行说明。
守护测试:app::tests::probe_prompt_does_not_persist_host_key"
```

---

## Task 11: `mullion-store` 抽出参数化解析内核（F92 前置）

**Files:**
- Modify: `crates/mullion-store/src/jump.rs`
- Modify: `crates/mullion-store/src/inherit.rs`
- Modify: `crates/mullion-store/src/vault.rs`

> 「测试连接」拨的是**当前表单（含未保存改动）**，此时会话可能还没有 `SessionId`。
> 现有解析全部以 id 为入口。这一任务把两处解析各抽一个参数化内核，
> **已保存路径改调同一内核**——是重构，不是新增第二条解析路径。
> 依赖方向不变：`mullion-store` 仍零 UI、零 async。

- [ ] **Step 1: 写失败测试（jump.rs）**

在 `jump.rs` 的 `mod tests` 追加：

```rust
    /// F92:从一条**给定的**跳板链展开,发起方不必存在于索引里 ——
    /// 「测试连接」拨的是还没保存、还没有 id 的草稿。
    ///
    /// 自证变红的方式:把 `expand_chain_of` 改成忽略 `chain` 参数、
    /// 直接返回空 vec。
    #[test]
    fn chain_of_expands_without_an_existing_origin_session() {
        // 草稿要经 2,而 2 自己要经 3 —— 拨号顺序必须是 3 → 2。
        let idx = index(vec![rec(2, vec![3]), rec(3, vec![])]);
        let chain = vec![JumpRef(SessionId(2))];
        let got = expand_chain_of(&chain, &idx, &no_groups()).unwrap();
        assert_eq!(got, vec![SessionId(3), SessionId(2)]);
    }

    /// F92:草稿路径的安全属性一个都不能少 —— 悬空跳板照样硬失败,
    /// 绝不静默降级成直连(用户会以为流量过了堡垒机)。
    ///
    /// 自证变红的方式:把 `visit` 里的 `.ok_or(StoreError::JumpDangling(id))?`
    /// 改成 `else { return Ok(()) }`。
    #[test]
    fn chain_of_still_rejects_dangling_and_cyclic_hops() {
        let idx = index(vec![rec(2, vec![])]);
        let err = expand_chain_of(&[JumpRef(SessionId(42))], &idx, &no_groups()).unwrap_err();
        assert!(
            matches!(err, StoreError::JumpDangling(SessionId(42))),
            "悬空引用必须报错,实际: {err:?}"
        );

        let cyc = index(vec![rec(1, vec![2]), rec(2, vec![1])]);
        let err = expand_chain_of(&[JumpRef(SessionId(1))], &cyc, &no_groups()).unwrap_err();
        assert!(matches!(err, StoreError::JumpCycle(_)), "环必须报错,实际: {err:?}");
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-store chain_of 2>&1 | tail -15`
Expected: 编译失败，`cannot find function expand_chain_of`。

- [ ] **Step 3: 实现 jump.rs 内核**

把 `expand_chain` 换成下面三个函数（`visit` / `layers_for` 一行不动）：

```rust
/// 展开 `target` 的完整跳板链,返回按拨号顺序排列的会话 id。
///
/// `sessions` / `groups` 是全量索引:展开过程要读每个跳板会话**自身**的
/// 跳板设置(含它从分组继承来的),所以不能只传目标那一条。
pub fn expand_chain(
    target: SessionId,
    sessions: &BTreeMap<SessionId, SessionRecord>,
    groups: &BTreeMap<crate::model::GroupId, crate::group::GroupRecord>,
) -> Result<Vec<SessionId>, StoreError> {
    let rec = sessions
        .get(&target)
        .ok_or(StoreError::JumpDangling(target))?;
    let chain = resolve(&layers_for(rec, groups)).jump;
    expand_from(Some(target), &chain, sessions, groups)
}

/// 从一条**已给定**的跳板链展开(F92)。发起方是尚未保存的草稿:它还没有
/// id,不可能被任何已存记录引用,因此不参与环检测。
///
/// 除入口外,与 `expand_chain` 共用同一个内核 —— 草稿路径和已保存路径
/// 的展开语义(递归、去重、环/悬空/超深判定)必须完全一致,否则「测试通过
/// 但保存后连不上」。
pub fn expand_chain_of(
    chain: &[crate::network::JumpRef],
    sessions: &BTreeMap<SessionId, SessionRecord>,
    groups: &BTreeMap<crate::model::GroupId, crate::group::GroupRecord>,
) -> Result<Vec<SessionId>, StoreError> {
    expand_from(None, chain, sessions, groups)
}

/// 两个入口共用的内核。`origin` 只用于环检测入栈与错误定位;
/// `None` = 发起方尚未入库。
fn expand_from(
    origin: Option<SessionId>,
    chain: &[crate::network::JumpRef],
    sessions: &BTreeMap<SessionId, SessionRecord>,
    groups: &BTreeMap<crate::model::GroupId, crate::group::GroupRecord>,
) -> Result<Vec<SessionId>, StoreError> {
    let mut out = Vec::new();
    let mut on_stack: Vec<SessionId> = origin.into_iter().collect();
    for hop in chain {
        visit(hop.0, sessions, groups, &mut out, &mut on_stack)?;
        if !out.contains(&hop.0) {
            out.push(hop.0);
        }
    }
    if out.len() > MAX_JUMP_DEPTH {
        // out 非空(len > MAX),`last()` 必有值。
        return Err(StoreError::JumpTooDeep(origin.unwrap_or_else(|| {
            *out.last().expect("out.len() > MAX_JUMP_DEPTH 时必非空")
        })));
    }
    Ok(out)
}
```

- [ ] **Step 4: 跑 jump.rs 全部测试**

Run: `cargo test -p mullion-store jump 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: `test result: ok.`——既有 13 条（含环/悬空/深度边界/菱形/继承）**一条不改也全过**，这是「重构没改语义」的证据。

- [ ] **Step 5: 自证变红**

把 `expand_chain_of` 的函数体临时改成 `Ok(Vec::new())`，
Run: `cargo test -p mullion-store chain_of_expands_without 2>&1 | grep -E "test result"`
Expected: FAILED。恢复后重跑 Expected: ok。

- [ ] **Step 6: `SessionDraft` 实现 `PrefsLayer`**

在 `inherit.rs` 里 `impl PrefsLayer for crate::group::GroupRecord` 之后追加：

```rust
/// 草稿也是一层可继承偏好 —— F92「测试连接」要按尚未保存的表单解析
/// 代理与跳板,而解析入口只认 `PrefsLayer`。
impl PrefsLayer for crate::vault::SessionDraft {
    fn tags(&self) -> &[String] {
        &self.identity.tags
    }
    fn terminal(&self) -> &TerminalPrefs {
        &self.terminal
    }
    fn appearance(&self) -> &AppearancePrefs {
        &self.appearance
    }
    fn network(&self) -> &NetworkPrefs {
        &self.network
    }
}
```

- [ ] **Step 7: Vault 抽内核**

把 `vault.rs` 的两个方法改成：

```rust
    /// 沿 `[会话, 分组]` 层序解析出最终配置。
    ///
    /// 结果应由调用方缓存,**不要在渲染热路径 / 每帧里重新调用**(本项目陷阱 T3:
    /// 喂数据和重绘没解耦 → 每秒几千次重绘,GPU 空转、风扇起飞)。
    ///
    /// 若会话的 `group_id` 指向一个已不存在的分组(悬空引用,例如分组被删除
    /// 后会话记录未同步、或数据被手改),本函数**不报错、不 panic**,而是静默
    /// 按「无分组」处理,回落到内置默认值——分组数据的问题不该拖垮会话本身。
    /// 本 crate 不接 `log`,排查这类问题目前唯一的线索就是这段文档。
    pub fn resolve_for(&self, id: SessionId) -> Result<crate::inherit::ResolvedConfig, StoreError> {
        let s = self.get(id).ok_or(StoreError::NotFound(id))?;
        Ok(self.resolve_layer(s, s.identity.group_id))
    }

    /// `resolve_for` 的参数化内核:直接吃一层 prefs + 它所属的分组 id,
    /// 不要求该层已经入库 —— F92「测试连接」解析的是尚未保存的草稿。
    /// 悬空 `group_id` 的静默降级语义与 `resolve_for` 完全一致(见上)。
    pub fn resolve_layer(
        &self,
        layer: &dyn crate::inherit::PrefsLayer,
        group_id: Option<crate::model::GroupId>,
    ) -> crate::inherit::ResolvedConfig {
        match group_id.and_then(|gid| self.groups.iter().find(|g| g.id == gid)) {
            Some(g) => crate::inherit::resolve(&[layer, g]),
            None => crate::inherit::resolve(&[layer]),
        }
    }

    /// 展开一条会话的完整跳板链(F5)。返回按拨号顺序排列的**跳板会话记录**。
    ///
    /// 返回记录而非 id:调用方(app)接下来要拿每一跳的 host/user/认证去物化 `Hop`,
    /// 让它再查一遍索引没有意义。
    pub fn expand_jump_chain(&self, id: SessionId) -> Result<Vec<SessionRecord>, StoreError> {
        let (sessions, groups) = self.jump_index();
        if !sessions.contains_key(&id) {
            return Err(StoreError::NotFound(id));
        }
        let ids = crate::jump::expand_chain(id, &sessions, &groups)?;
        Ok(ids.into_iter().map(|i| sessions[&i].clone()).collect())
    }

    /// `expand_jump_chain` 的参数化内核:直接吃一条跳板链(通常来自
    /// `resolve_layer(..).jump`),发起方不必已入库 —— F92 拨的是草稿。
    pub fn expand_jump_chain_of(
        &self,
        chain: &[crate::network::JumpRef],
    ) -> Result<Vec<SessionRecord>, StoreError> {
        let (sessions, groups) = self.jump_index();
        let ids = crate::jump::expand_chain_of(chain, &sessions, &groups)?;
        Ok(ids.into_iter().map(|i| sessions[&i].clone()).collect())
    }

    /// 建两张全量索引。展开跳板要读每个跳板会话自身(含继承)的链,
    /// 只传目标那一条不够。
    fn jump_index(
        &self,
    ) -> (
        std::collections::BTreeMap<SessionId, SessionRecord>,
        std::collections::BTreeMap<crate::model::GroupId, crate::group::GroupRecord>,
    ) {
        (
            self.list().iter().map(|r| (r.id, r.clone())).collect(),
            self.groups().iter().map(|g| (g.id, g.clone())).collect(),
        )
    }
```

- [ ] **Step 8: 跑绿**

Run: `cargo test -p mullion-store 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: `test result: ok.`（既有 vault/inherit 测试一条不改也全过）

Run: `cargo clippy -p mullion-store --all-targets -- -D warnings 2>&1 | tail -3`
Expected: 无 warning。

- [ ] **Step 9: 同步设计文档 §15**

设计文档 `docs/superpowers/specs/2026-08-04-slice-p1ab-session-ui-polish-design.md`
的 §15 依赖方向表里 `mullion-store` 一行写的是「无改动」。那张表是在用户批准
「测试连接拨的是当前表单（含未保存改动）」**之前**定稿的，本任务让它失效了。
文档与代码打架比没有文档更糟，就地改掉这一行：

```markdown
| `mullion-store` | **有**：`resolve_for` / `expand_jump_chain` 各抽一个参数化内核（`resolve_layer` / `expand_jump_chain_of`），已保存路径改调同一内核；`SessionDraft` 实现 `PrefsLayer`。**只加不改语义**，13 条 `jump.rs` 既有测试一行未动仍全绿 | 否，仍是被 app 单向依赖；本 crate 仍零 UI、零 async |
```

**只改这一行。** §15 其余行、§9.1 的 `mullion-ssh` 那行都不动。

- [ ] **Step 10: Commit**

```bash
git add crates/mullion-store/src/ docs/superpowers/specs/2026-08-04-slice-p1ab-session-ui-polish-design.md
git commit -m "refactor(store): 解析代理与跳板抽出参数化内核 (F92)

expand_chain/resolve_for 的入口从「库里的 id」拆出「层 + 链」内核,
两条入口共用同一实现 —— 草稿路径与已保存路径的语义(递归、去重、
环/悬空/超深判定、悬空分组静默降级)必须完全一致,否则会出现
「测试通过但保存后连不上」。SessionDraft 实现 PrefsLayer。
既有 jump/vault 测试一条未改全绿,即为语义未漂移的证据。
同步设计文档 §15 依赖方向表(原写「mullion-store 无改动」,已失效)。
守护测试:jump::tests::chain_of_expands_without_an_existing_origin_session
        jump::tests::chain_of_still_rejects_dangling_and_cyclic_hops"
```

---

## Task 12: `SessionStore::ssh_config_for_draft`（F92）

**Files:**
- Modify: `crates/mullion-app/src/shell/store.rs`

- [ ] **Step 1: 实现**

在 `impl SessionStore` 里 `ssh_config_for` 之后追加：

```rust
    /// 按**草稿**(含未保存改动)组 SshConfig。「测试连接」(F92)用。
    ///
    /// 与 `ssh_config_for` 走同一套解析内核(`resolve_layer` /
    /// `expand_jump_chain_of`),只是入口从「库里的 id」换成「手上的草稿」。
    /// 不是第二条解析路径 —— 否则「测试通过、保存后连不上」这种最伤
    /// 信任的 bug 迟早出现。
    ///
    /// 跳板悬空/成环同样**硬失败**:拨测的价值就在于提前把这些问题炸出来。
    pub fn ssh_config_for_draft(&self, draft: &SessionDraft) -> Result<SshConfig, StoreOpenError> {
        let rec = draft_to_record(draft);
        let secret = draft.secret.as_ref();
        let mut cfg = to_ssh_config(&rec, secret)?;

        let resolved = self.vault.resolve_layer(draft, draft.identity.group_id);
        let jumps = self.vault.expand_jump_chain_of(&resolved.jump)?;
        cfg.hops = super::dial_plan::build_hops_with_proxy_secret(
            resolved.proxy.as_ref(),
            &jumps,
            &|jid| self.vault.secret(jid).cloned(),
            secret,
        );
        Ok(cfg)
    }
```

并在文件末尾（`impl SessionStore` 之外）加：

```rust
/// 草稿 → 临时 `SessionRecord`,只为喂给 `to_ssh_config`。
/// 它只读 `connection` / `auth`,`id` 与 `modified_at` 是占位,
/// 不入库、不外泄 —— 草稿本来就还没有 id。
fn draft_to_record(d: &SessionDraft) -> SessionRecord {
    SessionRecord {
        id: SessionId(0),
        modified_at: String::new(),
        identity: d.identity.clone(),
        connection: d.connection.clone(),
        auth: d.auth.clone(),
        terminal: d.terminal.clone(),
        appearance: d.appearance.clone(),
        network: d.network.clone(),
    }
}
```

- [ ] **Step 2: 跑绿**

Run: `cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED|error\["`
Expected: `test result: ok.`

Run: `cargo clippy -p mullion-app --all-targets -- -D warnings 2>&1 | tail -3`
Expected: 无 warning。

- [ ] **Step 3: Commit**

```bash
git add crates/mullion-app/src/shell/store.rs
git commit -m "feat(app): SessionStore 支持按草稿组 SshConfig (F92)

复用 Task 11 抽出的 resolve_layer / expand_jump_chain_of,与已保存
路径同一内核。跳板悬空/成环照样硬失败 —— 拨测的价值正在于提前炸出
这些问题。"
```

---

## Task 13: 拨测任务与世代号（F92 核心）

**Files:**
- Modify: `crates/mullion-app/Cargo.toml`
- Modify: `crates/mullion-app/src/app.rs`
- Test: `crates/mullion-app/src/app.rs`（`mod tests`）

- [ ] **Step 1: 写失败测试**

在 `app.rs` 的 `mod tests` 追加：

```rust
    /// F92:世代号变了就必须丢弃迟到的拨测结果。
    ///
    /// 场景:对 A 点了「测试连接」,20 秒超时未到就切到了 B。若不校验世代,
    /// A 的结果会写到 B 的表单上 —— 用户看到「连接成功」,其实测的是别人。
    ///
    /// 自证变红的方式:把 `accept_probe` 里 `epoch == current` 的守卫去掉,
    /// 而不是改测试里传的 epoch 值。
    #[test]
    fn stale_probe_result_is_discarded_after_epoch_bump() {
        use crate::ui::session_manager::ProbeState;

        let mut state = ProbeState::Running;
        // 当前世代 7,回来的是 6 —— 期间切过一次会话。
        assert!(!crate::app::accept_probe(6, 7, &mut state, Ok(())));
        assert_eq!(state, ProbeState::Running, "过期结果不许改动状态");

        // 同世代的结果照常采纳。
        assert!(crate::app::accept_probe(7, 7, &mut state, Ok(())));
        assert_eq!(state, ProbeState::Ok);

        let mut state = ProbeState::Running;
        assert!(crate::app::accept_probe(3, 3, &mut state, Err("超时".to_owned())));
        assert_eq!(state, ProbeState::Err("超时".to_owned()));
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app stale_probe_result 2>&1 | tail -15`
Expected: 编译失败，`cannot find function accept_probe`。

- [ ] **Step 3: tokio 补 `"time"` feature**

`crates/mullion-app/Cargo.toml` 把 `tokio.workspace = true` 改成：

```toml
# 拨测(F92)要 tokio::time::timeout。现在不写也能编过,是因为 russh 0.54.5
# 打开了 tokio 的 time —— feature 是并集,靠别人的依赖泄漏过来是运气,
# 哪天 russh 收窄就整片编不过。显式声明。
tokio = { workspace = true, features = ["time"] }
```

Run: `cargo build -p mullion-app 2>&1 | tail -3`
Expected: 编译通过。

- [ ] **Step 4: 实现纯函数 + 事件 + 任务**

4a. `app.rs` 顶层（`impl App` 之外）加纯函数：

```rust
/// 采纳一次拨测结果:世代号对得上才写状态。返回是否采纳。
///
/// 抽成自由函数是为了能脱离窗口/运行时单测 —— 事件循环里那一大坨
/// 是本项目最难测的地方,判定逻辑绝不能埋在里面。
pub(crate) fn accept_probe(
    epoch: u64,
    current: u64,
    state: &mut crate::ui::session_manager::ProbeState,
    outcome: Result<(), String>,
) -> bool {
    use crate::ui::session_manager::ProbeState;
    if epoch != current {
        return false;
    }
    *state = match outcome {
        Ok(()) => ProbeState::Ok,
        Err(msg) => ProbeState::Err(msg),
    };
    true
}
```

4b. `UserEvent` 追加两个变体：

```rust
    /// F92:一次拨测成功。`u64` 是发起时的世代号,过期的直接丢。
    ProbeOk(u64),
    /// F92:一次拨测失败(含超时)。
    ProbeErr(u64, String),
```

4c. `App` 追加两个字段（并在 `App::new` 的 `Self { .. }` 里补 `probe_epoch: 0,` 与 `probe_task: None,`）：

```rust
    /// F92 拨测世代号。切会话 / 关编辑器 / 关会话管理器时 +1,
    /// 迟到的结果据此丢弃(见 `accept_probe`)。
    probe_epoch: u64,
    /// 在途拨测任务。退出或取消时 abort —— 20 秒的 timeout 悬着不管,
    /// 关窗后进程还要多活 20 秒。
    probe_task: Option<tokio::task::JoinHandle<()>>,
```

4d. `impl App` 里加 `spawn_probe`（照 `spawn_connect` 的形状）：

```rust
    /// F92:拨一次完整认证后立刻断开。**不开 channel、不起 pty** ——
    /// 拨测只回答「这条链路加上这份凭据能不能登上去」。
    fn spawn_probe(&mut self, cfg: SshConfig) {
        /// 拨测超时。比正常连接短:用户是站在弹窗前等结果的,
        /// 超过这个数就该告诉他「不通」,而不是继续转圈。
        const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

        self.probe_epoch = self.probe_epoch.wrapping_add(1);
        let epoch = self.probe_epoch;
        let proxy = self.proxy.clone();
        // persist=false:拨测遇到未知指纹只信任本次,绝不写 known_hosts。
        let policy: Arc<dyn HostKeyPolicy> = Arc::new(crate::host_key::PromptingPolicy::new(
            self.known_hosts.clone(),
            self.proxy.clone(),
            false,
        ));
        let h = self._runtime.spawn(async move {
            let ev = match tokio::time::timeout(
                PROBE_TIMEOUT,
                mullion_ssh::session::establish(&cfg, policy),
            )
            .await
            {
                Err(_) => UserEvent::ProbeErr(epoch, "超时(20s):链路不通或对端无响应".to_owned()),
                Ok(Err(e)) => UserEvent::ProbeErr(epoch, e.to_string()),
                Ok(Ok(c)) => {
                    // russh 的 Drop 不发 disconnect,必须显式断(见 SshConnection::disconnect)。
                    c.disconnect().await;
                    UserEvent::ProbeOk(epoch)
                }
            };
            let _ = proxy.send_event(ev);
        });
        // 覆盖前先 abort 上一个:用户连点两次「测试连接」不该留下孤儿任务。
        if let Some(old) = self.probe_task.replace(h) {
            old.abort();
        }
    }

    /// 按当前表单(含未保存改动)组拨测用的 SshConfig。
    ///
    /// 凭据三态合成必须带上库里的旧值 —— 编辑已有会话时用户多半没重输
    /// 密码,`build_draft` 自己看不到 store,合成出来是 None,拨测会误报
    /// 「缺少凭据」。这一步和 `apply_save` 做的是同一件事。
    fn build_probe_config(&self) -> Result<SshConfig, String> {
        let buf = self.ui.editor.as_ref().ok_or("没有正在编辑的会话")?;
        let mut draft = crate::ui::session_manager::build_draft(buf)?;
        let existing = self
            .ui
            .editor_id
            .and_then(|id| self.store.as_ref().and_then(|s| s.secret(id)));
        let (pw, pp, proxy) = crate::ui::session_manager::secret_fields(buf);
        draft.secret = crate::ui::session_manager::merge_secret(existing, &pw, &pp, &proxy);
        // 复用既有的 `sync_has_passphrase`(`apply_save` 用的是同一个),
        // 不要在这里手写第二份 `AuthKind::PublicKey { has_passphrase }` 同步逻辑 ——
        // 两份迟早漂移,而漂移的后果是「测试通过、保存后要不到口令」。
        crate::ui::session_manager::sync_has_passphrase(&mut draft, draft.secret.clone().as_ref());
        let store = self.store.as_ref().ok_or("会话库不可用")?;
        store
            .ssh_config_for_draft(&draft)
            .map_err(|e| e.to_string())
    }
```

4e. `user_event` 加两个分支：

```rust
            UserEvent::ProbeOk(epoch) => {
                crate::app::accept_probe(epoch, self.probe_epoch, &mut self.ui.probe, Ok(()));
                self.request_ui_redraw();
            }
            UserEvent::ProbeErr(epoch, msg) => {
                crate::app::accept_probe(epoch, self.probe_epoch, &mut self.ui.probe, Err(msg));
                self.request_ui_redraw();
            }
```

4f. 意图施加点（放在「连接」那段之后）：

```rust
        // F92:「测试连接」。取消优先于发起 —— 同一帧里既取消又点测试的
        // 唯一可能是切换会话时手抖,那也该以新表单为准。
        if std::mem::take(&mut self.ui.probe_cancel) {
            self.probe_epoch = self.probe_epoch.wrapping_add(1);
            if let Some(h) = self.probe_task.take() {
                h.abort();
            }
        }
        if std::mem::take(&mut self.ui.probe_click) {
            match self.build_probe_config() {
                Ok(cfg) => {
                    self.ui.probe = crate::ui::session_manager::ProbeState::Running;
                    self.spawn_probe(cfg);
                }
                Err(msg) => {
                    self.ui.probe = crate::ui::session_manager::ProbeState::Err(msg);
                }
            }
        }
```

4g. 退出兜底——在处理 `request_quit` / `WindowEvent::CloseRequested` 的地方（与既有断连清理同处）追加：

```rust
        // F92:进程要走了,20 秒的 timeout 别悬着。
        if let Some(h) = self.probe_task.take() {
            h.abort();
        }
```

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: `test result: ok.`

- [ ] **Step 6: 自证变红**

把 `accept_probe` 里 `if epoch != current { return false; }` 整段删掉，
Run: `cargo test -p mullion-app stale_probe_result 2>&1 | grep -E "test result|不许改动状态"`
Expected: FAILED。恢复后重跑 Expected: ok。

- [ ] **Step 7: Commit**

```bash
git add crates/mullion-app/Cargo.toml crates/mullion-app/src/app.rs
git commit -m "feat(app): 测试连接 —— 完整认证后立即断开,世代号丢弃过期结果 (F92)

拨测只回答「这条链路加这份凭据能不能登上去」:不开 channel、不起 pty,
establish 成功后立刻 disconnect。20s 超时;切会话/关窗自增世代号并 abort。
判定逻辑抽成自由函数 accept_probe,脱离窗口与运行时可单测。
tokio 显式声明 time feature —— 原先靠 russh 的 feature 并集泄漏。
守护测试:app::tests::stale_probe_result_is_discarded_after_epoch_bump"
```

---

## Task 14: 拨测结果卡片与按钮态（F92 UI）

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/editor.rs`

- [ ] **Step 1: 结果卡片（与错误卡片互斥）**

在 `editor.rs` 里既有 `last_error` 卡片那段**之后**追加：

```rust
    // F92:拨测结果卡片。与 last_error 卡片互斥 —— 两张同款卡片叠在一起
    // 用户分不清哪条是哪条。`last_error` 优先(它多半是刚才保存失败,
    // 更紧急),拨测结果让位但**不清空**,错误关掉后它还在。
    let error_shown = ui_state.last_error.is_some() && !ui_state.error_dismissed;
    if !error_shown {
        let card = match &ui_state.probe {
            super::ProbeState::Idle => None,
            super::ProbeState::Running => Some((t.info, "正在测试连接…".to_owned())),
            super::ProbeState::Ok => Some((t.ok, "连接成功(已立即断开,未记住指纹)".to_owned())),
            super::ProbeState::Err(msg) => Some((t.danger_soft, format!("连接失败:{msg}"))),
        };
        if let Some((color, text)) = card {
            egui::Frame::none()
                .fill(theme::c32(t.sunken_bg))
                .stroke(egui::Stroke::new(1.0, theme::c32(color)))
                .rounding(8.0)
                .inner_margin(10.0)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.colored_label(theme::c32(color), text);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("×").clicked() {
                                ui_state.probe = super::ProbeState::Idle;
                            }
                        });
                    });
                });
            ui.add_space(8.0);
        }
    }
```

- [ ] **Step 2: 改字段就清掉旧结论**

在 `editor.rs::show` 里、画完三个 Tab 的 `ScrollArea` **之后**追加：

```rust
    // F92:表单一改,上一次的成功/失败结论就不再可信 —— 但**不动世代号**:
    // 在途那次仍是针对这份表单发起的,让它跑完;世代号只在切会话/关窗时才变。
    //
    // 用既有的 `is_dirty`(buffer.rs:124),不要另写一份 `!=` 比较:它把三个
    // `*_touched` 位也算进了「脏」,手写的整体比较会漏掉「点进密码框再清空」
    // 这类文本上看不出、意图上却是「清除凭据」的改动。
    let changed = match (ui_state.editor.as_ref(), ui_state.editor_baseline.as_ref()) {
        (Some(buf), Some(base)) => super::is_dirty(buf, base),
        _ => false,
    };
    if changed
        && matches!(
            ui_state.probe,
            super::ProbeState::Ok | super::ProbeState::Err(_)
        )
    {
        ui_state.probe = super::ProbeState::Idle;
    }
```

- [ ] **Step 3: 跑绿**

Run: `cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: `test result: ok.`

Run: `cargo clippy -p mullion-app --all-targets -- -D warnings 2>&1 | tail -3`
Expected: 无 warning。

- [ ] **Step 4: Commit**

```bash
git add crates/mullion-app/src/ui/session_manager/editor.rs
git commit -m "feat(app): 拨测结果卡片,与错误卡片互斥 (F92)

四态各一种配色;last_error 优先展示,拨测结果让位但不清空。
改任意字段清掉旧结论,但不动世代号 —— 在途那次仍是针对这份表单发起的。"
```

---

## Task 15: `keyscan.rs` —— 扫描 `~/.ssh` 里像私钥的文件（F93）

**Files:**
- Create: `crates/mullion-app/src/ui/session_manager/keyscan.rs`
- Modify: `crates/mullion-app/src/ui/session_manager/mod.rs`（加 `mod keyscan;`）

> **绝不读文件内容。** 用户选定的决策是「只看文件名」。私钥是这台机器上最敏感的
> 东西，为了在下拉里显示一行候选去读它、去解析它、去判断它有没有口令，
> 是把风险换便利——不做。同理**不打印路径到日志**。

- [ ] **Step 1: 写失败测试**

新建 `crates/mullion-app/src/ui/session_manager/keyscan.rs`，先只写测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// 造一个假的 .ssh 目录。返回 tempdir(必须持有,drop 即删)。
    fn ssh_dir(names: &[&str]) -> tempfile::TempDir {
        let d = tempfile::tempdir().expect("建临时目录");
        for n in names {
            fs::write(d.path().join(n), b"not a real key").expect("写文件");
        }
        d
    }

    fn names(paths: &[std::path::PathBuf]) -> Vec<String> {
        paths
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    /// F93:只认两种线索 —— 有同名 `.pub` 兄弟,或文件名以 `id_` 开头。
    /// 不读内容,所以只能靠命名约定。
    ///
    /// 自证变红的方式:把 `looks_like_key` 里 `has_pub` 那一支删掉。
    #[test]
    fn picks_id_prefixed_and_pub_paired_only() {
        let d = ssh_dir(&[
            "id_ed25519",
            "id_ed25519.pub",
            "work-bastion",     // 有 .pub 兄弟 → 收
            "work-bastion.pub",
            "id_rsa",           // id_ 前缀,没 .pub → 收
            "notes.txt",        // 既无 .pub 也不是 id_ → 不收
        ]);
        let got = names(&scan(d.path()));
        assert_eq!(got, vec!["id_ed25519", "id_rsa", "work-bastion"], "应按文件名排序");
    }

    /// F93:`.ssh` 里那几个众所周知的非私钥文件必须排除,
    /// 否则用户在下拉里看到 `config` 会当成能选的东西。
    ///
    /// 自证变红的方式:把 `is_known_non_key` 的 `known_hosts` 分支删掉。
    #[test]
    fn excludes_config_known_hosts_and_pub() {
        let d = ssh_dir(&[
            "config",
            "known_hosts",
            "known_hosts.old",
            "authorized_keys",
            "id_ed25519.pub",
            "id_ed25519",
        ]);
        assert_eq!(names(&scan(d.path())), vec!["id_ed25519"]);
    }

    /// F93:目录不存在(Windows 上很常见 —— 从没用过 OpenSSH)或没权限读,
    /// 都只是「没有候选」,不是错误。绝不能让扫描失败冒泡成弹窗。
    ///
    /// 自证变红的方式:把 `read_dir` 的 `Ok(it) => it, Err(_) => return Vec::new()`
    /// 改成 `.expect(..)`。
    #[test]
    fn missing_dir_returns_empty_without_error() {
        let d = tempfile::tempdir().expect("建临时目录");
        let gone = d.path().join("no-such-dir");
        assert!(scan(&gone).is_empty());
    }

    /// F93:一个名叫 `id_trap` 的**目录**(或指向目录的符号链接)不是私钥。
    /// 判定必须用 `metadata()`(跟随链接),不能用 `file_type()`(只看链接本身,
    /// 会把指向目录的链接当成普通文件收进来)。
    ///
    /// 自证变红的方式:把 `entry.metadata()` 换成 `entry.file_type()`。
    #[cfg(unix)]
    #[test]
    fn symlink_to_dir_named_like_a_key_is_excluded() {
        let d = tempfile::tempdir().expect("建临时目录");
        fs::create_dir(d.path().join("real_dir")).expect("建子目录");
        std::os::unix::fs::symlink(d.path().join("real_dir"), d.path().join("id_trap"))
            .expect("建符号链接");
        fs::write(d.path().join("id_ok"), b"x").expect("写文件");
        assert_eq!(names(&scan(d.path())), vec!["id_ok"]);
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

先在 `mod.rs` 顶部 `mod fields;` 附近加 `mod keyscan;`，然后
Run: `cargo test -p mullion-app keyscan 2>&1 | tail -15`
Expected: 编译失败，`cannot find function scan`。

- [ ] **Step 3: 实现**

在 `keyscan.rs` 的 `#[cfg(test)] mod tests` **之前**写：

```rust
//! 扫描 `~/.ssh` 下「看起来像私钥」的文件（F93）。
//!
//! **只看文件名，绝不读内容。** 私钥是这台机器上最敏感的文件；为了在下拉框里
//! 多显示一行候选而去读它、解析它、判断它有没有口令，是拿风险换便利。
//! 同理不打印路径到日志。
//!
//! 代价是判定只能靠命名约定，会有误收（一个恰好叫 `id_notes` 的文本文件）
//! 和漏收（叫 `bastion` 且没有 `.pub` 兄弟的真私钥）。这是**候选列表**，
//! 不是自动选择——用户仍可手输或用「浏览…」，误判的成本是多看一行。

use std::path::{Path, PathBuf};

/// 单次扫描最多返回的条目数。`~/.ssh` 正常只有个位数文件；真碰上几万个
/// 文件的目录，画一个几万项的下拉框只会把 UI 卡死。截断即可，不报错。
const MAX_ENTRIES: usize = 512;

/// 扫描 `dir`，返回按文件名排序的候选私钥路径。
///
/// 目录不存在、没权限、不是目录 —— 一律返回空 vec。这些都只是
/// 「没有候选」，不是需要打扰用户的错误（Windows 上从没用过 OpenSSH
/// 的机器根本没有 `~/.ssh`）。
pub fn scan(dir: &Path) -> Vec<PathBuf> {
    let it = match std::fs::read_dir(dir) {
        Ok(it) => it,
        Err(_) => return Vec::new(),
    };

    // 先收一遍文件名，`looks_like_key` 要靠它判断有没有 `.pub` 兄弟。
    let mut files: Vec<String> = Vec::new();
    for entry in it.flatten().take(MAX_ENTRIES) {
        // `metadata()` 跟随符号链接；`file_type()` 只描述链接本身，
        // 会把指向目录的链接当普通文件收进来。
        let Ok(md) = entry.metadata() else { continue };
        if !md.is_file() {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue; // 非 UTF-8 文件名：显示不了，也就选不了。
        };
        files.push(name);
    }

    let mut out: Vec<PathBuf> = files
        .iter()
        .filter(|n| looks_like_key(n, &files))
        .map(|n| dir.join(n))
        .collect();
    out.sort();
    out
}

/// 返回默认的 `~/.ssh` 路径。取不到 home 时返回 `None`（不 panic）。
///
/// 用 `directories`（`shell/store.rs` 已经在用），而不是自己读 `HOME` —— 
/// Windows 上正确的变量是 `USERPROFILE`，手写这段迟早在一等公民平台上出错。
pub fn default_ssh_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|b| b.home_dir().join(".ssh"))
}

/// 靠命名约定判断 `name` 像不像私钥。`siblings` 是同目录下的全部文件名。
fn looks_like_key(name: &str, siblings: &[String]) -> bool {
    if is_known_non_key(name) {
        return false;
    }
    // 线索一：有同名 `.pub` 兄弟。ssh-keygen 默认成对生成，这条最准。
    let has_pub = siblings.iter().any(|s| s == &format!("{name}.pub"));
    // 线索二：`id_` 前缀。ssh-keygen 的默认命名（id_rsa / id_ed25519…），
    // 公钥被删掉时只剩这条线索。
    has_pub || name.starts_with("id_")
}

/// `.ssh` 里那几个众所周知**不是**私钥的文件。
fn is_known_non_key(name: &str) -> bool {
    name.ends_with(".pub")
        || name == "config"
        || name == "authorized_keys"
        // known_hosts / known_hosts.old / known_hosts2 …
        || name.starts_with("known_hosts")
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app keyscan 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: `test result: ok. 4 passed`（Windows 上 3 passed，符号链接那条是 `#[cfg(unix)]`）

- [ ] **Step 5: 自证变红（四条各一次）**

| 测试 | 破坏方式 | 恢复后 |
|---|---|---|
| `picks_id_prefixed_and_pub_paired_only` | 删掉 `looks_like_key` 里 `has_pub \|\|` | ok |
| `excludes_config_known_hosts_and_pub` | 删掉 `is_known_non_key` 的 `known_hosts` 分支 | ok |
| `missing_dir_returns_empty_without_error` | `read_dir(dir)` 改 `.expect("读目录")` | ok |
| `symlink_to_dir_named_like_a_key_is_excluded` | `entry.metadata()` 换 `entry.file_type()` | ok |

每条：破坏 → `cargo test -p mullion-app <测试名> 2>&1 | grep "test result"` → Expected: FAILED → 恢复 → Expected: ok。

- [ ] **Step 6: Commit**

```bash
git add crates/mullion-app/src/ui/session_manager/keyscan.rs crates/mullion-app/src/ui/session_manager/mod.rs
git commit -m "feat(app): 扫描 ~/.ssh 私钥候选,只看文件名不读内容 (F93)

判定靠两条命名约定:有同名 .pub 兄弟,或 id_ 前缀;排除
config/known_hosts*/authorized_keys/*.pub。目录不存在或无权限 →
空列表,不报错。判目录用 metadata() 而非 file_type(),否则指向目录的
符号链接会被当普通文件收进来。
守护测试:keyscan::tests 四条(含 #[cfg(unix)] 符号链接)"
```

---

## Task 16: 私钥行三段布局与候选下拉（F93）

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/fields.rs`
- Modify: `crates/mullion-app/src/ui/session_manager/editor.rs`

- [ ] **Step 1: 编辑器打开时扫一次**

在 `editor.rs::show` 开头（画任何东西之前）追加：

```rust
    // F93:候选只在编辑器首次打开时扫一次。**不能每帧扫** —— 那是每秒
    // 几十次 read_dir(本项目陷阱 T3 的同类:把 IO 放进渲染热路径)。
    // 关闭编辑器时把 ready 复位,下次打开重新扫(用户可能刚生成了新密钥)。
    if !ui_state.key_candidates_ready {
        ui_state.key_candidates = super::keyscan::default_ssh_dir()
            .map(|d| super::keyscan::scan(&d))
            .unwrap_or_default();
        ui_state.key_candidates_ready = true;
    }
```

并在 `ui/mod.rs::UiState::close_session_manager` 里（Task 5 已加过取消拨测那几行）追加：

```rust
        // F93:下次打开重新扫 —— 用户可能刚 ssh-keygen 生成了新密钥。
        self.key_candidates_ready = false;
        self.key_drop_note = None;
```

- [ ] **Step 2: 私钥行改三段**

`fields.rs` 里私钥路径那一行，替换成：

```rust
        ui.horizontal(|ui| {
            ui.add_sized(
                [FIELD_W - 96.0, 24.0],
                egui::TextEdit::singleline(key_path).hint_text("私钥文件路径"),
            );

            // 候选下拉。为空时禁用并说明原因 —— 一个点了没反应的按钮
            // 比一个明说「没找到」的灰按钮更让人困惑。
            let has_cand = !candidates.is_empty();
            ui.add_enabled_ui(has_cand, |ui| {
                let btn = egui::ComboBox::from_id_salt("key_candidates")
                    .selected_text("▾")
                    .width(28.0);
                btn.show_ui(ui, |ui| {
                    for p in candidates {
                        let label = p
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| p.display().to_string());
                        if ui.selectable_label(false, label).clicked() {
                            *key_path = p.display().to_string();
                        }
                    }
                });
            })
            .response
            .on_disabled_hover_text("未在 ~/.ssh 找到私钥");

            if ui.button("浏览…").clicked() {
                // 没有文件对话框依赖(本切片不新增 crate),这里只把焦点
                // 交回输入框并给一句提示 —— 真正的原生对话框留给后续切片。
                *drop_note = Some("把私钥文件拖进这个窗口,或直接粘贴路径".to_owned());
            }
        });
```

> **签名约定**：`candidates: &[std::path::PathBuf]`、`drop_note: &mut Option<String>`、
> `key_path: &mut String`，由 `fields.rs` 里承载认证 Tab 的那个函数从参数透传下来。
> 调用方 `editor.rs` 传 `&ui_state.key_candidates` 与 `&mut ui_state.key_drop_note`。

- [ ] **Step 3: 跑绿**

Run: `cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: `test result: ok.`

Run: `cargo clippy -p mullion-app --all-targets -- -D warnings 2>&1 | tail -3`
Expected: 无 warning。

- [ ] **Step 4: Commit**

```bash
git add crates/mullion-app/src/ui/session_manager/
git commit -m "feat(app): 私钥行改[输入框][候选▾][浏览…]三段 (F93)

候选在编辑器打开时扫一次并缓存,不进渲染热路径(T3 同类);关闭时复位,
下次打开重扫。候选为空时按钮禁用并 hover 说明「未在 ~/.ssh 找到私钥」。"
```

---

## Task 17: 拖拽私钥文件到窗口（F93）

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/editor.rs`

> **不触碰 T8。** 拖拽走的是 `ctx.input(|i| i.raw.dropped_files)`，是 egui 已经
> 收下的原始事件，与「键盘事件先判后喂」的路由规则无关，不新增任何
> `egui_state.on_window_event` 调用。

- [ ] **Step 1: 实现**

在 `editor.rs::show` 里、认证 Tab 渲染完之后追加：

```rust
    // F93:拖拽私钥。只在认证 Tab 且公钥模式下生效 —— 密码模式下拖一个
    // 文件进来没有任何合理含义,静默忽略好过写进一个用户看不到的字段。
    if ui_state.editor_tab == TAB_AUTH && is_pubkey_mode {
        let hovering = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
        if hovering {
            // 悬停高亮:整个编辑器区域描一圈 accent 边。
            ui.painter().rect_stroke(
                ui.max_rect(),
                8.0,
                egui::Stroke::new(2.0, theme::c32(t.accent)),
            );
        }

        let dropped: Vec<std::path::PathBuf> = ui.ctx().input(|i| {
            i.raw
                .dropped_files
                .iter()
                // `DroppedFile.path` 在 Web 上是 None;桌面端偶尔也会是
                // None(拖的是剪贴板内容而非文件)。没有路径就没法用,静默跳过。
                .filter_map(|f| f.path.clone())
                .collect()
        });

        if let Some(first) = dropped.first() {
            if first.is_dir() {
                ui_state.key_drop_note = Some("请拖入私钥文件,不是目录".to_owned());
            } else {
                *key_path_mut(ui_state) = first.display().to_string();
                ui_state.key_drop_note = if dropped.len() > 1 {
                    // 一个路径框只能放一条路径。明说忽略了几个,
                    // 好过让用户以为拖进去的另外几个也生效了。
                    Some(format!("已取第一个文件,忽略其余 {} 个", dropped.len() - 1))
                } else {
                    None
                };
            }
        }
    }

    // 提示条。放在按钮条上方,与错误/拨测卡片同一列。
    if let Some(note) = ui_state.key_drop_note.clone() {
        ui.horizontal(|ui| {
            ui.colored_label(theme::c32(t.fg_dimmer), note);
            if ui.small_button("×").clicked() {
                ui_state.key_drop_note = None;
            }
        });
    }
```

> `key_path_mut(ui_state)` 是取编辑缓冲里私钥路径字段的可变引用；
> 若 `editor.rs` 已有等价访问方式（例如 `ui_state.editor.as_mut()?.key_path`），
> 直接用它，**不要新增一个包装函数**。`TAB_AUTH` 是认证 Tab 的下标常量，
> `is_pubkey_mode` 取自编辑缓冲里的认证方式单选。

- [ ] **Step 2: 跑绿**

Run: `cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: `test result: ok.`

Run: `cargo clippy -p mullion-app --all-targets -- -D warnings 2>&1 | tail -3`
Expected: 无 warning。

> **无法自动验证**：拖拽本身要有窗口和真实文件管理器，无头容器验不了。
> 这一条进人工验收清单（Task 19）。

- [ ] **Step 3: Commit**

```bash
git add crates/mullion-app/src/ui/session_manager/editor.rs
git commit -m "feat(app): 拖拽私钥文件填路径 (F93)

只在认证 Tab + 公钥模式下生效;悬停时描 accent 边。多文件取第一个并
提示忽略了几个,拖目录明确拒绝,path 为 None 静默跳过。
走 ctx.input(i.raw.dropped_files),不新增 on_window_event 调用,不触碰 T8。
未验证:拖拽需真实窗口与文件管理器,进人工验收清单。"
```

---

## Task 18: spec.md 追加 F91/F92/F93

**Files:**
- Modify: `spec.md`

- [ ] **Step 1: 追加需求行**

在 §4.6（会话管理）功能表最后追加三行（当前最大编号 F90，F91–F93 未被占用）：

```markdown
| F91 | 会话编辑器必填校验：会话名称 / 主机 / 用户名任一为空时，「保存」「保存并连接」禁用，缺项所在 Tab 打红点，字段标红星，按钮 hover 说明缺什么。端口有默认 22，不算必填。 | P1 |
| F92 | 「测试连接」：按当前表单（含未保存改动）走完 代理 → 跳板 → TCP → 握手 → 指纹 → 认证，成功后**立即断开**，不开 channel、不起 pty。20 秒超时。结果以卡片展示。切换会话或关闭窗口即作废在途结果。 | P1 |
| F93 | 私钥选择辅助：扫描 `~/.ssh` 列出候选（**只看文件名，不读内容**），支持把私钥文件拖入窗口填路径。 | P2 |
```

- [ ] **Step 2: 给 F3 补一句**

在 F3（TOFU 指纹）那一行的描述末尾追加：

```
（F92「测试连接」触发的指纹确认**仅本次信任、不写 known_hosts**——一次拨测不该
在用户还没决定要不要保存这个会话时就改动信任库。）
```

- [ ] **Step 3: Commit**

```bash
git add spec.md
git commit -m "docs(spec): 追加 F91 必填校验 / F92 测试连接 / F93 私钥辅助

并给 F3 补注:测试连接触发的 TOFU 确认仅本次信任,不落盘。"
```

---

## Task 19: 交付一条龙（bump → 跑绿 → 交叉编译 → 发 Release）

**Files:**
- Modify: `Cargo.toml`（`workspace.package.version`）

> 按项目交付约定默认执行，不要停下来问「要不要发版」。

- [ ] **Step 1: 升 patch 版本**

把 `Cargo.toml` 的 `workspace.package.version` 第三位 +1（当前 `0.1.N` → `0.1.N+1`）。

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: 版本 0.1.N+1(会话管理器 UI 打磨 + 必填校验 + 测试连接 + 私钥辅助)"
```

- [ ] **Step 2: 跑绿（三样缺一不可）**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
cargo fmt --check
```
Expected: 全 ok / 无输出。**不绿不发。**

- [ ] **Step 3: 交叉编译 + objdump 验收**

```bash
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app
objdump -p target/x86_64-pc-windows-gnu/release/mullion-app.exe | grep -i "DLL Name"
```
Expected: **不出现** `libgcc_s_seh-1.dll` 或 `libwinpthread-1.dll`。出现即不合格，
按 `docs/cross-compile-windows.md` 修静态链接参数后重来。

- [ ] **Step 4: 发 Release**

```bash
cp target/x86_64-pc-windows-gnu/release/mullion-app.exe mullion.exe
sha256sum mullion.exe > mullion.exe.sha256
HTTPS_PROXY=http://127.0.0.1:7890 gh release create v0.1.N+1 \
  mullion.exe mullion.exe.sha256 -t "v0.1.N+1" -F notes.md --repo kilobitcy/Mullion
```

**Release 标题只能是纯版本号 `v0.1.N+1`**——不带破折号、不带摘要、不带 emoji。

`notes.md` 必须包含：修了什么 + sha256 + 首次运行提示（`Unblock-File .\mullion.exe`）
+ 下面这份人工验收清单：

```markdown
## 人工验收清单

**布局（F90 根因）**
1. 拉窄窗口到最小，右栏内容不再被裁切
2. 拖动左右分隔条，窗口宽度不跟着变
3. 分隔条能拖到 220–440 之间，超出范围会停住

**视觉**
4. 列表副文本（user@host）在深色底上看得清（原先偏灰）
5. 表单有分区小标题，字段不再糊成一片
6. 底部只有一个实心主按钮（「保存并连接」），在最右

**F91 必填校验**
7. 新建会话、三项都空时，「保存」「保存并连接」是灰的
8. hover 灰按钮，提示说得出缺哪一项
9. 缺项所在 Tab 有红点，字段有红星
10. **存量脏数据不锁死**（设计文档 §7.4）：找一条历史会话（或手改 TOML 把
    `user` 清空），打开后「保存」是灰的，但**仍能编辑字段、仍能删除这条会话、
    仍能关闭窗口**；把缺的字段填上，按钮立即恢复可用

**F92 测试连接**
11. 对一个正确配置的会话点「测试连接」→ 出「连接成功」卡片
12. 故意写错密码 → 出「连接失败」卡片，说得出原因
13. 对一个从未连过的主机点「测试连接」→ 弹指纹确认；**确认后检查
    `known_hosts` 没有被写入这台主机**（这是本切片安全性最关键的一条）
14. 点「测试连接」后立刻切到另一个会话 → 结果不会串到新会话的表单上
15. 拨测期间改任意字段 → 成功/失败卡片消失
16. 编辑一条已保存的密码会话、**不重输密码**直接点「测试连接」→ 能连上
    （不能报「缺少凭据」——库里的旧凭据必须参与合成）

**F93 私钥**
17. 认证选公钥 → 「▾」下拉能列出 `~/.ssh` 里的私钥
18. 从资源管理器拖一个私钥文件进窗口 → 路径被填上
19. 拖一个文件夹进去 → 提示「请拖入私钥文件，不是目录」

> 第 11–19 条在无头容器里完全无法验证（需真实窗口、真实 SSH 链路、
> 真实文件管理器）。第 4/5/6/10 条需人眼判定。
```

- [ ] **Step 5: 报给用户**

Release 链接 + sha256 + 上面这份清单。

---

## 收尾

全部任务完成后，用 `superpowers:finishing-a-development-branch` 收束分支
（本项目惯例：squash 入 main）。
