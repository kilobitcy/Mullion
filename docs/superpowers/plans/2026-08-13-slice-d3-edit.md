# 切片 D3：编辑（F53）实施计划

> **For agentic workers:** 逐任务执行，每任务自带 TDD 步骤与提交。步骤用 `- [ ]` 勾选。

**Goal:** 远端文件能改——右键「外部编辑」把文件下到临时目录、用系统默认程序打开、
改完自动回传（回传前查远端有没有被别人动过）；右键「内置编辑」在窗口里直接改小文本文件。

**Architecture:** 编辑**不走传输队列**，自成一条一次性读/写通路（理由见 D3-1）。
纯逻辑（文本探测、临时路径、编辑会话表）落在新 crate 模块 `mullion-app/src/edit/`，
零 egui / 零 tokio；协议层在 `mullion-ssh::sftp` 加两个全量读写方法；
UI 是一个编辑器窗口 + 一条「编辑中」底部面板；接线全在 `app.rs`。

**Tech Stack:** russh-sftp 2.4.0、egui 0.30、winit 0.30。**不引入新依赖**。

---

## 这一片新定死的设计决策

设计文档 `docs/superpowers/specs/2026-08-12-sftp-browser-design.md` 的 D13/D20 是骨架，
下面是实施期必须补齐的判断，每条都写清代价：

| # | 决策 | 理由 |
|---|---|---|
| D3-1 | 编辑**不进传输队列**，独立的一次性读/写 | 临时路径是我们自己造的，冲突/重名/Windows 非法名那一整套语义全不适用，硬塞进 `plan_transfer` 要给每条规则开例外；而且传输面板是「用户发起的传输」的账本，混进「打开文件」会让「全部取消」的语义变歧义 |
| D3-2 | 内置编辑 >1 MB 拒绝（D20）；**外部编辑 >64 MB 拒绝** | 两条通路都是整个读进内存。64 MB 之上应当走「下载到本地」再自己开 |
| D3-3 | 二进制判定在**读回内容之后**，菜单项只按 `size` 置灰 | 右键那一刻手上只有 `Entry`（名字/大小/权限），没有内容。设计原文说「读到 NUL 即置灰」在时序上做不到，落地成「打开后拒绝并说明」 |
| D3-4 | 换行符混用：**可编辑，但保存前必须显式选一种**，界面写明会统一 | 设计原文「只改编辑的行」需要行级 diff，egui `TextEdit` 只给整块 `String`，给不出。改成显式选择——保留功能（比只读有用），且不静默转换 |
| D3-5 | 内容非 UTF-8 → **只读**打开并标注（D20 原文） | 猜错编码写回去 = 静默毁文件 |
| D3-6 | BOM 读到就保留，写回原样带上 | 少了 BOM 的 `.bat`/`.csv` 在 Windows 侧行为会变 |
| D3-7 | 写回**直接 TRUNC 覆盖**，写前把「打开时读到的原文」写成同目录 `.mullion.bak`（默认开，编辑器窗口可关，不入库） | 同 D19：rename 会换 inode，丢属主/权限/硬链接。`.bak` 兜「写到一半断线留截断文件」 |
| D3-8 | 回传前 `stat` 远端，`mtime` 或 `size` 与打开时的快照对不上 → 不写，走冲突（保留远端 / 覆盖远端 / 另存副本） | D13 原文 |
| D3-9 | 「保留远端」= 这一次不回传 + **把快照更新成远端当前值**；本地临时文件不动 | 不更新快照的话，同一个框会在下一次轮询时再弹一遍，永远关不掉 |
| D3-10 | 监视靠**本地 mtime 轮询（1 s）**，不猜编辑器进程退没退 | D13 原文：VS Code 一个进程开一堆窗口 |
| D3-11 | 临时路径 `<data_local>/Mullion/edit/<净化会话名>/<FNV-1a 路径哈希>/<原文件名>`，哈希自己写（不加依赖） | 保留原文件名，编辑器的语法高亮才认得出类型；哈希只需「同路径同目录、不同路径不撞」 |
| D3-12 | 退出：有「已改未回传」拦一次确认；确认退出后删掉整个 `edit` 临时目录 | D13 原文 |

---

## 文件结构

新建：

