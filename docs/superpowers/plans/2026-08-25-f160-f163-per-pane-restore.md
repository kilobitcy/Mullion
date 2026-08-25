# F160–F163 叶子级现场恢复 实现 plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让「新开 exe 恢复现场」把每块 pane 恢复到**它当初那台机器上的那个 tmux 会话**里，而不是只摆回分屏形状。

**Architecture:** 落盘的身份从标签级下沉到叶子级（`SavedNodeEntry` 加两个 `Option` 字段）；attach 的真值源从「会话配置里写的 tmux 名」换成「远端标题上报的实测会话名」；跨机器的叶子复用「换节点」链路串行拨号。所有判据都做成纯函数放进 `mullion-store` / `shell/layout_snapshot.rs` / 新建的 `shell/restore_plan.rs`，`app.rs` 只做接线——这样「哪块 pane 接哪个会话」这类**错了也看不出来**的判据全部可单测。

**Tech Stack:** Rust 2021 / serde + toml（`mullion-store`）/ 现有 `Workspace` + `on_pane_ready` 收口点（`mullion-app`）。无新依赖。

**设计 spec：** `docs/superpowers/specs/2026-08-25-f160-f163-per-pane-restore-design.md`（决策 D1–D10 的「为什么不是别的」在那里，本文不重复）。

---

## 与 spec 的两处偏差（动手前先读）

**① D3「无 SessionId 的 pane」的触发条件不可达，换了一个可达的等价场景。**

写 plan 时核对源码发现：`SavedTab.session_id` 是 `SessionId` **不是** `Option`
（`crates/mullion-store/src/layout.rs:116`），而 `snapshot_tabs_of` 在
`app.rs:1352` 用 `(TabContent::Terminal(_) | TabContent::Files(_), None) => continue`
把**整个**快速连接标签跳过——它压根不落盘。所以「叶子没有任何 session_id」这条路
走不通：叶子缺字段时回落到 `SavedTab.session_id`，而它必然存在。

但 D3 要防的那件事（**摆出形状但拨不了号的叶子不能被静默丢掉，否则分屏比例变形**）
有一个真实可达的入口：**叶子指向的会话已经被用户删掉了**。用户「换节点」把一块 pane
搬到会话 7 上，之后在会话管理器里删了会话 7 —— 标签本身的会话还在（`usable` 不会丢
这个标签），但那个叶子拨不出去。

因此：**D3 的机制照做**（占位 pane + 说明文字，与 D6 的失败 pane 共用一套承载），
只是**入口判据**从「没有 session_id」换成「session_id 不在库里」。守护测试相应改名为
`a_leaf_whose_session_is_gone_is_kept_as_a_placeholder_not_dropped`。

**② `SavedNodeEntry` 会失去 `Copy`。**

加 `Option<String>` 之后 `#[derive(Copy)]` 编不过。这不是可选项——spec §4 的 TOML
格式把 `tmux` 放在叶子上。受影响的只有 3 处测试代码（Task 1 步骤 5 逐条列了），
不影响任何生产代码路径。

---

## 文件结构

| 文件 | 职责 | 动作 |
|---|---|---|
| `crates/mullion-store/src/layout.rs` | 磁盘格式：`SavedNodeEntry` 叶子身份字段 | 改 |
| `crates/mullion-store/src/automation.rs` | 只 attach 不新建的命令串（D2） | 改 |
| `crates/mullion-store/src/lib.rs` | 新 API 的 re-export | 改 |
| `crates/mullion-app/src/shell/layout_snapshot.rs` | `LeafIdentity`、`to_entries` 带身份、`leaf_identities` 读回 | 改 |
| `crates/mullion-app/src/shell/restore_plan.rs` | **新建**：主叶子选择 + 每叶子路由 + `-d` 键（D5） | 建 |
| `crates/mullion-app/src/shell/mod.rs` | 挂新模块 | 改 |
| `crates/mullion-app/src/automation.rs` | `pending_for_measured_attach`（F161 决策层） | 改 |
| `crates/mullion-app/src/shell/workspace/mod.rs` | `PaneState` 两个新字段、`apply_saved_tree` 落位参数 | 改 |
| `crates/mullion-app/src/ui/pane_title.rs` | pane 上挂提示（F163/D4、D6） | 改 |
| `crates/mullion-app/src/app.rs` | 全部接线 | 改 |

---

### Task 1: `SavedNodeEntry` 带叶子身份（F160 数据格式）

**Files:**
- Modify: `crates/mullion-store/src/layout.rs:60-88`（结构体 + `impl`）、`crates/mullion-store/src/layout.rs:336-360`（两条既有测试的结构体字面量）
- Modify: `crates/mullion-app/src/shell/layout_snapshot.rs:394-450`（测试里的 `Copy` 用法）
- Test: `crates/mullion-store/src/layout.rs`（同文件 `mod tests`）

- [ ] **Step 1: 写会失败的测试**

在 `crates/mullion-store/src/layout.rs` 的 `mod tests` 里，紧跟在
`a_half_written_split_is_not_mistaken_for_a_leaf` 之后加：

```rust
    /// F160:叶子带上身份字段之后**仍然是叶子**。
    ///
    /// `is_leaf` 只看 `dir`/`ratio` —— 把身份字段也算进去的话,一个记了
    /// tmux 名的叶子会被解码器当成分割节点,整棵树判坏,那个标签直接被
    /// `usable` 丢掉(现象:恢复列表里的标签莫名其妙少了几个)。
    ///
    /// 自证会变红:把 `is_leaf` 改成 `self.dir.is_none() && self.ratio.is_none()
    /// && self.session_id.is_none()`。
    #[test]
    fn a_leaf_that_carries_an_identity_is_still_a_leaf() {
        let l = SavedNodeEntry::leaf_with(Some(SessionId(7)), Some("web01".into()));
        assert!(l.is_leaf(), "带身份的叶子被当成了分割节点");
        assert_eq!(l.session_id, Some(SessionId(7)));
        assert_eq!(l.tmux.as_deref(), Some("web01"));
        assert_eq!(l.dir, None);
        assert_eq!(l.ratio, None);
    }

    /// F160:身份字段是 `Option` + `skip_serializing_if` —— 没有身份的叶子
    /// 编出来必须还是**空表**,与今天的文件逐字节一致。
    ///
    /// 这条钉的是**降级兼容的另一半**(D9 不升 schema 的前提):新版 exe 写出来
    /// 的、没有身份的那些叶子,旧版 exe 读到的东西跟它自己写的一模一样。
    ///
    /// 自证会变红:把两个新字段的 `skip_serializing_if` 去掉。
    #[test]
    fn a_leaf_without_an_identity_is_still_encoded_as_an_empty_entry() {
        let one = SavedLayout {
            schema_version: CURRENT_LAYOUT_SCHEMA,
            active_tab: 0,
            updated_at: 0,
            window: None,
            tabs: vec![SavedTab {
                kind: SavedTabKind::Terminal,
                session_id: SessionId(1),
                title: "t".into(),
                focus_leaf: 0,
                tree: vec![SavedNodeEntry::leaf()],
            }],
        };
        let text = toml::to_string_pretty(&one).unwrap();
        assert!(
            !text.contains("session_id = ") || text.matches("session_id").count() == 1,
            "空叶子不该写出 session_id(只有 [[tab]] 那一处该有):\n{text}"
        );
        assert!(!text.contains("tmux"), "空叶子不该写出 tmux:\n{text}");
    }

    /// F160:身份字段经过真实文件一个来回不变形。
    ///
    /// 会话名里**故意带空格和单引号** —— TOML 的字符串转义与后面 F161 的
    /// shell 转义是两套东西,这里钉的是前者。
    #[test]
    fn leaf_identities_round_trip_through_the_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let before = SavedLayout {
            schema_version: CURRENT_LAYOUT_SCHEMA,
            active_tab: 0,
            updated_at: 0,
            window: None,
            tabs: vec![SavedTab {
                kind: SavedTabKind::Terminal,
                session_id: SessionId(3),
                title: "prod".into(),
                focus_leaf: 1,
                tree: vec![
                    SavedNodeEntry::split(SavedDir::Horizontal, 0.4),
                    SavedNodeEntry::leaf_with(Some(SessionId(3)), Some("web 01".into())),
                    SavedNodeEntry::leaf_with(Some(SessionId(7)), Some("it's me".into())),
                ],
            }],
        };
        save(dir.path(), &before).unwrap();
        let got = load(dir.path());
        assert!(got.note.is_none(), "正常读回不该降级:{:?}", got.note);
        assert_eq!(got.layout, before);
    }

    /// D9:**旧版 exe 写出来的记录**(叶子只有 dir/ratio,没有身份字段)照样读得回来,
    /// 身份字段是 `None`。没有这条,升级之后第一次启动会把用户上次的现场整份判坏。
    #[test]
    fn a_file_written_by_an_older_exe_still_parses_with_empty_identities() {
        let text = r#"
schema_version = 1
active_tab = 0
updated_at = 0

[[tab]]
kind = "terminal"
session_id = 3
title = "prod"
focus_leaf = 0

  [[tab.tree]]
  dir = "horizontal"
  ratio = 0.5

  [[tab.tree]]

  [[tab.tree]]
"#;
        let got: SavedLayout = toml::from_str(text).expect("旧格式必须还读得动");
        assert_eq!(got.tabs[0].tree.len(), 3);
        assert!(got.tabs[0].tree[1].is_leaf());
        assert_eq!(got.tabs[0].tree[1].session_id, None);
        assert_eq!(got.tabs[0].tree[1].tmux, None);
    }
```

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test -p mullion-store --lib layout::tests 2>&1 | tail -20`
Expected: 编译失败，`no function or associated item named 'leaf_with' found`。

- [ ] **Step 3: 改结构体**

把 `crates/mullion-store/src/layout.rs:60-88` 整段替换成：

```rust
/// 树的**前序遍历**里的一项。
///
/// 为什么是扁平数组而不是嵌套结构:TOML 没有 enum,serde 的内部标签
/// (`#[serde(tag = ...)]`)走 `Content` 缓冲、跟 toml 的「值必须在表之前」
/// 规则相性很差,而递归的 `Box<Node>` 编出来是一串 `[tab.tree.a.b.a]` 这种
/// 人读不了也手改不了的深表。
///
/// 扁平编码还白得一个性质:**「树损坏」变成可判定的解码失败**。截断的数组
/// 拼不出完整的树,`from_entries` 直接返回 `None`,不需要另外定义「什么样的
/// 树算坏」。
///
/// 编码规则:`dir`/`ratio` 同时有值 = 二分节点,它的两棵子树紧跟在后面
/// (先 a 后 b);两者都没有 = 叶子。
///
/// F160:叶子还带**身份**(`session_id`/`tmux`)。分割节点上这两个恒为 `None`。
/// **不派生 `Copy`** —— `tmux` 是 `String`。这不是疏忽:身份必须跟叶子走,
/// 摆在别处(比如 `SavedTab` 上一个平行数组)就会出现「树改了、身份没跟着改」
/// 的错位,而错位的现象是某块 pane 接回了另一块的 tmux 会话。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SavedNodeEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<SavedDir>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ratio: Option<f32>,
    /// F160:这个叶子当初连的是哪条会话记录。`None` = 旧版 exe 写的(读回时
    /// 回落到 `SavedTab::session_id`,见 `layout_snapshot::leaf_identities`)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// F160/D1:关 exe 那一刻这块 pane **实际所在**的 tmux 会话名
    /// (F123/F124 由远端标题上报,不是会话配置里写的那个)。
    /// `None` = 当初不在 tmux 里 / 远端没开 `set-titles`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux: Option<String>,
}

impl SavedNodeEntry {
    /// 一个没有身份的叶子。
    pub fn leaf() -> Self {
        Self::default()
    }

    /// F160:一个带身份的叶子。
    pub fn leaf_with(session_id: Option<SessionId>, tmux: Option<String>) -> Self {
        Self {
            dir: None,
            ratio: None,
            session_id,
            tmux,
        }
    }

    /// 一个二分节点。
    pub fn split(dir: SavedDir, ratio: f32) -> Self {
        Self {
            dir: Some(dir),
            ratio: Some(ratio),
            session_id: None,
            tmux: None,
        }
    }

    /// 这一项是不是叶子。**两个字段必须一起判**:只判 `dir` 的话,一个
    /// 手改出来的 `{ dir = "horizontal" }`(漏了 ratio)会被当成分割节点,
    /// 然后拿 `unwrap_or` 补一个默认比例 —— 那是在猜用户的布局。
    ///
    /// F160:**身份字段不参与这条判据**。参与的话,一个记了 tmux 名的叶子
    /// 会被当成分割节点,整棵树判坏,那个标签被 `usable` 整个丢掉。
    /// 守护:`a_leaf_that_carries_an_identity_is_still_a_leaf`。
    pub fn is_leaf(&self) -> bool {
        self.dir.is_none() && self.ratio.is_none()
    }
}
```

- [ ] **Step 4: 修既有测试里的结构体字面量**

`crates/mullion-store/src/layout.rs` 里 `a_half_written_split_is_not_mistaken_for_a_leaf`
的两个字面量补 `..Default::default()`：

```rust
        let half = SavedNodeEntry {
            dir: Some(SavedDir::Horizontal),
            ratio: None,
            ..Default::default()
        };
        assert!(!half.is_leaf());
        let other = SavedNodeEntry {
            dir: None,
            ratio: Some(0.5),
            ..Default::default()
        };
        assert!(!other.is_leaf());
```

- [ ] **Step 5: 修 `Copy` 消失导致的三处编译错**

`crates/mullion-app/src/shell/layout_snapshot.rs`：

1. `corrupt_encodings_are_rejected_rather_than_guessed_at`（约 394 行）开头三行改成：

```rust
        let split = SavedNodeEntry::split(SavedDir::Horizontal, 0.5);
        let leaf = SavedNodeEntry::leaf();
        let half = SavedNodeEntry {
            dir: Some(SavedDir::Horizontal),
            ratio: None,
            ..Default::default()
        };
```

并把该测试里所有 `&[split, leaf]` / `&[leaf, leaf]` / `&[split, leaf, leaf]` /
`&[half, leaf, leaf]` / `&[half]` 形式的数组字面量改成 `clone()` 版本，例如：

```rust
        assert_eq!(leaf_count(&[]), None, "空数组:没有树");
        assert_eq!(leaf_count(&[split.clone()]), None, "分割节点缺了两棵子树");
        assert_eq!(
            leaf_count(&[split.clone(), leaf.clone()]),
            None,
            "分割节点只有一棵子树"
        );
        assert_eq!(leaf_count(&[leaf.clone(), leaf.clone()]), None, "尾部有多余项");
        assert_eq!(
            leaf_count(&[half.clone(), leaf.clone(), leaf.clone()]),
            None,
            "半拉的分割节点"
        );

        assert_eq!(from_entries(&[], &[]), None);
        assert_eq!(
            from_entries(&[leaf.clone(), leaf.clone()], &ids(2)),
            None,
            "尾部多余项"
        );
        assert_eq!(
            from_entries(&[split.clone(), leaf.clone(), leaf.clone()], &ids(3)),
            None,
            "id 数比叶子多:调用方算错了,不该悄悄用前两个"
        );
        assert_eq!(
            from_entries(&[split.clone(), leaf.clone(), leaf.clone()], &ids(1)),
            None,
            "id 数比叶子少"
        );
        assert_eq!(
            from_entries(&[half], &[]),
            None,
            "半拉的分割节点被当成了叶子"
        );
```

2. `insane_ratios_from_a_hand_edited_file_are_clamped`（约 434 行）里两处数组：

```rust
        let leaf = SavedNodeEntry::leaf();
        for (given, expect) in [(0.0f32, 0.05f32), (1.0, 0.95), (-3.0, 0.05), (17.0, 0.95)] {
            let e = [
                SavedNodeEntry::split(SavedDir::Vertical, given),
                leaf.clone(),
                leaf.clone(),
            ];
            let Some(Node::Split { ratio, .. }) = from_entries(&e, &ids(2)) else {
                panic!("该拼得出来");
            };
            assert_eq!(ratio, expect, "ratio {given} 没夹住");
        }
        let e = [
            SavedNodeEntry::split(SavedDir::Vertical, f32::NAN),
            leaf.clone(),
            leaf,
        ];
