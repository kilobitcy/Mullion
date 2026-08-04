# 会话管理器 UI 重构实现计划(F90 + F73)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把当前三个独立 `egui::Window`(会话列表 / 编辑器 / 删除确认)合并成设计稿的
880×560 单窗双栏会话管理器,并顺带修掉「编辑已有会话时密码框留空会静默清除凭据」的缺陷。

**Architecture:** `crates/mullion-app/src/ui/session_manager.rs`(1065 行)拆成
`session_manager/` 目录四文件:`mod.rs`(窗口骨架 + 双栏布局)、`buffer.rs`(表单缓冲与
纯逻辑,零 egui)、`list.rs`(左栏)、`editor.rs`(右栏)。凭据的「保持/覆盖/清除」三态用
app 层纯函数 `merge_secret` 在保存时合成,`mullion-store` 零改动。所有改 store / 发起连接
的动作仍然只往 `UiState` 写意图,由 `app.rs` 在 egui 借用释放后统一施加。

**Tech Stack:** Rust 2021 / egui 0.30.0 / winit 0.30 / wgpu / `mullion-store`(TOML + keyring)。
测试全部是 `cargo test -p mullion-app` 里的 `#[test]`,UI 测试靠 `ui/mod.rs` 已有的
`run_frame` / `rendered_text` 无头跑 egui。

**设计真源:** `docs/superpowers/specs/2026-08-03-session-manager-ui-design.md`(下称 spec)。
本计划的章节引用(§3、§5.4…)都指它。

---

## 文件结构

| 文件 | 职责 | 目标行数 |
|---|---|---|
| `crates/mullion-app/src/ui/session_manager/mod.rs` | `show()` 窗口骨架、双栏布局、常量、子模块声明与再导出 | ~200 |
| `crates/mullion-app/src/ui/session_manager/buffer.rs` | `EditorBuffer` / `AuthKindUi` / `ProxyModeUi` / `SecretField` / `SecretPresence` / `SaveIntent` / `build_draft` / `secret_fields` / `merge_secret` / `sync_has_passphrase` / `is_dirty`。**零 egui import** | ~420 |
| `crates/mullion-app/src/ui/session_manager/list.rs` | 左栏:搜索框、分组树、`session_row` 手绘、内联删除确认、底部「新建/导入」 | ~300 |
| `crates/mullion-app/src/ui/session_manager/editor.rs` | 右栏:标题条、三 Tab、错误卡片、字段表单、密码三态控件、底部「保存/连接」 | ~380 |
| `crates/mullion-app/src/theme.rs` | 新增 `danger_soft` token | +2 |
| `crates/mullion-app/src/ui/mod.rs` | `UiState` 增删字段、`UiFrame` 新增两个 `Copy` 字段、`set_error` | 局部 |
| `crates/mullion-app/src/shell/store.rs` | 新增 `secret()` / `secret_presence()` 转发 | +20 |
| `crates/mullion-app/src/app.rs` | `apply_save` 纯函数 + 10 处 `last_error` 改走 `set_error` + 删 `editor_open` | 局部 |

**约束(每个任务都适用):**
- `buffer.rs` 里**不许出现 `use egui`**。它是本切片唯一能脱离 GUI 单测的地方,漏进 egui 类型就等于把可测试性拆掉。
- 新增守护测试必须自证「破坏被守护的属性后确实变红」,且注入点要扎在真实疏漏会发生的位置(P0-b 的教训)。
- 每个任务结束前跑 `cargo clippy -p mullion-app --all-targets -- -D warnings`,不绿不提交。

---

## Task 1: 拆出 `buffer.rs`(纯搬移,零行为变化)

**Files:**
- Create: `crates/mullion-app/src/ui/session_manager/mod.rs`(由 `session_manager.rs` 改名而来)
- Create: `crates/mullion-app/src/ui/session_manager/buffer.rs`
- Delete: `crates/mullion-app/src/ui/session_manager.rs`

- [ ] **Step 1: 建目录并把文件改名**

```bash
cd /data/Mullion
mkdir -p crates/mullion-app/src/ui/session_manager
git mv crates/mullion-app/src/ui/session_manager.rs \
       crates/mullion-app/src/ui/session_manager/mod.rs
```

- [ ] **Step 2: 确认改名后仍然编译**

Run: `cargo build -p mullion-app 2>&1 | tail -5`
Expected: `Finished` —— Rust 2018+ 的 `mod.rs` 目录形式与单文件形式等价,`ui/mod.rs` 里的
`pub mod session_manager;` 不用改。

- [ ] **Step 3: 把纯逻辑整块搬进 `buffer.rs`**

从 `session_manager/mod.rs` **剪切**下列内容到新文件 `crates/mullion-app/src/ui/session_manager/buffer.rs`
(原文照搬,一个字符都不要改):

- `AuthKindUi`、`ProxyModeUi` 两个 enum 及其 `impl`
- `pub struct EditorBuffer` + `impl Default for EditorBuffer` + `fn redacted` + `impl Debug for EditorBuffer`
- `impl EditorBuffer { fn from_record }`
- `pub struct SaveIntent`
- `pub(crate) fn build_draft`
- 文件末尾 `#[cfg(test)] mod tests` 里**调用 `build_draft` 的那 12 个测试**与它们共用的 `fn buf()` helper

`buffer.rs` 顶部加文件头与 import:

```rust
//! 会话编辑表单的**纯逻辑**:表单缓冲、表单 → `SessionDraft` 的转换、凭据三态合成。
//!
//! **本文件不许 `use egui`**。会话表单的 bug(端口解析、代理三态、凭据被静默清除)
//! 全部能在没有窗口的情况下单测复现——这是把它从 UI 代码里切出来的全部理由。

use std::path::PathBuf;

use mullion_store::model::{
    AppearancePrefs, Auth, AuthKind, Connection, Identity, Protocol, SecretEntry, SessionRecord,
    TerminalPrefs,
};
use mullion_store::{GroupId, SessionId};
use mullion_store::vault::SessionDraft;
```

> 实际 import 路径以原 `mod.rs` 顶部的 `use` 为准 —— 把原文件里被搬走的那部分需要的
> `use` **复制**(不是剪切)过来,再让 `cargo build` 的 `unused_imports` 警告告诉你两边各该删哪些。

- [ ] **Step 4: 在 `mod.rs` 里声明并再导出**

在 `session_manager/mod.rs` 顶部(文件头注释之后)加:

```rust
mod buffer;

pub use buffer::{EditorBuffer, SaveIntent};
pub(crate) use buffer::{build_draft, AuthKindUi, ProxyModeUi};
```

- [ ] **Step 5: 跑到绿**

Run: `cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: `test result: ok.` —— 测试**总数与搬移前一致**(14 个 `session_manager` 相关
`#[test]`,现在分布在两个模块里)。数量对不上就是搬漏了。

Run: `cargo clippy -p mullion-app --all-targets -- -D warnings`
Expected: 无输出。

- [ ] **Step 6: 提交**

```bash
git add -A crates/mullion-app/src/ui
git commit -m "refactor(app): session_manager 拆出 buffer.rs 纯逻辑模块 (F90)

纯搬移,零行为变化。buffer.rs 不 import egui,表单逻辑从此可脱离 GUI 单测。
测试全部原样通过,数量未变。"
```

---

## Task 2: 双栏骨架 + 删掉独立编辑器窗口

**目标:** 让「会话管理器」变成**唯一**一个窗口,左 300px 列表、右自适应编辑区,
内容用现有的列表/表单原样填充(视觉重做留给 Task 10–13)。这一步存在的理由是尽快
拿到一个能上机的构建,验证 spec §3 里那条无头环境验不了的假设。

**Files:**
- Create: `crates/mullion-app/src/ui/session_manager/list.rs`
- Create: `crates/mullion-app/src/ui/session_manager/editor.rs`
- Modify: `crates/mullion-app/src/ui/session_manager/mod.rs`
- Modify: `crates/mullion-app/src/ui/mod.rs`(删 `UiState::editor_open`,改 `build_ui` 的调用)
- Modify: `crates/mullion-app/src/app.rs:793`、`crates/mullion-app/src/app.rs:911`
- Test: `crates/mullion-app/src/ui/mod.rs` 的 `mod tests`

- [ ] **Step 1: 写会变红的单窗断言测试**

加到 `crates/mullion-app/src/ui/mod.rs` 的 `#[cfg(test)] mod tests` 末尾:

```rust
/// F90:会话管理器必须是**单窗**。设计稿把「列表 / 编辑器 / 删除确认」三个弹窗
/// 合成一个 880×560 双栏窗口,再冒出第二个顶层 `egui::Window` 就是回归。
///
/// 计数机制:`Memory::areas().order()` 只给 `Vec<LayerId>`,不带 `UiKind` 标签,
/// 没法按「是不是 Window」过滤。但 `egui::Window` 默认落在 `Order::Middle`,而
/// `ComboBox` / `Popup` / tooltip 走 `Order::Foreground`,菜单栏与状态栏是
/// `TopBottomPanel`(`Order::Background`)——按 `Order::Middle` 过滤即等价于数窗口。
///
/// 自证会变红:把 `session_manager::editor::show` 的内容重新包一层
/// `egui::Window::new("编辑会话").show(ctx, ..)`(即本切片要消灭的那个窗口),
/// 这条断言立刻报 `2 != 1`。
#[test]
fn session_manager_is_a_single_window_so_the_editor_cannot_pop_out_again() {
    let ctx = egui::Context::default();
    crate::theme::apply_egui(&ctx, &crate::theme::MULLION_DARK);
    let mut st = UiState {
        session_manager_open: true,
        ..Default::default()
    };
    let frame = UiFrame {
        store_available: true,
        ..base_frame()
    };
    // 跑两遍:egui 的 Area 首帧是不可见的 sizing pass,第二帧才落进 areas order。
    for _ in 0..2 {
        run_frame(&ctx, &mut st, frame, egui::RawInput::default());
    }

    let windows = ctx.memory(|m| {
        m.areas()
            .order()
            .iter()
            .filter(|l| l.order == egui::Order::Middle)
            .count()
    });
    assert_eq!(
        windows, 1,
        "会话管理器必须是单窗;新增任何顶层 egui::Window 都会让这条变红"
    );
}
```

- [ ] **Step 2: 跑测试,确认它现在是红的**

Run: `cargo test -p mullion-app single_window 2>&1 | grep -E "test result|assertion|panicked"`
Expected: FAIL,形如 `assertion `left == right` failed: 会话管理器必须是单窗`,
`left: 2`(当前列表窗 + 编辑器窗)。

> 若 `left` 是 1,说明测试没打开编辑器窗口 —— 在构造 `UiState` 时补
> `editor_open: true, editor: Some(EditorBuffer::default())` 之类的字段(以当前
> `UiState` 实际字段名为准)让两个窗口都开着,确认红了再往下走。

- [ ] **Step 3: 建 `list.rs`,把现有左侧列表整块搬进去**

`crates/mullion-app/src/ui/session_manager/list.rs`:

```rust
//! 会话管理器**左栏**:搜索、分组树、会话行、底部操作。
//!
//! 只读 `UiFrame` 的数据,只往 `UiState` 写意图 —— 不碰 `SessionStore`
//! (egui 闭包里拿不到 `&mut SessionStore`,这是 app 侧的硬约束)。

use egui::Ui;
use mullion_store::model::SessionRecord;
use mullion_store::GroupRecord;

use crate::theme::{self, Theme};
use crate::ui::UiState;

pub(super) fn show(
    ui: &mut Ui,
    t: &Theme,
    ui_state: &mut UiState,
    sessions: &[SessionRecord],
    groups: &[GroupRecord],
) {
    // ← 把原 mod.rs `show()` 里 :358-442 的分组 CollapsingHeader + Grid + 底部按钮
    //    整段原样搬到这里,把 `ui` 换成本函数的 `ui` 参数。
    let _ = (t, ui_state, sessions, groups);
    let _ = theme::c32(t.fg);
}
```

搬完后删掉上面那两行 `let _ = ...` 占位。

- [ ] **Step 4: 建 `editor.rs`,把现有 `show_editor` 的表单整块搬进去**

`crates/mullion-app/src/ui/session_manager/editor.rs`:

```rust
//! 会话管理器**右栏**:被编辑会话的表单。
//!
//! 原来是一个独立的 `egui::Window`(F90 前),现在是主窗右侧的 `CentralPanel`。

use egui::Ui;
use mullion_store::model::SessionRecord;
use mullion_store::GroupRecord;

use crate::theme::Theme;
use crate::ui::UiState;

pub(super) fn show(
    ui: &mut Ui,
    t: &Theme,
    ui_state: &mut UiState,
    sessions: &[SessionRecord],
    groups: &[GroupRecord],
) {
    // ← 把原 mod.rs `show_editor()` 里 :495-746 的表单 Grid + 底部按钮搬到这里,
    //    **去掉**外层 `egui::Window::new(title).open(&mut open).show(ctx, |ui| {...})`
    //    包装,只保留闭包体。原来 `open` 变量控制的关窗行为改由左栏选中态承担。
    let _ = (t, ui_state, sessions, groups);
}
```

- [ ] **Step 5: 在 `mod.rs` 里写双栏骨架**

把 `session_manager/mod.rs` 里的 `pub fn show(...)` 整个替换为:

```rust
mod buffer;
mod editor;
mod list;

pub use buffer::{EditorBuffer, SaveIntent};
pub(crate) use buffer::{build_draft, AuthKindUi, ProxyModeUi};

/// 设计稿 §3:880×560 单窗,左栏定宽 300。
pub(crate) const WINDOW_W: f32 = 880.0;
pub(crate) const WINDOW_H: f32 = 560.0;
pub(crate) const LIST_W: f32 = 300.0;
/// 内容区最小高度。egui 的 `Window` 高度默认跟内容走,不撑到 `default_size` 给的
/// 高度;靠这一行把双栏撑满,否则会话少时窗口会缩成一条。见 §3 的待验证假设。
pub(crate) const CONTENT_MIN_HEIGHT: f32 = 480.0;

pub fn show(
    ctx: &egui::Context,
    t: &Theme,
    ui_state: &mut UiState,
    sessions: &[SessionRecord],
    groups: &[GroupRecord],
    store_available: bool,
) {
    if !ui_state.session_manager_open {
        return;
    }

    let mut open = true;
    egui::Window::new("会话管理器")
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size([WINDOW_W, WINDOW_H])
        .min_width(720.0)
        .frame(
            egui::Frame::window(&ctx.style())
                .fill(theme::c32(t.bar_status))
                .rounding(12.0),
        )
        .show(ctx, |ui| {
            ui.set_min_height(CONTENT_MIN_HEIGHT);

            // §3.1 降级:没有会话库时不画双栏,只给一句话,避免用户对着空表单填半天。
            if !store_available {
                ui.colored_label(
                    theme::c32(t.danger),
                    "会话库不可用,无法读写会话(详见状态栏错误)。",
                );
                return;
            }

            egui::SidePanel::left(ui.id().with("sm_list"))
                .exact_width(LIST_W)
                .resizable(false)
                .frame(egui::Frame::none().fill(theme::c32(t.panel_bg)).inner_margin(10.0))
                .show_inside(ui, |ui| list::show(ui, t, ui_state, sessions, groups));

            egui::CentralPanel::default()
                .frame(egui::Frame::none().fill(theme::c32(t.bar_status)).inner_margin(14.0))
                .show_inside(ui, |ui| editor::show(ui, t, ui_state, sessions, groups));
        });

    if !open {
        ui_state.session_manager_open = false;
    }
}
```

