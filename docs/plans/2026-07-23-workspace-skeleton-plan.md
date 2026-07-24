# 实现计划:Mullion workspace 骨架

- 日期: 2026-07-23
- 设计文档: `docs/plans/2026-07-23-workspace-skeleton-design.md`
- 关联: spec.md、README.md、CLAUDE.md 陷阱表、ADR-001/002/003
- 执行方式: 建议新开对话,用 `superpowers:executing-plans` skill 按 Phase 执行

## Overview

把四-crate 架构从"零行代码"立成**可编译的 workspace 骨架**,并让"能在无 GPU /
无真实 SSH 下单测"的守护测试到绿。功能实现一律不做。

**架构不变量(不得违反)**:`app → {core, term, ssh}`,其余互不依赖;
`core` 零 UI/IO/async;`term` 只依赖 alacritty_terminal/vte。

## Current State

- 仓库只有文档(spec/README/CLAUDE/ADR/本计划),无任何 `.rs`、无 `Cargo.toml`。
- 工具链:`cargo 1.83.0` / `rustc 1.83.0`。
- `~/.cargo/registry/src` 无缓存;`curl` crates.io CDN 返回 403 → **联网拉依赖能力未确认**。
- git:分支 `main`,本机无远端。

## Desired End State

- `cargo build --workspace` 通过。
- 六个守护测试**就位**,其中不依赖真实 GPU/SSH 的**到绿**:
  - `mullion-core`: 布局测试(F30–F33)
  - `mullion-term`: `emulator::tests::pty_write_is_collected`(T1)、
    `keymap::tests::shift_blocks_mouse_report_so_user_can_copy`(T5)、
    `keymap::tests::shift_enter_without_kitty_is_esc_cr`(T6)
  - `mullion-app`: `render::tests::sync_update_defers_present`(T2)、
    `app::tests::redraw_is_frame_capped`(T3)、`app::tests::reflow_emits_resize`(T4)
- GPU 渲染、真实 SSH 只留**可编译占位** + 注释标注「未验证,需人工确认」。
- `cargo clippy --workspace --all-targets -- -D warnings` 与 `cargo fmt --check` 干净。

> **绿的定义**(CLAUDE.md):`cargo test --workspace` 全过 **且** clippy 无输出。
> 骨架阶段 GPU/SSH 真实行为无法自动验证,不计入"绿",但**上述守护测试必须真绿**。

---

## Phase 0 — 依赖可达性与版本核对

**目标**:确认能否拉外部依赖,定 workspace 走完整版还是离线回退。

**步骤**:
1. `cargo new --lib /tmp/dep-probe && cd /tmp/dep-probe && cargo add alacritty_terminal@0.26 2>&1`
   —— 能成功即联网可用。
2. 若成功,逐个核对签名(不凭记忆,README「版本核对」要求):
   ```
   cargo doc -p russh --no-deps         # client::Handler 签名(历史多次改)
   cargo doc -p winit --no-deps         # 0.30 ApplicationHandler
   cargo doc -p alacritty_terminal      # Term::new / EventListener / Event::PtyWrite / damage()
   ```
   记录实际签名到本 Phase 的执行笔记。
3. 若拉取失败(离线)→ 触发**离线回退**:本次骨架只做 Phase 1(仅 core 成员)+ Phase 2,
   term/ssh/app 暂不加入 workspace(留待联网),并在计划里标注。

**验证**:`cargo add` 的 exit code;或明确记录"离线,走回退"。
**完成标准**:得到"联网可用 + 六个 crate 的实际签名" 或 "离线,回退到 core-only"的明确结论。

---

## Phase 1 — workspace 结构可编译

**目标**:根 `Cargo.toml` + 四个 crate 空壳,`cargo build --workspace` 通过。

**文件**:
- `Cargo.toml`(根):`[workspace]`,`members = ["crates/mullion-core", ".../term", ".../ssh", ".../app"]`,
  `resolver = "2"`,`[workspace.dependencies]` 统一锁版本
  (`alacritty_terminal=0.26, vte=0.15, russh=0.54, winit=0.30, wgpu=23, glyphon=0.7`)。
