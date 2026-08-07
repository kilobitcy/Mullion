//! 右栏五个 Tab 的字段布局。从 `editor.rs` 切出来是因为字段多、改动频繁,
//! 混在窗口骨架里会让 `editor.rs` 涨到读不动。

use egui::Ui;
use mullion_store::{GroupRecord, Protocol, SessionId, SessionRecord, TmuxChoice};

use crate::theme::Theme;
use crate::ui::session_manager::{
    AuthKindUi, EditorBuffer, JumpModeUi, ProxyModeUi, SecretPresence,
};

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

/// 三态下拉:继承(`None`)/ 开 / 关。
///
/// 「继承」与「显式关闭」必须可区分(同 `ProxyModeUi` 那个坑):合并二者会让
/// 用户无法在分组开了自动化时单独关掉某一条会话。`Option<bool>` 类型自身就
/// 表达了三态,不需要再造一个 `*Ui` 枚举。
///
/// 返回 `ComboBox` 按钮自身的 `Response`(同 `key_candidate_combo` 的理由):
/// 让守护测试能拿到精确矩形去点开下拉,不用在测试里猜像素坐标。
fn tri_state(ui: &mut Ui, id: &str, v: &mut Option<bool>, on: &str, off: &str) -> egui::Response {
    let text = match *v {
        None => "继承",
        Some(true) => on,
        Some(false) => off,
    };
    egui::ComboBox::from_id_salt(id)
        .selected_text(text)
        .show_ui(ui, |ui| {
            ui.selectable_value(v, None, "继承");
            ui.selectable_value(v, Some(true), on);
            ui.selectable_value(v, Some(false), off);
        })
        .response
}

/// 可选毫秒数:勾选框 + `DragValue`。未勾选时显示内置默认值作提示 ——
/// 光给一个空框,用户不知道不填会发生什么。
///
/// `min` 不是摆设:两个「延时」类字段填 0 就是「不等」,语义正常;而「就绪超时」
/// 填 0 意味着 `run()` 的 `sleep(0)` 必然抢在首字节前面,自动化每次都被跳过,
/// 状态栏还会打出「自动化已跳过:0s 未收到远端输出」—— `status_text` 那里
/// 特意用 `div_ceil` 就是为了永不出现「0s」(见 `sub_second_timeout_rounds_up_
/// so_it_never_reads_zero`),但 `div_ceil` 拦不住字面 0。用下界从源头挡掉。
fn opt_ms(ui: &mut Ui, t: &Theme, id: &str, v: &mut Option<u32>, default: u32, min: u32, max: u32) {
    // `push_id` 而不是 `let _ = id;` —— 三个延时框长得一模一样,勾选框的 id 靠
    // 位置生成。一旦某一行因为条件渲染消失,后面几行的位置 id 会整体前移,
    // egui 会把上一行的勾选状态套到下一行上。给个显式 salt 钉死。
    ui.push_id(id, |ui| {
        ui.horizontal(|ui| {
            let mut on = v.is_some();
            if ui.checkbox(&mut on, "").changed() {
                *v = if on { Some(default) } else { None };
            }
            match v {
                Some(ms) => {
                    ui.add(egui::DragValue::new(ms).range(min..=max).suffix(" ms"));
                }
                None => {
                    ui.colored_label(
                        crate::theme::c32(t.fg_dimmer),
                        format!("继承(内置默认 {default} ms)"),
                    );
                }
            }
        });
    });
}

/// 固定警告横幅。用 `warn` 描边而不是纯文字:这两条说的都是「会把明文/字符
/// 发到远端」,混在普通说明里读者会滑过去。
fn warn_banner(ui: &mut Ui, t: &Theme, text: &str) {
    egui::Frame::none()
        .fill(crate::theme::c32(t.sunken_bg))
        .stroke(egui::Stroke::new(1.0, crate::theme::c32(t.warn)))
        .rounding(6.0)
        .inner_margin(8.0)
        .show(ui, |ui| {
            ui.colored_label(crate::theme::c32(t.warn), text);
        });
}

/// F41:配了逐条延时时置顶的硬性警告。**不是可选润色**——这是设计 §2
/// 核心约束的用户可见面:一旦拆成多步,第二步起就无法保证屏幕还归我们。
const DELAY_WARNING: &str = "配了延时的命令会拆成多步发送。第二步起,字符会进入\
当时屏幕上的任何程序 —— 如果远端已经 attach 上 TUI,它们会被打进那个程序的输入框。";

/// F43:env 区的固定警告。**不是可选润色**——用户会拿它存密码,而这里
/// 存不住密码:值以明文进 `sessions.toml`(不进 `secrets.enc`),且终归要以
/// `export` 行发到远端。
const ENV_WARNING: &str = "环境变量不是存密码的地方 —— 值以明文存进 sessions.toml,\
并会以 export 行发到远端,落进 shell 历史与 /proc/<pid>/environ。要存密码请用凭据。";

pub(super) fn basic(
    ui: &mut Ui,
    t: &Theme,
    buf: &mut EditorBuffer,
    groups: &[GroupRecord],
    sessions: &[SessionRecord],
    editing: Option<SessionId>,
) {
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

    jump(ui, t, buf, groups, sessions, editing);
}

/// F61/F62 外观。独占一个 Tab(`TAB_APPEARANCE`,排在「登录后」之后)。
///
/// 原先塞在「连接」页「归类」之后,实机验收时用户要求单开一页 —— 图标和颜色
/// 是一组独立的视觉设置,跟主机/端口不是同一个决策。
pub(crate) fn appearance(ui: &mut Ui, t: &Theme, buf: &mut EditorBuffer) {
    use mullion_store::{ColorSpec, ColorTarget, IconKind, IconSpec};

    section(ui, t, "外观");
    grid(ui, "sm_basic_appearance", |ui| {
        ui.label("图标");
        ui.vertical(|ui| {
            // **模式是 `buf.icon_emoji_mode` 这个持久位,不从 `preserved_appearance
            // .icon` 反推。** 反推的写法(v0.1.23)有个当场可见的 bug:刚点上
            // 「emoji」时缓冲是空的 → 写回 `None` → 下一帧反推成「无」→ UI 弹回
            // 去,输入框根本出不来。用户报的「点 emoji 没有内容」就是这个。
            let prev = buf.icon_emoji_mode;
            let mut on = prev;
            ui.horizontal(|ui| {
                ui.selectable_value(&mut on, false, "无");
                ui.selectable_value(&mut on, true, "emoji");
            });
            buf.icon_emoji_mode = on;

            if on {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut buf.icon_emoji_buf)
                            .desired_width(60.0)
                            .hint_text("🔥"),
                    );
                    for e in ["🔥", "🐧", "🗄", "⚙", "🌐", "🔒", "🧪", "📦"] {
                        if ui.small_button(e).clicked() {
                            buf.icon_emoji_buf = e.to_string();
                        }
                    }
                });
                if !buf.icon_emoji_buf.is_empty()
                    && !crate::ui::badge::emoji_is_paintable(&buf.icon_emoji_buf)
                {
                    ui.colored_label(
                        crate::theme::c32(t.warn),
                        format!(
                            "太长了(最多 {} 个字符),这样不会显示",
                            crate::ui::badge::MAX_EMOJI_CHARS
                        ),
                    );
                }
                ui.colored_label(
                    crate::theme::c32(t.fg_dimmer),
                    "emoji 显示为黑白剪影(egui 不支持彩色字形)",
                );

                // 缓冲空时写 `None` 但**模式位留着** —— 这正是上面那个 bug 的
                // 修法:真值可以是「暂时没有」,模式不能跟着丢。
                buf.preserved_appearance.icon = if buf.icon_emoji_buf.is_empty() {
                    None
                } else {
                    Some(IconSpec {
                        kind: IconKind::Emoji,
                        value: buf.icon_emoji_buf.clone(),
                    })
                };
            } else if prev {
                // 这一帧刚从 emoji 切到「无」= 用户明确要求清掉。
                buf.preserved_appearance.icon = None;
            }
            // else(始终「无」):`preserved_appearance.icon` 原样透传。可能是
            // 不可编辑的 `Builtin`(内置形状,UI 已撤)或 `Custom` —— 不支持
            // 编辑不等于允许静默删除用户数据。
        });
        ui.end_row();

        ui.label("颜色");
        ui.vertical(|ui| {
            ui.horizontal_wrapped(|ui| {
                for (name, hex, usage) in crate::theme::LABEL_PALETTE {
                    let selected = buf
                        .preserved_appearance
                        .color
                        .as_ref()
                        .is_some_and(|c| c.hex.eq_ignore_ascii_case(hex));
                    let (rect, resp) =
                        ui.allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::click());
                    if selected {
                        ui.painter().rect_stroke(
                            rect,
                            egui::Rounding::same(6.0),
                            egui::Stroke::new(2.0, crate::theme::c32(t.fg)),
                        );
                    }
                    if let Some(rgb) = crate::theme::parse_hex(hex) {
                        ui.painter()
                            .circle_filled(rect.center(), 7.0, crate::theme::c32(rgb));
                    }
                    if resp.clicked() {
                        match &mut buf.preserved_appearance.color {
                            Some(c) => c.hex = hex.to_string(),
                            // 新设色时的默认落点:会话列表 + pane 标题条。
                            // 状态栏不默认勾 —— 多 pane 时它该显示谁的色没有
                            // 确定答案。
                            None => {
                                buf.preserved_appearance.color = Some(ColorSpec {
                                    hex: hex.to_string(),
                                    apply_to: vec![ColorTarget::ListItem, ColorTarget::PaneTitle],
                                })
                            }
                        }
                    }
                    resp.on_hover_text(format!("{name} · {usage}"));
                }
                if ui.button("清除").clicked() {
                    buf.preserved_appearance.color = None;
                }
            });

            if let Some(c) = &mut buf.preserved_appearance.color {
                ui.horizontal(|ui| {
                    ui.label("自定义");
                    ui.add(
                        egui::TextEdit::singleline(&mut c.hex)
                            .desired_width(90.0)
                            .hint_text("#rrggbb"),
                    );
                    if crate::theme::parse_hex(&c.hex).is_none() {
                        ui.colored_label(crate::theme::c32(t.warn), "不是 #rrggbb,不会显示");
                    }
                });
            }
        });
        ui.end_row();

        ui.label("作用于");
        ui.vertical(|ui| match &mut buf.preserved_appearance.color {
            Some(spec) => {
                for (target, label) in [
                    (ColorTarget::ListItem, "会话列表"),
                    (ColorTarget::PaneTitle, "pane 标题条"),
                    (ColorTarget::StatusBar, "状态栏"),
                ] {
                    let mut on = spec.apply_to.contains(&target);
                    if ui.checkbox(&mut on, label).changed() {
                        crate::ui::session_manager::set_color_target(spec, target, on);
                    }
                }
            }
            None => {
                ui.colored_label(crate::theme::c32(t.fg_dimmer), "先选一个颜色");
            }
        });
        ui.end_row();
    });
}

