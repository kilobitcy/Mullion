# 会话管理器 UI 走查 · 阶段 2「继承与信息架构」实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让「继承」这件事在界面上可见 —— 每一个选了「继承」的字段都当场显示**实际会生效的值和它来自哪一层**；同时把 Tab 角标、跳板路径预览、env 警告分级这三处信息架构缺陷补上。

**Architecture:** 新增三个纯逻辑模块 —— `inherit_row.rs`（「生效值 + 来源」文本的纯函数 + 一个 `Ui` 包装）、`tab_badge.rs`（Tab 三态角标的纯判据）、`jump_preview.rs`（跳板链路径预览与环/悬空/超深的人话化）。然后把六处继承字段（时序三项 / tmux / 工作目录 / 代理 / 跳板 / 命令 / env）改成走同一个继承槽部件，把 Tab 条的红点扩成三态，把 `ENV_WARNING` 从常驻横幅降级为常驻灰字 + 命中关键词才升级。不动 `sessions.toml` 结构、不动继承解析语义（`mullion-store` 只读不写）。

**Tech Stack:** Rust 2021 / egui 0.30 / epaint 0.30。全部改动落在 `crates/mullion-app`，`mullion-store` **零改动**。

---

## 前置约定（执行者必读）

1. **不升版本号、不发 Release。** 走查 4 个阶段共用一个 `v0.1.25`，版本 bump 与交叉编译在阶段 4 结束后一次性做（CLAUDE.md「交付约定」）。本阶段结束只要求 `cargo test --workspace` 全绿 + `clippy --workspace --all-targets -- -D warnings` 无输出 + `cargo fmt --check` 通过。
2. **提交粒度。** 每个 Task 末尾各自提交。阶段全部完成后 squash 成一个 `feat(ui): 会话管理器继承可见性与信息架构（走查 9/10/11/12/18/19）` 入 main，与阶段 1 的 `af0b986` 同粒度。
3. **架构不变量。** 新代码全在 `mullion-app`。三个新模块**不许 `use egui` 之外的 UI 类型**，且 `inherit_row::effective_line` / `tab_badge::badge_of` / `jump_preview::preview` 三个核心函数必须是**零 `Ui` 依赖的纯函数** —— 这是它们能被单测的全部理由。
4. **「继承」与「显式关闭」永不合并。** 这是 P0-b 与 P1-b 两轮各踩过一次的坑（`ProxyModeUi::Direct` vs `Inherit`、`AutomationPrefs::commands` 的 `None` vs `Some(vec![])`）。本阶段只改**显示**，一个字节的语义都不动。任何让 `None` 和 `Some(空)` 变得不可区分的改动都是设计错误。
5. **无头环境的边界。** 「灰字读起来顺不顺」「角标是不是太抢眼」属于 CLAUDE.md「你无法验证的东西」。本计划的测试只能守住文本内容与颜色值，不能证明观感。交付时如实标注人工验收项。

---

## 现状锚点（改之前先确认这些行还在）

| 位置 | 现状 | 本阶段动作 |
|---|---|---|
| `fields.rs:85-99` | `tri_state()`，三个选项 `继承/on/off` | 加「生效值」灰字（Task 2） |
| `fields.rs:109-132` | `opt_ms()`，未勾选时显示 `继承(内置默认 N ms)` | 换成统一格式（Task 2） |
| `fields.rs:202-212` | 协议 ComboBox，`ssh` / `sftp` 平级 | sftp 加「(未实现)」+ 选中提示（Task 4） |
| `fields.rs:433-435` | 跳板三态按钮，中间一个写「继承分组」 | 改「继承」+ 灰字带来源（Task 4） |
| `fields.rs:442-449` | `JumpModeUi::Inherit` 分支调 `inherit_hint()` | 换成 `inherit_row::slot`（Task 4） |
| `fields.rs:461-470` | `inherit_hint()`，跳板专用的来源文案 | 删除，由 `inherit_row` 统一（Task 4） |
| `fields.rs:735` | 代理 `ProxyModeUi::Inherit` 写「继承分组」 | 改「继承」+ 灰字（Task 4） |
| `fields.rs:807` | `pub(super) fn automation(ui, t, buf)` | **签名加 `groups: &[GroupRecord]`**（Task 2） |
| `fields.rs:830-858` | tmux ComboBox，`继承` 无来源说明 | 加灰字（Task 2） |
| `fields.rs:899` | 工作目录 hint `留空 = 继承(远端默认)` | 改走继承槽（Task 2） |
| `fields.rs:915-922` | 命令 `None` 分支：`继承上游的命令列表` + 「改为自定义」 | 换统一文案 + 生效条数（Task 3） |
| `fields.rs:1052-1053` | `warn_banner(ui, t, ENV_WARNING)` 无条件画 | 降级为灰字，命中关键词才升级（Task 8） |
| `fields.rs:1057-1063` | env `None` 分支：`继承上游的环境变量` | 同命令（Task 3） |
| `editor.rs:272-286` | Tab 条，`missing.tab() == Some(i)` 时拼 `"{name} ●"` | 三态角标 + `LayoutJob` 着色（Task 6） |
| `editor.rs:390` | `fields::automation(ui, t, buf)` | 加 `groups` 实参（Task 2） |
| `icon.rs:16-23` | `Glyph::{Cross, ArrowUp, ArrowDown}` | 加 `Info`（Task 8） |
| `icon.rs:253-263` | `points_of()` 测试辅助，遇 `Circle` 会 panic | 支持 `Circle`（Task 8） |
| `validate.rs:25-33` | `Missing::tab()` 返回第一个缺项的 Tab | 不动，被 `tab_badge` 读取 |

---

## 文件结构

**新建**

- `crates/mullion-app/src/ui/session_manager/inherit_row.rs` —— 继承槽。纯函数 `effective_line(value, source) -> String` + `upstream(group_id, groups) -> Option<&GroupRecord>`，以及 `Ui` 包装 `slot(ui, t, control, line)`。
- `crates/mullion-app/src/ui/session_manager/tab_badge.rs` —— Tab 角标三态判据。纯函数 `badge_of(tab, missing, buf) -> Badge`，零 egui。
- `crates/mullion-app/src/ui/session_manager/jump_preview.rs` —— 跳板链路径预览与错误人话化。纯函数 `preview(chain, sessions, proxy, target) -> String` 与 `check(chain, sessions) -> Option<String>`。

**修改**

- `crates/mullion-app/src/ui/session_manager/mod.rs` —— 加三行 `mod`。
- `crates/mullion-app/src/ui/session_manager/fields.rs` —— 六处继承字段接线、协议提示、env 警告分级。
- `crates/mullion-app/src/ui/session_manager/editor.rs` —— Tab 条角标、`automation()` 调用点。
- `crates/mullion-app/src/ui/icon.rs` —— `Glyph::Info`。

---

## Task 1: `inherit_row.rs` —— 「生效值 + 来源」的纯函数与继承槽

**为什么先做这个：** 走查 10（继承值不可见）与 11（继承控件语义混乱）本质是同一个缺陷 —— 六处继承字段各写各的说明文字，其中跳板那处（`inherit_hint`）写得最全，另外五处要么什么都不说（tmux、代理），要么只说「继承」不说继承到了什么（`opt_ms`）。先把「生效值 + 来源」这句话做成一个能单测的纯函数，后面三个 Task 才有东西可接。

**关键设计：`slot` 只在「当前选的就是继承」时画灰字。** 用户显式填了值的时候再解释一遍「实际生效 X」是噪音 —— 他填的就是 X。这条判据由**调用方**传 `Option<String>`（`None` = 不画）来表达，`slot` 自己不猜。

**Files:**
- Create: `crates/mullion-app/src/ui/session_manager/inherit_row.rs`
- Modify: `crates/mullion-app/src/ui/session_manager/mod.rs`

- [ ] **Step 1: 先写失败的测试**

新建 `crates/mullion-app/src/ui/session_manager/inherit_row.rs`，**只写测试模块**：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mullion_store::{GroupId, GroupRecord};

    fn group(id: u64, name: &str) -> GroupRecord {
        GroupRecord {
            id: GroupId(id),
            name: name.to_string(),
            tags: Vec::new(),
            terminal: Default::default(),
            appearance: Default::default(),
            network: Default::default(),
            automation: Default::default(),
        }
    }

    /// 三种来源必须能从文本上分辨。用户看到「实际生效:开」而不知道这个
    /// 「开」是分组配的还是内置默认,他就没法判断「改分组能不能影响到这里」——
    /// 这正是走查 10 报的缺陷。
    #[test]
    fn every_source_is_named_in_the_line() {
        assert_eq!(
            effective_line("自动 attach", Source::Group("生产")),
            "实际生效:自动 attach(来自分组「生产」)"
        );
        assert_eq!(
            effective_line("300 ms", Source::Builtin),
            "实际生效:300 ms(内置默认)"
        );
        assert_eq!(
            effective_line("不走跳板", Source::NoUpstream),
            "实际生效:不走跳板(未分组,没有上游可继承)"
        );
    }

    /// 分组名要原样出现在文本里 —— 用户得能照着这个名字去分组管理器里找。
    #[test]
    fn group_name_is_not_truncated_or_escaped() {
        let line = effective_line("SOCKS5 127.0.0.1:7891", Source::Group("代理走这台"));
        assert!(line.contains("代理走这台"), "分组名丢了: {line}");
        assert!(line.contains("SOCKS5 127.0.0.1:7891"), "生效值丢了: {line}");
    }

    /// 未分组、悬空分组 id 都必须落到 `None`。悬空 id 静默当未分组处理,
    /// 与 `jump::layers_for` 对分组的既有降级一致(悬空分组不是安全属性,
    /// 悬空**跳板**才是)。
    #[test]
    fn upstream_resolves_none_for_missing_and_dangling_group() {
        let gs = vec![group(1, "生产"), group(2, "测试")];
        assert_eq!(upstream(None, &gs).map(|g| g.name.as_str()), None);
        assert_eq!(
            upstream(Some(GroupId(9)), &gs).map(|g| g.name.as_str()),
            None,
            "悬空分组 id 应静默当未分组"
        );
        assert_eq!(
            upstream(Some(GroupId(2)), &gs).map(|g| g.name.as_str()),
            Some("测试")
        );
    }
}
```

- [ ] **Step 2: 运行测试确认它编译不过**

```bash
cargo test -p mullion-app inherit_row 2>&1 | tail -20
```

Expected: 编译失败，`cannot find type Source in this scope` / `cannot find function effective_line`。

- [ ] **Step 3: 写实现**

在 `inherit_row.rs` 的**测试模块之前**插入：

```rust
//! 继承槽(走查 10 / 11)。
//!
//! 走查报的是两件事:选了「继承」之后**不知道继承到了什么**,以及六处继承
//! 字段各写各的说明文字。这里统一的是「继承槽这个部件」——**不是**把六处
//! 都压成同一种控件形态。态数各字段自己定:代理是四态(继承/直连/SOCKS5/
//! HTTP)、跳板是三态、总开关是三态、时序是「勾/不勾」。硬要统一成二段开关
//! 会毁掉「继承」与「显式关闭」的区分,那是 P0-b 和 P1-b 各踩过一次的坑。
//!
//! `effective_line` 是纯函数:文案里少写一个来源、把「内置默认」和「来自
//! 分组」搞反,都不会有编译错误也不会 panic,只会让用户按着错误的心智模型
//! 去改分组配置。

use egui::Ui;
use mullion_store::{GroupId, GroupRecord};

use crate::theme::Theme;

/// 继承链上**真正生效的那一层**。
///
/// 分组是单层的(设计 D1),所以上游只有一级 —— 这里能把结果算准,
/// 不必含糊地说「跟随上游」。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source<'a> {
    /// 上游分组配了值。
    Group(&'a str),
    /// 全链路都没配,落到内置默认。
    Builtin,
    /// 当前会话未分组(或分组已被删),没有上游可继承。
    NoUpstream,
}

/// 「实际生效:X(来源)」这一行灰字的文本。
///
/// 三种来源都要在文本里点名。只说「实际生效:开」的话,用户无法判断
/// 「去分组里改一下能不能影响到这条会话」—— 走查 10 报的正是这个。
pub fn effective_line(value: &str, source: Source<'_>) -> String {
    match source {
        Source::Group(name) => format!("实际生效:{value}(来自分组「{name}」)"),
        Source::Builtin => format!("实际生效:{value}(内置默认)"),
        Source::NoUpstream => format!("实际生效:{value}(未分组,没有上游可继承)"),
    }
}

