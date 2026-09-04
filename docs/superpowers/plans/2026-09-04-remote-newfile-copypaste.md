# 远端栏新建文件(F219)+ 远端内复制/剪切/粘贴(F220)实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 远端文件栏补上「建一个空文件」和「在同一台机器上把东西拷/挪到别的目录」两件事。

**Architecture:** 两片各自可交付。F219 = 列表首行一个**只存在于本地**的输入行(幽灵行),回车才发一次带 `EXCLUDE` 的 create。F220 = per-tab 剪贴板 + 粘贴前预检查 + 一个批量冲突框 + `exec cp -a/mv` 快路径与 SFTP 逐文件回退(与 F57 `remove_tree` 完全对称)。纯逻辑(唯一名、冲突集合、自身/子孙闸门、粘贴计划)全在 `mullion-app/src/files/clip.rs` 里,零 IO、可纯单测;协议动作在 `mullion-ssh/src/copy_tree.rs`,零 UI。

**Tech Stack:** Rust / russh + russh-sftp 2.4.0 / egui 0.30 / winit 0.30。设计出处:`docs/superpowers/specs/2026-09-04-remote-newfile-copypaste-design.md`。

**贯穿全程的三条纪律:**

1. **每个守护测试都要自证变红**:实现写完之后,按测试文档注释里写的「自证会变红」那一句真的改坏一次,确认测试失败,再改回来。变异验证**前先 `git commit`** —— 已经被 `git checkout` 吞过五次未提交的编辑。
2. **「绿」的定义**:`cargo test --workspace` 全过 **且** `cargo clippy --workspace --all-targets -- -D warnings` 无输出。只跑单个 crate 不叫绿。
3. 大输出先落盘再 grep:`cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log`。

---

## 文件结构

**新建**

| 文件 | 职责 |
|---|---|
| `crates/mullion-app/src/files/clip.rs` | F220 纯逻辑:剪贴板类型、唯一名生成、冲突集合、自身/子孙闸门、粘贴计划。零 egui / 零 tokio / 零 IO |
| `crates/mullion-ssh/src/copy_tree.rs` | F220 协议层:`exec cp -a`/`mv` 快路径 + SFTP 逐文件回退。零 UI,与 `remove_tree.rs` 对称 |

**修改**

| 文件 | 改什么 |
|---|---|
| `crates/mullion-ssh/src/sftp.rs` | 加 `SftpClient::create_file`(带 `EXCLUDE`) |
| `crates/mullion-ssh/src/lib.rs` | `pub mod copy_tree;` |
| `crates/mullion-app/src/files/mod.rs` | `pub mod clip;` |
| `crates/mullion-app/src/files/state.rs` | 加 `new_edit` 字段、`begin_new_file()`、与 `rename_edit` 互斥、生命周期 |
| `crates/mullion-app/src/ui/files_panel.rs` | 幽灵行、`name_edit_row` 抽取、菜单四项、`FileAction` 四个新变体、`PanelFrame::clip` |
| `crates/mullion-app/src/ui/files_dialog.rs` | `FilesDialog::PasteConflict`、`FileOp::NewFile`/`Paste`、`cancel_op` 一臂 |
| `crates/mullion-app/src/app.rs` | `Modal::FilesNewName`、`UserEvent::PasteChecked`、`OpFollow`、`Ctrl+N/C/X/V`、两栏动作分派、粘贴编排 |
| `crates/mullion-ssh/tests/common/mod.rs` | 假服务端认 `cp -a`/`mv`(现在只认 `rm -rf --`) |
| `crates/mullion-ssh/tests/sftp_write.rs` | `create_file` 与 `copy_tree` 的集成测试 |
| `spec.md` | F219 / F220 两条 |

---

# 阶段 A —— F219 就地新建文件

## Task A1: `SftpClient::create_file`

**Files:**
- Modify: `crates/mullion-ssh/src/sftp.rs`(在 `open_write` 之后)
- Test: `crates/mullion-ssh/tests/sftp_write.rs`(文件末尾追加)

- [ ] **Step 1: 写失败的测试**

追加到 `crates/mullion-ssh/tests/sftp_write.rs` 末尾:

```rust
/// F219:新建一个空文件 —— 服务端上真的多出这一条,且大小为 0。
///
/// 判据是**服务端的树**,不是「客户端返回了 Ok」:后者恒绿,一个什么都
/// 不做的实现照样通过。
#[tokio::test]
async fn creating_a_file_makes_an_empty_one_appear_on_the_server() {
    let (addr, _probe, tree_h) = common::spawn_sftp_server(tree()).await;
    let sftp = client(addr).await;
    sftp.create_file(&rp("/home/testuser/notes.txt"))
        .await
        .expect("新建文件该成功");
    let t = tree_h.lock().unwrap();
    assert!(
        exists(&t, b"/home/testuser/notes.txt"),
        "服务端上没有这个文件,实际:{:?}",
        names_in(&t, b"/home/testuser")
    );
}

/// F219 的核心闸门:**撞上已存在必须失败**,不能把别人的文件截断成 0 字节。
///
/// 自证会变红:把 `create_file` 里的 `OpenFlags::EXCLUDE` 去掉。
#[tokio::test]
async fn creating_a_file_that_already_exists_fails_instead_of_truncating_it() {
    let (addr, _probe, tree_h) = common::spawn_sftp_server(tree()).await;
    let sftp = client(addr).await;
    // `tree()` 里 /home/testuser/a.txt 是有内容的既存文件。
    let before = {
        let t = tree_h.lock().unwrap();
        t.get(&b"/home/testuser".to_vec())
            .expect("父目录该在")
            .iter()
            .find(|n| n.name == b"a.txt")
            .expect("a.txt 该在")
            .data
            .clone()
    };
    assert!(!before.is_empty(), "前提:这个文件本来有内容");

    let err = sftp.create_file(&rp("/home/testuser/a.txt")).await;
    assert!(err.is_err(), "撞上已存在的文件该失败,而不是悄悄覆盖");

    let t = tree_h.lock().unwrap();
    let after = t
        .get(&b"/home/testuser".to_vec())
        .expect("父目录该在")
        .iter()
        .find(|n| n.name == b"a.txt")
        .expect("a.txt 该在")
        .data
        .clone();
    assert_eq!(after, before, "文件内容被动过了 —— EXCLUDE 没生效");
}
```

先确认 `tree()` 里确实有一个非空的 `a.txt`:

Run: `grep -n "fn tree()" -A 18 crates/mullion-ssh/tests/sftp_write.rs`

若 `a.txt` 不存在或内容为空,把测试里的名字换成 `tree()` 里那个**有内容的**普通文件,并同步改断言里的字面量。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-ssh --test sftp_write creating_a_file 2>&1 | tail -20`
Expected: 编译失败,`no method named 'create_file' found`。

- [ ] **Step 3: 写实现**

在 `crates/mullion-ssh/src/sftp.rs` 的 `open_write` 之后插入:

```rust
    /// F219:新建一个**空文件**。已存在就失败。
    ///
    /// flags 带 `EXCLUDE` —— 与 `open_write` 刻意不带的理由正好相反:
    /// 传输通路要不要覆盖由上层的冲突策略决定(设计 D19),而「新建」撞上
    /// 已存在必须当场失败。不带的话,用户在一个已有 `config.yaml` 的目录里
    /// 手滑建了个同名文件,那份配置会被**静默截断成 0 字节** —— 没有任何
    /// 报错,而远端删除/覆盖不可逆。
    pub async fn create_file(&self, path: &RemotePath) -> Result<(), SftpError> {
        let wire = path.as_wire()?;
        let flags = russh_sftp::protocol::OpenFlags::WRITE
            | russh_sftp::protocol::OpenFlags::CREATE
            | russh_sftp::protocol::OpenFlags::EXCLUDE;
        let file = self
            .inner
            .open_with_flags(wire, flags)
            .await
            .map_err(|e| SftpError::Protocol(e.to_string()))?;
        // 立刻关掉:句柄留着不 close,服务端那边会一直挂着一个打开的文件。
        file.sync_all()
            .await
            .map_err(|e| SftpError::Protocol(e.to_string()))?;
        Ok(())
    }
```

若 `sync_all` 在锁定版 russh-sftp 上不存在,改用与 `RemoteFile::finish` 相同的收尾方式 —— 先读:

Run: `grep -n "pub async fn finish" -A 12 crates/mullion-ssh/src/sftp.rs`

照它那份写法收口(**不要凭记忆写**,见 CLAUDE.md 的「API 漂移」)。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-ssh --test sftp_write creating_a_file 2>&1 | tail -20`
Expected: `test result: ok. 2 passed`

- [ ] **Step 5: 变异验证**

去掉实现里的 `| russh_sftp::protocol::OpenFlags::EXCLUDE`,重跑:
Expected: `creating_a_file_that_already_exists_fails_instead_of_truncating_it` FAILED。改回来。

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-ssh/src/sftp.rs crates/mullion-ssh/tests/sftp_write.rs
git commit -m "feat(ssh): SFTP 新建空文件,撞上已存在即失败 (F219)"
```

---

## Task A2: `PaneState::new_edit` —— 幽灵行的状态

**Files:**
- Modify: `crates/mullion-app/src/files/state.rs`

- [ ] **Step 1: 写失败的测试**

追加到 `crates/mullion-app/src/files/state.rs` 的 `mod tests` 里(沿用该模块已有的 `fn state_with(...)` 之类的构造助手 —— 先读一遍 `mod tests` 开头,用它现成的那个,别新造一个):

```rust
    /// F219:进新建态之后,缓冲是空的、焦点待办是真。
    #[test]
    fn beginning_a_new_file_opens_an_empty_buffer_asking_for_focus() {
        let mut s = ready_state();
        assert!(s.begin_new_file(), "该进得了新建态");
        let n = s.new_edit.as_ref().expect("没进新建态");
        assert_eq!(n.buf, "", "新建的输入框该是空的");
        assert!(n.focus_pending, "没要焦点 —— 用户得先拿鼠标点一下才能打字");
    }

    /// F219:两个就地输入框**不能同时活着** —— egui 里两个 `TextEdit` 会互抢
    /// 键盘焦点,先进编辑态那个永远 `lost_focus()` 不了、退不出来(F131 实测)。
    ///
    /// 自证会变红:把 `begin_new_file` 里清 `rename_edit` 的那句删掉。
    #[test]
    fn starting_a_new_file_cancels_an_in_flight_rename_and_the_other_way_round() {
        let mut s = ready_state();
        assert!(s.begin_rename(), "前提:进得了改名态");
        assert!(s.begin_new_file());
        assert!(s.rename_edit.is_none(), "改名态还赖着 —— 两个输入框会互抢焦点");

        let mut s = ready_state();
        assert!(s.begin_new_file());
        assert!(s.begin_rename(), "前提:进得了改名态");
        assert!(s.new_edit.is_none(), "新建态还赖着 —— 同上");
    }

    /// F219:换目录 / 换机器之后,那个输入框回车拼出来的是**另一个目录**里的
    /// 路径 —— 必须清掉。
    ///
    /// 自证会变红:把 `begin_load`/`invalidate` 里清 `new_edit` 的那句删掉。
    #[test]
    fn leaving_the_directory_drops_the_new_file_edit() {
        for leave in [0u8, 1] {
            let mut s = ready_state();
            assert!(s.begin_new_file());
            if leave == 0 {
                s.begin_load(RemotePath::from_bytes(b"/tmp".to_vec()));
            } else {
                s.invalidate();
            }
            assert!(s.new_edit.is_none(), "换目录/换机器后新建态还赖着");
        }
    }

    /// F219:**刷新不清新建态** —— 它不绑任何已有行,清掉会把用户正在打的字
    /// 吞掉(与 `rename_edit` 的处置故意不同:那个绑着一条具体的行)。
    ///
    /// 自证会变红:在 `accept` 里加一句 `self.new_edit = None;`。
    #[test]
    fn a_refresh_keeps_the_new_file_edit_because_it_is_not_tied_to_any_row() {
        let mut s = ready_state();
        assert!(s.begin_new_file());
        s.new_edit.as_mut().unwrap().buf = "half-typed".into();
        let seq = s.request_seq;
        assert!(s.accept(seq, Ok(sample_entries())));
        assert_eq!(
            s.new_edit.as_ref().map(|n| n.buf.as_str()),
            Some("half-typed"),
            "刷新把用户正在打的字吞了"
        );
    }
```

`ready_state()` / `sample_entries()`:该 `mod tests` 里已有等价助手(`begin_rename` 那几条测试用的就是它们)。先读:

Run: `sed -n '/#\[cfg(test)\]/,/fn the_rename_editor/p' crates/mullion-app/src/files/state.rs | head -60`

用现成的名字,不要新造。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib files::state 2>&1 | tail -20`
Expected: 编译失败,`no method named 'begin_new_file'` / `no field 'new_edit'`。

- [ ] **Step 3: 写实现**

在 `crates/mullion-app/src/files/state.rs`:

(a) `RenameEdit` 之后加类型:

```rust
/// F219:一次就地**新建**的编辑态。
///
/// 与 `RenameEdit` 分开而不是加个 `kind` 字段:改名绑着一条**已存在的行**
/// (`from` 是它的原名,`accept` 里那一行没了就得清掉),新建谁都不绑 ——
/// 塞进同一个结构体的话,`from` 对新建就是个必须编出来的假值,而编译器
/// 对这种「一半字段没意义」的类型一声不吭。
#[derive(Debug, Clone, PartialEq)]
pub struct NewEdit {
    /// 输入框内容。进来时是空的。
    pub buf: String,
    /// 刚进新建态、**还没把键盘焦点要过来**。渲染那侧要一次就清掉
    /// (每帧无条件 `request_focus()` 会让两栏互抢,见 `RenameEdit` 的文档)。
    pub focus_pending: bool,
}
```

(b) `PaneState` 加字段(挨着 `rename_edit`):

```rust
    /// F219:就地新建文件的输入缓冲。`None` = 没在新建(默认)。
    ///
    /// **与 `rename_edit` 互斥**:两个 `TextEdit` 同时活着会互抢键盘焦点,
    /// 先进编辑态那个永远退不出来(F131 实测过的同款事故)。互斥在
    /// `begin_new_file` / `begin_rename` 两个入口各清一次对方。
    pub new_edit: Option<NewEdit>,
```

`PaneState::new` 里补 `new_edit: None,`。

(c) `begin_load` 里,紧跟着那句 `self.rename_edit = None;` 之后:

```rust
        // F219:同上 —— 那个输入框回车拼出来的路径用的是**新** cwd,
        // 建出来的文件会落在用户根本没在看的目录里。
        self.new_edit = None;
```

(d) `invalidate` 里,紧跟着那句 `self.rename_edit = None;` 之后加同样一句(注释写「换的还是另一台机器,更不能留」)。

**注意:`accept` 里什么都不加** —— 见上面那条测试。

(e) `begin_rename` 函数体第一句(在取 `cursor` 之前)加:

