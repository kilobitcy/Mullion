# 切片 A — App 外壳 + 会话管理 设计

> 状态：已定稿（2026-07-25，brainstorming 通过）
> 关联 spec 编号：F70（凭据加密存储）、F71（可选主密码，部分预留）、F21/F12/F4… 的菜单外壳；
> 触碰 F34/T4（上下栏扣除后 reflow）、T1/T5/T6（输入分流不得破坏 keymap）、T3/T7（帧率节流不被 egui 破坏）。
> 里程碑：把产品从「只能 `mullion user@host` 直连」升级为「有主窗体 + 会话管理的可用外壳」。

这一片把 Mullion 从「一条命令直连单 pane」变成一个**能无参启动、管理已存会话、双击登录**的
桌面客户端外壳：新增持久化层，给 winit+wgpu 应用加上 egui 画的菜单栏 / 状态栏 / 会话 CRUD 弹窗，
并把连接流程统一成异步通道。终端网格仍是既有自绘 wgpu，egui 只画「外壳」。

---

## 1. 范围

**做**
- 两种启动：`mullion user@host -p PORT -i KEY`（直连，等价现有行为）与 `mullion`（无参 → 主窗体 + 会话管理器）。
- 新增 `mullion-store` crate：`SessionRecord` 数据模型 + TOML 持久化 + 敏感字段加密（F70）。
- 会话 CRUD：新建 / 编辑 / 删除 / 双击连接；一次配置认证、之后免追问（「一直使用」）。
- 主窗体外壳（egui）：菜单栏（对话 / 分屏 / 配置 / 关于）+ 状态栏。
- `App` 状态机改造：`Option<Connection>`，launcher 态 ↔ 终端态。
- 统一异步 connect 通道：CLI 直连与双击直连共用。
- F70 无头守护测试：磁盘字节搜不到明文口令。

**不做（留后续切片，超出先问）**
- 分屏 / 多 pane（切片 B，F30–F35）——菜单项占位 disabled。
- SFTP 连接行为（切片 D，F50）——`protocol` 字段可存/可编辑，双击 sftp 会话仅提示「未实现」。
- 终端打磨（切片 C：DECCKM / F21 字体可配 / F16 CJK / F12 差分）——「配置」菜单先给空壳。
- **TOFU 主机密钥持久化 + 变更弹窗（F3）**——切片 A 沿用现有内存态 TOFU（自动接受首次），F3 单列专项。
- F71 主密码完整实现——只预留密钥分层结构，Argon2id 到 F71 才引入（见 §3）。
- `~/.ssh/config` 导入（F2）、代理 / 跳板（F4/F5）。

---

## 2. 架构与 crate 布局

### 2.1 新增 `mullion-store` crate

第 5 个 crate，**无头、可纯单测**（仅同步 IO，零 UI/GPU/async）。依赖方向仍严格单向：

```
app → {core, term, ssh, store}     其余四者互不依赖
```

- `store` 拥有 `SessionRecord`、TOML 读写、敏感字段加密；**不依赖** core/term/ssh。
- **app 做整合者**：双击会话时，app 把 `SessionRecord` 映射成 `mullion_ssh::SshConfig` 再连接。
  这样 store 不必认识 ssh，ssh 也不用长出「会话库」概念（守住「ssh 只认字节流」）。
- 放独立 crate 而非塞进 app：F70「磁盘字节搜不到明文」这类验收可写成**无头单测**，
  正合本项目「把可测逻辑抽离最难测的 app」的气质。
- ⚠️ 这打破 CLAUDE.md「四个 crate」的记述——属已批准的架构决策，落地时同步更新 CLAUDE.md 架构表 + 写 ADR。

### 2.2 App 状态机改造

- `App` 从恒持有 `ssh`，改为持有 `Option<Connection>`。
  `Connection = { ssh, rx, pane, pacer, limiter }`（把当前 App 里这几项收进去）。
- 事件循环里「排空 rx → feed emu → 回写 PtyWrite(T1)」**只在 `Connection.is_some()` 时跑**；
  帧率 / `WaitUntil` 复位（T3/T7）逻辑保持不变。
- egui 层**恒绘**（菜单栏 / 状态栏 / 模态弹窗），与 `Connection` 是否存在无关。

---

## 3. 数据模型与持久化

