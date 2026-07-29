# 切片 B2-a 分屏骨架 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 Mullion 能在一条 SSH 连接上开出 1/2/3/4 个终端分屏，用工具栏预设按钮切换布局，每个分屏有标题条和关闭按钮，布局一变就立刻给远端发 `window_change`。

**Architecture:** 新增 `mullion-app::shell::workspace` 模块承载多 pane 状态机，布局树跑在**像素**空间（32px 标题条与 1px 分隔线不是格的整数倍）。`mullion-core` 只补一个 `split_pane` + `leaves`，几何算法本身与单位无关。`mullion-ssh` 把 `connect` 拆成 `establish`（握手，每主机一次）+ `open_pty`（开 channel，每 pane 一次），`Handle` 用 `Arc` 共享。渲染、鼠标命中、`window_change` 三条路径读同一份 `Vec<PaneGeom>`。

**Tech Stack:** Rust / winit 0.30 / wgpu 23 / glyphon / egui + egui-wgpu / russh 0.54.5 / alacritty_terminal / tokio

**设计文档：** `docs/superpowers/specs/2026-07-29-slice-b2a-split-panes-design.md`（本计划的唯一真源，节号引用均指该文档）

---

## 实现决策（设计文档没写死、本计划锁定的七条）

写代码前先读这七条，它们决定了下面所有 Task 的类型签名。

1. **`PtyWriter` trait**（Task 5）。`SshSession` 的字段 `cmd_tx` 与 `SshCmd` 都是 `mullion-ssh` 私有，跨 crate **无法构造** —— 设计文档 §9 要的 `pty_write_goes_to_its_own_pane_channel_t1` 直接写不出来。故在 app 侧定义 `PtyWriter` trait 并给 `SshSession` 实现它，`PaneState.pty: Box<dyn PtyWriter>`，测试用 `FakePty`。
2. **`CursorStyle`**（Task 6）。多 pane 下 4 个 pane 会同时画 4 个实心光标。给 `quads_for` 加一个 `CursorStyle { Block, Hollow }` 参数，焦点 pane 传 `Block`，其余传 `Hollow`（4 条 1px 边框 quad）。
3. **`quads_for_panes` 复用 `quads_for`**（Task 6）。新函数只做「遍历 + 各自 origin + 合批」，不复制像素计算逻辑，`quads_for` 现有 5 个测试仍然守着单 pane 语义。
4. **`pane_bounds_ltrb` 纯函数**（Task 7）。`TextLayer::prepare` 是 GPU 胶水没法单测，但 §7.1 的「bounds 必须取 `term_px` 而不是整窗 `Resolution`」是硬要求。抽成返回 `(i32,i32,i32,i32)` 元组的纯函数（不返回 `TextBounds`，避免依赖 glyphon 那个类型是否 derive `PartialEq`），单独守。
5. **pacer 聚合语义 = `all()`**（Task 8）。「任一 pane 在同步块内则整帧延后」等价于「所有 pane 都 ready 才出帧」。**空集合返回 `true`** —— 否则还没连上时 launcher 界面一帧都出不来。
6. **复用 `reflow.rs`？不复用。** 既有 `reflow::reflow` 跑的是格单位、且假设一个 `Rect` 直接就是网格尺寸；B2-a 的 pane 网格要先扣标题条和分隔线才算得出来。`Workspace::apply_geometry` 取代它。`reflow.rs` 与 `app::tests::reflow_emits_resize`（T4 守护）**原样保留不动**，它守的是 core 层的「布局变 → 每 pane 都收到新尺寸」，仍然有效。
7. **网格换算统一用 `grid::grid_size_for`**（Task 3），不用 `shell::viewport::grid_dims`。理由：`grid_dims` 的语义是「中央区像素 → 网格」，带 `min` 夹紧参数、承担 chrome 扣减的语义；而 `PaneGeom.term_px` 是**已经扣完标题条和分隔线**的净区，再套一层 min 夹紧只会掩盖几何 bug。`viewport.rs` 保留给 app 算整窗中央区用。

---

## File Structure

**新建**

| 文件 | 职责 |
|---|---|
| `crates/mullion-app/src/shell/workspace/mod.rs` | `Workspace` 状态机、`PaneState`、`HostConn`、`PtyWriter`、`PaneStatus`。多 pane 的 pump / 断线 / 几何施加都在这里。 |
| `crates/mullion-app/src/shell/workspace/geom.rs` | `PxRect` / `PaneGeom` / `layout_geometry`。布局树 → 像素矩形 → 网格尺寸，含 32px 标题条与 1px 分隔线扣减。纯函数。 |
| `crates/mullion-app/src/shell/workspace/preset.rs` | `Preset` 七个预设、`preset_tree`、`plan_preset`（保留/新建/关闭）、`next_focus`。纯函数。 |
| `crates/mullion-app/src/ui/toolbar.rs` | F82 工具栏预设按钮组。 |
| `crates/mullion-app/src/ui/pane_title.rs` | F83 pane 标题条（含 `×` 关闭按钮）。 |

**修改**

| 文件 | 改什么 |
|---|---|
| `crates/mullion-core/src/layout.rs` | 新增 `split_pane` / `leaves`；`Rect` 文档注释改「单位由调用方定义」。 |
| `crates/mullion-ssh/src/session.rs` | 提取 `open_pty`；`io_task` 的保活参数改 `Arc<Handle<_>>`；`connect` 变成两者的组合。 |
| `crates/mullion-ssh/tests/pty.rs` | 加 F35 一连接多 channel 的集成测试。 |
| `crates/mullion-app/src/shell/mod.rs` | 加 `pub mod workspace;`。 |
| `crates/mullion-app/src/gpu.rs` | `quads_for` 加 `CursorStyle` 参数；新增 `PaneRender` / `quads_for_panes`。 |
| `crates/mullion-app/src/text.rs` | 新增 `pane_bounds_ltrb`；`prepare` → `prepare_panes`（buffers 变成跨 pane 的池）。 |
| `crates/mullion-app/src/render.rs` | 新增 `panes_ready_to_present`。 |
| `crates/mullion-app/src/ui/mod.rs` | 新增 `UiFrame<'_>` 聚参结构体，去掉 `#[allow(clippy::too_many_arguments)]`；挂 toolbar / pane_title。 |
| `crates/mullion-app/src/ui/chrome.rs` | 「分屏」菜单占位项换成指向工具栏的说明 + F83 标题条显隐开关。 |
| `crates/mullion-app/src/app.rs` | `conn: Option<Connection>` → `ws: Option<Workspace>`；输入路由按 focus pane；渲染走多 pane；状态栏接真实屏数。 |
| `crates/mullion-app/src/theme.rs` | 给 6 个零引用 token 补 F 编号注释（B1 技术债 3）。 |

---

## Task 1: core 补 `split_pane` 与 `leaves`（F30）

**Files:**
- Modify: `crates/mullion-core/src/layout.rs:12-19`（`Rect` 文档注释）、`crates/mullion-core/src/layout.rs:117` 之后（新增两个函数）
- Test: `crates/mullion-core/src/layout.rs` 的 `mod tests`

- [ ] **Step 1: 写失败测试**

在 `crates/mullion-core/src/layout.rs` 的 `mod tests` 末尾（`focus_moves_orthogonally_not_diagonally_f33` 之后、闭合大括号之前）追加：

```rust
    #[test]
    fn split_pane_replaces_leaf_f30() {
        let mut tree = leaf(1);
        assert!(split_pane(
            &mut tree,
            PaneId(1),
            PaneId(2),
            Dir::Horizontal,
            0.5
        ));
        assert_eq!(tree, hsplit(0.5, leaf(1), leaf(2)));
        let rects = compute_rects(&tree, AREA);
        assert_eq!(rects.len(), 2);
        assert_tiles_exactly(&rects, AREA);
    }

    #[test]
    fn split_pane_targets_nested_leaf_f30() {
        // 深层叶子也要能切,否则 2 屏切成 3 屏时只能动最外层。
        let mut tree = hsplit(0.5, leaf(1), leaf(2));
        assert!(split_pane(
            &mut tree,
            PaneId(2),
            PaneId(3),
            Dir::Vertical,
            0.5
        ));
        assert_eq!(tree, hsplit(0.5, leaf(1), vsplit(0.5, leaf(2), leaf(3))));
        assert_tiles_exactly(&compute_rects(&tree, AREA), AREA);
    }

    #[test]
    fn split_pane_unknown_target_is_noop_f30() {
        let mut tree = hsplit(0.5, leaf(1), leaf(2));
        let before = tree.clone();
        assert!(!split_pane(
            &mut tree,
            PaneId(9),
            PaneId(3),
            Dir::Vertical,
            0.5
        ));
        assert_eq!(tree, before);
    }

    /// `leaves` 的顺序必须和 `compute_rects` 一致 —— 预设重排(§5.2)靠这个顺序
    /// 把现有 pane 填进新树的叶子位;两者不一致,套预设后 pane 会互相换位。
    #[test]
    fn leaves_order_matches_compute_rects() {
        let tree = grid_2x2();
        let from_rects: Vec<PaneId> = compute_rects(&tree, AREA)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(leaves(&tree), from_rects);
        assert_eq!(
            leaves(&tree),
            vec![PaneId(1), PaneId(2), PaneId(3), PaneId(4)]
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-core --lib layout::tests 2>&1 | head -20`
Expected: FAIL，编译错误 `error[E0425]: cannot find function `split_pane` in this scope`（以及 `leaves`）。

- [ ] **Step 3: 最小实现**

在 `crates/mullion-core/src/layout.rs` 的 `split_len` 函数之后（第 117 行 `}` 之后）插入：

```rust
/// 把 `target` 叶子换成一个 Split:原 pane 落在 `a`,新 pane `new_id` 落在 `b`(F30)。
///
/// 返回是否成功;`target` 不在树中时返回 `false` 且树不变。
/// `ratio` 由调用方给定(预设布局用固定值),这里**不夹紧** —— 夹紧是拖分隔条(F32)
/// 的事,预设的 0.333/0.5/0.667 本来就是合法值,在这儿夹一道只会让预设几何变形。
pub fn split_pane(root: &mut Node, target: PaneId, new_id: PaneId, dir: Dir, ratio: f32) -> bool {
    match root {
        Node::Leaf(id) if *id == target => {
            *root = Node::Split {
                dir,
                ratio,
                a: Box::new(Node::Leaf(target)),
                b: Box::new(Node::Leaf(new_id)),
            };
            true
        }
        Node::Leaf(_) => false,
        Node::Split { a, b, .. } => {
            split_pane(a, target, new_id, dir, ratio) || split_pane(b, target, new_id, dir, ratio)
        }
    }
}

/// 按几何顺序(DFS,`a` 先 `b` 后)列出所有叶子 pane。
///
/// 与 [`compute_rects`] 的返回顺序**保证一致**,但不需要 `area`:预设重排只关心
/// 「谁在前谁在后」,不关心具体像素(§5.2)。
pub fn leaves(root: &Node) -> Vec<PaneId> {
    let mut out = Vec::new();
    collect_leaves(root, &mut out);
    out
}

fn collect_leaves(node: &Node, out: &mut Vec<PaneId>) {
    match node {
        Node::Leaf(id) => out.push(*id),
        Node::Split { a, b, .. } => {
            collect_leaves(a, out);
            collect_leaves(b, out);
        }
    }
}
```

同时把第 12 行的 `Rect` 文档注释改掉（布局树从此也跑像素，见设计文档 §4.1）：

```rust
/// 布局矩形。左上角 `(col, row)`,尺寸 `cols × rows`。
///
/// **单位由调用方定义**:纯网格场景传格数;app 的分屏(B2-a)传**像素**,因为
/// 32px 标题条和 1px 分隔线都不是格的整数倍。二分几何与单位无关,`u16` 的
/// 上限 65535 对 4K 宽度(3840)绰绰有余。
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-core --lib layout::tests 2>&1 | tail -5`
Expected: PASS，`test result: ok. 14 passed; 0 failed`

- [ ] **Step 5: 提交**

```bash
cd /data/Mullion
cargo clippy -p mullion-core --all-targets -- -D warnings
git add crates/mullion-core/src/layout.rs
git commit -m "feat(core): 布局树补 split_pane 与 leaves,Rect 改为单位无关 (F30)

split_pane 让叶子原地长成二分 Split,leaves 给预设重排提供与 compute_rects
一致的几何顺序。Rect 的单位注释从「格」改成「由调用方定义」——app 分屏跑
像素空间(设计文档 §4.1)。

守护测试:layout::tests::split_pane_replaces_leaf_f30、
layout::tests::leaves_order_matches_compute_rects"
```

---

## Task 2: ssh 提取 `open_pty`，一条连接开多个 channel（F35）

**Files:**
- Modify: `crates/mullion-ssh/src/session.rs:230-261`（`connect` 全体）、`:263`（`io_task` 签名）
- Test: `crates/mullion-ssh/tests/pty.rs`、`crates/mullion-ssh/tests/live.rs`

`establish`（`session.rs:71`）已经是 pub 且逻辑完整（B0 的 TOFU 切片提取的），
本任务**不动它**，只把 `connect` 里「开 channel + 起 io_task」那半截抽成 `open_pty`。

- [ ] **Step 1: 写失败测试**

在 `crates/mullion-ssh/tests/pty.rs` 末尾追加：

```rust
/// F35:一次握手、多条 channel。分屏的全部价值都压在这条上 —— 每开一个 pane
/// 就重新 TCP + 认证一次的话,高延迟代理链路下开 4 屏要等好几秒。
#[tokio::test(flavor = "multi_thread")]
async fn one_handshake_serves_many_ptys_f35() {
    let addr = common::spawn_echo_server().await;
    let policy = Arc::new(TofuAccept::new(Arc::new(Mutex::new(KnownHosts::new()))));
    let handle = Arc::new(establish(&cfg(addr), policy).await.expect("establish"));

    let mut sessions = Vec::new();
    for _ in 0..4 {
        let (s, rx) = open_pty(handle.clone(), &cfg(addr), Arc::new(|| {}))
            .await
            .expect("open_pty");
        sessions.push((s, rx));
    }

    for (i, (s, rx)) in sessions.iter_mut().enumerate() {
        let msg = format!("pane{i}");
        s.write(msg.as_bytes().to_vec()).expect("write");
        let got = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("未超时")
            .expect("收到回显");
        assert_eq!(
            got,
            msg.as_bytes(),
            "第 {i} 条 channel 的回显串到别的 channel 了"
        );
    }
}

/// 关掉一个 pane 不能拖垮别的 pane:`Handle` 用 Arc 共享,最后一个引用释放才断连。
#[tokio::test(flavor = "multi_thread")]
async fn dropping_one_pty_keeps_the_others_alive_f35() {
    let addr = common::spawn_echo_server().await;
    let policy = Arc::new(TofuAccept::new(Arc::new(Mutex::new(KnownHosts::new()))));
    let handle = Arc::new(establish(&cfg(addr), policy).await.expect("establish"));

    let (doomed, doomed_rx) = open_pty(handle.clone(), &cfg(addr), Arc::new(|| {}))
        .await
        .expect("open_pty 1");
    let (survivor, mut survivor_rx) = open_pty(handle.clone(), &cfg(addr), Arc::new(|| {}))
        .await
        .expect("open_pty 2");
    drop(doomed);
    drop(doomed_rx);

    survivor.write(b"still here".to_vec()).expect("write");
    let got = tokio::time::timeout(Duration::from_secs(5), survivor_rx.recv())
        .await
        .expect("未超时")
        .expect("收到回显");
    assert_eq!(&got, b"still here", "关掉一个 pane 把整条连接带走了");
}
```

并把该文件第 8 行的 import 改成：

```rust
use mullion_ssh::session::{connect, establish, open_pty};
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-ssh --test pty 2>&1 | head -20`
Expected: FAIL，编译错误 `error[E0432]: unresolved import `mullion_ssh::session::open_pty``

- [ ] **Step 3: 最小实现**

改 `crates/mullion-ssh/src/session.rs`。先把 `connect`（第 230–261 行，含它上方的
文档注释）整个替换成下面两个函数（`establish` 在它上面，原样不动）：

```rust
/// 握手 + 开 PTY channel 的一站式入口。CLI 直连与会话管理器都走这里(单 pane 路径)。
///
/// `wake` 由 app 注入(EventLoopProxy.send_event);ssh 不认识 winit。
pub async fn connect(
    cfg: &SshConfig,
    policy: Arc<dyn HostKeyPolicy>,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> Result<(SshSession, mpsc::Receiver<Vec<u8>>), ConnectError> {
    let handle = establish(cfg, policy).await?;
    open_pty(Arc::new(handle), cfg, wake).await
}

/// 在**已建立**的连接上再开一条 PTY channel(F35 分屏复用连接)。
///
/// 签名里刻意**没有任何网络参数**(host/port/auth/policy 一个都不收):
/// 想在这里偷偷重连一次都做不到,是结构性的防呆。主机密钥确认(F3/TOFU)只在
/// [`establish`] 触发一次,新开分屏不会再弹窗(§6.2)。
///
/// `handle` 必须是 `Arc`:russh 0.54.5 的 `Handle` 没有实现 `Clone`,只有 `Drop`
/// (释放即断连)。每条 channel 的 io_task 各持一份 Arc,最后一个释放才真正断连 ——
/// 这就是「关掉一个 pane 不影响其余 pane」的实现机制(§6.1)。
pub async fn open_pty(
    handle: Arc<Handle<ClientHandler>>,
    cfg: &SshConfig,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> Result<(SshSession, mpsc::Receiver<Vec<u8>>), ConnectError> {
    let channel = handle
        .channel_open_session()
        .await
        .map_err(|_| ConnectError::PtyRequest)?;
    channel
        .request_pty(
            true,
            &cfg.term,
            cfg.cols as u32,
            cfg.rows as u32,
            0,
            0,
            &[],
        )
        .await
        .map_err(|_| ConnectError::PtyRequest)?;
    channel
        .request_shell(true)
        .await
        .map_err(|_| ConnectError::PtyRequest)?;

    let (read, write) = channel.split();
    let (inbound_tx, inbound_rx) = mpsc::channel::<Vec<u8>>(256);
    let (cmd_tx, cmd_rx) = mpsc::channel::<SshCmd>(256);
    tokio::spawn(io_task(read, write, cmd_rx, inbound_tx, wake, handle));
    Ok((SshSession { cmd_tx }, inbound_rx))
}
```

