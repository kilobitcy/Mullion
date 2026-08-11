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
use crate::tunnel::{TunnelId, TunnelKind, TunnelRecord};

/// id.to_string() → 敏感条目。
type SecretMap = BTreeMap<String, SecretEntry>;

pub struct Vault {
    dir: PathBuf,
    groups: Vec<GroupRecord>,
    sessions: Vec<SessionRecord>,
    tunnels: Vec<TunnelRecord>,
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
    pub network: crate::network::NetworkPrefs,
    pub automation: crate::automation::AutomationPrefs,
    /// 敏感部分(密码/口令);无则 None。
    pub secret: Option<SecretEntry>,
}

/// 新建/编辑隧道的输入(不含 id,由 vault 分配)。
///
/// 与 `SessionDraft` 同构,理由也相同:id 由 vault 统一分配,调用方造不出
/// 一个「已经带 id」的草稿,也就不会撞号。**没有 `secret` 字段** —— 隧道是
/// 纯引用,凭据一律来自 `session_id` 指向的会话(设计 D2)。
pub struct TunnelDraft {
    pub session_id: SessionId,
    pub listen_port: u16,
    pub note: String,
    pub autostart: bool,
    pub kind: TunnelKind,
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
        let Loaded {
            groups,
            sessions,
            tunnels,
            migrated,
            legacy_key_paths,
        } = load_sessions(&sessions_path)?;

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

        // v<5 迁移:旧文件里公钥会话只存了私钥**路径**,把路径指向的内容读进来
        // 存进加密侧车,路径本身就此丢弃。
        //
        // 读不到(文件被删/挪走/权限不足)时**跳过**,不报错:整个库因为一个私钥
        // 文件不在了就打不开,用户连进去改都改不了。降级后的表现是「该会话没有
        // 私钥」,UI 会红字提示重新导入(见 app 侧 `SecretPresence`)。
        for (id, path) in legacy_key_paths {
            let key = id.0.to_string();
            if !live.contains(&key) || secrets.get(&key).is_some_and(|s| s.private_key.is_some()) {
                continue;
            }
            if let Ok(text) = fs::read_to_string(&path) {
                secrets.entry(key).or_default().private_key = Some(text);
            }
        }

