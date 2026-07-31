# 切片 P0-b：代理 + 跳板 + 最小 UI 设计

> 上游：`docs/superpowers/specs/2026-07-30-session-management-roadmap-design.md` §4.4
> 前置切片：P0-a（`2026-07-30-slice-p0a-store-foundation.md`，已发布 v0.1.14）
> 覆盖 spec 编号：**F4**（SOCKS5 / HTTP CONNECT 代理，含认证）、**F5**（跳板机 ProxyJump）、
> **F60**（分组，本期补上 P0-a 缺失的 UI 入口）

---

## 1. 目标与范围

让一条会话可以经**网络代理**、经**跳板机**、或两者叠加地连上目标主机，
并把 P0-a 已经写好但没有任何入口的分组功能接上 UI。

**在范围内**：

- `SshConfig` 承载拨号计划；`establish()` 的 TCP 直连换成可插拔拨号
- SOCKS5（RFC 1928）与 HTTP CONNECT 两种代理，含用户名/口令认证（RFC 1929）
- 跳板链，跳板本身可位于代理之后；跳板机独立走 F3/TOFU 校验
- 会话编辑器新增代理栏、跳板栏、分组下拉；会话管理器按分组折叠；极简分组增删改

**明确不在范围内**（越界需求先问，别自己开工）：

- 自定义 `ProxyCommand`（已登记进路线图 §8 的 P4 储备区）
- 跳板机内联填写主机与凭据（见决策 D2）
- 分组的图标 / 配色 / 拖拽排序（P2-a）
- 自动重连时的拨号重放（P3-a）

---

## 2. 已定决策

| # | 决策 | 理由 |
|---|---|---|
| D1 | SOCKS5 与 HTTP CONNECT **手写**，零新依赖 | 两者合计约 210 行，都只是「往 stream 写几个字节、读回几个字节」。spec.md 对 F4 的验收本来就写的是「对本地 mock 代理的集成测试」——手写版能对假代理逐字节断言握手报文，比包一层库测得更严。交叉编译零风险，N6（安装包 < 25MB）不受影响 |
| D2 | 跳板**只能引用已有会话**，不支持内联填写 | 引用变体的凭据已在被引用会话的 `secrets.enc` 里，**键空间与孤儿裁剪逻辑一行不改**；UI 就是一个下拉框；改一处跳板密码，所有引用处同时生效。代价是纯中转机也要先建一条会话条目——可接受，用户本来也可能想直连它排障 |
| D3 | 跳板链**整条 Override**，绝不与分组的链拼接 | `jump` 内层是 `Vec`，但语义上是**复合对象**，适用路线图 §4.1 的「整体继承或整体覆盖」而非列表 Merge。Merge 会把分组的链和会话的链拼成一条顺序无意义的乱链，直接连到错误的机器。因此它落在**可继承分节** `NetworkPrefs` 里，而非永不可继承的 `Connection`（§3.1） |
| D4 | 代理口令**只存会话级** | `SecretEntry` 加一个 `proxy_password` 字段即可，`secrets.enc` 的键空间、孤儿裁剪、`delete_group` 全不动。这三处正是 P0-a 刚改过、真实数据迁移**尚未人工验收**的部分，不叠加风险。代价：一整组机器共用带认证代理时，口令要逐会话填 |
| D5 | 拨号器分**纯函数选路 + async 执行**两阶段 | 路线图给 P0-b 写的验收第一条就是「拨号器选择的纯函数单测」。两阶段让选路逻辑 100% 脱离网络可测，且「仅代理 / 仅跳板 / 代理+跳板嵌套」是同一条代码路径，不需要特例分支 |
| D6 | UI 含**极简分组管理**（新建/重命名/删除） | P0-a 只做了分组的数据层与 CRUD，没有任何创建入口。若本期只加一个分组下拉，它永远是空的，D3 定的跳板继承也无从人工验证 |

---

## 3. 数据模型

