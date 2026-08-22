//! F4/F5:逐跳拨号(设计 §5.1)。
//!
//! 产出一个「已经通向目标 host:port」的双向流,以及沿途 SSH 跳板的 Handle
//! (交给 `SshConnection` 保活)。目标自身的 SSH 握手不在这里做。

use std::sync::Arc;

use russh::client::{self, Handle};
use tokio::net::TcpStream;

use crate::error::{classify_tcp, ConnectError};
use crate::hop::Hop;
use crate::known_hosts::HostKeyPolicy;
use crate::session::ClientHandler;

/// 本机第一条 TCP 该连哪：有跳则连第一跳，没跳则直连目标。
pub(crate) fn first_tcp_target(hops: &[Hop], host: &str, port: u16) -> (String, u16) {
    match hops.first() {
        Some(Hop::Socks5 { host, port, .. })
        | Some(Hop::HttpConnect { host, port, .. })
        | Some(Hop::SshJump { host, port, .. }) => (host.clone(), *port),
        None => (host.to_string(), port),
    }
}

/// 第 `idx` 跳完成后，这条流要通向哪：下一跳的地址，或（已是最后一跳时）目标。
pub(crate) fn next_stop(hops: &[Hop], idx: usize, host: &str, port: u16) -> (String, u16) {
    match hops.get(idx + 1) {
        Some(Hop::Socks5 { host, port, .. })
        | Some(Hop::HttpConnect { host, port, .. })
        | Some(Hop::SshJump { host, port, .. }) => (host.clone(), *port),
        None => (host.to_string(), port),
    }
}

/// 拨号器产出:通向目标的流 + 沿途 SSH 跳板的 Handle(必须由调用方保活)。
pub struct Dialed {
    pub stream: DialStream,
    pub jumps: Vec<Handle<ClientHandler>>,
}

/// 拨号链的产物流。直连/代理链末端是裸 TCP;经 SSH 跳板则是 channel 流。
pub enum DialStream {
    Tcp(TcpStream),
    Channel(russh::ChannelStream<client::Msg>),
}

/// 按 `hops` 逐跳建立,直到流通向 `host:port`。
///
/// `policy` 用于**跳板自身**的主机密钥校验(F3 对跳板同样生效,设计 §5.3):
/// 跳板被换掉照样是中间人。
pub async fn dial(
    hops: &[Hop],
    host: &str,
    port: u16,
    policy: Arc<dyn HostKeyPolicy>,
) -> Result<Dialed, ConnectError> {
    let (first_host, first_port) = first_tcp_target(hops, host, port);
    let addr = resolve_one(&first_host, first_port)
        .await
        .map_err(|e| blame_first_hop(hops, e))?;
    let tcp = TcpStream::connect(addr)
        .await
        .map_err(classify_tcp)
        .map_err(|e| blame_first_hop(hops, e))?;
    // 手搓 connect_stream 绕过了 client::connect 对 Config.nodelay 的应用,
    // 须补上,否则 Nagle 算法拖慢每次小写入 —— 与高延迟链路「跟手」的目标冲突。
    tcp.set_nodelay(true)
        .map_err(|e| ConnectError::Io(format!("set_nodelay 失败: {e}")))?;

    let mut stream = DialStream::Tcp(tcp);
    let mut jumps = Vec::new();

    for (idx, hop) in hops.iter().enumerate() {
        let (nh, np) = next_stop(hops, idx, host, port);
        stream = advance(stream, hop, &nh, np, &policy, &mut jumps).await?;
    }

    Ok(Dialed { stream, jumps })
}

/// 本机到第一跳的 DNS/TCP 失败,要点名是「代理」还是「跳板」连不上,不能落回
/// 泛化的 `ConnectionRefused`/`DnsResolution`——那类消息暗示「去查目标 sshd」,
/// 而根因其实在代理/跳板本身(F6 的核心场景:错误消息不能把用户指向错误的主机)。
/// 没有跳(直连目标)时原样透传,不改变既有的直连错误分类。
fn blame_first_hop(hops: &[Hop], e: ConnectError) -> ConnectError {
    match hops.first() {
        Some(hop @ (Hop::Socks5 { .. } | Hop::HttpConnect { .. })) => {
            ConnectError::ProxyUnreachable {
                proxy: hop.endpoint(),
                cause: raw_cause(e),
            }
        }
        Some(hop @ Hop::SshJump { .. }) => ConnectError::JumpFailed {
            hop: hop.endpoint(),
            cause: raw_cause(e),
        },
        None => e,
    }
}