### 3.1 `SessionRecord`（`serde` 可序列化）

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | `u64` | 稳定主键，新建时取现有 `max+1`（不引随机） |
| `name` | `String` | 名称 |
| `host` | `String` | 主机 |
| `port` | `u16` | 端口，默认 22 |
| `protocol` | `Protocol`（`Ssh` \| `Sftp`） | 协议 |
| `user` | `String` | 用户名 |
| `note` | `String` | 备注（可空） |
| `modified_at` | RFC3339 `String` | 修改时间，新建/编辑时用 `SystemTime` 打戳 |
| `auth` | `AuthKind` | `Password` \| `PublicKey { path: PathBuf, has_passphrase: bool }` |

`AuthKind` 只存**非敏感**部分（公钥 path 明文、是否有口令的布尔）；真正的密码串 / 私钥口令走加密侧车。

⚠️ **id 完整性**：`max+1` 会复用刚删掉的顶部 id，若旧密文没随删除清掉，新会话会撞上旧密文。
因此**删除会话必须连带清 secrets.enc 里该 id 的条目**；保存 / 删除后断言 TOML 与 secrets.enc 的 id 集合一致
（守护测试覆盖）。若嫌复用烦，可改**持久化单调计数器**——本片先用 max+1 + 强制清密文。

### 3.2 磁盘布局

