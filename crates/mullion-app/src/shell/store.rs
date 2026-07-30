//! config-dir 解析 + `SessionStore`:app 侧对 `mullion_store::Vault` 的薄封装,
//! 额外提供「取会话 → 解密 secret → 映射成 SshConfig」的一步到位方法(供双击连接用)。
//! 时间戳由调用方(A2b 用 `time` crate)注入,保持本层可确定性测试。

use std::path::PathBuf;

use mullion_ssh::config::SshConfig;
use mullion_store::{MasterKeySource, SessionDraft, SessionId, SessionRecord, StoreError, Vault};

use super::session_map::{to_ssh_config, MapError};

/// mullion 的配置目录(Windows `%APPDATA%\mullion\`、Linux `~/.config/mullion/`)。
/// 无法确定时返回 None(极少见,如无 HOME)。
pub fn config_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "mullion").map(|d| d.config_dir().to_path_buf())
}

/// 打开会话保险库的错误。
#[derive(Debug)]
pub enum StoreOpenError {
    Store(StoreError),
    Map(MapError),
    NotFound(SessionId),
}

impl std::fmt::Display for StoreOpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreOpenError::Store(e) => write!(f, "{e}"),
            StoreOpenError::Map(e) => write!(f, "{e}"),
            StoreOpenError::NotFound(id) => write!(f, "会话不存在:{id:?}"),
        }
    }
}
impl std::error::Error for StoreOpenError {}
impl From<StoreError> for StoreOpenError {
    fn from(e: StoreError) -> Self {
        StoreOpenError::Store(e)
    }
}
impl From<MapError> for StoreOpenError {
    fn from(e: MapError) -> Self {
        StoreOpenError::Map(e)
    }
}

/// app 侧会话存储:薄封装 Vault,增加 `ssh_config_for`。
pub struct SessionStore {
    vault: Vault,
}

impl SessionStore {
    pub fn open(dir: PathBuf, key: &dyn MasterKeySource) -> Result<Self, StoreError> {
        Ok(Self {
            vault: Vault::open(dir, key)?,
        })
    }

    pub fn list(&self) -> &[SessionRecord] {
        self.vault.list()
    }

    pub fn add(&mut self, draft: SessionDraft, now_rfc3339: &str) -> SessionId {
        self.vault.add(draft, now_rfc3339)
    }

    pub fn update(
        &mut self,
        id: SessionId,
        draft: SessionDraft,
        now_rfc3339: &str,
    ) -> Result<(), StoreError> {
        self.vault.update(id, draft, now_rfc3339)
    }

    pub fn delete(&mut self, id: SessionId) -> Result<(), StoreError> {
        self.vault.delete(id)
    }

    pub fn save(&self) -> Result<(), StoreError> {
        self.vault.save()
    }

    /// 取会话 → 用其(已解密的)secret 组 SshConfig(双击连接用)。
    pub fn ssh_config_for(&self, id: SessionId) -> Result<SshConfig, StoreOpenError> {
        let rec = self.vault.get(id).ok_or(StoreOpenError::NotFound(id))?;
        let secret = self.vault.secret(id);
        Ok(to_ssh_config(rec, secret)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_ssh::config::AuthMethod;
    use mullion_store::{
        Auth, AuthKind, Connection, Identity, InMemoryKey, Protocol, SecretEntry, SessionDraft,
    };

    fn draft() -> SessionDraft {
        SessionDraft {
            identity: Identity {
                name: "dev".into(),
                note: String::new(),
                group_id: None,
                tags: Vec::new(),
            },
            connection: Connection {
                host: "192.0.2.10".into(),
                port: 22,
                protocol: Protocol::Ssh,
            },
            auth: Auth {
                user: "user".into(),
                kind: AuthKind::Password,
            },
            terminal: Default::default(),
            appearance: Default::default(),
            secret: Some(SecretEntry {
                password: Some("pw".into()),
                passphrase: None,
            }),
        }
    }

    #[test]
    fn open_add_then_ssh_config_for() {
        let dir = tempfile::tempdir().unwrap();
        let mut store =
            SessionStore::open(dir.path().to_path_buf(), &InMemoryKey([1u8; 32])).unwrap();
        let id = store.add(draft(), "2026-07-26T00:00:00Z");
        store.save().unwrap();
        assert_eq!(store.list().len(), 1);
        // 组连接参数:解密 secret + 映射
        let cfg = store.ssh_config_for(id).unwrap();
        assert_eq!(cfg.host, "192.0.2.10");
        assert!(matches!(cfg.auth, AuthMethod::Password(p) if p == "pw"));
    }

    #[test]
    fn ssh_config_for_missing_id_errors() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path().to_path_buf(), &InMemoryKey([1u8; 32])).unwrap();
        assert!(store.ssh_config_for(mullion_store::SessionId(999)).is_err());
    }
}
