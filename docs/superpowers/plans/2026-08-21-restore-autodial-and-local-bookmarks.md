# 恢复现场自动重连 + 本地目录收藏 + 换图标 实现计划(F153/F154)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让「恢复上次的现场」单击一条即恢复并一条接一条自动重连(tmux 由既有登录后自动化接回),让文件面板本地栏的 ☆ 真能收藏本地目录,并换掉程序图标。

**Architecture:** 四件事互不依赖。① 自动拨号复用既有的 `reconnect_tab`(不分叉出第二条拨号路径),用一个 `AutoDial{tried,ok,err}` 在 `ConnectOk`/`ConnectErr` 抵达时推进;前置必须修「`ConnectErr` 不清 `pending_restore`」这个会让占位标签永久连不上的既存 bug。② 本地书签是 `SftpPrefs` 上一个新的 `#[serde(default)]` 字段(不动 `CURRENT_SCHEMA`),沿用远端书签那条 store→app→panel 的三段接线。③ 图标只是替换 `assets/mullion.ico`。

**Tech Stack:** Rust 2021 / egui 0.30 / winit 0.30 / serde+toml(mullion-store)/ mingw-w64 交叉编译。

设计文档:`docs/superpowers/specs/2026-08-21-restore-autodial-and-local-bookmarks-design.md`

---

## 文件地图

| 文件 | 责任 | 本次改动 |
|---|---|---|
| `crates/mullion-store/src/sftp.rs` | 会话的 SFTP 偏好(纯数据) | 加 `local_bookmarks` 字段 |
| `crates/mullion-store/src/vault.rs` | 会话库读写 | 加 `add_local_bookmark`/`remove_local_bookmark` + 测试 |
| `crates/mullion-store/src/migrate.rs` | 迁移与 round-trip 测试 | 只修一处结构体字面量 |
| `crates/mullion-app/src/ui/session_manager/buffer.rs` | 会话编辑器表单 ↔ 记录 | 加 `preserved_local_bookmarks`(防保存时清空) |
| `crates/mullion-app/src/ui/files_panel.rs` | 两栏文件面板 | `PanelFrame` 加 `local_bookmarks`;本地栏传真书签视图;删 `BookmarkView::none()` |
| `crates/mullion-app/src/app.rs` | 事件循环 + 接线 | 本地书签落盘;`ConnectErr` 收口;`AutoDial` |
| `crates/mullion-app/src/ui/history.rs` | 恢复现场弹窗 | 单击即恢复 + 文案 |
| `crates/mullion-app/assets/mullion.ico` | 程序图标资源 | 换文件 |

---

## Task 1: store 层的本地书签

**Files:**
- Modify: `crates/mullion-store/src/sftp.rs:30-37`
- Modify: `crates/mullion-store/src/vault.rs:443-469`
- Modify: `crates/mullion-store/src/migrate.rs:441`(结构体字面量补字段)
- Modify: `crates/mullion-store/src/vault.rs:2031`(同上)
- Test: `crates/mullion-store/src/vault.rs`(模块内 `mod tests`)

- [ ] **Step 1: 写失败的测试**

加在 `crates/mullion-store/src/vault.rs` 的 `mod tests` 里,紧跟既有的
`bookmarks_added_from_the_path_bar_survive_save_and_reopen` 之后:

```rust
    /// F154:本地栏的书签是**另一份列表**,和远端那份互不干扰,而且同样要
    /// 存得进盘 —— 只改内存的症状是「收藏了,重开客户端没了」,全程不报错。
    ///
    /// 自证会变红:把 `add_local_bookmark` 里的 `rec.sftp.local_bookmarks`
    /// 写成 `rec.sftp.bookmarks`(复制粘贴改漏一个字段名的那类真实失误)。
    #[test]
    fn local_bookmarks_are_a_separate_list_that_survives_save_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let id;
        {
            let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
            id = v.add(draft(), "2026-08-21T00:00:00Z");
            v.add_bookmark(
                id,
                crate::sftp::Bookmark {
                    name: "远端日志".into(),
                    path: "/var/log".into(),
                },
            )
            .unwrap();
            v.add_local_bookmark(
                id,
                crate::sftp::Bookmark {
                    name: "工程".into(),
                    path: r"D:\work".into(),
                },
            )
            .unwrap();
            // 同一路径再来一次:去重规则与远端同一条(按 path)。
            v.add_local_bookmark(
                id,
                crate::sftp::Bookmark {
                    name: "另一个名字".into(),
                    path: r"D:\work".into(),
                },
            )
            .unwrap();
            v.save().unwrap();
        }
        {
            let mut v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
            let rec = v.get(id).unwrap();
            assert_eq!(rec.sftp.local_bookmarks.len(), 1, "同一路径收藏两次该去重");
            assert_eq!(rec.sftp.local_bookmarks[0].path, r"D:\work");
            assert_eq!(
                rec.sftp.local_bookmarks[0].name, "工程",
                "去重要留先来的那条,不是拿后来的覆盖"
            );
            assert_eq!(
                rec.sftp.bookmarks.len(),
                1,
                "本地收藏动作污染了远端列表"
            );
            assert_eq!(rec.sftp.bookmarks[0].path, "/var/log");
            v.remove_local_bookmark(id, r"D:\work").unwrap();
            v.save().unwrap();
        }
        let v = Vault::open(dir.path().to_path_buf(), &key()).unwrap();
        let rec = v.get(id).unwrap();
        assert!(
            rec.sftp.local_bookmarks.is_empty(),
            "取消收藏没存盘 —— 删掉的本地书签重启后会回来"
        );
        assert_eq!(
            rec.sftp.bookmarks.len(),
            1,
            "删本地书签把远端那条也删了"
        );
    }

    /// F154:老的 TOML(没有 `local_bookmarks` 这个键)读得进来,且是空列表。
    /// 不成立的话,这次升级会让所有既有会话直接读不出来 —— 用户的整个库消失。
    #[test]
    fn a_record_written_before_local_bookmarks_existed_still_loads() {
        let toml = r#"
default_remote = "/srv/app"

[[bookmarks]]
name = "日志"
path = "/var/log"
"#;
        let prefs: crate::sftp::SftpPrefs = toml::from_str(toml).expect("老记录该读得进来");
        assert_eq!(prefs.bookmarks.len(), 1);
        assert!(prefs.local_bookmarks.is_empty(), "缺键时该是空列表");
    }
```

- [ ] **Step 2: 跑测试确认变红**

```bash
cargo test -p mullion-store local_bookmark 2>&1 | tail -20
```

预期:编译失败,`no method named add_local_bookmark`、`no field local_bookmarks`。

- [ ] **Step 3: 加字段**

