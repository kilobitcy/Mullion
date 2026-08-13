//! F84:设置弹窗 —— 外观(字体族 / 字号)+ 安全(F71 主密码)+ 快捷键一览。
//!
//! **零 IO、零 GPU**:这里只改一份 [`SettingsDraft`] 并回报一个
//! [`SettingsOut`],真的换字体(`TextLayer::set_font`)与落盘
//! (`mullion_store::settings::save`)都在 `app.rs`。
//!
//! 表单骨架复用 `session_manager::form` 的构件(规范 #1),不另起一套 ——
//! 另起一套的话「标签列 88px」这类规则立刻开始漂。

use mullion_store::settings::{MAX_FONT_PT, MIN_FONT_PT};

use crate::font_pick::{family_missing, FontChoice};
use crate::theme::{self, Theme};
use crate::ui::annotate;
use crate::ui::metrics::{field_w, FIELD_W_M, FIELD_W_S, SP_L, SP_M, SP_S};
use crate::ui::session_manager::form;
use crate::ui::shortcuts::SHORTCUTS;

/// 弹窗里正在编辑的那份设置。
///
/// **不直接改 `App::settings`**:字号是拖动即预览的(设计 §8),没有一份草稿
/// 就没法在「取消」时回滚。
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsDraft {
    /// 选中的字体族。`None` = 内置默认。
    pub family: Option<String>,
    /// 字号(pt)。
    pub font_pt: f32,
    /// 手填框里的文本。**与 `family` 分开存**:用户打到一半的
    /// 「Casc」不该被当成一个真的族名去 `set_font`(那会当场回退到默认字体,
    /// 看起来像每敲一个字母字体就闪一下)。
    pub typed: String,
    /// F71:「新主密码」框。
    ///
    /// **不进 `Settings`、不落盘**:它是一次性动作的输入,不是偏好。
    /// 施加(`SetPassword`)之后由 `app.rs` 当场清空。
    pub new_password: String,
    /// F71:「确认」框。
    pub confirm_password: String,
}

impl SettingsDraft {
    /// 从落盘的设置起一份草稿。
    pub fn from_settings(s: &mullion_store::Settings) -> Self {
        Self {
            family: s.font_family.clone(),
            font_pt: s.font_pt,
            typed: s.font_family.clone().unwrap_or_default(),
            new_password: String::new(),
            confirm_password: String::new(),
        }
    }

    /// F71:两个密码框能不能拿去设定主密码。
    ///
    /// 纯函数,`show` 与测试共用同一份判据 —— 各写一遍的话,「按钮灰着但
    /// 提示说没问题」这种自相矛盾的状态迟早出现。
    pub fn password_ready(&self) -> bool {
        !self.new_password.is_empty() && self.new_password == self.confirm_password
    }

    /// F71:该不该画「两次输入不一致」。
    ///
    /// **确认框还空着时不算不一致**:一边打第一个框一边红着,是在指责用户
    /// 还没做完的事。
    pub fn password_mismatch(&self) -> bool {
        !self.confirm_password.is_empty() && self.new_password != self.confirm_password
    }
}

/// 画这一帧要用的、弹窗自己算不出来的东西。
#[derive(Clone, Copy)]
pub struct SettingsEnv<'a> {
    /// 系统里装了哪些字体族(已由 `font_pick::sort_families` 整理过)。
    pub families: &'a [FontChoice],
    /// 当前字体量出来**不是等宽**。判据在 `font_pick::is_monospace_advance`,
    /// 量宽度要 `FontSystem`,只能由 `app.rs` 算好传进来。
    pub not_monospace: bool,
    /// F71:这个库现在是不是主密码方案。决定按钮写「设定」还是「修改」,
    /// 以及画不画「取消主密码」。
    pub has_master_password: bool,
    /// F71:会话库这一刻可用没有。不可用时整个安全分节置灰 —— 库都没打开,
    /// 「设主密码」是设给谁的。
    pub store_available: bool,
}

