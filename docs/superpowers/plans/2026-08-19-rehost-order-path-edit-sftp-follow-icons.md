# 换节点排序 + 路径可编辑 + SFTP 跟随分屏 + 文件图标 —— 实现 plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 落地 F130~F134 五条 UI 优化——换节点弹窗顺序对齐左栏、文件面板路径条可编辑、`Ctrl+Shift+B` 按焦点分屏的节点开 SFTP、office 类文件四种图标、默认文件图标从「其他」里拆出来。

**Architecture:** 五条落在三簇代码上,互不依赖:`ui/rehost.rs`(顺序借用左栏的 `visible_order`)、`ui/files_panel.rs` + `files/state.rs` + `app.rs`(路径条编辑态 + 新的 `FileAction::GotoInput`,编辑态登记进 `Modal` 才能让 `TextEdit` 收到键——面板焦点下键盘根本不喂 egui)、`app.rs` 的 sftp 生命周期(`TerminalTab::sftp_host_ix` 记住这条 channel 属于哪台机器,焦点分屏换了台就重开)、`ui/file_icon.rs` + `theme.rs`(五个新 `IconKind`)。

**Tech Stack:** Rust 2021 / egui 0.30 / winit 0.30 / russh 0.54 / tokio。测试全部是 `cargo test --workspace` 里的单测,无新依赖。

**设计文档:** `docs/superpowers/specs/2026-08-19-rehost-order-path-edit-sftp-follow-icons-design.md`

**对设计文档的一处修正(实现期发现):** spec ② 里写「相对路径不在客户端拼接,交给远端解析」。实际不可行——sftp 的相对路径基准是**登录目录**,不是面板当前目录,用户在 `/var/log` 里敲 `nginx` 会跳到 `~/nginx`。改为:相对路径拼在当前 `cwd` 后面,`.` / `..` 用纯字符串规整(与既有 `RemotePath::parent()` 同一套字符串语义,不解析符号链接)。Task 6 的 `normalize_posix` 就是这件事。

---

## 文件结构

| 文件 | 责任 | 本次改动 |
|---|---|---|
| `crates/mullion-app/src/ui/rehost.rs` | 换节点弹窗 | `visible` 改走 `list::visible_order`;`show` 加 `groups` 参数 |
| `crates/mullion-app/src/ui/mod.rs` | UI 装配 | `rehost::show` 调用点传 `frame.groups` |
| `crates/mullion-app/src/theme.rs` | 色板 | 加 `icon_pdf/word/excel/slides/file` 五色 + 更新两条颜色守护 |
| `crates/mullion-app/src/ui/file_icon.rs` | 文件类型图标 | `IconKind` 加五个变体、`EXT_TABLE`、`outline`、`color_for`、`ALL` 挪进生产码 |
| `crates/mullion-app/src/files/state.rs` | 面板每栏运行态 | 加 `path_edit: Option<String>` |
| `crates/mullion-app/src/files/path_input.rs` | **新建**:路径输入解析(纯函数) | `resolve_remote_input` / `resolve_local_input` / `normalize_posix` |
| `crates/mullion-app/src/files/mod.rs` | 模块声明 | `pub mod path_input;` |
| `crates/mullion-app/src/ui/files_panel.rs` | 面板渲染 | 路径条只读↔编辑两态;`FileAction::GotoInput` |
| `crates/mullion-app/src/app.rs` | 事件循环 / sftp 生命周期 / 模态表 | `Modal::FilesPathEdit`;两个 apply 分支接 `GotoInput`;`sftp_host_ix` 全套;`sync_plan_of` |

---

## Task 1: F130 换节点弹窗顺序对齐左栏

**Files:**
- Modify: `crates/mullion-app/src/ui/rehost.rs`(`visible` / `show` / 既有测试的调用)
- Modify: `crates/mullion-app/src/ui/mod.rs:781`(`rehost::show` 调用点)
- Modify: `crates/mullion-app/src/ui/session_manager/list.rs`(若 `visible_order` 可见性不足)

- [ ] **Step 1: 写会失败的测试**

加到 `crates/mullion-app/src/ui/rehost.rs` 的 `mod tests` 里。注意 fixture **必须让数组顺序与分组顺序真的不同**,否则任何实现都绿:

```rust
    /// F130:弹窗的行顺序必须跟会话管理器左栏一模一样。两边各排一套的话,
    /// 左栏里挨着的两条在这儿可能隔着半屏 —— 而用户是照着左栏的记忆找的。
    ///
    /// 顺序的唯一真值来源是 `list::visible_order`(左栏渲染与键盘导航也用它)。
    ///
    /// fixture 刻意让**数组顺序与分组顺序相反**:`sessions` 里 7、8、9 依次是
    /// 未分组、组 2、组 1,而 `groups` 是 [组 1, 组 2]。照数组顺序出是
    /// [7,8,9],照分组出是 [9,8,7] —— 不这么造的话这条断言在旧实现下也是绿的。
    ///
    /// 自证会变红:把 `visible` 改回 `sessions.iter().filter(..)`。
    #[test]
    fn rows_are_ordered_exactly_like_the_session_manager_list() {
        use mullion_store::{GroupId, GroupRecord};

        let groups = vec![
            GroupRecord {
                id: GroupId(1),
                name: "一组".into(),
            },
            GroupRecord {
                id: GroupId(2),
                name: "二组".into(),
            },
        ];
        let mut a = rec(7, "未分组", "10.0.0.7", Protocol::Ssh);
        let mut b = rec(8, "二组的", "10.0.0.8", Protocol::Ssh);
        let mut c = rec(9, "一组的", "10.0.0.9", Protocol::Ssh);
        a.identity.group_id = None;
        b.identity.group_id = Some(GroupId(2));
        c.identity.group_id = Some(GroupId(1));
        let sessions = vec![a, b, c];

        let got: Vec<SessionId> = visible(&sessions, &groups, "")
            .iter()
            .map(|r| r.id)
            .collect();
        let want = crate::ui::session_manager::list::visible_order(
            &sessions,
            &groups,
            "",
            Protocol::Ssh,
        );
        assert_eq!(got, want, "换节点弹窗的顺序跟左栏对不上");
        assert_eq!(
            got,
            vec![SessionId(9), SessionId(8), SessionId(7)],
            "分组顺序没生效 —— fixture 或实现有一处没按分组排"
        );
    }
```

`GroupRecord` 的字段若与上面不符,以 `mullion_store` 里的定义为准(用
`grep -n "pub struct GroupRecord" -A 10 crates/mullion-store/src/*.rs` 查),
补齐缺的字段即可。

- [ ] **Step 2: 跑测试确认它失败**

```bash
cargo test -p mullion-app rows_are_ordered_exactly_like 2>&1 | tail -20
```

预期:编译失败——`visible` 只接两个参数。

- [ ] **Step 3: 改 `visible` 与 `show`**

`rehost.rs` 顶部 use 加 `mullion_store::GroupRecord`,替换 `visible`:

```rust
/// 这一帧该列出哪些会话。**顺序来自会话管理器左栏的 `visible_order`**——
/// 两边各排一套的话,左栏里挨着的两条在这儿会隔着半屏,而用户是照着左栏的
/// 记忆找的(F130)。搜索词传空:借它的**顺序**,不借它的过滤规则(左栏还搜
/// 标签,这里只搜名字和地址)。
///
/// `Protocol::Ssh` 同时替掉了原先那道协议过滤:SFTP 节点没有 PTY,换过去
/// 只有一块永远不出字的黑屏。
fn visible<'a>(
    sessions: &'a [SessionRecord],
    groups: &[GroupRecord],
    needle: &str,
) -> Vec<&'a SessionRecord> {
    crate::ui::session_manager::list::visible_order(sessions, groups, "", Protocol::Ssh)
        .into_iter()
        .filter_map(|id| sessions.iter().find(|r| r.id == id))
        .filter(|r| matches(r, needle))
        .collect()
}
```

`show` 的签名在 `sessions` 之后加一个参数,并把调用改掉:

```rust
pub fn show(
    ctx: &egui::Context,
    t: &Theme,
    draft: &mut Option<RehostDraft>,
    sessions: &[SessionRecord],
    groups: &[GroupRecord],
    appearance: &crate::ui::badge::AppearanceCache,
    pane_rect: Option<egui::Rect>,
) -> Option<RehostAction> {
```

函数体里 `let rows = visible(sessions, &d.filter);` 改成
`let rows = visible(sessions, groups, &d.filter);`。

- [ ] **Step 4: 修调用点与既有测试**

`crates/mullion-app/src/ui/mod.rs` 的 `rehost::show(...)` 调用里,在
`frame.sessions,` 之后插入 `frame.groups,`。

`rehost.rs` 里既有测试的三处 `visible(&sessions, "")` / `visible(&sessions, "生产")`
等,统一加中间参数 `&[]`(那几条测的是过滤规则,不需要分组)。测试辅助
`click` 里的 `show(ctx, &MULLION_DARK, draft, sessions, &cache, Some(pane()))`
两处也要补 `&[]`。

