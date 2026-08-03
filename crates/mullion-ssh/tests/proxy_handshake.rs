//! 代理握手的端到端测试:起一个进程内假代理,让 `dial` 真的连上去握手。
//!
//! 这段协议的坑不在逻辑而在**读多读少**:SOCKS5 回复的 BND 长度随 ATYP 变,
//! HTTP CONNECT 的响应头多读一个字节就吞掉了隧道里的 SSH 数据。单测覆盖不到,
//! 只有真收发才暴露。

use mullion_ssh::dial::dial;
use mullion_ssh::hop::Hop;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// `alice:secret` 的标准 base64(RFC 4648)。用 Python 标准库独立算的
/// (`base64.b64encode(b"alice:secret")`),不是从 `proxy.rs` 的
/// `base64_encode` 抄一份表达式——那样查表错位之类的 bug 会两边一起错,
/// 测试照样绿,起不到守护作用。
const ALICE_SECRET_BASIC: &str = "YWxpY2U6c2VjcmV0";

/// 起一个假的「目标服务器」,连上来就发 `banner`,然后回读一行并原样回显。
/// 返回 `JoinHandle`——调用方必须在确认隧道不再需要时 await/abort 它,
/// 否则服务器侧的 panic(真根因)会被 detach 掉,只留客户端看到的泛化症状。
async fn spawn_echo_target(banner: &'static [u8]) -> (u16, JoinHandle<()>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let handle = tokio::spawn(async move {
        let (mut s, _) = l.accept().await.unwrap();
        s.write_all(banner).await.unwrap();
        let mut buf = [0u8; 16];
        let n = s.read(&mut buf).await.unwrap();
        s.write_all(&buf[..n]).await.unwrap();
    });
    (port, handle)
}

/// 假 SOCKS5 代理。`bnd_atyp` 决定回复里用哪种地址类型——这正是最易错处。
async fn spawn_socks5(bnd_atyp: u8, target_port: u16) -> (u16, JoinHandle<()>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let handle = tokio::spawn(async move {
        let (mut c, _) = l.accept().await.unwrap();

        // 1) 方法协商:读 VER/NMETHODS/METHODS,回 05 00(免认证)。
        let mut head = [0u8; 2];
        c.read_exact(&mut head).await.unwrap();
        assert_eq!(head[0], 0x05, "客户端必须发 SOCKS5");
        let mut methods = vec![0u8; head[1] as usize];
        c.read_exact(&mut methods).await.unwrap();
        c.write_all(&[0x05, 0x00]).await.unwrap();

        // 2) CONNECT 请求:VER CMD RSV ATYP ADDR PORT。
        let mut req = [0u8; 4];
        c.read_exact(&mut req).await.unwrap();
        assert_eq!(&req[..3], &[0x05, 0x01, 0x00], "应为 CONNECT");
        match req[3] {
            0x01 => {
                let mut a = [0u8; 4];
                c.read_exact(&mut a).await.unwrap();
            }
            0x03 => {
                let mut n = [0u8; 1];
                c.read_exact(&mut n).await.unwrap();
                let mut a = vec![0u8; n[0] as usize];
                c.read_exact(&mut a).await.unwrap();
            }
            0x04 => {
                let mut a = [0u8; 16];
                c.read_exact(&mut a).await.unwrap();
            }
            other => panic!("未知 ATYP {other}"),
        }
        let mut p = [0u8; 2];
        c.read_exact(&mut p).await.unwrap();

        // 3) 回成功。BND 字段按 bnd_atyp 变长——客户端必须按 ATYP 读完。
        let mut reply = vec![0x05, 0x00, 0x00, bnd_atyp];
        match bnd_atyp {
            0x01 => reply.extend_from_slice(&[127, 0, 0, 1]),
            0x03 => {
                reply.push(9);
                reply.extend_from_slice(b"localhost");
            }
            0x04 => reply.extend_from_slice(&[0u8; 16]),
            other => panic!("未知 ATYP {other}"),
        }
        reply.extend_from_slice(&[0x00, 0x00]);
        c.write_all(&reply).await.unwrap();

        // 4) 双向转发到真目标。
        let mut up = tokio::net::TcpStream::connect(("127.0.0.1", target_port))
            .await
            .unwrap();
        tokio::io::copy_bidirectional(&mut c, &mut up).await.ok();
    });
    (port, handle)
}

