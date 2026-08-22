//! 主机密钥确认(F3):SSH 握手线程 ↔ GUI 线程之间的「挂起—回答」桥。
//!
//! 判断逻辑抽成纯函数 `check`,弹窗与 async 只是它的壳——这样 F3 的红线
//! (指纹变更必须拦下)能在没有窗口、没有 SSH 的情况下单测。

use std::sync::{Arc, Mutex};

use mullion_ssh::known_hosts::{
    Fingerprint, HostKeyDecision, HostKeyFuture, HostKeyOutcome, HostKeyPolicy,
};
use mullion_store::known_hosts::{HostKeyEntry, KnownHostsFile};
use tokio::sync::oneshot;
use winit::event_loop::EventLoopProxy;

use crate::app::UserEvent;

/// 一次待用户回答的主机密钥确认。经 `UserEvent::HostKeyPrompt` 送到 GUI 线程,
/// 用户点完把 bool 从 `reply` 送回,握手线程随即恢复。
pub struct HostKeyPrompt {
    /// TOFU 表的键,也是弹窗上「主机」那一行 —— 非默认端口写成
    /// `[host]:port`(`mullion_ssh::known_hosts::host_key_id`)。
    pub host: String,
    /// 形如 `ssh-ed25519`,供用户核对时对上 `ssh-keygen -lf` 的输出。
    pub algo: String,
    /// `SHA256:<base64>`。
    pub fingerprint: String,
    /// 存档里的旧记录;`Some` = 指纹变更(高危,UI 走警告态)。
    pub previous: Option<HostKeyEntry>,
    /// 用户接受后是否写 known_hosts。正式连接 `true`;
    /// 「测试连接」(F92)一律 `false` —— 探路不是承诺。
    pub persist: bool,
    pub reply: oneshot::Sender<bool>,
}

/// 一次检查的结论。
#[derive(Debug, PartialEq, Eq)]
pub enum HostKeyCheck {
    /// 已记录且一致 —— 直接放行,不打扰用户。
    Trusted,
    /// 需要用户确认。`previous = Some` 表示指纹变更。
    NeedsPrompt { previous: Option<HostKeyEntry> },
}

/// F3 的全部判断逻辑(纯函数,见模块头注释)。
///
/// `host_id` 是 ssh 层组好的端点键(含非默认端口),原样当表键用 ——
/// 这里再做任何加工都会让写入与读取的拼法漂移。
pub fn check(known: &KnownHostsFile, host_id: &str, fingerprint: &str) -> HostKeyCheck {
    match known.get(host_id) {
        Some(e) if e.fingerprint == fingerprint => HostKeyCheck::Trusted,
        Some(e) => HostKeyCheck::NeedsPrompt {
            previous: Some(e.clone()),
        },
        None => HostKeyCheck::NeedsPrompt { previous: None },
    }
}

/// 弹窗版主机密钥策略:未知/变更 → 送弹窗事件并在握手里 await 用户回答。
///
/// `proxy` 包 `Mutex` 的原因:winit 0.30 只给 `EventLoopProxy` 实现了 `Send`,
/// 没实现 `Sync`(`platform_impl` 里是个 `Sender`);而 `Arc<dyn HostKeyPolicy>`
/// 要求 `Send + Sync`。`Mutex<T>: Sync where T: Send` 正好补上,锁只在
/// `send_event` 那一瞬持有,**绝不跨 await**。
pub struct PromptingPolicy {
    known: Arc<Mutex<KnownHostsFile>>,
    proxy: Mutex<EventLoopProxy<UserEvent>>,
    /// 透传给 `HostKeyPrompt::persist`,决定用户接受后是否落盘。
    persist: bool,
}

impl PromptingPolicy {
    pub fn new(
        known: Arc<Mutex<KnownHostsFile>>,
        proxy: EventLoopProxy<UserEvent>,
        persist: bool,
    ) -> Self {
        Self {
            known,
            proxy: Mutex::new(proxy),
            persist,
        }
    }
}

