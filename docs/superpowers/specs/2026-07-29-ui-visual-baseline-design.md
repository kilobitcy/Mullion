# 视觉基线与外壳规格（F80–F85）

> 日期：2026-07-29
> 来源：Mullion 桌面外壳 mockup（1440×900，DCLogic 组件）
> 状态：F80/F81 本轮实现；F82/F83/F84 与 SFTP 侧栏冻结规格待后续切片；F85 已否决

## 1. 背景

mockup 给出了一套完整的深色桌面外壳设计。它的菜单项「对话/分屏/配置/关于」与
`ui/chrome.rs` 一字不差 —— 是照着现有代码画的，不是凭空设计，因此其信息架构与本项目
既有结构天然对齐。

但 mockup 是网页 demo，有三处与本项目的架构/场景冲突，**不照抄**：

1. 分屏用固定 `2/3/4 屏 + 子布局` 枚举；本项目 F30/F32 要的是任意嵌套布局树 +
   拖分隔条 resize，`mullion-core` 就是按后者写的。**预设只能是「一键套用一棵树」的
   快捷入口，不得反过来成为模型。**
2. 终端 `line-height: 1.65`。那是给截图看的，终端实用值 1.1~1.25
   （Windows Terminal 为 1.2）。1.65 在 80×24 里白扔约 10 行。
3. 图标全部用 emoji（📁📂📄⇄⚙）。egui 内嵌字体无 emoji；Windows 的 Segoe UI Emoji 是
   COLRv1 彩色字体，cosmic-text 渲染彩色字形是已知坑。改用 egui `Painter` 自绘几何
   图形，或用 `▥ ▦ ▤ ▸ ▾` 这类 CJK 字体必然覆盖的几何符号（`install_cjk_font` 已挂
   微软雅黑，安全）。

---

## 2. 色板 token 全表

后续切片直接查此表，不要重新调色。

### 2.1 结构色

| token | 值 | 用处 |
|---|---|---|
| `window_bg` | `#12141c` | 窗口底 |
| `bar_title` | `#1e2028` | 标题栏（F85 否决后暂无用，保留） |
| `bar_menu` | `#181a22` | 菜单栏 |
| `bar_tool` | `#151822` | 工具栏（F82） |
| `bar_status` | `#181b26` | 状态栏 |
| `panel_bg` | `#14161f` | pane 底 / SFTP 侧栏底 |
| `panel_head` | `#191c27` | pane 标题条（F83） |
| `sunken_bg` | `#0e1018` | 凹槽：分段控件底、快捷键徽标、滑轨 |
| `stroke` | `rgba(255,255,255,0.06)` | 分隔线、描边（0.05~0.08 区间取中值） |

### 2.2 前景灰阶

| token | 值 | 用处 |
|---|---|---|
| `fg` | `#e4e6f0` | 主文本、光标块 |
| `fg_strong` | `#d3d6ea` | pane 名、文件名 |
| `fg_mid` | `#c7cae0` | 标题、选中项 |
| `fg_muted` | `#a9aec2` | 菜单项常态 |
| `fg_dim` | `#9aa0b8` | 次级按钮、树节点 |
| `fg_dimmer` | `#8a90a8` | 未选中分段、提示符 |
| `fg_faint` | `#565b70` | IP、大小、时间、状态栏文字 |
| `fg_ghost` | `#4b5066` | 表头 |

### 2.3 语义色

| token | 值 | 用处 |
|---|---|---|
| `accent` | `#8b95ff` | 主按钮底、选中分段、进度、焦点边框 |
| `accent_fg` | `#0d0f16` | accent 底上的文字（深色反白） |
| `ok` | `#7fd99b` | 已连接、成功输出 |
| `warn` | `#e0b767` | 高负载、告警输出 |
| `info` | `#7c9eff` | 提示符、链接 |
| `danger` | `#e81123` | 关闭按钮 hover、错误（沿用 Windows 系统红） |