```

- [ ] **Step 6: 跑测试确认通过**

Run: `cargo test -p mullion-store 2>&1 | tail -5`
Expected: `test result: ok.`

Run: `cargo test -p mullion-app --lib shell::layout_snapshot 2>&1 | tail -5`
Expected: `test result: ok.`

- [ ] **Step 7: 提交**

```bash
git add crates/mullion-store/src/layout.rs crates/mullion-app/src/shell/layout_snapshot.rs
git commit -m "feat(store): SavedNodeEntry 叶子带 session_id + tmux 身份 (F160)

is_leaf 刻意不看身份字段,否则记了 tmux 名的叶子会被当成分割节点、
整棵树判坏。守护:a_leaf_that_carries_an_identity_is_still_a_leaf、
a_file_written_by_an_older_exe_still_parses_with_empty_identities(D9)。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 2: 只 attach、不新建的命令串（F161 / D2）

**Files:**
- Modify: `crates/mullion-store/src/automation.rs`（在 `tmux_command` 之后加新函数）
- Modify: `crates/mullion-store/src/lib.rs:42` 附近（re-export）
- Test: `crates/mullion-store/src/automation.rs`（同文件 `mod tests`）

- [ ] **Step 1: 写会失败的测试**

在 `crates/mullion-store/src/automation.rs` 的 `mod tests` 末尾加：

```rust
    /// D2 红线:按实测名接回去的命令**绝不新建会话**。
    ///
    /// 会话已经不在了(远端重启过 / 用户自己 kill 了),正确的表现是命令失败、
    /// 停在裸 shell,而不是凭空造一个同名空会话让用户以为「接回来了」。
    ///
    /// 自证会变红:在 `attach_only_command` 末尾拼回 `|| exec tmux new-session -s {q}`。
    #[test]
    fn the_attach_command_never_creates_a_session() {
        let cmd = attach_only_command("web01", false);
        assert!(
            !cmd.contains("new-session"),
            "只 attach 的命令串里出现了 new-session:{cmd}"
        );
        assert!(cmd.contains("attach"), "总得真的 attach 才行:{cmd}");
    }

    /// D2 的另一半:`has-session` 守门**不能省**。
    ///
    /// 裸的 `exec tmux attach -t X` 在会话不存在时,`exec` 已经把 shell 换成了
    /// tmux 进程,tmux 报错退出 → channel 关闭 → 这块 pane 当场死掉。那样 D4 的
    /// 「挂提示」和 D8 的「停在裸 shell」全部落空 —— shell 都没了。
    /// 有守门则 `&&` 短路,shell 原地活着。
    ///
    /// 自证会变红:把 `attach_only_command` 改成 `format!("exec tmux attach{d} -t {q}")`。
    #[test]
    fn a_failed_attach_leaves_the_shell_alive() {
        let cmd = attach_only_command("web01", false);
        assert!(
            cmd.starts_with("tmux has-session -t "),
            "没有 has-session 守门,attach 失败会连 shell 一起带走:{cmd}"
        );
        assert!(
            cmd.contains("2>/dev/null &&"),
            "守门必须用 `&&` 短路(探测失败就什么都不做):{cmd}"
        );
    }

    /// D5:带不带 `-d` 是**参数**,两种都要真的编进命令串。
    ///
    /// 自证会变红:把 `detach_others` 参数忽略掉、恒不加 `-d`(或恒加)。
    #[test]
    fn the_detach_flag_is_actually_emitted() {
        assert!(attach_only_command("a", true).contains("attach -d -t "));
        assert!(!attach_only_command("a", false).contains(" -d "));
    }

    /// 会话名走 `shell_quote`,不裸拼。远端会话名里带 `'` 或 `$(...)` 时,
    /// 裸拼就是把用户 tmux 会话名当 shell 代码执行。
    ///
    /// 自证会变红:把 `shell_quote(name)` 换成 `name.to_string()`。
    #[test]
    fn the_session_name_is_shell_quoted() {
        let cmd = attach_only_command("it's $(id)", false);
        assert!(
            cmd.contains(r#"'it'\''s $(id)'"#),
            "会话名没走 shell_quote:{cmd}"
        );
        assert!(!cmd.contains("-t it's"), "裸拼进去了:{cmd}");
    }

    /// F161:**实测名不过 `sanitize_tmux_name`**。
    ///
    /// 那个函数是给「拿会话记录的名字去**新建** tmux 会话」用的,它把
    /// `.`/`:`/`$`/`%`/`=`/`@` 换成 `-`。实测名是远端 tmux 自己报上来的
    /// `#S`,本来就合法;再 sanitize 一遍会把 `web@01` 改成 `web-01`,
    /// 于是 attach 到一个根本不存在的名字上。
    ///
    /// 自证会变红:在 `build_plan_attach_measured` 里对 `name` 调一次
    /// `sanitize_tmux_name`。
    #[test]
    fn a_measured_name_is_not_sanitized_because_the_remote_already_vouched_for_it() {
        let a = ResolvedAutomation {
            enabled: true,
            tmux: None,
            commands: Vec::new(),
            work_dir: None,
            env: Vec::new(),
            initial_delay_ms: 300,
            inter_delay_ms: 200,
            ready_timeout_ms: 15_000,
        };
        let steps = build_plan_attach_measured(&a, "web@01", false);
        let line = String::from_utf8(steps[0].bytes.clone()).unwrap();
        assert!(line.contains("'web@01'"), "实测名被改写了:{line}");
    }

    /// 空名字不发命令 —— `tmux attach -t ''` 是一条必然失败的残命令。
    #[test]
    fn an_empty_measured_name_produces_no_plan() {
        let a = ResolvedAutomation {
            enabled: true,
            tmux: None,
            commands: Vec::new(),
            work_dir: None,
            env: Vec::new(),
            initial_delay_ms: 300,
            inter_delay_ms: 200,
            ready_timeout_ms: 15_000,
        };
        assert!(build_plan_attach_measured(&a, "", false).is_empty());
        assert!(build_plan_attach_measured(&a, "   ", false).is_empty());
    }

    /// 自动化总开关关着时,实测 attach 也不发。
    ///
    /// 与 `build_plan`/`build_plan_reattach` 同口径(它们靠 `tmux_session_name`
    /// 里那句 `if !a.enabled` 把关)。用户明确关掉了「登录后自动化」,我们就
    /// 一个字节都不发 —— 哪怕我们自认为这一条对他有好处。
    ///
    /// 自证会变红:删掉 `build_plan_attach_measured` 里的 `if !a.enabled` 早退。
    #[test]
    fn turning_automation_off_also_turns_off_the_measured_attach() {
        let a = ResolvedAutomation {
            enabled: false,
            tmux: None,
            commands: Vec::new(),
            work_dir: None,
            env: Vec::new(),
            initial_delay_ms: 300,
            inter_delay_ms: 200,
            ready_timeout_ms: 15_000,
        };
        assert!(build_plan_attach_measured(&a, "web01", true).is_empty());
    }
```

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test -p mullion-store --lib automation 2>&1 | tail -20`
Expected: 编译失败，`cannot find function 'attach_only_command'`。

- [ ] **Step 3: 实现**

在 `crates/mullion-store/src/automation.rs` 里 `fn tmux_command` 之后插入：

```rust
/// F161/D1+D2:按**实测**会话名接回 tmux 的那一行命令。
///
/// 与 [`tmux_command`] 的唯一差别:**砍掉 `|| exec tmux new-session` 那半段**。
/// 这是 D2 的红线 —— 实测名是「关 exe 那一刻那块 pane 真的在里面」的会话,
/// 它不在了就说明远端重启过或用户自己 kill 了,凭空造一个同名空会话只会让
/// 用户以为现场回来了。**永不替用户在远端造东西。**
///
/// **`has-session` 守门必须留着**,砍掉的只是 `||` 后半段:裸的
/// `exec tmux attach -t X` 在会话不存在时,`exec` 已经把 shell 替换成 tmux
/// 进程,tmux 报错退出 → channel 关闭 → **pane 当场死掉**,D4 的「挂提示」和
/// D8 的「停在裸 shell」全部落空。守门之后 `&&` 短路,shell 原地活着。
/// 探测与 attach 之间的竞态窗口沿用 [`tmux_command`] 里「已知且接受」的结论。
///
/// `name` **不过 `sanitize_tmux_name`**:那是给「拿配置里的名字去新建」用的,
/// 会把 `@` 之类合法字符改掉。实测名是远端 tmux 自己报的 `#S`,已经合法。
pub fn attach_only_command(name: &str, detach_others: bool) -> String {
    let q = shell_quote(name);
    let d = if detach_others { " -d" } else { "" };
    format!("tmux has-session -t {q} 2>/dev/null && exec tmux attach{d} -t {q}")
}

/// F161:按实测会话名接回 tmux 的一步计划。空计划 = 不发任何字节。
///
/// 恒**恰好一个 Step**(与 `build_plan` 的 tmux 分支同一个不变量):attach
/// 一旦生效,屏幕就归那个 TUI 了,之后再发任何字节都是打进 TUI(D7)。
///
/// 返回空的两种情形:自动化总开关关着;名字去掉空白后为空。
pub fn build_plan_attach_measured(
    a: &ResolvedAutomation,
    name: &str,
    detach_others: bool,
) -> Vec<Step> {
    if !a.enabled || name.trim().is_empty() {
        return Vec::new();
    }
    vec![Step {
        delay: Duration::from_millis(u64::from(a.initial_delay_ms)),
        bytes: line(&attach_only_command(name, detach_others)),
    }]
}
```

- [ ] **Step 4: re-export**

`crates/mullion-store/src/lib.rs` 里找到导出 automation 那一组（含 `build_plan`、
`build_plan_reattach`、`build_plan_without_tmux`、`tmux_session_name`、`shell_quote`
的 `pub use`），把 `attach_only_command` 和 `build_plan_attach_measured` 加进去。

```bash
grep -n "build_plan_reattach" crates/mullion-store/src/lib.rs
```

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p mullion-store 2>&1 | tail -5`
Expected: `test result: ok.`

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-store/src/automation.rs crates/mullion-store/src/lib.rs
git commit -m "feat(store): 按实测名只 attach 不新建的 tmux 命令串 (F161)

D2 红线:砍掉 || exec tmux new-session,但 has-session 守门必须留着 ——
裸 exec attach 失败会把 shell 一起带走,pane 当场死。守护:
the_attach_command_never_creates_a_session、a_failed_attach_leaves_the_shell_alive。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 3: `LeafIdentity` + `to_entries` 带身份 + 读回（F160 判据层）

**Files:**
- Modify: `crates/mullion-app/src/shell/layout_snapshot.rs:32-48`（`to_entries`/`push_entries`）
- Test: `crates/mullion-app/src/shell/layout_snapshot.rs`（同文件 `mod tests`）

- [ ] **Step 1: 写会失败的测试**

在 `crates/mullion-app/src/shell/layout_snapshot.rs` 的 `mod tests` 里，
紧跟在 `leaves_are_renumbered_in_preorder_with_the_ids_given` 之后加：

```rust
    /// F160:每个叶子写进盘里的身份,是**它自己那块 pane** 的,不是标签默认的。
    ///
    /// 用户「换节点」把第二块 pane 搬到别的机器上之后,`ws.hosts` 里有两台,
    /// 而磁盘上只记得第一台 —— 恢复时所有 pane 一起拨向那一个会话
    /// (spec §1.1 症状②)。
    ///
    /// 自证会变红:把 `to_entries` 传给 `identity` 的 `PaneId` 换成恒定的
    /// 第一个叶子 id。
    #[test]
    fn a_leaf_carries_the_host_it_was_actually_on_not_the_tab_default() {
        let tree = sample_tree(); // 叶子按前序是 PaneId(1), PaneId(2), PaneId(3)
        let entries = to_entries(&tree, &|id: PaneId| LeafIdentity {
            session_id: Some(SessionId(u64::from(id.0) * 10)),
            tmux: Some(format!("s{}", id.0)),
        });
        let leaves: Vec<&SavedNodeEntry> = entries.iter().filter(|e| e.is_leaf()).collect();
        assert_eq!(leaves.len(), 3);
        assert_eq!(leaves[0].session_id, Some(SessionId(10)));
        assert_eq!(leaves[1].session_id, Some(SessionId(20)));
        assert_eq!(leaves[2].session_id, Some(SessionId(30)));
        assert_eq!(leaves[2].tmux.as_deref(), Some("s3"));
    }

    /// F160:分割节点上**恒无身份**。有的话文件里会出现一个语义不明的
    /// 「有 dir 又有 session_id」的项,下一版读它的人无从判断该信哪个。
    #[test]
    fn a_split_node_never_carries_an_identity() {
        let entries = to_entries(&sample_tree(), &|_| LeafIdentity {
            session_id: Some(SessionId(9)),
            tmux: Some("x".into()),
        });
        for e in entries.iter().filter(|e| !e.is_leaf()) {
            assert_eq!(e.session_id, None, "分割节点带上了身份:{e:?}");
            assert_eq!(e.tmux, None, "分割节点带上了身份:{e:?}");
        }
    }

    /// F160 读回:身份按**前序**给出来,顺序与 `from_entries` 发号的顺序一致。
    ///
    /// 错位的现象是「某块 pane 接回了另一块的 tmux 会话」,肉眼看不出来,
    /// 所以必须机械对拍。
    #[test]
    fn identities_come_back_in_the_same_preorder_the_ids_are_handed_out() {
        let entries = to_entries(&sample_tree(), &|id: PaneId| LeafIdentity {
            session_id: Some(SessionId(u64::from(id.0))),
            tmux: Some(format!("s{}", id.0)),
        });
        let got = leaf_identities(&entries, SessionId(99)).expect("结构完整");
        assert_eq!(
            got.iter().map(|i| i.tmux.clone()).collect::<Vec<_>>(),
            vec![
                Some("s1".to_string()),
                Some("s2".to_string()),
                Some("s3".to_string())
            ]
        );
    }

    /// D9 降级:旧版 exe 写的记录(叶子没有身份字段)回落到 `SavedTab::session_id`
    /// —— 也就是今天的标签级行为。没有这条,升级后第一次启动会把所有叶子都
    /// 判成「没有身份」。
    ///
    /// 自证会变红:把 `leaf_identities` 里的 `.or(Some(tab_session))` 去掉。
    #[test]
    fn an_old_record_without_leaf_fields_falls_back_to_the_tab_session() {
        let old = vec![
            SavedNodeEntry::split(SavedDir::Horizontal, 0.5),
            SavedNodeEntry::leaf(),
            SavedNodeEntry::leaf(),
        ];
        let got = leaf_identities(&old, SessionId(42)).expect("结构完整");
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|i| i.session_id == Some(SessionId(42))));
        assert!(
            got.iter().all(|i| i.tmux.is_none()),
            "旧记录里没有实测 tmux 名,不许凭空补一个"
        );
    }

    /// 回落**只填 session,不填 tmux**;而叶子自己记了 session 时不许被标签的
    /// 那个盖掉。两条分支各喂一种输入 —— 只喂一种的话,两道防御会互相掩护
    /// (`subagent-driven-review-lessons` 记的恒绿模式)。
    #[test]
    fn a_leaf_that_recorded_its_own_session_is_not_overwritten_by_the_tab_default() {
        let mixed = vec![
            SavedNodeEntry::split(SavedDir::Horizontal, 0.5),
            SavedNodeEntry::leaf_with(Some(SessionId(7)), Some("web01".into())),
            SavedNodeEntry::leaf(),
        ];
        let got = leaf_identities(&mixed, SessionId(3)).expect("结构完整");
        assert_eq!(got[0].session_id, Some(SessionId(7)), "被标签默认盖掉了");
        assert_eq!(got[0].tmux.as_deref(), Some("web01"));
        assert_eq!(got[1].session_id, Some(SessionId(3)), "该回落的没回落");
        assert_eq!(got[1].tmux, None);
    }

    /// 坏编码给 `None`,判据与 `leaf_count` 同源 —— 两处各写一遍的话,会出现
    /// 「树拼不出来但身份表拼出来了」这种谁也想不到的中间状态。
    #[test]
    fn a_corrupt_encoding_has_no_identities_either() {
        assert_eq!(leaf_identities(&[], SessionId(1)), None);
        assert_eq!(
            leaf_identities(&[SavedNodeEntry::split(SavedDir::Vertical, 0.5)], SessionId(1)),
            None
        );
    }
```

同时把该文件测试模块顶部的 `use super::*;` 之后补上 `use mullion_store::SessionId;`
（若已由 `super::*` 带入则跳过——`layout_snapshot.rs:14` 已 `use mullion_store::SessionId;`，
`super::*` 会带进来，无需再加）。

**还要修既有测试**：`a_tree_round_trips_through_the_flat_encoding`、
`the_focus_index_counts_leaves_in_the_same_order_the_encoder_writes_them`、
`leaves_are_renumbered_in_preorder_with_the_ids_given`、
`a_single_leaf_tree_round_trips` 里的 `to_entries(&tree)` 全部改成
`to_entries(&tree, &|_| LeafIdentity::default())`。

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test -p mullion-app --lib shell::layout_snapshot 2>&1 | tail -20`
Expected: 编译失败，`cannot find type 'LeafIdentity'`。

- [ ] **Step 3: 实现**

把 `crates/mullion-app/src/shell/layout_snapshot.rs:32-48` 整段替换成：

```rust
/// F160:一个叶子的**身份** —— 它连的是哪条会话、当初在哪个 tmux 会话里。
///
/// 两个字段的真值源不同,别混:`session_id` 来自 `HostConn`(拨号参数的出处),
/// `tmux` 来自 F123/F124 远端标题上报的实测值(D1:配置只在实测为空时回落)。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LeafIdentity {
    pub session_id: Option<SessionId>,
    pub tmux: Option<String>,
}

