//! 继承解析(设计 §4.1)。全部纯函数,零 IO。
//!
//! **层序约定**:所有 `sources` 一律按**优先级从高到低**传入(会话在前、分组在后)。
//! `resolve_merge_list` 内部负责反转,以产出「上游在前」的结果顺序。

use crate::model::{AppearancePrefs, TerminalPrefs};

/// 滚动回溯的内置默认(F17,spec 写的 10000)。
pub const DEFAULT_SCROLLBACK: u32 = 10_000;

/// 标量继承:按优先级取第一个 `Some`;全 `None` 则用内置默认。
///
/// 复合对象(`IconSpec`/`ColorSpec`)也走这里——**整体覆盖**,
/// 不支持字段级部分覆盖(设计 §4.1 硬限制)。
pub fn resolve_override<T>(sources: impl IntoIterator<Item = Option<T>>, builtin: T) -> T {
    sources.into_iter().flatten().next().unwrap_or(builtin)
}

/// 列表继承:各层并集,保序去重。
///
/// 入参同样按**优先级从高到低**传入,但产出顺序为「上游在前」——
/// 分组的 `prod` 排在会话的 `web01` 之前,读起来符合「从大类到具体」的直觉。
pub fn resolve_merge_list(sources: impl IntoIterator<Item = Vec<String>>) -> Vec<String> {
    let layers: Vec<Vec<String>> = sources.into_iter().collect();
    let mut out: Vec<String> = Vec::new();
    for layer in layers.into_iter().rev() {
        for item in layer {
            if !out.contains(&item) {
                out.push(item);
            }
        }
    }
    out
}

/// 一层可继承偏好的来源。会话、分组都实现它;
/// 将来 F64「环境等级隐含默认」只需再实现一个类型并塞进 layers,
/// **`resolve` 的签名不变**(设计 §4.1)。
pub trait PrefsLayer {
    fn tags(&self) -> &[String];
    fn terminal(&self) -> &TerminalPrefs;
    fn appearance(&self) -> &AppearancePrefs;
}

impl PrefsLayer for crate::model::SessionRecord {
    fn tags(&self) -> &[String] {
        &self.identity.tags
    }
    fn terminal(&self) -> &TerminalPrefs {
        &self.terminal
    }
    fn appearance(&self) -> &AppearancePrefs {
        &self.appearance
    }
}

impl PrefsLayer for crate::group::GroupRecord {
    fn tags(&self) -> &[String] {
        &self.tags
    }
    fn terminal(&self) -> &TerminalPrefs {
        &self.terminal
    }
    fn appearance(&self) -> &AppearancePrefs {
        &self.appearance
    }
}

/// 继承解析后的最终配置。
///
/// 「无会话」时的默认值请调用 `resolve(&[])` 取,**不要给本结构派生 `Default`**——
/// 那会把 `scrollback` 默认成 0 而非 `DEFAULT_SCROLLBACK`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedConfig {
    pub scrollback: u32,
    pub tags: Vec<String>,
    pub icon: Option<crate::model::IconSpec>,
    pub color: Option<crate::model::ColorSpec>,
}