        let vault = Self {
            dir,
            groups,
            sessions,
            tunnels,
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
            tunnel: self.tunnels.clone(),
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
            network: draft.network,
            automation: draft.automation,
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
        rec.network = draft.network;
        rec.automation = draft.automation;
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

    /// 只把会话挪到另一个分组(`None` = 未分组),别的字段一律不动。
    ///
    /// 不走 `update`:那个要一份完整 `SessionDraft`,而调用方(右键菜单)手上
    /// 只有一个 id。为改一个 `group_id` 去凭空重建 draft,漏填任何一个字段都是
    /// 静默把用户的配置改掉。
    ///
    /// **不重打 `modified_at`**:换分组是组织动作,不是内容变更;把它算成「修改」
    /// 会让按修改时间排序/审阅的人看到一堆没改过内容的会话浮到最前面。
    pub fn set_group(&mut self, id: SessionId, group: Option<GroupId>) -> Result<(), StoreError> {
        let rec = self
            .sessions
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or(StoreError::NotFound(id))?;
        rec.identity.group_id = group;
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
            network: crate::network::NetworkPrefs::default(),
            automation: crate::automation::AutomationPrefs::default(),
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

    pub fn tunnels(&self) -> &[TunnelRecord] {
        &self.tunnels
    }

    pub fn tunnel(&self, id: TunnelId) -> Option<&TunnelRecord> {
        self.tunnels.iter().find(|t| t.id == id)
    }

    /// 新增隧道。id 取现有 max+1(空库从 1 起),**与会话号池互不影响**。
    ///
    /// 与 `add` 不同,这里不碰 `secrets`:隧道没有自己的凭据(设计 D2)。
    /// 也不注入 `modified_at` —— 隧道不进「按修改时间排序」的视图,
    /// 存一个没人读的时间戳只会让人以为它有语义。
    pub fn add_tunnel(&mut self, draft: TunnelDraft) -> TunnelId {
        let id = TunnelId(
            self.tunnels
                .iter()
                .map(|t| t.id.0)
                .max()
                .map_or(1, |m| m + 1),
        );
        self.tunnels.push(TunnelRecord {
            id,
            session_id: draft.session_id,
            listen_port: draft.listen_port,
            note: draft.note,
            autostart: draft.autostart,
            kind: draft.kind,
        });
        id
    }

    pub fn update_tunnel(&mut self, id: TunnelId, draft: TunnelDraft) -> Result<(), StoreError> {
        let rec = self
            .tunnels
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or(StoreError::TunnelNotFound(id))?;
        rec.session_id = draft.session_id;
        rec.listen_port = draft.listen_port;
        rec.note = draft.note;
        rec.autostart = draft.autostart;
        rec.kind = draft.kind;
        Ok(())
    }

    pub fn delete_tunnel(&mut self, id: TunnelId) -> Result<(), StoreError> {
        let before = self.tunnels.len();
        self.tunnels.retain(|t| t.id != id);
        if self.tunnels.len() == before {
            return Err(StoreError::TunnelNotFound(id));
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
        Ok(self.resolve_layer(s, s.identity.group_id))
    }

    /// `resolve_for` 的参数化内核:直接吃一层 prefs + 它所属的分组 id,
    /// 不要求该层已经入库 —— F92「测试连接」解析的是尚未保存的草稿。
    /// 悬空 `group_id` 的静默降级语义与 `resolve_for` 完全一致(见上)。
    pub fn resolve_layer(
        &self,
        layer: &dyn crate::inherit::PrefsLayer,
        group_id: Option<crate::model::GroupId>,
    ) -> crate::inherit::ResolvedConfig {
        match group_id.and_then(|gid| self.groups.iter().find(|g| g.id == gid)) {
            Some(g) => crate::inherit::resolve(&[layer, g]),
            None => crate::inherit::resolve(&[layer]),
        }
    }

    /// 展开一条会话的完整跳板链(F5)。返回按拨号顺序排列的**跳板会话记录**。
    ///
    /// 返回记录而非 id:调用方(app)接下来要拿每一跳的 host/user/认证去物化 `Hop`,
    /// 让它再查一遍索引没有意义。
    pub fn expand_jump_chain(&self, id: SessionId) -> Result<Vec<SessionRecord>, StoreError> {
        let (sessions, groups) = self.jump_index();
        if !sessions.contains_key(&id) {
            return Err(StoreError::NotFound(id));
        }
        let ids = crate::jump::expand_chain(id, &sessions, &groups)?;
        Ok(ids.into_iter().map(|i| sessions[&i].clone()).collect())
    }

    /// `expand_jump_chain` 的参数化内核:直接吃一条跳板链(通常来自
    /// `resolve_layer(..).jump`),发起方不必已入库 —— F92 拨的是草稿。
    pub fn expand_jump_chain_of(
        &self,
        chain: &[crate::network::JumpRef],
    ) -> Result<Vec<SessionRecord>, StoreError> {
        let (sessions, groups) = self.jump_index();
        let ids = crate::jump::expand_chain_of(chain, &sessions, &groups)?;
        Ok(ids.into_iter().map(|i| sessions[&i].clone()).collect())
    }

    /// 建两张全量索引。展开跳板要读每个跳板会话自身(含继承)的链,
    /// 只传目标那一条不够。
    fn jump_index(
        &self,
    ) -> (
        std::collections::BTreeMap<SessionId, SessionRecord>,
        std::collections::BTreeMap<crate::model::GroupId, crate::group::GroupRecord>,
    ) {
        (
            self.list().iter().map(|r| (r.id, r.clone())).collect(),
            self.groups().iter().map(|g| (g.id, g.clone())).collect(),
        )
    }

    #[cfg(test)]
    pub(crate) fn secrets_keys_for_test(&self) -> Vec<String> {
        self.secrets.keys().cloned().collect()
    }
}

/// `load_sessions` 的产物。
struct Loaded {
    groups: Vec<GroupRecord>,
    sessions: Vec<SessionRecord>,
    tunnels: Vec<TunnelRecord>,
    /// 版本落后、已就地升级 → 必须立刻 `save()` 写回,否则下次打开重复迁移
    /// 并覆盖掉备份。
    migrated: bool,
    /// v<5 文件里的「会话 id → 私钥路径」。私钥内容的导入必须在
    /// `Vault::open` 里做 —— 那里才同时拿得到 `secrets`(本函数够不着)。
    legacy_key_paths: BTreeMap<SessionId, PathBuf>,
}

/// 读 `sessions.toml` 并按 schema 版本决定:直读 / 迁移 / 拒绝。
/// 文件不存在时返回空库、`migrated=false`。
fn load_sessions(sessions_path: &Path) -> Result<Loaded, StoreError> {
    if !sessions_path.exists() {
        return Ok(Loaded {
            groups: Vec::new(),
            sessions: Vec::new(),
            tunnels: Vec::new(),
            migrated: false,
            legacy_key_paths: BTreeMap::new(),
        });
    }
    let text = fs::read_to_string(sessions_path)?;
    let probe: SchemaProbe = toml::from_str(&text)?;
    if probe.schema_version > CURRENT_SCHEMA {
        // 更新版客户端写的文件。宁可打不开,也不能按旧结构解析后
        // 用 save() 把新字段整体抹掉。
        return Err(StoreError::UnsupportedSchema(probe.schema_version));
    }
    if probe.schema_version < CURRENT_SCHEMA {
        // 升级前留备份:这是唯一会改写用户既有数据的地方。
        // 备份必须在解析/迁移之前做 —— 失败时用户手里还得有一份原始数据。
        // 会覆盖用户目录里可能已存在的旧 .bak(上次迁移失败重试留下的);
        // 刻意接受,因为两者同源于当前这份文件,覆盖是幂等的。
        fs::copy(sessions_path, sessions_path.with_extension("toml.bak"))?;
        // 私钥路径必须在**这里**从原始文本里挑出来:解析成 v5 的 `AuthKind`
        // 之后 `path` 已经被 serde 当未知字段丢掉了,再也捞不回来。
        let legacy_key_paths = crate::migrate::legacy_key_paths(&text);
        let file = if probe.schema_version <= 1 {
            // 真 v1:扁平结构,必须先经 migrate_v1 转成分节结构。
            // migrate_v1 已产出语义正确的 StoreError::Migration,直接 `?` 透传,
            // 不再在这里额外包一层 —— 避免文案被二次格式化后自相矛盾。
            migrate_v1(&text)?
        } else {
            // v2~v4:结构已经和当前一致(分节嵌套),只是版本号落后 ——
            // 新分节(如 network)全带 serde(default) 能直接补齐,
            // 绝不能走 migrate_v1(它只认 v1 的扁平结构,喂 v2 会解析失败)。
            toml::from_str::<SessionsFile>(&text)?
        };
        Ok(Loaded {
            groups: file.group,
            sessions: file.session,
            tunnels: file.tunnel,
            migrated: true,
            legacy_key_paths,
        })
    } else {
        let file: SessionsFile = toml::from_str(&text)?;
        Ok(Loaded {
            groups: file.group,
            sessions: file.session,
            tunnels: file.tunnel,
            migrated: false,
            legacy_key_paths: BTreeMap::new(),
        })
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
            network: crate::network::NetworkPrefs::default(),
            automation: crate::automation::AutomationPrefs::default(),
            secret: Some(SecretEntry {
                password: Some(pw.into()),
                passphrase: None,
                proxy_password: None,
                private_key: None,
            }),
        }
    }

    fn tunnel_draft(session: SessionId, port: u16) -> TunnelDraft {
        TunnelDraft {
            session_id: session,
            listen_port: port,
            note: String::new(),
            autostart: false,
            kind: crate::tunnel::TunnelKind::Local {
                target_host: "db.internal".into(),
                target_port: 3306,
                expose: false,
            },
        }
    }

    /// 隧道 id 与会话 id **各自独立编号**。挤在一个号池里会让「隧道 7」和
    /// 「会话 7」在日志/错误文案里长得一样,排查时误判成同一个对象。
    #[test]
    fn tunnel_ids_are_max_plus_one_and_independent_of_session_ids() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        // 先把会话 id 推到 7,证明隧道编号不受它影响。
        for i in 0..7 {
            vault.add(draft_pw(&format!("s{i}"), "p"), "t");
        }
        assert_eq!(vault.list().last().unwrap().id, SessionId(7));

        let t1 = vault.add_tunnel(tunnel_draft(SessionId(7), 3306));
        let t2 = vault.add_tunnel(tunnel_draft(SessionId(7), 5432));
        assert_eq!(t1, TunnelId(1));
        assert_eq!(t2, TunnelId(2));
    }

    #[test]
    fn tunnels_survive_save_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let id = {
            let mut vault = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
            let sid = vault.add(draft_pw("a", "p1"), "t");
            let tid = vault.add_tunnel(tunnel_draft(sid, 3306));
            vault.save().unwrap();
            tid
        };
        let vault = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        assert_eq!(vault.tunnels().len(), 1);
        let t = vault.tunnel(id).expect("隧道应原样读回");
        assert_eq!(t.listen_port, 3306);
        assert_eq!(
            t.kind,
            crate::tunnel::TunnelKind::Local {
                target_host: "db.internal".into(),
                target_port: 3306,
                expose: false,
            }
        );
    }

