//! F330~F332：终端 Ctrl+V 粘贴文件的纯逻辑。
//!
//! 不碰 UI、网络、窗口 —— 冲突判定、`@` 引用格式化、批次汇合都能无头单测。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use mullion_core::layout::PaneId;

use crate::files::queue::{Conflict, Direction, JobState, NewJob, Queue};

/// 剪贴板里一个本地条目（调用处已 stat 过）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picked {
    /// 本地完整路径（上传的源）。
    pub local: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

/// Claude Code 的 `@` 引用。含空白的路径**不加 `@`**：`@` 解析在首个空格处中止，
/// 双引号与反斜杠转义均不支持（issue #80553）—— 加了反而静默引用错文件。
pub fn at_ref(path: &str) -> String {
    if path.chars().any(char::is_whitespace) {
        path.to_string()
    } else {
        format!("@{path}")
    }
}

/// 多个引用空格分隔，末尾留一个空格，不带回车：接着打字是自然的下一句话。
/// 第二个返回值是**没能带 `@` 的路径**，调用方据此弹 Warn toast，不静默。
pub fn at_refs_line(paths: &[String]) -> (String, Vec<String>) {
    let bare: Vec<String> = paths
        .iter()
        .filter(|p| p.chars().any(char::is_whitespace))
        .cloned()
        .collect();
    let mut s = paths
        .iter()
        .map(|p| at_ref(p))
        .collect::<Vec<_>>()
        .join(" ");
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

/// 同一批里第一个重名的文件名（来自不同文件夹的同名文件）。它们会落到远端同一个
/// 路径上，后传的盖掉先传的，回填的两个 `@` 指向同一个文件 —— 整批拒绝，不预检
/// 不入队。远端按字节比较名字，这里也按原样比较（不折大小写）。
pub fn first_duplicate_name(files: &[Picked]) -> Option<String> {
    let mut seen = HashSet::new();
    files
        .iter()
        .find(|f| !seen.insert(f.name.as_str()))
        .map(|f| f.name.clone())
}

/// 目标目录里已存在的同名项，顺序按 `names`。
pub fn conflicting(names: &[String], existing: &HashSet<String>) -> Vec<String> {
    names
        .iter()
        .filter(|n| existing.contains(*n))
        .cloned()
        .collect()
}

/// F330：发起那一刻冻结的一次粘贴。预检、冲突框、入队全程只认这一份 ——
/// 不在结果回来时回读「此刻的焦点 pane / 落点目录」（T11 同族：高延迟链路上
/// 预检要好几百毫秒，用户完全可能已经切了标签、切了分屏）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PastePlan {
    pub generation: u64,
    /// 回填路径的那块 pane。
    pub pane: PaneId,
    /// 焦点分屏所在的那台机器（F132）。预检与上传都开在它上面 —— 蹭侧栏那条
    /// 的话文件传到了另一台机器上，而打进输入行的路径看起来完全正常。
    pub host_ix: Option<usize>,
    /// 目标目录（用户配置原样；空 = `shot::DEFAULT_DIR`，由 `remote_join` 落回）。
    pub dir: String,
    /// 只有文件，文件夹在发起时已经剔掉了。
    pub files: Vec<Picked>,
    /// 被跳过的文件夹名。随计划带到入队那一刻的提示里 —— 提示框只有一格，
    /// 发起时报一次的话，几百毫秒后就被「正在上传」盖掉，用户看不见。
    pub skipped_dirs: Vec<String>,
}

impl PastePlan {
    /// 这个名字落到远端的绝对路径。
    pub fn remote_of(&self, name: &str) -> String {
        crate::shot::remote_join(&self.dir, name)
    }

    pub fn names(&self) -> Vec<String> {
        self.files.iter().map(|f| f.name.clone()).collect()
    }
}

/// F330：预检回来之后怎么办。
#[derive(Debug, PartialEq, Eq)]
pub enum AfterCheck {
    /// 没有同名，直接入队（不预设覆盖）。
    Enqueue,
    /// 有同名（按 `names` 的顺序），先问「全部覆盖 / 取消」；确认前整批不入队。
    Ask(Vec<String>),
    /// 预检本身失败 —— 报错、不入队。**不能当成「没冲突」**：那样等于用户
    /// 没被问过就开传，撞上已存在的文件时再一条条弹框，整批的「先问再传」落空。
    Fail(String),
}

