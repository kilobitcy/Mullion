# ADR-007: 用 egui 做外壳(菜单/状态栏/会话弹窗)

- 状态: 已接受
- 日期: 2026-07-27
- 关联: 切片 A2b spec/plan、ADR-001(glyphon 文字层)、ADR-006(mullion-store)

## 背景

切片 A2b 要给单 pane 终端加「外壳」:菜单栏(对话/分屏/配置/关于)、状态栏、会话
管理弹窗(列表 + 增删改查 + 编辑表单 + 文件选择)、关于弹窗。终端本身仍由 wgpu +
glyphon 自绘(ADR-001)。问题是这层 GUI 控件用什么画。

## 决策

用 **egui 0.30**(+ `egui-wgpu` / `egui-winit`)画外壳,与既有 winit 0.30 / wgpu 23
**同帧集成**:每帧 `egui_ctx.run` 出上下栏布局 → 取中央区 rect → 终端画进该 rect →
egui paint 叠上,单个 render pass 提交。文件选择用 `rfd`(原生对话框)。

## 备选与否决理由

- **手搓 GUI(用 glyphon + 自画矩形做按钮/输入框)**:菜单、下拉、文本输入框、焦点、
  IME、剪贴板、弹窗层叠全要自己实现——工作量巨大且极易出 bug,与「把精力放在终端仿真
  正确性」的项目重心背离。否掉。
- **原生系统弹窗(每个对话框一个 OS 窗口)**:跨平台不一致,且会话管理器这种富交互界面
  用原生控件拼装繁琐;与 wgpu 主窗口的集成也别扭。仅文件选择用原生(rfd),其余否掉。
- **换整套 GUI 框架(如 iced/slint)**:会动到主窗口/渲染管线的根基,推翻 ADR-001 的
  glyphon 终端层。代价过大,收益仅在外壳。否掉。

选 egui 的关键前提:**它能与锁定的 wgpu 23 / winit 0.30 共存而不强制升级**——已 spike
验证 `egui 0.30` ↔ `wgpu 23.0.1` / `winit 0.30.13` 版本统一(无重复版本),这是选它而非
更高版本 egui 的直接原因(高版本 egui 绑更高 wgpu,会连累终端层)。

## 关键实现取舍(集成坑,详见 gui-render-gotchas.md「egui 外壳」段)

- **`RenderPass<'static>`**:egui `Renderer::render` 要 `'static` pass;wgpu 23 用
  `pass.forget_lifetime()`。终端两趟(背景/文字)必须写在 `forget_lifetime` 之前,两趟与
  egui 画进**同一个** pass(它消费 pass 自身)。
- **输入分流**:每个 `WindowEvent` 先过 `egui_state.on_window_event`,再按
  `shell::input_route::route(modal, wants_kbd, wants_ptr, kind)` 决定归 egui 还是终端。
  守 T5/T6:终端聚焦时方向键/Shift+Enter 等必须回到既有 keymap,egui 只在有焦点控件或
  模态弹窗时截获。
- **中央区 rect**:egui 上下栏布局后 `ctx.available_rect() × pixels_per_point` 得中央区
  像素,喂 `shell::viewport::grid_dims` 得终端 cols/rows(F34/T4)。天然滞后一帧,可接受。
- **egui 闭包借不到 store**:UI 构建在 `egui_ctx.run(|ctx| ...)` 闭包里只有 `&mut UiState`,
  借不到 `&mut SessionStore`。所以 CRUD/连接一律 **UI 写 intent 到 UiState → app.rs 在
  render_frame 返回、借用释放后统一施加**(与 request_disconnect/quit 同构)。
- **帧率共存(T3/T7)**:egui 的 repaint 请求(`viewport_output` 的 `repaint_delay`)并进
  既有 `next_frame_at`/`WaitUntil` 排期,仍受 `FrameLimiter` 上限,不在原地 request_redraw。
- **GUI 子系统 + CJK 字体**:见 gotchas 段(v0.1.2 两坑)。

## 后果

- 依赖新增 egui/egui-wgpu/egui-winit 0.30 + rfd + time;`windows-sys` 转为直接依赖
  (cfg(windows),开 Console feature)。
- 升级纪律:egui 与 wgpu/winit 版本强耦合,后续升 wgpu 必须同步核对 egui 兼容(R1)。
- 终端正确性不受影响:egui 只画外壳,终端仍走 glyphon,ADR-001 的取舍不变。
