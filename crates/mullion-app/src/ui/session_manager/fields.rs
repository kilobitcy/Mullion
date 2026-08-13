//! 右栏四个 Tab 的字段布局。从 `editor.rs` 切出来是因为字段多、改动频繁,
//! 混在窗口骨架里会让 `editor.rs` 涨到读不动。

use egui::Ui;
use mullion_store::{GroupRecord, Protocol, SessionId, SessionRecord, TmuxChoice};

use crate::theme::Theme;
use crate::ui::metrics::{
    button_reserve, field_w, FIELD_W_L, FIELD_W_M, FIELD_W_S, TEXT_EDIT_MARGIN_X,
};
use crate::ui::session_manager::form::{field_error, grid, required, section};
use crate::ui::session_manager::inherit_row::{self, Source};
use crate::ui::session_manager::{
    AuthKindUi, CredSourceUi, EditorBuffer, JumpModeUi, ProxyModeUi, SecretPresence,
};

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

/// 可选毫秒数:勾选框 + `DragValue`。未勾选时显示**实际会生效的值和来源** ——
/// 光给一个空框,用户不知道不填会发生什么。
///
/// `line` 由调用方算好传进来(见 `resolve_u32`):旧版在这里写死
/// 「继承(内置默认 {default} ms)」,而分组**可以**配这三个字段,那时候
/// 这句话是错的 —— 用户会以为改分组不管用(走查 10)。
///
/// `min` 不是摆设:两个「延时」类字段填 0 就是「不等」,语义正常;而「就绪超时」
/// 填 0 意味着 `run()` 的 `sleep(0)` 必然抢在首字节前面,自动化每次都被跳过,
/// 状态栏还会打出「自动化已跳过:0s 未收到远端输出」—— `status_text` 那里
/// 特意用 `div_ceil` 就是为了永不出现「0s」(见 `sub_second_timeout_rounds_up_
/// so_it_never_reads_zero`),但 `div_ceil` 拦不住字面 0。用下界从源头挡掉。
#[allow(clippy::too_many_arguments)]
fn opt_ms(
    ui: &mut Ui,
    t: &Theme,
    id: &str,
    v: &mut Option<u32>,
    default: u32,
    min: u32,
    max: u32,
    line: Option<String>,
) {
    // `push_id` 而不是 `let _ = id;` —— 三个延时框长得一模一样,勾选框的 id 靠
    // 位置生成。一旦某一行因为条件渲染消失,后面几行的位置 id 会整体前移,
    // egui 会把上一行的勾选状态套到下一行上。给个显式 salt 钉死。
    ui.push_id(id, |ui| {
        // 这一行**不用** `horizontal_wrapped`:后面跟的是纯灰字,窄栏被裁掉
        // 只损失可读性;而 wrap 会把这一行的布局收敛推迟一帧,让按「提示文字
        // 位置反推复选框坐标」的守护测试(`checking_each_delay_box_...`)打空。
        // 阶段 1 换 `wrapped` 的那几处后面跟的都是**按钮**——被裁掉等于功能
        // 不可达,那才值得换。
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
                    if let Some(s) = line {
                        ui.colored_label(crate::theme::c32(t.fg_dimmer), s);
                    }
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

/// F43:env 区的常驻说明。**这是事实陈述,不是警告**——走查 18:原来这里是
/// 一整块 `warn` 描边横幅,不管有没有风险都常驻。天天见的警告等于没有警告,
/// 用户学会跳过它,真出事那次也一起跳过。所以事实降成灰字,风险交给
/// `env_hint::secret_warning` —— 只在变量名真的像密码时才升回红框。
const ENV_NOTE: &str = "变量值以明文存进 sessions.toml,并会以 export 行发到远端。";

/// 「登录后」页上游分组的快照:分组名 + 它的 automation 分节。
///
/// 拷出来而不是持 `&GroupRecord`:下面整段都持着 `&mut buf.preserved_automation`,
/// 而 `buf.preserved_group_id` 的读取必须先于那个可变借用。
type AutoUpstream = Option<(String, mullion_store::AutomationPrefs)>;

/// 标量继承的取值 + 来源。会话侧已经是 `None`(继承)时才调用。
///
/// 层序只有一级(分组),所以不需要 `inherit::resolve` 那套通用机制 ——
/// 但**取值规则必须和它一致**:分组有值就用分组的,否则内置默认。
/// 不一致的话,UI 显示的「实际生效」和真正连上去用的值会不同,
/// 这比不显示更坏。
fn resolve_bool<'a>(
    up: Option<&'a (String, mullion_store::AutomationPrefs)>,
    pick: impl Fn(&mullion_store::AutomationPrefs) -> Option<bool>,
    builtin: bool,
) -> (bool, Source<'a>) {
    match up {
        Some((name, prefs)) => match pick(prefs) {
            Some(v) => (v, Source::Group(name)),
            None => (builtin, Source::Builtin),
        },
        // 未分组 = 没东西可继承 = 落内置默认。**不用** `NoUpstream`:
        // 那个变体是给跳板/代理的,那里「没有上游」意味着「实际等同于无」,
        // 是用户需要知道的原因;标量字段没有分组照样有内置默认值,说
        // 「未分组,没有上游可继承」会让用户反问「那 300ms 哪来的」。
        None => (builtin, Source::Builtin),
    }
}

/// 同 `resolve_bool`,`u32` 版。两个函数不合并成泛型:泛型版要么带上
/// `T: Copy` 约束再让调用方写 turbofish,要么就得给 `String` 也开个口子 ——
/// 两行重复换来调用点全部无标注,划算。
fn resolve_u32<'a>(
    up: Option<&'a (String, mullion_store::AutomationPrefs)>,
    pick: impl Fn(&mullion_store::AutomationPrefs) -> Option<u32>,
    builtin: u32,
) -> (u32, Source<'a>) {
    match up {
        Some((name, prefs)) => match pick(prefs) {
            Some(v) => (v, Source::Group(name)),
            None => (builtin, Source::Builtin),
        },
        // 未分组 = 没东西可继承 = 落内置默认。**不用** `NoUpstream`:
        // 那个变体是给跳板/代理的,那里「没有上游」意味着「实际等同于无」,
        // 是用户需要知道的原因;标量字段没有分组照样有内置默认值,说
        // 「未分组,没有上游可继承」会让用户反问「那 300ms 哪来的」。
        None => (builtin, Source::Builtin),
    }
}

/// `focus_name`:走查 21 的一次性聚焦标志,由调用方 `take()` 后传进来 ——
/// 刚点「+ 新建」的那一帧把光标放进「名称」框,省用户一次点击。
#[allow(clippy::too_many_arguments)]
pub(super) fn basic(
    ui: &mut Ui,
    t: &Theme,
    buf: &mut EditorBuffer,
    groups: &[GroupRecord],
    sessions: &[SessionRecord],
    credentials: &[mullion_store::CredentialRecord],
    editing: Option<SessionId>,
    presence: SecretPresence,
    focus_name: bool,
    touched: &mut super::validate::Touched,
) {
    // 页面级游标:整个「连接」页(基本/归类/代理/跳板)共用一个,
    // 见 `section()` 文档注释。
    let mut first = true;
    section(ui, t, "会话管理器/右栏", "基本", &mut first);
    grid(ui, "sm_basic", |ui| {
        required(ui, t, "名称");
        let name_resp = ui.add(
            egui::TextEdit::singleline(&mut buf.name).desired_width(field_w(
                ui.available_width(),
                FIELD_W_M,
                0.0,
            )),
        );
        if focus_name {
            name_resp.request_focus();
        }
        touched.name |= name_resp.lost_focus();
        ui.end_row();
        field_error(
            ui,
            t,
            touched.name && buf.name.trim().is_empty(),
            "会话名称不能为空",
        );

        required(ui, t, "主机");
        let host_resp = ui.add(
            egui::TextEdit::singleline(&mut buf.host).desired_width(field_w(
                ui.available_width(),
                FIELD_W_M,
                0.0,
            )),
        );
        touched.host |= host_resp.lost_focus();
        ui.end_row();
        field_error(
            ui,
            t,
            touched.host && buf.host.trim().is_empty(),
            "主机不能为空",
        );

        ui.label("端口");
        let port_resp = ui.add(
            egui::TextEdit::singleline(&mut buf.port)
                .hint_text("22")
                .desired_width(field_w(ui.available_width(), FIELD_W_S, 0.0)),
        );
        touched.port |= port_resp.lost_focus();
        ui.end_row();
        // 端口的红字不看 `touched`:一个填错的端口就是填错了,跟用户填到
        // 哪一步无关;而空端口是合法的(落默认 22),压根没有红字可出。
        field_error(
            ui,
            t,
            super::validate::port(&buf.port).is_err(),
            "端口要填 1~65535 之间的数字",
        );

        ui.label("协议");
        // D3:只读。可改的话,一条记录会在保存那一刻从当前列表消失、跑到另一
        // 页去,而引用它的隧道会当场变成「经由一个 SFTP 节点」——那条隧道
        // 昨天还是好的。要换协议就新建一条(同 D1 接受的代价)。
        //
        // 原来这里是个能选 sftp 的下拉 + 一句「sftp 尚未实现,连接会按 ssh
        // 处理」。SFTP 节点有了自己的一档(F118),D1 起同一套映射会给它算出
        // 拨号参数(带 `wants_sftp`),这个下拉连同那句话一起下线。
        ui.label(match buf.protocol {
            Protocol::Ssh => "ssh",
            Protocol::Sftp => "sftp",
        });
        ui.end_row();
    });

    section(ui, t, "会话管理器/右栏", "归类", &mut first);
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

        // 走查 6:标签终于有编辑入口了。`identity.tags` 一直在 schema 里、
        // 一直被搜索命中、一直参与分组 Merge 继承 —— 唯独填不进去,而搜索框
        // 的占位符还写着「搜索名称 / 主机 / 标签」,承诺了一个填不了的东西。
        ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
            ui.label("标签");
        });
        ui.vertical(|ui| {
            let resp = ui.add(
                egui::TextEdit::singleline(&mut buf.tag_input)
                    .hint_text(crate::theme::hint_text(t, "回车添加,逗号或空格分隔"))
                    .desired_width(field_w(ui.available_width(), FIELD_W_L, 0.0)),
            );
            // 回车确认。`lost_focus()` 单独判会把「点到别处」也当成确认 ——
            // 那样用户点一下别的字段,半截输入就被当成标签存下了。
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                super::tags::merge_into(&mut buf.preserved_tags, &buf.tag_input);
                buf.tag_input.clear();
                resp.request_focus(); // 连着加几个标签时不用每次重新点输入框
            }
            if !buf.preserved_tags.is_empty() {
                ui.add_space(crate::ui::metrics::SP_XS);
                let mut remove: Option<usize> = None;
                ui.horizontal_wrapped(|ui| {
                    for (i, tag) in buf.preserved_tags.iter().enumerate() {
                        // id 必须 salt 到 `(索引, 文本)`:两个 chips 文字相同
                        // (大小写不同,去重放行)时,只按文本 salt 会撞 id,
                        // 点一个 ✕ 删掉另一个。
                        let id = ui.id().with(("sm_tag_chip", i, tag.as_str()));
                        ui.push_id(id, |ui| {
                            egui::Frame::none()
                                .fill(crate::theme::c32(t.sunken_bg))
                                .rounding(10.0)
                                .inner_margin(egui::Margin::symmetric(8.0, 2.0))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.colored_label(crate::theme::c32(t.fg_dim), tag);
                                        if ui
                                            .add(egui::Button::new("✕").frame(false).small())
                                            .clicked()
                                        {
                                            remove = Some(i);
                                        }
                                    });
                                });
                        });
                    }
                });
                if let Some(i) = remove {
                    buf.preserved_tags.remove(i);
                }
            }
        });
        ui.end_row();

        // 备注顶对齐:Grid 每行默认 `Align::Center`,3 行高的 multiline
        // 旁边的短标签会被垂直居中,跟上面几行的标签对不齐(走查 P2-17)。
        ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
            ui.label("备注");
        });
        ui.add(
            egui::TextEdit::multiline(&mut buf.note)
                .desired_rows(3)
                .desired_width(field_w(ui.available_width(), FIELD_W_L, 0.0)),
        );
        ui.end_row();
    });

    // 代理排在跳板之前:连接路径是 本机 →(代理)→ 第一跳 →…→ 目标主机,
    // 页面自上而下按这个顺序读得通。走查 P1-8 把原「高级」页并到这里 ——
    // 那一页只有一行代理,右侧 70% 是空白。
    network(ui, t, buf, groups, presence, &mut first);

    jump(
        ui,
        t,
        buf,
        groups,
        sessions,
        credentials,
        editing,
        &mut first,
    );
}

/// `Color32` → `#rrggbb`。色盘吐的是 `Color32`,库里存的是 hex 文本,
/// 两个方向都只有一份实现(反方向是 `theme::parse_hex`)。
pub(super) fn hex_of(c: egui::Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

/// 图标在列表实际取图尺寸下的实时预览。
///
/// 只画 32px 一档:三档列表现在都按 32 取图,继续预览 64 是在骗人 ——
/// 而「小尺寸下还认不认得出」正是选图标时唯一要判断的事。
fn icon_preview(
    ui: &mut Ui,
    t: &Theme,
    icon: Option<&mullion_store::IconSpec>,
    bg: Option<egui::Color32>,
) {
    let Some(icon) = icon else { return };
    let size = crate::ui::ico::SMALL;
    ui.horizontal(|ui| {
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(size as f32, size as f32), egui::Sense::hover());
        crate::ui::badge::paint_icon(ui.painter(), rect, icon, bg);
        ui.label(
            egui::RichText::new(format!("{size}px"))
                .size(11.0)
                .color(crate::theme::c32(t.fg_muted)),
        );
    });
}

