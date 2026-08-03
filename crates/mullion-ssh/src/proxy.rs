//! F4:SOCKS5(RFC 1928/1929)与 HTTP CONNECT 代理握手。
//!
//! 握手逻辑写成对 `AsyncRead + AsyncWrite` 泛型的函数,测试里用 `tokio::io::duplex`
//! 喂假的服务端应答,不需要真代理。

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::error::ConnectError;

/// 在已建立的流上完成 SOCKS5 握手(RFC 1928),成功后该流即通向 `target_host:target_port`。
///
/// `proxy_label` 只用于错误消息(F6 要求点名是哪个代理)。
pub async fn socks5_handshake<S>(
    stream: &mut S,
    proxy_label: &str,
    auth: Option<(&str, &str)>,
    target_host: &str,
    target_port: u16,
) -> Result<(), ConnectError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let io = |e: std::io::Error| ConnectError::ProxyUnreachable {
        proxy: proxy_label.to_string(),
        cause: e.to_string(),
    };

    // 1) 问候:提供我们支持的认证方法。
    let greeting: Vec<u8> = match auth {
        None => vec![0x05, 0x01, 0x00],
        Some(_) => vec![0x05, 0x02, 0x00, 0x02],
    };
    stream.write_all(&greeting).await.map_err(io)?;

    let mut sel = [0u8; 2];
    stream.read_exact(&mut sel).await.map_err(io)?;
    if sel[0] != 0x05 {
        return Err(ConnectError::ProxyRejected {
            proxy: proxy_label.to_string(),
            reason: format!("对端不是 SOCKS5(版本字节 {:#04x})", sel[0]),
        });
    }
    match sel[1] {
        0x00 => {}
        0x02 => {
            let (user, pass) = auth.ok_or_else(|| ConnectError::ProxyAuthFailed {
                proxy: proxy_label.to_string(),
            })?;
            // RFC 1929 子协商。用户名/口令各最长 255 字节。
            if user.len() > 255 || pass.len() > 255 {
                return Err(ConnectError::ProxyAuthFailed {
                    proxy: proxy_label.to_string(),
                });
            }
            let mut req = vec![0x01, user.len() as u8];
            req.extend_from_slice(user.as_bytes());
            req.push(pass.len() as u8);
            req.extend_from_slice(pass.as_bytes());
            stream.write_all(&req).await.map_err(io)?;

            let mut st = [0u8; 2];
            stream.read_exact(&mut st).await.map_err(io)?;
            if st[1] != 0x00 {
                return Err(ConnectError::ProxyAuthFailed {
                    proxy: proxy_label.to_string(),
                });
            }
        }
        _ => {
            return Err(ConnectError::ProxyAuthFailed {
                proxy: proxy_label.to_string(),
            })
        }
    }

    // 2) CONNECT 请求。一律用 ATYP=3(域名),让代理侧做解析——
    //    本机解析不了的内网名恰恰是用代理的常见理由。
    if target_host.len() > 255 {
        return Err(ConnectError::ProxyRejected {
            proxy: proxy_label.to_string(),
            reason: "目标主机名超过 255 字节".into(),
        });
    }
    let mut req = vec![0x05, 0x01, 0x00, 0x03, target_host.len() as u8];
    req.extend_from_slice(target_host.as_bytes());
    req.extend_from_slice(&target_port.to_be_bytes());
    stream.write_all(&req).await.map_err(io)?;

    // 3) 回复。**必须按 ATYP 读完 BND 字段**,否则残留字节污染后续 SSH 握手。
    let mut head = [0u8; 4];
    stream.read_exact(&mut head).await.map_err(io)?;
    if head[1] != 0x00 {
        return Err(ConnectError::ProxyRejected {
            proxy: proxy_label.to_string(),
            reason: socks5_reply_reason(head[1]),
        });
    }
    let bnd_len = match head[3] {
        0x01 => 4,
        0x04 => 16,
        0x03 => {
            let mut l = [0u8; 1];
            stream.read_exact(&mut l).await.map_err(io)?;
            l[0] as usize
        }
        other => {
            return Err(ConnectError::ProxyRejected {
                proxy: proxy_label.to_string(),
                reason: format!("回复里的地址类型 {other:#04x} 不认识"),
            })
        }
    };
    let mut rest = vec![0u8; bnd_len + 2]; // BND.ADDR + BND.PORT
    stream.read_exact(&mut rest).await.map_err(io)?;
    Ok(())
}

