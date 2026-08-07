//! 会话编辑表单的**纯逻辑**:表单缓冲、表单 → `SessionDraft` 的转换、凭据三态合成。
//!
//! **本文件不许 `use egui`**。会话表单的 bug(端口解析、代理三态、凭据被静默清除)
//! 全部能在没有窗口的情况下单测复现——这是把它从 UI 代码里切出来的全部理由。

use std::path::Path;

use mullion_store::{
    AppearancePrefs, Auth, AuthKind, AutomationPrefs, Connection, GroupId, Identity, NetworkPrefs,
    Protocol, SecretEntry, SessionDraft, SessionId, SessionRecord, TerminalPrefs,
};

/// 编辑表单里认证方式的选择。不复用 `AuthKind` 本身,因为 UI 在密码/公钥两种模式
/// 间切换时要各自保留自己的缓冲(密码框内容、私钥路径都不该因切换选项就丢)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthKindUi {
    Password,
    PublicKey,
}

/// 编辑表单里的代理选择。**四态**,不是三态:
/// 「跟随分组」与「不使用代理」必须分开,前者是不设置(继承),
/// 后者是显式 `Direct`(覆盖分组)。合并二者会让用户无法在有分组代理时单独直连。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxyModeUi {
    Inherit,
    Direct,
    Socks5,
    HttpConnect,
}

/// 编辑表单里的跳板选择。**三态**,与 `NetworkPrefs::jump` 的三态一一对应:
/// 无 = `Some(vec![])`(显式直连,覆盖分组)/ 继承分组 = `None` / 自定义 = `Some(chain)`。
///
/// 与 `ProxyModeUi` 同一个坑:「继承」与「显式不走跳板」必须分开,否则分组配了
/// 跳板时,用户没法单独让某一条会话直连。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JumpModeUi {
    /// 不走跳板(显式覆盖分组)。**新建会话的默认**。
    None,
    Inherit,
    Custom,
}

/// 编辑表单的跨帧字段缓冲。端口用 `String`(保存时才 `parse`),密码/口令是明文
/// 缓冲(仅存在于本进程内存,保存后随 `SaveIntent` 一次性转移给 store 加密)。
///
/// **不 derive Debug**:`password`/`passphrase`/`proxy_password` 三个字段是明文,
/// derive 会让 `{:?}` 把它们打印出来。目前全仓没有调用点,但属休眠风险
/// (对照 `mullion_ssh::hop::Hop` 同样的手写打码 Debug)。见下方手写实现。
#[derive(Clone, PartialEq)]
pub struct EditorBuffer {
    pub name: String,
    pub host: String,
    pub port: String,
    pub protocol: Protocol,
    pub user: String,
    pub note: String,
    pub auth_kind: AuthKindUi,
    pub password: String,
    /// 用户是否碰过密码框。未碰 = `SecretField::Keep`(已存值保留)。
    /// 编辑已有会话时密码框恒为空(store 不回吐明文),没有这一位就区分不了
    /// 「没动」和「清空了」——见 `SecretField` 的说明。
    pub password_touched: bool,
    /// 私钥**正文**的明文缓冲(v5)。只在「导入」那一刻被填满;编辑已有会话时
    /// 恒为空 —— store 不回吐明文,UI 只从 `SecretPresence::private_key` 知道
    /// 「已经有一把钥匙」。路径不再进表单:它既不是凭据也不是身份,却让会话跟
    /// 一台机器上的一个文件绑死。
    pub key_data: String,
    /// 用户这一轮是否导入/清除过私钥。语义同 `password_touched`。
    pub key_touched: bool,
    /// 导入操作的一行提示(「已导入 id_ed25519」/「这看起来是公钥」)。
    /// **瞬态**:`mod.rs` 每帧把它抽走转成 `UiState::key_drop_note`,
    /// 不能留在缓冲里参与 `is_dirty` 比对 —— 导入失败不该把表单判成脏。
    pub key_note: Option<String>,
    pub passphrase: String,
    pub passphrase_touched: bool,

    pub proxy_mode: ProxyModeUi,
    pub proxy_host: String,
    pub proxy_port: String,
    pub proxy_user: String,
    pub proxy_password: String,
    pub proxy_password_touched: bool,
    /// 跳板链,按拨号顺序。仅在 `jump_mode == Custom` 时写回。
    /// 切到「无」/「继承」时**不清空**:同 `AuthKindUi` 的缓冲逻辑,
    /// 用户切回「自定义」应看到自己刚才配的链,而不是从头再点一遍。
    pub jump_chain: Vec<SessionId>,
    pub jump_mode: JumpModeUi,

    // ↓↓↓ 透传字段:UI 目前没有编辑标签/终端偏好/外观偏好的入口(分组自
    // P0-b 起已可编辑,见下方 preserved_group_id 的注),但
    // `Vault::update` 对 `identity`/`terminal`/`appearance` 是整体字段替换
    // 而非合并(见 vault.rs)。所以编辑表单必须把这些字段原样存下来再原样写回,
    // 否则「编辑会话」会静默清空它们。新建会话时没有 `SessionRecord` 可读,
    // 保持默认值(未分组/无标签/默认偏好)。
    // (`network` 分节曾经也在这份透传名单里,现在有了 proxy_mode/jump_chain
    // 等真正的编辑字段,不再需要盲目透传。)
    // 注:`preserved_group_id` 自 P0-b 起可由编辑器下拉修改,名字沿用未改以免波及守护测试。
    pub preserved_group_id: Option<GroupId>,
    pub preserved_tags: Vec<String>,
    pub preserved_terminal: TerminalPrefs,
    pub preserved_appearance: AppearancePrefs,
    pub preserved_automation: AutomationPrefs,

    /// 「浏览…」按钮本帧被点了。`mod.rs` 在借用释放后转成
    /// `UiState::pick_key_request`,随即复位。
    pub pick_key_clicked: bool,
}

impl Default for EditorBuffer {
    fn default() -> Self {
        Self {
            name: String::new(),
            host: String::new(),
            port: "22".to_string(),
            protocol: Protocol::Ssh,
            user: String::new(),
            note: String::new(),
            auth_kind: AuthKindUi::Password,
            password: String::new(),
            password_touched: false,
            key_data: String::new(),
            key_touched: false,
            key_note: None,
            passphrase: String::new(),
            passphrase_touched: false,
            proxy_mode: ProxyModeUi::Inherit,
            proxy_host: String::new(),
            proxy_port: "1080".to_string(),
            proxy_user: String::new(),
            proxy_password: String::new(),
            proxy_password_touched: false,
            jump_chain: Vec::new(),
            jump_mode: JumpModeUi::None,
            preserved_group_id: None,
            preserved_tags: Vec::new(),
            preserved_terminal: TerminalPrefs::default(),
            preserved_appearance: AppearancePrefs::default(),
            preserved_automation: AutomationPrefs::default(),
            pick_key_clicked: false,
        }
    }
}