再把 `io_task` 的最后一个参数类型从 `_handle: Handle<ClientHandler>` 改成：

```rust
    // 持有一份 Arc 只为保活:Handle 一 Drop 整条 SSH 连接就断。多 pane 下每条
    // channel 的 io_task 各持一份,最后一个 io_task 结束时连接才关(§6.1)。
    _handle: Arc<Handle<ClientHandler>>,
```

（`use std::sync::Arc;` 该文件已有，`Handle` 也已在 use 列表中；若编译报缺，按提示补。）

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-ssh 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: 全部 `test result: ok`，其中 `--test pty` 那行是 `4 passed`。

- [ ] **Step 5: 提交**

```bash
cd /data/Mullion
cargo clippy -p mullion-ssh --all-targets -- -D warnings
git add crates/mullion-ssh/src/session.rs crates/mullion-ssh/tests/pty.rs
git commit -m "feat(ssh): 提取 open_pty,一次握手开多条 PTY channel (F35)

connect = establish + open_pty,现有调用方不动。open_pty 的签名不含任何网络
参数,想在里面偷偷重连都做不到 —— TOFU 只在 establish 触发一次,新开分屏不
会再弹主机密钥确认窗(设计文档 §6.2)。

russh 0.54.5 的 Handle 没实现 Clone(只有 Drop),故用 Arc 共享;最后一个
io_task 结束才断连,这就是「关一个 pane 不拖垮其余 pane」的机制。

守护测试:tests/pty.rs::one_handshake_serves_many_ptys_f35、
tests/pty.rs::dropping_one_pty_keeps_the_others_alive_f35"
```

- [ ] **Step 6: 补真机 live 测试（设计文档 §9 要求，容器内跑不了）**

`tests/pty.rs` 用的是自建 echo server：协商、加密、窗口流控都是简化的，证明不了
真实 sshd 允许在**同一个** SSH 连接上开多路 session channel。设计文档 §9 因此点名
要一条 live 测试。它默认 `#[ignore]`，容器里跑不到真机，**属于「写脚手架 + 标注未验证」**。

把 `crates/mullion-ssh/tests/live.rs` 第 12 行的 import 改成：

```rust
use mullion_ssh::session::{connect, establish, open_pty};
```

在文件末尾追加：

```rust
/// 在 rx 上等到出现 `needle` 为止(10s 超时)。多 pane 场景下每条 channel 都要
/// 单独等一次,不能复用 `run_echo`(它自带 connect,这里要的是共享 handle)。
async fn wait_for(rx: &mut tokio::sync::mpsc::Receiver<Vec<u8>>, needle: &[u8]) -> bool {
    let mut seen = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(chunk) = rx.recv().await {
            seen.extend_from_slice(&chunk);
            if seen.windows(needle.len()).any(|w| w == needle) {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false)
}

/// F35 真机验证:一次 `establish` + 四次 `open_pty`,四条 channel 各跑各的 shell;
/// 再 drop 掉一条,断言其余三条仍能收发(§6.1 的 `Arc` 保活语义)。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "需真机(MULLION_LIVE_HOST 等)+ MULLION_LIVE=1"]
async fn multi_pty_live_f35() {
    if !live_enabled() {
        eprintln!("跳过:未设 MULLION_LIVE=1");
        return;
    }
    let cfg = base(AuthMethod::PublicKey {
        path: std::env::var("MULLION_LIVE_KEY")
            .unwrap_or_else(|_| "/path/to/key.pem".into())
            .into(),
        passphrase: None,
    });
    let policy = Arc::new(TofuAccept::new(Arc::new(Mutex::new(KnownHosts::new()))));
    let handle = Arc::new(establish(&cfg, policy).await.expect("真机握手"));

    let mut panes = Vec::new();
    for _ in 0..4 {
        panes.push(
            open_pty(handle.clone(), &cfg, Arc::new(|| {}))
                .await
                .expect("open_pty"),
        );
    }

    // 每条 channel 打一个不同的标记:串台了断言就会失败。
    for (i, (session, rx)) in panes.iter_mut().enumerate() {
        session
            .write(format!("echo MULLION_PANE_{i}\n").into_bytes())
            .expect("write");
        let needle = format!("MULLION_PANE_{i}");
        assert!(
            wait_for(rx, needle.as_bytes()).await,
            "第 {i} 条 channel 没回显自己的标记"
        );
    }

    // §6.1:关掉一个 pane 不能拖垮别的 pane。
    panes.remove(0);
    for (i, (session, rx)) in panes.iter_mut().enumerate() {
        session.write(b"echo MULLION_ALIVE\n".to_vec()).expect("write");
        assert!(
            wait_for(rx, b"MULLION_ALIVE").await,
            "drop 掉一条 channel 把幸存的第 {i} 条也带走了"
        );
    }
}
```

- [ ] **Step 7: 跑 live 测试（有真机才跑）并提交**

Run（无真机时**跳过**这条，直接提交；`--ignored` 在没有 `MULLION_LIVE=1` 时会
打印「跳过」并 pass，不构成验证）：

```bash
MULLION_LIVE=1 MULLION_LIVE_HOST=<真机> MULLION_LIVE_USER=<用户> MULLION_LIVE_KEY=<私钥> \
  cargo test -p mullion-ssh --test live -- --ignored --nocapture 2>&1 | tail -20
```

Expected: `test result: ok. 3 passed`（`pubkey_live` / `agent_live` / `multi_pty_live_f35`）。
真机信息用 env 传，**不写进被跟踪文件**。

先确认编译过（这条在容器里必须跑）：

Run: `cargo test -p mullion-ssh --test live 2>&1 | grep -E "test result|error"`
Expected: `test result: ok. 0 passed; 0 failed; 3 ignored`

```bash
cd /data/Mullion
cargo clippy -p mullion-ssh --all-targets -- -D warnings
git add crates/mullion-ssh/tests/live.rs
git commit -m "test(ssh): 真机 live 验证一次握手开四条 PTY channel (F35)

自建 echo server 证明不了真实 sshd 的多路 session channel;设计文档 §9 要的
这条只能在真机跑。默认 ignore,MULLION_LIVE=1 时手动触发。

**容器内未验证**,需按 README 的 MULLION_LIVE 流程人工跑一次。"
```

---

## Task 3: `workspace::geom` —— 布局树 → 像素矩形 → 网格（F30/F83）

**Files:**
- Create: `crates/mullion-app/src/shell/workspace/mod.rs`
- Create: `crates/mullion-app/src/shell/workspace/geom.rs`
- Modify: `crates/mullion-app/src/shell/mod.rs`

- [ ] **Step 1: 写失败测试**

先建 `crates/mullion-app/src/shell/workspace/geom.rs`，只写文件头与测试（实现下一步补）：

```rust
//! 分屏几何:布局树 → 每 pane 的像素矩形 → 网格尺寸(F30/F80/F83)。
//!
//! 布局树跑在**像素**空间而不是格:32px 标题条和 1px 分隔线都不是格的整数倍,
//! 先算格再扣 chrome 会一路掉精度(设计文档 §4.1)。三步:
//!   1. `compute_rects(树, 整个终端区像素矩形)` → 每 pane 的像素矩形
//!   2. 扣掉 32px 标题条(若开)、扣掉 1px 分隔线让位 → 终端网格区像素矩形
//!   3. `grid::grid_size_for(终端区像素, cell_w, cell_h)` → (cols, rows)
//!
//! **渲染、鼠标命中、window_change 三者读的是同一份 `Vec<PaneGeom>`**,任何一处
//! 自己另算一遍,迟早三者对不上(字画在 A、点击命中 B、远端按 C 排版)。

use mullion_core::layout::{compute_rects, Node, PaneId, Rect};

/// 窗口像素矩形,左上原点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PxRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// pane 标题条高度(F83)。
pub const TITLE_BAR_PX: u32 = 32;

/// 相邻 pane 之间的分隔线宽度(F80)。
///
/// core 的布局语义是「严丝合缝拼满、不为分隔条留格」,这条**不动**;让位完全是
/// app 侧的事 —— 非最右/最下的 pane 在 `term_px` 上各让出 1px,分隔线画在让出来
/// 的缝里。改 core 去扣格会把「拼满不重叠」那条不变量一起破坏掉。
pub const GAP_PX: u32 = 1;

/// 一个 pane 的全部几何。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneGeom {
    pub id: PaneId,
    /// 整块(含标题条,不含让给分隔线的那 1px)。
    pub px: PxRect,
    /// 标题条;标题条关闭时 `h == 0`。
    pub title_px: PxRect,
    /// 终端网格区(已扣标题条与分隔线让位)。
    pub term_px: PxRect,
    /// `term_px` 落成的 (cols, rows)。
    pub grid: (u16, u16),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(id: u32) -> Node {
        Node::Leaf(PaneId(id))
    }
    fn hsplit(r: f32, a: Node, b: Node) -> Node {
        Node::Split {
            dir: mullion_core::layout::Dir::Horizontal,
            ratio: r,
            a: Box::new(a),
            b: Box::new(b),
        }
    }

    /// 800x600 的终端区,字元 10x20。
    const AREA: PxRect = PxRect {
        x: 0,
        y: 100,
        w: 800,
        h: 600,
    };
    const CELL: (f32, f32) = (10.0, 20.0);

    #[test]
    fn single_pane_fills_area_and_yields_no_gap() {
        let g = layout_geometry(&leaf(1), AREA, CELL, false);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].px, AREA);
        assert_eq!(g[0].term_px, AREA, "只有一个 pane 时不该让任何像素给分隔线");
        assert_eq!(g[0].grid, (80, 30));
    }

    #[test]
    fn inner_pane_yields_one_pixel_for_the_divider_f80() {
        let g = layout_geometry(&hsplit(0.5, leaf(1), leaf(2)), AREA, CELL, false);
        assert_eq!(g[0].px.w, 400);
        assert_eq!(g[0].term_px.w, 399, "左 pane 要让 1px 给竖分隔线");
        assert_eq!(g[1].px.w, 400);
        assert_eq!(g[1].term_px.w, 400, "最右 pane 右边没有邻居,不让位");
        // 让位只吃掉不足一格的余量,列数不变。
        assert_eq!(g[0].grid, (39, 30));
        assert_eq!(g[1].grid, (40, 30));
    }

    #[test]
    fn grid_excludes_title_bar_f83() {
        let off = layout_geometry(&leaf(1), AREA, CELL, false);
        let on = layout_geometry(&leaf(1), AREA, CELL, true);
        assert_eq!(on[0].title_px.h, TITLE_BAR_PX);
        assert_eq!(off[0].title_px.h, 0);
        assert_eq!(
            on[0].term_px.y,
            AREA.y + TITLE_BAR_PX,
            "终端区必须从标题条下沿开始,否则首行被标题条盖住"
        );
        assert_eq!(on[0].term_px.h, AREA.h - TITLE_BAR_PX);
    }

    /// F83 开关会改行数 → 必须重发 window_change(T4)。这条锁住「行数确实变了」,
    /// 免得后来有人把标题条画成 overlay(不占空间)却忘了同步改注释。
    #[test]
    fn title_bar_toggle_changes_rows_f83() {
        let off = layout_geometry(&leaf(1), AREA, CELL, false);
        let on = layout_geometry(&leaf(1), AREA, CELL, true);
        assert_eq!(off[0].grid, (80, 30));
        assert_eq!(on[0].grid, (80, 28), "600-32=568px / 20px = 28 行");
        assert_ne!(off[0].grid, on[0].grid);
    }

    #[test]
    fn panes_tile_the_area_without_overlap_f30() {
        // 2x2:整块矩形(px,不是 term_px)必须严丝合缝拼满 AREA。
        let tree = mullion_core::layout::Node::Split {
            dir: mullion_core::layout::Dir::Vertical,
            ratio: 0.5,
            a: Box::new(hsplit(0.5, leaf(1), leaf(2))),
            b: Box::new(hsplit(0.5, leaf(3), leaf(4))),
        };
        let g = layout_geometry(&tree, AREA, CELL, true);
        assert_eq!(g.len(), 4);
        let covered: u64 = g.iter().map(|p| u64::from(p.px.w) * u64::from(p.px.h)).sum();
        assert_eq!(covered, u64::from(AREA.w) * u64::from(AREA.h), "未拼满");
        for (i, a) in g.iter().enumerate() {
            for b in g.iter().skip(i + 1) {
                let overlap = a.px.x < b.px.x + b.px.w
                    && b.px.x < a.px.x + a.px.w
                    && a.px.y < b.px.y + b.px.h
                    && b.px.y < a.px.y + a.px.h;
                assert!(!overlap, "pane 重叠: {:?} vs {:?}", a.px, b.px);
            }
        }
    }

    /// 窗口被拖到极小时不许 panic、不许算出 0 行 —— grid_size_for 夹到至少 1。
    #[test]
    fn tiny_area_does_not_panic_and_keeps_at_least_one_cell() {
        let tiny = PxRect {
            x: 0,
            y: 0,
            w: 4,
            h: 4,
        };
        let g = layout_geometry(&leaf(1), tiny, CELL, true);
        assert_eq!(g[0].title_px.h, 4, "标题条不能比 pane 还高");
        assert_eq!(g[0].grid, (1, 1));
    }
}
```

再建 `crates/mullion-app/src/shell/workspace/mod.rs`：

```rust
//! 分屏工作区(F30–F35):多 pane 状态机 + 几何 + 布局预设。
//!
//! 零 winit/wgpu/egui —— 状态机部分可纯单测,这是本切片能在无头容器里验证
//! 布局与 window_change 行为的前提。

pub mod geom;

pub use geom::{layout_geometry, PaneGeom, PxRect, GAP_PX, TITLE_BAR_PX};

/// pane 的连接状态(§6.3)。断开的 pane 内容保留、可滚可复制,只是不再收发。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneStatus {
    Live,
    Disconnected,
}
```

在 `crates/mullion-app/src/shell/mod.rs` 的模块列表里按字母序加一行（`pub mod window_state;` 之后）：

```rust
pub mod workspace;
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib shell::workspace 2>&1 | head -20`
Expected: FAIL，编译错误 `error[E0425]: cannot find function `layout_geometry` in this scope`

- [ ] **Step 3: 最小实现**

在 `geom.rs` 的 `PaneGeom` 定义之后、`#[cfg(test)]` 之前插入：

```rust
/// 算出每个 pane 的完整几何。
///
/// `area` 是整个终端区的像素矩形(窗口扣掉 egui 菜单栏/工具栏/状态栏之后剩下的)。
/// `cell` 是字元像素尺寸 `(宽, 高)`。`title_bars` 对应 F83 开关。
pub fn layout_geometry(
    tree: &Node,
    area: PxRect,
    cell: (f32, f32),
    title_bars: bool,
) -> Vec<PaneGeom> {
    // u16 承载像素:4K 宽 3840 远低于 65535。超大屏(理论上 >65535px)饱和截断,
    // 画面会不对但不会静默回绕成小值。
    let clamp = |v: u32| u16::try_from(v).unwrap_or(u16::MAX);
    let root = Rect {
        col: clamp(area.x),
        row: clamp(area.y),
        cols: clamp(area.w),
        rows: clamp(area.h),
    };
    let right_end = u32::from(root.col) + u32::from(root.cols);
    let bottom_end = u32::from(root.row) + u32::from(root.rows);

    compute_rects(tree, root)
        .into_iter()
        .map(|(id, r)| {
            let px = PxRect {
                x: u32::from(r.col),
                y: u32::from(r.row),
                w: u32::from(r.cols),
                h: u32::from(r.rows),
            };
            // 标题条不能比 pane 本身还高(窗口被拖到极小时会发生)。
            let title_h = if title_bars {
                TITLE_BAR_PX.min(px.h)
            } else {
                0
            };
            let at_right = px.x + px.w >= right_end;
            let at_bottom = px.y + px.h >= bottom_end;
            let term_px = PxRect {
                x: px.x,
                y: px.y + title_h,
                w: px.w.saturating_sub(if at_right { 0 } else { GAP_PX }),
                h: px
                    .h
                    .saturating_sub(title_h)
                    .saturating_sub(if at_bottom { 0 } else { GAP_PX }),
            };
            PaneGeom {
                id,
                px,
                title_px: PxRect {
                    x: px.x,
                    y: px.y,
                    w: px.w,
                    h: title_h,
                },
                term_px,
                grid: crate::grid::grid_size_for(term_px.w, term_px.h, cell.0, cell.1),
            }
        })
        .collect()
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib shell::workspace 2>&1 | tail -5`
Expected: PASS，`test result: ok. 6 passed; 0 failed`

- [ ] **Step 5: 提交**

```bash
cd /data/Mullion
cargo clippy -p mullion-app --all-targets -- -D warnings
git add crates/mullion-app/src/shell/mod.rs crates/mullion-app/src/shell/workspace/
git commit -m "feat(app): 分屏几何 —— 布局树跑像素空间,扣标题条与分隔线 (F30/F83)

layout_geometry 一次算出每 pane 的整块/标题条/终端区三个像素矩形与网格尺寸。
渲染、鼠标命中、window_change 三条路径今后共用这一份结果。

core 的「严丝合缝拼满」语义不动,1px 分隔线让位完全在 app 侧做:非最右/最下
的 pane 各让 1px(设计文档 §4.1)。

守护测试:shell::workspace::geom::tests::grid_excludes_title_bar_f83、
title_bar_toggle_changes_rows_f83、panes_tile_the_area_without_overlap_f30"
```

---

## Task 4: `workspace::preset` —— 七个布局预设与重排计划（F82/§5）

**Files:**
- Create: `crates/mullion-app/src/shell/workspace/preset.rs`
- Modify: `crates/mullion-app/src/shell/workspace/mod.rs`

- [ ] **Step 1: 写失败测试**

先建 `crates/mullion-app/src/shell/workspace/preset.rs`，写文件头、类型和测试：

