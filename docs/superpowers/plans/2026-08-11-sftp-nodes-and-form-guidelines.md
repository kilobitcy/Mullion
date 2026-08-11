# SFTP 节点分档 + 隧道表单重排 + 表单布局规范 实施计划（F118 / F119）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 会话管理器加一档 SFTP 节点管理（复用 `SessionRecord`，按 `protocol` 分流），隧道表单按会话「连接」页重排，并把表单骨架抽成公共构件 + 一份规范文档 + 两条机械守护。

**Architecture:** 数据层零改动（schema 仍 v7）。SFTP 节点 = `protocol == Sftp` 的会话记录；分流的唯一真源是纯函数 `protocol_of(mode)`，列表渲染与键盘顺序共用它。表单骨架（分节 / 两列 Grid / 必填星号 / 内联红字）从 `fields.rs` 抽到新模块 `form.rs`，会话页与隧道页共用。

**Tech Stack:** Rust 2021 / egui 0.30 / eframe 无（winit + wgpu 自绘）/ `cargo test -p mullion-app`

**设计文档：** `docs/superpowers/specs/2026-08-11-sftp-nodes-and-form-guidelines-design.md`（决策编号 D1~D10 在下面被反复引用）

---

## 读之前必须知道的三件事

1. **这个仓库的「绿」= `cargo test --workspace` 全过 **且** `cargo clippy --workspace --all-targets -- -D warnings` 无输出 **且** `cargo fmt --check` 干净。**只跑单个 crate 不叫绿，不许据此说「测试通过」。
2. **UI 代码写完不许说「已验证」。** 无头容器里跑不了 GPU 渲染，所有观感类结论必须标「未验证，需人工确认」。见 `CLAUDE.md` 的「你无法验证的东西」。
3. **egui 测试的跑法**：本仓库已有成熟范式——建 `egui::Context::default()`，`ctx.run(...)` **跑两帧**（第一帧让布局稳定，读第二帧输出），从 `FullOutput::shapes` 里递归收集 `Shape::Text` 的文本做断言。直接抄 `mod.rs` 的 `tunnel_ui_tests::run` / `all_text` / `has` 三个 helper，不要另起一套。

## 文件结构

| 文件 | 责任 | 本计划中的动作 |
|---|---|---|
| `crates/mullion-app/src/ui/session_manager/form.rs` | **新建**。表单骨架四件套：`section` / `grid` / `required` / `field_error` | Task 1 创建（纯搬迁） |
| `.../session_manager/fields.rs` | 会话编辑器四页的字段布局 | Task 1 删四个 fn 改 `use`；Task 5 协议行改只读 |
| `.../session_manager/tunnel_editor.rs` | 隧道表单 | Task 2 重排；Task 7 会话下拉过滤 |
| `.../session_manager/mod.rs` | 管理器骨架、模式条、键盘、意图施加 | Task 3 加档 + `protocol_of`；Task 4 键盘门；Task 5 `visible_tabs` + 新建草稿协议；Task 6 连接闸门 |
| `.../session_manager/list.rs` | 左栏会话列表 | Task 3 分流；Task 6 行内连接入口置灰 |
| `.../session_manager/editor.rs` | 右栏 Tab 骨架与按钮条 | Task 5 Tab 映射；Task 6 连接按钮置灰 |
| `.../session_manager/dedupe.rs` | 重复 / 同形判定（纯函数） | Task 8 `looks_same` 加协议 |
| `crates/mullion-app/tests/form_guidelines.rs` | **新建**。扫源码的机械守护 | Task 9 |
| `docs/ui-form-guidelines.md` | **新建**。表单布局规范 | Task 10 |
| `spec.md` | 需求唯一真源 | Task 11 |

---

### Task 1: 抽公共表单构件 `form.rs`（D8）

**纯搬迁，行为零改动。** 判据是：搬完之后 `cargo test -p mullion-app` 与搬之前一样绿，不多一条也不少一条。

**Files:**
- Create: `crates/mullion-app/src/ui/session_manager/form.rs`
- Modify: `crates/mullion-app/src/ui/session_manager/fields.rs`（删 18-101 行的四个 fn，加 `use`）
- Modify: `crates/mullion-app/src/ui/session_manager/mod.rs`（加 `mod form;`）

- [ ] **Step 1: 先记下基线（这一步不写代码）**

Run: `cargo test -p mullion-app 2>&1 | tail -3`
把 `test result: ok. N passed` 里的 **N 记下来**。搬迁后必须**一模一样**。这是本任务唯一的验收标准——纯搬迁没有新测试可写，用例数变化就说明搬错了。

- [ ] **Step 2: 创建 `form.rs`**

把 `fields.rs` 第 17~101 行（`grid` / `section` / `required` / `field_error` 四个函数**连同它们全部的文档注释**）原样剪切过来。那几段注释记录了「为什么 `first` 必须是页面级游标」「为什么不能用 `min_rect` 推断」等踩过的坑，**一个字都不许删**。

```rust
//! 表单骨架构件：分节标题、两列 Grid、必填星号、字段下的内联红字。
//!
//! 2026-08-11（F119）从 `fields.rs` 切出来。理由不是「文件太长」，而是
//! 隧道表单（`tunnel_editor.rs`）要用**同一套**骨架：没有共享构件，
//! 「标签列 88px」「分节前一条细线」这类规则就只能靠人看，下个切片照样漂。
//! `docs/ui-form-guidelines.md` 是这套构件的文字版，两者必须同时改。
//!
//! **搬迁时逻辑一字未改**，四个函数连同文档注释原样搬过来。

use egui::Ui;

use crate::theme::Theme;
use crate::ui::annotate;
use crate::ui::metrics::LABEL_COL_W;

// ↓↓↓ 以下四个函数从 fields.rs 原样搬入，仅把 `fn` 改成 `pub(super) fn` ↓↓↓

/// 两列表单的统一样式:左列标签定宽,右列输入撑满。
pub(super) fn grid(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    egui::Grid::new(id)
        .num_columns(2)
        .spacing([crate::ui::metrics::SP_M, crate::ui::metrics::SP_S])
        .min_col_width(LABEL_COL_W)
        .show(ui, add);
}

// section / required / field_error 同理：原样搬入，签名前加 pub(super)。
// （完整函数体见 fields.rs 搬迁前的 54-101 行，含全部文档注释）
```

- [ ] **Step 3: 改 `fields.rs`**

删掉刚搬走的四个 fn，在文件顶部 `use` 区加一行：

```rust
use super::form::{field_error, grid, required, section};
```

`fields.rs` 原第 9-11 行的 `use crate::ui::metrics::{...}` 里，若 `LABEL_COL_W` 搬走后不再被 `fields.rs` 使用，从这个 `use` 里删掉它（编译器会以 unused import 警告提示；`clippy -D warnings` 会把它变成错误，别放过）。

- [ ] **Step 4: 在 `mod.rs` 注册模块**

在 `crates/mullion-app/src/ui/session_manager/mod.rs` 的模块声明区（第 9-24 行那一串 `mod xxx;`）按字母序插入：

```rust
mod form;
```

- [ ] **Step 5: 验证「什么都没变」**

Run: `cargo test -p mullion-app 2>&1 | tail -3`
Expected: `test result: ok. N passed`，**N 与 Step 1 记下的数字完全相等**。

Run: `cargo clippy -p mullion-app --all-targets -- -D warnings`
Expected: 无输出。

- [ ] **Step 6: Commit**

```bash
git add crates/mullion-app/src/ui/session_manager/form.rs \
        crates/mullion-app/src/ui/session_manager/fields.rs \
        crates/mullion-app/src/ui/session_manager/mod.rs
git commit -m "refactor(ui): 表单骨架四件套抽出 form.rs,会话页与隧道页共用 (F119)

纯搬迁,逻辑一字未改:section/grid/required/field_error 连同文档注释
从 fields.rs 原样搬入。为 F119 的规范落到代码上准备唯一真源。
验收=测试用例数与搬迁前完全一致。"
```

---

### Task 2: 隧道表单重排（D9）

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/tunnel_editor.rs`（`show` 函数体，259-385 行）
- Test: 同文件 `#[cfg(test)] mod tests`

**只动排版。** 原样保留、一个字不许改的：`wording()`（标签随类型翻转，是「填反了就连不通」的第一道防线）、`expose_warning()`（`-L` 说风险点名目标 / `-R` 必须写明「无法验证」）、`build_tunnel_draft()` 的全部校验。它们各自的测试也不动。

- [ ] **Step 1: 写失败测试——端口报红的判据是纯函数**

在 `tunnel_editor.rs` 的 `mod tests` 里加：

```rust
/// 端口框什么时候该出红字。空着**不报**——必填星号已经说明了它必须填，
/// 一打开表单就红一片是在骂用户还没开始填。填了但非法才报。
/// 与会话页 `validate::port` 同一口径：不看「碰没碰过」，填错就是填错了。
#[test]
fn port_field_error_only_fires_when_something_invalid_was_typed() {
    assert!(!port_field_error(""), "空着不该报红");
    assert!(!port_field_error("   "), "只有空白也不该报红");
    assert!(!port_field_error("3306"), "合法端口不该报红");
    assert!(port_field_error("0"), "0 不可侦听，必须报红");
    assert!(port_field_error("70000"), "越界必须报红");
    assert!(port_field_error("abc"), "非数字必须报红");
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib tunnel_editor::tests::port_field_error -- --exact 2>&1 | tail -5`
Expected: 编译错误 `cannot find function \`port_field_error\` in this scope`

- [ ] **Step 3: 实现 `port_field_error`**

加在 `parse_port` 下面：

```rust
/// 端口框要不要出内联红字。空着不报（必填星号已经在说这件事），
/// 填了但解析不出合法端口才报。
pub(crate) fn port_field_error(text: &str) -> bool {
    !text.trim().is_empty() && parse_port(text, "端口").is_err()
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib tunnel_editor::tests::port_field_error -- --exact 2>&1 | tail -5`
Expected: `test result: ok. 1 passed`

- [ ] **Step 5: 重排 `show` 的表单体**

把 `show` 里从 `ui.horizontal(|ui| { ui.label("类型"); ...` 开始、到「说明」那一段为止（即 274-374 行）整段替换。按钮条（376-384 行）保持不动。

