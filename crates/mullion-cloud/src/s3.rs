//! S3 兼容对象存储的两个操作:条件写与列举(F270)。**本 crate 唯一碰网络的地方。**
//!
//! 阻塞式(ureq)。调用方必须在 `spawn_blocking` 里用它 —— 在事件循环里同步
//! 跑网络会把帧率打到零(T3/T7)。

use std::time::Duration;

use crate::error::CloudError;
use crate::sigv4::{self, sha256_hex};
use crate::socks::Socks5Connector;
use crate::url::{query_string, Endpoint};
use crate::xml;
use ureq::unversioned::resolver::DefaultResolver;
use ureq::unversioned::transport::{
    ConnectProxyConnector, Connector, RustlsConnector, TcpConnector,
};

/// 单次请求的超时。高延迟代理链路是本项目的主场景,给得比"感觉够用"宽一档;
/// 但必须有限 —— 没有超时的话一次卡住的上传会把 `spawn_blocking` 的线程
/// 永久占住,而定时器还在往里塞新的。
const TIMEOUT: Duration = Duration::from_secs(60);

/// 一次能列多少个。取服务端上限,减少往返。
const MAX_KEYS: &str = "1000";

/// 续页的次数上限。**不是性能考虑,是防失控** —— 服务端若因某种原因永远
/// 回同一个令牌,没有上限就是一个不会结束的循环,而它跑在后台线程里,
/// 用户只看得见「备份一直在转」。
const MAX_PAGES: usize = 64;

/// LIST 单页响应正文上限。恶意/出错的服务端可以回任意大的页,不设上限就是
/// 内存炸弹(ureq 默认 10MB 也不是调用方的决定)。
const MAX_LIST_PAGE_BYTES: u64 = 2 * 1024 * 1024;

/// 跨页累计键数上限。同样防失控:每页都在上限内也能靠页数堆出大内存。
const MAX_LIST_KEYS: usize = 100_000;

/// 错误响应正文最多读这么多(只用于诊断,随后还会被截成 400 字符)。
const MAX_ERR_BODY_BYTES: u64 = 64 * 1024;

#[derive(Clone)]
pub struct Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
}

// 手写 Debug:SK 绝不进日志/panic 信息。
impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("access_key_id", &"<redacted>")
            .field("secret_access_key", &"<redacted>")
            .finish()
    }
}

pub struct S3Client {
    endpoint: Endpoint,
    creds: Credentials,
    region: String,
    agent: ureq::Agent,
}

impl S3Client {
    /// `socks5` = `Some("host:port")` 时全部请求走 SOCKS5 代理。
    ///
    /// `socks5` 解析不了时报错,**不静默退化成直连**——直连若恰好在内网通
    /// (常见情况),后果是备份"成功"但完全绕开了用户明确要求的代理路由,
    /// 界面上还显示"代理已配置",零提示。
    pub fn new(
        endpoint: Endpoint,
        creds: Credentials,
        region: String,
        socks5: Option<&str>,
    ) -> Result<Self, CloudError> {
        Self::new_with_timeout(endpoint, creds, region, socks5, TIMEOUT)
    }

