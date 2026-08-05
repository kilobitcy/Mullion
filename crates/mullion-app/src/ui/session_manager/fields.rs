//! 右栏三个 Tab 的字段布局。从 `editor.rs` 切出来是因为字段多、改动频繁,
//! 混在窗口骨架里会让 `editor.rs` 涨到读不动。

use egui::Ui;
use mullion_store::{GroupRecord, Protocol};

use crate::theme::Theme;
use crate::ui::session_manager::{AuthKindUi, EditorBuffer, ProxyModeUi, SecretPresence};

/// 两列表单的统一样式:左列标签定宽,右列输入撑满。
fn grid(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    egui::Grid::new(id)
        .num_columns(2)
        .spacing([12.0, 10.0])
        .min_col_width(88.0)
        .show(ui, add);
}

/// 分区小标题。11px + fg_muted,上面留 10px —— 表单一路平铺下来
/// 没有任何视觉锚点,眼睛找不到「这几行是一组」。
fn section(ui: &mut Ui, t: &Theme, title: &str) {
    ui.add_space(10.0);
    ui.label(
        egui::RichText::new(title)
            .size(11.0)
            .color(crate::theme::c32(t.fg_muted)),
    );
    ui.add_space(4.0);
}

/// 必填项标签:名字后跟一个 danger 色的星号。
fn required(ui: &mut Ui, t: &Theme, text: &str) {
    ui.horizontal(|ui| {
        ui.label(text);
        ui.colored_label(crate::theme::c32(t.danger), "*");
    });
}

pub(super) fn basic(ui: &mut Ui, t: &Theme, buf: &mut EditorBuffer, groups: &[GroupRecord]) {
    section(ui, t, "基本");
    grid(ui, "sm_basic", |ui| {
        required(ui, t, "名称");
        ui.add(egui::TextEdit::singleline(&mut buf.name).desired_width(f32::INFINITY));
        ui.end_row();

        required(ui, t, "主机");
        ui.add(egui::TextEdit::singleline(&mut buf.host).desired_width(f32::INFINITY));
        ui.end_row();

        ui.label("端口");
        ui.add(egui::TextEdit::singleline(&mut buf.port).desired_width(80.0));
        ui.end_row();

        ui.label("协议");
        egui::ComboBox::from_id_salt("sm_protocol")
            .selected_text(match buf.protocol {
                Protocol::Ssh => "ssh",
                Protocol::Sftp => "sftp",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut buf.protocol, Protocol::Ssh, "ssh");
                ui.selectable_value(&mut buf.protocol, Protocol::Sftp, "sftp");
            });
        ui.end_row();
    });

    section(ui, t, "归类");
    grid(ui, "sm_basic_group", |ui| {
        ui.label("分组");
        let current = buf
            .preserved_group_id
            .and_then(|gid| groups.iter().find(|g| g.id == gid))
            .map(|g| g.name.clone())
            .unwrap_or_else(|| "未分组".to_string());
        egui::ComboBox::from_id_salt("sm_group")
            .selected_text(current)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut buf.preserved_group_id, None, "未分组");
                for g in groups {
                    ui.selectable_value(&mut buf.preserved_group_id, Some(g.id), &g.name);
                }
            });
        ui.end_row();

        ui.label("备注");
        ui.add(
            egui::TextEdit::multiline(&mut buf.note)
                .desired_rows(3)
                .desired_width(f32::INFINITY),
        );
        ui.end_row();
    });
}