/// `existing` 是预检读回来的「目标处已存在的名字」。
pub fn after_check(names: &[String], existing: Result<Vec<String>, String>) -> AfterCheck {
    match existing {
        Err(e) => AfterCheck::Fail(e),
        Ok(found) => {
            let found: HashSet<String> = found.into_iter().collect();
            let hit = conflicting(names, &found);
            if hit.is_empty() {
                AfterCheck::Enqueue
            } else {
                AfterCheck::Ask(hit)
            }
        }
    }
}

/// 冲突框最多列几个名字。多出来的折成一行「等另外 N 个」，框不会被几百个
/// 名字撑出屏幕。
pub const CONFLICT_LIST_MAX: usize = 8;

/// 冲突框里列出的那几行。
pub fn conflict_lines(names: &[String]) -> Vec<String> {
    let mut out: Vec<String> = names.iter().take(CONFLICT_LIST_MAX).cloned().collect();
    if names.len() > CONFLICT_LIST_MAX {
        out.push(format!("等另外 {} 个", names.len() - CONFLICT_LIST_MAX));
    }
    out
}

/// F330：整批入队。`overwrite` = 用户已在冲突框里点了「全部覆盖」，每条预设
/// `Overwrite` —— 不预设的话 worker 撞上已存在的目标会落成 `Conflict`，
/// 同一件事再问一遍。
///
/// 返回 `(job id, 文件, 远端绝对路径)`，顺序同 `plan.files`（回填顺序 = 剪贴板顺序）。
pub fn enqueue(queue: &mut Queue, plan: &PastePlan, overwrite: bool) -> Vec<(u64, Picked, String)> {
    plan.files
        .iter()
        .map(|f| {
            let id = queue.push(NewJob {
                dir: Direction::Upload,
                generation: plan.generation,
                label: f.name.clone(),
                total: f.size,
            });
            if overwrite {
                queue.set_resolved(id, Conflict::Overwrite);
            }
            (id, f.clone(), plan.remote_of(&f.name))
        })
        .collect()
}

/// F332：一条 job 收工后，对批次来说算不算「按计划落地」。
///
/// `None` = 还没收尾（挂在冲突上等用户拿主意），这次不报给批次。只有 `Done`
/// **且没走「重命名」**才算成：改名之后文件不在我们要回填的那个路径上，
/// 回填就是一个指向别的文件（或不存在）的 `@` 引用。
pub fn landed_as_planned(state: &JobState, resolved: Option<Conflict>) -> Option<bool> {
    if !state.is_finished() {
        return None;
    }
    Some(*state == JobState::Done && resolved != Some(Conflict::Rename))
}

#[derive(Debug, PartialEq, Eq)]
pub enum BatchDone {
    Fill {
        generation: u64,
        pane: PaneId,
        paths: Vec<String>,
    },
    Abandoned {
        failed: usize,
    },
}

struct Batch {
    generation: u64,
    pane: PaneId,
    /// (job id, 远端绝对路径)，保持剪贴板顺序。
    jobs: Vec<(u64, String)>,
    finished: HashSet<u64>,
    failed: usize,
}

/// 批次汇合。**只看 `TransferDone`**（T11 同族：计时/判定不从调用点起算，
/// 唯一可靠的是收工事件）。
#[derive(Default)]
pub struct BatchTracker {
    batches: HashMap<u64, Batch>,
    by_job: HashMap<u64, u64>,
}

