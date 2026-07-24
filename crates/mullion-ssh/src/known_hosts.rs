//! TOFU 主机密钥校验(F3):首次记录指纹,变更时拦截。
//!
//! **红线**:`verify` 绝不无条件返回 true。指纹不匹配、或主机未记录过,
//! 一律返回 `false`——放行等于关掉 SSH 的全部身份保证。

use std::collections::HashMap;

/// 主机密钥指纹。骨架用原始字节表示(真实为公钥的哈希摘要)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint(pub Vec<u8>);

/// TOFU 已知主机表:host → 记录过的指纹。
#[derive(Debug, Default)]
pub struct KnownHosts {
    entries: HashMap<String, Fingerprint>,
}

impl KnownHosts {
    pub fn new() -> Self {
        Self::default()
    }

    /// 首次见到 `host` 时记录其指纹(TOFU)。已存在则覆盖(仅在上层确认变更后调用)。
    pub fn record(&mut self, host: &str, fp: Fingerprint) {
        self.entries.insert(host.to_owned(), fp);
    }

    /// 校验 `host` 的指纹。
    ///
    /// 只有「已记录过且指纹完全一致」才返回 `true`。指纹不匹配(主机密钥变更)、
    /// 或主机从未记录过,都返回 `false`——由上层走 TOFU 首次确认/变更拦截流程。
    pub fn verify(&self, host: &str, fp: &Fingerprint) -> bool {
        match self.entries.get(host) {
            Some(known) => known == fp,
            None => false,
        }
    }
}

impl Fingerprint {
    /// 从 SSH 公钥算 SHA-256 指纹(用 ssh_key 内置,不引 sha2)。
    pub fn from_public_key(key: &russh::keys::ssh_key::PublicKey) -> Self {
        let f = key.fingerprint(russh::keys::ssh_key::HashAlg::Sha256);
        Fingerprint(f.as_bytes().to_vec())
    }
}

impl KnownHosts {
    /// 查已记录的指纹(不改状态)。
    pub fn get(&self, host: &str) -> Option<&Fingerprint> {
        self.entries.get(host)
    }
}

/// 主机密钥被拒时的精确原因,供 connect 映射成 ConnectError。
#[derive(Debug, Clone)]
pub enum HostKeyOutcome {
    /// 记录过但指纹不一致(疑似 MITM)。
    Changed {
        host: String,
        expected: Fingerprint,
        got: Fingerprint,
    },
    /// 从未记录,策略不自动信任。
    Unknown { host: String, got: Fingerprint },
}

/// check_server_key 的决策结果。
#[derive(Debug)]
pub enum HostKeyDecision {
    Accept,
    Reject(HostKeyOutcome),
}

/// 主机密钥策略。ssh 不弹 UI —— app 注入实现(弹窗版),测试/首版注入 TofuAccept。
pub trait HostKeyPolicy: Send + Sync {
    fn decide(&self, host: &str, fp: &Fingerprint) -> HostKeyDecision;
}

/// TOFU 策略:未记录→记录并放行;一致→放行;不一致→拒(Changed)。
/// 冒烟/hermetic 测试/首版默认用它。app 弹窗版另做,未知时返回 Reject(Unknown)。
pub struct TofuAccept {
    known: std::sync::Arc<std::sync::Mutex<KnownHosts>>,
}

impl TofuAccept {
    pub fn new(known: std::sync::Arc<std::sync::Mutex<KnownHosts>>) -> Self {
        Self { known }
    }
}

impl HostKeyPolicy for TofuAccept {
    fn decide(&self, host: &str, fp: &Fingerprint) -> HostKeyDecision {
        let mut kh = self.known.lock().expect("known-hosts poisoned");
        match kh.get(host).cloned() {
            None => {
                kh.record(host, fp.clone());
                HostKeyDecision::Accept
            }
            Some(known) if &known == fp => HostKeyDecision::Accept,
            Some(known) => HostKeyDecision::Reject(HostKeyOutcome::Changed {
                host: host.to_owned(),
                expected: known,
                got: fp.clone(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(bytes: &[u8]) -> Fingerprint {
        Fingerprint(bytes.to_vec())
    }

    #[test]
    fn mismatched_fingerprint_is_rejected() {
        // F3:主机密钥变更(指纹不匹配)必须被拦截,连接失败。
        let mut kh = KnownHosts::new();
        kh.record("host.example", fp(b"AAAA"));
        assert!(
            !kh.verify("host.example", &fp(b"BBBB")),
            "指纹变更必须返回 false"
        );
    }

    #[test]
    fn matching_fingerprint_is_accepted() {
        let mut kh = KnownHosts::new();
        kh.record("host.example", fp(b"AAAA"));
        assert!(kh.verify("host.example", &fp(b"AAAA")));
    }

    #[test]
    fn unknown_host_is_rejected() {
        // 未记录过的主机绝不默认放行,交上层走 TOFU 首次确认。
        let kh = KnownHosts::new();
        assert!(!kh.verify("stranger.example", &fp(b"AAAA")));
    }

    #[test]
    fn fingerprint_from_key_is_deterministic_and_distinguishes_keys() {
        // 同一把私钥的公钥 → 指纹稳定;不同私钥 → 指纹不同。
        let k1 = russh::keys::load_secret_key("tests/fixtures/client_key", None).unwrap();
        let k2 = russh::keys::load_secret_key("tests/fixtures/other_key", None).unwrap();
        let fp1a = Fingerprint::from_public_key(k1.public_key());
        let fp1b = Fingerprint::from_public_key(k1.public_key());
        let fp2 = Fingerprint::from_public_key(k2.public_key());
        assert_eq!(fp1a, fp1b, "同一公钥指纹必须稳定");
        assert_ne!(fp1a, fp2, "不同公钥指纹必须不同");
        assert_eq!(fp1a.0.len(), 32, "SHA-256 应为 32 字节");
    }

    #[test]
    fn tofu_records_unknown_then_accepts_same_rejects_changed() {
        // F3:未知主机首次记录并放行;同指纹再来放行;指纹变更 → Reject(Changed)。
        let known = std::sync::Arc::new(std::sync::Mutex::new(KnownHosts::new()));
        let policy = TofuAccept::new(known);
        let a = fp(b"AAAA");
        let b = fp(b"BBBB");
        assert!(
            matches!(policy.decide("h", &a), HostKeyDecision::Accept),
            "首次应记录并放行"
        );
        assert!(
            matches!(policy.decide("h", &a), HostKeyDecision::Accept),
            "同指纹应放行"
        );
        match policy.decide("h", &b) {
            HostKeyDecision::Reject(HostKeyOutcome::Changed { expected, got, .. }) => {
                assert_eq!(expected, a);
                assert_eq!(got, b);
            }
            _ => panic!("指纹变更必须 Reject(Changed)(F3 红线)"),
        }
    }
}