```rust
        // F219:两个就地输入框互斥,见 `PaneState::new_edit` 的文档。
        self.new_edit = None;
```

(f) `begin_rename` 之后加新方法:

```rust
    /// F219:开始在**当前目录**里就地新建一个文件。返回是否真的进了新建态。
    ///
    /// 不像 `begin_rename` 那样需要光标行 —— 新建不针对任何一条已有的行,
    /// 空目录里同样成立(那正是用户最需要它的时候)。
    pub fn begin_new_file(&mut self) -> bool {
        // 互斥,见 `new_edit` 的文档。
        self.rename_edit = None;
        self.new_edit = Some(NewEdit {
            buf: String::new(),
            focus_pending: true,
        });
        true
    }
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib files::state 2>&1 | tail -20`
Expected: 全过。

- [ ] **Step 5: 变异验证**

逐条做上面四条测试注释里写的「自证会变红」,确认对应测试失败后改回。

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-app/src/files/state.rs
git commit -m "feat(app): 文件栏新建文件的编辑态,与改名互斥 (F219)"
```

---

## Task A3: 幽灵行的渲染与索引偏移

**Files:**
- Modify: `crates/mullion-app/src/ui/files_panel.rs`

- [ ] **Step 1: 写失败的测试**

先读现成的测试脚手架,复用它而不是新造:

Run: `sed -n '/fn rename_harness/,/^    }/p' crates/mullion-app/src/ui/files_panel.rs`

追加到 `mod tests`:

```rust
    /// F219:新建态下列表**第一行**是输入框 —— 看不见的输入框等于没有。
    ///
    /// 自证会变红:把 `show_rows` 的行数从 `rows.len() + new_row` 改回
    /// `rows.len()`。
    #[test]
    fn the_new_file_editor_occupies_the_first_row_of_the_list() {
        let (mut frame, mut cols, ctx) = rename_harness();
        assert!(frame.remote.begin_new_file(), "前提:进得了新建态");
        drive(&ctx, &mut frame, &mut cols);
        assert!(
            ctx.read_response(new_edit_id("远端")).is_some(),
            "新建态下没画出输入框"
        );
    }

    /// F219 最容易静默错行的一处:幽灵行占了第 0 行之后,**下面每一行的
    /// 索引都要 -1**。错了就是点第二行选中第一个文件 —— 而删除不可逆。
    ///
    /// 自证会变红:把行体里的 `rows[ix - new_row]` 改回 `rows[ix]`
    /// (或去掉那个偏移量)。
    #[test]
    fn rows_below_the_ghost_row_still_map_to_the_right_entry() {
        let (mut frame, mut cols, ctx) = rename_harness();
        let expected: Vec<String> = frame
            .remote
            .rows()
            .iter()
            .map(|e| e.name.display().to_string())
            .collect();
        assert!(expected.len() >= 2, "前提:样本目录至少两条");
        assert!(frame.remote.begin_new_file());
        drive(&ctx, &mut frame, &mut cols);
        // 画出来的名字序列(跳过第一行那个输入框)必须与 `rows()` 逐条对齐。
        let painted = painted_row_names(&ctx);
        assert_eq!(
            painted, expected,
            "幽灵行之后的行错位了 —— 点某一行会打到另一个文件上"
        );
    }

    /// F219:名字非法(空 / 含 `/` / `.` / `..`)时**不提交**,输入框留着。
    ///
    /// 自证会变红:去掉 `name_edit_row` 里的 `validate_name` 判断。
    #[test]
    fn an_invalid_new_name_is_not_submitted() {
        let (mut frame, mut cols, ctx) = rename_harness();
        assert!(frame.remote.begin_new_file());
        frame.remote.new_edit.as_mut().unwrap().buf = "a/b".into();
        let action = press_enter_and_drive(&ctx, &mut frame, &mut cols);
        assert!(action.is_none(), "非法名字被提交了 —— 那会打到另一个目录上");
        assert!(frame.remote.new_edit.is_some(), "非法名字不该退出编辑态");
    }

    /// F219:目录里已经有同名的 → 也不提交。这件事本地就知道,不必往返
    /// 一趟等服务端回 Failure。
    ///
    /// 自证会变红:去掉 `name_edit_row` 调用处传进去的 `taken` 判重。
    #[test]
    fn a_new_name_that_collides_with_an_existing_entry_is_not_submitted() {
        let (mut frame, mut cols, ctx) = rename_harness();
        let taken = frame.remote.rows()[0].name.display().to_string();
        assert!(frame.remote.begin_new_file());
        frame.remote.new_edit.as_mut().unwrap().buf = taken;
        let action = press_enter_and_drive(&ctx, &mut frame, &mut cols);
        assert!(action.is_none(), "重名被提交了 —— 服务端必然回一条 Failure");
        assert!(frame.remote.new_edit.is_some());
    }

    /// F219:合法名字回车 → 发出 `NewFile`,路径是**当前 cwd 拼出来的绝对
    /// 路径**(同 `Rename` 的理由:从开始输入到敲回车之间用户可能换过目录)。
    #[test]
    fn submitting_a_new_name_dispatches_new_file_with_an_absolute_path() {
        let (mut frame, mut cols, ctx) = rename_harness();
        assert!(frame.remote.begin_new_file());
        frame.remote.new_edit.as_mut().unwrap().buf = "notes.txt".into();
        let action = press_enter_and_drive(&ctx, &mut frame, &mut cols);
        let cwd = frame.remote.cwd.clone();
        assert_eq!(
            action,
            Some(FileAction::NewFile(cwd.join(b"notes.txt"))),
            "提交发出来的不是当前 cwd 下的绝对路径"
        );
        assert!(frame.remote.new_edit.is_none(), "提交后还留在编辑态");
    }
```

`drive` / `press_enter_and_drive` / `painted_row_names`:`rename_harness()` 附近已有等价的驱动助手(改名那几条测试用的那套)。**先读那几条测试**,用它们现成的驱动方式;缺 `painted_row_names` 就照着 `dialog_texts` / `rendered` 那种「从 `ctx` 的渲染输出里捞文字」的既有写法补一个私有助手,放在 `rename_harness` 旁边。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib ui::files_panel 2>&1 | tail -25`
Expected: 编译失败(`new_edit_id` / `FileAction::NewFile` 不存在)。

- [ ] **Step 3: 写实现**

(a) `rename_edit_id` 旁边加:

```rust
/// F219:新建输入框的 egui id。与 `rename_edit_id` 分开 —— 同 id 的两个
/// `TextEdit` 会共享 egui 的 `TextEditState`(选区/光标位置),互相把对方的
/// 选区改掉。
fn new_edit_id(id: &str) -> egui::Id {
    egui::Id::new(("files-new", id))
}
```

(b) 把 `rename_row` 的输入框部分抽成共用件。**新增**一个函数,`rename_row` 改为调用它:

```rust
/// F200/F219:就地编辑的那一行 —— 名称列换成输入框,其余列不画。
///
/// 两个用户共用一份(改名与新建):焦点走一次性 `focus_pending`、高度必须
/// 正好 `ROW_H`(矮 1pt 下面每行整体上移,而 `show_rows` 的虚拟滚动只管起始
/// 偏移、不检查行距,**编译/测试/日志全不吭声**)这两条前提两边都要,
/// 各写一遍必然漂移。
///
/// `extra_err`:调用方额外的校验(新建那条路上是「目录里已经有同名的」)。
/// `Some(理由)` 时与 `validate_name` 的错一样处理:画红框、悬停说原因、
/// 回车不提交也不退出。
///
/// 返回:`None` = 还在编辑;`Some(None)` = 放弃;`Some(Some(name))` = 提交。
fn name_edit_row(
    ui: &mut Ui,
    t: &Theme,
    edit_id: egui::Id,
    cols: &ColWidths,
    column: PanelColumn,
    buf: &mut String,
    focus_pending: &mut bool,
    preselect_stem: bool,
    extra_err: Option<&'static str>,
) -> Option<Option<String>> {
    // 行宽与 `row()` 同源(见那里的说明:总宽与视口宽取大者)。
    let w = content_w(cols, column).max(ui.available_width());
    let (row_rect, _) = ui.allocate_exact_size(egui::vec2(w, ROW_H), egui::Sense::hover());
    ui.painter()
        .rect_filled(row_rect, 2.0, theme::selection_fill(t));
    let err = crate::ui::files_dialog::validate_name(buf)
        .err()
        .or(extra_err);
    let name_rect = egui::Rect::from_min_size(
        row_rect.min + egui::vec2(name_start_x_offset(), 0.0),
        egui::vec2((cols.name - name_start_x_offset()).max(60.0), ROW_H),
    );
    let resp = ui.put(
        name_rect,
        egui::TextEdit::singleline(buf)
            .id(edit_id)
            .margin(egui::Margin::symmetric(2.0, 0.0)),
    );
    if *focus_pending {
        *focus_pending = false;
        if preselect_stem {
            select_all(ui.ctx(), edit_id, rename_stem_len(buf));
        }
        resp.request_focus();
    }
    if let Some(why) = err {
        ui.painter().rect_stroke(
            name_rect,
            egui::Rounding::same(2.0),
            egui::Stroke::new(1.0, theme::c32(t.danger)),
        );
        resp.clone().on_hover_text(why);
    }
    if resp.lost_focus() || resp.clicked_elsewhere() {
        if !ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            return Some(None);
        }
        if err.is_some() {
            resp.request_focus();
            return None;
        }
        return Some(Some(buf.trim().to_string()));
    }
    None
}
```

`rename_row` 的函数体整个换成一句转发(**文档注释保留原样**,它记着 F200 的那几条前提):

```rust
    name_edit_row(
        ui,
        t,
        rename_edit_id(id),
        cols,
        column,
        &mut r.buf,
        &mut r.focus_pending,
        true, // F200:预选中主干、留下扩展名
        None,
    )
```

(c) `show()` 里,与 `renaming` 并排把新建缓冲也挪出来(在 `let mut renaming = state.rename_edit.take();` 旁边):

```rust
    // F219:同 `renaming` —— 闭包里 `state` 只借得到不可变,输入框要 `&mut String`。
    let mut newing = state.new_edit.take();
    // `None` = 还在编辑;`Some(None)` = 放弃;`Some(Some(name))` = 提交。
    let mut new_done: Option<Option<String>> = None;
    // 幽灵行占不占第 0 行。**行数与索引偏移共用这一个值** —— 两处各写一遍
    // 必然有一处漏改,而漏改的症状是点某一行打到另一个文件上。
    let new_row = usize::from(newing.is_some());
```

(d) 目录里已有的名字集合(给判重用),在 `let rows = state.rows();` 之后:

```rust
    // F219:本地判重用。**按 `entries` 而不是 `rows`** —— 隐藏文件也占名字,
    // 关着隐藏开关时建一个 `.env` 照样会撞上服务端那条。
    let taken: std::collections::BTreeSet<Vec<u8>> = state
        .entries
        .iter()
        .map(|e| e.name.as_bytes().to_vec())
        .collect();
```

(e) `scroll_offset` 的换算里,行号加上幽灵行:

```rust
    let scroll_offset = scroll_to.and_then(|name| {
        let ix = rows.iter().position(|e| e.name == name)?;
        // F219:幽灵行占了第 0 行,目标行整体下移一格。
        Some(((ix + new_row) as f32 * ROW_H - body_h / 2.0).max(0.0))
    });
```

(f) 进新建态那一帧把视口钉到顶(挨着 F218 那段 `if let Some(y) = scroll_offset`):

```rust
    // F219:输入行在第一行 —— 用户此刻多半滚在目录中段,不钉的话他看不见
    // 自己刚打开的那个框。**只在进新建态那一帧钉**(`focus_pending` 恰好
    // 就是那一帧的标记),无条件钉的话新建态期间滚轮就废了。
    if newing.as_ref().is_some_and(|n| n.focus_pending) {
        area = area.vertical_scroll_offset(0.0);
    }
```

(g) `show_rows` 的行数与行体索引:

```rust
        .show_rows(ui, ROW_H, rows.len() + new_row, |ui, range| {
            ui.set_min_width(total_w);
            for ix in range {
                // F219:第 0 行是幽灵行(只在新建态存在),其余整体 -1。
                if new_row == 1 && ix == 0 {
                    let n = newing.as_mut().expect("new_row 为 1 说明它是 Some");
                    let dup = (!n.buf.trim().is_empty()
                        && taken.contains(n.buf.trim().as_bytes()))
                    .then_some("这个目录里已经有同名的了");
                    if let Some(done) = name_edit_row(
                        ui,
                        t,
                        new_edit_id(id),
                        cols,
                        column,
                        &mut n.buf,
                        &mut n.focus_pending,
                        false, // 空缓冲没有主干可预选
                        dup,
                    ) {
                        new_done = Some(done);
                    }
                    continue;
                }
                let e = rows[ix - new_row];
                // …以下原样不动…
```

(h) 收尾(挨着 `match rename_done` 那段,**放在它之前** —— 两个输入框互斥,同一帧最多一个有结果):

```rust
    // F219:新建的收尾。同改名,放在 `click_row` 之前 —— 提交那一下同时也是
    // 「点了别处」,先把动作定下来再让点击去改选中态。
    match new_done {
        None => state.new_edit = newing,
        Some(None) => {}
        Some(Some(name)) => {
            // **路径在这里拼**,用的是这一帧的 `state.cwd` —— 见
            // `FileAction::NewFile` 的文档。
            action = Some(FileAction::NewFile(state.cwd.join(name.as_bytes())));
        }
    }
```

(i) `FileAction` 加变体:

```rust
    /// F219:就地新建文件提交。**绝对路径**,在面板里用同一个 `cwd` 拼好 ——
    /// 理由与 `Rename` 逐字相同:从开始输入到敲回车中间用户完全可能换了
    /// 目录,app 侧再拿「当前 cwd」去拼就是在另一个目录里建文件。
    ///
    /// 名字已经过 `files_dialog::validate_name` 与「目录里没有同名」两道闸。
    NewFile(mullion_ssh::sftp::RemotePath),
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib ui::files_panel 2>&1 | tail -25`
Expected: 新增五条全过,既有改名那几条**一条都不能红**(共用件抽取不许改变改名的行为)。

- [ ] **Step 5: 变异验证**

