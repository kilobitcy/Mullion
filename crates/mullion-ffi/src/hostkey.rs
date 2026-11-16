//! F350:手机端 TOFU。
//!
//! 表就是 vault 目录下的 `known_hosts.toml`(包里带来的桌面记录 + 手机上确认过的)。
//! - 一致 → 放行;
//! - 不一致 → **硬拒**,没有「仍然连接」;
//! - 没见过 → 拒绝,并把待确认的 `(host, algo, 指纹)` 记下来。调用方据此弹窗,
//!   用户确认后经 [`trust_host_key`] 落盘再重拨。
//!
//! 不在握手里挂起等用户回答:那要跨语言的异步回调,收益只是少拨一次号。

use std::path::Path;
use std::sync::Mutex;

use mullion_ssh::known_hosts::{
    Fingerprint, HostKeyDecision, HostKeyFuture, HostKeyOutcome, HostKeyPolicy,
};
use mullion_store::{HostKeyEntry, KnownHostsFile};

use crate::error::FfiError;

/// 一条待用户确认的主机密钥。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct PendingHostKey {
    /// TOFU 表的键(`host_key_id`,非默认端口写成 `[host]:port`),也是弹窗上的「主机」。
    pub host: String,
    pub algo: String,
    /// `SHA256:<base64>`。
    pub fingerprint: String,
}

pub(crate) struct PhonePolicy {
    known: KnownHostsFile,
    pending: Mutex<Option<PendingHostKey>>,
}

impl PhonePolicy {
    pub(crate) fn load(vault_dir: &Path) -> Self {
        Self {
            known: KnownHostsFile::load(vault_dir),
            pending: Mutex::new(None),
        }
    }

    /// 表在但读不出来。此时「没记录」不等于「没见过」:可能正是记过、如今被换掉的那台,
    /// 走首次确认就把「变了只能硬拒」绕过去了。调用方须整体拒拨。
    pub(crate) fn is_corrupt(&self) -> bool {
        self.known.is_corrupt()
    }

    /// 这次拨号是不是卡在一台没见过的主机上。
    pub(crate) fn take_pending(&self) -> Option<PendingHostKey> {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }
}

impl HostKeyPolicy for PhonePolicy {
    fn decide<'a>(
        &'a self,
        host_id: &'a str,
        algo: &'a str,
        fp: &'a Fingerprint,
    ) -> HostKeyFuture<'a> {
        let text = fp.to_ssh_string();
        let decision = match self.known.get(host_id) {
            Some(e) if e.fingerprint == text => HostKeyDecision::Accept,
            Some(e) => HostKeyDecision::Reject(HostKeyOutcome::Changed {
                host: host_id.to_owned(),
                expected: Fingerprint::parse_ssh(&e.fingerprint)
                    .unwrap_or_else(|| Fingerprint(Vec::new())),
                got: fp.clone(),
            }),
            // 表坏了:不留待确认项,绝不让它变成首次确认弹窗。
            None if self.known.is_corrupt() => HostKeyDecision::Reject(HostKeyOutcome::Unknown {
                host: host_id.to_owned(),
                got: fp.clone(),
            }),
            None => {
                *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = Some(PendingHostKey {
                    host: host_id.to_owned(),
                    algo: algo.to_owned(),
                    fingerprint: text,
                });
                HostKeyDecision::Reject(HostKeyOutcome::Unknown {
                    host: host_id.to_owned(),
                    got: fp.clone(),
                })
            }
        };
        Box::pin(std::future::ready(decision))
    }
}

pub(crate) const CORRUPT: &str = "已知主机表损坏,为防主机密钥被偷换,拒绝连接;请重新导入";

