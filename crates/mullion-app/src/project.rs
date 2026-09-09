//! F223/F224:「打开项目」的 app 侧纯判据。零 UI、零 IO,可纯单测。
//!
//! 设计见 `docs/superpowers/specs/2026-09-08-f221-f225-project-unit-design.md`。

/// F223:打开项目要把当前 pane 从原连接上摘下来,摘之前这块 pane 上**真会丢**
/// 的东西。
///
/// 三个字段就是全部拦截理由。**没有第四种**——按设计 P5 项目一律走 tmux,
/// 断开 pane 不丢远端工作,那正是 tmux 的意义。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AtRisk {
    /// 内置编辑器里有未保存的改动。
    pub unsaved_edits: bool,
    /// 有 SFTP 传输在途。
    pub transfer_in_flight: bool,
    /// 这块 pane **确定**不在 tmux 里(裸 shell,断开是真丢)。
    /// 判据见 [`bare_shell`] —— 不能直接拿 `tmux.is_none()`。
    pub bare_shell: bool,
}

/// 要给用户看的拦截理由。空 = 直接开,不弹确认。
///
/// **否掉了「总是确认」**:打开项目是日常高频动作(切活),高频路径上的无谓
/// 确认,用户三天就学会闭眼点「确定」——那时它对真正危险的那三种也一起失效了。
pub fn confirm_reasons(r: AtRisk) -> Vec<&'static str> {
    let mut out = Vec::new();
    if r.unsaved_edits {
        out.push("编辑器里有未保存的改动");
    }
    if r.transfer_in_flight {
        out.push("有文件传输还没完成");
    }
    if r.bare_shell {
        out.push("当前 pane 不在 tmux 里,断开会丢掉正在跑的东西");
    }
    out
}

/// 「这块 pane 确定不在 tmux 里」。
///
/// **`title_ever_seen == false` 不算**——那是「还没收到上报」,不是「没有
/// tmux」。拿裸 `tmux.is_none()` 当判据的话,高延迟链路上(本项目的主场景)
/// 首字节还没回来就先弹一个确认框,而用户的 pane 明明好端端在 tmux 里。
/// 症状是「确认框有时弹有时不弹」,跟着链路快慢飘,几乎查不出来。
pub fn bare_shell(title_ever_seen: bool, tmux: Option<&str>) -> bool {
    title_ever_seen && tmux.is_none()
}

