# 切片 P1-a + P1-b 设计：会话管理器 UI 打磨 + 表单校验 + 测试连接

日期：2026-08-04
编号：F90（承接）+ **F91 表单必填校验**（新增）+ **F92 测试连接**（新增）+ **F93 私钥选取**（新增）
输入：v0.1.15 实机截图 `/tmp/1.png` + 用户 20 条 UI 意见 + 布局参考稿 `/tmp/2.png`
产出：修掉右栏宽度溢出的根因，按「能不能走完一次完整流程」补齐校验、拨测、私钥选取

---

## 1. 背景与范围

### 1.1 现状

`docs/superpowers/specs/2026-08-03-session-manager-ui-design.md`（F90）把三个窗口合成了
880×560 单窗双栏，v0.1.15 发布后实机截图暴露一个硬 bug 和一批打磨欠账。

用户原话的取舍锚点：

> 如果只能改三处，我会选：**修掉右侧溢出裁切**、**给底部按钮做主次区分并把主按钮移到右边**、
> **表单加必填校验和「测试连接」**。这三个直接影响"能不能用完一次流程"，其余属于打磨。

本切片 = 这三处 + 同一批改动顺带能做掉的低成本项。

### 1.2 溢出 bug 的根因（已实测坐实，非目测）

`mod.rs` 的 `egui::Window` 闭包里只对**高度**做了两件事：`ui.set_min_height(...)` 地板 +
条件式 `ui.set_max_height(...)` 天花板。**宽度维度一行都没有。**

egui 0.30 的行为（探针实测）：

- `SidePanel::show_inside` 通过 `ui.expand_to_include_rect` + 只增不减的棘轮把尺寸报回外层 ui
- `CentralPanel::show_inside` 直接吃 `ui.available_rect_before_wrap()`，**不回报**

结果：右栏（CentralPanel）画到了外层 ui 从未被撑开的宽度之外。探针数值 ——
**window 实际宽 312，内部 shapes 铺到 902，溢出 574px**。`min_width(720.0)` 那行**未生效**
（`egui::Window` 的 `min_width` 只约束 `Resize` 的下限，约束不到 `CentralPanel` 的绘制）。

用户报的三条现象 ——「超出边界被裁切」「两个面板顶底错位像两个独立浮层」
「resize 手柄位置误导」—— 是同一个 bug 的三种表现。

### 1.3 本轮做什么

| # | 内容 | 归属 |
|---|---|---|
| 1 | 窗口宽度地板 + 条件式天花板（修根因） | F90 |
| 2 | 左右栏可拖拽分隔条（220–480px 夹紧） | F90 |
| 3 | 副文本对比度提到 WCAG AA | F80 |
| 4 | 字段分区 + 行距 + 底部按钮条贴底 | F90 |
| 5 | 状态点 hover tooltip | F90 |
| 6 | 必填校验（纯函数 + 红星 + 禁用 + Tab 红点） | **F91** |
| 7 | 底部按钮条重排（唯一实心主按钮在最右） | F90 |
| 8 | 测试连接（完整认证后立即断开） | **F92** |
| 9 | 拨测指纹「仅本次信任、不落盘」 | F3 修订 |
| 10 | `~/.ssh` 私钥扫描（只看文件名） | **F93** |
| 11 | 拖拽私钥文件填路径 | **F93** |
| 12 | 认证方式胶囊选中态填充 · 口令占位符改「留空表示无口令」（§13） | F90 |

### 1.4 本轮不做（已排期，勿顺手做）

| 项 | 归属 |
|---|---|
| Windows named pipe ssh-agent（`\\.\pipe\openssh-ssh-agent`）+ `AuthKind::Agent` | **P1-c**，是 F1(P0) 的 Windows 缺口 |
| `last_connected` 字段 + store 迁移 + 重名会话区分 + 列表项 hover 图标 | **P1-d** |
| 浅色配色 | 用户已明确不采纳，`/tmp/2.png` 只当布局参考 |
| 圆角统一 | 见 §5，据实建议不改 |
| `editor_tab: usize` 换 enum、`UiState` 拆分、`last_error` 收 pub 等 | 既有技术债，独立处理 |

---

## 2. 窗口宽度：补齐宽度维度

在 `mod.rs` 现有高度处理**之前**插入宽度地板，之后插入条件式天花板：

```rust
// 地板:让 Window 至少给出容得下「左栏 + 右栏」的宽度。
// 不能靠 `Window::min_width` —— 它只约束 Resize 的下限,约束不到 CentralPanel 的绘制。
ui.set_min_width(WINDOW_W - 2.0 * ctx.style().spacing.window_margin.left);

// 天花板:必须条件式。`Placer::set_max_width` 是无条件覆写 region.max_rect,
// 无脑设会作废 Resize 当帧从拖拽算出的候选尺寸,resize 手柄就拖不动了。
if ui.max_rect().width() > avail_w {
    ui.set_max_width(avail_w);
}
```

