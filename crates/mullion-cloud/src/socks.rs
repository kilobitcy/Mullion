//! 自带的 SOCKS5 连接器(F363),替代 ureq 的 `socks-proxy` 特性。
//!
//! # 为什么不用 ureq 自带的
//!
//! ureq 3.4.2 的 `SocksConnector` 把握手丢进 `thread::scope`,用 `recv_timeout`
//! 限时 —— 但超时后 scope 退出时仍要 **join** 那个子线程,而子线程卡在 `socks`
//! crate 的**无超时读**上。代理接受 TCP 却不应答时,整次请求无限挂起(华为 P50
//! 真机 13 分钟以上),`timeout_global` 形同虚设。
//!
//! 这里每一步 IO 都挂 `set_read_timeout`/`set_write_timeout`,握手总预算由一个
//! 截止时刻统一约束,不开线程。
//!
//! # 协议范围
//!
//! 只做 RFC 1928 的「无认证(0x00)+ CONNECT」。目标地址:域名走 ATYP 0x03
//! (**让代理解析**,即 `socks5h` 语义);IP 字面量走 0x01/0x04。
//! 注意 ureq 原来对 `socks5://` 是**本机先解析、发 IP** —— 本连接器有意改为
//! 发域名:手机端本机 DNS 常被污染/不可达,而代理才是能解析目标的那一端。
//!
//! # 错误文案
//!
//! 一律固定中文,**不回显代理返回的原始字节**(只回显我们自己解析出的 REP 数值)。

use std::io::{self, Read, Write};
use std::net::{IpAddr, TcpStream};
use std::time::{Duration, Instant};

use ureq::unversioned::transport::{
    Buffers, ConnectionDetails, Connector, Either, LazyBuffers, NextTimeout, Transport,
};
use ureq::{Error, ProxyProtocol};

/// 握手阶段的 IO 错误。固定文案,不带对端内容。
fn bad(msg: &'static str) -> Error {
    Error::Io(io::Error::new(io::ErrorKind::InvalidData, msg))
}

fn timed_out() -> Error {
    Error::Io(io::Error::new(
        io::ErrorKind::TimedOut,
        "SOCKS5 代理握手超时(代理接受了连接但没有应答)",
    ))
}

/// 第 `idx` 次(0 起)连接尝试能分到多少:非最后一个地址拿剩余预算的一半,
/// 最后一个拿全部剩余 —— 前面的黑洞地址吃不光整个预算,后面的还有机会。
fn attempt_budget(left: Duration, idx: usize, total: usize) -> Duration {
    if idx + 1 >= total {
        left
    } else {
        left / 2
    }
}

/// 握手预算:每步 IO 前按剩余时间重设读写超时,总时长不超过截止时刻。
struct Budget(Instant);

impl Budget {
    fn left(&self) -> Result<Duration, Error> {
        let d = self.0.saturating_duration_since(Instant::now());
        if d.is_zero() {
            Err(timed_out())
        } else {
            Ok(d)
        }
    }
}

fn io_err(e: io::Error) -> Error {
    match e.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => timed_out(),
        io::ErrorKind::UnexpectedEof => bad("SOCKS5 代理在握手中途断开了连接"),
        _ => Error::Io(e),
    }
}

fn send(s: &mut TcpStream, b: &Budget, data: &[u8]) -> Result<(), Error> {
    s.set_write_timeout(Some(b.left()?)).map_err(io_err)?;
    s.write_all(data).map_err(io_err)
}

fn recv(s: &mut TcpStream, b: &Budget, buf: &mut [u8]) -> Result<(), Error> {
    s.set_read_timeout(Some(b.left()?)).map_err(io_err)?;
    s.read_exact(buf).map_err(io_err)
}

