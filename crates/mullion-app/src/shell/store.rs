//! config-dir 解析 + `SessionStore`:app 侧对 `mullion_store::Vault` 的薄封装,
//! 额外提供「取会话 → 解密 secret → 映射成 SshConfig」的一步到位方法(供双击连接用)。
//! 时间戳由调用方(A2b 用 `time` crate)注入,保持本层可确定性测试。

use std::path::PathBuf;

use mullion_ssh::config::SshConfig;
use mullion_store::{
    CredentialId, MasterKeySource, SessionDraft, SessionId, SessionRecord, StoreError, Vault,
};

use super::session_map::{to_dial_plan, DialPlan, MapError};
use crate::ui::session_manager::SecretPresence;

/// mullion 的配置目录(Windows `%APPDATA%\mullion\`、Linux `~/.config/mullion/`)。
/// 无法确定时返回 None(极少见,如无 HOME)。
pub fn config_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "mullion").map(|d| d.config_dir().to_path_buf())
}

/// F71:`<dir>/secrets.enc` 要不要主密码。
///
/// app 启动时**先问这个**再决定打不打开会话库(设计 D10):`mullion-store`
/// 零 UI,永远不会主动索要密码。探测失败(文件头读不懂)时**报错而不是
/// 当成「不用密码」**——那会拿钥匙串密钥去解一个主密码文件,报出来的是
/// 「密文损坏」,把真正的原因盖掉。
pub fn probe_needs_password(dir: &std::path::Path) -> Result<bool, StoreError> {
    Ok(Vault::probe_scheme(dir)?.has_password())
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

    /// F71:用主密码打开。密码错时 `Vault` 报的是 `WrongPassword`
    /// (不是 `Crypto`),调用方据此决定「让解锁框留着重试」还是「收掉弹窗报错」。
    pub fn unlock(dir: PathBuf, password: &str) -> Result<Self, StoreError> {
        Ok(Self {
            vault: Vault::open_with(dir, mullion_store::Unlock::Password(password))?,
        })
    }

    /// F71:当前这个库是不是主密码方案(设置弹窗显示「已设定 / 未设定」)。
    pub fn has_master_password(&self) -> bool {
        self.vault.has_master_password()
    }

    /// F71:设定或修改主密码。内部会立刻重写 `secrets.enc`。
    pub fn set_master_password(&mut self, password: &str) -> Result<(), StoreError> {
        self.vault.set_master_password(password)
    }

    /// F71:撤销主密码,回到钥匙串方案。
    pub fn clear_master_password(&mut self) -> Result<(), StoreError> {
        self.vault
            .clear_master_password(&mullion_store::KeyringSource::new())
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

    /// 走查 3:右键「移动到分组」。只改 `group_id`,见 `Vault::set_group`。
    pub fn set_group(
        &mut self,
        id: SessionId,
        group: Option<mullion_store::GroupId>,
    ) -> Result<(), StoreError> {
        self.vault.set_group(id, group)
    }

    /// F139:文件面板路径条上的 ☆ 收藏。见 `Vault::add_bookmark`(按路径去重)。
    pub fn add_bookmark(
        &mut self,
        id: SessionId,
        mark: mullion_store::Bookmark,
    ) -> Result<(), StoreError> {
        self.vault.add_bookmark(id, mark)
    }

    /// F139:取消收藏。见 `Vault::remove_bookmark`。
    pub fn remove_bookmark(&mut self, id: SessionId, path: &str) -> Result<(), StoreError> {
        self.vault.remove_bookmark(id, path)
    }

    /// F121:左栏拖拽排序。见 `Vault::move_session`。
    pub fn move_session(
        &mut self,
        id: SessionId,
        group: Option<mullion_store::GroupId>,
        before: Option<SessionId>,
    ) -> Result<(), StoreError> {
        self.vault.move_session(id, group, before)
    }

    pub fn save(&self) -> Result<(), StoreError> {
        self.vault.save()
    }

    pub fn groups(&self) -> &[mullion_store::GroupRecord] {
        self.vault.groups()
    }

    pub fn add_group(&mut self, name: String) -> mullion_store::GroupId {
        self.vault.add_group(name)
    }

    /// 共享凭据表(F74)。UI 拿它显示「有效用户名」与凭据档列表。
    pub fn credentials(&self) -> &[mullion_store::CredentialRecord] {
        self.vault.credentials()
    }

    /// F74 凭据 CRUD。与会话侧一样,写完由调用方 `save()` 落盘。
    pub fn add_credential(&mut self, draft: mullion_store::CredentialDraft) -> CredentialId {
        self.vault.add_credential(draft)
    }

    pub fn update_credential(
        &mut self,
        id: CredentialId,
        draft: mullion_store::CredentialDraft,
    ) -> Result<(), StoreError> {
        self.vault.update_credential(id, draft)
    }

    /// 删凭据。**被会话引用时拒绝**并回报引用者(设计 D7)——
    /// 静默删掉会让那些会话下次连接时整条报错。
    pub fn delete_credential(&mut self, id: CredentialId) -> Result<(), StoreError> {
        self.vault.delete_credential(id)
    }

    /// 哪些会话正引用这份凭据。删除确认框要照实列出来。
    pub fn sessions_using_credential(&self, id: CredentialId) -> Vec<SessionId> {
        self.vault.sessions_using(id)
    }

    pub fn tunnels(&self) -> &[mullion_store::TunnelRecord] {
        self.vault.tunnels()
    }

    /// F110 隧道 CRUD。与会话侧一样,写完由调用方 `save()` 落盘 ——
    /// 不在这里自动存,否则一次编辑会话的多步修改要写好几遍磁盘。
    pub fn add_tunnel(&mut self, draft: mullion_store::TunnelDraft) -> mullion_store::TunnelId {
        self.vault.add_tunnel(draft)
    }

    pub fn update_tunnel(
        &mut self,
        id: mullion_store::TunnelId,
        draft: mullion_store::TunnelDraft,
    ) -> Result<(), StoreError> {
        self.vault.update_tunnel(id, draft)
    }

    pub fn delete_tunnel(&mut self, id: mullion_store::TunnelId) -> Result<(), StoreError> {
        self.vault.delete_tunnel(id)
    }

    pub fn rename_group(&mut self, id: mullion_store::GroupId, name: String) -> bool {
        match self.vault.group_mut(id) {
            Some(g) => {
                g.name = name;
                true
            }
            None => false,
        }
    }

    /// 设置分组代理(F4)。分组只持有可继承字段,代理正是其中之一。
    pub fn set_group_proxy(
        &mut self,
        id: mullion_store::GroupId,
        proxy: Option<mullion_store::ProxyChoice>,
    ) {
        if let Some(g) = self.vault.group_mut(id) {
            g.network.proxy = proxy;
        }
    }

    pub fn delete_group(&mut self, id: mullion_store::GroupId) -> Result<(), StoreError> {
        self.vault.delete_group(id)
    }

    /// 解析后的配置(含继承来的代理/跳板)。
    pub fn resolved(&self, id: SessionId) -> Result<mullion_store::ResolvedConfig, StoreError> {
        self.vault.resolve_for(id)
    }

    /// 读一条会话的已存凭据。**返回明文**,只给保存路径的三态合成用
    /// (`app::apply_save`)——不要把它塞进 `UiFrame`,UI 层只该知道
    /// 「有没有设置」,那是 `secret_presence` 的职责。
    pub fn secret(&self, id: SessionId) -> Option<&mullion_store::model::SecretEntry> {
        self.vault.secret(id)
    }

    /// 只报告三个凭据槽位「有没有值」,不泄漏任何明文。UI 靠它决定密码框
    /// 显示「6 位黑点」还是「未设置」。
    pub fn secret_presence(&self, id: SessionId) -> SecretPresence {
        match self.vault.secret(id) {
            None => SecretPresence::default(),
            Some(s) => SecretPresence {
                password: s.password.is_some(),
                passphrase: s.passphrase.is_some(),
                proxy_password: s.proxy_password.is_some(),
                private_key: s.private_key.is_some(),
            },
        }
    }

    /// 读一份凭据的已存密文。**返回明文**,理由同上面的 `secret`:只给保存
    /// 路径的三态合成用(`app::apply_credential_save`)。
    pub fn credential_secret(
        &self,
        id: CredentialId,
    ) -> Option<&mullion_store::model::SecretEntry> {
        self.vault.credential_secret(id)
    }

    /// 凭据自己的密文存在情况(F74)。与 `secret_presence` 各自独立:
    /// 凭据没有代理口令那一格(设计 D4),那一位恒 false。
    pub fn credential_secret_presence(&self, id: CredentialId) -> SecretPresence {
        match self.vault.credential_secret(id) {
            None => SecretPresence::default(),
            Some(s) => SecretPresence {
                password: s.password.is_some(),
                passphrase: s.passphrase.is_some(),
                proxy_password: false,
                private_key: s.private_key.is_some(),
            },
        }
    }

    /// 取会话 → 用其(已解密的)secret 组 SshConfig(双击连接用)。
    ///
    /// 拨号链在这里物化:代理来自继承解析,跳板来自引用图展开。
    /// 跳板悬空/成环会在此**硬失败**——静默直连会让用户以为流量过了堡垒机(设计 §6)。
    ///
    /// 丢弃 `wants_sftp`——隧道那条调用(`start_tunnel`)永远只连 SSH 会话
    /// (D7:SFTP 不参与隧道),这一位对它没有意义。点「连接」需要这一位来
    /// 决定开终端标签还是文件标签,走 `dial_plan_for`(Task 10)。
    pub fn ssh_config_for(&self, id: SessionId) -> Result<SshConfig, StoreOpenError> {
        self.dial_plan_for(id).map(|(cfg, _)| cfg)
    }

    /// 同 `ssh_config_for`,多带回 `wants_sftp`(D24/F50:SFTP 节点连上后开
    /// sftp subsystem 而不是 PTY)。两者共用同一套解析内核,不是分叉的第二条
    /// 拨号逻辑——否则「哪条会正确映射」这类偏差迟早出现。
    pub fn dial_plan_for(&self, id: SessionId) -> Result<(SshConfig, bool), StoreOpenError> {
        let rec = self.vault.get(id).ok_or(StoreOpenError::NotFound(id))?;
        let secret = self.vault.secret(id);
        // F74:先解析身份(引用的凭据可能悬空 → 在这里 `?` 出去),再物化。
        let auth = self.vault.resolve_auth(rec)?;
        let DialPlan {
            mut cfg,
            wants_sftp,
        } = to_dial_plan(rec, &auth)?;

        let resolved = self.vault.resolve_for(id)?;
        let jumps = self.vault.expand_jump_chain(id)?;
        let hops = self.resolve_jumps(&jumps)?;
        cfg.hops = super::dial_plan::build_hops_with_proxy_secret(
            resolved.proxy.as_ref(),
            &hops,
            // 代理口令始终取**目标会话**的侧车,不跟着凭据走(设计 D4)。
            secret,
        );
        Ok((cfg, wants_sftp))
    }

    /// 每一跳都解析成「跳板记录 + 它自己的身份」。任何一跳引用的凭据悬空,
    /// 整条链就地失败 —— 与跳板悬空同一处置(设计 D6)。
    fn resolve_jumps<'a>(
        &self,
        jumps: &'a [SessionRecord],
    ) -> Result<Vec<super::dial_plan::Jump<'a>>, StoreError> {
        jumps
            .iter()
            .map(|rec| {
                Ok(super::dial_plan::Jump {
                    rec,
                    auth: self.vault.resolve_auth(rec)?,
                })
            })
            .collect()
    }

    /// 按**草稿**(含未保存改动)组 SshConfig。「测试连接」(F92)用。
    ///
    /// 与 `ssh_config_for` 走同一套解析内核(`resolve_layer` /
    /// `expand_jump_chain_of`),只是入口从「库里的 id」换成「手上的草稿」。
    /// 不是第二条解析路径 —— 否则「测试通过、保存后连不上」这种最伤
    /// 信任的 bug 迟早出现。
    ///
    /// 跳板悬空/成环同样**硬失败**:拨测的价值就在于提前把这些问题炸出来。
    pub fn ssh_config_for_draft(&self, draft: &SessionDraft) -> Result<SshConfig, StoreOpenError> {
        let rec = draft_to_record(draft);
        let secret = draft.secret.as_ref();
        // 草稿没有 id,查不到「自己的」密文,所以走参数化内核:手上的这份
        // `auth` + 手上的这份 secret(F74 设计 D5)。
        let auth = self.vault.resolve_auth_of(&draft.auth, secret)?;
        // 同上:`wants_sftp` 先丢弃,Task 10 才真正接线。
        let DialPlan { mut cfg, .. } = to_dial_plan(&rec, &auth)?;

        let resolved = self.vault.resolve_layer(draft, draft.identity.group_id);
        let jumps = self.vault.expand_jump_chain_of(&resolved.jump)?;
        let hops = self.resolve_jumps(&jumps)?;
        cfg.hops =
            super::dial_plan::build_hops_with_proxy_secret(resolved.proxy.as_ref(), &hops, secret);
        Ok(cfg)
    }
}

