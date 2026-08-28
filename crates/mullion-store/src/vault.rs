//! Vault:唯一碰文件系统的地方。sessions.toml(明文非敏感)+ secrets.enc(加密敏感)。
//! 两文件各自 tmp+rename 原子写;时间戳由调用方注入(store 不持时钟)。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::credential::{CredentialId, CredentialRecord};
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
    credentials: Vec<CredentialRecord>,
    secrets: SecretMap,
    key: [u8; 32],
    /// `secrets.enc` 用的密钥方案(F71)。**`save()` 按它决定写不写文件头**,
    /// 与 `key` 永远同源同步 —— 两者不一致的后果是「用 A 的密钥写出声称 B 的
    /// 文件」,下次打开时所有凭据永久解不开。
    scheme: crate::secrets_file::Scheme,
    /// F189:上一次与盘同步时,`sessions.toml` 里**实际是什么字节**。
    ///
    /// 与 `synced_toml` 分两份存,不是一份:刚 `open` 完这两者完全可能不等
    /// (迁移、手改过的排版、`skip_serializing_if` 的取舍),合成一份的话
    /// 「我们有没有没落盘的改动」这个判据一开机就恒真,重读永远不会发生。
    disk_toml: String,
    /// F189:上一次与盘同步时,**我们自己**序列化出来是什么。
    ///
    /// 「手上有没有还没落盘的改动」= 现在序列化一遍跟它比。**结构式判据,
    /// 不是每个 mutator 各自举手的脏标记** —— 后者在加新 mutator 时必然漏
    /// (本项目「列举式门控」已经踩过三次),而漏掉的后果是重读把用户刚改的
    /// 东西静默丢掉。
    synced_toml: String,
    /// F189:重读时发生过什么,留给 app 记日志(store 零 UI,也不依赖 `log`)。
    reload_notes: Vec<String>,
}

