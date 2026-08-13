# 切片 D4a：栏间拖拽 + 从资源管理器拖入（F58 拖拽部分 / F52）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task.
> Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让文件面板的两栏之间可以互拖即传输，并让资源管理器里的文件能拖进窗口上传。

**Architecture:** 拖拽的**判据全部落在 `files/drag.rs` 的纯函数里**（谁能拖、拖到哪儿、
落点算哪个目录、拖入的一批绝对路径怎么摊成「按父目录分组的上传批次」），egui 那层只
负责「把 payload 挂上去 / 把落点读回来」。传输本身**不新开通路**——复用 D2 已有的
`plan_transfer` + 队列，只是把「目标目录」和「源集合」从「两栏的 cwd + 选中集」放宽成
参数。

**Tech Stack:** egui 0.30 的 `dnd_set_drag_payload` / `dnd_release_payload`（`Response`
上的方法，`egui-0.30.0/src/response.rs:420/453`）、winit `DroppedFile`/`HoveredFile`
经 `egui-winit-0.30.0/src/lib.rs:423-442` 落进 `RawInput.hovered_files`/`dropped_files`。
**不引新依赖。**

---

## 范围：为什么把 D4 劈成 D4a / D4b

设计文档（`docs/superpowers/specs/2026-08-12-sftp-browser-design.md`）的 D4 是「拖拽三类」
一片：栏间 → 拖入 → 拖出。**拖出（F59）单独成片 D4b**，理由：

- 前两类是纯 egui / winit，跨平台、无头可测（egui 的 `RawInput` 能直接构造拖入事件）。
- 拖出是 Windows-only COM：手写 `IDataObject`/`IEnumFORMATETC`/`IStream`/`IDropSource`
  四个接口 + 专用 STA 线程 + free-threaded marshaler，**无头一行都验不了**（设计 D10/D12
  已明确记录）。它跟前两类没有共享代码，混在一片里只会让「拖拽不好使」这句反馈没法定位
  到底是哪一类坏了。
- 交付约定是一片一 Release。D4a 先发，用户能立刻验「互拖传输」这个高频动作。

## 拍板：落在**目录行**上松手 = 传进那个目录

spec F58 原文括号里写的是「拖到目录行进该目录」。字面可以读成 spring-loaded folder
（悬停一会儿自动进入），也可以读成「投进那个目录」。**本切片取后者**：

- 资源管理器 / Finder / 各家 FTP 客户端的通用语义就是「落到文件夹图标 = 放进去」。
- spring-loaded 需要悬停计时器，而且进去之后手上还拖着东西、动作没有终点——用户还得
  再松一次手，多一步。
- 「进该目录」这个能力已经有了（双击 / Enter / 面包屑），不缺这一个入口。

**这条要进人工验收清单请用户确认**——如果他要的是 spring-loaded，改动只在
`drag::drop_target` 一个函数里。

## 三条不做

- **不做拖出到资源管理器**——D4b。
- **不做拖拽重排标签**——设计 D3 已排除。
- **拖入不判落点**——设计 D9：winit 0.30 的 `DroppedFile` 不带坐标，Windows 在 OLE
  拖放期间不发 `CursorMoved`，最后已知指针位置不可靠。一律上传到**远端栏当前目录**，
  并在拖入悬停时把这条规则明写在远端栏顶部。

---

## 文件结构

| 文件 | 职责 |
|---|---|
| `crates/mullion-app/src/files/drag.rs`（新建） | 拖拽的**全部判据**，纯函数 + 纯数据，零 egui |
| `crates/mullion-app/src/files/mod.rs` | `pub mod drag;` |
| `crates/mullion-app/src/ui/files_panel.rs` | 行挂 drag payload；栏读落点；拖入横幅 |
| `crates/mullion-app/src/app.rs` | `FileAction::Drop` / `DropIn` 接线到传输 |
| `crates/mullion-app/src/ui/mod.rs` | 把 `DroppedFile`/`HoveredFile` 从 `ctx.input` 取出来交给面板 |

---

### Task 1：`files/drag.rs` —— 拖拽载荷与落点判据

**Files:**
- Create: `crates/mullion-app/src/files/drag.rs`
- Modify: `crates/mullion-app/src/files/mod.rs`

- [ ] **Step 1: 写失败的测试**

```rust
#[test]
fn dragging_within_the_same_column_is_not_a_transfer() {
    // 远端栏内部把文件拖到另一行 —— 那是「移动/改名」，本切片不做。
    assert_eq!(drop_target(PanelColumn::Remote, PanelColumn::Remote, None), None);
}

#[test]
fn dropping_on_a_directory_row_targets_that_subdirectory() {
    let t = drop_target(PanelColumn::Local, PanelColumn::Remote, Some(b"logs".to_vec()));
    assert_eq!(t, Some(Some(b"logs".to_vec())));
}

#[test]
fn dropping_on_blank_space_targets_the_current_directory() {
    assert_eq!(drop_target(PanelColumn::Local, PanelColumn::Remote, None), Some(None));
}
```