    /// 守设计 D2「隧道无独立密文条目」。
    ///
    /// `Vault::open` 里的 `secrets.retain(|k, _| live.contains(k))` 是按
    /// **`SessionId`** 裁剪的。哪天有人给隧道加了密文条目又沿用这套 GC,
    /// 隧道的密文会在每次 open 时被当成孤儿静默删掉。这条钉住当前契约:
    /// 增删隧道一律不碰 secrets。
    #[test]
    fn deleting_a_tunnel_does_not_touch_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let sid = vault.add(draft_pw("a", "p1"), "t");
        let tid = vault.add_tunnel(tunnel_draft(sid, 3306));

        vault.delete_tunnel(tid).unwrap();
        assert!(vault.tunnel(tid).is_none());
        assert_eq!(
            vault.secret(sid).unwrap().password.as_deref(),
            Some("p1"),
            "删隧道不该动会话密文"
        );
        assert!(
            matches!(vault.delete_tunnel(tid), Err(StoreError::TunnelNotFound(_))),
            "删不存在的隧道要报错,不能静默成功"
        );
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

    /// 换分组只动 `group_id`:密码、时间戳、其余字段都不许被顺手改掉
    /// (走查 3 的右键「移动到分组」用它)。
    #[test]
    fn set_group_moves_the_session_without_touching_anything_else() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let id = vault.add(draft_pw("a", "p1"), "2026-07-25T00:00:00Z");
        let gid = vault.add_group("生产".into());