/// 表单是否相对基线快照有改动。
///
/// 基线 = 打开这条会话时 `EditorBuffer::from_record` 的产物。整体比对而不是
/// 「按过键就算脏」:用户改完又改回来不该弹确认,弹多了用户就条件反射点
/// 「丢弃」,这个确认也就白设了。三个 `*_touched` 位一起参与比对 ——
/// 「点进密码框再清空」文本上看不出差别,意图上却是「清除凭据」。
pub(crate) fn is_dirty(buf: &EditorBuffer, baseline: &EditorBuffer) -> bool {
    buf != baseline
}

/// 左栏点击想切到哪里。切换前若表单是脏的,先弹确认,确认后再消费它。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchTarget {
    Session(SessionId),
    /// 「新建」按钮:切到一张空白草稿。
    NewDraft,
}

fn redacted(s: &str) -> &'static str {
    if s.is_empty() {
        "<空>"
    } else {
        "<已设置>"
    }
}

impl std::fmt::Debug for EditorBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditorBuffer")
            .field("name", &self.name)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("protocol", &self.protocol)
            .field("user", &self.user)
            .field("note", &self.note)
            .field("auth_kind", &self.auth_kind)
            .field("password", &redacted(&self.password))
            .field("password_touched", &self.password_touched)
            .field("key_data", &redacted(&self.key_data))
            .field("key_touched", &self.key_touched)
            .field("passphrase", &redacted(&self.passphrase))
            .field("passphrase_touched", &self.passphrase_touched)
            .field("proxy_mode", &self.proxy_mode)
            .field("proxy_host", &self.proxy_host)
            .field("proxy_port", &self.proxy_port)
            .field("proxy_user", &self.proxy_user)
            .field("proxy_password", &redacted(&self.proxy_password))
            .field("proxy_password_touched", &self.proxy_password_touched)
            .field("jump_chain", &self.jump_chain)
            .field("jump_mode", &self.jump_mode)
            .field("preserved_group_id", &self.preserved_group_id)
            .field("preserved_tags", &self.preserved_tags)
            .field("preserved_terminal", &self.preserved_terminal)
            .field("preserved_appearance", &self.preserved_appearance)
            .field("preserved_automation", &self.preserved_automation)
            .finish()
    }
}

impl EditorBuffer {
    /// 把已有会话的非敏感字段填入表单(密码/口令 store 不会明文回吐,留空 ——
    /// 编辑时留空 = 不改;见 `build_draft` 的说明)。
    pub(crate) fn from_record(rec: &SessionRecord) -> Self {
        let mut buf = Self {
            name: rec.identity.name.clone(),
            host: rec.connection.host.clone(),
            port: rec.connection.port.to_string(),
            protocol: rec.connection.protocol,
            user: rec.auth.user.clone(),
            note: rec.identity.note.clone(),
            preserved_group_id: rec.identity.group_id,
            preserved_tags: rec.identity.tags.clone(),
            preserved_terminal: rec.terminal.clone(),
            preserved_appearance: rec.appearance.clone(),
            preserved_automation: rec.automation.clone(),
            ..Self::default()
        };
        match &rec.network.proxy {
            None => buf.proxy_mode = ProxyModeUi::Inherit,
            Some(mullion_store::ProxyChoice::Direct) => buf.proxy_mode = ProxyModeUi::Direct,
            Some(mullion_store::ProxyChoice::Socks5(ep)) => {
                buf.proxy_mode = ProxyModeUi::Socks5;
                buf.proxy_host = ep.host.clone();
                buf.proxy_port = ep.port.to_string();
                buf.proxy_user = ep.user.clone().unwrap_or_default();
            }
            Some(mullion_store::ProxyChoice::HttpConnect(ep)) => {
                buf.proxy_mode = ProxyModeUi::HttpConnect;
                buf.proxy_host = ep.host.clone();
                buf.proxy_port = ep.port.to_string();
                buf.proxy_user = ep.user.clone().unwrap_or_default();
            }
        }
        // 三态回填。注意 `Some(vec![])` → 「无」而不是「自定义 + 空链」:
        // 二者写回时等价,但 UI 上「自定义但一跳都没配」是个说不清的中间态。
        buf.jump_mode = match &rec.network.jump {
            None => JumpModeUi::Inherit,
            Some(chain) if chain.is_empty() => JumpModeUi::None,
            Some(chain) => {
                buf.jump_chain = chain.iter().map(|j| j.0).collect();
                JumpModeUi::Custom
            }
        };
        // 公钥会话不回填任何私钥信息:正文是明文凭据,store 不回吐;路径 v5 起
        // 已不存在。UI 靠 `SecretPresence::private_key` 显示「已导入 / 未设置」。
        match &rec.auth.kind {
            AuthKind::Password => buf.auth_kind = AuthKindUi::Password,
            AuthKind::PublicKey { .. } => buf.auth_kind = AuthKindUi::PublicKey,
        }
        buf
    }
}

/// 一个密码框的**三态**意图。
///
/// store 的 `Option<String>` 只有二态(有值 / 无值),保存时无法区分「用户没动」
/// 和「用户清空了」——这正是 F73 那个「编辑一下会话密码就没了」的根因。
/// UI 层用三态表达意图,由 `merge_secret` 落回二态。
#[derive(Clone, PartialEq, Eq)]
pub enum SecretField {
    /// 用户没碰这个框 → 已存值原样保留。
    Keep,
    /// 用户输入了新值 → 覆盖。
    Set(String),
    /// 用户把框清空了 → 删除已存值。
    Clear,
}

/// 手写打码 Debug:`Set` 里是明文口令,`{:?}` 一打就写进日志/panic 消息,
/// 加密存储当场归零(与 `mullion_store::model::SecretEntry` 同一条红线)。
impl std::fmt::Debug for SecretField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SecretField::Keep => f.write_str("Keep"),
            SecretField::Set(_) => f.write_str("Set(<已设置>)"),
            SecretField::Clear => f.write_str("Clear"),
        }
    }
}

/// 把三态意图落回 store 的二态 `Option<SecretEntry>`。纯函数。
///
/// 四个字段各自独立合成;全为 `None` 时整条收成 `None`,不在 secrets.enc 里
/// 留全空的壳。
pub(crate) fn merge_secret(
    existing: Option<&SecretEntry>,
    password: &SecretField,
    passphrase: &SecretField,
    proxy_password: &SecretField,
    private_key: &SecretField,
) -> Option<SecretEntry> {
    fn one(existing: Option<&String>, f: &SecretField) -> Option<String> {
        match f {
            SecretField::Keep => existing.cloned(),
            SecretField::Set(v) => Some(v.clone()),
            SecretField::Clear => None,
        }
    }
    let merged = SecretEntry {
        password: one(existing.and_then(|e| e.password.as_ref()), password),
        passphrase: one(existing.and_then(|e| e.passphrase.as_ref()), passphrase),
        proxy_password: one(
            existing.and_then(|e| e.proxy_password.as_ref()),
            proxy_password,
        ),
        private_key: one(existing.and_then(|e| e.private_key.as_ref()), private_key),
    };
    if merged.password.is_none()
        && merged.passphrase.is_none()
        && merged.proxy_password.is_none()
        && merged.private_key.is_none()
    {
        None
    } else {
        Some(merged)
    }
}