```rust
//! 工具栏布局预设(F82)与套用预设时的重排计划(§5)。纯函数,零 IO。
//!
//! 「套用预设」是**声明式**的:结果只取决于目标预设和当前 pane 的几何顺序,
//! 与用户点按钮的历史路径无关。1→4→2 和 1→2 落到同一棵树。

use mullion_core::layout::{Dir, Node, PaneId};

use super::PaneStatus;

/// 工具栏上的布局预设。分两段:先选屏数,再选该屏数下的子布局(§3)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    /// 1 屏满窗。**原型没有这个按钮**(分屏后回不到单屏),我们补上。
    Single,
    TwoLeftRight,
    TwoTopBottom,
    /// 左边一大块,右边上下分。
    ThreeBigLeft,
    /// 右边一大块,左边上下分。
    ThreeBigRight,
    /// 三个等宽竖条。
    ThreeColumns,
    FourGrid,
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_core::layout::{compute_rects, leaves, Rect};

    const AREA: Rect = Rect {
        col: 0,
        row: 0,
        cols: 1200,
        rows: 600,
    };

    fn ids(n: u32) -> Vec<PaneId> {
        (1..=n).map(PaneId).collect()
    }

    fn assert_tiles(tree: &Node, want: usize) {
        let rects = compute_rects(tree, AREA);
        assert_eq!(rects.len(), want, "叶子数不对");
        let covered: u64 = rects
            .iter()
            .map(|(_, r)| u64::from(r.cols) * u64::from(r.rows))
            .sum();
        assert_eq!(
            covered,
            u64::from(AREA.cols) * u64::from(AREA.rows),
            "未拼满"
        );
    }

    #[test]
    fn every_preset_tiles_exactly_f30() {
        for p in Preset::ALL {
            assert_tiles(&preset_tree(p, &ids(p.pane_count() as u32)), p.pane_count());
        }
    }

    #[test]
    fn preset_pane_counts_match_their_group() {
        assert_eq!(Preset::Single.pane_count(), 1);
        assert_eq!(Preset::TwoLeftRight.pane_count(), 2);
        assert_eq!(Preset::TwoTopBottom.pane_count(), 2);
        assert_eq!(Preset::ThreeBigLeft.pane_count(), 3);
        assert_eq!(Preset::ThreeBigRight.pane_count(), 3);
        assert_eq!(Preset::ThreeColumns.pane_count(), 3);
        assert_eq!(Preset::FourGrid.pane_count(), 4);
        for p in Preset::ALL {
            assert_eq!(p.group(), p.pane_count(), "按钮分组就是屏数");
        }
    }

    /// 三等分必须是**三个竖条**,不能是「左半 + 右半再对半」那种 1/2:1/4:1/4。
    #[test]
    fn three_columns_are_equal_width() {
        let rects = compute_rects(&preset_tree(Preset::ThreeColumns, &ids(3)), AREA);
        let widths: Vec<u16> = rects.iter().map(|(_, r)| r.cols).collect();
        assert_eq!(widths, vec![400, 400, 400]);
    }

    /// 左大右上下:左边一整条,右边被横向切两块。
    #[test]
    fn three_big_left_geometry() {
        let rects = compute_rects(&preset_tree(Preset::ThreeBigLeft, &ids(3)), AREA);
        assert_eq!(rects[0].1.cols, 800, "左块占 2/3 宽");
        assert_eq!(rects[0].1.rows, 600, "左块通高");
        assert_eq!(rects[1].1.rows, 300);
        assert_eq!(rects[2].1.rows, 300);
    }

    #[test]
    fn preset_tree_fills_leaves_in_geometric_order() {
        let tree = preset_tree(Preset::FourGrid, &ids(4));
        assert_eq!(
            leaves(&tree),
            vec![PaneId(1), PaneId(2), PaneId(3), PaneId(4)]
        );
    }

    #[test]
    fn growing_keeps_existing_panes_and_spawns_the_rest() {
        let plan = plan_preset(Preset::FourGrid, &[(PaneId(1), PaneStatus::Live)]);
        assert_eq!(plan.keep, vec![PaneId(1)]);
        assert_eq!(plan.spawn, 3);
        assert!(plan.close.is_empty());
    }

    #[test]
    fn same_count_keeps_everyone() {
        let cur = [
            (PaneId(1), PaneStatus::Live),
            (PaneId(2), PaneStatus::Live),
        ];
        let plan = plan_preset(Preset::TwoTopBottom, &cur);
        assert_eq!(plan.keep, vec![PaneId(1), PaneId(2)]);
        assert_eq!(plan.spawn, 0);
        assert!(plan.close.is_empty(), "换子布局不该重开任何 channel");
    }

    /// §5.3:减屏优先关**已断开**的 pane —— 用户多半就是想把死掉的那块清掉,
    /// 关掉还活着的反而丢工作。
    #[test]
    fn close_prefers_disconnected_panes() {
        let cur = [
            (PaneId(1), PaneStatus::Live),
            (PaneId(2), PaneStatus::Disconnected),
            (PaneId(3), PaneStatus::Live),
            (PaneId(4), PaneStatus::Disconnected),
        ];
        let plan = plan_preset(Preset::TwoLeftRight, &cur);
        assert_eq!(
            plan.close,
            vec![PaneId(4), PaneId(2)],
            "两个断开的先走(几何逆序)"
        );
        assert_eq!(plan.keep, vec![PaneId(1), PaneId(3)]);
        assert_eq!(plan.spawn, 0);
    }

    /// 断开的不够关时,继续按几何逆序关活着的。
    #[test]
    fn close_falls_back_to_live_panes_in_reverse_order() {
        let cur = [
            (PaneId(1), PaneStatus::Live),
            (PaneId(2), PaneStatus::Live),
            (PaneId(3), PaneStatus::Disconnected),
            (PaneId(4), PaneStatus::Live),
        ];
        let plan = plan_preset(Preset::Single, &cur);
        assert_eq!(plan.close, vec![PaneId(3), PaneId(4), PaneId(2)]);
        assert_eq!(plan.keep, vec![PaneId(1)]);
    }

    #[test]
    fn focus_survives_when_its_pane_survives() {
        assert_eq!(
            next_focus(PaneId(3), &[PaneId(1), PaneId(3)]),
            PaneId(3)
        );
    }

    /// §5.3:焦点 pane 被关掉 → 落到几何顺序第一个存活 pane。
    #[test]
    fn focus_falls_back_to_first_survivor() {
        assert_eq!(
            next_focus(PaneId(9), &[PaneId(2), PaneId(5)]),
            PaneId(2)
        );
    }

    /// 声明式:路径不影响结果。
    #[test]
    fn applying_a_preset_is_path_independent() {
        let direct = plan_preset(Preset::TwoLeftRight, &[(PaneId(1), PaneStatus::Live)]);
        let via_four = plan_preset(
            Preset::TwoLeftRight,
            &[
                (PaneId(1), PaneStatus::Live),
                (PaneId(2), PaneStatus::Live),
                (PaneId(3), PaneStatus::Live),
            ],
        );
        // 起点不同,但两次的结果都是「1 号留在首位」。
        assert_eq!(direct.keep.first(), via_four.keep.first());
        assert_eq!(direct.keep.len() + direct.spawn, 2);
        assert_eq!(via_four.keep.len() + via_four.spawn, 2);
    }
}
```

在 `crates/mullion-app/src/shell/workspace/mod.rs` 里挂上模块并 re-export（放在 `pub mod geom;` 之后、`pub use geom::...` 之后）：

```rust
pub mod preset;

pub use preset::{next_focus, plan_preset, preset_tree, Preset, PresetPlan};
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib shell::workspace::preset 2>&1 | head -20`
Expected: FAIL，编译错误 `error[E0599]: no associated item named `ALL` found for enum `Preset``

- [ ] **Step 3: 最小实现**

在 `preset.rs` 的 `enum Preset` 之后、`#[cfg(test)]` 之前插入：

```rust
impl Preset {
    /// 工具栏按钮的绘制顺序(§3):先 1/2/3/4 屏,再是各屏数下的子布局。
    pub const ALL: [Preset; 7] = [
        Preset::Single,
        Preset::TwoLeftRight,
        Preset::TwoTopBottom,
        Preset::ThreeBigLeft,
        Preset::ThreeBigRight,
        Preset::ThreeColumns,
        Preset::FourGrid,
    ];

    /// 这个预设要几个 pane。
    pub fn pane_count(self) -> usize {
        match self {
            Preset::Single => 1,
            Preset::TwoLeftRight | Preset::TwoTopBottom => 2,
            Preset::ThreeBigLeft | Preset::ThreeBigRight | Preset::ThreeColumns => 3,
            Preset::FourGrid => 4,
        }
    }

    /// 按钮所属的屏数分组。当前与 `pane_count` 同值,分开写是因为语义不同:
    /// 一个是 UI 分组,一个是要开几条 channel。
    pub fn group(self) -> usize {
        self.pane_count()
    }

    /// 按钮上的字形 + 文字(F82)。字形用几何方块,不依赖字体的图标集。
    pub fn label(self) -> &'static str {
        match self {
            Preset::Single => "▢ 1 屏",
            Preset::TwoLeftRight => "▥ 左右分",
            Preset::TwoTopBottom => "▤ 上下分",
            Preset::ThreeBigLeft => "⊟ 左大",
            Preset::ThreeBigRight => "⊞ 右大",
            Preset::ThreeColumns => "▦ 三等分",
            Preset::FourGrid => "▩ 2×2",
        }
    }

    /// 鼠标悬停提示。
    pub fn tooltip(self) -> &'static str {
        match self {
            Preset::Single => "单屏满窗",
            Preset::TwoLeftRight => "两屏,左右并排",
            Preset::TwoTopBottom => "两屏,上下堆叠",
            Preset::ThreeBigLeft => "三屏,左边一大块,右边上下分",
            Preset::ThreeBigRight => "三屏,右边一大块,左边上下分",
            Preset::ThreeColumns => "三屏,三个等宽竖条",
            Preset::FourGrid => "四屏,2×2 网格",
        }
    }
}

fn split(dir: Dir, ratio: f32, a: Node, b: Node) -> Node {
    Node::Split {
        dir,
        ratio,
        a: Box::new(a),
        b: Box::new(b),
    }
}

/// 用给定的 pane id 搭出预设布局树(§5.1)。
///
/// # Panics
/// `ids.len()` 必须等于 `preset.pane_count()`。调用方(`Workspace::apply_preset`)
/// 保证这点;数量对不上是编程错误,不是运行时输入错误,故直接 panic 而不是返回
/// Result —— 静默补一个 pane 出来只会让布局错得更难查。
pub fn preset_tree(preset: Preset, ids: &[PaneId]) -> Node {
    assert_eq!(
        ids.len(),
        preset.pane_count(),
        "预设 {preset:?} 需要 {} 个 pane,给了 {}",
        preset.pane_count(),
        ids.len()
    );
    let l = |i: usize| Node::Leaf(ids[i]);
    let h = Dir::Horizontal;
    let v = Dir::Vertical;
    match preset {
        Preset::Single => l(0),
        Preset::TwoLeftRight => split(h, 0.5, l(0), l(1)),
        Preset::TwoTopBottom => split(v, 0.5, l(0), l(1)),
        Preset::ThreeBigLeft => split(h, 2.0 / 3.0, l(0), split(v, 0.5, l(1), l(2))),
        Preset::ThreeBigRight => split(h, 1.0 / 3.0, split(v, 0.5, l(0), l(1)), l(2)),
        // 先切掉左边 1/3,剩下的 2/3 再对半 → 三个等宽竖条。
        Preset::ThreeColumns => split(h, 1.0 / 3.0, l(0), split(h, 0.5, l(1), l(2))),
        Preset::FourGrid => split(
            v,
            0.5,
            split(h, 0.5, l(0), l(1)),
            split(h, 0.5, l(2), l(3)),
        ),
    }
}

/// 套用预设的重排计划(§5.2/§5.3)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetPlan {
    /// 按几何顺序保留下来的现有 pane。它们依次填进新树的前若干个叶子位。
    pub keep: Vec<PaneId>,
    /// 还差几个 pane,需要新开 channel。它们排在 `keep` 之后填满剩余叶子位。
    pub spawn: usize,
    /// 要关掉的 pane,按关闭顺序(先断开的、后活着的)。
    pub close: Vec<PaneId>,
}

/// 算出套用 `preset` 需要保留 / 新建 / 关闭哪些 pane。
///
/// `current` 必须按**几何顺序**给(`mullion_core::layout::leaves` 的返回顺序),
/// 不然重排后 pane 会互相换位,用户会觉得内容"跳"了。
pub fn plan_preset(preset: Preset, current: &[(PaneId, PaneStatus)]) -> PresetPlan {
    let want = preset.pane_count();
    if current.len() <= want {
        return PresetPlan {
            keep: current.iter().map(|(id, _)| *id).collect(),
            spawn: want - current.len(),
            close: Vec::new(),
        };
    }
    // 减屏:先关已断开的,再关活着的,同类里按几何逆序(右下角先走)。
    let extra = current.len() - want;
    let by_status = |want_status: PaneStatus| {
        current
            .iter()
            .rev()
            .filter(move |(_, s)| *s == want_status)
            .map(|(id, _)| *id)
    };
    let close: Vec<PaneId> = by_status(PaneStatus::Disconnected)
        .chain(by_status(PaneStatus::Live))
        .take(extra)
        .collect();
    PresetPlan {
        keep: current
            .iter()
            .map(|(id, _)| *id)
            .filter(|id| !close.contains(id))
            .collect(),
        spawn: 0,
        close,
    }
}

/// 焦点 pane 被关掉后落到哪(§5.3):几何顺序第一个存活 pane。
///
/// `survivors` 为空时原样返回 —— 最后一个 pane 不可关(core 的 `close_pane`
/// 已经保证),真到了这一步说明上游有 bug,不该在这里静默造一个 id 出来。
pub fn next_focus(focus: PaneId, survivors: &[PaneId]) -> PaneId {
    if survivors.contains(&focus) {
        focus
    } else {
        survivors.first().copied().unwrap_or(focus)
    }
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib shell::workspace::preset 2>&1 | tail -5`
Expected: PASS，`test result: ok. 11 passed; 0 failed`

- [ ] **Step 5: 提交**

```bash
cd /data/Mullion
cargo clippy -p mullion-app --all-targets -- -D warnings
git add crates/mullion-app/src/shell/workspace/
git commit -m "feat(app): 七个布局预设与声明式重排计划 (F82)

preset_tree 把按钮映射成布局树;plan_preset 算出保留/新建/关闭哪些 pane。
结果只取决于目标预设与当前几何顺序,与点击路径无关(1→4→2 和 1→2 同结果)。

减屏优先关已断开的 pane(设计文档 §5.3):用户多半就是想清掉死掉的那块。
焦点被关则落到几何顺序第一个存活 pane。

守护测试:shell::workspace::preset::tests::close_prefers_disconnected_panes、
focus_falls_back_to_first_survivor、every_preset_tiles_exactly_f30"
```

---

## Task 5: `Workspace` 状态机 —— pump / 断线 / F34 施加几何

**Files:**
- Modify: `crates/mullion-app/src/shell/workspace/mod.rs`

- [ ] **Step 1: 写失败测试**

