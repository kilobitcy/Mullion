//! secrets.enc 的 AEAD:XChaCha20-Poly1305,24 字节随机 nonce 前置于密文。
//! 只做「字节进字节出」,不碰文件系统。

use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};

use crate::error::StoreError;

const NONCE_LEN: usize = 24;

/// 用 32 字节密钥加密;输出 = 24 字节 nonce ‖ 密文(含 16 字节 tag)。
pub fn encrypt(key: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>, StoreError> {
    let cipher = XChaCha20Poly1305::new(Key::from_slice(key));
    let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng); // 24 字节,每条消息唯一
    let ct = cipher
        .encrypt(&nonce, plaintext)
        .map_err(|_| StoreError::Crypto)?;
    let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
    out.extend_from_slice(nonce.as_slice());
    out.extend_from_slice(&ct);
    Ok(out)
}

/// 解密 `encrypt` 产出的 blob。密钥错/被篡改/过短 → `StoreError::Crypto`。
pub fn decrypt(key: &[u8; 32], blob: &[u8]) -> Result<Vec<u8>, StoreError> {
    if blob.len() < NONCE_LEN {
        return Err(StoreError::Crypto);
    }
    let (nonce_bytes, ct) = blob.split_at(NONCE_LEN);
    let cipher = XChaCha20Poly1305::new(Key::from_slice(key));
    let nonce = XNonce::from_slice(nonce_bytes);
    cipher.decrypt(nonce, ct).map_err(|_| StoreError::Crypto)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_then_decrypt_round_trips() {
        let key = [7u8; 32];
        let msg = b"hunter2 the secret";
        let blob = encrypt(&key, msg).unwrap();
        // 只比明文长度那段(去掉 24 nonce 前缀、16 tag 后缀),等长比较才有意义:
        // 若拿 &blob[24..](含 tag,长度必不同)比 msg,assert_ne 只因长度不等而恒真。
        assert_ne!(&blob[24..24 + msg.len()], &msg[..], "密文不得等于明文");
        let back = decrypt(&key, &blob).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn wrong_key_fails_not_panics() {
        let blob = encrypt(&[1u8; 32], b"x").unwrap();
        assert!(matches!(
            decrypt(&[2u8; 32], &blob),
            Err(StoreError::Crypto)
        ));
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let key = [3u8; 32];
        let mut blob = encrypt(&key, b"abcdef").unwrap();
        let last = blob.len() - 1;
        blob[last] ^= 0xff;
        assert!(matches!(decrypt(&key, &blob), Err(StoreError::Crypto)));
    }

    #[test]
    fn short_blob_is_crypto_error() {
        assert!(matches!(
            decrypt(&[0u8; 32], &[0u8; 10]),
            Err(StoreError::Crypto)
        ));
    }
}
