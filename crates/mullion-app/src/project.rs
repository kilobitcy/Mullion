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
