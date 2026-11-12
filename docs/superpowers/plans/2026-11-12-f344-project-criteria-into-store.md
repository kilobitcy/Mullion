# F344 项目判据下沉 mullion-store 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把桌面端「项目」的纯判据(搜索分词、项目匹配与片段、候选节点顺序、灯枚举、列表筛选排序与空态判定、相对时间)从 `mullion-app` 原样搬进 `mullion-store`,让未来的 `mullion-ffi` 与桌面共用同一份;**桌面行为零变化**。

**Architecture:** 定义与它们的单测一起搬到 store 的三个模块(`search` / `localtime` / `project_view`)。app 原路径留 `pub use` 转出,**调用点一行不改**(`Lamp` 在 app 里有 70+ 处引用,改调用点就不是「只搬」了)。不在 store 里的 UI 措辞(`Surface`、`empty_text`)留在 app。

**Tech Stack:** Rust workspace;`time 0.3`(store 新增直接依赖,版本已在 Cargo.lock)。

**设计依据:** `docs/adr-013-android-client.md` 决策 3;`docs/superpowers/specs/2026-11-12-android-client-design.md` §2。

## 只搬不改的硬规则(每个任务都适用)

1. 函数体、doc 注释、测试体**逐字搬**。唯一允许的改动是**路径**:`mullion_store::X` → `crate::X`(进了 store 自己),`crate::project::matches` → `crate::project_view::matches`,`crate::project::Lamp` → `Lamp`,`crate::search::…` 保持(search 也进了 store,路径恰好不变)。
2. 不重命名、不改可见性(原来私有的 helper 搬过去仍私有)、不合并函数、不顺手改注释。注释里提到 app 文件名的(如 `highlight::segments`、`project_row`)**原样保留**——它们是在说明调用方,搬家不改变事实。
3. 搬走的测试在 app 里删掉;仍在 app 的测试若因此用不到某个 helper,删掉那个 helper(clippy `-D warnings` 会以 dead_code 报出来,以编译器为准,不要猜)。
4. 不搬的东西(本计划**明确不碰**):`plan_open`、`confirm_reasons`、`AtRisk`、`OpenStep`、`lamp()` 函数本身、`pick_node`(async,store 零 async)、`icon_for`/`icon_bg`(egui)、`list_clients_command`(依赖 mullion_ssh)、`fresh_project_name`、恢复现场一族、`localtime::{init, offset, format_unix}`。

## 文件结构

| 文件 | 动作 | 职责 |
|---|---|---|
| `crates/mullion-store/Cargo.toml` | 改 | 加 `time.workspace = true` |
| `crates/mullion-store/src/lib.rs` | 改 | 声明 `pub mod search; pub mod localtime; pub mod project_view;` |
| `crates/mullion-store/src/search.rs` | 新建 | app `search.rs` 全文搬入 |
| `crates/mullion-store/src/localtime.rs` | 新建 | 只有 `relative` + 它的 6 条测试 |
| `crates/mullion-store/src/project_view.rs` | 新建 | `Lamp`、`candidates`、`node_for`、`matches`、`hidden_hit_snippet`(+私有 helper)、`Tab`、`EmptyReason`、`rows`、`empty_reason`(+私有 helper) |
| `crates/mullion-app/src/search.rs` | 改 | 只剩一行 `pub use` 转出 |
| `crates/mullion-app/src/localtime.rs` | 改 | 删 `relative` 及其测试,加 `pub use` |
| `crates/mullion-app/src/project.rs` | 改 | 删搬走项,加 `pub use` |
| `crates/mullion-app/src/ui/project_list.rs` | 改 | 只留 `Surface`、`empty_text` 及引用它们的测试,其余 `pub use` |

---

### Task 0: 记录基线

**Files:** 无改动。

- [ ] **Step 1: 记录测试名多重集合**

```bash
cd /data/Mullion
cargo test --workspace -- --list 2>/dev/null | grep ': test$' | grep -v ' - ' | sed -E 's/: test$//; s/.*:://' | sort > /tmp/f344-tests-before.txt
wc -l /tmp/f344-tests-before.txt
```

记下行数。这是最终验收的正交证据之一:搬完之后**测试名多重集合必须完全相同**(只换了所在 crate,一条没多一条没少)。

