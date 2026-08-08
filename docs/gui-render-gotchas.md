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
- **`dirty` 有两个独立脏源,漏一个就丢交互。** 终端态若只拿 `pacer.should_present()`
  (=远端来了新字节)当 dirty,远端一安静,egui 自己的重绘需求(菜单展开、hover、弹窗、
  错误提示)就全被 `RedrawAction::Idle` 吞掉——点菜单没反应;而 launcher 态因为硬编码
  `dirty = true` 反倒正常,这个不一致极易被误判成 egui 的问题。**规则**:`frame_is_dirty`
  取「终端字节」与「UI 待画」的并集;标脏与 `request_redraw` 必须成对(只请求不标脏那帧
  照样被判 Idle,故统一走 `App::request_ui_redraw`);present 后清脏,`repaint_delay < MAX`
  时重新标脏。**守护**:`frame::tests::egui_repaint_alone_is_dirty_enough`。
- **攒帧(DEC 2026)必须跨 feed 边界匹配,还得有超时(T2)。** 一次 feed = 一个 SSH
  `ChannelMsg::Data`,TCP 可以把 `\x1b[?2026l` 切成 `\x1b[?2026` + `l`。只在单段内
  `starts_with` 的话这个 ESU 就检测不到,`in_sync` 永远为真,`should_present()` 恒 false,
  **画面永久冻结**——字节其实一直在正常收发,用户看到的却是「键盘没有任何反应」。
  **规则**:段尾若是 BSU/ESU 的真前缀就留到下段拼接后再扫;再加一道 ~150ms 超时,对端
  发了 BSU 却再不发 ESU(TUI 被 kill / 链路截断)时强行出帧,宁可闪一下也不停在死画面。
  **守护**:`render::tests::esu_split_across_feeds_is_still_detected`
  / `unterminated_sync_block_times_out`。

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
- **挑了 sRGB surface 格式(`.find(|f| f.is_srgb())`),自己的着色器和 `LoadOp::Clear` 就必须手动转线性,否则外壳与终端两套颜色。**
  选中 sRGB 格式后,硬件会把着色器输出色值、`Clear` 的值都当**线性值**再编码成 sRGB——写进去的
  必须是线性值,不是设计稿上那个 sRGB 十六进制。`egui`(`linear_from_gamma_rgb`)和
  `glyphon`(`srgb_to_linear`)各自都做了这层转换,本项目自绘的 `QUAD_WGSL` 原先没做,原样透传。
  底色纯黑时两个色彩空间都是 0,看不出差别;底色一旦非黑,同一个 token 在 egui 外壳和终端里
  就渲染成两个颜色——这正是 F80「外壳与终端是一个世界」验收失败的根因。**规则**:`QUAD_WGSL`
  顶点色、`theme::clear_color` 都要过 `srgb_to_linear`;**新开一条 quad/渲染路径(如 F82 工具栏、
  F83 pane 标题条)时照抄这条**——现有守护测试只查 `QUAD_WGSL` 字符串里还有转换调用,挡不住
  新路径漏转换。数值正确性本身无法在无头环境验证,最终靠人眼截图取色核对。
  守护:`gpu::tests::quad_shader_converts_srgb_to_linear`、`theme::tests::clear_color_is_linear_not_raw_srgb`
  / `theme::tests::srgb_to_linear_endpoints_and_cutoff`。
