//! 项目列表与打开项目的**纯判据**,桌面与 Android 共用(F344 从 app 搬来)。
//! 零 UI、零 IO、零 async。各函数的设计理由见其 doc 注释(原样保留)。

use crate::{ProjectRecord, SessionRecord};

/// F304:打开这个项目时按什么顺序试节点。
///
/// 直连的在前、经跳板或代理的在后(`is_routed`,取自
/// `AppearanceCache::is_routed`);同一组内首选排第一,其余按 `nodes` 的顺序。
/// 典型场景:同一台机器配了「局域网直连」和「公网跳板」两条路线 —— 在公司
/// 直连能通就走直连,在家直连 3 秒不通再退到跳板。首选只决定**组内**先后,
/// 压不过「直连优先」:用户拍板的规则,跳板那条再首选也比直连慢。
///
/// `preferred` **必须真在 `nodes` 里**才算数 —— 用户把首选那条从列表里去掉、
/// `preferred` 却没跟着清的话(F189 下别的实例改了配置就会发生),拿它去拨号会
/// 连到一台已经不属于这个项目的机器上。`validate_project` 那道闸只管保存路径,
/// 读回来的旧数据不受它管。
pub fn candidates(
    p: &crate::ProjectRecord,
    is_routed: impl Fn(crate::SessionId) -> bool,
) -> Vec<crate::SessionId> {
    let preferred = p.preferred.filter(|id| p.nodes.contains(id));
    let mut out: Vec<_> = preferred
        .into_iter()
        .chain(p.nodes.iter().copied().filter(|id| Some(*id) != preferred))
        .collect();
    // 稳定排序:组内保持上面排好的「首选 → nodes 顺序」。
    out.sort_by_key(|id| is_routed(*id));
    out
}

/// 这个项目**头一个**试哪台机器 = [`candidates`] 的第一条。
///
/// 单独摘出来是因为**列表上写着的节点名必须和点下去先连的那台是同一条判据**
/// (F225① launcher 每行都写着节点名)。各写一份的话,「显示 A、连上 B」是这类
/// 界面里最难查的一种错。
pub fn node_for(
    p: &crate::ProjectRecord,
    is_routed: impl Fn(crate::SessionId) -> bool,
) -> Option<crate::SessionId> {
    candidates(p, is_routed).first().copied()
}

/// F224:项目的运行指示灯。**三态,「灭」不许拿来冒充「未知」**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lamp {
    /// 有 pane(本实例或别的实例)正 attach 在这个项目的 tmux 会话上。
    Lit,
    /// 本地所有实例的 pane **都已上报**且无一命中。
    Dark,
    /// 还有 pane 没上报过 —— 它可能正 attach 在这个项目里。
    Unknown,
}

/// 项目身上**行里看得见**的那些字段:项目名 / 目录 / 每一条节点会话的名字与
/// 主机(后两者进副标题)。
///
/// 收节点是因为用户记得住的常是机器名或 IP 尾数,不是当初给活起的名字 ——
/// 与 `session_manager::list::matches` 收 host/tags 是同一条理由。
///
/// 收**全部** `nodes` 而不只是首选:多节点正是「同一台机器的等价路线」,
/// 用户搜哪条路线的名字都该找到这个活。
///
/// 只看这个项目自己的节点(`s.id == *id`)。丢掉 id 比对的话,任意一条会话名
/// 都能把全部项目一起捞出来 —— 搜索仍然「有反应」,但等于失效。
fn visible_fields<'a>(
    p: &'a crate::ProjectRecord,
    sessions: &'a [crate::SessionRecord],
) -> Vec<&'a str> {
    let mut out = vec![p.name.as_str(), p.dir.as_str()];
    for id in &p.nodes {
        if let Some(s) = sessions.iter().find(|s| s.id == *id) {
            out.push(s.identity.name.as_str());
            out.push(s.connection.host.as_str());
        }
    }
    out
}

