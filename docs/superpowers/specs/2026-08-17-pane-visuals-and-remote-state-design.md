# 分屏视觉与远端状态 —— 设计

> 一次七项用户报告的合并切片：终端长串重叠、SFTP 继承终端目录、分屏分界线、
> 焦点提示、标题条图标截断、标题条加目录名/tmux 名、隐藏标签栏。
> 涉及 spec 编号：F80(视觉基线) / F83(pane 标题条) / F50+F120(文件面板与起始目录) /
> F36(标签栏) / F21(字体) / F30(分屏)。

## 背景

用户在 Windows 11 实机(v0.1.48)上报了七项，其中两项已在本次调查中定位到确切根因，
两项(分界线/焦点)证实为**当前完全缺失**的功能，三项是新增。

七项之间有两组耦合，所以合成一个切片而不是七个独立改动：

- ⑤(图标截断) 与 ⑥(标题条加内容) 落在同一个 32px 高的条上，⑤ 的根治手段
  (标题条高度随 DPI)正是 ⑥ 的前提；
- ②(SFTP 起始目录) 与 ⑥(标题条显示目录名) 需要同一份「远端当前目录」数据。

## 已坐实的根因

### ① 长串与后续文字重叠(截图 `/tmp/1.png`)

`crates/mullion-app/src/text.rs` 里字号写了两遍，且两遍不等：

| 位置 | 字号 |
|---|---|
| `TextLayer::new` → `measure_cell_w(fs, font_px, line_h, family)` | `font_px` |
| `prepare_panes` → `Metrics::new(self.cell_h * 0.8, self.cell_h)` | `cell_h * 0.8` |

而 `cell_h = (font_px * 1.25).ceil()`，那个 `ceil` 让 `cell_h * 0.8 >= font_px`，
只在恰好整除时相等。于是**渲染用的字形比量出来的 `cell_w` 宽**。

ASCII 连续段是一个 `RowRun`、交给 cosmic-text 自由排版(见 `row_to_runs` 文档)，
误差在 run 内部逐字累加。临时探针实测 60 个 `M` 的漂移：

```
pt=10 scale=1.00  cell_w=11.504  漂移 1.20 格   ← 默认档，必现
pt=10 scale=1.25  cell_w=14.380  漂移 0.48 格
pt=10 scale=1.50  cell_w=17.256  漂移 0.00 格   ← 恰好整除，不复现
pt=11 scale=1.00  cell_w=12.654  漂移 2.18 格
pt=13 scale=1.25  cell_w=18.694  漂移 2.03 格
```

截图里那条 55 字符的 `docs/superpowers/specs/2026-08-17-annotate-auto-spots-design.md`
是一个 ASCII run(URL 是青色，与后文不同色，但同色分段在 run *内部*)，尾部的 `d`
漂过一格，压在下一个 run(全角 `（`，CJK 自成一 run、按 `col × cell_w` 定位)头上。

**这不是 CJK 宽度问题**，`row_to_runs` 那套逐格定位是对的；错的是 run 内部的字号。

### ⑤ 标题条图标底部被截

`TITLE_BAR_PX = 32` 是**物理**像素(`geom.rs` 层的语义，见 `toolbar.rs` 顶部那段
「px 有两种语义」的说明)。`pane_title.rs` 把它 `/ ppp` 换成逻辑点后：

| ppp | 标题条逻辑高 | `inner = full.shrink2((8,4))` 高 | 图标要求 |
|---|---|---|---|
| 1.00 | 32.0 | 24.0 | 14.0 ✓ |
| 1.25 | 25.6 | 17.6 | 14.0 ✓ |
| 1.50 | 21.3 | **13.3** | 14.0 ✗ 差 0.7 点 |

差的那 0.7 点被 `content.set_clip_rect(inner)` 从底部裁掉。**DPI 越高标题条越矮**，
这与菜单栏/工具栏(逻辑点、随缩放放大)的行为相反，是真 bug 而不是图标画大了。

### ③④ 分界线与焦点提示当前为零

- `GAP_PX = 1` 只是让 `term_px` 各让 1px，**没有任何代码往那道缝里画东西**；
  缝里露出的是 wgpu 清屏色，而清屏色与 `t.term_bg` 同源(`theme.rs::clear_color`)
  —— 所以分界线在视觉上不存在。
- 焦点唯一的体现是标题条底色 `panel_head`(`#191c27`) vs `panel_bg`(`#14161f`)
  和文字 `fg_strong` vs `fg_muted`。前者两色在深色屏上几乎无法分辨；
  且 F83 关掉标题条后**焦点提示完全消失**。

