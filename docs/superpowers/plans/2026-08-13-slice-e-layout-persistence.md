# 切片 E：布局持久化（F37）实施计划

> **For agentic workers:** 设计定案见 `docs/superpowers/specs/2026-08-13-layout-persistence-design.md`，
> 决策编号 E1~E10 在下面直接引用，不重复论证。步骤用 `- [ ]` 勾选。

**Goal:** 关窗时把标签/分屏形状/窗口几何记进 `layout.toml`，重启摆回骨架，用户点「重连」才建连接。

**Architecture:** 数据模型落在 `mullion-store`（零 UI/async），`Node ↔ SavedNode`
互转与丢弃规则落在 `mullion-app/shell/layout_snapshot.rs`（纯函数、可单测），
`app.rs` 只做接线。**store 不许依赖 core**（E4）。

**Tech Stack:** serde + toml（store 已有）、winit `available_monitors`（只在接线层）。

---

## 文件结构

| 文件 | 职责 |
|---|---|
| `crates/mullion-store/src/layout.rs` | **新建**。`SavedLayout`/`SavedTab`/`SavedNode`/`SavedWindow` + `load`/`save` + 容错 |
| `crates/mullion-store/src/lib.rs` | 挂 `pub mod layout` + re-export |
| `crates/mullion-app/src/shell/layout_snapshot.rs` | **新建**。`Node↔SavedNode`、丢弃规则、`clamp_to_monitors`、`should_flush` |
| `crates/mullion-app/src/shell/mod.rs` | 挂模块 |
| `crates/mullion-app/src/shell/workspace/mod.rs` | 加 `apply_saved_tree` |
| `crates/mullion-app/src/ui/restored.rs` | **新建**。占位标签的中央视图（未连接 + 重连按钮） |
| `crates/mullion-app/src/ui/mod.rs` | `UiActions` 加 `reconnect_tab` / `reconnect_all`；中央区分派 |
| `crates/mullion-app/src/ui/chrome.rs` | 菜单加「全部重连」 |
| `crates/mullion-app/src/app.rs` | `TabContent::Restored`、快照提取、落盘/加载接线、`pending_restore` |

---

### Task 1：`mullion-store` 的落盘模型（E1/E2/E5/E6）

**Files:** Create `crates/mullion-store/src/layout.rs`；Modify `crates/mullion-store/src/lib.rs`

- [ ] **Step 1: 写失败的 round-trip 测试**（spec F37 点名的验收标准）

```rust
#[test]
fn a_layout_survives_a_round_trip_through_toml() {
    let before = SavedLayout { /* 两个标签，一个 3 叶子嵌套树 */ };
    let text = to_toml(&before).unwrap();
    let after = from_toml(&text);
    assert_eq!(after, before);
}
```

- [ ] **Step 2: 跑测试确认红** —— `cargo test -p mullion-store layout`，预期 `cannot find`
- [ ] **Step 3: 写模型与 `to_toml`/`from_toml`**

关键类型（`SavedNode` 用 `#[serde(tag = "kind")]` 的内部标签，理由：TOML 没有
enum，外部标签会编成一层多余的表；`dir` 用 `"horizontal"`/`"vertical"` 字符串）：

```rust
pub const CURRENT_LAYOUT_SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SavedLayout {
    pub schema_version: u32,
    #[serde(default)] pub active_tab: usize,
    #[serde(default)] pub window: Option<SavedWindow>,
    #[serde(default, rename = "tab")] pub tabs: Vec<SavedTab>,
}
```

- [ ] **Step 4: 跑测试确认绿**
- [ ] **Step 5: 写容错测试**（E6：垃圾字节 / 未知 schema / 缺字段 → 空布局，不是 Err）
- [ ] **Step 6: `load(dir)` / `save(dir, &SavedLayout)`**，`save` 复用 `vault::write_atomic`
- [ ] **Step 7: 提交** `feat(store): 布局落盘模型 layout.toml —— 标签/树形状/窗口几何 (F37)`

---

### Task 2：守护 E4「store 不认识 core」

**Files:** Modify `crates/mullion-store/src/layout.rs`（测试放模块内）

- [ ] **Step 1: 写测试**

```rust
#[test]
fn the_store_crate_must_not_depend_on_any_other_mullion_crate() {
    let toml = include_str!("../Cargo.toml");
    // 只扫 [dependencies] 之后到下一个节之前
    assert!(!deps_section(toml).contains("mullion-"), "...");
}
```