- **最小化必须整帧跳过,不只是把尺寸 `max(1)`。** 上一条只挡住了 NaN,挡不住真正的伤害:
  `Resized(0,0)` 若继续往下传,`grid_size_for(0,0)` 钳成 1×1 → ① `emulator.resize(1,1)` 让
  alacritty 按 1 列 reflow 带 10000 行 scrollback 的 primary grid(tmux 在 alt screen 时
  `Term::resize` 仍对 primary 传 `reflow=true`),末尾 `truncate(max_scroll_limit + lines)`
  把历史**永久碾平,还原也回不来**;② `ssh.resize(1,1)` 把 `window_change 1×1` 发给远端,
  tmux / Claude Code TUI 按 1 列重排版;③ 还在对 0 面积表面 configure/acquire/present。
  **规则**:任一维为 0 → 不 configure、不重排网格、不发 `window_change`、不渲染;
  **但 IO 泵必须照跑**(排空 rx → feed emulator → 回写 `PtyWrite`,T1),否则有界 rx(256)
  灌满堵住 io_task、远端同步输出探测/光标查询永久无应答。最小化期间还未必收得到
  `RedrawRequested`,所以泵要挂在 `UserEvent::Wake` 上,不能只挂重绘。
  **守护**:`shell::window_state::tests`(`minimized_resize_touches_neither_gpu_nor_remote`
  / `minimized_still_pumps_io`)。
- **但 Minimized 不能是单向门。** 进去只需一次 `Resized(0,0)`,出来却完全指望对方补发
  非零 `Resized`。0×0 不总是「用户最小化了」——驱动/合成器抖动、显卡崩溃重启外壳
  (v0.1.4 真机日志最后一行正是 `Resized(0x0)`)也会送这一条,那些情况下**不会**有还原
  事件,窗口就永久停在只泵 IO 不绘制:字节照收照发,屏幕再不更新,用户看到的是
  「键盘没有任何反应」。**规则**:凡是「窗口本该看得见」的信号(`Focused(true)` /
  `Occluded(false)` / `RedrawRequested`)都拿 `window.inner_size()` 复查一次,非零就自愈,
  且要走与还原同一条路径(补 surface configure + grid 传播),不能只翻状态位。
  **守护**:`window_state::tests::minimized_recovers_when_real_size_is_nonzero`。
- **拿到 adapter 就记 `get_info()`,建完 device 就挂 `on_uncaptured_error` /
  `set_device_lost_callback`。** GPU 子系统出事(TDR、驱动重置)时,Windows 事件日志只会
  记下 Explorer/DWM 崩在 `amdxx64.dll` 之类,**不记录我们自己**;不自报就只能靠猜。见
  [adr-008](adr-008-diagnostics.md)。

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
- **`f32::fract()` 在负数区间与 `floor()` 不是同一套取整,拿它算「格内位置」会抖(F18)。**
  `fract()` = `x - x.trunc()`(**向零**截断),而列号换算 `cell_at` 用的是 `floor()`。拖拽划选时
  指针会移出窗口,winit 给负坐标:用 `fract()` 判半格得到的是
  `-1.0→Left  -3.0→Left  -4.0→Right  -5.0→Right  -9.0→Left  -13.0→Right` —— **非单调**,
  而列号已被夹在首格不动,于是选区边界在窗外来回抖。**规则**:格内分数一律
  `let cell = q.floor(); q - cell`,并与 `cell_at` **同源地**把越界坐标夹到首/末格
  (窗左外恒 Left、窗右外恒 Right),而不是让半格标志继续随指针翻转。
  守护:`input::tests::cell_side_clamps_out_of_bounds_pointer_like_cell_at`。
- **winit 失焦不补发 `MouseInput{Released}` → 按住左键 Alt-Tab 会让拖拽状态永久卡住(F18)。**
  Windows 的 `WM_CAPTURECHANGED` 不合成一次释放事件,别的窗口抢走捕获后我们只会收到
  `Focused(false)`。`dragging` 卡住 = 边界自动滚动停不下来(表现为「列表自己一直滚」)。
  **规则**:`Focused(false)` 里显式收尾(`dragging=false`、`autoscroll=0`);同时松手路径
  `selection_release` 必须先看「本地是否真的配对过一次按下」再决定动不动剪贴板 —— 指针事件按
  T8 是「先喂 egui 再判」,**按下与释放各自独立判路由**,在菜单上按下、拖到终端里松开就会走到
  释放分支而没有本地锚点;此时若无条件复制,会把仿真器里的**残留旧选区**静默写进剪贴板,
  用户毫无察觉、原有内容就没了。

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