**`avail_w` 不能复用 `window_chrome_reserve`** —— 那个函数（`mod.rs:90-97`）算的是
「标题栏高度 + 上下 margin」，是个**纵向**量，减到横向可用宽度上量纲就错了（标题栏高度随
字号/无障碍缩放在 20~124px 之间浮动，与横向 chrome 毫无关系）。横向另算：

```rust
let m = ctx.style().spacing.window_margin;
let avail_w = screen_w - (m.left + m.right) - SLACK;
```

`Margin` 的字段在 egui 0.30 已经是 `f32`（`epaint/src/margin.rs:9-14`），**不要写 `as f32`** ——
`clippy::unnecessary_cast` 会让 `-D warnings` 直接红。

**红线（一个字都不许动）**：`const SLACK: f32 = 8.0;`、`fn window_chrome_reserve(ctx)`、
`const WINDOW_TITLE`、现有那段高度处理、三条高度守护测试、测试 helper `fn new_button_rect(ctx)`
（必须在 `ctx.run` 闭包**内部**调用）、`collapsing_header_id_salt_disambiguates_same_titled_groups`。

探针实测：只加地板那一行，window 宽即从 312 变 880（剩 17.5 是 `Frame::window` 的阴影，正常）。

---

## 3. 可拖拽分隔条

用 egui 内置能力，不手写拖拽：

```rust
egui::SidePanel::left(...)
    .resizable(true)
    .default_width(LIST_W)              // 300.0
    .width_range(LIST_MIN_W..=LIST_MAX_W)  // 220.0..=440.0
```

`width_range` 收 `impl Into<Rangef>`，`220.0..=440.0` 经 `From<RangeInclusive<f32>>` 自动转换，可直接写。

### 3.1 上限取 440 而不是 480 —— 与窗口最小宽联立算过

`mod.rs:161` 现有 `.min_width(720.0)`，意味着用户可以把窗口横向拖到 720。此时若分隔条
拖到 480 上限，右栏只剩 `720 - 480 - (10 + 14) = 216px`，放不下两列表单 ——
「右栏永远有 ≥400px」这句话在 480 + 720 的组合下**不成立**。

两个约束一起改：

| 常量 | 原 | 新 | 理由 |
|---|---|---|---|
| `Window::min_width` | 720.0 | `WINDOW_W`（880.0） | §2 的 `set_min_width` 地板本来就强制内容 ≥880，720 已形同虚设、且与本节的宽度承诺自相矛盾 |
| `LIST_MAX_W` | （新增）480 | **440.0** | 最窄窗口下右栏 = `880 - 440 - 24 = 416px` ≥ 400 |

`mod.rs:499` 那条测试的注释里写着「屏幕宽 1000(超过 `.min_width(720.0)`…)」，改常量时
注释要同步成 880；1000 > 880，该测试的行为不变。

上限 440 保证右栏在**任何**合法窗口宽度下都有 ≥416px 放两列表单。

左右栏 `inner_margin` 拉平（现为左 10 / 右 14），共用窗口的 `Frame`，顶底自然对齐 ——
「两个独立浮层」的观感随之消失。

### 3.2 这里引入的新失效模式

`SidePanel` 的宽度棘轮是**只增不减**的，默认行为是拖宽左栏 → **把整个窗口撑宽**，
而不是「右栏变窄」。这与 split view 语义相反。§2 的条件式天花板就是拦这个的，
但它需要一条守护测试盯死：**拖分隔条到最右，窗口总宽必须不变，右栏被压到最小宽。**

---

## 4. 对比度：换 token，不改 token

按 WCAG（sRGB 线性化 + 相对亮度）实算用户点名的那处副文本：

| token | 值 | 在 `panel_bg #14161f` 上 | 判定 |
|---|---|---|---|
| `fg_faint`（`list.rs` 现用） | `#565b70` | **2.69 : 1** | ❌ 远低于 AA 4.5:1 |
| `fg_dimmer` | `#8a90a8` | **5.71 : 1** | ✅ |
| `fg_dim` | `#9aa0b8` | 6.9 : 1 | ✅ 但层级感偏弱 |

**改动**：会话列表副文本 `{user}@{host}` —— `list.rs:88` 那处 `theme::c32(t.fg_faint)`
换成 `theme::c32(t.fg_dimmer)`。

**不改 `fg_faint` 这个 token 本身** —— 它在别处用作真正的装饰性文本，且 F80/F81 色板
已实机验收（memory 记为「已冻结」）。零新增 token、零改动既有 token、零回归面。

`fields.rs:162` 跳板提示那处 `fg_faint` 同理照改（同为需要读的信息，不是装饰；
161 行是 `colored_label(` 调用行，颜色实参在 162）。

---

## 5. 圆角：据实建议不改

用户意见是「胶囊圆角 + 直角面板 + 中等圆角输入框三种半径并存 = 风格混搭」。

实际读码结论相反：这是设计文档 §2.5 定的**有意层级**，`theme.rs:180-182` 的注释即规则本身：

> 7px 是全局兜底圆角；设计文档 §2.5 按场景分了 pill 6 / 按钮 7 / 控件组 8 / 模态 12，
> 调用方需要不同值时用 `Frame::rounding` 覆盖，不改这里。

