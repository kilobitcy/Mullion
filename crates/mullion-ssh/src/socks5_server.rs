//! F113:SOCKS5 **服务端**(RFC 1928)。`-D` 动态转发的本机侧。
//!
//! `proxy.rs` 已经实现了同一份 RFC 的**客户端**(F4)。两边共用的只有常量语义,
//! 不共用代码 —— 客户端是「发请求读应答」,服务端是「读请求发应答」,
//! 字节序方向相反,硬抽公共函数只会让两边都难读。共用的是**测试手法**:
//! 对 `AsyncRead + AsyncWrite` 泛型 + `tokio::io::duplex` 喂假客户端。
//!
//! 两条边界(技术事实,不是取舍,见设计 D8):
//!
//! - **不做 UDP ASSOCIATE**,也做不到:SSH 的 `direct-tcpip` 只搬 TCP,
//!   SOCKS5 的 UDP 转发在 SSH 上没有承载。收到就回 `0x07 命令不支持`。
//! - **只提供 `NO AUTH`**:D5 把 `-D` 锁死在回环,能连上它的只有本机进程,
//!   再加一层用户名口令挡不住任何东西,只增加配置面。

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const VER: u8 = 0x05;
const METHOD_NO_AUTH: u8 = 0x00;
const METHOD_NONE_ACCEPTABLE: u8 = 0xFF;

const CMD_CONNECT: u8 = 0x01;

const ATYP_IPV4: u8 = 0x01;
const ATYP_DOMAIN: u8 = 0x03;
const ATYP_IPV6: u8 = 0x04;

/// RFC 1928 §6 的 REP 码,只留我们会发的这几个。
pub(crate) const REP_SUCCESS: u8 = 0x00;
pub(crate) const REP_HOST_UNREACHABLE: u8 = 0x04;
pub(crate) const REP_CMD_NOT_SUPPORTED: u8 = 0x07;
pub(crate) const REP_ATYP_NOT_SUPPORTED: u8 = 0x08;

/// 客户端想连的目标。主机保持**原样字符串**(域名不在本地解析)——
/// `-D` 的常见用途正是「本机解析不了的内网名交给远端解析」,
/// 在这里做 DNS 等于把这个用途废掉。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Socks5Request {
    pub host: String,
    pub port: u16,
}

/// 协商失败的两类。分开是因为**能不能回话**不同:对面根本不是 SOCKS5 时,
/// 发一个 SOCKS5 格式的拒绝只是往一个不认识这套协议的连接里灌垃圾。
#[derive(Debug)]
pub(crate) enum Socks5Refusal {
    /// 已经完成方法协商,可以按 RFC 回一个 REP 码。
    Reply(u8),
    /// 连协议都不对(版本字节不是 5 / 不接受 NO AUTH),不回 REP。
    Fatal(&'static str),
    Io(std::io::Error),
}

impl From<std::io::Error> for Socks5Refusal {
    fn from(e: std::io::Error) -> Self {
        Socks5Refusal::Io(e)
    }
}

impl std::fmt::Display for Socks5Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Socks5Refusal::Reply(rep) => write!(f, "请求不受支持(REP={rep:#04x})"),
            Socks5Refusal::Fatal(why) => write!(f, "{why}"),
            Socks5Refusal::Io(e) => write!(f, "读写失败:{e}"),
        }
    }
}

