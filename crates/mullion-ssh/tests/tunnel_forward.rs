//! F112/F113 端到端:`-R` 远程转发与 `-D` 动态转发,对着进程内假 sshd 跑。
//!
//! 这两类转发**没有一行能靠纯单测覆盖**:
//! - `-R` 的入站方向是「服务端主动开 channel 过来」,构造不出假的
//!   `russh::Channel`,只能真起一个会回连的 sshd;
//! - `-D` 的正确性判据是「一个真的 SOCKS5 客户端能不能走通」,而不是
//!   「我写出去的字节符合我自己的理解」。
//!
//! `-D` 那条测试刻意用 `mullion_ssh::proxy::socks5_handshake`(F4 的 SOCKS5
//! **客户端**)去打 F113 的 SOCKS5 **服务端**:两边是分别照 RFC 1928 写的,
//! 谁把字节序理解反了都会当场红。自己写一个「符合我实现」的假客户端就没有
//! 这个检测力。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use mullion_ssh::config::{AuthMethod, SshConfig};
use mullion_ssh::known_hosts::{Fingerprint, HostKeyDecision, HostKeyFuture, HostKeyPolicy};
use mullion_ssh::tunnel::{spawn_tunnel, Forward, TunnelDial, TunnelState};
use russh::server::{Auth, Handler, Msg, Session};
use russh::Channel;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const USER: &str = "fwduser";
const PASSWORD: &str = "fwd-password";

/// 服务端回连时拿到的回显字节。`-R` 那条测试的最终判据。
type Echoed = Arc<Mutex<Vec<u8>>>;

/// 假 sshd:同时支持 `-D` 需要的 `direct-tcpip` 和 `-R` 需要的 `tcpip-forward`。
struct FwdHandler {
    /// `false` = 模拟 sshd 拒绝远端侦听(端口被占 / `AllowTcpForwarding no`)。
    allow_forward: bool,
    echoed: Echoed,
}

impl Handler for FwdHandler {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        if user == USER && password == PASSWORD {
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

    /// `-D` 的远端侧:真去连客户端点名的地址并对搬。SOCKS5 请求里的目标
    /// 就是从这里出去的 —— 所以这条路径能证明「域名/地址原样送到了远端」。
    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host_to_connect: &str,
        port_to_connect: u32,
        _originator_address: &str,
        _originator_port: u32,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        let upstream = match TcpStream::connect((host_to_connect, port_to_connect as u16)).await {
            Ok(s) => s,
            Err(_) => return Ok(false),
        };
        let mut tunnel = channel.into_stream();
        tokio::spawn(async move {
            let mut upstream = upstream;
            let _ = tokio::io::copy_bidirectional(&mut tunnel, &mut upstream).await;
        });
        Ok(true)
    }

    /// `-R`:接受侦听请求后**立刻**开一条 `forwarded-tcpip` 回连,
    /// 模拟「远端那个端口上来了一个连接」。
    async fn tcpip_forward(
        &mut self,
        address: &str,
        port: &mut u32,
        session: &mut Session,
    ) -> Result<bool, Self::Error> {
        if !self.allow_forward {
            return Ok(false);
        }
        let handle = session.handle();
        let (addr, p) = (address.to_string(), *port);
        let echoed = self.echoed.clone();
        // 不等不睡:客户端那侧的接收端在握手之前就装好了(见
        // `establish_forwarding`),回连来早了会进缓冲,不会丢。
        tokio::spawn(async move {
            let Ok(ch) = handle
                .channel_open_forwarded_tcpip(addr, p, "10.0.0.1", 5555)
                .await
            else {
                return;
            };
            let mut s = ch.into_stream();
            if s.write_all(b"PING").await.is_err() {
                return;
            }
            let mut buf = [0u8; 4];
            if s.read_exact(&mut buf).await.is_ok() {
                *echoed.lock().unwrap() = buf.to_vec();
            }
        });
        Ok(true)
    }
}

async fn spawn_fwd_server(allow_forward: bool, echoed: Echoed) -> std::net::SocketAddr {
    let host_key =
        russh::keys::load_secret_key("tests/fixtures/server_hostkey", None).expect("load hostkey");
    let mut config = russh::server::Config::default();
    config.keys.push(host_key);
    let config = Arc::new(config);

    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind sshd");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let config = config.clone();
            let echoed = echoed.clone();
            tokio::spawn(async move {
                let _ = russh::server::run_stream(
                    config,
                    stream,
                    FwdHandler {
                        allow_forward,
                        echoed,
                    },
                )
                .await;
            });
        }
    });
    addr
}