/// 在已连上代理的流上做 SOCKS5 握手。`host` 是域名或 IP 字面量(IPv6 不带方括号)。
fn handshake(s: &mut TcpStream, b: &Budget, host: &str, port: u16) -> Result<(), Error> {
    // 方法协商:只提供「无认证」。
    send(s, b, &[0x05, 0x01, 0x00])?;
    let mut r = [0u8; 2];
    recv(s, b, &mut r)?;
    if r[0] != 0x05 {
        return Err(bad("对端不是 SOCKS5 代理(版本号不对)"));
    }
    if r[1] != 0x00 {
        return Err(bad("SOCKS5 代理要求认证,本客户端只支持无认证"));
    }

    // CONNECT 请求。
    let mut req = vec![0x05, 0x01, 0x00];
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(a)) => {
            req.push(0x01);
            req.extend_from_slice(&a.octets());
        }
        Ok(IpAddr::V6(a)) => {
            req.push(0x04);
            req.extend_from_slice(&a.octets());
        }
        Err(_) => {
            let n = u8::try_from(host.len()).map_err(|_| bad("目标域名过长"))?;
            req.push(0x03);
            req.push(n);
            req.extend_from_slice(host.as_bytes());
        }
    }
    req.extend_from_slice(&port.to_be_bytes());
    send(s, b, &req)?;

    let mut h = [0u8; 4];
    recv(s, b, &mut h)?;
    if h[0] != 0x05 {
        return Err(bad("对端不是 SOCKS5 代理(版本号不对)"));
    }
    if h[1] != 0x00 {
        // 只回显我们自己解出的数值,并给出 RFC 1928 的固定释义。
        let why = match h[1] {
            0x01 => "代理内部故障",
            0x02 => "代理规则不允许该连接",
            0x03 => "网络不可达",
            0x04 => "目标主机不可达",
            0x05 => "目标拒绝连接",
            0x06 => "TTL 过期",
            0x07 => "代理不支持 CONNECT",
            0x08 => "代理不支持该地址类型",
            _ => "未知原因",
        };
        return Err(Error::Io(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            format!("SOCKS5 代理拒绝了连接(REP={}:{why})", h[1]),
        )));
    }
    // 读掉绑定地址,让流停在正文起点。
    let tail = match h[3] {
        0x01 => 4 + 2,
        0x04 => 16 + 2,
        0x03 => {
            let mut n = [0u8; 1];
            recv(s, b, &mut n)?;
            n[0] as usize + 2
        }
        _ => return Err(bad("SOCKS5 代理回了未知的地址类型")),
    };
    let mut sink = vec![0u8; tail];
    recv(s, b, &mut sink)
}

/// 目标主机与端口(从 URI 取,默认端口按 scheme)。
fn target(details: &ConnectionDetails) -> Result<(String, u16), Error> {
    let auth = details
        .uri
        .authority()
        .ok_or_else(|| bad("请求地址缺少主机"))?;
    let port = auth
        .port_u16()
        .unwrap_or(if details.uri.scheme_str() == Some("https") {
            443
        } else {
            80
        });
    let host = auth.host().trim_matches(|c| c == '[' || c == ']');
    Ok((host.to_string(), port))
}

/// SOCKS5 连接器。只在 `Config` 里配了 socks5/socks5h 代理时出手,否则原样放行
/// 链上上一环(交给后面的 `TcpConnector`)。
#[derive(Debug)]
pub struct Socks5Connector {
    /// 握手总预算上限。实际取它与请求全局剩余时间的较小者。
    pub handshake_timeout: Duration,
}

impl<In: Transport> Connector<In> for Socks5Connector {
    type Out = Either<In, SocksTransport>;

    fn connect(
        &self,
        details: &ConnectionDetails,
        chained: Option<In>,
    ) -> Result<Option<Self::Out>, Error> {
        let proxy = match details.config.proxy() {
            Some(p) if matches!(p.protocol(), ProxyProtocol::Socks5 | ProxyProtocol::Socks5h) => p,
            // 环境变量里的 SOCKS4 代理:不支持就明说,不能静默退化成直连。
            Some(p)
                if matches!(p.protocol(), ProxyProtocol::Socks4 | ProxyProtocol::Socks4A)
                    && !p.is_no_proxy(details.uri) =>
            {
                return Err(bad("不支持 SOCKS4 代理,请改用 SOCKS5"));
            }
            _ => return Ok(chained.map(Either::A)),
        };
        if chained.is_some() {
            return Ok(chained.map(Either::A));
        }
        // NO_PROXY 命中:直连(交给链上的 TcpConnector)。
        if proxy.is_no_proxy(details.uri) {
            return Ok(None);
        }

        let mut budget = self.handshake_timeout;
        if let Some(t) = details.timeout.not_zero() {
            if !t.is_not_happening() {
                budget = budget.min(*t);
            }
        }
        let deadline = Budget(Instant::now() + budget);

        // 代理主机解析走 ureq 的 resolver(自带限时),不在这里裸调阻塞 DNS。
        let addrs = details
            .resolver
            .resolve(proxy.uri(), details.config, details.timeout)?;
        let mut last = None;
        let mut stream = None;
        let total = addrs.len();
        for (i, a) in addrs.iter().enumerate() {
            let per = attempt_budget(deadline.left()?, i, total);
            match TcpStream::connect_timeout(a, per) {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(e) => last = Some(io_err(e)),
            }
        }
        let Some(mut stream) = stream else {
            return Err(last.unwrap_or_else(timed_out));
        };

        let (host, port) = target(details)?;
        handshake(&mut stream, &deadline, &host, port)?;

        // 握手用的超时必须清掉:`SocksTransport` 的 `timeout_*` 记账从 None 起步,
        // 若套接字上还留着握手的超时,ureq 后续「值没变就不重设」会让它一直生效。
        stream.set_read_timeout(None).map_err(io_err)?;
        stream.set_write_timeout(None).map_err(io_err)?;
        if details.config.no_delay() {
            stream.set_nodelay(true).map_err(io_err)?;
        }
        let buffers = LazyBuffers::new(
            details.config.input_buffer_size(),
            details.config.output_buffer_size(),
        );
        Ok(Some(Either::B(SocksTransport {
            stream,
            buffers,
            timeout_read: None,
            timeout_write: None,
        })))
    }
}