- [ ] **Step 2: 确认起点是绿的**

```bash
cargo test --workspace > /tmp/f344-base.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/f344-base.log | grep -v " 0 failed" ; echo "exit-filter-done"
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: 第一条只打印 `exit-filter-done`(没有任何失败行);clippy 结尾 `Finished`,无 warning。不绿就停下来报告,不要在红的基线上开工。

---

### Task 1: `search` 模块整体搬进 store

**Files:**
- Create: `crates/mullion-store/src/search.rs`
- Modify: `crates/mullion-store/src/lib.rs`
- Modify: `crates/mullion-app/src/search.rs`

- [ ] **Step 1: 原样复制文件**

```bash
cd /data/Mullion
cp crates/mullion-app/src/search.rs crates/mullion-store/src/search.rs
```

文件内没有 `crate::`/`mullion_store::` 路径(已核实:只依赖 std),无需改路径。

- [ ] **Step 2: store 里声明模块**

`crates/mullion-store/src/lib.rs` 的 `pub mod` 列表按字母序插入(在 `pub mod secrets_file;` 之前):

```rust
pub mod search;
```

- [ ] **Step 3: app 的 search.rs 改成转出**

把 `crates/mullion-app/src/search.rs` 整个文件替换为:

```rust
//! F245 的分词与命中判据。**定义已搬到 `mullion_store::search`**(F344),
//! 让 Android 端与桌面共用同一份;这里只转出,调用点不变。
pub use mullion_store::search::*;
```

- [ ] **Step 4: 编译并跑两个 crate 的测试**

```bash
cargo test -p mullion-store search:: 2>&1 | grep -E "test result|FAILED"
cargo test -p mullion-app 2>&1 > /tmp/f344-t1.log; grep -nE "test result|FAILED|error\[" /tmp/f344-t1.log | head
```

Expected: store 里 `search::tests::*` 全部 PASS(条数等于原 app `search.rs` 测试条数);app 编译通过且全绿。

- [ ] **Step 5: Commit**

```bash
git add crates/mullion-store/src/search.rs crates/mullion-store/src/lib.rs crates/mullion-app/src/search.rs
git commit -m "refactor(store): 搜索分词判据下沉 mullion-store,app 原路径转出 (F344)

只搬不改:函数体、注释、测试逐字搬,app 调用点零改动。

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: `localtime::relative` 搬进 store

**Files:**
- Modify: `crates/mullion-store/Cargo.toml`
- Create: `crates/mullion-store/src/localtime.rs`
- Modify: `crates/mullion-store/src/lib.rs`
- Modify: `crates/mullion-app/src/localtime.rs`

- [ ] **Step 1: store 加 time 依赖**

`crates/mullion-store/Cargo.toml` 的 `[dependencies]` 末尾(`sha2.workspace = true` 之后)加:

```toml
# F344:项目列表「最后打开」相对时间(`localtime::relative`)下沉到这里,
# 桌面与 Android 共用。0.3 已在 Cargo.lock(app 直接依赖),不引新版本。
time.workspace = true
```

- [ ] **Step 2: 新建 store 的 localtime.rs**

内容 = 下面这个文件头 + 从 app `localtime.rs` **逐字剪切**的三块:

1. `pub fn relative(...)` 连同它上方整段 `/// F234:…` doc 注释(app 文件中从 `/// F234:「最后打开」的相对时间文案。` 到该函数结束的 `}`)。
2. 测试模块里 `relative` 的专用 helper:`fn now()`、`fn cn()`,连同各自 doc 注释。
3. 只调用 `relative` 的 6 条测试(连同各自 doc 注释与 `#[test]`):`anything_within_a_minute_reads_as_just_now`、`minutes_and_hours_are_counted_off_the_raw_difference`、`yesterday_is_decided_by_the_local_day_boundary_not_the_utc_one`、`a_few_days_back_counts_days_and_a_month_back_falls_back_to_a_date`、`a_timestamp_from_the_future_reads_as_just_now_not_a_negative_count`、`an_unparseable_stamp_is_shown_verbatim_instead_of_a_made_up_placeholder`,以及测试模块中 `cn()` 与 `the_files_panel_column…` 之间其余任何**只**调用 `relative` 的测试(以 `grep -n "relative(" crates/mullion-app/src/localtime.rs` 为准逐条核对,全部搬走)。

