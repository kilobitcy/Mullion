//! 会话管理弹窗:列表 + CRUD + 编辑表单(Task 6,§4.3/§1.2)。
//!
//! 关键约束:这里渲染在 `app.rs` 的 `egui_ctx.run(|ctx| ...)` 闭包内,只能拿到
//! `&mut UiState`,拿不到 `&mut SessionStore`(否则借用检查器过不了)。所以任何会
//! 改 store / 发起连接的动作,这里只写「意图」到 `UiState`,由 `app.rs` 在
//! `render_frame` 返回、借用释放之后统一施加——与既有 `request_disconnect`/
//! `request_quit` 完全同构。

use std::path::PathBuf;

use mullion_store::{
    AppearancePrefs, Auth, AuthKind, Connection, GroupId, GroupRecord, Identity, NetworkPrefs,
    Protocol, SecretEntry, SessionDraft, SessionId, SessionRecord, TerminalPrefs,
};

use super::UiState;

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

/// 编辑表单的跨帧字段缓冲。端口用 `String`(保存时才 `parse`),密码/口令是明文
/// 缓冲(仅存在于本进程内存,保存后随 `SaveIntent` 一次性转移给 store 加密)。
///
/// **不 derive Debug**:`password`/`passphrase`/`proxy_password` 三个字段是明文,
/// derive 会让 `{:?}` 把它们打印出来。目前全仓没有调用点,但属休眠风险
/// (对照 `mullion_ssh::hop::Hop` 同样的手写打码 Debug)。见下方手写实现。
#[derive(Clone)]
pub struct EditorBuffer {
    pub name: String,
    pub host: String,
    pub port: String,
    pub protocol: Protocol,
    pub user: String,
    pub note: String,
    pub auth_kind: AuthKindUi,
    pub password: String,
    pub key_path: String,
    pub passphrase: String,

    pub proxy_mode: ProxyModeUi,
    pub proxy_host: String,
    pub proxy_port: String,
    pub proxy_user: String,
    pub proxy_password: String,
    /// 跳板链,按拨号顺序。UI 用下拉逐个添加/删除。
    pub jump_chain: Vec<SessionId>,
    /// 跳板链是否被用户显式设过。`false` = 沿用继承(写回 `None`)。
    pub jump_set: bool,

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
            key_path: String::new(),
            passphrase: String::new(),
            proxy_mode: ProxyModeUi::Inherit,
            proxy_host: String::new(),
            proxy_port: "1080".to_string(),
            proxy_user: String::new(),
            proxy_password: String::new(),
            jump_chain: Vec::new(),
            jump_set: false,
            preserved_group_id: None,
            preserved_tags: Vec::new(),
            preserved_terminal: TerminalPrefs::default(),
            preserved_appearance: AppearancePrefs::default(),
        }
    }
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
            .field("key_path", &self.key_path)
            .field("passphrase", &redacted(&self.passphrase))
            .field("proxy_mode", &self.proxy_mode)
            .field("proxy_host", &self.proxy_host)
            .field("proxy_port", &self.proxy_port)
            .field("proxy_user", &self.proxy_user)
            .field("proxy_password", &redacted(&self.proxy_password))
            .field("jump_chain", &self.jump_chain)
            .field("jump_set", &self.jump_set)
            .field("preserved_group_id", &self.preserved_group_id)
            .field("preserved_tags", &self.preserved_tags)
            .field("preserved_terminal", &self.preserved_terminal)
            .field("preserved_appearance", &self.preserved_appearance)
            .finish()
    }
}

impl EditorBuffer {
    /// 把已有会话的非敏感字段填入表单(密码/口令 store 不会明文回吐,留空 ——
    /// 编辑时留空 = 不改;见 `build_draft` 的说明)。
    fn from_record(rec: &SessionRecord) -> Self {
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
        if let Some(chain) = &rec.network.jump {
            buf.jump_set = true;
            buf.jump_chain = chain.iter().map(|j| j.0).collect();
        }
        match &rec.auth.kind {
            AuthKind::Password => buf.auth_kind = AuthKindUi::Password,
            AuthKind::PublicKey { path, .. } => {
                buf.auth_kind = AuthKindUi::PublicKey;
                buf.key_path = path.display().to_string();
            }
        }
        buf
    }
}