### 3.1 store 侧：新开一个**可继承分节**，不动 `Connection`

代理与跳板**不能**放进 `Connection`。理由有两条，任一条都是决定性的：

1. `Connection` 的既有 doc 写死「**永不可继承**——连接目标是会话身份本身」（`model.rs:43`）
2. `GroupRecord` 只持有可继承字段，**根本没有 `connection` 字段**（`group.rs:10-20`）。
   放进去，D3「分组可继承跳板链」在结构上无法实现

P0-a 已立的模式是「**分节 = 继承单位**」：`Connection`/`Auth` 不可继承，
`TerminalPrefs`/`AppearancePrefs` 可继承。代理与跳板显然属于后者——
一整组机器共用同一台堡垒机正是分组存在的理由。所以新开第三个可继承分节：

```rust
/// 网络路径偏好(可继承分节)。**与 Connection 分开**:Connection 是
/// 「会话身份本身」永不可继承,而「怎么走到它」恰恰最该按分组继承。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkPrefs {
    /// F4 网络代理。`None` = 继承上游;`Some(Direct)` = 显式不走代理。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyChoice>,
    /// F5 跳板链,按拨号顺序:[0] 最先连。
    /// `None` = 继承上游;`Some(vec![])` = 显式直连。
    /// **整条链 Override**(D3),绝不与分组的链拼接——它类型上是 Vec,
    /// 语义上是复合对象,不适用 §4.1 的列表 Merge 规则。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jump: Option<Vec<JumpRef>>,
}

/// 代理选择。`Direct` 是一个**显式**取值,不是「没配」——
/// 「没配」由外层 `Option::None` 表达(见 §3.2)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProxyChoice {
    /// 显式直连:覆盖分组的代理设置。
    Direct,
    Socks5(ProxyEndpoint),
    HttpConnect(ProxyEndpoint),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyEndpoint {
    pub host: String,
    pub port: u16,
    /// 代理认证用户名;口令在本会话的 SecretEntry.proxy_password(D4)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}

/// 只引用,不内联(D2)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct JumpRef(pub SessionId);
```

挂载点三处，与既有两个可继承分节完全对称：

```rust
// SessionRecord
#[serde(default)]
pub network: NetworkPrefs,

// GroupRecord —— 这是 D3 得以成立的地方
#[serde(default)]
pub network: NetworkPrefs,

// PrefsLayer trait + 两个 impl
fn network(&self) -> &NetworkPrefs;
```

`SecretEntry` 增一个字段，与既有两个同样的写法：

```rust
#[serde(skip_serializing_if = "Option::is_none", default)]
pub proxy_password: Option<String>,
```

### 3.2 「未设置」与「显式设为空」必须可区分

`inherit.rs:100-102` 留了一条明确警告：当前模型把「本层未设（继承）」与
「本层显式设为空」折叠成同一个 `None`，若将来给某个复合字段加
「**显式清空、不再继承**」的语义，`resolve` 里的 `.map(Some)` 技巧**不能照抄**。

代理与跳板恰恰是第一个撞上这条警告的场景：分组配了堡垒机，
组里某台机器需要**显式直连**。若没有这个区分，用户只能把这台机器移出分组。

解法就是上面的两个 `Option` 语义：

| 写法 | 含义 |
|---|---|
| `proxy: None` | 继承上游 |
| `proxy: Some(Direct)` | 显式不走代理，覆盖分组 |
| `proxy: Some(Socks5(ep))` | 走这个代理 |
| `jump: None` | 继承上游 |
| `jump: Some(vec![])` | 显式直连，覆盖分组的链 |
| `jump: Some(vec![a, b])` | 走这条链 |

`ResolvedConfig` 相应增两个字段：`proxy: Option<ProxyChoice>` 与
`jump: Vec<JumpRef>`（解析完成后不再需要区分，扁平化成实际要走的链）。
解析走 `resolve_override`（**不是** `resolve_merge_list`），
外层 `Option` 天然表达「本层未设则看下一层」，无需 `.map(Some)` 技巧。

