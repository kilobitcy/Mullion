# F296~F301 启动页过滤/拨号动画 · 节点状态 · 编辑器查找 · 书签宽度与 `~` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 六条定案(F296~F301)一个切片做完、一次发版 v0.1.119。

**Architecture:** 全部落在 `mullion-app`,不动依赖方向。三处新纯函数模块(`node_stats`、`ui/editor_find`、F300 的像素省略函数)承担可单测的判据;App 侧只做接线,接线用源码切片守护 + 纯函数剥离。「哪些在拨号」从 F205 票据台账现算,不建影子状态;节点状态挂在 `HostConn` 的共享格子上,同 F124 `BootstrapFlags` 的跨线程模式。

**Tech Stack:** Rust / egui 0.30 / epaint 0.30 / russh exec channel / tokio。

设计定案:`docs/superpowers/specs/2026-09-28-f296-f301-launcher-node-stats-editor-find-bookmarks-design.md`。

**通用纪律(每个任务都适用):**
- 注释、提交信息一律简体中文;提交摘要带 spec 编号;结尾带 `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`。
- 每条新守护测试写好后**先自证会变红**(测试文档注释里写「自证会变红:…」并真的做一次变异)。**变异前先 commit**(`git checkout` 已经吞过五次未提交编辑)。
- 大输出落盘再 grep:`cargo test -p mullion-app > /tmp/t.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/t.log`。
- UI 字符串里的非 ASCII 符号要过 T9(`tests/glyph_whitelist.rs`)。本计划只用到中文、`%`、`·`、`--`、`/`、`…`;`·` 和 `…` 已在 `ui::glyphs::VERIFIED`(F124 标题条用过)——实现时 `grep -n "'·'\|'…'" crates/mullion-app/src/ui/glyphs.rs` 核一遍。
- egui `.strong()` 全库禁用(`tests/strong_text_color.rs`)。

---

## 文件结构

| 文件 | 动作 | 职责 |
|---|---|---|
| `crates/mullion-app/src/ui/launcher.rs` | 改 | F296 过滤;F297 行上的 spinner/忽略点击;`Lists` 加两张在拨表 |
| `crates/mullion-app/src/ui/project_row.rs` | 改 | F297 `Row.dialing` + 右侧 spinner |
| `crates/mullion-app/src/shell/dial_ledger.rs` | 改 | F297 `iter()` |
| `crates/mullion-app/src/app.rs` | 改 | F297 `DialTicket.project_id` + 在拨表推导;F298 `tick_node_stats`/事件/唤醒/标题接线;F301 `remote_home` 进 UiFrame |
| `crates/mullion-app/src/ui/mod.rs` | 改 | UiFrame 加 `dialing_sessions`/`dialing_projects`/`remote_home`;`UiActions.refresh_node_pane` |
| `crates/mullion-app/src/ui/files_panel.rs` | 改 | F300 下拉宽度;F301 `BookmarkView.home` + `~` 解析 |
| `crates/mullion-app/src/files/local.rs` | 改 | F301 `home_dir_cached()`;`pick_default_local` 展开 `~` |
| `crates/mullion-app/src/ui/editor_find.rs` | **新建** | F299 纯查找逻辑 |
| `crates/mullion-app/src/ui/editor_window.rs` | 改 | F299 查找条 + overlay |
| `crates/mullion-app/src/node_stats.rs` | **新建** | F298 命令/解析/调度/显示纯函数 + `StatsCell` |
| `crates/mullion-app/src/shell/workspace/mod.rs` | 改 | F298 `HostConn.stats` |
| `crates/mullion-app/src/ui/pane_title.rs` | 改 | F298 `TitleView.stats` + 右侧状态段 + `TitleAction.refresh_node` |
| `spec.md` | 改 | F296~F301 六行 |
| `Cargo.toml`(workspace) | 改 | 版本 0.1.119 |

---

## Task 1: F296 启动页会话列过滤 SFTP

**Files:**
- Modify: `crates/mullion-app/src/ui/launcher.rs:285-295`(`session_order`)、`:333-341`(空态判据)
- Test: 同文件 `mod tests`

- [ ] **Step 1: 写失败测试**

在 `launcher.rs` 的 `mod tests` 末尾加:

```rust
    /// F296:启动页第二列不显示 SFTP 会话 —— 这一列是「挑一台机器连上去开终端」,
    /// SFTP 节点连上去没有终端,点了只会开一个文件浏览标签,跟用户的预期不符。
    ///
    /// 同时钉住终端会话**照常出现**:只断言 SFTP 不在的话,把整列清空也是绿的。
    ///
    /// 自证会变红:删掉 `session_order` 里 `protocol != Protocol::Sftp` 那一行过滤。
    #[test]
    fn the_launcher_session_column_leaves_sftp_sessions_out() {
        let mut sftp = sess(5, "文件机");
        sftp.connection.protocol = mullion_store::Protocol::Sftp;
        let ss = vec![sess(9, "独立机"), sftp];
        let (_, shown, _) = columns_drawn(&[], &ss, &[], "");
        assert_eq!(shown, vec![9], "SFTP 会话漏进了启动页,或终端会话被一起过滤掉了");
    }

    /// F296:会话全是 SFTP 时,第二列的空态说「还没有会话」,不说「没有匹配的
    /// 会话」—— 用户根本没在搜索,后一句是假的。
    ///
    /// 自证会变红:把空态判据改回 `cx.lists.sessions.is_empty()`。
    #[test]
    fn only_sftp_sessions_reads_as_no_sessions_not_as_no_match() {
        let mut sftp = sess(5, "文件机");
        sftp.connection.protocol = mullion_store::Protocol::Sftp;
        let texts = texts_full(&[], &[sftp], &[], "");
        assert!(texts.iter().any(|s| s == "还没有会话"), "空态文案不对:{texts:?}");
        assert!(!texts.iter().any(|s| s == "没有匹配的会话"), "没在搜索却说没匹配:{texts:?}");
    }
```

- [ ] **Step 2: 跑,确认红**

Run: `cargo test -p mullion-app --lib launcher::tests::the_launcher_session_column_leaves_sftp_sessions_out launcher::tests::only_sftp_sessions_reads_as_no_sessions_not_as_no_match > /tmp/t.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/t.log`
Expected: 两条 FAIL(第一条 `shown == [9, 5]`,第二条文案是「没有匹配的会话」)。
若 `mullion_store::Protocol` 路径不对,按 `grep -n "pub enum Protocol" crates/mullion-store/src/*.rs` 改。

- [ ] **Step 3: 实现**

`session_order` 改为:

```rust
pub fn session_order<'a>(
    sessions: &'a [SessionRecord],
    groups: &[mullion_store::GroupRecord],
    query: &str,
) -> Vec<&'a SessionRecord> {
    crate::ui::group_manager::group_sessions(groups, sessions)
        .into_iter()
        .flat_map(|(_, bucket)| bucket)
        // F296:SFTP 节点连上去没有终端,不属于「挑一台机器开终端」这一列。
        .filter(|r| r.connection.protocol != mullion_store::Protocol::Sftp)
        .filter(|r| crate::ui::session_manager::list::matches(r, query))
        .collect()
}
```

同时更新 `session_order` 文档注释末段「与会话管理器唯一的差别是不按协议分页」→ 改成「与会话管理器的差别:不按协议分页,且不含 SFTP 会话(F296)」。

`sessions_column` 空态判据改为「过滤掉 SFTP 之后还剩不剩」:

```rust
        if rows.is_empty() {
            // F296:判据看「去掉 SFTP 之后」—— 全是 SFTP 时用户没在搜索,
            // 说「没有匹配」是假的。
            let none_at_all = session_order(cx.lists.sessions, cx.lists.groups, "").is_empty();
            ui.label(crate::theme::hint_text(
                t,
                if none_at_all {
                    "还没有会话"
                } else {
                    "没有匹配的会话"
                },
            ));
        }
```

- [ ] **Step 4: 跑,确认绿;再跑整个 launcher 模块**

Run: `cargo test -p mullion-app --lib launcher:: > /tmp/t.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/t.log`
Expected: 全过。

- [ ] **Step 5: 变异自证**(先 commit 再变异)

```bash
git add crates/mullion-app/src/ui/launcher.rs
git commit -m "feat(app): 启动页会话列不显示 SFTP 会话 (F296)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```
变异①:删掉 `.filter(|r| r.connection.protocol != …)` → 第一条红;还原。
变异②:空态判据换回 `cx.lists.sessions.is_empty()` → 第二条红;还原(`git diff` 应为空)。

---

## Task 2: F300 书签下拉不折行、按像素中段省略

**Files:**
- Modify: `crates/mullion-app/src/ui/files_panel.rs`(书签菜单闭包,约 1011-1030 行;新增 `fit_middle` 纯函数挨着 `elide`/`truncate_to_width`,约 2185 行附近)
- Test: 同文件 `mod tests`

- [ ] **Step 1: 写失败测试(两帧「先短后长」)**

在 `files_panel.rs` 的 `mod tests` 里,挨着 `open_bookmark_menu_and_find` 加一个侧栏宿主的跑帧函数和两条测试:

```rust
    /// 侧栏形态跑一帧远端栏:2560×1365 屏,远端栏装在 `SidePanel` 里 ——
    /// 与用户实报的现场同一个宿主。`CentralPanel` 上首帧预算是整块屏宽,
    /// 棘轮复现不出来。
    fn run_remote_sidebar(
        ctx: &egui::Context,
        state: &mut PaneState,
        cols: &mut ColWidths,
        bookmarks: &[mullion_store::Bookmark],
        mut input: egui::RawInput,
    ) -> Vec<egui::epaint::ClippedShape> {
        input.screen_rect = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(2560.0, 1365.0),
        ));
        let t = crate::theme::MULLION_DARK;
        ctx.run(input, |ctx| {
            egui::SidePanel::right("f300_side")
                .exact_width(420.0)
                .show(ctx, |ui| {
                    show(
                        ui,
                        &t,
                        "远端",
                        1,
                        PanelColumn::Remote,
                        state,
                        false,
                        BookmarkView {
                            list: bookmarks,
                            can_edit: true,
                            home: None,
                        },
                        0,
                        cols,
                        None,
                        None,
                        None,
                    );
                });
        })
        .shapes
    }

    fn galley_of(
        shapes: &[egui::epaint::ClippedShape],
        needle: &str,
    ) -> Option<std::sync::Arc<egui::Galley>> {
        fn walk(s: &egui::Shape, needle: &str) -> Option<std::sync::Arc<egui::Galley>> {
            match s {
                egui::Shape::Vec(v) => v.iter().find_map(|s| walk(s, needle)),
                egui::Shape::Text(ts) if ts.galley.text().contains(needle) => {
                    Some(ts.galley.clone())
                }
                _ => None,
            }
        }
        shapes.iter().find_map(|cs| walk(&cs.shape, needle))
    }

    fn bm(path: &str) -> mullion_store::Bookmark {
        mullion_store::Bookmark {
            name: String::new(),
            path: path.into(),
        }
    }

    /// F300:书签下拉的每一项**一行**画完。
    ///
    /// 根因是 egui `Area` 的尺寸棘轮(F259/F263 同形):菜单记住上一帧内容宽,
    /// 上一帧只有一条 `/` 时,下一帧换成长路径,按钮被压成 11 点宽、19 行的
    /// 竖条(v0.1.118 无头复现)。所以判据**必须**是「先短后长」:单画一条长的,
    /// 首帧预算够宽,永远是绿的。
    ///
    /// 自证会变红:删掉菜单闭包里的 `ui.set_max_width(..)`。
    #[test]
    fn a_long_bookmark_after_a_short_one_still_fits_on_one_line() {
        let ctx = egui::Context::default();
        annotate::toggle(&ctx);
        let mut state = ready_at(b"/");
        let mut cols = ColWidths::default();
        let short = vec![bm("/")];
        let long_path = "/home/brain/.claude/projects/-data-Mullion";
        let long = vec![bm(long_path)];
        run_remote_sidebar(&ctx, &mut state, &mut cols, &short, egui::RawInput::default());
        let arrow = annotate::spot_rect(&ctx, "文件面板/远端/路径/书签")
            .expect("有书签时该画下拉按钮")
            .center();
        run_remote_sidebar(&ctx, &mut state, &mut cols, &short, click_at(arrow));
        // 菜单开着、画了一帧短的,棘轮记下了窄宽度。
        run_remote_sidebar(&ctx, &mut state, &mut cols, &short, egui::RawInput::default());
        let mut g = None;
        for _ in 0..3 {
            let shapes =
                run_remote_sidebar(&ctx, &mut state, &mut cols, &long, egui::RawInput::default());
            if let Some(found) = galley_of(&shapes, long_path) {
                g = Some(found);
            }
        }
        let g = g.expect("长书签没画出来(菜单关了?)");
        assert_eq!(g.rows.len(), 1, "长路径被折成 {} 行", g.rows.len());
    }

    /// F300:比 60% 屏宽还长的路径**中段省略**且仍是一行,而不是被硬裁或折行。
    ///
    /// 自证会变红:把 `fit_middle` 的调用换成原样 `b.path.as_str()`
    /// (galley 里出现完整路径、宽度超过上限)。
    #[test]
    fn a_path_longer_than_the_cap_is_middle_elided_on_one_line() {
        let ctx = egui::Context::default();
        annotate::toggle(&ctx);
        let mut state = ready_at(b"/");
        let mut cols = ColWidths::default();
        let seg = "abcdefghij".repeat(40); // 400 字符,远超 2560×0.6
        let huge = format!("/head/{seg}/tail.txt");
        let list = vec![bm(&huge)];
        run_remote_sidebar(&ctx, &mut state, &mut cols, &list, egui::RawInput::default());
        let arrow = annotate::spot_rect(&ctx, "文件面板/远端/路径/书签").unwrap().center();
        run_remote_sidebar(&ctx, &mut state, &mut cols, &list, click_at(arrow));
        let mut g = None;
        for _ in 0..3 {
            let shapes =
                run_remote_sidebar(&ctx, &mut state, &mut cols, &list, egui::RawInput::default());
            if let Some(found) = galley_of(&shapes, "/head/") {
                g = Some(found);
            }
        }
        let g = g.expect("超长书签没画出来");
        assert_eq!(g.rows.len(), 1, "超长路径折行了");
        assert!(g.text().contains('…'), "超长路径没省略:{}", g.text());
        assert!(g.text().ends_with("tail.txt"), "省略把尾部吃掉了:{}", g.text());
        assert!(g.size().x <= 2560.0 * 0.6, "宽度 {} 超过 60% 屏宽", g.size().x);
    }
```

