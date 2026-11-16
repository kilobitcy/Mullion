//! F349:打开项目后发 attach 计划。桌面 `mullion-app/src/automation.rs::run` 的手机版。
//!
//! **T11**:计划要等远端**首字节**到了才发(那时登录 shell 才可能在听),
//! 等待上限从这里起算 —— 也就是拨号与开 PTY 已经完成之后,不是用户点下去那一刻。
//! 用户先动了键盘 = 接管,计划一个字节都不再发(抢输入比不 attach 更糟)。

use std::sync::Arc;
use std::time::Duration;

use mullion_ssh::schedule::{write_scheduled, ByteSink, ScheduleOutcome};
use mullion_store::Step;
use tokio::sync::oneshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Completed,
    /// 用户接管 / 泵先没了。
    Aborted,
    /// 首字节没等到。
    SkippedTimeout,
    Disconnected,
    Congested,
}

pub(crate) async fn run(
    sink: Arc<dyn ByteSink>,
    steps: Vec<Step>,
    ready: oneshot::Receiver<()>,
    mut cancel: oneshot::Receiver<()>,
    ready_timeout_ms: u32,
) -> Outcome {
    let timeout = Duration::from_millis(u64::from(ready_timeout_ms));
    tokio::select! {
        biased;
        _ = &mut cancel => return Outcome::Aborted,
        r = ready => if r.is_err() { return Outcome::Aborted },
        _ = tokio::time::sleep(timeout) => return Outcome::SkippedTimeout,
    }
    // 第一个 Step 的 delay 就是 initial_delay,这里不再多等。
    let plan = steps.into_iter().map(|s| (s.delay, s.bytes)).collect();
    match write_scheduled(sink, plan, cancel).await {
        ScheduleOutcome::Completed => Outcome::Completed,
        ScheduleOutcome::Cancelled => Outcome::Aborted,
        ScheduleOutcome::Disconnected => Outcome::Disconnected,
        ScheduleOutcome::Congested => Outcome::Congested,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_ssh::session::TrySendErr;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Sink(Mutex<Vec<Vec<u8>>>);
    impl ByteSink for Sink {
        fn write(&self, b: Vec<u8>) -> Result<(), TrySendErr> {
            self.0.lock().unwrap().push(b);
            Ok(())
        }
    }

    fn step() -> Vec<Step> {
        vec![Step {
            delay: Duration::from_millis(300),
            bytes: b"attach\r".to_vec(),
        }]
    }

    #[tokio::test(start_paused = true)]
    async fn nothing_is_sent_before_the_first_byte_and_it_goes_out_after() {
        let sink = Arc::new(Sink::default());
        let (ready_tx, ready) = oneshot::channel();
        let (_cancel_tx, cancel) = oneshot::channel();
        let task = tokio::spawn(run(sink.clone(), step(), ready, cancel, 15_000));
        // 远端还没出声:等多久都不发。
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert!(sink.0.lock().unwrap().is_empty());
        ready_tx.send(()).unwrap();
        assert_eq!(task.await.unwrap(), Outcome::Completed);
        assert_eq!(*sink.0.lock().unwrap(), vec![b"attach\r".to_vec()]);
    }

    #[tokio::test(start_paused = true)]
    async fn no_first_byte_within_the_limit_sends_nothing() {
        let sink = Arc::new(Sink::default());
        let (_ready_tx, ready) = oneshot::channel();
        let (_cancel_tx, cancel) = oneshot::channel();
        let out = run(sink.clone(), step(), ready, cancel, 15_000).await;
        assert_eq!(out, Outcome::SkippedTimeout);
        assert!(sink.0.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn the_user_typing_first_cancels_the_plan() {
        let sink = Arc::new(Sink::default());
        let (ready_tx, ready) = oneshot::channel();
        let (cancel_tx, cancel) = oneshot::channel();
        ready_tx.send(()).unwrap();
        // 首字节到了、initial_delay 还没过,用户先敲了键。
        cancel_tx.send(()).unwrap();
        let out = run(sink.clone(), step(), ready, cancel, 15_000).await;
        assert_eq!(out, Outcome::Aborted);
        assert!(sink.0.lock().unwrap().is_empty());
    }
}