/// 假 HTTP CONNECT 代理。响应头后**紧跟**隧道数据,用来抓「多读一块」的 bug。
/// `require_auth` 时真的按 `ALICE_SECRET_BASIC` 精确比对 Basic 头的取值,
/// 不是只判断 `Proxy-Authorization: Basic ` 前缀存在——前缀判断法查不出
/// base64 编码本身算错的 bug(本切片刚踩过这类假绿测试的坑)。
async fn spawn_http_connect(target_port: u16, require_auth: bool) -> (u16, JoinHandle<()>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let handle = tokio::spawn(async move {
        let (mut c, _) = l.accept().await.unwrap();
        // 逐字节读到 \r\n\r\n,不能多读——多读的就是隧道数据。
        let mut head = Vec::new();
        let mut b = [0u8; 1];
        while c.read_exact(&mut b).await.is_ok() {
            head.push(b[0]);
            if head.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let head = String::from_utf8_lossy(&head).to_string();
        assert!(head.starts_with("CONNECT "), "应为 CONNECT 请求: {head}");
        if require_auth {
            let expected = format!("Proxy-Authorization: Basic {ALICE_SECRET_BASIC}\r\n");
            if !head.contains(&expected) {
                c.write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
                    .await
                    .unwrap();
                return;
            }
        }
        c.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
            .await
            .unwrap();
        let mut up = tokio::net::TcpStream::connect(("127.0.0.1", target_port))
            .await
            .unwrap();
        tokio::io::copy_bidirectional(&mut c, &mut up).await.ok();
    });
    (port, handle)
}

/// 建隧道、验证首字节干净、验证全双工——并在结束前收尾服务器侧的两个任务,
/// 让它们的 panic(真根因)通过 `JoinError` 传播成本测试的失败,而不是被
/// detach 掉、客户端只看到 `dial()` 包出来的泛化连接错误。
///
/// `target_handle` 和 `proxy_handle` **不能同等对待**:代理握手若在转发到
/// 目标之前就失败(例如认证被拒),目标的 `l.accept()` 永远不会被连上,
/// 此时 await 它会让测试永久挂起。所以失败路径只 await `proxy_handle`
/// (它必定已经返回:要么正常吐出错误响应,要么在协议断言处 panic 后连接
/// 掉线);`target_handle` 只在确认隧道打通、已经做完 ping/pong 之后才 await。
async fn assert_tunnel_works(
    hop: Hop,
    target_port: u16,
    target_handle: JoinHandle<()>,
    proxy_handle: JoinHandle<()>,
) {
    let dialed = match dial(&[hop], "127.0.0.1", target_port, policy()).await {
        Ok(d) => d,
        Err(e) => {
            if let Err(join_err) = proxy_handle.await {
                panic!("拨号失败,且代理侧任务 panic(真根因): {join_err}");
            }
            // 代理没转发到目标,target_handle 会永远卡在 accept()——不等它,直接放弃。
            target_handle.abort();
            panic!("拨号应成功,实际失败: {e}");
        }
    };
    let mut s = dialed.stream;
    let mut banner = [0u8; 6];
    s.read_exact(&mut banner).await.expect("应读到目标 banner");
    assert_eq!(
        &banner, b"SSH-2.",
        "握手后第一个字节必须是目标的,不是代理残留"
    );
    s.write_all(b"ping").await.unwrap();
    let mut echo = [0u8; 4];
    s.read_exact(&mut echo).await.unwrap();
    assert_eq!(&echo, b"ping");
    drop(s); // 客户端这侧先关,代理的 copy_bidirectional 才会收到 EOF 收尾退出。
    target_handle
        .await
        .unwrap_or_else(|e| panic!("目标侧任务 panic: {e}"));
    proxy_handle
        .await
        .unwrap_or_else(|e| panic!("代理侧任务 panic: {e}"));
}

fn socks5(port: u16) -> Hop {
    Hop::Socks5 {
        host: "127.0.0.1".into(),
        port,
        auth: None,
    }
}

/// 本测试全是代理跳,不涉及 SSH 握手,主机密钥策略永远用不上——
/// 但 `dial` 的签名要一个,给个最简实现。
/// (`HostKeyPolicy` 是 trait,实参类型 `Arc<dyn HostKeyPolicy>`,**没有** `Default`。)
struct NoPolicy;
impl mullion_ssh::known_hosts::HostKeyPolicy for NoPolicy {
    fn decide<'a>(
        &'a self,
        _host: &'a str,
        _algo: &'a str,
        _fp: &'a mullion_ssh::known_hosts::Fingerprint,
    ) -> mullion_ssh::known_hosts::HostKeyFuture<'a> {
        Box::pin(std::future::ready(
            mullion_ssh::known_hosts::HostKeyDecision::Accept,
        ))
    }
}

fn policy() -> std::sync::Arc<dyn mullion_ssh::known_hosts::HostKeyPolicy> {
    std::sync::Arc::new(NoPolicy)
}

