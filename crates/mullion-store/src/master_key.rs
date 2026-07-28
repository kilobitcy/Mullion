//! 主密钥来源。真实实现走 OS keyring;测试用内存实现,让加密逻辑在无头 CI 可测。

use chacha20poly1305::aead::{KeyInit, OsRng};
use chacha20poly1305::XChaCha20Poly1305;

use crate::error::StoreError;

/// 取 32 字节主密钥;不存在则生成并持久化。
pub trait MasterKeySource {
    fn load_or_create(&self) -> Result<[u8; 32], StoreError>;
}

/// 生产实现:主密钥存 OS keyring(Windows DPAPI / macOS Keychain / Linux Secret Service)。
pub struct KeyringSource {
    pub service: String,
    pub account: String,
}

impl KeyringSource {
    /// 默认 service=`mullion`、account=`vault-master-key`。
    pub fn new() -> Self {
        Self {
            service: "mullion".into(),
            account: "vault-master-key".into(),
        }
    }
}

impl Default for KeyringSource {
    fn default() -> Self {
        Self::new()
    }
}

impl MasterKeySource for KeyringSource {
    fn load_or_create(&self) -> Result<[u8; 32], StoreError> {
        let entry = keyring::Entry::new(&self.service, &self.account)?;
        match entry.get_secret() {
            Ok(bytes) => {
                let arr: [u8; 32] = bytes.try_into().map_err(|_| StoreError::CorruptKey)?;
                Ok(arr)
            }
            Err(keyring::Error::NoEntry) => {
                // 复用 cipher 的密钥生成器(内部走 getrandom),免引 rand_core/getrandom 直接依赖。
                let key = XChaCha20Poly1305::generate_key(&mut OsRng);
                let arr: [u8; 32] = key.into();
                entry.set_secret(&arr)?;
                Ok(arr)
            }
            Err(e) => Err(StoreError::Keyring(e.to_string())),
        }
    }
}

/// 测试用:返回固定密钥,不碰 keyring。
pub struct InMemoryKey(pub [u8; 32]);

impl MasterKeySource for InMemoryKey {
    fn load_or_create(&self) -> Result<[u8; 32], StoreError> {
        Ok(self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_returns_fixed_key() {
        let src = InMemoryKey([9u8; 32]);
        assert_eq!(src.load_or_create().unwrap(), [9u8; 32]);
    }
}
