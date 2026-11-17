# F364~F367 实现计划:项目键盘高亮 / Alt 复制改人称 / 覆盖失败告警 / 备忘浮层

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 第一批四条(F364~F367)按设计文档 `docs/superpowers/specs/2026-11-17-f364-f370-keyboard-memo-tunnel-entry-design.md` 落地并发 v0.1.144。

**Architecture:** 判据一律抽纯函数单测(`project_nav` / `clipboard` / `files::fail` / `files::queue` / `memo_popup` 的出口枚举);egui 部分用 0.30 headless `ctx.run` harness 测绘制结果;新弹窗按 T8 登记 `Modal` 全表并被完备性测试兜住;store 只新增字段级 mutator(同 `set_project_archived` 姿态:先 `sync_from_disk_if_untouched`、不 validate)。

**Tech Stack:** Rust、egui 0.30、mullion-store(同步 IO)。

---

## 文件结构

| 文件 | 职责 | 任务 |
|---|---|---|
| `crates/mullion-app/src/clipboard.rs` | `to_first_person` 纯函数 | 1 |
| `crates/mullion-app/src/app.rs` `copy_selection_one_line` | 接 F365 + Warn toast | 1 |
| `crates/mullion-app/src/ui/theme.rs` | `SEL_BAR_W` 共用、`paint_keyboard_selection` | 2 |
| `crates/mullion-app/src/ui/files_panel.rs` | 改用 theme 的共用画法(行为不变) | 2 |
| `crates/mullion-app/src/ui/project_row.rs` | 选中行改画 accent 底 + 竖条 | 2 |
| `crates/mullion-app/src/ui/project_pick.rs` | 打开即聚焦 | 3 |
| `crates/mullion-app/src/ui/launcher.rs`、`ui/mod.rs` | 进入启动页聚焦一次(解锁框/模态让位) | 4 |
| `crates/mullion-app/src/ui/project_manager.rs` | 左栏 ↑↓/Enter,脏草稿不切 | 5 |
| `crates/mullion-app/src/ui/project_nav.rs` | 抽出 `enter_pressed`(Commit 闸)| 5、9 |
| `crates/mullion-app/src/files/fail.rs` | `os_code` / `overwrite_reason` | 6 |
| `crates/mullion-app/src/files/queue.rs` | `remove(id)` | 6 |
| `crates/mullion-app/src/ui/overwrite_alert.rs`(新) | 告警框 UI + 合并规则 | 7 |
| `crates/mullion-app/src/app.rs` | `Modal::OverwriteAlert` 登记 + TransferDone 接线 + 重试 | 7 |
| `crates/mullion-store/src/vault.rs`、`mullion-app/src/shell/store.rs` | 备忘字段级 mutator | 8 |
| `crates/mullion-app/src/hotkeys.rs`、`ui/settings.rs`、`ui/shortcuts.rs` | 第 8 个动作 `ProjectMemo` | 9 |
| `crates/mullion-app/src/ui/memo_popup.rs`(新) | 备忘浮层 UI | 10 |
| `crates/mullion-app/src/app.rs` | `Modal::MemoPopup` 登记 + 热键分发 + 出口施加 | 11 |
| `spec.md`、版本号 | F343 行注释、发版 | 12 |

---

### Task 1: F365 `to_first_person` + 接线

**Files:** Modify `crates/mullion-app/src/clipboard.rs`、`crates/mullion-app/src/app.rs`(`copy_selection_one_line`,约 9885 行)

- [ ] **Step 1: 失败测试**(`clipboard.rs` 末尾新增 `#[cfg(test)] mod tests`,已有则追加)

```rust
#[test]
fn every_ni_and_nin_becomes_wo_and_nothing_else_moves() {
    assert_eq!(to_first_person("需要你拍板的"), ("需要我拍板的".to_string(), 1));
    assert_eq!(to_first_person("请您确认,你们呢"), ("请我确认,我们呢".to_string(), 2));
    assert_eq!(to_first_person("git commit -m x"), ("git commit -m x".to_string(), 0));
    assert_eq!(to_first_person(""), (String::new(), 0));
}
```

- [ ] **Step 2:** `cargo test -p mullion-app clipboard::` → 编译失败(函数不存在)。
- [ ] **Step 3: 实现**