impl HostKeyPolicy for PromptingPolicy {
    fn decide<'a>(
        &'a self,
        host_id: &'a str,
        algo: &'a str,
        fp: &'a Fingerprint,
    ) -> HostKeyFuture<'a> {
        let text = fp.to_ssh_string();
        // 锁的作用域收到最小:后面要 await,std 的 MutexGuard 不能跨 await。
        //
        // `known` 是全 App 共享的单例锁(main.rs 建一次,所有连接共享),而
        // `spawn_connect` 用 `self._runtime.spawn` 起的任务其 `JoinHandle` 未被
        // 消费(app.rs)——一旦这里 panic,tokio 会吞掉它,`decide` 永远不
        // 返回,连接卡死不说,这把锁还从此**永久**中毒:此后每次连接都在同一处
        // panic,TOFU 对所有主机失效。表内只是 `BTreeMap::get`+`clone`,
        // panic-safe,中毒后继续用不会读到半成品,选择恢复而不是 expect。
        let outcome_of_check = {
            let known = self.known.lock().unwrap_or_else(|e| e.into_inner());
            check(&known, host_id, &text)
        };
        let previous = match outcome_of_check {
            HostKeyCheck::Trusted => {
                return Box::pin(std::future::ready(HostKeyDecision::Accept));
            }
            HostKeyCheck::NeedsPrompt { previous } => previous,
        };
        // 用户不同意 / 弹窗送不到 / GUI 先退出时的统一拒绝理由。
        let rejection = match &previous {
            Some(e) => HostKeyOutcome::Changed {
                host: host_id.to_owned(),
                // 存档里是文本;解析不出(文件被手改坏)只影响错误消息里的展示,
                // 不影响「拒绝」这个判定本身。
                expected: Fingerprint::parse_ssh(&e.fingerprint)
                    .unwrap_or_else(|| Fingerprint(Vec::new())),
                got: fp.clone(),
            },
            None => HostKeyOutcome::Unknown {
                host: host_id.to_owned(),
                got: fp.clone(),
            },
        };
        let (tx, rx) = oneshot::channel();
        // 同上:proxy 也是共享单例,同一条 panic-safe 理由,恢复而不是 expect。
        let sent = self
            .proxy
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send_event(UserEvent::HostKeyPrompt(Box::new(HostKeyPrompt {
                host: host_id.to_owned(),
                algo: algo.to_owned(),
                fingerprint: text,
                previous,
                persist: self.persist,
                reply: tx,
            })))
            .is_ok();
        Box::pin(await_reply(sent, rx, rejection))
    }
}

/// 后台重连专用的主机密钥策略(F114):**只信已记录且一致的主机**,
/// 未知与变更一律拒绝,不弹任何窗。
///
/// 这不是「更严格一点」的 `PromptingPolicy`,而是设计 D7 那两条硬约束的
/// 结构性兑现 —— 注意本类型**根本没有 `proxy` 字段**:
///
/// 1. 「需要交互的凭据不自动重连」:发不出 `HostKeyPrompt` 事件,
///    后台隧道在类型上就不可能在用户面前糊一屏模态框。
/// 2. 「指纹变更立即停止、绝不重试」:变更走 `Reject(Changed)`,
///    `establish` 会翻成 `ConnectError::HostKeyChanged`,而
///    `tunnel::is_fatal` 认它是致命错误 —— 一次都不会重试。
///
/// 首次启动隧道用的仍然是 `PromptingPolicy`:那时用户刚点完「启动」,
/// 人就在屏幕前,TOFU 确认是有意义的。
pub struct TrustedOnlyPolicy {
    known: Arc<Mutex<KnownHostsFile>>,
}

impl TrustedOnlyPolicy {
    pub fn new(known: Arc<Mutex<KnownHostsFile>>) -> Self {
        Self { known }
    }
}

