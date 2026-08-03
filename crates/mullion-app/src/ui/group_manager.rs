//! F60:极简分组管理弹窗(新建 / 改名 / 删除)+ 会话列表的分组归集。
//!
//! 与 `session_manager` 同构:UI 只写「意图」到 `UiState`,由 `app.rs` 在借用释放后施加。

use mullion_store::{GroupId, GroupRecord, SessionRecord};

/// 一次分组操作的意图。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupIntent {
    Add(String),
    Rename(GroupId, String),
    Delete(GroupId),
}

/// 把会话按分组归集。返回 `(分组, 该组会话)`,分组顺序跟随 `groups`,
/// 未分组(含 group_id 悬空)的会话归入末尾的 `None` 桶。空桶不返回。
pub fn group_sessions<'a>(
    groups: &[GroupRecord],
    sessions: &'a [SessionRecord],
) -> Vec<(Option<GroupId>, Vec<&'a SessionRecord>)> {
    let mut out: Vec<(Option<GroupId>, Vec<&SessionRecord>)> = Vec::new();
    for g in groups {
        let bucket: Vec<&SessionRecord> = sessions
            .iter()
            .filter(|s| s.identity.group_id == Some(g.id))
            .collect();
        if !bucket.is_empty() {
            out.push((Some(g.id), bucket));
        }
    }
    // 悬空 group_id 也落这里:分组被删后会话不能从列表里消失。
    let known: Vec<GroupId> = groups.iter().map(|g| g.id).collect();
    let orphans: Vec<&SessionRecord> = sessions
        .iter()
        .filter(|s| match s.identity.group_id {
            None => true,
            Some(g) => !known.contains(&g),
        })
        .collect();
    if !orphans.is_empty() {
        out.push((None, orphans));
    }
    out
}

/// 分组管理弹窗。只写意图,不碰 store。
pub fn show(ctx: &egui::Context, ui_state: &mut crate::ui::UiState, groups: &[GroupRecord]) {
    let mut open = ui_state.group_manager_open;
    egui::Window::new("分组管理")
        .open(&mut open)
        .resizable(false)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("新建分组");
                ui.text_edit_singleline(&mut ui_state.group_name_buf);
                let name = ui_state.group_name_buf.trim().to_string();
                if ui
                    .add_enabled(!name.is_empty(), egui::Button::new("添加"))
                    .clicked()
                {
                    ui_state.group_intent = Some(GroupIntent::Add(name));
                    ui_state.group_name_buf.clear();
                }
            });
            ui.separator();
            for g in groups {
                ui.horizontal(|ui| {
                    ui.label(&g.name);
                    if ui.button("删除").clicked() {
                        ui_state.group_intent = Some(GroupIntent::Delete(g.id));
                    }
                });
            }
            if groups.is_empty() {
                ui.label("还没有分组。分组用来给一批会话共享代理、跳板与终端偏好。");
            }
        });
    ui_state.group_manager_open = open;
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_store::{Auth, AuthKind, Connection, Identity, NetworkPrefs, Protocol, SessionId};

    fn rec(id: u64, name: &str, group: Option<u64>) -> SessionRecord {
        SessionRecord {
            id: SessionId(id),
            modified_at: "t".into(),
            identity: Identity {
                name: name.into(),
                note: String::new(),
                group_id: group.map(GroupId),
                tags: Vec::new(),
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
            terminal: Default::default(),
            appearance: Default::default(),
            network: NetworkPrefs::default(),
        }
    }

    fn grp(id: u64, name: &str) -> GroupRecord {
        GroupRecord {
            id: GroupId(id),
            name: name.into(),
            tags: Vec::new(),
            terminal: Default::default(),
            appearance: Default::default(),
            network: NetworkPrefs::default(),
        }
    }

    #[test]
    fn sessions_are_bucketed_by_group_in_group_order() {
        let groups = vec![grp(1, "生产"), grp(2, "测试")];
        let sessions = vec![rec(10, "a", Some(2)), rec(11, "b", Some(1))];
        let got = group_sessions(&groups, &sessions);
        assert_eq!(got.len(), 2, "只该有两个非空桶");
        assert_eq!(got[0].0, Some(GroupId(1)), "桶序跟随分组顺序");
        assert_eq!(got[0].1[0].identity.name, "b");
        assert_eq!(got[1].0, Some(GroupId(2)));
    }

    /// 未分组的会话必须仍然可见,且排在最后——否则用户会以为会话丢了。
    #[test]
    fn ungrouped_sessions_go_to_a_trailing_bucket() {
        let groups = vec![grp(1, "生产")];
        let sessions = vec![rec(10, "a", None), rec(11, "b", Some(1))];
        let got = group_sessions(&groups, &sessions);
        assert_eq!(got.len(), 2);
        assert_eq!(got[1].0, None, "未分组桶排最后");
        assert_eq!(got[1].1[0].identity.name, "a");
    }

    /// 悬空 group_id(分组被删)不能让会话消失。P0-a 的 `resolve_for` 对此静默降级,
    /// 列表也必须跟着降级到「未分组」而不是漏掉这一条。
    #[test]
    fn session_with_dangling_group_id_falls_into_ungrouped_not_dropped() {
        let groups = vec![grp(1, "生产")];
        let sessions = vec![rec(10, "orphan", Some(99))];
        let got = group_sessions(&groups, &sessions);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, None);
        assert_eq!(got[0].1[0].identity.name, "orphan");
    }

    #[test]
    fn empty_groups_produce_no_buckets() {
        let groups = vec![grp(1, "空组")];
        let got = group_sessions(&groups, &[]);
        assert!(got.is_empty(), "没有会话就不该渲染任何桶");
    }

    /// 同一桶内多条会话必须保持 `sessions` 入参的原始顺序——`group_sessions`
    /// 内部按 `sessions.iter().filter(..)` 实现,天然稳定,但之前的用例每桶只放
    /// 1 条,没能覆盖「桶内排序」这个维度。
    #[test]
    fn sessions_within_the_same_bucket_keep_input_order() {
        let groups = vec![grp(1, "生产")];
        let sessions = vec![
            rec(10, "c", Some(1)),
            rec(11, "a", Some(1)),
            rec(12, "b", Some(1)),
        ];
        let got = group_sessions(&groups, &sessions);
        assert_eq!(got.len(), 1);
        let names: Vec<&str> = got[0].1.iter().map(|s| s.identity.name.as_str()).collect();
        assert_eq!(names, vec!["c", "a", "b"], "桶内顺序必须跟随传入的会话顺序");
    }
}