`crates/mullion-store/src/sftp.rs`,`SftpPrefs` 里 `bookmarks` 之后:

```rust
    /// F154:**本地**栏路径条上的 ☆ 收进来的目录(Windows 形态的绝对路径)。
    ///
    /// 与 `bookmarks` **分成两份**,不是一份混着存:两栏的路径空间毫无关系
    /// (`D:\work` 和 `/var/log`),混在一起的话路径条那句「当前 cwd 在不在
    /// 列表里」的现算判据会在两栏之间串味 —— 远端进到一个恰好同名的目录
    /// 就会显示成已收藏。
    ///
    /// 挂在会话记录下(而不是全局):与 `bookmarks` 同一个存放位置、同一套
    /// 「没有 `SessionId` 就置灰」的规则,代价是同一台机器的两条会话各存
    /// 各的(设计 ③ 已认下)。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub local_bookmarks: Vec<Bookmark>,
```

- [ ] **Step 4: 加 Vault 方法**

`crates/mullion-store/src/vault.rs`,紧跟既有 `remove_bookmark` 之后:

```rust
    /// F154:给一条会话加一个**本地**书签(文件面板本地栏路径条上的 ☆)。
    ///
    /// 与 `add_bookmark` 的差别只有落在哪个列表上,去重规则(按 `path`)
    /// 共用 `push_deduped` —— 两边分叉的话,一边改了去重判据另一边不改,
    /// 症状是某一栏点「取消收藏」看起来没反应。
    pub fn add_local_bookmark(
        &mut self,
        id: SessionId,
        mark: crate::sftp::Bookmark,
    ) -> Result<(), StoreError> {
        let rec = self
            .sessions
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or(StoreError::NotFound(id))?;
        push_deduped(&mut rec.sftp.local_bookmarks, mark);
        Ok(())
    }

    /// F154:取消收藏本地目录。按路径相等匹配,同 `remove_bookmark`。
    pub fn remove_local_bookmark(&mut self, id: SessionId, path: &str) -> Result<(), StoreError> {
        let rec = self
            .sessions
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or(StoreError::NotFound(id))?;
        rec.sftp.local_bookmarks.retain(|b| b.path != path);
        Ok(())
    }
```

在 `impl Vault` 之外(文件末尾 `mod tests` 之前)加共用 helper:

```rust
/// F139/F154:书签入列表的去重。**按 `path`** —— 书签的身份就是路径,
/// 名字可以重复也可以为空。留先来的那条,不拿后来的覆盖。
fn push_deduped(list: &mut Vec<crate::sftp::Bookmark>, mark: crate::sftp::Bookmark) {
    if !list.iter().any(|b| b.path == mark.path) {
        list.push(mark);
    }
}
```

把既有 `add_bookmark` 里那三行 `if !rec.sftp.bookmarks.iter().any(...)` 换成
`push_deduped(&mut rec.sftp.bookmarks, mark);`(两边共用同一条判据)。

- [ ] **Step 5: 补齐两处结构体字面量**

`crates/mullion-store/src/migrate.rs:441` 和 `crates/mullion-store/src/vault.rs:2031`
的 `SftpPrefs { .. }` 各加一行 `local_bookmarks: Vec::new(),`。

- [ ] **Step 6: 跑测试确认变绿**

```bash
cargo test -p mullion-store 2>&1 | tail -5
```

预期:`test result: ok`,失败数 0。

- [ ] **Step 7: 提交**

```bash
git add crates/mullion-store/src/sftp.rs crates/mullion-store/src/vault.rs crates/mullion-store/src/migrate.rs
git commit -m "feat(store): 会话记录多存一份本地目录书签 (F154)

新字段带 serde default,不动 CURRENT_SCHEMA —— 没有任何值需要转换。
去重判据抽成 push_deduped,远端/本地共用一条,不许分叉。"
```

---

## Task 2: 会话编辑器保存时不要清空本地书签

**为什么单独一个任务**:`buffer.rs:711` 那条 `to_draft` 是**整份重建** `SftpPrefs`。
本地书签不是表单里的字段,不显式保住的话,用户在会话编辑器里点一次「保存」
就把它们全清了 —— 而且静默,没有任何提示。既有的 `preserved_automation`
(`buffer.rs:130/331/711`)就是为同一类问题设的,照着做。

**Files:**
- Modify: `crates/mullion-app/src/ui/session_manager/buffer.rs:130,184,267,331,711-730,1012`
- Test: 同文件 `mod tests`

- [ ] **Step 1: 写失败的测试**

加在 `buffer.rs` 的 `mod tests` 里,紧跟 `sftp_prefs_survive_the_editor_round_trip`
之后:

```rust
    /// F154:本地书签不是表单字段,`to_draft` 是整份重建 `SftpPrefs` ——
    /// 不显式保住的话,用户在会话编辑器里点一次「保存」就把它们全清了,
    /// **而且没有任何提示**(同 `preserved_automation` 那条的教训)。
    ///
    /// 自证会变红:把 `to_draft` 里的
    /// `local_bookmarks: buf.preserved_local_bookmarks.clone()` 换成
    /// `local_bookmarks: Vec::new()`。
    #[test]
    fn local_bookmarks_survive_an_editor_round_trip_even_though_no_field_shows_them() {
        let mut rec = rec_with_jump(None);
        rec.sftp = mullion_store::SftpPrefs {
            default_remote: Some("/srv/app".into()),
            default_local: Some(r"D:\work".into()),
            bookmarks: vec![mullion_store::Bookmark {
                name: "日志".into(),
                path: "/var/log".into(),
            }],
            local_bookmarks: vec![mullion_store::Bookmark {
                name: "工程".into(),
                path: r"D:\work\proj".into(),
            }],
        };
        let buf = EditorBuffer::from_record(&rec);
        let draft = to_draft(&buf, None).expect("表单该能转回草稿");
        assert_eq!(
            draft.sftp.local_bookmarks.len(),
            1,
            "编辑器保存把本地书签清空了"
        );
        assert_eq!(draft.sftp.local_bookmarks[0].path, r"D:\work\proj");
    }
```

- [ ] **Step 2: 跑测试确认变红**

```bash
cargo test -p mullion-app --lib local_bookmarks_survive_an_editor 2>&1 | tail -20
```

预期:编译失败(`SftpPrefs` 缺字段 / `preserved_local_bookmarks` 不存在)。

- [ ] **Step 3: 实现**

1. `buffer.rs:130` 的 `preserved_automation` 旁边加字段:

```rust
    /// F154:本地书签。表单里没有对应字段(本地目录收藏是在文件面板上点
    /// ☆ 加的),原样带着走 —— 不带的话保存一次就全没了,静默。
    pub preserved_local_bookmarks: Vec<mullion_store::Bookmark>,
```