/// 当前会话的上游分组。悬空 `group_id` 静默当未分组 —— 与
/// `jump::layers_for` 对分组的既有降级一致。
///
/// (悬空**跳板**是另一回事,那里必须硬失败:用户会以为流量过了堡垒机
/// 而实际没有。分组只影响偏好取值,降级不产生安全后果。)
pub fn upstream(group_id: Option<GroupId>, groups: &[GroupRecord]) -> Option<&GroupRecord> {
    group_id.and_then(|gid| groups.iter().find(|g| g.id == gid))
}

/// 继承槽的统一排版:左边是本字段自己的控件,右边跟一行「实际生效…」灰字。
///
/// `line` 为 `None` 时只画控件 —— 用户显式填了值的时候再解释一遍
/// 「实际生效 X」是噪音,他填的就是 X。判据由调用方给,这里不猜。
///
/// 用 `horizontal_wrapped` 而不是 `horizontal`:灰字比控件长得多
/// (「实际生效:SOCKS5 127.0.0.1:7891(来自分组「生产」)」将近 30 个字),
/// 不换行会把这一格撑宽、顶出面板 —— 走查 P0-1 的同族缺陷,阶段 1 已经
/// 在代理模式按钮和命令行上各踩过一次。
pub fn slot(ui: &mut Ui, t: &Theme, control: impl FnOnce(&mut Ui), line: Option<String>) {
    ui.horizontal_wrapped(|ui| {
        control(ui);
        if let Some(s) = line {
            ui.colored_label(crate::theme::c32(t.fg_dimmer), s);
        }
    });
}
```

- [ ] **Step 4: 挂到模块树**

`crates/mullion-app/src/ui/session_manager/mod.rs`，在既有的 `mod fields;` 一行**之后**加：

```rust
mod inherit_row;
```

（`mod.rs` 里已有 `mod buffer; mod editor; mod fields; mod keyscan; mod list; mod validate;` 一组，按字母序插在 `mod fields;` 与 `mod keyscan;` 之间。）

- [ ] **Step 5: 运行测试确认通过**

```bash
cargo test -p mullion-app inherit_row 2>&1 | grep -E "test result|FAILED"
```

Expected: `test result: ok. 3 passed`。

- [ ] **Step 6: 确认 clippy 干净**

```bash
cargo clippy -p mullion-app --all-targets -- -D warnings 2>&1 | tail -5
```

Expected: 无输出（`slot` 此刻还没有调用方，但它是 `pub` 的，不会触发 dead_code）。

- [ ] **Step 7: 提交**

```bash
git add crates/mullion-app/src/ui/session_manager/inherit_row.rs crates/mullion-app/src/ui/session_manager/mod.rs
git commit -m "feat(ui): 继承槽的生效值与来源文本 (走查 10/11)

新增 inherit_row 模块:effective_line 把「实际生效 X + 来自哪一层」摊成
一行文本,三种来源(分组/内置默认/未分组)在文案里各自点名。纯函数,
文案写错不会编译失败也不会 panic,只会让用户按错误的心智模型改配置。"
```

---

## Task 2: 「登录后」页的标量继承接线

**为什么：** 「登录后」页有五个标量继承字段（总开关 / tmux / 工作目录 / 三个时序），全都能选「继承」，但**没有一个**告诉用户继承到了什么。`opt_ms` 那三处只说「继承(内置默认 300 ms)」—— 它假定了上游一定是内置默认，而分组**可以**配 `initial_delay_ms`，这时候这句话是错的。

**关键前提：`automation()` 必须能看到分组。** 它现在的签名是 `automation(ui, t, buf)`，拿不到 `groups`。加第四个参数，调用点在 `editor.rs:390`。

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/fields.rs:807`（签名）、`:820-907`（四处字段）、`:1136-`（时序三项）
- Modify: `crates/mullion-app/src/ui/session_manager/editor.rs:390`（调用点）

- [ ] **Step 1: 先写失败的测试**

在 `fields.rs` 的 `mod tests` 里加（`run_page` 已存在，见阶段 1 补债）：

```rust
/// 走查 10:选了「继承」而分组配了值时,必须说清是**分组**配的。
///
/// 旧文案写死「继承(内置默认 300 ms)」—— 分组配了 900 时这句话是错的,
/// 用户会以为改分组没用。
#[test]
fn inherited_timing_names_the_group_when_the_group_sets_it() {
    let mut buf = EditorBuffer::default();
    buf.preserved_group_id = Some(mullion_store::GroupId(7));
    // 会话侧全 None = 全部继承。
    buf.preserved_automation = mullion_store::AutomationPrefs::default();

    let mut g = mullion_store::GroupRecord {
        id: mullion_store::GroupId(7),
        name: "生产".into(),
        tags: Vec::new(),
        terminal: Default::default(),
        appearance: Default::default(),
        network: Default::default(),
        automation: Default::default(),
    };
    g.automation.initial_delay_ms = Some(900);
    let groups = vec![g];

    let t = crate::theme::MULLION_DARK;
    let out = run_page(|ui| super::automation(ui, &t, &mut buf, &groups));
    let texts = all_text(&out.shapes);
    assert!(
        texts.iter().any(|s| s.contains("900 ms") && s.contains("生产")),
        "分组配了 initial_delay_ms=900,继承提示必须点名分组「生产」;实际画出的文字:{texts:?}"
    );
}

/// 分组没配时才落「内置默认」。这条和上一条是一对 —— 只留一条的话,
/// 把实现写死成任意一支都能过。
#[test]
fn inherited_timing_falls_back_to_builtin_when_no_group_sets_it() {
    let mut buf = EditorBuffer::default();
    buf.preserved_group_id = None;
    let t = crate::theme::MULLION_DARK;
    let out = run_page(|ui| super::automation(ui, &t, &mut buf, &[]));
    let texts = all_text(&out.shapes);
    assert!(
        texts.iter().any(|s| s.contains("300 ms") && s.contains("内置默认")),
        "未分组时三个时序应显示内置默认;实际画出的文字:{texts:?}"
    );
}

/// tmux 选「继承」时也要说清生效结果。走查 10 点名的六处里,
/// tmux 是唯一一处**什么都不说**的。
#[test]
fn inherited_tmux_shows_what_it_resolves_to() {
    let mut buf = EditorBuffer::default();
    buf.preserved_group_id = Some(mullion_store::GroupId(3));
    let mut g = mullion_store::GroupRecord {
        id: mullion_store::GroupId(3),
        name: "堡垒".into(),
        tags: Vec::new(),
        terminal: Default::default(),
        appearance: Default::default(),
        network: Default::default(),
        automation: Default::default(),
    };
    g.automation.tmux = Some(mullion_store::TmuxChoice::Off);
    let groups = vec![g];

    let t = crate::theme::MULLION_DARK;
    let out = run_page(|ui| super::automation(ui, &t, &mut buf, &groups));
    let texts = all_text(&out.shapes);
    assert!(
        texts.iter().any(|s| s.contains("不用 tmux") && s.contains("堡垒")),
        "分组把 tmux 关了,继承提示要说清;实际画出的文字:{texts:?}"
    );
}
```

如果 `mod tests` 里还没有 `all_text` 这个辅助，一并加上（放在 `find_text_pos` 旁边）：

```rust
/// 抠出本帧画出的所有文本。`find_text_pos` 只回位置,判「有没有说某句话」
/// 时要的是内容本身。
fn all_text(shapes: &[egui::epaint::ClippedShape]) -> Vec<String> {
    let mut out = Vec::new();
    collect_text(shapes.iter().map(|c| &c.shape), &mut out);
    out
}

fn collect_text<'a>(shapes: impl Iterator<Item = &'a egui::Shape>, out: &mut Vec<String>) {
    for s in shapes {
        match s {
            egui::Shape::Text(ts) => out.push(ts.galley.text().to_string()),
            egui::Shape::Vec(v) => collect_text(v.iter(), out),
            _ => {}
        }
    }
}
```

- [ ] **Step 2: 运行测试确认它红**

```bash
cargo test -p mullion-app inherited_ 2>&1 | grep -E "test result|FAILED|error\["
```

Expected: 先是编译失败（`automation` 只收 3 个参数）。这**就是**本 Task 要求的信号 —— 签名不改，测试根本写不出来。

- [ ] **Step 3: 改 `automation()` 签名**

`fields.rs:807`：

```rust
/// F40~F44「登录后」页。字段全部落在 `buf.preserved_automation` 上。
///
/// 字段名沿用 `preserved_*` 前缀而**没有**改成 `automation`:与
/// `preserved_group_id`(自 P0-b 起可编辑,名字未改)同一个理由 —— 改名会波及
/// `buffer.rs` 的透传守护测试,收益为零。
///
/// `groups` 是走查 10 加进来的:这一页有五个标量字段能选「继承」,而
/// 「继承到了什么」只有看得见分组才算得出来。
pub(super) fn automation(ui: &mut Ui, t: &Theme, buf: &mut EditorBuffer, groups: &[GroupRecord]) {
```

紧接着 `let derived = ...` 那两行**之前**插入上游解析（`buf` 的可变借用还没开始）：

```rust
    // 上游只解析一次:这一页有五个字段要问它。`upstream` 是线性查找,
    // 分组数量级是个位数,但每帧五次仍然没必要(本项目陷阱 T3)。
    let up = crate::ui::session_manager::inherit_row::upstream(buf.preserved_group_id, groups)
        .map(|g| (g.name.clone(), g.automation.clone()));
```

（拷贝 `name` 与 `automation` 而不是持 `&GroupRecord`：下面整段都持着 `&mut buf`，同时持 `groups` 的不可变借用没问题，但 `buf.preserved_group_id` 的读取要先于 `let a = &mut buf.preserved_automation`，拷出来最省心。`AutomationPrefs` 是 `Clone` 的小结构，每帧一次可接受。）

- [ ] **Step 4: 接线总开关**

把 `fields.rs:820-825` 的「总开关」分区改成：

```rust
    section(ui, t, "总开关", &mut first);
    grid(ui, "sm_auto_enabled", |ui| {
        ui.label("登录后自动化");
        // 选了「继承」才画生效值 —— 显式选了「开」的人不需要被告知「实际生效:开」。
        let line = a.enabled.is_none().then(|| {
            let (v, src) = resolve_bool(
                up.as_ref(),
                |p| p.enabled,
                mullion_store::automation::DEFAULT_AUTOMATION_ENABLED,
            );
            inherit_row::effective_line(if v { "开" } else { "关" }, src)
        });
        inherit_row::slot(
            ui,
            t,
            |ui| {
                tri_state(ui, "sm_auto_enabled_combo", &mut a.enabled, "开", "关");
            },
            line,
        );
        ui.end_row();
    });
```

在 `fields.rs` 顶部的 `use` 区加：

```rust
use crate::ui::session_manager::inherit_row::{self, Source};
```

并在 `automation()` **之前**加两个本文件私有的解析助手（放在 `ENV_WARNING` 常量之后）：

```rust
/// 标量继承的取值 + 来源。会话侧已经是 `None`(继承)时才调用。
///
/// 层序只有一级(分组),所以不需要 `inherit::resolve` 那套通用机制 ——
/// 但**取值规则必须和它一致**:分组有值就用分组的,否则内置默认。
/// 不一致的话,UI 显示的「实际生效」和真正连上去用的值会不同,
/// 这比不显示更坏。
fn resolve_bool<'a>(
    up: Option<&'a (String, mullion_store::AutomationPrefs)>,
    pick: impl Fn(&mullion_store::AutomationPrefs) -> Option<bool>,
    builtin: bool,
) -> (bool, Source<'a>) {
    match up {
        Some((name, prefs)) => match pick(prefs) {
            Some(v) => (v, Source::Group(name)),
            None => (builtin, Source::Builtin),
        },
        None => (builtin, Source::NoUpstream),
    }
}

/// 同 `resolve_bool`,`u32` 版。两个函数不合并成泛型:泛型版要么带上
/// `T: Copy` 约束再让调用方写 turbofish,要么就得给 `String` 也开个口子 ——
/// 两行重复换来调用点全部无标注,划算。
fn resolve_u32<'a>(
    up: Option<&'a (String, mullion_store::AutomationPrefs)>,
    pick: impl Fn(&mullion_store::AutomationPrefs) -> Option<u32>,
    builtin: u32,
) -> (u32, Source<'a>) {
    match up {
        Some((name, prefs)) => match pick(prefs) {
            Some(v) => (v, Source::Group(name)),
            None => (builtin, Source::Builtin),
        },
        None => (builtin, Source::NoUpstream),
    }
}
```

