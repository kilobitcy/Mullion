# 切片 K：标签本地覆盖 + 会话列表拖拽排序 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 标签的名称/颜色改成只作用于该标签自身的运行期覆盖（颜色画成整块背景），
并让会话管理器左栏支持拖拽排序（组内换位 + 跨组）。

**Architecture:** 覆盖挂在 `shell/tabs.rs::Tab` 的两个新字段上，一行 store 都不写、
不进 F37 布局快照；排序真值直接用 `sessions.toml` 的数组顺序，`Vault::move_session`
是唯一入口，零 schema 改动。所有判定（有效色、落点、可否拖、前景取色）都抽成纯函数单测。

**Tech Stack:** Rust / egui 0.30（`dnd_set_drag_payload` / `dnd_release_payload`）/
mullion-store（TOML + keyring）

**设计文档：** `docs/superpowers/specs/2026-08-16-tab-overrides-and-list-reorder-design.md`

---

## 文件清单

| 文件 | 责任 | 动作 |
|---|---|---|
| `crates/mullion-store/src/vault.rs` | `move_session`：改组 + 挪数组位置 | 修改 |
| `crates/mullion-app/src/shell/store.rs` | 向 `Vault::move_session` 转发 | 修改 |
| `crates/mullion-app/src/shell/tabs.rs` | `Tab` 的两个 override 字段 + `display_title` | 修改 |
| `crates/mullion-app/src/theme.rs` | `readable_fg` | 修改 |
| `crates/mullion-app/src/ui/tab_props.rs` | 弹窗改按 `TabId`、删「应用到」 | 修改 |
| `crates/mullion-app/src/ui/chrome.rs` | `tab_fill` + 整块背景 + 右键解禁 | 修改 |
| `crates/mullion-app/src/ui/session_manager/reorder.rs` | 落点判定与拖拽门控纯函数 | **新建** |
| `crates/mullion-app/src/ui/session_manager/mod.rs` | 挂 `mod reorder` | 修改 |
| `crates/mullion-app/src/ui/session_manager/list.rs` | 拖源/落点/插入线 | 修改 |
| `crates/mullion-app/src/ui/mod.rs` | `UiState` 两个字段（`tab_props` 改键、`reorder_request`） | 修改 |
| `crates/mullion-app/src/app.rs` | 接线：覆盖施加、排序施加、删两个旧自由函数 | 修改 |
| `spec.md` | F121/F122 新增，F36/F62 修订 | 修改 |

每个 Task 结束后跑：

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check
```

---

## Task 1：`Vault::move_session`（F121 数据层）

**Files:**
- Modify: `crates/mullion-store/src/vault.rs`（挨着 `set_group`，约 383 行）
- Test: 同文件 `mod tests`（约 820 行起）

- [ ] **Step 1: 写失败的测试**（加在 `mod tests` 末尾）

```rust
/// F121:组内前移。`before` 指向谁,就插在谁**前面**。
///
/// 自证会变红:把实现里「先 remove 再定位 before」改成「先定位再 remove」——
/// 目标在被拖走那条右边时会差一位。
#[test]
fn move_session_puts_the_record_right_before_the_target() {
    let dir = tempfile::tempdir().unwrap();
    let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
    let mut names = Vec::new();
    for n in ["a", "b", "c"] {
        let mut d = draft();
        d.identity.name = n.into();
        names.push(v.add(d, "2026-08-16T00:00:00Z"));
    }
    // c 挪到 a 前面 → c, a, b
    v.move_session(names[2], None, Some(names[0])).unwrap();
    let order: Vec<&str> = v.list().iter().map(|r| r.identity.name.as_str()).collect();
    assert_eq!(order, vec!["c", "a", "b"]);

    // a 挪到 b 前面(目标在自己右边)→ c, a, b 不变
    v.move_session(names[0], None, Some(names[1])).unwrap();
    let order: Vec<&str> = v.list().iter().map(|r| r.identity.name.as_str()).collect();
    assert_eq!(order, vec!["c", "a", "b"], "先定位再 remove 会把 a 插到 b 后面");
}

/// `before = None` = 挪到末尾。组内最后一行的下半区落点走这条。
#[test]
fn move_session_with_no_target_goes_to_the_end() {
    let dir = tempfile::tempdir().unwrap();
    let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
    let mut ids = Vec::new();
    for n in ["a", "b", "c"] {
        let mut d = draft();
        d.identity.name = n.into();
        ids.push(v.add(d, "2026-08-16T00:00:00Z"));
    }
    v.move_session(ids[0], None, None).unwrap();
    let order: Vec<&str> = v.list().iter().map(|r| r.identity.name.as_str()).collect();
    assert_eq!(order, vec!["b", "c", "a"]);
}

/// 跨组拖动:顺带改 `group_id`。位置与组两件事一个入口做完 ——
/// 分两次调用会在中间留下一个「已经改了组、还没挪位置」的可观察状态。
#[test]
fn move_session_across_groups_sets_the_group_too() {
    let dir = tempfile::tempdir().unwrap();
    let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
    let gid = v.add_group("生产".into());
    let a = v.add(draft(), "2026-08-16T00:00:00Z");
    v.move_session(a, Some(gid), None).unwrap();
    assert_eq!(v.get(a).unwrap().identity.group_id, Some(gid));
}

/// 拖到自己身上 = 什么都不做,**不报错**。UI 侧已经挡了一道,
/// 这里再挡一道:报错会让上层弹一个用户看不懂的失败提示。
#[test]
fn move_session_onto_itself_is_a_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
    let a = v.add(draft(), "2026-08-16T00:00:00Z");
    let b = v.add(draft(), "2026-08-16T00:00:00Z");
    v.move_session(a, None, Some(a)).unwrap();
    assert_eq!(v.list().iter().map(|r| r.id).collect::<Vec<_>>(), vec![a, b]);
}

/// `before` 指向一条已经不存在的记录(别处刚删掉)→ 落到末尾,不报错。
#[test]
fn move_session_with_a_dangling_target_falls_back_to_the_end() {
    let dir = tempfile::tempdir().unwrap();
    let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
    let a = v.add(draft(), "2026-08-16T00:00:00Z");
    let b = v.add(draft(), "2026-08-16T00:00:00Z");
    v.move_session(a, None, Some(SessionId(9999))).unwrap();
    assert_eq!(v.list().iter().map(|r| r.id).collect::<Vec<_>>(), vec![b, a]);
}

/// 被拖的记录不存在 → `Err`。这一条是真错误(UI 手上的 id 来自本帧列表)。
#[test]
fn move_session_reports_a_missing_record() {
    let dir = tempfile::tempdir().unwrap();
    let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
    assert!(v.move_session(SessionId(9999), None, None).is_err());
}