/// 运行时树 → 磁盘格式(前序遍历)。**丢掉 `PaneId`**,只留结构、比例与身份(E5/F160)。
///
/// `identity` 回答「这个 pane 该往盘上写什么身份」。**传闭包而不是 `&Workspace`**:
/// 同 `usable` 的 `known` —— 判据要能脱离一个真实工作区单测,而且这一层不该知道
/// 「身份是量出来的还是从盘上读回来的」(那条优先级在 app 侧的 `leaf_identity_of`,
/// 见设计 5.2②)。
pub fn to_entries(root: &Node, identity: &dyn Fn(PaneId) -> LeafIdentity) -> Vec<SavedNodeEntry> {
    let mut out = Vec::new();
    push_entries(root, identity, &mut out);
    out
}

fn push_entries(
    node: &Node,
    identity: &dyn Fn(PaneId) -> LeafIdentity,
    out: &mut Vec<SavedNodeEntry>,
) {
    match node {
        Node::Leaf(id) => {
            let i = identity(*id);
            out.push(SavedNodeEntry::leaf_with(i.session_id, i.tmux));
        }
        Node::Split { dir, ratio, a, b } => {
            out.push(SavedNodeEntry::split(to_saved_dir(*dir), *ratio));
            push_entries(a, identity, out);
            push_entries(b, identity, out);
        }
    }
}

/// F160 读回:磁盘编码里每个叶子的身份,**按前序**。
///
/// 顺序与 [`from_entries`] 发号的顺序完全一致 —— 先用 [`leaf_count`] 验过结构,
/// 之后线性过滤出来的叶子序就是前序叶子序(`is_leaf` 刻意不看身份字段,
/// 见 `SavedNodeEntry::is_leaf`)。两处各写一遍遍历会让某块 pane 接回另一块的
/// tmux 会话,而那种错位肉眼看不出来。
///
/// `tab_session` = `SavedTab::session_id`。叶子自己没记 session 时回落到它 ——
/// 旧版 exe 写出来的记录整份都没有叶子字段,靠这条降级成今天的标签级行为(D9)。
/// **回落只补 session,不补 tmux**:旧记录里根本没有实测名,凭空补一个就是猜。
///
/// `None` = 编码损坏(判据同 [`leaf_count`])。
pub fn leaf_identities(entries: &[SavedNodeEntry], tab_session: SessionId) -> Option<Vec<LeafIdentity>> {
    leaf_count(entries)?;
    Some(
        entries
            .iter()
            .filter(|e| e.is_leaf())
            .map(|e| LeafIdentity {
                session_id: e.session_id.or(Some(tab_session)),
                tmux: e.tmux.clone(),
            })
            .collect(),
    )
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib shell::layout_snapshot 2>&1 | tail -5`
Expected: `test result: ok.`（`app.rs` 里 `to_entries` 的调用点此时还没改，
整个 crate 编不过是预期的——本步只跑本模块的测试即可，Task 6 会补上接线。
若 `--lib` 因整 crate 编译失败而跑不起来，先按 Task 6 Step 3 把 `snapshot_tabs_of`
的调用改掉再回来跑。）

> **执行提示**：Rust 的 `cargo test --lib` 要整个 crate 编过。为避免中间态编不过，
> 本 Task 与 Task 6 的 Step 3 可以合并成一次提交；但**测试必须先写**。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/shell/layout_snapshot.rs
git commit -m "feat(app): 布局快照按叶子写入身份并能读回 (F160)

to_entries 收 identity 闭包而不是 &Workspace —— 判据要能脱离真实工作区单测,
而且这一层不该知道身份是量出来的还是从盘上读回来的(设计 5.2②)。
守护:a_leaf_carries_the_host_it_was_actually_on_not_the_tab_default、
an_old_record_without_leaf_fields_falls_back_to_the_tab_session。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 4: `restore_plan` 模块——主叶子、路由、`-d` 键（F161/F162 判据层）

**Files:**
- Create: `crates/mullion-app/src/shell/restore_plan.rs`
- Modify: `crates/mullion-app/src/shell/mod.rs`

- [ ] **Step 1: 挂模块**

在 `crates/mullion-app/src/shell/mod.rs` 里，按既有 `pub mod` 的字母序位置加一行：

```rust
pub mod restore_plan;
```

- [ ] **Step 2: 写会失败的测试（连同模块骨架一起建文件，但函数体先 `todo!()`）**

建 `crates/mullion-app/src/shell/restore_plan.rs`，**先只写模块文档 + 类型 + 签名（体为 `todo!()`）+ 全部测试**：

```rust
//! F161/F162:恢复一个标签时,每个叶子该怎么处理 —— 主叶子选谁、哪些叶子要
//! 另拨一台机器、谁的 `attach` 该带 `-d`。**纯函数,零 egui/winit/tokio/store IO**。
//!
//! 这些判据全是「错了也看不出来、直到某天接到别人的会话上」的那一类,所以
//! 一条都不留在 `app.rs` 的事件分支里(那里要一个真的 `App` + 真的 SSH 连接
//! 才跑得起来,等于只能靠人眼验)。

use mullion_store::SessionId;

use crate::shell::layout_snapshot::LeafIdentity;

/// 一个叶子在恢复时该走哪条路(F162,设计 5.2)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeafPlan {
    /// 跟主叶子同一条会话:在**已有**的那条 SSH 连接上另开一条 channel
    /// (今天 `spawn_fresh_panes` 走的路)。
    SameHost,
    /// 另一台机器:排进串行拨号队列,走「换节点」那条链路(D10)。
    Dial(SessionId),
    /// 会话已经不在库里(用户把它删了)。摆出形状、挂一句说明,**不拨号**(D3)。
    ///
    /// 不丢掉这个叶子:丢了分屏比例会静默变形 —— 存的是 2×2,恢复回来变三块,
    /// 而没有任何提示。
    Orphan,
}

/// F162:哪个叶子当「主叶子」—— **前序第一个身份还连得上**的叶子。
///
/// 它决定标签用哪条会话去 `spawn_connect`(今天那条路,零改动),连上之后
/// 已有的那块 pane 就落在这个叶子位上(见 `Workspace::apply_saved_tree` 的
/// `main_leaf` 参数,设计 5.2①)。
///
/// `known` 回答「这条会话现在还在库里吗」。传闭包而不是 `&SessionStore`:
/// store 打不开时(keyring 不可用)照样要能跑这段判据,而且这样才测得动
/// (同 `layout_snapshot::usable`)。
///
/// `None` = 一个能连的叶子都没有 —— 整个标签保持占位态。
pub fn main_leaf(
    identities: &[LeafIdentity],
    known: &dyn Fn(SessionId) -> bool,
) -> Option<(usize, SessionId)> {
    todo!()
}

/// F162:每个叶子的路由。`main` = [`main_leaf`] 选出来的那条会话。
pub fn plan_leaves(
    identities: &[LeafIdentity],
    main: SessionId,
    known: &dyn Fn(SessionId) -> bool,
) -> Vec<LeafPlan> {
    todo!()
}

/// D5:一批要 attach 的叶子里,哪几块该带 `-d`。
///
/// **键是(机器, 会话名)二元组**,不是会话名。pane A 在机器 X 的会话 `a`、
/// pane B 在机器 Y 的会话 `a` 是两台 tmux 服务器上两个互不相干的会话,
/// **都**该带 `-d`(各踢各的残骸);只按名字去重会让 B 白白不踢。
/// 机器一侧用叶子的 `session_id` —— 同一条会话记录 = 同一台机器,恢复场景里
/// 没有比它更细的机器身份。
///
/// 为什么第一块要带 `-d`:exe 崩溃/强杀之后远端 tmux client 会残留到 TCP 超时,
/// 不踢的话两个 client 同时挂着,tmux 的 `window-size` 会跟着两边尺寸反复
/// reflow(F141 的原始理由)。
/// 为什么其余不能带:第二块会把第一块踢成 detached,恢复出来一块死屏。
pub fn detach_flags(leaves: &[LeafIdentity]) -> Vec<bool> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(session: u64, tmux: Option<&str>) -> LeafIdentity {
        LeafIdentity {
            session_id: Some(SessionId(session)),
            tmux: tmux.map(str::to_string),
        }
    }

    fn nothing() -> LeafIdentity {
        LeafIdentity::default()
    }

    /// 主叶子是前序第一个**连得上**的,不是恒第 0 个。
    ///
    /// 自证会变红:把 `main_leaf` 改成恒返回 `Some((0, ..))`。
    #[test]
    fn the_main_leaf_is_the_first_one_that_can_still_be_dialed() {
        let ids = [leaf(7, None), leaf(3, Some("a"))];
        // 会话 7 被用户删了 → 主叶子该是第 1 个。
        let got = main_leaf(&ids, &|s| s == SessionId(3));
        assert_eq!(got, Some((1, SessionId(3))));
    }

    #[test]
    fn a_tab_with_no_dialable_leaf_has_no_main_leaf() {
        assert_eq!(main_leaf(&[leaf(7, None)], &|_| false), None);
        assert_eq!(main_leaf(&[nothing()], &|_| true), None);
        assert_eq!(main_leaf(&[], &|_| true), None);
    }

    /// F162:同一条会话的叶子复用已有连接,别的会话排队另拨。
    ///
    /// 自证会变红:把 `plan_leaves` 里的 `s == main` 判断去掉(全变 `Dial`)——
    /// 那样恢复一个普通的 2×2 单机标签会凭空拨 3 次号,每次一个密码框。
    #[test]
    fn leaves_on_the_main_session_reuse_the_connection_and_the_rest_get_queued() {
        let ids = [leaf(3, Some("a")), leaf(3, None), leaf(7, Some("b"))];
        let got = plan_leaves(&ids, SessionId(3), &|_| true);
        assert_eq!(
            got,
            vec![LeafPlan::SameHost, LeafPlan::SameHost, LeafPlan::Dial(SessionId(7))]
        );
    }

    /// D3(按 plan 开头「与 spec 的偏差①」调整过入口):会话被删掉的叶子
    /// **摆出来**,不丢掉。丢掉的话分屏比例会静默变形 —— 存的是 2×2,
    /// 恢复回来变三块,而没有任何提示。
    ///
    /// 自证会变红:把 `Orphan` 那条分支删掉、改成不产出这个叶子。
    #[test]
    fn a_leaf_whose_session_is_gone_is_kept_as_a_placeholder_not_dropped() {
        let ids = [leaf(3, Some("a")), leaf(7, Some("b")), leaf(3, None)];
        let got = plan_leaves(&ids, SessionId(3), &|s| s == SessionId(3));
        assert_eq!(got.len(), 3, "叶子数必须与树的叶子数一一对应,少一个就是变形");
        assert_eq!(got[1], LeafPlan::Orphan);
    }

    /// 身份完全缺失的叶子(理论上不可达,见 plan 开头的偏差①)同样按占位处理,
    /// **不许 panic、不许丢**。
    #[test]
    fn a_leaf_with_no_identity_at_all_is_also_a_placeholder() {
        let got = plan_leaves(&[nothing()], SessionId(3), &|_| true);
        assert_eq!(got, vec![LeafPlan::Orphan]);
    }

    /// D5 的核心:键是(机器, 会话名)。**两台机器上的同名会话都要带 `-d`**。
    ///
    /// 自证会变红:把去重键退化成只按会话名。
    #[test]
    fn the_detach_flag_is_keyed_per_host_and_session_name() {
        let ids = [leaf(3, Some("a")), leaf(7, Some("a"))];
        assert_eq!(
            detach_flags(&ids),
            vec![true, true],
            "两台机器上各自的会话 a 互不相干,都该各踢各的残骸"
        );
    }

    /// D5 的另一半:**同机同名**只有第一块带 `-d`。第二块带的话会把第一块
    /// 踢成 detached,恢复出来一块死屏。
    ///
    /// 自证会变红:全加 `-d`(第二个断言红)/ 全不加(第一个断言红)——
    /// 两个方向各扎一条,不许互相掩护。
    #[test]
    fn only_the_first_pane_on_the_same_host_session_gets_the_detach_flag() {
        let ids = [leaf(3, Some("a")), leaf(3, Some("a")), leaf(3, Some("b"))];
        assert_eq!(detach_flags(&ids), vec![true, false, true]);
    }

    /// 没有实测名的叶子根本不发 attach —— 它的标志位是什么无所谓,
    /// 但**绝不能占掉同名那一格**,否则真正要 attach 的那块就不带 `-d` 了。
    #[test]
    fn a_leaf_without_a_measured_name_does_not_consume_the_detach_slot() {
        let ids = [leaf(3, None), leaf(3, Some("a"))];
        assert_eq!(detach_flags(&ids), vec![false, true]);
    }
}
```

- [ ] **Step 3: 跑测试确认它失败**

Run: `cargo test -p mullion-app --lib shell::restore_plan 2>&1 | tail -20`
Expected: 每条测试 panic 于 `not yet implemented`。

- [ ] **Step 4: 实现三个函数体**

把三处 `todo!()` 换成：

```rust
pub fn main_leaf(
    identities: &[LeafIdentity],
    known: &dyn Fn(SessionId) -> bool,
) -> Option<(usize, SessionId)> {
    identities.iter().enumerate().find_map(|(ix, i)| {
        let s = i.session_id?;
        known(s).then_some((ix, s))
    })
}

pub fn plan_leaves(
    identities: &[LeafIdentity],
    main: SessionId,
    known: &dyn Fn(SessionId) -> bool,
) -> Vec<LeafPlan> {
    identities
        .iter()
        .map(|i| match i.session_id {
            Some(s) if s == main => LeafPlan::SameHost,
            Some(s) if known(s) => LeafPlan::Dial(s),
            // 会话被删了 / 压根没有身份:摆出来,不拨号。
            _ => LeafPlan::Orphan,
        })
        .collect()
}

pub fn detach_flags(leaves: &[LeafIdentity]) -> Vec<bool> {
    let mut seen: Vec<(Option<SessionId>, &str)> = Vec::new();
    leaves
        .iter()
        .map(|i| {
            let Some(name) = i.tmux.as_deref() else {
                // 不发 attach 的叶子不占同名那一格,否则真正要 attach 的
                // 那块就拿不到 `-d` 了。
                return false;
            };
            let key = (i.session_id, name);
            if seen.contains(&key) {
                false
            } else {
                seen.push(key);
                true
            }
        })
        .collect()
}
```

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib shell::restore_plan 2>&1 | tail -5`
Expected: `test result: ok. 8 passed`

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-app/src/shell/restore_plan.rs crates/mullion-app/src/shell/mod.rs
git commit -m "feat(app): 恢复时的叶子路由与 -d 键判据 (F161/F162)

D5 的键是(机器, 会话名)二元组:两台机器上的同名会话互不相干,都该各踢
各的残骸;只按名字去重会让第二台白白不踢。守护:
the_detach_flag_is_keyed_per_host_and_session_name、
only_the_first_pane_on_the_same_host_session_gets_the_detach_flag。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 5: `PaneState` 两个新字段 + `apply_saved_tree` 落位参数（5.2①）

**Files:**
- Modify: `crates/mullion-app/src/shell/workspace/mod.rs:162-195`（`PaneState`）、`:327-351`（`apply_saved_tree`）、`:626`（`Workspace` 测试造 pane 的地方）
- Modify: `crates/mullion-app/src/app.rs:5573`、`:6567`、`:11192`、`:14321`、`:14408`（`PaneState` 字面量）
- Test: `crates/mullion-app/src/shell/workspace/mod.rs`（同文件 `mod tests`）

- [ ] **Step 1: 写会失败的测试**