同时在 `mod tests` 里给 `fit_middle` 加纯函数测试:

```rust
    /// F300:`fit_middle` 放得下就原样,放不下就中段省略且不超预算。
    ///
    /// 自证会变红:让 `fit_middle` 恒返回原串。
    #[test]
    fn fit_middle_keeps_short_text_and_elides_long_text_within_budget() {
        let w = |s: &str| s.chars().count() as f32 * 7.0; // 等宽假度量
        assert_eq!(fit_middle("/srv/api", 100.0, w), "/srv/api");
        let long = "/home/brain/projects/very/deep/tree/file.rs";
        let out = fit_middle(long, 140.0, w);
        assert!(out.contains('…'), "{out}");
        assert!(w(&out) <= 140.0, "{out} 超预算");
        assert!(out.starts_with("/home"), "{out}");
        assert!(out.ends_with("file.rs"), "{out}");
    }
```

- [ ] **Step 2: 跑,确认红**

Run: `cargo test -p mullion-app --lib files_panel::tests::a_long_bookmark_after files_panel::tests::a_path_longer files_panel::tests::fit_middle > /tmp/t.log 2>&1; grep -nE "test result|FAILED|panicked|error\[" /tmp/t.log`
Expected: 编译失败(`home` 字段、`fit_middle` 不存在)。先临时在 `BookmarkView` 加字段(见 Task 3 Step 3 的定义——F300 与 F301 共用这个字段,**在本任务里先把字段加上**,所有 `BookmarkView {..}` 字面量补 `home: None`,`BookmarkView::none()` 里补 `home: None`,生产的三处调用点也先填 `None`,Task 3 再接真值),再跑:第一条 FAIL(`rows.len() == 19` 左右),第二条 FAIL,第三条编译失败。

- [ ] **Step 3: 实现 `fit_middle`**

挨着 `truncate_to_width` 加:

```rust
/// F300:按**像素**把 `s` 中段省略到 `max_w` 以内。放得下原样返回。
///
/// 省略本身交给 `reveal::elide_middle`(按字符预算,保头、尾留约 2/3 ——
/// 路径的尾部是认路径的关键);这里只二分出「多少个字符恰好放得下」。
/// 度量由调用方给(`measure`),纯函数、可脱离 egui 单测。
pub(crate) fn fit_middle(s: &str, max_w: f32, measure: impl Fn(&str) -> f32) -> String {
    if measure(s) <= max_w {
        return s.to_owned();
    }
    let n = s.chars().count();
    let (mut lo, mut hi) = (1usize, n);
    let mut best = crate::files::reveal::elide_middle(s, 1);
    while lo <= hi {
        let mid = (lo + hi) / 2;
        let cand = crate::files::reveal::elide_middle(s, mid);
        if measure(&cand) <= max_w {
            best = cand;
            lo = mid + 1;
        } else {
            hi = mid - 1;
        }
    }
    best
}
```

实现前先 `sed -n 320,370p crates/mullion-app/src/files/reveal.rs` 核 `elide_middle` 的签名与可见性(若是私有,改成 `pub(crate)`),以及 `budget_chars` 太小时它的返回值(要保证 `mid=1` 不 panic)。

- [ ] **Step 4: 改菜单闭包**

把书签菜单闭包改为:

```rust
            let menu = egui::menu::menu_custom_button(ui, btn, |ui| {
                // F300:菜单宽度按内容走、封顶 60% 屏宽。**这一句是切断 Area
                // 尺寸棘轮的地方**(F259/F263 同形):egui 把菜单 Area 上一帧的
                // 内容宽记成这一帧的预算,上一帧是一条 `/`,下一帧的长路径就被
                // 压成 11 点宽的竖条。`set_max_width` 直接改 `max_rect.max`,
                // 能把预算改大。守护:`a_long_bookmark_after_a_short_one_still_
                // fits_on_one_line`(必须是先短后长两帧)。
                let cap = ui.ctx().screen_rect().width() * 0.6;
                ui.set_max_width(cap);
                let font = egui::TextStyle::Button.resolve(ui.style());
                let pad = ui.spacing().button_padding.x * 2.0;
                for b in bookmarks.list {
                    // F145:主文本恒是完整路径;F300:放不下时中段省略,
                    // 完整路径挪到 hover。
                    let shown = fit_middle(&b.path, cap - pad, |s| {
                        ui.fonts(|f| {
                            f.layout_no_wrap(s.to_owned(), font.clone(), egui::Color32::WHITE)
                                .size()
                                .x
                        })
                    });
                    let elided = shown != b.path;
                    let mut item = ui.add(
                        egui::Button::new(shown).wrap_mode(egui::TextWrapMode::Extend),
                    );
                    let named = !b.name.is_empty() && b.name != b.path;
                    let hover = match (elided, named) {
                        (true, true) => Some(format!("{}\n{}", b.path, b.name)),
                        (true, false) => Some(b.path.clone()),
                        (false, true) => Some(b.name.clone()),
                        (false, false) => None,
                    };
                    if let Some(h) = hover {
                        item = item.on_hover_text(h);
                    }
                    if item.clicked() {
                        action = Some(FileAction::Goto(mullion_ssh::sftp::RemotePath::from_bytes(
                            b.path.as_bytes().to_vec(),
                        )));
                        ui.close_menu();
                    }
                }
            });
```

(点击分支 Task 3 还会改,这里保持原语义。)

- [ ] **Step 5: 跑,确认绿;跑整个 files_panel 模块**

Run: `cargo test -p mullion-app --lib files_panel:: > /tmp/t.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/t.log`
Expected: 全过。既有书签测试(`open_bookmark_menu_and_find` 那几条)用 `find_text_pos` 按子串找,短路径不省略,应不受影响;若红,读失败信息再定,不改断言。

- [ ] **Step 6: commit + 变异自证**

```bash
git add crates/mullion-app/src/ui/files_panel.rs crates/mullion-app/src/files/reveal.rs
git commit -m "fix(app): 书签下拉切断 Area 宽度棘轮,长路径一行显示、超 60% 屏宽中段省略 (F300)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```
变异①:删 `ui.set_max_width(cap);` → 第一条红。变异②:`fit_middle(..)` 换成 `b.path.clone()` → 第二条红。变异③:`fit_middle` 首行改成 `return s.to_owned();` → 纯函数那条红。每次还原后 `git diff` 为空。

---

## Task 3: F301 书签支持 `~`

**Files:**
- Modify: `crates/mullion-app/src/ui/files_panel.rs:148-170`(`BookmarkView`)、路径条 ★ 判定与 ☆ 点击(约 978-1003)、书签点击(菜单闭包)、`sidebar`/`content` 签名与四处 `BookmarkView {..}`
- Modify: `crates/mullion-app/src/files/local.rs:157-200`
- Modify: `crates/mullion-app/src/ui/mod.rs`(`UiFrame.remote_home`、两处调用)
- Modify: `crates/mullion-app/src/app.rs`(构造 UiFrame 处约 13960)
- Test: `files_panel.rs`、`local.rs` 的 `mod tests`

- [ ] **Step 1: 写失败测试(面板侧,纯 egui 无 App)**

`files_panel.rs` `mod tests`:

```rust
    fn run_remote_home(
        ctx: &egui::Context,
        state: &mut PaneState,
        bookmarks: &[mullion_store::Bookmark],
        home: Option<&[u8]>,
        input: egui::RawInput,
    ) -> (Option<FileAction>, Vec<egui::epaint::ClippedShape>) {
        let t = crate::theme::MULLION_DARK;
        let mut cols = ColWidths::default();
        let mut action = None;
        let out = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                action = show(
                    ui, &t, "远端", 1, PanelColumn::Remote, state, false,
                    BookmarkView { list: bookmarks, can_edit: true, home },
                    0, &mut cols, None, None, None,
                );
            });
        });
        (action, out.shapes)
    }

    /// F301:书签 `~` 在主目录下亮实心 ★ —— 判定先解析再比。
    ///
    /// 自证会变红:把 ★ 判定改回 `b.path == path` 字面比较。
    #[test]
    fn a_tilde_bookmark_lights_the_star_when_we_are_at_home() {
        let ctx = egui::Context::default();
        let mut state = ready_at(b"/home/brain");
        let marks = vec![bm("~")];
        let (_, shapes) =
            run_remote_home(&ctx, &mut state, &marks, Some(b"/home/brain"), egui::RawInput::default());
        assert!(find_text_pos(&shapes, "★").is_some(), "在主目录下 `~` 书签没点亮 ★");
        // 反例:不在主目录时不亮 —— 否则「恒亮」也是绿的。
        let mut elsewhere = ready_at(b"/var/log");
        let (_, shapes) =
            run_remote_home(&ctx, &mut elsewhere, &marks, Some(b"/home/brain"), egui::RawInput::default());
        assert!(find_text_pos(&shapes, "☆").is_some(), "不在主目录也亮了 ★");
    }

    /// F301:在主目录点 ☆ 存成 `~`,子目录仍存绝对路径。
    ///
    /// 自证会变红:删掉「cwd == home → 存 `~`」那个分支。
    #[test]
    fn starring_home_saves_a_tilde_and_a_subdirectory_saves_an_absolute_path() {
        for (cwd, want) in [(&b"/home/brain"[..], "~"), (&b"/home/brain/src"[..], "/home/brain/src")] {
            let ctx = egui::Context::default();
            let mut state = ready_at(cwd);
            let (_, shapes) =
                run_remote_home(&ctx, &mut state, &[], Some(b"/home/brain"), egui::RawInput::default());
            let star = find_text_pos(&shapes, "☆").expect("该画 ☆");
            let (a, _) = run_remote_home(&ctx, &mut state, &[], Some(b"/home/brain"), click_at(star));
            match a {
                Some(FileAction::BookmarkAdd { path, .. }) => assert_eq!(path, want),
                other => panic!("点 ☆ 没发 BookmarkAdd:{other:?}"),
            }
        }
    }

    /// F301:取消收藏时传**那条书签的原始路径** `~`,不是解析后的绝对路径 ——
    /// 否则 store 里按路径找不到,★ 永远取消不掉。
    ///
    /// 自证会变红:`BookmarkRemove { path: path.clone() }`(传 cwd)。
    #[test]
    fn unstarring_a_tilde_bookmark_removes_it_by_its_raw_path() {
        let ctx = egui::Context::default();
        let mut state = ready_at(b"/home/brain");
        let marks = vec![bm("~")];
        let (_, shapes) =
            run_remote_home(&ctx, &mut state, &marks, Some(b"/home/brain"), egui::RawInput::default());
        let star = find_text_pos(&shapes, "★").expect("该画 ★");
        let (a, _) = run_remote_home(&ctx, &mut state, &marks, Some(b"/home/brain"), click_at(star));
        assert_eq!(a, Some(FileAction::BookmarkRemove { path: "~".into() }));
    }

    /// F301:点下拉里的 `~` 书签发 `GotoInput("~")`,交给 App 侧按这一栏
    /// 的主目录解析(两栏各一条既有通路);发 `Goto("~")` 的话会去列一个
    /// 名叫 `~` 的相对目录。
    ///
    /// 自证会变红:把书签点击改回无条件 `FileAction::Goto(..)`。
    #[test]
    fn clicking_a_tilde_bookmark_goes_through_the_input_resolver() {
        let ctx = egui::Context::default();
        let mut state = ready_at(b"/var");
        let mut cols = ColWidths::default();
        let marks = vec![bm("~/src")];
        let pos = open_bookmark_menu_and_find(&ctx, &mut state, &mut cols, &marks, "~/src")
            .expect("下拉里该有 ~/src");
        let (a, _) = run_remote(&ctx, &mut state, &mut cols, &marks, true, click_at(pos));
        assert_eq!(a, Some(FileAction::GotoInput("~/src".into())));
    }
```

`local.rs` `mod tests`:

