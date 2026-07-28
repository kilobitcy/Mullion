//! russh 连接 + 认证 + 主机校验 + PTY io task。
//! ssh 只认字节流:不认识 pane/窗口,不碰 winit(唤醒经注入的 wake 回调)。

use std::sync::{Arc, Mutex};

use russh::client::{self, AuthResult, Handle};
use russh::keys::ssh_key;
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::config::{AuthMethod, SshConfig};
use crate::error::{classify_tcp, ConnectError};
#[cfg(test)]
use crate::known_hosts::HostKeyFuture;
use crate::known_hosts::{Fingerprint, HostKeyDecision, HostKeyOutcome, HostKeyPolicy};

/// russh 客户端 Handler:只负责主机密钥校验(TOFU,F3)。
/// 被拒原因经共享 `outcome` 回传给 establish(Handler 会被 move 进 connect_stream)。
/// 必须 pub:establish/connect 是 pub 且签名里出现 Handle<ClientHandler>,私有类型出现在
/// 公开接口会 E0446。字段保持私有 → 对外不透明。
pub struct ClientHandler {
    host: String,
    policy: Arc<dyn HostKeyPolicy>,
    outcome: Arc<Mutex<Option<HostKeyOutcome>>>,
}

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &ssh_key::PublicKey) -> Result<bool, Self::Error> {
        let fp = Fingerprint::from_public_key(key);
        // 算法名给上层弹窗展示;只承诺指纹与 `ssh-keygen -lf` 第二列同格式可核对——
        // algo 是协议 wire 名(如 "ssh-ed25519"),`ssh-keygen -lf` 展示的是括号里的
        // 短名(如 "(ED25519)"),两者不同,不该让用户逐字比对 algo。
        let algo = key.algorithm().to_string();
        // 弹窗策略会在这里挂起,等用户回答——sshd 的 LoginGraceTime(默认 120s)
        // 是这里能等多久的上限,超时对端会直接断开。
        match self.policy.decide(&self.host, &algo, &fp).await {
            HostKeyDecision::Accept => Ok(true),
            HostKeyDecision::Reject(o) => {
                *self.outcome.lock().unwrap_or_else(|e| e.into_inner()) = Some(o);
                Ok(false) // russh 据此中止握手
            }
        }
    }
}

/// 其余 russh 传输错误 → Io(兜底)。认证被拒走 AuthResult::Failure,不经此。
fn map_russh(e: russh::Error) -> ConnectError {
    ConnectError::Io(e.to_string())
}

/// 若握手期记录了主机密钥被拒原因,产出精确错误;否则按传输错误兜底。
fn host_key_or(outcome: &Arc<Mutex<Option<HostKeyOutcome>>>, e: russh::Error) -> ConnectError {
    match outcome.lock().expect("outcome poisoned").take() {
        Some(HostKeyOutcome::Changed {
            host,
            expected,
            got,
        }) => ConnectError::HostKeyChanged {
            host,
            expected,
            got,
        },
        Some(HostKeyOutcome::Unknown { host, got }) => ConnectError::HostKeyUnknown { host, got },
        None => map_russh(e),
    }
}

/// 连接 + 认证 + 主机校验。成功返回存活的 russh Handle(PTY 由 connect 接)。
pub async fn establish(
    cfg: &SshConfig,
    policy: Arc<dyn HostKeyPolicy>,
) -> Result<Handle<ClientHandler>, ConnectError> {
    // 1) DNS:此步任何失败都归 DnsResolution。
    let mut addrs = tokio::net::lookup_host((cfg.host.as_str(), cfg.port))
        .await
        .map_err(|e| ConnectError::DnsResolution(e.to_string()))?;
    let addr = addrs
        .next()
        .ok_or_else(|| ConnectError::DnsResolution(format!("{} 无解析结果", cfg.host)))?;

    // 2) TCP:分类 refused / 其他 io(F6)。
    let stream = TcpStream::connect(addr).await.map_err(classify_tcp)?;
    // 手搓 connect_stream 绕过了 client::connect 对 Config.nodelay 的应用,
    // 须补上,否则 Nagle 算法拖慢每次小写入 —— 与高延迟链路「跟手」的目标冲突。
    stream
        .set_nodelay(true)
        .map_err(|e| ConnectError::Io(format!("set_nodelay 失败: {e}")))?;

    // 3) 握手:触发 check_server_key(TOFU)。
    let outcome = Arc::new(Mutex::new(None));
    let handler = ClientHandler {
        host: cfg.host.clone(),
        policy,
        outcome: outcome.clone(),
    };
    let config = Arc::new(client::Config::default());
    let mut handle = client::connect_stream(config, stream, handler)
        .await
        .map_err(|e| host_key_or(&outcome, e))?;

    // 4) 认证。
    let result = authenticate(&mut handle, cfg).await?;
    match result {
        AuthResult::Success => Ok(handle),
        AuthResult::Failure { .. } => Err(ConnectError::AuthFailed),
    }
}

