# 切片 B2-a 设计 —— 分屏骨架（F30/F31/F34/F35/F82/F83）

日期：2026-07-29
状态：已确认，待写实现计划
上一片：切片 B1（F18 划选复制/粘贴 + F80/F81 视觉基线），v0.1.11 已发布

---

## 1. 目标

让分屏真正跑起来：一个窗口里同时开多个终端，各自连到远端的独立 shell，
布局变了远端排版立刻跟着对。

这是 spec 目标 G2「分屏是一等公民，且分屏后远端排版立刻正确」和场景 S1
「左右分屏，左边跑 Claude Code，右边看日志」的落地。

当前状态：`mullion-core` 的布局树几何**已经写好且有单测**（`compute_rects` /
`close_pane` / `resize_for_pane` / `focus_neighbor`），但 `mullion-app` 侧
是彻底的单 pane —— `App` 持有一个 `conn: Option<Connection>`，`Connection`
里一个 `Pane`，渲染层 `TextLayer::buffers` 是「每屏面行一个 Buffer」。
也就是说：**几何是现成的，接线是零。** 本切片就是把这条线接上。

## 2. 范围

分屏整体拆成两片，本片是 **B2-a 骨架**，手感留 **B2-b**。

| 项 | 编号 | 做 | 说明 |
|---|---|---|---|
| 布局树增删与几何 | F30 | ✅ | core 补 `split_pane`；UI 只给预设入口，不给「再切一刀」 |
| 关闭 pane，兄弟顶替 | F31 | ✅ | 入口 = pane 标题条右侧的 `×` |
| 布局变更立刻 `window_change` | F34 | ✅ | 四种触发路径统一走一条代码路径 |
| 新 pane 复用连接开新 channel | F35 | ✅ | `open_pty` 签名里不含任何网络参数 |
| 工具栏布局预设按钮组 | F82(部分) | ✅ | 1/2/3/4 屏 + 子布局；SFTP/设置按钮不做 |
| pane 标题条 | F83 | ✅ | 主机名 + IP + 状态徽标，可关，默认开 |
| 鼠标点击切焦点 | — | ✅ | 粗命中测试（落点在哪个 pane 矩形里） |

**非目标（明确推给后续切片）**：

| 项 | 编号 | 去向 | 原因 |
|---|---|---|---|
| 拖分隔条 resize | F32 | B2-b | 骨架不含手感 |
| 悬停即作用于该 pane（划选/滚轮/鼠标上报的精细归属） | — | B2-b | 见 §7 已知缺口 |
| 方向键切焦点 | F33 | 快捷键切片 | 本片不引入任何快捷键（用户决定） |
| 新 pane 换接别的主机 | — | B2-b | 数据模型按 N host × M channel 设计好，只是不给 UI 入口 |
| 任意嵌套的手动分屏入口 | — | 暂不做 | `split_pane` 照样实现并单测，只是不给菜单入口 |
| 断线重连 | F6 | 后续 | 本片断了就是断了，内容保留可读 |

**分屏快捷键**（`Ctrl+1/2/3` 切布局、`Ctrl+Tab` 切焦点等）记入 todolist，本片不做。

## 3. 交互模型

来源是原型 `Mullion Standalone.html`，**两段式**：先点屏数，再点子布局。

工具栏（48px，已有「+新建连接」的那一条）右接一个按钮组：

```
[ ▢ 1屏 ][ ▥ 2屏 ][ ▦ 3屏 ][ ▩ 4屏 ]   |   （选中屏数后出现）子布局组
```

子布局选项：

| 屏数 | 子布局 |
|---|---|
| 1 | 无 |
| 2 | 左右分 ▥ / 上下分 ▤ |
| 3 | 左大右上下 ⊟ / 右大左上下 ⊞ / 三等分 ▦ |
| 4 | 无（仅 2×2） |

**`1屏 ▢` 是对原型的补充。** 原型只有 2/3/4，一旦分屏就回不到单屏满窗
（只能靠标题条的 `×` 一个个关，4 屏回 1 屏要点三次）。补这个按钮。

## 4. 架构

依赖方向不变：`app → {core, term, ssh, store}`。

```
mullion-core   布局树几何 + 新增 split_pane（纯函数，零 UI/IO/async）
mullion-ssh    establish / open_pty 分离，一条 handle 开多条 channel
mullion-app    shell::workspace —— 布局状态机 + rect→grid 换算（纯函数，可脱窗单测）
               渲染 / 输入路由 / egui chrome
```

### 4.1 坐标系：布局树跑在像素空间

这是本切片唯一需要新决策的地方。

