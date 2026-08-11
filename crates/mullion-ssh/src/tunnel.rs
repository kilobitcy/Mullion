//! 端口转发(F111~F115)。只认字节流与地址 —— 不认识「隧道是第几条」
//! 「引用了哪个会话」,那些留在 app 侧。
//!
//! 设计见 `docs/superpowers/specs/2026-08-11-tunnels-design.md`(D5/D7/D8/D10)
//! 与 `docs/adr-010-tunnels-as-first-class-objects.md`。
//!
//! 三种转发共用同一套监管循环(建链 → 服务 → 死了就退避重连),
//! 差别只在 [`Forward`] 那一层「怎么服务」。

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;

use crate::error::ConnectError;
use crate::session::SshConnection;

/// 连续失败多少次之后停下等人工。
///
/// 有上限而不是无限重试:一台已经报废/搬走的目标机,后台永远重拨既是无谓的
/// 网络噪音,也让「隧道还活着吗」这个问题永远得不到确定答案。
pub const MAX_ATTEMPTS: u32 = 8;

/// 退避上限。再长就退化成「一小时试一次」,笔记本合盖再打开要等很久才恢复。
pub const BACKOFF_CAP: Duration = Duration::from_secs(30);

/// 第 `attempt` 次重试(从 1 开始)该等多久;`None` = 别再试了。
///
/// 1s → 2s → 4s → 8s → 16s → 30s(封顶)…… 第 `MAX_ATTEMPTS` 次之后放弃。
/// **指数**让掉线瞬间恢复得快,**封顶**让长时间断网不至于退化成一小时试一次,
/// **放弃**让它不会永远在后台重拨一台已经报废的机器 —— 三者缺一都是真实故障。
pub fn backoff_delay(attempt: u32) -> Option<Duration> {
    if attempt == 0 || attempt > MAX_ATTEMPTS {
        return None;
    }
    let secs = 1u64 << (attempt - 1);
    Some(Duration::from_secs(secs).min(BACKOFF_CAP))
}

/// 这个失败**不该重试**吗?
///
/// 几类致命,理由各不相同:
/// - `HostKeyChanged`:指纹变了可能是中间人。自动重连 = 自动把凭据往可疑主机上送。
/// - `HostKeyUnknown`:需要人来确认。后台反复弹窗是骚扰,不弹又永远过不去。
/// - `AuthFailed`:凭据不对,重试 8 次只会把账号锁掉。
/// - `ListenFailed` / `RemoteForwardDenied`:端口(本机的/远端的)拿不到。
///   重试不会让端口空出来,也不会让 sshd 改配置。
///
/// 其余(网络、代理、跳板、开 channel)都是可能自愈的瞬时故障 —— 那正是
/// 退避重连存在的理由。
pub fn is_fatal(e: &ConnectError) -> bool {
    matches!(
        e,
        ConnectError::HostKeyChanged { .. }
            | ConnectError::HostKeyUnknown { .. }
            | ConnectError::AuthFailed
            | ConnectError::ListenFailed { .. }
            | ConnectError::RemoteForwardDenied { .. }
    )
}

/// `-R` 请求远端侦听哪个地址(设计 D5/D9)。
///
/// **返回值是「请求」不是「结果」**:sshd 默认 `GatewayPorts no` 会把
/// `0.0.0.0` 静默降级成回环,而 `tcpip-forward` 的协议应答只带端口号、
/// 不带实际绑定地址 —— 客户端**结构性地无法验证**这件事是否生效。
/// 所以 UI 那边必须写明「无法验证」,不许拿这个函数的返回值当既成事实。
pub fn remote_bind_address(expose: bool) -> &'static str {
    if expose {
        "0.0.0.0"
    } else {
        "127.0.0.1"
    }
}

/// 本机侦听地址。`expose` 的方向在 store 侧已经定死为「默认 false = 仅本机」
/// (缺字段时 serde 给 `false`,而 `false` 必须是安全的那一侧)。
pub fn bind_addr(expose: bool, port: u16) -> SocketAddr {
    let ip = if expose {
        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    } else {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    };
    SocketAddr::new(ip, port)
}

