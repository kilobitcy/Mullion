# 会话管理器 UI 重构设计（F90）

日期：2026-08-03
输入：`/tmp/design-export/Mullion.dc.html`（Claude Design 导出的设计稿，会话管理器模态在第 162–311 行）
产出：把现有「会话管理器 + 编辑器 + 删除确认」三个 `egui::Window` 合并成单窗双栏

---

## 1. 背景与范围

### 1.1 现状

`crates/mullion-app/src/ui/session_manager.rs` 单文件 1065 行，画三个 `egui::Window`：

- `show()`（:337–480）—— 会话管理器：分组 `CollapsingHeader` 内嵌 5 列 `Grid`，底部一行 `[新建][编辑][删除][连接]`
- `show_editor()`（:484–749）—— 编辑器，标题「编辑会话」/「新建会话」
- 删除二次确认（:451）—— 标题「确认删除」的独立小窗

三个问题：**编辑器的字段已经堆到一屏放不下**（P0-b 加完代理四态 + 跳板链之后）、
**窗口互相遮挡**（改一条会话要在两个窗口之间来回看）、
**列表底部按钮会被长列表顶出视野**。

### 1.2 本轮范围（已与用户确认）

**做**：三个窗口合并成设计稿的 880×560 单窗双栏。

**同时新增一个交互**：左栏「复制」按钮（§4.4）。它不在「重排」的字面范围内，
但设计稿画了、且实现成本为零（不碰 store），故一并做。除此之外本轮无新功能。

**不做**（各有归属，本轮一律不碰）：

| 设计稿里画了但本轮不做 | 归属 |
|---|---|
| SFTP 目录树侧栏 | F50~F55（P1） |
| 设置弹窗（主题/字号/快捷键） | F84（P3-c） |
| 顶部工具条（+新建连接 / SFTP / ⚙） | F82 修订已明确否决独立工具栏 |
| 左侧「已连接会话」侧栏 | 未排期 |
| 图标 / 语义色 / 标签 / 收藏 / 排序 | F61~F63（P2-a） |

**持久化数据模型一个字段不加**：`SessionRecord`、`SessionDraft`、`GroupRecord`
全部保持原样，`mullion-store` 零改动。

UI 传输态按重构需要改动：`EditorBuffer` 重排字段顺序并加 `#[derive(PartialEq)]`，
`UiState` 增删若干字段（§7.1），`SaveIntent` 增 `then_connect`。
这两类是不同性质的东西，不要混谈。

### 1.3 编号

spec.md 现有号段（F1-6 / F10-21 / F30-38 / F50-57 / F60-69 / F70-72 / F80-84）中
没有描述会话管理器**形态**的条目——它一直是 F60（分组）与 F70（存储）的隐含 UI 载体。
`F60~F69` 已被会话管理路线图 §3 排满，`F86~F89` 已排给会话级外观覆盖。

故本轮启用新编号 **F90**（已核对 `F90` 在 `spec.md` 全文零命中）。实现时往 `spec.md` §4.6 加一行：

| ID | 需求 | 优先级 | 验收标准 |
|---|---|---|---|
| F90 | 会话管理器单窗双栏：左栏搜索+分组列表，右栏三 Tab 编辑器，删除确认内联，不再弹独立窗口 | P1 | 单测断言一帧内 `Order::Middle` 层只有一个 Area；搜索匹配与脏检查为纯函数、可无窗口单测 |

---

## 2. 视觉

设计稿色板与已冻结的 `MULLION_DARK`（`theme.rs`）**几乎完全同源**，逐条比对（已核对色值）：

| 设计稿 | 用途 | `theme.rs` token | 一致 |
|---|---|---|---|
| `#14161f` | 左栏底 | `panel_bg` | ✅ |
| `#0e1018` | 搜索框底 | `sunken_bg` | ✅ |
| `#181b26` | 模态底 | `bar_status` | ✅ |
| `#8b95ff` | accent（选中边条 / 主按钮 / Tab 下划线） | `accent` | ✅ |
| `#7fd99b` | 状态-已连接、SFTP 徽章 | `ok` | ✅ |
| `#7c9eff` | SSH 徽章 | `info` | ✅ |
| `#e0b767` | NEW 徽章 | `warn` | ✅ |
| `#4b5066` | 状态-闲置 | `fg_ghost` | ✅ |
| `#e06767` | 错误卡片 / 删除按钮 | `danger` = `#e81123` | ❌ |

