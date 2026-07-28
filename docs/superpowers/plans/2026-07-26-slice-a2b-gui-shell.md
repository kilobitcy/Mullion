# Plan A2b — App 外壳「GUI」实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> **本片大量落「编译 + 人工验收」**:egui 渲染是否正确/不撕裂、输入分流手感、reflow 排版、keyring 真机——**无头容器测不了**(见 CLAUDE.md「你无法验证的东西」)。每个 GUI Task 末尾给「人工验收步骤」,汇总进最终 PR/验收清单。守护测试只覆盖能抽出的纯逻辑(A2a 已备)。

**Goal:** 把 A2a 的无头逻辑基座接成一个能用的桌面外壳:无参启动显示主窗体 + 会话管理弹窗,双击登录;`mullion user@host` 仍直连。菜单栏(对话/分屏/配置/关于)+ 状态栏,egui 画外壳、终端仍自绘 wgpu。

**Architecture:** `App` 从恒持 `ssh` 改为持 `Option<Connection>`,launcher 态 ↔ 终端态;连接统一走「开窗→runtime spawn connect→UserEvent 回送」。egui(0.30)与现有 winit0.30/wgpu23 同帧集成:每帧 `ctx.run` 出上下栏布局 → 取中央区 rect → 终端画进该 rect → egui paint 叠上。输入用 `egui_winit::State::on_window_event` + `shell::input_route::route` 分流。会话 UI 走 `shell::SessionStore`(A2a)。

**Tech Stack:** egui / egui-wgpu / egui-winit **0.30**(已验证与 wgpu 23.0.1 / winit 0.30.13 统一,无重复版本)· `rfd`(选私钥文件)· `time`(RFC3339 时间戳)· 现有 `mullion-store`/`shell`(A1/A2a)。

> 关联 spec:`docs/superpowers/specs/2026-07-25-app-shell-session-manager-design.md`(切片 A · §2.2/§4/§5)。
> 前置:A1(mullion-store)、A2a(shell 逻辑基座)已入 main。
> **本片解决待定 F(CLI 退出码)/ G(keyring 兜底)**,见 Task 2 / Task 6。

---

## egui 0.30 已核实签名(写代码照这个,勿凭记忆/勿用 0.35)

```
egui_winit::State::new(egui_ctx: egui::Context, viewport_id: ViewportId, display_target: &dyn HasDisplayHandle,
                       native_pixels_per_point: Option<f32>, theme: Option<Theme>, max_texture_side: Option<usize>) -> State
state.on_window_event(&window, &event: &WindowEvent) -> egui_winit::EventResponse { repaint: bool, consumed: bool }
state.take_egui_input(&window) -> egui::RawInput
state.handle_platform_output(&window, platform_output)
egui::Context::run(raw_input, |ctx| { ...build ui... }) -> egui::FullOutput { platform_output, textures_delta, shapes, pixels_per_point }
ctx.tessellate(full_output.shapes, full_output.pixels_per_point) -> Vec<ClippedPrimitive>
egui_wgpu::Renderer::new(&device, color_format: TextureFormat, depth: Option<TextureFormat>, msaa: u32, dithering: bool) -> Renderer
renderer.update_texture(&device, &queue, id, &image_delta)               // for (id, delta) in textures_delta.set
renderer.update_buffers(&device, &queue, &mut encoder, &paint_jobs, &screen_desc) -> Vec<CommandBuffer>
renderer.render(&mut render_pass: &mut RenderPass<'static>, &paint_jobs, &screen_desc)   // ← 'static:wgpu23 用 pass.forget_lifetime()
renderer.free_texture(&id)                                               // for id in textures_delta.free
egui_wgpu::ScreenDescriptor { size_in_pixels: [u32;2], pixels_per_point: f32 }
```

现有 `Gpu`(`gpu.rs`)公开 `surface / device / queue / config`,egui Renderer 直接用 `&gpu.device`、`gpu.config.format`。

---

## Task 0:加 egui/rfd/time 依赖 + 验编译 + 摸清渲染入口

**Files:** Modify root `Cargo.toml`(workspace deps)+ `crates/mullion-app/Cargo.toml`.

- [ ] **Step 1: workspace 依赖.** 在 `[workspace.dependencies]` 加:
```toml
egui = "0.30"
egui-wgpu = "0.30"
egui-winit = "0.30"
rfd = "0.15"
time = { version = "0.3", features = ["formatting"] }
```
`crates/mullion-app/Cargo.toml` 的 `[dependencies]` 加:
```toml
egui.workspace = true
egui-wgpu.workspace = true
egui-winit.workspace = true
rfd.workspace = true
time.workspace = true
```

