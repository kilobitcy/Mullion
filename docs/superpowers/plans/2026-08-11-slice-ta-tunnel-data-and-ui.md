# 切片 T-a：隧道数据模型 + 管理 UI 骨架 实施计划（F110/F116/F117）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 隧道成为可增删改查的一等对象：`sessions.toml` 新增 `[[tunnel]]`（schema v6 → v7），会话管理器新增「会话 / 隧道」顶层模式切换，隧道编辑器的字段标签随类型翻转、绑定安全策略生效。**本切片不做任何转发实现**，启动按钮置灰、不发版。

**Architecture:** 数据层全部落 `mullion-store`（零 async、纯单测）；UI 层全部落 `mullion-app/src/ui/session_manager/`，复用既有窗口骨架与「UI 只写意图、app.rs 事后施加」的既有模式。`mullion-ssh` **本切片零改动**。

**关键设计手法 —— 让非法状态不可表示：** `TunnelKind` 用 `#[serde(tag = "kind")]` 的带数据枚举（同 `AuthKind` 的既有模式），`Dynamic` 变体**在类型上就没有** `target_host`/`target_port`/`expose` 字段。于是设计 D5「`-D` 锁死本机」和 D12「`-D` 目标区消失」不是靠 UI 层 disabled 兜着，而是**编译期保证**：想给动态转发配一个暴露地址，代码写不出来。

**Tech Stack:** Rust / serde+toml / egui 0.30。测试为 `cargo test -p mullion-store`（纯单测）与 `cargo test -p mullion-app`（egui 无头，`Context::run` 跑一帧查 `FullOutput.shapes`）。

**Spec:** `docs/superpowers/specs/2026-08-11-tunnels-design.md`

---

## 文件结构

| 文件 | 本次的职责 | 改动性质 |
|---|---|---|
| `crates/mullion-store/src/tunnel.rs` | `TunnelId`/`TunnelKind`/`TunnelRecord` + 悬垂与影响面纯函数 | **新建** |
| `crates/mullion-store/src/model.rs:216` | `CURRENT_SCHEMA` 6 → 7 | 一行 + `SessionsFile` 加 `tunnel` 字段 |
| `crates/mullion-store/src/vault.rs` | 隧道 CRUD、id 分配、save 时写出 | 加方法，不动既有 |
| `crates/mullion-store/src/migrate.rs` | v6 → v7 迁移测试 | 只加测试（迁移本身靠 `#[serde(default)]` 自动完成） |
| `crates/mullion-store/src/lib.rs` | 导出 | 加 `pub mod tunnel` + `pub use` |
| `crates/mullion-app/src/ui/mod.rs` | `UiState` 加隧道模式与意图字段 | 加字段 |
| `crates/mullion-app/src/ui/session_manager/mod.rs:465` | 模式切换条 + 按模式分派左右栏 | 主战场 |
| `crates/mullion-app/src/ui/session_manager/tunnel_list.rs` | 隧道模式的左栏 | **新建** |
| `crates/mullion-app/src/ui/session_manager/tunnel_editor.rs` | 隧道模式的右栏（标签翻转 + 绑定安全） | **新建** |
| `crates/mullion-app/src/ui/session_manager/list.rs` | 删除确认改为列出受影响隧道 | 局部 |
| `crates/mullion-app/src/app.rs` | 施加隧道 CRUD 意图 | 加分支 |

任务顺序由**编译约束**决定：Task 1 的类型是后面全部的前提；Task 5/6 依赖 Task 4 的 `UiState` 字段；Task 7 依赖 Task 3 的查询函数。

---

### Task 1: 隧道数据类型

**Files:**
- Create: `crates/mullion-store/src/tunnel.rs`
- Modify: `crates/mullion-store/src/model.rs:216`（`CURRENT_SCHEMA`）、`:224-232`（`SessionsFile`）
- Modify: `crates/mullion-store/src/lib.rs`

- [ ] **Step 1: 先写失败测试 —— 三种类型的 TOML 往返**

在新建的 `tunnel.rs` 里写 `mod tests`，三条：

1. `local_tunnel_round_trips_through_toml`：`Local { target_host, target_port, expose }` 序列化后再反序列化相等，且产出的 TOML 里 `kind = "local"` 是**平铺**在 `[[tunnel]]` 下的（不额外产生一层 table）。
2. `dynamic_tunnel_has_no_target_or_expose_fields`：把一条 `Dynamic` 序列化成字符串，断言文本里**不含** `target_host`、`target_port`、`expose` 三个子串。这条是 D5/D12「让非法状态不可表示」的守护 —— 有人把 `Dynamic` 改成带字段的变体时它会红。
3. `expose_defaults_to_false_when_key_absent`：从**不含** `expose` 键的 TOML 反序列化，断言 `expose == false`。

