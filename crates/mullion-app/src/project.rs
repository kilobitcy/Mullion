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
}