- [ ] **Step 2: 验解析/编译.** Run: `cargo build -p mullion-app`. Expected: 通过(egui 0.30 与 wgpu23/winit0.30 已验证统一)。若报重复 wgpu/winit 版本,STOP 报告(不该发生)。
> `rfd 0.15` / `time 0.3` 版本若不存在按 registry 实际主版本改。

- [ ] **Step 3: 读现有渲染入口(不改代码,只记录).** 读 `crates/mullion-app/src/app.rs`,定位:①每帧渲染函数(acquire surface texture → encoder → begin render_pass → `gpu.draw_quads` + `text.render` → submit → present);② `Active` 结构;③ `about_to_wait`/帧率节流(T3/T7)与 `next_frame_at`。在提交信息或一句注释里记下这几处行号,供 Task 3/4/5 精确接线。

- [ ] **Step 4: Commit.**
```bash
git add Cargo.toml crates/mullion-app/Cargo.toml Cargo.lock
git commit -m "feat(app): 加 egui/egui-wgpu/egui-winit 0.30 + rfd/time 依赖 (切片 A2b/§4.1)"
```

---

## Task 1:`Connection` 抽出 + `App` 改持 `Option<Connection>`

**Files:** Modify `crates/mullion-app/src/app.rs`.

**领域陷阱**:动 `app.rs` 事件循环前读 CLAUDE.md 领域陷阱表(T1/T3/T7)与 `docs/gui-render-gotchas.md`。本 Task 后必须重跑 `app::tests`(T3/T4/T7 守护)与 keymap 全套,全绿才算完。

- [ ] **Step 1: 定义 `Connection`.** 在 `app.rs` 把当前 `App` 里与「一条连接」绑定的字段收进一个结构:
```rust
/// 一条活跃连接的全部状态。launcher 态时 App 持有 `None`。
struct Connection {
    ssh: SshSession,
    rx: Receiver<Vec<u8>>,
    pane: Pane,
    pacer: SyncFramePacer,
    limiter: FrameLimiter,
}
```

- [ ] **Step 2: `App` 改字段.** 把 `App` 里 `ssh/rx/pane/pacer/limiter` 五个字段换成 `conn: Option<Connection>`;保留 `_runtime / start / mods / kitty / active / next_frame_at`。`App::new` 签名改为 `new(runtime: Runtime) -> Self`(不再在构造时接 ssh/rx),`conn: None` 起步。

- [ ] **Step 3: 事件循环按 `Option` 分支.** 所有原来直接用 `self.ssh/self.rx/self.pane` 的地方,改成 `if let Some(conn) = self.conn.as_mut() { ... }`:
  - **排空 rx → feed emu → 回写 PtyWrite(T1)** 只在 `conn` 存在时跑;`conn` 为 None 时跳过(launcher 态无终端字节)。
  - 键盘/鼠标输入到终端、reflow 后 `ssh.resize`、`window_change` 同理只在 `conn` 存在时。
  - **帧率节流 / `WaitUntil` 复位(T3/T7)保持原样**——egui 恒绘,故重绘调度与 `conn` 无关(Task 3 会让 egui repaint 也参与)。这一步先保证 `conn=None` 时不 panic、不忙转。

- [ ] **Step 4: 编译 + 守护测试.**
```bash
cargo build -p mullion-app
cargo test -p mullion-app --lib   # app::tests(T3/T4/T7)+ 其余纯件必须全绿
cargo test -p mullion-term        # keymap T1/T5/T6 不受影响
```
Expected: 编译过,所有既有测试仍绿。
> `main.rs` 此刻会因 `App::new` 签名变化编译失败——**Task 2 修 main.rs**。本 Task 只需 `cargo build -p mullion-app`(lib)过 + lib 测试绿;若要整体过,可临时在 main.rs 用占位,但更干净是本 Task 与 Task 2 连续做、合并验证。

- [ ] **Step 5: Commit.**
```bash
git add crates/mullion-app/src/app.rs
git commit -m "refactor(app): 抽出 Connection,App 改持 Option<Connection>(launcher/终端两态)(§2.2)"
```
正文注明:跑了 app::tests(T3/T7)+ keymap,全绿。

---

## Task 2:统一异步 connect + 两种启动(待定 F:CLI 直连保留 exit(1))

**Files:** Modify `crates/mullion-app/src/app.rs`(UserEvent + 连接触发/回收)、`crates/mullion-app/src/main.rs`。

- [ ] **Step 1: 扩 `UserEvent`.** 现有只有 `Wake`。加两个变体:
```rust
pub enum UserEvent {
    Wake,
    ConnectOk { ssh: SshSession, rx: Receiver<Vec<u8>> },
    ConnectErr(String),      // 已格式化的可操作错误(F6 分类由 connect 内部给)
}
```