在 `crates/mullion-app/src/shell/workspace/mod.rs` 的 `mod tests` 里，
紧跟在既有的 `apply_saved_tree` 那组测试之后加：

```rust
    /// 设计 5.2①:已连上的那块 pane 必须落在**主叶子**位上。
    ///
    /// `apply_saved_tree` 原本把已有 pane 恒填第一个叶子位(`ids = keep + fresh`),
    /// 而主叶子是「前序第一个**身份还连得上**的叶子」—— 叶子 0 是个占位
    /// (会话被删了)时,已连的 pane 会占掉本该空着的那格,主叶子反而空着,
    /// 现象是「恢复回来内容全串了一格」。
    ///
    /// 自证会变红:把 `main_leaf` 参数忽略掉、恒按 0 处理。
    #[test]
    fn the_connected_pane_lands_on_the_main_leaf_not_the_first_leaf() {
        use mullion_store::{SavedDir, SavedNodeEntry};
        let (mut ws, _probes) = ws_with(1);
        let entries = vec![
            SavedNodeEntry::split(SavedDir::Horizontal, 0.5),
            SavedNodeEntry::leaf(),
            SavedNodeEntry::leaf(),
        ];
        ws.apply_saved_tree(&entries, 0, 1).expect("结构完整");
        let leaves = mullion_core::layout::leaves(ws.tree());
        assert_eq!(leaves.len(), 2);
        assert_eq!(
            leaves[1],
            PaneId(1),
            "已连上的那块 pane 该落在主叶子(第 1 个)位上,实际落在了 {leaves:?}"
        );
    }

    /// 旧调用点传 0 时行为与改动前逐字节一致。
    #[test]
    fn passing_zero_keeps_the_old_placement() {
        use mullion_store::{SavedDir, SavedNodeEntry};
        let (mut ws, _probes) = ws_with(1);
        let entries = vec![
            SavedNodeEntry::split(SavedDir::Horizontal, 0.5),
            SavedNodeEntry::leaf(),
            SavedNodeEntry::leaf(),
        ];
        ws.apply_saved_tree(&entries, 0, 0).expect("结构完整");
        assert_eq!(mullion_core::layout::leaves(ws.tree())[0], PaneId(1));
    }

    /// 越界的 `main_leaf`(文件被手改过)夹回 0,不 panic —— 恢复是启动路径,
    /// 为一份坏文件崩掉整个进程不成比例。
    #[test]
    fn an_out_of_range_main_leaf_is_clamped() {
        use mullion_store::SavedNodeEntry;
        let (mut ws, _probes) = ws_with(1);
        ws.apply_saved_tree(&[SavedNodeEntry::leaf()], 0, 9)
            .expect("单叶子");
        assert_eq!(mullion_core::layout::leaves(ws.tree()), vec![PaneId(1)]);
    }
```

`ws_with(n)` 是该模块既有的构造辅助（`crates/mullion-app/src/shell/workspace/mod.rs`
的 `pub mod tests_support`，返回 `(Workspace, Vec<Probe>)`，pane id 从 1 起）。
**复用它**，不要新造一个平行的构造函数。

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test -p mullion-app --lib shell::workspace 2>&1 | tail -20`
Expected: 编译失败，`this function takes 2 arguments but 3 arguments were supplied`。

- [ ] **Step 3: 给 `PaneState` 加两个字段**

在 `crates/mullion-app/src/shell/workspace/mod.rs` 的 `PaneState` 里，
`history_reported` 之后加：

```rust
    /// F162:`true` = 这块 pane 还没连上**它自己**那台机器 —— 排队等串行拨号的、
    /// 会话被删掉的占位(D3)、拨号失败降级的(D6)三种。
    ///
    /// 这三种 pane 都必须有 `PaneState`(没有的话画不出文字,树上只是一块短暂
    /// 空白,见 F35 的「空窗期」约定),但它们的 `host_ix` 指着**主叶子那台机器**,
    /// **不代表自己的身份**。落盘时据此判断该照运行时量、还是照抄上次从盘上
    /// 读回来的那份(设计 5.2②:不这么做的话,恢复途中被 kill 掉的 exe 会把
    /// 还没连上的叶子身份**永久**丢掉)。
    pub host_pending: bool,
    /// F163/D6:挂在这块 pane 标题条上的一句说明(「当初的会话 web01 已不存在」
    /// / 「会话已被删除,无法自动恢复」/「连接失败」)。`None` = 没话要说。
    ///
    /// **不弹窗**:多块 pane 同时失败时会连弹好几次(D4)。
    pub notice: Option<String>,
```

- [ ] **Step 4: 补全所有 `PaneState` 字面量**

以下位置各加 `host_pending: false,` 和 `notice: None,`：

- `crates/mullion-app/src/shell/workspace/mod.rs:626` 附近（测试辅助）
- `crates/mullion-app/src/app.rs:5573`（`ConnectOk` 建首块 pane）
- `crates/mullion-app/src/app.rs:6567`（`PaneOpened`）
- `crates/mullion-app/src/app.rs:11192`（测试辅助 `test_pane`）
- `crates/mullion-app/src/app.rs:14321`、`:14408`（测试里的字面量）

```bash
grep -rn "history_reported: 0," crates/mullion-app/src | cat
```
逐个跟进——`history_reported: 0,` 是每个字面量的最后一个字段，在它后面插入两行即可。

- [ ] **Step 5: 改 `apply_saved_tree` 签名与落位**

把 `crates/mullion-app/src/shell/workspace/mod.rs:327-351` 的
`apply_saved_tree` 整段替换成：

```rust
    /// 按 `layout.toml` 里存的树形状恢复分屏(F37,设计 E10)。语义与
    /// [`Workspace::apply_preset`] 完全一致:返回**待新建**的 pane id,
    /// 调用方为每个 id 发起 `open_pty`,完成后调 [`Workspace::attach_pane`]。
    ///
    /// 恢复的是**形状与比例**,pane 里的内容一律是新的 —— 终端 scrollback
    /// 从不落盘(设计 E2)。
    ///
    /// `main_leaf`(F160,设计 5.2①)= 已经连上的那块 pane 该落在**第几个**
    /// 叶子位上。恢复时主叶子是「前序第一个身份还连得上的叶子」,不一定是
    /// 第 0 个 —— 叶子 0 是个拨不了号的占位时,恒填 0 会让已连的 pane 占掉
    /// 本该空着的那格,主叶子反而空着(现象:恢复回来内容全串了一格)。
    /// 旧调用点传 `0` 行为不变。越界(文件被手改过)夹回 0,不 panic。
    ///
    /// **只在恰好保留一块 pane 时挪位**:保留多块时「哪块算已连的那块」本身
    /// 就没有定义,乱挪只会把内容互相换位。恢复路径上 `ws` 是刚建的,恒只有
    /// 一块(`Workspace::new`)。
    ///
    /// `None` = 树编码损坏,**什么都不动**。校验放在任何 mutation 之前:
    /// 中途失败会留下一个树与 `panes` 对不上的工作区,那比不恢复糟得多。
    pub fn apply_saved_tree(
        &mut self,
        entries: &[mullion_store::SavedNodeEntry],
        focus_leaf: usize,
        main_leaf: usize,
    ) -> Option<Vec<PaneId>> {
        use crate::shell::layout_snapshot as snap;
        // 先验后改。`leaf_count` 通过就意味着下面的 `from_entries` 一定能拼出来
        // (结构完整 + id 数与叶子数相等,是它仅有的两个失败条件)。
        let want = snap::leaf_count(entries)?;
        let plan = preset::plan_for_count(want, &self.statuses());
        for id in &plan.close {
            self.panes.retain(|p| p.id != *id);
        }
        let keep = plan.keep.len();
        let mut ids = plan.keep;
        let mut fresh = Vec::new();
        for _ in 0..plan.spawn {
            let id = self.alloc_id();
            ids.push(id);
            fresh.push(id);
        }
        // 5.2①:把那块已连上的 pane 从 0 号叶子位挪到主叶子位。
        // `rotate_left(1)` 把 `[a, f1, f2]` 变成 `[f1, f2, a]` —— 其余叶子
        // 依次前移,前序顺序不乱。
        if keep == 1 {
            let main = main_leaf.min(ids.len().saturating_sub(1));
            ids[0..=main].rotate_left(1);
        }
        self.tree = snap::from_entries(entries, &ids)
            .expect("leaf_count 已经验过结构,这里拼不出来说明两者的判据走样了");
        self.focus = ids[snap::sane_focus_leaf(focus_leaf, ids.len())];
        Some(fresh)
    }
```

- [ ] **Step 6: 修既有调用点与测试**

`crates/mullion-app/src/shell/workspace/mod.rs:1003`、`:1046`、`:1061` 三处测试调用
补第三个参数 `0`；`crates/mullion-app/src/app.rs:5654` 的
`t.ws.apply_saved_tree(&p.tree, p.focus_leaf)?` 暂时补 `, 0`（Task 7 会换成真的主叶子）。

- [ ] **Step 7: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib shell::workspace 2>&1 | tail -5`
Expected: `test result: ok.`

- [ ] **Step 8: 提交**

```bash
git add crates/mullion-app/src/shell/workspace/mod.rs crates/mullion-app/src/app.rs
git commit -m "feat(app): pane 带「还没连上自己那台机器」标记,恢复时已连 pane 落在主叶子 (F160/F162)

设计 5.2①:apply_saved_tree 原本把已有 pane 恒填第 0 个叶子位,主叶子不是
第 0 个时内容会整体串一格。5.2②:host_pending 是「落盘该量还是该照抄」的
判据,少了它恢复途中被 kill 会把未连叶子的身份永久丢掉。
守护:the_connected_pane_lands_on_the_main_leaf_not_the_first_leaf。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 6: 落盘接线——`snapshot_tabs_of` 真的写出叶子身份（F160 接线）

**Files:**
- Modify: `crates/mullion-app/src/app.rs:1315-1360`（`snapshot_tabs_of`）+ 新增自由函数
- Test: `crates/mullion-app/src/app.rs`（同文件 `mod tests`）

- [ ] **Step 1: 先补两个测试辅助**

`app.rs` 的测试模块里已经有 `arm_of`（切 `match` 分支体）和它依赖的
`brace_balanced_arm`（`crates/mullion-app/src/app.rs:13586`、`:13607`），但**没有**
切具名函数体的。本轮的接线守护要切函数，补一个同源的：

```rust
    /// 取一个**具名函数**的块体。与 `arm_of` 同源(共用 `brace_balanced_arm`),
    /// 只是锚点从 `模式 => {` 换成函数签名的开头。
    ///
    /// `sig` 给到函数名加左括号(`"fn snapshot_tabs_of("`)——光给函数名会命中
    /// 调用处,截出来的是别处的代码,断言全部落空(同 `arm_of` 文档里那条)。
    fn body_of<'a>(production: &'a str, sig: &str) -> &'a str {
        let at = production
            .find(sig)
            .unwrap_or_else(|| panic!("找不到 {sig} —— 这条测试的锚点失效了"));
        let rest = &production[at + production[at..].find('{').expect("函数没有块体")..];
        let body = brace_balanced_arm(rest);
        assert!(
            body.len() < rest.len(),
            "{sig} 没截到闭合大括号,断言会退化成扫全文件"
        );
        body
    }

    /// `app.rs` 去掉测试模块之后的那一半。源码切片断言**必须**先切掉测试模块,
    /// 否则测试自己写的那句字面量就能把断言喂饱,恒绿。
    fn prod_src() -> &'static str {
        let src = include_str!("app.rs");
        let (prod, _) = src
            .split_once("\n#[cfg(test)]\nmod tests {")
            .expect("app.rs 的测试模块分界变了,所有源码切片断言的锚点都失效了");
        prod
    }

```

> **不要造假 `HostConn`。** 它的 `handle: Arc<SshConnection>` 攥着一条真的 russh
> `Handle`（`crates/mullion-ssh/src/session.rs:125`），无头环境里造不出来——这正是
> 本项目所有碰 `HostConn` 的守护都退化成源码切片断言的原因。
> 所以 Step 4 的 `leaf_identity_of` **不收 `&Workspace`**，只收它真正需要的两样
> （一个 `host_ix → SessionId` 的闭包 + 那块 `PaneState`），判据因此能在无头环境里
> 真跑。`PaneState` 用既有的 `test_pane(id)` 就能造（`app.rs:11190`，挂 `NullPty`）。

- [ ] **Step 2: 写会失败的测试**

在 `crates/mullion-app/src/app.rs` 的 `mod tests` 里加（放在既有
`the_auto_dial_*` 那组附近即可）：

```rust
    /// 设计 5.2②:恢复途中拍的快照**不许**把还没连上的叶子身份冲掉。
    ///
    /// `save_layout_if_changed` 每 2 秒从运行时状态现算快照。串行拨号进行中,
    /// 排队的叶子还没有自己的 `HostConn`,它的 `host_ix` 指着主叶子那台机器
    /// —— 照着量会把它的身份写成**别人的**;写 `None` 则半路 kill 掉 exe 之后
    /// 这条身份**永久丢失**。两种都是数据损坏。
    ///
    /// 自证会变红:把 `leaf_identity_of` 里的 `host_pending` 分支删掉,
    /// 改成一律查 `hosts[host_ix]`。
    #[test]
    fn a_snapshot_taken_mid_restore_keeps_the_pending_leaf_identities() {
        use crate::shell::layout_snapshot::LeafIdentity;
        use mullion_store::SessionId;

        // 排队等拨号的那块 pane:`host_ix` 仍是 0(主叶子那台机器)。
        let mut queued = test_pane(2);
        queued.host_pending = true;
        queued.tmux = Some("主叶子那台机器上的会话".into());

        let wanted = vec![(
            PaneId(2),
            LeafIdentity {
                session_id: Some(SessionId(7)),
                tmux: Some("web01".into()),
            },
        )];

        let got = leaf_identity_of(&|_| Some(SessionId(3)), Some(&queued), &wanted, PaneId(2));
        assert_eq!(
            got.session_id,
            Some(SessionId(7)),
            "排队中的叶子被写成了主叶子那台机器的身份"
        );
        assert_eq!(got.tmux.as_deref(), Some("web01"));
    }

    /// 另一半:**已经连上**的 pane 身份必须现量,不能照抄盘上那份。
    ///
    /// 用户在远端 `tmux detach` 之后 `p.tmux` 变 `None`,这才是事实(D1:
    /// 真值源是实测)。照抄盘上那份会让「已经退出 tmux」的 pane 下次恢复
    /// 又被塞回一个会话里。
    ///
    /// 两条分支各喂一种输入 —— 只喂一种会让两道防御互相掩护。
    #[test]
    fn a_connected_leaf_is_measured_not_copied_from_disk() {
        use crate::shell::layout_snapshot::LeafIdentity;
        use mullion_store::SessionId;

        let mut live = test_pane(1);
        live.host_pending = false;
        live.tmux = None; // 用户刚在远端 detach 出来 —— 这才是事实
        let wanted = vec![(
            PaneId(1),
            LeafIdentity {
                session_id: Some(SessionId(7)),
                tmux: Some("stale".into()),
            },
        )];
        let got = leaf_identity_of(&|ix| (ix == 0).then_some(SessionId(3)), Some(&live), &wanted, PaneId(1));
        assert_eq!(got.session_id, Some(SessionId(3)), "该现量的没量");
        assert_eq!(got.tmux, None, "陈旧的 tmux 名被写回盘上了");
    }

    /// 接线守护:`snapshot_tabs_of` 真的把身份传给了 `to_entries`,不是传了个
    /// 空闭包。本项目反复踩过「纯函数写对了没接线」。
    ///
    /// 自证会变红:把 `snapshot_tabs_of` 里的 identity 闭包换成
    /// `&|_| LeafIdentity::default()`。
    #[test]
    fn the_leaf_identity_actually_reaches_the_snapshot() {
        let body = body_of(prod_src(), "fn snapshot_tabs_of(");
        assert!(
            body.contains("leaf_identity_of("),
            "snapshot_tabs_of 没有把真实身份传给 to_entries:\n{body}"
        );
    }