2. `buffer.rs:184`(`Default`)加 `preserved_local_bookmarks: Vec::new(),`。
3. `buffer.rs:267`(手写的 `Debug`)加
   `.field("preserved_local_bookmarks", &self.preserved_local_bookmarks)`。
4. `buffer.rs:331`(`from_record`)加
   `preserved_local_bookmarks: rec.sftp.local_bookmarks.clone(),`。
5. `buffer.rs:711` 的 `SftpPrefs { .. }` 里,`bookmarks: ...collect(),` 之后加:

```rust
            // F154:表单里没有这一项,原样带回去(同 `preserved_automation`)。
            local_bookmarks: buf.preserved_local_bookmarks.clone(),
```

6. `buffer.rs:1012` 那处测试里的 `SftpPrefs { .. }` 字面量加
   `local_bookmarks: Vec::new(),`。

- [ ] **Step 4: 跑测试确认变绿**

```bash
cargo test -p mullion-app --lib buffer:: 2>&1 | tail -5
```

预期:`test result: ok`。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/ui/session_manager/buffer.rs
git commit -m "fix(app): 会话编辑器保存不再清空本地书签 (F154)

to_draft 是整份重建 SftpPrefs,表单里没有的字段必须显式保住——
同 preserved_automation 那条的教训。"
```

---

## Task 3: 本地栏画真的书签视图

**Files:**
- Modify: `crates/mullion-app/src/ui/files_panel.rs:82-90`(删 `none()`)、`1359`(加字段)、`1385-1418`(`Default`/`new`)、`1523`、`1672`
- Test: 同文件 `mod tests`

- [ ] **Step 1: 写失败的测试**

加在 `files_panel.rs` 的 `mod tests` 里。先加脚手架(放在既有 `run_remote` 之后):

```rust
    /// 跑一帧**本地**栏。与 `run_remote` 对称,只差栏别和标题。
    fn run_local(
        ctx: &egui::Context,
        state: &mut PaneState,
        cols: &mut ColWidths,
        bookmarks: &[mullion_store::Bookmark],
        can_edit: bool,
        input: egui::RawInput,
    ) -> (Option<FileAction>, Vec<egui::epaint::ClippedShape>) {
        let t = crate::theme::MULLION_DARK;
        let mut action = None;
        let out = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                action = show(
                    ui,
                    &t,
                    "本地",
                    1,
                    PanelColumn::Local,
                    state,
                    false,
                    BookmarkView {
                        list: bookmarks,
                        can_edit,
                    },
                    0,
                    cols,
                );
            });
        });
        (action, out.shapes)
    }
```

再加两条测试:

```rust
    /// F154:本地栏的 ☆ 不再是死按钮 —— 点它发 `BookmarkAdd`,路径取本地 cwd。
    ///
    /// 自证会变红:把 `sidebar`/`content` 里本地栏那两个调用点改回
    /// `BookmarkView::none()`(那时 `can_edit=false`,按钮点不动)。
    #[test]
    fn clicking_the_local_hollow_star_bookmarks_the_current_local_directory() {
        let ctx = egui::Context::default();
        let mut state = ready_at(b"/home/me/proj");
        let mut cols = ColWidths::default();
        let (_, shapes) = run_local(
            &ctx,
            &mut state,
            &mut cols,
            &[],
            true,
            egui::RawInput::default(),
        );
        let pos = find_text_pos(&shapes, "☆").expect("没收藏时该画空心星");
        let (action, _) = run_local(&ctx, &mut state, &mut cols, &[], true, click_at(pos));
        match action {
            Some(FileAction::BookmarkAdd { path, .. }) => {
                assert_eq!(path, "/home/me/proj", "收藏的不是本地栏当前目录");
            }
            other => panic!("本地栏点 ☆ 没有发出 BookmarkAdd:{other:?}"),
        }
    }

    /// F154 接线守护:本地栏读的是**本地**那份列表。传成远端那份的话,
    /// 收藏了本地目录也不会变实心 —— 而 store 里其实存着(静默不一致)。
    ///
    /// 自证会变红:把 `content` 里本地栏的 `list: &frame.local_bookmarks`
    /// 改成 `list: &frame.bookmarks`。
    #[test]
    fn the_local_column_reads_the_local_bookmark_list_not_the_remote_one() {
        let t = crate::theme::MULLION_DARK;
        let mut frame = PanelFrame {
            remote: PaneState::new(RemotePath::from_bytes(b"/srv".to_vec())),
            local: PaneState::new(RemotePath::from_bytes(b"/home/me".to_vec())),
            bookmarks: Vec::new(),
            local_bookmarks: vec![mullion_store::Bookmark {
                name: "家".into(),
                path: "/home/me".into(),
            }],
            session_bound: true,
            active_column: PanelColumn::default(),
        };
        frame.remote.load = Load::Ready;
        frame.local.load = Load::Ready;
        let ctx = egui::Context::default();
        let mut cols = ColWidths::default();
        let mut shapes = Vec::new();
        // 两帧:`CentralPanel` 首帧是 sizing pass(同本文件其余跑帧测试)。
        for _ in 0..2 {
            shapes = ctx
                .run(egui::RawInput::default(), |ctx| {
                    content(ctx, &t, 1, false, &mut frame, 0, &mut cols, &mut None);
                })
                .shapes;
        }
        let star = find_text_pos(&shapes, "★").expect("本地目录已收藏,本地栏该画实心星");
        let mid = ctx.screen_rect().center().x;
        assert!(
            star.x < mid,
            "实心星画在了右半边(远端栏)—— 本地栏读的不是本地那份列表"
        );
    }
```

- [ ] **Step 2: 跑测试确认变红**

```bash
cargo test -p mullion-app --lib files_panel:: 2>&1 | tail -20
```

预期:编译失败,`PanelFrame` 没有 `local_bookmarks` 字段。

- [ ] **Step 3: 实现**

1. `files_panel.rs:1359` 的 `bookmarks` 字段之后加:

```rust
    /// F154:该会话配置的**本地**书签,转给本地栏。与 `bookmarks` 分两份的
    /// 理由见 `mullion_store::SftpPrefs::local_bookmarks`。
    pub local_bookmarks: Vec<mullion_store::Bookmark>,
```

2. `Default`(`files_panel.rs:1385` 那个 `fn default`)加 `local_bookmarks: Vec::new(),`。
3. `PanelFrame::new`(`1406`)加一个参数并填上:

```rust
    pub fn new(
        default_local: Option<&str>,
        bookmarks: Vec<mullion_store::Bookmark>,
        local_bookmarks: Vec<mullion_store::Bookmark>,
        session_bound: bool,
    ) -> Self {
        Self {
            local: PaneState::new(crate::files::local::default_local(default_local)),
            bookmarks,
            local_bookmarks,
            session_bound,
            ..Self::default()
        }
    }