- [ ] **Step 2: 跑，确认因为 `drop_target` 不存在而失败**

`cargo test -p mullion-app drag:: 2>&1 | tail -20`

- [ ] **Step 3: 写实现**

```rust
//! 拖拽的判据（F58/F52）。**零 egui** —— egui 那层只负责挂 payload、读落点，
//! 「这一拖成不成立、落到哪个目录」全在这里，才能脱离窗口单测。

use crate::ui::files_panel::PanelColumn;

/// 一次栏间拖拽的载荷。**只带「从哪栏拖的」** —— 拖的是哪几条不进 payload：
/// 源栏的选中集是 `PaneState` 里的真值，松手那一刻现取即可；抄一份进 payload
/// 会让「拖动过程中选中集变了」出现两个互相矛盾的答案。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DragFrom(pub PanelColumn);

/// 松手落在某处时该传到哪儿。
///
/// 返回值三态：
/// - `None`：这一拖不成立（同栏内拖）。
/// - `Some(None)`：传到目标栏的**当前目录**。
/// - `Some(Some(name))`：传到目标栏当前目录下的**子目录 `name`**。
///
/// `over_dir`：松手那一刻指针底下那一行的名字，**且它是个目录**；文件行/空白
/// 都给 `None`（落在文件上不是「覆盖那个文件」，那种语义太容易误操作）。
pub fn drop_target(
    from: PanelColumn,
    onto: PanelColumn,
    over_dir: Option<Vec<u8>>,
) -> Option<Option<Vec<u8>>> {
    if from == onto {
        return None;
    }
    Some(over_dir)
}
```

- [ ] **Step 4: 跑，确认通过**
- [ ] **Step 5: 提交** `feat(app): 栏间拖拽的落点判据 (F58)`

---

### Task 2：拖入的一批绝对路径 → 按父目录分组的上传批次

**Files:** `crates/mullion-app/src/files/drag.rs`

现有 `plan_transfer(dir=Upload, picked=[(name, is_dir, size)], remote_cwd, local_cwd)`
的 `picked` 是**相对于 `local_cwd` 的名字**。而拖入来的是任意绝对路径，可能来自好几个
不同的目录。**不改 `plan_transfer`**（它被右键上传那条路径共用，改它等于同时动两条），
而是把拖入的一批路径摊成「若干个 (父目录, 名字列表)」，逐组调用。

- [ ] **Step 1: 写失败的测试**

```rust
#[test]
fn dropped_paths_are_grouped_by_their_parent_directory() {
    let g = group_by_parent(&[
        PathBuf::from("/a/x.txt"),
        PathBuf::from("/b/y.txt"),
        PathBuf::from("/a/z.txt"),
    ]);
    assert_eq!(g.len(), 2, "两个父目录 → 两组");
    let a = g.iter().find(|(p, _)| p == Path::new("/a")).expect("/a 那组在");
    assert_eq!(a.1.len(), 2, "/a 下的两条并在一组，不是两次单独上传");
}

#[test]
fn a_dropped_path_without_a_parent_is_skipped_rather_than_uploaded_to_nowhere() {
    // 盘符根（Windows 的 `C:\`、Unix 的 `/`）没有父目录也没有文件名。
    let g = group_by_parent(&[PathBuf::from("/")]);
    assert!(g.is_empty(), "没有文件名的路径不该变成一条上传");
}
```

- [ ] **Step 2: 跑，确认失败**
- [ ] **Step 3: 实现**

```rust
/// 把拖进来的一批绝对路径按**父目录**分组，每组给 `(父目录, 名字列表)`。
///
/// 顺序稳定（按首次出现的父目录排）：拖 5 个文件进来，队列里的顺序要跟用户
/// 选的顺序对得上，用 `HashMap` 迭代出来的随机序会让每次都不一样。
pub fn group_by_parent(paths: &[PathBuf]) -> Vec<(PathBuf, Vec<Vec<u8>>)> { ... }
```

- [ ] **Step 4: 跑，确认通过**
- [ ] **Step 5: 提交** `feat(app): 拖入路径按父目录分组 (F52)`

---

### Task 3：行变拖源、栏变落区

**Files:** `crates/mullion-app/src/ui/files_panel.rs`

- [ ] **Step 1**：`FileAction` 加一条

```rust
    /// F58：从另一栏拖过来松手了。`Some(name)` = 落在名为 `name` 的目录行上，
    /// 传进那个子目录；`None` = 落在空白/列头，传到当前目录。
    ///
    /// **方向由收到这个动作的栏决定**（远端栏收到 = 上传，本地栏收到 = 下载），
    /// 跟 `Transfer` 那条正好相反 —— `Transfer` 是「把我这栏的东西送出去」，
    /// `Drop` 是「把别人的东西收进来」。
    Drop(Option<Vec<u8>>),
```