/// 打开保险库时用什么开锁(F71,设计 D9/D10)。
///
/// `mullion-store` **永远不会主动索要密码**(零 UI 是架构不变量):是不是要密码,
/// 由 app 先 [`Vault::probe_scheme`] 探测、自己弹解锁框,再把密码传进来。
pub enum Unlock<'a> {
    /// 钥匙串方案(今天的默认)。
    Keyring(&'a dyn MasterKeySource),
    /// 主密码方案。
    Password(&'a str),
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
    /// F120:SFTP 默认目录 + 书签(D15:纯会话字段,不参与分组继承)。
    pub sftp: crate::sftp::SftpPrefs,
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

/// 新建/编辑凭据的输入(不含 id,由 vault 分配)。与 `SessionDraft` 同构。
pub struct CredentialDraft {
    pub name: String,
    pub user: String,
    pub kind: crate::model::AuthKind,
    /// 密码 / 私钥正文 / 私钥口令;无则 None。
    /// **代理口令不在这里** —— 那是会话的东西(设计 D4)。
    pub secret: Option<SecretEntry>,
}

/// 解析后的认证:到底以谁的身份、用什么方式、拿哪份密文去登录。
///
/// owned 而非借用:跳板链本来就是 `Vec<SessionRecord>`(clone 过一遍),
/// 借用只会把调用点被生命周期绑死,收益为零(设计 D5)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAuth {
    pub user: String,
    pub kind: crate::model::AuthKind,
    pub secret: Option<SecretEntry>,
}

/// 凭据密文在 `secrets.enc` 里的键(设计 D3)。
///
/// 带 `cred:` 前缀而不是另开一张表:`secrets.enc` 是一个
/// `BTreeMap<String, SecretEntry>`,另开表要改文件格式(又一次不兼容);
/// 前缀是纯加法,旧文件天然没有这类键,且与 `SessionId` 的十进制表示
/// 不可能撞车(有测试钉着)。
fn cred_key(id: CredentialId) -> String {
    format!("cred:{}", id.0)
}

impl Vault {
    fn sessions_path(&self) -> PathBuf {
        self.dir.join("sessions.toml")
    }
    fn secrets_path(&self) -> PathBuf {
        self.dir.join("secrets.enc")
    }

    /// `<dir>/secrets.enc` 声明的密钥方案。文件不存在 → `Keyring`(新库默认)。
    ///
    /// app 启动时先调这个,决定要不要在打开会话库之前先弹解锁框(设计 D10)。
    pub fn probe_scheme(dir: &Path) -> Result<crate::secrets_file::Scheme, StoreError> {
        let path = dir.join("secrets.enc");
        if !path.exists() {
            return Ok(crate::secrets_file::Scheme::Keyring);
        }
        let blob = fs::read(&path)?;
        Ok(crate::secrets_file::parse(&blob)?.0)
    }

    /// 打开(或初始化)`dir` 下的保险库。dir 由调用方(app)算好(directories)传入。
    ///
    /// 保留原签名(设计 D9):遇到主密码加密的文件时返回
    /// [`StoreError::PasswordRequired`],**而不是** `Crypto` —— 后者会让调用方
    /// 以为文件坏了。要开主密码库走 [`Vault::open_with`]。
    pub fn open(dir: PathBuf, key_source: &dyn MasterKeySource) -> Result<Self, StoreError> {
        Self::open_with(dir, Unlock::Keyring(key_source))
    }

    /// 打开保险库,由调用方指定开锁方式(F71)。
    pub fn open_with(dir: PathBuf, unlock: Unlock<'_>) -> Result<Self, StoreError> {
        fs::create_dir_all(&dir)?;

        let sessions_path = dir.join("sessions.toml");
        let Loaded {
            groups,
            sessions,
            tunnels,
            credentials,
            migrated,
            legacy_key_paths,
        } = load_sessions(&sessions_path)?;

        let secrets_path = dir.join("secrets.enc");
        let raw = if secrets_path.exists() {
            Some(fs::read(&secrets_path)?)
        } else {
            None
        };
        // 方案由**文件自己**声明(设计 D1);文件不在就按调用方给的开锁方式定。
        let scheme = match &raw {
            Some(blob) => crate::secrets_file::parse(blob)?.0,
            None => match unlock {
                Unlock::Keyring(_) => crate::secrets_file::Scheme::Keyring,
                Unlock::Password(_) => crate::secrets_file::Scheme::Argon2id {
                    params: crate::kdf::KdfParams::default(),
                    salt: crate::kdf::random_salt(),
                },
            },
        };
        let key = derive_for(&scheme, &unlock)?;

        let mut secrets = match &raw {
            Some(blob) => {
                let payload = crate::secrets_file::parse(blob)?.1;
                // 主密码方案下,解不开**优先解释成密码错**(设计 D8):用户的下一步
                // 动作完全不同(重打一遍 vs 从备份恢复)。
                let plain = crypto::decrypt(&key, payload).map_err(|e| match scheme {
                    crate::secrets_file::Scheme::Argon2id { .. } => StoreError::WrongPassword,
                    crate::secrets_file::Scheme::Keyring => e,
                })?;
                let text = String::from_utf8(plain)?;
                toml::from_str::<SecretMap>(&text)?
            }
            None => SecretMap::new(),
        };

        // 裁剪孤儿密文:sessions.toml 可能被手改或在两文件写入之间崩溃残留旧 id
        // (spec §3.2「load 容忍 desync」)。不裁的话,后续 add() 用 max+1 复用旧 id
        // 会静默继承无关会话的密文。
        // F74:集合必须**同时**含凭据的键。少了它,凭据口令每次打开都被当成
        // 孤儿静默删掉 —— 用户看到的是「昨天还能连,今天要我重新输密码」。
        let live: std::collections::BTreeSet<String> = sessions
            .iter()
            .map(|s| s.id.0.to_string())
            .chain(credentials.iter().map(|c| cred_key(c.id)))
            .collect();
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

        let mut vault = Self {
            dir,
            groups,
            sessions,
            tunnels,
            credentials,
            secrets,
            key,
            scheme,
            disk_toml: fs::read_to_string(&sessions_path).unwrap_or_default(),
            synced_toml: String::new(),
            reload_notes: Vec::new(),
        };
        vault.synced_toml = vault.sessions_toml()?;
        if migrated {
            // 立即写回 v2,避免下次打开重复迁移并覆盖掉备份。
            vault.save()?;
        }
        Ok(vault)
    }

    /// 现在这份内存状态序列化出来是什么 —— `save()` 写出去的正文,以及
    /// 「有没有还没落盘的改动」那个判据的左边。两处必须**同一个函数**算:
    /// 各算各的话,某天有人只改了其中一处,重读的判据就悄悄失准了。
    fn sessions_toml(&self) -> Result<String, StoreError> {
        let file = SessionsFile {
            schema_version: CURRENT_SCHEMA,
            group: self.groups.clone(),
            session: self.sessions.clone(),
            tunnel: self.tunnels.clone(),
            credential: self.credentials.clone(),
        };
        Ok(toml::to_string_pretty(&file)?)
    }

    /// 落盘:两文件各自原子写。
    pub fn save(&mut self) -> Result<(), StoreError> {
        let toml_text = self.sessions_toml()?;
        write_atomic(&self.sessions_path(), toml_text.as_bytes())?;

        let secret_text = toml::to_string_pretty(&self.secrets)?;
        let payload = crypto::encrypt(&self.key, secret_text.as_bytes())?;
        // F71:`Keyring` 方案下 `encode` 是恒等,写出的字节与本片之前完全一致。
        let blob = crate::secrets_file::encode(&self.scheme, &payload);
        write_atomic(&self.secrets_path(), &blob)?;
        // 刚写出去的就是盘上那份,两个基准一起对齐 —— 漏了这一步,下一次
        // 改动会把**自己**刚写的内容当成「别的实例动过」再读一遍。
        self.disk_toml = toml_text.clone();
        self.synced_toml = toml_text;
        Ok(())
    }

    /// F189:动手改之前,先看看盘上那份是不是被**别的实例**写过了。
    ///
    /// 用户报的问题 1(升级后收藏夹全没了)的机制:多开是本项目的主场景,
    /// 而每个实例都在 `open` 那一刻把整个库读进内存,此后任何一次 `save()`
    /// 都是**整份覆盖**。A 实例开着不动,B 实例收藏了几个目录,A 这边随手
    /// 点一下保存 —— B 写进去的东西当场消失,全程没有任何报错。
    ///
    /// **有没落盘的改动时一律不读**:导入 ssh config(F2)那条路径是「连着
    /// `add` 十几条,最后统一 `save`」,中途读回来等于把前面几条静默丢掉。
    /// 判据是「现在序列化一遍 == 上次同步时序列化的那份」——**结构式**的,
    /// 新加 mutator 自动算进来,不靠每个 mutator 记得举手。
    ///
    /// 全程**不报错**:重读只是尽力而为,失败(文件被删/正被写/内容坏了)
    /// 就守着内存里这份继续用,和本片之前的行为完全一样。硬失败会把「点一下
    /// 收藏」变成「整个库打不开」,不成比例。
    ///
    /// `secrets.enc` 跟着一起重读:只读 sessions 的话,别的实例新建的会话
    /// 会指向一份我们手上没有的密文,表现是「明明存了密码却每次都问」。
    /// 解不开(别的实例设了主密码)时**保留内存里那份**并记一条 note ——
    /// 那种情况下两边的密钥已经分叉,这里做不了更多。
    fn sync_from_disk_if_untouched(&mut self) {
        let Ok(mine) = self.sessions_toml() else {
            return;
        };
        if mine != self.synced_toml {
            return;
        }
        let path = self.sessions_path();
        let Ok(text) = fs::read_to_string(&path) else {
            return;
        };
        if text == self.disk_toml {
            return;
        }
        let Ok(loaded) = load_sessions(&path) else {
            self.reload_notes.push(format!(
                "{} 被别的实例改成了读不懂的样子,继续用内存里那份",
                path.display()
            ));
            return;
        };
        self.groups = loaded.groups;
        self.sessions = loaded.sessions;
        self.tunnels = loaded.tunnels;
        self.credentials = loaded.credentials;
        match self.read_secrets() {
            Ok(Some(secrets)) => self.secrets = secrets,
            // 文件还不存在 = 新库,内存里那份(空的)就是对的。
            Ok(None) => {}
            Err(()) => self
                .reload_notes
                .push("secrets.enc 解不开(别的实例改过主密码?),密文继续用内存里那份".into()),
        }
        self.trim_orphan_secrets();
        self.disk_toml = text;
        self.synced_toml = self.sessions_toml().unwrap_or_default();
        self.reload_notes
            .push("sessions.toml 被别的实例改过,已重新读入再改".into());
    }

    /// 拿当前的 `key`/`scheme` 把 `secrets.enc` 读回来。`Ok(None)` = 文件还不在。
    ///
    /// **不重新派生密钥**:`open` 时派生的那把还在手上(主密码方案下重新派生
    /// 意味着再问用户要一次密码,而 store 是零 UI 的)。
    fn read_secrets(&self) -> Result<Option<SecretMap>, ()> {
        let path = self.secrets_path();
        if !path.exists() {
            return Ok(None);
        }
        let blob = fs::read(&path).map_err(|_| ())?;
        let payload = crate::secrets_file::parse(&blob).map_err(|_| ())?.1;
        let plain = crypto::decrypt(&self.key, payload).map_err(|_| ())?;
        let text = String::from_utf8(plain).map_err(|_| ())?;
        toml::from_str::<SecretMap>(&text).map(Some).map_err(|_| ())
    }

    /// 裁剪孤儿密文。判据与 `open_with` 里那段同源(见其注释:少了凭据那一支,
    /// 凭据口令会被当成孤儿静默删掉)。
    fn trim_orphan_secrets(&mut self) {
        let live: std::collections::BTreeSet<String> = self
            .sessions
            .iter()
            .map(|s| s.id.0.to_string())
            .chain(self.credentials.iter().map(|c| cred_key(c.id)))
            .collect();
        self.secrets.retain(|k, _| live.contains(k));
    }

    /// F189:把重读期间攒下的说明取走(取完就清)。app 拿去写日志 ——
    /// 这类事件必须留痕:用户看到的现象是「我明明收藏过」,而没有这行日志
    /// 就没法把它和「另一个实例覆盖了」对上。
    pub fn take_reload_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.reload_notes)
    }

    /// 当前是不是主密码方案(UI 显示「已设定 / 未设定」)。
    pub fn has_master_password(&self) -> bool {
        self.scheme.has_password()
    }

    /// 设定或修改主密码(F71)。换新盐、重新派生、**立刻整文件重写**。
    ///
    /// 空密码不算密码:那会让「已设定」这个状态对应一个人人都能解开的库。
    ///
    /// **不动钥匙串条目**(设计 D6):删除是不可逆的,而它的存在完全无害
    /// (没有文件再引用它),留着还让「撤销主密码」有一条不必重新生成密钥的路。
    pub fn set_master_password(&mut self, password: &str) -> Result<(), StoreError> {
        self.sync_from_disk_if_untouched();
        if password.is_empty() {
            return Err(StoreError::Kdf("主密码不能为空".into()));
        }
        let params = crate::kdf::KdfParams::default();
        let salt = crate::kdf::random_salt();
        let key = crate::kdf::derive_key(password, &salt, params)?;
        self.key = key;
        self.scheme = crate::secrets_file::Scheme::Argon2id { params, salt };
        self.save()
    }

    /// 撤销主密码,回到钥匙串方案(F71)。
    ///
    /// **先拿到钥匙串密钥再改自己的字段**(设计 D7):钥匙串不可用时报错并停在
    /// 主密码方案。反过来写的话,内存里的密钥换了、文件还是老的,下一次任何
    /// `save()` 都会用错的密钥重写整个 `secrets.enc` —— 所有凭据当场变成
    /// 永久解不开的字节。
    pub fn clear_master_password(
        &mut self,
        key_source: &dyn MasterKeySource,
    ) -> Result<(), StoreError> {
        self.sync_from_disk_if_untouched();
        let key = key_source.load_or_create()?;
        self.key = key;
        self.scheme = crate::secrets_file::Scheme::Keyring;
        self.save()
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
        self.sync_from_disk_if_untouched();
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
            sftp: draft.sftp,
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
        self.sync_from_disk_if_untouched();
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
        // F189:**书签不跟着整份 draft 覆盖**。draft 里那份是「编辑器打开
        // 那一刻」的快照,而路径条上的 ☆(`add_bookmark`/`remove_bookmark`)
        // 是同一份数据的另一条写入口 —— 编辑器开着的时候收藏的目录,点一下
        // 「保存」就被那份旧快照顶掉了,而且没有任何提示。
        //
        // 编辑器里那张书签表要生效,走 `set_bookmarks`(只在用户真的动过
        // 它时才调,判据在 app 侧的 `is_dirty` 基线上)。
        let bookmarks = std::mem::take(&mut rec.sftp.bookmarks);
        let local_bookmarks = std::mem::take(&mut rec.sftp.local_bookmarks);
        rec.sftp = draft.sftp;
        rec.sftp.bookmarks = bookmarks;
        rec.sftp.local_bookmarks = local_bookmarks;
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
        self.sync_from_disk_if_untouched();
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
        self.sync_from_disk_if_untouched();
        let rec = self
            .sessions
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or(StoreError::NotFound(id))?;
        rec.identity.group_id = group;
        Ok(())
    }

    /// F121:把一条会话挪到 `before` 之前(`None` = 末尾),顺带改组。
    ///
    /// 组内排序与跨组拖动**共用这一个入口** —— 拆成两个函数会让「跨组时位置
    /// 怎么算」有两份实现,而这两份必然分叉。
    ///
    /// `before` 指向的记录不存在时落到末尾而不是报错:UI 拿到 id 与松手之间
    /// 隔着若干帧,那条记录可能刚被删掉,这不是异常。
    ///
    /// **不重打 `modified_at`**:换位置是组织动作,不是内容变更(同 `set_group`)。
    pub fn move_session(
        &mut self,
        id: SessionId,
        group: Option<GroupId>,
        before: Option<SessionId>,
    ) -> Result<(), StoreError> {
        self.sync_from_disk_if_untouched();
        if before == Some(id) {
            return Ok(());
        }
        let from = self
            .sessions
            .iter()
            .position(|s| s.id == id)
            .ok_or(StoreError::NotFound(id))?;
        let mut rec = self.sessions.remove(from);
        rec.identity.group_id = group;
        // 下标必须在 `remove` **之后**再算:先算的话,目标在被拖走那条右边时
        // 会因为整体左移而差一位。
        let at = match before {
            Some(t) => self
                .sessions
                .iter()
                .position(|s| s.id == t)
                .unwrap_or(self.sessions.len()),
            None => self.sessions.len(),
        };
        self.sessions.insert(at, rec);
        Ok(())
    }

    /// F139:给一条会话加一个 SFTP 书签(文件面板路径条上的 ☆)。
    ///
    /// 不走 `update`:同 `set_group` 的理由 —— 调用方手上只有一个 id,为了改
    /// 一条书签去凭空重建整份 `SessionDraft`,漏填任何字段都是静默把用户的
    /// 配置改掉。
    ///
    /// **按 `path` 去重**:书签的身份就是路径(`remove_bookmark` 也按它匹配),
    /// 而路径条的 ★/☆ 靠「当前目录在不在列表里」现算 —— 存进两条同路径的
    /// 书签,用户点「取消收藏」会看起来点一次没反应。
    ///
    /// **不重打 `modified_at`**:收藏是浏览过程中的组织动作,不是配置内容变更。
    pub fn add_bookmark(
        &mut self,
        id: SessionId,
        mark: crate::sftp::Bookmark,
    ) -> Result<(), StoreError> {
        self.sync_from_disk_if_untouched();
        let rec = self
            .sessions
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or(StoreError::NotFound(id))?;
        push_deduped(&mut rec.sftp.bookmarks, mark);
        Ok(())
    }

    /// F139:取消收藏。按路径相等匹配 —— 书签的身份就是路径,名字可以重复
    /// 也可以为空。
    pub fn remove_bookmark(&mut self, id: SessionId, path: &str) -> Result<(), StoreError> {
        self.sync_from_disk_if_untouched();
        let rec = self
            .sessions
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or(StoreError::NotFound(id))?;
        rec.sftp.bookmarks.retain(|b| b.path != path);
        Ok(())
    }

    /// F189:把会话编辑器里那张书签表**整份**写回去。
    ///
    /// 与 `update` 分开,是因为两者的触发条件不同:`update` 是「点了保存」,
    /// 而这个是「点了保存**并且**真的动过书签表」。合在一起的话,任何一次
    /// 保存都会拿编辑器打开那一刻的快照覆盖掉路径条 ☆ 后来收藏的东西。
    ///
    /// **不重打 `modified_at`**:同 `add_bookmark`,收藏是组织动作。
    pub fn set_bookmarks(
        &mut self,
        id: SessionId,
        marks: Vec<crate::sftp::Bookmark>,
    ) -> Result<(), StoreError> {
        self.sync_from_disk_if_untouched();
        let rec = self
            .sessions
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or(StoreError::NotFound(id))?;
        rec.sftp.bookmarks = marks;
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
        self.sync_from_disk_if_untouched();
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
        self.sync_from_disk_if_untouched();
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
        self.sync_from_disk_if_untouched();
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

    // ---- F74 凭据实体 --------------------------------------------------

    pub fn credentials(&self) -> &[CredentialRecord] {
        &self.credentials
    }

    pub fn credential(&self, id: CredentialId) -> Option<&CredentialRecord> {
        self.credentials.iter().find(|c| c.id == id)
    }

    /// 凭据自己的密文(密码 / 私钥正文 / 私钥口令)。
    ///
    /// **不含代理口令**:那是会话的东西,不是身份的东西(设计 D4)。
    pub fn credential_secret(&self, id: CredentialId) -> Option<&SecretEntry> {
        self.secrets.get(&cred_key(id))
    }

    /// 新增凭据。id 取现有 max+1(空库从 1 起),**与会话/隧道号池互不影响**。
    pub fn add_credential(&mut self, draft: CredentialDraft) -> CredentialId {
        self.sync_from_disk_if_untouched();
        let id = CredentialId(
            self.credentials
                .iter()
                .map(|c| c.id.0)
                .max()
                .map_or(1, |m| m + 1),
        );
        self.put_credential_secret(id, draft.secret);
        self.credentials.push(CredentialRecord {
            id,
            name: draft.name,
            user: draft.user,
            kind: draft.kind,
        });
        id
    }

    pub fn update_credential(
        &mut self,
        id: CredentialId,
        draft: CredentialDraft,
    ) -> Result<(), StoreError> {
        self.sync_from_disk_if_untouched();
        let rec = self
            .credentials
            .iter_mut()
            .find(|c| c.id == id)
            .ok_or(StoreError::CredentialNotFound(id))?;
        rec.name = draft.name;
        rec.user = draft.user;
        rec.kind = draft.kind;
        self.put_credential_secret(id, draft.secret);
        Ok(())
    }

    /// 删除凭据。**被引用时硬失败并带上引用者**(设计 D7)。
    ///
    /// 不学分组的「删了就把引用置空」:分组是组织手段,丢了不影响能不能连;
    /// 凭据是身份,悄悄解绑等于把一堆会话变成连不上的废配置,而用户要到
    /// 下次连接时才发现。
    pub fn delete_credential(&mut self, id: CredentialId) -> Result<(), StoreError> {
        self.sync_from_disk_if_untouched();
        if self.credential(id).is_none() {
            return Err(StoreError::CredentialNotFound(id));
        }
        let users = self.sessions_using(id);
        if !users.is_empty() {
            return Err(StoreError::CredentialInUse(users));
        }
        self.credentials.retain(|c| c.id != id);
        self.secrets.remove(&cred_key(id));
        Ok(())
    }

    /// 引用了这份凭据的会话(按 id 升序,UI 直接拿去列「先解绑这几条」)。
    pub fn sessions_using(&self, id: CredentialId) -> Vec<SessionId> {
        let mut v: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|s| s.auth.credential_id() == Some(id))
            .map(|s| s.id)
            .collect();
        v.sort_unstable();
        v
    }

    fn put_credential_secret(&mut self, id: CredentialId, secret: Option<SecretEntry>) {
        match secret {
            Some(sec) => {
                self.secrets.insert(cred_key(id), sec);
            }
            None => {
                self.secrets.remove(&cred_key(id));
            }
        }
    }

    /// 解析一条会话的认证:本会话独有就用它自己的,引用共享凭据就查凭据表。
    pub fn resolve_auth(&self, rec: &SessionRecord) -> Result<ResolvedAuth, StoreError> {
        self.resolve_auth_of(&rec.auth, self.secret(rec.id))
    }

    /// `resolve_auth` 的参数化内核:直接吃一份 `Auth` + 「本会话独有」时该用的
    /// 密文,不要求它已经入库 —— F92「测试连接」解析的是**尚未保存的草稿**,
    /// 草稿没有 id,查不到自己的密文(与 `resolve_layer` /
    /// `expand_jump_chain_of` 同一个模式)。
    ///
    /// 两条路径共用这一个内核,否则迟早出现「拨测通过、保存后连不上」。
    ///
    /// 悬空引用**硬失败**(设计 D6):不回落到 agent、不回落到空口令、
    /// 不回落到任何别的身份。
    pub fn resolve_auth_of(
        &self,
        auth: &Auth,
        inline_secret: Option<&SecretEntry>,
    ) -> Result<ResolvedAuth, StoreError> {
        match auth {
            Auth::Inline(i) => Ok(ResolvedAuth {
                user: i.user.clone(),
                kind: i.kind.clone(),
                secret: inline_secret.cloned(),
            }),
            Auth::Ref(id) => {
                let c = self
                    .credential(*id)
                    .ok_or(StoreError::DanglingCredential(*id))?;
                Ok(ResolvedAuth {
                    user: c.user.clone(),
                    kind: c.kind.clone(),
                    secret: self.credential_secret(*id).cloned(),
                })
            }
        }
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
        self.sync_from_disk_if_untouched();
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
        self.sync_from_disk_if_untouched();
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
        self.sync_from_disk_if_untouched();
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
    credentials: Vec<CredentialRecord>,
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
            credentials: Vec::new(),
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
            credentials: file.credential,
            migrated: true,
            legacy_key_paths,
        })
    } else {
        let file: SessionsFile = toml::from_str(&text)?;
        Ok(Loaded {
            groups: file.group,
            sessions: file.session,
            tunnels: file.tunnel,
            credentials: file.credential,
            migrated: false,
            legacy_key_paths: BTreeMap::new(),
        })
    }
}