accent 备选（设置里可换）：`#8b95ff` / `#7fd99b` / `#e0b767` / `#7c9eff`。

### 2.4 终端色

| token | 值 | 说明 |
|---|---|---|
| `term_bg` | `#14161f` | 与 `panel_bg` 同值。**注意 §3.2 的三处同源约束** |
| `term_fg` | `#e4e6f0` | 替代 `palette::DEFAULT_FG` 的出厂值 `#cccccc` |

ANSI 16 色沿用 `mullion-term/src/palette.rs` 现值，本轮不动。

### 2.5 尺寸节奏

栏高：菜单 30px / 工具栏 48px / pane 标题条 32px / 状态栏 24px / SFTP 头 38px。
圆角：pill 6px、按钮 7px、控件组 8px、modal 12px。
字号：UI 12~12.5px，次级 11~11.5px，弱 10~10.5px，modal 标题 14px。
终端行高：**1.25**（代码 `text.rs:71` 的现值，落在合理区间 1.1~1.25 内；
不是 mockup 的 1.65）。改它会变动行数、踩 T4/F34，收益为零，本轮不动。
后续由 F21 做成可配。

---

## 3. 本轮实现

### 3.1 F80 视觉 token 统一

新增 `crates/mullion-app/src/theme.rs`（**不放 `ui/` 下** —— 终端渲染层也要用它，
不只是 egui 外壳）：

```rust
pub struct Theme { /* §2 的全部 token */ }
pub const MULLION_DARK: Theme = ...;

pub fn apply_egui(ctx: &egui::Context, t: &Theme);   // 写 egui::Visuals
pub fn clear_color(t: &Theme) -> wgpu::Color;        // 与 t.term_bg 同源
```

依赖方向不变：`theme.rs` 属于 `mullion-app`（该 crate 本就依赖 egui/wgpu），跨 crate
方向上只向下用到 `mullion-term::snapshot::Rgb`。**不得**把 `Theme` 或任何 egui/wgpu
类型漏进 `mullion-term`。

### 3.2 三处同源约束（本设计最重要的一条）

改终端背景色必须同时改三处，否则整屏底色错乱：

```
app.rs:1293  LoadOp::Clear(黑)      ← wgpu 清屏色
gpu.rs:40    cell.bg == default_bg  ← 相等就 continue，不画背景 quad
palette.rs:34 DEFAULT_BG = #000000  ← snapshot 里默认背景格解析成的值
```

`quads_for` 对「背景 == 默认背景」的格子**跳过不画 quad**，让 clear 色直接透出来
（这是有意的性能优化，`gpu.rs::tests::default_bg_cell_makes_no_quad` 守着）。
所以 clear 色一旦 ≠ 默认背景色，满屏空白格显示的是 clear 色而非主题色。

**解法**：三处全部改为从 `Theme` 单一来源取值（出口见 §6 的 `term_default_colors`）。

- `app.rs` 的 clear：改用 `theme::clear_color(&theme)`。
- `gpu.rs::quads_for` 的 `default_bg` 参数：调用方传 `term_default_colors(&theme).1`
  （签名已经是参数，无需改动函数；`app.rs:1227` 现在传的是 `palette::DEFAULT_BG`）。
- `palette.rs`：见下面「注入链路」—— 不是加个字段就够。

**注入链路（复核补：spec 初稿低估了这一层）**

const 不是在 `snapshot()` 里被读的，而是在两跳之外：

```
emulator.rs:158   palette::resolve(cell.fg, colors)
palette.rs:81       → named_default(named)
palette.rs:65-67      → DEFAULT_FG / DEFAULT_BG      ← const 在这里被吃掉
```

`resolve` 是 pub 纯函数，`named_default` 是私有自由函数。`Emulator` 光存字段传不进去，
必须改 `resolve` 的签名：

```rust
#[derive(Clone, Copy)]
pub struct DefaultColors { pub fg: Rgb, pub bg: Rgb }   // Default = 现有出厂值

pub fn resolve(color: AnsiColor, colors: &Colors, d: DefaultColors) -> Rgb;
fn named_default(named: NamedColor, d: DefaultColors) -> Rgb;
```

