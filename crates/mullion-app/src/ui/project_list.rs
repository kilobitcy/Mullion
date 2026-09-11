//! F257:三处项目列表(启动页 / 项目管理器左栏 / 切换项目弹窗)共用的
//! **纯函数**:列哪些行、按什么排、空了说什么。零 egui、零 IO,可纯单测。
//!
//! 为什么必须共用:项目里原来为「顺序」写了三条注释反复强调复用
//! `by_recent_access`(理由:同一个搜索词在两个界面给出不同结果,用户几分钟内
//! 就会都看到一遍;那个函数已被这个模块取代,理由原样搬了过来)。F257 又往里
//! 加了「归档要不要列」和「空了说什么」两件同样会漂的事 —— 三处各写一份
//! `if`,加第四处列表时必漏。

use mullion_store::{ProjectRecord, SessionRecord};

/// 项目管理器上那两个 tab。**只有项目管理器有 tab**(设计 D3):启动页与
/// 切换项目弹窗恒传 [`Tab::Active`] —— 那两处是「干活入口」,归档项目默认
/// 不该在那儿碍事,但**搜得到**(见 [`rows`] 的搜索态)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Active,
    Archived,
}

impl Default for Tab {
    /// 默认「在用」—— 打开项目管理器时该看见还在做的活。
    fn default() -> Self {
        Tab::Active
    }
}

impl Tab {
    /// tab 上的字。**「在用」不叫「活跃」**:F258 把「最后活跃」定成了排序
    /// 判据,同一个词在同一个界面指两件事。也不叫「全部」(它不含归档的)、
    /// 不叫「运行中」(那是 F224 的灯,一个项目可以在用但现在没开)。
    pub fn label(self) -> &'static str {
        match self {
            Tab::Active => "在用",
            Tab::Archived => "归档",
        }
    }
}

/// 哪个界面在问。只用来挑空态文案的措辞 —— 项目管理器里 tab 就在眼前,
/// 不用指路;另外两处得告诉用户去哪儿撤。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Manager,
    Launcher,
    Pick,
}

/// 列表为什么是空的。**四档穷尽**,不是一个 `bool`。
///
/// 为什么非要枚举:原来三处各写一句 `if projects.is_empty()`,而归档一上来
/// `projects.is_empty()` 仍是 `false`、列表却是空的 —— 三处会照旧喊
/// 「还没有项目,去建一个」,用户会真的去建一个重复的。加档时漏一处的症状是
/// 一句**说错的话**,编译器不会管。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmptyReason {
    /// 库里一个项目都没有。
    NoProjects,
    /// 有项目,但全在归档里。带上归档里有几个。
    AllArchived(usize),
    /// 搜索词没有匹配。
    NoMatch,
    /// 归档 tab 是空的(还没归档过任何项目)。
    NoArchived,
}

