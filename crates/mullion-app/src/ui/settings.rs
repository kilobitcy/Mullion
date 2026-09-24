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

/// 自举开关的标签。测试要靠它在画出来的 `Shape::Text` 里找到这个部件,
/// 所以实现与测试必须共用同一份 —— 各写一遍的话改文案时测试会静默地
/// 点不中,`interact` 里那句 panic 才是唯一的提示。
const BOOTSTRAP_LABEL: &str = "自动配置远端 tmux 的状态上报";

/// F156-c 那个开关的标签。同上,实现与测试**共用这一份**。
const OSC7_LABEL: &str = "让远端 shell 报出当前目录(非 tmux 场景)";

/// F253 那个开关的标签。同上,实现与测试**共用这一份**。
///
/// 文案不含 `.` 之外的任何非 ASCII 符号(T9 字形白名单:egui 的字体链只有
/// 内置 + 微软雅黑两级,链外字形静默画成豆腐块)。
const HIDDEN_FILES_LABEL: &str = "文件面板显示以 . 开头的项";

/// 三档的中文标签。**实现与测试共用同一份** —— 各写一遍的话,改文案时
/// 测试会静默地点不中,`interact` 里那句 panic 才是唯一的提示。
const LEVEL_ERROR_LABEL: &str = "只记错误";
const LEVEL_INFO_LABEL: &str = "常规（含性能剖面）";
const LEVEL_DEBUG_LABEL: &str = "详细（排查用）";

/// 云端备份开关的标签。实现与测试**共用这一份**(同 `BOOTSTRAP_LABEL` 的理由)。
const CLOUD_ENABLED_LABEL: &str = "开启云端备份";
/// path-style 开关的标签。
const CLOUD_PATH_STYLE_LABEL: &str = "用 path-style 寻址（自建 MinIO 多半要打开）";

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
    /// F253:文件面板显示 `.` 开头的项。
    ///
    /// 「确定」时除了落盘,还要把**已经开着的**面板两栏一起刷成这个值
    /// (`App::take_settings_draft`)—— 不刷的话用户在这儿勾完、回面板毫无
    /// 变化,只会以为程序坏了。
    pub show_hidden_files: bool,
    /// F155:日志详细档位。回写进 `Settings` 与施加到 log facade 都在
    /// `app.rs` 的「确定」分支里做。
    pub log_level: mullion_store::LogLevel,
    /// F271:云端备份的草稿。**与 `Settings` 分开** —— 它们落在两个文件里
    /// (`cloud.toml` 不进迁移包,见 `mullion_store::cloud` 的模块文档),
    /// 合成一份的话「确定」那一步会分不清该写哪个文件。
    pub cloud_enabled: bool,
    pub cloud_endpoint: String,
    pub cloud_region: String,
    pub cloud_bucket: String,
    pub cloud_prefix: String,
    pub cloud_path_style: bool,
    pub cloud_keep: u32,
    pub cloud_interval_min: u32,
    /// SOCKS5 代理,空 = 直连。见 `CloudConfig::socks5` 上那段理由。
    pub cloud_socks5: String,
    pub cloud_access_key_id: String,
    /// 新填的 SK。**空 = 不改**(不是「清空」):每次打开设置都要用户重打一遍
    /// 一串 30 位的密钥,是在逼人把它记在别处。
    pub cloud_secret_new: String,
    /// F283:新填的备份口令。**空 = 不改**,理由同 `cloud_secret_new`。
    ///
    /// **不进 `Settings`、不落盘**,与 `new_password` 同理:它是一次性动作
    /// 的输入,写回成功后由 `app.rs` 当场清空,不跨次打开留着明文。
    pub cloud_pass_new: String,
    /// F283:「确认口令」框。
    pub cloud_pass_confirm: String,
}

impl SettingsDraft {
    /// 从落盘的设置起一份草稿。云端那一节按 `CloudConfig::default()` 起手。
    ///
    /// **生产路径上没人该调它** —— 设置弹窗走
    /// [`Self::from_settings_and_cloud`],因为这一个读不到 `cloud.toml`,
    /// 拿它起的草稿云端字段全是默认值,而「确定」是会把草稿写回
    /// `cloud.toml` 的:endpoint / bucket / AK 会被一次「打开设置再点确定」
    /// 悄悄清空(本项目登记过的「整份覆盖」缺陷族,已经踩过五处)。
    /// 留着它只为那些跟云端毫无关系的单测能少写一个参数。**现在生产路径上
    /// 还有一处在调它**(`app.rs` 的 `sync_settings_dialog`)——把那处切到
    /// `from_settings_and_cloud`、并补一条「生产代码不许出现 `from_settings(`」
    /// 的守护,是下一个任务(菜单与驱动接线)的事。在那之前这段风险是真的。
    pub fn from_settings(s: &mullion_store::Settings) -> Self {
        Self::from_settings_and_cloud(s, &mullion_store::CloudConfig::default())
    }