/// F245:项目身上**行里看不见**的那些字段:说明、以及最终会 attach 的 tmux 名。
///
/// 返回 `String` 而不是 `&str`:tmux 名要过 `project_tmux_name`(留空时回落
/// 项目名 + `sanitize_tmux_name`),那是算出来的,借不出去。
///
/// 取**最终名**而不是裸 `p.tmux_name`:用户在远端 `tmux ls` 里看到、心里记着的
/// 就是这个。裸字段的话,没显式设过 tmux 名的项目(默认就是 `None`)在这一维
/// 上完全搜不到,而用户不知道自己搜的名字是显式设的还是推导出来的 —— 表现为
/// 「有时搜得到有时搜不到」。
fn hidden_fields(p: &crate::ProjectRecord) -> Vec<String> {
    let mut out = vec![p.note.clone(), crate::project_tmux_name(p)];
    // F340:备忘同样是行里看不见的字段,并进来后 `hidden_hit_snippet` 自动覆盖。
    out.extend(p.memos.iter().map(|m| m.text.clone()));
    out
}

/// F233/F245:一个项目是否命中搜索词。空查询放行全部。
///
/// 分词与 AND/OR 的语义见 [`crate::search::matches_all`] —— 会话管理器共用
/// 同一份,两处语义分叉的话同一个词在两个界面给出不同结果。
///
/// **F245 收了 `note` 与 tmux 名**,推翻了 F233 当初「不收 note」的决定。当初
/// 的顾虑(多行长文本会命中一堆用户在列表上看不见的东西)是真的,所以这一片
/// 同时补上了「看得见」那一半:见 [`hidden_hit_snippet`]。
///
/// 三处列表(项目管理器左栏 / 启动页 / pane 切换弹窗)共用这一份 —— 各写一份
/// 的话,同一个搜索词在两个界面给出不同结果,而用户几分钟内就会都看到一遍。
pub fn matches(p: &crate::ProjectRecord, query: &str, sessions: &[crate::SessionRecord]) -> bool {
    let hidden = hidden_fields(p);
    let mut fields = visible_fields(p, sessions);
    fields.extend(hidden.iter().map(String::as_str));
    crate::search::matches_all(query, &fields)
}

/// 片段里命中词**之前**保留多少个字符。
const SNIPPET_BEFORE: usize = 8;

/// 片段里命中词**之后**保留多少个字符。比前文给得多:命中词更可能靠句子前部,
/// 后文比前文有信息量。
const SNIPPET_AFTER: usize = 24;

/// F245:这一行是不是**只**靠说明 / tmux 名才出现的;是的话,给出一段能解释
/// 「凭什么」的正文片段。`None` = 行上看得见的字段已经解释得了,副标题照旧。
///
/// 判据是「**只有**隐藏字段命中」而不是「隐藏字段命中了」:后者会让普通搜索
/// (打项目名)的行也平白变样。只有那些**凭空冒出来的行**才需要自己解释自己。
///
/// 逐词判、取第一个这样的词 —— 保证返回的片段里必定含有一处会被
/// `highlight::segments` 染色的命中。从说明第一行开头截的话,画出来的可能是
/// 一段不含高亮的文字:行变样了,却仍然没回答「为什么是这一行」。
pub fn hidden_hit_snippet(
    p: &crate::ProjectRecord,
    sessions: &[crate::SessionRecord],
    query: &str,
) -> Option<String> {
    let visible = visible_fields(p, sessions);
    let hidden = hidden_fields(p);
    crate::search::tokens(query).into_iter().find_map(|tok| {
        if crate::search::token_hits(tok, &visible) {
            return None;
        }
        hidden
            .iter()
            .find_map(|f| snippet_around(f, tok))
            .filter(|s| !s.is_empty())
    })
}

/// 从 `text` 里截出 `tok` 命中处前后的一段。`None` = 没命中。
///
/// 全程在 `char` 上做:说明是中文,按字节切当场 panic(同
/// `highlight::segments` 的理由)。
///
/// **控制字符换成空格**:说明是多行的(F237),片段跨行时直接画会在
/// `LayoutJob` 里换行,把两行的行高撑成三行 —— 而 `project_row` 的
/// `NAME_TOP`/`SUB_TOP` 是写死的常量、三处列表共用。
fn snippet_around(text: &str, tok: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let hay: Vec<char> = chars.iter().map(|c| fold(*c)).collect();
    let needle: Vec<char> = tok.chars().map(fold).collect();
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    let at = (0..=hay.len() - needle.len()).find(|i| hay[*i..i + needle.len()] == needle[..])?;
    let start = at.saturating_sub(SNIPPET_BEFORE);
    let end = (at + needle.len() + SNIPPET_AFTER).min(chars.len());
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.extend(
        chars[start..end]
            .iter()
            .map(|c| if c.is_control() { ' ' } else { *c }),
    );
    if end < chars.len() {
        out.push('…');
    }
    Some(out)
}