```

4. `sidebar`(`1523`)和 `content`(`1672`)里本地栏那两个 `BookmarkView::none()`
   换成:

```rust
                        BookmarkView {
                            list: &frame.local_bookmarks,
                            can_edit: frame.session_bound,
                        },
```

5. 删掉 `BookmarkView::none()`(`files_panel.rs:82-90` 整个 `impl BookmarkView`)
   —— 本次改动之后它没有调用方。
6. 修既有测试里两处 `PanelFrame { .. }` 字面量(`2249`、`2669` 附近)各加
   `local_bookmarks: Vec::new(),`。

- [ ] **Step 4: 跑测试确认变绿**

```bash
cargo test -p mullion-app --lib files_panel:: 2>&1 | tail -5
```

预期:`test result: ok`。此时 `app.rs` 的两个 `PanelFrame::new` 调用点会因为
少一个参数编译失败——那是 Task 4 的第一步,本步只跑 `--lib` 之外别急。
若 `--lib` 也编不过(同一 crate),先按 Task 4 Step 3 的第 1 条把两个调用点补上,
再回来跑本步。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/ui/files_panel.rs
git commit -m "feat(app): 文件面板本地栏接上本地书签视图 (F154)

本地栏不再传 BookmarkView::none()(那让 ☆ 恒置灰);两份列表分开,
新增接线守护钉住「本地栏读的是本地那份」。"
```

---

## Task 4: 本地书签落盘接线

**Files:**
- Modify: `crates/mullion-app/src/app.rs:3100-3105`(本地栏的书签分支)、`3183-3190`(远端调用点)、`3266-3327`(两个方法加参数)、`5234`/`5335`(`PanelFrame::new` 调用点)、`10352`(接线守护)
- Test: `crates/mullion-app/src/app.rs` 的 `mod tests`

- [ ] **Step 1: 写失败的测试**

改既有的接线守护(`app.rs:10352` 那条测试,现在遍历
`["fn add_bookmark(&mut self", "fn remove_bookmark(&mut self"]`),在它的
断言循环里补两条:

```rust
        for f in ["fn add_bookmark(&mut self", "fn remove_bookmark(&mut self"] {
            let after = src.split(f).nth(1).unwrap_or_else(|| panic!("找不到 {f}"));
            // 到下一个方法定义为止 = 这一个函数的函数体。
            let body = &after[..after.find("\n    fn ").expect("找不到该函数的结尾")];
            assert!(
                body.contains("by_generation(generation)"),
                "{f} 的函数体切歪了 —— 下面那条断言会空过"
            );
            assert!(
                body.contains("store.save()"),
                "{f} 只改了内存没存盘:收藏在重启后消失(F139)"
            );
            // F154:两栏各有一条 store 路径和一份帧内镜像,漏掉任何一条的
            // 症状都是「某一栏的 ☆ 点了没反应 / 重启后没了」,且不报错。
            assert!(
                body.contains("PanelColumn::Local"),
                "{f} 没有按栏分流 —— 本地栏的收藏会写进远端那份列表"
            );
            assert!(
                body.contains("local_bookmarks"),
                "{f} 没碰本地那份镜像 —— 本地栏收藏后这一帧不会变实心"
            );
        }
```

再加一条:本地栏不再把书签动作当接线错误扔掉。

```rust
    /// F154 接线守护:本地栏收到书签动作要**真处理**,不是记一条 warn 扔掉。
    ///
    /// 自证会变红:把 `apply_local_file_action` 里那两条分支改回
    /// `log::warn!("本地栏收到了书签动作,已忽略(书签只属于远端栏)")`。
    #[test]
    fn the_local_column_actually_stores_its_bookmarks() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn apply_local_file_action")
            .nth(1)
            .expect("找不到 apply_local_file_action");
        let body = &after[..after.find("\n    fn ").expect("找不到该函数的结尾")];
        assert!(
            body.contains("self.add_bookmark(") && body.contains("self.remove_bookmark("),
            "本地栏没接上书签落盘 —— ☆ 点了什么都不会发生"
        );
        assert!(
            !body.contains("书签只属于远端栏"),
            "本地栏还在把书签动作当接线错误扔掉(F154 已经把它接上了)"
        );
    }
```

- [ ] **Step 2: 跑测试确认变红**

```bash
cargo test -p mullion-app --lib app::tests::the_local_column_actually_stores 2>&1 | tail -20
```

预期:FAIL(断言不成立),或编译失败(`PanelFrame::new` 参数对不上)。

- [ ] **Step 3: 实现**

1. `app.rs:5234` 与 `app.rs:5335` 两处 `PanelFrame::new(...)` 补第三个参数
   (两处写法相同):

```rust
                    files: crate::ui::files_panel::PanelFrame::new(
                        sftp_prefs.default_local.as_deref(),
                        sftp_prefs.bookmarks,
                        sftp_prefs.local_bookmarks,
                        // F139:没有会话记录就没地方存书签,☆ 置灰。
                        session_id.is_some(),
                    ),
```

注意 `sftp_prefs` 在这两处之间只 clone 了一次(`app.rs:5220`
`.map(|rec| rec.sftp.clone())`),两个分支各自 move 自己那份,不冲突。

2. `add_bookmark` / `remove_bookmark` 各加一个栏别参数(`app.rs:3266`、`3304`):

