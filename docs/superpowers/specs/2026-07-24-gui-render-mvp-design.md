# GUI 渲染 MVP —— winit + wgpu + glyphon 单 pane 跑通远端 tmux

- 日期: 2026-07-24
- 状态: 已批准(brainstorm)
- 关联: spec.md v0.1 里程碑(单 pane 跑真实 Claude Code 全屏 TUI,目视不闪);
  F10/F11/F16(渲染)、F13/F14/F15(输入)、F34(window_change)、F6(连接错误);
  陷阱 T1/T2/T3/T4/T5/T6;ADR-004(async 边界,方案 B)
- 依赖: 本切片建在 `feat/f1-ssh-connect-pty`(SSH 库切片)之上

## 1. 目标与非目标

**目标**:第一个能真正双击运行、连真实 SSH、在 GPU 窗口里显示并操作远端 tmux 的版本。
单 pane 铺满窗口、等宽字体、基础前景/背景色 + 块状光标、键盘输入、resize 时发
`window_change`(T4)。这是 spec.md 的 v0.1 里程碑目标。

**非目标(各自后续 spec)**:分屏 UI(F4)、鼠标上报(F15 的上报侧;逃生门逻辑已在 keymap)、
回溯/scrollback(F17)、连字、粗体/斜体/下划线等富属性、`Term::damage()` 差分渲染(F12,
MVP 先整屏重画)、密码认证的交互式输入、断线重连(S3)、弹窗式 TOFU、多字重字体、
连接中/错误的窗口内提示。

## 2. 关键取舍:term 如何把网格交给渲染层(快照法)

`Emulator` 现仅有 `feed()` / `take_pty_writes()`,无读网格出口,也无 `resize()`。
渲染需读每格的字符 + 颜色 + 光标。选**快照法**:

`Emulator::snapshot() -> GridSnapshot` 返回纯数据(行 × 单元格 + 光标),颜色在 term 内解析成具体 RGB。

**默认调色板归属(核 alacritty 0.26 源码后确认)**:`Term::colors()` 返回的
`Colors([Option<Rgb>; 269])` 默认**全为 `None`**,只装 OSC-4 运行时覆盖,**不含任何默认 RGB**。
因此「红 / index 1 / 默认前景」具体是什么 RGB,由**我们自己拥有的默认调色板**(16 ANSI +
256 立方 + 默认 fg/bg 的 const 表)决定,再叠加 `term.colors()` 的覆盖。
`Cell.fg/bg` 是 `Color::{Named(n) | Indexed(u8) | Spec(Rgb)}`:`Spec` 直取;`Named`/`Indexed`
查我们的表(有覆盖用覆盖)。这层解析是纯函数,是干净的可测面(`SGR 31 → 我们表里的红`)。

- 备选「访问者 `for_each_cell`」:省一次分配,但难测;80×24≈2000 格/帧分配无关紧要,YAGNI。否。
- 备选「暴露 `&Term`」:把 alacritty 类型漏进 app,破坏封装且耦合 app 到 alacritty API。否。

选快照法的决定性理由:它把 **CJK 宽字符(F16)** 与 **颜色解析** 这两个只有人眼能最终验的东西,
各劈出一个纯数据的可测切面,守住项目「布局/键码 bug 要能无窗口复现」的核心价值。

`GridSnapshot` 是自包含纯数据,留在 term(VT 域),不依赖 core/ssh/app —— 不违反架构不变量。

## 3. 数据流(每帧)与 async 边界(沿用 ADR-004 方案 B)

```
tokio 运行时(app 拥有,启动时建;io_task 跑在其 worker 线程)
  io_task ──inbound bytes──> mpsc::Receiver ──┐   wake() = proxy.send_event(Wake)
                                              │            │
winit 主线程(同步):                          ▼            ▼
  user_event(Wake) → window.request_redraw()
  RedrawRequested:
    1. 排空 rx → emu.feed(bytes)            ← 永远做(便宜;保 T1 的 PtyWrite 应答流动)
    2. emu.take_pty_writes() → ssh.write()  ← 永远做(T1 红线)
    3. 若 FrameLimiter(T3) 且 SyncFramePacer(T2) 都放行 → GPU present 一帧
       否则 ControlFlow::WaitUntil(下一帧时刻)延迟提交(N3 空闲不空转)
  键盘事件 → keymap::encode_key → ssh.write()(非阻塞 try_send)
  Resized → 由像素算新 cols/rows;变了则 emu.resize + ssh.resize(window_change) ← T4
```

- `feed`(便宜)与 `present`(贵)解耦 = **T3**;`present` 被同步块攒住 = **T2**;
  二者都不挡 PtyWrite 回写 = **T1**。
- 唤醒经注入的 `wake: Arc<dyn Fn()+Send+Sync>`(app 传 `EventLoopProxy::send_event`),
  ssh 不认识 winit —— 守 ADR-004。
- App 持有 `tokio::runtime::Runtime` 保活(drop 即关运行时);winit 线程启动后不 block_on,
  仅持 `SshSession`(cmd_tx)做非阻塞 `try_send`。

## 4. 模块划分

**term 侧(新增纯数据,不违反不变量)**
- `snapshot.rs`(新):`GridSnapshot`、`SnapCell { ch, fg: Rgb, bg: Rgb, flags, width }`、
  `Cursor { row, col, visible, shape }`、`Rgb` + xterm-256 调色板解析(Named/Indexed/Spec → RGB)。
