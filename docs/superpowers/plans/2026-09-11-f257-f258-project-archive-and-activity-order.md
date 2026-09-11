# F257 项目归档 + F258 最后活跃排序 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让项目可以「归档」收进第二个 tab（搜索仍穿透），并让「换节点」弹窗与三处项目列表按**最后活跃**排序。

**Architecture:** 两个独立切片、两个 commit、**一次发版**（0.1.105 → 0.1.106）。
F257 先做（决定列表「列什么」），F258 后做（决定「怎么排」），第二片踩在第一片稳定后的接口上。
两片共用一个新纯函数模块 `ui/project_list.rs` —— 三处项目列表（启动页 / 项目管理器左栏 / 切换项目弹窗）
的「列哪些行 + 按什么排 + 空了说什么」全部收在那里，调用方只负责画。

**Tech Stack:** Rust / egui 0.30 / `mullion-store`（TOML + serde）/ `time` crate。

---

## 设计决案（grilling 逐条拍板，实现时不得偏离）

| # | 决策 | 来源 |
|---|---|---|
| D1 | 归档字段 = `ProjectRecord.archived_at: Option<String>`（RFC3339，app 注入时钟，`default` + `skip_serializing_if`），**升 schema v11 → v12** | Q11-B（+ Task 1 复核推翻原「不升 schema」） |
| D2 | tab 词：「**在用**」/「**归档**」；动作：「归档」/「**取消归档**」（"恢复"是 F148 的词，不许复用） | Q9 |
| D3 | tab **只在项目管理器**出现。启动页 / 切换项目弹窗只列「在用」 | Q10-A |
| D4 | **搜索态没有 tab**：搜索框非空时 tab 栏整条收起，列表跨两态；清空搜索 tab 栏回来。接受这一次布局位移 | Q16-B |
| D5 | 归档项目**照常参与** `validate_project`（项目名 + tmux 名都不释放）→ 取消归档永不失败 | Q13-A |
| D6 | 归档零运行时影响：tmux 照跑、`.alive` 照报、灯照亮、pane 照在；归档项目**能打开**；打开**不自动**取消归档。`archived_at` **只由两个按钮写** | Q12-A |
| D7 | 入口 = 项目管理器右栏底部，摆在「删除项目」**左边**，非 danger 色，**无确认框**；不做右键、不做批量 | Q14-A |
| D8 | 空态判据抽成**一个共用函数**返回枚举，三处调它，不各写 `if` | Q15 |
| D9 | 会话侧 `SessionRecord.last_connected_at: Option<String>`，落 `sessions.toml` | Q3-A |
| D10 | 写入判据 = **这次拨号是用户当帧的动作发起的**。落地为 `DialTicket.user_initiated`，**不是**在 `ConnectOk` 里 `if` 猜来源 | Q6 |
| D11 | rehost 顶部「最近连过」段 **3 条**，下面主段保持 `visible_order` 左栏同序，**不去重**；搜索框非空时整段收起 | Q4-B / Q5-A |
| D12 | `by_recent_access` 加 `lamps` 入参改名 `by_activity`，**三处统一**，不新开函数 | Q8 |
| D13 | 排序表（无空格）：<br>浏览·在用 = Lit 置顶 → `last_accessed_at` 倒序 → id 兜底（**定格**）<br>浏览·归档 = `archived_at` 倒序 → id 兜底（不定格）<br>搜索（跨两态）= 在用段整体在前 → 段内各按上表，**不做 Lit 置顶**（不定格） | Q7-B / Q11 / Q17-A |

### D1 的更正：`archived_at` 要升 schema（Task 1 复核推翻）

原判「与 `last_accessed_at` 同姿态，所以不升 schema」**类比错了**。
`model.rs:215-239` 是这个仓库的成文规则：新增持久化字段只要旧客户端会「当未知字段
丢掉再写回」，就升号让旧客户端**明确拒绝**。`icon`（F238）为此升到 v11，理由是
「用户设的图标静默消失」。

`archived_at` 比 icon 更该保护：丢掉它 = **用户主动做过的归档判断被系统悄悄推翻**，
正是这个字段自己的注释在强调的那类静默错误。

分界线是**可再生性**，不是「长得像」：
- `last_accessed_at` / `last_connected_at`（F258）= 派生数据，下次打开/连接就重写，
  丢了自愈 → **不升号**。
- `archived_at` = 用户的决定，丢了不会自愈 → **升号（v12）**。

所以 F258 的 `SessionRecord.last_connected_at`（Task 8）**仍然不升 schema**，
理由已在上面这条分界线里说清。

### D13 的「定格」实现细化（对 Q7 机制的收窄，行为不变）

Q7 拍板的是「排序在列表打开那一刻定格，之后灯只改颜色、不改位置」。
**唯一需要定格的输入是灯**——`last_accessed_at` 只在「打开项目」时变，而那个动作本身就会关掉这三个列表。
所以不存整份顺序快照（那要显式失效，漏一处的症状是"顺序永远停在上次"，静默），
只存**一份灯的拷贝**：

- 排序读**冻结的灯**；行上画的灯读**实时的灯**（灯变色、位置不动，正是 Q7 要的）。
- 冻结表里查不到的项目（列表开着的时候新建的）→ 当 `Lamp::Unknown` → 不置顶 → 按访问时间落位。**不会消失**。
- 失效点因此只剩「列表关掉」一处，且每处都与它自己的开关同一行代码，漏不掉。

一条必须写进守护的前提：**启动页上灯不可能是 `Unknown`**。
`project::lamp` 只在「有 pane 没上报过」时返回 `Unknown`，而 launcher 态一块 pane 都没有
（`launcher::show` 的 `pane` 恒传 `None`），`panes` 是空切片。若这条不成立，
启动页第一帧就会把全部灯冻成 `Unknown`，Lit 置顶**在最需要它的场景下永远不生效且零报错**。

---

## 文件结构

### 新建

| 文件 | 职责 |
|---|---|
| `crates/mullion-app/src/ui/project_list.rs` | 三处项目列表共用的**纯函数**：`Tab` / `rows()` / `EmptyReason` / `empty_text()`。零 egui 状态、零 IO，可纯单测 |

### 修改

| 文件 | 改什么 |
|---|---|
| `crates/mullion-store/src/project.rs` | `+ archived_at` 字段；归档仍参与 `validate` 的守护 |
| `crates/mullion-store/src/model.rs` | `+ last_connected_at` 字段 |
| `crates/mullion-store/src/vault.rs` | `add_project` 补字段；`+ set_project_archived`；`+ touch_session_connected` |
| `crates/mullion-app/src/ui/project_manager.rs` | `+ ProjectIntent::SetArchived`；`by_recent_access` → `by_activity`；tab 栏；右栏底部按钮；左栏改调 `project_list::rows` |
| `crates/mullion-app/src/ui/launcher.rs` | 改调 `project_list::rows` + 新空态 |
| `crates/mullion-app/src/ui/project_pick.rs` | 改调 `project_list::rows` + 新空态 |
| `crates/mullion-app/src/ui/project_row.rs` | 行上「归档」标记 |
| `crates/mullion-app/src/ui/rehost.rs` | 「最近连过」分段 |
| `crates/mullion-app/src/ui/mod.rs` | `UiState` 新字段；两处 `else` 清冻结灯 |
| `crates/mullion-app/src/app.rs` | `SetArchived` 消费；`DialTicket.user_initiated`；`ConnectOk` 记一笔；冻结灯装填 |
| `spec.md` | 补 F257 / F258 两行 |

---

# 第一部分 · F257 项目归档

## Task 1: store —— `archived_at` 字段

**Files:**
- Modify: `crates/mullion-store/src/project.rs`（`ProjectRecord` 结构 + 测试）
- Modify: `crates/mullion-store/src/vault.rs:798` (`add_project`)
- Modify（补构造字面量，逐处加 `archived_at: None`）：
  `crates/mullion-store/src/project.rs:697`、`crates/mullion-store/src/vault.rs:821`、
  `crates/mullion-app/src/automation.rs:476`、
  `crates/mullion-app/src/project.rs:439,497,655,891,1254`、
  `crates/mullion-app/src/ui/mod.rs:2535`、`crates/mullion-app/src/ui/project_row.rs:295`、
  `crates/mullion-app/src/ui/project_manager.rs:834`、`crates/mullion-app/src/ui/project_pick.rs:236`、
  `crates/mullion-app/src/ui/launcher.rs`（`fn proj` 测试助手）

- [ ] **Step 1: 写失败测试**

在 `crates/mullion-store/src/project.rs` 的 `mod tests` 里追加：

```rust
    /// 没归档的项目不该往 TOML 里写空键（同 `tmux_name`/`last_accessed_at`），
    /// 归档了要能原样读回来。
    ///
    /// 自证会变红：把 `archived_at` 上的 `skip_serializing_if` 删掉。
    #[test]
    fn archived_at_round_trips_and_stays_out_of_the_file_when_unset() {
        let mut p = sample();
        assert!(p.archived_at.is_none(), "新记录默认不是归档态");
        let s = toml::to_string_pretty(&p).unwrap();
        assert!(!s.contains("archived_at"), "未归档不应写出这个键: {s}");
        p.archived_at = Some("2026-09-11T08:00:00Z".into());
        let s = toml::to_string_pretty(&p).unwrap();
        let back: ProjectRecord = toml::from_str(&s).unwrap();
        assert_eq!(back, p);
    }

    /// 旧文件里没有这个键，读回来必须是「未归档」而不是解析失败 ——
    /// 失败的话用户升级一次客户端，整个项目表就读不出来了。
    #[test]
    fn a_file_written_before_f257_still_loads_as_not_archived() {
        let text = r#"
id = 3
name = "Mullion"
dir = "/data/Mullion"
created_at = "2026-09-01T00:00:00Z"
"#;
        let back: ProjectRecord = toml::from_str(text).unwrap();
        assert_eq!(back.archived_at, None);
    }
```

`sample()` 若不存在，同时加：

```rust
    fn sample() -> ProjectRecord {
        ProjectRecord {
            id: ProjectId(3),
            name: "Mullion".into(),
            note: String::new(),
            nodes: Vec::new(),
            preferred: None,
            dir: "/data/Mullion".into(),
            tmux_name: None,
            created_at: "2026-09-01T00:00:00Z".into(),
            last_accessed_at: None,
            archived_at: None,
            icon: None,
        }
    }
```

- [ ] **Step 2: 跑测试确认它红**

Run: `cargo test -p mullion-store archived_at 2>&1 | tail -20`
Expected: 编译失败，`ProjectRecord` 没有 `archived_at` 字段。

- [ ] **Step 3: 加字段**

`crates/mullion-store/src/project.rs`，在 `last_accessed_at` 之后插入：

```rust
    /// F257:归档时刻(RFC3339)。`None` = 在用。
    ///
    /// 与 `last_accessed_at` 同姿态的部分:app 注入时钟(store 不持有时钟)、
    /// 旧文件缺键即"未归档"、未归档不写出这个键。
    ///
    /// 但**升了 schema v12**(理由见 `CURRENT_SCHEMA` 文档):`last_accessed_at`
    /// 是可再生的派生数据,丢了下次打开就重写;`archived_at` 是**用户的决定**,
    /// 旧客户端把它当未知字段丢掉再写回 = 归档静默失效,不会自愈。
    ///
    /// 为什么不是 `bool`:归档 tab 要按「什么时候归的」倒序排(刚归错的在最上面,
    /// 马上能撤)。`bool` 只能回落 `last_accessed_at`,那样"上周归档的"和"半年前
    /// 归档的"混在一起,顺序取决于它们当年被打开的时间 —— 解释不通。
    ///
    /// **只由项目管理器右栏那两个按钮写**(设计 D6)。打开一个归档项目**不**自动
    /// 撤销归档:系统看到的只是"你打开了它",而打开的理由可能只是去捞一个文件;
    /// 让系统推翻用户的判断,错的时候是静默的。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<String>,
```

- [ ] **Step 4: 补全部构造字面量**

Run 一遍编译，按报错逐处加 `archived_at: None,`（位置紧跟 `last_accessed_at`）：

```bash
cargo build --workspace 2>&1 | grep -E "^error|-->" | head -40
```

`vault.rs:798` 的 `add_project` 里同样加 `archived_at: None,` —— **新建的项目一律是在用态**。

- [ ] **Step 5: 跑测试确认它绿**

Run: `cargo test -p mullion-store archived 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok.`

- [ ] **Step 6: 提交**