窗口 12、错误卡片 8、列表行 6 都在规格内。统一成单一值会抹掉「模态 > 卡片 > 行」的纵深。

**本轮不动圆角。** 溢出 bug 让两个面板边界错位，很可能放大了违和感；建议本版实机后再判断。
（此条已当面向用户报告分歧，用户批准按此执行。）

---

## 6. 留白、分区与状态点

### 6.1 字段分区

右栏每个 Tab 内按小标题分区，区间用 `ui.add_space(10.0)` + 小标题（`fg_muted`，11px）：

| Tab | 分区 |
|---|---|
| 连接 | **基本**（名称/主机/端口/协议）· **归类**（分组/备注） |
| 认证 | **身份**（用户名/认证方式）· **凭据**（密码 或 私钥+口令） |
| 高级 | **代理**（模式/地址/用户/口令）· **跳板**（启用+已配置提示） |

`grid()` 的行距从 `spacing([12.0, 8.0])` 拉到 `spacing([12.0, 10.0])`。

### 6.2 输入框右边缘统一

现状混用 `f32::INFINITY` / `80.0` / `70.0` / `secret_edit` 的 `200.0` / 私钥 `TextEdit` 无宽度。
统一规则：

- 单值长文本（名称/主机/用户名/代理用户）→ `f32::INFINITY`
- 数值短字段（端口/代理端口）→ `80.0`
- `secret_edit` → 去掉 `desired_width(200.0)`，改 `f32::INFINITY`（与同 Grid 其他行对齐）
- 私钥路径 → 在 `ui.horizontal` 内用 `ui.available_width() - 按钮宽` 撑满

### 6.3 状态点保留 + tooltip

用户判断「颜色全灰、没有含义就删」不成立。`list.rs` 的小圆点**有**两态语义：
绿(`ok`)=已连接 / 灰(`fg_ghost`)=未连接；截图全灰只是因为当时没连。

**改动**：给圆点 rect 加 `on_hover_text`，文案「已连接」/「未连接」。

「连接中」态做不出来 —— `UserEvent::ConnectOk` / `ConnectErr` 都不带 `SessionId`，
在途连接归不到具体行上。这是既有限制，本片不解（P1-d 的 `last_connected` 会顺带处理事件带 id）。

---

## 7. 必填校验（F91）

### 7.1 `validate.rs` —— 新建，纯函数，零 egui

`crates/mullion-app/src/ui/session_manager/validate.rs`。**只新建，不搬现有代码。**

```rust
/// 缺哪些必填项。端口有默认值 22,不算必填。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Missing {
    pub name: bool,
    pub host: bool,
    pub user: bool,
}

impl Missing {
    pub fn any(self) -> bool { self.name || self.host || self.user }

    /// 第一个缺项所在的 Tab 索引(与 `UiState::editor_tab` 同义:0 连接 / 1 认证 / 2 高级)。
    /// 用 usize 而非新枚举:`editor_tab: usize` 换 enum 是既有技术债,不在本片范围。
    pub fn tab(self) -> Option<usize> {
        if self.name || self.host { Some(0) } else if self.user { Some(1) } else { None }
    }

    /// 给按钮 tooltip 用,如「还缺：主机、用户名」。
    pub fn hint(self) -> String { /* 拼接 */ }
}

/// 判定用 trim(),防止一串空格骗过校验。
pub fn check(name: &str, host: &str, user: &str) -> Missing
```

签名收三个 `&str` 而非 `&EditorBuffer`：这样 `validate.rs` 连 `EditorBuffer` 都不依赖，
测试不用构造整个 buffer。调用方在 `editor.rs` 里拆字段传入。

### 7.2 UI 消费

- 必填标签加红星：`名称 *` / `主机 *` / `用户名 *`，星号用 `danger_soft`
- Tab 标题右上角画红点：`missing.tab() == Some(i)` 时在 `selectable_label` 的 rect 右上角
  画 3px `danger_soft` 圆

### 7.3 按钮禁用是**两个**原因的并集，tooltip 必须分支

必填校验（§7.2）和拨测在途（§9.3）都会禁用按钮，且作用集合不同。统一在一处算出，
**禁用理由决定 tooltip 文案** —— 否则「字段都填齐了、只是正在拨测」时会弹出一句
文不对题的「还缺：…」：

```rust
enum Disabled { No, Missing(String), Probing }

fn why(missing: Missing, probe: &ProbeState) -> Disabled { /* Missing 优先于 Probing */ }
```

| 按钮 | 必填未齐 | 拨测在途 |
|---|---|---|
| 保存 | 禁用 | **可用**（存草稿不需要链路） |
| 保存并连接 | 禁用 | 禁用 |
| 测试连接 | 禁用 | 禁用 |
| 复制连接串 | 禁用 | 可用 |

tooltip：`Missing(hint)` → 「还缺：主机、用户名」；`Probing` → 「测试连接进行中…」。
一律走 `.on_disabled_hover_text(...)`。