/// 这一帧该列哪些项目,已排好序。
///
/// **搜索态跨两态**:`query` 非空时忽略 `tab`,在用的整体排在归档的前面,
/// 段内各按自己的判据。清空搜索才回到 `tab` 说了算。
///
/// 段内判据:
/// - 在用:`last_accessed_at` 倒序 → id 升序兜底(F258 会在这里插入 Lit 置顶)
/// - 归档:`archived_at` 倒序 → id 升序兜底
///
/// 时间是 RFC3339 字符串,同一时区下**字典序即时间序**(与已被这个函数取代的
/// `by_recent_access` 同样的取舍:跨时区搬配置目录会排错位置,后果有上限,
/// 不值得引日期解析库)。
///
/// id 兜底**不能靠 `sort_by` 的稳定性**:那样顺序就取决于入参顺序,而入参顺序
/// 来自磁盘上 `[[project]]` 的书写次序,用户手改一次配置文件列表就重排了。
pub fn rows<'a>(
    projects: &'a [ProjectRecord],
    tab: Tab,
    query: &str,
    sessions: &[SessionRecord],
    frozen_lamps: &std::collections::BTreeMap<mullion_store::ProjectId, crate::project::Lamp>,
) -> Vec<&'a ProjectRecord> {
    let searching = !query.trim().is_empty();
    // F258:Lit 置顶**只在浏览态的「在用」tab**(设计 D13/D17)。
    //
    // 搜索态不置顶:用户已经明确知道要找谁,置顶只会打乱 —— 而且不置顶让
    // 搜索态成为纯函数,不需要下面那份冻结灯,需要显式失效的状态只剩一处。
    //
    // 归档 tab 不置顶:归档项目正跑着是个异常情况,不该因此排到最上面
    // (灯照常亮,那才是"它还没关干净"的提示)。
    let float_lit = !searching && tab == Tab::Active;
    let mut out: Vec<&ProjectRecord> = projects
        .iter()
        .filter(|p| searching || in_tab(p, tab))
        .filter(|p| crate::project::matches(p, query, sessions))
        .collect();
    out.sort_by(|a, b| {
        lit_rank(a, float_lit, frozen_lamps)
            .cmp(&lit_rank(b, float_lit, frozen_lamps))
            // 搜索态:在用的整段在前。非搜索态两边同档,这一比恒 Equal。
            .then_with(|| archived(a).cmp(&archived(b)))
            .then_with(|| segment_order(a, b))
    });
    out
}

/// 置顶用的排名:`0` = 亮着要置顶,`1` = 其余。
///
/// 读的是**冻结的灯**(设计 D13):灯是异步变的(别的实例开/关项目、心跳超时),
/// 每帧实时排的话,某一行会在你正要点它的那一瞬间跳到列表最上面、把目标挤下去
/// —— 点错项目 = 连到另一台机器、attach 另一个 tmux,是本项目最不想要的那类
/// 「看不出错的误操作」。行上画的灯仍然是**实时**的:灯变色,位置不动。
///
/// 冻结表里查不到 → 按 `Unknown` 处置 → 不置顶,按时间落位。列表开着的时候
/// 新建的项目走这条,**不会消失**。
///
/// `Unknown` 不算亮:它的语义是"还有 pane 没上报过",拿它当亮的话启动那几帧
/// 全表都被判成亮,置顶等于没置顶。
fn lit_rank(
    p: &ProjectRecord,
    float_lit: bool,
    frozen: &std::collections::BTreeMap<mullion_store::ProjectId, crate::project::Lamp>,
) -> u8 {
    if !float_lit {
        // 返回 `0` 而不是 `1`:两者效果等价——不置顶时全表同档,`cmp` 恒
        // `Equal`,排序退化到下一档判据。选 `0` 只是语义上更好读:「都在
        // 置顶档里、谁也不比谁靠前」,比「全都不置顶」这种双重否定顺口。
        return 0;
    }
    match frozen.get(&p.id) {
        Some(crate::project::Lamp::Lit) => 0,
        _ => 1,
    }
}

/// 这个项目属不属于这个 tab。
fn in_tab(p: &ProjectRecord, tab: Tab) -> bool {
    match tab {
        Tab::Active => p.archived_at.is_none(),
        Tab::Archived => p.archived_at.is_some(),
    }
}

/// 排序用的"归不归档"。`false` < `true`,所以在用的自然排前面。
fn archived(p: &ProjectRecord) -> bool {
    p.archived_at.is_some()
}