/// 一个普通 TCP 回显服务。`-R` 里它是**本机**的目标,`-D` 里它是**远端**的目标
/// —— 同一个进程内两种角色,因为方向是由转发类型决定的,不是由它自己决定的。
async fn spawn_tcp_echo() -> std::net::SocketAddr {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                loop {
                    match sock.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => {
                            if sock.write_all(&buf[..n]).await.is_err() {
                                return;
                            }
                        }
                    }
                }
            });
        }
    });
    addr
}

struct AcceptAll;
impl HostKeyPolicy for AcceptAll {
    fn decide<'a>(&'a self, _h: &'a str, _a: &'a str, _f: &'a Fingerprint) -> HostKeyFuture<'a> {
        Box::pin(async { HostKeyDecision::Accept })
    }
}

fn dial_to(sshd: std::net::SocketAddr) -> TunnelDial {
    TunnelDial {
        cfg: SshConfig {
            host: sshd.ip().to_string(),
            port: sshd.port(),
            user: USER.into(),
            auth: AuthMethod::Password(PASSWORD.into()),
            cols: 80,
            rows: 24,
            term: "xterm-256color".into(),
            hops: Vec::new(),
        },
        first_policy: Arc::new(AcceptAll),
        retry_policy: Arc::new(AcceptAll),
    }
}

/// 轮询到条件成立,超时就返回 false。不用固定 sleep:建链在 CI 上耗时不定,
/// 睡死数会变成偶发红(而偶发红最后总是被人加个 `#[ignore]` 干掉)。
async fn wait_for(mut cond: impl FnMut() -> bool) -> bool {
    for _ in 0..300 {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    cond()
}

fn sink() -> (Arc<Mutex<Vec<TunnelState>>>, mullion_ssh::tunnel::StateSink) {
    let seen: Arc<Mutex<Vec<TunnelState>>> = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    (seen, Arc::new(move |st| s.lock().unwrap().push(st)))
}

/// F112 主路径:服务端在远端侦听口上收到连接 → 开 `forwarded-tcpip` 回来 →
/// **本机**去连目标 → 字节双向通。
///
/// 断言落在服务端拿到的回显上,而不是「状态变成了 Running」—— 后者只证明
/// `tcpip-forward` 请求被接受了,证明不了回连真的接上了本机目标。
#[tokio::test]
async fn remote_forward_pipes_a_server_initiated_connection_to_the_local_target() {
    let echo = spawn_tcp_echo().await;
    let echoed: Echoed = Arc::new(Mutex::new(Vec::new()));
    let sshd = spawn_fwd_server(true, echoed.clone()).await;
    let (seen, on_state) = sink();

    let _handle = spawn_tunnel(
        Forward::Remote {
            bind: "127.0.0.1",
            port: 18080,
            target: ("127.0.0.1".into(), echo.port()),
        },
        dial_to(sshd),
        on_state,
    );

    let got = { || !echoed.lock().unwrap().is_empty() };
    assert!(
        wait_for(got).await,
        "服务端的回连没能通到本机目标,状态轨迹: {:?}",
        seen.lock().unwrap()
    );
    assert_eq!(
        &*echoed.lock().unwrap(),
        b"PING",
        "回连里的字节必须原样往返 —— 只连上不搬字节等于没转发"
    );
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .any(|s| matches!(s, TunnelState::Running)),
        "转发通了却没上报 Running,状态栏会一直显示建链中"
    );
}

/// 服务端拒绝 `tcpip-forward`(远端端口被占 / `AllowTcpForwarding no`)时,
/// **第一次就停**,不进退避。
///
/// 自证会变红:把 `RemoteForwardDenied` 从 `tunnel::is_fatal` 里去掉 ——
/// 轨迹里会出现 `Reconnecting`,下面那条断言当场红。而它的实际后果是
/// 客户端对着一台永远会拒绝的 sshd 重拨 8 次,最长拖 2 分钟才给出结论。
#[tokio::test]
async fn remote_forward_denied_by_the_server_gives_up_without_retrying() {
    let echoed: Echoed = Arc::new(Mutex::new(Vec::new()));
    let sshd = spawn_fwd_server(false, echoed).await;
    let (seen, on_state) = sink();

    let _handle = spawn_tunnel(
        Forward::Remote {
            bind: "127.0.0.1",
            port: 18081,
            target: ("127.0.0.1".into(), 1),
        },
        dial_to(sshd),
        on_state,
    );

    let failed = {
        || {
            seen.lock()
                .unwrap()
                .iter()
                .any(|s| matches!(s, TunnelState::Failed(_)))
        }
    };
    assert!(
        wait_for(failed).await,
        "被拒绝必须落到 Failed,实际轨迹: {:?}",
        seen.lock().unwrap()
    );
    let states = seen.lock().unwrap().clone();
    assert!(
        !states
            .iter()
            .any(|s| matches!(s, TunnelState::Reconnecting { .. })),
        "远端拒绝侦听不会因为重试而改变,不该进退避。实际轨迹: {states:?}"
    );
    let Some(TunnelState::Failed(why)) =
        states.iter().find(|s| matches!(s, TunnelState::Failed(_)))
    else {
        unreachable!()
    };
    assert!(
        why.contains("18081"),
        "失败原因要点名是哪个远端端口,实际: {why}"
    );
}

