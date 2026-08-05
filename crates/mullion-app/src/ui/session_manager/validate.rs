//! 会话表单的必填项判定(F91)。**纯函数,零 egui、零 IO**——
//! 这里的分支全是「哪些字段为空 → 该禁哪个按钮 / 跳哪个 Tab」,
//! 放进 UI 就再也测不动了。

/// 缺哪些必填项。端口有默认值 22,不算必填。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Missing {
    pub name: bool,
    pub host: bool,
    pub user: bool,
}

impl Missing {
    pub fn any(self) -> bool {
        self.name || self.host || self.user
    }

    /// 第一个缺项所在的 Tab 索引(与 `UiState::editor_tab` 同义:
    /// 0 连接 / 1 认证 / 2 高级)。
    ///
    /// 用 `usize` 而非新枚举:`editor_tab: usize` 是既有技术债,
    /// 换 enum 会波及所有 Tab 相关代码,不在本切片范围内。
    pub fn tab(self) -> Option<usize> {
        if self.name || self.host {
            Some(super::TAB_CONNECT)
        } else if self.user {
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
        format!("还缺:{}", parts.join("、"))
    }
}

/// 判定用 `trim()`:一串空格既连不上也存不住,不能骗过校验。
pub fn check(name: &str, host: &str, user: &str) -> Missing {
    Missing {
        name: name.trim().is_empty(),
        host: host.trim().is_empty(),
        user: user.trim().is_empty(),
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
        let m = check("  ", "\t", " \n ");
        assert_eq!(
            m,
            Missing {
                name: true,
                host: true,
                user: true
            }
        );
        assert!(m.any());

        let ok = check("web01", "10.0.0.1", "root");
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
        assert_eq!(check("web01", "10.0.0.1", "").tab(), Some(1));
        // 缺主机(连接 Tab)优先于缺用户名(认证 Tab)
        assert_eq!(check("web01", "", "").tab(), Some(0));
        // 只缺名称 → 连接 Tab(0)
        assert_eq!(check("", "10.0.0.1", "root").tab(), Some(0));

        assert_eq!(check("web01", "", "").hint(), "还缺:主机、用户名");
        assert_eq!(check("", "10.0.0.1", "root").hint(), "还缺:会话名称");
    }
}