## egui 外壳(切片 A2b,详见 [adr-007](adr-007-egui-chrome.md))

- **egui `Renderer::render` 要 `RenderPass<'static>`(wgpu 23)。** 用 `pass.forget_lifetime()`
  转 'static;它**消费** pass 自身,所以终端两趟(背景 quads / glyphon 文字)必须录在
  `forget_lifetime()` **之前**,三者画进**同一个** render pass(不是两个 pass)。顺序错 →
  借用检查器过不了或终端被 egui 覆盖。
- **egui 默认字体不含 CJK → 中文全是 tofu 方框(编译过、跑起来才见)。** egui 只内嵌拉丁
  字体(Ubuntu/Hack),菜单「对话/分屏…」、状态栏中文会渲染成缺字方框。**规则**:启动时
  `ctx.set_fonts` 挂系统 CJK 字体作末位回退(`ui::install_cjk_font`:按序试
  `msyh.ttc`/`simhei.ttf`/…,`FontData::from_owned` 用 face index 0)。找不到就静默用默认
  (不崩)。终端层不受影响——glyphon/cosmic-text 会自动回退系统字体。
- **GUI 程序默认 console 子系统 → 双击/启动附带黑控制台窗口。** Rust 默认 console 子系统,
  一个 GUI app 会多弹一个黑框。**规则**:`main.rs` 顶 `#![cfg_attr(windows, windows_subsystem
  = "windows")]`;但为保住 CLI 直连诊断,`main()` 开头 `AttachConsole(ATTACH_PARENT_PROCESS)`
  (windows-sys,cfg(windows)),从终端启动时附着父控制台、双击时静默失败不弹框。
  `objdump -p` 应显示 `Subsystem = Windows GUI`。
- **输入分流别把整段 KeyboardInput 无条件喂 egui(T5/T6 红线)。** egui 只在
  `wants_keyboard_input`/`wants_pointer_input` 或有模态弹窗时截获,否则落既有终端 keymap。
  `modal` 判定要含**所有**开着的弹窗(session_manager / about / **editor**)——漏一个,
  那个弹窗开着时点击会漏到终端。
- **键盘要「先判后喂」,指针才是「先喂后判」——顺序反了 Tab 会废掉整个键盘(T8)。**
  egui 的焦点系统在 `Memory::begin_pass` 里扫**原始事件**,看到 Tab 且当前无焦点,就把焦点
  给第一个可聚焦控件——我们的菜单栏「对话」按钮
  (egui 0.30 `memory/mod.rs`:"nothing has focus and the user pressed tab")。此后
  `wants_keyboard_input()` 恒 true,`route` 把**每一个**按键都判给 egui,终端永久收不到键。
  真机症状:`cd /tm` 按 Tab 补全成功(那个 Tab 已经发给远端了),之后回车/退格全无反应,
  但鼠标点菜单还灵。所以键盘必须先过 `shell::input_route::egui_should_see`,判给终端就
  **整段跳过 `on_window_event`**;指针相反,不喂 `CursorMoved` 就没有 hover 与
  `wants_pointer_input()` 可言,菜单/弹窗全点不动。
  逃生门:egui 收到 Escape 会清焦点,所以卡住时按一下 Esc 能自愈(可用于现场确诊)。
  守护:`shell::input_route::tests::terminal_keyboard_is_never_fed_to_egui_so_tab_cannot_steal_focus`。
- **egui 闭包里借不到 `&mut store`。** UI 构建在 `egui_ctx.run(|ctx| ...)` 里只有 `&mut UiState`。
  改 store/发起连接一律写 intent 到 UiState,由 app.rs 在 `render_frame` 返回后统一施加
  (delete/save 用 `&mut store`,connect 用 `&self` 调 spawn_connect,顺序执行不冲突)。