## 已定死的决策

| # | 决策 | 否掉的备选与理由 |
|---|---|---|
| D1 | 远端状态只从 **pane 自己的字节流** 采集(OSC) | 旁路 exec 通道行不通：adr-009 一条 TCP 连接承载所有 pane，`$SSH_CONNECTION` 四元组相同、无法区分是哪块分屏；`channel.set_env` 受 sshd `AcceptEnv` 限制(默认只放 `LANG`/`LC_*`)。刮 prompt 文本太脆，拒。 |
| D2 | cwd 双来源：**OSC 7 优先，OSC 0/2 标题兜底** | 只用标题的话裸 shell 下只能拿到 bash `\w`(带 `~` 缩写要展开)且漏掉不设标题的 shell；只用 OSC 7 的话 tmux 会把它吃掉(tmux 用它维护 `pane_current_path`)。 |
| D3 | tmux 里需要远端加 `set -g set-titles on`，**拿不到就不显示** | 备选是在登录后自动化里往 PTY 注入 shell 集成命令 —— 会在用户会话里留可见的一行，且 tmux 内 shell 由 tmux 起、注入时机不确定。 |
| D4 | 分界线 = **1px 静态细线**，画在 `GAP_PX` 已经让出的那 1px 里 | 「随焦点变亮」会让边界跟着焦点跳动，扫视时有闪烁感；「加宽到 2px + 拖拽热区」超出本次范围且再吃 1px 终端。 |
| D5 | 焦点 = **标题条 accent tint + pane 边界 1px accent 描边**(两层冗余) | 只加强标题条的话 F83 关掉后没有任何提示；「非焦点 pane 整体压暗」会改变终端本身的颜色观感，而颜色正确是本项目的存在理由之一。 |
| D6 | 描边**压住终端最外圈 1px 字形边缘**，不给焦点 pane 缩终端区 | 缩 1px 会改 `term_px` → 改 `grid` → 每次切焦点都发 `window_change`(T4)，远端 TUI 每次点击都重排。 |
| D7 | 分界线与描边走 `ctx.layer_painter(Order::Background)` | 不 allocate `Area` 就不吃指针事件(T8)；`Order::Background` 在 wgpu 终端之上、在 egui 面板/弹窗之下，不会盖住模态框。 |
| D8 | `TITLE_BAR_PX`(物理 32) → `TITLE_BAR_PT`(逻辑 32) + `title_bar_px(ppp)` | 「只把图标尺寸改成从 `inner.height()` 推」是把症状按下去：150% 下标题条仍只有 21 逻辑点，⑥ 再加两段文字就挤爆。 |
| D9 | 标题条**左右分区**，两区各自 `truncate()` | 单行拼接时从右截断，最先消失的恰好是新加的目录名和 tmux 名；「目录名优先、主机名让位」与既有习惯反着来，多节点场景反而看不到主机名。 |
| D10 | SFTP 起始目录**只在面板从关变开的那一帧**取一次 | 持续跟随焦点 pane 的 cwd 会把用户在面板里的浏览位置反复拽回去。 |
| D11 | 标签栏用**源码常量**开关 | 设置项要动 store schema 和设置表单；「只有 ≥2 个标签才显示」与 spec F36 验收「只剩一个标签也画标签栏」直接冲突，而且显隐是高度跳变、会触发整幅 reflow(T4)。 |

## 各项设计

### ① 字号同源

`text.rs` 抽一个函数，量宽和排版都只经它：

```rust
/// 网格排版用的 metrics。**量 `cell_w` 与排版必须用同一个字号** —— 两处各写
/// 一遍的话，`cell_h = ceil(font_px * 1.25)` 里那个 `ceil` 会让排版字号比量宽
/// 时大一点，一个 ASCII run 内部逐字累加，长路径尾部就压到下一个 run 头上。
fn grid_metrics(font_px: f32, cell_h: f32) -> Metrics {
    Metrics::new(font_px, cell_h)
}
```

- `measure_advance` 用它替 `Metrics::new(font_px, line_h)`；
- `prepare_panes` 用它替 `Metrics::new(self.cell_h * 0.8, self.cell_h)`。

视觉影响：字形从 `cell_h * 0.8` 缩到 `font_px`，最多小 0.8 物理像素，落在
「用户设的 pt 就是实际渲染的 pt」这个正确的一侧。