- [ ] **Step 2: App 里发起连接的方法.** 加:
```rust
impl App {
    /// 在 runtime 上异步连接;结果经 proxy 以 UserEvent 回送。`proxy` 是 App 持有的
    /// EventLoopProxy<UserEvent>(在 App::new 里从 main 注入)。
    fn spawn_connect(&self, cfg: mullion_ssh::config::SshConfig) {
        let proxy = self.proxy.clone();
        let policy = self.tofu.clone();               // 沿用现有内存 TofuAccept(切片 A2 不做 F3 持久化)
        let wake_proxy = self.proxy.clone();
        self._runtime.spawn(async move {
            let wake: std::sync::Arc<dyn Fn() + Send + Sync> =
                std::sync::Arc::new(move || { let _ = wake_proxy.send_event(UserEvent::Wake); });
            match mullion_ssh::session::connect(&cfg, policy, wake).await {
                Ok((ssh, rx)) => { let _ = proxy.send_event(UserEvent::ConnectOk { ssh, rx }); }
                Err(e) => { let _ = proxy.send_event(UserEvent::ConnectErr(e.to_string())); }
            }
        });
    }
}
```
> App 需新增字段 `proxy: EventLoopProxy<UserEvent>` 与 `tofu: Arc<TofuAccept>`,在 `App::new` 里接收。`connect` 的确切签名见 `crates/mullion-ssh/src/session.rs`(写前核对:`connect(&cfg, policy, wake) -> Result<(SshSession, Receiver<Vec<u8>>), ConnectError>`)。

- [ ] **Step 3: `user_event` 处理连接结果.** 在 `ApplicationHandler::user_event` 里:
```rust
UserEvent::ConnectOk { ssh, rx } => {
    let (cols, rows) = self.current_grid_dims();     // 中央区扣上下栏后的初值(Task 4 提供)
    let mut pane = Pane::new(PaneId(1), cols, rows);
    // 立刻按真实网格发一次 window_change(F34):新连接的 PTY 尺寸对齐
    let _ = ssh.resize(cols, rows);
    self.conn = Some(Connection { ssh, rx, pane, pacer: SyncFramePacer::new(), limiter: FrameLimiter::new(16) });
    // 关会话管理器模态(Task 6 的 UI 状态)、请求重绘
    self.ui.session_manager_open = false;
    if let Some(a) = &self.active { a.window.request_redraw(); }
}
UserEvent::ConnectErr(msg) => { self.ui.last_error = Some(msg); /* 状态栏/弹窗显示,Task 4/6 */ }
```

- [ ] **Step 4: 改 `main.rs` —— 两种启动 + 待定 F.**
```rust
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let runtime = /* 现有 build */;
    let event_loop = EventLoop::<UserEvent>::with_user_event().build().expect("事件循环");
    let proxy = event_loop.create_proxy();
    let tofu = Arc::new(TofuAccept::new(Arc::new(Mutex::new(KnownHosts::new()))));

    // 待定 F:CLI 直连(路径①)失败仍走 stderr + exit(1)(可脚本化);
    // 无参(路径②)进 launcher,失败 in-window 报错。
    let initial = if args.is_empty() {
        None
    } else {
        match cli::parse_args(&args) {
            Ok(cfg) => Some(cfg),
            Err(e) => { eprintln!("参数错误: {e}\n用法: mullion user@host [-p PORT] [-i KEYPATH]"); std::process::exit(2); }
        }
    };
    let mut app = App::new(runtime, proxy, tofu, initial); // initial: Option<SshConfig>
    event_loop.run_app(&mut app).expect("run_app");
}
```
App 在 `resumed`(窗口/GPU 就绪)后:若 `self.initial` 有值 → `self.spawn_connect(cfg)` 进终端态;否则打开会话管理器(`self.ui.session_manager_open = true`)。
> **待定 F 收尾**:CLI 直连失败现在是 in-window `ConnectErr`(窗口已开),丢了 exit(1)。**折中**:CLI 直连(`initial.is_some()`)时,若 `ConnectErr` 且尚未成功连过任何会话 → `eprintln!` + `event_loop.exit()` 后 `std::process::exit(1)`,保留可脚本化语义;launcher 态的失败只在窗口内提示。把这条逻辑放 `user_event` 的 `ConnectErr` 分支,用一个 `self.cli_direct: bool` 区分。