顶部 `use` 补齐:`use crate::theme::{self, Theme};`、`use crate::ui::UiState;`、
`use mullion_store::model::SessionRecord;`、`use mullion_store::GroupRecord;`。

- [ ] **Step 6: 删掉 `UiState::editor_open`,把 11 个引用点收干净**

Run: `grep -rn "editor_open" crates/mullion-app/src`
Expected: 11 处。逐一处理:

- `ui/mod.rs:74` 的字段定义 —— 删。
- `ui/mod.rs:186` 的 `if ui_state.session_manager_open || ui_state.editor_open` —— 改成只判 `session_manager_open`。
- `app.rs:793` `self.ui.editor_open = false;` —— 删(同一处的 `session_manager_open = false` 保留)。
- `app.rs:911` modal 判断里的 `|| self.ui.editor_open` —— 删。
- 其余在 `session_manager/` 内部的引用 —— 随窗口合并一并删除;编辑区是否有内容改由
  `ui_state.editor.is_some()` 判定。

改完再 grep 一次:

Run: `grep -rn "editor_open" crates/mullion-app/src`
Expected: 无输出。

- [ ] **Step 7: 跑到绿**

Run: `cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: `test result: ok.`,且 `single_window` 那条从红转绿。

Run: `cargo clippy -p mullion-app --all-targets -- -D warnings`
Expected: 无输出。

- [ ] **Step 8: 提交**

```bash
git add -A crates/mullion-app/src
git commit -m "feat(app): 会话管理器合并成单窗双栏骨架 (F90)

三个独立 egui::Window(列表/编辑器/删除确认)合成 880x560 双栏单窗,
UiState::editor_open 随之删除(11 处引用清零)。内容暂用原有列表与表单填充,
视觉重做在后续任务。

守护测试:ui::tests::session_manager_is_a_single_window_so_the_editor_cannot_pop_out_again
(改前 left=2 红,改后绿)。触到 T8 同类的输入路由分叉点 app.rs:911 modal 判断。"
```

---

## Task 3: 上机验证 `set_min_height` 假设(**阻塞点**)

spec §3 有一条无头环境验证不了的 GUI 假设:`egui::Window` 的高度默认跟内容走,
`ui.set_min_height(CONTENT_MIN_HEIGHT)` 能不能把双栏撑到 560,只有人眼能判定。
这一步先出一个能上机的构建,拿到结论再往下做右栏——否则右栏全做完才发现高度模型
不对,要整段返工。

**Files:**
- Modify: `Cargo.toml:12`(版本 0.1.15 → 0.1.16)

- [ ] **Step 1: bump 版本**

把 `/data/Mullion/Cargo.toml` 第 12 行改为:

```toml
version = "0.1.16"
```

```bash
git add Cargo.toml
git commit -m "chore: 版本 0.1.16(会话管理器双栏骨架,上机验证窗口高度模型)"
```

- [ ] **Step 2: 跑绿(全工作区)**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```
Expected: 全部 `ok.`、clippy 与 fmt 均无输出。

- [ ] **Step 3: 交叉编译并做 objdump 依赖验收**

```bash
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app
x86_64-w64-mingw32-objdump -p target/x86_64-pc-windows-gnu/release/mullion.exe \
  | grep -i "DLL Name"
```
Expected: 输出里**不得出现** `libgcc_s_seh-1.dll` 或 `libwinpthread-1.dll`。
出现即不合格,按 `docs/cross-compile-windows.md` 修完重来。

- [ ] **Step 4: 发 Release**

```bash
cd target/x86_64-pc-windows-gnu/release
sha256sum mullion.exe > mullion.exe.sha256
cat mullion.exe.sha256
```

写 `notes.md`,正文包含:本版改了什么(单窗双栏骨架)、sha256、首次运行提示
(未签名 exe 会被 SmartScreen 拦,`Unblock-File .\mullion.exe`),以及下面这份验收清单:

```
## 人工验收清单(v0.1.16 骨架验证版)
- [ ] 菜单打开「会话管理器」,只弹出**一个**窗口(不再有独立的编辑器窗口)
- [ ] 窗口初始高度接近 560px,左右两栏**等高撑满**,不会缩成一条细窗
- [ ] 会话列表只有 1 条(或 0 条)时,窗口高度仍然撑住,右栏不塌陷
- [ ] 拖动窗口右下角能自由缩放,左栏始终 300px 定宽
- [ ] 左栏选中会话后,右侧表单填入该会话内容
```

先推 main 再发 Release(P0-b 的教训:tag 会指向旧提交):

```bash
git push origin main
HTTPS_PROXY=http://127.0.0.1:7890 gh release create v0.1.16 \
  target/x86_64-pc-windows-gnu/release/mullion.exe \
  target/x86_64-pc-windows-gnu/release/mullion.exe.sha256 \
  -t "v0.1.16" -F notes.md --repo kilobitcy/Mullion
```

- [ ] **Step 5: 把 Release 链接 + sha256 + 验收清单报给用户,等结论**

**这一步会阻塞。** 拿到人工结论前不要开始 Task 4 之后的右栏视觉任务
(Task 4–9 是纯逻辑,不依赖高度结论,可以并行推进)。

- [ ] **Step 6: 结论为「高度没撑住」时,换退路**

把 `mod.rs` 的窗口构造改成:

```rust
    egui::Window::new("会话管理器")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .fixed_size([WINDOW_W, WINDOW_H])
        .frame(
            egui::Frame::window(&ctx.style())
                .fill(theme::c32(t.bar_status))
                .rounding(12.0),
        )
        .show(ctx, |ui| {
            ui.set_min_size(ui.available_size());
```

并把 `CONTENT_MIN_HEIGHT` 常量与那行 `ui.set_min_height(...)` 删掉。改完重跑 Step 2,
提交:

```bash
git commit -am "fix(app): 会话管理器改 fixed_size + set_min_size 撑高度 (F90)

上机验证否定了 set_min_height 能撑满 Window 的假设(spec §3 待验证项),
按设计给的退路改成固定尺寸窗口 + 闭包内 set_min_size(available_size)。"
```

- [ ] **Step 7: 把结论回填进 spec**

无论成立与否,把 `docs/superpowers/specs/2026-08-03-session-manager-ui-design.md` §3 里
「待验证假设」那段改写成结论(哪个方案胜出、v0.1.16 实测),并提交:

```bash
git commit -am "docs: 回填 set_min_height 假设的实机验证结论 (F90)"
```

---

## Task 4: theme 新增 `danger_soft`