/// 换位置不算「改了这条会话」——`modified_at` 不许动(同 `set_group` 的理由)。
///
/// 自证会变红:在 `move_session` 里补一句写 `modified_at`。
#[test]
fn move_session_does_not_touch_modified_at() {
    let dir = tempfile::tempdir().unwrap();
    let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
    let a = v.add(draft(), "2026-08-16T00:00:00Z");
    let b = v.add(draft(), "2026-08-16T00:00:00Z");
    let before = v.get(a).unwrap().modified_at.clone();
    v.move_session(a, None, Some(b)).unwrap();
    assert_eq!(v.get(a).unwrap().modified_at, before);
}
```

- [ ] **Step 2: 跑一遍确认它们红**

Run: `cargo test -p mullion-store move_session 2>&1 | tail -20`
Expected: 编译失败，`no method named 'move_session' found`

- [ ] **Step 3: 实现**（加在 `set_group` 之后）

```rust
/// F121:把一条会话挪到 `before` 之前(`None` = 末尾),顺带改组。
///
/// 组内排序与跨组拖动**共用这一个入口** —— 拆成两个函数会让「跨组时位置
/// 怎么算」有两份实现,而这两份必然分叉。
///
/// `before` 指向的记录不存在时落到末尾而不是报错:UI 拿到 id 与松手之间
/// 隔着若干帧,那条记录可能刚被删掉,这不是异常。
///
/// **不重打 `modified_at`**:换位置是组织动作,不是内容变更(同 `set_group`)。
pub fn move_session(
    &mut self,
    id: SessionId,
    group: Option<GroupId>,
    before: Option<SessionId>,
) -> Result<(), StoreError> {
    if before == Some(id) {
        return Ok(());
    }
    let from = self
        .sessions
        .iter()
        .position(|s| s.id == id)
        .ok_or(StoreError::NotFound(id))?;
    let mut rec = self.sessions.remove(from);
    rec.identity.group_id = group;
    // 下标必须在 `remove` **之后**再算:先算的话,目标在被拖走那条右边时
    // 会因为整体左移而差一位。
    let at = match before {
        Some(t) => self
            .sessions
            .iter()
            .position(|s| s.id == t)
            .unwrap_or(self.sessions.len()),
        None => self.sessions.len(),
    };
    self.sessions.insert(at, rec);
    Ok(())
}
```

- [ ] **Step 4: 跑绿**

Run: `cargo test -p mullion-store move_session 2>&1 | grep "test result"`
Expected: `test result: ok. 7 passed`

- [ ] **Step 5: 转发到 `SessionStore`**（`crates/mullion-app/src/shell/store.rs`，挨着 `set_group`）

```rust
/// F121:左栏拖拽排序。见 `Vault::move_session`。
pub fn move_session(
    &mut self,
    id: SessionId,
    group: Option<mullion_store::GroupId>,
    before: Option<SessionId>,
) -> Result<(), StoreError> {
    self.vault.move_session(id, group, before)
}
```

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-store/src/vault.rs crates/mullion-app/src/shell/store.rs
git commit -m "feat(store): 会话列表手动排序的数据入口 —— move_session (F121)"
```

---

## Task 2：`Tab` 的两个覆盖字段（F122 数据层）

**Files:**
- Modify: `crates/mullion-app/src/shell/tabs.rs`（`Tab` 结构约 32 行、`open` 约 97 行、`replace` 约 144 行）
- Test: 同文件 `mod tests`

- [ ] **Step 1: 写失败的测试**（加在 `mod tests` 末尾；该模块已有 `Tabs<C>` 的构造用例，
  内容类型沿用文件里现成的那个测试用 payload 类型）

```rust
/// F122:没设覆盖时显示名 = 连接时拼的 `title`。
#[test]
fn display_title_falls_back_to_the_connection_title() {
    let mut tabs = Tabs::default();
    let id = tabs.open("u@h".into(), None, P(1));
    assert_eq!(tabs.iter().next().unwrap().display_title(), "u@h");
    let _ = id;
}

/// F122 的核心判据(D3):覆盖挂在**标签**上,不挂在会话上。同一个会话开两个
/// 标签,改其中一个,另一个纹丝不动。
///
/// 自证会变红:把 `title_override` 改成按 `session_id` 查表。
#[test]
fn a_title_override_belongs_to_one_tab_not_to_its_session() {
    let sid = SessionId(7);
    let mut tabs = Tabs::default();
    tabs.open("u@h".into(), Some(sid), P(1));
    tabs.open("u@h".into(), Some(sid), P(2));
    tabs.iter_mut().next().unwrap().title_override = Some("日志".into());
    let shown: Vec<&str> = tabs.iter().map(|t| t.display_title()).collect();
    assert_eq!(shown, vec!["日志", "u@h"]);
}

/// F37:占位标签重连走 `replace`,它只换 `title`/`content` ——
/// 用户改的名字/颜色**必须活过重连**,否则「重连一下名字自己变回去了」。
///
/// 自证会变红:在 `replace` 里给新 `Tab` 填 `title_override: None`。
#[test]
fn reconnecting_a_tab_keeps_its_overrides() {
    let mut tabs = Tabs::default();
    let id = tabs.open("占位".into(), None, P(1));
    {
        let tab = tabs.iter_mut().next().unwrap();
        tab.title_override = Some("构建机".into());
        tab.color_override = Some(Rgb::new(0xe0, 0x67, 0x67));
    }
    tabs.replace(id, "u@h".into(), P(2));
    let tab = tabs.iter().next().unwrap();
    assert_eq!(tab.display_title(), "构建机");
    assert_eq!(tab.color_override, Some(Rgb::new(0xe0, 0x67, 0x67)));
}
```

> 执行者注意：`P(_)` 是本文件测试里现成的 payload 构造，照抄同模块其它用例的写法；
> `Rgb` 需要 `use mullion_term::snapshot::Rgb;`。

- [ ] **Step 2: 跑一遍确认它们红**

Run: `cargo test -p mullion-app --lib shell::tabs 2>&1 | tail -20`
Expected: 编译失败，`no field 'title_override'`

- [ ] **Step 3: 实现**

`Tab<C>` 加字段（文件头 `use` 补 `use mullion_term::snapshot::Rgb;`）：

```rust
    /// F122:用户在**这个标签**上改的名字。`None` = 用 `title`。
    ///
    /// 覆盖是运行期的:不写 store(会话列表/pane 标题条/状态栏一律不受影响)、
    /// 也不进 F37 布局快照 —— 关窗口即丢是设计,不是欠账。
    pub title_override: Option<String>,
    /// F122:用户在这个标签上配的色。`None` = 退回会话色(`ColorTarget::Tab`)。
    pub color_override: Option<Rgb>,
```

`open` 与 `replace` 里构造 `Tab` 的地方补 `title_override` / `color_override`：
`open` 填 `None`；**`replace` 必须从旧标签搬过来**：