- [ ] **Step 5: 编译 + 全绿.** `cargo build -p mullion-app` + `cargo test --workspace` + clippy + fmt。
- [ ] **人工验收(记入清单)**:交叉编译 exe,`mullion user@真机` 直连成功进终端;`mullion` 无参弹会话管理器;直连失败有 exit code(`echo $LASTEXITCODE`)。

- [ ] **Step 6: Commit.**
```bash
git add crates/mullion-app/src/app.rs crates/mullion-app/src/main.rs
git commit -m "feat(app): 统一异步 connect 通道 + 两种启动;CLI 直连保留 exit(1)(§5/待定F)"
```

---

## Task 3:egui 同帧集成 —— State + Renderer + 渲染合成

**Files:** Modify `crates/mullion-app/src/app.rs`(`Active` 加 egui 字段 + 改每帧渲染)。

**领域陷阱 T2/T3/T7**:egui 的 repaint 请求不得破坏帧率节流与 `WaitUntil` 复位;终端同步块(BSU/ESU)攒帧仍生效。改完重跑 `render::tests`/`frame::tests`/`app::tests`。

- [ ] **Step 1: `Active` 加 egui.** 在 `Active` 结构加:
```rust
egui_ctx: egui::Context,
egui_state: egui_winit::State,
egui_renderer: egui_wgpu::Renderer,
```
在 `resumed` 里建 GPU 之后构造(用已核实的 0.30 签名):
```rust
let egui_ctx = egui::Context::default();
let egui_state = egui_winit::State::new(
    egui_ctx.clone(), egui::ViewportId::ROOT, &*window,
    Some(window.scale_factor() as f32), None, None,
);
let egui_renderer = egui_wgpu::Renderer::new(&gpu.device, gpu.config.format, None, 1, false);
```

- [ ] **Step 2: 每帧渲染改成 egui-先布局-后合成.** 把现有「acquire → encoder → pass(quads+text)→ submit → present」改成下面顺序(伪代码,按现有变量名落实):
```rust
// 1) egui 跑 UI,拿布局 + 绘制指令。build_ui 在 Task 4/6 填(菜单/状态栏/会话弹窗)。
let raw_input = active.egui_state.take_egui_input(&active.window);
let full_output = active.egui_ctx.run(raw_input, |ctx| {
    crate::ui::build_ui(ctx, /* &mut self.ui, &self.conn, ... 见 Task 4/6 */);
});
active.egui_state.handle_platform_output(&active.window, full_output.platform_output);
let paint_jobs = active.egui_ctx.tessellate(full_output.shapes, full_output.pixels_per_point);
let screen = egui_wgpu::ScreenDescriptor {
    size_in_pixels: [active.gpu.config.width, active.gpu.config.height],
    pixels_per_point: full_output.pixels_per_point,
};

// 2) 中央区 rect(窗口减去 egui 上下栏)——Task 4 用 ctx 的可用区算,存进 self.ui.central_px。
//    终端 grid rows/cols 用 shell::viewport::grid_dims(central_px, cell_px, (2,1)) 得到(F34/T4)。

let frame = active.gpu.surface.get_current_texture()?;   // 现有获取方式
let view = frame.texture.create_view(&Default::default());
let mut encoder = active.gpu.device.create_command_encoder(&Default::default());

// 3) egui 纹理/缓冲更新(在 pass 之前)
for (id, delta) in &full_output.textures_delta.set {
    active.egui_renderer.update_texture(&active.gpu.device, &active.gpu.queue, *id, delta);
}
let egui_cmds = active.egui_renderer.update_buffers(
    &active.gpu.device, &active.gpu.queue, &mut encoder, &paint_jobs, &screen);

{
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("frame"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: &view,
            resolve_target: None,
            ops: wgpu::Operations { load: wgpu::LoadOp::Clear(bg), store: wgpu::StoreOp::Store },
        })],
        depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None,
    });
    // 3a) 终端画进中央区:用 pass.set_viewport(central.x, central.y, central.w, central.h, 0, 1)
    //     限制到中央矩形,再 gpu.draw_quads(&mut pass, ...) + text.render(&mut pass, ...)。
    //     (现有 quads/text 用整窗坐标;A2b 里终端坐标系仍整窗,靠 set_viewport 平移/裁剪到中央区。)
    // 3b) egui 画整窗(菜单/状态栏/弹窗):需要 RenderPass<'static>
    let mut spass = pass.forget_lifetime();
    active.egui_renderer.render(&mut spass, &paint_jobs, &screen);
}
active.gpu.queue.submit(egui_cmds.into_iter().chain(std::iter::once(encoder.finish())));
frame.present();
for id in &full_output.textures_delta.free { active.egui_renderer.free_texture(id); }
```
> 细节以现有 `app.rs` 渲染函数为准(Task 0 记录的行号)。**关键点**:egui `render` 要 `RenderPass<'static>`,用 wgpu23 的 `pass.forget_lifetime()`;终端限制到中央区用 `set_viewport`(避免画到菜单/状态栏底下)。若终端现用整窗 NDC 且不便 set_viewport,退而用 scissor rect,或把终端也整窗画、菜单/状态栏用不透明 egui panel 盖住——**三选一由实现者按现有渲染实测,记进注释**。