endpoint 副文本用现有 `fg_faint` = `#565b70`（设计稿为 `#6b7188`，差异在深色底上不可辨，
不新增 token）。

**唯一实质分歧是错误色**。`danger = #e81123` 是 Windows 系统红，饱和度高，
铺成大面积卡片底色会在深色面板上过于跳。处理：**新增一个 token `danger_soft = #e06767`**，
用于错误卡片底（alpha 0.09）/ 边（alpha 0.28）/ ⚠ 图标、以及「删除」按钮 hover 底色；
`danger` 原值不动，继续服务状态栏的 `last_error` 小字。

已冻结的视觉基线（`2026-07-29-ui-visual-baseline-design.md` §2）不因本轮改动而修订，
只在色板表末尾追加 `danger_soft` 一行。

圆角：`apply_egui` 的全局兜底是 `Rounding::same(7.0)`，基线 §2.5 规定模态用 12。
本窗口用 `egui::Frame::rounding(12.0)` 覆盖，不动全局值。
（已核对 egui 0.30.0 有 `Frame::rounding`；0.31 起才改名 `corner_radius`，本项目不受影响。）

---

## 3. 窗口骨架

```rust
// ui/session_manager/mod.rs
egui::Window::new("会话管理器")
    .open(&mut open)
    .collapsible(false)
    .default_size([880.0, 560.0])
    .resizable(true)
    .min_width(720.0)          // 低于此宽度双栏挤不下
    .frame(egui::Frame::window(&ctx.style()).fill(bar_status).rounding(12.0))
    .show(ctx, |ui| {
        ui.set_min_height(CONTENT_MIN_HEIGHT);   // 见下方「必须验证的假设」
        egui::SidePanel::left(ui.id().with("sm_list"))
            .exact_width(300.0)
            .resizable(false)
            .frame(list_frame)
            .show_inside(ui, |ui| list::show(ui, t, ui_state, sessions, groups, connected));
        egui::CentralPanel::default()
            .frame(editor_frame)
            .show_inside(ui, |ui| editor::show(ui, t, ui_state, sessions, groups));
    });
```

已核对 egui 0.30.0 存在：`SidePanel::show_inside`、`TopBottomPanel::show_inside`、
`CollapsingHeader::id_salt`、`Ui::allocate_response`。

**必须在实现时验证的假设**：`egui::Window` 的高度由内容决定，而
`SidePanel::show_inside` 取 `ui.available_rect_before_wrap()` 的高度。若不给下限，
左栏会塌陷成「内容有多高就多高」，而不是撑满 560。`ui.set_min_height(...)` 是本设计
选定的解法，但**它在 egui 0.30 下是否真的让 `SidePanel` 撑满，无头环境验证不了**，
必须在实机验收里目视确认（进 §10 清单）。若不成立，退路是改用
`Window::fixed_size([880.0, 560.0])` + 闭包首行 `ui.set_min_size(ui.available_size())`。

### 3.1 `store_available == false` 的降级

现有实现在 `show()` 首行短路（:349）：整个窗口只显示一行错误文案。

新结构保留同一策略，但位置上移到**双栏之前**：`set_min_height` 之后立刻判断，
`false` 时只画一行 `last_error`（或兜底文案「会话功能不可用」）后 `return`，
**不进入 SidePanel / CentralPanel**。理由：store 不可用时 `sessions` 必为空，
画一个空的双栏比一行错误文案更让人困惑。

`store_available` 因此只需传给 `mod.rs::show()`，**不再往下传给 `list::show` /
`editor::show`**——两个子函数被调用时 store 必然可用。

### 3.2 `editor_open` 字段的语义变更

现有 `UiState::editor_open: bool` 表示「编辑窗口是否打开」。右栏常驻后这个概念消失，
**该字段删除**。已核对全部 11 个引用点，删除时都要处理：

| 位置 | 现在做什么 | 改成 |
|---|---|---|
| `ui/mod.rs:74` | 字段定义 | 删除 |
| `ui/mod.rs:186` | `session_manager_open \|\| editor_open` 决定是否画 | 只判 `session_manager_open` |
| `app.rs:793` | 断连时 `editor_open = false` | 删除该行（管理器窗口本身不因断连关闭） |
| `app.rs:911` | 参与「是否有模态弹窗」判断，影响输入路由 | 换成 `session_manager_open` |
| `session_manager.rs` 内 8 处（:414,427,474,477,491,735,743,748） | 开关编辑窗口 | 随旧 `show_editor` 一并删除 |