/// 用户在弹窗里确认了一台**没见过**的主机:写进手机本地 `known_hosts.toml`。
///
/// 表里已有**不同**指纹时拒绝:那是「主机密钥变了」,只能硬拒,不能借首次确认的
/// 入口悄悄覆盖掉。
#[uniffi::export]
pub fn trust_host_key(vault_dir: String, key: PendingHostKey) -> Result<(), FfiError> {
    let mut known = KnownHostsFile::load(Path::new(&vault_dir));
    if known.is_corrupt() {
        return Err(FfiError::Storage(CORRUPT.into()));
    }
    match known.get(&key.host) {
        Some(e) if e.fingerprint == key.fingerprint => return Ok(()),
        Some(_) => {
            return Err(FfiError::Connect(format!(
                "{} 的主机密钥与记录不一致,拒绝覆盖",
                key.host
            )))
        }
        None => {}
    }
    known.record(
        &key.host,
        HostKeyEntry {
            algo: key.algo,
            fingerprint: key.fingerprint,
        },
    );
    known.save().map_err(|e| FfiError::Storage(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(b: u8) -> Fingerprint {
        Fingerprint(vec![b; 32])
    }

    fn decide(p: &PhonePolicy, host: &str, f: &Fingerprint) -> HostKeyDecision {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(p.decide(host, "ssh-ed25519", f))
    }

    fn trust(dir: &Path, host: &str, f: &Fingerprint) -> Result<(), FfiError> {
        trust_host_key(
            dir.to_string_lossy().into_owned(),
            PendingHostKey {
                host: host.into(),
                algo: "ssh-ed25519".into(),
                fingerprint: f.to_ssh_string(),
            },
        )
    }

    #[test]
    fn unknown_is_rejected_and_remembered_then_trusted_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let p = PhonePolicy::load(dir.path());
        assert!(matches!(
            decide(&p, "h", &fp(1)),
            HostKeyDecision::Reject(HostKeyOutcome::Unknown { .. })
        ));
        let pending = p.take_pending().expect("未知主机要留下待确认项");
        assert_eq!(pending.host, "h");
        assert_eq!(pending.fingerprint, fp(1).to_ssh_string());
        assert_eq!(p.take_pending(), None, "读即清");

        trust_host_key(dir.path().to_string_lossy().into_owned(), pending).unwrap();
        let p = PhonePolicy::load(dir.path());
        assert!(matches!(decide(&p, "h", &fp(1)), HostKeyDecision::Accept));
        assert_eq!(p.take_pending(), None);
    }

    #[test]
    fn a_changed_key_is_rejected_hard_and_never_becomes_pending() {
        let dir = tempfile::tempdir().unwrap();
        trust(dir.path(), "h", &fp(1)).unwrap();
        let p = PhonePolicy::load(dir.path());
        assert!(matches!(
            decide(&p, "h", &fp(2)),
            HostKeyDecision::Reject(HostKeyOutcome::Changed { .. })
        ));
        assert_eq!(p.take_pending(), None, "变了不能走首次确认弹窗");
    }

    #[test]
    fn a_corrupt_table_never_turns_into_a_first_time_prompt() {
        let dir = tempfile::tempdir().unwrap();
        trust(dir.path(), "h", &fp(1)).unwrap();
        std::fs::write(dir.path().join("known_hosts.toml"), "不是 toml [[[").unwrap();
        let p = PhonePolicy::load(dir.path());
        assert!(p.is_corrupt());
        // 记过的 h 换了密钥:读不出表也不能落到「首次确认」。
        assert!(matches!(
            decide(&p, "h", &fp(2)),
            HostKeyDecision::Reject(_)
        ));
        assert_eq!(p.take_pending(), None);
        assert!(
            trust(dir.path(), "h", &fp(2)).is_err(),
            "坏表上不许写入新指纹"
        );
    }

    #[test]
    fn trusting_never_overwrites_a_different_recorded_key() {
        let dir = tempfile::tempdir().unwrap();
        trust(dir.path(), "h", &fp(1)).unwrap();
        assert!(trust(dir.path(), "h", &fp(2)).is_err());
        trust(dir.path(), "h", &fp(1)).expect("同一指纹重复确认是幂等的");
        let p = PhonePolicy::load(dir.path());
        assert!(matches!(decide(&p, "h", &fp(1)), HostKeyDecision::Accept));
    }
}
