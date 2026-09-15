//! 假 S3 服务端 + 协议流程测试(F270)。
//!
//! 手法照抄 `mullion-ssh` 的「假 sshd + 拿自家客户端打自家服务端」——
//! 真 bucket 的端到端在 `tests/live.rs`(`#[ignore]`,要 AK/SK)。
//!
//! **这一层证不了签名对不对**(假 server 不验签),那是 `sigv4.rs` 里官方
//! 向量的活。这里证的是:方法/路径/头拼对了没、409 有没有被认成
//! `AlreadyExists`、分页有没有跟着续页令牌走完。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;

use mullion_cloud::error::CloudError;
use mullion_cloud::s3::{Credentials, S3Client};
use mullion_cloud::url::Endpoint;

/// 服务端收到的一次请求(测试要断言的部分)。
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    authorization: String,
    body: Vec<u8>,
    forbid_overwrite: Option<String>,
    if_none_match: Option<String>,
}

/// 起一个只回放固定响应的 HTTP server。返回 (端口, 收到的请求的接收端)。
///
/// `replies` 按顺序回放,用完之后一律回 500 —— **不循环回放最后一条**:
/// 循环的话「多发了一次请求」这种 bug 会被静默吸收掉。
fn serve(replies: Vec<(u16, String)>) -> (u16, mpsc::Receiver<Seen>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut replies = replies.into_iter();
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { break };
            let Some(seen) = read_request(&mut s) else {
                break;
            };
            let _ = tx.send(seen);
            let (code, body) = replies.next().unwrap_or((500, String::new()));
            // 3xx 自动带 Location,因为跳转降级是这一族唯一要测的东西——
            // 不给 `replies` 的元组加第三个字段,免得改动已有 5 条测试的
            // 调用形态。
            let location = if (300..400).contains(&code) {
                "Location: https://elsewhere.invalid/moved\r\n"
            } else {
                ""
            };
            let resp = format!(
                "HTTP/1.1 {code} X\r\n{location}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = s.write_all(resp.as_bytes());
            let _ = s.flush();
        }
    });
    (port, rx)
}

fn read_request(s: &mut TcpStream) -> Option<Seen> {
    let mut r = BufReader::new(s.try_clone().ok()?);
    let mut line = String::new();
    r.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let mut authorization = String::new();
    let mut forbid_overwrite = None;
    let mut if_none_match = None;
    let mut len = 0usize;
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).ok()? == 0 || h.trim().is_empty() {
            break;
        }
        let lower = h.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("authorization:") {
            authorization = v.trim().to_string();
        }
        if let Some(v) = lower.strip_prefix("x-oss-forbid-overwrite:") {
            forbid_overwrite = Some(v.trim().to_string());
        }
        if let Some(v) = lower.strip_prefix("if-none-match:") {
            if_none_match = Some(v.trim().to_string());
        }
        if let Some(v) = lower.strip_prefix("content-length:") {
            len = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; len];
    if len > 0 {
        r.read_exact(&mut body).ok()?;
    }
    Some(Seen {
        method,
        path,
        authorization,
        body,
        forbid_overwrite,
        if_none_match,
    })
}

fn client(port: u16) -> S3Client {
    S3Client::new(
        Endpoint {
            base: format!("http://127.0.0.1:{port}"),
            bucket: "b".into(),
            // 假 server 只有一个 IP,virtual-hosted 的 `b.127.0.0.1` 解析不了
            // —— path-style 是本地测试唯一走得通的寻址方式。
            path_style: true,
        },
        Credentials {
            access_key_id: "AK".into(),
            secret_access_key: "SK".into(),
        },
        "cn-hangzhou".into(),
        None,
    )
    .expect("建客户端")
}