- [ ] **Step 5: 接线 tmux**

把 `fields.rs:828-859` 的 tmux ComboBox 包进 `slot`。整段替换为：

```rust
    section(ui, t, "tmux", &mut first);
    grid(ui, "sm_auto_tmux", |ui| {
        ui.label("连上后");
        let text = match &a.tmux {
            None => "继承",
            Some(TmuxChoice::Off) => "不用 tmux",
            Some(TmuxChoice::Attach { .. }) => "自动 attach",
        };
        let line = a.tmux.is_none().then(|| {
            // tmux 的内置默认是「不用」——`ResolvedAutomation` 里 tmux 为
            // `None` 时 `run()` 不发 attach 命令。
            let (v, src) = match up.as_ref() {
                Some((name, prefs)) => match &prefs.tmux {
                    Some(TmuxChoice::Off) => ("不用 tmux", Source::Group(name)),
                    Some(TmuxChoice::Attach { .. }) => ("自动 attach", Source::Group(name)),
                    None => ("不用 tmux", Source::Builtin),
                },
                None => ("不用 tmux", Source::NoUpstream),
            };
            inherit_row::effective_line(v, src)
        });
        inherit_row::slot(
            ui,
            t,
            |ui| {
                egui::ComboBox::from_id_salt("sm_auto_tmux_combo")
                    .selected_text(text)
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(a.tmux.is_none(), "继承").clicked() {
                            a.tmux = None;
                        }
                        if ui
                            .selectable_label(matches!(a.tmux, Some(TmuxChoice::Off)), "不用 tmux")
                            .clicked()
                        {
                            a.tmux = Some(TmuxChoice::Off);
                        }
                        // 已经是 Attach 时不要重建 —— 会把用户填好的会话名清掉。
                        if ui
                            .selectable_label(
                                matches!(a.tmux, Some(TmuxChoice::Attach { .. })),
                                "自动 attach",
                            )
                            .clicked()
                            && !matches!(a.tmux, Some(TmuxChoice::Attach { .. }))
                        {
                            a.tmux = Some(TmuxChoice::Attach { session_name: None });
                        }
                    });
            },
            line,
        );
        ui.end_row();

        if let Some(TmuxChoice::Attach { session_name }) = &mut a.tmux {
```

（`if let Some(TmuxChoice::Attach ...)` 那一段及其后的内容原样保留，不动。）

- [ ] **Step 6: 接线工作目录**

`fields.rs:893-907` 的「工作目录」分区，把 hint 从「留空 = 继承(远端默认)」换成中性文案，继承结果改由灰字说：

```rust
    section(ui, t, "工作目录", &mut first);
    grid(ui, "sm_auto_dir", |ui| {
        ui.label("初始目录");
        let mut s = a.work_dir.clone().unwrap_or_default();
        let line = a.work_dir.is_none().then(|| {
            let (v, src) = match up.as_ref() {
                Some((name, prefs)) => match prefs.work_dir.as_deref() {
                    Some(d) => (d.to_string(), Source::Group(name)),
                    None => ("远端默认目录".to_string(), Source::Builtin),
                },
                None => ("远端默认目录".to_string(), Source::NoUpstream),
            };
            inherit_row::effective_line(&v, src)
        });
        inherit_row::slot(
            ui,
            t,
            |ui| {
                // hint 从「留空 = 继承(远端默认)」改成中性的「留空 = 继承」:
                // 「继承到了什么」现在由右边的灰字负责,写在 hint 里既重复
                // 又管不住长度(hint 走 egui 那条忽略 wrap_width 的单行排版,
                // 见 gui-render-gotchas.md)。
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut s)
                        .hint_text(crate::theme::hint_text(t, "留空 = 继承"))
                        .desired_width(field_w(
                            ui.available_width(),
                            FIELD_W_M,
                            TEXT_EDIT_MARGIN_X,
                        )),
                );
                if resp.changed() {
                    a.work_dir = if s.trim().is_empty() { None } else { Some(s.clone()) };
                }
            },
            line,
        );
        ui.end_row();
    });
```

注意宽度档从 `FIELD_W_L` 降到 `FIELD_W_M`：这一行现在要跟一条灰字，撑满整行的话灰字必然折到下一行，看着像两个字段。

- [ ] **Step 7: 接线时序三项**

`opt_ms` 的签名加两个参数，把「内置默认」这句话的决定权交给调用方。改 `fields.rs:109-132`：