    /// F271:从落盘的设置 + 落盘的云配置起一份草稿。
    ///
    /// **两个文件各读各的** —— 设置在 `settings.toml`,云配置在 `cloud.toml`,
    /// 后者不进迁移包(见 `mullion_store::cloud` 的模块文档)。
    ///
    /// 这里是 `SettingsDraft` **唯一**的穷尽字面量。加字段时只有这一处要改,
    /// 漏了当场编译不过 —— 而不是「有两处、改了一处、另一处悄悄给了错值」。
    pub fn from_settings_and_cloud(
        s: &mullion_store::Settings,
        c: &mullion_store::CloudConfig,
    ) -> Self {
        Self {
            family: s.font_family.clone(),
            font_pt: s.font_pt,
            typed: s.font_family.clone().unwrap_or_default(),
            new_password: String::new(),
            confirm_password: String::new(),
            tmux_bootstrap: s.tmux_bootstrap,
            shell_osc7_bootstrap: s.shell_osc7_bootstrap,
            show_hidden_files: s.show_hidden_files,
            log_level: s.log_level,
            cloud_enabled: c.enabled,
            cloud_endpoint: c.endpoint.clone(),
            cloud_region: c.region.clone(),
            cloud_bucket: c.bucket.clone(),
            cloud_prefix: c.prefix.clone(),
            cloud_path_style: c.path_style,
            cloud_keep: c.keep,
            cloud_interval_min: c.interval_min,
            cloud_socks5: c.socks5.clone(),
            cloud_access_key_id: c.access_key_id.clone(),
            // **空 = 不改**,不是「清空」。见字段上那段理由。
            cloud_secret_new: String::new(),
            cloud_pass_new: String::new(),
            cloud_pass_confirm: String::new(),
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

    /// F283:两次输入的备份口令是否不一致(空着 = 不改,不算不一致)。
    pub fn cloud_pass_mismatch(&self) -> bool {
        !self.cloud_pass_new.is_empty() && self.cloud_pass_new != self.cloud_pass_confirm
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
    /// F283:云端配置里当前有没有设过备份口令(不解密,只判有没有)。
    /// 只用来画「当前:已设置/还没设置」这一句状态,**不做门控** ——
    /// 拿它去罩「填口令」那一格,会让唯一能设上口令的入口自己被灰掉
    /// (F270 踩过的「没有出口的陷阱」)。
    pub cloud_has_passphrase: bool,
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

/// F239:窗口标题。`app.rs::dismiss_areas` 与这里的 `Window::new` 必须用
/// 同一个常量算 egui area id,否则标题漂移后「点外面关」会静默失效。
pub(crate) const WINDOW_TITLE: &str = "设置";

/// 画设置弹窗。返回这一帧的结论。
pub fn show(
    ctx: &egui::Context,
    t: &Theme,
    draft: &mut SettingsDraft,
    env: SettingsEnv<'_>,
) -> SettingsOut {
    let mut out = SettingsOut::None;
    egui::Window::new(WINDOW_TITLE)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            annotate::mark(ui.ctx(), "设置弹窗", ui.max_rect());
            // F280:小屏幕上这个弹窗内容比屏幕还高,而 egui 的窗口 constrain
            // 只**平移**窗口不压缩内容 —— 不设上限的话大约 1/7 的内容画到了
            // 屏幕外面,划都划不到。
            //
            // 高度上界从**这一行离屏幕底还剩多少**实测,不走 `Window::max_height`:
            // 那个限的是内容区,得再自己减一个「标题栏 + 边框 + 内外边距」的
            // 常量(照 `project_manager.rs` 已验证过的写法)。
            let room = ctx.screen_rect().bottom() - ui.cursor().top() - SP_M;
            ui.set_max_height(room.max(160.0));
            // 宽度同理:小屏或大字号下表单可能比屏幕宽,横向也要能滚到。
            // 地板取 `FIELD_W_M`(320)—— 本表单里常规输入框就是按这一档给宽的,
            // 窗口比它还窄的话,连一个字段都摆不下,不如死死保住这一档。
            ui.set_max_width((ctx.screen_rect().width() - 2.0 * SP_M).max(FIELD_W_M));
            // **不猜按钮行高度去反推正文高度** —— `egui::Window` 内部走的是
            // `Resize`,每帧 `desired_size = desired_size.max(last_content_size)`
            // (0.30 `containers/resize.rs:258`,F217 已踩过),猜小了的差额会
            // 每帧累积,表现为打开弹窗时窗口自己长高一段时间。
            //
            // 治法照 `editor_window.rs`(F217)已验证的结构:`bottom_up` 里
            // **按钮先加、摆在最下面**,正文的 `ScrollArea` 用剩下的全部空间
            // (不给 `max_height`,egui 自己 `at_most(可用空间)`)。`ScrollArea`
            // 的内容 `Ui` 会继承外层 `bottom_up` 的方向,所以正文必须再套一层
            // `top_down` 把方向拨回来,否则各分节会整段倒着画。
            ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                ui.horizontal(|ui| {
                    // F283:两次输入的备份口令不一致时按不动 —— 只给一行红字
                    // 不够,弹窗一关红字就没了,用户会以为口令设上了,而实际
                    // 没写进去(忘记口令 = 云端已有的备份全部报废,这个误会
                    // 代价太大)。
                    let can_ok = !draft.cloud_pass_mismatch();
                    if ui
                        .add_enabled(can_ok, egui::Button::new("确定"))
                        .on_disabled_hover_text("两次输入的备份口令不一致,改成一样或都留空")
                        .clicked()
                    {
                        out = SettingsOut::Commit;
                    }
                    ui.add_space(SP_S);
                    if ui.button("取消").clicked() {
                        out = SettingsOut::Cancel;
                    }
                });
                ui.add_space(SP_L);
                ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        let mut first = true;
                        form::section(ui, t, "设置", "外观", &mut first);
                        appearance(ui, t, draft, env, &mut out);
                        form::section(ui, t, "设置", "远端", &mut first);
                        remote(ui, t, draft, &mut out);
                        form::section(ui, t, "设置", "文件面板", &mut first);
                        files(ui, t, draft, &mut out);
                        form::section(ui, t, "设置", "诊断", &mut first);
                        diagnostics(ui, t, draft, &mut out);
                        form::section(ui, t, "设置", "安全", &mut first);
                        security(ui, t, draft, env, &mut out);
                        form::section(ui, t, "设置", "云端备份", &mut first);
                        cloud(ui, t, draft, env, &mut out);
                        form::section(ui, t, "设置", "快捷键", &mut first);
                        shortcut_table(ui, t);
                    });
                });
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

/// 画一段**会换行**的灰字说明(F285)。
///
/// **不能直接 `ui.label(..)`**:`form::grid` 的单元格是 horizontal layout,而
/// egui 在 horizontal 下推断出来的默认 `wrap_mode` 是 `Extend` —— 整段排成一行,
/// **与可用宽多少完全无关**。`show()` 里那句 `ui.set_max_width(..)` 因此对它
/// 一点约束力都没有(F280 以为宽度那一半也修好了,其实从来没生效)。
///
/// 后果:自动定尺的 `egui::Window` 被这一行顶到 2243 逻辑点宽,再被
/// `CENTER_CENTER` 锚定**左右对称**切掉 —— 1366 宽的窗口上两边各约 439 点
/// 内容在窗外,划都划不到。而它**在大屏上完全正常**,所以开发机上永远看不见。
///
/// 守护在 `tests/dialog_bounds.rs`(三档窗口尺寸真渲染量矩形)。
fn wrapped_hint(ui: &mut egui::Ui, t: &Theme, text: &str) {
    ui.add(
        egui::Label::new(
            egui::RichText::new(text)
                .size(11.0)
                .color(theme::c32(t.fg_muted)),
        )
        .wrap(),
    );
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
        wrapped_hint(
            ui,
            t,
            "连上后开一条旁路命令通道,打开远端 tmux 的 set-titles 并让它报出当前目录。\
             分屏标题条上的目录名、以及文件面板继承终端所在目录都靠它。\
             改的是 tmux 服务器内存里的全局选项(不写任何文件,server 退出即失效),\
             那台机器上 attach 同一个 tmux 的其它终端,窗口标题也会跟着变成这个格式。",
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
        wrapped_hint(
            ui,
            t,
            "分屏刚连上时往远端 shell 发一行命令,让它此后每个提示符都报一次当前目录。\
             上面那条只在远端开着 tmux 时管用,这条管的是不经过 tmux 的场景 ——\
             文件面板继承终端所在目录靠它。\
             只改这条 shell 内存里的提示符钩子(bash 是 PROMPT_COMMAND,zsh 是 precmd_functions;\
             不写远端任何文件,断开即消失),\
             发完会清一次屏,所以登录横幅会被一起清掉。\
             远端 shell 不是 bash / zsh(比如 fish)时,屏幕上会打出一行报错,\
             那种情况请关掉这个开关。",
        );
        ui.end_row();
    });
    ui.add_space(SP_M);
}