`app.rs:911` 那处**不能漏**：它决定弹窗打开时键盘事件是否还判给终端。漏改会让
「管理器开着时按键仍被终端吃掉」——与 T8 同类的输入路由 bug。

`editor_id: Option<SessionId>` 保留并改变语义：
`Some(id)` = 右栏正在编辑既有会话，`None` = 右栏是一份新建草稿。

---

## 4. 左栏（300px）

自上而下四段：

```
搜索框     TextEdit::singleline，hint「搜索名称、主机、用户…」，底色 sunken_bg
ScrollArea（占据中段剩余高度）
  CollapsingHeader(组名).id_salt(gid)      标题右对齐「N 个」
    session_row × N
TopBottomPanel::bottom(show_inside)        删除确认条（条件显示）+ [+ 新建] [复制] [删除]
```

底部区用 `TopBottomPanel::bottom(...).show_inside(ui, ...)` 固定在左栏内，
不随列表滚动跑出视野——这是现有实现的实际问题之一（§1.1）。

### 4.1 `session_row` 自定义 widget

现有实现用 5 列 `egui::Grid` + `selectable_label`，做不出「选中行整行底色 + 左侧 2px 竖条」。
改为 `ui.allocate_response(row_size, Sense::click())` 拿整行矩形后用 `painter` 手绘：

| 元素 | 规格 |
|---|---|
| 选中态背景 | `accent.gamma_multiply(0.12)`，圆角 6 |
| 选中态左边条 | 宽 2px，色 `accent`，贴行左缘 |
| 状态点 | 直径 6px，已连接 `ok` / 其余 `fg_ghost` |
| 名称 | 默认字号，色 `fg` |
| endpoint | `host:port`，`FontId::monospace(10.0)`，色 `fg_faint` |
| 协议徽章 | 右对齐 pill，圆角 6；SSH → 前景 `info` / 底 `info` alpha 0.16；SFTP → 前景 `ok` / 底 `ok` alpha 0.16 |

**交互必须承接现有的两种点击**（现在挂在 `selectable_label` 上，:383–388）：

- `response.clicked()` → 选中（走 §5.3 的脏检查）
- `response.double_clicked()` → `connect_request = Some(rec.id)`

手绘 row 不会自动带上这两个行为，重写时容易只做单击。这是本节最容易漏的一点。

### 4.2 搜索

纯函数，放 `list.rs`，可无窗口单测：

```rust
/// 名称 / 主机 / 用户三者任一包含 query（大小写不敏感）即命中；query 为空全通过。
pub(crate) fn matches(rec: &SessionRecord, query: &str) -> bool
```

**只匹配这三个字段**。tags / 收藏 / 排序权重属 F63（P2-a），本轮不做。

**F63 落地时 `matches` 会被取代**：路线图 §6 为 P2-a 预留的验收签名是
`filter(sessions, query, tags, env) -> Vec<SessionId>`。届时 `matches` 或被 `filter`
内部复用、或整体让位，**不承诺保留它的 bool 谓词签名**。本轮不为那次演进预留抽象。

**搜索非空时分组一律展开**。实现上必须用 `CollapsingHeader::open(Some(true))`
**而不是 `default_open(true)`**——已核对 egui 0.30 的 `collapsing_header.rs`：
`default_open` 只在 `CollapsingState` 首次加载时生效，用户手动折叠过之后状态被写进
`ctx.data()`，后续帧的 `default_open` 不再覆盖它。现有 `group_header()`（:332）用的正是
`default_open(true)`，照抄过来会得到「搜到了但组是折的」。搜索为空时恢复
`open(None)`（交还用户控制）。

### 4.3 删除二次确认（承接 `pending_delete`）

现有实现是第三个 `egui::Window`（「确认删除」，:451）。**改为左栏底部的内联确认条**：
`pending_delete.is_some()` 时，在三个按钮上方显示一行
「删除「{name}」？此操作不可撤销。 [确认删除] [取消]」。

- `[确认删除]` → `delete_request = Some(id)`；`pending_delete = None`；若 `selected == Some(id)` 则清空
- `[取消]` → `pending_delete = None`

行为与现有完全一致（都不直接碰 store，只写意图），只是从独立窗口改成内联。
改内联的两个理由：设计稿的模态是单层；以及 §9.2 的「只有一个窗口」断言需要它。

### 4.4 「复制」按钮