设计稿的错误卡片用的红比 `danger`(#e81123,来自 Windows 系统红)柔和。这是本切片
与已冻结色板(`docs/superpowers/specs/2026-07-29-ui-visual-baseline-design.md`)的**唯一**分歧,
按 spec §2 新增一个 token,不动 `danger`。

**Files:**
- Modify: `crates/mullion-app/src/theme.rs:18-70`(结构体)、`:73-103`(常量)

- [ ] **Step 1: 加字段**

在 `pub struct Theme` 的 `pub danger: Rgb,` 之后插入:

```rust
    /// 错误**卡片/横幅**底纹用的柔和红(F90 会话管理器 §5.2)。`danger` 是
    /// Windows 系统红 #e81123,用作大面积底色太刺眼;两者不互相替代。
    pub danger_soft: Rgb,
```

在 `pub const MULLION_DARK: Theme` 的 `danger: Rgb::new(0xe8, 0x11, 0x23),` 之后插入:

```rust
    danger_soft: Rgb::new(0xe0, 0x67, 0x67),
```

- [ ] **Step 2: 编译并跑 theme 测试**

Run: `cargo test -p mullion-app theme:: 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok.`(theme.rs 里已有 6 个测试)

- [ ] **Step 3: 提交**

```bash
git add crates/mullion-app/src/theme.rs
git commit -m "feat(app): 主题新增 danger_soft token (F90)

会话管理器错误卡片的底纹用它;danger(#e81123 系统红)保留给强调文字,
两者不互相替代。"
```

---

## Task 5: 凭据三态 —— `SecretField` / `merge_secret` / `sync_has_passphrase`

**这是 F73 的核心。** 当前缺陷:编辑已有会话时,密码框因为 store 不明文回吐而永远
是空的,保存就把已存凭据静默清掉。store 的 `Option<SecretEntry>` 只有二态(覆盖/删除),
UI 需要三态(保持/覆盖/清除)。解法是在 app 层用纯函数合成,`mullion-store` 零改动。

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/buffer.rs`
- Test: 同文件的 `mod tests`

- [ ] **Step 1: 写 8 条失败测试**

追加到 `buffer.rs` 的 `#[cfg(test)] mod tests`:

```rust
    fn entry(pw: Option<&str>, pp: Option<&str>, proxy: Option<&str>) -> SecretEntry {
        SecretEntry {
            password: pw.map(String::from),
            passphrase: pp.map(String::from),
            proxy_password: proxy.map(String::from),
        }
    }

    /// F73 红线:用户没碰密码框 → 已存密码必须原样留着。
    /// 这正是本切片要修的 bug:改前 `build_draft` 把空字符串当「清除」,
    /// 编辑任意一个已有会话再保存,密码就没了。
    /// 自证会变红:把 `merge_secret` 里 `SecretField::Keep => existing.cloned()`
    /// 改成 `=> None`(即改前的行为),这条立刻红。
    #[test]
    fn keep_preserves_existing_password() {
        let existing = entry(Some("old-pw"), None, None);
        let got = merge_secret(
            Some(&existing),
            &SecretField::Keep,
            &SecretField::Keep,
            &SecretField::Keep,
        );
        assert_eq!(got.unwrap().password.as_deref(), Some("old-pw"));
    }

    #[test]
    fn set_overwrites_existing_password() {
        let existing = entry(Some("old-pw"), None, None);
        let got = merge_secret(
            Some(&existing),
            &SecretField::Set("new-pw".into()),
            &SecretField::Keep,
            &SecretField::Keep,
        );
        assert_eq!(got.unwrap().password.as_deref(), Some("new-pw"));
    }

    /// 用户主动清空 → 真的清除。这是「保持不变」的对偶,不能因为修了 Keep
    /// 就把清除路径一起弄丢。
    #[test]
    fn clear_removes_existing_password() {
        let existing = entry(Some("old-pw"), Some("ph"), None);
        let got = merge_secret(
            Some(&existing),
            &SecretField::Clear,
            &SecretField::Keep,
            &SecretField::Keep,
        )
        .expect("passphrase 还在,整条不该塌成 None");
        assert_eq!(got.password, None);
        assert_eq!(got.passphrase.as_deref(), Some("ph"));
    }

    /// 三个字段全空 → 整条 `SecretEntry` 收成 `None`,不要在 secrets.enc 里
    /// 留一条三字段全 None 的空壳。
    #[test]
    fn all_cleared_collapses_to_none() {
        let existing = entry(Some("pw"), Some("ph"), Some("proxy"));
        let got = merge_secret(
            Some(&existing),
            &SecretField::Clear,
            &SecretField::Clear,
            &SecretField::Clear,
        );
        assert!(got.is_none(), "全清后不该留空壳 SecretEntry");
    }

    /// 新建会话(existing = None)且全部 Keep → 仍是 None,不能凭空造出空条目。
    #[test]
    fn keep_on_empty_existing_stays_none() {
        let got = merge_secret(
            None,
            &SecretField::Keep,
            &SecretField::Keep,
            &SecretField::Keep,
        );
        assert!(got.is_none());
    }

    /// 三个字段互相独立:清掉密码不该波及私钥口令与代理口令。
    #[test]
    fn clearing_password_keeps_other_secrets() {
        let existing = entry(Some("pw"), Some("ph"), Some("proxy"));
        let got = merge_secret(
            Some(&existing),
            &SecretField::Clear,
            &SecretField::Keep,
            &SecretField::Keep,
        )
        .unwrap();
        assert_eq!(got.password, None);
        assert_eq!(got.passphrase.as_deref(), Some("ph"));
        assert_eq!(got.proxy_password.as_deref(), Some("proxy"));
    }

    /// `SecretField` 会进 `EditorBuffer` 的 Debug、也可能进日志/panic 消息。
    /// 明文一旦被 `{:?}` 打出来,加密存储就白做了(与 `SecretEntry` 同一条红线)。
    #[test]
    fn secret_field_debug_never_leaks_plaintext() {
        let s = format!("{:?}", SecretField::Set("hunter2".to_string()));
        assert!(!s.contains("hunter2"), "Debug 泄漏了明文:{s}");
        assert!(s.contains("<已设置>"), "应打码成 <已设置>,实得:{s}");
    }

    /// §5.4.3:`has_passphrase` 必须跟**合成后**的凭据走,而不是跟表单当前
    /// 内容走。否则「有已存口令 + 用户没碰口令框」会被写成 has_passphrase=false,
    /// 下次连接时 russh 拿到加密私钥却不知道要口令,直接认证失败。
    /// 自证会变红:让 `sync_has_passphrase` 早退不写值,这条报 false != true。
    #[test]
    fn has_passphrase_follows_merged_secret_not_form() {
        let mut buf = buf();
        buf.auth_kind = AuthKindUi::PublicKey;
        buf.key_path = "/k".into();
        // 用户没碰口令框 → 表单是空的
        let mut draft = build_draft(&buf).expect("build");
        assert!(
            matches!(draft.auth.kind, AuthKind::PublicKey { has_passphrase: false, .. }),
            "表单空 + 无已存值时应为 false"
        );
        // 但库里存着口令 → 合成后应变 true
        let merged = entry(None, Some("ph"), None);
        sync_has_passphrase(&mut draft, Some(&merged));
        assert!(
            matches!(draft.auth.kind, AuthKind::PublicKey { has_passphrase: true, .. }),
            "合成后有 passphrase,has_passphrase 必须是 true"
        );
    }
```

- [ ] **Step 2: 跑到红**

Run: `cargo test -p mullion-app buffer:: 2>&1 | grep -E "^error|cannot find"`
Expected: 编译失败,`cannot find function \`merge_secret\``、
`cannot find type \`SecretField\``、`cannot find function \`sync_has_passphrase\``。

- [ ] **Step 3: 实现**

追加到 `buffer.rs`(放在 `EditorBuffer` 定义之后、`build_draft` 之前):

```rust
/// 一个密码框的**三态**意图。
///
/// store 的 `Option<String>` 只有二态(有值 / 无值),保存时无法区分「用户没动」
/// 和「用户清空了」——这正是 F73 那个「编辑一下会话密码就没了」的根因。
/// UI 层用三态表达意图,由 `merge_secret` 落回二态。
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum SecretField {
    /// 用户没碰这个框 → 已存值原样保留。
    Keep,
    /// 用户输入了新值 → 覆盖。
    Set(String),
    /// 用户把框清空了 → 删除已存值。
    Clear,
}

/// 手写打码 Debug:`Set` 里是明文口令,`{:?}` 一打就写进日志/panic 消息,
/// 加密存储当场归零(与 `mullion_store::model::SecretEntry` 同一条红线)。
impl std::fmt::Debug for SecretField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SecretField::Keep => f.write_str("Keep"),
            SecretField::Set(_) => f.write_str("Set(<已设置>)"),
            SecretField::Clear => f.write_str("Clear"),
        }
    }
}

/// 把三态意图落回 store 的二态 `Option<SecretEntry>`。纯函数。
///
/// 三个字段各自独立合成;全为 `None` 时整条收成 `None`,不在 secrets.enc 里
/// 留三字段全空的壳。
pub(crate) fn merge_secret(
    existing: Option<&SecretEntry>,
    password: &SecretField,
    passphrase: &SecretField,
    proxy_password: &SecretField,
) -> Option<SecretEntry> {
    fn one(existing: Option<&String>, f: &SecretField) -> Option<String> {
        match f {
            SecretField::Keep => existing.cloned(),
            SecretField::Set(v) => Some(v.clone()),
            SecretField::Clear => None,
        }
    }
    let merged = SecretEntry {
        password: one(existing.and_then(|e| e.password.as_ref()), password),
        passphrase: one(existing.and_then(|e| e.passphrase.as_ref()), passphrase),
        proxy_password: one(
            existing.and_then(|e| e.proxy_password.as_ref()),
            proxy_password,
        ),
    };
    if merged.password.is_none()
        && merged.passphrase.is_none()
        && merged.proxy_password.is_none()
    {
        None
    } else {
        Some(merged)
    }
}

/// 让 `AuthKind::PublicKey { has_passphrase }` 跟**合成后**的凭据一致。
///
/// 它不能跟表单当前内容走:编辑已有会话时口令框恒为空(store 不回吐明文),
/// 跟着表单走会把 has_passphrase 写成 false,下次连接时 russh 拿到加密私钥
/// 却不知道要口令。密码认证(`AuthKind::Password`)没有这个字段,原样跳过。
pub(crate) fn sync_has_passphrase(draft: &mut SessionDraft, merged: Option<&SecretEntry>) {
    if let AuthKind::PublicKey { has_passphrase, .. } = &mut draft.auth.kind {
        *has_passphrase = merged.is_some_and(|s| s.passphrase.is_some());
    }
}
```

- [ ] **Step 4: 跑到绿**

Run: `cargo test -p mullion-app buffer:: 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok.`,测试数 = 原 12 + 新 8 = 20。

- [ ] **Step 5: 红队自证(逐条做,做完撤销)**

```bash
# 注入 1:把 Keep 分支改回改前行为
#   buffer.rs 的 one(): SecretField::Keep => existing.cloned()  →  => None
cargo test -p mullion-app buffer:: 2>&1 | grep -E "test result|FAILED"
# Expected: keep_preserves_existing_password 与 clearing_password_keeps_other_secrets 变红
git checkout crates/mullion-app/src/ui/session_manager/buffer.rs

# 注入 2:让 sync_has_passphrase 早退
#   函数体首行加 `return;`
cargo test -p mullion-app has_passphrase_follows 2>&1 | grep -E "test result|FAILED"
# Expected: has_passphrase_follows_merged_secret_not_form 变红
git checkout crates/mullion-app/src/ui/session_manager/buffer.rs
```

两次注入都必须真的变红。若某条注入后仍然全绿,说明那条测试没扎在真实注入点上,
**改测试**(不是改注入),直到它能捕获。

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-app/src/ui/session_manager/buffer.rs
git commit -m "feat(app): 凭据三态 SecretField + merge_secret 纯函数 (F73)

store 的 Option<SecretEntry> 只有二态,表达不了「用户没动这个框」——
这是「编辑已有会话保存后密码被静默清除」的根因。三态在 app 层合成,
mullion-store 零改动。SecretField 手写打码 Debug,不泄漏明文。

8 条新测试,其中 keep_preserves_existing_password 与
has_passphrase_follows_merged_secret_not_form 已红队自证:
把 Keep 分支改回 None / 让 sync_has_passphrase 早退,均能变红。"
```

---

## Task 6: `EditorBuffer` 接上三态(`*_touched` + `PartialEq`)

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/buffer.rs`

- [ ] **Step 1: 给 `EditorBuffer` 加三个触碰位与 `PartialEq`**

把 `EditorBuffer` 的 derive 改成:

```rust
#[derive(Clone, PartialEq)]
pub struct EditorBuffer {
```

(21 个字段的类型 —— `String` / `u16` / `bool` / `Protocol` / `Option<GroupId>` /
`Vec<String>` / `TerminalPrefs` / `AppearancePrefs` —— 全都已经 `PartialEq`,
`mullion-store` 不用改。)

在三个密码字段各自后面加一个触碰位:

```rust
    pub password: String,
    /// 用户是否碰过密码框。未碰 = `SecretField::Keep`(已存值保留)。
    /// 编辑已有会话时密码框恒为空(store 不回吐明文),没有这一位就区分不了
    /// 「没动」和「清空了」——见 `SecretField` 的说明。
    pub password_touched: bool,
    // …中间的既有字段原样不动…
    pub passphrase: String,
    pub passphrase_touched: bool,
    // …中间的既有字段原样不动…
    pub proxy_password: String,
    pub proxy_password_touched: bool,
```

`impl Default` 里三个新字段走 `false`(用 `..Default::default()` 的地方无需改);
手写的 `impl Debug` 里把三个触碰位也列出来(它们是 bool,不涉密):

```rust
            .field("password_touched", &self.password_touched)
            .field("passphrase_touched", &self.passphrase_touched)
            .field("proxy_password_touched", &self.proxy_password_touched)
```

- [ ] **Step 2: 写 `secret_fields` 的测试**

追加到 `mod tests`:

```rust
    /// 未碰 → Keep;碰过且非空 → Set;碰过且空 → Clear。
    #[test]
    fn secret_fields_maps_touch_state_to_three_way_intent() {
        let mut b = buf();
        assert_eq!(secret_fields(&b).0, SecretField::Keep, "未碰应为 Keep");

        b.password_touched = true;
        b.password = "pw".into();
        assert_eq!(secret_fields(&b).0, SecretField::Set("pw".into()));

        b.password = String::new();
        assert_eq!(secret_fields(&b).0, SecretField::Clear, "碰过后清空应为 Clear");
    }

    /// 认证方式选了密码 → 私钥口令字段必须是 `Clear`(而不是 Keep),
    /// 否则会在 secrets.enc 里留下一条用不到的孤儿口令。这与改前
    /// `build_draft` 的行为一致(密码模式下 secret.passphrase 恒为 None)。
    #[test]
    fn inactive_auth_branch_is_cleared_not_kept() {
        let mut b = buf();
        b.auth_kind = AuthKindUi::Password;
        assert_eq!(secret_fields(&b).1, SecretField::Clear, "密码模式下口令应清除");

        b.auth_kind = AuthKindUi::PublicKey;
        assert_eq!(secret_fields(&b).0, SecretField::Clear, "公钥模式下密码应清除");
    }
```

- [ ] **Step 3: 跑到红**

Run: `cargo test -p mullion-app secret_fields 2>&1 | grep -E "^error|cannot find"`
Expected: `cannot find function \`secret_fields\``。

- [ ] **Step 4: 实现 `secret_fields`,并让 `build_draft` 走它**

追加到 `buffer.rs`:

```rust
/// 表单缓冲 → 三个密码框各自的三态意图。纯函数。
///
/// 当前认证方式用不到的那一支走 `Clear` 而不是 `Keep`:密码认证的会话不该在
/// secrets.enc 里留一条孤儿私钥口令(这也与改造前 `build_draft` 的行为一致)。
pub(crate) fn secret_fields(buf: &EditorBuffer) -> (SecretField, SecretField, SecretField) {
    fn field(touched: bool, v: &str) -> SecretField {
        if !touched {
            SecretField::Keep
        } else if v.is_empty() {
            SecretField::Clear
        } else {
            SecretField::Set(v.to_string())
        }
    }
    let (password, passphrase) = match buf.auth_kind {
        AuthKindUi::Password => (
            field(buf.password_touched, &buf.password),
            SecretField::Clear,
        ),
        AuthKindUi::PublicKey => (
            SecretField::Clear,
            field(buf.passphrase_touched, &buf.passphrase),
        ),
    };
    (
        password,
        passphrase,
        field(buf.proxy_password_touched, &buf.proxy_password),
    )
}
```

改 `build_draft`:把原来那段 `match (secret, proxy_password)` 的三分支合成整段**删掉**,
换成下面这两处。

secret 的计算改为:

```rust
    // 这里传 `existing = None`:`build_draft` 看不到 store,它产出的是「若库里
    // 原本没有凭据时的合成结果」。真正的合成在 `app::apply_save` 里用真实
    // existing 重算一遍并覆盖 —— 编辑已有会话时以那一次为准。
    let (pw_f, pp_f, proxy_f) = secret_fields(buf);
    let secret = merge_secret(None, &pw_f, &pp_f, &proxy_f);
```

`auth.kind` 的构造改为先填 `false`、再同步:

```rust
    let kind = match buf.auth_kind {
        AuthKindUi::Password => AuthKind::Password,
        AuthKindUi::PublicKey => AuthKind::PublicKey {
            path: PathBuf::from(buf.key_path.trim()),
            // 占位;下面用合成结果统一修正,避免两处各算一遍算歪。
            has_passphrase: false,
        },
    };
```

在函数末尾构造出 `SessionDraft` 之后、`Ok(draft)` 之前插入:

```rust
    sync_has_passphrase(&mut draft, secret.as_ref());
```

(`let draft = SessionDraft {...}` 相应改成 `let mut draft = ...`。)

- [ ] **Step 5: 给 12 个老测试补触碰位**

原来那 12 个 `build_draft` 测试里,**凡是给密码/口令赋了值的**,必须同步把对应的
`*_touched` 置 `true` —— 否则新逻辑判定为 `Keep`,在 `existing = None` 下合成出 `None`。
断言**一律不改**(改断言就是削弱测试)。例如:

```rust
    #[test]
    fn password_session_builds_draft_with_secret() {
        let mut b = buf();
        b.password = "pw".into();
        b.password_touched = true; // ← 新增这一行,模拟用户真的输入过
        let d = build_draft(&b).expect("build");
        assert_eq!(d.secret.unwrap().password.as_deref(), Some("pw")); // 断言原样
    }
```

Run: `cargo test -p mullion-app buffer:: 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: `test result: ok.`,测试数 = 20 + 2 = 22。**任何一条老断言都不该被修改**;
若某条只能靠改断言才能通过,停下来问,那说明合成逻辑真的改变了行为。

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-app/src/ui/session_manager/buffer.rs
git commit -m "feat(app): EditorBuffer 加触碰位,build_draft 走三态合成 (F73)

三个密码字段各加一个 *_touched 位,secret_fields() 把它们映射成 SecretField。
build_draft 传 existing=None 合成(真实合成在 apply_save),has_passphrase 统一
由 sync_has_passphrase 修正,不再两处各算一遍。

12 个老测试只补 *_touched=true(模拟用户输入过),断言一字未改。
EditorBuffer 加 PartialEq,为后续「未保存变更」脏检查做准备。"
```

---

## Task 7: 保存路径接上真实 `existing` —— `SessionStore::secret` + `apply_save`

当前 `app.rs:1388-1417` 直接 `store.update(id, save.draft, &now)`,`draft.secret` 是
`build_draft` 在 `existing = None` 下算的 —— 编辑已有会话就把凭据清了。这一步把真实
`existing` 接进来。同时 `store.add` 的返回值(新分配的 `SessionId`)现在被丢弃,
后面「保存并连接」需要它。

**Files:**
- Modify: `crates/mullion-app/src/shell/store.rs`(新增两个转发方法)
- Modify: `crates/mullion-app/src/ui/session_manager/buffer.rs`(`SaveIntent` 扩字段)
- Modify: `crates/mullion-app/src/app.rs:1388-1417`
- Test: `crates/mullion-app/src/app.rs` 的 `mod tests`(:1785)

- [ ] **Step 1: 给 `SessionStore` 加转发**

追加到 `crates/mullion-app/src/shell/store.rs` 的 `impl SessionStore`:

```rust
    /// 读一条会话的已存凭据。**返回明文**,只给保存路径的三态合成用
    /// (`app::apply_save`)——不要把它塞进 `UiFrame`,UI 层只该知道
    /// 「有没有设置」,那是 `secret_presence` 的职责。
    pub fn secret(&self, id: SessionId) -> Option<&mullion_store::model::SecretEntry> {
        self.vault.secret(id)
    }

    /// 只报告三个凭据槽位「有没有值」,不泄漏任何明文。UI 靠它决定密码框
    /// 显示「6 位黑点」还是「未设置」。
    pub fn secret_presence(&self, id: SessionId) -> SecretPresence {
        match self.vault.secret(id) {
            None => SecretPresence::default(),
            Some(s) => SecretPresence {
                password: s.password.is_some(),
                passphrase: s.passphrase.is_some(),
                proxy_password: s.proxy_password.is_some(),
            },
        }
    }
```

`SecretPresence` 定义放 `buffer.rs`(它要进 `UiFrame`,必须 `Copy`):

```rust
/// 三个凭据槽位「有没有值」。**只有 bool,不含任何明文** —— 它要穿过
/// `UiFrame` 进 egui 闭包,明文绝不能走这条路。
/// 必须 `Copy`:`UiFrame` 整体 `Copy`(`egui::Context::run` 内部是个 loop)。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SecretPresence {
    pub password: bool,
    pub passphrase: bool,
    pub proxy_password: bool,
}
```

`shell/store.rs` 顶部加 `use crate::ui::session_manager::SecretPresence;`,
`session_manager/mod.rs` 的再导出补上 `SecretPresence`:

```rust
pub use buffer::{EditorBuffer, SaveIntent, SecretPresence};
```

- [ ] **Step 2: `SaveIntent` 扩字段**

把 `buffer.rs` 里的 `SaveIntent` 改为:

```rust
/// 一次「保存」的意图:app 事后据此调用 `store.add`(`editing_id=None`)或
/// `store.update`(`Some(id)`)。
///
/// `draft.secret` 里装的是「库里原本没有凭据时」的合成结果,**不是最终值**——
/// `app::apply_save` 会用真实的已存凭据重算一遍并覆盖它(见 `merge_secret`)。
pub struct SaveIntent {
    pub editing_id: Option<SessionId>,
    pub draft: SessionDraft,
    pub password: SecretField,
    pub passphrase: SecretField,
    pub proxy_password: SecretField,
    /// 保存成功后立刻连接(右栏底部的「保存并连接」)。
    pub then_connect: bool,
}
```

`SecretField` 需要跨模块可见,把它的可见性从 `pub(crate)` 提到 `pub`
(它已手写打码 Debug,提可见性不增加泄漏面),并在 `mod.rs` 再导出:

```rust
pub use buffer::{EditorBuffer, SaveIntent, SecretField, SecretPresence};
```

- [ ] **Step 3: 写 `apply_save` 的失败测试**

追加到 `crates/mullion-app/src/app.rs` 的 `#[cfg(test)] mod tests`(:1785):

```rust
    use mullion_store::model::SecretEntry;
    use mullion_store::MasterKeySource;

    /// 测试用主密钥源:不碰 keyring(CI/无头环境没有钥匙串守护进程)。
    struct FixedKey;
    impl MasterKeySource for FixedKey {
        fn load_or_create(&self) -> Result<[u8; 32], mullion_store::StoreError> {
            Ok([7u8; 32])
        }
    }

    fn tmp_store() -> (tempfile::TempDir, crate::shell::store::SessionStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store =
            crate::shell::store::SessionStore::open(dir.path().to_path_buf(), &FixedKey)
                .expect("open store");
        (dir, store)
    }

    /// F73 端到端红线:**先存一个带密码的会话,再原样保存一次(密码框没碰过),
    /// 密码必须还在。** 这是用户实际会走的路径,也是改前会丢密码的那条。
    ///
    /// 自证会变红:把 `apply_save` 里的 `store.secret(id)` 换成 `None`
    /// (即改前「看不到已存凭据」的状态),这条报 None != Some("pw")。
    #[test]
    fn editing_a_session_without_touching_password_keeps_it() {
        let (_dir, mut store) = tmp_store();

        let mut first = crate::ui::session_manager::EditorBuffer {
            name: "dev".into(),
            host: "192.0.2.10".into(),
            user: "user".into(),
            ..Default::default()
        };
        first.password = "pw".into();
        first.password_touched = true;
        let id = apply_save(
            &mut store,
            crate::ui::session_manager::SaveIntent {
                editing_id: None,
                draft: crate::ui::session_manager::build_draft(&first).expect("build"),
                password: crate::ui::session_manager::SecretField::Set("pw".into()),
                passphrase: crate::ui::session_manager::SecretField::Clear,
                proxy_password: crate::ui::session_manager::SecretField::Keep,
                then_connect: false,
            },
            "2026-08-03T00:00:00Z",
        )
        .expect("首次保存应成功");
        assert_eq!(store.secret(id).and_then(|s| s.password.clone()).as_deref(), Some("pw"));

        // 第二次:只改备注,密码框一次都没碰过(触碰位全 false)
        let mut again = first.clone();
        again.note = "改了备注".into();
        again.password = String::new();
        again.password_touched = false;
        apply_save(
            &mut store,
            crate::ui::session_manager::SaveIntent {
                editing_id: Some(id),
                draft: crate::ui::session_manager::build_draft(&again).expect("build"),
                password: crate::ui::session_manager::SecretField::Keep,
                passphrase: crate::ui::session_manager::SecretField::Clear,
                proxy_password: crate::ui::session_manager::SecretField::Keep,
                then_connect: false,
            },
            "2026-08-03T00:01:00Z",
        )
        .expect("二次保存应成功");

        assert_eq!(
            store.secret(id).and_then(|s| s.password.clone()).as_deref(),
            Some("pw"),
            "没碰密码框就保存,已存密码必须原样留着(F73)"
        );
    }

    /// 新建路径必须把 store 分配的 `SessionId` 交回去 ——「保存并连接」要用它。
    /// 改前 `app.rs` 是 `None => { store.add(draft, &now); store.save() }`,
    /// 返回值直接丢弃。
    /// 自证会变红:把 `apply_save` 新建分支改成 `Ok(SessionId(0))`,
    /// 这条报「新 id 应能在 store 里查到」。
    #[test]
    fn apply_save_new_returns_id_allocated_by_store() {
        let (_dir, mut store) = tmp_store();
        let buf = crate::ui::session_manager::EditorBuffer {
            name: "dev".into(),
            host: "192.0.2.10".into(),
            user: "user".into(),
            ..Default::default()
        };
        let id = apply_save(
            &mut store,
            crate::ui::session_manager::SaveIntent {
                editing_id: None,
                draft: crate::ui::session_manager::build_draft(&buf).expect("build"),
                password: crate::ui::session_manager::SecretField::Clear,
                passphrase: crate::ui::session_manager::SecretField::Clear,
                proxy_password: crate::ui::session_manager::SecretField::Clear,
                then_connect: false,
            },
            "2026-08-03T00:00:00Z",
        )
        .expect("保存应成功");
        assert!(
            store.list().iter().any(|r| r.id == id),
            "apply_save 返回的 id 必须是 store 真正分配的那个"
        );
    }
```

> `MasterKeySource` 的实际 trait 方法签名以 `mullion-store` 当前源码为准
> (`grep -n "trait MasterKeySource" -A 5 crates/mullion-store/src/*.rs`),
> 若与上面不一致,按实际签名写 `FixedKey`,**不要**改 store。

- [ ] **Step 4: 跑到红**

Run: `cargo test -p mullion-app apply_save 2>&1 | grep -E "^error|cannot find"`
Expected: `cannot find function \`apply_save\``。

- [ ] **Step 5: 实现 `apply_save`**

在 `crates/mullion-app/src/app.rs` 里(靠近事件循环外、`impl` 之外)加一个自由函数:

```rust
/// 施加一次保存意图。抽成纯函数是为了能在没有窗口的情况下测「编辑已有会话
/// 不会把凭据清掉」(F73)——这条路径以前埋在事件循环里,只能靠上机手点。
///
/// 返回被写入的会话 id:新建时是 store 分配的那个,「保存并连接」要用。
fn apply_save(
    store: &mut SessionStore,
    save: SaveIntent,
    now: &str,
) -> Result<SessionId, String> {
    let SaveIntent {
        editing_id,
        mut draft,
        password,
        passphrase,
        proxy_password,
        then_connect: _,
    } = save;

    // 先把已存凭据 clone 出来,释放对 store 的不可变借用,下面才能 &mut。
    let existing = editing_id.and_then(|id| store.secret(id)).cloned();
    let merged = merge_secret(existing.as_ref(), &password, &passphrase, &proxy_password);
    sync_has_passphrase(&mut draft, merged.as_ref());
    draft.secret = merged;

    match editing_id {
        Some(id) => {
            store
                .update(id, draft, now)
                .map_err(|e| format!("保存失败:{e}"))?;
            store.save().map_err(|e| format!("保存失败:{e}"))?;
            Ok(id)
        }
        None => {
            let id = store.add(draft, now);
            store.save().map_err(|e| format!("保存失败:{e}"))?;
            Ok(id)
        }
    }
}
```

`merge_secret` / `sync_has_passphrase` 需要跨模块可见 —— 把它们的可见性从
`pub(crate)` 保持不变即可(`crate` 内可见),在 `session_manager/mod.rs` 补:

```rust
pub(crate) use buffer::{build_draft, merge_secret, secret_fields, sync_has_passphrase, AuthKindUi, ProxyModeUi};
```

- [ ] **Step 6: 把 `app.rs:1388-1417` 的施加点改成调用它**

```rust
        if let Some(save) = self.ui.save_request.take() {
            if let Some(store) = self.store.as_mut() {
                let now = time::OffsetDateTime::now_utc()
                    .format(&time::format_description::well_known::Rfc3339)
                    .unwrap_or_default();
                let then_connect = save.then_connect;
                match apply_save(store, save, &now) {
                    Ok(id) => {
                        if then_connect {
                            self.ui.connect_request = Some(id);
                        }
                    }
                    Err(msg) => self.ui.set_error(msg),
                }
            }
        }
```

(`set_error` 在 Task 8 才引入 —— 本步先写 `self.ui.last_error = Some(msg);`,
Task 8 统一改。)

- [ ] **Step 7: 跑到绿并红队自证**

Run: `cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: `test result: ok.`

```bash
# 注入:让 apply_save 看不到已存凭据(改前的状态)
#   `let existing = editing_id.and_then(|id| store.secret(id)).cloned();`
#   → `let existing: Option<SecretEntry> = None;`
cargo test -p mullion-app editing_a_session_without 2>&1 | grep -E "test result|FAILED"
# Expected: 变红
git checkout crates/mullion-app/src/app.rs
```

- [ ] **Step 8: 提交**

```bash
git add crates/mullion-app/src/app.rs crates/mullion-app/src/shell/store.rs \
        crates/mullion-app/src/ui/session_manager
git commit -m "fix(app): 保存时用真实已存凭据合成,不再静默清除密码 (F73)

SessionStore 新增 secret()/secret_presence() 转发;保存路径从事件循环里抽成
纯函数 apply_save,能脱离 GUI 测。新建分支现在把 store 分配的 SessionId 交回去
(改前直接丢弃),「保存并连接」要用。

红队自证:把 apply_save 里的 store.secret(id) 换成 None(改前状态),
editing_a_session_without_touching_password_keeps_it 变红。"
```

---

## Task 8: 错误写入收口到 `UiState::set_error`

右栏要画错误卡片,还要能被用户关掉。关掉之后**下一个新错误必须重新弹出来** ——
如果各处继续直接写 `last_error`,新错误来了 `error_dismissed` 还是 `true`,卡片
永远不再出现。把写入收成一个方法是唯一可靠的做法。

**Files:**
- Modify: `crates/mullion-app/src/ui/mod.rs:46-103`(`UiState`)
- Modify: `crates/mullion-app/src/app.rs`(10 处 `last_error = Some(...)`)

- [ ] **Step 1: 写失败测试**

追加到 `crates/mullion-app/src/ui/mod.rs` 的 `mod tests`:

```rust
    /// 用户关掉错误卡片后,**下一个**错误必须重新弹出来。
    /// 自证会变红:删掉 `set_error` 里的 `self.error_dismissed = false;`
    /// 这一行(这正是漏写时会发生的事),断言 2 立刻红。
    #[test]
    fn set_error_reopens_the_card_after_the_user_dismissed_the_previous_one() {
        let mut st = UiState::default();
        st.set_error("第一个错误".into());
        assert!(!st.error_dismissed);

        st.error_dismissed = true; // 用户点了 ×

        st.set_error("第二个错误".into());
        assert!(
            !st.error_dismissed,
            "新错误必须重新展开卡片,否则用户再也看不到任何错误"
        );
        assert_eq!(st.last_error.as_deref(), Some("第二个错误"));
    }
```

- [ ] **Step 2: 跑到红**

Run: `cargo test -p mullion-app set_error_reopens 2>&1 | grep -E "^error|cannot find"`
Expected: `no method named \`set_error\``、`no field \`error_dismissed\``。

- [ ] **Step 3: 实现**

`UiState` 加字段:

```rust
    /// 用户是否关掉了当前这条错误卡片。**只该由 `set_error` 复位** ——
    /// 各处直接写 `last_error` 会绕过复位,导致关掉一次后再也看不到错误。
    pub error_dismissed: bool,
```

加方法(放在 `impl UiState` 里;若当前没有 `impl UiState`,新建一个):

```rust
impl UiState {
    /// 报告一条错误。**所有**错误写入都必须走这里,不要直接赋值 `last_error`。
    pub fn set_error(&mut self, msg: String) {
        self.last_error = Some(msg);
        self.error_dismissed = false;
    }
}
```

- [ ] **Step 4: 把 10 处直接赋值改成调用**

Run: `grep -rn "last_error = Some" crates/mullion-app/src`
Expected: 10 处(`app.rs` 的 :714 / :720 / :854 / :887 / :1486 附近,以及
Task 7 新写的那处,和其余若干)。逐一改成 `self.ui.set_error(...)`:

```rust
// 改前
self.ui.last_error = Some(format!("会话库打开失败:{e}"));
// 改后
self.ui.set_error(format!("会话库打开失败:{e}"));
```

改完再 grep:

Run: `grep -rn "last_error = Some" crates/mullion-app/src`
Expected: 只剩 `ui/mod.rs` 里 `set_error` 自己那一行。

> `chrome.rs` 的状态栏读 `last_error` 展示,**保持不变** —— 状态栏是常驻摘要,
> 不受 `error_dismissed` 影响;卡片才是可关闭的那个。

- [ ] **Step 5: 跑到绿并红队自证**

Run: `cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: `test result: ok.`

```bash
# 注入:删掉 set_error 里的 self.error_dismissed = false;
cargo test -p mullion-app set_error_reopens 2>&1 | grep -E "test result|FAILED"
# Expected: 变红
git checkout crates/mullion-app/src/ui/mod.rs
```

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-app/src
git commit -m "refactor(app): 错误写入收口到 UiState::set_error (F90)

新增 error_dismissed 位供右栏错误卡片用,10 处直接赋值 last_error 改成
调用 set_error 统一复位。绕过它写入会导致「关掉一次卡片后再也看不到错误」。

红队自证:删掉 set_error 里的复位行,
ui::tests::set_error_reopens_the_card_after_the_user_dismissed_the_previous_one 变红。"
```

---

## Task 9: 脏检查 `is_dirty` + 切换目标 `SwitchTarget` + `UiFrame` 扩字段

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/buffer.rs`
- Modify: `crates/mullion-app/src/ui/mod.rs`(`UiState` / `UiFrame`)
- Modify: `crates/mullion-app/src/app.rs`(填 `UiFrame` 的两个新字段)

- [ ] **Step 1: 写失败测试**

追加到 `buffer.rs` 的 `mod tests`:

```rust
    /// 脏检查用「与基线快照逐字段比对」,不是「有没有按过键」——用户改完又
    /// 改回来不该算脏,否则每次切会话都弹一次「有未保存的更改」,弹到用户
    /// 条件反射点「丢弃」,这个确认就废了。
    #[test]
    fn is_dirty_compares_against_baseline_not_keystrokes() {
        let baseline = buf();
        let mut edited = baseline.clone();
        assert!(!is_dirty(&edited, &baseline), "没改动不算脏");

        edited.note = "改了".into();
        assert!(is_dirty(&edited, &baseline), "改了字段算脏");

        edited.note = baseline.note.clone();
        assert!(!is_dirty(&edited, &baseline), "改回来不算脏");
    }

    /// 触碰位本身也参与比对:用户点进密码框又清空(= 意图「清除凭据」),
    /// 文本内容和基线一样都是空的,但意图变了,必须算脏。
    /// 自证会变红:把 `is_dirty` 改成只比 `buf.name != baseline.name` 之类的
    /// 子集比对,这条报「应算脏」。
    #[test]
    fn clearing_a_password_counts_as_dirty_even_though_the_text_is_still_empty() {
        let baseline = buf();
        let mut edited = baseline.clone();
        edited.password_touched = true; // 点进去过,框里仍是空 → 意图清除
        assert!(
            is_dirty(&edited, &baseline),
            "清除凭据的意图必须算脏,否则切走时静默丢弃"
        );
    }
```

- [ ] **Step 2: 跑到红**

Run: `cargo test -p mullion-app is_dirty 2>&1 | grep -E "^error|cannot find"`
Expected: `cannot find function \`is_dirty\``。

- [ ] **Step 3: 实现**

追加到 `buffer.rs`:

```rust
/// 表单是否相对基线快照有改动。
///
/// 基线 = 打开这条会话时 `EditorBuffer::from_record` 的产物。整体比对而不是
/// 「按过键就算脏」:用户改完又改回来不该弹确认,弹多了用户就条件反射点
/// 「丢弃」,这个确认也就白设了。三个 `*_touched` 位一起参与比对 ——
/// 「点进密码框再清空」文本上看不出差别,意图上却是「清除凭据」。
pub(crate) fn is_dirty(buf: &EditorBuffer, baseline: &EditorBuffer) -> bool {
    buf != baseline
}
```

- [ ] **Step 4: 加 `SwitchTarget` 与 `UiState` 字段**

`buffer.rs`:

```rust
/// 左栏点击想切到哪里。切换前若表单是脏的,先弹确认,确认后再消费它。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchTarget {
    Session(SessionId),
    /// 「新建」按钮:切到一张空白草稿。
    NewDraft,
}
```

`ui/mod.rs` 的 `UiState` 加:

```rust
    /// 编辑区当前内容的基线快照,用于脏检查(见 `session_manager::is_dirty`)。
    /// 与 `editor` 同时设置、同时清空。
    pub editor_baseline: Option<EditorBuffer>,
    /// 待切换目标。表单脏时先弹确认,用户选「丢弃」才消费它。
    pub pending_switch: Option<SwitchTarget>,
    /// 左栏搜索框内容。
    pub search: String,
    /// 右栏当前 Tab(0=基础 1=认证 2=网络)。
    pub editor_tab: usize,
```

- [ ] **Step 5: `UiFrame` 加两个 `Copy` 字段**

`ui/mod.rs` 的 `UiFrame` 加:

```rust
    /// 当前被编辑会话的三个凭据槽位「有没有值」。**只有 bool,无明文**。
    pub secret_presence: SecretPresence,
    /// 当前已连接的会话(状态点用)。`UserEvent::ConnectOk` 不带 SessionId,
    /// 所以这里追踪的是「最后一次成功连上的那条」,不是全量连接集合。
    pub connected_session: Option<SessionId>,
```

> `UiFrame` 必须整体 `Copy`(`egui::Context::run` 内部是 loop,要能反复传值)。
> `SecretPresence` 已 derive `Copy`,`Option<SessionId>` 也是 `Copy`,不破坏约束。
> 编译报 `Copy` 不满足就是加错了类型,回头看这条。

`base_frame()`(`ui/mod.rs:223-234`)补默认值:

```rust
        secret_presence: SecretPresence::default(),
        connected_session: None,
```

`app.rs` 里构造 `UiFrame` 的地方补:

```rust
            secret_presence: match (self.store.as_ref(), self.ui.editor_id) {
                (Some(s), Some(id)) => s.secret_presence(id),
                _ => SecretPresence::default(),
            },
            connected_session: self.connected_session,
```

`App` 结构体加字段 `connected_session: Option<SessionId>`(`Default` 为 `None`),
在 `UserEvent::ConnectOk` 分支(`app.rs:750` 起)里,紧挨着
`self.ui.session_manager_open = false;` 处补:

```rust
                // ConnectOk 不带 SessionId(见 UserEvent 定义),用发起连接时
                // 记下的那条。状态点只区分「这条连上了 / 没连上」两态。
                self.connected_session = self.ui.connect_request_last;
```

`self.ui.connect_request_last: Option<SessionId>` 在 `connect_request` 被消费时
(`app.rs:1451` 附近)记下:

```rust
        if let Some(id) = self.ui.connect_request.take() {
            self.ui.connect_request_last = Some(id);
```

- [ ] **Step 6: 跑到绿**

Run: `cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: `test result: ok.`

Run: `cargo clippy -p mullion-app --all-targets -- -D warnings`
Expected: 无输出。

- [ ] **Step 7: 提交**

```bash
git add crates/mullion-app/src
git commit -m "feat(app): 脏检查 is_dirty + SwitchTarget + UiFrame 扩两个 Copy 字段 (F90)

is_dirty 用基线快照整体比对(含三个触碰位),改回来不算脏。UiFrame 新增
secret_presence(只有 bool,无明文)与 connected_session,两者都是 Copy,
不破坏 UiFrame 整体 Copy 的约束。"
```

---

## Task 10: 左栏 `list.rs` 重做

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/list.rs`(整块重写)
- Test: 同文件的 `mod tests`

- [ ] **Step 1: 写搜索过滤的失败测试**

追加到 `list.rs` 末尾:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mullion_store::model::{Auth, AuthKind, Connection, Identity, Protocol, SessionRecord};
    use mullion_store::SessionId;

    fn rec(id: u64, name: &str, host: &str, tags: &[&str]) -> SessionRecord {
        SessionRecord {
            id: SessionId(id),
            modified_at: "2026-08-03T00:00:00Z".into(),
            identity: Identity {
                name: name.into(),
                note: String::new(),
                group_id: None,
                tags: tags.iter().map(|s| s.to_string()).collect(),
            },
            connection: Connection {
                host: host.into(),
                port: 22,
                protocol: Protocol::Ssh,
            },
            auth: Auth {
                user: "user".into(),
                kind: AuthKind::Password,
            },
            terminal: Default::default(),
            appearance: Default::default(),
            network: Default::default(),
        }
    }

    /// 搜索要覆盖名称 / 主机 / 标签三处,且大小写不敏感 —— 用户记得住的往往是
    /// IP 尾数或标签,不是当初起的名字。
    #[test]
    fn search_matches_name_host_and_tags_case_insensitively() {
        let r = rec(1, "Prod-DB", "192.0.2.10", &["生产", "MySQL"]);
        assert!(matches(&r, ""), "空查询放行全部");
        assert!(matches(&r, "  "), "只有空白的查询等同空查询");
        assert!(matches(&r, "prod"), "名称匹配应大小写不敏感");
        assert!(matches(&r, "2.10"), "主机子串应匹配");
        assert!(matches(&r, "mysql"), "标签匹配应大小写不敏感");
        assert!(!matches(&r, "staging"), "无关词不该匹配");
    }
}
```

- [ ] **Step 2: 跑到红**

Run: `cargo test -p mullion-app search_matches 2>&1 | grep -E "^error|cannot find"`
Expected: `cannot find function \`matches\``。

