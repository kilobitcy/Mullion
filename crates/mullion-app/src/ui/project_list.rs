//! F257:三处项目列表(启动页 / 项目管理器左栏 / 切换项目弹窗)共用的
//! **纯函数**:列哪些行、按什么排、空了说什么。零 egui、零 IO,可纯单测。
//!
//! 为什么必须共用:项目里原来为「顺序」写了三条注释反复强调复用
//! `by_recent_access`(理由:同一个搜索词在两个界面给出不同结果,用户几分钟内
//! 就会都看到一遍;那个函数已被这个模块取代,理由原样搬了过来)。F257 又往里
//! 加了「归档要不要列」和「空了说什么」两件同样会漂的事 —— 三处各写一份
//! `if`,加第四处列表时必漏。

// F344:筛选/排序/空态判定已搬到 `mullion_store::project_view`,与 Android
// 端共用;这里转出,调用点不变。空态**措辞**(`Surface`/`empty_text`)是各端
// 自己的 UI 文案,留在这里。
pub use mullion_store::project_view::{empty_reason, rows, EmptyReason, Tab};

/// 哪个界面在问。只用来挑空态文案的措辞 —— 项目管理器里 tab 就在眼前,
/// 不用指路;另外两处得告诉用户去哪儿撤。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Manager,
    Launcher,
    Pick,
}

/// 空态那句话。
///
/// 「归档里还有 N 个」要**报数**:不报的话用户不知道那边是不是也空的,还得
/// 切过去看一眼。
///
/// 三处 `NoProjects` 文案**原样沿用**各界面现有的那句(不是新写),接线时
/// (Task 6)两句要能对上,这里就是权威来源。
///
/// 匹配**不用 `_` 兜底**,四档 `EmptyReason` 各自把剩下的 `Surface` 用
/// or-pattern 显式列全:`AllArchived`/`NoMatch`/`NoArchived` 那几句文案里
/// 带着「到「会话 → 项目管理器 → 归档」」这种**指路**,对不对得看具体是
/// 哪个界面在问。用 `_` 的话,将来加第 4 个 `Surface` 时这三档会静默吃进
/// 通配分支,拿到一句写着别处路径的话,没人会去审;换成 or-pattern,加一个
/// `Surface` 变体时这四档全部编译报错,逼着把每一句文案重新过一遍。
pub fn empty_text(reason: EmptyReason, surface: Surface) -> String {
    match (reason, surface) {
        (EmptyReason::NoProjects, Surface::Launcher) => {
            "还没有项目。一个项目 = 一台机器上的一个目录 + 一个专属 tmux 会话;\
             从菜单「会话 → 项目管理器」建一个,以后开机点一下就回到现场。"
                .to_string()
        }
        (EmptyReason::NoProjects, Surface::Manager) => {
            "还没有项目。项目 = 一台机器上的一个开发目录 + 一个专属 tmux 会话,打开它就回到那个活。"
                .to_string()
        }
        (EmptyReason::NoProjects, Surface::Pick) => {
            "还没有项目。从「会话 → 项目管理器」建一个。".to_string()
        }
        (EmptyReason::AllArchived(n), Surface::Manager) => {
            format!("没有在用的项目。归档里还有 {n} 个。")
        }
        (EmptyReason::AllArchived(n), Surface::Launcher | Surface::Pick) => {
            format!("没有在用的项目。归档里还有 {n} 个 —— 到「会话 → 项目管理器 → 归档」取消归档。")
        }
        (EmptyReason::NoMatch, Surface::Manager | Surface::Launcher | Surface::Pick) => {
            "没有匹配的项目".to_string()
        }
        (EmptyReason::NoArchived, Surface::Manager | Surface::Launcher | Surface::Pick) => {
            "还没有归档任何项目。归档 = 收起不再做的活,随时能取消。".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_store::{ProjectId, ProjectRecord, SessionId};
    use std::collections::BTreeMap;

    fn proj(id: u64, name: &str, accessed: Option<&str>, archived: Option<&str>) -> ProjectRecord {
        ProjectRecord {
            id: ProjectId(id),
            name: name.into(),
            note: String::new(),
            nodes: vec![SessionId(7)],
            preferred: None,
            dir: format!("/data/{name}"),
            tmux_name: None,
            created_at: "2026-09-01T00:00:00Z".into(),
            last_accessed_at: accessed.map(str::to_string),
            archived_at: archived.map(str::to_string),
            memos: Vec::new(),
            icon: None,
        }
    }

    /// 有项目、但全归档了 —— 三处的空态**不能**再喊「还没有项目,去建一个」,
    /// 用户会真的去建一个重复的。
    #[test]
    fn an_all_archived_library_does_not_claim_there_are_no_projects() {
        let ps = vec![proj(1, "老活", None, Some("2026-09-01T00:00:00Z"))];
        assert_eq!(
            empty_reason(&ps, Tab::Active, "", &[], &BTreeMap::new()),
            Some(EmptyReason::AllArchived(1))
        );
        let text = empty_text(EmptyReason::AllArchived(1), Surface::Launcher);
        assert!(text.contains("没有在用的项目"), "{text}");
        assert!(text.contains('1'), "要报出归档里还有几个:{text}");
        assert!(!text.contains("还没有项目"), "不许说没有项目:{text}");
    }
}