不动 store：把当前选中记录的 `EditorBuffer` 克隆一份、名称追加 ` 副本`、
`editor_id = None`、baseline 设为该草稿的初始值（§5.3），右栏即变成一份预填好的
新建草稿，用户点保存才真正写盘。

**已知限制**：密码/口令存在加密侧车里，UI 层拿到的 `&[SessionRecord]` 不含明文，
所以复制出来的草稿凭据为空，需要重新输入。认证 Tab 的提示文案会说明这一点。

---

## 5. 右栏

```
标题行     {name}，16px；新建时右侧 NEW 徽章（前景 warn / 底 warn alpha 0.16）
副标题     user@host:port，FontId::monospace(11.5)，色 fg_faint
Tab 条     连接 │ 认证 │ 高级        选中项底部手绘 2px accent 横线
放弃确认   条件见 §5.3
错误卡片   条件见 §5.2
ScrollArea → 当前 Tab 的字段
TopBottomPanel::bottom      [保存] [保存并连接 / 连接]
```

Tab 状态存 `UiState::editor_tab: EditorTab`（`Connection` / `Auth` / `Advanced`，
`Default` = `Connection`）。egui 无内置 Tab 控件，用 `ui.selectable_label` 横排 +
`painter.hline` 手绘下划线。

### 5.1 三个 Tab 的字段分配

**严格只重排 `EditorBuffer` 的现有 21 个字段，不增不减**（已逐字段核对）。

| Tab | 行 | 字段（左 / 右） |
|---|---|---|
| 连接 | 1 | 名称 `name` / 协议 `protocol`（宽 110） |
| | 2 | 主机 `host` / 端口 `port`（宽 110） |
| | 3 | 用户名 `user` / 分组（下拉，写 `preserved_group_id`） |
| | 4 | 备注 `note`（整行 `TextEdit::multiline`） |
| | 5 | 只读文本「最后修改 {rec.modified_at}」，新建时不显示 |
| 认证 | 1 | 认证方式下拉 `auth_kind`（密码 / 公钥） |
| | 2 | `Password` → 密码框；`PublicKey` → 私钥路径 `key_path` + `[选择…]`（触发 `pick_key_request`） |
| | 3 | `PublicKey` 时追加私钥口令 `passphrase` |
| | 4 | 提示文案（见 §5.4） |
| 高级 | 1 | 代理模式下拉 `proxy_mode`（跟随分组 / 不使用代理 / SOCKS5 / HTTP CONNECT） |
| | 2 | 选中 SOCKS5 或 HTTP CONNECT 时展开 `proxy_host` / `proxy_port` / `proxy_user` / `proxy_password` |
| | 3 | 跳板链 `jump_chain`：已选条目列表（每条一个「移除」）+ 「添加跳板」下拉（候选 = 其余会话）；`jump_set == false` 时显示「跟随分组」与 `[改为自定义]` |

余下 5 个字段（`preserved_tags` / `preserved_terminal` / `preserved_appearance`
以及 `preserved_group_id`、`jump_set` 的存储用途）不出现在表单上，
由 `build_draft` 原样透传——现有守护测试
`editing_a_session_preserves_fields_the_form_cannot_edit` 钉着这个行为。

代理下拉必须保持**四态**。「跟随分组」（`Inherit`，写 `None`）与「不使用代理」
（`Direct`，写显式直连）语义不同，P0-b 有两条守护测试钉着
（`choosing_no_proxy_writes_explicit_direct_not_inherit` / `choosing_inherit_leaves_proxy_unset`），
合并两态会让它们变红。

### 5.2 错误卡片

设计稿的错误卡片可关闭。要正确实现「关闭后下一次出错要重新弹出」，
就必须保证每次写 `last_error` 都复位 `error_dismissed`。已核对 `app.rs` 有
**10 处**直接给 `self.ui.last_error` 赋值：

| 行 | 场景 |
|---|---|
| 714 | store 打开失败 |
| 720 | 无法定位配置目录 |
| 854 | `PaneOpened` 世代过滤后的错误 |
| 887 | 连接失败（`ConnectErr`） |
| 1396 | 删除失败 |
| 1415 | 保存失败 |
| 1434 / 1439 | 分组操作失败 |
| 1459 | `connect_request` 里 `ssh_config_for` 返回 `Err` |
| 1486 | 指纹落盘失败 |

逐处补一行 `error_dismissed = false` 必然漏掉，**收口成单一入口**：