/// 完成方法协商 + 读出 CONNECT 请求。
///
/// **不发最终应答** —— 那一步要等 SSH channel 真的开出来才知道该回
/// `0x00` 还是 `0x04`。抢先回 `0x00` 再发现开不出来,客户端会拿到一个
/// 「连上了但立刻断」的连接,比干脆的拒绝难排查得多。
pub(crate) async fn negotiate<S>(stream: &mut S) -> Result<Socks5Request, Socks5Refusal>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // 1) 方法协商:VER NMETHODS METHODS...
    let mut head = [0u8; 2];
    stream.read_exact(&mut head).await?;
    if head[0] != VER {
        return Err(Socks5Refusal::Fatal("对端不是 SOCKS5"));
    }
    let mut methods = vec![0u8; head[1] as usize];
    stream.read_exact(&mut methods).await?;
    if !methods.contains(&METHOD_NO_AUTH) {
        // RFC 要求这一句照发:客户端据此知道是「没有共同方法」而不是网络断了。
        stream.write_all(&[VER, METHOD_NONE_ACCEPTABLE]).await?;
        return Err(Socks5Refusal::Fatal("客户端不接受免认证方法"));
    }
    stream.write_all(&[VER, METHOD_NO_AUTH]).await?;

    // 2) 请求:VER CMD RSV ATYP DST.ADDR DST.PORT
    let mut req = [0u8; 4];
    stream.read_exact(&mut req).await?;
    if req[0] != VER {
        return Err(Socks5Refusal::Fatal("请求阶段版本字节不是 5"));
    }
    // **地址必须读完再拒绝**,否则残留字节会让后面那句 REP 落在客户端
    // 眼里错位。BIND(0x02)/UDP ASSOCIATE(0x03)都走这条。
    let host = match req[3] {
        ATYP_IPV4 => {
            let mut b = [0u8; 4];
            stream.read_exact(&mut b).await?;
            std::net::Ipv4Addr::from(b).to_string()
        }
        ATYP_IPV6 => {
            let mut b = [0u8; 16];
            stream.read_exact(&mut b).await?;
            std::net::Ipv6Addr::from(b).to_string()
        }
        ATYP_DOMAIN => {
            let mut l = [0u8; 1];
            stream.read_exact(&mut l).await?;
            let mut b = vec![0u8; l[0] as usize];
            stream.read_exact(&mut b).await?;
            // 域名按 RFC 是字节串;非 UTF-8 的主机名在 SSH 请求里也放不进去。
            String::from_utf8(b).map_err(|_| Socks5Refusal::Reply(REP_ATYP_NOT_SUPPORTED))?
        }
        _ => return Err(Socks5Refusal::Reply(REP_ATYP_NOT_SUPPORTED)),
    };
    let mut p = [0u8; 2];
    stream.read_exact(&mut p).await?;
    let port = u16::from_be_bytes(p);

    if req[1] != CMD_CONNECT {
        return Err(Socks5Refusal::Reply(REP_CMD_NOT_SUPPORTED));
    }
    Ok(Socks5Request { host, port })
}