- `crates/mullion-core/{Cargo.toml, src/lib.rs}` — 无外部依赖。
- `crates/mullion-term/{Cargo.toml, src/lib.rs}` — 依赖 alacritty_terminal、vte。
- `crates/mullion-ssh/{Cargo.toml, src/lib.rs}` — 依赖 russh、tokio。
- `crates/mullion-app/{Cargo.toml, src/main.rs}` — 依赖 core/term/ssh + winit/wgpu/glyphon。

**gotcha**:依赖方向靠 Cargo.toml 强制——core/term/ssh 的 Cargo.toml **不得**互相引用,也不得引 app。
**验证**:`cargo build --workspace` exit 0。
**完成标准**:四 crate 空壳编译通过,依赖方向单向。

---

## Phase 2 — mullion-core 布局树(F30–F33)到绿

**目标**:自研分屏的布局树 + 布局测试到绿。这是纯 Rust、无外部依赖,最先能真绿。

**文件**:`crates/mullion-core/src/layout.rs`(+ `lib.rs` 导出)。

**类型草案**(实现时可调,保持零 UI/IO):
```rust
pub struct Rect { pub col: u16, pub row: u16, pub cols: u16, pub rows: u16 }
pub enum Dir { Horizontal, Vertical }
pub enum Node { Leaf(PaneId), Split { dir: Dir, ratio: f32, a: Box<Node>, b: Box<Node> } }
pub fn compute_rects(root: &Node, area: Rect) -> Vec<(PaneId, Rect)>;
```

**测试(到绿)**:
- 水平/垂直嵌套分屏,`compute_rects` 矩形不重叠、拼满 area(F30)。
- 关闭 pane 兄弟顶替;最后一个 pane 不可关(F31)。
- 拖分隔条 resize 夹紧最小尺寸(F32)。
- 方向键切焦点,几何法不跳斜对角(F33)。

**验证**:`cargo test -p mullion-core` 全绿。
**完成标准**:F30–F33 各有测试且绿。

---

## Phase 3 — mullion-term:keymap(T5/T6) + emulator(T1)到绿

**目标**:输入编码与 VT 封装的守护测试到绿 + VT fixture 目录就位。

**文件**:`src/keymap.rs`、`src/emulator.rs`、`tests/fixtures/README.md`(录制约定)。

**keymap 草案**:
```rust
pub fn encode_key(key: Key, mods: Mods, kitty: bool) -> Vec<u8>;
pub fn mouse_should_report(mods: Mods, capture_on: bool) -> bool; // Shift 时恒 false
```
**测试(到绿)**:
- T6 `keymap::tests::shift_enter_without_kitty_is_esc_cr`:Shift+Enter 无 kitty → `\x1b\r`;
  有 kitty → `CSI 13;2u`;`Ctrl+J` 恒 `\n`(F14)。
- T5 `keymap::tests::shift_blocks_mouse_report_so_user_can_copy`:capture 开 + 按 Shift → 不上报(F15)。

**emulator 草案**:封装 `alacritty_terminal::Term` + 一个 `EventListener`,
把 `Event::PtyWrite` 收进出站缓冲(**T1 红线**:漏了会导致同步输出探测无应答→闪)。
```rust
pub struct Emulator { /* Term + 出站缓冲 */ }
impl Emulator { pub fn feed(&mut self, bytes: &[u8]); pub fn take_pty_writes(&mut self) -> Vec<u8>; }
```
**测试(到绿)**:T1 `emulator::tests::pty_write_is_collected`——喂入会触发 `PtyWrite` 的序列
(如 DA/光标位置查询),断言 `take_pty_writes()` 非空且内容正确。

**gotcha**:`Event::PtyWrite` 的确切枚举/`EventListener` 签名按 Phase 0 记录的实际 API 写,别凭记忆。
**验证**:`cargo test -p mullion-term` 上述测试绿。
**完成标准**:T1/T5/T6 绿;fixture 目录 + 录制约定 README 就位(暂无 .bin)。

---

## Phase 4 — mullion-app 纯逻辑:T2/T3/T4 到绿(GPU 留占位)