/// 一次「保存」的意图:app 事后据此调用 `store.add`(`editing_id=None`)或
/// `store.update`(`Some(id)`)。
pub struct SaveIntent {
    pub editing_id: Option<SessionId>,
    pub draft: SessionDraft,
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
    let (auth, secret) = match buf.auth_kind {
        AuthKindUi::Password => {
            let secret = if buf.password.is_empty() {
                None
            } else {
                Some(SecretEntry {
                    password: Some(buf.password.clone()),
                    passphrase: None,
                    proxy_password: None,
                })
            };
            (AuthKind::Password, secret)
        }
        AuthKindUi::PublicKey => {
            let has_passphrase = !buf.passphrase.is_empty();
            let secret = if has_passphrase {
                Some(SecretEntry {
                    password: None,
                    passphrase: Some(buf.passphrase.clone()),
                    proxy_password: None,
                })
            } else {
                None
            };
            (
                AuthKind::PublicKey {
                    path: PathBuf::from(buf.key_path.trim()),
                    has_passphrase,
                },
                secret,
            )
        }
    };
    // 代理口令与 SSH 凭据存在同一个 SecretEntry 里。即使 SSH 侧没有任何凭据,
    // 只要配了代理口令就得建一个 entry,否则口令无处存。
    let proxy_password = if buf.proxy_password.is_empty() {
        None
    } else {
        Some(buf.proxy_password.clone())
    };
    let secret = match (secret, proxy_password) {
        (Some(mut s), pp) => {
            s.proxy_password = pp;
            Some(s)
        }
        (None, Some(pp)) => Some(SecretEntry {
            password: None,
            passphrase: None,
            proxy_password: Some(pp),
        }),
        (None, None) => None,
    };
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
    let jump = if buf.jump_set {
        Some(
            buf.jump_chain
                .iter()
                .map(|id| mullion_store::JumpRef(*id))
                .collect(),
        )
    } else {
        None
    };
    Ok(SessionDraft {
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
            kind: auth,
        },
        terminal: buf.preserved_terminal.clone(),
        appearance: buf.preserved_appearance.clone(),
        network: NetworkPrefs { proxy, jump },
        secret,
    })
}

/// 每个分组桶对应的 `CollapsingHeader` 构造。抽成独立函数**只为了能在测试里
/// 直接调它**:`CollapsingHeader::new` 默认把标题文本本身当 id 源(见 egui 0.30
/// `collapsing_header.rs`)。两个分组恰好同名(且当前会话数也一样,标题完全
/// 一致)时会撞 id、共享同一份展开/收起状态——编译不报错,跑起来才会看到
/// "点开 A,B 也跟着变"。`.id_salt(gid)` 用分组主键(`None`=未分组桶)强制
/// 区分,彻底绕开标题文本。守护测试
/// `collapsing_header_id_salt_disambiguates_same_titled_groups` 直接调这个函数
/// (不是重抄一遍表达式),删掉 `.id_salt(gid)` 这行测试就会红。
fn group_header(title: &str, gid: Option<GroupId>, count: usize) -> egui::CollapsingHeader {
    egui::CollapsingHeader::new(format!("{title}({count})"))
        .id_salt(gid)
        .default_open(true)
}