**守护测试(先红)**：遍历 pt ∈ {10, 11, 13} × scale ∈ {1.0, 1.25, 1.5}，
断言 60 个 `M` 的总推进与 `60 × cell_w` 之差 < 0.5px。用 `M` 是为了不依赖
容器里装了哪款字体(`measure_cell_w` 本来就是拿 `M` 量的)。
自证会变红：把 `prepare_panes` 那处改回 `cell_h * 0.8`。

### ② SFTP 继承终端当前目录

纯函数(放 `crates/mullion-app/src/files/state.rs` 或新 `files/start_dir.rs`)：

```rust
/// 文件面板远端栏该开在哪。优先级：焦点 pane 的 cwd > F120 配置的默认远端目录
/// > `None`(交给 `SftpClient::open` 的 `canonicalize(".")` 落回登录目录)。
pub fn files_start_dir(pane_cwd: Option<&RemotePath>, default_remote: Option<&str>)
    -> Option<RemotePath>
```

接线(`app.rs`)：`files_hotkey_event` 与菜单项那条「文件面板」共用一个
`open_files_sidebar()`，在 `false → true` 的跃迁上：

- `tab.content.sftp_client().is_none()` → 把 `files_start_dir(..)` 交给
  `spawn_sftp_open`(替今天直接传 `sftp_default_remote` 的那一步)；
- 已经有 client → 走一次普通的 `list_dir` 导航到该目录(与用户手点目录同一条路径)。

拿不到 cwd 时行为与今天逐字节相同，不报错、不提示。

**守护测试**：
- `files_start_dir` 的三档优先级(pane cwd / 默认远端 / 都没有)；
- 「面板已经开着时不重新导航」—— 直接测 `open_files_sidebar` 只在跃迁上动作
  (自证会变红：把跃迁判断删掉、改成每帧都算)。

### ③ 分界线 + ④ 焦点描边(新 `crates/mullion-app/src/ui/pane_edges.rs`)

纯几何、可脱 GPU/egui 单测：

```rust
/// 相邻 pane 之间那道 1px 缝的位置。`GAP_PX` 已经在 `layout_geometry` 里让出来了，
/// 这里只负责说出「线该画在哪」——**不动 geom.rs，不吃终端格**。
pub fn divider_lines(geoms: &[PaneGeom], area: PxRect) -> Vec<PxRect>;

/// 焦点 pane 的描边矩形(`px` 边界，含标题条)。
pub fn focus_ring(geoms: &[PaneGeom], focus: PaneId) -> Option<PxRect>;
```

`paint(ctx, t, geoms, area, focus)` 用 `ctx.layer_painter(LayerId::new(
egui::Order::Background, Id::new("pane_edges")))`，两个矩形都 `/ ppp` 换成逻辑点。

颜色：`Theme` 新增一个 token `divider: Rgb::new(0x28, 0x2b, 0x38)`
(≈ `term_bg` 上叠 8% 白，色相跟着调色板偏冷)。**不复用 `theme::stroke`**：
它是白 6%(`stroke_alpha = 15`)，1px 线在深色终端上基本看不见，而 ③ 的要求是
「不能忽略」。这是 F80 冻结色板之外的新 token —— 冻结的那张表里没有分界线，
因为分界线以前根本没画。描边取 `t.accent`(`#8b95ff`)，1 逻辑点宽。

标题条那一层(`pane_title.rs`)：底色统一按 `t.panel_bg` 画，焦点时**再叠一层**
`c32(t.accent).gamma_multiply(0.14)`(`gamma_multiply` 把不透明色变成半透明，
叠出来才是 tint 而不是纯 accent 块)，文字仍 `fg_strong`。非焦点保持
`panel_bg` + `fg_muted`(即今天 `panel_head` 那一档不再用在这里)。

**守护测试**：
- 单 pane 时 `divider_lines` 为空(不画一条贴着窗口边的线)；
- 左右分屏时恰好一条竖线，且它落在**两块 pane 的 `term_px` 都没占用**的那 1px 上
  (自证会变红：把线的 x 挪进任一块 `term_px`)；
- 上下分屏同理出一条横线；
- `focus_ring` 跟着 `focus` 走、focus 指向不存在的 pane 时返回 `None`；
- F83 关掉标题条(`title_px.h == 0`)时 `focus_ring` **仍然**返回矩形
  (自证会变红：给 `focus_ring` 加「标题条关了就不画」的短路)。

### ⑤ 标题条高度随 DPI(`shell/workspace/geom.rs`)

```rust
/// pane 标题条高度(F83)。**逻辑点**——与菜单栏/工具栏同一套缩放语义。
pub const TITLE_BAR_PT: f32 = 32.0;

/// 换成物理像素。`layout_geometry` 是唯一的调用点。
pub fn title_bar_px(ppp: f32) -> u32;
```