/// 段内顺序。归档的按归档时间倒序,在用的按最后访问倒序;都以 id 升序兜底。
///
/// 注意:`key` 闭包是按**各自**的 `archived(p)` 取字段的(归档的取
/// `archived_at`、在用的取 `last_accessed_at`),不是统一按某一套算 ——
/// 一个不归档、一个归档时,两边其实在比"归档时间"和"最后访问"这两个不同
/// 语义的字段,是苹果比橘子。之所以没事:当前唯一调用点 `rows` 里
/// `archived(a).cmp(&archived(b))` 已经先把两档分了开,混档的比较**到不了**
/// 这里。这么写清楚是为了以后有人复用 `segment_order` 时不被"结果无害"
/// 这种说法误导,以为它对混档输入也有意义。
fn segment_order(a: &ProjectRecord, b: &ProjectRecord) -> std::cmp::Ordering {
    let key = |p: &ProjectRecord| -> Option<String> {
        if archived(p) {
            p.archived_at.clone()
        } else {
            p.last_accessed_at.clone()
        }
    };
    newest_first(&key(a), &key(b)).then(a.id.0.cmp(&b.id.0))
}

/// 有时间戳的排在没有的前面;都有就倒序;都没有算平手(交给 id 兜底)。
///
/// 抽出来是因为 F258 的 Lit 置顶要在它外面再套一层,而"没时间戳的沉底"这条
/// 规则两处都要,写两份必漂。
fn newest_first(a: &Option<String>, b: &Option<String>) -> std::cmp::Ordering {
    match (a, b) {
        (Some(x), Some(y)) => y.cmp(x),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

/// 列表空了是为什么。`None` = 没空,别画提示。
///
/// **判据顺序有意义**:搜索没匹配要排在"全归档"前面 —— 库里全归档、又搜了个
/// 搜不到的词时,该说「没有匹配的项目」而不是「归档里还有 N 个」(后者答非所问)。
pub fn empty_reason(
    projects: &[ProjectRecord],
    tab: Tab,
    query: &str,
    sessions: &[SessionRecord],
    frozen_lamps: &std::collections::BTreeMap<mullion_store::ProjectId, crate::project::Lamp>,
) -> Option<EmptyReason> {
    if !rows(projects, tab, query, sessions, frozen_lamps).is_empty() {
        return None;
    }
    if projects.is_empty() {
        return Some(EmptyReason::NoProjects);
    }
    if !query.trim().is_empty() {
        return Some(EmptyReason::NoMatch);
    }
    match tab {
        Tab::Archived => Some(EmptyReason::NoArchived),
        Tab::Active => Some(EmptyReason::AllArchived(
            projects.iter().filter(|p| archived(p)).count(),
        )),
    }
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
    use crate::project::Lamp;
    use mullion_store::{ProjectId, SessionId};
    use std::collections::BTreeMap;

    fn lamps(pairs: &[(u64, Lamp)]) -> BTreeMap<ProjectId, Lamp> {
        pairs.iter().map(|(id, l)| (ProjectId(*id), *l)).collect()
    }

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
            icon: None,
        }
    }

    /// 浏览态的「在用」tab 只列没归档的。
    #[test]
    fn the_active_tab_lists_only_projects_that_are_not_archived() {
        let ps = vec![
            proj(1, "在用的", Some("2026-09-10T00:00:00Z"), None),
            proj(
                2,
                "归档的",
                Some("2026-09-11T00:00:00Z"),
                Some("2026-09-11T01:00:00Z"),
            ),
        ];
        let got: Vec<u64> = rows(&ps, Tab::Active, "", &[], &BTreeMap::new())
            .iter()
            .map(|p| p.id.0)
            .collect();
        assert_eq!(got, vec![1], "归档的项目不该出现在「在用」里");
    }

    /// 「归档」tab 按**归档时间**倒序,不是按最后访问 —— 刚归错的要在最上面,
    /// 马上能撤。
    ///
    /// 判据故意让两种排法给出**相反**的顺序:`old` 访问得更晚、归档得更早。
    /// 不这么造的话,把 `archived_at` 换成 `last_accessed_at` 也是绿的。
    #[test]
    fn the_archived_tab_sorts_by_when_it_was_archived_not_when_it_was_last_opened() {
        let ps = vec![
            proj(
                1,
                "先归的",
                Some("2026-09-10T00:00:00Z"),
                Some("2026-09-01T00:00:00Z"),
            ),
            proj(
                2,
                "后归的",
                Some("2026-09-02T00:00:00Z"),
                Some("2026-09-09T00:00:00Z"),
            ),
        ];
        let got: Vec<u64> = rows(&ps, Tab::Archived, "", &[], &BTreeMap::new())
            .iter()
            .map(|p| p.id.0)
            .collect();
        assert_eq!(got, vec![2, 1], "归档 tab 要按归档时间倒序");
    }

    /// 搜索穿透两态,且**在用的整体排在归档的前面**。
    ///
    /// 判据造成:归档那条的访问时间**更新**。混排(不分段)会把它排到前面。
    #[test]
    fn searching_crosses_both_states_and_puts_the_active_ones_first() {
        let ps = vec![
            proj(1, "活 alpha", Some("2026-09-01T00:00:00Z"), None),
            proj(
                2,
                "档 alpha",
                Some("2026-09-30T00:00:00Z"),
                Some("2026-09-02T00:00:00Z"),
            ),
        ];
        for tab in [Tab::Active, Tab::Archived] {
            let got: Vec<u64> = rows(&ps, tab, "alpha", &[], &BTreeMap::new())
                .iter()
                .map(|p| p.id.0)
                .collect();
            assert_eq!(
                got,
                vec![1, 2],
                "搜索要穿透归档,且在用的整体在前(tab={tab:?} 不该影响搜索结果)"
            );
        }
    }

    /// 从没打开过的项目沉底,但**不消失** —— 新建一个项目之后它就在这一档,
    /// 掉了的话用户会以为没建成。
    #[test]
    fn a_project_never_opened_sinks_to_the_bottom_but_does_not_disappear() {
        let ps = vec![
            proj(1, "没开过的", None, None),
            proj(2, "开过的", Some("2026-09-01T00:00:00Z"), None),
        ];
        let got: Vec<u64> = rows(&ps, Tab::Active, "", &[], &BTreeMap::new())
            .iter()
            .map(|p| p.id.0)
            .collect();
        assert_eq!(got, vec![2, 1]);
    }

    /// 时间完全一样时按 id 升序 —— **不能靠 `sort_by` 的稳定性**,
    /// 那样顺序就取决于磁盘上 `[[project]]` 的书写次序,用户手改一次配置
    /// 文件列表就重排了。
    ///
    /// 判据故意把入参顺序造成与期望**相反**,靠稳定性的实现会红。
    #[test]
    fn projects_with_the_same_timestamp_fall_back_to_id_not_to_file_order() {
        let ps = vec![
            proj(9, "后写的", Some("2026-09-01T00:00:00Z"), None),
            proj(2, "先写的", Some("2026-09-01T00:00:00Z"), None),
        ];
        let got: Vec<u64> = rows(&ps, Tab::Active, "", &[], &BTreeMap::new())
            .iter()
            .map(|p| p.id.0)
            .collect();
        assert_eq!(got, vec![2, 9]);
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

    /// 一个项目都没有时,原来那句话原样保留。
    #[test]
    fn a_truly_empty_library_keeps_the_original_wording() {
        assert_eq!(
            empty_reason(&[], Tab::Active, "", &[], &BTreeMap::new()),
            Some(EmptyReason::NoProjects)
        );
    }

    /// 搜索没匹配是**第三档**,不能被上面两档吃掉 —— 库里全归档、又搜了个
    /// 搜不到的词时,该说「没有匹配的项目」,不是「归档里还有 N 个」。
    #[test]
    fn a_search_with_no_hits_is_its_own_case_even_when_everything_is_archived() {
        let ps = vec![proj(1, "老活", None, Some("2026-09-01T00:00:00Z"))];
        assert_eq!(
            empty_reason(&ps, Tab::Active, "找不到的词", &[], &BTreeMap::new()),
            Some(EmptyReason::NoMatch)
        );
    }

    /// 归档 tab 空了是第四档,不能复用「还没有项目」。
    #[test]
    fn an_empty_archive_tab_says_so_instead_of_claiming_there_are_no_projects() {
        let ps = vec![proj(1, "在用的", None, None)];
        assert_eq!(
            empty_reason(&ps, Tab::Archived, "", &[], &BTreeMap::new()),
            Some(EmptyReason::NoArchived)
        );
    }

    /// 有行可列时不能返回空态 —— 反了的话列表和提示会同时出现。
    #[test]
    fn a_non_empty_list_has_no_empty_reason() {
        let ps = vec![proj(1, "在用的", None, None)];
        assert_eq!(
            empty_reason(&ps, Tab::Active, "", &[], &BTreeMap::new()),
            None
        );
    }

    /// 库里一个项目都没有时,**即使搜索框里有字**也该说「还没有项目」——
    /// 库是空的,说「没有匹配」是答非所问,而且把用户往"换个词再搜"引,
    /// 那条路上什么都没有。
    ///
    /// 自证会变红:把 `empty_reason` 里 `projects.is_empty()` 和
    /// `!query.trim().is_empty()` 两道判据互换顺序。
    #[test]
    fn an_empty_library_says_so_even_while_you_are_searching() {
        assert_eq!(
            empty_reason(&[], Tab::Active, "随便什么词", &[], &BTreeMap::new()),
            Some(EmptyReason::NoProjects)
        );
    }

    /// 纯空格不算搜索:tab 该照常起作用,归档的不该被搜出来。
    ///
    /// 这条钉的是一份**跨模块契约**:本模块用 `trim().is_empty()` 判"在不在
    /// 搜索态",而命中判定在 `project::matches` → `search::tokens` 里用
    /// `split_whitespace`。两边对"纯空格"的理解一旦分家,症状是「光标停在
    /// 搜索框里没打字,归档项目却冒出来了」,没有任何报错。
    #[test]
    fn a_query_of_only_spaces_is_not_a_search() {
        let ps = vec![
            proj(1, "在用的", None, None),
            proj(2, "归档的", None, Some("2026-09-01T00:00:00Z")),
        ];
        let got: Vec<u64> = rows(&ps, Tab::Active, "   ", &[], &BTreeMap::new())
            .iter()
            .map(|p| p.id.0)
            .collect();
        assert_eq!(got, vec![1], "纯空格不该被当成搜索、把归档的放进来");
    }

    /// 两个都访问过的项目按最后访问时间**倒序**排 —— 从
    /// `project_manager::by_recent_access` 删掉的
    /// `the_most_recently_opened_project_comes_first` 判据搬到这里
    /// (那个函数已被 `project_list::rows` 取代)。
    #[test]
    fn the_most_recently_accessed_project_comes_first() {
        let ps = vec![
            proj(1, "旧", Some("2026-09-01T00:00:00Z"), None),
            proj(2, "新", Some("2026-09-07T00:00:00Z"), None),
        ];
        let got: Vec<u64> = rows(&ps, Tab::Active, "", &[], &BTreeMap::new())
            .iter()
            .map(|p| p.id.0)
            .collect();
        assert_eq!(got, vec![2, 1], "倒序:最近访问的排最上面");
    }

    /// D13:浏览态的「在用」tab 里,正亮着灯的项目**置顶**。
    ///
    /// 判据造成:亮灯那条的访问时间**更旧**。不置顶的话它排在后面。
    #[test]
    fn a_lit_project_floats_to_the_top_of_the_active_tab() {
        let ps = vec![
            proj(1, "亮着的老活", Some("2026-09-01T00:00:00Z"), None),
            proj(2, "刚开过的", Some("2026-09-10T00:00:00Z"), None),
        ];
        let f = lamps(&[(1, Lamp::Lit), (2, Lamp::Dark)]);
        let got: Vec<u64> = rows(&ps, Tab::Active, "", &[], &f)
            .iter()
            .map(|p| p.id.0)
            .collect();
        assert_eq!(got, vec![1, 2], "亮着的要置顶");
    }

    /// `Unknown` **不算亮** —— 它的语义是"还有 pane 没上报过,可能在跑",
    /// 拿它当亮的话启动那几帧全表都会被判成亮,置顶等于没置顶。
    #[test]
    fn an_unknown_lamp_does_not_count_as_lit() {
        let ps = vec![
            proj(1, "灯未知的老活", Some("2026-09-01T00:00:00Z"), None),
            proj(2, "刚开过的", Some("2026-09-10T00:00:00Z"), None),
        ];
        // 只登记 1 号(`Unknown`)。2 号**不在**冻结表里,与 1 号一样落到
        // `lit_rank` 的 `_ => 1` 分支 —— 这样才能把「值是 `Unknown`」和
        // 「压根没查到」这两种都不算亮的路径**同时**钉住:如果 `lit_rank`
        // 被错改成「查到什么值都算亮」(`Some(_) => 0`),1 号会被误判成
        // 置顶而 2 号不会,顺序变成 `[1, 2]`,能与下面的期望值分得开。
        // 之前的写法给两边都塞了值(`Unknown`/`Dark`),那样错改后两边会一起
        // 被判成置顶、又被访问时间的顺序悄悄对上,变异杀不掉。
        let f = lamps(&[(1, Lamp::Unknown)]);
        let got: Vec<u64> = rows(&ps, Tab::Active, "", &[], &f)
            .iter()
            .map(|p| p.id.0)
            .collect();
        assert_eq!(got, vec![2, 1], "Unknown 不该置顶");
    }

    /// D17:**搜索态不做 Lit 置顶**。搜的时候用户已经明确知道要找谁,
    /// 置顶只会打乱;而且不置顶意味着搜索态是纯函数、不需要定格。
    #[test]
    fn searching_does_not_float_lit_projects() {
        let ps = vec![
            proj(1, "亮着的 alpha", Some("2026-09-01T00:00:00Z"), None),
            proj(2, "刚开过的 alpha", Some("2026-09-10T00:00:00Z"), None),
        ];
        let f = lamps(&[(1, Lamp::Lit), (2, Lamp::Dark)]);
        let got: Vec<u64> = rows(&ps, Tab::Active, "alpha", &[], &f)
            .iter()
            .map(|p| p.id.0)
            .collect();
        assert_eq!(got, vec![2, 1], "搜索态不该置顶");
    }

    /// 归档 tab 不做 Lit 置顶 —— 归档项目正跑着是个异常情况,不该因此排到
    /// 最上面(灯照常亮,那是提示"它还没关干净")。
    #[test]
    fn the_archived_tab_does_not_float_lit_projects() {
        let ps = vec![
            proj(
                1,
                "亮着的",
                Some("2026-09-30T00:00:00Z"),
                Some("2026-09-01T00:00:00Z"),
            ),
            proj(
                2,
                "后归的",
                Some("2026-09-01T00:00:00Z"),
                Some("2026-09-09T00:00:00Z"),
            ),
        ];
        let f = lamps(&[(1, Lamp::Lit), (2, Lamp::Dark)]);
        let got: Vec<u64> = rows(&ps, Tab::Archived, "", &[], &f)
            .iter()
            .map(|p| p.id.0)
            .collect();
        assert_eq!(got, vec![2, 1], "归档 tab 按归档时间排,不置顶");
    }

    /// 冻结表里查不到的项目(列表开着的时候新建的)按 `Unknown` 处置 ——
    /// **不能消失**,也不能被当成亮的。
    #[test]
    fn a_project_missing_from_the_frozen_lamps_still_shows_up() {
        let ps = vec![proj(1, "新建的", None, None)];
        let got: Vec<u64> = rows(&ps, Tab::Active, "", &[], &BTreeMap::new())
            .iter()
            .map(|p| p.id.0)
            .collect();
        assert_eq!(got, vec![1], "冻结表里没有它,也必须画出来");
    }
}