```rust
    pub fn replace(&mut self, id: TabId, title: String, content: C) -> Option<Tab<C>> {
        let ix = self.tabs.iter().position(|t| t.id == id)?;
        let session_id = self.tabs[ix].session_id;
        // F122:覆盖是标签自己的事实,重连换的是内容,不该把它抹掉。
        let title_override = self.tabs[ix].title_override.clone();
        let color_override = self.tabs[ix].color_override;
        let fresh = Tab {
            id,
            title,
            session_id,
            title_override,
            color_override,
            content,
        };
        Some(std::mem::replace(&mut self.tabs[ix], fresh))
    }
```

`impl<C> Tab<C>`（没有就新开一个 impl 块）加：

```rust
impl<C> Tab<C> {
    /// 标签栏上真正显示的名字:本地覆盖优先。
    pub fn display_title(&self) -> &str {
        self.title_override.as_deref().unwrap_or(&self.title)
    }
}
```

- [ ] **Step 4: 跑绿**

Run: `cargo test -p mullion-app --lib shell::tabs 2>&1 | grep "test result"`
Expected: `test result: ok.`（原有用例 + 新增 3 条全过）

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/shell/tabs.rs
git commit -m "feat(app): 标签带上自己的名称/颜色覆盖,重连不丢 (F122)"
```

---

## Task 3：弹窗与接线改成运行期覆盖（F122 行为）

这一步会同时删掉切片 J 留下的 `apply_tab_props_save` / `sync_tab_titles_for_session`
及其测试。**先删测试再删实现**，中间不要停在编译不过的状态上提交。

**Files:**
- Modify: `crates/mullion-app/src/ui/tab_props.rs`（整文件）
- Modify: `crates/mullion-app/src/ui/mod.rs:318-322`（`UiState` 两个字段的类型说明）
- Modify: `crates/mullion-app/src/app.rs`（约 5705-5745 打开弹窗、5994 `touched_store`、
  6060-6075 施加、6690 `apply_tab_props_save`、其后 `sync_tab_titles_for_session`）
- Test: `crates/mullion-app/src/app.rs` 的 `mod tests`

- [ ] **Step 1: 写失败的测试**（加在 `app.rs` 的 `mod tests` 里，紧挨着原来那两条测试的位置）

```rust
/// F122 的核心判据(D1):标签属性保存**一行 store 都不许写**。
///
/// 自证会变红:把 `apply_tab_props` 改回去调 `store.update`。
#[test]
fn saving_tab_props_leaves_every_session_record_byte_identical() {
    let mut tabs: crate::shell::tabs::Tabs<u64> = Default::default();
    let sid = mullion_store::SessionId(1);
    let tab = tabs.open("u@h".into(), Some(sid), 1);
    apply_tab_props(
        &mut tabs,
        tab,
        "日志".into(),
        Some(mullion_term::snapshot::Rgb::new(0xe0, 0x67, 0x67)),
    );
    let t = tabs.iter().next().unwrap();
    assert_eq!(t.display_title(), "日志");
    assert_eq!(
        t.color_override,
        Some(mullion_term::snapshot::Rgb::new(0xe0, 0x67, 0x67))
    );
    assert_eq!(t.title, "u@h", "连接时拼的 title 不该被改写");
}

/// F122/D2:覆盖**不进 F37 布局快照**。`snapshot_tabs_of` 存的必须是连接时拼的
/// `tab.title`,不是 `display_title()` —— 存了覆盖的话,「关窗口即丢」这条承诺
/// 就变成了「关窗口还在,但会话改了名又不跟着变」的第三种语义。
///
/// 自证会变红:把 `snapshot_tabs_of` 里的 `tab.title.clone()` 改成
/// `tab.display_title().to_string()`。
#[test]
fn a_tab_override_never_reaches_the_layout_snapshot() {
    let mut tabs: Tabs<TabContent> = Default::default();
    tabs.open("u@h".into(), Some(SessionId(1)), restored_tab(1, 1));
    tabs.iter_mut().next().unwrap().title_override = Some("日志".into());
    let (saved, _) = snapshot_tabs_of(&tabs);
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].title, "u@h", "覆盖被写进了布局快照");
}