```rust
/// F365:Alt+拖选复制时把「你」「您」逐字换成「我」,其余一字不动。返回替换次数。
///
/// 放在 app 层而不是 `mullion-term`:单行拼接是终端语义(有 fixture 守着),
/// 人称替换是产品策略。不做反向「我→你」、不做语义判断(「迷你」也会改,
/// 用户 2026-11-17 复议后维持一律替换,由调用方飘 Warn toast 兜底)。
pub fn to_first_person(s: &str) -> (String, usize) {
    let mut n = 0;
    let out = s
        .chars()
        .map(|c| match c {
            '你' | '您' => {
                n += 1;
                '我'
            }
            c => c,
        })
        .collect();
    (out, n)
}
```

- [ ] **Step 4: 接线** `copy_selection_one_line`:

```rust
let (text, swapped) = crate::clipboard::to_first_person(&text);
self.clipboard.set(&text);
if swapped > 0 {
    self.ui.set_toast(crate::ui::toast::Kind::Warn, format!("已将 {swapped} 处你/您改为我"));
} else if merged {
    self.ui.set_toast(crate::ui::toast::Kind::Ok, "已复制为单行");
}
```

并在 `alt_is_read_at_press_and_handed_back_on_every_release` 里追加断言 `copy.contains("to_first_person(")`(自证:删掉那一行接线 → 红)。

- [ ] **Step 5:** `cargo test -p mullion-app clipboard:: alt_is_read_at_press` 通过 → 提交 `feat(app): Alt 拖选复制把你/您改为我 (F365)`。

---

### Task 2: F364 选中行高亮 = accent 底 + 左缘竖条

**Files:** `ui/theme.rs`、`ui/files_panel.rs:505,2234`、`ui/project_row.rs:375`

- [ ] **Step 1: 失败测试**(`project_row.rs` tests):把 harness `row_shapes_with` 加一个 `selected: bool` 参数(既有调用点传 `false`),新增:

```rust
/// F364:选中行要有 accent 竖条 + accent 薄底 —— 旧的 sunken_bg 与悬停分不开。
/// 自证会变红:把 show 里的 selected 分支改回 row_bg(row.selected, ..)。
#[test]
fn a_selected_row_paints_an_accent_bar_and_an_accent_fill() {
    let t = Theme::default();
    let shapes = row_shapes_with(None, None, true);
    let rects = filled_rects(&shapes); // (rect, color)
    assert!(rects.iter().any(|(r, c)| *c == theme::c32(t.accent) && r.width() <= theme::SEL_BAR_W + 0.01),
        "选中行没有 accent 竖条");
    assert!(rects.iter().any(|(_, c)| *c == theme::selection_fill(&t)), "选中行没有 accent 薄底");
}
```

(`filled_rects` 复用 `a_lit_row_is_tinted_across_its_whole_width` 里遍历 shapes 的写法抽成测试辅助。)

- [ ] **Step 2:** 跑 → 红。
- [ ] **Step 3: 实现**:`theme.rs` 新增

```rust
/// F150/F364:键盘选中行的竖条宽度(文件面板与三处项目列表共用)。
pub const SEL_BAR_W: f32 = 2.0;

/// F150/F364:键盘选中行 = accent 半透明铺满 + 行首实色竖条。
pub fn paint_selected_row(painter: &egui::Painter, rect: egui::Rect, t: &Theme) {
    painter.rect_filled(rect, 2.0, selection_fill(t));
    painter.rect_filled(
        egui::Rect::from_min_size(rect.left_top(), egui::vec2(SEL_BAR_W, rect.height())),
        0.0,
        c32(t.accent),
    );
}
```

`files_panel.rs` 删私有 `SEL_BAR_W`,原两句改调 `theme::paint_selected_row`(纯搬家,行为不变)。`project_row::show` 中 `row_bg(row.selected, ..)` 改为:

```rust
if row.selected {
    crate::ui::theme::paint_selected_row(ui.painter(), rect, t);
} else if let Some(bg) = crate::ui::session_manager::list::row_bg(false, resp.hovered(), None, t) { .. }
```

(按 `row_bg` 实际返回类型调整;亮灯底 `lit_tint` 仍先画、垫在下面。)