impl BatchTracker {
    pub fn open(&mut self, batch: u64, generation: u64, pane: PaneId, jobs: Vec<(u64, String)>) {
        for (j, _) in &jobs {
            self.by_job.insert(*j, batch);
        }
        self.batches.insert(
            batch,
            Batch {
                generation,
                pane,
                jobs,
                finished: HashSet::new(),
                failed: 0,
            },
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

    /// 不经 `TransferDone` 就收了尾的 job（排队中被取消、冲突框里点了跳过、
    /// worker 没起来、整栏被清掉）。它们永远等不到收工事件，不在这里结算的话
    /// 批次一直挂着，用户等不到「没回填」的提示。
    ///
    /// `failed(job)` = 队列说这条已经没成。**这里只报失败**，所以永远产出
    /// 不了 `Fill` —— 回填只可能来自 `TransferDone`（T11）。
    pub fn sweep(&mut self, failed: impl Fn(u64) -> bool) -> Vec<BatchDone> {
        let gone: Vec<u64> = self
            .batches
            .values()
            .flat_map(|b| {
                b.jobs
                    .iter()
                    .map(|(j, _)| *j)
                    .filter(|j| !b.finished.contains(j))
            })
            .filter(|j| failed(*j))
            .collect();
        gone.into_iter()
            .filter_map(|j| self.job_finished(j, false))
            .collect()
    }

    /// 标签关闭：该世代的批次整体作废（`wind_down` 直接 abort task，
    /// `TransferDone` 可能永远不来，不回收就一直堆着）。
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

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_core::layout::PaneId;

    /// F331：含空白的路径**不加 `@`**。Claude Code 的 `@` 在首个空格处中止，
    /// 引号/转义都不支持 —— 加了 `@` 只会静默引用到半截路径。
    ///
    /// 自证会变红：把 `at_ref` 里「含空白就不加 @」那支删掉。
    #[test]
    fn a_path_with_spaces_gets_no_at_because_claude_code_would_cut_it() {
        assert_eq!(at_ref("/tmp/a.png"), "@/tmp/a.png");
        assert_eq!(at_ref("/tmp/my file.png"), "/tmp/my file.png");
    }

    /// F331：多个之间一个空格，末尾再留一个，**不带回车**；并报出哪些没带 `@`。
    ///
    /// 自证会变红：去掉末尾的 `s.push(' ')`，或让 `bare` 恒为空。
    #[test]
    fn refs_line_is_space_separated_with_one_trailing_space_and_reports_bare_paths() {
        let (l, bare) = at_refs_line(&["/tmp/a".into(), "/tmp/b c".into()]);
        assert_eq!(l, "@/tmp/a /tmp/b c ");
        assert!(!l.contains('\n') && !l.contains('\r'));
        assert_eq!(bare, vec!["/tmp/b c".to_string()]);
    }

    /// F330：文件夹跳过且要报名字，不能静默。
    ///
    /// 自证会变红：让 `split_picked` 不收集 `skipped`，或把文件夹也放进 `files`。
    #[test]
    fn directories_are_skipped_and_named() {
        let picked = vec![
            Picked {
                local: "C:/x/a.txt".into(),
                name: "a.txt".into(),
                is_dir: false,
                size: 3,
            },
            Picked {
                local: "C:/x/d".into(),
                name: "d".into(),
                is_dir: true,
                size: 0,
            },
        ];
        let (files, skipped) = split_picked(picked);
        assert_eq!(files.len(), 1);
        assert_eq!(skipped, vec!["d".to_string()]);
    }

    /// F330：同一批里来自不同文件夹的同名文件会落到远端同一路径上，后者盖掉前者，
    /// 回填的两个引用指向同一个文件 —— 必须在预检前认出来。
    ///
    /// 自证会变红：让 `first_duplicate_name` 恒返回 `None`，或改成比较 `local`。
    #[test]
    fn two_files_with_the_same_name_in_one_batch_are_caught() {
        let mut a2 = file("a.txt", 1);
        a2.local = "D:/y/a.txt".into();
        assert_eq!(
            first_duplicate_name(&[file("a.txt", 1), file("b.txt", 1), a2]),
            Some("a.txt".to_string())
        );
        assert_eq!(
            first_duplicate_name(&[file("a.txt", 1), file("b.txt", 1)]),
            None
        );
        assert_eq!(
            first_duplicate_name(&[file("A.txt", 1), file("a.txt", 1)]),
            None,
            "远端区分大小写，不该当成重名"
        );
    }

    /// F330：只报**确实存在**的目标名，顺序保持（按传入名单的顺序，而非集合的）。
    ///
    /// 自证会变红：把 `conflicting` 的过滤条件取反，或改成遍历 `existing`。
    #[test]
    fn conflicts_keep_order_and_only_name_existing_targets() {
        let names = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let existing: std::collections::HashSet<String> = ["c".into(), "a".into()].into();
        assert_eq!(
            conflicting(&names, &existing),
            vec!["a".to_string(), "c".to_string()]
        );
    }

    /// F332：全部成功才给出回填；缺一个都不给。
    ///
    /// 自证会变红：让 `job_finished` 在第一条完成时就返回 `Fill`。
    #[test]
    fn a_batch_fills_back_only_when_every_job_succeeded() {
        let mut t = BatchTracker::default();
        t.open(
            7,
            1,
            PaneId(10),
            vec![(100, "/tmp/a".into()), (101, "/tmp/b".into())],
        );
        assert_eq!(t.job_finished(100, true), None);
        assert_eq!(
            t.job_finished(101, true),
            Some(BatchDone::Fill {
                generation: 1,
                pane: PaneId(10),
                paths: vec!["/tmp/a".into(), "/tmp/b".into()]
            })
        );
    }

    /// F332：任一条失败/取消 → 整批不回填，且只报一次。
    ///
    /// 自证会变红：把 `failed == 0` 的判断改成「最后一条成功即 Fill」。
    #[test]
    fn one_failed_job_means_no_path_is_filled_in() {
        let mut t = BatchTracker::default();
        t.open(
            7,
            1,
            PaneId(10),
            vec![(100, "/tmp/a".into()), (101, "/tmp/b".into())],
        );
        assert_eq!(t.job_finished(100, false), None);
        assert_eq!(
            t.job_finished(101, true),
            Some(BatchDone::Abandoned { failed: 1 })
        );
        assert_eq!(t.job_finished(101, true), None, "批次已结算，不重复触发");
    }

    /// F332：标签关闭（`wind_down` 直接 abort task，`TransferDone` 可能永远不来）后，
    /// 该世代的批次必须被回收；回收后迟到的收工事件不得再触发回填，别的世代不受影响。
    ///
    /// 自证会变红：让 `drop_generation` 变成空函数，或不清理 `by_job`。
    #[test]
    fn dropping_a_generation_discards_its_batches() {
        let mut t = BatchTracker::default();
        t.open(7, 1, PaneId(10), vec![(100, "/tmp/a".into())]);
        t.open(8, 2, PaneId(11), vec![(200, "/tmp/b".into())]);
        t.drop_generation(1);
        assert_eq!(t.job_finished(100, true), None);
        assert_eq!(
            t.job_finished(200, true),
            Some(BatchDone::Fill {
                generation: 2,
                pane: PaneId(11),
                paths: vec!["/tmp/b".into()]
            })
        );
    }

    fn file(name: &str, size: u64) -> Picked {
        Picked {
            local: format!("C:/x/{name}").into(),
            name: name.into(),
            is_dir: false,
            size,
        }
    }

    fn plan(dir: &str, names: &[&str]) -> PastePlan {
        PastePlan {
            generation: 3,
            pane: PaneId(2),
            host_ix: Some(1),
            dir: dir.into(),
            files: names.iter().map(|n| file(n, 10)).collect(),
            skipped_dirs: Vec::new(),
        }
    }

    /// F330：预检出错**不得**当成「没冲突」—— 那样用户没被问过就开传。
    ///
    /// 自证会变红：把 `after_check` 的 `Err` 臂改成 `AfterCheck::Enqueue`。
    #[test]
    fn a_failed_check_is_an_error_not_a_green_light() {
        let names = vec!["a".to_string(), "b".to_string()];
        assert_eq!(
            after_check(&names, Err("断了".into())),
            AfterCheck::Fail("断了".into())
        );
        assert_eq!(after_check(&names, Ok(vec![])), AfterCheck::Enqueue);
        assert_eq!(
            after_check(&names, Ok(vec!["b".into(), "zzz".into()])),
            AfterCheck::Ask(vec!["b".into()])
        );
    }

    /// F330：冲突框最多列 8 个名字，多的折成一行并报出还剩几个。
    ///
    /// 自证会变红：去掉 `take(CONFLICT_LIST_MAX)`，或不追加「等另外 N 个」。
    #[test]
    fn the_conflict_list_is_capped_and_says_how_many_more() {
        let few: Vec<String> = (0..3).map(|i| format!("f{i}")).collect();
        assert_eq!(conflict_lines(&few), few);
        let many: Vec<String> = (0..11).map(|i| format!("f{i}")).collect();
        let lines = conflict_lines(&many);
        assert_eq!(lines.len(), CONFLICT_LIST_MAX + 1);
        assert_eq!(lines[..CONFLICT_LIST_MAX], many[..CONFLICT_LIST_MAX]);
        assert_eq!(lines.last().unwrap(), "等另外 3 个");
    }

    /// F330：点了「全部覆盖」才入队，且每条都带 `Overwrite` —— 否则 worker
    /// 撞上已存在的目标会落成 `Conflict`，同一件事再问一遍；没冲突时入队
    /// **不得**预设覆盖（绝不静默覆盖，F55）。
    ///
    /// 自证会变红：把 `enqueue` 里的 `if overwrite` 去掉（恒设），或删掉 `set_resolved`。
    #[test]
    fn overwrite_is_preset_only_when_the_user_chose_it() {
        let p = plan("/tmp/", &["a.png", "b.txt"]);
        let mut q = Queue::new(4);
        let got = enqueue(&mut q, &p, true);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].2, "/tmp/a.png");
        assert_eq!(got[1].2, "/tmp/b.txt");
        for (id, f, _) in &got {
            let j = q.get(*id).unwrap();
            assert_eq!(j.resolved, Some(Conflict::Overwrite));
            assert_eq!(j.dir, Direction::Upload);
            assert_eq!(j.generation, 3);
            assert_eq!(j.label, f.name);
        }

        let mut q = Queue::new(4);
        for (id, _, _) in enqueue(&mut q, &p, false) {
            assert_eq!(q.get(id).unwrap().resolved, None, "没问过用户就预设了覆盖");
        }
    }