core 的 `Rect` 原本注释为「单位：格」，但 F83 的 32px 标题条和 1px 分隔线
**不是格宽/格高的整数倍**。若布局树跑在格空间，每次都要把像素余数在 pane
之间分配，误差会累积到边界对不齐。

做法：**布局树跑在像素空间**。core 的二分几何本来就与单位无关，改的只是文档：
`Rect` 的注释从「单位：格」改成「单位由调用方定义（格或像素）」。

`Workspace::layout(px_w, px_h, cell_w, cell_h) -> Vec<PaneGeom>` 是纯函数：

```
1. compute_rects(&tree, 整窗终端区像素矩形)  → 每 pane 的像素矩形
2. 每 pane 扣掉 32px 标题条（若开启）、扣掉 1px 分隔线间隙  → 终端区像素矩形
3. grid_size_for(终端区像素, cell_w, cell_h)             → (cols, rows)
```

第 2 步的分隔线间隙口径：**core 的「子 pane 严丝合缝拼满父区域、分隔条不占独立
格」语义不动**，间隙完全是 app 侧的事 —— 对非最右 / 最下的 pane，`term_px` 的
宽 / 高各减 1px，让出原型 `gap:1px` 的位置。core 不知道分隔线的存在。

`compute_rects` 收的是 `core::Rect`（`u16` 字段），装像素值上限 65535，4K 宽
3840 远未触顶，无需改类型。

```rust
pub struct PxRect { pub x: u32, pub y: u32, pub w: u32, pub h: u32 }  // 定义在 workspace.rs

pub struct PaneGeom {
    pub id: PaneId,
    pub px: PxRect,        // 整块（含标题条）
    pub title_px: PxRect,  // 标题条；标题条关闭时高度为 0
    pub term_px: PxRect,   // 终端网格区
    pub grid: (u16, u16),  // 由 term_px 落成的 (cols, rows)
}
```

**渲染、鼠标命中、`window_change` 三者读同一个 `PaneGeom` 列表。**
这是不让三者对不齐的唯一办法 —— 各算各的必然漂移。

`clamp_ratio` / `MIN_PANE_COLS` 那套格语义的最小尺寸约束本片用不到
（预设的 ratio 是固定值，不存在用户拖出 0 宽 pane 的路径），把它参数化留给
B2-b 的 F32。`grid_size_for` 已经 clamp 到至少 1×1，窗口极小时不会算出 0 格。

### 4.2 `shell::workspace` 数据模型

新建 `crates/mullion-app/src/shell/workspace.rs`。`App` 从 `conn: Option<Connection>`
改为 `ws: Option<Workspace>`。

```rust
pub struct Workspace {
    tree: Node,                 // core 布局树
    focus: PaneId,
    panes: Vec<PaneState>,      // 顺序无语义；几何顺序由 tree 决定
    hosts: Vec<HostConn>,       // 多 handle 池，为 B2-b「改接别的主机」预留
    next_id: u32,               // 单调递增，PaneId 永不复用
    title_bars: bool,           // F83 开关，默认 true
}

struct PaneState {
    id: PaneId,
    host_ix: usize,
    emulator: Emulator,
    pty: SshSession,
    rx: Receiver<Vec<u8>>,
    pacer: SyncFramePacer,      // 每 pane 一个，见 §8 T2
    status: PaneStatus,
    last_grid: (u16, u16),      // 用于 F34 的「仅在变化时发 resize」
}

enum PaneStatus { Live, Disconnected }

struct HostConn {
    label: String,              // 主机名，标题条用
    addr: String,               // IP:port，标题条用
    handle: Handle<ClientHandler>,
}
```

B2-a 里 `hosts` 恒为 1 个元素、所有 pane 的 `host_ix` 恒为 0。做成 `Vec` 是
为了 B2-b 加「换主机」时不用改数据模型返工。

## 5. 布局预设与减屏语义

### 5.1 预设 → 布局树映射

六种预设一一映射成 F30 二分树（符合 F82「预设须套用成一棵 F30 布局树，
不得另立固定枚举模型」）。已按原型的 `grid-area` 逐个核对顺序：

| 预设 | 布局树 |
|---|---|
| 1 屏 | `L1` |
| 2 / 左右分 | `H(0.5, L1, L2)` |
| 2 / 上下分 | `V(0.5, L1, L2)` |
| 3 / 左大右上下 | `H(0.667, L1, V(0.5, L2, L3))` |
| 3 / 右大左上下 | `H(0.333, V(0.5, L1, L2), L3)` |
| 3 / 三等分 | `H(0.333, L1, H(0.5, L2, L3))` |
| 4 / 2×2 | `V(0.5, H(0.5, L1, L2), H(0.5, L3, L4))` |

（`H` = 左右并排即沿列切分，`V` = 上下堆叠即沿行切分，与 core 的 `Dir` 语义一致。）

