//! 按时间表把字节写进某个 sink。**只认延时与字节**,不认识 tmux / 自动化 /
//! 会话——那些语义留在 `mullion-store` 的纯函数里(架构不变量:ssh 不依赖 store)。
//!
//! 定时靠 `tokio::time::sleep` 而**不是** app 的帧循环:堆进事件循环会与
//! 帧率节流打架(陷阱 T3/T7)。

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::oneshot;

use crate::session::{SshSession, TrySendErr};

/// 出站队列满时的重试次数(含首次尝试)。
const FULL_ATTEMPTS: u32 = 3;
/// 重试之间的退避。
const FULL_BACKOFF: Duration = Duration::from_millis(50);

/// 能收字节的东西。存在的唯一理由是**可测**:有了它就能用假 sink +
/// `tokio::time::pause()` 零网络验证顺序 / 延时 / 取消 / 断线即停。
pub trait ByteSink: Send + Sync {
    fn write(&self, bytes: Vec<u8>) -> Result<(), TrySendErr>;
}

impl ByteSink for SshSession {
    fn write(&self, bytes: Vec<u8>) -> Result<(), TrySendErr> {
        SshSession::write(self, bytes)
    }
}

/// 时间表跑完的结局。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleOutcome {
    /// 全部步骤都发完了。
    Completed,
    /// 被取消(用户接管,或取消端被 drop)。
    Cancelled,
    /// 对端已关闭(pane 关了 / 链路断了)。
    Disconnected,
    /// 出站队列持续满,放弃。
    Congested,
}

