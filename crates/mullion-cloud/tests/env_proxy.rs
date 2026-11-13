//! 环境变量代理(F363 复核 I1)。
//!
//! 旧 ureq `Config` 默认读 ALL_PROXY/HTTPS_PROXY/HTTP_PROXY/NO_PROXY;自带连接器链
//! 必须保持这个行为,否则环境里的 HTTP 代理会被静默忽略(直连)。
//!
//! **独立 test target**:改进程环境变量会干扰同进程里别的测试,故不与 fake_s3 /
//! socks5 同进程;本文件内两条用例再用 Mutex 串行。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use mullion_cloud::s3::{Credentials, S3Client};
use mullion_cloud::url::Endpoint;

static ENV: Mutex<()> = Mutex::new(());

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
                            <Contents><Key>k1</Key></Contents></ListBucketResult>";
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

/// 假 HTTP CONNECT 代理:记下 CONNECT 行,回 200 后转发到 `target_port`。
fn fake_connect_proxy(target_port: u16) -> (u16, mpsc::Receiver<String>, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    let hits = Arc::new(AtomicUsize::new(0));
    let h2 = hits.clone();
    thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { break };
            h2.fetch_add(1, Ordering::SeqCst);
            let mut buf = Vec::new();
            let mut b = [0u8; 1];
            while !buf.ends_with(b"\r\n\r\n") {
                if s.read(&mut b).unwrap_or(0) == 0 {
                    break;
                }
                buf.push(b[0]);
            }
            let first = String::from_utf8_lossy(&buf)
                .lines()
                .next()
                .unwrap_or("")
                .to_string();
            let _ = tx.send(first);
            let _ = s.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n");
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
    (port, rx, hits)
}

fn list(host: &str, port: u16) -> Result<Vec<String>, mullion_cloud::CloudError> {
    S3Client::new_with_timeout(
        Endpoint {
            base: format!("http://{host}:{port}"),
            bucket: "b".into(),
            path_style: true,
        },
        Credentials {
            access_key_id: "AK".into(),
            secret_access_key: "SK".into(),
        },
        "r".into(),
        None,
        Duration::from_secs(5),
    )
    .expect("建客户端")
    .list_keys("", "20260915T101500Z")
}

fn clear_env() {
    for k in [
        "ALL_PROXY",
        "all_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "NO_PROXY",
        "no_proxy",
    ] {
        std::env::remove_var(k);
    }
}

/// 环境里的 HTTP 代理必须被用上:假代理要看到 `CONNECT 主机:端口`。
#[test]
fn an_http_proxy_from_the_environment_is_still_honored() {
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    clear_env();
    let target = fake_http();
    let (pp, rx, _hits) = fake_connect_proxy(target);
    std::env::set_var("ALL_PROXY", format!("http://127.0.0.1:{pp}"));
    // 目标主机名解析不了:只有走代理才可能成功。
    let r = list("s3.fake.test", target);
    clear_env();
    assert_eq!(r.expect("经环境代理 LIST 应成功"), vec!["k1"]);
    let line = rx
        .recv_timeout(Duration::from_secs(1))
        .expect("代理没被用到");
    assert_eq!(line, format!("CONNECT s3.fake.test:{target} HTTP/1.1"));
}

/// NO_PROXY 命中的主机必须直连,代理一次都不该被碰。
#[test]
fn no_proxy_from_the_environment_still_bypasses_the_proxy() {
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    clear_env();
    let target = fake_http();
    let (pp, _rx, hits) = fake_connect_proxy(target);
    std::env::set_var("ALL_PROXY", format!("http://127.0.0.1:{pp}"));
    std::env::set_var("NO_PROXY", "127.0.0.1");
    let r = list("127.0.0.1", target);
    clear_env();
    assert_eq!(r.expect("直连 LIST 应成功"), vec!["k1"]);
    assert_eq!(hits.load(Ordering::SeqCst), 0, "NO_PROXY 命中却仍连了代理");
}

/// 只数连接数、不应答的假代理(黑洞)。
fn counting_hole() -> (u16, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let h2 = hits.clone();
    thread::spawn(move || {
        let mut keep = Vec::new();
        for s in l.incoming().flatten() {
            h2.fetch_add(1, Ordering::SeqCst);
            keep.push(s);
        }
    });
    (port, hits)
}

/// 环境里的 SOCKS5 代理 + NO_PROXY 命中:直连,代理一次都不该被碰(自带 SOCKS5 连接器的 NO_PROXY 分支)。
#[test]
fn no_proxy_also_bypasses_a_socks5_proxy_from_the_environment() {
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    clear_env();
    let target = fake_http();
    let (pp, hits) = counting_hole();
    std::env::set_var("ALL_PROXY", format!("socks5://127.0.0.1:{pp}"));
    std::env::set_var("NO_PROXY", "127.0.0.1");
    let r = list("127.0.0.1", target);
    clear_env();
    assert_eq!(r.expect("直连 LIST 应成功"), vec!["k1"]);
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "NO_PROXY 命中却仍连了 SOCKS5 代理"
    );
}

/// 环境里的 SOCKS4 代理:我们不支持,必须报错而不是静默直连。
#[test]
fn a_socks4_proxy_from_the_environment_is_an_error_not_a_silent_direct_connection() {
    let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
    clear_env();
    let target = fake_http();
    std::env::set_var("ALL_PROXY", "socks4://127.0.0.1:9");
    let r = list("127.0.0.1", target);
    clear_env();
    let e = r.expect_err("SOCKS4 应报错").to_string();
    assert!(e.contains("SOCKS4"), "{e}");
}
