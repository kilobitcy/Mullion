//! 主密钥来源。真实实现走 OS keyring;测试用内存实现,让加密逻辑在无头 CI 可测。

use crate::error::StoreError;

/// 取 32 字节主密钥;不存在则生成并持久化。
pub trait MasterKeySource {
    fn load_or_create(&self) -> Result<[u8; 32], StoreError>;
}

/// 生产实现:主密钥存 OS keyring(Windows DPAPI / macOS Keychain / Linux Secret Service)。
/// Android 上不存在(F345):那里 keyring 会静默回落到内存库。
#[cfg(not(target_os = "android"))]
pub struct KeyringSource {
    pub service: String,
    pub account: String,
}

#[cfg(not(target_os = "android"))]
impl KeyringSource {
    /// 默认 service=`mullion`、account=`vault-master-key`。
    pub fn new() -> Self {
        Self {
            service: "mullion".into(),
            account: "vault-master-key".into(),
        }
    }
}

#[cfg(not(target_os = "android"))]
impl Default for KeyringSource {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(not(target_os = "android"))]
impl MasterKeySource for KeyringSource {
    fn load_or_create(&self) -> Result<[u8; 32], StoreError> {
        use chacha20poly1305::aead::{KeyInit, OsRng};
        use chacha20poly1305::XChaCha20Poly1305;

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

/// 固定密钥,不碰 keyring。**测试与 Android 生产路径共用**(F346-a):Android 的
/// 数据密钥由 Keystore 信封解出后经 ffi 传进来。不要把它当测试辅助 `#[cfg(test)]` 掉。
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