        vault.set_group(id, Some(gid)).unwrap();
        assert_eq!(vault.get(id).unwrap().identity.group_id, Some(gid));
        assert_eq!(
            vault.get(id).unwrap().modified_at,
            "2026-07-25T00:00:00Z",
            "换分组是组织动作,不该重打修改时间"
        );
        assert_eq!(
            vault.secret(id).unwrap().password.as_deref(),
            Some("p1"),
            "换分组不该动密文"
        );
        assert_eq!(vault.get(id).unwrap().identity.name, "a");

        // 移回未分组。
        vault.set_group(id, None).unwrap();
        assert_eq!(vault.get(id).unwrap().identity.group_id, None);

        assert!(matches!(
            vault.set_group(SessionId(999), None),
            Err(StoreError::NotFound(_))
        ));
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
        assert!(
            now.contains(&format!("schema_version = {CURRENT_SCHEMA}")),
            "磁盘上应已升到当前版本"
        );
    }

    /// 真实 v2(已是分节嵌套结构,非 v1 扁平结构)的 `sessions.toml`。
    /// `schema_version = 2` 但结构已经和当前一致,只是缺 `[session.network]`。
    const V2_ON_DISK: &str = r#"
schema_version = 2

[[session]]
id = 1
modified_at = "2026-07-25T00:00:00Z"

[session.identity]
name = "v2sess"

[session.connection]
host = "192.0.2.20"
port = 2222
protocol = "ssh"