// 与本 crate 其余集成测试(`auth.rs`/`pty.rs`/`live.rs`/`smoke_server.rs`)
// 统一用 `flavor = "multi_thread"`——本文件把假代理/假目标 spawn 成独立任务,
// 靠 `current_thread` 单线程轮询也能工作,但和既有约定不一致就该改成一致。

/// 三种 ATYP 各跑一遍:BND 长度算错会让残留字节污染 banner,断言当场炸。
#[tokio::test(flavor = "multi_thread")]
async fn socks5_tunnel_survives_every_bnd_address_type() {
    for atyp in [0x01u8, 0x03, 0x04] {
        let (target, target_handle) = spawn_echo_target(b"SSH-2.").await;
        let (proxy, proxy_handle) = spawn_socks5(atyp, target).await;
        assert_tunnel_works(socks5(proxy), target, target_handle, proxy_handle).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn http_connect_tunnel_does_not_swallow_tunnel_bytes() {
    let (target, target_handle) = spawn_echo_target(b"SSH-2.").await;
    let (proxy, proxy_handle) = spawn_http_connect(target, false).await;
    assert_tunnel_works(
        Hop::HttpConnect {
            host: "127.0.0.1".into(),
            port: proxy,
            auth: None,
        },
        target,
        target_handle,
        proxy_handle,
    )
    .await;
}

/// 407 必须映射成 `ProxyAuthFailed`,不能混进泛化的「代理拒绝」——
/// 这两种情况用户的下一步动作完全不同(F6:每个错都要可行动)。
#[tokio::test(flavor = "multi_thread")]
async fn http_connect_407_maps_to_proxy_auth_failed() {
    let (target, target_handle) = spawn_echo_target(b"SSH-2.").await;
    let (proxy, proxy_handle) = spawn_http_connect(target, true).await;
    // `Dialed`(内含 `russh::ChannelStream`)没有 `Debug`,`expect_err` 用不了,手动 match。
    let err = match dial(
        &[Hop::HttpConnect {
            host: "127.0.0.1".into(),
            port: proxy,
            auth: None,
        }],
        "127.0.0.1",
        target,
        policy(),
    )
    .await
    {
        Ok(_) => panic!("无凭据应被 407 拒绝,实际拨号成功"),
        Err(e) => e,
    };
    assert!(
        matches!(
            err,
            mullion_ssh::error::ConnectError::ProxyAuthFailed { .. }
        ),
        "407 应映射成 ProxyAuthFailed,实际: {err:?}"
    );
    // 407 在代理侧就被拒绝,代理任务此时已正常返回,可以安全 await。
    proxy_handle
        .await
        .unwrap_or_else(|e| panic!("代理侧任务 panic: {e}"));
    // 目标从未被连接(407 在握手阶段就被拒绝,代理不会转发到目标),
    // echo target 的任务永远卡在 `accept()`——await 会挂死,直接 abort。
    target_handle.abort();
}

/// 带凭据时 407 不该发生:精确比对 `Proxy-Authorization: Basic` 的取值,
/// 手写 base64 若查表错位/padding 算错,这里会炸。
#[tokio::test(flavor = "multi_thread")]
async fn http_connect_sends_basic_credentials() {
    let (target, target_handle) = spawn_echo_target(b"SSH-2.").await;
    let (proxy, proxy_handle) = spawn_http_connect(target, true).await;
    assert_tunnel_works(
        Hop::HttpConnect {
            host: "127.0.0.1".into(),
            port: proxy,
            auth: Some(("alice".into(), "secret".into())),
        },
        target,
        target_handle,
        proxy_handle,
    )
    .await;
}

/// 代理端口没人监听 → 必须是「代理连不上」,不能报成「目标连不上」,
/// 否则用户会去查目标主机而真正的问题在代理(F6)。
#[tokio::test(flavor = "multi_thread")]
async fn unreachable_proxy_blames_the_proxy_not_the_target() {
    // 绑一个端口再立刻释放,拿到一个几乎必然无人监听的号。
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead = l.local_addr().unwrap().port();
    drop(l);
    // 同上:`Dialed` 无 `Debug`,手动 match 代替 `expect_err`。
    let err = match dial(&[socks5(dead)], "example.invalid", 22, policy()).await {
        Ok(_) => panic!("代理不可达应失败,实际拨号成功"),
        Err(e) => e,
    };
    let msg = format!("{err}");
    assert!(msg.contains("代理"), "错误消息应点名代理: {msg}");
    assert!(
        !msg.contains("sshd"),
        "不该把「检查 sshd」这种指向目标主机的引导语带出来: {msg}"
    );
}