**目标**:三个"挂在 app 上"的守护测试用**不碰 wgpu/winit 的纯件**实现到绿;GPU 路径占位。

**文件**:`src/render.rs`(攒帧纯件 + glyphon 接口占位)、`src/frame.rs` 或同模块(帧率节流)、
`src/reflow.rs`、`src/pane.rs`(占位)、`src/main.rs`(winit 占位)。

**测试(到绿)**:
- T2 `render::tests::sync_update_defers_present`:攒帧状态机——喂含 `CSI ? 2026 h/l` 的序列,
  断言 `h`..`l` 之间 `should_present()==false`,收到 `l` 才 true。present 用抽象回调,不碰 GPU。
- T3 `app::tests::redraw_is_frame_capped`:**可注入时钟**的节流器,断言 16ms 窗口内不超发一帧(N3)。
- T4 `app::tests::reflow_emits_resize`:布局变更→算每个受影响 pane 新行列→经**抽象 resize sink** 发出;
  测试注入 fake sink,断言收到的列数与新矩形一致(F34)。

**占位(标注「未验证,需人工确认」)**:`main.rs` 的 `winit::ApplicationHandler`、
`render.rs` 的 wgpu/glyphon 真实渲染、`pane.rs` 串接。这些**编译通过即可**,不实现真渲染。

**验证**:`cargo test -p mullion-app` 上述三测试绿。
**完成标准**:T2/T3/T4 绿;GPU 占位可编译且带标注。

---

## Phase 5 — mullion-ssh 占位 + KnownHosts 桩(F3)

**目标**:PTY channel 占位 + TOFU 骨架。**KnownHosts 绝不返回 `Ok(true)`**。

**文件**:`src/pty.rs`、`src/known_hosts.rs`。

**草案**:
```rust
pub struct KnownHosts { /* 指纹表 */ }
impl KnownHosts { pub fn verify(&self, host: &str, fp: &Fingerprint) -> bool; } // 不匹配→false
```
**测试(到绿)**:F3 —— 指纹不匹配时 `verify()` 返回 `false`(单测,不需网络)。
**占位**:`pty.rs` 的 russh 连接/PTY channel 编译通过 + 标注「未验证,需人工确认」。

**验证**:`cargo build -p mullion-ssh` + `cargo test -p mullion-ssh`(KnownHosts 测试绿)。
**完成标准**:F3 测试绿;russh 占位可编译。

---

## Phase 6 — 收尾:lint + 原子提交

**步骤**:
1. `cargo fmt`;`cargo clippy --workspace --all-targets -- -D warnings` 修到无输出。
2. `cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log`。
3. 按 crate 拆**原子提交**,摘要引用 spec 编号,触到陷阱的正文写明守护测试:
   - `feat(core): 布局树与分屏 (F30–F33)`
   - `feat(term): 键鼠编码与 emulator PtyWrite 回收 (F14/F15, T1/T5/T6)`
   - `feat(app): 攒帧/帧率/reflow 纯逻辑 (F11/N3/F34, T2/T3/T4)`
   - `feat(ssh): PTY 占位与 KnownHosts TOFU 骨架 (F3)`

**完成标准**:workspace 测试全过(GPU/SSH 真实行为除外)、clippy/fmt 干净、提交拆分完成。

---

## Testing Strategy

- 纯逻辑守护测试(布局、键码、攒帧、帧率、reflow、KnownHosts)= 骨架阶段的正确性红线,必须绿。
- VT 快照:本骨架只建 fixture 目录 + 录制约定;新增 VT 功能时才补 `.bin`/`.snap`。
- **无法自动验证**(GPU 渲染、真实 SSH、输入法、"不闪")→ 只占位 + 标注,写进 PR 的人工验证清单,不假装绿。

## Rollout / Risks

- **R-net**:离线则走 Phase 0 回退(core-only);联网恢复后再补 term/ssh/app。
- **R-apidrift**:russh/winit/wgpu API 漂移 → 一切签名以 Phase 0 `cargo doc` 记录为准,不凭记忆。
- 骨架不引入任何功能范围;超出即 Scope 违规。