[session.auth]
user = "u2"
kind = "password"
"#;

    /// v2 结构已经是嵌套分节,不是 v1 的扁平结构——不能走 `migrate_v1`
    /// (它只认扁平结构,喂给它会因缺 `name`/`host` 等顶层字段而解析失败)。
    /// 升级到 v3 后,`schema_version == 2` 的真实文件必须仍能被
    /// `Vault::open` 直接读出来,只是版本号被带到 CURRENT_SCHEMA。
    #[test]
    fn open_upgrades_real_v2_file_without_going_through_migrate_v1() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("sessions.toml"), V2_ON_DISK).unwrap();

        let vault = Vault::open(dir.path().to_path_buf(), &key())
            .expect("真实 v2 文件必须能被 Vault::open 直接读出来,不能报错");

        assert_eq!(vault.list().len(), 1, "v2 会话不能丢");
        let s = &vault.list()[0];
        assert_eq!(s.identity.name, "v2sess");
        assert_eq!(s.connection.host, "192.0.2.20");
        assert_eq!(s.connection.port, 2222);
        assert_eq!(s.auth.user, "u2");

        assert!(
            dir.path().join("sessions.toml.bak").exists(),
            "升级前必须留备份"
        );
        let bak = std::fs::read_to_string(dir.path().join("sessions.toml.bak")).unwrap();
        assert!(bak.contains("name = \"v2sess\""), "备份应是原始 v2 内容");
        assert!(
            bak.contains("schema_version = 2"),
            "备份应是升级前的原文,而非已升级的内容"
        );

        let now = std::fs::read_to_string(dir.path().join("sessions.toml")).unwrap();
        assert!(
            now.contains(&format!("schema_version = {CURRENT_SCHEMA}")),
            "磁盘上应已升到当前版本"
        );
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

    /// `opening_v2_file_does_not_create_backup` 是按当年 `CURRENT_SCHEMA == 2` 写的,
    /// 钉住「打开已是当前 schema 的文件」这条路径。文件由 `save()` 自己产出,
    /// 所以它天然就是当前版本 —— 版本号再升也不用改这条测试。
    ///
    /// 为什么这条重要:`.bak` 只有一份,如果 `Vault::open` 在文件已经是当前 schema
    /// 时仍然调用 `save()` 重写,就会用「刚打开时的内容」去覆盖 `.bak`,用户唯一的
    /// 升级前快照就没了。这里同时钉住「不产生/不覆盖 `.bak`」与「磁盘字节原样不变」
    /// 两件事,并顺带验证 `automation` 分节在打开过程中没有丢失。
    ///
    /// 构造文件的方式选了「Vault::add + save() → 读回磁盘字节」而不是像
    /// `V3_ON_DISK`/`V2_ON_DISK` 那样手写 TOML 常量:手写常量必须跟实际序列化格式
    /// (字段顺序、`skip_serializing_if` 产生的省略)保持同步,一旦 serde 输出格式变了
    /// 常量就可能悄悄失真;而这条测试关心的正是「打开前后字节要完全相等」,用
    /// 序列化器自己产出的内容做基准,才是最贴近真实用户文件的做法。
    #[test]
    fn opening_a_current_schema_file_does_not_rewrite_or_touch_backup() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
            let mut d = draft();
            d.automation = crate::automation::AutomationPrefs {
                enabled: Some(true),
                tmux: Some(crate::automation::TmuxChoice::Attach {
                    session_name: Some("claude".into()),
                }),
                commands: Some(vec![crate::automation::AutomationCommand {
                    text: "echo 'hi'".into(),
                    delay_ms: Some(500),
                }]),
                work_dir: Some("/srv".into()),
                env: Some(vec![crate::automation::EnvVar {
                    key: "RUST_LOG".into(),
                    value: "debug".into(),
                }]),
                initial_delay_ms: Some(300),
                inter_delay_ms: None,
                ready_timeout_ms: None,
            };
            v.add(d, "2026-08-06T00:00:00Z");
            v.save().unwrap();
        }

        let sessions_path = dir.path().join("sessions.toml");
        let bak_path = dir.path().join("sessions.toml.bak");
        let before = std::fs::read(&sessions_path).unwrap();
        let before_text = String::from_utf8(before.clone()).unwrap();
        assert!(
            before_text.contains(&format!("schema_version = {CURRENT_SCHEMA}")),
            "前提条件:这份文件必须真的是当前版本,否则没测到点子上"
        );
        assert!(
            before_text.contains("[session.automation]"),
            "前提条件:必须含 automation 分节"
        );
        assert!(!bak_path.exists(), "首次 save 不该凭空产生 .bak");

        let vault = Vault::open(dir.path().to_path_buf(), &key())
            .expect("已是 CURRENT_SCHEMA 的文件必须能正常打开");

        assert!(
            !bak_path.exists(),
            ".bak 只有一份;打开已是当前版本的文件绝不该产生/覆盖备份"
        );
        let after = std::fs::read(&sessions_path).unwrap();
        assert_eq!(
            before, after,
            "打开已是当前版本的文件不该重写磁盘字节 —— 否则每次打开都会覆盖用户唯一的升级前快照"
        );

        let a = &vault.list()[0].automation;
        assert_eq!(
            a.tmux,
            Some(crate::automation::TmuxChoice::Attach {
                session_name: Some("claude".into())
            }),
            "automation 分节不能在打开过程中丢失"
        );
        assert_eq!(a.commands.as_ref().unwrap()[0].text, "echo 'hi'");
        assert_eq!(a.commands.as_ref().unwrap()[0].delay_ms, Some(500));
        assert_eq!(a.work_dir.as_deref(), Some("/srv"));
        assert_eq!(a.env.as_ref().unwrap()[0].key, "RUST_LOG");
        assert_eq!(a.env.as_ref().unwrap()[0].value, "debug");
        assert_eq!(a.initial_delay_ms, Some(300));
    }

    /// 真实 v3 文件:结构已含 `[session.network]`,但没有 `[session.automation]`。
    const V3_ON_DISK: &str = r#"
