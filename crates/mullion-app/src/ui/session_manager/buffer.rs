//! 会话编辑表单的**纯逻辑**:表单缓冲、表单 → `SessionDraft` 的转换、凭据三态合成。
//!
//! **本文件不许 `use egui`**。会话表单的 bug(端口解析、代理三态、凭据被静默清除)
//! 全部能在没有窗口的情况下单测复现——这是把它从 UI 代码里切出来的全部理由。

use std::path::Path;

use mullion_store::{
    AppearancePrefs, Auth, AuthKind, AutomationPrefs, Connection, CredentialId, GroupId, Identity,
    InlineAuth, NetworkPrefs, Protocol, SecretEntry, SessionDraft, SessionId, SessionRecord,
    TerminalPrefs,
};

/// 「这条会话的身份从哪来」(F74 设计 D9)。**严格二选一** —— 与
/// `mullion_store::Auth` 的两个变体一一对应,UI 上不给「引用 + 局部覆盖」的
/// 中间态,那正是本功能要消灭的东西。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredSourceUi {
    /// 本会话独有:用户名 + 密码/私钥都填在这张表单上。
    Own,
    /// 引用共享凭据:身份整块来自 `credential_id` 指的那份。
    Shared,
}

/// 编辑表单里认证方式的选择。不复用 `AuthKind` 本身,因为 UI 在密码/公钥两种模式
/// 间切换时要各自保留自己的缓冲(密码框内容、私钥路径都不该因切换选项就丢)。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AuthKindUi {
    #[default]
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
    /// F74:身份来自本会话还是共享凭据。`Shared` 时下面的
    /// `user`/`auth_kind`/`password`/`key_data`/`passphrase` 全部不参与保存,
    /// 但**不清空** —— 用户切回 `Own` 应看到自己原来填的,而不是从头再来
    /// (同 `jump_chain` 切模式不清空的理由)。
    pub cred_source: CredSourceUi,
    /// `cred_source == Shared` 时引用哪一份。`None` = 还没选,保存被禁。
    pub credential_id: Option<CredentialId>,
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
    /// 标签输入框里还没敲回车的那一截(走查 6)。
    ///
    /// 参与 `is_dirty` 比对是**有意的**:用户输了一半就切走,那半截会丢,
    /// 该弹确认。反过来把它排除掉要手写 `PartialEq`,为一个字段推翻整个
    /// derive,代价大得多。回车确认后这里会被清空,不会一直挂着让表单显脏。
    pub tag_input: String,
    pub preserved_terminal: TerminalPrefs,
    pub preserved_appearance: AppearancePrefs,
    pub preserved_automation: AutomationPrefs,
    /// F154:本地书签。表单里没有对应字段(本地目录收藏是在文件面板上点
    /// ☆ 加的),原样带着走 —— 不带的话保存一次就全没了,静默。
    pub preserved_local_bookmarks: Vec<mullion_store::Bookmark>,

    /// 「浏览…」按钮本帧被点了。`mod.rs` 在借用释放后转成
    /// `UiState::pick_key_request`,随即复位。
    pub pick_key_clicked: bool,
    /// 图标页的「导入…」本帧被点了。同上,由 `mod.rs` 转交给 app 去开文件
    /// 对话框 —— 不在 egui 闭包里同步开对话框(会把整个事件循环堵死,
    /// 理由见 `app.rs::spawn_key_picker`)。
    ///
    /// 导入**失败**的原因不放这里,放 `UiState::icon_error`:这个结构整体参与
    /// `is_dirty` 比对,一条错误提示会让「什么都没改成」的表单显示成脏的、
    /// 切走时白弹一次确认(触碰位当初也是为了这个搬去 `UiState` 的)。
    pub pick_icon_clicked: bool,

    /// F120:SFTP 默认远端目录。空 = 用登录目录。
    pub sftp_default_remote: String,
    /// F120:SFTP 默认本地目录。空 = 用用户主目录。
    pub sftp_default_local: String,
    /// F120:远端书签 `(名称, 路径)`。顺序即用户排的顺序,保存时不许重排。
    pub sftp_bookmarks: Vec<(String, String)>,
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
            cred_source: CredSourceUi::Own,
            credential_id: None,
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
            tag_input: String::new(),
            preserved_terminal: TerminalPrefs::default(),
            preserved_appearance: AppearancePrefs::default(),
            preserved_automation: AutomationPrefs::default(),
            preserved_local_bookmarks: Vec::new(),
            pick_key_clicked: false,
            pick_icon_clicked: false,
            sftp_default_remote: String::new(),
            sftp_default_local: String::new(),
            sftp_bookmarks: Vec::new(),
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