/// 占住本机端口。
///
/// **必须在建 SSH 连接之前调**(设计 S1):端口被占用是配置错误,`bind` 0ms
/// 就能发现,不该先烧一次完整建链(高延迟代理链路上要几秒)才告诉用户
/// 「端口已被占用」。而且这个 listener **跨重连不释放** —— 重连期间放掉端口,
/// 它会被别的进程抢走,隧道恢复时再也绑不回来。
pub async fn bind_listener(addr: SocketAddr) -> Result<TcpListener, ConnectError> {
    TcpListener::bind(addr)
        .await
        .map_err(|e| ConnectError::ListenFailed {
            port: addr.port(),
            cause: match e.kind() {
                std::io::ErrorKind::AddrInUse => "端口已被占用".to_string(),
                std::io::ErrorKind::PermissionDenied => {
                    "没有权限(1024 以下的端口通常需要管理员)".to_string()
                }
                _ => e.to_string(),
            },
        })
}

/// 双向搬字节,任一方向 EOF 都要传播到另一侧。
///
/// 签名泛型到 `AsyncRead + AsyncWrite` 而不是直接吃 `Channel<Msg>`,是为了
/// 让这块逻辑能用 `tokio::io::duplex` 造的假流纯测 —— 设计 D10 把转发放进
/// `mullion-ssh` 的第三条理由就是这个。
pub(crate) async fn pump_bidirectional<A, B>(
    mut local: A,
    mut remote: B,
) -> std::io::Result<(u64, u64)>
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    tokio::io::copy_bidirectional(&mut local, &mut remote).await
}

/// accept 循环:每个入站连接开一条 `direct-tcpip` 打到 `host:port`。
///
/// **只在连接死掉时返回**(返回值即死因),正常情况下永远跑下去。
async fn serve_local(
    listener: &TcpListener,
    conn: &Arc<SshConnection>,
    host: &str,
    port: u16,
) -> ConnectError {
    loop {
        let (sock, _peer) = match listener.accept().await {
            Ok(x) => x,
            // accept 失败几乎只有 fd 耗尽这一种可能;它不会自愈,但也不该
            // 把整条隧道判死 —— 让上层按瞬时故障重连(会重新 accept)。
            Err(e) => return ConnectError::Io(format!("accept 失败: {e}")),
        };
        let channel = match conn.open_direct_tcpip(host, port).await {
            Ok(c) => c,
            Err(e) => {
                // 分两种:整条 SSH 死了 → 交给上层重连;只是这一次开 channel
                // 失败(远端目标端口没开着、被 sshd 的 permitopen 挡了)→
                // 关掉这一条本地连接继续 accept,不拖垮整条隧道。
                if conn.is_closed() {
                    return e;
                }
                drop(sock);
                continue;
            }
        };
        // `into_stream()` 包的是 `ChannelCloseOnDrop`(russh `channels/mod.rs:589`),
        // 流一 Drop 就发 CHANNEL_CLOSE。**不要**改成手工 `channel.data()` 搬运 ——
        // 那条路要自己管 close,正是 ADR-009 不变量 3「channel 泄漏」的老坑。
        let stream = channel.into_stream();
        // 每条连接一个任务:一条慢连接不该卡住别人的 accept。
        // 持一份 `Arc<SshConnection>` 保活:Handle 一 Drop 整条连接立刻断。
        let keepalive = conn.clone();
        tokio::spawn(async move {
            let _ = pump_bidirectional(sock, stream).await;
            drop(keepalive);
        });
    }
}