```rust
    /// F139/F154:☆ 收藏必须**当场存盘**。这两个方法各写两处(store 一份、
    /// 这一帧的镜像一份),漏掉任何一处都是静默的不一致。
    ///
    /// `column` 决定落在哪一份列表上:两栏的路径空间毫无关系,混着存会让
    /// 路径条那句「当前 cwd 在不在列表里」的现算判据在两栏之间串味。
    fn add_bookmark(
        &mut self,
        generation: u64,
        path: String,
        name: String,
        column: crate::files::PanelColumn,
    ) {
        let Some(sid) = self
            .tabs
            .by_generation(generation)
            .and_then(|t| t.session_id)
        else {
            // UI 已经按 `BookmarkView::can_edit` 把 ☆ 置灰了,走到这儿说明
            // 接线被改坏了 —— 不静默吞。
            log::warn!("收到 BookmarkAdd 但标签没有 SessionId,已忽略");
            return;
        };
        let mark = mullion_store::Bookmark {
            name,
            path: path.clone(),
        };
        let local = column == crate::files::PanelColumn::Local;
        if let Some(store) = self.store.as_mut() {
            let r = if local {
                store.add_local_bookmark(sid, mark.clone())
            } else {
                store.add_bookmark(sid, mark.clone())
            };
            if let Err(e) = r.and_then(|_| store.save()) {
                self.ui.set_error(e.to_string());
                return;
            }
        }
        if let Some(files) = self
            .tabs
            .by_generation_mut(generation)
            .and_then(|t| t.content.files_panel_mut())
        {
            let list = if local {
                &mut files.local_bookmarks
            } else {
                &mut files.bookmarks
            };
            // 去重判据与 store 侧同一条(按路径),两边不许分叉。
            if !list.iter().any(|b| b.path == mark.path) {
                list.push(mark);
            }
        }
        self.ui_dirty = true;
    }

    /// F139/F154:取消收藏。按路径相等匹配 —— 书签的身份就是路径。
    fn remove_bookmark(
        &mut self,
        generation: u64,
        path: String,
        column: crate::files::PanelColumn,
    ) {
        let Some(sid) = self
            .tabs
            .by_generation(generation)
            .and_then(|t| t.session_id)
        else {
            log::warn!("收到 BookmarkRemove 但标签没有 SessionId,已忽略");
            return;
        };
        let local = column == crate::files::PanelColumn::Local;
        if let Some(store) = self.store.as_mut() {
            let r = if local {
                store.remove_local_bookmark(sid, &path)
            } else {
                store.remove_bookmark(sid, &path)
            };
            if let Err(e) = r.and_then(|_| store.save()) {
                self.ui.set_error(e.to_string());
                return;
            }
        }
        if let Some(files) = self
            .tabs
            .by_generation_mut(generation)
            .and_then(|t| t.content.files_panel_mut())
        {
            if local {
                files.local_bookmarks.retain(|b| b.path != path);
            } else {
                files.bookmarks.retain(|b| b.path != path);
            }
        }
        self.ui_dirty = true;
    }
```

3. 远端调用点(`app.rs:3183-3190`)补参数:

```rust
            FileAction::BookmarkAdd { name, path } => {
                self.add_bookmark(
                    generation,
                    path.clone(),
                    name.clone(),
                    crate::files::PanelColumn::Remote,
                );
                return;
            }
            FileAction::BookmarkRemove { path } => {
                self.remove_bookmark(
                    generation,
                    path.clone(),
                    crate::files::PanelColumn::Remote,
                );
                return;
            }
```

4. 本地栏那条 warn 分支(`app.rs:3100-3105`)换成真处理。**注意它在
   `let target = match &action {` 那个表达式里**,而 `add_bookmark` 要
   `&mut self` —— 借着 `files` 是调不了的。所以要把它挪到函数更靠前的位置,
   与既有的 `Transfer`/`Drop`/`Reconnect` 那几条早退分流放在一起
   (`app.rs:3029-3048` 那一段之后、`let Some(tab) = ...` 之前):

```rust
        // F154:本地目录收藏。**在借出 `files` 之前分流** —— 它们要
        // `&mut self`(store + 存盘),借着 `tab.content.files_panel_mut()`
        // 是调不了的;而且它们不改当前目录,不该走下面那条 `target` 的路。
        match &action {
            FileAction::BookmarkAdd { name, path } => {
                self.add_bookmark(
                    generation,
                    path.clone(),
                    name.clone(),
                    crate::files::PanelColumn::Local,
                );
                return;
            }
            FileAction::BookmarkRemove { path } => {
                self.remove_bookmark(
                    generation,
                    path.clone(),
                    crate::files::PanelColumn::Local,
                );
                return;
            }
            _ => {}
        }
```

   并把原来 `let target = match &action { ... }` 里那条
   `FileAction::BookmarkAdd { .. } | FileAction::BookmarkRemove { .. } => { log::warn!(...); return; }`
   分支整个删掉(上面已经分流走了,留着会让 `match` 出现不可达臂)。

- [ ] **Step 4: 跑测试确认变绿**

```bash
cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED|panicked" | tail -5
```

预期:`test result: ok`。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): 本地栏 ☆ 真的落盘 (F154)

add/remove_bookmark 加栏别参数,两栏各写各的列表;本地栏那条
「已忽略」的 warn 分支换成真处理。接线守护扩到两栏都钉。"
```

---

## Task 5: 修「重连失败一次后占位标签永久连不上」

**这是既存 bug,不是新功能。** `ConnectErr` 既不清 `pending_restore` 也不复位
`RestoredTab::dialing`,而 `reconnect_tab` 开头有 `if self.pending_restore.is_some()
{ return; }` —— 一次失败之后,这个进程里所有占位标签的「重连」都静默失效,
按钮永远停在禁用的「连接中…」。Task 6 的串行队列必然踩到它。

**Files:**
- Modify: `crates/mullion-app/src/app.rs:6677-6686`
- Test: `crates/mullion-app/src/app.rs` 的 `mod tests`

- [ ] **Step 1: 写失败的测试**

加在 `app.rs` 的 `mod tests` 里:

```rust
    /// **接线守护 / F37 既存 bug**:拨号失败必须把 `pending_restore` 和那个
    /// 标签的 `dialing` 一起收口。
    ///
    /// 少了 `pending_restore.take()`:`reconnect_tab` 开头那道闸永久关闭 ——
    /// 这个进程里**所有**占位标签的「重连」从此静默无反应。
    /// 少了 `dialing = false`:按钮永远停在禁用的「连接中…」。
    /// 两条都是「一次失败换永久坏 + 全程不报错」,所以分开断言 —— 只钉一条
    /// 会让另一条悄悄退化。
    ///
    /// 自证会变红:把 `ConnectErr` 分支里的 `self.pending_restore.take()`
    /// 删掉(第一条红),或把复位 `dialing` 那几行删掉(第二条红)。
    #[test]
    fn a_failed_dial_releases_the_reconnect_latch_and_re_enables_the_button() {
        let src = include_str!("app.rs");
        let after = src
            .split("UserEvent::ConnectErr(msg) => {")
            .nth(1)
            .expect("找不到 ConnectErr 分支");
        let body = &after[..after.find("\n            UserEvent::").unwrap_or(after.len())];
        assert!(
            body.contains("self.pending_restore.take()"),
            "拨号失败没释放 pending_restore —— 之后所有占位标签都再也连不上"
        );
        assert!(
            body.contains("dialing = false"),
            "拨号失败没复位 dialing —— 那个标签的「重连」按钮永远禁用"
        );
    }