三条注释里写了「自证会变红」的,逐条改坏确认失败后改回。特别是 `rows[ix - new_row]` → `rows[ix]` 那条。

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-app/src/ui/files_panel.rs
git commit -m "feat(app): 文件栏首行幽灵输入行,回车才发新建 (F219)"
```

---

## Task A4: 接线 —— 菜单项、两栏分派、`FileOp::NewFile`

**Files:**
- Modify: `crates/mullion-app/src/ui/files_panel.rs`(`menu_items_for`、`MenuItem`)
- Modify: `crates/mullion-app/src/ui/files_dialog.rs`(`FileOp`)
- Modify: `crates/mullion-app/src/app.rs`(两栏分派、`apply_file_op`)

- [ ] **Step 1: 写失败的测试**

`crates/mullion-app/src/ui/files_panel.rs` 的 `mod tests`:

```rust
    /// F219:「新建文件」只在远端栏出现(D5),且**不带省略号** ——
    /// 省略号在这套界面里的意思是「会弹框」(F200 定的),而它是就地编辑。
    #[test]
    fn the_new_file_item_is_remote_only_and_carries_no_ellipsis() {
        let labels: Vec<&str> = menu_items_for(PanelColumn::Remote, None)
            .iter()
            .map(|e| e.label)
            .collect();
        assert!(labels.contains(&"新建文件"), "远端栏没有「新建文件」");
        assert!(
            !labels.iter().any(|l| l.starts_with("新建文件…")),
            "「新建文件」带了省略号 —— 那是弹框的记号"
        );
        let local: Vec<&str> = menu_items_for(PanelColumn::Local, None)
            .iter()
            .map(|e| e.label)
            .collect();
        assert!(
            !local.contains(&"新建文件"),
            "本地栏出现了写操作(D5:本地文件管理外包给资源管理器)"
        );
    }