/// F5 跳板链。放在**「连接」页**而不是「高级」页:跳板回答的是「怎么走到这台
/// 机器」,和主机/端口是同一个决策,配完主机紧接着就要配它;埋进「高级」里
/// 用户根本找不到。
///
/// 三态与 `NetworkPrefs::jump` 一一对应,见 `JumpModeUi` 的说明。
fn jump(
    ui: &mut Ui,
    t: &Theme,
    buf: &mut EditorBuffer,
    groups: &[GroupRecord],
    sessions: &[SessionRecord],
    editing: Option<SessionId>,
) {
    section(ui, t, "跳板");
    grid(ui, "sm_basic_jump", |ui| {
        ui.label("跳板");
        ui.horizontal(|ui| {
            // 与「认证方式」同样的选中态处理:egui 默认的 gamma_multiply 底色
            // 在深色面板上偏暗,分不出选中态。见 `auth()` 里那段注释。
            let vis = &mut ui.visuals_mut().selection;
            vis.bg_fill = crate::theme::c32(t.accent).linear_multiply(0.35);
            ui.selectable_value(&mut buf.jump_mode, JumpModeUi::None, "无");
            ui.selectable_value(&mut buf.jump_mode, JumpModeUi::Inherit, "继承分组");
            ui.selectable_value(&mut buf.jump_mode, JumpModeUi::Custom, "自定义");
        });
        ui.end_row();

        match buf.jump_mode {
            // 「无」是默认态,不需要任何解释性文字。
            JumpModeUi::None => {}
            JumpModeUi::Inherit => {
                ui.label("");
                ui.colored_label(
                    crate::theme::c32(t.fg_dimmer),
                    inherit_hint(buf.preserved_group_id, groups),
                );
                ui.end_row();
            }
            JumpModeUi::Custom => {
                ui.label("跳板链");
                ui.vertical(|ui| chain_editor(ui, t, buf, sessions, editing));
                ui.end_row();
            }
        }
    });
}

/// 「继承分组」下的说明文字。分组是**单层**的(设计 D1),所以上游只有一级,
/// 这里能把继承结果算准,不必含糊地说「跟随上游」。
fn inherit_hint(group_id: Option<mullion_store::GroupId>, groups: &[GroupRecord]) -> String {
    let Some(g) = group_id.and_then(|gid| groups.iter().find(|g| g.id == gid)) else {
        return "当前未分组,没有上游可继承 —— 实际等同于「无」。".to_string();
    };
    match &g.network.jump {
        Some(chain) if !chain.is_empty() => format!("跟随分组「{}」:{} 跳。", g.name, chain.len()),
        // 分组自己也没配(`None` 或显式空链)→ 继承下来还是不走跳板。
        _ => format!("分组「{}」未配跳板 —— 实际等同于「无」。", g.name),
    }
}

/// 自定义跳板链的逐跳编辑。按拨号顺序,`[0]` 最先连。
fn chain_editor(
    ui: &mut Ui,
    t: &Theme,
    buf: &mut EditorBuffer,
    sessions: &[SessionRecord],
    editing: Option<SessionId>,
) {
    // 本帧的结构变更先记下来,循环结束后统一施加 —— 循环里正持着
    // `buf.jump_chain` 的不可变借用。
    let mut remove: Option<usize> = None;
    let mut swap: Option<(usize, usize)> = None;
    let mut add: Option<SessionId> = None;

    let len = buf.jump_chain.len();
    for (i, id) in buf.jump_chain.iter().enumerate() {
        // salt 用 `(位置, 会话 id)`:上移/下移会让同一个 id 换位置,salt 跟着变,
        // egui 的按钮状态不会留在原位对上另一跳(同「登录后」页命令列表那个坑)。
        ui.push_id((i, id.0), |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("{}.", i + 1));
                match sessions.iter().find(|s| s.id == *id) {
                    Some(s) => {
                        ui.label(&s.identity.name);
                        ui.colored_label(
                            crate::theme::c32(t.fg_dimmer),
                            format!("{}@{}", s.auth.user, s.connection.host),
                        );
                    }
                    // 悬空引用不能悄悄不显示:拨号时 `expand_jump_chain` 会
                    // **硬失败**(设计 §6),用户得先看见是哪一跳没了。
                    None => {
                        ui.colored_label(
                            crate::theme::c32(t.danger),
                            format!("#{} 会话已删除", id.0),
                        );
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("✕").clicked() {
                        remove = Some(i);
                    }
                    if ui
                        .add_enabled(i + 1 < len, egui::Button::new("↓"))
                        .clicked()
                    {
                        swap = Some((i, i + 1));
                    }
                    if ui.add_enabled(i > 0, egui::Button::new("↑")).clicked() {
                        swap = Some((i - 1, i));
                    }
                });
            });
        });
    }

    if buf.jump_chain.is_empty() {
        ui.colored_label(
            crate::theme::c32(t.fg_dimmer),
            "还没加跳板 —— 保存后等同于「无」。",
        );
    }

    // 候选里剔掉**自己**和已在链里的:前者会被 `expand_chain` 判成成环而硬失败,
    // 后者会被去重成空操作,点了没反应比灰掉更让人困惑。
    egui::ComboBox::from_id_salt("sm_jump_add")
        .selected_text("+ 添加跳板")
        .show_ui(ui, |ui| {
            for s in sessions
                .iter()
                .filter(|s| Some(s.id) != editing && !buf.jump_chain.contains(&s.id))
            {
                let label = format!(
                    "{} ({}@{})",
                    s.identity.name, s.auth.user, s.connection.host
                );
                if ui.selectable_label(false, label).clicked() {
                    add = Some(s.id);
                }
            }
        });

    if let Some((a, b)) = swap {
        buf.jump_chain.swap(a, b);
    }
    if let Some(i) = remove {
        buf.jump_chain.remove(i);
    }
    if let Some(id) = add {
        buf.jump_chain.push(id);
    }
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
                // 三段 [状态文本][候选▾][导入…] 要在一行内伸缩,且不能引入
                // 硬编码的整行宽度(项目里没有 FIELD_W 这类常量)。用
                // right_to_left 布局:先摆两个按钮(它们的宽度由自身内容
                // 决定),摆完之后 `ui.available_width()` 就是右栏拖宽/
                // 缩窄后剩下的真实空间,留给状态文本。
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // v5 起路径不再进表单,这个按钮的语义从「选一个路径」变成
                    // 「把这个文件的内容读进来存库」。接线仍是老那套(另起线程
                    // 开 rfd,见 app.rs::spawn_key_picker),只是回调改成读正文。
                    if ui.button("导入…").clicked() {
                        buf.pick_key_clicked = true;
                    }

                    if let Some(p) = key_candidate_combo(ui, key_candidates).1 {
                        super::import_key_file(buf, &p, |p| std::fs::read_to_string(p));
                    }

                    // 已有钥匙时给个「清除」——否则用户没有任何办法把一把
                    // 存错的钥匙从库里弄掉(改认证方式再改回来也不会清)。
                    let has_key =
                        presence.private_key || (buf.key_touched && !buf.key_data.is_empty());
                    if has_key && ui.button("清除").clicked() {
                        super::clear_key(buf);
                    }

                    // 只报状态,不显示正文也不显示路径。「未设置」标红:v4→v5
                    // 迁移读不到旧文件的会话就是这个样子,不标红用户只会在
                    // 连接失败时才发现。
                    let (text, color) = if buf.key_touched && !buf.key_data.is_empty() {
                        ("已导入(未保存)", t.fg)
                    } else if buf.key_touched {
                        ("已清除(未保存)", t.danger)
                    } else if presence.private_key {
                        ("已导入", t.fg)
                    } else {
                        ("未设置 —— 请导入私钥文件", t.danger)
                    };
                    ui.colored_label(crate::theme::c32(color), text);
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
///
/// 返回 `(Response, 本帧被点中的候选)`。**不自己读文件**:导入是调用方的事,
/// 这样这个函数仍是零 IO 的,守护测试不用去铺一棵假的 `~/.ssh`。
pub(super) fn key_candidate_combo(
    ui: &mut Ui,
    key_candidates: &[std::path::PathBuf],
) -> (egui::Response, Option<std::path::PathBuf>) {
    // 候选下拉。为空时禁用并说明原因——一个点了没反应的
    // 按钮比一个明说「没找到」的灰按钮更让人困惑。
    let mut picked = None;
    let has_cand = !key_candidates.is_empty();
    let resp = ui
        .add_enabled_ui(has_cand, |ui| {
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
                            picked = Some(p.clone());
                        }
                    }
                });
        })
        .response
        .on_disabled_hover_text("未在 ~/.ssh 找到私钥");
    (resp, picked)
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
}

