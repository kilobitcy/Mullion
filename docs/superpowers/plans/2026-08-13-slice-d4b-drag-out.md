# 切片 D4b：拖出到资源管理器（F59）实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 从 Mullion 的远端栏把选中的文件拖到资源管理器 / 桌面 / 聊天软件，落地成真实文件；
下载在**目标程序要数据时**才发生（虚拟文件、延迟渲染），不预先落盘、UI 全程不冻。

**Architecture:** 三层。判据与字节布局落在**零 COM 的纯逻辑层**（`dragout/`），Windows COM
实现单独一个 `#[cfg(windows)]` 模块，非 Windows 给一个只记日志的 stub。手势判定在
`ui::files_panel`，只负责「什么时候把这一拖交给操作系统」。

**Tech Stack:** `windows` 0.59 + `windows-implement` / `windows-interface`（**已在依赖树里**，
wgpu/winit 的传递依赖，`x86_64-pc-windows-gnu` 下已编译通过——不引第三个版本）；
`tokio::runtime::Handle::block_on`（从 COM 的外部线程回到我们的 runtime 读 SFTP）。

---

## 前置：这一片为什么单独拆出来

D 系列原定 **D4 拖拽三类**（栏间 / 拖入 / 拖出）。前两类跨平台、无头可测，已在 D4a
（v0.1.36）交付。拖出这一类：

- 纯 Windows COM，**无头环境一行都验不了**（设计文档 D10 原话）；
- 失败发生在**别人的进程里**（目标程序只认 `CF_HDROP` 时会落 0 字节文件），
  唯一诊断手段是 D12 的日志；
- 风险最高、最可能返工。

混在 D4a 里发版会让「拖拽能用了」这句话真假掺半。

## 已定的设计（来自 `docs/superpowers/specs/2026-08-12-sftp-browser-design.md`）

- **D10 绝不能在 winit 回调栈内启动 `DoDragDrop`。** `winit-0.30.13/src/platform_impl/
  windows/event_loop/runner.rs:208` 对 `RedrawRequested` 绕过事件缓冲直调 handler，
  而 `call_event_handler` 里是 `event_handler.take().expect(...)`。`DoDragDrop` 是
  嵌套模态消息循环，期间必然有 WM_PAINT → 进程 panic。**这不是概率问题。**
  → 单开一条 **STA 线程**：`OleInitialize` → `DoDragDrop` → `OleUninitialize`。
- **D11 虚拟文件（延迟渲染）**：`CFSTR_FILEDESCRIPTORW` + `CFSTR_FILECONTENTS`，
  目标程序落下时才向我们要 `IStream`，边下边喂。`CoCreateFreeThreadedMarshaler`
  让目标进程在**它自己的线程**直接调 `IStream::Read`。
- **D12 全流程日志**，target `mullion::sftp::drag_out`。

## 本片新决的三条（设计文档没拍板的）

### N1 手势：**指针离开窗口**才把这一拖交给操作系统

冲突：D4a 的 F58「远端栏拖到本地栏 = 下载」已经占用了「在远端栏行上按住拖」这个手势。
若远端栏一起拖就立刻 `DoDragDrop`，OS 会接管鼠标捕获，F58 的远端→本地方向**当场失效**。

- 否掉「用修饰键区分」：不可发现，用户不会知道要按住 Alt。
- 否掉「远端栏一律走 OS 拖出」：牺牲刚交付并已进 main 的 F58 一半功能。
- **采用**：远端栏起拖照旧挂 egui payload（F58 不变）；**一旦指针移出窗口边界**
  且 egui 拖拽仍在进行 → 才启动 OS 拖出。窗口内 = 内部传输，窗口外 = 交给系统，
  语义直白。

代价（写进人工验收清单）：`DoDragDrop` 是在鼠标已按下之后才接管的，目标程序看到拖拽
的时刻比「从资源管理器起拖」晚一点；跨窗口的 drop 高亮可能有一帧延迟。

### N2 只拖**普通文件**，不拖目录

目录要另造一层 `FILEDESCRIPTORW`（`FILE_ATTRIBUTE_DIRECTORY` + 逐个子项描述符），
而子项要在**起拖那一刻**就递归列完远端目录才知道有几项——那是一次可能几十秒的网络
往返，卡在手势里。选中集里的目录直接**跳过**，不是静默：栏顶提示写明「N 项文件
（目录已跳过）」。

### N3 落地名要**净化 + 去重**

远端名是字节真源，可能含 Windows 非法字符（`\ / : * ? " < > |`）、控制字符、以尾随
点/空格结尾，或撞上设备名（`CON`/`NUL`/`COM1`…）。净化之后**两个不同的远端名可能撞成
同一个 Windows 名**（`a:b` 和 `a?b` 都变 `a_b`），资源管理器会拿后一个盖掉前一个。
所以净化之后必须在这一批内部去重（`a_b`、`a_b (2)`）。

