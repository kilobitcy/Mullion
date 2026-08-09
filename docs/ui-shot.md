# 离屏 UI 截图 harness（`examples/ui_shot.rs`）

无头把 egui 外壳渲染成 PNG。**不是产品功能**，example target，不进 exe。

## 为什么有这东西

改 UI 的人在无头容器里是瞎的。在此之前，验证任何视觉改动的唯一路径是
「bump → 交叉编译 → 发 Release → 人肉眼验 → 反馈」，一轮几十分钟，而且
「按钮重叠了」这种当场能看出来的粗错也要走完整条链。

这个 harness 把那条链的第一段砍掉：改完立刻渲染一张图看一眼。

它**不替代**人工验收——见下面「这张图能信什么」。

## 用法

```bash
cargo run -p mullion-app --example ui_shot -- --list          # 列场景
cargo run -p mullion-app --example ui_shot -- list-12
cargo run -p mullion-app --example ui_shot -- list-12 --size 1024x700 --ppp 1.5
cargo run -p mullion-app --example ui_shot -- list-12 --out /tmp/a.png
```

默认输出 `target/ui-shot/<场景>.png`，默认 1280x800 @ ppp 1.0。

场景清单在 `examples/ui_shot.rs` 顶部的 `SCENES`，数据工厂在同文件 `fixture()`。
走查时发现新的可疑状态就往里加一条——加场景比每次手写临时代码便宜，而且
「我看到的那一帧」你能用同一条命令复现。

## 这张图能信什么、不能信什么

| | |
|---|---|
| ✅ 能信 | 布局、层级、控件位置与尺寸、是否溢出/重叠/截断、文字换行 |
| ❌ 不能信 | 与 Windows 的**像素级**一致——驱动的抗锯齿与浮点舍入不同 |
| ⚠️ 有条件 | 中文的宽度/换行/截断，**只在装到 `msyh.ttc` 时可信**（见下） |

所以这里**不做像素基线快照**：基线只对本机驱动 + 本机字体版本有效，换环境全红，
维护成本远超收益。图的用途是「给人（或我）看一眼」，不是 diff 门禁。

「是否不闪 / 手感 / 输入法」这些仍然只有人眼能判，见 `CLAUDE.md` 的
「你无法验证的东西」。

## 环境依赖

### 渲染后端：lavapipe 软件 Vulkan

harness 显式把 `DeviceType::Cpu` 的适配器排在前面。理由：真 GPU（本机 RTX 4060）
要 `/dev/dri/renderD128` 权限，而它属 `render` 组，当前用户不在组里，
`request_device` 会失败。软渲染一帧几百毫秒，对「出一张图」完全够。

因此**不要**用 `TestRenderer::new()`——它取 `enumerate_adapters` 的第一个，不可控。

枚举不到 Vulkan 适配器时装 `mesa-vulkan-drivers`，或显式
`VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json`。

### 中文字体：必须是微软雅黑

**判断「挤不挤 / 会不会截断」完全取决于字体度量。** 用 Noto 之类的替身渲染，
中文宽度与实机不同，图会系统性地骗人。

产品的 `ui::install_cjk_font` 只查 `C:\Windows\Fonts`，在 Linux 上直接 return，
所以 harness 自己装一份同构的：

```
~/.local/share/mullion-dev-fonts/msyh.ttc      # 默认路径
MULLION_UI_SHOT_FONT=/path/to/msyh.ttc         # 或显式指定
```

从 Windows 拷来即可（**不入库、不推送**）。没装到时程序会在输出里显式警告，
那种图上**不要**对中文文本下任何结论。

## 坑：必须调 `theme::apply_egui`，否则图在骗人

`session_manager` 里**手绘**的部分（会话行、状态点、色条）自带主题色，看着像对的；
但 egui 自己的部件——按钮、`TextEdit`、`CollapsingHeader` 的三角、**滚动条**——
全部走全局 `Style`。不调 `apply_egui` 就是 egui 出厂的默认深色，跟实机不是一套。

harness 第一版漏了这一句，代价是：滚动条按 egui 默认样式画（静止态 alpha = 0），
图上完全看不见，我据此报了一个「列表溢出且没有滚动条」的错误结论——实际滚动一直
是好的，只是那条滚动条在静止时被画成了全透明。

**任何新增的 harness 场景都要走同一条初始化路径**（`apply_egui` + 中文字体），
顺序是「建 `Harness` → `apply_egui` → 装字体 → `run()`」：后两者都只影响之后的帧。

## 坑：`egui` 与 `egui-winit` 的 `accesskit` feature 必须同步

`egui_kittest` 给 `egui` 开 `accesskit`，于是 `egui::PlatformOutput` 多出
`accesskit_update` 字段；而 `egui-winit` 里对该结构的**穷尽解构**由它自己的同名
feature 控制，不开就少一个字段，`E0027` 整片编不过。

解法（已在 `crates/mullion-app/Cargo.toml`）：把 `egui-winit = { features =
["accesskit"] }` 也写进 `[dev-dependencies]`。靠 workspace 的 `resolver = "2"`，
dev-deps 的 feature 不统一到正式构建——`cargo test` / `--all-targets` 下两者都带
accesskit（编得过），`cargo build --release` 不含 dev-deps，正式 exe 不白搭一个
`accesskit_winit` 进去。

**动 egui 版本时先跑一遍 `cargo build --example ui_shot`**，这对 feature 是最先炸的地方。

## 已知限制

- **只覆盖 egui 外壳**（菜单/状态栏/会话弹窗）。glyphon 自绘的终端网格不在内——
  它没有 widget，要抓字形/CJK 对齐问题得走另一条路（VT 快照或人眼）。
- **没有「窄密度档」场景**。左栏三档密度由 `SidePanel` 宽度决定（`list::density_for`，
  阈值 208 / 132），而宽度是用户拖出来的、存在 egui memory 里。要在 harness 里
  触发窄档得预置 `PanelState`，还没做。
- 场景是**静态一帧**。交互后的状态（悬停、拖拽中、下拉展开）需要用
  `egui_kittest` 的事件 API 先驱动再渲染，当前场景都没用到。
