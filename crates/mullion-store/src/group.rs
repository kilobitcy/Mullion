//! 单层分组(F60)。分组只持有**可继承**字段(设计 §4.2):
//! tags / terminal / appearance。连接目标与凭据永不进分组。

use serde::{Deserialize, Serialize};

use crate::model::{AppearancePrefs, GroupId, TerminalPrefs};

/// 一个分组。不嵌套——单层结构(设计 D1)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupRecord {
    pub id: GroupId,
    pub name: String,
    /// 继承策略 Merge。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default)]
    pub terminal: TerminalPrefs,
    #[serde(default)]
    pub appearance: AppearancePrefs,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ColorSpec, ColorTarget};

    #[test]
    fn group_round_trips() {
        let g = GroupRecord {
            id: GroupId(3),
            name: "生产".into(),
            tags: vec!["prod".into()],
            terminal: TerminalPrefs {
                scrollback: Some(50_000),
            },
            appearance: AppearancePrefs {
                icon: None,
                color: Some(ColorSpec {
                    hex: "#E5484D".into(),
                    apply_to: vec![ColorTarget::Tab],
                }),
            },
        };
        let s = toml::to_string_pretty(&g).unwrap();
        let back: GroupRecord = toml::from_str(&s).unwrap();
        assert_eq!(back, g);
    }
}