- [ ] **Step 3: 帧率/repaint(T3/T7).** `full_output.platform_output` 里 egui 若请求重绘(`ctx.has_requested_repaint()` 或 `viewport_output` 的 repaint delay),把它并进现有 `next_frame_at` 调度:egui 要重绘 → 参与「下次何时 request_redraw」,但仍受 `FrameLimiter`(~60fps)上限。**保持 `about_to_wait` 三分支显式复位 control_flow 的结构不变**(T7 红线)。

- [ ] **Step 4: 编译 + 守护测试全绿**(render/frame/app 测试)。
- [ ] **人工验收(记入清单)**:交叉编译 exe,窗口出现、egui 能画(先放一个临时 `egui::Window` 测试标签)、终端在中央区正常、**不撕裂**、空闲 CPU 低(T3)、流式输出不抖(T2)。

- [ ] **Step 5: Commit.**
```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): egui 0.30 同帧集成(布局→中央区终端→egui paint,forget_lifetime)(§4.1/T2/T3/T7)"
```

---

## Task 4:菜单栏 + 状态栏 + 中央区 rect → cols/rows

**Files:** Create `crates/mullion-app/src/ui/mod.rs`(egui UI 构建,与 app 状态解耦)、`crates/mullion-app/src/ui/chrome.rs`;modify `app.rs`(调用 + 存中央区 rect)、`lib.rs`(`pub mod ui;`)。

- [ ] **Step 1: UI 状态结构.** `ui/mod.rs` 里定义 App 的 UI 侧状态(与渲染解耦,便于 Task 6 扩展):
```rust
pub struct UiState {
    pub session_manager_open: bool,
    pub about_open: bool,
    pub last_error: Option<String>,
    pub central_px: (u32, u32),      // 中央区可用像素,egui 布局后写入(喂 viewport::grid_dims)
    // Task 6 再加:editor 表单缓冲、选中会话等
}
```

- [ ] **Step 2: 菜单栏 + 状态栏(chrome.rs).**
```rust
pub fn top_menu(ctx: &egui::Context, ui_state: &mut UiState, connected: bool) {
    egui::TopBottomPanel::top("menu").show(ctx, |ui| {
        egui::menu::bar(ui, |ui| {
            ui.menu_button("对话", |ui| {
                if ui.button("会话管理器").clicked() { ui_state.session_manager_open = true; ui.close_menu(); }
                if ui.add_enabled(connected, egui::Button::new("断开")).clicked() { ui_state.request_disconnect = true; ui.close_menu(); }
                if ui.button("退出").clicked() { ui_state.request_quit = true; ui.close_menu(); }
            });
            ui.menu_button("分屏", |ui| { ui.add_enabled(false, egui::Button::new("(切片 B)")); }); // §禁用占位
            ui.menu_button("配置", |ui| { ui.add_enabled(false, egui::Button::new("(切片 C:字体等)")); });
            ui.menu_button("关于", |ui| { if ui.button("关于 Mullion").clicked() { ui_state.about_open = true; ui.close_menu(); } });
        });
    });
}

pub fn status_bar(ctx: &egui::Context, status: &str) {
    egui::TopBottomPanel::bottom("status").show(ctx, |ui| { ui.label(status); });
}
```
(`UiState` 相应加 `request_disconnect / request_quit` bool。)

- [ ] **Step 3: 中央区 rect.** 在 `build_ui` 里,菜单/状态栏 panel show 之后,用 `ctx.available_rect()`(剩余中央区,单位 point)× `pixels_per_point` 换成像素写入 `ui_state.central_px`。app.rs 每帧据此 `let (cols, rows) = shell::viewport::grid_dims(ui_state.central_px, cell_px, (2,1));`,若变化则 `conn.pane` reflow + `ssh.resize`(F34/T4)。

- [ ] **Step 4: build_ui 组装.** `ui/mod.rs::build_ui(ctx, ui_state, connected, status)` 依次调 `top_menu` / `status_bar` / (Task 6) 会话弹窗 / 关于弹窗。app.rs Task 3 的 `ctx.run` 闭包里调它。

