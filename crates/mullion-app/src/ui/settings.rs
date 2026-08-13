//! F84:设置弹窗 —— 外观(字体族 / 字号)+ 快捷键一览。
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
}

impl SettingsDraft {
    /// 从落盘的设置起一份草稿。
    pub fn from_settings(s: &mullion_store::Settings) -> Self {
        Self {
            family: s.font_family.clone(),
            font_pt: s.font_pt,
            typed: s.font_family.clone().unwrap_or_default(),
        }
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

    fn draft() -> SettingsDraft {
        SettingsDraft {
            family: Some("Cascadia Mono".into()),
            font_pt: 10.0,
            typed: "Cascadia Mono".into(),
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
        let fams = known();
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut out = SettingsOut::None;
        let mut shapes = Vec::new();
        for _ in 0..2 {
            let full = ctx.run(egui::RawInput::default(), |ctx| {
                out = show(
                    ctx,
                    &t,
                    d,
                    SettingsEnv {
                        families: &fams,
                        not_monospace,
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
        let fams = known();
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut shapes = Vec::new();
        for _ in 0..2 {
            let full = ctx.run(egui::RawInput::default(), |ctx| {
                show(
                    ctx,
                    &t,
                    d,
                    SettingsEnv {
                        families: &fams,
                        not_monospace: false,
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
