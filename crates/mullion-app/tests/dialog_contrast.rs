//! 守护:画在弹窗底色上的文字必须读得到(F213)。
//!
//! `apply_egui` 把 `window_fill` 设成 `modal_bg`(#3f3f3f)。那是一块相当亮的
//! 中灰 —— 在深色面板(#14161f)上完全够用的几个暗色 token,搬到它上面就掉出
//! 4.5:1:
//!
//! | token | on #14161f | on #3f3f3f |
//! |---|---|---|
//! | `fg_dim` #9aa0b8 | 8.05 | **4.06** |
//! | `fg_dimmer` #8a90a8 | 6.53 | **3.33** |
//! | `danger` #e81123 | 3.93 | **2.27** |
//!
//! F203 发现过这件事,但当时的闸门是**列举式**的:一个写死的
//! `[("files_dialog.rs", ..), ("editor_window.rs", ..)]` 数组。于是另外五个
//! 弹窗文件(settings / import_dialog / edit_panel / history / host_key)
//! 一直漏在外面,共 13 处 —— 「列举式门控在加档时必然漏」在本仓库第四次踩中。
//!
//! 这一版把清单**从源码里现算**:扫 `src/ui/**/*.rs`,凡是开了
//! `egui::Window::new` / `egui::Modal::new` 且**没有自己 `.frame(..)` 覆盖
//! 底色**的文件,内容就画在 `modal_bg` 上,该文件里的这三个 token 一律不许用。
//! 新加一个弹窗文件,自动被罩住,不用回来改数组。
//!
//! **逃生门**:同一个文件里既有弹窗、又有画在别的底色上的面板(`edit_panel.rs`
//! 的「编辑中列表」在 `panel_bg` 上,`session_manager/mod.rs` 的主窗自己
//! `.frame(..fill(bar_status))`)。这些行在**同行或上方三行内**写一句带
//! `弹窗外` 的注释放行 —— 逃生门在行上、要写理由,不是在文件上一刀切。

use std::path::{Path, PathBuf};

/// 放行标记。写在同行或上方三行的注释里,后面要接理由。
const ESCAPE: &str = "弹窗外";

/// 在 `modal_bg` 上读不到 4.5:1 的 token(不含 `danger_text`,那个是 F213
/// 专门为此加的,4.84:1)。
const BANNED: [&str; 3] = [".fg_dimmer", ".fg_dim", ".danger"];