impl HostKeyPolicy for TrustedOnlyPolicy {
    fn decide<'a>(
        &'a self,
        host_id: &'a str,
        _algo: &'a str,
        fp: &'a Fingerprint,
    ) -> HostKeyFuture<'a> {
        let text = fp.to_ssh_string();
        // 锁只在读表那一瞬持有,不跨 await(同 `PromptingPolicy` 的理由);
        // 中毒后继续用也只是 `BTreeMap::get`,panic-safe。
        let outcome = {
            let known = self.known.lock().unwrap_or_else(|e| e.into_inner());
            check(&known, host_id, &text)
        };
        let decision = match outcome {
            HostKeyCheck::Trusted => HostKeyDecision::Accept,
            HostKeyCheck::NeedsPrompt {
                previous: Some(e), ..
            } => HostKeyDecision::Reject(HostKeyOutcome::Changed {
                host: host_id.to_owned(),
                expected: Fingerprint::parse_ssh(&e.fingerprint)
                    .unwrap_or_else(|| Fingerprint(Vec::new())),
                got: fp.clone(),
            }),
            HostKeyCheck::NeedsPrompt { previous: None } => {
                HostKeyDecision::Reject(HostKeyOutcome::Unknown {
                    host: host_id.to_owned(),
                    got: fp.clone(),
                })
            }
        };
        Box::pin(std::future::ready(decision))
    }
}

/// fail-closed 的全部判定:只有用户明确点「接受」才放行。
///
/// 抽成独立函数是为了能单测——`decide` 依赖 `EventLoopProxy`,而它只能由
/// `EventLoop::create_proxy` 创建,无窗口环境构造不出来;这段判定本身不依赖
/// proxy/存档,抽出来就能脱离窗口单测(F3 红线,`decide` 之前是零覆盖)。
async fn await_reply(
    sent: bool,
    rx: oneshot::Receiver<bool>,
    rejection: HostKeyOutcome,
) -> HostKeyDecision {
    // fail-closed:事件循环已关闭(send_event Err)或 sender 被丢弃
    // (rx.await Err,例如 GUI 退出/旧弹窗被新的顶掉)一律当拒绝。任何
    // 「送不到就放行」的写法都会让 MITM 只要能让 GUI 崩一下就过关。
    if !sent {
        return HostKeyDecision::Reject(rejection);
    }
    match rx.await {
        Ok(true) => HostKeyDecision::Accept,
        _ => HostKeyDecision::Reject(rejection),
    }
}