/// SOCKS5 服务端(`-D`,F113):一条本地连接的全流程。
///
/// 与 `-L` 不同,这里**整条都在自己的任务里**跑:SOCKS5 要先跟客户端来回
/// 协商才知道目标,把协商放进 accept 循环等于让一个不说话的客户端卡住
/// 后面所有人。SSH 死掉由监管循环的健康探测(S6)发现,不靠这里。
async fn serve_socks5_conn(sock: tokio::net::TcpStream, conn: Arc<SshConnection>) {
    use crate::socks5_server as s5;
    let mut sock = sock;
    let req = match s5::negotiate(&mut sock).await {
        Ok(r) => r,
        Err(refusal) => {
            // 这条连接挂了,但隧道本身还好好跑着 —— 没有任何 UI 出口,
            // 不落日志的话「浏览器连不上 1080」就查无对证(ADR-008)。
            log::debug!("SOCKS5 拒绝一条本地连接:{refusal}");
            // `Reply` 是「协商成功但请求不受支持」,按 RFC 回一个码再断;
            // 其余两类连回话都不该回(见 `Socks5Refusal` 的文档)。
            if let s5::Socks5Refusal::Reply(rep) = refusal {
                let _ = s5::reply(&mut sock, rep).await;
            }
            return;
        }
    };
    match conn.open_direct_tcpip(&req.host, req.port).await {
        Ok(channel) => {
            // 先回成功再搬字节:客户端要拿到 REP 才会开始发它的应用层数据。
            if s5::reply(&mut sock, s5::REP_SUCCESS).await.is_err() {
                let _ = channel.close().await;
                return;
            }
            let stream = channel.into_stream();
            let _ = pump_bidirectional(sock, stream).await;
        }
        // 远端连不上目标 —— 这是**这一条连接**的事,不是整条隧道的事。
        // 回 `0x04` 让客户端知道是目标不可达,而不是代理坏了。
        Err(e) => {
            log::debug!(
                "SOCKS5 开 direct-tcpip 到 {}:{} 失败:{e}",
                req.host,
                req.port
            );
            let _ = s5::reply(&mut sock, s5::REP_HOST_UNREACHABLE).await;
        }
    }
    drop(conn);
}

/// `-D` 的 accept 循环。**只在 accept 出错时返回**。
async fn serve_dynamic(listener: &TcpListener, conn: &Arc<SshConnection>) -> ConnectError {
    loop {
        let (sock, _peer) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => return ConnectError::Io(format!("accept 失败: {e}")),
        };
        // 持一份 `Arc<SshConnection>` 保活,理由同 `serve_local`。
        tokio::spawn(serve_socks5_conn(sock, conn.clone()));
    }
}

/// `-R` 的入站循环(F112):远端每来一个连接,本机去连 `target` 并对搬。
///
/// 返回 = 接收端断了 = `ClientHandler` 连同 russh 的会话任务一起没了,
/// 也就是这条 SSH 死了。
async fn serve_remote(
    rx: &mut tokio::sync::mpsc::Receiver<crate::session::ForwardedTcpip>,
    conn: &Arc<SshConnection>,
    target: &(String, u16),
) -> ConnectError {
    while let Some(f) = rx.recv().await {
        // `into_stream()` 包的是 `ChannelCloseOnDrop`,流一 Drop 就发
        // CHANNEL_CLOSE —— 下面那条「连不上就 drop」的路径靠的就是它,
        // 别改成手工搬运(ADR-009 不变量 3)。
        let stream = f.channel.into_stream();
        let target = target.clone();
        let keepalive = conn.clone();
        tokio::spawn(async move {
            match tokio::net::TcpStream::connect((target.0.as_str(), target.1)).await {
                Ok(sock) => {
                    let _ = pump_bidirectional(sock, stream).await;
                }
                // 本机连不上目标(服务没起、端口写错)= **这一次回连**的事。
                // 关掉它继续等下一个,不判整条隧道死 —— 远端还在侦听,
                // 用户把本机服务起起来之后下一次回连就通了。
                Err(_) => drop(stream),
            }
            drop(keepalive);
        });
    }
    ConnectError::Io("SSH 连接已断开".to_string())
}

/// 这条隧道要「服务」什么。三种转发的差别全在这里,监管循环共用。
pub enum Forward {
    /// `-L`:本机 listener 已由调用方 bind 好(S1),每个入站连接开一条
    /// `direct-tcpip` 打到 `target`(由**远端**解析)。
    Local {
        listener: TcpListener,
        target: (String, u16),
    },
    /// `-R`:请求远端在 `bind:port` 上侦听,回连由**本机**去连 `target`。
    ///
    /// 没有本机 listener —— 这类隧道占的是**远端**的端口,所以 S1
    /// 「先占端口再建链」在这里无从谈起,端口冲突只能等远端拒绝
    /// (`RemoteForwardDenied`,是致命错误)。
    Remote {
        bind: &'static str,
        port: u16,
        target: (String, u16),
    },
    /// `-D`:本机 listener 上跑 SOCKS5 服务端,目标由每条连接自己带。
    Dynamic { listener: TcpListener },
}