若编译报 `visible_order` 或 `list` 模块不可见,把
`crates/mullion-app/src/ui/session_manager/list.rs` 里
`pub(crate) fn visible_order` 保持不变即可(`list` 已经是 `pub(crate) mod`);
真不通就把它改成 `pub(crate)`,**不要**在 rehost 里另写一套排序。

- [ ] **Step 5: 跑测试**

```bash
cargo test -p mullion-app rehost 2>&1 | grep -E "test result|FAILED|panicked"
```

预期:`test result: ok`,含新加的那条与既有四条。

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-app/src/ui/rehost.rs crates/mullion-app/src/ui/mod.rs
git commit -m "feat(ui): 换节点弹窗的顺序对齐会话管理器左栏 (F130)"
```

---

## Task 2: F133/F134 色板加五色

**Files:**
- Modify: `crates/mullion-app/src/theme.rs`(`Theme` 结构、`MULLION_DARK`、两条颜色守护测试)

- [ ] **Step 1: 写会失败的测试**

改 `theme.rs` 里既有的两条测试,把新色加进两张表(它们是列举式的,加档必须
同时改——这正是本项目踩过三次的那类漏改,所以宁可让它编译失败也不加
`_ => {}` 兜底):

`file_icon_colors_are_visible_on_the_panel` 的数组里补:

```rust
            ("pdf", t.icon_pdf),
            ("word", t.icon_word),
            ("excel", t.icon_excel),
            ("slides", t.icon_slides),
            ("file", t.icon_file),
```

`file_icon_colors_are_all_distinct` 的数组里补:

```rust
            t.icon_pdf,
            t.icon_word,
            t.icon_excel,
            t.icon_slides,
            t.icon_file,
```

- [ ] **Step 2: 跑测试确认它失败**

```bash
cargo test -p mullion-app file_icon_colors 2>&1 | tail -20
```

预期:编译失败——`Theme` 上没有 `icon_pdf` 等字段。

- [ ] **Step 3: 加字段与取值**

`Theme` 结构里 `icon_other` 附近加(注释照既有字段的风格,一行说清是给谁的):

```rust
    /// F133:PDF。红,对齐 Windows 上的既有心智。
    pub icon_pdf: Rgb,
    /// F133:Word 文档(doc/docx)。蓝。
    pub icon_word: Rgb,
    /// F133:表格(xls/xlsx/csv)。绿。
    pub icon_excel: Rgb,
    /// F133:演示文稿(ppt/pptx)。橙。
    pub icon_slides: Rgb,
    /// F134:普通文件的默认图标色。中性,比 `icon_other`(设备/socket)亮 ——
    /// 「不认识的普通文件」是绝大多数,不该跟极少数特殊类型抢注意力。
    pub icon_file: Rgb,
```

`MULLION_DARK` 里补(取值已按 3:1 对比度与两两不同挑过,底色 `panel_bg`):

```rust
    icon_pdf: Rgb::new(0xe2, 0x6d, 0x6d),
    icon_word: Rgb::new(0x5b, 0x9c, 0xf0),
    icon_excel: Rgb::new(0x4f, 0xb3, 0x72),
    icon_slides: Rgb::new(0xe8, 0x8b, 0x3d),
    icon_file: Rgb::new(0xc2, 0xc8, 0xd8),
```

若还有别的 `Theme` 常量(浅色主题等),同样补齐——编译器会逐个指出来。

- [ ] **Step 4: 跑测试**

```bash
cargo test -p mullion-app file_icon_colors 2>&1 | grep -E "test result|FAILED|panicked"
```

预期:两条都 ok。若某条对比度不足 3:1,把该色**调亮**再跑,不要改阈值。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/theme.rs
git commit -m "feat(ui): 色板加 PDF/Word/Excel/PPT/普通文件五色 (F133/F134)"
```

---

## Task 3: F133 office 四类图标 + F134 默认文件图标

**Files:**
- Modify: `crates/mullion-app/src/ui/file_icon.rs`

- [ ] **Step 1: 写会失败的测试**

`file_icon.rs` 的 `mod tests` 里:把开头的 `const ALL_KINDS: [IconKind; 8] = [...]`
**整块删掉**,两处 `for kind in ALL_KINDS` / `ALL_KINDS.iter()` 改成用生产码里的
`IconKind::ALL`(下一步会加):

```rust
        for &kind in IconKind::ALL {
```
```rust
        let shapes: Vec<(IconKind, String)> = IconKind::ALL
            .iter()
            .map(|&k| (k, format!("{:?}", outline(cell, k))))
            .collect();
```

改 `classify_maps_extensions_to_kinds` 的表(office 四类 + 默认文件):

```rust
        for (name, want) in [
            ("a.zip", IconKind::Archive),
            ("a.TAR.GZ", IconKind::Archive),
            ("a.tgz", IconKind::Archive),
            ("photo.png", IconKind::Image),
            ("photo.JPEG", IconKind::Image),
            ("main.rs", IconKind::Code),
            ("build.sh", IconKind::Code),
            ("Cargo.toml", IconKind::Code),
            ("README.md", IconKind::Doc),
            ("app.log", IconKind::Doc),
            ("setup.exe", IconKind::Exec),
            ("report.pdf", IconKind::Pdf),
            ("合同.DOCX", IconKind::Word),
            ("notes.doc", IconKind::Word),
            ("budget.xlsx", IconKind::Excel),
            ("data.csv", IconKind::Excel),
            ("deck.pptx", IconKind::Slides),
            ("deck.ppt", IconKind::Slides),
            ("data.bin", IconKind::File),
            ("Makefile", IconKind::File),
        ] {
```

改 `dotfiles_have_no_extension` 里两处期望(`.bashrc` / `.gz` 现在是普通文件):

```rust
        assert_eq!(classify(EntryKind::File, ".bashrc", 0o644), IconKind::File);
```
```rust
        assert_eq!(
            classify(EntryKind::File, ".gz", 0o644),
            IconKind::File,
            "点号在开头时,哪怕「扩展名」凑巧撞上表项也不能当真"
        );
```

`entry_kind_wins_over_extension_for_dirs_and_links` 里 `EntryKind::Other` 那条
**保持 `IconKind::Other` 不变**——F134 拆的正是这两者的区别,这条是它的对照组。

新增两条:

```rust
    /// F134:「不认识的普通文件」和「设备/socket 这类特殊类型」必须是两种
    /// 图标。合成一种的话,一屏陌生扩展名的普通文件全成了菱形,而真正需要
    /// 「这不是普通文件」提示的那几条被淹掉了。
    ///
    /// 自证会变红:把 `classify` 末尾的兜底从 `IconKind::File` 改回
    /// `IconKind::Other`。
    #[test]
    fn an_unknown_regular_file_is_not_the_same_as_a_device_node() {
        assert_eq!(classify(EntryKind::File, "data.bin", 0o644), IconKind::File);
        assert_eq!(classify(EntryKind::Other, "ttyS0", 0o666), IconKind::Other);
        assert_ne!(
            outline(
                egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(16.0, 16.0)),
                IconKind::File
            ),
            outline(
                egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(16.0, 16.0)),
                IconKind::Other
            ),
            "两者共用了同一支形状"
        );
    }

    /// 加了新类型却忘了写进 `IconKind::ALL` 的话,「两两不同」「不越格」两条
    /// 守护会**悄悄漏掉**新类型 —— 本项目已经踩过三次「列举式门控在加档时
    /// 必然漏」。这条把 `EXT_TABLE` 当交叉验证:表里出现过的每个类型都必须
    /// 在 `ALL` 里。
    ///
    /// 自证会变红:往 `EXT_TABLE` 加一个 `ALL` 里没有的类型。
    #[test]
    fn every_kind_used_by_the_extension_table_is_listed_in_all() {
        for (ext, kind) in EXT_TABLE {
            assert!(
                IconKind::ALL.contains(kind),
                "{kind:?}(来自扩展名 {ext})不在 IconKind::ALL 里"
            );
        }
    }
```

- [ ] **Step 2: 跑测试确认它失败**

```bash
cargo test -p mullion-app file_icon 2>&1 | tail -20
```

预期:编译失败——`IconKind::ALL` / `IconKind::Pdf` 等不存在。

- [ ] **Step 3: 改生产码**

`IconKind` 枚举加五个变体,并把 `ALL` 放进生产码:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconKind {
    Dir,
    Archive,
    Image,
    Code,
    Doc,
    /// F133:PDF。
    Pdf,
    /// F133:Word 文档。
    Word,
    /// F133:表格(含 csv —— 双击它多半是想到表格里看)。
    Excel,
    /// F133:演示文稿。
    Slides,
    Exec,
    Link,
    /// F134:普通文件的兜底(扩展名不认识、也没有可执行位)。
    File,
    /// F134:`EntryKind::Other` 专用 —— 设备文件 / socket / 命名管道。
    /// **不含**「不认识的普通文件」,那是 `File`。
    Other,
}

