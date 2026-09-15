//! AWS Signature Version 4(F270)。**纯函数,零 IO、零网络。**
//!
//! 单独成模块的唯一理由:这是整个云端备份里**唯一能被官方测试向量证明对错**
//! 的部分。签名算错的症状是服务端回 403 `SignatureDoesNotMatch`,而它与
//! 「AK 权限不够」「时钟偏差太大」长得完全一样 —— 靠真机试错去区分,一次
//! 往返几秒钟,而且要真的有一个 bucket。向量测试是零成本的替代品。

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// 十六进制小写。SigV4 全篇用的都是小写 hex,大写会静默算出另一个签名。
pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// SHA-256 的十六进制摘要。空载荷也要算(SigV4 要求 `x-amz-content-sha256`
/// 永远有值,不能省略)。
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hmac(key: &[u8], msg: &[u8]) -> Vec<u8> {
    // `new_from_slice` 对 HMAC 只在密钥长度为 0 时才可能出错,而下面四次调用
    // 的密钥都非空;真出错了也不该 panic 掉整个 app,退回一个不可能匹配的
    // 空签名让服务端拒掉 —— 拿到的是一条 403,比进程没了强。
    match HmacSha256::new_from_slice(key) {
        Ok(mut m) => {
            m.update(msg);
            m.finalize().into_bytes().to_vec()
        }
        Err(_) => Vec::new(),
    }
}

/// 派生签名密钥。**四步的顺序是规范定死的**,见模块文档里那条症状。
pub fn signing_key(secret: &str, date: &str, region: &str, service: &str) -> Vec<u8> {
    let k_date = hmac(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let k_region = hmac(&k_date, region.as_bytes());
    let k_service = hmac(&k_region, service.as_bytes());
    hmac(&k_service, b"aws4_request")
}

/// 要签的这一次请求。**字段全是已经定型的字符串** —— 本模块不做任何
/// URI 编码/规范化,那是 `url.rs` 的事。两处都做的话,S3 的「不要二次编码」
/// 规则迟早被违反,而症状还是那条 403。
pub struct Request<'a> {
    pub method: &'a str,
    pub host: &'a str,
    /// 已编码的路径,以 `/` 开头。
    pub path: &'a str,
    /// 已编码、**已按名字排序**的 query string,不含 `?`。空串 = 没有 query。
    pub query: &'a str,
    /// 载荷的 SHA-256 十六进制。
    pub payload_sha256: &'a str,
    /// `YYYYMMDD'T'HHMMSS'Z'`。
    pub amz_date: &'a str,
    /// 除 `host` / `x-amz-date` / `x-amz-content-sha256` 之外还要签的头。
    /// 名字**必须已经是小写**(调用方给大写的话这里不替他改,因为改了之后
    /// 实际发出去的头与签进去的头就可能不是同一个)。
    pub extra_headers: &'a [(&'a str, &'a str)],
}