- [ ] **Step 5: 编译 + 全绿 + 处理 request_disconnect/quit**(disconnect → `self.conn=None` 回 launcher;quit → `event_loop.exit()`,在 app.rs 每帧末尾读这些 flag)。
- [ ] **人工验收**:菜单栏四项显示、分屏/配置禁用带提示、状态栏显示连接态、上下栏扣除后终端行数正确(远端 `tput lines` 对得上)。

- [ ] **Step 6: Commit.**
```bash
git add crates/mullion-app/src/ui/ crates/mullion-app/src/lib.rs crates/mullion-app/src/app.rs
git commit -m "feat(app): egui 菜单栏(对话/分屏/配置/关于)+ 状态栏 + 中央区→cols/rows reflow(§4.2/§4.4/F34)"
```

---

## Task 5:输入分流接线(守 T5/T6)

**Files:** Modify `crates/mullion-app/src/app.rs`(`window_event` 分发)。

- [ ] **Step 1: 每个 WindowEvent 先过 egui,再按 route 决策.** 在 `ApplicationHandler::window_event` 开头:
```rust
if let Some(active) = &mut self.active {
    let resp = active.egui_state.on_window_event(&active.window, &event);
    if resp.repaint { active.window.request_redraw(); }
    let modal = self.ui.session_manager_open || self.ui.about_open || self.ui.editor_open;
    let wants_kbd = active.egui_ctx.wants_keyboard_input();
    let wants_ptr = active.egui_ctx.wants_pointer_input();
    let kind_kbd = matches!(event, WindowEvent::KeyboardInput { .. });
    let kind_ptr = matches!(event, WindowEvent::MouseInput { .. } | WindowEvent::MouseWheel { .. } | WindowEvent::CursorMoved { .. });
    if kind_kbd || kind_ptr {
        let kind = if kind_kbd { shell::input_route::InputKind::Keyboard } else { shell::input_route::InputKind::Pointer };
        match shell::input_route::route(modal, wants_kbd, wants_ptr, kind) {
            shell::input_route::Route::Egui => { /* egui 已吃,return 不转终端 */ return; }
            shell::input_route::Route::Terminal => { /* 落到下面既有终端 keymap/SGR 分支 */ }
        }
    }
}
// …既有:CloseRequested / Resized / RedrawRequested / 终端 KeyboardInput→keymap / MouseInput→SGR…
// 终端相关分支全部包在 `if let Some(conn) = self.conn.as_mut()` 里(Task 1)。
```
> **T5/T6 红线**:`Route::Terminal` 分支必须原样走既有 `keymap`(Shift+Enter、Ctrl+J、Kitty 退化)与 SGR 鼠标(Shift 逃生门)。egui 只在 `wants_*` 或模态时截获。别把整段 KeyboardInput 无条件喂给 egui。

- [ ] **Step 2: 编译 + keymap/frame 守护测试全绿**(`cargo test -p mullion-term`,`cargo test -p mullion-app`)。这些纯逻辑测试保护 T1/T5/T6/T3/T7 不被本次分发改动破坏。
- [ ] **人工验收**:终端聚焦时方向键/快捷键进 Claude Code(不被 egui 吞);打开会话弹窗后键盘进表单、不漏到终端;Shift+划选仍能本地选中(T5)。

- [ ] **Step 3: Commit.**
```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): 输入分流(egui on_window_event + shell::route),守 T5/T6(§4.5)"
```

---

## Task 6:会话管理弹窗 —— 列表 + 编辑表单 + CRUD + 双击连接(待定 G)

**Files:** Create `crates/mullion-app/src/ui/session_manager.rs`;modify `ui/mod.rs`、`app.rs`(持有 `SessionStore` + KeyringSource)。

- [ ] **Step 1: app 持有 store.** `App` 加 `store: Option<shell::store::SessionStore>`。`resumed` 时:
```rust
let dir = shell::store::config_dir();
self.store = match dir {
    Some(d) => match shell::store::SessionStore::open(d, &mullion_store::KeyringSource::new()) {
        Ok(s) => Some(s),
        Err(e) => { self.ui.last_error = Some(format!("会话库打开失败:{e}")); None } // 待定 G:keyring 不可用→报错不崩
    },
    None => { self.ui.last_error = Some("无法定位配置目录".into()); None }
};
```
> **待定 G 收尾**:keyring 打开失败 → 状态栏/弹窗报可操作错误,程序继续跑(能看菜单/关于),会话功能禁用而非崩溃。