```bash
git add -A
git commit -m "$(cat <<'EOF'
feat(store): 项目记录加归档时刻 archived_at (F257)

Option<String> + skip_serializing_if,与 last_accessed_at 同姿态:
旧文件缺键即「未归档」,不升 schema。存时刻而非 bool —— 归档 tab
要按归档时间倒序,bool 排不出来。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: store —— 归档不释放名字（D5 的守护）+ `set_project_archived`

**Files:**
- Modify: `crates/mullion-store/src/project.rs`（测试）
- Modify: `crates/mullion-store/src/vault.rs`（`set_project_archived`，放在 `touch_project_accessed` 之后）

- [ ] **Step 1: 写失败测试**

`crates/mullion-store/src/project.rs` 的 `mod tests`：

```rust
    /// D5:归档**不**释放名字。归档一个项目之后再建一个同名的,仍然要被拒。
    ///
    /// 反过来的方案(归档退出校验)会让**取消归档**可能撞车 —— 而归档这个功能
    /// 的全部安全感来自"随时能撤回来"。撞车放过去就是
    /// 「两个项目共用一个 Claude Code」,本设计里后果最严重且完全静默的错误。
    ///
    /// 自证会变红:在 `validate` 的 `for other in all.iter()` 上加
    /// `.filter(|o| o.archived_at.is_none())`。
    #[test]
    fn archiving_a_project_does_not_free_up_its_name() {
        let mut old = sample();
        old.archived_at = Some("2026-09-11T08:00:00Z".into());
        let mut fresh = sample();
        fresh.id = ProjectId(9);
        assert_eq!(
            validate(&fresh, &[old.clone(), fresh.clone()], &[]),
            Err(ProjectIssue::DuplicateName { with: old.id }),
            "归档项目仍占着项目名 —— 否则取消归档时会撞车,而那时撤不回来"
        );
    }

    /// tmux 名同理。项目名可以不同、tmux 名撞上也一样是「共用一个 tmux」。
    #[test]
    fn archiving_a_project_does_not_free_up_its_tmux_name() {
        let mut old = sample();
        old.archived_at = Some("2026-09-11T08:00:00Z".into());
        old.tmux_name = Some("shared".into());
        let mut fresh = sample();
        fresh.id = ProjectId(9);
        fresh.name = "另一个".into();
        fresh.tmux_name = Some("shared".into());
        assert!(
            matches!(
                validate(&fresh, &[old, fresh.clone()], &[]),
                Err(ProjectIssue::TmuxNameClash { .. })
            ),
            "归档项目仍占着 tmux 名"
        );
    }
```

- [ ] **Step 2: 跑测试**

Run: `cargo test -p mullion-store does_not_free_up 2>&1 | grep -E "test result|FAILED"`
Expected: **PASS**（`validate` 本来就不看 `archived_at`）。

> 这两条是**回归锁**，不是驱动实现的红灯：它们锁住"以后没人顺手给 `validate` 加归档过滤"。
> 上面标的自证变异必须真的跑一次确认它变红，否则这两条是恒绿的。

- [ ] **Step 3: 跑自证变异**

先 `git status` 确认工作区干净（本项目已有 5 次未提交编辑被 `git checkout` 吞掉的记录）。
临时在 `validate` 里加 `.filter(|o| o.archived_at.is_none())`，跑：

Run: `cargo test -p mullion-store does_not_free_up 2>&1 | grep -E "test result|FAILED"`
Expected: **FAILED**，两条都红。确认后 `git checkout crates/mullion-store/src/project.rs` 撤掉变异。

- [ ] **Step 4: 写 `set_project_archived` 的失败测试**

`crates/mullion-store/src/vault.rs` 的 `mod tests`：

```rust
    /// F257:归档 / 取消归档只改这一个字段,**不跑 validate** —— 理由同
    /// `touch_project_accessed`:收起一个项目不该因为库里别处有个撞名的老项目
    /// 就失败。真要拦,拦在保存那一刻。
    #[test]
    fn archiving_flips_only_the_archive_field_and_can_be_undone() {
        let (mut v, _tmp) = vault_with_one_project();
        let id = v.projects()[0].id;
        let before = v.projects()[0].clone();
        v.set_project_archived(id, true, "2026-09-11T08:00:00Z");
        let after = v.projects()[0].clone();
        assert_eq!(after.archived_at.as_deref(), Some("2026-09-11T08:00:00Z"));
        assert_eq!(
            ProjectRecord { archived_at: None, ..after.clone() },
            before,
            "归档只许动 archived_at 一个字段"
        );
        v.set_project_archived(id, false, "2026-09-11T09:00:00Z");
        assert_eq!(v.projects()[0].archived_at, None, "取消归档要把键清掉");
    }
```

`vault_with_one_project()` 若不存在，照该文件已有的 vault 测试助手（见 `touch_project_accessed`
那两条测试，`vault.rs:3055` / `3080`）同款写法建一个。

- [ ] **Step 5: 跑测试确认它红**

Run: `cargo test -p mullion-store archiving_flips 2>&1 | tail -20`
Expected: 编译失败，没有 `set_project_archived`。

- [ ] **Step 6: 实现**

`crates/mullion-store/src/vault.rs`，紧接 `touch_project_accessed` 之后：

```rust
    /// F257:归档 / 取消归档。`now` 由调用方给(store 不持有时钟)。
    ///
    /// **只改这一个字段,且不跑 `validate`** —— 同 `touch_project_accessed`:
    /// 收起一个项目不该因为库里别处有个撞名的老项目就失败。
    ///
    /// 取消归档传 `false`,把键清掉(而不是写一个空串)—— `skip_serializing_if`
    /// 才能让文件里不留痕迹,否则每个撤销过的项目都会在 TOML 里留一行
    /// `archived_at = ""`,而 `is_none()` 判据会把它当成"还在归档"。
    pub fn set_project_archived(
        &mut self,
        id: crate::project::ProjectId,
        archived: bool,
        now: &str,
    ) {
        self.sync_from_disk_if_untouched();
        if let Some(slot) = self.projects.iter_mut().find(|p| p.id == id) {
            slot.archived_at = archived.then(|| now.to_string());
        }
    }
```

- [ ] **Step 7: 跑测试确认它绿**

Run: `cargo test -p mullion-store archiving_flips 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok.`

- [ ] **Step 8: 提交**

```bash
git add -A
git commit -m "$(cat <<'EOF'
test(store): 锁住「归档不释放项目名/tmux 名」+ 加 set_project_archived (F257)

归档退出校验会让取消归档可能撞车,而撞车放过去 = 两个项目共用一个
tmux。两条回归锁已按注释里的变异自证变红。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: app —— 列表纯函数 `ui/project_list.rs`

这是两个切片的共用地基。**先只做 F257 那一半**（tab 分档 + 空态 + 搜索穿透），
Lit 置顶留给 Task 9。

**Files:**
- Create: `crates/mullion-app/src/ui/project_list.rs`
- Modify: `crates/mullion-app/src/ui/mod.rs`（`pub mod project_list;`）

- [ ] **Step 1: 写失败测试**

新建 `crates/mullion-app/src/ui/project_list.rs`，先只写模块头 + 测试：

```rust
//! F257:三处项目列表(启动页 / 项目管理器左栏 / 切换项目弹窗)共用的
//! **纯函数**:列哪些行、按什么排、空了说什么。零 egui、零 IO,可纯单测。
//!
//! 为什么必须共用:项目里已经为「顺序」写了三条注释反复强调复用
//! `by_recent_access`(理由:同一个搜索词在两个界面给出不同结果,用户几分钟内
//! 就会都看到一遍)。F257 又往里加了「归档要不要列」和「空了说什么」两件同样
//! 会漂的事 —— 三处各写一份 `if`,加第四处列表时必漏。
//!
//! 设计见 `docs/superpowers/plans/2026-09-11-f257-f258-project-archive-and-activity-order.md`。

use mullion_store::{ProjectRecord, SessionRecord};

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_store::{ProjectId, SessionId};

    fn proj(id: u64, name: &str, accessed: Option<&str>, archived: Option<&str>) -> ProjectRecord {
        ProjectRecord {
            id: ProjectId(id),
            name: name.into(),
            note: String::new(),
            nodes: vec![SessionId(7)],
            preferred: None,
            dir: format!("/data/{name}"),
            tmux_name: None,
            created_at: "2026-09-01T00:00:00Z".into(),
            last_accessed_at: accessed.map(str::to_string),
            archived_at: archived.map(str::to_string),
            icon: None,
        }
    }

    /// 浏览态的「在用」tab 只列没归档的。
    #[test]
    fn the_active_tab_lists_only_projects_that_are_not_archived() {
        let ps = vec![
            proj(1, "在用的", Some("2026-09-10T00:00:00Z"), None),
            proj(2, "归档的", Some("2026-09-11T00:00:00Z"), Some("2026-09-11T01:00:00Z")),
        ];
        let got: Vec<u64> = rows(&ps, Tab::Active, "", &[]).iter().map(|p| p.id.0).collect();
        assert_eq!(got, vec![1], "归档的项目不该出现在「在用」里");
    }

    /// 「归档」tab 按**归档时间**倒序,不是按最后访问 —— 刚归错的要在最上面,
    /// 马上能撤。
    ///
    /// 判据故意让两种排法给出**相反**的顺序:`old` 访问得更晚、归档得更早。
    /// 不这么造的话,把 `archived_at` 换成 `last_accessed_at` 也是绿的。
    #[test]
    fn the_archived_tab_sorts_by_when_it_was_archived_not_when_it_was_last_opened() {
        let ps = vec![
            proj(1, "先归的", Some("2026-09-10T00:00:00Z"), Some("2026-09-01T00:00:00Z")),
            proj(2, "后归的", Some("2026-09-02T00:00:00Z"), Some("2026-09-09T00:00:00Z")),
        ];
        let got: Vec<u64> = rows(&ps, Tab::Archived, "", &[]).iter().map(|p| p.id.0).collect();
        assert_eq!(got, vec![2, 1], "归档 tab 要按归档时间倒序");
    }

    /// 搜索穿透两态,且**在用的整体排在归档的前面**(D13)。
    ///
    /// 判据造成:归档那条的访问时间**更新**。混排(不分段)会把它排到前面。
    #[test]
    fn searching_crosses_both_states_and_puts_the_active_ones_first() {
        let ps = vec![
            proj(1, "活 alpha", Some("2026-09-01T00:00:00Z"), None),
            proj(2, "档 alpha", Some("2026-09-30T00:00:00Z"), Some("2026-09-02T00:00:00Z")),
        ];
        for tab in [Tab::Active, Tab::Archived] {
            let got: Vec<u64> = rows(&ps, tab, "alpha", &[]).iter().map(|p| p.id.0).collect();
            assert_eq!(
                got,
                vec![1, 2],
                "搜索要穿透归档,且在用的整体在前(tab={tab:?} 不该影响搜索结果)"
            );
        }
    }

    /// 从没打开过的项目沉底,但**不消失** —— 新建一个项目之后它就在这一档,
    /// 掉了的话用户会以为没建成。
    ///
    /// (这一条与下一条是从 `project_manager::by_recent_access` 的三条单测搬过来的,
    /// Task 10 删那个函数时靠它们保住覆盖。)
    #[test]
    fn a_project_never_opened_sinks_to_the_bottom_but_does_not_disappear() {
        let ps = vec![
            proj(1, "没开过的", None, None),
            proj(2, "开过的", Some("2026-09-01T00:00:00Z"), None),
        ];
        let got: Vec<u64> = rows(&ps, Tab::Active, "", &[]).iter().map(|p| p.id.0).collect();
        assert_eq!(got, vec![2, 1]);
    }

    /// 时间完全一样时按 id 升序 —— **不能靠 `sort_by` 的稳定性**,
    /// 那样顺序就取决于磁盘上 `[[project]]` 的书写次序,用户手改一次配置
    /// 文件列表就重排了。
    ///
    /// 判据故意把入参顺序造成与期望**相反**,靠稳定性的实现会红。
    #[test]
    fn projects_with_the_same_timestamp_fall_back_to_id_not_to_file_order() {
        let ps = vec![
            proj(9, "后写的", Some("2026-09-01T00:00:00Z"), None),
            proj(2, "先写的", Some("2026-09-01T00:00:00Z"), None),
        ];
        let got: Vec<u64> = rows(&ps, Tab::Active, "", &[]).iter().map(|p| p.id.0).collect();
        assert_eq!(got, vec![2, 9]);
    }

    /// 有项目、但全归档了 —— 三处的空态**不能**再喊「还没有项目,去建一个」,
    /// 用户会真的去建一个重复的。
    #[test]
    fn an_all_archived_library_does_not_claim_there_are_no_projects() {
        let ps = vec![proj(1, "老活", None, Some("2026-09-01T00:00:00Z"))];
        assert_eq!(
            empty_reason(&ps, Tab::Active, "", &[]),
            Some(EmptyReason::AllArchived(1))
        );
        let text = empty_text(EmptyReason::AllArchived(1), Surface::Launcher);
        assert!(text.contains("没有在用的项目"), "{text}");
        assert!(text.contains('1'), "要报出归档里还有几个:{text}");
        assert!(!text.contains("还没有项目"), "不许说没有项目:{text}");
    }

    /// 一个项目都没有时,原来那句话原样保留。
    #[test]
    fn a_truly_empty_library_keeps_the_original_wording() {
        assert_eq!(empty_reason(&[], Tab::Active, "", &[]), Some(EmptyReason::NoProjects));
    }

    /// 搜索没匹配是**第三档**,不能被上面两档吃掉 —— 库里全归档、又搜了个
    /// 搜不到的词时,该说「没有匹配的项目」,不是「归档里还有 N 个」。
    #[test]
    fn a_search_with_no_hits_is_its_own_case_even_when_everything_is_archived() {
        let ps = vec![proj(1, "老活", None, Some("2026-09-01T00:00:00Z"))];
        assert_eq!(
            empty_reason(&ps, Tab::Active, "找不到的词", &[]),
            Some(EmptyReason::NoMatch)
        );
    }

    /// 归档 tab 空了是第四档,不能复用「还没有项目」。
    #[test]
    fn an_empty_archive_tab_says_so_instead_of_claiming_there_are_no_projects() {
        let ps = vec![proj(1, "在用的", None, None)];
        assert_eq!(empty_reason(&ps, Tab::Archived, "", &[]), Some(EmptyReason::NoArchived));
    }

    /// 有行可列时不能返回空态 —— 反了的话列表和提示会同时出现。
    #[test]
    fn a_non_empty_list_has_no_empty_reason() {
        let ps = vec![proj(1, "在用的", None, None)];
        assert_eq!(empty_reason(&ps, Tab::Active, "", &[]), None);
    }
}
```