```rust
    /// F301:默认本地目录配成 `~` / `~/x` 时展开到主目录。原样返回的话,
    /// 面板会去开一个名叫 `~` 的相对目录。
    ///
    /// 自证会变红:删掉 `pick_default_local` 里对 `configured` 的 `~` 展开。
    #[test]
    fn a_configured_tilde_expands_to_home() {
        let home = RemotePath::from_bytes(b"/home/u".to_vec());
        let cwd = RemotePath::from_bytes(b"/tmp".to_vec());
        assert_eq!(
            pick_default_local(Some("~"), None, false, Some(home.clone()), cwd.clone()),
            home
        );
        let sub = pick_default_local(Some("~/work"), None, false, Some(home.clone()), cwd);
        assert!(sub.display().ends_with("work"), "{}", sub.display());
        assert!(sub.display().starts_with("/home/u"), "{}", sub.display());
    }
```

(若 `FileAction` 没派生 `PartialEq`,用 `matches!` 改写断言;先 `grep -n "enum FileAction" -B3 crates/mullion-app/src/ui/files_panel.rs` 看派生。)

- [ ] **Step 2: 跑,确认红**

Run: `cargo test -p mullion-app --lib tilde local::tests::a_configured > /tmp/t.log 2>&1; grep -nE "test result|FAILED|panicked|error\[" /tmp/t.log`
Expected: 5 条 FAIL。

- [ ] **Step 3: 实现面板侧**

`BookmarkView` 加字段(Task 2 已加,这里补文档):

```rust
    /// F301:**远端栏**的登录目录(SFTP `canonicalize(".")` 的结果,见
    /// `TabContent::sftp_home`)。书签 `~` / `~/…` 靠它解析,★ 判定与
    /// 「在主目录收藏存成 `~`」都要它。`None` = 还不知道(sftp 没开好)或
    /// 这是本地栏 —— 本地栏的主目录走 `local::home_dir_cached()`,不经这里。
    pub home: Option<&'a [u8]>,
```

新增纯函数(放在 `bookmark_default_name` 旁边):

```rust
/// F301:这条书签此刻指向哪里。只对 `~` 开头的解析;其余原样 —— 字面比较
/// 是既有行为,解析普通路径会把 `/a/../b` 这类写法悄悄规整掉,改变 ★ 语义。
/// 解析不出来(`~` 但主目录未知)= `None`,★ 判定当作不匹配。
fn bookmark_target(
    column: PanelColumn,
    raw: &str,
    cwd: &mullion_ssh::sftp::RemotePath,
    remote_home: Option<&[u8]>,
) -> Option<String> {
    if !raw.starts_with('~') {
        return Some(raw.to_owned());
    }
    let p = match column {
        PanelColumn::Remote => crate::files::path_input::resolve_remote_input(raw, cwd, remote_home),
        PanelColumn::Local => crate::files::path_input::resolve_local_input(
            raw,
            cwd,
            crate::files::local::home_dir_cached(),
        ),
    }?;
    Some(p.display().to_string())
}

/// F301:这一栏的主目录(显示形),用于「在主目录收藏存成 `~`」。
fn column_home(column: PanelColumn, remote_home: Option<&[u8]>) -> Option<String> {
    match column {
        PanelColumn::Remote => remote_home.map(|h| String::from_utf8_lossy(h).into_owned()),
        PanelColumn::Local => crate::files::local::home_dir_cached().map(|h| h.display().to_string()),
    }
}
```

路径条 ★ 段改为:

```rust
        // F301:先解析再比 —— `~` 书签在主目录下要亮。命中的那条留着,
        // 取消收藏要按它的**原始路径**删(传 cwd 的话 store 里找不到 `~`)。
        let hit_mark = bookmarks.list.iter().find(|b| {
            bookmark_target(column, &b.path, &state.cwd, bookmarks.home).as_deref()
                == Some(path.as_str())
        });
        let starred = hit_mark.is_some();
```

☆ 点击分支改为:

```rust
            if hit.clicked() {
                action = Some(match hit_mark {
                    Some(b) => FileAction::BookmarkRemove { path: b.path.clone() },
                    None => {
                        // F301:正在主目录 → 存 `~`,换机器/换用户也指得对;
                        // 子目录照旧存绝对路径(用户确认的取舍)。
                        let at_home = column_home(column, bookmarks.home).as_deref() == Some(path.as_str());
                        let saved = if at_home { "~".to_owned() } else { path.clone() };
                        FileAction::BookmarkAdd {
                            name: bookmark_default_name(&path),
                            path: saved,
                        }
                    }
                });
            }
```

菜单闭包里点击分支改为:

```rust
                    if item.clicked() {
                        // F301:`~` 书签交给 App 侧按这一栏的主目录解析
                        // (`GotoInput` 两栏各有一条既有通路)。
                        action = Some(if b.path.starts_with('~') {
                            FileAction::GotoInput(b.path.clone())
                        } else {
                            FileAction::Goto(mullion_ssh::sftp::RemotePath::from_bytes(
                                b.path.as_bytes().to_vec(),
                            ))
                        });
                        ui.close_menu();
                    }
```

注意 `hit_mark` 借着 `bookmarks.list`,闭包里 `action` 可变借用 —— 两者不冲突(`bookmarks` 是 `Copy` 视图)。若借用检查报错,先把 `hit_mark` 映射成 `Option<String>`(原始路径)再用。

- [ ] **Step 4: 实现 `local.rs`**

```rust
/// F301:本机主目录,进程内只查一次。面板每帧都要问(★ 判定),
/// 每帧一次 `BaseDirs::new()` 是 T3 那类每帧系统调用。
pub fn home_dir_cached() -> Option<&'static RemotePath> {
    static HOME: std::sync::OnceLock<Option<RemotePath>> = std::sync::OnceLock::new();
    HOME.get_or_init(home_dir).as_ref()
}
```

`pick_default_local` 的 `configured` 与 `bookmark` 两支都经 `~` 展开:

```rust
    // F301:配置里写 `~` / `~/…` 也要能用 —— 与路径条同一个解析函数。
    let expand = |s: &str| {
        crate::files::path_input::resolve_local_input(s, &cwd, home.as_ref())
            .unwrap_or_else(|| RemotePath::from_bytes(s.as_bytes().to_vec()))
    };
    if let Some(s) = configured.filter(usable) {
        return expand(s);
    }
    if let Some(s) = bookmark.filter(usable).filter(|_| bookmark_exists) {
        return expand(s);
    }
    home.unwrap_or(cwd)
```

注意:`resolve_local_input` 对非 `~` 的相对路径会拼 cwd —— 与旧行为(原样返回)不同。为不改变旧语义,`expand` 只在 `s.trim_start().starts_with('~')` 时调解析,否则原样。`default_local` 里 `bookmark_exists` 的 `is_dir()` 检查也要用展开后的路径:把展开提到 `default_local` 里先算 `home_dir_cached()`,对书签首条展开后再 `is_dir()`。

- [ ] **Step 5: 接真值(App → UiFrame → sidebar/content)**

1. `ui/mod.rs` `UiFrame` 加字段(紧挨 `pane_cwd`):

```rust
    /// F301:活动标签 sftp 的登录目录。远端书签 `~` 靠它解析。每帧现取,
    /// 不存 —— 理由同 `pane_cwd`。
    pub remote_home: Option<&'a [u8]>,
```
`base_frame()` 里补 `remote_home: None`。

2. `files_panel::sidebar` / `content` 各加一个参数 `remote_home: Option<&[u8]>`,远端栏那处 `BookmarkView { list: &frame.bookmarks, can_edit: frame.session_bound, home: remote_home }`;本地栏两处 `home: None`(本地走缓存)。`ui/mod.rs:965`、`:1226` 两处调用传 `frame.remote_home`。测试里调 `sidebar`/`content` 的地方补 `None`(`grep -n "sidebar(\|content(" crates/mullion-app/src/ui/files_panel.rs` 找齐)。

3. `app.rs` 构造 UiFrame 处(`let pane_cwd = self` 那段,约 13960 行)旁边:

```rust
                            // F301:远端书签 `~` 的解析基准。
                            let remote_home = self.tabs.active().and_then(|t| t.content.sftp_home());
```
UiFrame 字面量补 `remote_home: remote_home.as_deref(),`。`self.tabs.active()` 的实际方法名先 `grep -n "fn active" crates/mullion-app/src/shell/tabs.rs` 核。

- [ ] **Step 6: 接线守护(源码切片)**

`app.rs` 的测试模块里,仿照既有 `pane_cwd` 相关源码切片测试(`grep -n "pane_cwd: pane_cwd.as_deref()" crates/mullion-app/src/app.rs` 找邻近写法与 `tests/common` 剪刀):

```rust
    /// F301 接线:远端书签 `~` 的主目录必须真的流进 UiFrame —— 纯函数测得
    /// 再扎实,这里传 `None` 的话 `~` 书签在远端栏永远解析不出来,★ 永远不亮,
    /// 画面上只是「一个不起作用的书签」,零报错。
    ///
    /// 自证会变红:把 `remote_home: remote_home.as_deref(),` 改成 `remote_home: None,`。
    #[test]
    fn the_remote_home_reaches_the_ui_frame() {
        let src = crate::test_src::production_app_rs(); // 用本文件既有的剥注释剪刀
        assert!(src.contains("remote_home: remote_home.as_deref(),"));
        assert!(src.contains("t.content.sftp_home()"));
    }
```

`crate::test_src::production_app_rs()` 只是占位名 —— 实现时用 app.rs 测试里**已经在用的**那个「剥掉注释与测试模块的生产源码」取法(`grep -n "fn prod_src\|fn production\|strip_comments" crates/mullion-app/src/app.rs crates/mullion-app/tests/common/mod.rs`),不要另写一把剪刀。

- [ ] **Step 7: 跑绿**

Run: `cargo test -p mullion-app > /tmp/t.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/t.log`
Expected: 全过。

- [ ] **Step 8: commit + 变异自证**

```bash
git add -A crates/mullion-app
git commit -m "feat(app): 书签支持 ~ 与 ~/…,按所在栏的主目录解析,主目录收藏存 ~ (F301)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```
逐条按测试注释里写的变异跑一遍,确认每条都红、还原后 `git diff` 为空。

---

## Task 4: F299 编辑器查找 —— 纯逻辑

**Files:**
- Create: `crates/mullion-app/src/ui/editor_find.rs`
- Modify: `crates/mullion-app/src/ui/mod.rs`(`pub mod editor_find;`)

- [ ] **Step 1: 写模块与失败测试**

```rust
//! F299:内置编辑器的查找逻辑。零 egui,纯函数。
//!
//! 位置一律是**字符下标**(不是字节):egui 的 `CCursor` 按字符计,
//! 给字节下标的话中文文件里第一处匹配之后全部错位。

/// 在 `text` 里找 `query` 的全部非重叠出现,返回 `[起, 止)` 字符区间。
/// `query` 空 = 没有匹配。`case` 为假时逐字符小写折叠后比较。
pub fn find_all(text: &str, query: &str, case: bool) -> Vec<(usize, usize)> {
    if query.is_empty() {
        return Vec::new();
    }
    let fold = |c: char| -> char {
        if case {
            c
        } else {
            // 只取小写映射的第一个字符:个别字符(如 'İ')小写后是两个字符,
            // 展开会让字符下标和原文对不上。
            c.to_lowercase().next().unwrap_or(c)
        }
    };
    let hay: Vec<char> = text.chars().map(fold).collect();
    let needle: Vec<char> = query.chars().map(fold).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        if hay[i..i + needle.len()] == needle[..] {
            out.push((i, i + needle.len()));
            i += needle.len();
        } else {
            i += 1;
        }
    }
    out
}

/// 从光标 `at`(字符下标)出发第一处 `起点 >= at` 的匹配;没有就回绕到第一处。
pub fn first_at_or_after(hits: &[(usize, usize)], at: usize) -> Option<usize> {
    if hits.is_empty() {
        return None;
    }
    Some(hits.iter().position(|h| h.0 >= at).unwrap_or(0))
}

/// 下一个 / 上一个,首尾回绕。
pub fn step(len: usize, cur: usize, forward: bool) -> usize {
    if len == 0 {
        return 0;
    }
    if forward {
        (cur + 1) % len
    } else {
        (cur + len - 1) % len
    }
}

/// `3/17` 这样的计数;没有匹配时 `0/0`。
pub fn counter(cur: Option<usize>, len: usize) -> String {
    match cur {
        Some(i) if len > 0 => format!("{}/{}", i + 1, len),
        _ => format!("0/{len}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 字符下标而非字节:中文在前,匹配位置要按字符算。
    /// 自证会变红:把 `hay` 改成按字节(`text.bytes()`)建。
    #[test]
    fn positions_are_char_indices_even_after_cjk() {
        assert_eq!(find_all("中文abc", "abc", true), vec![(2, 5)]);
    }

    /// 默认不区分大小写;区分时只命中同形。
    /// 自证会变红:让 `fold` 恒返回原字符。
    #[test]
    fn case_folding_is_off_by_default_and_on_when_asked() {
        assert_eq!(find_all("Foo foo FOO", "foo", false).len(), 3);
        assert_eq!(find_all("Foo foo FOO", "foo", true), vec![(4, 7)]);
    }

    #[test]
    fn matches_do_not_overlap_and_empty_query_matches_nothing() {
        assert_eq!(find_all("aaaa", "aa", true), vec![(0, 2), (2, 4)]);
        assert!(find_all("abc", "", false).is_empty());
    }

    /// 首尾回绕。自证会变红:去掉 `% len`。
    #[test]
    fn stepping_wraps_both_ways() {
        assert_eq!(step(3, 2, true), 0);
        assert_eq!(step(3, 0, false), 2);
        assert_eq!(step(0, 0, true), 0);
    }

    #[test]
    fn first_hit_from_the_caret_wraps_to_the_top() {
        let hits = [(1, 2), (5, 6)];
        assert_eq!(first_at_or_after(&hits, 0), Some(0));
        assert_eq!(first_at_or_after(&hits, 3), Some(1));
        assert_eq!(first_at_or_after(&hits, 9), Some(0), "越过最后一处要回到第一处");
        assert_eq!(first_at_or_after(&[], 0), None);
    }

    #[test]
    fn the_counter_reads_one_based() {
        assert_eq!(counter(Some(2), 17), "3/17");
        assert_eq!(counter(None, 0), "0/0");
    }
}
```