- [ ] **Step 2: 跑绿**（当前本就不依赖，这条是**防回归**，不是发现问题）
- [ ] **Step 3: 变异验收** —— 临时往 `Cargo.toml` 加 `mullion-core.workspace = true`，确认变红，然后 `cp` 还原
- [ ] **Step 4: 并入 Task 1 的提交或单独提交**

---

### Task 3：`Node ↔ SavedNode` 互转 + 丢弃规则（E5/E6）

**Files:** Create `crates/mullion-app/src/shell/layout_snapshot.rs`

- [ ] **Step 1: 写互转 round-trip 测试**：嵌套三层的 `Node` → `SavedNode` → `Node`，
      断言**结构与 ratio 逐字段相等**，`PaneId` 按传入的新 id 序列重发
- [ ] **Step 2: 跑红**
- [ ] **Step 3: 实现**

```rust
pub fn to_saved(node: &Node) -> SavedNode;
/// 用 `ids` 按前序遍历给叶子发号。`ids` 不够时返回 None（树结构与 id 数对不上）。
pub fn from_saved(saved: &SavedNode, ids: &[PaneId]) -> Option<Node>;
pub fn leaf_count(saved: &SavedNode) -> usize;
```

- [ ] **Step 4: 跑绿**
- [ ] **Step 5: 写丢弃规则测试**（E6 六条各一）

```rust
pub fn usable_tabs(saved: SavedLayout, known: &dyn Fn(SessionId) -> bool) -> SavedLayout;
```

断言：会话被删的丢掉、叶子数 0 的丢掉、`active_tab` 越界夹到 0、全丢完等于空布局。

- [ ] **Step 6: 实现 + 跑绿**
- [ ] **Step 7: 提交** `feat(app): 布局树互转与恢复丢弃规则 (F37)`

---

### Task 4：窗口几何夹紧 + 落盘节流（E7/E8）

**Files:** Modify `crates/mullion-app/src/shell/layout_snapshot.rs`

- [ ] **Step 1: 写 `clamp_to_monitors` 测试**：与所有显示器都无交集 → 位置被丢弃、
      尺寸保留；有交集 → 原样；尺寸小于下界 → 抬到下界
- [ ] **Step 2: 跑红 → 实现 → 跑绿**
- [ ] **Step 3: 写 `should_flush` 测试**：不脏恒不写；脏且未到节流窗口不写；脏且到点写
- [ ] **Step 4: 实现 + 跑绿**
- [ ] **Step 5: 提交** `feat(app): 窗口几何夹回可见区域 + 布局落盘节流 (F37)`

---

### Task 5：`Workspace::apply_saved_tree`（E10）

**Files:** Modify `crates/mullion-app/src/shell/workspace/mod.rs`

- [ ] **Step 1: 写测试**：1 个 pane 的 ws 恢复一棵 3 叶子树 → 返回 2 个新 id、
      `pane_count()` 仍是 1（新 pane 要调用方开完 channel 再 `attach_pane`）、
      `tree()` 的形状与 ratio 与保存的一致、焦点落在指定叶子上
- [ ] **Step 2: 跑红 → 实现（照抄 `apply_preset` 的套路）→ 跑绿**
- [ ] **Step 3: 提交** `feat(app): Workspace 按保存的树形状恢复分屏 (F37)`

---

### Task 6：`TabContent::Restored` 占位标签 + 中央视图

**Files:** Create `crates/mullion-app/src/ui/restored.rs`；Modify `app.rs`、`ui/mod.rs`

- [ ] **Step 1: 加变体**

```rust
struct RestoredTab {
    session_id: SessionId,
    tree: SavedNode,
    generation: u64,
    wants_sftp: bool,
    /// 已经点过重连、正在拨号 —— 按钮要禁用，否则连点会拉起 N 条连接
    dialing: bool,
}
```

`wind_down` 的 no-catch-all 守护测试会因此变红 —— **这是它该有的表现**，补一条
`Restored` 分支（没有连接/没有 task 要收，只记一行日志）。

- [ ] **Step 2: 写占位视图**（`ui/restored.rs`）：会话名 + 「未连接 · 上次是 N 分屏」+
      「重连」按钮（`dialing` 时禁用并改文案）。`annotate::mark` 登记（F100）
- [ ] **Step 3: 写测试**：跑一帧断言文案里有会话名与 pane 数；`dialing = true` 时按钮禁用
- [ ] **Step 4: `UiActions` 加 `reconnect_tab: Option<TabId>` 与 `reconnect_all: bool`；
      `has_real_action` 补这两个字段**（D4b 的教训：新字段漏进去会被 egui 丢弃帧静默吞掉）