文件头与骨架:

```rust
//! F234 的相对时间文案。**纯函数**:`now` 与 `offset` 都是参数,
//! 不读进程级时区(那一份 `OnceLock` 留在 app 的 `localtime`,它要求
//! 在起任何线程之前取值,是进程启动的事,不属于 store)。F344 从 app 搬来。

use time::{OffsetDateTime, UtcOffset};

// <在此粘贴 relative 及其 doc 注释>

#[cfg(test)]
mod tests {
    use super::*;

    // <在此粘贴 now()、cn() 与 relative 的测试>
}
```

- [ ] **Step 3: store 里声明模块**

`crates/mullion-store/src/lib.rs` 在 `pub mod layout;` 之后插入:

```rust
pub mod localtime;
```

- [ ] **Step 4: app 的 localtime.rs 转出**

在 app `localtime.rs` 原 `relative` 所在位置(已剪走)放:

```rust
/// F234 的相对时间。定义已搬到 `mullion_store::localtime`(F344),这里只转出。
pub use mullion_store::localtime::relative;
```

app 测试模块里 `now()`/`cn()` 若已无人使用,一并删掉(以 clippy 报 dead_code 为准)。

- [ ] **Step 5: 编译并跑测试**

```bash
cargo test -p mullion-store localtime:: 2>&1 | grep -E "test result|FAILED"
cargo test -p mullion-app localtime 2>&1 | grep -E "test result|FAILED|error\["
cargo clippy -p mullion-app -p mullion-store --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: store 的 localtime 测试全 PASS;app 剩下的 localtime 测试(`format_unix` 两条、偏移捕获、文件面板列)全 PASS;clippy 无 warning。

- [ ] **Step 6: Commit**

```bash
git add crates/mullion-store/Cargo.toml crates/mullion-store/src/localtime.rs crates/mullion-store/src/lib.rs crates/mullion-app/src/localtime.rs Cargo.lock
git commit -m "refactor(store): 相对时间判据下沉 mullion-store,app 原路径转出 (F344)

只搬 relative 及其测试;进程级时区偏移(init/offset)与 format_unix 留在 app。

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: 项目匹配 / 候选节点 / 灯枚举 → `store::project_view`

**Files:**
- Create: `crates/mullion-store/src/project_view.rs`
- Modify: `crates/mullion-store/src/lib.rs`
- Modify: `crates/mullion-app/src/project.rs`

- [ ] **Step 1: 新建 project_view.rs,逐字搬入以下项(连同 doc 注释)**

从 `crates/mullion-app/src/project.rs` 剪切:
- `pub fn candidates`、`pub fn node_for`
- `pub enum Lamp`(连同 derive 与 doc;**不搬** `pub fn lamp`)
- 私有 `fn visible_fields`、`fn hidden_fields`
- `pub fn matches`
- `const SNIPPET_BEFORE`、`const SNIPPET_AFTER`
- `pub fn hidden_hit_snippet`、私有 `fn snippet_around`、`fn fold`

路径改写(仅此三类):`mullion_store::` → `crate::`;`crate::search::` 保持不变。

从 app `project.rs` 的测试模块剪切:
- helper `sess`、`pr`、`proj`(`proj` 仍被 app 里 `plan_open` 的测试使用 → **复制**过去而不是剪切;`sess`/`pr` 若 app 里仍有人用同理复制,以编译结果为准)
- candidates 测试:`a_direct_node_is_tried_before_a_routed_preferred_one`、`within_each_group_the_preferred_node_leads_and_the_rest_keep_their_order`
- matches/片段测试(`// ---- matches / fresh_project_name` 分节下、`fresh_project_name` 测试之前的全部 12 条):`an_empty_query_lets_every_project_through`、`name_and_directory_both_match_case_insensitively`、`a_project_is_found_by_the_name_or_host_of_any_node_it_can_dial`、`a_session_that_is_not_a_node_of_this_project_never_makes_it_match`、`a_project_is_found_by_a_word_from_its_note`、`a_project_is_found_by_the_tmux_name_it_will_actually_attach_to`、`two_words_may_land_on_the_note_and_on_a_node_host`、`a_row_that_only_matched_the_note_offers_a_snippet_of_it`、`memos_are_searchable_and_explain_themselves_in_the_snippet`、`a_row_whose_visible_fields_already_explain_it_keeps_its_subtitle`、`a_snippet_marks_where_it_was_cut_and_never_carries_a_newline`、`a_snippet_that_cut_nothing_has_no_ellipsis`