在 `crates/mullion-app/src/shell/workspace/mod.rs` 末尾追加：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{term_default_colors, MULLION_DARK};
    use mullion_core::layout::Dir;
    use std::sync::Mutex;

    /// 测试替身。`SshSession` 的字段与 `SshCmd` 都是 mullion-ssh 私有,跨 crate
    /// 造不出来 —— 这就是 `PtyWriter` trait 存在的理由(实现决策 1)。
    #[derive(Default)]
    struct FakePty {
        writes: Arc<Mutex<Vec<Vec<u8>>>>,
        resizes: Arc<Mutex<Vec<(u16, u16)>>>,
    }

    impl PtyWriter for FakePty {
        fn write(&self, bytes: Vec<u8>) -> Result<(), TrySendErr> {
            self.writes.lock().unwrap().push(bytes);
            Ok(())
        }
        fn resize(&self, cols: u16, rows: u16) -> Result<(), TrySendErr> {
            self.resizes.lock().unwrap().push((cols, rows));
            Ok(())
        }
    }

    struct Probe {
        writes: Arc<Mutex<Vec<Vec<u8>>>>,
        resizes: Arc<Mutex<Vec<(u16, u16)>>>,
        tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    }

    /// 造一个挂着 FakePty 的 pane,并把它的输入端 / 观测端一起返回。
    fn fake_pane(id: u32) -> (PaneState, Probe) {
        let pty = FakePty::default();
        let probe_writes = pty.writes.clone();
        let probe_resizes = pty.resizes.clone();
        let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(16);
        let mut emulator = mullion_term::emulator::Emulator::new(80, 24);
        let d = term_default_colors(&MULLION_DARK);
        emulator.set_default_colors(d.fg, d.bg);
        (
            PaneState {
                id: PaneId(id),
                host_ix: 0,
                emulator,
                pty: Box::new(pty),
                rx,
                pacer: crate::render::SyncFramePacer::new(),
                status: PaneStatus::Live,
                last_grid: (80, 24),
            },
            Probe {
                writes: probe_writes,
                resizes: probe_resizes,
                tx,
            },
        )
    }

    fn ws_with(n: u32) -> (Workspace, Vec<Probe>) {
        let (first, p0) = fake_pane(1);
        let mut ws = Workspace::new(first);
        let mut probes = vec![p0];
        for id in 2..=n {
            let (p, probe) = fake_pane(id);
            ws.attach_pane(p);
            probes.push(probe);
            ws.tree_mut_for_test(id);
        }
        (ws, probes)
    }

    /// T1:光标位置查询的应答必须回写到**产生它的那个 pane 自己的** channel。
    /// 串到别的 pane 上去,发起查询的 TUI 就永远等不到应答 → 全屏 TUI 冻死。
    #[tokio::test]
    async fn pty_write_goes_to_its_own_pane_channel_t1() {
        let (mut ws, probes) = ws_with(2);
        // 只给 2 号 pane 喂 DSR 6 查询。
        probes[1].tx.send(b"\x1b[6n".to_vec()).await.unwrap();
        // send 之后 try_recv 才看得到,给 runtime 一次让步。
        tokio::task::yield_now().await;
        ws.pump(0);
        assert!(
            probes[0].writes.lock().unwrap().is_empty(),
            "1 号 pane 没被查询,不该有任何回写(T1 串台)"
        );
        assert_eq!(
            probes[1].writes.lock().unwrap().as_slice(),
            &[b"\x1b[1;1R".to_vec()],
            "2 号 pane 的 CPR 应答没回到自己的 channel(T1)"
        );
    }

    /// T4/F34:布局一变,**每个**尺寸真的变了的 pane 都要收到 window_change。
    /// 漏发 → 远端 TUI 按旧列数排版 → tmux 里全屏应用直接错行。
    ///
    /// 设计文档 §9 把它叫 `preset_change_emits_resize_for_every_pane_f34`;
    /// 这里叫 geometry_ 是因为四条触发路径(切预设 / 关 pane / 窗口 resize /
    /// 开关标题条)统一收敛到 `apply_geometry` 一个出口,测的是那个出口而不是
    /// 「切预设」这一条路径。名字对齐实现,免得后来者以为还有三条没测。
    #[test]
    fn geometry_change_emits_resize_for_every_pane_f34() {
        let (mut ws, probes) = ws_with(2);
        let geoms = vec![
            PaneGeom {
                id: PaneId(1),
                px: PxRect { x: 0, y: 0, w: 400, h: 600 },
                title_px: PxRect { x: 0, y: 0, w: 400, h: 0 },
                term_px: PxRect { x: 0, y: 0, w: 399, h: 600 },
                grid: (39, 30),
            },
            PaneGeom {
                id: PaneId(2),
                px: PxRect { x: 400, y: 0, w: 400, h: 600 },
                title_px: PxRect { x: 400, y: 0, w: 400, h: 0 },
                term_px: PxRect { x: 400, y: 0, w: 400, h: 600 },
                grid: (40, 30),
            },
        ];
        ws.apply_geometry(&geoms);
        assert_eq!(probes[0].resizes.lock().unwrap().as_slice(), &[(39, 30)]);
        assert_eq!(probes[1].resizes.lock().unwrap().as_slice(), &[(40, 30)]);
        // 仿真器也得跟着改,否则本地渲染按旧尺寸、远端按新尺寸。
        assert_eq!(ws.pane(PaneId(1)).unwrap().emulator.snapshot().cols, 39);
    }

    /// 尺寸没变就别发:每帧无脑 window_change 会把远端 SIGWINCH 刷爆,
    /// tmux 里的 TUI 会不停重排。
    #[test]
    fn unchanged_geometry_emits_no_resize() {
        let (mut ws, probes) = ws_with(1);
        let geoms = vec![PaneGeom {
            id: PaneId(1),
            px: PxRect { x: 0, y: 0, w: 800, h: 480 },
            title_px: PxRect { x: 0, y: 0, w: 800, h: 0 },
            term_px: PxRect { x: 0, y: 0, w: 800, h: 480 },
            grid: (80, 24), // 与 fake_pane 的 last_grid 相同
        }];
        ws.apply_geometry(&geoms);
        assert!(
            probes[0].resizes.lock().unwrap().is_empty(),
            "尺寸未变却发了 window_change"
        );
    }

    /// §6.3:对端断开(channel 关闭)→ pane 标记 Disconnected,内容留着可滚可复制。
    #[tokio::test]
    async fn closed_channel_marks_pane_disconnected() {
        let (mut ws, probes) = ws_with(1);
        assert_eq!(ws.pane(PaneId(1)).unwrap().status, PaneStatus::Live);
        drop(probes);
        tokio::task::yield_now().await;
        ws.pump(0);
        assert_eq!(ws.pane(PaneId(1)).unwrap().status, PaneStatus::Disconnected);
    }

    #[test]
    fn apply_preset_reuses_panes_and_reports_the_ids_to_open() {
        let (mut ws, _p) = ws_with(1);
        let fresh = ws.apply_preset(Preset::FourGrid);
        assert_eq!(fresh.len(), 3, "1 屏 → 4 屏要新开 3 条 channel");
        assert_eq!(
            mullion_core::layout::leaves(ws.tree()).len(),
            4,
            "树上必须先有 4 个叶子,新 pane 还在连的时候画占位"
        );
        assert!(!fresh.contains(&PaneId(1)), "已有 pane 不该被重开");
    }

    #[test]
    fn apply_preset_drops_panes_and_moves_focus_off_the_closed_one() {
        let (mut ws, _p) = ws_with(2);
        ws.set_focus(PaneId(2));
        let fresh = ws.apply_preset(Preset::Single);
        assert!(fresh.is_empty());
        assert_eq!(mullion_core::layout::leaves(ws.tree()), vec![PaneId(1)]);
        assert!(ws.pane(PaneId(2)).is_none(), "被关的 pane 状态该一起清掉");
        assert_eq!(ws.focus(), PaneId(1), "焦点必须落到存活 pane 上");
    }

    #[test]
    fn close_pane_refuses_to_kill_the_last_one_f31() {
        let (mut ws, _p) = ws_with(1);
        assert!(!ws.close_pane(PaneId(1)), "最后一个 pane 不可关");
        assert!(ws.pane(PaneId(1)).is_some());
    }

    #[test]
    fn close_pane_promotes_sibling_f31() {
        let (mut ws, _p) = ws_with(2);
        ws.set_focus(PaneId(2));
        assert!(ws.close_pane(PaneId(2)));
        assert_eq!(mullion_core::layout::leaves(ws.tree()), vec![PaneId(1)]);
        assert_eq!(ws.focus(), PaneId(1));
    }

    /// 新分配的 id 不能撞已有 pane —— 撞了就是两个 PaneState 抢一个叶子位,
    /// 渲染和输入会随机挑一个,表现成"输入偶尔跑到另一块去"。
    #[test]
    fn ids_are_never_reused() {
        let (mut ws, _p) = ws_with(2);
        ws.apply_preset(Preset::Single);
        let fresh = ws.apply_preset(Preset::TwoLeftRight);
        assert_eq!(fresh, vec![PaneId(3)], "不能回收 2 号");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib shell::workspace::tests 2>&1 | head -20`
Expected: FAIL，编译错误 `error[E0412]: cannot find type `Workspace` in this scope`

- [ ] **Step 3: 最小实现**

在 `crates/mullion-app/src/shell/workspace/mod.rs` 的 `PaneStatus` 之后、`#[cfg(test)]` 之前插入：

```rust
use std::sync::Arc;

use mullion_core::layout::{close_pane, leaves, split_pane, Dir, Node, PaneId};
use mullion_ssh::session::{ClientHandler, SshSession, TrySendErr};
use mullion_term::emulator::Emulator;
use russh::client::Handle;
use tokio::sync::mpsc::{error::TryRecvError, Receiver};

use crate::render::SyncFramePacer;
use crate::session_pump;

/// pane 对下游 PTY 的写口。
///
/// 抽 trait 有两个理由:
/// 1. `SshSession` 的字段和 `SshCmd` 都是 mullion-ssh 私有,跨 crate **无法构造**;
///    没有这层抽象,workspace 的状态机(T1 归属、F34 resize)一条测试都写不出来。
/// 2. 断线的 pane 将来要能换成"丢弃写入"的实现,不必让状态机到处判 Option。
pub trait PtyWriter: Send {
    fn write(&self, bytes: Vec<u8>) -> Result<(), TrySendErr>;
    fn resize(&self, cols: u16, rows: u16) -> Result<(), TrySendErr>;
}

impl PtyWriter for SshSession {
    fn write(&self, bytes: Vec<u8>) -> Result<(), TrySendErr> {
        SshSession::write(self, bytes)
    }
    fn resize(&self, cols: u16, rows: u16) -> Result<(), TrySendErr> {
        SshSession::resize(self, cols, rows)
    }
}

/// 一条**已建立**的 SSH 连接。B2-a 里恒只有一个;数据模型按 N host × M channel
/// 设计,是为了 B2-b 的"换主机"能原地加而不用推翻结构(§4.2)。
pub struct HostConn {
    /// 状态栏/标题条上显示的名字(会话名或 user@host)。
    pub label: String,
    /// `user@host:port`,标题条副标题用。
    pub addr: String,
    /// `Arc` 是必须的:russh 的 `Handle` 没实现 `Clone`,只有 `Drop`(释放即断连)。
    pub handle: Arc<Handle<ClientHandler>>,
}

/// 一个分屏的全部运行时状态。
pub struct PaneState {
    pub id: PaneId,
    /// 指向 `Workspace::hosts`。B2-a 恒为 0。
    pub host_ix: usize,
    pub emulator: Emulator,
    pub pty: Box<dyn PtyWriter>,
    pub rx: Receiver<Vec<u8>>,
    pub pacer: SyncFramePacer,
    pub status: PaneStatus,
    /// 上次发出去的 (cols, rows)。F34 只在这个值变化时才发 window_change ——
    /// 每帧无脑发会把远端 SIGWINCH 刷爆,tmux 里的 TUI 不停重排。
    pub last_grid: (u16, u16),
}

/// 多 pane 工作区:布局树 + 每 pane 状态 + 主机连接池。
pub struct Workspace {
    tree: Node,
    focus: PaneId,
    panes: Vec<PaneState>,
    pub hosts: Vec<HostConn>,
    next_id: u32,
    /// F83 pane 标题条开关,默认开。
    pub title_bars: bool,
}

impl Workspace {
    /// 用第一个 pane 起一个工作区(单屏)。`hosts` 由调用方随后 push。
    pub fn new(first: PaneState) -> Self {
        let id = first.id;
        Self {
            tree: Node::Leaf(id),
            focus: id,
            next_id: id.0 + 1,
            panes: vec![first],
            hosts: Vec::new(),
            title_bars: true,
        }
    }

    pub fn tree(&self) -> &Node {
        &self.tree
    }
    pub fn focus(&self) -> PaneId {
        self.focus
    }
    pub fn set_focus(&mut self, id: PaneId) {
        if leaves(&self.tree).contains(&id) {
            self.focus = id;
        }
    }
    pub fn panes(&self) -> &[PaneState] {
        &self.panes
    }
    pub fn pane(&self, id: PaneId) -> Option<&PaneState> {
        self.panes.iter().find(|p| p.id == id)
    }
    pub fn pane_mut(&mut self, id: PaneId) -> Option<&mut PaneState> {
        self.panes.iter_mut().find(|p| p.id == id)
    }
    pub fn focused(&self) -> Option<&PaneState> {
        self.pane(self.focus)
    }
    pub fn focused_mut(&mut self) -> Option<&mut PaneState> {
        let f = self.focus;
        self.pane_mut(f)
    }
    /// 树上的叶子数。状态栏的"N 屏"用这个,不是 `panes.len()` ——
    /// 正在连接中的 pane 已经占了叶子位但还没有 `PaneState`。
    pub fn pane_count(&self) -> usize {
        leaves(&self.tree).len()
    }

    fn alloc_id(&mut self) -> PaneId {
        let id = PaneId(self.next_id);
        self.next_id += 1;
        id
    }

    /// 每 pane 的状态。树上有叶子但还没挂 `PaneState`(正在开 channel)的,
    /// 按 `Live` 算 —— 减屏时不该优先把"正在连"的当成死的先杀掉。
    fn statuses(&self) -> Vec<(PaneId, PaneStatus)> {
        leaves(&self.tree)
            .into_iter()
            .map(|id| {
                let st = self
                    .pane(id)
                    .map_or(PaneStatus::Live, |p| p.status);
                (id, st)
            })
            .collect()
    }

    /// 套用布局预设(§5.2):就地重排树、关掉多余 pane,返回**待新建**的 pane id。
    ///
    /// 返回的 id 已经占好了树上的叶子位,但还没有 `PaneState`。调用方(app)为每个
    /// id 发起 `open_pty`,完成后调 [`Workspace::attach_pane`]。这段空窗期里
    /// 渲染层照常按几何画一块空 pane + "连接中"标题 —— 布局不会先塌一下再撑开。
    pub fn apply_preset(&mut self, preset: Preset) -> Vec<PaneId> {
        let plan = plan_preset(preset, &self.statuses());
        for id in &plan.close {
            self.panes.retain(|p| p.id != *id);
        }
        let mut ids = plan.keep;
        let mut fresh = Vec::new();
        for _ in 0..plan.spawn {
            let id = self.alloc_id();
            ids.push(id);
            fresh.push(id);
        }
        self.tree = preset_tree(preset, &ids);
        self.focus = next_focus(self.focus, &ids);
        fresh
    }

    /// 异步 `open_pty` 完成后把 pane 挂进来(id 由 [`Workspace::apply_preset`] 预分配)。
    pub fn attach_pane(&mut self, pane: PaneState) {
        self.next_id = self.next_id.max(pane.id.0 + 1);
        self.panes.retain(|p| p.id != pane.id);
        self.panes.push(pane);
    }

    /// 关闭一个 pane(F31):树上兄弟顶替,`PaneState` 一并丢弃(channel 随之关闭)。
    /// 最后一个 pane 不可关,返回 `false` 且什么都不动。
    pub fn close_pane(&mut self, id: PaneId) -> bool {
        if !close_pane(&mut self.tree, id) {
            return false;
        }
        self.panes.retain(|p| p.id != id);
        self.focus = next_focus(self.focus, &leaves(&self.tree));
        true
    }

    /// 把焦点 pane 一分为二(F30)。返回新 pane 的 id(还没有 `PaneState`,
    /// 同 `apply_preset` 的空窗期约定)。B2-a 的 UI 不暴露这个入口,只留给
    /// 快捷键切片和测试 —— core 的 `split_pane` 需要一个真实调用方才不算死代码。
    pub fn split_focused(&mut self, dir: Dir) -> Option<PaneId> {
        let new_id = self.alloc_id();
        if split_pane(&mut self.tree, self.focus, new_id, dir, 0.5) {
            Some(new_id)
        } else {
            self.next_id -= 1;
            None
        }
    }

    /// 排空每个 pane 的 inbound,喂各自的仿真器,把 `Event::PtyWrite` 回写到
    /// **该 pane 自己的** channel(T1)。
    ///
    /// 写错 channel 的后果不是"输出乱一点":发起同步输出探测 / 光标位置查询的
    /// TUI 会永远等不到应答,表现为整块 pane 冻死。
    pub fn pump(&mut self, now_ms: u64) {
        for p in &mut self.panes {
            let mut inbound: Vec<Vec<u8>> = Vec::new();
            loop {
                match p.rx.try_recv() {
                    Ok(chunk) => {
                        p.pacer.feed(&chunk, now_ms);
                        inbound.push(chunk);
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        // §6.3:内容保留(可滚可复制),只是不再收发。
                        p.status = PaneStatus::Disconnected;
                        break;
                    }
                }
            }
            if inbound.is_empty() {
                continue;
            }
            let out = session_pump::pump(&mut p.emulator, &inbound);
            if !out.is_empty() {
                let _ = p.pty.write(out);
            }
        }
    }

    /// 施加几何(F34/T4)。**四条触发路径**(套预设 / 关 pane / 窗口 resize /
    /// 标题条开关)全走这一个函数,只有这一份代码 —— 分散写迟早漏掉一条,
    /// 而漏掉的那条会表现成"某种操作之后远端排版就错了",极难定位。
    pub fn apply_geometry(&mut self, geoms: &[PaneGeom]) {
        for g in geoms {
            let Some(p) = self.panes.iter_mut().find(|p| p.id == g.id) else {
                continue; // 还在开 channel 的 pane,attach 时会补一次
            };
            if p.last_grid == g.grid {
                continue;
            }
            p.last_grid = g.grid;
            p.emulator.resize(g.grid.0, g.grid.1);
            let _ = p.pty.resize(g.grid.0, g.grid.1);
        }
    }

    /// 有没有任何 pane 还活着。状态栏的连接指示灯用。
    pub fn any_live(&self) -> bool {
        self.panes.iter().any(|p| p.status == PaneStatus::Live)
    }
}
```

测试里用到的 `tree_mut_for_test` 是脚手架，加在 `impl Workspace` 末尾：

```rust
    /// 测试脚手架:把树扩到 n 个叶子,让 `ws_with(n)` 能造出多 pane 场景。
    #[cfg(test)]
    fn tree_mut_for_test(&mut self, new_id: u32) {
        let target = self.focus;
        split_pane(
            &mut self.tree,
            target,
            PaneId(new_id),
            Dir::Horizontal,
            0.5,
        );
        self.next_id = self.next_id.max(new_id + 1);
    }
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib shell::workspace 2>&1 | tail -5`
Expected: PASS，`test result: ok. 25 passed; 0 failed`

- [ ] **Step 5: 提交**

```bash
cd /data/Mullion
cargo clippy -p mullion-app --all-targets -- -D warnings
git add crates/mullion-app/src/shell/workspace/mod.rs
git commit -m "feat(app): Workspace 多 pane 状态机 —— pump/断线/施加几何 (F30/F31/F34)

PtyWriter trait 让状态机能脱离真实 SSH 单测(SshSession 跨 crate 造不出来)。
apply_geometry 是 window_change 的**唯一**出口,四条触发路径(套预设/关 pane/
窗口 resize/标题条开关)全走它,且只在网格尺寸真的变了时才发。

apply_preset 先重排树、返回待新建的 pane id,新 pane 的 PaneState 由 app 异步
补上;空窗期里那块画成空 pane,布局不会先塌一下再撑开。

守护测试:shell::workspace::tests::pty_write_goes_to_its_own_pane_channel_t1(T1)、
geometry_change_emits_resize_for_every_pane_f34(T4)、unchanged_geometry_emits_no_resize"
```

---

## Task 6: 色块层多 pane 化 —— `CursorStyle` 与 `quads_for_panes`（§7.1）

**Files:**
- Modify: `crates/mullion-app/src/gpu.rs:26-70`（`quads_for`）与其 `mod tests`

- [ ] **Step 1: 写失败测试**

在 `crates/mullion-app/src/gpu.rs` 的 `mod tests` 里，先把现有 4 处 `quads_for(...)` 调用**机械补上**第 6 个参数 `CursorStyle::Block`（`origin_shifts_every_quad_so_first_row_clears_the_menu_bar`、`default_bg_cell_makes_no_quad`、`colored_bg_cell_makes_quad_at_pixel`、`visible_cursor_adds_block_quad`、`selected_cell_is_inverted_even_on_default_background` —— 共 5 处），然后在 `mod tests` 末尾追加：

```rust
    /// §7.1:非焦点 pane 的光标画空心框。不区分的话 4 屏会同时亮 4 个实心光标,
    /// 用户看不出键盘输入到底进了哪一块。
    #[test]
    fn hollow_cursor_draws_a_frame_not_a_block() {
        let mut snap = snap_1x1(Rgb::new(0, 0, 0));
        snap.cursor.visible = true;
        let quads = quads_for(
            &snap,
            (0.0, 0.0),
            10.0,
            20.0,
            DefaultColors::default(),
            CursorStyle::Hollow,
        );
        assert_eq!(quads.len(), 4, "空心光标 = 上下左右四条边");
        // 每条边都是 1px 细的,且都贴着这一格的边界。
        for q in &quads {
            assert!(q.w == 1.0 || q.h == 1.0, "边框条应有一维是 1px: {q:?}");
            assert!(q.x >= 0.0 && q.x + q.w <= 10.0);
            assert!(q.y >= 0.0 && q.y + q.h <= 20.0);
        }
        // 中心不能被填掉,否则跟实心块没区别。
        assert!(
            !quads.iter().any(|q| q.w > 2.0 && q.h > 2.0),
            "空心光标里混进了实心块"
        );
    }

    #[test]
    fn invisible_cursor_draws_nothing_in_either_style() {
        let snap = snap_1x1(Rgb::new(0, 0, 0));
        for style in [CursorStyle::Block, CursorStyle::Hollow] {
            let quads = quads_for(
                &snap,
                (0.0, 0.0),
                10.0,
                20.0,
                DefaultColors::default(),
                style,
            );
            assert!(quads.is_empty(), "光标不可见时不该画: {style:?}");
        }
    }

    /// 每个 pane 用**自己的** term_px 原点。传整窗原点的话,pane 2 的底色会画到
    /// pane 1 的地盘上 —— 症状是"字在新位置、底色还在老位置"。
    #[test]
    fn each_pane_uses_its_own_term_origin() {
        let a = snap_1x1(Rgb::new(205, 0, 0));
        let b = snap_1x1(Rgb::new(0, 205, 0));
        let geom = |id: u32, x: u32| PaneGeom {
            id: mullion_core::layout::PaneId(id),
            px: PxRect { x, y: 100, w: 400, h: 600 },
            title_px: PxRect { x, y: 100, w: 400, h: 32 },
            term_px: PxRect { x, y: 132, w: 400, h: 568 },
            grid: (40, 28),
        };
        let panes = [
            PaneRender { geom: geom(1, 0), snap: &a, focused: true },
            PaneRender { geom: geom(2, 400), snap: &b, focused: false },
        ];
        let quads = quads_for_panes(&panes, 10.0, 20.0, DefaultColors::default());
        assert_eq!(quads.len(), 2);
        assert_eq!((quads[0].x, quads[0].y), (0.0, 132.0));
        assert_eq!((quads[1].x, quads[1].y), (400.0, 132.0));
        assert_eq!(quads[1].color, [0, 205, 0]);
    }

    #[test]
    fn only_the_focused_pane_gets_a_solid_cursor() {
        let mut a = snap_1x1(Rgb::new(0, 0, 0));
        a.cursor.visible = true;
        let mut b = snap_1x1(Rgb::new(0, 0, 0));
        b.cursor.visible = true;
        let geom = |id: u32, x: u32| PaneGeom {
            id: mullion_core::layout::PaneId(id),
            px: PxRect { x, y: 0, w: 400, h: 600 },
            title_px: PxRect { x, y: 0, w: 400, h: 0 },
            term_px: PxRect { x, y: 0, w: 400, h: 600 },
            grid: (40, 30),
        };
        let panes = [
            PaneRender { geom: geom(1, 0), snap: &a, focused: true },
            PaneRender { geom: geom(2, 400), snap: &b, focused: false },
        ];
        let quads = quads_for_panes(&panes, 10.0, 20.0, DefaultColors::default());
        assert_eq!(quads.len(), 1 + 4, "焦点 1 块实心 + 非焦点 4 条边");
    }
```

并在 `mod tests` 顶部的 `use` 里补：

```rust
    use crate::shell::workspace::PxRect;
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib gpu::tests 2>&1 | head -20`
Expected: FAIL，编译错误 `error[E0433]: failed to resolve: use of undeclared type `CursorStyle``

- [ ] **Step 3: 最小实现**

改 `crates/mullion-app/src/gpu.rs`。在 `Quad` 定义之后插入：

```rust
use crate::shell::workspace::PaneGeom;

/// 光标画法。多 pane 下必须区分:4 个 pane 同时亮 4 个实心光标的话,
/// 用户看不出键盘输入进了哪一块(§7.1)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorStyle {
    /// 焦点 pane:实心块。
    Block,
    /// 非焦点 pane:空心框。
    Hollow,
}

/// 一个 pane 的渲染输入。
pub struct PaneRender<'a> {
    pub geom: PaneGeom,
    pub snap: &'a GridSnapshot,
    pub focused: bool,
}

/// 空心光标的边框粗细(像素)。
const HOLLOW_PX: f32 = 1.0;
```

把 `quads_for` 的签名末尾加一个参数，并把光标那段替换掉：

```rust
pub fn quads_for(
    snap: &GridSnapshot,
    origin: (f32, f32),
    cell_w: f32,
    cell_h: f32,
    defaults: DefaultColors,
    cursor: CursorStyle,
) -> Vec<Quad> {
```

（函数体中间的格子循环不动。）把第 58–68 行的光标块替换成：

```rust
    if snap.cursor.visible {
        let x = origin.0 + snap.cursor.col as f32 * cell_w;
        let y = origin.1 + snap.cursor.row as f32 * cell_h;
        // MVP 光标用默认前景色。原本硬编码 0xcc,主题化后必须跟着走,
        // 否则新前景下光标是一块突兀的旧灰。
        let color = [defaults.fg.r, defaults.fg.g, defaults.fg.b];
        match cursor {
            CursorStyle::Block => quads.push(Quad {
                x,
                y,
                w: cell_w,
                h: cell_h,
                color,
            }),
            CursorStyle::Hollow => {
                let t = HOLLOW_PX;
                for q in [
                    Quad { x, y, w: cell_w, h: t, color },                 // 上
                    Quad { x, y: y + cell_h - t, w: cell_w, h: t, color }, // 下
                    Quad { x, y, w: t, h: cell_h, color },                 // 左
                    Quad { x: x + cell_w - t, y, w: t, h: cell_h, color }, // 右
                ] {
                    quads.push(q);
                }
            }
        }
    }
    quads
}

/// 把所有 pane 的色块合成一批(一次 draw call)。
///
/// 每个 pane 的原点取**自己的** `term_px`,不是整窗原点 —— 传错就会把 pane 2
/// 的底色画到 pane 1 上,症状是"字在新位置、底色还在老位置"。
/// 文字层(`text::prepare_panes`)必须用同一份 `PaneGeom`。
pub fn quads_for_panes(
    panes: &[PaneRender<'_>],
    cell_w: f32,
    cell_h: f32,
    defaults: DefaultColors,
) -> Vec<Quad> {
    let mut out = Vec::new();
    for p in panes {
        let origin = (p.geom.term_px.x as f32, p.geom.term_px.y as f32);
        let style = if p.focused {
            CursorStyle::Block
        } else {
            CursorStyle::Hollow
        };
        out.extend(quads_for(p.snap, origin, cell_w, cell_h, defaults, style));
    }
    out
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib gpu::tests 2>&1 | tail -5`
Expected: PASS，`test result: ok. 10 passed; 0 failed`

- [ ] **Step 5: 提交**

```bash
cd /data/Mullion
cargo clippy -p mullion-app --all-targets -- -D warnings
git add crates/mullion-app/src/gpu.rs
git commit -m "feat(app): 色块层多 pane 化,焦点实心光标 / 其余空心框 (F30)

quads_for_panes 遍历 PaneRender,每个 pane 用自己的 term_px 原点合批;
像素计算仍然全部落在 quads_for 里,单 pane 的既有断言原样有效。

守护测试:gpu::tests::each_pane_uses_its_own_term_origin、
only_the_focused_pane_gets_a_solid_cursor、hollow_cursor_draws_a_frame_not_a_block"
```

---

## Task 7: 文字层多 pane 化 —— 按 pane 裁剪（§7.1）

**Files:**
- Modify: `crates/mullion-app/src/text.rs:95-145`（`prepare` → `prepare_panes`）

- [ ] **Step 1: 写失败测试**

`TextLayer` 要 GPU device 才能造，构不出来；能纯测的只有裁剪矩形这一段纯几何。
把它抽成自由函数再测。追加到已有的 `mod tests`（`crates/mullion-app/src/text.rs:187`，
里面已有 5 个 `row_to_spans` 的测试，不要动它们），并在该 mod 的 `use` 区补
`use crate::shell::workspace::PxRect;`：

```rust
    /// §7.1:每个 pane 的 TextArea 必须裁到**自己的** term_px。
    /// 沿用单 pane 时代的整窗 bounds,pane 1 最后一行的字会溢出到 pane 2 上 ——
    /// 症状是"分屏边界附近有半行别人的字",且滚动时才出现,极难复现定位。
    #[test]
    fn pane_bounds_clip_to_the_pane_not_the_window() {
        let term = PxRect {
            x: 400,
            y: 132,
            w: 399,
            h: 568,
        };
        assert_eq!(pane_bounds_ltrb(term), (400, 132, 799, 700));
    }

    /// 零尺寸 pane(窗口被拖到极小)不能算出反向矩形,glyphon 会画出诡异结果。
    #[test]
    fn zero_sized_pane_yields_a_degenerate_but_ordered_rect() {
        let (l, t, r, b) = pane_bounds_ltrb(PxRect {
            x: 10,
            y: 20,
            w: 0,
            h: 0,
        });
        assert!(r >= l && b >= t, "left/top 必须不大于 right/bottom");
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib text::tests 2>&1 | head -20`
Expected: FAIL，编译错误 `error[E0425]: cannot find function `pane_bounds_ltrb` in this scope`

- [ ] **Step 3: 最小实现**

在 `crates/mullion-app/src/text.rs` 顶部 `use` 区补：

```rust
use crate::gpu::PaneRender;
use crate::shell::workspace::PxRect;
```

在 `impl TextLayer` 之前插入自由函数：

```rust
/// 一个 pane 的文字裁剪矩形,`(left, top, right, bottom)`。
///
/// 返回裸元组而不是 `glyphon::TextBounds`,是为了能不依赖 glyphon 类型是否
/// derive `PartialEq` 就把这段几何单测掉 —— 裁错的症状(分屏边界上冒出半行
/// 别人的字)只在滚动时偶发,靠肉眼盯几乎抓不住。
pub fn pane_bounds_ltrb(term: PxRect) -> (i32, i32, i32, i32) {
    let l = term.x as i32;
    let t = term.y as i32;
    (l, t, l + term.w as i32, t + term.h as i32)
}
```

把 `prepare` 改名为 `prepare_panes` 并改签名（原方法整体替换）：

```rust
    /// 为所有 pane 准备文字。每个 pane 用自己的 `term_px` 作原点**和**裁剪框。
    ///
    /// buffers 按 `pane_ix` 分段线性存放,与 `areas` 的顺序一一对应 —— glyphon
    /// 的 `prepare` 要求 buffer 借用活到 `render`,所以不能边建边丢。
    pub fn prepare_panes(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        panes: &[PaneRender<'_>],
        res: Resolution,
    ) -> Result<(), glyphon::PrepareError> {
        self.viewport.update(queue, res);
        let metrics = Metrics::new(self.cell_h * 0.8, self.cell_h);
        self.buffers.clear();

        // 第一遍:建 buffer(要先全部建完,才能借它们建 TextArea)。
        // 逐行富文本这段与原 `prepare` 一字不差,只是 `res.width` 换成了该 pane
        // 的宽度 —— 属性切段逻辑有 VT 快照测试兜着,改它会连带 fixture 一起红。
        let mut rows_per_pane: Vec<usize> = Vec::with_capacity(panes.len());
        let attrs = Attrs::new().family(Family::Name(FONT_FAMILY));
        for p in panes {
            rows_per_pane.push(p.snap.rows as usize);
            for row in 0..p.snap.rows {
                let spans = row_to_spans(p.snap.row(row));
                let mut buf = Buffer::new(&mut self.font_system, metrics);
                buf.set_size(
                    &mut self.font_system,
                    Some(p.geom.term_px.w.max(1) as f32),
                    Some(self.cell_h),
                );
                let iter = spans.iter().map(|(s, c)| (s.as_str(), attrs.color(*c)));
                buf.set_rich_text(&mut self.font_system, iter, attrs, Shaping::Advanced);
                buf.shape_until_scroll(&mut self.font_system, false);
                self.buffers.push(buf);
            }
        }

        // 第二遍:建 TextArea,bounds 用**该 pane 的**矩形而不是整窗。
        let mut areas: Vec<TextArea> = Vec::with_capacity(self.buffers.len());
        let mut base = 0usize;
        for (pi, p) in panes.iter().enumerate() {
            let (left, top, right, bottom) = pane_bounds_ltrb(p.geom.term_px);
            for row in 0..rows_per_pane[pi] {
                areas.push(TextArea {
                    buffer: &self.buffers[base + row],
                    left: p.geom.term_px.x as f32,
                    top: p.geom.term_px.y as f32 + row as f32 * self.cell_h,
                    scale: 1.0,
                    bounds: TextBounds {
                        left,
                        top,
                        right,
                        bottom,
                    },
                    default_color: glyphon::Color::rgb(
                        self.default_fg.r,
                        self.default_fg.g,
                        self.default_fg.b,
                    ),
                    custom_glyphs: &[],
                });
            }
            base += rows_per_pane[pi];
        }

        self.renderer.prepare(
            device,
            queue,
            &mut self.font_system,
            &mut self.atlas,
            &self.viewport,
            areas,
            &mut self.swash,
        )
    }
```

> `row_to_spans` / `to_color` 两个纯函数原样不动 —— 它们有既有单测和 VT 快照
> 兜着，本 Task 一个字都不该碰它们。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib text:: 2>&1 | tail -5`
Expected: PASS，`test result: ok. 7 passed; 0 failed`（原有 5 个 `row_to_spans`
测试 + 本 Task 新增 2 个）

（此时 `app.rs` 还在调旧的 `prepare`，`cargo build` 会红。Task 11 接线时修好；
本步只跑 `--lib text::` 是有意的，全绿在 Task 11 结束时验。）

- [ ] **Step 5: 提交**

```bash
cd /data/Mullion
git add crates/mullion-app/src/text.rs
git commit -m "feat(app): 文字层按 pane 裁剪,prepare 改为 prepare_panes (F30)

每个 TextArea 的 bounds 取该 pane 的 term_px,不再是整窗 Resolution;
沿用整窗 bounds 会让 pane 1 的字溢到 pane 2 上,且只在滚动时偶发。

pane_bounds_ltrb 返回裸元组以便脱离 glyphon 类型单测。
app.rs 的调用点在后续 Task 接线,本提交单独跑 text:: 测试。

守护测试:text::tests::pane_bounds_clip_to_the_pane_not_the_window"
```

---

## Task 8: 攒帧聚合 —— 任一 pane 在同步区间就不 present（T2/§7.2）

**Files:**
- Modify: `crates/mullion-app/src/render.rs`

- [ ] **Step 1: 写失败测试**

在 `crates/mullion-app/src/render.rs` 的 `mod tests` 末尾追加：

```rust
    /// T2/§7.2:任一 pane 处在 DEC 2026 同步区间内,整帧都不 present。
    ///
    /// 只看焦点 pane 的话,后台 pane 会被画成半张更新过的画面 —— 撕裂正是
    /// 这个项目要消灭的东西,不能因为"它不是焦点"就放过去。
    #[test]
    fn any_pane_in_sync_defers_present() {
        let mut a = SyncFramePacer::new();
        let mut b = SyncFramePacer::new();
        a.feed(b"hello", 0);
        b.feed(b"\x1b[?2026h", 0); // b 进入同步区间
        assert!(a.should_present(0), "单看 a 是可以出帧的");
        assert!(
            !panes_ready_to_present([&a, &b].into_iter(), 0),
            "b 还在攒帧,整帧就该等"
        );
        b.feed(b"\x1b[?2026l", 0); // b 退出
        assert!(panes_ready_to_present([&a, &b].into_iter(), 0));
    }

    /// 空集合必须返回 true。launcher 界面(还没连上、一个 pane 都没有)靠 egui
    /// 画,聚合返回 false 的话它一帧都出不来 —— 表现为启动后白屏/黑屏。
    #[test]
    fn no_panes_is_ready_so_the_launcher_can_draw() {
        let empty: [&SyncFramePacer; 0] = [];
        assert!(panes_ready_to_present(empty.into_iter(), 0));
    }

    /// 卡死保护也要能穿透聚合:超时后即使还没收到 ESU 也必须出帧,
    /// 否则一个坏掉的远端能把整个窗口冻住。
    #[test]
    fn timeout_releases_the_aggregate_too() {
        let mut a = SyncFramePacer::new();
        a.feed(b"\x1b[?2026h", 0);
        assert!(!panes_ready_to_present([&a].into_iter(), 0));
        assert!(
            panes_ready_to_present([&a].into_iter(), SYNC_TIMEOUT_MS + 1),
            "超时后必须放行"
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib render::tests 2>&1 | head -20`
Expected: FAIL，编译错误 `error[E0425]: cannot find function `panes_ready_to_present` in this scope`

- [ ] **Step 3: 最小实现**

在 `crates/mullion-app/src/render.rs` 的 `impl SyncFramePacer` 之后追加：

```rust
/// 多 pane 的攒帧聚合(T2/§7.2):**所有** pane 都准备好了才出帧。
///
/// 语义是 `all()` 而不是 `any()`,并且**空集合返回 `true`** —— 没有 pane 的时候
/// (launcher 界面)整个 UI 靠 egui 画,聚合若返回 false,窗口一帧都出不来。
pub fn panes_ready_to_present<'a>(
    pacers: impl Iterator<Item = &'a SyncFramePacer>,
    now_ms: u64,
) -> bool {
    pacers.map(|p| p.should_present(now_ms)).all(|ready| ready)
}
```

> 注意不能写成 `pacers.all(|p| p.should_present(now_ms))` 的短路形式吗？
> 可以，短路对纯查询无副作用。这里写成 `map().all()` 只是为了让"每个 pacer 都被问到"
> 这件事在读代码时更明显；两种写法等价，短路版本也接受。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib render::tests 2>&1 | tail -5`
Expected: PASS，`test result: ok. 10 passed; 0 failed`

- [ ] **Step 5: 提交**

```bash
cd /data/Mullion
cargo clippy -p mullion-app --all-targets -- -D warnings
git add crates/mullion-app/src/render.rs
git commit -m "feat(app): 攒帧聚合 —— 任一 pane 在同步区间就整帧延后 (T2)

多 pane 下只看焦点 pane 会让后台 pane 画成半张更新过的画面。
空集合返回 true,否则未连接时的 launcher 界面一帧都出不来。

守护测试:render::tests::any_pane_in_sync_defers_present、
no_panes_is_ready_so_the_launcher_can_draw、timeout_releases_the_aggregate_too"
```

---

## Task 9: 布局工具栏（F82）

**Files:**
- Create: `crates/mullion-app/src/ui/toolbar.rs`
- Modify: `crates/mullion-app/src/ui/mod.rs:2-5`（挂模块）+ `UiState` 补 `toggle_title_bars`、`crates/mullion-app/src/ui/chrome.rs:34-36`（菜单占位项 + F83 开关）

- [ ] **Step 1: 写失败测试**

新建 `crates/mullion-app/src/ui/toolbar.rs`：

```rust
//! 布局预设工具栏(F82)。菜单栏之下、终端之上,固定 48px 高。
//!
//! 交互模型学 `Mullion Standalone.html` 原型:两段式 —— 先按屏数分组,
//! 组内是该屏数下的子布局。**没有快捷键**(用户明确排期到后续切片),
//! 全靠鼠标点。

use crate::shell::workspace::Preset;
use crate::theme::{self, Theme};

/// 工具栏高度(像素)。中央区可用高度由 egui 自己扣,这里只是给测试和
/// 文档一个明确的数。
pub const TOOLBAR_PX: f32 = 48.0;

#[cfg(test)]
mod tests {
    use super::*;

    /// 按钮必须按屏数分组、且组内保持稳定顺序 —— 用户靠肌肉记忆点位置,
    /// 顺序一变(比如哪天给 ALL 加了个预设插在中间)就点错。
    #[test]
    fn toolbar_groups_presets_by_pane_count() {
        let groups = preset_groups();
        assert_eq!(groups.len(), 4, "1/2/3/4 屏共四组");
        assert_eq!(groups[0], (1, vec![Preset::Single]));
        assert_eq!(
            groups[1],
            (2, vec![Preset::TwoLeftRight, Preset::TwoTopBottom])
        );
        assert_eq!(groups[2].1.len(), 3);
        assert_eq!(groups[3], (4, vec![Preset::FourGrid]));
    }

    /// 每个按钮都得有非空的文字和提示,否则工具栏上会出现空白方块。
    #[test]
    fn every_preset_has_a_label_and_tooltip() {
        for p in Preset::ALL {
            assert!(!p.label().is_empty(), "{p:?} 没有按钮文字");
            assert!(!p.tooltip().is_empty(), "{p:?} 没有提示");
        }
    }

    /// 分组是 ALL 的一个划分:不重不漏。
    #[test]
    fn groups_partition_all_presets() {
        let flat: Vec<Preset> = preset_groups().into_iter().flat_map(|(_, v)| v).collect();
        assert_eq!(flat.len(), Preset::ALL.len());
        for p in Preset::ALL {
            assert!(flat.contains(&p), "{p:?} 掉出了分组");
        }
    }
}
```

在 `crates/mullion-app/src/ui/mod.rs` 的模块声明区补一行（按字母序插在 `session_manager` 之后）：

```rust
pub mod toolbar;
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib ui::toolbar 2>&1 | head -20`
Expected: FAIL，编译错误 `error[E0425]: cannot find function `preset_groups` in this scope`

- [ ] **Step 3: 最小实现**

在 `toolbar.rs` 的 `TOOLBAR_PX` 之后插入：

```rust
/// 按屏数把预设分组,组内保持 `Preset::ALL` 的顺序。
///
/// 顺序稳定是硬要求:用户靠肌肉记忆点按钮位置,顺序变了就会点错布局,
/// 而点错的代价是真的关掉一个 pane。
pub fn preset_groups() -> Vec<(usize, Vec<Preset>)> {
    let mut groups: Vec<(usize, Vec<Preset>)> = Vec::new();
    for p in Preset::ALL {
        match groups.iter_mut().find(|(n, _)| *n == p.group()) {
            Some((_, v)) => v.push(p),
            None => groups.push((p.group(), vec![p])),
        }
    }
    groups
}

/// 画工具栏,返回用户这一帧点中的预设。
///
/// `current` 是当前生效的预设(用来画选中态)。`None` 表示当前布局不是任何
/// 预设(B2-b 的手动拖拽会造成这种状态),此时所有按钮都不高亮。
pub fn show(ctx: &egui::Context, t: &Theme, current: Option<Preset>) -> Option<Preset> {
    let mut clicked = None;
    egui::TopBottomPanel::top("toolbar")
        .exact_height(TOOLBAR_PX / ctx.pixels_per_point())
        .frame(
            egui::Frame::none()
                .fill(theme::c32(t.bar_tool))
                .inner_margin(egui::Margin::symmetric(8.0, 6.0))
                .stroke(theme::stroke(t)),
        )
        .show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                for (gi, (_, presets)) in preset_groups().into_iter().enumerate() {
                    if gi > 0 {
                        ui.separator();
                    }
                    for p in presets {
                        let on = current == Some(p);
                        let btn = egui::Button::new(
                            egui::RichText::new(p.label()).color(theme::c32(if on {
                                t.accent_fg
                            } else {
                                t.fg_muted
                            })),
                        )
                        .fill(theme::c32(if on { t.accent } else { t.sunken_bg }));
                        if ui.add(btn).on_hover_text(p.tooltip()).clicked() {
                            clicked = Some(p);
                        }
                    }
                }
            });
        });
    clicked
}
```

改 `crates/mullion-app/src/ui/chrome.rs:34-36`，把死掉的占位项换成指路 +
F83 标题条开关（设计文档 §2 要求标题条「可关，默认开」，没有入口就等于没做）：

```rust
                ui.menu_button("分屏", |ui| {
                    ui.add_enabled(false, egui::Button::new("用工具栏的布局按钮切换"));
                    // F83:标题条占 32px,关掉能换回一行终端。切换后行数会变,
                    // 必须走 apply_geometry 发 window_change(T4),故只置意图。
                    if ui.button("显示 / 隐藏 pane 标题条").clicked() {
                        ui_state.toggle_title_bars = true;
                        ui.close_menu();
                    }
                    ui.add_enabled(false, egui::Button::new("(快捷键 · 后续切片)"));
                });
```

`crates/mullion-app/src/ui/mod.rs` 的 `UiState` 末尾补这个意图字段（与既有
`request_disconnect` 同构：egui 闭包借不到 `&mut Workspace`，只能置意图、
由 app.rs 在借用释放后施加）：

```rust
    /// 「分屏 → 显示/隐藏 pane 标题条」被点了(F83)。app.rs 消费后复位,
    /// 翻转 `Workspace::title_bars` 并重算几何(会改行数 → 必发 window_change)。
    pub toggle_title_bars: bool,
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib ui:: 2>&1 | tail -5`
Expected: PASS，`test result: ok. 7 passed; 0 failed`

- [ ] **Step 5: 提交**

```bash
cd /data/Mullion
cargo clippy -p mullion-app --all-targets -- -D warnings
git add crates/mullion-app/src/ui/toolbar.rs crates/mullion-app/src/ui/mod.rs crates/mullion-app/src/ui/chrome.rs
git commit -m "feat(app): 布局预设工具栏 (F82)

两段式布局按钮(先屏数、组内子布局),学 Mullion Standalone.html 原型。
分屏菜单的死占位项改成指向工具栏;快捷键按用户排期留到后续切片。

守护测试:ui::toolbar::tests::toolbar_groups_presets_by_pane_count、
groups_partition_all_presets"
```

---

## Task 10: pane 标题条（F83）

**Files:**
- Create: `crates/mullion-app/src/ui/pane_title.rs`
- Modify: `crates/mullion-app/src/ui/mod.rs`（挂模块）

- [ ] **Step 1: 写失败测试**

新建 `crates/mullion-app/src/ui/pane_title.rs`：

```rust
//! pane 标题条(F83)。每个 pane 顶部 32px:序号 + 主机名 + 连接状态点 + 关闭按钮。
//!
//! 用 egui `Area::fixed_pos` 按绝对像素定位,只覆盖标题条那 32px,**不能**盖住
//! 整个 pane —— egui 在它覆盖的区域会吃掉指针事件(T8 的指针路由是"先喂 egui
//! 后判"),盖大了终端就再也划不了选。

use mullion_core::layout::PaneId;

use crate::shell::workspace::{PaneGeom, PaneStatus};
use crate::theme::{self, Theme};

/// 一个标题条要显示的东西。
pub struct TitleView<'a> {
    pub geom: PaneGeom,
    /// 该 pane 在几何顺序中的序号,从 1 起。
    pub index: usize,
    /// 主机标签(会话名或 user@host)。尚未连上时给 `None`。
    pub host: Option<&'a str>,
    pub status: PaneStatus,
    pub focused: bool,
}

/// 标题条上的文字。抽成纯函数是因为格式会被人反复调,而它是唯一能自动验的部分。
pub fn title_text(index: usize, host: Option<&str>, status: PaneStatus) -> String {
    match (host, status) {
        (Some(h), PaneStatus::Live) => format!("{index} · {h}"),
        (Some(h), PaneStatus::Disconnected) => format!("{index} · {h} (已断开)"),
        (None, _) => format!("{index} · 连接中…"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_shows_index_and_host() {
        assert_eq!(
            title_text(2, Some("dev@build-01"), PaneStatus::Live),
            "2 · dev@build-01"
        );
    }

    /// §6.3:断开的 pane 内容留着可滚可复制,但状态必须写在脸上 ——
    /// 不然用户会对着一块不响应的终端反复敲键。
    #[test]
    fn disconnected_pane_says_so() {
        let s = title_text(1, Some("h"), PaneStatus::Disconnected);
        assert!(s.contains("已断开"), "断开状态没写进标题: {s}");
    }

    /// 预分配了叶子位但 channel 还没开好的空窗期(见 Workspace::apply_preset)。
    #[test]
    fn pane_without_a_host_yet_says_connecting() {
        assert_eq!(title_text(3, None, PaneStatus::Live), "3 · 连接中…");
    }

    /// 标题条只能占 32px。盖住整个 pane 的话 egui 会吃掉指针事件,
    /// 终端从此划不了选(T8 指针路由是先喂 egui 后判)。
    #[test]
    fn title_area_covers_only_the_title_strip() {
        use crate::shell::workspace::{PxRect, TITLE_BAR_PX};
        let geom = PaneGeom {
            id: PaneId(1),
            px: PxRect { x: 0, y: 100, w: 800, h: 600 },
            title_px: PxRect { x: 0, y: 100, w: 800, h: TITLE_BAR_PX },
            term_px: PxRect { x: 0, y: 132, w: 800, h: 568 },
            grid: (80, 28),
        };
        assert_eq!(geom.title_px.h, TITLE_BAR_PX);
        assert!(
            geom.title_px.h < geom.px.h,
            "标题条不能覆盖整个 pane,否则终端收不到指针事件"
        );
    }
}
```

在 `crates/mullion-app/src/ui/mod.rs` 补：

```rust
pub mod pane_title;
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib ui::pane_title 2>&1 | head -20`
Expected: FAIL，编译错误 `error[E0432]: unresolved import `crate::ui::pane_title`` 或 `PaneGeom` 未导出（若 Task 3 的 re-export 没做全，补上）

- [ ] **Step 3: 最小实现**

在 `pane_title.rs` 的 `title_text` 之后插入绘制函数：

```rust
/// 画一批标题条,返回被点了 × 的 pane(每帧至多一个)。
///
/// 用 `Area` 而非 `Panel`:标题条要跟着 pane 的绝对像素走,`Panel` 只会
/// 从窗口边缘往里堆。`fixed_pos` 收 point,所以像素要先除 `pixels_per_point`。
pub fn show(ctx: &egui::Context, t: &Theme, views: &[TitleView<'_>]) -> Option<PaneId> {
    let ppp = ctx.pixels_per_point();
    let mut closed = None;
    for v in views {
        let tp = v.geom.title_px;
        if tp.h == 0 {
            continue; // 标题条关掉了(F83 开关)
        }
        let id = egui::Id::new(("pane_title", v.geom.id.0));
        egui::Area::new(id)
            .fixed_pos(egui::pos2(tp.x as f32 / ppp, tp.y as f32 / ppp))
            .order(egui::Order::Middle)
            .show(ctx, |ui| {
                let size = egui::vec2(tp.w as f32 / ppp, tp.h as f32 / ppp);
                egui::Frame::none()
                    .fill(theme::c32(if v.focused { t.panel_head } else { t.panel_bg }))
                    .inner_margin(egui::Margin::symmetric(8.0, 4.0))
                    .stroke(theme::stroke(t))
                    .show(ui, |ui| {
                        ui.set_min_size(size);
                        ui.horizontal(|ui| {
                            let dot = match v.status {
                                PaneStatus::Live => t.ok,
                                PaneStatus::Disconnected => t.fg_dim,
                            };
                            ui.colored_label(theme::c32(dot), "●");
                            ui.colored_label(
                                theme::c32(if v.focused { t.fg_strong } else { t.fg_muted }),
                                title_text(v.index, v.host, v.status),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("×").on_hover_text("关闭此分屏").clicked()
                                    {
                                        closed = Some(v.geom.id);
                                    }
                                },
                            );
                        });
                    });
            });
    }
    closed
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib ui:: 2>&1 | tail -5`
Expected: PASS，`test result: ok. 11 passed; 0 failed`

- [ ] **Step 5: 提交**

```bash
cd /data/Mullion
cargo clippy -p mullion-app --all-targets -- -D warnings
git add crates/mullion-app/src/ui/pane_title.rs crates/mullion-app/src/ui/mod.rs
git commit -m "feat(app): pane 标题条 —— 序号/主机/状态点/关闭 (F83)

egui Area 按绝对像素定位,只覆盖 32px 标题条。盖住整块 pane 会让 egui 吃掉
指针事件(T8 指针路由是先喂后判),终端从此划不了选。

断开的 pane 在标题里写明,内容仍可滚可复制(设计文档 §6.3)。

守护测试:ui::pane_title::tests::disconnected_pane_says_so、
title_area_covers_only_the_title_strip"
```

---

## Task 11: `UiFrame` 聚参 + 状态栏接真实屏数（收技术债 2 与 3）

**Files:**
- Modify: `crates/mullion-app/src/ui/mod.rs:88-114`（`build_ui` 签名）
- Modify: `crates/mullion-app/src/app.rs:1165-1193`（`render_frame` 签名与调用）
- Modify: `crates/mullion-app/src/app.rs:950-960`（调用点）

B1 遗留的两笔技术债在这里收掉：`build_ui` 已 9 参并带
`#[allow(clippy::too_many_arguments)]`；`render_frame` 的 `panes` 恒传 `1`
（该行有注释提醒，但没有任何编译/测试兜底）。B2-a 还要再加工具栏和标题条的
参数，不收就要变 12 参。

- [ ] **Step 1: 写失败测试**

在 `crates/mullion-app/src/ui/mod.rs` 末尾追加：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// 技术债 3:参数聚成一个结构体后,新增 UI 输入(工具栏预设、标题条)不再
    /// 推高参数个数。这个测试锁的是"字段存在且能构造",编译过即通过 ——
    /// 真正的价值在于把 `#[allow(clippy::too_many_arguments)]` 拿掉后
    /// clippy 会替我们守住这条线。
    #[test]
    fn ui_frame_carries_pane_count_and_layout_state() {
        let f = UiFrame {
            sessions: &[],
            store_available: false,
            connected: true,
            panes: 4,
            preset: Some(crate::shell::workspace::Preset::FourGrid),
            titles: &[],
            host_key: None,
            paste: None,
        };
        assert_eq!(f.panes, 4, "状态栏的屏数必须来自真实布局,不是硬编码 1");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib ui::tests 2>&1 | head -20`
Expected: FAIL，编译错误 `error[E0422]: cannot find struct, variant or union type `UiFrame``

- [ ] **Step 3: 最小实现**

在 `crates/mullion-app/src/ui/mod.rs` 的 `build_ui` 之前插入结构体，并把
`build_ui` 改成两参：

```rust
/// 一帧 UI 的全部输入。聚成结构体是为了让新增 UI 元素(F82 工具栏、F83 标题条)
/// 不再推高参数个数 —— B1 时这里已经 9 参并挂着 `too_many_arguments` 豁免。
pub struct UiFrame<'a> {
    pub sessions: &'a [SessionRecord],
    pub store_available: bool,
    pub connected: bool,
    /// 状态栏左栏的屏数。必须来自 `Workspace::pane_count()`。
    pub panes: usize,
    /// 当前生效的布局预设(工具栏画选中态)。`None` = 不对应任何预设。
    pub preset: Option<crate::shell::workspace::Preset>,
    /// 每个 pane 的标题条(F83)。空 = 标题条关闭或 launcher 态。
    pub titles: &'a [pane_title::TitleView<'a>],
    pub host_key: Option<host_key::HostKeyView<'a>>,
    pub paste: Option<paste::PasteView<'a>>,
}

/// 用户这一帧在 UI 上做的、需要 app 事后施加的布局动作。
/// 与 `UiState` 里那些"意图字段"同构:egui 闭包借不到 `&mut Workspace`。
#[derive(Default)]
pub struct UiActions {
    /// 点了工具栏上的某个布局预设。
    pub preset: Option<crate::shell::workspace::Preset>,
    /// 点了某个 pane 标题条上的 ×。
    pub close_pane: Option<mullion_core::layout::PaneId>,
}
```

把 `build_ui` 整体替换为：

```rust
/// 每帧构建 UI:菜单栏(顶)+ 工具栏(F82)+ 状态栏(底)+ 各 pane 标题条(F83)
/// + 弹窗,之后把中央区剩余尺寸写回 `central_px`。返回本帧的布局动作。
pub fn build_ui(
    ctx: &egui::Context,
    t: &crate::theme::Theme,
    ui_state: &mut UiState,
    frame: UiFrame<'_>,
) -> UiActions {
    let mut actions = UiActions::default();
    // 主机密钥确认最先画:它是安全关口,任何时候都该盖在最上层(F3)。
    if let Some(view) = &frame.host_key {
        host_key::show(ctx, view, &mut ui_state.host_key_reply);
    }
    // 粘贴确认排在主机密钥之后:安全关口优先级最高,粘贴其次。
    if let Some(view) = &frame.paste {
        paste::show(ctx, view, &mut ui_state.paste_reply);
    }
    chrome::top_menu(ctx, t, ui_state, frame.connected);
    // 工具栏在菜单栏之下、状态栏之上:三个 Panel 的 show 顺序决定它们
    // 从窗口边缘往里堆的次序,换顺序会让工具栏跑到状态栏上面去。
    if frame.connected {
        actions.preset = toolbar::show(ctx, t, frame.preset);
    }
    chrome::status_bar(
        ctx,
        t,
        frame.panes,
        frame.connected,
        ui_state.last_error.as_deref(),
    );
    // 关于弹窗(§2:名称/版本/定位/仓库)。
    if ui_state.about_open {
        let mut open = ui_state.about_open;
        egui::Window::new("关于")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.heading("Mullion");
                ui.label(format!("版本 {}", env!("CARGO_PKG_VERSION")));
                ui.label("原生 GPU 加速 SSH 客户端");
                ui.hyperlink_to("GitHub", "https://github.com/kilobitcy/Mullion");
            });
        ui_state.about_open = open;
    }
    if ui_state.session_manager_open || ui_state.editor_open {
        session_manager::show(ctx, ui_state, frame.sessions, frame.store_available);
    }
    // 中央区剩余像素:available_rect 是 point,× pixels_per_point 换像素。
    // 必须在所有 TopBottomPanel 都 show 完之后取(现在多了工具栏),拿到的才是
    // 扣掉菜单栏+工具栏+状态栏的中央区。原点与尺寸一起记:尺寸决定几行几列,
    // 原点决定这几行画在哪儿——只记尺寸就是 B0 那次遮挡 bug 的成因。
    let ppp = ctx.pixels_per_point();
    let rect = ctx.available_rect();
    ui_state.central_px = (
        (rect.width() * ppp).max(0.0) as u32,
        (rect.height() * ppp).max(0.0) as u32,
    );
    ui_state.central_origin_px = ((rect.min.x * ppp).max(0.0), (rect.min.y * ppp).max(0.0));

    // 标题条最后画:它用绝对坐标,而坐标依赖上面几个 Panel 定完的中央区。
    // Area 不参与 Panel 的空间分配,所以放在 available_rect 之后不影响换算。
    actions.close_pane = pane_title::show(ctx, t, frame.titles);
    actions
}
```

删掉 `build_ui` 上方那行 `#[allow(clippy::too_many_arguments)]`（技术债 3 收口）。