/// F223:点「打开项目」之后下一步做什么。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenStep {
    /// 直接开:把当前 pane 挂到这条会话上。
    Go(mullion_store::SessionId),
    /// 先问。`Vec` 是逐条理由(见 [`confirm_reasons`]),问完再走 [`OpenStep::Go`]。
    Ask(mullion_store::SessionId, Vec<&'static str>),
    /// 开不了,把话说清楚。
    Refuse(&'static str),
}

/// 这个项目会往哪台机器上拨。
///
/// `preferred` 优先,**但必须真在 `nodes` 里** —— 用户把首选那条从列表里去掉、
/// `preferred` 却没跟着清的话(F189 下别的实例改了配置就会发生),拿它去拨号会
/// 连到一台已经不属于这个项目的机器上。`validate_project` 那道闸只管保存路径,
/// 读回来的旧数据不受它管。
///
/// 单独摘出来是因为**列表上写着的节点名必须和点下去真连的那台是同一条判据**
/// (F225① launcher 每行都写着节点名)。各写一份的话,「显示 A、连上 B」是这类
/// 界面里最难查的一种错。
pub fn node_for(p: &mullion_store::ProjectRecord) -> Option<mullion_store::SessionId> {
    p.preferred
        .filter(|id| p.nodes.contains(id))
        .or_else(|| p.nodes.first().copied())
}

/// F233:一个项目是否命中搜索词。空查询(trim 后为空)放行全部。
///
/// 匹配**项目名 / 目录 / 每一条节点会话的名字与主机**,大小写不敏感。
/// 收节点是因为用户记得住的常是机器名或 IP 尾数,不是当初给活起的名字 ——
/// 与 `session_manager::list::matches` 收 host/tags 是同一条理由。
///
/// 收**全部** `nodes` 而不只是首选:多节点正是「同一台机器的等价路线」,
/// 用户搜哪条路线的名字都该找到这个活。
///
/// 只看这个项目自己的节点(`s.id == *id`)。丢掉 id 比对的话,任意一条会话名
/// 都能把全部项目一起捞出来 —— 搜索仍然「有反应」,但等于失效。
///
/// **不收 `note`**:F237 把说明改成了多行,长文本参与匹配会让搜索命中一堆
/// 用户在列表上看不见的东西。
///
/// 三处列表(项目管理器左栏 / 启动页 / pane 切换弹窗)共用这一份 —— 各写一份
/// 的话,同一个搜索词在两个界面给出不同结果,而用户几分钟内就会都看到一遍。
pub fn matches(
    p: &mullion_store::ProjectRecord,
    query: &str,
    sessions: &[mullion_store::SessionRecord],
) -> bool {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    if p.name.to_lowercase().contains(&q) || p.dir.to_lowercase().contains(&q) {
        return true;
    }
    p.nodes.iter().any(|id| {
        sessions.iter().any(|s| {
            s.id == *id
                && (s.identity.name.to_lowercase().contains(&q)
                    || s.connection.host.to_lowercase().contains(&q))
        })
    })
}

/// F236:「+ 添加项目」用的默认名。
///
/// 「新项目」,撞名就往后找**第一个空号**(「新项目 2」「新项目 3」…)。
/// 不是 max+1:删掉「新项目」再点添加,给出的应该是「新项目」,而不是跳过
/// 一堆空号变成「新项目 7」。
///
/// 为什么必须去重:`mullion_store::validate_project` 要求项目名全局唯一,而
/// `ProjectIntent::Add` 是**立刻落盘**的。不去重就会在盘上建出一条必然存不
/// 进去的记录 —— 列表里两行同名、右栏「保存」灰着,用户看不出为什么。
pub fn fresh_project_name(existing: &[mullion_store::ProjectRecord]) -> String {
    const BASE: &str = "新项目";
    let taken = |cand: &str| existing.iter().any(|p| p.name == cand);
    if !taken(BASE) {
        return BASE.to_string();
    }
    (2..)
        .map(|n| format!("{BASE} {n}"))
        .find(|cand| !taken(cand))
        .expect("2.. 是无穷序列,find 必然返回")
}

/// 打开项目的决策。零 IO 纯函数 —— 把「选哪条路线」和「要不要先问」这两件
/// 各自会出错的事从事件循环里摘出来。
///
/// **没有自动故障转移**(设计拍板):首选连不上就报错,由用户自己决定换哪条。
/// 悄悄换一条的话,用户以为自己在 A 机器上干活,其实在 B 机器上。
pub fn plan_open(p: &mullion_store::ProjectRecord, risk: AtRisk) -> OpenStep {
    let Some(node) = node_for(p) else {
        return OpenStep::Refuse("这个项目还没有节点,先在项目管理器里勾一条。");
    };
    let reasons = confirm_reasons(risk);
    if reasons.is_empty() {
        OpenStep::Go(node)
    } else {
        OpenStep::Ask(node, reasons)
    }
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

/// F224:一盏灯。`panes` 是本机**所有实例**每块 pane 的
/// `(是否上报过, 上报的 tmux 名)`;`others` 是别的实例心跳文件里那批
/// tmux 名(它们的 pane 我们看不见,只能信心跳)。
///
/// 判据就是 P7 的那张三态表,**不另造一套记账**:「项目 X 在跑」= 有 pane
/// 报出的 tmux 名等于 `project_tmux_name(X)`。这样一来,用户不走项目入口、
/// 直接连会话 attach 进那个会话,灯照样亮 —— 两套记账才会出现「明明在跑
/// 灯却不亮」。
///
/// 「灭」**故意不要求远端核对过**:核对只在 attach 前那一刻发生,要求它的话
/// 「灭」永远不可达,三态实际退化成两态。它的已知盲区(非 Mullion 的 client,
/// 比如 PowerShell 直接 ssh 上去 attach)由 attach 前的远端核对兜底 ——
/// 那才是产生后果的时刻。
pub fn lamp(project_tmux: &str, panes: &[(bool, Option<&str>)], others: &[String]) -> Lamp {
    if project_tmux.is_empty() {
        return Lamp::Dark;
    }
    if others.iter().any(|n| n == project_tmux) {
        return Lamp::Lit;
    }
    if panes.iter().any(|(_, name)| *name == Some(project_tmux)) {
        return Lamp::Lit;
    }
    if panes.iter().any(|(seen, _)| !seen) {
        return Lamp::Unknown;
    }
    Lamp::Dark
}

/// F224:此刻**正命中**的项目集合。
///
/// `reports` 是各 pane 上报的 tmux 名(只收上报过的那些)。
pub fn hits(
    projects: &[mullion_store::ProjectRecord],
    reports: &[&str],
) -> std::collections::BTreeSet<mullion_store::ProjectId> {
    projects
        .iter()
        .filter(|p| {
            let name = mullion_store::project_tmux_name(p);
            !name.is_empty() && reports.iter().any(|r| *r == name)
        })
        .map(|p| p.id)
        .collect()
}

/// F225③:这块 pane 属于哪个项目 —— 判据与 [`hits`] 同一条(上报的 tmux 名
/// == `project_tmux_name`),只是这里要的是**那一个**而不是一整个集合。
///
/// `report` 是这块 pane 上报的 tmux 名。`None`(还没上报)一律不属于任何项目
/// —— 把「还不知道」当成命中的话,刚开的 pane 会先顶着一个错项目名。
pub fn project_of<'a>(
    report: Option<&str>,
    projects: &'a [mullion_store::ProjectRecord],
) -> Option<&'a mullion_store::ProjectRecord> {
    let report = report?;
    projects.iter().find(|p| {
        let name = mullion_store::project_tmux_name(p);
        !name.is_empty() && name == report
    })
}

