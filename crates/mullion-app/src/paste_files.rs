//! F330~F332：终端 Ctrl+V 粘贴文件的纯逻辑。
//!
//! 不碰 UI、网络、窗口 —— 冲突判定、`@` 引用格式化、批次汇合都能无头单测。

use std::collections::{HashMap, HashSet};

use mullion_core::layout::PaneId;

/// 剪贴板里一个本地条目（调用处已 stat 过）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picked {
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

/// 目标目录里已存在的同名项，顺序按 `names`。
pub fn conflicting(names: &[String], existing: &HashSet<String>) -> Vec<String> {
    names
        .iter()
        .filter(|n| existing.contains(*n))
        .cloned()
        .collect()
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
                name: "a.txt".into(),
                is_dir: false,
                size: 3,
            },
            Picked {
                name: "d".into(),
                is_dir: true,
                size: 0,
            },
        ];
        let (files, skipped) = split_picked(picked);
        assert_eq!(files.len(), 1);
        assert_eq!(skipped, vec!["d".to_string()]);
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
}
