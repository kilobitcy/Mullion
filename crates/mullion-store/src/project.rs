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
    /// F257:归档时刻(RFC3339)。`None` = 在用。
    ///
    /// 与 `last_accessed_at` **完全同姿态**:app 注入时钟(store 不持有时钟)、
    /// 旧文件缺键即"未归档"、未归档不写出这个键 —— 所以**不需要升 schema**。
    ///
    /// 为什么不是 `bool`:归档 tab 要按「什么时候归的」倒序排(刚归错的在最上面,
    /// 马上能撤)。`bool` 只能回落 `last_accessed_at`,那样"上周归档的"和"半年前
    /// 归档的"混在一起,顺序取决于它们当年被打开的时间 —— 解释不通。
    ///
    /// **只由项目管理器右栏那两个按钮写**(设计 D6)。打开一个归档项目**不**自动
    /// 撤销归档:系统看到的只是"你打开了它",而打开的理由可能只是去捞一个文件;
    /// 让系统推翻用户的判断,错的时候是静默的。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<String>,
    /// F238:项目自设的图标。`None` = 回落首选节点的图标(解析在 app 侧的
    /// `project::icon_for`,store 不认识 `SessionRecord` 的外观)。
    ///
    /// 复用会话侧同一个 `IconSpec` 而不是新开一个类型:导入归一化
    /// (`ui::ico`)、渲染(`badge::paint_icon`)两条路径都只认它,新开类型
    /// 等于把那两条路径各复制一遍。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<crate::model::IconSpec>,
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
    /// 最终 tmux 名 sanitize 之后是空的。放过去 = 打开项目一个字节都不发
    /// (`build_plan` 对空名返回空计划),且全程零报错。
    TmuxNameEmpty,
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
    if mine.is_empty() {
        return Err(ProjectIssue::TmuxNameEmpty);
    }
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

// ---- F223 打开项目 = 一次性覆盖 ---------------------------------------

/// F223:把项目的上下文一次性盖在这次连接的自动化配置上。零 IO 纯函数。
///
/// **只盖三样**,其余(登录后命令 / env / 各档延时)全留会话自己的:
///
/// | 字段 | 覆盖成 | 为什么非盖不可 |
/// |---|---|---|
/// | `enabled` | `true` | 会话把总开关关了的话,`build_plan` 直接返回空计划 —— 「打开项目」会静默退化成一次普通换节点,tmux 永远不 attach |
/// | `tmux` | 项目的 `Attach` | 项目一律走 tmux(设计 P5);会话配的 `Off` 或别的名字都得让位,项目 tmux 是**另一个** session |
/// | `work_dir` | `p.dir` | 项目是更具体的上下文,盖掉更泛的会话默认值 |
///
/// **一次性,绝不写回会话记录**(同 F122 标签覆盖不落盘的姿态):同一台机器
/// 用户明天可能不带项目直接连,那时候该拿回他自己配的那份。
///
/// 会话名不参与:`session_name` 填的是 [`project_tmux_name`],它自己已经在
/// 「项目没配 tmux 名」时回落到**项目名**而不是会话名(理由见那边)。
pub fn overlay_project(
    p: &ProjectRecord,
    base: &crate::automation::ResolvedAutomation,
) -> crate::automation::ResolvedAutomation {
    crate::automation::ResolvedAutomation {
        enabled: true,
        tmux: Some(crate::automation::TmuxChoice::Attach {
            session_name: Some(project_tmux_name(p)),
        }),
        work_dir: Some(p.dir.clone()),
        ..base.clone()
    }
}

// ---- F222 同机指纹核对 -------------------------------------------------

