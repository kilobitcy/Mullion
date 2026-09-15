//! S3 兼容对象存储的两个操作:条件写与列举(F270)。**本 crate 唯一碰网络的地方。**
//!
//! 阻塞式(ureq)。调用方必须在 `spawn_blocking` 里用它 —— 在事件循环里同步
//! 跑网络会把帧率打到零(T3/T7)。

use std::time::Duration;

use crate::error::CloudError;
use crate::sigv4::{self, sha256_hex};
use crate::url::{query_string, Endpoint};
use crate::xml;

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

#[derive(Debug, Clone)]
pub struct Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
}

pub struct S3Client {
    endpoint: Endpoint,
    creds: Credentials,
    region: String,
    agent: ureq::Agent,
}

impl S3Client {
    /// `socks5` = `Some("host:port")` 时全部请求走 SOCKS5 代理。
    pub fn new(
        endpoint: Endpoint,
        creds: Credentials,
        region: String,
        socks5: Option<&str>,
    ) -> Self {
        let mut b = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            // **必须关掉**:409(ForbidOverwrite 命中)与 403(签名/权限)都要
            // 我们自己看状态码分类。留着默认的话它们全变成同一个
            // `Error::StatusCode`,`AlreadyExists` 就认不出来了。
            .http_status_as_error(false);
        if let Some(p) = socks5 {
            if let Ok(proxy) = ureq::Proxy::new(&format!("socks5://{p}")) {
                b = b.proxy(Some(proxy));
            }
        }
        Self {
            endpoint,
            creds,
            region,
            agent: b.build().into(),
        }
    }

    /// 写一个对象,**要求服务端在对象已存在时拒绝**。
    ///
    /// `amz_date` 由调用方注入(本 crate 不持时钟,同 `mullion-store` 的姿态:
    /// 持了时钟就没法写确定性测试)。
    pub fn put_no_overwrite(
        &self,
        key: &str,
        body: &[u8],
        amz_date: &str,
    ) -> Result<(), CloudError> {
        let t = self.endpoint.target(key);
        let payload = sha256_hex(body);
        // 阿里云 OSS 认 `x-oss-forbid-overwrite`;S3/R2/MinIO 认
        // `If-None-Match: *`。**两个都发** —— 不认识的那个会被忽略,而漏发
        // 任一个都意味着在对应的服务端上完全没有并发保护(且零报错)。
        let extra = [
            ("if-none-match", "*"),
            ("x-oss-forbid-overwrite", "true"),
        ];
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
        Err(status_err(code, resp))
    }

    /// 列出 `prefix` 下的全部键(跟完续页令牌)。
    pub fn list_keys(&self, prefix: &str, amz_date: &str) -> Result<Vec<String>, CloudError> {
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut params: Vec<(&str, &str)> =
                vec![("list-type", "2"), ("prefix", prefix), ("max-keys", MAX_KEYS)];
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
            if !(200..300).contains(&code) {
                return Err(status_err(code, resp));
            }
            let text = body_text(resp);
            let page = xml::parse_list(&text)?;
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

fn body_text(mut resp: ureq::http::Response<ureq::Body>) -> String {
    resp.body_mut().read_to_string().unwrap_or_default()
}

fn status_err(code: u16, resp: ureq::http::Response<ureq::Body>) -> CloudError {
    let body = body_text(resp);
    CloudError::Status {
        code,
        // 截断:错误正文会进日志,而对象存储偶尔会回几 KB 的 HTML 错误页。
        body: body.chars().take(400).collect(),
    }
}
