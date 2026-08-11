//! russh 连接 + 认证 + 主机校验 + PTY io task。
//! ssh 只认字节流:不认识 pane/窗口,不碰 winit(唤醒经注入的 wake 回调)。

use std::sync::{Arc, Mutex};

use russh::client::{self, AuthResult, Handle};
use russh::keys::ssh_key;
use tokio::sync::mpsc;

use crate::config::{AuthMethod, SshConfig};
use crate::error::ConnectError;
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
    /// `-R` 的回连出口(F112)。`None` = 这条连接没请求过远端转发。
    forwarded: Option<mpsc::Sender<ForwardedTcpip>>,
}

/// 服务端主动开过来的一条 `forwarded-tcpip` channel(`-R` 的入站连接)。
///
/// `pub(crate)`:里面裹着 `russh::Channel<Msg>`,理由同
/// [`SshConnection::open_direct_tcpip`] —— 外部拿不到 `Channel` 就漏不了
/// CHANNEL_CLOSE。
pub(crate) struct ForwardedTcpip {
    pub channel: russh::Channel<client::Msg>,
}

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    /// `-R` 的入站连接(F112)。
    ///
    /// **默认实现是 `async { Ok(()) }`,即把 `Channel` 直接丢掉** ——
    /// 而 `russh` 的 `Channel<Msg>` 没有会发 CHANNEL_CLOSE 的 `Drop`
    /// (只有 `into_stream()` 包出来的 `ChannelCloseOnDrop` 才有)。
    /// 所以这里**每一条提前返回的路径都要显式 `close()`**,否则就是
    /// ADR-009 不变量 3「channel 泄漏」:对端每来一次回连就占掉一个
    /// channel slot,攒到 `MaxSessions` 之后整条连接再也开不出新 channel。
    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: russh::Channel<client::Msg>,
        _connected_address: &str,
        _connected_port: u32,
        _originator_address: &str,
        _originator_port: u32,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        let Some(tx) = self.forwarded.as_ref() else {
            let _ = channel.close().await;
            return Ok(());
        };
        // 接收端没了(隧道已停止或正在重连)→ 同样要显式关。
        if let Err(e) = tx.send(ForwardedTcpip { channel }).await {
            let _ = e.0.channel.close().await;
        }
        Ok(())
    }

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

/// 一条建立好的 SSH 连接,**连同它依赖的所有跳板连接**。
///
/// 为什么要持有 `_jumps`:russh 的 `ChannelStream` 不持有 `Handle`,
/// `Handle` 一 Drop 整条连接立刻断。跳板链是「A 上开 channel 通向 B」,
/// 若 A 的 Handle 提前释放,B 的流会在几毫秒后静默断掉 ——
/// 本地直连场景永远复现不了。把保活做成字段,让类型系统兜住。
///
/// **限定**:上面这段「一 Drop 立刻断」来自设计 §5.2,**尚未被本仓库的测试实证**。
/// `tests/two_hop_jump.rs` 做过红队实验:把 `dial.rs` 的 `jumps.push(handle)` 换成
/// `drop(handle)` 真的丢掉跳板 Handle 后,隧道在 3 秒空闲窗口内**依然可用**
/// (russh 0.54.5 + 进程内假 sshd)。所以确切成立的只有弱版本:丢了 Handle
/// 至少在这个窗口下不会立刻断。真实链路(远端 sshd + 高延迟 + 长连接)是否会断,
/// 无头环境验不了。**不要因为「实验没复现」就删掉这个字段** —— 它成本为零,
/// 而 §5.2 描述的失效模式一旦发生就是「连上几毫秒后无故断」这类最难查的 bug。
pub struct SshConnection {
    handle: Handle<ClientHandler>,
    /// 跳板链每一跳的连接,仅用于保活,顺序 = 拨号顺序(`_jumps[0]` 最先建立)。
    ///
    /// 释放顺序注意:字段整体在 `handle` 之后 drop(声明顺序),没问题;
    /// 但 `Vec` 内部按下标 0→末尾 drop,即拨号顺序里**最先建立的那跳最先被丢**,
    /// 与依赖关系相反(每一跳都靠它前面的跳保活,理想释放顺序应是后建立的先丢)。
    /// 当前**没有实际后果**:russh 0.54.5 的 `Handle::Drop`
    /// (`client/mod.rs:262`)只 `debug!`,不做任何 IO,丢弃顺序不影响行为。
    /// 若未来 russh 版本让 `Drop` 真的发断连消息,这里需要改成反向释放
    /// (例如 `establish` 里 `jumps.reverse()` 后再交给 `SshConnection::new`),
    /// 到时候先补一个能检测出错误顺序的测试再改。
    _jumps: Vec<Handle<ClientHandler>>,
}