```rust
impl UiState {
    pub fn set_error(&mut self, msg: String) {
        self.last_error = Some(msg);
        self.error_dismissed = false;
    }
}
```

上述 10 处全部改走 `set_error`。实现后用 `grep -n "last_error = Some" app.rs` 自查应为零命中。

显示条件：`last_error.is_some() && !error_dismissed && editor_id.is_some()`。
新建草稿时不显示——那条错误多半来自别的会话，挂在草稿上是误导。

样式：底 `danger_soft` alpha 0.09，边 `danger_soft` alpha 0.28（1px），圆角 8，
内容为「⚠ {粗体首行} / {说明}」+ 右上 ✕。首行取 `last_error` 全文，
说明为固定文案「请核对用户名与密钥/密码，或先单独连接该跳板验证凭据。」

`chrome.rs::status_bar()` 里那段用 `t.danger` 常驻展示 `last_error` 的兜底逻辑（:135）
**保持不变**——它的存在理由（「不受弹窗开关状态影响，兜底展示」）在本轮之后依然成立，
而且错误卡片有 `editor_id.is_some()` 的显示前提，更需要状态栏兜底。

### 5.3 脏检查与底部按钮

**基线快照法**。`UiState` 存一份 `editor_baseline: EditorBuffer`，
每次把内容载入右栏时**同时**设置 `editor` 和 `editor_baseline`：

| 载入场景 | `editor` | `editor_baseline` |
|---|---|---|
| 选中既有会话 | `EditorBuffer::from_record(rec)` | 同左（同一个值） |
| 点「+ 新建」 | `EditorBuffer::default()` | 同左 |
| 点「复制」 | 克隆 + 改名后的草稿 | 同左 |

脏判定于是对三种场景**统一**：

```rust
/// 当前 buffer 与载入时的基线快照不等 ⇒ 用户改过东西。
pub(crate) fn is_dirty(buf: &EditorBuffer, baseline: &EditorBuffer) -> bool {
    buf != baseline
}
```

**这比 `is_dirty(buf, rec)` 那种「拿记录现算」的写法更正确**：新建草稿没有
`SessionRecord` 可传，现算法在新建路径上无解（复核发现的缺口）。基线快照法
天然覆盖新建与复制。

为此给 `EditorBuffer` 加 `#[derive(PartialEq)]`。已核对 21 个字段的类型
（含 `TerminalPrefs` / `AppearancePrefs` / `IconSpec` / `ColorSpec` / `Protocol` /
`SessionId` / `GroupId`）全部已 derive `PartialEq`，不会编译失败。
**`Debug` 仍是手写打码实现，不受影响**（守护测试
`debug_never_leaks_editor_buffer_secrets` 继续绿）。
**不得手写 `PartialEq` 跳过密码字段**——那会让「只改了密码」判不出脏。

按钮语义：

| 状态 | secondary | primary |
|---|---|---|
| 未改动（`!is_dirty`） | 保存（禁用） | 连接（`editor_id == Some`）／保存并连接（`None`） |
| 已改动 | 保存 | 保存并连接 |

**没有「取消」按钮**。右栏常驻，「取消」没有对应语义；放弃修改 = 切走或关窗，
由下面的脏检查兜底。

**切换选中项 / 关窗时的脏检查**。目标用显式枚举表达，不用嵌套 `Option`：

```rust
pub enum SwitchTarget { Session(SessionId), NewDraft }
// UiState:
pub pending_switch: Option<SwitchTarget>,
```

（`Option<Option<SessionId>>` 语义上能自洽，但 clippy 的 `option_option` lint 正是
为这种写法准备的，且全仓无先例。显式枚举读起来不用心算哪一层的 `None`。）

脏时点另一条会话/新建/关窗 → 不直接切换，把目标记进 `pending_switch`，
右栏 Tab 条下方显示内联确认「有未保存的修改，放弃吗？[放弃] [继续编辑]」。
选「放弃」才真正执行切换并清空 `pending_switch`；选「继续编辑」只清空 `pending_switch`。

### 5.4 已知缺陷（本轮不修，登记为遗留）

编辑既有会话时密码框恒为空——密码在加密侧车，UI 层拿不到明文。而
`build_draft` 把空密码当作「清除凭据」（守护测试
`password_session_with_empty_password_clears_secret` 正是这个行为）。
右栏常驻后「保存」按钮更显眼，用户「改个备注 → 随手保存 → 已存密码被清空」的概率上升。