- [ ] **Step 3: 实现左栏**

把 `list.rs` 的 `show` 整块替换为:

```rust
/// 一行会话的高度(设计稿 §4.1:两行文字 + 上下 8px)。
const ROW_H: f32 = 44.0;

/// 会话是否命中搜索。空查询放行全部。名称 / 主机 / 标签三处都查,
/// 大小写不敏感 —— 用户记得住的常是 IP 尾数或标签,不是当初起的名字。
pub(crate) fn matches(rec: &SessionRecord, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    rec.identity.name.to_lowercase().contains(&q)
        || rec.connection.host.to_lowercase().contains(&q)
        || rec
            .identity
            .tags
            .iter()
            .any(|t| t.to_lowercase().contains(&q))
}

/// 手绘一行会话。不用 `selectable_label`:设计稿要「状态点 + 名称 + user@host
/// 两行 + 选中态左侧强调条」,`selectable_label` 只画得出单行文本。
fn session_row(
    ui: &mut Ui,
    t: &Theme,
    rec: &SessionRecord,
    selected: bool,
    connected: bool,
) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), egui::Sense::click());
    let p = ui.painter();

    let bg = if selected {
        theme::c32(t.sunken_bg)
    } else if resp.hovered() {
        theme::c32(t.panel_head)
    } else {
        egui::Color32::TRANSPARENT
    };
    p.rect_filled(rect, egui::Rounding::same(6.0), bg);
    if selected {
        p.rect_filled(
            egui::Rect::from_min_size(rect.min, egui::vec2(3.0, ROW_H)),
            egui::Rounding::same(2.0),
            theme::c32(t.accent),
        );
    }

    // §6:状态点只有两态 —— 已连接(ok 绿)/ 未连接(fg_ghost 灰)。
    // 「连接中」态做不出来:UserEvent::ConnectOk/ConnectErr 都不带 SessionId,
    // 无法把在途连接归到某一行上。
    p.circle_filled(
        egui::pos2(rect.left() + 16.0, rect.center().y),
        4.0,
        if connected {
            theme::c32(t.ok)
        } else {
            theme::c32(t.fg_ghost)
        },
    );
    p.text(
        egui::pos2(rect.left() + 30.0, rect.top() + 7.0),
        egui::Align2::LEFT_TOP,
        &rec.identity.name,
        egui::FontId::proportional(14.0),
        theme::c32(t.fg),
    );
    p.text(
        egui::pos2(rect.left() + 30.0, rect.top() + 25.0),
        egui::Align2::LEFT_TOP,
        format!("{}@{}", rec.auth.user, rec.connection.host),
        egui::FontId::proportional(11.0),
        theme::c32(t.fg_faint),
    );
    resp
}

pub(super) fn show(
    ui: &mut Ui,
    t: &Theme,
    ui_state: &mut UiState,
    sessions: &[SessionRecord],
    groups: &[GroupRecord],
    connected: Option<SessionId>,
) {
    // 搜索框
    ui.add(
        egui::TextEdit::singleline(&mut ui_state.search)
            .hint_text("搜索名称 / 主机 / 标签")
            .desired_width(f32::INFINITY),
    );
    ui.add_space(8.0);

    // ⚠️ 这段写法已在实机上被证伪,**不要照抄**。见下方注记与 Task 11 的正确写法。
    let bottom = 40.0;
    let list_h = (ui.available_height() - bottom).max(0.0);

    egui::ScrollArea::vertical()
        .max_height(list_h)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let filtered: Vec<&SessionRecord> = sessions
                .iter()
                .filter(|r| matches(r, &ui_state.search))
                .collect();

            for g in groups {
                let members: Vec<&&SessionRecord> = filtered
                    .iter()
                    .filter(|r| r.identity.group_id == Some(g.id))
                    .collect();
                if members.is_empty() && !ui_state.search.trim().is_empty() {
                    continue; // 搜索时不显示空分组
                }
                // 搜索期间强制展开:`default_open` 只在 CollapsingState 首次
                // 加载时生效,用户手动折叠过就被持久化进 ctx.data(),再也展不开。
                let force = if ui_state.search.trim().is_empty() {
                    None
                } else {
                    Some(true)
                };
                egui::CollapsingHeader::new(format!("{} ({})", g.name, members.len()))
                    .id_salt(g.id)
                    .default_open(true)
                    .open(force)
                    .show(ui, |ui| {
                        for r in &members {
                            row(ui, t, ui_state, r, connected);
                        }
                    });
            }

            let ungrouped: Vec<&&SessionRecord> = filtered
                .iter()
                .filter(|r| r.identity.group_id.is_none())
                .collect();
            if !ungrouped.is_empty() {
                egui::CollapsingHeader::new(format!("未分组 ({})", ungrouped.len()))
                    .id_salt("ungrouped")
                    .default_open(true)
                    .show(ui, |ui| {
                        for r in &ungrouped {
                            row(ui, t, ui_state, r, connected);
                        }
                    });
            }
        });

    ui.separator();
    ui.horizontal(|ui| {
        if ui.button("+ 新建").clicked() {
            ui_state.pending_switch = Some(SwitchTarget::NewDraft);
        }
    });
}

/// 画一行 + 挂交互(单击选中 / 双击连接 / 右键删除确认)。
fn row(
    ui: &mut Ui,
    t: &Theme,
    ui_state: &mut UiState,
    rec: &SessionRecord,
    connected: Option<SessionId>,
) {
    let selected = ui_state.editor_id == Some(rec.id);
    let resp = session_row(ui, t, rec, selected, connected == Some(rec.id));
    if resp.clicked() {
        ui_state.pending_switch = Some(SwitchTarget::Session(rec.id));
    }
    if resp.double_clicked() {
        ui_state.connect_request = Some(rec.id);
    }
    resp.context_menu(|ui| {
        if ui.button("删除").clicked() {
            ui_state.pending_delete = Some(rec.id);
            ui.close_menu();
        }
    });

    // §4.3:删除确认内联展开在被删那一行下面,不再弹第三个窗口。
    if ui_state.pending_delete == Some(rec.id) {
        egui::Frame::none()
            .fill(theme::c32(t.sunken_bg))
            .inner_margin(8.0)
            .rounding(6.0)
            .show(ui, |ui| {
                ui.colored_label(theme::c32(t.danger_soft), format!("删除「{}」?", rec.identity.name));
                ui.horizontal(|ui| {
                    if ui.button("删除").clicked() {
                        ui_state.delete_request = Some(rec.id);
                        ui_state.pending_delete = None;
                    }
                    if ui.button("取消").clicked() {
                        ui_state.pending_delete = None;
                    }
                });
            });
    }
}
```

