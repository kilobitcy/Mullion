//! `secrets.enc` 的**外层封装**(F71,设计 §2 D2~D4)。纯字节进字节出,零 IO。
//!
//! 两种布局:
//!
//! ```text
//! 旧(不设主密码,与今天逐字节同构):
//!   nonce(24) ‖ ct
//!
//! 新(设了主密码):
//!   0   8  magic  b"MULLION\x01"   ← 末字节 = 头版本号
//!   8   1  kdf    0x01 = Argon2id
//!   9   4  m_cost u32 小端(KiB)
//!  13   4  t_cost u32 小端
//!  17   1  p_cost u8
//!  18   1  salt_len
//!  19  16  salt
//!  35  ..  nonce(24) ‖ ct          ← 与旧布局的载荷部分完全一致
//! ```
//!
//! **为什么不设主密码时不写头**(设计 D2):spec F71 要的是「未设主密码时与今日
//! 行为逐字节等价」;更实际的是降级路径 —— 未签名 exe 被退回旧版这事会发生,
//! 旧版读不懂文件头,表现是「所有保存的密码都没了」。绝大多数用户没启用主密码,
//! 不该为它承担这个风险。
//!
//! 代价是格式检测靠魔数试探:旧布局前 24 字节是随机 nonce,恰好撞上这 8 个
//! 魔数字节的概率是 2⁻⁶⁴ —— 与「AEAD tag 被随机碰撞」同量级,而且真撞上了
//! 后续解密也会失败(报错,不是静默读出垃圾)。

use crate::error::StoreError;
use crate::kdf::{KdfParams, SALT_LEN};

/// 新布局的魔数。末字节是**头版本号**,不是字符串的一部分。
const MAGIC: &[u8; 8] = b"MULLION\x01";

/// `kdf` 字节的取值。0x00 留给「无 KDF」,当前**不写出**(那就是旧布局)。
const KDF_ARGON2ID: u8 = 0x01;

/// 新布局的头长度。
const HEADER_LEN: usize = 8 + 1 + 4 + 4 + 1 + 1 + SALT_LEN;

/// `secrets.enc` 用的密钥方案。**由文件自己声明**,不由任何本地配置声明
/// (设计 D1:第二真源会制造「配置说没设密码、文件却是密码加密的」这种
/// 纯粹由二义状态造出来的假故障)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    /// 主密钥来自 OS 钥匙串(今天的默认)。
    Keyring,
    /// 主密钥由主密码经 Argon2id 派生。
    Argon2id {
        params: KdfParams,
        salt: [u8; SALT_LEN],
    },
}

impl Scheme {
    /// UI 要展示「已设定 / 未设定」时用。
    pub fn has_password(&self) -> bool {
        matches!(self, Scheme::Argon2id { .. })
    }
}

/// 拆开一个 `secrets.enc` 的字节串:声明的方案 + 交给 `crypto::decrypt` 的载荷。
pub fn parse(blob: &[u8]) -> Result<(Scheme, &[u8]), StoreError> {
    if !blob.starts_with(MAGIC) {
        // 旧布局:整个 blob 就是载荷。**注意不校验长度** —— 载荷太短是
        // `crypto::decrypt` 的判断,重复判一次只会让两处的判据将来漂开。
        return Ok((Scheme::Keyring, blob));
    }
    if blob.len() < HEADER_LEN {
        return Err(StoreError::CorruptSecrets(format!(
            "文件头被截断:只有 {} 字节,至少要 {HEADER_LEN}",
            blob.len()
        )));
    }
    let kdf = blob[8];
    if kdf != KDF_ARGON2ID {
        // **不能当成旧布局兜底**:那会拿钥匙串密钥去解一个主密码加密的文件,
        // 报出来的是「密文损坏」,把「本版本读不懂这个 KDF」这条真信息弄丢。
        return Err(StoreError::CorruptSecrets(format!(
            "不认识的 KDF 标记 0x{kdf:02x} —— 这个文件可能由更新版本的 Mullion 写出"
        )));
    }
    let m_cost = u32::from_le_bytes([blob[9], blob[10], blob[11], blob[12]]);
    let t_cost = u32::from_le_bytes([blob[13], blob[14], blob[15], blob[16]]);
    let p_cost = u32::from(blob[17]);
    // 盐长**读头里的那个字节**,不是拿 SALT_LEN 当常量:盐长要变时老文件还得能读。
    let salt_len = usize::from(blob[18]);
    if salt_len != SALT_LEN {
        return Err(StoreError::CorruptSecrets(format!(
            "本版本只认 {SALT_LEN} 字节的盐,文件里写的是 {salt_len}"
        )));
    }
    let mut salt = [0u8; SALT_LEN];
    salt.copy_from_slice(&blob[19..19 + SALT_LEN]);
    Ok((
        Scheme::Argon2id {
            params: KdfParams {
                m_cost,
                t_cost,
                p_cost,
            },
            salt,
        },
        &blob[HEADER_LEN..],
    ))
}

