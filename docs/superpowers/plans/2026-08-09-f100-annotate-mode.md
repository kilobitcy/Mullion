# F100 标注模式 — 实现 plan

> 2026-08-09。共识来自一轮 `/grill-me`（14 问），参考 `kilobitcy/snapmark`（web 端 UI
> 注释工具：选元素 → 加注释 → 导出带 DOM 选择器的 Markdown 给 AI）。

## 要解决的问题

后续大量 UI 打磨工作里，人与 Claude 之间的带宽是瓶颈：用户看到「那个东西不对」，
要转成 Claude 能定位的东西，目前只能靠自然语言描述 + 让 Claude 猜是哪段代码。

snapmark 的真实价值不是「截图标注」，是**把视觉位置映射回代码标识**。egui 侧要
找的就是这个等价物。

图片传输这条路是死的：Windows 剪贴板里的图片粘不进 SSH 里跑的 Claude Code。
所以产出**必须是纯文本**。

## 已定的设计约束（共识，不再讨论）

1. 只盖 egui 外壳，**不碰 glyphon 终端网格**（它没有 widget，抓不到身份）。
2. **注释不在应用内敲**——只给编号，用户在 Claude Code 里口述「第 2 个太挤」。
   理由：中文注释输入要过 winit + 第三方 IME，那是 `CLAUDE.md`「你无法验证的
   东西」里的一条；且在应用内做文本框会撞 T8 的键盘路由陷阱。
3. 导出带 rect + 全局上下文（窗口尺寸 / 缩放 / 主题 / 密度档 / 当前页 / 选中项），
   **不带逐 widget 的样式值**——那些 Claude 自己去读代码更准。
4. 输出三档详细度，默认紧凑。
5. 快捷键 `Ctrl+Shift+F` 切模式 / `Ctrl+Shift+E` 导出 / `Esc` 退出。
6. 分两步落地：先骨架，再逐步铺细粒度标签。

## 偏离共识的一处：widget 身份的来源

**共识原文是「语义标签优先 + 未标的回退 egui `callstack`」。回退那一半做不到**——
不是麻烦，是 egui 0.30 的 API 边界上关着的。逐条查证（版本 `egui-0.30.0`）：

| 想走的路 | 实际情况 |
|---|---|
| 读 egui 抓好的 callstack | `src/lib.rs:445` 是 `mod callstack`（**非 `pub mod`**）；捕获结果存进 `pass_state::DebugRect`，而 `mod pass_state`（`lib.rs:428`）同样非 pub，`Context` 上也没有 pub 访问器 |
| 自己调 `callstack::capture()` | 同上，模块不导出。即使能调，它抓的是**当前**调用栈——我们在事件处理阶段调，栈里只有自己的处理函数，没有 widget 的构造位置 |
| 用 egui 的 `WidgetInfo` 拿语义标签 | `Context::register_widget_info` 整个函数体被 `#[cfg(debug_assertions)]` 包住（`context.rs:1349`），release exe 里永不记录；且只对调过 `Response::widget_info()` 的交互式 widget 填，我们手绘的会话行没有 |
| 枚举本帧所有 widget 的 rect | `WidgetRects::layers()` 是 pub，但拿得到它的 `Context::debug_painting`（`context.rs:2170`）是私有函数，没有 pub 出口 |

**替代方案更好**：`#[track_caller]` + `std::panic::Location::caller()`。

```rust
#[track_caller]
pub fn mark(ctx: &egui::Context, path: &str, rect: egui::Rect) {
    let src = std::panic::Location::caller();   // 编译期常量，零运行时开销
    …
}
```

比 egui 的 callstack 好在四点：编译期解析（不是运行时 backtrace）；给一行而不是
整条栈（无需解析、无噪音）；不需要 `callstack` feature、不需要 release `debug = 1`、
**不受 `panic = "abort"` 影响**（原方案的三条前提一并消失）；且能同时带上人写的
语义路径，比 `list.rs:349` 更有用——那种东西 Claude 还得再去读一遍代码才知道是什么。