    /// 同 [`new`](Self::new),但超时可调。**仅供测试**(黑洞代理测试要在秒级内
    /// 看到超时,而不是等 60 秒)。`timeout` 同时是整次请求的全局超时与 SOCKS5
    /// 握手的预算上限。
    #[doc(hidden)]
    pub fn new_with_timeout(
        endpoint: Endpoint,
        creds: Credentials,
        region: String,
        socks5: Option<&str>,
        timeout: Duration,
    ) -> Result<Self, CloudError> {
        let mut b = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            // **必须关掉**:409(ForbidOverwrite 命中)与 403(签名/权限)都要
            // 我们自己看状态码分类。留着默认的话它们全变成同一个
            // `Error::StatusCode`,`AlreadyExists` 就认不出来了。
            .http_status_as_error(false)
            // **必须关掉自动跳转**:ureq-proto 对 301/302/303 会把非
            // GET/HEAD 方法悄悄降级成匿名 GET(丢 body、丢
            // Authorization——`redirect_auth_headers` 默认就是
            // `Never`)。PUT 到一个挂了强制 https 跳转的 endpoint 会变成
            // 一次匿名 GET,我们只看状态码,2xx 就误判成写入成功,而对象
            // 存储上其实什么都没多。`max_redirects(0)` 时 ureq 把原始
            // 3xx 响应原样返回(不是 Err)——见 ureq 3.4.2
            // `Config::max_redirects` 文档:"If max_redirects is 0, no
            // redirects are followed and the response is always
            // returned"。我们要的正是这个:能把 Location 报给用户。
            .max_redirects(0);
        if let Some(p) = socks5 {
            // `socks5h`:目标域名交给代理解析(自带连接器按此发 ATYP 0x03)。
            let proxy = ureq::Proxy::new(&format!("socks5h://{p}"))
                .map_err(|_| CloudError::Config("SOCKS5 代理地址解析失败".into()))?;
            b = b.proxy(Some(proxy));
        }
        // 连接器链照抄 ureq `DefaultConnector`,只把 `SocksConnector` 换成自带的
        // (F363):SOCKS5 → HTTP CONNECT → TCP → rustls。
        // `ConnectProxyConnector` 必须留着:未显式配 SOCKS5 时,ureq 的 Config 默认
        // 读 ALL_PROXY/HTTPS_PROXY/HTTP_PROXY/NO_PROXY,去掉它环境变量里的 HTTP
        // 代理就会被静默忽略(变成直连)。NO_PROXY 由两个代理连接器各自照 ureq 原样处理。
        let connector = ()
            .chain(Socks5Connector {
                handshake_timeout: timeout,
            })
            .chain(ConnectProxyConnector::default())
            .chain(TcpConnector::default())
            .chain(RustlsConnector::default());
        Ok(Self {
            endpoint,
            creds,
            region,
            agent: ureq::Agent::with_parts(b.build(), connector, DefaultResolver::default()),
        })
    }

    /// 写一个对象,**要求服务端在对象已存在时拒绝**。
    ///
    /// `amz_date` 由调用方注入(本 crate 不持时钟,同 `mullion-store` 的姿态:
    /// 持了时钟就没法写确定性测试)。
    #[cfg(feature = "write")]
    pub fn put_no_overwrite(
        &self,
        key: &str,
        body: &[u8],
        amz_date: &str,
        skip_if_none_match: bool,
    ) -> Result<(), CloudError> {
        let t = self.endpoint.target(key);
        let payload = sha256_hex(body);
        // 阿里云 OSS 认 `x-oss-forbid-overwrite`;S3/R2/MinIO 认
        // `If-None-Match: *`。**默认两个都发** —— 不认识的那个本应被忽略,
        // 漏发任一个都意味着在对应的服务端上完全没有并发保护(且零报错)。
        //
        // F282:上面这条「不认识就忽略」的假设对 OSS 是事实错误 —— OSS 对
        // PUT 上的 `If-None-Match` 回 400 NotImplemented,不是忽略。调用方
        // 一旦从 `IfNoneMatchRejected` 学到这一点,就会把 `skip_if_none_match`
        // 拧成 true,之后只发 `x-oss-forbid-overwrite`,防覆盖不缩水。
        let extra: Vec<(&str, &str)> = if skip_if_none_match {
            vec![("x-oss-forbid-overwrite", "true")]
        } else {
            vec![("if-none-match", "*"), ("x-oss-forbid-overwrite", "true")]
        };
        let auth = sigv4::authorization(
            &sigv4::Request {
                method: "PUT",
                host: &t.host,
                path: &t.path,
                query: "",
                payload_sha256: &payload,
                amz_date,
                extra_headers: &extra,
            },
            &self.creds.access_key_id,
            &self.creds.secret_access_key,
            &self.region,
            "s3",
        );
        let mut req = self
            .agent
            .put(&t.url)
            .header("authorization", &auth)
            .header("x-amz-date", amz_date)
            .header("x-amz-content-sha256", &payload);
        for (k, v) in extra {
            req = req.header(k, v);
        }
        let resp = req.send(body).map_err(transport)?;
        let code = resp.status().as_u16();
        if (200..300).contains(&code) {
            return Ok(());
        }
        // 409 = OSS 的 FileAlreadyExists;412 = S3/R2 对 If-None-Match 的
        // Precondition Failed。**两个都要认** —— 只认一个的话,在另一族服务端
        // 上「有人抢先推了」会被报成普通失败,重试逻辑不会启动。
        if code == 409 || code == 412 {
            return Err(CloudError::AlreadyExists);
        }
        // 3xx 单独归一档:根因是 endpoint 配置错了(host/scheme,或该换
        // region),用户能自己修——报成一条普通 `Status` 会把唯一可操作的
        // 信息(Location 指向哪)扔掉。
        if (300..400).contains(&code) {
            return Err(redirect_err(code, &resp));
        }
        // F282:OSS 对 PUT 上的 `If-None-Match` 回 400 NotImplemented,正文点名
        // `<Header>If-None-Match</Header>`。**必须认出这个专用形状** ——
        // 混进下面的 `Status` 里,调用方就没法决定「摘掉这个头重试」,只能
        // 把整次上传报失败给用户。用两个子串而不是完整正文匹配:不同版本的
        // OSS 错误 XML 措辞可能有出入,但 `Code` 与被点名的 `Header` 稳定。
        if code == 400 {
            let body = body_text(resp);
            if body.contains("NotImplemented") && body.contains("If-None-Match") {
                return Err(CloudError::IfNoneMatchRejected);
            }
            return Err(CloudError::Status {
                code,
                body: body.chars().take(400).collect(),
            });
        }
        Err(status_err(code, resp))
    }