- **`cli_direct` 的 exit(1) 只该对「初始 CLI 直连首次失败」生效。** 它是启动静态标志,若不
  复位,断线后从会话管理器连别的会话失败会命中 `cli_direct && conn.is_none()` 把整个 GUI
  `exit(1)`(GUI 子系统下无控制台,表现为「程序凭空消失」)。**规则**:ConnectOk 与用户主动
  connect_request 两处清 `cli_direct=false`。
- **原生文件对话框不能在 egui 闭包里同步开(表现为「卡死」)。** `rfd::FileDialog::pick_file()`
  是阻塞调用,而 egui 闭包跑在 `RedrawRequested` 中间 —— 一阻塞就是整个 winit 事件循环停摆:
  IO 泵不动(T1)、窗口不重绘、看门狗只能报「卡在 window_event」。**规则**:UI 只记意图
  (`pick_key_request`),app.rs 在借用释放后另起线程跑,结果经 `EventLoopProxy` 以
  `UserEvent` 回送;取消也要回送(否则 busy 标志永远清不掉,按钮再点就没反应)。
  **还必须 `set_parent(&window)`** —— 无 owner 的对话框可能被排到主窗口后面,而主窗此时
  已被 owner 关系禁用,用户看到的同样是「点不动的卡死窗口」。`rfd::FileDialog` 自身
  `unsafe impl Send`(只存 raw handle),跨线程带 owner 正是 rfd `AsyncFileDialog` 的内部做法。
- **给终端算行列数时扣了 chrome 高度,画的时候也得平移——只做一半 = 首行被菜单栏吃掉。**
  行数走 `shell::viewport::grid_dims(central_px)`(已扣菜单栏+状态栏),但 `gpu::quads_for` /
  `text::prepare` 的绘制原点若仍是窗口 `(0,0)`,第 0 行就画在窗口最顶端;egui 在**同一个**
  render pass 里后画,直接盖上去。真机症状:登录横幅第一行看不见,同时**底部多出一段等高的
  空白**(行数少了、起点没动,让出来的空间跑到下面去了)。**规则**:`build_ui` 在两个
  `TopBottomPanel` 都 `show()` 完之后同时记 `available_rect()` 的**尺寸和原点**
  (`central_px` / `central_origin_px`),绘制两趟都平移到原点,glyphon 的 `TextBounds` 上边界
  也收到原点(挡住字形上伸部溢进菜单栏)。**鼠标换算必须同源减去这个原点**,否则点到的格子与
  看到的差一个菜单栏高度——平移只在 `App::cursor_in_grid` 一处做。
  注意 `central_origin_px` 在渲染点是**同帧**新鲜的(`build_ui` 在终端绘制之前跑),
  而 `central_px` 的消费点在 present 之后,滞后一帧;两者别混着推理。
  守护:`gpu::tests::origin_shifts_every_quad_so_first_row_clears_the_menu_bar`。
- **egui repaint 不得绕过帧率闸(T3/T7)。** 取 `viewport_output[ROOT].repaint_delay` 并进
  既有 `next_frame_at`/`WaitUntil` 排期,受 `FrameLimiter` 上限;别在渲染完原地 request_redraw。
- **`EventLoopProxy` 只有 `Send`,没有 `Sync`。** 把 `EventLoopProxy<UserEvent>` 直接放进要做成
  `Arc<dyn Trait>` 的结构体,编译报 `EventLoopProxy<UserEvent> cannot be shared between threads
  safely`。**规则**:winit 0.30 的 `platform_impl::EventLoopProxy` 内部是个 `Sender`,只
  `unsafe impl<T: Send> Send`,没有 `Sync`。要跨线程共享就包一层 `std::sync::Mutex`
  (`Mutex<T>: Sync where T: Send`),锁只在 `send_event` 那一瞬持有,**绝不跨 `.await`**。
  守护:`host_key::PromptingPolicy`(它必须满足 `HostKeyPolicy: Send + Sync`,bound 写在
  `mullion-ssh/src/known_hosts.rs` 的 trait 定义上)。