代价：**必须插桩**，没有「零标签也能用」。但共识第 6 条本来就是「先骨架再铺标签」，
且**容器级**插桩就够——十几处覆盖整个会话管理器，而「左栏 / 搜索框 / 会话行」正是
人说话的粒度。未插桩的位置回退成「最近的已标容器 + 相对坐标」。

## 架构

新模块 `crates/mullion-app/src/ui/annotate.rs`。依赖方向不变（annotate 只认 egui）。

### 状态存哪

挂 `egui::Context` 的 temp data，**不放 `UiState`**。理由：插桩点散布在整个 UI 树
深处，那些地方只拿得到 `&Ui` / `&Painter`，拿不到 `&mut UiState`（它的可变借用被
外层闭包占着）。`badge.rs` 的纹理缓存出于同一个理由放在同一个地方。

```rust
struct State {
    on: bool,
    spots: Vec<Spot>,      // 本帧登记的候选，每帧清空重建
    picked: Vec<Picked>,   // 已点选的，跨帧保留，下标 + 1 = 屏上徽标编号
}
```

### 每帧顺序

1. UI 正常画，插桩点顺手 `mark()` 登记（模式关着时只是一次 bool 读 + 早退）。
2. `build_ui` 末尾调 `annotate::overlay(ctx, t, …)`：
   - 铺一层吃掉全部指针输入的顶层 `Area`——**必须吃掉**，否则点会话行就真切会话了；
   - 几何 hit test 找指针下**最小**的 spot（最小 = 最具体）→ 描边 + 显示它的路径；
   - 点击 → push 进 `picked`；已选中的再点一次 → 移除（编号顺延）；
   - 给每个 `picked` 画编号徽标。
3. `Ctrl+Shift+E` → 生成 Markdown → 剪贴板。

### 键盘

加在 `app.rs` 那个 `mods.ctrl && mods.shift` 分支里（`'c'`/`'v'` 已被 F18 占用，
`'f'`/`'e'` 空着）。那里已经是「键盘先判后喂」的正确位置（T8）。`Esc` 只在标注模式
开着时截住，否则照旧转发给终端。

### 导出格式（紧凑档）

```markdown
## Mullion UI 标注 · v0.1.27
窗口 1280x800 @1.0 · mullion-dark · 会话管理器（编辑器「基础」页）· 左栏 280px/Full

1. 会话管理器/左栏/会话行 — (45,118)-(307,162) — list.rs:349
2. 会话管理器/右栏/Tab 条 — (335,60)-(893,88) — editor.rs:214
```

标准档加父容器 rect 与完整层级；详细档连本帧**未选中**的 spots 一起列（用来问
「这一片区域都有些什么」）。

## 步骤

1. `annotate.rs`：`Spot` / `State` / `mark` / hit test / `picked` 增删 / Markdown 生成。
   纯逻辑部分全部单测（hit test 取最小、编号顺延、三档输出）。
2. `overlay()`：描边 + 徽标 + 吃指针。渲染部分靠 `examples/ui_shot.rs` 出图自查。
3. `app.rs` 接快捷键 + 剪贴板（复用 `clipboard.rs`）。
4. 铺容器级插桩：会话管理器（左栏各段 / 右栏 Tab / 各分节 / 底部按钮）、菜单栏、
   状态栏。
5. `ui_shot.rs` 加 `annotate` 场景（标注模式开 + 选中两处），出图确认徽标位置。
6. spec.md 记 F100；`docs/ui-shot.md` 补新场景。

## 不做

- 终端网格内的标注（约束 1）。
- 应用内输入注释（约束 2）。
- 像素基线快照（同 `docs/ui-shot.md` 的理由）。
- 导出图片（Windows 剪贴板图片进不了 SSH 里的 Claude Code，这是整个方案的起点）。