/// 这一帧用户干了什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsOut {
    /// 什么都没动。
    None,
    /// 改了草稿,要**立刻**看到效果(拖字号滑块、换字体族)。不落盘。
    Preview,
    /// 「确定」:落盘。
    Commit,
    /// 「取消」:调用方把进弹窗前的值装回去,并再换一次字体。
    Cancel,
    /// F71:按了「设定 / 修改主密码」。密码在 `draft.new_password` 里,
    /// 由 `app.rs` 取走并当场清空两个框。**弹窗不关**:改完主密码还可能
    /// 接着改字体,而且用户需要看到那句「已生效」。
    SetPassword,
    /// F71:按了「取消主密码」。回到钥匙串方案。
    ClearPassword,
}

/// 画设置弹窗。返回这一帧的结论。
pub fn show(
    ctx: &egui::Context,
    t: &Theme,
    draft: &mut SettingsDraft,
    env: SettingsEnv<'_>,
) -> SettingsOut {
    let mut out = SettingsOut::None;
    egui::Window::new("设置")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            annotate::mark(ui.ctx(), "设置弹窗", ui.max_rect());
            let mut first = true;
            form::section(ui, t, "设置", "外观", &mut first);
            appearance(ui, t, draft, env, &mut out);
            form::section(ui, t, "设置", "安全", &mut first);
            security(ui, t, draft, env, &mut out);
            form::section(ui, t, "设置", "快捷键", &mut first);
            shortcut_table(ui, t);
            ui.add_space(SP_L);
            ui.horizontal(|ui| {
                if ui.button("确定").clicked() {
                    out = SettingsOut::Commit;
                }
                ui.add_space(SP_S);
                if ui.button("取消").clicked() {
                    out = SettingsOut::Cancel;
                }
            });
        });
    out
}

/// 外观分节:字体族下拉 + 手填 + 字号滑块 + 主题(置灰)。
fn appearance(
    ui: &mut egui::Ui,
    t: &Theme,
    draft: &mut SettingsDraft,
    env: SettingsEnv<'_>,
    out: &mut SettingsOut,
) {
    let avail = ui.available_width();
    // 弹窗里这几行后面没有附属控件,`reserve` 为 0。
    form::grid(ui, "settings_appearance", |ui| {
        ui.label("字体");
        let w = field_w(avail, FIELD_W_M, 0.0);
        let current = draft
            .family
            .clone()
            .unwrap_or_else(|| "(内置默认)".to_string());
        egui::ComboBox::from_id_salt("settings_font_family")
            .width(w)
            .selected_text(current)
            .show_ui(ui, |ui| {
                // 第一条永远是「内置默认」——用户改坏之后要有一条回得去的路,
                // 而「把输入框清空」这种回退方式没人猜得到。
                if ui
                    .selectable_label(draft.family.is_none(), "(内置默认)")
                    .clicked()
                {
                    draft.family = None;
                    draft.typed.clear();
                    *out = SettingsOut::Preview;
                }
                for c in env.families {
                    let label = if c.monospaced {
                        format!("{}  · 等宽", c.name)
                    } else {
                        c.name.clone()
                    };
                    let on = draft.family.as_deref() == Some(c.name.as_str());
                    if ui.selectable_label(on, label).clicked() {
                        draft.family = Some(c.name.clone());
                        draft.typed = c.name.clone();
                        *out = SettingsOut::Preview;
                    }
                }
            });
        ui.end_row();

        ui.label("手填族名");
        let resp = ui.add(egui::TextEdit::singleline(&mut draft.typed).desired_width(w));
        // **失焦或回车才生效**,不是每敲一个字母就换一次字体:打到一半的
        // 「Casc」匹配不上,cosmic-text 会静默回退到默认字体,看起来像字体
        // 在闪(设计 §3 那条「不静默」的另一面)。
        if resp.lost_focus() {
            let want = draft.typed.trim();
            let next = if want.is_empty() {
                None
            } else {
                Some(want.to_string())
            };
            if next != draft.family {
                draft.family = next;
                *out = SettingsOut::Preview;
            }
        }
        ui.end_row();

        // 提示挂**输入列**、不挂标签列(规范 #5/#6)。两条提示都是「设置看着
        // 生效了但画面不对」的唯一解释来源。
        let missing = draft
            .family
            .as_deref()
            .is_some_and(|f| family_missing(f, env.families));
        form::field_error(
            ui,
            t,
            missing,
            "系统里没有这个字体族,画面上会回退到默认字体",
        );
        if !missing && env.not_monospace {
            ui.label("");
            ui.label(theme::hint_text(t, "这不是等宽字体,终端里会整屏错列"));
            ui.end_row();
        }

        ui.label("字号");
        let slider = ui.add(
            egui::Slider::new(&mut draft.font_pt, MIN_FONT_PT..=MAX_FONT_PT)
                .suffix(" pt")
                .fixed_decimals(1),
        );
        // 拖动即预览(设计 §8):字号是「看着舒不舒服」,不试怎么知道。
        // 代价是拖的每一帧都发一次 window_change —— 与拖窗口同量级,而防抖
        // 会引入「松手才生效」的延迟,反而更难判断字号合不合适。
        if slider.changed() {
            *out = SettingsOut::Preview;
        }
        ui.end_row();

        ui.label("主题");
        let w_s = field_w(avail, FIELD_W_S, 0.0);
        ui.add_enabled_ui(false, |ui| {
            ui.add_sized([w_s, 0.0], egui::Button::new("Mullion Dark"))
                .on_disabled_hover_text(
                    "暂只有这一套。换主题要重算 F62 的对比度闸门(≥3:1 / ≥4.5:1),\
                     那是独立一片的工作量",
                );
        });
        ui.end_row();
    });
}