### 5.2 套用预设 = 声明式重排

不是「在当前树上增删」，而是：

1. 把现有 pane 按**当前树的几何顺序**（`compute_rects` 的返回顺序，深度优先 a 先 b 后）排成队
2. 依次填进新树的叶子位 `L1..Ln`
3. 不足则新建 pane（复用当前 host 开新 channel，见 §6）
4. 多余则按 §5.3 的优先级关闭

好处是「点 3 屏」的结果与路径无关：不管你之前是 2 屏还是 4 屏，点完就是那个样子。

### 5.3 减屏关闭优先级

**直接关，不弹窗。** 顺序：

1. 先关 `Disconnected` 的 pane，按几何逆序
2. 还不够，再关 `Live` 的，按几何逆序
3. 焦点若被关掉 → 落到几何顺序第一个存活 pane

「优先关已断开的」是因为那些 pane 已经没有活的 shell，关掉不损失任何东西。

## 6. SSH 多 channel（F35）

`session.rs` 做一次纯提取式重构，**不改现有行为**：

```rust
pub async fn establish(cfg, policy) -> Result<Handle<ClientHandler>, ConnectError>  // 已有，已公开
pub async fn open_pty(handle: Handle<ClientHandler>, cfg: &SshConfig, wake)         // 新增
    -> Result<(SshSession, Receiver<Vec<u8>>), ConnectError>                         // = 原 connect 后半段
pub async fn connect(...)                                                            // 保留 = establish + open_pty
    -> Result<(SshSession, Receiver<Vec<u8>>), ConnectError>                         // 现有调用方与测试不动
```

`open_pty` 的签名里**没有任何主机 / 网络参数**（handle 是传进来的）。这是 F35
「开 4 个 pane，底层只有 1 次 TCP 连接」的结构性保证：想多开一次 TCP，你得在
类型层面先拿到另一个 `Handle`，改不动是编译不过，不是靠人记得。

每条 channel 的 `io_task` 各持一份 `handle.clone()` 保活，最后一个 pane 关闭时
handle 归零、TCP 才断。

**pane 断线** = 它的 `rx.recv()` 返回 `None` → `status = Disconnected`。
不自动重连（F6 不在本片）。emulator 内容原样保留，仍可滚动（F17）、可划选复制
（F18），只是键盘输入被丢弃。标题条状态点由 `#7fd99b ● 已连接` 变为灰点 `● 已断开`。

## 7. 渲染与输入

### 7.1 渲染分工

按 adr-007 划死：**glyphon 只画终端网格，egui 画一切 chrome。**

`TextLayer` 入口从「喂一屏」改成 `prepare(panes: &[PaneRender])`，
`PaneRender { geom: PaneGeom, snapshot }`。内部 `buffers: Vec<Buffer>` 从
「每屏面行一个」变成按需扩缩的**池**，按 `pane_ix * rows + row` 索引
——cosmic-text 的 `Buffer` 重建很贵，每帧重建会掉帧。

每个 `TextArea`：
- `left` / `top` 取该 pane 的 `term_px` 原点 + 行号 × `cell_h`
- **`bounds` 取该 pane 的 `term_px`** —— 硬要求。不裁剪的话一条超长行会直接
  画到邻居 pane 上去。

**光标只在 focus pane 画实心块，其余 pane 画空心框。** 这是「哪个 pane 在收
键盘」的唯一视觉线索，比标题条高亮更直接。

多 pane 后字形量上去了，`atlas.trim()` 的调用时机按 `docs/gui-render-gotchas.md`
那条办，不改。

### 7.2 egui chrome（F82 / F83）

沿用 `ui-visual-spec-frozen` 已冻结的色板：

- **分隔线**：1px，`rgba(255,255,255,0.06)`，在 pane 边界 overlay
- **pane 标题条**：32px，底色 `#191c27`，底部 1px `rgba(255,255,255,0.05)`；
  内容 = 7px 圆点状态徽标 + 主机名 `12px / 600 / #d3d6ea` ellipsis + IP +
  右侧 `×`（F31 的唯一入口）
- **工具栏按钮组**：底 `#0e1018`，padding 3，radius 8；选中态用 accent `#8b95ff`

### 7.3 输入路由

T8 铁律原样保留，只是「终端」从单例变成 focus pane：

- **键盘：先判后喂。** egui 要键盘（弹窗开着）→ 喂 egui；否则 → 编码后写给
  focus pane 的 `SshSession`，**绝不先过 `egui_state.on_window_event`**。
- **指针：先喂后判。** egui 没消费 → 粗命中测试（落点在哪个 `PaneGeom.px` 里）
  → 点击即切焦点。

