//! 会话管理弹窗:列表 + CRUD + 编辑表单(Task 6,§4.3/§1.2)。
//!
//! 关键约束:这里渲染在 `app.rs` 的 `egui_ctx.run(|ctx| ...)` 闭包内,只能拿到
//! `&mut UiState`,拿不到 `&mut SessionStore`(否则借用检查器过不了)。所以任何会
//! 改 store / 发起连接的动作,这里只写「意图」到 `UiState`,由 `app.rs` 在
//! `render_frame` 返回、借用释放之后统一施加——与既有 `request_disconnect`/
//! `request_quit` 完全同构。

mod buffer;

pub(crate) use buffer::{build_draft, AuthKindUi, ProxyModeUi};
pub use buffer::{EditorBuffer, SaveIntent};

use mullion_store::{GroupId, GroupRecord, Protocol, SessionRecord};

use super::UiState;

/// 每个分组桶对应的 `CollapsingHeader` 构造。抽成独立函数**只为了能在测试里
/// 直接调它**:`CollapsingHeader::new` 默认把标题文本本身当 id 源(见 egui 0.30
/// `collapsing_header.rs`)。两个分组恰好同名(且当前会话数也一样,标题完全
/// 一致)时会撞 id、共享同一份展开/收起状态——编译不报错,跑起来才会看到
/// "点开 A,B 也跟着变"。`.id_salt(gid)` 用分组主键(`None`=未分组桶)强制
/// 区分,彻底绕开标题文本。守护测试
/// `collapsing_header_id_salt_disambiguates_same_titled_groups` 直接调这个函数
/// (不是重抄一遍表达式),删掉 `.id_salt(gid)` 这行测试就会红。
fn group_header(title: &str, gid: Option<GroupId>, count: usize) -> egui::CollapsingHeader {
    egui::CollapsingHeader::new(format!("{title}({count})"))
        .id_salt(gid)
        .default_open(true)
}