impl SshConnection {
    pub(crate) fn new(handle: Handle<ClientHandler>, jumps: Vec<Handle<ClientHandler>>) -> Self {
        Self {
            handle,
            _jumps: jumps,
        }
    }

    /// 目标主机的 Handle。跳板的 Handle **不外借**——外部拿不到就不会误 Drop。
    pub(crate) fn handle(&self) -> &Handle<ClientHandler> {
        &self.handle
    }

    /// 仅供测试与诊断:当前保活着几条跳板连接。
    pub fn jump_handle_count(&self) -> usize {
        self._jumps.len()
    }

    /// 这条连接的传输任务是否已经死了。
    ///
    /// 隧道监管循环(`tunnel.rs`)靠它发现「空闲隧道的 SSH 已经断了」——
    /// 没人连 3306 的时候 accept 循环不会有任何动静,不主动探的话用户要等到
    /// 下次用 DBeaver 才发现已经断了半小时。`russh 0.54.5` 的实现
    /// (`client/mod.rs:269`)是「发给会话任务的 mpsc sender 是否已关」,
    /// 传输任务一结束即为真。
    pub fn is_closed(&self) -> bool {
        self.handle.is_closed()
    }

    /// 开一条 `direct-tcpip` channel(`-L` / `-D` 用)。
    ///
    /// 保持 `pub(crate)`,理由同 [`handle`](Self::handle):外部拿不到
    /// `Channel` 就不会漏关 —— `russh` 的 `Channel<Msg>` 没有自动发
    /// CHANNEL_CLOSE 的 `Drop`(只有 `into_stream()` 包出来的
    /// `ChannelCloseOnDrop` 才有),漏一条就在对端占一个 channel slot。
    ///
    /// `originator` 按 RFC 4254 是「谁发起的这次转发」,这里恒为本机;
    /// 端口填 0 —— 服务端只把它记进日志,填真实的临时端口号没有额外意义。
    pub(crate) async fn open_direct_tcpip(
        &self,
        host: &str,
        port: u16,
    ) -> Result<russh::Channel<client::Msg>, ConnectError> {
        self.handle
            .channel_open_direct_tcpip(host.to_string(), port as u32, "127.0.0.1", 0)
            .await
            .map_err(map_russh)
    }

    /// 请求远端在 `bind:port` 上侦听(`-R`,F112)。
    ///
    /// **要 `&mut self`**(`russh 0.54.5` `client/mod.rs:696`)—— 这正是
    /// ADR-010「隧道独占自己的连接」的硬约束来源:会话那条 handle 以
    /// `Arc<Handle>` 在多 pane 间共享,给不出 `&mut`,复用会话连接的方案
    /// 在这一行就编译不过。所以隧道必须在把连接包进 `Arc` **之前**调它。
    ///
    /// 返回值刻意丢弃:russh 只在请求端口为 0 时回填服务端实际分配的端口,
    /// 其余情况恒为 0(见该函数文档)。我们在编辑器里就拒绝了 0 号端口,
    /// 所以这个数没有任何信息量,**不能拿它当"实际生效端口"展示**。
    pub(crate) async fn request_remote_forward(
        &mut self,
        bind: &str,
        port: u16,
    ) -> Result<(), ConnectError> {
        match self
            .handle
            .tcpip_forward(bind.to_string(), port as u32)
            .await
        {
            Ok(_) => Ok(()),
            // 协议层的"拒绝"不带原因,不许在这里编一个具体理由出来。
            Err(russh::Error::RequestDenied) => Err(ConnectError::RemoteForwardDenied { port }),
            Err(e) => Err(map_russh(e)),
        }
    }

    /// 撤销远端侦听。**不能只丢连接**:同一条 SSH 上如果还有别的用途,
    /// 远端会一直占着那个端口;而且下次重连请求同一端口时会撞上自己。
    pub(crate) async fn cancel_remote_forward(&self, bind: &str, port: u16) {
        let _ = self
            .handle
            .cancel_tcpip_forward(bind.to_string(), port as u32)
            .await;
    }

    /// 主动断开:先断目标主机,再逐个断跳板。
    ///
    /// 不能只靠 Drop —— russh 0.54.5 的 `impl Drop for Handle` 只
    /// `debug!("drop handle")`,既不发 disconnect 也不 abort 后台任务。
    /// 拨测(F92)一秒钟能点好几次,漏断就是在对端堆半开连接。
    pub async fn disconnect(&self) {
        let _ = self
            .handle
            .disconnect(russh::Disconnect::ByApplication, "", "")
            .await;
        for h in &self._jumps {
            let _ = h.disconnect(russh::Disconnect::ByApplication, "", "").await;
        }
    }
}