```

`crates/mullion-app/src/app.rs` 的 `mod tests`(源码切片守护,与该文件里既有那批同一手法):

```rust
    /// F219:远端栏收到 `NewFile` 要**直接发写操作**,不绕对话框。
    ///
    /// 自证会变红:把那一臂删掉(落进 `_ => {}` 之后它会掉进下面
    /// 那个 `target` 的 match,编译不过 —— 所以真正要防的是有人把它接进
    /// `Ask`,那样就静默多弹一个框)。
    #[test]
    fn the_remote_column_sends_new_file_straight_to_a_write_op() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn apply_remote_file_action")
            .nth(1)
            .expect("找不到 apply_remote_file_action");
        let body = &after[..after.find("\n    }\n").expect("找不到函数结尾")];
        let at = body
            .find("FileAction::NewFile")
            .expect("远端栏没接 NewFile —— 回车之后什么都不会发生");
        let arm = &body[at..at + 400.min(body.len() - at)];
        assert!(
            arm.contains("FileOp::NewFile"),
            "NewFile 没落到写操作上(接成弹框的话会多一个没人要的对话框)"
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib the_new_file_item 2>&1 | tail -10` → FAIL
Run: `cargo test -p mullion-app --lib the_remote_column_sends_new_file 2>&1 | tail -10` → FAIL

- [ ] **Step 3: 写实现**

(a) `files_panel.rs` 的 `MenuItem` 加一档 + `into_action` 一臂:

```rust
    /// F219:就地新建文件。**不是 `Ask`** —— 它不弹框,是让列表首行长出一个
    /// 输入框(同 F200 的改名)。
    NewFile,
```

```rust
            MenuItem::NewFile => FileAction::BeginNewFile,
```

(b) `FileAction` 再加一个「请求进入新建态」的变体(与提交用的 `NewFile` 分开 —— 一个是「打开输入框」,一个是「名字定了,发出去」):

```rust
    /// F219:请求进入就地新建态(右键菜单 / `Ctrl+N`)。真正的写操作要等
    /// 用户在那一行里敲完名字回车,由 `NewFile` 发出。
    BeginNewFile,
```

(c) `menu_items_for` 远端分支,「新建文件夹…」下面一行:

```rust
        out.push(on("新建文件", MenuItem::NewFile));
```

(d) `files_dialog.rs` 的 `FileOp` 加:

```rust
    /// F219:新建一个空文件。完整的目标路径(已经拼好,同 `NewDir`)。
    NewFile(RemotePath),
```

(e) `app.rs` `apply_file_op` 的 match 加一臂(挨着 `FileOp::NewDir`):

```rust
                FileOp::NewFile(p) => client.create_file(&p).await.map_err(|e| e.to_string()),
```

(f) `app.rs` `apply_remote_file_action` 开头那段 match 里,`FileAction::Rename` 那一臂之后:

```rust
            // F219:请求进入就地新建态。纯 UI 状态,不发任何请求 ——
            // 与 `Ask` 一样在借出 `files` 之前分流。
            FileAction::BeginNewFile => {
                if let Some(files) = self
                    .tabs
                    .by_generation_mut(generation)
                    .and_then(|t| t.content.files_panel_mut())
                {
                    files.remote.begin_new_file();
                    mark_ui_dirty!(self.ui_dirty);
                    self.request_ui_redraw();
                }
                return;
            }
            // F219:名字定了 —— 路径已经在面板里拼好、也过了两道校验闸
            // (见 `FileAction::NewFile` 的文档),这里只管发。
            FileAction::NewFile(path) => {
                let op = crate::ui::files_dialog::FileOp::NewFile(path.clone());
                self.apply_file_op(generation, op);
                return;
            }
```

(g) `apply_remote_file_action` 下半段那个 `target` 的穷尽 match 里,把两个新变体加进「上面已经分流走了」那一组:

```rust
            | FileAction::Rename { .. }
            | FileAction::BeginNewFile
            | FileAction::NewFile(_) => return,
```

(h) `apply_local_file_action` 的穷尽 match 里加一臂(D5:本地栏没有写操作):

```rust
            // F219:同 `Rename` —— 本地栏根本进不了新建态(`menu_items_for`
            // 不给这一项,`handle_panel_key` 的 Ctrl+N 也只在远端栏放行)。
            FileAction::BeginNewFile | FileAction::NewFile(_) => {
                log::warn!("本地栏收到了新建文件请求,已忽略(D5)");
                return;
            }
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED"`
Expected: 全过。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/ui/files_panel.rs crates/mullion-app/src/ui/files_dialog.rs crates/mullion-app/src/app.rs
git commit -m "feat(app): 新建文件接进右键菜单与远端写操作 (F219)"
```

---

## Task A5: `Ctrl+N` 与 `Modal::FilesNewName`

**Files:**
- Modify: `crates/mullion-app/src/app.rs`

- [ ] **Step 1: 写失败的测试**

`app.rs` 的 `mod tests`:

```rust
    /// F219/T8:就地新建的输入框必须登记成模态 —— 不登记的话面板持有键盘
    /// 焦点时那个框**一个键都收不到**,而 Backspace 还会被 `handle_panel_key`
    /// 解释成「回上级目录」,一按就跳走。
    ///
    /// 自证会变红:把 `Modal::FilesNewName` 从 `Modal::ALL` 里删掉。
    #[test]
    fn the_new_file_editor_is_registered_as_a_modal_so_it_can_receive_keys() {
        assert!(
            Modal::ALL.contains(&Modal::FilesNewName),
            "FilesNewName 没登记进 Modal::ALL(T8)"
        );
        let src = include_str!("app.rs");
        let after = src
            .split("fn modal_open(&self)")
            .nth(1)
            .expect("找不到 modal_open");
        let body = &after[..after.find("\n    }\n").expect("找不到函数结尾")];
        assert!(
            body.contains("Modal::FilesNewName => self.files_new_file_editing()"),
            "modal_open 里没有 FilesNewName 独立的那一臂(T8)"
        );
    }

    /// F219:`Ctrl+N` 只在**远端栏**放行。焦点在本地栏时静默不动,不是转投
    /// 远端栏 —— 用户看着本地栏按键、结果动了远端,是这一片最坏的后果
    /// (与 Delete/F2 同一条判据)。
    ///
    /// 自证会变红:把 `handle_panel_key` 里 Ctrl+N 那段的栏判断删掉。
    #[test]
    fn ctrl_n_only_starts_a_new_file_on_the_remote_column() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn handle_panel_key(")
            .nth(1)
            .expect("找不到 handle_panel_key");
        let body = &after[..after.find("\n    }\n").expect("找不到函数结尾")];
        let at = body
            .find("\"n\"")
            .expect("Ctrl+N 没接上 —— 键盘那条入口不存在");
        let arm = &body[at..at + 400.min(body.len() - at)];
        assert!(
            arm.contains("PanelColumn::Remote"),
            "Ctrl+N 没判栏 —— 在本地栏按会动到远端(D5)"
        );
        assert!(
            arm.contains("BeginNewFile"),
            "Ctrl+N 没派发 BeginNewFile"
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib the_new_file_editor_is_registered 2>&1 | tail -10` → FAIL

- [ ] **Step 3: 写实现**

(a) `Modal` 枚举里,`FilesRename` 之后:

```rust
    /// F219:文件面板里有一行正在**就地新建**。理由与 `FilesRename` 逐字
    /// 相同 —— 那个输入框收不到任何键(T8),Backspace 还会被
    /// `handle_panel_key` 解释成「回上级目录」,一按就跳走。
    ///
    /// **不进 `touched_store`**:它一行 store 都不写(同 `FilesPathEdit`)。
    FilesNewName,
```

(b) `Modal::ALL` 里 `Modal::FilesRename,` 之后加 `Modal::FilesNewName,`。

(c) `modal_open` 的 match 里 `Modal::FilesRename => …` 之后:

```rust
            // F219:见 `Modal::FilesNewName` 的说明。
            Modal::FilesNewName => self.files_new_file_editing(),
```

(d) `files_renaming` 之后加方法,并在同文件里照 `files_renaming_of` 的写法加自由函数(**照抄它的判据来源**:`files_owner_generation_of` + 两栏,不要另起一套):

```rust
    /// F219:文件面板里有没有一行正在就地新建(`Modal::FilesNewName` 的判据)。
    fn files_new_file_editing(&self) -> bool {
        files_new_file_editing_of(&self.tabs, self.ui.files_sidebar_open)
    }
```

先读现成的那份再照写:

Run: `grep -n "fn files_renaming_of" -A 20 crates/mullion-app/src/app.rs`

(e) `handle_panel_key` 里 `Ctrl+H` 那段之内(它已经在 `if mods.control_key()` + `WinitKey::Character` 里了),给 `"n"` 加一支:

```rust
                // F219:Ctrl+N 就地新建文件。**只在远端栏**(D5)——
                // 焦点在本地栏时静默不动,不是转投远端栏。
                if s.as_str() == "n" {
                    let column = self
                        .tabs
                        .by_generation(generation)
                        .and_then(|t| t.content.files_panel())
                        .map(|f| f.active_column);
                    if column == Some(crate::ui::files_panel::PanelColumn::Remote) {
                        self.dispatch_panel_action(generation, FileAction::BeginNewFile);
                    }
                    return;
                }
```

(f) 若 `app.rs` 里有 Modal 完备性的对照表(F148 记的「要改两处」),把新档补进去 —— 先找:

Run: `grep -rn "Modal::FilesRename" crates/mullion-app/src crates/mullion-app/tests`

**每一处**都补上对应的 `FilesNewName`。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED"`
Expected: 全过(尤其那条 Modal 完备性测试)。

- [ ] **Step 5: 变异验证 + 提交**

两条注释里的「自证会变红」逐条做一遍。然后:

```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): Ctrl+N 新建文件并登记模态 (F219, T8)"
```

---

## Task A6: 建完之后光标落到新文件上

**Files:**
- Modify: `crates/mullion-app/src/app.rs`

- [ ] **Step 1: 写失败的测试**

```rust
    /// F219:建完之后光标要落到那个新文件上 —— 用户下一步多半是编辑它 /
    /// 改权限,让他在刷新后的列表里重新找一遍等于白建了一半。
    ///
    /// **顺序是死的**:`reveal_pick` 必须写在那次 `Refresh` **之后** ——
    /// `begin_load` 会 `clear_selection`,写在它之前会被自己清掉(F218 踩过)。
    ///
    /// 自证会变红:把落 `reveal_pick` 那句挪到 `Refresh` 之前。
    #[test]
    fn a_freshly_created_file_is_revealed_after_the_refresh_not_before() {
        let src = include_str!("app.rs");
        let after = src
            .split("UserEvent::SftpOpDone {")
            .nth(1)
            .expect("找不到 SftpOpDone 的处理分支");
        let body = &after[..after.find("UserEvent::ShotUploaded").expect("找不到下一个分支")];
        let refresh_at = body
            .find("FileAction::Refresh")
            .expect("成功之后没刷新目录");
        let reveal_at = body
            .find("reveal_pick")
            .expect("建完之后没有把光标落到新文件上");
        assert!(
            reveal_at > refresh_at,
            "reveal_pick 写在 Refresh 之前 —— begin_load 会把它连同选中一起清掉(F218)"
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib a_freshly_created_file 2>&1 | tail -10`
Expected: FAIL,「建完之后没有把光标落到新文件上」。

- [ ] **Step 3: 写实现**

(a) `UserEvent::SftpOpDone` 加一个「后续动作」字段:

```rust
/// F219/F220:一次远端写操作**成功之后**还要做的事。
///
/// 挂在完成事件上而不是在发出那一刻就做:非成功(链路断了 / 权限不足)
/// 意味着那件事压根没发生,提前做就是拿一个假前提改状态(T11 同族)。
#[derive(Debug, Clone, PartialEq)]
pub enum OpFollow {
    /// 没有后续(删除 / 改名 / 改权限)。
    None,
    /// F219:刷新之后把光标落到这一条上(**末段名字**,不含目录)。
    Reveal(mullion_ssh::sftp::RemotePath),
}
```

`SftpOpDone` 改成:

```rust
    SftpOpDone {
        generation: u64,
        result: Result<(), String>,
        /// 成功之后还要做的事,见 `OpFollow`。
        follow: OpFollow,
    },
```

(b) `apply_file_op` 里,在 spawn 之前按 op 算出 follow:

```rust
        // F219:建完之后把光标落到新文件上。**在这里算** —— 到了完成事件
        // 那一刻,`op` 已经被 move 进后台 task 了。
        let follow = match &op {
            crate::ui::files_dialog::FileOp::NewFile(p) => {
                OpFollow::Reveal(RemotePath::from_bytes(
                    p.as_bytes()
                        .rsplit(|b| *b == b'/')
                        .next()
                        .unwrap_or_default()
                        .to_vec(),
                ))
            }
            _ => OpFollow::None,
        };
```

(末段切法:若 `RemotePath` 已有等价方法就用它 —— 先 `grep -n "pub fn " crates/mullion-ssh/src/sftp.rs | head -20`,有 `file_name`/`last` 之类就别自己切。)

task 里 `send_event` 改成带 `follow`。

(c) 事件处理分支:

```rust
            UserEvent::SftpOpDone {
                generation,
                result,
                follow,
            } => {
                diag::count_sftp_op();
                match result {
                    Ok(()) => {
                        self.ui.set_toast(crate::ui::toast::Kind::Ok, "已完成");
                        self.dispatch_panel_action_for(
                            generation,
                            crate::ui::files_panel::PanelColumn::Remote,
                            crate::ui::files_panel::FileAction::Refresh,
                        );
                        // F219:**必须在 `Refresh` 之后** —— 那一步的
                        // `begin_load` 会 `clear_selection`,写在前面会被
                        // 自己清掉(F218 同款顺序陷阱)。
                        if let OpFollow::Reveal(name) = follow {
                            if let Some(files) = self
                                .tabs
                                .by_generation_mut(generation)
                                .and_then(|t| t.content.files_panel_mut())
                            {
                                files.remote.reveal_pick = Some(name);
                            }
                        }
                    }
                    Err(msg) => self.ui.set_error(msg),
                }
                self.request_ui_redraw();
            }
```

(d) 修其余 `SftpOpDone` 的构造点(至少 `app.rs:5323` 附近还有一处)——编译器会逐个报出来,一律补 `follow: OpFollow::None`。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED"`

- [ ] **Step 5: 变异验证 + 提交**

把 `reveal_pick` 那段挪到 `Refresh` 之前,确认测试变红,再挪回来。

```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): 新建成功后光标落到新文件上 (F219)"
```

---

## Task A7: 阶段 A 收口 —— spec 条目 + 全量绿

**Files:**
- Modify: `spec.md`

- [ ] **Step 1: 写 spec 条目**

在 `spec.md` 的功能表里 F218 之后加一行(格式照抄 F218 那行:`| 编号 | 描述 | 优先级 | 判据/坑 |`):

```
| F219 | **远端栏就地新建文件**:右键菜单「新建文件」(无省略号)或 `Ctrl+N`,列表**第一行**长出一个只存在于本地的输入行,回车才发一次带 `EXCLUDE` 的 create;Esc / 点别处放弃,远端一个字节都没动。名字过 `files_dialog::validate_name` + 「目录里没有同名」两道闸;建成后光标落到新文件上。远端栏专有(设计 D5) | P1 | **`EXCLUDE` 不能省**:不带的话在已有 `config.yaml` 的目录里手滑同名就把那份配置静默截断成 0 字节。**幽灵行的索引偏移**:行数 `rows.len() + new_row`、行体 `rows[ix - new_row]`,两处共用同一个值——错了就是点第二行打到第一个文件上,而删除不可逆。**与 `rename_edit` 互斥**:两个 `TextEdit` 同时活着会互抢键盘焦点,先进编辑态那个永远退不出来(F131 实测)。**刷新不清新建态**(它不绑任何已有行,清掉会吞掉用户正在打的字),但换目录/换机器必须清(回车会拼出另一个目录的路径)。**必须登记 `Modal::FilesNewName`**(五处一处不落),否则 T8:一个键都收不到、Backspace 还会跳去上级目录。`reveal_pick` 必须落在 `Refresh` **之后**(`begin_load` 会 `clear_selection`,F218 同款顺序陷阱) |
```

- [ ] **Step 2: 全量跑绿**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
cargo fmt --check
```

Expected: 无 FAILED、clippy 无输出、fmt 无输出。有 fmt 差异就跑 `cargo fmt` 并把格式化单独一次提交。

- [ ] **Step 3: 提交**

```bash
git add spec.md
git commit -m "docs: spec 补 F219(远端栏就地新建文件)"
```

---

# 阶段 B —— F220 远端复制 / 剪切 / 粘贴

## Task B1: `files/clip.rs` —— 全部纯逻辑

**Files:**
- Create: `crates/mullion-app/src/files/clip.rs`
- Modify: `crates/mullion-app/src/files/mod.rs`(加 `pub mod clip;`)

- [ ] **Step 1: 写失败的测试**

新建 `crates/mullion-app/src/files/clip.rs`,先只写文件头 + 测试:

```rust
//! F220:远端内复制/剪切/粘贴的**纯逻辑**。零 egui / 零 tokio / 零 IO ——
//! 「粘到自己里面去了没」「同名怎么改」「跳过之后还剩什么」全是判据类
//! 错误,得能在没有网络的情况下复现。
//!
//! 协议动作在 `mullion_ssh::copy_tree`,编排在 `app.rs`。

use mullion_ssh::sftp::RemotePath;

#[cfg(test)]
mod tests {
    use super::*;

    fn rp(s: &str) -> RemotePath {
        RemotePath::from_bytes(s.as_bytes().to_vec())
    }

    /// 唯一名:扩展名留在最后,别变成 `a.txt (副本)`(双击就打不开了)。
    #[test]
    fn a_duplicate_keeps_its_extension_at_the_end() {
        let taken = [b"a.txt".to_vec()].into_iter().collect();
        assert_eq!(
            unique_name(b"a.txt", false, &taken),
            b"a (\xe5\x89\xaf\xe6\x9c\xac).txt".to_vec(),
            "应为 `a (副本).txt`"
        );
    }

    /// 第二次撞 → 带序号。
    #[test]
    fn a_second_duplicate_gets_a_number() {
        let taken = [b"a.txt".to_vec(), "a (副本).txt".as_bytes().to_vec()]
            .into_iter()
            .collect();
        assert_eq!(
            unique_name(b"a.txt", false, &taken),
            "a (副本 2).txt".as_bytes().to_vec()
        );
    }

    /// 目录不切扩展名:`v1.2` 是目录名的一部分,不是后缀。
    #[test]
    fn a_directory_is_not_split_at_its_dot() {
        let taken = [b"v1.2".to_vec()].into_iter().collect();
        assert_eq!(
            unique_name(b"v1.2", true, &taken),
            "v1.2 (副本)".as_bytes().to_vec()
        );
    }

    /// 点开头的名字(`.env`)整个是名字,没有主干可切。
    #[test]
    fn a_dotfile_has_no_stem_to_split() {
        let taken = [b".env".to_vec()].into_iter().collect();
        assert_eq!(
            unique_name(b".env", false, &taken),
            ".env (副本)".as_bytes().to_vec()
        );
    }

    /// **本片最重要的一条闸门**:目标是源自身或源的子孙 → 整批拒绝。
    ///
    /// 远端 `cp` 自己会拦,但 SFTP 回退是我们**自己写的递归** ——
    /// 边列源边往源的子孙里写,会一直递归到把磁盘写满。
    ///
    /// 自证会变红:把 `is_within` 改成恒 `false`。
    #[test]
    fn pasting_into_yourself_or_your_own_descendant_is_refused() {
        assert!(is_within(&rp("/a/b"), &rp("/a/b")), "目标就是源自己");
        assert!(is_within(&rp("/a/b"), &rp("/a/b/c")), "目标在源里面");
        assert!(
            is_within(&rp("/a/b"), &rp("/a/b/c/d")),
            "目标在源的更深处"
        );
        assert!(!is_within(&rp("/a/b"), &rp("/a/bb")), "`/a/bb` 不是 `/a/b` 的子孙");
        assert!(!is_within(&rp("/a/b"), &rp("/a")), "父目录不是子孙");
        assert!(is_within(&rp("/"), &rp("/anything")), "根是一切的祖先");
    }

    /// 覆盖:每一条都落到 `dst/原名`,一条不少。
    #[test]
    fn overwriting_maps_every_item_onto_its_own_name() {
        let items = vec![(rp("/src/a.txt"), false), (rp("/src/b.txt"), false)];
        let existing = [b"a.txt".to_vec()].into_iter().collect();
        let plan = plan_paste(&items, &rp("/dst"), Policy::Overwrite, &existing);
        assert_eq!(
            plan.pairs,
            vec![
                (rp("/src/a.txt"), rp("/dst/a.txt")),
                (rp("/src/b.txt"), rp("/dst/b.txt")),
            ]
        );
        assert_eq!(plan.skipped, 0);
    }

    /// 跳过同名:**客户端把冲突项滤掉**,不靠 `cp -n`(coreutils 9.2 反转过
    /// 跳过时的退出码,会被 `succeeded()` 判成失败)。
    ///
    /// 自证会变红:把 `Policy::Skip` 那一支改成不过滤。
    #[test]
    fn skipping_drops_the_colliding_items_client_side() {
        let items = vec![(rp("/src/a.txt"), false), (rp("/src/b.txt"), false)];
        let existing = [b"a.txt".to_vec()].into_iter().collect();
        let plan = plan_paste(&items, &rp("/dst"), Policy::Skip, &existing);
        assert_eq!(plan.pairs, vec![(rp("/src/b.txt"), rp("/dst/b.txt"))]);
        assert_eq!(plan.skipped, 1);
    }

    /// 保留两者:撞名的改名,没撞的原样。
    #[test]
    fn keeping_both_renames_only_the_colliding_ones() {
        let items = vec![(rp("/src/a.txt"), false), (rp("/src/b.txt"), false)];
        let existing = [b"a.txt".to_vec()].into_iter().collect();
        let plan = plan_paste(&items, &rp("/dst"), Policy::KeepBoth, &existing);
        assert_eq!(
            plan.pairs,
            vec![
                (rp("/src/a.txt"), rp("/dst/a (副本).txt")),
                (rp("/src/b.txt"), rp("/dst/b.txt")),
            ]
        );
    }

    /// 同一批里两条都要改名时,**第二条要避开第一条刚占掉的名字** ——
    /// 不然两条都叫 `a (副本).txt`,后一条把前一条盖掉,而用户选的是「保留两者」。
    ///
    /// 自证会变红:`plan_paste` 里不把新名字加进 `taken` 就下一轮。
    #[test]
    fn two_renamed_items_in_one_batch_do_not_collide_with_each_other() {
        let items = vec![(rp("/x/a.txt"), false), (rp("/y/a.txt"), false)];
        let existing = [b"a.txt".to_vec()].into_iter().collect();
        let plan = plan_paste(&items, &rp("/dst"), Policy::KeepBoth, &existing);
        assert_eq!(
            plan.pairs,
            vec![
                (rp("/x/a.txt"), rp("/dst/a (副本).txt")),
                (rp("/y/a.txt"), rp("/dst/a (副本 2).txt")),
            ],
            "同一批里两条改名撞在一起了 —— 后一条会盖掉前一条"
        );
    }

    /// 冲突集合:只按末段名字比。
    #[test]
    fn conflicts_are_compared_by_the_last_path_segment_only() {
        let items = vec![(rp("/src/a.txt"), false), (rp("/src/b.txt"), false)];
        let existing = [b"b.txt".to_vec()].into_iter().collect();
        assert_eq!(conflicts(&items, &existing), vec![b"b.txt".to_vec()]);
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

`crates/mullion-app/src/files/mod.rs` 加 `pub mod clip;`(与既有 `pub mod drag;` 那组并列,按字母序)。

Run: `cargo test -p mullion-app --lib files::clip 2>&1 | tail -20`
Expected: 编译失败(函数都不存在)。

- [ ] **Step 3: 写实现**

在 `clip.rs` 的 `mod tests` 之前插入:

```rust
/// 剪贴板里装的是复制还是剪切。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipMode {
    Copy,
    Cut,
}

/// F220:一个标签的远端剪贴板。**per-tab**(设计 B4)——里面的路径永远
/// 属于当前这条连接,不会指到另一台机器上不存在的路径去。
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteClip {
    pub mode: ClipMode,
    /// **绝对路径** + 是不是目录。复制那一刻就拼好 —— 之后用户换目录、
    /// 换排序都不影响它指向谁。
    pub items: Vec<(RemotePath, bool)>,
}

/// 同名时怎么办。**没有「静默覆盖」这一档** —— 覆盖必须是用户在框里
/// 明确选的(设计 D17 的精神)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    Overwrite,
    Skip,
    KeepBoth,
}

/// 一次粘贴要干的活。
#[derive(Debug, Clone, PartialEq)]
pub struct PastePlan {
    /// (源绝对路径, 目标绝对路径)。**目标是全路径不是目录** ——
    /// 「保留两者」时目标末段与源末段不同,只给目录的话那个新名字就丢了。
    pub pairs: Vec<(RemotePath, RemotePath)>,
    /// 按 `Skip` 滤掉了几条。用户要知道「5 条里跳过了 2 条」。
    pub skipped: usize,
}

/// 路径的末段(不含目录)。空路径 / 结尾是 `/` 时给空切片。
fn last_segment(p: &RemotePath) -> Vec<u8> {
    p.as_bytes()
        .rsplit(|b| *b == b'/')
        .next()
        .unwrap_or_default()
        .to_vec()
}

/// `dst` 是不是 `src` 自己或它的子孙。
///
/// **判据是字节前缀 + `/` 边界**:光比前缀的话 `/a/bb` 会被判成 `/a/b`
/// 的子孙,而那是两个毫不相干的目录 —— 用户会看到「不能粘到这里」却
/// 找不出原因。根目录(`/`)是一切的祖先,单独处理。
pub fn is_within(src: &RemotePath, dst: &RemotePath) -> bool {
    let (s, d) = (src.as_bytes(), dst.as_bytes());
    if s == d {
        return true;
    }
    if s == b"/" {
        return true;
    }
    d.len() > s.len() && d.starts_with(s) && d[s.len()] == b'/'
}

/// 目标目录里已经有的、与这批条目撞名的**末段名字**(按传入顺序)。
pub fn conflicts(
    items: &[(RemotePath, bool)],
    existing: &std::collections::BTreeSet<Vec<u8>>,
) -> Vec<Vec<u8>> {
    items
        .iter()
        .map(|(p, _)| last_segment(p))
        .filter(|n| existing.contains(n))
        .collect()
}

/// 避开 `taken` 的一个新名字:`a.txt` → `a (副本).txt` → `a (副本 2).txt`。
///
/// `is_dir` 为真时**不切扩展名** —— `v1.2` 是目录名的一部分,不是后缀。
/// 文件按**最后一个** `.` 切,且那个点不在开头(`.env` 整个是名字)。
pub fn unique_name(
    name: &[u8],
    is_dir: bool,
    taken: &std::collections::BTreeSet<Vec<u8>>,
) -> Vec<u8> {
    let (stem, ext): (&[u8], &[u8]) = if is_dir {
        (name, b"")
    } else {
        match name.iter().rposition(|b| *b == b'.') {
            Some(i) if i > 0 => (&name[..i], &name[i..]),
            _ => (name, b""),
        }
    };
    for n in 1..10_000u32 {
        let mut cand = stem.to_vec();
        cand.extend_from_slice(" (副本".as_bytes());
        if n > 1 {
            cand.extend_from_slice(format!(" {n}").as_bytes());
        }
        cand.extend_from_slice(")".as_bytes());
        cand.extend_from_slice(ext);
        if !taken.contains(&cand) {
            return cand;
        }
    }
    // 一万个同名副本 —— 到这一步只可能是调用方传了个恒真的 `taken`。
    name.to_vec()
}

/// 一次粘贴的计划。`existing` = 目标目录里现有的名字(预检查列回来的)。
pub fn plan_paste(
    items: &[(RemotePath, bool)],
    dst_dir: &RemotePath,
    policy: Policy,
    existing: &std::collections::BTreeSet<Vec<u8>>,
) -> PastePlan {
    // 同一批里改出来的新名字也要占位 —— 不占的话两条都叫 `a (副本).txt`,
    // 后一条把前一条盖掉,而用户选的正是「保留两者」。
    let mut taken = existing.clone();
    let mut pairs = Vec::new();
    let mut skipped = 0usize;
    for (src, is_dir) in items {
        let name = last_segment(src);
        let hit = taken.contains(&name);
        let target = match (policy, hit) {
            (Policy::Skip, true) => {
                skipped += 1;
                continue;
            }
            (Policy::KeepBoth, true) => unique_name(&name, *is_dir, &taken),
            _ => name,
        };
        taken.insert(target.clone());
        pairs.push((src.clone(), dst_dir.join(&target)));
    }
    PastePlan { pairs, skipped }
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib files::clip 2>&1 | tail -20`
Expected: 10 passed。

- [ ] **Step 5: 变异验证**

三条写了「自证会变红」的逐条做:`is_within` 恒 `false`;`Policy::Skip` 不过滤;`plan_paste` 里不把新名字插进 `taken`。

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-app/src/files/clip.rs crates/mullion-app/src/files/mod.rs
git commit -m "feat(app): 远端粘贴的纯逻辑(唯一名/冲突/自身闸门/计划) (F220)"
```

---

## Task B2: 假 SFTP 服务端认 `cp -a` 与 `mv`

**Files:**
- Modify: `crates/mullion-ssh/tests/common/mod.rs`

- [ ] **Step 1: 把 `parse_rm_rf` 泛化**

现有 `parse_rm_rf` 里那段「解单引号字面量」的逻辑与前缀绑死了。拆成两个:

```rust
/// 解一串 `'a' 'b' 'c'` 形式的单引号参数。认不出来返回 `None` ——
/// 在测试里就是「命令没跑成」,正好扎住「转义漏了导致命令行结构不对」。
#[allow(dead_code)]
fn parse_quoted_args(mut rest: &[u8]) -> Option<Vec<Vec<u8>>> {
    // …把 `parse_rm_rf` 里 `while !rest.is_empty()` 那整段原样搬过来…
}

#[allow(dead_code)]
fn parse_rm_rf(cmd: &[u8]) -> Option<Vec<Vec<u8>>> {
    let prefix = b"rm -rf -- ";
    parse_quoted_args(cmd.strip_prefix(prefix)?)
}
```

- [ ] **Step 2: 加 cp/mv 的解析与执行**

```rust
/// F220:认 `cp -a[f] -- '<src>' '<dst>'`(单对,多对用 ` && ` 串),
/// 以及同形状的 `mv`。返回 (是不是移动, 一串 (src, dst))。
///
/// 起一个真 shell 来解析既不可能也没必要 —— 要验的是「转义对不对 +
/// 回退判定对不对」,不是 shell 的实现(同 `parse_rm_rf` 的理由)。
#[allow(dead_code)]
fn parse_copy_or_move(cmd: &[u8]) -> Option<(bool, Vec<(Vec<u8>, Vec<u8>)>)> {
    let mut is_move = None;
    let mut out = Vec::new();
    // ` && ` 分段。**按字节找**,路径里可能有奇怪字符,但 `shell_quote`
    // 保证它们都在单引号里,不会构造出假的 ` && `。
    for seg in split_on(cmd, b" && ") {
        let (mv, rest) = if let Some(r) = strip_any(&seg, &[b"mv -f -- ", b"mv -- "]) {
            (true, r)
        } else if let Some(r) = strip_any(&seg, &[b"cp -af -- ", b"cp -a -- "]) {
            (false, r)
        } else {
            return None;
        };
        if *is_move.get_or_insert(mv) != mv {
            return None; // 一条命令里混着 cp 和 mv —— 实现出错了
        }
        let args = parse_quoted_args(rest)?;
        if args.len() != 2 {
            return None;
        }
        out.push((args[0].clone(), args[1].clone()));
    }
    Some((is_move?, out))
}
```

`split_on` / `strip_any` 两个字节小工具就写在旁边(各三五行,`#[allow(dead_code)]`)。

内存树上的执行:

```rust
/// 在内存树里把一条(文件、链接或整棵目录树)拷到新路径。
/// **不跟随符号链接**:链接节点原样复制(连同它的目标字符串)。
#[allow(dead_code)]
fn copy_recursively(tree: &mut sftp_server::Tree, from: &[u8], to: &[u8]) {
    // 1. 取源节点(在它父目录的 Vec 里找)。找不到就什么都不做。
    // 2. 克隆一份,名字换成 `to` 的末段,插进 `to` 的父目录。
    // 3. 源是目录的话:`tree.insert(to.to_vec(), vec![])`,再对每个孩子
    //    递归 `copy_recursively(child_from, child_to)`。
    // 具体写法照 `remove_recursively` 的结构镜像过来(它是同一套树操作
    // 的反向),并用 `sftp_server::split_last_pub` 切父目录/名字 ——
    // 自己再写一遍切法就会两边不一致。
}
```

`exec_request` 的 match 扩成:

```rust
        let code = match parse_rm_rf(data) {
            Some(paths) => {
                let mut tree = self.tree.lock().unwrap();
                for p in paths {
                    remove_recursively(&mut tree, &p);
                }
                0
            }
            None => match parse_copy_or_move(data) {
                Some((is_move, pairs)) => {
                    let mut tree = self.tree.lock().unwrap();
                    for (from, to) in pairs {
                        copy_recursively(&mut tree, &from, &to);
                        if is_move {
                            remove_recursively(&mut tree, &from);
                        }
                    }
                    0
                }
                // 认不出来的命令 —— 与真 shell 的 "command not found" 同码。
                None => 127,
            },
        };
```

- [ ] **Step 3: 编译通过**

Run: `cargo test -p mullion-ssh --test sftp_write 2>&1 | grep -E "test result|error"`
Expected: 既有测试全过(脚手架改动不该动到任何现有行为)。

- [ ] **Step 4: 提交**

```bash
git add crates/mullion-ssh/tests/common/mod.rs
git commit -m "test(ssh): 假服务端认 cp -a/mv,为 F220 的 exec 快路径备好判据"
```

---

## Task B3: `mullion-ssh/src/copy_tree.rs`

**Files:**
- Create: `crates/mullion-ssh/src/copy_tree.rs`
- Modify: `crates/mullion-ssh/src/lib.rs`
- Test: `crates/mullion-ssh/tests/sftp_write.rs`

- [ ] **Step 1: 写失败的测试**

追加到 `sftp_write.rs`(该文件里已有 `nested_tree()`,复用它):

```rust
/// F220 快路径:exec 可用时走一条 `cp -a`,**一条 SFTP 写请求都不发**。
#[tokio::test]
async fn a_paste_uses_the_exec_fast_path_when_it_is_allowed() {
    let (addr, probe, tree_h) = common::spawn_sftp_server(nested_tree()).await;
    let (conn, sftp) = (conn_of(addr).await, client(addr).await);
    let pairs = vec![(rp("/home/testuser/box"), rp("/home/testuser/box-copy"))];
    let report = mullion_ssh::copy_tree::transfer_into(
        &sftp,
        &conn,
        &pairs,
        mullion_ssh::copy_tree::CopyMode::Copy,
        false,
    )
    .await
    .expect("复制该成功");
    assert_eq!(report, mullion_ssh::copy_tree::TransferReport::Exec);

    let t = tree_h.lock().unwrap();
    assert!(exists(&t, b"/home/testuser/box-copy"), "目标没建出来");
    assert!(exists(&t, b"/home/testuser/box"), "复制不该动源");
    let p = probe.lock().unwrap();
    assert!(
        p.paths_for("write").is_empty() && p.paths_for("mkdir").is_empty(),
        "走了 exec 就不该再发逐文件的 SFTP 写请求:{:?}",
        p.seen
    );
}

/// F220 的核心:**exec 被拒时回退到 SFTP 逐文件递归**,而不是报错收场
/// (sftp-only 账号上功能不能残缺)。
#[tokio::test]
async fn a_paste_falls_back_to_sftp_when_exec_is_refused() {
    let (addr, _probe, tree_h) = common::spawn_sftp_server_without_exec(nested_tree()).await;
    let (conn, sftp) = (conn_of(addr).await, client(addr).await);
    let pairs = vec![(rp("/home/testuser/box"), rp("/home/testuser/box-copy"))];
    let report = mullion_ssh::copy_tree::transfer_into(
        &sftp,
        &conn,
        &pairs,
        mullion_ssh::copy_tree::CopyMode::Copy,
        false,
    )
    .await
    .expect("回退该成功");
    assert_eq!(report, mullion_ssh::copy_tree::TransferReport::Sftp);
    let t = tree_h.lock().unwrap();
    assert!(exists(&t, b"/home/testuser/box-copy"), "回退没把树建出来");
}

/// F220:文件内容要真的一样。只看「路径存在」是恒绿的 —— 一个建空文件的
/// 实现照样通过。
#[tokio::test]
async fn the_sftp_fallback_copies_the_bytes_not_just_the_names() {
    let (addr, _probe, tree_h) = common::spawn_sftp_server_without_exec(nested_tree()).await;
    let (conn, sftp) = (conn_of(addr).await, client(addr).await);
    // nested_tree() 里挑一个**有内容**的文件(先 grep 那个函数确认路径)。
    let src = rp("/home/testuser/box/inner/deep.txt");
    let dst = rp("/home/testuser/deep-copy.txt");
    let before = {
        let t = tree_h.lock().unwrap();
        let (dir, name) = common::sftp_server::split_last_pub(src.as_bytes());
        t.get(&dir)
            .expect("源目录该在")
            .iter()
            .find(|n| n.name == name)
            .expect("源文件该在")
            .data
            .clone()
    };
    assert!(!before.is_empty(), "前提:源文件有内容");
    mullion_ssh::copy_tree::transfer_into(
        &sftp,
        &conn,
        &[(src, dst.clone())],
        mullion_ssh::copy_tree::CopyMode::Copy,
        false,
    )
    .await
    .expect("复制该成功");
    let t = tree_h.lock().unwrap();
    let (dir, name) = common::sftp_server::split_last_pub(dst.as_bytes());
    let after = t
        .get(&dir)
        .expect("目标目录该在")
        .iter()
        .find(|n| n.name == name)
        .expect("目标文件该在")
        .data
        .clone();
    assert_eq!(after, before, "拷过去的字节不一样");
}

/// F220:**绝不跟随符号链接** —— 跟进去等于把链接指向的整个目录复制一遍。
/// 两条路都要验(同 F57 的那条守护)。
#[tokio::test]
async fn a_paste_never_follows_a_symlink_on_either_path() {
    for without_exec in [true, false] {
        let (addr, _probe, tree_h) = if without_exec {
            common::spawn_sftp_server_without_exec(nested_tree()).await
        } else {
            common::spawn_sftp_server(nested_tree()).await
        };
        let (conn, sftp) = (conn_of(addr).await, client(addr).await);
        // nested_tree() 里那条指向目录的链接(名字见该函数)。
        let link = rp("/home/testuser/box/link");
        mullion_ssh::copy_tree::transfer_into(
            &sftp,
            &conn,
            &[(link, rp("/home/testuser/link-copy"))],
            mullion_ssh::copy_tree::CopyMode::Copy,
            false,
        )
        .await
        .expect("复制链接本身该成功");
        let t = tree_h.lock().unwrap();
        assert!(
            !t.contains_key(&b"/home/testuser/link-copy".to_vec()),
            "链接被当目录跟进去了(without_exec={without_exec}) —— 整个目标目录被复制了一遍"
        );
    }
}

/// F220:剪切 = 移动。源没了、目标有了。
#[tokio::test]
async fn a_cut_paste_moves_the_entry_instead_of_copying_it() {
    for without_exec in [true, false] {
        let (addr, _probe, tree_h) = if without_exec {
            common::spawn_sftp_server_without_exec(nested_tree()).await
        } else {
            common::spawn_sftp_server(nested_tree()).await
        };
        let (conn, sftp) = (conn_of(addr).await, client(addr).await);
        mullion_ssh::copy_tree::transfer_into(
            &sftp,
            &conn,
            &[(rp("/home/testuser/box"), rp("/home/testuser/moved"))],
            mullion_ssh::copy_tree::CopyMode::Move,
            false,
        )
        .await
        .expect("移动该成功");
        let t = tree_h.lock().unwrap();
        assert!(exists(&t, b"/home/testuser/moved"), "目标没出现");
        assert!(
            !exists(&t, b"/home/testuser/box"),
            "源还在(without_exec={without_exec}) —— 剪切变成了复制"
        );
    }
}

/// F220:脏名字(空格 / 单引号 / `$`)必须原样打到那条路径上 ——
/// 引号漏一个就是远端任意命令执行。
#[tokio::test]
async fn the_paste_fast_path_quotes_nasty_names_correctly() {
    let mut t0 = nested_tree();
    // 先在树里放一个脏名字的文件(照 `the_exec_fast_path_quotes_nasty_names_correctly`
    // 那条现成测试的构造方式来 —— 先读它)。
    let key = b"/home/testuser/it's a $(x) file".to_vec();
    t0.entry(b"/home/testuser".to_vec())
        .or_default()
        .push(common::sftp_server::Node::file(
            b"it's a $(x) file",
            b"hi".to_vec(),
        ));
    let (addr, _probe, tree_h) = common::spawn_sftp_server(t0).await;
    let (conn, sftp) = (conn_of(addr).await, client(addr).await);
    mullion_ssh::copy_tree::transfer_into(
        &sftp,
        &conn,
        &[(
            mullion_ssh::sftp::RemotePath::from_bytes(key),
            rp("/home/testuser/copied"),
        )],
        mullion_ssh::copy_tree::CopyMode::Copy,
        false,
    )
    .await
    .expect("脏名字也该复制成功");
    let t = tree_h.lock().unwrap();
    assert!(exists(&t, b"/home/testuser/copied"), "脏名字的引号处理错了");
}
```

先读 `nested_tree()` 与 `Node::file` 的实际签名,把上面用到的路径/构造对齐(**不要凭记忆写**):

Run: `grep -n "fn nested_tree" -A 30 crates/mullion-ssh/tests/sftp_write.rs`
Run: `grep -n "pub fn file" -A 14 crates/mullion-ssh/tests/common/sftp_server.rs`

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-ssh --test sftp_write paste 2>&1 | tail -20`
Expected: 编译失败,`could not find copy_tree in mullion_ssh`。

- [ ] **Step 3: 写实现**

`crates/mullion-ssh/src/lib.rs` 加 `pub mod copy_tree;`(挨着 `pub mod remove_tree;`)。

`crates/mullion-ssh/src/copy_tree.rs`:

```rust
//! F220:远端**内部**的复制 / 移动。**先试 exec `cp -a` / `mv`,被拒则回退
//! SFTP 逐文件递归** —— 与 `remove_tree` 逐字对称的取舍(设计 D17):
//!
//! - 一律走 exec:sftp-only 账号(`ForceCommand internal-sftp`)会拒绝 exec,
//!   功能在那种账号上直接残缺。
//! - 一律逐文件:同一台机器上拷一个 `node_modules`,每个字节都要拉到客户端
//!   再送回去 —— 高延迟代理链路上是几十分钟对几秒的差别。
//!
//! **绝不跟随符号链接**:列举用 `list_dir`(readdir = lstat 语义),遇到
//! `EntryKind::Symlink` 用 `read_link` + `symlink` 原样重建,不进去。搞错了
//! 就是把链接指向的整个目录复制一遍。
//!
//! **调用方必须先过 `files::clip::is_within` 那道闸**(目标不能是源自身或
//! 其子孙):远端 `cp` 自己会拦,但下面这段递归是我们自己写的,会一直
//! 递归到把磁盘写满。

use std::sync::Arc;

use crate::exec::{exec, shell_quote};
use crate::session::SshConnection;
use crate::sftp::{EntryKind, RemotePath, SftpClient, SftpError};

/// 复制还是移动。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyMode {
    Copy,
    Move,
}

/// 这一次实际走了哪条路。调用方用它写日志 / 断言,不影响正确性。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferReport {
    Exec,
    Sftp,
}

/// 把 `pairs` 里的每一条(源绝对路径 → 目标绝对路径)拷 / 挪过去。
///
/// `overwrite` 为真时目标可以已存在(命令加 `-f`,回退路上先删目标);
/// 为假时调用方(`files::clip::plan_paste`)已经保证目标不存在。
///
/// 任一路径 `as_wire()` 过不了就**一个请求都不发**(同 `remove_tree`
/// 开头那道门):拿一串替换字符去 `cp` 是本项目能犯的最严重的错。
pub async fn transfer_into(
    sftp: &SftpClient,
    conn: &Arc<SshConnection>,
    pairs: &[(RemotePath, RemotePath)],
    mode: CopyMode,
    overwrite: bool,
) -> Result<TransferReport, SftpError> {
    for (a, b) in pairs {
        let _ = a.as_wire()?;
        let _ = b.as_wire()?;
    }
    if pairs.is_empty() {
        return Ok(TransferReport::Exec);
    }

    match try_exec(conn, pairs, mode, overwrite).await {
        Some(true) => return Ok(TransferReport::Exec),
        // 命令跑了但没成功(权限不足、磁盘满…):**不回退**。回退只会把
        // 同一个错误再犯一遍,而且是慢一千倍地犯(同 `remove_tree`)。
        Some(false) => {
            return Err(SftpError::Protocol(
                "远端 cp/mv 命令执行失败(可能是权限不足或磁盘已满)".into(),
            ))
        }
        None => {}
    }

    for (from, to) in pairs {
        if overwrite {
            // 目标可能是个非空目录 —— `rename`/逐文件写都盖不掉它。
            let _ = crate::remove_tree::remove_tree(sftp, conn, to).await;
        }
        match mode {
            CopyMode::Copy => copy_one(sftp, from, to).await?,
            CopyMode::Move => {
                // 同一文件系统内 `rename` 是一次往返就完事的最优解;
                // 跨设备(EXDEV)会失败,那时才退成「拷完删源」。
                if sftp.rename(from, to).await.is_err() {
                    copy_one(sftp, from, to).await?;
                    crate::remove_tree::remove_tree(sftp, conn, from).await?;
                }
            }
        }
    }
    Ok(TransferReport::Sftp)
}

/// 拼一条命令发出去。`None` = 对端**拒绝**执行(sftp-only 账号),该回退;
/// `Some(false)` = 命令跑了但失败了。
async fn try_exec(
    conn: &Arc<SshConnection>,
    pairs: &[(RemotePath, RemotePath)],
    mode: CopyMode,
    overwrite: bool,
) -> Option<bool> {
    // 每一对一条子命令,`&&` 串起来 —— 一次往返干完整批,且前一条失败
    // 就停(半途而废好过继续往一个已经出错的目标里灌)。
    let head: &[u8] = match (mode, overwrite) {
        (CopyMode::Copy, false) => b"cp -a -- ",
        (CopyMode::Copy, true) => b"cp -af -- ",
        (CopyMode::Move, false) => b"mv -- ",
        (CopyMode::Move, true) => b"mv -f -- ",
    };
    let mut cmd: Vec<u8> = Vec::new();
    for (from, to) in pairs {
        if !cmd.is_empty() {
            cmd.extend_from_slice(b" && ");
        }
        cmd.extend_from_slice(head);
        cmd.extend_from_slice(&shell_quote(from.as_bytes()));
        cmd.push(b' ');
        cmd.extend_from_slice(&shell_quote(to.as_bytes()));
    }
    match exec(conn, cmd).await {
        Ok(out) => Some(out.succeeded()),
        Err(_) => None,
    }
}

/// SFTP 回退:拷一条(文件 / 链接 / 整棵目录树)。**不跟随链接**。
async fn copy_one(sftp: &SftpClient, from: &RemotePath, to: &RemotePath) -> Result<(), SftpError> {
    let meta = sftp.stat(from).await?;
    match meta.kind {
        EntryKind::Dir => {
            sftp.create_dir(to).await?;
            // 列举用 `list_dir`(readdir = lstat 语义):链接在这里是
            // `Symlink`,不会被当成它指向的目录。
            for e in sftp.list_dir(from).await? {
                let child_from = from.join(e.name.as_bytes());
                let child_to = to.join(e.name.as_bytes());
                Box::pin(copy_one(sftp, &child_from, &child_to)).await?;
            }
            sftp.set_permissions(to, meta.mode & 0o7777).await?;
        }
        EntryKind::Symlink => {
            // 原样重建,**不跟进去**。目标读不出来就跳过这一条 ——
            // 一条断链不该让整批粘贴失败。
            if let Some(target) = meta.link_target.clone() {
                sftp.symlink(to, &target).await?;
            }
        }
        _ => {
            copy_file_bytes(sftp, from, to).await?;
            sftp.set_permissions(to, meta.mode & 0o7777).await?;
        }
    }
    Ok(())
}

/// 一个普通文件的字节搬运。分块走,不整个读进内存 —— 远端上一个几 GB 的
/// core dump 会把客户端 OOM 掉(同 `read_all` 那条上限的理由)。
async fn copy_file_bytes(
    sftp: &SftpClient,
    from: &RemotePath,
    to: &RemotePath,
) -> Result<(), SftpError> {
    let mut src = sftp.open_read(from).await?;
    let mut dst = sftp.open_write(to, true).await?;
    // 256 KiB:对齐 russh-sftp 的单包上限,往返数才是成本(F214)。
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = src.read_chunk(&mut buf).await?;
        if n == 0 {
            break;
        }
        dst.write_chunk(&buf[..n]).await?;
    }
    dst.finish().await
}
```

`SftpClient` 上若还没有 `symlink`,照 `create_dir` 的写法加一个(russh-sftp 2.4.0 的 `session.rs:231` 有 `symlink(path, target)`,**参数顺序按它的实际签名来**,先读一遍源码,别凭记忆)。`Entry` 上若没有 `link_target`(`stat` 返回的那份),用 `sftp.read_link()` 单独问一次。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-ssh --test sftp_write 2>&1 | grep -E "test result|FAILED"`
Expected: 全过(6 条新的 + 既有全部)。

- [ ] **Step 5: 变异验证**

- 把 `copy_one` 的 `Symlink` 那一臂改成走 `Dir` 的分支 → `a_paste_never_follows_a_symlink_on_either_path` 必须红。
- 把 `try_exec` 里 `Err(_) => None` 改成 `Err(_) => Some(false)` → 回退那条必须红。
- 把 `CopyMode::Move` 回退路里删源那句去掉 → 剪切那条必须红。

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-ssh/src/copy_tree.rs crates/mullion-ssh/src/lib.rs crates/mullion-ssh/src/sftp.rs crates/mullion-ssh/tests/sftp_write.rs
git commit -m "feat(ssh): 远端内复制/移动,exec 快路径 + SFTP 回退 (F220)"
```

---

## Task B4: 剪贴板落到标签上 + 三个入口

**Files:**
- Modify: `crates/mullion-app/src/ui/files_panel.rs`
- Modify: `crates/mullion-app/src/app.rs`

- [ ] **Step 1: 写失败的测试**

`files_panel.rs` 的 `mod tests`:

```rust
    /// F220:复制/剪切/粘贴只在远端栏出现(D5:只在远端)。
    #[test]
    fn the_clipboard_items_are_remote_only() {
        let tg = Some(MenuTarget { is_file: true, size: 10 });
        let remote: Vec<&str> = menu_items_for_with_clip(PanelColumn::Remote, tg, true)
            .iter()
            .map(|e| e.label)
            .collect();
        for want in ["复制", "剪切", "粘贴"] {
            assert!(remote.contains(&want), "远端栏少了「{want}」");
        }
        let local: Vec<&str> = menu_items_for_with_clip(PanelColumn::Local, tg, true)
            .iter()
            .map(|e| e.label)
            .collect();
        for never in ["复制", "剪切", "粘贴"] {
            assert!(!local.contains(&never), "本地栏出现了「{never}」(只在远端)");
        }
    }

    /// F220:剪贴板空的时候「粘贴」**置灰并说出理由**,不是消失 ——
    /// 悄悄少一项,用户只会以为程序坏了(D3-2)。
    ///
    /// 自证会变红:把那一项改成「剪贴板空就不 push」。
    #[test]
    fn paste_is_greyed_out_with_a_reason_when_the_clipboard_is_empty() {
        let items = menu_items_for_with_clip(PanelColumn::Remote, None, false);
        let paste = items
            .iter()
            .find(|e| e.label == "粘贴")
            .expect("剪贴板空时「粘贴」不该消失,该置灰");
        assert!(paste.disabled.is_some(), "剪贴板空时「粘贴」没置灰");
    }
```

(把 `menu_items_for` 改名为带 clip 参数的版本;既有那几条调用 `menu_items_for` 的测试同步改。**新签名统一叫 `menu_items_for`**,别留两个名字 —— 上面测试里的 `menu_items_for_with_clip` 只是占位,写实现时统一成一个名字并回来改掉测试。)

`app.rs` 的 `mod tests`:

```rust
    /// F220:三个剪贴板快捷键只在**远端栏**放行(D5 + 用户明确要的
    /// 「只在远端」)。焦点在本地栏时静默不动。
    ///
    /// 自证会变红:把 `handle_panel_key` 里那段的栏判断删掉。
    #[test]
    fn the_clipboard_shortcuts_are_remote_only() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn handle_panel_key(")
            .nth(1)
            .expect("找不到 handle_panel_key");
        let body = &after[..after.find("\n    }\n").expect("找不到函数结尾")];
        for (key, action) in [("\"c\"", "ClipCopy"), ("\"x\"", "ClipCut"), ("\"v\"", "ClipPaste")] {
            let at = body
                .find(key)
                .unwrap_or_else(|| panic!("Ctrl+{key} 没接上"));
            let arm = &body[at..(at + 400).min(body.len())];
            assert!(
                arm.contains("PanelColumn::Remote"),
                "Ctrl+{key} 没判栏 —— 在本地栏按会动到远端"
            );
            assert!(arm.contains(action), "Ctrl+{key} 没派发 {action}");
        }
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib clipboard 2>&1 | tail -20` → FAIL

- [ ] **Step 3: 写实现**

(a) `PanelFrame` 加字段:

```rust
    /// F220:这个标签的远端剪贴板。`None` = 空。
    ///
    /// **per-tab**(设计 B4):里面装的是绝对路径,只对当前这条连接有意义。
    /// 挂在这里而不是 `App` 上,语义就零歧义 —— 切到别的标签自然是空的。
    ///
    /// 默认 `None` 是安全值,符合这个结构体 `Default` 那条「加字段前先想
    /// 清楚」的约束(它同时当新标签初值和借用过桥的占位):过桥期间被
    /// `mem::take` 换成 `None` 又原样放回,窗口极短、期间没人读它。
    pub clip: Option<crate::files::clip::RemoteClip>,
```

`Default::default()` 与 `PanelFrame::new` 里补 `clip: None,`。

(b) `MenuItem` / `FileAction` 各加三档:

```rust
    /// F220:把选中集放进这个标签的远端剪贴板。
    ClipCopy,
    ClipCut,
    /// F220:粘到当前目录。剪贴板空时这一项是置灰的。
    ClipPaste,
```

`MenuItem::into_action` 加三条一一对应的转发。

(c) `menu_items_for` 加参数并补三项:

```rust
/// 这一栏此刻该有哪些右键菜单项。
///
/// - `column`:远端栏才有写操作(设计 D5)。
/// - `target`:光标行。`None` 就不给单目标操作。
/// - `clip_ready`:这个标签的远端剪贴板里有东西没(F220)。空的时候
///   「粘贴」**置灰并说明理由**,不是消失 —— 悄悄少一项,用户只会以为
///   程序坏了(D3-2)。
pub fn menu_items_for(
    column: PanelColumn,
    target: Option<MenuTarget>,
    clip_ready: bool,
) -> Vec<MenuEntry> {
```

远端分支里,「删除…」之后:

```rust
        if target.is_some() {
            out.push(on("复制", MenuItem::ClipCopy));
            out.push(on("剪切", MenuItem::ClipCut));
        }
        out.push(MenuEntry {
            label: "粘贴",
            item: MenuItem::ClipPaste,
            disabled: (!clip_ready).then_some("剪贴板是空的 —— 先在远端栏复制或剪切"),
        });
```

`menu_body` / `show` 一路把 `clip_ready` 传下去(`show` 加一个 `clip_ready: bool` 参数,四个调用点各传 `frame.clip.is_some()`)。

(d) `app.rs` `apply_remote_file_action` 加三臂(在借出 `files` 之前那段 match 里):

```rust
            // F220:复制 / 剪切。**只记路径,不发任何请求** —— 真正的动作
            // 在粘贴那一刻。路径在这里就拼成绝对路径:用户接着换目录、
            // 换排序都不该影响剪贴板指向谁。
            FileAction::ClipCopy | FileAction::ClipCut => {
                let mode = if matches!(action, FileAction::ClipCut) {
                    crate::files::clip::ClipMode::Cut
                } else {
                    crate::files::clip::ClipMode::Copy
                };
                if let Some(files) = self
                    .tabs
                    .by_generation_mut(generation)
                    .and_then(|t| t.content.files_panel_mut())
                {
                    // 名字发不出去 wire 请求的一律不收(同删除/传输的判据):
                    // 收进来只会让用户以为「拷了 5 个」,其实有一条必然失败。
                    let items: Vec<(mullion_ssh::sftp::RemotePath, bool)> = files
                        .remote
                        .picked_entries()
                        .into_iter()
                        .filter(|e| e.name.is_operable())
                        .map(|e| {
                            (
                                files.remote.cwd.join(e.name.as_bytes()),
                                e.kind == mullion_ssh::sftp::EntryKind::Dir,
                            )
                        })
                        .collect();
                    if items.is_empty() {
                        return;
                    }
                    let n = items.len();
                    files.clip = Some(crate::files::clip::RemoteClip { mode, items });
                    let verb = if mode == crate::files::clip::ClipMode::Cut {
                        "剪切"
                    } else {
                        "复制"
                    };
                    self.ui
                        .set_toast(crate::ui::toast::Kind::Ok, format!("已{verb} {n} 项"));
                    mark_ui_dirty!(self.ui_dirty);
                    self.request_ui_redraw();
                }
                return;
            }
            // F220:粘贴。编排见 `start_paste`(要先列一次目标目录做预检查)。
            FileAction::ClipPaste => {
                self.start_paste(generation);
                return;
            }
```

`target` 那个穷尽 match 的「上面已分流」组里补上这三个变体;`apply_local_file_action` 的穷尽 match 里加一臂 `log::warn!("本地栏收到了剪贴板操作,已忽略(只在远端)")`。

(e) `handle_panel_key` 的 Ctrl 段里补三支(与 `Ctrl+N` 同一个形状,栏判断照抄):

```rust
                // F220:剪贴板三键。**只在远端栏**(用户明确要的「只在远端」)。
                let clip = match s.as_str() {
                    "c" => Some(FileAction::ClipCopy),
                    "x" => Some(FileAction::ClipCut),
                    "v" => Some(FileAction::ClipPaste),
                    _ => None,
                };
                if let Some(a) = clip {
                    let column = self
                        .tabs
                        .by_generation(generation)
                        .and_then(|t| t.content.files_panel())
                        .map(|f| f.active_column);
                    if column == Some(crate::ui::files_panel::PanelColumn::Remote) {
                        self.dispatch_panel_action(generation, a);
                    }
                    return;
                }
```

(f) 剪切后源行画淡:`row()` 里选中判断附近,给「在剪贴板里且 mode 是 Cut」的行把前景色降一档。`row()` 现在没有剪贴板信息,加一个 `dimmed: bool` 参数,由 `show` 按 `cut_names: &BTreeSet<Vec<u8>>` 算 —— 这份集合在 `show` 开头从新加的参数算出来(`show` 已经有 `clip_ready`,把它换成 `clip: Option<&RemoteClip>` 更省一个参数)。文字颜色用 `t.fg_muted`,**不新造色值**(UI 视觉规格已冻结)。

- [ ] **Step 4: 跑测试确认通过 + 提交**

Run: `cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED"`

```bash
git add crates/mullion-app/src/ui/files_panel.rs crates/mullion-app/src/app.rs
git commit -m "feat(app): 远端剪贴板与三个入口(复制/剪切/粘贴) (F220)"
```

---

## Task B5: 粘贴编排 —— 自身闸门 + 预检查

**Files:**
- Modify: `crates/mullion-app/src/app.rs`

- [ ] **Step 1: 写失败的测试**

```rust
    /// F220:粘贴**必须先列一次目标目录**再动手 —— 面板里那份 `entries`
    /// 可能是几分钟前的,而覆盖不可逆。
    ///
    /// 自证会变红:把 `start_paste` 里那次列目录删掉、直接发写操作。
    #[test]
    fn a_paste_checks_the_destination_before_it_writes_anything() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn start_paste(")
            .nth(1)
            .expect("找不到 start_paste");
        let body = &after[..after.find("\n    }\n").expect("找不到函数结尾")];
        assert!(
            body.contains("list_dir"),
            "粘贴没做预检查 —— 会直接盖掉目标目录里的同名文件"
        );
        assert!(
            !body.contains("FileOp::Paste"),
            "粘贴在预检查回来之前就把写操作发出去了"
        );
    }

    /// F220 最重要的一条闸门:目标是源自身或其子孙 → **一个请求都不发**。
    /// SFTP 回退那条路是我们自己写的递归,会一直递归到把磁盘写满。
    ///
    /// 自证会变红:把 `start_paste` 里那句 `is_within` 判断删掉。
    #[test]
    fn pasting_into_your_own_subtree_is_refused_before_any_request() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn start_paste(")
            .nth(1)
            .expect("找不到 start_paste");
        let body = &after[..after.find("\n    }\n").expect("找不到函数结尾")];
        let gate = body
            .find("is_within")
            .expect("没有自身/子孙闸门 —— SFTP 回退会无限递归");
        let check = body.find("list_dir").expect("找不到预检查");
        assert!(gate < check, "闸门排在预检查之后 —— 请求已经发出去了");
    }
```

- [ ] **Step 2: 跑测试确认失败** → FAIL(`找不到 start_paste`)

- [ ] **Step 3: 写实现**

(a) 新事件:

```rust
    /// F220:粘贴前的目标目录预检查回来了。`Ok` 是那个目录里**现有的名字**。
    ///
    /// 按世代路由(S1):高延迟链路上列一次目录要好几百毫秒,用户完全可能
    /// 已经切了标签 —— 结果必须回到发起它的那个标签。标签没了就丢弃。
    PasteChecked {
        generation: u64,
        result: Result<Vec<mullion_ssh::sftp::RemotePath>, String>,
    },
```

(b) `start_paste`:

```rust
    /// F220:发起一次粘贴。**三步里的前两步**:自身/子孙闸门 → 预检查。
    /// 第三步(弹框 / 直接执行)在 `UserEvent::PasteChecked` 里。
    fn start_paste(&mut self, generation: u64) {
        let Some(tab) = self.tabs.by_generation(generation) else {
            return;
        };
        let Some(files) = tab.content.files_panel() else {
            return;
        };
        let Some(clip) = files.clip.clone() else {
            self.ui.set_toast(
                crate::ui::toast::Kind::Warn,
                "剪贴板是空的 —— 先在远端栏复制或剪切",
            );
            return;
        };
        let dst = files.remote.cwd.clone();

        // **闸门排在任何请求之前**:目标是源自身或其子孙时,SFTP 回退那条
        // 路会边列源边往源里写,一直递归到把磁盘写满。远端 `cp` 自己会拦,
        // 但我们不能只靠远端兜底 —— 回退路上没有 `cp`。
        if clip
            .items
            .iter()
            .any(|(src, _)| crate::files::clip::is_within(src, &dst))
        {
            self.ui.set_toast(
                crate::ui::toast::Kind::Warn,
                "不能粘到源目录自己或它的子目录里",
            );
            return;
        }
        // 同目录剪切是空操作 —— 一个请求都不用发。
        if clip.mode == crate::files::clip::ClipMode::Cut
            && clip.items.iter().all(|(src, _)| src.parent() == dst)
        {
            self.ui
                .set_toast(crate::ui::toast::Kind::Warn, "源和目标是同一个目录");
            return;
        }

        let Some(client) = tab.content.sftp_client() else {
            self.ui
                .set_error("SFTP 通道还没建立,请先等目录加载完".into());
            return;
        };
        let proxy = self.proxy.clone();
        let task = self._runtime.spawn(async move {
            let result = client
                .list_dir(&dst)
                .await
                .map(|v| v.into_iter().map(|e| e.name).collect())
                .map_err(|e| e.to_string());
            let _ = proxy.send_event(UserEvent::PasteChecked { generation, result });
        });
        self.track_sftp_task(generation, task);
        self.ui
            .set_toast(crate::ui::toast::Kind::Busy, "正在检查目标目录…");
    }
```

(c) 事件处理:

```rust
            UserEvent::PasteChecked { generation, result } => {
                self.accept_paste_check(generation, result);
                self.request_ui_redraw();
            }
```

```rust
    /// F220:预检查回来了 —— 有冲突就弹批量框,没有就直接发。
    ///
    /// **同目录复制必然全撞** → 直接走「保留两者」,不弹框:问用户
    /// 「要不要覆盖自己」没有意义。
    fn accept_paste_check(
        &mut self,
        generation: u64,
        result: Result<Vec<mullion_ssh::sftp::RemotePath>, String>,
    ) {
        use crate::files::clip::{self, Policy};
        let existing: std::collections::BTreeSet<Vec<u8>> = match result {
            Ok(v) => v.into_iter().map(|n| n.as_bytes().to_vec()).collect(),
            Err(msg) => {
                self.ui.set_error(msg);
                return;
            }
        };
        let Some(files) = self
            .tabs
            .by_generation(generation)
            .and_then(|t| t.content.files_panel())
        else {
            return;
        };
        let Some(clip) = files.clip.clone() else {
            return;
        };
        let dst = files.remote.cwd.clone();
        let same_dir = clip.items.iter().all(|(src, _)| src.parent() == dst);
        let hits = clip::conflicts(&clip.items, &existing);
        if hits.is_empty() || (same_dir && clip.mode == clip::ClipMode::Copy) {
            let policy = if same_dir { Policy::KeepBoth } else { Policy::Overwrite };
            self.dispatch_paste(generation, policy);
            return;
        }
        self.ui.files_dialog = Some(crate::ui::files_dialog::FilesDialog::PasteConflict {
            names: hits.iter().map(|n| String::from_utf8_lossy(n).into_owned()).collect(),
            mode_is_cut: clip.mode == clip::ClipMode::Cut,
        });
        self.request_ui_redraw();
    }
```

(注:`hits.is_empty()` 那条路里的 `Policy::Overwrite` 只是形式参数 —— 没有冲突时三档算出来的 `pairs` 完全一样。)

- [ ] **Step 4: 跑测试确认通过 + 提交**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): 粘贴的自身闸门与目标目录预检查 (F220)"
```

---

## Task B6: 批量冲突框

**Files:**
- Modify: `crates/mullion-app/src/ui/files_dialog.rs`

- [ ] **Step 1: 写失败的测试**

`files_dialog.rs` 的 `mod tests`(照该文件里既有的 `click_button` / `dialog_texts` 助手写):

```rust
    /// F220:冲突框给三条出路,且**列出撞了哪几条** —— 只说「有冲突」
    /// 用户没法判断该选哪个。
    #[test]
    fn the_paste_conflict_dialog_offers_three_ways_out_and_names_the_collisions() {
        let mut d = Some(FilesDialog::PasteConflict {
            names: vec!["a.txt".into(), "b.txt".into()],
            mode_is_cut: false,
        });
        let texts = dialog_texts(&mut d);
        for want in ["覆盖", "跳过同名", "保留两者", "a.txt", "b.txt"] {
            assert!(
                texts.iter().any(|s| s.contains(want)),
                "冲突框里没有「{want}」,实际:{texts:?}"
            );
        }
    }

    /// F220:取消 / ✕ = **整批不动**。粘贴还没发出去,没有挂起的工作要处置
    /// (与 `Conflict`/`EditConflict` 那两个框正相反)。
    ///
    /// 自证会变红:把 `cancel_op` 里 `PasteConflict` 归到 `Some(..)` 那一组。
    #[test]
    fn cancelling_a_paste_conflict_does_nothing_at_all() {
        let d = FilesDialog::PasteConflict {
            names: vec!["a.txt".into()],
            mode_is_cut: false,
        };
        assert_eq!(cancel_op(&d), None, "取消粘贴不该发出任何处置");
    }

    /// F220:三颗按钮各自送回对应的处置。
    #[test]
    fn each_button_sends_back_its_own_policy() {
        for (label, want) in [
            ("覆盖", crate::files::clip::Policy::Overwrite),
            ("跳过同名", crate::files::clip::Policy::Skip),
            ("保留两者", crate::files::clip::Policy::KeepBoth),
        ] {
            let mut d = Some(FilesDialog::PasteConflict {
                names: vec!["a.txt".into()],
                mode_is_cut: false,
            });
            assert_eq!(
                click_button(&mut d, label),
                Some(FileOp::Paste { policy: want }),
                "「{label}」送回的处置不对"
            );
            assert!(d.is_none(), "点完「{label}」框没关");
        }
    }
```

- [ ] **Step 2: 跑测试确认失败** → FAIL

- [ ] **Step 3: 写实现**

(a) `FilesDialog` 加变体:

```rust
    /// F220:粘贴的目标目录里已经有同名的。**必须问**,绝不静默覆盖。
    ///
    /// 只带**名字**(给用户看的)不带路径:真正要粘什么在标签的剪贴板里,
    /// 到时候按用户选的处置现算(同 `Conflict` 只带 `job` 的理由)。
    PasteConflict {
        names: Vec<String>,
        /// 剪切还是复制 —— 文案要说清「移动」还是「复制」。
        mode_is_cut: bool,
    },
```

(b) `FileOp` 加:

```rust
    /// F220:用户在冲突框里选完了。**不带路径与条目** —— 粘什么、粘到哪
    /// 都在标签的剪贴板与当前目录里,界面只把选择原样送回(同 `Resolve`)。
    Paste {
        policy: crate::files::clip::Policy,
    },
```

(c) `cancel_op` 的第一组里加上 `FilesDialog::PasteConflict { .. }`(返回 `None`),并把那段文档注释里的「前四个框」改成「前五个框」。

(d) `show` 加一臂:

```rust
        FilesDialog::PasteConflict { names, mode_is_cut } => {
            let verb = if *mode_is_cut { "移动" } else { "复制" };
            let n = names.len();
            let list = names.clone();
            let x = modal(ctx, "目标目录里已有同名项", |ui| {
                ui.colored_label(
                    theme::c32(t.danger_text),
                    format!("有 {n} 项与目标目录里的同名"),
                );
                ui.label(
                    egui::RichText::new(format!("选「覆盖」会用{verb}过去的那份盖掉原有内容,不可逆。"))
                        .color(theme::c32(t.fg_muted)),
                );
                egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                    for name in &list {
                        ui.label(name);
                    }
                });
                ui.horizontal(|ui| {
                    if ui.button("覆盖").clicked() {
                        op = Some(FileOp::Paste { policy: crate::files::clip::Policy::Overwrite });
                        close = true;
                    }
                    if ui.button("跳过同名").clicked() {
                        op = Some(FileOp::Paste { policy: crate::files::clip::Policy::Skip });
                        close = true;
                    }
                    if ui.button("保留两者").clicked() {
                        op = Some(FileOp::Paste { policy: crate::files::clip::Policy::KeepBoth });
                        close = true;
                    }
                    if ui.button("取消").clicked() {
                        cancelled!();
                    }
                });
            });
            if x {
                cancelled!();
            }
        }
```

(e) `files_dialog.rs` 里那两处**对话框完备性表**(该文件 `mod tests` 里枚举全部变体的那两个测试,约 514 行与 538 行)各补一条 `PasteConflict`。跑测试会直接告诉你漏了哪个。

- [ ] **Step 4: 跑测试确认通过 + 提交**

```bash
git add crates/mullion-app/src/ui/files_dialog.rs
git commit -m "feat(app): 粘贴同名的批量冲突框,取消即整批不动 (F220)"
```

---

## Task B7: 执行粘贴 + 剪切成功后清剪贴板

**Files:**
- Modify: `crates/mullion-app/src/app.rs`

- [ ] **Step 1: 写失败的测试**

```rust
    /// F220:**剪切成功之后才清剪贴板**。发出那一刻就清是拿假前提改状态 ——
    /// 链路一断,用户的剪贴板没了、东西也没挪走(T11 同族)。
    ///
    /// B6 已经把 `dispatch_paste` 拆成「只取 client 就转发」+ 自由函数
    /// `spawn_paste_task`(复核 C1 逼出来的形状:后者没有 `&self`,真正
    /// 决定 `follow`、发起传输的逻辑必须写在这个自由函数里,不能写回
    /// `dispatch_paste`——那样又会长出一条摸得到 `self.tabs` 的路)。所以
    /// 这条测试扫的是 `spawn_paste_task` 的函数体,不是 `dispatch_paste`。
    ///
    /// 自证会变红:把清剪贴板那句挪到 `dispatch_paste` 里、发出那一刻就清
    /// (而 `dispatch_paste` 根本摸不到 `self.tabs...files_panel_mut()`
    /// 之外的清空路径,一挪就会发现要么编译不过、要么绕回 C1 的老问题)。
    #[test]
    fn the_clipboard_is_cleared_only_after_a_cut_actually_lands() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn spawn_paste_task(")
            .nth(1)
            .expect("找不到 spawn_paste_task");
        let body = &after[..after.find("\n}\n").expect("找不到函数结尾")];
        assert!(
            !body.contains("clip = None"),
            "在发出那一刻就清了剪贴板 —— 链路一断东西没挪走、剪贴板也没了"
        );
        // 清空只能挂在完成事件的后续动作上。
        assert!(
            src.contains("OpFollow::ClearClip"),
            "没有「成功之后清剪贴板」这条后续动作"
        );
    }

    /// F220:复制粘贴成功**不清**剪贴板 —— 连着粘几个目录是常见用法。
    ///
    /// 自证会变红:`spawn_paste_task` 里给 Copy 也挂 `ClearClip`。
    #[test]
    fn a_copy_paste_keeps_the_clipboard_so_it_can_be_pasted_again() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn spawn_paste_task(")
            .nth(1)
            .expect("找不到 spawn_paste_task");
        let body = &after[..after.find("\n}\n").expect("找不到函数结尾")];
        let at = body
            .find("ClearClip")
            .expect("剪切那条腿丢了 —— 剪完源还在剪贴板里");
        let arm = &body[..at];
        assert!(
            arm.contains("ClipMode::Cut"),
            "ClearClip 没跟 Cut 绑在一起 —— 复制粘完剪贴板也被清了"
        );
    }
```

- [ ] **Step 2: 跑测试确认失败** → FAIL

- [ ] **Step 3: 写实现**

(a) `OpFollow` 加一档:

```rust
    /// F220:剪切粘贴成功 —— 清空这个标签的远端剪贴板。**只在成功之后**:
    /// 发出那一刻就清的话,链路一断用户的剪贴板没了、东西也没挪走(T11)。
    ClearClip,
```

(b) 复核 C1 已经把这一段提前搬到了 B6:`FileOp::Paste` 不是界面送回来的单一「选择」,而是 `accept_paste_check` 那一刻就冻结好的一整批值(`dst`/`clip`/`seq`/`policy`/`existing`,`existing` 是预检查时**服务器现读**的目标目录列表,不是面板缓存——理由见 `FileOp::Paste` 的文档注释)。`apply_file_op` 开头(与 `Resolve`/`ResolveEdit` 同一段提前分流)现状已经是:

```rust
        // 要不要开 SFTP 通道(B7 的活),不走下面这条通用「先拿 client 再判
        // op 是什么」的路径。`dst`/`clip`/`seq`/`existing` 原样转发,不在
        // 这里重新读面板状态(复核 C1/I2 的教训)。
        if let FileOp::Paste {
            dst,
            clip,
            seq,
            policy,
            existing,
        } = op
        {
            self.dispatch_paste(generation, dst, clip, seq, policy, existing);
            return;
        }
```

这一段 B7 不用再改,照抄现状即可 —— **别把它退化回单字段 `FileOp::Paste { policy }`,那正是复核 C1 要防的写法**。

(c) `dispatch_paste` + `spawn_paste_task`:B6 已经把「取 client」和「真正发起传输」拆成了两层,后者是一个**没有 `&self`/`&Tabs` 参数的自由函数**(复核 C1 的类型层保证:这个函数的作用域里连 `self`/`tab` 这两个标识符都不存在,想现读面板状态是编译错误,不是靠约定挡)。`dispatch_paste` 现状:

```rust
    fn dispatch_paste(
        &mut self,
        generation: u64,
        dst: mullion_ssh::sftp::RemotePath,
        clip: crate::files::clip::RemoteClip,
        seq: u64,
        policy: crate::files::clip::Policy,
        existing: std::collections::BTreeSet<Vec<u8>>,
    ) {
        let Some(client) = self
            .tabs
            .by_generation(generation)
            .and_then(|t| t.content.sftp_client())
        else {
            self.ui.set_error("SFTP 通道还没建立,粘贴没能执行".into());
            return;
        };
        let task = spawn_paste_task(
            &self._runtime,
            &self.proxy,
            generation,
            client,
            dst,
            clip,
            seq,
            policy,
            existing,
        );
        self.track_sftp_task(generation, task);
    }
```

这一段 B7 也不用改。B7 要填的是 `spawn_paste_task`(紧跟在 `dispatch_paste` 下面,靠近 `spawn_sftp_stat` 那一片自由函数)现在的桩身体:

```rust
#[allow(clippy::too_many_arguments)]
fn spawn_paste_task(
    runtime: &Runtime,
    proxy: &EventLoopProxy<UserEvent>,
    generation: u64,
    _client: Arc<mullion_ssh::sftp::SftpClient>,
    dst: mullion_ssh::sftp::RemotePath,
    clip: crate::files::clip::RemoteClip,
    seq: u64,
    policy: crate::files::clip::Policy,
    existing: std::collections::BTreeSet<Vec<u8>>,
) -> tokio::task::JoinHandle<()> {
    let _ = proxy;
    runtime.spawn(async move {
        log::warn!("F220:粘贴执行尚未接通(…),留给 B7");
    })
}
```

换成真正执行,大致形状(`conn` 的取法照 `apply_file_op` 里那一句现读现抄;toast 措辞、`seq` 陈旧校验照 `decide_paste_drops_a_result_whose_sequence_has_gone_stale` 已经钉死的规矩,不要另起一套;`_client` 要用上就去掉下划线):

```rust
#[allow(clippy::too_many_arguments)] // 见 B6 已加的注释,原样保留
fn spawn_paste_task(
    runtime: &Runtime,
    proxy: &EventLoopProxy<UserEvent>,
    generation: u64,
    client: Arc<mullion_ssh::sftp::SftpClient>,
    dst: mullion_ssh::sftp::RemotePath,
    clip: crate::files::clip::RemoteClip,
    seq: u64,
    policy: crate::files::clip::Policy,
    existing: std::collections::BTreeSet<Vec<u8>>,
) -> tokio::task::JoinHandle<()> {
    use crate::files::clip::{plan_paste, ClipMode};
    let proxy = proxy.clone();
    // `existing` 是 `accept_paste_check` 那一刻服务器现读的实况,原样冻结
    // 带到这里(复核 C1)——不再现读面板缓存。`plan_paste` 是纯函数,拿
    // 它跟同一份 `policy` 再算一次「保留两者」的新名字,与预检查判冲突
    // 用的是同一份 `existing`,不会因为两次读的时机不同而对不上。
    runtime.spawn(async move {
        let plan = plan_paste(&clip.items, &dst, policy, &existing);
        if plan.pairs.is_empty() {
            // 全部跳过:没有传输要等,不发 SftpOpDone,直接开一张 toast
            // 收尾;`seq` 在这里怎么核对陈旧 UI 状态,参照上面提到的那条
            // 已经定规矩的测试。
            return;
        }
        let mode = match clip.mode {
            ClipMode::Copy => mullion_ssh::copy_tree::CopyMode::Copy,
            ClipMode::Cut => mullion_ssh::copy_tree::CopyMode::Move,
        };
        let overwrite = policy == crate::files::clip::Policy::Overwrite;
        // 剪切成功之后才清剪贴板;复制不清(连着粘几个目录是常见用法)。
        // 这里只算 `follow` 传出去,真正清空的动作在 `SftpOpDone` 的成功
        // 分支里做(见下面 (d))——**不能**在这个自由函数里直接碰
        // `self.tabs`,它压根没有 `self` 可碰(复核 C1 的类型层保证)。
        let follow = if clip.mode == ClipMode::Cut {
            OpFollow::ClearClip
        } else {
            OpFollow::None
        };
        let conn = /* 照 apply_file_op 里取 conn 的写法现读现抄 */;
        let result = mullion_ssh::copy_tree::transfer_into(
            &client,
            &conn,
            &plan.pairs,
            mode,
            overwrite,
        )
        .await
        .map(|_| ())
        .map_err(|e| e.to_string());
        let _ = proxy.send_event(UserEvent::SftpOpDone {
            generation,
            result,
            follow,
        });
        let _ = seq; // 陈旧校验接上,别漏掉
    })
}
```

进度提示(`正在{verb} {n} 项…` 那条 toast)在这个自由函数里发不出去 —— 它没有 `&mut self.ui`。要么在 `dispatch_paste` 派发之前、还在 `&mut self` 作用域里先发一次(这时候 `plan.pairs` 还没算出来,只能用 `clip.items.len()` 估个数,跟最终 `plan.pairs.len()` 会因为跳过/去重差一点,措辞上说清楚是「打算处理几项」不是「正在处理几项」);要么把 `SftpOpDone` 之外再加一个进度事件。两种都行,写代码时挑一种、别悄悄两种都不做。

(d) `SftpOpDone` 的成功分支里,`OpFollow::Reveal` 那段旁边补:

```rust
                        // F220:剪切落地了 —— 清空剪贴板。**在这里**而不是
                        // 发出那一刻:非成功意味着东西压根没挪走(T11)。
                        if follow == OpFollow::ClearClip {
                            if let Some(files) = self
                                .tabs
                                .by_generation_mut(generation)
                                .and_then(|t| t.content.files_panel_mut())
                            {
                                files.clip = None;
                            }
                        }
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib 2>&1 | grep -E "test result|FAILED"`

- [ ] **Step 5: 变异验证 + 提交**

两条注释里的「自证会变红」逐条做。

```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): 执行远端粘贴,剪切落地后才清剪贴板 (F220)"
```

---

## Task B8: 阶段 B 收口 —— spec 条目 + 全量绿

**Files:**
- Modify: `spec.md`

- [ ] **Step 1: 写 spec 条目**

```
| F220 | **远端内复制 / 剪切 / 粘贴**:远端栏 `Ctrl+C`/`Ctrl+X`/`Ctrl+V` 与右键三项,剪贴板 **per-tab**(切到别的标签就是空的),剪切后源行画淡。粘贴 = 自身/子孙闸门 → 列一次目标目录做预检查 → 有同名才弹批量框(覆盖 / 跳过同名 / 保留两者,对整批生效)→ `exec cp -a`/`mv` 快路径,被拒回退 SFTP 逐文件递归。同目录复制直接走「保留两者」,同目录剪切是空操作 | P1 | **自身/子孙闸门必须排在任何请求之前**:远端 `cp` 自己会拦,但 SFTP 回退是我们写的递归,会边列源边往源子孙里写、一直递归到磁盘满;判据是字节前缀 + `/` 边界(`/a/bb` 不是 `/a/b` 的子孙)。**「跳过同名」不用 `cp -n`**:coreutils 9.2 反转过跳过时的退出码(9.3 又改回),撞上那个版本会被 `ExecOutcome::succeeded` 判成失败;改成客户端按预检查结果过滤。**「保留两者」同一批里的新名字要互相避让**,否则两条都叫 `a (副本).txt`、后一条盖掉前一条。**绝不跟随符号链接**(同 F57),回退路上用 `read_link` + `symlink` 原样重建。**剪贴板只在剪切成功之后才清**(T11:非成功意味着压根没挪走)。冲突框的取消 = 整批不动(`cancel_op` 归 `None` 组,与 `Conflict`/`EditConflict` 正相反) |
```

- [ ] **Step 2: 全量跑绿**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
cargo fmt --check
```