`list.rs` 顶部 `use` 补 `use mullion_store::SessionId;`、
`use crate::ui::session_manager::SwitchTarget;`。
`mod.rs` 调用处补第六个参数:`list::show(ui, t, ui_state, sessions, groups, frame.connected_session)`
—— 相应地 `mod.rs::show` 的签名要多收一个 `connected: Option<SessionId>`,由
`build_ui` 传 `frame.connected_session`。

- [ ] **Step 4: 跑到绿**

Run: `cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: `test result: ok.`

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/ui/session_manager
git commit -m "feat(app): 会话管理器左栏重做 —— 搜索/手绘行/内联删除确认 (F90)

session_row 手绘(状态点 + 名称 + user@host + 选中态强调条),selectable_label
画不出两行。删除确认从独立 Window 改成内联展开在被删那行下面。搜索期间用
.open(Some(true)) 强制展开分组 —— default_open 只在 CollapsingState 首次加载
时生效,用户手动折叠过就再也展不开。

守护测试:list::tests::search_matches_name_host_and_tags_case_insensitively"
```

---

## Task 11: 右栏 `editor.rs` 骨架 —— 标题条 / 三 Tab / 错误卡片 / 底部按钮

**Files:**
- Modify: `crates/mullion-app/src/ui/mod.rs`(`editor` 字段改 `Option`;`show` 调用点传 presence)
- Modify: `crates/mullion-app/src/app.rs:868`(私钥回填适配 `Option`)
- Modify: `crates/mullion-app/src/ui/session_manager/mod.rs`(`show` 加 presence 参数)
- Modify: `crates/mullion-app/src/ui/session_manager/editor.rs`

