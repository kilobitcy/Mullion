//! F225②:项目管理器弹窗(新建 / 编辑 / 删除项目)。
//!
//! 与 `group_manager` 同构:UI 只写「意图」到 `UiState`,由 `app.rs` 在借用
//! 释放后施加并落盘。校验一律委托 `mullion_store::validate_project` ——
//! 这里不重写一份判据,重写就必然与落盘那一侧漂移。
//!
//! 设计见 `docs/superpowers/specs/2026-09-08-f221-f225-project-unit-design.md`。

use mullion_store::{ProjectId, ProjectRecord, Protocol, SessionId, SessionRecord};

/// 一次项目操作的意图。与 `GroupIntent` 同姿态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectIntent {
    /// 新建:只给名字,其余字段进弹窗再填。
    Add(String),
    /// 整份保存。`ProjectRecord.id` 由 `app` 侧以第一个参数为准。
    Save(ProjectId, Box<ProjectRecord>),
    Delete(ProjectId),
}

/// 列表顺序:**按最后访问时间倒序**,从没打开过的排在最后。
///
/// 用户开机后想干的第一件事是回到昨天那个活,不是从一堆项目里找 ——
/// 所以「最近在干的」必须在最上面。F225① 的 launcher 列表用同一个函数,
/// 两处顺序不许各写一份(写两份就会漂,而「顺序不一样」用户第一眼就看得出来)。
///
/// 时间是 RFC3339 字符串,同一时区下**字典序即时间序**;跨时区写入的记录
/// (配置目录被搬到另一台机器,F48)可能排错位置 —— 后果有上限(列表顺序
/// 不对,不影响任何数据),不值得为它引一个日期解析库。
///
/// 同一时间(或都没访问过)时按 `id` 升序兜底 —— **不能靠 `sort_by_key` 的
/// 稳定性**:那样顺序就取决于入参顺序,而入参顺序来自磁盘上 `[[project]]`
/// 的书写次序,用户手改一次配置文件列表就重排了。
pub fn by_recent_access(projects: &[ProjectRecord]) -> Vec<&ProjectRecord> {
    let mut out: Vec<&ProjectRecord> = projects.iter().collect();
    out.sort_by(|a, b| {
        match (&a.last_accessed_at, &b.last_accessed_at) {
            (Some(x), Some(y)) => y.cmp(x),
            // 访问过的一律排在没访问过的前面,与两者具体是什么值无关。
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
        .then(a.id.0.cmp(&b.id.0))
    });
    out
}

/// 能当项目节点的会话:**只有 SSH**。
///
/// SFTP 节点没有 PTY,attach 过去是一块永远不出字的黑屏。`validate_project`
/// 那一道是落盘前的闸;这一道是**根本不让它出现在勾选列表里** ——
/// 让用户勾一个注定被拒的东西,是把校验当交互用。
pub fn selectable_nodes(sessions: &[SessionRecord]) -> Vec<&SessionRecord> {
    sessions
        .iter()
        .filter(|s| s.connection.protocol == Protocol::Ssh)
        .collect()
}

/// 一条会话在 `known_hosts` 里的键。与 SSH 侧**同一个拼法** ——
/// 拼法漂移的后果见 `KnownHostsFile::get` 的文档:同一台主机占两条记录。
pub fn host_key_of(s: &SessionRecord) -> String {
    mullion_ssh::known_hosts::host_key_id(&s.connection.host, s.connection.port)
}

/// 候选节点与项目里**已选中的其余节点**是否同机(F222)。
///
/// `table` 为 `None`(指纹表拿不到)时一律返回待核 —— 不是「同机」:
/// 拿不到证据时报「已核实」是伪阳性的安全结论。
pub fn node_verdict(
    chosen: &[SessionId],
    candidate: &SessionRecord,
    sessions: &[SessionRecord],
    table: Option<&mullion_store::known_hosts::KnownHostsFile>,
) -> mullion_store::SameMachine {
    let Some(table) = table else {
        return mullion_store::SameMachine::Pending;
    };
    let existing: Vec<String> = chosen
        .iter()
        .filter(|id| **id != candidate.id)
        .filter_map(|id| sessions.iter().find(|s| s.id == *id))
        .map(host_key_of)
        .collect();
    mullion_store::can_join(&existing, &host_key_of(candidate), table)
}

/// 项目管理弹窗。只写意图,不碰 store。
///
/// `table` 是 `known_hosts` 指纹表(F222)。`None` = 拿不到 —— 此时所有节点
/// 一律标「待核」,**不会**因为拿不到证据就放行成「已核实同机」。
pub fn show(
    ctx: &egui::Context,
    t: &crate::theme::Theme,
    ui_state: &mut crate::ui::UiState,
    projects: &[ProjectRecord],
    sessions: &[SessionRecord],
    table: Option<&mullion_store::known_hosts::KnownHostsFile>,
) {
    let mut open = ui_state.project_manager_open;
    // 选中项被别的实例删掉(F189 重读)时草稿要跟着退回,否则右栏在编辑一个
    // 已经不存在的项目,保存时静默落空。
    if let Some(id) = ui_state.project_selected {
        if !projects.iter().any(|p| p.id == id) {
            ui_state.project_selected = None;
            ui_state.project_draft = None;
        }
    }
    egui::Window::new("项目管理")
        .open(&mut open)
        .default_width(720.0)
        .show(ctx, |ui| {
            ui.horizontal_top(|ui| {
                list_column(ui, t, ui_state, projects);
                ui.separator();
                ui.vertical(|ui| {
                    form_column(ui, t, ui_state, projects, sessions, table);
                });
            });
        });
    ui_state.project_manager_open = open;
    if !open {
        // 关窗即丢草稿:留着的话下次打开会拿一份可能已经过期的内容盖上去。
        ui_state.project_draft = None;
        ui_state.project_selected = None;
    }
}

/// 左栏:新建 + 项目列表。
fn list_column(
    ui: &mut egui::Ui,
    t: &crate::theme::Theme,
    ui_state: &mut crate::ui::UiState,
    projects: &[ProjectRecord],
) {
    use crate::ui::metrics::{FIELD_W_S, SP_S};
    ui.vertical(|ui| {
        // 列表列取「两个 S 档」宽:项目名比会话名短,不需要 M 档。
        ui.set_width(FIELD_W_S * 2.0);
        ui.horizontal(|ui| {
            let w = crate::ui::metrics::field_w(ui.available_width(), FIELD_W_S, 56.0);
            ui.add(
                egui::TextEdit::singleline(&mut ui_state.project_name_buf)
                    .hint_text("新建项目")
                    .desired_width(w),
            );
            let name = ui_state.project_name_buf.trim().to_string();
            let dup = projects.iter().any(|p| p.name == name);
            if ui
                .add_enabled(!name.is_empty() && !dup, egui::Button::new("添加"))
                .clicked()
            {
                ui_state.project_intent = Some(ProjectIntent::Add(name));
                ui_state.project_name_buf.clear();
            }
        });
        ui.add_space(SP_S);
        if projects.is_empty() {
            ui.label(
                egui::RichText::new("还没有项目。项目 = 一台机器上的一个开发目录 + 一个专属 tmux 会话,打开它就回到那个活。")
                    .color(crate::theme::c32(t.fg_muted)),
            );
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt("project_list")
            .max_height(360.0)
            .show(ui, |ui| {
                for p in by_recent_access(projects) {
                    let selected = ui_state.project_selected == Some(p.id);
                    if ui.selectable_label(selected, &p.name).clicked() {
                        ui_state.project_selected = Some(p.id);
                        ui_state.project_draft = Some(p.clone());
                    }
                }
            });
    });
}

/// 右栏:选中项目的表单。
fn form_column(
    ui: &mut egui::Ui,
    t: &crate::theme::Theme,
    ui_state: &mut crate::ui::UiState,
    projects: &[ProjectRecord],
    sessions: &[SessionRecord],
    table: Option<&mullion_store::known_hosts::KnownHostsFile>,
) {
    use crate::ui::metrics::{field_w, FIELD_W_L, FIELD_W_M, SP_M, SP_S, SP_XS};
    let Some(draft) = ui_state.project_draft.as_mut() else {
        ui.label(
            egui::RichText::new("从左边选一个项目来编辑。").color(crate::theme::c32(t.fg_muted)),
        );
        return;
    };
    let id = draft.id;
    let mut first = true;
    crate::ui::session_manager::form::section(ui, t, "项目管理器", "基本", &mut first);
    crate::ui::session_manager::form::grid(ui, "project_basic", |ui| {
        ui.label("名称");
        let w = field_w(ui.available_width(), FIELD_W_M, 0.0);
        ui.add(egui::TextEdit::singleline(&mut draft.name).desired_width(w));
        ui.end_row();
        ui.label("说明");
        let w = field_w(ui.available_width(), FIELD_W_L, 0.0);
        ui.add(egui::TextEdit::singleline(&mut draft.note).desired_width(w));
        ui.end_row();
        ui.label("目录");
        let w = field_w(ui.available_width(), FIELD_W_L, 0.0);
        ui.add(
            egui::TextEdit::singleline(&mut draft.dir)
                .hint_text("/srv/app")
                .desired_width(w),
        );
        ui.end_row();
        ui.label("tmux 名");
        let mut spelled = draft.tmux_name.clone().unwrap_or_default();
        let w = field_w(ui.available_width(), FIELD_W_M, 0.0);
        if ui
            .add(
                egui::TextEdit::singleline(&mut spelled)
                    .hint_text("留空 = 用项目名")
                    .desired_width(w),
            )
            .changed()
        {
            draft.tmux_name = (!spelled.trim().is_empty()).then(|| spelled.trim().to_string());
        }
        ui.end_row();
    });
    ui.add_space(SP_XS);
    ui.label(
        egui::RichText::new(format!(
            "会 attach 的 tmux 会话:{}",
            mullion_store::project_tmux_name(draft)
        ))
        .color(crate::theme::c32(t.fg_muted)),
    );
    // 设计里明写要让用户看见的那条:`-c` 只对新建会话生效。
    ui.label(
        egui::RichText::new(
            "改目录只影响新建的 tmux 会话;已经存在的那个不会移动,需先在远端结束它。",
        )
        .color(crate::theme::c32(t.fg_muted)),
    );

    crate::ui::session_manager::form::section(ui, t, "项目管理器", "节点", &mut first);
    ui.label(
        egui::RichText::new("同一台机器的等价路线(不同凭据 / 端口 / 跳板)。只列 SSH 会话。")
            .color(crate::theme::c32(t.fg_muted)),
    );
    ui.add_space(SP_XS);
    let candidates = selectable_nodes(sessions);
    if candidates.is_empty() {
        ui.label(
            egui::RichText::new("还没有 SSH 会话可选。先在会话管理器里建一条。")
                .color(crate::theme::c32(t.fg_muted)),
        );
    }
    egui::ScrollArea::vertical()
        .id_salt("project_nodes")
        .max_height(180.0)
        .show(ui, |ui| {
            for s in &candidates {
                ui.horizontal(|ui| {
                    let mut on = draft.nodes.contains(&s.id);
                    if ui.checkbox(&mut on, &s.identity.name).changed() {
                        if on {
                            draft.nodes.push(s.id);
                        } else {
                            draft.nodes.retain(|n| *n != s.id);
                            if draft.preferred == Some(s.id) {
                                draft.preferred = None;
                            }
                        }
                    }
                    if on {
                        let verdict = node_verdict(&draft.nodes, s, sessions, table);
                        let (text, color) = match verdict {
                            mullion_store::SameMachine::Same => {
                                ("已核实同机", crate::theme::c32(t.fg_muted))
                            }
                            mullion_store::SameMachine::Pending => {
                                ("待核", crate::theme::c32(t.fg_muted))
                            }
                            mullion_store::SameMachine::Different { .. } => {
                                ("指纹与其他节点不一致", crate::theme::c32(t.danger_text))
                            }
                        };
                        ui.label(egui::RichText::new(text).size(11.0).color(color));
                        let mut pref = draft.preferred == Some(s.id);
                        if ui.radio(pref, "首选").clicked() {
                            pref = true;
                        }
                        if pref {
                            draft.preferred = Some(s.id);
                        }
                    }
                });
            }
        });

    crate::ui::session_manager::form::section(ui, t, "项目管理器", "记录", &mut first);
    ui.label(
        egui::RichText::new(format!(
            "创建于 {} · 最后打开 {}",
            draft.created_at,
            draft.last_accessed_at.as_deref().unwrap_or("从未")
        ))
        .color(crate::theme::c32(t.fg_muted)),
    );

    ui.add_space(SP_M);
    // 校验一律走 store 的那份判据 —— 这里重写一遍就必然与落盘那侧漂移。
    let issue = mullion_store::validate_project(draft, projects, sessions).err();
    if let Some(ref e) = issue {
        ui.label(
            egui::RichText::new(issue_text(e, projects, sessions))
                .color(crate::theme::c32(t.danger_text)),
        );
        ui.add_space(SP_XS);
    }
    let blank = draft.name.trim().is_empty() || draft.dir.trim().is_empty();
    ui.horizontal(|ui| {
        if ui
            .add_enabled(issue.is_none() && !blank, egui::Button::new("保存"))
            .clicked()
        {
            ui_state.project_intent = Some(ProjectIntent::Save(id, Box::new(draft.clone())));
        }
        ui.add_space(SP_S);
        if ui.button("删除项目").clicked() {
            ui_state.project_intent = Some(ProjectIntent::Delete(id));
        }
    });
}

/// 把校验失败翻成人话。**逐条指出撞在谁身上** —— 只说「名字重复」的话
/// 用户得自己翻一遍项目表去找。
fn issue_text(
    e: &mullion_store::ProjectIssue,
    projects: &[ProjectRecord],
    sessions: &[SessionRecord],
) -> String {
    use mullion_store::{ProjectIssue as I, TmuxNameOwner as O};
    let pname = |id| {
        projects
            .iter()
            .find(|p| p.id == id)
            .map_or_else(|| "(已删除)".to_string(), |p| p.name.clone())
    };
    let sname = |id| {
        sessions
            .iter()
            .find(|s| s.id == id)
            .map_or_else(|| "(已删除)".to_string(), |s| s.identity.name.clone())
    };
    match e {
        I::DuplicateName { with } => format!("项目名与「{}」重复。", pname(*with)),
        I::TmuxNameClash {
            name,
            with: O::Project(p),
        } => format!("tmux 名「{name}」与项目「{}」撞车。", pname(*p)),
        I::TmuxNameClash {
            name,
            with: O::Session(s),
        } => format!("tmux 名「{name}」与会话「{}」写死的名字撞车。", sname(*s)),
        I::PreferredNotInNodes => "首选节点不在已勾选的节点里。".to_string(),
        I::NonSshNode { node } => format!("「{}」不是 SSH 会话,不能当项目节点。", sname(*node)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proj(id: u64, name: &str, accessed: Option<&str>) -> ProjectRecord {
        ProjectRecord {
            id: ProjectId(id),
            name: name.into(),
            note: String::new(),
            nodes: Vec::new(),
            preferred: None,
            dir: "/srv/app".into(),
            tmux_name: None,
            created_at: "2026-09-01T00:00:00Z".into(),
            last_accessed_at: accessed.map(str::to_string),
        }
    }

    fn sess(id: u64, name: &str, proto: Protocol) -> SessionRecord {
        SessionRecord {
            id: SessionId(id),
            modified_at: "t".into(),
            identity: mullion_store::Identity {
                name: name.into(),
                note: String::new(),
                group_id: None,
                tags: Vec::new(),
            },
            connection: mullion_store::Connection {
                host: "h".into(),
                port: 22,
                protocol: proto,
            },
            auth: mullion_store::Auth::inline("u", mullion_store::AuthKind::Password),
            terminal: Default::default(),
            appearance: Default::default(),
            network: Default::default(),
            automation: Default::default(),
            sftp: Default::default(),
        }
    }

    #[test]
    fn the_most_recently_opened_project_comes_first() {
        let ps = vec![
            proj(1, "旧", Some("2026-09-01T00:00:00Z")),
            proj(2, "新", Some("2026-09-07T00:00:00Z")),
        ];
        let got = by_recent_access(&ps);
        assert_eq!(got[0].name, "新", "倒序:最近访问的排最上面");
        assert_eq!(got[1].name, "旧");
    }

    /// 从没打开过的排最后 —— 不管它的 id 多小、也不管它在磁盘上写在哪一行。
    ///
    /// 自证会变红:把 `(Some(_), None)` 那一臂改成 `Greater`。
    #[test]
    fn a_project_never_opened_sinks_below_every_opened_one() {
        let ps = vec![
            proj(1, "没打开过", None),
            proj(2, "打开过", Some("2026-09-01T00:00:00Z")),
        ];
        let got = by_recent_access(&ps);
        assert_eq!(got[0].name, "打开过");
        assert_eq!(got[1].name, "没打开过");
    }

    /// 平局按 id 兜底,**不靠排序的稳定性**。
    ///
    /// 靠稳定性的话顺序就等于 `[[project]]` 在磁盘上的书写次序 ——
    /// 用户手改一次配置文件、或哪天换成 `sort_unstable_by`,列表就重排了,
    /// 而这两件事都不会有任何报错。
    ///
    /// 自证会变红:把 `.then(a.id.0.cmp(&b.id.0))` 去掉 —— 入参是逆序的,
    /// 稳定排序会原样保留 `[b, a]`。
    #[test]
    fn projects_that_tie_are_ordered_by_id_not_by_input_order() {
        let ps = vec![proj(9, "后写的", None), proj(2, "先写的", None)];
        let got = by_recent_access(&ps);
        assert_eq!(
            got.iter().map(|p| p.id.0).collect::<Vec<_>>(),
            vec![2, 9],
            "平局要按 id,不能沿用入参顺序"
        );
    }

    /// SFTP 会话根本不该出现在节点勾选列表里 —— 让用户勾一个注定被
    /// `validate_project` 拒掉的东西,是把校验当交互用。
    ///
    /// 自证会变红:把 `filter` 去掉。
    #[test]
    fn an_sftp_session_never_shows_up_as_a_candidate_node() {
        let ss = vec![
            sess(1, "ssh 的", Protocol::Ssh),
            sess(2, "sftp 的", Protocol::Sftp),
        ];
        let got = selectable_nodes(&ss);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].identity.name, "ssh 的");
    }

    /// 拿不到指纹表时是**待核**,不是同机 —— 报「已核实同机」是伪阳性的
    /// 安全结论,而界面上待核标记也不会出现,用户以为核过了。
    ///
    /// 自证会变红:把早退改成 `SameMachine::Same`。
    #[test]
    fn without_a_fingerprint_table_every_node_is_pending_not_same() {
        let ss = vec![sess(1, "a", Protocol::Ssh), sess(2, "b", Protocol::Ssh)];
        assert_eq!(
            node_verdict(&[SessionId(1)], &ss[1], &ss, None),
            mullion_store::SameMachine::Pending
        );
    }

    /// 候选**自己**已经在已选列表里(重新校验一条已勾的)时,不能拿它跟
    /// 自己比 —— 那永远是 `Same`,会把「这一条其实还没连过」的待核态
    /// 掩盖成「已核实」。
    ///
    /// 自证会变红:把 `filter(|id| **id != candidate.id)` 去掉。
    #[test]
    fn a_node_is_never_compared_against_itself() {
        let ss = vec![sess(1, "a", Protocol::Ssh)];
        let mut table = mullion_store::known_hosts::KnownHostsFile::default();
        table.record(
            &host_key_of(&ss[0]),
            mullion_store::known_hosts::HostKeyEntry {
                algo: "ssh-ed25519".into(),
                fingerprint: "SHA256:AAAA".into(),
            },
        );
        assert_eq!(
            node_verdict(&[SessionId(1)], &ss[0], &ss, Some(&table)),
            mullion_store::SameMachine::Pending,
            "只有它自己一条时没有可比对的对象,应是待核"
        );
    }
}
