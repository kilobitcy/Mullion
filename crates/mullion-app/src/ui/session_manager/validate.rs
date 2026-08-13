//! 会话表单的必填项判定(F91)。**纯函数,零 egui、零 IO**——
//! 这里的分支全是「哪些字段为空 → 该禁哪个按钮 / 跳哪个 Tab」,
//! 放进 UI 就再也测不动了。

/// 缺哪些必填项。端口有默认值 22,不算必填。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Missing {
    pub name: bool,
    pub host: bool,
    pub user: bool,
    /// F74:选了「共享凭据」却还没挑具体是哪一份。
    ///
    /// 与 `user` 分开而不是复用它:两者**互斥**(共享档没有用户名可填),
    /// 合成一个的话提示文案只能写死一种,另一种模式下就指错地方 ——
    /// 「还缺:用户名」会让用户去找一个界面上根本不存在的输入框。
    pub credential: bool,
}

impl Missing {
    pub fn any(self) -> bool {
        self.name || self.host || self.user || self.credential
    }

    /// 第一个缺项所在的 Tab 索引(与 `UiState::editor_tab` 同义)。
    /// 「高级」页已并入「连接」页(走查 P1-8),必填项只落在这两页上,
    /// 所以本函数实际只会返回 `TAB_CONNECT`(名称/主机缺)、
    /// `TAB_AUTH`(用户名缺)或 `None`(都不缺)——不会返回其余下标。
    ///
    /// 用 `usize` 而非新枚举:`editor_tab: usize` 是既有技术债,
    /// 换 enum 会波及所有 Tab 相关代码,不在本切片范围内。
    pub fn tab(self) -> Option<usize> {
        if self.name || self.host {
            Some(super::TAB_CONNECT)
        } else if self.user || self.credential {
            Some(super::TAB_AUTH)
        } else {
            None
        }
    }

    /// 给按钮的 disabled tooltip 用,如「还缺:主机、用户名」。
    pub fn hint(self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if self.name {
            parts.push("会话名称");
        }
        if self.host {
            parts.push("主机");
        }
        if self.user {
            parts.push("用户名");
        }
        if self.credential {
            parts.push("共享凭据");
        }
        format!("还缺:{}", parts.join("、"))
    }
}

/// 哪些必填框已经被用户碰过(聚焦过又离开)。**只有碰过的框才配红字**——
/// 新建草稿一打开就三行全红,等于在骂用户「你还没填」,而他连第一个字都
/// 还没敲。碰过 = 用户认为自己填完了,这时候才该指出问题。
///
/// 放在 `UiState` 而不是 `EditorBuffer`:后者整体参与 `is_dirty` 比对,
/// 点进框里再点出来什么都没改也会被判成脏,切会话时白弹一次确认。
/// (`password_touched` 那几位留在 buffer 里是**有意的** —— 它们真的改变
/// 保存意图,见 `SecretField`。)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Touched {
    pub name: bool,
    pub host: bool,
    pub user: bool,
    pub port: bool,
}

/// 端口:**留空 = 默认 22**(标签上没有 `*`,它本来就不是必填项),
/// 其余必须落在 1~65535。
///
/// `0` 曾经能存进去:老代码直接 `parse::<u16>()`,`"0"` 是合法 `u16`,
/// 于是拨号时对着 0 端口连,报一句看不懂的系统错误。
pub fn port(s: &str) -> Result<u16, &'static str> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(22);
    }
    match s.parse::<u32>() {
        Ok(p) if (1..=65535).contains(&p) => Ok(p as u16),
        _ => Err("端口要填 1~65535 之间的数字"),
    }
}

/// 身份那一栏该按哪套判据来判(F74 设计 D10)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Identity {
    /// 本会话独有 → 判「用户名填了没」。
    Own,
    /// 引用共享凭据 → 判「挑了哪一份没」,用户名不参与
    /// (共享档界面上压根没有用户名输入框)。
    Shared { chosen: bool },
}

