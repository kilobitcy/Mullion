//! 隧道编辑器:右栏在「隧道」模式下的表单。
//!
//! 与会话编辑器同构 —— UI 只写意图到 `UiState`,由 `app.rs` 在借用释放后施加。
//! 本文件先只放跨帧缓冲与意图类型(Task 4),渲染在 Task 6。

use mullion_store::{SessionId, SessionRecord, TunnelDraft, TunnelId, TunnelKind};

use crate::theme::{self, Theme};
use crate::ui::metrics::{field_w, FIELD_W_L, FIELD_W_M, FIELD_W_S};
use crate::ui::session_manager::form;
use crate::ui::UiState;

/// 转发类型的 UI 侧选择。
///
/// 不直接用 `TunnelKind`:那是带数据枚举,而下拉框要在用户**还没填目标**时
/// 就能选中一个类型。UI 侧用无数据的三态,提交时才与缓冲里的目标字段合成
/// 真正的 `TunnelKind`(见 `build_tunnel_draft`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelKindUi {
    Local,
    Remote,
    Dynamic,
}

impl TunnelKindUi {
    /// 下拉框里的显示名。方向词是这个功能最容易填反的地方,标签里就写死方向。
    pub fn label(self) -> &'static str {
        match self {
            TunnelKindUi::Local => "本地转发 (-L)",
            TunnelKindUi::Remote => "远程转发 (-R)",
            TunnelKindUi::Dynamic => "动态转发 (-D, SOCKS5)",
        }
    }
}

/// 隧道表单的跨帧缓冲。
///
/// 端口存字符串而非 `u16`:与 `EditorBuffer::port` 同一理由 —— 文本框要允许
/// 用户敲到一半的非法值,校验发生在保存时,不能靠类型把中间态卡死。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelEditorBuffer {
    /// 提供拨号目标与凭据的会话。悬垂时**保持原值**,不静默跳到第一条。
    pub session_id: Option<SessionId>,
    pub kind: TunnelKindUi,
    pub listen_port: String,
    pub target_host: String,
    pub target_port: String,
    /// 允许本机以外的主机连这个侦听端口。默认 `false`(F117)。
    /// `Dynamic` 下这一位**不写回** —— `TunnelKind::Dynamic` 没有这个字段。
    pub expose: bool,
    pub note: String,
    pub autostart: bool,
}

impl Default for TunnelEditorBuffer {
    fn default() -> Self {
        Self {
            session_id: None,
            kind: TunnelKindUi::Local,
            listen_port: String::new(),
            target_host: String::new(),
            target_port: String::new(),
            // 安全默认:新建隧道一律仅本机可连。
            expose: false,
            note: String::new(),
            autostart: false,
        }
    }
}

impl TunnelEditorBuffer {
    /// 从已有记录回填。
    pub fn from_record(rec: &mullion_store::TunnelRecord) -> Self {
        let (kind, target_host, target_port, expose) = match &rec.kind {
            TunnelKind::Local {
                target_host,
                target_port,
                expose,
            } => (
                TunnelKindUi::Local,
                target_host.clone(),
                target_port.to_string(),
                *expose,
            ),
            TunnelKind::Remote {
                target_host,
                target_port,
                expose,
            } => (
                TunnelKindUi::Remote,
                target_host.clone(),
                target_port.to_string(),
                *expose,
            ),
            TunnelKind::Dynamic => (TunnelKindUi::Dynamic, String::new(), String::new(), false),
        };
        Self {
            session_id: Some(rec.session_id),
            kind,
            listen_port: rec.listen_port.to_string(),
            target_host,
            target_port,
            expose,
            note: rec.note.clone(),
            autostart: rec.autostart,
        }
    }
}

/// 保存意图。`editing_id = None` 即新建。
pub struct TunnelSaveIntent {
    pub editing_id: Option<TunnelId>,
    pub draft: TunnelDraft,
}