/// F113 主路径:本机 SOCKS5 端口 → SSH → 远端目标。
///
/// 客户端用的是 F4 那份**独立写成**的 SOCKS5 客户端(`proxy::socks5_handshake`),
/// 不是照着服务端实现反写的假客户端 —— 两边只要有一侧把 RFC 1928 理解错,
/// 这条就红。
#[tokio::test]
async fn dynamic_forward_lets_a_real_socks5_client_reach_the_remote_target() {
    let echo = spawn_tcp_echo().await;
    let echoed: Echoed = Arc::new(Mutex::new(Vec::new()));
    let sshd = spawn_fwd_server(true, echoed).await;
    let (seen, on_state) = sink();

    // 端口交给内核:测试不许跟别的进程抢固定端口。
    let listener = mullion_ssh::tunnel::bind_listener(mullion_ssh::tunnel::bind_addr(false, 0))
        .await
        .unwrap();
    let socks = listener.local_addr().unwrap();
    let _handle = spawn_tunnel(Forward::Dynamic { listener }, dial_to(sshd), on_state);

    let running = {
        || {
            seen.lock()
                .unwrap()
                .iter()
                .any(|s| matches!(s, TunnelState::Running))
        }
    };
    assert!(
        wait_for(running).await,
        "SOCKS5 隧道没起来,轨迹: {:?}",
        seen.lock().unwrap()
    );

    let mut client = TcpStream::connect(socks).await.unwrap();
    mullion_ssh::proxy::socks5_handshake(
        &mut client,
        "本机 -D",
        None,
        &echo.ip().to_string(),
        echo.port(),
    )
    .await
    .expect("SOCKS5 协商应当成功");

    client.write_all(b"HELLO").await.unwrap();
    let mut buf = [0u8; 5];
    client.read_exact(&mut buf).await.unwrap();
    assert_eq!(
        &buf, b"HELLO",
        "字节要经 SSH 打到远端目标再原样回来 —— 只协商成功不搬字节等于没转发"
    );
}

/// SOCKS5 里不受支持的命令必须回一个 REP 码再断,而不是静默断开。
///
/// 静默断开会让客户端以为是网络问题,于是去查防火墙 —— 而它真正需要做的是
/// 换一种代理(UDP 在 SSH 的 `direct-tcpip` 上没有承载,见设计 D8)。
#[tokio::test]
async fn dynamic_forward_answers_udp_associate_with_a_refusal_not_silence() {
    let echoed: Echoed = Arc::new(Mutex::new(Vec::new()));
    let sshd = spawn_fwd_server(true, echoed).await;
    let (seen, on_state) = sink();
    let listener = mullion_ssh::tunnel::bind_listener(mullion_ssh::tunnel::bind_addr(false, 0))
        .await
        .unwrap();
    let socks = listener.local_addr().unwrap();
    let _handle = spawn_tunnel(Forward::Dynamic { listener }, dial_to(sshd), on_state);

    let running = {
        || {
            seen.lock()
                .unwrap()
                .iter()
                .any(|s| matches!(s, TunnelState::Running))
        }
    };
    assert!(wait_for(running).await);

    let mut client = TcpStream::connect(socks).await.unwrap();
    client.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
    let mut sel = [0u8; 2];
    client.read_exact(&mut sel).await.unwrap();
    assert_eq!(sel, [0x05, 0x00]);
    // UDP ASSOCIATE,ATYP=IPv4。
    client
        .write_all(&[0x05, 0x03, 0x00, 0x01, 1, 2, 3, 4, 0, 53])
        .await
        .unwrap();
    let mut rep = [0u8; 10];
    client.read_exact(&mut rep).await.unwrap();
    assert_eq!(
        rep[1], 0x07,
        "必须回「命令不支持」,实际 REP={:#04x}",
        rep[1]
    );
}