```rust
/// 可选毫秒数:勾选框 + `DragValue`。未勾选时显示**实际会生效的值和来源** ——
/// 光给一个空框,用户不知道不填会发生什么。
///
/// `line` 由调用方算好传进来(见 `resolve_u32`):旧版在这里写死
/// 「继承(内置默认 {default} ms)」,而分组**可以**配这三个字段,那时候
/// 这句话是错的 —— 用户会以为改分组不管用(走查 10)。
///
/// `min` 不是摆设:两个「延时」类字段填 0 就是「不等」,语义正常;而「就绪超时」
/// 填 0 意味着 `run()` 的 `sleep(0)` 必然抢在首字节前面,自动化每次都被跳过,
/// 状态栏还会打出「自动化已跳过:0s 未收到远端输出」—— `status_text` 那里
/// 特意用 `div_ceil` 就是为了永不出现「0s」(见 `sub_second_timeout_rounds_up_
/// so_it_never_reads_zero`),但 `div_ceil` 拦不住字面 0。用下界从源头挡掉。
fn opt_ms(
    ui: &mut Ui,
    t: &Theme,
    id: &str,
    v: &mut Option<u32>,
    default: u32,
    min: u32,
    max: u32,
    line: Option<String>,
) {
    // `push_id` 而不是 `let _ = id;` —— 三个延时框长得一模一样,勾选框的 id 靠
    // 位置生成。一旦某一行因为条件渲染消失,后面几行的位置 id 会整体前移,
    // egui 会把上一行的勾选状态套到下一行上。给个显式 salt 钉死。
    ui.push_id(id, |ui| {
        ui.horizontal_wrapped(|ui| {
            let mut on = v.is_some();
            if ui.checkbox(&mut on, "").changed() {
                *v = if on { Some(default) } else { None };
            }
            match v {
                Some(ms) => {
                    ui.add(egui::DragValue::new(ms).range(min..=max).suffix(" ms"));
                }
                None => {
                    if let Some(s) = line {
                        ui.colored_label(crate::theme::c32(t.fg_dimmer), s);
                    }
                }
            }
        });
    });
}
```

（`horizontal` → `horizontal_wrapped`：新文案比旧的长了一截，窄栏下不换行会顶出面板。）

然后 `fields.rs:1136-` 的「时序」分区三处调用各加一个 `line` 实参。第一处：

```rust
    section(ui, t, "时序", &mut first);
    grid(ui, "sm_auto_timing", |ui| {
        ui.label("首字节后再等");
        let (v, src) = resolve_u32(
            up.as_ref(),
            |p| p.initial_delay_ms,
            mullion_store::automation::DEFAULT_INITIAL_DELAY_MS,
        );
        opt_ms(
            ui,
            t,
            "sm_auto_initial",
            &mut a.initial_delay_ms,
            mullion_store::automation::DEFAULT_INITIAL_DELAY_MS,
            0,
            10_000,
            a.initial_delay_ms
                .is_none()
                .then(|| inherit_row::effective_line(&format!("{v} ms"), src)),
        );
        ui.end_row();
```

其余两处（`inter_delay_ms` / `ready_timeout_ms`）照同一形状写，分别用 `DEFAULT_INTER_DELAY_MS` / `DEFAULT_READY_TIMEOUT_MS` 与各自的 `id` / 范围 —— 现有代码里那两处的 `id`、`min`、`max` 原样保留，只在末尾加 `line` 实参。

- [ ] **Step 8: 改调用点**

`editor.rs:390`：

```rust
            super::TAB_AUTOMATION => super::fields::automation(ui, t, buf, groups),
```

- [ ] **Step 9: 修既有测试**

阶段 1 起有三条测试断言旧文案（`fields.rs:1670` / `:1682` / `:1694` 附近，找 `继承(内置默认 300 ms)`）。它们的**意图**（三个延时框各自显示自己的默认值、复选框位置正确）仍然有效，只是文案变了。把三处 `find_text_pos` 的字符串改成新格式：

```rust
        let hint_pos = find_text_pos(&out.shapes, "实际生效:300 ms(内置默认)")
            .expect("「首字节后再等」应显示内置默认 300ms 的继承提示");
```

另外这些测试调用 `automation(ui, &t, &mut buf)` 的地方全部加 `&[]` 第四参。

**不要**为了让测试过而放宽断言（比如改成 `contains("300")`）—— 那会让「把两个默认值写反」这类 bug 溜过去。

- [ ] **Step 10: 跑全量测试**

```bash
cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|panicked"
```

Expected: 全 ok，新加的三条通过。

- [ ] **Step 11: 变异验证**

把 `resolve_u32` 里 `Some(v) => (v, Source::Group(name))` 改成 `Some(_) => (builtin, Source::Builtin)`（模拟「忘了看分组」），跑：

```bash
cargo test -p mullion-app inherited_timing_names_the_group 2>&1 | grep -E "test result|FAILED"
```

Expected: FAILED。改回来再跑一次确认恢复绿。

- [ ] **Step 12: 提交**

```bash
git add crates/mullion-app/src/ui/session_manager/fields.rs crates/mullion-app/src/ui/session_manager/editor.rs
git commit -m "feat(ui): 「登录后」页五个标量继承字段显示生效值与来源 (走查 10)

总开关/tmux/工作目录/三个时序全部走 inherit_row::slot。automation() 加
groups 参数——旧版 opt_ms 把「继承」的结果写死成内置默认,而分组可以配
这三个字段,那时候提示是错的,用户会以为改分组不管用。"
```

---

## Task 3: 「登录后」页的列表继承接线

**为什么：** 命令列表和环境变量是**整体覆盖**继承（`None` = 继承上游，`Some(vec![])` = 显式覆盖成空）。现在选「继承」时只说「继承上游的命令列表」，不说上游到底有几条命令 —— 用户不点开分组管理器就不知道自己会执行什么。这是走查 10 里后果最重的一处：这些命令**会真的发到远端 shell**。

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/fields.rs:914-922`（命令 `None` 分支）、`:1056-1064`（env `None` 分支）

- [ ] **Step 1: 先写失败的测试**

```rust
/// 走查 10 里后果最重的一处:继承来的命令**会真的发到远端 shell**。
/// 只说「继承上游的命令列表」而不说几条,用户就得去分组管理器里翻。
#[test]
fn inherited_commands_say_how_many_will_run() {
    let mut buf = EditorBuffer::default();
    buf.preserved_group_id = Some(mullion_store::GroupId(5));
    buf.preserved_automation.commands = None; // 继承

    let mut g = mullion_store::GroupRecord {
        id: mullion_store::GroupId(5),
        name: "生产".into(),
        tags: Vec::new(),
        terminal: Default::default(),
        appearance: Default::default(),
        network: Default::default(),
        automation: Default::default(),
    };
    g.automation.commands = Some(vec![
        mullion_store::AutomationCommand { text: "cd /srv".into(), delay_ms: None },
        mullion_store::AutomationCommand { text: "tail -f log".into(), delay_ms: None },
    ]);
    let groups = vec![g];

    let t = crate::theme::MULLION_DARK;
    let out = run_page(|ui| super::automation(ui, &t, &mut buf, &groups));
    let texts = all_text(&out.shapes);
    assert!(
        texts.iter().any(|s| s.contains("2 条") && s.contains("生产")),
        "继承提示要说清「几条、来自哪个分组」;实际画出的文字:{texts:?}"
    );
}

/// 上游也没配时,继承的结果是「一条都不执行」。这句话必须说出来 ——
/// 「继承上游的命令列表」在上游为空时读起来像「有东西但我没显示」。
#[test]
fn inherited_commands_say_nothing_will_run_when_upstream_is_empty() {
    let mut buf = EditorBuffer::default();
    buf.preserved_automation.commands = None;
    let t = crate::theme::MULLION_DARK;
    let out = run_page(|ui| super::automation(ui, &t, &mut buf, &[]));
    let texts = all_text(&out.shapes);
    assert!(
        texts.iter().any(|s| s.contains("不执行任何命令")),
        "上游为空时要明说一条都不跑;实际画出的文字:{texts:?}"
    );
}
```

- [ ] **Step 2: 运行确认它红**

```bash
cargo test -p mullion-app inherited_commands 2>&1 | grep -E "test result|FAILED"
```

Expected: 两条都 FAILED（画出来的是旧文案「继承上游的命令列表」）。

- [ ] **Step 3: 改命令的 `None` 分支**

`fields.rs:914-922`：

```rust
    match a.commands.as_mut() {
        None => {
            // 继承来的命令**会真的发到远端 shell**。只说「继承上游的命令列表」
            // 的话,用户得去分组管理器里翻才知道自己会执行什么(走查 10)。
            let (v, src) = match up.as_ref() {
                Some((name, prefs)) => match prefs.commands.as_deref() {
                    Some(cs) if !cs.is_empty() => {
                        (format!("{} 条命令", cs.len()), Source::Group(name))
                    }
                    // 分组显式覆盖成空 与 分组没配,继承下来都是「不执行」。
                    _ => ("不执行任何命令".to_string(), Source::Builtin),
                },
                None => ("不执行任何命令".to_string(), Source::NoUpstream),
            };
            inherit_row::slot(
                ui,
                t,
                |ui| {
                    if ui.button("改为自定义").clicked() {
                        reset_commands = true; // 见下方,这里借着 a.commands 不能直接赋值
                    }
                },
                Some(inherit_row::effective_line(&v, src)),
            );
        }
```

- [ ] **Step 4: 改 env 的 `None` 分支**

`fields.rs:1056-1064`：

```rust
    match a.env.as_mut() {
        None => {
            let (v, src) = match up.as_ref() {
                Some((name, prefs)) => match prefs.env.as_deref() {
                    Some(vs) if !vs.is_empty() => {
                        (format!("{} 个变量", vs.len()), Source::Group(name))
                    }
                    _ => ("不设任何变量".to_string(), Source::Builtin),
                },
                None => ("不设任何变量".to_string(), Source::NoUpstream),
            };
            inherit_row::slot(
                ui,
                t,
                |ui| {
                    if ui.button("改为自定义").clicked() {
                        reset_env = true;
                    }
                },
                Some(inherit_row::effective_line(&v, src)),
            );
        }
```

- [ ] **Step 5: 跑测试确认绿**

```bash
cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|panicked"
```

Expected: 全 ok。

- [ ] **Step 6: 确认「继承 vs 显式空」没被破坏**

这是本阶段第 4 条前置约定的核心。跑既有守护测试：

```bash
cargo test -p mullion-app commands 2>&1 | grep -E "test result|FAILED"
cargo test -p mullion-store automation 2>&1 | grep -E "test result|FAILED"
```

Expected: 全 ok。本 Task **只读** `a.commands` / `a.env` 的值来生成文本，没有任何写入 —— 唯一的写入路径仍是既有的 `reset_commands` / `reset_env`。

- [ ] **Step 7: 提交**

```bash
git add crates/mullion-app/src/ui/session_manager/fields.rs
git commit -m "feat(ui): 继承的命令与环境变量显示条数和来源 (走查 10)

「继承上游的命令列表」不说几条——而这些命令会真的发到远端 shell。改成
「实际生效:2 条命令(来自分组「生产」)」;上游为空时明说「不执行任何命令」,
不再让人以为「有东西但没显示」。"
```

---

## Task 4: 「连接」页的继承接线 + 术语统一 + 协议提示

**为什么：** 走查 19 报的是术语不一致：跳板和代理写「继承分组」，tmux 和时序写「继承」，命令和 env 写「继承上游的…」。三种说法指同一件事。统一成**「继承」**，来源交给右边的灰字 —— 走查原文建议统一成「继承（分组）」，但那对时序三项是**错的**：它们的上游默认来自内置默认而非分组。

顺带把协议下拉的 sftp 标成未实现（走查 19 后半）：现在 `ssh` / `sftp` 平级可选，选了 sftp 保存后连不上，没有任何提示。

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/fields.rs:202-212`（协议）、`:426-457`（跳板）、`:461-470`（删 `inherit_hint`）、`:729-740`（代理）

- [ ] **Step 1: 先写失败的测试**

```rust
/// 走查 19:同一件事三种说法。全页扫一遍,不许再出现「继承分组」
/// 「继承上游」这类变体 —— 来源由灰字负责说。
#[test]
fn inheritance_is_called_the_same_thing_on_every_page() {
    let t = crate::theme::MULLION_DARK;
    let mut buf = EditorBuffer::default();
    buf.jump_mode = JumpModeUi::Inherit;
    buf.proxy_mode = ProxyModeUi::Inherit;

    let mut pages: Vec<String> = Vec::new();
    let out = run_page(|ui| super::basic(ui, &t, &mut buf, &[], &[], None, SecretPresence::default()));
    pages.extend(all_text(&out.shapes));
    let out = run_page(|ui| super::automation(ui, &t, &mut buf, &[]));
    pages.extend(all_text(&out.shapes));

    for s in &pages {
        assert!(
            !s.contains("继承分组"),
            "还有「继承分组」的旧说法:{s:?}"
        );
        assert!(
            !s.contains("继承上游"),
            "还有「继承上游」的旧说法:{s:?}"
        );
    }
    assert!(
        pages.iter().any(|s| s == "继承"),
        "统一后的「继承」一个都没出现,说明改过头了:{pages:?}"
    );
}

/// 走查 19 后半:sftp 在下拉里跟 ssh 平级,选了保存后连不上,
/// 而界面上没有任何提示。
#[test]
fn sftp_is_marked_unimplemented_in_the_protocol_picker() {
    let t = crate::theme::MULLION_DARK;
    let mut buf = EditorBuffer::default();
    buf.protocol = mullion_store::Protocol::Sftp;
    let out = run_page(|ui| super::basic(ui, &t, &mut buf, &[], &[], None, SecretPresence::default()));
    let texts = all_text(&out.shapes);
    assert!(
        texts.iter().any(|s| s.contains("未实现")),
        "选中 sftp 时必须提示它还没实现;实际画出的文字:{texts:?}"
    );
}
```

- [ ] **Step 2: 运行确认它红**

```bash
cargo test -p mullion-app -- inheritance_is_called sftp_is_marked 2>&1 | grep -E "test result|FAILED"
```

Expected: 两条都 FAILED。

- [ ] **Step 3: 改跳板**

`fields.rs:426-457` 整段替换：

```rust
    section(ui, t, "跳板", first);
    grid(ui, "sm_basic_jump", |ui| {
        ui.label("跳板");
        // 「继承分组」→「继承」:同一件事全项目一个说法(走查 19)。
        // 来源不写进按钮文字,交给下面那行灰字 —— 未分组时上游根本不是
        // 「分组」,按钮上写死「分组」就是错的。
        let line = matches!(buf.jump_mode, JumpModeUi::Inherit).then(|| {
            let (v, src) = match inherit_row::upstream(buf.preserved_group_id, groups) {
                Some(g) => match &g.network.jump {
                    Some(chain) if !chain.is_empty() => {
                        (format!("{} 跳", chain.len()), Source::Group(&g.name))
                    }
                    _ => ("不走跳板".to_string(), Source::Builtin),
                },
                None => ("不走跳板".to_string(), Source::NoUpstream),
            };
            inherit_row::effective_line(&v, src)
        });
        inherit_row::slot(
            ui,
            t,
            |ui| {
                // 与「认证方式」同样的选中态处理:egui 默认的 gamma_multiply 底色
                // 在深色面板上偏暗,分不出选中态。见 `auth()` 里那段注释。
                let vis = &mut ui.visuals_mut().selection;
                vis.bg_fill = crate::theme::c32(t.accent).linear_multiply(0.35);
                ui.selectable_value(&mut buf.jump_mode, JumpModeUi::None, "无");
                ui.selectable_value(&mut buf.jump_mode, JumpModeUi::Inherit, "继承");
                ui.selectable_value(&mut buf.jump_mode, JumpModeUi::Custom, "自定义");
            },
            line,
        );
        ui.end_row();

        if matches!(buf.jump_mode, JumpModeUi::Custom) {
            ui.label("跳板链");
            ui.vertical(|ui| chain_editor(ui, t, buf, sessions, editing));
            ui.end_row();
        }
    });
```

- [ ] **Step 4: 删掉 `inherit_hint`**

`fields.rs:459-470` 整个函数删除 —— 它唯一的调用方刚被 `inherit_row` 替掉，留着会触发 `dead_code`（clippy `-D warnings` 直接失败）。

- [ ] **Step 5: 改代理**

`fields.rs:729-740`：

```rust
    section(ui, t, "代理", first);
    grid(ui, "sm_net_proxy", |ui| {
        ui.label("代理");
        let line = matches!(buf.proxy_mode, ProxyModeUi::Inherit).then(|| {
            let (v, src) = match inherit_row::upstream(buf.preserved_group_id, groups) {
                Some(g) => match &g.network.proxy {
                    Some(mullion_store::ProxyChoice::Direct) => {
                        ("直连".to_string(), Source::Group(&g.name))
                    }
                    Some(mullion_store::ProxyChoice::Socks5(e)) => {
                        (format!("SOCKS5 {}:{}", e.host, e.port), Source::Group(&g.name))
                    }
                    Some(mullion_store::ProxyChoice::HttpConnect(e)) => {
                        (format!("HTTP {}:{}", e.host, e.port), Source::Group(&g.name))
                    }
                    None => ("直连".to_string(), Source::Builtin),
                },
                None => ("直连".to_string(), Source::NoUpstream),
            };
            inherit_row::effective_line(&v, src)
        });
        inherit_row::slot(
            ui,
            t,
            |ui| {
                // 窄栏放不下四个模式按钮:`ui.horizontal` 不换行会把这一格撑宽 8px,
                // 顶出面板 —— 走查 P0-1 同族缺陷。外层 `slot` 已经是
                // `horizontal_wrapped`,这里不用再套一层。
                ui.selectable_value(&mut buf.proxy_mode, ProxyModeUi::Inherit, "继承");
                ui.selectable_value(&mut buf.proxy_mode, ProxyModeUi::Direct, "直连");
                ui.selectable_value(&mut buf.proxy_mode, ProxyModeUi::Socks5, "SOCKS5");
                ui.selectable_value(&mut buf.proxy_mode, ProxyModeUi::HttpConnect, "HTTP");
            },
            line,
        );
        ui.end_row();
```

`network()` 现在需要 `groups` 与 `buf.preserved_group_id`。它的签名是 `network(ui, t, buf, presence, first)` —— `buf` 已经有 `preserved_group_id`，只需再加 `groups`：

```rust
pub(super) fn network(
    ui: &mut Ui,
    t: &Theme,
    buf: &mut EditorBuffer,
    groups: &[GroupRecord],
    presence: SecretPresence,
    first: &mut bool,
) {
```

`basic()` 里调用 `network(...)` 的那一行加 `groups` 实参（`basic` 本来就收 `groups`）。

- [ ] **Step 6: 改协议下拉**

`fields.rs:202-212`：

```rust
        ui.label("协议");
        // sftp 在数据模型里存在(`Protocol::Sftp`),但拨号路径还没实现 ——
        // 平级摆着会让人选了、存了、然后连不上而不知道为什么(走查 19)。
        // 保留可选而不是删掉:spec §4.10 已登记它,删了将来还得加回来,
        // 且已经存了 sftp 的会话不该在 UI 上凭空消失。
        inherit_row::slot(
            ui,
            t,
            |ui| {
                egui::ComboBox::from_id_salt("sm_protocol")
                    .selected_text(match buf.protocol {
                        Protocol::Ssh => "ssh",
                        Protocol::Sftp => "sftp",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut buf.protocol, Protocol::Ssh, "ssh");
                        ui.selectable_value(&mut buf.protocol, Protocol::Sftp, "sftp(未实现)");
                    });
            },
            matches!(buf.protocol, Protocol::Sftp)
                .then(|| "sftp 尚未实现,连接会按 ssh 处理".to_string()),
        );
        ui.end_row();
```

（复用 `slot` 而不是另写一遍 `horizontal_wrapped` + 灰字：它就是「控件 + 一行灰说明」这个形状，跟继不继承无关。）

- [ ] **Step 7: 跑测试**

```bash
cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|panicked"
```

Expected: 全 ok。若有既有测试断言「继承分组」字样（`fields.rs:2117` 那条 `for opt in ["无", "继承分组", "自定义"]`），把它改成 `["无", "继承", "自定义"]` —— 那条测试的意图是「三个模式按钮都在」，文案变了意图没变。

- [ ] **Step 8: 变异验证**

把 `slot` 里的 `if let Some(s) = line` 改成 `if let Some(_s) = line`（画不出灰字），跑：

```bash
cargo test -p mullion-app -- inherited_ sftp_is_marked 2>&1 | grep -E "test result|FAILED"
```

Expected: 多条 FAILED（Task 2/3/4 的测试全靠这一行）。改回来确认恢复绿。

- [ ] **Step 9: 提交**

```bash
git add crates/mullion-app/src/ui/session_manager/fields.rs
git commit -m "feat(ui): 「连接」页继承可见 + 术语统一 + sftp 标未实现 (走查 11/19)

跳板/代理的「继承分组」改「继承」,来源交给灰字——未分组时上游根本不是
分组,按钮上写死「分组」是错的。走查建议的「继承(分组)」对时序三项同样
错(它们的上游是内置默认)。删掉 inherit_hint,它的活由 inherit_row 接了。"
```

---

## Task 5: `tab_badge.rs` —— Tab 角标的三态判据

**为什么：** 现在 Tab 条只有「缺必填项」一种红点。用户配完一圈回头看，四个 Tab 长得一模一样，不知道哪几页自己动过。走查 9 要的是**单一状态位**：`缺项红● > 已配置灰· > 无`，优先级从高到低，一个 Tab 只显示一个符号。

**关键设计：「已配置」= 偏离新建默认，不是「非空」。** 「连接」页永远有名称和主机（必填），拿「非空」当判据的话它恒亮，等于没有。走查明确「连接页判据只算跳板/代理」。

**Files:**
- Create: `crates/mullion-app/src/ui/session_manager/tab_badge.rs`
- Modify: `crates/mullion-app/src/ui/session_manager/mod.rs`

- [ ] **Step 1: 先写失败的测试**

新建 `crates/mullion-app/src/ui/session_manager/tab_badge.rs`，只写测试模块：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::session_manager::validate::Missing;
    use crate::ui::session_manager::{
        AuthKindUi, EditorBuffer, JumpModeUi, ProxyModeUi, TAB_APPEARANCE, TAB_AUTH,
        TAB_AUTOMATION, TAB_CONNECT,
    };

    /// 新建会话四页全空,不该有任何角标 —— 满屏小点等于没有信息。
    #[test]
    fn a_fresh_buffer_has_no_badges_at_all() {
        let buf = EditorBuffer::default();
        let none = Missing::default();
        for tab in [TAB_CONNECT, TAB_AUTH, TAB_AUTOMATION, TAB_APPEARANCE] {
            assert_eq!(badge_of(tab, none, &buf), Badge::None, "tab {tab} 不该有角标");
        }
    }

    /// 缺项压过已配置:两者同时成立时只显示红点。用户先要被带去补必填,
    /// 「这页你配过东西」是次要信息(走查 9 的「单一状态位」)。
    #[test]
    fn missing_outranks_configured() {
        let mut buf = EditorBuffer::default();
        buf.proxy_mode = ProxyModeUi::Socks5; // 「连接」页已配置
        let missing = Missing {
            name: true,
            host: false,
            user: false,
        };
        assert_eq!(missing.tab(), Some(TAB_CONNECT));
        assert_eq!(badge_of(TAB_CONNECT, missing, &buf), Badge::Missing);
    }

    /// 「连接」页的已配置判据**只算跳板和代理**。名称/主机是必填,
    /// 拿它们当判据的话这个点恒亮,等于没有(走查 9)。
    #[test]
    fn connect_badge_ignores_name_and_host() {
        let mut buf = EditorBuffer::default();
        buf.name = "web01".into();
        buf.host = "10.0.0.1".into();
        buf.port = "2222".into();
        assert_eq!(
            badge_of(TAB_CONNECT, Missing::default(), &buf),
            Badge::None,
            "填了名称/主机/端口不算「配置过连接方式」"
        );

        buf.proxy_mode = ProxyModeUi::Direct;
        assert_eq!(
            badge_of(TAB_CONNECT, Missing::default(), &buf),
            Badge::Configured,
            "显式直连是对分组的覆盖,算配置过"
        );
    }

    /// 「继承」不算配置过 —— 它恰恰是「我什么都没决定」。
    #[test]
    fn inheriting_is_not_configuring() {
        let mut buf = EditorBuffer::default();
        buf.jump_mode = JumpModeUi::Inherit;
        buf.proxy_mode = ProxyModeUi::Inherit;
        assert_eq!(badge_of(TAB_CONNECT, Missing::default(), &buf), Badge::None);
    }

    /// 选了「自定义」但链是空的,也不算配置过 —— 空链的实际效果等于「无」。
    #[test]
    fn an_empty_custom_jump_chain_is_not_configured() {
        let mut buf = EditorBuffer::default();
        buf.jump_mode = JumpModeUi::Custom;
        buf.jump_chain.clear();
        assert_eq!(badge_of(TAB_CONNECT, Missing::default(), &buf), Badge::None);

        buf.jump_chain.push(mullion_store::SessionId(3));
        assert_eq!(
            badge_of(TAB_CONNECT, Missing::default(), &buf),
            Badge::Configured
        );
    }

    /// 其余三页各自的判据。
    #[test]
    fn other_pages_use_their_own_criteria() {
        let none = Missing::default();

        let mut auth = EditorBuffer::default();
        auth.auth_kind = AuthKindUi::PublicKey;
        assert_eq!(badge_of(TAB_AUTH, none, &auth), Badge::Configured);

        let mut auto = EditorBuffer::default();
        auto.preserved_automation.enabled = Some(false);
        assert_eq!(badge_of(TAB_AUTOMATION, none, &auto), Badge::Configured);

        let mut look = EditorBuffer::default();
        look.preserved_appearance.color = Some(mullion_store::ColorSpec {
            hex: "#ff0000".into(),
            apply_to: vec![mullion_store::ColorTarget::ListItem],
        });
        assert_eq!(badge_of(TAB_APPEARANCE, none, &look), Badge::Configured);
    }
}
```

- [ ] **Step 2: 运行确认编译失败**

```bash
cargo test -p mullion-app tab_badge 2>&1 | tail -20
```

Expected: `cannot find function badge_of` / `cannot find type Badge`。

- [ ] **Step 3: 写实现**

在测试模块**之前**插入：

```rust
//! Tab 角标(走查 9)。**纯函数,零 egui**——「这一页算不算配置过」的判据
//! 全是字段比对,放进渲染代码里就再也测不动了。
//!
//! **单一状态位**:一个 Tab 只显示一个符号,优先级 `缺项 > 已配置 > 无`。
//! 走查原文列了「红点表缺项、灰点表已配置、角标数字表几处覆盖」三套并存的
//! 方案,那会让四个 Tab 上同时挂三种记号 —— 信息密度上去了,可读性没了。

use crate::ui::session_manager::validate::Missing;
use crate::ui::session_manager::{
    AuthKindUi, EditorBuffer, JumpModeUi, ProxyModeUi, TAB_APPEARANCE, TAB_AUTH, TAB_AUTOMATION,
    TAB_CONNECT,
};

/// 一个 Tab 的角标状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Badge {
    /// 这一页有必填项没填。红点。
    Missing,
    /// 这一页有偏离默认的设置。灰点。
    Configured,
    /// 什么都不画。
    None,
}

/// 某个 Tab 该显示哪个角标。
///
/// 「已配置」的判据是**偏离新建默认**,不是「非空」:「连接」页永远有名称和
/// 主机(必填),拿非空当判据的话这个点恒亮,等于没有。所以这一页只算
/// 跳板与代理 —— 它们回答的是「怎么走到这台机器」,才是这一页真正的可选配置。
pub fn badge_of(tab: usize, missing: Missing, buf: &EditorBuffer) -> Badge {
    if missing.tab() == Some(tab) {
        return Badge::Missing;
    }
    let configured = match tab {
        TAB_CONNECT => {
            // 「继承」不算配置过 —— 它恰恰是「我什么都没决定」。
            // 「自定义」但链为空也不算:空链的实际效果等于「无」。
            !matches!(buf.proxy_mode, ProxyModeUi::Inherit)
                || (matches!(buf.jump_mode, JumpModeUi::Custom) && !buf.jump_chain.is_empty())
        }
        // 密码是默认认证方式,换成公钥才算动过。凭据本身填没填不看:
        // 编辑已有会话时密码框恒为空(store 不回吐明文),看它会误报。
        TAB_AUTH => matches!(buf.auth_kind, AuthKindUi::PublicKey),
        // 整个分节跟默认值比。`AutomationPrefs` 全字段 `Option`,
        // 默认即「全继承」,任何一项被显式设置都会让它不等于默认。
        TAB_AUTOMATION => buf.preserved_automation != mullion_store::AutomationPrefs::default(),
        TAB_APPEARANCE => {
            buf.preserved_appearance.icon.is_some() || buf.preserved_appearance.color.is_some()
        }
        _ => false,
    };
    if configured {
        Badge::Configured
    } else {
        Badge::None
    }
}
```

- [ ] **Step 4: 挂到模块树**

`mod.rs` 里 `mod inherit_row;` 之后加：

```rust
mod tab_badge;
```

- [ ] **Step 5: 跑测试**

```bash
cargo test -p mullion-app tab_badge 2>&1 | grep -E "test result|FAILED"
```

Expected: `6 passed`。

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-app/src/ui/session_manager/tab_badge.rs crates/mullion-app/src/ui/session_manager/mod.rs
git commit -m "feat(ui): Tab 角标的三态纯判据 (走查 9)

单一状态位:缺项 > 已配置 > 无。「已配置」判据是偏离新建默认而非非空——
「连接」页永远有名称主机,拿非空判会恒亮。这一页只算跳板与代理。"
```

---

## Task 6: Tab 条渲染三态角标

**为什么：** Task 5 的判据要接到界面上。难点是**着色**：角标要红/灰两色，而 Tab 名本身必须保留 egui 的选中态/hover 配色。

**关键机制（已在 egui 0.30 源码核实）：**
- `SelectableLabel::ui` 最后一行是 `ui.painter().galley(text_pos, galley, visuals.text_color())`（`egui-0.30.0/src/widgets/selected_label.rs:81`），第三个参数是 **fallback color**。
- epaint 只把 galley 里颜色等于 `Color32::PLACEHOLDER` 的部分替换成 fallback（`epaint-0.30.0/src/tessellator.rs:1843`）。
- `WidgetText` 有 `From<LayoutJob>`（`widget_text.rs:756`），且 `into_galley_impl` 对 `LayoutJob` 分支**只改 wrap，不碰颜色**（`widget_text.rs:698`）。

所以：Tab 名那一段给 `Color32::PLACEHOLDER`（保留选中态高亮），角标那一段给显式颜色。

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/editor.rs:270-287`

- [ ] **Step 1: 先写失败的测试**

在 `editor.rs` 的 `mod tests` 里加：

```rust
/// 角标必须是**两段不同颜色**的一个 galley:Tab 名保留 egui 的选中态配色
/// (`Color32::PLACEHOLDER`,由 `SelectableLabel` 用 `visuals.text_color()`
/// 填),角标自己是显式的红/灰。
///
/// 整条 label 染成一色的话,选中的那一页文字会跟着角标变红 —— 那是渲染
/// 出来才看得见的 bug,编译器和普通文字断言都拦不住。
#[test]
fn tab_badge_colors_only_the_dot_not_the_tab_name() {
    let t = crate::theme::MULLION_DARK;
    let mut ui_state = UiState::default();
    let mut buf = EditorBuffer::default();
    // 「登录后」页配过东西 → 灰点;必填齐全 → 没有红点。
    buf.name = "web01".into();
    buf.host = "10.0.0.1".into();
    buf.user = "root".into();
    buf.preserved_automation.enabled = Some(false);
    ui_state.editor = Some(buf);

    let ctx = egui::Context::default();
    let run = || {
        ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                show(ui, &t, &mut ui_state, &[], &[], SecretPresence::default());
            });
        })
    };
    let _ = run();
    let out = run();

    let job = find_galley_job(&out.shapes, "登录后").expect("没找到「登录后」这个 Tab");
    assert!(
        job.sections.len() >= 2,
        "Tab 名和角标必须分段,否则没法各自着色:{:?}",
        job.text
    );
    let name_color = job.sections[0].format.color;
    let dot_color = job.sections[1].format.color;
    assert_eq!(
        name_color,
        egui::Color32::PLACEHOLDER,
        "Tab 名必须留给 SelectableLabel 填色,否则选中态高亮会失效"
    );
    assert_eq!(
        dot_color,
        crate::theme::c32(t.fg_dimmer),
        "「已配置」角标应该是 fg_dimmer 灰"
    );
}

/// 缺必填时角标是 danger 红,且压过「已配置」。
#[test]
fn missing_badge_is_danger_red_and_wins() {
    let t = crate::theme::MULLION_DARK;
    let mut ui_state = UiState::default();
    let mut buf = EditorBuffer::default();
    buf.host = "10.0.0.1".into();
    buf.user = "root".into();
    // 名称空 → 「连接」页缺项;同时给「连接」页配上代理 → 也满足「已配置」。
    buf.proxy_mode = ProxyModeUi::Socks5;
    ui_state.editor = Some(buf);

    let ctx = egui::Context::default();
    let run = || {
        ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                show(ui, &t, &mut ui_state, &[], &[], SecretPresence::default());
            });
        })
    };
    let _ = run();
    let out = run();

    let job = find_galley_job(&out.shapes, "连接").expect("没找到「连接」这个 Tab");
    assert_eq!(
        job.sections[1].format.color,
        crate::theme::c32(t.danger),
        "缺项角标应该是 danger 红,且压过灰点"
    );
}

/// 按 Tab 名找到那个 galley 的 `LayoutJob`。
fn find_galley_job(
    shapes: &[egui::epaint::ClippedShape],
    starts_with: &str,
) -> Option<egui::text::LayoutJob> {
    fn walk(
        shapes: impl Iterator<Item = egui::Shape>,
        starts_with: &str,
        out: &mut Option<egui::text::LayoutJob>,
    ) {
        for s in shapes {
            match s {
                egui::Shape::Text(ts) => {
                    if out.is_none() && ts.galley.text().starts_with(starts_with) {
                        *out = Some((*ts.galley.job).clone());
                    }
                }
                egui::Shape::Vec(v) => walk(v.into_iter(), starts_with, out),
                _ => {}
            }
        }
    }
    let mut out = None;
    walk(shapes.iter().map(|c| c.shape.clone()), starts_with, &mut out);
    out
}
```

- [ ] **Step 2: 运行确认它红**

```bash
cargo test -p mullion-app -- tab_badge_colors missing_badge_is 2>&1 | grep -E "test result|FAILED"
```

Expected: FAILED —— 现在整条 label 是一个 `RichText`，只有一段。

- [ ] **Step 3: 改 Tab 条**

`editor.rs:270-287` 整段替换：

```rust
    // Tab 条
    ui.horizontal(|ui| {
        for (i, name) in TABS.iter().enumerate() {
            // 走查 9:单一状态位。缺项红点(F91 原有)压过「这页配过东西」灰点。
            //
            // 用 `LayoutJob` 而不是 `format!("{name} ●")` + `RichText::color`:
            // 后者只能给整条 label 一个颜色,选中的那一页会连 Tab 名一起变红。
            // `LayoutJob` 能分段着色,而 Tab 名那段给 `Color32::PLACEHOLDER`
            // 就还能保留 egui 的选中态/hover 配色 —— `SelectableLabel` 最后
            // 是 `painter().galley(pos, galley, visuals.text_color())`,
            // 第三参是 **fallback**,epaint 只替换 PLACEHOLDER 的部分
            // (egui-0.30.0 selected_label.rs:81 / epaint-0.30.0
            //  tessellator.rs:1843)。
            let badge = super::tab_badge::badge_of(i, missing, buf);
            let font = egui::TextStyle::Button.resolve(ui.style());
            let mut job = egui::text::LayoutJob::default();
            job.append(
                name,
                0.0,
                egui::TextFormat {
                    font_id: font.clone(),
                    color: egui::Color32::PLACEHOLDER,
                    ..Default::default()
                },
            );
            if let Some((sym, color)) = match badge {
                super::tab_badge::Badge::Missing => Some(("●", theme::c32(t.danger))),
                super::tab_badge::Badge::Configured => Some(("·", theme::c32(t.fg_dimmer))),
                super::tab_badge::Badge::None => None,
            } {
                job.append(
                    sym,
                    4.0,
                    egui::TextFormat {
                        font_id: font,
                        color,
                        ..Default::default()
                    },
                );
            }
            if ui
                .selectable_label(ui_state.editor_tab == i, job)
                .clicked()
            {
                ui_state.editor_tab = i;
            }
        }
    });
```

- [ ] **Step 4: 跑测试**

```bash
cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|panicked"
```

Expected: 全 ok。若既有测试断言过 `"连接 ●"` 这种拼接串（grep `● `），改成按 `starts_with` 判定。

- [ ] **Step 5: 变异验证**

把 `badge_of` 里 `if missing.tab() == Some(tab) { return Badge::Missing; }` 整句删掉，跑：

```bash
cargo test -p mullion-app missing_badge_is 2>&1 | grep -E "test result|FAILED"
```

Expected: FAILED（角标降级成灰点）。改回来确认恢复绿。

再把 `Color32::PLACEHOLDER` 改成 `theme::c32(t.fg)`，跑 `tab_badge_colors_only_the_dot`，Expected: FAILED。改回来。

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-app/src/ui/session_manager/editor.rs
git commit -m "feat(ui): Tab 条三态角标 (走查 9)

缺项红● / 已配置灰· / 无,单一状态位。用 LayoutJob 分段着色而不是给整条
label 一个颜色——后者会让选中那一页的 Tab 名跟着角标变红。Tab 名那段给
Color32::PLACEHOLDER,保留 SelectableLabel 的选中态配色(见代码里的
egui/epaint 源码行号)。"
```

---

## Task 7: 跳板链路径预览与前置校验

**为什么：** 走查 12 说「主机解析没有语境」—— 配好一条三跳的链，界面上只是三行会话名，看不出最终的连接路径长什么样；而环、自引用、悬空引用要等到**真正拨号**的时候才报错。数据层三种检测早就做全了（`jump.rs` 有三条守护测试），只需把结果提前到编辑时。

**关键约束（本项目陷阱 T3）：** `expand_chain_of` 要 `BTreeMap`，而 UI 手上是 `&[SessionRecord]`。每帧给几十条会话建一次 `BTreeMap` 是白烧 CPU。所以**只在链非空时**才构造，且构造与展开都放在 `chain_editor` 的末尾一次性做完。

**Files:**
- Create: `crates/mullion-app/src/ui/session_manager/jump_preview.rs`
- Modify: `crates/mullion-app/src/ui/session_manager/mod.rs`、`fields.rs`（`chain_editor` 末尾）

- [ ] **Step 1: 先写失败的测试**

新建 `jump_preview.rs`，只写测试模块：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mullion_store::{
        Auth, AuthKind, Connection, Identity, JumpRef, NetworkPrefs, Protocol, SessionId,
        SessionRecord,
    };

    fn sess(id: u64, name: &str, host: &str, jump: Option<Vec<u64>>) -> SessionRecord {
        SessionRecord {
            id: SessionId(id),
            identity: Identity {
                name: name.into(),
                group_id: None,
                tags: Vec::new(),
                note: String::new(),
            },
            connection: Connection {
                host: host.into(),
                port: 22,
                protocol: Protocol::Ssh,
            },
            auth: Auth {
                user: "root".into(),
                kind: AuthKind::Password,
            },
            terminal: Default::default(),
            appearance: Default::default(),
            network: NetworkPrefs {
                proxy: None,
                jump: jump.map(|v| v.into_iter().map(|i| JumpRef(SessionId(i))).collect()),
            },
            automation: Default::default(),
        }
    }

    /// 预览要按**拨号顺序**读得通,并且把代理放在第一跳之前 ——
    /// 连接路径是 `本机 →(代理)→ 第一跳 →…→ 目标`。
    #[test]
    fn preview_reads_in_dial_order_with_proxy_first() {
        let sessions = vec![
            sess(1, "堡垒", "bastion.example", None),
            sess(2, "网关", "gw.internal", None),
        ];
        let line = preview(
            &[SessionId(1), SessionId(2)],
            &sessions,
            Some("SOCKS5"),
            "web01",
        );
        assert_eq!(line, "本机 →(SOCKS5)→ 堡垒 → 网关 → web01");
    }

    /// 没配代理时不该凭空冒出一个 `→()→`。
    #[test]
    fn preview_omits_the_proxy_hop_when_there_is_none() {
        let sessions = vec![sess(1, "堡垒", "bastion.example", None)];
        assert_eq!(
            preview(&[SessionId(1)], &sessions, None, "web01"),
            "本机 → 堡垒 → web01"
        );
    }

    /// 悬空引用在预览里也要看得见是**哪一跳**没了 —— 光说「有一跳不存在」
    /// 用户得挨个点开会话去找。
    #[test]
    fn preview_marks_a_dangling_hop_inline() {
        let sessions = vec![sess(1, "堡垒", "bastion.example", None)];
        let line = preview(&[SessionId(1), SessionId(42)], &sessions, None, "web01");
        assert!(line.contains("#42"), "悬空跳要带出 id:{line}");
        assert!(line.contains("已删除"), "悬空跳要说明原因:{line}");
    }

    /// 自引用是环的一种。数据层早就能测出来(`jump::tests::
    /// self_reference_is_a_cycle`),走查 12 要的只是把它提前到编辑时。
    #[test]
    fn check_catches_a_self_referencing_hop_before_dialing() {
        // #1 的跳板是它自己。
        let sessions = vec![sess(1, "堡垒", "bastion.example", Some(vec![1]))];
        let msg = check(&[SessionId(1)], &sessions).expect("自引用必须被拦下");
        assert!(msg.contains("环"), "错误要说人话:{msg}");
    }

    /// 两条会话互为跳板 —— 拨号时会无限递归,必须在编辑时就拦。
    #[test]
    fn check_catches_a_two_node_cycle() {
        let sessions = vec![
            sess(1, "A", "a.example", Some(vec![2])),
            sess(2, "B", "b.example", Some(vec![1])),
        ];
        assert!(check(&[SessionId(1)], &sessions).is_some(), "互引用必须被拦下");
    }

    /// 一条干净的链不该报任何错 —— 误报比不报更烦人,用户会学会无视它。
    #[test]
    fn check_stays_quiet_on_a_healthy_chain() {
        let sessions = vec![
            sess(1, "堡垒", "bastion.example", None),
            sess(2, "网关", "gw.internal", Some(vec![1])),
        ];
        assert_eq!(check(&[SessionId(2)], &sessions), None);
    }
}
```

- [ ] **Step 2: 运行确认编译失败**

```bash
cargo test -p mullion-app jump_preview 2>&1 | tail -20
```

Expected: `cannot find function preview` / `cannot find function check`。

- [ ] **Step 3: 写实现**

在测试模块之前插入：

```rust
//! 跳板链的路径预览与前置校验(走查 12)。**纯函数,零 egui。**
//!
//! 环 / 自引用 / 悬空 / 超深四种检测在 `mullion_store::jump` 里早就做全了
//! (那里有四条守护测试)。这里**不重新实现**任何一条 —— 只是把同一个内核
//! 的结果提前到编辑时,并把 `StoreError` 翻成用户看得懂的话。重新实现一遍
//! 的话,两套判据迟早漂移,结果就是「编辑时说没问题、拨号时连不上」。

use std::collections::BTreeMap;

use mullion_store::{GroupId, GroupRecord, JumpRef, SessionId, SessionRecord, StoreError};

/// 一行连接路径:`本机 →(SOCKS5)→ 堡垒 → 网关 → web01`。
///
/// `proxy` 是代理的短标签(`"SOCKS5"` / `"HTTP"` / `None`)。代理排在第一跳
/// **之前**:连接路径就是 `本机 →(代理)→ 第一跳 →…→ 目标`,页面上「代理」
/// 分区也排在「跳板」之前,顺序一致才读得通。
///
/// 悬空跳原地标出来而不是跳过:光说「有一跳不存在」的话,用户得挨个点开
/// 会话去找是哪一跳。
pub fn preview(
    chain: &[SessionId],
    sessions: &[SessionRecord],
    proxy: Option<&str>,
    target: &str,
) -> String {
    let mut parts = vec!["本机".to_string()];
    let head = match proxy {
        Some(p) => format!("→({p})→"),
        None => "→".to_string(),
    };
    let mut sep = head;
    for id in chain {
        let name = match sessions.iter().find(|s| s.id == *id) {
            Some(s) => s.identity.name.clone(),
            None => format!("#{} 已删除", id.0),
        };
        parts.push(format!("{sep} {name}"));
        sep = "→".to_string();
    }
    parts.push(format!("{sep} {target}"));
    // 第一段是「本机」,它后面的分隔符已经跟在下一段前面了。
    parts.join(" ").replace("  ", " ")
}

/// 拨号前把 `mullion_store::jump` 的四种失败翻成人话。干净时返回 `None`。
///
/// **不自己判环**:调的就是拨号时用的同一个 `expand_chain_of`。
pub fn check(chain: &[SessionId], sessions: &[SessionRecord]) -> Option<String> {
    if chain.is_empty() {
        return None;
    }
    // 只在链非空时才建索引:每帧给几十条会话建 `BTreeMap` 是白烧 CPU
    // (本项目陷阱 T3)。`chain_editor` 只在「自定义」模式下渲染,
    // 这里再加一道空链短路。
    let by_id: BTreeMap<SessionId, SessionRecord> =
        sessions.iter().map(|s| (s.id, s.clone())).collect();
    // 分组只影响跳板会话**自身**继承来的跳板设置。编辑器手上没有分组的
    // 全量索引,传空表 = 「跳板会话自己不从分组继承跳板」。这会让一种
    // 罕见情况漏报(A 的跳板由分组配、且构成环),但不会误报 —— 宁可漏,
    // 不可在干净的链上弹红字。拨号时 `expand_chain` 用的是全量索引,
    // 真有环仍然拦得住。
    let groups: BTreeMap<GroupId, GroupRecord> = BTreeMap::new();
    let refs: Vec<JumpRef> = chain.iter().map(|id| JumpRef(*id)).collect();
    match mullion_store::jump::expand_chain_of(&refs, &by_id, &groups) {
        Ok(_) => None,
        Err(StoreError::JumpCycle(id)) => Some(format!(
            "跳板链存在环,经过会话 #{} —— 拨号时会直接失败,请检查该会话自己的跳板设置",
            id.0
        )),
        Err(StoreError::JumpDangling(id)) => Some(format!(
            "第 #{} 跳指向的会话已被删除 —— 拨号会硬失败(不会悄悄改走直连)",
            id.0
        )),
        Err(StoreError::JumpTooDeep(_)) => Some(format!(
            "展开后超过 {} 跳 —— 每多一跳都乘一次延迟,几乎必是配错了",
            mullion_store::jump::MAX_JUMP_DEPTH
        )),
        Err(e) => Some(e.to_string()),
    }
}
```

- [ ] **Step 4: 挂到模块树**

`mod.rs` 里加 `mod jump_preview;`（按字母序在 `mod inherit_row;` 之后）。

- [ ] **Step 5: 跑纯函数测试**

```bash
cargo test -p mullion-app jump_preview 2>&1 | grep -E "test result|FAILED"
```

Expected: `6 passed`。若 `preview` 的空格拼接跟断言差一个空格，**改实现不改断言** —— 断言里那句话就是要显示给用户的成品。

- [ ] **Step 6: 接到 `chain_editor`**

在 `fields.rs` 的 `chain_editor` 末尾（施加完 `remove` / `swap` / `add`、「+ 添加跳板」按钮之后）加：

```rust
    // 走查 12:配好的链在界面上只是几行会话名,看不出最终路径长什么样;
    // 而环/自引用/悬空要等到真正拨号才报错。两者都提前到这里。
    if !buf.jump_chain.is_empty() {
        let proxy = match buf.proxy_mode {
            ProxyModeUi::Socks5 => Some("SOCKS5"),
            ProxyModeUi::HttpConnect => Some("HTTP"),
            // 「继承」时代理由分组决定,这里显示不出来 —— 与其猜,不如不画。
            ProxyModeUi::Inherit | ProxyModeUi::Direct => None,
        };
        let target = if buf.name.trim().is_empty() {
            "这台机器"
        } else {
            buf.name.trim()
        };
        ui.add_space(crate::ui::metrics::SP_XS);
        ui.colored_label(
            crate::theme::c32(t.fg_dimmer),
            jump_preview::preview(&buf.jump_chain, sessions, proxy, target),
        );
        if let Some(msg) = jump_preview::check(&buf.jump_chain, sessions) {
            ui.add_space(crate::ui::metrics::SP_XS);
            warn_banner(ui, t, &msg);
        }
    }
```

`fields.rs` 顶部 `use` 加 `use crate::ui::session_manager::jump_preview;`。

`warn_banner` 的签名现在是 `(ui, t, text: &str)` —— 若它写的是 `&'static str`，改成 `&str`（两个既有调用点传的是 `const`，`&str` 兼容）。

- [ ] **Step 7: 写接线层的守护测试**

在 `fields.rs` 的 `mod tests` 里加：

```rust
/// 走查 12:环要在**编辑时**就看得见,不能等到拨号。
///
/// 这条和 `jump_preview::tests::check_catches_a_two_node_cycle` 不重复:
/// 那条守的是判据本身,这条守的是「判据真的被接到了界面上」——
/// 接线漏掉不会有编译错误。
#[test]
fn a_cyclic_jump_chain_is_flagged_while_editing() {
    let t = crate::theme::MULLION_DARK;
    let sessions = vec![
        session_with_jump(1, "A", vec![2]),
        session_with_jump(2, "B", vec![1]),
    ];
    let mut buf = EditorBuffer::default();
    buf.name = "web01".into();
    buf.jump_mode = JumpModeUi::Custom;
    buf.jump_chain = vec![mullion_store::SessionId(1)];

    let out = run_page(|ui| {
        super::basic(ui, &t, &mut buf, &[], &sessions, None, SecretPresence::default())
    });
    let texts = all_text(&out.shapes);
    assert!(
        texts.iter().any(|s| s.contains("环")),
        "编辑时就该看见环的警告;实际画出的文字:{texts:?}"
    );
    assert!(
        texts.iter().any(|s| s.contains("本机") && s.contains("web01")),
        "路径预览没画出来;实际画出的文字:{texts:?}"
    );
}
```

`session_with_jump` 这个测试辅助如果 `fields.rs` 的 `mod tests` 里还没有，照 `jump_preview` 测试里的 `sess()` 写一个同形状的（`fields.rs` 的测试模块已有构造 `SessionRecord` 的代码，优先复用既有的那个，只加 `network.jump` 参数）。

- [ ] **Step 8: 跑全量 + 变异验证**

```bash
cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|panicked"
```

把 Step 6 那段 `if let Some(msg) = jump_preview::check(...)` 整块删掉，跑 `a_cyclic_jump_chain_is_flagged_while_editing`，Expected: FAILED。改回来。

- [ ] **Step 9: 提交**

```bash
git add crates/mullion-app/src/ui/session_manager/jump_preview.rs \
        crates/mullion-app/src/ui/session_manager/mod.rs \
        crates/mullion-app/src/ui/session_manager/fields.rs
git commit -m "feat(ui): 跳板链路径预览 + 环/悬空的编辑时提示 (走查 12)

调的是拨号时用的同一个 expand_chain_of,不重新实现判据——两套判据迟早
漂移,结果是「编辑时说没问题、拨号时连不上」。BTreeMap 只在链非空时才建
(陷阱 T3)。守护测试:a_cyclic_jump_chain_is_flagged_while_editing。"
```

---

## Task 8: env 警告分级 + `Glyph::Info`

**为什么：** `ENV_WARNING` 现在是**无条件**画的红框横幅，占三行，每次打开「登录后」页都在。走查 18 说它太吵 —— 常驻的警告等于没有警告，用户学会了直接略过，真到了往 env 里塞密码的时候反而看不见。降级为常驻一行灰字 + ⓘ；**只有当变量名看起来像密码时**才升级回红框。

`DELAY_WARNING` **不动** —— 它本来就是条件显示的（配了延时才画），不属于走查 18 的范围。

**Files:**
- Modify: `crates/mullion-app/src/ui/icon.rs`（加 `Glyph::Info`）
- Modify: `crates/mullion-app/src/ui/session_manager/fields.rs:1051-1053`

- [ ] **Step 1: 先写 `Glyph::Info` 的失败测试**

在 `icon.rs` 的 `mod tests` 里，把 `every_glyph_stays_inside_its_rect` 的循环加上 `Glyph::Info`，并加一条：

```rust
    /// ⓘ 得是「一个圈 + 一竖 + 一点」,不能退化成光秃秃一条竖线 ——
    /// 那看着像个感叹号的下半截。
    #[test]
    fn info_glyph_has_a_ring_around_it() {
        let sh = shapes(r(), Glyph::Info, s());
        let rings = sh
            .iter()
            .filter(|x| matches!(x, egui::Shape::Circle(_)))
            .count();
        assert_eq!(rings, 1, "ⓘ 少了外圈");
        let strokes = sh.len() - rings;
        assert_eq!(strokes, 2, "ⓘ 的竖线和点应该是两笔");
    }
```

同时把 `points_of` 改成支持 `Circle`（否则上面那条循环测试会 panic）：

```rust
    /// 从形状里抠出所有端点,给上面几个测试用。
    ///
    /// `Circle` 没有「端点」,取它的外接框四角 —— 越界判定要的正是
    /// 「这个圈会不会画到邻居地盘上」。
    fn points_of(shapes: &[egui::Shape]) -> Vec<egui::Pos2> {
        let mut out = Vec::new();
        for s in shapes {
            match s {
                egui::Shape::LineSegment { points, .. } => out.extend_from_slice(points),
                egui::Shape::Path(p) => out.extend_from_slice(&p.points),
                egui::Shape::Circle(c) => {
                    let b = egui::Rect::from_center_size(c.center, egui::Vec2::splat(c.radius * 2.0));
                    out.extend_from_slice(&[b.left_top(), b.right_top(), b.left_bottom(), b.right_bottom()]);
                }
                other => panic!("图标里出现了没预期的形状:{other:?}"),
            }
        }
        out
    }
```

- [ ] **Step 2: 运行确认它红**

```bash
cargo test -p mullion-app -- icon:: 2>&1 | grep -E "test result|FAILED|error\["
```

Expected: 编译失败（`no variant named Info`）。

- [ ] **Step 3: 实现 `Glyph::Info`**

`icon.rs` 的 `Glyph` 枚举加：

```rust
    /// 信息:一句说明的引子。用在不该用红框喊人的地方。
    Info,
```

`shapes()` 的 `match` 加一支：

```rust
        // 圈 + 竖 + 点。`Circle` 用描边(fill 透明),`radius` 取 `h` ——
        // 跟其他图标同一个内切尺度,四个图标并排时大小才一致。
        Glyph::Info => vec![
            Shape::Circle(egui::epaint::CircleShape {
                center: c,
                radius: h,
                fill: egui::Color32::TRANSPARENT,
                stroke,
            }),
            // 竖线:圈内偏下 2/3。
            Shape::LineSegment {
                points: [pos2(c.x, c.y - h * 0.1), pos2(c.x, c.y + h * 0.5)],
                stroke: stroke.into(),
            },
            // 点:用一段极短的竖线代替,免得再引入一种形状。
            Shape::LineSegment {
                points: [pos2(c.x, c.y - h * 0.5), pos2(c.x, c.y - h * 0.42)],
                stroke: stroke.into(),
            },
        ],
```

- [ ] **Step 4: 跑 icon 测试**

```bash
cargo test -p mullion-app -- icon:: 2>&1 | grep -E "test result|FAILED"
```

Expected: 全 ok（含 `every_glyph_stays_inside_its_rect` 对 `Info` 的越界检查）。

- [ ] **Step 5: 写警告分级的失败测试**

在 `fields.rs` 的 `mod tests` 里加：

```rust
/// 走查 18:常驻的红框警告等于没有警告 —— 用户学会了略过它,
/// 真到了往 env 里塞密码的时候反而看不见。
///
/// 判据是「有没有画出 warn 描边的框」。`warn_banner` 用
/// `Stroke::new(1.0, t.warn)` 描边,别的地方不用这个颜色画框。
#[test]
fn env_warning_is_quiet_until_a_key_looks_like_a_secret() {
    let t = crate::theme::MULLION_DARK;

    let mut calm = EditorBuffer::default();
    calm.preserved_automation.env = Some(vec![mullion_store::EnvVar {
        key: "LANG".into(),
        value: "C.UTF-8".into(),
    }]);
    let out = run_page(|ui| super::automation(ui, &t, &mut calm, &[]));
    assert_eq!(
        warn_framed_rects(&out.shapes, &t),
        0,
        "普通变量名不该弹红框"
    );
    assert!(
        all_text(&out.shapes).iter().any(|s| s.contains("明文")),
        "降级后仍要有一行常驻说明,不能一句都不说"
    );

    let mut alarming = EditorBuffer::default();
    alarming.preserved_automation.env = Some(vec![mullion_store::EnvVar {
        key: "DB_PASSWORD".into(),
        value: "hunter2".into(),
    }]);
    let out = run_page(|ui| super::automation(ui, &t, &mut alarming, &[]));
    assert!(
        warn_framed_rects(&out.shapes, &t) > 0,
        "变量名里带 PASSWORD 必须升级成红框"
    );
}

/// 关键词判定不区分大小写 —— 用户写 `db_password` 和 `DB_PASSWORD` 一样多。
#[test]
fn secret_key_detection_is_case_insensitive_and_errs_on_the_loud_side() {
    for k in ["db_password", "API_TOKEN", "MySecret", "ssh_key", "creds"] {
        assert!(looks_like_secret(k), "{k} 应该被判成疑似密码");
    }
    for k in ["LANG", "TERM", "PATH", "EDITOR"] {
        assert!(!looks_like_secret(k), "{k} 不该误报");
    }
}

/// 数出用 `warn` 色描边的矩形。`warn_banner` 是本页唯一这么画的东西。
fn warn_framed_rects(shapes: &[egui::epaint::ClippedShape], t: &crate::theme::Theme) -> usize {
    fn walk(shapes: impl Iterator<Item = egui::Shape>, want: egui::Color32, n: &mut usize) {
        for s in shapes {
            match s {
                egui::Shape::Rect(r) if r.stroke.color == want && r.stroke.width > 0.0 => *n += 1,
                egui::Shape::Vec(v) => walk(v.into_iter(), want, n),
                _ => {}
            }
        }
    }
    let mut n = 0;
    walk(
        shapes.iter().map(|c| c.shape.clone()),
        crate::theme::c32(t.warn),
        &mut n,
    );
    n
}
```

- [ ] **Step 6: 运行确认它红**

```bash
cargo test -p mullion-app -- env_warning secret_key_detection 2>&1 | grep -E "test result|FAILED|error\["
```

Expected: `cannot find function looks_like_secret` + 第一条 FAILED（现在无条件画红框）。

- [ ] **Step 7: 实现分级**

`fields.rs`，在 `ENV_WARNING` 常量旁边加一条短文案与判据：

```rust
/// F43:env 区的常驻说明。**降级自原来的红框横幅**(走查 18):无条件画的
/// 警告等于没有警告 —— 用户学会了略过它,真到了往 env 里塞密码的时候反而
/// 看不见。红框只在 `looks_like_secret` 命中时才回来。
const ENV_NOTE: &str = "值以明文存进 sessions.toml,并以 export 行发到远端。";

/// 变量名看起来像在存密码。
///
/// **宁可误报**:命中了只是多显示一个红框,漏报了则是用户把密码明文写进了
/// `sessions.toml` 而没人提醒过他。所以用 `contains` 而不是词边界匹配 ——
/// `MONKEY_ID` 会因为含 `key` 被误判,那是可以接受的代价。
fn looks_like_secret(key: &str) -> bool {
    const NEEDLES: [&str; 8] = [
        "password", "passwd", "secret", "token", "key", "cred", "auth", "pwd",
    ];
    let k = key.to_ascii_lowercase();
    NEEDLES.iter().any(|n| k.contains(n))
}
```

把 `fields.rs:1051-1053` 换成：

```rust
    section(ui, t, "环境变量", &mut first);
    // 命中关键词才升级回红框。判据看的是**变量名**不是值:值可能是任意
    // 字符串,而名字是用户自己起的,他起名叫 DB_PASSWORD 的时候心里想的
    // 就是密码。
    let loud = a
        .env
        .iter()
        .flatten()
        .any(|v| looks_like_secret(&v.key));
    if loud {
        warn_banner(ui, t, ENV_WARNING);
    } else {
        ui.horizontal_wrapped(|ui| {
            // 自绘 ⓘ:U+24D8 在 egui 内置拉丁字体和微软雅黑里都没有,
            // 实机是豆腐块 —— 跟走查 P0-5 那三个按钮同一个缺陷。
            let size = egui::Vec2::splat(ui.spacing().interact_size.y * 0.7);
            let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
            ui.painter().extend(crate::ui::icon::shapes(
                rect,
                crate::ui::icon::Glyph::Info,
                egui::Stroke::new(1.2, crate::theme::c32(t.fg_dimmer)),
            ));
            ui.colored_label(crate::theme::c32(t.fg_dimmer), ENV_NOTE);
        });
    }
    ui.add_space(crate::ui::metrics::SP_S);
```

- [ ] **Step 8: 跑全量测试**

```bash
cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|panicked"
```

Expected: 全 ok。既有测试里若有断言 `ENV_WARNING` 全文常驻的（grep `环境变量不是存密码的地方`），把它改成「配了疑似密码的变量时才出现」—— 这正是本 Task 改变的行为，测试跟着改是对的，**但不要把断言删掉**。

- [ ] **Step 9: 变异验证**

把 `looks_like_secret` 改成 `fn looks_like_secret(_key: &str) -> bool { true }`，跑 `env_warning_is_quiet_until_a_key_looks_like_a_secret`，Expected: FAILED（第一段断言「普通变量名不该弹红框」）。

再改成恒 `false`，Expected: FAILED（第二段）。改回来确认恢复绿。

- [ ] **Step 10: 提交**

```bash
git add crates/mullion-app/src/ui/icon.rs crates/mullion-app/src/ui/session_manager/fields.rs
git commit -m "feat(ui): env 警告降级为常驻灰字,命中关键词才升红框 (走查 18)

无条件画的警告等于没有警告。新增自绘 Glyph::Info(U+24D8 在两套字体里
都是豆腐块)。looks_like_secret 用 contains 宁可误报——漏报的代价是用户
把密码明文写进 sessions.toml 而没人提醒过他。DELAY_WARNING 不动,它本来
就是条件显示的。"
```

---

## 收尾

- [ ] **Step 1: 全量绿**

```bash
cargo test --workspace > /tmp/p2b2.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/p2b2.log | grep -v "ok\."
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
cargo fmt --check
```

Expected: 三条命令都无输出（第一条只留失败行的 grep）。

- [ ] **Step 2: 交叉编译能过**

```bash
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app 2>&1 | tail -5
```

Expected: 编译通过。**本阶段不做 objdump 验收、不发 Release、不升版本** —— 那些在阶段 4 结束后一次性做。

- [ ] **Step 3: squash 入 main**

```bash
git checkout main
git merge --squash <本阶段分支>
git diff --cached --stat    # 必须只有 crates/ 和 docs/ 下的文件
```

`git diff --cached --stat` 出现 `mullion.exe` / `notes.md` / `todo.md` / `.playwright-mcp/` 中任何一个 = 有人又用了 `git add -A`，先 `git reset HEAD -- <那几项>` 再提交（阶段 1 补债踩过，已进 `.gitignore`，但新产物仍可能漏网）。

---

## 人工验收清单（无头环境验不了，需在 Windows 实机看）

1. 「登录后」页把 tmux、工作目录、三个时序都选成「继承」，右边那五行灰字**读起来是不是像五行**而不是糊成一片。这是本阶段唯一一处新增大量文字的地方。
2. 建一个分组、给它配上 `initial_delay_ms` 和 tmux，再把某条会话归进去 —— 灰字里的分组名和值对不对。
3. 右栏拖到最窄（列表列 440px）时，那些灰字折行折在哪儿，会不会跟下一个字段挤在一起。
4. Tab 条上的 `●` 和 `·` 的**大小**是否合适（`·` 在 13px 字号下可能小到看不见），以及选中那一页的 Tab 名有没有跟着角标变色。
5. 跳板链路径预览那一行在三跳时会不会太长而折行；折了以后还读不读得通。
6. 故意配一个自引用的跳板，红框里那句话是否说清了该去改哪里。
7. env 里加一个 `LANG` 变量看灰字 + ⓘ，再改名成 `DB_PASSWORD` 看红框是否当场弹出来 —— **ⓘ 是不是画成了一个圈里带一竖，而不是豆腐块或一团糊**。
8. 125% / 150% 缩放下上述七条重看一遍。

---

## 自检

**走查条目覆盖：** 本阶段认领 9 / 10 / 11 / 12 / 18 / 19 六条。

| 走查条目 | 落在哪个 Task | 备注 |
|---|---|---|
| 9 Tab 角标 | Task 5（判据）+ Task 6（渲染） | 单一状态位，「连接」页只算跳板/代理 |
| 10 继承值不可见 | Task 1（纯函数）+ Task 2（标量五处）+ Task 3（列表两处）+ Task 4（连接页两处） | 共九处字段接线 |
| 11 继承控件语义混乱 | Task 1（`slot`）+ Task 2/3/4（接线） | **不**统一成二段开关，统一的是继承槽这个部件；态数各字段自定 |
| 12 主机解析语境 | Task 7 | 复用 `jump::expand_chain_of`，不重新实现判据 |
| 18 env 警告太吵 | Task 8 | `DELAY_WARNING` 不动（本就条件显示） |
| 19 术语统一 | Task 4 | 统一为「继承」而非走查建议的「继承（分组）」—— 后者对时序三项是错的 |

**明确不在本阶段（后续阶段认领）：** 3、4、6、21、22 → 阶段 3；13、14、15、16、20 → 阶段 4。

**类型一致性检查：**
- `inherit_row::Source<'a>` —— Task 1 定义，Task 2/3/4 使用。三个变体名 `Group(&str)` / `Builtin` / `NoUpstream` 全程一致。
- `inherit_row::effective_line(value: &str, source: Source<'_>) -> String` —— 两参调用点共九处，签名一致。
- `inherit_row::slot(ui, t, control: impl FnOnce(&mut Ui), line: Option<String>)` —— 四参，调用点共十处（含 Task 4 的协议下拉）。
- `inherit_row::upstream(Option<GroupId>, &[GroupRecord]) -> Option<&GroupRecord>` —— Task 1 定义，Task 2（间接经 `up`）与 Task 4（直接）使用。
- `resolve_bool` / `resolve_u32` —— Task 2 定义在 `fields.rs` 内，仅 Task 2 使用；返回 `(T, Source<'a>)`，生命周期挂在 `up` 上。
- `tab_badge::Badge::{Missing, Configured, None}` / `badge_of(usize, Missing, &EditorBuffer) -> Badge` —— Task 5 定义，Task 6 使用。
- `jump_preview::preview(&[SessionId], &[SessionRecord], Option<&str>, &str) -> String` 与 `check(&[SessionId], &[SessionRecord]) -> Option<String>` —— Task 7 定义与使用一致。
- `icon::Glyph::Info` —— Task 8 定义，同 Task 使用；`shapes()` 的 `match` 是穷尽的，漏了编译不过。
- `fields::automation` 签名从 3 参变 4 参 —— Task 2 改，调用点 `editor.rs:390` 同 Task 改，测试里全部调用点在 Task 2 Step 9 一并改。
- `fields::network` 签名从 5 参变 6 参 —— Task 4 改，唯一调用点在 `basic()` 内，同 Task 改。
- `warn_banner(ui, t, &str)` —— Task 7 可能需要把 `&'static str` 放宽成 `&str`；三个调用点（`DELAY_WARNING`、`ENV_WARNING`、Task 7 的动态 msg）都兼容。

**遗漏风险自查：**
- Task 2 的 `up` 是 `Option<(String, AutomationPrefs)>`，每帧 clone 一次。`AutomationPrefs` 含两个 `Vec`，分组配了几十条命令时这是每帧一次的堆分配 —— 可接受（编辑器是模态窗口，不是终端热路径），但如果 Task 2 完成后 `MULLION_LOG=debug` 下看到帧时间异常，改成持 `&GroupRecord` 并把 `a = &mut buf.preserved_automation` 的借用推后。
- Task 7 的 `check` 给 `expand_chain_of` 传空分组表，会漏报「跳板会话的跳板由分组配、且构成环」这一种情况。已在代码注释里写明理由（宁漏不误报），拨号路径仍有全量检查兜底。