`layout_geometry(tree, area, cell, title_bars, ppp)` 多收一个 `ppp`；唯一调用点
`App::compute_geoms` 从 `a.window.scale_factor() as f32` 取(与 `font_px_for` 同一个源)。
`ppp` 非有限或 `<= 0` 时按 `1.0` 处理 —— 让 NaN 进去会污染整条几何链。

图标尺寸另外再加一道兜底：`inner.height().min(14.0)`。理由是 `title_bar_px`
在极小窗口下仍会被 `TITLE_BAR_PX.min(px.h)` 压矮(既有逻辑)，不能只靠标题条够高。

要跟着改期望值的既有测试：`geom.rs` 里用 `TITLE_BAR_PX` 的几条、
`pane_title.rs` 的 4 处、`ui/mod.rs` 的 3 处 —— 全部换成 `title_bar_px(ppp)`，
**不放松断言**。另加：
- `title_bar_px(1.0) == 32`(100% 下一字不变，这是「不回归」的锚点)；
- `title_bar_px(1.5) == 48`；
- `pane_title` 的图标在 ppp ∈ {1.0, 1.25, 1.5} 下都完整落在 `inner` 里
  (自证会变红：把图标尺寸改回硬编码 `14.0` 且把 `TITLE_BAR_PT` 改回物理语义)。

### ⑥ 远端状态采集与展示

**采集(`crates/mullion-term/src/remote_state.rs`，新文件)**

纯函数，零 IO：

```rust
pub struct RemoteState { pub cwd: Option<Vec<u8>>, pub tmux: Option<String> }

/// OSC 7 嗅探器。alacritty 0.26 不解析 OSC 7(`Handler` 里没有
/// `set_current_directory`)，所以我们在 `feed()` 里自己扫。OSC 可能跨 `feed()`
/// 切开，所以持有一小段未完成的前缀；上限 4KB，超了就丢弃这一条(恶意/损坏的
/// 流不该让我们无界增长)。
pub struct Osc7Sniffer { .. }
impl Osc7Sniffer {
    /// 喂字节，返回本批里最后一次看到的路径。
    pub fn feed(&mut self, bytes: &[u8]) -> Option<Vec<u8>>;
}

/// 解析 `file://host/path`(路径做百分号解码，字节语义，不假设 UTF-8)。
pub fn parse_osc7(payload: &[u8]) -> Option<Vec<u8>>;

/// 从 OSC 0/2 标题里抽 tmux 会话名和 cwd。
///
/// 规则(按顺序)：
/// 1. `^([^:\s]+):(\d+):` → tmux 会话名 = 组 1。tmux 默认
///    `set-titles-string` 就是 `#S:#I:#W`(如 `main:0:bash`)。要求第二段是纯
///    数字，否则 `user@host: ~/x` 会被误判成会话名 `user@host`。
/// 2. 标题里第一个以 `/`、`~/` 开头或恰为 `~` 的空白分隔 token → cwd。
pub fn parse_title(title: &str) -> RemoteState;
```

`emulator.rs`：照 `PtyWriteCollector` 的样子给 `EventListener` 再挂一个
`Arc<Mutex<Option<String>>>` 收 `Event::Title`；`Emulator::feed` 里同时喂
`Osc7Sniffer`。新增 `pub fn take_remote_state(&mut self) -> Option<RemoteState>`，
**OSC 7 给出的 cwd 覆盖标题给出的 cwd**，tmux 名只来自标题。

**存放**：`shell/workspace/mod.rs` 的 `PaneState` 加 `cwd: Option<RemotePath>` 与
`tmux: Option<String>`，在喂数据那条路径上顺手更新(与 `snapshot` 同一处)。
`RemotePath` 在 `mullion-ssh`，而 `mullion-term` 不能依赖它(依赖方向) ——
所以 `remote_state` 返回 `Vec<u8>`，转换在 app 侧做。

**展示(`pane_title.rs`)**：`TitleView` 加 `cwd_leaf: Option<&str>` 与
`tmux: Option<&str>`。右区在 `×`、`⇆` 之后(`right_to_left` 布局里更靠左)加一段
`fg_muted` 的 `Label::truncate()`，文本由纯函数给：

```rust
/// 标题条右区文字。两者都没有时返回 `None`(整段不画，不留一个孤零零的 `·`)。
pub fn side_text(cwd_leaf: Option<&str>, tmux: Option<&str>) -> Option<String>;
// (Some("Mullion"), Some("main")) → "Mullion · tmux:main"
// (Some("Mullion"), None)         → "Mullion"
// (None,            Some("main")) → "tmux:main"
// (None,            None)         → None
```

目录名取最后一级的纯函数：`~` → `~`，`/` → `/`，`/a/b/` → `b`，
`~/Mullion` → `Mullion`。非 UTF-8 字节走 `to_string_lossy`。

**守护测试**：`parse_title` 的四种输入(tmux 默认串 / Ubuntu bash `user@host: ~/dir` /
两者都有 / 什么都不是)、`parse_osc7` 的百分号解码与畸形输入、`Osc7Sniffer`
跨 `feed()` 切开的 OSC(**在每个字节位置切一刀都要能拼回来**，比照
`emulator.rs` 里 ESU 那条 `cut` 循环)、4KB 上限、`side_text` 的四种组合、
目录名末级提取。以及 `pane_title` 加了右区之后 `Area` 几何仍精确等于
`title_px`(复用既有那两条 `area_rect_*` 断言，加上带 cwd/tmux 的入参)。

**远端前提(写进 `docs/` 与 Release notes)**：裸 shell 下 Ubuntu 默认 bash 就发
OSC 0，零改动可用。tmux 默认 `set-titles off`，需要在远端 `~/.tmux.conf` 加：

```tmux
set -g set-titles on
set -g set-titles-string '#S:#I:#W #{pane_current_path}'
```

第一行让 tmux 开始上报标题(默认串 `#S:#I:#W` 就够拿会话名)，第二行才拿得到目录。