/// 会话管理器弹窗:列表 + CRUD 按钮(+ 内嵌二次确认)。`store_available=false`
/// 时(待定 G:keyring/库打开失败)不崩,只展示 `last_error` 或兜底提示。
pub fn show(
    ctx: &egui::Context,
    ui_state: &mut UiState,
    sessions: &[SessionRecord],
    groups: &[GroupRecord],
    store_available: bool,
) {
    let mut open = ui_state.session_manager_open;
    egui::Window::new("会话管理器")
        .open(&mut open)
        .default_width(560.0)
        .show(ctx, |ui| {
            if !store_available {
                let msg = ui_state
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "会话功能不可用".to_string());
                ui.colored_label(egui::Color32::RED, msg);
                return;
            }

            for (gid, bucket) in crate::ui::group_manager::group_sessions(groups, sessions) {
                let title = match gid {
                    Some(id) => groups
                        .iter()
                        .find(|g| g.id == id)
                        .map(|g| g.name.clone())
                        .unwrap_or_else(|| "未分组".to_string()),
                    None => "未分组".to_string(),
                };
                group_header(&title, gid, bucket.len()).show(ui, |ui| {
                    egui::Grid::new(format!("session_list_grid_{gid:?}"))
                        .num_columns(5)
                        .striped(true)
                        .show(ui, |ui| {
                            ui.strong("名称");
                            ui.strong("主机:端口");
                            ui.strong("协议");
                            ui.strong("用户");
                            ui.strong("修改时间");
                            ui.end_row();
                            for rec in &bucket {
                                let is_selected = ui_state.selected == Some(rec.id);
                                let name_resp =
                                    ui.selectable_label(is_selected, &rec.identity.name);
                                if name_resp.clicked() {
                                    ui_state.selected = Some(rec.id);
                                }
                                if name_resp.double_clicked() {
                                    ui_state.connect_request = Some(rec.id);
                                }
                                ui.label(format!(
                                    "{}:{}",
                                    rec.connection.host, rec.connection.port
                                ));
                                ui.label(match rec.connection.protocol {
                                    Protocol::Ssh => "ssh",
                                    Protocol::Sftp => "sftp",
                                });
                                ui.label(&rec.auth.user);
                                ui.label(&rec.modified_at);
                                ui.end_row();
                            }
                        });
                });
            }

            ui.separator();
            // 双击/点连接失败(如 sftp 会话映射拒绝)、保存/删除失败都写
            // ui_state.last_error;这里必须总是渲染,否则「点了没反应」(复核 #2)。
            if let Some(err) = &ui_state.last_error {
                ui.colored_label(egui::Color32::RED, err);
            }
            ui.horizontal(|ui| {
                if ui.button("新建").clicked() {
                    ui_state.editor_id = None;
                    ui_state.editor = EditorBuffer::default();
                    ui_state.editor_open = true;
                }
                let has_selection = ui_state.selected.is_some();
                if ui
                    .add_enabled(has_selection, egui::Button::new("编辑"))
                    .clicked()
                {
                    if let Some(rec) = ui_state
                        .selected
                        .and_then(|id| sessions.iter().find(|s| s.id == id))
                    {
                        ui_state.editor_id = Some(rec.id);
                        ui_state.editor = EditorBuffer::from_record(rec);
                        ui_state.editor_open = true;
                    }
                }
                if ui
                    .add_enabled(has_selection, egui::Button::new("删除"))
                    .clicked()
                {
                    ui_state.pending_delete = ui_state.selected;
                }
                if ui
                    .add_enabled(has_selection, egui::Button::new("连接"))
                    .clicked()
                {
                    ui_state.connect_request = ui_state.selected;
                }
            });

            // 删除二次确认:内嵌一个独立小窗口,确认/取消都不直接碰 store。
            if let Some(id) = ui_state.pending_delete {
                let name = sessions
                    .iter()
                    .find(|s| s.id == id)
                    .map(|s| s.identity.name.clone())
                    .unwrap_or_default();
                egui::Window::new("确认删除")
                    .collapsible(false)
                    .resizable(false)
                    .show(ui.ctx(), |ui| {
                        ui.label(format!("确定删除会话「{name}」?此操作不可撤销。"));
                        ui.horizontal(|ui| {
                            if ui.button("确认删除").clicked() {
                                ui_state.delete_request = Some(id);
                                ui_state.pending_delete = None;
                                if ui_state.selected == Some(id) {
                                    ui_state.selected = None;
                                }
                            }
                            if ui.button("取消").clicked() {
                                ui_state.pending_delete = None;
                            }
                        });
                    });
            }
        });
    ui_state.session_manager_open = open;
    if !open {
        // 列表主窗被关掉:编辑子表单不该变成没有父窗的孤儿窗(复核 #5)。
        ui_state.editor_open = false;
    }

    if ui_state.editor_open {
        show_editor(ctx, ui_state, sessions, groups);
    }
}

