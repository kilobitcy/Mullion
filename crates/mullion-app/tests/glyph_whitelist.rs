//! 守护：UI 字符串里不许出现字体画不出来的符号（F143）。
//!
//! 判据与纪律见 `mullion_app::ui::glyphs` 的模块文档。这里只负责
//! 「把 `src/**/*.rs` 里的字符串字面量捞出来，逐字符过白名单」。
//!
//! **两条必须跳过的东西，少一条这条测试就会假红**：
//!
//! 1. **attribute 里的字符串**。`///` 文档注释在 token 流里就是
//!    `#[doc = "..."]` —— 是货真价实的字符串字面量。不跳过的话，
//!    `ui/icon.rs` 模块头里举反例用的那个字符会当场把这条测试打红，
//!    而那行字根本不会画到屏幕上。本项目记过的
//!    「源码扫描类守护会假红：注释里举反例」，这是第三种形态。
//! 2. **`#[cfg(test)]` 模块**。测试数据里有 emoji、有故意造的畸形字符串，
//!    它们同样不上屏。
//!
//! 为什么不用正则：注释里出现一个引号就会让配对错位，而本项目的注释
//! 又长又密。走 `proc-macro2` 的真词法分析是唯一可靠的做法。

use std::path::{Path, PathBuf};

use proc_macro2::{Delimiter, TokenStream, TokenTree};

/// 一条违规：文件、行号、字符、它所在的那个字面量。
#[derive(Debug)]
struct Offence {
    file: PathBuf,
    line: usize,
    ch: char,
    lit: String,
}

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

/// 把 token 流里的字符串/字符字面量收集出来（行号 + 原文）。
fn collect_literals(ts: TokenStream, out: &mut Vec<(usize, String)>) {
    let mut it = ts.into_iter().peekable();
    // 上一个 attribute 是不是 `#[cfg(test)]` —— 是的话，随后第一个 `{}`
    // 整块跳过（`#[cfg(test)] mod tests { .. }` / `#[cfg(test)] fn .. { .. }`）。
    let mut skip_next_brace = false;
    while let Some(tt) = it.next() {
        match tt {
            // `#[...]` 或 `#![...]` = attribute。整块不进（见模块头第 1 条）。
            //
            // **那个 `!` 不能忘**：`//!` 内部文档注释展开成 `#![doc = "..."]`，
            // `#` 与 `[` 之间隔着一个 Punct。只认 `#[` 的话，全库每个模块头
            // 的正文都会被当成 UI 字符串扫 —— 实测多出 23 处假红，其中就有
            // `icon.rs` 模块头里当反例举的那几个字符。
            TokenTree::Punct(p) if p.as_char() == '#' => {
                if matches!(it.peek(), Some(TokenTree::Punct(b)) if b.as_char() == '!') {
                    it.next();
                }
                let is_attr = matches!(it.peek(), Some(TokenTree::Group(g))
                    if g.delimiter() == Delimiter::Bracket);
                if is_attr {
                    let Some(TokenTree::Group(g)) = it.next() else {
                        unreachable!("上一行刚 peek 确认过是 Bracket group")
                    };
                    // `to_string()` 的空格排布随 proc-macro2 版本变，去掉再比。
                    if g.stream().to_string().replace(' ', "") == "cfg(test)" {
                        skip_next_brace = true;
                    }
                }
            }
            TokenTree::Group(g) => {
                if g.delimiter() == Delimiter::Brace && skip_next_brace {
                    skip_next_brace = false;
                    continue;
                }
                collect_literals(g.stream(), out);
            }
            TokenTree::Literal(l) => {
                let s = l.to_string();
                // 字符串 `"..."` / 原始串 `r"..."`、`r#"..."#` / 字符 `'x'`。
                // 数字字面量和生命周期不会走到这里（生命周期是 Punct+Ident）。
                if s.starts_with('"') || s.starts_with('r') || s.starts_with('\'') {
                    out.push((l.span().start().line, s));
                }
            }
            _ => {}
        }
    }
}

#[test]
fn no_ui_string_contains_a_glyph_the_font_cannot_draw() {
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
        let mut lits = Vec::new();
        collect_literals(ts, &mut lits);
        for (line, lit) in lits {
            for ch in lit.chars() {
                if !mullion_app::ui::glyphs::is_allowed(ch) {
                    offences.push(Offence {
                        file: f.clone(),
                        line,
                        ch,
                        lit: lit.clone(),
                    });
                }
            }
        }
    }

    if !offences.is_empty() {
        let mut msg = format!("{} 处 UI 字符串用了字体画不出来的符号：\n", offences.len());
        for o in &offences {
            let lit = if o.lit.chars().count() > 60 {
                o.lit.chars().take(60).collect::<String>() + "…"
            } else {
                o.lit.clone()
            };
            msg += &format!(
                "  {}:{}  U+{:04X} {:?}  在 {}\n",
                o.file.display(),
                o.line,
                o.ch as u32,
                o.ch,
                lit
            );
        }
        msg += "\n处置二选一：\
                \n  A. 换成 `ui::glyphs::VERIFIED` 里已登记的字符；\
                \n  B. 走 `ui::icon` 自绘（不受字体覆盖面影响）。\
                \n往 VERIFIED 里加新字符**必须先在 Windows 实机上看一眼**。";
        panic!("{msg}");
    }
}