/// 发最终应答。BND 字段一律填 `0.0.0.0:0`。
///
/// 那两个字段本该是「代理侧用于连出去的地址」,但 SSH 的 `direct-tcpip`
/// 不告诉我们远端用了哪个源地址 —— 编一个出来就是**假装知道**。
/// 填零是 SOCKS5 实现里的通行做法,客户端在 CONNECT 下不使用它。
pub(crate) async fn reply<S>(stream: &mut S, rep: u8) -> std::io::Result<()>
where
    S: AsyncWrite + Unpin,
{
    stream
        .write_all(&[VER, rep, 0x00, ATYP_IPV4, 0, 0, 0, 0, 0, 0])
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::DuplexStream;

    /// 造一个「假客户端」:返回服务端要用的那一头,以及一个已经把
    /// `bytes` 写进去的客户端头。
    async fn wire(bytes: &[u8]) -> (DuplexStream, DuplexStream) {
        let (server, mut client) = tokio::io::duplex(4096);
        // 缓冲 4096 远大于这些请求,写不会阻塞在没人读上。
        client.write_all(bytes).await.unwrap();
        (server, client)
    }

    fn greeting() -> Vec<u8> {
        vec![0x05, 0x01, 0x00]
    }

    #[tokio::test]
    async fn connect_to_a_domain_is_passed_through_unresolved() {
        let mut req = greeting();
        req.extend_from_slice(&[0x05, 0x01, 0x00, 0x03, 11]);
        req.extend_from_slice(b"db.internal");
        req.extend_from_slice(&3306u16.to_be_bytes());
        let (mut s, _c) = wire(&req).await;
        let got = negotiate(&mut s).await.unwrap();
        assert_eq!(
            got,
            Socks5Request {
                host: "db.internal".into(),
                port: 3306
            },
            "域名必须原样交给远端解析 —— 在本机解析会把 -D 最主要的用途废掉"
        );
    }

    #[tokio::test]
    async fn ipv4_and_ipv6_literals_are_formatted_as_addresses() {
        let mut v4 = greeting();
        v4.extend_from_slice(&[0x05, 0x01, 0x00, 0x01, 10, 0, 0, 7]);
        v4.extend_from_slice(&80u16.to_be_bytes());
        let (mut s, _c) = wire(&v4).await;
        assert_eq!(negotiate(&mut s).await.unwrap().host, "10.0.0.7");

        let mut v6 = greeting();
        v6.extend_from_slice(&[0x05, 0x01, 0x00, 0x04]);
        v6.extend_from_slice(&[0u8; 15]);
        v6.push(1);
        v6.extend_from_slice(&443u16.to_be_bytes());
        let (mut s, _c) = wire(&v6).await;
        let r = negotiate(&mut s).await.unwrap();
        assert_eq!(r.host, "::1");
        assert_eq!(r.port, 443);
    }

    /// SSH 的 `direct-tcpip` 只搬 TCP,UDP 转发在这条链路上**没有承载**。
    /// 回 `0x07` 而不是默默断开 —— 断开会让客户端以为是网络问题,
    /// 而它其实需要换一种代理。
    #[tokio::test]
    async fn udp_associate_is_refused_with_command_not_supported() {
        let mut req = greeting();
        req.extend_from_slice(&[0x05, 0x03, 0x00, 0x01, 1, 2, 3, 4]);
        req.extend_from_slice(&53u16.to_be_bytes());
        let (mut s, _c) = wire(&req).await;
        match negotiate(&mut s).await {
            Err(Socks5Refusal::Reply(REP_CMD_NOT_SUPPORTED)) => {}
            other => panic!("UDP ASSOCIATE 必须回 0x07,实际: {other:?}"),
        }
    }

    /// BIND 同理。**必须与 CONNECT 分开测**:只测 UDP 的话,把判据写成
    /// 「cmd == 0x03 才拒」也是绿的,而那会让 BIND 请求被当成 CONNECT 处理。
    #[tokio::test]
    async fn bind_command_is_refused_too() {
        let mut req = greeting();
        req.extend_from_slice(&[0x05, 0x02, 0x00, 0x01, 1, 2, 3, 4]);
        req.extend_from_slice(&8080u16.to_be_bytes());
        let (mut s, _c) = wire(&req).await;
        assert!(matches!(
            negotiate(&mut s).await,
            Err(Socks5Refusal::Reply(REP_CMD_NOT_SUPPORTED))
        ));
    }

    #[tokio::test]
    async fn unknown_address_type_is_refused_without_reading_garbage() {
        let mut req = greeting();
        req.extend_from_slice(&[0x05, 0x01, 0x00, 0x09]);
        let (mut s, _c) = wire(&req).await;
        assert!(matches!(
            negotiate(&mut s).await,
            Err(Socks5Refusal::Reply(REP_ATYP_NOT_SUPPORTED))
        ));
    }

    /// 非 SOCKS5 的字节流(例如有人把浏览器的 HTTP 代理指到了这个端口)
    /// 必须**不回 SOCKS5 格式的应答** —— 对面不认识这套协议,回了只是灌垃圾。
    #[tokio::test]
    async fn non_socks5_greeting_is_rejected_without_replying() {
        let (mut s, mut c) = wire(b"GET / HTTP/1.1\r\n").await;
        assert!(matches!(
            negotiate(&mut s).await,
            Err(Socks5Refusal::Fatal(_))
        ));
        drop(s);
        let mut buf = [0u8; 1];
        assert_eq!(
            c.read(&mut buf).await.unwrap(),
            0,
            "不该往非 SOCKS5 连接里写任何应答"
        );
    }

    /// 只提供 `NO AUTH`。客户端若只给用户名口令方法,要按 RFC 回 `05 FF`
    /// (而不是静默断),否则客户端分不出「方法不匹配」和「代理挂了」。
    #[tokio::test]
    async fn client_without_no_auth_gets_the_rfc_no_acceptable_methods_reply() {
        let (mut s, mut c) = wire(&[0x05, 0x01, 0x02]).await;
        assert!(matches!(
            negotiate(&mut s).await,
            Err(Socks5Refusal::Fatal(_))
        ));
        let mut buf = [0u8; 2];
        c.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf, [0x05, 0xFF]);
    }

    #[tokio::test]
    async fn successful_negotiation_selects_no_auth_before_the_request() {
        let mut req = greeting();
        req.extend_from_slice(&[0x05, 0x01, 0x00, 0x01, 127, 0, 0, 1]);
        req.extend_from_slice(&22u16.to_be_bytes());
        let (mut s, mut c) = wire(&req).await;
        negotiate(&mut s).await.unwrap();
        let mut buf = [0u8; 2];
        c.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf, [0x05, 0x00], "必须先回一句方法选择");
    }

    #[tokio::test]
    async fn reply_is_ten_bytes_with_a_zero_bnd_address() {
        let (mut s, mut c) = tokio::io::duplex(64);
        reply(&mut s, REP_HOST_UNREACHABLE).await.unwrap();
        let mut buf = [0u8; 10];
        c.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf, [0x05, 0x04, 0x00, 0x01, 0, 0, 0, 0, 0, 0]);
    }
}
