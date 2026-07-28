//! Vault:唯一碰文件系统的地方。sessions.toml(明文非敏感)+ secrets.enc(加密敏感)。
//! 两文件各自 tmp+rename 原子写;时间戳由调用方注入(store 不持时钟)。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::crypto;
use crate::error::StoreError;
use crate::master_key::MasterKeySource;
use crate::model::{SecretEntry, SessionId, SessionRecord, SessionsFile};

/// id.to_string() → 敏感条目。
type SecretMap = BTreeMap<String, SecretEntry>;

pub struct Vault {
    dir: PathBuf,
    sessions: Vec<SessionRecord>,
    secrets: SecretMap,
    key: [u8; 32],
}

/// 新建/编辑会话的输入(不含 id/modified_at,由 vault 分配/注入)。
pub struct SessionDraft {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub protocol: crate::model::Protocol,
    pub user: String,
    pub note: String,
    pub auth: crate::model::AuthKind,
    /// 敏感部分(密码/口令);无则 None。
    pub secret: Option<SecretEntry>,
}

impl Vault {
    fn sessions_path(&self) -> PathBuf {
        self.dir.join("sessions.toml")
    }
    fn secrets_path(&self) -> PathBuf {
        self.dir.join("secrets.enc")
    }

    /// 打开(或初始化)`dir` 下的保险库。dir 由调用方(app)算好(directories)传入。
    pub fn open(dir: PathBuf, key_source: &dyn MasterKeySource) -> Result<Self, StoreError> {
        fs::create_dir_all(&dir)?;
        let key = key_source.load_or_create()?;

        let sessions_path = dir.join("sessions.toml");
        let sessions = if sessions_path.exists() {
            let text = fs::read_to_string(&sessions_path)?;
            let file: SessionsFile = toml::from_str(&text)?;
            file.session
        } else {
            Vec::new()
        };

        let secrets_path = dir.join("secrets.enc");
        let mut secrets = if secrets_path.exists() {
            let blob = fs::read(&secrets_path)?;
            let plain = crypto::decrypt(&key, &blob)?;
            let text = String::from_utf8(plain)?;
            toml::from_str::<SecretMap>(&text)?
        } else {
            SecretMap::new()
        };

        // 裁剪孤儿密文:sessions.toml 可能被手改或在两文件写入之间崩溃残留旧 id
        // (spec §3.2「load 容忍 desync」)。不裁的话,后续 add() 用 max+1 复用旧 id
        // 会静默继承无关会话的密文。
        let live: std::collections::BTreeSet<String> =
            sessions.iter().map(|s| s.id.0.to_string()).collect();
        secrets.retain(|k, _| live.contains(k));

        Ok(Self {
            dir,
            sessions,
            secrets,
            key,
        })
    }

    /// 落盘:两文件各自原子写。
    pub fn save(&self) -> Result<(), StoreError> {
        let file = SessionsFile {
            session: self.sessions.clone(),
        };
        let toml_text = toml::to_string_pretty(&file)?;
        write_atomic(&self.sessions_path(), toml_text.as_bytes())?;

        let secret_text = toml::to_string_pretty(&self.secrets)?;
        let blob = crypto::encrypt(&self.key, secret_text.as_bytes())?;
        write_atomic(&self.secrets_path(), &blob)?;
        Ok(())
    }

    pub fn list(&self) -> &[SessionRecord] {
        &self.sessions
    }

    pub fn get(&self, id: SessionId) -> Option<&SessionRecord> {
        self.sessions.iter().find(|s| s.id == id)
    }

    pub fn secret(&self, id: SessionId) -> Option<&SecretEntry> {
        self.secrets.get(&id.0.to_string())
    }

    /// 新增会话。id 取现有 max+1(空库从 1 起);modified_at 由调用方注入。
    pub fn add(&mut self, draft: SessionDraft, now_rfc3339: &str) -> SessionId {
        let id = SessionId(
            self.sessions
                .iter()
                .map(|s| s.id.0)
                .max()
                .map_or(1, |m| m + 1),
        );
        match draft.secret {
            Some(sec) => {
                self.secrets.insert(id.0.to_string(), sec);
            }
            None => {
                self.secrets.remove(&id.0.to_string());
            }
        }
        self.sessions.push(SessionRecord {
            id,
            name: draft.name,
            host: draft.host,
            port: draft.port,
            protocol: draft.protocol,
            user: draft.user,
            note: draft.note,
            modified_at: now_rfc3339.to_string(),
            auth: draft.auth,
        });
        id
    }

    /// 编辑现有会话:替换非敏感字段、重打时间戳、覆盖敏感部分。
    pub fn update(
        &mut self,
        id: SessionId,
        draft: SessionDraft,
        now_rfc3339: &str,
    ) -> Result<(), StoreError> {
        let rec = self
            .sessions
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or(StoreError::NotFound(id))?;
        rec.name = draft.name;
        rec.host = draft.host;
        rec.port = draft.port;
        rec.protocol = draft.protocol;
        rec.user = draft.user;
        rec.note = draft.note;
        rec.auth = draft.auth;
        rec.modified_at = now_rfc3339.to_string();
        match draft.secret {
            Some(sec) => {
                self.secrets.insert(id.0.to_string(), sec);
            }
            None => {
                self.secrets.remove(&id.0.to_string());
            }
        }
        Ok(())
    }