- [ ] **Step 2: 跑测试确认它红**

Run: `cargo test -p mullion-app project_list 2>&1 | tail -20`
Expected: 编译失败，`rows` / `Tab` / `EmptyReason` / `empty_text` / `Surface` 都不存在。

- [ ] **Step 3: 实现**

在 `crates/mullion-app/src/ui/project_list.rs` 的模块头与 `mod tests` 之间插入：

```rust
/// 项目管理器上那两个 tab。**只有项目管理器有 tab**(设计 D3):启动页与
/// 切换项目弹窗恒传 [`Tab::Active`] —— 那两处是「干活入口」,归档项目默认
/// 不该在那儿碍事,但**搜得到**(见 [`rows`] 的搜索态)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Active,
    Archived,
}

impl Tab {
    /// tab 上的字。**「在用」不叫「活跃」**:F258 把「最后活跃」定成了排序
    /// 判据,同一个词在同一个界面指两件事。也不叫「全部」(它不含归档的)、
    /// 不叫「运行中」(那是 F224 的灯,一个项目可以在用但现在没开)。
    pub fn label(self) -> &'static str {
        match self {
            Tab::Active => "在用",
            Tab::Archived => "归档",
        }
    }
}

/// 哪个界面在问。只用来挑空态文案的措辞 —— 项目管理器里 tab 就在眼前,
/// 不用指路;另外两处得告诉用户去哪儿撤。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Manager,
    Launcher,
    Pick,
}

/// 列表为什么是空的。**四档穷尽**,不是一个 `bool`。
///
/// 为什么非要枚举:原来三处各写一句 `if projects.is_empty()`,而归档一上来
/// `projects.is_empty()` 仍是 `false`、列表却是空的 —— 三处会照旧喊
/// 「还没有项目,去建一个」,用户会真的去建一个重复的。加档时漏一处的症状是
/// 一句**说错的话**,编译器不会管。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmptyReason {
    /// 库里一个项目都没有。
    NoProjects,
    /// 有项目,但全在归档里。带上归档里有几个。
    AllArchived(usize),
    /// 搜索词没有匹配。
    NoMatch,
    /// 归档 tab 是空的(还没归档过任何项目)。
    NoArchived,
}

/// 这一帧该列哪些项目,已排好序。
///
/// **搜索态跨两态**(设计 D4/D16):`query` 非空时忽略 `tab`,在用的整体排在
/// 归档的前面,段内各按自己的判据。清空搜索才回到 `tab` 说了算。
///
/// 段内判据(设计 D13):
/// - 在用:`last_accessed_at` 倒序 → id 升序兜底(F258 会在这里插入 Lit 置顶)
/// - 归档:`archived_at` 倒序 → id 升序兜底
///
/// 时间是 RFC3339 字符串,同一时区下**字典序即时间序**(同 `by_recent_access`
/// 的既有取舍:跨时区搬配置目录会排错位置,后果有上限,不值得引日期解析库)。
///
/// id 兜底**不能靠 `sort_by` 的稳定性**:那样顺序就取决于入参顺序,而入参顺序
/// 来自磁盘上 `[[project]]` 的书写次序,用户手改一次配置文件列表就重排了。
pub fn rows<'a>(
    projects: &'a [ProjectRecord],
    tab: Tab,
    query: &str,
    sessions: &[SessionRecord],
) -> Vec<&'a ProjectRecord> {
    let searching = !query.trim().is_empty();
    let mut out: Vec<&ProjectRecord> = projects
        .iter()
        .filter(|p| searching || in_tab(p, tab))
        .filter(|p| crate::project::matches(p, query, sessions))
        .collect();
    out.sort_by(|a, b| {
        // 搜索态:在用的整段在前。非搜索态两边同档,这一比恒 Equal。
        archived(a)
            .cmp(&archived(b))
            .then_with(|| segment_order(a, b))
    });
    out
}

/// 这个项目属不属于这个 tab。
fn in_tab(p: &ProjectRecord, tab: Tab) -> bool {
    match tab {
        Tab::Active => p.archived_at.is_none(),
        Tab::Archived => p.archived_at.is_some(),
    }
}

/// 排序用的"归不归档"。`false` < `true`,所以在用的自然排前面。
fn archived(p: &ProjectRecord) -> bool {
    p.archived_at.is_some()
}

/// 段内顺序。归档的按归档时间倒序,在用的按最后访问倒序;都以 id 升序兜底。
///
/// 两个不同档的记录走到这里时(搜索态下不可能,上面那一比已经分开了)按
/// 在用那套算,结果无害。
fn segment_order(a: &ProjectRecord, b: &ProjectRecord) -> std::cmp::Ordering {
    let key = |p: &ProjectRecord| -> Option<String> {
        if archived(p) {
            p.archived_at.clone()
        } else {
            p.last_accessed_at.clone()
        }
    };
    newest_first(&key(a), &key(b)).then(a.id.0.cmp(&b.id.0))
}

/// 有时间戳的排在没有的前面;都有就倒序;都没有算平手(交给 id 兜底)。
///
/// 抽出来是因为 F258 的 Lit 置顶要在它外面再套一层,而"没时间戳的沉底"这条
/// 规则两处都要,写两份必漂。
fn newest_first(a: &Option<String>, b: &Option<String>) -> std::cmp::Ordering {
    match (a, b) {
        (Some(x), Some(y)) => y.cmp(x),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

/// 列表空了是为什么。`None` = 没空,别画提示。
///
/// **判据顺序有意义**:搜索没匹配要排在"全归档"前面 —— 库里全归档、又搜了个
/// 搜不到的词时,该说「没有匹配的项目」而不是「归档里还有 N 个」(后者答非所问)。
pub fn empty_reason(
    projects: &[ProjectRecord],
    tab: Tab,
    query: &str,
    sessions: &[SessionRecord],
) -> Option<EmptyReason> {
    if !rows(projects, tab, query, sessions).is_empty() {
        return None;
    }
    if projects.is_empty() {
        return Some(EmptyReason::NoProjects);
    }
    if !query.trim().is_empty() {
        return Some(EmptyReason::NoMatch);
    }
    match tab {
        Tab::Archived => Some(EmptyReason::NoArchived),
        Tab::Active => Some(EmptyReason::AllArchived(
            projects.iter().filter(|p| archived(p)).count(),
        )),
    }
}

/// 空态那句话。
///
/// 「归档里还有 N 个」要**报数**:不报的话用户不知道那边是不是也空的,还得
/// 切过去看一眼。
pub fn empty_text(reason: EmptyReason, surface: Surface) -> String {
    match (reason, surface) {
        (EmptyReason::NoProjects, Surface::Launcher) => "还没有项目。一个项目 = 一台机器上的一个目录 + 一个专属 tmux 会话;\
             从菜单「会话 → 项目管理器」建一个,以后开机点一下就回到现场。"
            .to_string(),
        (EmptyReason::NoProjects, Surface::Manager) => {
            "还没有项目。项目 = 一台机器上的一个开发目录 + 一个专属 tmux 会话,打开它就回到那个活。"
                .to_string()
        }
        (EmptyReason::NoProjects, Surface::Pick) => {
            "还没有项目。从「会话 → 项目管理器」建一个。".to_string()
        }
        (EmptyReason::AllArchived(n), Surface::Manager) => {
            format!("没有在用的项目。归档里还有 {n} 个。")
        }
        (EmptyReason::AllArchived(n), _) => {
            format!("没有在用的项目。归档里还有 {n} 个 —— 到「会话 → 项目管理器 → 归档」取消归档。")
        }
        (EmptyReason::NoMatch, _) => "没有匹配的项目".to_string(),
        (EmptyReason::NoArchived, _) => {
            "还没有归档任何项目。归档 = 收起不再做的活,随时能取消。".to_string()
        }
    }
}
```

在 `crates/mullion-app/src/ui/mod.rs` 的模块声明处加：

```rust
pub mod project_list;
```

- [ ] **Step 4: 跑测试确认它绿**

Run: `cargo test -p mullion-app project_list 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok.` 10 passed。

- [ ] **Step 5: 过一遍字形白名单（T9）**

新加了一批 UI 字符串，全是汉字 + ASCII，但必须机械确认：

Run: `cargo test -p mullion-app --test glyph_whitelist 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok.`

- [ ] **Step 6: 提交**

```bash
git add -A
git commit -m "$(cat <<'EOF'
feat(app): 三处项目列表的「列什么/怎么排/空了说什么」收进纯函数 (F257)

ui/project_list.rs:Tab / rows / EmptyReason 四档 / empty_text。
搜索态跨两态且在用的整段在前;归档 tab 按归档时间倒序(不是最后访问,
两者在测试里被造成相反顺序,换判据会红)。

空态改四档枚举而不是三处各写 if:全归档时 projects.is_empty() 仍是
false,原来那句「还没有项目,去建一个」会让用户真的建一个重复的。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: app —— `ProjectIntent::SetArchived` + 右栏按钮

**Files:**
- Modify: `crates/mullion-app/src/ui/project_manager.rs`（`ProjectIntent` 枚举 + 底部按钮行）
- Modify: `crates/mullion-app/src/app.rs:13413` 附近（intent 消费的 `match`）

- [ ] **Step 1: 写失败测试**

`crates/mullion-app/src/ui/project_manager.rs` 的 `mod tests`：

```rust
    /// D7:归档按钮摆在「删除项目」**左边** —— 危险程度从右往左递减,
    /// 同 `pane_title` 那三个按钮的既有规矩(× 最右)。
    ///
    /// 量的是**画出来的横坐标**,不是源码里的书写顺序:`ui.horizontal` 里
    /// 换个位置写、但用 `right_to_left` 布局的话,源码顺序会骗人。
    ///
    /// 自证会变红:把「归档」那个 `if ui.button(..)` 挪到「删除项目」后面。
    #[test]
    fn the_archive_button_sits_to_the_left_of_delete() {
        let (archive, delete) = archive_and_delete_button_rects();
        assert!(
            archive.right() <= delete.left(),
            "归档按钮({archive:?})必须整个在删除按钮({delete:?})左边"
        );
    }

    /// 归档态决定按钮上的字。写死「归档」的话,已归档的项目右栏会给出一个
    /// 点了没有任何变化的按钮 —— 而归档是可逆的,撤销入口就是这一个。
    #[test]
    fn the_button_says_undo_when_the_project_is_already_archived() {
        assert_eq!(archive_button_label(false), "归档");
        assert_eq!(archive_button_label(true), "取消归档");
    }
```

`archive_and_delete_button_rects()` 用本文件既有的 egui 测试 harness 写（参照
`project_manager.rs:1245` 那条「删除项目那一行仍然落在屏幕里」的测试，它已经在跑一个
真实 `egui::Context` 并按文本找按钮矩形，照它的写法找两个文本、返回两个 `Rect`）。

- [ ] **Step 2: 跑测试确认它红**

Run: `cargo test -p mullion-app archive_button 2>&1 | tail -20`
Expected: 编译失败 / 找不到「归档」按钮。

- [ ] **Step 3: 加 intent 变体**

`crates/mullion-app/src/ui/project_manager.rs`：

```rust
pub enum ProjectIntent {
    /// 新建:只给名字,其余字段进弹窗再填。
    Add(String),
    /// 整份保存。`ProjectRecord.id` 由 `app` 侧以第一个参数为准。
    Save(ProjectId, Box<ProjectRecord>),
    Delete(ProjectId),
    /// F257:归档 / 取消归档。`bool` = 归档后的目标状态。
    ///
    /// **单独一条 intent,不走 `Save`**:`Save` 会跑 `validate_project`,而归档
    /// 一个"名字跟别人撞了、右栏保存正灰着"的项目必须成立 —— 收起一个项目
    /// 不该被别处的校验问题挡住(同 `touch_project_accessed` 的姿态)。
    SetArchived(ProjectId, bool),
}
```

- [ ] **Step 4: 加按钮**

`crates/mullion-app/src/ui/project_manager.rs` 底部按钮行。先在 `save_draft` 附近加：

```rust
    // F257:按钮上的字取**库里那份**的归档态,不是草稿 —— 草稿里的
    // `archived_at` 永远不会被右栏任何一个控件改到(设计 D6:只由这个按钮写),
    // 但草稿是「保存前的编辑缓冲」,拿它当真值会在保存失败时给出过期的字。
    let is_archived = stored.is_some_and(|p| p.archived_at.is_some());
```

在 `ui.horizontal(|ui| { ... })` 里，把「删除项目」那一段改成：

```rust
                ui.add_space(SP_S);
                // F257:归档摆在「删除项目」**左边** —— 危险程度从右往左递减,
                // 同 `pane_title` 三个按钮的既有规矩。**不上确认框**:归档可逆、
                // 撤销就在同一个按钮上,而"高频路径上的无谓确认会被用户练成
                // 闭眼点确定,那时它对真正危险的几种也失效"(同本文件
                // `show_open_confirm` 的原话)。
                if ui.button(archive_button_label(is_archived)).clicked() {
                    ui_state.project_intent = Some(ProjectIntent::SetArchived(id, !is_archived));
                }
                ui.add_space(SP_S);
                if ui.button("删除项目").clicked() {
                    ui_state.project_intent = Some(ProjectIntent::Delete(id));
                }