/// 弹窗保存不再是「写 store 的意图」——`touched_store` 里不许再有它,
/// 否则每改一次标签名都白跑一次外观全表重算。
///
/// 自证会变红:把 `self.ui.tab_props_save.is_some()` 加回 `touched_store`。
#[test]
fn tab_props_is_no_longer_a_store_write_intent() {
    let src = include_str!("app.rs");
    let after = src
        .split("let touched_store = ")
        .nth(1)
        .expect("找不到 touched_store 的赋值");
    let expr = &after[..after.find(";\n").expect("找不到该赋值的结尾")];
    assert!(
        expr.contains("self.ui.save_request"),
        "切歪了 —— 下面那条会空过"
    );
    assert!(
        !expr.contains("tab_props"),
        "标签属性已改成运行期覆盖(F122),不该再算进 touched_store"
    );
}
```

- [ ] **Step 2: 删掉被反转的旧测试与旧实现**

删除 `app.rs` 里这三处（连同各自的文档注释）：

1. `fn apply_tab_props_save(...)` 及测试 `apply_tab_props_save_only_touches_name_and_color_leaves_everything_else_alone`
2. `fn sync_tab_titles_for_session(...)` 及测试 `saving_tab_props_retitles_every_tab_pointing_at_the_same_session_not_just_the_active_one`
3. 测试 `renaming_a_tab_counts_as_touching_the_store_so_the_look_is_recomputed`
   —— **整条删除**，不要改成「断言不含」，那是拿常量断言常量的重言式；
   「不写 store」这条由上面 `saving_tab_props_leaves_every_session_record_byte_identical` 守

同步删掉 `mod tests` 顶部 `use` 里的 `apply_tab_props_save`（约 7178 行那一串）。

- [ ] **Step 3: 改弹窗**（`ui/tab_props.rs`）

头部文档注释整段换成：

```rust
//! F122:标签属性弹窗 —— 改名 + 配色。
//!
//! **改的是这个标签自己**(`Tab::title_override` / `Tab::color_override`),
//! 一行 store 都不写:会话管理器左栏那条、pane 标题条、状态栏一律不受影响。
//! 覆盖不进 F37 布局快照 —— 关窗口即丢是设计(设计文档 D2)。
//!
//! **这个弹窗仍要登记进 `app.rs::modal_open` 的 `Modal` 枚举** —— 否则里面
//! 敲的字会漏给远端 shell(T8)。但**不再进 `touched_store`**:它不写 store,
//! 登记进去只会让每次改名白跑一次外观全表重算。
```

结构与动作改成按标签走（`use` 里删掉 `ColorSpec` / `ColorTarget` / `COLOR_TARGET_LABELS`，
补 `use crate::shell::tabs::TabId; use mullion_term::snapshot::Rgb;`）：

```rust
pub struct TabPropsDraft {
    pub tab_id: TabId,
    pub name: String,
    /// `None` = 不配颜色(退回会话色,没有会话色就是主题底色)。
    pub color: Option<egui::Color32>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TabPropsAction {
    Save {
        tab_id: TabId,
        name: String,
        color: Option<Rgb>,
    },
    Cancel,
}
```

`show` 里：**删掉整段「应用到」**（`ui.label("应用到")` 与那个 `for (target, label)` 循环，
以及它上面那句 `ui.add_space`）——覆盖只作用于标签自身，选落点没有意义。
「保存」分支改成：

```rust
                if ui.button("保存").clicked() {
                    action = Some(TabPropsAction::Save {
                        tab_id: d.tab_id,
                        name: d.name.clone(),
                        color: d.color.map(|c| Rgb::new(c.r(), c.g(), c.b())),
                    });
                    close = true;
                }
```

「清除」按钮的 hover 文案补一句语义（它现在的含义变了）：

```rust
                if ui
                    .button("清除")
                    .on_hover_text("退回会话自己配的颜色")
                    .clicked()
                {
                    d.color = None;
                }
```

文件末尾那段关于 `hex_of` 的注释一并删掉（不再用它）。

- [ ] **Step 4: 改接线**（`app.rs`）

打开弹窗那段（`Some(crate::ui::chrome::TabAction::Props(ix))` 分支）整段换成：

```rust
                                // F122:双击标签,或右键菜单点了「重命名…」/
                                // 「设置颜色…」。初值取**当前有效值**(覆盖优先,
                                // 否则会话名/会话色)—— 不再去 store 里捞记录,
                                // 改的东西也不再写回去。
                                Some(crate::ui::chrome::TabAction::Props(ix)) => {
                                    if let Some(tab) = self.tabs.get(ix) {
                                        let color = tab.color_override.map(theme::c32).or_else(|| {
                                            tab.session_id
                                                .and_then(|sid| self.appearance.get(sid))
                                                .and_then(|a| {
                                                    crate::ui::badge::should_paint(
                                                        a,
                                                        mullion_store::ColorTarget::Tab,
                                                    )
                                                })
                                        });
                                        self.ui.tab_props =
                                            Some(crate::ui::tab_props::TabPropsDraft {
                                                tab_id: tab.id,
                                                name: tab.display_title().to_string(),
                                                color,
                                            });
                                    }
                                    self.ui_dirty = true;
                                }
```

`touched_store` 里删掉 `|| self.ui.tab_props_save.is_some()` 那一项（连同它上面
那段「E3:标签属性弹窗改的是会话的 identity.name…」的注释）。

施加点（原 `if let Some(...TabPropsAction::Save { session_id, name, color }) = ...` 整段）换成：

```rust
                // F122:标签属性的施加点。**不碰 store** —— 只写标签自己的
                // 两个覆盖字段。放在这里(而不是渲染闭包里)的理由不变:
                // 闭包里 `self.tabs` 正被借出去画标签栏。
                if let Some(crate::ui::tab_props::TabPropsAction::Save {
                    tab_id,
                    name,
                    color,
                }) = self.ui.tab_props_save.take()
                {
                    apply_tab_props(&mut self.tabs, tab_id, name, color);
                    self.ui_dirty = true;
                }
```

在原 `apply_tab_props_save` 的位置写新的自由函数：

```rust
/// F122:把弹窗里改的名字/颜色写到那个标签上。
///
/// 空名字(或只有空白)= 清除覆盖,退回连接时拼的 `title` —— 存一个空标签名
/// 会让标签栏上出现一块点得到但看不见的东西。
///
/// 自由函数而不是 `&mut self` 方法:调用点在渲染闭包之后,`self` 的其它字段
/// 此时另有借用(同 `apply_save` 的理由),而且这样能脱离 `App` 单测。
fn apply_tab_props<C>(
    tabs: &mut crate::shell::tabs::Tabs<C>,
    tab_id: crate::shell::tabs::TabId,
    name: String,
    color: Option<mullion_term::snapshot::Rgb>,
) {
    let trimmed = name.trim();
    if let Some(tab) = tabs.iter_mut().find(|t| t.id == tab_id) {
        tab.title_override = (!trimmed.is_empty()).then(|| trimmed.to_string());
        tab.color_override = color;
    }
}
```

- [ ] **Step 5: 跑绿**

Run:
```bash
cargo test -p mullion-app --lib tab_props 2>&1 | grep -E "test result|FAILED"
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result: FAILED|panicked" /tmp/test.log
```
Expected: 前者 ok，后者无输出

- [ ] **Step 6: 提交**

```bash
git add -u crates/mullion-app/src
git commit -m "feat(app): 标签的名称/颜色改成只作用于本标签的运行期覆盖 (F122)

不再写 SessionRecord —— 会话列表那条、pane 标题条、状态栏都不再跟着变。
删掉 apply_tab_props_save / sync_tab_titles_for_session 及其测试。
Modal::TabProps 保留(T8:弹窗里敲的字不许漏给远端 shell)。"
```

---

## Task 4：标签整块背景色 + 右键解禁（F122 视觉）

**Files:**
- Modify: `crates/mullion-app/src/theme.rs`（`contrast_ratio` 之后，约 215 行）
- Modify: `crates/mullion-app/src/ui/chrome.rs`（`TabView` 约 147 行、`one_tab` 约 231 行）
- Modify: `crates/mullion-app/src/app.rs`（构造 `tab_views`，约 5487 行）
- Test: `theme.rs` / `chrome.rs` 各自的 `mod tests`

- [ ] **Step 1: 写失败的测试**

`theme.rs` 的 `mod tests`：

```rust
/// F122:整块上色后,标题文字的对比度不能再靠色板纪律 —— 用户配的是任意
/// hex。`readable_fg` 必须在任何底色上都给出 ≥ 4.5:1(WCAG 1.4.3 文本阈值)。
///
/// 自证会变红:把 `readable_fg` 改成恒返回浅色。
#[test]
fn readable_fg_clears_the_text_threshold_on_any_background() {
    // 8 个预设直接取 `LABEL_PALETTE`,不手抄一份 —— 抄的那份改了色板不会跟着变。
    let presets = LABEL_PALETTE.iter().map(|(_, hex, _)| *hex);
    for hex in presets.chain(["#000000", "#ffffff", "#808080"]) {
        let bg = parse_hex(hex).unwrap();
        let fg = readable_fg(bg);
        assert!(
            contrast_ratio(fg, bg) >= 4.5,
            "{hex} 上的前景色只有 {:.2}:1",
            contrast_ratio(fg, bg)
        );
    }
}
```

`chrome.rs` 的 `mod tests`（没有就新建 `#[cfg(test)] mod tests { use super::*; ... }`）：

```rust
/// F122:标签底色四象限。有色时整块上色(活动满色、非活动降一档),
/// 无色时维持原来的两档主题底色。
///
/// 自证会变红:让非活动有色标签也返回满色 —— 第二条断言会红。
#[test]
fn tab_fill_covers_the_four_cases() {
    // 主题是常量,不是构造函数(`theme::MULLION_DARK`)。
    let t = &crate::theme::MULLION_DARK;
    let c = egui::Color32::from_rgb(0xe0, 0x67, 0x67);
    assert_eq!(tab_fill(Some(c), true, t), c, "活动 + 有色 = 满色");
    assert_ne!(tab_fill(Some(c), false, t), c, "非活动要降一档");
    assert_ne!(
        tab_fill(Some(c), false, t),
        theme::c32(t.bar_tool),
        "降一档不等于不上色"
    );
    assert_eq!(tab_fill(None, true, t), theme::c32(t.panel_head));
    assert_eq!(tab_fill(None, false, t), theme::c32(t.bar_tool));
}
```

- [ ] **Step 2: 跑一遍确认它们红**

Run: `cargo test -p mullion-app --lib readable_fg 2>&1 | tail -5`
Expected: 编译失败，`cannot find function 'readable_fg'`

- [ ] **Step 3: 实现两个纯函数**

`theme.rs`（`contrast_ratio` 之后）：

```rust
/// 铺在 `bg` 上的文字该用深色还是浅色。
///
/// **实算对比度取胜者**,不按亮度阈值拍脑袋:阈值法在中间调(#808080 一带)
/// 两边都勉强,而 F122 的标签底色是用户自选的任意 hex,没有色板纪律兜底。
///
/// 两个候选取纯黑/纯白:任何底色上,这两个里至少有一个 ≥ 4.5:1。
pub fn readable_fg(bg: Rgb) -> Rgb {
    let dark = Rgb::new(0, 0, 0);
    let light = Rgb::new(0xff, 0xff, 0xff);
    if contrast_ratio(dark, bg) >= contrast_ratio(light, bg) {
        dark
    } else {
        light
    }
}
```

`ui/chrome.rs`（`one_tab` 之前）：

```rust
/// F122:标签底色。`color` = 有效色(标签覆盖 ?? 会话色),`None` 时退回主题两档。
///
/// 非活动标签把有效色按 `INACTIVE_MIX` 混面板底色降一档 —— 一排标签全是满色
/// 时,「哪个是当前的」就只能靠底部横杠一条线去认了。
pub fn tab_fill(color: Option<egui::Color32>, active: bool, t: &Theme) -> egui::Color32 {
    match (color, active) {
        (Some(c), true) => c,
        (Some(c), false) => mix(theme::c32(t.bar_tool), c, INACTIVE_MIX),
        (None, true) => theme::c32(t.panel_head),
        (None, false) => theme::c32(t.bar_tool),
    }
}

/// 非活动标签保留多少节点色。0.55 是「一眼还认得出是哪个颜色」与
/// 「和活动标签分得开」两边的折中,人工验收清单里有一条专门看它。
const INACTIVE_MIX: f32 = 0.55;

/// 线性混色。与 `session_manager::list::blend` 同一手法 —— 那个是
/// `pub(crate)` 到 `session_manager` 内部的私有函数,跨模块复用要先提可见性,
/// 而这里只用一处,先各留一份;哪天出现第三处再收敛到 `theme`。
fn mix(base: egui::Color32, top: egui::Color32, a: f32) -> egui::Color32 {
    let f = |b: u8, t: u8| (b as f32 + (t as f32 - b as f32) * a).round().clamp(0.0, 255.0) as u8;
    egui::Color32::from_rgb(
        f(base.r(), top.r()),
        f(base.g(), top.g()),
        f(base.b(), top.b()),
    )
}
```

- [ ] **Step 4: 跑绿**

Run: `cargo test -p mullion-app --lib -- readable_fg tab_fill 2>&1 | grep "test result"`
Expected: `test result: ok. 2 passed`

- [ ] **Step 5: `TabView` 带上有效色，`one_tab` 用它画**

`chrome.rs` 的 `TabView` 加字段（`appearance` 字段保留，图标还要用）：

```rust
    /// F122:这个标签的**有效色** = 标签覆盖 ?? 会话色(过 `ColorTarget::Tab`
    /// 闸门)。在 `app.rs` 构造时算好 —— 一个落点一份判定,`one_tab` 里
    /// 不许再自己调一次 `should_paint`,两处迟早分叉。
    pub color: Option<egui::Color32>,
```

`one_tab` 里，把原来算 `node` / `bg` / 画底与横杠那一段换成：

```rust
    let node = v.color;
    let bg = tab_fill(node, v.active, t);
    let p = ui.painter();
    p.rect_filled(rect, egui::Rounding::same(4.0), bg);
    // F122:活动横杠**保留**。整块上色之后它不再表达节点色,只表达
    // 「哪个是活动标签」—— 有色时用前景色画,才在满色底上看得见。
    let fg = match node {
        Some(_) => theme::c32(theme::readable_fg(mullion_term::snapshot::Rgb::new(
            bg.r(),
            bg.g(),
            bg.b(),
        ))),
        None => theme::c32(if v.active { t.fg_strong } else { t.fg_muted }),
    };
    if v.active {
        p.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(rect.min.x, rect.max.y - TAB_UNDERLINE_H),
                rect.max,
            ),
            egui::Rounding::same(1.0),
            match node {
                Some(_) => fg,
                None => theme::c32(t.accent),
            },
        );
    }
```

下面画图标与标题的两处改用 `fg`：

```rust
                crate::ui::badge::paint_icon(ui.painter(), r, icon, Some(fg));
            }
            ui.add(
                egui::Label::new(egui::RichText::new(v.title).color(fg))
                    .truncate()
                    // E1:**必须关掉**。egui 0.30 默认 `selectable_labels = true`,
                    // 可选中文本的 Label 会 sense click 把命中抢走,自己又不做
                    // 任何事 —— 现象是「点标签的文字那一片没反应」。
                    .selectable(false),
            );
```

右键菜单解禁（D6）：`has_has_session` 那三段换成

```rust
    resp.context_menu(|ui| {
        // F122:覆盖挂在标签上,不需要会话记录 —— 快速连接开的标签一样能
        // 改名配色,原来那道 `add_enabled(has_session, ..)` 的前提已经不存在。
        if ui.button("重命名…").clicked() {
            props = true;
            ui.close_menu();
        }
        if ui.button("设置颜色…").clicked() {
            props = true;
            ui.close_menu();
        }
        ui.separator();
        if ui.button("关闭").clicked() {
            closed = true;
            ui.close_menu();
        }
    });
```

- [ ] **Step 6: `app.rs` 构造 `tab_views` 时算有效色**

```rust
                            let tab_views: Vec<crate::ui::chrome::TabView<'_>> = self
                                .tabs
                                .iter()
                                .enumerate()
                                .map(|(i, tab)| {
                                    let appearance =
                                        tab.session_id.and_then(|sid| self.appearance.get(sid));
                                    crate::ui::chrome::TabView {
                                        title: tab.display_title(),
                                        active: i == active_ix,
                                        session_id: tab.session_id,
                                        appearance,
                                        // F122:覆盖优先,否则会话色(设计 D5:
                                        // 同一条视觉通道,一个标签上不出现两种颜色)。
                                        color: tab.color_override.map(theme::c32).or_else(|| {
                                            appearance.and_then(|a| {
                                                crate::ui::badge::should_paint(
                                                    a,
                                                    mullion_store::ColorTarget::Tab,
                                                )
                                            })
                                        }),
                                    }
                                })
                                .collect();
```

- [ ] **Step 7: 补一条取值序的测试**（`chrome.rs` 的 `mod tests`）

```rust
/// F122/D5:有效色的取值序 = 标签覆盖 > 会话色 > 无。这条判据在 `app.rs`
/// 构造 `TabView` 时施加,这里锁住它的**语义等价形式**,免得将来有人把
/// `or_else` 写反 —— 写反的现象是「标签配了色但显示的是会话色」。
///
/// 自证会变红:把下面 `or_else` 的两边对调。
#[test]
fn the_override_colour_wins_over_the_session_colour() {
    let over = egui::Color32::from_rgb(1, 2, 3);
    let session = egui::Color32::from_rgb(4, 5, 6);
    let pick = |o: Option<egui::Color32>, s: Option<egui::Color32>| o.or(s);
    assert_eq!(pick(Some(over), Some(session)), Some(over));
    assert_eq!(pick(None, Some(session)), Some(session));
    assert_eq!(pick(None, None), None);
}
```

- [ ] **Step 8: 跑绿并提交**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result: FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check
git add -u crates/mullion-app/src
git commit -m "feat(app): 标签色改画整块背景,前景按对比度自动取色 (F122/F62)

活动横杠保留,只表达「哪个是活动标签」;有色时用 readable_fg 画。
快速连接标签的右键改名/配色一并解禁(覆盖不再需要会话记录)。"
```

---

## Task 5：拖拽落点与门控纯函数（F121 判定层）

**Files:**
- Create: `crates/mullion-app/src/ui/session_manager/reorder.rs`
- Modify: `crates/mullion-app/src/ui/session_manager/mod.rs`（模块声明处加 `pub(crate) mod reorder;`）

- [ ] **Step 1: 建文件，先写测试再写实现**

```rust
//! F121:左栏拖拽排序的判定层。**零 egui**,可纯单测。
//!
//! UI 只负责「指针在哪一行的哪半边」,落点算什么意图全在这里 ——
//! 判据散在渲染代码里的话,「拖到组内最后一行的下半」这类边界只能靠手点。

use mullion_store::{GroupId, SessionId};

/// 一次拖拽的结论。字段与 `Vault::move_session` 的参数一一对应,
/// 中间不做二次翻译。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReorderIntent {
    pub id: SessionId,
    pub group: Option<GroupId>,
    /// 插在谁前面。`None` = 该组末尾。
    pub before: Option<SessionId>,
}

