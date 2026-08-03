//! F5 端到端:两个进程内假 sshd 串成一条跳板链。本地直连场景永远走不到跳板
//! 代码路径,必须真起两个假 sshd 才能覆盖它。
//!
//! 本文件覆盖三个**互相独立**的属性,别把它们混成一句「验证了跳板」:
//!   1. 跳板 Handle 被 `SshConnection` 持有(结构断言,见断言 1);
//!   2. 隧道确实通到了目标而不是停在跳板上(见断言 2);
//!   3. 连接空闲一段时间后仍可复用(见断言 3)。
//!
//! 设计 §5.2 说「`Handle` 一 Drop 整条连接立刻断」——**本文件没能实证这一点**:
//! 红队实验里真丢掉跳板 Handle 后,断言 2/3 依然全绿(详见断言 1 的注释)。
//! 因此对「忘记保活」这个 bug,唯一有检测力的是断言 1。

mod common;

use std::sync::Arc;
use std::time::Duration;

use mullion_ssh::config::{AuthMethod, SshConfig};
use mullion_ssh::hop::Hop;
use mullion_ssh::known_hosts::{Fingerprint, HostKeyDecision, HostKeyFuture, HostKeyPolicy};
use mullion_ssh::session::{establish, open_pty};
use russh::server::{Auth, Handler, Msg, Session};
use russh::Channel;
use tokio::net::TcpStream;

/// 跳板专用凭据,**故意**与目标 sshd 的 `common::TEST_USER`/`TEST_PASSWORD` 不同。
/// 见 `JumpHandler` 上的注释:这是断言 2 能证明「到了目标而不是停在跳板」的关键。
const JUMP_USER: &str = "jumpuser";
const JUMP_PASSWORD: &str = "jump-password";

/// 假跳板 sshd:只接受 `JUMP_USER`/`JUMP_PASSWORD`,实现 `channel_open_direct_tcpip`
/// 把 channel 转成流后原样双向转发到请求里指定的下一跳。
///
/// 不认识、也不解析隧道里跑的字节——纯管道,这正是 SSH 跳板的定义。
struct JumpHandler;

impl Handler for JumpHandler {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        if user == JUMP_USER && password == JUMP_PASSWORD {
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

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
}

/// 在 127.0.0.1:0 起假跳板 sshd,返回实际监听地址。结构与
/// `common::spawn_echo_server` 一致,只是换了 Handler(跳板不认识 pty/shell,
/// 只认 direct-tcpip 转发)。
///
/// 这里的 `tokio::spawn` 句柄被丢掉(detach):`JumpHandler` 内部不做任何断言,
/// 它 panic 与否不影响结论,测试靠客户端侧的超时兜底。**若将来往 `JumpHandler`
/// 里加断言**(例如「必须收到指向目标端口的 direct-tcpip 请求」),必须改成收集
/// `JoinHandle` 并在测试末尾 `await`——否则断言失败会被吞掉,测试假绿。
/// (`proxy_handshake.rs` 就是因为加了断言才返回 `JoinHandle` 的。)
async fn spawn_jump_server() -> std::net::SocketAddr {
    let host_key =
        russh::keys::load_secret_key("tests/fixtures/server_hostkey", None).expect("load hostkey");
    let mut config = russh::server::Config::default();
    config.keys.push(host_key);
    let config = Arc::new(config);

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind jump server");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => break,
            };
            let config = config.clone();
            tokio::spawn(async move {
                let _ = russh::server::run_stream(config, stream, JumpHandler).await;
            });
        }
    });
    addr
}

/// 一律放行的主机密钥策略。跳板与目标各自触发一次 `check_server_key`——两个
/// 独立的假 sshd,用哪把测试 hostkey fixture无关紧要,不需要真模拟 TOFU 存档,
/// 那是 `known_hosts.rs` 自己的覆盖范围,不是这个测试的目的。
struct AcceptAll;
impl HostKeyPolicy for AcceptAll {
    fn decide<'a>(
        &'a self,
        _host: &'a str,
        _algo: &'a str,
        _fp: &'a Fingerprint,
    ) -> HostKeyFuture<'a> {
        Box::pin(std::future::ready(HostKeyDecision::Accept))
    }
}