```

并在文件里加：

```rust
/// F257:归档按钮上的字。抽成函数只为了能单测 —— 写死「归档」的话,已归档的
/// 项目右栏会给出一个点了看不出变化的按钮,而这是撤销归档的唯一入口。
pub(crate) fn archive_button_label(archived: bool) -> &'static str {
    if archived {
        "取消归档"
    } else {
        "归档"
    }
}
```

- [ ] **Step 5: 加消费分支**

`crates/mullion-app/src/app.rs`，在 `ProjectIntent::Delete(id) => { ... }` 之后追加：

```rust
                            // F257:归档 / 取消归档。**不跑 validate**(见
                            // `ProjectIntent::SetArchived` 的文档),所以不会
                            // 置 `ok = false`,照常落盘。
                            crate::ui::project_manager::ProjectIntent::SetArchived(
                                id,
                                archived,
                            ) => {
                                let now = time::OffsetDateTime::now_utc()
                                    .format(&time::format_description::well_known::Rfc3339)
                                    .unwrap_or_default();
                                store.set_project_archived(id, archived, &now);
                                // 右栏那份草稿也要跟上 —— 不跟的话
                                // `stored != Some(&*draft)` 恒真,「打开」按钮
                                // 会永久灰着且解释是"有未保存的改动",而用户
                                // 什么都没改。
                                self.ui.project_draft =
                                    store.projects().iter().find(|p| p.id == id).cloned();
                            }
```

- [ ] **Step 6: 跑测试确认它绿**

Run: `cargo test -p mullion-app archive_button 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok.`

- [ ] **Step 7: 跑自证变异**

先确认工作区干净。把「归档」那个 `if` 整段挪到「删除项目」之后，跑：

Run: `cargo test -p mullion-app the_archive_button_sits 2>&1 | grep -E "test result|FAILED"`
Expected: **FAILED**。确认后 `git checkout crates/mullion-app/src/ui/project_manager.rs`。

- [ ] **Step 8: 提交**

```bash
git add -A
git commit -m "$(cat <<'EOF'
feat(app): 项目管理器右栏加「归档 / 取消归档」(F257)

单独一条 ProjectIntent 而不是走 Save:Save 跑 validate,而归档一个
「名字撞了、保存正灰着」的项目必须成立。摆在「删除项目」左边(危险度
从右往左递减),按钮位置有守护测试并已自证变红。

不上确认框:归档可逆、撤销就在同一个按钮上。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: ui —— 项目管理器 tab 栏（搜索态收起）

**Files:**
- Modify: `crates/mullion-app/src/ui/mod.rs`（`UiState` 加 `project_tab`）
- Modify: `crates/mullion-app/src/ui/project_manager.rs`（左栏）

- [ ] **Step 1: 写失败测试**

`crates/mullion-app/src/ui/project_manager.rs` 的 `mod tests`：

```rust
    /// D4:**搜索态没有 tab**。搜索框一非空,tab 栏整条收起 —— 留着的话两个
    /// tab 会显示完全相同的结果(搜索穿透两态),切过去画面不变,用户会以为
    /// 点坏了。
    ///
    /// 自证会变红:把 `if ui_state.project_search.trim().is_empty()` 那道闸删掉。
    #[test]
    fn the_tab_bar_disappears_while_searching_because_both_tabs_would_look_identical() {
        assert!(tab_bar_visible(""), "不搜索时 tab 栏要在");
        assert!(!tab_bar_visible("alpha"), "搜索时 tab 栏必须收起");
    }
```

`tab_bar_visible(query: &str) -> bool` 用本文件既有 harness 跑一帧、按文本「归档」在左栏区域
内找 tab 按钮是否存在（注意与右栏那个「归档」动作按钮区分：右栏按钮只在选中了项目时才画，
harness 里不选中任何项目即可，两者不会同时出现）。

- [ ] **Step 2: 跑测试确认它红**

Run: `cargo test -p mullion-app the_tab_bar_disappears 2>&1 | tail -20`
Expected: 编译失败 / 两个断言都不成立。

- [ ] **Step 3: 加 UiState 字段**

`crates/mullion-app/src/ui/mod.rs`，在 `project_search` 旁边：

```rust
    /// F257:项目管理器左栏当前在哪个 tab。**只有项目管理器有 tab**(设计 D3)。
    ///
    /// 搜索态下这个值仍然保留(不清零)—— 清空搜索要能回到你原来看的那一档。
    pub project_tab: crate::ui::project_list::Tab,
```

`UiState` 是 `#[derive(Default)]` 的，所以给 `Tab` 加：

```rust
impl Default for Tab {
    /// 默认「在用」—— 打开项目管理器时该看见还在做的活。
    fn default() -> Self {
        Tab::Active
    }
}
```

（写在 `project_list.rs` 里，紧跟 `Tab` 定义。）

- [ ] **Step 4: 画 tab 栏**

`crates/mullion-app/src/ui/project_manager.rs` 左栏，在搜索框与列表之间（`ui.add_space(SP_S);`
之后）插入：

```rust
        // F257:tab 栏。**搜索态整条收起**(设计 D4):搜索穿透两态,留着 tab
        // 的话两个 tab 显示完全相同的结果,切过去画面不变 —— 那是那种
        // 「编译过、跑起来看着像坏了」的 UI。
        //
        // 收起会让下面的列表上移约一行,布局跳一下 —— 接受:它由用户自己
        // 打字触发,是有因果的位移。灰着一排点不动的 tab 反而更像 bug
        // (同 `rehost.rs` 里那条「灰着一条永远点不动的项」的既有判断)。
        if ui_state.project_search.trim().is_empty() {
            ui.horizontal(|ui| {
                for tab in [
                    crate::ui::project_list::Tab::Active,
                    crate::ui::project_list::Tab::Archived,
                ] {
                    let on = ui_state.project_tab == tab;
                    if ui.selectable_label(on, tab.label()).clicked() {
                        ui_state.project_tab = tab;
                    }
                }
            });
            ui.add_space(SP_S);
        }
```

- [ ] **Step 5: 左栏改调 `project_list`**

把左栏那段 `let rows: Vec<&ProjectRecord> = by_recent_access(projects) ... ` 连同上面那个
`if projects.is_empty()` 一起换成：

```rust
        // F257:列什么 / 怎么排 / 空了说什么,一律走 `project_list` ——
        // 三处列表共用同一份判据(设计 D8)。
        let tab = ui_state.project_tab;
        let rows = crate::ui::project_list::rows(projects, tab, &ui_state.project_search, sessions);
        if let Some(reason) =
            crate::ui::project_list::empty_reason(projects, tab, &ui_state.project_search, sessions)
        {
            ui.label(
                egui::RichText::new(crate::ui::project_list::empty_text(
                    reason,
                    crate::ui::project_list::Surface::Manager,
                ))
                .color(crate::theme::c32(t.fg_muted)),
            );
            if reason == crate::ui::project_list::EmptyReason::NoMatch {
                ui.add_space(SP_S);
                if ui.button("清空搜索").clicked() {
                    ui_state.project_search.clear();
                }
            }
            return;
        }
```

> 注意：原来那个 `if projects.is_empty() { ...; return; }` 提前 return 会**跳过底部的
> 「+ 添加项目」按钮**吗？不会——那个按钮画在 `TopBottomPanel::bottom` 里，已经在这段之前
> `show_inside` 过了。保持原有顺序即可。

- [ ] **Step 6: 跑测试确认它绿**

Run: `cargo test -p mullion-app -- project_manager 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok.`

- [ ] **Step 7: 跑自证变异**

删掉 `if ui_state.project_search.trim().is_empty()` 那道闸（改成恒画），跑：

Run: `cargo test -p mullion-app the_tab_bar_disappears 2>&1 | grep -E "test result|FAILED"`
Expected: **FAILED**。确认后 `git checkout`。

- [ ] **Step 8: 提交**

```bash
git add -A
git commit -m "$(cat <<'EOF'
feat(app): 项目管理器左栏分「在用 / 归档」两 tab (F257)

搜索态整条收起 tab 栏 —— 搜索穿透两态,留着的话两个 tab 结果一模一样,
切过去画面不变。守护已自证变红。

左栏的「列什么/怎么排/空了说什么」改调 ui::project_list,不再自己拼
by_recent_access + projects.is_empty()。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: ui —— 启动页 / 切换项目弹窗接同一套

**Files:**
- Modify: `crates/mullion-app/src/ui/launcher.rs:70-90`
- Modify: `crates/mullion-app/src/ui/project_pick.rs:120-140`

- [ ] **Step 1: 写失败测试**

`crates/mullion-app/src/ui/launcher.rs` 的 `mod tests`：

```rust
    /// 启动页默认只列「在用」的 —— 归档项目在干活入口里不该碍事(设计 D3)。
    /// 但**搜得到**:搜索穿透归档(设计 D4)。
    ///
    /// 自证会变红:把 `Tab::Active` 换成把 `projects` 直接喂给 `project_row`。
    #[test]
    fn the_launcher_hides_archived_projects_until_you_search_for_them() {
        let ps = vec![
            proj_archived(1, "老活", "/data/old"),
            proj(2, "在做的", "/data/now", None),
        ];
        assert_eq!(names_drawn(&ps, ""), vec!["在做的"], "归档的不该默认出现");
        assert_eq!(names_drawn(&ps, "老活"), vec!["老活"], "搜索必须能搜到归档的");
    }

    /// 库里有项目、只是全归档了 —— 启动页不能喊「还没有项目,去建一个」。
    #[test]
    fn a_launcher_with_only_archived_projects_does_not_tell_you_to_create_one() {
        let ps = vec![proj_archived(1, "老活", "/data/old")];
        let text = drawn_text(&ps, "");
        assert!(text.contains("没有在用的项目"), "{text}");
        assert!(!text.contains("还没有项目"), "{text}");
    }
```

`names_drawn` / `drawn_text` 照本文件既有的 `launcher::show` egui harness 写
（`ui/mod.rs:2518` 那条测试已经在跑同款 harness 并取回画出来的文本，照它写）。
`proj_archived` = `proj(...)` 再把 `archived_at` 置上。

`crates/mullion-app/src/ui/project_pick.rs` 的 `mod tests` 加一条对称的：

```rust
    /// 切换项目弹窗同上:默认只列在用的,搜索穿透归档。
    #[test]
    fn the_pick_popup_hides_archived_projects_until_you_search_for_them() {
        let ps = vec![
            proj_archived(1, "老活", "/data/old"),
            proj(2, "在做的", "/data/now", None),
        ];
        assert_eq!(names_drawn(&ps, ""), vec!["在做的"]);
        assert_eq!(names_drawn(&ps, "老活"), vec!["老活"]);
    }
```

- [ ] **Step 2: 跑测试确认它红**

Run: `cargo test -p mullion-app hides_archived_projects 2>&1 | grep -E "test result|FAILED"`
Expected: **FAILED**（归档的项目现在照样出现）。

- [ ] **Step 3: 改 launcher**

把 `crates/mullion-app/src/ui/launcher.rs` 里 `if projects.is_empty() { ... return; }` 到
`if rows.is_empty() { ... return; }` 那一整段换成：

```rust
            // F257:列什么 / 空了说什么,与另外两处列表共用 `project_list`。
            // 启动页恒传 `Tab::Active` —— 这里是干活入口,没有 tab(设计 D3);
            // 但搜索穿透归档,搜得到。
            let rows = crate::ui::project_list::rows(
                projects,
                crate::ui::project_list::Tab::Active,
                &ui_state.launcher_search,
                sessions,
            );
            if let Some(reason) = crate::ui::project_list::empty_reason(
                projects,
                crate::ui::project_list::Tab::Active,
                &ui_state.launcher_search,
                sessions,
            ) {
                ui.vertical_centered(|ui| {
                    // `hint_text(t, s: impl Into<String>)` —— 传 `String` 本身,
                    // **不要**传 `&String`:泛型参数上不发生 deref coercion,
                    // `&String` 不实现 `Into<String>`,那样编译不过。
                    ui.label(crate::theme::hint_text(
                        t,
                        crate::ui::project_list::empty_text(
                            reason,
                            crate::ui::project_list::Surface::Launcher,
                        ),
                    ));
                    if reason == crate::ui::project_list::EmptyReason::NoMatch {
                        ui.add_space(SP_S);
                        if ui.button("清空搜索").clicked() {
                            ui_state.launcher_search.clear();
                        }
                    }
                });
                return;
            }