（注意：用户**主动重设密码**的路径没问题——往密码框敲字符会让
`buf != baseline` 成立，保存按钮启用，新密码正常写入。有风险的只是
「改了别的字段、无意中连带清空密码」。）

修它需要给 `EditorBuffer` 加 `password_touched: bool`，并让 store 支持
「本次保存不改动凭据」的语义——**这超出「本轮不扩数据模型」的边界，故不做**。

本轮的缓解措施只有一条：认证 Tab 顶部**加粗**显示
「留空并保存将清除已存凭据，不会保留原值。」

> **此项需用户确认**：接受作为遗留，还是本轮一并修（会突破范围）。

---

## 6. 状态点与连接追踪

设计稿的状态点是三态（ok / fail / idle）。本设计**降级为两态**：

- 绿点 `ok` = 该会话是当前已连接的那条
- 灰点 `fg_ghost` = 其余

理由：fail 态需要一张 `HashMap<SessionId, 上次连接结果>` 的内存表，而已核对
`UserEvent::ConnectErr(String)`（`app.rs:48`）与 `ConnectOk { ssh, rx, handle }`（:42）
**都不带 `SessionId`**，要补一条完整的「哪条会话正在连」的追踪线才能归因。
而 fail 是瞬时事件，错误卡片（§5.2）已经表达了它。三态的成本远高于收益。

两态的接线成本是 `app.rs` 三处：

1. `connect_request.take()` 施加时（:1451）记 `self.connecting_session = Some(id)`
2. 处理 `ConnectOk`（:750）时 `self.connected_session = self.connecting_session.take()`
3. 处理 `ConnectErr`（:880）时 `self.connecting_session = None`

CLI 直连（`cli_direct`）没有 session id，`connecting_session` 保持 `None`，
于是所有会话都是灰点——正确，因为那条连接确实不对应任何已存会话。

`UiFrame` 增一个 `connected_session: Option<SessionId>` 字段传给左栏。

---

## 7. 意图接线

egui 的借用约束不变：`egui_ctx.run(|ctx| ...)` 闭包内只有 `&mut UiState`，
拿不到 `&mut SessionStore`。所有改 store / 发起连接的动作仍然只写「意图」到 `UiState`，
由 `app.rs` 在借用释放后统一施加。

### 7.1 `UiState` 增删

```rust
+ pub search: String,                       // §4.2
+ pub editor_tab: EditorTab,                // §5
+ pub editor_baseline: EditorBuffer,        // §5.3
+ pub error_dismissed: bool,                // §5.2
+ pub pending_switch: Option<SwitchTarget>, // §5.3
- pub editor_open: bool,                     // §3.2，删
```

`pending_delete` / `selected` / `editor_id` / `connect_request` / `delete_request` /
`save_request` / `pick_key_request` 全部保留，语义不变。

`SaveIntent` 增 `then_connect: bool`（§5.3 的「保存并连接」）。

### 7.2 `app.rs` 的 `save_request` 施加逻辑抽成纯函数

现在这段逻辑（:1415 附近）内联在事件循环里，无法单测。抽出：

```rust
/// 施加一次保存意图，返回受影响的会话 id（供 then_connect 用）。
/// 保存失败返回 Err(错误文案)。
fn apply_save(store: &mut SessionStore, save: SaveIntent, now: &str)
    -> Result<SessionId, String>
```

已核对 `store.add(draft, now) -> SessionId`（`vault.rs:117`）返回新分配的 id，
而**当前调用点丢弃了这个返回值**。新建路径改为接住它；编辑路径返回 `save.editing_id`
里的 id。

调用方在 `then_connect` 为真时把返回的 id 塞进 `connect_request`，
下一帧走既有的连接施加路径（不复制连接逻辑）。

---

## 8. 文件结构

`ui/session_manager.rs`（1065 行）拆成目录：

| 文件 | 职责 | 约 |
|---|---|---|
| `mod.rs` | 窗口壳、`store_available` 降级、双栏骨架、`is_dirty`、`SwitchTarget` 与切换确认、`pub use` 重导出 | 200 |
| `list.rs` | 左栏全部：搜索框、`matches()`、分组树、`session_row`、删除确认条、底部三按钮 | 300 |
| `editor.rs` | 右栏全部：标题、Tab 条、错误卡片、三 Tab 字段、底部按钮 | 380 |
| `buffer.rs` | `EditorBuffer` / `build_draft` / `SaveIntent` / `AuthKindUi` / `ProxyModeUi` + 现有纯逻辑单测 | 360 |