`DEFAULT_FG/BG` 两个 const 保留为出厂值（`DefaultColors::default()` 的来源）。
`Emulator` 持 `defaults: DefaultColors` + `set_default_colors(fg, bg)`，`snapshot()`
传给 `resolve`。

**连带改动**：`palette.rs` 现有 6 个测试全打在 `resolve` 上，签名一改都要补第三个实参
（机械改动，传 `DefaultColors::default()` 即可，断言不变）。

**计划期追加：实为五处，且有一个色彩空间前置问题**

除上面三处，另有两处 `0xcc` 字面量硬编码，**不引用** `palette::DEFAULT_FG`，
grep 常量名找不到：`gpu.rs` 的光标色块、`text.rs` 的 glyphon `default_color`。

更要紧的是：surface 是 sRGB 格式（`gpu.rs` 用 `is_srgb()` 挑的），
着色器输出会被硬件当**线性**值再编码。egui（`egui.wgsl` 的
`linear_from_gamma_rgb`）与 glyphon（`shader.wgsl` 的 `srgb_to_linear`）都做了
转换，**只有我们的 quad 着色器原样透传**。底色是纯黑时看不出（0 在两个空间都是
0），底色一旦非黑，同一个 token 在外壳和终端里就是两个颜色 —— F80 的人工验收
第一条「没有两个世界的割裂感」会直接失败。

所以 `clear_color` 必须返回**线性**值，且 `QUAD_WGSL` 必须补 `srgb_to_linear`。
修完后现存 ANSI 彩色格会变暗（回到正确值），这是修正不是回归。

### 3.3 为什么默认色注入放在 Emulator 而不是 app 层替换

三个备选：

| 方案 | 判定 |
|---|---|
| (a) 写 alacritty `Colors` 的 256/257 槽（与 OSC 10/11 同路径） | 否 —— 需要 `colors_mut()` 一类 API，`alacritty_terminal` 属 API 漂移风险清单 |
| (b) **Emulator 持有可注入的默认 fg/bg，随调用传进 `palette::resolve`** | **采纳** |
| (c) app 拿到 snapshot 后遍历替换颜色 | 否 —— 每帧 O(cells) 额外开销，且同一映射做两遍 |

(b) 不违反架构不变量：「默认前景/背景色」本就是 VT 协议概念（SGR 39/49、OSC 10/11），
`mullion-term` 早已持有 `DEFAULT_FG/BG` 常量，此处只是从硬编码变为可注入，注入的类型
是 term 自己的 `Rgb`，**没有任何 UI 类型漏进 term**。将来实现 OSC 10/11 时，
`resolve()` 里「`colors[named]` 有值优先」的既有短路仍然生效，两者不冲突。

**副作用（复核补）**：`named_default` 的兜底分支是 `_ => DEFAULT_FG`，即 `Cursor` /
`Bright*` / `Dim*` 等未单列的具名色全部落到默认前景上（`palette.rs:67`，MVP 的有意简化）。
注入后它们会跟着变成 `theme.term_fg` —— 本轮正好是想要的效果（光标与文本同色系），
但这条耦合必须写明：**将来想单独调光标色，源头在这个兜底分支，不在 `Theme`。**

### 3.4 F81 状态栏信息架构

`chrome.rs::status_bar` 现在是一个裸 label。改为左右两栏：

- 左：`● {N} 屏布局 · {连接态}` —— 色点用 `ok`/`warn`/`fg_faint`
- 右：`{编码}` —— 即 `UTF-8`
- `last_error` 保持现状（红色，右侧，优先级最高），它的兜底职责不能动
  （见 `chrome.rs:40-43` 的注释）

格式化抽成纯函数 `chrome::status_text(...) -> (String, String)` 以便单测。