/// 安全分节(F71):主密码状态 + 两个密码框 + 设定/修改 与 取消两个动作。
fn security(
    ui: &mut egui::Ui,
    t: &Theme,
    draft: &mut SettingsDraft,
    env: SettingsEnv<'_>,
    out: &mut SettingsOut,
) {
    let avail = ui.available_width();
    let w = field_w(avail, FIELD_W_M, 0.0);
    form::grid(ui, "settings_security", |ui| {
        ui.label("主密码");
        ui.label(if env.has_master_password {
            "已设定"
        } else {
            "未设定"
        });
        ui.end_row();

        ui.label("新主密码");
        ui.add(
            egui::TextEdit::singleline(&mut draft.new_password)
                .password(true)
                .desired_width(w),
        );
        ui.end_row();

        ui.label("确认");
        ui.add(
            egui::TextEdit::singleline(&mut draft.confirm_password)
                .password(true)
                .desired_width(w),
        );
        ui.end_row();

        form::field_error(ui, t, draft.password_mismatch(), "两次输入不一致");
        // **恒显示**,不是打了字才出现:这句话要在用户决定设不设之前就看到。
        // 没有第二把钥匙是这个设计的属性,不是缺陷(设计 §4),但属性也得说。
        ui.label("");
        ui.label(
            egui::RichText::new("忘记主密码没有找回途径 —— 已保存的密码与私钥将永久无法解开")
                .size(11.0)
                .color(theme::c32(t.danger)),
        );
        ui.end_row();
    });

    ui.add_space(SP_M);
    ui.horizontal(|ui| {
        let label = if env.has_master_password {
            "修改主密码"
        } else {
            "设定主密码"
        };
        let can_set = env.store_available && draft.password_ready();
        if ui
            .add_enabled(can_set, egui::Button::new(label))
            .on_disabled_hover_text(if env.store_available {
                "先在两个框里输入同一个非空密码"
            } else {
                "会话库没打开,没有可以加密的东西"
            })
            .clicked()
        {
            *out = SettingsOut::SetPassword;
        }
        // 「取消主密码」只在**真的设了**的时候才出现:没设的时候摆一个灰按钮
        // 在那儿,只会让人以为自己漏看了什么状态。
        if env.has_master_password {
            ui.add_space(SP_S);
            if ui
                .add_enabled(env.store_available, egui::Button::new("取消主密码"))
                .on_hover_text("改回由本机钥匙串保管密钥 —— 配置目录就不能再搬到别的机器上用了")
                .clicked()
            {
                *out = SettingsOut::ClearPassword;
            }
        }
    });
}