/// 松手落在某一行上。`next_in_group` = 被悬停行在**该组内**的下一条
/// (组内最后一行传 `None`)。
///
/// 上半 → 插在它前面;下半 → 插在它后面(= 它的下一条前面)。
/// 拖到自己身上 → `None`,什么都不做。
pub(crate) fn drop_on_row(
    dragged: SessionId,
    over: SessionId,
    over_group: Option<GroupId>,
    next_in_group: Option<SessionId>,
    upper_half: bool,
) -> Option<ReorderIntent> {
    if dragged == over {
        return None;
    }
    Some(ReorderIntent {
        id: dragged,
        group: over_group,
        before: if upper_half { Some(over) } else { next_in_group },
    })
}

/// 松手落在分组头上 → 插到该组末尾。折叠的组、空组都只能从这里进。
pub(crate) fn drop_on_group(dragged: SessionId, group: Option<GroupId>) -> ReorderIntent {
    ReorderIntent {
        id: dragged,
        group,
        before: None,
    }
}

/// 这一帧准不准拖(设计 D9)。
///
/// 搜索中、或 `Icons` 档下都有行被藏起来,此时**可见顺序 ≠ 真实顺序**:
/// 松手落在两行之间,到底插在哪一条隐藏行的前后是歧义的。与其猜一个,
/// 不如不让拖 —— 猜错的代价是用户的顺序被悄悄改成他没要的样子。
pub(crate) fn drag_enabled(query: &str, density: super::list::Density) -> bool {
    query.trim().is_empty() && density != super::list::Density::Icons
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::session_manager::list::Density;

    fn sid(n: u64) -> SessionId {
        SessionId(n)
    }

    #[test]
    fn upper_half_inserts_before_the_hovered_row() {
        let i = drop_on_row(sid(1), sid(2), None, Some(sid(3)), true).unwrap();
        assert_eq!(i.before, Some(sid(2)));
    }

    /// 自证会变红:把 `upper_half` 的两个分支对调。
    #[test]
    fn lower_half_inserts_before_the_next_row() {
        let i = drop_on_row(sid(1), sid(2), None, Some(sid(3)), false).unwrap();
        assert_eq!(i.before, Some(sid(3)));
    }

    /// 组内最后一行的下半区 = 组末尾。没有这条,拖到列表最下面会没反应。
    #[test]
    fn lower_half_of_the_last_row_means_the_end_of_the_group() {
        let i = drop_on_row(sid(1), sid(2), None, None, false).unwrap();
        assert_eq!(i.before, None);
    }

    #[test]
    fn dropping_a_row_onto_itself_does_nothing() {
        assert!(drop_on_row(sid(1), sid(1), None, None, true).is_none());
    }

    /// 跨组:目标行所在的组就是新组。
    #[test]
    fn the_hovered_rows_group_becomes_the_new_group() {
        let g = GroupId(9);
        let i = drop_on_row(sid(1), sid(2), Some(g), None, true).unwrap();
        assert_eq!(i.group, Some(g));
    }

    #[test]
    fn dropping_on_a_group_header_appends_to_that_group() {
        let g = GroupId(9);
        let i = drop_on_group(sid(1), Some(g));
        assert_eq!((i.group, i.before), (Some(g), None));
    }

    /// D9 的门控。自证会变红:把 `drag_enabled` 改成恒 `true`。
    #[test]
    fn dragging_is_off_while_filtering_because_the_visible_order_is_not_the_real_one() {
        assert!(drag_enabled("", Density::Full));
        assert!(!drag_enabled("web", Density::Full), "搜索中不许拖");
        assert!(!drag_enabled("  ", Density::Icons), "图标档藏了行,不许拖");
        assert!(!drag_enabled("", Density::Icons));
    }
}
```

> 执行者注意：`Density` 现在是 `pub(crate)`，`reorder.rs` 在同一 crate 内可见；
> 若编译器报可见性错误，把 `list.rs` 里 `enum Density` 的可见性保持 `pub(crate)` 即可，
> **不要**为此把它提成 `pub`。

- [ ] **Step 2: 挂模块**

`crates/mullion-app/src/ui/session_manager/mod.rs` 的模块声明区加：

```rust
pub(crate) mod reorder;
```

- [ ] **Step 3: 跑绿**

Run: `cargo test -p mullion-app --lib reorder 2>&1 | grep "test result"`
Expected: `test result: ok. 7 passed`

- [ ] **Step 4: 提交**

```bash
git add crates/mullion-app/src/ui/session_manager/reorder.rs crates/mullion-app/src/ui/session_manager/mod.rs
git commit -m "feat(app): 左栏拖拽排序的判定层 —— 落点与门控纯函数 (F121)"
```

---

## Task 6：左栏拖拽接线（F121 交互）

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/list.rs`（`show` 约 518-708、`row` 约 709-）
- Modify: `crates/mullion-app/src/ui/mod.rs`（`UiState` 加字段）
- Modify: `crates/mullion-app/src/app.rs`（`touched_store` 段加施加点）