    /// F346-b:取一个对象的全部字节。**只读**,不在 `write` feature 后面。
    ///
    /// `max_bytes` 是硬上限:超过即 `TooLarge`,不会把整份读进内存。ureq 的
    /// `read_to_vec()` 不带 `.limit()` 时默认 10 MB 上限 —— 必须显式给,否则
    /// 上限由库的默认值决定,而不是调用方。
    pub fn get(&self, key: &str, amz_date: &str, max_bytes: u64) -> Result<Vec<u8>, CloudError> {
        let t = self.endpoint.target(key);
        let payload = sha256_hex(b"");
        let auth = sigv4::authorization(
            &sigv4::Request {
                method: "GET",
                host: &t.host,
                path: &t.path,
                query: "",
                payload_sha256: &payload,
                amz_date,
                extra_headers: &[],
            },
            &self.creds.access_key_id,
            &self.creds.secret_access_key,
            &self.region,
            "s3",
        );
        let mut resp = self
            .agent
            .get(&t.url)
            .header("authorization", &auth)
            .header("x-amz-date", amz_date)
            .header("x-amz-content-sha256", &payload)
            .call()
            .map_err(transport)?;
        let code = resp.status().as_u16();
        if (300..400).contains(&code) {
            return Err(redirect_err(code, &resp));
        }
        if !(200..300).contains(&code) {
            return Err(status_err(code, resp));
        }
        // ureq 的 LimitReader 在额度用尽后**再被读一次**就报超限(哪怕下一次读到的
        // 是 EOF),所以 `.limit(max_bytes)` 会把「恰好等于上限」的对象也拒掉。
        // 多放 1 字节额度,读完再自己比长度:内存上界仍是 max_bytes + 1。
        let v = match resp
            .body_mut()
            .with_config()
            .limit(max_bytes.saturating_add(1))
            .read_to_vec()
        {
            Ok(v) => v,
            Err(ureq::Error::BodyExceedsLimit(_)) => {
                return Err(CloudError::TooLarge { limit: max_bytes });
            }
            Err(e) => return Err(transport(e)),
        };
        if v.len() as u64 > max_bytes {
            return Err(CloudError::TooLarge { limit: max_bytes });
        }
        Ok(v)
    }