fn ui_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/ui")
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).expect("读 src/ui 失败") {
        let p = e.expect("读目录项失败").path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// 只留生产代码。`#[cfg(test)]` 里有大量拿 token 当**断言期望值**的行,
/// 它们不上屏;算进来这条测试从第一天起就是假红。
fn prod(src: &str) -> &str {
    src.split("#[cfg(test)]").next().expect("源码切歪了")
}

/// 去掉行注释。注释里提 `t.fg_dim` 的地方不少(本次改动的说明就是),
/// 不去掉的话它们会被当成真实绘制。
fn strip_comment(line: &str) -> &str {
    match line.find("//") {
        Some(i) => &line[..i],
        None => line,
    }
}

/// 这个文件里有没有「用默认边框」的 `Window`/`Modal`?
///
/// 判据是从 `::new(` 到随后第一个 `.show(` 这一段里有没有 `.frame(`:有,
/// 说明它自己指定了底色(如会话管理器主窗用 `bar_status`),内容不在
/// `modal_bg` 上;没有,走的就是 `window_fill`。
fn hosts_a_default_framed_dialog(prod_src: &str) -> bool {
    for opener in ["egui::Window::new(", "egui::Modal::new("] {
        let mut from = 0;
        while let Some(rel) = prod_src[from..].find(opener) {
            let at = from + rel;
            let end = prod_src[at..]
                .find(".show(")
                .map(|e| at + e)
                .unwrap_or(prod_src.len());
            if !prod_src[at..end].contains(".frame(") {
                return true;
            }
            from = at + opener.len();
        }
    }
    false
}

/// `.fg_dim` 是 `.fg_dimmer` 的前缀,`.danger` 是 `.danger_text` 的前缀 ——
/// 必须看紧跟着的那个字符,否则 `danger_text` 会被自己的闸门拦下来。
fn used_as_whole_token(code: &str, at: usize, tok: &str) -> bool {
    match code[at + tok.len()..].chars().next() {
        Some(c) => !(c.is_alphanumeric() || c == '_'),
        None => true,
    }
}

/// 一条违规。
#[derive(Debug)]
struct Offence {
    file: String,
    line: usize,
    tok: String,
    text: String,
}

/// F213:弹窗正文不许用在 `modal_bg` 上掉出 4.5:1 的三个 token。
///
/// 自证会变红:把 `settings.rs` 里任意一处 `t.fg_muted` 改回 `t.fg_dim`,
/// 或把 `edit_panel.rs` 那句 `弹窗外` 的注释删掉。
#[test]
fn nothing_drawn_on_the_dialog_fill_uses_a_token_that_fails_aa_on_it() {
    let mut files = Vec::new();
    rs_files(&ui_dir(), &mut files);
    files.sort();
    assert!(files.len() > 10, "只扫到 {} 个文件,目录走歪了", files.len());

    let mut dialogs = 0usize;
    let mut offences: Vec<Offence> = Vec::new();

    for path in &files {
        let src = std::fs::read_to_string(path).expect("读源码失败");
        let prod_src = prod(&src);
        // 判宿主也要去注释:`session_manager/editor.rs` 的注释里两次提到
        // `egui::Window::new(title)`(在解释窗口 id 的来历),不去掉的话它会被
        // 当成弹窗宿主 —— 而它其实画在会话管理器主窗自定的 `bar_status` 上。
        let code_src: String = prod_src
            .lines()
            .map(strip_comment)
            .collect::<Vec<_>>()
            .join("\n");
        if !hosts_a_default_framed_dialog(&code_src) {
            continue;
        }
        dialogs += 1;
        let name = path
            .strip_prefix(ui_dir().parent().unwrap().parent().unwrap())
            .unwrap_or(path)
            .display()
            .to_string();
        let lines: Vec<&str> = prod_src.lines().collect();
        for (i, raw) in lines.iter().enumerate() {
            // 逃生门看**原始**行(注释还在),违规看去注释后的代码。
            let excused = lines[i.saturating_sub(3)..=i].iter().any(|l| {
                let c = l.trim_start();
                (c.starts_with("//") || l.contains("// ")) && l.contains(ESCAPE)
            });
            if excused {
                continue;
            }
            let code = strip_comment(raw);
            for tok in BANNED {
                let mut from = 0;
                while let Some(rel) = code[from..].find(tok) {
                    let at = from + rel;
                    if used_as_whole_token(code, at, tok) {
                        offences.push(Offence {
                            file: name.clone(),
                            line: i + 1,
                            tok: tok.to_string(),
                            text: raw.trim().to_string(),
                        });
                        break;
                    }
                    from = at + tok.len();
                }
            }
        }
    }

    assert!(
        dialogs >= 10,
        "只认出 {dialogs} 个弹窗宿主文件 —— `hosts_a_default_framed_dialog` \
         的判据多半跟不上 egui 的写法变化了,这条测试正在悄悄空转"
    );
    if !offences.is_empty() {
        let mut msg = format!(
            "{} 处文字画在 modal_bg(#3f3f3f)上,却用了在它上面读不到 4.5:1 的 token:\n",
            offences.len()
        );
        for o in &offences {
            msg += &format!("  {}:{}  {}  在 {}\n", o.file, o.line, o.tok, o.text);
        }
        msg += &format!(
            "\n处置三选一:\
             \n  A. 次要文字改 `fg_muted`(4.77:1);\
             \n  B. 危险语义的文字改 `danger_text`(4.84:1);\
             \n  C. 这一行其实画在别的底色上 —— 在同行或上方三行内写一句带\
             「{ESCAPE}」的注释,说明它画在哪儿。"
        );
        panic!("{msg}");
    }
}