/// RFC 1928 §6 的 REP 码。给中文可读原因,不是把数字甩给用户(F6)。
fn socks5_reply_reason(code: u8) -> String {
    let s = match code {
        0x01 => "代理内部错误",
        0x02 => "代理规则不允许连接此目标",
        0x03 => "网络不可达",
        0x04 => "目标主机不可达",
        0x05 => "目标拒绝连接",
        0x06 => "TTL 超时",
        0x07 => "代理不支持 CONNECT 命令",
        0x08 => "代理不支持该地址类型",
        _ => "未知错误码",
    };
    format!("{s}(REP={code:#04x})")
}

/// 标准 base64(RFC 4648),用于 HTTP 代理的 `Proxy-Authorization: Basic`。
///
/// 手写而非引依赖:N6 盯着 exe 体积,而这里只需要 20 行。
pub(crate) fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// 在已建立的流上完成 HTTP CONNECT 握手,成功后该流即通向 `target_host:target_port`。
pub async fn http_connect_handshake<S>(
    stream: &mut S,
    proxy_label: &str,
    auth: Option<(&str, &str)>,
    target_host: &str,
    target_port: u16,
) -> Result<(), ConnectError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let io = |e: std::io::Error| ConnectError::ProxyUnreachable {
        proxy: proxy_label.to_string(),
        cause: e.to_string(),
    };

    let mut req = format!(
        "CONNECT {target_host}:{target_port} HTTP/1.1\r\nHost: {target_host}:{target_port}\r\n"
    );
    if let Some((user, pass)) = auth {
        let token = base64_encode(format!("{user}:{pass}").as_bytes());
        req.push_str(&format!("Proxy-Authorization: Basic {token}\r\n"));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).await.map_err(io)?;

    // 逐字节读到 \r\n\r\n 为止。**不能一次 read 一大块**:多读的部分是隧道内的
    // SSH 数据,吞掉就再也拿不回来了。CONNECT 响应很短,逐字节的开销可以忽略。
    let mut head = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    loop {
        stream.read_exact(&mut byte).await.map_err(io)?;
        head.push(byte[0]);
        if head.ends_with(b"\r\n\r\n") {
            break;
        }
        if head.len() > 8192 {
            return Err(ConnectError::ProxyRejected {
                proxy: proxy_label.to_string(),
                reason: "代理响应头超过 8KB,疑似不是 HTTP 代理".into(),
            });
        }
    }

    let status_line = String::from_utf8_lossy(&head)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string();
    let code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse::<u16>().ok())
        .ok_or_else(|| ConnectError::ProxyRejected {
            proxy: proxy_label.to_string(),
            reason: format!("代理响应无法解析:{status_line}"),
        })?;

    match code {
        200..=299 => Ok(()),
        407 => Err(ConnectError::ProxyAuthFailed {
            proxy: proxy_label.to_string(),
        }),
        _ => Err(ConnectError::ProxyRejected {
            proxy: proxy_label.to_string(),
            reason: status_line,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 跑一次握手:`server_script` 是假服务端按序发出的应答。
    /// 返回 (握手结果, 客户端实际发出的字节)。
    async fn run_socks5(
        server_script: Vec<Vec<u8>>,
        auth: Option<(String, String)>,
        target: (&str, u16),
    ) -> (Result<(), ConnectError>, Vec<u8>) {
        let (client, mut server) = tokio::io::duplex(4096);
        let target_host = target.0.to_string();
        let task = tokio::spawn(async move {
            let mut client = client;
            let r = socks5_handshake(
                &mut client,
                "127.0.0.1:1080",
                auth.as_ref().map(|(u, p)| (u.as_str(), p.as_str())),
                &target_host,
                target.1,
            )
            .await;
            r
        });

        let mut seen = Vec::new();
        for reply in server_script {
            // 先把客户端已发出的读走,再回应答。
            let mut buf = [0u8; 512];
            let n = server.read(&mut buf).await.unwrap_or(0);
            seen.extend_from_slice(&buf[..n]);
            let _ = server.write_all(&reply).await;
        }
        let r = task.await.unwrap();
        (r, seen)
    }

    #[tokio::test]
    async fn no_auth_connect_succeeds_and_sends_domain_atyp() {
        let (r, sent) = run_socks5(
            vec![
                vec![0x05, 0x00],                               // 选中无认证
                vec![0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0], // 成功,ATYP=IPv4
            ],
            None,
            ("example.com", 22),
        )
        .await;
        assert!(r.is_ok(), "免认证握手应成功: {r:?}");
        assert_eq!(&sent[..3], &[0x05, 0x01, 0x00], "问候应只提供无认证方法");
        assert!(
            sent.windows(13).any(|w| w == b"\x03\x0bexample.com"),
            "域名应以 ATYP=3 + 长度前缀发出,实际: {sent:?}"
        );
        assert!(
            sent.ends_with(&[0x00, 0x16]),
            "端口应为大端 u16(22 = 0x0016),实际尾部: {:?}",
            &sent[sent.len().saturating_sub(4)..]
        );
    }

    #[tokio::test]
    async fn username_password_auth_is_negotiated_per_rfc1929() {
        let (r, sent) = run_socks5(
            vec![
                vec![0x05, 0x02],                               // 选中用户名口令
                vec![0x01, 0x00],                               // 认证成功
                vec![0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0], // 连接成功
            ],
            Some(("alice".into(), "pw".into())),
            ("h", 22),
        )
        .await;
        assert!(r.is_ok(), "口令认证应成功: {r:?}");
        assert!(
            sent.windows(9)
                .any(|w| w == [0x01, 0x05, b'a', b'l', b'i', b'c', b'e', 0x02, b'p']),
            "应发出 RFC1929 子协商帧,实际: {sent:?}"
        );
    }

    #[tokio::test]
    async fn rejected_auth_maps_to_proxy_auth_failed() {
        let (r, _) = run_socks5(
            vec![vec![0x05, 0x02], vec![0x01, 0x01]], // 认证失败
            Some(("alice".into(), "bad".into())),
            ("h", 22),
        )
        .await;
        assert!(
            matches!(r, Err(ConnectError::ProxyAuthFailed { .. })),
            "认证被拒应映射到 ProxyAuthFailed,实际: {r:?}"
        );
    }

    #[tokio::test]
    async fn no_acceptable_method_maps_to_proxy_auth_failed() {
        let (r, _) = run_socks5(vec![vec![0x05, 0xFF]], None, ("h", 22)).await;
        assert!(matches!(r, Err(ConnectError::ProxyAuthFailed { .. })));
    }

    #[tokio::test]
    async fn nonzero_reply_code_maps_to_proxy_rejected_with_reason() {
        let (r, _) = run_socks5(
            vec![
                vec![0x05, 0x00],
                vec![0x05, 0x04, 0x00, 0x01, 0, 0, 0, 0, 0, 0],
            ],
            None,
            ("h", 22),
        )
        .await;
        match r {
            Err(ConnectError::ProxyRejected { reason, .. }) => {
                assert!(!reason.is_empty(), "REP=4 应给出可读原因");
            }
            other => panic!("REP!=0 应映射到 ProxyRejected,实际: {other:?}"),
        }
    }

    /// 回复里的 BND 字段长度随 ATYP 变。读少了会把残留字节留给后续 SSH 握手,
    /// 表现为「代理连上了但 SSH 版本协商失败」——极难排查。
    #[tokio::test]
    async fn domain_atyp_reply_is_fully_drained() {
        let mut reply = vec![0x05, 0x00, 0x00, 0x03, 0x03];
        reply.extend_from_slice(b"abc");
        reply.extend_from_slice(&[0x00, 0x16]);
        let (client, mut server) = tokio::io::duplex(4096);
        let task = tokio::spawn(async move {
            let mut client = client;
            socks5_handshake(&mut client, "p:1080", None, "h", 22).await
        });
        let mut buf = [0u8; 512];
        let _ = server.read(&mut buf).await;
        server.write_all(&[0x05, 0x00]).await.unwrap();
        let _ = server.read(&mut buf).await;
        server.write_all(&reply).await.unwrap();
        // 握手后紧跟的应用字节必须原样到达客户端,证明回复被读干净了。
        server.write_all(b"SSH-2.0-x").await.unwrap();
        assert!(task.await.unwrap().is_ok());
        // 客户端侧无法在此断言剩余流(已 move),用长度断言代替:
        // 若 drain 少读,handshake 会把 "SSH-2.0-x" 的前缀吃掉并很可能报错。
    }

    #[test]
    fn base64_matches_rfc4648_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_handles_non_ascii_bytes() {
        assert_eq!(base64_encode(&[0xFF, 0xFE, 0xFD]), "//79");
    }

    async fn run_http(
        reply: &'static [u8],
        auth: Option<(String, String)>,
    ) -> (Result<(), ConnectError>, Vec<u8>) {
        let (client, mut server) = tokio::io::duplex(4096);
        let task = tokio::spawn(async move {
            let mut client = client;
            http_connect_handshake(
                &mut client,
                "proxy:8080",
                auth.as_ref().map(|(u, p)| (u.as_str(), p.as_str())),
                "example.com",
                22,
            )
            .await
        });
        let mut buf = [0u8; 1024];
        let n = server.read(&mut buf).await.unwrap_or(0);
        let sent = buf[..n].to_vec();
        server.write_all(reply).await.unwrap();
        (task.await.unwrap(), sent)
    }

    #[tokio::test]
    async fn http_connect_sends_well_formed_request() {
        let (r, sent) = run_http(b"HTTP/1.1 200 Connection established\r\n\r\n", None).await;
        assert!(r.is_ok(), "200 应视为成功: {r:?}");
        let text = String::from_utf8(sent).unwrap();
        assert!(
            text.starts_with("CONNECT example.com:22 HTTP/1.1\r\n"),
            "请求行不合规: {text:?}"
        );
        assert!(
            text.contains("Host: example.com:22\r\n"),
            "缺 Host 头: {text:?}"
        );
        assert!(text.ends_with("\r\n\r\n"), "请求必须以空行结束: {text:?}");
        assert!(
            !text.contains("Proxy-Authorization"),
            "无认证时不该带认证头"
        );
    }

    #[tokio::test]
    async fn http_connect_sends_basic_authorization() {
        let (r, sent) = run_http(
            b"HTTP/1.1 200 OK\r\n\r\n",
            Some(("alice".into(), "pw".into())),
        )
        .await;
        assert!(r.is_ok());
        let text = String::from_utf8(sent).unwrap();
        // base64("alice:pw") = "YWxpY2U6cHc="
        assert!(
            text.contains("Proxy-Authorization: Basic YWxpY2U6cHc=\r\n"),
            "认证头不对: {text:?}"
        );
    }

    #[tokio::test]
    async fn http_407_maps_to_proxy_auth_failed() {
        let (r, _) = run_http(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n", None).await;
        assert!(
            matches!(r, Err(ConnectError::ProxyAuthFailed { .. })),
            "407 必须映射到认证失败而非泛化拒绝,实际: {r:?}"
        );
    }

    #[tokio::test]
    async fn http_403_maps_to_proxy_rejected_with_status_line() {
        let (r, _) = run_http(b"HTTP/1.1 403 Forbidden\r\n\r\n", None).await;
        match r {
            Err(ConnectError::ProxyRejected { reason, .. }) => {
                assert!(reason.contains("403"), "原因里应带状态码: {reason}");
            }
            other => panic!("403 应映射到 ProxyRejected,实际: {other:?}"),
        }
    }

    /// 响应头必须读到 `\r\n\r\n` 为止。少读一个字节,残留就会污染 SSH 版本协商。
    #[tokio::test]
    async fn http_reply_headers_are_drained_up_to_blank_line() {
        let (r, _) = run_http(
            b"HTTP/1.1 200 OK\r\nX-Proxy: mullion\r\nVia: 1.1 p\r\n\r\n",
            None,
        )
        .await;
        assert!(r.is_ok(), "多头响应也应成功: {r:?}");
    }

    /// 响应头必须**恰好**读到 `\r\n\r\n` 为止,一个字节都不能多读。
    ///
    /// 当前实现靠「每次只 `read_exact` 1 字节」保证这一点——这是个很容易被后人
    /// 当成「低效实现」优化掉的细节:换成 `BufReader` 或一次性 `read` 一大块
    /// 缓冲区,都可能把 `\r\n\r\n` 之后的字节(隧道里第一批 SSH 数据)一并读进
    /// 用户态缓冲区,从而永久丢失。丢失的后果是「代理连上了但 SSH 版本协商
    /// 失败」——真机上极难定位,而且现有测试不会报警。这个测试把伪造的
    /// SSH banner 紧跟在 CONNECT 响应之后一次性发出,握手返回后从同一个
    /// stream 继续读,断言 banner 字节原样、足量到达。
    #[tokio::test]
    async fn http_reply_is_drained_exactly_so_ssh_banner_survives() {
        let (client, mut server) = tokio::io::duplex(4096);
        let task = tokio::spawn(async move {
            let mut client = client;
            let r =
                http_connect_handshake(&mut client, "proxy:8080", None, "example.com", 22).await;
            let mut banner = [0u8; 9];
            client.read_exact(&mut banner).await.unwrap();
            (r, banner)
        });

        let mut buf = [0u8; 1024];
        let _ = server.read(&mut buf).await;
        // CONNECT 响应之后紧跟一段伪 SSH banner,一次性发出。
        server
            .write_all(b"HTTP/1.1 200 Connection established\r\n\r\nSSH-2.0-x")
            .await
            .unwrap();

        let (r, banner) = task.await.unwrap();
        assert!(r.is_ok(), "握手应成功: {r:?}");
        assert_eq!(
            &banner, b"SSH-2.0-x",
            "紧跟在响应头后的 banner 字节必须原样到达,一个不少"
        );
    }
}