/// F224:该给哪些项目记一笔访问时间。
///
/// **跃迁触发,不是电平触发。** 上报是持续的(每几秒一批),照字面「命中就
/// 更新」等于**每几秒往 `sessions.toml` 写一次盘** —— 切片 T-b 的原话是
/// 「播报判据是跃迁不是当前状态」,这里是同一个坑。
///
/// pane 断开再接回、或从项目 A 的 tmux 切到项目 B,都会先离开集合再进来,
/// 于是各自是一次新的跃迁 —— 该记的都记得上。
///
/// 「非 `Completed` 的结局不记」(等首字节超时 / 用户接管 / 断线,T11)被这条
/// 判据**自动蕴含**:那些结局下 attach 压根没发生,命中上报永远到不了。
pub fn newly_entered(
    prev: &std::collections::BTreeSet<mullion_store::ProjectId>,
    now: &std::collections::BTreeSet<mullion_store::ProjectId>,
) -> Vec<mullion_store::ProjectId> {
    now.difference(prev).copied().collect()
}

// ---- F224 attach 前的远端二次核对 --------------------------------------

/// 核对命令:这个 tmux 会话此刻挂着几个 client。
///
/// 走**独立的 exec channel**,不进 PTY —— PTY 那边此刻是一个干净的 shell
/// 停在提示符上,往里写字节会破坏「恰好一个 Step」这条不变量。
///
/// `2>/dev/null`:会话不存在时 tmux 往 stderr 喷一行错,而「不存在」正是
/// 最常见的正常情况(第一次打开这个项目),不该在日志里当异常记。
pub fn list_clients_command(tmux: &str) -> Vec<u8> {
    let mut out = b"tmux list-clients -t ".to_vec();
    out.extend_from_slice(&mullion_ssh::exec::shell_quote(tmux.as_bytes()));
    out.extend_from_slice(b" 2>/dev/null");
    out
}

