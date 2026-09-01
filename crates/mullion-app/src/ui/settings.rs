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

/// 自举开关的标签。测试要靠它在画出来的 `Shape::Text` 里找到这个部件,
/// 所以实现与测试必须共用同一份 —— 各写一遍的话改文案时测试会静默地
/// 点不中,`interact` 里那句 panic 才是唯一的提示。
const BOOTSTRAP_LABEL: &str = "自动配置远端 tmux 的状态上报";

/// F156-c 那个开关的标签。同上,实现与测试**共用这一份**。
const OSC7_LABEL: &str = "让远端 shell 报出当前目录(非 tmux 场景)";

/// 三档的中文标签。**实现与测试共用同一份** —— 各写一遍的话,改文案时
/// 测试会静默地点不中,`interact` 里那句 panic 才是唯一的提示。
const LEVEL_ERROR_LABEL: &str = "只记错误";
const LEVEL_INFO_LABEL: &str = "常规（含性能剖面）";
const LEVEL_DEBUG_LABEL: &str = "详细（排查用）";

fn level_label(lv: mullion_store::LogLevel) -> &'static str {
    match lv {
        mullion_store::LogLevel::Error => LEVEL_ERROR_LABEL,
        mullion_store::LogLevel::Info => LEVEL_INFO_LABEL,
        mullion_store::LogLevel::Debug => LEVEL_DEBUG_LABEL,
    }
}

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
    /// F124:自动配置远端 tmux 状态上报。
    pub tmux_bootstrap: bool,
    /// F156-c:往远端 shell 注入一次 OSC 7 上报。
    pub shell_osc7_bootstrap: bool,
    /// F155:日志详细档位。回写进 `Settings` 与施加到 log facade 都在
    /// `app.rs` 的「确定」分支里做。
    pub log_level: mullion_store::LogLevel,
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
            tmux_bootstrap: s.tmux_bootstrap,
            shell_osc7_bootstrap: s.shell_osc7_bootstrap,
            log_level: s.log_level,
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
    /// F155:按了「导出脱敏日志」。真正的读盘/写盘由 `app.rs` 做 —— 弹窗这一层
    /// 零 IO。**弹窗不关**:导出是个附带动作,用户多半还要接着改别的。
    ExportLog,
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
            form::section(ui, t, "设置", "远端", &mut first);
            remote(ui, t, draft, &mut out);
            form::section(ui, t, "设置", "诊断", &mut first);
            diagnostics(ui, t, draft, &mut out);
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

/// 远端分节:自动配置 tmux 状态上报(F124)+ 让远端 shell 报出当前目录(F156-c)。
///
/// **两个独立开关**,不是一个。副作用完全不同:F124 改的是远端 tmux 服务器
/// 内存里的全局选项,F156-c 往用户**当前这条 shell** 里写一行命令并清屏。
/// 想只关掉其中一件是合理诉求,一个开关做不到。
///
/// 走 `form::grid` 两列骨架(规范 #1):复选框和灰字说明都挂**输入列**、
/// 标签列留空(规范 #6),否则它俩会从 x=0 起画,跟上下两个分节里所有输入框
/// 的左边缘错开。
fn remote(ui: &mut egui::Ui, t: &Theme, draft: &mut SettingsDraft, out: &mut SettingsOut) {
    form::grid(ui, "settings_remote", |ui| {
        ui.label("");
        if ui
            .checkbox(&mut draft.tmux_bootstrap, BOOTSTRAP_LABEL)
            .changed()
        {
            *out = SettingsOut::Preview;
        }
        ui.end_row();

        ui.label("");
        ui.label(
            egui::RichText::new(
                "连上后开一条旁路命令通道,打开远端 tmux 的 set-titles 并让它报出当前目录。\
                 分屏标题条上的目录名、以及文件面板继承终端所在目录都靠它。\
                 改的是 tmux 服务器内存里的全局选项(不写任何文件,server 退出即失效),\
                 那台机器上 attach 同一个 tmux 的其它终端,窗口标题也会跟着变成这个格式。",
            )
            .size(11.0)
            .color(theme::c32(t.fg_muted)),
        );
        ui.end_row();

        ui.label("");
        if ui
            .checkbox(&mut draft.shell_osc7_bootstrap, OSC7_LABEL)
            .changed()
        {
            *out = SettingsOut::Preview;
        }
        ui.end_row();

        ui.label("");
        ui.label(
            egui::RichText::new(
                "分屏刚连上时往远端 shell 发一行命令,让它此后每个提示符都报一次当前目录。\
                 上面那条只在远端开着 tmux 时管用,这条管的是不经过 tmux 的场景 ——\
                 文件面板继承终端所在目录靠它。\
                 只改这条 shell 内存里的提示符钩子(bash 是 PROMPT_COMMAND,zsh 是 precmd_functions;\
                 不写远端任何文件,断开即消失),\
                 发完会清一次屏,所以登录横幅会被一起清掉。\
                 远端 shell 不是 bash / zsh(比如 fish)时,屏幕上会打出一行报错,\
                 那种情况请关掉这个开关。",
            )
            .size(11.0)
            .color(theme::c32(t.fg_muted)),
        );
        ui.end_row();
    });
    ui.add_space(SP_M);
}

