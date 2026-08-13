//! 主密码 → 主密钥的派生(F71,设计 §2 D5)。纯函数,零 IO。
//!
//! 只有一件事:`derive_key(密码, 盐, 参数) -> [u8; 32]`。盐和参数**都由调用方
//! 传进来**,因为它们来自 `secrets.enc` 的文件头(见 [`crate::secrets_file`])
//! 而不是本模块的常量 —— 参数写死在代码里的话,哪天调参,所有老文件都解不开,
//! 而症状是「主密码突然不对了」,用户会以为自己记错了密码。

use argon2::{Algorithm, Argon2, Params, Version};

use crate::error::StoreError;

/// 盐长度。16 字节 = Argon2 规范的推荐值,也是 `password-hash` 的默认。
pub const SALT_LEN: usize = 16;

/// 派生出的主密钥长度,与 `crypto::encrypt` 要的一致。
pub const KEY_LEN: usize = 32;

/// Argon2id 的三个代价参数。**随密文存**,不随代码走。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KdfParams {
    /// 内存代价,单位 KiB。
    pub m_cost: u32,
    /// 迭代次数。
    pub t_cost: u32,
    /// 并行度。
    pub p_cost: u32,
}

impl Default for KdfParams {
    /// RustCrypto 跟随的 OWASP 当前建议值(19 MiB / t=2 / p=1)。
    ///
    /// 不自己调参:调参要在**目标机器**上实测,而目标机器是用户的 Windows,
    /// 不是开发机。19 MiB 的内存峰值对桌面应用可忽略。
    fn default() -> Self {
        Self {
            m_cost: Params::DEFAULT_M_COST,
            t_cost: Params::DEFAULT_T_COST,
            p_cost: Params::DEFAULT_P_COST,
        }
    }
}

/// 由主密码与盐派生 32 字节主密钥。
///
/// 空盐**报错而不是照算**:Argon2 自己也会拒(盐有最小长度),但那条错误信息
/// 是库的英文内部消息;更要紧的是空盐意味着调用方读文件头时读出了空值,
/// 那是一个应该在这里就停住的损坏信号,不该带着走到密钥里去。
pub fn derive_key(
    password: &str,
    salt: &[u8],
    params: KdfParams,
) -> Result<[u8; KEY_LEN], StoreError> {
    if salt.is_empty() {
        return Err(StoreError::Kdf("盐为空".into()));
    }
    let p = Params::new(params.m_cost, params.t_cost, params.p_cost, Some(KEY_LEN))
        .map_err(|e| StoreError::Kdf(e.to_string()))?;
    let a = Argon2::new(Algorithm::Argon2id, Version::V0x13, p);
    let mut out = [0u8; KEY_LEN];
    a.hash_password_into(password.as_bytes(), salt, &mut out)
        .map_err(|e| StoreError::Kdf(e.to_string()))?;
    Ok(out)
}

/// 生成一条新盐。
///
/// 熵源复用 `chacha20poly1305` 的 `OsRng`(内部走 `getrandom`),理由与
/// `master_key.rs` 里那句一样:免引 `rand_core`/`getrandom` 的直接依赖。
/// 取 32 字节密钥的前 16 字节 —— 截断均匀随机串仍是均匀随机串。
pub fn random_salt() -> [u8; SALT_LEN] {
    use chacha20poly1305::aead::{KeyInit, OsRng};
    let key = chacha20poly1305::XChaCha20Poly1305::generate_key(&mut OsRng);
    let mut salt = [0u8; SALT_LEN];
    salt.copy_from_slice(&key[..SALT_LEN]);
    salt
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用的轻量参数。默认的 19 MiB × 十几条测试会让 `cargo test` 明显变慢,
    /// 而本模块要证的性质(确定性、盐/密码/参数都进了派生)与代价大小无关。
    fn fast() -> KdfParams {
        KdfParams {
            m_cost: 8,
            t_cost: 1,
            p_cost: 1,
        }
    }

    #[test]
    fn same_password_and_salt_derive_the_same_key() {
        let salt = [3u8; SALT_LEN];
        let a = derive_key("hunter2", &salt, fast()).unwrap();
        let b = derive_key("hunter2", &salt, fast()).unwrap();
        assert_eq!(a, b, "同口令同盐必须派生出同一把密钥,否则重开就解不开了");
    }

    /// spec F71 点名的验收项:盐必须真的参与派生。
    #[test]
    fn changing_the_salt_changes_the_key() {
        let a = derive_key("hunter2", &[1u8; SALT_LEN], fast()).unwrap();
        let b = derive_key("hunter2", &[2u8; SALT_LEN], fast()).unwrap();
        assert_ne!(a, b, "改盐必须改密钥 —— 否则盐等于没存");
    }

    #[test]
    fn changing_the_password_changes_the_key() {
        let salt = [3u8; SALT_LEN];
        let a = derive_key("hunter2", &salt, fast()).unwrap();
        let b = derive_key("hunter3", &salt, fast()).unwrap();
        assert_ne!(a, b);
    }

    /// 参数是**随文件走**的,所以它必须真的影响派生结果 —— 否则「参数进文件头」
    /// 这件事就是装饰,将来调参会静默地让老文件解不开(症状:密码突然不对)。
    #[test]
    fn params_are_carried_not_ignored_so_old_files_still_open() {
        let salt = [3u8; SALT_LEN];
        let a = derive_key("hunter2", &salt, fast()).unwrap();
        let b = derive_key(
            "hunter2",
            &salt,
            KdfParams {
                t_cost: 2,
                ..fast()
            },
        )
        .unwrap();
        assert_ne!(a, b, "改 t_cost 必须改结果,否则参数没进派生");
    }

    #[test]
    fn an_empty_salt_is_rejected_not_silently_accepted() {
        assert!(matches!(
            derive_key("hunter2", &[], fast()),
            Err(StoreError::Kdf(_))
        ));
    }

    #[test]
    fn illegal_params_are_an_error_not_a_panic() {
        let bad = KdfParams {
            m_cost: 0,
            t_cost: 0,
            p_cost: 0,
        };
        assert!(matches!(
            derive_key("x", &[1u8; SALT_LEN], bad),
            Err(StoreError::Kdf(_))
        ));
    }

    #[test]
    fn two_salts_are_not_the_same() {
        assert_ne!(
            random_salt(),
            random_salt(),
            "盐必须每次不同 —— 相同就说明取的不是随机源"
        );
    }

    /// 默认参数就是 argon2 crate 的默认(OWASP 建议),不是我们拍的数。
    /// 这条钉住的是「有人为了让测试快一点把默认值调小」——那会静默削弱
    /// 所有真实用户的口令强度。
    #[test]
    fn the_default_params_are_the_upstream_recommendation() {
        let d = KdfParams::default();
        assert_eq!(d.m_cost, Params::DEFAULT_M_COST);
        assert_eq!(d.t_cost, Params::DEFAULT_T_COST);
        assert_eq!(d.p_cost, Params::DEFAULT_P_COST);
        assert!(d.m_cost >= 19 * 1024, "内存代价不得低于 19 MiB");
    }
}