/// 让 `AuthKind::PublicKey { has_passphrase }` 跟**合成后**的凭据一致。
///
/// 它不能跟表单当前内容走:编辑已有会话时口令框恒为空(store 不回吐明文),
/// 跟着表单走会把 has_passphrase 写成 false,下次连接时 russh 拿到加密私钥
/// 却不知道要口令。密码认证(`AuthKind::Password`)没有这个字段,原样跳过。
pub(crate) fn sync_has_passphrase(draft: &mut SessionDraft, merged: Option<&SecretEntry>) {
    if let AuthKind::PublicKey { has_passphrase, .. } = &mut draft.auth.kind {
        *has_passphrase = merged.is_some_and(|s| s.passphrase.is_some());
    }
}

/// 表单缓冲 → 四个凭据槽位各自的三态意图。纯函数。
///
/// 当前认证方式用不到的那一支走 `Clear` 而不是 `Keep`:密码认证的会话不该在
/// secrets.enc 里留一条孤儿私钥口令/私钥(这也与改造前 `build_draft` 的行为一致)。
pub(crate) fn secret_fields(
    buf: &EditorBuffer,
) -> (SecretField, SecretField, SecretField, SecretField) {
    fn field(touched: bool, v: &str) -> SecretField {
        if !touched {
            SecretField::Keep
        } else if v.is_empty() {
            SecretField::Clear
        } else {
            SecretField::Set(v.to_string())
        }
    }
    let (password, passphrase, private_key) = match buf.auth_kind {
        AuthKindUi::Password => (
            field(buf.password_touched, &buf.password),
            SecretField::Clear,
            SecretField::Clear,
        ),
        AuthKindUi::PublicKey => (
            SecretField::Clear,
            field(buf.passphrase_touched, &buf.passphrase),
            field(buf.key_touched, &buf.key_data),
        ),
    };
    (
        password,
        passphrase,
        field(buf.proxy_password_touched, &buf.proxy_password),
        private_key,
    )
}

/// 导入一个私钥文件:读正文填进缓冲,并留一行提示给用户看。
///
/// IO 由调用方注入(生产是 `std::fs::read_to_string`),「读不了 / 选成了公钥 /
/// 正常导入」三条分支因此都能脱离 GUI 单测。
pub(crate) fn import_key_file(
    buf: &mut EditorBuffer,
    path: &Path,
    read: impl FnOnce(&Path) -> std::io::Result<String>,
) {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    match read(path) {
        Err(e) => buf.key_note = Some(format!("读不了 {name}:{e}")),
        Ok(text) if !looks_like_private_key(&text) => {
            buf.key_note = Some(format!(
                "{name} 不像私钥 —— 要选的是私钥本体,不是 .pub 公钥"
            ));
        }
        Ok(text) => {
            buf.key_data = text;
            buf.key_touched = true;
            buf.key_note = Some(format!("已导入 {name}"));
        }
    }
}

/// 只认 PEM / OpenSSH 私钥的起始标记。**故意不做真解析**:带口令的私钥要先
/// 有口令才解得开,用 `decode_secret_key` 去验会把正常的加密私钥判成坏文件。
/// 这里只拦最常见的一种误操作 —— 选成了 `id_xxx.pub`。
fn looks_like_private_key(text: &str) -> bool {
    text.contains("PRIVATE KEY")
}

/// 清除表单里的私钥(「清除」按钮)。保存后 `SecretField::Clear` 会把侧车里
/// 那把钥匙一起删掉。
pub(crate) fn clear_key(buf: &mut EditorBuffer) {
    buf.key_data.clear();
    buf.key_touched = true;
    buf.key_note = Some("已清除私钥(保存后生效)".to_string());
}

/// 四个凭据槽位「有没有值」。**只有 bool,不含任何明文** —— 它要穿过
/// `UiFrame` 进 egui 闭包,明文绝不能走这条路。
/// 必须 `Copy`:`UiFrame` 整体 `Copy`(`egui::Context::run` 内部是个 loop)。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SecretPresence {
    pub password: bool,
    pub passphrase: bool,
    pub proxy_password: bool,
    /// 库里有没有一把已导入的私钥(v5)。为 false 的公钥会话要在 UI 上标红:
    /// v4→v5 迁移时读不到旧路径的会话就落在这里,不提示用户就只剩连接时报错。
    pub private_key: bool,
}

/// 一次「保存」的意图:app 事后据此调用 `store.add`(`editing_id=None`)或
/// `store.update`(`Some(id)`)。
///
/// `draft.secret` 里装的是「库里原本没有凭据时」的合成结果,**不是最终值**——
/// `app::apply_save` 会用真实的已存凭据重算一遍并覆盖它(见 `merge_secret`)。
pub struct SaveIntent {
    pub editing_id: Option<SessionId>,
    pub draft: SessionDraft,
    pub password: SecretField,
    pub passphrase: SecretField,
    pub proxy_password: SecretField,
    pub private_key: SecretField,
    /// 保存成功后立刻连接(右栏底部的「保存并连接」)。
    pub then_connect: bool,
}