    /// 列出 `prefix` 下的全部键(跟完续页令牌)。
    pub fn list_keys(&self, prefix: &str, amz_date: &str) -> Result<Vec<String>, CloudError> {
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut params: Vec<(&str, &str)> = vec![
                ("list-type", "2"),
                ("prefix", prefix),
                ("max-keys", MAX_KEYS),
            ];
            if let Some(t) = &token {
                params.push(("continuation-token", t));
            }
            let query = query_string(&params);
            // 列举的目标是 bucket 根,不是某个对象 —— key 传空串。
            let t = self.endpoint.target("");
            let payload = sha256_hex(b"");
            let auth = sigv4::authorization(
                &sigv4::Request {
                    method: "GET",
                    host: &t.host,
                    path: &t.path,
                    query: &query,
                    payload_sha256: &payload,
                    amz_date,
                    extra_headers: &[],
                },
                &self.creds.access_key_id,
                &self.creds.secret_access_key,
                &self.region,
                "s3",
            );
            let resp = self
                .agent
                .get(&format!("{}?{}", t.url, query))
                .header("authorization", &auth)
                .header("x-amz-date", amz_date)
                .header("x-amz-content-sha256", &payload)
                .call()
                .map_err(transport)?;
            let code = resp.status().as_u16();
            if (300..400).contains(&code) {
                return Err(redirect_err(code, &resp));
            }
            if !(200..300).contains(&code) {
                return Err(status_err(code, resp));
            }
            let text = read_limited(resp, MAX_LIST_PAGE_BYTES)?;
            let page = xml::parse_list(&text)?;
            if out.len().saturating_add(page.keys.len()) > MAX_LIST_KEYS {
                return Err(CloudError::TooManyKeys {
                    limit: MAX_LIST_KEYS,
                });
            }
            out.extend(page.keys);
            match page.next_token {
                Some(t) => token = Some(t),
                None => return Ok(out),
            }
        }
        Err(CloudError::Malformed(format!(
            "续页超过 {MAX_PAGES} 次仍未结束 —— 服务端可能在重复同一个令牌"
        )))
    }
}

fn transport(e: ureq::Error) -> CloudError {
    CloudError::Transport(e.to_string())
}

/// 读正文,最多 `max` 字节,超限报 `TooLarge`。做法同 `get`:多放 1 字节额度
/// 再比长度(ureq 的 LimitReader 恰好用尽后再读一次会报 BodyExceedsLimit)。
fn read_limited(
    mut resp: ureq::http::Response<ureq::Body>,
    max: u64,
) -> Result<String, CloudError> {
    let v = match resp
        .body_mut()
        .with_config()
        .limit(max.saturating_add(1))
        .read_to_vec()
    {
        Ok(v) => v,
        Err(ureq::Error::BodyExceedsLimit(_)) => return Err(CloudError::TooLarge { limit: max }),
        Err(e) => return Err(transport(e)),
    };
    if v.len() as u64 > max {
        return Err(CloudError::TooLarge { limit: max });
    }
    Ok(String::from_utf8_lossy(&v).into_owned())
}

/// 错误页正文:仅供诊断,超限/读失败都当空串(不让错误路径再出新错误)。
fn body_text(resp: ureq::http::Response<ureq::Body>) -> String {
    read_limited(resp, MAX_ERR_BODY_BYTES).unwrap_or_default()
}

fn status_err(code: u16, resp: ureq::http::Response<ureq::Body>) -> CloudError {
    let body = body_text(resp);
    CloudError::Status {
        code,
        // 截断:错误正文会进日志,而对象存储偶尔会回几 KB 的 HTML 错误页。
        body: body.chars().take(400).collect(),
    }
}

/// 3xx 归一成 `Config`,带上状态码与 `Location`——这一族的根因是
/// endpoint 填错了(host/scheme 或该换 region),是用户能自己修的配置
/// 问题;只报状态码等于把唯一可操作的信息(该往哪儿改)扔掉。
///
/// **只读 header,不读 body**:调用方传的是 `&resp` 引用,body 留给别处
/// (这里其实没有别处会再读,但保持「只消费一次 body」的形状,免得以后
/// 有人在这之后又想读 body 却发现已经被吃掉)。
fn redirect_err(code: u16, resp: &ureq::http::Response<ureq::Body>) -> CloudError {
    let location = resp
        .headers()
        .get("location")
        .map(|v| v.to_str().unwrap_or("(无法解析的 Location)").to_string())
        .unwrap_or_else(|| "(没有 Location 头)".to_string());
    CloudError::Config(format!(
        "服务端返回 {code} 重定向到 {location} —— 多半是 endpoint 的 host/scheme 填错了,\
         或者 bucket 在另一个 region;SigV4 签名不会跟着重定向自动生效,需要改配置"
    ))
}