fn cfg(jump: std::net::SocketAddr, target: std::net::SocketAddr) -> SshConfig {
    SshConfig {
        host: target.ip().to_string(),
        port: target.port(),
        user: common::TEST_USER.to_string(),
        auth: AuthMethod::Password(common::TEST_PASSWORD.into()),
        cols: 80,
        rows: 24,
        term: "xterm-256color".to_string(),
        hops: vec![Hop::SshJump {
            host: jump.ip().to_string(),
            port: jump.port(),
            user: JUMP_USER.to_string(),
            auth: AuthMethod::Password(JUMP_PASSWORD.to_string()),
        }],
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn two_hop_jump_holds_handle_reaches_target_and_survives_idle_wait() {
    let jump_addr = spawn_jump_server().await;
    let target_addr = common::spawn_echo_server().await;
    let policy: Arc<dyn HostKeyPolicy> = Arc::new(AcceptAll);
    let cfg = cfg(jump_addr, target_addr);

    let conn = tokio::time::timeout(Duration::from_secs(10), establish(&cfg, policy))
        .await
        .expect("establish 不应挂起(未超时)")
        .expect("两跳(SshJump -> 目标)应该建立成功");

    // 断言 1(§5.2 红线,本文件里**唯一**真正捕获「跳板 Handle 未保活」的断言):
    // 跳板 Handle 必须被 SshConnection 持有。若 dial() 把它丢在栈上,这里读到的
    // 是 0。已用两种方式验证过这条断言确实会变红:
    //   a) 临时改 establish() 让 `SshConnection::new(handle, Vec::new())`;
    //   b) 更贴近真实疏漏发生位置的红队实验——在 `dial.rs` 的 `advance()` 里把
    //      `jumps.push(handle)` 换成 `drop(handle)`(真实的「忘记保活」注入点)。
    // 用 (b) 时,若同时把这条断言注掉,下面的断言 2/3 **依然全绿**——它们对
    // 「Handle 未保活」这个 bug 零检测力(原因见各自注释)。所以这条断言不是
    // 三条里的一条,而是本测试对这个 bug 的唯一防线。
    assert_eq!(conn.jump_handle_count(), 1, "跳板 Handle 必须被连接持有");

    let conn = Arc::new(conn);
    let (session, mut rx) = tokio::time::timeout(
        Duration::from_secs(10),
        open_pty(conn.clone(), &cfg, Arc::new(|| {})),
    )
    .await
    .expect("open_pty 不应挂起(未超时)")
    .expect("open_pty 应该在跳板之外的目标上成功");

    // 断言 2:隧道真的通到了目标,而不是停在跳板上。
    //
    // 跳板(JumpHandler)只接受 JUMP_USER/JUMP_PASSWORD,而这里 open_pty 走的是
    // establish() 里已经用**目标凭据**(common::TEST_USER/TEST_PASSWORD)在隧道
    // 另一端完成的握手+认证。如果 dial() 算错了 next_stop、把 direct-tcpip 的转发
    // 目标误接回跳板自己(自环 bug),跳板的 auth_password 会直接拒绝目标凭据,
    // establish() 在上面那行就已经报错退出,根本走不到这里。所以「能走到这里 +
    // 读到回显」这件事本身,加上两跳凭据互不相通这个结构性约束,合起来证明字节
    // 确实走完了「客户端 -> 跳板(纯转发,不解释协议)-> 目标(真正握手/回应)」全程,
    // 而不是两个 echo server 互相冒充。
    // 注意:这条断言只守护「到没到目标」,**不**守护「跳板 Handle 有没有被保活」
    // ——跳板 Handle 只在建隧道那一刻用一次,之后隧道数据走的是已经转发好的
    // TCP 连接,和 Handle 是否还活着无关(见断言 1 注释里的红队实验)。
    session.write(b"hello".to_vec()).expect("write");
    let got = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("5 秒内应读到目标的回显(未超时)")
        .expect("目标 channel 不应提前关闭");
    assert_eq!(&got, b"hello", "应读到目标 sshd 的回显,证明隧道通到了目标");

    // 断言 3:连接空闲一段时间后仍可复用——能在同一条连接上再开一条 PTY
    // channel 并正常往返。这段等待是**人为选定的冗余值**,不对应任何真实周期:
    // 本项目目前没有任何 keepalive 机制(`russh::client::Config::default()` 的
    // `keepalive_interval`/`inactivity_timeout` 均为 None;keepalive 相关需求
    // F7-F9 排在 P3-a,还没做),`is_alive()` 也不存在。
    //
    // 红队实验验证过:这条断言**不能**独立证明「跳板 Handle 被保活」——用
    // 断言 1 注释里 (b) 的注入点(`dial.rs` 的 `jumps.push(handle)` 换成
    // `drop(handle)`)真实丢掉跳板 Handle 后,这条断言依然通过(隧道底层是
    // 已经建立好的 TCP 转发,不经过跳板 Handle 就能继续收发)。它守护的是一个
    // 独立、仍有价值的属性——「连接不会因为空闲就变得不可用」——但检测「忘记
    // 保活」这件事,唯一防线是断言 1。
    tokio::time::sleep(Duration::from_secs(3)).await;
    let (session2, mut rx2) = tokio::time::timeout(
        Duration::from_secs(5),
        open_pty(conn.clone(), &cfg, Arc::new(|| {})),
    )
    .await
    .expect("空闲等待后仍应能在同一条连接上开新 channel(未超时)")
    .expect("空闲等待后仍应能在同一条连接上开新 channel(open_pty 成功)");

    session2.write(b"still-alive".to_vec()).expect("write");
    let got2 = tokio::time::timeout(Duration::from_secs(5), rx2.recv())
        .await
        .expect("空闲等待后仍应能正常往返(未超时)")
        .expect("空闲等待后仍应能正常往返(收到回显)");
    assert_eq!(&got2, b"still-alive", "空闲等待后连接仍应可用并能正常往返");
}