```

> 注意原来那个 `if projects.is_empty()` 分支排在**搜索框之前**（空库时连搜索框都不画）。
> 新写法排在搜索框**之后**，于是空库时也会画一个搜索框。这是**有意的改动**：全归档时
> 用户需要那个搜索框去把归档项目搜出来。真·空库时多一个搜索框无害。

`crate::theme::hint_text` 若只接 `&str`，`empty_text` 返回 `String`，取引用即可（已按此写）。

- [ ] **Step 4: 改 project_pick**

`crates/mullion-app/src/ui/project_pick.rs`，把 `let rows: Vec<_> = ...by_recent_access...`
连同 `if rows.is_empty() { ... }` 换成：

```rust
                    // F257:同 launcher —— 恒 `Tab::Active`,搜索穿透归档。
                    let rows = crate::ui::project_list::rows(
                        projects,
                        crate::ui::project_list::Tab::Active,
                        &d.filter,
                        sessions,
                    );
                    if let Some(reason) = crate::ui::project_list::empty_reason(
                        projects,
                        crate::ui::project_list::Tab::Active,
                        &d.filter,
                        sessions,
                    ) {
                        ui.label(
                            egui::RichText::new(crate::ui::project_list::empty_text(
                                reason,
                                crate::ui::project_list::Surface::Pick,
                            ))
                            .color(theme::c32(t.fg_muted)),
                        );
                    }
```

- [ ] **Step 5: 跑测试确认它绿**

Run: `cargo test -p mullion-app 2>&1 > /tmp/t.log; grep -nE "test result|FAILED|panicked" /tmp/t.log | head -20`
Expected: 全绿。

- [ ] **Step 6: 提交**

```bash
git add -A
git commit -m "$(cat <<'EOF'
feat(app): 启动页与切换项目弹窗默认只列在用项目,搜索穿透归档 (F257)

两处一起改调 ui::project_list,空态换成四档枚举 —— 原来「全归档」时
两处都会喊「还没有项目,去建一个」,用户会真的建一个重复的。

启动页的空态提前 return 从「搜索框之前」挪到「之后」:全归档时用户
需要那个搜索框才能把归档项目搜出来。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: ui —— 行上的「归档」标记

搜索穿透之后，结果里会混着两种态。不标的话用户点开一个归档项目却不知道它是归档的。

**Files:**
- Modify: `crates/mullion-app/src/ui/project_row.rs`

- [ ] **Step 1: 写失败测试**

`crates/mullion-app/src/ui/project_row.rs` 的 `mod tests`：

```rust
    /// F257:归档项目的时间列改写「已归档 · <相对时间>」。
    ///
    /// 搜索穿透两态之后,结果里会混着在用的和归档的 —— 不标的话用户点开一个
    /// 归档项目却不知道它是归档的,而"为什么它在列表里"这个问题没人回答。
    ///
    /// 复用**时间列**而不是新加一列:行宽是弹窗里最紧张的资源(pane 宽度减去
    /// 内边距),新加一列会把名字挤掉一截;而归档项目的"最后打开时间"本来就是
    /// 这一行上最没用的信息。
    ///
    /// 自证会变红:把 `time_text` 里的归档分支删掉。
    #[test]
    fn an_archived_project_says_so_in_the_time_column() {
        let now = time::OffsetDateTime::parse(
            "2026-09-11T12:00:00Z",
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap();
        let mut p = proj(1, "老活", "/data/old", Some("2026-09-01T00:00:00Z"));
        assert!(
            !time_text(&p, now).contains("归档"),
            "在用的项目不该说归档"
        );
        p.archived_at = Some("2026-09-10T12:00:00Z".into());
        let s = time_text(&p, now);
        assert!(s.starts_with("已归档"), "归档态要一眼看得见:{s}");
    }

    /// 归档态的时间取的是**归档时间**,不是最后打开时间 —— 两者在这条判据里
    /// 被造成不同的相对档位,取错会红。
    #[test]
    fn the_archived_row_shows_when_it_was_archived_not_when_it_was_opened() {
        let now = time::OffsetDateTime::parse(
            "2026-09-11T12:00:00Z",
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap();
        let mut p = proj(1, "老活", "/data/old", Some("2026-01-01T00:00:00Z"));
        p.archived_at = Some("2026-09-11T11:00:00Z".into());
        let s = time_text(&p, now);
        let opened = crate::localtime::relative(
            "2026-01-01T00:00:00Z",
            now,
            crate::localtime::offset(),
        );
        assert!(!s.contains(&opened), "取成了最后打开时间:{s}");
    }
```

- [ ] **Step 2: 跑测试确认它红**

Run: `cargo test -p mullion-app archived_project_says_so 2>&1 | grep -E "test result|FAILED"`
Expected: **FAILED**。

- [ ] **Step 3: 改 `time_text`**

`crates/mullion-app/src/ui/project_row.rs`：

```rust
pub fn time_text(p: &ProjectRecord, now: time::OffsetDateTime) -> String {
    // F257:归档态优先。搜索穿透两态之后结果里会混着两种行,不标的话用户点开
    // 一个归档项目却不知道它是归档的。
    //
    // **占用时间列而不是新加一列**:行宽在弹窗里是最紧张的资源(pane 宽度减
    // 内边距),新加一列会把名字挤掉一截;而归档项目的"最后打开时间"本来就是
    // 这一行上最没用的信息。
    if let Some(at) = p.archived_at.as_deref() {
        return format!(
            "已归档 · {}",
            crate::localtime::relative(at, now, crate::localtime::offset())
        );
    }
    match p.last_accessed_at.as_deref() {
        Some(s) => crate::localtime::relative(s, now, crate::localtime::offset()),
        None => "从未打开".to_string(),
    }
}
```

- [ ] **Step 4: 跑测试确认它绿**

Run: `cargo test -p mullion-app -- project_row 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok.`

- [ ] **Step 5: 时间列宽度回归**

> **执行时的更正（`24abefb`）**：这一步的「若宽度断言变红就降级」是个**测不着的闸** ——
> 库里根本没有对时间列/名字截断的数值断言，按字面走等于永远不会触发，而回归是真的。
> 复核用真实 `egui::Context::run` 铺排量出来：`"3 天前"` 30px、`"已归档 · 3 天前"` **70px**、
> `"已归档"` 33px；而名字可用宽是 `(text_avail - time_w - NAME_TIME_GAP)`，时间列从名字预算里扣。
> 窄行宽（row_w=200，即 pane 里的「切换项目」弹窗，宽度随分屏浮动）下 6 字名字可见字数
> 由 6 掉到 4。**已按下面预备的方案降级**：时间列短式「已归档」，完整信息（归档相对时间 +
> 最后打开时间）挪进 hover（`archived_hover_text`，挂在 time_rect 上，避开灯自己的 tooltip）。
> 并补了一条窄行宽下的名字可见字数守护（改回长式会变红，已自证）。
>
> 教训：**「等某条断言变红再降级」只有在那条断言存在时才是判据**，否则它只是一句没人执行的注释。

时间列文字变长了（「已归档 · 3 天前」比「3 天前」宽一截），可能把名字挤掉。
`project_row.rs` 里有 `NAME_TIME_GAP` 与名字截断逻辑，跑一遍那批既有测试：

Run: `cargo test -p mullion-app -- project_row 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok.` 若有关于宽度的断言变红，**不许放宽断言** ——
改成归档时时间列文本用短式（`已归档`，不带相对时间），并把相对时间挪进 hover 提示。

- [ ] **Step 6: 字形白名单**

Run: `cargo test -p mullion-app --test glyph_whitelist 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok.`（`·` 这个符号库里已在用，`project_row::subtitle` 就用它。）

- [ ] **Step 7: 提交**

```bash
git add -A
git commit -m "$(cat <<'EOF'
fix(app): 归档项目在行上标出来,时间列改「已归档 · 相对时间」(F257)

搜索穿透两态之后结果里混着两种行,不标的话用户点开一个归档项目却
不知道它是归档的。占时间列而不是新加一列:行宽在 pane 弹窗里最紧张,
而归档项目的「最后打开时间」本来就是这行上最没用的信息。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

# 第二部分 · F258 最后活跃排序

## Task 8: store —— `SessionRecord.last_connected_at` + `touch_session_connected`

**Files:**
- Modify: `crates/mullion-store/src/model.rs`（`SessionRecord`）
- Modify: `crates/mullion-store/src/vault.rs`（`touch_session_connected`）
- Modify: 各处 `SessionRecord` 构造字面量（按编译错误逐处补 `last_connected_at: None,`）

- [ ] **Step 1: 写失败测试**

`crates/mullion-store/src/model.rs` 的 `mod tests`：

```rust
    /// F258:从没连上过的会话不该往 TOML 里写空键;连上过要能读回来。
    ///
    /// **自证方式**(Task 1 实测踩过的坑):锁定的 `toml 0.8.23` 对结构体里的
    /// `Option::None` **本来就自动省略**,所以「删掉 `skip_serializing_if`」
    /// 这条变异**不会**让它变红。有效的变异是加 `#[serde(skip)]` ——
    /// 回读变 `None`,跟写入的 `Some` 对不上。跑变异时用这一条。
    ///
    /// (`skip_serializing_if` 仍然要写:同文件其它可选字段全是这个写法,
    /// 一致性优先于「去掉当前冗余的属性」。)
    #[test]
    fn last_connected_at_round_trips_and_stays_out_of_the_file_when_unset() {
        let mut rec = sample_record();
        assert!(rec.last_connected_at.is_none());
        let s = toml::to_string_pretty(&rec).unwrap();
        assert!(!s.contains("last_connected_at"), "从没连过不该写出这个键: {s}");
        rec.last_connected_at = Some("2026-09-11T08:00:00Z".into());
        let s = toml::to_string_pretty(&rec).unwrap();
        let back: SessionRecord = toml::from_str(&s).unwrap();
        assert_eq!(back, rec);
    }
```

`sample_record()` 用本文件 `model.rs:274` 那条既有测试里的构造式抽出来。

`crates/mullion-store/src/vault.rs` 的 `mod tests`：

```rust
    /// F258:记一笔「连上了」只改这一个字段,不跑任何校验 —— 同
    /// `touch_project_accessed`:记一笔访问不该因为库里别处有问题就失败。
    #[test]
    fn touching_a_session_connection_flips_only_that_one_field() {
        let (mut v, _tmp) = vault_with_one_session();
        let id = v.list()[0].id;
        let before = v.list()[0].clone();
        v.touch_session_connected(id, "2026-09-11T08:00:00Z");
        let after = v.list()[0].clone();
        assert_eq!(after.last_connected_at.as_deref(), Some("2026-09-11T08:00:00Z"));
        assert_eq!(
            SessionRecord { last_connected_at: None, ..after },
            before,
            "记一笔连接只许动 last_connected_at —— 碰了 modified_at 的话,\
             「上次编辑」会被每次连接刷掉"
        );
    }
```

- [ ] **Step 2: 跑测试确认它红**

Run: `cargo test -p mullion-store last_connected 2>&1 | tail -20`
Expected: 编译失败。

- [ ] **Step 3: 加字段**

`crates/mullion-store/src/model.rs`，`SessionRecord` 里 `sftp` 之后：

```rust
    /// F258:最后一次**连上**的时刻(RFC3339)。`None` = 从没连上过。
    ///
    /// 与 `modified_at`(编辑时间)是**两回事**:改一次配置不等于用过它,
    /// 而 rehost 弹窗要回答的是"我最后用的是哪台"。
    ///
    /// **只在"用户当帧动作发起的拨号"连上时记**(设计 D10):启动恢复现场的
    /// 批量重连、F128 断线自愈都不记 —— 前者会让 N 条会话拿到几乎相同的
    /// 时间戳,把昨天攒下的先后顺序一次开机整体抹平,且全程无报错。
    ///
    /// 旧文件缺键即 `None`,不升 schema(同 F118 的 `sftp`)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_connected_at: Option<String>,
```

按编译错误逐处补 `last_connected_at: None,`：

```bash
cargo build --workspace 2>&1 | grep -E "^error|-->" | head -40
```

- [ ] **Step 4: 加 mutator**

`crates/mullion-store/src/vault.rs`，紧接 `touch_project_accessed` 之后：

```rust
    /// F258:记一笔「这条会话连上了」。`now` 由调用方给(store 不持有时钟)。
    ///
    /// **只改这一个字段、不跑校验、不碰 `modified_at`** —— 碰了的话
    /// 「上次编辑」会被每次连接刷掉,而会话管理器右栏拿它当"这条改过没"的依据。
    ///
    /// 调用时机见 `app::accept_connect_ok`:**只有 `DialTicket.user_initiated`
    /// 为真的那次拨号**才记。
    pub fn touch_session_connected(&mut self, id: crate::model::SessionId, now: &str) {
        self.sync_from_disk_if_untouched();
        if let Some(slot) = self.sessions.iter_mut().find(|s| s.id == id) {
            slot.last_connected_at = Some(now.to_string());
        }
    }
```

（字段名若不是 `self.sessions`，按该文件里 `list()` 返回的那个字段名改。）

- [ ] **Step 5: 跑测试确认它绿**

Run: `cargo test -p mullion-store 2>&1 | grep -E "test result|FAILED"`
Expected: 全绿。

- [ ] **Step 6: 提交**

```bash
git add -A
git commit -m "$(cat <<'EOF'
feat(store): 会话记录加 last_connected_at + touch_session_connected (F258)

与 modified_at(编辑时间)是两回事 —— rehost 要回答的是「我最后用的是
哪台」。mutator 只动这一个字段:碰了 modified_at 的话「上次编辑」会被
每次连接刷掉。旧文件缺键即 None,不升 schema。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: app —— `user_initiated` 随票走 + `ConnectOk` 记一笔

