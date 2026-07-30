//! Vault:唯一碰文件系统的地方。sessions.toml(明文非敏感)+ secrets.enc(加密敏感)。
//! 两文件各自 tmp+rename 原子写;时间戳由调用方注入(store 不持时钟)。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::crypto;
use crate::error::StoreError;
use crate::group::GroupRecord;
use crate::master_key::MasterKeySource;
use crate::migrate::{migrate_v1, SchemaProbe};
use crate::model::{
    AppearancePrefs, Auth, Connection, GroupId, Identity, SecretEntry, SessionId, SessionRecord,
    SessionsFile, TerminalPrefs, CURRENT_SCHEMA,
};

/// id.to_string() → 敏感条目。
type SecretMap = BTreeMap<String, SecretEntry>;

pub struct Vault {
    dir: PathBuf,
    groups: Vec<GroupRecord>,
    sessions: Vec<SessionRecord>,
    secrets: SecretMap,
    key: [u8; 32],
}

/// 新建/编辑会话的输入(不含 id/modified_at,由 vault 分配/注入)。
pub struct SessionDraft {
    pub identity: Identity,
    pub connection: Connection,
    pub auth: Auth,
    pub terminal: TerminalPrefs,
    pub appearance: AppearancePrefs,
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
        let (groups, sessions, migrated) = load_sessions(&sessions_path)?;

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