```

- [ ] **Step 2: 跑测试确认变红**

```bash
cargo test -p mullion-app --lib a_failed_dial_releases 2>&1 | tail -10
```

预期:FAIL,提示「拨号失败没释放 pending_restore」。

- [ ] **Step 3: 实现**

`app.rs:6677` 的 `ConnectErr` 分支改成:

```rust
            UserEvent::ConnectErr(msg) => {
                // 待定 F:CLI 直连从未成功连过时,保留可脚本化的 exit(1) 语义;
                // launcher 态(或已连过又断开)只记错误,交 UI 展示(ui.last_error)。
                crate::logx::line(&format!("连接失败: {msg}"));
                if self.cli_direct && self.active_ws().is_none() {
                    std::process::exit(1);
                }
                // F37:这次失败的如果是某个占位标签的重连,**必须在这里收口**。
                // 不收的话 `reconnect_tab` 开头那道 `pending_restore` 闸永久
                // 关闭 —— 这个进程里所有占位标签的「重连」从此静默无反应,
                // 而按钮还停在禁用的「连接中…」。没有自愈路径,只能重启 exe。
                if let Some(p) = self.pending_restore.take() {
                    if let Some(TabContent::Restored(r)) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.id == p.tab_id)
                        .map(|t| &mut t.content)
                    {
                        r.dialing = false;
                    }
                }
                self.ui.set_error(msg);
                self.request_ui_redraw();
            }
```

- [ ] **Step 4: 跑测试确认变绿**

```bash
cargo test -p mullion-app --lib a_failed_dial_releases 2>&1 | tail -5
```

预期:`test result: ok. 1 passed`。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "fix(app): 拨号失败后收口 pending_restore 与 dialing (F37)

既存 bug:一次重连失败之后,reconnect_tab 开头那道闸永久关闭,
本进程内所有占位标签再也连不上,按钮永停在「连接中…」,且不报错。"
```

---

## Task 6: 恢复现场后自动串行重连(F153-a)

**Files:**
- Modify: `crates/mullion-app/src/app.rs`(新增 `AutoDial` 结构、`next_auto_dial` 自由函数、`advance_auto_dial` 方法;`reconnect_tab` 返回 `bool`;`restore_history` 收尾;`ConnectOk`/`ConnectErr` 推进)
- Test: `crates/mullion-app/src/app.rs` 的 `mod tests`

- [ ] **Step 1: 写失败的测试**

```rust
    /// F153:自动串行拨号选下一条时**必须跳过已经试过的**。
    ///
    /// 不跳的话:失败那条的 `dialing` 刚被 `ConnectErr` 复位(Task 5),
    /// 「第一个未 dialing 的占位标签」判据会把它反复选中 —— 队列在一条
    /// 连不上的会话上原地打转,永远走不到后面的标签。
    ///
    /// 自证会变红:把 `next_auto_dial` 里的 `!tried.contains(&t.id)` 去掉。
    #[test]
    fn the_auto_dial_queue_skips_tabs_it_already_tried() {
        let mut tabs: Tabs<TabContent> = Tabs::default();
        tabs.open("甲".into(), Some(SessionId(1)), restored_tab(1, 1));
        tabs.open("乙".into(), Some(SessionId(2)), restored_tab(2, 1));
        let first = next_auto_dial(&tabs, &[]).expect("该给出第一条");
        let second = next_auto_dial(&tabs, &[first]).expect("该给出第二条");
        assert_ne!(first, second, "试过的标签又被选了一次 —— 队列会原地打转");
        assert_eq!(
            next_auto_dial(&tabs, &[first, second]),
            None,
            "两条都试过了还给第三条"
        );
    }

    /// F153:已经连上的标签不在自动拨号队列里 —— 它没有什么可拨的。
    #[test]
    fn the_auto_dial_queue_only_looks_at_placeholder_tabs() {
        let tabs = tabs_with_one_terminal_tab();
        assert_eq!(next_auto_dial(&tabs, &[]), None);
    }

    /// F153:收尾那条 toast 的文案。全成功和有失败要分得开 —— 后者得让用户
    /// 知道「有几条要自己点」。
    #[test]
    fn the_auto_dial_summary_tells_failures_apart_from_a_clean_run() {
        assert_eq!(auto_dial_summary(3, 0), "已自动连上 3 个标签");
        assert_eq!(
            auto_dial_summary(2, 1),
            "2 条已连接,1 条失败(点「重连」可再试)"
        );
    }

    /// **接线守护 / F153**:恢复现场之后要自己开始拨号,不能等用户挨个点。
    ///
    /// 自证会变红:删掉 `restore_history` 里那两句 `self.auto_dial = ...` /
    /// `self.advance_auto_dial(None)`。
    #[test]
    fn restoring_a_record_starts_dialing_on_its_own() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn restore_history(")
            .nth(1)
            .expect("找不到 restore_history");
        let body = &after[..after.find("\n    }\n").expect("找不到 restore_history 的结尾")];
        assert!(
            body.contains("self.advance_auto_dial("),
            "恢复现场之后没有自动开始拨号(F153)"
        );
    }

    /// **接线守护 / F153**:两种结局都要推进队列。只接 `ConnectOk` 的话,
    /// 第一条连不上就把整条队列吊死在那儿。
    ///
    /// 自证会变红:删掉 `ConnectErr` 分支里那句 `advance_auto_dial(Some(false))`。
    #[test]
    fn both_outcomes_advance_the_auto_dial_queue() {
        let src = include_str!("app.rs");
        let err = src
            .split("UserEvent::ConnectErr(msg) => {")
            .nth(1)
            .expect("找不到 ConnectErr 分支");
        let err_body = &err[..err.find("\n            UserEvent::").unwrap_or(err.len())];
        assert!(
            err_body.contains("advance_auto_dial(Some(false))"),
            "拨号失败不推进队列 —— 一条连不上就把后面全吊死"
        );
        let ok = src
            .split("UserEvent::ConnectOk {")
            .nth(1)
            .expect("找不到 ConnectOk 分派点");
        let ok_body = &ok[..ok.find("\n            UserEvent::").unwrap_or(ok.len())];
        assert!(
            ok_body.contains("advance_auto_dial(Some(true))"),
            "拨号成功不推进队列 —— 只会连上第一条"
        );
    }
```

- [ ] **Step 2: 跑测试确认变红**

```bash
cargo test -p mullion-app --lib auto_dial 2>&1 | tail -20
```

预期:编译失败(`next_auto_dial`/`auto_dial_summary` 不存在)。

- [ ] **Step 3: 实现**

1. 在 `PendingRestore`(`app.rs:559`)之后加结构体:

```rust
/// F153:恢复现场之后正在自动串行拨号。`None` = 没在自动拨。
///
/// **一条接一条,不并发**:F37 §1 否掉自动重连的理由是「别让高延迟代理链路上
/// 同时挤一堆握手」——那条理由否的是并发,不是自动。串行既满足「恢复完就能用」,
/// 也不违反它。
#[derive(Debug, Default)]
struct AutoDial {
    /// 已经试过的标签(不管成没成)。**不能省**:失败那条的 `dialing` 会被
    /// `ConnectErr` 复位,「第一个未 dialing 的占位标签」判据会把它反复选中,
    /// 队列在一条连不上的会话上原地打转。
    tried: Vec<shell::tabs::TabId>,
    ok: usize,
    err: usize,
}
```