/// 公钥/agent 认证用的签名 hash 算法。**RSA 必须用 rsa-sha2-512**:传 `None` 会退化成
/// 废弃的 ssh-rsa(SHA-1),现代 OpenSSH 默认拒收 → AuthFailed(真机 RSA 钥匙实测暴露)。
/// 非 RSA 密钥(ed25519 等)russh 会忽略此 hash_alg。pubkey 与 agent 两条路径共用此常量,
/// 避免只改一处漏另一处;守护测试见本文件 tests::rsa_pubkey_hash_is_sha2_512_not_legacy_sha1。
const PUBKEY_HASH: Option<russh::keys::HashAlg> = Some(russh::keys::HashAlg::Sha512);

async fn authenticate(
    handle: &mut Handle<ClientHandler>,
    cfg: &SshConfig,
) -> Result<AuthResult, ConnectError> {
    match &cfg.auth {
        AuthMethod::Password(pw) => handle
            .authenticate_password(&cfg.user, pw)
            .await
            .map_err(map_russh),
        AuthMethod::PublicKey { path, passphrase } => {
            let key = russh::keys::load_secret_key(path, passphrase.as_deref())
                .map_err(|e| ConnectError::Io(format!("读私钥失败: {e}")))?;
            let with = russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key), PUBKEY_HASH);
            handle
                .authenticate_publickey(&cfg.user, with)
                .await
                .map_err(map_russh)
        }
        AuthMethod::Agent => authenticate_agent(handle, &cfg.user).await,
    }
}

/// ssh-agent 认证:从 SSH_AUTH_SOCK 取身份逐个尝试。
/// 仅 live 测试覆盖。注意:`authenticate_publickey_with` 的错误类型是
/// `Signer::Error`(此处即 `AgentAuthError`),不是 `russh::Error`,
/// 故用内联闭包映射,不能复用 map_russh。
async fn authenticate_agent(
    handle: &mut Handle<ClientHandler>,
    user: &str,
) -> Result<AuthResult, ConnectError> {
    // `AgentClient::connect_env()`(经 SSH_AUTH_SOCK 的 unix socket)仅 Unix 提供。
    // Windows 的 ssh-agent 走命名管道(\\.\pipe\openssh-ssh-agent),接口不同,MVP 暂未接;
    // 给一条可操作的错误而非让 Windows 构建编不过(F6)。用户实际走 -i(pubkey)两平台皆可。
    #[cfg(not(unix))]
    {
        let _ = (handle, user);
        Err(ConnectError::Io(
            "ssh-agent 认证目前仅支持 Unix;Windows 请用 -i 指定私钥".into(),
        ))
    }
    #[cfg(unix)]
    {
        let mut agent = russh::keys::agent::client::AgentClient::connect_env()
            .await
            .map_err(|e| ConnectError::Io(format!("连 ssh-agent 失败: {e}")))?;
        let identities = agent
            .request_identities()
            .await
            .map_err(|e| ConnectError::Io(format!("取 agent 身份失败: {e}")))?;
        // 逐个身份试;成功即返回,否则记住最后一次结果交上层映射为 AuthFailed。
        let mut last: Option<AuthResult> = None;
        for id in identities {
            // 有意在首个签名错误即中止整个尝试(用 `?`,不 continue):
            // Signer::Error(AgentAuthError) 代表 agent/传输故障(agent 掉线、协议错),
            // 不是「这把钥匙服务端不认」——那种情况服务端会正常回 AuthResult::Failure,
            // 走下面的 last 分支,不会走到这里。
            // PUBKEY_HASH(Sha512):同 pubkey 路径,RSA 必须走 rsa-sha2 而非废弃 ssh-rsa(SHA-1)。
            let res = handle
                .authenticate_publickey_with(user, id, PUBKEY_HASH, &mut agent)
                .await
                .map_err(|e| ConnectError::Io(format!("agent 签名失败: {e}")))?;
            if matches!(res, AuthResult::Success) {
                return Ok(res);
            }
            last = Some(res);
        }
        last.ok_or(ConnectError::AuthFailed)
    }
}

/// 交给 io task 的命令。
enum SshCmd {
    Write(Vec<u8>),
    Resize(u16, u16),
}

/// 句柄非阻塞发命令的失败原因。
#[derive(Debug, PartialEq)]
pub enum TrySendErr {
    /// 出站队列已满(粘贴大段 + 慢链路;本切片键鼠几乎不触发)。
    Full,
    /// 对端已关闭。
    Closed,
}