/// F40~F44「登录后」页。字段全部落在 `buf.preserved_automation` 上。
///
/// 字段名沿用 `preserved_*` 前缀而**没有**改成 `automation`:与
/// `preserved_group_id`(自 P0-b 起可编辑,名字未改)同一个理由 —— 改名会波及
/// `buffer.rs` 的透传守护测试,收益为零。
pub(super) fn automation(ui: &mut Ui, t: &Theme, buf: &mut EditorBuffer) {
    // tmux 会话名留空时的推导结果,作 placeholder 实时显示。先算好再借
    // `buf.preserved_automation`,后面几节就不用反复回头碰 `buf` 了。
    let derived = mullion_store::automation::sanitize_tmux_name(&buf.name);
    let a = &mut buf.preserved_automation;

    // 警告置顶:滚到页面底部才看到「会打进 TUI 输入框」就太晚了。
    if a.commands.iter().flatten().any(|c| c.delay_ms.is_some()) {
        warn_banner(ui, t, DELAY_WARNING);
        ui.add_space(6.0);
    }

    section(ui, t, "总开关");
    grid(ui, "sm_auto_enabled", |ui| {
        ui.label("登录后自动化");
        tri_state(ui, "sm_auto_enabled_combo", &mut a.enabled, "开", "关");
        ui.end_row();
    });

    section(ui, t, "tmux");
    grid(ui, "sm_auto_tmux", |ui| {
        ui.label("连上后");
        let text = match &a.tmux {
            None => "继承",
            Some(TmuxChoice::Off) => "不用 tmux",
            Some(TmuxChoice::Attach { .. }) => "自动 attach",
        };
        egui::ComboBox::from_id_salt("sm_auto_tmux_combo")
            .selected_text(text)
            .show_ui(ui, |ui| {
                if ui.selectable_label(a.tmux.is_none(), "继承").clicked() {
                    a.tmux = None;
                }
                if ui
                    .selectable_label(matches!(a.tmux, Some(TmuxChoice::Off)), "不用 tmux")
                    .clicked()
                {
                    a.tmux = Some(TmuxChoice::Off);
                }
                // 已经是 Attach 时不要重建 —— 会把用户填好的会话名清掉。
                if ui
                    .selectable_label(
                        matches!(a.tmux, Some(TmuxChoice::Attach { .. })),
                        "自动 attach",
                    )
                    .clicked()
                    && !matches!(a.tmux, Some(TmuxChoice::Attach { .. }))
                {
                    a.tmux = Some(TmuxChoice::Attach { session_name: None });
                }
            });
        ui.end_row();

        if let Some(TmuxChoice::Attach { session_name }) = &mut a.tmux {
            ui.label("会话名");
            // 不给 EditorBuffer 另开一个 String 字段:两个真源迟早漂移。
            // 每帧从 Option<String> 展开成临时 String,改动时写回 ——
            // 清空 = 回到「由会话名推导」,正是要的语义。
            let mut s = session_name.clone().unwrap_or_default();
            let resp = ui.add(
                egui::TextEdit::singleline(&mut s)
                    .hint_text(if derived.is_empty() {
                        "会话名为空,无法推导 —— 必须手填".to_string()
                    } else {
                        format!("留空则用「{derived}」")
                    })
                    .desired_width(f32::INFINITY),
            );
            if resp.changed() {
                *session_name = if s.trim().is_empty() { None } else { Some(s) };
            }
            ui.end_row();
        }
    });

    section(ui, t, "工作目录");
    grid(ui, "sm_auto_dir", |ui| {
        ui.label("初始目录");
        let mut s = a.work_dir.clone().unwrap_or_default();
        let resp = ui.add(
            egui::TextEdit::singleline(&mut s)
                .hint_text("留空 = 继承(远端默认)")
                .desired_width(f32::INFINITY),
        );
        if resp.changed() {
            a.work_dir = if s.trim().is_empty() { None } else { Some(s) };
        }
        ui.end_row();
    });

    section(ui, t, "登录后命令");
    // `None`(继承)与 `Some(vec![])`(显式空覆盖)必须可区分 —— 所以**绝不能**
    // 用 `get_or_insert_with(Vec::new)`:那样光是打开这一页就会把「继承」
    // 悄悄翻成「显式覆盖成空」,分组里配的命令全部失效。
    let mut reset_commands = false;
    match a.commands.as_mut() {
        None => {
            ui.horizontal(|ui| {
                ui.colored_label(crate::theme::c32(t.fg_dimmer), "继承上游的命令列表");
                if ui.button("改为自定义").clicked() {
                    reset_commands = true; // 见下方,这里借着 a.commands 不能直接赋值
                }
            });
        }
        Some(cmds) => {
            let len = cmds.len();
            let mut remove: Option<usize> = None;
            let mut swap: Option<(usize, usize)> = None;
            // 多行拆条:(在第几行之后, 插哪些)。
            let mut insert_after: Option<(usize, Vec<String>)> = None;

            // egui 给没写 id 的 widget 发的是**位置 id**,而 `TextEdit` 的光标、
            // 选区、撤销栈都按这个 id 跨帧存活。列表一做增删换序,「第 N 个槽位」
            // 前后两帧就不是同一条命令了 —— 旧的撤销栈留在原地,用户一个 Ctrl+Z
            // 就能把别行的旧文本贴到这一行上,而这行文本是要真发到远端 shell 的。
            // 拿一个只在结构变化时才 +1 的世代号当 salt:重排后全体换 id、状态一
            // 起丢弃。丢掉撤销历史,好过把它套到错误的行上。
            let gen_id = egui::Id::new("sm_auto_cmds_gen");
            let generation: u64 = ui.data(|d| d.get_temp(gen_id).unwrap_or(0));

            for (i, c) in cmds.iter_mut().enumerate() {
                ui.push_id((generation, i), |ui| {
                    ui.horizontal(|ui| {
                        // 用 multiline 而不是 singleline:singleline 是否把粘贴进来
                        // 的换行原样交给我们,取决于 egui 内部实现;multiline 一定
                        // 会,拆条逻辑才有输入可拆。`desired_rows(1)` 让它看起来
                        // 仍是一行。
                        let resp = ui.add(
                            egui::TextEdit::multiline(&mut c.text)
                                .desired_rows(1)
                                .desired_width(240.0),
                        );
                        if resp.changed() && c.text.contains(['\n', '\r']) {
                            // 手敲回车 = 新增一条空行(spec §5)。两端空段的保留规则
                            // 见 `split_edited_line`,那里有单测。
                            let (head, rest) = crate::automation::split_edited_line(&c.text);
                            c.text = head;
                            if !rest.is_empty() {
                                insert_after = Some((i, rest));
                            }
                        }

                        let mut has_delay = c.delay_ms.is_some();
                        if ui.checkbox(&mut has_delay, "延时").changed() {
                            c.delay_ms = if has_delay {
                                Some(mullion_store::automation::DEFAULT_INTER_DELAY_MS)
                            } else {
                                None
                            };
                        }
                        if let Some(ms) = c.delay_ms.as_mut() {
                            ui.add(egui::DragValue::new(ms).range(0..=60_000u32).suffix(" ms"));
                        }

                        if ui.add_enabled(i > 0, egui::Button::new("↑")).clicked() {
                            swap = Some((i, i - 1));
                        }
                        if ui
                            .add_enabled(i + 1 < len, egui::Button::new("↓"))
                            .clicked()
                        {
                            swap = Some((i, i + 1));
                        }
                        if ui.button("✕").clicked() {
                            remove = Some(i);
                        }
                    });
                });
            }

            // 结构一变就换世代号,见上面 `gen_id` 那段注释。
            if insert_after.is_some() || swap.is_some() || remove.is_some() {
                ui.data_mut(|d| d.insert_temp(gen_id, generation.wrapping_add(1)));
            }

            // 变更统一在遍历结束后施加 —— 边遍历边改会让索引失效。
            if let Some((i, parts)) = insert_after {
                for (k, text) in parts.into_iter().enumerate() {
                    cmds.insert(
                        i + 1 + k,
                        mullion_store::AutomationCommand {
                            text,
                            delay_ms: None,
                        },
                    );
                }
            }
            if let Some((x, y)) = swap {
                cmds.swap(x, y);
            }
            if let Some(i) = remove {
                cmds.remove(i);
            }

            ui.horizontal(|ui| {
                if ui.button("+ 添加命令").clicked() {
                    cmds.push(mullion_store::AutomationCommand {
                        text: String::new(),
                        delay_ms: None,
                    });
                }
                if ui.button("恢复继承").clicked() {
                    reset_commands = true;
                }
            });
        }
    }
    // `a.commands` 的可变借用到这里才结束,现在才能整体换掉它。
    if reset_commands {
        a.commands = match a.commands {
            None => Some(Vec::new()),
            Some(_) => None,
        };
    }

    section(ui, t, "环境变量");
    warn_banner(ui, t, ENV_WARNING);
    ui.add_space(6.0);
    // 同 commands:`None`(继承)与 `Some(vec![])`(显式空覆盖)必须可区分。
    let mut reset_env = false;
    match a.env.as_mut() {
        None => {
            ui.horizontal(|ui| {
                ui.colored_label(crate::theme::c32(t.fg_dimmer), "继承上游的环境变量");
                if ui.button("改为自定义").clicked() {
                    reset_env = true;
                }
            });
        }
        Some(vars) => {
            let mut remove: Option<usize> = None;

            // 同「登录后命令」那节:这张表能删行,位置 id 会因此在两帧之间
            // 错位到不同的变量上(见上面 `gen_id` 那段注释,同一个坑)。用
            // 独立的世代号 salt,**删除**后整体换 id。末尾追加不换 —— 它不动
            // 任何已有行的槽位。
            let env_gen_id = egui::Id::new("sm_auto_env_gen");
            let env_generation: u64 = ui.data(|d| d.get_temp(env_gen_id).unwrap_or(0));

            for (i, v) in vars.iter_mut().enumerate() {
                ui.push_id((env_generation, i), |ui| {
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut v.key)
                                .hint_text("KEY")
                                .desired_width(140.0),
                        );
                        ui.label("=");
                        ui.add(
                            egui::TextEdit::singleline(&mut v.value)
                                .hint_text("值(明文)")
                                .desired_width(220.0),
                        );
                        if ui.button("✕").clicked() {
                            remove = Some(i);
                        }
                    });
                });
            }

            if remove.is_some() {
                ui.data_mut(|d| d.insert_temp(env_gen_id, env_generation.wrapping_add(1)));
            }

            if let Some(i) = remove {
                vars.remove(i);
            }
            ui.horizontal(|ui| {
                if ui.button("+ 添加变量").clicked() {
                    vars.push(mullion_store::EnvVar {
                        key: String::new(),
                        value: String::new(),
                    });
                }
                if ui.button("恢复继承").clicked() {
                    reset_env = true;
                }
            });
        }
    }
    if reset_env {
        a.env = match a.env {
            None => Some(Vec::new()),
            Some(_) => None,
        };
    }

    section(ui, t, "时序");
    grid(ui, "sm_auto_timing", |ui| {
        ui.label("首字节后再等");
        opt_ms(
            ui,
            t,
            "sm_auto_initial",
            &mut a.initial_delay_ms,
            mullion_store::automation::DEFAULT_INITIAL_DELAY_MS,
            0,
            10_000,
        );
        ui.end_row();

        ui.label("行间延时");
        opt_ms(
            ui,
            t,
            "sm_auto_inter",
            &mut a.inter_delay_ms,
            mullion_store::automation::DEFAULT_INTER_DELAY_MS,
            0,
            10_000,
        );
        ui.end_row();

        ui.label("就绪超时");
        opt_ms(
            ui,
            t,
            "sm_auto_ready",
            &mut a.ready_timeout_ms,
            mullion_store::automation::DEFAULT_READY_TIMEOUT_MS,
            1,
            120_000,
        );
        ui.end_row();
    });
}