- [ ] **Step 0: 把 `UiState::editor` 改成 `Option<EditorBuffer>`,并把 presence 接到右栏**

> **这一步是计划的补漏,写计划时漏掉了。** Task 11 往后(11/12/13/14)全部按
> `ui_state.editor.as_mut()` / `= Some(...)` / `= None` 写,但 Task 1–10 没有
> 任何一步改过这个字段的类型 —— 它至今仍是非 `Option` 的
> `pub editor: session_manager::EditorBuffer`。不先改,Step 1 的代码一行都编译不过。
>
> 为什么必须是 `Option`:右栏要区分「新建中(空表单)」和「什么都没选(空态提示)」。
> 两者的 `editor_id` 都是 `None`,非 `Option` 的 `editor` 表达不了这个区别。

`ui/mod.rs`:

```rust
    /// 编辑表单的跨帧字段缓冲。`None` = 右栏未在编辑任何会话(画空态提示)。
    pub editor: Option<session_manager::EditorBuffer>,
```

`UiState` 是 `#[derive(Default)]`,`Option` 的默认值就是 `None`,无需另改初值;
`close_session_manager` 目前也不碰 `editor`,同样不用动。

`app.rs:868` 的私钥路径回填(现在是 `self.ui.editor.key_path = ...`):

```rust
                    if let Some(buf) = self.ui.editor.as_mut() {
                        buf.key_path = p.display().to_string();
                    }
```

`session_manager::show` 加一个参数(右栏画密码占位要知道库里有没有值):

```rust
pub fn show(
    ctx: &egui::Context,
    t: &Theme,
    ui_state: &mut UiState,
    sessions: &[SessionRecord],
    groups: &[GroupRecord],
    store_available: bool,
    connected: Option<SessionId>,
    presence: SecretPresence,
) {
```

`ui/mod.rs` 的调用点(约 220 行)补最后一个实参 `frame.secret_presence,`。
`CentralPanel` 里的调用改成
`editor::show(ui, t, ui_state, groups, presence)` —— 去掉 `sessions`:
Task 12 把跳板链退化成「勾选 + 只读跳数」,右栏不再需要会话全表。

> **已知中间态,不是回归**:`editor` 改成 `Option` 后,在 Task 14 接上
> 「消费 `pending_switch` → 载入 `editor`」之前,右栏会一直显示空态提示,
> 点左栏的行和「+ 新建」都不会打开表单。这不是本步引入的缺陷 —— Task 10
> 之后 `list.rs` 就只写 `pending_switch` 而没人消费,原先那个非 `Option`
> 的空表单本来也载不进任何已有会话。Task 14 接上即恢复。**这几步之间不发版。**

- [ ] **Step 1: 写右栏骨架**

把 `editor.rs` 的 `show` 替换为:

```rust
/// 三个 Tab 的标题。索引即 `UiState::editor_tab`。
const TABS: [&str; 3] = ["基础", "认证", "网络"];

pub(super) fn show(
    ui: &mut Ui,
    t: &Theme,
    ui_state: &mut UiState,
    groups: &[GroupRecord],
    presence: SecretPresence,
) {
    // 没选中任何会话 → 空态提示,不画一张什么都填不进去的空表单。
    let Some(buf) = ui_state.editor.as_mut() else {
        ui.centered_and_justified(|ui| {
            ui.colored_label(theme::c32(t.fg_faint), "从左侧选一条会话,或点「+ 新建」");
        });
        return;
    };

    // 标题条
    ui.horizontal(|ui| {
        let title = if buf.name.trim().is_empty() {
            "新建会话".to_string()
        } else {
            buf.name.clone()
        };
        ui.label(egui::RichText::new(title).size(16.0).color(theme::c32(t.fg)));
    });
    ui.add_space(6.0);

    // §5.2 错误卡片:比状态栏那行显眼,且可关闭。关闭后下一个新错误会由
    // `UiState::set_error` 重新展开(它复位 error_dismissed)。
    if let (Some(msg), false) = (ui_state.last_error.clone(), ui_state.error_dismissed) {
        egui::Frame::none()
            .fill(theme::c32(t.sunken_bg))
            .stroke(egui::Stroke::new(1.0, theme::c32(t.danger_soft)))
            .rounding(8.0)
            .inner_margin(10.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(theme::c32(t.danger_soft), msg);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("×").clicked() {
                            ui_state.error_dismissed = true;
                        }
                    });
                });
            });
        ui.add_space(8.0);
    }

    // Tab 条
    ui.horizontal(|ui| {
        for (i, name) in TABS.iter().enumerate() {
            if ui
                .selectable_label(ui_state.editor_tab == i, *name)
                .clicked()
            {
                ui_state.editor_tab = i;
            }
        }
    });
    ui.separator();

    // 底部按钮条用 TopBottomPanel 先占位,Tab 内容吃剩余高度。
    //
    // **不要写成 `let bottom = 44.0; let body_h = ui.available_height() - bottom;`**
    // 再喂给 `ScrollArea::max_height` —— 左栏原本就是这么写的,在 Windows 11
    // 实机上把「+ 新建」按钮顶出了可见区(见 c4eb7f1)。两个原因:
    // `ui.available_height()` 在 panel 内返回的是 `Window` 的**布局高度**而非
    // 真实可见高度;硬编码的 44.0 必须与底栏实际渲染高度保持同步,一旦界面缩放
    // 或字号变大就失同步,且没有任何编译错误或测试会提示。
    // panel 布局天然保证「panel 先分配、中央区吃剩余」,不需要猜数字。
    //
    // 「取消」只置意图,不在这里改 `ui_state.editor` —— 见代码块后的借用说明。
    let mut cancel = false;
    egui::TopBottomPanel::bottom(ui.id().with("sm_editor_bottom"))
        .frame(egui::Frame::none())
        .show_separator_line(false)
        .show_inside(ui, |ui| {
            ui.separator();
            ui.horizontal(|ui| {
                let save = ui.button("保存").clicked();
                let save_connect = ui.button("保存并连接").clicked();
                if save || save_connect {
                    ui_state.save_click = Some(save_connect);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    cancel |= ui.button("取消").clicked();
                });
            });
        });

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| match ui_state.editor_tab {
            0 => super::fields::basic(ui, t, buf, groups),
            1 => super::fields::auth(ui, t, buf, presence),
            _ => super::fields::network(ui, t, buf, presence),
        });

    // `buf` 的借用到此结束,现在才能动 `ui_state.editor`。
    if cancel {
        ui_state.editor = None;
        ui_state.editor_baseline = None;
        ui_state.editor_id = None;
    }
}
```

> **注意这里有个借用冲突,是把底栏挪到前面才出现的。** `buf` 是函数开头从
> `ui_state.editor.as_mut()` 解出来的 `&mut`。底栏里 `ui_state.save_click = ...`
> 没问题(Rust 2021 的闭包按字段捕获,`save_click` 与 `editor` 是不相交的字段),
> 但「取消」的 `ui_state.editor = None` 借的正是 `buf` 借着的那个字段,而此时
> `buf` 后面还要给 `ScrollArea` 用 —— NLL 救不了,编译期直接报错。
> (原先的顺序里底栏在 `ScrollArea` **之后**,`buf` 已经死了,所以没暴露。)
>
> 上面代码块用的就是本项目一贯的「写意图、事后施加」:底栏只置局部
> `cancel` 标志,函数末尾 `buf` 借用结束后再执行清空。**别用 `clone()` 或
> 提前 `drop(buf)` 绕。**

`editor.rs` 顶部 `use` 补:`use crate::theme::{self, Theme};`、
`use crate::ui::session_manager::SecretPresence;`。

`UiState` 加一个字段(点击意图,借用释放后由 `mod.rs` 转成 `save_request`):

```rust
    /// 右栏「保存」被点了。`Some(true)` = 「保存并连接」。
    /// 中转一层是因为 `build_draft` 要读整个 `EditorBuffer`,而这里正持着
    /// 它的 `&mut`。
    pub save_click: Option<bool>,
```

- [ ] **Step 2: 在 `mod.rs` 里消费 `save_click`**

`mod.rs::show` 的 `Window::show` 闭包**之后**(借用已释放)加:

```rust
    if let Some(then_connect) = ui_state.save_click.take() {
        if let Some(buf) = ui_state.editor.as_ref() {
            match build_draft(buf) {
                Ok(draft) => {
                    let (password, passphrase, proxy_password) = secret_fields(buf);
                    ui_state.save_request = Some(SaveIntent {
                        editing_id: ui_state.editor_id,
                        draft,
                        password,
                        passphrase,
                        proxy_password,
                        then_connect,
                    });
                    // 保存成功后基线要跟上,否则刚存完就被判成脏。
                    ui_state.editor_baseline = ui_state.editor.clone();
                }
                Err(msg) => ui_state.set_error(msg),
            }
        }
    }
```

- [ ] **Step 3: 建 `fields.rs` 占位,让它编译**

新建 `crates/mullion-app/src/ui/session_manager/fields.rs`:

```rust
//! 右栏三个 Tab 的字段布局。从 `editor.rs` 切出来是因为字段多、改动频繁,
//! 混在窗口骨架里会让 `editor.rs` 涨到读不动。

use egui::Ui;
use mullion_store::GroupRecord;

use crate::theme::Theme;
use crate::ui::session_manager::{EditorBuffer, SecretPresence};

pub(super) fn basic(_ui: &mut Ui, _t: &Theme, _buf: &mut EditorBuffer, _groups: &[GroupRecord]) {}
pub(super) fn auth(_ui: &mut Ui, _t: &Theme, _buf: &mut EditorBuffer, _p: SecretPresence) {}
pub(super) fn network(_ui: &mut Ui, _t: &Theme, _buf: &mut EditorBuffer, _p: SecretPresence) {}
```

`mod.rs` 加 `mod fields;`。

- [ ] **Step 4: 跑到绿**

Run: `cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: `test result: ok.`

Run: `cargo clippy -p mullion-app --all-targets -- -D warnings`
Expected: 无输出。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src
git commit -m "feat(app): 会话管理器右栏骨架 —— 标题/三 Tab/错误卡片/底部按钮 (F90)

错误卡片可关闭,关闭后下一个新错误由 set_error 复位 error_dismissed 重新展开。
save_click 中转一层再转 SaveIntent:build_draft 要读整个 EditorBuffer,
而右栏正持着它的 &mut。字段布局切到 fields.rs,先占位。"
```

---

## Task 12: 三个 Tab 的字段布局

字段的**分配**按 spec §5.1,不新增任何数据字段(范围决策:「不扩,只重排现有字段」)。

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/fields.rs`

- [ ] **Step 1: 写「基础」Tab**

把 `fields.rs` 的 `basic` 替换为(内容整块取自原 `show_editor` 的对应行,
只是拆到三个 Tab 里):

```rust
/// 两列表单的统一样式:左列标签定宽,右列输入撑满。
fn grid(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    egui::Grid::new(id)
        .num_columns(2)
        .spacing([12.0, 8.0])
        .min_col_width(88.0)
        .show(ui, add);
}

pub(super) fn basic(ui: &mut Ui, t: &Theme, buf: &mut EditorBuffer, groups: &[GroupRecord]) {
    let _ = t;
    grid(ui, "sm_basic", |ui| {
        ui.label("名称");
        ui.add(egui::TextEdit::singleline(&mut buf.name).desired_width(f32::INFINITY));
        ui.end_row();

        ui.label("主机");
        ui.add(egui::TextEdit::singleline(&mut buf.host).desired_width(f32::INFINITY));
        ui.end_row();

        ui.label("端口");
        ui.add(egui::TextEdit::singleline(&mut buf.port).desired_width(80.0));
        ui.end_row();

        ui.label("分组");
        let current = buf
            .preserved_group_id
            .and_then(|gid| groups.iter().find(|g| g.id == gid))
            .map(|g| g.name.clone())
            .unwrap_or_else(|| "未分组".to_string());
        egui::ComboBox::from_id_salt("sm_group")
            .selected_text(current)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut buf.preserved_group_id, None, "未分组");
                for g in groups {
                    ui.selectable_value(&mut buf.preserved_group_id, Some(g.id), &g.name);
                }
            });
        ui.end_row();

        ui.label("备注");
        ui.add(egui::TextEdit::multiline(&mut buf.note).desired_rows(3));
        ui.end_row();
    });
}
```

- [ ] **Step 2: 写「认证」Tab**

```rust
pub(super) fn auth(ui: &mut Ui, t: &Theme, buf: &mut EditorBuffer, presence: SecretPresence) {
    grid(ui, "sm_auth", |ui| {
        ui.label("用户名");
        ui.add(egui::TextEdit::singleline(&mut buf.user).desired_width(f32::INFINITY));
        ui.end_row();

        ui.label("认证方式");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut buf.auth_kind, AuthKindUi::Password, "密码");
            ui.selectable_value(&mut buf.auth_kind, AuthKindUi::PublicKey, "公钥");
        });
        ui.end_row();

        match buf.auth_kind {
            AuthKindUi::Password => {
                ui.label("密码");
                super::secret_edit(
                    ui,
                    t,
                    "sm_password",
                    &mut buf.password,
                    &mut buf.password_touched,
                    presence.password,
                );
                ui.end_row();
            }
            AuthKindUi::PublicKey => {
                ui.label("私钥");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut buf.key_path));
                    if ui.button("浏览…").clicked() {
                        buf.pick_key_clicked = true;
                    }
                });
                ui.end_row();

                ui.label("私钥口令");
                super::secret_edit(
                    ui,
                    t,
                    "sm_passphrase",
                    &mut buf.passphrase,
                    &mut buf.passphrase_touched,
                    presence.passphrase,
                );
                ui.end_row();
            }
        }
    });
}
```

`EditorBuffer` 加一个瞬时位(不参与持久化,但参与 `PartialEq` 无妨 ——
它每帧被 `mod.rs` 消费后立刻复位):

```rust
    /// 「浏览…」按钮本帧被点了。`mod.rs` 在借用释放后转成
    /// `UiState::pick_key_request`,随即复位。
    pub pick_key_clicked: bool,