/// 依次「等 `delay` → 写 `bytes`」,直到跑完或被打断。
///
/// `cancel` 一旦就绪(收到值**或**发送端被 drop),立即停止,**剩余步骤一个字节
/// 都不再发**——这是「用户接管优先」的落点:用户已经开始打字,再插字节就是抢输入。
pub async fn write_scheduled(
    sink: Arc<dyn ByteSink>,
    steps: Vec<(Duration, Vec<u8>)>,
    mut cancel: oneshot::Receiver<()>,
) -> ScheduleOutcome {
    for (delay, bytes) in steps {
        tokio::select! {
            // 取消优先:同时就绪时先看取消,避免「刚被取消却又发了一步」。
            biased;
            _ = &mut cancel => return ScheduleOutcome::Cancelled,
            _ = tokio::time::sleep(delay) => {}
        }

        let mut attempt = 0u32;
        loop {
            match sink.write(bytes.clone()) {
                Ok(()) => break,
                Err(TrySendErr::Closed) => return ScheduleOutcome::Disconnected,
                Err(TrySendErr::Full) => {
                    attempt += 1;
                    if attempt >= FULL_ATTEMPTS {
                        return ScheduleOutcome::Congested;
                    }
                    tokio::select! {
                        biased;
                        _ = &mut cancel => return ScheduleOutcome::Cancelled,
                        _ = tokio::time::sleep(FULL_BACKOFF) => {}
                    }
                }
            }
        }
    }
    ScheduleOutcome::Completed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// 假 sink:记录收到的字节,可编程失败模式。**零网络**。
    #[derive(Default)]
    struct FakeSink {
        written: Mutex<Vec<Vec<u8>>>,
        /// 每次 write 都返回这个错(None = 一律成功)。
        fail_with: Option<TrySendErr>,
        /// 前 N 次返回 Full,之后成功。
        full_times: Mutex<u32>,
        /// 旁路计数器:统计 `write` 被调用的总次数,与 `full_times` 的业务语义
        /// 无关,专门用来钉死重试次数(不能只看最终结局)。
        calls: AtomicUsize,
    }

    impl FakeSink {
        fn written(&self) -> Vec<Vec<u8>> {
            self.written.lock().unwrap().clone()
        }
    }

    impl ByteSink for FakeSink {
        fn write(&self, bytes: Vec<u8>) -> Result<(), TrySendErr> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(e) = &self.fail_with {
                return Err(match e {
                    TrySendErr::Full => TrySendErr::Full,
                    TrySendErr::Closed => TrySendErr::Closed,
                });
            }
            let mut left = self.full_times.lock().unwrap();
            if *left > 0 {
                *left -= 1;
                return Err(TrySendErr::Full);
            }
            self.written.lock().unwrap().push(bytes);
            Ok(())
        }
    }

    fn steps() -> Vec<(Duration, Vec<u8>)> {
        vec![
            (Duration::from_millis(300), b"a\r".to_vec()),
            (Duration::from_millis(200), b"b\r".to_vec()),
        ]
    }

    #[tokio::test(start_paused = true)]
    async fn write_scheduled_respects_delays() {
        let fake = Arc::new(FakeSink::default());
        let sink: Arc<dyn ByteSink> = fake.clone();
        // 注意:必须用 `_tx` 这样的具名绑定持有 sender。写成裸 `_` 会当场 drop,
        // 接收端立刻就绪 → 整个计划被判定为「已取消」,测试会莫名其妙变红。
        let (_tx, rx) = tokio::sync::oneshot::channel();

        let start = tokio::time::Instant::now();
        let out = write_scheduled(sink, steps(), rx).await;

        assert_eq!(out, ScheduleOutcome::Completed);
        assert_eq!(
            fake.written(),
            vec![b"a\r".to_vec(), b"b\r".to_vec()],
            "顺序必须与时间表一致"
        );
        assert_eq!(
            start.elapsed(),
            Duration::from_millis(500),
            "延时应累加:300 + 200(假时钟,零真实等待)"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn write_scheduled_stops_on_cancel() {
        let fake = Arc::new(FakeSink::default());
        let sink: Arc<dyn ByteSink> = fake.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();

        let handle = tokio::spawn(write_scheduled(sink, steps(), rx));
        // 推进到第一步已发、第二步还在等的时刻。
        tokio::time::sleep(Duration::from_millis(350)).await;
        tx.send(()).unwrap();

        let out = handle.await.unwrap();
        assert_eq!(out, ScheduleOutcome::Cancelled);
        assert_eq!(
            fake.written(),
            vec![b"a\r".to_vec()],
            "取消后剩余步骤一个字节都不许发(用户接管优先)"
        );
    }

    /// sender 被 drop(pane 关了、状态机没了)等同取消 —— 不能傻等下去。
    #[tokio::test(start_paused = true)]
    async fn dropping_the_canceller_stops_the_schedule() {
        let fake = Arc::new(FakeSink::default());
        let sink: Arc<dyn ByteSink> = fake.clone();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        drop(tx);

        let out = write_scheduled(sink, steps(), rx).await;
        assert_eq!(out, ScheduleOutcome::Cancelled);
        assert!(fake.written().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn write_scheduled_stops_when_sink_closed() {
        let fake = Arc::new(FakeSink {
            fail_with: Some(TrySendErr::Closed),
            ..Default::default()
        });
        let sink: Arc<dyn ByteSink> = fake.clone();
        let (_tx, rx) = tokio::sync::oneshot::channel();

        let out = write_scheduled(sink, steps(), rx).await;
        assert_eq!(out, ScheduleOutcome::Disconnected, "链路断了就停,不重试");
        assert!(fake.written().is_empty());
    }

    /// 出站队列偶发满(粘贴大段 + 慢链路)应重试,而不是当场放弃。
    #[tokio::test(start_paused = true)]
    async fn transient_full_is_retried() {
        let fake = Arc::new(FakeSink {
            full_times: Mutex::new(2),
            ..Default::default()
        });
        let sink: Arc<dyn ByteSink> = fake.clone();
        let (_tx, rx) = tokio::sync::oneshot::channel();

        let out = write_scheduled(sink, vec![(Duration::ZERO, b"a\r".to_vec())], rx).await;
        assert_eq!(out, ScheduleOutcome::Completed);
        assert_eq!(fake.written(), vec![b"a\r".to_vec()]);
    }

    /// 一直满就放弃,绝不无限重试。
    #[tokio::test(start_paused = true)]
    async fn persistent_full_gives_up_as_congested() {
        let fake = Arc::new(FakeSink {
            fail_with: Some(TrySendErr::Full),
            ..Default::default()
        });
        let sink: Arc<dyn ByteSink> = fake.clone();
        let (_tx, rx) = tokio::sync::oneshot::channel();

        let out = write_scheduled(sink, vec![(Duration::ZERO, b"a\r".to_vec())], rx).await;
        assert_eq!(out, ScheduleOutcome::Congested);
        assert!(fake.written().is_empty());
        // 注意:此处故意写死字面量 3,不能写成 `FULL_ATTEMPTS as usize`——
        // 生产代码本身就是「攒够 FULL_ATTEMPTS 次才放弃」,用同一个常量当预期值
        // 会变成重言式,常量本身被改大时两边同步漂移,测试永远绿,钉不住任何东西。
        assert_eq!(
            fake.calls.load(Ordering::SeqCst),
            3,
            "一直满就放弃,重试次数必须由 FULL_ATTEMPTS 钉死(当前=3),不能悄悄放大"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn empty_plan_completes_without_writing() {
        let fake = Arc::new(FakeSink::default());
        let sink: Arc<dyn ByteSink> = fake.clone();
        let (_tx, rx) = tokio::sync::oneshot::channel();

        let out = write_scheduled(sink, Vec::new(), rx).await;
        assert_eq!(out, ScheduleOutcome::Completed);
        assert!(fake.written().is_empty());
    }
}