### 3.3 schema 升 v3

新增字段全部带 `#[serde(default)]`，**v3 结构可直接读 v2 文件**——不需要 P0-a 那种
`SessionRecordV1` 影子结构，迁移函数体基本为空（只改写 `schema_version`）。

那为什么还要升版本？因为 serde 默认**忽略未知字段**：不升版本的话，v0.1.14 读到一份含
代理配置的文件会静默丢弃整个 `[session.network]` 分节，保存时写回一份没有它的文件——
用户的代理与跳板配置无声消失。升到 v3 后，P0-a 已实现的「拒绝未来版本 schema」
会让旧客户端明确报错。

`CURRENT_SCHEMA` 是单一常量源（`model.rs:144`），只改这一处。

### 3.4 ssh 侧（`mullion-ssh/src/dial.rs`，新文件）

```rust
/// 拨号链上的一跳。**完全物化**:不含任何 store 类型(红线 2)。
pub enum Hop {
    Socks5 { host: String, port: u16, auth: Option<(String, String)> },
    HttpConnect { host: String, port: u16, auth: Option<(String, String)> },
    SshJump { host: String, port: u16, user: String, auth: AuthMethod },
}
```

**`Hop` 手写 `Debug` 并对口令打码，绝不 `derive`。** 它携带物化后的明文凭据，
一旦被 `{:?}` 打进 ADR-008 的日志就是明文泄露。

`SshConfig` 增一个字段承载计划，目标仍用既有的 `host`/`port`，不引入第二个真源：

```rust
/// 拨号链。空 = 直连(既有行为)。按顺序穿过后到达 host:port。
pub hops: Vec<Hop>,
```

---

## 4. 三层分工

红线 2（`mullion-ssh` 禁止出现任何 store 类型）不是靠代码审查守住的，
而是靠**依赖树里根本没有 `mullion-store`** 守住的。分工如下：

| 层 | 文件 | 职责 | 可测性 |
|---|---|---|---|
| store | `mullion-store/src/jump.rs`（新建） | 跳板**引用图**解析：递归展开 + 环检测 + 深度上限。输入 `SessionId`，输出 `Vec<SessionId>` | 纯函数，零 IO，纯单测 |
| app | `mullion-app/src/shell/dial_plan.rs`（新建） | **物化**：把 `ResolvedConfig` 的 `jump` 链与 `proxy` 翻译成 `Vec<Hop>`，从 vault 取口令 | 同步纯函数，纯单测 |
| ssh | `mullion-ssh/src/dial.rs`（新建） | **执行**：逐跳拨号，产出 stream 与保活句柄 | 对 mock 代理集成测试 |

只有 app 同时认识 store 与 ssh —— 这正是架构不变量里 app 的定位。

### 4.1 跳板引用图的展开规则

目标会话 A 的 `jump = [B]`，而 B 自己也配了 `jump = [C]`，则实际拨号顺序是
先连 C、经 C 连 B、经 B 连 A。函数返回的是**跳板链**（不含目标）：`[C, B]`。
与 OpenSSH 的 `ProxyJump` 递归行为一致。

三条必须写死的规则：

- **递归时用被引用会话的「解析后」链，不是它的原始字段。** 若 B 自己没配 `jump`
  但属于某个配了堡垒机的分组，展开 B 时要连同那条继承来的链一起展开——
  否则「直接连 B」与「经 B 连 A」走的路径不同，用户无从预期
- **代理不递归。** 只有目标会话（或其继承来的）的 `proxy` 生效，且它作用于
  **整条链的第一跳**；中间跳板会话自己的 `proxy` 被忽略。理由：代理是
  「**本机**怎么出网」的属性，跳板机不是本机，它的代理设置对我们无意义
- **深度上限 8 跳**（不含目标）。超出返回 `JumpTooDeep`
- **环检测**：展开过程中维护已访问集合（含目标 A 自身），命中即返回
  `JumpCycle { chain }`，`chain` 带完整路径供 UI 展示，让用户知道环在哪