`mod.rs` 用 `pub use buffer::{EditorBuffer, SaveIntent};` 重导出，
`ui/mod.rs` 与 `app.rs` 里的 `session_manager::SaveIntent` / `session_manager::EditorBuffer`
**引用路径完全不变**，本次拆分对调用方不可见。

拆分理由：`buffer.rs` 的内容（数据结构 + 校验纯函数 + 它们的单测）与绘制代码零耦合，
是天然切割线；而 1065 行的单文件再加三 Tab 必然过 1500 行。

---

## 9. 测试策略

CLAUDE.md 与 P0-b 的教训要求：**每条守护测试都要自证「破坏被守护的属性后确实变红」，
且注入点必须扎在 bug 的真实发生位置，不能只改顶层参数。** 下表逐条给出注入点。

### 9.0 现有测试的迁移

`session_manager.rs` 现有 **14 个** `#[test]`（已核对，不是 11 个），其中 12 个调用
`build_draft`。迁移安排：

| 测试 | 去处 |
|---|---|
| 12 个 `build_draft` 系列 | 原样平移到 `buffer.rs`，行为不得改变 |
| `debug_never_leaks_editor_buffer_secrets` | 原样平移到 `buffer.rs`（加 `PartialEq` 后尤其要它继续绿） |
| `collapsing_header_id_salt_disambiguates_same_titled_groups` | 平移到 `list.rs`（分组树搬去那里）；`id_salt` 不得因重写而丢失 |

迁移后 `cargo test -p mullion-app` 的测试**总数只增不减**。

### 9.1 新增纯逻辑测试（无窗口、无 GPU）

| 测试 | 位置 | 红队注入点 |
|---|---|---|
| `search_matches_name_host_and_user` | `list.rs` | 从 `matches` 里删掉任一字段的匹配分支 |
| `empty_search_matches_everything` | `list.rs` | 给空 query 加一条 `return false` 的短路 |
| `search_is_case_insensitive` | `list.rs` | 去掉 `to_lowercase()` |
| `unedited_buffer_is_not_dirty` | `mod.rs` | 载入记录时把 `editor_baseline` 设成 `EditorBuffer::default()` 而非与 `editor` 同源（这正是「忘了同步设置基线」的真实疏漏形态） |
| `typing_a_password_makes_buffer_dirty` | `mod.rs` | 手写 `PartialEq` 跳过 `password` 字段（真实动机：有人会以为「密码不该参与比较」） |
| `new_draft_with_typed_fields_is_dirty` | `mod.rs` | 让新建路径不设 baseline，或让 `is_dirty` 在 `editor_id.is_none()` 时恒返回 false |
| `set_error_resets_dismissed` | `ui/mod.rs` | 从 `set_error` 里删掉 `error_dismissed = false` |
| `apply_save_new_returns_id_allocated_by_store` | `app.rs` | 让新建路径丢弃 `store.add` 的返回值、改返回 `SessionId::default()`（即当前代码的行为） |
| `apply_save_edit_returns_editing_id` | `app.rs` | 让编辑路径也走 `store.add`（等于每次保存都新建一条） |

### 9.2 跑真 UI

复用 `ui/mod.rs` 现成的测试基础设施（已核对签名）：
`run_frame(&Context, &mut UiState, UiFrame, RawInput) -> (FullOutput, UiActions)`（:239）、
`rendered_text(UiFrame) -> (String, UiActions)`（:263，**内部跑两遍**以消化
`egui::Area` 的首帧 sizing pass）、`ctx.read_response(id).rect.center()` +
`egui::Event::PointerButton` 模拟真实点击。

| 测试 | 断言 | 红队注入点 |
|---|---|---|
| `session_manager_draws_exactly_one_window` | 见下方实现说明 | 把 editor 或删除确认拆回独立 `egui::Window` |
| `dirty_switch_asks_before_discarding` | 脏时点另一条会话 → 出现「放弃吗」文案且 `editor_id` 未变 | 让点击直接覆写 `editor_id` |
| `save_and_connect_carries_then_connect` | 点「保存并连接」→ `save_request.then_connect == true` | 让该按钮走与「保存」相同的分支 |
| `switching_tab_changes_visible_fields` | 切到「高级」后代理下拉可见、主机输入框不可见 | 三个 Tab 都无条件绘制全部字段 |
| `double_click_on_row_requests_connect` | 双击行 → `connect_request == Some(id)` | 手绘 row 只处理 `clicked()`（§4.1 点名的易漏项） |