/// 缓冲 → `TunnelDraft`。纯函数,不碰 egui,可脱离 GUI 单测。
///
/// `Dynamic` 分支**不读** `target_*`/`expose`:那三个值在类型上没有去处,
/// 读了也写不进去。这正是 D5「`-D` 锁死本机」由编译器兜住的地方。
pub fn build_tunnel_draft(buf: &TunnelEditorBuffer) -> Result<TunnelDraft, String> {
    let session_id = buf.session_id.ok_or("请先选择这条隧道要用的会话")?;
    let listen_port = parse_port(&buf.listen_port, "侦听端口")?;
    let kind = match buf.kind {
        TunnelKindUi::Dynamic => TunnelKind::Dynamic,
        TunnelKindUi::Local | TunnelKindUi::Remote => {
            let host = buf.target_host.trim();
            if host.is_empty() {
                return Err("目标主机不能为空".into());
            }
            let target_port = parse_port(&buf.target_port, "目标端口")?;
            let (target_host, expose) = (host.to_string(), buf.expose);
            if buf.kind == TunnelKindUi::Local {
                TunnelKind::Local {
                    target_host,
                    target_port,
                    expose,
                }
            } else {
                TunnelKind::Remote {
                    target_host,
                    target_port,
                    expose,
                }
            }
        }
    };
    Ok(TunnelDraft {
        session_id,
        listen_port,
        note: buf.note.trim().to_string(),
        autostart: buf.autostart,
        kind,
    })
}

/// 端口校验。`0` 不是「随机端口」而是**没填**——SSH 转发里 0 号端口不可侦听,
/// 放过它只会让失败推迟到启动那一刻,错误还出在远端。
fn parse_port(text: &str, what: &str) -> Result<u16, String> {
    let n: u16 = text
        .trim()
        .parse()
        .map_err(|_| format!("{what}必须是 1~65535 的整数"))?;
    if n == 0 {
        return Err(format!("{what}必须是 1~65535 的整数"));
    }
    Ok(n)
}

/// 端口框要不要出内联红字。空着不报(必填星号已经在说这件事),
/// 填了但解析不出合法端口才报。
pub(crate) fn port_field_error(text: &str) -> bool {
    !text.trim().is_empty() && parse_port(text, "端口").is_err()
}

/// 各类型的字段措辞(设计 D12)。
///
/// **标签必须随类型翻转**,不能一套文案套三种转发:`-L` 的「侦听」在本机、
/// `-R` 的在远端;目标主机名 `-L` 由**远端**解析、`-R` 由**本机**解析。
/// 填反了的症状是「隧道起来了但连不通」,而 `-R` 的错误还出在远端 ——
/// 这是本功能最难排查的一类问题,第一道防线只能是标签本身。
struct Wording {
    listen: &'static str,
    listen_hint: &'static str,
    target: &'static str,
    /// 谁来解析目标主机名。
    resolver: &'static str,
    expose: &'static str,
}

fn wording(kind: TunnelKindUi) -> Wording {
    match kind {
        TunnelKindUi::Local => Wording {
            listen: "本机侦听端口",
            listen_hint: "连本机这个端口,流量从远端主机发出",
            target: "目标(从远端看)",
            resolver: "目标主机名由远端主机解析",
            expose: "允许同网段其他主机连这个端口",
        },
        TunnelKindUi::Remote => Wording {
            listen: "远端侦听端口",
            listen_hint: "连远端主机这个端口,流量从本机发出",
            target: "目标(从本机看)",
            resolver: "目标主机名由本机解析",
            expose: "请求远端侦听所有网卡(受服务端 GatewayPorts 限制)",
        },
        TunnelKindUi::Dynamic => Wording {
            listen: "本机 SOCKS5 端口",
            listen_hint: "把浏览器/工具的 SOCKS5 代理指向本机这个端口",
            target: "",
            resolver: "",
            expose: "",
        },
    }
}

/// 勾了「允许对外」之后那一句话的语气。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExposeTone {
    /// 事实上已经开了口子,后果由用户承担。
    Danger,
    /// 我们请求了,但**验证不了**是否生效。
    Unverifiable,
}