schema_version = 3

[[session]]
id = 1
modified_at = "2026-08-01T00:00:00Z"

[session.identity]
name = "v3sess"

[session.connection]
host = "192.0.2.30"
port = 22
protocol = "ssh"

[session.auth]
user = "u3"
kind = "password"

[session.network]

[session.network.proxy]
kind = "socks5"
host = "127.0.0.1"
port = 7891
"#;

    #[test]
    fn open_upgrades_v3_file_and_adds_empty_automation() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("sessions.toml"), V3_ON_DISK).unwrap();

        let vault =
            Vault::open(dir.path().to_path_buf(), &key()).expect("真实 v3 文件必须能被直接读出来");

        assert_eq!(vault.list().len(), 1, "v3 会话不能丢");
        let s = &vault.list()[0];
        assert_eq!(s.identity.name, "v3sess");
        assert_eq!(s.connection.host, "192.0.2.30");
        assert!(
            matches!(
                s.network.proxy,
                Some(crate::network::ProxyChoice::Socks5(_))
            ),
            "v3 已有的代理配置不能在升级中丢掉"
        );
        assert_eq!(
            s.automation,
            crate::automation::AutomationPrefs::default(),
            "缺 automation 分节应落默认(全继承),迁移不得凭空写值"
        );

        assert!(
            dir.path().join("sessions.toml.bak").exists(),
            "升级前必须留备份"
        );
        let bak = std::fs::read_to_string(dir.path().join("sessions.toml.bak")).unwrap();
        assert!(bak.contains("schema_version = 3"), "备份应是升级前的原文");

        let now = std::fs::read_to_string(dir.path().join("sessions.toml")).unwrap();
        assert!(
            now.contains(&format!("schema_version = {CURRENT_SCHEMA}")),
            "磁盘上应已升到当前版本"
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

    /// 最小合法 draft:不带密钥、不分组。计划文档假定本文件已有同名辅助函数,
    /// 实际只有 `draft_pw`;这里补上,风格与 `draft_pw` 保持一致。
    fn draft() -> SessionDraft {
        SessionDraft {
            identity: Identity {
                name: "a".into(),
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
            network: crate::network::NetworkPrefs::default(),
            automation: crate::automation::AutomationPrefs::default(),
            secret: None,
        }
    }

    #[test]
    fn resolve_for_carries_network_from_group() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let gid = v.add_group("生产".into());
        v.group_mut(gid).unwrap().network = crate::network::NetworkPrefs {
            proxy: Some(crate::network::ProxyChoice::Socks5(
                crate::network::ProxyEndpoint {
                    host: "127.0.0.1".into(),
                    port: 7891,
                    user: None,
                },
            )),
            jump: None,
        };
        let mut d = draft();
        d.identity.group_id = Some(gid);
        let id = v.add(d, "2026-07-31T00:00:00Z");

        let got = v.resolve_for(id).unwrap();
        assert!(
            matches!(got.proxy, Some(crate::network::ProxyChoice::Socks5(_))),
            "分组代理应经 resolve_for 透出"
        );
    }

    #[test]
    fn expand_jump_chain_reports_dangling_reference() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let mut d = draft();
        d.network = crate::network::NetworkPrefs {
            proxy: None,
            jump: Some(vec![crate::network::JumpRef(SessionId(999))]),
        };
        let id = v.add(d, "2026-07-31T00:00:00Z");
        let err = v.expand_jump_chain(id).unwrap_err();
        assert!(matches!(err, StoreError::JumpDangling(SessionId(999))));
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

    #[test]
    fn automation_survives_save_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let id;
        {
            let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
            let mut d = draft();
            d.automation = crate::automation::AutomationPrefs {
                enabled: Some(true),
                tmux: Some(crate::automation::TmuxChoice::Attach {
                    session_name: Some("claude".into()),
                }),
                commands: Some(vec![crate::automation::AutomationCommand {
                    text: "echo 'hi'".into(),
                    delay_ms: Some(500),
                }]),
                work_dir: Some("/srv".into()),
                env: Some(vec![crate::automation::EnvVar {
                    key: "RUST_LOG".into(),
                    value: "debug".into(),
                }]),
                initial_delay_ms: Some(300),
                inter_delay_ms: None,
                ready_timeout_ms: None,
            };
            id = v.add(d, "2026-08-06T00:00:00Z");
            v.save().unwrap();
        }
        let v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let a = &v.get(id).unwrap().automation;
        assert_eq!(
            a.tmux,
            Some(crate::automation::TmuxChoice::Attach {
                session_name: Some("claude".into())
            })
        );
        assert_eq!(a.commands.as_ref().unwrap()[0].text, "echo 'hi'");
        assert_eq!(a.commands.as_ref().unwrap()[0].delay_ms, Some(500));
        assert_eq!(a.work_dir.as_deref(), Some("/srv"));
        assert_eq!(a.env.as_ref().unwrap()[0].key, "RUST_LOG");
        assert_eq!(a.inter_delay_ms, None, "未设的字段不能被写成 0");
    }

    #[test]
    fn group_automation_survives_save_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let g;
        {
            let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
            g = v.add_group("生产".into());
            v.group_mut(g).unwrap().automation.tmux = Some(crate::automation::TmuxChoice::Off);
            v.save().unwrap();
        }
        let v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        assert_eq!(
            v.groups()[0].automation.tmux,
            Some(crate::automation::TmuxChoice::Off),
            "显式 Off 必须能落盘再读回,不能被当成未设写没"
        );
    }

    #[test]
    fn resolve_for_carries_automation_from_group() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let gid = v.add_group("生产".into());
        v.group_mut(gid).unwrap().automation.tmux = Some(crate::automation::TmuxChoice::Attach {
            session_name: Some("shared".into()),
        });
        let mut d = draft();
        d.identity.group_id = Some(gid);
        let id = v.add(d, "2026-08-06T00:00:00Z");

        let got = v.resolve_for(id).unwrap();
        assert_eq!(
            got.automation.tmux,
            Some(crate::automation::TmuxChoice::Attach {
                session_name: Some("shared".into())
            }),
            "分组的 tmux 设置应经 resolve_for 透出"
        );
    }

    #[test]
    fn update_replaces_automation() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let mut d = draft();
        d.automation.tmux = Some(crate::automation::TmuxChoice::Attach {
            session_name: Some("old".into()),
        });
        let id = v.add(d, "t");

        let mut d2 = draft();
        d2.automation.tmux = Some(crate::automation::TmuxChoice::Off);
        v.update(id, d2, "t2").unwrap();

        assert_eq!(
            v.get(id).unwrap().automation.tmux,
            Some(crate::automation::TmuxChoice::Off),
            "update 必须把 automation 一起替换掉"
        );
    }

    /// v<5 的真实文件:私钥只有一个**路径**。
    fn v4_with_key_path(path: &str) -> String {
        format!(
            r#"
schema_version = 4

[[session]]
id = 1
modified_at = "t"

[session.identity]
name = "a"

[session.connection]
host = "h"
port = 22
protocol = "ssh"

[session.auth]
user = "u"
kind = "public_key"
path = "{path}"
has_passphrase = false
"#
        )
    }

    /// v4→v5 迁移的主路径:打开时把路径指向的**私钥内容**读进加密侧车,
    /// 并把路径本身从 sessions.toml 里抹掉。
    ///
    /// 三条断言缺一不可:内容进了侧车(否则公钥会话直接连不上)、路径不再落盘
    /// (这是本次改动的全部目的)、私钥内容没跟着落进明文 TOML(那比留路径更糟)。
    #[test]
    fn opening_a_v4_file_imports_the_key_file_content_and_drops_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let key_file = dir.path().join("id_ed25519");
        let body = "-----BEGIN OPENSSH PRIVATE KEY-----\nMIGRATED-BODY\n-----END OPENSSH PRIVATE KEY-----\n";
        std::fs::write(&key_file, body).unwrap();
        std::fs::write(
            dir.path().join("sessions.toml"),
            v4_with_key_path(&key_file.display().to_string()),
        )
        .unwrap();

        let v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        assert_eq!(
            v.secret(SessionId(1))
                .and_then(|s| s.private_key.as_deref()),
            Some(body),
            "私钥内容必须被读进加密侧车"
        );

        let on_disk = std::fs::read_to_string(dir.path().join("sessions.toml")).unwrap();
        assert!(
            !on_disk.contains("id_ed25519"),
            "迁移后 sessions.toml 里不该再有私钥路径: {on_disk}"
        );
        assert!(
            !on_disk.contains("MIGRATED-BODY"),
            "私钥内容绝不能落进明文 sessions.toml: {on_disk}"
        );
        assert!(on_disk.contains(&format!("schema_version = {CURRENT_SCHEMA}")));
    }

    /// 私钥文件已经不在了(被删/挪走/权限不足)时,库仍必须能打开 —— 整个
    /// 会话库因为一个私钥文件没了就打不开,用户连进去改都改不了。
    /// 降级后的表现是「这条会话没有私钥」,由 UI 提示重新导入。
    #[test]
    fn an_unreadable_legacy_key_file_degrades_to_no_key_instead_of_failing_to_open() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("sessions.toml"),
            v4_with_key_path("/definitely/not/here/id_rsa"),
        )
        .unwrap();

        let v = Vault::open(dir.path().to_path_buf(), &key())
            .expect("私钥文件读不到不该让整个库打不开");
        assert!(
            v.secret(SessionId(1))
                .and_then(|s| s.private_key.as_ref())
                .is_none(),
            "读不到就该是「没有私钥」,不能塞一个假值进去"
        );
        assert_eq!(v.list().len(), 1, "会话本身必须保留下来供用户重新导入");
    }

    /// 已经在侧车里的私钥**不能**被旧路径覆盖。用户有可能先用新版导入过一次
    /// 私钥、之后又打开了一份还带 path 的旧文件(比如从备份恢复 sessions.toml);
    /// 让路径赢会把用户刚导入的、可能已经轮换过的私钥换成一把旧钥匙。
    #[test]
    fn an_already_imported_private_key_is_not_overwritten_by_the_legacy_path() {
        let dir = tempfile::tempdir().unwrap();
        let key_file = dir.path().join("id_ed25519");
        std::fs::write(&key_file, "OLD-KEY-FROM-FILE").unwrap();
        std::fs::write(
            dir.path().join("sessions.toml"),
            v4_with_key_path(&key_file.display().to_string()),
        )
        .unwrap();
        // 先造出一份「侧车里已有私钥」的状态:直接写 secrets.enc 太绕,
        // 借一次正常的 open+update 把新私钥存进去,再把 sessions.toml 换回 v4。
        {
            let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
            let mut d = draft();
            d.auth.kind = AuthKind::PublicKey {
                has_passphrase: false,
            };
            d.secret = Some(SecretEntry {
                password: None,
                passphrase: None,
                proxy_password: None,
                private_key: Some("NEW-KEY-ALREADY-IMPORTED".into()),
            });
            v.update(SessionId(1), d, "t2").unwrap();
            v.save().unwrap();
        }
        std::fs::write(
            dir.path().join("sessions.toml"),
            v4_with_key_path(&key_file.display().to_string()),
        )
        .unwrap();

        let v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        assert_eq!(
            v.secret(SessionId(1))
                .and_then(|s| s.private_key.as_deref()),
            Some("NEW-KEY-ALREADY-IMPORTED"),
            "侧车里已有的私钥不该被旧路径里的内容覆盖"
        );
    }
}