- [ ] **Step 2: 会话列表 + CRUD 弹窗(session_manager.rs).** `egui::Window::new("会话管理器").open(&mut ui_state.session_manager_open)`:
  - 表格列:名称 / 主机:端口 / 协议 / 用户 / 修改时间(`store.list()`)。
  - 行双击 → `ui_state.connect_request = Some(id)`(app.rs 读它 → `store.ssh_config_for(id)` → `spawn_connect`;`Err(SftpNotSupported)` → 提示,不连)。
  - 按钮:`新建`(打开空编辑表单)/`编辑`(选中项填表单)/`删除`(确认 → `store.delete(id)` + `store.save()`)。
- [ ] **Step 3: 编辑子表单.** 字段:名称/主机/端口/协议(`egui::ComboBox` ssh|sftp)/用户名/备注/认证(密码框 or 公钥 path+`rfd::FileDialog` 选文件 + 可选口令框)。保存:
```rust
let now = time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).unwrap();
let draft = /* 从表单缓冲组 SessionDraft,secret 装密码/口令 */;
match editing_id { Some(id) => store.update(id, draft, &now)?, None => { store.add(draft, &now); } }
store.save()?;
```

- [ ] **Step 4: 编译 + 全绿.** 纯 GUI,无新单测;`shell::store`/`session_map` 的逻辑已在 A2a 测过。
- [ ] **人工验收(重点)**:无参启动弹出;新建密码会话→保存→重启仍在且**双击直连不再问密码**(一次配置一直使用);编辑/删除生效;sftp 会话双击提示未实现;keyring 不可用时报错不崩。**真机验 keyring 持久化**(补 A1 缺口)。

- [ ] **Step 5: Commit.**
```bash
git add crates/mullion-app/src/ui/ crates/mullion-app/src/app.rs
git commit -m "feat(app): 会话管理弹窗 CRUD + 双击连接 + keyring 兜底(§4.3/§1.2/待定G)"
```

---

## Task 7:关于弹窗 + launcher 自动弹窗 + 打磨

**Files:** Modify `crates/mullion-app/src/ui/`(关于)、`app.rs`。

- [ ] **Step 1: 关于弹窗.** `egui::Window::new("关于").open(&mut ui_state.about_open)`:名称 Mullion、版本(`env!("CARGO_PKG_VERSION")`)、仓库、一句话定位。
- [ ] **Step 2: launcher 自动弹窗.** 无参启动(`initial.is_none()`)时 `resumed` 后 `ui_state.session_manager_open = true`(Task 2 已接,此处确认)。
- [ ] **Step 3: 断开/退出 flag 落实**(Task 4 的 request_disconnect/quit 若未完成,这里收口)。
- [ ] **Step 4: 编译 + 全绿 + clippy + fmt.**
- [ ] **Step 5: Commit.**
```bash
git commit -am "feat(app): 关于弹窗 + launcher 自动弹会话管理器 + 断开/退出(§2/§4.4)"
```

---

## Task 8:交叉编译 exe + 人工验收清单 + 文档

**Files:** Create `docs/adr-007-egui-chrome.md`;modify `docs/gui-render-gotchas.md`(egui 集成坑)、`CLAUDE.md`(架构表补 egui;若 shell/ui 值得记)。

- [ ] **Step 1: 交叉编译 Windows exe.**
```bash
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app
```
Expected: 出 `target/x86_64-pc-windows-gnu/release/mullion.exe`(交人工实测)。若 egui/rfd 在 mingw 目标下有链接问题,按 `docs/cross-compile-windows.md` 排查并补记。

- [ ] **Step 2: 写 ADR-007(egui 做外壳)** —— 决策 + 备选(egui vs 手搓 vs 原生弹窗,已在 spec 定)+ 版本对齐(egui 0.30 ↔ wgpu 23.0.1 / winit 0.30.13)+ forget_lifetime/输入分流/中央区 rect 三个集成坑。

- [ ] **Step 3: gui-render-gotchas.md 追加 egui 段** —— `RenderPass<'static>`(forget_lifetime)、egui repaint 与帧率闸协调(T3/T7)、`wants_keyboard_input` 分流、中央区 rect 取自本帧布局。

- [ ] **Step 4: 汇总人工验收清单(写进 PR 描述 / 本文件末尾).** 逐条列 Task 2/3/4/5/6 的人工验收项 + spec §6.3 的 6 条(egui 不撕裂、repaint 不忙转、输入分流、双击端到端、CRUD 手感、reflow 列数)+ A1 遗留的 keyring 真机验证。**未经人眼确认前,不得声称「切片 A 完成」——只能说「代码就绪,待人工验收」。**

- [ ] **Step 5: Commit.**
```bash
git add docs/ CLAUDE.md
git commit -m "docs: ADR-007(egui 外壳)+ gui-render-gotchas egui 段 + A2b 人工验收清单"
```

---

## 自查(写完计划的复盘)