- `emulator.rs`:加 `snapshot() -> GridSnapshot`(遍历 `term.grid()`,解析颜色、标 width/spacer)
  与 `resize(cols, rows)`(重建/调整 Term 维度)。

**app 侧**
- `cli.rs`(新):解析 `user@host [-p N] [-i keypath]`(自己写一小段,不引 clap),
  产出 `SshConfig`(auth 先支持 PublicKey/-i 与 Agent)。
- `gpu.rs`(新):wgpu instance/surface/adapter/device/queue + surface 配置/resize +
  **背景/光标色块**的 colored-quad 管线。**不可测的 GPU 胶水,尽量薄。**
- `text.rs`(新):glyphon 初始化(FontSystem/Cache/Atlas/Viewport/TextRenderer)+
  `snapshot → glyphon TextArea/带色 span` 的映射(纯映射可测)。
- `grid.rs`(新):`grid_size_for(px_w, px_h, cell_w, cell_h) -> (cols, rows)` 纯函数。
- `app.rs`:`ApplicationHandler<UserEvent>` 实体,持窗口 / gpu / text / pane /
  `SshSession` / `Receiver` / `Runtime` / `SyncFramePacer` / `FrameLimiter`。
- 复用现有:`frame.rs`(T3)、`render.rs::SyncFramePacer`(T2)、`reflow.rs`(T4 的
  `ResizeSink`,MVP 单 pane 也走它)、`session_pump.rs`、`pane.rs`。

## 5. 渲染管线(MVP,两趟)

1. **背景趟**:对 `bg ≠ 默认` 的格 + 光标格,用 wgpu colored-quad 管线画实心块
   (tmux 状态栏 / Claude Code TUI 重度依赖背景色,故 MVP 必须有)。
2. **前景趟**:glyphon 画文字,每行一个 `TextArea`,行内按 span 给 fg 色。
3. **CJK(F16)**:宽字符占 2 格、跳过 spacer 格(快照已标 `width`/flags)。
4. **光标**:MVP 用块状(背景趟里画反色块)。
5. 字体走 cosmic-text `FontSystem` 加载系统字体,请求等宽族;`cell_w/cell_h` 由字体度量取一次。
   前景趟用 `Buffer::set_rich_text`(每格一个 span 给 fg 色)。

**已知风险(spec.md Q1 已拍板取舍):** v0.1 用 glyphon 通用文本路径,让 cosmic-text 逐行 shape。
纯等宽 ASCII 下 advance == `cell_w` 能对齐;但 **CJK 宽字符 / 字体回退时,字形 x 不保证正好落在
`col*cell_w`**,会与我们自画的背景色块错位。这是**人工目视验项**(F16),不写自动断言。
若实测对齐不可接受,退路是改用专用 wgpu 逐格字形管线(spec.md Q1 已预留;届时另开决策,MVP 不做)。

glyphon 0.7 / wgpu 23 / winit 0.30 的确切签名已按锁定版本核实(见本文件复核记录);细节在 impl 时再对。

## 6. 连接 UX(MVP)

`main()` 顺序(注意 `connect` 需要 `wake=proxy.send_event`,proxy 来自 EventLoop,故必须先建循环):
解析 CLI → 建 `EventLoop::<UserEvent>::with_user_event()` → `proxy = create_proxy()` →
`runtime.block_on(connect(cfg, TofuAccept, wake=proxy))`,用默认 80×24 起 PTY →
失败把 `ConnectError`(F6 已有的可操作消息)打到 stderr 并非零退出;成功则把 `(SshSession, rx)`
交给 `App` → `event_loop.run_app(app)`。窗口在 `resumed()` 里建,建好后按真实尺寸立即
`window_change`(顺带即时练 T4)。
TOFU 主机密钥 MVP 用 `TofuAccept`(自动记、存内存);弹窗版后延。

## 7. 测试策略(守住「无窗口可复现」)

**可自动验(写断言)**
- term:`snapshot` 颜色解析(如 `SGR 31` → 红 RGB)、CJK `width == 2`、`resize` 后维度变化。
- app 纯函数:`grid_size_for` 边界、`snapshot → span 映射`(某格 fg/bg/字符正确)、
  复用已有 T2/T3/T4/键码(T5/T6)测试。
- CLI 解析:`user@192.0.2.10 -p 22 -i /tmp/k` → 预期 `SshConfig`。

**不可自动验、需人工确认(写进 PR 清单,不编造通过结论)**
- 字形位置/颜色是否正确、是否真的不闪(G1)、CJK 是否真占两格对齐、光标形状、
  输入法候选框、真实高延迟手感。
- GPU 胶水(`gpu.rs`)与 glyphon 调用不写断言测试,只保证编译 + 能跑起来。

## 8. 落地顺序(逐步降风险)

1. **term 快照 + resize**:纯数据 + 单测,先不碰 GPU。
2. **GPU 通路**:`gpu.rs` + `text.rs` 对一个**静态快照**画出网格(背景趟 + 前景趟),
   人工目视确认能出字。此步不连 SSH。
3. **接 SSH**:`cli.rs` + `main()` 连接 + 事件循环 pump(rx→feed→take_pty_writes→write),
   窗口里显示真实远端输出。
4. **接输入 + resize**:键盘 → keymap → write;Resized → `grid_size_for` → emu.resize +
   window_change(T4)。
5. 全绿定义:`cargo test --workspace` 全过 + `clippy -D warnings` 干净 + `fmt --check` 干净;
   人工验证清单随 PR。

每步凡 GPU/人眼类一律标「未验证,需人工确认」。