**已知缺口（有意，B2-b 补）**：划选（F18）、滚轮（F17）、鼠标上报（F5）三者
在 B2-a 里**只作用于 focus pane，坐标换算用该 pane 的 `term_px` 原点**。
所以不会错位，但你必须先点一下才能在那个 pane 里划选。「指针悬停即作用于该
pane」留 B2-b。

## 8. 领域陷阱

| 陷阱 | 分屏下的新风险 | 做法 / 守护测试 |
|---|---|---|
| **T1** | pane A 的 `PtyWrite` 串到 pane B 的 channel —— 分屏最容易出的串线 bug | 每个 `PaneState` 各持自己的 `SshSession`；新测 `workspace::tests::pty_write_goes_to_its_own_pane_channel_t1` |
| **T2** | pacer 是 per-pane，present 是整窗一次 | 规则：**任一 pane 在同步块内则延后 present**，但每 pane 独立跑 150ms 超时兜底（防一个 pane 卡住全窗）。新测 `render::tests::any_pane_in_sync_defers_present` |
| **T3** | 不变，全窗一个 `FrameLimiter` | 沿用 `app::tests::redraw_is_frame_capped` |
| **T4** | 切预设 / 关 pane / 窗口 resize / 开关标题条，**四种**路径都会改 grid | 统一走一条代码路径：布局变更后对每个 pane 比对 `last_grid`，**仅在变化时**发 `window_change`。新测 `workspace::tests::preset_change_emits_resize_for_every_pane_f34`、`title_bar_toggle_changes_rows_f83` |
| **T7** | 不变 | 事件循环三分支仍显式复位 `control_flow`，沿用 `frame::tests` |
| **T8** | 判定对象从单例变 focus pane，规则不变 | 沿用 `input_route::tests::terminal_keyboard_is_never_fed_to_egui_so_tab_cannot_steal_focus` |

## 9. 测试计划

新增单测**全部脱离窗口**（这正是架构不变量的价值所在）：

**mullion-core**
- `split_pane_replaces_leaf_f30`
- `split_pane_unknown_target_is_noop_f30`

**mullion-app / shell::workspace**
- 7 种预设各断言 tile 严丝合缝、无重叠无缝隙（复用现有 `assert_tiles_exactly` 思路）
- `close_prefers_disconnected_panes`
- `focus_falls_back_to_first_survivor`
- `grid_excludes_title_bar_f83`
- `title_bar_toggle_changes_rows_f83`
- `preset_change_emits_resize_for_every_pane_f34`
- `pty_write_goes_to_its_own_pane_channel_t1`

**mullion-app / render**
- `any_pane_in_sync_defers_present`

**mullion-ssh / live（`MULLION_LIVE=1`，需真机，不进 CI）**
- `establish` 一次 + `open_pty` 四次，断言四条 channel 都收到 shell 首屏（F35 真实验证）

## 10. 顺手收的技术债

两笔都被本切片直接触发，不算 scope 蔓延：

1. `app.rs` 里 `render_frame` 那个恒传 `1` 的 pane 数接真实值
   （B1 收尾时点名要求 F30 落地必须处理）
2. `build_ui` 已 9 参且带 `#[allow(clippy::too_many_arguments)]`，本切片还要
   再塞 workspace。把参数聚成一个 `UiFrame<'_>` 结构体，去掉那个 allow

B1 遗留的另一笔（`theme.rs` 7 个零引用 token 缺 F 编号注释、三个弹窗的错误红
未接 `theme.danger`）本片不碰 —— 与分屏无关。

## 11. 人工验收清单

以下无法在无头容器验证，需 Windows 实机确认：

- [ ] 点 2/3/4 屏及各子布局，pane 排布与原型一致，边界无缝隙无重叠
- [ ] 分屏后在各 pane 里跑 `tmux` + 全屏 TUI，排版正确不错行（F34 的真正验收）
- [ ] 开 4 个 pane 期间，远端 `ss -tn | grep <本机IP>` 只有 1 条连接（F35）
- [ ] 关闭 pane 后兄弟顶替，占满原区域，远端排版立刻跟上
- [ ] 关掉某个 pane 的远端 shell（`exit`），标题条转灰点，内容仍可滚动/复制
- [ ] 减屏时优先关掉已断开的那个
- [ ] 标题条开关切换时，终端行数确实变化（tmux 状态栏位置跟着动）
- [ ] 点击不同 pane 切焦点，光标实心/空心切换正确，键盘打到对的 pane
- [ ] 多 pane 流式输出（4 个 pane 同时 `yes` 或 `tail -f`）时不闪、不撕裂、风扇不起飞
- [ ] CJK 宽字符在各 pane 里都占两格且不越界到邻居 pane