        let vault = Self {
            dir,
            groups,
            sessions,
            secrets,
            key,
        };
        if migrated {
            // 立即写回 v2,避免下次打开重复迁移并覆盖掉备份。
            vault.save()?;
        }
        Ok(vault)
    }

    /// 落盘:两文件各自原子写。
    pub fn save(&self) -> Result<(), StoreError> {
        let file = SessionsFile {
            schema_version: CURRENT_SCHEMA,
            group: self.groups.clone(),
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
            modified_at: now_rfc3339.to_string(),
            identity: draft.identity,
            connection: draft.connection,
            auth: draft.auth,
            terminal: draft.terminal,
            appearance: draft.appearance,
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
        rec.identity = draft.identity;
        rec.connection = draft.connection;
        rec.auth = draft.auth;
        rec.terminal = draft.terminal;
        rec.appearance = draft.appearance;
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

    pub fn groups(&self) -> &[GroupRecord] {
        &self.groups
    }

    /// 取可变引用以编辑分组的非 id 字段(名称/标签/继承偏好)。
    ///
    /// **不要修改返回值的 `id` 字段**——那会产生重复 id,且让所有原本指向
    /// 旧 id 的会话 `group_id` 变成悬空引用(`resolve_for` 会静默按「无分组」
    /// 降级处理,不会报错提醒)。
    pub fn group_mut(&mut self, id: GroupId) -> Option<&mut GroupRecord> {
        self.groups.iter_mut().find(|g| g.id == id)
    }

    /// 新增分组。id 取现有 max+1(空库从 1 起)。
    ///
    /// **`GroupId` 会被复用,不是永不重现的稳定标识符**:分配基于*当前剩余*
    /// 记录的 max 计算,若删掉的恰好是当前最大 id,下一次分配会拿到刚删掉的
    /// 那个值(例如 `[1, 2]` 删 `2` 后再 `add_group` → 拿到 `2`;但 `[1, 2, 3]`
    /// 删中间的 `2` 后下一个是 `4`,不复用)。不要拿 `GroupId` 做撤销栈、
    /// 外部持久引用等需要「旧 id 永不重现」语义的用途。
    pub fn add_group(&mut self, name: String) -> GroupId {
        let id = GroupId(
            self.groups
                .iter()
                .map(|g| g.id.0)
                .max()
                .map_or(1, |m| m + 1),
        );
        self.groups.push(GroupRecord {
            id,
            name,
            tags: Vec::new(),
            terminal: TerminalPrefs::default(),
            appearance: AppearancePrefs::default(),
        });
        id
    }

    /// 删除分组。归属该组的会话**不删除**,只把 `group_id` 置 `None`
    /// ——分组是组织手段,不是会话的所有者。
    pub fn delete_group(&mut self, id: GroupId) -> Result<(), StoreError> {
        let before = self.groups.len();
        self.groups.retain(|g| g.id != id);
        if self.groups.len() == before {
            return Err(StoreError::GroupNotFound(id));
        }
        for s in &mut self.sessions {
            if s.identity.group_id == Some(id) {
                s.identity.group_id = None;
            }
        }
        Ok(())
    }

    /// 沿 `[会话, 分组]` 层序解析出最终配置。
    ///
    /// 结果应由调用方缓存,**不要在渲染热路径 / 每帧里重新调用**(本项目陷阱 T3:
    /// 喂数据和重绘没解耦 → 每秒几千次重绘,GPU 空转、风扇起飞)。
    ///
    /// 若会话的 `group_id` 指向一个已不存在的分组(悬空引用,例如分组被删除
    /// 后会话记录未同步、或数据被手改),本函数**不报错、不 panic**,而是静默
    /// 按「无分组」处理,回落到内置默认值——分组数据的问题不该拖垮会话本身。
    /// 本 crate 不接 `log`,排查这类问题目前唯一的线索就是这段文档。
    pub fn resolve_for(&self, id: SessionId) -> Result<crate::inherit::ResolvedConfig, StoreError> {
        let s = self.get(id).ok_or(StoreError::NotFound(id))?;
        let g = s
            .identity
            .group_id
            .and_then(|gid| self.groups.iter().find(|g| g.id == gid));
        Ok(match g {
            Some(g) => crate::inherit::resolve(&[s as &dyn crate::inherit::PrefsLayer, g]),
            None => crate::inherit::resolve(&[s as &dyn crate::inherit::PrefsLayer]),
        })
    }

    #[cfg(test)]
    pub(crate) fn secrets_keys_for_test(&self) -> Vec<String> {
        self.secrets.keys().cloned().collect()
    }
}

/// 读 `sessions.toml` 并按 schema 版本决定:直读 / 迁移 / 拒绝。
/// 返回 `(分组, 会话, 是否发生了迁移)`;文件不存在时返回空 vec、`migrated=false`。
fn load_sessions(
    sessions_path: &Path,
) -> Result<(Vec<GroupRecord>, Vec<SessionRecord>, bool), StoreError> {
    if !sessions_path.exists() {
        return Ok((Vec::new(), Vec::new(), false));
    }
    let text = fs::read_to_string(sessions_path)?;
    let probe: SchemaProbe = toml::from_str(&text)?;
    if probe.schema_version > CURRENT_SCHEMA {
        // 更新版客户端写的文件。宁可打不开,也不能按旧结构解析后
        // 用 save() 把新字段整体抹掉。
        return Err(StoreError::UnsupportedSchema(probe.schema_version));
    }
    if probe.schema_version < CURRENT_SCHEMA {
        // 迁移前留备份:这是唯一会改写用户既有数据的地方。
        // 备份必须在 migrate_v1 之前做 —— 迁移失败时用户手里
        // 还得有一份原始数据。
        // 会覆盖用户目录里可能已存在的旧 .bak(上次迁移失败重试留下的);
        // 刻意接受,因为两者同源于当前这份 v1 文件,覆盖是幂等的。
        fs::copy(sessions_path, sessions_path.with_extension("toml.bak"))?;
        // migrate_v1 已产出语义正确的 StoreError::Migration,直接 `?` 透传,
        // 不再在这里额外包一层 —— 避免文案被二次格式化后自相矛盾。
        let file = migrate_v1(&text)?;
        Ok((file.group, file.session, true))
    } else {
        let file: SessionsFile = toml::from_str(&text)?;
        Ok((file.group, file.session, false))
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
            identity: Identity {
                name: name.into(),
                note: String::new(),
                group_id: None,
                tags: Vec::new(),
            },
            connection: Connection {
                host: "h".into(),
                port: 22,
                protocol: Protocol::Ssh,
            },
            auth: Auth {
                user: "u".into(),
                kind: AuthKind::Password,
            },
            terminal: TerminalPrefs::default(),
            appearance: AppearancePrefs::default(),
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
        d.connection.host = "newhost".into();
        vault.update(id, d, "2026-07-25T09:00:00Z").unwrap();
        let rec = vault.get(id).unwrap();
        assert_eq!(rec.identity.name, "a-renamed");
        assert_eq!(rec.connection.host, "newhost");
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
        d.auth.kind = AuthKind::PublicKey {
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
        assert_eq!(reopened.get(id).unwrap().identity.name, "a");
        assert_eq!(
            reopened.secret(id).unwrap().password.as_deref(),
            Some("secretpw")
        );
    }

    const V1_ON_DISK: &str = r#"
[[session]]
id = 1
name = "old"
host = "h"
port = 22
protocol = "ssh"
user = "u"
note = ""
modified_at = "2026-07-25T00:00:00Z"

[session.auth]
kind = "password"
"#;

    #[test]
    fn open_migrates_v1_file_and_writes_backup() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("sessions.toml"), V1_ON_DISK).unwrap();

        let vault = Vault::open(dir.path().to_path_buf(), &key()).unwrap();

        assert_eq!(vault.list().len(), 1, "迁移后会话应仍在");
        assert_eq!(vault.list()[0].identity.name, "old");
        assert!(
            dir.path().join("sessions.toml.bak").exists(),
            "迁移前必须留备份"
        );
        let bak = std::fs::read_to_string(dir.path().join("sessions.toml.bak")).unwrap();
        assert!(bak.contains("name = \"old\""), "备份应是原始 v1 内容");

        let now = std::fs::read_to_string(dir.path().join("sessions.toml")).unwrap();
        assert!(now.contains("schema_version = 2"), "磁盘上应已是 v2");
    }

    #[test]
    fn opening_v2_file_does_not_create_backup() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
            v.add(draft_pw("a", "p"), "t");
            v.save().unwrap();
        }
        std::fs::remove_file(dir.path().join("sessions.toml.bak")).ok();
        let _v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        assert!(
            !dir.path().join("sessions.toml.bak").exists(),
            "已是 v2 不应重复备份"
        );
    }

    #[test]
    fn opening_future_schema_is_rejected_not_silently_mangled() {
        // 用更新版客户端写出的文件被旧客户端打开:必须明确报错,
        // 绝不能当成 v1 去迁移(那会用旧结构覆盖掉新数据)。
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("sessions.toml"), "schema_version = 99\n").unwrap();

        let Err(err) = Vault::open(dir.path().to_path_buf(), &key()) else {
            panic!("更新版 schema 必须被拒绝,不能静默按旧结构解析");
        };
        assert!(
            matches!(err, StoreError::UnsupportedSchema(99)),
            "应报未支持的 schema 版本"
        );
        assert!(
            !dir.path().join("sessions.toml.bak").exists(),
            "拒绝打开时不应动用户文件"
        );
    }

    #[test]
    fn add_group_allocates_incrementing_ids() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let g1 = v.add_group("生产".into());
        let g2 = v.add_group("测试".into());
        assert_eq!(g1, GroupId(1));
        assert_eq!(g2, GroupId(2));
        assert_eq!(v.groups().len(), 2);
    }

    #[test]
    fn delete_group_detaches_sessions_but_keeps_them() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let g = v.add_group("生产".into());
        let mut d = draft_pw("a", "p");
        d.identity.group_id = Some(g);
        let sid = v.add(d, "t");

        v.delete_group(g).unwrap();

        assert!(v.groups().is_empty());
        assert!(v.get(sid).is_some(), "删分组绝不能级联删会话");
        assert!(
            v.get(sid).unwrap().identity.group_id.is_none(),
            "归属该组的会话 group_id 应置 None"
        );
    }

    #[test]
    fn delete_missing_group_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        assert!(v.delete_group(GroupId(99)).is_err());
    }

    #[test]
    fn resolve_for_uses_group_layer_when_attached() {
        use crate::inherit::DEFAULT_SCROLLBACK;
        let dir = tempfile::tempdir().unwrap();
        let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let g = v.add_group("生产".into());
        v.group_mut(g).unwrap().terminal.scrollback = Some(50_000);
        v.group_mut(g).unwrap().tags.push("prod".into());

        let mut d = draft_pw("a", "p");
        d.identity.group_id = Some(g);
        d.identity.tags.push("web01".into());
        let sid = v.add(d, "t");

        let cfg = v.resolve_for(sid).unwrap();
        assert_eq!(cfg.scrollback, 50_000, "应取分组值");
        assert_eq!(cfg.tags, vec!["prod".to_string(), "web01".to_string()]);

        // 未分组会话回落内置默认
        let sid2 = v.add(draft_pw("b", "p"), "t");
        assert_eq!(v.resolve_for(sid2).unwrap().scrollback, DEFAULT_SCROLLBACK);
    }

    #[test]
    fn groups_survive_save_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let g;
        {
            let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
            g = v.add_group("生产".into());
            v.group_mut(g).unwrap().terminal.scrollback = Some(1234);
            v.save().unwrap();
        }
        let v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        assert_eq!(v.groups().len(), 1);
        assert_eq!(v.groups()[0].terminal.scrollback, Some(1234));
    }

    #[test]
    fn migration_failure_is_reported_as_migration_not_generic_parse_error() {
        // 结构坏掉的 v1(缺 host)迁移失败时,错误要说清「这是迁移失败」,
        // 而不是笼统的「TOML 解析失败(文件可能被手改坏)」——后者会
        // 把用户引去查语法,实际是版本/结构问题。
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("sessions.toml"),
            "[[session]]\nid = 1\nname = \"x\"\n",
        )
        .unwrap();

        let Err(err) = Vault::open(dir.path().to_path_buf(), &key()) else {
            panic!("结构损坏的 v1 迁移必须失败,不能静默成功");
        };
        assert!(matches!(err, StoreError::Migration(_)), "应报迁移失败");
        let msg = err.to_string();
        assert!(msg.contains("迁移"), "错误文案应点明迁移:{msg}");
        assert!(
            !msg.contains("手改坏"),
            "迁移失败不该把用户引去查语法,应是单层语义自洽的迁移错误:{msg}"
        );
        assert!(
            dir.path().join("sessions.toml.bak").exists(),
            "迁移失败也要保住备份 —— 用户数据不能只剩一份坏的"
        );
    }
}