- [ ] **Step 1: `UiState` 加意图字段**（`ui/mod.rs`，挨着 `move_to_group`）

```rust
    /// F121:左栏拖拽排序的结论。同 `move_to_group`:UI 只写意图,
    /// `app.rs` 才碰 store。
    pub reorder_request: Option<crate::ui::session_manager::reorder::ReorderIntent>,
```

- [ ] **Step 2: 写守护测试**（`list.rs` 的 `mod tests`）

```rust
/// F58 踩过的坑,这里必须再钉一次:`ScrollArea` 的 `drag_to_scroll` 默认开着,
/// 它在视口上注册一个吃 drag 的部件,把按在行上的那一下抢去当滚动手势 ——
/// 行的 `drag_started()` 永远为假,拖拽排序整个功能安静地不存在。
///
/// 自证会变红:删掉 `list.rs` 里那行 `.drag_to_scroll(false)`。
#[test]
fn the_list_scroll_area_does_not_eat_drags() {
    let src = include_str!("list.rs");
    assert!(
        src.contains(".drag_to_scroll(false)"),
        "左栏 ScrollArea 没关掉 drag_to_scroll,行的 drag_started() 会恒假(F58/F121)"
    );
}
```

- [ ] **Step 3: 跑一遍确认它红**

Run: `cargo test -p mullion-app --lib the_list_scroll_area_does_not_eat_drags 2>&1 | grep -E "test result|assertion"`
Expected: FAILED，`左栏 ScrollArea 没关掉 drag_to_scroll`