/// 上面那条命令的输出里有几个 client。
///
/// **一行一个 client**。空行不算 —— 会话不存在时 tmux 什么都不输出,而
/// `"".lines()` 给出零行、`"\n".lines()` 给出一个空行,后者会被数成 1
/// 然后弹一个「已在别处打开」的确认框,而实际上一个人都没有。
pub fn clients_in_output(stdout: &str) -> usize {
    stdout.lines().filter(|l| !l.trim().is_empty()).count()
}

/// 核对结论 → 要不要停下来问用户。`Some(n)` = 有 n 个客户端挂着,得问;
/// `None` = 直接按现状(不带 `-d`)发。
///
/// 入参的 `None` 是「核对本身没跑成」,**按无人处理**(fail-open),
/// 理由见 `a_check_that_could_not_run_is_treated_as_nobody_being_attached`。
pub fn takeover_needed(clients: Option<usize>) -> Option<usize> {
    clients.filter(|n| *n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(id: u64, name: &str) -> mullion_store::ProjectRecord {
        mullion_store::ProjectRecord {
            id: mullion_store::ProjectId(id),
            name: name.into(),
            note: String::new(),
            nodes: Vec::new(),
            preferred: None,
            dir: "/srv/api".into(),
            tmux_name: None,
            created_at: "t".into(),
            last_accessed_at: None,
            icon: None,
        }
    }

    /// F225③:「这块 pane 属于哪个项目」由**上报的 tmux 名**现推,与 F224
    /// 那盏灯同一条判据。同一条的好处是:用户绕过项目入口、自己 attach 进
    /// 那个会话,标题条照样认得出来。
    #[test]
    fn a_pane_belongs_to_the_project_whose_tmux_session_it_reports() {
        let ps = [named(1, "我的项目"), named(2, "别的活")];
        let name = mullion_store::project_tmux_name(&ps[1]);
        assert_eq!(project_of(Some(&name), &ps).map(|p| p.id.0), Some(2));
    }

    /// 还没上报(刚连上)/ 报的是别的会话 —— 都**不属于**任何项目。
    ///
    /// 尤其是 `None`:不许把「还不知道」当成命中某个项目,否则刚开的 pane
    /// 会先顶着一个错项目名,几秒后才跳回去。
    #[test]
    fn a_pane_that_has_not_reported_yet_belongs_to_nothing() {
        let ps = [named(1, "我的项目")];
        assert!(project_of(None, &ps).is_none());
        assert!(project_of(Some("随便一个会话"), &ps).is_none());
    }

    /// 核对失败(exec 起不来 / 账号被 `ForceCommand` 挡住 / 远端根本没有
    /// tmux)必须**按无人处理**,而不是当成「有人」去弹确认框。
    ///
    /// 这里刻意 fail-open,与本项目其余安全判据(TOFU 那类)相反,理由是
    /// 两边的失败代价不对称:fail-closed 的话,凡是 exec 通不了的环境
    /// (sftp-only 账号、老 tmux)每次打开项目都要被问一句「已在别处打开
    /// (0 个客户端)」——一句我们根本没证据支持的话,而用户唯一学得会的
    /// 反应是闭眼点继续,那时它对真有人挂着的那次也一起失效。
    ///
    /// 自证会变红:把 `takeover_needed` 的 `None` 分支改成 `Some(0)` 之外
    /// 的任何值。
    #[test]
    fn a_check_that_could_not_run_is_treated_as_nobody_being_attached() {
        assert_eq!(takeover_needed(None), None);
        assert_eq!(takeover_needed(Some(0)), None);
        assert_eq!(takeover_needed(Some(2)), Some(2));
    }

    /// 核对命令必须把会话名**引起来**:tmux 名允许空格与 CJK
    /// (`sanitize_tmux_name` 只滤控制字符和几个定址前缀),不引的话
    /// `我的 项目` 会被 shell 拆成两个参数,核对恒查错东西。
    #[test]
    fn the_check_command_quotes_the_session_name() {
        let cmd = String::from_utf8(list_clients_command("我的 项目")).unwrap();
        assert_eq!(cmd, "tmux list-clients -t '我的 项目' 2>/dev/null");
    }

    /// 名字里的单引号不许越出参数边界 —— 越出去就是远端任意命令执行。
    #[test]
    fn a_single_quote_in_the_name_cannot_escape_the_argument() {
        let cmd = String::from_utf8(list_clients_command("a'; id; echo '")).unwrap();
        assert_eq!(
            cmd,
            r#"tmux list-clients -t 'a'\''; id; echo '\''' 2>/dev/null"#
        );
    }

    /// **空输出 = 没人**,不是一个人。
    ///
    /// 自证会变红:把 `clients_in_output` 改成 `stdout.lines().count()`
    /// (第二段红:一个尾随换行会被数成 1,于是每次打开一个**没人用**的
    /// 项目都弹一次「已在别处打开」)。
    #[test]
    fn an_empty_listing_means_nobody_is_attached() {
        assert_eq!(clients_in_output(""), 0);
        assert_eq!(clients_in_output("\n"), 0);
        assert_eq!(clients_in_output("   \n \n"), 0);
    }

    #[test]
    fn each_line_of_the_listing_is_one_client() {
        assert_eq!(clients_in_output("/dev/pts/3: 0 [80x24 xterm]\n"), 1);
        assert_eq!(
            clients_in_output("/dev/pts/3: 0 [80x24]\n/dev/pts/9: 0 [120x40]\n"),
            2
        );
    }

    #[test]
    fn nothing_at_risk_means_no_confirmation_at_all() {
        assert!(confirm_reasons(AtRisk::default()).is_empty());
    }

    /// 三条拦截理由各自成立。
    #[test]
    fn each_of_the_three_real_risks_raises_the_dialog_on_its_own() {
        for r in [
            AtRisk {
                unsaved_edits: true,
                ..AtRisk::default()
            },
            AtRisk {
                transfer_in_flight: true,
                ..AtRisk::default()
            },
            AtRisk {
                bare_shell: true,
                ..AtRisk::default()
            },
        ] {
            assert_eq!(confirm_reasons(r).len(), 1, "{r:?}");
        }
    }

    /// 同时成立时逐条都要说出来 —— 只报一条的话,用户处理完那条再点一次
    /// 又弹一个新的,像是软件在跟他捉迷藏。
    #[test]
    fn several_risks_at_once_are_all_spelled_out() {
        assert_eq!(
            confirm_reasons(AtRisk {
                unsaved_edits: true,
                transfer_in_flight: true,
                bare_shell: true,
            })
            .len(),
            3
        );
    }

    /// **F223 的判据红线。** 还没收到远端标题上报 ≠ 没有 tmux。
    ///
    /// 自证会变红:把 `bare_shell` 改成 `tmux.is_none()`(去掉
    /// `title_ever_seen &&`)。
    #[test]
    fn a_pane_that_has_not_reported_yet_is_not_treated_as_a_bare_shell() {
        assert!(
            !bare_shell(false, None),
            "还没上报就当裸 shell,慢链路上会乱弹确认框"
        );
        assert!(
            bare_shell(true, None),
            "上报过且说没有 tmux —— 这才是裸 shell"
        );
        assert!(!bare_shell(true, Some("proj-x")), "在 tmux 里,不拦");
    }

    // ---- plan_open -----------------------------------------------------

    fn proj(nodes: &[u64], preferred: Option<u64>) -> mullion_store::ProjectRecord {
        mullion_store::ProjectRecord {
            id: mullion_store::ProjectId(1),
            name: "我的项目".into(),
            note: String::new(),
            nodes: nodes
                .iter()
                .copied()
                .map(mullion_store::SessionId)
                .collect(),
            preferred: preferred.map(mullion_store::SessionId),
            dir: "/srv/app".into(),
            tmux_name: None,
            created_at: "2026-09-01T00:00:00Z".into(),
            last_accessed_at: None,
            icon: None,
        }
    }

    #[test]
    fn the_preferred_node_is_the_one_we_dial() {
        assert_eq!(
            plan_open(&proj(&[7, 9], Some(9)), AtRisk::default()),
            OpenStep::Go(mullion_store::SessionId(9))
        );
    }

    /// 没配首选就用列表里第一条 —— 不是「拒绝打开」。多数项目只有一条路线,
    /// 逼用户为一条路线的项目去点一次「首选」是没有意义的仪式。
    #[test]
    fn a_project_without_a_preferred_node_just_uses_the_first_one() {
        assert_eq!(
            plan_open(&proj(&[7, 9], None), AtRisk::default()),
            OpenStep::Go(mullion_store::SessionId(7))
        );
    }

    /// **首选必须真在 `nodes` 里。**
    ///
    /// `preferred` 与 `nodes` 脱节是读回来的旧数据里真会有的状态(F189:别的
    /// 实例把那条节点从项目里去掉了,而本实例内存里还留着旧的 `preferred`;
    /// `validate_project` 只管保存路径,管不到读回来的)。不过滤的话会拨到
    /// 一台**已经不属于这个项目**的机器上,而界面上写的还是项目名。
    ///
    /// 自证会变红:把 `.filter(|id| p.nodes.contains(id))` 去掉。
    #[test]
    fn a_preferred_node_that_is_no_longer_in_the_list_is_ignored_not_dialed() {
        assert_eq!(
            plan_open(&proj(&[7], Some(9)), AtRisk::default()),
            OpenStep::Go(mullion_store::SessionId(7))
        );
    }

    #[test]
    fn a_project_with_no_nodes_at_all_is_refused_with_a_reason() {
        assert!(matches!(
            plan_open(&proj(&[], None), AtRisk::default()),
            OpenStep::Refuse(_)
        ));
    }

    /// 有东西会丢时先问,**但节点已经选好了** —— 问完直接开,不用再算一遍
    /// (再算一遍的话,确认框开着的那段时间里配置变了就会拨到别处)。
    #[test]
    fn a_risky_open_still_carries_the_node_it_already_picked() {
        let step = plan_open(
            &proj(&[7, 9], Some(9)),
            AtRisk {
                bare_shell: true,
                ..AtRisk::default()
            },
        );
        assert_eq!(
            step,
            OpenStep::Ask(
                mullion_store::SessionId(9),
                confirm_reasons(AtRisk {
                    bare_shell: true,
                    ..AtRisk::default()
                })
            )
        );
    }

    // ---- F224 灯与访问时间 ----------------------------------------------

    /// 三态各一条。
    ///
    /// 自证会变红:把 `lamp` 里 `panes.iter().any(|(seen, _)| !seen)` 那一段
    /// 删掉(第三段红 —— 「未知」会被冒充成「灭」)。
    #[test]
    fn the_lamp_has_three_states_and_unknown_is_not_allowed_to_masquerade_as_dark() {
        // 亮:有 pane 报出这个名字。
        assert_eq!(
            lamp("proj-x", &[(true, Some("proj-x")), (true, None)], &[]),
            Lamp::Lit
        );
        // 灭:所有 pane 都上报过,无一命中。
        assert_eq!(
            lamp("proj-x", &[(true, Some("别的")), (true, None)], &[]),
            Lamp::Dark
        );
        // 未知:还有 pane 没上报过 —— 它可能正 attach 在这个项目里。
        assert_eq!(
            lamp("proj-x", &[(false, None), (true, Some("别的"))], &[]),
            Lamp::Unknown
        );
    }

    /// 一块 pane 都没有(刚启动、只有 launcher)也是**灭**,不是未知 ——
    /// 没有任何「可能正 attach 着」的候选。
    #[test]
    fn no_panes_at_all_is_dark_not_unknown() {
        assert_eq!(lamp("proj-x", &[], &[]), Lamp::Dark);
    }

    /// 别的实例的心跳同样点亮 —— 多开是本项目的主场景,只看自己那几块 pane
    /// 的话,另一个窗口里正跑着的项目在这边显示为「灭」,用户会去开第二份。
    ///
    /// 而且它**盖过「未知」**:心跳是确凿证据,不该被一块还没上报的 pane 拖成未知。
    #[test]
    fn another_instances_heartbeat_lights_the_lamp_even_while_our_own_panes_are_silent() {
        assert_eq!(
            lamp("proj-x", &[(false, None)], &["proj-x".to_string()]),
            Lamp::Lit
        );
    }

    /// **P7 的自证**:判据是「上报的 tmux 名」,不是「我们从项目入口打开过」。
    ///
    /// 用户不走项目入口、直接连会话 attach 进那个 tmux 会话,一样算命中。
    /// 只钉项目入口那条路的话这条恒绿 —— 所以这里刻意不经过任何项目入口。
    #[test]
    fn a_tmux_session_entered_the_ordinary_way_still_counts_as_the_project_running() {
        let mut p = proj(&[7], None);
        p.name = "我的项目".into();
        p.tmux_name = Some("proj-x".into());
        assert_eq!(
            hits(std::slice::from_ref(&p), &["proj-x"]),
            [p.id].into_iter().collect()
        );
    }

    /// tmux 名算空的项目**不许命中** —— `reports` 里混进一个空串(远端报了
    /// 一条怪标题)就会把所有空名项目一起点亮。
    #[test]
    fn a_project_with_an_empty_tmux_name_never_matches_anything() {
        let mut p = proj(&[7], None);
        p.name = "   ".into();
        assert!(mullion_store::project_tmux_name(&p).is_empty(), "前提");
        assert!(hits(std::slice::from_ref(&p), &[""]).is_empty());
    }

    /// **跃迁触发,不是电平触发。** 这条是防「每几秒往 `sessions.toml` 写
    /// 一次盘」的唯一闸(切片 T-b 的原话:播报判据是跃迁不是当前状态)。
    ///
    /// 自证会变红:把 `newly_entered` 改成 `now.iter().copied().collect()`。
    #[test]
    fn two_consecutive_batches_of_the_same_hit_only_record_one_visit() {
        use std::collections::BTreeSet;
        let a: BTreeSet<_> = [mullion_store::ProjectId(1)].into_iter().collect();
        assert_eq!(
            newly_entered(&BTreeSet::new(), &a),
            vec![mullion_store::ProjectId(1)],
            "第一批命中要记一笔"
        );
        assert!(
            newly_entered(&a, &a).is_empty(),
            "同一个项目连续命中只记一笔,否则每几秒写一次盘"
        );
    }

    /// 断开再接回 / 从项目 A 切到项目 B,都是**新的**跃迁,各记各的。
    #[test]
    fn leaving_and_coming_back_is_a_fresh_visit() {
        use std::collections::BTreeSet;
        let a: BTreeSet<_> = [mullion_store::ProjectId(1)].into_iter().collect();
        let none = BTreeSet::new();
        assert!(newly_entered(&a, &none).is_empty(), "离开不记");
        assert_eq!(
            newly_entered(&none, &a),
            vec![mullion_store::ProjectId(1)],
            "回来再记一笔"
        );
    }

    /// **钉住 T11 的那条蕴含关系。** 等首字节超时 / 用户接管 / 断线这些结局下
    /// attach 压根没发出去,于是远端永远不会报出项目的 tmux 名 —— 命中集合恒空,
    /// 访问时间自然不记。不必为它单独接线,但这条推论要有守护,否则日后有人
    /// 「顺手」把访问时间挂到点击那一刻,列表就会被失败的尝试污染。
    #[test]
    fn an_attach_that_never_went_out_leaves_no_hit_and_therefore_no_visit() {
        let p = proj(&[7], None);
        // 没发出去 = 远端没报过这个名字。上报里全是别的东西(或干脆没有)。
        assert!(hits(std::slice::from_ref(&p), &[]).is_empty());
        assert!(hits(std::slice::from_ref(&p), &["别的会话"]).is_empty());
    }

    /// 没节点时**先拒绝,不问** —— 反过来的话用户点完「确定」才被告知
    /// 这个项目压根打不开,白白丢了他刚确认放弃的那些东西。
    #[test]
    fn a_project_with_no_nodes_is_refused_before_we_ask_the_user_to_give_anything_up() {
        assert!(matches!(
            plan_open(
                &proj(&[], None),
                AtRisk {
                    unsaved_edits: true,
                    ..AtRisk::default()
                }
            ),
            OpenStep::Refuse(_)
        ));
    }

    // ---- matches / fresh_project_name ----------------------------------

    fn sess(id: u64, name: &str, host: &str) -> mullion_store::SessionRecord {
        mullion_store::SessionRecord {
            id: mullion_store::SessionId(id),
            modified_at: "t".into(),
            identity: mullion_store::Identity {
                name: name.into(),
                note: String::new(),
                group_id: None,
                tags: Vec::new(),
            },
            connection: mullion_store::Connection {
                host: host.into(),
                port: 22,
                protocol: mullion_store::Protocol::Ssh,
            },
            auth: mullion_store::Auth::inline("u", mullion_store::AuthKind::Password),
            terminal: Default::default(),
            appearance: Default::default(),
            network: Default::default(),
            automation: Default::default(),
            sftp: Default::default(),
        }
    }

    fn pr(name: &str, dir: &str, nodes: &[u64]) -> mullion_store::ProjectRecord {
        mullion_store::ProjectRecord {
            id: mullion_store::ProjectId(1),
            name: name.into(),
            note: String::new(),
            nodes: nodes.iter().map(|n| mullion_store::SessionId(*n)).collect(),
            preferred: None,
            dir: dir.into(),
            tmux_name: None,
            created_at: "2026-09-01T00:00:00Z".into(),
            last_accessed_at: None,
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
    /// 自证会变红:把 `p.nodes.iter().any(..)` 那一整段删掉。
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
    /// 自证会变红:把 `s.id == *id &&` 那一段判断去掉。
    #[test]
    fn a_session_that_is_not_a_node_of_this_project_never_makes_it_match() {
        let ss = vec![sess(7, "web01", "10.0.0.9"), sess(9, "db01", "10.0.0.20")];
        let p = pr("接口", "/srv/api", &[7]);
        assert!(!matches(&p, "db01", &ss), "不是这个项目的节点也命中了");
    }

    /// 一个项目都没有时就是「新项目」,不带后缀。
    #[test]
    fn the_first_new_project_has_no_suffix() {
        assert_eq!(fresh_project_name(&[]), "新项目");
    }

    /// `validate_project` 要求项目名全局唯一。不去重就会在盘上建出一条**必然
    /// 存不进去**的记录:列表里两行同名、右栏「保存」灰着,而用户看不出为什么。
    ///
    /// 自证会变红:把整个函数改成恒返回 `"新项目".to_string()`。
    #[test]
    fn a_clashing_name_gets_the_next_free_number() {
        let ps = vec![pr("新项目", "/a", &[])];
        assert_eq!(fresh_project_name(&ps), "新项目 2");
    }

    /// 「新项目」被删了就把它让出来的号补回去,不是接着往后排。
    #[test]
    fn the_base_name_is_reused_once_it_is_free_again() {
        let ps = vec![pr("新项目 2", "/a", &[]), pr("新项目 3", "/b", &[])];
        assert_eq!(fresh_project_name(&ps), "新项目");
    }

    /// 找**第一个空号**,不是 max+1、也不是 len+1。
    ///
    /// **中间有空号**才分得出这几种实现:`["新项目", "新项目 3"]` 下补空号给
    /// 「新项目 2」,而 max+1 给「新项目 4」、len+1 给「新项目 3」—— 后者直接
    /// 撞名,建出来的记录必然存不进去。
    ///
    /// 这条是补上来的:原来那两条用例里,`len()+1` 一条走早退分支、一条数值
    /// 恰好撞巧,变异**杀不掉** —— 判据看着有,其实是恒绿的。
    ///
    /// 自证会变红:把实现的 `(2..)...find` 换成
    /// `format!("{BASE} {}", existing.len() + 1)`。
    #[test]
    fn the_lowest_free_number_is_picked_not_the_highest_plus_one() {
        let ps = vec![pr("新项目", "/a", &[]), pr("新项目 3", "/b", &[])];
        assert_eq!(fresh_project_name(&ps), "新项目 2");
    }
}
