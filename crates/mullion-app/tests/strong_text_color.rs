//! 守护:全库不许用 egui 的 `.strong()`(F260)。
//!
//! # 为什么是「禁止」而不是「小心用」
//!
//! `RichText::strong()` 读起来像「加粗」,实际做的是**换颜色**:
//!
//! ```text
//! widget_text.rs::get_text_color  ->  visuals.strong_text_color()
//! style.rs::strong_text_color     ->  self.widgets.active.text_color()
//! theme.rs::apply_egui            ->  widgets.active.fg_stroke = accent_fg
//! ```
//!
//! 而 `accent_fg` 是 #0d0f16 —— 一个**专门给亮色 accent 底做反白**的近黑色。
//! 本项目所有底色都是深的,于是 `.strong()` 一律画成黑字:落在弹窗底
//! #3f3f3f 上是 **1.82:1**(正文门槛 4.5:1),用户实报「快捷键那一列看不清」。
//!
//! 那能不能改 `widgets.active.fg_stroke` 一劳永逸?不能 —— 它同时是按下态
//! 按钮的文字色,那块底**就是** accent,改了等于把另一处反过来弄瞎。egui
//! 没有独立的 strong 色字段,所以唯一的出路是在调用点显式给色。
//!
//! # 判据
//!
//! 扫 `src/**/*.rs` 的**生产代码**(切掉 `#[cfg(test)]`、去掉注释),凡出现
//! `.strong()` 一律违规。规则刻意做成无条件的:写成「给了 `.color(..)` 就
//! 放行」的话,留在那儿的 `.strong()` 其实是死代码(显式色排在 strong 前面),
//! 而一条会放行死代码的规则,下一个人照着抄的时候又会漏掉那句 `.color`。
//!
//! **逃生门**:真要在 accent 底上要这个反白色时,在同行或上方三行的注释里
//! 写一句带 `strong 例外` 的理由。逃生门在行上、要写理由,不在文件上一刀切
//! (同 `dialog_contrast.rs` 的姿态)。

use std::path::{Path, PathBuf};

/// 放行标记。写在同行或上方三行的注释里,后面要接理由。
const ESCAPE: &str = "strong 例外";

/// 被禁的写法。带括号:`.strong()` 是方法调用,而 `strong` 作为普通标识符
/// (比如某个字段名 `fg_strong`)满地都是。
const BANNED: &str = ".strong()";

fn src_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).expect("读 src 失败") {
        let p = e.expect("读目录项失败").path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// 只留生产代码。测试里拿 `.strong()` 当**断言期望值**或造反例是合法的,
/// 算进来这条测试从第一天起就是假红(同 `dialog_contrast.rs::prod`)。
fn prod(src: &str) -> &str {
    src.split("#[cfg(test)]").next().expect("源码切歪了")
}

/// 去掉行注释。**这一条少不得**:本仓库记过「源码切片守护不剥注释」这个坑
/// —— 上面这段模块文档、以及 `settings.rs` 里解释这次改动的那段注释,都原样
/// 写着 `.strong()`,不剥的话它们会被当成真实绘制,守护当场假红。
fn strip_comment(line: &str) -> &str {
    match line.find("//") {
        Some(i) => &line[..i],
        None => line,
    }
}

/// 一条违规。
#[derive(Debug)]
struct Offence {
    file: String,
    line: usize,
    text: String,
}

/// F260:`.strong()` 在本项目的主题下等于「画成近黑色」,一律不许用。
///
/// 自证会变红:把 `settings.rs` 里快捷键那一列改回
/// `egui::RichText::new(s.chord).strong()`。
#[test]
fn no_production_code_leaves_a_text_color_to_egui_strong() {
    let root = src_dir();
    let mut files = Vec::new();
    rs_files(&root, &mut files);
    files.sort();
    assert!(
        files.len() > 20,
        "只扫到 {} 个文件,路径多半错了",
        files.len()
    );

    let mut offences: Vec<Offence> = Vec::new();
    for path in &files {
        let src = std::fs::read_to_string(path).expect("读源码失败");
        let name = path
            .strip_prefix(root.parent().and_then(Path::parent).unwrap_or(&root))
            .unwrap_or(path)
            .display()
            .to_string();
        let lines: Vec<&str> = prod(&src).lines().collect();
        for (i, raw) in lines.iter().enumerate() {
            // 逃生门看**原始**行(注释还在),违规看去注释后的代码。
            let excused = lines[i.saturating_sub(3)..=i].iter().any(|l| {
                let c = l.trim_start();
                (c.starts_with("//") || l.contains("// ")) && l.contains(ESCAPE)
            });
            if excused {
                continue;
            }
            if strip_comment(raw).contains(BANNED) {
                offences.push(Offence {
                    file: name.clone(),
                    line: i + 1,
                    text: raw.trim().to_string(),
                });
            }
        }
    }

    if !offences.is_empty() {
        let mut msg = format!(
            "{} 处用了 `.strong()` —— 在本项目的 Visuals 下它等于把文字画成 \
             accent_fg(#0d0f16),深色底上读不出来:\n",
            offences.len()
        );
        for o in &offences {
            msg += &format!("  {}:{}  {}\n", o.file, o.line, o.text);
        }
        msg += "\n改法:显式 `.color(theme::c32(t.fg))`(要醒目)或 `t.fg_muted`\
                (次要信息),再把 `.strong()` 删掉 —— 给了色之后它一点效果都没有。\
                \n真要那个反白色(文字画在 accent 底上),在上方三行内写一句带\
                「strong 例外」的注释说明理由。";
        panic!("{msg}");
    }
}
