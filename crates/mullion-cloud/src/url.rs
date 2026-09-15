//! endpoint + bucket + key → 请求 URL 与 `Host` 头(F270)。**纯函数。**
//!
//! 两种寻址方式都得支持:阿里云 OSS / AWS S3 默认是 virtual-hosted
//! (`<bucket>.<endpoint>`),而自建 MinIO 通常只能走 path-style
//! (`<endpoint>/<bucket>`)—— 选了「通吃」就不能只做一种。
//!
//! **签名用的 path 与实际请求的 path 必须逐字节一致**,所以两者从同一个函数
//! 出来。分两处各拼一次的话,path-style 下签名里少个 `/bucket` 前缀,服务端
//! 回 403 `SignatureDoesNotMatch` —— 又是那条什么都看不出来的错。

/// 一台对象存储 + 一个 bucket。
#[derive(Debug, Clone)]
pub struct Endpoint {
    /// 形如 `https://oss-cn-hangzhou.aliyuncs.com`,末尾斜杠会被吃掉。
    pub base: String,
    pub bucket: String,
    /// `true` = `<base>/<bucket>/<key>`(MinIO 等自建服务多半只支持这种);
    /// `false` = `<bucket>.<host>/<key>`(OSS / S3 / R2 的默认)。
    pub path_style: bool,
}

/// 一次请求要用到的三件套。**三者同源**,见模块文档。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub url: String,
    pub host: String,
    /// 签名用的 canonical path(已编码,以 `/` 开头)。
    pub path: String,
}

/// RFC 3986 的 unreserved 之外一律转义。**`/` 也转义** —— 本函数只用来编
/// query 的值,那里的 `/` 必须是 `%2F`(canonical query string 的规则)。
fn encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => {
                use std::fmt::Write as _;
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

/// 对象键的编码:与 [`encode_component`] 同规则,**但保留 `/`** —— 键里的
/// 斜杠是路径分隔符,转义掉之后对象会存到一个名字里带 `%2F` 的键上,而且
/// 一切正常、零报错,只有在控制台里看才发现层级没了。
fn encode_key(s: &str) -> String {
    s.split('/')
        .map(encode_component)
        .collect::<Vec<_>>()
        .join("/")
}

/// 按名字排序后拼成 canonical query string。
pub fn query_string(params: &[(&str, &str)]) -> String {
    let mut v: Vec<(&str, &str)> = params.to_vec();
    v.sort_by(|a, b| a.0.cmp(b.0));
    v.iter()
        .map(|(k, val)| format!("{}={}", encode_component(k), encode_component(val)))
        .collect::<Vec<_>>()
        .join("&")
}

impl Endpoint {
    /// 算出访问 `key` 要用的 URL / Host / 签名 path。
    pub fn target(&self, key: &str) -> Target {
        let base = self.base.trim_end_matches('/');
        // 用户没写 scheme 时补 https。**不要写成 `.map(|(s, h)| (s, h))`** ——
        // 那是 clippy 的 `map_identity`,`-D warnings` 下直接编不过。
        let (scheme, host_only) = base.split_once("://").unwrap_or(("https", base));
        let enc = encode_key(key);
        if self.path_style {
            let path = format!("/{}/{}", encode_component(&self.bucket), enc);
            Target {
                url: format!("{scheme}://{host_only}{path}"),
                host: host_only.to_string(),
                path,
            }
        } else {
            let host = format!("{}.{}", self.bucket, host_only);
            let path = format!("/{enc}");
            Target {
                url: format!("{scheme}://{host}{path}"),
                host,
                path,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oss() -> Endpoint {
        Endpoint {
            base: "https://oss-cn-hangzhou.aliyuncs.com".into(),
            bucket: "my-bucket".into(),
            path_style: false,
        }
    }

    #[test]
    fn virtual_hosted_puts_the_bucket_in_the_host() {
        let t = oss().target("mullion/000001-20260915T101500Z.mpk");
        assert_eq!(t.host, "my-bucket.oss-cn-hangzhou.aliyuncs.com");
        assert_eq!(t.path, "/mullion/000001-20260915T101500Z.mpk");
        assert_eq!(
            t.url,
            "https://my-bucket.oss-cn-hangzhou.aliyuncs.com/mullion/000001-20260915T101500Z.mpk"
        );
    }

    #[test]
    fn path_style_puts_the_bucket_in_the_path() {
        let mut e = oss();
        e.path_style = true;
        let t = e.target("mullion/a.mpk");
        assert_eq!(t.host, "oss-cn-hangzhou.aliyuncs.com");
        assert_eq!(
            t.path, "/my-bucket/mullion/a.mpk",
            "path-style 下 bucket 必须进 path —— 而签名签的就是这个 path"
        );
    }

    /// 签名里的 path 与 URL 里的 path 是**同一个字符串**。这条钉住的是
    /// 「有人以后为了省事在 s3.rs 里另拼一次 URL」。
    #[test]
    fn the_signed_path_is_the_tail_of_the_url() {
        for path_style in [false, true] {
            let mut e = oss();
            e.path_style = path_style;
            let t = e.target("mullion/x.mpk");
            assert!(
                t.url.ends_with(&t.path),
                "path_style={path_style} 时 URL 尾巴不是签名用的 path:{} vs {}",
                t.url,
                t.path
            );
        }
    }

    /// 末尾斜杠是用户手打 endpoint 时最常见的多余字符。不吃掉的话
    /// URL 里会出现 `//`,而**签名里也会**,于是签名对得上但 OSS 把
    /// 它当成一个名字以 `/` 开头的对象 —— 静默存到一个谁都找不到的键上。
    #[test]
    fn a_trailing_slash_on_the_endpoint_is_absorbed() {
        let e = Endpoint {
            base: "https://oss-cn-hangzhou.aliyuncs.com/".into(),
            bucket: "b".into(),
            path_style: false,
        };
        assert_eq!(
            e.target("k").url,
            "https://b.oss-cn-hangzhou.aliyuncs.com/k"
        );
    }

    /// query 参数必须**按名字排序**后拼 —— SigV4 的 canonical query string
    /// 要求如此,而我们把同一个字符串既拿去签名又拿去发请求。
    #[test]
    fn query_parameters_are_sorted_by_name() {
        let q = query_string(&[("prefix", "mullion/"), ("list-type", "2")]);
        assert_eq!(q, "list-type=2&prefix=mullion%2F");
    }
}