#[test]
fn a_put_sends_the_bytes_and_signs_the_request() {
    let (port, rx) = serve(vec![(200, String::new())]);
    client(port)
        .put_no_overwrite("mullion/000001-x.mpk", b"hello", "20260915T101500Z")
        .expect("PUT 应该成功");
    let seen = rx.recv().expect("服务端没收到请求");
    assert_eq!(seen.method, "PUT");
    assert_eq!(seen.path, "/b/mullion/000001-x.mpk");
    assert_eq!(seen.body, b"hello");
    assert!(
        seen.authorization
            .starts_with("aws4-hmac-sha256 credential=ak/20260915/cn-hangzhou/s3/"),
        "Authorization 头不对:{}",
        seen.authorization
    );
}

/// **不带 ForbidOverwrite 的 PUT 会静默覆盖别的机器刚推上去的那一份。**
/// 而追加式序号布局的全部安全性就建立在这个头上(设计 D8:OSS 没有
/// PUT If-Match,这是唯一能用的并发保护)。
#[test]
fn a_put_always_asks_the_server_to_refuse_overwriting() {
    let (port, rx) = serve(vec![(200, String::new())]);
    let _ = client(port).put_no_overwrite("k", b"x", "20260915T101500Z");
    let seen = rx.recv().expect("没收到请求");
    assert_eq!(
        seen.forbid_overwrite.as_deref(),
        Some("true"),
        "PUT 没带 x-oss-forbid-overwrite —— 阿里云 OSS 上并发时会静默覆盖别人的备份"
    );
    // **两个头都要断言。** 只守一个的话,漏发另一个在对应的那一族服务端
    // 上就是「完全没有并发保护」,而客户端侧看到的是一次成功的 PUT ——
    // 零报错、测试全绿。这正是项目里「列举式门控在加档时必然漏」的形状。
    assert_eq!(
        seen.if_none_match.as_deref(),
        Some("*"),
        "PUT 没带 If-None-Match: * —— S3/R2/MinIO 上并发时会静默覆盖别人的备份"
    );
}

/// 409 必须被认成 `AlreadyExists`,而不是一条普通的 `Status`。
/// 调用方要靠这个分辨「换个序号重试」和「报失败给用户」。
#[test]
fn a_409_becomes_already_exists_not_a_generic_status_error() {
    let (port, _rx) = serve(vec![(
        409,
        "<Error><Code>FileAlreadyExists</Code></Error>".into(),
    )]);
    let e = client(port)
        .put_no_overwrite("k", b"x", "20260915T101500Z")
        .expect_err("409 应该报错");
    assert!(
        matches!(e, CloudError::AlreadyExists),
        "409 没被认成 AlreadyExists,拿到的是 {e:?}"
    );
}

/// 非 2xx 的响应正文必须带进错误里:对象存储的真实原因全在正文的
/// `<Code>` 里(SignatureDoesNotMatch / AccessDenied / NoSuchBucket),
/// 只报状态码等于把唯一有用的信息扔了。
#[test]
fn an_error_response_carries_the_server_message() {
    let (port, _rx) = serve(vec![(
        403,
        "<Error><Code>SignatureDoesNotMatch</Code></Error>".into(),
    )]);
    let e = client(port)
        .put_no_overwrite("k", b"x", "20260915T101500Z")
        .expect_err("403 应该报错");
    match e {
        CloudError::Status { code, body } => {
            assert_eq!(code, 403);
            assert!(body.contains("SignatureDoesNotMatch"), "正文丢了:{body}");
        }
        other => panic!("期望 Status,拿到 {other:?}"),
    }
}