- [ ] **Step 4:** `cargo test -p mullion-app project_row files_panel` 通过 → 提交 `feat(app): 项目列表选中行改 accent 底+竖条 (F364)`。

---

### Task 3: F364 切换项目弹窗打开即聚焦

**Files:** `ui/project_pick.rs`

- [ ] **Step 1: 失败测试**:复制 `arrow_down_then_enter_picks_the_highlighted_project`,**去掉手动 `request_focus`**,命名 `the_search_box_has_focus_the_moment_the_dialog_opens`:首帧后断言 `ctx.memory(|m| m.has_focus(Id::new("project_pick_search")))`,再 ↓+Enter 能选中。另加 `focus_is_requested_only_once`:首帧后 `ctx.memory_mut(|m| m.surrender_focus(id))`,再跑一帧 → 断言不再持焦点。
- [ ] **Step 2:** 跑 → 红。
- [ ] **Step 3:** `ProjectPickDraft` 加 `focus_pending: bool`(`new` 里为 `true`),画完搜索框后 `if std::mem::take(&mut d.focus_pending) { r.request_focus(); }`(`r` 为 `search_box` 返回的 `Response`)。
- [ ] **Step 4:** 测试通过 → 提交 `feat(app): 切换项目弹窗打开即聚焦搜索框 (F364)`。

---

### Task 4: F364 启动页进入时聚焦一次(解锁框开着不抢)

**Files:** `ui/launcher.rs`、`ui/mod.rs`(UiState、UiFrame、build_ui 约 1252/1270 行)、`app.rs`(UiFrame 构造约 15510 行)

- [ ] **Step 1: 失败测试**(`launcher.rs`):

```rust
#[test]
fn launcher_focus_waits_for_the_unlock_box_and_any_modal_and_fires_once() {
    assert!(should_focus_launcher(false, false, false, false));
    assert!(!should_focus_launcher(true, false, false, false), "已聚焦过");
    assert!(!should_focus_launcher(false, true, false, false), "解锁框开着:主密码会打进搜索框");
    assert!(!should_focus_launcher(false, false, true, false), "有模态开着");
    assert!(!should_focus_launcher(false, false, false, true), "这一帧别人已持焦点");
}
```

外加源码切片守护 `build_ui_hands_the_unlock_state_to_the_launcher_focus_gate`:`body_of(build_ui)` 剥注释后含 `should_focus_launcher(` 且其实参含 `ui_state.unlock.is_some()`。

- [ ] **Step 2:** 跑 → 红。
- [ ] **Step 3: 实现**:

```rust
/// F364:进入启动页时把焦点给搜索框,**只一次**。判据同 `unlock.rs`:
/// 解锁框开着 / 有模态 / 这一帧已有人持焦点都让位 —— 否则同帧后到的
/// request_focus 胜出,主密码明文打进搜索框(复核二)。
pub fn should_focus_launcher(done: bool, unlock_open: bool, modal_open: bool, someone_focused: bool) -> bool {
    !done && !unlock_open && !modal_open && !someone_focused
}
```

UiState 加 `launcher_focused_once: bool`;离开启动页的 `else` 分支与 `launcher_sel` 一起复位;UiFrame 加 `modal_open: bool`,app 侧用 `self.modal_open()` 填;launcher 画完搜索框后:

```rust
if should_focus_launcher(ui_state.launcher_focused_once, unlock_open, modal_open,
                         ui.ctx().memory(|m| m.focused().is_some())) {
    r.request_focus();
    ui_state.launcher_focused_once = true;
}
```

- [ ] **Step 4:** 测试通过 + `cargo test -p mullion-app launcher` → 提交 `feat(app): 启动页进入时聚焦搜索框一次,解锁框让位 (F364)`。

---

### Task 5: F364 项目管理器左栏 ↑↓/Enter

**Files:** `ui/project_manager.rs`、`ui/project_nav.rs`(模块注释)、`spec.md` F343 行

