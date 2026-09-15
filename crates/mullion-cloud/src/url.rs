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
        // bucket 名进 Host 头时不编码(下面 virtual-hosted 分支),所以这里
        // 挡一道:含 `/` 或空白的 bucket 会拼出 authority 在半路被截断的
        // URL,而签名里的 host 仍是完整串 —— 又是一条 403,且看不出来。
        debug_assert!(
            !self.bucket.contains(['/', ' ']),
            "bucket 名不能含 `/` 或空格:{}",
            self.bucket
        );
        let base = self.base.trim_end_matches('/');
        // 用户没写 scheme 时补 https。**不要写成 `.map(|(s, h)| (s, h))`** ——
        // 那是 clippy 的 `map_identity`,`-D warnings` 下直接编不过。
        let (scheme, authority) = base.split_once("://").unwrap_or(("https", base));
        // **路径前缀必须从 host 里摘出来。** endpoint 被填成
        // `https://host/prefix`(用户粘贴时多带一段是常事)时,若把
        // `host/prefix` 整个当 Host 头去签名,而 HTTP 客户端按 RFC 3986
        // 只把第一个 `/` 之前的部分当 authority 发出去 —— 签的头和发的头
        // 不是同一个,必现 403 且零提示。摘出来并进 path,两边就始终同源,
        // 顺带支持「对象存储挂在反代子路径下」这种真实部署。
        let (host_only, prefix) = authority.split_once('/').unwrap_or((authority, ""));

        // 按段拼:空段一律不进去 —— 否则会拼出 `//` 或尾随 `/`。
        // 尤其是空 key(`list_keys` 列举 bucket 根时就是这么调的):
        // path-style 下必须得到 `/bucket` 而不是 `/bucket/`,后者是
        // 「键为空字符串的对象」而不是「bucket 根」,某些 S3 兼容实现
        // (自建 MinIO 正是 path-style 的主要用户)会按前者解释,回空列表。
        let mut segs: Vec<String> = Vec::new();
        if !prefix.is_empty() {
            segs.push(encode_key(prefix));
        }
        let host = if self.path_style {
            segs.push(encode_component(&self.bucket));
            host_only.to_string()
        } else {
            format!("{}.{}", self.bucket, host_only)
        };
        if !key.is_empty() {
            segs.push(encode_key(key));
        }
        let path = format!("/{}", segs.join("/"));
        Target {
            url: format!("{scheme}://{host}{path}"),
            host,
            path,
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

    /// 守的是「`url` 与 `path` 同源」这个结构性质——`url` 必须以 `path`
    /// 收尾,不能是两个各拼一次、可能悄悄分叉的字符串。
    ///
    /// **独立杀伤力有限**:当前实现里 `url` 就是拿 `path` 字面拼出来的
    /// (`format!("{scheme}://{host}{path}")`),所以只要 `path` 算对了,
    /// 这条测试几乎必然跟着 `virtual_hosted_puts_the_bucket_in_the_host`
    /// / `path_style_puts_the_bucket_in_the_path` 等精确断言同绿同红——
    /// 复核者实测过:把 path-style 分支的 path 改成丢掉 bucket 段,这条
    /// 依然全绿,真正抓到回归的是 `path_style_puts_the_bucket_in_the_path`。
    /// 它守不住「有人以后在 `s3.rs` 里另拼一次 URL、两处从此不同源」这种
    /// 跨文件回归——`url.rs` 这一层看不到 `s3.rs` 怎么用 `Target`。真要
    /// 守住这条,得等 `s3.rs` 落地后在那边加断言(比如断言它只用
    /// `t.url`/`t.path`,不自己再拼一次)。
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

    /// endpoint 带路径前缀(virtual-hosted)。host 里混进路径 = 签的头
    /// 与实际发出去的 Host 头不是同一个字符串(HTTP 客户端按 RFC 3986
    /// 在第一个 `/` 处截断 authority)——必现 403,且 url.rs 这一层零提示。
    /// 前缀必须摘出来并进 path,而不能留在 host 里。
    #[test]
    fn a_path_prefix_on_the_endpoint_moves_into_the_path_virtual_hosted() {
        let e = Endpoint {
            base: "https://host/p1/p2".into(),
            bucket: "b".into(),
            path_style: false,
        };
        let t = e.target("k");
        assert_eq!(t.host, "b.host", "host 里不能再混进路径前缀");
        assert_eq!(t.path, "/p1/p2/k");
        assert_eq!(t.url, "https://b.host/p1/p2/k");
    }

    /// 同上,path-style:前缀排在 bucket 之前,host 只剩纯 authority。
    #[test]
    fn a_path_prefix_on_the_endpoint_moves_into_the_path_path_style() {
        let e = Endpoint {
            base: "https://host/p1/p2".into(),
            bucket: "b".into(),
            path_style: true,
        };
        let t = e.target("k");
        assert_eq!(t.host, "host");
        assert_eq!(t.path, "/p1/p2/b/k");
    }

    /// `list_keys` 列举 bucket 根时就是拿空 key 调 `target`。path-style
    /// 下必须是 `/my-bucket`(不带尾斜杠)——带了会被部分 S3 兼容实现
    /// (自建 MinIO)解释成「键为空字符串的对象」而不是 bucket 根,回空列表。
    #[test]
    fn an_empty_key_does_not_leave_a_trailing_slash() {
        let mut e = oss();
        e.path_style = true;
        let t = e.target("");
        assert_eq!(t.path, "/my-bucket");
        assert!(!t.path.ends_with('/'));

        let t = oss().target("");
        assert_eq!(t.path, "/");
    }

    /// 非 ASCII key:逐字节 UTF-8 百分号编码、大写十六进制、`/` 保留
    /// (不然多级目录的对象会存到一个名字里带 `%2F` 的键上)。
    #[test]
    fn non_ascii_keys_are_percent_encoded_byte_by_byte_with_slash_preserved() {
        let t = oss().target("中文/文件.mpk");
        assert_eq!(t.path, "/%E4%B8%AD%E6%96%87/%E6%96%87%E4%BB%B6.mpk");
    }

    /// 路径前缀必须和 key 走同一条编码规则(`encode_key`):按 `/` 分段、
    /// 每段用 [`encode_component`] 编码、`/` 本身保留。守两件事:
    ///
    /// - **前缀不能不编码**——若改成 `segs.push(prefix.to_string())`,
    ///   原始字符会直接落进签名用的 `path` 和实际请求的 `url`;一旦底层
    ///   HTTP 客户端对路径里的非法字符做自动转义再发送,签名用的 path
    ///   与实际发出去的路径就分叉,回一条零提示的 403
    ///   `SignatureDoesNotMatch`——与本模块开头「endpoint 路径前缀混进
    ///   Host 头」是同一形状的坑。
    /// - 前缀里的 `/` 不能被转义成 `%2F`——否则多级前缀被拍扁成一段。
    ///
    /// **必须用含空格 + 非 ASCII 的多级前缀**:纯 ASCII 前缀(如
    /// `p1/p2`)编码前后字面完全相同,查不出「根本没编码」这条变异——
    /// 这正是这条覆盖此前漏掉的原因。期望串用 `encode_key` 本身的规则
    /// 手算(逐字节 UTF-8 百分号编码、大写十六进制)并用 Python 交叉核对
    /// 过,不是凭记忆抄的。
    #[test]
    fn a_path_prefix_is_percent_encoded_just_like_a_key() {
        let e = Endpoint {
            base: "https://host/my proxy/中文段".into(),
            bucket: "b".into(),
            path_style: false,
        };
        let t = e.target("k");
        assert_eq!(
            t.path, "/my%20proxy/%E4%B8%AD%E6%96%87%E6%AE%B5/k",
            "前缀必须和 key 一样逐段百分号编码,`/` 保留为分隔符"
        );
        assert_eq!(
            t.url,
            "https://b.host/my%20proxy/%E4%B8%AD%E6%96%87%E6%AE%B5/k"
        );
    }

    /// bucket 名进 Host 头不能编码,所以含 `/` 的 bucket 只能在开发期
    /// 就地炸掉——真编码了反而是错(Host 头里不能有百分号转义)。
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "bucket 名不能含")]
    fn a_bucket_name_containing_a_slash_panics_in_debug() {
        let e = Endpoint {
            base: "https://host".into(),
            bucket: "b/evil".into(),
            path_style: false,
        };
        e.target("k");
    }
}