- [ ] **Step 2: 跑**

Run: `cargo test -p mullion-app --lib editor_find:: > /tmp/t.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/t.log`
Expected: 全过(纯函数先写实现一起落,变异自证代替红→绿)。

- [ ] **Step 3: commit + 变异**

```bash
git add crates/mullion-app/src/ui/editor_find.rs crates/mullion-app/src/ui/mod.rs
git commit -m "feat(app): 编辑器查找的纯逻辑(字符下标/大小写折叠/回绕) (F299)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```
按每条测试注释变异,确认红后还原。

---

## Task 5: F299 编辑器查找 —— 查找条与 overlay

**Files:**
- Modify: `crates/mullion-app/src/ui/editor_window.rs`(`EditorState` 加 `find`;`show()` 里按键、标题栏按钮、查找条、正文 `.show()` + overlay)
- Test: 同文件 `mod tests`

- [ ] **Step 1: 状态结构**

`EditorState` 加字段(构造处 `new` 里 `find: None`):

```rust
    /// F299:查找条。`None` = 没开。
    pub find: Option<Find>,
```

同文件新增:

```rust
/// F299:查找条状态。命中表是缓存 —— 键是 (查询, 大小写, 正文版本),
/// 任何一样变了就重算;不缓存的话 368 KiB 的文件每帧全文扫一遍(T3)。
pub struct Find {
    pub query: String,
    pub case: bool,
    /// 当前是第几处(`hits` 的下标)。
    pub cur: Option<usize>,
    hits: Vec<(usize, usize)>,
    /// 算 `hits` 时的 (query, case, text_rev)。
    key: Option<(String, bool, u64)>,
    /// 这一帧要把当前匹配滚进视野(当前匹配变了才滚,不然用户没法手动滚走)。
    scroll: bool,
    /// 首帧把焦点给查找框。
    focus: bool,
}
```

`EditorState` 再加 `text_rev: u64`(正文每改一次 +1,在 `TextEdit` 的 `response.changed()` 时递增)。

- [ ] **Step 2: 写失败测试**

先看既有测试怎么驱动编辑器:`grep -n "fn run\|fn frame\|editor_window::show\|show(&ctx" crates/mullion-app/src/ui/editor_window.rs | head`,复用那个跑帧辅助(下面记作 `run_editor(ctx, state, input) -> (Option<EditorAction>, shapes)`;没有就按下面签名新写一个,只调 `show(ctx, &MULLION_DARK, state)`)。

```rust
    fn key(k: egui::Key, mods: egui::Modifiers) -> egui::RawInput {
        let mut i = egui::RawInput::default();
        for pressed in [true, false] {
            i.events.push(egui::Event::Key {
                key: k,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: mods,
            });
        }
        i.modifiers = mods;
        i
    }

    fn typed(s: &str) -> egui::RawInput {
        let mut i = egui::RawInput::default();
        i.events.push(egui::Event::Text(s.into()));
        i
    }

    /// F299:Ctrl+F 开条、打字即跳到第一处、Enter 到下一处、首尾回绕。
    ///
    /// 自证会变红:删掉 `consume_key(COMMAND, F)` 那一段(条开不出来);
    /// 或删掉 Enter → `step(.., true)` 那一句(停在第一处)。
    #[test]
    fn ctrl_f_opens_the_bar_typing_jumps_and_enter_steps_with_wraparound() {
        let ctx = egui::Context::default();
        let mut st = Some(editor_with("foo\nbar foo\nFOO"));
        for _ in 0..2 {
            run_editor(&ctx, &mut st, egui::RawInput::default());
        }
        run_editor(&ctx, &mut st, key(egui::Key::F, egui::Modifiers::COMMAND));
        run_editor(&ctx, &mut st, egui::RawInput::default()); // 焦点落到查找框
        run_editor(&ctx, &mut st, typed("foo"));
        let f = st.as_ref().unwrap().find.as_ref().expect("Ctrl+F 没开出查找条");
        assert_eq!(f.cur, Some(0));
        assert_eq!(super::editor_find::counter(f.cur, 3), "1/3", "默认不区分大小写应是 3 处");
        for want in [1, 2, 0] {
            run_editor(&ctx, &mut st, key(egui::Key::Enter, egui::Modifiers::NONE));
            run_editor(&ctx, &mut st, egui::RawInput::default());
            assert_eq!(st.as_ref().unwrap().find.as_ref().unwrap().cur, Some(want));
        }
    }

    /// F299:Shift+F3 上一处(从第一处回绕到最后一处)。
    /// 自证会变红:把 Shift+F3 的方向写反。
    #[test]
    fn shift_f3_goes_backwards_and_wraps() {
        let ctx = egui::Context::default();
        let mut st = Some(editor_with("a x a x a"));
        run_editor(&ctx, &mut st, egui::RawInput::default());
        run_editor(&ctx, &mut st, key(egui::Key::F, egui::Modifiers::COMMAND));
        run_editor(&ctx, &mut st, egui::RawInput::default());
        run_editor(&ctx, &mut st, typed("a"));
        run_editor(&ctx, &mut st, key(egui::Key::F3, egui::Modifiers::SHIFT));
        assert_eq!(st.as_ref().unwrap().find.as_ref().unwrap().cur, Some(2));
    }

    /// F299:Esc 关条,并把正文选区落在当前匹配上(关条后用户直接接着改)。
    /// 自证会变红:删掉关条时写 `TextEditState` 选区那一段。
    #[test]
    fn escape_closes_the_bar_and_leaves_the_match_selected_in_the_body() {
        let ctx = egui::Context::default();
        let mut st = Some(editor_with("xx needle yy"));
        run_editor(&ctx, &mut st, egui::RawInput::default());
        run_editor(&ctx, &mut st, key(egui::Key::F, egui::Modifiers::COMMAND));
        run_editor(&ctx, &mut st, egui::RawInput::default());
        run_editor(&ctx, &mut st, typed("needle"));
        run_editor(&ctx, &mut st, key(egui::Key::Escape, egui::Modifiers::NONE));
        run_editor(&ctx, &mut st, egui::RawInput::default());
        assert!(st.as_ref().unwrap().find.is_none(), "Esc 没关掉查找条");
        let ts = egui::TextEdit::load_state(&ctx, body_id(&st.as_ref().unwrap().key))
            .expect("正文没有 TextEditState");
        let r = ts.cursor.char_range().expect("正文没有选区");
        assert_eq!((r.primary.index.min(r.secondary.index), r.primary.index.max(r.secondary.index)), (3, 9));
    }

    /// F299:当前匹配画一块高亮底(overlay)—— 查找框拿着焦点时 `TextEdit`
    /// 不画正文选区,不另画的话用户看不见跳到了哪。
    /// 自证会变红:删掉 overlay 的 `rect_filled`。
    #[test]
    fn the_current_match_is_painted_while_the_find_box_has_focus() {
        let ctx = egui::Context::default();
        let mut st = Some(editor_with("xx needle yy"));
        run_editor(&ctx, &mut st, egui::RawInput::default());
        run_editor(&ctx, &mut st, key(egui::Key::F, egui::Modifiers::COMMAND));
        run_editor(&ctx, &mut st, egui::RawInput::default());
        run_editor(&ctx, &mut st, typed("needle"));
        let (_, shapes) = run_editor(&ctx, &mut st, egui::RawInput::default());
        let want = crate::theme::c32(crate::theme::MULLION_DARK.find_hit());
        assert!(
            shapes_contain_fill(&shapes, want),
            "没画当前匹配的高亮底"
        );
    }
```

`editor_with(text)`:用 `EditorState::new(..)` 造一个可写的状态(看既有测试里怎么造的,照抄)。`body_id(key)`:正文 `TextEdit` 的显式 id —— 本任务要给正文 `.id(body_id(&s.key))`,原来没有显式 id 的话 `load_state` 拿不到。`shapes_contain_fill`:遍历 `Shape::Rect` 看 `fill == want`。高亮色:**不新增主题色**,用既有的 `t.accent.gamma_multiply(0.35)` 之类;测试里的 `want` 跟实现取同一个函数 `find_hit_color(t)`(定义在 editor_window.rs,`pub(crate)`),上面伪写的 `find_hit()` 以此为准。

- [ ] **Step 3: 跑,确认红**

Run: `cargo test -p mullion-app --lib editor_window::tests > /tmp/t.log 2>&1; grep -nE "test result|FAILED|panicked|error\[" /tmp/t.log`
Expected: 新 4 条 FAIL / 编译失败,既有全过。

- [ ] **Step 4: 实现**

4.1 按键(`show()` 开头,Ctrl+S 那段之后):

```rust
    // F299:查找。`consume_key` 同 Ctrl+S 的理由 —— 不取走的话 F 会被打进正文。
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::F)) {
        let seed = selected_single_line(ctx, s); // 当前正文选区(单行才预填)
        let f = s.find.get_or_insert_with(Find::default);
        if let Some(q) = seed {
            f.query = q;
        }
        f.focus = true;
    }
    if let Some(f) = s.find.as_mut() {
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, egui::Key::F3)) {
            f.go(false);
        } else if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F3)) {
            f.go(true);
        }
    }
```

`Find::go(forward)`:`if let Some(c) = self.cur { self.cur = Some(editor_find::step(self.hits.len(), c, forward)); self.scroll = true; }`。
`Find` 手写 `Default`(`case: false`, 其余空)。
`selected_single_line`:`egui::TextEdit::load_state(ctx, body_id(&s.key))` 取 `cursor.char_range()`,按字符切 `s.text`,不含 `\n` 且非空才返回。

4.2 标题栏:最大化按钮之后(right_to_left 里在它左边)加

```rust
                if icon_button(ui, Glyph::Search, true, "查找 (Ctrl+F)") {
                    let f = s.find.get_or_insert_with(Find::default);
                    f.focus = true;
                }
```

4.3 查找条(`ui.separator()` 之前、只读/换行提示之后):

```rust
        if let Some(f) = s.find.as_mut() {
            let mut close = false;
            ui.horizontal(|ui| {
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut f.query)
                        .id(find_box_id(&s.key))
                        .hint_text("查找")
                        .desired_width(240.0),
                );
                if std::mem::take(&mut f.focus) {
                    resp.request_focus();
                }
                // Enter 让单行框失焦 —— 用「失焦 + Enter」判,再把焦点要回来,
                // 用户可以一直按 Enter 往下找。
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    let back = ui.input(|i| i.modifiers.shift);
                    f.go(!back);
                    resp.request_focus();
                }
                if resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    close = true;
                }
                let aa = ui.selectable_label(f.case, "Aa").on_hover_text("区分大小写");
                if aa.clicked() {
                    f.case = !f.case;
                }
                ui.label(
                    egui::RichText::new(crate::ui::editor_find::counter(f.cur, f.hits.len()))
                        .color(theme::c32(t.fg_muted)),
                );
            });
            if close {
                s.pending_select = f.cur.map(|i| f.hits[i]);
                s.find = None;
            }
        }
```

Esc 注意:egui 在 `begin_pass` 里对 Escape 清焦点(见 app.rs:5219 那段注释),`resp.has_focus()` 在同帧可能已为假。实现时若测试第三条红在「条没关」,改为 `ctx.input_mut(|i| i.consume_key(NONE, Key::Escape))` 在 `show()` 开头判(`s.find.is_some()` 时),不要靠焦点。编辑器是 `Modal::Editor`,Esc 不会漏给终端。

`EditorState` 再加 `pending_select: Option<(usize, usize)>`:关条那帧记下,正文画完后写进正文 `TextEditState` 并 `request_focus` 正文,然后清掉。

4.4 命中表刷新(查找条之后、正文之前):

```rust
        if let Some(f) = s.find.as_mut() {
            let k = (f.query.clone(), f.case, s.text_rev);
            if f.key.as_ref() != Some(&k) {
                let caret = f.cur.and_then(|i| f.hits.get(i)).map_or(0, |h| h.0);
                f.hits = crate::ui::editor_find::find_all(&s.text, &f.query, f.case);
                // 查询变了 = 从头(或从上一处附近)重新找;正文变了同理。
                f.cur = crate::ui::editor_find::first_at_or_after(&f.hits, caret);
                f.key = Some(k);
                f.scroll = true;
            }
        }
```

4.5 正文:`ui.add(TextEdit…)` 换成 `let out = TextEdit::multiline(..)…id(body_id(&s.key)).show(ui);`,之后:

