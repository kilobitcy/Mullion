//! 传输队列的**纯逻辑**(F55/F56)。零 egui / 零 tokio / 零 IO ——
//! 「并发闸门放行几个」「一条失败会不会连坐」「冲突选完有没有重排」
//! 全是状态机 bug,得能在没有网络的情况下复现。
//!
//! 调度的形状:`app.rs` 每帧调一次 [`Queue::take_runnable`],拿到的 id
//! 各起一条 sftp channel 跑;worker 只回报结果,**不碰队列** ——
//! 队列的所有权留在 UI 线程,于是这里一把锁都不需要。

/// 传输方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Upload,
    Download,
}

/// 冲突时的处置。**没有「静默覆盖」这一档** —— F55 的硬要求。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conflict {
    Overwrite,
    Skip,
    Rename,
}

/// worker 回报的失败原因。
///
/// `Conflict` 单独一档:它不是错误,是「需要用户拿主意」。混进 `Failed`
/// 的话队列会把它当永久失败扔掉,用户永远等不到那个对话框。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobError {
    Conflict,
    Other(String),
}

/// 冲突在 `Result<(), String>` 里的哨兵串。worker 与队列**必须共用这一个
/// 常量** —— 两边各写一遍字面量,改动其中一边就会静默退化成普通失败。
pub const CONFLICT_MARKER: &str = "\u{1}mullion-conflict";