**本轮不做「远端 SSH 版本」**（复核后砍掉）。核过 russh 0.54.5：
`client::Session::remote_sshid() -> &[u8]` 确实存在（`client/session.rs:534`），但它挂在
`Session` 上，**只有带 `session: &mut Session` 参数的 Handler 回调够得着** —— F3 用的
`check_server_key` 签名里没有 `session`。真实工作量是「在 `data` / `channel_open_confirmation`
一类回调里首次捕获 → 塞进已有事件通道 → 送到 app」，属跨 crate 接线，会把本轮从纯视觉
改动变成带 SSH 侧改动。要做时按上述路径单开一条，别混进 F81。

### 3.5 顺带：占位菜单项接编号

`chrome.rs:25,28` 的 `"(切片 B 实现)"` / `"(切片 C:字体等)"` 改为带 spec 编号的占位
（`"(F30 分屏 · 后续切片)"` / `"(F84 设置 · 后续切片)"`）。零成本的欠账可见性 —— 每次
启动都在眼前，比任何 todo 列表都难丢。

菜单「对话」改名「会话」：它装的是会话管理器/断开/退出，与 `CLAUDE.md`「边界」一节
点名不做的「AI 侧栏」无关，现名是历史遗留。

### 3.6 本轮不做

不动渲染管线结构、不动输入路由（T8）、不碰分屏、不新增依赖、不改 `session_manager` /
`host_key` / `paste` 三个弹窗的**结构**（它们只被动继承新 `Visuals`）。

---

## 4. 冻结规格（暂不实现）

### 4.1 F82 工具栏（随分屏切片）

48px 高，底 `bar_tool`，下边 `stroke`。左起：

1. `+ 新建连接` —— accent 实心，`accent_fg` 文字，12.5px/600，padding 7×12，圆角 7px
2. 1px×20px 竖分隔线
3. **分屏操作**分段控件组（底 `sunken_bg`，padding 3，圆角 8px，描边 `stroke`）：
   `垂直分屏` / `水平分屏` / `关闭 pane`
4. **布局预设**分段控件组：`左右` / `上下` / `三等分` / `四宫格`

右起：`SFTP` 开关（激活时 accent 实心）、`⚙` 设置。

**语义约束**：第 4 组的每个预设 = 一次性套用一棵 `mullion-core` 布局树，套用后仍可用
F32 拖分隔条自由调整、用第 3 组任意嵌套。预设不是状态，不得存在「当前处于 3 屏模式」
这种字段。

### 4.2 F83 pane 标题条（随分屏切片）

32px 高，底 `panel_head`，下边 `stroke`，padding 0 10px。

左：7px 圆点（`ok`/`warn`）+ 主机名（12px/600，`fg_strong`，溢出省略）+ IP（11px，
`fg_faint`）。右：状态徽标（10.5px/600，`● 已连接` / `● 高负载`）。

**必须可关**（设置里开关，默认开）。目标场景是高延迟链路里看远端 tmux，tmux 自带
status line，再叠 32px 是双份浪费垂直空间。

关闭时这 32px 归还终端网格，`shell::viewport::grid_dims` 的行数随之变化 —— 属于 T4/F34
地雷区，改动必须跑 `app::tests::reflow_emits_resize`。

### 4.3 SFTP 侧栏（F50，切片 D）

宽 380px，底 `panel_bg`，左边 `stroke`。

- 头 38px：`SFTP — {标题}`（12px/600，`fg_mid`）+ 右侧 `{user}@{host}`（10.5px，`fg_faint`）
- 主体左右分：
  - 左 150px 目录树：折叠符 `▾`/`▸`（9px，`#5c6178`，占位宽 10px），缩进
    `8 + depth*14` px，行 padding 4×6，圆角 5px，选中底 `rgba(139,149,255,0.14)`
  - 右列表：面包屑路径（10.5px，`fg_faint`，溢出省略）+ 三列
    `grid-template-columns: 1fr 56px 90px`（名称 / 大小 / 修改时间），表头 10px/600
    `fg_ghost`，行 12px padding 5×12