config dir 靠 `directories` crate（Windows `%APPDATA%\mullion\`、Linux `~/.config/mullion/`）。

- `sessions.toml` —— 会话数组，只含非敏感字段。可 diff、可手改（守 ADR-002）。
- `secrets.enc` —— 单个加密 blob = `XChaCha20-Poly1305( map<id → {password?/passphrase?}> )`，24 字节随机 nonce，opaque。
- **存**：两文件各自 **tmp + rename 原子写**（防写到一半崩溃导致 sessions.toml 与 secrets.enc desync）。
- **读**：解析 TOML + 解密 secrets.enc，按 `id` 合并；**容忍「有会话无密文」**（标记为需重配认证，不 panic）。

### 3.3 加密与密钥

- 主密钥：32 字节随机，首次生成后存进 **OS keyring**（service=`mullion`）。用它做 AEAD 加密 `secrets.enc`。
- 「一次配置，一直使用」= 双击直连不再追问口令 ⇒ 解密密钥必须能自动拿到 ⇒ keyring 存主密钥（本地，
  不违反 F72 无遥测）。
- **主密钥来源抽成 trait**（如 `MasterKeySource`）：真实实现走 keyring，测试实现走内存 —— 让加密逻辑
  在无头 CI 可测（keyring 在无头环境常不可用）。
- **F70 的 Argon2id 细节（已认可的再解读）**：Argon2id 是「从口令派生密钥」，只在 F71 设了主密码时才有输入。
  切片 A（默认无主密码）用 keyring 高熵随机主密钥 + XChaCha20-Poly1305 即满足 F70 的 P0「磁盘搜不到明文」；
  **Argon2id 留到 F71 主密码层再引入**（届时用它派生/包裹主密钥）。
- ⚠️ 新依赖：`keyring`（OS 密钥库）。已批准。
- **待定 G（keyring 运行时不可用的兜底）**：keyring 是解密承重件，真机上缺 Secret Service 等会整个失效。
  失败行为需定：报可操作错误 / 回退 F71 主密码提示。Windows-first 下 DPAPI 可靠，低优;实现时给一条明确失败路径,不静默吞。

### 3.4 store 引入的依赖

`serde` / `toml` / `chacha20poly1305` / `keyring` / `directories` / `getrandom`（F71 时再加 `argon2`）。
体积均小，N6（安装包 < 25MB）无压力。

---

## 4. UI —— 主窗体外壳 + 会话管理弹窗

### 4.1 egui 集成

- 新增 `egui` / `egui-wgpu` / `egui-winit`，全在 mullion-app 内（守「app 是唯一知道 UI 的地方」）。
- **每帧顺序有数据依赖，不能颠倒**：`egui.run()` 先出 panel 布局 → 取中央区 rect → 终端按该 rect
  设 scissor 画进去 → egui `Renderer::render` 叠在上层。即「GPU 绘制先终端后 egui」，但**终端的视口 rect
  必须取自本帧 egui 的布局结果**（不能用上一帧，否则 reflow 慢一帧、rows 偶发差一）。
- winit 事件先过 `egui-winit`，但路由**不只信 `consumed`**（见 §4.5）。

### 4.2 单窗口布局

```
┌───────────────────────────────────────────────┐
│ 对话  分屏  配置  关于               ← 菜单栏   │  egui top panel
├───────────────────────────────────────────────┤
│                                               │
│   终端网格(已连接，自绘 wgpu)                  │  中央区 = 窗口减去上下栏
│   / 启动器空态(未连接)                        │
│                                               │
├───────────────────────────────────────────────┤
│ ● 已连接 user@192.0.2.10  80×24      ← 状态栏  │  egui bottom panel
└───────────────────────────────────────────────┘
```

⚠️ **关键**：上下栏吃掉垂直空间 → 终端 rows 必须按**扣除后**的中央区 rect 计算，
reflow / `window_change` 也用这个 rect（直接关联 F34/T4）。该 rect 取自**本帧** egui 布局（见 §4.1 的顺序）；
「rect → cols/rows」抽成纯函数单测。

### 4.3 会话管理弹窗

- 无参启动即自动弹出（满足请求 1.2）。
- 列表列：名称 / 主机:端口 / 协议 / 用户 / 修改时间。按钮 `新建` `编辑` `删除`。**双击行 → 连接**。
- 编辑子弹窗（增/改共用）：名称、主机、端口、协议（下拉 ssh|sftp）、用户名、备注、认证方式：
  - 密码 → 掩码密码框；公钥 → 私钥 path（`rfd` 选文件）+ 可选口令框。
  - 保存 → 非敏感写 TOML、密码/口令加密写 secrets.enc、打 `modified_at` 戳。
- 删除 → 确认框 → 从 store 删、**连带清 secrets.enc 里该 id 的密文**、原子重写两文件（见 §3.1 id 完整性、§3.2 原子写）。
- 新依赖：`rfd`（原生文件选择框）。

### 4.4 菜单栏 / 状态栏（切片 A 行为）

- **对话**：打开会话管理器 / 断开 / 退出。
- **分屏**：占位 **disabled**（切片 B 才实现，tooltip 说明）。
- **配置**：打开设置弹窗外壳（字体 F21 等属切片 C，先放空壳/最小页）。
- **关于**：名称 / 版本 / 仓库。
- **状态栏**：未连接 `● 未连接`；已连接 `● 已连接 user@host  cols×rows`。

### 4.5 输入分流（守 T 系陷阱）

egui 的 `consumed` 在「无控件聚焦」时不足以保证方向键 / 快捷键回到终端——顶栏 / 菜单可能间歇抢键，
正好踩 T5/T6。因此**不只信 `consumed`**：

- 引入**显式「终端是否聚焦」状态**(`terminal_focused`)：无模态、焦点在中央区时为真。
- 路由决策抽成**纯函数**：`route(modal_open, terminal_focused, egui_wants_kbd, event_kind) → Egui | Terminal`。
  - 有模态 / egui 控件真正持有键盘焦点 → `Egui`。
  - 否则（终端聚焦）→ `Terminal`：键盘走现有 `keymap`+PtyWrite、鼠标走 SGR 上报（Shift 逃生门 T5）原路不动。
- 该纯函数进无头单测（见 §6.1）——这是全片最高危逻辑，不靠玄学、不整段丢给人工验证。
- 剩余「egui 与终端焦点争用的真实观感」仍需人工确认(见 §6.3)。

---

## 5. 连接流程与两种启动

**统一走「开窗 → App 内异步发起连接」**（现有 `main.rs` 是开窗前 `block_on(connect)`，
启动器态下行不通——还没选会话）：

- `main.rs` 不再预连：先建 `EventLoop` + `App`，把「初始意图」交给 App。
- 连接在 runtime 上 `spawn`（不阻塞事件循环），结果经**新增 `UserEvent` 变体**回送：
  `ConnectOk(SshSession, rx)` / `ConnectErr(SshError)`。App 在 `user_event` 里填充 `Connection` 或报错。
- **CLI 直连与双击直连从此共用同一条 connect 通道**。

两条路径：
- ① `mullion user@host -p -i`：`cli::parse_args` → `SshConfig` → App 启动即发起连接、进终端态。
- ② 无参 `mullion`：App 启动即弹会话管理器，`Connection=None`。双击 → spawn 一个 async 任务，
  **在该任务内**（不在 UI 线程）读 secrets.enc + 调 keyring 解密 → 组 `SshConfig` → connect
  → 成功填充 Connection、关弹窗、切终端；失败按 F6 分类（DNS/拒连/认证/主机密钥）在状态栏或弹窗给可操作错误。
  （解密要读文件 + 调 keyring，Linux 的 Secret Service/D-Bus 可能阻塞甚至弹系统框，故必须离开 UI 线程。）

**认证时机**：密码 / 私钥口令在编辑弹窗里一次配置即加密入库；双击时直接解密用、不再追问。
CLI 直连沿用现有（agent 或无口令 -i key），不在切片 A 扩展。

**SFTP 边界**：`protocol` 可存/可编辑为 `sftp`，但连接行为是 F50 / 切片 D；切片 A 双击 sftp 会话
仅提示「SFTP 未实现（切片 D）」或该项 disabled，只有 ssh 会话真正连。

**TOFU**：切片 A 沿用现有内存态 `KnownHosts`（双击连接复用同一 policy 回调）；F3 持久化 + 变更弹窗留后续专项。

**待定 F（CLI 直连失败的退出码）**：统一成 in-window 异步连后，`mullion user@host` 连不上会开窗再报错、
丢了现有 `exit(1)` 的可脚本化语义。**推荐默认**:CLI 直连(路径 ①)失败仍走 stderr + `exit(1)`,
只有无参启动器(路径 ②)的失败才 in-window 报错。实现时确认。

---

## 6. 测试策略

### 6.1 无头可测（TDD 先行）

- **`mullion-store` 全套**：`SessionRecord` serde round-trip；Vault CRUD（增/改/删 → 读回一致）；
  id 分配（max+1）、`modified_at` 更新;加解密 round-trip（加密→解密=原文）。
- **F70 守护测试**：写带密码会话 → 断言 `sessions.toml` 与 `secrets.enc` 原始字节搜不到明文口令。
- **主密钥 trait 内存实现** 让上述加密测试在无头 CI 可跑。
- **app 纯件**：`SessionRecord → SshConfig` 映射；「中央区 rect → cols/rows」（扣除上下栏后，护 F34/T4）;
  **输入路由决策纯函数** `route(modal_open, terminal_focused, egui_wants_kbd, event_kind)`（§4.5，全片最高危逻辑）。
- **id 完整性守护测试**：保存 / 删除后，`sessions.toml` 与 `secrets.enc` 的 id 集合一致（§3.1）。

### 6.2 必须仍绿的既有测试

keymap 全套（T1/T5/T6）、frame（T3/T7）——输入分流与 egui 集成**不得**破坏它们。

### 6.3 无头测不了 · 写进 PR 人工验证清单

1. egui 与终端同帧渲染正确、不撕裂。
2. egui repaint 请求不破坏帧率节流 + `WaitUntil` 复位（空闲 CPU 不忙转，T3/T7）。
3. 输入分流：终端聚焦时方向键 / 快捷键不被 egui 吞;弹窗聚焦时按键不漏到终端。
4. 双击 → 真实 SSH → 终端出画面，端到端。
5. 会话 CRUD 弹窗交互手感。
6. 上下栏扣除后 reflow 列数正确（远端 tmux 排版对）。

### 6.4 TDD 顺序

store 全套无头测先行 → 两个 app 纯件（映射 + rect→cols/rows）→ 再接 egui / 事件循环（落到人工验证层）。

---

## 7. 新依赖汇总（供 review）

| crate | 用途 | 位置 |
|---|---|---|
| `serde` / `toml` | 会话 TOML 持久化 | store |
| `chacha20poly1305` | secrets.enc AEAD 加密（F70） | store |
| `keyring` | OS 密钥库存主密钥 | store |
| `directories` | 跨平台 config dir | store |
| `getrandom` | 随机主密钥 / nonce | store |
| `argon2` | F71 主密码派生（预留，本片不引） | store（后续） |
| `egui` / `egui-wgpu` / `egui-winit` | 菜单栏 / 状态栏 / 弹窗 | app |
| `rfd` | 原生文件选择框（选私钥） | app |

---

## 8. 落地时的文档动作

- 更新 `CLAUDE.md` 架构表:四 crate → 五 crate（加 `mullion-store`），补一行职责。
- 新增 ADR:`docs/adr-006-session-store-and-egui-chrome.md`——记「新增 store crate、egui 做外壳、
  keyring 存主密钥、F70 的 Argon2id 推迟到 F71」及各自被否的备选。
