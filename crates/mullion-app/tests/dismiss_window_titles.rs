//! F239 守护：`egui::Window::new(...)` 不许直接喂裸字符串字面量。
//!
//! 背景见 `app.rs::dismiss_areas` 的文档注释：它靠
//! `egui::Window::new(t)` 的 area id 恒为 `Id::new(t)` 这条事实，
//! 用 `w("关于")` 这类字面串去反算某个弹窗这一帧占的 egui area。
//! 复核实测过：把某个 `Window::new` 的标题字面串改一个字，
//! `cargo test --workspace` 全绿不变红——那个弹窗从此**永久点不外面
//! 关掉**，且没有任何报错。唯一可靠的解法是让 `Window::new` 与
//! `dismiss_areas` 引用**同一个 `pub(crate) const`**，本条测试挡的
//! 就是「有人绕过常量、直接写字面串」这个退化。
//!
//! **判据是 token 级的函数调用形状**：`Ident("Window") "::" Ident("new")
//! Group(Paren)`，且这个括号组的第一个 token 是字符串/原始字符串字面量。
//! 不是简单 grep 文本。
//!
//! **为什么这天然不会被注释假红**（本仓库已知坑：源码切片守护不剥注释，
//! 见 `docs/superpowers/plans/.../source-slice-guards-do-not-strip-comments`
//! 一类记录）：`///`/`//!` 文档注释在 `proc-macro2` 的 token 流里被编译成
//! `#[doc = "整行原文"]`，也就是说 `ui/mod.rs` 里那句
//! “自证会变红：…`Window::new("编辑会话")`…” 的说明文字，在 token 流里
//! 是**一整个字符串字面量**，不会被拆成 `Ident("Window")` `Punct(':')`
//! `Punct(':')` `Ident("new")` `Group(..)` 这一串真实的调用形状。本测试
//! 找的是这串形状本身，所以文档注释里出现同样的字面文本对它完全免疫，
//! 不需要另外按行过滤 `//`。`//` 行注释则根本不会进入 token 流。
//!
//! 白名单只有两类（与 F239 复核意见一致）：
//! - `DISMISS_EXEMPT` 里天生不参与「点外面关」的弹窗：`ui/unlock.rs`
//!   （`需要主密码`）、`ui/editor_window.rs`（`编辑文件`）。
//! - `ui/dismiss.rs` 里判定逻辑自己的单测（`Window::new("Dialog")` 等），
//!   那是测试夹具，不是真弹窗。

use std::path::{Path, PathBuf};

use proc_macro2::{Delimiter, TokenStream, TokenTree};

/// 一条违规。
#[derive(Debug)]
struct Offence {
    file: PathBuf,
    line: usize,
    lit: String,
}

/// 整份文件豁免——不是「这一行」豁免，因为 `ui/dismiss.rs` 里同一个
/// `"Dialog"` 字面串出现了好几次，行号会随测试增删漂移。
const ALLOW_FILES: &[&str] = &["ui/unlock.rs", "ui/editor_window.rs", "ui/dismiss.rs"];

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).expect("读 src 目录失败") {
        let p = e.expect("读目录项失败").path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// 递归扫 token 流，找 `Window::new(<字符串字面量>)` 这个调用形状。
fn scan(ts: TokenStream, file: &Path, out: &mut Vec<Offence>) {
    let toks: Vec<TokenTree> = ts.into_iter().collect();
    for (i, tt) in toks.iter().enumerate() {
        if let TokenTree::Ident(id) = tt {
            if *id == "Window" {
                if let (
                    Some(TokenTree::Punct(p1)),
                    Some(TokenTree::Punct(p2)),
                    Some(TokenTree::Ident(new_id)),
                    Some(TokenTree::Group(g)),
                ) = (toks.get(i + 1), toks.get(i + 2), toks.get(i + 3), toks.get(i + 4))
                {
                    if p1.as_char() == ':'
                        && p2.as_char() == ':'
                        && *new_id == "new"
                        && g.delimiter() == Delimiter::Parenthesis
                    {
                        if let Some(TokenTree::Literal(lit)) = g.stream().into_iter().next() {
                            let s = lit.to_string();
                            if s.starts_with('"') || s.starts_with('r') {
                                out.push(Offence {
                                    file: file.to_path_buf(),
                                    line: id.span().start().line,
                                    lit: s,
                                });
                            }
                        }
                    }
                }
            }
        }
        if let TokenTree::Group(g) = tt {
            scan(g.stream(), file, out);
        }
    }
}

#[test]
fn window_new_never_takes_a_bare_string_literal_outside_the_whitelist() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rs_files(&src, &mut files);
    assert!(
        files.len() > 20,
        "只扫到 {} 个文件，路径多半错了",
        files.len()
    );

    let mut offences: Vec<Offence> = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).expect("读源文件失败");
        let ts: TokenStream = text
            .parse()
            .unwrap_or_else(|e| panic!("{} 解析失败：{e}", f.display()));
        scan(ts, f, &mut offences);
    }

    let bad: Vec<&Offence> = offences
        .iter()
        .filter(|o| {
            let rel = o
                .file
                .strip_prefix(&src)
                .unwrap_or(&o.file)
                .to_string_lossy()
                .replace('\\', "/");
            !ALLOW_FILES.iter().any(|a| rel == *a)
        })
        .collect();

    assert!(
        bad.is_empty(),
        "{} 处 `Window::new` 直接喂了裸字符串字面量，没有走 `pub(crate) const`：\n{}\n\
         把标题提成模块内的 `pub(crate) const WINDOW_TITLE`（照 `session_manager::WINDOW_TITLE` \
         的先例），让 `Window::new` 和 `app.rs::dismiss_areas` 引用同一个常量。",
        bad.len(),
        bad.iter()
            .map(|o| format!("  {}:{}  {}", o.file.display(), o.line, o.lit))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