**Files:**
- Modify: `crates/mullion-app/src/app.rs`（`DialTicket`、`spawn_connect`、`reconnect_tab`、
  `accept_connect_ok`，以及 5 + 4 个调用点）

- [ ] **Step 1: 写失败测试**

`crates/mullion-app/src/app.rs` 的 `mod tests`（用本文件既有的**源码切片**手法，
参照 `app.rs:24624` 那条 `self.dials.claim(dial)` 的守护）：

```rust
    /// D10:「记一笔连上了」的判据必须来自**票**,不能在 `ConnectOk` 分支里
    /// `if` 猜来源。
    ///
    /// 猜来源的写法(比如 `if self.auto_dial.is_none()`)是列举式门控 ——
    /// 加一条新的拨号入口就会漏,而漏了的症状是「某条路径连上之后顺序不更新」,
    /// 静默。这个库里同款缺陷已经踩过多次。
    ///
    /// 自证会变红:把 `ticket.user_initiated` 换成任何一个读 `self` 的判据。
    #[test]
    fn whether_we_record_a_connection_comes_from_the_ticket_not_from_guessing() {
        let body = body_of(prod_src(), "fn accept_connect_ok(");
        assert!(
            body.contains("ticket.user_initiated"),
            "记一笔的判据必须读票上的 user_initiated"
        );
        assert!(
            body.contains("touch_session_connected"),
            "ConnectOk 里要真的记这一笔"
        );
    }

    /// 恢复现场的**批量**重连不许记 —— 一口气重连 N 条会让它们拿到几乎相同的
    /// 时间戳,把昨天攒下的先后顺序整体抹平,且全程无报错。
    ///
    /// 用户在占位标签上**手点**「重连」仍然算(那是当帧动作),所以判据挂在
    /// `advance_auto_dial` 这个**队列驱动**上,而不是 `reconnect_tab` 本身。
    ///
    /// 自证会变红:把 `advance_auto_dial` 里那句 `reconnect_tab(next, false)`
    /// 的 `false` 改成 `true`。
    #[test]
    fn the_startup_reconnect_queue_does_not_stamp_every_session_with_the_same_time() {
        let body = body_of(prod_src(), "fn advance_auto_dial(");
        assert!(
            body.contains("reconnect_tab(next, false)"),
            "批量重连必须传 user_initiated=false:{body}"
        );
    }
```

- [ ] **Step 2: 跑测试确认它红**

Run: `cargo test -p mullion-app comes_from_the_ticket 2>&1 | grep -E "test result|FAILED"`
Expected: **FAILED**。

- [ ] **Step 3: 票上加字段**

`crates/mullion-app/src/app.rs`，`DialTicket` 里追加：

```rust
    /// F258:这次拨号是不是**用户当帧的动作**发起的。
    ///
    /// 只有它为真时,`ConnectOk` 才往 `last_connected_at` 记一笔。
    ///
    /// **随票走而不是在 `ConnectOk` 里猜来源**:猜的写法是列举式门控,
    /// 加一条新的拨号入口就会漏,漏了的症状是「某条路径连上之后顺序不更新」,
    /// 静默。同 `skip_automation` 装进票的理由。
    ///
    /// 传 `false` 的只有一处:`advance_auto_dial` 驱动的启动批量重连
    /// —— 一口气重连 N 条会让它们拿到几乎相同的时间戳,把昨天攒下的先后
    /// 顺序整体抹平。用户在占位标签上**手点**「重连」不走那条队列,照记。
    ///
    /// F128 断线自愈不经过这里(它走 `spawn_reconnect`,不发票),天然不记。
    user_initiated: bool,
```

- [ ] **Step 4: 穿参数**

`spawn_connect` 加末位参数 `user_initiated: bool`，装进票。四处调用点：

| 位置 | 传 | 理由 |
|---|---|---|
| `app.rs:3023`（CLI 直连） | `true` | 命令行是用户发起的（`session_id` 为 `None`，实际不记） |
| `app.rs:3765`（`reconnect_tab` 内） | 该函数新增的同名参数 | 见下 |
| `app.rs:9741`（`dial_project`） | `true` | 打开项目 |
| `app.rs:13536`（会话管理器点连接 / 双击） | `true` | |

`reconnect_tab` 加参数 `user_initiated: bool`，透传给 `spawn_connect`。四处调用点：

| 位置 | 传 |
|---|---|
| `app.rs:3816` | `true`（占位标签上手点「重连」） |
| `app.rs:3834` | `true`（同上，另一入口） |
| `app.rs:3863`（`advance_auto_dial` 内） | **`false`** |
| `app.rs:12854` | `true`（用户动作，确认现场后逐条点） |

> 逐个调用点确认时**读一遍上下文**再决定，不要照抄这张表——若某处上下文表明它是
> 队列驱动的，一律传 `false`。判据只有一句：**这一帧有没有用户的手在动**。

- [ ] **Step 5: `ConnectOk` 里记一笔**

`crates/mullion-app/src/app.rs::accept_connect_ok`，在 `let session_id = ticket.session_id;`
之后插入：

```rust
        // F258:记一笔「这条会话连上了」。判据来自**票**(设计 D10)——
        // 在这里 `if` 猜来源的话,加一条新的拨号入口就会漏,而漏了没有任何报错。
        //
        // 排在 `wants_sftp` 那条 early-return **之前**:SFTP 节点也是"用过"。
        if ticket.user_initiated {
            if let (Some(id), Some(store)) = (session_id, self.store.as_mut()) {
                let now = time::OffsetDateTime::now_utc()
                    .format(&time::format_description::well_known::Rfc3339)
                    .unwrap_or_default();
                store.touch_session_connected(id, &now);
                if let Err(e) = store.save() {
                    // 记一笔失败不该拦住连接本身 —— 只落日志,不弹错。
                    log::warn!(target: "mullion", "记录会话连接时间失败:{e}");
                }
            }
        }
```

- [ ] **Step 6: 跑测试确认它绿**

Run: `cargo test -p mullion-app 2>&1 > /tmp/t.log; grep -nE "test result|FAILED|panicked" /tmp/t.log | head`
Expected: 全绿。

- [ ] **Step 7: 跑自证变异**

先确认工作区干净。把 `advance_auto_dial` 里的 `false` 改成 `true`，跑：

Run: `cargo test -p mullion-app startup_reconnect_queue 2>&1 | grep -E "test result|FAILED"`
Expected: **FAILED**。确认后 `git checkout crates/mullion-app/src/app.rs`。

> **源码切片的已知陷阱**（本项目吃过亏）：判据串若同时出现在**注释**里，测试会假绿。
> 写完检查 `advance_auto_dial` 的注释里没有出现 `reconnect_tab(next, false)` 这个串。

- [ ] **Step 8: 提交**

```bash
git add -A
git commit -m "$(cat <<'EOF'
feat(app): 用户发起的拨号连上时记一笔 last_connected_at (F258)

判据 user_initiated 随拨号票走,不在 ConnectOk 里 if 猜来源 —— 猜的
写法是列举式门控,加一条新拨号入口就漏,且漏了零报错。

只有 advance_auto_dial 驱动的启动批量重连传 false:一口气重连 N 条会
让它们拿到几乎相同的时间戳,把昨天的先后顺序整体抹平。手点「重连」
不走那条队列,照记。F128 自愈走 spawn_reconnect 不发票,天然不记。

守护已按注释里的变异自证变红。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: app —— Lit 置顶 + 冻结灯

**Files:**
- Modify: `crates/mullion-app/src/ui/project_list.rs`（`rows` 加 `frozen` 入参）
- Modify: `crates/mullion-app/src/ui/mod.rs`（`UiState` 两个冻结槽 + 两处 `else` 清空）
- Modify: `crates/mullion-app/src/ui/project_pick.rs`（`ProjectPickDraft` 带一份冻结灯）
- Modify: `crates/mullion-app/src/ui/project_manager.rs` / `launcher.rs`（传参）
- Delete: `crates/mullion-app/src/ui/project_manager.rs::by_recent_access`（被 `project_list::rows` 取代）

- [ ] **Step 1: 写失败测试**

`crates/mullion-app/src/ui/project_list.rs` 的 `mod tests`：

```rust
    use std::collections::BTreeMap;
    use crate::project::Lamp;

    fn lamps(pairs: &[(u64, Lamp)]) -> BTreeMap<ProjectId, Lamp> {
        pairs.iter().map(|(id, l)| (ProjectId(*id), *l)).collect()
    }

    /// D13:浏览态的「在用」tab 里,正亮着灯的项目**置顶**。
    ///
    /// 判据造成:亮灯那条的访问时间**更旧**。不置顶的话它排在后面。
    #[test]
    fn a_lit_project_floats_to_the_top_of_the_active_tab() {
        let ps = vec![
            proj(1, "亮着的老活", Some("2026-09-01T00:00:00Z"), None),
            proj(2, "刚开过的", Some("2026-09-10T00:00:00Z"), None),
        ];
        let f = lamps(&[(1, Lamp::Lit), (2, Lamp::Dark)]);
        let got: Vec<u64> = rows(&ps, Tab::Active, "", &[], &f).iter().map(|p| p.id.0).collect();
        assert_eq!(got, vec![1, 2], "亮着的要置顶");
    }

    /// `Unknown` **不算亮** —— 它的语义是"还有 pane 没上报过,可能在跑",
    /// 拿它当亮的话启动那几帧全表都会被判成亮,置顶等于没置顶。
    #[test]
    fn an_unknown_lamp_does_not_count_as_lit() {
        let ps = vec![
            proj(1, "灯未知的老活", Some("2026-09-01T00:00:00Z"), None),
            proj(2, "刚开过的", Some("2026-09-10T00:00:00Z"), None),
        ];
        let f = lamps(&[(1, Lamp::Unknown), (2, Lamp::Dark)]);
        let got: Vec<u64> = rows(&ps, Tab::Active, "", &[], &f).iter().map(|p| p.id.0).collect();
        assert_eq!(got, vec![2, 1], "Unknown 不该置顶");
    }

    /// D17:**搜索态不做 Lit 置顶**。搜的时候用户已经明确知道要找谁,
    /// 置顶只会打乱;而且不置顶意味着搜索态是纯函数、不需要定格。
    #[test]
    fn searching_does_not_float_lit_projects() {
        let ps = vec![
            proj(1, "亮着的 alpha", Some("2026-09-01T00:00:00Z"), None),
            proj(2, "刚开过的 alpha", Some("2026-09-10T00:00:00Z"), None),
        ];
        let f = lamps(&[(1, Lamp::Lit), (2, Lamp::Dark)]);
        let got: Vec<u64> = rows(&ps, Tab::Active, "alpha", &[], &f).iter().map(|p| p.id.0).collect();
        assert_eq!(got, vec![2, 1], "搜索态不该置顶");
    }

    /// 归档 tab 不做 Lit 置顶 —— 归档项目正跑着是个异常情况,不该因此排到
    /// 最上面(灯照常亮,那是提示"它还没关干净")。
    #[test]
    fn the_archived_tab_does_not_float_lit_projects() {
        let ps = vec![
            proj(1, "亮着的", Some("2026-09-30T00:00:00Z"), Some("2026-09-01T00:00:00Z")),
            proj(2, "后归的", Some("2026-09-01T00:00:00Z"), Some("2026-09-09T00:00:00Z")),
        ];
        let f = lamps(&[(1, Lamp::Lit), (2, Lamp::Dark)]);
        let got: Vec<u64> = rows(&ps, Tab::Archived, "", &[], &f).iter().map(|p| p.id.0).collect();
        assert_eq!(got, vec![2, 1], "归档 tab 按归档时间排,不置顶");
    }

    /// 冻结表里查不到的项目(列表开着的时候新建的)按 `Unknown` 处置 ——
    /// **不能消失**,也不能被当成亮的。
    #[test]
    fn a_project_missing_from_the_frozen_lamps_still_shows_up() {
        let ps = vec![proj(1, "新建的", None, None)];
        let got: Vec<u64> = rows(&ps, Tab::Active, "", &[], &BTreeMap::new())
            .iter()
            .map(|p| p.id.0)
            .collect();
        assert_eq!(got, vec![1], "冻结表里没有它,也必须画出来");
    }
```

再在 `crates/mullion-app/src/project.rs` 的 `mod tests` 加一条**前提锁**：

```rust
    /// D13 的前提:**启动页上灯不可能是 `Unknown`**。
    ///
    /// launcher 态一块 pane 都没有(`launcher::show` 的 `pane` 恒传 `None`),
    /// `panes` 是空切片。若这条不成立,启动页第一帧就会把全表的灯冻成
    /// `Unknown`,Lit 置顶**在最需要它的场景下永远不生效且零报错**。
    #[test]
    fn with_no_panes_at_all_a_lamp_is_never_unknown() {
        assert_eq!(lamp("proj-x", &[], &[]), Lamp::Dark);
        assert_eq!(lamp("proj-x", &[], &["proj-x".to_string()]), Lamp::Lit);
    }