/// 勾选 / 取消一个颜色落点。
///
/// **只增删指定的那一个**：`apply_to` 里可能有编辑器当下没展示勾选框的落点
/// （`ColorTarget` 是 store schema 的一部分，加落点和加勾选框是两笔改动，中间
/// 必然存在只有前者的版本；旧配置文件里也可能存着更新版本写下的落点）。按
/// 「勾了什么存什么」重建整个列表，会把这些落点静默剥掉，而用户完全看不出。
pub(crate) fn set_color_target(
    spec: &mut mullion_store::ColorSpec,
    target: mullion_store::ColorTarget,
    on: bool,
) {
    let has = spec.apply_to.contains(&target);
    if on && !has {
        spec.apply_to.push(target);
    } else if !on && has {
        spec.apply_to.retain(|t| *t != target);
    }
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
            .field("preserved_local_bookmarks", &self.preserved_local_bookmarks)
            .field("sftp_default_remote", &self.sftp_default_remote)
            .field("sftp_default_local", &self.sftp_default_local)
            .field("sftp_bookmarks", &self.sftp_bookmarks)
            .finish()
    }
}

/// 新建会话时预填的用户名(走查 21)。
///
/// Windows 用 `USERNAME`,类 Unix 用 `USER` —— 顺序不能反:Git Bash / WSL
/// 之类的环境两个都设,而这个项目的一等公民是 Windows,那里 `USERNAME` 才
/// 是权威的那个。
///
/// 取一个 `getenv` 闭包而不是直接读进程环境,是为了让这条判据可以单测 ——
/// 测试进程里 `USER` 是跑 CI 的那个账号,断不出什么来。
pub(crate) fn default_user(getenv: impl Fn(&str) -> Option<String>) -> String {
    for key in ["USERNAME", "USER"] {
        if let Some(v) = getenv(key) {
            let v = v.trim();
            if !v.is_empty() {
                return v.to_string();
            }
        }
    }
    String::new()
}

impl EditorBuffer {
    /// 空白新建表单,用户名预填成当前系统账号(走查 21)。
    ///
    /// 「新建会话」十有八九是连自己常用的那个账号;预填省一次输入,填错了
    /// 也就是改两个字 —— 比每次都从空开始划算。
    pub(crate) fn new_draft() -> Self {
        Self {
            user: default_user(|k| std::env::var(k).ok()),
            ..Self::default()
        }
    }

