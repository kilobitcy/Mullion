//! F221:「项目」管理单位的数据模型与纯函数。零 IO、零 async。
//!
//! 设计见 `docs/superpowers/specs/2026-09-08-f221-f225-project-unit-design.md`。

use serde::{Deserialize, Serialize};

/// 项目稳定主键。新建时取现有 max+1(见 vault),与 `SessionId`/`GroupId` 同一姿态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProjectId(pub u64);

/// 一个项目 = 一台机器上的一个开发目录 + 到达它的若干条等价路线。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRecord {
    pub id: ProjectId,
    /// 全局唯一(校验见 `validate`)。
    pub name: String,
    #[serde(default)]
    pub note: String,
    /// 同机的等价路线。只收 `Protocol::Ssh`。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<crate::model::SessionId>,
    /// 打开项目时用哪条路线。`None` = 无可用节点。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred: Option<crate::model::SessionId>,
    /// 终端 `work_dir` 与文件面板落脚点,**一份**(设计 P9)。
    pub dir: String,
    /// `None` = 由项目名推导(见 [`project_tmux_name`])。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_name: Option<String>,
    /// RFC3339;由调用方(app)注入,store 不持有时钟。
    pub created_at: String,
    /// `None` = 从未打开过。更新时机见 F224(**跃迁触发**,不是每批上报都写)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_accessed_at: Option<String>,
}

/// 这个项目最终会 attach 的 tmux 会话名。
///
/// 留空时回退到**项目名**,`sanitize_tmux_name` 之后返回。
///
/// **绝不回退到会话名**(那是 `automation::tmux_session_name` 的行为):同一台
/// 机器上的两个项目走同一条路线时,回退到会话名会让它们算出同一个名字、
/// attach 进同一个 tmux 会话 ——**两个项目共用一个 Claude Code**,是本设计里
/// 后果最严重且完全静默的错误。
pub fn project_tmux_name(p: &ProjectRecord) -> String {
    crate::automation::sanitize_tmux_name(p.tmux_name.as_deref().unwrap_or(&p.name))
}

/// 校验没通过的原因。**逐条带上撞车对象**:只说「名字重复」而不说跟谁重复,
/// 用户得自己翻一遍项目表去找。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectIssue {
    /// 项目名与另一个项目重复。
    DuplicateName { with: ProjectId },
    /// 最终 tmux 名与别处撞车。
    TmuxNameClash { name: String, with: TmuxNameOwner },
    /// 首选节点不在节点列表里。
    PreferredNotInNodes,
    /// 节点不是 SSH 会话(SFTP 没有 PTY,attach 过去是一块永远不出字的黑屏)。
    NonSshNode { node: crate::model::SessionId },
}

/// tmux 名撞在谁身上。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TmuxNameOwner {
    Project(ProjectId),
    Session(crate::model::SessionId),
}

/// 保存前的校验。
///
/// `all` 传**全表**(含 `candidate` 自己那行,按 id 排除)—— 让调用方先过滤
/// 的话,漏过滤的症状是「任何更新都报跟自己撞车、永远存不进去」。
///
/// 会话侧**只比对显式配置的** tmux 名(见 [`explicit_session_tmux_name`])。
pub fn validate(
    candidate: &ProjectRecord,
    all: &[ProjectRecord],
    sessions: &[crate::model::SessionRecord],
) -> Result<(), ProjectIssue> {
    if let Some(p) = candidate.preferred {
        if !candidate.nodes.contains(&p) {
            return Err(ProjectIssue::PreferredNotInNodes);
        }
    }
    for n in &candidate.nodes {
        if sessions
            .iter()
            .any(|s| s.id == *n && s.connection.protocol != crate::model::Protocol::Ssh)
        {
            return Err(ProjectIssue::NonSshNode { node: *n });
        }
    }
    let mine = project_tmux_name(candidate);
    for other in all.iter().filter(|o| o.id != candidate.id) {
        if other.name == candidate.name {
            return Err(ProjectIssue::DuplicateName { with: other.id });
        }
        if project_tmux_name(other) == mine {
            return Err(ProjectIssue::TmuxNameClash {
                name: mine,
                with: TmuxNameOwner::Project(other.id),
            });
        }
    }
    for s in sessions {
        if explicit_session_tmux_name(s).is_some_and(|n| n == mine) {
            return Err(ProjectIssue::TmuxNameClash {
                name: mine,
                with: TmuxNameOwner::Session(s.id),
            });
        }
    }
    Ok(())
}

/// 一条会话**显式写死**的 tmux 名(sanitize 后)。`None` = 没配 tmux、
/// 显式 `Off`、或**配了 attach 但名字留空**。
///
/// 最后那一种是**故意排除**的:留空时 `automation::tmux_session_name` 按
/// 会话名推导,而那个值会随会话改名而变 —— 追不过来,追了还会在改会话名时
/// 反过来把项目卡住。代价是会话名恰好等于项目名时可能撞车(已认下的缺口)。
fn explicit_session_tmux_name(s: &crate::model::SessionRecord) -> Option<String> {
    let Some(crate::automation::TmuxChoice::Attach { session_name }) = &s.automation.tmux else {
        return None;
    };
    let name = crate::automation::sanitize_tmux_name(session_name.as_deref()?);
    (!name.is_empty()).then_some(name)
}