2. 在 `replace_target`(`app.rs:1390`)附近加两个自由函数(**自由函数而不是方法**:
   `App` 要 `EventLoopProxy`,单测里造不出来):

```rust
/// F153:自动串行拨号该轮到哪个占位标签。`None` = 没有下一条了。
fn next_auto_dial(
    tabs: &shell::tabs::Tabs<TabContent>,
    tried: &[shell::tabs::TabId],
) -> Option<shell::tabs::TabId> {
    tabs.iter().find_map(|t| match &t.content {
        TabContent::Restored(_) if !tried.contains(&t.id) => Some(t.id),
        _ => None,
    })
}

/// F153:自动串行拨号收尾那条 toast。抽成纯函数 —— 文案是这条路径上唯一
/// 有分支的东西,跑一整轮拨号去测它是拿最贵的手段测最便宜的。
fn auto_dial_summary(ok: usize, err: usize) -> String {
    if err == 0 {
        format!("已自动连上 {ok} 个标签")
    } else {
        format!("{ok} 条已连接,{err} 条失败(点「重连」可再试)")
    }
}
```

3. `App` 加字段(挨着 `pending_restore`,`app.rs:1601`):

```rust
    /// F153:恢复现场之后的自动串行拨号进度。
    auto_dial: Option<AutoDial>,
```

   并在构造处(`app.rs:1902` 那行 `pending_restore: None,` 旁)加 `auto_dial: None,`。

4. `reconnect_tab` 改成返回「有没有真的发起拨号」。签名与两处早退:

```rust
    fn reconnect_tab(&mut self, tab_id: shell::tabs::TabId) -> bool {
```

   把函数体里所有裸 `return;` 改成 `return false;`,末尾 `self.spawn_connect(cfg, wants_sftp);`
   之后加 `true`。**为什么要返回值**:缺凭据 / 库没打开时这个函数直接 return,
   不会有 `ConnectOk`/`ConnectErr` 回来 —— 自动队列会永远等一个不来的事件。

   既有的三个调用点全部改成 `let _ = self.reconnect_tab(...);`:
   `app.rs:2502`(`demote_files_tab_and_reconnect` 末尾)、
   `app.rs:2518`(`reconnect_next_restored` 末尾)、
   `app.rs:7876`(占位中央区「重连」按钮的处置)。

5. 加推进方法(放在 `reconnect_next_restored` 之后):

```rust
    /// F153:推进自动串行拨号。`outcome` = 刚结束那一条的结果
    /// (`Some(true)` 成功 / `Some(false)` 失败 / `None` = 起点,还没拨过)。
    ///
    /// 用 `loop` 而不是递归:一条都没发起出去(缺凭据/库没打开)时要接着
    /// 试下一条,递归深度会跟着标签数走。
    fn advance_auto_dial(&mut self, outcome: Option<bool>) {
        let Some(mut auto) = self.auto_dial.take() else {
            return;
        };
        match outcome {
            Some(true) => auto.ok += 1,
            Some(false) => auto.err += 1,
            None => {}
        }
        loop {
            let Some(next) = next_auto_dial(&self.tabs, &auto.tried) else {
                // 一条都没试过就到头了(恢复出来的标签全被筛掉)——不报
                // 「已自动连上 0 个标签」,那只会让人以为出了什么事。
                if !auto.tried.is_empty() {
                    self.ui.set_toast(auto_dial_summary(auto.ok, auto.err));
                }
                self.ui_dirty = true;
                return;
            };
            auto.tried.push(next);
            if self.reconnect_tab(next) {
                self.auto_dial = Some(auto);
                self.ui_dirty = true;
                return;
            }
            // 连拨号都没发起出去 —— 不会有 `ConnectOk`/`ConnectErr` 回来
            // 推进队列,当场记一笔失败,接着试下一条。
            auto.err += 1;
        }
    }
```

6. `restore_history` 末尾(`self.ui_dirty = true;` 之前)加:

```rust
        // F153:摆完就一条接一条地拨号。用户选「恢复」的意思就是要用它们,
        // 不是要再挨个点一遍「重连」。
        self.auto_dial = Some(AutoDial::default());
        self.advance_auto_dial(None);
```

7. `ConnectOk` 的分派点(`app.rs:6202-6206`)改成:

```rust
            UserEvent::ConnectOk {
                handle,
                wants_sftp,
                pty,
            } => {
                self.accept_connect_ok(handle, wants_sftp, pty);
                // F153:**在分派点推进而不是在 `accept_connect_ok` 里面** ——
                // 那个函数有两条早退 return(SFTP 标签、缺 pty 的异常路径),
                // 写在里面会漏掉其中一条,症状是自动拨号连到某个 SFTP 标签
                // 就停住。
                self.advance_auto_dial(Some(true));
            }
```

8. `ConnectErr` 分支里,在 Task 5 加的那段收口之后、`self.ui.set_error(msg);`
   之前加:

```rust
                // F153:这一条完了,接着拨下一条。失败不中断队列 —— 一条会话
                // 的凭据不对,不该把其余的一起吊死。
                self.advance_auto_dial(Some(false));
```

- [ ] **Step 4: 跑测试确认变绿**

```bash
cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED|panicked" | tail -5
```

预期:`test result: ok`。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): 恢复现场后一条接一条自动重连 (F153)

复用既有 reconnect_tab,不分叉第二条拨号路径;tried 集合防止失败那条
被反复选中;ConnectOk/ConnectErr 两种结局都推进队列。tmux attach 由
既有登录后自动化承接,零新代码。"
```

---

## Task 7: 恢复弹窗单击即恢复(F153-b)

**Files:**
- Modify: `crates/mullion-app/src/ui/history.rs:129-190`
- Test: 同文件 `mod tests`

- [ ] **Step 1: 写失败的测试**

先加脚手架(放在既有 `click` 之后):

```rust
    /// 点第 `i` 行的中央。行高 44,与 `show` 里 `allocate_response` 的取值
    /// 同一个来源 —— 那边改了这里要跟着改(改漏的话点空,测试会红)。
    fn click_row(draft: &mut Option<HistoryDraft>, i: usize) -> Option<HistoryOut> {
        let t = crate::theme::MULLION_DARK;
        let ctx = egui::Context::default();
        let mut shapes = Vec::new();
        for _ in 0..2 {
            shapes = ctx
                .run(egui::RawInput::default(), |ctx| {
                    show(ctx, &t, draft);
                })
                .shapes;
        }
        // 拿第 i 行的第一行文字当锚点:它就画在那一行的 rect 里。
        fn find(shape: &egui::Shape, label: &str) -> Option<egui::Pos2> {
            match shape {
                egui::Shape::Vec(v) => v.iter().find_map(|s| find(s, label)),
                egui::Shape::Text(ts) if ts.galley.text() == label => {
                    Some(ts.pos + ts.galley.size() / 2.0)
                }
                _ => None,
            }
        }
        let head = draft.as_ref().expect("草稿").rows[i].head.clone();
        let pos = shapes
            .iter()
            .find_map(|cs| find(&cs.shape, &head))
            .unwrap_or_else(|| panic!("列表里没有第 {i} 行(head = {head})"));
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
        let mut out = None;
        let _ = ctx.run(input, |ctx| {
            out = show(ctx, &t, draft);
        });
        out
    }