impl IconKind {
    /// 全部类型。**加变体必须同时加进这里** —— 「两两长得不一样」「不越格」
    /// 两条守护都照它遍历,漏加等于新类型不受任何守护。
    /// `every_kind_used_by_the_extension_table_is_listed_in_all` 会逮住漏加。
    pub const ALL: &'static [IconKind] = &[
        IconKind::Dir,
        IconKind::Archive,
        IconKind::Image,
        IconKind::Code,
        IconKind::Doc,
        IconKind::Pdf,
        IconKind::Word,
        IconKind::Excel,
        IconKind::Slides,
        IconKind::Exec,
        IconKind::Link,
        IconKind::File,
        IconKind::Other,
    ];
}
```

`EXT_TABLE`:把 `("pdf", IconKind::Doc)`、`("doc", ..)`、`("docx", ..)` 三行删掉,
`("csv", IconKind::Doc)` 也删掉,在 `("csv"…)` 原位置附近补:

```rust
    ("pdf", IconKind::Pdf),
    ("doc", IconKind::Word),
    ("docx", IconKind::Word),
    ("xls", IconKind::Excel),
    ("xlsx", IconKind::Excel),
    ("csv", IconKind::Excel),
    ("ppt", IconKind::Slides),
    ("pptx", IconKind::Slides),
```

`classify` 末尾兜底改掉:

```rust
    if mode & 0o111 != 0 {
        return IconKind::Exec;
    }
    // F134:不认识的**普通文件**用普通文件的图标。`Other` 只留给
    // `EntryKind::Other`(设备/socket/命名管道)——上面已经 return 过了。
    IconKind::File
```

`outline` 的 `match` 加五支。四种 office 都以「页」为底 + 各自标记,靠标记区分:

```rust
        // PDF:页 + 底部一条实心横条(粗到一眼能认出是色块而不是文字线)。
        IconKind::Pdf => {
            let (pl, pr) = (l + r.width() * 0.15, rt - r.width() * 0.15);
            vec![
                vec![
                    egui::pos2(pl, t),
                    egui::pos2(pr, t),
                    egui::pos2(pr, b),
                    egui::pos2(pl, b),
                    egui::pos2(pl, t),
                ],
                vec![
                    egui::pos2(pl, b - r.height() * 0.3),
                    egui::pos2(pr, b - r.height() * 0.3),
                    egui::pos2(pr, b - r.height() * 0.12),
                    egui::pos2(pl, b - r.height() * 0.12),
                    egui::pos2(pl, b - r.height() * 0.3),
                ],
            ]
        }
        // Word:页 + 一个折线 W。
        IconKind::Word => {
            let (pl, pr) = (l + r.width() * 0.15, rt - r.width() * 0.15);
            let (wt, wb) = (t + r.height() * 0.35, b - r.height() * 0.2);
            vec![
                vec![
                    egui::pos2(pl, t),
                    egui::pos2(pr, t),
                    egui::pos2(pr, b),
                    egui::pos2(pl, b),
                    egui::pos2(pl, t),
                ],
                vec![
                    egui::pos2(pl + r.width() * 0.08, wt),
                    egui::pos2(pl + r.width() * 0.22, wb),
                    egui::pos2(r.center().x, wt + r.height() * 0.18),
                    egui::pos2(pr - r.width() * 0.22, wb),
                    egui::pos2(pr - r.width() * 0.08, wt),
                ],
            ]
        }
        // Excel:页 + 2×2 网格。
        IconKind::Excel => {
            let (pl, pr) = (l + r.width() * 0.15, rt - r.width() * 0.15);
            let (gt, gb) = (t + r.height() * 0.35, b - r.height() * 0.15);
            let (gl, gr) = (pl + r.width() * 0.08, pr - r.width() * 0.08);
            vec![
                vec![
                    egui::pos2(pl, t),
                    egui::pos2(pr, t),
                    egui::pos2(pr, b),
                    egui::pos2(pl, b),
                    egui::pos2(pl, t),
                ],
                vec![
                    egui::pos2(gl, gt),
                    egui::pos2(gr, gt),
                    egui::pos2(gr, gb),
                    egui::pos2(gl, gb),
                    egui::pos2(gl, gt),
                ],
                vec![
                    egui::pos2(gl, (gt + gb) * 0.5),
                    egui::pos2(gr, (gt + gb) * 0.5),
                ],
                vec![
                    egui::pos2((gl + gr) * 0.5, gt),
                    egui::pos2((gl + gr) * 0.5, gb),
                ],
            ]
        }
        // 演示文稿:页 + 一块横向「屏幕」条(比 Excel 的网格宽而扁)。
        IconKind::Slides => {
            let (pl, pr) = (l + r.width() * 0.15, rt - r.width() * 0.15);
            vec![
                vec![
                    egui::pos2(pl, t),
                    egui::pos2(pr, t),
                    egui::pos2(pr, b),
                    egui::pos2(pl, b),
                    egui::pos2(pl, t),
                ],
                vec![
                    egui::pos2(pl + r.width() * 0.06, t + r.height() * 0.38),
                    egui::pos2(pr - r.width() * 0.06, t + r.height() * 0.38),
                    egui::pos2(pr - r.width() * 0.06, b - r.height() * 0.28),
                    egui::pos2(pl + r.width() * 0.06, b - r.height() * 0.28),
                    egui::pos2(pl + r.width() * 0.06, t + r.height() * 0.38),
                ],
            ]
        }
        // F134:普通文件 —— 折角空白页。跟 `Doc`(页 + 三条横线)、
        // `Link`(折角页 + 箭头)靠「有没有横线 / 有没有箭头」区分。
        IconKind::File => {
            let fold = r.width() * 0.3;
            vec![
                vec![
                    egui::pos2(l + r.width() * 0.1, t),
                    egui::pos2(rt - r.width() * 0.1 - fold, t),
                    egui::pos2(rt - r.width() * 0.1, t + fold),
                    egui::pos2(rt - r.width() * 0.1, b),
                    egui::pos2(l + r.width() * 0.1, b),
                    egui::pos2(l + r.width() * 0.1, t),
                ],
                vec![
                    egui::pos2(rt - r.width() * 0.1 - fold, t),
                    egui::pos2(rt - r.width() * 0.1 - fold, t + fold),
                    egui::pos2(rt - r.width() * 0.1, t + fold),
                ],
            ]
        }
```

`color_for` 的 `match` 加五支:

```rust
        IconKind::Pdf => t.icon_pdf,
        IconKind::Word => t.icon_word,
        IconKind::Excel => t.icon_excel,
        IconKind::Slides => t.icon_slides,
        IconKind::File => t.icon_file,
```

- [ ] **Step 4: 跑测试**

```bash
cargo test -p mullion-app file_icon 2>&1 | grep -E "test result|FAILED|panicked"
```

预期:全 ok。`every_icon_stays_inside_its_cell` 若报某个顶点越界,调该形状的
系数(不要动 `rect.shrink(2.0)`——那是这条测试守的东西)。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/ui/file_icon.rs
git commit -m "feat(ui): office 四类图标 + 普通文件默认图标从「其他」拆出 (F133/F134)"
```

---

## Task 4: F131 路径输入解析(纯函数,新文件)

**Files:**
- Create: `crates/mullion-app/src/files/path_input.rs`
- Modify: `crates/mullion-app/src/files/mod.rs`(加 `pub mod path_input;`)

- [ ] **Step 1: 建文件,先写测试**

新建 `crates/mullion-app/src/files/path_input.rs`,先只写模块头 + 测试:

```rust
//! F131:路径条里用户敲进来的那一串,解析成一个真的能跳过去的路径。
//!
//! **纯函数,不碰 IO** —— 远端登录目录由调用方(`app.rs`)从 `sftp_home` 取,
//! 本地主目录由调用方从 `files::local` 取。面板自己既不知道 home 是什么,
//! 也不该知道。
//!
//! 相对路径拼在**当前目录**后面,不是发给远端让它按登录目录解析:用户在
//! `/var/log` 里敲 `nginx`,心智一定是 `/var/log/nginx`。`.` / `..` 在这里
//! 按**纯字符串**规整(不解析符号链接)——与既有的 `RemotePath::parent()`
//! (上级按钮)同一套语义,两处不同源的话「敲 `..` 回车」和「点 ↑」会去到
//! 两个不同的地方。

use mullion_ssh::sftp::RemotePath;

#[cfg(test)]
mod tests {
    use super::*;

    fn rp(s: &str) -> RemotePath {
        RemotePath::from_bytes(s.as_bytes().to_vec())
    }

    /// 绝对路径原样走。
    #[test]
    fn an_absolute_path_goes_through_untouched() {
        assert_eq!(
            resolve_remote_input("/var/log", &rp("/home/dev"), Some(b"/home/dev")),
            Some(rp("/var/log"))
        );
    }

    /// `~` / `~/x` 用远端登录目录展开 —— openssh 的 sftp-server **不认 `~`**,
    /// 原样发过去只会得到「找不到」。
    #[test]
    fn tilde_expands_with_the_sftp_login_directory() {
        assert_eq!(
            resolve_remote_input("~", &rp("/var"), Some(b"/home/dev")),
            Some(rp("/home/dev"))
        );
        assert_eq!(
            resolve_remote_input("~/Mullion", &rp("/var"), Some(b"/home/dev")),
            Some(rp("/home/dev/Mullion"))
        );
    }

    /// 登录目录还不知道时,`~` 无从展开 —— 不跳(而不是把字面量 `~` 发过去
    /// 换一条看不懂的错误)。
    #[test]
    fn tilde_without_a_known_home_does_not_jump() {
        assert_eq!(resolve_remote_input("~/x", &rp("/var"), None), None);
    }

    /// 相对路径拼在当前目录后面。
    #[test]
    fn a_relative_path_is_joined_onto_the_current_directory() {
        assert_eq!(
            resolve_remote_input("nginx", &rp("/var/log"), Some(b"/home/dev")),
            Some(rp("/var/log/nginx"))
        );
        assert_eq!(
            resolve_remote_input("./nginx/", &rp("/var/log"), Some(b"/home/dev")),
            Some(rp("/var/log/nginx"))
        );
    }

    /// `..` 就地规整,跟点「上一级」去到同一个地方。
    #[test]
    fn dotdot_is_normalised_the_same_way_the_up_button_does_it() {
        assert_eq!(
            resolve_remote_input("..", &rp("/var/log"), None),
            Some(rp("/var"))
        );
        assert_eq!(
            resolve_remote_input("../lib/x", &rp("/var/log"), None),
            Some(rp("/var/lib/x"))
        );
        // 根之上还是根 —— 不能跑出一个 `/..` 这种打不开的路径。
        assert_eq!(resolve_remote_input("../..", &rp("/var"), None), Some(rp("/")));
    }

    /// 空串 / 全空白 = 用户没打算跳,当取消。
    #[test]
    fn an_empty_input_is_a_cancel_not_a_jump_to_root() {
        assert_eq!(resolve_remote_input("", &rp("/var"), None), None);
        assert_eq!(resolve_remote_input("   ", &rp("/var"), None), None);
    }

    /// `~x` 不是 home 的写法(那是「叫 ~x 的目录」),不展开。
    #[test]
    fn tilde_glued_to_a_name_is_not_a_home_reference() {
        assert_eq!(
            resolve_remote_input("~x", &rp("/var"), Some(b"/home/dev")),
            Some(rp("/var/~x"))
        );
    }

    /// 本地栏:绝对路径原样,相对拼当前目录,`~` 用传进来的本地主目录。
    /// 用平台分隔符拼(`join_local` 的规矩),所以断言按平台算期望值。
    #[test]
    fn local_input_resolves_against_the_local_cwd_and_home() {
        let home = rp(&std::path::PathBuf::from("/h/dev").to_string_lossy());
        let cwd = rp(&std::path::PathBuf::from("/w").to_string_lossy());
        let got = resolve_local_input("sub", &cwd, Some(&home)).expect("该给出路径");
        assert_eq!(
            crate::files::local::to_path(&got),
            std::path::Path::new("/w").join("sub")
        );
        assert_eq!(resolve_local_input("  ", &cwd, Some(&home)), None);
        assert_eq!(resolve_local_input("~", &cwd, Some(&home)), Some(home));
    }
}
```

`crates/mullion-app/src/files/mod.rs` 里,在 `pub mod local;` 与 `pub mod queue;`
之间加一行(既有声明是字母序:drag / fail / local / queue / state / transfer):

```rust
pub mod path_input;
```

- [ ] **Step 2: 跑测试确认它失败**

```bash
cargo test -p mullion-app path_input 2>&1 | tail -20
```

预期:编译失败——`resolve_remote_input` / `resolve_local_input` 不存在。

- [ ] **Step 3: 写实现**

在 `path_input.rs` 的 `mod tests` **之前**加:

```rust
/// POSIX 路径的纯字符串规整:吃掉空段与 `.`,`..` 就地回退一级,到根为止。
/// **不解析符号链接** —— 与 `RemotePath::parent()` 同一套语义(见模块头)。
fn normalize_posix(bytes: &[u8]) -> Vec<u8> {
    let mut out: Vec<&[u8]> = Vec::new();
    for seg in bytes.split(|b| *b == b'/') {
        match seg {
            b"" | b"." => {}
            b".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    let mut v = Vec::with_capacity(bytes.len().max(1));
    for seg in out {
        v.push(b'/');
        v.extend_from_slice(seg);
    }
    if v.is_empty() {
        v.push(b'/');
    }
    v
}

/// F131:远端路径条里敲的一串 → 要跳过去的绝对路径。
///
/// `None` = 这一下不跳(空输入,或 `~` 但登录目录还不知道)。
pub fn resolve_remote_input(
    input: &str,
    cwd: &RemotePath,
    home: Option<&[u8]>,
) -> Option<RemotePath> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    let b = s.as_bytes();
    let joined: Vec<u8> = if b.starts_with(b"/") {
        b.to_vec()
    } else if b == b"~" || b.starts_with(b"~/") {
        let home = home?;
        let mut v = home.to_vec();
        v.extend_from_slice(&b[1..]);
        v
    } else {
        let mut v = cwd.as_bytes().to_vec();
        v.push(b'/');
        v.extend_from_slice(b);
        v
    };
    Some(RemotePath::from_bytes(normalize_posix(&joined)))
}

/// F131:本地路径条里敲的一串 → 要跳过去的本地路径。
///
/// 规整交给 `std::path`(`..` 由 `list_dir` 那一侧的 `PathBuf` 处理),这里只
/// 负责「相对拼在当前目录后面」与 `~` 展开。**用平台分隔符**(见
/// `local::join_local` 的说明:`D:\work/sub` 虽然能用,但拷给 PowerShell 很难看)。
///
/// 已知偏门行为:Windows 上的盘符相对路径(`C:foo`)会被 `PathBuf::join`
/// 整体替换掉当前目录。等价于「当绝对路径处理」,不会跳到别的目录去,
/// 不为它特判。
pub fn resolve_local_input(
    input: &str,
    cwd: &RemotePath,
    home: Option<&RemotePath>,
) -> Option<RemotePath> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    if s == "~" {
        return home.cloned();
    }
    if let Some(rest) = s.strip_prefix("~/").or_else(|| s.strip_prefix("~\\")) {
        let home = home?;
        return Some(crate::files::local::join_local(home, rest.as_bytes()));
    }
    let p = std::path::Path::new(s);
    if p.is_absolute() {
        return Some(RemotePath::from_bytes(
            crate::files::local::path_bytes(p),
        ));
    }
    Some(crate::files::local::join_local(cwd, s.as_bytes()))
}
```

`local::path_bytes`(`crates/mullion-app/src/files/local.rs:117`)已经是 `pub(crate)`,
直接用 —— 它是「`Path` → 字节」这个转换的唯一实现,复制一份到 `path_input.rs`
会出现两套平台分支。

- [ ] **Step 4: 跑测试**

```bash
cargo test -p mullion-app path_input 2>&1 | grep -E "test result|FAILED|panicked"
```

预期:8 条全 ok。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/files/path_input.rs crates/mullion-app/src/files/mod.rs crates/mullion-app/src/files/local.rs
git commit -m "feat(files): 路径输入解析(绝对/~/相对/.. 规整)的纯函数 (F131)"
```

---

## Task 5: F131 `FileAction::GotoInput` 接到两栏

**Files:**
- Modify: `crates/mullion-app/src/ui/files_panel.rs`(`FileAction` 枚举)
- Modify: `crates/mullion-app/src/app.rs`(`apply_local_file_action` / `apply_remote_file_action`)

- [ ] **Step 1: 加枚举变体**

`files_panel.rs` 的 `FileAction` 里,`Goto` 下面加:

```rust
    /// F131:用户在路径条里敲完回车的**原文**。故意不在面板里解析 ——
    /// `~` 要用远端登录目录展开,而那个值挂在 `TabContent` 上,面板不知道
    /// 也不该知道(同 `Reconnect` 不带参数的理由)。
    GotoInput(String),
```

- [ ] **Step 2: 跑一次 check 让编译器列出所有要补的 match 臂**

```bash
cargo check -p mullion-app 2>&1 | grep -E "^error|patterns.*not covered|-->" | head -20
```

预期:`apply_local_file_action` / `apply_remote_file_action` 两处
"non-exhaustive patterns" 报错(可能还有别处,以编译器输出为准)。

- [ ] **Step 3: 远端分支**

`apply_remote_file_action` 里,先在借出 `files` **之前**把 home 取出来
(借用顺序:`sftp_home()` 借的是 `&tab.content`,`files_panel_mut()` 借的是
`&mut`,不能同时):

```rust
        let home = self
            .tabs
            .by_generation(generation)
            .and_then(|t| t.content.sftp_home());