/// 分页必须跟着续页令牌走完。只取第一页的话,超过 1000 个对象之后
/// 「最大序号」就是错的,下一次上传会撞上已存在的键。
#[test]
fn listing_follows_the_continuation_token_until_it_is_gone() {
    let page1 = "<ListBucketResult><IsTruncated>true</IsTruncated>\
                 <NextContinuationToken>t2</NextContinuationToken>\
                 <Contents><Key>mullion/000001-a.mpk</Key></Contents></ListBucketResult>";
    let page2 = "<ListBucketResult><IsTruncated>false</IsTruncated>\
                 <Contents><Key>mullion/000002-b.mpk</Key></Contents></ListBucketResult>";
    let (port, rx) = serve(vec![(200, page1.into()), (200, page2.into())]);
    let keys = client(port)
        .list_keys("mullion/", "20260915T101500Z")
        .expect("LIST 失败");
    assert_eq!(
        keys,
        vec![
            "mullion/000001-a.mpk".to_string(),
            "mullion/000002-b.mpk".to_string()
        ]
    );
    let first = rx.recv().expect("第一页请求");
    assert!(
        first.path.contains("list-type=2"),
        "不是 ListObjectsV2:{}",
        first.path
    );
    let second = rx.recv().expect("没发第二页请求 —— 续页令牌被忽略了");
    assert!(
        second.path.contains("continuation-token=t2"),
        "第二页没带令牌:{}",
        second.path
    );
}

/// PUT 碰到 301 必须报错,**不能被 ureq 悄悄降级成的匿名 GET 骗过**。
/// ureq 默认跟随重定向时,非 GET/HEAD 方法在 301/302/303 上会被改写成
/// GET、丢弃 body、丢弃 Authorization——那次 GET 若恰好拿到 2xx,我们
/// 只看状态码的话就会把它当成写入成功,而对象存储上其实什么都没多。
/// 错误信息里必须带得上 Location,不然用户不知道该往哪改配置。
#[test]
fn a_put_that_gets_redirected_is_reported_not_silently_turned_into_a_get() {
    let (port, _rx) = serve(vec![(301, String::new())]);
    let e = client(port)
        .put_no_overwrite("k", b"x", "20260915T101500Z")
        .expect_err("301 应该报错,而不是被降级成的匿名 GET 骗成功");
    match e {
        CloudError::Config(msg) => assert!(
            msg.contains("https://elsewhere.invalid/moved"),
            "错误信息里没带 Location:{msg}"
        ),
        other => panic!("期望 Config,拿到 {other:?}"),
    }
}

/// LIST 碰到 302 同样必须报错,不能返回半份 keys ——
/// 半份列表会让「最大序号」算错,下一次上传撞上已存在的键。
#[test]
fn a_list_that_gets_redirected_is_reported_not_partially_returned() {
    let (port, _rx) = serve(vec![(302, String::new())]);
    let e = client(port)
        .list_keys("mullion/", "20260915T101500Z")
        .expect_err("302 应该报错");
    assert!(
        matches!(e, CloudError::Config(_)),
        "302 没被认成 Config,拿到 {e:?}"
    );
}

/// 代理串解析失败必须报出来,**不能悄悄退化成直连**。直连若恰好在内网
/// 通(常见情况),后果是备份"成功"但完全绕开了用户明确要求的代理路由,
/// 界面上还显示"代理已配置",零提示。
#[test]
fn a_bad_proxy_string_is_reported_instead_of_silently_falling_back_to_a_direct_connection() {
    // `S3Client` 没有 `Debug`(agent 里的连接池等不值得为了测试去实现),
    // 用 `match` 而不是 `expect_err`。
    match S3Client::new(
        Endpoint {
            base: "http://127.0.0.1:1".into(),
            bucket: "b".into(),
            path_style: true,
        },
        Credentials {
            access_key_id: "AK".into(),
            secret_access_key: "SK".into(),
        },
        "cn-hangzhou".into(),
        // 带空格的串解析不成 URI 的 authority —— `ureq::Proxy::new` 对它
        // 可靠地返回 `Err`(已用一个独立的 scratch crate 核实过,不是
        // 凭记忆猜的)。
        Some("not a proxy"),
    ) {
        Ok(_) => panic!("代理串解析不了应该报错,不该悄悄退化成直连"),
        Err(e) => assert!(matches!(e, CloudError::Config(_)), "拿到 {e:?}"),
    }
}