/// 文件面板分节(F253):要不要显示 `.` 开头的项。
///
/// **单独一个分节**,没塞进「外观」:它不是配色/字号那类纯观感,它决定
/// 「有些文件你根本看不见」——本项目主场景(在远端跑 Claude Code)的工作
/// 目录里最要紧的东西恰好全是 `.` 开头的(`.claude/`、`.git/`、`.env`)。
///
/// 走 `form::grid` 两列骨架(规范 #1),复选框和灰字说明都挂**输入列**、
/// 标签列留空(规范 #6)。
fn files(ui: &mut egui::Ui, t: &Theme, draft: &mut SettingsDraft, out: &mut SettingsOut) {
    form::grid(ui, "settings_files", |ui| {
        ui.label("");
        if ui
            .checkbox(&mut draft.show_hidden_files, HIDDEN_FILES_LABEL)
            .changed()
        {
            *out = SettingsOut::Preview;
        }
        ui.end_row();

        ui.label("");
        wrapped_hint(
            ui,
            t,
            "点「确定」后连已经开着的面板也会跟着变(远端栏和本地栏都变)。\
             面板里按 Ctrl+H 也能切,那一下同样存下来 —— 两个入口是同一个开关。\
             默认开着:远端工作目录里最要紧的东西多半就是 .claude/、.git/ 这些。",
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
        wrapped_hint(
            ui,
            t,
            "常规档每 5 秒记一行性能剖面（帧耗时、吞吐、各阶段占用、回显往返），\
             排查卡顿靠它。详细档还会逐事件记录，日志会大很多。\
             环境变量 MULLION_LOG 若设了，会盖过这里的选择。",
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
        // F285:同 `wrapped_hint`,Grid 单元格里不显式 `.wrap()` 就不换行。
        // 这条不走 `wrapped_hint` 只因为它是 `danger_text` 而不是 `fg_muted`
        // —— 危险色是这句话的要点,不该被一个通用 helper 淹掉。
        ui.add(
            egui::Label::new(
                egui::RichText::new("忘记主密码没有找回途径 —— 已保存的密码与私钥将永久无法解开")
                    .size(11.0)
                    .color(theme::c32(t.danger_text)),
            )
            .wrap(),
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

/// 云端备份分节(F271)。
///
/// F283:门控从「有没有主密码」改成「库有没有打开」(`env.store_available`)——
/// 备份加密已经跟主密码解耦成独立的备份口令,继续拿主密码当闸门会把
/// 「填口令」那一格也一起罩住,而那格恰好是唯一能设上口令的入口
/// (F270 踩过的「没有出口的陷阱」的同形)。**「有没有设过口令」本身
/// 不做门控**,只用来画状态文字。
fn cloud(
    ui: &mut egui::Ui,
    t: &Theme,
    draft: &mut SettingsDraft,
    env: SettingsEnv<'_>,
    out: &mut SettingsOut,
) {
    // F283:门控从「有没有主密码」改成「库有没有打开」——原来那句
    // 「云端备份需要先设置主密码…」已经不成立(备份口令已跟主密码解耦),
    // 整段删掉。
    let ready = env.store_available;
    let avail = ui.available_width();
    let w = field_w(avail, FIELD_W_M, 0.0);

    // F271/D12:门控是**逐控件**的,不是整节一起罩,而且「开启云端备份」
    // 那颗复选框**故意不受 `ready` 门控**。原因:门控要挡的是「库没打开
    // 却填出一份注定失败的配置」,不是「已经开着的备份想关掉」—— 这颗
    // 复选框要是也灰着,就成了一个没有出口的陷阱。
    //
    // 受控字段各自套 `ui.add_enabled(ready, ..)`;`ui.end_row()` 全部留在
    // 这一层 grid 里,行结构不变 —— Task 12 的三条守护(逐行绑定/仅 SK
    // 掩码/逐控件带预览)按行数和顺序判,换成"两层 grid"会把它们全部拆穿。
    form::grid(ui, "settings_cloud", |ui| {
        ui.label("");
        if ui
            .checkbox(&mut draft.cloud_enabled, CLOUD_ENABLED_LABEL)
            .changed()
        {
            *out = SettingsOut::Preview;
        }
        ui.end_row();

        ui.label("Endpoint");
        if ui
            .add_enabled(
                ready,
                egui::TextEdit::singleline(&mut draft.cloud_endpoint).desired_width(w),
            )
            .changed()
        {
            *out = SettingsOut::Preview;
        }
        ui.end_row();

        ui.label("Region");
        if ui
            .add_enabled(
                ready,
                egui::TextEdit::singleline(&mut draft.cloud_region).desired_width(w),
            )
            .changed()
        {
            *out = SettingsOut::Preview;
        }
        ui.end_row();

        ui.label("Bucket");
        if ui
            .add_enabled(
                ready,
                egui::TextEdit::singleline(&mut draft.cloud_bucket).desired_width(w),
            )
            .changed()
        {
            *out = SettingsOut::Preview;
        }
        ui.end_row();

        ui.label("前缀");
        if ui
            .add_enabled(
                ready,
                egui::TextEdit::singleline(&mut draft.cloud_prefix).desired_width(w),
            )
            .changed()
        {
            *out = SettingsOut::Preview;
        }
        ui.end_row();

        ui.label("Access Key ID");
        if ui
            .add_enabled(
                ready,
                egui::TextEdit::singleline(&mut draft.cloud_access_key_id).desired_width(w),
            )
            .changed()
        {
            *out = SettingsOut::Preview;
        }
        ui.end_row();

        ui.label("Access Key Secret");
        if ui
            .add_enabled(
                ready,
                egui::TextEdit::singleline(&mut draft.cloud_secret_new)
                    .password(true)
                    .desired_width(w)
                    .hint_text("留空 = 不改"),
            )
            .changed()
        {
            *out = SettingsOut::Preview;
        }
        ui.end_row();

        // F283:备份口令,两遍确认。与主密码彻底分开(设计 D7:不提供
        // 「用主密码当备份口令」的快捷项,加了等于把耦合请回来一半)。
        ui.label("备份口令");
        if ui
            .add_enabled(
                ready,
                egui::TextEdit::singleline(&mut draft.cloud_pass_new)
                    .password(true)
                    .desired_width(w)
                    .hint_text("留空 = 不改"),
            )
            .changed()
        {
            *out = SettingsOut::Preview;
        }
        ui.end_row();

        ui.label("确认口令");
        if ui
            .add_enabled(
                ready,
                egui::TextEdit::singleline(&mut draft.cloud_pass_confirm)
                    .password(true)
                    .desired_width(w),
            )
            .changed()
        {
            *out = SettingsOut::Preview;
        }
        ui.end_row();

        form::field_error(
            ui,
            t,
            draft.cloud_pass_mismatch(),
            "两次输入的备份口令不一致",
        );

        ui.label("");
        wrapped_hint(
            ui,
            t,
            if env.cloud_has_passphrase {
                "当前:已设置备份口令"
            } else {
                "当前:还没设置备份口令,云端备份不会运行"
            },
        );
        ui.end_row();

        ui.label("");
        wrapped_hint(
            ui,
            t,
            "备份口令与主密码无关。忘了没有找回途径,云端已有的备份将无法恢复。",
        );
        ui.end_row();

        ui.label("");
        if ui
            .add_enabled(
                ready,
                egui::Checkbox::new(&mut draft.cloud_path_style, CLOUD_PATH_STYLE_LABEL),
            )
            .changed()
        {
            *out = SettingsOut::Preview;
        }
        ui.end_row();

        ui.label("保留份数");
        if ui
            .add_enabled(
                ready,
                egui::DragValue::new(&mut draft.cloud_keep).range(1..=200),
            )
            .changed()
        {
            *out = SettingsOut::Preview;
        }
        ui.end_row();

        // **这一行不能省。** 片一完全不删云端对象(清理是片二的活),
        // 这个数字存得下、也回得来,但没有任何代码会用它 —— 不说明的话
        // 它就是一个假开关:用户设成 5,以为云上只会留 5 份,实际一直在涨,
        // 直到有天发现 bucket 里几百个对象。这类「看得见摸不着的开关」
        // 本项目在 F265 上刚吃过一次(「灯早就有了,用户根本没注意到」的
        // 反面:控件早就有了,用户以为它在起作用)。
        //
        // 小字用 `.size(11.0)` + `c32(t.fg_muted)`,跟本分节另外两处说明
        // 以及 `settings.rs` 里其余六处同形。**别改成 `theme::hint_text`**:
        // 那一层是给 `TextEdit` 的 hint 用的(egui 派生的 weak 色达不到 AA),
        // 它给的是 `fg_dimmer`,跟并排的两段说明会深浅不一。这个文件里两套
        // 写法确实并存(6 处 vs 2 处),新写的一律跟多数那套走,
        // 至少别在同一个分节里混用。
        ui.label("");
        wrapped_hint(ui, t, "下一个版本生效：当前版本只往上传，不清理旧份");
        ui.end_row();

        ui.label("检查间隔");
        if ui
            .add_enabled(
                ready,
                egui::DragValue::new(&mut draft.cloud_interval_min)
                    .range(5..=1440)
                    .suffix(" 分钟"),
            )
            .changed()
        {
            *out = SettingsOut::Preview;
        }
        ui.end_row();

        // SOCKS5 代理。**不补这一格的话 `socks5` 参数就是条死线** ——
        // `mullion-cloud` 为它开了 ureq 的 `socks-proxy` 特性、
        // `S3Client::new` 专门收了这个参数,而设计 D15 把「SOCKS 代理
        // 链路通不通」列进了片一的真机验收项。没有入口就永远传 `None`,
        // 那条验收项验的是一条从没走过的路。
        ui.label("SOCKS5 代理");
        if ui
            .add_enabled(
                ready,
                egui::TextEdit::singleline(&mut draft.cloud_socks5)
                    .desired_width(w)
                    // **hint 里写清不带 `socks5://`**:`S3Client::new` 自己
                    // 补前缀,用户照直觉填全 URL 的话会拼成
                    // `socks5://socks5://…` 而当场报「配置不合法」。
                    .hint_text("127.0.0.1:1080,留空 = 直连"),
            )
            .changed()
        {
            *out = SettingsOut::Preview;
        }
        ui.end_row();

        ui.label("");
        wrapped_hint(
            ui,
            t,
            "整份配置会用主密码派生的密钥加密之后再上传，云上那份是不可读的二进制；\
             内容没变就不上传。窗口布局与现场记录不上云（它们是这台机器的属性）。\
             建议用 RAM 子账号、只授权这一个 bucket 的这一个前缀。",
        );
        ui.end_row();
    });
    ui.add_space(SP_M);
}

/// 快捷键一览。只读表格,数据源是 `ui::shortcuts::SHORTCUTS`(那边有撞键守护)。
///
/// F260:组合键那一列**必须显式给色**。原来写的是 `RichText::new(..).strong()`,
/// 而 egui 的 `strong` 不是「加粗」而是「换成 `Visuals::strong_text_color()`」,
/// 后者取的是 `widgets.active.fg_stroke`——本项目把它设成了 `accent_fg`
/// (#0d0f16,专门给亮色 accent 底做反白用的近黑)。落在弹窗底 #3f3f3f 上是
/// **1.82:1**,正文门槛 4.5:1,于是一整列快捷键几乎看不见。
///
/// 显式 `.color(..)` 之后 `.strong()` 就只剩噪音了(`RichText::get_text_color`
/// 里 `text_color` 排在 `strong` 前面,给了色 strong 一点效果都没有),所以
/// 直接去掉;这一列的「更醒目」靠 `fg`(9.4:1)与另一列的 `fg_muted`(4.77:1)
/// 分层。全库级的守护在 `tests/strong_text_color.rs`。
///
/// F295:按小节分组 —— 每节一行小标题 + 两列(原来是不分节的三列,中间那列
/// 是「在哪儿生效」,每行都重复一遍)。
///
/// 表头**不用 `theme::hint_text`**:那个是 `fg_dimmer`,在 modal_bg 上只有
/// 3.33:1(低于 AA),比它领着的正文行还暗。改成与 `form::section` 同一套
/// (11pt + `fg_muted`),让「这是个标题」靠字号而不是靠更淡。
///
/// 也**不再自带 `ScrollArea`**:设置正文外层已经是一个不设 max_height 的
/// `ScrollArea`(F217 的形状),内层再套一个 220pt 的只会露出两节,还把守护
/// 测试逼成只查得了前三节。
fn shortcut_table(ui: &mut egui::Ui, t: &Theme) {
    egui::Grid::new("settings_shortcuts")
        .num_columns(2)
        .spacing([SP_M, SP_S])
        .show(ui, |ui| {
            for (name, rows) in crate::ui::shortcuts::sections() {
                // 小节表头占一整行。**八节共用这一个 Grid**:每节各起一个的话
                // 列宽各算各的,「作用」那一列的左沿会节节参差。
                ui.label(
                    egui::RichText::new(name)
                        .size(11.0)
                        .color(theme::c32(t.fg_muted)),
                );
                ui.label("");
                ui.end_row();
                for s in rows {
                    ui.label(egui::RichText::new(s.keys.display()).color(theme::c32(t.fg)));
                    ui.label(s.what);
                    ui.end_row();
                }
            }
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
    /// (逐帧量「取消」按钮的位置得出)。
    ///
    /// F280 套 `ScrollArea` 之后多嵌了一层(`bottom_up` + 里面再 `top_down`),
    /// 实测第 10 帧才稳,这里取 12 留余量。**这里不能靠猜大一点的常数糊过去**
    /// ——先试过给正文 `ScrollArea::max_height` 塞一个「离屏幕底减去按钮行
    /// 猜出来的高度」,`egui::Window` 内部走 `Resize`,`desired_size` 每帧
    /// 只涨不缩(F217),猜小了的差额会一直累积,实测要连续跑 30 帧才收敛
    /// (意味着真机上打开弹窗时窗口会自己长高近半秒,是真 bug 不是测试假象)。
    /// 改成 F217 已验证的 `bottom_up` 结构(按钮先摆到底,正文吃剩下的全部,
    /// 不猜任何一个控件的高度)之后收敛帧数就掉回个位数尾巴,10~12 帧这个
    /// 量级才是「多一层容器多一帧」的正常收敛,不是又踩中一次棘轮。
    ///
    /// 帧数不够有两种症状,都不报错:少太多是画面从中间某一行起整段消失;
    /// 差一两帧是位置还差几个像素 —— 于是点击落在按钮外面,`click` 返回
    /// `None`,看着像「按钮不响应」。
    const FRAMES: usize = 12;

    fn draft() -> SettingsDraft {
        // 只覆盖这一组测试真正在意的那几项,其余从构造器起手。
        // **不要**把 11 个云端字段一个个补进来 —— 那样每加一个字段都要回来
        // 改一次,而补错值的表现是这一整组测试悄悄测了别的东西。
        SettingsDraft {
            family: Some("Cascadia Mono".into()),
            font_pt: 10.0,
            typed: "Cascadia Mono".into(),
            ..SettingsDraft::from_settings(&mullion_store::Settings::default())
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
                        cloud_has_passphrase: false,
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
                        cloud_has_passphrase: false,
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
                    cloud_has_passphrase: false,
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
            ..draft()
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

    // ---- F253 文件面板分节 ----

    /// F253:点这个开关要当场回报 `Preview`(草稿变了、要重画)。回报 `None`
    /// 的话用户点了没反应。用文件里既有的 `interact` 脚手架(跑满 `FRAMES`
    /// 帧预热,再按标签文字找部件中心点下去;复选框要同帧松手)。
    ///
    /// 自证会变红:把这个复选框的 `.changed()` 分支删掉。
    #[test]
    fn toggling_the_hidden_files_checkbox_reports_a_preview() {
        let mut d = draft();
        assert!(d.show_hidden_files, "脚手架的初值该是开着的");
        let out = interact(&mut d, HIDDEN_FILES_LABEL, egui::Vec2::ZERO, true);
        assert!(
            !d.show_hidden_files,
            "复选框没被真的点到,这条测试测了个寂寞"
        );
        assert_eq!(out, SettingsOut::Preview);
    }

    /// F253:点这个不许把远端那两个开关带翻 —— 同一份 `draft` 上的字段名
    /// 只差几个字母,复制粘贴写错第一个参数**不报错、只是两个开关联动**
    /// (F156-c 那条测试就是为这个写的)。
    #[test]
    fn toggling_hidden_files_does_not_drag_the_remote_switches() {
        let mut d = draft();
        let _ = interact(&mut d, HIDDEN_FILES_LABEL, egui::Vec2::ZERO, true);
        assert!(!d.show_hidden_files, "点的是隐藏项那个");
        assert!(d.tmux_bootstrap && d.shell_osc7_bootstrap, "带翻了远端开关");
    }

    /// F253:草稿从**落盘的真值**起。起错了的症状是「用户关掉过,再打开设置
    /// 弹窗又显示开着」—— 而只要他这时点了确定,关掉的选择就被覆盖回去。
    ///
    /// 自证会变红:把 `from_settings` 里那行改成 `show_hidden_files: true,`。
    #[test]
    fn the_hidden_files_draft_starts_from_the_stored_switch() {
        let s = mullion_store::Settings {
            show_hidden_files: false,
            ..Default::default()
        };
        assert!(!SettingsDraft::from_settings(&s).show_hidden_files);
        assert!(
            SettingsDraft::from_settings(&mullion_store::Settings::default()).show_hidden_files
        );
    }

    /// F253:灰字说明必须点明**已经开着的面板会跟着变**。
    ///
    /// 这不是凑文案:这一项和面板里的 `Ctrl+H` 是同一个真值的两个入口
    /// (`Ctrl+H` 写穿这里、这里刷回所有已开面板)。不说的话,用户在设置里
    /// 勾完、回到面板看见变了,会以为是别的什么东西在动。
    #[test]
    fn the_hidden_files_hint_says_open_panels_follow_along() {
        let mut d = draft();
        let (texts, _) = run(&mut d, false);
        let joined = texts.join("\n");
        assert!(
            joined.contains(HIDDEN_FILES_LABEL),
            "没画这个开关:{texts:?}"
        );
        assert!(
            joined.contains("Ctrl+H"),
            "说明里没提面板内的 Ctrl+H:{texts:?}"
        );
        assert!(
            joined.contains("已经开着的"),
            "说明里没交代已开面板会跟着变:{texts:?}"
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

    /// F295:表按小节画,Esc 只出现一次。
    ///
    /// 内层 `ScrollArea` 去掉之后全部八节都画得出来,所以查得到后面的节。
    /// **故意不查「文件面板」**——设置弹窗自己有一节就叫这个名字,拿它当
    /// 判据的话,哪怕分节整个没画出来也照样绿(判据被同名的无关文本接住
    /// = 恒绿)。
    ///
    /// 「整表 Esc 唯一」在这里是**画面层**的判据;结构层由
    /// `shortcuts::tests::escape_is_listed_exactly_once` 守,两条都要在。
    ///
    /// 自证会变红:把 `shortcut_table` 里那行小节表头 `ui.label(RichText::new(name)…)`
    /// 删掉(第一条红);往 `SHORTCUTS` 的任意一节(含「标注模式」)加一行
    /// `Keys::Text("Esc")`(第二条红)。
    #[test]
    fn the_shortcut_table_is_grouped_into_sections() {
        let mut d = draft();
        let (texts, _) = run(&mut d, false);
        for want in [
            "通用",
            "标签",
            "终端",
            "会话管理器",
            "标注模式",
            "项目",
            "命令抽屉",
        ] {
            assert!(
                texts.iter().any(|s| s == want),
                "没画小节「{want}」:{texts:?}"
            );
        }
        assert_eq!(
            texts.iter().filter(|s| s.as_str() == "Esc").count(),
            1,
            "Esc 不是恰好一次:{texts:?}"
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

    // ---- F271 云端备份分节 ----

    /// F283:门控从「有没有主密码」改成「库有没有打开」——没有主密码时
    /// 云端分节**不该**再置灰、也不该再说「需要先设置主密码」(那句话
    /// 现在是错的:备份加密已经解耦成独立口令)。
    ///
    /// 继续拿主密码当闸门会把「填口令」那一格也一起罩住,那格恰好是
    /// 唯一能设上口令的入口(F270 踩过的「没有出口的陷阱」的同形)。
    #[test]
    fn the_cloud_section_is_no_longer_gated_by_the_master_password() {
        let mut d = draft();
        let out = interact_env(
            &mut d,
            CLOUD_ENABLED_LABEL,
            egui::Vec2::ZERO,
            true,
            /* store_available */ true,
            /* has_master_password */ false,
        );
        assert_eq!(
            out,
            SettingsOut::Preview,
            "没有主密码、但库开着时,开关应该还能点"
        );
        assert!(d.cloud_enabled, "开关没被点开");

        let (texts, _) = run_env(&mut d, false, /* has_master_password */ false);
        assert!(
            !texts.iter().any(|t| t.contains("需要先设置主密码")),
            "云端分节不该再拿主密码当门槛:{texts:?}"
        );
    }

    /// 设了主密码之后,开关必须真的可点,且点一下报 Preview(草稿变了,
    /// 要等「确定」才落盘)。
    ///
    /// **最后两个 `true` 是 `store_available` / `has_master_password`** ——
    /// 任一为 `false` 的话整节是 `add_enabled_ui(false)`,点不动,这条会红在
    /// 一个跟它想测的东西无关的原因上。
    #[test]
    fn toggling_the_cloud_switch_reports_a_preview() {
        let mut d = draft();
        let out = interact_env(
            &mut d,
            CLOUD_ENABLED_LABEL,
            egui::Vec2::ZERO,
            true,
            true,
            true,
        );
        assert_eq!(out, SettingsOut::Preview);
        assert!(d.cloud_enabled, "开关没被点开");
    }

    /// 草稿必须从**落盘的那份**起,不是硬编码默认值。
    /// 从默认值起的症状:打开设置弹窗、什么都没动、点「确定」,
    /// 用户配好的云端备份被关掉了。
    ///
    // `CloudConfig::corrupt` 是私有的,`..Default::default()` 在 mullion-store
    // 之外编不过(E0451)。只能 default 完再逐字段赋 —— 没有别的写法。
    // **别为了这条 lint 把 `corrupt` 改成 pub**,它私有是 Task 8 的设计。
    #[allow(clippy::field_reassign_with_default)]
    #[test]
    fn the_cloud_draft_starts_from_the_stored_config_not_a_hardcoded_default() {
        let mut stored = mullion_store::CloudConfig::default();
        stored.enabled = true;
        stored.bucket = "my-bucket".into();
        stored.keep = 7;
        let d =
            SettingsDraft::from_settings_and_cloud(&mullion_store::Settings::default(), &stored);
        assert!(d.cloud_enabled);
        assert_eq!(d.cloud_bucket, "my-bucket");
        assert_eq!(d.cloud_keep, 7);
    }

    /// `cloud()` 分节的登记表:**(源码里那句标签怎么写的, 它该绑的草稿字段)**。
    ///
    /// 标签一列存的是「源码里的那串字符」而不是「显示出来的文案」——
    /// 两个 checkbox 的标签是常量名(`CLOUD_ENABLED_LABEL`),不是字面串。
    const CLOUD_ROWS: &[(&str, &str)] = &[
        ("CLOUD_ENABLED_LABEL", "cloud_enabled"),
        ("ui.label(\"Endpoint\")", "cloud_endpoint"),
        ("ui.label(\"Region\")", "cloud_region"),
        ("ui.label(\"Bucket\")", "cloud_bucket"),
        ("ui.label(\"前缀\")", "cloud_prefix"),
        ("ui.label(\"Access Key ID\")", "cloud_access_key_id"),
        ("ui.label(\"Access Key Secret\")", "cloud_secret_new"),
        ("ui.label(\"备份口令\")", "cloud_pass_new"),
        ("ui.label(\"确认口令\")", "cloud_pass_confirm"),
        ("CLOUD_PATH_STYLE_LABEL", "cloud_path_style"),
        ("ui.label(\"保留份数\")", "cloud_keep"),
        ("ui.label(\"检查间隔\")", "cloud_interval_min"),
        ("ui.label(\"SOCKS5 代理\")", "cloud_socks5"),
    ];

    /// 剥掉行注释。**源码切片守护必须剥注释**:这个文件的注释里字面写着
    /// 「前缀」「`socks5://`」这类词,不剥的话判据会命中注释而不是代码,
    /// 造出假绿(以及改注释就假红)。本项目已把这条登记成独立欠账。
    ///
    /// 只剥 `//` 起的行尾。`cloud()` 体内没有含 `//` 的字符串字面量
    /// (唯一提到 `socks5://` 的地方在注释里,正好被剥掉),所以这个
    /// 朴素版够用;将来往里加带 `//` 的字面串要回来看这里。
    fn strip_comments(s: &str) -> String {
        s.lines()
            .map(|l| l.split("//").next().unwrap_or(l))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 取 `cloud()` 的函数体,剥注释后按 `ui.end_row();` 切成一行一块。
    fn cloud_rows() -> Vec<String> {
        let src = include_str!("settings.rs");
        // 先切掉测试模块:这条测试自己的正文里就字面写着 `fn cloud(`、
        // `.password(true)` 和上面那张登记表,不切的话判据会落在测试自己身上。
        // (已核实:本文件只有 `settings.rs:818` 一处 `#[cfg(test)]`。)
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .expect("测试模块分界变了,这几条测试的锚点失效了");
        assert!(
            prod.len() < src.len(),
            "没能切掉测试模块 —— 下面会考到测试自己"
        );
        let body = prod.split("fn cloud(").nth(1).expect("没有 cloud 分节函数");
        // 分节函数都在顶格,下一个 `\nfn ` 就是本节的结束。
        let body = body.split("\nfn ").next().unwrap_or(body);
        let body = strip_comments(body);
        let rows: Vec<String> = body.split("ui.end_row();").map(str::to_owned).collect();
        assert!(
            rows.len() > CLOUD_ROWS.len(),
            "切出来的行块比登记的控件还少({}) —— 多半是切错了,\
             下面几条断言会变成考空气",
            rows.len()
        );
        rows
    }

    /// 一个行块里绑了哪些 `cloud_*` 草稿字段。
    ///
    /// **按标识符取,不用 `contains("&mut draft.cloud_prefix")` 这种子串包含。**
    /// 子串判据在字段名互为前缀时会误命中(真加一个 `cloud_prefix2`,
    /// `cloud_prefix` 的判据就被它那一行接走了)。现有 11 个字段之间恰好没有
    /// 前缀关系,而「恰好」不是判据该有的底气 —— 何况另一半的反列举能不能
    /// 兜住这种误判,得靠一段绕来绕去的交叉论证才说得清,那本身就是上一轮
    /// 栽的那种「判据放在看不出差别的那一层」。token 级精确匹配是自证的。
    fn fields_in(row: &str) -> std::collections::BTreeSet<String> {
        row.split("&mut draft.")
            .skip(1)
            .map(|p| {
                p.chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect::<String>()
            })
            .filter(|i| i.starts_with("cloud_"))
            .collect()
    }

    /// 找到「标签 + 绑了这个字段(token 级)」的那一块。三条测试共用同一套
    /// 定位逻辑,省得各写一遍、改一处忘了改另外两处。
    ///
    /// **`each_cloud_control_is_bound_to_its_own_draft_field` 不走这个**:
    /// 它要的判据是「恰好一块」而不是「找到第一块」,`find` 吞掉了「命中
    /// 几块」这个信息,会把「恰好一块」这层判据弄没了。
    fn row_of<'a>(rows: &'a [String], label: &str, field: &str) -> &'a str {
        rows.iter()
            .find(|r| r.contains(label) && fields_in(r).contains(field))
            .unwrap_or_else(|| panic!("找不到「{label}」那一块"))
    }

    /// 每一格控件都必须绑在**自己那个**草稿字段上。
    ///
    /// **判据是「恰好一个行块同时含这句标签、且 token 级绑了这个字段」**,
    /// 不是「整个分节里出现过这个字段」。后者是本项目恒绿清单里「判据放在
    /// 看不出差别的那一层」:把 AK ID 与 AK Secret 的绑定互换,两个字段在
    /// 分节里都还在,整段扫描全绿,而界面上真实的 Secret Key 明文显示。
    /// 切到行块之后,互换会让「Access Key Secret」那一块里出现
    /// `cloud_access_key_id`,当场变红。
    ///
    /// 第二半是**反列举**:分节里出现的每个 `&mut draft.cloud_*` 都必须在
    /// 登记表里。没有这一半的话,将来加第 12 个字段谁也不会想起来补守护
    /// ——「列举式门控在加档时必然漏」本项目已经踩中三次。
    #[test]
    fn each_cloud_control_is_bound_to_its_own_draft_field() {
        let rows = cloud_rows();
        for (label, field) in CLOUD_ROWS {
            let hits = rows
                .iter()
                .filter(|r| r.contains(label) && fields_in(r).contains(*field))
                .count();
            assert_eq!(
                hits, 1,
                "「{label}」这一格没有正好绑在 `draft.{field}` 上(命中 {hits} 块)。\
                 绑串了的话用户改 A 改的是 B,而编译、clippy、其余测试全干净。"
            );
        }

        let registered: std::collections::BTreeSet<&str> =
            CLOUD_ROWS.iter().map(|(_, f)| *f).collect();
        let seen: std::collections::BTreeSet<String> =
            rows.iter().flat_map(|r| fields_in(r)).collect();
        let seen: std::collections::BTreeSet<&str> = seen.iter().map(String::as_str).collect();
        assert_eq!(
            seen, registered,
            "cloud 分节里绑的草稿字段与登记表对不上。新加的字段要同时加进 \
             `CLOUD_ROWS`,否则它天生没有守护。"
        );
    }

    /// F283:口令与确认两格都必须打码,别的格一格都不许。
    ///
    /// 这条取代 `only_the_secret_key_field_is_masked`(判据从"只有 SK"
    /// 扩成"SK 与两个口令格"),**两头都钉**:漏钉后半句的话,
    /// "把 password(true) 挂到 Endpoint 上"这种复制粘贴 bug 逃得掉。
    #[test]
    fn only_the_secret_fields_are_masked() {
        let rows = cloud_rows();
        for (label, field) in CLOUD_ROWS {
            let row = row_of(&rows, label, field);
            assert_eq!(
                row.contains(".password(true)"),
                matches!(
                    *field,
                    "cloud_secret_new" | "cloud_pass_new" | "cloud_pass_confirm"
                ),
                "「{label}」的打码状态不对"
            );
        }
    }

    /// F283:两次输入不一致时,**「确定」必须按不动**。
    ///
    /// 只给一行红字是不够的:用户点了确定、对话框关掉、红字消失,
    /// 他会以为口令设好了 —— 而实际上没写进去。忘记口令 = 云端备份作废,
    /// 这个误会的代价太大。
    ///
    /// **真跑一遍 egui**(`interact` 驱动 `ctx.run` 找到写着「确定」的部件
    /// 并合成鼠标点击),不是读源码切片 —— 单读源码只能证明"写了
    /// `add_enabled` 这几个字",证不了"传进去的条件真的挡住了点击"。
    #[test]
    fn a_mismatched_passphrase_blocks_the_ok_button() {
        let mut d = draft();
        d.cloud_pass_new = "abc123".into();
        d.cloud_pass_confirm = "xyz789".into();
        let out = click(&mut d, "确定");
        assert_ne!(
            out,
            SettingsOut::Commit,
            "两次输入的备份口令不一致时,「确定」不该生效"
        );
    }

    /// 两次输入一致(或都留空 = 不改)时,「确定」必须能按下去 ——
    /// 否则上一条测试的判据只是恰好卡住了所有输入,不是精确地只卡不一致。
    #[test]
    fn a_matching_or_empty_passphrase_lets_the_ok_button_through() {
        let mut d = draft();
        d.cloud_pass_new = "abc123".into();
        d.cloud_pass_confirm = "abc123".into();
        let out = click(&mut d, "确定");
        assert_eq!(out, SettingsOut::Commit, "两次输入一致时,「确定」应该生效");

        let mut d2 = draft();
        let out2 = click(&mut d2, "确定");
        assert_eq!(out2, SettingsOut::Commit, "两格都留空时,「确定」应该生效");
    }

    /// 设置页必须写明"忘了没有找回途径"(设计 D7 的硬要求)。
    #[test]
    fn the_cloud_section_says_a_forgotten_passphrase_cannot_be_recovered() {
        let src = include_str!("settings.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .expect("测试模块分界变了,这条测试的锚点失效了");
        assert!(
            prod.len() < src.len(),
            "没能切掉测试模块 —— 下面会考到测试自己"
        );
        let body = prod.split("fn cloud(").nth(1).expect("没有 cloud 分节函数");
        let body = body.split("\nfn ").next().unwrap_or(body);
        assert!(
            body.contains("忘") && body.contains("找回"),
            "云端备份分节里没有「忘了没有找回途径」这层警示"
        );
    }

    /// 每一格改动都要报 `Preview`,否则「确定」按钮不亮 / 预览不刷新 ——
    /// 用户填完一整页,点不动保存,而且没有任何报错。
    #[test]
    fn every_cloud_control_reports_a_preview() {
        let rows = cloud_rows();
        for (label, field) in CLOUD_ROWS {
            let row = row_of(&rows, label, field);
            assert!(
                row.contains("*out = SettingsOut::Preview;"),
                "「{label}」改了不报 Preview —— 保存按钮不会亮,而且不报错:\n{row}"
            );
        }
    }

    /// 门控是逐控件的,而且「开启云端备份」这颗**故意在门外**。
    ///
    /// 门控的目的是不让库还没打开的用户配出一个注定失败的备份,不是拦住
    /// 他关掉已经开着的那个。两者一起锁的后果是:库一旦打不开,云备份每
    /// 60 秒失败一次,而用户进设置也关不掉 —— 一个没有出口的陷阱。
    ///
    /// **闸门本身换过一次**:F283 之前是「有没有设主密码」(设计 D12,
    /// 当时载荷用 vault 密钥封、换台机器解不开),F283 把载荷改成用独立
    /// 备份口令派生之后那条理由不成立了,闸门只剩「库打开着」。这条测试
    /// 守的不变量两次都一样,换的只是闸门的含义。
    ///
    /// 遍历 `CLOUD_ROWS` 而不是逐个列举,所以**两种反向变异都杀得掉**:
    /// 把开关也罩进门控(第一半红),以及将来加一个新字段却忘了给它门控
    /// (第二半红)。「列举式门控在加档时必然漏」是本项目登记过三次的形状。
    #[test]
    fn only_the_enable_toggle_escapes_the_store_gate() {
        let rows = cloud_rows();
        for (label, field) in CLOUD_ROWS {
            let row = row_of(&rows, label, field);
            if *field == "cloud_enabled" {
                assert!(
                    !row.contains("add_enabled("),
                    "「开启云端备份」被罩进了门控 —— 库一打不开用户就再也关不掉它:{row}"
                );
            } else {
                assert!(
                    row.contains("add_enabled("),
                    "`{field}` 这个控件没有门控 —— 库还没打开的用户能填出一份注定失败的配置:{row}"
                );
            }
        }
    }

    /// 从 `src[from]`(必须是 `{`)起配平花括号,返回含首尾大括号的子串。
    /// `panic_msg` 只用于配平失败时的提示,方便区分是哪一次切片炸的。
    fn brace_balanced<'a>(src: &'a str, from: usize, panic_msg: &str) -> &'a str {
        let mut depth = 0i32;
        let mut end = from;
        for (i, c) in src[from..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = from + i;
                        break;
                    }
                }
                _ => {}
            }
        }
        assert!(end > from, "{panic_msg}");
        &src[from..=end]
    }

    /// F280:设置窗要按可视区收缩。三件事缺一不可,分开断言 ——
    /// ① 高度上界从「离屏幕底实测剩余」来(F217:不许用 Window::max_height
    ///    再猜 chrome 常量);② 正文套 ScrollArea(小屏时靠滚动够到全部内容);
    /// ③ 确定/取消在 `ScrollArea::show` 闭包体**之外**(卷进去的话,小屏上
    ///    按钮要滚到底才看得见)—— 判据是花括号配平切出闭包体后看「确定」
    ///    在不在里面,不是比较两个子串谁在源码里先出现:本项目按 F217 的
    ///    `bottom_up` 结构实现,按钮在源码里先加、视觉上却在最下面,源码
    ///    顺序与视觉顺序刻意相反,朴素的下标判据会认反。
    #[test]
    fn the_settings_window_shrinks_to_the_screen_instead_of_overflowing() {
        let src = strip_comments(include_str!("settings.rs"));
        // 用花括号配平切出 `show()` 自己的函数体,**不用 `\npub fn ` 当右边界**:
        // `show` 之后的分节函数(`appearance`/`shortcut_table`……)全是私有 `fn`,
        // 不带 `pub`,`\npub fn ` 永远碰不到下一个边界,会把 `body` 一路撑到
        // 文件末尾 —— `shortcut_table` 自己那个无关的 `ScrollArea::vertical()`
        // 也被囊括进来,变异掉 `show()` 里刚加的 ScrollArea 时这条测试照样绿
        // (已用变异验证过一次:只删 `show()` 里的 ScrollArea/`bottom_up`,
        // 守护测试因为切到了 `shortcut_table` 的 ScrollArea 而假绿)。
        let after_sig = src.split("pub fn show(").nth(1).expect("show 没了");
        let brace_start = after_sig.find('{').expect("show 没有函数体");
        let body = brace_balanced(after_sig, brace_start, "`show` 花括号没配平");
        assert!(
            body.contains("screen_rect().bottom() - ui.cursor().top()"),
            "高度上界不是实测剩余(F217:别用 Window::max_height 猜 chrome)"
        );
        assert!(body.contains("ScrollArea"), "正文没套滚动区");
        // 确定/取消必须在 `ScrollArea::show` 那个闭包体**之外** —— 卷进去的话
        // 小屏上要滚到底才够得着。
        //
        // **不比较两个子串谁先出现**:F217 已验证的正确结构是 `bottom_up`
        // (按钮在源码里先加、摆到视觉上的最下面,`ScrollArea` 在源码里后写
        // 却是视觉上方的正文),源码顺序与视觉顺序相反是**有意的**——朴素的
        // “谁的下标大”判据在这个结构下会认反。改成花括号配平,精确切出
        // `ScrollArea::show` 闭包体的真实范围,判据落在“确定是否被包进那个
        // 范围”上,跟外层用 `bottom_up` 还是顺着写没有关系。
        let scroll_at = body.find("ScrollArea").unwrap();
        let brace_start = body[scroll_at..]
            .find('{')
            .map(|i| i + scroll_at)
            .expect("ScrollArea::show 没有闭包体");
        let scroll_body = brace_balanced(body, brace_start, "花括号没配平,切不出闭包体");
        assert!(
            !scroll_body.contains("\"确定\""),
            "确定按钮被卷进了滚动区 —— 小屏上要滚到底才看得见"
        );
    }
}