- [ ] **Step 1: 失败测试**(`project_manager.rs` tests,用现有 `proj`/`show` harness;给 `proj` 加节点的变体 `proj_with_node`):
  1. `arrow_down_in_the_search_box_moves_the_selection_and_the_form`:聚焦 `project_manager_search`,按 ↓ → `ui_state.project_selected == Some(第一行 id)` 且 `project_draft` 是那一条。
  2. `arrow_keys_do_not_switch_away_from_a_dirty_draft`:先选中 A、改草稿名,按 ↓ → 仍是 A、草稿保留、`pm_nav_blocked == true`。
  3. `enter_opens_the_highlighted_project_like_the_open_button`:有节点、干净 → Enter 后 `project_open_request == Some((id, None))`。
  4. `enter_does_nothing_when_the_project_cannot_be_opened`:无节点 → `project_open_request == None`。
- [ ] **Step 2:** 跑 → 红。
- [ ] **Step 3: 实现**:
  - 抽纯函数 `fn form_state(stored: Option<&ProjectRecord>, draft: &ProjectRecord) -> (bool /*dirty*/, bool /*openable*/)`,`form_column` 改调它(判据不复制)。
  - `list_column` 把 `rows` 计算挪到搜索框之前;`let mut sel = rows.iter().position(|r| Some(r.id) == ui_state.project_selected);` 调 `project_nav::scan(ctx, search_id, &mut sel, rows.len(), !search.is_empty())`。
  - `nav.moved`:当前草稿脏 → `ui_state.pm_nav_blocked = true`;否则按 `sel` 切 `project_selected` + `project_draft`(同点击那一支,抽成局部闭包/函数共用)并 `pm_nav_blocked = false`。
  - `nav.open`:目标行即为当前选中行时用当前 `(dirty, openable)`;目标行非当前选中时以 `stored` 判 `openable`(无脏草稿前提下)→ 选中并发 `project_open_request = Some((id, None))`。
  - `form_column` 的「有未保存的改动」提示:`pm_nav_blocked` 为真时用 `t.warn` 色;草稿变干净时复位。
  - 行 `selected: Some(p.id) == ui_state.project_selected` 已有,画法随 Task 2 自动生效。
  - 改 `project_nav.rs` 模块注释第 4 行;`spec.md` F343 行「项目管理器不动」改为「项目管理器左栏由 F364 接入」。
- [ ] **Step 4:** `cargo test -p mullion-app project_manager project_nav` 通过 → 提交 `feat(app): 项目管理器左栏 ↑↓ 切换、回车打开,脏草稿不切 (F364)`。

---

### Task 6: F366 失败原因翻译 + 队列单条摘除

**Files:** `files/fail.rs`、`files/queue.rs`

- [ ] **Step 1: 失败测试**

```rust
// fail.rs
#[test]
fn the_os_error_suffix_std_prints_is_what_we_parse() {
    // 钉住 std 的固定格式:io::Error 的 Display 以 "(os error N)" 结尾。
    let e = std::io::Error::from_raw_os_error(32);
    assert_eq!(os_code(&format!("写不了本地文件:{e}")), Some(32));
    assert_eq!(os_code("No such file: No such file"), None);
}
#[test]
fn sharing_violation_and_access_denied_are_translated_others_kept_verbatim() {
    assert!(overwrite_reason("x (os error 32)").contains("被其他程序占用"));
    assert!(overwrite_reason("x (os error 33)").contains("被其他程序占用"));
    assert!(overwrite_reason("x (os error 5)").contains("没有写入权限"));
    assert_eq!(overwrite_reason("磁盘满了 (os error 112)"), "磁盘满了 (os error 112)");
}
// queue.rs
#[test]
fn remove_only_takes_finished_jobs_out() {
    let mut q = Queue::new(2);
    let a = q.push(new_job("a")); let b = q.push(new_job("b"));
    q.take_runnable();
    q.finish(a, Err("boom".into()));
    q.remove(a); q.remove(b);
    assert!(q.get(a).is_none(), "终态应被摘掉");
    assert!(q.get(b).is_some(), "在跑的不许摘");
}
```

- [ ] **Step 2:** 跑 → 红。
- [ ] **Step 3: 实现**

