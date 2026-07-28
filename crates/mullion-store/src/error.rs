//! store 的错误。手写 Display + std::error::Error,匹配项目既有 `ConnectError` 风格(不引 thiserror)。

use std::fmt;

use crate::model::SessionId;

#[derive(Debug)]
pub enum StoreError {
    /// 文件读写失败。
    Io(String),
    /// TOML 序列化失败。
    TomlSer(String),
    /// TOML 解析失败(文件被手改坏)。
    TomlDe(String),
    /// 加解密失败:密钥错误或密文被篡改/损坏。
    Crypto,
    /// keyring 里的主密钥长度非 32。
    CorruptKey,
    /// OS keyring 访问失败(缺 Secret Service 等)。
    Keyring(String),
    /// secrets.enc 解密后非法 UTF-8。
    Utf8,
    /// 目标会话不存在。
    NotFound(SessionId),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Io(e) => write!(f, "文件读写失败:{e}"),
            StoreError::TomlSer(e) => write!(f, "TOML 序列化失败:{e}"),
            StoreError::TomlDe(e) => write!(f, "TOML 解析失败(文件可能被手改坏):{e}"),
            StoreError::Crypto => write!(f, "加解密失败 —— 密钥错误或密文损坏"),
            StoreError::CorruptKey => write!(f, "主密钥损坏(长度非 32 字节)"),
            StoreError::Keyring(e) => {
                write!(f, "系统密钥库访问失败:{e} —— 检查 keyring/Secret Service")
            }
            StoreError::Utf8 => write!(f, "secrets.enc 解密后非法 UTF-8"),
            StoreError::NotFound(id) => write!(f, "会话不存在:{id:?}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        StoreError::Io(e.to_string())
    }
}
impl From<toml::ser::Error> for StoreError {
    fn from(e: toml::ser::Error) -> Self {
        StoreError::TomlSer(e.to_string())
    }
}
impl From<toml::de::Error> for StoreError {
    fn from(e: toml::de::Error) -> Self {
        StoreError::TomlDe(e.to_string())
    }
}
impl From<std::string::FromUtf8Error> for StoreError {
    fn from(_: std::string::FromUtf8Error) -> Self {
        StoreError::Utf8
    }
}
impl From<keyring::Error> for StoreError {
    fn from(e: keyring::Error) -> Self {
        StoreError::Keyring(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crypto_and_io_have_distinct_messages() {
        let a = StoreError::Crypto.to_string();
        let b = StoreError::Io("x".into()).to_string();
        assert!(!a.is_empty() && !b.is_empty());
        assert_ne!(a, b, "不同错误消息必须可区分");
    }
}