/// 握手完成后的字节流。ureq 的 `TcpTransport` 不对外导出,这里照其逻辑写一份:
/// 每次读写前按 `NextTimeout`(含 `timeout_global` 余量)设套接字超时。
#[derive(Debug)]
pub struct SocksTransport {
    stream: TcpStream,
    buffers: LazyBuffers,
    timeout_read: Option<Duration>,
    timeout_write: Option<Duration>,
}

fn upd(
    t: NextTimeout,
    prev: &mut Option<Duration>,
    s: &TcpStream,
    f: impl Fn(&TcpStream, Option<Duration>) -> io::Result<()>,
) -> io::Result<()> {
    let want = t.not_zero().map(|d| *d);
    if want != *prev {
        f(s, want)?;
        *prev = want;
    }
    Ok(())
}

impl Transport for SocksTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), Error> {
        upd(
            timeout,
            &mut self.timeout_write,
            &self.stream,
            TcpStream::set_write_timeout,
        )?;
        let out = &self.buffers.output()[..amount];
        match self.stream.write_all(out) {
            Ok(()) => Ok(()),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                Err(Error::Timeout(timeout.reason))
            }
            Err(e) => Err(e.into()),
        }
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, Error> {
        upd(
            timeout,
            &mut self.timeout_read,
            &self.stream,
            TcpStream::set_read_timeout,
        )?;
        let buf = self.buffers.input_append_buf();
        let n = match self.stream.read(buf) {
            Ok(n) => n,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                return Err(Error::Timeout(timeout.reason))
            }
            Err(e) => return Err(e.into()),
        };
        self.buffers.input_appended(n);
        Ok(n > 0)
    }

    fn is_open(&mut self) -> bool {
        // 池化复用前的探活:非阻塞读,有字节(垃圾)或出错即视为已死。
        if self.stream.set_nonblocking(true).is_err() {
            return false;
        }
        let mut b = [0u8; 1];
        let alive = matches!(self.stream.read(&mut b),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock);
        alive && self.stream.set_nonblocking(false).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn the_first_of_several_proxy_addresses_cannot_eat_the_whole_budget() {
        let left = Duration::from_secs(60);
        assert_eq!(attempt_budget(left, 0, 2), Duration::from_secs(30));
        assert_eq!(attempt_budget(left, 1, 2), left);
        assert_eq!(attempt_budget(left, 0, 1), left);
    }

    /// 域名超过 255 字节放不进 ATYP 0x03 的单字节长度:必须报错,不能截断后发出去。
    #[test]
    fn a_domain_over_255_bytes_is_refused_not_truncated() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let srv = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut g = [0u8; 3];
            s.read_exact(&mut g).unwrap();
            s.write_all(&[5, 0]).unwrap();
            // 之后客户端应直接放弃:读到 EOF,而不是收到一个 CONNECT 请求。
            let mut rest = Vec::new();
            let _ = s.read_to_end(&mut rest);
            rest
        });
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let b = Budget(Instant::now() + Duration::from_secs(5));
        let long = "a".repeat(256);
        let e = handshake(&mut c, &b, &long, 80).expect_err("256 字节域名应报错");
        assert!(e.to_string().contains("过长"), "{e}");
        drop(c);
        assert!(srv.join().unwrap().is_empty(), "超长域名居然被发出去了");
        // 255 字节恰好放得下(边界):握手能走到发出 CONNECT。
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let srv = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut g = [0u8; 3];
            s.read_exact(&mut g).unwrap();
            s.write_all(&[5, 0]).unwrap();
            let mut req = [0u8; 4 + 1 + 255 + 2];
            s.read_exact(&mut req).unwrap();
            s.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).unwrap();
        });
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        handshake(&mut c, &b, &"a".repeat(255), 80).expect("255 字节应可发");
        srv.join().unwrap();
    }
}