#[cfg(test)]
mod tests {
    use super::{auth, automation, basic, key_candidate_combo, network, tri_state};
    use crate::ui::session_manager::{AuthKindUi, EditorBuffer, JumpModeUi, SecretPresence};
    use mullion_store::{Auth, AuthKind, Connection, Identity, Protocol, SessionId, SessionRecord};

    /// 在渲染结果的形状树里找**第一处**含 `needle` 的文字位置。抄自
    /// `list.rs` 同名测试辅助(那边是私有的,没有跨文件复用的路子,所以
    /// 这里按项目既有做法各自留一份)。
    fn find_text_pos(shapes: &[egui::epaint::ClippedShape], needle: &str) -> Option<egui::Pos2> {
        fn walk(shape: &egui::Shape, needle: &str) -> Option<egui::Pos2> {
            match shape {
                egui::Shape::Vec(v) => v.iter().find_map(|s| walk(s, needle)),
                egui::Shape::Text(t) if t.galley.job.text.contains(needle) => Some(t.pos),
                _ => None,
            }
        }
        shapes.iter().find_map(|cs| walk(&cs.shape, needle))
    }

    /// 同 `find_text_pos`,但返回**所有**匹配位置——命令列表/环境变量表格
    /// 每行都有一个同名按钮(「↑」「✕」…),只找第一个没法区分点的是哪一行。
    fn find_all_text_pos(shapes: &[egui::epaint::ClippedShape], needle: &str) -> Vec<egui::Pos2> {
        fn walk(shape: &egui::Shape, needle: &str, out: &mut Vec<egui::Pos2>) {
            match shape {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, needle, out)),
                egui::Shape::Text(t) if t.galley.job.text.contains(needle) => out.push(t.pos),
                _ => {}
            }
        }
        let mut out = Vec::new();
        shapes
            .iter()
            .for_each(|cs| walk(&cs.shape, needle, &mut out));
        out
    }

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
        let ctx_empty = egui::Context::default();
        let mut enabled_when_empty = true;
        let _ = ctx_empty.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let resp = key_candidate_combo(ui, &[]).0;
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
                let resp = key_candidate_combo(ui, &candidates).0;
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

    /// 靶子1(F44 总开关三态):`tri_state` 里 `on`/`off` 的映射在两处独立
    /// 发生——关闭态按钮显示的文字(`text` 那个 `match`)与下拉选项点击后
    /// 写回的值(`selectable_value`)。两处一旦互换,用户会看着写「关」的
    /// 按钮却是 `Some(true)`(自动化其实没关,是本靶子里危害最大的一种)。
    ///
    /// 用真实指针事件驱动,只渲染孤立的 `tri_state`(不经过整页
    /// `automation()`)——页面上「继承」类文案不止一处(工作目录/三个延时
    /// 的提示都含「继承」子串),混进整页会让文字匹配产生歧义。
    ///
    /// 验证边界:覆盖了 `text` match 与 `selectable_value` 两处映射是否
    /// 首尾一致;覆盖不到 `ComboBox` 弹出动画/悬停高亮这类纯视觉细节。
    #[test]
    fn tri_state_combo_keeps_on_off_labels_and_written_values_paired_not_swapped() {
        let mut v: Option<bool> = None;
        let ctx = egui::Context::default();
        let mut btn_rect = egui::Rect::NOTHING;

        let run = |ctx: &egui::Context,
                   v: &mut Option<bool>,
                   btn_rect: &mut egui::Rect,
                   input: egui::RawInput| {
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let resp = tri_state(ui, "guard_tri_state", v, "开", "关");
                    *btn_rect = resp.rect;
                });
            })
        };
        let click = |pos: egui::Pos2, pressed: bool| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let click_at = |ctx: &egui::Context,
                        v: &mut Option<bool>,
                        btn_rect: &mut egui::Rect,
                        pos: egui::Pos2| {
            run(
                ctx,
                v,
                btn_rect,
                egui::RawInput {
                    events: vec![
                        egui::Event::PointerMoved(pos),
                        click(pos, true),
                        click(pos, false),
                    ],
                    ..Default::default()
                },
            )
        };

        // 前两帧只是让布局稳定下来。
        let _ = run(&ctx, &mut v, &mut btn_rect, egui::RawInput::default());
        let _ = run(&ctx, &mut v, &mut btn_rect, egui::RawInput::default());

        // 打开下拉,点「开」。
        let open_pos = btn_rect.center();
        let _ = click_at(&ctx, &mut v, &mut btn_rect, open_pos);
        let out = run(&ctx, &mut v, &mut btn_rect, egui::RawInput::default());
        let on_pos = find_text_pos(&out.shapes, "开").expect("打开的下拉里应该有「开」选项");
        let _ = click_at(&ctx, &mut v, &mut btn_rect, on_pos);
        let out = run(&ctx, &mut v, &mut btn_rect, egui::RawInput::default());

        assert_eq!(
            v,
            Some(true),
            "点击「开」选项后 v 必须是 Some(true);实际 {v:?} —— \
             selectable_value(v, Some(true), on) 那一行可能被换成了 Some(false)"
        );
        assert!(
            find_text_pos(&out.shapes, "开").is_some(),
            "v=Some(true) 时关闭态按钮应显示「开」——text match 的 \
             Some(true)/Some(false) 分支可能被互换了,实际按钮没有「开」字样"
        );
        assert!(
            find_text_pos(&out.shapes, "关").is_none(),
            "v=Some(true) 时关闭态按钮不该出现「关」——text match 分支可能被互换了"
        );

        // 再打开,点「关」。
        let open_pos = btn_rect.center();
        let _ = click_at(&ctx, &mut v, &mut btn_rect, open_pos);
        let out = run(&ctx, &mut v, &mut btn_rect, egui::RawInput::default());
        let off_pos = find_text_pos(&out.shapes, "关").expect("打开的下拉里应该有「关」选项");
        let _ = click_at(&ctx, &mut v, &mut btn_rect, off_pos);
        let out = run(&ctx, &mut v, &mut btn_rect, egui::RawInput::default());

        assert_eq!(
            v,
            Some(false),
            "点击「关」选项后 v 必须是 Some(false);实际 {v:?}"
        );
        assert!(
            find_text_pos(&out.shapes, "关").is_some(),
            "v=Some(false) 时关闭态按钮应显示「关」;实际按钮没有「关」字样"
        );
        assert!(
            find_text_pos(&out.shapes, "开").is_none(),
            "v=Some(false) 时关闭态按钮不该出现「开」"
        );
    }

    /// 靶子3(高优先级):命令列表「上移」按钮点击后,必须是把第 i 条与第
    /// i-1 条互换,不能原地不动(等价于 `swap(i, i)`)。被挪动的是要真发
    /// 到远端 shell 的命令行——静默不生效比明显报错更危险,用户会以为顺序
    /// 已经调整好了。
    ///
    /// 用真实指针事件驱动:构造三条互不相同的命令,点第二条(索引 1)的
    /// 「↑」按钮,断言渲染结果里的顺序真的换了。按钮位置用
    /// `find_all_text_pos` 按 y 坐标排序后取第二个——一页里三行各有一个
    /// 「↑」,只找第一个区分不出点的是哪一行。
    ///
    /// 验证边界:覆盖「点了上移,列表真的换序」这条端到端链路——不管调包
    /// 发生在算 `(i, i-1)` 那一步还是最终 `cmds.swap(x, y)` 那一步都会被
    /// 扎到;覆盖不到按钮的悬停/禁用视觉样式。
    #[test]
    fn clicking_move_up_on_second_command_swaps_it_with_the_first_not_a_noop() {
        let t = crate::theme::MULLION_DARK;
        let mut buf = EditorBuffer::default();
        buf.preserved_automation.commands = Some(vec![
            mullion_store::AutomationCommand {
                text: "cmd-alpha".to_string(),
                delay_ms: None,
            },
            mullion_store::AutomationCommand {
                text: "cmd-beta".to_string(),
                delay_ms: None,
            },
            mullion_store::AutomationCommand {
                text: "cmd-gamma".to_string(),
                delay_ms: None,
            },
        ]);
        let ctx = egui::Context::default();

        let run = |ctx: &egui::Context, buf: &mut EditorBuffer, input: egui::RawInput| {
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    automation(ui, &t, buf);
                });
            })
        };
        let click = |pos: egui::Pos2, pressed: bool| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };

        let _ = run(&ctx, &mut buf, egui::RawInput::default());
        let out = run(&ctx, &mut buf, egui::RawInput::default());

        let mut up_positions = find_all_text_pos(&out.shapes, "↑");
        up_positions.sort_by(|a, b| a.y.partial_cmp(&b.y).unwrap());
        assert_eq!(up_positions.len(), 3, "三条命令应该各有一个「↑」按钮");
        let second_row_up = up_positions[1];

        let _ = run(
            &ctx,
            &mut buf,
            egui::RawInput {
                events: vec![
                    egui::Event::PointerMoved(second_row_up),
                    click(second_row_up, true),
                    click(second_row_up, false),
                ],
                ..Default::default()
            },
        );

        let cmds = buf.preserved_automation.commands.as_ref().unwrap();
        let texts: Vec<&str> = cmds.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(
            texts,
            vec!["cmd-beta", "cmd-alpha", "cmd-gamma"],
            "点第二条的「↑」应该让它跟第一条互换位置;实际顺序 {texts:?}"
        );
    }

    /// 靶子4(高优先级):环境变量「删除」按钮必须删掉**被点的那一行**,
    /// 不能恒删第一行(`vars.remove(0)`)——只有两行以内时这两种实现表现
    /// 一样,必须用三行以上、点中间那行才能把它们区分开。
    ///
    /// 验证边界同靶子3:端到端指针事件驱动,覆盖「点哪一行的 ✕ 就删哪一
    /// 行」这条链路;覆盖不到「恢复继承」按钮的重置行为(不在本靶子范围)。
    #[test]
    fn clicking_remove_on_middle_env_var_deletes_that_row_not_always_the_first() {
        let t = crate::theme::MULLION_DARK;
        let mut buf = EditorBuffer::default();
        buf.preserved_automation.env = Some(vec![
            mullion_store::EnvVar {
                key: "KEY_A".to_string(),
                value: "a".to_string(),
            },
            mullion_store::EnvVar {
                key: "KEY_B".to_string(),
                value: "b".to_string(),
            },
            mullion_store::EnvVar {
                key: "KEY_C".to_string(),
                value: "c".to_string(),
            },
        ]);
        let ctx = egui::Context::default();

        let run = |ctx: &egui::Context, buf: &mut EditorBuffer, input: egui::RawInput| {
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    automation(ui, &t, buf);
                });
            })
        };
        let click = |pos: egui::Pos2, pressed: bool| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };

        let _ = run(&ctx, &mut buf, egui::RawInput::default());
        let out = run(&ctx, &mut buf, egui::RawInput::default());

        let mut remove_positions = find_all_text_pos(&out.shapes, "✕");
        remove_positions.sort_by(|a, b| a.y.partial_cmp(&b.y).unwrap());
        assert_eq!(
            remove_positions.len(),
            3,
            "三条环境变量应该各有一个「✕」按钮"
        );
        let middle_row_remove = remove_positions[1];

        let _ = run(
            &ctx,
            &mut buf,
            egui::RawInput {
                events: vec![
                    egui::Event::PointerMoved(middle_row_remove),
                    click(middle_row_remove, true),
                    click(middle_row_remove, false),
                ],
                ..Default::default()
            },
        );

        let vars = buf.preserved_automation.env.as_ref().unwrap();
        let keys: Vec<&str> = vars.iter().map(|v| v.key.as_str()).collect();
        assert_eq!(
            keys,
            vec!["KEY_A", "KEY_C"],
            "点第二行(KEY_B)的「✕」应该只删掉 KEY_B;实际剩下 {keys:?}"
        );
    }

    /// 靶子6(中优先级):延时警告横幅的触发条件——「任意一条命令配了
    /// `delay_ms`」——不能被取反。取反后要么「配了延时却不提示会打进 TUI
    /// 输入框」(真出问题时用户毫无防备),要么「没配延时却一直吓唬用户」
    /// (狼来了,久了没人看)。
    ///
    /// 用两种命令列表状态(全部 `delay_ms: None` / 至少一条 `Some(_)`)各
    /// 跑一次,断言横幅文案只在后一种情况下出现。断言用 `DELAY_WARNING`
    /// 常量的一段独有子串,不是重新拼一遍常量再判等——否则常量整体改写时
    /// 测试会跟着常量一起改意思,变成恒真的重言式。
    ///
    /// 验证边界:覆盖了触发条件的真假两支;覆盖不到 `warn_banner` 的颜色/
    /// 描边等纯视觉细节。
    #[test]
    fn delay_warning_banner_shows_only_when_some_command_has_a_delay_not_inverted() {
        let t = crate::theme::MULLION_DARK;

        let render = |commands: Vec<mullion_store::AutomationCommand>| {
            let mut buf = EditorBuffer::default();
            buf.preserved_automation.commands = Some(commands);
            let ctx = egui::Context::default();
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    automation(ui, &t, &mut buf);
                });
            });
            ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    automation(ui, &t, &mut buf);
                });
            })
        };

        let no_delay = render(vec![mullion_store::AutomationCommand {
            text: "echo hi".to_string(),
            delay_ms: None,
        }]);
        assert!(
            find_text_pos(&no_delay.shapes, "配了延时的命令会拆成多步发送").is_none(),
            "全部命令都没配延时时,不该出现延时警告横幅"
        );

        let with_delay = render(vec![mullion_store::AutomationCommand {
            text: "echo hi".to_string(),
            delay_ms: Some(500),
        }]);
        assert!(
            find_text_pos(&with_delay.shapes, "配了延时的命令会拆成多步发送").is_some(),
            "有命令配了延时时,必须出现延时警告横幅"
        );

        // 混合列表:只有**一条**配了延时。上面两例都是单元素列表,`any` 和 `all`
        // 在单元素上等价 —— 光靠它们,把 `.any(..)` 改成 `.all(..)` 测试照样绿。
        // 而 `all` 的现实含义是「只要有一条没配延时就不警告」,那正是最该警告的
        // 情形:剩下几条不带延时的会跟着被拆成多步发出去。
        let mixed = render(vec![
            mullion_store::AutomationCommand {
                text: "echo first".to_string(),
                delay_ms: None,
            },
            mullion_store::AutomationCommand {
                text: "echo second".to_string(),
                delay_ms: Some(500),
            },
        ]);
        assert!(
            find_text_pos(&mixed.shapes, "配了延时的命令会拆成多步发送").is_some(),
            "只要**有一条**命令配了延时就必须警告,不是「全部都配了」才警告"
        );
    }

    /// 靶子5(中优先级):三个「时序」延时框勾选后写入的默认值,必须是
    /// `automation()` 在对应那一行调用 `opt_ms` 时传的 `default` 实参——
    /// 三处调用点一旦互相调包(比如「首字节后再等」传成了行间延时的
    /// 200ms),用户打勾后看到的数字会跟标签对不上,且不报错。
    ///
    /// 复选框没有文字标签(`ui.checkbox(&mut on, "")`),按钮位置靠同一行
    /// 右侧「继承(内置默认 N ms)」提示文案反推——复选框紧贴在提示文字
    /// 左边(见 `opt_ms` 里 `ui.horizontal` 的组装顺序:先 checkbox 后
    /// `colored_label`),偏移量按 egui 0.30 默认 `icon_width=14` 加
    /// `item_spacing.x=8` 估算,取宽裕的点位保证落在 18×18 的复选框内。
    /// 每次点击后都重新渲染一帧、重新定位下一个字段的提示文字,不复用
    /// 点击前的坐标——前一行从「提示文字」换成「DragValue」后,grid 行高
    /// 可能变化,后续行的位置会跟着挪动。
    ///
    /// 验证边界:覆盖了三处调用点各自的 `default` 实参是否对应正确常量;
    /// 覆盖不到 `DragValue` 拖拽改值的手感。
    #[test]
    fn checking_each_delay_box_writes_its_own_call_sites_default_not_a_swapped_one() {
        let t = crate::theme::MULLION_DARK;
        let mut buf = EditorBuffer::default();
        let ctx = egui::Context::default();

        let run = |ctx: &egui::Context, buf: &mut EditorBuffer, input: egui::RawInput| {
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    automation(ui, &t, buf);
                });
            })
        };
        let click = |pos: egui::Pos2, pressed: bool| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let checkbox_pos_left_of_hint =
            |hint_pos: egui::Pos2| egui::pos2(hint_pos.x - 17.0, hint_pos.y + 8.0);
        let check = |ctx: &egui::Context, buf: &mut EditorBuffer, pos: egui::Pos2| {
            run(
                ctx,
                buf,
                egui::RawInput {
                    events: vec![
                        egui::Event::PointerMoved(pos),
                        click(pos, true),
                        click(pos, false),
                    ],
                    ..Default::default()
                },
            )
        };

        let _ = run(&ctx, &mut buf, egui::RawInput::default());
        let out = run(&ctx, &mut buf, egui::RawInput::default());

        // 「首字节后再等」→ DEFAULT_INITIAL_DELAY_MS。
        let hint_pos = find_text_pos(&out.shapes, "继承(内置默认 300 ms)")
            .expect("「首字节后再等」应显示内置默认 300ms 的继承提示");
        let _ = check(&ctx, &mut buf, checkbox_pos_left_of_hint(hint_pos));
        let out = run(&ctx, &mut buf, egui::RawInput::default());
        assert_eq!(
            buf.preserved_automation.initial_delay_ms,
            Some(mullion_store::automation::DEFAULT_INITIAL_DELAY_MS),
            "勾选「首字节后再等」应写入 DEFAULT_INITIAL_DELAY_MS(300);实际 {:?}",
            buf.preserved_automation.initial_delay_ms
        );

        // 「行间延时」→ DEFAULT_INTER_DELAY_MS。
        let hint_pos = find_text_pos(&out.shapes, "继承(内置默认 200 ms)")
            .expect("「行间延时」应显示内置默认 200ms 的继承提示");
        let _ = check(&ctx, &mut buf, checkbox_pos_left_of_hint(hint_pos));
        let out = run(&ctx, &mut buf, egui::RawInput::default());
        assert_eq!(
            buf.preserved_automation.inter_delay_ms,
            Some(mullion_store::automation::DEFAULT_INTER_DELAY_MS),
            "勾选「行间延时」应写入 DEFAULT_INTER_DELAY_MS(200);实际 {:?}",
            buf.preserved_automation.inter_delay_ms
        );

        // 「就绪超时」→ DEFAULT_READY_TIMEOUT_MS。
        let hint_pos = find_text_pos(&out.shapes, "继承(内置默认 15000 ms)")
            .expect("「就绪超时」应显示内置默认 15000ms 的继承提示");
        let _ = check(&ctx, &mut buf, checkbox_pos_left_of_hint(hint_pos));
        let _ = run(&ctx, &mut buf, egui::RawInput::default());
        assert_eq!(
            buf.preserved_automation.ready_timeout_ms,
            Some(mullion_store::automation::DEFAULT_READY_TIMEOUT_MS),
            "勾选「就绪超时」应写入 DEFAULT_READY_TIMEOUT_MS(15000);实际 {:?}",
            buf.preserved_automation.ready_timeout_ms
        );
    }

    /// 跳板测试用的最小会话记录。名字/用户/主机都取得互不相同,便于在形状树里
    /// 按文字区分是哪一条。
    fn sess(id: u64, name: &str, host: &str) -> SessionRecord {
        SessionRecord {
            id: SessionId(id),
            modified_at: "t".into(),
            identity: Identity {
                name: name.into(),
                note: String::new(),
                group_id: None,
                tags: Vec::new(),
            },
            connection: Connection {
                host: host.into(),
                port: 22,
                protocol: Protocol::Ssh,
            },
            auth: Auth {
                user: "ops".into(),
                kind: AuthKind::Password,
            },
            terminal: Default::default(),
            appearance: Default::default(),
            network: Default::default(),
            automation: Default::default(),
        }
    }

    fn run_basic(buf: &mut EditorBuffer, sessions: &[SessionRecord]) -> egui::FullOutput {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let run = |buf: &mut EditorBuffer| {
            ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    basic(ui, &t, buf, &[], sessions, Some(SessionId(1)));
                });
            })
        };
        let _ = run(buf);
        run(buf)
    }

    /// 跑「图标」页两帧,返回**第二帧**的输出。
    ///
    /// 必须跑两帧:本页所有 bug 都是「第一帧看着对、第二帧弹回去」这一类
    /// (状态写回 → 下一帧重新读)。只跑一帧的测试对它们全盲。
    fn run_appearance(buf: &mut EditorBuffer) -> egui::FullOutput {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let run = |buf: &mut EditorBuffer| {
            ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    super::appearance(ui, &t, buf);
                });
            })
        };
        let _ = run(buf);
        run(buf)
    }

    /// **v0.1.23 实机验收报的「点 emoji 没有内容」的守护测试。**
    ///
    /// 旧实现把模式从 `preserved_appearance.icon` 反推:刚点上「emoji」时缓冲
    /// 还是空的 → 写回 `None` → 下一帧反推回「无」→ 输入框当场消失。用户看到
    /// 的就是「点了没反应」。
    ///
    /// 自证会变红的方式:把 `appearance()` 里的
    /// `let mut on = buf.icon_emoji_mode;` 换回反推写法
    /// `let mut on = buf.preserved_appearance.icon.is_some();`。
    #[test]
    fn emoji_mode_survives_the_next_frame_with_an_empty_buffer() {
        let mut buf = EditorBuffer {
            icon_emoji_mode: true,
            ..Default::default()
        };
        let out = run_appearance(&mut buf);
        assert!(
            find_text_pos(&out.shapes, "黑白剪影").is_some(),
            "选了 emoji 模式但还没填内容时,输入区必须留在页面上;\
             消失了就是用户报的「点 emoji 没有内容」"
        );
        assert!(buf.icon_emoji_mode, "模式位不该被自己的写回逻辑抹掉");
        assert!(
            buf.preserved_appearance.icon.is_none(),
            "缓冲是空的,不该凭空造出一个空 emoji 图标"
        );
    }

    /// 填了内容就要真写进 `preserved_appearance`(保存路径取的是它)。
    /// 这是上一条的对照组:少了它,把写回整段删掉也能让上一条全绿。
    #[test]
    fn a_filled_emoji_buffer_is_written_back_to_the_record() {
        let mut buf = EditorBuffer {
            icon_emoji_mode: true,
            icon_emoji_buf: "🔥".into(),
            ..Default::default()
        };
        run_appearance(&mut buf);
        let icon = buf
            .preserved_appearance
            .icon
            .as_ref()
            .expect("填了 emoji 就该写出图标");
        assert_eq!(icon.kind, mullion_store::IconKind::Emoji);
        assert_eq!(icon.value, "🔥");
    }

    /// 用户主动点「无」→ 真清掉图标,但**缓冲留着**。
    ///
    /// 端到端指针事件驱动,不手改 `icon_emoji_mode` —— 手改等于把「上一帧是
    /// 什么模式」这个信息一起抹掉,而那正是这条分支的判据。
    ///
    /// 自证会变红的方式:删掉 `appearance()` 里的 `else if prev { ... = None }`
    /// 整个分支——切到「无」后图标还在,用户以为删掉了,保存下去却纹丝不动。
    #[test]
    fn clicking_none_actually_clears_the_icon_but_keeps_the_buffer() {
        let t = crate::theme::MULLION_DARK;
        let mut buf = EditorBuffer {
            icon_emoji_mode: true,
            icon_emoji_buf: "🔥".into(),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let run = |ctx: &egui::Context, buf: &mut EditorBuffer, input: egui::RawInput| {
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    super::appearance(ui, &t, buf);
                });
            })
        };
        let click = |pos: egui::Pos2, pressed: bool| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };

        let out = run(&ctx, &mut buf, Default::default());
        assert!(
            buf.preserved_appearance.icon.is_some(),
            "前提没成立:点「无」之前得先有一个图标,否则这条测试恒过"
        );
        let none_pos = find_text_pos(&out.shapes, "无").expect("图标区应该有个「无」可点");

        let _ = run(
            &ctx,
            &mut buf,
            egui::RawInput {
                events: vec![
                    egui::Event::PointerMoved(none_pos),
                    click(none_pos, true),
                    click(none_pos, false),
                ],
                ..Default::default()
            },
        );

        assert!(!buf.icon_emoji_mode, "点了「无」模式位必须跟着落下来");
        assert!(
            buf.preserved_appearance.icon.is_none(),
            "从 emoji 切到「无」必须真把图标清掉"
        );
        assert_eq!(buf.icon_emoji_buf, "🔥", "缓冲要留着,用户切回来还能看到");
    }

    /// UI 上编辑不了的图标种类(`Builtin` 内置形状 v0.1.24 撤掉、`Custom` 不做)
    /// **不该被静默抹掉**。这两个变体还在 store schema 里,旧配置可能有值。
    ///
    /// 自证会变红的方式:把 `appearance()` 里的 `else if prev` 改成裸 `else`
    /// —— 只要用户翻到这一页,不认识的图标就被清空了,而且没有任何提示。
    #[test]
    fn an_uneditable_icon_is_preserved_instead_of_being_wiped() {
        for kind in [
            mullion_store::IconKind::Builtin,
            mullion_store::IconKind::Custom,
        ] {
            let mut buf = EditorBuffer::default();
            buf.preserved_appearance.icon = Some(mullion_store::IconSpec {
                kind,
                value: "hexagon".into(),
            });
            run_appearance(&mut buf);
            assert_eq!(
                buf.preserved_appearance.icon.as_ref().map(|i| i.kind),
                Some(kind),
                "UI 编辑不了的 {kind:?} 图标不该因为翻了一下这一页就消失"
            );
        }
    }

    /// 「形状」按用户要求撤掉,图标只剩「无 / emoji」两态。
    #[test]
    fn the_appearance_page_no_longer_offers_builtin_shapes() {
        let mut buf = EditorBuffer::default();
        let out = run_appearance(&mut buf);
        assert!(
            find_text_pos(&out.shapes, "形状").is_none(),
            "「形状」模式已撤,不该还画在页面上"
        );
        assert!(
            find_text_pos(&out.shapes, "emoji").is_some(),
            "「emoji」模式必须还在——它现在是唯一的图标载体"
        );
    }

    /// 外观搬去了独立的「图标」页,「连接」页上不能还留一份。
    ///
    /// **两侧都要断言**:只断言「图标页有」的话,搬运时忘了从「连接」页删掉
    /// 就会两处都有,而两处各写各的 `preserved_appearance`,后画的那处每帧
    /// 覆盖前一处 —— 同跳板那条测试的姿态(`jump_lives_on_the_connect_page`)。
    #[test]
    fn the_appearance_section_moved_off_the_connect_page() {
        let mut buf = EditorBuffer::default();
        let out = run_basic(&mut buf, &[]);
        assert!(
            find_text_pos(&out.shapes, "外观").is_none(),
            "「连接」页不该还有外观分节"
        );

        let mut buf2 = EditorBuffer::default();
        let out2 = run_appearance(&mut buf2);
        assert!(
            find_text_pos(&out2.shapes, "外观").is_some(),
            "「图标」页必须有外观分节"
        );
    }

    fn run_auth(buf: &mut EditorBuffer, presence: SecretPresence) -> egui::FullOutput {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let run = |buf: &mut EditorBuffer| {
            ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    auth(ui, &t, buf, presence, &[]);
                });
            })
        };
        let _ = run(buf);
        run(buf)
    }

    /// 用户明确要求:私钥**路径不保存也不显示**。认证页只报「已导入 / 未设置」。
    /// 顺带钉死「未设置」这条 —— v4→v5 迁移读不到旧文件的会话就落在这个状态,
    /// 界面上不提示的话,用户只会在下次连接失败时才发现钥匙没了。
    #[test]
    fn the_auth_page_reports_key_presence_instead_of_a_path() {
        let mut buf = EditorBuffer {
            auth_kind: AuthKindUi::PublicKey,
            ..Default::default()
        };

        let out = run_auth(&mut buf, SecretPresence::default());
        assert!(
            find_text_pos(&out.shapes, "未设置").is_some(),
            "没有私钥时要明说「未设置」"
        );

        let mut buf2 = EditorBuffer {
            auth_kind: AuthKindUi::PublicKey,
            ..Default::default()
        };
        let out = run_auth(
            &mut buf2,
            SecretPresence {
                private_key: true,
                ..Default::default()
            },
        );
        assert!(
            find_text_pos(&out.shapes, "已导入").is_some(),
            "库里有私钥时要显示「已导入」"
        );
        assert!(
            find_text_pos(&out.shapes, "未设置").is_none(),
            "已导入的会话不该同时显示「未设置」"
        );
    }

    /// 私钥正文绝不能画到屏幕上 —— 截图、录屏、旁人一眼就拿走了。
    #[test]
    fn the_auth_page_never_renders_the_key_body() {
        let mut buf = EditorBuffer {
            auth_kind: AuthKindUi::PublicKey,
            key_touched: true,
            key_data: "-----BEGIN OPENSSH PRIVATE KEY-----\nSECRETBODY\n".into(),
            ..Default::default()
        };
        let out = run_auth(&mut buf, SecretPresence::default());
        assert!(
            find_text_pos(&out.shapes, "SECRETBODY").is_none(),
            "私钥正文被画到界面上了"
        );
    }

    /// 用户明确要求:跳板配在**「连接」页**,不在「高级」页。这条同时钉死两侧
    /// —— 只断言「连接页有」的话,搬运时忘了从「高级」页删掉就会两处都有,
    /// 用户改了一处以为生效、另一处显示的还是旧值。
    #[test]
    fn jump_section_lives_on_the_connect_page_and_not_on_the_advanced_page() {
        let mut buf = EditorBuffer::default();
        let out = run_basic(&mut buf, &[]);
        assert!(
            find_text_pos(&out.shapes, "跳板").is_some(),
            "「连接」页必须有跳板分节"
        );
        for opt in ["无", "继承分组", "自定义"] {
            assert!(
                find_text_pos(&out.shapes, opt).is_some(),
                "「连接」页的跳板应给出三个选项,缺了「{opt}」"
            );
        }

        let t = crate::theme::MULLION_DARK;
        let mut buf = EditorBuffer::default();
        let ctx = egui::Context::default();
        let run_net = |buf: &mut EditorBuffer| {
            ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    network(ui, &t, buf, SecretPresence::default());
                });
            })
        };
        let _ = run_net(&mut buf);
        let out = run_net(&mut buf);
        assert!(
            find_text_pos(&out.shapes, "跳板").is_none(),
            "「高级」页不该再有跳板分节 —— 两处都有会让用户改了一处以为生效"
        );
    }

    /// 「继承分组」下不该冒出跳板链编辑器,「自定义」下必须冒出来。选了继承却
    /// 还能编辑一条只在「自定义」时才写回的链,是纯粹的假编辑框。
    #[test]
    fn the_chain_editor_appears_only_in_custom_mode() {
        let sessions = [
            sess(1, "self-session", "10.0.0.1"),
            sess(2, "hop-alpha", "10.0.0.2"),
        ];

        let mut inherit = EditorBuffer {
            jump_mode: JumpModeUi::Inherit,
            jump_chain: vec![SessionId(2)],
            ..EditorBuffer::default()
        };
        let out = run_basic(&mut inherit, &sessions);
        assert!(
            find_text_pos(&out.shapes, "hop-alpha").is_none(),
            "「继承分组」下不该画出跳板链"
        );

        let mut custom = EditorBuffer {
            jump_mode: JumpModeUi::Custom,
            jump_chain: vec![SessionId(2)],
            ..EditorBuffer::default()
        };
        let out = run_basic(&mut custom, &sessions);
        assert!(
            find_text_pos(&out.shapes, "hop-alpha").is_some(),
            "「自定义」下必须把链里的跳板显示出来"
        );
    }

    /// 候选下拉必须剔掉**正在编辑的这条会话自己**:自引用会被
    /// `jump::expand_chain` 判成 `JumpCycle` 而**硬失败**(设计 §6),用户点得到
    /// 就等于给了他一个点完必然连不上的选项。
    #[test]
    fn the_hop_picker_excludes_the_session_being_edited_so_it_cannot_reference_itself() {
        let t = crate::theme::MULLION_DARK;
        let sessions = [
            sess(1, "self-session", "10.0.0.1"),
            sess(2, "hop-alpha", "10.0.0.2"),
        ];
        let mut buf = EditorBuffer {
            jump_mode: JumpModeUi::Custom,
            ..EditorBuffer::default()
        };
        let ctx = egui::Context::default();
        let run = |buf: &mut EditorBuffer, input: egui::RawInput| {
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    basic(ui, &t, buf, &[], &sessions, Some(SessionId(1)));
                });
            })
        };
        let click = |pos: egui::Pos2, pressed: bool| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };

        let _ = run(&mut buf, egui::RawInput::default());
        let out = run(&mut buf, egui::RawInput::default());
        let add_pos =
            find_text_pos(&out.shapes, "添加跳板").expect("自定义模式下应有「+ 添加跳板」");
        let _ = run(
            &mut buf,
            egui::RawInput {
                events: vec![
                    egui::Event::PointerMoved(add_pos),
                    click(add_pos, true),
                    click(add_pos, false),
                ],
                ..Default::default()
            },
        );
        let out = run(&mut buf, egui::RawInput::default());

        assert!(
            find_text_pos(&out.shapes, "hop-alpha").is_some(),
            "别的会话应出现在候选里 —— 下拉没打开的话这条测试什么也没验到"
        );
        assert!(
            find_text_pos(&out.shapes, "self-session").is_none(),
            "正在编辑的这条会话不该出现在候选里:选中它会让拨号时成环硬失败"
        );
    }

    /// 点第二跳的「↑」必须真的与第一跳互换,不能原地不动。跳板顺序决定实际
    /// 拨号路径,静默不生效比报错更危险 —— 同「登录后」页命令列表那条靶子。
    #[test]
    fn clicking_move_up_on_second_hop_swaps_it_with_the_first_not_a_noop() {
        let t = crate::theme::MULLION_DARK;
        let sessions = [
            sess(1, "self-session", "10.0.0.1"),
            sess(2, "hop-alpha", "10.0.0.2"),
            sess(3, "hop-beta", "10.0.0.3"),
            sess(4, "hop-gamma", "10.0.0.4"),
        ];
        let mut buf = EditorBuffer {
            jump_mode: JumpModeUi::Custom,
            jump_chain: vec![SessionId(2), SessionId(3), SessionId(4)],
            ..EditorBuffer::default()
        };
        let ctx = egui::Context::default();
        let run = |buf: &mut EditorBuffer, input: egui::RawInput| {
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    basic(ui, &t, buf, &[], &sessions, Some(SessionId(1)));
                });
            })
        };
        let click = |pos: egui::Pos2, pressed: bool| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };

        let _ = run(&mut buf, egui::RawInput::default());
        let out = run(&mut buf, egui::RawInput::default());

        let mut ups = find_all_text_pos(&out.shapes, "↑");
        ups.sort_by(|a, b| a.y.partial_cmp(&b.y).unwrap());
        assert_eq!(ups.len(), 3, "三跳应该各有一个「↑」按钮");
        let second = ups[1];

        let _ = run(
            &mut buf,
            egui::RawInput {
                events: vec![
                    egui::Event::PointerMoved(second),
                    click(second, true),
                    click(second, false),
                ],
                ..Default::default()
            },
        );
        assert_eq!(
            buf.jump_chain,
            vec![SessionId(3), SessionId(2), SessionId(4)],
            "点第二跳的「↑」应让它与第一跳互换;实际 {:?}",
            buf.jump_chain
        );
    }

    /// 点某一跳的「✕」必须删掉**被点的那一跳**,不能恒删第一跳。两跳以内两种
    /// 实现表现一样,必须三跳、点中间那跳才能区分开。
    #[test]
    fn clicking_remove_on_middle_hop_deletes_that_hop_not_always_the_first() {
        let t = crate::theme::MULLION_DARK;
        let sessions = [
            sess(1, "self-session", "10.0.0.1"),
            sess(2, "hop-alpha", "10.0.0.2"),
            sess(3, "hop-beta", "10.0.0.3"),
            sess(4, "hop-gamma", "10.0.0.4"),
        ];
        let mut buf = EditorBuffer {
            jump_mode: JumpModeUi::Custom,
            jump_chain: vec![SessionId(2), SessionId(3), SessionId(4)],
            ..EditorBuffer::default()
        };
        let ctx = egui::Context::default();
        let run = |buf: &mut EditorBuffer, input: egui::RawInput| {
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    basic(ui, &t, buf, &[], &sessions, Some(SessionId(1)));
                });
            })
        };
        let click = |pos: egui::Pos2, pressed: bool| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };

        let _ = run(&mut buf, egui::RawInput::default());
        let out = run(&mut buf, egui::RawInput::default());

        let mut xs = find_all_text_pos(&out.shapes, "✕");
        xs.sort_by(|a, b| a.y.partial_cmp(&b.y).unwrap());
        assert_eq!(xs.len(), 3, "三跳应该各有一个「✕」按钮");
        let middle = xs[1];

        let _ = run(
            &mut buf,
            egui::RawInput {
                events: vec![
                    egui::Event::PointerMoved(middle),
                    click(middle, true),
                    click(middle, false),
                ],
                ..Default::default()
            },
        );
        assert_eq!(
            buf.jump_chain,
            vec![SessionId(2), SessionId(4)],
            "点第二跳的「✕」应只删掉第二跳;实际 {:?}",
            buf.jump_chain
        );
    }
}