```rust
/// F366:从已格式化的 `io::Error` 文案里取 OS 错误码。std 的 Display 固定以
/// `(os error N)` 结尾 —— 不改 worker 签名,在这里解析(测试钉住该格式)。
pub fn os_code(msg: &str) -> Option<i32> {
    let start = msg.rfind("(os error ")? + "(os error ".len();
    let end = start + msg[start..].find(')')?;
    msg[start..end].trim().parse().ok()
}

/// F366:覆盖失败给人看的原因。32/33 = Windows 共享/锁定冲突(Excel/WPS 独占),
/// 5 = 拒绝访问;其它原样(Windows 上已是 FormatMessageW 的本地化文本)。
pub fn overwrite_reason(msg: &str) -> String {
    match os_code(msg) {
        Some(32 | 33) => "文件正被其他程序占用(例如 Excel),关掉它后重试".into(),
        Some(5) => "没有写入权限(文件可能是只读的)".into(),
        _ => msg.to_string(),
    }
}
```

```rust
/// F366:摘掉**一条已收尾**的 job(重试时把旧的 Failed 换下来,否则 F308
/// 「有 Failed 永不自动收」使传输栏永不收起)。没收尾的不动。
pub fn remove(&mut self, id: u64) {
    self.jobs.retain(|j| j.id != id || !j.state.is_finished());
}
```

- [ ] **Step 4:** 通过 → 提交 `feat(app): 覆盖失败原因翻译与队列单条摘除 (F366)`。

---

### Task 7: F366 告警框 UI + Modal 登记 + TransferDone 接线 + 重试

**Files:** Create `ui/overwrite_alert.rs`;Modify `ui/mod.rs`(mod、UiState 字段 `overwrite_alert: Option<OverwriteAlert>`、build_ui 绘制、actions 字段)、`app.rs`

- [ ] **Step 1: 失败测试**(`overwrite_alert.rs`):

```rust
#[test]
fn failures_in_one_batch_merge_into_one_alert() {
    let mut a: Option<OverwriteAlert<u32>> = None;
    push_failure(&mut a, "a.xlsx".into(), "x (os error 32)", 1);
    push_failure(&mut a, "b.xlsx".into(), "x (os error 5)", 2);
    let a = a.unwrap();
    assert_eq!(a.items.len(), 2);
    assert!(a.items[0].reason.contains("占用"));
}
#[test]
fn the_list_folds_after_the_limit() {
    let lines = visible_lines(&(0..12).map(|i| format!("f{i}")).collect::<Vec<_>>());
    assert_eq!(lines.last().unwrap(), "等另外 4 个");  // VISIBLE = 8
}
```

`OverwriteAlert<S>` 泛型于 payload(app 侧放 `TransferSpec`),`items: Vec<Item<S>>{ label, reason, spec }`;`show(ctx, t, &mut Option<OverwriteAlert<S>>) -> Option<AlertOut>`,`AlertOut::{Retry, Close}`;窗口标题常量 `WINDOW_TITLE = "覆盖失败"`。文字只用 GBK 内字符(T9:`tests/glyph_whitelist.rs` 会扫)。

- [ ] **Step 2:** 红 → 实现 → 绿。
- [ ] **Step 3: Modal 登记**(app.rs):新增 `Modal::OverwriteAlert`,逐处补:enum + doc(T8:按钮激活键)、`Modal::ALL`、`modal_open` 臂(`self.ui.overwrite_alert.is_some()`)、`draft_baseline_is_in_vault`(false)、`dismiss_areas`(`None`,豁免)、`dismiss_dirty`(false)、`dismiss`(`{}`)、`DISMISS_EXEMPT`(理由:点外面关掉 = 静默丢掉重试机会)、`every_modal_variant_is_listed_in_all` 的 match 臂 + 循环列表、`the_dismiss_order_is_the_exact_reverse_of_the_paint_order` 的 marker(`""`)。不进 `touched_store`。
- [ ] **Step 4: TransferDone 接线**:在 `self.transfer.queue.finish(job, result)` 之前取 `let overwrite_download = queue.get(job).is_some_and(|j| j.dir == Download && j.resolved == Some(Conflict::Overwrite));`;F279 的 `set_error` 加条件 `!overwrite_download`;`finish` 之后若 `overwrite_download && Err(m)`(且非哨兵,复用 `should_pop_transfer_error`)且 `specs.get(&job)` 存在 → `push_failure(&mut self.ui.overwrite_alert, label, m, (job, spec.clone()))`。纯函数 `fn overwrite_alert_applies(dir, resolved, &result) -> bool` 抽出单测(上传/非覆盖/哨兵都为 false)。
- [ ] **Step 5: 重试**:`apply_overwrite_alert(AlertOut)`:`Retry` → 对每个 item:`queue.remove(old_id)`;`let id = queue.push(NewJob{dir, generation, label, total})`(total 取旧 job 的 total,item 里同存);`queue.set_resolved(id, Conflict::Overwrite)`;`specs.insert(id, spec)`;关框;`Close` → 关框。属主标签已关(`tabs.by_generation(gen).is_none()`)的条目跳过,同 `enqueue_paste_files`。
- [ ] **Step 6:** `cargo test -p mullion-app` 相关(`overwrite_alert`、`every_modal`、`dismiss_order`、`glyph_whitelist`)通过 → 提交 `feat(app): 下载覆盖失败弹窗告警 + 重试 (F366)`。