/// 折叠大小写。与 `highlight::segments` 同一条近似(只取 `to_lowercase()` 的
/// 首个 char),这样片段里被截出来的那一段,一定也是那边会染色的那一段。
fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

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
    frozen_lamps: &std::collections::BTreeMap<crate::ProjectId, Lamp>,
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
        .filter(|p| matches(p, query, sessions))
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
    frozen: &std::collections::BTreeMap<crate::ProjectId, Lamp>,
) -> u8 {
    if !float_lit {
        // 返回 `0` 而不是 `1`:两者效果等价——不置顶时全表同档,`cmp` 恒
        // `Equal`,排序退化到下一档判据。选 `0` 只是语义上更好读:「都在
        // 置顶档里、谁也不比谁靠前」,比「全都不置顶」这种双重否定顺口。
        return 0;
    }
    match frozen.get(&p.id) {
        Some(Lamp::Lit) => 0,
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
    frozen_lamps: &std::collections::BTreeMap<crate::ProjectId, Lamp>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ProjectId, SessionId};
    use std::collections::BTreeMap;

    fn proj(nodes: &[u64], preferred: Option<u64>) -> crate::ProjectRecord {
        crate::ProjectRecord {
            id: crate::ProjectId(1),
            name: "我的项目".into(),
            note: String::new(),
            nodes: nodes.iter().copied().map(crate::SessionId).collect(),
            preferred: preferred.map(crate::SessionId),
            dir: "/srv/app".into(),
            tmux_name: None,
            created_at: "2026-09-01T00:00:00Z".into(),
            last_accessed_at: None,
            archived_at: None,
            memos: Vec::new(),
            icon: None,
        }
    }

    /// F304:直连压过首选 —— 首选是跳板那条,直连那条照样排第一。
    ///
    /// 自证会变红:删掉 `candidates` 里的 `sort_by_key`。
    #[test]
    fn a_direct_node_is_tried_before_a_routed_preferred_one() {
        let routed = |id: crate::SessionId| id.0 == 7;
        assert_eq!(
            candidates(&proj(&[7, 9], Some(7)), routed),
            vec![crate::SessionId(9), crate::SessionId(7)]
        );
    }

    /// F304:同一组内首选排第一、其余按 `nodes` 的顺序;两组各自如此。
    ///
    /// 自证会变红:删掉把首选提到最前的那一步(直接用 `p.nodes` 排序)。
    #[test]
    fn within_each_group_the_preferred_node_leads_and_the_rest_keep_their_order() {
        let routed = |id: crate::SessionId| id.0 >= 20;
        assert_eq!(
            candidates(&proj(&[20, 11, 21, 12, 22, 13], Some(21)), routed),
            [11, 12, 13, 21, 20, 22].map(crate::SessionId).to_vec()
        );
        assert_eq!(
            candidates(&proj(&[11, 12, 13], Some(13)), |_| false),
            vec![
                crate::SessionId(13),
                crate::SessionId(11),
                crate::SessionId(12)
            ]
        );
    }

    fn sess(id: u64, name: &str, host: &str) -> crate::SessionRecord {
        crate::SessionRecord {
            id: crate::SessionId(id),
            modified_at: "t".into(),
            identity: crate::Identity {
                name: name.into(),
                note: String::new(),
                group_id: None,
                tags: Vec::new(),
            },
            connection: crate::Connection {
                host: host.into(),
                port: 22,
                protocol: crate::Protocol::Ssh,
            },
            auth: crate::Auth::inline("u", crate::AuthKind::Password),
            terminal: Default::default(),
            appearance: Default::default(),
            network: Default::default(),
            automation: Default::default(),
            sftp: Default::default(),
            last_connected_at: None,
        }
    }

    fn pr(name: &str, dir: &str, nodes: &[u64]) -> crate::ProjectRecord {
        crate::ProjectRecord {
            id: crate::ProjectId(1),
            name: name.into(),
            note: String::new(),
            nodes: nodes.iter().map(|n| crate::SessionId(*n)).collect(),
            preferred: None,
            dir: dir.into(),
            tmux_name: None,
            created_at: "2026-09-01T00:00:00Z".into(),
            last_accessed_at: None,
            archived_at: None,
            memos: Vec::new(),
            icon: None,
        }
    }

    /// 空查询放行全部 —— 调用方不用特判「还没输字」。
    #[test]
    fn an_empty_query_lets_every_project_through() {
        assert!(matches(&pr("接口", "/srv/api", &[]), "", &[]));
        assert!(matches(&pr("接口", "/srv/api", &[]), "   ", &[]));
    }

    #[test]
    fn name_and_directory_both_match_case_insensitively() {
        let p = pr("API 网关", "/srv/Api", &[]);
        assert!(matches(&p, "api", &[]));
        assert!(matches(&p, "/SRV", &[]));
        assert!(!matches(&p, "数据库", &[]));
    }

    /// 用户记得住的常是机器名或 IP 尾数,不是当初给活起的名字 —— 与
    /// `session_manager::list::matches` 收 host/tags 是同一条理由。
    ///
    /// 自证会变红:把 `visible_fields` 里那个 `for id in &p.nodes` 循环删掉。
    #[test]
    fn a_project_is_found_by_the_name_or_host_of_any_node_it_can_dial() {
        let ss = vec![sess(7, "web01", "10.0.0.9"), sess(8, "web02", "10.0.0.10")];
        let p = pr("接口", "/srv/api", &[7, 8]);
        assert!(matches(&p, "web02", &ss), "按节点会话名没搜到");
        assert!(matches(&p, "0.0.10", &ss), "按节点主机没搜到");
    }

    /// 只收**这个项目自己的**节点。收全表的话,任意一条会话名都能把所有项目
    /// 一起捞出来 —— 搜索仍然「有反应」,但等于失效。
    ///
    /// 自证会变红:把 `visible_fields` 里的 `sessions.iter().find(|s| s.id == *id)`
    /// 换成 `sessions.first()`。
    #[test]
    fn a_session_that_is_not_a_node_of_this_project_never_makes_it_match() {
        let ss = vec![sess(7, "web01", "10.0.0.9"), sess(9, "db01", "10.0.0.20")];
        let p = pr("接口", "/srv/api", &[7]);
        assert!(!matches(&p, "db01", &ss), "不是这个项目的节点也命中了");
    }

    // ---- F245:说明 / tmux 名参与搜索 --------------------------------------

    /// F245:说明(`note`)参与匹配。用户记得住的常是「那个跑爬虫的」,而项目名
    /// 多半是个代号。
    ///
    /// 自证会变红:把 `hidden_fields` 里的 `p.note.clone()` 换成 `String::new()`。
    #[test]
    fn a_project_is_found_by_a_word_from_its_note() {
        let mut p = pr("proj-7", "/srv/api", &[]);
        p.note = "每天凌晨跑爬虫,产出丢到 oss".into();
        assert!(matches(&p, "爬虫", &[]), "按说明没搜到");
    }

    /// F245:搜的是**最终会 attach 的** tmux 名,不是裸 `p.tmux_name`。
    ///
    /// 留空的项目(默认就是 `None`)会回落项目名 + `sanitize_tmux_name`,
    /// 而用户从远端 `tmux ls` 抄出来的正是那个改造后的名字:项目名 `web:生产`
    /// 的会话在 tmux 里叫 `web-生产`,拿裸字段的话永远搜不到。
    ///
    /// 自证会变红:把 `hidden_fields` 里的 `project_tmux_name(p)` 换成
    /// `p.tmux_name.clone().unwrap_or_default()`。
    #[test]
    fn a_project_is_found_by_the_tmux_name_it_will_actually_attach_to() {
        let explicit = {
            let mut p = pr("接口", "/srv/api", &[]);
            p.tmux_name = Some("claude-api".into());
            p
        };
        assert!(matches(&explicit, "claude-api", &[]), "显式 tmux 名没搜到");

        // 留空 → 回落项目名并 sanitize(`:` → `-`)。
        let derived = pr("web:生产", "/srv/api", &[]);
        assert!(
            matches(&derived, "web-生产", &[]),
            "推导出来的 tmux 名没搜到"
        );
    }

    /// F245:词之间 AND、字段之间 OR —— 两个词可以分别落在说明和节点主机上。
    #[test]
    fn two_words_may_land_on_the_note_and_on_a_node_host() {
        let ss = vec![sess(7, "web01", "10.0.2.219")];
        let mut p = pr("proj-7", "/srv/api", &[7]);
        p.note = "每天凌晨跑爬虫".into();
        assert!(matches(&p, "爬虫 219", &ss));
        assert!(!matches(&p, "爬虫 220", &ss), "有一个词落空还放行了");
    }

    /// F245:只有说明命中时,行要能自己解释「凭什么出现」—— 说明和 tmux 名
    /// **行上一个字都不显示**,不给片段的话这一行完全没有线索。
    ///
    /// 自证会变红:把 `hidden_hit_snippet` 整个改成恒返回 `None`。
    #[test]
    fn a_row_that_only_matched_the_note_offers_a_snippet_of_it() {
        let mut p = pr("proj-7", "/srv/api", &[]);
        p.note = "每天凌晨跑爬虫,产出丢到 oss".into();
        let got = hidden_hit_snippet(&p, &[], "爬虫").expect("只命中说明,该给片段");
        assert!(got.contains("爬虫"), "片段里没有命中词本身:{got}");
    }

    /// F340:备忘参与搜索,且只命中备忘时行要给出片段(多条备忘都要搜得到,
    /// 不止第一条)。
    ///
    /// 自证会变红:把 `hidden_fields` 里那句 `out.extend(..memos..)` 删掉。
    #[test]
    fn memos_are_searchable_and_explain_themselves_in_the_snippet() {
        let mut p = pr("proj-7", "/srv/api", &[]);
        p.memos = vec![
            crate::Memo {
                text: "先看日志".into(),
                created_at: String::new(),
            },
            crate::Memo {
                text: "11-07 23:00 前查一次状态".into(),
                created_at: String::new(),
            },
        ];
        assert!(matches(&p, "23:00", &[]), "第二条备忘搜不到");
        assert!(!matches(&p, "不存在的词", &[]));
        let got = hidden_hit_snippet(&p, &[], "23:00").expect("只命中备忘,该给片段");
        assert!(got.contains("23:00"), "片段里没有命中词:{got}");
    }

    /// **只有**隐藏字段命中才换副标题。普通搜索(打项目名 / 目录 / 节点名)的
    /// 行不该平白变样 —— 目录和节点名比一段说明更能认出这是哪个活。
    ///
    /// 自证会变红:把 `hidden_hit_snippet` 里那句
    /// `if crate::search::token_hits(tok, &visible) { return None; }` 删掉。
    #[test]
    fn a_row_whose_visible_fields_already_explain_it_keeps_its_subtitle() {
        let mut p = pr("爬虫网关", "/srv/api", &[]);
        p.note = "每天凌晨跑爬虫".into();
        assert_eq!(
            hidden_hit_snippet(&p, &[], "爬虫"),
            None,
            "项目名里就有这个词,不该换成说明片段"
        );
    }

    /// 片段两端截断处要有省略号,**换行符要换成空格**。
    ///
    /// 说明是多行的(F237),片段跨行时直接画会在 `LayoutJob` 里换行,把两行
    /// 的行高撑成三行 —— 而 `project_row` 的 `NAME_TOP`/`SUB_TOP` 是写死的
    /// 常量、三处列表共用。
    ///
    /// 自证会变红:把 `snippet_around` 里的 `if c.is_control()` 那个 map 去掉。
    #[test]
    fn a_snippet_marks_where_it_was_cut_and_never_carries_a_newline() {
        let text =
            "0123456789ABCDEFGH 命中\n后面还有很长很长很长很长很长很长很长很长很长很长的一段话";
        let got = snippet_around(text, "命中").expect("该命中");
        assert!(got.starts_with('…'), "左端截断没加省略号:{got}");
        assert!(got.ends_with('…'), "右端截断没加省略号:{got}");
        assert!(!got.contains('\n'), "片段里带了换行:{got:?}");
        assert!(got.contains("命中"), "片段里没有命中词:{got}");
    }

    /// 命中在开头 / 结尾时不加多余的省略号 —— 没截掉东西却画一个「…」,
    /// 用户会以为前面还有内容。
    #[test]
    fn a_snippet_that_cut_nothing_has_no_ellipsis() {
        assert_eq!(snippet_around("爬虫", "爬虫").as_deref(), Some("爬虫"));
        assert_eq!(snippet_around("不相干", "爬虫"), None);
    }

    fn lamps(pairs: &[(u64, Lamp)]) -> BTreeMap<crate::ProjectId, Lamp> {
        pairs
            .iter()
            .map(|(id, l)| (crate::ProjectId(*id), *l))
            .collect()
    }

    fn listed(
        id: u64,
        name: &str,
        accessed: Option<&str>,
        archived: Option<&str>,
    ) -> ProjectRecord {
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

    /// 浏览态的「在用」tab 只列没归档的。
    #[test]
    fn the_active_tab_lists_only_projects_that_are_not_archived() {
        let ps = vec![
            listed(1, "在用的", Some("2026-09-10T00:00:00Z"), None),
            listed(
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
            listed(
                1,
                "先归的",
                Some("2026-09-10T00:00:00Z"),
                Some("2026-09-01T00:00:00Z"),
            ),
            listed(
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
            listed(1, "活 alpha", Some("2026-09-01T00:00:00Z"), None),
            listed(
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
            listed(1, "没开过的", None, None),
            listed(2, "开过的", Some("2026-09-01T00:00:00Z"), None),
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
            listed(9, "后写的", Some("2026-09-01T00:00:00Z"), None),
            listed(2, "先写的", Some("2026-09-01T00:00:00Z"), None),
        ];
        let got: Vec<u64> = rows(&ps, Tab::Active, "", &[], &BTreeMap::new())
            .iter()
            .map(|p| p.id.0)
            .collect();
        assert_eq!(got, vec![2, 9]);
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
        let ps = vec![listed(1, "老活", None, Some("2026-09-01T00:00:00Z"))];
        assert_eq!(
            empty_reason(&ps, Tab::Active, "找不到的词", &[], &BTreeMap::new()),
            Some(EmptyReason::NoMatch)
        );
    }

    /// 归档 tab 空了是第四档,不能复用「还没有项目」。
    #[test]
    fn an_empty_archive_tab_says_so_instead_of_claiming_there_are_no_projects() {
        let ps = vec![listed(1, "在用的", None, None)];
        assert_eq!(
            empty_reason(&ps, Tab::Archived, "", &[], &BTreeMap::new()),
            Some(EmptyReason::NoArchived)
        );
    }

    /// 有行可列时不能返回空态 —— 反了的话列表和提示会同时出现。
    #[test]
    fn a_non_empty_list_has_no_empty_reason() {
        let ps = vec![listed(1, "在用的", None, None)];
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
            listed(1, "在用的", None, None),
            listed(2, "归档的", None, Some("2026-09-01T00:00:00Z")),
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
            listed(1, "旧", Some("2026-09-01T00:00:00Z"), None),
            listed(2, "新", Some("2026-09-07T00:00:00Z"), None),
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
            listed(1, "亮着的老活", Some("2026-09-01T00:00:00Z"), None),
            listed(2, "刚开过的", Some("2026-09-10T00:00:00Z"), None),
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
            listed(1, "灯未知的老活", Some("2026-09-01T00:00:00Z"), None),
            listed(2, "刚开过的", Some("2026-09-10T00:00:00Z"), None),
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
            listed(1, "亮着的 alpha", Some("2026-09-01T00:00:00Z"), None),
            listed(2, "刚开过的 alpha", Some("2026-09-10T00:00:00Z"), None),
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
            listed(
                1,
                "亮着的",
                Some("2026-09-30T00:00:00Z"),
                Some("2026-09-01T00:00:00Z"),
            ),
            listed(
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
        let ps = vec![listed(1, "新建的", None, None)];
        let got: Vec<u64> = rows(&ps, Tab::Active, "", &[], &BTreeMap::new())
            .iter()
            .map(|p| p.id.0)
            .collect();
        assert_eq!(got, vec![1], "冻结表里没有它,也必须画出来");
    }
}