```

放在 `let Some(tab) = self.tabs.by_generation_mut(generation)` 那一行**之前**。
然后在 `let target = match &action {` 里,`FileAction::Goto(target)` 下面加:

```rust
            // F131:路径条敲的原文,在这里才解析 —— `~` 要用远端登录目录展开。
            // 解析不出来(空输入 / `~` 但还不知道登录目录)就什么都不做:
            // 「敲了回车什么都没发生」比「跳到一个猜出来的目录」好,但两者
            // 都不如报错——所以真正跳不过去的路径交给远端报,见下面的
            // `spawn_sftp_list_dir`(失败会落 `Load::Failed`)。
            FileAction::GotoInput(input) => {
                match crate::files::path_input::resolve_remote_input(
                    input,
                    &files.remote.cwd,
                    home.as_deref(),
                ) {
                    Some(p) => p,
                    None => return,
                }
            }
```

- [ ] **Step 4: 本地分支**

`apply_local_file_action` 的 `let target = match &action {` 里,
`FileAction::Goto(target)` 下面加:

```rust
            // F131:同远端那条,只是 home 来自本机。
            FileAction::GotoInput(input) => {
                let home = crate::files::local::home_dir();
                match crate::files::path_input::resolve_local_input(
                    input,
                    &files.local.cwd,
                    home.as_ref(),
                ) {
                    Some(p) => p,
                    None => return,
                }
            }
```

`files/local.rs` 里加(把 `default_local` 里那段主目录逻辑抽出来复用,
`default_local` 改成调它——两处各写一遍会分叉):

```rust
/// 本机主目录。`None` = 取不到(极少数无 HOME 的环境)。
pub fn home_dir() -> Option<RemotePath> {
    directories::BaseDirs::new()
        .map(|b| RemotePath::from_bytes(path_bytes(b.home_dir())))
}
```

`default_local` 里对应那段改成:

```rust
    if let Some(h) = home_dir() {
        return h;
    }
    let cwd = std::env::current_dir().unwrap_or_else(|| PathBuf::from("."));
    RemotePath::from_bytes(path_bytes(&cwd))
```

- [ ] **Step 5: 跑 check + 全量测试**

```bash
cargo check -p mullion-app 2>&1 | grep -E "^error" | head
cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|panicked"
```

预期:check 无 error;既有测试全过(这一步没改任何行为,只加了一条走不到的路)。

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-app/src/ui/files_panel.rs crates/mullion-app/src/app.rs crates/mullion-app/src/files/local.rs
git commit -m "feat(files): 加 FileAction::GotoInput,两栏各自解析路径原文 (F131)"
```

---

## Task 6: F131 路径条的只读↔编辑两态

**Files:**
- Modify: `crates/mullion-app/src/files/state.rs`(`PaneState` 加字段)
- Modify: `crates/mullion-app/src/ui/files_panel.rs:472-483`(路径条那一段)

- [ ] **Step 1: 写会失败的测试**

加到 `files_panel.rs` 的 `mod tests`。先看文件里既有的测试辅助(如
`run_panel` / `click_at` 之类),**复用它们**,不要另造一套 —— 若没有能直接
用的,照下面这个自足版本写:

```rust
    /// F131:点一下路径条就该进入编辑态。进不去的话这个功能对用户完全
    /// 不存在(它没有别的入口 —— 没有按钮、没有菜单项)。
    ///
    /// 两帧预热 + 显式推进时间:`Area`/面板的首帧布局没走完时点不中
    /// (同 `rehost::tests::click` 那个坑)。
    ///
    /// 自证会变红:把 `show` 里路径 `Label` 外面那层 `Sense::click`
    /// 的 `if resp.clicked()` 分支删掉。
    #[test]
    fn clicking_the_path_bar_starts_editing_it() {
        let mut state = PaneState::new(mullion_ssh::sftp::RemotePath::from_bytes(
            b"/var/log".to_vec(),
        ));
        state.load = Load::Ready;
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(1.0);
        let base = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(600.0, 500.0),
            )),
            ..Default::default()
        };
        let mut run = |input: egui::RawInput, state: &mut PaneState| {
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show(
                        ui,
                        &crate::theme::MULLION_DARK,
                        "远端",
                        1,
                        PanelColumn::Remote,
                        state,
                        true,
                        &[],
                        0,
                    );
                });
            });
        };
        for t in [0.0_f64, 1.0] {
            run(
                egui::RawInput {
                    time: Some(t),
                    ..base()
                },
                &mut state,
            );
        }
        let rect = ctx
            .read_response(path_label_id("远端"))
            .expect("路径条没有可交互的响应 —— 它点不动")
            .rect;
        let pos = rect.center();
        let m = egui::Modifiers::default();
        run(
            egui::RawInput {
                time: Some(2.0),
                events: vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: m,
                    },
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: m,
                    },
                ],
                ..base()
            },
            &mut state,
        );
        assert_eq!(
            state.path_edit.as_deref(),
            Some("/var/log"),
            "点了路径条却没进入编辑态(或者没把当前目录填进去)"
        );
    }

    /// F131:退出编辑态**不跳转**是默认;只有回车才跳。反过来的话
    /// (失焦即提交)用户点别处就会被莫名其妙带走。
    ///
    /// 这条只测状态机本身(`finish_path_edit`),不驱动 egui —— 回车/Esc
    /// 在 egui 里都表现为 `lost_focus`,区分靠的是当帧有没有 Enter 键事件,
    /// 那一段是接线,由下面那条源码级守护钉住。
    #[test]
    fn leaving_the_path_editor_without_enter_discards_the_input() {
        let mut state = PaneState::new(mullion_ssh::sftp::RemotePath::from_bytes(
            b"/var/log".to_vec(),
        ));
        state.path_edit = Some("/etc".into());
        assert_eq!(finish_path_edit(&mut state, false), None);
        assert!(state.path_edit.is_none(), "退出编辑态时缓冲没清");

        state.path_edit = Some("/etc".into());
        assert_eq!(
            finish_path_edit(&mut state, true),
            Some(FileAction::GotoInput("/etc".into()))
        );
        assert!(state.path_edit.is_none());
    }
```

- [ ] **Step 2: 跑测试确认它失败**

```bash
cargo test -p mullion-app clicking_the_path_bar 2>&1 | tail -20
```

预期:编译失败——`path_edit` / `path_label_id` / `finish_path_edit` 都不存在。

- [ ] **Step 3: 加字段**

`files/state.rs` 的 `PaneState` 里加:

```rust
    /// F131:路径条正在被编辑时的缓冲。`None` = 只读态(默认)。
    ///
    /// 放每栏一份而不是每面板一份:两栏各有自己的路径条,而 egui 的键盘
    /// 焦点唯一 —— 另一栏的编辑框一失焦就自己取消了,不需要额外互斥。
    pub path_edit: Option<String>,
```

`PaneState::new` 里补 `path_edit: None,`。

- [ ] **Step 4: 改路径条渲染**

`files_panel.rs` 里加两个辅助(放在 `show` 之前):

```rust
/// 路径条只读态那块 `Label` 的 id。**必须是稳定的**——「点得中路径条」是
/// F131 唯一的入口,而 egui 自动分配的 id 在测试里只能靠猜坐标
/// (同 `rehost::row_id` 的理由)。按栏分:两栏各有一条路径条。
fn path_label_id(id: &str) -> egui::Id {
    egui::Id::new(("files-path-label", id))
}

/// 编辑框自己的 id。同上。
fn path_edit_id(id: &str) -> egui::Id {
    egui::Id::new(("files-path-edit", id))
}

/// F131:退出编辑态。`commit` = 这一下是回车(要跳),否则是 Esc / 失焦
/// (丢弃)。返回要发出去的动作。
///
/// **默认丢弃**:失焦最常见的原因是用户去点了别处,那不是「确认」。
fn finish_path_edit(state: &mut PaneState, commit: bool) -> Option<FileAction> {
    let buf = state.path_edit.take()?;
    commit.then(|| FileAction::GotoInput(buf))
}
```

把 `show` 里路径条那一段(`let path = state.cwd.display().to_string();` 起两行)
替换成:

```rust
        let path = state.cwd.display().to_string();
        annotate::mark(ui.ctx(), format!("文件面板/{id}/路径"), ui.max_rect());
        match state.path_edit.as_mut() {
            // 编辑态。**注意它能收到键盘全靠 `Modal::FilesPathEdit`** ——
            // 面板拿着键盘焦点时,键根本不会喂给 egui(`input_route::
            // egui_should_see_focused`,T8 的注入点),不算模态的话这个框
            // 里一个字都打不出来(同 `Modal::Editor` 当年踩的坑)。
            Some(buf) => {
                let resp = ui.add(
                    egui::TextEdit::singleline(buf)
                        .id(path_edit_id(id))
                        .desired_width(ui.available_width()),
                );
                if !resp.has_focus() && state.path_edit.is_some() {
                    // 首帧:把焦点给它,否则用户还得再点一下才能打字。
                    resp.request_focus();
                }
                if resp.lost_focus() {
                    let commit = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if let Some(a) = finish_path_edit(state, commit) {
                        action = Some(a);
                    }
                }
            }
            None => {
                let label = ui.add(
                    egui::Label::new(egui::RichText::new(path.clone()).color(theme::c32(t.fg_mid)))
                        .truncate()
                        .sense(egui::Sense::click()),
                );
                let hit = ui.interact(label.rect, path_label_id(id), egui::Sense::click());
                if hit.clicked() {
                    state.path_edit = Some(path);
                }
            }
        }
```

若 egui 0.30 的 `Label::sense` 不存在,就只留 `ui.interact(..)` 那一句
(`Label` 照旧 `ui.add`,`interact` 拿它的 `rect`)——两条路都能被上面的
测试验到。

- [ ] **Step 5: 跑测试**

```bash
cargo test -p mullion-app files_panel 2>&1 | grep -E "test result|FAILED|panicked"
```

预期:两条新测试 + 既有 files_panel 测试全过。

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-app/src/files/state.rs crates/mullion-app/src/ui/files_panel.rs
git commit -m "feat(files): 路径条点一下进入编辑,回车跳转、失焦丢弃 (F131)"
```

---

## Task 7: F131 编辑态登记进 `Modal`

**Files:**
- Modify: `crates/mullion-app/src/app.rs`(`enum Modal` / `Modal::ALL` / `modal_open`)

- [ ] **Step 1: 写会失败的测试**

加到 `app.rs` 的 `mod tests`(照既有 `the_exit_confirm_is_a_modal_...` 那条
源码级守护的写法;**锚点字符串必须带行首缩进**,否则会匹配到测试自己写的
字面量,变成恒绿——本项目已实证的第五类恒绿模式):

```rust
    /// F131:路径条的编辑态必须算模态。**不算的话那个输入框里一个字都打不
    /// 出来** —— 面板拿着键盘焦点时,键不会喂给 egui
    /// (`input_route::egui_should_see_focused` 是 T8 的注入点),而是被
    /// `handle_panel_key` 吃掉:Backspace 变成「回上级目录」,字母键什么都
    /// 不做。这跟 `Modal::Editor` 当年踩的是同一个坑。
    ///
    /// 自证会变红:把 `Modal::FilesPathEdit` 从 `Modal::ALL` 里删掉
    /// (第二条断言红),或把 `modal_open` 里那一支改成 `=> false`
    /// (第三条断言红)。
    #[test]
    fn the_files_path_editor_is_a_modal_or_it_cannot_receive_a_single_keystroke() {
        let src = include_str!("app.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .expect("split 至少给一段");
        assert!(
            prod.len() < src.len(),
            "没能把搜索范围切到 `mod tests` 之前 —— 下面的断言会命中测试自己\
             写的字面量,变成恒绿"
        );
        assert!(
            prod.contains("    FilesPathEdit,"),
            "Modal 枚举里没有 FilesPathEdit"
        );
        assert!(
            prod.contains("        Modal::FilesPathEdit,"),
            "Modal::ALL 里漏了 FilesPathEdit —— modal_open 照 ALL 遍历,\
             漏加等于这一支从来不生效"
        );
        assert!(
            prod.contains("            Modal::FilesPathEdit => self.files_path_editing(),"),
            "modal_open 没有认 FilesPathEdit"
        );
    }
```

- [ ] **Step 2: 跑测试确认它失败**

```bash
cargo test -p mullion-app the_files_path_editor_is_a_modal 2>&1 | grep -E "test result|panicked" | head
```

预期:FAILED,第一条断言就红。

- [ ] **Step 3: 加变体 + 判据**

`enum Modal` 末尾(`Rehost` 之后)加:

```rust
    /// F131:文件面板的路径条正在被编辑。**不算模态的话那个输入框收不到
    /// 任何键** —— 面板持有键盘焦点时键根本不喂 egui(T8 的注入点在
    /// `input_route::egui_should_see_focused`),Backspace 还会被
    /// `handle_panel_key` 解释成「回上级目录」。同 `Editor` 的理由。
    ///
    /// **不进 `touched_store`**:它一行 store 都不写(同 `Rehost` 的姿态)。
    FilesPathEdit,
```

`Modal::ALL` 末尾加 `Modal::FilesPathEdit,`。

`modal_open` 的 match 里加:

```rust
            // F131:见 `Modal::FilesPathEdit` 的说明。
            Modal::FilesPathEdit => self.files_path_editing(),
```

`App` 上加方法(放在 `files_owner_generation` 附近):

```rust
    /// F131:这一帧文件面板的某一栏正在编辑路径吗。
    ///
    /// 判据走 `files_owner_generation()`,与面板「这一帧到底画不画得出来」
    /// 同源 —— 面板不可见时恒 `false`,不会因为某个后台标签里留着一个没清
    /// 干净的编辑缓冲就把整个窗口判成模态。
    fn files_path_editing(&self) -> bool {
        self.files_owner_generation()
            .and_then(|g| self.tabs.by_generation(g))
            .and_then(|t| t.content.files_panel())
            .is_some_and(|f| f.remote.path_edit.is_some() || f.local.path_edit.is_some())
    }
```

- [ ] **Step 4: 跑测试**

```bash
cargo test -p mullion-app modal 2>&1 | grep -E "test result|FAILED|panicked"
```

预期:新测试与既有几条 modal 守护全过。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): 路径条编辑态登记进 Modal,否则输入框收不到键 (F131/T8)"
```

---

## Task 8: F132 记住 sftp channel 属于哪台机器

**Files:**
- Modify: `crates/mullion-app/src/app.rs`(`TerminalTab` / `UserEvent::SftpOpened` / `sftp_connection` / `trigger_sftp_open` / `accept_sftp_opened` / `spawn_sftp_open`)

- [ ] **Step 1: 写会失败的测试**

加到 `app.rs` 的 `mod tests`:

```rust
    /// F132:这条 sftp channel 到底开在哪台机器上,必须在**打开成功回来的
    /// 那一刻**记下(`accept_sftp_opened`),而不是发起时就写。
    ///
    /// 开 channel 是一次真实网络往返,期间用户完全可能又换了焦点分屏;
    /// 发起时写的话,`sftp_host_ix` 记的是「最后一次发起的意图」,而不是
    /// 「手上这条 client 的真实归属」——之后的比对全部错位,症状是换节点后
    /// 侧栏时对时不对,查都没法查。
    ///
    /// 扎的是源码结构:真验它要一条活 sftp 连接,这个测试容器里造不出来
    /// (同本文件其余几条接线守护)。锚点带行首缩进,否则会匹配到测试自己。
    ///
    /// 自证会变红:把 `t.sftp_host_ix = host_ix;` 从 `accept_sftp_opened`
    /// 挪进 `trigger_sftp_open`。
    #[test]
    fn the_sftp_host_is_recorded_when_the_channel_opens_not_when_it_is_requested() {
        let src = include_str!("app.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .expect("split 至少给一段");
        assert!(prod.len() < src.len(), "范围没切到 mod tests 之前");
        let at = prod
            .find("    fn accept_sftp_opened(")
            .expect("找不到 accept_sftp_opened");
        let body = &prod[at..];
        let end = body.find("\n    }\n").expect("找不到函数结尾");
        assert!(
            body[..end].contains("sftp_host_ix = host_ix"),
            "accept_sftp_opened 里没记 sftp_host_ix —— 换节点后的比对会永远错位"
        );
    }

    /// F132:「侧栏关→开」那一帧该做什么,三选一。这是这一条里唯一测得动的
    /// 核心逻辑(`App` 本身要 `EventLoopProxy`,无头容器造不出来)。
    ///
    /// 自证会变红:把 `sync_plan_of` 里 host 比对那一支删掉 —— 第三条断言
    /// 会从 `Reopen` 变成 `Goto`,也就是回到「路径对了、机器错了」那个 bug。
    #[test]
    fn the_open_edge_reopens_sftp_only_when_the_focused_pane_is_on_another_host() {
        // 还没连上:什么都不做(`trigger_sftp_open` 那条路负责起步)。
        assert_eq!(
            sync_plan_of(false, None, Some(0), Some(b"/srv"), Some(b"/home/dev")),
            SyncPlan::Nothing
        );
        // 同一台:只同步目录。
        assert_eq!(
            sync_plan_of(true, Some(0), Some(0), Some(b"/srv"), Some(b"/home/dev")),
            SyncPlan::Goto("/srv".into())
        );
        // 换过节点的分屏:必须重开,否则连的是第一台机器、路径却来自第二台。
        assert_eq!(
            sync_plan_of(true, Some(0), Some(1), Some(b"/srv"), Some(b"/home/dev")),
            SyncPlan::Reopen
        );
        // SFTP 节点标签(没有终端,拿不到 host_ix):照旧只同步目录。
        assert_eq!(
            sync_plan_of(true, Some(0), None, Some(b"/srv"), Some(b"/home/dev")),
            SyncPlan::Goto("/srv".into())
        );
        // 同一台但 pane 没报过目录:什么都不做,不能把用户当前浏览的位置
        // 拽回一个猜出来的目录。
        assert_eq!(
            sync_plan_of(true, Some(0), Some(0), None, Some(b"/home/dev")),
            SyncPlan::Nothing
        );
    }
```

- [ ] **Step 2: 跑测试确认它失败**

```bash
cargo test -p mullion-app sync_plan_of 2>&1 | tail -20
```

预期:编译失败——`sync_plan_of` / `SyncPlan` 不存在。

- [ ] **Step 3: 加 `SyncPlan` 与纯函数**

放在 `sync_target_of` 旁边(**保留** `sync_target_of`,`sync_plan_of` 内部调它):

```rust
/// F132:「文件侧栏关→开」那一帧该做什么。
#[derive(Debug, Clone, PartialEq, Eq)]
enum SyncPlan {
    /// 什么都不做(还没连上 / pane 没报过目录)。
    Nothing,
    /// 同一台机器,只把远端栏带到这个目录。
    Goto(String),
    /// 焦点分屏在**另一台**机器上:摘掉现在这条 sftp channel,在那台上重开。
    Reopen,
}

/// [`SyncPlan`] 的判定。纯函数 —— `App` 要 `EventLoopProxy`,无头测试里
/// 造不出来,只有把判定摘出来才验得了。
///
/// `focus_host_ix` 为 `None` = 这个标签没有终端(SFTP 节点标签),
/// 「焦点分屏在哪台」无从谈起,不重开。
fn sync_plan_of(
    has_client: bool,
    sftp_host_ix: Option<usize>,
    focus_host_ix: Option<usize>,
    pane_cwd: Option<&[u8]>,
    home: Option<&[u8]>,
) -> SyncPlan {
    if !has_client {
        return SyncPlan::Nothing;
    }
    if let Some(fix) = focus_host_ix {
        if sftp_host_ix != Some(fix) {
            return SyncPlan::Reopen;
        }
    }
    match files_start_dir(pane_cwd, None, home) {
        Some(dir) => SyncPlan::Goto(dir),
        None => SyncPlan::Nothing,
    }
}
```

- [ ] **Step 4: 加 `sftp_host_ix` 字段与取连接的按台版本**

`TerminalTab` 结构里(`sftp` 字段附近)加:

```rust
    /// F132:`sftp` 这条 channel 开在 `ws.hosts` 的哪一台上。`None` = 还没开过。
    ///
    /// 用户用「换节点」把某块分屏挪到第二台机器之后,`hosts` 里就有两台,
    /// 而侧栏只有一条 channel。不记归属的话,侧栏连的是第一台、目录却来自
    /// 焦点分屏(第二台)——**路径对了、机器错了**,一次看不出错的误操作。
    sftp_host_ix: Option<usize>,
```

所有 `TerminalTab { .. }` 字面构造处补 `sftp_host_ix: None,`
(`cargo check` 会逐个指出来)。

`TabContent` 上加:

```rust
    /// F132:焦点分屏挂在 `ws.hosts` 的哪一台上。SFTP 节点标签/占位标签
    /// 没有终端,恒 `None`。
    fn focused_pane_host_ix(&self) -> Option<usize> {
        self.as_terminal()
            .and_then(|t| t.ws.focused())
            .map(|p| p.host_ix)
    }
```

把 `sftp_connection` 改成按台取(**保留原函数名**会误导,改名并更新文档):

```rust
    /// D6/F132:这个标签的 sftp 该蹭哪条连接。
    ///
    /// `host_ix` = 要开在哪台上(`None` 或越界时落回 `hosts[0]`,也就是
    /// 这个标签的主连接)。`Files` 宿主独占自己那条(ADR-010),不看 `host_ix`。
    ///
    /// `hosts[ix]` 在断线重连时是**就地替换** `handle`(见
    /// `UserEvent::PaneReconnected`),所以重连之后这里取到的仍是活的那条。
    /// F128 早期版本走的是 push 新连接那条路,`hosts[0]` 从此指向死连接,
    /// 症状是重连之后文件面板永久打不开。
    fn sftp_connection_for(&self, host_ix: Option<usize>) -> Option<Arc<SshConnection>> {
        match self {
            TabContent::Terminal(t) => {
                let ix = host_ix.unwrap_or(0);
                t.ws.hosts
                    .get(ix)
                    .or_else(|| t.ws.hosts.first())
                    .map(|h| h.handle.clone())
            }
            TabContent::Files(f) => Some(f.conn.clone()),
            TabContent::Restored(_) => None,
        }
    }
```

原 `sftp_connection` 的调用点全部改成 `sftp_connection_for(host_ix)`
(`cargo check` 会指出来);老函数删掉,不留一个「恒取第一台」的入口。

- [ ] **Step 5: 让 host_ix 随事件走一圈**

`UserEvent::SftpOpened` 加字段:

```rust
    SftpOpened {
        generation: u64,
        /// F132:这条 channel 开在哪台上。发起时是什么,回来时还是什么 ——
        /// 期间用户可能已经换了焦点分屏,不能在收到时现算。
        host_ix: Option<usize>,
        result: Result<
            (
                Arc<mullion_ssh::sftp::SftpClient>,
                mullion_ssh::sftp::RemotePath,
                mullion_ssh::sftp::RemotePath,
            ),
            String,
        >,
    },
```

`spawn_sftp_open` 加同名参数,并在发事件时带上:

```rust
fn spawn_sftp_open(
    runtime: &Runtime,
    proxy: &EventLoopProxy<UserEvent>,
    generation: u64,
    host_ix: Option<usize>,
    handle: Arc<SshConnection>,
    default_remote: Option<String>,
    pane_cwd: Option<Vec<u8>>,
) -> tokio::task::JoinHandle<()> {
```
```rust
        let _ = proxy.send_event(UserEvent::SftpOpened {
            generation,
            host_ix,
            result,
        });
```

`trigger_sftp_open` 里取连接改成:

```rust
        let host_ix = tab.content.focused_pane_host_ix();
        let Some(conn) = tab.content.sftp_connection_for(host_ix) else {
            return;
        };
```

并把 `host_ix` 传给 `spawn_sftp_open`。

`accept_sftp_opened` 的签名加 `host_ix: Option<usize>`,`Ok` 分支里、
`*slot = Some(client.clone());` 之后加:

```rust
                    // F132:记住这条 channel 的真实归属。**在这里记,不在发起处**
                    // —— 开 channel 是一次网络往返,期间焦点分屏可能已经换了。
                    if let Some(t) = tab.content.as_terminal_mut() {
                        t.sftp_host_ix = host_ix;
                    }
```

事件分发处(`UserEvent::SftpOpened { generation, result } =>`)改成解构三个字段
并传下去。

- [ ] **Step 6: 跑测试**

```bash
cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|panicked"
```

预期:全过,含两条新测试。

- [ ] **Step 7: 提交**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): sftp channel 记住开在哪台机器上 (F132)"
```

---

## Task 9: F132 关→开那一帧按焦点分屏的节点重开

**Files:**
- Modify: `crates/mullion-app/src/app.rs`(`sync_files_to_focused_pane`)

- [ ] **Step 1: 写会失败的测试**

```rust
    /// F132:重开之前必须**先摘掉旧 client**,再调 `trigger_sftp_open`。
    ///
    /// 顺序反了是静默失败:`trigger_sftp_open` 开头就有
    /// `if tab.content.sftp_client().is_some() { return; }`,旧 client 还挂着
    /// 的话它直接早退,一个字节都不会发 —— 用户看到的是「按了没反应,侧栏
    /// 还是连着另一台机器」。
    ///
    /// 同理**不能提前把面板置成 `Loading`**:`trigger_sftp_open` 的另一个
    /// 早退条件正是 `already_loading`。
    ///
    /// 扎的是源码结构(要活连接才验得了真行为)。锚点带行首缩进。
    ///
    /// 自证会变红:把 `reopen_sftp_on_focused_host` 里那句
    /// `*slot = None;` 删掉,或在它里面加一句把 `load` 置成 `Loading`。
    #[test]
    fn reopening_sftp_drops_the_old_client_before_asking_for_a_new_one() {
        let src = include_str!("app.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .expect("split 至少给一段");
        assert!(prod.len() < src.len(), "范围没切到 mod tests 之前");
        let at = prod
            .find("    fn reopen_sftp_on_focused_host(")
            .expect("缺 reopen_sftp_on_focused_host —— 换节点后侧栏不会跟过去");
        let body = &prod[at..];
        let end = body.find("\n    }\n").expect("找不到函数结尾");
        let body = &body[..end];
        let drop_at = body.find("*slot = None;").expect("没摘掉旧 client");
        let call_at = body
            .find("self.trigger_sftp_open(generation);")
            .expect("没调 trigger_sftp_open");
        assert!(
            drop_at < call_at,
            "先调了 trigger_sftp_open 才摘旧 client —— 它会在开头早退,静默失败"
        );
        assert!(
            !body.contains("Load::Loading"),
            "提前把面板置成 Loading 会撞上 trigger_sftp_open 的 already_loading 早退"
        );
    }
```

- [ ] **Step 2: 跑测试确认它失败**

```bash
cargo test -p mullion-app reopening_sftp_drops 2>&1 | grep -E "panicked|test result" | head
```

预期:FAILED —— 找不到 `reopen_sftp_on_focused_host`。

- [ ] **Step 3: 改 `sync_files_to_focused_pane` + 加重开函数**

```rust
    fn sync_files_to_focused_pane(&mut self) {
        let Some(gen) = self.files_owner_generation() else {
            return;
        };
        let tab = self.tabs.by_generation(gen);
        let has_client = tab.is_some_and(|t| t.content.sftp_client().is_some());
        let sftp_host_ix = tab
            .and_then(|t| t.content.as_terminal())
            .and_then(|t| t.sftp_host_ix);
        let focus_host_ix = tab.and_then(|t| t.content.focused_pane_host_ix());
        let pane_cwd = tab.and_then(|t| t.content.focused_pane_cwd());
        let home = tab.and_then(|t| t.content.sftp_home());
        match sync_plan_of(
            has_client,
            sftp_host_ix,
            focus_host_ix,
            pane_cwd.as_deref(),
            home.as_deref(),
        ) {
            SyncPlan::Nothing => {}
            SyncPlan::Goto(dir) => {
                let target = mullion_ssh::sftp::RemotePath::from_bytes(dir.into_bytes());
                self.apply_remote_file_action(
                    gen,
                    crate::ui::files_panel::FileAction::Goto(target),
                );
            }
            SyncPlan::Reopen => self.reopen_sftp_on_focused_host(gen),
        }
    }

    /// F132:焦点分屏在另一台机器上 —— 把这条 sftp channel 换过去。
    ///
    /// **顺序不可换**:先 abort 在途任务、摘掉旧 client,再
    /// `trigger_sftp_open`。反过来的话它开头那句
    /// `if tab.content.sftp_client().is_some() { return; }` 会直接早退,
    /// 什么都不发 —— 用户看到的是「按了没反应」。同理**不在这里把面板置成
    /// `Loading`**:那会撞上它的 `already_loading` 早退(置 `Loading` 是
    /// `trigger_sftp_open` 自己的事)。
    ///
    /// 摘 client 只是把 `Arc` 从槽位拿走:正在跑的传输各自持有自己的 `Arc`,
    /// 能跑完,不会被腰斩。
    fn reopen_sftp_on_focused_host(&mut self, generation: u64) {
        if let Some(tab) = self.tabs.by_generation_mut(generation) {
            if let Some(tasks) = tab.content.sftp_tasks_mut() {
                for t in tasks.drain(..) {
                    t.abort();
                }
            }
            if let Some(slot) = tab.content.sftp_mut() {
                *slot = None;
            }
            if let Some(t) = tab.content.as_terminal_mut() {
                t.sftp_host_ix = None;
            }
            if let Some(files) = tab.content.files_panel_mut() {
                // 换了台机器,原来那一屏的选中/光标指的是另一台上的文件,
                // 留着只会让下一次操作打到不存在的路径上。
                files.remote.clear_selection();
            }
        }
        self.trigger_sftp_open(generation);
    }
```

`clear_selection` 若不是 `pub`,把它改成 `pub`(`files/state.rs`)。

- [ ] **Step 4: 跑全量测试**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
```

预期:全部 `test result: ok`。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/app.rs crates/mullion-app/src/files/state.rs
git commit -m "feat(app): 侧栏关→开时按焦点分屏的节点重开 sftp (F132)"
```

---

## Task 10: spec.md 登记五条需求

**Files:**
- Modify: `spec.md`(§4 的需求表,F129 之后)

- [ ] **Step 1: 加五行**

照既有行的格式(`| 编号 | 描述 | 优先级 | 验收/守护 |`)加:

```markdown
| F130 | **换节点弹窗的会话顺序与会话管理器左栏一致**（分组桶顺序 + 组内数组顺序）。两边各排一套的话，左栏里挨着的两条在弹窗里可能隔着半屏，而用户是照着左栏的记忆找的 | P2 | `rehost::tests::rows_are_ordered_exactly_like_the_session_manager_list`：顺序取自 `list::visible_order`（左栏渲染与键盘导航的同一个函数），fixture 刻意让数组顺序与分组顺序相反 |
| F131 | **文件面板路径条可编辑**（两栏）。单击进入编辑、回车跳转、Esc/失焦丢弃；`~` 用远端登录目录（本地栏用本机主目录）展开，相对路径拼在当前目录后面，`..` 按纯字符串规整 | P2 | `path_input` 的八条解析单测；`files_panel::tests::clicking_the_path_bar_starts_editing_it`；**编辑态必须进 `Modal`**（面板持焦时键盘不喂 egui，不算模态则一个字都打不出来，T8），有源码级守护 |
| F132 | **`Ctrl+Shift+B` 开侧栏时，SFTP 开在焦点分屏所在的那台机器上**，起始目录 = 该分屏的 cwd。换过节点的分屏此前恒连 `hosts[0]`——路径对了、机器错了，一次看不出错的误操作 | P2 | `sync_plan_of` 的五条分支单测；「host_ix 在打开成功时记、不在发起时记」与「先摘旧 client 再 `trigger_sftp_open`」两条源码级守护（后者反了会撞早退，静默失败） |
| F133 | **office 大类各自的图标**：PDF / Word（doc·docx）/ 表格（xls·xlsx·csv）/ 演示（ppt·pptx），颜色对齐 Windows 心智（红蓝绿橙） | P3 | `file_icon::tests::classify_maps_extensions_to_kinds`；既有的「两两不同」「不越格」两条照 `IconKind::ALL` 遍历；`every_kind_used_by_the_extension_table_is_listed_in_all` 防「加档漏进 ALL」 |
| F134 | **普通文件的默认图标从「其他」里拆出来**：不认识扩展名的普通文件用折角空白页，菱形只留给设备/socket/命名管道 | P3 | `an_unknown_regular_file_is_not_the_same_as_a_device_node` |
```

- [ ] **Step 2: 提交**

```bash
git add spec.md
git commit -m "docs: spec 登记 F130~F134"
```

---

## Task 11: 跑绿 + 发版

**Files:**
- Modify: `Cargo.toml`(`workspace.package.version` 第三位 +1)

- [ ] **Step 1: 全量绿**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
cargo fmt --check && echo "fmt ok"
```

预期:测试全 ok、clippy 无输出、fmt ok。**三条都过才叫绿**(CLAUDE.md)。

- [ ] **Step 2: 升 patch 版本号,单独一个提交**

```bash
# 把 Cargo.toml 里 workspace.package.version 的第三位 +1(当前 0.1.52 → 0.1.53)
git add Cargo.toml Cargo.lock
git commit -m "chore: v0.1.53"
```

- [ ] **Step 3: 交叉编译 + 发 Release**

用 `release-windows` skill(说「发版」即自动加载),它覆盖:交叉编译、
objdump 依赖验收(出现 `libgcc_s_seh-1.dll` / `libwinpthread-1.dll` 即不合格)、
先 push 再 `gh release create`(标题只能是纯版本号 `v0.1.53`)、代理设置。

- [ ] **Step 4: 交人工验收**

Release 链接 + sha256 + 下面这份清单:

1. **F130**:开一块分屏的「换节点」,对照会话管理器左栏,逐行核对顺序一致
   (需要至少两个分组、且组内顺序被拖拽调整过)。
2. **F131**:远端栏点路径条 → 输 `/var/log` 回车 → 进得去;输 `~` 回车 →
   回登录目录;输 `../` 回车 → 上一级;输一半按 Esc → 停在原处;
   点别处 → 停在原处。本地栏同样跑一遍。**中文输入法**在路径框里能不能用。
3. **F132**:把一块分屏「换节点」到第二台机器(两台上各 `touch` 一个只有
   自己有的文件),焦点停在它上面按 `Ctrl+Shift+B` —— 侧栏里出现的应该是
   **第二台**那个文件,且目录是该分屏的当前目录。再把焦点切回第一台的分屏,
   关掉侧栏再打开,应该切回第一台。
4. **F133/F134**:找一个有 pdf/docx/xlsx/pptx/无扩展名文件的目录,看五种图标
   在真实 DPI 下**认不认得出、扫不扫得开**,颜色在深色底上够不够清楚。