/// 会话管理器弹窗:列表 + CRUD 按钮(+ 内嵌二次确认)。`store_available=false`
/// 时(待定 G:keyring/库打开失败)不崩,只展示 `last_error` 或兜底提示。
pub fn show(
    ctx: &egui::Context,
    ui_state: &mut UiState,
    sessions: &[SessionRecord],
    groups: &[GroupRecord],
    store_available: bool,
) {
    let mut open = ui_state.session_manager_open;
    egui::Window::new("会话管理器")
        .open(&mut open)
        .default_width(560.0)
        .show(ctx, |ui| {
            if !store_available {
                let msg = ui_state
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "会话功能不可用".to_string());
                ui.colored_label(egui::Color32::RED, msg);
                return;
            }

            for (gid, bucket) in crate::ui::group_manager::group_sessions(groups, sessions) {
                let title = match gid {
                    Some(id) => groups
                        .iter()
                        .find(|g| g.id == id)
                        .map(|g| g.name.clone())
                        .unwrap_or_else(|| "未分组".to_string()),
                    None => "未分组".to_string(),
                };
                group_header(&title, gid, bucket.len()).show(ui, |ui| {
                    egui::Grid::new(format!("session_list_grid_{gid:?}"))
                        .num_columns(5)
                        .striped(true)
                        .show(ui, |ui| {
                            ui.strong("名称");
                            ui.strong("主机:端口");
                            ui.strong("协议");
                            ui.strong("用户");
                            ui.strong("修改时间");
                            ui.end_row();
                            for rec in &bucket {
                                let is_selected = ui_state.selected == Some(rec.id);
                                let name_resp =
                                    ui.selectable_label(is_selected, &rec.identity.name);
                                if name_resp.clicked() {
                                    ui_state.selected = Some(rec.id);
                                }
                                if name_resp.double_clicked() {
                                    ui_state.connect_request = Some(rec.id);
                                }
                                ui.label(format!(
                                    "{}:{}",
                                    rec.connection.host, rec.connection.port
                                ));
                                ui.label(match rec.connection.protocol {
                                    Protocol::Ssh => "ssh",
                                    Protocol::Sftp => "sftp",
                                });
                                ui.label(&rec.auth.user);
                                ui.label(&rec.modified_at);
                                ui.end_row();
                            }
                        });
                });
            }

            ui.separator();
            // 双击/点连接失败(如 sftp 会话映射拒绝)、保存/删除失败都写
            // ui_state.last_error;这里必须总是渲染,否则「点了没反应」(复核 #2)。
            if let Some(err) = &ui_state.last_error {
                ui.colored_label(egui::Color32::RED, err);
            }
            ui.horizontal(|ui| {
                if ui.button("新建").clicked() {
                    ui_state.editor_id = None;
                    ui_state.editor = EditorBuffer::default();
                    ui_state.editor_open = true;
                }
                let has_selection = ui_state.selected.is_some();
                if ui
                    .add_enabled(has_selection, egui::Button::new("编辑"))
                    .clicked()
                {
                    if let Some(rec) = ui_state
                        .selected
                        .and_then(|id| sessions.iter().find(|s| s.id == id))
                    {
                        ui_state.editor_id = Some(rec.id);
                        ui_state.editor = EditorBuffer::from_record(rec);
                        ui_state.editor_open = true;
                    }
                }
                if ui
                    .add_enabled(has_selection, egui::Button::new("删除"))
                    .clicked()
                {
                    ui_state.pending_delete = ui_state.selected;
                }
                if ui
                    .add_enabled(has_selection, egui::Button::new("连接"))
                    .clicked()
                {
                    ui_state.connect_request = ui_state.selected;
                }
            });

            // 删除二次确认:内嵌一个独立小窗口,确认/取消都不直接碰 store。
            if let Some(id) = ui_state.pending_delete {
                let name = sessions
                    .iter()
                    .find(|s| s.id == id)
                    .map(|s| s.identity.name.clone())
                    .unwrap_or_default();
                egui::Window::new("确认删除")
                    .collapsible(false)
                    .resizable(false)
                    .show(ui.ctx(), |ui| {
                        ui.label(format!("确定删除会话「{name}」?此操作不可撤销。"));
                        ui.horizontal(|ui| {
                            if ui.button("确认删除").clicked() {
                                ui_state.delete_request = Some(id);
                                ui_state.pending_delete = None;
                                if ui_state.selected == Some(id) {
                                    ui_state.selected = None;
                                }
                            }
                            if ui.button("取消").clicked() {
                                ui_state.pending_delete = None;
                            }
                        });
                    });
            }
        });
    ui_state.session_manager_open = open;
    if !open {
        // 列表主窗被关掉:编辑子表单不该变成没有父窗的孤儿窗(复核 #5)。
        ui_state.editor_open = false;
    }

    if ui_state.editor_open {
        show_editor(ctx, ui_state, sessions, groups);
    }
}