### 7.4 存量脏数据不会把用户锁死

历史会话若 `user` 为空（v1→v2 迁移或手改 TOML 造成），打开后「保存」是灰的。
这**不是**死锁：把缺的字段填上按钮即恢复。校验只挡保存，不挡编辑、不挡删除、不挡关闭。
此项写进人工验收清单（§16）。

---

## 8. 底部按钮条重排

按 `/tmp/2.png` 的层级，从左到右：

```
[测试连接]  [复制连接串]                    [取消]  [保存]  [ 保存并连接 ]
└──── 次要动作,靠左 ────┘                └──── 靠右,主按钮在最右 ────┘
```

- **唯一实心主按钮 = 「保存并连接」**：`accent` 底 + `accent_fg` 字（`Button::fill` + `Button::stroke`）
- 「保存」「取消」保持既有描边/幽灵样式，不加填充 —— 主次要一眼可辨
- 「复制连接串」从右栏标题条（`editor.rs:48-52`）挪到这里 —— 它是动作，不是标题装饰
- 沿用现有 `TopBottomPanel::bottom(...).show_inside(ui, ...)` 贴底写法

**不许写成**：`let bottom = 44.0; let body_h = ui.available_height() - bottom;` 再喂
`ScrollArea::max_height`。`editor.rs` 的注释记着这个坑 —— 左栏原本这么写，把「+ 新建」
顶出了可见区（`c4eb7f1`）。

---

## 9. 测试连接（F92）

### 9.1 复用 `establish`，但 **`mullion-ssh` 要加一个 `disconnect`**

`session.rs` 的 `establish(cfg, policy)` 走完的正是完整链路：
**代理 → 跳板 → TCP → 握手 → 指纹校验 → 认证**，返回 `SshConnection`。
不调 `open_pty` 就不开 channel。拨测复用它，不新造拨测 API。

**但「drop 掉 `SshConnection` 就是断开」这句话不成立，本仓库自己已经证伪过：**

`session.rs:70-83` 的 `_jumps` 字段注释记着一次红队实验 —— `tests/two_hop_jump.rs` 里
把 `jumps.push(handle)` 换成 `drop(handle)` 真的丢掉跳板 Handle 后，
**隧道在 3 秒空闲窗口内依然可用**。根因在 russh 0.54.5：

```rust
// russh-0.54.5/src/client/mod.rs:262-265
impl<H: Handler> Drop for Handle<H> {
    fn drop(&mut self) { debug!("drop handle") }   // 只打日志,不做任何 IO
}
```

Drop 不发 `SSH_MSG_DISCONNECT`、不 abort 后台 task。连接真正关闭要等**所有**
`Sender<Msg>` 克隆都被丢弃、run loop 的 `recv()` 返回 `None` 才 break
（`client/mod.rs:1152-1159`）。拨测场景不开 channel，理论上没有额外的 sender 克隆
（`channels/mod.rs:377` 那处 `sender.clone()` 不会发生），所以 drop **可能**够用 ——
但这只是推理，且**一旦拨测配置带跳板链，红队实验已经证明它不成立**。

拨测恰恰要覆盖跳板链，所以不能赌。改法：

```rust
// crates/mullion-ssh/src/session.rs —— 薄封装,暴露 russh 既有的 Handle::disconnect
impl SshConnection {
    /// 主动断开:先断目标主机,再逐个断跳板。
    /// 不能只靠 Drop —— russh 的 `impl Drop for Handle` 只打日志、不发 disconnect。
    pub async fn disconnect(&self) {
        let _ = self.handle.disconnect(Disconnect::ByApplication, "", "").await;
        for h in &self._jumps {
            let _ = h.disconnect(Disconnect::ByApplication, "", "").await;
        }
    }
}
```

`Handle::disconnect` 已存在（`client/mod.rs:796-811`），会真的 `send(Msg::Disconnect{..})`。
现在拦路的只是 `SshConnection::handle()` 是 `pub(crate)`（`session.rs:107`），app 侧够不着。
加的是一个**薄方法**，不是 §9.1 原本否掉的那种拨测抽象层。

**这推翻了「`mullion-ssh` 一行不用改」，§15 的依赖表已同步更正。**

### 9.2 `app.rs` 新增 `spawn_probe`（带世代号）

仿既有 `spawn_connect`（`app.rs:529-561`），但**必须带世代号**（理由见 §9.3）：

```rust
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

fn spawn_probe(&mut self, cfg: SshConfig) {
    self.probe_epoch = self.probe_epoch.wrapping_add(1);
    let epoch = self.probe_epoch;
    let policy = Arc::new(PromptingPolicy::new(known, proxy_for_policy, /* persist */ false));
    let proxy = /* clone EventLoopProxy */;
    let h = self._runtime.spawn(async move {
        let ev = match tokio::time::timeout(PROBE_TIMEOUT, establish(&cfg, policy)).await {
            Err(_)     => UserEvent::ProbeErr(epoch, "超时(20s):链路不通或对端无响应".into()),
            Ok(Err(e)) => UserEvent::ProbeErr(epoch, e.to_string()),
            Ok(Ok(c))  => { c.disconnect().await; UserEvent::ProbeOk(epoch) }
        };
        let _ = proxy.lock()./* ... */send_event(ev);
    });
    self.probe_task = Some(h);   // 供退出时 abort,见 §9.4
}
```