第 3 条的意义要写进注释：**serde 对 `bool` 的 `#[serde(default)]` 是 `false`，而 `false` 正是安全值（仅本机可连）**。若字段取名 `local_only`，缺键会默认成 `false` = 全网卡暴露，**手改文件漏一个键就静默开放端口**。字段命名方向本身就是一道安全闸门。

- [ ] **Step 2: 实现类型，跑绿**

```rust
pub struct TunnelId(pub u64);   // derive 同 SessionId

#[derive(Serialize, Deserialize, ...)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum TunnelKind {
    Local  { target_host: String, target_port: u16, #[serde(default)] expose: bool },
    Remote { target_host: String, target_port: u16, #[serde(default)] expose: bool },
    Dynamic,
}

pub struct TunnelRecord {
    pub id: TunnelId,
    pub session_id: SessionId,
    pub listen_port: u16,
    #[serde(default)] pub note: String,
    #[serde(default)] pub autostart: bool,
    #[serde(flatten)] pub kind: TunnelKind,
}
```

`#[serde(tag)] + #[serde(flatten)]` 的组合照抄 `model.rs:52-60` 的 `Auth`/`AuthKind` —— 那里已经验证过这个模式在 toml 下能正确平铺。

- [ ] **Step 3: 挂进 `SessionsFile` 并升版本**

`model.rs:224` 的 `SessionsFile` 加 `#[serde(default)] pub tunnel: Vec<TunnelRecord>`，`CURRENT_SCHEMA` 改 7。`lib.rs` 加 `pub mod tunnel;` 与 `pub use tunnel::{TunnelId, TunnelKind, TunnelRecord};`。

- [ ] **Step 4: 全量跑绿**

`cargo test -p mullion-store`。此时 `migrate.rs:255` 那条 `assert_eq!(CURRENT_SCHEMA, 6)` 会红 —— 它是**故意**写来强制「升版本时必须回来看一眼迁移链」的哨兵，改成 7 并在 Task 2 补 v6→v7 的迁移测试。

---

### Task 2: v6 → v7 迁移与 Vault CRUD

**Files:**
- Modify: `crates/mullion-store/src/vault.rs`（`Vault` 加 `tunnels` 字段、CRUD、`save` 写出）
- Modify: `crates/mullion-store/src/migrate.rs`（只加测试）

- [ ] **Step 1: 先写失败测试 —— v6 文件读进来隧道为空且不丢字段**

在 `migrate.rs` 的 `mod tests` 里加 `v6_file_without_tunnel_key_loads_as_empty_and_keeps_everything_else`：喂一份 `schema_version = 6` 的完整 TOML（含 `[[group]]`、`[[session]]` 及其 `network`/`automation` 子表），断言 `tunnel` 为空数组**且**所有既有字段逐一相等。

**迁移本身不需要写代码** —— `#[serde(default)]` 会自动补空数组，`load_sessions`（`vault.rs:387`）看到 `probe.schema_version < CURRENT_SCHEMA` 会自动备份 `.toml.bak` 并按新结构解析。这条测试是**证明它确实自动成立**，而不是实现某个迁移函数。

- [ ] **Step 2: 先写失败测试 —— CRUD 与 id 分配**

在 `vault.rs` 的 `mod tests` 里加：

1. `tunnel_ids_are_max_plus_one_and_independent_of_session_ids`：库里已有 `SessionId(7)`，新增两条隧道拿到 `TunnelId(1)`、`TunnelId(2)`，不受会话 id 影响。
2. `tunnels_survive_save_and_reopen`：`add_tunnel` → `save` → `Vault::open` → 内容相等。
3. `deleting_a_tunnel_does_not_touch_secrets`：删隧道后 `secret(SessionId)` 仍在。这条守 D2「隧道无独立密文条目」——防止有人后来给隧道加密文时忘了 `vault.rs:78` 那个 `secrets.retain` GC 是按 `SessionId` 裁剪的。

- [ ] **Step 3: 实现，跑绿**

`Vault` 加 `tunnels: Vec<TunnelRecord>` 字段；`open` 从 `SessionsFile.tunnel` 读入；`save`（`vault.rs:113`）写出时带上；加
`tunnels()` / `tunnel(TunnelId)` / `add_tunnel(TunnelDraft) -> TunnelId` / `update_tunnel` / `delete_tunnel`。