/// 按文件声明的方案 + 调用方给的开锁方式,算出主密钥(F71)。
///
/// 两种**不匹配**各有专门的错误,不许混成一个「解密失败」:
/// - 文件要密码、调用方只给了钥匙串 → `PasswordRequired`(app 据此弹解锁框)
/// - 文件不要密码、调用方却给了密码 → `WrongPassword`(这是调用方搞错了对象,
///   拿密码去派生只会得到一把解不开这个文件的钥匙,提前说清楚)
fn derive_for(
    scheme: &crate::secrets_file::Scheme,
    unlock: &Unlock<'_>,
) -> Result<[u8; 32], StoreError> {
    match (scheme, unlock) {
        (crate::secrets_file::Scheme::Keyring, Unlock::Keyring(src)) => src.load_or_create(),
        (crate::secrets_file::Scheme::Keyring, Unlock::Password(_)) => {
            Err(StoreError::WrongPassword)
        }
        (crate::secrets_file::Scheme::Argon2id { .. }, Unlock::Keyring(_)) => {
            Err(StoreError::PasswordRequired)
        }
        (crate::secrets_file::Scheme::Argon2id { params, salt }, Unlock::Password(pw)) => {
            crate::kdf::derive_key(pw, salt, *params)
        }
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

/// F139/F154:书签入列表的去重。**按 `path`** —— 书签的身份就是路径,名字
/// 可以重复也可以为空。留先来的那条,不拿后来的覆盖。
///
/// 远端与本地两份列表共用这一条:分叉的话,一边改了去重判据另一边不改,
/// 症状是某一栏点「取消收藏」看起来没反应。
///
/// F187 之后 `settings.rs` 里那份全局本地书签也用它 —— 同上,判据只能有一条。
pub(crate) fn push_deduped(list: &mut Vec<crate::sftp::Bookmark>, mark: crate::sftp::Bookmark) {
    if !list.iter().any(|b| b.path == mark.path) {
        list.push(mark);
    }
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
            auth: Auth::inline("u", AuthKind::Password),
            terminal: TerminalPrefs::default(),
            appearance: AppearancePrefs::default(),
            network: crate::network::NetworkPrefs::default(),
            automation: crate::automation::AutomationPrefs::default(),
            sftp: crate::sftp::SftpPrefs::default(),
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

    // ---- F74 凭据实体 --------------------------------------------------

    fn cred_draft(name: &str, user: &str, pw: Option<&str>) -> CredentialDraft {
        CredentialDraft {
            name: name.into(),
            user: user.into(),
            kind: AuthKind::Password,
            secret: pw.map(|p| SecretEntry {
                password: Some(p.into()),
                passphrase: None,
                proxy_password: None,
                private_key: None,
            }),
        }
    }

    /// 凭据号池独立于会话/隧道:挤在一个号池里会让「凭据 7」和「会话 7」
    /// 在日志/错误文案里长得一样,排查时误判成同一个对象。
    #[test]
    fn credential_ids_are_max_plus_one_and_independent() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        for i in 0..3 {
            v.add(draft_pw(&format!("s{i}"), "p"), "t");
        }
        assert_eq!(
            v.add_credential(cred_draft("运维", "ops", Some("p1"))),
            CredentialId(1)
        );
        assert_eq!(
            v.add_credential(cred_draft("只读", "ro", None)),
            CredentialId(2)
        );
    }

    /// 失效模式 1:`Vault::open` 的孤儿裁剪集合必须**同时**含凭据的键。
    ///
    /// 自证会变红:把 `open` 里 `live` 的 `.chain(credentials...)` 去掉,
    /// 这条立刻报「凭据口令被当成孤儿删掉了」。症状在真实使用里是
    /// 「昨天还能连,今天要我重新输密码」,而且没有任何报错。
    #[test]
    fn credential_secrets_survive_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let cid = {
            let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
            let cid = v.add_credential(cred_draft("运维", "ops", Some("shared-pw")));
            v.save().unwrap();
            cid
        };
        let v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        assert_eq!(v.credentials().len(), 1, "凭据本身不该丢");
        assert_eq!(
            v.credential_secret(cid).and_then(|s| s.password.as_deref()),
            Some("shared-pw"),
            "凭据口令被当成孤儿密文删掉了"
        );
    }

    /// 失效模式 8:`cred:` 前缀与 `SessionId` 的十进制表示不可能撞车。
    /// 撞了的后果是凭据密文覆盖掉会话密文(或反过来),两边都读到错的口令。
    #[test]
    fn credential_secret_keys_cannot_collide_with_session_ids() {
        let k = cred_key(CredentialId(1));
        assert!(k.starts_with("cred:"), "实际 {k}");
        assert!(
            k.parse::<u64>().is_err(),
            "凭据键必须不是纯十进制,否则会与 SessionId 的键撞车"
        );
        assert_ne!(k, SessionId(1).0.to_string());
    }

    /// 失效模式 3:被引用的凭据不可删,且错误要**带上引用者**——
    /// 只说「有人在用」而不说是谁,用户得挨条会话点开看。
    #[test]
    fn deleting_a_referenced_credential_is_refused_and_names_the_referents() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let cid = v.add_credential(cred_draft("运维", "ops", Some("p1")));
        let mut d = draft_pw("a", "ignored");
        d.auth = Auth::Ref(cid);
        d.secret = None;
        let sid = v.add(d, "t");

        match v.delete_credential(cid) {
            Err(StoreError::CredentialInUse(ids)) => assert_eq!(ids, vec![sid]),
            other => panic!("被引用的凭据必须拒绝删除并列出引用者,实际 {other:?}"),
        }
        assert!(v.credential(cid).is_some(), "拒绝之后凭据必须还在");

        // 解绑之后才能删,且连带清掉它的密文。
        let mut d2 = draft_pw("a", "own-pw");
        d2.auth = Auth::inline("u", AuthKind::Password);
        v.update(sid, d2, "t2").unwrap();
        v.delete_credential(cid).unwrap();
        assert!(v.credential_secret(cid).is_none(), "删凭据必须连带清密文");
        assert!(matches!(
            v.delete_credential(cid),
            Err(StoreError::CredentialNotFound(_))
        ));
    }

    /// 引用凭据的会话解析出的是**凭据的**用户名与密文。
    #[test]
    fn a_session_referencing_a_credential_resolves_to_the_credentials_identity() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let cid = v.add_credential(cred_draft("运维", "ops", Some("shared-pw")));
        let mut d = draft_pw("a", "own-pw");
        d.auth = Auth::Ref(cid);
        let sid = v.add(d, "t");

        let r = v.resolve_auth(v.get(sid).unwrap()).unwrap();
        assert_eq!(r.user, "ops");
        assert_eq!(r.secret.unwrap().password.as_deref(), Some("shared-pw"));

        // 反面:本会话独有时解析出的是它自己的。
        let sid2 = v.add(draft_pw("b", "b-pw"), "t");
        let r2 = v.resolve_auth(v.get(sid2).unwrap()).unwrap();
        assert_eq!(r2.user, "u");
        assert_eq!(r2.secret.unwrap().password.as_deref(), Some("b-pw"));
    }

    /// 失效模式 2:悬空引用**硬失败**,绝不降级。
    ///
    /// 降级的后果是「用一个别的身份登上了机器」—— 与 `JumpDangling` /
    /// `TunnelDangling` 同一条铁律。这里的悬空只能靠手改文件或旧版本残留
    /// 造出来(`delete_credential` 拦着正常路径),所以直接构造。
    #[test]
    fn a_dangling_credential_is_rejected_never_degraded() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let mut d = draft_pw("a", "own-pw");
        d.auth = Auth::Ref(CredentialId(99));
        let sid = v.add(d, "t");

        match v.resolve_auth(v.get(sid).unwrap()) {
            Err(StoreError::DanglingCredential(CredentialId(99))) => {}
            other => panic!("悬空凭据必须硬失败,实际 {other:?}"),
        }
    }

    /// 失效模式 4:代理口令**永远**来自会话自己的密文,不随凭据走。
    ///
    /// 代理是「怎么出网」,凭据是「以谁的身份」。两台机器共用一把私钥却各走
    /// 各的代理是完全正常的配置;让凭据接管代理口令,换一次共享凭据就会把
    /// 一串机器的代理口令换没了。
    #[test]
    fn the_proxy_password_always_comes_from_the_session_not_the_credential() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let cid = v.add_credential(cred_draft("运维", "ops", Some("shared-pw")));
        let mut d = draft_pw("a", "unused");
        d.auth = Auth::Ref(cid);
        d.secret = Some(SecretEntry {
            password: None,
            passphrase: None,
            proxy_password: Some("proxy-pw".into()),
            private_key: None,
        });
        let sid = v.add(d, "t");

        assert_eq!(
            v.secret(sid).and_then(|s| s.proxy_password.as_deref()),
            Some("proxy-pw"),
            "会话自己的代理口令不该因为引用了凭据就消失"
        );
        assert!(
            v.resolve_auth(v.get(sid).unwrap())
                .unwrap()
                .secret
                .unwrap()
                .proxy_password
                .is_none(),
            "凭据里不该带代理口令 —— 那是会话的东西"
        );
    }

    /// 草稿路径(F92 拨测)与入库路径共用同一个解析内核:草稿没有 id,
    /// 查不到自己的密文,必须能把手上那份直接喂进来。
    #[test]
    fn a_draft_resolves_through_the_same_kernel() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let cid = v.add_credential(cred_draft("运维", "ops", Some("shared-pw")));

        let own = SecretEntry {
            password: Some("draft-pw".into()),
            passphrase: None,
            proxy_password: None,
            private_key: None,
        };
        let inline = v
            .resolve_auth_of(&Auth::inline("me", AuthKind::Password), Some(&own))
            .unwrap();
        assert_eq!(inline.user, "me");
        assert_eq!(inline.secret.unwrap().password.as_deref(), Some("draft-pw"));

        // 草稿引用凭据时,手上那份 inline 密文一律**不参与** —— 严格二选一。
        let by_ref = v.resolve_auth_of(&Auth::Ref(cid), Some(&own)).unwrap();
        assert_eq!(by_ref.user, "ops");
        assert_eq!(
            by_ref.secret.unwrap().password.as_deref(),
            Some("shared-pw")
        );
    }

    /// 失效模式 7:v8 库升 v9 **零迁移代码**,且迁移**绝不静默合并**出凭据
    /// (那是 F75,只在用户点头后做)。
    #[test]
    fn migrating_v8_maps_every_session_to_inline_and_leaves_the_credential_table_empty() {
        const V8: &str = r#"
schema_version = 8

[[session]]
id = 1
modified_at = "2026-08-01T00:00:00Z"

[session.identity]
name = "机器 A"

[session.connection]
host = "10.0.0.1"
port = 22
protocol = "ssh"

[session.auth]
user = "ubuntu"
kind = "password"

[[session]]
id = 2
modified_at = "2026-08-01T00:00:00Z"

[session.identity]
name = "机器 B"

[session.connection]
host = "10.0.0.2"
port = 2222
protocol = "ssh"

[session.auth]
user = "ubuntu"
kind = "public_key"
has_passphrase = true
"#;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("sessions.toml"), V8).unwrap();
        let v = Vault::open(dir.path().to_path_buf(), &key()).expect("v8 文件必须能直接打开");

        assert_eq!(v.list().len(), 2);
        let a = v.get(SessionId(1)).unwrap();
        assert_eq!(a.identity.name, "机器 A");
        assert_eq!(a.connection.host, "10.0.0.1");
        assert_eq!(
            a.auth,
            Auth::inline("ubuntu", AuthKind::Password),
            "v8 的 auth 分节必须逐字段等价映射成 Inline"
        );
        let b = v.get(SessionId(2)).unwrap();
        assert_eq!(b.connection.port, 2222);
        assert_eq!(
            b.auth,
            Auth::inline(
                "ubuntu",
                AuthKind::PublicKey {
                    has_passphrase: true
                }
            )
        );
        assert!(
            v.credentials().is_empty(),
            "迁移不许自作主张把两条同名 ubuntu 合并成一份共享凭据 —— 那是 F75,要用户点头"
        );
        assert!(
            dir.path().join("sessions.toml.bak").exists(),
            "升级前必须留备份"
        );
        let now = std::fs::read_to_string(dir.path().join("sessions.toml")).unwrap();
        assert!(now.contains("schema_version = 9"), "磁盘上应已升到 v9");
    }

    /// 引用凭据的会话存盘再读回,引用关系不能丢(丢了就是悄悄变回自带认证,
    /// 而自带的那份是空的 → 连不上)。
    #[test]
    fn a_reference_survives_save_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let (cid, sid) = {
            let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
            let cid = v.add_credential(cred_draft("运维", "ops", Some("shared-pw")));
            let mut d = draft_pw("a", "x");
            d.auth = Auth::Ref(cid);
            d.secret = None;
            let sid = v.add(d, "t");
            v.save().unwrap();
            (cid, sid)
        };
        let v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        assert_eq!(v.get(sid).unwrap().auth, Auth::Ref(cid));
        assert_eq!(v.resolve_auth(v.get(sid).unwrap()).unwrap().user, "ops");
    }

    // ---- F71 主密码 ----------------------------------------------------

    /// 建一个装了一条密码的库,返回目录。
    fn vault_with_secret(dir: &std::path::Path) {
        let mut v = Vault::open(dir.to_path_buf(), &key()).unwrap();
        v.add(draft_pw("a", "p1"), "2026-08-13T00:00:00Z");
        v.save().unwrap();
    }

    /// spec F71 要的「未设主密码时与今日行为逐字节等价」的可测形式:
    /// 磁盘上的 `secrets.enc` 必须能被**本片之前那条路径**(钥匙串密钥直接
    /// `crypto::decrypt`)原样解开 —— 没有文件头、没有任何新前缀。
    #[test]
    fn without_a_master_password_the_file_stays_in_the_legacy_format() {
        let dir = tempfile::tempdir().unwrap();
        vault_with_secret(dir.path());
        let bytes = fs::read(dir.path().join("secrets.enc")).unwrap();
        let plain =
            crypto::decrypt(&[5u8; 32], &bytes).expect("不设主密码时,老路径必须能一字不改地解开");
        assert!(String::from_utf8(plain).unwrap().contains("p1"));
    }

    #[test]
    fn setting_a_master_password_rewrites_the_file_so_the_old_key_no_longer_opens_it() {
        let dir = tempfile::tempdir().unwrap();
        vault_with_secret(dir.path());
        {
            let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
            v.set_master_password("hunter2").unwrap();
        }
        let bytes = fs::read(dir.path().join("secrets.enc")).unwrap();
        assert!(
            crypto::decrypt(&[5u8; 32], &bytes).is_err(),
            "设了主密码之后,钥匙串那把旧密钥必须再也解不开这个文件"
        );
        assert!(
            Vault::probe_scheme(dir.path()).unwrap().has_password(),
            "文件必须自报是主密码方案"
        );
    }

    #[test]
    fn a_password_protected_vault_reopens_with_the_right_password() {
        let dir = tempfile::tempdir().unwrap();
        vault_with_secret(dir.path());
        {
            let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
            v.set_master_password("hunter2").unwrap();
        }
        let v = Vault::open_with(dir.path().to_path_buf(), Unlock::Password("hunter2")).unwrap();
        assert_eq!(
            v.secret(SessionId(1)).unwrap().password.as_deref(),
            Some("p1"),
            "换成主密码方案不得丢任何一条凭据"
        );
    }

    /// 设计 D8:密码错与文件损坏必须分开报 —— 用户的下一步动作完全不同。
    #[test]
    fn the_wrong_password_is_reported_as_wrong_password_not_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        vault_with_secret(dir.path());
        {
            let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
            v.set_master_password("hunter2").unwrap();
        }
        let r = Vault::open_with(dir.path().to_path_buf(), Unlock::Password("hunter3")).err();
        assert!(
            matches!(r, Some(StoreError::WrongPassword)),
            "密码错必须报 WrongPassword,报 Crypto 会把人引去查文件损坏,实际 {r:?}"
        );
    }

    /// 设计 D9:老签名遇到主密码文件要说「需要密码」,不是「密文损坏」。
    #[test]
    fn opening_a_password_protected_vault_the_old_way_asks_for_a_password() {
        let dir = tempfile::tempdir().unwrap();
        vault_with_secret(dir.path());
        {
            let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
            v.set_master_password("hunter2").unwrap();
        }
        let r = Vault::open(dir.path().to_path_buf(), &key()).err();
        assert!(
            matches!(r, Some(StoreError::PasswordRequired)),
            "实际 {r:?}"
        );
    }

    #[test]
    fn changing_the_master_password_invalidates_the_old_one() {
        let dir = tempfile::tempdir().unwrap();
        vault_with_secret(dir.path());
        {
            let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
            v.set_master_password("old").unwrap();
        }
        {
            let mut v =
                Vault::open_with(dir.path().to_path_buf(), Unlock::Password("old")).unwrap();
            v.set_master_password("new").unwrap();
        }
        assert!(matches!(
            Vault::open_with(dir.path().to_path_buf(), Unlock::Password("old")),
            Err(StoreError::WrongPassword)
        ));
        let v = Vault::open_with(dir.path().to_path_buf(), Unlock::Password("new")).unwrap();
        assert_eq!(
            v.secret(SessionId(1)).unwrap().password.as_deref(),
            Some("p1")
        );
    }

    #[test]
    fn clearing_the_master_password_returns_the_file_to_the_legacy_format() {
        let dir = tempfile::tempdir().unwrap();
        vault_with_secret(dir.path());
        {
            let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
            v.set_master_password("hunter2").unwrap();
        }
        {
            let mut v =
                Vault::open_with(dir.path().to_path_buf(), Unlock::Password("hunter2")).unwrap();
            v.clear_master_password(&key()).unwrap();
            assert!(!v.has_master_password());
        }
        assert!(!Vault::probe_scheme(dir.path()).unwrap().has_password());
        let bytes = fs::read(dir.path().join("secrets.enc")).unwrap();
        crypto::decrypt(&[5u8; 32], &bytes).expect("撤销之后必须回到钥匙串能开的老格式");
        let v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        assert_eq!(
            v.secret(SessionId(1)).unwrap().password.as_deref(),
            Some("p1"),
            "撤销主密码不得丢凭据"
        );
    }

    /// 空密码不算密码:允许它就等于让「已设定」这个状态对应一个人人都能开的库。
    #[test]
    fn an_empty_master_password_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        assert!(matches!(v.set_master_password(""), Err(StoreError::Kdf(_))));
        assert!(!v.has_master_password(), "被拒之后必须还停在钥匙串方案");
    }

    #[test]
    fn probe_scheme_on_a_missing_file_says_keyring() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!Vault::probe_scheme(dir.path()).unwrap().has_password());
    }

    /// 空库(还没 save 过)也能直接设主密码 —— 此时 `secrets.enc` 尚不存在,
    /// `set_master_password` 里的 `save()` 负责把它连同文件头一起造出来。
    #[test]
    fn a_brand_new_vault_can_be_given_a_master_password() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
            v.set_master_password("hunter2").unwrap();
            v.add(draft_pw("a", "p1"), "2026-08-13T00:00:00Z");
            v.save().unwrap();
        }
        let v = Vault::open_with(dir.path().to_path_buf(), Unlock::Password("hunter2")).unwrap();
        assert_eq!(
            v.secret(SessionId(1)).unwrap().password.as_deref(),
            Some("p1")
        );
    }

    /// 拿密码去开一个钥匙串库 —— 报「密码不对」而不是拿密码派生出一把
    /// 解不开的钥匙再报「密文损坏」。
    #[test]
    fn a_password_against_a_keyring_vault_is_reported_not_silently_wrong() {
        let dir = tempfile::tempdir().unwrap();
        vault_with_secret(dir.path());
        assert!(matches!(
            Vault::open_with(dir.path().to_path_buf(), Unlock::Password("hunter2")),
            Err(StoreError::WrongPassword)
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
        let mut vault = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
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
        d.auth = Auth::inline(
            "u",
            AuthKind::PublicKey {
                has_passphrase: false,
            },
        );
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
        assert_eq!(s.auth.as_inline().unwrap().user, "u2");

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

    /// F120:SFTP 偏好必须**存得进盘、也改得动**。
    ///
    /// 这条补的是一个真实缺口:schema v8 只给 `SessionRecord` 加了 `sftp`,
    /// `SessionDraft` 上没有对应字段 —— `add` 硬写 `SftpPrefs::default()`、
    /// `update` 压根不碰它。症状是**编辑器里填了、点保存、重开全没了**,
    /// 且没有任何报错。补上字段之后仍然零守护:实测把 `update` 里
    /// `rec.sftp = draft.sftp;` 整行删掉、或把 `add` 里 `sftp: draft.sftp`
    /// 改回 `SftpPrefs::default()`,全 workspace 测试**一条都不红**。
    ///
    /// 所以这里两条路径都要走到:`add`(新建)与 `update`(改已有),
    /// 而且都要**存盘再重开**,不能只查内存里那份 —— 只查内存的话,
    /// 序列化漏字段(比如 `skip_serializing_if` 写错)照样测不出来。
    #[test]
    fn sftp_prefs_survive_save_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let id;
        {
            let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
            let mut d = draft();
            d.sftp = crate::sftp::SftpPrefs {
                default_remote: Some("/srv/app".into()),
                default_local: None,
                bookmarks: vec![crate::sftp::Bookmark {
                    name: "日志".into(),
                    path: "/var/log".into(),
                }],
                local_bookmarks: Vec::new(),
            };
            id = v.add(d, "2026-08-13T00:00:00Z");
            v.save().unwrap();
        }
        {
            let v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
            let rec = v.get(id).unwrap();
            assert_eq!(rec.sftp.default_remote.as_deref(), Some("/srv/app"));
            assert_eq!(rec.sftp.bookmarks.len(), 1, "书签在新建这条路径上丢了");
            assert_eq!(rec.sftp.bookmarks[0].path, "/var/log");
        }
        // 再走一遍 `update`:新建存得住不代表改得动,两条是分开的写入口。
        {
            let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
            let mut d = draft();
            d.sftp.default_remote = Some("/opt/data".into());
            v.update(id, d, "2026-08-13T01:00:00Z").unwrap();
            v.save().unwrap();
        }
        let v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let rec = v.get(id).unwrap();
        assert_eq!(
            rec.sftp.default_remote.as_deref(),
            Some("/opt/data"),
            "update 没把 SFTP 偏好写回去 —— 用户改了保存,下次打开还是旧值"
        );
        // F189 起这里**反过来**了:书签不再跟着整份 draft 覆盖(见 `update`
        // 里那段注释 —— draft 带的是编辑器打开那一刻的快照,而路径条上的 ☆
        // 是同一份数据的另一条写入口)。编辑器里那张表要生效走 `set_bookmarks`。
        assert_eq!(
            rec.sftp.bookmarks.len(),
            1,
            "update 把书签一起覆盖了 —— 编辑器开着时收藏的目录会被旧快照顶掉"
        );
        assert_eq!(rec.sftp.bookmarks[0].path, "/var/log");
    }

    /// F139:路径条上的 ☆ 收藏必须**存得进盘**,而不是只改内存里那份 ——
    /// 只改内存的症状是「收藏了,重开客户端没了」,且全程没有报错。
    ///
    /// 顺带守住去重:同一个路径收藏两次只该留一条。路径条的 ★/☆ 靠
    /// 「当前目录在不在列表里」现算,重复项会让「取消收藏」看起来点了没反应。
    #[test]
    fn bookmarks_added_from_the_path_bar_survive_save_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let id;
        {
            let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
            id = v.add(draft(), "2026-08-20T00:00:00Z");
            v.add_bookmark(
                id,
                crate::sftp::Bookmark {
                    name: "log".into(),
                    path: "/var/log".into(),
                },
            )
            .unwrap();
            // 同一个路径再来一次(用户在同一个目录连点两下 ☆ 之间隔了一次
            // 刷新,列表还没同步),名字不同也不许多出一条。
            v.add_bookmark(
                id,
                crate::sftp::Bookmark {
                    name: "另一个名字".into(),
                    path: "/var/log".into(),
                },
            )
            .unwrap();
            v.save().unwrap();
        }
        {
            let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
            let rec = v.get(id).unwrap();
            assert_eq!(rec.sftp.bookmarks.len(), 1, "同一路径收藏两次该去重");
            assert_eq!(rec.sftp.bookmarks[0].path, "/var/log");
            assert_eq!(
                rec.sftp.bookmarks[0].name, "log",
                "去重要留先来的那条,不是拿后来的覆盖"
            );
            v.remove_bookmark(id, "/var/log").unwrap();
            v.save().unwrap();
        }
        let v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        assert!(
            v.get(id).unwrap().sftp.bookmarks.is_empty(),
            "取消收藏没存盘 —— 删掉的书签重启后会回来"
        );
    }

    /// F189 / 用户报的问题 1:**多开是本项目的主场景**,而每个实例都在
    /// `open` 那一刻把整个库读进内存,此后任何一次 `save()` 都是整份覆盖。
    ///
    /// 现象:A 实例开着不动,B 实例收藏了几个目录并存盘,A 这边随手点一下
    /// 保存 —— B 写进去的东西当场消失,全程没有任何报错。
    ///
    /// 自证会变红:把 `set_group`(或任何一个 mutator)开头那句
    /// `self.sync_from_disk_if_untouched();` 删掉。
    #[test]
    fn another_instances_bookmarks_survive_our_next_save() {
        let dir = tempfile::tempdir().unwrap();
        let id;
        {
            let mut seed = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
            id = seed.add(draft(), "2026-08-28T00:00:00Z");
            seed.save().unwrap();
        }
        // A:开着,手上是这一刻的快照。
        let mut a = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        // A 先自己存过一次。**这一步是必要的**:`save` 结尾若不把两个基准
        // 对齐到刚写出去那份,`synced_toml` 就永远停在开机那一刻,此后
        // 「有没落盘的改动」恒为真 —— 重读从此静默停摆,而不是报错。
        a.set_group(id, None).unwrap();
        a.save().unwrap();
        let _ = a.take_reload_notes();
        // B:另一个实例收藏了一个目录并存盘。
        {
            let mut b = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
            b.add_bookmark(
                id,
                crate::sftp::Bookmark {
                    name: "日志".into(),
                    path: "/var/log".into(),
                },
            )
            .unwrap();
            b.save().unwrap();
        }
        // A 这边随手改点别的再存。
        a.set_group(id, None).unwrap();
        a.save().unwrap();
        assert!(
            !a.take_reload_notes().is_empty(),
            "重读这件事必须留痕 —— 没有日志的话,用户报「我明明收藏过」时无从对账"
        );

        let v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let rec = v.get(id).unwrap();
        assert_eq!(
            rec.sftp.bookmarks.len(),
            1,
            "另一个实例收藏的目录被我们这份开机快照整份覆盖掉了"
        );
        assert_eq!(rec.sftp.bookmarks[0].path, "/var/log");
    }

    /// 重读的**反面**:手上有还没落盘的改动时一律不读。
    ///
    /// 导入 ssh config(F2)那条路径是「连着 `add` 十几条,最后统一 `save`」——
    /// 中途重读一次,前面几条只在内存里的会话就被静默丢掉了,用户看到的是
    /// 「导入了 20 条,只进来最后 1 条」。
    ///
    /// 自证会变红:把 `sync_from_disk_if_untouched` 里那句
    /// `if mine != self.synced_toml { return; }` 删掉。
    #[test]
    fn a_batch_of_unsaved_adds_is_never_reloaded_out_from_under_itself() {
        let named = |n: &str| {
            let mut d = draft();
            d.identity.name = n.into();
            d
        };
        let dir = tempfile::tempdir().unwrap();
        let mut a = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        a.add(named("导入-1"), "2026-08-28T00:00:00Z");
        // 中途别的实例写了盘(A 手上那条还没落盘)。
        {
            let mut b = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
            b.add(named("别人的"), "2026-08-28T00:00:01Z");
            b.save().unwrap();
        }
        a.add(named("导入-2"), "2026-08-28T00:00:02Z");
        a.save().unwrap();
        let v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        // **按名字点名,不数条数**:只数条数的话,「A 的第一条被吞掉、
        // 换成了 B 那条」同样是 2,断言照样绿(实测过)。
        let names: Vec<&str> = v.list().iter().map(|r| r.identity.name.as_str()).collect();
        assert!(
            names.contains(&"导入-1") && names.contains(&"导入-2"),
            "这一批里有会话被中途的重读吞掉了:{names:?}"
        );
    }

    /// 自己刚写出去的那份**不算「别人动过」**。对不齐的话每次改动都要多读
    /// 一遍整个库,而且会往日志里刷一行假警报。
    ///
    /// 自证会变红:把 `save()` 结尾那两句 `self.disk_toml = ...` /
    /// `self.synced_toml = ...` 删掉。
    #[test]
    fn our_own_save_is_not_mistaken_for_another_instance() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let id = v.add(draft(), "2026-08-28T00:00:00Z");
        v.save().unwrap();
        let _ = v.take_reload_notes();
        v.set_group(id, None).unwrap();
        assert!(
            v.take_reload_notes().is_empty(),
            "把自己刚写的内容当成了别的实例的改动"
        );
    }

    /// **机械完备性守护**:`Vault` 上每一个会改状态的 `pub fn` 都必须以
    /// `sync_from_disk_if_untouched()` 开头。
    ///
    /// 这条守的不是今天的 18 个方法,而是**明天新加的那个**:漏掉一个的后果
    /// 是「只有走那条入口时才会覆盖掉别的实例」,概率性、无报错、极难复现
    /// (「列举式门控在加档时必然漏」,本项目已踩过三次)。
    ///
    /// 自证会变红:随便删掉一句 `self.sync_from_disk_if_untouched();`。
    #[test]
    fn every_mutating_entry_point_reloads_before_it_writes() {
        // `save` 自己就是落盘那一步 —— 它前面不该再读(读了等于把要写的
        // 东西丢掉);它在结尾对齐两个基准。`take_reload_notes` 动的是
        // 「重读时留下的说明」这本账,不是库的内容。
        const EXEMPT: &[&str] = &["save", "take_reload_notes"];
        let src = include_str!("vault.rs");
        let (prod, _) = src
            .split_once("\n#[cfg(test)]\nmod tests {")
            .expect("vault.rs 的测试模块分界变了,这条断言的锚点失效了");
        let mut checked = 0;
        for (i, _) in prod.match_indices("\n    pub fn ") {
            let rest = &prod[i + 1..];
            let name = rest["    pub fn ".len()..]
                .split(['(', '<'])
                .next()
                .expect("函数名")
                .to_string();
            // 签名可能跨多行:找到函数体的左大括号(第一条以 `{` 结尾的行)。
            let mut at = 0;
            let head = loop {
                let eol = rest[at..].find('\n').expect("函数没有结尾") + at;
                if rest[at..eol].trim_end().ends_with('{') {
                    break eol;
                }
                at = eol + 1;
            };
            if !rest[..head].contains("&mut self") || EXEMPT.contains(&name.as_str()) {
                continue;
            }
            let first = rest[head + 1..]
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("");
            assert_eq!(
                first.trim(),
                "self.sync_from_disk_if_untouched();",
                "`Vault::{name}` 改状态却没先重读 —— 走这条入口时会把别的实例\
                 刚写进去的东西整份覆盖掉,且完全静默"
            );
            checked += 1;
        }
        assert!(
            checked >= 18,
            "只扫到 {checked} 个 mutator,切片逻辑多半失效了(退化成恒绿)"
        );
    }

    /// F187:每条会话名下那份老的本地书签**必须仍然读得出来** —— 迁移
    /// (`Settings::merge_local_bookmarks`)的输入就是它。
    ///
    /// F154 当初把本地书签挂在会话下,F187 把它搬去了全局 `settings.toml`。
    /// 写入侧的两个方法(`add_local_bookmark`/`remove_local_bookmark`)随之
    /// 删掉了,**但字段留着**:老库里的数据要在首次启动时被读一遍并进去。
    /// 这条钉的就是那个读取口。
    ///
    /// 自证会变红:把 `SftpPrefs::local_bookmarks` 上的 `#[serde(default)]`
    /// 连同字段一起删掉(反序列化当场失败)。
    #[test]
    fn the_old_per_session_local_bookmarks_are_still_readable_for_the_migration() {
        let dir = tempfile::tempdir().unwrap();
        let id;
        {
            let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
            id = v.add(draft(), "2026-08-21T00:00:00Z");
            // 写入侧的方法已随 F187 删掉,直接摆字段 —— 这条测的是**读**。
            let rec = v.sessions.iter_mut().find(|r| r.id == id).unwrap();
            rec.sftp.local_bookmarks.push(crate::sftp::Bookmark {
                name: "工程".into(),
                path: r"D:\work".into(),
            });
            v.save().unwrap();
        }
        let v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let rec = v.get(id).unwrap();
        assert_eq!(
            rec.sftp.local_bookmarks.len(),
            1,
            "老数据读不出来了 —— 升级时用户的本地收藏会静默清零"
        );
        assert_eq!(rec.sftp.local_bookmarks[0].path, r"D:\work");
    }

    /// F154:老的 TOML(没有 `local_bookmarks` 这个键)读得进来,且是空列表。
    /// 不成立的话,这次升级会让所有既有会话直接读不出来 —— 用户的整个库消失。
    #[test]
    fn a_record_written_before_local_bookmarks_existed_still_loads() {
        let toml = r#"
default_remote = "/srv/app"

[[bookmarks]]
name = "日志"
path = "/var/log"
"#;
        let prefs: crate::sftp::SftpPrefs = toml::from_str(toml).expect("老记录该读得进来");
        assert_eq!(prefs.bookmarks.len(), 1);
        assert!(prefs.local_bookmarks.is_empty(), "缺键时该是空列表");
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
            auth: Auth::inline("u", AuthKind::Password),
            terminal: TerminalPrefs::default(),
            appearance: AppearancePrefs::default(),
            network: crate::network::NetworkPrefs::default(),
            automation: crate::automation::AutomationPrefs::default(),
            sftp: crate::sftp::SftpPrefs::default(),
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
            d.auth = Auth::inline(
                "u",
                AuthKind::PublicKey {
                    has_passphrase: false,
                },
            );
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

    // ---- F121 手动排序 --------------------------------------------------

    /// F121:组内前移。`before` 指向谁,就插在谁**前面**。
    ///
    /// 自证会变红:把实现里「先 remove 再定位 before」改成「先定位再 remove」——
    /// 目标在被拖走那条右边时会差一位。
    #[test]
    fn move_session_puts_the_record_right_before_the_target() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let mut names = Vec::new();
        for n in ["a", "b", "c"] {
            let mut d = draft();
            d.identity.name = n.into();
            names.push(v.add(d, "2026-08-16T00:00:00Z"));
        }
        // c 挪到 a 前面 → c, a, b
        v.move_session(names[2], None, Some(names[0])).unwrap();
        let order: Vec<&str> = v.list().iter().map(|r| r.identity.name.as_str()).collect();
        assert_eq!(order, vec!["c", "a", "b"]);

        // a 挪到 b 前面(目标在自己右边)→ c, a, b 不变
        v.move_session(names[0], None, Some(names[1])).unwrap();
        let order: Vec<&str> = v.list().iter().map(|r| r.identity.name.as_str()).collect();
        assert_eq!(
            order,
            vec!["c", "a", "b"],
            "先定位再 remove 会把 a 插到 b 后面"
        );
    }

    /// `before = None` = 挪到末尾。组内最后一行的下半区落点走这条。
    #[test]
    fn move_session_with_no_target_goes_to_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let mut ids = Vec::new();
        for n in ["a", "b", "c"] {
            let mut d = draft();
            d.identity.name = n.into();
            ids.push(v.add(d, "2026-08-16T00:00:00Z"));
        }
        v.move_session(ids[0], None, None).unwrap();
        let order: Vec<&str> = v.list().iter().map(|r| r.identity.name.as_str()).collect();
        assert_eq!(order, vec!["b", "c", "a"]);
    }

    /// 跨组拖动:顺带改 `group_id`。位置与组两件事一个入口做完 ——
    /// 分两次调用会在中间留下一个「已经改了组、还没挪位置」的可观察状态。
    #[test]
    fn move_session_across_groups_sets_the_group_too() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let gid = v.add_group("生产".into());
        let a = v.add(draft(), "2026-08-16T00:00:00Z");
        v.move_session(a, Some(gid), None).unwrap();
        assert_eq!(v.get(a).unwrap().identity.group_id, Some(gid));
    }

    /// 拖到自己身上 = 什么都不做,**不报错**。UI 侧已经挡了一道,
    /// 这里再挡一道:报错会让上层弹一个用户看不懂的失败提示。
    #[test]
    fn move_session_onto_itself_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let a = v.add(draft(), "2026-08-16T00:00:00Z");
        let b = v.add(draft(), "2026-08-16T00:00:00Z");
        v.move_session(a, None, Some(a)).unwrap();
        assert_eq!(
            v.list().iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![a, b]
        );
    }

    /// `before` 指向一条已经不存在的记录(别处刚删掉)→ 落到末尾,不报错。
    #[test]
    fn move_session_with_a_dangling_target_falls_back_to_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let a = v.add(draft(), "2026-08-16T00:00:00Z");
        let b = v.add(draft(), "2026-08-16T00:00:00Z");
        v.move_session(a, None, Some(SessionId(9999))).unwrap();
        assert_eq!(
            v.list().iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![b, a]
        );
    }

    /// 被拖的记录不存在 → `Err`。这一条是真错误(UI 手上的 id 来自本帧列表)。
    #[test]
    fn move_session_reports_a_missing_record() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        assert!(v.move_session(SessionId(9999), None, None).is_err());
    }

    /// 换位置不算「改了这条会话」——`modified_at` 不许动(同 `set_group` 的理由)。
    ///
    /// 自证会变红:在 `move_session` 里补一句写 `modified_at`。
    #[test]
    fn move_session_does_not_touch_modified_at() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let a = v.add(draft(), "2026-08-16T00:00:00Z");
        let b = v.add(draft(), "2026-08-16T00:00:00Z");
        let before = v.get(a).unwrap().modified_at.clone();
        v.move_session(a, None, Some(b)).unwrap();
        assert_eq!(v.get(a).unwrap().modified_at, before);
    }
}