/// 草稿 → 临时 `SessionRecord`,只为喂给 `to_dial_plan`。
/// 它只读 `connection` / `auth`,`id` 与 `modified_at` 是占位,
/// 不入库、不外泄 —— 草稿本来就还没有 id。
fn draft_to_record(d: &SessionDraft) -> SessionRecord {
    SessionRecord {
        id: SessionId(0),
        modified_at: String::new(),
        identity: d.identity.clone(),
        connection: d.connection.clone(),
        auth: d.auth.clone(),
        terminal: d.terminal.clone(),
        appearance: d.appearance.clone(),
        network: d.network.clone(),
        automation: d.automation.clone(),
        sftp: Default::default(),
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
            auth: Auth::inline("user", AuthKind::Password),
            terminal: Default::default(),
            appearance: Default::default(),
            network: Default::default(),
            automation: Default::default(),
            sftp: Default::default(),
            secret: Some(SecretEntry {
                password: Some("pw".into()),
                passphrase: None,
                proxy_password: None,
                private_key: None,
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

    #[test]
    fn group_crud_is_reachable_from_app_layer() {
        let dir = tempfile::tempdir().unwrap();
        let mut store =
            SessionStore::open(dir.path().to_path_buf(), &InMemoryKey([1u8; 32])).unwrap();
        let gid = store.add_group("生产".into());
        assert_eq!(store.groups().len(), 1);
        assert_eq!(store.groups()[0].name, "生产");
        store.delete_group(gid).unwrap();
        assert!(store.groups().is_empty());
    }

    /// 会话经分组继承来的代理,必须一路落到 `SshConfig.hops`。
    #[test]
    fn ssh_config_carries_hops_from_inherited_proxy() {
        let dir = tempfile::tempdir().unwrap();
        let mut store =
            SessionStore::open(dir.path().to_path_buf(), &InMemoryKey([1u8; 32])).unwrap();
        let gid = store.add_group("生产".into());
        store.set_group_proxy(
            gid,
            Some(mullion_store::ProxyChoice::Socks5(
                mullion_store::ProxyEndpoint {
                    host: "127.0.0.1".into(),
                    port: 7891,
                    user: None,
                },
            )),
        );
        let mut d = draft();
        d.identity.group_id = Some(gid);
        let id = store.add(d, "2026-07-31T00:00:00Z");

        let cfg = store.ssh_config_for(id).unwrap();
        assert_eq!(cfg.hops.len(), 1, "继承来的代理应成为一跳");
        assert!(matches!(cfg.hops[0], mullion_ssh::hop::Hop::Socks5 { .. }));
    }

    /// 配了跳板的会话,`ssh_config_for` 产出的拨号链必须真的带上跳板——这是本切片
    /// 最严重的事故模型(用户以为流量过了堡垒机,实际直连目标)的兜底。
    /// `ssh_config_carries_hops_from_inherited_proxy` 只覆盖了「代理继承」这条路径,
    /// 跳板走的是另一条(`expand_jump_chain` + `jump_auth`),两条路径都要被
    /// `ssh_config_for` 这个唯一生产入口正确覆盖,缺一不可。
    #[test]
    fn ssh_config_carries_hops_from_configured_jump_chain() {
        let dir = tempfile::tempdir().unwrap();
        let mut store =
            SessionStore::open(dir.path().to_path_buf(), &InMemoryKey([1u8; 32])).unwrap();

        let mut bastion = draft();
        bastion.identity.name = "bastion".into();
        bastion.connection.host = "203.0.113.1".into();
        let bastion_id = store.add(bastion, "2026-07-31T00:00:00Z");

        let mut d = draft();
        d.network = mullion_store::NetworkPrefs {
            proxy: None,
            jump: Some(vec![mullion_store::JumpRef(bastion_id)]),
        };
        let id = store.add(d, "2026-07-31T00:00:00Z");

        let cfg = store.ssh_config_for(id).unwrap();
        assert_eq!(
            cfg.hops.len(),
            1,
            "配置了跳板就必须体现在拨号链里,绝不能静默直连"
        );
        match &cfg.hops[0] {
            mullion_ssh::hop::Hop::SshJump { host, .. } => assert_eq!(host, "203.0.113.1"),
            other => panic!("跳板应物化成 SshJump,实际: {other:?}"),
        }
    }

    /// 跳板悬空必须硬失败,不许静默直连(安全属性,设计 §6)。
    #[test]
    fn dangling_jump_makes_connect_fail_instead_of_going_direct() {
        let dir = tempfile::tempdir().unwrap();
        let mut store =
            SessionStore::open(dir.path().to_path_buf(), &InMemoryKey([1u8; 32])).unwrap();
        let mut d = draft();
        d.network = mullion_store::NetworkPrefs {
            proxy: None,
            jump: Some(vec![mullion_store::JumpRef(mullion_store::SessionId(999))]),
        };
        let id = store.add(d, "2026-07-31T00:00:00Z");
        assert!(
            store.ssh_config_for(id).is_err(),
            "悬空跳板必须报错,绝不能悄悄直连"
        );
    }

    /// F92:拨测组的是**手上这份草稿**,不是库里存着的旧值。
    ///
    /// 场景:用户打开已存会话、把 host 改成新地址、还没点保存就点「测试连接」。
    /// 若实现退化成按 id 去库里查(`ssh_config_for`),测的就是老机器 ——
    /// 用户会得到一个与他眼前表单无关的结论。
    ///
    /// 自证变红的方式:把 `ssh_config_for_draft` 的函数体改成
    /// `self.ssh_config_for(SessionId(1))`(即按库里的记录组),
    /// 而不是改测试里传的 host。
    #[test]
    fn draft_config_uses_unsaved_edits_not_the_stored_record() {
        let dir = tempfile::tempdir().unwrap();
        let mut store =
            SessionStore::open(dir.path().to_path_buf(), &InMemoryKey([1u8; 32])).unwrap();
        store.add(draft(), "2026-08-04T00:00:00Z");

        // 表单上把 host 改了,但没保存。
        let mut edited = draft();
        edited.connection.host = "198.51.100.7".into();

        let cfg = store.ssh_config_for_draft(&edited).unwrap();
        assert_eq!(cfg.host, "198.51.100.7", "拨测必须用草稿里的新 host");
        // 库里那条一点没动 —— 拨测是只读的。
        assert_eq!(store.list()[0].connection.host, "192.0.2.10");
    }

    /// F74:引用共享凭据的会话,拨号时用的是**凭据里的**用户名与密码,
    /// 不是会话自己身上那份(它压根没有)。
    ///
    /// 自证变红的方式:把 `dial_plan_for` 里的 `resolve_auth(rec)?` 换回
    /// 直接读会话记录 —— 引用型会话根本给不出 user/kind,那条路走不通,
    /// 而这正是本测试要钉住的事实。
    #[test]
    fn a_session_referencing_a_credential_dials_with_the_credentials_identity() {
        let dir = tempfile::tempdir().unwrap();
        let mut store =
            SessionStore::open(dir.path().to_path_buf(), &InMemoryKey([1u8; 32])).unwrap();
        let cid = store.add_credential(mullion_store::CredentialDraft {
            name: "运维".into(),
            user: "ops".into(),
            kind: AuthKind::Password,
            secret: Some(SecretEntry {
                password: Some("shared-pw".into()),
                passphrase: None,
                proxy_password: None,
                private_key: None,
            }),
        });

        let mut d = draft();
        d.auth = Auth::Ref(cid);
        // 会话自己那份密文里放一个**不一样**的密码:被拿去用就说明串味了。
        d.secret = Some(SecretEntry {
            password: Some("session-pw".into()),
            passphrase: None,
            proxy_password: None,
            private_key: None,
        });
        let id = store.add(d, "2026-08-13T00:00:00Z");

        let cfg = store.ssh_config_for(id).unwrap();
        assert_eq!(cfg.user, "ops", "用户名必须来自凭据");
        assert!(
            matches!(cfg.auth, AuthMethod::Password(p) if p == "shared-pw"),
            "密码必须来自凭据的侧车"
        );
    }

    /// 跳板引用共享凭据时,那一跳也得拿凭据的身份 —— 跳板的解析与目标会话
    /// 走的是同一个内核,不是只给目标会话接了线。
    #[test]
    fn a_jump_referencing_a_credential_carries_the_credentials_key() {
        let dir = tempfile::tempdir().unwrap();
        let mut store =
            SessionStore::open(dir.path().to_path_buf(), &InMemoryKey([1u8; 32])).unwrap();
        let cid = store.add_credential(mullion_store::CredentialDraft {
            name: "堡垒机私钥".into(),
            user: "jump-ops".into(),
            kind: AuthKind::PublicKey {
                has_passphrase: false,
            },
            secret: Some(SecretEntry {
                password: None,
                passphrase: None,
                proxy_password: None,
                private_key: Some("KEYBODY".into()),
            }),
        });

        let mut bastion = draft();
        bastion.identity.name = "bastion".into();
        bastion.connection.host = "203.0.113.1".into();
        bastion.auth = Auth::Ref(cid);
        bastion.secret = None;
        let bastion_id = store.add(bastion, "2026-08-13T00:00:00Z");

        let mut d = draft();
        d.network = mullion_store::NetworkPrefs {
            proxy: None,
            jump: Some(vec![mullion_store::JumpRef(bastion_id)]),
        };
        let id = store.add(d, "2026-08-13T00:00:00Z");

        let cfg = store.ssh_config_for(id).unwrap();
        match &cfg.hops[0] {
            mullion_ssh::hop::Hop::SshJump {
                user,
                auth: AuthMethod::PublicKey { key_data, .. },
                ..
            } => {
                assert_eq!(user, "jump-ops");
                assert_eq!(key_data, "KEYBODY");
            }
            other => panic!("跳板应带上凭据的私钥,实际: {other:?}"),
        }
    }

    /// 悬空的凭据引用必须让整条 `ssh_config_for` 报错,**绝不降级**。
    ///
    /// 与跳板悬空同样的道理:静默退回 agent / 空口令,用户看到的是一条
    /// 指不到原因的 AuthFailed,甚至可能以另一个身份连上去。
    ///
    /// 自证变红的方式:把 `Vault::resolve_auth_of` 里 `Auth::Ref` 那支的
    /// `ok_or(DanglingCredential)?` 换成「查不到就当成空 inline」。
    #[test]
    fn a_dangling_credential_reference_fails_the_whole_dial() {
        let dir = tempfile::tempdir().unwrap();
        let mut store =
            SessionStore::open(dir.path().to_path_buf(), &InMemoryKey([1u8; 32])).unwrap();
        let mut d = draft();
        d.auth = Auth::Ref(mullion_store::CredentialId(999));
        let id = store.add(d, "2026-08-13T00:00:00Z");
        // 断言到**具体变体**,不是笼统的 `is_err()`:降级成「空身份」照样会
        // 因为缺密码而报 `MissingSecret`,笼统断言分辨不出这两种,变异跑不红
        // (第一次写成 `is_err()` 时正是这样漏过去的)。
        assert!(
            matches!(
                store.ssh_config_for(id),
                Err(StoreOpenError::Store(StoreError::DanglingCredential(c)))
                    if c == mullion_store::CredentialId(999)
            ),
            "悬空凭据必须报「凭据不存在」,不能降级成别的失败"
        );
        // 草稿路径(F92 拨测)同样不许降级 —— 两条路共用一个解析内核。
        let mut draft_ref = draft();
        draft_ref.auth = Auth::Ref(mullion_store::CredentialId(999));
        assert!(
            matches!(
                store.ssh_config_for_draft(&draft_ref),
                Err(StoreOpenError::Store(StoreError::DanglingCredential(_)))
            ),
            "拨测遇到悬空凭据也必须报同一个错"
        );
    }

    /// F92 + 安全:草稿里的跳板引用悬空时必须**硬失败**。
    ///
    /// 静默降级成直连是安全事故:用户配了堡垒机、拨测报「成功」,
    /// 他会以为流量过了堡垒机,实际是裸连到目标机。
    ///
    /// 自证变红的方式:把 `ssh_config_for_draft` 里 `expand_jump_chain_of(..)?`
    /// 的 `?` 换成 `.unwrap_or_default()`,而不是改测试里的 SessionId。
    #[test]
    fn draft_config_hard_fails_on_dangling_jump_instead_of_silently_going_direct() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path().to_path_buf(), &InMemoryKey([1u8; 32])).unwrap();

        let mut d = draft();
        d.network = mullion_store::NetworkPrefs {
            proxy: None,
            jump: Some(vec![mullion_store::JumpRef(mullion_store::SessionId(999))]),
        };

        let err = store.ssh_config_for_draft(&d).unwrap_err();
        assert!(
            matches!(
                err,
                StoreOpenError::Store(mullion_store::StoreError::JumpDangling(_))
            ),
            "悬空跳板必须硬失败,实际:{err:?}"
        );
    }
}