```

`mod.rs` 的 `Window::show` 之后加:

```rust
    if let Some(buf) = ui_state.editor.as_mut() {
        if std::mem::take(&mut buf.pick_key_clicked) {
            ui_state.pick_key_request = true;
        }
    }
```

> 注意 `pick_key_clicked` 会让 `is_dirty` 在点击那一帧误判为脏 —— 因为它在同一帧
> 就被 `std::mem::take` 复位,基线比对发生在下一帧,不会有可观察的影响。若实现
> 中发现顺序反了(先比对后复位),把 `pick_key_clicked` 从 `PartialEq` 里排除:
> 手写 `impl PartialEq for EditorBuffer` 逐字段比,跳过这一位。

- [ ] **Step 3: 写「网络」Tab**

```rust
pub(super) fn network(ui: &mut Ui, t: &Theme, buf: &mut EditorBuffer, presence: SecretPresence) {
    grid(ui, "sm_network", |ui| {
        ui.label("代理");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut buf.proxy_mode, ProxyModeUi::Inherit, "继承分组");
            ui.selectable_value(&mut buf.proxy_mode, ProxyModeUi::Direct, "直连");
            ui.selectable_value(&mut buf.proxy_mode, ProxyModeUi::Socks5, "SOCKS5");
            ui.selectable_value(&mut buf.proxy_mode, ProxyModeUi::HttpConnect, "HTTP");
        });
        ui.end_row();

        if matches!(buf.proxy_mode, ProxyModeUi::Socks5 | ProxyModeUi::HttpConnect) {
            ui.label("代理地址");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut buf.proxy_host));
                ui.add(egui::TextEdit::singleline(&mut buf.proxy_port).desired_width(70.0));
            });
            ui.end_row();

            ui.label("代理用户");
            ui.add(egui::TextEdit::singleline(&mut buf.proxy_user).desired_width(f32::INFINITY));
            ui.end_row();

            ui.label("代理口令");
            super::secret_edit(
                ui,
                t,
                "sm_proxy_password",
                &mut buf.proxy_password,
                &mut buf.proxy_password_touched,
                presence.proxy_password,
            );
            ui.end_row();
        }

        ui.label("跳板链");
        ui.vertical(|ui| {
            ui.checkbox(&mut buf.jump_set, "启用跳板");
            if buf.jump_set {
                ui.colored_label(
                    crate::theme::c32(t.fg_faint),
                    format!("已配置 {} 跳(在分组管理里编辑)", buf.jump_chain.len()),
                );
            }
        });
        ui.end_row();
    });
}
```

`fields.rs` 顶部 `use` 补 `use crate::ui::session_manager::{AuthKindUi, ProxyModeUi};`。

- [ ] **Step 4: 跑到绿**

Run: `cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: `test result: ok.`(此时 `secret_edit` 还没实现,Step 5 之前会编译失败 ——
先把 `secret_edit` 用一行 `ui.add(egui::TextEdit::singleline(v).password(true));` 顶上,
Task 13 再做成三态。)

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/ui/session_manager
git commit -m "feat(app): 会话管理器右栏三 Tab 字段布局 (F90)

字段按设计稿分到基础/认证/网络三个 Tab,数据模型零扩展(只重排现有字段)。
密码框统一走 secret_edit,三态实现在下一个提交。"
```

---

## Task 13: 密码三态控件 `secret_edit`

用户拍板的行为:**默认显示 6 位黑点 / 没改动则密码不变 / 清空即清除凭据 / 改了则保存新密码。**

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/mod.rs`(新增 `secret_edit`)
- Test: `crates/mullion-app/src/ui/session_manager/buffer.rs`(状态机的纯逻辑部分已在 Task 6 覆盖)

- [ ] **Step 1: 实现控件**

追加到 `session_manager/mod.rs`:

```rust
/// 密码框的三态控件(F73)。
///
/// 三个显示状态:
///   1. 没碰过 + 库里有值 → 6 个 `*` 占位 + `password(true)`,只读观感;
///   2. 没碰过 + 库里没值 → 空框 + hint「未设置」;
///   3. 碰过        → 正常可编辑 + 右侧「撤销」按钮。
///
/// **占位符永远不会流进 `SecretField::Set`**:状态 1/2 下 `touched` 是 false,
/// `secret_fields` 直接给 `Keep`,根本不读框里的字符串。迁移点选
/// `gained_focus()` 而不是 `changed()` —— 聚焦那一刻就把框清空,用户看到的是
/// 一个空框(而不是 6 个星号后面接着自己输的字),也就不可能把 `******` 连同
/// 新密码一起存进去。
///
/// 位数固定 6 位:黑点数量若跟真实长度走,就把密码长度泄漏给了肩窥者。
pub(crate) fn secret_edit(
    ui: &mut egui::Ui,
    t: &Theme,
    id: &str,
    value: &mut String,
    touched: &mut bool,
    has_stored: bool,
) {
    ui.horizontal(|ui| {
        if *touched {
            ui.add(
                egui::TextEdit::singleline(value)
                    .id_salt(id)
                    .password(true)
                    .desired_width(200.0),
            );
            if ui.small_button("撤销").clicked() {
                *touched = false;
                value.clear();
            }
            if value.is_empty() {
                ui.colored_label(theme::c32(t.warn), "留空 = 清除已存凭据");
            }
        } else if has_stored {
            let mut placeholder = "******".to_string();
            let resp = ui.add(
                egui::TextEdit::singleline(&mut placeholder)
                    .id_salt(id)
                    .password(true)
                    .desired_width(200.0),
            );
            if resp.gained_focus() {
                // 一聚焦就翻面:框清空、进入可编辑态。占位符本身从不外流。
                *touched = true;
                value.clear();
            }
            ui.colored_label(theme::c32(t.fg_faint), "已设置(不修改则保持不变)");
        } else {
            let resp = ui.add(
                egui::TextEdit::singleline(value)
                    .id_salt(id)
                    .password(true)
                    .hint_text("未设置")
                    .desired_width(200.0),
            );
            if resp.gained_focus() {
                *touched = true;
            }
        }
    });
}
```

- [ ] **Step 2: 删掉旧提示文案**

原 `show_editor` 里那行「留空密码 / 私钥口令 = 清除已存凭据(不是「保持不变」)。」
(旧 `session_manager.rs:720`)现在是**错的** —— 留空不再等于清除,除非用户主动碰过框。
删掉它;新语义由 `secret_edit` 内联的两句提示表达。

Run: `grep -rn "不是「保持不变」" crates/mullion-app/src`
Expected: 无输出。

- [ ] **Step 3: 写占位符不外流的守护测试**

追加到 `buffer.rs` 的 `mod tests`:

```rust
    /// 占位符 `******` 绝不能被当成真密码存进去。控件用 `gained_focus()` 做
    /// 迁移点、聚焦即清空,保证了这一点;这条测试守的是**下游**:即使某天
    /// 控件写错、把占位符留在了 `value` 里,只要 `touched` 还是 false,
    /// `secret_fields` 就必须给 `Keep`,不读那个字符串。
    /// 自证会变红:把 `secret_fields` 里的 `field()` 改成不看 `touched`
    /// (`if v.is_empty() { Clear } else { Set(..) }`),这条报 Keep != Set。
    #[test]
    fn untouched_field_never_leaks_its_placeholder_into_a_set_intent() {
        let mut b = buf();
        b.password = "******".into(); // 模拟控件把占位符留在了缓冲里
        b.password_touched = false;
        assert_eq!(
            secret_fields(&b).0,
            SecretField::Keep,
            "未碰过的框无论里面装着什么,都必须是 Keep"
        );
    }
```

- [ ] **Step 4: 跑到绿并红队自证**

Run: `cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: `test result: ok.`

```bash
# 注入:secret_fields 的 field() 不看 touched
cargo test -p mullion-app untouched_field_never_leaks 2>&1 | grep -E "test result|FAILED"
# Expected: 变红
git checkout crates/mullion-app/src/ui/session_manager/buffer.rs
```

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/ui/session_manager
git commit -m "feat(app): 密码框三态控件 secret_edit (F73)

默认 6 位黑点(位数固定,跟真实长度走会泄漏密码长度);聚焦即清空并进入
可编辑态,占位符永不流进 SecretField::Set;可「撤销」回保持态;碰过后留空
= 清除凭据,并给出黄色提示。删掉旧的「留空 = 清除」文案 —— 那句现在是错的。

红队自证:让 secret_fields 不看 touched 位,
untouched_field_never_leaks_its_placeholder_into_a_set_intent 变红。"
```

---

## Task 14: 未保存变更的切换确认

左栏点另一条会话时,若右栏是脏的,不能静默丢弃。

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/mod.rs`
- Modify: `crates/mullion-app/src/ui/mod.rs`(`UiState` 加一位)

- [ ] **Step 1: 在 `mod.rs` 里消费 `pending_switch`**

`UiState` 加:

```rust
    /// 切换时表单是脏的 → 正在等用户确认。为真时右栏顶部压一条确认横幅。
    pub confirm_switch: bool,
```

`mod.rs::show` 的 `Window::show` 闭包**之后**加(放在消费 `save_click` 之后):

```rust
    // 切换目标的消费:表单脏就先挂起等确认,不静默丢弃用户刚打的字。
    if ui_state.pending_switch.is_some() {
        let dirty = match (ui_state.editor.as_ref(), ui_state.editor_baseline.as_ref()) {
            (Some(b), Some(base)) => is_dirty(b, base),
            _ => false,
        };
        if dirty {
            ui_state.confirm_switch = true;
        } else {
            apply_switch(ui_state, sessions);
        }
    }
```

加辅助函数:

```rust
/// 真正切到 `pending_switch` 指向的目标,同时重置基线与 Tab。
fn apply_switch(ui_state: &mut UiState, sessions: &[SessionRecord]) {
    let Some(target) = ui_state.pending_switch.take() else {
        return;
    };
    ui_state.confirm_switch = false;
    match target {
        SwitchTarget::NewDraft => {
            ui_state.editor = Some(EditorBuffer::default());
            ui_state.editor_id = None;
        }
        SwitchTarget::Session(id) => {
            let Some(rec) = sessions.iter().find(|r| r.id == id) else {
                // 会话在这一帧之间被删了(左栏的删除也走意图,可能先落地)。
                // 静默忽略,别把编辑区切成一张指向不存在会话的表单。
                return;
            };
            ui_state.editor = Some(EditorBuffer::from_record(rec));
            ui_state.editor_id = Some(id);
        }
    }
    // 基线必须在这里同步设置:漏了它,刚打开的会话立刻被判成脏,
    // 下一次切换就会弹一个莫名其妙的确认。
    ui_state.editor_baseline = ui_state.editor.clone();
    ui_state.editor_tab = 0;
}
```

`EditorBuffer::from_record` 需要跨模块可见 —— 把它从 `fn` 提到 `pub(crate) fn`。

- [ ] **Step 2: 在右栏画确认横幅**

`editor.rs` 的标题条之后、错误卡片之前插入:

```rust
    if ui_state.confirm_switch {
        egui::Frame::none()
            .fill(theme::c32(t.sunken_bg))
            .stroke(egui::Stroke::new(1.0, theme::c32(t.warn)))
            .rounding(8.0)
            .inner_margin(10.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(theme::c32(t.warn), "有未保存的更改");
                    if ui.button("丢弃并切换").clicked() {
                        ui_state.discard_and_switch = true;
                    }
                    if ui.button("留在这里").clicked() {
                        ui_state.pending_switch = None;
                        ui_state.confirm_switch = false;
                    }
                });
            });
        ui.add_space(8.0);
    }
```

`UiState` 再加一位 `pub discard_and_switch: bool,`,`mod.rs` 在
`Window::show` 之后消费:

```rust
    if std::mem::take(&mut ui_state.discard_and_switch) {
        apply_switch(ui_state, sessions);
    }