测试里的 `mullion_store::` 同样改成 `crate::`。

文件头:

```rust
//! 项目列表与打开项目的**纯判据**,桌面与 Android 共用(F344 从 app 搬来)。
//! 零 UI、零 IO、零 async。各函数的设计理由见其 doc 注释(原样保留)。

use crate::{ProjectRecord, SessionId, SessionRecord};
```

(若原代码全用全限定路径而不需要 `use`,则不加这一行,以 clippy `unused_imports` 为准。)

- [ ] **Step 2: store 声明模块**

`crates/mullion-store/src/lib.rs` 在 `pub mod project;` 之后插入:

```rust
pub mod project_view;
```

- [ ] **Step 3: app project.rs 转出**

在 app `project.rs` 文件顶部(模块 doc 之后、第一个 item 之前)加:

```rust
// F344:以下判据已搬到 `mullion_store::project_view`,与 Android 端共用;
// 这里转出,调用点不变。
pub use mullion_store::project_view::{candidates, hidden_hit_snippet, matches, node_for, Lamp};
```

- [ ] **Step 4: 编译并跑测试**

```bash
cargo test -p mullion-store project_view:: 2>&1 | grep -E "test result|FAILED"
cargo test -p mullion-app 2>&1 > /tmp/f344-t3.log; grep -nE "test result|FAILED|error\[" /tmp/f344-t3.log | head
cargo clippy -p mullion-app -p mullion-store --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: store 新增 14 条测试全 PASS;app 全绿;clippy 无 warning。

- [ ] **Step 5: Commit**

```bash
git add crates/mullion-store/src/project_view.rs crates/mullion-store/src/lib.rs crates/mullion-app/src/project.rs
git commit -m "refactor(store): 项目匹配/候选节点/灯枚举下沉 mullion-store,app 原路径转出 (F344)

只搬不改;lamp() 计算、plan_open、pick_node(async)、图标与恢复现场一族留在 app。

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: 列表筛选排序与空态判定 → `store::project_view`

**Files:**
- Modify: `crates/mullion-store/src/project_view.rs`
- Modify: `crates/mullion-app/src/ui/project_list.rs`

- [ ] **Step 1: 逐字搬入 project_view.rs 末尾(测试模块之前)**

从 `crates/mullion-app/src/ui/project_list.rs` 剪切:`pub enum Tab`(连同 `impl Default` 与 `impl Tab { fn label }`)、`pub enum EmptyReason`、`pub fn rows`、`fn lit_rank`、`fn in_tab`、`fn archived`、`fn segment_order`、`fn newest_first`、`pub fn empty_reason`,全部连同 doc 注释。

路径改写:`crate::project::matches` → `matches`;`crate::project::Lamp` → `Lamp`;`mullion_store::` → `crate::`。

**留在 app**:`pub enum Surface`、`pub fn empty_text`、模块 doc、`use`。

- [ ] **Step 2: 测试分家**

app `project_list.rs` 测试模块里:**函数体出现 `empty_text` 或 `Surface` 的测试留在 app**,其余全部剪到 store `project_view.rs` 的测试模块。helper `lamps`、`proj` 按需复制(两边都用就两边各一份;此处的 `proj` 与 Task 3 的 `proj` 签名不同——store 里若撞名,把**本任务**这份改名为 `listed`,并同步改它在搬过去的测试里的调用;这是本计划唯一允许的改名,原因是两个 app 模块合进了一个 store 模块)。

- [ ] **Step 3: app project_list.rs 转出**

在 app `ui/project_list.rs` 的 `use` 之后加:

```rust
// F344:筛选/排序/空态判定已搬到 `mullion_store::project_view`,与 Android
// 端共用;这里转出,调用点不变。空态**措辞**(`Surface`/`empty_text`)是各端
// 自己的 UI 文案,留在这里。
pub use mullion_store::project_view::{empty_reason, rows, EmptyReason, Tab};
```

- [ ] **Step 4: 编译并跑测试**

```bash
cargo test -p mullion-store project_view:: 2>&1 | grep -E "test result|FAILED"
cargo test -p mullion-app project_list 2>&1 | grep -E "test result|FAILED|error\["
cargo clippy -p mullion-app -p mullion-store --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: 全 PASS,clippy 无 warning。

- [ ] **Step 5: Commit**

```bash
git add crates/mullion-store/src/project_view.rs crates/mullion-app/src/ui/project_list.rs
git commit -m "refactor(store): 项目列表筛选排序与空态判定下沉 mullion-store,app 原路径转出 (F344)

只搬不改;各端的空态措辞(Surface/empty_text)留在 app。

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: 正交验收

**Files:** 无改动。

- [ ] **Step 1: 测试名多重集合与基线逐字相同**

```bash
cd /data/Mullion
cargo test --workspace -- --list 2>/dev/null | grep ': test$' | grep -v ' - ' | sed -E 's/: test$//; s/.*:://' | sort > /tmp/f344-tests-after.txt
diff /tmp/f344-tests-before.txt /tmp/f344-tests-after.txt && echo SAME
```

Expected: 打印 `SAME`。有差异 = 漏搬了测试(少)或复制成了两份(多),逐条查。注意 helper 函数不带 `#[test]`,不会出现在列表里。

- [ ] **Step 2: store 没有被 UI/async 污染**

```bash
grep -nE "egui|crate::ui|async fn|tokio|mullion_ssh|mullion_app" crates/mullion-store/src/{search,localtime,project_view}.rs; echo "grep-exit=$?"
```

Expected: `grep-exit=1`(零命中)。

- [ ] **Step 3: 依赖方向未破**

```bash
grep -nE "mullion-(app|core|term|ssh|cloud)" crates/mullion-store/Cargo.toml; echo "grep-exit=$?"
```

Expected: `grep-exit=1`。

- [ ] **Step 4: 「绿」**

```bash
cargo test --workspace > /tmp/f344-final.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/f344-final.log | grep -v " 0 failed"; echo filter-done
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -3
cargo fmt --check && echo FMT-OK
```

Expected: 只打印 `filter-done`;clippy `Finished` 无 warning;`FMT-OK`。

- [ ] **Step 5: spec 登记状态**

`spec.md` §4.13 表 F344 行的「验收」列末尾追加:`已实现(v<版本号>):search / localtime::relative / project_view 三模块下沉,测试名多重集合与搬前逐字相同`。版本号在 Task 6 bump 后回填。

---

### Task 6: 桌面补丁版发版 + 人工验收

**REQUIRED SUB-SKILL:** `release-windows`(升 patch 版本号 → 跑绿 → 交叉编译 → objdump 验收 → 签名 → GitHub Release)。**不要凭记忆做**。

- [ ] **Step 1:** 按 `.claude/skills/release-windows/SKILL.md` 执行全流程;提交信息摘要带 `(F344)`。
- [ ] **Step 2:** 回填 Task 5 Step 5 的版本号,与版本提交一起提交。
- [ ] **Step 3:** Release notes 与给用户的人工验收清单(本切片行为应**零变化**,验的是「没坏」):
  1. 启动页项目列表顺序与升级前一致(按最后打开时间倒序;无记录的沉底)。
  2. 启动页 / 项目管理器 / pane 标题条切换弹窗三处搜索:按项目名、目录、节点名、节点 IP 尾数、说明里的词、备忘里的词各搜一次,结果与升级前一致;只靠说明命中的行副标题显示片段。
  3. 项目管理器「在用 / 归档」两 tab 内容正确;搜索时穿透归档、在用排前。
  4. 列表右端相对时间(刚刚 / N 分钟前 / 昨天 …)正常。
  5. 正亮着灯的项目在启动页「在用」里置顶。
  6. 打开一个配了直连+跳板两条节点的项目,仍是直连优先。