- **Spec 覆盖**:§2.2 状态机 → Task 1;§5 连接/两启动/待定F → Task 2;§4.1 egui 同帧 → Task 3;§4.2/§4.4 菜单/状态栏/rect → Task 4;§4.5 分流 → Task 5;§4.3/§1.2 会话 UI/CRUD/双击/待定G → Task 6;§2 关于/launcher → Task 7;§6.3 人工验收 + 文档 → Task 8。
- **API 漂移**:egui 0.30 全部签名已从 registry 源码核实并列在开头;`mullion_ssh::session::connect` 签名 Task 2 写前再核;`rfd`/`time` 主版本 Task 0 验。
- **领域陷阱**:动 app.rs 的 Task 1/2/3/5 都要求重跑 app::tests(T3/T4/T7)+ keymap(T1/T5/T6)+ render/frame(T2/T3/T7);T5/T6 红线在 Task 5 明列。
- **可测性诚实**:GUI 行为无头测不了,每个 GUI Task 给人工验收项,Task 8 汇总;纯逻辑(A2a 的 route/grid_dims/map)已单测。
- **待定收尾**:F(CLI exit(1))→ Task 2;G(keyring 兜底报错不崩)→ Task 6。

## 完成的判定

**代码就绪 ≠ 切片 A 完成。** 判定 = Task 8 的人工验收清单在 Windows 11 实机逐条过(尤其:无参启动弹会话管理器 → 新建会话 → 双击直连不再问密码 → 终端跑 Claude Code 不闪 → 输入分流正确)。这条只有人眼能签字(spec §6「v0.1 判定是人工目视」)。

---

## 人工验收清单(切片 A · v0.1.3 交付构建)

代码 + 无头能测的逻辑都绿;下面每条只有 Windows 11 实机人眼能判。exe 从
GitHub Release **v0.1.3** 下载(`Get-FileHash` 核对 sha256)。已在 v0.1.1/v0.1.2
验过的基座项(单窗口无黑框、菜单/状态栏中文不 tofu、CLI 直连出终端、不闪)不再重列。

**启动 / 外壳**
- [ ] 无参 `.\mullion.exe` → 自动弹出「会话管理器」窗口(Task 7)。
- [ ] 菜单「关于→关于 Mullion」→ 弹窗显示 名称 / 版本 `0.1.3` / 定位 / GitHub 链接(Task 7)。
- [ ] 菜单「对话→退出」关窗;连接后「对话→断开」回到空 launcher(Task 4/7)。

**会话 CRUD(Task 6,核心)**
- [ ] 会话管理器列表显示 名称 / 主机:端口 / 协议 / 用户 / 修改时间。
- [ ] 「新建」→ 填密码会话(名称/主机/端口/ssh/用户/密码)→ 保存 → 列表出现该会话。
- [ ] **重启 mullion → 会话仍在,双击直连不再问密码**(F70「一次配置一直使用」;同时验 A1 的
      keyring 真机持久化——这是 A1 headless 只测了 InMemoryKey 的遗留缺口)。
- [ ] 「编辑」改端口/备注 → 保存 → 生效;「删除」→ 二次确认 → 列表移除。
- [ ] 新建**公钥**会话(选私钥文件走「选择…」原生对话框,可选口令)→ 双击直连成功。
- [ ] 双击一个 **sftp** 协议会话 → 会话管理器内红字提示「SFTP 尚未实现」,**不连、不崩**。
- [ ] 连接失败(故意填错密码/主机)→ 会话管理器内红字显示错误,**程序不消失**(复核 #1/#2)。

**输入分流(Task 5/6,守 T5/T6)**
- [ ] 终端聚焦时方向键 / Shift+Enter / 快捷键进远端 Claude Code,不被 egui 吞。
- [ ] 打开编辑表单时键盘进表单、不漏到终端(含关掉列表主窗、编辑器仍开的情况,复核 #4)。
- [ ] `/tui fullscreen` 类全屏 TUI 下 Shift+划选仍能本地选中复制(T5)。

**渲染 / 性能(spec §6.3,人眼)**
- [ ] 分屏尚未实现(切片 B),此处只验单 pane:窗口 resize 后远端 `tput lines/cols` 与实际排版一致(F34/T4)。
- [ ] 流式输出(`ls -R /` / Claude Code 刷屏)不撕裂、不抖(T2),空闲 CPU 低(T3)。

**已知不做(非 bug)**:分屏、配置(字体等,菜单灰)、DECCKM 方向键模式、CJK 宽字与背景块的
亚像素对齐(adr-001 取舍)、字体不可配(F21)、断线重连(S3)。

签字前不得声称「切片 A 完成」——只能说「代码就绪,待人工验收」。