/// 判定用 `trim()`:一串空格既连不上也存不住,不能骗过校验。
pub fn check(name: &str, host: &str, user: &str, identity: Identity) -> Missing {
    Missing {
        name: name.trim().is_empty(),
        host: host.trim().is_empty(),
        user: matches!(identity, Identity::Own) && user.trim().is_empty(),
        credential: matches!(identity, Identity::Shared { chosen: false }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// F91:空白字符不算填了。用户在主机框里敲了个空格就以为填好了,
    /// 存进去连的是空主机名。
    ///
    /// 自证变红的方式:把 `check` 里的 `.trim()` 去掉。
    #[test]
    fn required_fields_reject_whitespace_only() {
        let m = check("  ", "\t", " \n ", Identity::Own);
        assert_eq!(
            m,
            Missing {
                name: true,
                host: true,
                user: true,
                credential: false,
            }
        );
        assert!(m.any());

        let ok = check("web01", "10.0.0.1", "root", Identity::Own);
        assert_eq!(ok, Missing::default());
        assert!(!ok.any());
        assert_eq!(ok.tab(), None);
    }

    /// F91:点了禁用的按钮要能被带到第一个缺项所在的 Tab。
    /// 用户名在「认证」Tab 上,不在「连接」Tab —— 跳错就等于没跳。
    ///
    /// 自证变红的方式:把 `tab()` 里 `Some(1)` 改成 `Some(0)`。
    #[test]
    fn missing_maps_to_first_offending_tab() {
        // 只缺用户名 → 认证 Tab(1)
        assert_eq!(check("web01", "10.0.0.1", "", Identity::Own).tab(), Some(1));
        // 缺主机(连接 Tab)优先于缺用户名(认证 Tab)
        assert_eq!(check("web01", "", "", Identity::Own).tab(), Some(0));
        // 只缺名称 → 连接 Tab(0)
        assert_eq!(check("", "10.0.0.1", "root", Identity::Own).tab(), Some(0));

        assert_eq!(
            check("web01", "", "", Identity::Own).hint(),
            "还缺:主机、用户名"
        );
        assert_eq!(
            check("", "10.0.0.1", "root", Identity::Own).hint(),
            "还缺:会话名称"
        );
    }

    /// F74:共享凭据档下,判据从「用户名填了没」换成「凭据挑了没」,
    /// 缺项仍落在「认证」Tab。
    ///
    /// 两处都容易错:①共享档界面上根本没有用户名输入框,若还判 `user`,
    /// 保存按钮永远灰着、提示写「还缺:用户名」,用户翻遍表单也找不到那个框;
    /// ②挑好了凭据却因为用户名为空仍判缺,等于共享凭据这个功能压根不能用。
    ///
    /// 自证变红的方式:把 `check` 里 `user` 那行的 `matches!(identity,
    /// Identity::Own) &&` 去掉。
    #[test]
    fn shared_identity_requires_a_chosen_credential_instead_of_a_user_name() {
        let none_chosen = check("web01", "10.0.0.1", "", Identity::Shared { chosen: false });
        assert_eq!(
            none_chosen,
            Missing {
                name: false,
                host: false,
                user: false,
                credential: true,
            },
            "共享档缺的是凭据,不是用户名"
        );
        assert_eq!(none_chosen.tab(), Some(super::super::TAB_AUTH));
        assert_eq!(none_chosen.hint(), "还缺:共享凭据");

        let chosen = check("web01", "10.0.0.1", "", Identity::Shared { chosen: true });
        assert_eq!(chosen, Missing::default(), "挑好了凭据就不该再拦");
        assert!(!chosen.any());
    }

    /// 走查 15:端口。留空落默认 22;`0` 和 65536 都不是能连的端口,
    /// 必须拦在保存之前 —— 老代码 `parse::<u16>()` 会把 `"0"` 当成合法值
    /// 存进去,拨号时才炸,那时候用户已经忘了自己填过什么。
    ///
    /// 自证会变红:把范围判断改回 `parse::<u16>()`,`"0"` 这条报「应拒绝」。
    #[test]
    fn port_defaults_to_22_when_blank_and_rejects_out_of_range() {
        assert_eq!(port(""), Ok(22));
        assert_eq!(port("   "), Ok(22));
        assert_eq!(port("22"), Ok(22));
        assert_eq!(port(" 2222 "), Ok(2222));
        assert_eq!(port("65535"), Ok(65535));

        assert!(port("0").is_err(), "0 不是能连的端口");
        assert!(port("65536").is_err());
        assert!(port("-1").is_err());
        assert!(port("22x").is_err());
        assert!(port("二十二").is_err());
    }
}