```

再加两条测试:

```rust
    /// F153-b:单击一行就恢复那一条,不用再去够「恢复」按钮。
    ///
    /// 自证会变红:把 `clicked()` 分支改回只写 `d.selected = i`。
    #[test]
    fn clicking_a_row_restores_that_record_right_away() {
        let mut draft = Some(HistoryDraft::new(rows()));
        assert_eq!(click_row(&mut draft, 1), Some(HistoryOut::Restore("b".into())));
    }

    /// F153:提示语必须跟行为一致。自动重连之后,「点「重连」才拨号」是假话
    /// —— 用户照着它等,以为程序没反应。
    ///
    /// 自证会变红:把那句文案改回去。
    #[test]
    fn the_hint_no_longer_promises_a_manual_reconnect() {
        let mut draft = Some(HistoryDraft::new(rows()));
        let joined = texts(&mut draft).join(" ");
        assert!(
            !joined.contains("点「重连」才拨号"),
            "提示语还在说要手动重连:{joined}"
        );
        assert!(
            joined.contains("自动重连"),
            "提示语没说会自动重连:{joined}"
        );
    }
```

- [ ] **Step 2: 跑测试确认变红**

```bash
cargo test -p mullion-app --lib history:: 2>&1 | tail -20
```

预期:两条新测试 FAIL(单击只选中、文案还是老的)。

- [ ] **Step 3: 实现**

`history.rs:182-188` 那两段改成:

```rust
                        // F153-b:**单击即恢复**。原来是「单击选中 + 双击恢复」,
                        // 用户报的是「点了没反应」—— 双击在高延迟远程桌面/触控板
                        // 上本来就不好按,而这个弹窗只有一件事可做。
                        //
                        // `selected` 与「恢复」按钮都留着:那是键盘路径的出口。
                        if resp.clicked() {
                            d.selected = i;
                            out = Some(HistoryOut::Restore(row.id.clone()));
                        }
```

(删掉原来的 `double_clicked()` 分支 —— 双击的第二次点击同样会置 `clicked()`,
留着是死代码。)

`history.rs:129-132` 的提示语改成:

```rust
            ui.label(theme::hint_text(
                t,
                "选一条摆回标签栏,会一条接一条自动重连。",
            ));
```

- [ ] **Step 4: 跑测试确认变绿**

```bash
cargo test -p mullion-app --lib history:: 2>&1 | tail -5
```

预期:`test result: ok`。既有的
`merely_showing_the_dialog_restores_nothing` 必须仍然绿(防止改出「一画出来
就自己恢复」)。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/ui/history.rs
git commit -m "feat(app): 恢复现场弹窗单击一行即恢复 + 文案跟上自动重连 (F153)

提示语原来写「点「重连」才拨号」,自动重连之后那是假话,配文案守护测试。"
```

---

## Task 8: 换程序图标

**Files:**
- Modify: `crates/mullion-app/assets/mullion.ico`
- Delete: `favicon.ico`(仓库根目录那份源文件)

- [ ] **Step 1: 确认新图标的帧齐全**

```bash
python3 -c "
import struct
d=open('favicon.ico','rb').read()
_,typ,n=struct.unpack('<HHH',d[:6])
print('type',typ,'frames',n)
for i in range(n):
    w,h,c,r,pl,bc,sz,off=struct.unpack('<BBBBHHII',d[6+i*16:22+i*16])
    print(' ',w or 256,'x',h or 256,'bpp',bc)
"
```

预期:`type 1`、6 帧、含 16/32/48/256(`tests/icon_resource.rs` 要求的四档)。

- [ ] **Step 2: 替换并删掉源副本**

```bash
cp favicon.ico crates/mullion-app/assets/mullion.ico
rm favicon.ico
```

- [ ] **Step 3: 跑图标守护测试**

```bash
cargo test -p mullion-app --test icon_resource 2>&1 | tail -5
```

预期:`test result: ok`,3 条全过(资源序号、尺寸帧齐全、app.rs 不写字面量)。

- [ ] **Step 4: 提交**

```bash
git add crates/mullion-app/assets/mullion.ico favicon.ico
git commit -m "chore(app): 换程序图标 (F152)

帧结构与前一版一致(16/32/48/64/128/256,32bpp),.rc 与资源序号不动。
根目录那份源文件同时删掉——图标源只留一份,两份迟早对不上。"
```

---

## Task 9: 全绿 + 发版

- [ ] **Step 1: 跑全量测试**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log | tail -20
```

预期:每个 crate 都是 `test result: ok`,没有 FAILED / panicked。

- [ ] **Step 2: clippy 与 fmt**

```bash
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20
cargo fmt --check 2>&1 | tail -5
```

预期:两条都无输出。**只跑单个 crate 的测试不叫绿**(CLAUDE.md)。

- [ ] **Step 3: 发版**

调用 `release-windows` skill,按它的步骤走完:版本 0.1.62 → **0.1.63**、
交叉编译 `x86_64-pc-windows-gnu`、objdump 依赖验收、签名、发 GitHub Release
(走代理)。**别凭记忆做** —— 每一步都有漏了也不报错的坑。

- [ ] **Step 4: 报人工验收清单**

把设计文档末尾那 5 条原样交给用户:图标四处、单击即恢复 + 串行自动连、
tmux 接回原会话、失败一条不拖累其余且「重连」按钮必须有反应、本地栏 ☆
收藏与重启后仍在。

---

## 无法自动验证的部分(必须交人工)

- 图标在 Windows 上的实际观感(任务栏/标题栏/资源管理器)
- 真实拨号:串行的实际节奏、高延迟代理链路下的表现
- tmux 是否真的接回**原来那个**会话(取决于用户的登录后自动化配置;
  没配 tmux 的会话恢复后只登录,这不是 bug)
- 本地书签在 Windows 路径形态(`D:\work`)下的显示与跳转