- **弹窗承载「安全决策」时不要给关闭按钮。** F3 主机密钥确认弹窗背后是握手线程正挂在
  `oneshot::Receiver` 上等回答;若窗口能被"什么都不做"地关掉,用户会以为「关掉 = 什么都没
  发生」,而握手会一直挂到 sshd 的 `LoginGraceTime`(默认 120s)才被对端掐断,期间 UI 看起来
  毫无反应。**规则**:`ui::host_key::show` 用 `egui::Modal`(而非 `egui::Window`)—— `Modal`
  本身不提供 `.open()`/关闭按钮这一构造,阻塞下层点击(点遮罩、按 Esc 都不会关闭它),只有
  两个显式动作按钮会给 `reply` 赋值;即便如此,`PromptingPolicy::decide` 侧仍按 fail-closed
  兜底 —— sender 被丢弃(GUI 退出/旧弹窗被新弹窗顶掉)或事件发送失败,`rx.await` 都返回非
  `Ok(true)`,一律判 `HostKeyDecision::Reject`,绝不「送不到就放行」。
  守护:`ui::host_key::show` 不带 `.open()`;`host_key::tests::dropped_sender_is_rejected`、
  `host_key::tests::send_event_failure_is_rejected`。
- **可取消的 `Modal` 要用 `ModalResponse::should_close()`,别手写 `key_pressed(Escape)`(F18)。**
  `egui::Modal::show` 的返回值不是 `()`,丢掉它就丢掉了唯一的关闭出口:`should_close()` 同时
  覆盖「按 Esc」与「点遮罩(backdrop)」两条路径,且它用 `consume_key` **消费**掉 Esc(仅在本
  modal 是最顶层、且没有 popup 打开时),不会让同一次 Esc 再被下层看见。手写 `key_pressed`
  只补了 Esc 半边、还不消费按键,而**点弹窗外面完全没有反应**——用户以为窗卡死了。
  **规则**:`let resp = Modal::new(..).show(..); if resp.should_close() { /* 等价于取消 */ }`。
  注意这与上一条不矛盾:承载**安全决策**的弹窗(主机密钥)故意**不**接 `should_close`,让它无法
  被"什么都不做"地关掉;粘贴确认这类「取消 = 不做事」的弹窗才该接,因为取消本身就是安全默认。
  另:`ScrollArea` 放进弹窗要给 `id_salt`(egui 0.30 已把 `id_source` 改名),否则同一帧里多个
  滚动区会撞 id。守护:`ui::paste::tests`(纯函数部分)+ 人工验收「Esc / 点背景都能取消」。
- **循环里渲染 `CollapsingHeader` 用动态标题当 id,同标题的两个实例会共享展开状态(F60)。**
  `CollapsingHeader::new(text)` 默认把 `text` 本身当 id 源(`egui-0.30.0/src/containers/
  collapsing_header.rs::new`:`id_salt = Id::new(text.text())`)。会话列表按分组折叠、循环里
  给每个桶建一个 header,若两个分组恰好同名**且**当前展示的会话数也相同(标题因此完全一致,
  如两个都叫「生产(1)」),`ui.make_persistent_id` 算出同一个 `Id`,两个 header 就共享同一份
  `CollapsingState`——编译不报错,点开其中一个,另一个下一帧跟着展开/收起。这是
  `ScrollArea`/上一条同类坑的变体:**任何在循环里用「内容拼出来的字符串」当 egui 部件默认
  id 源的写法都有此风险**,不限于 `CollapsingHeader`。**规则**:循环体内建的部件,只要标题/
  文案可能重复,一律显式 `.id_salt(稳定主键)`(优先用领域 id 如 `GroupId`,而不是文本或数组
  下标——下标在增删排序后会错位指向别的项)。**守护**:
  `session_manager::tests::collapsing_header_id_salt_disambiguates_same_titled_groups`——
  该测试直接调用 `show()` 内部实际使用的 `group_header` 函数(不是重抄一遍表达式),删掉
  `.id_salt(gid)` 这一行测试立即变红(已实测)。无头容器里这条能测,因为撞的是 egui 的
  `Id` 值本身(可用 `header_response.id` 读出来比较),不需要真的截图或判断像素。