fn map_try_send<T>(e: mpsc::error::TrySendError<T>) -> TrySendErr {
    match e {
        mpsc::error::TrySendError::Full(_) => TrySendErr::Full,
        mpsc::error::TrySendError::Closed(_) => TrySendErr::Closed,
    }
}

/// 一条存活的 SSH PTY 会话句柄。winit 线程可直接调 write/resize(非阻塞)。
pub struct SshSession {
    cmd_tx: mpsc::Sender<SshCmd>,
}

impl SshSession {
    /// 把字节回写对端(键鼠编码 / take_pty_writes 的回写,T1)。非阻塞。
    pub fn write(&self, bytes: Vec<u8>) -> Result<(), TrySendErr> {
        self.cmd_tx
            .try_send(SshCmd::Write(bytes))
            .map_err(map_try_send)
    }

    /// reflow 后同步 PTY 尺寸(window_change,F34/T4)。非阻塞。
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), TrySendErr> {
        self.cmd_tx
            .try_send(SshCmd::Resize(cols, rows))
            .map_err(map_try_send)
    }
}

/// 连接 + 认证 + 开 PTY + 起 io task。返回句柄与「远端字节」接收端(app 每帧 drain)。
/// `wake` 由 app 注入(EventLoopProxy.send_event);ssh 不认识 winit。
pub async fn connect(
    cfg: &SshConfig,
    policy: Arc<dyn HostKeyPolicy>,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> Result<(SshSession, mpsc::Receiver<Vec<u8>>), ConnectError> {
    let handle = establish(cfg, policy).await?;

    let channel = handle
        .channel_open_session()
        .await
        .map_err(|_| ConnectError::PtyRequest)?;
    channel
        .request_pty(true, &cfg.term, cfg.cols as u32, cfg.rows as u32, 0, 0, &[])
        .await
        .map_err(|_| ConnectError::PtyRequest)?;
    channel
        .request_shell(true)
        .await
        .map_err(|_| ConnectError::PtyRequest)?;

    // 拆读写半:read.wait()(&mut) 与 write.data()(&) 同任务 select! 不冲突。
    let (read, write) = channel.split();

    let (inbound_tx, inbound_rx) = mpsc::channel::<Vec<u8>>(256);
    let (cmd_tx, cmd_rx) = mpsc::channel::<SshCmd>(256);

    tokio::spawn(io_task(read, write, cmd_rx, inbound_tx, wake, handle));

    Ok((SshSession { cmd_tx }, inbound_rx))
}

async fn io_task(
    mut read: russh::ChannelReadHalf,
    write: russh::ChannelWriteHalf<client::Msg>,
    mut cmd_rx: mpsc::Receiver<SshCmd>,
    inbound_tx: mpsc::Sender<Vec<u8>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    _handle: Handle<ClientHandler>, // 持有以保活连接;drop 即断连
) {
    loop {
        // 单任务顺序 select:inbound send().await 期间不处理 cmd(大 burst 下键入有延迟,非死锁)。
        // Task 8 接 app 侧泵时复审是否需拆读/写双任务。
        tokio::select! {
            msg = read.wait() => match msg {
                Some(russh::ChannelMsg::Data { data }) => {
                    if inbound_tx.send(data.to_vec()).await.is_err() {
                        break; // app 侧接收端已丢
                    }
                    wake();
                }
                Some(russh::ChannelMsg::Eof) | Some(russh::ChannelMsg::Close) | None => {
                    wake(); // 远端断线也要唤醒 app,否则 S3 断连要等无关重绘才被发现
                    break;
                }
                _ => {} // ExitStatus / WindowAdjusted 等骨架先忽略
            },
            cmd = cmd_rx.recv() => match cmd {
                Some(SshCmd::Write(b)) => {
                    let _ = write.data(&b[..]).await;
                }
                Some(SshCmd::Resize(c, r)) => {
                    let _ = write.window_change(c as u32, r as u32, 0, 0).await;
                }
                None => {
                    let _ = write.eof().await;
                    break; // 所有句柄已 drop
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rsa_pubkey_hash_is_sha2_512_not_legacy_sha1() {
        // 守护 fix 9fd5174/0dd99e0:RSA 公钥必须走 rsa-sha2-512。若把 PUBKEY_HASH 改回
        // None,组合算法会退化成 "ssh-rsa"(SHA-1),现代 sshd 拒收,此断言即红。
        // (hermetic 只有 ed25519 fixture、live 又是 #[ignore],故用此纯单测锁住选择。)
        let key = russh::keys::load_secret_key("tests/fixtures/rsa_key", None).unwrap();
        let signer = russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key), PUBKEY_HASH);
        assert_eq!(
            signer.algorithm().as_str(),
            "rsa-sha2-512",
            "RSA 公钥退化成了废弃算法(SHA-1),现代 sshd 会拒(F1)"
        );
    }

    struct AlwaysAccept;
    impl HostKeyPolicy for AlwaysAccept {
        fn decide<'a>(
            &'a self,
            _host: &'a str,
            _algo: &'a str,
            _fp: &'a Fingerprint,
        ) -> HostKeyFuture<'a> {
            Box::pin(std::future::ready(HostKeyDecision::Accept))
        }
    }

    /// 故意在回答前 yield 一次:证明 `check_server_key` 真的 await 得下去,
    /// 而不是只在「策略立刻就绪」的情况下碰巧能跑(弹窗版一定不是立刻就绪)。
    struct RejectAfterYield;
    impl HostKeyPolicy for RejectAfterYield {
        fn decide<'a>(
            &'a self,
            host: &'a str,
            _algo: &'a str,
            fp: &'a Fingerprint,
        ) -> HostKeyFuture<'a> {
            let outcome = HostKeyOutcome::Unknown {
                host: host.to_owned(),
                got: fp.clone(),
            };
            Box::pin(async move {
                tokio::task::yield_now().await;
                HostKeyDecision::Reject(outcome)
            })
        }
    }

    use russh::client::Handler as _;

    fn handler(
        policy: Arc<dyn HostKeyPolicy>,
    ) -> (ClientHandler, Arc<Mutex<Option<HostKeyOutcome>>>) {
        let outcome = Arc::new(Mutex::new(None));
        (
            ClientHandler {
                host: "h".into(),
                policy,
                outcome: outcome.clone(),
            },
            outcome,
        )
    }

    fn test_pubkey() -> ssh_key::PublicKey {
        russh::keys::load_secret_key("tests/fixtures/client_key", None)
            .unwrap()
            .public_key()
            .clone()
    }

    #[tokio::test]
    async fn policy_accept_completes_handshake() {
        let (mut h, outcome) = handler(Arc::new(AlwaysAccept));
        assert!(h.check_server_key(&test_pubkey()).await.unwrap());
        assert!(outcome.lock().unwrap().is_none(), "放行不该记拒绝原因");
    }

    #[tokio::test]
    async fn policy_reject_aborts_handshake_and_records_reason() {
        // F3:策略拒绝必须让 russh 中止握手(Ok(false)),并把原因留给 establish
        // 翻译成可操作错误——否则用户只看到一句无从下手的传输错误。
        let (mut h, outcome) = handler(Arc::new(RejectAfterYield));
        assert!(!h.check_server_key(&test_pubkey()).await.unwrap());
        assert!(matches!(
            outcome.lock().unwrap().take(),
            Some(HostKeyOutcome::Unknown { .. })
        ));
    }

    /// 记下策略实际收到的 algo:参数顺序写反 / 误传 host 都会在这里暴露。
    /// 这个字符串将来要显示在 F3 确认弹窗上给用户核对,不能是错的。
    struct RecordAlgo(Arc<Mutex<Option<String>>>);
    impl HostKeyPolicy for RecordAlgo {
        fn decide<'a>(
            &'a self,
            _host: &'a str,
            algo: &'a str,
            _fp: &'a Fingerprint,
        ) -> HostKeyFuture<'a> {
            *self.0.lock().unwrap() = Some(algo.to_owned());
            Box::pin(std::future::ready(HostKeyDecision::Accept))
        }
    }

    #[tokio::test]
    async fn policy_receives_the_real_key_algorithm_name() {
        let seen = Arc::new(Mutex::new(None));
        let (mut h, _) = handler(Arc::new(RecordAlgo(seen.clone())));
        h.check_server_key(&test_pubkey()).await.unwrap();
        assert_eq!(seen.lock().unwrap().as_deref(), Some("ssh-ed25519"));
    }

    struct AlwaysChanged;
    impl HostKeyPolicy for AlwaysChanged {
        fn decide<'a>(
            &'a self,
            host: &'a str,
            _algo: &'a str,
            fp: &'a Fingerprint,
        ) -> HostKeyFuture<'a> {
            let outcome = HostKeyOutcome::Changed {
                host: host.to_owned(),
                expected: fp.clone(),
                got: fp.clone(),
            };
            Box::pin(std::future::ready(HostKeyDecision::Reject(outcome)))
        }
    }

    #[tokio::test]
    async fn policy_reject_changed_also_aborts_and_records_changed() {
        // F3 红线:指纹变更这条路径必须与 Unknown 一样中止握手并留下原因,
        // 否则 establish 翻译不出「疑似中间人」那句可操作的错误。
        let (mut h, outcome) = handler(Arc::new(AlwaysChanged));
        assert!(!h.check_server_key(&test_pubkey()).await.unwrap());
        assert!(matches!(
            outcome.lock().unwrap().take(),
            Some(HostKeyOutcome::Changed { .. })
        ));
    }
}
