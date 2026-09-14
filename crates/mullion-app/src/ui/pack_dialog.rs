//! F46-a:整机迁移包的弹窗 —— 导出要一次性口令,导入要同一串口令。
//!
//! **零 IO**:这里只收口令并回报一个 [`PackOut`],读包/写包/替换配置目录
//! 全在 `app.rs`(打包解包本身在 `mullion_store::portable`)。姿态与
//! `ui/unlock.rs` 一致。
//!
//! 三态一个窗口,不是三个窗口:它们是同一件事的三个瞬间,而每多一个
//! `egui::Window` 就要多进一遍 `Modal`/`DISMISS_ORDER`/`dismiss_areas`
//! 三张表(切片 I 的教训:少一张就是一个静默的 bug)。

use crate::theme::{self, Theme};
use crate::ui::annotate;
use crate::ui::metrics::{field_w, FIELD_W_M, SP_L, SP_M, SP_S, SP_XS};

/// 窗口标题。三态共用 —— `dismiss_areas` 按标题算 area id,标题随状态变的话
/// 「点外面关掉」会在某一态上静默失效。
pub const WINDOW_TITLE: &str = "配置迁移";

/// 一份包里有什么,给用户在输口令之前看一眼。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackInfo {
    /// 导出它的客户端版本。
    pub app_version: String,
    /// 导出时刻(RFC3339 原文)。
    pub exported_at: String,
    /// 带了几个明文文件。
    pub files: usize,
    /// 带没带密文。源机一条密码都没存过时是 `false` —— 这时口令框应当不出现,
    /// 否则用户会对着一个没用的框猜自己当初设了什么。
    pub has_secrets: bool,
}

/// 弹窗此刻在哪一步。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackStage {
    /// 导出:设一次性口令(两遍)。
    Export,
    /// 导入:包已经读出来了,要口令。
    Import { path: String, info: PackInfo },
    /// 导入完成。**这一态不能被 toast 代替** —— 「请重启」是用户必须看到的
    /// 一句话,而 toast 三秒就没了。
    Done {
        backup: String,
        written: usize,
        skipped: usize,
    },
}

/// 弹窗自己的那点状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackDialog {
    pub stage: PackStage,
    pub pass: String,
    /// 导出时的第二遍。导入时不用。
    pub confirm: String,
    /// 已经试错过一次口令。红字只在**试过之后**出现(同 `unlock.rs`)。
    pub failed: bool,
}

impl PackDialog {
    pub fn export() -> Self {
        Self {
            stage: PackStage::Export,
            pass: String::new(),
            confirm: String::new(),
            failed: false,
        }
    }

    pub fn import(path: String, info: PackInfo) -> Self {
        Self {
            stage: PackStage::Import { path, info },
            pass: String::new(),
            confirm: String::new(),
            failed: false,
        }
    }

    /// 导出这一步:两遍口令都填了、且一致才放行。
    ///
    /// 判据在这里而不是在按钮那一行:下面的提示红字与按钮的可用性必须同源,
    /// 否则会出现「按钮灰着,但一个字的解释都没有」。
    fn export_ready(&self) -> bool {
        !self.pass.is_empty() && self.pass == self.confirm
    }
}

/// 这一帧用户干了什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackOut {
    None,
    /// 口令定了,去开「另存为」对话框。
    Export,
    /// 口令给了,执行导入。
    Import,
    /// 关掉弹窗。
    Close,
}