#[cfg(test)]
mod tests {
    use crate::project::project_tmux_name;

    /// tmux 名留空时回退到**项目名**,绝不回退到会话名。
    ///
    /// 回退到会话名的话,同一台机器上的两个项目会 attach 进同一个 tmux 会话
    /// ——**两个项目共用一个 Claude Code**,是本设计里后果最严重、且完全
    /// 静默的错误。
    #[test]
    fn a_blank_tmux_name_falls_back_to_the_project_name() {
        let p = super::tests::helpers::project("我的项目", None);
        assert_eq!(project_tmux_name(&p), "我的项目");
    }

    /// 显式配了就用显式的那个(仍要过 sanitize)。
    #[test]
    fn an_explicit_tmux_name_wins_over_the_project_name() {
        let p = super::tests::helpers::project("proj", Some("web.01"));
        assert_eq!(project_tmux_name(&p), "web-01");
    }

    /// 两个项目名**不同**、却 sanitize 成同一个 tmux 名 —— 必须拒绝。
    ///
    /// 这是撞车路径里最隐蔽的一条:光比项目名相等拦不住它,而后果是两个项目
    /// attach 进同一个 tmux 会话。
    #[test]
    fn two_projects_whose_names_sanitize_to_the_same_tmux_name_clash() {
        let a = super::tests::helpers::with_id(1, "web.01", None);
        let b = super::tests::helpers::with_id(2, "web:01", None);
        let issue = crate::project::validate(&b, &[a], &[]).unwrap_err();
        assert!(
            matches!(issue, crate::project::ProjectIssue::TmuxNameClash { .. }),
            "应报 tmux 名撞车,实得 {issue:?}"
        );
    }

    /// 校验一个**已在表里**的项目(更新场景)不该跟自己撞。
    ///
    /// 调用方传全表最省心;函数内按 id 排除自己。忘了排除的话,任何更新都会
    /// 报「跟自己撞车」而**永远存不进去**。
    #[test]
    fn a_project_does_not_clash_with_its_own_row_when_revalidated() {
        let a = super::tests::helpers::with_id(1, "web", None);
        crate::project::validate(&a, std::slice::from_ref(&a), &[]).unwrap();
    }

    /// 项目的 tmux 名撞上**某条会话显式配置的** tmux 名 —— 拒绝。
    ///
    /// 不拦的话,用户从那条会话普通连上去,会直接落进项目的 tmux 会话里,
    /// 而项目那边的灯与访问时间会把这次也算成「项目在跑」。
    #[test]
    fn a_project_clashes_with_a_tmux_name_a_session_spells_out() {
        let p = super::tests::helpers::with_id(1, "web", None);
        let s = super::tests::helpers::session_with_tmux(7, "别的名字", Some("web"));
        let issue = crate::project::validate(&p, &[], &[s]).unwrap_err();
        assert!(
            matches!(
                issue,
                crate::project::ProjectIssue::TmuxNameClash {
                    with: crate::project::TmuxNameOwner::Session(_),
                    ..
                }
            ),
            "应报撞在会话上,实得 {issue:?}"
        );
    }

    /// **故意不完备的缺口,这条把它钉成有意的行为而不是遗漏。**
    ///
    /// 会话没显式配 tmux 名时,它 attach 的名字由**会话名**推导。那个值会随
    /// 会话改名而变,追不过来,追了还会在改会话名时反过来把项目卡住 ——
    /// 所以**不校验**。代价:会话名恰好等于项目名时可能撞车。
    ///
    /// 变异自证:把这条缺口"补上"(把会话名也纳入比对)→ 本条变红。
    #[test]
    fn a_session_name_that_merely_derives_the_same_tmux_name_is_not_checked() {
        let p = super::tests::helpers::with_id(1, "web", None);
        let s = super::tests::helpers::session_with_tmux(7, "web", None);
        crate::project::validate(&p, &[], &[s])
            .expect("留空推导出来的会话名不在校验范围内(故意的)");
    }

    /// 首选节点必须在节点列表里。
    ///
    /// 不拦的话,打开项目会连到一条**已经被移出这个项目**的路线上,而界面上
    /// 那条路线根本不显示 —— 用户看到的是「打开项目连到了莫名其妙的机器」。
    #[test]
    fn the_preferred_node_must_be_one_of_the_listed_nodes() {
        let mut p = super::tests::helpers::with_id(1, "web", None);
        p.nodes = vec![crate::model::SessionId(7)];
        p.preferred = Some(crate::model::SessionId(9));
        let issue = crate::project::validate(&p, &[], &[]).unwrap_err();
        assert_eq!(issue, crate::project::ProjectIssue::PreferredNotInNodes);
    }

