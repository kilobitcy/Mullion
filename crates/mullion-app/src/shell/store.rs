//! config-dir 解析 + `SessionStore`:app 侧对 `mullion_store::Vault` 的薄封装,
//! 额外提供「取会话 → 解密 secret → 映射成 SshConfig」的一步到位方法(供双击连接用)。
//! 时间戳由调用方(A2b 用 `time` crate)注入,保持本层可确定性测试。

use std::path::PathBuf;

use mullion_ssh::config::SshConfig;
use mullion_store::{MasterKeySource, SessionDraft, SessionId, SessionRecord, StoreError, Vault};

use super::session_map::{to_ssh_config, MapError};
use crate::ui::session_manager::SecretPresence;

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

    /// 走查 3:右键「移动到分组」。只改 `group_id`,见 `Vault::set_group`。
    pub fn set_group(
        &mut self,
        id: SessionId,
        group: Option<mullion_store::GroupId>,
    ) -> Result<(), StoreError> {
        self.vault.set_group(id, group)
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

    /// 取会话 → 用其(已解密的)secret 组 SshConfig(双击连接用)。
    ///
    /// 拨号链在这里物化:代理来自继承解析,跳板来自引用图展开。
    /// 跳板悬空/成环会在此**硬失败**——静默直连会让用户以为流量过了堡垒机(设计 §6)。
    pub fn ssh_config_for(&self, id: SessionId) -> Result<SshConfig, StoreOpenError> {
        let rec = self.vault.get(id).ok_or(StoreOpenError::NotFound(id))?;
        let secret = self.vault.secret(id);
        let mut cfg = to_ssh_config(rec, secret)?;

        let resolved = self.vault.resolve_for(id)?;
        let jumps = self.vault.expand_jump_chain(id)?;
        cfg.hops = super::dial_plan::build_hops_with_proxy_secret(
            resolved.proxy.as_ref(),
            &jumps,
            &|jid| self.vault.secret(jid).cloned(),
            secret,
        );
        Ok(cfg)
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
        let mut cfg = to_ssh_config(&rec, secret)?;

        let resolved = self.vault.resolve_layer(draft, draft.identity.group_id);
        let jumps = self.vault.expand_jump_chain_of(&resolved.jump)?;
        cfg.hops = super::dial_plan::build_hops_with_proxy_secret(
            resolved.proxy.as_ref(),
            &jumps,
            &|jid| self.vault.secret(jid).cloned(),
            secret,
        );
        Ok(cfg)
    }
}

/// 草稿 → 临时 `SessionRecord`,只为喂给 `to_ssh_config`。
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
            network: Default::default(),
            automation: Default::default(),
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