**超时是必须的**：高延迟代理链路下 `establish` 可能长时间不返回，没有超时按钮就永久转圈。
20s 取自「本项目的目标场景是高延迟代理链路」——比常见的 10s 宽松一档。

### 9.3 世代号：迟到的拨测结果不许污染别的会话

`ProbeOk`/`ProbeErr` 和既有的 `ConnectOk`/`ConnectErr` 一样**不带 `SessionId`**
（§6.3 提过这个限制）。没有世代号就会有这条竞态：

> 点「测试连接」→ 立刻切到另一个会话（`probe` 被重置为 `Idle`）→
> 旧任务的 `ProbeErr("认证失败")` 姗姗来迟 → 无条件写进 `self.ui.probe` →
> **上一个会话的失败结果显示在当前这个毫不相关的会话表单上**。

所以事件带 epoch，`app.rs` 只在 `epoch == self.probe_epoch` 时才采纳，否则丢弃：

```rust
UserEvent::ProbeOk(e) if e == self.probe_epoch => self.ui.probe = ProbeState::Ok,
UserEvent::ProbeOk(_) => { /* 迟到的旧世代,丢弃 */ }
```

切换会话 / 关闭编辑器 / 关闭会话管理器时 `probe_epoch += 1` —— 一次自增即让所有在途
结果失效，比"记住是哪个会话发起的"简单且不会漏。

**为什么 §6.3 的状态点做不到而这里做得到**：列表状态点要同时表示 N 个会话的连接态，
需要真正的 `SessionId` 归属；拨测同一时刻至多一个（编辑器只有一个、按钮拨测期间禁用），
是单例操作，一个世代号就够。两处标准不同不是疏忽。

`UiState` 新增：

```rust
pub enum ProbeState { Idle, Running, Ok, Err(String) }
```

- 结果显示在按钮条**上方**，复用现有错误卡片**样式**（`rounding(8.0)`），成功态换 `ok` 色
- **与既有 `last_error` 卡片互斥**：同一时刻只画一张。`last_error` 优先（保存失败比拨测
  结果更需要立刻处理），probe 结果让位但不清空，`last_error` 消失后重新显示
- `Running` 期间的按钮禁用见 §7.3 的表
- 改任意字段 → `Ok`/`Err` 重置为 `Idle`（旧结果对新配置无意义），但**不动 `probe_epoch`**
  （在途任务的结果本就该被丢弃，靠 §9.3 的 epoch 比对，不靠这里）

禁用「保存并连接」的理由不是美观：`pending_host_key` 是单个 `Option`，两个
`HostKeyPrompt` 同时在途会互相顶掉。（既有 `await_reply` 是 fail-closed 的，被顶掉的那个
会 Reject 而非挂死，所以这不是安全洞；但用户会看到一次莫名其妙的「连接被拒绝」。）

### 9.4 拨测在途时关窗 / 退出程序

两件事，行为不同，都要显式定义：

| 动作 | 行为 |
|---|---|
| 点「取消」关编辑器 | `probe_epoch += 1`，UI 置 `Idle`。后台任务继续跑到自然结束，结果因世代不符被丢弃 |
| 关掉会话管理器窗口（右上角 X） | 同上。注意 `UiState::close_session_manager()`（`ui/mod.rs:138-141`）现在**只清 `pending_delete`、不清 `editor`**，本轮要在那里补上 `probe_epoch += 1` |
| 关闭整个程序 | `event_loop.exit()` **不会** cancel `_runtime.spawn` 出去的任务。叠加 §9.1 的 drop 不确定性，理论上存在「点了关闭、进程要等最长 20s 才退干净」的风险。**用 `self.probe_task.take().map(\|h\| h.abort())` 兜底**，不依赖超时 |

最后一条写进人工验收清单（§16）—— 无头环境验不了「点 X 后进程多久真正消失」。

---

## 10. 「仅本次信任」（F3 修订）

### 10.1 落盘点只有一处

`PromptingPolicy`（`host_key.rs`）**本身不落盘**，只做 `check()` + 送
`UserEvent::HostKeyPrompt` + `await_reply`。真正写 `known_hosts` 的是 `app.rs:1478-1506`。

### 10.2 改动

**先改 `PromptingPolicy` 自己** —— 这一步容易漏：`HostKeyPrompt` 全仓库唯一的构造点在
`PromptingPolicy::decide`（`host_key.rs:117`）**内部**，那里只拿得到 `&self`，
所以 `persist` 必须先落到 policy 的字段上：