---

### Task 8: F367 store 字段级备忘 mutator

**Files:** `mullion-store/src/vault.rs`、`mullion-app/src/shell/store.rs`

- [ ] **Step 1: 失败测试**(vault.rs,仿 `archiving_flips_only_the_archive_field_and_can_be_undone`):

```rust
#[test]
fn memo_mutators_touch_only_their_own_field_and_skip_validation() {
    let dir = tempfile::tempdir().unwrap();
    let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
    let id = v.add_project("web".into(), "/srv/web".into(), "2026-11-01T00:00:00Z");
    let before = v.projects()[0].clone();
    v.add_project_memo(id, "上线前清缓存".into(), "2026-11-17T08:00:00Z");
    v.add_project_memo(id, "周五备份".into(), "2026-11-17T09:00:00Z");
    let p = v.projects()[0].clone();
    assert_eq!(p.memos.len(), 2);
    v.remove_project_memo(id, &p.memos[0]);
    assert_eq!(v.projects()[0].memos.len(), 1);
    assert_eq!(v.projects()[0].memos[0].text, "周五备份");
    v.set_project_note(id, "长期说明".into());
    let after = v.projects()[0].clone();
    assert_eq!(ProjectRecord { memos: vec![], note: String::new(), ..after }, before, "只许动 memos/note");
}
```

- [ ] **Step 2:** 红 → 实现三个方法(各自先 `sync_from_disk_if_untouched()`、按 id 找 slot、不 validate;`remove_project_memo` 按 `text + created_at` 全等删第一条)。`shell/store.rs` 加三个薄封装。
- [ ] **Step 3:** `cargo test -p mullion-store` 通过 → 提交 `feat(store): 项目备忘字段级增删与 note 写入 (F367)`。

---

### Task 9: F367 抽出 Commit 闸 + 第 8 个热键动作

**Files:** `ui/project_nav.rs`、`hotkeys.rs`、`ui/settings.rs`、`ui/shortcuts.rs`、`app.rs`(`bound_hotkey_event`)

- [ ] **Step 1:** `project_nav.rs` 抽 `pub fn take_enter(i: &mut egui::InputState) -> bool`(同帧有 `ImeEvent::Commit` → false 且不消费;否则 `consume_key(NONE, Enter)`),`scan` 改调它;新测试 `enter_in_the_same_frame_as_an_ime_commit_is_not_taken`(构造含 Commit + Enter 的 RawInput,断言返回 false 且 Enter 事件仍在)。
- [ ] **Step 2:** `hotkeys.rs`:`Action::ProjectMemo`,`name = "project_memo"`,默认 `ctrl(true, KeyName::Char('m'))`,`ALL: [Action; 8]`,所有写死「7」的注释/文档改「8」(`grep -n "7 条\|7 个\|; 7\]" hotkeys.rs ui/settings.rs ui/shortcuts.rs`);设置页标签「项目备忘」;`shortcuts.rs` 加 `Keys::Bound(Action::ProjectMemo)` 一行。跑已有 hotkeys 测试(默认键不重复、名字唯一),补 `project_memo_defaults_to_ctrl_shift_m`。
- [ ] **Step 3:** `bound_hotkey_event` 加臂:`Action::ProjectMemo => { if self.effective_focus() == Focus::FilesPanel { return false; } self.apply_memo_hotkey(); }`;并把 36100 行附近那张「动作 → 调用」表补上 `Action::ProjectMemo => "self.apply_memo_hotkey()"`。`apply_memo_hotkey` 本任务先留空壳实现于 Task 11。
- [ ] **Step 4:** 测试通过 → 提交 `feat(app): 新增项目备忘热键 Ctrl+Shift+M,抽出组字回车闸 (F367)`。