/// 取 `e` 的底层技术原因,**不带它自己 `Display` 里那句引导语**。
///
/// `resolve_one`/`classify_tcp` 只会产出 `DnsResolution`/`ConnectionRefused`/`Io`
/// 三种变体,三者内部都只存了原始 `io::Error`/解析失败的字符串,没有引导语——
/// 引导语是 `ConnectError::fmt` 现算的。若直接对 `e` 调用 `.to_string()` 再塞进
/// `ProxyUnreachable{cause}`,会把内层那句「检查端口/sshd 是否在跑」原样嵌进外层
/// 消息,和外层自己的引导语打架,一条消息给出两句互相矛盾的排查指引。
/// 未预期的变体(理论上不会发生)才退化到完整 `Display`,不悄悄丢失信息。
fn raw_cause(e: ConnectError) -> String {
    match e {
        ConnectError::DnsResolution(s) => s,
        ConnectError::ConnectionRefused(s) => s,
        ConnectError::Io(s) => s,
        other => other.to_string(),
    }
}

async fn resolve_one(host: &str, port: u16) -> Result<std::net::SocketAddr, ConnectError> {
    let mut addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| ConnectError::DnsResolution(e.to_string()))?;
    addrs
        .next()
        .ok_or_else(|| ConnectError::DnsResolution(format!("{host} 无解析结果")))
}

/// 在当前流上跨过一跳,返回通向 `next_host:next_port` 的新流。
async fn advance(
    stream: DialStream,
    hop: &Hop,
    next_host: &str,
    next_port: u16,
    policy: &Arc<dyn HostKeyPolicy>,
    jumps: &mut Vec<Handle<ClientHandler>>,
) -> Result<DialStream, ConnectError> {
    match hop {
        Hop::Socks5 { auth, .. } => {
            let label = hop.endpoint();
            let pair = auth.as_ref().map(|(u, p)| (u.as_str(), p.as_str()));
            match stream {
                DialStream::Tcp(mut s) => {
                    crate::proxy::socks5_handshake(&mut s, &label, pair, next_host, next_port)
                        .await?;
                    Ok(DialStream::Tcp(s))
                }
                DialStream::Channel(mut s) => {
                    crate::proxy::socks5_handshake(&mut s, &label, pair, next_host, next_port)
                        .await?;
                    Ok(DialStream::Channel(s))
                }
            }
        }
        Hop::HttpConnect { auth, .. } => {
            let label = hop.endpoint();
            let pair = auth.as_ref().map(|(u, p)| (u.as_str(), p.as_str()));
            match stream {
                DialStream::Tcp(mut s) => {
                    crate::proxy::http_connect_handshake(
                        &mut s, &label, pair, next_host, next_port,
                    )
                    .await?;
                    Ok(DialStream::Tcp(s))
                }
                DialStream::Channel(mut s) => {
                    crate::proxy::http_connect_handshake(
                        &mut s, &label, pair, next_host, next_port,
                    )
                    .await?;
                    Ok(DialStream::Channel(s))
                }
            }
        }
        Hop::SshJump {
            host,
            port,
            user,
            auth,
        } => {
            let label = hop.endpoint();
            let fail = |cause: String| ConnectError::JumpFailed {
                hop: label.clone(),
                cause,
            };
            // 跳板自己的 SSH 握手 + 认证。主机密钥同样过 policy(F3)。
            let handle = match stream {
                DialStream::Tcp(s) => crate::session::handshake_and_auth(
                    s,
                    host,
                    *port,
                    user,
                    auth,
                    policy.clone(),
                    None,
                )
                .await
                .map_err(|e| fail(e.to_string()))?,
                DialStream::Channel(s) => crate::session::handshake_and_auth(
                    s,
                    host,
                    *port,
                    user,
                    auth,
                    policy.clone(),
                    None,
                )
                .await
                .map_err(|e| fail(e.to_string()))?,
            };
            // 在跳板上开一条通向下一站的转发通道。
            // originator 字段填本地占位:sshd 只记日志,不参与路由。
            let channel = handle
                .channel_open_direct_tcpip(next_host, next_port as u32, "127.0.0.1", 0)
                .await
                .map_err(|e| fail(e.to_string()))?;
            jumps.push(handle);
            Ok(DialStream::Channel(channel.into_stream()))
        }
    }
}