```rust
                    if out.response.changed() {
                        s.text_rev = s.text_rev.wrapping_add(1);
                    }
                    if let Some(f) = s.find.as_mut() {
                        if let Some(&(a, b)) = f.cur.and_then(|i| f.hits.get(i)) {
                            let r0 = out.galley.pos_from_ccursor(egui::text::CCursor::new(a));
                            let r1 = out.galley.pos_from_ccursor(egui::text::CCursor::new(b));
                            let off = out.galley_pos.to_vec2();
                            // 单行匹配(查询不含换行,查找框是单行),两端同一行。
                            let rect = egui::Rect::from_min_max(r0.min, r1.max).translate(off);
                            ui.painter().rect_filled(rect, 2.0, find_hit_color(t));
                            if std::mem::take(&mut f.scroll) {
                                ui.scroll_to_rect(rect, Some(egui::Align::Center));
                            }
                        }
                    }
                    if let Some((a, b)) = s.pending_select.take() {
                        let mut st = out.state;
                        st.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                            egui::text::CCursor::new(a),
                            egui::text::CCursor::new(b),
                        )));
                        st.store(ui.ctx(), out.response.id);
                        out.response.request_focus();
                    }
```

overlay 画在文字**之上**会盖住字:用半透明色(`find_hit_color` 取 `t.accent.gamma_multiply(0.35)`),或改用 `ui.painter().with_layer_id(..)` 画到下层;先用半透明,人工验收时看观感。

API 核对(epaint/egui 0.30,已在本地 registry 核过):`TextEditOutput{response, galley, galley_pos, text_clip_rect, state, cursor_range}`;`Galley::pos_from_ccursor(CCursor) -> Rect`;`TextEditState::store(self, ctx, id)`;`CCursorRange::two`。编译报签名不符时按报错里的实际签名改,别猜。

- [ ] **Step 5: 跑绿;跑整个 editor_window + glyph 白名单**

Run: `cargo test -p mullion-app --lib editor_window:: > /tmp/t.log 2>&1; cargo test -p mullion-app --test glyph_whitelist >> /tmp/t.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/t.log`
Expected: 全过。F216/F217 那几条尺寸守护(棘轮/`MIN_SIZE`)**必须**仍绿 —— 查找条多一行会改变内容高度,若红,读失败信息,不改断言。

- [ ] **Step 6: commit + 变异**

```bash
git add crates/mullion-app/src/ui/editor_window.rs
git commit -m "feat(app): 内置编辑器 Ctrl+F 查找条(F3/Shift+F3/回绕/计数/大小写/Esc) (F299)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```
按四条测试注释逐一变异、确认红、还原。

---

## Task 6: F297 拨号中的行显示「连接中…」

**Files:**
- Modify: `crates/mullion-app/src/shell/dial_ledger.rs`(`iter()`)
- Modify: `crates/mullion-app/src/app.rs`(`DialTicket.project_id`;`spawn_connect` 装票;UiFrame 两张表)
- Modify: `crates/mullion-app/src/ui/mod.rs`(UiFrame 字段 → `launcher::Lists`)
- Modify: `crates/mullion-app/src/ui/launcher.rs`、`crates/mullion-app/src/ui/project_row.rs`

- [ ] **Step 1: `DialLedger::iter` + 测试**

```rust
    /// F297:在途票据的只读视图。启动页据此现算「哪几行在拨」——
    /// 票的生命周期就是转圈的生命周期,不另存一张会漏清的表。
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.open.iter().map(|(_, t)| t)
    }
```

测试(`dial_ledger.rs` `mod tests`):

```rust
    /// 自证会变红:让 `iter` 返回 `std::iter::empty()`。
    #[test]
    fn iter_sees_open_tickets_and_forgets_claimed_ones() {
        let mut l = DialLedger::default();
        let a = l.issue("a");
        let _b = l.issue("b");
        assert_eq!(l.iter().copied().collect::<Vec<_>>(), vec!["a", "b"]);
        l.claim(a);
        assert_eq!(l.iter().copied().collect::<Vec<_>>(), vec!["b"]);
    }
```
(若 `DialLedger` 没有 `Default`,用它的构造函数 —— 看文件 `:40-50`。)

- [ ] **Step 2: 纯函数:从票据推导在拨表**

`app.rs`,挨着 `DialTicket`:

```rust
/// F297:哪些会话 / 项目**正由用户点击发起**的拨号在途。启动页每帧现算。
///
/// 只数 `user_initiated`:启动批量重连(`advance_auto_dial`)不是用户在
/// 这一页点的,给它转圈会让一整列同时转起来,用户以为自己点了什么。
fn dialing_from<'a>(
    tickets: impl Iterator<Item = &'a DialTicket>,
) -> (Vec<SessionId>, Vec<mullion_store::ProjectId>) {
    let mut sessions = Vec::new();
    let mut projects = Vec::new();
    for t in tickets.filter(|t| t.user_initiated) {
        if let Some(p) = t.project_id {
            projects.push(p);
        } else if let Some(s) = t.session_id {
            sessions.push(s);
        }
    }
    (sessions, projects)
}
```

`DialTicket` 加:

```rust
    /// F297:这次拨号是「从启动页打开项目」时,那个项目的 id。启动页据此
    /// 在项目行上转圈;`project_dir` 只有目录,认不出是哪个项目。
    project_id: Option<mullion_store::ProjectId>,
```

`spawn_connect` 里 `self.dials.issue(DialTicket { .. })` 补 `project_id: project.map(|p| p.id),`(`project: Option<&ProjectRecord>` 已是参数)。其余构造 `DialTicket {` 的地方(`grep -n "DialTicket {" crates/mullion-app/src/app.rs`)补 `project_id: None`。

测试:

```rust
    /// F297:项目拨号只进项目表(不让它背后那条会话也在会话列转圈),
    /// 非用户发起的一律不算。
    ///
    /// 自证会变红:删掉 `.filter(|t| t.user_initiated)`;或把 `else if`
    /// 改成独立的 `if`(项目拨号的会话也进会话表)。
    #[test]
    fn dialing_tables_count_user_clicks_only_and_projects_do_not_leak_into_sessions() {
        let t = |s: u64, p: Option<u64>, user: bool| DialTicket {
            session_id: Some(SessionId(s)),
            cfg: test_cfg(),
            automation: Default::default(),
            project_dir: None,
            user_initiated: user,
            project_id: p.map(mullion_store::ProjectId),
        };
        let v = [t(1, None, true), t(2, Some(7), true), t(3, None, false)];
        let (s, p) = dialing_from(v.iter());
        assert_eq!(s, vec![SessionId(1)]);
        assert_eq!(p, vec![mullion_store::ProjectId(7)]);
    }
```
`test_cfg()`:app.rs 测试里已有造 `SshConfig` 的辅助,`grep -n "fn .*cfg() -> SshConfig\|SshConfig {" crates/mullion-app/src/app.rs | head` 找来用。

- [ ] **Step 3: 接到 UiFrame → Lists**

`ui/mod.rs` UiFrame 加:

```rust
    /// F297:正由用户点击发起、还没有结果的拨号(会话 / 项目)。启动页在这些
    /// 行上转圈并忽略重复点击。每帧从票据台账现算。
    pub dialing_sessions: &'a [mullion_store::SessionId],
    pub dialing_projects: &'a [mullion_store::ProjectId],
```
`base_frame()` 补 `&[]`。`launcher::Lists` 加同名两个字段,`ui/mod.rs:1206-1222` 构造处从 `frame` 透传;launcher 测试里的 `draw()` 补 `&[]`(并给 `draw` 加一个带在拨表的变体,见 Step 4)。

`app.rs` 构造 UiFrame 处:

```rust
                            // F297:启动页转圈的依据 —— 票据台账现算,不存。
                            let (dialing_sessions, dialing_projects) = dialing_from(self.dials.iter());
```
字面量补 `dialing_sessions: &dialing_sessions, dialing_projects: &dialing_projects,`。

- [ ] **Step 4: 启动页行为 —— 失败测试**

launcher.rs `mod tests`:把 `draw` 扩成接收 `dialing: (&[SessionId], &[ProjectId])`(原 `draw` 保留为传 `(&[], &[])` 的薄壳,既有测试不动),然后:

```rust
    /// F297:在拨的会话行再点不发第二次连接请求,别的行照常可点。
    ///
    /// 自证会变红:删掉 `sessions_column` 里 `if resp.clicked() && !dialing` 的 `!dialing`。
    #[test]
    fn a_session_row_being_dialed_ignores_clicks_but_its_neighbours_do_not() {
        let ss = vec![sess(9, "独立机"), sess(10, "另一台")];
        let busy = [SessionId(9)];
        let hit = |target: SessionId| {
            let ctx = egui::Context::default();
            let mut ui_state = crate::ui::UiState::default();
            let lamps = std::collections::BTreeMap::new();
            let mut rect = None;
            for _ in 0..2 {
                let mut a = crate::ui::UiActions::default();
                let _ = ctx.run(wide(), |ctx| {
                    draw_dialing(ctx, &mut ui_state, &[], &lamps, &ss, &[], (&busy, &[]), &mut a);
                    rect = ctx.read_response(session_row_id(target)).map(|r| r.rect);
                });
            }
            let pos = rect.unwrap().center();
            let mut input = wide();
            input.events.push(egui::Event::PointerMoved(pos));
            for pressed in [true, false] {
                input.events.push(egui::Event::PointerButton {
                    pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default(),
                });
            }
            let mut a = crate::ui::UiActions::default();
            let _ = ctx.run(input, |ctx| {
                draw_dialing(ctx, &mut ui_state, &[], &lamps, &ss, &[], (&busy, &[]), &mut a);
            });
            ui_state.connect_request
        };
        assert_eq!(hit(SessionId(9)), None, "在拨的行又发了一次连接");
        assert_eq!(hit(SessionId(10)), Some(SessionId(10)), "别的行被连带锁住了");
    }

    /// F297:在拨的行画出「连接中…」;不在拨的行不画。
    ///
    /// 自证会变红:删掉 `sessions_column` 里画「连接中…」那一句。
    #[test]
    fn a_session_row_being_dialed_says_so() {
        let ss = vec![sess(9, "独立机"), sess(10, "另一台")];
        let texts = texts_full_dialing(&[], &ss, &[], "", (&[SessionId(9)], &[]));
        assert_eq!(texts.iter().filter(|s| *s == "连接中…").count(), 1, "{texts:?}");
    }

    /// F297:项目行同理(走 `project_row::Row.dialing`)。
    ///
    /// 自证会变红:`launcher.rs` 构造 `Row` 时 `dialing: false` 写死;或
    /// 项目列 `if r.clicked()` 去掉 `&& !dialing`。
    #[test]
    fn a_project_row_being_dialed_says_so_and_ignores_clicks() {
        let ps = vec![proj(1, "接口", "/srv/api", None)];
        let texts = texts_full_dialing(&ps, &[], &[], "", (&[], &[ProjectId(1)]));
        assert!(texts.iter().any(|s| s == "连接中…"), "{texts:?}");
        // 点击被忽略:同上一条会话的写法,目标换成 project_row::row_id("launcher", ProjectId(1)),
        // 断言 ui_state.project_open_request == None。
    }
```

`texts_full_dialing`:把既有 `texts_full` 抽出带在拨表的版本(`texts_full` 变成传空表的薄壳)。第三条的点击段照第一条写全(实现时别留注释占位)。

- [ ] **Step 5: 跑,确认红**

Run: `cargo test -p mullion-app --lib launcher::tests dial_ledger app::tests::dialing_tables > /tmp/t.log 2>&1; grep -nE "test result|FAILED|panicked|error\[" /tmp/t.log`

- [ ] **Step 6: 实现行绘制**

`project_row::Row` 加 `pub dialing: bool`(文档:「F297:这个项目正在拨号。右侧时间列换成 spinner +『连接中…』」),三处调用方:launcher 传 `cx.lists.dialing_projects.contains(&p.id)`,project_manager / project_pick 传 `false`。`show()` 里画时间列那段改:

```rust
    if row.dialing {
        // F297:时间列的位置换成「转圈 + 连接中…」。Spinner 自己会 request_repaint,
        // 只在拨号在途时存在 —— 不在拨时零额外重绘(T3)。
        let label = "连接中…";
        let font = egui::FontId::proportional(SUB_SIZE);
        let g = ui.painter().layout_no_wrap(label.into(), font, crate::theme::c32(t.fg_muted));
        let right = rect.right() - TEXT_RIGHT_PAD;
        let text_pos = egui::pos2(right - g.size().x, rect.center().y - g.size().y / 2.0);
        let s = g.size().y;
        let spin = egui::Rect::from_min_size(
            egui::pos2(text_pos.x - s - 4.0, rect.center().y - s / 2.0),
            egui::vec2(s, s),
        );
        egui::Spinner::new().size(s).paint_at(ui, spin);
        ui.painter().galley(text_pos, g, crate::theme::c32(t.fg_muted));
    } else {
        // 原时间列代码不动
    }
```
(`SUB_SIZE`/`TEXT_RIGHT_PAD` 是本文件已有常量;`painter().galley` 的第三个参数在 0.30 是 fallback color,核一遍签名。)

launcher 项目列:`if r.clicked() && !dialing { .. }`。会话列:

```rust
            let dialing = cx.lists.dialing_sessions.contains(&r.id);
            // …名称/副标题照画,`avail` 在拨号时再扣掉右侧「连接中…」的宽度…
            if dialing {
                crate::ui::project_row::paint_dialing(ui, rect, t);
            }
            if resp.clicked() && !dialing {
                ui_state.connect_request = Some(r.id);
            }
```
把上面项目行里的绘制段抽成 `pub(crate) fn paint_dialing(ui: &egui::Ui, rect: egui::Rect, t: &Theme) -> f32`(返回占用宽度),两列共用 —— 不各写一份。会话列在拨时 `avail -= 占用宽度 + SP_S`,防止名字压到「连接中…」上。