    /// 删除会话,并**连带清除**其密文(守 id 完整性,见 spec §3.1)。
    pub fn delete(&mut self, id: SessionId) -> Result<(), StoreError> {
        let before = self.sessions.len();
        self.sessions.retain(|s| s.id != id);
        if self.sessions.len() == before {
            return Err(StoreError::NotFound(id));
        }
        self.secrets.remove(&id.0.to_string());
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn secrets_keys_for_test(&self) -> Vec<String> {
        self.secrets.keys().cloned().collect()
    }
}

/// tmp + rename 原子写:防写到一半崩溃导致两文件 desync。
/// `known_hosts` 模块复用同一实现,故 `pub(crate)`。
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::master_key::InMemoryKey;
    use crate::model::{AuthKind, Protocol, SecretEntry, SessionId};

    fn key() -> InMemoryKey {
        InMemoryKey([5u8; 32])
    }

    fn draft_pw(name: &str, pw: &str) -> SessionDraft {
        SessionDraft {
            name: name.into(),
            host: "h".into(),
            port: 22,
            protocol: Protocol::Ssh,
            user: "u".into(),
            note: String::new(),
            auth: AuthKind::Password,
            secret: Some(SecretEntry {
                password: Some(pw.into()),
                passphrase: None,
            }),
        }
    }

    #[test]
    fn add_allocates_incrementing_ids_and_stamps_time() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let id1 = vault.add(draft_pw("a", "p1"), "2026-07-25T00:00:00Z");
        let id2 = vault.add(draft_pw("b", "p2"), "2026-07-25T00:00:01Z");
        assert_eq!(id1, SessionId(1));
        assert_eq!(id2, SessionId(2));
        assert_eq!(vault.list().len(), 2);
        assert_eq!(vault.get(id1).unwrap().modified_at, "2026-07-25T00:00:00Z");
        assert_eq!(vault.secret(id1).unwrap().password.as_deref(), Some("p1"));
    }

    #[test]
    fn open_empty_dir_has_no_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        assert!(vault.list().is_empty());
    }

    #[test]
    fn save_writes_both_files_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        vault.save().unwrap();
        assert!(dir.path().join("sessions.toml").exists());
        assert!(dir.path().join("secrets.enc").exists());
        assert!(!dir.path().join("sessions.tmp").exists());
        assert!(!dir.path().join("secrets.tmp").exists());
    }

    /// 保存/删除后不变量:secrets 的 key 集合 ⊆ 会话 id 集合(无孤儿密文)。
    fn assert_no_orphan_secrets(vault: &Vault) {
        let ids: std::collections::BTreeSet<String> =
            vault.list().iter().map(|s| s.id.0.to_string()).collect();
        for k in vault.secrets_keys_for_test() {
            assert!(ids.contains(&k), "孤儿密文 id={k}");
        }
    }

    #[test]
    fn update_replaces_fields_and_restamps() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let id = vault.add(draft_pw("a", "p1"), "2026-07-25T00:00:00Z");
        let mut d = draft_pw("a-renamed", "p1-new");
        d.host = "newhost".into();
        vault.update(id, d, "2026-07-25T09:00:00Z").unwrap();
        let rec = vault.get(id).unwrap();
        assert_eq!(rec.name, "a-renamed");
        assert_eq!(rec.host, "newhost");
        assert_eq!(rec.modified_at, "2026-07-25T09:00:00Z");
        assert_eq!(
            vault.secret(id).unwrap().password.as_deref(),
            Some("p1-new")
        );
    }

    #[test]
    fn update_missing_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let r = vault.update(SessionId(99), draft_pw("x", "p"), "t");
        assert!(matches!(r, Err(StoreError::NotFound(SessionId(99)))));
    }

    #[test]
    fn delete_removes_session_and_purges_secret() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let id = vault.add(draft_pw("a", "p1"), "t");
        vault.delete(id).unwrap();
        assert!(vault.get(id).is_none());
        assert!(vault.secret(id).is_none(), "删除必须连带清密文(id 完整性)");
        assert_no_orphan_secrets(&vault);
    }

    #[test]
    fn add_with_no_secret_never_inherits_orphan() {
        // 构造孤儿密文:先建带密码会话并 save,再手动清空 sessions.toml(模拟手改/崩溃残留)
        let dir = tempfile::tempdir().unwrap();
        {
            let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
            v.add(draft_pw("old", "old-secret"), "t");
            v.save().unwrap();
        }
        std::fs::write(dir.path().join("sessions.toml"), "").unwrap(); // 只留孤儿密文
        let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        // open 后孤儿密文应已被裁剪
        assert!(v.secrets_keys_for_test().is_empty(), "open 应裁掉孤儿密文");
        // 新建一个无密钥会话(会复用 id=1),绝不能继承旧密码
        let mut d = draft_pw("new", "x");
        d.auth = AuthKind::PublicKey {
            path: "/k".into(),
            has_passphrase: false,
        };
        d.secret = None;
        let id = v.add(d, "t");
        assert!(v.secret(id).is_none(), "无密钥新会话不得继承孤儿密文");
    }

    #[test]
    fn update_to_no_secret_removes_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let id = v.add(draft_pw("a", "p1"), "t");
        let mut d = draft_pw("a", "ignored");
        d.secret = None;
        v.update(id, d, "t2").unwrap();
        assert!(v.secret(id).is_none(), "update 传 None 应清掉密文");
    }

    #[test]
    fn save_then_reopen_preserves_everything() {
        let dir = tempfile::tempdir().unwrap();
        let id;
        {
            let mut vault = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
            id = vault.add(draft_pw("a", "secretpw"), "2026-07-25T00:00:00Z");
            vault.save().unwrap();
        }
        let reopened = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        assert_eq!(reopened.list().len(), 1);
        assert_eq!(reopened.get(id).unwrap().name, "a");
        assert_eq!(
            reopened.secret(id).unwrap().password.as_deref(),
            Some("secretpw")
        );
    }
}