- **悬空引用报错**，见 §6

---

## 5. 拨号执行与 Handle 保活

### 5.1 执行流程

`establish()` 的第 2 步（`TcpStream::connect`）替换为：

1. 若 `hops` 为空 → 既有的 `TcpStream::connect` + `set_nodelay(true)`，行为完全不变
2. 否则逐跳处理，每跳把上一跳产出的 stream 作为输入：
   - `Socks5` / `HttpConnect`：在当前 stream 上跑握手，成功后**同一个 stream** 继续用
   - `SshJump`：用当前 stream 调 `client::connect_stream` 建一条 SSH 连接 → 认证 →
     `channel_open_direct_tcpip(下一跳的 host, port, "127.0.0.1", 0)` →
     `into_stream()` 得到新 stream
3. 最终 stream 交给既有的 `client::connect_stream` + `authenticate`，后半段一个字不改

第一跳的输入是对**第一跳自己的地址**做 `TcpStream::connect`。因此「跳板在代理之后」
（spec F5 明确要求）不是特例：它就是 `hops = [Socks5{...}, SshJump{...}]`。

已核实的 russh 0.54.5 签名（非记忆）：

- `connect_stream<H, R>(config, stream, handler)` 的约束仅 `R: AsyncRead + AsyncWrite + Unpin + Send + 'static`
- `Handle::channel_open_direct_tcpip(host, port, originator_addr, originator_port)`（`client/mod.rs:649`）
- `Channel::into_stream() -> ChannelStream<S>`，`ChannelStream` 实现 `AsyncRead`/`AsyncWrite`

### 5.2 Handle 保活（本切片最危险的一处）

`ChannelStream` 的字段只有 `ChannelTx` 与 `ChannelRx`，**不持有 `Handle`**（已读源码确认）。
而 `Handle` 一 `Drop` 整条 SSH 连接立即断开（B2-a 时已踩过这个性质，
`session.rs:248-250` 有注释记录）。

若把跳板的 `Handle` 留在 `dial()` 的栈上，函数返回即 Drop → 拨号会**成功**，
连接会在几毫秒后无故断开。本地直连路径测不出，只在真机跳板路径偶发。

所以 `establish()` 不再返回裸 `Handle`：

```rust
/// 一条已建立的 SSH 连接。多个 pane 共享同一个 Arc(adr-009)。
pub struct SshConnection {
    handle: Handle<ClientHandler>,
    /// 跳板链每一跳的连接,仅用于保活,顺序 = 拨号顺序。
    /// 不是可有可无的簿记:任一跳 Drop,其上承载的 direct-tcpip 立刻断,
    /// 表现为「连上了,几毫秒后无故断线」。
    _jumps: Vec<Handle<ClientHandler>>,
}
```

`open_pty` 改收 `Arc<SshConnection>`，保活由类型强制，app 侧漏不掉。
**红线 1 仍然成立**：`open_pty` 的签名里依然没有任何网络参数。

app 侧改动是纯类型替换，共 5 处：`workspace/mod.rs:58`、`app.rs:46`、
`app.rs:542`、`app.rs:549`、`app.rs:589`。

### 5.3 跳板机的主机密钥校验

**每一跳独立走 F3/TOFU**，用与目标主机同一个 `HostKeyPolicy`。
连一条新配置的两跳会话可能连弹三次确认窗（与 OpenSSH 行为一致）。

这条不可省。跳板机是流量必经之处，若跳过它的密钥校验，
中间人只需攻陷跳板机的身份即可，F3 就形同虚设。

---

## 6. 错误处理

链路现在有三段，失败必须能一眼定位到段，否则用户面对
`Io("connection reset")` 无从下手。`ConnectError` 新增：