/// 画弹窗。返回这一帧的结论。
pub fn show(ctx: &egui::Context, t: &Theme, d: &mut PackDialog) -> PackOut {
    let mut out = PackOut::None;
    egui::Window::new(WINDOW_TITLE)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            annotate::mark(ui.ctx(), "配置迁移弹窗", ui.max_rect());
            // 借 `FIELD_W_M` 当窗宽的量纲:不给宽度的话,`egui::Area` 的尺寸
            // 棘轮会让窗口宽度取决于**上一帧**画了哪一态(F259/F263 同一个
            // 根因),从导出切到导入时那几行说明会被压成竖条。
            ui.set_max_width(FIELD_W_M + crate::ui::metrics::LABEL_COL_W + SP_L * 2.0);
            match &d.stage {
                PackStage::Export => export_body(ui, t, d, &mut out),
                PackStage::Import { path, info } => {
                    let (path, info) = (path.clone(), info.clone());
                    import_body(ui, t, d, &path, &info, &mut out);
                }
                PackStage::Done {
                    backup,
                    written,
                    skipped,
                } => {
                    let (backup, written, skipped) = (backup.clone(), *written, *skipped);
                    done_body(ui, t, &backup, written, skipped, &mut out);
                }
            }
        });
    out
}

fn export_body(ui: &mut egui::Ui, t: &Theme, d: &mut PackDialog, out: &mut PackOut) {
    ui.label(theme::hint_text(
        t,
        "把这台机器上的会话、凭据、设置、已知主机和现场历史打成一个文件,\
         拿到新电脑上导入。",
    ));
    ui.add_space(SP_XS);
    // 说清楚口令的作用范围。不说的话,用户会以为这是「主密码」,于是在新机器上
    // 输主密码、解不开,然后来报「包坏了」。
    ui.label(theme::hint_text(
        t,
        "包里的密码和私钥由下面这串口令加密。它只用于这一个文件,与主密码无关,\
         忘了就只能重新导出。",
    ));
    ui.add_space(SP_M);
    let avail = ui.available_width();
    ui.horizontal(|ui| {
        ui.label("口令");
        ui.add_space(SP_S);
        ui.add(
            egui::TextEdit::singleline(&mut d.pass)
                .password(true)
                .desired_width(field_w(avail, FIELD_W_M, 0.0)),
        );
    });
    ui.add_space(SP_XS);
    ui.horizontal(|ui| {
        ui.label("再输一遍");
        ui.add_space(SP_S);
        let resp = ui.add(
            egui::TextEdit::singleline(&mut d.confirm)
                .password(true)
                .desired_width(field_w(avail, FIELD_W_M, 0.0)),
        );
        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && d.export_ready() {
            *out = PackOut::Export;
        }
    });
    // 内联红字紧贴它解释的那个字段(表单规范 #5)。只在第二遍**已经打了字**
    // 之后才说 —— 一边打一边红等于在指责用户还没打完的输入。
    if !d.confirm.is_empty() && d.pass != d.confirm {
        ui.add_space(SP_XS);
        ui.label(
            egui::RichText::new("两次输入不一致")
                .size(11.0)
                .color(theme::c32(t.danger_text)),
        );
    }
    ui.add_space(SP_L);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(d.export_ready(), egui::Button::new("选择保存位置…"))
            .on_disabled_hover_text("先把口令输两遍,且两遍一致")
            .clicked()
        {
            *out = PackOut::Export;
        }
        ui.add_space(SP_S);
        if ui.button("取消").clicked() {
            *out = PackOut::Close;
        }
    });
}