    /// F330：落点目录为空时落回 `/tmp`（与截图共用 `remote_join`）。
    #[test]
    fn an_empty_dir_lands_in_the_default() {
        assert_eq!(plan("", &["a"]).remote_of("a"), "/tmp/a");
    }

    /// F332：只有 `Done` 且没改名才算落地；没收尾（挂在冲突上）不报。
    ///
    /// 自证会变红：去掉 `resolved != Some(Conflict::Rename)`，或让未收尾的也返回 `Some`。
    #[test]
    fn only_a_done_job_that_kept_its_name_counts_as_landed() {
        assert_eq!(landed_as_planned(&JobState::Conflict, None), None);
        assert_eq!(landed_as_planned(&JobState::Running, None), None);
        assert_eq!(landed_as_planned(&JobState::Done, None), Some(true));
        assert_eq!(
            landed_as_planned(&JobState::Done, Some(Conflict::Overwrite)),
            Some(true)
        );
        assert_eq!(
            landed_as_planned(&JobState::Done, Some(Conflict::Rename)),
            Some(false),
            "改了名的文件不在要回填的路径上"
        );
        for s in [
            JobState::Skipped,
            JobState::Canceled,
            JobState::Failed("x".into()),
        ] {
            assert_eq!(landed_as_planned(&s, None), Some(false), "{s:?}");
        }
    }

    /// F332：不经 `TransferDone` 收尾的 job（排队中被取消 / 跳过）要能结算成
    /// `Abandoned`，否则批次永远挂着、用户等不到提示；且 sweep 永远产不出 `Fill`。
    ///
    /// 自证会变红：让 `sweep` 返回空 `Vec`，或把 `job_finished(j, false)` 改成 `true`。
    #[test]
    fn a_job_that_ends_without_transfer_done_still_settles_the_batch_as_abandoned() {
        let mut t = BatchTracker::default();
        t.open(
            7,
            1,
            PaneId(10),
            vec![(100, "/tmp/a".into()), (101, "/tmp/b".into())],
        );
        assert_eq!(t.job_finished(100, true), None);
        assert!(t.sweep(|_| false).is_empty(), "没人失败不该结算");
        assert_eq!(
            t.sweep(|j| j == 101),
            vec![BatchDone::Abandoned { failed: 1 }]
        );
        assert!(t.sweep(|_| true).is_empty(), "已结算的批次不重复报");
        assert_eq!(t.job_finished(101, true), None);
    }
}