- `crates/mullion-app/src/edit/mod.rs` —— 编辑会话表 `EditSessions`：登记 / 查 / 撤，
  以及「本地变没变」「远端变没变」两条纯判定。零 egui、零 tokio、零 IO。
- `crates/mullion-app/src/edit/text.rs` —— 文本探测：二进制（NUL）、UTF-8/BOM、
  换行符（LF/CRLF/混用）、大小上限。纯函数。
- `crates/mullion-app/src/edit/tempdir.rs` —— 临时路径构造 + 会话名净化 + FNV-1a 哈希
  + 清理整棵 `edit` 目录。只有清理那一个函数碰 IO。
- `crates/mullion-app/src/edit/launch.rs` —— 用系统默认程序打开一个本地文件。
  平台命令抽成可测的纯函数（同 `files::local::open_command` 的做法）。
- `crates/mullion-app/src/ui/editor_window.rs` —— 内置编辑器窗口。
- `crates/mullion-app/src/ui/edit_panel.rs` —— 底部「编辑中」列表。

修改：

- `crates/mullion-ssh/src/sftp.rs` —— `read_all` / `write_all_truncate`。
- `crates/mullion-app/src/ui/files_panel.rs` —— 两个新菜单项 + 双击文件 = 外部编辑。
- `crates/mullion-app/src/ui/files_dialog.rs` —— `FilesDialog::EditConflict` 三选。
- `crates/mullion-app/src/ui/mod.rs` —— `UiActions`/`UiState` 新字段 + 接线。
- `crates/mullion-app/src/app.rs` —— 新 `UserEvent`、异步任务、1 s 轮询、退出拦截。
- `crates/mullion-app/src/lib.rs` —— `pub mod edit;`

---

## Task 1：`mullion-ssh` 的全量读写

**Files:** `crates/mullion-ssh/src/sftp.rs`

- [ ] **Step 1**：写失败测试 `reading_more_than_the_limit_is_refused_instead_of_filling_memory`
      （纯逻辑部分：`over_limit` 判定）与 `sftp` live 侧不可测部分留给 Task 12 的假服务端。
- [ ] **Step 2**：`cargo test -p mullion-ssh` 确认红。
- [ ] **Step 3**：实现

```rust
/// 一次把整个远端文件读进内存(F53 编辑用)。**带上限**——编辑通路的两条
/// 大小闸门(内置 1 MB / 外部 64 MB)最终都落在这里,漏了就是拿一个 8 GB 的
/// core dump 把自己 OOM 掉。
pub async fn read_all(&self, path: &RemotePath, limit: u64) -> Result<Vec<u8>, SftpError> { .. }

/// 覆盖写回。**TRUNC 直接写目标**,不走临时文件 + rename(设计 D19/D3-7:
/// rename 换 inode,属主/权限/ACL/硬链接全丢)。
pub async fn write_all_truncate(&self, path: &RemotePath, bytes: &[u8]) -> Result<(), SftpError> { .. }
```

- [ ] **Step 4**：`cargo test -p mullion-ssh` 绿。
- [ ] **Step 5**：提交 `feat(ssh): SFTP 全量读写(带上限)供编辑通路用 (F53)`。

## Task 2：文本探测 `edit/text.rs`

**Files:** 新建 `crates/mullion-app/src/edit/text.rs`、`crates/mullion-app/src/edit/mod.rs`、
`crates/mullion-app/src/lib.rs`

- [ ] **Step 1**：写测试
  - `a_nul_byte_means_binary_so_a_png_can_never_be_edited`
  - `invalid_utf8_opens_read_only_instead_of_guessing_an_encoding`
  - `a_bom_is_kept_so_the_file_does_not_change_shape_on_save`
  - `mixed_line_endings_are_reported_so_the_user_can_pick_one`
  - `encoding_back_restores_the_original_bytes_when_nothing_was_edited`（往返）
- [ ] **Step 2**：跑红。
- [ ] **Step 3**：实现

```rust
pub enum Eol { Lf, Crlf, Mixed }
pub struct Probe { pub binary: bool, pub utf8: bool, pub bom: bool, pub eol: Eol }
pub fn probe(bytes: &[u8]) -> Probe;
/// 解码成编辑器用的文本:**换行统一成 `\n`**(egui 只认它),BOM 去掉。
pub fn decode(bytes: &[u8], p: &Probe) -> String;
/// 编回字节:按 `eol` 还原换行,按 `bom` 还原 BOM。
pub fn encode(text: &str, eol: Eol, bom: bool) -> Vec<u8>;
```