### 4.4 F84 设置弹窗（切片 C）

420px 宽，底 `bar_status`，描边 `stroke`，圆角 12px，padding 20×22，遮罩
`rgba(6,7,12,0.6)`。三段（段标题 11px/600 `#7a8095`，段间距 18px）：

1. **终端主题** —— 64×40 色板，圆角 8px，选中描边 accent。**本期只有 Mullion
   `#14161f` 一个**（复核后缩减）。mockup 原本给了 One Dark / Dracula / Nord，但 §2.4
   已定「ANSI 16 色本轮不动」—— 只换背景不换 ANSI 16 色，切到 Dracula 出来的是四不像。
   **前置条件**：ANSI 16 色可配之后再补其余主题，届时一个主题 = 一整套 16 色 + fg/bg。
2. **字体大小** —— 滑块 12px~18px，轨 4px 高底 `sunken_bg`，已填充段 accent，
   把手 13px 圆 `fg`。承载 F21
3. **快捷键** —— 行式列表，动作名 12px `fg_mid`，键位徽标 11px `fg_dimmer`，
   底 `sunken_bg`，padding 3×8，圆角 5px，描边 `stroke`

---

## 5. 已否决

### F85 自绘标题栏 + Windows 三键

winit `with_decorations(false)` 之后，Windows 上必须自接 `WM_NCHITTEST` 才能保住：
resize 边框命中区、双击标题栏最大化、Aero Snap（Win+方向）、Win11 Snap Layouts
（悬停最大化按钮弹出的布局菜单）。用这些系统集成风险换 32px 的视觉统一，在功能欠账
（F30 分屏、F50 SFTP）未清前不划算。

若将来重提，需先给出 `WM_NCHITTEST` 的可测方案。规格保留在 §2.1 的 `bar_title` token。

---

## 6. 测试策略

§3.2 的三处同源里，前两处（clear 色、`quads_for` 的 `default_bg`）是编译期常量，好守；
第三处（`Emulator` 的注入值）发生在 app 运行时接线，**恰恰是最容易漏改的那处**。
解法是收敛出**单一出口**：

```rust
pub fn term_default_colors(t: &Theme) -> (Rgb, Rgb);   // (fg, bg)
pub fn clear_color(t: &Theme) -> wgpu::Color;          // 内部走 term_default_colors().1
```

app 层三处全从它取，**禁止再直接引用 `palette::DEFAULT_FG/BG`**（`app.rs:1227` 现在就是
直接引用的，本轮一并改掉）。

| 测试 | 守什么 |
|---|---|
| `theme::tests::clear_color_matches_term_bg` | clear 色 ↔ `term_bg`，改一处忘另一处直接红 |
| `theme::tests::term_defaults_match_theme` | `term_default_colors` ↔ `term_fg`/`term_bg` |
| `app::tests::terminal_defaults_come_from_theme` | 第三处：app 接线时注入的确实是主题色，不是 `palette` 出厂常量 |
| `gpu::tests::default_bg_cell_makes_no_quad`（改造） | 断言在**非黑**主题背景下同样成立 |
| `emulator::tests::injected_default_colors_reach_snapshot` | §3.3 的注入真的穿透 `resolve` 到达 snapshot |
| `chrome::tests::status_text_*` | F81 各状态下的格式化输出 |
| 回归必跑 | `app::tests::reflow_emits_resize`(T4)、`gpu::tests::origin_shifts_every_quad_so_first_row_clears_the_menu_bar`、`input_route::tests::*`(T8) |

**人工验收**（无头环境验不了，进 Release notes）：

- 外壳与终端是否是同一个色系，没有「两个世界」的割裂感
- 深色下文字对比度是否够（`fg_faint` `#565b70` 在 `bar_status` `#181b26` 上）
- 终端底色改成 `#14161f` 后，全屏 TUI 是否仍不闪（T2/N3 红线）
- CJK 字符在新前景色下的清晰度