`TunnelDraft` 与 `SessionDraft` 同构（不含 `id`），理由同后者：id 由 vault 分配，调用方造不出。

---

### Task 3: 悬垂检测与删除影响面

**Files:**
- Modify: `crates/mullion-store/src/tunnel.rs`（加纯函数 + 测试）
- Modify: `crates/mullion-store/src/error.rs`（加 `TunnelDangling`）

- [ ] **Step 1: 先写失败测试**

命名**刻意对齐** `jump.rs:219` 的既有约定，让两处一眼看出是同一条规则：

1. `dangling_tunnel_reference_is_rejected_never_silently_dropped`：`session_id` 指向不存在的会话时，`resolve_target` 返回 `Err(StoreError::TunnelDangling(..))`，**不是** `None`、不是跳过、不是回落到某个默认会话。
2. `tunnels_referencing_finds_all_and_only_the_affected`：三条隧道两条引用 `SessionId(7)`，`tunnels_referencing(7)` 恰好返回那两条。
3. `tunnels_referencing_is_stable_ordered`：按 `TunnelId` 升序返回 —— 删除确认框里列出的顺序不能每次刷新都变。

- [ ] **Step 2: 实现，跑绿**

```rust
pub fn resolve_target<'a>(t: &TunnelRecord, sessions: &BTreeMap<SessionId, SessionRecord>)
    -> Result<&'a SessionRecord, StoreError>;
pub fn tunnels_referencing(id: SessionId, tunnels: &[TunnelRecord]) -> Vec<&TunnelRecord>;
```

零 IO 纯函数，与 `jump.rs` 同构。`error.rs` 的 `TunnelDangling` 文案照 `:69` 的口吻写：
「隧道引用的会话 {id:?} 不存在 —— 它可能已被删除，请重新指定或删除此隧道」。

---

### Task 4: `UiState` 隧道模式与意图字段

**Files:**
- Modify: `crates/mullion-app/src/ui/mod.rs:52+`
- Modify: `crates/mullion-app/src/app.rs`（施加意图）

- [ ] **Step 1: 加字段**

沿用既有注释风格，说明「UI 闭包只写意图、app.rs 借用释放后统一施加」：

```rust
pub manager_mode: ManagerMode,          // Sessions | Tunnels，默认 Sessions
pub tunnel_editor_id: Option<TunnelId>, // None = 新建
pub tunnel_editor: Option<TunnelEditorBuffer>,
pub tunnel_editor_baseline: Option<TunnelEditorBuffer>,
pub tunnel_save_request: Option<TunnelSaveIntent>,
pub tunnel_delete_request: Option<TunnelId>,
pub pending_tunnel_delete: Option<TunnelId>,
```

- [ ] **Step 2: app.rs 施加分支**

在既有 `save_request` / `delete_request` 施加处旁边加对应分支，调 Task 2 的 vault 方法。

- [ ] **Step 3: 写测试 —— 切模式时表单脏检查不串**

`switching_manager_mode_does_not_clobber_the_other_editors_dirty_state`：会话表单编辑到一半 → 切到隧道模式 → 切回来，`editor` 与 `editor_baseline` 原样保留。

**这条是必需的**：两个模式各有一套 editor/baseline，很容易写成共用一个脏标记，症状是「切一次模式，会话表单的未保存改动被静默丢弃」。

---

