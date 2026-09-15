//! `mullion-cloud` 的错误类型。**不用 thiserror** —— 本 crate 的依赖清单越短
//! 越好(它是 exe 体积与交叉编译风险的直接来源),手写一个 Display 够用。

use std::fmt;

#[derive(Debug)]
pub enum CloudError {
    /// 目标对象已经存在(`ForbidOverwrite` 生效)。**必须与别的错分开** ——
    /// 调用方要靠它决定「换个序号重试」而不是「报失败给用户」。
    AlreadyExists,
    /// 服务端回了非 2xx。带上状态码与响应正文的前若干字节:对象存储的报错
    /// 几乎全在正文的 `<Code>` 里(`SignatureDoesNotMatch` / `AccessDenied` /
    /// `NoSuchBucket`),只报状态码等于把唯一有用的那条信息扔了。
    Status { code: u16, body: String },
    /// 网络层(连不上 / 超时 / TLS 握手失败)。
    Transport(String),
    /// 响应解析不出来。
    Malformed(String),
    /// 配置本身不合法(endpoint 为空、bucket 名非法…),还没发请求就停住。
    Config(String),
}

impl fmt::Display for CloudError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CloudError::AlreadyExists => write!(f, "目标对象已存在"),
            CloudError::Status { code, body } => write!(f, "服务端返回 {code}:{body}"),
            CloudError::Transport(e) => write!(f, "网络错误:{e}"),
            CloudError::Malformed(e) => write!(f, "响应解析失败:{e}"),
            CloudError::Config(e) => write!(f, "配置不合法:{e}"),
        }
    }
}

impl std::error::Error for CloudError {}
