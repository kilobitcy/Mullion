# GUI / 渲染层的坑（动 app.rs / text.rs / gpu.rs / input.rs / keymap.rs 前必读）

> 这些是本项目实测 + 代码复核里踩出来的、**光看编译过/单测绿发现不了**的坑。
> 多数属「编译过、跑起来才崩/表现错」，且大都在无头容器里验不了——所以写在这里。
> 每条给「症状 / 规则 / 守护」。锁定版本：winit 0.30.13 / wgpu 23 / glyphon 0.7 /
> cosmic-text 0.12 / alacritty_terminal 0.26。

## 事件循环 / 帧率

- **`ControlFlow::WaitUntil` 不自复位 → 忙转（严重，T3/N3 红线）。**
  设了 `WaitUntil(deadline)` 后不复位，deadline 一过 winit 每轮 `ResumeTimeReached`
  零延迟返回 → 100% CPU 空转（比「每秒几千次重绘」更隐蔽，重绘都不发生）。
  **规则**：每次 `RedrawRequested` 三条出路都显式 `set_control_flow`
  （present/idle→`Wait`；节流→`WaitUntil` 且记 `next_frame_at`）；被节流的帧靠
  `about_to_wait` 在 deadline 到点补**一次** `request_redraw`，**不要**在原地 `request_redraw`。
  决策抽成纯函数 `FrameLimiter::plan`（守护：`frame::tests` 的 4 条 plan 测试）。

## glyphon / 文字

- **`TextAtlas::trim()` 必须每帧调、且要「可达」，否则长会话卡死。**
  glyphon 的 `glyphs_in_use` 只有 `trim()` 会清；不调则 LRU 永远淘汰不掉，图集迟早满 →
  `prepare()` 返回 `PrepareError::AtlasFull`。**坑中坑**：若 `trim()` 放在 `render_frame`
  末尾，而 `prepare` 失败会提前 `return`——于是它本该自愈的那条路径反而到不了，一次
  AtlasFull 后**永久卡死冻屏**。**规则**：`trim()` 放 `render_frame` **最开头**（任何
  early-return 之前);渲染路径的 `prepare/render` 失败**记录并跳过本帧,不 panic**。
  （GPU 胶水,无单测,只能读码推断。)
- **glyphon 逐行 shape ≠ 我们的等宽网格。** 我们按 `col*cell_w` 自画背景块,文字却由
  cosmic-text 按字形 advance 排——纯等宽 ASCII 能对齐,CJK/字体回退时字形 x 未必落在格上,
  可能与背景块错位。这是 [adr-001](adr-001-glyph-rendering.md) 已拍板的 v0.1 取舍(通用文本路径),
  人眼验项;不可接受时退路是专用 wgpu 逐格字形管线。

## wgpu

- **`SurfaceConfiguration` 与 uniform 的尺寸都要 `max(1)`。** 窗口最小化时 Windows 送
  `Resized(0,0)`;config 钳到 1×1 但若 resolution uniform 写 `(0,0)`,着色器 `px/resolution`
  出 NaN,该帧几何全坏。`new()` 和 `resize()` **两处**都要用钳制后的值。
- **`get_current_texture()` 分 `SurfaceError` 四变体处理**:`Timeout` 跳过本帧(别 reconfigure)、
  `Lost`/`Outdated` reconfigure、`OutOfMemory` 记录。别 `Err(_)` 一把吞(黑屏无日志难查)。

## 输入 / 键盘

- **空格及常用控制键是 `NamedKey`,不是 `Character`。** winit 里空格 = `Key::Named(NamedKey::Space)`,
  不是 `Character(" ")`;Tab/Backspace/Escape/Delete/方向键同理。早期 `translate_logical` 只认
  `Named(Enter)` + `Character`,其余 `Named` 落 `_ => None` 被丢 → 「很多键没反应」。
  **规则**:新键要在 `keymap::Key`(+`encode_key` 字节)和 `input::translate_logical`(NamedKey 映射)
  **两处**都加。守护:`keymap::tests`(space/控制键/方向键)+ `input::tests`(NamedKey 映射)。
- **`KeyEvent::platform_specific` 是 `pub(crate)`** → 外部 crate 测试里无法用字面量构造 `KeyEvent`。
  所以可测逻辑抽到 `translate_logical(logical, mods)`(收 `logical_key` 而非整个 event),
  `translate_key` 只转调;测试测 `translate_logical`。
- **方向键 DECCKM 未处理**:现发普通光标键 `ESC[A/B/C/D`;应用光标键模式(DECCKM,部分全屏 TUI 开)
  下应发 `ESC O A`。当前不追踪该模式 → 某些 TUI 里方向键可能不对。补法:app 从仿真器取模式传给编码。

## alacritty_terminal / 快照

- **`Term::colors()` 默认全 `None`。** 它只装 OSC-4 运行时覆盖,**不含任何默认 RGB**。
  「红/index 1/默认前景」具体 RGB 归我们所有(`palette.rs` 的 const 表);解析时覆盖优先、否则查表。
  守护:`palette::tests`。
- **`WIDE_CHAR_SPACER` ≠ `LEADING_WIDE_CHAR_SPACER`。** 前者是行内宽字右半(该跳过渲染);
  后者是宽字在行尾放不下、整体换行时行尾插的**独立空白占位格**(左邻不是宽字,当普通空格,背景要照画)。
  `SnapCell.spacer` **只标 `WIDE_CHAR_SPACER`**;把 LEADING 也标会让下游漏画那一列背景。
  守护:`emulator::tests::snapshot_cjk_line_wrap_leading_spacer_is_not_spacer`。

## SSH（平台差异）

- **`AgentClient::connect_env()` 仅 Unix。** Windows 的 ssh-agent 走命名管道,无此函数,
  原样写会让 Windows 目标**编不过**。已按 `#[cfg(unix)]` 门控,Windows 上 agent 路径返回可操作
  F6 错误(用 `-i`)。改 agent 路径时注意别破坏这个门。

## 字体

- **字号按 DPI 缩放**:`window.inner_size()` 是物理像素,字号也须物理像素:
  `px = pt * 96 * scale_factor / 72`。否则 Windows 高 DPI(125%/150%)下过小。
  当前只在建窗口时取一次 `scale_factor`,**未跟随 `ScaleFactorChanged`**——跨不同 DPI 显示器不更新(F21 待做)。
- **`Family::Name("Google Sans Code")` 须系统已装**,否则 cosmic-text 回退默认字体(不崩,对齐可能差)。
  字体族/字号当前硬编码,可配置见 spec **F21**。
