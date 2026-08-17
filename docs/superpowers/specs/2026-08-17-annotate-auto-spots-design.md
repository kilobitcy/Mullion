# F100 标注模式：自动候选 + 默认详细档（设计）

日期：2026-08-17 · 起点 `main`（v0.1.47）

来源：用户实测反馈——「按 `Ctrl+Shift+F` 后，很多界面元素无法选中，开启后默认详细
模式，可以选中大部分控件和 UI 元素」。

现状的成因：候选**全部**来自手工 `annotate::mark()`，全项目 45 处，且都是**容器级**
（左栏 / 搜索框 / 会话行 / 模式条…）。按钮、输入框、复选框、下拉框、滑块一个都没有
登记，所以点不中——用户能指的粒度比他想指的粗一整级。

不新增 spec 编号，这是 F100 自身的增强；`spec.md` 的 F100 条目补一句「候选 = 手工
插桩 + accesskit 自动树」，默认档从紧凑改详细。

---

## 1. 已定死的决策

| # | 决策 | 理由 / 影响 |
|---|---|---|
| D1 | 候选**两条腿**：手工插桩 + accesskit 自动树 | 手工那条带 `文件:行号`（F100 的全部价值），自动那条负责覆盖面；只留一条都不成立 |
| D2 | 自动树来源 = egui 的 `accesskit` feature，**只在标注模式开着时** `enable_accesskit()` | 模式关着 egui 压根不构树，「零登记开销」这条不变 |
| D3 | 正式依赖开 `egui/accesskit` + `egui-winit/accesskit`，**不调 `init_accesskit`** | 两个同名 feature 必须同步开否则 `PlatformOutput` 解构 E0027；不 init 就不注册 Windows UIA adapter，无平台侧开销 |
| D4 | 自动候选的路径 = **包住它的最小手工容器** + 角色 + 标签 | 「设置/字体页/按钮「恢复默认」」；无容器时挂 `自动/`。让导出至少能定位到文件 |
| D5 | `Spot.src` 从 `&'static Location` 改成 enum | 自动候选没有插桩点，硬塞一个假的等于骗人；手工 = `文件:行号`，自动 = 容器插桩点 + `(自动)` 标记 |
| D6 | 自动候选与手工 spot 矩形几乎重合（<1pt）时**丢自动那个** | 同一个东西不该在候选表里出现两次，且手工那份信息更多 |
| D7 | 同容器下同名多个 → 加 `[2] [3]` | 一屏五个「删除」按钮，不编号在导出里分不出是哪个 |
| D8 | `Detail::default()` 改 `Full`，导出**不设上限、全倒** | 用户明确选的；「静默截断」在本项目是禁止项，不设限就不存在截断 |
| D9 | 自动候选**慢一帧**：本帧的树给下一帧当候选 | `accesskit_update` 只在 `ctx.run` 返回后才拿得到，而 `overlay()` 在 `ctx.run` 内部。静止界面无感 |
| D10 | 只在 `auto` 为空时 `request_repaint()` | 刚进模式那一帧要主动催一帧出来，否则自动候选永远不出现；持续催帧会撞 T3/N3 红线 |
| D11 | overlay 自己那块全屏 click 区按 **widget id** 从自动候选里剔除 | `Sense::click()` 的 `focusable: true`，它自己会进树，变成一个盖住全屏的候选 |
| D12 | 顺带补手工插桩：终端 pane 区、pane 标题条、分隔条 | 它们不是 egui widget，accesskit 覆盖不到；而「这块终端」正是最常要指的东西 |

---

## 2. 数据流

```
annotate::toggle(ctx)
  └─ on  → ctx.enable_accesskit()      模式开：egui 开始每帧构 accesskit 树
     off → ctx.disable_accesskit()     模式关：停止构树，清空 auto/picked/spots

app.rs::render_frame
  let mut full_output = egui_ctx.run(raw_input, |ctx| build_ui(...))
        └─ build_ui 末尾 annotate::overlay(ctx, …)
              候选 = 本帧 spots(手工) + 上一帧 auto(自动，此刻转成 Spot)
  annotate::ingest_accesskit(&ctx, full_output.platform_output.accesskit_update.take())
        └─ 归约成 Vec<AutoNode>，存进 State.auto，供下一帧
  egui_state.handle_platform_output(&window, full_output.platform_output)
```

`accesskit_update` 必须在 `handle_platform_output` **之前** `take()` 走——那个函数
按值吃掉整个 `PlatformOutput`。`take()` 而不是 clone：adapter 没 init，egui-winit
拿到 `None` 什么都不做。

egui 每帧生成的是**全量**树（`context.rs:2358` 从 `this_pass.accesskit_state` 取走
本帧构建的全部节点），不是增量，所以直接整份替换 `State.auto`。

---

## 3. 类型改动