impl tokio::io::AsyncRead for DialStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            DialStream::Tcp(s) => std::pin::Pin::new(s).poll_read(cx, buf),
            DialStream::Channel(s) => std::pin::Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl tokio::io::AsyncWrite for DialStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        match self.get_mut() {
            DialStream::Tcp(s) => std::pin::Pin::new(s).poll_write(cx, buf),
            DialStream::Channel(s) => std::pin::Pin::new(s).poll_write(cx, buf),
        }
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            DialStream::Tcp(s) => std::pin::Pin::new(s).poll_flush(cx),
            DialStream::Channel(s) => std::pin::Pin::new(s).poll_flush(cx),
        }
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            DialStream::Tcp(s) => std::pin::Pin::new(s).poll_shutdown(cx),
            DialStream::Channel(s) => std::pin::Pin::new(s).poll_shutdown(cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AuthMethod;

    /// 第一跳的地址决定了本机 TCP 连到哪。写错这一步,整条链都是错的。
    #[test]
    fn first_tcp_target_is_first_hop_not_final_destination() {
        let hops = vec![
            Hop::Socks5 {
                host: "127.0.0.1".into(),
                port: 7891,
                auth: None,
            },
            Hop::SshJump {
                host: "bastion".into(),
                port: 22,
                user: "ops".into(),
                auth: AuthMethod::Agent,
            },
        ];
        assert_eq!(
            first_tcp_target(&hops, "target", 2222),
            ("127.0.0.1".to_string(), 7891)
        );
    }

    #[test]
    fn empty_hops_dial_the_destination_directly() {
        assert_eq!(
            first_tcp_target(&[], "target", 2222),
            ("target".to_string(), 2222)
        );
    }

    /// 每一跳要连的「下一站」是它后面那一跳的地址;最后一跳连目标。
    #[test]
    fn next_stop_after_each_hop_walks_the_chain() {
        let hops = vec![
            Hop::Socks5 {
                host: "p".into(),
                port: 1080,
                auth: None,
            },
            Hop::SshJump {
                host: "b".into(),
                port: 22,
                user: "o".into(),
                auth: AuthMethod::Agent,
            },
        ];
        assert_eq!(next_stop(&hops, 0, "target", 2222), ("b".to_string(), 22));
        assert_eq!(
            next_stop(&hops, 1, "target", 2222),
            ("target".to_string(), 2222)
        );
    }

    #[test]
    fn single_hop_goes_straight_to_destination() {
        let hops = vec![Hop::HttpConnect {
            host: "p".into(),
            port: 8080,
            auth: None,
        }];
        assert_eq!(
            next_stop(&hops, 0, "target", 22),
            ("target".to_string(), 22)
        );
    }

    /// 内层 `ConnectionRefused` 自带一句「检查端口/sshd 是否在跑」的引导语。
    /// 包成 `ProxyUnreachable` 后,最终字符串只能剩外层那一句(指向代理),
    /// 不能把内层引导语原样嵌进去——两句指导语打架,用户不知道该查哪个。
    #[test]
    fn blame_first_hop_does_not_nest_a_second_guidance_sentence() {
        let hops = vec![Hop::Socks5 {
            host: "127.0.0.1".into(),
            port: 1080,
            auth: None,
        }];
        let inner = ConnectError::ConnectionRefused("Connection refused (os error 111)".into());
        let msg = blame_first_hop(&hops, inner).to_string();
        assert!(
            !msg.contains("sshd"),
            "不该残留内层「检查 sshd」引导语: {msg}"
        );
        assert_eq!(
            msg,
            "连不上代理 127.0.0.1:1080:Connection refused (os error 111) —— 检查代理是否在跑/地址端口是否写对"
        );
    }
}