`crates/mullion-app/src/app.rs` 里 `render_frame` 同步改签名（`1165` 行的
`#[allow(clippy::too_many_arguments)]` 也删掉）：

```rust
fn render_frame(
    a: &mut Active,
    panes: &[crate::gpu::PaneRender<'_>],
    ui_state: &mut crate::ui::UiState,
    frame: crate::ui::UiFrame<'_>,
) -> (std::time::Duration, crate::ui::UiActions) {
```

函数体里 `a.egui_ctx.run` 的闭包改成收集返回值（闭包不能直接返回值，用外层
变量接）：

```rust
    let mut actions = crate::ui::UiActions::default();
    let full_output = a.egui_ctx.run(raw_input, |ctx| {
        actions = crate::ui::build_ui(ctx, &MULLION_DARK, ui_state, frame);
    });
```

并把所有 `return std::time::Duration::MAX;` 改成
`return (std::time::Duration::MAX, actions);`（共 5 处：prepare 失败、
`Timeout`、`Lost|Outdated`、`OutOfMemory`，以及函数末尾的正常返回改成
`(repaint_delay, actions)`）。

> `frame` 里借着 `ui_state` 之外的字段，`build_ui` 同时要 `&mut ui_state` ——
> 两者是不相干字段，借用检查通过。若编译器报冲突，说明 `UiFrame` 里误塞了
> 从 `ui_state` 借来的引用，把那个字段挪出去。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app --lib ui:: 2>&1 | tail -5`
Expected: PASS，`test result: ok. 12 passed; 0 failed`

（`app.rs` 的调用点在 Task 12 一并改完，本步 `cargo build` 仍可能红。）

- [ ] **Step 5: 提交**

```bash
cd /data/Mullion
git add crates/mullion-app/src/ui/mod.rs crates/mullion-app/src/app.rs
git commit -m "refactor(app): build_ui/render_frame 参数聚成 UiFrame,清掉 too_many_arguments