- [ ] **Step 7: 跑绿 + 接线源码切片**

`app.rs` 测试加:

```rust
    /// F297 接线:在拨表必须来自票据台账并流进 UiFrame。纯函数、launcher
    /// 两层都测得扎实,这里一句 `&[]` 就让转圈永远不出现,零报错
    /// (「纯函数测得扎实、接线没人看着」,F226~F232 记的恒绿模式)。
    ///
    /// 自证会变红:把 `dialing_from(self.dials.iter())` 换成 `(Vec::new(), Vec::new())`;
    /// 或把 `spawn_connect` 里 `project_id: project.map(|p| p.id)` 换成 `None`。
    #[test]
    fn the_dialing_tables_come_from_the_ledger_and_reach_the_frame() {
        let src = /* 同 Task 3 Step 6 用的那把生产源码剪刀 */;
        assert!(src.contains("dialing_from(self.dials.iter())"));
        assert!(src.contains("dialing_sessions: &dialing_sessions"));
        assert!(src.contains("project_id: project.map(|p| p.id)"));
    }
```
(`/* … */` 处用 Task 3 Step 6 找到的同一个函数调用,实现时写实。)

Run: `cargo test -p mullion-app > /tmp/t.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/t.log`

- [ ] **Step 8: commit + 变异**

```bash
git add -A crates/mullion-app
git commit -m "feat(app): 启动页点项目/会话后该行转圈显示「连接中…」,在途忽略重复点击 (F297)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```
逐条变异自证。

---

## Task 7: F298 节点状态 —— 纯数据层 `node_stats`

**Files:**
- Create: `crates/mullion-app/src/node_stats.rs`
- Modify: `crates/mullion-app/src/main.rs` 或 `lib.rs`(`mod node_stats;`,跟 `remote_bootstrap` 同处声明——`grep -n "mod remote_bootstrap" crates/mullion-app/src/*.rs`)

- [ ] **Step 1: 写模块(实现 + 测试)**

```rust
//! F298:节点状态(内存 / 磁盘 / 出口国家)。命令、解析、调度、显示,全是纯函数;
//! 唯一有状态的是 [`StatsCell`] —— 后台 task 写、事件循环读的共享格子,
//! 同 `remote_bootstrap::BootstrapFlags` 的跨线程模式。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const SAMPLE_EVERY: Duration = Duration::from_secs(10);
pub const COUNTRY_EVERY: Duration = Duration::from_secs(300);
/// exec 整体超时。挂住的 exec 不超时的话 `busy` 永远置着,这条连接从此不再采样。
pub const EXEC_TIMEOUT: Duration = Duration::from_secs(15);
pub const WARN_PCT: u8 = 90;

/// 内存 + 磁盘一次取完。输出两行:`mem <total_kb> <avail_kb>` / `disk <used_kb> <avail_kb>`。
/// 没有 `/proc/meminfo`(非 Linux)时第一行不出现 → 内存那格省掉。
/// `LC_ALL=C`:df 的表头/数字格式跟着 locale 走。
pub fn sample_command() -> Vec<u8> {
    b"LC_ALL=C; export LC_ALL; \
awk '/^MemTotal:/{t=$2} /^MemAvailable:/{a=$2} END{if(t>0&&a!=\"\")print \"mem\",t,a}' /proc/meminfo 2>/dev/null; \
df -Pk / 2>/dev/null | awk 'NR==2{print \"disk\",$3,$4}'"
        .to_vec()
}

/// 出口国家。curl 优先、没有就 wget、都没有退出 127。
/// `-f`:HTTP 错误(限流 429)给非零退出码,不把错误页当国家码。
pub fn country_command() -> Vec<u8> {
    b"if command -v curl >/dev/null 2>&1; then curl -fsS --max-time 5 https://ipinfo.io/country; \
elif command -v wget >/dev/null 2>&1; then wget -qO- -T 5 https://ipinfo.io/country; \
else exit 127; fi"
        .to_vec()
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Reading<T> {
    /// 还没取到过。不显示(连上头几秒不闪一个 `--`)。
    #[default]
    Unknown,
    Ok(T),
    /// 取失败,字符串是给 hover 的原因。显示 `--`。
    Failed(String),
    /// 这台机器上没有这一项(非 Linux 没有内存)。不显示。
    Absent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub used_kb: u64,
    pub total_kb: u64,
}

impl Usage {
    pub fn pct(&self) -> u8 {
        if self.total_kb == 0 {
            return 0;
        }
        ((self.used_kb as f64 * 100.0 / self.total_kb as f64).round() as u64).min(100) as u8
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Snapshot {
    pub mem: Reading<Usage>,
    pub disk: Reading<Usage>,
    pub country: Reading<String>,
}

/// 解析 [`sample_command`] 的输出。
pub fn parse_sample(stdout: &str) -> (Reading<Usage>, Reading<Usage>) {
    let mut mem = Reading::Absent;
    let mut disk = Reading::Failed("df 没有输出".into());
    for line in stdout.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        let n = |i: usize| f.get(i).and_then(|s| s.parse::<u64>().ok());
        match f.first().copied() {
            Some("mem") => {
                mem = match (n(1), n(2)) {
                    (Some(t), Some(a)) if t > 0 => Reading::Ok(Usage {
                        used_kb: t.saturating_sub(a),
                        total_kb: t,
                    }),
                    _ => Reading::Failed(format!("看不懂 /proc/meminfo:{line}")),
                };
            }
            Some("disk") => {
                disk = match (n(1), n(2)) {
                    // df 的 Use% 口径 = used / (used + avail),不是 used / total
                    // (ext4 的保留块会让后者偏低)。与用户在远端敲 df 看到的一致。
                    (Some(u), Some(a)) if u + a > 0 => Reading::Ok(Usage {
                        used_kb: u,
                        total_kb: u + a,
                    }),
                    _ => Reading::Failed(format!("看不懂 df 输出:{line}")),
                };
            }
            _ => {}
        }
    }
    (mem, disk)
}

/// 解析国家命令的结果。`exit` = 远端退出码(`None` = 远端没报)。
pub fn parse_country(exit: Option<u32>, stdout: &str) -> Reading<String> {
    match exit {
        Some(0) | None => {
            let s = stdout.trim();
            if s.len() == 2 && s.bytes().all(|b| b.is_ascii_uppercase()) {
                Reading::Ok(s.to_owned())
            } else {
                Reading::Failed(format!("ipinfo 返回的不是国家代码:{}", s.chars().take(40).collect::<String>()))
            }
        }
        Some(127) => Reading::Failed("远端没有 curl 也没有 wget".into()),
        Some(28) | Some(4) => Reading::Failed("访问 ipinfo.io 超时 / 网络不通".into()),
        Some(22) | Some(8) => Reading::Failed("ipinfo.io 返回错误(可能被限流)".into()),
        Some(c) => Reading::Failed(format!("取国家失败,退出码 {c}")),
    }
}

/// 到点没有。`busy` 时恒不到(上一次还挂在网络上)。
pub fn due(last: Option<Instant>, busy: bool, every: Duration, now: Instant) -> bool {
    !busy && last.is_none_or(|at| now.duration_since(at) >= every)
}

/// 下一次该醒的时刻(并入 `next_timer_wake`)。`busy` 时 `None` —— 结果回来的
/// 事件会唤醒,在这里报一个过去的时刻会让事件循环忙转(T7)。
pub fn next_due(last: Option<Instant>, busy: bool, every: Duration, now: Instant) -> Option<Instant> {
    if busy {
        return None;
    }
    Some(last.map_or(now, |at| at + every))
}

#[derive(Debug, Default)]
struct Inner {
    snap: Snapshot,
    sample_at: Option<Instant>,
    sample_busy: bool,
    country_at: Option<Instant>,
    country_busy: bool,
}

/// 一条连接的状态格子。`Clone` = 同一份(`Arc`)。
#[derive(Debug, Clone, Default)]
pub struct StatsCell(Arc<Mutex<Inner>>);

/// 事件循环这一侧要发起什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Plan {
    pub sample: bool,
    pub country: bool,
}

impl StatsCell {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // 中毒 = 某个 task 在持锁时 panic;数据只是显示用,照读。
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn snapshot(&self) -> Snapshot {
        self.lock().snap.clone()
    }
    /// 判到点并**同一次加锁里**置 busy、记发起时刻(发起侧只有事件循环一个线程,
    /// 同 `BootstrapFlags` 的前提)。
    pub fn plan(&self, now: Instant) -> Plan {
        let mut g = self.lock();
        let p = Plan {
            sample: due(g.sample_at, g.sample_busy, SAMPLE_EVERY, now),
            country: due(g.country_at, g.country_busy, COUNTRY_EVERY, now),
        };
        if p.sample {
            g.sample_busy = true;
            g.sample_at = Some(now);
        }
        if p.country {
            g.country_busy = true;
            g.country_at = Some(now);
        }
        p
    }
    pub fn next_wake(&self, now: Instant) -> Option<Instant> {
        let g = self.lock();
        [
            next_due(g.sample_at, g.sample_busy, SAMPLE_EVERY, now),
            next_due(g.country_at, g.country_busy, COUNTRY_EVERY, now),
        ]
        .into_iter()
        .flatten()
        .min()
    }
    pub fn finish_sample(&self, mem: Reading<Usage>, disk: Reading<Usage>) {
        let mut g = self.lock();
        g.snap.mem = mem;
        g.snap.disk = disk;
        g.sample_busy = false;
    }
    pub fn finish_country(&self, c: Reading<String>) {
        let mut g = self.lock();
        g.snap.country = c;
        g.country_busy = false;
    }
    /// 用户点了国家那格:下一次 tick 立刻取(在途时不叠发)。
    pub fn refresh_country_now(&self) {
        self.lock().country_at = None;
    }
    /// 断线重连换了 handle:两项都立刻重取,旧值留着(不闪空)。
    pub fn reset_schedule(&self) {
        let mut g = self.lock();
        g.sample_at = None;
        g.country_at = None;
        g.sample_busy = false;
        g.country_busy = false;
    }
}

/// 标题条上的一格。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Piece {
    pub text: String,
    pub hover: String,
    pub warn: bool,
    /// 点它刷新国家。
    pub is_country: bool,
}

fn gib(kb: u64) -> String {
    format!("{:.1} GiB", kb as f64 / 1024.0 / 1024.0)
}

/// 快照 → 标题条上的几格(从左到右:内存、磁盘、国家)。`Unknown`/`Absent` 不出格。
pub fn pieces(s: &Snapshot) -> Vec<Piece> {
    let mut out = Vec::new();
    let usage = |label: &str, r: &Reading<Usage>, out: &mut Vec<Piece>| match r {
        Reading::Ok(u) => out.push(Piece {
            text: format!("{label} {}%", u.pct()),
            hover: format!("{label}已用 {} / {}", gib(u.used_kb), gib(u.total_kb)),
            warn: u.pct() >= WARN_PCT,
            is_country: false,
        }),
        Reading::Failed(why) => out.push(Piece {
            text: format!("{label} --"),
            hover: why.clone(),
            warn: false,
            is_country: false,
        }),
        Reading::Unknown | Reading::Absent => {}
    };
    usage("内存", &s.mem, &mut out);
    usage("磁盘", &s.disk, &mut out);
    match &s.country {
        Reading::Ok(c) => out.push(Piece {
            text: c.clone(),
            hover: "远端出口国家(ipinfo.io),点击刷新".into(),
            warn: false,
            is_country: true,
        }),
        Reading::Failed(why) => out.push(Piece {
            text: "--".into(),
            hover: format!("{why}\n点击重试"),
            warn: false,
            is_country: true,
        }),
        Reading::Unknown | Reading::Absent => {}
    }
    out
}

/// 窄条取舍:状态段宽 `stats_w`、标题全宽 `title_w`、可用 `avail`。
/// 标题保底 `min(title_w, TITLE_RESERVE)`,剩下的放得下状态段才画 ——
/// 状态段先于标题让位(用户确认的取舍:认 pane 靠标题)。
pub const TITLE_RESERVE: f32 = 220.0;
pub fn stats_fit(avail: f32, stats_w: f32, title_w: f32) -> bool {
    avail - stats_w >= title_w.min(TITLE_RESERVE)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 自证会变红:把 disk 的 total 改成只取 `$2`(总量)口径。
    #[test]
    fn a_linux_sample_parses_into_memory_and_df_style_disk_usage() {
        let (m, d) = parse_sample("mem 8000000 2000000\ndisk 710 290\n");
        assert_eq!(m, Reading::Ok(Usage { used_kb: 6000000, total_kb: 8000000 }));
        assert_eq!(d, Reading::Ok(Usage { used_kb: 710, total_kb: 1000 }));
        assert_eq!(match d { Reading::Ok(u) => u.pct(), _ => 0 }, 71);
    }

    /// 非 Linux:没有 mem 行 → 内存 Absent(不出格),磁盘照常。
    /// 自证会变红:把 `mem` 初值改成 `Reading::Failed(..)`。
    #[test]
    fn no_meminfo_means_memory_is_absent_not_failed() {
        let (m, d) = parse_sample("disk 1 1\n");
        assert_eq!(m, Reading::Absent);
        assert!(matches!(d, Reading::Ok(_)));
        assert!(pieces(&Snapshot { mem: m, disk: d, country: Reading::Unknown })
            .iter()
            .all(|p| !p.text.starts_with("内存")));
    }

    #[test]
    fn garbage_is_a_failure_with_a_reason() {
        let (m, d) = parse_sample("mem x y\n");
        assert!(matches!(m, Reading::Failed(_)));
        assert!(matches!(d, Reading::Failed(_)));
    }

    /// 自证会变红:删掉 127 分支(落到通用「退出码」文案)。
    #[test]
    fn country_exit_codes_map_to_reasons_a_user_can_act_on() {
        assert_eq!(parse_country(Some(0), "JP\n"), Reading::Ok("JP".into()));
        assert!(matches!(parse_country(Some(0), "<html>"), Reading::Failed(_)));
        match parse_country(Some(127), "") {
            Reading::Failed(s) => assert!(s.contains("curl"), "{s}"),
            _ => panic!(),
        }
        match parse_country(Some(22), "") {
            Reading::Failed(s) => assert!(s.contains("限流"), "{s}"),
            _ => panic!(),
        }
    }

    /// busy 时不到点、也不报唤醒时刻(报过去的时刻 = T7 忙转)。
    /// 自证会变红:把 `next_due` 里 `if busy { return None; }` 删掉。
    #[test]
    fn a_busy_probe_is_never_due_and_never_asks_for_a_wakeup() {
        let now = Instant::now();
        assert!(!due(None, true, SAMPLE_EVERY, now));
        assert_eq!(next_due(None, true, SAMPLE_EVERY, now), None);
        assert!(due(None, false, SAMPLE_EVERY, now));
        let last = now - Duration::from_secs(3);
        assert!(!due(Some(last), false, SAMPLE_EVERY, now));
        assert_eq!(next_due(Some(last), false, SAMPLE_EVERY, now), Some(last + SAMPLE_EVERY));
    }

    /// plan 同时置 busy:连续两次 plan,第二次不会再发。
    /// 自证会变红:删掉 `plan` 里 `g.sample_busy = true;`。
    #[test]
    fn planning_marks_busy_so_a_second_tick_does_not_double_fire() {
        let c = StatsCell::default();
        let now = Instant::now();
        assert_eq!(c.plan(now), Plan { sample: true, country: true });
        assert_eq!(c.plan(now), Plan::default());
        c.finish_sample(Reading::Absent, Reading::Absent);
        assert!(!c.plan(now).sample, "刚采过,10 秒内不该再采");
        c.refresh_country_now();
        c.finish_country(Reading::Unknown);
        assert!(c.plan(now).country, "点了刷新,下一次 tick 该立刻取");
    }

    /// ≥90% 标 warn,89% 不标。自证会变红:`>=` 改 `>`(90% 不标)。
    #[test]
    fn ninety_percent_is_a_warning() {
        let at = |used| Snapshot {
            mem: Reading::Ok(Usage { used_kb: used, total_kb: 100 }),
            ..Default::default()
        };
        assert!(pieces(&at(90))[0].warn);
        assert!(!pieces(&at(89))[0].warn);
    }

    #[test]
    fn failures_render_as_dashes_with_the_reason_on_hover() {
        let s = Snapshot {
            disk: Reading::Failed("df 没有输出".into()),
            country: Reading::Failed("远端没有 curl 也没有 wget".into()),
            ..Default::default()
        };
        let p = pieces(&s);
        assert_eq!(p[0].text, "磁盘 --");
        assert_eq!(p[0].hover, "df 没有输出");
        assert_eq!(p[1].text, "--");
        assert!(p[1].is_country);
    }

    /// 窄条让位:放不下时状态段先让。三点:放得下 / 恰好 / 差一点。
    /// 自证会变红:把 `>=` 改成 `>`(恰好那点翻转)。
    #[test]
    fn the_stats_segment_yields_before_the_title() {
        assert!(stats_fit(600.0, 200.0, 300.0));
        assert!(stats_fit(420.0, 200.0, 300.0), "恰好留够 220 保底");
        assert!(!stats_fit(419.0, 200.0, 300.0));
        assert!(stats_fit(300.0, 200.0, 100.0), "标题本身短于保底时按标题宽算");
    }
}
```