/// 一条隧道当下的样子。app 侧照着它画行状态与状态栏。
///
/// `Failed` 带的是**已格式化的原因**:错误分类在这一层做完,UI 层不该
/// 再做第二次(两处分类迟早漂移,而漂移的后果是同一个故障两种说法)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelState {
    /// 正在建链(可能要走代理 + 跳板链,高延迟下这段能有好几秒)。
    Connecting,
    /// 端口已侦听、SSH 已连上,可以用了。
    Running,
    /// 掉线了,正在退避重连。
    Reconnecting { attempt: u32, retry_in: Duration },
    /// 停下了,等人工。原因已格式化。
    Failed(String),
    /// 用户主动停止。
    Stopped,
}

/// 一次失败之后干什么。把**决策**从**执行**里剥出来,是为了让重连策略
/// 能纯测 —— 监管循环本身要真网络,无头环境验不了。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Retry(Duration),
    GiveUp(String),
}

/// 第 `attempt` 次失败(从 1 开始)之后的判决。
pub fn next_step(attempt: u32, err: &ConnectError) -> Step {
    if is_fatal(err) {
        return Step::GiveUp(err.to_string());
    }
    match backoff_delay(attempt) {
        Some(d) => Step::Retry(d),
        None => Step::GiveUp(format!("连续失败 {MAX_ATTEMPTS} 次后停止重连:{err}")),
    }
}

/// 状态回传。用注入回调而不是让本 crate 认识 `TunnelId` —— 同 `session.rs`
/// 里 `wake` 的既有做法,`mullion-ssh` 只认字节流。
pub type StateSink = Arc<dyn Fn(TunnelState) + Send + Sync>;

/// 拨号材料。首连与重连用**不同的**主机密钥策略:首连时用户刚点完「启动」,
/// 人在场,可以弹 TOFU 确认;重连是后台行为,必须换成「只信已记录的主机」
/// 那一档。这一手把设计 D7 的两条硬约束变成结构性保证 ——
/// 后台重连**不可能**弹出模态框(策略里根本没有发弹窗的通路),
/// 指纹变了也**不可能**被自动接受。
pub struct TunnelDial {
    pub cfg: crate::config::SshConfig,
    pub first_policy: Arc<dyn crate::known_hosts::HostKeyPolicy>,
    pub retry_policy: Arc<dyn crate::known_hosts::HostKeyPolicy>,
}

/// 多久探一次「SSH 是不是已经死了」(S6)。
const HEALTH_TICK: Duration = Duration::from_secs(2);

/// 一条在跑的隧道。**Drop 即停止** —— 句柄没人拿着的隧道还在本机 listen
/// 一个端口,是安全问题不是资源问题。
pub struct TunnelHandle {
    stop: tokio::sync::watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl TunnelHandle {
    /// 请求停止。监管任务会先 `disconnect()` 再退出 —— `russh` 的 `Drop`
    /// 不发 disconnect(见 `SshConnection::disconnect` 的注释),漏了就在
    /// 对端堆半开连接。
    pub fn stop(&self) {
        let _ = self.stop.send(true);
    }

    /// 监管任务是否已经结束(测试与诊断用)。
    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }
}