- [ ] **Step 4: 接线**

`show` 里那个 `egui::ScrollArea::vertical()` 补一行（紧跟在 `.auto_shrink` 之前）：

```rust
        // F121:**必须关掉**,理由见 `the_list_scroll_area_does_not_eat_drags`。
        .drag_to_scroll(false)
```

`show` 的行循环里，把每一行在**本组内的下一条**算出来传给 `row`：

```rust
                        for (i, r) in members.iter().enumerate() {
                            let next_in_group = members.get(i + 1).map(|n| n.id);
                            row(
                                ui,
                                t,
                                ui_state,
                                r,
                                next_in_group,
                                gid,
                                drag_on,
                                sessions,
                                groups,
                                credentials,
                                tunnels,
                                running_note.as_deref(),
                                pending_delete_target,
                                &mut pending_delete_rendered,
                                appearance,
                                d,
                            );
                        }
```

`drag_on` 在 `show` 里算一次（放在 `let d = density_for(..)` 之后）：

```rust
    // F121/D9:这一帧准不准拖。搜索中或 `Icons` 档下有行被藏起来,
    // 可见顺序不等于真实顺序,落点是歧义的 —— 见 `reorder::drag_enabled`。
    let drag_on = super::reorder::drag_enabled(&ui_state.search, d);
```

分组头接落点（`group_header(...).show(ui, ...)` 拿到的 `header` 之后）：

```rust
                // F121:分组头也是落点 —— 折叠的组、空组只能从这里进去。
                if let Some(from) = header
                    .header_response
                    .dnd_release_payload::<DragSession>()
                {
                    ui_state.reorder_request = Some(super::reorder::drop_on_group(from.0, gid));
                }
```

`list.rs` 顶部的 `use` 补上 `GroupId`（现在只 import 了 `GroupRecord, SessionId`）：

```rust
use mullion_store::{GroupId, GroupRecord, SessionId};
```

`list.rs` 顶部加载荷类型：

```rust
/// F121:拖拽载荷。裹一层 newtype 而不是直接用 `SessionId` ——
/// egui 的 `DragAndDrop` 按类型取载荷,裸 id 会跟将来别处的拖拽撞类型。
#[derive(Debug, Clone, Copy)]
pub(crate) struct DragSession(pub SessionId);
```

`row` 的签名加四个参数（`next_in_group: Option<SessionId>`、`group: Option<GroupId>`、
`drag_on: bool` 放在 `rec` 之后），函数体里 `let resp = session_row(...)` 之后插入：

```rust
    // F121:行既是拖源也是落点。
    if drag_on {
        if resp.drag_started() {
            resp.dnd_set_drag_payload(DragSession(rec.id));
        }
        // 插入线要在**松手之前**看得见:落点规则不写在屏幕上,用户就只能
        // 松手试一次再撤销(而这里没有撤销)。
        if let Some(from) = egui::DragAndDrop::payload::<DragSession>(ui.ctx()) {
            if from.0 != rec.id && resp.contains_pointer() {
                let upper = ui
                    .input(|i| i.pointer.hover_pos())
                    .is_some_and(|p| p.y < resp.rect.center().y);
                let y = if upper { resp.rect.top() } else { resp.rect.bottom() };
                ui.painter().hline(
                    resp.rect.x_range(),
                    y,
                    egui::Stroke::new(2.0, theme::c32(t.accent)),
                );
            }
        }
        if let Some(from) = resp.dnd_release_payload::<DragSession>() {
            let upper = ui
                .input(|i| i.pointer.interact_pos())
                .is_some_and(|p| p.y < resp.rect.center().y);
            ui_state.reorder_request =
                super::reorder::drop_on_row(from.0, rec.id, group, next_in_group, upper);
        }
    }
```