```

- [ ] **Step 3: 跑测试确认它失败**

Run: `cargo test -p mullion-app --lib a_snapshot_taken_mid_restore 2>&1 | tail -20`
Expected: 编译失败，`cannot find function 'leaf_identity_of'`。

- [ ] **Step 4: 实现 `leaf_identity_of` 并接进 `snapshot_tabs_of`**

在 `crates/mullion-app/src/app.rs` 里 `fn snapshot_tabs_of` **之前**插入：

```rust
/// F160:一个叶子该往盘上写什么身份(设计 5.2②)。
///
/// 两条来源,优先级不能反:
/// - **已连上**的 pane(`host_pending == false`)→ 现量:会话看
///   `hosts[host_ix].session_id`,tmux 名看 `PaneState::tmux`(F123/F124 远端
///   标题上报的实测值)。D1 的真值源就是它 —— 用户在远端 `tmux switch-client`
///   切过之后,只有实测值是对的。
/// - **还没连上**的叶子(排队 / 占位 / 失败,`host_pending == true`,以及树上
///   有叶子但 `PaneState` 还没挂上的那段空窗期)→ 照抄恢复时从盘上读回来的
///   那份。它的 `host_ix` 指着主叶子那台机器,现量会把身份写成别人的;写空
///   则半路 kill 掉 exe 之后这条身份**永久丢失**。
///
/// 写成自由函数(而不是 `Workspace` 的方法)是因为这条优先级不属于工作区 ——
/// 「盘上那份」住在 `TerminalTab` 上,而 `Workspace` 不该知道有磁盘这回事。
///
/// **不收 `&Workspace`,只收真正需要的两样。** `HostConn` 攥着一条真的 russh
/// `Handle`,无头环境里造不出来 —— 收整个工作区的话这条判据就只能退化成源码
/// 切片断言,而它恰恰是那种「错了也看不出来、直到某天写错身份」的判据。
/// `host_session` 回答 `hosts[ix].session_id`。
fn leaf_identity_of(
    host_session: &dyn Fn(usize) -> Option<SessionId>,
    pane: Option<&crate::shell::workspace::PaneState>,
    wanted: &[(PaneId, crate::shell::layout_snapshot::LeafIdentity)],
    id: PaneId,
) -> crate::shell::layout_snapshot::LeafIdentity {
    use crate::shell::layout_snapshot::LeafIdentity;
    if let Some(p) = pane {
        if !p.host_pending {
            return LeafIdentity {
                session_id: host_session(p.host_ix),
                tmux: p.tmux.clone(),
            };
        }
    }
    wanted
        .iter()
        .find(|(pid, _)| *pid == id)
        .map(|(_, w)| w.clone())
        .unwrap_or_default()
}
```

再把 `snapshot_tabs_of` 里 Terminal 那条分支的 `tree` 一行改成：

```rust
            (TabContent::Terminal(t), Some(session_id)) => SavedTab {
                kind: SavedTabKind::Terminal,
                session_id,
                title: tab.title.clone(),
                focus_leaf: snap::focus_leaf_index(t.ws.tree(), t.ws.focus()),
                // F160:每个叶子写它**自己**那块 pane 的身份 —— 换过节点之后
                // `ws.hosts` 里有两台机器,只写标签级那一个会让恢复时所有 pane
                // 一起拨向第一台(spec §1.1 症状②)。
                tree: snap::to_entries(t.ws.tree(), &|id| {
                    leaf_identity_of(
                        &|ix| t.ws.hosts.get(ix).and_then(|h| h.session_id),
                        t.ws.pane(id),
                        &t.leaf_wanted,
                        id,
                    )
                }),
            },
```

以及 Files 分支的单叶子：

```rust
            // D1:SFTP 节点标签没有分屏树 —— 恒一个叶子。它没有 tmux,身份就是
            // 标签自己那条会话。
            (TabContent::Files(_), Some(session_id)) => SavedTab {
                kind: SavedTabKind::Files,
                session_id,
                title: tab.title.clone(),
                focus_leaf: 0,
                tree: vec![SavedNodeEntry::leaf_with(Some(session_id), None)],
            },
```

- [ ] **Step 5: 给 `TerminalTab` 加 `leaf_wanted`**

在 `crates/mullion-app/src/app.rs` 的 `struct TerminalTab` 里加：

```rust
    /// F160/F161:恢复出来、**还没连上**的那些叶子分别是什么身份、attach 该不该
    /// 带 `-d`。一块 pane 连上并把 attach 发出去之后,它这一条就被取走
    /// (`on_pane_ready`)—— 此后身份改由运行时实测(设计 5.2②)。
    ///
    /// 落在标签上而不是 `Workspace` 上:它是「上次盘上那份」,而 `Workspace`
    /// 不该知道有磁盘这回事(架构不变量)。
    leaf_wanted: Vec<(PaneId, crate::shell::layout_snapshot::LeafIdentity)>,
    /// F161:这块 pane 下一次「就绪」时,attach 要不要带 `-d`(D5)。
    /// 与 `leaf_wanted` 同进同退,拆成两个表只是因为前者要参与落盘、后者不要。
    leaf_detach: Vec<(PaneId, bool)>,
```

并在**所有** `TerminalTab { ... }` 字面量里补 `leaf_wanted: Vec::new(),` 和
`leaf_detach: Vec::new(),`（`grep -n "TerminalTab {" -A12 crates/mullion-app/src/app.rs` 逐个定位）。

- [ ] **Step 6: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED|panicked" | tail -5`
Expected: `test result: ok.`

- [ ] **Step 7: 提交**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): 布局快照按叶子写入真实身份 (F160)

leaf_identity_of 的优先级不能反:已连上的现量(D1 真值源是实测),没连上的
照抄盘上那份 —— 后者的 host_ix 指着主叶子那台机器,现量会写成别人的身份,
写空则半路 kill 掉 exe 就永久丢失(设计 5.2②)。守护:
a_snapshot_taken_mid_restore_keeps_the_pending_leaf_identities、
a_connected_leaf_is_measured_not_copied_from_disk、
the_leaf_identity_actually_reaches_the_snapshot。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 7: 恢复时读回叶子身份、按主叶子拨号（F162 接线，第一半）

**Files:**
- Modify: `crates/mullion-app/src/app.rs:2539-2600`（`reconnect_tab`）、`:5647-5664`（恢复分支）
- Test: `crates/mullion-app/src/app.rs`

- [ ] **Step 1: 写会失败的测试**

```rust
    /// F162 接线:恢复一个标签时,拨号拨的是**主叶子**那条会话,不是
    /// `SavedTab::session_id`。
    ///
    /// 叶子 0 的会话被用户删了时,照 `SavedTab::session_id` 拨会连上一台
    /// 「树上其实没有任何叶子属于它」的机器。
    ///
    /// 自证会变红:把 `reconnect_tab` 里那句 `restore_plan::main_leaf(...)`
    /// 删掉、改回直接用 `r.session_id`。
    #[test]
    fn reconnecting_a_restored_tab_dials_the_main_leaf_session() {
        let body = body_of(prod_src(), "fn reconnect_tab(");
        assert!(
            body.contains("restore_plan::main_leaf("),
            "reconnect_tab 还在照标签级 session_id 拨号:\n{body}"
        );
    }
```

（源码切片断言是**弱**断言，只用来钉「接线没被删掉」。真正的判据已经在 Task 4
的 `the_main_leaf_is_the_first_one_that_can_still_be_dialed` 里扎住了。
两者缺一不可——本项目反复踩过「纯函数写对了没接线」。）

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test -p mullion-app --lib reconnecting_a_restored_tab 2>&1 | tail -10`
Expected: FAILED，`reconnect_tab 还在照标签级 session_id 拨号`。

- [ ] **Step 3: 改 `reconnect_tab`**

先看现状：

```bash
sed -n '2539,2600p' crates/mullion-app/src/app.rs
```

把「取 `r.session_id` 去 `ssh_config_for` / `spawn_connect`」那段改成先算主叶子。
在取 `RestoredTab` 之后、拨号之前插入：

```rust
        // F162:拨的是**主叶子**那条会话 —— 前序第一个身份还连得上的叶子。
        // 照 `SavedTab::session_id` 拨的话,叶子 0 的会话被用户删掉之后,
        // 会连上一台「树上其实没有任何叶子属于它」的机器。
        let known: Vec<SessionId> = self
            .store
            .as_ref()
            .map(|s| s.list().iter().map(|r| r.id).collect())
            .unwrap_or_default();
        let identities =
            crate::shell::layout_snapshot::leaf_identities(&tree, saved_session)?;
        let Some((main_leaf, session_id)) =
            crate::shell::restore_plan::main_leaf(&identities, &|s| known.contains(&s))
        else {
            // 一个能连的叶子都没有(会话全被删了)。保持占位态,别把标签
            // 的 `dialing` 置起来 —— 那会让「重连」按钮永久灰着。
            self.ui
                .set_error("这个标签里的会话都已经不在库里了,无法恢复".to_string());
            return false;
        };
```

其中 `tree`/`saved_session` 取自 `RestoredTab`（`r.tree.clone()` / `r.session_id`）。
`leaf_identities` 返回 `Option`，`?` 在返回 `bool` 的函数里用不了——用
`let Some(identities) = ... else { return false };`，并在 else 分支记一行
`log::warn!(target: "mullion", "恢复:标签的树编码坏了,不拨号")`。

然后把 `PendingRestore` 加一个字段并在这里填上：

```rust
/// F37:一次「占位标签重连」在途期间要记住的东西(E9)。
struct PendingRestore {
    /// 连上之后**就地替换**的是这个标签,不是「当前活动标签」——
    /// 拨号要几百毫秒,期间用户完全可能切到别的标签去。
    tab_id: shell::tabs::TabId,
    /// 上次的分屏树(扁平前序编码)。
    tree: Vec<mullion_store::SavedNodeEntry>,
    /// 上次焦点落在第几个叶子。
    focus_leaf: usize,
    /// F162:已连上的那块 pane 该落在第几个叶子位上(设计 5.2①)。
    main_leaf: usize,
    /// F160:每个叶子的身份,**按前序**。连上之后照它给每个叶子分派
    /// (同机器 / 另拨一台 / 占位),并在 pane 连上之前替它们保管身份
    /// (设计 5.2②)。
    identities: Vec<crate::shell::layout_snapshot::LeafIdentity>,
}
```

- [ ] **Step 4: 改恢复分支（`app.rs:5647` 附近）**

把那段 `if replaced { ... }` 整个替换成：

```rust
        // F37/F160:是重连一个占位标签 → 把上次的分屏形状搭回来,给每个叶子
        // 按它**自己**的身份分派:同一条会话的在已有连接上开 channel(F35 那条路),
        // 别的会话排进串行拨号队列(F162),会话已被删的摆成占位(D3)。
        // 树坏了(`apply_saved_tree` 返回 `None`)就保持单屏,不阻断连接。
        if replaced {
            if let Some(p) = pending.as_ref() {
                let known: Vec<SessionId> = self
                    .store
                    .as_ref()
                    .map(|s| s.list().iter().map(|r| r.id).collect())
                    .unwrap_or_default();
                let plans = crate::shell::restore_plan::plan_leaves(
                    &p.identities,
                    // 主叶子那条会话就是这次拨通的这条。
                    p.identities[p.main_leaf]
                        .session_id
                        .expect("main_leaf 选出来的叶子必有 session_id"),
                    &|s| known.contains(&s),
                );
                let detach = crate::shell::restore_plan::detach_flags(&p.identities);
                let laid_out = self
                    .tabs
                    .by_generation_mut(generation)
                    .and_then(|tab| tab.content.as_terminal_mut())
                    .and_then(|t| {
                        let fresh = t.ws.apply_saved_tree(&p.tree, p.focus_leaf, p.main_leaf)?;
                        // 恢复出来的形状一般不对应任何预设按钮;单叶子
                        // 例外(它就是 Single)。
                        t.current_preset = (p.tree.len() == 1).then_some(Preset::Single);
                        // 叶子(前序)→ pane id。`leaves` 与 `to_entries` /
                        // `leaf_identities` 共用同一条前序约定,不许在这里
                        // 另写一遍遍历。
                        let leaves = mullion_core::layout::leaves(t.ws.tree());
                        // 5.2②:身份先由标签替它们保管,连上之后才切回实测。
                        t.leaf_wanted = leaves
                            .iter()
                            .zip(p.identities.iter())
                            .map(|(id, i)| (*id, i.clone()))
                            .collect();
                        t.leaf_detach = leaves
                            .iter()
                            .zip(detach.iter())
                            .map(|(id, d)| (*id, *d))
                            .collect();
                        Some((leaves, fresh))
                    });
                if let Some((leaves, fresh)) = laid_out {
                    self.dispatch_restored_leaves(generation, &leaves, &plans, p.main_leaf, fresh);
                }
            }
        }
```

- [ ] **Step 5: 写 `dispatch_restored_leaves`（骨架，只处理 `SameHost`）**

在 `spawn_fresh_panes` 之前插入：

```rust
    /// F162:恢复出来的叶子各走各的路。
    ///
    /// - `SameHost` → 在这个标签已有的那条连接上开 channel(F35 那条路)。
    /// - `Dial(s)`  → 排进**串行**拨号队列(D10;并发会同时弹好几个密码框 /
    ///   主机指纹确认)。走「换节点」链路,不新写第二条拨号路径 ——
    ///   第二条一定会漏掉防连点那道闸。
    /// - `Orphan`   → 摆一块占位 pane,挂一句说明,不拨号(D3)。
    ///
    /// `main_leaf` 那个叶子已经是连上的那块 pane 了,跳过。
    fn dispatch_restored_leaves(
        &mut self,
        generation: u64,
        leaves: &[PaneId],
        plans: &[crate::shell::restore_plan::LeafPlan],
        main_leaf: usize,
        fresh: Vec<PaneId>,
    ) {
        use crate::shell::restore_plan::LeafPlan;
        let mut same_host = Vec::new();
        for (ix, (id, plan)) in leaves.iter().zip(plans.iter()).enumerate() {
            if ix == main_leaf {
                continue;
            }
            // `fresh` 是 `apply_saved_tree` 新分配的那些 —— 只有它们需要开
            // channel / 拨号。不在里面的是已经有 `PaneState` 的(理论上只有
            // 主叶子那块)。
            if !fresh.contains(id) {
                continue;
            }
            match plan {
                LeafPlan::SameHost => same_host.push(*id),
                LeafPlan::Dial(s) => self.restore_dial.push_back((generation, *id, *s)),
                LeafPlan::Orphan => self.place_orphan_pane(generation, *id),
            }
        }
        self.spawn_fresh_panes(same_host);
        self.drive_restore_dial();
    }
```

`restore_dial` / `place_orphan_pane` / `drive_restore_dial` 在 Task 8 实现——
**本步先只加 `SameHost` 那条能编过的最小实现**：把 `Dial`/`Orphan` 两条分支
暂时替换成 `log::warn!(target: "mullion", "F162 未接线的叶子 {}", id.0)`，
并去掉最后那句 `self.drive_restore_dial();`。Task 8 再补回来。

- [ ] **Step 6: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED|panicked" | tail -5`
Expected: `test result: ok.`

- [ ] **Step 7: 提交**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): 恢复时按叶子身份分派,拨主叶子那条会话 (F162)

叶子 0 的会话被删掉时,照 SavedTab::session_id 拨会连上一台树上没有任何
叶子属于它的机器。守护:reconnecting_a_restored_tab_dials_the_main_leaf_session
(接线)+ the_main_leaf_is_the_first_one_that_can_still_be_dialed(判据)。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 8: 跨机器串行拨号 + 占位/失败 pane（F162 接线，第二半 / D3 / D6 / D10）

**Files:**
- Modify: `crates/mullion-app/src/app.rs`（`App` 新字段、`dispatch_restored_leaves`、`PaneRehosted` / `PaneRehostErr` 分支）
- Test: `crates/mullion-app/src/app.rs`

- [ ] **Step 1: 写会失败的测试**

