//! Tab 角标(走查 9)。**纯函数,零 egui**——「这一页算不算配置过」的判据
//! 全是字段比对,放进渲染代码里就再也测不动了。
//!
//! **单一状态位**:一个 Tab 只显示一个符号,优先级 `缺项 > 已配置 > 无`。
//! 走查原文列了「红点表缺项、灰点表已配置、角标数字表几处覆盖」三套并存的
//! 方案,那会让四个 Tab 上同时挂三种记号 —— 信息密度上去了,可读性没了。

use crate::ui::session_manager::validate::Missing;
use crate::ui::session_manager::{
    AuthKindUi, EditorBuffer, JumpModeUi, ProxyModeUi, TAB_APPEARANCE, TAB_AUTH, TAB_AUTOMATION,
    TAB_CONNECT, TAB_SFTP,
};

/// 一个 Tab 的角标状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Badge {
    /// 这一页有必填项没填。红点。
    Missing,
    /// 这一页有偏离默认的设置。灰点。
    Configured,
    /// 什么都不画。
    None,
}

/// 某个 Tab 该显示哪个角标。
///
/// 「已配置」的判据是**偏离新建默认**,不是「非空」:「连接」页永远有名称和
/// 主机(必填),拿非空当判据的话这个点恒亮,等于没有。所以这一页只算
/// 跳板与代理 —— 它们回答的是「怎么走到这台机器」,才是这一页真正的可选配置。
pub(super) fn badge_of(tab: usize, missing: Missing, buf: &EditorBuffer) -> Badge {
    if missing.tab() == Some(tab) {
        return Badge::Missing;
    }
    let configured = match tab {
        TAB_CONNECT => {
            // 「继承」不算配置过 —— 它恰恰是「我什么都没决定」。
            // 「自定义」但链为空也不算:空链的实际效果等于「无」。
            !matches!(buf.proxy_mode, ProxyModeUi::Inherit)
                || (matches!(buf.jump_mode, JumpModeUi::Custom) && !buf.jump_chain.is_empty())
        }
        // 密码是默认认证方式,换成公钥才算动过。凭据本身填没填不看:
        // 编辑已有会话时密码框恒为空(store 不回吐明文),看它会误报。
        TAB_AUTH => matches!(buf.auth_kind, AuthKindUi::PublicKey),
        // 整个分节跟默认值比。`AutomationPrefs` 全字段 `Option`,
        // 默认即「全继承」,任何一项被显式设置都会让它不等于默认。
        TAB_AUTOMATION => buf.preserved_automation != mullion_store::AutomationPrefs::default(),
        TAB_APPEARANCE => {
            buf.preserved_appearance.icon.is_some() || buf.preserved_appearance.color.is_some()
        }
        // trim 判据跟 `build_draft` 里落回 `SftpPrefs` 的判据对齐:那边空白
        // 也会被判成「未设置」而写 `None`,角标不能说「配置过」而保存后又
        // 变回没配置的默认态。书签同理 —— `build_draft` 会把路径纯空白的整条
        // 丢掉,所以点一次「+ 添加书签」但没填内容**不算**配置过。
        TAB_SFTP => {
            !buf.sftp_default_remote.trim().is_empty()
                || !buf.sftp_default_local.trim().is_empty()
                || buf.sftp_bookmarks.iter().any(|(_, p)| !p.trim().is_empty())
        }
        _ => false,
    };
    if configured {
        Badge::Configured
    } else {
        Badge::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_store::SessionId;

    /// 新建会话四页全空,不该有任何角标 —— 满屏小点等于没有信息。
    #[test]
    fn a_fresh_buffer_has_no_badges_at_all() {
        let buf = EditorBuffer::default();
        let none = Missing::default();
        for tab in [
            TAB_CONNECT,
            TAB_AUTH,
            TAB_AUTOMATION,
            TAB_SFTP,
            TAB_APPEARANCE,
        ] {
            assert_eq!(
                badge_of(tab, none, &buf),
                Badge::None,
                "tab {tab} 不该有角标"
            );
        }
    }

    /// 缺项压过已配置:两者同时成立时只显示红点。用户先要被带去补必填,
    /// 「这页你配过东西」是次要信息(走查 9 的「单一状态位」)。
    #[test]
    fn missing_outranks_configured() {
        let buf = EditorBuffer {
            proxy_mode: ProxyModeUi::Socks5, // 「连接」页已配置
            ..Default::default()
        };
        let missing = Missing {
            name: true,
            host: false,
            user: false,
        };
        assert_eq!(missing.tab(), Some(TAB_CONNECT));
        assert_eq!(badge_of(TAB_CONNECT, missing, &buf), Badge::Missing);
    }

    /// 「连接」页的已配置判据**只算跳板和代理**。名称/主机是必填,
    /// 拿它们当判据的话这个点恒亮,等于没有(走查 9)。
    #[test]
    fn connect_badge_ignores_name_and_host() {
        let mut buf = EditorBuffer {
            name: "web01".into(),
            host: "10.0.0.1".into(),
            port: "2222".into(),
            ..Default::default()
        };
        assert_eq!(
            badge_of(TAB_CONNECT, Missing::default(), &buf),
            Badge::None,
            "填了名称/主机/端口不算「配置过连接方式」"
        );

        buf.proxy_mode = ProxyModeUi::Direct;
        assert_eq!(
            badge_of(TAB_CONNECT, Missing::default(), &buf),
            Badge::Configured,
            "显式直连是对分组的覆盖,算配置过"
        );
    }

    /// 「继承」不算配置过 —— 它恰恰是「我什么都没决定」。
    #[test]
    fn inheriting_is_not_configuring() {
        let buf = EditorBuffer {
            jump_mode: JumpModeUi::Inherit,
            proxy_mode: ProxyModeUi::Inherit,
            ..Default::default()
        };
        assert_eq!(badge_of(TAB_CONNECT, Missing::default(), &buf), Badge::None);
    }

    /// 选了「自定义」但链是空的,也不算配置过 —— 空链的实际效果等于「无」。
    #[test]
    fn an_empty_custom_jump_chain_is_not_configured() {
        let mut buf = EditorBuffer {
            jump_mode: JumpModeUi::Custom,
            ..Default::default()
        };
        buf.jump_chain.clear();
        assert_eq!(badge_of(TAB_CONNECT, Missing::default(), &buf), Badge::None);

        buf.jump_chain.push(SessionId(3));
        assert_eq!(
            badge_of(TAB_CONNECT, Missing::default(), &buf),
            Badge::Configured
        );
    }

    /// 其余三页各自的判据。
    #[test]
    fn other_pages_use_their_own_criteria() {
        let none = Missing::default();

        let auth = EditorBuffer {
            auth_kind: AuthKindUi::PublicKey,
            ..Default::default()
        };
        assert_eq!(badge_of(TAB_AUTH, none, &auth), Badge::Configured);

        let mut auto = EditorBuffer::default();
        auto.preserved_automation.enabled = Some(false);
        assert_eq!(badge_of(TAB_AUTOMATION, none, &auto), Badge::Configured);

        let mut look = EditorBuffer::default();
        look.preserved_appearance.color = Some(mullion_store::ColorSpec {
            hex: "#ff0000".into(),
            apply_to: vec![mullion_store::ColorTarget::ListItem],
        });
        assert_eq!(badge_of(TAB_APPEARANCE, none, &look), Badge::Configured);
    }

    /// F120:「SFTP」页的已配置判据 —— 两个默认目录或书签列表任一非空。
    /// 三种触发方式分开断言,避免只测出「或」链的一支就误判整个判据是对的。
    #[test]
    fn sftp_badge_fires_on_default_remote_default_local_or_bookmarks() {
        let none = Missing::default();
        assert_eq!(
            badge_of(TAB_SFTP, none, &EditorBuffer::default()),
            Badge::None,
            "空表单不该有角标"
        );

        let remote = EditorBuffer {
            sftp_default_remote: "/srv/app".into(),
            ..Default::default()
        };
        assert_eq!(badge_of(TAB_SFTP, none, &remote), Badge::Configured);

        let local = EditorBuffer {
            sftp_default_local: r"D:\work".into(),
            ..Default::default()
        };
        assert_eq!(badge_of(TAB_SFTP, none, &local), Badge::Configured);

        let mut marks = EditorBuffer::default();
        marks
            .sftp_bookmarks
            .push(("日志".into(), "/var/log".into()));
        assert_eq!(badge_of(TAB_SFTP, none, &marks), Badge::Configured);
    }

    /// 只填了空白字符不算配置过 —— 跟保存时 `build_draft` 的 trim 判据对齐,
    /// 否则会出现「角标说配置过,保存后又变回默认」的不一致。
    #[test]
    fn whitespace_only_sftp_fields_do_not_count_as_configured() {
        let none = Missing::default();
        let buf = EditorBuffer {
            sftp_default_remote: "   ".into(),
            sftp_default_local: "\t".into(),
            ..Default::default()
        };
        assert_eq!(badge_of(TAB_SFTP, none, &buf), Badge::None);
    }

    /// 书签这一支也得跟 `build_draft` 的过滤对齐:那边把**路径**纯空白的整条
    /// 丢掉,所以「点了一次『+ 添加书签』但什么都没填」不算配置过。
    ///
    /// 不对齐的症状很具体:点一下加号,角标立刻亮;保存 → 空书签被过滤掉 →
    /// 重开编辑器角标又灭了。用户会以为是保存丢了东西。
    #[test]
    fn a_blank_bookmark_row_does_not_count_as_configured() {
        let none = Missing::default();

        let mut blank = EditorBuffer::default();
        blank.sftp_bookmarks.push((String::new(), String::new()));
        assert_eq!(
            badge_of(TAB_SFTP, none, &blank),
            Badge::None,
            "刚点出来的空书签行不算配置过 —— 它保存时会被丢掉"
        );

        // 只填了名称、路径还空着,同样不算:没有路径的书签点了也去不了哪里,
        // `build_draft` 一样会丢。
        let mut named_only = EditorBuffer::default();
        named_only.sftp_bookmarks.push(("日志".into(), "  ".into()));
        assert_eq!(badge_of(TAB_SFTP, none, &named_only), Badge::None);

        // 混着来:有一条路径是真的,就算配置过。
        let mut mixed = EditorBuffer::default();
        mixed.sftp_bookmarks.push((String::new(), String::new()));
        mixed
            .sftp_bookmarks
            .push(("日志".into(), "/var/log".into()));
        assert_eq!(badge_of(TAB_SFTP, none, &mixed), Badge::Configured);
    }
}