/// 表单缓冲 → `SessionDraft`。纯函数,不碰 egui,可脱离 GUI 单测。
///
/// 密码认证:密码框留空 → `secret=None`(= 清除已存凭据,留空的语义在 UI 上有提示)。
/// 公钥认证:`has_passphrase` 由口令框是否非空决定;口令非空才带 `secret`。
pub(crate) fn build_draft(buf: &EditorBuffer) -> Result<SessionDraft, String> {
    let port: u16 = buf
        .port
        .trim()
        .parse()
        .map_err(|_| "端口非法,须为 1-65535 的整数".to_string())?;
    let kind = match buf.auth_kind {
        AuthKindUi::Password => AuthKind::Password,
        AuthKindUi::PublicKey => AuthKind::PublicKey {
            // 占位;下面用合成结果统一修正,避免两处各算一遍算歪。
            has_passphrase: false,
        },
    };
    // 这里传 `existing = None`:`build_draft` 看不到 store,它产出的是「若库里
    // 原本没有凭据时的合成结果」。真正的合成在 `app::apply_save` 里用真实
    // existing 重算一遍并覆盖 —— 编辑已有会话时以那一次为准。
    let (pw_f, pp_f, proxy_f, key_f) = secret_fields(buf);
    let secret = merge_secret(None, &pw_f, &pp_f, &proxy_f, &key_f);
    let proxy = match buf.proxy_mode {
        ProxyModeUi::Inherit => None,
        ProxyModeUi::Direct => Some(mullion_store::ProxyChoice::Direct),
        ProxyModeUi::Socks5 | ProxyModeUi::HttpConnect => {
            let pport: u16 = buf
                .proxy_port
                .trim()
                .parse()
                .map_err(|_| "代理端口非法,须为 1-65535 的整数".to_string())?;
            let ep = mullion_store::ProxyEndpoint {
                host: buf.proxy_host.trim().to_string(),
                port: pport,
                user: if buf.proxy_user.trim().is_empty() {
                    None
                } else {
                    Some(buf.proxy_user.trim().to_string())
                },
            };
            Some(if buf.proxy_mode == ProxyModeUi::Socks5 {
                mullion_store::ProxyChoice::Socks5(ep)
            } else {
                mullion_store::ProxyChoice::HttpConnect(ep)
            })
        }
    };
    // 「无」写 `Some(vec![])` 而不是 `None`:后者是继承,分组一旦配了跳板就会
    // 被拉回去走跳板 —— 用户选的明明是「无」。
    let jump = match buf.jump_mode {
        JumpModeUi::None => Some(Vec::new()),
        JumpModeUi::Inherit => None,
        JumpModeUi::Custom => Some(
            buf.jump_chain
                .iter()
                .map(|id| mullion_store::JumpRef(*id))
                .collect(),
        ),
    };
    let mut draft = SessionDraft {
        identity: Identity {
            name: buf.name.trim().to_string(),
            // note 不 trim:用户备注里的前后空格属于用户数据(既有行为)。
            note: buf.note.clone(),
            group_id: buf.preserved_group_id,
            tags: buf.preserved_tags.clone(),
        },
        connection: Connection {
            host: buf.host.trim().to_string(),
            port,
            protocol: buf.protocol,
        },
        auth: Auth {
            user: buf.user.trim().to_string(),
            kind,
        },
        terminal: buf.preserved_terminal.clone(),
        appearance: buf.preserved_appearance.clone(),
        network: NetworkPrefs { proxy, jump },
        automation: buf.preserved_automation.clone(),
        secret: None,
    };
    sync_has_passphrase(&mut draft, secret.as_ref());
    draft.secret = secret;
    Ok(draft)
}