/// 新建/编辑子表单。保存时把缓冲组装成 `SaveIntent` 写进 `ui_state.save_request`,
/// 不在这里直接碰 store。
fn show_editor(
    ctx: &egui::Context,
    ui_state: &mut UiState,
    sessions: &[SessionRecord],
    groups: &[GroupRecord],
) {
    let editing_id = ui_state.editor_id;
    let mut open = ui_state.editor_open;
    let title = if ui_state.editor_id.is_some() {
        "编辑会话"
    } else {
        "新建会话"
    };
    egui::Window::new(title)
        .open(&mut open)
        .collapsible(false)
        .show(ctx, |ui| {
            // 在 `buf` 的可变借用块里碰不到 ui_state,先记在局部,出块后再写意图。
            let mut pick_key = false;
            {
                let buf = &mut ui_state.editor;
                egui::Grid::new("editor_form_grid")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("名称");
                        ui.text_edit_singleline(&mut buf.name);
                        ui.end_row();

                        ui.label("主机");
                        ui.text_edit_singleline(&mut buf.host);
                        ui.end_row();

                        ui.label("端口");
                        ui.text_edit_singleline(&mut buf.port);
                        ui.end_row();

                        ui.label("协议");
                        egui::ComboBox::from_id_salt("session_editor_protocol")
                            .selected_text(match buf.protocol {
                                Protocol::Ssh => "ssh",
                                Protocol::Sftp => "sftp",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut buf.protocol, Protocol::Ssh, "ssh");
                                ui.selectable_value(&mut buf.protocol, Protocol::Sftp, "sftp");
                            });
                        ui.end_row();

                        ui.label("用户名");
                        ui.text_edit_singleline(&mut buf.user);
                        ui.end_row();

                        ui.label("备注");
                        ui.text_edit_singleline(&mut buf.note);
                        ui.end_row();

                        ui.label("分组");
                        egui::ComboBox::from_id_salt("editor_group")
                            .selected_text(
                                buf.preserved_group_id
                                    .and_then(|id| groups.iter().find(|g| g.id == id))
                                    .map(|g| g.name.as_str())
                                    .unwrap_or("未分组"),
                            )
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut buf.preserved_group_id, None, "未分组");
                                for g in groups {
                                    ui.selectable_value(
                                        &mut buf.preserved_group_id,
                                        Some(g.id),
                                        &g.name,
                                    );
                                }
                            });
                        ui.end_row();

                        ui.label("认证方式");
                        egui::ComboBox::from_id_salt("session_editor_auth_kind")
                            .selected_text(match buf.auth_kind {
                                AuthKindUi::Password => "密码",
                                AuthKindUi::PublicKey => "公钥",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut buf.auth_kind,
                                    AuthKindUi::Password,
                                    "密码",
                                );
                                ui.selectable_value(
                                    &mut buf.auth_kind,
                                    AuthKindUi::PublicKey,
                                    "公钥",
                                );
                            });
                        ui.end_row();

                        match buf.auth_kind {
                            AuthKindUi::Password => {
                                ui.label("密码");
                                ui.add(
                                    egui::TextEdit::singleline(&mut buf.password).password(true),
                                );
                                ui.end_row();
                            }
                            AuthKindUi::PublicKey => {
                                ui.label("私钥路径");
                                ui.horizontal(|ui| {
                                    ui.text_edit_singleline(&mut buf.key_path);
                                    if ui.button("选择…").clicked() {
                                        pick_key = true;
                                    }
                                });
                                ui.end_row();

                                ui.label("私钥口令");
                                ui.add(
                                    egui::TextEdit::singleline(&mut buf.passphrase).password(true),
                                );
                                ui.end_row();
                            }
                        }

                        ui.label("代理");
                        egui::ComboBox::from_id_salt("editor_proxy_mode")
                            .selected_text(match buf.proxy_mode {
                                ProxyModeUi::Inherit => "跟随分组",
                                ProxyModeUi::Direct => "不使用代理",
                                ProxyModeUi::Socks5 => "SOCKS5",
                                ProxyModeUi::HttpConnect => "HTTP CONNECT",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut buf.proxy_mode,
                                    ProxyModeUi::Inherit,
                                    "跟随分组",
                                );
                                ui.selectable_value(
                                    &mut buf.proxy_mode,
                                    ProxyModeUi::Direct,
                                    "不使用代理",
                                );
                                ui.selectable_value(
                                    &mut buf.proxy_mode,
                                    ProxyModeUi::Socks5,
                                    "SOCKS5",
                                );
                                ui.selectable_value(
                                    &mut buf.proxy_mode,
                                    ProxyModeUi::HttpConnect,
                                    "HTTP CONNECT",
                                );
                            });
                        ui.end_row();

                        if matches!(
                            buf.proxy_mode,
                            ProxyModeUi::Socks5 | ProxyModeUi::HttpConnect
                        ) {
                            ui.label("代理地址");
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut buf.proxy_host)
                                        .desired_width(160.0),
                                );
                                ui.label(":");
                                ui.add(
                                    egui::TextEdit::singleline(&mut buf.proxy_port)
                                        .desired_width(60.0),
                                );
                            });
                            ui.end_row();

                            ui.label("代理用户名");
                            ui.text_edit_singleline(&mut buf.proxy_user);
                            ui.end_row();

                            ui.label("代理口令");
                            ui.add(
                                egui::TextEdit::singleline(&mut buf.proxy_password).password(true),
                            );
                            ui.end_row();
                        }

                        ui.label("跳板");
                        ui.vertical(|ui| {
                            if !buf.jump_set {
                                ui.horizontal(|ui| {
                                    ui.label("跟随分组");
                                    if ui.button("改为自定义").clicked() {
                                        buf.jump_set = true;
                                    }
                                });
                            } else {
                                let mut remove_at = None;
                                for (i, id) in buf.jump_chain.iter().enumerate() {
                                    ui.horizontal(|ui| {
                                        let name = sessions
                                            .iter()
                                            .find(|r| r.id == *id)
                                            .map(|r| r.identity.name.clone())
                                            // 悬空引用在 UI 上就点出来,不要等到连接时才报错。
                                            .unwrap_or_else(|| format!("<已删除的会话 {:?}>", id));
                                        ui.label(format!("{}. {name}", i + 1));
                                        if ui.button("移除").clicked() {
                                            remove_at = Some(i);
                                        }
                                    });
                                }
                                if let Some(i) = remove_at {
                                    buf.jump_chain.remove(i);
                                }
                                egui::ComboBox::from_id_salt("editor_jump_add")
                                    .selected_text("添加跳板…")
                                    .show_ui(ui, |ui| {
                                        for rec in sessions {
                                            // 不能把自己当自己的跳板(那是环)。
                                            if Some(rec.id) == editing_id {
                                                continue;
                                            }
                                            if ui.button(&rec.identity.name).clicked() {
                                                buf.jump_chain.push(rec.id);
                                            }
                                        }
                                    });
                                if ui.button("恢复为跟随分组").clicked() {
                                    buf.jump_set = false;
                                    buf.jump_chain.clear();
                                }
                            }
                        });
                        ui.end_row();
                    });
            }
            if pick_key {
                ui_state.pick_key_request = true;
            }
            ui.label("留空密码 / 私钥口令 = 清除已存凭据(不是「保持不变」)。");

            if let Some(err) = &ui_state.last_error {
                ui.colored_label(egui::Color32::RED, err);
            }

            ui.horizontal(|ui| {
                if ui.button("保存").clicked() {
                    match build_draft(&ui_state.editor) {
                        Ok(draft) => {
                            ui_state.last_error = None;
                            ui_state.save_request = Some(SaveIntent {
                                editing_id: ui_state.editor_id,
                                draft,
                            });
                            ui_state.editor_open = false;
                            // 别让刚输入的明文密码/口令原样滞留在 UiState 内存里(复核 #6)。
                            ui_state.editor = EditorBuffer::default();
                        }
                        Err(e) => ui_state.last_error = Some(e),
                    }
                }
                if ui.button("取消").clicked() {
                    ui_state.editor_open = false;
                    ui_state.editor = EditorBuffer::default();
                }
            });
        });
    ui_state.editor_open = ui_state.editor_open && open;
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
            key_path: String::new(),
            passphrase: String::new(),
            ..EditorBuffer::default()
        }
    }

    #[test]
    fn password_session_builds_draft_with_secret() {
        let mut b = buf();
        b.password = "pw".into();
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

    #[test]
    fn password_session_with_empty_password_clears_secret() {
        let b = buf(); // password 留空
        let draft = build_draft(&b).unwrap();
        assert!(draft.secret.is_none(), "留空密码应清除已存凭据");
    }

    #[test]
    fn pubkey_with_passphrase_sets_has_passphrase_and_secret() {
        let mut b = buf();
        b.auth_kind = AuthKindUi::PublicKey;
        b.key_path = "/home/me/id_ed25519".into();
        b.passphrase = "ph".into();
        let draft = build_draft(&b).unwrap();
        match draft.auth.kind {
            AuthKind::PublicKey {
                path,
                has_passphrase,
            } => {
                assert_eq!(path, PathBuf::from("/home/me/id_ed25519"));
                assert!(has_passphrase);
            }
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
        b.key_path = "/home/me/id_ed25519".into();
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

    /// 复审坑:`egui::CollapsingHeader::new(text)` 默认把标题文本本身当 id 源
    /// (`egui-0.30.0/src/containers/collapsing_header.rs::new`)。两个分组
    /// 名字相同、桶内会话数也相同时,列表里两个 header 的标题文本会完全一致
    /// ——不加 `.id_salt` 就会撞 id、共享同一份展开/收起状态,点开一个另一个
    /// 也跟着变。这条测试直接调 `group_header`(`show()` 内部实际用的同一个
    /// 函数,不是重抄一遍表达式),同一个父 `ui`、相同标题、不同 `gid`:
    /// 去掉 `group_header` 里的 `.id_salt(gid)` 这行,两个 `header_response.id`
    /// 会相等,下面的 `assert_ne!` 就会失败(已实测确认,见提交说明)。
    #[test]
    fn collapsing_header_id_salt_disambiguates_same_titled_groups() {
        let ctx = egui::Context::default();
        let mut ids: Option<(egui::Id, egui::Id)> = None;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let resp_a = group_header("生产", Some(GroupId(1)), 1).show(ui, |_| {});
                let resp_b = group_header("生产", Some(GroupId(2)), 1).show(ui, |_| {});
                ids = Some((resp_a.header_response.id, resp_b.header_response.id));
            });
        });
        let (id_a, id_b) = ids.expect("闭包必须跑到底,写回 ids");
        assert_ne!(
            id_a, id_b,
            "两个分组标题相同时,header 的持久化 id 必须靠 gid 区分,否则展开状态会互相串"
        );
    }
}