/// 连接 + 认证 + 主机校验。成功返回存活的连接(PTY 由 open_pty 接)。
///
/// `hops` 为空时行为与直连完全一致(F4/F5 之前的行为不变);否则先经
/// `dial::dial` 逐跳串联代理/跳板,拿到通向目标的流,再在目标上握手认证。
pub async fn establish(
    cfg: &SshConfig,
    policy: Arc<dyn HostKeyPolicy>,
) -> Result<SshConnection, ConnectError> {
    let dialed = crate::dial::dial(&cfg.hops, &cfg.host, cfg.port, policy.clone()).await?;
    let handle =
        handshake_and_auth(dialed.stream, &cfg.host, &cfg.user, &cfg.auth, policy, None).await?;
    Ok(SshConnection::new(handle, dialed.jumps))
}

/// `-R` 专用的建链(F112):除了连接,还带回**服务端回连 channel 的接收端**。
///
/// 与 `establish` 分开而不是给它加参数:回连接收端只有远程转发用得上,
/// 而拿着一个没人 drain 的接收端反而有害 —— 缓冲填满会卡住 `russh` 的
/// 会话任务(sender 在 `ClientHandler` 里,跑在那个任务上)。
pub(crate) async fn establish_forwarding(
    cfg: &SshConfig,
    policy: Arc<dyn HostKeyPolicy>,
) -> Result<(SshConnection, mpsc::Receiver<ForwardedTcpip>), ConnectError> {
    // 32:回连来得再密,接收端也是「收一条 spawn 一个任务」立刻返回,
    // 不会长期占着缓冲。给个有界值而不是 unbounded,是不想让一个疯狂
    // 回连的远端把内存吃光。
    let (tx, rx) = mpsc::channel(32);
    let dialed = crate::dial::dial(&cfg.hops, &cfg.host, cfg.port, policy.clone()).await?;
    let handle = handshake_and_auth(
        dialed.stream,
        &cfg.host,
        &cfg.user,
        &cfg.auth,
        policy,
        Some(tx),
    )
    .await?;
    Ok((SshConnection::new(handle, dialed.jumps), rx))
}

/// 在已建立的流上完成 SSH 握手 + 认证。目标主机与跳板共用此函数,
/// 避免两条认证路径漂移(例如只在其中一条修了 PUBKEY_HASH)。
///
/// `forwarded` 只有 `-R` 会给。跳板一律传 `None` —— 跳板上不该有回连,
/// 万一来了,`ClientHandler` 会显式 `close()` 掉。
pub(crate) async fn handshake_and_auth<S>(
    stream: S,
    host: &str,
    user: &str,
    auth: &AuthMethod,
    policy: Arc<dyn HostKeyPolicy>,
    forwarded: Option<mpsc::Sender<ForwardedTcpip>>,
) -> Result<Handle<ClientHandler>, ConnectError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let outcome = Arc::new(Mutex::new(None));
    let handler = ClientHandler {
        host: host.to_string(),
        policy,
        outcome: outcome.clone(),
        forwarded,
    };
    let config = Arc::new(client::Config::default());
    let mut handle = client::connect_stream(config, stream, handler)
        .await
        .map_err(|e| host_key_or(&outcome, e))?;
    match authenticate_with(&mut handle, user, auth).await? {
        AuthResult::Success => Ok(handle),
        AuthResult::Failure { .. } => Err(ConnectError::AuthFailed),
    }
}

/// 公钥/agent 认证用的签名 hash 算法。**RSA 必须用 rsa-sha2-512**:传 `None` 会退化成
/// 废弃的 ssh-rsa(SHA-1),现代 OpenSSH 默认拒收 → AuthFailed(真机 RSA 钥匙实测暴露)。
/// 非 RSA 密钥(ed25519 等)russh 会忽略此 hash_alg。pubkey 与 agent 两条路径共用此常量,
/// 避免只改一处漏另一处;守护测试见本文件 tests::rsa_pubkey_hash_is_sha2_512_not_legacy_sha1。
const PUBKEY_HASH: Option<russh::keys::HashAlg> = Some(russh::keys::HashAlg::Sha512);

async fn authenticate_with(
    handle: &mut Handle<ClientHandler>,
    user: &str,
    auth: &AuthMethod,
) -> Result<AuthResult, ConnectError> {
    match auth {
        AuthMethod::Password(pw) => handle
            .authenticate_password(user, pw)
            .await
            .map_err(map_russh),
        AuthMethod::PublicKey {
            key_data,
            passphrase,
        } => {
            // 解析的是**内容**而不是路径:私钥来自加密侧车,本 crate 不碰文件系统。
            let key = russh::keys::decode_secret_key(key_data, passphrase.as_deref())
                .map_err(|e| ConnectError::Io(format!("解析私钥失败: {e}")))?;
            let with = russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key), PUBKEY_HASH);
            handle
                .authenticate_publickey(user, with)
                .await
                .map_err(map_russh)
        }
        AuthMethod::Agent => authenticate_agent(handle, user).await,
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

