//! F287 ③:全库每一个「筛选输入框」都必须走 `ui::search_box`。
//!
//! **为什么需要机械对账**:构件本身的行为(叉、清空、焦点、内边距)由
//! `ui::search_box::tests` 四条真渲染测试守着,但那四条**管不了「谁在用它」**。
//! 抽构件这件事最典型的失败方式不是构件写错,而是六个调用方只改了五个 ——
//! 剩下那一个静默留在旧样子,编译器不报、测试不报,只有用户切到那个页面才
//! 发现「这里怎么没有叉」。本仓库的记忆里,「列举式门控在加档时必然漏」
//! 已经踩中四次以上,解药一直是同一个:**计数对账**。
//!
//! 这里对两件事对账:
//! 1. 登记表里的六处,每一处都确实在调 `search_box(`;反过来,全库调
//!    `search_box(` 的文件恰好就是这六处(多一处没登记也红 —— 逼你说明它是谁)。
//! 2. **没有人另起炉灶**:`.hint_text(` 的实参里不许出现「搜索 / 筛选 / 过滤 /
//!    模糊匹配」这些字眼。新写搜索框的人最自然的动作就是照着别处抄一个裸
//!    `TextEdit` 再 `.hint_text("搜索…")` —— 那一刻这条会红,并告诉他去用构件。

use std::path::{Path, PathBuf};

mod common;
use common::prod_lines;

/// 全库六个筛选输入框 → (文件, 它筛的是什么)。
///
/// 第二栏写「这个框筛什么」而不是「它在哪」:路径本身已经说了在哪,
/// 而「筛什么」是判断新加的框该不该也进这张表的唯一依据。
const LEDGER: &[(&str, &str)] = &[
    ("ui/launcher.rs", "启动页:按项目名 / 目录 / 节点筛项目列表"),
    (
        "ui/project_manager.rs",
        "项目管理器左栏:按项目名 / 目录 / 节点筛项目列表",
    ),
    ("ui/project_pick.rs", "「这块分屏切到哪个项目」弹窗里的筛选"),
    ("ui/rehost.rs", "「这块分屏换到哪个节点」弹窗里的筛选"),
    (
        "ui/session_manager/list.rs",
        "会话管理器左栏:按名称 / 主机 / 标签筛会话",
    ),
    (
        "ui/files_panel.rs",
        "F278 远端栏递归模糊搜索。id 必须沿用 `find_edit_id`——\
         `Modal::FilesFind` 那条键盘路由拿它当锚",
    ),
];

/// 一看见就说明有人绕开了构件。`files_panel.rs` 那个框的提示语是
/// 「文件名(模糊匹配,回车开始)」,所以「模糊匹配」也得在列里 ——
/// 只收「搜索」两个字的话,照着它抄的人不会被拦下。
const SMELLS: &[&str] = &["搜索", "筛选", "过滤", "模糊匹配"];

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

fn rel(p: &Path) -> String {
    p.strip_prefix(src_dir())
        .expect("路径不在 src 下")
        .to_string_lossy()
        .replace('\\', "/")
}

/// 生产代码里调了 `search_box(` 的文件。
fn callers() -> Vec<String> {
    let mut files = Vec::new();
    rs_files(&src_dir(), &mut files);
    let mut out: Vec<String> = files
        .iter()
        .filter(|p| !p.ends_with("ui/search_box.rs"))
        .filter(|p| {
            let src = std::fs::read_to_string(p).expect("读源码失败");
            prod_lines(&src).iter().any(|l| l.contains("search_box("))
        })
        .map(|p| rel(p))
        .collect();
    out.sort();
    out
}

/// 双向对账:登记表 ↔ 真调用。
///
/// 自证会变红:把 `rehost.rs` 那处调用改回裸 `TextEdit`(第一段红);
/// 或把 LEDGER 里 `ui/launcher.rs` 那条删掉(第二段红)。
#[test]
fn every_filter_box_in_the_tree_goes_through_the_shared_component() {
    let found = callers();

    for (path, what) in LEDGER {
        assert!(
            found.contains(&path.to_string()),
            "LEDGER 说 `{path}` 有一个筛选框({what}),但它没在调 search_box —— \
             要么它被改回裸 TextEdit 了(那个框会没有清空叉),要么这条该删"
        );
    }

    for path in &found {
        assert!(
            LEDGER.iter().any(|(p, _)| p == path),
            "`{path}` 在调 search_box,但 LEDGER 没登记它。新加的筛选框请补一条 —— \
             写明**它筛什么**,那是判断下一个框该不该也进这张表的唯一依据"
        );
    }
}

/// 没有人另起炉灶写第二个搜索框。
///
/// 自证会变红:在任意 UI 文件里加一句
/// `egui::TextEdit::singleline(&mut s).hint_text("搜索点什么")`。
#[test]
fn nobody_hand_rolls_a_second_search_box() {
    let mut files = Vec::new();
    rs_files(&src_dir(), &mut files);

    let mut bad = Vec::new();
    for p in &files {
        let src = std::fs::read_to_string(p).expect("读源码失败");
        for (i, line) in prod_lines(&src).iter().enumerate() {
            let Some(rest) = line.split_once(".hint_text(").map(|(_, r)| r) else {
                continue;
            };
            if let Some(smell) = SMELLS.iter().find(|s| rest.contains(**s)) {
                bad.push(format!("{}:{} 提到「{smell}」", rel(p), i + 1));
            }
        }
    }

    assert!(
        bad.is_empty(),
        "这些地方在用裸 `TextEdit::hint_text` 写筛选框,绕开了 `ui::search_box` —— \
         那样的框没有清空叉,而且右内边距没撑开,长搜索词会和别的东西叠印:\n  {}",
        bad.join("\n  ")
    );
}