/// 用户回答完主机密钥弹窗后,决定是否把指纹落盘。
///
/// 抽成独立函数是为了能单测:真正的施加点在 `app.rs` 的 `render_frame`
/// 深处,无窗口环境根本调不到,写在那里的门控等于零覆盖。
///
/// `persist == false`(F92 拨测)时**什么都不做**——拨测是探路,不是承诺。
pub fn persist_if_allowed(
    known: &Mutex<KnownHostsFile>,
    prompt: &HostKeyPrompt,
    accept: bool,
) -> Result<(), mullion_store::StoreError> {
    if !(accept && prompt.persist) {
        return Ok(());
    }
    crate::diag::mark(crate::diag::Stage::StoreIo);
    // 与既有施加点同源:GUI 线程 panic 比 tokio 线程更糟(直接崩窗口),
    // 锁中毒时恢复而不是 expect。
    let mut kh = known.lock().unwrap_or_else(|e| e.into_inner());
    kh.record(
        &prompt.host,
        HostKeyEntry {
            algo: prompt.algo.clone(),
            fingerprint: prompt.fingerprint.clone(),
        },
    );
    kh.save()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_store::known_hosts::HostKeyEntry;

    fn entry(fp: &str) -> HostKeyEntry {
        HostKeyEntry {
            algo: "ssh-ed25519".into(),
            fingerprint: fp.into(),
        }
    }

    #[test]
    fn unknown_host_requires_prompt_without_previous() {
        let known = KnownHostsFile::default();
        assert_eq!(
            check(&known, "h", "SHA256:AAAA"),
            HostKeyCheck::NeedsPrompt { previous: None }
        );
    }

    #[test]
    fn known_and_matching_is_trusted_without_prompt() {
        // 每次连都弹窗 = 用户学会闭眼点「接受」,TOFU 就废了。
        let mut known = KnownHostsFile::default();
        known.record("h", entry("SHA256:AAAA"));
        assert_eq!(check(&known, "h", "SHA256:AAAA"), HostKeyCheck::Trusted);
    }

    #[test]
    fn changed_fingerprint_requires_prompt_carrying_previous() {
        // F3 红线:指纹变了必须拦下并把旧指纹一并交给 UI 做对比展示。
        let mut known = KnownHostsFile::default();
        known.record("h", entry("SHA256:AAAA"));
        assert_eq!(
            check(&known, "h", "SHA256:BBBB"),
            HostKeyCheck::NeedsPrompt {
                previous: Some(entry("SHA256:AAAA"))
            }
        );
    }

    /// 拿一份真公钥算出的 `Fingerprint` 太重(要读 fixture),而
    /// `to_ssh_string()` 对非 32 字节的输入会退化成十六进制 —— 这里只需要
    /// 「同一份字节 → 同一个字符串」这一条性质,所以直接造字节。
    fn fp(bytes: &[u8]) -> Fingerprint {
        Fingerprint(bytes.to_vec())
    }

    /// F114 的安全闸门。三条状态都要断言:
    /// - 已记录且一致 → 放行(否则重连功能整个不存在,断一次网就永久停)
    /// - 未记录 → 拒绝(需要人确认,后台不能替用户答应)
    /// - 已变更 → 拒绝且带上 `Changed`(`tunnel::is_fatal` 靠这个变体
    ///   把它判成致命,一次都不重试)
    ///
    /// 「不弹窗」这件事**由类型保证**:`TrustedOnlyPolicy` 没有
    /// `EventLoopProxy` 字段,想发 `HostKeyPrompt` 都发不出去。
    #[tokio::test]
    async fn trusted_only_policy_rejects_unknown_and_changed_hosts_without_prompting() {
        let mut file = KnownHostsFile::default();
        let good = fp(b"AAAA");
        file.record("h", entry(&good.to_ssh_string()));
        let policy = TrustedOnlyPolicy::new(Arc::new(Mutex::new(file)));

        assert!(
            matches!(
                policy.decide("h", "ssh-ed25519", &good).await,
                HostKeyDecision::Accept
            ),
            "已记录且一致必须放行,否则隧道断一次网就再也回不来"
        );
        assert!(
            matches!(
                policy.decide("h", "ssh-ed25519", &fp(b"BBBB")).await,
                HostKeyDecision::Reject(HostKeyOutcome::Changed { .. })
            ),
            "指纹变更必须拒且标成 Changed —— tunnel::is_fatal 靠它判致命"
        );
        assert!(
            matches!(
                policy.decide("newhost", "ssh-ed25519", &good).await,
                HostKeyDecision::Reject(HostKeyOutcome::Unknown { .. })
            ),
            "没记录过的主机后台不能替用户答应"
        );
    }

    fn sample_rejection() -> HostKeyOutcome {
        HostKeyOutcome::Unknown {
            host: "h".into(),
            got: Fingerprint(b"AAAA".to_vec()),
        }
    }

    #[tokio::test]
    async fn dropped_sender_is_rejected() {
        // GUI 退出 / 旧弹窗被新的顶掉时 sender 被 drop——rx.await 必须 Err,
        // 必须拒绝,不能悬挂或放行。
        let (tx, rx) = oneshot::channel::<bool>();
        drop(tx);
        let decision = await_reply(true, rx, sample_rejection()).await;
        assert!(
            matches!(decision, HostKeyDecision::Reject(_)),
            "sender 被丢弃必须 Reject"
        );
    }

    #[tokio::test]
    async fn user_declines_is_rejected() {
        // 用户在弹窗里点「拒绝」→ send(false)。
        let (tx, rx) = oneshot::channel::<bool>();
        tx.send(false).unwrap();
        let decision = await_reply(true, rx, sample_rejection()).await;
        assert!(
            matches!(decision, HostKeyDecision::Reject(_)),
            "用户点拒绝必须 Reject"
        );
    }

    #[tokio::test]
    async fn send_event_failure_is_rejected() {
        // sent=false = 事件循环已关闭,弹窗根本没送到。这是 F3 红线:
        // 「送不到就放行」等于 MITM 只要能让 GUI 崩一下就过关。
        let (_tx, rx) = oneshot::channel::<bool>();
        let decision = await_reply(false, rx, sample_rejection()).await;
        assert!(
            matches!(decision, HostKeyDecision::Reject(_)),
            "send_event 失败必须 Reject,绝不能 fail-open"
        );
    }

    #[tokio::test]
    async fn user_accepts_is_accepted() {
        // 正向用例:唯一的放行路径——sent=true 且用户回了 true。
        let (tx, rx) = oneshot::channel::<bool>();
        tx.send(true).unwrap();
        let decision = await_reply(true, rx, sample_rejection()).await;
        assert!(
            matches!(decision, HostKeyDecision::Accept),
            "用户明确接受才能 Accept"
        );
    }

    /// 造一个可用的 `HostKeyPrompt`;`reply` 的接收端本次用例用不上,直接丢弃。
    fn sample_prompt(persist: bool) -> HostKeyPrompt {
        HostKeyPrompt {
            host: "h".into(),
            algo: "ssh-ed25519".into(),
            fingerprint: "SHA256:AAAA".into(),
            previous: None,
            persist,
            reply: oneshot::channel().0,
        }
    }

    #[test]
    fn probing_accept_never_persists() {
        // 本任务的核心守护:F92 拨测(persist=false)即使用户点了接受,
        // 也绝不能落盘——探路不是承诺,写盘 = 把陌生指纹永久钉死。
        let dir = tempfile::tempdir().unwrap();
        let known = Mutex::new(KnownHostsFile::load(dir.path()));
        let prompt = sample_prompt(false);

        persist_if_allowed(&known, &prompt, true).expect("persist=false 时不该报错");

        assert!(
            known.lock().unwrap().get("h").is_none(),
            "拨测确认后指纹不该进内存表"
        );
        assert!(
            !dir.path().join("known_hosts.toml").exists(),
            "拨测确认后不该生成 known_hosts.toml"
        );
    }

    #[test]
    fn normal_connection_accept_persists() {
        // 正向:persist=true(正式连接)+ 用户接受 → 必须真的落盘,
        // 否则无法区分「门控生效」和「函数整个坏掉」。
        let dir = tempfile::tempdir().unwrap();
        let known = Mutex::new(KnownHostsFile::load(dir.path()));
        let prompt = sample_prompt(true);

        persist_if_allowed(&known, &prompt, true).expect("落盘应成功");

        let entry = known
            .lock()
            .unwrap()
            .get("h")
            .cloned()
            .expect("正式连接接受后指纹应已记录");
        assert_eq!(entry.fingerprint, "SHA256:AAAA");
        assert!(
            dir.path().join("known_hosts.toml").exists(),
            "正式连接接受后应生成 known_hosts.toml"
        );
    }

    #[test]
    fn normal_connection_decline_does_not_persist() {
        // persist=true 但用户点了拒绝 → 同样不该落盘。
        let dir = tempfile::tempdir().unwrap();
        let known = Mutex::new(KnownHostsFile::load(dir.path()));
        let prompt = sample_prompt(true);

        persist_if_allowed(&known, &prompt, false).expect("accept=false 时不该报错");

        assert!(known.lock().unwrap().get("h").is_none());
        assert!(!dir.path().join("known_hosts.toml").exists());
    }
}