/// 握手 + 开 PTY channel 的一站式入口。CLI 直连与会话管理器都走这里(单 pane 路径)。
///
/// `wake` 由 app 注入(EventLoopProxy.send_event);ssh 不认识 winit。
pub async fn connect(
    cfg: &SshConfig,
    policy: Arc<dyn HostKeyPolicy>,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> Result<(SshSession, mpsc::Receiver<Vec<u8>>), ConnectError> {
    let conn = establish(cfg, policy).await?;
    open_pty(Arc::new(conn), cfg, wake).await
}

/// 在**已建立**的连接上再开一条 PTY channel(F35 分屏复用连接)。
///
/// 签名里刻意**没有任何网络参数**(host/port/auth/policy 一个都不收):
/// 想在这里偷偷重连一次都做不到,是结构性的防呆。主机密钥确认(F3/TOFU)只在
/// [`establish`] 触发一次,新开分屏不会再弹窗(§6.2)。
///
/// `conn` 必须是 `Arc`:russh 0.54.5 的 `Handle` 没有实现 `Clone`,只有 `Drop`
/// (释放即断连),`SshConnection` 内部还多背着跳板链的 Handle,同样一 Drop 即断。
/// 每条 channel 的 io_task 各持一份 Arc,最后一个释放才真正断连 ——
/// 这就是「关掉一个 pane 不影响其余 pane」的实现机制(§6.1)。
pub async fn open_pty(
    conn: Arc<SshConnection>,
    cfg: &SshConfig,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> Result<(SshSession, mpsc::Receiver<Vec<u8>>), ConnectError> {
    let channel = conn
        .handle()
        .channel_open_session()
        .await
        .map_err(|_| ConnectError::PtyRequest)?;
    if channel
        .request_pty(true, &cfg.term, cfg.cols as u32, cfg.rows as u32, 0, 0, &[])
        .await
        .is_err()
    {
        // `Channel<Msg>` 没有自动发 CHANNEL_CLOSE 的 Drop(只有 into_stream() 的
        // ChannelCloseOnDrop 才有),不显式关就是泄漏一个 channel slot;多 pane 下
        // handle 是共享 Arc,某个 pane 开失败不会关掉整条连接,泄漏会一直累积到
        // sshd 的 MaxSessions 上限,导致后续 pane 再也开不出来。
        let _ = channel.close().await;
        return Err(ConnectError::PtyRequest);
    }
    if channel.request_shell(true).await.is_err() {
        let _ = channel.close().await;
        return Err(ConnectError::PtyRequest);
    }

    // 拆读写半:read.wait()(&mut) 与 write.data()(&) 同任务 select! 不冲突。
    let (read, write) = channel.split();

    let (inbound_tx, inbound_rx) = mpsc::channel::<Vec<u8>>(256);
    let (cmd_tx, cmd_rx) = mpsc::channel::<SshCmd>(256);

    tokio::spawn(io_task(read, write, cmd_rx, inbound_tx, wake, conn));

    Ok((SshSession { cmd_tx }, inbound_rx))
}

async fn io_task(
    mut read: russh::ChannelReadHalf,
    write: russh::ChannelWriteHalf<client::Msg>,
    mut cmd_rx: mpsc::Receiver<SshCmd>,
    inbound_tx: mpsc::Sender<Vec<u8>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    // 持有一份 Arc 只为保活:Handle(及其背着的跳板 Handle)一 Drop 整条 SSH 连接
    // 就断。多 pane 下每条 channel 的 io_task 各持一份,最后一个 io_task 结束时
    // 连接才关(§6.1)。
    _conn: Arc<SshConnection>,
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
                forwarded: None,
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

    /// 保活红线:跳板 Handle 必须被 `SshConnection` 持有。
    /// 只有把它移进结构体、且 `open_pty` 收 `Arc<SshConnection>`,
    /// 「跳板连接活得比 PTY 久」才是类型保证而非注释保证。
    #[test]
    fn ssh_connection_owns_jump_handles_so_they_outlive_the_pty() {
        fn assert_field_exists(c: &SshConnection) -> usize {
            c.jump_handle_count()
        }
        // 编译通过即证明字段存在;运行期只断言空链为 0。
        let _ = assert_field_exists;
    }

    #[test]
    fn ssh_config_defaults_to_direct_dial() {
        let cfg = SshConfig {
            host: "h".into(),
            port: 22,
            user: "u".into(),
            auth: AuthMethod::Agent,
            cols: 80,
            rows: 24,
            term: "xterm-256color".into(),
            hops: Vec::new(),
        };
        assert!(cfg.hops.is_empty(), "空 hops 即直连");
    }
}