/// 快捷键一览。只读表格,数据源是 `ui::shortcuts::SHORTCUTS`(那边有撞键守护)。
fn shortcut_table(ui: &mut egui::Ui, t: &Theme) {
    egui::ScrollArea::vertical()
        .max_height(220.0)
        .show(ui, |ui| {
            egui::Grid::new("settings_shortcuts")
                .num_columns(3)
                .spacing([SP_M, SP_S])
                .show(ui, |ui| {
                    for s in SHORTCUTS {
                        ui.label(egui::RichText::new(s.chord).strong());
                        ui.label(theme::hint_text(t, s.scope));
                        ui.label(s.what);
                        ui.end_row();
                    }
                });
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::metrics::LABEL_COL_W;

    /// 预热多少帧才去读画面。
    ///
    /// 弹窗里嵌了 `ScrollArea` + 两层 `Grid`,宽度是逐帧往外撑的:内层网格
    /// 这一帧量出来的宽度,下一帧才会把窗口撑开,窗口撑开后 `available_width`
    /// 又变了……**收敛需要好几帧**,不是两帧。加安全分节之后实测第 7 帧才稳
    /// (逐帧量「取消」按钮的位置得出),这里取 8 留余量。
    ///
    /// 帧数不够有两种症状,都不报错:少太多是画面从中间某一行起整段消失;
    /// 差一两帧是位置还差几个像素 —— 于是点击落在按钮外面,`click` 返回
    /// `None`,看着像「按钮不响应」。
    const FRAMES: usize = 8;

    fn draft() -> SettingsDraft {
        SettingsDraft {
            family: Some("Cascadia Mono".into()),
            font_pt: 10.0,
            typed: "Cascadia Mono".into(),
            new_password: String::new(),
            confirm_password: String::new(),
        }
    }

    fn known() -> Vec<FontChoice> {
        crate::font_pick::sort_families(vec![
            ("Cascadia Mono".into(), true),
            ("Arial".into(), false),
        ])
    }

    /// 跑两帧并收本帧画出来的文字。两帧:egui 的容器首帧常只记
    /// `Shape::Noop`(同 `ui/restored.rs` 的说明)。
    fn run(d: &mut SettingsDraft, not_monospace: bool) -> (Vec<String>, SettingsOut) {
        run_env(d, not_monospace, false)
    }

    /// `run` 的全参版:`has_master_password` 影响安全分节画什么。
    fn run_env(
        d: &mut SettingsDraft,
        not_monospace: bool,
        has_master_password: bool,
    ) -> (Vec<String>, SettingsOut) {
        let fams = known();
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut out = SettingsOut::None;
        let mut shapes = Vec::new();
        for _ in 0..FRAMES {
            let full = ctx.run(egui::RawInput::default(), |ctx| {
                out = show(
                    ctx,
                    &t,
                    d,
                    SettingsEnv {
                        families: &fams,
                        not_monospace,
                        has_master_password,
                        store_available: true,
                    },
                );
            });
            shapes = full.shapes;
        }
        let mut texts = Vec::new();
        fn walk(shape: &egui::Shape, acc: &mut Vec<String>) {
            match shape {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, acc)),
                egui::Shape::Text(ts) => acc.push(ts.galley.text().to_string()),
                _ => {}
            }
        }
        for cs in &shapes {
            walk(&cs.shape, &mut texts);
        }
        (texts, out)
    }

    /// 点一下写着 `label` 的部件,返回那一帧的结论。
    fn click(d: &mut SettingsDraft, label: &str) -> SettingsOut {
        interact(d, label, egui::Vec2::ZERO, true)
    }

    /// 在「写着 `label` 的部件的中心 + `offset`」处按下鼠标。
    ///
    /// `offset` 用来打同一行里没有文字、按文字找不到的部件(滑轨)。
    /// `release` = 同帧松手:按钮认 `clicked()`,**滑块不认** —— 它是
    /// `Sense::drag()`,松了手 `interact_pointer_pos()` 当帧就没了。
    fn interact(
        d: &mut SettingsDraft,
        label: &str,
        offset: egui::Vec2,
        release: bool,
    ) -> SettingsOut {
        interact_env(d, label, offset, release, true, false)
    }

    /// `interact` 的全参版。`store_available` / `has_master_password` 决定
    /// 安全分节那两个按钮的可点性与去留。
    #[allow(clippy::fn_params_excessive_bools)]
    fn interact_env(
        d: &mut SettingsDraft,
        label: &str,
        offset: egui::Vec2,
        release: bool,
        store_available: bool,
        has_master_password: bool,
    ) -> SettingsOut {
        let fams = known();
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut shapes = Vec::new();
        for _ in 0..FRAMES {
            let full = ctx.run(egui::RawInput::default(), |ctx| {
                show(
                    ctx,
                    &t,
                    d,
                    SettingsEnv {
                        families: &fams,
                        not_monospace: false,
                        has_master_password,
                        store_available,
                    },
                );
            });
            shapes = full.shapes;
        }
        fn find(shape: &egui::Shape, label: &str) -> Option<egui::Pos2> {
            match shape {
                egui::Shape::Vec(v) => v.iter().find_map(|s| find(s, label)),
                egui::Shape::Text(ts) if ts.galley.text() == label => {
                    Some(ts.pos + ts.galley.size() / 2.0)
                }
                _ => None,
            }
        }
        let pos = shapes
            .iter()
            .find_map(|cs| find(&cs.shape, label))
            .unwrap_or_else(|| panic!("设置弹窗里没有写着「{label}」的部件"))
            + offset;
        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::PointerMoved(pos));
        let phases: &[bool] = if release { &[true, false] } else { &[true] };
        for &pressed in phases {
            input.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            });
        }
        let mut out = SettingsOut::None;
        let fams2 = known();
        let _ = ctx.run(input, |ctx| {
            out = show(
                ctx,
                &t,
                d,
                SettingsEnv {
                    families: &fams2,
                    not_monospace: false,
                    has_master_password,
                    store_available,
                },
            );
        });
        out
    }

    /// 光画一帧不该产生任何动作 —— 否则弹窗一打开就开始换字体、写盘。
    #[test]
    fn merely_showing_the_dialog_changes_nothing() {
        let mut d = draft();
        let before = d.clone();
        let (_, out) = run(&mut d, false);
        assert_eq!(out, SettingsOut::None);
        assert_eq!(d, before, "光画一帧就把草稿改了");
    }

    /// 拖字号滑块必须当场回报 `Preview`(改字体 + 标脏),而不是 `Commit`
    /// (每拖一格写一次盘)也不是 `None`(拖了没反应,用户只能靠确定后重启看)。
    ///
    /// 点滑轨等价于把滑块拖到那一点(egui 的 `Slider` 对 click 与 drag 一视同仁)。
    ///
    /// 自证会变红:把 `slider.changed()` 那个分支里的 `Preview` 改成 `None`。
    #[test]
    fn dragging_the_size_slider_reports_a_preview_not_a_commit() {
        let mut d = draft();
        let before = d.font_pt;
        let out = interact(
            &mut d,
            "字号",
            egui::vec2(LABEL_COL_W + SP_M + 70.0, 0.0),
            false,
        );
        assert_eq!(out, SettingsOut::Preview);
        assert_ne!(d.font_pt, before, "滑块没被真的拖动,这条测试测了个寂寞");
    }

    /// 「取消」必须回报 `Cancel` 而不是 `None` —— 调用方靠它把进弹窗前的值
    /// 装回去。回报 `None` 的话预览过的字号就永久留下了,而用户按的是取消。
    ///
    /// 自证会变红:把 `SettingsOut::Cancel` 改成 `SettingsOut::None`。
    #[test]
    fn cancel_reports_cancel_so_the_caller_can_roll_back() {
        assert_eq!(click(&mut draft(), "取消"), SettingsOut::Cancel);
    }

    /// 「确定」落盘,与预览区分开:预览不写盘,否则拖一次滑块写几十次文件。
    #[test]
    fn ok_reports_commit_not_preview() {
        assert_eq!(click(&mut draft(), "确定"), SettingsOut::Commit);
    }

    /// 非等宽字体必须当场说出来。终端里用比例字体的症状是整屏错列,而错列
    /// 看起来像「程序有 bug」不像「字体选错了」——这条提示是把因果关系摆到
    /// 用户眼前的唯一机会。
    ///
    /// 自证会变红:把 `env.not_monospace` 那个分支删掉。
    #[test]
    fn a_non_monospace_font_is_called_out_next_to_the_field() {
        let mut d = draft();
        let (texts, _) = run(&mut d, true);
        assert!(
            texts.iter().any(|s| s.contains("不是等宽字体")),
            "选了比例字体却什么都没提示:{texts:?}"
        );
    }

    /// 装不上的字体族同样要说 —— cosmic-text 匹配不到会**静默**回退到默认
    /// 字体,画面看着正常,用户只会以为设置没生效。
    #[test]
    fn a_font_that_is_not_installed_is_called_out() {
        let mut d = SettingsDraft {
            family: Some("Comic Sans MS".into()),
            font_pt: 10.0,
            typed: "Comic Sans MS".into(),
            new_password: String::new(),
            confirm_password: String::new(),
        };
        let (texts, _) = run(&mut d, false);
        assert!(
            texts.iter().any(|s| s.contains("没有这个字体族")),
            "选了没装的字体却什么都没提示:{texts:?}"
        );
    }

    /// 主题那一栏是灰的,而且**说得出为什么**(表单规范 #9:灰着的按钮不说话,
    /// 用户只会反复点然后以为程序坏了)。
    #[test]
    fn the_theme_row_is_present_and_names_the_only_theme() {
        let mut d = draft();
        let (texts, _) = run(&mut d, false);
        assert!(texts.iter().any(|s| s == "主题"));
        assert!(texts.iter().any(|s| s.contains("Mullion Dark")));
    }

    // ---- F71 安全分节 ----

    /// 当前是不是主密码方案,得**明说**。用户装到一半忘了自己设没设,
    /// 唯一的验证途径不该是「重启一次看弹不弹解锁框」。
    #[test]
    fn the_security_section_says_whether_a_master_password_is_set() {
        let (off, _) = run_env(&mut draft(), false, false);
        assert!(off.iter().any(|s| s == "未设定"), "没说没设:{off:?}");
        assert!(
            off.iter().any(|s| s == "设定主密码"),
            "没设的时候按钮该写「设定」:{off:?}"
        );
        let (on, _) = run_env(&mut draft(), false, true);
        assert!(on.iter().any(|s| s == "已设定"), "没说已设:{on:?}");
        assert!(
            on.iter().any(|s| s == "修改主密码"),
            "设过的时候按钮该写「修改」:{on:?}"
        );
    }

    /// 两次不一致要当场说,而且按钮点不动 —— 否则用户设成一个自己打错的
    /// 密码,下次启动才发现,那时已经进不去了。
    #[test]
    fn mismatched_confirmation_is_called_out_and_blocks_the_button() {
        let mut d = SettingsDraft {
            new_password: "hunter2".into(),
            confirm_password: "hunter3".into(),
            ..draft()
        };
        assert!(!d.password_ready(), "不一致却认为可以设");
        let (texts, _) = run(&mut d, false);
        assert!(
            texts.iter().any(|s| s.contains("两次输入不一致")),
            "两次不一致却什么都不说:{texts:?}"
        );
        assert_eq!(
            click(&mut d, "设定主密码"),
            SettingsOut::None,
            "不一致时按钮该是灰的"
        );
    }

    /// 确认框还空着不算「不一致」——一边打第一个框一边红着,是在指责用户
    /// 还没做完的事。
    #[test]
    fn a_half_typed_password_is_not_yet_a_mismatch() {
        let mut d = SettingsDraft {
            new_password: "hunter2".into(),
            confirm_password: String::new(),
            ..draft()
        };
        assert!(!d.password_mismatch(), "只打了第一个框就判定不一致");
        let (texts, _) = run(&mut d, false);
        assert!(
            !texts.iter().any(|s| s.contains("两次输入不一致")),
            "还没开始打确认框就红了:{texts:?}"
        );
    }

    /// 空密码设不了 —— 两个框都空着时「一致」在字面上成立,判据必须额外
    /// 挡住空串,否则一次误点就把库换成一个用空串解开的主密码。
    #[test]
    fn an_empty_password_cannot_be_set() {
        let mut d = draft();
        assert!(d.new_password.is_empty() && d.confirm_password.is_empty());
        assert!(!d.password_ready(), "两个框都空着却认为可以设");
        assert_eq!(
            click(&mut d, "设定主密码"),
            SettingsOut::None,
            "空密码不该能设"
        );
    }

    /// 一致且非空时才真的发 `SetPassword`。上一条只证明了「点不动」,
    /// 这条证明按钮不是**永远**点不动。
    #[test]
    fn a_matching_password_can_be_set() {
        let mut d = SettingsDraft {
            new_password: "hunter2".into(),
            confirm_password: "hunter2".into(),
            ..draft()
        };
        assert!(d.password_ready());
        assert_eq!(click(&mut d, "设定主密码"), SettingsOut::SetPassword);
    }

    /// 「忘了没有找回途径」必须**一开始就在**,不是打了字才出现:
    /// 这句话要在用户决定设不设之前就看到,设完再说等于事后通知。
    #[test]
    fn the_irreversible_warning_is_always_visible_not_only_after_typing() {
        let (texts, _) = run(&mut draft(), false);
        assert!(
            texts.iter().any(|s| s.contains("没有找回途径")),
            "还没打字就该看到这句警告:{texts:?}"
        );
    }

    /// 「取消主密码」只在**设过**的时候出现。没设的时候摆一个出来,用户
    /// 会以为自己设过。
    #[test]
    fn clearing_is_only_offered_when_a_password_is_set() {
        let (off, _) = run_env(&mut draft(), false, false);
        assert!(
            !off.iter().any(|s| s.contains("取消主密码")),
            "没设主密码却给了「取消主密码」:{off:?}"
        );
        let (on, _) = run_env(&mut draft(), false, true);
        assert!(
            on.iter().any(|s| s.contains("取消主密码")),
            "设过了却撤不掉:{on:?}"
        );
        assert_eq!(
            interact_env(
                &mut draft(),
                "取消主密码",
                egui::Vec2::ZERO,
                true,
                true,
                true
            ),
            SettingsOut::ClearPassword
        );
    }

    /// 会话库没打开时整个分节点不动 —— 库都没打开,「设主密码」是设给谁的。
    #[test]
    fn a_closed_store_cannot_have_its_password_changed() {
        let mut d = SettingsDraft {
            new_password: "hunter2".into(),
            confirm_password: "hunter2".into(),
            ..draft()
        };
        assert_eq!(
            interact_env(&mut d, "设定主密码", egui::Vec2::ZERO, true, false, false),
            SettingsOut::None,
            "库没打开却能设主密码"
        );
    }

    /// 快捷键一览真的画出来了(不是一个空表)。
    #[test]
    fn the_shortcut_table_lists_real_chords() {
        let mut d = draft();
        let (texts, _) = run(&mut d, false);
        assert!(
            texts.iter().any(|s| s == "Ctrl+Shift+C"),
            "快捷键一览是空的:{texts:?}"
        );
    }
}