- [ ] **Step 2**：`row()` 的返回值改成带 `Sense::click_and_drag()`，并在 `show()` 里
      对**已选中的行**挂 payload：

```rust
    // 拖源：只有「已经选中的行」能起拖 —— 拖一条没选中的行，用户以为拖的是
    // 这一条，实际传的是别处那批选中项（同右键菜单那条已知陷阱）。
    if resp.drag_started() && selected {
        resp.dnd_set_drag_payload(crate::files::drag::DragFrom(column));
    }
```

- [ ] **Step 3**：栏级落区。松手那一刻先看**行**（落在目录行上），再看**栏**：

```rust
    // 落区：先问行，再问栏。次序反了的话，落在目录行上会被栏那一份先吃掉，
    // 「传进子目录」永远走不到。
    if let Some(from) = resp.dnd_release_payload::<DragFrom>() {
        if drop_target(from.0, column, dir_name_of(e)).is_some() { ... }
    }
```

- [ ] **Step 4**：写 egui 测试（两帧！Panels 第一帧只记 `Shape::Noop`，见
      `docs/gui-render-gotchas.md`），驱动一次「本地栏起拖 → 远端栏松手」。
- [ ] **Step 5: 提交** `feat(app): 两栏互拖即传输 (F58)`

---

### Task 4：app 侧接线 —— 带目标目录的传输

**Files:** `crates/mullion-app/src/app.rs`

- [ ] **Step 1**：把 `start_transfer(generation, dir)` 泛化成
      `start_transfer_into(generation, dir, into: Option<Vec<u8>>)`，`into` 是目标栏
      当前目录下的子目录名。原 `start_transfer` 保留为 `into = None` 的薄封装 ——
      右键「下载到本地」那条路径一个字不动。

- [ ] **Step 2**：`apply_remote_file_action` / `apply_local_file_action` 各接一条
      `FileAction::Drop(into)`：远端栏收到 → `Direction::Upload`；本地栏收到 →
      `Direction::Download`。

- [ ] **Step 3**：守护测试——**方向不能弄反**。

```rust
#[test]
fn a_drop_onto_the_remote_column_uploads_rather_than_downloads() { ... }
```

- [ ] **Step 4: 提交** `feat(app): 拖拽落点接进传输队列 (F58)`

---

### Task 5：F52 从资源管理器拖入

**Files:** `crates/mullion-app/src/ui/mod.rs`、`crates/mullion-app/src/ui/files_panel.rs`、
`crates/mullion-app/src/app.rs`

- [ ] **Step 1**：`build_ui` 从 `ctx.input(|i| (i.raw.hovered_files.len(), 取 dropped 的 path))`
      读出来，交给 `sidebar`/`content`。

- [ ] **Step 2**：悬停时远端栏描边 + 顶部一行明写「松开上传到 /path/to/cwd」。
      **规则先于动作可见**（设计 D9）：拖到窗口任何位置都是上传到远端栏当前目录，
      这句话必须在用户松手**之前**就在屏幕上。

- [ ] **Step 3**：`UiActions` 加 `files_drop_in: Vec<PathBuf>`，`has_real_action` 补一条
      （**必须**——`app.rs:5242` 那条注释记着「曾发现删掉 `files_remote` 这一条，662 个
      既有测试全绿」）。

- [ ] **Step 4**：守护测试——构造一帧带 `dropped_files` 的 `RawInput`，断言产生了
      **上传到远端栏 cwd** 的动作；另一个断言「面板没打开时拖入不产生任何动作」。

- [ ] **Step 5: 提交** `feat(app): 从资源管理器拖入即上传到远端当前目录 (F52)`

---

### Task 6：变异验收 + 绿门 + 发版

- [ ] **变异 1**：`drop_target` 去掉 `from == onto` 那条 → 同栏拖测试必须变红。
- [ ] **变异 2**：Task 4 里把远端栏的方向改成 `Download` → 方向守护测试必须变红。
- [ ] **变异 3**：`group_by_parent` 用 `HashMap` 迭代序 → 顺序断言必须变红。
- [ ] **变异 4**：`has_real_action` 删掉 `files_drop_in` 那条 → 拖入守护测试必须变红。
- [ ] **变异 5**：拖源那里把 `&& selected` 去掉 → 「拖未选中行」的守护测试必须变红。

每条都用 `cp <file> /tmp/x.bak` 备份、`cp /tmp/x.bak <file>` 还原。
**绝不用 `git checkout <file>`**（D2 的教训：会连未提交的改动一起抹掉）。

- [ ] 绿门：`cargo test --workspace` + `cargo clippy --workspace --all-targets -- -D warnings`
      + `cargo fmt --check`
- [ ] `chore: 版本 0.1.36(…)`
- [ ] 交叉编译 + objdump 验收 + GitHub Release + 人工验收清单