`Option::is_none_or` 需要 Rust 1.82+;先 `rustc --version` 核,不够就写成 `last.map_or(true, |at| ..)`(clippy 可能提示 `unnecessary_map_or`,以 clippy 为准)。

- [ ] **Step 2: 跑 + clippy**

Run: `cargo test -p mullion-app --lib node_stats:: > /tmp/t.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/t.log; cargo clippy -p mullion-app --all-targets -- -D warnings 2>&1 | tail -20`
Expected: 全过、clippy 无输出(未使用警告:本任务先 `#[allow(dead_code)]` 挂在模块声明上,Task 8 接线后删掉)。

- [ ] **Step 3: commit + 变异**

```bash
git add crates/mullion-app/src/node_stats.rs crates/mullion-app/src/main.rs
git commit -m "feat(app): 节点状态纯数据层(采样/国家命令、解析、调度、显示) (F298)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```
按测试注释逐条变异。

---

## Task 8: F298 驱动 —— `HostConn.stats` + `tick_node_stats` + 事件 + 唤醒

**Files:**
- Modify: `crates/mullion-app/src/shell/workspace/mod.rs:144-175`(`HostConn`)
- Modify: `crates/mullion-app/src/app.rs`(`UserEvent`、`user_event_marks_dirty`、`user_event` 分派、`tick_node_stats`、`about_to_wait`、`next_timer_wake`、`PaneReconnected` 处置、两处 `HostConn {` 构造)

- [ ] **Step 1: `HostConn.stats`**

```rust
    /// F298:这台机器的节点状态(内存/磁盘/出口国家)。**按连接存**,同连接的
    /// pane 共享一份 —— 同一台机器开三块 pane 不该采三遍。后台 task 写、
    /// 事件循环读,见 `node_stats::StatsCell`。
    pub stats: crate::node_stats::StatsCell,
```
所有 `HostConn {` 构造处(`grep -n "HostConn {" crates/mullion-app/src -r`)补 `stats: Default::default(),`。

- [ ] **Step 2: 事件变体**

`UserEvent` 加:

```rust
    /// F298:某条连接的节点状态格子更新了。**不带负载**:数据已写进那条
    /// 连接的 `HostConn.stats`,这条事件只负责标脏重绘。不带世代路由 ——
    /// 连接没了格子跟着没,没有「送错标签」这回事。
    NodeStatsUpdated,
```
`user_event_marks_dirty` 的「其余一律标脏」列表里加 `| NodeStatsUpdated`(这个 match 是穷尽的,漏了编译不过)。`user_event` 分派里加 `UserEvent::NodeStatsUpdated => {}`,注释「标脏已在函数开头做了,见 `user_event_marks_dirty`」。实现时核一遍:标脏后是谁请求重绘(`grep -n "if self.ui_dirty" crates/mullion-app/src/app.rs`),确认 `TunnelState` 同样只靠标脏就会重绘;若不是,照 `TunnelState` 的做法补。

- [ ] **Step 3: `tick_node_stats`**

紧挨 `tick_tmux_bootstrap` 之后:

```rust
    /// F298:到点的连接采一次节点状态。结构同 `tick_tmux_bootstrap`:每次空闲
    /// 都跑,真正的活只有每条连接一次加锁判到点(`StatsCell::plan`)。
    ///
    /// **有超时**(`node_stats::EXEC_TIMEOUT`),与 F124 那条刻意不包超时不同:
    /// 这里是周期任务,挂住一次就永远不再更新,用户看到的是一个停住不动、
    /// 却看起来正常的数字 —— 比显示 `--` 更糟。
    fn tick_node_stats(&mut self) {
        let now = Instant::now();
        for tab in self.tabs.iter_mut() {
            let Some(t) = tab.content.as_terminal_mut() else {
                // SFTP 节点标签没有标题条,占位标签没有连接。
                continue;
            };
            for host in &t.ws.hosts {
                let plan = host.stats.plan(now);
                if plan.sample {
                    let conn = host.handle.clone();
                    let cell = host.stats.clone();
                    let proxy = self.proxy.clone();
                    self._runtime.spawn(async move {
                        let cmd = crate::node_stats::sample_command();
                        let r = tokio::time::timeout(
                            crate::node_stats::EXEC_TIMEOUT,
                            mullion_ssh::exec::exec(&conn, &cmd),
                        )
                        .await;
                        let (mem, disk) = match r {
                            Ok(Ok(out)) => crate::node_stats::parse_sample(&String::from_utf8_lossy(&out.stdout)),
                            Ok(Err(e)) => {
                                let why = format!("采样失败:{e}");
                                (crate::node_stats::Reading::Failed(why.clone()), crate::node_stats::Reading::Failed(why))
                            }
                            Err(_) => {
                                let why = "采样超时".to_string();
                                (crate::node_stats::Reading::Failed(why.clone()), crate::node_stats::Reading::Failed(why))
                            }
                        };
                        cell.finish_sample(mem, disk);
                        let _ = proxy.send_event(UserEvent::NodeStatsUpdated);
                    });
                }
                if plan.country {
                    let conn = host.handle.clone();
                    let cell = host.stats.clone();
                    let proxy = self.proxy.clone();
                    self._runtime.spawn(async move {
                        let cmd = crate::node_stats::country_command();
                        let r = tokio::time::timeout(
                            crate::node_stats::EXEC_TIMEOUT,
                            mullion_ssh::exec::exec(&conn, &cmd),
                        )
                        .await;
                        let c = match r {
                            Ok(Ok(out)) => crate::node_stats::parse_country(
                                out.exit_status,
                                &String::from_utf8_lossy(&out.stdout),
                            ),
                            Ok(Err(e)) => crate::node_stats::Reading::Failed(format!("取国家失败:{e}")),
                            Err(_) => crate::node_stats::Reading::Failed("取国家超时".into()),
                        };
                        cell.finish_country(c);
                        let _ = proxy.send_event(UserEvent::NodeStatsUpdated);
                    });
                }
            }
        }
    }
```

`exec` 的参数是 `&Vec<u8>` 还是 `&[u8]`,按 `crates/mullion-ssh/src/exec.rs` 实际签名传。`self.proxy` / `self._runtime` 与 `self.tabs` 是不同字段,借用分得开(同 `tick_tmux_bootstrap` 的注释)。

`about_to_wait` 里 `self.tick_tmux_bootstrap();` 下一行加 `self.tick_node_stats();`。

- [ ] **Step 4: 唤醒并入 `next_timer_wake`**

```rust
    fn next_timer_wake(&self, now: u64) -> Option<Instant> {
        let blink_now = self.start + std::time::Duration::from_millis(now);
        [
            self.sync_timeout_wake(now),
            self.blink_wake(blink_now),
            self.node_stats_wake(blink_now),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// F298:所有终端标签上所有连接里,最早到点的那次采样。
    fn node_stats_wake(&self, now: Instant) -> Option<Instant> {
        self.tabs
            .iter()
            .filter_map(|t| t.content.as_terminal())
            .flat_map(|t| t.ws.hosts.iter())
            .filter_map(|h| h.stats.next_wake(now))
            .min()
    }
```

T7 自查:`next_wake` 在「从没采过且不 busy」时返回 `now` —— 只会发生在连接刚建好、`tick_node_stats` 还没跑的那一帧;`about_to_wait` 里 tick 先于 `next_timer_wake`(实现时核顺序:`grep -n "tick_node_stats\|next_timer_wake" crates/mullion-app/src/app.rs`),tick 跑完就 busy 了。若顺序相反,把 tick 挪到前面。

- [ ] **Step 5: 断线重连重置调度**

`UserEvent::PaneReconnected` 处置里清 `tmux_last_try` 那几行旁边加 `host.stats.reset_schedule();`(换了 handle,旧 task 攥着旧连接,结果回来也只是写一次旧值,随即被新采样覆盖)。

- [ ] **Step 6: 源码切片守护**

`app.rs` 测试加(用 Task 3 Step 6 找到的同一把剪刀,仿 `tick_tmux_bootstrap` 那两条既有守护的写法——`grep -n "tick_tmux_bootstrap();\"" crates/mullion-app/src/app.rs`):

```rust
    /// F298:采样 tick 必须挂在 `about_to_wait` 上。
    /// 自证会变红:删掉 `self.tick_node_stats();`。
    #[test]
    fn about_to_wait_drives_the_node_stats_probe() { /* 切 about_to_wait 函数体,contains */ }

    /// F298:采样时刻必须并入唯一的定时唤醒汇合点 —— 否则空闲时 10 秒一次的
    /// 采样要等到别的事件把循环唤醒才发生(数字「时灵时不灵」)。
    /// 自证会变红:从 `next_timer_wake` 的数组里删掉 `self.node_stats_wake(..)`。
    #[test]
    fn node_stats_wakeups_join_the_single_timer_aggregation_point() { /* 切 next_timer_wake 函数体 */ }

    /// F298:`tick_node_stats` 必须遍历**全部**标签(F128 那条「drive_* 每帧
    /// 驱动函数必须遍历全部标签」)—— 只看活动标签的话,后台标签的数字停住。
    /// 自证会变红:把 `self.tabs.iter_mut()` 换成只取活动标签。
    #[test]
    fn the_node_stats_tick_walks_every_tab() { /* 切 tick_node_stats 函数体,contains "self.tabs.iter_mut()" */ }
```
三个函数体实现时写实(照抄邻近 `tick_tmux_bootstrap` 守护的切片代码,别留 `/* */`)。