pub(super) fn auth(
    ui: &mut Ui,
    t: &Theme,
    buf: &mut EditorBuffer,
    presence: SecretPresence,
    key_candidates: &[std::path::PathBuf],
) {
    section(ui, t, "身份");
    grid(ui, "sm_auth", |ui| {
        required(ui, t, "用户名");
        ui.add(egui::TextEdit::singleline(&mut buf.user).desired_width(f32::INFINITY));
        ui.end_row();

        ui.label("认证方式");
        ui.horizontal(|ui| {
            // theme.rs 全局默认已给选中态 35% accent 底(gamma_multiply),egui
            // interact_selectable() 又不画 bg_stroke(那行被注释掉了),只能靠底色
            // 分辨选中态。gamma_multiply 在 sRGB 空间直接缩四通道,深色面板上偏暗;
            // 换成 linear_multiply,同样标称 35% alpha,转线性空间缩放再转回后明显更亮。
            let vis = &mut ui.visuals_mut().selection;
            vis.bg_fill = crate::theme::c32(t.accent).linear_multiply(0.35);
            ui.selectable_value(&mut buf.auth_kind, AuthKindUi::Password, "密码");
            ui.selectable_value(&mut buf.auth_kind, AuthKindUi::PublicKey, "公钥");
        });
        ui.end_row();
    });

    section(ui, t, "凭据");
    grid(ui, "sm_auth_secret", |ui| match buf.auth_kind {
        AuthKindUi::Password => {
            ui.label("密码");
            super::secret_edit(
                ui,
                t,
                "sm_password",
                &mut buf.password,
                &mut buf.password_touched,
                presence.password,
                "未设置",
            );
            ui.end_row();
        }
        AuthKindUi::PublicKey => {
            ui.label("私钥");
            ui.horizontal(|ui| {
                // 三段 [输入框][候选▾][浏览…] 要在一行内伸缩,且不能引入
                // 硬编码的整行宽度(项目里没有 FIELD_W 这类常量)。用
                // right_to_left 布局:先摆两个按钮(它们的宽度由自身内容
                // 决定),摆完之后 `ui.available_width()` 就是右栏拖宽/
                // 缩窄后剩下的真实空间,喂给输入框——这样输入框自然吃满
                // 剩余宽度,不用猜一个像素数。
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // 「浏览…」原样保留:它已经接好原生文件对话框(另起线程
                    // 开 rfd,见 app.rs::spawn_key_picker),本任务只在它左边
                    // 插入候选下拉,不改这一按钮的行为。
                    if ui.button("浏览…").clicked() {
                        buf.pick_key_clicked = true;
                    }

                    key_candidate_combo(ui, key_candidates, &mut buf.key_path);

                    ui.add(
                        egui::TextEdit::singleline(&mut buf.key_path)
                            .desired_width(ui.available_width()),
                    );
                });
            });
            ui.end_row();

            ui.label("私钥口令");
            super::secret_edit(
                ui,
                t,
                "sm_passphrase",
                &mut buf.passphrase,
                &mut buf.passphrase_touched,
                presence.passphrase,
                "留空表示无口令",
            );
            ui.end_row();
        }
    });
}

/// 私钥候选下拉。抽成独立函数并**返回 `Response`**,是为了让守护测试能扎在
/// 真实生产代码上 —— 原先测试自己复制一份同构的 ComboBox 去断言,`auth()` 里
/// 的接线(`has_cand` 算反、`on_disabled_hover_text` 挂错 response、漏掉
/// `add_enabled_ui` 包装)坏掉时测试不会变红,等于没有保护。
pub(super) fn key_candidate_combo(
    ui: &mut Ui,
    key_candidates: &[std::path::PathBuf],
    key_path: &mut String,
) -> egui::Response {
    // 候选下拉。为空时禁用并说明原因——一个点了没反应的
    // 按钮比一个明说「没找到」的灰按钮更让人困惑。
    let has_cand = !key_candidates.is_empty();
    ui.add_enabled_ui(has_cand, |ui| {
        egui::ComboBox::from_id_salt("key_candidates")
            // 默认 combo_width(100.0)是给正常下拉配的,对一个只画内置箭头图标
            // 的按钮太宽;28.0 把 combo_width 下限降下来,让按钮不占多余空间——
            // 实际宽度取 `文字+图标间距+图标` 与 `width-2*padding` 的较大者
            // (egui-0.30.0 combo_box.rs:353,366),不是「固定尺寸」。
            .width(28.0)
            // 留空,不要填 "▾":ComboBox 按钮无条件会画一个内置向下三角图标
            // (combo_box.rs:373-383,`paint_default_icon`),跟 selected_text
            // 的文字是两处独立绘制、互不排斥。填 "▾" 会变成「文字 ▾ + 内置
            // 三角」两个箭头叠在一起,看起来像画重了。
            .selected_text("")
            .show_ui(ui, |ui| {
                for p in key_candidates {
                    let label = p
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| p.display().to_string());
                    if ui.selectable_label(false, label).clicked() {
                        *key_path = p.display().to_string();
                    }
                }
            });
    })
    .response
    .on_disabled_hover_text("未在 ~/.ssh 找到私钥")
}