- **测试里用 `ctx.set_pixels_per_point()` 设 DPI,会把画布悄悄变成 8000×8000。**
  它是 `Context::set_zoom_factor` 的包装,只在**下一帧**生效(`egui-0.30.0/src/context.rs`
  1953-1957 是包装、1994-2004 是延迟);生效那一帧会把「上一帧的 screen_rect」按新旧
  ppp 之比重新缩放后**盖掉本帧显式传入的 screen_rect**(同文件 462-475,注释自称是给
  「zoom 抖动」擦屁股)。对全新 `Context`,「上一帧」是 egui 内置的默认占位符
  10_000×10_000(`input_state/mod.rs:247`),ppp=1.25 缩出 8000×8000 —— 热身帧里的
  `Grid` 就在这块虚假巨宽画布上把列宽记忆定成 8000,而列宽记忆是跨帧累积在同一份
  `ctx.memory` 里的,第二帧即使 screen_rect 已正确回落到 300×600,分区分隔线照样被撑到
  x=8000。表现是「布局测试报了一个荒谬的越界数字」,极易被误判成生产代码 bug。
  **规则**:测试里设 DPI 一律走 `RawInput.viewports[&viewport_id].native_pixels_per_point
  = Some(ppp)`,它直接作用于当前帧,不经过 `set_zoom_factor`/抖动规避那条路径。
  **守护**:`session_manager::fields::tests::run_page_at`(所有页级越界测试的唯一入口,
  那里有同样的注释)。
- **量「有没有画出面板」必须先 `ui.set_clip_rect(Rect::EVERYTHING)`。** `CentralPanel`
  把 `ui` 的 clip_rect 钉死在面板矩形上(`panel.rs:1109`,注释「If we overflow, don't do
  so visibly (#4475)」),而控件 paint 前会查 `ui.is_rect_visible(rect)` —— **完全**越界的
  控件压根不产生 Shape,扫 `FullOutput::shapes` 找最右边界的测量对它失明(对**部分**
  越界、只被削掉半个字的仍有效)。撑大 clip_rect 只是为了让测量拿到「控件本该画在哪」,
  不代表生产代码的裁剪被关掉。
- **`TextEdit` 的内容和 hint 都会画到框外,`desired_width`/`clip_text` 拦不住。**
  singleline 走 `LayoutJob::simple_singleline`(`widgets/text_edit/builder.rs:514-521`),
  **忽略 `wrap_width`**,galley 永远按完整文本宽排版;再由 `builder.rs:726-734` 的
  `extra_size = galley.size() - rect.size()` 触发一次 `ui.allocate_rect`,把**父 `Ui`**
  的 min_rect/max_rect 一起撑宽(上游是为了让 ScrollArea 能滚到光标)。后果是同一个 `Ui`
  里跟在后面的所有兄弟控件都按撑宽后的 `available_width` 重算,越界「到处都是」而不是
  一处。`Grid::max_col_width` 同样拦不住(`allocate_rect` 直接打在父 `Ui` 上,不过 Grid
  的宽度计算)。**规则**:hint 文案有硬长度预算(300px 面板下框内容区约 192pt ≈ 12 个
  汉字),超了就是画到面板外;用户数据那一支管不住,只能靠 clip 裁剪。

## 字体

- **字号按 DPI 缩放**:`window.inner_size()` 是物理像素,字号也须物理像素:
  `px = pt * 96 * scale_factor / 72`。否则 Windows 高 DPI(125%/150%)下过小。
  当前只在建窗口时取一次 `scale_factor`,**未跟随 `ScaleFactorChanged`**——跨不同 DPI 显示器不更新(F21 待做)。
- **`Family::Name("Google Sans Code")` 须系统已装**,否则 cosmic-text 回退默认字体(不崩,对齐可能差)。
  字体族/字号当前硬编码,可配置见 spec **F21**。