/// 算出 `Authorization` 头的完整值。
pub fn authorization(req: &Request<'_>, ak: &str, sk: &str, region: &str, service: &str) -> String {
    let date = &req.amz_date[..8];

    // 三个必签头 + 调用方给的。排序在这里做一次,签名与 SignedHeaders 用的
    // 是**同一个已排序列表** —— 分两处各排一次,迟早漂开。
    let mut headers: Vec<(String, String)> = vec![
        ("host".to_string(), req.host.to_string()),
        (
            "x-amz-content-sha256".to_string(),
            req.payload_sha256.to_string(),
        ),
        ("x-amz-date".to_string(), req.amz_date.to_string()),
    ];
    for (k, v) in req.extra_headers {
        headers.push((k.to_string(), v.trim().to_string()));
    }
    headers.sort_by(|a, b| a.0.cmp(&b.0));

    let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed_headers: Vec<&str> = headers.iter().map(|(k, _)| k.as_str()).collect();
    let signed_headers = signed_headers.join(";");

    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        req.method, req.path, req.query, canonical_headers, signed_headers, req.payload_sha256
    );

    let scope = format!("{date}/{region}/{service}/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{}\n{}",
        req.amz_date,
        scope,
        sha256_hex(canonical_request.as_bytes())
    );

    let key = signing_key(sk, date, region, service);
    let signature = hex(&hmac(&key, string_to_sign.as_bytes()));

    format!(
        "AWS4-HMAC-SHA256 Credential={ak}/{scope}, SignedHeaders={signed_headers}, Signature={signature}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `aws-sig-v4-test-suite` 那对示例凭据(AKIDEXAMPLE)的 secret。
    ///
    /// **注意结尾是 `ENG+bPx` 不是 `ENG/bPx`。** AWS 在不同文档里用了两对长得
    /// 极像的示例凭据:`AKIDEXAMPLE` 配 `...MDENG+bPx...`(本条向量用它),
    /// `AKIAIOSFODNN7EXAMPLE` 配 `...MDENG/bPx...`(下面 S3 GET Object 那条用它)。
    /// 配错一个字符,派生出的密钥完全不同,而症状只是一条对不上的 hex ——
    /// 很容易被误判成「实现写错了」而去改正确的实现。两条向量的期望值都已用
    /// openssl 独立复算核对过。
    const EX_SECRET: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";

    /// 派生链必须是 kDate → kRegion → kService → kSigning,**顺序不能换**。
    /// 顺序错了照样能算出一个 32 字节的东西,服务端只会回一个 403
    /// `SignatureDoesNotMatch` —— 与「AK 权限不够」长得一模一样。
    #[test]
    fn the_signing_key_matches_the_published_vector() {
        let got = signing_key(EX_SECRET, "20150830", "us-east-1", "iam");
        assert_eq!(
            hex(&got),
            "c4afb1cc5771d871763a393e44b703571b55cc28424d1a5e86da6ed3c154a4b9",
            "派生链与 AWS 公布的向量不符"
        );
    }

    /// AWS "Example: GET Object" 的完整端到端向量(期望值已用 openssl 复算)。
    ///
    /// 这一条同时钉住五件事:canonical request 的拼法、signed headers 的**排序与
    /// 小写**、string to sign 的三行结构、scope 的四段、Authorization 头的格式。
    /// 其中任何一件写错,服务端都只会回同一条 403。
    ///
    /// 注意这里的 secret 是**斜杠**那一版(配 AKIAIOSFODNN7EXAMPLE),
    /// 与上面 `EX_SECRET` 的加号版不是同一个。
    #[test]
    fn the_authorization_header_matches_the_published_get_object_vector() {
        let req = Request {
            method: "GET",
            host: "examplebucket.s3.amazonaws.com",
            // **已 URI-encode 过的**路径。SigV4 对 S3 要求「不要二次编码」,
            // 所以编码在 `url.rs` 那边一次做完,这里只管拼。
            path: "/test.txt",
            query: "",
            payload_sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            amz_date: "20130524T000000Z",
            extra_headers: &[("range", "bytes=0-9")],
        };
        let got = authorization(
            &req,
            "AKIAIOSFODNN7EXAMPLE",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            "us-east-1",
            "s3",
        );
        assert_eq!(
            got,
            "AWS4-HMAC-SHA256 \
             Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, \
             SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, \
             Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }

    /// signed headers 必须**按名字排序**,不是按调用方给的顺序。给的顺序反过来
    /// 也得算出同一个签名 —— 否则「加一个自定义头」就会随机地让签名失效。
    #[test]
    fn header_order_from_the_caller_does_not_change_the_signature() {
        let mk = |extra: &'static [(&'static str, &'static str)]| Request {
            method: "PUT",
            host: "b.example.com",
            path: "/k",
            query: "",
            payload_sha256: "abc",
            amz_date: "20260915T101500Z",
            extra_headers: extra,
        };
        let a = authorization(
            &mk(&[("x-a", "1"), ("x-b", "2")]),
            "AK",
            "SK",
            "cn-hangzhou",
            "s3",
        );
        let b = authorization(
            &mk(&[("x-b", "2"), ("x-a", "1")]),
            "AK",
            "SK",
            "cn-hangzhou",
            "s3",
        );
        assert_eq!(a, b, "signed headers 没排序 —— 调用方换个顺序签名就变了");
    }
}