- [ ] **Step 4**：绿。**变异验收**：把 `binary` 恒 `false`、把 `encode` 的 BOM 分支删掉，
      各自确认变红（备份用 `cp`，回滚也用 `cp`）。
- [ ] **Step 5**：提交 `feat(app): 编辑通路的文本探测——二进制/编码/BOM/换行 (F53)`。

## Task 3：临时路径 `edit/tempdir.rs`

- [ ] **Step 1**：测试
  - `two_different_remote_paths_never_share_a_temp_directory`
  - `the_original_file_name_is_kept_so_the_editor_can_highlight_it`
  - `a_session_name_with_windows_illegal_characters_is_sanitized`
- [ ] **Step 2**：红 → **Step 3** 实现 `temp_path(root, session, remote) -> PathBuf`、
      `sanitize`、`fnv1a64`、`purge(root)`。
- [ ] **Step 4**：绿 + 变异（哈希恒 0 → 撞目录测试变红）。
- [ ] **Step 5**：提交。

## Task 4：外部程序启动 `edit/launch.rs`

- [ ] **Step 1**：测试 `the_open_command_passes_the_path_as_a_single_argument`
      （同 `local::open_command` 的判据：绝不拼 shell 串）。
- [ ] **Step 2~4**：实现 `open_command(path) -> (String, OsString)` +
      `open_with_default(path) -> Result<(), String>`。Windows 走
      `cmd /c start "" <path>`（`explorer.exe <file>` 对无关联类型不报错也不开），
      macOS `open`，其余 `xdg-open`。
- [ ] **Step 5**：提交。

## Task 5：编辑会话表 `edit/mod.rs`

- [ ] **Step 1**：测试
  - `a_local_change_is_detected_by_mtime_and_size_not_by_a_process_exit`
  - `a_remote_change_since_open_blocks_the_write_back`
  - `keeping_the_remote_version_refreshes_the_snapshot_so_the_dialog_does_not_reopen`（D3-9）
  - `ending_an_edit_removes_it_from_the_watch_list`
- [ ] **Step 2~4**：实现

```rust
pub type EditKey = u64;
pub enum EditKind { External, Inline }
pub struct EditEntry {
    pub key: EditKey, pub generation: u64, pub kind: EditKind,
    pub remote: RemotePath, pub local: PathBuf, pub label: String,
    /// 打开(或上次成功回传)那一刻远端的 mtime/size。回传前拿它跟远端比。
    pub snapshot: (u32, u64),
    /// 上次看到的本地 mtime/len。变了才发起回传。
    pub seen: Option<(u64, u64)>,
    pub state: EditState,
}
pub enum EditState { Watching, Uploading, Conflict, Failed(String), Saved }
pub struct EditSessions { .. }  // add / get / remove / iter / by_local_change
```

- [ ] **Step 5**：提交。

## Task 6：内置编辑器窗口 `ui/editor_window.rs`

- [ ] **Step 1**：测试（两帧渲染取文本，同 `files_dialog` 的 harness）
  - `a_read_only_file_says_why_instead_of_silently_dropping_edits`
  - `a_dirty_buffer_marks_the_title_so_the_user_can_see_unsaved_work`
  - `a_mixed_eol_file_forces_an_explicit_choice_before_saving`（D3-4）
- [ ] **Step 2~4**：实现 `EditorState` + `show(ctx, t, &mut Option<EditorState>) -> Option<EditorAction>`，
      `EditorAction::{Save, Close, ToggleBackup}`。
- [ ] **Step 5**：提交。

## Task 7：底部「编辑中」面板 `ui/edit_panel.rs`

- [ ] 空表不画（同 `transfer_panel`，不偷终端行数）。每条：文件名 + 状态 +
      「结束编辑」按钮；冲突态额外一句可读原因。
- [ ] 测试 `an_empty_edit_list_draws_nothing`、
      `a_conflicted_entry_says_what_to_do_instead_of_just_turning_red`。
- [ ] 提交。

## Task 8：菜单项与双击语义

**Files:** `crates/mullion-app/src/ui/files_panel.rs`