### Task 5: 模式切换条与左栏分派

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/mod.rs:465`
- Create: `crates/mullion-app/src/ui/session_manager/tunnel_list.rs`

- [ ] **Step 1: 先写失败测试**

1. `tunnel_mode_renders_tunnel_list_not_session_list`：切到 `Tunnels` 后，跑一帧查 `FullOutput.shapes` 里**没有**会话名的 galley、**有**隧道行的 galley。
2. `mode_bar_is_annotated_for_f100`：`annotate::mark` 登记了「会话管理器/模式条」——`chrome.rs:242` 的先例说明新 UI 元素必须登记，否则 F100 标注模式导不出它。

- [ ] **Step 2: 实现**

在 `SidePanel::left` **之上**加一条模式切换（两个 `SelectableLabel`）。左栏内按 `manager_mode` 分派到 `list::show` 或 `tunnel_list::show`。

隧道行内容：`状态图标 | 类型 | 侦听端口 → 目标 | 引用的会话名`。**不做分组、不做三档密度**（设计 D11）。悬垂的行整行 `danger` 色并在副标题写「引用的会话已删除」。

底部「+ 新建」复用 `list.rs:445` 的手绘按钮模式（**不能用 `ui.button()`** —— `list.rs:429` 的注释写明了守护测试靠显式 id 反查，自动 id 会让测试失效）。

- [ ] **Step 3: 启动按钮置灰**

每行右侧放「启动」按钮，本切片一律 `add_enabled(false, ..)`，hover 提示「转发实现在切片 T-b」。加测试
`start_button_is_disabled_in_this_slice` —— 防止 T-b 之前有人误接一个空实现上去。

---

### Task 6: 隧道编辑器（标签翻转 + 绑定安全）

**Files:**
- Create: `crates/mullion-app/src/ui/session_manager/tunnel_editor.rs`
- Modify: `crates/mullion-app/src/ui/session_manager/mod.rs`（右栏分派）

- [ ] **Step 1: 先写失败测试 —— 标签随类型翻转**

1. `local_and_remote_use_different_listen_captions`：`Local` 下渲染出「本机侦听」，`Remote` 下渲染出「远端侦听」，且**两者不同时出现**。
2. `local_and_remote_explain_who_resolves_the_target_differently`：`Local` 下副文本含「由远端主机解析」，`Remote` 下含「由本机解析」。这条守 D12 那个最难排查的坑（填错了症状是「连上了但连不通」，错误在远端）。
3. `dynamic_renders_no_target_section`：`Dynamic` 下**不渲染**任何目标主机/端口控件。
4. `dynamic_has_no_expose_checkbox_at_all`：`Dynamic` 下连「允许其他主机连接」勾选框都不出现 —— 因为 Task 1 已让它在类型上不存在，UI 若还画一个就是画了个写不进数据的控件。

- [ ] **Step 2: 先写失败测试 —— 绑定安全（F117）**

1. `expose_defaults_to_unchecked_for_new_tunnels`：新建隧道时默认仅本机。
2. `checking_expose_shows_a_danger_colored_warning`：勾上后出现 `danger` 色警告文案，且文案里含具体目标（`db.internal:3306`），不是泛泛一句「有风险」。

- [ ] **Step 3: 实现，跑绿**

字段区按类型分组渲染，措辞表照设计 D12。类型下拉切换时**保留可共用的字段值**（`listen_port`、`note`），只重置该类型没有的字段 —— 切一下类型把用户填的端口清掉是无谓的返工。

- [ ] **Step 4: 引用会话下拉**

下拉列出全部会话（显示 `名称 (user@host)`）。**必须能显示悬垂状态**：当前 `session_id` 已不存在时，下拉顶部显示一条 `danger` 色的「⚠ 已删除的会话 (id=N)」占位项并保持选中，而不是静默跳到第一条 —— 静默跳会把用户的隧道悄悄指到另一台机器上。加测试 `dangling_reference_shows_a_placeholder_entry_instead_of_snapping_to_the_first_session`。

---

### Task 7: 删除会话时列出受影响隧道

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/list.rs`（删除二次确认）

- [ ] **Step 1: 先写失败测试**

1. `deleting_a_referenced_session_lists_the_affected_tunnels`：确认框里出现那两条隧道的描述。
2. `deleting_an_unreferenced_session_keeps_the_plain_confirmation`：没有隧道引用时，确认框**保持原样**，不平白多出一块空的「受影响隧道」区域。
3. `affected_tunnel_list_is_capped_and_says_so`：引用超过 5 条时只列前 5 条并显式写「另有 N 条」。**不静默截断** —— 截断而不说等于告诉用户「就这些」。

- [ ] **Step 2: 实现，跑绿**

复用 Task 3 的 `tunnels_referencing`。本切片没有运行态，「其中 N 条正在运行」那半句留到 T-b 接。

---

## 验收

- [ ] `cargo test --workspace` 全绿
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` 无输出
- [ ] `cargo fmt --check` 通过
- [ ] 手工确认：拿一份真实的 v6 `sessions.toml` 打开，`.toml.bak` 已生成、既有会话一条不少、`schema_version` 变 7

**不发版**（同 P1-a 先例：纯数据层 + UI 骨架，没有可实机验证的行为）。转发实现见后续 T-b / T-c。

## 移交给 T-b 的已知欠账

1. 启动/停止按钮置灰，运行态字段（`Running`/`Reconnecting`/`Failed`）尚未建模
2. 删除确认框缺「其中 N 条正在运行」那半句
3. 状态栏隧道指示器（F115）与失败 toast 未做
4. `autostart` 字段已落盘但无 UI、无行为