fn import_body(
    ui: &mut egui::Ui,
    t: &Theme,
    d: &mut PackDialog,
    path: &str,
    info: &PackInfo,
    out: &mut PackOut,
) {
    ui.label(theme::hint_text(t, path));
    ui.add_space(SP_XS);
    ui.label(theme::hint_text(t, describe(info)));
    ui.add_space(SP_M);
    // 导入是**整份替换**。这句话必须在按钮之前、用醒目色写出来 —— 用户点
    // 「导入」之前得知道自己现在这份配置会被换掉(备份是有的,但那是事后)。
    ui.label(
        egui::RichText::new("导入会用包里的配置替换这台机器上现有的配置。原有的会先备份一份。")
            .color(theme::c32(t.warn)),
    );
    ui.add_space(SP_M);
    if info.has_secrets {
        let avail = ui.available_width();
        ui.horizontal(|ui| {
            ui.label("口令");
            ui.add_space(SP_S);
            let resp = ui.add(
                egui::TextEdit::singleline(&mut d.pass)
                    .password(true)
                    .desired_width(field_w(avail, FIELD_W_M, 0.0)),
            );
            if resp.lost_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter))
                && !d.pass.is_empty()
            {
                *out = PackOut::Import;
            }
            if ui.ctx().memory(|m| m.focused().is_none()) {
                resp.request_focus();
            }
        });
        if d.failed {
            ui.add_space(SP_XS);
            ui.label(
                egui::RichText::new("口令不对,再试一次")
                    .size(11.0)
                    .color(theme::c32(t.danger_text)),
            );
        }
    } else {
        // 源机一条密码都没存过。摆一个永远也解不开东西的口令框,只会让用户
        // 对着它猜自己当初设了什么。
        ui.label(theme::hint_text(t, "这份包里没有密码或私钥,不需要口令。"));
    }
    ui.add_space(SP_L);
    ui.horizontal(|ui| {
        let can = !info.has_secrets || !d.pass.is_empty();
        if ui
            .add_enabled(can, egui::Button::new("导入并替换"))
            .on_disabled_hover_text("先输入导出这份包时设的口令")
            .clicked()
        {
            *out = PackOut::Import;
        }
        ui.add_space(SP_S);
        if ui.button("取消").clicked() {
            *out = PackOut::Close;
        }
    });
}

fn done_body(
    ui: &mut egui::Ui,
    t: &Theme,
    backup: &str,
    written: usize,
    skipped: usize,
    out: &mut PackOut,
) {
    ui.label(
        egui::RichText::new("已导入,请重启 Mullion")
            .size(15.0)
            .color(theme::c32(t.fg)),
    );
    ui.add_space(SP_XS);
    // 为什么不热重载(设计定案):会话库、设置、已知主机、现场历史四份东西
    // 分别被四处不同的内存状态持有着,其中还有正开着的连接。就地换掉它们
    // 要改的接线远多于这个功能本身,而重启一次是用户在「换新电脑」这个语境
    // 下完全接受的代价。
    ui.label(theme::hint_text(
        t,
        "新的配置要下次启动才会读进来。现在窗口里的连接不受影响。",
    ));
    ui.add_space(SP_M);
    ui.label(theme::hint_text(t, format!("写入 {written} 个文件")));
    // 跳过的条目必须说。「导入成功」但少了几条会话,用户下次开机才发现。
    if skipped > 0 {
        ui.add_space(SP_XS);
        ui.label(
            egui::RichText::new(format!("有 {skipped} 条包里的内容本版本不认,已跳过"))
                .size(11.0)
                .color(theme::c32(t.warn)),
        );
    }
    ui.add_space(SP_XS);
    ui.label(theme::hint_text(t, format!("原有配置已备份到:{backup}")));
    ui.add_space(SP_L);
    if ui.button("知道了").clicked() {
        *out = PackOut::Close;
    }
}