```rust
    // 页面级游标：整张表单共用一个，`section()` 靠它决定「首个分区不画线」。
    // 见 form.rs 里 `section` 的文档注释——**不许**在子块里另起 `let mut first = true`。
    let mut first = true;
    let w = wording(buf.kind);

    form::section(ui, t, "转发", &mut first);
    form::grid(ui, "tunnel_forward", |ui| {
        form::required(ui, t, "类型");
        egui::ComboBox::from_id_salt("tunnel_kind")
            .selected_text(buf.kind.label())
            .show_ui(ui, |ui| {
                for k in [
                    TunnelKindUi::Local,
                    TunnelKindUi::Remote,
                    TunnelKindUi::Dynamic,
                ] {
                    // 切类型**不清空** `listen_port`/`note`:那两个字段三种类型
                    // 都有,切一下就把用户填的端口抹掉是无谓的返工。
                    ui.selectable_value(&mut buf.kind, k, k.label());
                }
            });
        ui.end_row();

        form::required(ui, t, "经由会话");
        // 下拉候选与悬垂措辞在 Task 7 收口，这里先保持既有行为。
        let dangling = buf
            .session_id
            .is_some_and(|id| !sessions.iter().any(|s| s.id == id));
        let selected = match buf.session_id {
            Some(id) => match sessions.iter().find(|s| s.id == id) {
                Some(s) => format!(
                    "{} ({}@{})",
                    s.identity.name, s.auth.user, s.connection.host
                ),
                None => format!("⚠ 已删除的会话 (id={})", id.0),
            },
            None => "(未选择)".to_string(),
        };
        egui::ComboBox::from_id_salt("tunnel_session")
            .selected_text(selected)
            .show_ui(ui, |ui| {
                for s in sessions {
                    let label = format!(
                        "{} ({}@{})",
                        s.identity.name, s.auth.user, s.connection.host
                    );
                    ui.selectable_value(&mut buf.session_id, Some(s.id), label);
                }
            });
        ui.end_row();
        form::field_error(ui, t, dangling, "引用的会话已删除");
    });

    form::section(ui, t, "侦听", &mut first);
    form::grid(ui, "tunnel_listen", |ui| {
        form::required(ui, t, w.listen);
        ui.add(
            egui::TextEdit::singleline(&mut buf.listen_port)
                .desired_width(field_w(ui.available_width(), FIELD_W_S, 0.0)),
        );
        ui.end_row();
        form::field_error(
            ui,
            t,
            port_field_error(&buf.listen_port),
            "侦听端口要填 1~65535 之间的数字",
        );

        // 灰字说明占一整行，左格留空让它跟输入框左对齐（同 `field_error` 的理由：
        // 挂在标签列会被读成「这一行的标签」）。
        ui.label("");
        ui.colored_label(theme::c32(t.fg_dimmer), w.listen_hint);
        ui.end_row();

        // F117 绑定安全。`Dynamic` 这一段整块不画 —— 它在类型上就没有
        // `expose` 字段(见 `TunnelKind::Dynamic`),画一个写不进数据的勾选框
        // 只会让人以为动态转发也能对外开放。
        if buf.kind != TunnelKindUi::Dynamic {
            ui.label("");
            ui.checkbox(&mut buf.expose, w.expose);
            ui.end_row();
            // 警告里带**具体目标/端口**,不是泛泛一句「有风险」:用户要判断的是
            // 「把这台机器暴露出去要不要紧」,没有目标就没法判断。
            // 措辞与语气见纯函数 `expose_warning`(它自己有穷举测试)。
            if let Some((tone, text)) = expose_warning(buf) {
                let color = match tone {
                    ExposeTone::Danger => t.danger,
                    ExposeTone::Unverifiable => t.warn,
                };
                ui.label("");
                ui.colored_label(theme::c32(color), text);
                ui.end_row();
            }
        }
    });

    if buf.kind != TunnelKindUi::Dynamic {
        form::section(ui, t, "目标", &mut first);
        form::grid(ui, "tunnel_target", |ui| {
            form::required(ui, t, w.target);
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut buf.target_host)
                        .desired_width(field_w(ui.available_width(), FIELD_W_M, 0.0))
                        .hint_text(theme::hint_text(t, "主机名或 IP")),
                );
                ui.label(":");
                ui.add(
                    egui::TextEdit::singleline(&mut buf.target_port)
                        .desired_width(field_w(ui.available_width(), FIELD_W_S, 0.0)),
                );
            });
            ui.end_row();
            form::field_error(
                ui,
                t,
                port_field_error(&buf.target_port),
                "目标端口要填 1~65535 之间的数字",
            );

            ui.label("");
            ui.colored_label(theme::c32(t.fg_dimmer), w.resolver);
            ui.end_row();
        });
    }

    form::section(ui, t, "其他", &mut first);
    form::grid(ui, "tunnel_misc", |ui| {
        ui.label("说明");
        ui.add(
            egui::TextEdit::singleline(&mut buf.note)
                .desired_width(field_w(ui.available_width(), FIELD_W_L, 0.0))
                .hint_text(theme::hint_text(t, "这条转发是干什么的(可选)")),
        );
        ui.end_row();
    });

    ui.add_space(crate::ui::metrics::SP_M);
```

文件顶部 `use` 区补：

```rust
use crate::ui::metrics::{field_w, FIELD_W_L, FIELD_W_M, FIELD_W_S};
use crate::ui::session_manager::form;
```

- [ ] **Step 6: 写渲染断言——四个分节都在，且端口非法时有内联红字**

在 `mod tests` 里加（这个文件此前没有 egui 渲染测试，helper 要新建）：

```rust
    /// 跑两帧（第一帧让布局稳定），把第二帧画出的所有文字收集起来。
    fn rendered_texts(buf: TunnelEditorBuffer, sessions: &[SessionRecord]) -> Vec<String> {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut st = crate::ui::UiState {
            tunnel_editor: Some(buf),
            ..Default::default()
        };
        let mut texts = Vec::new();
        for _ in 0..2 {
            let out = ctx.run(Default::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(ui, &t, &mut st, sessions);
                });
            });
            texts = collect_text(&out.shapes);
        }
        texts
    }

    fn collect_text(shapes: &[egui::epaint::ClippedShape]) -> Vec<String> {
        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                egui::Shape::Text(ts) => out.push(ts.galley.text().to_string()),
                _ => {}
            }
        }
        let mut acc = Vec::new();
        shapes.iter().for_each(|cs| walk(&cs.shape, &mut acc));
        acc
    }

    fn has(texts: &[String], needle: &str) -> bool {
        texts.iter().any(|s| s.contains(needle))
    }

    /// 表单必须有视觉锚点。全部字段一路平铺下来时，眼睛找不到「这几行是一组」
    /// （走查 P2-17 的原话），这正是本次重排要解决的问题。
    ///
    /// 自证会变红：把任意一个 `form::section(...)` 调用删掉，这条当场红。
    #[test]
    fn the_form_is_grouped_into_named_sections() {
        let texts = rendered_texts(filled(TunnelKindUi::Local), &[]);
        for s in ["转发", "侦听", "目标", "其他"] {
            assert!(has(&texts, s), "分节「{s}」没画出来: {texts:?}");
        }
    }

    /// 动态转发没有目标，「目标」那一节整块不画——画一个写不进数据的框
    /// 只会让人以为 SOCKS5 也要填目标。
    #[test]
    fn dynamic_forwarding_has_no_target_section() {
        let texts = rendered_texts(filled(TunnelKindUi::Dynamic), &[]);
        assert!(has(&texts, "侦听"), "侦听节该在: {texts:?}");
        assert!(!has(&texts, "目标"), "动态转发不该画目标节: {texts:?}");
    }

    /// 端口填错了要**当场**在字段下方说，而不是等用户点了保存再从顶部
    /// 弹一条通知——那时用户的视线早已离开出错的那个框。
    #[test]
    fn an_invalid_port_is_reported_inline_under_the_field() {
        let mut buf = filled(TunnelKindUi::Local);
        buf.listen_port = "0".into();
        let texts = rendered_texts(buf, &[]);
        assert!(
            has(&texts, "侦听端口要填 1~65535"),
            "端口非法时没有内联红字: {texts:?}"
        );
    }
```

`use` 区补 `use mullion_store::SessionRecord;`（`mod tests` 内部）。

- [ ] **Step 7: 跑测试**

Run: `cargo test -p mullion-app --lib tunnel_editor 2>&1 | tail -5`
Expected: 全部 pass（含此前已有的 `expose_warning` / `build_tunnel_draft` 那批——它们**必须一条不少地继续绿**，绿不了说明动到了不该动的地方）。

- [ ] **Step 8: Commit**

```bash
git add crates/mullion-app/src/ui/session_manager/tunnel_editor.rs
git commit -m "feat(ui): 隧道表单按会话「连接」页重排:四分节 + 对齐标签列 + 内联红字 (F119)

只动排版:宽度换 field_w/FIELD_W_*、间距换 SP_*、加必填星号、
端口非法从「保存后顶部通知」改为字段下方内联红字。
wording()/expose_warning()/build_tunnel_draft() 一字未改,其测试全绿。

未验证:观感(分节线、标签对齐、端口框宽度)需 Windows 实机人眼确认。"
```

---

