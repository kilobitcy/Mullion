//! 自带 SOCKS5 连接器的协议测试(F363)。
//!
//! 起因:ureq 的 socks 连接器在代理「接受 TCP 但不应答」时无限挂起(真机 13 分钟+)。
//! 这里用本地假 SOCKS5 服务端覆盖:黑洞(必须限时报错)、正常握手(断言握手字节)、
//! 代理拒绝 / 选了不支持的方法(报错且不回显代理回的内容)。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use mullion_cloud::error::CloudError;
use mullion_cloud::s3::{Credentials, S3Client};
use mullion_cloud::url::Endpoint;

/// 假代理的行为。
#[derive(Clone, Copy)]
enum Mode {
    /// 握手完成后把流量转给目标;`Bnd` 是 CONNECT 应答里绑定地址的类型。
    Relay(Bnd),
    /// 接受连接后一声不吭(并保持连接不关)。
    Blackhole,
    /// 方法协商时回 0x02(要求用户名密码)。
    PickAuthMethod,
    /// CONNECT 回 REP=0x05,并在绑定地址里塞一段标记文本。
    Refuse,
}

/// CONNECT 应答里的 BND.ADDR 类型(客户端必须按类型读对长度,否则流错位)。
#[derive(Clone, Copy, Debug)]
enum Bnd {
    V4,
    Domain,
    V6,
}

const MARK: &str = "PROXY-RAW-MARK";

/// 假代理收到的握手内容。
#[derive(Debug, PartialEq)]
struct Handshake {
    greeting: Vec<u8>,
    atyp: u8,
    host: String,
    port: u16,
}

fn read_n(s: &mut TcpStream, n: usize) -> Vec<u8> {
    let mut b = vec![0u8; n];
    s.read_exact(&mut b).expect("假代理读握手");
    b
}

fn fake_socks(mode: Mode, target_port: u16) -> (u16, mpsc::Receiver<Handshake>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        // 黑洞要把连接留着直到测试进程结束,不能 drop(drop 会让客户端读到 EOF 而不是挂起)。
        let mut keep = Vec::new();
        for s in l.incoming() {
            let Ok(mut s) = s else { break };
            if matches!(mode, Mode::Blackhole) {
                keep.push(s);
                continue;
            }
            let mut greeting = read_n(&mut s, 2);
            greeting.extend(read_n(&mut s, greeting[1] as usize));
            if matches!(mode, Mode::PickAuthMethod) {
                let _ = s.write_all(&[5, 2]);
                continue;
            }
            s.write_all(&[5, 0]).unwrap();
            let h = read_n(&mut s, 4);
            let (host, atyp) = match h[3] {
                3 => {
                    let n = read_n(&mut s, 1)[0] as usize;
                    (String::from_utf8(read_n(&mut s, n)).unwrap(), 3)
                }
                1 => {
                    let a = read_n(&mut s, 4);
                    (format!("{}.{}.{}.{}", a[0], a[1], a[2], a[3]), 1)
                }
                t => panic!("意外的 ATYP {t}"),
            };
            let p = read_n(&mut s, 2);
            let port = u16::from_be_bytes([p[0], p[1]]);
            let _ = tx.send(Handshake {
                greeting,
                atyp,
                host,
                port,
            });
            if matches!(mode, Mode::Refuse) {
                let mut r = vec![5, 5, 0, 3, MARK.len() as u8];
                r.extend_from_slice(MARK.as_bytes());
                r.extend_from_slice(&[0, 0]);
                let _ = s.write_all(&r);
                continue;
            }
            let Mode::Relay(bnd) = mode else {
                unreachable!()
            };
            let reply: Vec<u8> = match bnd {
                Bnd::V4 => vec![5, 0, 0, 1, 1, 2, 3, 4, 0, 80],
                Bnd::Domain => [&[5, 0, 0, 3, 5][..], b"bnd.x", &[0, 80]].concat(),
                Bnd::V6 => [&[5, 0, 0, 4][..], &[7u8; 16], &[0, 80]].concat(),
            };
            s.write_all(&reply).unwrap();
            let up = TcpStream::connect(("127.0.0.1", target_port)).unwrap();
            let (mut c2, mut u2) = (s.try_clone().unwrap(), up.try_clone().unwrap());
            thread::spawn(move || {
                let _ = std::io::copy(&mut c2, &mut u2);
            });
            let (mut s, mut up) = (s, up);
            thread::spawn(move || {
                let _ = std::io::copy(&mut up, &mut s);
            });
        }
    });
    (port, rx)
}

/// 目标 HTTP 服务:对每个请求回一页固定的 LIST 结果。
fn fake_http() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { break };
            thread::spawn(move || {
                let mut buf = Vec::new();
                let mut b = [0u8; 1];
                while !buf.ends_with(b"\r\n\r\n") {
                    if s.read(&mut b).unwrap_or(0) == 0 {
                        return;
                    }
                    buf.push(b[0]);
                }
                let body = "<ListBucketResult><IsTruncated>false</IsTruncated>\
                            <Contents><Key>mullion/000001-a.mpk</Key></Contents></ListBucketResult>";
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            });
        }
    });
    port
}