`session_row` 里的 `egui::Sense::click()` 改成 `egui::Sense::click_and_drag()`。

`drag_on == false` 时给一句说明（接在 `session_row` 返回的 tooltip 之后，
`row` 里 `resp` 上再挂一层）：

```rust
    let resp = if drag_on {
        resp
    } else {
        resp.on_hover_text("搜索中 / 图标档下不能拖动排序 —— 有行被藏起来,落点会有歧义")
    };
```

- [ ] **Step 5: `app.rs` 施加**（挨着 `move_to_group` 那段，约 6030 行）

`touched_store` 的表达式里加一项：

```rust
                    // F121:拖拽排序改的是会话的 group_id 与顺序 —— 跨组会换
                    // 继承来源,外观(图标/颜色)可能跟着变,必须重算全表。
                    || self.ui.reorder_request.is_some()
```

施加点：

```rust
                if let Some(i) = self.ui.reorder_request.take() {
                    if let Some(store) = self.store.as_mut() {
                        match store
                            .move_session(i.id, i.group, i.before)
                            .and_then(|_| store.save())
                        {
                            Ok(()) => self.ui.set_toast("已调整顺序"),
                            Err(e) => self.ui.set_error(format!("调整顺序失败:{e}")),
                        }
                    }
                }
```

上面那段 `if self.ui.delete_request.is_some() || ... { diag::mark(diag::Stage::StoreIo); }`
的条件里也加 `|| self.ui.reorder_request.is_some()`（它同样是同步 IO）。

- [ ] **Step 6: 跑绿**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result: FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check
```
Expected: 第一条无输出，第二条无输出

- [ ] **Step 7: 提交**

```bash
git add -u crates/mullion-app/src
git commit -m "feat(app): 左栏拖拽排序 —— 组内换位与跨组拖动 (F121)

ScrollArea 必须关掉 drag_to_scroll(F58 同款坑,有测试钉着)。
搜索中/图标档下禁用拖拽:可见顺序不等于真实顺序,落点有歧义。"
```

---

## Task 7：spec 修订

**Files:**
- Modify: `spec.md`

- [ ] **Step 1: 新增两行**（F120 那行之后，§4.4 表格末尾）

```markdown
| F121 | **会话列表手动排序**：左栏拖拽调整会话顺序，支持组内换位与跨组拖动（跨组顺带改 `group_id`，等价于右键「移动到分组」）。顺序真值 = `sessions.toml` 的数组顺序，**不加排序字段、无 schema 改动**。分组自身的顺序不在范围内 | P2 | `Vault::move_session` 的位置算术为纯函数单测（先移除再定位、`before` 悬空落末尾、拖到自己身上 no-op、不动 `modified_at`）；落点判定（上/下半、组内末行、拖到分组头）为纯函数单测；搜索中/`Icons` 档禁用拖拽有单测；`ScrollArea` 关掉 `drag_to_scroll` 有源码级守护（F58 同款坑）；真实拖放观感人工验收 |
| F122 | **标签本地覆盖**：标签的名称/颜色只作用于该标签自身，**不写回会话记录**（会话列表、pane 标题条、状态栏一律不变），也不进 F37 布局快照——关窗口即丢。同一会话的两个标签可各自改名配色；快速连接开的标签同样可改。颜色画成**整块背景**，标题前景按对比度自动取黑/白 | P2 | 保存后 `store.list()` 逐字段等价（一行都不许写）有单测；覆盖归属标签而非会话（同会话两标签互不影响）有单测；重连（`Tabs::replace`）不丢覆盖有单测；`readable_fg` 在 8 个预设 + 黑/白/中灰上对比度 ≥ 4.5:1 实算单测；`Modal::TabProps` 仍在 `Modal::ALL` 里（T8）；实际观感人工验收 |
```

- [ ] **Step 2: 修订两条既有条目**

- F36 那行的正文里，「不做拖拽重排」之后补一句：
  `；标签的名称/颜色支持**本地覆盖**（F122），覆盖不落盘`
- F62 那行的落点表述改为：
  `落点由 ColorSpec.apply_to 决定：会话列表 / pane 标题条 / 状态栏 / 标签页（标签页落点自 F122 起画成**整块背景**，不再是底部横杠；标签自己的覆盖色优先于会话色）`

- [ ] **Step 3: 提交**

```bash
git add spec.md
git commit -m "docs: spec 补 F121/F122,修订 F36/F62 的落点表述"
```

---

## Task 8：发版（交付约定一条龙）

- [ ] **Step 1: bump 版本**

`Cargo.toml` 的 `workspace.package.version` 改 `0.1.44`。

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: 版本 0.1.44(标签名称/颜色改成本地覆盖 + 左栏拖拽排序)"
```

- [ ] **Step 2: 跑绿**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```
Expected: 无 FAILED、clippy 无输出、fmt 无输出

- [ ] **Step 3: 交叉编译 + objdump 验收**

```bash
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app
x86_64-w64-mingw32-objdump -p target/x86_64-pc-windows-gnu/release/mullion.exe | grep "DLL Name"
```
Expected: 输出里**不得**出现 `libgcc_s_seh-1.dll` / `libwinpthread-1.dll`
（出现即不合格，按 `docs/cross-compile-windows.md` 修）

- [ ] **Step 4: 发 Release**

```bash
cd target/x86_64-pc-windows-gnu/release && sha256sum mullion.exe > mullion.exe.sha256
HTTPS_PROXY=http://127.0.0.1:7890 gh release create v0.1.44 \
  mullion.exe mullion.exe.sha256 -t "v0.1.44" -F notes.md --repo kilobitcy/Mullion
```

`notes.md` 里写：修了什么 + 下面这份人工验收清单 + sha256 + 首次运行提示
（未签名 exe 会被 SmartScreen 拦，`Unblock-File .\mullion.exe`）。

**人工验收清单：**

- [ ] 标签整块上色后，8 个预设色 + 几个自选深/浅色下标题文字都读得清
- [ ] 非活动有色标签与活动有色标签能一眼分出哪个是活动的
- [ ] 改标签名/色后：会话管理器左栏那条、pane 标题条、状态栏**都没变**
- [ ] 同一台机开两个标签，分别改名改色互不干扰；关掉再开退回会话名
- [ ] 快速连接开的标签也能改名配色
- [ ] 左栏拖拽：组内换位、拖进另一个组、拖到折叠组的头上、拖到空组
- [ ] 拖动过程中插入线的位置与松手后的落点一致
- [ ] 搜索框有内容时拖不动，且提示说得清为什么
- [ ] 排序结果重启后还在

- [ ] **Step 5: 报告** —— Release 链接 + sha256 + 上面这份清单
