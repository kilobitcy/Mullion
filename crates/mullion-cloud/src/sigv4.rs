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
}