fn client(proxy_port: u16, target_port: u16, timeout: Duration) -> S3Client {
    S3Client::new_with_timeout(
        Endpoint {
            base: format!("http://s3.fake.test:{target_port}"),
            bucket: "b".into(),
            path_style: true,
        },
        Credentials {
            access_key_id: "AK".into(),
            secret_access_key: "SK".into(),
        },
        "r".into(),
        Some(&format!("127.0.0.1:{proxy_port}")),
        timeout,
    )
    .expect("建客户端")
}

/// 在子线程里跑 LIST,主线程限时等;超过 `hard` 说明挂起,直接 panic,不让套件卡死。
fn list_with_cap(c: S3Client, hard: Duration) -> (Result<Vec<String>, CloudError>, Duration) {
    let (tx, rx) = mpsc::channel();
    let t0 = Instant::now();
    thread::spawn(move || {
        let _ = tx.send(c.list_keys("mullion/", "20260915T101500Z"));
    });
    match rx.recv_timeout(hard) {
        Ok(r) => (r, t0.elapsed()),
        Err(_) => panic!("请求在 {hard:?} 内没有返回 —— 握手挂起了(F363 回归)"),
    }
}

/// 黑洞代理:接受 TCP 但不应答。必须在「超时 + 余量」内报错,而不是挂起。
#[test]
fn a_black_hole_proxy_fails_within_the_timeout_instead_of_hanging() {
    let (pp, _rx) = fake_socks(Mode::Blackhole, 0);
    let timeout = Duration::from_secs(2);
    let (r, took) = list_with_cap(client(pp, 1, timeout), timeout * 3);
    let e = r.expect_err("黑洞代理应报错");
    assert!(took < timeout + Duration::from_secs(1), "耗时 {took:?}");
    assert!(matches!(e, CloudError::Transport(_)), "{e:?}");
}

/// 正常握手:LIST 成功,且握手字节正确(只提供无认证;CONNECT 用域名 0x03 + 端口)。
#[test]
fn a_socks5_handshake_sends_no_auth_and_a_domain_connect() {
    let target = fake_http();
    let (pp, rx) = fake_socks(Mode::Relay(Bnd::V4), target);
    let (r, _) = list_with_cap(
        client(pp, target, Duration::from_secs(5)),
        Duration::from_secs(15),
    );
    assert_eq!(r.expect("LIST 应成功"), vec!["mullion/000001-a.mpk"]);
    let h = rx.recv().expect("假代理没收到握手");
    assert_eq!(
        h,
        Handshake {
            greeting: vec![5, 1, 0],
            atyp: 3,
            host: "s3.fake.test".into(),
            port: target,
        }
    );
}

/// 代理回 REP=0x05:报错,文案不含代理在绑定地址里塞的原始内容。
#[test]
fn a_refused_connect_is_an_error_without_echoing_proxy_bytes() {
    let (pp, _rx) = fake_socks(Mode::Refuse, 0);
    let (r, _) = list_with_cap(
        client(pp, 1, Duration::from_secs(5)),
        Duration::from_secs(15),
    );
    let e = r.expect_err("REP≠0 应报错").to_string();
    assert!(e.contains("拒绝"), "{e}");
    assert!(!e.contains(MARK), "错误回显了代理的原始内容:{e}");
}

/// 代理选了我们没提供的方法(0x02 用户名密码):报错,不继续发 CONNECT。
#[test]
fn an_unsupported_auth_method_is_an_error() {
    let (pp, _rx) = fake_socks(Mode::PickAuthMethod, 0);
    let (r, _) = list_with_cap(
        client(pp, 1, Duration::from_secs(5)),
        Duration::from_secs(15),
    );
    let e = r.expect_err("选了 0x02 应报错").to_string();
    assert!(e.contains("认证"), "{e}");
}

/// 应答里的绑定地址是域名(0x03)/ IPv6(0x04)时也要读对长度:读少了,
/// 残留字节会被当成 HTTP 响应的开头,LIST 随即解析失败。
#[test]
fn a_connect_reply_with_a_domain_or_ipv6_bound_address_is_consumed_whole() {
    for bnd in [Bnd::V4, Bnd::Domain, Bnd::V6] {
        let target = fake_http();
        let (pp, _rx) = fake_socks(Mode::Relay(bnd), target);
        let (r, _) = list_with_cap(
            client(pp, target, Duration::from_secs(5)),
            Duration::from_secs(15),
        );
        assert_eq!(
            r.unwrap_or_else(|e| panic!("{bnd:?}: {e}")),
            vec!["mullion/000001-a.mpk"]
        );
    }
}
