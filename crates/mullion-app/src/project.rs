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

/// 打开项目的决策。零 IO 纯函数 —— 把「选哪条路线」和「要不要先问」这两件
/// 各自会出错的事从事件循环里摘出来。
///
/// 选路线:`preferred` 优先,**但必须真在 `nodes` 里** —— 用户把首选那条从
/// 列表里去掉、`preferred` 却没跟着清的话(F189 下别的实例改了配置就会发生),
/// 拿它去拨号会连到一台已经不属于这个项目的机器上。`validate_project` 那道
/// 闸只管保存路径,读回来的旧数据不受它管。
///
/// **没有自动故障转移**(设计拍板):首选连不上就报错,由用户自己决定换哪条。
/// 悄悄换一条的话,用户以为自己在 A 机器上干活,其实在 B 机器上。
pub fn plan_open(p: &mullion_store::ProjectRecord, risk: AtRisk) -> OpenStep {
    let node = p
        .preferred
        .filter(|id| p.nodes.contains(id))
        .or_else(|| p.nodes.first().copied());
    let Some(node) = node else {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