/// 沿 `layers`(优先级从高到低)解析出最终配置。
///
/// 调用方负责组装层序,当前为 `[会话, 分组]`;未分组时只传 `[会话]`。
///
/// 结果应由调用方缓存,**不要在渲染热路径 / 每帧里重新调用**(本项目陷阱 T3:
/// 喂数据和重绘没解耦 → 每秒几千次重绘,GPU 空转、风扇起飞)。
pub fn resolve(layers: &[&dyn PrefsLayer]) -> ResolvedConfig {
    ResolvedConfig {
        scrollback: resolve_override(
            layers.iter().map(|l| l.terminal().scrollback),
            DEFAULT_SCROLLBACK,
        ),
        tags: resolve_merge_list(layers.iter().map(|l| l.tags().to_vec())),
        // 复合对象没有「内置默认值」这个概念(builtin 只能是 None),
        // 而 resolve_override<T> 要求 builtin: T,所以额外套一层 `.map(Some)`,
        // 让 T = Option<IconSpec>/Option<ColorSpec>。
        // 关键机制:`Option::map` 遇 `None` 不执行闭包,该层在 flatten() 后贡献
        // **0 个元素**、被整体跳过,而不是贡献一个参与比较的 `None`——
        // 这样才能保留「本层未设则看下一层」的语义,而非「本层没有 → 直接判定全局无值」。
        // 前提:当前模型里「本层未设(继承)」和「本层显式设为空」被折叠成同一个 None,
        // 没有区分;将来若给某个复合字段加「显式清空、不再继承」的语义,这段 `.map(Some)`
        // 技巧不能直接照抄,需要额外一层标记。
        icon: resolve_override(
            layers.iter().map(|l| l.appearance().icon.clone().map(Some)),
            None,
        ),
        color: resolve_override(
            layers
                .iter()
                .map(|l| l.appearance().color.clone().map(Some)),
            None,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::group::GroupRecord;
    use crate::model::{
        AppearancePrefs, Auth, AuthKind, ColorSpec, ColorTarget, Connection, GroupId, IconKind,
        IconSpec, Identity, Protocol, SessionId, SessionRecord, TerminalPrefs,
    };

    fn session(
        tags: Vec<String>,
        terminal: TerminalPrefs,
        appearance: AppearancePrefs,
    ) -> SessionRecord {
        SessionRecord {
            id: SessionId(1),
            modified_at: "t".into(),
            identity: Identity {
                name: "s".into(),
                note: String::new(),
                group_id: Some(GroupId(1)),
                tags,
            },
            connection: Connection {
                host: "h".into(),
                port: 22,
                protocol: Protocol::Ssh,
            },
            auth: Auth {
                user: "u".into(),
                kind: AuthKind::Password,
            },
            terminal,
            appearance,
        }
    }

    fn group(
        tags: Vec<String>,
        terminal: TerminalPrefs,
        appearance: AppearancePrefs,
    ) -> GroupRecord {
        GroupRecord {
            id: GroupId(1),
            name: "g".into(),
            tags,
            terminal,
            appearance,
        }
    }

    #[test]
    fn resolve_uses_group_when_session_unset() {
        let s = session(
            vec![],
            TerminalPrefs { scrollback: None },
            AppearancePrefs::default(),
        );
        let g = group(
            vec![],
            TerminalPrefs {
                scrollback: Some(50_000),
            },
            AppearancePrefs::default(),
        );
        let got = resolve(&[&s, &g]);
        assert_eq!(got.scrollback, 50_000);
    }

    #[test]
    fn resolve_without_group_falls_back_to_builtin() {
        let s = session(
            vec![],
            TerminalPrefs { scrollback: None },
            AppearancePrefs::default(),
        );
        let got = resolve(&[&s]);
        assert_eq!(got.scrollback, DEFAULT_SCROLLBACK, "未分组会话应取内置默认");
    }

    #[test]
    fn resolve_merges_tags_from_both_layers() {
        let s = session(
            vec!["web01".into()],
            TerminalPrefs::default(),
            AppearancePrefs::default(),
        );
        let g = group(
            vec!["prod".into()],
            TerminalPrefs::default(),
            AppearancePrefs::default(),
        );
        let got = resolve(&[&s, &g]);
        assert_eq!(got.tags, vec!["prod".to_string(), "web01".to_string()]);
    }

    /// 钉死设计 §4.1 的硬限制:复合对象整体覆盖,不做字段级合并。
    #[test]
    fn composite_color_is_replaced_wholesale_never_field_merged() {
        let s = session(
            vec![],
            TerminalPrefs::default(),
            AppearancePrefs {
                icon: None,
                color: Some(ColorSpec {
                    hex: "#111111".into(),
                    apply_to: vec![],
                }),
            },
        );
        let g = group(
            vec![],
            TerminalPrefs::default(),
            AppearancePrefs {
                icon: Some(IconSpec {
                    kind: IconKind::Builtin,
                    value: "ubuntu".into(),
                }),
                color: Some(ColorSpec {
                    hex: "#E5484D".into(),
                    apply_to: vec![ColorTarget::Tab, ColorTarget::StatusBar],
                }),
            },
        );
        let got = resolve(&[&s, &g]);

        let color = got.color.expect("应有颜色");
        assert_eq!(color.hex, "#111111", "会话的 hex 胜出");
        assert!(
            color.apply_to.is_empty(),
            "整体覆盖:会话的空 apply_to 必须原样保留,绝不能合并分组的 apply_to"
        );
        // icon 会话未设 → 整体取分组的
        assert_eq!(got.icon.unwrap().value, "ubuntu");
    }

    #[test]
    fn override_takes_highest_priority_some() {
        let got = resolve_override([Some(5u32), Some(9), None], DEFAULT_SCROLLBACK);
        assert_eq!(got, 5, "会话层(最高优先级)应胜出");
    }

    #[test]
    fn override_falls_through_none_to_next_layer() {
        let got = resolve_override([None, Some(9u32)], DEFAULT_SCROLLBACK);
        assert_eq!(got, 9, "会话未设时应取分组值");
    }

    #[test]
    fn override_falls_back_to_builtin_when_all_none() {
        let got = resolve_override([None, None::<u32>], DEFAULT_SCROLLBACK);
        assert_eq!(got, DEFAULT_SCROLLBACK, "全未设时应取内置默认");
    }

    #[test]
    fn override_supports_more_than_two_layers() {
        // 为 F64「环境等级隐含默认」预留:插入第三层不需要改签名。
        let got = resolve_override([None, None, Some(7u32)], DEFAULT_SCROLLBACK);
        assert_eq!(got, 7);
    }

    #[test]
    fn merge_puts_upstream_first_and_dedups() {
        // 入参按优先级从高到低:会话在前、分组在后。
        let got = resolve_merge_list([
            vec!["web01".to_string(), "prod".to_string()],
            vec!["prod".to_string(), "华东".to_string()],
        ]);
        assert_eq!(
            got,
            vec!["prod".to_string(), "华东".to_string(), "web01".to_string()],
            "结果应「上游在前」,且重复项只保留一次"
        );
    }

    #[test]
    fn merge_of_empty_layers_is_empty() {
        let got = resolve_merge_list(Vec::<Vec<String>>::new());
        assert!(got.is_empty());
    }

    #[test]
    fn merge_preserves_order_within_a_layer() {
        let got = resolve_merge_list([vec!["b".to_string(), "a".to_string()]]);
        assert_eq!(
            got,
            vec!["b".to_string(), "a".to_string()],
            "层内顺序不排序"
        );
    }
}