```

- [ ] **Step 2: 跑测试确认它红**

Run: `cargo test -p mullion-app project_list 2>&1 | tail -20`
Expected: 编译失败（`rows` 只有四个参数）。

- [ ] **Step 3: 改 `rows`**

`crates/mullion-app/src/ui/project_list.rs`：

```rust
pub fn rows<'a>(
    projects: &'a [ProjectRecord],
    tab: Tab,
    query: &str,
    sessions: &[SessionRecord],
    frozen_lamps: &std::collections::BTreeMap<mullion_store::ProjectId, crate::project::Lamp>,
) -> Vec<&'a ProjectRecord> {
    let searching = !query.trim().is_empty();
    // F258:Lit 置顶**只在浏览态的「在用」tab**(设计 D13/D17)。
    //
    // 搜索态不置顶:用户已经明确知道要找谁,置顶只会打乱 —— 而且不置顶让
    // 搜索态成为纯函数,不需要下面那份冻结灯,需要显式失效的状态只剩一处。
    //
    // 归档 tab 不置顶:归档项目正跑着是个异常情况,不该因此排到最上面
    // (灯照常亮,那才是"它还没关干净"的提示)。
    let float_lit = !searching && tab == Tab::Active;
    let mut out: Vec<&ProjectRecord> = projects
        .iter()
        .filter(|p| searching || in_tab(p, tab))
        .filter(|p| crate::project::matches(p, query, sessions))
        .collect();
    out.sort_by(|a, b| {
        lit_rank(a, float_lit, frozen_lamps)
            .cmp(&lit_rank(b, float_lit, frozen_lamps))
            .then_with(|| archived(a).cmp(&archived(b)))
            .then_with(|| segment_order(a, b))
    });
    out
}

/// 置顶用的排名:`0` = 亮着要置顶,`1` = 其余。
///
/// 读的是**冻结的灯**(设计 D13):灯是异步变的(别的实例开/关项目、心跳超时),
/// 每帧实时排的话,某一行会在你正要点它的那一瞬间跳到列表最上面、把目标挤下去
/// —— 点错项目 = 连到另一台机器、attach 另一个 tmux,是本项目最不想要的那类
/// 「看不出错的误操作」。行上画的灯仍然是**实时**的:灯变色,位置不动。
///
/// 冻结表里查不到 → 按 `Unknown` 处置 → 不置顶,按时间落位。列表开着的时候
/// 新建的项目走这条,**不会消失**。
///
/// `Unknown` 不算亮:它的语义是"还有 pane 没上报过",拿它当亮的话启动那几帧
/// 全表都被判成亮,置顶等于没置顶。
fn lit_rank(
    p: &ProjectRecord,
    float_lit: bool,
    frozen: &std::collections::BTreeMap<mullion_store::ProjectId, crate::project::Lamp>,
) -> u8 {
    if !float_lit {
        return 0;
    }
    match frozen.get(&p.id) {
        Some(crate::project::Lamp::Lit) => 0,
        _ => 1,
    }
}
```

`empty_reason` 同步加 `frozen_lamps` 入参并透传给 `rows`（它只关心"空不空"，
顺序不影响结论，但签名要一致，否则调用方要维护两套参数表）。

- [ ] **Step 4: 加冻结槽**

`crates/mullion-app/src/ui/mod.rs`，`UiState`：

```rust
    /// F258:项目管理器左栏这一次打开期间**冻结的灯**。`None` = 还没装填。
    ///
    /// 只冻结灯,不冻结整份顺序 —— `last_accessed_at` 只在"打开项目"时变,
    /// 而那个动作本身就会关掉这个列表。整份顺序快照要显式失效,漏一处的症状
    /// 是"顺序永远停在上次",静默;冻结灯的失效点只有"列表关掉"一处,且就写在
    /// 开关那一行的 `else` 里。
    pub pm_frozen_lamps: Option<
        std::collections::BTreeMap<mullion_store::ProjectId, crate::project::Lamp>,
    >,
    /// F258:启动页这一次显示期间冻结的灯。同上。
    pub launcher_frozen_lamps: Option<
        std::collections::BTreeMap<mullion_store::ProjectId, crate::project::Lamp>,
    >,
```

在同一文件加一个共用装填器：

```rust
/// F258:第一次画的时候把当前的灯拷一份冻住,之后每帧都用这一份排序。
///
/// 「装填」而不是「打开时赋值」:装填点与使用点在同一行代码上,不可能漏 ——
/// 而"打开时赋值"要在每一个能打开这个列表的入口上各写一次。
pub fn freeze_lamps<'a>(
    slot: &'a mut Option<
        std::collections::BTreeMap<mullion_store::ProjectId, crate::project::Lamp>,
    >,
    live: &std::collections::BTreeMap<mullion_store::ProjectId, crate::project::Lamp>,
) -> &'a std::collections::BTreeMap<mullion_store::ProjectId, crate::project::Lamp> {
    slot.get_or_insert_with(|| live.clone())
}
```

- [ ] **Step 5: 两处 `else` 清空**

`crates/mullion-app/src/ui/mod.rs:977`：

```rust
    if ui_state.project_manager_open {
        project_manager::show( /* 原样 */ );
    } else {
        // F258:关掉了就把冻结的灯扔掉 —— 下次打开按新灯重排。
        // **写在这个 `else` 里**而不是各个关闭入口:关闭入口有好几个
        // (× / Esc / 打开项目顺手关掉),逐个去清必漏一个,而漏了的症状是
        // 「顺序永远停在上次打开那一刻」,静默。
        ui_state.pm_frozen_lamps = None;
    }
```

`crates/mullion-app/src/ui/mod.rs:1098`：

```rust
    if frame.launcher {
        launcher::show( /* 原样 */ );
    } else {
        // F258:同上。离开启动页(第一个标签立起来)就扔掉。
        ui_state.launcher_frozen_lamps = None;
    }
```

切换项目弹窗的那一份挂在 `ProjectPickDraft` 上，随弹窗一起生灭：

```rust
pub struct ProjectPickDraft {
    pub pane: PaneId,
    /// 搜索框里的字。
    pub filter: String,
    /// F258:这一次打开期间冻结的灯。**挂在 draft 上**:弹窗关掉 = draft 置
    /// `None`,冻结的灯跟着没了,不需要任何额外的失效点。
    pub frozen_lamps: Option<
        std::collections::BTreeMap<mullion_store::ProjectId, crate::project::Lamp>,
    >,
}
```

`ProjectPickDraft::new` 里 `frozen_lamps: None`。

- [ ] **Step 6: 三处调用点传参**

三处都改成：

```rust
    let frozen = crate::ui::freeze_lamps(&mut <对应的槽>, lamps);
    let rows = crate::ui::project_list::rows(projects, tab, query, sessions, frozen);
```

（`project_pick` 里 `<对应的槽>` 是 `&mut d.frozen_lamps`；注意 `d` 已经是 `&mut`，
借用顺序上先取 `frozen` 再进 `Area::show` 闭包，或者先 `clone` 一份出来——
`Area::show` 的闭包会再次可变借用 `d.filter`，直接把 `&BTreeMap` 带进闭包即可。）

- [ ] **Step 7: 删掉 `by_recent_access`**

`crates/mullion-app/src/ui/project_manager.rs` 里的 `by_recent_access` 已无调用方，
连同它那三条测试一起删——**但先确认**：

```bash
grep -rn "by_recent_access" crates/ | grep -v "^crates/mullion-app/src/ui/project_manager.rs"
```

Expected: 无输出。有输出就先改那些调用点。

> 删之前读一遍它的文档注释：里面「字典序即时间序」「id 兜底不能靠 sort 稳定性」
> 两条理由已经搬进 `project_list::newest_first` / `segment_order`。**不要连理由一起删掉**
> ——本项目有过"删死代码时把它带着的功能一起删了"的记录。

> **另有两条测试的文档注释点名了它**：`launcher.rs:169`
> (`the_launcher_puts_the_most_recently_used_project_first`) 与 `project_pick.rs:370`
> (`the_list_puts_the_most_recently_used_project_first`)。这两条断言的是**画出来的行序**，
> 不是源码切片——所以换成 `project_list::rows` 之后它们照旧成立，**别删**，
> 它们正好是这次改动的免费回归锁。只改注释里的函数名与"自证会变红"那句
> （改成「把 `project_list::rows(..)` 换成 `projects.iter()`」）。
> `project_manager.rs:869/883/899` 那三条才是直接调 `by_recent_access` 的单测，
> 它们的判据已被 `project_list` 的测试覆盖（时间倒序 / 无时间戳沉底 / id 兜底），
> 随函数一起删；删之前逐条对一遍确认 `project_list` 里真有对应的那一条。

- [ ] **Step 8: 跑测试确认它绿**

Run: `cargo test --workspace 2>&1 > /tmp/t.log; grep -nE "test result|FAILED|panicked" /tmp/t.log | head -20`
Expected: 全绿。

- [ ] **Step 9: 跑自证变异**

先确认工作区干净。把 `lit_rank` 里的 `if !float_lit { return 0; }` 删掉（让搜索态也置顶），跑：

Run: `cargo test -p mullion-app searching_does_not_float 2>&1 | grep -E "test result|FAILED"`
Expected: **FAILED**。撤掉。

再把 `Some(crate::project::Lamp::Lit) => 0,` 改成 `Some(_) => 0,`，跑：

Run: `cargo test -p mullion-app an_unknown_lamp 2>&1 | grep -E "test result|FAILED"`
Expected: **FAILED**。撤掉。

- [ ] **Step 10: 提交**

```bash
git add -A
git commit -m "$(cat <<'EOF'
feat(app): 项目列表把正亮着的置顶,排序按冻结的灯定格 (F258)

灯是异步变的(别的实例开关项目、心跳超时),每帧实时排的话某一行会在你
正要点它的瞬间跳到最上面 —— 点错项目 = 连到另一台机器、attach 另一个
tmux。所以排序读**冻结**的灯,行上画的灯仍是实时的:灯变色,位置不动。

只冻结灯而不是整份顺序:整份快照要显式失效,漏一处就「顺序永远停在上次」
且静默;冻结灯的失效点只剩「列表关掉」一处,写在开关那一行的 else 里。

搜索态与归档 tab 都不置顶,于是它们是纯函数、不需要定格。
by_recent_access 三处调用全部并入 project_list::rows 后删除。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

## Task 11: ui —— rehost「最近连过」段

**Files:**
- Modify: `crates/mullion-app/src/ui/rehost.rs`

- [ ] **Step 1: 写失败测试**

`crates/mullion-app/src/ui/rehost.rs` 的 `mod tests`：

```rust
    fn sess(id: u64, name: &str, connected: Option<&str>) -> SessionRecord {
        let mut r = base_session(id, name);
        r.last_connected_at = connected.map(str::to_string);
        r
    }

    /// D11:顶部「最近连过」段取 **3 条**,按最后连上时间倒序。
    ///
    /// 3 而不是 5:弹窗按 pane 定位,列表上限 260pt ≈ 5~6 行,5 条最近 +
    /// 分隔线就把主段挤没了 —— 用户会以为下面没东西了。
    ///
    /// 自证会变红:把 `RECENT_N` 改成 5。
    #[test]
    fn the_recent_section_takes_the_three_most_recently_connected() {
        let all = vec![
            sess(1, "a", Some("2026-09-01T00:00:00Z")),
            sess(2, "b", Some("2026-09-04T00:00:00Z")),
            sess(3, "c", Some("2026-09-03T00:00:00Z")),
            sess(4, "d", Some("2026-09-02T00:00:00Z")),
            sess(5, "e", None),
        ];
        let got: Vec<u64> = recent(&all, "").iter().map(|r| r.id.0).collect();
        assert_eq!(got, vec![2, 3, 4], "取最近 3 条,按时间倒序");
    }

    /// 从没连过的会话不进这一段 —— 那一段的名字就叫「最近连过」。
    #[test]
    fn a_session_never_connected_is_not_in_the_recent_section() {
        let all = vec![sess(1, "a", None), sess(2, "b", None)];
        assert!(recent(&all, "").is_empty(), "一条都没连过时整段不该出现");
    }

    /// 不足 3 条就有几条画几条。
    #[test]
    fn fewer_than_three_recent_sessions_just_draw_what_there_is() {
        let all = vec![sess(1, "a", Some("2026-09-01T00:00:00Z")), sess(2, "b", None)];
        let got: Vec<u64> = recent(&all, "").iter().map(|r| r.id.0).collect();
        assert_eq!(got, vec![1]);
    }

    /// D11:**搜索框非空时整段收起**。两段都过滤的话,一个五行的框里会出现两个
    /// 一模一样的行(主段不去重),用户会以为是 bug。
    ///
    /// 自证会变红:把 `recent` 里那句 `if !needle.is_empty() { return Vec::new(); }`
    /// 删掉。
    #[test]
    fn searching_collapses_the_recent_section_so_no_row_appears_twice() {
        let all = vec![sess(1, "alpha", Some("2026-09-01T00:00:00Z"))];
        assert!(
            recent(&all, "alpha").is_empty(),
            "搜索时「最近连过」段必须整段收起 —— 否则同一条会在框里出现两次"
        );
    }

    /// 主段**不去重**:下半段要与左栏严格同序,挖掉几条的话「照左栏记忆找」
    /// 那条既有决策(F130)就又破了。
    #[test]
    fn the_main_section_still_lists_sessions_that_are_in_the_recent_section() {
        let all = vec![sess(1, "a", Some("2026-09-01T00:00:00Z"))];
        let main: Vec<u64> = visible(&all, &[], "").iter().map(|r| r.id.0).collect();
        assert_eq!(main, vec![1], "主段不许把「最近连过」里那几条挖掉");
    }
```

`base_session(id, name)` 照本文件既有测试里构造 `SessionRecord` 的写法抽出来。