这一整条是**纯逻辑、可无头单测**，也是这片里唯一能真正验证的部分之一。

---

## 文件结构

| 文件 | 职责 |
|---|---|
| `crates/mullion-app/src/dragout/mod.rs`（新建） | 平台分派入口 `start(...)` + 手势判据 `should_hand_off` + `DragOutItem` / `items_for` |
| `crates/mullion-app/src/dragout/name.rs`（新建） | N3：Windows 文件名净化 + 批内去重。零平台代码 |
| `crates/mullion-app/src/dragout/descriptor.rs`（新建） | `FILEGROUPDESCRIPTORW` 的**字节布局**。零平台代码，字节数组可直接断言 |
| `crates/mullion-app/src/dragout/win.rs`（新建，`#[cfg(windows)]`） | COM：`IDataObject` / `IEnumFORMATETC` / `IStream` / `IDropSource` + STA 线程 + free-threaded marshaler + D12 日志 |
| `crates/mullion-app/src/lib.rs` | `pub mod dragout;` |
| `crates/mullion-app/src/ui/files_panel.rs` | N1 手势 → `FileAction::DragOut`；N2 的「目录已跳过」提示 |
| `crates/mullion-app/src/ui/mod.rs` | 把「指针在不在窗口里」喂进 `sidebar`/`content` |
| `crates/mullion-app/src/app.rs` | `apply_remote_file_action` 接 `DragOut` → `dragout::start` |
| `crates/mullion-app/Cargo.toml` | `[target.'cfg(windows)'.dependencies]` 加 `windows` 0.59 的几个 feature |
| `docs/gui-render-gotchas.md` | 补 D10 那条（winit 回调栈内不得起嵌套模态循环） |

---

## Task 1：Windows 文件名净化 + 批内去重（`dragout/name.rs`）

**Files:** Create `crates/mullion-app/src/dragout/name.rs`

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn illegal_windows_characters_become_underscores() {
    assert_eq!(sanitize("a:b*c?.txt"), "a_b_c_.txt");
}

#[test]
fn a_reserved_device_name_gets_a_prefix_so_the_file_can_exist_at_all() {
    // Windows 上 `NUL` / `CON` 这类名字建不出文件,资源管理器会静默失败。
    assert_eq!(sanitize("NUL"), "_NUL");
    assert_eq!(sanitize("com1.txt"), "_com1.txt");
    assert_eq!(sanitize("common.txt"), "common.txt", "只有恰好是设备名才加前缀");
}

#[test]
fn trailing_dots_and_spaces_are_stripped_because_windows_silently_drops_them() {
    assert_eq!(sanitize("report. "), "report");
}

#[test]
fn two_different_remote_names_that_sanitize_alike_do_not_overwrite_each_other() {
    let out = unique(&["a:b".into(), "a?b".into(), "a_b".into()]);
    assert_eq!(out, vec!["a_b", "a_b (2)", "a_b (3)"]);
}
```

- [ ] **Step 2: 跑,确认红**（函数不存在）
- [ ] **Step 3: 实现** `sanitize` / `unique`
- [ ] **Step 4: 跑,确认绿**
- [ ] **Step 5: 提交** `feat(app): 拖出落地名净化与批内去重 (F59)`

## Task 2：`FILEGROUPDESCRIPTORW` 字节布局（`dragout/descriptor.rs`）

**Files:** Create `crates/mullion-app/src/dragout/descriptor.rs`

布局（`shlobj_core.h`，全部小端、对齐 4）：

```
FILEGROUPDESCRIPTORW:
  0..4      UINT  cItems
  4..       FILEDESCRIPTORW[cItems]          每项 592 字节

FILEDESCRIPTORW:
  0..4      DWORD    dwFlags
  4..20     CLSID    clsid
  20..28    SIZEL    sizel
  28..36    POINTL   pointl
  36..40    DWORD    dwFileAttributes
  40..48    FILETIME ftCreationTime
  48..56    FILETIME ftLastAccessTime
  56..64    FILETIME ftLastWriteTime
  64..68    DWORD    nFileSizeHigh
  68..72    DWORD    nFileSizeLow
  72..592   WCHAR    cFileName[MAX_PATH]     260 个 UTF-16 码元,含结尾 NUL