impl From<JobError> for String {
    fn from(e: JobError) -> String {
        match e {
            JobError::Conflict => CONFLICT_MARKER.to_string(),
            JobError::Other(m) => m,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobState {
    Pending,
    Running,
    /// 等用户在冲突对话框里拿主意。
    Conflict,
    Done,
    Skipped,
    Canceled,
    Failed(String),
}

impl JobState {
    pub fn is_finished(&self) -> bool {
        matches!(
            self,
            JobState::Done | JobState::Skipped | JobState::Canceled | JobState::Failed(_)
        )
    }
}

/// 入队时要填的东西。
pub struct NewJob {
    pub dir: Direction,
    /// S1:属主标签的世代。异步结果按它路由,永远不投给活动标签。
    pub generation: u64,
    /// 界面上显示的一行(通常是文件名)。
    pub label: String,
    pub total: u64,
}

pub struct Job {
    pub id: u64,
    pub dir: Direction,
    pub generation: u64,
    pub label: String,
    pub total: u64,
    pub done: u64,
    pub state: JobState,
    /// 用户对这一条的冲突处置。worker 起跑时读它决定覆盖还是改名。
    pub resolved: Option<Conflict>,
}

#[derive(Debug, Default, PartialEq)]
pub struct Summary {
    /// 正在跑的上行 / 下行条数。
    pub up: usize,
    pub down: usize,
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// 还有没有没收尾的活。折叠行显示什么看它。
    pub busy: bool,
    /// F167:没收尾的条数(Pending + Running)。场景判据与 profile.load 的分母。
    pub active: usize,
}

pub struct Queue {
    jobs: Vec<Job>,
    next_id: u64,
    concurrency: usize,
    /// 「对本批后续冲突都这么办」选过之后的默认处置。
    blanket: Option<Conflict>,
    rate: RateMeter,
}

impl Queue {
    pub fn new(concurrency: usize) -> Self {
        Self {
            jobs: Vec::new(),
            next_id: 1,
            concurrency: concurrency.max(1),
            blanket: None,
            rate: RateMeter::default(),
        }
    }

    pub fn jobs(&self) -> &[Job] {
        &self.jobs
    }

    pub fn get(&self, id: u64) -> Option<&Job> {
        self.jobs.iter().find(|j| j.id == id)
    }

    fn get_mut(&mut self, id: u64) -> Option<&mut Job> {
        self.jobs.iter_mut().find(|j| j.id == id)
    }

    pub fn push(&mut self, n: NewJob) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.jobs.push(Job {
            id,
            dir: n.dir,
            generation: n.generation,
            label: n.label,
            total: n.total,
            done: 0,
            state: JobState::Pending,
            resolved: None,
        });
        id
    }

    /// 放行一批可以起跑的 job,并把它们就地置成 `Running`。
    /// **同一个 id 不会被放行两次** —— 状态已经改掉了。
    pub fn take_runnable(&mut self) -> Vec<u64> {
        let running = self
            .jobs
            .iter()
            .filter(|j| j.state == JobState::Running)
            .count();
        let mut slots = self.concurrency.saturating_sub(running);
        let mut out = Vec::new();
        for j in self.jobs.iter_mut() {
            if slots == 0 {
                break;
            }
            if j.state == JobState::Pending {
                j.state = JobState::Running;
                out.push(j.id);
                slots -= 1;
            }
        }
        out
    }

    /// worker 报进度。**只有在跑的才收** —— 一条迟到的进度上报(worker
    /// 在发完成事件之后才被 abort、或者取消时那一块刚好写完)会把已经
    /// 收尾的 job 的 `done` 拽回去,进度条当着用户的面倒退。
    pub fn progress(&mut self, id: u64, done: u64) {
        if let Some(j) = self.get_mut(id) {
            if j.state == JobState::Running {
                j.done = done;
            }
        }
    }

    /// worker 收工。`Err(CONFLICT_MARKER)` 会落到 `JobState::Conflict` 等用户;
    /// 已经选过「全部应用」的话直接按那个处置走,不再打扰。
    pub fn finish(&mut self, id: u64, result: Result<(), String>) {
        let blanket = self.blanket;
        let Some(j) = self.get_mut(id) else { return };
        match result {
            Ok(()) => {
                j.done = j.total;
                j.state = JobState::Done;
            }
            Err(msg) if msg == CONFLICT_MARKER => j.state = JobState::Conflict,
            Err(msg) => j.state = JobState::Failed(msg),
        }
        if j.state == JobState::Conflict {
            if let Some(c) = blanket {
                self.apply_conflict(id, c);
            }
        }
    }

    /// 用户在冲突对话框里选完了。`apply_all` 会把这个处置记成本批的默认。
    pub fn resolve_conflict(&mut self, id: u64, choice: Conflict, apply_all: bool) {
        if apply_all {
            self.blanket = Some(choice);
        }
        self.apply_conflict(id, choice);
    }

    fn apply_conflict(&mut self, id: u64, choice: Conflict) {
        let Some(j) = self.get_mut(id) else { return };
        if j.state != JobState::Conflict {
            return;
        }
        match choice {
            // 跳过不需要再跑一趟网络,直接收尾。排回 Pending 的话
            // worker 会重跑一遍、再撞一次冲突,来回死循环。
            Conflict::Skip => j.state = JobState::Skipped,
            Conflict::Overwrite | Conflict::Rename => {
                j.resolved = Some(choice);
                j.state = JobState::Pending;
            }
        }
    }

    /// 还在等用户处置的第一条(对话框一次只问一个)。
    pub fn first_conflict(&self) -> Option<&Job> {
        self.jobs.iter().find(|j| j.state == JobState::Conflict)
    }

    pub fn cancel(&mut self, id: u64) {
        if let Some(j) = self.get_mut(id) {
            if !j.state.is_finished() {
                j.state = JobState::Canceled;
            }
        }
    }

    pub fn cancel_all(&mut self) {
        let ids: Vec<u64> = self
            .jobs
            .iter()
            .filter(|j| !j.state.is_finished())
            .map(|j| j.id)
            .collect();
        for id in ids {
            self.cancel(id);
        }
    }

    /// 属主标签关掉时把它的 job 全部作废 —— 留着会往一个已经不存在的
    /// 世代上派活,worker 起跑时找不到连接只能报错,而用户已经不在乎了。
    ///
    /// **返回被作废的 id**:调用方还要扳它们的取消旗标(队列不认识旗标,
    /// 那是 `app.rs` 的东西)。只改状态不扳旗标的话,在跑的 worker 会闷头
    /// 把整个文件传完 —— 还是往一个已经关掉的标签的落点写。
    pub fn cancel_generation(&mut self, generation: u64) -> Vec<u64> {
        let ids: Vec<u64> = self
            .jobs
            .iter()
            .filter(|j| j.generation == generation && !j.state.is_finished())
            .map(|j| j.id)
            .collect();
        for id in &ids {
            self.cancel(*id);
        }
        ids
    }

    pub fn clear_finished(&mut self) {
        self.jobs.retain(|j| !j.state.is_finished());
    }

    pub fn summary(&self) -> Summary {
        let mut s = Summary::default();
        for j in &self.jobs {
            if j.state == JobState::Running {
                match j.dir {
                    Direction::Upload => s.up += 1,
                    Direction::Download => s.down += 1,
                }
            }
            if j.state.is_finished() && j.state != JobState::Done {
                // 跳过 / 取消 / 失败的不该把分母撑大,否则进度条永远到不了头。
                continue;
            }
            s.bytes_total += j.total;
            // 完成的 job 的 `done` 已经在 `finish` 里补成 `total` 了,这里
            // 不必再特判 —— 特判是测不出来的冗余,真正的防线在 `progress`。
            s.bytes_done += j.done.min(j.total);
            if !j.state.is_finished() {
                s.busy = true;
                s.active += 1;
            }
        }
        s
    }

    /// 每帧调一次,更新速率估计。`now_secs` 由调用方给(单调时钟的秒数),
    /// 队列自己不碰时钟 —— 碰了就没法纯单测。
    pub fn tick(&mut self, now_secs: f64) {
        let done = self.summary().bytes_done;
        self.rate.sample(now_secs, done);
    }

    pub fn rate_bps(&self) -> f64 {
        self.rate.bps()
    }
}

/// 速率估计。指数平滑 —— 不平滑的话数字每帧乱跳,读都读不出来。
#[derive(Default)]
pub struct RateMeter {
    last: Option<(f64, u64)>,
    bps: f64,
}

impl RateMeter {
    pub fn sample(&mut self, now_secs: f64, total_done: u64) -> f64 {
        match self.last {
            None => self.last = Some((now_secs, total_done)),
            Some((t0, b0)) => {
                let dt = now_secs - t0;
                // 同一时刻两次采样会算出 inf,ETA 随之变成 NaN 显示成空白。
                if dt > 0.0 {
                    let inst = total_done.saturating_sub(b0) as f64 / dt;
                    self.bps = if self.bps == 0.0 {
                        inst
                    } else {
                        self.bps * 0.7 + inst * 0.3
                    };
                    self.last = Some((now_secs, total_done));
                }
            }
        }
        self.bps
    }

    pub fn bps(&self) -> f64 {
        self.bps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q() -> Queue {
        Queue::new(2)
    }

    fn job(dir: Direction) -> NewJob {
        NewJob {
            dir,
            generation: 7,
            label: "x".into(),
            total: 100,
        }
    }

    fn conflict() -> Result<(), String> {
        Err(JobError::Conflict.into())
    }

    #[test]
    fn the_concurrency_limit_caps_how_many_jobs_run_at_once() {
        // F56:闸门开太大就是同时开 N 条 sftp channel,高延迟链路上
        // 每条都在抢同一个 TCP 窗口,总吞吐反而掉。
        let mut q = q();
        for _ in 0..5 {
            q.push(job(Direction::Download));
        }
        assert_eq!(q.take_runnable().len(), 2, "第一轮只该放行 2 个");
        assert!(q.take_runnable().is_empty(), "闸门满了不该再放行");
    }

    #[test]
    fn finishing_a_job_frees_a_slot_for_the_next_one() {
        let mut q = q();
        for _ in 0..3 {
            q.push(job(Direction::Download));
        }
        let first = q.take_runnable();
        q.finish(first[0], Ok(()));
        assert_eq!(q.take_runnable().len(), 1, "腾出一个槽就该放行一个");
    }

    #[test]
    fn a_single_failure_does_not_stop_the_rest_of_the_queue() {
        // F55 明确要求:一条失败不掀桌子。
        let mut q = q();
        let a = q.push(job(Direction::Upload));
        let b = q.push(job(Direction::Upload));
        assert_eq!(q.take_runnable().len(), 2);
        q.finish(a, Err("炸了".into()));
        assert_eq!(
            q.get(b).unwrap().state,
            JobState::Running,
            "另一条不该被连坐"
        );
        assert!(matches!(q.get(a).unwrap().state, JobState::Failed(_)));
    }

    #[test]
    fn a_conflicted_job_waits_for_the_user_and_reruns_after_the_choice() {
        let mut q = q();
        let a = q.push(job(Direction::Download));
        q.take_runnable();
        q.finish(a, conflict());
        assert_eq!(q.get(a).unwrap().state, JobState::Conflict);
        assert!(q.take_runnable().is_empty(), "等用户拿主意期间不许自己重跑");

        q.resolve_conflict(a, Conflict::Overwrite, false);
        assert_eq!(q.take_runnable(), vec![a], "选完了该重新排上");
        assert_eq!(q.get(a).unwrap().resolved, Some(Conflict::Overwrite));
    }

    #[test]
    fn apply_to_all_answers_later_conflicts_without_asking_again() {
        let mut q = Queue::new(1);
        let a = q.push(job(Direction::Download));
        let b = q.push(job(Direction::Download));
        q.take_runnable();
        q.finish(a, conflict());
        q.resolve_conflict(a, Conflict::Skip, true);
        q.take_runnable();
        q.finish(b, conflict());
        assert_ne!(
            q.get(b).unwrap().state,
            JobState::Conflict,
            "已经选过「全部应用」就不该再拦一次"
        );
        assert_eq!(q.get(b).unwrap().state, JobState::Skipped);
    }

    #[test]
    fn skipping_a_conflict_finishes_the_job_instead_of_transferring_it() {
        let mut q = Queue::new(1);
        let a = q.push(job(Direction::Download));
        q.take_runnable();
        q.finish(a, conflict());
        q.resolve_conflict(a, Conflict::Skip, false);
        assert_eq!(
            q.get(a).unwrap().state,
            JobState::Skipped,
            "选了跳过就该直接收尾,而不是排队重跑"
        );
        assert!(q.take_runnable().is_empty());
    }

    #[test]
    fn canceling_a_pending_job_never_lets_it_start() {
        let mut q = Queue::new(1);
        let a = q.push(job(Direction::Upload));
        let b = q.push(job(Direction::Upload));
        q.take_runnable();
        q.cancel(b);
        q.finish(a, Ok(()));
        assert!(q.take_runnable().is_empty(), "取消掉的不该被调度");
    }

    #[test]
    fn closing_a_tab_cancels_only_its_own_jobs() {
        let mut q = Queue::new(4);
        let mine = q.push(job(Direction::Upload));
        let other = q.push(NewJob {
            dir: Direction::Upload,
            generation: 99,
            label: "y".into(),
            total: 100,
        });
        let canceled = q.cancel_generation(7);
        assert_eq!(q.get(mine).unwrap().state, JobState::Canceled);
        assert_eq!(
            q.get(other).unwrap().state,
            JobState::Pending,
            "别的标签的传输不该被连累"
        );
        // 返回值是给调用方扳取消旗标用的(见 `cancel_generation` 的文档)。
        // 漏报的后果是 worker 闷头把整个文件传完 —— 状态是「已取消」,
        // 网络却还在跑,而且往一个已经关掉的标签的落点写。
        assert_eq!(canceled, vec![mine], "被作废的 id 必须报回去:{canceled:?}");
    }

    #[test]
    fn the_summary_counts_only_unfinished_bytes_so_the_bar_does_not_jump_backwards() {
        let mut q = Queue::new(4);
        let a = q.push(job(Direction::Upload));
        let b = q.push(job(Direction::Download));
        q.take_runnable();
        q.progress(a, 40);
        let s = q.summary();
        assert_eq!((s.up, s.down), (1, 1), "上下行各一条在跑");
        assert_eq!(s.bytes_total, 200);
        assert_eq!(s.bytes_done, 40);

        q.finish(b, Ok(()));
        assert_eq!(
            q.summary().bytes_done,
            140,
            "完成的那条按 total 计入,不能倒退"
        );
    }

    #[test]
    fn a_late_progress_report_cannot_drag_a_finished_job_backwards() {
        let mut q = Queue::new(4);
        let a = q.push(job(Direction::Upload));
        q.take_runnable();
        q.progress(a, 60);
        q.finish(a, Ok(()));
        q.progress(a, 60); // 迟到的一条
        assert_eq!(
            q.summary().bytes_done,
            100,
            "收尾之后的进度上报把进度条拽回去了"
        );
    }

    #[test]
    fn a_skipped_job_leaves_the_denominator_so_the_bar_can_reach_the_end() {
        let mut q = Queue::new(4);
        let a = q.push(job(Direction::Upload));
        q.take_runnable();
        q.finish(a, conflict());
        q.resolve_conflict(a, Conflict::Skip, false);
        assert_eq!(q.summary().bytes_total, 0, "跳过的不该还占着分母");
        assert!(!q.summary().busy);
    }

    #[test]
    fn clearing_finished_jobs_keeps_the_ones_still_in_flight() {
        let mut q = Queue::new(4);
        let a = q.push(job(Direction::Upload));
        let b = q.push(job(Direction::Upload));
        q.take_runnable();
        q.finish(a, Ok(()));
        q.clear_finished();
        assert_eq!(q.jobs().len(), 1);
        assert_eq!(q.jobs()[0].id, b);
    }

    #[test]
    fn the_rate_meter_reports_bytes_per_second_from_two_samples() {
        let mut m = RateMeter::default();
        assert_eq!(m.sample(0.0, 0), 0.0, "第一次采样没有区间,给 0");
        assert_eq!(m.sample(2.0, 2_000), 1_000.0);
    }

    #[test]
    fn the_rate_meter_ignores_a_zero_length_interval_instead_of_dividing_by_zero() {
        let mut m = RateMeter::default();
        m.sample(1.0, 100);
        let r = m.sample(1.0, 500);
        assert!(r.is_finite(), "同一时刻两次采样不能算出 inf:{r}");
    }

    /// F167:场景判据「传输队列非空」用的是 active(未收尾条数),不是
    /// running —— pending 的 job 也说明用户正等着传输。
    ///
    /// 自证会变红:把 `summary` 里 `s.active += 1` 挪进 `Running` 分支。
    #[test]
    fn active_counts_pending_and_running_but_not_finished() {
        let mut q = Queue::new(1);
        let a = q.push(job(Direction::Download));
        let _b = q.push(job(Direction::Download)); // 并发 1,这条留在 Pending
        assert_eq!(q.take_runnable(), vec![a]);
        assert_eq!(q.summary().active, 2, "1 running + 1 pending");
        q.progress(a, 100);
        q.finish(a, Ok(()));
        assert_eq!(q.summary().active, 1, "收尾的不算");
    }
}