```rust
pub struct PromptingPolicy {
    known: Arc<Mutex<KnownHostsFile>>,
    proxy: Mutex<EventLoopProxy<UserEvent>>,
    /// 用户接受后是否写入 known_hosts。正式连接 true;测试连接 false(仅本次信任)。
    persist: bool,
}

// 现签名 `new(known, proxy)` 加第三参 —— 唯一调用点在 app.rs 的连接路径
pub fn new(known: Arc<Mutex<KnownHostsFile>>, proxy: EventLoopProxy<UserEvent>, persist: bool) -> Self
```

再给 `HostKeyPrompt` 加字段：

```rust
pub struct HostKeyPrompt {
    pub host: String,
    pub algo: String,
    pub fingerprint: String,
    pub previous: Option<HostKeyEntry>,
    /// 接受后是否写入 known_hosts。正式连接 true;测试连接 false(仅本次信任)。
    pub persist: bool,
    pub reply: oneshot::Sender<bool>,
}
```

`app.rs` 那处：

```rust
if accept && prompt.persist {          // ← 原为 `if accept`
    diag::mark(diag::Stage::StoreIo);
    let mut kh = self.known_hosts.lock()./* ... */;
    kh.record(&prompt.host, HostKeyEntry { /* ... */ });
    if let Err(e) = kh.save() { /* set_error 不变 */ }
}
let _ = prompt.reply.send(accept);     // 无论如何都要回复,否则 await_reply 挂死
```

`PromptingPolicy` 的 fail-closed 语义（事件循环已关 / sender 被丢 → Reject）**一行不动**，
5 条既有测试保持不变。

### 10.3 弹窗文案必须区分

拨测触发的指纹弹窗，正文追加一行：

> 本次测试不会记住此指纹，正式连接时会再次询问。

不说清楚的话，用户会把「同一主机被问两次」当成 bug。这是用户在设计问答里明确接受的代价
（「弹确认框，仅本次信任不落盘；测试按钮永不写 known_hosts」）。

---

## 11. `~/.ssh` 私钥扫描（F93）

### 11.1 `keyscan.rs` —— 新建，纯函数，路径参数化

`crates/mullion-app/src/ui/session_manager/keyscan.rs`：

```rust
/// 扫描给定目录下**看起来像私钥**的文件。只 read_dir 取文件名,绝不读文件内容。
/// 目录路径是参数而非内部拼 home:否则无法用 tempdir 单测。
pub fn scan(ssh_dir: &Path) -> Vec<PathBuf>
```

规则：

- **收**：同目录存在同名 `.pub` 兄弟的文件；或文件名以 `id_` 开头
- **排除**：`*.pub`、`known_hosts*`、`config`、`authorized_keys`、**任何指向目录的条目**
- 目录不存在 / 无权限 / 读失败 → 返回空 `vec![]`，不报错、不打断 UI
- 结果按文件名排序（保证 UI 顺序稳定、测试可断言）
- 条目数上限 `const MAX_ENTRIES: usize = 512;`，超出即停 —— 这是个被参数化的纯函数，
  可以被指向任意目录，不能假定它总是小小的 `~/.ssh`

**绝不读文件内容** —— 用户明确选定「只看文件名，不把私钥内容读进内存」。

**符号链接必须用 `entry.metadata()` 判目录，不能用 `entry.file_type()`**：
`file_type()` 不跟随符号链接，一个指向目录的链接会给出 `is_symlink()=true, is_dir()=false`，
若它恰好叫 `id_prod` 就会被当成候选私钥、把目录路径填进 `key_path`，
后面连接时报一句含混的错。`metadata()` 跟随链接，能正确判掉。

文件名非 UTF-8 时 `PathBuf`/`OsStr` 本身不会崩；ComboBox 文案用 `.to_string_lossy()`
有损展示，可能显示替换字符，功能不受影响，不特殊处理。

调用方在 app 侧取 home：项目已依赖 **`directories = "5"`（不是 `dirs`）**，写法是
`directories::BaseDirs::new()?.home_dir().join(".ssh")`。**本轮不新增任何依赖。**

### 11.2 UI

私钥输入行变成三段：`[路径输入框] [▾ 候选] [浏览…]`。

「▾ 候选」是个 `ComboBox`，列出 `scan()` 结果的**文件名**（悬停出完整路径），选中即填 `buf.key_path`。
扫描在**打开编辑器 / 切到认证 Tab 时做一次**，结果缓存在 `UiState`，不是每帧扫盘。

候选为空时该下拉禁用，tooltip「未在 ~/.ssh 找到私钥」。

---

## 12. 拖拽私钥文件（F93）

egui-winit 已经把 `WindowEvent::DroppedFile` / `HoveredFile` 转成
`ctx.input(|i| i.raw.dropped_files)` / `i.raw.hovered_files`，**不用碰 winit 层**。

- 私钥输入行 rect 上有 `hovered_files` 时，画 1px `accent` 虚线边框提示可放
- `dropped_files` 非空且落点在该 rect 内 → 取第一个文件的 `path` 填入 `buf.key_path`

`DroppedFile.path` 是 **`Option<PathBuf>`**（`egui/src/data/input.rs:360`），不是裸
`PathBuf` —— 浏览器场景下会是 `None`。`None` 时静默忽略该条。