```rust
    /// D10:跨机器恢复**一条接一条**。并发会同时弹好几个密码框 / 主机指纹
    /// 确认框,用户根本分不清哪个框对应哪台机器。
    ///
    /// 判据放在纯函数上:队列里有在途的那一条时,`take_next_restore_dial`
    /// 必须什么都不给。
    ///
    /// 自证会变红:把 `in_flight` 那道判断去掉。
    #[test]
    fn restoring_a_two_host_tab_dials_serially() {
        let mut q = std::collections::VecDeque::from(vec![
            (1u64, PaneId(2), mullion_store::SessionId(7)),
            (1u64, PaneId(3), mullion_store::SessionId(9)),
        ]);
        let first = take_next_restore_dial(&mut q, false).expect("该发起第一条");
        assert_eq!(first.1, PaneId(2));
        assert_eq!(
            take_next_restore_dial(&mut q, true),
            None,
            "上一条还在途,不许并发拨第二条"
        );
        let second = take_next_restore_dial(&mut q, false).expect("上一条收口后该轮到第二条");
        assert_eq!(second.1, PaneId(3));
        assert_eq!(take_next_restore_dial(&mut q, false), None);
    }

    /// D6:一台机器连不上,**只有那块 pane** 变成断开态,其余照常用。
    ///
    /// 为什么不是全或无:一台机器关机就让另外两台也连不成,不成比例。
    ///
    /// 自证会变红:把 `PaneRehostErr` 分支里的
    /// `self.degrade_restored_pane(` 换成 `self.wind_down(`(整标签退回占位)。
    #[test]
    fn one_unreachable_host_only_disconnects_its_own_pane() {
        let body = body_of(prod_src(), "fn on_pane_rehost_err(");
        assert!(
            body.contains("degrade_restored_pane("),
            "换节点失败没有做 pane 级降级:\n{body}"
        );
        assert!(
            !body.contains("wind_down("),
            "整个标签退回了占位态 —— 一台机器关机不该让另外两台也用不了:\n{body}"
        );
    }
```

（`body_of` 的锚点是 `fn 名(`，所以这条测试要求 `UserEvent::PaneRehostErr` 那条
**多行模式**的分支体先抽成具名方法——`arm_of` 的锚点是 `模式 => {`，对多行模式
拼不出来。抽方法是纯搬运，见 Step 3。`App God object 重构` 那一轮就在做同样的事。）

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test -p mullion-app --lib restoring_a_two_host_tab 2>&1 | tail -10`
Expected: 编译失败，`cannot find function 'take_next_restore_dial'`。

- [ ] **Step 3: 先把两条 rehost 分支抽成具名方法（纯搬运，先跑绿）**

`UserEvent::PaneRehosted { .. }` / `UserEvent::PaneRehostErr { .. }` 是多行模式，
`arm_of` 切不动。抽成 `fn on_pane_rehosted(&mut self, generation, pane, handle, ssh, rx)`
与 `fn on_pane_rehost_err(&mut self, generation, pane, msg)`，分支只留一句调用。
**只搬不改**，搬完先跑一次全量测试确认没红，再改语义。

Run: `cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED" | tail -3`
Expected: 除本 Task 新加的两条外全绿。

- [ ] **Step 4: 实现串行队列**

`App` 加两个字段：

```rust
    /// F162:恢复途中还要拨向**别的机器**的那些叶子。一条接一条,不并发
    /// (D10:并发会同时弹好几个密码框 / 主机指纹确认)。
    /// 三元组 =(标签世代, 那块 pane, 目标会话)。
    restore_dial: std::collections::VecDeque<(u64, PaneId, SessionId)>,
    /// F162:上面那条队列里有没有一条正在途中。`PaneRehosted`/`PaneRehostErr`
    /// 抵达时复位 —— **两条路径都要复位**,漏一条队列就永久停在这里。
    restore_dial_busy: bool,
```

在 `App::new`（`app.rs:1967` 附近，`auto_dial: None,` 那一带）里初始化：

```rust
            restore_dial: std::collections::VecDeque::new(),
            restore_dial_busy: false,
```

自由函数（放在 `next_auto_dial` 旁边）：

```rust
/// F162/D10:串行拨号队列该不该发起下一条。`in_flight` = 上一条还在途中。
///
/// 抽成自由函数,理由同 `next_auto_dial`:`App` 要 `EventLoopProxy`,单测里
/// 造不出来,而「不许并发」这条性质**必须**测得动 —— 破了它的现象是屏幕上
/// 同时叠着三个密码框,而这在无头环境里一个断言都写不出来。
fn take_next_restore_dial(
    queue: &mut std::collections::VecDeque<(u64, PaneId, SessionId)>,
    in_flight: bool,
) -> Option<(u64, PaneId, SessionId)> {
    if in_flight {
        return None;
    }
    queue.pop_front()
}
```

驱动方法：

```rust
    /// F162:推进跨机器恢复的串行拨号。每帧调(挂在 `drive_*` 那一组里)。
    ///
    /// **遍历的是队列不是活动标签**:队列里的三元组自带世代号,拨号途中用户
    /// 完全可能切到别的标签去(记忆里那条「`drive_*` 每帧驱动函数必须遍历
    /// 全部标签」的同源教训)。
    fn drive_restore_dial(&mut self) {
        let Some((generation, pane, session)) =
            take_next_restore_dial(&mut self.restore_dial, self.restore_dial_busy)
        else {
            return;
        };
        // 世代号即路由键:标签已经被关掉了就跳过这一条,接着试下一条。
        if self.tabs.by_generation(generation).is_none() {
            self.drive_restore_dial();
            return;
        }
        self.restore_dial_busy = true;
        // 复用「换节点」那条链路(D10)。不新写第二条拨号路径 —— 第二条
        // 一定会漏掉 `pending_rehost` 那道防连点的闸。
        self.spawn_rehost_on(generation, pane, session);
    }
```

`spawn_rehost_on` = 把现有 `spawn_rehost(&mut self, pane, session)` 里
「取活动标签的 generation」那一步改成**收参数**。现有签名从活动标签取世代
（恢复途中用户可能已经切走），必须改：

```bash
sed -n '5760,5790p' crates/mullion-app/src/app.rs
```

把 `fn spawn_rehost(&mut self, pane: PaneId, session: SessionId)` 改名为
`fn spawn_rehost_on(&mut self, generation: u64, pane: PaneId, session: SessionId)`，
删掉函数体里那句从活动标签取 `generation` 的代码，并给原调用点
（`app.rs:8604` 附近的 `rehost_request` 分支）补上活动标签的世代号：

```rust
                if let Some((pane, session)) = self.ui.rehost_request.take() {
                    // 用户在 pane 标题条上亲手指定的 —— 就是当前活动标签。
                    if let Some(g) = self.active_ws().map(|ws| ws.generation()) {
                        self.spawn_rehost_on(g, pane, session);
                    }
                }
```

在 `on_pane_rehosted` 与 `on_pane_rehost_err` 两个方法的**开头**各加一句：

```rust
        // F162:串行队列的闸。**两条路径都要复位**,漏一条队列就永久停在这里,
        // 后面的叶子一个都不会再拨。
        self.restore_dial_busy = false;
```

并在两个方法的**结尾**各加 `self.drive_restore_dial();`。
`on_pane_rehosted` 里 `rehost_pane` 成功之后还要把那块 pane 的
`host_pending` 置回 `false` —— 它现在真的连上**自己**那台机器了，落盘该改成
现量（5.2②）：

```rust
        // F162:这块 pane 从此有了自己的 `HostConn`,身份改由运行时实测。
        if let Some(p) = ws.pane_mut(pane) {
            p.host_pending = false;
            p.notice = None;
        }
```

- [ ] **Step 5: 实现占位 pane 与失败降级**

```rust
    /// D3:摆一块**拨不了号**的占位 pane(它那条会话已经被用户删了)。
    ///
    /// 承载机制沿用 F128 的 `Disconnected` pane(emulator + 一条死 channel),
    /// 不发明新的渲染路径 —— 树上有叶子而没有 `PaneState` 的话,那一格
    /// 什么都画不出来(F35 的「空窗期」约定只覆盖短暂空白)。
    fn place_orphan_pane(&mut self, generation: u64, pane: PaneId) {
        self.place_dead_pane(generation, pane, "会话已被删除,无法自动恢复");
    }

    /// D6:某台机器连不上时,**只有那块 pane** 降级成断开态,其余照常用。
    ///
    /// 为什么不是全或无:一台机器关机就让另外两台也连不成,不成比例。
    /// 为什么不接 F128 的指数退避自动重试:认证失败类错误会反复重试到退避
    /// 封顶,远端多出一串登录失败记录。用户点标题条上的「换节点」可以再试。
    fn degrade_restored_pane(&mut self, generation: u64, pane: PaneId, msg: &str) {
        self.place_dead_pane(generation, pane, msg);
    }

    /// D3/D6 共用的承载:一块有 `PaneState`、状态是 `Disconnected`、
    /// 标题条上挂着一句说明的 pane。
    fn place_dead_pane(&mut self, generation: u64, pane: PaneId, msg: &str) {
        let scrollback = resolved_scrollback(
            self.store.as_ref(),
            self.tabs.by_generation(generation).and_then(|t| t.session_id),
        );
        let Some(ws) = self
            .tabs
            .by_generation_mut(generation)
            .and_then(|t| t.content.as_terminal_mut())
            .map(|t| &mut t.ws)
        else {
            return;
        };
        if !pane_still_wanted(ws, pane, generation) {
            return;
        }
        if let Some(p) = ws.pane_mut(pane) {
            // 已经有 `PaneState` 了(拨号途中降级):只改状态与说明,
            // 别把 emulator 重建掉 —— 里面可能有用户想看的报错。
            p.status = crate::shell::workspace::PaneStatus::Disconnected;
            p.host_pending = true;
            p.notice = Some(msg.to_string());
            return;
        }
        // `PaneState::rx` 是 `tokio::sync::mpsc::Receiver<Vec<u8>>`。丢掉发送端
        // 之后它恒返回 `None` —— 喂数据那条路会把它当「对端已关」处理,正是
        // 我们要的语义,不必新加分支。
        let (dead_tx, dead_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
        drop(dead_tx);
        ws.attach_pane(crate::shell::workspace::PaneState {
            id: pane,
            host_ix: 0,
            emulator: new_pane_emulator(scrollback),
            pty: Box::new(crate::shell::workspace::DeadPty),
            rx: dead_rx,
            pacer: SyncFramePacer::new(),
            status: crate::shell::workspace::PaneStatus::Disconnected,
            saw_first_byte: false,
            last_grid: (0, 0),
            cwd: None,
            tmux: None,
            history_reported: 0,
            // 它从来没连上过自己那台机器 —— 身份要照抄盘上那份(5.2②)。
            host_pending: true,
            notice: Some(msg.to_string()),
        });
        mark_ui_dirty!(self.ui_dirty);
    }
```

`DeadPty` 是新的哑写口，加在 `crates/mullion-app/src/shell/workspace/mod.rs`
里 `PtyWriter` 的实现旁边：

```rust
/// D3/D6:一条**永远写不出去**的 pty。给「摆出来了但没有连接」的占位 pane 用。
///
/// 不用 `Option<Box<dyn PtyWriter>>`:那会让每个写入点都多一层判空,而漏掉
/// 任何一个的现象是 panic。写失败在本项目本来就是**静默丢弃 + 一行日志**的
/// 既有语义(出站队列满时就是这样),让它走同一条路。
pub struct DeadPty;

impl PtyWriter for DeadPty {
    fn write(&self, _bytes: Vec<u8>) -> Result<(), TrySendErr> {
        Err(TrySendErr::Closed)
    }
    fn resize(&self, _cols: u16, _rows: u16) -> Result<(), TrySendErr> {
        Err(TrySendErr::Closed)
    }
    /// 没有 channel 可关。`close` 没有默认实现是刻意的(F140),这里显式给空体。
    fn close(&self) {}
}
```

（`TrySendErr` 定义在 `crates/mullion-ssh/src/session.rs:436`，两个变体
`Full` / `Closed`。`PtyWriter` 在 `crates/mullion-app/src/shell/workspace/mod.rs:78`。）

`on_pane_rehost_err` 里把「只 `set_error`」改成：

```rust
                // D6:pane 级降级 —— 只有这块变成断开态,同标签其余 pane 照常用。
                self.degrade_restored_pane(generation, pane, "这台机器连不上,恢复失败");
```

（`set_error` 那句**保留**：用户主动点「换节点」失败时那条 toast 仍要有。）

- [ ] **Step 6: 恢复 Task 7 Step 5 里被暂时注掉的两条分支**

把 `dispatch_restored_leaves` 里的 `log::warn!` 占位换回：

```rust
                LeafPlan::Dial(s) => self.restore_dial.push_back((generation, *id, *s)),
                LeafPlan::Orphan => self.place_orphan_pane(generation, *id),
```

并把结尾的 `self.drive_restore_dial();` 加回来。

- [ ] **Step 7: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED|panicked" | tail -5`
Expected: `test result: ok.`

- [ ] **Step 8: 提交**

```bash
git add crates/mullion-app/src
git commit -m "feat(app): 跨机器恢复串行拨号 + pane 级失败降级 (F162/D3/D6/D10)

复用换节点链路,不新写第二条拨号路径(第二条一定会漏掉 pending_rehost
那道防连点的闸)。PaneRehosted/PaneRehostErr 两条路径都要复位串行闸,
漏一条队列就永久停住。守护:restoring_a_two_host_tab_dials_serially、
one_unreachable_host_only_disconnects_its_own_pane。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 9: `on_pane_ready` 按实测名 attach（F161 接线 / D7 / D8）

**Files:**
- Modify: `crates/mullion-app/src/automation.rs`（新增 `pending_for_measured_attach`）
- Modify: `crates/mullion-app/src/app.rs:5192`（`on_pane_ready`）
- Test: 两个文件各自的 `mod tests`

- [ ] **Step 1: 写会失败的测试**

`crates/mullion-app/src/automation.rs` 的 `mod tests` 里：

```rust
    /// D7:有实测 tmux 名的 pane **只发 attach**,配置里的登录后命令整个跳过。
    ///
    /// 反例说明为什么必须这么定:用户某会话配了 `cd /srv && npm run dev`,
    /// 先 attach 进 tmux(dev 正跑着)再发这一串,就是往一个正在跑的进程里
    /// 打字节。`automation.rs` 开篇那条不变量说的就是这件事:attach 一旦生效,
    /// 屏幕归那个 TUI。
    ///
    /// 自证会变红:把 `build_plan_attach_measured` 换成 `build_plan`
    /// (那条会把 cd/export 一起拼进来)。
    #[test]
    fn a_pane_with_a_measured_tmux_name_skips_the_configured_plan() {
        let mut a = sample_automation();
        a.commands = vec![AutomationCommand {
            text: "npm run dev".into(),
            ..Default::default()
        }];
        let got = pending_for_measured_attach(&a, "web01", false).expect("有实测名就该有计划");
        assert_eq!(got.steps.len(), 1, "attach 之后不许再叠别的步骤");
        let line = String::from_utf8(got.steps[0].bytes.clone()).unwrap();
        assert!(line.contains("attach"), "{line}");
        assert!(!line.contains("npm run dev"), "配置的命令被叠上来了:{line}");
    }

    /// D7 的另一半:**没有**实测名的 pane 照旧跑配置计划。
    ///
    /// 单独一条,防止把今天的行为一起删掉 —— 只测上面那条的话,把
    /// `pending_for_extra_pane` 整个删掉测试照样绿。
    #[test]
    fn a_pane_without_a_measured_name_still_runs_the_configured_plan() {
        let mut a = sample_automation();
        a.commands = vec![AutomationCommand {
            text: "npm run dev".into(),
            ..Default::default()
        }];
        assert!(
            pending_for_measured_attach(&a, "", false).is_none(),
            "没有实测名时不该产出 attach 计划"
        );
        let fallback = pending_for_extra_pane(&a).expect("该回落到配置计划");
        let line = String::from_utf8(fallback.steps[0].bytes.clone()).unwrap();
        assert!(line.contains("npm run dev"), "{line}");
    }
```

（`sample_automation()` / `AutomationCommand` 的实际构造方式按该文件既有测试
辅助来——`grep -n "fn sample\|ResolvedAutomation {" crates/mullion-app/src/automation.rs | head`。
**复用既有辅助，别新造。**）

`crates/mullion-app/src/app.rs` 的 `mod tests` 里：

```rust
    /// D8:attach 失败之后**不补跑**配置的登录后命令。
    ///
    /// 失败检测发生在「发完等几秒看标题」之后,那时用户很可能已经在那块 pane
    /// 里敲东西了。延迟补发字节是本项目最危险的一类行为(同 F156-c 只在 pane
    /// 刚建立时注入 OSC 7 的理由)。停在裸 shell,pane 上挂提示,下一步交给用户。
    ///
    /// 自证会变红:在 `finish_attach_check` 的失败分支里加一句
    /// `self.start_automation(`。
    #[test]
    fn a_failed_attach_does_not_replay_the_configured_plan() {
        let body = body_of(prod_src(), "fn finish_attach_check(");
        assert!(
            !body.contains("start_automation("),
            "attach 失败之后补发了字节 —— 那时用户可能正在这块 pane 里打字:\n{body}"
        );
        assert!(
            !body.contains("pending_for_extra_pane("),
            "attach 失败之后补跑了配置计划:\n{body}"
        );
    }
```

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test -p mullion-app --lib a_pane_with_a_measured 2>&1 | tail -10`
Expected: 编译失败，`cannot find function 'pending_for_measured_attach'`。

- [ ] **Step 3: 实现 `pending_for_measured_attach`**

在 `crates/mullion-app/src/automation.rs` 里 `pending_for_reattach` 之后加：

```rust
/// F161/D1+D7:按**实测**会话名把这块 pane 接回 tmux。`None` = 没有可用的名字
/// (不在 tmux 里 / 远端没开 `set-titles` / 自动化总开关关着)。
///
/// 与 `pending_for_reattach` 的差别:那条的名字来自**会话配置**
/// (`tmux_session_name`),这条来自 F123/F124 远端标题上报的**实测值**。
/// 用户的 tmux 是在远端手敲 `tt web01` 进去的时候,配置里根本没有那个名字
/// —— 这正是「一块 pane 都没接回来」的根因(spec §1.1 症状④)。
///
/// 与 `pending_for_extra_pane` 的关系是**互斥**,不是叠加:attach 一旦生效,
/// 屏幕就归那个 TUI 了,之后发任何字节都是打进 TUI(D7)。调用方只能二选一。
///
/// `detach_others` 见 `restore_plan::detach_flags`(D5:键是「机器 + 会话名」)。
pub fn pending_for_measured_attach(
    tpl: &ResolvedAutomation,
    name: &str,
    detach_others: bool,
) -> Option<PendingAutomation> {
    let steps = mullion_store::build_plan_attach_measured(tpl, name, detach_others);
    if steps.is_empty() {
        return None;
    }
    Some(PendingAutomation {
        steps,
        ready_timeout_ms: tpl.ready_timeout_ms,
    })
}
```

- [ ] **Step 4: 接进 `on_pane_ready`**

把 `crates/mullion-app/src/app.rs` 的 `on_pane_ready` 末尾那段改成：

```rust
        // F161/D7:这块 pane 有「当初在哪个 tmux 会话里」的记录 → **只发 attach**,
        // 调用方算好的配置计划整个跳过。两者不能叠加:attach 一旦生效,屏幕就
        // 归那个 TUI 了,之后发任何字节都是打进 TUI。
        //
        // 收口在这里而不是各调用点:三条建立路径(首次连接 / 分屏新开 /
        // 换节点)+ 断线重连都要走同一条规则,分头写迟早走样,而走样的现象是
        // 「某块 pane 接回了别人的会话」。守护:
        // `every_pane_ready_path_goes_through_on_pane_ready`。
        let plan = match self.take_attach_intent(generation, pane) {
            Some(p) => Some(p),
            None => plan,
        };
        if let Some(plan) = plan {
            self.start_automation(generation, pane, plan, sink);
        }
    }

    /// F161:取走这块 pane 的「该接回哪个 tmux 会话」记录并算成计划。
    ///
    /// **取走**语义:恰好生效一次。留着的话,同一块 pane 下次因为别的原因
    /// 再走一遍 `on_pane_ready`(比如断线重连)会拿一个陈旧的会话名去 attach。
    ///
    /// 记录的两个来源共用这一个表(设计 §3 要求「恢复与 F128 断线重连共用」):
    /// - 恢复:从 `layout.toml` 的叶子读回来的上次实测名(Task 7 填的)
    /// - 重连:断线前那块 pane 自己量到的名(Task 10 填的)
    fn take_attach_intent(
        &mut self,
        generation: u64,
        pane: PaneId,
    ) -> Option<crate::automation::PendingAutomation> {
        let t = self
            .tabs
            .by_generation_mut(generation)
            .and_then(|tab| tab.content.as_terminal_mut())?;
        let ix = t.leaf_wanted.iter().position(|(id, _)| *id == pane)?;
        let (_, wanted) = t.leaf_wanted.remove(ix);
        let detach = t
            .leaf_detach
            .iter()
            .position(|(id, _)| *id == pane)
            .map(|i| t.leaf_detach.remove(i).1)
            .unwrap_or(false);
        let name = wanted.tmux?;
        let tpl = t.automation_template.clone()?;
        // F163:发出去之后要校验它到底接上没有(见 `drive_attach_checks`)。
        self.attach_checks.push(AttachCheck {
            generation,
            pane,
            name: name.clone(),
            deadline: std::time::Instant::now()
                + std::time::Duration::from_millis(u64::from(tpl.initial_delay_ms))
                + ATTACH_CHECK_GRACE,
        });
        crate::automation::pending_for_measured_attach(&tpl, &name, detach)
    }
```

> **注意借用**：`self.attach_checks.push` 与 `t`（借自 `self.tabs`）冲突。
> 实现时把 `t` 那段包在一个块里、先把 `(name, detach, tpl)` 拷出来再 `push`。
> `AttachCheck` / `ATTACH_CHECK_GRACE` / `attach_checks` / `drive_attach_checks`
> 在 Task 11 定义——本步先把 `attach_checks` 相关三行注掉，Task 11 补上。

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED|panicked" | tail -5`
Expected: `test result: ok.`（`a_failed_attach_does_not_replay_the_configured_plan`
此时还没有 `finish_attach_check` 可切——把它标 `#[ignore]` 并在 Task 11 去掉。）

**同时确认既有的 `every_pane_ready_path_goes_through_on_pane_ready` 仍然绿**
（`crates/mullion-app/src/app.rs:12942`）：它数生产代码里 `self.start_automation(`
的出现次数**必须恰好是 1**。本 Task 的改动把决策塞进 `on_pane_ready`、不新开
调用点，正是为了让这条继续成立。它一红就说明新逻辑另开了一条发字节的路。

Run: `cargo test -p mullion-app --lib every_pane_ready_path 2>&1 | tail -3`
Expected: `test result: ok. 1 passed`

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-app/src/automation.rs crates/mullion-app/src/app.rs
git commit -m "feat(app): 有实测 tmux 名的 pane 只发 attach、跳过配置计划 (F161/D7)

收口在 on_pane_ready 而不是各调用点:三条建立路径 + 断线重连共用同一条
规则,分头写走样的现象是「某块 pane 接回了别人的会话」。
守护:a_pane_with_a_measured_tmux_name_skips_the_configured_plan、
a_pane_without_a_measured_name_still_runs_the_configured_plan。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 10: F128 断线重连改用实测名（修 spec §1.3 那条既有 bug）

**Files:**
- Modify: `crates/mullion-app/src/app.rs:6713-6870`（`PaneReconnected` 分支）
- Modify: `crates/mullion-app/src/app.rs:16580` 附近（既有守护测试）
- Test: `crates/mullion-app/src/app.rs`

- [ ] **Step 1: 写会失败的测试**

```rust
    /// spec §1.3 那条**既有 bug** 的回归守护。
    ///
    /// `build_plan_reattach` 的会话名判据是 `tmux_session_name(配置)`,配置没配
    /// tmux 时返回空计划 —— 用户的 tmux 是在远端手敲 `tt web01` 进去的,配置里
    /// 根本没有那个名字,所以**今天断线自动重连也接不回 tmux**(不只是新开 exe)。
    ///
    /// 真值源换成实测(`PaneState::tmux`,`reattach_pane` 刻意保留了它),
    /// 配置只在实测为空时回落(D1)。
    ///
    /// 自证会变红:把 `PaneReconnected` 分支里那句读 `p.tmux` 的代码删掉、
    /// 改回只认 `tmux_attach`。
    #[test]
    fn the_reattach_path_reads_the_measured_name_not_the_configured_one() {
        let body = body_of(prod_src(), "fn on_pane_reconnected(");
        let measured = body
            .find("p.tmux")
            .expect("重连分支没有读实测的 tmux 名 —— 手敲进 tmux 的用法接不回来");
        let configured = body.find("tmux_attach").unwrap_or(usize::MAX);
        assert!(
            measured < configured,
            "实测名必须**优先于**配置名(D1),现在顺序反了:\n{body}"
        );
    }
```

（前提：把 `crates/mullion-app/src/app.rs:6713` 那条 `UserEvent::PaneReconnected`
分支体抽成具名方法。它是**多行模式**，`arm_of`（锚点 `模式 => {`）切不动，
只能用 `body_of`（锚点 `fn 名(`）。抽方法是纯搬运，不改语义；搬完先跑一次
全量测试确认没红，再改语义。）

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test -p mullion-app --lib the_reattach_path_reads_the_measured 2>&1 | tail -10`
Expected: FAILED，`重连分支没有读实测的 tmux 名`。

- [ ] **Step 3: 抽方法（纯搬运，先跑绿）**

把 `UserEvent::PaneReconnected { .. } => { <整段> }` 改成
`UserEvent::PaneReconnected { generation, host_ix, handle, channels } =>
 self.on_pane_reconnected(generation, host_ix, handle, channels),`
并把整段搬进新方法。

Run: `cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED" | tail -3`
Expected: 除新测试外全绿。

- [ ] **Step 4: 改真值源**

在 `on_pane_reconnected` 里，`reattach_pane` 成功、`attached` 收集完之后、
借用 `t` 结束之前，收集每块 pane 的实测名并写进 `leaf_wanted`/`leaf_detach`：

```rust
                    // F161/D1:重连时该接回哪个会话,**真值源是实测**。
                    // `reattach_pane` 刻意保留了 `emulator` 连同它嗅出来的
                    // `cwd`/`tmux`,所以此刻 `p.tmux` 还是断线前那个名字。
                    //
                    // 配置(`tmux_attach`)只在实测为空时回落 —— 用户的 tmux
                    // 是在远端手敲 `tt web01` 进去的,配置里根本没有那个名字,
                    // 只认配置的话断线之后那个会话永远接不回来(spec §1.3)。
                    let measured: Vec<crate::shell::layout_snapshot::LeafIdentity> = attached
                        .iter()
                        .map(|(id, _)| crate::shell::layout_snapshot::LeafIdentity {
                            session_id: t.ws.hosts.get(host_ix).and_then(|h| h.session_id),
                            tmux: t
                                .ws
                                .pane(*id)
                                .and_then(|p| p.tmux.clone())
                                .or_else(|| {
                                    tmux_attach
                                        .as_ref()
                                        .filter(|x| x.matches(*id, host_ix))
                                        .map(|x| x.session_name.clone())
                                }),
                        })
                        .collect();
                    // D5:同一台机器上的同一个会话名,只有第一块带 `-d`。
                    // 重连场景里「其他 client」几乎必然是我们自己的残骸
                    // (SSH 断了但远端 tmux 要等 TCP 超时才知道),第一块必须踢;
                    // 第二块再踢就会把第一块踢成 detached(F141 的原始理由)。
                    let flags = crate::shell::restore_plan::detach_flags(&measured);
                    for ((id, _), (want, d)) in
                        attached.iter().zip(measured.into_iter().zip(flags))
                    {
                        t.leaf_wanted.retain(|(x, _)| x != id);
                        t.leaf_detach.retain(|(x, _)| x != id);
                        t.leaf_wanted.push((*id, want));
                        t.leaf_detach.push((*id, d));
                    }
```

然后把下面那段 `let plan = match tmux_attach.as_ref() { ... }` 简化成：

```rust
                // F161:计划由 `on_pane_ready` 按上面写进 `leaf_wanted` 的实测名
                // 决定(D7:有名字就只发 attach)。这里只给「没有任何 tmux 名」
                // 那些 pane 备一份配置计划 —— 分屏出来的、换过节点的照旧
                // 跳过 tmux、只重跑 cd/export/命令。
                if let Some(tpl) = template {
                    for (id, sink) in attached {
                        let plan = crate::automation::pending_for_extra_pane(&tpl);
                        // F156-c:重连出来的 channel 也是一条刚起的干净 shell,
                        // 同样要经 `on_pane_ready` 注入 OSC 7;`false` = 不清屏
                        // (断线前那一屏是用户想看的东西)。
                        self.on_pane_ready(generation, id, sink, plan, false);
                    }
                }
```

- [ ] **Step 5: 更新既有的源码切片守护测试**

`crates/mullion-app/src/app.rs:16587` 附近那条断言分支里含
`pending_for_reattach(` 的测试**会变红**——这是对的，它守的行为已经被更好的
机制取代了。把它改写成：

```rust
    /// F141 的语义没变,只是真值源换了(F161/D1):断线重连回来的 pane 要真的
    /// **接回原会话**,而不是「在一块新 shell 里把 cd/export 重跑一遍」。
    ///
    /// 判据从「分支里调了 `pending_for_reattach`」换成「分支把实测名写进了
    /// `leaf_wanted`」—— 后者是 `on_pane_ready` 决定发不发 attach 的唯一依据。
    ///
    /// 自证会变红:把 `on_pane_reconnected` 里那段写 `leaf_wanted` 的代码删掉。
    #[test]
    fn reconnecting_still_reattaches_the_original_tmux_session() {
        let body = body_of(prod_src(), "fn on_pane_reconnected(");
        assert!(
            body.contains("leaf_wanted.push("),
            "重连分支没有登记「该接回哪个会话」,断线前的 tmux 会话回不来:\n{body}"
        );
    }
```

- [ ] **Step 6: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED|panicked" | tail -5`
Expected: `test result: ok.`

- [ ] **Step 7: 提交**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "fix(app): 断线重连按实测 tmux 名接回,配置只做回落 (F161/F128)

spec §1.3 的既有 bug:build_plan_reattach 的判据是配置里的 tmux 名,而用户
的 tmux 是在远端手敲 tt <名> 进去的 —— 配置里根本没有,所以今天断线重连也
接不回 tmux,不只是新开 exe。reattach_pane 刻意保留了 emulator 连同嗅出来
的 p.tmux,真值源现成就有。
守护:the_reattach_path_reads_the_measured_name_not_the_configured_one、
reconnecting_still_reattaches_the_original_tmux_session。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 11: attach 结果校验 + pane 上挂提示（F163 / D4 / D8）

**Files:**
- Modify: `crates/mullion-app/src/app.rs`（`AttachCheck`、`drive_attach_checks`、`finish_attach_check`、每帧驱动接线）
- Modify: `crates/mullion-app/src/ui/pane_title.rs:60-82`（`title_text` 带提示）
- Test: 两个文件各自的 `mod tests`

- [ ] **Step 1: 写会失败的测试**

`crates/mullion-app/src/ui/pane_title.rs` 的 `mod tests` 里：

```rust
    /// F163/D4:attach 失败的说明挂在**这块 pane 的标题条**上,不弹窗 ——
    /// 多块 pane 同时失败时会连弹好几次。
    ///
    /// 自证会变红:把 `title_text` 的 `notice` 参数忽略掉。
    #[test]
    fn a_pane_notice_shows_up_on_its_own_title_bar() {
        let got = title_text(
            2,
            Some("prod"),
            Some("srv"),
            None,
            PaneStatus::Live,
            Some("当初的会话 web01 已不存在"),
        );
        assert!(got.contains("web01 已不存在"), "{got}");
        assert!(got.contains("prod"), "说明不该把原来那串顶掉:{got}");
    }

    /// 断开态 pane 的说明同样要出得来(D3 占位 / D6 降级都是断开态)——
    /// 断开分支原本是一条 early return,漏了它就整条看不见。
    #[test]
    fn a_disconnected_pane_still_shows_its_notice() {
        let got = title_text(
            1,
            Some("prod"),
            None,
            None,
            PaneStatus::Disconnected,
            Some("会话已被删除,无法自动恢复"),
        );
        assert!(got.contains("会话已被删除"), "{got}");
    }
```

`crates/mullion-app/src/app.rs` 的 `mod tests` 里：

```rust
    /// F163:发完 attach 之后要真的比对 —— `automation::Outcome::Completed`
    /// 的语义只是「字节发出去了」,远端 `tmux attach -t X` 返回什么客户端根本
    /// 不看,默认情况下 attach 失败**完全静默**。
    ///
    /// 接上了(实测名变成期望的那个)→ 收摊,不留提示。
    ///
    /// 自证会变红:把 `attach_check_verdict` 的成功分支删掉、恒返回
    /// `Verdict::Waiting`。
    #[test]
    fn a_successful_attach_clears_the_check() {
        assert_eq!(
            attach_check_verdict(Some("web01"), "web01", false),
            AttachVerdict::Ok
        );
    }

    /// 超时之前不下结论 —— 太早判会在慢链路上误报「没接上」。
    #[test]
    fn the_check_waits_until_the_deadline_before_complaining() {
        assert_eq!(
            attach_check_verdict(None, "web01", false),
            AttachVerdict::Waiting
        );
        assert_eq!(
            attach_check_verdict(None, "web01", true),
            AttachVerdict::Failed
        );
        assert_eq!(
            attach_check_verdict(Some("other"), "web01", true),
            AttachVerdict::Failed
        );
    }

    /// D4 的边界:这条判据**依赖 F124 在跑**。用户把 `tmux_bootstrap` 关掉时,
    /// attach 成功也不会有标题上报,校验会恒误报「没接上」。
    /// 开关关着就跳过校验(attach 照发,只是不许下失败结论)。
    ///
    /// 自证会变红:把 `should_check_attach` 里的 bootstrap 判断去掉。
    #[test]
    fn the_attach_check_is_skipped_when_title_reporting_is_off() {
        assert!(should_check_attach(true));
        assert!(
            !should_check_attach(false),
            "没开远端标题上报时校验会恒误报「没接上」"
        );
    }
```

- [ ] **Step 2: 跑测试确认它失败**

Run: `cargo test -p mullion-app --lib attach_check 2>&1 | tail -10`
Expected: 编译失败，`cannot find function 'attach_check_verdict'`。

- [ ] **Step 3: 改 `title_text`**

`crates/mullion-app/src/ui/pane_title.rs`：

```rust
pub fn title_text(
    index: usize,
    host: Option<&str>,
    dir: Option<&str>,
    tmux: Option<&str>,
    status: PaneStatus,
    notice: Option<&str>,
) -> String {
    let Some(h) = host else {
        return format!("{index} · 连接中…");
    };
    // F163/D4:说明拼在最后。**断开态也要拼**(D3 的占位 pane 与 D6 的降级
    // pane 都是断开态)—— 断开那条原本是 early return,不在这里补的话整条
    // 说明看不见,而它是用户唯一能知道「为什么这一格是空的」的地方。
    let tail = |mut s: String| {
        if let Some(n) = notice {
            s.push_str(" · ");
            s.push_str(n);
        }
        s
    };
    if status == PaneStatus::Disconnected {
        return tail(format!("{index} · {h} (已断开)"));
    }
    let mut parts = vec![index.to_string(), h.to_string()];
    parts.extend(dir.map(str::to_string));
    parts.extend(tmux.map(str::to_string));
    tail(parts.join(" · "))
}
```

给 `TitleView` 加字段并在 `show` 里传下去：

```rust
    /// F163/D4:挂在这块 pane 上的一句说明(attach 失败 / 会话已删 / 连不上)。
    /// 来自 `PaneState::notice`。**不弹窗** —— 多块 pane 同时失败会连弹好几次。
    pub notice: Option<&'a str>,
```

`grep -n "TitleView {" crates/mullion-app/src` 找到所有构造点补上；
`app.rs` 那个真实构造点填 `notice: p.notice.as_deref()`。

**T9 字形白名单**：本 Task 新增的 UI 字符串（「当初的会话 X 已不存在」
「会话已被删除,无法自动恢复」「这台机器连不上,恢复失败」）全是 GBK 内的汉字与
半角标点，无需登记。跑一次 `cargo test -p mullion-app --test glyph_whitelist` 确认。

- [ ] **Step 4: 实现校验**

`crates/mullion-app/src/app.rs`：

```rust
/// F163:发完 attach 之后再宽限这么久才下「没接上」的结论。
///
/// 时长是**猜的**,要人工调(见 spec §7):太短会在慢链路上误报,太长则提示
/// 来得毫无意义(用户早就自己看出来了)。
const ATTACH_CHECK_GRACE: std::time::Duration = std::time::Duration::from_secs(4);

/// F163:一条在途的 attach 校验。
struct AttachCheck {
    generation: u64,
    pane: PaneId,
    /// 期望远端报上来的会话名。
    name: String,
    deadline: std::time::Instant,
}

/// F163 的判决。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttachVerdict {
    /// 接上了(远端标题报的会话名 = 期望的那个)。
    Ok,
    /// 还没到期,别下结论。
    Waiting,
    /// 到期了还没报上来 / 报的是别的会话。
    Failed,
}

/// F163:这一刻该给这条校验什么判决。纯函数 —— 「几秒算没接上」这条判据
/// 本身要能脱离事件循环单测。
///
/// `measured` = `PaneState::tmux`(F123/F124 远端标题上报的实测值)。
/// attach 成功之后 tmux 必然按 F124 配的 `set-titles-string` 发标题,
/// 所以「接回来了没有」本来就是可观测的 —— 这是实测那条腿的第二个用途。
fn attach_check_verdict(measured: Option<&str>, want: &str, expired: bool) -> AttachVerdict {
    if measured == Some(want) {
        return AttachVerdict::Ok;
    }
    if expired {
        AttachVerdict::Failed
    } else {
        AttachVerdict::Waiting
    }
}

/// F163/D4 的边界:这条校验**依赖 F124 在跑**。
///
/// 用户把 `tmux_bootstrap` 开关关掉时,远端不设标题,attach 成功也不会有会话名
/// 报上来 —— 校验会恒误报「没接上」,而那条误报比不校验糟得多(用户会去查一个
/// 根本不存在的问题)。开关关着就跳过:attach 照发,只是不许下失败结论。
fn should_check_attach(title_reporting_on: bool) -> bool {
    title_reporting_on
}
```

驱动与收尾：

```rust
    /// F163:每帧推进在途的 attach 校验。挂在 `drive_*` 那一组里。
    ///
    /// **遍历的是 `attach_checks` 不是活动标签**:每条自带世代号,校验途中
    /// 用户完全可能切到别的标签去(记忆里那条「`drive_*` 每帧驱动函数必须
    /// 遍历全部标签」的同源教训)。
    fn drive_attach_checks(&mut self) {
        if self.attach_checks.is_empty() {
            return;
        }
        let now = std::time::Instant::now();
        let mut done: Vec<(u64, PaneId, String, AttachVerdict)> = Vec::new();
        // `retain` 的闭包要 `&mut self.attach_checks`,而里面又要读 `self.tabs`
        // —— 借用检查器分不开。先整份取出来再放回未决的那些。
        let mut pending = std::mem::take(&mut self.attach_checks);
        pending.retain(|c| {
            let measured = self
                .tabs
                .by_generation(c.generation)
                .and_then(|t| t.content.as_terminal())
                .and_then(|t| t.ws.pane(c.pane))
                .and_then(|p| p.tmux.clone());
            let v = attach_check_verdict(measured.as_deref(), &c.name, now >= c.deadline);
            if v == AttachVerdict::Waiting {
                return true;
            }
            done.push((c.generation, c.pane, c.name.clone(), v));
            false
        });
        self.attach_checks = pending;
        for (generation, pane, name, verdict) in done {
            self.finish_attach_check(generation, pane, &name, verdict);
        }
    }

    /// F163:一条校验有结论了。
    ///
    /// **D8:失败之后不补跑配置的登录后命令。** 结论是在「发完等几秒」之后
    /// 才有的,那时用户很可能已经在这块 pane 里敲东西了 —— 延迟补发字节是
    /// 本项目最危险的一类行为(同 F156-c 只在 pane 刚建立时注入 OSC 7 的理由)。
    /// 停在裸 shell,pane 上挂提示,下一步交给用户。
    fn finish_attach_check(
        &mut self,
        generation: u64,
        pane: PaneId,
        name: &str,
        verdict: AttachVerdict,
    ) {
        if verdict == AttachVerdict::Ok {
            return;
        }
        log::warn!(
            target: "mullion",
            "pane {} 没能接回 tmux 会话 {name} —— 它多半已经不在远端了",
            pane.0
        );
        if let Some(p) = self
            .tabs
            .by_generation_mut(generation)
            .and_then(|t| t.content.as_terminal_mut())
            .and_then(|t| t.ws.pane_mut(pane))
        {
            // D4:挂在这块 pane 上,**不弹窗** —— 多块 pane 都失败时会连弹好几次。
            p.notice = Some(format!("当初的会话 {name} 已不存在"));
        }
        mark_ui_dirty!(self.ui_dirty);
    }
```

`App` 加字段 `attach_checks: Vec<AttachCheck>,`，`App::new` 里 `Vec::new()`。
在 `drive_automation` 等每帧驱动的调用处旁边加 `self.drive_attach_checks();`
（`grep -n "self.drive_automation();" crates/mullion-app/src/app.rs`）。

把 Task 9 Step 4 里注掉的 `attach_checks.push` 三行放开，并用
`should_check_attach` 把关：

```rust
        // D4 的边界:远端标题上报关着时不许下失败结论(会恒误报)。
        // 判据是**全局设置**那个开关(`Settings::tmux_bootstrap`,F124),
        // 不是 `HostConn::tmux_bootstrap`(那是个 `BootstrapFlags`,记的是
        // 「这条连接上发过了没有」的进度,不是「用户想不想要」)。
        // 同 `tick_tmux_bootstrap` 里那句 `let enabled = self.settings.tmux_bootstrap;`。
        if should_check_attach(self.settings.tmux_bootstrap) {
            self.attach_checks.push(AttachCheck { .. });
        }
```

- [ ] **Step 5: 去掉 Task 9 里那条 `#[ignore]`**

`a_failed_attach_does_not_replay_the_configured_plan` 现在能切到
`fn finish_attach_check` 了，去掉 `#[ignore]`。

- [ ] **Step 6: 跑测试确认通过**

Run: `cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|panicked" | tail -8`
Expected: 全 `ok.`

- [ ] **Step 7: 提交**

```bash
git add crates/mullion-app/src
git commit -m "feat(app): attach 结果校验,不符就在那块 pane 上挂提示 (F163/D4/D8)

Outcome::Completed 的语义只是「字节发出去了」,远端 attach 返回什么客户端
根本不看 —— 默认情况下 attach 失败完全静默。校验依赖 F124 在跑,标题上报
关着时跳过(否则恒误报)。失败后不补跑配置命令:那时用户可能正在这块 pane
里打字(D8)。守护:a_successful_attach_clears_the_check、
the_attach_check_is_skipped_when_title_reporting_is_off、
a_failed_attach_does_not_replay_the_configured_plan。

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 12: 变异验收 + 全量绿 + 文档

**Files:**
- Modify: `docs/gui-render-gotchas.md`（若本轮踩到新的渲染/输入坑才加，没踩到就不动）
- Modify: `CLAUDE.md` 的领域陷阱表（**只在**本轮出现了新的「静默失效」类陷阱时加一行）

- [ ] **Step 1: 逐条跑变异验收**

**先 `git commit`**（记忆里踩过两次：变异验证时 `git checkout` 把未提交的编辑吞掉）。
然后对下表每一条：改代码 → 跑测试 → 确认**指定那条**测试变红 → `git checkout -- <文件>`。

| 测试 | 变异 |
|---|---|
| `a_leaf_that_carries_an_identity_is_still_a_leaf` | `is_leaf` 加上 `&& self.session_id.is_none()` |
| `an_old_record_without_leaf_fields_falls_back_to_the_tab_session` | `leaf_identities` 去掉 `.or(Some(tab_session))` |
| `the_attach_command_never_creates_a_session` | 命令串拼回 `\|\| exec tmux new-session -s {q}` |
| `a_failed_attach_leaves_the_shell_alive` | 命令串改成裸 `exec tmux attach{d} -t {q}` |
| `the_session_name_is_shell_quoted` | `shell_quote(name)` → `name.to_string()` |
| `the_detach_flag_is_keyed_per_host_and_session_name` | `detach_flags` 的键退化成只按 `name` |
| `only_the_first_pane_on_the_same_host_session_gets_the_detach_flag` | 全加 `-d`；再单独试全不加（**两个方向各跑一次**） |
| `a_pane_with_a_measured_tmux_name_skips_the_configured_plan` | `build_plan_attach_measured` → `build_plan` |
| `a_pane_without_a_measured_name_still_runs_the_configured_plan` | `pending_for_measured_attach` 去掉空名早退（恒产出计划） |
| `a_failed_attach_does_not_replay_the_configured_plan` | `finish_attach_check` 失败分支加一句 `self.start_automation(` |
| `a_leaf_whose_session_is_gone_is_kept_as_a_placeholder_not_dropped` | `plan_leaves` 用 `filter_map` 把 `Orphan` 滤掉 |
| `restoring_a_two_host_tab_dials_serially` | `take_next_restore_dial` 去掉 `in_flight` 早退 |
| `one_unreachable_host_only_disconnects_its_own_pane` | `PaneRehostErr` 里删掉 `degrade_restored_pane` 调用 |
| `the_connected_pane_lands_on_the_main_leaf_not_the_first_leaf` | `apply_saved_tree` 忽略 `main_leaf`（恒 0） |
| `a_snapshot_taken_mid_restore_keeps_the_pending_leaf_identities` | `leaf_identity_of` 删掉 `host_pending` 分支 |
| `a_connected_leaf_is_measured_not_copied_from_disk` | `leaf_identity_of` 改成恒查 `wanted` |
| `the_leaf_identity_actually_reaches_the_snapshot` | `snapshot_tabs_of` 的闭包换成 `&\|_\| LeafIdentity::default()` |
| `the_reattach_path_reads_the_measured_name_not_the_configured_one` | `on_pane_reconnected` 删掉读 `p.tmux` 那段 |
| `the_attach_check_is_skipped_when_title_reporting_is_off` | `should_check_attach` 恒 `true` |
| `a_pane_notice_shows_up_on_its_own_title_bar` | `title_text` 忽略 `notice` |

**任何一条变异之后测试仍然全绿 = 那条守护是恒绿的**，当场停下来修测试
（判据没扎到真实注入点），别继续往下走。

- [ ] **Step 2: 全量绿**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log | tail -20
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20
cargo fmt --check
```
Expected: 全 `ok.`；clippy 无输出；fmt 无输出。

- [ ] **Step 3: 提交**

```bash
git add -A
git commit -m "test: F160~F163 变异验收(20 条守护逐条自证变红)

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

- [ ] **Step 4: 发版（交付约定，默认执行，不用再问）**

本轮改动全部落在 `mullion-app` 且要拿去实机验 —— 按
`.claude/skills/release-windows/SKILL.md` 一条龙做完：升 patch 版本号 → 跑绿 →
交叉编译 + objdump 验收 → 签名 → 发 GitHub Release → 报链接。
**别凭记忆做**，每一步都有漏了也不报错的坑；`gh`/curl 全部走 socks 代理
（本机 DNS 解析不了 github）。

Release notes 里带上下面这份人工验收清单。

---

## 人工验收清单（我验证不了的，必须真机跑）

来自 spec §7，逐条：

1. **手敲 tmux 的现场能不能整个回来**（本轮的存在理由）：在两块分屏里分别
   `tt a` / `tt b`，关掉 exe，重开点恢复 → 两块应该各自回到 a 和 b。
2. **换过节点的 pane 连的是不是对的机器**：分屏 → 第二块换到另一台 → 关 exe →
   恢复。第二块的标题条上应该是**第二台**的节点名。
3. **F128 断线重连能不能接回 tmux**（spec §1.3 那条既有 bug）：手敲进 tmux 之后
   拔网线 / 断代理，等重连 → 那个会话应该回来。
4. **attach 字节的发送时机**：pane 建立 → 首字节 → 发 attach，中间那段裸 shell
   会不会被用户抢先输入（`Outcome::Aborted` 会中止，行为正确，但手感要实测）。
5. **F163 的等待时长**（代码里 `ATTACH_CHECK_GRACE = 4s`，是猜的）：几秒算
   「没接上」。太短在慢代理链路上会误报，太长提示来得毫无意义。
6. **跨机器串行恢复的实际体验**：连续几次密码框 / 主机指纹确认，是否难以忍受。
7. **D7 对已配 tmux 的会话是否也合适**：给某个会话配上 tmux 会话名，再走一遍
   恢复——规则要对「配了」和「没配」两类都成立。
8. **同名镜像时的 `window-size` 行为**（D5 已接受的代价）：两块尺寸不同的 pane
   attach 同一个会话，实际是留白还是反复 reflow。
9. **会话被删掉的叶子**：把某个叶子的会话在会话管理器里删掉再恢复 → 那一格应该
   摆出来并写着「会话已被删除，无法自动恢复」，分屏比例不变形。
10. 一如既往：是否不闪 / 字形对齐 / CJK 宽字 / 输入法。

---

## 明确不做（spec §8）

- 生命周期事件流 / 用量时长统计（本轮调查的原始问法，用户已明确重点不在那儿）。
- tmux 内部状态（window 序号、tmux 自己的分屏、当前 pane）——attach 之后由 tmux
  自己还原。
- scrollback 恢复、SFTP 标签的远端目录（F120 明确「不记忆上次打开的目录」）。
- 启动即自动摆回（F37 那部分已被 F148 取代，摆什么由用户在恢复列表里选）。