    /// 把已有会话的非敏感字段填入表单(密码/口令 store 不会明文回吐,留空 ——
    /// 编辑时留空 = 不改;见 `build_draft` 的说明)。
    pub(crate) fn from_record(rec: &SessionRecord) -> Self {
        let mut buf = Self {
            name: rec.identity.name.clone(),
            host: rec.connection.host.clone(),
            port: rec.connection.port.to_string(),
            protocol: rec.connection.protocol,
            // 引用共享凭据的会话身上没有用户名(F74)—— 留空,由下面的
            // `cred_source` 把表单切到共享档,用户名那行整个不画。
            user: rec
                .auth
                .as_inline()
                .map_or_else(String::new, |i| i.user.clone()),
            cred_source: match rec.auth {
                Auth::Inline(_) => CredSourceUi::Own,
                Auth::Ref(_) => CredSourceUi::Shared,
            },
            credential_id: rec.auth.credential_id(),
            note: rec.identity.note.clone(),
            preserved_group_id: rec.identity.group_id,
            preserved_tags: rec.identity.tags.clone(),
            preserved_terminal: rec.terminal.clone(),
            preserved_appearance: rec.appearance.clone(),
            preserved_automation: rec.automation.clone(),
            preserved_local_bookmarks: rec.sftp.local_bookmarks.clone(),
            sftp_default_remote: rec.sftp.default_remote.clone().unwrap_or_default(),
            sftp_default_local: rec.sftp.default_local.clone().unwrap_or_default(),
            sftp_bookmarks: rec
                .sftp
                .bookmarks
                .iter()
                .map(|b| (b.name.clone(), b.path.clone()))
                .collect(),
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
        match rec.auth.as_inline().map(|i| &i.kind) {
            Some(AuthKind::PublicKey { .. }) => buf.auth_kind = AuthKindUi::PublicKey,
            Some(AuthKind::Password) | None => buf.auth_kind = AuthKindUi::Password,
        }
        // 图标不需要任何回填:它整个躺在 `preserved_appearance` 里(第 289 行
        // 已经 clone 过来了),图标页直接读写那份。
        //
        // v0.1.23~v0.1.25 这里有一个「模式位」要回填,因为 emoji 是边打边存的
        // ——缓冲空的那一瞬间会被反推成「没图标」,UI 当场弹回去(实机报的
        // 「点 emoji 没有内容」)。改成导入 .ico 之后不存在中间态:文件选完
        // 图标就有值,没选就没有,模式位没有存在的理由了。
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
    if let Some(InlineAuth {
        kind: AuthKind::PublicKey { has_passphrase, .. },
        ..
    }) = draft.auth.as_inline_mut()
    {
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
    // 引用共享凭据时,身份三件套全在凭据的侧车里(设计 D4)。会话自己那三格
    // 一律 `Clear`:留着就是 secrets.enc 里三条谁也不会去读的孤儿明文。
    // **代理口令不在此列** —— 它归会话,不归凭据。
    if buf.cred_source == CredSourceUi::Shared {
        return (
            SecretField::Clear,
            SecretField::Clear,
            field(buf.proxy_password_touched, &buf.proxy_password),
            SecretField::Clear,
        );
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
    match read_key_file(path, read) {
        Ok((text, note)) => {
            buf.key_data = text;
            buf.key_touched = true;
            buf.key_note = Some(note);
        }
        Err(note) => buf.key_note = Some(note),
    }
}

/// 上面那件事里与缓冲无关的那一半:读文件 + 判「像不像私钥」+ 措辞。
/// `Ok((正文, 提示))` / `Err(提示)`。
///
/// 抽出来是因为凭据表单(F74)有自己的缓冲类型,却必须给出**同一套判定和
/// 同一句话** —— 各写一遍的话,「选成了 .pub」这条提示迟早只剩一边有。
pub(crate) fn read_key_file(
    path: &Path,
    read: impl FnOnce(&Path) -> std::io::Result<String>,
) -> Result<(String, String), String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    match read(path) {
        Err(e) => Err(format!("读不了 {name}:{e}")),
        Ok(text) if !looks_like_private_key(&text) => Err(format!(
            "{name} 不像私钥 —— 要选的是私钥本体,不是 .pub 公钥"
        )),
        Ok(text) => Ok((text, format!("已导入 {name}"))),
    }
}

/// 把选中的 .ico 读进表单(F61)。跟 `import_key_file` 同一形状:IO 由调用方
/// 注入,函数本身可纯单测。
///
/// 返回 `Err(提示文案)`,由调用方落进 `UiState::icon_error` —— **不放
/// `EditorBuffer`**,那个结构整体参与 `is_dirty` 比对,一条错误提示会让
/// 「什么都没改成」的表单显示成脏的、切走时白弹一次确认。
///
/// 已设的底色跨导入保留:换一张图不意味着要重挑一次底色。
pub(crate) fn import_icon_file(
    buf: &mut EditorBuffer,
    path: &Path,
    read: impl FnOnce(&Path) -> std::io::Result<Vec<u8>>,
) -> Result<(), String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let bytes = read(path).map_err(|e| format!("读不了 {name}:{e}"))?;
    let value = crate::ui::ico::import(&bytes).map_err(|e| e.message())?;
    let bg = buf
        .preserved_appearance
        .icon
        .as_ref()
        .and_then(|i| i.bg.clone());
    buf.preserved_appearance.icon = Some(mullion_store::IconSpec {
        kind: mullion_store::IconKind::Ico,
        value,
        bg,
    });
    Ok(())
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
    // 走查 15:端口校验只有一处真源(`validate::port`),UI 上的红字和这里的
    // 保存拦截判的是同一个函数——否则会出现「红字说没问题、保存又失败」。
    let port: u16 = super::validate::port(&buf.port).map_err(str::to_string)?;
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
        auth: match buf.cred_source {
            CredSourceUi::Own => Auth::inline(buf.user.trim(), kind),
            // 保存按钮在没选凭据时是禁用的(`validate::check`),但
            // `build_draft` 得是全函数 —— 键盘保存那条路径够不着按钮状态。
            CredSourceUi::Shared => Auth::Ref(
                buf.credential_id
                    .ok_or_else(|| "请先选一份共享凭据".to_string())?,
            ),
        },
        terminal: buf.preserved_terminal.clone(),
        appearance: buf.preserved_appearance.clone(),
        network: NetworkPrefs { proxy, jump },
        automation: buf.preserved_automation.clone(),
        sftp: mullion_store::SftpPrefs {
            default_remote: {
                let v = buf.sftp_default_remote.trim();
                (!v.is_empty()).then(|| v.to_string())
            },
            default_local: {
                let v = buf.sftp_default_local.trim();
                (!v.is_empty()).then(|| v.to_string())
            },
            bookmarks: buf
                .sftp_bookmarks
                .iter()
                .filter(|(_, path)| !path.trim().is_empty())
                .map(|(name, path)| mullion_store::Bookmark {
                    name: name.clone(),
                    path: path.clone(),
                })
                .collect(),
            // F154:表单里没有这一项,原样带回去(同 `preserved_automation`)。
            local_bookmarks: buf.preserved_local_bookmarks.clone(),
        },
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

    /// 本模块绝大多数测试用的是「自带认证」档(`CredSourceUi::Own`),
    /// 断言认证方式时统一走这里。共享凭据档单独由下面三条覆盖。
    fn inline_kind(d: &SessionDraft) -> &AuthKind {
        &d.auth.as_inline().expect("build_draft 产出自带认证").kind
    }

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
            auth: Auth::inline("u", AuthKind::Password),
            terminal: TerminalPrefs::default(),
            appearance: AppearancePrefs::default(),
            network: NetworkPrefs { proxy: None, jump },
            automation: AutomationPrefs::default(),
            sftp: Default::default(),
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
        assert!(matches!(inline_kind(&draft), AuthKind::Password));
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
        match inline_kind(&draft) {
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
        match inline_kind(&draft) {
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
            auth: Auth::inline("user", AuthKind::Password),
            terminal: TerminalPrefs {
                scrollback: Some(12345),
            },
            appearance: AppearancePrefs {
                icon: Some(IconSpec {
                    kind: IconKind::Emoji,
                    value: "🚀".into(),
                    bg: None,
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
            sftp: Default::default(),
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
            auth: Auth::inline("user", AuthKind::Password),
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
            sftp: Default::default(),
        };
        let buf = EditorBuffer::from_record(&rec);
        let draft = build_draft(&buf).unwrap();
        assert_eq!(draft.network, rec.network, "代理与跳板必须原样往返");
    }

    /// F120:SFTP 默认目录 + 书签必须原样往返,不能被 `build_draft` 悄悄丢掉
    /// —— 这是 Task 11 的核心断言,`build_draft` 的 `SessionDraft` 字面量里
    /// 若漏掉 `sftp: ...` 这一支,这条测试要能抓到。
    #[test]
    fn sftp_prefs_survive_the_editor_round_trip() {
        let mut rec = rec_with_jump(None);
        rec.sftp = mullion_store::SftpPrefs {
            default_remote: Some("/srv/app".into()),
            default_local: Some(r"D:\work".into()),
            bookmarks: vec![mullion_store::Bookmark {
                name: "日志".into(),
                path: "/var/log".into(),
            }],
            local_bookmarks: Vec::new(),
        };
        let buf = EditorBuffer::from_record(&rec);
        let draft = build_draft(&buf).unwrap();
        assert_eq!(draft.sftp, rec.sftp, "SFTP 默认目录与书签必须原样往返");
    }

    /// F154:本地书签不是表单字段,`build_draft` 是整份重建 `SftpPrefs` ——
    /// 不显式保住的话,用户在会话编辑器里点一次「保存」就把它们全清了,
    /// **而且没有任何提示**(同 `preserved_automation` 那条的教训)。
    ///
    /// 自证会变红:把 `build_draft` 里的
    /// `local_bookmarks: buf.preserved_local_bookmarks.clone()` 换成
    /// `local_bookmarks: Vec::new()`。
    #[test]
    fn local_bookmarks_survive_an_editor_round_trip_even_though_no_field_shows_them() {
        let mut rec = rec_with_jump(None);
        rec.sftp = mullion_store::SftpPrefs {
            default_remote: Some("/srv/app".into()),
            default_local: Some(r"D:\work".into()),
            bookmarks: vec![mullion_store::Bookmark {
                name: "日志".into(),
                path: "/var/log".into(),
            }],
            local_bookmarks: vec![mullion_store::Bookmark {
                name: "工程".into(),
                path: r"D:\work\proj".into(),
            }],
        };
        let buf = EditorBuffer::from_record(&rec);
        let draft = build_draft(&buf).expect("表单该能转回草稿");
        assert_eq!(
            draft.sftp.local_bookmarks.len(),
            1,
            "编辑器保存把本地书签清空了"
        );
        assert_eq!(draft.sftp.local_bookmarks[0].path, r"D:\work\proj");
    }

    /// **F120 记录一处刻意保留的超范围行为**:路径是纯空白的书签行会在
    /// `build_draft` 里被**静默丢弃**(`.filter(|(_, path)| !path.trim().is_empty())`)。
    /// 计划原文没要求这个过滤,是实现时顺手加的——复核判定「超范围」。
    ///
    /// **决定保留,不改成报错**:一条路径是空白的书签本来就点了也去不了
    /// 哪里,存一条这样的书签比静默清掉它更糟——用户会在文件面板里点开一个
    /// 无法解释的空路径。改成报错又会让「书签名填了、路径没顾上填」这种
    /// 半成品编辑状态挡住整张表单保存,惩罚过重。丢弃是两害相权的选择,这
    /// 条测试把它钉死,不许在未来的重构里退化成「原样存下去」或「报错」。
    ///
    /// 自证会变红:把 `build_draft` 里的
    /// `.filter(|(_, path)| !path.trim().is_empty())` 删掉——两条书签都会被
    /// 存下来,第一条 `assert_eq!`(长度必须是 1)会失败。
    #[test]
    fn build_draft_silently_drops_bookmarks_with_a_blank_path() {
        let buf = EditorBuffer {
            sftp_bookmarks: vec![
                ("日志".into(), "/var/log".into()),
                ("空白".into(), "   ".into()),
            ],
            ..buf()
        };
        let draft = build_draft(&buf).unwrap();
        assert_eq!(
            draft.sftp.bookmarks.len(),
            1,
            "路径是纯空白的书签必须被丢弃,不能原样存进去"
        );
        assert_eq!(
            draft.sftp.bookmarks[0],
            mullion_store::Bookmark {
                name: "日志".into(),
                path: "/var/log".into(),
            },
            "留下来的那一条必须是路径非空的那条,内容不能走样"
        );
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
                inline_kind(&draft),
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
                inline_kind(&draft),
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

    /// 走查 21:新建会话预填系统用户名。**`USERNAME` 优先于 `USER`** ——
    /// Git Bash / WSL 之类的环境两个都设,而这个项目的一等公民是 Windows,
    /// 那里 `USERNAME` 才是权威的那个。
    #[test]
    fn a_new_draft_prefills_the_system_user_preferring_windows_username() {
        let both = |k: &str| match k {
            "USERNAME" => Some("win-me".to_string()),
            "USER" => Some("unix-me".to_string()),
            _ => None,
        };
        assert_eq!(default_user(both), "win-me", "两个都在时该取 USERNAME");

        let only_unix = |k: &str| (k == "USER").then(|| "unix-me".to_string());
        assert_eq!(default_user(only_unix), "unix-me");

        // 设了但是空的 = 没设。填一串空格进「用户」框比留空更烦人。
        let blank = |k: &str| (k == "USERNAME").then(|| "   ".to_string());
        assert_eq!(default_user(blank), "");
        assert_eq!(default_user(|_| None), "");
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

    /// F62:勾选框只增删**指定的那一个**落点。
    ///
    /// `apply_to` 里可能有编辑器当下没展示勾选框的落点(`ColorTarget` 是 store
    /// schema 的一部分,加落点和加勾选框是两笔改动;旧配置文件里也可能存着更新
    /// 版本写下的落点)。如果按「勾了什么存什么」重建整个 `apply_to`,用户随便
    /// 改一下勾选、保存,那些落点就被静默剥掉了 —— 而且用户完全看不出。
    ///
    /// 这里拿 `Tab` 当样本(F36 落地后它已经有勾选框了,但这条测试**不动别的
    /// 落点**,断言的是「操作 A 不该影响 B」这条不变量,与 UI 展示了哪几个无关)。
    #[test]
    fn set_color_target_preserves_targets_the_ui_does_not_show() {
        let mut spec = ColorSpec {
            hex: "#e06767".into(),
            apply_to: vec![ColorTarget::Tab, ColorTarget::ListItem],
        };
        // 用户取消勾选「会话列表」、勾上「状态栏」
        set_color_target(&mut spec, ColorTarget::ListItem, false);
        set_color_target(&mut spec, ColorTarget::StatusBar, true);
        assert!(
            spec.apply_to.contains(&ColorTarget::Tab),
            "UI 上没有勾选框的 Tab 必须原样保留,不能被静默剥掉"
        );
        assert!(!spec.apply_to.contains(&ColorTarget::ListItem));
        assert!(spec.apply_to.contains(&ColorTarget::StatusBar));
    }

    /// 重复勾选不产生重复项 —— `apply_to` 是集合语义,存成 `Vec` 只是因为
    /// toml 没有集合类型。
    #[test]
    fn set_color_target_is_idempotent() {
        let mut spec = ColorSpec {
            hex: "#e06767".into(),
            apply_to: vec![],
        };
        set_color_target(&mut spec, ColorTarget::ListItem, true);
        set_color_target(&mut spec, ColorTarget::ListItem, true);
        assert_eq!(spec.apply_to, vec![ColorTarget::ListItem]);
        set_color_target(&mut spec, ColorTarget::ListItem, false);
        set_color_target(&mut spec, ColorTarget::ListItem, false);
        assert!(spec.apply_to.is_empty());
    }

    /// 取消勾选所有落点**不清除颜色**。`ColorSpec { hex, apply_to: [] }` 是
    /// 合法状态 =「色留着,暂时哪都不显示」——与跳板「切到无/继承时链条缓冲
    /// 不清空」同一条原则:用户切走再切回,配的东西还在。
    #[test]
    fn clearing_all_targets_keeps_the_color_itself() {
        let mut spec = ColorSpec {
            hex: "#e06767".into(),
            apply_to: vec![ColorTarget::ListItem],
        };
        set_color_target(&mut spec, ColorTarget::ListItem, false);
        assert!(spec.apply_to.is_empty());
        assert_eq!(spec.hex, "#e06767", "颜色本身必须留着");
    }

    /// 编辑外观必须让表单变脏 —— 否则用户改完颜色直接切到别的会话,改动
    /// 被静默丢弃,连确认框都不弹。`EditorBuffer` derive 了 `PartialEq`,
    /// `preserved_appearance` 是它的字段,所以这是白拿的;这条测试钉死
    /// 「白拿」这件事不会在将来某次重构里被拿走(比如把 appearance 挪进
    /// 一个不参与比对的旁路结构)。
    #[test]
    fn editing_appearance_makes_the_form_dirty() {
        let baseline = EditorBuffer::default();
        let mut buf = baseline.clone();
        buf.preserved_appearance.color = Some(ColorSpec {
            hex: "#e06767".into(),
            apply_to: vec![ColorTarget::ListItem],
        });
        assert!(
            is_dirty(&buf, &baseline),
            "改了外观表单必须判脏,否则切换会话时改动被静默丢弃"
        );
    }

    /// **F120 补零覆盖**:`is_dirty` 靠 `EditorBuffer` 整体 derive 的
    /// `PartialEq` 白拿到了 SFTP 三个字段(`sftp_default_remote`/
    /// `sftp_default_local`/`sftp_bookmarks`)的比对,这件事本身没有任何断言
    /// 撑着。复核实测:把这三个字段从比对里剔除,全 workspace 零变红。
    ///
    /// 逐字段验:三个字段各改一次都要单独判脏,不能只改一个就收工——否则
    /// 挡不住「漏了其中一个字段」这种局部退化。
    ///
    /// 自证会变红:给 `EditorBuffer` 手写一份跳过这三个字段的 `PartialEq`
    /// (或者把它们挪进一个不参与 derive 比对的旁路结构),同 `preserved_appearance`
    /// 那条测试文档里提到的风险。
    #[test]
    fn editing_sftp_prefs_makes_the_form_dirty_field_by_field() {
        let baseline = EditorBuffer::default();

        let mut remote_changed = baseline.clone();
        remote_changed.sftp_default_remote = "/srv/app".into();
        assert!(
            is_dirty(&remote_changed, &baseline),
            "改了默认远端目录必须判脏"
        );

        let mut local_changed = baseline.clone();
        local_changed.sftp_default_local = r"D:\work".into();
        assert!(
            is_dirty(&local_changed, &baseline),
            "改了默认本地目录必须判脏"
        );

        let mut bookmarks_changed = baseline.clone();
        bookmarks_changed.sftp_bookmarks = vec![("日志".into(), "/var/log".into())];
        assert!(
            is_dirty(&bookmarks_changed, &baseline),
            "改了书签列表必须判脏"
        );
    }

    /// 造一张真 .ico 的原始字节。
    fn ico_bytes() -> Vec<u8> {
        let px: Vec<u8> = std::iter::repeat_n([7u8, 8, 9, 255], 32 * 32)
            .flatten()
            .collect();
        let img = ico::IconImage::from_rgba_data(32, 32, px);
        let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
        dir.add_entry(ico::IconDirEntry::encode_as_png(&img).unwrap());
        let mut raw = Vec::new();
        dir.write(&mut raw).unwrap();
        raw
    }

    /// 导入成功要写成 `Ico`,并且**保住已挑好的底色** —— 换一张图不等于要
    /// 重挑一次底色。
    #[test]
    fn importing_an_icon_keeps_the_background_colour_you_already_picked() {
        let mut buf = EditorBuffer {
            preserved_appearance: AppearancePrefs {
                icon: Some(IconSpec {
                    kind: IconKind::Ico,
                    value: "旧的".into(),
                    bg: Some("#123456".into()),
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        let raw = ico_bytes();
        import_icon_file(&mut buf, Path::new("a.ico"), |_| Ok(raw)).expect("这是一张真图标");
        let icon = buf.preserved_appearance.icon.as_ref().unwrap();
        assert_eq!(icon.kind, IconKind::Ico);
        assert_ne!(icon.value, "旧的", "新图标没写进去");
        assert_eq!(icon.bg.as_deref(), Some("#123456"), "底色被导入顺手清掉了");
    }

    /// 导入**失败**不能动已有的图标。选错文件的代价应该只是一条提示,
    /// 不能顺手把用户之前设好的图标弄没了。
    #[test]
    fn a_failed_import_leaves_the_existing_icon_alone() {
        for (label, read) in [
            (
                "读不了文件",
                Box::new(|_: &Path| Err(std::io::Error::other("坏了")))
                    as Box<dyn FnOnce(&Path) -> std::io::Result<Vec<u8>>>,
            ),
            ("不是 ico", Box::new(|_: &Path| Ok(b"not an icon".to_vec()))),
        ] {
            let mut buf = EditorBuffer {
                preserved_appearance: AppearancePrefs {
                    icon: Some(IconSpec {
                        kind: IconKind::Ico,
                        value: "旧的".into(),
                        bg: None,
                    }),
                    ..Default::default()
                },
                ..Default::default()
            };
            let err = import_icon_file(&mut buf, Path::new("a.ico"), read)
                .expect_err("{label} 这条本该失败");
            assert!(!err.is_empty(), "{label}:失败必须给一句能看懂的话");
            assert_eq!(
                buf.preserved_appearance
                    .icon
                    .as_ref()
                    .map(|i| i.value.as_str()),
                Some("旧的"),
                "{label}:导入失败不该把原来的图标弄没"
            );
        }
    }

    /// F74:表单切到「共享凭据」档 → 草稿产出 `Auth::Ref`,并且会话自己那三个
    /// 身份槽位一律 `Clear`(设计 D4)。
    ///
    /// 只断 `Auth::Ref` 不够:身份槽位若走 `Keep`,secrets.enc 里会留下三条谁也
    /// 不会去读的孤儿明文 —— 用户以为「密码搬到凭据里了」,旧密码其实还躺在盘上。
    /// 代理口令归会话不归凭据,必须**不**被清掉,否则改一次来源就得重填代理口令。
    #[test]
    fn shared_source_builds_a_reference_and_clears_the_sessions_own_identity_secrets() {
        let mut b = buf();
        b.password = "旧密码".into();
        b.password_touched = true;
        b.proxy_password = "代理口令".into();
        b.proxy_password_touched = true;
        b.cred_source = CredSourceUi::Shared;
        b.credential_id = Some(CredentialId(7));

        let draft = build_draft(&b).expect("选好了凭据就该能存");
        assert_eq!(
            draft.auth,
            Auth::Ref(CredentialId(7)),
            "共享档的草稿必须是引用,不能又落一份内联身份"
        );

        let (password, passphrase, proxy_password, private_key) = secret_fields(&b);
        assert_eq!(password, SecretField::Clear, "会话自己的密码必须清掉");
        assert_eq!(passphrase, SecretField::Clear);
        assert_eq!(private_key, SecretField::Clear);
        assert_eq!(
            proxy_password,
            SecretField::Set("代理口令".into()),
            "代理口令归会话,不该被凭据档连坐清掉"
        );
    }

    /// F74:来源切到共享再切回独有,用户名得还在。
    ///
    /// 共享档界面上不画用户名那一行,但缓冲里的 `user` 不能跟着被抹掉 ——
    /// 否则用户只是点开下拉看了一眼又切回来,原来的用户名就没了,保存按钮
    /// 突然变灰,他还得回想自己本来填的是谁。
    #[test]
    fn switching_back_to_own_source_restores_the_original_user() {
        let mut b = buf();
        b.cred_source = CredSourceUi::Shared;
        b.credential_id = Some(CredentialId(7));
        // 用户改了主意,切回「本会话独有」。
        b.cred_source = CredSourceUi::Own;

        let draft = build_draft(&b).expect("切回独有档后应能存");
        let inline = draft.auth.as_inline().expect("独有档应产出内联身份");
        assert_eq!(inline.user, "user", "切来源不该把用户名擦掉");
    }

    /// F74:共享档没挑具体哪一份凭据 → `build_draft` 硬失败。
    ///
    /// 保存按钮此时是禁用的(`validate::check`),但键盘保存那条路径够不着按钮
    /// 状态,`build_draft` 必须自己是全函数。悄悄退回内联身份是最坏的处置:
    /// 存进去的是一条用户名为空、没有任何凭据的会话,拨号时才炸。
    #[test]
    fn shared_source_without_a_chosen_credential_refuses_to_build() {
        let mut b = buf();
        b.cred_source = CredSourceUi::Shared;
        b.credential_id = None;

        // `SessionDraft` 没有 Debug(它带明文口令),不能用 `expect_err`。
        match build_draft(&b) {
            Err(msg) => assert!(!msg.is_empty(), "失败必须给一句能看懂的话"),
            Ok(_) => panic!("没挑凭据就不该存得下去"),
        }
    }
}