```rust
/// 连不上代理本身 —— 与「连不上目标」是两回事,该查的东西不同。
ProxyUnreachable { host: String, port: u16, cause: String },
/// 代理认证被拒 —— 与 SSH 认证被拒是两回事。
ProxyAuthFailed,
/// 代理接受了连接但拒绝转发。
ProxyRejected { detail: String },
/// 第 hop 跳失败,嵌套真实原因。
JumpFailed { hop: usize, host: String, cause: Box<ConnectError> },
```

`ProxyRejected` 的 `detail`：SOCKS5 把 REP 码逐个译成中文
（`0x02` 规则不允许 / `0x03` 网络不可达 / `0x04` 主机不可达 / `0x05` 连接被拒 /
`0x06` TTL 超时 / `0x07` 命令不支持 / `0x08` 地址类型不支持）；
HTTP CONNECT 把非 2xx 的状态行原样带出。

`JumpFailed` 的嵌套是关键：用户要能看到「**第 2 跳 bastion-b 的主机密钥变了**」，
而不是一句笼统的传输错误。

`StoreError` 新增三个，均由 `jump.rs` 的引用图解析产生：

```rust
JumpCycle { chain: Vec<SessionId> },
JumpTooDeep { max: usize },
JumpDangling { id: SessionId },
```

**与 P0-a 的一处刻意区别**：`resolve_for` 对悬空 `group_id` 是**静默降级**为「无分组」。
跳板悬空**必须报错，不许沿用这个先例**——静默降级意味着悄悄改走直连，
用户以为流量过了堡垒机、实际没有。这是安全属性，不是便利属性。

---

## 7. UI（egui，最小但可用）

### 7.1 会话编辑器

- **代理栏**：下拉四项，直接对应 §3.2 的语义表——
  **继承分组**（`None`）/ **直连**（`Some(Direct)`）/ SOCKS5 / HTTP CONNECT。
  后两项才展开 host / port / 用户名 / 口令四个输入框。
  会话未分组时「继承分组」项置灰，因为无上游可继承
- **跳板栏**：一个「继承分组 / 自定义」开关，选「自定义」后是可增删的**有序**列表，
  每项一个会话下拉。空列表 = `Some(vec![])` = 显式直连，UI 上给一句
  「（不经跳板，覆盖分组设置）」把这个状态说明白。
  下拉候选**排除本会话自己**（最常见的环，UI 层就挡掉，但 store 层的环检测
  不因此省略——编辑器不是唯一的写入路径）
- **分组下拉**：无分组 / 各已有分组

### 7.2 会话管理器

按分组折叠成分区；未分组的会话归入一个「未分组」区。

### 7.3 极简分组管理

新建 / 重命名 / 删除，纯文字。删除不级联删会话（P0-a 已实现为把成员的
`group_id` 置 `None`）。图标、配色、拖拽排序均属 P2-a，本期不做。

### 7.4 与 P0-a 遗留物的衔接

`EditorBuffer` 目前有四个 `preserved_*` 透传字段。本期：

- `preserved_group_id` **从「透传」升级为「真正可编辑」**
- `preserved_terminal` / `preserved_appearance` **保持透传**（本期仍无 UI）
- `preserved_tags` 保持透传
- 新分节 `network` 是**新增的可编辑字段**，不进 `preserved_*` 家族

守护测试 `editing_a_session_preserves_fields_the_form_cannot_edit` 必须**同步更新而非删除**：
它验证的是「表单编辑不到的字段不被静默清空」，`group_id` 移出这个集合后，
其余三项的断言仍然要在。P0-a 终审发现的静默清空 bug 就是这个测试挡住的。

---

## 8. 测试策略