**`session_manager_draws_exactly_one_window` 的实现说明**（复核指出的坑）：
egui 0.30 里 `UiKind::Window` 只存在于构建期的 `UiStackInfo`，
`Memory::areas().order()` 只给 `Vec<LayerId>`，**没有公开 API 能按 kind 计数**。
可行路径是按 `Order` 过滤：

```rust
let n = ctx.memory(|m| m.areas().order().iter()
    .filter(|l| l.order == egui::Order::Middle).count());
assert_eq!(n, 1, "会话管理器必须是单窗；新增顶层 Window 会让这条变红");
```

`egui::Window` 默认 `Order::Middle`，而 `ComboBox` / `Popup` 用 `Order::Foreground`，
不会干扰计数。**将来若新增别的 `Order::Middle` 顶层窗口，这条会误报——那是有意的**：
它会强制新增者重新确认「会话管理器仍是单窗」这个属性，注释里要写明这一点。

### 9.3 回归

本轮不触碰终端/输入/渲染路径，但 `app.rs` 事件循环有改动（§3.2、§6、§7.2），
按 CLAUDE.md 的要求必须重跑：`app::tests::redraw_is_frame_capped`（T3）、
`frame::tests`（T7）、
`input_route::tests::terminal_keyboard_is_never_fed_to_egui_so_tab_cannot_steal_focus`（T8）。
T8 尤其相关——§3.2 要改 `app.rs:911` 的模态判断，那正是输入路由的分叉点。

「绿」= `cargo test --workspace` 全过 **且** `clippy --workspace --all-targets -- -D warnings` 无输出。

---

## 10. 人工验收清单（无头环境验证不了）

按 CLAUDE.md「你无法验证的东西」，以下必须在 Windows 11 实机目视确认，
交叉编译发布后写进 Release notes：

1. **双栏高度**：左栏是否撑满整个窗口高度（§3 的 `set_min_height` 假设是否成立）
2. 窗口在 880×560 默认尺寸下三 Tab 的字段是否都不需要滚动即可看全
3. 拖拽 resize 到 `min_width`（720）时双栏是否仍可用、不重叠
4. 选中行的 accent 左边条与底色是否与设计稿一致，是否与终端背景形成足够对比
5. 协议徽章（SSH/SFTP）的 pill 圆角与配色
6. 错误卡片的 `danger_soft` 在深色面板上是否不刺眼、✕ 是否可点
7. 搜索框输入中文时输入法候选框是否遮挡列表
8. 搜索时已被手动折叠过的分组是否确实展开（§4.2 的 `open(Some(true))` 是否生效）
9. 「保存并连接」是否真的先落盘再发起连接（改字段 → 点它 → 断开后重开管理器，字段仍在）
10. 脏检查：改一半字段点另一条会话，是否弹确认而非静默丢弃
11. 删除确认从独立窗口改成内联条后是否仍然醒目（不会被误点）

---

## 11. 与架构不变量的关系

- 本轮改动全部落在 `mullion-app`，不新增任何 crate 间依赖，依赖方向
  `app → {core, term, ssh, store}` 不变。
- `matches` / `is_dirty` / `build_draft` / `apply_save` 均为纯函数，可无窗口单测——
  这正是 CLAUDE.md 强调的「布局 bug 和键码 bug 能在没有窗口的情况下写测试复现」的同类收益。
- `mullion-store` 零改动。
- 不触碰 `emulator.rs`、`keymap.rs`、`text.rs`、`gpu.rs`，T1/T4/T5/T6 不受影响；
  T3/T7/T8 因 `app.rs` 有改动需回归（§9.3）。
- `host_key_reply`（TOFU 确认）与 `paste_reply`（多行粘贴确认）是 `ui/mod.rs` 里
  独立于 `session_manager` 之外的模态，本轮不涉及。但它们也是顶层 `Window`——
  §9.2 的单窗断言测试必须在**不触发这两个模态**的状态下运行。

## 12. 开放问题

1. §5.4 的密码清空缺陷：接受为遗留，还是本轮一并修（需扩 `EditorBuffer` + store 语义）？
2. §3 的 `set_min_height` 能否让 `SidePanel` 撑满——实现时首个任务就应验证，
   不成立则走文中给的退路。