### ⑦ 隐藏标签栏(`ui/chrome.rs`)

```rust
/// F36 标签栏总开关。**关掉但不删代码** —— 将来要回来只改这一个字面量。
/// 关掉的代价：鼠标切标签的入口没有了，只剩 `Ctrl+Tab` / `Ctrl+PgUp/PgDn` 和菜单。
const SHOW_TAB_BAR: bool = false;

pub fn tab_bar(ctx, t, views) -> Option<TabAction> {
    tab_bar_inner(ctx, t, views, SHOW_TAB_BAR)
}
```

`enabled == false` 时 `tab_bar_inner` 直接返回 `None`，**不建 `TopBottomPanel`**
—— 不占高度，中央区自然变高，走既有的 `compute_geoms` → `apply_geometry` →
`window_change` 链路 reflow(T4)，不需要新代码。

既有两条守护测试(`tab_bar_is_shown_even_with_a_single_tab`、
`tab_bar_takes_a_constant_slice_of_the_central_rect`)与 F100 那条
`annotate_mode_registers_the_tab_bar_and_each_tab` 改成显式传 `enabled: true`
走 `tab_bar_inner`，**断言一条不删**(将来开回来时它们还得管用)。
新增一条钉公开路径当前的行为：`tab_bar` 返回 `None` 且不占中央区高度。

## 不动的东西

- `GAP_PX`、`mullion-core` 的布局语义(「严丝合缝拼满、不为分隔条留格」)；
- store schema —— 本切片**零 schema 改动**(`Theme` 是代码里的常量，不落盘)；
- `row_to_runs` 的逐格定位策略(它是对的，① 错在 run 内部的字号)；
- SFTP 面板已经开着时的浏览位置；
- 依赖方向：`mullion-term` 不认识 `RemotePath`，远端状态以 `Vec<u8>` 出境。

## 人工验收清单(无头环境验不了的)

1. 100% 缩放下，让 Claude Code 输出一条 50+ 字符的 ASCII 路径紧跟中文括号 ——
   `.md` 与后面的全角括号**不再重叠**；换 11pt、13pt 再看一遍。
2. 150% 缩放下 pane 标题条上的会话图标**底部不再被切**。
3. 四分屏下：分界线看得见但不抢视线；焦点那块一眼能认出；F83 关掉标题条后
   仍然认得出焦点那块。
4. 裸 shell(不进 tmux) `cd` 几次 → 标题条右区目录名跟着变。
5. 远端 `~/.tmux.conf` 加上那两行、重开 tmux → 标题条右区出现 `tmux:<会话名>`
   和目录名；不加配置时右区**什么都不显示**、不报错。
6. 选中 SSH 分屏 → `Ctrl+Shift+B` → 文件面板远端栏开在**该分屏当前目录**；
   在面板里 `cd` 到别处后不会被拽回去；关掉再开则重新回到分屏当前目录。
7. 标签栏消失、终端多出一行多；`Ctrl+Tab` 仍能切标签，菜单里的标签项仍在。