| 项 | 手段 |
|---|---|
| 选路正确 | `build_hops` 纯函数单测：直连 / 仅代理 / 仅跳板 / 代理+跳板嵌套 / 递归展开 |
| SOCKS5 握手 | tokio 起 **mock 代理**，逐字节断言：方法协商、用户名口令认证（RFC 1929）、域名 / IPv4 / IPv6 三种地址类型、各 REP 码的错误映射 |
| HTTP CONNECT 握手 | 同上：请求行格式、`Proxy-Authorization: Basic` 头、2xx 与非 2xx 分支 |
| 环检测 | 自引用 / 二元环 / 三元环 / 深度超限 / 悬空引用，五个单测 |
| jump 继承 | 钉死 D3：分组有链、会话也有链时，结果**是会话的链**而非两者拼接 |
| 继承 vs 显式空 | 钉死 §3.2：分组配了堡垒机时，会话 `jump: None` 得到分组的链，`jump: Some(vec![])` 得到空链；`proxy: None` vs `Some(Direct)` 同理 |
| 递归展开 | 钉死 §4.1：被引用会话 B 自己继承自分组的链，展开 A 时要一并展开；中间跳板的 `proxy` 被忽略 |
| 红线 2 | `cargo tree -p mullion-ssh` 不出现 `mullion-store`，**做成测试而非人工检查** |
| 迁移 | v2 → v3 round-trip：旧文件读入后 `proxy` 为 `None`、`jump` 为空，`.bak` 存在 |
| T1 | 动了 SSH 层，重跑 `emulator::tests::pty_write_is_collected` |

### 8.1 建议加做：in-process 两跳集成测试

用 `russh::server` 起一个假 sshd，做一次真实两跳（本机 → 假跳板 → 假目标）。

理由：§5.2 的 Handle 保活 bug 纯单测抓不到，而它恰恰是本切片最危险的一处，
表现为「连上了，几毫秒后无故断」。spec.md 对 F5 的验收列写的也正是「两跳链路集成测试」。

成本与前置核实（**计划阶段必须先验证，不能假定可行**）：
`russh` 在我们的 `default-features = false, features = ["ring","flate2","rsa"]`
配置下，server 模块是否需要额外 feature。仅作 **dev-dependency** 引入，不进发布产物。

---

## 9. 人工验收清单

以下属 CLAUDE.md「你无法验证的东西」，无头环境一律验不了，需在 Windows 11 实机确认：

- 经真实 SOCKS5 代理（如本机 clash 的 7891）连通目标
- 经真实 HTTP CONNECT 代理连通目标
- 经真实堡垒机两跳连通；跳板机与目标机各弹一次 TOFU 确认窗
- 代理不可达 / 代理认证错 / 跳板认证错 三种失败各给出**指向正确那一段**的错误文案
- 分组：新建、改名、删除；删除后成员会话落入「未分组」而非消失
- 分组上配代理与跳板，成员会话不填时自动继承；成员会话自己配了则整条覆盖
- `MULLION_LIVE=1` 对内网真机跑 `mullion-ssh` 的 live 测试

---

## 10. 与架构不变量的关系

- 依赖方向不变：新增的 `jump.rs` 在 store（纯函数、零 async），
  `dial.rs` 在 ssh（不认识 pane，也不认识 store），物化在 app
- `mullion-store` 不新增任何依赖，仍是零 UI / 零 async / 仅同步 IO
- `mullion-ssh` 不新增运行时依赖（D1：代理握手手写）；
  §8.1 若成立，只增 dev-dependency
- 红线 1（`open_pty` 签名无网络参数）与红线 2（ssh 不认识 store）均在 §4、§5.2 保持

---

## 11. 留给后续期的问题

- **P3-a 自动重连**：重连时要重放整条拨号链，`Vec<Hop>` 的物化结果是否缓存、
  被引用的跳板会话在重连前被改了怎么办 —— 本期不解决，但 D5 的两阶段设计
  让「重放」有个明确的重放对象
- **P2-a 分组 UI**：图标 / 配色 / 排序接上时，`preserved_terminal` /
  `preserved_appearance` 两个透传字段才会退休
- **代理口令的分组级共享**：D4 有意限制在会话级。若将来真出现企业场景，
  再单独开一期扩展 `secrets.enc` 的键空间，届时孤儿裁剪与 `delete_group` 要一并改