- [ ] **Step 7: 跑绿 + clippy**

删掉 Task 7 的 `#[allow(dead_code)]`。
Run: `cargo test -p mullion-app > /tmp/t.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/t.log; cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20`
**T3/T7 守护必须仍绿**:`app::tests::redraw_is_frame_capped`、`frame::tests`。在日志里 grep 这两个名字确认跑了。

- [ ] **Step 8: commit + 变异**

```bash
git add -A crates/mullion-app
git commit -m "feat(app): 按连接定时采样节点状态(exec+超时),唤醒并入 next_timer_wake (F298)

跑过守护:app::tests::redraw_is_frame_capped、frame::tests(T3/T7)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

## Task 9: F298 标题条右侧状态段

**Files:**
- Modify: `crates/mullion-app/src/ui/pane_title.rs`(`TitleView.stats`、`TitleAction.refresh_node`、`show()`)
- Modify: `crates/mullion-app/src/ui/pane_edges.rs:212`、`ui/mod.rs:1998`、`ui/files_panel.rs:4933` 与 pane_title.rs 测试里全部 `TitleView {` 字面量(补 `stats: Vec::new()`)
- Modify: `crates/mullion-app/src/ui/mod.rs`(`UiActions.refresh_node_pane` + 转发)
- Modify: `crates/mullion-app/src/app.rs`(TitleView 构造填 stats;处置 `refresh_node_pane`;`has_real_action`)

- [ ] **Step 1: 字段**

`TitleView` 加:

```rust
    /// F298:这块 pane 所在连接的节点状态,已由 `node_stats::pieces` 排好。
    /// 空 = 不显示(还没取到 / `host_pending` / 没连上)。**拿 owned**:源头是
    /// `Mutex` 里的快照,借不出活到本帧结束的引用;每 pane 每帧三个短串。
    pub stats: Vec<crate::node_stats::Piece>,
```
`TitleAction` 加 `pub refresh_node: Option<PaneId>,`(文档:「F298:点了国家那格 —— 请求立刻重取出口国家」)。

- [ ] **Step 2: 写失败测试(pane_title.rs)**

复用本文件既有的 `click_button(id)` 与造 `TitleView` 的写法(`sed -n 760,830p crates/mullion-app/src/ui/pane_title.rs`):

```rust
    fn with_stats(w: f32) -> Vec<crate::node_stats::Piece> {
        let _ = w;
        crate::node_stats::pieces(&crate::node_stats::Snapshot {
            mem: crate::node_stats::Reading::Ok(crate::node_stats::Usage { used_kb: 43, total_kb: 100 }),
            disk: crate::node_stats::Reading::Ok(crate::node_stats::Usage { used_kb: 95, total_kb: 100 }),
            country: crate::node_stats::Reading::Ok("JP".into()),
        })
    }

    /// F298:宽条上画出三格,磁盘 95% 用 warn 色。
    /// 自证会变红:删掉画状态段的那段;或 warn 色换成 fg_muted。
    #[test]
    fn a_wide_title_bar_shows_node_stats_with_the_warning_color_over_ninety() {
        // 造一块 1200px 宽标题条的 TitleView,stats = with_stats(..),跑两帧取 shapes;
        // 断言 find_text(shapes, "内存 43%")、"JP" 存在,"磁盘 95%" 的颜色 == c32(t.warn)。
    }

    /// F298:窄条上状态段让位,标题文字仍在。
    /// 自证会变红:删掉 `stats_fit` 判断(恒画)。
    #[test]
    fn a_narrow_title_bar_drops_the_stats_before_the_title() {
        // 同上但标题条 260px 宽;断言 "内存" 不在 shapes 里、`title_text(&view)` 的前缀在。
    }

    /// F298:点国家那格报 `refresh_node`,不串到 × / 换节点。
    /// 自证会变红:国家那格不 `Sense::click()`,或报成 `rehost`。
    #[test]
    fn clicking_the_country_asks_for_a_refresh_of_this_pane() {
        let a = click_button(country_id(PaneId(1))); // click_button 需要能喂带 stats 的 view,见下
        assert_eq!(a.refresh_node, Some(PaneId(1)));
        assert_eq!(a.close, None);
        assert_eq!(a.rehost, None);
    }
```
既有 `click_button` 造的 view 没有 stats —— 给它加一个 `click_button_with(view_fn, id)` 变体或让默认 view 带 `with_stats`(后者会影响既有断言,选前者)。三条测试的函数体实现时按本文件既有测试写全(取 shapes、找文字、取颜色的辅助本文件已有:`grep -n "fn find_text\|fn text_color\|fn run_titles" crates/mullion-app/src/ui/pane_title.rs`)。

- [ ] **Step 3: 跑,确认红**

Run: `cargo test -p mullion-app --lib pane_title:: > /tmp/t.log 2>&1; grep -nE "test result|FAILED|panicked|error\[" /tmp/t.log`

- [ ] **Step 4: 实现 `show()` 里的状态段**

在 right_to_left 闭包里、项目按钮之后、`ui.with_layout(left_to_right ..)` 之前:

```rust
                    // F298:节点状态段。right_to_left 里先加的在右 —— 国家最右。
                    // 放不下时整段不画(先于标题让位,`node_stats::stats_fit`)。
                    if !v.stats.is_empty() {
                        let font = egui::FontId::proportional(12.0);
                        let sep = " · ";
                        let measure = |s: &str| {
                            ui.fonts(|f| f.layout_no_wrap(s.to_owned(), font.clone(), egui::Color32::WHITE).size().x)
                        };
                        let stats_w: f32 = v.stats.iter().map(|p| measure(&p.text)).sum::<f32>()
                            + measure(sep) * (v.stats.len().saturating_sub(1)) as f32
                            + ui.spacing().item_spacing.x * (2 * v.stats.len()) as f32;
                        let title_w = measure(&title_text(v));
                        if crate::node_stats::stats_fit(ui.available_width(), stats_w, title_w) {
                            for (i, p) in v.stats.iter().rev().enumerate() {
                                if i > 0 {
                                    ui.label(egui::RichText::new(sep.trim()).font(font.clone()).color(theme::c32(t.fg_dim)));
                                }
                                let color = if p.warn { t.warn } else { t.fg_muted };
                                let text = egui::RichText::new(&p.text).font(font.clone()).color(theme::c32(color));
                                if p.is_country {
                                    let r = ui.push_id(country_id(v.geom.id), |ui| {
                                        ui.add(egui::Label::new(text).sense(egui::Sense::click()))
                                    }).inner;
                                    let r = r.on_hover_text(&p.hover);
                                    if r.clicked() {
                                        action.refresh_node = Some(v.geom.id);
                                    }
                                } else {
                                    ui.label(text).on_hover_text(&p.hover);
                                }
                            }
                        }
                    }
```

`country_id(id)`:仿 `close_id` 写 `egui::Id::new(("pane_title_country", id.0))`;测试靠 `click_button` 按 id 取 rect —— 若 `push_id` 包出来的 Label id 不等于 `country_id(..)`,改成 `let (rect, _) = ui.allocate_exact_size(..); ui.interact(rect, country_id(id), Sense::click())` + painter 画字(与本文件 `small_action_button` 的手法一致,**首选这个**,id 精确可控)。`title_text(v)` 的签名以本文件为准。颜色字段 `fg_dim`/`fg_muted`/`warn` 以 `theme.rs` 为准。

- [ ] **Step 5: 接线**

1. `ui/mod.rs`:`UiActions` 加 `pub refresh_node_pane: Option<mullion_core::layout::PaneId>,`;`:1262` 之后 `actions.refresh_node_pane = title_action.refresh_node;`。
2. `app.rs` `has_real_action` 加 `|| a.refresh_node_pane.is_some()`(**枚举式门控,漏了 = 点击在 discard 趟被静默吃掉**)。
3. `app.rs` 处置(挨着 `pick_project_pane` 的处置,约 14202):

```rust
                            if let Some(pane) = actions.refresh_node_pane {
                                // F298:点国家那格 —— 这块 pane 所在连接下一次 tick 立刻重取。
                                if let Some(ws) = active_ws_of(&self.tabs) {
                                    if let Some(h) = ws.pane(pane).and_then(|p| ws.hosts.get(p.host_ix)) {
                                        h.stats.refresh_country_now();
                                    }
                                }
                            }
```
(`active_ws_of` 若只有可变版本,用对应的只读取法;`refresh_country_now(&self)` 只要共享引用。)

4. `app.rs` TitleView 构造(约 13778)补:

```rust
                                                    // F298:与 `host` 同一条判据 ——
                                                    // `host_pending` 时不借主叶子那台的数字。
                                                    stats: ws.pane(g.id).filter(|p| !p.host_pending)
                                                        .and_then(|p| ws.hosts.get(p.host_ix))
                                                        .map(|h| crate::node_stats::pieces(&h.stats.snapshot()))
                                                        .unwrap_or_default(),
```

5. 源码切片守护(`app.rs` 测试):

```rust
    /// F298 接线:`has_real_action` 必须登记 `refresh_node_pane`(列举式门控,
    /// 第 N 次踩:漏了 = 点国家那格「有时候没反应」);TitleView 的 stats 必须
    /// 从 `HostConn.stats` 取且与 `host` 同守 `host_pending`。
    /// 自证会变红:删 `|| a.refresh_node_pane.is_some()`;或删 `.filter(|p| !p.host_pending)`。
    #[test]
    fn the_node_stats_reach_the_title_bar_and_the_refresh_click_is_a_real_action() { /* 实现时写实 */ }
```

- [ ] **Step 6: 跑全绿 + clippy + fmt**

Run:
```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20
cargo fmt --check
```
Expected: 全过、clippy 无输出、fmt 无 diff。

- [ ] **Step 7: commit + 变异**

```bash
git add -A crates/mullion-app
git commit -m "feat(app): 分屏标题条右侧显示内存/磁盘/出口国家,≥90% 警示色,窄条先让位 (F298)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```
逐条变异自证。

---

## Task 10: spec.md 六行 + 整体终审

**Files:**
- Modify: `spec.md`(F295 行之后)

- [ ] **Step 1: 写 spec 行**

按 F293~F295 的表格格式(`| Fnnn | **标题**:描述 | 优先级 | 已实现(v0.1.119)。要点…守护:…人工验:… |`)追加 F296~F301 六行。每行写:实际实现要点、守护测试全名、变异自证结果、人工验收项、否掉的备选(抄设计文档「否掉的备选」)。

- [ ] **Step 2: 终审(缝里的缺陷)**

派一个 review subagent(`superpowers:requesting-code-review`)看 `git diff 46023f5..HEAD`,重点提示它查**任务之间的缝**(本项目教训:单任务复核全 APPROVED、缺陷长在缝里):
- F300 与 F301 同改书签菜单闭包:省略后的文本是否仍被 F301 点击分支按**原始** `b.path` 判 `~`(不能按省略后的显示串)。
- F297:`ConnectErr` / 主机密钥拒绝 / 用户取消,票据是否都被 `claim`(否则永远转圈)。`grep -n "self.dials.claim" crates/mullion-app/src/app.rs` 列全路径逐一核。
- F298:SFTP 节点标签是否真的不采样(`as_terminal_mut` 过滤);`wind_down` 关标签后在途 task 只写一个没人读的格子,无泄漏。
- F299:查找条开着时 Ctrl+S 仍能保存;只读文件能查找。

- [ ] **Step 3: commit**

```bash
git add spec.md
git commit -m "docs(spec): F296~F301 六条落表

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

## Task 11: 发版 v0.1.119

- [ ] **Step 1:** 按 `.claude/skills/release-windows/SKILL.md` 一条龙:升 workspace 版本到 0.1.119 → 跑绿(`cargo test --workspace` + clippy + fmt)→ 交叉编译 + objdump 验收 → 签名 → **先 squash 再建 tag**、push 走 nc socks5 ProxyCommand → `gh release create`(注意 tag 别建在远端旧 HEAD 上)→ 报链接。
- [ ] **Step 2:** Release notes 的人工验收清单(以下全部「未验证,需人工确认」):
  1. F296:启动页会话列不再出现 SFTP 会话;全是 SFTP 时显示「还没有会话」。
  2. F297:点项目/会话行立刻出现转圈 +「连接中…」;连打多次只开一个标签;连不上时行恢复并报错;慢链路下转圈是否跟手。
  3. F298:标题条右侧 `内存 43% · 磁盘 71% · JP`;悬停看绝对值;≥90% 黄色;点国家刷新;远端没 curl/被限流时显示 `--` 并在悬停说明;窄分屏时先消失的是状态段;空闲时 CPU 不因采样上升(对照 profile 日志 N1)。
  4. F299:编辑器 Ctrl+F / 标题栏放大镜开条;预填选区;打字即跳;Enter/F3/Shift+Enter/Shift+F3 回绕;`3/17`;Aa;Esc 回正文且选中匹配;高亮底色是否遮字(观感)。
  5. F300:书签下拉先点短书签再看长书签,一行显示;超长路径中段 `…`,悬停完整路径。
  6. F301:在远端/本地主目录点 ☆ 存 `~`;换目录 ★ 熄灭、回主目录 ★ 亮;点 `~` 书签跳到各栏自己的主目录;默认本地目录填 `~` 能用。