/// 诊断分节(F155):日志详细度 + 导出脱敏日志。
///
/// 走 `form::grid` 两列骨架(规范 #1),说明文字挂**输入列**、标签列留空
/// (规范 #6)。
fn diagnostics(ui: &mut egui::Ui, t: &Theme, draft: &mut SettingsDraft, out: &mut SettingsOut) {
    let avail = ui.available_width();
    form::grid(ui, "settings_diagnostics", |ui| {
        ui.label("日志详细度");
        let w = field_w(avail, FIELD_W_M, 0.0);
        egui::ComboBox::from_id_salt("settings_log_level")
            .width(w)
            .selected_text(level_label(draft.log_level))
            .show_ui(ui, |ui| {
                for lv in [
                    mullion_store::LogLevel::Error,
                    mullion_store::LogLevel::Info,
                    mullion_store::LogLevel::Debug,
                ] {
                    if ui
                        .selectable_label(draft.log_level == lv, level_label(lv))
                        .clicked()
                    {
                        draft.log_level = lv;
                    }
                }
            });
        ui.end_row();

        ui.label("");
        ui.label(
            egui::RichText::new(
                "常规档每 5 秒记一行性能剖面（帧耗时、吞吐、各阶段占用、回显往返），\
                 排查卡顿靠它。详细档还会逐事件记录，日志会大很多。\
                 环境变量 MULLION_LOG 若设了，会盖过这里的选择。",
            )
            .size(11.0)
            .color(theme::c32(t.fg_muted)),
        );
        ui.end_row();
    });
    ui.add_space(SP_M);
    if ui.button("导出脱敏日志…").clicked() {
        *out = SettingsOut::ExportLog;
    }
    // 脱敏是**尽力而为的模式匹配**，不是「导出即安全」——对外发送前
    // 请自己再看一眼(同 `redact` 模块文档顶部那句如实陈述)。
    ui.label(
        egui::RichText::new(
            "脱敏是尽力而为的模式匹配，覆盖不到的写法会漏，对外发送前请自己再看一眼。",
        )
        .size(11.0)
        .color(theme::c32(t.fg_muted)),
    );
    ui.add_space(SP_M);
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
                .color(theme::c32(t.danger_text)),
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
            tmux_bootstrap: true,
            shell_osc7_bootstrap: true,
            log_level: mullion_store::LogLevel::Info,
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
            tmux_bootstrap: true,
            shell_osc7_bootstrap: true,
            log_level: mullion_store::LogLevel::Info,
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

    // ---- F124 远端分节 ----

    /// F124:点自举开关要当场回报 `Preview`(草稿变了、需要重画),
    /// 「确定」时才落盘。回报 `None` 的话用户点了没反应。
    ///
    /// 用文件里既有的 `interact` 脚手架:它跑满 `FRAMES` 帧预热(切片 G 吃过
    /// 「预热帧数不足 → 点击落在按钮外面」的亏),再按标签文字找到部件中心点
    /// 下去。复选框是 `Sense::click()`,要**同帧松手**(`release = true`)。
    ///
    /// 自证会变红:把 `resp.changed()` 那个分支删掉。
    #[test]
    fn toggling_the_bootstrap_checkbox_reports_a_preview() {
        let mut d = draft();
        assert!(d.tmux_bootstrap, "脚手架的初值该是开着的");
        let out = interact(&mut d, BOOTSTRAP_LABEL, egui::Vec2::ZERO, true);
        assert!(!d.tmux_bootstrap, "复选框没被真的点到,这条测试测了个寂寞");
        assert_eq!(out, SettingsOut::Preview);
    }

    /// F124:草稿要从**落盘的真值**起,不是每次都摆一个 `true` 上去。
    /// 起错了的症状是「用户关掉过,再打开设置弹窗又显示开着」——而只要他
    /// 这时点了确定,关掉的选择就被这个假的初值覆盖回去了。
    ///
    /// 自证会变红:把 `from_settings` 里那行改成 `tmux_bootstrap: true,`。
    #[test]
    fn the_draft_starts_from_the_stored_switch_not_from_a_hardcoded_default() {
        let s = mullion_store::Settings {
            tmux_bootstrap: false,
            ..Default::default()
        };
        assert!(!SettingsDraft::from_settings(&s).tmux_bootstrap);
        assert!(SettingsDraft::from_settings(&mullion_store::Settings::default()).tmux_bootstrap);
    }

    // ---- F156-c 远端分节第二个开关 ----

    /// F156-c:点这个开关要当场回报 `Preview`(草稿变了、要重画),
    /// 「确定」时才落盘。回报 `None` 的话用户点了没反应。
    ///
    /// 用文件里既有的 `interact` 脚手架(跑满 `FRAMES` 帧预热,再按标签文字
    /// 找部件中心点下去;复选框是 `Sense::click()`,要同帧松手)。
    ///
    /// 自证会变红:把 `remote()` 里这个复选框的 `.changed()` 分支删掉。
    #[test]
    fn toggling_the_shell_osc7_checkbox_reports_a_preview() {
        let mut d = draft();
        assert!(d.shell_osc7_bootstrap, "脚手架的初值该是开着的");
        let out = interact(&mut d, OSC7_LABEL, egui::Vec2::ZERO, true);
        assert!(
            !d.shell_osc7_bootstrap,
            "复选框没被真的点到,这条测试测了个寂寞"
        );
        assert_eq!(out, SettingsOut::Preview);
    }

    /// F156-c:两个开关是**独立**的 —— 点了这个,F124 那个不许跟着动。
    /// 它们的副作用完全不同(一个改远端 tmux 服务器的内存选项,一个往用户
    /// 当前这条 shell 里写命令并清屏),串在一起等于把「只关掉其中一件」
    /// 这个合理诉求堵死。
    ///
    /// 自证会变红:把 `remote()` 里第二个 `checkbox` 的第一个参数写成
    /// `&mut draft.tmux_bootstrap`(**这正是复制粘贴最容易出的错**,
    /// 而且它不报错、只是两个开关联动)。
    #[test]
    fn the_two_remote_switches_are_independent() {
        let mut d = draft();
        let _ = interact(&mut d, OSC7_LABEL, egui::Vec2::ZERO, true);
        assert!(!d.shell_osc7_bootstrap, "点的是 OSC 7 那个");
        assert!(d.tmux_bootstrap, "点 OSC 7 那个把 F124 的开关也带翻了");

        let mut d = draft();
        let _ = interact(&mut d, BOOTSTRAP_LABEL, egui::Vec2::ZERO, true);
        assert!(!d.tmux_bootstrap, "点的是 tmux 那个");
        assert!(
            d.shell_osc7_bootstrap,
            "点 tmux 那个把 OSC 7 的开关也带翻了"
        );
    }

    /// F156-c:草稿从**落盘的真值**起。起错了的症状是「用户关掉过,再打开
    /// 设置弹窗又显示开着」—— 而只要他这时点了确定,关掉的选择就被覆盖回去。
    ///
    /// 自证会变红:把 `from_settings` 里那行改成 `shell_osc7_bootstrap: true,`。
    #[test]
    fn the_osc7_draft_starts_from_the_stored_switch() {
        let s = mullion_store::Settings {
            shell_osc7_bootstrap: false,
            ..Default::default()
        };
        assert!(!SettingsDraft::from_settings(&s).shell_osc7_bootstrap);
        assert!(
            SettingsDraft::from_settings(&mullion_store::Settings::default()).shell_osc7_bootstrap
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

    // ---- F155 诊断分节 ----

    /// 三档都要画出来,而且**当前档位要被选中**。只画不选中的话,用户看到
    /// 三个一样的选项,无从判断现在是哪档。
    #[test]
    fn the_diagnostics_section_shows_the_current_level() {
        let mut d = draft();
        d.log_level = mullion_store::LogLevel::Debug;
        let (texts, _) = run(&mut d, false);
        assert!(
            texts.iter().any(|s| s == "日志详细度"),
            "没画标签:{texts:?}"
        );
        assert!(
            texts.iter().any(|s| s == LEVEL_DEBUG_LABEL),
            "下拉没显示当前档位:{texts:?}"
        );
        // 换一档,显示的也要跟着换 —— 否则「显示的是当前档」这条断言
        // 可能只是碰巧撞上了写死的文案。
        d.log_level = mullion_store::LogLevel::Error;
        let (texts, _) = run(&mut d, false);
        assert!(
            texts.iter().any(|s| s == LEVEL_ERROR_LABEL),
            "换了档位显示没跟着变:{texts:?}"
        );
        assert!(
            !texts.iter().any(|s| s == LEVEL_DEBUG_LABEL),
            "旧档位的文案还在:{texts:?}"
        );
    }

    /// 草稿从**落盘的真值**起。起错了的症状是「用户改成 debug,重开设置又
    /// 显示默认档」—— 而他只要这时点确定,改过的选择就被假初值覆盖回去了。
    ///
    /// 自证会变红:把 `from_settings` 里那行改成写死的 `LogLevel::Info`。
    #[test]
    fn the_draft_starts_from_the_stored_log_level() {
        let s = mullion_store::Settings {
            log_level: mullion_store::LogLevel::Error,
            ..Default::default()
        };
        assert_eq!(
            SettingsDraft::from_settings(&s).log_level,
            mullion_store::LogLevel::Error
        );
        assert_eq!(
            SettingsDraft::from_settings(&mullion_store::Settings::default()).log_level,
            mullion_store::LogLevel::Info,
            "默认档不是 info"
        );
    }

    /// 三档的标签必须两两不同。撞了的话下拉里出现两行一样的字,
    /// 用户点哪一行都像没反应,而所有断言「文案出现过」的测试照绿。
    #[test]
    fn the_three_level_labels_are_distinct() {
        let all = [LEVEL_ERROR_LABEL, LEVEL_INFO_LABEL, LEVEL_DEBUG_LABEL];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b, "两档标签撞了");
            }
            assert!(!a.is_empty(), "有一档的标签是空的");
        }
        assert_eq!(
            level_label(mullion_store::LogLevel::Error),
            LEVEL_ERROR_LABEL
        );
        assert_eq!(level_label(mullion_store::LogLevel::Info), LEVEL_INFO_LABEL);
        assert_eq!(
            level_label(mullion_store::LogLevel::Debug),
            LEVEL_DEBUG_LABEL
        );
    }

    /// 说明文字必须点破「环境变量会盖过这里的选择」。不说的话,带着
    /// `MULLION_LOG=debug` 启动的用户在这儿选了「只记错误」、日志却照旧,
    /// 这是个查无可查的问题 —— 设置文件里存的确实是他选的那个值。
    #[test]
    fn the_hint_admits_that_the_environment_variable_wins() {
        let mut d = draft();
        let (texts, _) = run(&mut d, false);
        assert!(
            texts.iter().any(|s| s.contains("MULLION_LOG")),
            "没说明环境变量会覆盖:{texts:?}"
        );
    }

    /// F155:诊断分节里必须有「导出脱敏日志…」按钮,点了要回报
    /// `SettingsOut::ExportLog`(真正的读盘/写盘由 `app.rs` 做)。
    ///
    /// 第二条断言是这个功能的诚信底线:脱敏是尽力而为的模式匹配,不是
    /// 「导出即安全」——按钮旁边必须如实说清楚,否则用户会把这份「脱敏」
    /// 日志当成真的安全就往外发。
    ///
    /// 自证会变红:把 `diagnostics` 里 `*out = SettingsOut::ExportLog;`
    /// 那一行删掉(第一条断言红);把那句「脱敏是尽力而为…」的提示删掉
    /// (第二条断言红)。
    #[test]
    fn the_diagnostics_section_offers_export_and_admits_it_is_best_effort() {
        let mut d = draft();
        let (texts, _) = run(&mut d, false);
        assert!(
            texts.iter().any(|s| s.contains("尽力而为")),
            "没有说清楚脱敏是尽力而为的模式匹配,会给用户「导出即安全」的错觉:{texts:?}"
        );
        assert_eq!(
            click(&mut d, "导出脱敏日志…"),
            SettingsOut::ExportLog,
            "按钮没有回报 ExportLog"
        );
    }
}