impl Drop for TunnelHandle {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

/// 等停止信号。发送端被丢弃(= `TunnelHandle` 被 Drop)也算停止。
async fn wait_stop(rx: &mut tokio::sync::watch::Receiver<bool>) {
    loop {
        if *rx.borrow_and_update() {
            return;
        }
        if rx.changed().await.is_err() {
            return; // 句柄没了,没人再关心这条隧道
        }
    }
}

/// 一次失败之后的处置。返回 `false` = 到此为止(状态已经落定)。
async fn handle_failure(
    err: ConnectError,
    attempt: &mut u32,
    stop: &mut tokio::sync::watch::Receiver<bool>,
    on_state: &StateSink,
) -> bool {
    *attempt += 1;
    match next_step(*attempt, &err) {
        Step::GiveUp(why) => {
            on_state(TunnelState::Failed(why));
            false
        }
        Step::Retry(d) => {
            on_state(TunnelState::Reconnecting {
                attempt: *attempt,
                retry_in: d,
            });
            tokio::select! {
                _ = tokio::time::sleep(d) => true,
                _ = wait_stop(stop) => {
                    on_state(TunnelState::Stopped);
                    false
                }
            }
        }
    }
}

/// 起一条隧道并一直看着它(F111/F112/F113 + F114)。
///
/// `-L`/`-D` 的 `listener` 由调用方**先 bind 好**再交进来(S1):端口占用要在
/// 烧掉一次完整建链之前就报出来,而且这个 listener 跨重连不释放,免得端口
/// 被别的进程抢走。`-R` 没有本机 listener,那类的端口冲突只能等远端拒绝。
pub fn spawn_tunnel(forward: Forward, dial: TunnelDial, on_state: StateSink) -> TunnelHandle {
    let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(async move {
        // 只有 `-R` 需要服务端回连的接收端。`-L`/`-D` 拿着一个没人 drain 的
        // 接收端反而有害:缓冲填满会卡住 russh 的会话任务(见
        // `establish_forwarding` 的注释)。
        let want_forwarding = matches!(forward, Forward::Remote { .. });
        // 连上一次就归零:一条挂了三个月、期间断过几十次的隧道,不该因为
        // 累计次数到 8 就再也不重连。
        let mut attempt = 0u32;
        loop {
            on_state(TunnelState::Connecting);
            let policy = if attempt == 0 {
                dial.first_policy.clone()
            } else {
                dial.retry_policy.clone()
            };
            let dialed = tokio::select! {
                r = async {
                    if want_forwarding {
                        crate::session::establish_forwarding(&dial.cfg, policy)
                            .await
                            .map(|(c, rx)| (c, Some(rx)))
                    } else {
                        crate::session::establish(&dial.cfg, policy).await.map(|c| (c, None))
                    }
                } => r,
                _ = wait_stop(&mut stop_rx) => {
                    on_state(TunnelState::Stopped);
                    return;
                }
            };
            let (mut conn, mut rx) = match dialed {
                Ok(x) => x,
                Err(e) => {
                    if handle_failure(e, &mut attempt, &mut stop_rx, &on_state).await {
                        continue;
                    }
                    return;
                }
            };
            // `-R` 必须在包进 `Arc` **之前**请求转发:`tcpip_forward` 要
            // `&mut self`,`Arc` 给不出(ADR-010 的硬约束)。
            if let Forward::Remote { bind, port, .. } = &forward {
                if let Err(e) = conn.request_remote_forward(bind, *port).await {
                    conn.disconnect().await;
                    if handle_failure(e, &mut attempt, &mut stop_rx, &on_state).await {
                        continue;
                    }
                    return;
                }
            }
            let conn = Arc::new(conn);
            attempt = 0;
            on_state(TunnelState::Running);

            let death = tokio::select! {
                e = async {
                    match &forward {
                        Forward::Local { listener, target } =>
                            serve_local(listener, &conn, &target.0, target.1).await,
                        Forward::Dynamic { listener } => serve_dynamic(listener, &conn).await,
                        Forward::Remote { target, .. } => match rx.as_mut() {
                            Some(rx) => serve_remote(rx, &conn, target).await,
                            // 不可达:与 `want_forwarding` 是同一个判据。
                            None => ConnectError::Io("内部错误:远端转发没有接收端".to_string()),
                        },
                    }
                } => Some(e),
                // S6:空闲隧道断线时 accept 循环毫无动静,必须主动探。
                _ = async {
                    let mut tick = tokio::time::interval(HEALTH_TICK);
                    tick.tick().await; // interval 的第一下立即就绪,丢掉
                    loop {
                        tick.tick().await;
                        if conn.is_closed() {
                            return;
                        }
                    }
                } => Some(ConnectError::Io("SSH 连接已断开".to_string())),
                _ = wait_stop(&mut stop_rx) => None,
            };
            // 撤销远端侦听。不撤的话远端会一直占着那个端口,下次重连请求
            // 同一端口时会撞上自己上一轮留下的侦听。
            if let Forward::Remote { bind, port, .. } = &forward {
                conn.cancel_remote_forward(bind, *port).await;
            }
            conn.disconnect().await;
            let Some(e) = death else {
                on_state(TunnelState::Stopped);
                return;
            };
            if !handle_failure(e, &mut attempt, &mut stop_rx, &on_state).await {
                return;
            }
        }
    });
    TunnelHandle {
        stop: stop_tx,
        task,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::known_hosts::Fingerprint;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn backoff_is_bounded_exponential_and_gives_up() {
        let got: Vec<Option<u64>> = (1..=9)
            .map(|a| backoff_delay(a).map(|d| d.as_secs()))
            .collect();
        assert_eq!(
            got,
            vec![
                Some(1),
                Some(2),
                Some(4),
                Some(8),
                Some(16),
                Some(30), // 32 被 BACKOFF_CAP 削到 30
                Some(30),
                Some(30),
                None, // 第 9 次:放弃
            ]
        );
        assert_eq!(
            backoff_delay(0),
            None,
            "attempt 从 1 开始计,0 是调用方写错了"
        );
    }

    /// 安全断言。指纹变更意味着**可能有中间人**,自动重连等于自动把凭据
    /// 往可疑主机上送 —— 这条比「用户体验差一点」严重一个量级,所以单独一条测试。
    #[test]
    fn fingerprint_change_is_fatal_and_never_retried() {
        assert!(is_fatal(&ConnectError::HostKeyChanged {
            host: "h".into(),
            expected: Fingerprint(vec![1]),
            got: Fingerprint(vec![2]),
        }));
    }

    #[test]
    fn auth_failure_is_fatal_so_we_do_not_lock_the_account() {
        assert!(is_fatal(&ConnectError::AuthFailed));
        assert!(
            is_fatal(&ConnectError::HostKeyUnknown {
                host: "h".into(),
                got: Fingerprint(vec![3]),
            }),
            "未知指纹要人来确认,后台重试既过不去也只会反复骚扰"
        );
    }

    /// 反面必须一起测:只测致命那一半的话,把 `is_fatal` 写成恒真也是绿的
    /// ——而恒真意味着**重连功能整个不存在**,掉一次网隧道就永久停了。
    #[test]
    fn transient_network_errors_are_retryable() {
        let retryable = [
            ConnectError::ConnectionRefused("1.2.3.4:22".into()),
            ConnectError::Io("broken pipe".into()),
            ConnectError::DnsResolution("h".into()),
            ConnectError::PtyRequest,
            ConnectError::JumpFailed {
                hop: "bastion:22".into(),
                cause: "timeout".into(),
            },
            ConnectError::ProxyUnreachable {
                proxy: "127.0.0.1:7891".into(),
                cause: "refused".into(),
            },
        ];
        for e in &retryable {
            assert!(!is_fatal(e), "{e} 是瞬时故障,应当可重试");
        }
    }

    /// 两条必须成对。只测 `false` 那条的话,把函数写成恒返回 loopback 也是绿的
    /// ——而那意味着 F117 勾了「允许同网段访问」不生效,用户以为开了其实没开。
    #[test]
    fn expose_false_binds_loopback_only() {
        assert_eq!(bind_addr(false, 3306).ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(bind_addr(false, 3306).port(), 3306);
    }

    /// `-R` 的两档同理成对。只测一边的话,写成恒返回 `"0.0.0.0"` 也是绿的
    /// —— 而那意味着**每一条 `-R` 都在请求远端对全网开放**,与 F117 的
    /// 「默认仅本机」正好相反。
    #[test]
    fn remote_bind_defaults_to_loopback_and_only_opens_up_when_asked() {
        assert_eq!(remote_bind_address(false), "127.0.0.1");
        assert_eq!(remote_bind_address(true), "0.0.0.0");
    }

    /// 远端拒绝侦听与本机端口占用同类:重试不会让端口空出来,也不会让
    /// sshd 改配置。混进瞬时故障的话,用户要等 8 次退避(最长 2 分钟)
    /// 才拿到「远端不让你侦听这个端口」这个本可以立刻给出的结论。
    #[test]
    fn remote_forward_denial_is_fatal_like_a_busy_local_port() {
        let e = ConnectError::RemoteForwardDenied { port: 8080 };
        assert!(is_fatal(&e));
        match next_step(1, &e) {
            Step::GiveUp(why) => assert!(why.contains("8080"), "要点名端口,实际: {why}"),
            other => panic!("被拒绝必须第一次就停,实际: {other:?}"),
        }
    }

    #[test]
    fn expose_true_binds_all_interfaces() {
        assert_eq!(
            bind_addr(true, 3306).ip(),
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        );
    }

    /// 起一条真的 TCP 侦听,accept 到的连接与一段 `duplex` 假流对接 ——
    /// 「假流」这一头就相当于 `direct-tcpip` channel 的远端。
    ///
    /// 端口交给内核(`:0`):测试不能跟别的进程抢固定端口,那是最典型的
    /// CI 偶发红。
    async fn wire() -> (SocketAddr, tokio::io::DuplexStream) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (remote_side, test_side) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let _ = pump_bidirectional(sock, remote_side).await;
        });
        (addr, test_side)
    }