边界情况逐条定死（不留给实现阶段发挥）：

| 情况 | 行为 |
|---|---|
| 一次拖多个文件 | 取第一个，其余忽略。在结果卡片位置提示「已忽略其余 N 个文件」 |
| 拖的是目录 | 用 `fs::metadata` 判到是目录 → 不填入，提示「请拖入私钥文件，不是目录」（与 §11.1 同一个判据，不要写两套） |
| 落点在窗口内但不在私钥输入行 rect 内 | 静默忽略，不填、不提示。虚线高亮本身就是可放区域的指示 |
| 认证方式当前是「密码」（私钥行不可见） | 整个拖放逻辑不生效（rect 不存在） |

**不触碰 T8**：T8 管的是键盘事件必须「先判后喂」（否则 egui 焦点系统吞 Tab）。
`DroppedFile`/`HoveredFile` 在 `app.rs:901-956` 的路由里既不满足 `is_kbd` 也不满足
`is_ptr`，落进 `else` 分支被**无条件喂给 `egui_state.on_window_event`**（`:931`）——
根本不参与 `route()` 判定，所以 T8 的键盘规则不受影响。
（措辞更正：不是「走先喂后判分支」，是压根不进分支判定。）

---

## 13. 其余打磨（低成本，随手做）

| 项 | 改法 |
|---|---|
| 认证方式胶囊选中态对比度弱 | `fields.rs:75-78` 两个 `selectable_value` 改成选中态 `accent` 填充 + `accent_fg` 字 |
| 私钥口令占位符误导 | `secret_edit` 状态 2 的 `hint_text("未设置")` → **「留空表示无口令」** |
| 「复制连接串」空表单可点 | 纳入 §7.2 的统一禁用 |

---

## 14. 测试策略

| 测试 | 守护什么 | 位置 | 无窗口？ |
|---|---|---|---|
| `validate::tests::required_fields_reject_whitespace_only` | 全空格不算填 | `validate.rs` | ✅ 纯函数 |
| `validate::tests::missing_maps_to_first_offending_tab` | 缺项 → Tab 索引 | `validate.rs` | ✅ |
| `keyscan::tests::picks_id_prefixed_and_pub_paired_only` | 收 `id_*` / 有 `.pub` 兄弟的 | `keyscan.rs` | ✅ tempdir |
| `keyscan::tests::excludes_config_known_hosts_and_pub` | 排除项 | `keyscan.rs` | ✅ tempdir |
| `keyscan::tests::missing_dir_returns_empty_without_error` | 目录缺失不炸 | `keyscan.rs` | ✅ tempdir |
| `keyscan::tests::symlink_to_dir_named_like_a_key_is_excluded` | §11.1 符号链接坑 | `keyscan.rs` | ✅ tempdir（`std::os::unix::fs::symlink`，`#[cfg(unix)]`） |
| `editor_panel_stays_within_window_rect` | §2 溢出根因 | `mod.rs` | egui headless ctx |
| `dragging_the_split_does_not_widen_the_window` | §3.2 棘轮 bug | `mod.rs` | egui headless ctx |
| `save_buttons_are_disabled_when_required_fields_are_empty` | §7.3 | `mod.rs` | egui headless ctx |
| `probe_prompt_does_not_persist_host_key` | §10 `persist` | `app.rs` | ✅ tempdir |
| `stale_probe_result_is_discarded_after_epoch_bump` | §9.3 世代竞态 | `app.rs` | ✅ 纯状态机 |

### 14.1 自证纪律（P0-b 教训）

每条守护测试都要**自证「破坏被守护的属性后确实变红」**，且自证必须扎到 bug 的
**真实注入点**，不能只改顶层参数。举例：

- `probe_prompt_does_not_persist_host_key` 的自证 = 把 `if accept && prompt.persist`
  改回 `if accept`，**不是**把传进去的 `persist` 参数改成 `true`
- `editor_panel_stays_within_window_rect` 的自证 = 注释掉 `ui.set_min_width(...)` 那一行
- `dragging_the_split_does_not_widen_the_window` 的自证 = 把条件式天花板整段删掉
- `stale_probe_result_is_discarded_after_epoch_bump` 的自证 = 把 `epoch == self.probe_epoch`
  的守卫去掉，**不是**改测试里传的 epoch 值

### 14.2 溢出测试断**真实 rect**，不扫 shapes

**不要**去 `FullOutput.shapes` 里找「最右的 shape」—— 三个 Tab 内容不同，"最右图元"是谁
没锁死，极易复现本项目已经踩过两次的假阳性（文字锚点 vs 真实 rect、`this_pass` vs
`prev_pass`）。`list.rs` 的注释记着其中一次：按钮真实矩形底边 694、文字锚点只有 637、
屏幕高 680，**14px 的真实溢出被锚点判定当成了通过**。

正确做法是直接捕获右栏的 `Response.rect`：`CentralPanel::show_inside` 返回 `InnerResponse`，
`mod.rs:310` 已经用同一手法（`window_resp.map(|r| r.response.rect)`）拿过窗口整体矩形，
照此把右栏 rect 也回传出来，断言：