```rust
/// 一处候选的来源。
pub enum Src {
    /// 手工 `mark()` 的插桩点。编译期常量。
    Site(&'static Location<'static>),
    /// accesskit 自动登记。`container` = 包住它的最小手工容器的插桩点，
    /// 没有则 `None`。
    Auto { container: Option<String> },
}
```

渲染规则（`src_of`）：

| 来源 | 导出里写成 |
|---|---|
| `Site` | `crates/mullion-app/src/ui/settings.rs:123` |
| `Auto{Some}` | `crates/mullion-app/src/ui/settings.rs:123（容器 · 自动候选）` |
| `Auto{None}` | `（自动候选 · 无插桩容器）` |

`Picked.src` 仍是 `String`（跨帧存活、要判等），不受影响。

accesskit 节点先归约成一个**不含 accesskit 类型**的中间结构，纯函数只认它：

```rust
struct AutoNode { rect: egui::Rect, role: &'static str, label: Option<String> }

fn auto_spots(auto: &[AutoNode], manual: &[Spot]) -> Vec<Spot>
```

分两层是为了单测：构造 `AutoNode` 是三个字段的字面量，构造 `accesskit::Node` 要串
一堆 builder。归约那层（`accesskit::TreeUpdate → Vec<AutoNode>`）由 kittest 端到端
兜住。

角色中文名（`Role → &'static str`）：按钮 / 输入框 / 复选框 / 单选 / 下拉框 / 滑块 /
数值框 / 文字 / 链接 / 色块 / 进度 / 窗口 / 控件（`Unknown` 兜底）。`bounds` 为
`None` 或退化的节点直接丢——跟 `mark()` 里那条一致。

---

## 4. 测试

纯函数 `auto_spots`：

1. `自动候选挂到包住它的最小手工容器下` —— 两层嵌套容器，节点落在内层，路径必须带
   内层前缀。自证变红：把「最小」改成「第一个命中的」。
2. `与手工插桩重合的自动候选被丢弃` —— 同一矩形手工已登记，结果里只剩一条。
3. `同容器同名控件被编号` —— 三个「删除」按钮 → `[2] [3]`。
4. `没有标签的控件用角色名兜底` —— label 为 `None` 时路径不能出现空的「」。
5. `退化矩形不进候选` —— `Rect::NOTHING` 与零宽。

其他：

6. `默认档是详细` —— `Detail::default() == Detail::Full`。自证变红：改回 `Compact`。
7. `退出标注模式清空自动候选` —— 否则下次进来第一帧带着上次界面的候选，鬼影。
8. kittest 端到端：画一个 `ui.button("保存")` 的最小 UI，开标注模式跑两帧，断言
   `spot_paths` 里出现「按钮「保存」」，且能 `ensure_picked` 中。这一条同时钉住
   「enable → 树 → 归约 → 路径」整条链，是唯一能证明 accesskit 那半边真的通了的测试。

守护测试跑法：`cargo test -p mullion-app annotate`，交付前仍按项目定义跑全绿
（`cargo test --workspace` + `clippy -D warnings` + `fmt --check`）。

---

## 5. 交付

- `crates/mullion-app/Cargo.toml`：正式依赖 `egui` / `egui-winit` 开 `accesskit`；
  dev-deps 段里那段「正式构建不含 accesskit」的注释**必须改**——它将不再成立，留着
  就是下一次踩坑的陷阱。
- 版本 bump patch + 交叉编译 + objdump 验收 + GitHub Release，按 `CLAUDE.md` 的
  交付约定一条龙。
- 已验证：`cargo check` 与 `cargo build --release`（`x86_64-pc-windows-gnu`）均过；
  exe 新增 `uiautomationcore.dll` / `propsys.dll` 两个依赖，都是 Windows 自带的系统
  DLL，`libgcc_s_seh-1.dll` / `libwinpthread-1.dll` 未出现，符合验收红线。

## 6. 人工验收清单（无头验不了）

1. `Ctrl+Shift+F` 后，提示条显示档位「详细」。
2. 悬停能描边并选中：菜单栏各按钮、设置弹窗里的输入框 / 复选框 / 下拉框 / 滑块、
   会话管理器表单里的各字段、文件面板的行与列头。
3. 终端 pane 区域、pane 标题条、分隔条能选中（D12 补的手工插桩）。
4. `Ctrl+Shift+E` 导出后粘贴，检查自动候选那些行的路径是否读得懂、容器前缀是否对。
5. 退出标注模式后：终端里 `Esc`、打字、鼠标划选一切照旧；CPU 占用回到平时水平
   （D10 那条若写错，症状是标注模式退出后仍持续满帧重绘）。
6. 屏幕阅读器：我们不 init adapter，理论上系统无障碍栈感知不到本应用有变化——
   若你机器上开着 NVDA / 讲述人，顺手确认没有异常行为。