```

- [ ] **Step 1: 写失败测试**（字节位置逐条断言：`cItems`、`nFileSizeLow/High`、
  名字的 UTF-16、长名截断不劈开代理对、总长度 = `4 + 592 * n`）
- [ ] **Step 2/3/4: 红 → 实现 → 绿**
- [ ] **Step 5: 提交** `feat(app): 拖出的 FILEGROUPDESCRIPTORW 字节布局 (F59)`

## Task 3：拖出项摘取 + 手势判据（`dragout/mod.rs`）

- [ ] `items_for(cwd, entries, selected) -> (Vec<DragOutItem>, usize /* 跳过的目录数 */)`
      —— 只收普通文件（N2）、只收 `is_operable()` 的名字，落地名经 Task 1 的
      `sanitize` + `unique`。
- [ ] `should_hand_off(from: Option<PanelColumn>, pointer_inside_window: bool,
      already_running: bool) -> bool`（N1）。三条测试各锁一个原因：
      本地栏不交（那是资源管理器自己的事）、指针还在窗口里不交（**否则 F58 被抢走**）、
      已经在跑不重复交（`DoDragDrop` 是模态的，重入会开出第二条线程）。
- [ ] 提交 `feat(app): 拖出项摘取与「何时交给系统」的判据 (F59)`

## Task 4：Windows COM 实现（`dragout/win.rs`）

- [ ] `Cargo.toml` 加 feature：`Win32_System_Com`、`Win32_System_Ole`、
      `Win32_System_Com_StructuredStorage`、`Win32_System_Memory`、
      `Win32_System_DataExchange`、`Win32_UI_Shell`、`Win32_Foundation`。
- [ ] `SftpStream`：`impl IStream` + `ISequentialStream::Read`，内部
      `Handle::block_on(remote_file.read_chunk(..))`，用 `Mutex` 包状态
      （free-threaded 之后调用方线程不可控）。
- [ ] `Items`：`impl IDataObject`。`GetData` 认两种格式：
      `CFSTR_FILEDESCRIPTORW`（`TYMED_HGLOBAL`，内容 = Task 2 的字节）与
      `CFSTR_FILECONTENTS`（`TYMED_ISTREAM`，`lindex` = 第几项）。
      `EnumFormatEtc` 给 `IEnumFORMATETC`。写方向（`SetData`/`DAdvise`）一律
      `E_NOTIMPL`/`OLE_E_ADVISENOTSUPPORTED`。
- [ ] `DropSource`：`impl IDropSource`。左键松开 → `DRAGDROP_S_DROP`；
      Esc / 左键在按下前就没了 → `DRAGDROP_S_CANCEL`；否则 `S_OK`。
- [ ] `start_sta_thread`：`std::thread::spawn` → `OleInitialize(None)` →
      `DoDragDrop(&data, &src, DROPEFFECT_COPY, &mut effect)` → 记返回码与耗时 →
      `OleUninitialize`。**UI 线程只做 spawn，不进任何嵌套循环**（D10）。
- [ ] D12 的打点全部就位。
- [ ] 提交 `feat(app): F59 拖出 —— 专用 STA 线程 + 虚拟文件 IDataObject (F59)`

## Task 5：接线 + 文档 + 发版

- [ ] `FileAction::DragOut`；`files_panel` 按 N1 发它；`app.rs` 接到
      `dragout::start(runtime_handle, sftp, items)`。
- [ ] `has_real_action` **不用改**（`DragOut` 走的是既有的 `files_remote` 字段）——
      但要在测试里确认这一点，不能只是假设。
- [ ] `docs/gui-render-gotchas.md` 补 D10。
- [ ] 绿门（`cargo test --workspace` + `clippy -D warnings` + `fmt --check`）
      **且** `cargo build --release --target x86_64-pc-windows-gnu` 通过
      —— 对这一片，「Windows target 编得过」是仅有的机器验证。
- [ ] `chore: 版本 0.1.37(...)` → 交叉编译 + objdump → Release v0.1.37。

---

## 变异验收清单（每条都要亲手改一次、确认变红、再改回）

1. `sanitize` 里去掉设备名那一支 → `a_reserved_device_name_...` 变红。
2. `unique` 里去掉去重 → `two_different_remote_names_that_sanitize_alike_...` 变红。
3. `descriptor` 里把 `cFileName` 的偏移从 72 改成 68 → 布局测试变红。
4. `descriptor` 里把 `nFileSizeHigh`/`Low` 写反 → 大文件那条变红。
5. `should_hand_off` 里去掉 `!pointer_inside_window` → 「窗口内不交」那条变红。
6. `items_for` 里去掉「跳过目录」→ 目录那条变红。

## 明确不做

- **目录拖出**（N2）。
- **拖出到 Mullion 自己**（自己拖给自己）。
- **`CF_HDROP` 回退**：要先把整批文件落盘到临时目录才给得出路径，那正是 D11 要
  避免的「先等下载完再能拖」。只认 `CF_HDROP` 的目标程序会失败，靠 D12 的日志诊断。
- **macOS/Linux 拖出**：stub，只记一条 warn。