/// 包的一句话简介。纯函数 —— 这几个字段的取舍(哪些说、哪些不说)是这里
/// 唯一容易写错的地方,而它跟窗口无关。
pub fn describe(info: &PackInfo) -> String {
    let mut s = String::new();
    if !info.exported_at.is_empty() {
        s.push_str(&format!("导出于 {}", info.exported_at));
    }
    if !info.app_version.is_empty() {
        if !s.is_empty() {
            s.push_str(" · ");
        }
        s.push_str(&format!("Mullion {}", info.app_version));
    }
    if !s.is_empty() {
        s.push_str(" · ");
    }
    s.push_str(&format!("{} 个文件", info.files));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> PackInfo {
        PackInfo {
            app_version: "0.1.108".into(),
            exported_at: "2026-09-14T00:00:00Z".into(),
            files: 4,
            has_secrets: true,
        }
    }

    /// 跑两帧并收本帧画出来的文字。两帧的理由同 `ui/unlock.rs` 的 `run`。
    fn run(d: &mut PackDialog) -> (Vec<String>, PackOut) {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut out = PackOut::None;
        let mut shapes = Vec::new();
        for _ in 0..2 {
            let full = ctx.run(egui::RawInput::default(), |ctx| {
                out = show(ctx, &t, d);
            });
            shapes = full.shapes;
        }
        (texts_of(&shapes), out)
    }

    fn texts_of(shapes: &[egui::epaint::ClippedShape]) -> Vec<String> {
        fn walk(shape: &egui::Shape, acc: &mut Vec<String>) {
            match shape {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, acc)),
                egui::Shape::Text(ts) => acc.push(ts.galley.text().to_string()),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for cs in shapes {
            walk(&cs.shape, &mut out);
        }
        out
    }

    /// 点一下写着 `label` 的部件,返回那一帧的结论。
    fn click(d: &mut PackDialog, label: &str) -> PackOut {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut shapes = Vec::new();
        for _ in 0..2 {
            let full = ctx.run(egui::RawInput::default(), |ctx| {
                show(ctx, &t, d);
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
            .unwrap_or_else(|| panic!("弹窗里没有写着「{label}」的部件"));
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
        let mut out = PackOut::None;
        let _ = ctx.run(input, |ctx| {
            out = show(ctx, &t, d);
        });
        out
    }

    /// 光画一帧不该产生任何动作 —— 否则弹窗一出现就拿空口令去导出。
    #[test]
    fn merely_showing_the_dialog_changes_nothing() {
        let mut d = PackDialog::export();
        let before = d.clone();
        let (_, out) = run(&mut d);
        assert_eq!(out, PackOut::None);
        assert_eq!(d, before);
    }

    /// 两遍不一致时导不出去。放行的话,用户拿着一个自己以为知道口令的包去新
    /// 电脑上导入,在那边才发现打不开 —— 而那时旧电脑可能已经不在手边了。
    #[test]
    fn a_mismatched_confirmation_cannot_be_exported() {
        let mut d = PackDialog::export();
        d.pass = "hunter2".into();
        d.confirm = "hunter3".into();
        let (texts, _) = run(&mut d);
        assert!(
            texts.iter().any(|s| s.contains("两次输入不一致")),
            "不一致却一个字都不说:{texts:?}"
        );
        assert_eq!(
            click(&mut d, "选择保存位置…"),
            PackOut::None,
            "两遍不一致不该能导出"
        );
    }

    #[test]
    fn an_empty_passphrase_cannot_be_exported() {
        let mut d = PackDialog::export();
        assert_eq!(click(&mut d, "选择保存位置…"), PackOut::None);
    }

    #[test]
    fn two_matching_entries_export() {
        let mut d = PackDialog::export();
        d.pass = "hunter2".into();
        d.confirm = "hunter2".into();
        assert_eq!(click(&mut d, "选择保存位置…"), PackOut::Export);
    }

    /// 红字只在第二遍**已经打了字**之后出现。一边打一边红等于在指责用户还
    /// 没打完的输入。
    #[test]
    fn the_mismatch_line_waits_until_the_second_field_has_something_in_it() {
        let mut d = PackDialog::export();
        d.pass = "hunter2".into();
        let (texts, _) = run(&mut d);
        assert!(
            !texts.iter().any(|s| s.contains("两次输入不一致")),
            "第二遍还没开始打就先红了:{texts:?}"
        );
    }

    /// 口令与主密码是两码事。不说清楚的话,用户会在新机器上输主密码、解不开,
    /// 然后来报「包坏了」。
    #[test]
    fn the_export_page_says_the_passphrase_is_not_the_master_password() {
        let (texts, _) = run(&mut PackDialog::export());
        let all = texts.join(" ");
        assert!(all.contains("与主密码无关"), "没说清口令的作用范围:{all}");
    }

    /// 导入是整份替换。用户点「导入」之前必须知道自己现在这份配置会被换掉 ——
    /// 备份是有的,但那是事后。
    #[test]
    fn the_import_page_warns_before_the_button_not_after() {
        let mut d = PackDialog::import("C:/x.mullionpack".into(), info());
        let (texts, _) = run(&mut d);
        let all = texts.join(" ");
        assert!(all.contains("替换"), "没说会替换现有配置:{all}");
        assert!(all.contains("备份"), "没说会先备份:{all}");
    }

    #[test]
    fn an_empty_passphrase_cannot_be_imported() {
        let mut d = PackDialog::import("p".into(), info());
        assert_eq!(click(&mut d, "导入并替换"), PackOut::None);
        d.pass = "x".into();
        assert_eq!(click(&mut d, "导入并替换"), PackOut::Import);
    }

    /// 源机一条密码都没存过时不摆口令框:摆一个永远也解不开东西的框,只会让
    /// 用户对着它猜自己当初设了什么。
    #[test]
    fn a_pack_without_secrets_does_not_ask_for_a_passphrase() {
        let mut d = PackDialog::import(
            "p".into(),
            PackInfo {
                has_secrets: false,
                ..info()
            },
        );
        let (texts, _) = run(&mut d);
        assert!(
            texts.iter().any(|s| s.contains("不需要口令")),
            "没说明为什么没有口令框:{texts:?}"
        );
        assert_eq!(
            click(&mut d, "导入并替换"),
            PackOut::Import,
            "没有密文时空口令也该能导入"
        );
    }

    #[test]
    fn a_wrong_passphrase_is_said_in_place() {
        let mut d = PackDialog::import("p".into(), info());
        d.failed = true;
        let (texts, _) = run(&mut d);
        assert!(
            texts.iter().any(|s| s.contains("口令不对")),
            "试错了却什么都不说:{texts:?}"
        );
    }

    /// 「请重启」是用户必须看到的一句话。这一态不能被 toast 代替 —— 三秒就没了,
    /// 而用户很可能正好在那三秒里去看别的窗口。
    #[test]
    fn the_done_page_says_to_restart() {
        let mut d = PackDialog::export();
        d.stage = PackStage::Done {
            backup: "C:/cfg/backup-x".into(),
            written: 4,
            skipped: 0,
        };
        let (texts, _) = run(&mut d);
        let all = texts.join(" ");
        assert!(all.contains("请重启"), "导入完了不提重启:{all}");
        assert!(all.contains("C:/cfg/backup-x"), "不告诉用户备份在哪:{all}");
    }

    /// 「导入成功」但少了几条会话,用户下次开机才发现。
    #[test]
    fn skipped_entries_are_reported_on_the_done_page() {
        let mut d = PackDialog::export();
        d.stage = PackStage::Done {
            backup: "b".into(),
            written: 2,
            skipped: 3,
        };
        let (texts, _) = run(&mut d);
        let all = texts.join(" ");
        assert!(all.contains("3 条"), "跳过了 3 条却不说:{all}");

        d.stage = PackStage::Done {
            backup: "b".into(),
            written: 2,
            skipped: 0,
        };
        let (texts, _) = run(&mut d);
        assert!(
            !texts.iter().any(|s| s.contains("已跳过")),
            "一条都没跳过还挂着一行警告:{texts:?}"
        );
    }

    #[test]
    fn the_done_page_closes_on_acknowledge() {
        let mut d = PackDialog::export();
        d.stage = PackStage::Done {
            backup: "b".into(),
            written: 1,
            skipped: 0,
        };
        assert_eq!(click(&mut d, "知道了"), PackOut::Close);
    }

    #[test]
    fn describe_leaves_out_what_it_does_not_know() {
        assert_eq!(
            describe(&info()),
            "导出于 2026-09-14T00:00:00Z · Mullion 0.1.108 · 4 个文件"
        );
        assert_eq!(
            describe(&PackInfo {
                files: 2,
                ..Default::default()
            }),
            "2 个文件",
            "字段是空的就别写「导出于 」这种半截话"
        );
    }
}
