# 终端 Ctrl+V 粘贴文件 / `@` 前缀 / 焦点光标闪烁 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在终端 pane 里 Ctrl+V 能粘贴资源管理器复制的文件(传到节点 `/tmp`,同名先确认,传完把 `@路径` 打进输入行);截图路径同样加 `@`;焦点 pane 里 Claude Code 的自绘假光标会闪。

**Architecture:** 纯逻辑(`@` 引用格式化、冲突判定、批次汇合)放新模块 `paste_files.rs`,可无窗口单测;传输复用全局 `Queue`(底栏面板已对所有标签可见,**不加状态栏指示**);假光标闪烁在渲染层(`gpu.rs` 色块 + `text.rs` 字色)按相位切换,并接进 F159 整帧指纹。

**Tech Stack:** Rust、russh sftp(`SftpClient::exists`)、egui 弹窗(`FilesDialog`)、现有 `files::queue`。

**spec 编号(新开,先在 `spec.md` 登记):** F330 终端粘贴文件;F331 `@` 前缀(截图+文件);F332 文件粘贴走队列进度、传完才回填;F333 焦点 pane 假光标闪烁。

## 已锁定的决策(来自 grilling,勿再改)

1. 目标目录 = `tab.sftp_screenshot_dir`(空则 `shot::DEFAULT_DIR`=`/tmp`),截图与文件共用。
2. 多文件支持;文件夹本期跳过并 toast「已跳过文件夹 X」。
3. 上传前对每个目标 `exists`;有冲突弹「全部覆盖 / 取消」,确认前整批不开传。
4. 输出 `@绝对路径`,多个空格分隔,末尾留一个空格,不带回车;含空白的路径**不加 `@`、回填裸绝对路径**并弹 Warn toast(已核实:Claude Code 的 `@` 在首个空格处中止,双引号与反斜杠转义均不支持,见 issue #80553,二手来源)。
5. 复用全局队列(可取消、无大小上限);**整批全部成功后**才一次性回填路径,失败/取消则一个都不回填。截图维持 20 MiB 上限、不加进度。
6. 不做非焦点 pane 压暗。
7. 假光标闪烁:焦点 pane + 真光标被远端隐藏(`shape==Hidden`)+ 光标格是反显(`bg==defaults.fg`)→ 闪烁灭相位把该格画成普通格。非焦点 pane 保持静态。
8. 一次做完,发一个 patch 版本。

## 已核实的事实(写计划时读过代码)

- 裸 Ctrl+V 分流在 `app.rs:~14150`(`shot::clip_paste(has_text, has_image)`);截图上传 `paste_screenshot`(`app.rs:~9725`)、回填 `accept_shot_uploaded`(`app.rs:~9805`,`out.push(b' ')` 那段)。
- `dragout::win::read()` 返回 `ClipRead`,其 `.hdrop: Vec<PathBuf>` 是 `CF_HDROP`(`app.rs:7145` 文件面板已在用)。它走独立线程、最长阻塞 3 秒。
- 队列是应用全局 `self.transfer.queue`;`transfer_panel::show` 对所有标签画,队列非空即显示。终端标签有 `sftp_connection_for(host_ix)`。
- `Queue::push(NewJob)`;`Queue::get_mut` 私有;`Job.resolved: Option<Conflict>`;worker(`app.rs:~1703`)见 `resolved==Some(Overwrite)` 即覆盖,`None` 且目标存在 → `JobError::Conflict`。
- `TransferDone{job,result}`(`app.rs:~13480`)是每条 job 收工的唯一汇合点;`TransferSpec` 需要 `arrival`。
- **Claude Code 光标成因已由现有 fixture 证实**:`crates/mullion-term/tests/fixtures/claude-code-input-cursor.bin` 全程 `?25l`,输入位置是一格 SGR 7 反显;`Cursor.shape==Hidden`,反显在 emulator 里**已解析成交换后的 fg/bg**(`snapshot` 没有 INVERSE 标志)。
- F159 整帧指纹 `frame_fp::hash_pane` 吃 `style_for(...)` 结果;任何只在渲染层变化的光标相位都必须进指纹,否则整帧被跳过、闪烁看不见(静默)。

## 文件结构

- Create: `crates/mullion-app/src/paste_files.rs` — 纯逻辑:`at_ref`、`at_refs_line`、`split_picked`、`conflicting`、`Batch`/`BatchTracker`。
- Modify: `crates/mullion-app/src/lib.rs`(`pub mod paste_files;`)
- Modify: `crates/mullion-app/src/shot.rs` — `ClipPaste::Files`、`clip_paste` 三参。
- Modify: `crates/mullion-app/src/app.rs` — Ctrl+V 分流、`paste_files` 发起/预检/入队、`TransferDone` 汇合、`accept_shot_uploaded` 加 `@`。
- Modify: `crates/mullion-app/src/files/queue.rs` — `Queue::set_resolved`。
- Modify: `crates/mullion-app/src/ui/files_dialog.rs` — 新变体 `PasteFilesConflict`。
- Modify: `crates/mullion-app/src/gpu.rs`、`text.rs`、`frame_fp.rs` — 假光标。
- Modify: `spec.md`、`docs/` 手验清单。

---

### Task 0: 登记 spec 编号(`@` 含空格写法已核实:不支持引号/转义,走方案 A)

**Files:** Modify `spec.md`(F330~F333 四行,格式照 F326 附近)。

- [x] **Step 1:** 已核实并经用户决策(方案 A):含空白的路径回填裸路径 + Warn toast,不加 `@`。
- [ ] **Step 2:** 在 `spec.md` 登记 F330~F333(需求一句话 + 守护测试占位名,测试名在各任务里已定)。
- [ ] **Step 3:** `git add spec.md && git commit -m "docs(spec): 登记 F330~F333 终端粘贴文件/@ 前缀/进度/焦点光标闪烁"`

### Task 1: 纯逻辑模块 `paste_files.rs`

**Files:** Create `crates/mullion-app/src/paste_files.rs`;Modify `lib.rs`。

- [ ] **Step 1: 写失败测试**(文件末尾 `#[cfg(test)]`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// F331:含空白的路径**不加 `@`**。Claude Code 的 `@` 在首个空格处中止,
    /// 引号/转义都不支持 —— 加了 `@` 只会静默引用到半截路径。
    /// 自证会变红:把 `at_ref` 里「含空白就不加 @」那支删掉。
    #[test]
    fn a_path_with_spaces_gets_no_at_because_claude_code_would_cut_it() {
        assert_eq!(at_ref("/tmp/a.png"), "@/tmp/a.png");
        assert_eq!(at_ref("/tmp/my file.png"), "/tmp/my file.png");
    }

    /// F331:多个之间一个空格,末尾再留一个,**不带回车**;并报出哪些没带 `@`。
    #[test]
    fn refs_line_is_space_separated_with_one_trailing_space_and_reports_bare_paths() {
        let (l, bare) = at_refs_line(&["/tmp/a".into(), "/tmp/b c".into()]);
        assert_eq!(l, "@/tmp/a /tmp/b c ");
        assert!(!l.contains('\n') && !l.contains('\r'));
        assert_eq!(bare, vec!["/tmp/b c".to_string()]);
    }

    /// F330:文件夹跳过且要报名字;不能静默。
    #[test]
    fn directories_are_skipped_and_named() {
        let picked = vec![
            Picked { name: "a.txt".into(), is_dir: false, size: 3 },
            Picked { name: "d".into(), is_dir: true, size: 0 },
        ];
        let (files, skipped) = split_picked(picked);
        assert_eq!(files.len(), 1);
        assert_eq!(skipped, vec!["d".to_string()]);
    }

    /// F330:只报**确实存在**的目标名,顺序保持。
    #[test]
    fn conflicts_keep_order_and_only_name_existing_targets() {
        let names = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let existing: std::collections::HashSet<String> = ["c".into(), "a".into()].into();
        assert_eq!(conflicting(&names, &existing), vec!["a".to_string(), "c".to_string()]);
    }

    /// F332:全部成功才给出回填;缺一个都不给。
    #[test]
    fn a_batch_fills_back_only_when_every_job_succeeded() {
        let mut t = BatchTracker::default();
        t.open(7, 1, 10, vec![(100, "/tmp/a".into()), (101, "/tmp/b".into())]);
        assert_eq!(t.job_finished(100, true), None);
        assert_eq!(
            t.job_finished(101, true),
            Some(BatchDone::Fill { generation: 1, pane: 10, paths: vec!["/tmp/a".into(), "/tmp/b".into()] })
        );
    }

    /// F332:任一条失败/取消 → 整批不回填,且只报一次。
    #[test]
    fn one_failed_job_means_no_path_is_filled_in() {
        let mut t = BatchTracker::default();
        t.open(7, 1, 10, vec![(100, "/tmp/a".into()), (101, "/tmp/b".into())]);
        assert_eq!(t.job_finished(100, false), None);
        assert_eq!(t.job_finished(101, true), Some(BatchDone::Abandoned { failed: 1 }));
        assert_eq!(t.job_finished(101, true), None, "批次已结算,不重复触发");
    }
}
```

- [ ] **Step 2:** `cargo test -p mullion-app paste_files` → 预期编译失败(模块未定义)。
- [ ] **Step 3: 最小实现**

```rust
//! F330~F332:终端 Ctrl+V 粘贴文件的纯逻辑。
//!
//! 不碰 UI、网络、窗口 —— 冲突判定、`@` 引用格式化、批次汇合都能无头单测。

use std::collections::{HashMap, HashSet};

/// 剪贴板里一个本地条目(调用处已 stat 过)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picked {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

/// Claude Code 的 `@` 引用。含空白的路径**不加 `@`**:`@` 解析在首个空格处中止,
/// 双引号与反斜杠转义均不支持(issue #80553)—— 加了反而静默引用错文件。
pub fn at_ref(path: &str) -> String {
    if path.chars().any(char::is_whitespace) {
        path.to_string()
    } else {
        format!("@{path}")
    }
}

/// 多个引用空格分隔,末尾留一个空格,不带回车:接着打字是自然的下一句话。
/// 第二个返回值是**没能带 `@` 的路径**,调用方据此弹 Warn toast,不静默。
pub fn at_refs_line(paths: &[String]) -> (String, Vec<String>) {
    let bare: Vec<String> = paths
        .iter()
        .filter(|p| p.chars().any(char::is_whitespace))
        .cloned()
        .collect();
    let mut s = paths.iter().map(|p| at_ref(p)).collect::<Vec<_>>().join(" ");
    s.push(' ');
    (s, bare)
}

/// 拆成「要传的文件」与「被跳过的文件夹名」。
pub fn split_picked(all: Vec<Picked>) -> (Vec<Picked>, Vec<String>) {
    let mut files = Vec::new();
    let mut skipped = Vec::new();
    for p in all {
        if p.is_dir {
            skipped.push(p.name);
        } else {
            files.push(p);
        }
    }
    (files, skipped)
}

pub fn conflicting(names: &[String], existing: &HashSet<String>) -> Vec<String> {
    names.iter().filter(|n| existing.contains(*n)).cloned().collect()
}

#[derive(Debug, PartialEq, Eq)]
pub enum BatchDone {
    Fill { generation: u64, pane: u64, paths: Vec<String> },
    Abandoned { failed: usize },
}

struct Batch {
    generation: u64,
    pane: u64,
    /// (job id, 远端绝对路径),保持剪贴板顺序。
    jobs: Vec<(u64, String)>,
    finished: HashSet<u64>,
    failed: usize,
}

/// 批次汇合。**只看 `TransferDone`**(T11 同族:计时/判定不从调用点起算,
/// 唯一可靠的是收工事件)。
#[derive(Default)]
pub struct BatchTracker {
    batches: HashMap<u64, Batch>,
    by_job: HashMap<u64, u64>,
}

impl BatchTracker {
    pub fn open(&mut self, batch: u64, generation: u64, pane: u64, jobs: Vec<(u64, String)>) {
        for (j, _) in &jobs {
            self.by_job.insert(*j, batch);
        }
        self.batches.insert(
            batch,
            Batch { generation, pane, jobs, finished: HashSet::new(), failed: 0 },
        );
    }

    pub fn job_finished(&mut self, job: u64, ok: bool) -> Option<BatchDone> {
        let bid = *self.by_job.get(&job)?;
        let b = self.batches.get_mut(&bid)?;
        if !b.finished.insert(job) {
            return None;
        }
        if !ok {
            b.failed += 1;
        }
        if b.finished.len() < b.jobs.len() {
            return None;
        }
        let b = self.batches.remove(&bid)?;
        for (j, _) in &b.jobs {
            self.by_job.remove(j);
        }
        Some(if b.failed == 0 {
            BatchDone::Fill {
                generation: b.generation,
                pane: b.pane,
                paths: b.jobs.into_iter().map(|(_, p)| p).collect(),
            }
        } else {
            BatchDone::Abandoned { failed: b.failed }
        })
    }

    /// 标签关闭:该世代的批次整体作废(`wind_down` 直接 abort task,
    /// `TransferDone` 可能永远不来,不回收就一直堆着)。
    pub fn drop_generation(&mut self, generation: u64) {
        let dead: Vec<u64> = self
            .batches
            .iter()
            .filter(|(_, b)| b.generation == generation)
            .map(|(k, _)| *k)
            .collect();
        for k in dead {
            if let Some(b) = self.batches.remove(&k) {
                for (j, _) in b.jobs {
                    self.by_job.remove(&j);
                }
            }
        }
    }
}
```

  `lib.rs` 加 `pub mod paste_files;`(在 `pub mod shot;` 旁)。`pane` 用 `u64` 存(调用处 `PaneId` 转换时按 `app.rs` 里 `PaneId` 的实际内层类型写 `.0 as u64`;若 `PaneId` 是 `Copy` 的 newtype 且在 lib 可见,直接把字段类型换成 `mullion_core::layout::PaneId`,测试里同步替换 — 以编译器签名为准,别猜)。
- [ ] **Step 4:** `cargo test -p mullion-app paste_files` → 6 条全绿。
- [ ] **Step 5:** 加一条守护「标签关闭回收」`drop_generation` 的测试(开批次→drop→再 `job_finished` 得 `None`),通过后提交:`git commit -m "feat(app): 终端粘贴文件的纯逻辑(@ 引用/冲突/批次汇合) (F330 F331 F332)"`

### Task 2: 截图路径加 `@`(F331)

**Files:** Modify `app.rs`(`accept_shot_uploaded`)。

- [ ] **Step 1: 写失败测试** — 在 `app.rs` 测试区加源码切片守护(照 `32761` 附近 `fn_body` 的写法):`accept_shot_uploaded` 函数体必须含 `at_refs_line`,不得再含 `path.clone().into_bytes()`。命名 `shot_backfill_goes_through_at_refs_line`。注意项目教训:源码切片须先确认函数体切对、剥注释方式与同文件既有守护一致(`source-slice-guards-do-not-strip-comments`)。
- [ ] **Step 2:** 跑 → 红。
- [ ] **Step 3:** 把 `let mut out = path.clone().into_bytes(); out.push(b' ');` 换成 `let (line, _bare) = crate::paste_files::at_refs_line(std::slice::from_ref(&path)); let out = line.into_bytes();`(截图文件名是我们生成的、不含空格,`_bare` 恒空),注释同步:「`@` 是 Claude Code 的文件引用语法;末尾空格含在 `at_refs_line` 里」。toast 文案里的路径保持不带 `@`。
- [ ] **Step 4:** 跑 → 绿;`cargo test -p mullion-app` 全 crate 绿。
- [ ] **Step 5:** 提交 `fix(app): 截图回填的路径加 @ 前缀 (F331)`

### Task 3: Ctrl+V 识别「剪贴板里是文件」(F330)

**Files:** Modify `shot.rs`(`ClipPaste`、`clip_paste`)、`app.rs:~14150`。

- [ ] **Step 1: 改测试**(`shot.rs` 已有 `clip_paste` 测试,签名将变,先改测试):

```rust
#[test]
fn files_beat_image_and_text() {
    assert_eq!(clip_paste(true, true, true), ClipPaste::Files);
    assert_eq!(clip_paste(false, false, true), ClipPaste::Files);
}
// 原有四条断言补第三个参数 false,期望不变。
```

  优先级理由写进文档注释:Explorer 复制文件时同时可能带文本(文件名)或缩略图位图,**文件优先**,否则用户拿到的是文件名字符串。
- [ ] **Step 2:** 跑 → 编译失败/红。
- [ ] **Step 3:** `ClipPaste` 加 `Files`;`clip_paste(has_text, has_image, has_files)` 第一条臂 `(_, _, true) => Files`。
- [ ] **Step 4:** `app.rs` 分流处:**三样都读**再交给 `clip_paste`(同 F209 教训,不得「有文件就提前 return」):
  `#[cfg(windows)] let files = crate::dragout::win::read().hdrop; #[cfg(not(windows))] let files: Vec<std::path::PathBuf> = Vec::new();`,`ClipPaste::Files => { self.paste_files(files); self.request_ui_redraw(); return; }`。
  注意:`win::read()` 最长阻塞 3 秒(独立线程);每次裸 Ctrl+V 都会读一次。这是已知代价,在手验清单里点名「Ctrl+V 无卡顿」。
- [ ] **Step 5:** 暂时让 `paste_files` 是空壳 `fn paste_files(&mut self, _files: Vec<std::path::PathBuf>) {}`,保证编译;`cargo test -p mullion-app shot` 绿;提交 `feat(app): Ctrl+V 识别剪贴板里的文件,文件优先于图和文本 (F330)`

### Task 4: 发起 → 预检 → 冲突弹窗 → 入队 → 汇合回填(F330 F332)

**Files:** Modify `app.rs`、`files/queue.rs`、`ui/files_dialog.rs`。这是最大的一块,拆成可独立提交的小步。

- [ ] **Step 1: `Queue::set_resolved`** — `files/queue.rs` 加 `pub fn set_resolved(&mut self, id: u64, c: Conflict)`;单测:push 一条 → `set_resolved(id, Overwrite)` → `get(id).resolved == Some(Overwrite)`。提交。
- [ ] **Step 2: 事件** — `UserEvent` 加 `PasteFilesChecked { generation, pane, dir: RemotePath, items: Vec<Picked + local PathBuf>, skipped_dirs: Vec<String>, existing: Vec<String>, result_err: Option<String> }`;同步加进 `17218` 附近那个按世代路由的 `matches!` 列表(漏了会被当成「不属于任何标签」丢弃,静默)。
- [ ] **Step 3: `paste_files` 发起** — 实现:
  1. 取 `generation`/焦点 pane/`sftp_connection_for(host_ix)`/`dir`(同 `paste_screenshot` 的取法;非终端标签直接 return)。
  2. 本地 `std::fs::metadata` → `Picked`;`split_picked`;若 `skipped` 非空,toast Warn「已跳过文件夹:X、Y」(不静默)。`files` 空则 return(提示「没有可上传的文件」)。
  3. **另开一条 SFTP channel**(同 `paste_screenshot` 的理由:不蹭侧栏那条,F132),`spawn` 一个任务对每个 `remote_join(dir, name)` 调 `client.exists`,收集 `existing`,发 `PasteFilesChecked`;`track_sftp_task`;`user_took_over()`;toast Busy「正在检查目标…」。
- [ ] **Step 4: 预检回来(`accept_paste_files_checked`)** — 按世代路由(标签没了就丢);`existing` 空 → 直接 `enqueue_paste_files(overwrite=false)`;非空 → 开 `FilesDialog::PasteFilesConflict { names, ctx }`,按钮「全部覆盖 / 取消」,文案列出冲突名(最多 8 条 + 「等 N 个」)。`files_dialog.rs` 里同步补三处穷举匹配(`:233` 的 `wants_keyboard` 判定 false、`:254` 的取消处置返回 `None`、`:281` 的标题「文件已存在」),并保证 Esc/遮罩与「取消」同一出口(F203)。**T9:弹窗字符串只用 ASCII/GBK 内符号**,跑 `tests/glyph_whitelist.rs`、`tests/form_guidelines.rs`。
- [ ] **Step 5: `enqueue_paste_files`** — 对每个文件 `queue.push(NewJob{dir:Upload,generation,label:name,total:size})`;`overwrite` 为真时 `set_resolved(id, Overwrite)`;写 `self.transfer.specs.insert(id, TransferSpec{dir:Upload,generation,local,remote,arrival:(dir.clone(),name_path)})`(`arrival` 的语义见 F252 注释,终端标签没有面板可点亮,取目录+文件名即可,以 `TransferSpec` 现有构造点为准);`self.paste_batches.open(batch_id, generation, pane, jobs)`;toast Busy「正在上传 N 个文件,进度见下方传输队列」。`batch_id` 用递增计数。
- [ ] **Step 6: 汇合** — `UserEvent::TransferDone` 臂里,在 `queue.finish(job, result)` 之后调 `self.paste_batches.job_finished(job, result.is_ok())`:
  - `Fill{..}` → 复用 `accept_shot_uploaded` 里回填那段(抽成 `fn backfill_refs(&mut self, generation, pane, paths)`,截图与文件共用,避免两份):`at_refs_line` → `scroll_to_bottom` → `pty.write`;`bare` 非空则 Warn toast「路径含空格,Claude Code 的 @ 引用会在空格处断开,已按普通路径填入:X」;pane 没了则 toast 带路径。
  - `Abandoned{failed}` → toast Warn「有 N 个文件没传成,没有回填路径」。`Skipped`(用户在队列里点了跳过)按失败算。
  - 标签关闭处(`wind_down` 所在路径)调 `drop_generation`。
- [ ] **Step 7: 守护测试**(每条在提交前先变异自证会红,且**先确认变异能编译**):
  - `paste_files_reads_text_image_and_files_before_deciding`(切片守护:分流处同时出现三个读取)。
  - `paste_files_never_enqueues_before_the_conflict_check_returns`(`paste_files` 函数体不得含 `queue.push`)。
  - `backfill_happens_only_from_the_transfer_done_arm`(`BatchDone::Fill` 只在 `TransferDone` 臂消费,T11)。
  - `a_closed_tab_drops_its_paste_batches`(接线)。
- [ ] **Step 8:** `cargo test --workspace` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo fmt --check`,三者全绿才算绿(项目「绿」的定义)。提交 `feat(app): 终端 Ctrl+V 上传文件到节点 /tmp,冲突先确认,传完回填 @ 路径 (F330 F332)`;正文写明跑了 `emulator::tests::pty_write_is_collected` 与 `app::tests::redraw_is_frame_capped`(触到 app.rs 事件循环)。

### Task 5: 焦点 pane 假光标闪烁(F333)

**Files:** Modify `gpu.rs`、`text.rs`、`frame_fp.rs`。**风险最高,先做 spike 再动手。**

- [ ] **Step 1: spike 读代码** — 读 `text.rs::row_to_spans` 与 F12 行指纹整形缓存(`slice-f12-row-fingerprint-diff-shaping`)、F172 行带差分(`bands.rs`)。确认:一格字色/底色变化是否会正确触发该行重整形、行带顶点重建。**结论写进本任务注释;若发现需要给行指纹加输入,在此步补上,否则闪烁只在第一次生效。**
- [ ] **Step 2: 纯函数 + 测试**(`gpu.rs`)

```rust
/// F333:Claude Code 隐掉真光标、用一格 SGR 7 反显自绘输入位置(见 fixture
/// `claude-code-input-cursor`)。反显在 emulator 里已解析成对调后的颜色,
/// 所以判据是「光标格的底色 == 默认前景色」。返回该格坐标。
pub fn fake_cursor_cell(
    snap: &GridSnapshot,
    focused: bool,
    defaults: DefaultColors,
) -> Option<(u16, u16)> { /* shape==Hidden && cursor.visible && cell.bg==defaults.fg && focused */ }
```

  测试三条:焦点+Hidden+反显格 → `Some`;非焦点 → `None`;shape 不是 Hidden(真光标可见)→ `None`(避免与真光标双重处理);该格不是反显 → `None`。再加一条**用真 fixture** 的测试:`Emulator` 喂 `claude-code-input-cursor.bin` 后 `fake_cursor_cell(.., true, ..) == Some((25,4))`(坐标取自 `.snap` 头两行)。
- [ ] **Step 3: 灭相位渲染** — `quads_for_panes` 已有 `blink_on`;当 `!blink_on` 且 `fake_cursor_cell` 命中时,该格的背景 quad 不画(落回默认底);`text.rs` 里该格文字色改回 `defaults.fg`(否则有字符时是深色字落在深底上读不出)。`blink_on` 窗口失焦时恒 `true`(`blink_on_at`),所以失焦时假光标常亮,符合「静态」。
- [ ] **Step 4: 接进指纹(F159)** — `frame_fp::hash_pane` 里再 hash 一位 `fake_cursor_off = fake_cursor_cell(..).is_some() && !blink_on`。**先写红测试**:`the_fake_cursor_blink_phase_changes_the_fingerprint`(同一帧只翻 `blink_on`,指纹必须不同);并保留既有 `only_the_focused_pane_churns_when_the_blink_phase_flips`(非焦点 pane 翻相位指纹不能变 → 无空闲帧,T3)。变异自证:删掉新增那句 hash,新测试变红。
- [ ] **Step 5:** 跑 `frame_fp::tests`、`gpu::tests`、`redraw_is_frame_capped`;全绿后提交 `feat(app): 焦点 pane 里 Claude Code 的自绘光标会闪 (F333)`。
- [ ] **Step 6: 标注未验证** — 闪烁是否真的可见、灭相位下字符是否可读、CPU 是否因每 530ms 重整形一行而上升:只能人眼/实机,写进手验清单。

### Task 6: 文档、版本与发版

- [ ] **Step 1:** 在 `spec.md` F330~F333 补「守护测试」真名。若新增了领域陷阱(假光标判据建在颜色签名上、`win::read` 阻塞 3 秒)评估是否登记进 `CLAUDE.md` 陷阱表 —— **先问用户**,不擅自改。
- [ ] **Step 2:** 按 `.claude/skills/release-windows/SKILL.md` 走发版一条龙(升 patch → 全套绿 → 交叉编译 + objdump → 签名 → Release)。**Skill 加载后逐步照做,不凭记忆。** 私密信息不进被跟踪文件。
- [ ] **Step 3:** 报告里附手验清单(以下全为「未验证,需人工确认」):
  1. 资源管理器选 1 个文件 Ctrl+C → pane 里 Ctrl+V:文件落在节点 `/tmp`(或配置的截图目录),输入行出现 `@/tmp/名字 `。
  2. 选 3 个文件(其中一个文件名带空格)→ 不含空格的带 `@`,含空格的是裸路径且弹 Warn toast。
  3. 目标已有同名文件 → 弹框列出冲突名;「取消」后无任何上传、无回填;「全部覆盖」后正常传完。
  4. 选一个大文件(几百 MB,走代理链路):底部传输队列有进度;中途取消 → 不回填路径。
  5. 选中包含文件夹 → toast 说明已跳过,其余文件正常。
  6. 截图 Ctrl+V → 路径前有 `@`。
  7. Ctrl+V 无明显卡顿(`win::read` 最长 3 秒,正常应瞬时)。
  8. 分屏:焦点 pane 的 Claude Code 输入光标闪烁,非焦点 pane 的静止;切换焦点跟着变;失焦窗口不闪、CPU 不涨。
  9. 灭相位时光标下有字符(如行中编辑)仍可读。

---

## Self-Review

- **Spec 覆盖:** 决策 1→Task 4 Step 3;2→Task 1/4;3→Task 4 Step 4;4→Task 0/1/2;5→Task 4 Step 5/6(队列已全局可见,无状态栏任务);6→无任务(明确不做);7→Task 5;8→Task 6。
- **占位符:** Task 4 Step 5 的 `arrival` 与 Task 1 的 `pane` 类型两处写明「以编译器/现有构造点为准」—— 这是因为需读 `TransferSpec` 构造点/`PaneId` 定义才能落笔,实现者第一步就要读,不是 TBD。
- **类型一致:** `BatchTracker::{open, job_finished, drop_generation}`、`BatchDone::{Fill, Abandoned}`、`at_ref`/`at_refs_line`(返回 `(String, Vec<String>)`)、`Picked`、`fake_cursor_cell`、`clip_paste(bool,bool,bool)` 在各任务中同名同签名。
- **风险点:** ① Task 5 行指纹/行带差分是否随单格颜色变化失效(Step 1 spike);② `win::read()` 每次 Ctrl+V 的 3 秒阻塞上限;③ 含空格文件名无法带 `@`(Claude Code 限制),只能 toast 提示。