- [ ] **Step 3: 提交**

```bash
git add spec.md
git commit -m "docs: spec 补 F220(远端内复制/剪切/粘贴)"
```

---

# 阶段 C —— 交付

## Task C1: 发版 v0.1.95

- [ ] **Step 1: 走发版一条龙**

改动落在 `mullion-app`,按项目交付约定执行 —— **不要凭记忆做**,加载 skill:

```
Skill(release-windows)
```

它会带着做:升 patch 版本号 → 跑绿 → 交叉编译 + objdump 验收 → 签名 → 发 GitHub Release(走 socks 代理,本机 DNS 解析不了 github)。

- [ ] **Step 2: 写人工验收清单**

Release notes 里必须写明**无头容器验不了、需要人工确认**的部分:

1. 远端栏 `Ctrl+N` / 右键「新建文件」→ 列表第一行出现输入框,打字**零等待**(高延迟链路上尤其要看这条),回车后新文件出现且被选中。
2. Esc 放弃 → 远端目录里**没有**多出任何东西。
3. 已有同名文件时输入那个名字 → 输入框红框 + 悬停说明,回车不提交。
4. 选几个文件 `Ctrl+C` → 切到另一个远端目录 `Ctrl+V` → 内容与权限都对;大目录(如 `node_modules`)应当是**秒级**完成(说明走了 exec 快路径)。
5. `Ctrl+X` 之后源行**画淡**,粘贴成功后源消失、剪贴板清空。
6. 目标目录有同名 → 冲突框列出撞了哪几条;三个选项各试一次(尤其「保留两者」的 `xxx (副本).txt` 命名在 Windows 上显示正常,不是豆腐块)。
7. 试着把一个目录粘到它自己的子目录里 → 应当**立刻**被拒(一条 toast),而不是卡住或写满磁盘。
8. 跨挂载点剪切(源和目标在不同的 mount)→ 应当成功(EXDEV 回退)。
9. 切到另一个标签 → 剪贴板是空的、「粘贴」置灰并说明理由。

---

## 自查记录(写计划时做过的核对)

- **spec 覆盖**:五条边界 B1–B5 各有落点(B1→B4 剪贴板 per-tab/Task B4;B2→Task B3;B3→Task B5+B6;B5→Task A2/A3)。spec 第四节列的测试逐条对应到 Task A1/A2/A3/A5/A6/B1/B3/B5/B7。spec 第五节 → Task C1。
- **类型一致性**:`OpFollow` 在 Task A6 定义、Task B7 扩一档;`Policy`/`RemoteClip`/`ClipMode`/`PastePlan` 在 Task B1 定义,B5/B6/B7 沿用同名;`FileAction::BeginNewFile`(开输入框)与 `FileAction::NewFile`(提交)是两个变体,别混。
- **已知会需要临场对齐的地方**(计划里都标了「先读再写」):`state.rs` 测试模块现有的助手名、`rename_harness` 那套驱动、`nested_tree()` 里的实际路径、russh-sftp 的 `symlink` 参数顺序、`RemoteFile` 的收尾方法名。这些一律**读了再写**,不凭记忆。