/// 勾了 `expose` 之后该说什么(设计 D5/D9)。纯函数,便于把「说了没说」钉死。
///
/// `-L` 和 `-R` 必须是**两句不同性质的话**:
/// - `-L` 的口子开在本机,一定生效 —— 说的是风险,要点名具体目标。
/// - `-R` 的口子开在远端,`tcpip-forward` 的协议应答只回端口号、不回实际
///   绑定地址,sshd 默认 `GatewayPorts no` 会静默降级成回环。客户端
///   **结构性地无法验证** —— 说的是「别当成已经生效」。
///   写成跟 `-L` 一样的口气就是在给用户一个我们证明不了的安全承诺。
pub(crate) fn expose_warning(buf: &TunnelEditorBuffer) -> Option<(ExposeTone, String)> {
    if !buf.expose {
        return None;
    }
    let or_blank = |s: &str| {
        if s.trim().is_empty() {
            "(未填)".to_string()
        } else {
            s.trim().to_string()
        }
    };
    match buf.kind {
        // 类型上就没有 `expose` 字段,这一段整块不画。
        TunnelKindUi::Dynamic => None,
        TunnelKindUi::Local => Some((
            ExposeTone::Danger,
            format!(
                "同网段任何人都能通过本机 {} 端口访问 {}:{} —— 这条转发不做任何认证。",
                or_blank(&buf.listen_port),
                or_blank(&buf.target_host),
                or_blank(&buf.target_port)
            ),
        )),
        TunnelKindUi::Remote => Some((
            ExposeTone::Unverifiable,
            format!(
                "已请求远端在所有网卡上侦听 {};是否真的生效取决于服务端的 GatewayPorts 配置,\
                 协议只回端口号不回绑定地址,本客户端无法验证。",
                or_blank(&buf.listen_port)
            ),
        )),
    }
}

/// 「经由会话」下拉该显示什么，以及要不要在旁边出一行红字。
///
/// 三态必须分开说：**没选**、**引用的会话被删了**、**引用的是 SFTP 节点**。
/// 后两者的修法完全不同（重建会话 vs 改选一条 SSH 会话），合成一句
/// 「引用无效」等于让用户自己猜。
///
/// 悬垂时**保持原值**不静默改选：静默跳到第一条会把这条隧道悄悄接到另一台
/// 机器上（切片 T-a 的 D3 已经定死了这条）。
pub(crate) fn session_ref_label(
    id: Option<SessionId>,
    sessions: &[SessionRecord],
    credentials: &[mullion_store::CredentialRecord],
) -> (String, Option<&'static str>) {
    use mullion_store::Protocol;
    let Some(id) = id else {
        return ("(未选择)".to_string(), None);
    };
    match sessions.iter().find(|s| s.id == id) {
        // F143:原先两条前缀的警告符 U+26A0 不在 GBK,是豆腐块。纯字符串,
        // 没有自绘余地,改中文措辞。
        None => (
            format!("（已删除）会话 id={}", id.0),
            Some("引用的会话已删除"),
        ),
        Some(s) if s.connection.protocol != Protocol::Ssh => (
            format!("（不可用）{}（SFTP 节点）", s.identity.name),
            Some("该会话是 SFTP 节点，不能用于转发"),
        ),
        Some(s) => (
            format!(
                "{} ({}@{})",
                s.identity.name,
                mullion_store::display_user(&s.auth, credentials),
                s.connection.host
            ),
            None,
        ),
    }
}

/// 「经由会话」下拉能选的会话。
///
/// 只列 SSH:隧道走的是 `direct-tcpip` / `tcpip-forward`,SFTP 节点跟它
/// 没关系(D6)。拆成纯函数是因为 egui 的 `ComboBox` 候选只在**点击展开**
/// 后才画,渲染测试探不到这段逻辑有没有生效,只能直接测这个函数本身。
pub(crate) fn ssh_candidates(sessions: &[SessionRecord]) -> Vec<&SessionRecord> {
    sessions
        .iter()
        .filter(|s| s.connection.protocol == mullion_store::Protocol::Ssh)
        .collect()
}