```

> `editor.rs` 里持着 `ui_state.editor` 的 `&mut`,不能在那里直接调 `apply_switch`
> (它要重设 `ui_state.editor`)—— 这就是中转一个 bool 的原因。

- [ ] **Step 3: 写守护测试**

追加到 `crates/mullion-app/src/ui/mod.rs` 的 `mod tests`:

```rust
    /// 打开一条会话后,基线必须同步设置好 —— 否则刚打开就被判成脏,
    /// 用户什么都没改也会挨一次「有未保存的更改」。
    /// 自证会变红:删掉 `apply_switch` 末尾的
    /// `ui_state.editor_baseline = ui_state.editor.clone();`,这条报「不该脏」。
    #[test]
    fn opening_a_session_sets_the_baseline_so_it_is_not_immediately_dirty() {
        let mut st = UiState {
            session_manager_open: true,
            pending_switch: Some(crate::ui::session_manager::SwitchTarget::NewDraft),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        crate::theme::apply_egui(&ctx, &crate::theme::MULLION_DARK);
        let frame = UiFrame {
            store_available: true,
            ..base_frame()
        };
        for _ in 0..2 {
            run_frame(&ctx, &mut st, frame, egui::RawInput::default());
        }

        assert!(st.editor.is_some(), "新建草稿应已切入编辑区");
        assert_eq!(
            st.editor, st.editor_baseline,
            "基线必须与刚打开的表单一致,否则会立刻被判成脏"
        );
        assert!(!st.confirm_switch, "刚打开不该弹未保存确认");
    }
```

- [ ] **Step 4: 跑到绿并红队自证**

Run: `cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: `test result: ok.`

```bash
# 注入:删掉 apply_switch 末尾的基线赋值
cargo test -p mullion-app opening_a_session_sets_the_baseline 2>&1 | grep -E "test result|FAILED"
# Expected: 变红
git checkout crates/mullion-app/src/ui/session_manager/mod.rs
```

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src
git commit -m "feat(app): 切换会话时对未保存变更弹确认 (F90)

pending_switch 先挂起、is_dirty 判定后再消费;确认横幅压在右栏顶部,
不再开新窗口。apply_switch 里同步重设基线,漏了它会导致刚打开就被判成脏。

守护测试:ui::tests::opening_a_session_sets_the_baseline_so_it_is_not_immediately_dirty
(删掉基线赋值即变红)。"
```

---

## Task 15: 复制连接串按钮 + UI 集成测试

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/editor.rs`
- Test: `crates/mullion-app/src/ui/session_manager/buffer.rs`、`crates/mullion-app/src/ui/mod.rs`

- [ ] **Step 1: 写连接串纯函数的失败测试**

追加到 `buffer.rs` 的 `mod tests`:

```rust
    /// 复制出来的连接串要能直接粘进终端跑。端口是 22 时省略 `-p`
    /// (`ssh -p 22` 虽然能跑,但没人这么写)。
    #[test]
    fn connect_string_is_pasteable_and_omits_the_default_port() {
        let mut b = buf();
        b.user = "root".into();
        b.host = "192.0.2.10".into();
        b.port = "22".into();
        assert_eq!(connect_string(&b), "ssh root@192.0.2.10");

        b.port = "2222".into();
        assert_eq!(connect_string(&b), "ssh -p 2222 root@192.0.2.10");
    }

    /// 连接串里**绝不能**出现密码 —— 它会进系统剪贴板,再进用户的聊天记录。
    /// 自证会变红:在 `connect_string` 里拼上 `buf.password`,这条立刻红。
    #[test]
    fn connect_string_never_contains_a_password() {
        let mut b = buf();
        b.password = "hunter2".into();
        b.password_touched = true;
        assert!(
            !connect_string(&b).contains("hunter2"),
            "连接串会进剪贴板,绝不能带密码"
        );
    }
```

- [ ] **Step 2: 跑到红**

Run: `cargo test -p mullion-app connect_string 2>&1 | grep -E "^error|cannot find"`
Expected: `cannot find function \`connect_string\``。

- [ ] **Step 3: 实现**

追加到 `buffer.rs`:

```rust
/// 表单 → 可直接粘进终端的 ssh 连接串。**只用非敏感字段** ——
/// 它会进系统剪贴板,拼上口令等于把口令交给剪贴板历史。
pub(crate) fn connect_string(buf: &EditorBuffer) -> String {
    let user = buf.user.trim();
    let host = buf.host.trim();
    match buf.port.trim() {
        "22" | "" => format!("ssh {user}@{host}"),
        p => format!("ssh -p {p} {user}@{host}"),
    }
}
```

`editor.rs` 的标题条那一行右侧加按钮:

```rust
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("复制连接串").clicked() {
                ui.ctx().copy_text(super::connect_string(buf));
            }
        });
```

> `Context::copy_text` 是 egui 0.30 的写法。若编译报 `no method named copy_text`,
> 改用 `ui.output_mut(|o| o.copied_text = ...)`(0.29 及更早的写法),**以编译器
> 报的实际签名为准,不要猜**。

- [ ] **Step 4: 写右栏 UI 集成测试**

追加到 `crates/mullion-app/src/ui/mod.rs` 的 `mod tests`:

```rust
    /// 没选中会话时,右栏给空态提示而不是一张填不进去的空表单。
    #[test]
    fn editor_shows_empty_state_when_nothing_is_selected() {
        let frame = UiFrame {
            store_available: true,
            ..base_frame()
        };
        let mut st = UiState {
            session_manager_open: true,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        crate::theme::apply_egui(&ctx, &crate::theme::MULLION_DARK);
        let mut text = String::new();
        for _ in 0..2 {
            let (out, _) = run_frame(&ctx, &mut st, frame, egui::RawInput::default());
            text = collect_text(&out);
        }
        assert!(text.contains("从左侧选一条会话"), "应显示空态提示,实得:{text}");
    }

    /// §3.1 降级:会话库打不开时不画双栏,只给一句话 —— 否则用户对着一张
    /// 永远存不下去的表单填半天。
    #[test]
    fn store_unavailable_degrades_to_a_single_line_instead_of_a_dead_form() {
        let frame = UiFrame {
            store_available: false,
            ..base_frame()
        };
        let mut st = UiState {
            session_manager_open: true,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        crate::theme::apply_egui(&ctx, &crate::theme::MULLION_DARK);
        let mut text = String::new();
        for _ in 0..2 {
            let (out, _) = run_frame(&ctx, &mut st, frame, egui::RawInput::default());
            text = collect_text(&out);
        }
        assert!(text.contains("会话库不可用"), "应给降级提示,实得:{text}");
        assert!(!text.contains("从左侧选一条会话"), "降级时不该画双栏");
    }
```

> `collect_text` 是 `rendered_text`(`ui/mod.rs:263-283`)内部把 `FullOutput`
> 拍平成文本的那一段。若它现在是内联的,把它抽成一个 `fn collect_text(out: &egui::FullOutput) -> String`
> 供两处复用 —— `rendered_text` 自己也改成调它,不要复制一份。

- [ ] **Step 5: 跑到绿并红队自证**

Run: `cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: `test result: ok.`

```bash
# 注入:connect_string 里拼上密码
cargo test -p mullion-app connect_string_never 2>&1 | grep -E "test result|FAILED"
# Expected: 变红
git checkout crates/mullion-app/src/ui/session_manager/buffer.rs
```

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-app/src
git commit -m "feat(app): 复制连接串按钮 + 右栏空态/降级的 UI 测试 (F90)

connect_string 只拼非敏感字段 —— 它进系统剪贴板,带口令等于交给剪贴板历史。
端口 22 时省略 -p。

红队自证:在 connect_string 里拼上 password,
connect_string_never_contains_a_password 变红。"
```

---

## Task 16: spec.md 登记编号 + 全量回归

**Files:**
- Modify: `spec.md`
- Modify: `docs/superpowers/specs/2026-08-03-session-manager-ui-design.md`(勾掉已完成项)

- [ ] **Step 1: 在 `spec.md` 登记 F90 / F73**

按 `spec.md` 现有的编号表格式补两行(F73 若已存在则只更新描述):

```markdown
| F90 | 会话管理器单窗双栏(880×560),左栏列表 + 右栏三 Tab 编辑区 | 已实现 |
| F73 | 编辑已有会话时凭据三态(保持/覆盖/清除),不再静默清除密码 | 已实现 |
```

- [ ] **Step 2: 全量回归**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```
Expected: 全 `ok.`,clippy 与 fmt 无输出。**这才是「绿」** —— 只跑
`-p mullion-app` 不算。

- [ ] **Step 3: 领域陷阱回归(本轮动过 `app.rs` 事件循环)**

```bash
cargo test -p mullion-app redraw_is_frame_capped -- --nocapture
cargo test -p mullion-app frame::tests
cargo test -p mullion-app terminal_keyboard_is_never_fed_to_egui
```
Expected: 三组全过。T3(重绘节流)/T7(`ControlFlow::WaitUntil` 复位)/T8(键盘路由)
是最容易在改事件循环时被悄悄破坏的 —— 本轮改了 `app.rs:911` 的 modal 判断,
T8 直接相关。

- [ ] **Step 4: 确认 `buffer.rs` 没被 egui 污染**

Run: `grep -n "egui" crates/mullion-app/src/ui/session_manager/buffer.rs`
Expected: 无输出。有输出就是把 UI 类型漏进了唯一能纯单测的模块,必须挪走。

- [ ] **Step 5: 提交**

```bash
git add spec.md docs/superpowers/specs/2026-08-03-session-manager-ui-design.md
git commit -m "docs: spec.md 登记 F90/F73,回填设计文档完成状态"
```

---

## Task 17: 交付(bump / 交叉编译 / Release)

按 CLAUDE.md 的交付约定一条龙做完,不用再问。

**Files:**
- Modify: `Cargo.toml:12`(0.1.16 → 0.1.17)

- [ ] **Step 1: bump 并单独提交**

```bash
# Cargo.toml 第 12 行:version = "0.1.17"
git add Cargo.toml
git commit -m "chore: 版本 0.1.17(会话管理器单窗双栏 + 凭据三态修复)"
```

- [ ] **Step 2: 跑绿**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```
Expected: 全 `ok.`,两条命令无输出。不绿不发。

- [ ] **Step 3: 交叉编译 + objdump 验收**

```bash
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app
x86_64-w64-mingw32-objdump -p target/x86_64-pc-windows-gnu/release/mullion.exe | grep -i "DLL Name"
```
Expected: 不出现 `libgcc_s_seh-1.dll` / `libwinpthread-1.dll`。出现即不合格。

- [ ] **Step 4: 写 notes.md**

正文包含:改了什么、sha256、SmartScreen 首次运行提示、以及这份人工验收清单
(spec §10,无头环境全部验不了):

```
## 人工验收清单(v0.1.17)
- [ ] 会话管理器只有一个窗口,880×560,左栏 300px 定宽
- [ ] 左栏搜索框输入名称 / IP 片段 / 标签都能过滤;搜索时分组自动展开
- [ ] 选中行有左侧强调条,已连接的会话状态点是绿色,其余是灰色
- [ ] 右键会话 →「删除」→ 确认条内联展开在那一行下面(不再弹独立窗口)
- [ ] 右栏三个 Tab(基础/认证/网络)切换正常,字段无错位/无重叠
- [ ] **F73 主验收**:打开一条已有密码的会话 → 只改备注 → 保存 → 重新打开 →
      直接连接,**密码仍然有效**(改前这一步会认证失败)
- [ ] 密码框默认显示 6 个黑点;点进去后框变空、出现「撤销」按钮
- [ ] 点「撤销」后回到 6 个黑点态;保存后密码不变
- [ ] 点进密码框、留空、保存 → 再连接时提示需要密码(凭据确实被清除了)
- [ ] 改了字段不保存就点另一条会话 → 弹「有未保存的更改」,「留在这里」有效
- [ ] 「复制连接串」粘贴出来是 `ssh user@host`(非 22 端口带 `-p`),且不含密码
- [ ] 触发一个错误(如填非法端口保存)→ 右栏出现红色错误卡片 → 点 × 关掉 →
      再触发另一个错误 → 卡片**重新出现**
- [ ] 中文输入法在名称/备注框里候选框位置正常
- [ ] 窗口在高 DPI 下字号正常,不糊
```

- [ ] **Step 5: 先推 main,再发 Release**

```bash
git push origin main
cd target/x86_64-pc-windows-gnu/release
sha256sum mullion.exe > mullion.exe.sha256
HTTPS_PROXY=http://127.0.0.1:7890 gh release create v0.1.17 \
  mullion.exe mullion.exe.sha256 -t "v0.1.17" -F /data/Mullion/notes.md \
  --repo kilobitcy/Mullion
```

> **先推后发** —— P0-b 那次先发 Release,tag 指向了旧提交,只能事后用
> `gh api -X PATCH` 移正(`sha` 要给完整 40 位)。Release 标题**只能**是纯版本号
> `v0.1.17`,不带破折号、摘要、emoji。

- [ ] **Step 6: 报给用户**

Release 链接 + sha256 + 上面那份验收清单。明确写出:GPU 渲染正确性、是否真的不闪、
输入法行为、手感 —— 这些无头环境验证不了,**未验证,需人工确认**。

---

## Self-Review

**spec 覆盖(逐节对照):**

| spec 节 | 覆盖任务 |
|---|---|
| §1 背景与范围 / 编号 F90+F73 | Task 16 |
| §2 视觉 · `danger_soft` | Task 4 |
| §3 窗口骨架 · `set_min_height` 假设 | Task 2、Task 3 |
| §3.1 `store_available` 降级 | Task 2(实现)、Task 15(测试) |
| §3.2 删 `editor_open`(11 处) | Task 2 |
| §4.1 `session_row` 手绘 | Task 10 |
| §4.2 搜索 `matches` | Task 10 |
| §4.3 删除内联确认 | Task 10 |
| §4.4 复制按钮 | Task 15 |
| §5.1 三 Tab 字段分配 | Task 12 |
| §5.2 错误卡片 + `set_error` 收口 | Task 8、Task 11 |
| §5.3 基线快照脏检查 + `SwitchTarget` | Task 9、Task 14 |
| §5.4 凭据三态(六个子节) | Task 5、6、7、13 |
| §6 状态点两态 + 连接追踪 | Task 9(`connected_session`)、Task 10(画点) |
| §7.1 `UiState` 增删 | Task 2、8、9、11、14 |
| §7.2 `apply_save` 抽纯函数 | Task 7 |
| §8 文件结构(mod/list/editor/buffer) | Task 1、2、10、11;**偏差**:多切了一个 `fields.rs`(Task 11/12),`editor.rs` 只留骨架,理由见下 |
| §9.0 测试迁移 | Task 1、Task 6 |
| §9.1 新增纯逻辑测试(含 7 条凭据红队) | Task 5(8 条)、Task 6(2 条)、Task 9(2 条)、Task 13(1 条)、Task 15(2 条) |
| §9.2 跑真 UI 的测试 | Task 2(单窗)、Task 14(基线)、Task 15(空态/降级) |
| §9.3 回归 | Task 16 |
| §10 人工验收清单 | Task 17 |
| §11 架构不变量 | Task 16 Step 4(`buffer.rs` 零 egui 的 grep 门禁) |
| §12 已拍板决策 | 决策 2(`set_min_height` 先验证)= Task 3,排在所有右栏视觉任务之前 |

**与 spec §8 的偏差(一处,有意):** spec 给的是四文件(mod/list/editor/buffer),
本计划切成五个 —— 右栏的字段布局单独进 `fields.rs`。理由:`editor.rs` 若同时装
窗口骨架、错误卡片、Tab 条、三个 Tab 的全部字段和密码控件,会超过 600 行,
违反「文件够小才能整个装进上下文」这条。`editor.rs` 只留骨架(~180 行),
`fields.rs` 装字段(~220 行)。实现时若发现拆得多余,合回去也可以,
但不要让 `editor.rs` 单文件超过 400 行。

**类型/函数名一致性核对:**
`SecretField`(Task 5 定义,7/12/13 使用)、`merge_secret`(5 定义,7 使用)、
`sync_has_passphrase`(5 定义,6/7 使用)、`secret_fields`(6 定义,11/13 使用)、
`SecretPresence`(7 定义,9/11/12 使用)、`SaveIntent`(7 扩字段,11 构造,7 消费)、
`is_dirty`(9 定义,14 使用)、`SwitchTarget`(9 定义,10/14 使用)、
`set_error`(8 定义,7/11 使用)、`apply_save`(7 定义,7 接线)、
`connect_string`(15 定义,15 使用)、`secret_edit`(13 定义,12 调用)、
`EditorBuffer::from_record`(1 搬移,14 提可见性到 `pub(crate)`)——
全部一致,无孤儿引用。

**已知的顺序耦合:** Task 12 会调用 Task 13 才实现的 `secret_edit`,Task 12 Step 4
已注明先用一行 `TextEdit::singleline(v).password(true)` 顶上。Task 7 Step 6 会用到
Task 8 才有的 `set_error`,同样已注明先写直接赋值、Task 8 统一改。这两处是有意的
(把「能独立测的纯逻辑」排在「要 GUI 的控件」前面),不是遗漏。