/// 新建/编辑子表单。保存时把缓冲组装成 `SaveIntent` 写进 `ui_state.save_request`,
/// 不在这里直接碰 store。
fn show_editor(
    ctx: &egui::Context,
    ui_state: &mut UiState,
    sessions: &[SessionRecord],
    groups: &[GroupRecord],
) {
    let editing_id = ui_state.editor_id;
    let mut open = ui_state.editor_open;
    let title = if ui_state.editor_id.is_some() {
        "编辑会话"
    } else {
        "新建会话"
    };
    egui::Window::new(title)
        .open(&mut open)
        .collapsible(false)
        .show(ctx, |ui| {
            // 在 `buf` 的可变借用块里碰不到 ui_state,先记在局部,出块后再写意图。
            let mut pick_key = false;
            {
                let buf = &mut ui_state.editor;
                egui::Grid::new("editor_form_grid")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("名称");
                        ui.text_edit_singleline(&mut buf.name);
                        ui.end_row();

                        ui.label("主机");
                        ui.text_edit_singleline(&mut buf.host);
                        ui.end_row();

                        ui.label("端口");
                        ui.text_edit_singleline(&mut buf.port);
                        ui.end_row();

                        ui.label("协议");
                        egui::ComboBox::from_id_salt("session_editor_protocol")
                            .selected_text(match buf.protocol {
                                Protocol::Ssh => "ssh",
                                Protocol::Sftp => "sftp",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut buf.protocol, Protocol::Ssh, "ssh");
                                ui.selectable_value(&mut buf.protocol, Protocol::Sftp, "sftp");
                            });
                        ui.end_row();

                        ui.label("用户名");
                        ui.text_edit_singleline(&mut buf.user);
                        ui.end_row();

                        ui.label("备注");
                        ui.text_edit_singleline(&mut buf.note);
                        ui.end_row();

                        ui.label("分组");
                        egui::ComboBox::from_id_salt("editor_group")
                            .selected_text(
                                buf.preserved_group_id
                                    .and_then(|id| groups.iter().find(|g| g.id == id))
                                    .map(|g| g.name.as_str())
                                    .unwrap_or("未分组"),
                            )
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut buf.preserved_group_id, None, "未分组");
                                for g in groups {
                                    ui.selectable_value(
                                        &mut buf.preserved_group_id,
                                        Some(g.id),
                                        &g.name,
                                    );
                                }
                            });
                        ui.end_row();

                        ui.label("认证方式");
                        egui::ComboBox::from_id_salt("session_editor_auth_kind")
                            .selected_text(match buf.auth_kind {
                                AuthKindUi::Password => "密码",
                                AuthKindUi::PublicKey => "公钥",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut buf.auth_kind,
                                    AuthKindUi::Password,
                                    "密码",
                                );
                                ui.selectable_value(
                                    &mut buf.auth_kind,
                                    AuthKindUi::PublicKey,
                                    "公钥",
                                );
                            });
                        ui.end_row();

                        match buf.auth_kind {
                            AuthKindUi::Password => {
                                ui.label("密码");
                                ui.add(
                                    egui::TextEdit::singleline(&mut buf.password).password(true),
                                );
                                ui.end_row();
                            }
                            AuthKindUi::PublicKey => {
                                ui.label("私钥路径");
                                ui.horizontal(|ui| {
                                    ui.text_edit_singleline(&mut buf.key_path);
                                    if ui.button("选择…").clicked() {
                                        pick_key = true;
                                    }
                                });
                                ui.end_row();

                                ui.label("私钥口令");
                                ui.add(
                                    egui::TextEdit::singleline(&mut buf.passphrase).password(true),
                                );
                                ui.end_row();
                            }
                        }

                        ui.label("代理");
                        egui::ComboBox::from_id_salt("editor_proxy_mode")
                            .selected_text(match buf.proxy_mode {
                                ProxyModeUi::Inherit => "跟随分组",
                                ProxyModeUi::Direct => "不使用代理",
                                ProxyModeUi::Socks5 => "SOCKS5",
                                ProxyModeUi::HttpConnect => "HTTP CONNECT",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut buf.proxy_mode,
                                    ProxyModeUi::Inherit,
                                    "跟随分组",
                                );
                                ui.selectable_value(
                                    &mut buf.proxy_mode,
                                    ProxyModeUi::Direct,
                                    "不使用代理",
                                );
                                ui.selectable_value(
                                    &mut buf.proxy_mode,
                                    ProxyModeUi::Socks5,
                                    "SOCKS5",
                                );
                                ui.selectable_value(
                                    &mut buf.proxy_mode,
                                    ProxyModeUi::HttpConnect,
                                    "HTTP CONNECT",
                                );
                            });
                        ui.end_row();

                        if matches!(
                            buf.proxy_mode,
                            ProxyModeUi::Socks5 | ProxyModeUi::HttpConnect
                        ) {
                            ui.label("代理地址");
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut buf.proxy_host)
                                        .desired_width(160.0),
                                );
                                ui.label(":");
                                ui.add(
                                    egui::TextEdit::singleline(&mut buf.proxy_port)
                                        .desired_width(60.0),
                                );
                            });
                            ui.end_row();

                            ui.label("代理用户名");
                            ui.text_edit_singleline(&mut buf.proxy_user);
                            ui.end_row();

                            ui.label("代理口令");
                            ui.add(
                                egui::TextEdit::singleline(&mut buf.proxy_password).password(true),
                            );
                            ui.end_row();
                        }

                        ui.label("跳板");
                        ui.vertical(|ui| {
                            if !buf.jump_set {
                                ui.horizontal(|ui| {
                                    ui.label("跟随分组");
                                    if ui.button("改为自定义").clicked() {
                                        buf.jump_set = true;
                                    }
                                });
                            } else {
                                let mut remove_at = None;
                                for (i, id) in buf.jump_chain.iter().enumerate() {
                                    ui.horizontal(|ui| {
                                        let name = sessions
                                            .iter()
                                            .find(|r| r.id == *id)
                                            .map(|r| r.identity.name.clone())
                                            // 悬空引用在 UI 上就点出来,不要等到连接时才报错。
                                            .unwrap_or_else(|| format!("<已删除的会话 {:?}>", id));
                                        ui.label(format!("{}. {name}", i + 1));
                                        if ui.button("移除").clicked() {
                                            remove_at = Some(i);
                                        }
                                    });
                                }
                                if let Some(i) = remove_at {
                                    buf.jump_chain.remove(i);
                                }
                                egui::ComboBox::from_id_salt("editor_jump_add")
                                    .selected_text("添加跳板…")
                                    .show_ui(ui, |ui| {
                                        for rec in sessions {
                                            // 不能把自己当自己的跳板(那是环)。
                                            if Some(rec.id) == editing_id {
                                                continue;
                                            }
                                            if ui.button(&rec.identity.name).clicked() {
                                                buf.jump_chain.push(rec.id);
                                            }
                                        }
                                    });
                                if ui.button("恢复为跟随分组").clicked() {
                                    buf.jump_set = false;
                                    buf.jump_chain.clear();
                                }
                            }
                        });
                        ui.end_row();
                    });
            }
            if pick_key {
                ui_state.pick_key_request = true;
            }
            ui.label("留空密码 / 私钥口令 = 清除已存凭据(不是「保持不变」)。");

            if let Some(err) = &ui_state.last_error {
                ui.colored_label(egui::Color32::RED, err);
            }

            ui.horizontal(|ui| {
                if ui.button("保存").clicked() {
                    match build_draft(&ui_state.editor) {
                        Ok(draft) => {
                            ui_state.last_error = None;
                            ui_state.save_request = Some(SaveIntent {
                                editing_id: ui_state.editor_id,
                                draft,
                            });
                            ui_state.editor_open = false;
                            // 别让刚输入的明文密码/口令原样滞留在 UiState 内存里(复核 #6)。
                            ui_state.editor = EditorBuffer::default();
                        }
                        Err(e) => ui_state.last_error = Some(e),
                    }
                }
                if ui.button("取消").clicked() {
                    ui_state.editor_open = false;
                    ui_state.editor = EditorBuffer::default();
                }
            });
        });
    ui_state.editor_open = ui_state.editor_open && open;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 复审坑:`egui::CollapsingHeader::new(text)` 默认把标题文本本身当 id 源
    /// (`egui-0.30.0/src/containers/collapsing_header.rs::new`)。两个分组
    /// 名字相同、桶内会话数也相同时,列表里两个 header 的标题文本会完全一致
    /// ——不加 `.id_salt` 就会撞 id、共享同一份展开/收起状态,点开一个另一个
    /// 也跟着变。这条测试直接调 `group_header`(`show()` 内部实际用的同一个
    /// 函数,不是重抄一遍表达式),同一个父 `ui`、相同标题、不同 `gid`:
    /// 去掉 `group_header` 里的 `.id_salt(gid)` 这行,两个 `header_response.id`
    /// 会相等,下面的 `assert_ne!` 就会失败(已实测确认,见提交说明)。
    #[test]
    fn collapsing_header_id_salt_disambiguates_same_titled_groups() {
        let ctx = egui::Context::default();
        let mut ids: Option<(egui::Id, egui::Id)> = None;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let resp_a = group_header("生产", Some(GroupId(1)), 1).show(ui, |_| {});
                let resp_b = group_header("生产", Some(GroupId(2)), 1).show(ui, |_| {});
                ids = Some((resp_a.header_response.id, resp_b.header_response.id));
            });
        });
        let (id_a, id_b) = ids.expect("闭包必须跑到底,写回 ids");
        assert_ne!(
            id_a, id_b,
            "两个分组标题相同时,header 的持久化 id 必须靠 gid 区分,否则展开状态会互相串"
        );
    }
}