- [ ] **Step 2: 跑测试确认它红**

Run: `cargo test -p mullion-app rehost 2>&1 | tail -20`
Expected: 编译失败（没有 `recent`）。

- [ ] **Step 3: 实现 `recent`**

`crates/mullion-app/src/ui/rehost.rs`：

```rust
/// 「最近连过」段最多列几条。
///
/// **3 而不是 5**:这个弹窗按 pane 定位,列表高度上限 260pt ≈ 5~6 行。
/// 5 条最近 + 一条分隔线就把主段挤没了,用户会以为下面没东西 —— 而主段
/// (与会话管理器左栏同序)才是"照记忆找"的那一半。
const RECENT_N: usize = 3;

/// 顶部「最近连过」段列哪几条。
///
/// **搜索框非空时整段收起**(设计 D11):主段不去重,两段都过滤的话,一个
/// 五行的框里会出现两个一模一样的行 —— 短列表里的视觉重复是实打实的代价,
/// 而"我记得最近连过某台,搜个关键字"这个动作里时间序帮不上忙(用户已经
/// 明确知道要找谁)。
///
/// 从没连上过的(`last_connected_at` 为 `None`)不进这一段 —— 段名就叫
/// 「最近连过」。一条都没有时返回空,调用方连分隔线一起不画。
///
/// 只收 `Protocol::Ssh`,理由同 `visible`:SFTP 节点没有 PTY,换过去只有
/// 一块永远不出字的黑屏。
fn recent<'a>(sessions: &'a [SessionRecord], needle: &str) -> Vec<&'a SessionRecord> {
    if !needle.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<&SessionRecord> = sessions
        .iter()
        .filter(|r| r.connection.protocol == Protocol::Ssh)
        .filter(|r| r.last_connected_at.is_some())
        .collect();
    // 时间倒序;同刻按 id 升序兜底(不能靠 sort 的稳定性,那样顺序取决于
    // 磁盘上 `[[session]]` 的书写次序)。
    out.sort_by(|a, b| {
        b.last_connected_at
            .cmp(&a.last_connected_at)
            .then(a.id.0.cmp(&b.id.0))
    });
    out.truncate(RECENT_N);
    out
}
```

- [ ] **Step 4: 画出来**

`crates/mullion-app/src/ui/rehost.rs::show`，在 `let rows = visible(...)` 之前插入：

```rust
                    let recents = recent(sessions, &d.filter);
```

在 `ScrollArea` 的闭包**开头**（`ui.set_min_width(field_w);` 之后）插入：

```rust
                            // F258:「最近连过」段。放在滚动区**里面**而不是
                            // 上面 —— 放外面的话它会占掉 `CHROME_H` 没算过的
                            // 高度,把取消按钮顶出 pane 外,而那是这个模态
                            // 弹窗唯一的退出口。
                            if !recents.is_empty() {
                                ui.label(
                                    egui::RichText::new("最近连过")
                                        .size(11.0)
                                        .color(theme::c32(t.fg_muted)),
                                );
                                for rec in &recents {
                                    let color = appearance.get(rec.id).and_then(|a| {
                                        crate::ui::badge::should_paint(a, ColorTarget::ListItem)
                                    });
                                    if row(ui, rec, color, t) {
                                        action = Some(RehostAction::Pick {
                                            pane,
                                            session: rec.id,
                                        });
                                    }
                                }
                                ui.separator();
                            }
```

> **行 id 会撞**：`rehost.rs:22` 的 `row_id(id: SessionId)` 只吃一个参数，
> 同一条会话在两段各画一次就是同一个 id，egui 会报重复 id 并让其中一处点不动。
> 给 `row` 加一个 `salt: &str` 参数，`row_id` 改成
> `egui::Id::new(("rehost_row", salt, id.0))`，两段分别传 `"recent"` / `"main"`。
> **这条必须做**，否则「最近连过」段整段点不动，而且**不报错**。
> 形状照抄 `project_row::row_id(list, id)` ——那边早就是 `(salt, id)` 两参，
> 正是因为同一个项目要在三处列表里各画一次。

补一条守护：

```rust
    /// 同一条会话在两段各画一次,行 id 必须不同 —— 撞 id 的话 egui 会让其中
    /// 一段整段点不动,而**不报错**。
    ///
    /// 自证会变红:把 `row_id` 的 `salt` 参数去掉。
    #[test]
    fn the_same_session_gets_different_ids_in_the_two_sections() {
        assert_ne!(row_id("recent", SessionId(7)), row_id("main", SessionId(7)));
    }
```

- [ ] **Step 5: 跑测试确认它绿**

Run: `cargo test -p mullion-app rehost 2>&1 | grep -E "test result|FAILED"`
Expected: `test result: ok.`

- [ ] **Step 6: 跑自证变异**

工作区干净后，逐条跑注释里标的变异（`RECENT_N` 改 5、删搜索收起那句、去掉 `salt`），
每次确认对应测试变红再撤。

- [ ] **Step 7: 提交**

```bash
git add -A
git commit -m "$(cat <<'EOF'
feat(app): 换节点弹窗顶部加「最近连过」3 条 (F258)

主段保持与会话管理器左栏同序(F130 那条既有决策原样保留),**不去重** ——
挖掉几条的话「照左栏记忆找」就又破了。搜索框非空时整段收起:主段不去重,
两段都过滤会让一个五行的框里出现两个一模一样的行。

3 条而不是 5:弹窗列表上限 260pt ≈ 5~6 行,5 条 + 分隔线把主段挤没了。

行 id 加 salt:同一条会话在两段各画一次,撞 id 会让其中一段整段点不动
且不报错。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

## Task 12: spec.md 补两行

**Files:**
- Modify: `spec.md`（F252 那一行之后）

- [ ] **Step 1: 追加两行**

照该表既有格式（`| 编号 | 描述 | 优先级 | 坑 |`）在 F252 之后加：

```markdown
| F257 | **项目可以归档**：`ProjectRecord.archived_at: Option<String>`（RFC3339，`skip_serializing_if`，**升 schema v12**）。项目管理器左栏分「在用」/「归档」两 tab；启动页与切换项目弹窗只列在用的，**搜索穿透两态**。三处的「列什么 / 怎么排 / 空了说什么」收进 `ui::project_list` 纯函数。入口在右栏底部「删除项目」左边，无确认框 | P2 | **① 归档仍参与 `validate_project`**：退出校验的话名字被释放，**取消归档**就可能撞车 —— 而归档的全部安全感来自「随时能撤回来」，撞车放过去就是「两个项目共用一个 Claude Code」。**② 空态必须四档穷尽**：全归档时 `projects.is_empty()` 仍是 `false`，原来那句「还没有项目，去建一个」会让用户真的建一个重复的；三处各写 `if` 加档必漏，所以判据抽成返回枚举的共用函数。**③ 搜索态整条收起 tab 栏**：搜索穿透两态，留着 tab 的话两个 tab 显示完全相同的结果，切过去画面不变，用户会以为点坏了。**④ 归档时刻存时间戳不存 `bool`**：归档 tab 要按「什么时候归的」倒序（刚归错的马上能撤），`bool` 只能回落 `last_accessed_at`，那样「上周归的」和「半年前归的」混在一起。**⑤ 升 schema v11 → v12**：旧客户端会把 `archived_at` 当未知字段丢掉再写回，用户归档过的项目静默回到「在用」；分界线是**可再生性**——`last_accessed_at` / `last_connected_at` 丢了下次打开/连接就重写、不升号，`archived_at` 是用户的决定、不会自愈、必须升号。**⑥ 打开归档项目不自动撤销归档**：系统看到的只是「你打开了它」，而理由可能只是去捞个文件；让系统推翻用户的判断，错的时候是静默的。`archived_at` 只由那两个按钮写 |
| F258 | **按最后活跃排序**：会话侧新增 `SessionRecord.last_connected_at`，换节点弹窗顶部加「最近连过」3 条（主段仍与左栏同序、不去重）；项目侧三处列表把正亮着灯的置顶，排序读**冻结的灯** | P2 | **① 写入判据随拨号票走**（`DialTicket.user_initiated`），不在 `ConnectOk` 里 `if` 猜来源：猜是列举式门控，加一条新拨号入口就漏且零报错。**② 启动批量重连不许记**：一口气重连 N 条会让它们拿到几乎相同的时间戳，把昨天攒下的先后顺序一次开机整体抹平，全程无报错；用户手点「重连」不走那条队列，照记。**③ 排序读冻结的灯、行上画实时的灯**：灯是异步变的，实时排会让某一行在你正要点它的瞬间跳到最上面 —— 点错项目 = 连到另一台机器、attach 另一个 tmux。**④ 只冻结灯，不冻结整份顺序**：整份快照要显式失效，漏一处就「顺序永远停在上次」且静默；冻结灯的失效点只剩「列表关掉」一处，就写在开关那行的 `else` 里。**⑤ 搜索态与归档 tab 都不置顶** —— 于是它们是纯函数、不需要定格。**⑥ 两段的行 id 要加 salt**：同一条会话在「最近连过」和主段各画一次，撞 id 会让其中一段整段点不动且不报错。**⑦ 前提锁**：launcher 态一块 pane 都没有，`lamp()` 因此不可能返回 `Unknown` —— 若这条不成立，启动页第一帧就把全表冻成 `Unknown`，置顶在最需要它的场景下永远不生效 |
```

- [ ] **Step 2: 提交**

```bash
git add spec.md
git commit -m "$(cat <<'EOF'
docs(spec): 补 F257 项目归档 / F258 最后活跃排序

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
EOF
)"
```

---

## Task 13: 跑绿 + 发版 0.1.106

- [ ] **Step 1: 全量跑绿**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log | head -30
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20
cargo fmt --check
```

Expected: 测试全过；clippy 无输出；fmt 无输出。**三条都满足才叫「绿」。**

- [ ] **Step 2: 发版**

按 `.claude/skills/release-windows/SKILL.md` 一条龙：升 patch（0.1.105 → **0.1.106**）→
跑绿 → 交叉编译 → objdump 依赖验收 → 签名 → 发 GitHub Release（走 socks 代理）。
**别凭记忆做**，每一步都有漏了也不报错的坑。

- [ ] **Step 3: 人工验收清单（写进 Release notes）**

**当天能验：**
1. 项目管理器左栏出现「在用 / 归档」两个 tab，默认在「在用」。
2. 选中一个项目 → 右栏底部「归档」在「删除项目」左边 → 点它 → 该项目从「在用」消失、出现在「归档」，按钮变成「取消归档」。
3. 点「取消归档」→ 回到「在用」。
4. 在搜索框打字 → **tab 栏整条消失**，归档项目也能被搜出来，行的时间列写「已归档」；**鼠标悬在那三个字上**要浮出「已归档 · <相对时间>」＋「最后打开 · <相对时间>」。
5. 清空搜索 → tab 栏回来，还停在你原来那个 tab。
6. 把所有项目都归档 → 启动页 / 切换项目弹窗说「没有在用的项目。归档里还有 N 个 —— 到…取消归档」，**不再**说「还没有项目」。
7. 归档一个正开着的项目 → 它的灯照常亮、pane 照常在、tmux 没被动。
8. 归档项目**能打开**，打开后**仍在归档 tab 里**（不自动撤销）。
9. 换节点弹窗：首次升级后「最近连过」段**不出现**（`last_connected_at` 还是空的，这是预期）。

**要攒几天才验得了（F258 的真正判据）：**
10. 正常用几天后，换节点弹窗顶部出现「最近连过」3 条，顺序 = 你最近连的顺序；下半段仍与会话管理器左栏同序，且**那 3 条在下半段里照样在**。
11. 在弹窗里搜索 → 「最近连过」段整段消失，没有重复行。
12. 项目列表里，**正开着的项目排在最上面**；开着列表时让另一个实例开/关一个项目，灯会变色但**行不跳位**；关掉列表再打开，位置才按新灯重排。
13. 重启客户端、走「恢复现场」把一批标签连回来 → 项目/会话的先后顺序**没有被抹平**（这条是 D10 的核心，也是最容易静默错的一条）。

---

## Self-Review

**Spec 覆盖：** Q1~Q18 的 13 条决案逐条对到任务——D1/D5→T1,T2；D2/D3/D4/D8→T3,T5,T6；
D6→T1 文档 + T4 消费点；D7→T4；D9/D10→T8,T9；D12/D13→T3,T10；D11→T11。
Q12「归档项目能打开 / 灯照常」是**零改动即成立**（归档只进列表过滤，不进任何运行时路径），
在 T1 的字段文档里写明并由人工验收 7/8 条兜底。

**占位符扫描：** 无 TBD / 无「类似 Task N」。三处测试 harness（`archive_and_delete_button_rects`、
`tab_bar_visible`、`names_drawn`）指明了照哪条既有测试写，并给了那条测试的行号。

**类型一致：** `Tab` / `Surface` / `EmptyReason` / `rows` / `empty_reason` / `empty_text` /
`freeze_lamps` / `lit_rank` / `recent` / `RECENT_N` / `archive_button_label` /
`set_project_archived` / `touch_session_connected` / `ProjectIntent::SetArchived` /
`DialTicket.user_initiated` 在定义处与使用处名字一致。`rows` 在 T3 是 4 参、T10 加到 5 参，
T10 的 Step 6 明确要求三处调用点同步改。