- [ ] **Step 5: 提交** `feat(app): 未连接的占位标签与重连入口 (F37)`

---

### Task 7：快照提取与落盘接线（E7）

**Files:** Modify `crates/mullion-app/src/app.rs`

- [ ] **Step 1: `App` 加 `layout_dirty: bool` + `layout_flushed_at: u64`**
- [ ] **Step 2: 打脏点**：`tabs.open` / `close` / 切换 / `split_focused` / `close_pane` /
      `apply_preset` / 窗口 `Resized`·`Moved`
- [ ] **Step 3: `fn snapshot_layout(&self) -> SavedLayout`**：遍历 `tabs`，
      `session_id == None` 的**跳过**（E6），`Restored` 标签按原样写回去
      （用户上次没重连，这次也别把它弄丢）
- [ ] **Step 4: `about_to_wait` 里按 `should_flush` 写盘；`CloseRequested` 无条件写一次**
- [ ] **Step 5: 结构守护测试**：`CloseRequested` 分支里必须有 `save_layout` 调用
      （真跑要 `EventLoopProxy`，无头造不出）
- [ ] **Step 6: 提交** `feat(app): 布局快照与落盘接线 (F37)`

---

### Task 8：启动恢复接线

**Files:** Modify `crates/mullion-app/src/app.rs`、`main.rs`

- [ ] **Step 1: 启动时 `layout::load` → `usable_tabs` 过滤 → 逐个 `tabs.open` 成 `Restored`**
- [ ] **Step 2: 窗口几何：`clamp_to_monitors(saved, monitors)` 之后 `set_outer_position` / 尺寸 / `set_maximized`**
- [ ] **Step 3: 结构守护测试**：恢复路径必须过 `usable_tabs`（不许直接把盘上的标签塞进 `tabs`）
- [ ] **Step 4: 提交** `feat(app): 启动恢复标签骨架与窗口几何 (F37)`

---

### Task 9：重连接线（E9）

**Files:** Modify `crates/mullion-app/src/app.rs`

- [ ] **Step 1: `App` 加 `pending_restore: Option<(TabId, SavedNode)>`**
- [ ] **Step 2: `reconnect_tab` 动作 → 记 `pending_restore` + `dialing = true` + `spawn_connect`**
- [ ] **Step 3: `ConnectOk` 分支：`pending_restore` 命中 → 原地替换那个 `TabId` 的标签；
      否则照旧 `tabs.open`**
- [ ] **Step 4: 连上后按树 `apply_saved_tree`，对返回的每个 id 开 channel（复用 `apply_preset` 后的那段）**
- [ ] **Step 5: 两条守护测试**
  - 既有的 `connecting_opens_a_new_tab_instead_of_replacing_the_active_one` **不许削弱**，
    改成「无 `pending_restore` 时仍开新标签」
  - 新增：替换的是 `pending_restore.tab_id` 指名的标签，**不是活动标签**
- [ ] **Step 6: `reconnect_all`：对每个 `Restored` 标签依次触发**（同一路径，不分叉）
- [ ] **Step 7: 提交** `feat(app): 占位标签重连并恢复分屏形状 (F37)`

---

### Task 10：变异验收

- [ ] 对每条关键守护测试做一次变异，确认**当场变红**。备份用 `cp file /tmp/x.bak`，
      还原用 `cp` —— **绝不用 `git checkout <file>`**（会抹掉未提交改动）
- [ ] 变异清单至少覆盖：round-trip 的 ratio 精度、`from_saved` 的叶子发号顺序、
      `usable_tabs` 的四条丢弃规则、`clamp_to_monitors` 的无交集判据、
      `should_flush` 的节流窗口、`has_real_action` 漏字段、`ConnectOk` 替换的是不是活动标签

---

### Task 11：交付（CLAUDE.md 交付约定）

- [ ] `chore: 版本 0.1.38(...)`
- [ ] 绿门：`cargo test --workspace` + `clippy -D warnings` + `fmt --check`
- [ ] 交叉编译 `x86_64-pc-windows-gnu` + objdump 验收（出现 `libgcc_s_seh-1.dll` / `libwinpthread-1.dll` 即不合格）
- [ ] 发 Release `v0.1.38`（标题纯版本号），notes 写人工验收清单（多显示器、关窗再开、重连后分屏形状）
- [ ] 报链接 + sha256 + 清单