/// 右栏在隧道模式下的渲染。
pub(super) fn show(
    ui: &mut egui::Ui,
    t: &Theme,
    ui_state: &mut UiState,
    sessions: &[SessionRecord],
    credentials: &[mullion_store::CredentialRecord],
) {
    let Some(buf) = ui_state.tunnel_editor.as_mut() else {
        ui.colored_label(
            theme::c32(t.fg_dimmer),
            "从左边选一条隧道,或点「+ 新建隧道」。",
        );
        return;
    };

    // 页面级游标:整张表单共用一个,`section()` 靠它决定「首个分区不画线」。
    // 见 form.rs 里 `section` 的文档注释——**不许**在子块里另起 `let mut first = true`。
    let mut first = true;
    let w = wording(buf.kind);

    form::section(ui, t, "会话管理器/右栏", "转发", &mut first);
    form::grid(ui, "tunnel_forward", |ui| {
        form::required(ui, t, "类型");
        egui::ComboBox::from_id_salt("tunnel_kind")
            .selected_text(buf.kind.label())
            .show_ui(ui, |ui| {
                for k in [
                    TunnelKindUi::Local,
                    TunnelKindUi::Remote,
                    TunnelKindUi::Dynamic,
                ] {
                    // 切类型**不清空** `listen_port`/`note`:那两个字段三种类型
                    // 都有,切一下就把用户填的端口抹掉是无谓的返工。
                    ui.selectable_value(&mut buf.kind, k, k.label());
                }
            });
        ui.end_row();

        form::required(ui, t, "经由会话");
        let (selected, err) = session_ref_label(buf.session_id, sessions, credentials);
        egui::ComboBox::from_id_salt("tunnel_session")
            .selected_text(selected)
            .show_ui(ui, |ui| {
                for s in ssh_candidates(sessions) {
                    let label = format!(
                        "{} ({}@{})",
                        s.identity.name,
                        mullion_store::display_user(&s.auth, credentials),
                        s.connection.host
                    );
                    ui.selectable_value(&mut buf.session_id, Some(s.id), label);
                }
            });
        ui.end_row();
        form::field_error(ui, t, err.is_some(), err.unwrap_or_default());
    });

    form::section(ui, t, "会话管理器/右栏", "侦听", &mut first);
    form::grid(ui, "tunnel_listen", |ui| {
        form::required(ui, t, w.listen);
        ui.add(
            egui::TextEdit::singleline(&mut buf.listen_port).desired_width(field_w(
                ui.available_width(),
                FIELD_W_S,
                0.0,
            )),
        );
        ui.end_row();
        form::field_error(
            ui,
            t,
            port_field_error(&buf.listen_port),
            "侦听端口要填 1~65535 之间的数字",
        );

        // 灰字说明占一整行,左格留空让它跟输入框左对齐(同 `field_error` 的理由:
        // 挂在标签列会被读成「这一行的标签」)。
        ui.label("");
        ui.colored_label(theme::c32(t.fg_dimmer), w.listen_hint);
        ui.end_row();

        // F117 绑定安全。`Dynamic` 这一段整块不画 —— 它在类型上就没有
        // `expose` 字段(见 `TunnelKind::Dynamic`),画一个写不进数据的勾选框
        // 只会让人以为动态转发也能对外开放。
        if buf.kind != TunnelKindUi::Dynamic {
            ui.label("");
            ui.checkbox(&mut buf.expose, w.expose);
            ui.end_row();
            // 警告里带**具体目标/端口**,不是泛泛一句「有风险」:用户要判断的是
            // 「把这台机器暴露出去要不要紧」,没有目标就没法判断。
            // 措辞与语气见纯函数 `expose_warning`(它自己有穷举测试)。
            if let Some((tone, text)) = expose_warning(buf) {
                let color = match tone {
                    ExposeTone::Danger => t.danger,
                    ExposeTone::Unverifiable => t.warn,
                };
                ui.label("");
                ui.colored_label(theme::c32(color), text);
                ui.end_row();
            }
        }
    });

    if buf.kind != TunnelKindUi::Dynamic {
        form::section(ui, t, "会话管理器/右栏", "目标", &mut first);
        form::grid(ui, "tunnel_target", |ui| {
            form::required(ui, t, w.target);
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut buf.target_host)
                        .desired_width(field_w(ui.available_width(), FIELD_W_M, 0.0))
                        .hint_text(theme::hint_text(t, "主机名或 IP")),
                );
                ui.label(":");
                ui.add(
                    egui::TextEdit::singleline(&mut buf.target_port).desired_width(field_w(
                        ui.available_width(),
                        FIELD_W_S,
                        0.0,
                    )),
                );
            });
            ui.end_row();
            form::field_error(
                ui,
                t,
                port_field_error(&buf.target_port),
                "目标端口要填 1~65535 之间的数字",
            );

            ui.label("");
            ui.colored_label(theme::c32(t.fg_dimmer), w.resolver);
            ui.end_row();
        });
    }

    form::section(ui, t, "会话管理器/右栏", "其他", &mut first);
    form::grid(ui, "tunnel_misc", |ui| {
        ui.label("说明");
        ui.add(
            egui::TextEdit::singleline(&mut buf.note)
                .desired_width(field_w(ui.available_width(), FIELD_W_L, 0.0))
                .hint_text(theme::hint_text(t, "这条转发是干什么的(可选)")),
        );
        ui.end_row();
    });

    ui.add_space(crate::ui::metrics::SP_M);
    ui.horizontal(|ui| {
        if ui.button("保存").clicked() {
            ui_state.tunnel_save_click = true;
        }
        if ui_state.tunnel_editor_id.is_some() && ui.button("删除").clicked() {
            ui_state.pending_tunnel_delete = ui_state.tunnel_editor_id;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled(kind: TunnelKindUi) -> TunnelEditorBuffer {
        TunnelEditorBuffer {
            session_id: Some(SessionId(7)),
            kind,
            listen_port: "3306".into(),
            target_host: "db.internal".into(),
            target_port: "3306".into(),
            expose: false,
            note: String::new(),
            autostart: false,
        }
    }

    /// 端口框什么时候该出红字。空着**不报**——必填星号已经说明了它必须填,
    /// 一打开表单就红一片是在骂用户还没开始填。填了但非法才报。
    /// 与会话页 `validate::port` 同一口径**仅限于**「不看碰没碰过,填错就是填错了」
    /// 这一点——空值语义两者不同:会话端口有默认值 22(`validate::port("")` 返回
    /// `Ok(22)`),隧道侦听/目标端口没有默认值,空着靠保存时 `parse_port` 的
    /// 必填校验兜底,不是这里悄悄按 22 处理。
    #[test]
    fn port_field_error_only_fires_when_something_invalid_was_typed() {
        assert!(!port_field_error(""), "空着不该报红");
        assert!(!port_field_error("   "), "只有空白也不该报红");
        assert!(!port_field_error("3306"), "合法端口不该报红");
        assert!(port_field_error("0"), "0 不可侦听,必须报红");
        assert!(port_field_error("70000"), "越界必须报红");
        assert!(port_field_error("abc"), "非数字必须报红");
    }

    #[test]
    fn new_buffer_is_local_only_by_default() {
        // F117:新建隧道默认不对外暴露。
        assert!(!TunnelEditorBuffer::default().expose);
    }

    /// 用户填了目标又切到「动态」时,那些值**不能**跟着写进去 ——
    /// `TunnelKind::Dynamic` 没有目标字段,这条钉住「切类型即丢弃不适用的值」。
    #[test]
    fn dynamic_draft_carries_no_target_even_if_the_buffer_still_holds_one() {
        let mut buf = filled(TunnelKindUi::Dynamic);
        buf.listen_port = "1080".into();
        buf.expose = true; // 缓冲里残留的暴露位也必须被丢弃
        let draft = build_tunnel_draft(&buf).unwrap();
        assert_eq!(draft.kind, TunnelKind::Dynamic);
    }

    #[test]
    fn local_and_remote_keep_their_target_and_expose() {
        for (ui, expect_local) in [(TunnelKindUi::Local, true), (TunnelKindUi::Remote, false)] {
            let mut buf = filled(ui);
            buf.expose = true;
            let draft = build_tunnel_draft(&buf).unwrap();
            let ok = match draft.kind {
                TunnelKind::Local {
                    ref target_host,
                    target_port,
                    expose,
                } => expect_local && target_host == "db.internal" && target_port == 3306 && expose,
                TunnelKind::Remote {
                    ref target_host,
                    target_port,
                    expose,
                } => !expect_local && target_host == "db.internal" && target_port == 3306 && expose,
                TunnelKind::Dynamic => false,
            };
            assert!(ok, "{}: 目标或暴露位丢了", ui.label());
        }
    }

    #[test]
    fn empty_target_host_is_rejected_for_local_and_remote() {
        for ui in [TunnelKindUi::Local, TunnelKindUi::Remote] {
            let mut buf = filled(ui);
            buf.target_host = "   ".into();
            assert!(build_tunnel_draft(&buf).is_err(), "{}", ui.label());
        }
        // 动态转发没有目标,空着是正常的。
        let mut d = filled(TunnelKindUi::Dynamic);
        d.target_host = String::new();
        assert!(build_tunnel_draft(&d).is_ok());
    }

    /// 0 号端口不可侦听。放过它,失败会推迟到启动那一刻,而 `-R` 的错误
    /// 还出在远端 —— 那是本功能最难排查的一类症状。
    #[test]
    fn port_zero_is_rejected_not_treated_as_auto() {
        let mut buf = filled(TunnelKindUi::Local);
        buf.listen_port = "0".into();
        assert!(build_tunnel_draft(&buf).is_err());
        let mut buf2 = filled(TunnelKindUi::Local);
        buf2.target_port = "0".into();
        assert!(build_tunnel_draft(&buf2).is_err());
    }

    #[test]
    fn missing_session_reference_is_rejected() {
        let mut buf = filled(TunnelKindUi::Local);
        buf.session_id = None;
        assert!(build_tunnel_draft(&buf).is_err());
    }

    /// D9 的守护。`-R` 勾了对外**不能**沿用 `-L` 那句斩钉截铁的警告 ——
    /// sshd 默认 `GatewayPorts no` 会把 `0.0.0.0` 静默降级成回环,而协议应答
    /// 只回端口号,客户端结构性地验证不了。照抄 `-L` 的口气等于给用户一个
    /// 我们证明不了的安全承诺(spec F43 已经拒绝过这类做法)。
    ///
    /// 自证会变红:把 `Remote` 分支合进 `Local` 分支 —— 下面「必须写明无法
    /// 验证」那条当场红。
    #[test]
    fn remote_expose_says_it_cannot_be_verified_instead_of_promising_it_works() {
        let mut r = filled(TunnelKindUi::Remote);
        r.expose = true;
        r.listen_port = "8080".into();
        let (tone, text) = expose_warning(&r).expect("勾了就必须说话");
        assert_eq!(tone, ExposeTone::Unverifiable);
        assert!(text.contains("无法验证"), "实际: {text}");
        assert!(
            text.contains("GatewayPorts"),
            "要点名是服务端哪个配置在管这件事,实际: {text}"
        );

        let mut l = filled(TunnelKindUi::Local);
        l.expose = true;
        let (tone, text) = expose_warning(&l).expect("勾了就必须说话");
        assert_eq!(tone, ExposeTone::Danger, "本机的口子是**一定**开了的");
        assert!(
            text.contains("db.internal"),
            "本机侧要点名具体目标,否则用户没法判断要不要紧,实际: {text}"
        );
        assert!(
            !text.contains("无法验证"),
            "本机侧是确定生效的,不许含糊,实际: {text}"
        );
    }

    #[test]
    fn nothing_is_said_when_the_box_is_unchecked_or_the_kind_has_no_box() {
        assert!(expose_warning(&filled(TunnelKindUi::Local)).is_none());
        let mut d = filled(TunnelKindUi::Dynamic);
        d.expose = true; // 缓冲里残留的位不该让动态转发也画出一句警告
        assert!(expose_warning(&d).is_none());
    }

    #[test]
    fn from_record_round_trips_through_the_buffer() {
        let rec = mullion_store::TunnelRecord {
            id: TunnelId(1),
            session_id: SessionId(7),
            listen_port: 8080,
            note: "内网 web".into(),
            autostart: false,
            kind: TunnelKind::Remote {
                target_host: "127.0.0.1".into(),
                target_port: 3000,
                expose: true,
            },
        };
        let buf = TunnelEditorBuffer::from_record(&rec);
        let draft = build_tunnel_draft(&buf).unwrap();
        assert_eq!(draft.session_id, rec.session_id);
        assert_eq!(draft.listen_port, rec.listen_port);
        assert_eq!(draft.note, rec.note);
        assert_eq!(draft.kind, rec.kind);
    }

    use mullion_store::SessionRecord;

    /// D6:隧道走的是 `direct-tcpip` / `tcpip-forward`,跟 SFTP 节点没关系。
    /// 候选里列出 SFTP 节点,用户选了之后隧道永远起不来,而错误要等到启动
    /// 那一刻才出现。
    #[test]
    fn session_ref_label_distinguishes_deleted_from_sftp() {
        let ssh = sess_rec(1, "生产主控", mullion_store::Protocol::Ssh);
        let sftp = sess_rec(2, "文件中转", mullion_store::Protocol::Sftp);
        let all = vec![ssh.clone(), sftp.clone()];

        let (text, err) = session_ref_label(Some(SessionId(1)), &all, &[]);
        assert!(text.contains("生产主控"), "实际: {text}");
        assert!(err.is_none(), "正常引用不该报错");

        let (text, err) = session_ref_label(Some(SessionId(9)), &all, &[]);
        assert!(text.contains("已删除"), "实际: {text}");
        assert_eq!(err, Some("引用的会话已删除"));

        // 「删掉了」和「是 SFTP 节点」是两种完全不同的修法:前者要重建会话,
        // 后者只需改选一条 SSH 会话。合成一句话等于让用户猜。
        let (text, err) = session_ref_label(Some(SessionId(2)), &all, &[]);
        assert!(text.contains("文件中转"), "实际: {text}");
        assert_eq!(err, Some("该会话是 SFTP 节点，不能用于转发"));

        let (text, err) = session_ref_label(None, &all, &[]);
        assert_eq!(text, "(未选择)");
        assert!(err.is_none());
    }

    fn sess_rec(id: u64, name: &str, protocol: mullion_store::Protocol) -> SessionRecord {
        use mullion_store::model::{Auth, AuthKind, Connection, Identity};
        SessionRecord {
            id: SessionId(id),
            modified_at: "t".into(),
            identity: Identity {
                name: name.into(),
                note: String::new(),
                group_id: None,
                tags: Vec::new(),
            },
            connection: Connection {
                host: "10.0.0.1".into(),
                port: 22,
                protocol,
            },
            auth: Auth::inline("root", AuthKind::Password),
            terminal: Default::default(),
            appearance: Default::default(),
            network: Default::default(),
            automation: Default::default(),
            sftp: Default::default(),
        }
    }

    /// 跑两帧(第一帧让布局稳定),把第二帧画出的所有文字收集起来。
    fn rendered_texts(buf: TunnelEditorBuffer, sessions: &[SessionRecord]) -> Vec<String> {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut st = crate::ui::UiState {
            tunnel_editor: Some(buf),
            ..Default::default()
        };
        let mut texts = Vec::new();
        for _ in 0..2 {
            let out = ctx.run(Default::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(ui, &t, &mut st, sessions, &[]);
                });
            });
            texts = collect_text(&out.shapes);
        }
        texts
    }

    fn collect_text(shapes: &[egui::epaint::ClippedShape]) -> Vec<String> {
        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                egui::Shape::Text(ts) => out.push(ts.galley.text().to_string()),
                _ => {}
            }
        }
        let mut acc = Vec::new();
        shapes.iter().for_each(|cs| walk(&cs.shape, &mut acc));
        acc
    }

    fn has(texts: &[String], needle: &str) -> bool {
        texts.iter().any(|s| s.contains(needle))
    }

    /// 精确匹配,不是 `contains`。四个分节标题("转发"/"侦听"/"目标"/"其他")
    /// 都只有两个字,而它们又恰好是相邻字段措辞里会用到的字:下拉框选中文本
    /// 是「本地转发 (-L)」、侦听端口标签是「本机侦听端口」、目标标签是
    /// 「目标(从远端看)」、暴露勾选文案是「允许同网段其他主机连这个端口」——
    /// 这几处**都**把分节标题的字样当子串包含在内。实测过:用 `contains`
    /// 时,把任意一个 `form::section(...)` 调用整行删掉,断言照样绿,因为
    /// 需要的两个字总能在旁边某条字段文案里凑出来。分节标题是 `ui.label`
    /// 单独画的一整块文字,跟这些字段文案永远不是同一个 galley,所以用
    /// 精确相等才能只命中标题本身。
    fn has_exact(texts: &[String], needle: &str) -> bool {
        texts.iter().any(|s| s == needle)
    }

    /// 表单必须有视觉锚点。全部字段一路平铺下来时,眼睛找不到「这几行是一组」
    /// (走查 P2-17 的原话),这正是本次重排要解决的问题。
    ///
    /// 自证会变红:把任意一个 `form::section(...)` 调用删掉,这条当场红。
    #[test]
    fn the_form_is_grouped_into_named_sections() {
        let texts = rendered_texts(filled(TunnelKindUi::Local), &[]);
        for s in ["转发", "侦听", "目标", "其他"] {
            assert!(has_exact(&texts, s), "分节「{s}」没画出来: {texts:?}");
        }
    }

    /// 动态转发没有目标,「目标」那一节整块不画——画一个写不进数据的框
    /// 只会让人以为 SOCKS5 也要填目标。
    #[test]
    fn dynamic_forwarding_has_no_target_section() {
        let texts = rendered_texts(filled(TunnelKindUi::Dynamic), &[]);
        assert!(has(&texts, "侦听"), "侦听节该在: {texts:?}");
        assert!(!has(&texts, "目标"), "动态转发不该画目标节: {texts:?}");
    }

    /// 端口填错了要**当场**在字段下方说,而不是等用户点了保存再从顶部
    /// 弹一条通知——那时用户的视线早已离开出错的那个框。
    #[test]
    fn an_invalid_port_is_reported_inline_under_the_field() {
        let mut buf = filled(TunnelKindUi::Local);
        buf.listen_port = "0".into();
        let texts = rendered_texts(buf, &[]);
        assert!(
            has(&texts, "侦听端口要填 1~65535"),
            "端口非法时没有内联红字: {texts:?}"
        );
    }

    /// `rendered_texts` 探不到这条:egui 的 `ComboBox` 候选只在**点击展开**
    /// 后才画,测试没有模拟点击,`show_ui` 的闭包根本不会跑
    /// (探针实测:塞一个未选中的候选,断言它出现在渲染文字里 ——
    /// 断言失败,候选压根没画出来)。所以候选是否只列 SSH 只能靠测
    /// `show` 实际调用的这个纯函数本身。
    ///
    /// 自证会变红:把 `ssh_candidates` 里的 `.filter(...)` 去掉。
    #[test]
    fn the_session_dropdown_never_offers_an_sftp_node() {
        let all = vec![
            sess_rec(1, "生产主控", mullion_store::Protocol::Ssh),
            sess_rec(2, "文件中转", mullion_store::Protocol::Sftp),
        ];
        let candidates = ssh_candidates(&all);
        assert!(
            candidates.iter().all(|s| s.id != SessionId(2)),
            "SFTP 节点出现在了隧道的会话候选里: {candidates:?}"
        );
        assert_eq!(candidates.len(), 1, "只有一条 SSH 会话,候选也该只有一条");
    }
}