pub(super) fn network(ui: &mut Ui, t: &Theme, buf: &mut EditorBuffer, presence: SecretPresence) {
    section(ui, t, "代理");
    grid(ui, "sm_net_proxy", |ui| {
        ui.label("代理");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut buf.proxy_mode, ProxyModeUi::Inherit, "继承分组");
            ui.selectable_value(&mut buf.proxy_mode, ProxyModeUi::Direct, "直连");
            ui.selectable_value(&mut buf.proxy_mode, ProxyModeUi::Socks5, "SOCKS5");
            ui.selectable_value(&mut buf.proxy_mode, ProxyModeUi::HttpConnect, "HTTP");
        });
        ui.end_row();

        if matches!(
            buf.proxy_mode,
            ProxyModeUi::Socks5 | ProxyModeUi::HttpConnect
        ) {
            ui.label("代理地址");
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut buf.proxy_host).desired_width(f32::INFINITY),
                );
                ui.add(egui::TextEdit::singleline(&mut buf.proxy_port).desired_width(80.0));
            });
            ui.end_row();

            ui.label("代理用户");
            ui.add(egui::TextEdit::singleline(&mut buf.proxy_user).desired_width(f32::INFINITY));
            ui.end_row();

            ui.label("代理口令");
            super::secret_edit(
                ui,
                t,
                "sm_proxy_password",
                &mut buf.proxy_password,
                &mut buf.proxy_password_touched,
                presence.proxy_password,
                "未设置",
            );
            ui.end_row();
        }
    });

    section(ui, t, "跳板");
    grid(ui, "sm_net_jump", |ui| {
        ui.label("跳板链");
        ui.vertical(|ui| {
            ui.checkbox(&mut buf.jump_set, "启用跳板");
            if buf.jump_set {
                ui.colored_label(
                    crate::theme::c32(t.fg_dimmer),
                    format!("已配置 {} 跳(在分组管理里编辑)", buf.jump_chain.len()),
                );
            }
        });
        ui.end_row();
    });
}

#[cfg(test)]
mod tests {
    use super::key_candidate_combo;

    /// F93 复核关切:候选为空时,私钥候选下拉必须走
    /// `ui.add_enabled_ui(false, ..).response.on_disabled_hover_text(..)`
    /// 才能让用户看到「未在 ~/.ssh 找到私钥」——这条路径成立的必要条件是
    /// `add_enabled_ui(false, ..)` 返回的 `response.enabled() == false`
    /// (`Response::on_disabled_hover_ui` 内部判据正是 `!self.enabled &&
    /// should_show_hover_ui()`,见 egui-0.30.0 `response.rs:557-568`)。
    ///
    /// 直接调用生产函数 `key_candidate_combo`(`auth()` 里私钥候选那段的
    /// 唯一实现),而不是在测试里另起一份同构代码——这样 `auth()` 的接线
    /// (`has_cand` 算反、`on_disabled_hover_text` 挂错 response、漏掉
    /// `add_enabled_ui` 包装)一旦坏掉,这条测试才会真的变红。
    ///
    /// 候选为空/非空各用一个独立的 `egui::Context` 各跑一次 `ctx.run`——同一
    /// 个 pass 里两次 `ComboBox::from_id_salt("key_candidates")` 会撞 id,
    /// 0.30 在 debug 下会画红色警告 / 触发 debug assert。
    ///
    /// 验证边界:tooltip 是否真的绘制出来,还依赖真实指针悬停 +
    /// `tooltip_delay` 帧推进,无头环境没有真实指针事件,没法进一步验证
    /// 「用户眼睛真的会看到这行字」。但 `response.enabled()` 是整条链路
    /// 成立的前提——前提不成立后面全部免谈;前提一旦成立,剩下的
    /// `should_show_hover_ui()`(本质是「鼠标在不在这个矩形上」)是 egui
    /// 自身的职责,不是本项目代码,没有必要在这里重新验证 egui 内部实现
    /// 是否正确。
    #[test]
    fn key_candidate_combo_enabled_state_tracks_whether_candidates_exist() {
        let mut key_path = String::new();

        let ctx_empty = egui::Context::default();
        let mut enabled_when_empty = true;
        let _ = ctx_empty.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let resp = key_candidate_combo(ui, &[], &mut key_path);
                enabled_when_empty = resp.enabled();
            });
        });
        assert!(
            !enabled_when_empty,
            "候选列表为空时 key_candidate_combo 返回的 response 必须 \
             enabled() == false,否则 on_disabled_hover_text 的判据 \
             `!self.enabled` 恒假,禁用提示永远不会弹出;实际 enabled() \
             == true"
        );

        let candidates = vec![std::path::PathBuf::from("/home/u/.ssh/id_ed25519")];
        let ctx_nonempty = egui::Context::default();
        let mut enabled_when_nonempty = false;
        let _ = ctx_nonempty.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let resp = key_candidate_combo(ui, &candidates, &mut key_path);
                enabled_when_nonempty = resp.enabled();
            });
        });
        assert!(
            enabled_when_nonempty,
            "候选列表非空时 key_candidate_combo 返回的 response 必须 \
             enabled() == true,否则用户面对一个找到了候选却点不动的灰按钮; \
             实际 enabled() == false"
        );
    }
}