- [ ] `MenuItem::{EditExternal, EditInline}` + `FileAction::{EditExternal, EditInline}`。
      只在**远端栏**且**有光标行**时出现（本地文件双击资源管理器就够了，D5）。
- [ ] 双击**文件**（`enter_target` 给不出目标的那一类）→ `FileAction::EditExternal`。
      双击目录/链接照旧进目录。
- [ ] 测试
  - `double_clicking_a_plain_file_opens_the_external_editor_instead_of_doing_nothing`
  - `the_local_column_never_offers_a_remote_edit_entry`
- [ ] 提交。

## Task 9：回传冲突对话框

**Files:** `crates/mullion-app/src/ui/files_dialog.rs`

- [ ] `FilesDialog::EditConflict { name, key }` + `FileOp::ResolveEdit { key, choice }`，
      `EditResolve::{KeepRemote, Overwrite, SaveCopy}`。
- [ ] 「覆盖远端」标危险色；关框（点取消）= `KeepRemote`（**不能只关框**，
      同 F55 那条：留着 `Conflict` 状态队列再也走不动）。
- [ ] 测试三个按钮各自的 `FileOp`，以及「取消也要结掉」。
- [ ] 提交。

## Task 10：`app.rs` 接线——打开

- [ ] 新 `UserEvent::EditOpened { generation, key, result: Result<EditOpen, String> }`
      （`EditOpen { bytes, snapshot }`）。
- [ ] `App::start_edit(generation, kind)`：取光标行 → `is_operable` + 大小闸门
      （内置 1 MB / 外部 64 MB）→ spawn 一条 sftp channel 读 → 回送。
- [ ] 收到后：
  - 内置：`probe` → 二进制拒绝 / 非 UTF-8 只读 → 开窗口。
  - 外部：写临时文件 → `open_with_default` → 登记进 `EditSessions`。
- [ ] 测试（源码守护）`edit_open_events_never_request_a_redraw_directly`（T3 同款）
      与 `a_binary_file_is_refused_with_a_reason_not_opened_as_mojibake`。
- [ ] 提交。

## Task 11：`app.rs` 接线——监视与回传

- [ ] 1 s 轮询：`RedrawRequested` 里若 `edits` 非空则 `poll_edits()` +
      排 `WaitUntil(now + 1s)`，**那一支同样显式复位 `control_flow`**（T7）。
- [ ] `poll_edits`：本地 mtime/len 变了 → 置 `Uploading` → spawn：
      `stat` 远端 → 与 `snapshot` 比 → 一致就 `write_all_truncate`（先写 `.mullion.bak`）
      → 回送 `UserEvent::EditSaved { key, result }`；不一致 → 回送 `Conflict`。
- [ ] `FileOp::ResolveEdit` 三条分支落地（`KeepRemote` 要刷新快照，D3-9）。
- [ ] 内置编辑器点「保存」走同一条回传路径（**同一个函数**，不另写一份——
      两份写回逻辑里必然有一份漏掉 stat 检查）。
- [ ] 测试
  - `a_remote_change_since_open_never_overwrites_and_asks_instead`（守护测试 8）
  - `saving_writes_a_backup_next_to_the_file_before_truncating_it`
  - `the_write_back_path_is_shared_by_both_editors_so_the_stat_check_cannot_be_skipped`（源码守护）
- [ ] 提交。

## Task 12：假 SFTP 服务端端到端（守护测试 8/9）

**Files:** `crates/mullion-ssh/tests/`（沿用 D1/D2 的进程内假服务端）

- [ ] 打开→改远端→回传：必须走冲突分支，远端内容**没被覆盖**。
- [ ] `read_all` 超限拒绝。
- [ ] 提交。

## Task 13：退出拦截与清理

- [ ] `CloseRequested`：`edits` 里有 `Uploading`/`Conflict`/本地改了没传的 → 弹确认，
      不退出；确认后 `tempdir::purge` 再 `event_loop.exit()`。
- [ ] 测试 `a_pending_edit_blocks_the_first_close_request`（纯判定函数 `blocks_exit`）。
- [ ] 提交。

## Task 14：交付

- [ ] `cargo test --workspace` + `clippy -D warnings` + `fmt --check` 全绿。
- [ ] `chore: 版本 0.1.35(…)`。
- [ ] 交叉编译 + objdump 验收 + 发 Release `v0.1.35`（notes 带人工验收清单）。