    /// 节点全被删光之后 `preferred` 是 `None` —— 这是合法的「无可用节点」态,
    /// 不是校验失败。项目照样能存(目录/tmux 名是用户手打的,不能因一次删
    /// 会话就存不回去)。
    #[test]
    fn a_project_with_no_nodes_at_all_is_still_valid() {
        let p = super::tests::helpers::with_id(1, "web", None);
        crate::project::validate(&p, &[], &[]).expect("无节点是合法态");
    }

    /// SFTP 节点不能进项目 —— 它没有 PTY,attach 过去只会得到一块永远不出字
    /// 的黑屏。这是**第一道**闸;rehost 的 `wants_sftp` 检查是第二道。
    #[test]
    fn an_sftp_session_cannot_be_a_project_node() {
        let mut p = super::tests::helpers::with_id(1, "web", None);
        p.nodes = vec![crate::model::SessionId(7)];
        let mut s = super::tests::helpers::session_with_tmux(7, "文件", None);
        s.connection.protocol = crate::model::Protocol::Sftp;
        let issue = crate::project::validate(&p, &[], &[s]).unwrap_err();
        assert_eq!(
            issue,
            crate::project::ProjectIssue::NonSshNode {
                node: crate::model::SessionId(7)
            }
        );
    }

    #[test]
    fn project_toml_round_trips() {
        let mut p = super::tests::helpers::with_id(3, "Mullion", Some("mull"));
        p.note = "客户端本体".into();
        p.nodes = vec![crate::model::SessionId(7), crate::model::SessionId(8)];
        p.preferred = Some(crate::model::SessionId(7));
        p.last_accessed_at = Some("2026-09-08T12:00:00Z".into());
        let s = toml::to_string_pretty(&p).unwrap();
        let back: crate::project::ProjectRecord = toml::from_str(&s).unwrap();
        assert_eq!(back, p);
    }

    /// 没配的可选字段不该往 TOML 里写空行。
    #[test]
    fn unset_optional_fields_are_not_written_out() {
        let p = super::tests::helpers::with_id(1, "web", None);
        let s = toml::to_string_pretty(&p).unwrap();
        assert!(!s.contains("tmux_name"), "留空的 tmux_name 不应写出: {s}");
        assert!(!s.contains("last_accessed_at"), "从未访问过不应写出: {s}");
    }

    /// v9 的文件(没有 `[[project]]`)读进来是空表,**不报错**。
    #[test]
    fn a_v9_file_without_projects_reads_as_an_empty_table() {
        let f: crate::model::SessionsFile = toml::from_str("schema_version = 9").unwrap();
        assert!(f.project.is_empty());
    }

    /// schema 必须升到 10:旧客户端读到 v10 会把整个 `[[project]]` 表当未知
    /// 字段丢掉再写回 —— **用户的项目静默消失**。拒绝比装作能用好。
    #[test]
    fn the_schema_version_is_bumped_so_old_clients_refuse_instead_of_dropping_projects() {
        assert_eq!(crate::model::CURRENT_SCHEMA, 10);
    }

    pub(super) mod helpers {
        use crate::project::ProjectRecord;

        /// 一条会话,`tmux` = 它**显式配置**的 tmux 名(`None` = 没配,
        /// 走会话名推导)。
        pub fn session_with_tmux(
            id: u64,
            name: &str,
            tmux: Option<&str>,
        ) -> crate::model::SessionRecord {
            crate::model::SessionRecord {
                id: crate::model::SessionId(id),
                modified_at: "2026-09-08T00:00:00Z".into(),
                identity: crate::model::Identity {
                    name: name.into(),
                    note: String::new(),
                    group_id: None,
                    tags: Vec::new(),
                },
                connection: crate::model::Connection {
                    host: "h".into(),
                    port: 22,
                    protocol: crate::model::Protocol::Ssh,
                },
                auth: crate::model::Auth::inline("u", crate::model::AuthKind::Password),
                terminal: crate::model::TerminalPrefs::default(),
                appearance: crate::model::AppearancePrefs::default(),
                network: crate::network::NetworkPrefs::default(),
                automation: crate::automation::AutomationPrefs {
                    tmux: Some(crate::automation::TmuxChoice::Attach {
                        session_name: tmux.map(Into::into),
                    }),
                    ..Default::default()
                },
                sftp: crate::sftp::SftpPrefs::default(),
            }
        }

        pub fn with_id(id: u64, name: &str, tmux: Option<&str>) -> ProjectRecord {
            ProjectRecord {
                id: crate::project::ProjectId(id),
                ..project(name, tmux)
            }
        }

        pub fn project(name: &str, tmux: Option<&str>) -> ProjectRecord {
            ProjectRecord {
                id: crate::project::ProjectId(1),
                name: name.into(),
                note: String::new(),
                nodes: Vec::new(),
                preferred: None,
                dir: "/srv/app".into(),
                tmux_name: tmux.map(Into::into),
                created_at: "2026-09-08T00:00:00Z".into(),
                last_accessed_at: None,
            }
        }
    }
}