---

### Task 10: F367 备忘浮层 UI

**Files:** Create `ui/memo_popup.rs`

- [ ] **Step 1: 失败测试**:
  - `the_input_has_focus_on_open_and_enter_adds_a_memo`:首帧聚焦 `memo_popup_input`,输入「abc」+ Enter → 出口 `Some(MemoOut::Add("abc"))`,输入框清空。
  - `enter_while_composing_does_not_add`:同帧 Commit + Enter → 出口 None(复用 `take_enter`)。
  - `blank_input_adds_nothing`。
  - `memos_are_listed_newest_first`(纯函数 `ordered(&[Memo]) -> Vec<&Memo>`)。
  - `the_note_is_folded_by_default_and_its_dirtiness_is_tracked`(`MemoDraft::note_dirty()`)。
- [ ] **Step 2:** 红 → 实现:`MemoDraft { pane, project, title, memos(快照), input, note, note_baseline, note_open, focus_pending, error }`;`show(ctx, t, &mut Option<MemoDraft>, rect) -> Option<MemoOut>`;`MemoOut::{Add(String), Remove(Memo), SaveNote(String), Close}`;`area_id(pane)`;Esc → 若 `note_dirty` 不关(提示「备注有未保存的改动」)否则 `Close`;每条备忘行末一个删除按钮(`ui::icon` 自绘叉,T9)。
- [ ] **Step 3:** 绿 → 提交 `feat(app): 项目备忘浮层 (F367)`。

---

### Task 11: F367 Modal 登记 + app 接线

**Files:** `ui/mod.rs`、`app.rs`

- [ ] **Step 1: 失败测试**:
  - 纯函数 `memo_target(tmux: Option<&str>, projects) -> MemoTarget::{Open(ProjectId), NotReported, NoProject}`(`project.rs`)三分支单测。
  - Modal 完备性两条测试先补 `Modal::MemoPopup` 臂 → 编译红。
- [ ] **Step 2: 登记**:`Modal::MemoPopup`(doc:T8 输入框)、`ALL`、`modal_open`、`draft_baseline_is_in_vault`(true:note 基线在库)、`dismiss_areas`(`area_id(pane)`)、`dismiss_dirty`(`note_dirty()`)、`dismiss`(`= None`)、`DISMISS_ORDER`(按 build_ui 绘制位置:画在 `project_pick::show` 之后 → 排在 `Modal::ProjectPick` 之前)、两条完备性测试的臂/marker(`"memo_popup::show("`)。进 `touched_store`(写项目)。
- [ ] **Step 3: 热键施加** `apply_memo_hotkey`:取焦点 pane 的 tmux 与 projects → `memo_target`;`NotReported` → toast「这块分屏还没识别到 tmux 会话,稍后再试」;`NoProject` → toast「这块分屏不属于任何项目」;`Open(id)` → `self.ui.memo_popup = Some(MemoDraft::new(pane, project))`。焦点不在 pane(无终端标签)→ 无动作。
- [ ] **Step 4: 出口施加**(在项目意图施加点附近):`Add` → `store.add_project_memo(id, text, &now_rfc3339())` + `save()`;`Remove` → `remove_project_memo`;`SaveNote` → `set_project_note`;成功后刷新草稿快照(memos / note_baseline);`save()` 失败 → `draft.error = Some(..)` 浮层内联红字;`Close` → `None`。
- [ ] **Step 5:** build_ui 里在 `project_pick::show` 之后绘制,rect 复用 pick_rect 的算法(抽 `pane_rect(ctx, frame, pane)` 两处共用)。
- [ ] **Step 6:** `cargo test --workspace` 相关通过 → 提交 `feat(app): 备忘浮层接入热键与落盘 (F367)`。

---

### Task 12: 收尾

- [ ] `cargo fmt --check`;`cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log`;`cargo clippy --workspace --all-targets -- -D warnings`。
- [ ] 按 `.claude/skills/release-windows/SKILL.md` 发 v0.1.144,附人工验收清单(设计文档「无法自动验证」F364/F366/F367 三条 + F366 原地截断边界 + F367「还在连接」提示)。
- [ ] 记 memory 切片记录。