/// 把载荷按方案封回去。`Keyring` 就是原样返回(设计 D2)。
pub fn encode(scheme: &Scheme, payload: &[u8]) -> Vec<u8> {
    match scheme {
        Scheme::Keyring => payload.to_vec(),
        Scheme::Argon2id { params, salt } => {
            let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
            out.extend_from_slice(MAGIC);
            out.push(KDF_ARGON2ID);
            out.extend_from_slice(&params.m_cost.to_le_bytes());
            out.extend_from_slice(&params.t_cost.to_le_bytes());
            // p_cost 上限是 0xFFFFFF,一字节存不下全部合法值;但我们只写
            // `KdfParams::default()`(p=1)与用户改不到的值,超界时夹到 255 —— 夹了
            // 也解得开自己写的文件,因为解出来的还是同一个字节。
            out.push(params.p_cost.min(u32::from(u8::MAX)) as u8);
            out.push(SALT_LEN as u8);
            out.extend_from_slice(salt);
            out.extend_from_slice(payload);
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argon() -> Scheme {
        Scheme::Argon2id {
            params: KdfParams {
                m_cost: 4096,
                t_cost: 3,
                p_cost: 2,
            },
            salt: [7u8; SALT_LEN],
        }
    }

    #[test]
    fn a_legacy_blob_is_recognised_as_keyring() {
        // 真实旧文件的开头是随机 nonce;这里拿一段不像魔数的字节代表它。
        let blob = vec![0xABu8; 60];
        let (scheme, payload) = parse(&blob).unwrap();
        assert_eq!(scheme, Scheme::Keyring);
        assert_eq!(
            payload,
            &blob[..],
            "旧布局整个 blob 就是载荷,不许切掉任何前缀"
        );
    }

    #[test]
    fn a_legacy_blob_round_trips_unchanged() {
        let payload = vec![1u8, 2, 3, 4];
        assert_eq!(
            encode(&Scheme::Keyring, &payload),
            payload,
            "不设主密码时字节必须与今天完全一致(设计 D2)"
        );
    }

    #[test]
    fn header_round_trips_every_field() {
        let blob = encode(&argon(), b"payload-here");
        let (scheme, payload) = parse(&blob).unwrap();
        assert_eq!(scheme, argon());
        assert_eq!(payload, b"payload-here");
    }

    /// 载荷部分必须与旧布局逐字节一致 —— `crypto::decrypt` 两条路走同一份代码,
    /// 这里多切/少切一个字节都会表现成「密码不对」。
    #[test]
    fn the_payload_after_the_header_is_byte_identical_to_the_legacy_layout() {
        let payload: Vec<u8> = (0u8..=200).collect();
        let blob = encode(&argon(), &payload);
        assert_eq!(&blob[HEADER_LEN..], &payload[..]);
        assert_eq!(parse(&blob).unwrap().1, &payload[..]);
    }

    #[test]
    fn a_truncated_header_is_an_error_not_a_panic() {
        let blob = encode(&argon(), b"x");
        for cut in MAGIC.len()..HEADER_LEN {
            let r = parse(&blob[..cut]);
            assert!(
                matches!(r, Err(StoreError::CorruptSecrets(_))),
                "截到 {cut} 字节时应报损坏,实际 {r:?}"
            );
        }
    }

    #[test]
    fn an_unknown_kdf_byte_is_rejected_not_treated_as_keyring() {
        let mut blob = encode(&argon(), b"x");
        blob[8] = 0x7f;
        assert!(matches!(parse(&blob), Err(StoreError::CorruptSecrets(_))));
    }

    /// 头版本号是魔数的末字节。它一变,前 8 字节就不再匹配,于是走旧布局分支 ——
    /// 这**不是**bug:更高版本的头会在解密时失败并报错,而不是把老客户端带进
    /// 一个它读不懂的结构里瞎猜。这条钉住的是「别哪天给魔数加个前缀通配」。
    #[test]
    fn a_different_header_version_does_not_match_the_magic() {
        let mut blob = encode(&argon(), b"x");
        blob[7] = 0x02;
        assert_eq!(parse(&blob).unwrap().0, Scheme::Keyring);
    }

    /// 盐长读的是**头里的字节**而不是常量:写死常量的话,一个 `salt_len = 8`
    /// 的头会被照着 16 字节读下去,多读的 8 字节来自载荷 —— 静默解错。
    #[test]
    fn a_salt_length_this_version_does_not_know_is_rejected() {
        let mut blob = encode(&argon(), b"x");
        blob[18] = 8;
        assert!(matches!(parse(&blob), Err(StoreError::CorruptSecrets(_))));
    }

    #[test]
    fn an_empty_blob_is_a_legacy_blob_not_a_panic() {
        assert_eq!(parse(&[]).unwrap(), (Scheme::Keyring, &[][..]));
    }

    #[test]
    fn has_password_distinguishes_the_two_schemes() {
        assert!(!Scheme::Keyring.has_password());
        assert!(argon().has_password());
    }
}