/// 表单 → 可直接粘进终端的 ssh 连接串。**只用非敏感字段** ——
/// 它会进系统剪贴板,拼上口令等于把口令交给剪贴板历史。
pub(crate) fn connect_string(buf: &EditorBuffer) -> String {
    let user = buf.user.trim();
    let host = buf.host.trim();
    match buf.port.trim() {
        "22" | "" => format!("ssh {user}@{host}"),
        p => format!("ssh -p {p} {user}@{host}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_store::{
        ColorSpec, ColorTarget, IconKind, IconSpec, JumpRef, ProxyChoice, ProxyEndpoint,
    };

    /// 红线:`EditorBuffer` 携带三个明文口令缓冲。若 derive(Debug),`{:?}` 会把
    /// 它们打印出来——目前虽无调用点,但属休眠风险(对照 `mullion_ssh::hop::Hop`
    /// 的手写打码 Debug)。必须手写 Debug 并打码。
    #[test]
    fn debug_never_leaks_editor_buffer_secrets() {
        let mut b = buf();
        b.password = "hunter2".into();
        b.passphrase = "keypw".into();
        b.proxy_password = "proxypw".into();
        let s = format!("{b:?}");
        assert!(!s.contains("hunter2"), "密码绝不能出现在 Debug 里: {s}");
        assert!(!s.contains("keypw"), "私钥口令绝不能出现在 Debug 里: {s}");
        assert!(!s.contains("proxypw"), "代理口令绝不能出现在 Debug 里: {s}");
        assert!(s.contains("192.0.2.10"), "非敏感字段应保留以便排障: {s}");
    }

    /// 一条只有 `network.jump` 有意义的最小会话记录,给回填方向的测试用。
    fn rec_with_jump(jump: Option<Vec<JumpRef>>) -> SessionRecord {
        SessionRecord {
            id: SessionId(1),
            modified_at: "t".into(),
            identity: Identity {
                name: "dev".into(),
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
            network: NetworkPrefs { proxy: None, jump },
            automation: AutomationPrefs::default(),
        }
    }

    fn buf() -> EditorBuffer {
        EditorBuffer {
            name: "dev".into(),
            host: "192.0.2.10".into(),
            port: "22".into(),
            protocol: Protocol::Ssh,
            user: "user".into(),
            note: "跳板后".into(),
            auth_kind: AuthKindUi::Password,
            password: String::new(),
            passphrase: String::new(),
            ..EditorBuffer::default()
        }
    }

    #[test]
    fn password_session_builds_draft_with_secret() {
        let mut b = buf();
        b.password = "pw".into();
        b.password_touched = true; // ← 新增这一行,模拟用户真的输入过
        let draft = build_draft(&b).unwrap();
        assert_eq!(draft.identity.name, "dev");
        assert_eq!(draft.connection.host, "192.0.2.10");
        assert_eq!(draft.connection.port, 22);
        assert!(matches!(draft.auth.kind, AuthKind::Password));
        assert_eq!(
            draft.secret.as_ref().and_then(|s| s.password.clone()),
            Some("pw".to_string())
        );
    }

    /// touched=false(未碰)时,即便密码框是空的,也不该产生 secret ——
    /// 这条测的是「全新会话不因为字段为空就画蛇添足」,不是「清除」路径。
    /// 真正的 Clear 路径见 merge_secret 的 clear_removes_existing_password。
    #[test]
    fn untouched_empty_password_produces_no_secret() {
        let b = buf(); // password 留空
        let draft = build_draft(&b).unwrap();
        assert!(draft.secret.is_none(), "留空密码应清除已存凭据");
    }

    #[test]
    fn pubkey_with_passphrase_sets_has_passphrase_and_secret() {
        let mut b = buf();
        b.auth_kind = AuthKindUi::PublicKey;
        b.key_data = "-----BEGIN OPENSSH PRIVATE KEY-----".into();
        b.key_touched = true;
        b.passphrase = "ph".into();
        b.passphrase_touched = true; // ← 新增这一行,模拟用户真的输入过
        let draft = build_draft(&b).unwrap();
        match draft.auth.kind {
            AuthKind::PublicKey { has_passphrase } => assert!(has_passphrase),
            _ => panic!("应为 PublicKey"),
        }
        assert_eq!(
            draft.secret.as_ref().and_then(|s| s.passphrase.clone()),
            Some("ph".to_string())
        );
    }

    #[test]
    fn pubkey_without_passphrase_has_no_secret() {
        let mut b = buf();
        b.auth_kind = AuthKindUi::PublicKey;
        let draft = build_draft(&b).unwrap();
        match draft.auth.kind {
            AuthKind::PublicKey { has_passphrase, .. } => assert!(!has_passphrase),
            _ => panic!("应为 PublicKey"),
        }
        assert!(draft.secret.is_none());
    }

    #[test]
    fn invalid_port_is_rejected() {
        let mut b = buf();
        b.port = "not-a-port".into();
        assert!(build_draft(&b).is_err());

        let mut b2 = buf();
        b2.port = "99999999".into(); // 超出 u16 范围
        assert!(build_draft(&b2).is_err());
    }

    /// 回归测试(critical):编辑表单编辑不到的字段(分组/标签/终端偏好/外观偏好)
    /// 在「读入表单 → 写回 draft」这趟往返里必须原样保留,不能被表单的占位默认值
    /// 悄悄清空。`Vault::update` 对这四项是整体替换而非合并,一旦 build_draft 填了
    /// 默认值,保存就会真的把用户数据清空。
    #[test]
    fn editing_a_session_preserves_fields_the_form_cannot_edit() {
        let rec = SessionRecord {
            id: SessionId(7),
            modified_at: "2026-07-25T00:00:00Z".into(),
            identity: Identity {
                name: "dev".into(),
                note: "跳板后".into(),
                group_id: Some(GroupId(1)),
                tags: vec!["web01".into()],
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
            terminal: TerminalPrefs {
                scrollback: Some(12345),
            },
            appearance: AppearancePrefs {
                icon: Some(IconSpec {
                    kind: IconKind::Emoji,
                    value: "🚀".into(),
                }),
                color: Some(ColorSpec {
                    hex: "#ff0000".into(),
                    apply_to: vec![ColorTarget::Tab],
                }),
            },
            // 非默认值:表单目前没有代理/跳板编辑控件(那是后续任务的事),
            // 但这条守护测试要能抓住「编辑时被静默清空」——值必须区别于
            // `NetworkPrefs::default()`,否则清空和保留看起来一样。
            network: NetworkPrefs {
                proxy: Some(ProxyChoice::Socks5(ProxyEndpoint {
                    host: "127.0.0.1".into(),
                    port: 7891,
                    user: None,
                })),
                jump: None,
            },
            // 同上:表单也还没有编辑自动化配置的入口,必须原样透传,
            // 值须区别于 `AutomationPrefs::default()` 才能抓住被静默清空的情况。
            automation: AutomationPrefs {
                enabled: Some(false),
                ..Default::default()
            },
        };

        let editor_buf = EditorBuffer::from_record(&rec);
        let draft = build_draft(&editor_buf).unwrap();

        assert_eq!(
            draft.identity.group_id, rec.identity.group_id,
            "编辑不该清空 UI 编辑不到的字段:group_id"
        );
        assert_eq!(
            draft.identity.tags, rec.identity.tags,
            "编辑不该清空 UI 编辑不到的字段:tags"
        );
        assert_eq!(
            draft.terminal, rec.terminal,
            "编辑不该清空 UI 编辑不到的字段:terminal"
        );
        assert_eq!(
            draft.appearance, rec.appearance,
            "编辑不该清空 UI 编辑不到的字段:appearance"
        );
        assert_eq!(
            draft.network, rec.network,
            "编辑不该清空 UI 编辑不到的字段:network(代理/跳板)"
        );
        assert_eq!(
            draft.automation, rec.automation,
            "编辑不该清空 UI 编辑不到的字段:automation"
        );
    }

    /// 表单能编代理与跳板了,它们必须真的往返一次而不被吃掉。
    #[test]
    fn editor_round_trips_proxy_and_jump_chain() {
        let rec = SessionRecord {
            id: SessionId(7),
            modified_at: "2026-07-25T00:00:00Z".into(),
            identity: Identity {
                name: "dev".into(),
                note: "跳板后".into(),
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
            terminal: TerminalPrefs::default(),
            appearance: AppearancePrefs::default(),
            network: NetworkPrefs {
                proxy: Some(ProxyChoice::Socks5(ProxyEndpoint {
                    host: "127.0.0.1".into(),
                    port: 7891,
                    user: Some("alice".into()),
                })),
                jump: Some(vec![JumpRef(SessionId(2))]),
            },
            automation: AutomationPrefs::default(),
        };
        let buf = EditorBuffer::from_record(&rec);
        let draft = build_draft(&buf).unwrap();
        assert_eq!(draft.network, rec.network, "代理与跳板必须原样往返");
    }

    /// 分组代理下,会话选「不使用代理」必须落成显式 `Direct` 而非 `None`——
    /// 落成 `None` 会继续继承分组代理,与用户所选相反。
    #[test]
    fn choosing_no_proxy_writes_explicit_direct_not_inherit() {
        let buf = EditorBuffer {
            port: "22".into(),
            proxy_mode: ProxyModeUi::Direct,
            ..EditorBuffer::default()
        };
        let draft = build_draft(&buf).unwrap();
        assert_eq!(
            draft.network.proxy,
            Some(ProxyChoice::Direct),
            "「不使用代理」是覆盖,不是不设置"
        );
    }

    #[test]
    fn choosing_inherit_leaves_proxy_unset() {
        let buf = EditorBuffer {
            port: "22".into(),
            proxy_mode: ProxyModeUi::Inherit,
            ..EditorBuffer::default()
        };
        let draft = build_draft(&buf).unwrap();
        assert_eq!(draft.network.proxy, None, "「跟随分组」= 不设置");
    }

    /// 跳板三态 → `NetworkPrefs::jump` 的完整映射。三条必须**互不相同**:
    /// 「无」写 `None` 会被分组的跳板拉回去走跳板,与用户所选相反;
    /// 「继承分组」写 `Some(vec![])` 则永远继承不到分组的跳板。
    #[test]
    fn jump_mode_maps_onto_all_three_states_of_network_jump() {
        let mk = |mode| {
            build_draft(&EditorBuffer {
                port: "22".into(),
                jump_mode: mode,
                jump_chain: vec![SessionId(2), SessionId(3)],
                ..EditorBuffer::default()
            })
            .unwrap()
            .network
            .jump
        };
        assert_eq!(
            mk(JumpModeUi::None),
            Some(Vec::new()),
            "「无」是显式覆盖成不走跳板,不是不设置"
        );
        assert_eq!(mk(JumpModeUi::Inherit), None, "「继承分组」= 不设置");
        assert_eq!(
            mk(JumpModeUi::Custom),
            Some(vec![JumpRef(SessionId(2)), JumpRef(SessionId(3))]),
            "「自定义」按拨号顺序原样写出"
        );
    }

    /// 回填方向的三态。`Some(vec![])` 必须回成「无」而不是「自定义 + 空链」——
    /// 后者在 UI 上是个说不清的中间态。
    #[test]
    fn from_record_restores_all_three_jump_modes() {
        let mut rec = rec_with_jump(None);
        assert_eq!(
            EditorBuffer::from_record(&rec).jump_mode,
            JumpModeUi::Inherit
        );

        rec.network.jump = Some(Vec::new());
        assert_eq!(EditorBuffer::from_record(&rec).jump_mode, JumpModeUi::None);

        rec.network.jump = Some(vec![JumpRef(SessionId(9))]);
        let b = EditorBuffer::from_record(&rec);
        assert_eq!(b.jump_mode, JumpModeUi::Custom);
        assert_eq!(b.jump_chain, vec![SessionId(9)]);
    }

    /// 新建会话的默认是「无」(用户明确要求)。默认成「继承分组」会让新建的
    /// 会话在有分组跳板时悄悄走跳板,而用户什么都没配。
    #[test]
    fn new_session_defaults_to_no_jump_host() {
        assert_eq!(EditorBuffer::default().jump_mode, JumpModeUi::None);
        assert_eq!(
            build_draft(&EditorBuffer {
                port: "22".into(),
                ..EditorBuffer::default()
            })
            .unwrap()
            .network
            .jump,
            Some(Vec::new())
        );
    }

    /// 切到「无」/「继承」不清空已配的链:用户切回「自定义」应看到自己刚才
    /// 配的那几跳,而不是从头再点一遍。
    #[test]
    fn switching_away_from_custom_keeps_the_chain_buffer() {
        let mut b = EditorBuffer {
            port: "22".into(),
            jump_mode: JumpModeUi::Custom,
            jump_chain: vec![SessionId(2)],
            ..EditorBuffer::default()
        };
        b.jump_mode = JumpModeUi::None;
        assert_eq!(build_draft(&b).unwrap().network.jump, Some(Vec::new()));
        assert_eq!(b.jump_chain, vec![SessionId(2)], "缓冲不该被写回逻辑清掉");

        b.jump_mode = JumpModeUi::Custom;
        assert_eq!(
            build_draft(&b).unwrap().network.jump,
            Some(vec![JumpRef(SessionId(2))]),
            "切回「自定义」应原样恢复"
        );
    }

    #[test]
    fn proxy_port_must_be_a_valid_number() {
        let buf = EditorBuffer {
            port: "22".into(),
            proxy_mode: ProxyModeUi::Socks5,
            proxy_host: "127.0.0.1".into(),
            proxy_port: "abc".into(),
            ..EditorBuffer::default()
        };
        let err = match build_draft(&buf) {
            Err(e) => e,
            Ok(_) => panic!("非法代理端口应被拒绝"),
        };
        assert!(err.contains("代理端口"), "错误消息应点名是代理端口: {err}");
    }

    /// 钉死 note 不被 trim(既有行为:旧代码是 `buf.note.clone()`,迁移不该顺手改成
    /// `trim()`——用户备注里的前后空格属于用户数据,不该被悄悄吃掉)。
    #[test]
    fn note_is_not_trimmed_when_building_draft() {
        let mut b = buf();
        b.note = "  缩进备注  ".into();
        let draft = build_draft(&b).unwrap();
        assert_eq!(
            draft.identity.note, "  缩进备注  ",
            "note 不应被 trim,前后空格属于用户数据"
        );
    }

    /// 新建会话(没有 SessionRecord 可读)时,这四项仍应是默认值——确认
    /// `EditorBuffer::default()` 的新建路径没被这次修复带偏。
    #[test]
    fn new_session_defaults_have_no_preserved_fields() {
        let b = EditorBuffer::default();
        let draft = build_draft(&b).unwrap();
        assert_eq!(draft.identity.group_id, None);
        assert_eq!(draft.identity.tags, Vec::<String>::new());
        assert_eq!(draft.terminal, TerminalPrefs::default());
        assert_eq!(draft.appearance, AppearancePrefs::default());
    }

    fn entry(pw: Option<&str>, pp: Option<&str>, proxy: Option<&str>) -> SecretEntry {
        SecretEntry {
            password: pw.map(String::from),
            passphrase: pp.map(String::from),
            proxy_password: proxy.map(String::from),
            private_key: None,
        }
    }

    /// F73 红线:用户没碰密码框 → 已存密码必须原样留着。
    /// 这正是本切片要修的 bug:改前 `build_draft` 把空字符串当「清除」,
    /// 编辑任意一个已有会话再保存,密码就没了。
    /// 自证会变红:把 `merge_secret` 里 `SecretField::Keep => existing.cloned()`
    /// 改成 `=> None`(即改前的行为),这条立刻红。
    #[test]
    fn keep_preserves_existing_password() {
        let existing = entry(Some("old-pw"), None, None);
        let got = merge_secret(
            Some(&existing),
            &SecretField::Keep,
            &SecretField::Keep,
            &SecretField::Keep,
            &SecretField::Keep,
        );
        assert_eq!(got.unwrap().password.as_deref(), Some("old-pw"));
    }

    #[test]
    fn set_overwrites_existing_password() {
        let existing = entry(Some("old-pw"), None, None);
        let got = merge_secret(
            Some(&existing),
            &SecretField::Set("new-pw".into()),
            &SecretField::Keep,
            &SecretField::Keep,
            &SecretField::Keep,
        );
        assert_eq!(got.unwrap().password.as_deref(), Some("new-pw"));
    }

    /// 用户主动清空 → 真的清除。这是「保持不变」的对偶,不能因为修了 Keep
    /// 就把清除路径一起弄丢。
    #[test]
    fn clear_removes_existing_password() {
        let existing = entry(Some("old-pw"), Some("ph"), None);
        let got = merge_secret(
            Some(&existing),
            &SecretField::Clear,
            &SecretField::Keep,
            &SecretField::Keep,
            &SecretField::Keep,
        )
        .expect("passphrase 还在,整条不该塌成 None");
        assert_eq!(got.password, None);
        assert_eq!(got.passphrase.as_deref(), Some("ph"));
    }

    /// 三个字段全空 → 整条 `SecretEntry` 收成 `None`,不要在 secrets.enc 里
    /// 留一条三字段全 None 的空壳。
    #[test]
    fn all_cleared_collapses_to_none() {
        let existing = entry(Some("pw"), Some("ph"), Some("proxy"));
        let got = merge_secret(
            Some(&existing),
            &SecretField::Clear,
            &SecretField::Clear,
            &SecretField::Clear,
            &SecretField::Keep,
        );
        assert!(got.is_none(), "全清后不该留空壳 SecretEntry");
    }

    /// 新建会话(existing = None)且全部 Keep → 仍是 None,不能凭空造出空条目。
    #[test]
    fn keep_on_empty_existing_stays_none() {
        let got = merge_secret(
            None,
            &SecretField::Keep,
            &SecretField::Keep,
            &SecretField::Keep,
            &SecretField::Keep,
        );
        assert!(got.is_none());
    }

    /// 三个字段互相独立:清掉密码不该波及私钥口令与代理口令。
    #[test]
    fn clearing_password_keeps_other_secrets() {
        let existing = entry(Some("pw"), Some("ph"), Some("proxy"));
        let got = merge_secret(
            Some(&existing),
            &SecretField::Clear,
            &SecretField::Keep,
            &SecretField::Keep,
            &SecretField::Keep,
        )
        .unwrap();
        assert_eq!(got.password, None);
        assert_eq!(got.passphrase.as_deref(), Some("ph"));
        assert_eq!(got.proxy_password.as_deref(), Some("proxy"));
    }

    /// `SecretField` 会进 `EditorBuffer` 的 Debug、也可能进日志/panic 消息。
    /// 明文一旦被 `{:?}` 打出来,加密存储就白做了(与 `SecretEntry` 同一条红线)。
    #[test]
    fn secret_field_debug_never_leaks_plaintext() {
        let s = format!("{:?}", SecretField::Set("hunter2".to_string()));
        assert!(!s.contains("hunter2"), "Debug 泄漏了明文:{s}");
        assert!(s.contains("<已设置>"), "应打码成 <已设置>,实得:{s}");
    }

    /// §5.4.3:`has_passphrase` 必须跟**合成后**的凭据走,而不是跟表单当前
    /// 内容走。否则「有已存口令 + 用户没碰口令框」会被写成 has_passphrase=false,
    /// 下次连接时 russh 拿到加密私钥却不知道要口令,直接认证失败。
    /// 自证会变红:让 `sync_has_passphrase` 早退不写值,这条报 false != true。
    #[test]
    fn has_passphrase_follows_merged_secret_not_form() {
        let mut buf = buf();
        buf.auth_kind = AuthKindUi::PublicKey;
        // 用户没碰口令框 → 表单是空的
        let mut draft = build_draft(&buf).expect("build");
        assert!(
            matches!(
                draft.auth.kind,
                AuthKind::PublicKey {
                    has_passphrase: false,
                    ..
                }
            ),
            "表单空 + 无已存值时应为 false"
        );
        // 但库里存着口令 → 合成后应变 true
        let merged = entry(None, Some("ph"), None);
        sync_has_passphrase(&mut draft, Some(&merged));
        assert!(
            matches!(
                draft.auth.kind,
                AuthKind::PublicKey {
                    has_passphrase: true,
                    ..
                }
            ),
            "合成后有 passphrase,has_passphrase 必须是 true"
        );
    }

    /// 未碰 → Keep;碰过且非空 → Set;碰过且空 → Clear。
    #[test]
    fn secret_fields_maps_touch_state_to_three_way_intent() {
        let mut b = buf();
        assert_eq!(secret_fields(&b).0, SecretField::Keep, "未碰应为 Keep");

        b.password_touched = true;
        b.password = "pw".into();
        assert_eq!(secret_fields(&b).0, SecretField::Set("pw".into()));

        b.password = String::new();
        assert_eq!(
            secret_fields(&b).0,
            SecretField::Clear,
            "碰过后清空应为 Clear"
        );
    }

    /// 认证方式选了密码 → 私钥口令字段必须是 `Clear`(而不是 Keep),
    /// 否则会在 secrets.enc 里留下一条用不到的孤儿口令。这与改前
    /// `build_draft` 的行为一致(密码模式下 secret.passphrase 恒为 None)。
    #[test]
    fn inactive_auth_branch_is_cleared_not_kept() {
        let mut b = buf();
        b.auth_kind = AuthKindUi::Password;
        assert_eq!(
            secret_fields(&b).1,
            SecretField::Clear,
            "密码模式下口令应清除"
        );

        b.auth_kind = AuthKindUi::PublicKey;
        assert_eq!(
            secret_fields(&b).0,
            SecretField::Clear,
            "公钥模式下密码应清除"
        );
    }

    /// 密码认证的会话不该在侧车里留一把用不到的私钥(与口令同理)。
    #[test]
    fn switching_to_password_auth_clears_the_stored_private_key() {
        let mut b = buf();
        b.auth_kind = AuthKindUi::Password;
        assert_eq!(
            secret_fields(&b).3,
            SecretField::Clear,
            "密码模式下私钥应清除"
        );
    }

    /// 公钥模式没导入过 → `Keep`,不能变成 `Clear`:编辑一条已有公钥会话
    /// 只改个备注就保存,库里那把钥匙必须还在(F73 同一条红线)。
    #[test]
    fn editing_a_pubkey_session_without_reimporting_keeps_the_stored_key() {
        let mut b = buf();
        b.auth_kind = AuthKindUi::PublicKey;
        assert_eq!(
            secret_fields(&b).3,
            SecretField::Keep,
            "没重新导入就该原样保留已存私钥"
        );

        b.key_touched = true;
        b.key_data = "-----BEGIN OPENSSH PRIVATE KEY-----".into();
        assert_eq!(
            secret_fields(&b).3,
            SecretField::Set("-----BEGIN OPENSSH PRIVATE KEY-----".into())
        );

        b.key_data.clear();
        assert_eq!(secret_fields(&b).3, SecretField::Clear, "清除后应为 Clear");
    }

    /// 导入正常私钥:正文进缓冲、触碰位置起、给一行带文件名的提示。
    #[test]
    fn importing_a_private_key_stores_the_body_not_the_path() {
        let mut b = buf();
        import_key_file(&mut b, Path::new("/home/u/.ssh/id_ed25519"), |_| {
            Ok("-----BEGIN OPENSSH PRIVATE KEY-----\nBODY\n".to_string())
        });
        assert_eq!(b.key_data, "-----BEGIN OPENSSH PRIVATE KEY-----\nBODY\n");
        assert!(b.key_touched);
        let note = b.key_note.expect("应给一行提示");
        assert!(
            note.contains("id_ed25519"),
            "提示要点名导入了哪个文件: {note}"
        );
    }

    /// 选成了 `.pub` 公钥 —— 最常见的一种误操作。必须当场拒收:存进去的话
    /// 用户要等到下次连接才会看到一条「解析私钥失败」,而且不知道错在哪。
    #[test]
    fn importing_a_public_key_is_rejected_and_leaves_the_buffer_untouched() {
        let mut b = buf();
        import_key_file(&mut b, Path::new("/home/u/.ssh/id_ed25519.pub"), |_| {
            Ok("ssh-ed25519 AAAAC3Nza... u@h\n".to_string())
        });
        assert_eq!(b.key_data, "", "公钥不该被存进私钥缓冲");
        assert!(
            !b.key_touched,
            "被拒的导入不该置触碰位 —— 否则保存时会当成「清除」"
        );
        assert!(b.key_note.is_some(), "要告诉用户为什么没导进去");
    }

    /// 文件读不了(权限/被删)→ 只给提示,不动缓冲。
    #[test]
    fn an_unreadable_key_file_leaves_the_buffer_untouched() {
        let mut b = buf();
        b.key_data = "已有内容".into();
        import_key_file(&mut b, Path::new("/no/such/key"), |_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "没有这个文件",
            ))
        });
        assert_eq!(b.key_data, "已有内容", "读失败不该清掉已导入的内容");
        assert!(!b.key_touched);
        assert!(b.key_note.is_some());
    }

    /// 「清除」按钮:置空 + 触碰位,保存时才会真的把侧车里那把钥匙删掉。
    #[test]
    fn clearing_the_key_marks_it_touched_so_save_actually_removes_it() {
        let mut b = buf();
        b.key_data = "-----BEGIN OPENSSH PRIVATE KEY-----".into();
        b.key_touched = true;
        clear_key(&mut b);
        assert_eq!(b.key_data, "");
        assert!(b.key_touched, "不置触碰位的话保存时会走 Keep,钥匙删不掉");
        b.auth_kind = AuthKindUi::PublicKey;
        assert_eq!(secret_fields(&b).3, SecretField::Clear);
    }

    /// 私钥槽位与其余三个互不干扰。四个同类型参数并排,写反一个就串味 ——
    /// 这是 F73 参数错位红线在第四个槽位上的延伸。
    #[test]
    fn private_key_slot_is_independent_of_the_other_three() {
        let existing = SecretEntry {
            password: Some("pw".into()),
            passphrase: Some("ph".into()),
            proxy_password: Some("proxy".into()),
            private_key: Some("old-key".into()),
        };
        let got = merge_secret(
            Some(&existing),
            &SecretField::Keep,
            &SecretField::Keep,
            &SecretField::Keep,
            &SecretField::Set("new-key".into()),
        )
        .unwrap();
        assert_eq!(got.private_key.as_deref(), Some("new-key"));
        assert_eq!(got.password.as_deref(), Some("pw"));
        assert_eq!(got.passphrase.as_deref(), Some("ph"));
        assert_eq!(got.proxy_password.as_deref(), Some("proxy"));
    }

    /// 只剩一把私钥时整条 `SecretEntry` 不能塌成 `None` —— 塌了就等于保存
    /// 时把刚导入的钥匙丢了(无口令的公钥会话正是这种形状)。
    #[test]
    fn a_lone_private_key_does_not_collapse_to_none() {
        let got = merge_secret(
            None,
            &SecretField::Clear,
            &SecretField::Clear,
            &SecretField::Clear,
            &SecretField::Set("key".into()),
        );
        assert_eq!(
            got.and_then(|s| s.private_key),
            Some("key".to_string()),
            "只有私钥的 secret 也必须保留"
        );
    }

    /// 私钥正文是明文凭据,`{:?}` 一打就写进日志。与 password 同一条红线。
    #[test]
    fn editor_buffer_debug_never_leaks_the_key_body() {
        let mut b = buf();
        b.key_data = "-----BEGIN OPENSSH PRIVATE KEY-----\nSECRETBODY\n".into();
        let s = format!("{b:?}");
        assert!(!s.contains("SECRETBODY"), "Debug 泄漏了私钥正文: {s}");
    }

    /// `.0` 那条只覆盖了 password;`.1`/`.2` 目前共用同一个 `field()`,
    /// 但将来若有人给 passphrase / proxy_password 加特殊处理(例如按
    /// proxy_mode 门控),回归会从这里溜过去。补齐三态映射的覆盖。
    #[test]
    fn secret_fields_maps_touch_state_for_passphrase_and_proxy_password() {
        let mut b = buf();
        b.auth_kind = AuthKindUi::PublicKey;
        assert_eq!(secret_fields(&b).1, SecretField::Keep);
        b.passphrase_touched = true;
        b.passphrase = "ph".into();
        assert_eq!(secret_fields(&b).1, SecretField::Set("ph".into()));
        b.passphrase = String::new();
        assert_eq!(secret_fields(&b).1, SecretField::Clear);

        assert_eq!(secret_fields(&b).2, SecretField::Keep);
        b.proxy_password_touched = true;
        b.proxy_password = "pp".into();
        assert_eq!(secret_fields(&b).2, SecretField::Set("pp".into()));
        b.proxy_password = String::new();
        assert_eq!(secret_fields(&b).2, SecretField::Clear);
    }

    /// 脏检查用「与基线快照逐字段比对」,不是「有没有按过键」——用户改完又
    /// 改回来不该算脏,否则每次切会话都弹一次「有未保存的更改」,弹到用户
    /// 条件反射点「丢弃」,这个确认就废了。
    #[test]
    fn is_dirty_compares_against_baseline_not_keystrokes() {
        let baseline = buf();
        let mut edited = baseline.clone();
        assert!(!is_dirty(&edited, &baseline), "没改动不算脏");

        edited.note = "改了".into();
        assert!(is_dirty(&edited, &baseline), "改了字段算脏");

        edited.note = baseline.note.clone();
        assert!(!is_dirty(&edited, &baseline), "改回来不算脏");
    }

    /// 触碰位本身也参与比对:用户点进密码框又清空(= 意图「清除凭据」),
    /// 文本内容和基线一样都是空的,但意图变了,必须算脏。
    /// 自证会变红:把 `is_dirty` 改成只比 `buf.name != baseline.name` 之类的
    /// 子集比对,这条报「应算脏」。
    #[test]
    fn clearing_a_password_counts_as_dirty_even_though_the_text_is_still_empty() {
        let baseline = buf();
        let mut edited = baseline.clone();
        edited.password_touched = true; // 点进去过,框里仍是空 → 意图清除
        assert!(
            is_dirty(&edited, &baseline),
            "清除凭据的意图必须算脏,否则切走时静默丢弃"
        );
    }

    /// 复制出来的连接串要能直接粘进终端跑。端口是 22 时省略 `-p`
    /// (`ssh -p 22` 虽然能跑,但没人这么写)。
    #[test]
    fn connect_string_is_pasteable_and_omits_the_default_port() {
        let mut b = buf();
        b.user = "root".into();
        b.host = "192.0.2.10".into();
        b.port = "22".into();
        assert_eq!(connect_string(&b), "ssh root@192.0.2.10");

        b.port = "2222".into();
        assert_eq!(connect_string(&b), "ssh -p 2222 root@192.0.2.10");
    }

    /// 连接串里**绝不能**出现密码 —— 它会进系统剪贴板,再进用户的聊天记录。
    /// 自证会变红:在 `connect_string` 里拼上 `buf.password`,这条立刻红。
    #[test]
    fn connect_string_never_contains_a_password() {
        let mut b = buf();
        b.password = "hunter2".into();
        b.password_touched = true;
        assert!(
            !connect_string(&b).contains("hunter2"),
            "连接串会进剪贴板,绝不能带密码"
        );
    }

    /// 占位符 `******` 绝不能被当成真密码存进去。控件用 `gained_focus()` 做
    /// 迁移点、聚焦即清空,保证了这一点;这条测试守的是**下游**:即使某天
    /// 控件写错、把占位符留在了 `value` 里,只要 `touched` 还是 false,
    /// `secret_fields` 就必须给 `Keep`,不读那个字符串。
    /// 自证会变红:把 `secret_fields` 里的 `field()` 改成不看 `touched`
    /// (`if v.is_empty() { Clear } else { Set(..) }`),这条报 Keep != Set。
    #[test]
    fn untouched_field_never_leaks_its_placeholder_into_a_set_intent() {
        let mut b = buf();
        b.password = "******".into(); // 模拟控件把占位符留在了缓冲里
        b.password_touched = false;
        assert_eq!(
            secret_fields(&b).0,
            SecretField::Keep,
            "未碰过的框无论里面装着什么,都必须是 Keep"
        );
    }
}