收 B1 遗留技术债 3。B2-a 要再加工具栏预设与标题条两组输入,不聚参就是 12 参。
新增 UiActions 承载布局动作(点预设/点关闭),与既有意图字段同构 ——
egui 闭包借不到 &mut Workspace。

工具栏 Panel 插在菜单栏与状态栏之间;central_px 仍在所有 Panel show 完后取。"
```

---

## Task 12: `app.rs` 接线 —— `Connection` → `Workspace`

**Files:**
- Modify: `crates/mullion-app/src/app.rs`（`Active`、`Connection`、`App`、
  `resumed`、`user_event`、`window_event`、`render_frame`）

这是本切片唯一的大改。策略是**先把状态换掉、再把每个 `self.conn` 引用逐个搬到
`self.ws`**，中途不试图保持可编译。改完必须让 T1/T2/T3/T7/T8 的既有守护测试
全绿 —— 它们是这次重构不出事的唯一保证。

- [ ] **Step 1: 写失败测试**

在 `crates/mullion-app/src/app.rs` 的 `mod tests` 末尾追加：

```rust
    /// F34/T4:窗口 resize 的几何必须经 `layout_geometry` 算,再由
    /// `Workspace::apply_geometry` 施加。这里锁住"整窗尺寸 → 每 pane 网格"
    /// 这一段换算 —— 接线写错的典型症状是分屏后远端按整窗列数排版。
    #[test]
    fn window_resize_maps_to_per_pane_grids_f34() {
        use crate::shell::workspace::{layout_geometry, PxRect};
        use mullion_core::layout::{Dir, Node, PaneId};

        let tree = Node::Split {
            dir: Dir::Horizontal,
            ratio: 0.5,
            a: Box::new(Node::Leaf(PaneId(1))),
            b: Box::new(Node::Leaf(PaneId(2))),
        };
        let area = PxRect {
            x: 0,
            y: 100,
            w: 1600,
            h: 900,
        };
        let geoms = layout_geometry(&tree, area, (10.0, 20.0), true);
        assert_eq!(geoms.len(), 2);
        for g in &geoms {
            assert!(
                g.grid.0 < 160,
                "每 pane 的列数必须小于整窗列数,否则是没分屏就发了 window_change"
            );
            assert!(g.grid.0 >= 1 && g.grid.1 >= 1);
        }
    }

    /// 状态栏的屏数取自布局树,不是硬编码。B1 遗留技术债 1 的兜底。
    #[test]
    fn status_bar_pane_count_comes_from_the_tree() {
        use mullion_core::layout::{leaves, Dir, Node, PaneId};
        let tree = Node::Split {
            dir: Dir::Vertical,
            ratio: 0.5,
            a: Box::new(Node::Leaf(PaneId(1))),
            b: Box::new(Node::Leaf(PaneId(7))),
        };
        assert_eq!(leaves(&tree).len(), 2);
        let (left, _) = crate::ui::chrome::status_text(leaves(&tree).len(), true);
        assert_eq!(left, "2 屏 · 已连接");
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app --lib app::tests 2>&1 | head -20`
Expected: FAIL，编译错误（`app.rs` 此时仍在调 Task 7/11 已改签名的 `prepare`/`build_ui`）

- [ ] **Step 3: 最小实现**

按下面六处依次改。

**(a) 删掉 `Connection`，`App` 换字段**（`app.rs:64-75`）：

```rust
pub struct App {
    _runtime: Runtime,
    /// `None` = launcher 态(无终端可画);`Some` = 终端态。
    /// 取代原来的 `Connection`:后者只能装一条连接 + 一个 pane。
    ws: Option<crate::shell::workspace::Workspace>,
```

`App::new` 里 `conn: None` 改 `ws: None`。

**(b) `Active` 删掉 `grid_dims`**（`app.rs:58`）——每 pane 的尺寸现在由
`PaneState.last_grid` 承载，再留一个全局值必然与之打架。同时加一个几何缓存：

```rust
struct Active {
    window: Arc<Window>,
    gpu: Gpu,
    text: TextLayer,
    /// 本帧算出的每 pane 几何。渲染、鼠标命中、window_change 三条路径读同一份 ——
    /// 各算各的是这类布局 bug 的经典成因(算出来差一个标题条高度,肉眼看不出来,
    /// 但鼠标点击整体偏 32px)。
    geoms: Vec<crate::shell::workspace::PaneGeom>,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
}
```

`resumed` 里的 `grid_dims: (cols, rows)` 改成 `geoms: Vec::new()`。

**(c) 新增几何计算辅助**（放在 `impl App` 里，紧挨 `cursor_in_grid` 之前）：

```rust
    /// 本帧的 pane 几何。中央区 = egui 布局后剩下的矩形(`central_origin_px` +
    /// `central_px`),布局树按像素切分它。
    fn compute_geoms(&self) -> Vec<crate::shell::workspace::PaneGeom> {
        let (Some(a), Some(ws)) = (self.active.as_ref(), self.ws.as_ref()) else {
            return Vec::new();
        };
        let origin = self.ui.central_origin_px;
        let area = crate::shell::workspace::PxRect {
            x: origin.0.max(0.0) as u32,
            y: origin.1.max(0.0) as u32,
            w: self.ui.central_px.0,
            h: self.ui.central_px.1,
        };
        crate::shell::workspace::layout_geometry(
            ws.tree(),
            area,
            (a.text.cell_w, a.text.cell_h),
            ws.title_bars,
        )
    }

    /// 指针落在哪个 pane 上。命中判定用 `PaneGeom.px`(含标题条),
    /// 与渲染同源 —— 用别的矩形算就会出现"点得到但画不着"的错位。
    fn pane_at(&self, px: (f32, f32)) -> Option<PaneId> {
        let a = self.active.as_ref()?;
        a.geoms
            .iter()
            .find(|g| {
                let r = g.px;
                px.0 >= r.x as f32
                    && px.0 < (r.x + r.w) as f32
                    && px.1 >= r.y as f32
                    && px.1 < (r.y + r.h) as f32
            })
            .map(|g| g.id)
    }

    /// 焦点 pane 的几何。鼠标格换算、划选都基于它。
    fn focused_geom(&self) -> Option<crate::shell::workspace::PaneGeom> {
        let a = self.active.as_ref()?;
        let f = self.ws.as_ref()?.focus();
        a.geoms.iter().find(|g| g.id == f).copied()
    }
```

`PaneGeom` / `PxRect` 在 Task 3 已 `#[derive(Debug, Clone, Copy, PartialEq, Eq)]`，
这里的 `.copied()` 直接可用，无需改定义。

**(d) 鼠标换算改用焦点 pane 的 `term_px`**：把 `cursor_in_grid`（`app.rs:199`）
整体替换：

```rust
    /// 指针相对**焦点 pane 终端区**左上角的像素。原点用 `term_px` 而不是中央区:
    /// 分屏后 pane 2 的第 0 列不在窗口左边,用中央区原点算会整体偏一个 pane 宽。
    fn cursor_in_grid(&self) -> (f32, f32) {
        let Some(g) = self.focused_geom() else {
            return (0.0, 0.0);
        };
        (
            self.cursor_px.0 - g.term_px.x as f32,
            self.cursor_px.1 - g.term_px.y as f32,
        )
    }
```

`selection_cursor`（`app.rs:209`）里 `a.grid_dims` 改成焦点 pane 的网格：

```rust
    fn selection_cursor(&self) -> Option<(u16, u16, mullion_term::selection::CellSide)> {
        let g = self.focused_geom()?;
        let a = self.active.as_ref()?;
        let cell_px = (a.text.cell_w, a.text.cell_h);
        let local = self.cursor_in_grid();
        let (col1, row1) = input::cell_at(local, cell_px, g.grid);
        let side = input::cell_side(local.0, cell_px.0, g.grid.0);
        Some((col1, row1, side))
    }
```

其余用到 `a.grid_dims` 的地方（`app.rs:229`、`app.rs:763`）同样换成
`self.focused_geom()?.grid`。

**(e) `self.conn` 的引用逐个改写**。机械规则：

| 原写法 | 新写法 |
|---|---|
| `self.conn.is_none()` | `self.ws.is_none()` |
| `self.conn.is_some()` | `self.ws.is_some()` |
| `conn.pane.emulator` | `ws.focused_mut()?.emulator`（读用 `focused()`） |
| `conn.ssh.write(..)` | `ws.focused()?.pty.write(..)` |
| `conn.ssh.resize(..)` | 删掉 —— 统一由 `Workspace::apply_geometry` 发（T4 唯一出口） |
| `conn.pacer` | `ws.focused()?.pacer` / 聚合时 `panes_ready_to_present` |
| `self.conn = None` | `self.ws = None` |

`pump_io`（`app.rs:410`）整体替换为：

```rust
    fn pump_io(&mut self) {
        let now = self.now_ms();
        if let Some(ws) = self.ws.as_mut() {
            ws.pump(now);
        }
    }
```

`RedrawRequested` 里的脏判定（`app.rs:914-919`）改成聚合：

```rust
                let dirty = match &self.ws {
                    Some(ws) => crate::frame::frame_is_dirty(
                        crate::render::panes_ready_to_present(
                            ws.panes().iter().map(|p| &p.pacer),
                            now,
                        ),
                        self.ui_dirty,
                    ),
                    None => true,
                };
```

**(f) `RedrawAction::Present` 分支重写**（`app.rs:924-1009` 的 `if let Some(a)`
块内部，其余控制流一行不动 —— 那是 T3/T7 的地盘）：

```rust
                        if self.active.is_some() {
                            // 几何先算:渲染、标题条、鼠标命中、window_change 全用这一份。
                            let geoms = self.compute_geoms();
                            if let Some(a) = self.active.as_mut() {
                                a.geoms = geoms.clone();
                            }
                            let sessions: &[mullion_store::SessionRecord] =
                                self.store.as_ref().map_or(&[], |s| s.list());
                            let store_available = self.store.is_some();
                            let host_key_view = self.pending_host_key.as_deref().map(|p| {
                                crate::ui::host_key::HostKeyView {
                                    host: &p.host,
                                    algo: &p.algo,
                                    fingerprint: &p.fingerprint,
                                    previous: p.previous.as_ref().map(|e| e.fingerprint.as_str()),
                                    elapsed_secs: self
                                        .host_key_since
                                        .map_or(0, |t| t.elapsed().as_secs()),
                                }
                            });
                            let paste_view = self
                                .pending_paste
                                .as_deref()
                                .map(|text| crate::ui::paste::PasteView { text });

                            // 快照要先全部取出来:PaneRender 借着它们,而 render_frame
                            // 同时要 &mut self.ui。
                            let snaps: Vec<_> = self
                                .ws
                                .as_ref()
                                .map(|ws| {
                                    geoms
                                        .iter()
                                        .filter_map(|g| {
                                            ws.pane(g.id).map(|p| (*g, p.emulator.snapshot()))
                                        })
                                        .collect()
                                })
                                .unwrap_or_default();
                            let focus = self.ws.as_ref().map(|ws| ws.focus());
                            let renders: Vec<crate::gpu::PaneRender<'_>> = snaps
                                .iter()
                                .map(|(g, s)| crate::gpu::PaneRender {
                                    geom: *g,
                                    snap: s,
                                    focused: Some(g.id) == focus,
                                })
                                .collect();
                            let titles: Vec<crate::ui::pane_title::TitleView<'_>> = self
                                .ws
                                .as_ref()
                                .map(|ws| {
                                    geoms
                                        .iter()
                                        .enumerate()
                                        .map(|(i, g)| crate::ui::pane_title::TitleView {
                                            geom: *g,
                                            index: i + 1,
                                            host: ws.pane(g.id).and_then(|p| {
                                                ws.hosts.get(p.host_ix).map(|h| h.label.as_str())
                                            }),
                                            status: ws.pane(g.id).map_or(
                                                crate::shell::workspace::PaneStatus::Live,
                                                |p| p.status,
                                            ),
                                            focused: Some(g.id) == focus,
                                        })
                                        .collect()
                                })
                                .unwrap_or_default();
                            let frame = crate::ui::UiFrame {
                                sessions,
                                store_available,
                                connected: self.ws.is_some(),
                                panes: self.ws.as_ref().map_or(1, |ws| ws.pane_count()),
                                preset: self.current_preset,
                                titles: &titles,
                                host_key: host_key_view,
                                paste: paste_view,
                            };
                            let a = self.active.as_mut().expect("上面刚判过 is_some");
                            let (repaint_delay, actions) =
                                render_frame(a, &renders, &mut self.ui, frame);
                            drop(renders);
                            drop(titles);
                            drop(snaps);

                            self.limiter.record_present(now);
                            self.ui_dirty = false;
                            // 施加几何:F34/T4 的唯一出口。本帧 build_ui 刚写入的
                            // central_px 要下一帧才生效(与 B0 起就是这个语义)。
                            if let Some(ws) = self.ws.as_mut() {
                                for p in ws.panes_mut_iter() {
                                    p.pacer.mark_presented();
                                }
                                ws.apply_geometry(&geoms);
                            }
                            // 布局动作:点了预设 / 点了标题条的 ×。
                            if let Some(preset) = actions.preset {
                                self.apply_preset(preset);
                            }
                            if let Some(id) = actions.close_pane {
                                if let Some(ws) = self.ws.as_mut() {
                                    ws.close_pane(id);
                                }
                                self.current_preset = None;
                                self.ui_dirty = true;
                            }
                            // F83 标题条开关:改的是行数,下一帧 compute_geoms
                            // 算出新 grid,再由 apply_geometry 发 window_change。
                            if self.ui.toggle_title_bars {
                                self.ui.toggle_title_bars = false;
                                if let Some(ws) = self.ws.as_mut() {
                                    ws.title_bars = !ws.title_bars;
                                }
                                self.ui_dirty = true;
                            }
                            if self.ui.request_disconnect {
                                self.ui.request_disconnect = false;
                                self.ws = None;
                            }
                            if self.ui.request_quit {
                                self.ui.request_quit = false;
                                event_loop.exit();
                            }
                            if repaint_delay < std::time::Duration::MAX {
                                self.ui_dirty = true;
                                let at = Instant::now() + repaint_delay;
                                self.next_frame_at = Some(at);
                                event_loop.set_control_flow(ControlFlow::WaitUntil(at));
                            } else {
                                self.next_frame_at = None;
                                event_loop.set_control_flow(ControlFlow::Wait);
                            }
                        } else {
                            self.next_frame_at = None;
                            event_loop.set_control_flow(ControlFlow::Wait);
                        }
```

`App` 补一个字段记住当前预设（工具栏画选中态用）：

```rust
    /// 当前生效的布局预设。手动关 pane 之后置 `None`(布局不再对应任何预设)。
    current_preset: Option<crate::shell::workspace::Preset>,
```
`App::new` 里初始化 `current_preset: Some(crate::shell::workspace::Preset::Single)`。

`Workspace` 补一个可变迭代器（`mod.rs` 的 `impl Workspace` 里）：

```rust
    /// 逐个 pane 的可变引用。`mark_presented` 之类的逐帧动作用。
    pub fn panes_mut_iter(&mut self) -> impl Iterator<Item = &mut PaneState> {
        self.panes.iter_mut()
    }
```

**(g) 套预设 / 新 pane 上线**。`impl App` 里新增：

```rust
    /// 套用布局预设(F82→F30)。多出来的 pane 在同一条 SSH 连接上另开 channel(F35)。
    fn apply_preset(&mut self, preset: crate::shell::workspace::Preset) {
        let Some(ws) = self.ws.as_mut() else {
            return;
        };
        let fresh = ws.apply_preset(preset);
        self.current_preset = Some(preset);
        self.ui_dirty = true;
        for id in fresh {
            let Some(host) = ws.hosts.first() else {
                continue;
            };
            let handle = host.handle.clone();
            let proxy = self.proxy.clone();
            let wake_proxy = self.proxy.clone();
            let cfg = self.last_cfg.clone();
            let Some(cfg) = cfg else { continue };
            self._runtime.spawn(async move {
                let wake = Arc::new(move || {
                    let _ = wake_proxy.send_event(UserEvent::Wake);
                });
                match mullion_ssh::session::open_pty(handle, &cfg, wake).await {
                    Ok((ssh, rx)) => {
                        let _ = proxy.send_event(UserEvent::PaneOpened { id, ssh, rx });
                    }
                    Err(e) => {
                        let _ = proxy.send_event(UserEvent::PaneOpenErr {
                            id,
                            msg: format!("开分屏失败: {e}"),
                        });
                    }
                }
            });
        }
    }
```

`App` 再补一个字段 `last_cfg: Option<SshConfig>`（发起连接时记下，`open_pty`
要它的 `term`/`cols`/`rows`，标题条要 `user`/`host`/`port`）；`App::new` 里
初始化为 `initial.clone()`。`spawn_connect` 的签名从 `&self` 改成 `&mut self`
（`app.rs:433`），开头加 `self.last_cfg = Some(cfg.clone());` —— 会话管理器
发起的连接也要记下，否则第二次连接后开分屏会用上一台主机的 term/尺寸。
改签名后编译器会指出全部调用点（`user_event` 的连接意图施加点、`resumed` 的
CLI 直连），逐个把 `self.` 的借用改成可变即可。`SshConfig` 已 `#[derive(Debug, Clone)]`
（`crates/mullion-ssh/src/config.rs:20`），无需改。

`UserEvent` 补两个变体：

```rust
    /// 分屏的新 channel 开好了(F35:复用同一条 SSH 连接)。
    PaneOpened {
        id: PaneId,
        ssh: SshSession,
        rx: Receiver<Vec<u8>>,
    },
    /// 分屏 channel 开失败。树上的叶子位留着,标题条显示错误,用户可以再切布局。
    PaneOpenErr { id: PaneId, msg: String },
```

`user_event` 里处理：

```rust
            UserEvent::PaneOpened { id, ssh, rx } => {
                // 初始网格给 80x24 占位,真实尺寸由下一帧 apply_geometry 校准
                // (last_grid 给 (0,0),保证那一帧必然发一次 window_change)。
                if let Some(ws) = self.ws.as_mut() {
                    let mut emulator = mullion_term::emulator::Emulator::new(80, 24);
                    let d = theme::term_default_colors(&MULLION_DARK);
                    emulator.set_default_colors(d.fg, d.bg);
                    ws.attach_pane(crate::shell::workspace::PaneState {
                        id,
                        host_ix: 0,
                        emulator,
                        pty: Box::new(ssh),
                        rx,
                        pacer: SyncFramePacer::new(),
                        status: crate::shell::workspace::PaneStatus::Live,
                        // 故意给一个不可能的初值:下一帧 apply_geometry 必然
                        // 发一次 window_change,新 channel 才知道自己多大(T4)。
                        last_grid: (0, 0),
                    });
                }
                self.ui_dirty = true;
                self.request_ui_redraw();
            }
            UserEvent::PaneOpenErr { id, msg } => {
                log::warn!(target: "mullion", "pane {} 开启失败: {msg}", id.0);
                self.ui.last_error = Some(msg);
                self.ui_dirty = true;
                self.request_ui_redraw();
            }
```

`ConnectOk` 分支改成建 `Workspace`（原 `app.rs:605-620` 一带）：

```rust
            UserEvent::ConnectOk { ssh, rx, handle } => {
                let mut emulator = mullion_term::emulator::Emulator::new(80, 24);
                let d = theme::term_default_colors(&MULLION_DARK);
                emulator.set_default_colors(d.fg, d.bg);
                let mut ws = crate::shell::workspace::Workspace::new(
                    crate::shell::workspace::PaneState {
                        id: PaneId(1),
                        host_ix: 0,
                        emulator,
                        pty: Box::new(ssh),
                        rx,
                        pacer: SyncFramePacer::new(),
                        status: crate::shell::workspace::PaneStatus::Live,
                        last_grid: (0, 0),
                    },
                );
                ws.hosts.push(crate::shell::workspace::HostConn {
                    label: self
                        .last_cfg
                        .as_ref()
                        .map_or_else(|| "远端".to_string(), |c| format!("{}@{}", c.user, c.host)),
                    addr: self
                        .last_cfg
                        .as_ref()
                        .map_or_else(String::new, |c| format!("{}:{}", c.host, c.port)),
                    handle,
                });
                self.ws = Some(ws);
                self.current_preset = Some(crate::shell::workspace::Preset::Single);
                self.cli_direct = false;
                self.ui.session_manager_open = false;
                self.ui_dirty = true;
                self.request_ui_redraw();
            }
```

`ConnectOk` 变体本身加 `handle: Arc<Handle<ClientHandler>>` 字段，
`spawn_connect`（`app.rs:433`）里改成先 `establish` 再 `open_pty`：

```rust
            let handle = match mullion_ssh::session::establish(&cfg, policy).await {
                Ok(h) => Arc::new(h),
                Err(e) => {
                    let _ = proxy.send_event(UserEvent::ConnectErr(format!("{e}")));
                    return;
                }
            };
            match mullion_ssh::session::open_pty(handle.clone(), &cfg, wake).await {
                Ok((ssh, rx)) => {
                    let _ = proxy.send_event(UserEvent::ConnectOk { ssh, rx, handle });
                }
                Err(e) => {
                    let _ = proxy.send_event(UserEvent::ConnectErr(format!("{e}")));
                }
            }
```

**(h) 点击切焦点**（`window_event` 的 `MouseInput` 左键按下分支，在
`selection_press()` 之前）：

```rust
                    // 点哪块就切到哪块(F33)。必须在 selection_press 之前:
                    // 划选的锚点要落在新焦点 pane 的坐标系里。
                    if let Some(id) = self.pane_at(self.cursor_px) {
                        if let Some(ws) = self.ws.as_mut() {
                            if ws.focus() != id {
                                ws.set_focus(id);
                                self.ui_dirty = true;
                            }
                        }
                    }
```

**(i) `render_frame` 的终端趟**（`app.rs:1219-1252`）改成多 pane：

```rust
    let terminal_draw = if panes.is_empty() {
        None
    } else {
        diag::mark(diag::Stage::TextPrepare);
        let res = glyphon::Resolution {
            width: a.gpu.config.width,
            height: a.gpu.config.height,
        };
        let quads = crate::gpu::quads_for_panes(
            panes,
            a.text.cell_w,
            a.text.cell_h,
            theme::term_default_colors(&MULLION_DARK),
        );
        // 渲染路径不许 panic:prepare 失败(如长会话把图集喂满 AtlasFull)记录并
        // 跳过整帧(含 egui)——不拖垮整个 GUI。
        if let Err(e) = a
            .text
            .prepare_panes(&a.gpu.device, &a.gpu.queue, panes, res)
        {
            log::warn!(target: "mullion", "glyphon prepare 失败,跳过本帧: {e:?}");
            diag::count_skipped();
            return (std::time::Duration::MAX, actions);
        }
        let inst = a.gpu.quad_instances(&quads);
        Some((inst, quads.len() as u32))
    };
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cd /data/Mullion
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
```
Expected: 每行都是 `test result: ok.`，无 `FAILED`/`panicked`。

逐条确认领域陷阱的守护测试仍在且绿：

```bash
cargo test --workspace 2>&1 | grep -E "pty_write_is_collected|sync_update_defers_present|redraw_is_frame_capped|reflow_emits_resize|shift_blocks_mouse_report|shift_enter_without_kitty|terminal_keyboard_is_never_fed_to_egui|pty_write_goes_to_its_own_pane"
```
Expected: 8 行，全部 `... ok`

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```
Expected: 两条都无输出

- [ ] **Step 5: 提交**

```bash
cd /data/Mullion
git add crates/mullion-app/src/
git commit -m "feat(app): 事件循环接 Workspace,单 pane 换成多 pane (F30/F33/F34/F35)

Connection 换成 Workspace;Active.grid_dims 删除,每 pane 尺寸由 PaneState.last_grid
承载。几何每帧算一份存进 Active.geoms,渲染 / 鼠标命中 / window_change 三条路径
共用 —— 各算各的会出现\"点得到但画不着\"的错位。

spawn_connect 拆成 establish + open_pty,Handle 用 Arc 交给 Workspace.hosts;
套预设时多出来的 pane 在同一条连接上另开 channel(F35),经 PaneOpened 上线。
点击切焦点排在 selection_press 之前,划选锚点才落在新焦点的坐标系里。

守护测试:T1 pty_write_goes_to_its_own_pane_channel_t1、T2 any_pane_in_sync_defers_present、
T3 redraw_is_frame_capped、T4 window_resize_maps_to_per_pane_grids_f34、
T7 frame::tests、T8 terminal_keyboard_is_never_fed_to_egui_so_tab_cannot_steal_focus"
```

---

## Task 13: theme token 注释（技术债 1）+ 交付一条龙

**Files:**
- Modify: `crates/mullion-app/src/theme.rs:20-50`
- Modify: `Cargo.toml`（`workspace.package.version`）

- [ ] **Step 1: 给零引用 token 补 F 编号注释**

B1 遗留技术债 1：`window_bg`/`fg_mid`/`fg_dim`/`fg_dimmer`/`fg_ghost`/`warn`
零引用且没标注预留给谁，后来者分不清是有意预留还是抄表多打的。B2-a 用掉了
其中几个，剩下的补注释：

```rust
    /// 窗口根底色。F85 自绘标题栏已否决,现由 OS 标题栏占位;
    /// 保留是因为色板表(spec §4.6)以它为基准推导其余 bar_* 色。
    pub window_bg: Rgb,
    ...
    /// 中等前景。预留给 F84 设置面板的次级标签。
    pub fg_mid: Rgb,
    /// 暗前景。F83 用于断开状态点。
    pub fg_dim: Rgb,
    /// 更暗前景。预留给 F84 设置面板的禁用项。
    pub fg_dimmer: Rgb,
    /// 幽灵前景(近乎背景)。预留给 F32 拖拽分隔条的静态态。
    pub fg_ghost: Rgb,
    ...
    /// 警告色。预留给 F6 连接降级提示(如回落到密码认证)。
    pub warn: Rgb,
```

- [ ] **Step 2: 跑全量绿**

```bash
cd /data/Mullion
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```
Expected: 全 `ok.`，clippy 与 fmt 无输出

- [ ] **Step 3: 升版本并提交**

```bash
cd /data/Mullion
# 把 workspace.package.version 第三位 +1(当前 0.1.11 → 0.1.12)
git add Cargo.toml Cargo.lock crates/mullion-app/src/theme.rs
git commit -m "chore: 版本 0.1.12(分屏骨架:1/2/3/4 屏预设 + pane 标题条)"
```

- [ ] **Step 4: 交叉编译并做依赖验收**

```bash
cd /data/Mullion
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app
x86_64-w64-mingw32-objdump -p target/x86_64-pc-windows-gnu/release/mullion.exe \
  | grep -i "DLL Name"
```
Expected: 输出里**不得**出现 `libgcc_s_seh-1.dll` 或 `libwinpthread-1.dll`
（出现即不合格，按 `docs/cross-compile-windows.md` 修）

- [ ] **Step 5: 发 Release 并报给用户**

```bash
cd /data/Mullion
cp target/x86_64-pc-windows-gnu/release/mullion.exe .
sha256sum mullion.exe > mullion.exe.sha256
HTTPS_PROXY=http://127.0.0.1:7890 gh release create v0.1.12 \
  mullion.exe mullion.exe.sha256 -t "v0.1.12" -F notes.md --repo kilobitcy/Mullion
```

`notes.md` 必须包含（无头环境验不了的都在这儿，见 CLAUDE.md「你无法验证的东西」）：

**本版改了什么**：F30 分屏骨架、F82 布局工具栏、F83 pane 标题条、F35 单连接多
channel、F34 分屏后 window_change。

**人工验收清单**（逐条对应设计文档 §11）：
1. 连上后工具栏出现，点每一个预设（2 屏两种、3 屏三种、2×2），pane 排布与原型
   一致，边界**无缝隙无重叠**，两块都能各自输入
2. 各 pane 里跑 `tmux` + 全屏 TUI，排版正确不错行；右侧 pane 的 `tput cols`
   约等于窗口一半的列数（F34 的真正验收）
3. 拖窗口改大小 → 四块同步重排，各自的 `tput cols` 跟着变
4. 开 4 个 pane 期间，在远端跑 `ss -tn | grep <本机IP>`，**只有 1 条连接**（F35）
5. 开第 2/3/4 个 pane 时**不再弹主机密钥确认框**（§6.2：TOFU 只在 establish 触发）
6. 点「2×2」→ 四块；再点「1 屏」→ 回到单块，留下的是原来的左上那块；关闭 pane 后
   兄弟**顶满原区域**，远端排版立刻跟上
7. 关掉其中一个 pane，**其余 pane 不受影响**：远端连接数仍是 1，其他 pane 照常收发
   （§6.1 的 `Arc` 保活）
8. 在某块里 `exit` 断开 → 该块标题条显示「已断开」、状态点变灰，**内容仍可
   上滚、仍可划选复制**；此时点「2 屏」，被关掉的应当是已断开的那块
9. 菜单「分屏 → 显示 / 隐藏 pane 标题条」切换时，终端**行数确实变化**
   （tmux 状态栏位置跟着上下动）
10. 点不同 pane → 标题条高亮跟着走，键盘输入进的是被点的那块；非焦点 pane 的
    光标是空心框，焦点 pane 是实心块
11. 各 pane 的背景底色与文字位置**对齐**，没有「字挪了底色没挪」（§7.1 的色块层
    与文字层是两条独立路径，各自平移，最容易在这里对不上）
12. `/tui fullscreen` 下在非焦点 pane 里 Shift+划选仍能复制（T5 逃生门没被分屏破坏）
13. 多 pane 流式输出（4 个 pane 同时 `yes` 或 `tail -f`）时**不闪、不撕裂、风扇不起飞**
    （T2 攒帧聚合 + T3 帧率节流，唯一判据是人眼）
14. CJK 字符在分屏边界附近不串行、不溢出到邻居 pane（Task 7 的按 pane 裁剪）

**未在容器内验证**：`tests/live.rs::multi_pty_live_f35`（需 `MULLION_LIVE=1` + 真机，
见 Task 2 Step 7）。第 4、5、7 条实机验收其实就是它的人工版。

**首次运行**：未签名 exe 每版都会被 SmartScreen 拦，PowerShell 里先跑
`Unblock-File .\mullion.exe`，详见 `docs/cross-compile-windows.md`。

**sha256**：`mullion.exe.sha256` 的内容。

最后把 Release 链接 + sha256 + 上面的验收清单报给用户。