/// F61/F62 外观。独占一个 Tab(`TAB_APPEARANCE`,排在「登录后」之后)。
///
/// 原先塞在「连接」页「归类」之后,实机验收时用户要求单开一页 —— 图标和颜色
/// 是一组独立的视觉设置,跟主机/端口不是同一个决策。
pub(crate) fn appearance(
    ui: &mut Ui,
    t: &Theme,
    buf: &mut EditorBuffer,
    icon_error: &mut Option<String>,
) {
    use mullion_store::{ColorSpec, ColorTarget, IconKind};

    let mut first = true;
    section(ui, t, "会话管理器/右栏", "外观", &mut first);
    grid(ui, "sm_basic_appearance", |ui| {
        ui.label("图标");
        ui.vertical(|ui| {
            // 状态直接读 `preserved_appearance.icon`,**没有独立的模式位**。
            // v0.1.23~v0.1.25 那个模式位是 emoji 边打边存逼出来的(缓冲空的
            // 那一瞬会被反推成「没图标」,UI 当场弹回去);导入 .ico 没有中间态,
            // 选完就有值,不需要它。
            //
            // 先把要看的几件事**取成 owned 的局部量**再动手改:后面几段都要
            // `as_mut()`,留着一份 `as_ref()` 会直接撞借用检查。
            let kind = buf.preserved_appearance.icon.as_ref().map(|i| i.kind);
            let has_ico = kind == Some(IconKind::Ico);

            ui.horizontal_wrapped(|ui| {
                if ui.button("导入 .ico…").clicked() {
                    buf.pick_icon_clicked = true;
                }
                if kind.is_some() && ui.button("清除").clicked() {
                    buf.preserved_appearance.icon = None;
                    *icon_error = None;
                }
            });

            // 老库里的 emoji/内置图标:UI 不再支持编辑,但**数据原样留着** ——
            // 清掉是用户的决定,不是我们的。只是得说清它为什么不显示了,
            // 否则看起来就是个 bug。
            if let Some(old) = kind.filter(|k| *k != IconKind::Ico) {
                ui.colored_label(
                    crate::theme::c32(t.warn),
                    match old {
                        // emoji 不是「不想支持」,是 epaint 画不了彩色字形 ——
                        // 画出来是黑白剪影,64px 纯图标档下认不出哪台是哪台。
                        IconKind::Emoji => {
                            "这条会话存的还是旧的 emoji 图标,已不再显示 —— 导入一个 .ico 换掉它"
                        }
                        _ => "这条会话存的是旧版本的图标,已不再显示 —— 导入一个 .ico 换掉它",
                    },
                );
            }

            if let Some(err) = icon_error.as_ref() {
                ui.colored_label(crate::theme::c32(t.danger), err);
            }

            if has_ico {
                // 预览:按列表真实取图的尺寸画一次。不当场给看,用户得先存、
                // 再去拖列表才知道自己选的图标在小尺寸下还认不认得出。
                //
                // 底色跟节点色走(过 `ListItem` 闸门),与列表行同源 —— 只勾了
                // 「pane 标题条」时预览也不垫底,预览的是列表里的真实效果,
                // 不是理想效果。
                icon_preview(
                    ui,
                    t,
                    buf.preserved_appearance.icon.as_ref(),
                    crate::ui::badge::color_rgb(
                        buf.preserved_appearance.color.as_ref(),
                        ColorTarget::ListItem,
                    )
                    .map(crate::theme::c32),
                );
            }
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

                // 预设之外还要能自由取色(用户要求)。预设**留着**:常用的
                // 几个语义色一键就点到,进色盘里再调一次是白费事。
                let mut picked = buf
                    .preserved_appearance
                    .color
                    .as_ref()
                    .and_then(|c| crate::theme::parse_hex(&c.hex))
                    .map_or(egui::Color32::WHITE, crate::theme::c32);
                if egui::color_picker::color_edit_button_srgba(
                    ui,
                    &mut picked,
                    egui::color_picker::Alpha::Opaque,
                )
                .changed()
                {
                    let hex = hex_of(picked);
                    match &mut buf.preserved_appearance.color {
                        Some(c) => c.hex = hex,
                        // 从色盘首次设色,落点与点预设色块时一致 —— 两条路
                        // 设出来的东西必须是同一个,否则「点色块有效、用色盘
                        // 没效果」。
                        None => {
                            buf.preserved_appearance.color = Some(ColorSpec {
                                hex,
                                apply_to: vec![ColorTarget::ListItem, ColorTarget::PaneTitle],
                            })
                        }
                    }
                }

                if ui.button("清除").clicked() {
                    buf.preserved_appearance.color = None;
                }
            });

            if let Some(c) = &mut buf.preserved_appearance.color {
                // 后面可能跟一条长警告文字,窄栏下必须允许它折行。
                ui.horizontal_wrapped(|ui| {
                    ui.label("自定义");
                    ui.add(
                        egui::TextEdit::singleline(&mut c.hex)
                            // `#rrggbb` 是定长短值,归短值档。
                            .desired_width(field_w(ui.available_width(), FIELD_W_S, 0.0))
                            .hint_text(crate::theme::hint_text(t, "#rrggbb")),
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
                    (ColorTarget::Tab, "标签页"),
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

    // 走查 4:「竖条如果是『图标颜色』的体现,就和『图标』页的颜色设置对应上,
    // 并在图标页加实时预览」。竖条本来就同源(两边都走
    // `badge::should_paint(ColorTarget::ListItem)`),缺的只是让用户当场看见。
    section(ui, t, "会话管理器/右栏", "预览", &mut first);
    let preview = crate::ui::badge::Appearance {
        icon: buf.preserved_appearance.icon.clone(),
        color: buf.preserved_appearance.color.clone(),
    };
    let name = if buf.name.trim().is_empty() {
        "会话名称"
    } else {
        buf.name.trim()
    };
    let host = if buf.host.trim().is_empty() {
        "host"
    } else {
        buf.host.trim()
    };
    let user = if buf.user.trim().is_empty() {
        "user"
    } else {
        buf.user.trim()
    };
    crate::ui::session_manager::list::preview_row(
        ui,
        t,
        name,
        &format!("{user}@{host}"),
        buf.protocol,
        &preview,
    );
    ui.colored_label(
        crate::theme::c32(t.fg_dimmer),
        "左栏列表里就是这个样子。选中时的背景色和右侧竖条,只在「作用于」勾了「会话列表」时出现;图标背景色则跟各个落点各自的勾选走 —— 勾「会话列表」在左栏垫底,勾「pane 标题条」在终端标题栏垫底。",
    );
}

/// F5 跳板链。放在**「连接」页**而不是「高级」页:跳板回答的是「怎么走到这台
/// 机器」,和主机/端口是同一个决策,配完主机紧接着就要配它;埋进「高级」里
/// 用户根本找不到。
///
/// 三态与 `NetworkPrefs::jump` 一一对应,见 `JumpModeUi` 的说明。
// 同上:内部叶子函数,为压参数数引一个专用结构体没有实际收益。
#[allow(clippy::too_many_arguments)]
fn jump(
    ui: &mut Ui,
    t: &Theme,
    buf: &mut EditorBuffer,
    groups: &[GroupRecord],
    sessions: &[SessionRecord],
    credentials: &[mullion_store::CredentialRecord],
    editing: Option<SessionId>,
    first: &mut bool,
) {
    section(ui, t, "会话管理器/右栏", "跳板", first);
    grid(ui, "sm_basic_jump", |ui| {
        ui.label("跳板");
        // 「继承分组」→「继承」:同一件事全项目一个说法(走查 19)。
        // 来源不写进按钮文字,交给右边那行灰字 —— 未分组时上游根本不是
        // 「分组」,按钮上写死「分组」就是错的。
        let line = matches!(buf.jump_mode, JumpModeUi::Inherit).then(|| {
            let (v, src) = match inherit_row::upstream(buf.preserved_group_id, groups) {
                Some(g) => match &g.network.jump {
                    Some(chain) if !chain.is_empty() => {
                        (format!("{} 跳", chain.len()), Source::Group(&g.name))
                    }
                    // 分组自己也没配(`None` 或显式空链)→ 继承下来还是不走跳板。
                    _ => ("不走跳板".to_string(), Source::Group(&g.name)),
                },
                // 这里才是 `NoUpstream` 真正该出现的地方:用户选了「继承」,
                // 而根本没有上游 —— 结果等同于「无」,这个原因他需要知道。
                None => ("不走跳板".to_string(), Source::NoUpstream),
            };
            inherit_row::effective_line(&v, src)
        });
        inherit_row::slot(
            ui,
            t,
            |ui| {
                // 与「认证方式」同样的选中态处理:egui 默认的 gamma_multiply 底色
                // 在深色面板上偏暗,分不出选中态。见 `auth()` 里那段注释。
                let vis = &mut ui.visuals_mut().selection;
                vis.bg_fill = crate::theme::c32(t.accent).linear_multiply(0.35);
                ui.selectable_value(&mut buf.jump_mode, JumpModeUi::None, "无");
                ui.selectable_value(&mut buf.jump_mode, JumpModeUi::Inherit, "继承");
                ui.selectable_value(&mut buf.jump_mode, JumpModeUi::Custom, "自定义");
            },
            line,
        );
        ui.end_row();

        match buf.jump_mode {
            // 「无」是默认态,不需要任何解释性文字;「继承」的说明已经跟在
            // 模式按钮右边了。
            JumpModeUi::None | JumpModeUi::Inherit => {}
            JumpModeUi::Custom => {
                ui.label("跳板链");
                ui.vertical(|ui| chain_editor(ui, t, buf, sessions, credentials, editing));
                ui.end_row();
            }
        }
    });
}

/// 自定义跳板链的逐跳编辑。按拨号顺序,`[0]` 最先连。
fn chain_editor(
    ui: &mut Ui,
    t: &Theme,
    buf: &mut EditorBuffer,
    sessions: &[SessionRecord],
    credentials: &[mullion_store::CredentialRecord],
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
                            format!(
                                "{}@{}",
                                mullion_store::display_user(&s.auth, credentials),
                                s.connection.host
                            ),
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
                    use crate::ui::icon::{icon_button, Glyph};
                    // 自绘而不是打字:U+2715 / U+2191 / U+2193 都不在 egui
                    // 内置拉丁字体和微软雅黑里,实机上全渲染成豆腐块 □
                    // (走查 P0-5 报的「□ 完全看不出是删除」)。
                    // tooltip 是 `icon_button` 的必填参数,忘不了。
                    if icon_button(ui, Glyph::Cross, true, "移除此跳板") {
                        remove = Some(i);
                    }
                    if icon_button(ui, Glyph::ArrowDown, i + 1 < len, "下移") {
                        swap = Some((i, i + 1));
                    }
                    if icon_button(ui, Glyph::ArrowUp, i > 0, "上移") {
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
                    s.identity.name,
                    mullion_store::display_user(&s.auth, credentials),
                    s.connection.host
                );
                if ui.selectable_label(false, label).clicked() {
                    add = Some(s.id);
                }
            }
        });

    // 走查 12:配好一条三跳的链,界面上只是三行会话名,看不出最终的连接
    // 路径长什么样;而环 / 自引用 / 悬空要等到真正拨号才报错。这两行把
    // `mullion_store::jump` 的既有判据提前到编辑时。
    if !buf.jump_chain.is_empty() {
        // 代理只在**显式**选了 SOCKS5/HTTP 时标进路径。「继承」态下上游代理
        // 是什么这里查不到(`chain_editor` 拿不到 groups),宁可不画也不猜 ——
        // 画错的连接路径比不画更糟。
        let proxy = match buf.proxy_mode {
            ProxyModeUi::Socks5 => Some("SOCKS5"),
            ProxyModeUi::HttpConnect => Some("HTTP"),
            ProxyModeUi::Inherit | ProxyModeUi::Direct => None,
        };
        // 目标用名称,没填名称就退回主机 —— 新建会话时往往先填主机。
        let target = if buf.name.trim().is_empty() {
            buf.host.trim()
        } else {
            buf.name.trim()
        };
        let target = if target.is_empty() {
            "本会话"
        } else {
            target
        };
        ui.colored_label(
            crate::theme::c32(t.fg_dimmer),
            super::jump_preview::preview(&buf.jump_chain, sessions, proxy, target),
        );
        if let Some(msg) = super::jump_preview::check(&buf.jump_chain, sessions) {
            ui.colored_label(crate::theme::c32(t.danger), msg);
        }
    }

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

/// 引用共享凭据时贴在「身份」节下面的一句话。
///
/// 必须说清「改的是这份凭据本身、会连带影响别的会话」—— 共享凭据的全部价值
/// 是「换密钥改一处」,同一枚硬币的背面就是「改一处影响多处」。不写这句,
/// 用户会把它当成一个「填起来更省事的用户名」,直到某天改一份凭据连带把
/// 五台机器的登录改掉才发现。
pub(super) const SHARED_CREDENTIAL_NOTE: &str =
    "用户名与密码属于这份凭据本身;要改请去「凭据」页 —— 改动会作用到所有引用它的会话。";

#[allow(clippy::too_many_arguments)] // 表单页函数,参数就是页面要画的那些东西
pub(super) fn auth(
    ui: &mut Ui,
    t: &Theme,
    buf: &mut EditorBuffer,
    presence: SecretPresence,
    key_candidates: &[std::path::PathBuf],
    credentials: &[mullion_store::CredentialRecord],
    touched: &mut super::validate::Touched,
) {
    let mut first = true;
    section(ui, t, "会话管理器/右栏", "身份", &mut first);
    grid(ui, "sm_auth", |ui| {
        // 来源开关放在「身份」节顶上、两档都画:它是这一页的分岔口,
        // 藏进任何一档里都会让用户切过去之后找不到切回来的路。
        ui.label("凭据来源");
        ui.horizontal(|ui| {
            let vis = &mut ui.visuals_mut().selection;
            vis.bg_fill = crate::theme::c32(t.accent).linear_multiply(0.35);
            ui.selectable_value(&mut buf.cred_source, CredSourceUi::Own, "本会话独有");
            ui.selectable_value(&mut buf.cred_source, CredSourceUi::Shared, "共享凭据");
        });
        ui.end_row();

        if buf.cred_source == CredSourceUi::Shared {
            // 共享档:用户名/认证方式/密码整块都不画。**不是禁用而是不画** ——
            // 严格二选一(设计 D1),画一组灰着的输入框等于在暗示「这里还有一份
            // 会话自己的身份」,而那正是本功能要消灭的东西。
            required(ui, t, "凭据");
            credential_combo(ui, buf, credentials);
            ui.end_row();
            field_error(ui, t, buf.credential_id.is_none(), "请选一份共享凭据");

            ui.label("这份凭据");
            let (text, color) = credential_summary(buf.credential_id, credentials, t);
            ui.colored_label(crate::theme::c32(color), text);
            ui.end_row();
            return;
        }

        required(ui, t, "用户名");
        let user_resp = ui.add(
            egui::TextEdit::singleline(&mut buf.user).desired_width(field_w(
                ui.available_width(),
                FIELD_W_M,
                0.0,
            )),
        );
        touched.user |= user_resp.lost_focus();
        ui.end_row();
        field_error(
            ui,
            t,
            touched.user && buf.user.trim().is_empty(),
            "用户名不能为空",
        );

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

    if buf.cred_source == CredSourceUi::Shared {
        ui.add_space(crate::ui::metrics::SP_XS);
        ui.label(
            egui::RichText::new(SHARED_CREDENTIAL_NOTE)
                .size(11.0)
                .color(crate::theme::c32(t.fg_muted)),
        );
        return;
    }

    section(ui, t, "会话管理器/右栏", "凭据", &mut first);
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

    // 走查 20:说清楚这些东西存哪儿、怎么护着。用户手里是一台随时可能被
    // 别人碰到的 Windows 机器,「这密码到底存哪了」是个合理且必须回答的问题
    // —— 不回答,谨慎的人就干脆不用这个功能,每次连都手敲。
    //
    // 这段话必须与真实实现严格一致(`mullion-store`:`secrets.enc` +
    // XChaCha20-Poly1305,主密钥进 Windows 凭据管理器)。UI 上写一句比实现
    // 更好听的安全承诺,比不写更糟。
    ui.add_space(crate::ui::metrics::SP_XS);
    ui.label(
        egui::RichText::new(SECRET_STORAGE_NOTE)
            .size(11.0)
            .color(crate::theme::c32(t.fg_muted)),
    );
}

/// 凭据存储的说明文案。抽成常量是为了让守护测试能直接钉住它 —— 尤其是
/// 「不许出现『明文』二字」这条:凭据**不是**明文存的,写错就是在自证清白
/// 的地方栽赃自己。
pub(super) const SECRET_STORAGE_NOTE: &str =
    "密码与私钥经 XChaCha20-Poly1305 加密后存进 secrets.enc,主密钥交给 Windows 凭据管理器保管。";

/// 共享凭据下拉。抽成独立函数并**返回 `Response`**,理由同 `key_candidate_combo`:
/// 让守护测试扎在生产代码上,而不是在测试里另起一份同构的 ComboBox。
///
/// 空库时禁用并说明去处 —— 一个点了没反应的下拉,用户只会以为程序卡了。
pub(super) fn credential_combo(
    ui: &mut Ui,
    buf: &mut EditorBuffer,
    credentials: &[mullion_store::CredentialRecord],
) -> egui::Response {
    let has_any = !credentials.is_empty();
    let selected = buf
        .credential_id
        .and_then(|id| credentials.iter().find(|c| c.id == id))
        .map_or_else(|| "请选择…".to_string(), |c| c.name.clone());
    let w = field_w(ui.available_width(), FIELD_W_M, 0.0);
    ui.add_enabled_ui(has_any, |ui| {
        egui::ComboBox::from_id_salt("sm_credential")
            .width(w)
            .selected_text(selected)
            .show_ui(ui, |ui| {
                for c in credentials {
                    // 下拉项带上用户名:凭据名是用户自己起的,「运维号」这种
                    // 名字光看名字分不出是 root 还是 ops。
                    let label = format!("{} —— {}", c.name, c.user);
                    let picked = Some(c.id) == buf.credential_id;
                    if ui.selectable_label(picked, label).clicked() {
                        buf.credential_id = Some(c.id);
                    }
                }
            });
    })
    .response
    .on_disabled_hover_text("凭据库是空的 —— 先去「凭据」页新建一份")
}

/// 选中凭据的只读摘要 `(文案, 颜色)`。
///
/// 悬空(引用的凭据已被删)必须**标红说明**,不能画成空白:这条会话拨号时
/// 是硬失败的(设计 D6),界面上装作没事,用户只会在连接失败时才发现。
fn credential_summary(
    id: Option<mullion_store::CredentialId>,
    credentials: &[mullion_store::CredentialRecord],
    t: &Theme,
) -> (String, mullion_term::snapshot::Rgb) {
    let Some(id) = id else {
        return ("尚未选择".to_string(), t.fg_muted);
    };
    match credentials.iter().find(|c| c.id == id) {
        Some(c) => {
            let kind = match c.kind {
                mullion_store::AuthKind::Password => "密码",
                mullion_store::AuthKind::PublicKey { .. } => "公钥",
            };
            (format!("{}(以 {} 认证)", c.user, kind), t.fg)
        }
        None => ("引用的凭据已被删除 —— 请重新选一份".to_string(), t.danger),
    }
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

pub(super) fn network(
    ui: &mut Ui,
    t: &Theme,
    buf: &mut EditorBuffer,
    groups: &[GroupRecord],
    presence: SecretPresence,
    first: &mut bool,
) {
    section(ui, t, "会话管理器/右栏", "代理", first);
    grid(ui, "sm_net_proxy", |ui| {
        ui.label("代理");
        let line = matches!(buf.proxy_mode, ProxyModeUi::Inherit).then(|| {
            let (v, src) = match inherit_row::upstream(buf.preserved_group_id, groups) {
                Some(g) => {
                    let v = match &g.network.proxy {
                        Some(mullion_store::ProxyChoice::Direct) | None => "直连".to_string(),
                        Some(mullion_store::ProxyChoice::Socks5(e)) => {
                            format!("SOCKS5 {}:{}", e.host, e.port)
                        }
                        Some(mullion_store::ProxyChoice::HttpConnect(e)) => {
                            format!("HTTP {}:{}", e.host, e.port)
                        }
                    };
                    (v, Source::Group(&g.name))
                }
                None => ("直连".to_string(), Source::NoUpstream),
            };
            inherit_row::effective_line(&v, src)
        });
        // 窄栏放不下四个模式按钮:`ui.horizontal` 不换行会把这一格撑宽 8px,
        // 顶出面板(「跳板」分区的分隔线因此画到面板外)—— 走查 P0-1 同族缺陷。
        // `slot` 外层已经是 `horizontal_wrapped`,这里不用再套一层。
        inherit_row::slot(
            ui,
            t,
            |ui| {
                ui.selectable_value(&mut buf.proxy_mode, ProxyModeUi::Inherit, "继承");
                ui.selectable_value(&mut buf.proxy_mode, ProxyModeUi::Direct, "直连");
                ui.selectable_value(&mut buf.proxy_mode, ProxyModeUi::Socks5, "SOCKS5");
                ui.selectable_value(&mut buf.proxy_mode, ProxyModeUi::HttpConnect, "HTTP");
            },
            line,
        );
        ui.end_row();

        if matches!(
            buf.proxy_mode,
            ProxyModeUi::Socks5 | ProxyModeUi::HttpConnect
        ) {
            ui.label("代理地址");
            ui.horizontal(|ui| {
                // 端口跟在同一行,先给它留位置再算主机框 —— 否则主机框
                // 吃光整行,端口框被顶出去(走查 P0-1 的同型缺陷)。
                // `+ TEXT_EDIT_MARGIN_X` 的理由同 `secret_edit` 里「撤销」
                // 预留那段注释:`TextEdit` 默认内边距 `Margin::symmetric(4.0,
                // 2.0)` 会让端口框自己的外框比它的 `desired_width` 多出 8px,
                // 不算进主机框的预留,端口框照样会被顶出去 8px。
                let reserve = FIELD_W_S
                    + crate::ui::metrics::TEXT_EDIT_MARGIN_X
                    + ui.spacing().item_spacing.x;
                ui.add(
                    egui::TextEdit::singleline(&mut buf.proxy_host).desired_width(field_w(
                        ui.available_width(),
                        FIELD_W_M,
                        reserve,
                    )),
                );
                // 跟 `basic()` 里的「端口」字段用同一套写法(`field_w` + 下界
                // 保护),不要裸写 `desired_width(FIELD_W_S)` —— 同语义字段
                // 两套写法正是走查 P0-2 想根治的问题,裸值也没有下界夹护。
                ui.add(
                    egui::TextEdit::singleline(&mut buf.proxy_port).desired_width(field_w(
                        ui.available_width(),
                        FIELD_W_S,
                        0.0,
                    )),
                );
            });
            ui.end_row();

            ui.label("代理用户");
            ui.add(
                egui::TextEdit::singleline(&mut buf.proxy_user).desired_width(field_w(
                    ui.available_width(),
                    FIELD_W_M,
                    0.0,
                )),
            );
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
///
/// `groups` 是走查 10 加进来的:这一页有五个标量字段能选「继承」,而
/// 「继承到了什么」只有看得见分组才算得出来。
pub(super) fn automation(ui: &mut Ui, t: &Theme, buf: &mut EditorBuffer, groups: &[GroupRecord]) {
    // tmux 会话名留空时的推导结果,作 placeholder 实时显示。先算好再借
    // `buf.preserved_automation`,后面几节就不用反复回头碰 `buf` 了。
    let derived = mullion_store::automation::sanitize_tmux_name(&buf.name);
    // 上游只解析一次:这一页有七个字段要问它。`upstream` 是线性查找,
    // 分组数量级是个位数,但每帧七次仍然没必要(本项目陷阱 T3)。
    let up: AutoUpstream = inherit_row::upstream(buf.preserved_group_id, groups)
        .map(|g| (g.name.clone(), g.automation.clone()));
    let a = &mut buf.preserved_automation;

    // 警告置顶:滚到页面底部才看到「会打进 TUI 输入框」就太晚了。
    if a.commands.iter().flatten().any(|c| c.delay_ms.is_some()) {
        warn_banner(ui, t, DELAY_WARNING);
        ui.add_space(crate::ui::metrics::SP_S);
    }

    let mut first = true;
    section(ui, t, "会话管理器/右栏", "总开关", &mut first);
    grid(ui, "sm_auto_enabled", |ui| {
        ui.label("登录后自动化");
        // 选了「继承」才画生效值 —— 显式选了「开」的人不需要被告知「实际生效:开」。
        let line = a.enabled.is_none().then(|| {
            let (v, src) = resolve_bool(
                up.as_ref(),
                |p| p.enabled,
                mullion_store::automation::DEFAULT_AUTOMATION_ENABLED,
            );
            inherit_row::effective_line(if v { "开" } else { "关" }, src)
        });
        inherit_row::slot(
            ui,
            t,
            |ui| {
                tri_state(ui, "sm_auto_enabled_combo", &mut a.enabled, "开", "关");
            },
            line,
        );
        ui.end_row();
    });

    section(ui, t, "会话管理器/右栏", "tmux", &mut first);
    grid(ui, "sm_auto_tmux", |ui| {
        ui.label("连上后");
        let text = match &a.tmux {
            None => "继承",
            Some(TmuxChoice::Off) => "不用 tmux",
            Some(TmuxChoice::Attach { .. }) => "自动 attach",
        };
        let line = a.tmux.is_none().then(|| {
            // tmux 的内置默认是「不用」——`ResolvedAutomation` 里 tmux 为
            // `None` 时不发 attach 命令。
            let (v, src) = match up.as_ref() {
                Some((name, prefs)) => match &prefs.tmux {
                    Some(TmuxChoice::Off) => ("不用 tmux", Source::Group(name)),
                    Some(TmuxChoice::Attach { .. }) => ("自动 attach", Source::Group(name)),
                    None => ("不用 tmux", Source::Builtin),
                },
                None => ("不用 tmux", Source::Builtin),
            };
            inherit_row::effective_line(v, src)
        });
        inherit_row::slot(
            ui,
            t,
            |ui| {
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
            },
            line,
        );
        ui.end_row();

        if let Some(TmuxChoice::Attach { session_name }) = &mut a.tmux {
            ui.label("会话名");
            // 不给 EditorBuffer 另开一个 String 字段:两个真源迟早漂移。
            // 每帧从 Option<String> 展开成临时 String,改动时写回 ——
            // 清空 = 回到「由会话名推导」,正是要的语义。
            let mut s = session_name.clone().unwrap_or_default();
            let resp = ui.add(
                egui::TextEdit::singleline(&mut s)
                    // hint 也走 egui 那条忽略 `wrap_width` 的单行排版,文字比框
                    // 宽就直接画到框外(窄栏下就是画到面板外)。所以这里的文案
                    // 有硬长度预算:300px 面板下框内容区只有约 192pt ≈ 12 个汉字。
                    // 原文案「会话名为空,无法推导 —— 必须手填」实测溢出 4px。
                    // (`derived` 那一支由用户数据决定长度,管不住,只能靠裁剪。)
                    .hint_text(crate::theme::hint_text(
                        t,
                        if derived.is_empty() {
                            "会话名为空,须手填".to_string()
                        } else {
                            format!("留空则用「{derived}」")
                        },
                    ))
                    // 曾是 `f32::INFINITY`:吃光整行,再叠上 `TextEdit` 自己
                    // 8px 的左右内边距,外框正好画到面板外(走查 P0-1 的余数)。
                    .desired_width(field_w(ui.available_width(), FIELD_W_L, TEXT_EDIT_MARGIN_X)),
            );
            if resp.changed() {
                *session_name = if s.trim().is_empty() { None } else { Some(s) };
            }
            ui.end_row();
        }
    });

    section(ui, t, "会话管理器/右栏", "工作目录", &mut first);
    grid(ui, "sm_auto_dir", |ui| {
        ui.label("初始目录");
        let mut s = a.work_dir.clone().unwrap_or_default();
        let line = a.work_dir.is_none().then(|| {
            let (v, src) = match up.as_ref() {
                Some((name, prefs)) => match prefs.work_dir.as_deref() {
                    Some(d) => (d.to_string(), Source::Group(name)),
                    None => ("远端默认目录".to_string(), Source::Builtin),
                },
                None => ("远端默认目录".to_string(), Source::Builtin),
            };
            inherit_row::effective_line(&v, src)
        });
        inherit_row::slot(
            ui,
            t,
            |ui| {
                // hint 从「留空 = 继承(远端默认)」改成中性的「留空 = 继承」:
                // 「继承到了什么」现在由右边的灰字负责,写在 hint 里既重复
                // 又管不住长度(hint 走 egui 那条忽略 wrap_width 的单行排版)。
                //
                // 宽度档从 `FIELD_W_L` 降到 `FIELD_W_M`:这一行现在要跟一条
                // 灰字,撑满整行的话灰字必然折到下一行,看着像两个字段。
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut s)
                        .hint_text(crate::theme::hint_text(t, "留空 = 继承"))
                        .desired_width(field_w(
                            ui.available_width(),
                            FIELD_W_M,
                            TEXT_EDIT_MARGIN_X,
                        )),
                );
                if resp.changed() {
                    a.work_dir = if s.trim().is_empty() {
                        None
                    } else {
                        Some(s.clone())
                    };
                }
            },
            line,
        );
        ui.end_row();
    });

    section(ui, t, "会话管理器/右栏", "登录后命令", &mut first);
    // `None`(继承)与 `Some(vec![])`(显式空覆盖)必须可区分 —— 所以**绝不能**
    // 用 `get_or_insert_with(Vec::new)`:那样光是打开这一页就会把「继承」
    // 悄悄翻成「显式覆盖成空」,分组里配的命令全部失效。
    let mut reset_commands = false;
    match a.commands.as_mut() {
        None => {
            // 继承来的命令**会真的发到远端 shell**。只说「继承上游的命令列表」
            // 的话,用户得去分组管理器里翻才知道自己会执行什么(走查 10)。
            let (v, src) = match up.as_ref() {
                Some((name, prefs)) => match prefs.commands.as_deref() {
                    Some(cs) if !cs.is_empty() => {
                        (format!("{} 条命令", cs.len()), Source::Group(name))
                    }
                    // 分组显式覆盖成空 与 分组没配,继承下来都是「不执行」。
                    _ => ("不执行任何命令".to_string(), Source::Builtin),
                },
                None => ("不执行任何命令".to_string(), Source::Builtin),
            };
            inherit_row::slot(
                ui,
                t,
                |ui| {
                    if ui.button("改为自定义").clicked() {
                        reset_commands = true; // 见下方,这里借着 a.commands 不能直接赋值
                    }
                },
                Some(inherit_row::effective_line(&v, src)),
            );
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
                    // 这一行输入框后面还串着「延时」勾选框、延时数值框和
                    // ↑/↓/✕ 三个按钮。定宽 240 + 不换行 = 右栏拖窄到 300px 时
                    // 「✕」被整个推出面板,**用户没有任何办法删掉一条命令**
                    // (走查 P0-1 同族)。`wrapped` 兜底 + `field_w` 预留双保险:
                    // 预留量只能是估的(勾选框的方框宽、`DragValue` 的实际宽度
                    // 运行时都读不到),估少了也只是折行,不会再被裁掉。
                    ui.horizontal_wrapped(|ui| {
                        use crate::ui::icon::{icon_button, Glyph};
                        // 图标按钮是正方形,边长 = `interact_size.y`。
                        let icon_w = ui.spacing().interact_size.y + ui.spacing().item_spacing.x;
                        let reserve = button_reserve(ui, "延时")
                            + if c.delay_ms.is_some() {
                                button_reserve(ui, "60000 ms")
                            } else {
                                0.0
                            }
                            + 3.0 * icon_w
                            + TEXT_EDIT_MARGIN_X;
                        // 用 multiline 而不是 singleline:singleline 是否把粘贴进来
                        // 的换行原样交给我们,取决于 egui 内部实现;multiline 一定
                        // 会,拆条逻辑才有输入可拆。`desired_rows(1)` 让它看起来
                        // 仍是一行。
                        let resp = ui.add(
                            egui::TextEdit::multiline(&mut c.text)
                                .desired_rows(1)
                                .desired_width(field_w(ui.available_width(), FIELD_W_M, reserve)),
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

                        // 自绘图标,不用文字按钮:`✕`(U+2715) 在 egui 内置拉丁
                        // 字体和微软雅黑里都没有,实机渲染成豆腐块 —— 跟走查
                        // P0-5 报的跳板链那三个按钮是同一个缺陷,当时只改了跳板链。
                        if icon_button(ui, Glyph::ArrowUp, i > 0, "上移") {
                            swap = Some((i, i - 1));
                        }
                        if icon_button(ui, Glyph::ArrowDown, i + 1 < len, "下移") {
                            swap = Some((i, i + 1));
                        }
                        if icon_button(ui, Glyph::Cross, true, "删除这条命令") {
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

    section(ui, t, "会话管理器/右栏", "环境变量", &mut first);
    // 走查 18:常驻的是灰字事实,红框只留给真的像在存密码的变量名。
    // 先 clone 出来:下面整段持着 `a.env` 的可变借用。命中通常是空 Vec,
    // 不产生分配。
    let secret_keys: Vec<String> = a
        .env
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .filter(|v| super::env_hint::looks_like_secret(&v.key))
        .map(|v| v.key.clone())
        .collect();
    if secret_keys.is_empty() {
        // 用 `horizontal` + 显式换行的 `Label`,**不用** `horizontal_wrapped`:
        // 后者在窄栏下会把这一格的宽度撑出约 0.1px,而 `section()` 的分隔线
        // 按 `available_width` 画,于是整页越界 ——
        // `automation_page_never_paints_past_the_panel_at_any_width_or_dpi`
        // 会红。这一行里只有图标和灰字,`Label` 自己换行就够了。
        ui.horizontal_top(|ui| {
            crate::ui::icon::icon_inline(
                ui,
                crate::ui::icon::Glyph::Info,
                crate::theme::c32(t.fg_dimmer),
            );
            ui.add(
                egui::Label::new(
                    egui::RichText::new(ENV_NOTE).color(crate::theme::c32(t.fg_dimmer)),
                )
                // `horizontal` 里 `Label` 默认**不换行**,这行字比窄栏长 65px。
                .wrap(),
            );
        });
    } else {
        warn_banner(ui, t, &super::env_hint::secret_warning(&secret_keys));
    }
    ui.add_space(crate::ui::metrics::SP_S);
    // 同 commands:`None`(继承)与 `Some(vec![])`(显式空覆盖)必须可区分。
    let mut reset_env = false;
    match a.env.as_mut() {
        None => {
            let (v, src) = match up.as_ref() {
                Some((name, prefs)) => match prefs.env.as_deref() {
                    Some(vs) if !vs.is_empty() => {
                        (format!("{} 个变量", vs.len()), Source::Group(name))
                    }
                    _ => ("不设任何变量".to_string(), Source::Builtin),
                },
                None => ("不设任何变量".to_string(), Source::Builtin),
            };
            inherit_row::slot(
                ui,
                t,
                |ui| {
                    if ui.button("改为自定义").clicked() {
                        reset_env = true;
                    }
                },
                Some(inherit_row::effective_line(&v, src)),
            );
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
                    // 同「登录后命令」:定宽 140+220 加起来就超过窄栏可用宽,
                    // 「✕」会被顶出面板 —— 删不掉变量。
                    ui.horizontal_wrapped(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut v.key)
                                .hint_text(crate::theme::hint_text(t, "KEY"))
                                // 变量名归短值档。比原来的 140 窄,但这一行
                                // 必须容得下 `=`、值框和 ✕ 三样东西。
                                .desired_width(field_w(ui.available_width(), FIELD_W_S, 0.0)),
                        );
                        ui.label("=");
                        let reserve = ui.spacing().interact_size.y
                            + ui.spacing().item_spacing.x
                            + TEXT_EDIT_MARGIN_X;
                        ui.add(
                            egui::TextEdit::singleline(&mut v.value)
                                .hint_text(crate::theme::hint_text(t, "值(明文)"))
                                .desired_width(field_w(ui.available_width(), FIELD_W_M, reserve)),
                        );
                        // 同「登录后命令」那行:文字「✕」在实机是豆腐块。
                        if crate::ui::icon::icon_button(
                            ui,
                            crate::ui::icon::Glyph::Cross,
                            true,
                            "删除这个变量",
                        ) {
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

    section(ui, t, "会话管理器/右栏", "时序", &mut first);
    grid(ui, "sm_auto_timing", |ui| {
        ui.label("首字节后再等");
        let (v, src) = resolve_u32(
            up.as_ref(),
            |p| p.initial_delay_ms,
            mullion_store::automation::DEFAULT_INITIAL_DELAY_MS,
        );
        let line = a
            .initial_delay_ms
            .is_none()
            .then(|| inherit_row::effective_line(&format!("{v} ms"), src));
        opt_ms(
            ui,
            t,
            "sm_auto_initial",
            &mut a.initial_delay_ms,
            mullion_store::automation::DEFAULT_INITIAL_DELAY_MS,
            0,
            10_000,
            line,
        );
        ui.end_row();

        ui.label("行间延时");
        let (v, src) = resolve_u32(
            up.as_ref(),
            |p| p.inter_delay_ms,
            mullion_store::automation::DEFAULT_INTER_DELAY_MS,
        );
        let line = a
            .inter_delay_ms
            .is_none()
            .then(|| inherit_row::effective_line(&format!("{v} ms"), src));
        opt_ms(
            ui,
            t,
            "sm_auto_inter",
            &mut a.inter_delay_ms,
            mullion_store::automation::DEFAULT_INTER_DELAY_MS,
            0,
            10_000,
            line,
        );
        ui.end_row();

        ui.label("就绪超时");
        let (v, src) = resolve_u32(
            up.as_ref(),
            |p| p.ready_timeout_ms,
            mullion_store::automation::DEFAULT_READY_TIMEOUT_MS,
        );
        let line = a
            .ready_timeout_ms
            .is_none()
            .then(|| inherit_row::effective_line(&format!("{v} ms"), src));
        opt_ms(
            ui,
            t,
            "sm_auto_ready",
            &mut a.ready_timeout_ms,
            mullion_store::automation::DEFAULT_READY_TIMEOUT_MS,
            1,
            120_000,
            line,
        );
        ui.end_row();
    });
}

/// F120:SFTP 默认目录 + 书签。`SftpPrefs` 不参与分组继承(D15)——不需要
/// `inherit_row`/三态那一整套,是全页面最简单的一张纯字段表单。
///
/// `first` 由调用方(`editor.rs` 的 Tab 分发)传进来而不是像 `auth`/
/// `automation`/`appearance` 那样自己 `let mut first = true`:这一页目前
/// 只有两节,让调用方持有游标,将来要跟别的内容拼一页也不用改签名。
pub(super) fn sftp(ui: &mut Ui, t: &Theme, buf: &mut EditorBuffer, first: &mut bool) {
    section(ui, t, "会话管理器/右栏", "默认目录", first);
    grid(ui, "sm_sftp_dirs", |ui| {
        ui.label("默认远端目录");
        ui.add(
            egui::TextEdit::singleline(&mut buf.sftp_default_remote)
                .hint_text(crate::theme::hint_text(t, "留空 = 登录目录"))
                .desired_width(field_w(ui.available_width(), FIELD_W_M, 0.0)),
        );
        ui.end_row();

        ui.label("默认本地目录");
        ui.add(
            egui::TextEdit::singleline(&mut buf.sftp_default_local)
                .hint_text(crate::theme::hint_text(t, "留空 = 用户主目录(USERPROFILE)"))
                .desired_width(field_w(ui.available_width(), FIELD_W_M, 0.0)),
        );
        ui.end_row();
    });

    section(ui, t, "会话管理器/右栏", "书签", first);
    grid(ui, "sm_sftp_bookmarks", |ui| {
        ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
            ui.label("书签");
        });
        ui.vertical(|ui| bookmark_editor(ui, t, &mut buf.sftp_bookmarks));
        ui.end_row();
    });
}

/// 书签列表的逐条编辑:名称 + 路径 + 删除。不支持拖拽排序(D1 明确排除)。
///
/// 名称允许留空 —— 那时界面(书签栏)回退显示路径本身,`Bookmark` 的文档
/// 注释里已经写明这是有意的合法状态,这里的 hint 只是把话说给填表的人听。
fn bookmark_editor(ui: &mut Ui, t: &Theme, bookmarks: &mut Vec<(String, String)>) {
    // 同「登录后」页命令列表那个坑(见 `sm_auto_cmds_gen` 处的长注释):`TextEdit`
    // 的光标/选区/撤销栈按**位置 id** 跨帧存活,删掉中间一条会让后面的行整体
    // 上移一格、套上前一行遗留的状态。这里没有天然稳定 key 可用(书签就是两个
    // 可编辑字符串,拿内容当 salt 会导致每敲一个字就换 id、状态每帧丢),所以
    // 照搬那边的做法:只在结构变化时 +1 的世代号当 salt,删完全体换 id。
    let gen_id = egui::Id::new("sm_sftp_bookmarks_gen");
    let generation: u64 = ui.data(|d| d.get_temp(gen_id).unwrap_or(0));

    let mut remove: Option<usize> = None;
    for (i, (name, path)) in bookmarks.iter_mut().enumerate() {
        ui.push_id((generation, i), |ui| {
            ui.horizontal(|ui| {
                use crate::ui::icon::{icon_button, Glyph};
                // 自绘图标,不用 "✕" 文字:同 `chain_editor`——U+2715 在 egui
                // 内置拉丁字体和微软雅黑里都没有,实机上渲染成豆腐块。
                if icon_button(ui, Glyph::Cross, true, "删除书签") {
                    remove = Some(i);
                }
                ui.add(
                    egui::TextEdit::singleline(name)
                        .hint_text("名称(留空则显示路径)")
                        .desired_width(field_w(ui.available_width(), FIELD_W_S, 0.0)),
                );
                ui.add(
                    egui::TextEdit::singleline(path)
                        .hint_text("远端路径")
                        .desired_width(field_w(ui.available_width(), FIELD_W_M, 0.0)),
                );
            });
        });
    }
    if bookmarks.is_empty() {
        ui.colored_label(crate::theme::c32(t.fg_dimmer), "还没加书签。");
    }
    if let Some(i) = remove {
        bookmarks.remove(i);
        ui.data_mut(|d| d.insert_temp(gen_id, generation.wrapping_add(1)));
    }
    if ui.button("+ 添加书签").clicked() {
        bookmarks.push((String::new(), String::new()));
    }
}

#[cfg(test)]
mod tests {
    use super::{
        auth, automation, basic, credential_combo, key_candidate_combo, network, tri_state,
    };
    use crate::theme::Theme;
    use crate::ui::session_manager::{
        AuthKindUi, CredSourceUi, EditorBuffer, JumpModeUi, ProxyModeUi, SecretPresence,
    };
    use mullion_store::{
        Auth, AuthKind, Connection, GroupRecord, Identity, Protocol, SessionId, SessionRecord,
    };

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

    /// 抠出本帧画出的所有文本。`find_text_pos` 只回位置,判「有没有说某句话」
    /// 时要的是内容本身。
    fn all_text(shapes: &[egui::epaint::ClippedShape]) -> Vec<String> {
        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                egui::Shape::Text(t) => out.push(t.galley.text().to_string()),
                _ => {}
            }
        }
        let mut out = Vec::new();
        shapes.iter().for_each(|cs| walk(&cs.shape, &mut out));
        out
    }

    /// SFTP 页测试用的最小样本缓冲。
    fn sample_buffer() -> EditorBuffer {
        EditorBuffer::default()
    }

    /// 跑两帧(第一帧只让布局稳定,egui `Panel`/`Area` 首帧 fade_in 只记
    /// `Shape::Noop`),把第二帧画出的所有文字收集起来。`run_page`/`all_text`
    /// 已经是这套两帧手法,这里只是包一层把 `Theme` 也递给 `page`。
    fn render_texts(mut page: impl FnMut(&mut egui::Ui, &Theme)) -> Vec<String> {
        let t = crate::theme::MULLION_DARK;
        let out = run_page(|ui| page(ui, &t));
        all_text(&out.shapes)
    }

    /// F120:两个默认目录 + 书签列表都要在「SFTP」页上画得出来。
    /// 判据是画出来的文本 —— 只断言函数存在等于什么都没测。
    #[test]
    fn the_sftp_section_shows_both_default_directories_and_the_bookmarks() {
        let mut buf = sample_buffer();
        buf.sftp_default_remote = "/srv/app".into();
        buf.sftp_default_local = r"D:\work".into();
        buf.sftp_bookmarks = vec![("日志".into(), "/var/log".into())];

        let texts = render_texts(|ui, t| {
            let mut first = true;
            super::sftp(ui, t, &mut buf, &mut first);
        });
        assert!(texts.iter().any(|s| s.contains("默认远端目录")));
        assert!(texts.iter().any(|s| s.contains("默认本地目录")));
        assert!(texts.iter().any(|s| s.contains("日志")));
        assert!(texts.iter().any(|s| s.contains("/var/log")));
    }

    /// 留空要说清缺省是什么 —— 否则用户不知道「不填」会发生什么,
    /// 只能试(F119 的空态文案规范)。
    #[test]
    fn empty_default_directories_explain_what_happens_instead() {
        let mut buf = sample_buffer();
        buf.sftp_default_remote.clear();
        buf.sftp_default_local.clear();
        let texts = render_texts(|ui, t| {
            let mut first = true;
            super::sftp(ui, t, &mut buf, &mut first);
        });
        let all = texts.join(" ");
        assert!(all.contains("登录目录"), "远端留空的缺省要写出来: {all}");
        assert!(
            all.contains("用户主目录") || all.contains("USERPROFILE"),
            "本地留空的缺省要写出来: {all}"
        );
    }

    /// 「登录后」页继承测试用的分组:只填 `automation`,其余取默认。
    fn auto_group(
        id: u64,
        name: &str,
        f: impl FnOnce(&mut mullion_store::AutomationPrefs),
    ) -> GroupRecord {
        let mut g = GroupRecord {
            id: mullion_store::GroupId(id),
            name: name.into(),
            tags: Vec::new(),
            terminal: Default::default(),
            appearance: Default::default(),
            network: Default::default(),
            automation: Default::default(),
        };
        f(&mut g.automation);
        g
    }

    /// 走查 10:选了「继承」而分组配了值时,必须说清是**分组**配的。
    ///
    /// 旧文案写死「实际生效:300 ms(内置默认)」—— 分组配了 900 时这句话是错的,
    /// 用户会以为改分组没用。
    #[test]
    fn inherited_timing_names_the_group_when_the_group_sets_it() {
        let mut buf = EditorBuffer {
            preserved_group_id: Some(mullion_store::GroupId(7)),
            ..Default::default()
        };
        let groups = vec![auto_group(7, "生产", |a| a.initial_delay_ms = Some(900))];

        let t = crate::theme::MULLION_DARK;
        let out = run_page(|ui| automation(ui, &t, &mut buf, &groups));
        let texts = all_text(&out.shapes);
        assert!(
            texts
                .iter()
                .any(|s| s.contains("900 ms") && s.contains("生产")),
            "分组配了 initial_delay_ms=900,继承提示必须点名分组「生产」;实际画出的文字:{texts:?}"
        );
    }

    /// 分组没配时才落「内置默认」。这条和上一条是一对 —— 只留一条的话,
    /// 把实现写死成任意一支都能过。
    #[test]
    fn inherited_timing_falls_back_to_builtin_when_no_group_sets_it() {
        let mut buf = EditorBuffer::default();
        let t = crate::theme::MULLION_DARK;
        let out = run_page(|ui| automation(ui, &t, &mut buf, &[]));
        let texts = all_text(&out.shapes);
        assert!(
            texts
                .iter()
                .any(|s| s.contains("300 ms") && s.contains("内置默认")),
            "未分组时三个时序应显示内置默认;实际画出的文字:{texts:?}"
        );
    }

    /// 走查 19:同一件事三种说法(「继承」/「继承分组」/「继承上游的…」)。
    /// 全页扫一遍,不许再出现旧变体 —— 来源由灰字负责说。
    #[test]
    fn inheritance_is_called_the_same_thing_on_every_page() {
        let t = crate::theme::MULLION_DARK;
        let mut buf = EditorBuffer {
            jump_mode: JumpModeUi::Inherit,
            proxy_mode: ProxyModeUi::Inherit,
            ..Default::default()
        };

        let mut pages: Vec<String> = Vec::new();
        let out = run_page(|ui| {
            basic(
                ui,
                &t,
                &mut buf,
                &[],
                &[],
                &[],
                None,
                SecretPresence::default(),
                false,
                &mut Default::default(),
            )
        });
        pages.extend(all_text(&out.shapes));
        let out = run_page(|ui| automation(ui, &t, &mut buf, &[]));
        pages.extend(all_text(&out.shapes));

        for s in &pages {
            assert!(!s.contains("继承分组"), "还有「继承分组」的旧说法:{s:?}");
            assert!(!s.contains("继承上游"), "还有「继承上游」的旧说法:{s:?}");
        }
        assert!(
            pages.iter().any(|s| s == "继承"),
            "统一后的「继承」一个都没出现,说明改过头了:{pages:?}"
        );
    }

    /// 走查 19 后半原本测的是「sftp 在下拉里跟 ssh 平级,选了保存后连不上,
    /// 界面上要有提示」——D1 起 SFTP 节点走同一条映射,只是多带
    /// `wants_sftp`,不再有「连不上」这回事。F118 给了
    /// SFTP 节点自己的一档,拨号路径已经实现,协议此后只读(D3),那个下拉
    /// 连同「未实现」提示一起下线。这条测试改测 D3 实际留下的行为:
    /// `Protocol::Sftp` 就该显示成纯文本「sftp」,不再提未实现,也不再是
    /// 能选的下拉候选(那条由 `mod.rs::the_protocol_row_is_plain_text_
    /// with_no_dropdown_to_open` 用真实点击钉住)。
    #[test]
    fn sftp_protocol_shows_as_plain_readonly_text_not_an_unimplemented_dropdown() {
        let t = crate::theme::MULLION_DARK;
        let mut buf = EditorBuffer {
            protocol: mullion_store::Protocol::Sftp,
            ..Default::default()
        };
        let out = run_page(|ui| {
            basic(
                ui,
                &t,
                &mut buf,
                &[],
                &[],
                &[],
                None,
                SecretPresence::default(),
                false,
                &mut Default::default(),
            )
        });
        let texts = all_text(&out.shapes);
        assert!(
            texts.iter().any(|s| s == "sftp"),
            "协议是 sftp 时该显示纯文本「sftp」;实际画出的文字:{texts:?}"
        );
        assert!(
            !texts.iter().any(|s| s.contains("未实现")),
            "D3 之后 sftp 已经有自己的一档、拨号路径已实现,不该再提未实现:{texts:?}"
        );
    }

    /// 走查 10 里后果最重的一处:继承来的命令**会真的发到远端 shell**。
    /// 只说「继承上游的命令列表」而不说几条,用户就得去分组管理器里翻。
    #[test]
    fn inherited_commands_say_how_many_will_run() {
        let mut buf = EditorBuffer {
            preserved_group_id: Some(mullion_store::GroupId(5)),
            ..Default::default()
        };
        buf.preserved_automation.commands = None; // 继承
        let groups = vec![auto_group(5, "生产", |a| {
            a.commands = Some(vec![
                mullion_store::AutomationCommand {
                    text: "cd /srv".into(),
                    delay_ms: None,
                },
                mullion_store::AutomationCommand {
                    text: "tail -f log".into(),
                    delay_ms: None,
                },
            ])
        })];

        let t = crate::theme::MULLION_DARK;
        let out = run_page(|ui| automation(ui, &t, &mut buf, &groups));
        let texts = all_text(&out.shapes);
        assert!(
            texts
                .iter()
                .any(|s| s.contains("2 条") && s.contains("生产")),
            "继承提示要说清「几条、来自哪个分组」;实际画出的文字:{texts:?}"
        );
    }

    /// 上游也没配时,继承的结果是「一条都不执行」。这句话必须说出来 ——
    /// 「继承上游的命令列表」在上游为空时读起来像「有东西但我没显示」。
    #[test]
    fn inherited_commands_say_nothing_will_run_when_upstream_is_empty() {
        let mut buf = EditorBuffer::default();
        buf.preserved_automation.commands = None;
        let t = crate::theme::MULLION_DARK;
        let out = run_page(|ui| automation(ui, &t, &mut buf, &[]));
        let texts = all_text(&out.shapes);
        assert!(
            texts.iter().any(|s| s.contains("不执行任何命令")),
            "上游为空时要明说一条都不跑;实际画出的文字:{texts:?}"
        );
    }

    /// tmux 选「继承」时也要说清生效结果。走查 10 点名的六处里,
    /// tmux 是唯一一处**什么都不说**的。
    #[test]
    fn inherited_tmux_shows_what_it_resolves_to() {
        let mut buf = EditorBuffer {
            preserved_group_id: Some(mullion_store::GroupId(3)),
            ..Default::default()
        };
        let groups = vec![auto_group(3, "堡垒", |a| {
            a.tmux = Some(mullion_store::TmuxChoice::Off)
        })];

        let t = crate::theme::MULLION_DARK;
        let out = run_page(|ui| automation(ui, &t, &mut buf, &groups));
        let texts = all_text(&out.shapes);
        assert!(
            texts
                .iter()
                .any(|s| s.contains("不用 tmux") && s.contains("堡垒")),
            "分组把 tmux 关了,继承提示要说清;实际画出的文字:{texts:?}"
        );
    }

    /// 数渲染结果里的水平分隔线(`Shape::LineSegment` 且两端 y 相等 ——
    /// `ui.separator()` 在竖直布局下正是这么画的,见 egui-0.30.0
    /// `widgets/separator.rs`(`painter.hline`)+ epaint-0.30.0
    /// `src/shape.rs` 的 `Shape::hline`;跳板行 `Glyph::Cross` 图标虽然也用
    /// `Shape::LineSegment`,但画的是两条斜线,两端 y 不相等,不会混进来)。
    /// 走查 P2-17 的分区节奏守护测试用它数「分区数 − 1」是否成立。
    fn count_horizontal_separators(shapes: &[egui::epaint::ClippedShape]) -> usize {
        fn walk(shape: &egui::Shape, count: &mut usize) {
            match shape {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, count)),
                egui::Shape::LineSegment { points, .. } if points[0].y == points[1].y => {
                    *count += 1;
                }
                _ => {}
            }
        }
        let mut count = 0;
        shapes.iter().for_each(|cs| walk(&cs.shape, &mut count));
        count
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
                    automation(ui, &t, buf, &[]);
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

        // 按钮是自绘图标(不是文字),所以按「行内那条命令的文本」定位行,
        // 再从形状里认出这一行的三个图标 —— 跟跳板链那两条测试同一套做法。
        // tol=8.0:实测命令行行距 30px,半程 15px,8.0 有充足冗余不会吸到邻行。
        // 本行有一个「延时」勾选框,而 `icon_buttons_on_row` 已知会把**勾上**
        // 的勾选框对勾误判成 `Down`(见它的文档注释);这里三条命令的
        // `delay_ms` 都是 `None`,勾选框未勾、不画对勾,不构成碰撞。
        let row_y = find_text_pos(&out.shapes, "cmd-beta")
            .expect("cmd-beta 应该出现在第二条命令这一行")
            .y;
        let buttons = icon_buttons_on_row(&out.shapes, row_y, 8.0);
        assert_eq!(buttons.len(), 3, "第二条命令这一行应该有 3 个图标按钮");
        assert_eq!(buttons[0].1, FoundGlyph::Up, "最左边应该是「↑」");
        assert_eq!(buttons[1].1, FoundGlyph::Down, "中间应该是「↓」");
        assert_eq!(buttons[2].1, FoundGlyph::Cross, "最右边应该是「✕」");
        let second_row_up = buttons[0].0;

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

    /// 走查 18:env 区常驻的是**灰字事实**,红框只在变量名真像密码时出现。
    /// 这条守的是接线 —— `env_hint` 的纯函数测试证明判据对,这条证明判据真的
    /// 接到了横幅的显示条件上。
    #[test]
    fn the_env_warning_only_turns_red_when_a_key_looks_like_a_secret() {
        let t = crate::theme::MULLION_DARK;
        let run = |vars: Vec<mullion_store::EnvVar>| {
            let mut buf = EditorBuffer::default();
            buf.preserved_automation.env = Some(vars);
            let ctx = egui::Context::default();
            let out = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    automation(ui, &t, &mut buf, &[]);
                });
            });
            all_text(&out.shapes)
        };
        let env = |k: &str| mullion_store::EnvVar {
            key: k.to_string(),
            value: "v".to_string(),
        };

        let calm = run(vec![env("LANG"), env("EDITOR")]);
        assert!(
            calm.iter().any(|s| s.contains("以明文存进")),
            "常驻那句事实不能没了 —— 用户得知道值不加密:{calm:?}"
        );
        assert!(
            !calm.iter().any(|s| s.contains("看着像在存密码")),
            "普通变量名不该弹红框,天天见的警告等于没有警告:{calm:?}"
        );

        let alarmed = run(vec![env("LANG"), env("DB_PASSWORD")]);
        assert!(
            alarmed
                .iter()
                .any(|s| s.contains("看着像在存密码") && s.contains("DB_PASSWORD")),
            "变量名像密码时必须升成红框并点名是哪个:{alarmed:?}"
        );
    }

    /// 靶子4(高优先级):环境变量「删除」按钮必须删掉**被点的那一行**,
    /// 不能恒删第一行(`vars.remove(0)`)——只有两行以内时这两种实现表现
    /// 一样,必须用三行以上、点中间那行才能把它们区分开。
    ///
    /// 验证边界同靶子3:端到端指针事件驱动,覆盖「点哪一行的 ✕ 就删哪一
    /// 行」这条链路;覆盖不到「恢复继承」按钮的重置行为(不在本靶子范围)。
    ///
    /// 变量名用 `VAR_*` 而不是 `KEY_*`:后者会命中
    /// `env_hint::looks_like_secret`(走查 18),红框里会把变量名原样点出来,
    /// 于是 `find_text_pos("KEY_B")` 定位到的是横幅而不是表格行。断言本身没变。
    #[test]
    fn clicking_remove_on_middle_env_var_deletes_that_row_not_always_the_first() {
        let t = crate::theme::MULLION_DARK;
        let mut buf = EditorBuffer::default();
        buf.preserved_automation.env = Some(vec![
            mullion_store::EnvVar {
                key: "VAR_A".to_string(),
                value: "a".to_string(),
            },
            mullion_store::EnvVar {
                key: "VAR_B".to_string(),
                value: "b".to_string(),
            },
            mullion_store::EnvVar {
                key: "VAR_C".to_string(),
                value: "c".to_string(),
            },
        ]);
        let ctx = egui::Context::default();

        let run = |ctx: &egui::Context, buf: &mut EditorBuffer, input: egui::RawInput| {
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    automation(ui, &t, buf, &[]);
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

        // 同上:按钮是自绘图标,按行内的变量名定位行再认图标。
        // 这一行没有勾选框,不存在 `icon_buttons_on_row` 那条对勾碰撞。
        let row_y = find_text_pos(&out.shapes, "VAR_B")
            .expect("VAR_B 应该出现在第二行")
            .y;
        let buttons = icon_buttons_on_row(&out.shapes, row_y, 8.0);
        assert_eq!(buttons.len(), 1, "环境变量每行只有一个「✕」");
        assert_eq!(buttons[0].1, FoundGlyph::Cross);
        let middle_row_remove = buttons[0].0;

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
            vec!["VAR_A", "VAR_C"],
            "点第二行(VAR_B)的「✕」应该只删掉 VAR_B;实际剩下 {keys:?}"
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
                    automation(ui, &t, &mut buf, &[]);
                });
            });
            ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    automation(ui, &t, &mut buf, &[]);
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
                    automation(ui, &t, buf, &[]);
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
        let hint_pos = find_text_pos(&out.shapes, "实际生效:300 ms(内置默认)")
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
        let hint_pos = find_text_pos(&out.shapes, "实际生效:200 ms(内置默认)")
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
        let hint_pos = find_text_pos(&out.shapes, "实际生效:15000 ms(内置默认)")
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
            auth: Auth::inline("ops", AuthKind::Password),
            terminal: Default::default(),
            appearance: Default::default(),
            network: Default::default(),
            automation: Default::default(),
            sftp: Default::default(),
        }
    }

    /// 用默认 `RawInput` 跑两帧任意一页,返回**第二帧**的输出。
    ///
    /// 功能类测试(找文字、按坐标点按钮)用它;要量「有没有画出面板」用
    /// `run_page_at` —— 那个会显式给面板宽和 DPI,并撑开 clip_rect。
    ///
    /// 必须跑两帧:本组页面的 bug 多是「第一帧看着对、第二帧弹回去」这一类
    /// (状态写回 → 下一帧重新读)。只跑一帧的测试对它们全盲。
    fn run_page(mut page: impl FnMut(&mut egui::Ui)) -> egui::FullOutput {
        let ctx = egui::Context::default();
        let run = |page: &mut dyn FnMut(&mut egui::Ui)| {
            ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| page(ui));
            })
        };
        let _ = run(&mut page);
        run(&mut page)
    }

    fn run_basic(buf: &mut EditorBuffer, sessions: &[SessionRecord]) -> egui::FullOutput {
        let t = crate::theme::MULLION_DARK;
        run_page(|ui| {
            basic(
                ui,
                &t,
                buf,
                &[],
                sessions,
                &[],
                Some(SessionId(1)),
                SecretPresence::default(),
                false,
                &mut Default::default(),
            );
        })
    }

    /// 走查 15:必填项的红字**只在用户碰过那个框之后**才出。
    ///
    /// 新建草稿一打开三行全红,读起来像「你错了」——而用户连第一个字都还没
    /// 敲。所以判据是「碰过 + 仍为空」,不是「为空」。
    ///
    /// 自证会变红:把 `field_error` 的条件里的 `touched.name &&` 去掉,
    /// 第一段断言(没碰过时不该有红字)立刻炸。
    #[test]
    fn a_required_field_only_turns_red_after_you_have_been_in_it() {
        let t = crate::theme::MULLION_DARK;
        let run = |touched: super::super::validate::Touched| {
            let mut buf = EditorBuffer::default(); // 名称/主机/端口全空
            let mut touched = touched;
            let out = run_page(|ui| {
                basic(
                    ui,
                    &t,
                    &mut buf,
                    &[],
                    &[],
                    &[],
                    None,
                    SecretPresence::default(),
                    false,
                    &mut touched,
                );
            });
            all_text(&out.shapes)
        };

        let untouched = run(Default::default());
        assert!(
            !untouched.iter().any(|s| s.contains("不能为空")),
            "还没碰过任何框就报错等于劈头骂人:{untouched:?}"
        );

        let touched = run(super::super::validate::Touched {
            name: true,
            host: true,
            user: false,
            port: false,
        });
        assert!(
            touched.iter().any(|s| s == "会话名称不能为空"),
            "碰过又留空的名称该出红字:{touched:?}"
        );
        assert!(
            touched.iter().any(|s| s == "主机不能为空"),
            "碰过又留空的主机该出红字:{touched:?}"
        );
    }

    /// 走查 15:端口填错**不看有没有碰过** —— 框里躺着 `0` 或 `abc` 就是错的,
    /// 跟用户填到哪一步无关(它还是从别处粘进来的呢)。留空则合法(落 22),
    /// 所以空端口不该出红字。
    ///
    /// 自证会变红:把 `validate::port` 的空串分支改成报错,第一段断言炸。
    #[test]
    fn a_bad_port_is_flagged_immediately_but_a_blank_one_is_fine() {
        let t = crate::theme::MULLION_DARK;
        let run = |port: &str| {
            let mut buf = EditorBuffer {
                port: port.to_string(),
                ..Default::default()
            };
            let out = run_page(|ui| {
                basic(
                    ui,
                    &t,
                    &mut buf,
                    &[],
                    &[],
                    &[],
                    None,
                    SecretPresence::default(),
                    false,
                    &mut Default::default(),
                );
            });
            all_text(&out.shapes)
        };

        let blank = run("");
        assert!(
            !blank.iter().any(|s| s.contains("1~65535")),
            "空端口是合法的(落默认 22),不该报错:{blank:?}"
        );

        for bad in ["0", "65536", "abc"] {
            let texts = run(bad);
            assert!(
                texts.iter().any(|s| s == "端口要填 1~65535 之间的数字"),
                "端口 {bad:?} 该被当场标出来:{texts:?}"
            );
        }
    }

    /// 走查 6:标签终于有编辑入口。真的敲进去、真的回车、真的点 ✕ ——
    /// 直接改 `buf.preserved_tags` 测的是「Vec 能 push」,证不了 UI 接上了。
    ///
    /// 自证会变红:把 `merge_into(...)` 那行删掉 → 回车后标签不出现;
    /// 把 `remove = Some(i)` 改成不赋值 → 点 ✕ 后标签还在。
    #[test]
    fn typing_a_tag_and_pressing_enter_adds_a_chip_that_can_be_removed() {
        let t = crate::theme::MULLION_DARK;
        let mut buf = EditorBuffer {
            name: "web01".into(),
            host: "10.0.0.1".into(),
            user: "root".into(),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let run = |buf: &mut EditorBuffer, input: egui::RawInput| {
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    basic(
                        ui,
                        &t,
                        buf,
                        &[],
                        &[],
                        &[],
                        Some(SessionId(1)),
                        SecretPresence::default(),
                        false,
                        &mut Default::default(),
                    );
                });
            })
        };
        let click = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };

        let _ = run(&mut buf, egui::RawInput::default());
        let out = run(&mut buf, egui::RawInput::default());
        // 输入框还空着,靠占位符文字反推它的位置。
        let pos = find_text_pos(&out.shapes, "回车添加").expect("标签输入框该有占位提示");
        let _ = run(
            &mut buf,
            egui::RawInput {
                events: vec![
                    egui::Event::PointerMoved(pos),
                    click(pos, true),
                    click(pos, false),
                ],
                ..Default::default()
            },
        );
        // 敲字 + 回车。singleline 的 TextEdit 收到回车会交出焦点,
        // `lost_focus() && key_pressed(Enter)` 同时成立。
        let out = run(
            &mut buf,
            egui::RawInput {
                events: vec![
                    egui::Event::Text("prod".into()),
                    egui::Event::Key {
                        key: egui::Key::Enter,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::default(),
                    },
                ],
                ..Default::default()
            },
        );
        assert_eq!(
            buf.preserved_tags,
            vec!["prod".to_string()],
            "回车该把输入变成一个标签"
        );
        assert!(buf.tag_input.is_empty(), "确认后输入框该清空,否则会重复加");
        assert!(
            all_text(&out.shapes).iter().any(|s| s == "prod"),
            "标签该以 chip 的形式画出来"
        );

        // 点 chip 上的 ✕ 删掉它。
        let out = run(&mut buf, egui::RawInput::default());
        let x = find_text_pos(&out.shapes, "✕").expect("chip 上该有个 ✕");
        let _ = run(
            &mut buf,
            egui::RawInput {
                events: vec![
                    egui::Event::PointerMoved(x),
                    click(x, true),
                    click(x, false),
                ],
                ..Default::default()
            },
        );
        assert!(buf.preserved_tags.is_empty(), "点 ✕ 该把这个标签删掉");
    }

    fn run_appearance(buf: &mut EditorBuffer) -> egui::FullOutput {
        let t = crate::theme::MULLION_DARK;
        run_page(|ui| super::appearance(ui, &t, buf, &mut None))
    }

    /// 走查 4:「图标」页要有实时预览,否则用户设完颜色只能保存了去左栏看。
    ///
    /// 判据:勾了「会话列表」才画那条竖色条。这跟真列表行走的是同一个
    /// `badge::should_paint(ColorTarget::ListItem)`,所以预览不会跟实际漂移。
    ///
    /// (图标那一半的预览由 `the_icon_page_previews_both_size_steps` 守,
    /// 那是 v0.1.26 换成 .ico 之后的形态。)
    ///
    /// 自证会变红:把 `appearance()` 末尾那段 `preview_row` 调用删掉。
    #[test]
    fn the_appearance_page_previews_the_color_bar() {
        use mullion_store::{ColorSpec, ColorTarget};

        /// 数本帧画了多少个图元。`list.rs` 里有个同名辅助(私有,跨文件复用
        /// 不了),项目既有做法是各留一份。
        fn count_shapes(shapes: &[egui::epaint::ClippedShape]) -> usize {
            fn walk(s: &egui::Shape) -> usize {
                match s {
                    egui::Shape::Vec(v) => v.iter().map(walk).sum(),
                    egui::Shape::Noop => 0,
                    _ => 1,
                }
            }
            shapes.iter().map(|cs| walk(&cs.shape)).sum()
        }

        // 竖条:只勾「pane 标题条」时预览行上不该有条,勾上「会话列表」才有。
        let mut off = EditorBuffer {
            preserved_appearance: mullion_store::AppearancePrefs {
                color: Some(ColorSpec {
                    hex: "#e06767".into(),
                    apply_to: vec![ColorTarget::PaneTitle],
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut on = EditorBuffer {
            preserved_appearance: mullion_store::AppearancePrefs {
                color: Some(ColorSpec {
                    hex: "#e06767".into(),
                    apply_to: vec![ColorTarget::PaneTitle, ColorTarget::ListItem],
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        let n_off = count_shapes(&run_appearance(&mut off).shapes);
        let n_on = count_shapes(&run_appearance(&mut on).shapes);
        assert!(
            n_on > n_off,
            "勾了「会话列表」后预览行上应多一条竖色条(未勾 {n_off} 个图形,勾了 {n_on} 个)"
        );
    }

    /// 走查 P2-17 的判据护栏:`section()` 靠调用方显式传入的 `first` 参数
    /// 判断「是不是本页第一个分区」。**这里原计划用
    /// `ui.min_rect().height() > 0.0` 推断,已实测证伪**——`egui::CentralPanel`
    /// 一进去 `min_rect` 就等于整个 `max_rect`(非零),只有生产环境实际
    /// 包这一页的 `egui::ScrollArea` 内层 ui 才从零开始;判据成立与否取决
    /// 于外面拿什么容器包这一页,而不是「这是不是第一个分区」本身,所以
    /// 换成了显式参数。这条测试仍然只测**渲染结果**,不关心 `section()`
    /// 内部怎么判断——`first` 传错同样会被它抓到。
    ///
    /// 用 `count_horizontal_separators`(定义见上,`find_all_text_pos` 之后)
    /// 数各页分隔线条数,覆盖**全部四个 Tab**——复核指出只测「连接」/「外观」
    /// 两页时,`auth()`/`automation()` 八处 `section()` 调用点的 `first`
    /// 传错(比如把「凭据」的游标传成一个新开的 `true`)完全测不出来:
    /// `cargo test --workspace` 会全绿。
    ///
    /// 各页预期条数 = 分区数 − 1:`appearance()` 2 个分区(外观/预览,后者是
    /// 走查 4 加的实时预览)→ 1;
    /// `basic()` 4 个(基本/归类/代理/跳板)→ 3;`auth()` 2 个(身份/凭据)
    /// → 1;`automation()` 6 个(总开关/tmux/工作目录/登录后命令/环境变量/
    /// 时序)→ 5。`automation()` 用默认 buffer 渲染(不触发 `commands` 里
    /// 带 `delay_ms` 的分支),这样「总开关」前面不会先冒出一条
    /// `DELAY_WARNING` 横幅——这条测试只关心分区间的线,不关心警告横幅。
    #[test]
    fn only_the_first_section_on_a_page_skips_the_divider_line() {
        let mut appearance_buf = EditorBuffer::default();
        let out = run_appearance(&mut appearance_buf);
        assert_eq!(
            count_horizontal_separators(&out.shapes),
            1,
            "「外观」页有外观/预览两个分区,应该有 1 条分隔线(首个分区不画)"
        );

        let mut basic_buf = EditorBuffer::default();
        let out = run_basic(&mut basic_buf, &[]);
        assert_eq!(
            count_horizontal_separators(&out.shapes),
            3,
            "「连接」页有基本/归类/代理/跳板四个分区,应该有 3 条分隔线(分区数 − 1)"
        );

        let mut auth_buf = EditorBuffer::default();
        let out = run_auth(&mut auth_buf, SecretPresence::default());
        assert_eq!(
            count_horizontal_separators(&out.shapes),
            1,
            "「身份」页有身份/凭据两个分区,应该有 1 条分隔线"
        );

        let mut automation_buf = EditorBuffer::default();
        let out = run_automation(&mut automation_buf);
        assert_eq!(
            count_horizontal_separators(&out.shapes),
            5,
            "「登录后」页有总开关/tmux/工作目录/登录后命令/环境变量/时序\
             六个分区,应该有 5 条分隔线(分区数 − 1)"
        );
    }

    /// 必修 1 的缺陷坐实:`network()`/`jump()` 是被 `basic()` 调用的**子
    /// 函数**,不是页面级函数——它们必须原样接住调用方传来的游标,不许
    /// 在函数内部自己 `let mut first = true`。这条测试把 `network()`
    /// 单独挂到一个新页面顶部渲染(游标由调用方新开、传 `&mut true`),
    /// 断言顶上不画线——这正是复核指出「`network()` 被单独挂到一页顶部
    /// 渲染,顶部多画出 1 条分隔线」的场景,修完必须有测试钉住它。
    #[test]
    fn network_rendered_alone_with_a_fresh_cursor_does_not_draw_a_leading_divider() {
        let t = crate::theme::MULLION_DARK;
        let mut buf = EditorBuffer::default();
        let ctx = egui::Context::default();
        let run = |buf: &mut EditorBuffer| {
            ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let mut first = true;
                    network(ui, &t, buf, &[], SecretPresence::default(), &mut first);
                });
            })
        };
        let _ = run(&mut buf);
        let out = run(&mut buf);
        assert_eq!(
            count_horizontal_separators(&out.shapes),
            0,
            "`network()` 被单独挂到一页顶部渲染时,「代理」是那一页的第一个\
             分区,不该画分隔线"
        );
    }

    /// 造一张真图标的 base64,走的是生产代码那条归一化路径。
    fn real_ico() -> String {
        let px: Vec<u8> = std::iter::repeat_n([7u8, 8, 9, 255], 32 * 32)
            .flatten()
            .collect();
        let img = ico::IconImage::from_rgba_data(32, 32, px);
        let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
        dir.add_entry(ico::IconDirEntry::encode_as_png(&img).unwrap());
        let mut raw = Vec::new();
        dir.write(&mut raw).unwrap();
        crate::ui::ico::import(&raw).unwrap()
    }

    fn ico_buf() -> EditorBuffer {
        let mut buf = EditorBuffer::default();
        buf.preserved_appearance.icon = Some(mullion_store::IconSpec {
            kind: mullion_store::IconKind::Ico,
            value: real_ico(),
            bg: None,
        });
        buf
    }

    /// 走查 P2-7:界面上不该出现实现细节。用户不需要知道 egui 是什么,也不
    /// 需要知道「彩色字形」是什么 —— 提示里只该说「导入一个 .ico 换掉它」。
    #[test]
    fn the_ui_never_mentions_egui_or_its_limitations() {
        let mut buf = EditorBuffer::default();
        buf.preserved_appearance.icon = Some(mullion_store::IconSpec {
            kind: mullion_store::IconKind::Emoji,
            value: "🔥".into(),
            bg: None,
        });
        let out = run_appearance(&mut buf);
        for leak in ["egui", "剪影", "字形", "epaint"] {
            assert!(
                find_text_pos(&out.shapes, leak).is_none(),
                "界面上出现了实现细节 {leak:?}"
            );
        }
    }

    /// 老库里的 emoji 图标:**数据留着,但要当场告诉用户它不显示了**。
    ///
    /// 两半缺一不可。只留数据不提示,用户看到的是「我明明设过图标,列表里
    /// 却什么都没有」——那就是个 bug;只提示不留数据,等于我们替用户决定
    /// 删掉他存过的东西。
    ///
    /// 自证会变红:删掉 `appearance()` 里那段 `if let Some(old) = kind.filter(...)`,
    /// 第一段断言炸;把那段改成顺手 `icon = None`,第二段炸。
    #[test]
    fn an_old_emoji_icon_is_kept_but_the_user_is_told_it_no_longer_shows() {
        let mut buf = EditorBuffer::default();
        buf.preserved_appearance.icon = Some(mullion_store::IconSpec {
            kind: mullion_store::IconKind::Emoji,
            value: "🔥".into(),
            bg: None,
        });
        let out = run_appearance(&mut buf);
        assert!(
            find_text_pos(&out.shapes, "不再显示").is_some(),
            "旧图标不显示了,得当场说一声,否则看起来就是个 bug"
        );
        assert_eq!(
            buf.preserved_appearance.icon.as_ref().map(|i| i.kind),
            Some(mullion_store::IconKind::Emoji),
            "翻一下这一页不该把用户存过的图标抹掉"
        );
    }

    /// UI 上编辑不了的图标种类(`Builtin` 内置形状 v0.1.24 撤掉、`Custom` 从未
    /// 有 UI 产出过)**不该被静默抹掉**。这两个变体还在 store schema 里,
    /// 旧配置可能有值。
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
                bg: None,
            });
            run_appearance(&mut buf);
            assert_eq!(
                buf.preserved_appearance.icon.as_ref().map(|i| i.kind),
                Some(kind),
                "UI 编辑不了的 {kind:?} 图标不该因为翻了一下这一页就消失"
            );
        }
    }

    /// 点「导入 .ico…」只是**举手**,真正开对话框是 app 的事。
    ///
    /// 这条钉的是接线的存在性:标志位不置,按钮点了就没有任何反应。
    /// (不在 egui 闭包里同步开文件对话框的理由见 `app.rs::spawn_key_picker`
    /// ——那会把整个事件循环堵死,表现为「点一下窗口就卡住」。)
    ///
    /// 自证会变红:把 `buf.pick_icon_clicked = true` 那行删掉。
    #[test]
    fn clicking_import_raises_a_request_instead_of_opening_a_dialog_inline() {
        let t = crate::theme::MULLION_DARK;
        let mut buf = EditorBuffer::default();
        let mut err = None;
        let ctx = egui::Context::default();
        let mut run = |ctx: &egui::Context, buf: &mut EditorBuffer, input: egui::RawInput| {
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    super::appearance(ui, &t, buf, &mut err);
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
        let pos = find_text_pos(&out.shapes, "导入").expect("图标页要有导入按钮");
        assert!(!buf.pick_icon_clicked, "前提:还没点之前不该是 true");

        let _ = run(
            &ctx,
            &mut buf,
            egui::RawInput {
                events: vec![
                    egui::Event::PointerMoved(pos),
                    click(pos, true),
                    click(pos, false),
                ],
                ..Default::default()
            },
        );
        assert!(
            buf.pick_icon_clicked,
            "点了「导入」必须举手让 app 去开对话框"
        );
    }

    /// 「底色」那一行已下线(v0.1.28):图标底色跟节点色走,不再单独配。
    ///
    /// 第二段钉住「不再往库里写」:UI 没了但代码若还在某处塞 `DEFAULT_ICON_BG`,
    /// 用户看不见却被写进了配置文件,那是最难查的一类脏数据。
    ///
    /// 自证会变红:把那段底色 `ui.horizontal` 加回 `appearance()`。
    #[test]
    fn the_appearance_page_no_longer_offers_a_separate_icon_background() {
        let mut buf = ico_buf();
        let out = run_appearance(&mut buf);
        assert!(
            find_text_pos(&out.shapes, "底色").is_none(),
            "「底色」那一行该没了"
        );
        assert_eq!(
            buf.preserved_appearance
                .icon
                .as_ref()
                .and_then(|i| i.bg.as_ref()),
            None,
            "UI 下线后不该还有代码往库里写底色"
        );
    }

    /// 预览只剩 32px 一档 —— 列表三档现在都按 32 取图,继续预览 64 是在骗人。
    /// 「小尺寸下还认不认得出」正是选图标时唯一要判断的事,预览尺寸必须与列表
    /// 真实取图的尺寸一致。
    ///
    /// 自证会变红:把 `icon_preview` 的循环改回 `[SMALL, LARGE]`。
    #[test]
    fn the_icon_preview_shows_only_the_size_the_list_actually_uses() {
        let mut buf = ico_buf();
        let out = run_appearance(&mut buf);
        assert!(
            find_text_pos(&out.shapes, "32px").is_some(),
            "该有 32px 那一档"
        );
        assert!(
            find_text_pos(&out.shapes, "64px").is_none(),
            "64px 那一档该没了"
        );
    }

    /// 按「填色 + 正方形 + 边长等于给定值 + 在某条 y 分界线以上」过滤
    /// `Shape::Rect`。与 `list.rs` 的 `square_fill_count` 同一个填色/正方形
    /// 判据,这里多加两道:
    ///
    /// - 边长:图标预览页上色盘按钮(`interact_size` 40x18,非正方形)、
    ///   色板圆点(`circle_filled`,根本不是 `Shape::Rect`)都过不了前两重
    ///   筛选,但留着这道门槛以防将来页面上出现别的正方形色块。
    /// - y 分界线:本页下方「预览」分节(`preview_row`)另画了一份图标 ——
    ///   同样受 `ColorTarget::ListItem` 门控,但那是另一个早已存在、未被
    ///   本次改动触及的调用点。原始版本没有这道分界线,直接数全页方块
    ///   得到 2(顶部字段一块 + 底部「预览」一块),混进了不该数的那个,
    ///   导致断言写错。用「预览」这个分节标题的渲染位置(`section()` 精确
    ///   画出的那一行文字)当分界线,只数分界线以上——也就是「图标」字段
    ///   那一行——的方块,才是本 Task 真正要钉住的那次调用。
    fn square_fill_count_above(
        shapes: &[egui::epaint::ClippedShape],
        color: egui::Color32,
        side: f32,
        below_y: f32,
    ) -> usize {
        fn walk(s: &egui::Shape, color: egui::Color32, side: f32, below_y: f32) -> usize {
            match s {
                egui::Shape::Vec(v) => v.iter().map(|s| walk(s, color, side, below_y)).sum(),
                egui::Shape::Rect(r)
                    if r.fill == color
                        && (r.rect.width() - r.rect.height()).abs() < 0.5
                        && (r.rect.width() - side).abs() < 0.5
                        && r.rect.min.y < below_y =>
                {
                    1
                }
                _ => 0,
            }
        }
        shapes
            .iter()
            .map(|cs| walk(&cs.shape, color, side, below_y))
            .sum()
    }

    /// 复核挖出的真缺口:编辑器「图标」字段那一行预览的底色必须过
    /// `ColorTarget::ListItem` 这道闸门,不能随手用了别的落点 —— 复核实测:
    /// 把 `icon_preview` 调用处传入的 `ColorTarget::ListItem` 改成
    /// `ColorTarget::PaneTitle`,`cargo test --workspace` 546 项全过、
    /// 0 失败,没有任何测试会变红。与 `list.rs` 侧的
    /// `icon_backdrop_uses_the_list_item_target_not_pane_title` 同源:
    /// 「预览的是列表里的真实效果,不是理想效果」这条设计需要一张安全网。
    ///
    /// 自证会变红:把 `appearance()` 里 `icon_preview(...)` 调用传入的
    /// `ColorTarget::ListItem` 改成 `ColorTarget::PaneTitle`。
    #[test]
    fn the_icon_preview_backdrop_uses_the_list_item_target_not_pane_title() {
        use mullion_store::ColorTarget;
        // #1e88e5 不在项目调色板(`theme::LABEL_PALETTE`)里,避免跟主题色/
        // 背景色碰撞出假阳性——与 `list.rs` 同一条测试用的标记色一致。
        let marker = egui::Color32::from_rgb(0x1e, 0x88, 0xe5);
        let side = crate::ui::ico::SMALL as f32;

        let run_with = |apply_to: Vec<ColorTarget>| {
            let mut buf = ico_buf();
            buf.preserved_appearance.color = Some(mullion_store::ColorSpec {
                hex: "#1e88e5".to_string(),
                apply_to,
            });
            run_appearance(&mut buf)
        };

        let out_pt = run_with(vec![ColorTarget::PaneTitle]);
        let boundary_pt = find_text_pos(&out_pt.shapes, "预览")
            .expect("「预览」分节标题必须画出来,否则分界线本身就是假的")
            .y;
        assert_eq!(
            square_fill_count_above(&out_pt.shapes, marker, side, boundary_pt),
            0,
            "只勾了「pane 标题条」时,「图标」字段那行预览不该垫这个颜色的底"
        );

        let out_li = run_with(vec![ColorTarget::ListItem]);
        let boundary_li = find_text_pos(&out_li.shapes, "预览")
            .expect("「预览」分节标题必须画出来,否则分界线本身就是假的")
            .y;
        assert_eq!(
            square_fill_count_above(&out_li.shapes, marker, side, boundary_li),
            1,
            "勾了「会话列表」时,「图标」字段那行预览该恰好垫一块这个颜色的底"
        );
    }

    /// hex 与 `Color32` 的往返必须闭合:色盘吐 `Color32`、库里存 `#rrggbb`,
    /// 中间转错一次,用户选的颜色和显示出来的就不是同一个。
    #[test]
    fn a_colour_survives_the_round_trip_between_the_picker_and_the_hex_text() {
        for hex in ["#000000", "#ffffff", "#1e88e5", "#0a0b0c"] {
            let c = crate::theme::c32(crate::theme::parse_hex(hex).unwrap());
            assert_eq!(super::hex_of(c), hex, "{hex} 转回来变了样");
        }
    }

    /// 「形状」(v0.1.24 撤)和「emoji」(v0.1.26 撤)都已经不是图标载体了,
    /// 页面上不该还留着它们的入口 —— 留着的话用户会去点,点完什么也画不出来。
    /// 唯一的入口是「导入 .ico…」。
    #[test]
    fn the_appearance_page_only_offers_importing_an_ico() {
        let mut buf = EditorBuffer::default();
        let out = run_appearance(&mut buf);
        for gone in ["形状", "emoji"] {
            assert!(
                find_text_pos(&out.shapes, gone).is_none(),
                "「{gone}」模式已撤,不该还画在页面上"
            );
        }
        assert!(
            find_text_pos(&out.shapes, ".ico").is_some(),
            "「导入 .ico…」是现在唯一的图标入口"
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
        run_page(|ui| auth(ui, &t, buf, presence, &[], &[], &mut Default::default()))
    }

    fn run_automation(buf: &mut EditorBuffer) -> egui::FullOutput {
        let t = crate::theme::MULLION_DARK;
        run_page(|ui| automation(ui, &t, buf, &[]))
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

    /// 走查 20:认证页必须当场说清凭据存哪儿、怎么护着。不说的话,谨慎的人
    /// 干脆不用这个功能,每次连都手敲密码。
    ///
    /// 三条断言各守一件事:
    /// - 加密算法 + 主密钥托管方要出现 —— 这是「护着」的实际内容,只写一句
    ///   「已加密」等于没说
    /// - 全页不许出现「明文」二字 —— 凭据**不是**明文存的(`mullion-store`
    ///   的 `crypto.rs` 是 XChaCha20-Poly1305,`master_key.rs` 走 OS keyring),
    ///   在自证清白的地方栽赃自己是最坏的一种文案错误
    ///
    /// 自证会变红:删掉 `auth()` 末尾那段 `ui.label(SECRET_STORAGE_NOTE)`,
    /// 前两段断言炸;把常量里的「加密后存进」改成「明文存进」,第三段炸。
    #[test]
    fn the_auth_page_says_where_secrets_go_and_never_calls_them_plaintext() {
        let mut buf = EditorBuffer::default();
        let out = run_auth(&mut buf, SecretPresence::default());

        assert!(
            find_text_pos(&out.shapes, "XChaCha20-Poly1305").is_some(),
            "认证页要写明用的什么加密"
        );
        assert!(
            find_text_pos(&out.shapes, "Windows 凭据管理器").is_some(),
            "认证页要写明主密钥交给谁保管"
        );
        assert!(
            find_text_pos(&out.shapes, "明文").is_none(),
            "凭据不是明文存的,界面上不许这么写"
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

    /// 「高级」页已并入「连接」页(走查 P1-8),不再是独立标签页。这条现在守的是
    /// 「跳板不会在代理分区里重复出现」:`network()` 只画代理,不该带出跳板 ——
    /// 否则两处都有跳板,用户改了一处以为生效、另一处显示的还是旧值。
    #[test]
    fn jump_section_appears_once_on_the_connect_page_and_not_inside_the_proxy_section() {
        let mut buf = EditorBuffer::default();
        let out = run_basic(&mut buf, &[]);
        assert!(
            find_text_pos(&out.shapes, "跳板").is_some(),
            "「连接」页必须有跳板分节"
        );
        // 走查 19 起统一叫「继承」——旧文案「继承分组」在未分组时是错的
        // (上游根本不是分组)。这条测试守的是「三个模式按钮都在」,
        // 文案变了意图没变。
        for opt in ["无", "继承", "自定义"] {
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
                    // 每帧新开一个游标,模拟 `network()` 被单独挂到某个新
                    // 页面顶部渲染的场景(见下面
                    // `network_rendered_alone_with_a_fresh_cursor_does_not_
                    // draw_a_leading_divider`)。
                    let mut first = true;
                    network(ui, &t, buf, &[], SecretPresence::default(), &mut first);
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

    /// 走查 12:环要在**编辑时**就报出来,而不是等拨号才硬失败。
    /// 这条守的是接线 —— `jump_preview` 的纯函数测试证明判据对,这条证明
    /// 判据真的画到了链编辑器下面。顺带守住路径预览也在。
    #[test]
    fn a_cyclic_jump_chain_is_flagged_while_editing() {
        let mut hop = sess(2, "hop-alpha", "10.0.0.2");
        // hop-alpha 的跳板是它自己 —— 链一展开就成环。
        hop.network.jump = Some(vec![mullion_store::JumpRef(SessionId(2))]);
        let sessions = [sess(1, "self-session", "10.0.0.1"), hop];

        let mut buf = EditorBuffer {
            name: "web01".into(),
            jump_mode: JumpModeUi::Custom,
            jump_chain: vec![SessionId(2)],
            ..EditorBuffer::default()
        };
        let out = run_basic(&mut buf, &sessions);
        let texts = all_text(&out.shapes);

        assert!(
            texts
                .iter()
                .any(|s| s.contains("本机") && s.contains("web01")),
            "自定义链下必须画出连接路径预览:{texts:?}"
        );
        assert!(
            texts.iter().any(|s| s.contains("环")),
            "成环的链必须在编辑时就报出来,不能等拨号:{texts:?}"
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
                    basic(
                        ui,
                        &t,
                        buf,
                        &[],
                        &sessions,
                        &[],
                        Some(SessionId(1)),
                        SecretPresence::default(),
                        false,
                        &mut Default::default(),
                    );
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

    /// 图标按钮改自绘后(走查 P0-5),`find_all_text_pos` 找不到文字了 ——
    /// 换成按**笔画形状**本身定位:`Glyph::Cross` 是两条**中点重合且斜率
    /// 异号**的 `Shape::LineSegment`(同 `icon.rs` 里
    /// `cross_is_two_segments_that_actually_cross` 的判据 `k0 * k1 < 0.0`;
    /// 单条线段不算数 —— egui 自己的 `ui.separator()`
    /// (`egui-0.30.0 src/widgets/separator.rs:117`)也是单条
    /// `Shape::LineSegment`,只看「落在带宽内的线段」会把分隔线误判成
    /// Cross 的一半);`Glyph::ArrowUp`/`ArrowDown` 是一条三点
    /// `Shape::Path`,靠「哪个端点单独出现在纵向极值」分上下(同 `icon.rs`
    /// 里 `arrow_up_points_up_and_arrow_down_points_down` 的判据)。返回值
    /// 按 x 升序 —— `right_to_left` 布局下从左到右恒定是 ↑ / ↓ / ✕。
    ///
    /// **已知碰撞,不是本函数独有特征**:三点 `Shape::Path` 不只有本页的
    /// 箭头会产生 —— egui 内置 `Checkbox` 勾选态的对勾(`egui-0.30.0
    /// src/widgets/checkbox.rs:121`,`Shape::line(vec![左中, 下中, 右上],
    /// stroke)`)同样是三点 Path,复核实测过会被误判成 `FoundGlyph::Down`。
    /// 本函数只在「调用方划定的 `row_y ± tol` 范围内没有勾选框」这个前提下
    /// 成立;当前两处调用点 `row_y ≈ 312~354`、`tol = 12.0`,页面上离得够远
    /// 才没炸。放大 `tol` 或把这个函数挪去别的页面复用前,必须重新确认目标
    /// 带宽内没有勾选框,不能想当然。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum FoundGlyph {
        Up,
        Down,
        Cross,
    }

    fn icon_buttons_on_row(
        shapes: &[egui::epaint::ClippedShape],
        row_y: f32,
        tol: f32,
    ) -> Vec<(egui::Pos2, FoundGlyph)> {
        // Cross 的两条线段分开收集、配对后再产出:遍历到单独一条线段时,
        // 没法当场判断它是不是 Cross 的一半(还是一条无关的分隔线)。
        let mut segments: Vec<(egui::Pos2, f32)> = Vec::new(); // (中点, 斜率)
        let mut out: Vec<(egui::Pos2, FoundGlyph)> = Vec::new();

        fn walk(
            s: &egui::Shape,
            row_y: f32,
            tol: f32,
            segments: &mut Vec<(egui::Pos2, f32)>,
            out: &mut Vec<(egui::Pos2, FoundGlyph)>,
        ) {
            match s {
                egui::Shape::Vec(v) => v.iter().for_each(|x| walk(x, row_y, tol, segments, out)),
                egui::Shape::LineSegment { points, .. } => {
                    let mid = egui::pos2(
                        (points[0].x + points[1].x) / 2.0,
                        (points[0].y + points[1].y) / 2.0,
                    );
                    if (mid.y - row_y).abs() <= tol {
                        let dx = points[1].x - points[0].x;
                        // 垂直线(dx=0)理论上不会出现在本项目的图标笔画里,
                        // 用 +∞ 兜底,不让除零把斜率变成 NaN 破坏后面的比较。
                        let slope = if dx.abs() < f32::EPSILON {
                            f32::INFINITY
                        } else {
                            (points[1].y - points[0].y) / dx
                        };
                        segments.push((mid, slope));
                    }
                }
                egui::Shape::Path(p) if p.points.len() == 3 => {
                    let cy = (p.points[0].y + p.points[1].y + p.points[2].y) / 3.0;
                    if (cy - row_y).abs() <= tol {
                        let cx = (p.points[0].x + p.points[1].x + p.points[2].x) / 3.0;
                        // chevron 的三个端点里,两个「底边」端点纵坐标相等,
                        // 「尖端」那个单独出现在极值上 —— 尖端在最小值 → 朝上
                        // (Up),在最大值 → 朝下(Down)。
                        let min_y = p.points.iter().map(|q| q.y).fold(f32::INFINITY, f32::min);
                        let apex_at_min = p
                            .points
                            .iter()
                            .filter(|q| (q.y - min_y).abs() < 0.01)
                            .count()
                            == 1;
                        let glyph = if apex_at_min {
                            FoundGlyph::Up
                        } else {
                            FoundGlyph::Down
                        };
                        out.push((egui::pos2(cx, cy), glyph));
                    }
                }
                _ => {}
            }
        }
        shapes
            .iter()
            .for_each(|cs| walk(&cs.shape, row_y, tol, &mut segments, &mut out));

        // 按「中点精确重合 + 斜率异号」配对:两条都满足才算一个 Cross,
        // 落单的线段(比如误扫进来的 `ui.separator()`)直接丢弃,不产出。
        let mut used = vec![false; segments.len()];
        for i in 0..segments.len() {
            if used[i] {
                continue;
            }
            for j in (i + 1)..segments.len() {
                if used[j] {
                    continue;
                }
                let same_mid = (segments[i].0 - segments[j].0).length() < 0.01;
                let crosses = segments[i].1 * segments[j].1 < 0.0;
                if same_mid && crosses {
                    used[i] = true;
                    used[j] = true;
                    out.push((segments[i].0, FoundGlyph::Cross));
                    break;
                }
            }
        }

        out.sort_by(|a, b| a.0.x.total_cmp(&b.0.x));
        out
    }

    /// code review 复核意见 2:落单的 `Shape::LineSegment`(比如 egui 自己的
    /// `ui.separator()`)不该被误判成 Cross —— 必须「中点重合 + 斜率异号」
    /// 成对出现才算数。不经过真实 UI、不依赖 `chain_editor` 布局,直接构造
    /// 一条合成的孤立线段丢给 `icon_buttons_on_row`,专门测判据本身够不够
    /// 紧,不测集成。
    #[test]
    fn icon_buttons_on_row_ignores_a_lone_line_segment_that_looks_like_half_a_separator() {
        let lone_separator = egui::epaint::ClippedShape {
            clip_rect: egui::Rect::EVERYTHING,
            shape: egui::Shape::LineSegment {
                points: [egui::pos2(0.0, 100.0), egui::pos2(50.0, 100.0)],
                stroke: egui::Stroke::new(1.0, egui::Color32::WHITE).into(),
            },
        };
        let buttons = icon_buttons_on_row(std::slice::from_ref(&lone_separator), 100.0, 12.0);
        assert!(
            buttons.is_empty(),
            "落单的线段不该被判成任何图标按钮 —— 只有中点重合 + 斜率异号的一对\
             才算 Cross;实际 {buttons:?}"
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
                    basic(
                        ui,
                        &t,
                        buf,
                        &[],
                        &sessions,
                        &[],
                        Some(SessionId(1)),
                        SecretPresence::default(),
                        false,
                        &mut Default::default(),
                    );
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

        // 第二跳是 hop-beta(jump_chain = [2, 3, 4]);用它的名字标签定位所在行,
        // 再按 x 升序取该行第一个按钮 —— `right_to_left` 布局下最左边是「↑」。
        // tol=12.0:复核实测三跳行距 21.0px(y=312.175/333.175/354.175),半程
        // 10.5px < 12.0,冗余为负,只是当前每次只扫单一目标行才没有误吸邻行 ——
        // 行高若被压缩,这个值需要重新核对。
        let row_y = find_text_pos(&out.shapes, "hop-beta")
            .expect("hop-beta 应该出现在第二跳这一行")
            .y;
        let buttons = icon_buttons_on_row(&out.shapes, row_y, 12.0);
        assert_eq!(buttons.len(), 3, "第二跳这一行应该有 3 个图标按钮");
        assert_eq!(buttons[0].1, FoundGlyph::Up, "最左边应该是「↑」");
        assert_eq!(buttons[1].1, FoundGlyph::Down, "中间应该是「↓」");
        let second = buttons[0].0;

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
                    basic(
                        ui,
                        &t,
                        buf,
                        &[],
                        &sessions,
                        &[],
                        Some(SessionId(1)),
                        SecretPresence::default(),
                        false,
                        &mut Default::default(),
                    );
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

        // 中间跳是 hop-beta(jump_chain = [2, 3, 4]);用它的名字标签定位所在行,
        // 再按 x 升序取该行最后一个按钮 —— `right_to_left` 布局下最右边是「✕」。
        // tol=12.0:同上一条测试,复核实测行距 21.0px、半程 10.5px < 12.0,
        // 冗余为负,靠每次只扫单一目标行才不误吸邻行。
        let row_y = find_text_pos(&out.shapes, "hop-beta")
            .expect("hop-beta 应该出现在中间这一行")
            .y;
        let buttons = icon_buttons_on_row(&out.shapes, row_y, 12.0);
        assert_eq!(buttons.len(), 3, "中间这一行应该有 3 个图标按钮");
        assert_eq!(buttons[1].1, FoundGlyph::Down, "中间应该是「↓」");
        assert_eq!(buttons[2].1, FoundGlyph::Cross, "最右边应该是「✕」");
        let middle = buttons[2].0;

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

    /// 走查 P0-5。老写法用 `ui.button("✕")` —— U+2715 不在 egui 内置
    /// 拉丁字体里,也不在微软雅黑里,实机渲染成豆腐块 □,用户完全看不出
    /// 是「删除」。改成自绘后,页面上不该再有任何这三个字符的文字形状。
    #[test]
    fn jump_row_buttons_are_drawn_not_typed_so_they_cannot_render_as_tofu() {
        let sessions = [
            sess(1, "self-session", "10.0.0.1"),
            sess(2, "hop-alpha", "10.0.0.2"),
            sess(3, "hop-beta", "10.0.0.3"),
        ];
        let mut buf = EditorBuffer {
            jump_mode: JumpModeUi::Custom,
            jump_chain: vec![SessionId(2), SessionId(3)],
            ..Default::default()
        };
        let out = run_basic(&mut buf, &sessions);
        for ch in ["✕", "↑", "↓"] {
            assert!(
                find_text_pos(&out.shapes, ch).is_none(),
                "页面上还有文字形状 {ch:?} —— 它在真机上是豆腐块,必须改成自绘"
            );
        }
    }

    /// 所有形状的最右边界。无穷/NaN 的(整屏底色之类)跳过。
    ///
    /// 用它而不是「找某个控件的 Response」:走查 P0-1 的症状是**画出去了**
    /// 被 clip_rect 裁掉,`Response.rect` 反而看不出问题 —— 形状边界才看得出。
    fn max_right(shapes: &[egui::epaint::ClippedShape]) -> f32 {
        fn walk(s: &egui::Shape, acc: &mut f32) {
            if let egui::Shape::Vec(v) = s {
                v.iter().for_each(|x| walk(x, acc));
                return;
            }
            let r = s.visual_bounding_rect();
            if r.is_finite() && r.right() > *acc {
                *acc = r.right();
            }
        }
        let mut acc = f32::MIN;
        shapes.iter().for_each(|cs| walk(&cs.shape, &mut acc));
        acc
    }

    /// 在给定面板宽与 DPI 下跑**两帧**任意一页,返回第二帧输出。
    /// 页级越界测试(`*_never_paints_past_the_panel_*`)全部走这一个入口。
    ///
    /// 抽出来不是为了少写几行:下面 `native_pixels_per_point` 和
    /// `set_clip_rect(EVERYTHING)` 各自都有一段踩坑史,而漏掉任何一条都
    /// **不会报错,只会静默量错**(前者量出 8000,后者量出「一切正常」)。
    /// 复制粘贴第四份迟早漏。
    ///
    /// 跑两帧的理由同 `run_appearance`:第一帧的布局是估的。
    fn run_page_at(width: f32, ppp: f32, mut page: impl FnMut(&mut egui::Ui)) -> egui::FullOutput {
        let ctx = egui::Context::default();
        let input = || {
            let mut raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 600.0),
                )),
                ..Default::default()
            };
            // **不要**改成 `ctx.set_pixels_per_point(ppp)`——实测过会崩:那条
            // API 是 `Context::set_zoom_factor` 的包装,只在**下一帧**生效
            // (egui-0.30.0 context.rs:1994 起),生效那一帧会把「上一帧的
            // screen_rect」按新旧 ppp 之比重新缩放后盖掉本帧显式传入的
            // screen_rect(同文件 462-475 行,注释自称是给「zoom 抖动」擦屁股)。
            // 对全新 `Context`,「上一帧」是 egui 内置的默认占位符
            // 10_000×10_000(`input_state/mod.rs:247`),1.0/1.25 的比例缩出
            // 8000×8000——热身帧的 `Grid`(`sm_basic_group` 等)就在这个虚假的
            // 巨宽画布上把列宽记忆定成了 8000,而 `Grid` 的列宽记忆是跨帧
            // 累积在同一个 `ctx.memory` 里的,第二帧(真正拿来断言的那帧)
            // 即使 screen_rect 已经正确回落到 300×600,也会把这份 8000 的
            // 记忆当「历史最大宽度」继续撑着,分区分隔线就被撑到 x=8000。
            // 这是测试脚手架的坑,不是生产代码 bug——已用 `eprintln!` 探针
            // 核实过 `ctx.screen_rect()` 在第二帧确实是 300×600,越界的是
            // `Grid` 记忆,不是 screen_rect。
            // 改用 `ViewportInfo.native_pixels_per_point` 直接给当前这一帧
            // 设 ppp,不经过 `set_zoom_factor`/抖动规避那条路径,两帧
            // screen_rect 全程等于我们显式传入的值。
            raw.viewports
                .get_mut(&raw.viewport_id)
                .expect("RawInput::default() 必须自带 ROOT viewport 项")
                .native_pixels_per_point = Some(ppp);
            raw
        };
        let run = |page: &mut dyn FnMut(&mut egui::Ui)| {
            ctx.run(input(), |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::none())
                    .show(ctx, |ui| {
                        // CentralPanel 会把 `ui` 的 clip_rect 钉死在面板矩形上
                        // (egui-0.30.0 panel.rs:1109,注释明写着"If we overflow,
                        // don't do so visibly (#4475)")——这本是 egui 的保护特性,
                        // 副作用是 `Label`/`Button` 等控件在 paint 前会先查
                        // `ui.is_rect_visible(rect)`(`rect.intersects(clip_rect)`),
                        // 一旦控件整个落在 clip_rect 外面就直接不产生 Shape,压根
                        // 进不了 `FullOutput::shapes`。这正是走查 P0-1 在真实实现里
                        // "肉眼看不出溢出"的原因,但也让 `max_right` 这种"扫
                        // shapes 找最右边界"的测量对**完全**越界的控件失明
                        // (对**部分**越界、被 GPU scissor 削掉半个字的控件仍然
                        // 有效——那种情况 shape 还在,只是显示时被裁)。这里把
                        // clip_rect 撑到无穷大,只是为了让测量拿到"控件本该画在
                        // 哪"的真实几何,不代表生产代码的裁剪行为被关掉。
                        ui.set_clip_rect(egui::Rect::EVERYTHING);
                        page(ui);
                    });
            })
        };
        let _ = run(&mut page);
        run(&mut page)
    }

    /// 在给定的面板宽度下跑两帧「认证」页,返回第二帧输出。
    fn run_auth_at(
        width: f32,
        presence: SecretPresence,
        buf: &mut EditorBuffer,
    ) -> egui::FullOutput {
        let t = crate::theme::MULLION_DARK;
        run_page_at(width, 1.0, |ui| {
            auth(ui, &t, buf, presence, &[], &[], &mut Default::default())
        })
    }

    /// **走查 P0-1 的守护测试。**
    ///
    /// 老写法:`TextEdit::singleline(value).desired_width(f32::INFINITY)`
    /// 吃光整行,后面的「已设置(不修改则保持不变)」被推到面板外,
    /// 只露出半个字。右栏被分隔条拖窄时更狠。
    ///
    /// 300.0 不是随手挑的:分隔条拖到 `LIST_MAX_W = 440` 时右栏内容宽
    /// 实测约 300px,是本项目真实可达的最窄值。
    #[test]
    fn password_row_never_paints_past_the_panel_even_at_the_narrowest_pane() {
        for width in [300.0f32, 440.0, 900.0] {
            let mut buf = EditorBuffer {
                auth_kind: AuthKindUi::Password,
                ..Default::default()
            };
            let presence = SecretPresence {
                password: true,
                ..Default::default()
            };
            let out = run_auth_at(width, presence, &mut buf);
            let right = max_right(&out.shapes);
            assert!(
                right <= width + 0.5,
                "面板宽 {width},却画到了 x={right} —— 密码框右边的附属控件被裁了"
            );
        }
    }

    /// 同上,但走「已改过」分支(touched = true) —— 那一支右边挂的是
    /// 「撤销」按钮 + 「留空 = 清除已存凭据」,加起来比另外两支都长。
    #[test]
    fn touched_password_row_fits_the_revert_button_at_the_narrowest_pane() {
        for width in [300.0f32, 440.0] {
            let mut buf = EditorBuffer {
                auth_kind: AuthKindUi::Password,
                password_touched: true,
                ..Default::default()
            };
            let out = run_auth_at(width, SecretPresence::default(), &mut buf);
            let right = max_right(&out.shapes);
            assert!(
                right <= width + 0.5,
                "面板宽 {width},却画到了 x={right} —— 「撤销」被裁了"
            );
        }
    }

    /// **走查 P0-2 的守护测试。** 名称框不该横跨整行。
    #[test]
    fn medium_fields_are_capped_and_do_not_span_the_whole_row() {
        // 原计划文本这里写的是「直接调 metrics::field_w() 存进 widths[0] 再跟
        // FIELD_W_M 比」——自证会发现那是重言式:不管 `basic()` 里的名称框
        // 是不是真按 `field_w` 收窄,`widths[0]` 都只是重新调用同一个纯函数,
        // 断言恒真(在 metrics.rs 里已经单测过这个函数本身)。改成量
        // `basic()` **实际画出来**的名称输入框右边界——这才是本测试要守的
        // 东西:900px 宽的面板下,名称框不该伸到接近整行(未修时会到 ~880),
        // 修完该停在 `LABEL_COL_W + 间距 + FIELD_W_M` 附近(~430)。
        let mut buf = EditorBuffer::default();
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let run = |buf: &mut EditorBuffer| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(900.0, 600.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::none())
                        .show(ctx, |ui| {
                            // 同 `run_auth_at` 那段注释:CentralPanel 把 clip_rect
                            // 钉死在面板矩形上,越界控件会被 `is_rect_visible`
                            // 挡在 shapes 之外,量不到真实溢出。撑到无穷大只是
                            // 为了测量,不代表关掉生产代码的裁剪行为。
                            ui.set_clip_rect(egui::Rect::EVERYTHING);
                            basic(
                                ui,
                                &t,
                                buf,
                                &[],
                                &[],
                                &[],
                                None,
                                SecretPresence::default(),
                                false,
                                &mut Default::default(),
                            );
                        });
                },
            )
        };
        let _ = run(&mut buf);
        let out = run(&mut buf);

        let label_pos = find_text_pos(&out.shapes, "名称").expect("「基本」分区必须有「名称」标签");
        // 名称输入框跟标签同一 Grid 行,取标签所在行的高度范围(±10px 足够
        // 覆盖 singleline TextEdit 的行高,不会越界勾到「主机」那一行)扫最右
        // 边界——这样才是量输入框本身的画框宽度,不是全页面(全页面会被
        // 「备注」多行框的合法全宽拉到 900 附近,盖住这条测试想抓的问题)。
        let mut row_right = f32::MIN;
        fn walk(s: &egui::Shape, y: f32, tol: f32, acc: &mut f32) {
            match s {
                egui::Shape::Vec(v) => v.iter().for_each(|x| walk(x, y, tol, acc)),
                other => {
                    let r = other.visual_bounding_rect();
                    if r.is_finite() && (r.center().y - y).abs() <= tol && r.right() > *acc {
                        *acc = r.right();
                    }
                }
            }
        }
        out.shapes
            .iter()
            .for_each(|cs| walk(&cs.shape, label_pos.y + 4.0, 10.0, &mut row_right));

        assert!(
            row_right < 500.0,
            "900px 宽的面板里,名称框右边界画到了 x={row_right} —— \
             FIELD_W_M(320)没有生效,框还在往整行撑"
        );
    }

    /// 走查 P1-8:「高级」页只有一行代理,右侧 70% 全是空白。
    /// 合并后代理必须出现在「连接」页上。
    #[test]
    fn proxy_settings_render_on_the_connect_page_after_the_merge() {
        let mut buf = EditorBuffer {
            proxy_mode: ProxyModeUi::Socks5,
            ..Default::default()
        };
        let out = run_basic(&mut buf, &[]);
        assert!(
            find_text_pos(&out.shapes, "代理地址").is_some(),
            "选了 SOCKS5 却在「连接」页上找不到代理地址 —— 合并没做完"
        );
    }

    /// 代理排在跳板之前:连接路径是 本机 →(代理)→ 第一跳 →…→ 目标,
    /// 页面自上而下得按这个顺序,阶段 2 的「连接路径预览」才读得通。
    #[test]
    fn proxy_section_comes_before_the_jump_section() {
        let mut buf = EditorBuffer {
            proxy_mode: ProxyModeUi::Socks5,
            jump_mode: JumpModeUi::Custom,
            ..Default::default()
        };
        let out = run_basic(&mut buf, &[]);
        let proxy = find_text_pos(&out.shapes, "代理").expect("找不到代理分区");
        let jump = find_text_pos(&out.shapes, "跳板").expect("找不到跳板分区");
        assert!(
            proxy.y < jump.y,
            "代理 y={} 排在跳板 y={} 下面了",
            proxy.y,
            jump.y
        );
    }

    /// 在给定面板宽与 DPI 下跑两帧「连接」页,返回第二帧输出。
    fn run_basic_at(width: f32, ppp: f32, buf: &mut EditorBuffer) -> egui::FullOutput {
        let t = crate::theme::MULLION_DARK;
        run_page_at(width, ppp, |ui| {
            basic(
                ui,
                &t,
                buf,
                &[],
                &[],
                &[],
                None,
                SecretPresence::default(),
                false,
                &mut Default::default(),
            )
        })
    }

    /// **走查验收标准第一条的自动化部分,常规内容分支。**
    ///
    /// 这条是缺陷 A(代理模式四个按钮在窄栏放不下,`ui.horizontal` 不换行
    /// 撑宽了这一格,把「跳板」分区的分隔线顶出面板)的守护测试——字段值
    /// 都是短的默认值,不掺长名称/长主机,这样越界只可能来自窄栏本身的
    /// 控件布局,不会被 egui 单行 `TextEdit` 撑宽 `Ui` 的已知行为(见下一条
    /// 测试)掩盖或误报。
    ///
    /// 注意边界:egui 的布局全程以「点」为单位,`pixels_per_point` 只在
    /// 光栅化时生效,所以三档 DPI 的布局矩形基本一致 —— 本测试守的是
    /// 字形栅格化的取整漂移。「125%/150% 截图无错位」仍是人工验收项。
    ///
    /// 300.0 是分隔条拖到 `LIST_MAX_W = 440` 时右栏内容宽的实测值,
    /// 是本项目真实可达的最窄面板。
    #[test]
    fn connect_page_never_paints_past_the_panel_at_any_width_or_dpi() {
        for width in [300.0f32, 440.0, 900.0] {
            for ppp in [1.0f32, 1.25, 1.5] {
                let mut buf = EditorBuffer {
                    proxy_mode: ProxyModeUi::Socks5,
                    jump_mode: JumpModeUi::Custom,
                    ..Default::default()
                };
                let out = run_basic_at(width, ppp, &mut buf);
                let right = max_right(&out.shapes);
                assert!(
                    right <= width + 0.5,
                    "面板宽 {width} @ {ppp}x,却画到了 x={right}"
                );
            }
        }
    }

    /// **长内容分支:越界不可避免,但有实测上限——不是「只有分隔线会越界」。**
    ///
    /// 原计划文本的判据是「豁免水平分隔线,断言其余形状 <= width + 0.5」,
    /// 基于的假设是「只有 `section()` 画的分隔线会被撑出面板」。**这个假设
    /// 已用本测试自己的探针证伪**:实际测到的最右形状是几个 `Shape::Rect`
    /// (「备注」多行框、代理/跳板区后续的输入框背景等的画框矩形),
    /// 跟分隔线基本打平(三档 DPI 下都只低 0.5pt,从未反超)——不是分隔线
    /// 一家在越界,是**整个「连接」页
    /// 这个 `Ui` 都被撑宽了**,越界表现在所有跟在撑宽源后面的控件上,豁免
    /// 分隔线这一种形状类型拦不住。
    ///
    /// 撑宽的根因是 egui-0.30.0 `TextEdit` 的两处上游行为叠加,且已实测
    /// 证明修不掉:
    ///
    /// 1. `widgets/text_edit/builder.rs:514-521`:singleline 走
    ///    `LayoutJob::simple_singleline(...)`,**忽略 `wrap_width`**,galley
    ///    永远按完整文本宽度排版,`desired_width`/`clip_text` 都不影响它。
    /// 2. `widgets/text_edit/builder.rs:726-734`:`extra_size = galley.size()
    ///    - rect.size()`,只要内容比框宽就 `ui.allocate_rect(...)` 额外占位,
    ///    把父 `Ui` 的 `min_rect`/`max_rect` 撑宽——这一撑不只影响撑宽的
    ///    那个 `TextEdit` 自己,后面同一个 `Ui` 作用域里的所有兄弟控件
    ///    (Grid 的下一行、`section()` 的分隔线……)都会按撑宽后的
    ///    `available_width` 重新计算,越界因此“到处都是”而不是一处。
    ///
    /// 试过两种拦截方式,**都无效**(已实测):给 `field_w(...)` 加
    /// `TEXT_EDIT_MARGIN_X` 预留(无变化)、给 `grid()` 加
    /// `.max_col_width(328.0)`(无变化)——`extra_size` 是在 `TextEdit`
    /// 内部直接对父 `Ui` 调用 `allocate_rect`,不经过 `Grid`/`field_w` 算出
    /// 的宽度,拦不住。
    ///
    /// **实测数据**(`max_right`,含分隔线,三档 DPI):
    /// - 300px:342.5 / 349.5 / 348.5——**只有这一档真的越界**,300 是分隔条
    ///   拖到 `LIST_MAX_W` 时右栏内容宽的实测最窄值,峰值约 +49.5px。
    /// - 440px / 900px:440.5 / 900.5(三档 DPI 一致)——**完全不越界**,富余
    ///   空间够把撑宽的内容仍旧包在面板里。
    ///
    /// 因此本测试**不能**照抄「>= width + 0.5」的紧公差(那是伪造出来的绿,
    /// 实测过不了),但也**不能**放到没有上限——300px 这一档给一个比实测峰值
    /// 349.5 略宽的余量(352.0,+2.5px),440/900px 维持跟其它守护测试一致的
    /// 紧公差。阈值本身就是「已知上限」:真出现新的越界(比如又一个像
    /// 缺陷 A 那样的窄栏溢出,或者 `run_basic_at` 又踩中另一个 egui 版本
    /// 差异),数字会明显超过这个已知上限,测试才会红。
    ///
    /// 实机后果:被撑宽的只有「连接」页布局账本上的数字,`CentralPanel` 的
    /// clip 会把画出面板的部分裁掉(生产代码没有关掉裁剪,只有测试用
    /// `ui.set_clip_rect(EVERYTHING)` 才看得见这些形状),肉眼不可见——
    /// 后果轻微,但布局数字确实是错的,后来人要不要修可以拿这条注释当依据。
    #[test]
    fn connect_page_long_content_stays_within_a_measured_bound() {
        for width in [300.0f32, 440.0, 900.0] {
            for ppp in [1.0f32, 1.25, 1.5] {
                let mut buf = EditorBuffer {
                    proxy_mode: ProxyModeUi::Socks5,
                    jump_mode: JumpModeUi::Custom,
                    name: "一个相当长的会话名称用来把标签列撑开".into(),
                    host: "very-long-hostname.internal.example.com".into(),
                    ..Default::default()
                };
                let out = run_basic_at(width, ppp, &mut buf);
                let right = max_right(&out.shapes);
                // 见上面文档注释的「实测数据」:只有 300px 这一档真的越界,
                // 峰值 349.5,留 2.5px 余量;440/900px 没有理由越界,维持
                // 紧公差,能第一时间抓到「怎么突然在宽面板下也越界了」。
                let tolerance = if width <= 300.0 { 52.0 } else { 0.5 };
                assert!(
                    right <= width + tolerance,
                    "面板宽 {width} @ {ppp}x,画到了 x={right},超出已知上限 \
                     (width + {tolerance})—— 已知的 egui TextEdit 撑宽不会到这个\
                     地步,这可能是新的越界,需要排查"
                );
            }
        }
    }

    /// **`SecretPresence.proxy_password` 的两个非空分支此前从未被执行过。**
    ///
    /// 「认证」页的 password / passphrase 都有守护测试,代理口令这条没有:
    /// 全项目找不到一处把 `proxy_password` 设成 `true` 的测试,`secret_edit`
    /// 在这一格的宽度算错、或者接线接到了别的字段上,都不会有测试变红。
    /// 它跟前两条走的是同一个 `secret_edit`,但可用宽是在「代理」分区里算的
    /// (同一格前面还有 `horizontal_wrapped` 的模式按钮排),不能靠前两条推断。
    ///
    /// 两个分支都要:`touched=false` 走「已存值」占位分支(必须出现
    /// 「已设置」说明文字),`touched=true` 走可编辑分支(必须出现「撤销」)。
    #[test]
    fn proxy_password_row_renders_both_stored_branches_and_fits_the_narrowest_pane() {
        for touched in [false, true] {
            let t = crate::theme::MULLION_DARK;
            let mut buf = EditorBuffer {
                proxy_mode: ProxyModeUi::Socks5,
                proxy_password_touched: touched,
                ..Default::default()
            };
            let presence = SecretPresence {
                proxy_password: true,
                ..Default::default()
            };
            let out = run_page_at(300.0, 1.0, |ui| {
                let mut first = true;
                network(ui, &t, &mut buf, &[], presence, &mut first);
            });
            let right = max_right(&out.shapes);
            assert!(
                right <= 300.5,
                "touched={touched} 时代理口令行画到了 x={right}"
            );
            // 找的是**说明文字/按钮**而不是输入框:输入框在两个分支里长得
            // 一模一样(都是 password 遮罩),只有这两处文字能区分走的是哪支。
            let needle = if touched { "撤销" } else { "已设置" };
            assert!(
                find_text_pos(&out.shapes, needle).is_some(),
                "touched={touched} 时没渲染出「{needle}」—— \
                 presence.proxy_password 没接到 secret_edit 上"
            );
        }
    }

    /// **走查 P0-1 在「登录后」页的同构缺陷。**
    ///
    /// 阶段 1 只认领了「基本/身份/代理」三页,这一页留着裸
    /// `desired_width(240.0/140.0/220.0)` 和两处 `f32::INFINITY`。最狠的是
    /// 「登录后命令」那一行:一个定宽 240 的输入框后面还串着「延时」勾选框、
    /// 延时数值框和 ↑/↓/✕ 三个图标按钮,`ui.horizontal` 又不换行 ——
    /// 右栏被分隔条拖窄到 300px 时,最右边的「✕」直接被推出面板,
    /// **用户没有任何办法删掉一条命令**。
    ///
    /// 字段值一律用短内容:越界只能来自控件布局本身,不会跟 egui 单行
    /// `TextEdit` 被长文本撑宽的已知上游行为(见
    /// `connect_page_long_content_stays_within_a_measured_bound`)混淆。
    #[test]
    fn automation_page_never_paints_past_the_panel_at_any_width_or_dpi() {
        for width in [300.0f32, 440.0, 900.0] {
            for ppp in [1.0f32, 1.25, 1.5] {
                let t = crate::theme::MULLION_DARK;
                let mut buf = EditorBuffer::default();
                let a = &mut buf.preserved_automation;
                a.tmux = Some(mullion_store::TmuxChoice::Attach { session_name: None });
                a.work_dir = Some("/tmp".to_string());
                // `delay_ms` 给上值,那一行才会多出一个 `DragValue` —— 它是
                // 这页最宽的一行,也是最容易把 ✕ 顶出去的那一行。
                a.commands = Some(vec![mullion_store::AutomationCommand {
                    text: "ls".to_string(),
                    delay_ms: Some(500),
                }]);
                a.env = Some(vec![mullion_store::EnvVar {
                    key: "K".to_string(),
                    value: "v".to_string(),
                }]);
                let out = run_page_at(width, ppp, |ui| automation(ui, &t, &mut buf, &[]));
                let right = max_right(&out.shapes);
                assert!(
                    right <= width + 0.5,
                    "面板宽 {width} @ {ppp}x,却画到了 x={right}"
                );
            }
        }
    }

    /// **走查 P0-1 在「图标」页的同构缺陷。**
    ///
    /// emoji 那一行是「60px 输入框 + 8 个 `small_button`」串在一个不换行的
    /// `ui.horizontal` 里,窄栏下后面几个预设 emoji 直接被顶出面板。
    #[test]
    fn appearance_page_never_paints_past_the_panel_at_any_width_or_dpi() {
        use mullion_store::{ColorSpec, ColorTarget};
        for width in [300.0f32, 440.0, 900.0] {
            for ppp in [1.0f32, 1.25, 1.5] {
                let t = crate::theme::MULLION_DARK;
                // 有图标时才会出现预览那一档;有颜色时才会出现
                // 「自定义 #rrggbb」那一行,连带把「作用于」的三个勾选框也铺开
                // —— 一次覆盖本页所有会变宽的分支。
                let mut buf = ico_buf();
                buf.preserved_appearance.color = Some(ColorSpec {
                    hex: "#ff0000".to_string(),
                    apply_to: vec![ColorTarget::ListItem],
                });
                // 导入失败的红字是本页最长的一行文本,一并覆盖。
                let mut err = Some(crate::ui::ico::ImportError::NotIco.message());
                let out = run_page_at(width, ppp, |ui| {
                    super::appearance(ui, &t, &mut buf, &mut err)
                });
                let right = max_right(&out.shapes);
                assert!(
                    right <= width + 0.5,
                    "面板宽 {width} @ {ppp}x,却画到了 x={right}"
                );
            }
        }
    }

    /// F74 用的凭据样本。
    fn cred(id: u64, name: &str, user: &str) -> mullion_store::CredentialRecord {
        mullion_store::CredentialRecord {
            id: mullion_store::CredentialId(id),
            name: name.into(),
            user: user.into(),
            kind: AuthKind::Password,
        }
    }

    /// 在同一个 `Context` 上反复渲染「认证」页,每帧点一次写着 `clicks[i]`
    /// 的部件,返回最后一帧画出的全部文字。
    ///
    /// **必须多帧**:ComboBox 的候选项要等按钮被点开的下一帧才画得出来,
    /// 一帧之内既点不开也点不中。
    fn click_through_auth(
        buf: &mut EditorBuffer,
        credentials: &[mullion_store::CredentialRecord],
        clicks: &[&str],
    ) -> Vec<String> {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let run = |input: egui::RawInput, buf: &mut EditorBuffer| {
            ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    auth(
                        ui,
                        &t,
                        buf,
                        SecretPresence::default(),
                        &[],
                        credentials,
                        &mut Default::default(),
                    );
                });
            })
        };
        let mut out = run(egui::RawInput::default(), buf);
        for label in clicks {
            let pos = find_text_center(&out.shapes, label)
                .unwrap_or_else(|| panic!("页面上找不到写着「{label}」的部件"));
            let mut input = egui::RawInput::default();
            input.events.push(egui::Event::PointerMoved(pos));
            for pressed in [true, false] {
                input.events.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                });
            }
            let _ = run(input, buf);
            // 点完再空跑一帧才取形状:ComboBox 的候选列表画在 `egui::Area` 里,
            // 而 `Area` 首帧 fade_in 只记 `Shape::Noop`,点击那一帧扫不到任何
            // 候选项文字(D1 切片踩过同一个坑)。
            out = run(egui::RawInput::default(), buf);
        }
        all_text(&out.shapes)
    }

    /// 找写着 `needle` 的那段文字的**中心点** —— `find_text_pos` 给的是左上角,
    /// 拿它去点击会落在字的边上,点不中控件。
    fn find_text_center(shapes: &[egui::epaint::ClippedShape], needle: &str) -> Option<egui::Pos2> {
        fn walk(shape: &egui::Shape, needle: &str) -> Option<egui::Pos2> {
            match shape {
                egui::Shape::Vec(v) => v.iter().find_map(|s| walk(s, needle)),
                egui::Shape::Text(t) if t.galley.job.text.contains(needle) => {
                    Some(t.pos + t.galley.size() / 2.0)
                }
                _ => None,
            }
        }
        shapes.iter().find_map(|cs| walk(&cs.shape, needle))
    }

    /// F74/D1:选了「共享凭据」,会话自己那套身份输入(用户名、认证方式、
    /// 密码/私钥)整块**不画**。
    ///
    /// 不是禁用而是不画:画一组灰着的框等于在暗示「这里还留着一份会话自己的
    /// 身份」,而「一台机器到底用哪个用户名要靠追查」正是本功能要消灭的东西。
    /// 顺带钉住摘要与那句「去『凭据』页改」——用户得知道改动会波及别的会话。
    ///
    /// 自证变红的方式:把 `auth()` 里共享档那条 `return` 删掉。
    #[test]
    fn the_shared_source_hides_the_sessions_own_identity_inputs() {
        let creds = vec![cred(1, "运维号", "ops")];
        let mut buf = EditorBuffer {
            cred_source: CredSourceUi::Shared,
            credential_id: Some(mullion_store::CredentialId(1)),
            ..Default::default()
        };
        let texts = click_through_auth(&mut buf, &creds, &[]);

        for gone in ["用户名", "认证方式", "密码", "私钥", "私钥口令"] {
            assert!(
                !texts.iter().any(|s| s == gone),
                "共享档下不该画「{gone}」这一行;实际画出:{texts:?}"
            );
        }
        assert!(
            texts.iter().any(|s| s.contains("ops")),
            "得让用户看见这份凭据是谁;实际画出:{texts:?}"
        );
        assert!(
            texts.iter().any(|s| s == super::SHARED_CREDENTIAL_NOTE),
            "得说清改凭据会波及所有引用它的会话;实际画出:{texts:?}"
        );
    }

    /// F74:凭据库是空的时候,下拉必须**禁用**——`on_disabled_hover_text`
    /// 的判据是 `!self.enabled`(egui-0.30.0 `response.rs:557-568`),
    /// enabled 为真则那句「先去『凭据』页新建一份」永远弹不出来,用户对着
    /// 一个点了没反应的下拉只会以为程序卡了。判据与 `key_candidate_combo`
    /// 那条同源。
    ///
    /// 自证变红的方式:把 `credential_combo` 里的 `has_any` 改成 `true`。
    #[test]
    fn the_credential_combo_is_disabled_only_while_the_library_is_empty() {
        let mut enabled = Vec::new();
        for creds in [Vec::new(), vec![cred(1, "运维号", "ops")]] {
            // 两次各用一个独立 Context:同一 pass 里两个同 id_salt 的
            // ComboBox 会撞 id,0.30 在 debug 下会画红字警告。
            let ctx = egui::Context::default();
            let mut buf = EditorBuffer::default();
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    enabled.push(credential_combo(ui, &mut buf, &creds).enabled());
                });
            });
        }
        assert_eq!(
            enabled,
            vec![false, true],
            "空库应禁用、有凭据应可用(顺序:空库、有一份)"
        );
    }

    /// F74:在下拉里点中一份凭据,`credential_id` 要真的改过去 ——
    /// 这是「引用共享凭据」在 UI 上唯一的入口,接线断了整个功能就没了。
    ///
    /// 自证变红的方式:把 `credential_combo` 里
    /// `buf.credential_id = Some(c.id)` 那行删掉。
    #[test]
    fn picking_a_credential_from_the_combo_updates_the_buffer() {
        let creds = vec![cred(1, "运维号", "ops"), cred(2, "备用号", "root")];
        let mut buf = EditorBuffer {
            cred_source: CredSourceUi::Shared,
            ..Default::default()
        };
        // 第一下点开下拉(按钮上写着「请选择…」),第二下点中第二份凭据。
        click_through_auth(&mut buf, &creds, &["请选择", "备用号"]);
        assert_eq!(
            buf.credential_id,
            Some(mullion_store::CredentialId(2)),
            "点中的是「备用号」"
        );
    }
}