    #[tokio::test]
    async fn bytes_flow_from_local_client_to_remote_end() {
        let (addr, mut remote) = wire().await;
        let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
        client.write_all(b"SELECT 1").await.unwrap();
        let mut buf = [0u8; 8];
        remote.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"SELECT 1");
    }

    #[tokio::test]
    async fn bytes_flow_back_from_remote_end_to_local_client() {
        let (addr, mut remote) = wire().await;
        let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
        remote.write_all(b"OK").await.unwrap();
        let mut buf = [0u8; 2];
        client.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"OK");
    }

    /// 远端 EOF 必须传播回本地,否则 DBeaver 那边会挂着一个永远不返回的
    /// 死连接 —— 表现为「转圈到超时」,比干脆的 reset 难排查得多。
    #[tokio::test]
    async fn closing_the_remote_end_closes_the_local_client() {
        let (addr, remote) = wire().await;
        let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
        drop(remote);
        let mut buf = [0u8; 1];
        let n = client.read(&mut buf).await.unwrap();
        assert_eq!(n, 0, "远端断了必须传播成本地 EOF");
    }

    /// 反向同理:本地客户端关了不传播过去,远端 sshd 那侧会堆半开 channel,
    /// 累积到 `MaxSessions` 之后整条隧道再也开不出新连接。
    #[tokio::test]
    async fn closing_the_local_client_closes_the_remote_end() {
        let (addr, mut remote) = wire().await;
        let client = tokio::net::TcpStream::connect(addr).await.unwrap();
        drop(client);
        let mut buf = [0u8; 1];
        let n = remote.read(&mut buf).await.unwrap();
        assert_eq!(n, 0, "本地断了必须传播成远端 EOF");
    }

    /// `AddrInUse` 的原文是英文 os error,用户看不出该去关哪个程序、
    /// 也看不出是哪个端口。这条守 F6「不许统一 connection failed」在
    /// 侦听侧的兑现。
    #[tokio::test]
    async fn binding_an_occupied_port_names_the_port_instead_of_a_generic_io_error() {
        let squatter = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = squatter.local_addr().unwrap();
        let err = bind_listener(addr).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains(&addr.port().to_string()),
            "错误里必须点名端口号,实际: {msg}"
        );
        assert!(msg.contains("占用"), "要说清是被占用了,实际: {msg}");
        // 且它是致命的 —— 重试 8 次不会让端口变得没被占用。
        assert!(is_fatal(&err), "侦听失败不该进退避重连");
    }

    /// **自证变红的方式**:把 `next_step` 开头那个 `is_fatal` 分支删掉 ——
    /// 这条会从 `GiveUp` 变成 `Retry(1s)`,即「指纹变了还在后台每秒重连」。
    #[test]
    fn fatal_error_gives_up_on_the_first_attempt() {
        let err = ConnectError::HostKeyChanged {
            host: "db".into(),
            expected: Fingerprint(vec![1]),
            got: Fingerprint(vec![2]),
        };
        match next_step(1, &err) {
            Step::GiveUp(why) => assert!(
                why.contains("中间人"),
                "放弃的理由要保留原始可操作错误,实际: {why}"
            ),
            other => panic!("指纹变更必须第一次就停,实际: {other:?}"),
        }
    }

    #[test]
    fn transient_error_retries_with_growing_delay() {
        let err = ConnectError::Io("broken pipe".into());
        assert_eq!(next_step(1, &err), Step::Retry(Duration::from_secs(1)));
        assert_eq!(next_step(2, &err), Step::Retry(Duration::from_secs(2)));
        assert_eq!(next_step(3, &err), Step::Retry(Duration::from_secs(4)));
    }

    /// 放弃时必须说清「试了多少次」。只说「连接失败」的话,用户分不出
    /// 「刚断一下」和「已经试了 8 次、这台机器是真没了」。
    #[test]
    fn giving_up_after_the_last_attempt_says_how_many_times_it_tried() {
        let err = ConnectError::Io("timeout".into());
        match next_step(MAX_ATTEMPTS + 1, &err) {
            Step::GiveUp(why) => {
                assert!(why.contains("8 次"), "实际: {why}");
                assert!(why.contains("timeout"), "根因不能丢,实际: {why}");
            }
            other => panic!("超过上限必须放弃,实际: {other:?}"),
        }
    }

    /// 停止必须在**退避等待中**也立刻生效。
    ///
    /// 这条覆盖的是监管循环里唯一能无头验证的那条路径:拨一个必然拒绝的
    /// 地址(`127.0.0.1:1`)让它进 `Reconnecting`,然后 `stop()`。
    /// 若 `handle_failure` 里那个 `select!` 写成了裸 `sleep(d).await`,
    /// 这条会卡满超时 —— 用户点了「停止」却要等最多 30 秒才真的停,
    /// 而端口在这期间一直占着。
    #[tokio::test]
    async fn stopping_during_backoff_takes_effect_immediately() {
        use std::sync::Mutex;

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let known = Arc::new(Mutex::new(crate::known_hosts::KnownHosts::new()));
        let dial = TunnelDial {
            cfg: crate::config::SshConfig {
                host: "127.0.0.1".into(),
                port: 1, // 必然 ECONNREFUSED
                user: "u".into(),
                auth: crate::config::AuthMethod::Password("x".into()),
                cols: 80,
                rows: 24,
                term: "xterm-256color".into(),
                hops: Vec::new(),
            },
            first_policy: Arc::new(crate::known_hosts::TofuAccept::new(known.clone())),
            retry_policy: Arc::new(crate::known_hosts::TofuAccept::new(known)),
        };

        let seen: Arc<Mutex<Vec<TunnelState>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let handle = spawn_tunnel(
            Forward::Local {
                listener,
                target: ("db.internal".into(), 3306),
            },
            dial,
            Arc::new(move |s| sink.lock().unwrap().push(s)),
        );

        // 等它真的进了退避(第一次拨号必然失败)。
        for _ in 0..200 {
            if seen
                .lock()
                .unwrap()
                .iter()
                .any(|s| matches!(s, TunnelState::Reconnecting { .. }))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            seen.lock()
                .unwrap()
                .iter()
                .any(|s| matches!(s, TunnelState::Reconnecting { .. })),
            "拨 127.0.0.1:1 该失败并进重连,实际: {:?}",
            seen.lock().unwrap()
        );

        handle.stop();
        // 退避是 1s 起步;300ms 内停不下来就说明 stop 没被 select 到。
        for _ in 0..30 {
            if handle.is_finished() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            handle.is_finished(),
            "stop() 必须打断退避等待,而不是等它睡完"
        );
        assert_eq!(
            seen.lock().unwrap().last(),
            Some(&TunnelState::Stopped),
            "停止后的终态必须是 Stopped,不是 Failed"
        );
    }
}