```rust
assert!(editor_rect.right() <= window_rect.right() + SLACK);
```

取值必须在 `ctx.run` 闭包**内部**（既有 `new_button_rect(ctx)` 的约定，别在闭包外读）。

---

## 15. 依赖方向检查

| crate | 本轮改动 | 是否违反单向依赖 |
|---|---|---|
| `mullion-core` | 无 | — |
| `mullion-term` | 无 | — |
| `mullion-ssh` | **有**：`SshConnection::disconnect()` 一个薄方法（§9.1） | 否，仍是被 app 单向依赖 |
| `mullion-store` | 无 | — |
| `mullion-app` | 其余全部改动 | 否，`app → {…}` 方向不变 |

`validate.rs` / `keyscan.rs` 虽在 `mullion-app` 内，但零 egui 依赖、可无窗口单测 ——
符合「布局 bug 和键码 bug 能在没有窗口的情况下写测试复现」的架构初衷。

### 15.1 依赖与 feature

- **不新增任何 crate 依赖。** `directories = "5"`（取 home）与 `tempfile = "3"`
  （dev-dep，keyscan 测试用）都已在 `crates/mullion-app/Cargo.toml` 里
- **要显式补一个 feature**：`mullion-app` 声明的 tokio features 是
  `rt-multi-thread, macros, net, io-util, sync`，**没有 `time`**。
  `tokio::time::timeout` 现在能编过，是靠 russh 0.54.5 要求 `time`、
  resolver v2 做 feature 并集**泄漏**过来的。这种依赖别人 feature 的写法很脆
  （russh 哪天去掉这个 feature 就炸），本轮在 `mullion-app` 的 tokio 依赖上显式加 `"time"`

---

## 16. 人工验收清单（无头环境无法验证）

发版 notes 原样带上：

1. 打开会话管理器，**右栏不再被裁切**，左右栏顶底对齐、像一个整体
2. 拖动中间分隔条：右栏跟着变窄/变宽，**窗口总宽不变**；拖到两端会停住（220 / 440）
3. 会话列表副文本 `user@host` 现在能看清（对比度 2.69 → 5.71）
4. 底部按钮：「保存并连接」是唯一实心按钮且在最右
5. 新建会话时三个必填项标红星，不填齐按钮全灰，悬停提示缺什么，对应 Tab 有红点
6. 「测试连接」能跑通一次真实链路（含代理/跳板），成功/失败/超时三态文案正确
7. 测试连接遇到**新主机**时弹指纹框，文案写明「本次不会记住」；接受后**正式连接会再问一次**，
   且 `known_hosts` 里**没有**多出该主机
8. 认证 Tab 的私钥下拉能列出 `~/.ssh` 里的私钥（本机 `~/.ssh` 被权限拒绝，我完全没验证过）
9. 从资源管理器拖一个私钥文件到私钥输入行，路径被填入
10. 认证方式胶囊的选中态现在看得出来
11. 整体观感：字段分区、行距、留白是否比 v0.1.15 舒服
12. **拨测在途时关掉会话管理器 / 关掉整个程序**：进程要立刻退干净，不能卡到 20s 超时才消失
    （§9.4 用 `JoinHandle::abort` 兜底，但"是否真的立刻退"只有实机能看）
13. **拨测在途时切到另一个会话**：等旧拨测结束，结果**不得**显示在新会话的表单上（§9.3 世代号）
14. 历史会话里有必填项为空的（如 `user` 空）：打开后能正常编辑、填上就能保存，不卡死（§7.4）
15. 拖拽边界：一次拖多个文件只取第一个并提示、拖目录被拒、拖到私钥行以外无反应（§12 表）

第 6/7/8/9/12/13/15 条我在无头容器里**完全无法验证**，只有测试脚手架。
其中第 8 条尤其：这台机器的 `~/.ssh` 被权限设置拒绝，扫描逻辑我一次都没在真实目录上跑过。

---

## 17. spec.md 需要新增的编号

实现时一并追加到 spec.md 的 F80–F90 表格后：

| ID | 需求 | 优先级 | 验收标准 |
|---|---|---|---|
| F91 | 会话表单必填校验：名称/主机/用户名为空时禁用保存类按钮，提示缺项并在对应 Tab 标红点 | P1 | 校验为纯函数、可无窗口单测；全空格不得通过 |
| F92 | 「测试连接」拨测：走完整链路（代理→跳板→握手→指纹→认证）后**显式 disconnect**（不能只靠 Drop），不开 channel，20s 超时 | P1 | 拨测**永不**写 known_hosts（单测断言 tempdir 内文件无新增）；成功/失败/超时三态；迟到结果按世代号丢弃 |
| F93 | 私钥选取：扫描 `~/.ssh` 列出候选（只读文件名不读内容）+ 支持拖拽私钥文件填路径 | P1 | 扫描为纯函数、tempdir 单测；拖拽需人工验证 |

F3 补充一句：**测试连接触发的 TOFU 确认仅本次信任，不落盘。**