/// 候选节点与项目已有节点是否同机。**三态**,`Pending` 不是失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SameMachine {
    /// 双方都有指纹且相同。
    Same,
    /// 双方都有指纹且不同。`with` 是**第一个**对不上的已有节点的键,
    /// 拿去告诉用户「跟哪一条不一致」——多节点项目里没有它无从查起。
    Different { with: String },
    /// 至少一边没连过、拿不到指纹。**放行**并标待核,由 app 在该节点
    /// 真握手拿到指纹那一刻再跑一次本函数。
    Pending,
}

/// 判定候选端点能否加入一个项目。零 IO 纯函数。
///
/// `existing`/`candidate` 是 **`known_hosts` 的键**(由 app 侧的
/// `mullion_ssh::known_hosts::host_key_id` 拼,非默认端口写成 `[host]:port`)。
/// 键在这里只用来查表,**判据是查出来的指纹** —— 按键(即 host 串)判会把
/// 「同一台机器换个端口/换个域名」直接判死,而那恰恰是项目要多节点的理由。
///
/// 拼键的函数在 `mullion-ssh` 里,store 不能依赖它(架构不变量),
/// 所以收已拼好的键。**调用方必须用同一个拼法** —— 拼法漂移的后果与
/// `KnownHostsFile::get` 的注释同款:同一台主机在表里占两条,判定形同虚设。
///
/// 只要与**任意一个**已有节点冲突就拒绝:项目的不变量是「所有节点同机」,
/// 不是「跟某一个同机」。
pub fn can_join(
    existing: &[String],
    candidate: &str,
    table: &crate::known_hosts::KnownHostsFile,
) -> SameMachine {
    let Some(mine) = table.get(candidate) else {
        return SameMachine::Pending;
    };
    let mut matched = false;
    for key in existing {
        let Some(other) = table.get(key) else {
            continue;
        };
        if other.fingerprint == mine.fingerprint {
            matched = true;
        } else {
            return SameMachine::Different { with: key.clone() };
        }
    }
    if matched {
        SameMachine::Same
    } else {
        SameMachine::Pending
    }
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
    use crate::project::{can_join, project_tmux_name, SameMachine};

    // ---- F222 同机指纹核对 ---------------------------------------------

    /// 表用纯内存的 `KnownHostsFile::default()`(`path=None` → `save` 是
    /// no-op),测试里不落盘。
    fn table(rows: &[(&str, &str)]) -> crate::known_hosts::KnownHostsFile {
        let mut t = crate::known_hosts::KnownHostsFile::default();
        for (key, fp) in rows {
            t.record(
                key,
                crate::known_hosts::HostKeyEntry {
                    algo: "ssh-ed25519".into(),
                    fingerprint: (*fp).into(),
                },
            );
        }
        t
    }

    #[test]
    fn two_endpoints_with_the_same_fingerprint_are_the_same_machine() {
        let t = table(&[("h", "SHA256:AAAA"), ("j", "SHA256:AAAA")]);
        assert_eq!(can_join(&["h".into()], "j", &t), SameMachine::Same);
    }

    /// **P2 的自证**:同一台机器换个端口,`host_key_id` 拼出来的键完全不同
    /// (`h` vs `[h]:2222`),按 host 串判会直接判成两台;按指纹判才是同一台。
    ///
    /// 这恰是「为什么项目要允许多节点」的那个场景 —— 判死了功能就没了。
    ///
    /// 自证会变红:把判据换成「两个键相等」→ 当场变红。
    #[test]
    fn the_same_machine_reached_on_two_ports_is_still_the_same_machine() {
        let t = table(&[("h", "SHA256:AAAA"), ("[h]:2222", "SHA256:AAAA")]);
        assert_eq!(can_join(&["h".into()], "[h]:2222", &t), SameMachine::Same);
    }

    #[test]
    fn two_endpoints_with_different_fingerprints_are_rejected() {
        let t = table(&[("h", "SHA256:AAAA"), ("j", "SHA256:BBBB")]);
        assert_eq!(
            can_join(&["h".into()], "j", &t),
            SameMachine::Different { with: "h".into() },
            "要指出跟哪个已有节点不一致,否则用户在多节点项目里无从查起"
        );
    }

    /// 没连过的节点**放行并标待核**,不是拒绝 —— 否则「离线整理配置」
    /// 会被一次强制连接卡住。
    #[test]
    fn an_endpoint_we_have_never_connected_to_is_pending_not_rejected() {
        let t = table(&[("h", "SHA256:AAAA")]);
        assert_eq!(
            can_join(&["h".into()], "brand-new", &t),
            SameMachine::Pending
        );
    }

    /// 指纹表被清空(或 corrupt 当空表)后,全部退化成**待核**,
    /// 不是「异机」—— 判成异机的话用户的项目会一夜之间全部报错。
    #[test]
    fn an_empty_table_degrades_everything_to_pending_not_different() {
        let t = table(&[]);
        assert_eq!(can_join(&["h".into()], "j", &t), SameMachine::Pending);
    }

    /// 多节点时**只要与任意一个已有节点冲突就拒绝**,不能因为跟另一个
    /// 对得上就放行 —— 项目的不变量是「所有节点同机」,不是「跟某一个同机」。
    ///
    /// 自证会变红:把遍历改成 `any(相同)` 就放行 → 变红。
    #[test]
    fn conflicting_with_any_existing_node_is_enough_to_reject() {
        let t = table(&[
            ("h", "SHA256:AAAA"),
            ("j", "SHA256:BBBB"),
            ("k", "SHA256:AAAA"),
        ]);
        assert_eq!(
            can_join(&["h".into(), "j".into()], "k", &t),
            SameMachine::Different { with: "j".into() }
        );
    }

    /// 候选**有**指纹、但已有节点一个都没连过 —— 仍是**待核**,不是同机。
    ///
    /// 空表那条测不到这里(候选也查不到,在函数头就早退了)。少了这条,
    /// 「一个都没比对过」会被报成「已核实同机」——判定给出的是伪阳性的
    /// 安全结论,而 UI 上待核标记不会出现,用户以为核过了。
    ///
    /// 自证会变红:把函数尾巴改成无条件 `SameMachine::Same`。
    #[test]
    fn a_candidate_with_no_verified_peer_to_compare_against_is_still_pending() {
        let t = table(&[("j", "SHA256:AAAA")]);
        assert_eq!(
            can_join(&["never-dialed".into()], "j", &t),
            SameMachine::Pending
        );
    }

    /// 已有节点里有没连过的,不影响与**连过的**那个的判定 ——
    /// 待核只在「拿不到任何可比对的指纹」时才是结论。
    #[test]
    fn an_unverified_existing_node_does_not_mask_a_real_conflict() {
        let t = table(&[("h", "SHA256:AAAA"), ("k", "SHA256:BBBB")]);
        assert_eq!(
            can_join(&["never-dialed".into(), "h".into()], "k", &t),
            SameMachine::Different { with: "h".into() }
        );
    }

    /// 候选就是已有节点自己(重新校验同一条),不该跟自己打架。
    #[test]
    fn revalidating_a_node_against_itself_is_the_same_machine() {
        let t = table(&[("h", "SHA256:AAAA")]);
        assert_eq!(can_join(&["h".into()], "h", &t), SameMachine::Same);
    }

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

    /// schema 必须升到 11:旧客户端读到 v11 会把 `[[project]].icon` 当未知
    /// 字段丢掉再写回 —— **用户设的图标静默消失**。拒绝比装作能用好。
    #[test]
    fn the_schema_version_is_bumped_so_old_clients_refuse_instead_of_dropping_projects() {
        assert_eq!(crate::model::CURRENT_SCHEMA, 11);
    }

    /// F238:项目自设的图标要能原样往返。**跟着 `[[project]]` 存在一起**,
    /// 不另开一张表 —— 图标是项目的一个属性,拆开存会让「删项目」多一处
    /// 要记得清的地方,而漏清的症状是下一个拿到同一个 id 的项目莫名带上
    /// 前任的图标。
    ///
    /// 自证会变红:把 `ProjectRecord.icon` 字段删掉(编译不过)或加上
    /// `#[serde(skip)]`(读回来是 `None`)。
    #[test]
    fn a_project_icon_round_trips_through_toml() {
        let mut p = super::tests::helpers::with_id(1, "web", None);
        p.icon = Some(crate::model::IconSpec {
            kind: crate::model::IconKind::Ico,
            value: "AAAA".into(),
            bg: None,
        });
        let s = toml::to_string_pretty(&p).expect("项目应能序列化");
        let back: crate::project::ProjectRecord = toml::from_str(&s).expect("项目应能读回来");
        assert_eq!(back.icon, p.icon, "图标没往返回来:{s}");
    }

    /// 没设图标的项目不该往 TOML 里写空键(同 `tmux_name`/`last_accessed_at`)。
    #[test]
    fn a_project_without_an_icon_writes_no_icon_key() {
        let p = super::tests::helpers::with_id(1, "web", None);
        let s = toml::to_string_pretty(&p).expect("项目应能序列化");
        assert!(!s.contains("icon"), "没设图标不该写出 icon 键:{s}");
    }

    /// 没归档的项目不该往 TOML 里写空键(同 `tmux_name`/`last_accessed_at`),
    /// 归档了要能原样读回来。
    ///
    /// 自证会变红:把 `archived_at` 字段删掉(编译不过)或加上
    /// `#[serde(skip)]`(读回来变成 `None`,跟写入的 `Some` 对不上)。
    #[test]
    fn archived_at_round_trips_and_stays_out_of_the_file_when_unset() {
        let mut p = super::tests::helpers::with_id(3, "Mullion", None);
        assert!(p.archived_at.is_none(), "新记录默认不是归档态");
        let s = toml::to_string_pretty(&p).unwrap();
        assert!(!s.contains("archived_at"), "未归档不应写出这个键: {s}");
        p.archived_at = Some("2026-09-11T08:00:00Z".into());
        let s = toml::to_string_pretty(&p).unwrap();
        let back: crate::project::ProjectRecord = toml::from_str(&s).unwrap();
        assert_eq!(back, p);
    }

    /// 旧文件里没有这个键,读回来必须是「未归档」而不是解析失败 ——
    /// 失败的话用户升级一次客户端,整个项目表就读不出来了。
    #[test]
    fn a_file_written_before_f257_still_loads_as_not_archived() {
        let text = r#"
id = 3
name = "Mullion"
dir = "/data/Mullion"
created_at = "2026-09-01T00:00:00Z"
"#;
        let back: crate::project::ProjectRecord = toml::from_str(text).unwrap();
        assert_eq!(back.archived_at, None);
    }

    // ---- F223 打开项目 = 一次性覆盖 -------------------------------------

    /// F223:tmux 名 sanitize 之后是空的,必须在**保存那一刻**拦下来。
    ///
    /// 放过去的后果全程静默:`tmux_session_name` 对空名返回 `None`
    /// → `build_plan` 返回空计划(「宁可什么都不做,也不发 `attach -t ''`」)
    /// → 打开项目连字节都不发,pane 停在裸 shell。日志、界面、测试全都正常,
    /// 用户只会觉得「这个项目点了没反应」。
    ///
    /// 撞不上 `TmuxNameClash`:那条要有**第二个**同名对象才触发,而第一个
    /// 空名项目谁也不撞。
    #[test]
    fn a_project_whose_tmux_name_sanitizes_to_nothing_is_rejected_at_save_time() {
        let mut p = helpers::project("我的项目", Some("   "));
        assert_eq!(project_tmux_name(&p), "", "前提:这个名字确实 sanitize 成空");
        assert_eq!(
            crate::validate_project(&p, &[], &[]),
            Err(crate::ProjectIssue::TmuxNameEmpty)
        );
        // 名字本身为空、又没配 tmux 名,同样落到这条上。
        p = helpers::project("", None);
        assert_eq!(
            crate::validate_project(&p, &[], &[]),
            Err(crate::ProjectIssue::TmuxNameEmpty)
        );
    }

    /// 项目一律走 tmux(设计 P5)。会话把自动化总开关关了、或显式配了
    /// `Off`,**都不能**让项目的 tmux 落空 —— `build_plan` 在 `enabled=false`
    /// 时直接返回空计划,那时候「打开项目」就退化成一次普通换节点:cd 也不做、
    /// tmux 也不 attach,而客户端零报错。
    #[test]
    fn opening_a_project_forces_tmux_even_when_the_session_switched_automation_off() {
        for base in [
            with(false, Some(crate::TmuxChoice::Off)),
            with(true, Some(crate::TmuxChoice::Off)),
            with(true, None),
        ] {
            let a = crate::overlay_project(&helpers::project("我的项目", None), &base);
            let plan = crate::build_plan(&a, "web01");
            assert_eq!(plan.len(), 1, "项目打开必须恰好一步 tmux 计划");
            let line = String::from_utf8(plan[0].bytes.clone()).unwrap();
            assert!(line.contains("exec tmux attach"), "{line}");
        }
    }

    /// 会话自己配了 tmux 名也得让位:项目的 tmux 是**另一个** session。
    #[test]
    fn the_project_tmux_name_wins_over_whatever_the_session_configured() {
        let base = with(
            true,
            Some(crate::TmuxChoice::Attach {
                session_name: Some("会话自己的".into()),
            }),
        );
        let a = crate::overlay_project(&helpers::project("我的项目", Some("proj-x")), &base);
        assert_eq!(
            crate::tmux_session_name(&a, "web01").as_deref(),
            Some("proj-x")
        );
    }

    /// 目录同理:项目是更具体的上下文,盖掉更泛的会话默认值。
    #[test]
    fn the_project_directory_replaces_the_session_work_dir() {
        let mut base = with(true, None);
        base.work_dir = Some("/home/me".into());
        let mut p = helpers::project("我的项目", None);
        p.dir = "/srv/app".into();
        assert_eq!(
            crate::overlay_project(&p, &base).work_dir.as_deref(),
            Some("/srv/app")
        );
    }

    /// **只盖这三样。** 用户配在会话上的登录后命令 / env / 各档延时是他自己
    /// 的东西,项目不该顺手吃掉 —— 那会让「打开项目」和「直接连这台机」跑出
    /// 两套不同的环境,而差异无处可查。
    #[test]
    fn the_overlay_leaves_the_sessions_own_commands_env_and_delays_alone() {
        let mut base = with(true, None);
        base.commands = vec![crate::AutomationCommand {
            text: "claude".into(),
            delay_ms: None,
        }];
        base.env = vec![crate::EnvVar {
            key: "RUST_LOG".into(),
            value: "debug".into(),
        }];
        base.initial_delay_ms = 777;
        let a = crate::overlay_project(&helpers::project("我的项目", None), &base);
        assert_eq!(a.commands, base.commands);
        assert_eq!(a.env, base.env);
        assert_eq!(a.initial_delay_ms, 777);
        assert_eq!(a.inter_delay_ms, base.inter_delay_ms);
        assert_eq!(a.ready_timeout_ms, base.ready_timeout_ms);
    }

    fn with(enabled: bool, tmux: Option<crate::TmuxChoice>) -> crate::ResolvedAutomation {
        crate::ResolvedAutomation {
            enabled,
            tmux,
            commands: Vec::new(),
            work_dir: None,
            env: Vec::new(),
            initial_delay_ms: 300,
            inter_delay_ms: 200,
            ready_timeout_ms: 15_000,
        }
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
                archived_at: None,
                icon: None,
            }
        }
    }
}