### Task 3: 模式条加 SFTP 档 + 按协议分流（D1、D2 前半）

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/mod.rs`（`ManagerMode`、`mode_bar`、`show` 里的分派）
- Modify: `crates/mullion-app/src/ui/session_manager/list.rs`（`visible_order`、`show`）
- Test: `mod.rs` 的 `mod tunnel_ui_tests`、`list.rs` 的 `mod tests`

- [ ] **Step 1: 写失败测试——`protocol_of` 的三档映射**

在 `mod.rs` 的 `mod tunnel_ui_tests` 里加：

```rust
    /// 分流的唯一真源。列表渲染和键盘顺序都走它，两边分叉就会出现
    /// 「按了下箭头，右栏换了一条陌生会话，左栏却没有任何行高亮」。
    #[test]
    fn each_mode_maps_to_exactly_one_protocol_or_none() {
        use mullion_store::Protocol;
        assert_eq!(protocol_of(ManagerMode::Sessions), Some(Protocol::Ssh));
        assert_eq!(protocol_of(ManagerMode::Sftp), Some(Protocol::Sftp));
        assert_eq!(
            protocol_of(ManagerMode::Tunnels),
            None,
            "隧道档不列会话，必须是 None——给它编一个协议会让键盘顺序走进会话列表"
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib each_mode_maps_to 2>&1 | tail -5`
Expected: 编译错误 `no variant named \`Sftp\`` / `cannot find function \`protocol_of\``

- [ ] **Step 3: 加档 + 真源函数**

`mod.rs` 的 `ManagerMode` 定义替换为：

```rust
/// 会话管理器的顶层模式(F116/F118)。三类对象左右栏整体切换,不共用列表 ——
/// 混在一个列表里会让「这一行是什么」需要读图标才知道。
///
/// `Sftp` 档列的是 `protocol == Sftp` 的**会话记录**(D1:数据层不分家,
/// schema 仍是 v7)。放在会话与隧道中间:它跟会话是同一类东西的两种协议,
/// 跟隧道不是。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ManagerMode {
    #[default]
    Sessions,
    Sftp,
    Tunnels,
}

/// 这一档该列哪种协议的会话记录。`Tunnels` 不列会话,故为 `None`。
///
/// **这是分流的唯一真源。** 列表渲染(`list::show`)与键盘顺序
/// (`list::visible_order`)必须都走它 —— 只过滤其中一侧,方向键会把选中态
/// 跳到当前页根本看不见的记录上。
pub(crate) fn protocol_of(mode: ManagerMode) -> Option<mullion_store::Protocol> {
    use mullion_store::Protocol;
    match mode {
        ManagerMode::Sessions => Some(Protocol::Ssh),
        ManagerMode::Sftp => Some(Protocol::Sftp),
        ManagerMode::Tunnels => None,
    }
}
```

`mode_bar` 的表加一项：

```rust
        for (mode, label) in [
            (ManagerMode::Sessions, "会话"),
            (ManagerMode::Sftp, "SFTP"),
            (ManagerMode::Tunnels, "隧道"),
        ] {
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib each_mode_maps_to 2>&1 | tail -5`
Expected: `test result: ok. 1 passed`（其余地方此时会因 `match` 不穷尽而编译失败——下一步修）

- [ ] **Step 5: 写失败测试——分流后两页各列各的**

在 `mod tunnel_ui_tests` 里加。先给 `sess` 补一个能造 SFTP 记录的姊妹函数：

```rust
    fn sftp_sess(id: u64, name: &str) -> SessionRecord {
        let mut s = sess(id, name);
        s.connection.protocol = mullion_store::Protocol::Sftp;
        s
    }

    /// SFTP 档列 SFTP 节点，会话档列 SSH 会话，互不串台。
    ///
    /// 自证会变红：把 `list::show`/`visible_order` 里的协议 filter 去掉，
    /// 两个方向的断言各红一条。
    #[test]
    fn each_page_lists_only_its_own_protocol() {
        let sessions = vec![sess(1, "生产主控"), sftp_sess(2, "文件中转")];

        let (_c, out) = run(&mut open(ManagerMode::Sessions), &sessions, &[]);
        let texts = all_text(&out.shapes);
        assert!(has(&texts, "生产主控"), "会话页没画 SSH 会话: {texts:?}");
        assert!(!has(&texts, "文件中转"), "会话页混进了 SFTP 节点: {texts:?}");

        let (_c, out) = run(&mut open(ManagerMode::Sftp), &sessions, &[]);
        let texts = all_text(&out.shapes);
        assert!(has(&texts, "文件中转"), "SFTP 页没画 SFTP 节点: {texts:?}");
        assert!(!has(&texts, "生产主控"), "SFTP 页混进了 SSH 会话: {texts:?}");
    }

    /// 键盘顺序与渲染必须是**同一份**集合。这条钉的是「按 ↓ 跳到看不见的行」
    /// ——症状是右栏表单换了一条陌生会话，左栏却没有任何一行高亮。
    ///
    /// 自证会变红：只给 `list::show` 加 filter、不给 `visible_order` 加。
    #[test]
    fn keyboard_order_never_contains_a_row_the_page_does_not_render() {
        use mullion_store::Protocol;
        let sessions = vec![sess(1, "生产主控"), sftp_sess(2, "文件中转")];
        let order = list::visible_order(&sessions, &[], "", Protocol::Ssh);
        assert_eq!(
            order,
            vec![SessionId(1)],
            "SSH 页的键盘顺序里不该有 SFTP 节点"
        );
        let order = list::visible_order(&sessions, &[], "", Protocol::Sftp);
        assert_eq!(order, vec![SessionId(2)]);
    }
```

- [ ] **Step 6: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib each_page_lists_only 2>&1 | tail -5`
Expected: 编译错误（`visible_order` 参数个数不对）

- [ ] **Step 7: 实现分流**

`list.rs` 加一个合并判据（**关键**：所有过滤点必须走这一个函数）：

```rust
/// 这条记录在当前页可见吗 —— 协议 + 搜索词，两个判据成对出现。
///
/// 只判搜索会让另一档协议的记录漏进来；只判协议会让搜索失效。
/// `visible_order`（键盘顺序）与 `show`（渲染）都走这一个函数，
/// 是「方向键跳到看不见的行」那条失效模式的唯一防线 —— 谁再加一个
/// 过滤点，也必须调它，不许在别处重写这个条件。
fn on_page(rec: &SessionRecord, query: &str, protocol: mullion_store::Protocol) -> bool {
    rec.connection.protocol == protocol && matches(rec, query)
}
```

`visible_order` 改：

```rust
pub(crate) fn visible_order(
    sessions: &[SessionRecord],
    groups: &[GroupRecord],
    query: &str,
    protocol: mullion_store::Protocol,
) -> Vec<SessionId> {
    crate::ui::group_manager::group_sessions(groups, sessions)
        .into_iter()
        .flat_map(|(_, bucket)| bucket)
        .filter(|r| on_page(r, query, protocol))
        .map(|r| r.id)
        .collect()
}
```

`list::show` 签名末尾加 `protocol: mullion_store::Protocol`，并改三个过滤点：

1. 空搜索结果判定（原 582 行）：
```rust
    if searching && !sessions.iter().any(|r| on_page(r, &ui_state.search, protocol)) {
```
2. 归桶后的 `matched`（原 598 行起的循环内）：把 `.filter(|r| matches(r, &ui_state.search))` 换成 `.filter(|r| on_page(r, &ui_state.search, protocol))`。
3. 「没有匹配的会话」那句文案在 SFTP 页要说得对——**保持原文不动**（"会话" 在这里是通称，SFTP 节点也是会话记录）。

`mod.rs` 的 `SidePanel` 分派改：

```rust
                .show_inside(ui, |ui| match ui_state.manager_mode {
                    ManagerMode::Sessions | ManagerMode::Sftp => list::show(
                        ui,
                        t,
                        ui_state,
                        sessions,
                        groups,
                        tunnels,
                        tunnel_states,
                        appearance,
                        // 上面 `match` 的两个分支都保证 `protocol_of` 有值。
                        protocol_of(ui_state.manager_mode).expect("会话/SFTP 档必有协议"),
                    ),
                    ManagerMode::Tunnels => {
                        tunnel_list::show(ui, t, ui_state, tunnels, tunnel_states, sessions)
                    }
                });
```

`CentralPanel` 的分派同理把 `ManagerMode::Sessions` 改成 `ManagerMode::Sessions | ManagerMode::Sftp`（`editor::show` 的参数在 Task 5 才加，这一步先原样调用）。

`list.rs` 自己的测试 helper（`run_list_sized` / `run_list_with` 等，约 8 处调用 `list::show`）都要补最后一个参数 `mullion_store::Protocol::Ssh`。

- [ ] **Step 8: 跑测试**

Run: `cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED|panicked" | tail -5`
Expected: 全绿。

- [ ] **Step 9: Commit**

```bash
git add crates/mullion-app/src/ui/session_manager/mod.rs \
        crates/mullion-app/src/ui/session_manager/list.rs
git commit -m "feat(ui): 会话管理器加 SFTP 档,列表与键盘顺序按 protocol 分流 (F118)

模式条改三档「会话|SFTP|隧道」。分流真源是纯函数 protocol_of;
list::show 与 visible_order 共用 on_page() 判据 —— 只过滤渲染那一侧,
方向键会把选中态跳到当前页看不见的记录上。
schema 不动(仍 v7),SFTP 节点就是 protocol=Sftp 的会话记录。"
```

---

### Task 4: 键盘动作的模式门（D2 后半）

**这条修的是既有漏洞**：`Prev/Next/Open/Tab` 四个动作目前没有任何模式判断，站在隧道页按 `↑/↓` 走的仍是会话列表的顺序。

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/mod.rs`（`show` 里 359-394 行的 `for action in ...` 循环）
- Test: `mod.rs` 的 `mod tunnel_ui_tests`

- [ ] **Step 1: 写失败测试（必须在改动前先红）**

```rust
    /// 隧道页没有会话可切、可连。不加这道门的话，站在隧道页按 ↑↓ 会对一条
    /// **看不见**的会话发 `pending_switch`（会话表单脏着时还会凭空弹出
    /// 「有未保存的更改」确认框），按 Enter 会连接一条看不见的会话。
    ///
    /// 这是 F116 引入模式条时漏的门，本测试在修复前必须先红。
    #[test]
    fn tunnel_mode_ignores_session_keyboard_actions() {
        let sessions = vec![sess(1, "生产主控")];
        let mut st = open(ManagerMode::Tunnels);

        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::Key {
            key: egui::Key::ArrowDown,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        });
        input.events.push(egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        });
        let _ = ctx.run(input, |ctx| {
            show(
                ctx,
                &t,
                &mut st,
                &sessions,
                &[],
                &[],
                &[],
                true,
                SecretPresence::default(),
                &crate::ui::badge::AppearanceCache::default(),
            );
        });

        assert!(
            st.pending_switch.is_none(),
            "隧道页按 ↓ 不该切会话，实际: {:?}",
            st.pending_switch.is_some()
        );
        assert!(
            st.connect_request.is_none(),
            "隧道页按 Enter 不该发起连接，实际: {:?}",
            st.connect_request
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib tunnel_mode_ignores_session_keyboard 2>&1 | tail -8`
Expected: FAIL，`隧道页按 ↓ 不该切会话`（证明漏洞真实存在）。

若这一步意外地绿了，**停下来查清楚为什么**——多半是测试没真的把按键喂进去（`ctx.run` 只跑了一帧、或 `typing` 判定把按键吞了），而不是代码本来就对。

- [ ] **Step 3: 加模式门**

把 `show` 里 `keys::Action` 的三个分支改成：

```rust
            keys::Action::Prev | keys::Action::Next => {
                // 隧道页没有会话列表可走。不判模式的话，站在隧道页按 ↑↓ 会对
                // 一条看不见的会话发 pending_switch —— 会话表单脏着时还会凭空
                // 弹出「有未保存的更改」确认框，用户完全不知道自己动了什么。
                let Some(protocol) = protocol_of(ui_state.manager_mode) else {
                    continue;
                };
                let order = list::visible_order(sessions, groups, &ui_state.search, protocol);
                let forward = action == keys::Action::Next;
                if let Some(id) = keys::step(&order, ui_state.editor_id, forward) {
                    // 走 `pending_switch` 而不是直接换 `editor`:表单脏的时候
                    // 要弹确认,这套机制已经在那条路上了(见本文件下方的消费点)。
                    ui_state.pending_switch = Some(SwitchTarget::Session(id));
                }
            }
            keys::Action::Open => {
                // 只有会话档能连。SFTP 节点连不了(F50 未实现,见 D4 的统一
                // 闸门),隧道档压根没有会话可连。
                if ui_state.manager_mode == ManagerMode::Sessions {
                    if let Some(id) = ui_state.editor_id {
                        ui_state.connect_request = Some(id);
                    }
                }
            }
            keys::Action::Tab(n) => {
                // 隧道档的右栏不是 Tab 编辑器;没在编辑任何会话时右栏是空态,
                // 切页也没有意义。
                // SFTP 档少一页(D5),`n` 是**位置序号**不是 Tab 下标,
                // 必须过 `visible_tabs` 映射 —— 直接写 `editor_tab = n`
                // 会让 Ctrl+3 打开「登录后」而 Tab 条上高亮的是「图标」。
                if ui_state.manager_mode != ManagerMode::Tunnels && ui_state.editor.is_some() {
                    if let Some(&tab) = visible_tabs(ui_state.manager_mode).get(n) {
                        ui_state.editor_tab = tab;
                    }
                }
            }
```

`visible_tabs` 在 Task 5 才定义——**本任务先只改前两个分支**，`Tab(n)` 分支保持原样（`ui_state.editor_tab = n`），Task 5 再回来改成上面这段。这样每个任务都能独立编译通过。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib tunnel_mode_ignores_session_keyboard 2>&1 | tail -5`
Expected: `test result: ok. 1 passed`

- [ ] **Step 5: 跑全量确认没碰坏既有键盘行为**

Run: `cargo test -p mullion-app --lib session_manager 2>&1 | grep -E "test result|FAILED" | tail -3`
Expected: 全绿。

- [ ] **Step 6: Commit**

```bash
git add crates/mullion-app/src/ui/session_manager/mod.rs
git commit -m "fix(ui): 键盘动作补模式门,隧道页 ↑↓/Enter 不再操作看不见的会话 (F116/F118)

F116 引入模式条时漏的:Prev/Next/Open 没有任何模式判断,站在隧道页
按 ↓ 会对一条看不见的会话发 pending_switch(会话表单脏着还会凭空弹
确认框),按 Enter 会连一条看不见的会话。守护测试在修复前先红。"
```

---

### Task 5: Tab 映射 + 协议只读 + 新建草稿带模式协议（D3、D5）

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/mod.rs`（`visible_tabs`、`apply_switch`、`Tab(n)` 分支、`CentralPanel` 调用）
- Modify: `crates/mullion-app/src/ui/session_manager/editor.rs`（`show` 加 `mode` 参数、Tab 条循环、clamp）
- Modify: `crates/mullion-app/src/ui/session_manager/fields.rs`（协议行改只读）

- [ ] **Step 1: 写失败测试——Tab 集与下标映射**

在 `mod tunnel_ui_tests` 里加：

```rust
    /// SFTP 节点不画「登录后」：那一页是发 shell 命令与 tmux 附着（F40~F44），
    /// 对 SFTP 没有落点，画出来等于让用户配一堆永远不会执行的东西。
    ///
    /// 返回的是 **`TABS` 的原始下标**而不是重新编号。`TAB_*` 是下标常量：
    /// 用 `enumerate()` 的位置序号去写 `editor_tab`，点「图标」会打开「登录后」。
    #[test]
    fn sftp_hides_the_automation_tab_and_keeps_original_indices() {
        assert_eq!(
            visible_tabs(ManagerMode::Sftp),
            &[TAB_CONNECT, TAB_AUTH, TAB_APPEARANCE],
            "SFTP 档必须是这三页，且用的是原始下标"
        );
        assert_eq!(
            visible_tabs(ManagerMode::Sessions),
            &[TAB_CONNECT, TAB_AUTH, TAB_AUTOMATION, TAB_APPEARANCE]
        );
    }

    /// D5 防漂移：必填项只落在「连接」「认证」两页，两页在 SFTP 档都在。
    /// 将来有人给「登录后」加必填项时，这条会先红 —— 那时才需要处理
    /// 「校验要把用户导向一页看不见的 Tab」。
    #[test]
    fn every_reachable_validation_target_is_visible_in_sftp_mode() {
        let all_missing = super::validate::Missing {
            name: true,
            host: true,
            user: true,
        };
        let visible = visible_tabs(ManagerMode::Sftp);
        for m in [
            all_missing,
            super::validate::Missing {
                name: false,
                host: false,
                user: true,
            },
        ] {
            if let Some(tab) = m.tab() {
                assert!(
                    visible.contains(&tab),
                    "校验会把用户导向 SFTP 档看不见的 Tab {tab}"
                );
            }
        }
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib sftp_hides_the_automation 2>&1 | tail -5`
Expected: 编译错误 `cannot find function \`visible_tabs\``

- [ ] **Step 3: 实现 `visible_tabs`**

加在 `mod.rs` 的 `TAB_*` 常量下面：

```rust
/// 这个模式下右栏画哪几页，元素是 `editor::TABS` 的**原始下标**。
///
/// SFTP 节点不画「登录后」(F40~F44 是发 shell 命令与 tmux 附着，对 SFTP
/// 没有落点)。隧道档不走这里 —— 它的右栏根本不是 Tab 编辑器。
///
/// **必须返回原始下标。** `TAB_CONNECT..TAB_APPEARANCE` 是下标常量
/// (`editor.rs` 头上写明这是既有技术债)，隐藏中间一页之后若用
/// `TABS.iter().enumerate()` 的位置序号去写 `editor_tab`，第三个画出来的是
/// 「图标」但下标是 2，点下去打开的是「登录后」。
pub(crate) fn visible_tabs(mode: ManagerMode) -> &'static [usize] {
    match mode {
        ManagerMode::Sftp => &[TAB_CONNECT, TAB_AUTH, TAB_APPEARANCE],
        _ => &[TAB_CONNECT, TAB_AUTH, TAB_AUTOMATION, TAB_APPEARANCE],
    }
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib sftp_hides_the_automation 2>&1 | tail -5`
Expected: `test result: ok. 1 passed`（第二条 `every_reachable_validation_target` 也应绿）

- [ ] **Step 5: 写失败测试——点第三个 Tab 打开的是「图标」**

```rust
    /// D5 的下标陷阱。SFTP 档 Tab 条上第三个是「图标」，点它必须打开「图标」。
    ///
    /// 自证会变红：把 `editor.rs` 的 Tab 循环改回 `TABS.iter().enumerate()`，
    /// 这条立刻红（`editor_tab` 会变成 2 = 登录后）。
    #[test]
    fn clicking_the_third_tab_in_sftp_mode_opens_appearance_not_automation() {
        let sessions = vec![sftp_sess(1, "文件中转")];
        let mut st = open(ManagerMode::Sftp);
        st.editor_id = Some(SessionId(1));
        st.editor = Some(EditorBuffer::from_record(&sessions[0]));
        st.editor_baseline = st.editor.clone();

        // 先跑两帧把 Tab 画出来，再用真实指针事件点「图标」那一格
        // （位置从渲染出的文字锚点反查）。
        let (ctx, _out) = run(&mut st, &sessions, &[]);
        let pos = find_text_pos(&ctx, "图标").expect("Tab「图标」没画出来");
        let mut input = egui::RawInput::default();
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
        let t = crate::theme::MULLION_DARK;
        let _ = ctx.run(input, |c| {
            show(
                c,
                &t,
                &mut st,
                &sessions,
                &[],
                &[],
                &[],
                true,
                SecretPresence::default(),
                &crate::ui::badge::AppearanceCache::default(),
            );
        });

        assert_eq!(
            st.editor_tab, TAB_APPEARANCE,
            "点 SFTP 档第三个 Tab「图标」，打开的却是下标 {} 那一页",
            st.editor_tab
        );
    }

    /// 反查一段文字画在哪儿，用来给点击事件定位（`editor.rs` 的测试里已有
    /// 同款做法，这里按本模块需要重写一份最小版）。
    fn find_text_pos(ctx: &egui::Context, needle: &str) -> Option<egui::Pos2> {
        let mut found = None;
        ctx.graphics(|g| {
            for (_, layer) in g.iter() {
                for cs in layer.all_entries() {
                    fn walk(shape: &egui::Shape, needle: &str, out: &mut Option<egui::Pos2>) {
                        match shape {
                            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, needle, out)),
                            egui::Shape::Text(ts) => {
                                if out.is_none() && ts.galley.text().contains(needle) {
                                    *out = Some(ts.pos + ts.galley.size() / 2.0);
                                }
                            }
                            _ => {}
                        }
                    }
                    walk(&cs.1.shape, needle, &mut found);
                }
            }
        });
        found
    }
```

> **实现者注意**：`ctx.graphics(...)` 的具体读法在 egui 0.30 里可能与上面写法有出入。**先查实际签名**（`~/.cargo/registry/src/**/egui-0.30.0/src/context.rs`），别硬套。`editor.rs` 的 `mod tests` 里已有 `find_text_pos_exact` / `find_galley_job` 两个可直接参考的实现——**优先照抄那两个**，本文件只是给出意图。

- [ ] **Step 6: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib clicking_the_third_tab 2>&1 | tail -8`
Expected: FAIL，`editor_tab` 是 2（`TAB_AUTOMATION`）而不是 3。

- [ ] **Step 7: 改 `editor::show` 的 Tab 循环**

`editor::show` 签名末尾加参数：

```rust
pub(super) fn show(
    ui: &mut Ui,
    t: &Theme,
    ui_state: &mut UiState,
    groups: &[GroupRecord],
    sessions: &[SessionRecord],
    presence: SecretPresence,
    mode: super::ManagerMode,
) {
```

在 `let focus_name = ...` 之前插入 clamp：

```rust
    // 切到 SFTP 档时若还停在「登录后」，把它拉回「连接」——那一页在这个模式下
    // 不画 Tab，留着会让内容区画着一页、而 Tab 条上没有任何一个高亮。
    let tabs = super::visible_tabs(mode);
    if !tabs.contains(&ui_state.editor_tab) {
        ui_state.editor_tab = super::TAB_CONNECT;
    }
```

Tab 条循环改（原 380 行）：

```rust
    let tab_bar = ui.horizontal(|ui| {
        for &i in tabs {
            let name = TABS[i];
```

循环体内 `badge_of(i, missing, buf)`、`ui_state.editor_tab == i`、`ui_state.editor_tab = i` **全部保持用 `i`**（它现在是原始下标，正是想要的）。

`mod.rs` 的 `CentralPanel` 调用补参数：

```rust
                        ManagerMode::Sessions | ManagerMode::Sftp => {
                            editor::show(ui, t, ui_state, groups, sessions, presence, ui_state.manager_mode)
                        }
```

> 借用冲突提示：`ui_state` 已被可变借出，不能在同一表达式里再读 `ui_state.manager_mode`。在 `match` **之前**先 `let mode = ui_state.manager_mode;`（`ManagerMode` 是 `Copy`），再传 `mode`。

同时把 Task 4 Step 3 里留着没改的 `Tab(n)` 分支换成带 `visible_tabs` 映射的版本。

- [ ] **Step 8: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib clicking_the_third_tab 2>&1 | tail -5`
Expected: `test result: ok. 1 passed`

- [ ] **Step 9: 写失败测试——协议只读 + 新建草稿带模式协议**

```rust
    /// D3：协议只读。可改的话，一条记录会在保存那一刻从当前列表消失、跑到
    /// 另一页去（用户看到的是「我点了保存，会话没了」），而引用它的隧道会
    /// 当场变成「经由一个 SFTP 节点」。要换协议就新建一条。
    #[test]
    fn the_protocol_field_is_not_editable_in_either_mode() {
        let sessions = vec![sess(1, "生产主控")];
        let mut st = open(ManagerMode::Sessions);
        st.editor_id = Some(SessionId(1));
        st.editor = Some(EditorBuffer::from_record(&sessions[0]));
        st.editor_baseline = st.editor.clone();
        let (_c, out) = run(&mut st, &sessions, &[]);
        let texts = all_text(&out.shapes);
        assert!(has(&texts, "ssh"), "协议值该显示出来: {texts:?}");
        assert!(
            !has(&texts, "sftp(未实现)"),
            "协议下拉的候选项还在，说明它仍可改: {texts:?}"
        );
    }

    /// 在哪一档按「+ 新建」，建出来的就是哪一类节点。协议此后只读（D3），
    /// 所以这是它唯一的决定点 —— 漏了这里，SFTP 页新建的节点会是 ssh，
    /// 保存后当场从 SFTP 页消失。
    #[test]
    fn new_draft_takes_its_protocol_from_the_current_mode() {
        use mullion_store::Protocol;
        for (mode, want) in [
            (ManagerMode::Sessions, Protocol::Ssh),
            (ManagerMode::Sftp, Protocol::Sftp),
        ] {
            let mut st = open(mode);
            st.pending_switch = Some(SwitchTarget::NewDraft);
            apply_switch(&mut st, &[]);
            assert_eq!(
                st.editor.as_ref().map(|b| b.protocol),
                Some(want),
                "{mode:?} 档新建的草稿协议不对"
            );
        }
    }
```

- [ ] **Step 10: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib new_draft_takes_its_protocol 2>&1 | tail -5`
Expected: FAIL（SFTP 档建出来的是 `Ssh`）

- [ ] **Step 11: 实现两处**

`mod.rs` 的 `apply_switch`，`NewDraft` 分支：

```rust
        SwitchTarget::NewDraft => {
            // 走查 21:用户名预填成当前系统账号,光标落到「名称」上。
            let mut draft = EditorBuffer::new_draft();
            // D1/D3：在哪一档按「+ 新建」，建出来的就是哪一类节点。协议此后
            // 只读，所以这是它唯一的决定点。
            if let Some(p) = protocol_of(ui_state.manager_mode) {
                draft.protocol = p;
            }
            ui_state.editor = Some(draft);
            ui_state.editor_id = None;
            ui_state.focus_name_request = true;
        }
```

`fields.rs` 的 `basic()`，协议那一行（原 326-353 行）整段替换：

```rust
        ui.label("协议");
        // D3：只读。可改的话，一条记录会在保存那一刻从当前列表消失、跑到另一
        // 页去，而引用它的隧道会当场变成「经由一个 SFTP 节点」——那条隧道
        // 昨天还是好的。要换协议就新建一条（同 D1 接受的代价）。
        //
        // 原来这里是个能选 sftp 的下拉 + 一句「sftp 尚未实现，连接会按 ssh
        // 处理」。那条路本来就通向 `SftpNotSupported`，现在 SFTP 节点有自己
        // 的一档（F118），这个下拉连同那句话一起下线。
        ui.label(match buf.protocol {
            Protocol::Ssh => "ssh",
            Protocol::Sftp => "sftp",
        });
        ui.end_row();
```

若 `inherit_row` 的 `use` 因此变成未使用，按编译器提示清理（`fields.rs` 别处还在用 `inherit_row::slot`，多半仍需保留）。

- [ ] **Step 12: 跑测试**

Run: `cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED|panicked" | tail -5`
Expected: 全绿。若 `fields.rs` 里有测试断言过「sftp(未实现)」这段文案，**把那条测试一并更新**（它测的行为已被 D3 显式取消），并在 commit message 里点名。

- [ ] **Step 13: Commit**

```bash
git add crates/mullion-app/src/ui/session_manager/mod.rs \
        crates/mullion-app/src/ui/session_manager/editor.rs \
        crates/mullion-app/src/ui/session_manager/fields.rs
git commit -m "feat(ui): SFTP 档隐掉「登录后」页 + 协议字段只读 + 新建草稿随档取协议 (F118)

Tab 走 visible_tabs 的原始下标映射:TAB_* 是下标常量,隐藏中间一页后
用 enumerate 的位置序号会让点「图标」打开「登录后」,守护测试钉住。
协议改只读(D3):可改会让记录在保存那一刻从当前页消失、并让引用它的
隧道变成「经由 SFTP 节点」。原「sftp(未实现)」下拉选项随之下线。"
```

---

### Task 6: 连接入口统一闸门 + 视觉置灰（D4）

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/editor.rs`（`Disabled` 枚举、`why`、`tip`）
- Modify: `crates/mullion-app/src/ui/session_manager/list.rs`（行双击 / 右键菜单）
- Modify: `crates/mullion-app/src/ui/session_manager/mod.rs`（统一闸门）

**为什么要「统一闸门 + 视觉置灰」两层**：左栏双击、右键菜单、右栏两个按钮、Enter 键是**四条独立的路**。逐条挡容易漏（本项目已经在 P2-c 切片上栽过两次「闸门覆盖缺口」）。做法是：`mod.rs` 里一道兜底闸门保证**行为**一定不发生，各入口再各自置灰保证**用户看得懂**为什么点不动。

- [ ] **Step 1: 写失败测试**

在 `mod tunnel_ui_tests` 里加：

```rust
    /// D4：SFTP 传输还没实现（F50），点了也连不上。四条入口（左栏双击、
    /// 右键菜单、右栏按钮、Enter）都要挡住 —— 这里测的是兜底闸门：
    /// 无论哪条路把 connect_request 写了进来，出了 show() 都必须是 None。
    ///
    /// 自证会变红：把 mod.rs 里那段闸门删掉。
    #[test]
    fn no_connect_request_survives_outside_session_mode() {
        let sessions = vec![sess(1, "生产主控"), sftp_sess(2, "文件中转")];
        for mode in [ManagerMode::Sftp, ManagerMode::Tunnels] {
            let mut st = open(mode);
            // 模拟「某条入口已经把意图写了进来」。
            st.connect_request = Some(SessionId(2));
            st.connect_skip_automation = true;
            let _ = run(&mut st, &sessions, &[]);
            assert!(
                st.connect_request.is_none(),
                "{mode:?} 档不该放行连接意图"
            );
            assert!(
                !st.connect_skip_automation,
                "{mode:?} 档的跳过自动化标志也要一并清掉，否则它会漂到下一次真连接上"
            );
        }

        // 反面：会话档必须照常放行，否则这道闸门就把正常功能一起挡了。
        let mut st = open(ManagerMode::Sessions);
        st.connect_request = Some(SessionId(1));
        let _ = run(&mut st, &sessions, &[]);
        assert_eq!(
            st.connect_request,
            Some(SessionId(1)),
            "会话档的连接意图被误挡了"
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib no_connect_request_survives 2>&1 | tail -8`
Expected: FAIL（SFTP 档的 `connect_request` 还在）

- [ ] **Step 3: 加统一闸门**

在 `mod.rs` 的 `show()` 里、`Window::show` 闭包**结束之后**（借用已释放处，与既有的 `tunnel_save_click` 消费段同一区域）加：

```rust
    // D4 统一闸门。SFTP 节点连不上(F50 未实现)、隧道档压根没有会话可连。
    // 入口有四条(左栏双击、右键菜单、右栏按钮、Enter),逐条挡必然漏 ——
    // 这里做唯一一道兜底,各入口再各自置灰(那是给人看的,这一道是保证行为)。
    // `connect_skip_automation` 必须一起清:留着它会漂到下一次真正的连接上,
    // 用户会莫名其妙地跳过一次自动化。
    if ui_state.manager_mode != ManagerMode::Sessions {
        ui_state.connect_request = None;
        ui_state.connect_skip_automation = false;
    }
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib no_connect_request_survives 2>&1 | tail -5`
Expected: `test result: ok. 1 passed`

- [ ] **Step 5: 右栏按钮置灰**

`editor.rs` 的 `Disabled` 枚举加一档：

```rust
/// 底部按钮为什么点不动。原因是并集，顺序即优先级 ——
/// `Sftp` 排最前：节点填齐了也连不了，这时说「还缺主机」是误导。
enum Disabled {
    No,
    /// SFTP 传输还没实现(F50)，节点管理可用但连不上。
    Sftp,
    Missing(String),
    Probing,
}

/// SFTP 节点为什么连不上。两处要用同一份文案（右栏按钮、左栏右键菜单），
/// 所以留一个常量，别各写各的。
pub(super) const SFTP_NOT_YET: &str = "SFTP 传输尚未实现（F50）";

fn why(
    mode: super::ManagerMode,
    missing: super::validate::Missing,
    probe: &super::ProbeState,
) -> Disabled {
    if matches!(mode, super::ManagerMode::Sftp) {
        Disabled::Sftp
    } else if missing.any() {
        Disabled::Missing(missing.hint())
    } else if matches!(probe, super::ProbeState::Running) {
        Disabled::Probing
    } else {
        Disabled::No
    }
}

fn tip(d: &Disabled) -> Option<String> {
    match d {
        Disabled::No => None,
        Disabled::Sftp => Some(SFTP_NOT_YET.to_owned()),
        Disabled::Missing(h) => Some(h.clone()),
        Disabled::Probing => Some("测试连接进行中…".to_owned()),
    }
}
```

`show` 里 `let reason = why(missing, &ui_state.probe);` 改成 `let reason = why(mode, missing, &ui_state.probe);`。

`editor.rs` 里既有的 `why` / `tip` 单测（`why_is_no_when_nothing_missing_and_probe_idle` 等 5 条）要补 `mode` 参数，一律传 `ManagerMode::Sessions`；再加一条：

```rust
    /// SFTP 优先于其它原因：节点填齐了也连不上，说「还缺主机」是在指错方向。
    #[test]
    fn sftp_outranks_every_other_disabled_reason() {
        let missing = super::super::validate::Missing {
            name: true,
            host: true,
            user: true,
        };
        let d = why(
            super::super::ManagerMode::Sftp,
            missing,
            &super::super::ProbeState::Running,
        );
        assert_eq!(tip(&d).as_deref(), Some(SFTP_NOT_YET));
    }
```

- [ ] **Step 6: 左栏行内入口置灰**

`list::show` 里，把 `protocol` 换算成一个布尔量传给 `row`：

```rust
    // SFTP 节点连不上(F50)。行为由 mod.rs 的统一闸门保证；这里管的是
    // 「让用户看得出来为什么点不动」。
    let connectable = protocol == mullion_store::Protocol::Ssh;
```

`row()` 签名加 `connectable: bool`，函数体两处改：

```rust
    if connectable && resp.double_clicked() {
        ui_state.connect_request = Some(rec.id);
    }
    resp.context_menu(|ui| {
        if ui
            .add_enabled(connectable, egui::Button::new("连接"))
            .on_disabled_hover_text(super::editor::SFTP_NOT_YET)
            .clicked()
        {
            ui_state.connect_request = Some(rec.id);
            ui.close_menu();
        }
        if ui
            .add_enabled(connectable, egui::Button::new("连接(跳过自动化)"))
            .on_disabled_hover_text(super::editor::SFTP_NOT_YET)
            .clicked()
        {
            ui_state.connect_request = Some(rec.id);
            ui_state.connect_skip_automation = true;
            ui.close_menu();
        }
```

`list.rs` 里所有 `row(...)` 调用点补这个参数。

- [ ] **Step 7: 跑全量**

Run: `cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED|panicked" | tail -5`
Expected: 全绿。

- [ ] **Step 8: Commit**

```bash
git add crates/mullion-app/src/ui/session_manager/
git commit -m "feat(ui): SFTP 档连接入口置灰 + 一道兜底闸门 (F118)

连接有四条入口(左栏双击、右键菜单、右栏按钮、Enter),逐条挡必然漏
(P2-c 已栽过两次「闸门覆盖缺口」)。做法:mod.rs 一道兜底保证行为、
各入口置灰保证用户看得懂。connect_skip_automation 一并清 —— 留着会
漂到下一次真连接上。"
```

---

### Task 7: 隧道「经由会话」只列 SSH（D6）

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/tunnel_editor.rs`

- [ ] **Step 1: 写失败测试**

```rust
    /// D6：隧道走的是 `direct-tcpip` / `tcpip-forward`，跟 SFTP 节点没关系。
    /// 候选里列出 SFTP 节点，用户选了之后隧道永远起不来，而错误要等到启动
    /// 那一刻才出现。
    #[test]
    fn session_ref_label_distinguishes_deleted_from_sftp() {
        let ssh = sess_rec(1, "生产主控", mullion_store::Protocol::Ssh);
        let sftp = sess_rec(2, "文件中转", mullion_store::Protocol::Sftp);
        let all = vec![ssh.clone(), sftp.clone()];

        let (text, err) = session_ref_label(Some(SessionId(1)), &all);
        assert!(text.contains("生产主控"), "实际: {text}");
        assert!(err.is_none(), "正常引用不该报错");

        let (text, err) = session_ref_label(Some(SessionId(9)), &all);
        assert!(text.contains("已删除"), "实际: {text}");
        assert_eq!(err, Some("引用的会话已删除"));

        // 「删掉了」和「是 SFTP 节点」是两种完全不同的修法：前者要重建会话，
        // 后者只需改选一条 SSH 会话。合成一句话等于让用户猜。
        let (text, err) = session_ref_label(Some(SessionId(2)), &all);
        assert!(text.contains("文件中转"), "实际: {text}");
        assert_eq!(err, Some("该会话是 SFTP 节点，不能用于转发"));

        let (text, err) = session_ref_label(None, &all);
        assert_eq!(text, "(未选择)");
        assert!(err.is_none());
    }

    fn sess_rec(id: u64, name: &str, protocol: mullion_store::Protocol) -> SessionRecord {
        use mullion_store::model::{Auth, AuthKind, Connection, Identity};
        SessionRecord {
            id: SessionId(id),
            modified_at: "t".into(),
            identity: Identity {
                name: name.into(),
                note: String::new(),
                group_id: None,
                tags: Vec::new(),
            },
            connection: Connection {
                host: "10.0.0.1".into(),
                port: 22,
                protocol,
            },
            auth: Auth {
                user: "root".into(),
                kind: AuthKind::Password,
            },
            terminal: Default::default(),
            appearance: Default::default(),
            network: Default::default(),
            automation: Default::default(),
        }
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib session_ref_label 2>&1 | tail -5`
Expected: 编译错误 `cannot find function \`session_ref_label\``

- [ ] **Step 3: 实现**

```rust
/// 「经由会话」下拉该显示什么，以及要不要在旁边出一行红字。
///
/// 三态必须分开说：**没选**、**引用的会话被删了**、**引用的是 SFTP 节点**。
/// 后两者的修法完全不同（重建会话 vs 改选一条 SSH 会话），合成一句
/// 「引用无效」等于让用户自己猜。
///
/// 悬垂时**保持原值**不静默改选：静默跳到第一条会把这条隧道悄悄接到另一台
/// 机器上（切片 T-a 的 D3 已经定死了这条）。
pub(crate) fn session_ref_label(
    id: Option<SessionId>,
    sessions: &[SessionRecord],
) -> (String, Option<&'static str>) {
    use mullion_store::Protocol;
    let Some(id) = id else {
        return ("(未选择)".to_string(), None);
    };
    match sessions.iter().find(|s| s.id == id) {
        None => (
            format!("⚠ 已删除的会话 (id={})", id.0),
            Some("引用的会话已删除"),
        ),
        Some(s) if s.connection.protocol != Protocol::Ssh => (
            format!("⚠ {} (SFTP 节点)", s.identity.name),
            Some("该会话是 SFTP 节点，不能用于转发"),
        ),
        Some(s) => (
            format!(
                "{} ({}@{})",
                s.identity.name, s.auth.user, s.connection.host
            ),
            None,
        ),
    }
}
```

`show` 里「经由会话」那一格改用它，且候选只列 SSH：

```rust
        form::required(ui, t, "经由会话");
        let (selected, err) = session_ref_label(buf.session_id, sessions);
        egui::ComboBox::from_id_salt("tunnel_session")
            .selected_text(selected)
            .show_ui(ui, |ui| {
                // 只列 SSH 会话：隧道走的是 direct-tcpip / tcpip-forward，
                // SFTP 节点跟它没关系（D6）。
                for s in sessions
                    .iter()
                    .filter(|s| s.connection.protocol == mullion_store::Protocol::Ssh)
                {
                    let label = format!(
                        "{} ({}@{})",
                        s.identity.name, s.auth.user, s.connection.host
                    );
                    ui.selectable_value(&mut buf.session_id, Some(s.id), label);
                }
            });
        ui.end_row();
        form::field_error(ui, t, err.is_some(), err.unwrap_or_default());
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib session_ref_label 2>&1 | tail -5`
Expected: `test result: ok. 1 passed`

- [ ] **Step 5: 加渲染断言——候选里没有 SFTP 节点**

```rust
    /// 自证会变红：把 `show` 里下拉候选的 `.filter(...)` 去掉。
    #[test]
    fn the_session_dropdown_never_offers_an_sftp_node() {
        let all = vec![
            sess_rec(1, "生产主控", mullion_store::Protocol::Ssh),
            sess_rec(2, "文件中转", mullion_store::Protocol::Sftp),
        ];
        let mut buf = filled(TunnelKindUi::Local);
        buf.session_id = Some(SessionId(1));
        let texts = rendered_texts(buf, &all);
        assert!(
            !has(&texts, "文件中转"),
            "SFTP 节点出现在了隧道的会话候选里: {texts:?}"
        );
    }
```

> 注：egui 的 `ComboBox` 候选只在下拉**展开**时才画。若这条断言因为候选压根没渲染而恒绿，就改成对 `session_ref_label` 之外再抽一个 `ssh_candidates(sessions) -> Vec<&SessionRecord>` 纯函数并直接测它——**恒绿的测试比没有测试更糟**（见记忆：三类恒绿模式）。选后者时，`show` 必须改用这个纯函数，别让测试测一个渲染代码不走的路径。

- [ ] **Step 6: 跑测试并确认它不是恒绿**

Run: `cargo test -p mullion-app --lib the_session_dropdown_never 2>&1 | tail -5`
Expected: pass。**然后手工把 `.filter(...)` 去掉再跑一次，必须变红**；变不红就按 Step 5 的注改成纯函数测法。改回来。

- [ ] **Step 7: Commit**

```bash
git add crates/mullion-app/src/ui/session_manager/tunnel_editor.rs
git commit -m "feat(ui): 隧道「经由会话」只列 SSH 会话,SFTP 节点单独措辞 (F118)

三态分开说:未选 / 会话已删除 / 是 SFTP 节点。后两者修法完全不同
(重建会话 vs 改选一条 SSH 会话),合成一句「引用无效」等于让用户猜。
悬垂时保持原值不静默改选(沿用切片 T-a D3)。"
```

---

### Task 8: 同形判定纳入协议（D7）

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/dedupe.rs`

**与 spec D7 的实现差异（等价，但更小）**：spec 写的是「`disambiguate` 拿当前页过滤后的集合」。实际实现改为**在 `looks_same` 里加协议判据**——因为分页判据就是协议，两者结果完全等价，但不用改 `disambiguate` 的签名，也不用在调用方构造一份过滤后的 `Vec`。`duplicate_of` 不动（它**已经**比较了 protocol）。

- [ ] **Step 1: 写失败测试**

```rust
    /// 列表按协议分页之后，两个协议的记录永远不同框出现 —— 给它们互相追加
    /// 「区分信息」是纯噪音。而按 D1 的推荐做法，同一台机器建一条 SSH 会话
    /// + 一条 SFTP 节点正是**正常用法**，每行都挂个后缀会让人以为配错了。
    #[test]
    fn records_of_different_protocols_are_never_twins() {
        let mut a = rec(1, "web01", "10.0.0.1", 22);
        let mut b = rec(2, "web01", "10.0.0.1", 22);
        a.connection.protocol = mullion_store::Protocol::Ssh;
        b.connection.protocol = mullion_store::Protocol::Sftp;
        let all = vec![a.clone(), b.clone()];
        assert_eq!(
            disambiguate(&a, &all, &[]),
            None,
            "跨协议的两条不该被判成同形"
        );
        assert_eq!(disambiguate(&b, &all, &[]), None);

        // 反面：同协议、其它都一样时仍然要追加区分信息，否则这条改动
        // 把原有保护一起弄没了。
        let mut c = rec(3, "web01", "10.0.0.1", 2222);
        c.connection.protocol = mullion_store::Protocol::Ssh;
        let all = vec![a.clone(), c];
        assert_eq!(disambiguate(&a, &all, &[]), Some(":22".into()));
    }
```

> `rec(...)` 是 `dedupe.rs` 测试里已有的构造函数，签名 `rec(id, name, host, port)`。若它不设 protocol，按上面写法显式覆盖即可。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib records_of_different_protocols 2>&1 | tail -8`
Expected: FAIL（跨协议被判成 twins，返回了 `Some("未分组")` 之类）

- [ ] **Step 3: 实现**

```rust
/// 在列表上会不会被看成同一行。列表只显示名称 + `user@host`,所以只比这三样
/// ——**外加协议**：列表按协议分页（F118）之后，两个协议的记录永远不会同框
/// 出现，把它们判成同形等于给一个不存在的视觉冲突加噪音。而「同一台机器
/// 建一条 SSH + 一条 SFTP」是 D1 明确接受的正常用法。
fn looks_same(a: &SessionRecord, b: &SessionRecord) -> bool {
    a.identity.name == b.identity.name
        && a.auth.user == b.auth.user
        && a.connection.host == b.connection.host
        && a.connection.protocol == b.connection.protocol
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib dedupe 2>&1 | tail -5`
Expected: 全绿（含既有那批）。

- [ ] **Step 5: Commit**

```bash
git add crates/mullion-app/src/ui/session_manager/dedupe.rs
git commit -m "fix(ui): 同形判定纳入协议,跨协议的两条不再互相追加区分信息 (F118)

列表按协议分页后两者永不同框,判成同形是纯噪音;而「同一台机器一条
SSH 一条 SFTP」是 D1 接受的正常用法。duplicate_of 不动 —— 它本来就
比较 protocol。"
```

---

### Task 9: 机械守护测试（D10 前半）

**Files:**
- Create: `crates/mullion-app/tests/form_guidelines.rs`

- [ ] **Step 1: 先把当前违规扫一遍，作为白名单的依据**

Run:
```bash
grep -rn "add_space([0-9]" crates/mullion-app/src/ui/session_manager/*.rs
grep -rn "desired_width([0-9]" crates/mullion-app/src/ui/session_manager/*.rs
```
Task 2 做完后，`tunnel_editor.rs` 的 9 处应当已经消失。剩下的按设计 D10 分两类：`4.0`/`8.0` 等值替换成 `SP_XS`/`SP_S`；`2.0`/`6.0`（不在五档上）进白名单。

- [ ] **Step 2: 写守护测试**

```rust
//! F119 表单规范的机械守护：扫源码，挡住新写的裸数字。
//!
//! 判据是「参数不得是数字字面量」，不是「只允许某几个名字」——
//! `mod.rs` 的凭据输入框是先 `let w = field_w(...)` 再 `desired_width(w)`，
//! 白名单式判据会把最规范的写法反而拦下。
//!
//! **这道网挡不住什么**（写在这里，免得有人以为它是全部保障）：
//! `let w = 80.0; desired_width(w)` 绕得过去；「用了常量但选错档位」
//! （该 `FIELD_W_S` 却写 `FIELD_W_L`）它看不出来。那两类只能靠评审。
//! 规范全文见 `docs/ui-form-guidelines.md`。

use std::path::Path;

/// 扫描范围：会话管理器的全部 UI 源码。
const DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/ui/session_manager"
);

/// 既有违规的行级白名单。
///
/// **不是「加进来就绿了」的口袋。** 只允许一种理由：这个数值不在
/// `SP_*` 五档刻度上，改成最近的档位会带来**未经人工验收的视觉变化**，
/// 而本切片的范围是布局重排、不是调间距（Scope Discipline）。
/// 每条都写明理由，下次视觉走查时清空。
///
/// 匹配的是**行内容**（trim 后），不是行号 —— 行号会随任何编辑漂移。
const ALLOW: &[(&str, &str, &str)] = &[
    (
        "tunnel_list.rs",
        "ui.add_space(2.0);",
        "行内紧凑间距，2.0 不在五档上；等下次视觉走查统一",
    ),
    (
        "mod.rs",
        "ui.add_space(6.0);",
        "模式条与双栏之间的呼吸，6.0 不在五档上；同上",
    ),
    (
        "editor.rs",
        "ui.add_space(6.0);",
        "标题条/错误卡片下方的呼吸，6.0 不在五档上；同上",
    ),
];

fn is_allowed(file: &str, line: &str) -> bool {
    ALLOW
        .iter()
        .any(|(f, l, _)| *f == file && line.trim() == *l)
}

/// 找出 `needle(` 后面紧跟数字字面量的行。返回 `(文件名, 行号, 行内容)`。
///
/// 只扫**渲染代码**：碰到 `#[cfg(test)]` 就停 —— 测试里为了构造场景写死
/// 尺寸是正当的（`egui::vec2(280.0, 300.0)` 之类），规范管的是产品代码。
fn scan(src: &str, file: &str, needle: &str) -> Vec<(String, usize, String)> {
    let mut out = Vec::new();
    for (i, line) in src.lines().enumerate() {
        if line.trim_start().starts_with("#[cfg(test)]") {
            break;
        }
        // 注释行不算 —— 文档里举反例是正常的。
        if line.trim_start().starts_with("//") {
            continue;
        }
        let mut rest = line;
        while let Some(p) = rest.find(needle) {
            let after = &rest[p + needle.len()..];
            if after.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                out.push((file.to_string(), i + 1, line.to_string()));
                break;
            }
            rest = after;
        }
    }
    out
}

fn each_source(mut f: impl FnMut(&str, &str)) {
    for entry in std::fs::read_dir(Path::new(DIR)).expect("扫描目录读不开") {
        let path = entry.expect("目录项读不出").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .expect("文件名不是 UTF-8")
            .to_string();
        let src = std::fs::read_to_string(&path).expect("源码读不出");
        f(&name, &src);
    }
}

/// 间距只能用 `SP_XS/S/M/L/XL` 五档。裸数字让「这一处该多松」变成各人各拍
/// 脑袋，表单一路铺下来就没有节奏可言。
///
/// 自证会变红：把任意一处 `ui.add_space(SP_S)` 改回 `ui.add_space(8.0)`。
#[test]
fn no_bare_numeric_spacing_in_session_manager_ui() {
    let mut bad = Vec::new();
    each_source(|name, src| {
        for (f, line_no, line) in scan(src, name, "add_space(") {
            if !is_allowed(&f, &line) {
                bad.push(format!("{f}:{line_no}: {}", line.trim()));
            }
        }
    });
    assert!(
        bad.is_empty(),
        "间距必须用 SP_* 五档（见 docs/ui-form-guidelines.md）：\n{}",
        bad.join("\n")
    );
}

/// 输入框宽度必须过 `field_w`（扣预留 → 取上限 → 夹下界，三步缺一不可，
/// 理由见 metrics.rs）。硬编码宽度在右栏被拖宽/拖窄时一定错。
///
/// 自证会变红：把任意一处 `desired_width(field_w(...))` 改回 `desired_width(80.0)`。
#[test]
fn no_bare_numeric_field_width_in_session_manager_ui() {
    let mut bad = Vec::new();
    each_source(|name, src| {
        for (f, line_no, line) in scan(src, name, "desired_width(") {
            if !is_allowed(&f, &line) {
                bad.push(format!("{f}:{line_no}: {}", line.trim()));
            }
        }
    });
    assert!(
        bad.is_empty(),
        "输入框宽度必须过 field_w / FIELD_W_*（见 docs/ui-form-guidelines.md）：\n{}",
        bad.join("\n")
    );
}
```

- [ ] **Step 3: 跑测试——预期先红，红的正是待替换的那批**

Run: `cargo test -p mullion-app --test form_guidelines 2>&1 | tail -25`
Expected: FAIL，列出 `4.0` / `8.0` 那批（约 10 处）。

- [ ] **Step 4: 做等值替换**

按测试列出的清单，逐处把 `add_space(4.0)` → `add_space(crate::ui::metrics::SP_XS)`、`add_space(8.0)` → `add_space(crate::ui::metrics::SP_S)`。文件已 `use` 了 metrics 的就直接写 `SP_XS`/`SP_S`。

**只换等值的**。若测试列出了 `2.0`/`6.0` 而白名单没盖住，说明白名单的行内容写得不对（可能缩进/分号不一致）——修白名单，**不要**去改那些数值。

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p mullion-app --test form_guidelines 2>&1 | tail -5`
Expected: `test result: ok. 2 passed`

- [ ] **Step 6: 验证守护能自证变红**

手工把某一处刚换好的 `SP_S` 改回 `8.0`，跑：
Run: `cargo test -p mullion-app --test form_guidelines 2>&1 | tail -10`
Expected: FAIL 并点名那一行。确认后改回来再跑一次确认绿。

**这一步不能跳。** 一条永远不会红的守护测试比没有测试更糟——它给人「这块有保障」的错觉。

- [ ] **Step 7: Commit**

```bash
git add crates/mullion-app/tests/form_guidelines.rs crates/mullion-app/src/ui/session_manager/
git commit -m "test(ui): 扫源码的表单规范守护 + 等值裸间距归档 (F119)

判据是「参数不得是数字字面量」而非白名单式的「只允许某几个名字」——
后者会误伤 mod.rs 先 let w = field_w(..) 再 desired_width(w) 的写法。
4.0/8.0 等值换成 SP_XS/SP_S(零视觉变化);2.0/6.0 不在五档上,进带
理由的行级白名单,不在本切片偷改数值。已实测自证变红。"
```

---

### Task 10: 规范文档 `docs/ui-form-guidelines.md`（D10 后半）

**Files:**
- Create: `docs/ui-form-guidelines.md`
- Modify: `CLAUDE.md`（目录约定里挂一行指针）

- [ ] **Step 1: 写文档**

九条，每条给「规则 / 为什么 / 谁保证」。**「为什么」一栏必须写具体症状，不许写「更美观」**——这份文档的价值全在于半年后有人想破例时能看到代价。

```markdown
# 表单布局规范（F119）

> 适用范围：`crates/mullion-app/src/ui/` 下的所有 egui 表单。
> 构件真源在 `session_manager/form.rs` 与 `ui/metrics.rs`，本文件是它们的文字版 ——
> **两者必须同时改**。色板不在这里，见 `spec.md` §4.6（F80~F85），别复制一份过来。

| # | 规则 | 为什么（症状） | 谁保证 |
|---|---|---|---|
| 1 | 表单骨架三层：`form::section` 分节 → `form::grid` 两列 → 88px 定宽标签列 | 字段一路平铺下来没有任何视觉锚点，眼睛找不到「这几行是一组」（走查 P2-17）；标签列不定宽则各分区输入框左边缘错开 | `form.rs`，评审 |
| 2 | 输入框宽度只用 `FIELD_W_S/M/L` 三档，且必须过 `field_w(available, max, reserve)` | 不扣 reserve → 同行的按钮被裁（走查 P0-1）；不取上限 →「LEG」填在 800px 的框里（P0-2）；不夹下界 → 极窄时框塌成一条缝，看起来像「输入框不见了」 | `tests/form_guidelines.rs` |
| 3 | 间距只用 `SP_XS/S/M/L/XL` 五档，禁止裸数字 | 「这一处该多松」各人各拍脑袋，表单没有节奏 | `tests/form_guidelines.rs` |
| 4 | 必填项用 `form::required`（标签后跟 danger 星号） | 不标的话，用户要靠点保存后报错才知道哪些是必填 | `form.rs`，评审 |
| 5 | 内联红字用 `form::field_error`，挂**输入列**，不挂标签列 | 挂标签列会被读成「这一行的标签」（走查 15） | `form.rs`，评审 |
| 6 | 灰字说明同样挂输入列下方，不占标签列 | 同上；且说明多是整句，挤在输入框右边无论怎么算宽度都放不下（P0-1） | 评审 |
| 7 | **危险措辞**：能验证的后果，说风险并**点名具体目标**；验证不了的，必须写明「无法验证」及原因 | 用户要判断的是「这台机器暴露出去要不要紧」，没有目标就没法判断；而 `-R` 的远端绑定 sshd 默认会静默降级、协议只回端口号不回绑定地址，照抄 `-L` 的口气等于给一个我们证明不了的安全承诺（F112/F117，spec F43 已拒绝过同类做法） | `tunnel_editor::expose_warning` 的穷举测试，评审 |
| 8 | 空态文案告诉用户**下一步做什么**，且只说这里真能做到的事 | 「从左侧选一条」在一条都没有时是句废话；提「导入 ~/.ssh/config」则是承诺一个还没实现的功能（走查 21/22） | `editor.rs` 的空态测试 |
| 9 | 禁用的控件必须给 `on_disabled_hover_text` 说明原因 | 灰着的按钮不说话，用户只会反复点、然后以为程序坏了 | 评审 |

## 机械守护挡不住什么

`tests/form_guidelines.rs` 扫的是源码文本，只挡「参数是数字字面量」这一种写法：

- `let w = 80.0; desired_width(w)` 绕得过去。
- 「用了常量但选错档位」（该 `FIELD_W_S` 却写 `FIELD_W_L`）它看不出来。
- 白名单里的行是**既有欠债**，不是许可 —— 每条都写了理由和清理条件。

这三类只能靠评审。守护测试是第一道网，不是全部。
```

- [ ] **Step 2: 在 `CLAUDE.md` 挂指针**

在「`docs/` 关键非 ADR 文件」那一节，`gui-render-gotchas.md` 那条后面加一行：

```markdown
- `ui-form-guidelines.md` —— 表单布局规范（分节/宽度三档/间距五档/危险措辞/空态文案），
  写任何 egui 表单前先扫一眼；机械守护在 `crates/mullion-app/tests/form_guidelines.rs`
```

- [ ] **Step 3: 验证文档里引用的东西都真的存在**

Run:
```bash
grep -n "SP_XS\|SP_S\|SP_M\|SP_L\|SP_XL" crates/mullion-app/src/ui/metrics.rs | head -5
grep -n "FIELD_W_S\|FIELD_W_M\|FIELD_W_L" crates/mullion-app/src/ui/metrics.rs | head -5
ls crates/mullion-app/src/ui/session_manager/form.rs crates/mullion-app/tests/form_guidelines.rs
```
Expected: 全部命中/存在。文档里写了不存在的路径，下一个人照着找会浪费半小时。

- [ ] **Step 4: Commit**

```bash
git add docs/ui-form-guidelines.md CLAUDE.md
git commit -m "docs: 表单布局规范落盘,CLAUDE.md 挂指针 (F119)

九条规则各带「为什么(具体症状)」——半年后有人想破例时要看得到代价。
明写机械守护挡不住的三类(变量绕行/选错档位/白名单欠债),免得被当成
全部保障。色板不复制,仍指向 spec §4.6。"
```

---

### Task 11: spec.md 增补 F118 / F119，改写 §A 与 F116

**Files:**
- Modify: `spec.md`

- [ ] **Step 1: 追加两行功能编号**

在 F117 那一行之后加：

```markdown
| F118 | 会话管理器 SFTP 节点档：模式条第三档「会话 \| SFTP \| 隧道」，SFTP 节点 = `protocol == Sftp` 的 `SessionRecord`（**schema 不动**，仍 v7）。协议字段改**只读**（要换协议就新建）；SFTP 档隐掉「登录后」页；连接入口置灰（F50 未实现）；隧道「经由会话」只列 SSH | P1 | 分流真源是纯函数 `protocol_of`，`list::show` 与 `visible_order` **共用**同一判据（只过滤渲染侧会让方向键跳到看不见的行，有守护测试）；Tab 下标走 `visible_tabs` 映射（隐藏中间页后用 `enumerate` 序号会让点「图标」打开「登录后」）；连接意图有一道兜底闸门 + 各入口置灰两层 |
| F119 | 表单布局规范：骨架构件抽 `session_manager/form.rs`（分节/两列 Grid/必填星号/内联红字），规范文档 `docs/ui-form-guidelines.md`，两条扫源码的机械守护 | P1 | 守护判据是「参数不得是数字字面量」而非白名单式（后者会误伤 `let w = field_w(..)` 再 `desired_width(w)` 的合规写法）；必须实测自证变红；文档须写明守护挡不住的三类 |
```

- [ ] **Step 2: 改写 §A（协议筛选 chips 的否决理由）**

原文的否决理由是「`Protocol` 只有两个值且 `Sftp` 拨号未实现，库里所有会话实际都是 SSH——单一有效值的筛选器是纯噪音」。现在库里真的会有两类了，理由必须换，否则下一个人会拿一条已经失效的理由做决定：

```markdown
| §A | 左栏协议筛选 chips（全部 / SSH / SFTP） | **理由已于 2026-08 更新**：不再是「只有一个有效值」（F118 之后库里真的有两类了），而是**模式条已经承担了协议这条轴**（F118），再加一层筛选是同一件事做两遍 | 出现第三类协议、或用户需要「跨协议一起看」的明确场景 |
```

- [ ] **Step 3: 更新 F116 的描述**

把「会话 / 隧道模式切换」改成三档，并保留原有验收：

```markdown
| F116 | 会话管理器顶层模式切换：**会话 / SFTP / 隧道**（第三档见 F118）。左栏一级组织轴**不变**（仍按分组归桶）；隧道列表不做分组、不做三档密度 | P1 | 切模式不污染另一侧编辑器的脏标记，有单测；模式条须过 F100 登记；**键盘动作按模式分流**——隧道档的 ↑↓/Enter/Ctrl+N 一律 no-op（原先无模式判断，会操作看不见的会话列表） |
```

- [ ] **Step 4: 更新路线图行**

`| **插队** | F110–F117 | 隧道可用（§4.12，2026-08 取货）…` 那一行后面追加一行：

```markdown
| **插队** | F118–F119 | SFTP 节点管理 + 表单规范（2026-08）。**F50–F57 SFTP 传输本体仍在其后**，本档只管节点配置 |
```

- [ ] **Step 5: 自查引用一致**

Run: `grep -n "F118\|F119" spec.md`
Expected: 至少 4 处命中（两行定义、§A、F116 或路线图），且没有把 F118 写成 F117 之类的笔误。

- [ ] **Step 6: Commit**

```bash
git add spec.md
git commit -m "docs(spec): 增补 F118/F119,改写 §A 与 F116 (F118/F119)

§A 的否决理由必须换 —— 原文说「库里所有会话实际都是 SSH」,F118 之后
不成立了,留着会让下一个人拿一条失效的理由做决定。
F116 补上键盘按模式分流这条验收。"
```

---

### Task 12: 交付（版本 / 跑绿 / 交叉编译 / 发 Release）

按 `CLAUDE.md` 的「交付约定」一条龙做完，**不要停下来问**。

- [ ] **Step 1: 升版本**

`Cargo.toml` 的 `workspace.package.version`：`0.1.30` → `0.1.31`。

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: 版本 0.1.31(F118 SFTP 节点档 + F119 隧道表单重排与布局规范)"
```

- [ ] **Step 2: 跑绿（三样缺一不可）**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log | tail -20
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```
Expected: 测试全 ok；clippy 无输出；fmt 无输出。**不绿不发。**

- [ ] **Step 3: 交叉编译 + objdump 验收**

```bash
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app
```
按 `docs/cross-compile-windows.md` 做依赖验收：出现 `libgcc_s_seh-1.dll` / `libwinpthread-1.dll` 即**不合格**，必须修。

- [ ] **Step 4: 发 Release**

```bash
cd target/x86_64-pc-windows-gnu/release
sha256sum mullion.exe > mullion.exe.sha256
HTTPS_PROXY=http://127.0.0.1:7890 gh release create v0.1.31 \
  mullion.exe mullion.exe.sha256 -t "v0.1.31" -F notes.md --repo kilobitcy/Mullion
```

**Release 标题只能是纯版本号 `v0.1.31`**，不带破折号、不带摘要、不带 emoji。

`notes.md` 正文必须含：修了什么 + 下面这份人工验收清单 + sha256 + 首次运行提示（`Unblock-File .\mullion.exe`）。

**人工验收清单（无头环境验不了，逐条抄进 notes）：**

1. 模式条三档 `会话 | SFTP | 隧道` 在默认 880px 窗口下不换行、对齐正常。
2. 隧道表单重排后的观感：四个分节的分隔线、标签列是否对齐、端口框宽度是否显得空/挤。
3. 把左右分隔条拖到最窄和最宽两个极限，隧道表单「目标主机 : 端口」那一行不被裁、不溢出。
4. **既有会话库打开后，会话页的条数与升级前一致**（若你的库里有 `protocol = "sftp"` 的记录，它现在应出现在 SFTP 页，而不是消失）。
5. SFTP 页新建一个节点 → 保存 → 重开程序仍在；「连接」按钮点不动，且悬停能看到「SFTP 传输尚未实现（F50）」。
6. 隧道页新建时，「经由会话」下拉里看不到第 5 步建的那个 SFTP 节点。
7. 会话页编辑器里「协议」那一行现在是只读文本（不再是可选 sftp 的下拉）。

- [ ] **Step 5: 报告**

给出 Release 链接 + sha256 + 上面的验收清单。

---

## 计划自查

- **spec 覆盖**：D1→Task 3；D2→Task 3+4；D3→Task 5；D4→Task 6；D5→Task 5；D6→Task 7；D7→Task 8；D8→Task 1；D9→Task 2；D10→Task 9+10。「不做什么」四条（F50 本体、`autostart`、隧道脏标记、协议筛选 chips）在任何 Task 里都没有实现步骤，符合预期；§A 的理由改写在 Task 11。
- **类型一致**：`protocol_of(mode) -> Option<Protocol>`（Task 3 定义，Task 4/5 使用）；`visible_tabs(mode) -> &'static [usize]`（Task 5 定义，Task 4 的 `Tab(n)` 分支使用——故 Task 4 明确写了「先不改这个分支，Task 5 回来改」）；`on_page(rec, query, protocol) -> bool`（Task 3 内部）；`session_ref_label(id, sessions) -> (String, Option<&'static str>)`（Task 7）；`port_field_error(text) -> bool`（Task 2）；`SFTP_NOT_YET`（Task 6 定义于 `editor.rs`，Task 6 内 `list.rs` 引用）。
- **已知的跨任务依赖**：Task 2 必须在 Task 9 之前（否则 `tunnel_editor.rs` 的 9 处裸数字会把守护测试淹掉）；Task 5 必须在 Task 4 之后（`visible_tabs` 的使用点）。其余可按顺序执行。
