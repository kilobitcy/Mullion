# F371~F374 实现 plan:双击选路径 / 侧栏 SFTP 对账 / 备忘浮层居中 / Ctrl+Shift+B 用最近复制

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 落地 `docs/superpowers/specs/2026-11-17-f371-f374-path-dblclick-sftp-follow-memo-center-reveal-copy-design.md` 的四条,发 v0.1.146。

**Architecture:** F371 判据是 `mullion-term::selection` 里的纯函数,`Emulator::selection_start` 的 `Semantic` 分支先走它;F373 只改 `ui/memo_popup.rs` 的 Area 锚点;F374/F372 在 `app.rs`,判据各抽一个纯函数(`files::reveal::recall`、`reconcile_plan_of`),`trigger_sftp_open`/`reopen_sftp_on_focused_host` 抽出带目标参数的版本。

**Tech Stack:** Rust、alacritty_terminal 0.26、egui 0.30。

执行顺序:F373 → F371 → F374 → F372(F372 依赖 F374 改归属的写点)。每条一个提交,摘要带编号。

---

### Task 1: F373 备忘浮层居中

**Files:** Modify `crates/mullion-app/src/ui/memo_popup.rs`(`show` 里的 `egui::Area` 链)

- [ ] 写失败测试 `the_popup_is_centered_in_its_pane`:给 `show` 传 `Some(pane_rect)`(偏离屏幕中心的一块),连跑 3 帧,`ctx.memory(|m| m.area_rect(area_id(pane)))` 的中心与 `pane_rect.center()` 误差 ≤1px。现有 `frame` 辅助传 `None`,新增一个带 rect 的版本。
- [ ] 跑:`cargo test -p mullion-app memo_popup` → 红(左上对齐)。
- [ ] 实现:`.pivot(egui::Align2::CENTER_CENTER).fixed_pos(host.center())`,保留 `constrain_to(host)`。首帧闪烁由 egui 0.30 自己处理:`Area::show` 在 state 不存在时走 `sizing_pass().invisible()`(area.rs:397/549)。
- [ ] 跑绿,提交 `feat(app): 备忘浮层在 pane 内水平垂直居中 (F373)`。

### Task 2: F371 双击选完整路径

**Files:** Modify `crates/mullion-term/src/selection.rs`(新增 `is_path_char`/`path_span`)、`crates/mullion-term/src/emulator.rs`(`with_history` 的 `Config`、`selection_start`)、`crates/mullion-term/tests/vt_fixtures.rs`

- [ ] 纯函数测试(selection.rs):`设计文档：docs/superpowers/specs/x-design.md` 在 `docs` 中间任意点给出整段;不含 `/` → `None`;落在非路径字符 → `None`;段尾 `.` 保留;写满右缘且下一行行首是路径字符 → 跨行;没写满 → 不跨;向上续同理。
- [ ] `path_span(rows: &[Vec<char>], row, col) -> Option<((row,col),(row,col))>` 实现:左右扩到路径字符最大连续段,左端到 0 列且上一行最后一列是路径字符就继续上翻;右端到最后一列且下一行 0 列是路径字符就继续下翻;拼出的串含 `/` 才返回。
- [ ] Emulator:`Semantic` 分支先取可视区每行字符(`WIDE_CHAR_SPACER` 记成空格),命中 → `Selection::new(Simple, 起点, Left)` + `update(终点, Right)`;否则照旧 `Semantic`。`semantic_escape_chars = SEMANTIC_ESCAPE_CHARS + "：，。、；！？（）「」『』《》【】“”‘’"`。
- [ ] Emulator 单测:feed `设计文档：docs/a/b.md`,双击 `b` → 选区文本 `docs/a/b.md`;feed `说明：hello世界`(无 `/`)双击 `hello` → `hello`(全角冒号成分隔符)。
- [ ] VT fixture:沿用 `claude-code-wrapped-command.bin`(Ink 硬切超宽 token 的真机字节),双击被切开的那条路径的后半截 → 选出整条跨行路径。
- [ ] `cargo test -p mullion-term`,提交 `feat(term): 双击路径选中整条、全角标点作词边界 (F371)`。

### Task 3: F374 Ctrl+Shift+B 用最近一次终端复制

**Files:** Modify `crates/mullion-app/src/files/reveal.rs`(`Recall`、`recall`)、`crates/mullion-app/src/app.rs`(`last_terminal_copy` 字段、`copy_selection`/`copy_selection_one_line` 写点、`reveal_target_of` 加来源 pane 参数、`apply_files_hotkey`、`RevealTarget.pane`、`reveal_remote` 改归属)

- [ ] `recall(memo: Option<&str>, same_tab: bool, clipboard: Option<&str>) -> Recall { Use, Unknown, Forget }` 测试:无记忆 / 跨标签 → `Forget`;剪贴板读不到 → `Unknown`;相等 → `Use`;不等 → `Forget`。
- [ ] `TerminalCopy { text, generation, pane }`;两个复制函数在 `clipboard.set(&text)` 之后记(Alt 复制记改人称后的那份)。
- [ ] `apply_files_hotkey`:侧栏开着、焦点选区认不出路径时查记忆;`Use` → 按来源 pane 的 cwd/host 解析;`Unknown` → toast「剪贴板暂时读不到,再按一次」且不关栏。
- [ ] `reveal_remote`:把 `files_owner_pane` 改成 `(generation, 来源 pane)`;跨机重开改走带目标的 `reopen_sftp_on`(来源 pane 不一定是焦点)。
- [ ] 源码守护:写点在 `clipboard.set` 之后;`reveal_remote` 改归属。提交 `feat(app): Ctrl+Shift+B 认最近一次终端复制的路径,跳到来源 pane 的节点 (F374)`。

### Task 4: F372 侧栏 SFTP 对账

**Files:** Modify `crates/mullion-app/src/app.rs`

- [ ] 纯函数 `reconcile_plan_of(owner, owner_generation, owner_host_ix, has_client, loading, sftp_host_ix, tried) -> Option<ReconcileKey>`,测试覆盖四道门、一致不动、不一致给键、同键尝试过不再给、换节点 = 新键。
- [ ] `about_to_wait` 调 `reconcile_sidebar_sftp()`:取归属 pane 的 host_ix/cwd,命中则记 `sftp_reconcile_tried` 并 `reopen_sftp_on(gen, Some(SftpAt{host_ix, cwd}))`。零 IO。
- [ ] 抽 `trigger_sftp_open_at` / `reopen_sftp_on`(旧名留作焦点版包装),既有守护的锚点随之改到新函数体。
- [ ] 根因:换节点(`on_pane_rehosted`)时若 sftp 还在开(`has_client=false`),F341 → `SyncPlan::Nothing`,开完连的是旧机器;多 pane 时归属 pane 不是焦点也不同步。对账收敛两者。
- [ ] 接线守护:`about_to_wait` 调用对账;对账走 `reconcile_plan_of`。提交 `fix(app): 侧栏 SFTP 按归属 pane 的节点对账 (F372)`。

### Task 5: 绿 + 复核 + 发版

- [ ] `cargo test --workspace`、`clippy -D warnings`、`fmt --check`。
- [ ] review subagent 复核 F372/F374(SFTP 换机器 + 剪贴板)。
- [ ] 升 0.1.146,走 release-windows。
