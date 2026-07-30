# 会话管理五期路线图（P0~P4）设计

日期：2026-07-30
输入：`docs/session-mng.md`（通用 SSH 客户端会话字段清单，13 分组约 150 字段）
产出：把该清单裁剪、编号、排进 P0~P4 五期

---

## 1. 背景与裁剪原则

`docs/session-mng.md` 是一份 Xshell / MobaXterm 风格的**通用** SSH 客户端字段清单。
Mullion 的定位是「Windows 上通过高延迟代理链路操作远端 tmux 里的 Claude Code TUI」，
清单里相当一部分字段服务的是别的产品形态。

**已定原则**：P0~P3 只排服务于主线场景的字段；越界字段不删除也不实现，
统一登记进 **P4 储备区**（写进 `spec.md` 的「已登记未排期」表），
每条注明*为什么现在不做*与*什么条件下重新评估*。边界扩大时从储备区取。

一处澄清：**SFTP 侧栏不属于越界**——`spec.md` 的 `F50~F57` 已把它列为 P1 需求。
越界的只是清单 §10 里的 ZMODEM/XMODEM 与传输模式细节。

## 2. 已定关键决策

| # | 决策 | 理由 |
|---|---|---|
| D1 | 组织形态 = **单层分组**，不做任意深度树 | 服务器数量级在几十台，一级继承够用；树形要处理环检测/移动子树/删除级联，收益不抵成本 |
| D2 | 继承表达 = **`Option<T>`**，`None` 即继承分组 | 「三态」是 UI 概念（灰显继承值 / 覆盖 / 恢复继承），数据层只需两态 + 内置默认兜底 |
| D3 | P0 **同期做地基与代理**，且拆 P0-a / P0-b 两切片 | 继承一旦晚引入，所有已加字段要从 `T` 改成 `Option<T>`，TOML 与全部单测重写 |
| D4 | 自动重连排到 **P3** | 它引入连接状态机，与 adr-009「一条连接多 channel」耦合最重，等分屏彻底稳定再动 |
| D5 | TOML 引入 `schema_version` + 一次性迁移 + `.bak` 备份 | 这是唯一会动用户已有数据的改动 |

## 3. 编号规划

避开已用号段（F1-6 / F10-21 / F30-38 / F50-57 / F70-72 / F80-84；F85 自绘标题栏已否决）。

| 号段 | 语义 | 期 |
|---|---|---|
| `F60~F69` | 会话组织与防误操作（分组/图标/色板/标签/环境等级/只读/确认） | P0 起（F60 在 P0），主体 P2 |
| `F40~F44` | 登录后自动化（tmux attach / 命令 / 目录 / env） | P1 |
| `F7~F9` | 连接韧性（超时 / 重连 / keepalive） | P3 |
| `F22~F29` | 终端行为参数（TERM / 编码 / 键序列 / 光标 / 铃声 / 鼠标） | P3 |
| `F86~F89` | 会话级外观覆盖 | P3 |

`F4`（代理）、`F5`（跳板）沿用 spec 已有编号，在 P0 落地。

---

## 4. P0 — 数据地基 + 链路打通

一次把 TOML 格式定死，后续四期**只加字段、不改类型**。

### P0-a：store 地基（纯单测，不碰 GPU / SSH）

- `SessionRecord` 由扁平结构拆成分节嵌套：
  `identity` / `connection` / `auth` / `automation` / `terminal` / `appearance` / `safety`。
  后续每期往对应节里加字段，不再动顶层结构。
- 新增 `GroupRecord`（单层，字段：`id` / `name` / 可继承的默认值集合），
  `SessionRecord` 增 `group_id: Option<GroupId>`。（**F60**）
- 可继承字段一律 `Option<T>`。新增纯函数
  `resolve(session, groups, defaults) -> ResolvedConfig`：
  会话 `Some` → 分组 `Some` → 内置默认，三级 fallback。
- `schema_version` + 一次性迁移：旧扁平 `[[session]]` → 新格式，迁移前写 `sessions.toml.bak`。
- `secrets.enc` 结构不变（F70 不受影响）。

**验收**
- `store::resolve` 三级 fallback 单测：会话覆盖 / 会话未设→取分组 / 分组未设→取默认。
- 迁移 round-trip 单测：喂入 v0.1.12 格式的 `sessions.toml`，迁移后逐字段等价，且 `.bak` 存在。
- 删除分组时归属该组的会话 `group_id` 置 `None`（不级联删会话）单测。
- `tests/f70_no_plaintext.rs` 继续绿（迁移不得把口令写进明文文件）。

**架构约束**：`resolve` 是纯函数，留在 `mullion-store`（零 UI / 零 async / 仅同步 IO）。

### P0-b：代理 + 跳板 + 最小 UI

- `SshConfig` 增 `proxy: Option<ProxyConfig>`（SOCKS5 / HTTP CONNECT，含认证）
  与 `jump: Vec<JumpHost>`。（**F4** / **F5**）
- `establish()` 中把 `TcpStream::connect` 换成可插拔拨号器：直连 / 经代理 / 经跳板。
  跳板机本身可位于代理之后（spec F5 明确要求）。
- 跳板可引用已有会话 id 以复用其凭据 → **必须做环检测**（A 跳 B、B 跳 A）。
  单层分组不会有环，跳板链会，环检测因此归属这里。
- app 侧最小改动：编辑器表单加「分组」下拉 + 「代理」「跳板」两栏；
  会话管理器列表按分组折叠。

**验收**
- 拨号器选择的纯函数单测（给定 config → 选出直连/代理/跳板链）。
- 跳板链环检测单测（自引用、二元环、三元环）。
- `MULLION_LIVE=1` 经真实 SOCKS5 连内网真机（人工跑，无头 CI 跑不了）。
- 人工验收：Windows 实机经代理链路连通。

**风险 / 守护**
- 改 `SshConfig` 波及 `session_map.rs`、`app.rs` 连接路径、以及 `open_pty()`。
  **`open_pty()` 签名故意不含任何网络参数（adr-009，防误重连），
  代理/跳板逻辑只能在 `establish()` 里，绝不能漏进 `open_pty()`。**
- 动 SSH 层后重跑 `emulator::tests::pty_write_is_collected`（T1，已升级为 per-pane）。

---

## 5. P1 — 登录后自动化（F40~F44）

- **F40** tmux 自动 attach：`tmux new-session -A -s <name>` 语义（不存在则建），可开关
- **F41** 登录后命令列表（逐行发送，每行可设延时）
- **F42** 初始工作目录
- **F43** 环境变量注入（优先 SSH env 请求，服务端拒绝则回退到 `export` 行）
- **F44** 自动化整体开关（便于临时跳过，排障用）

**两个必须在实现计划阶段定死的问题**
1. **「登录完成」判定**：匹配 shell 提示符不可靠（提示符形态千差万别）。
   采用「首次收到 PTY 输出 + 可配延时」。
2. **分屏语义**：一条连接开 N 个 pane（adr-009），每个 PTY channel 各跑一次自动化。
   tmux 会话名是否附 pane 序号，需在计划里给出明确规则
   （倾向：同名 attach，靠 tmux 自身的多客户端 attach 能力，不附序号）。

**验收**
- 自动化脚本生成为纯函数：给定 `ResolvedConfig` → 待发送字节序列 + 延时表，单测覆盖。
- tmux 命令拼装的转义单测（会话名含空格 / 引号）。
- 人工验收：Windows 实机连上即落在远端 tmux 里的 Claude Code。

**守护**：新 pane 建立后仍须发 `window_change`（T4，`app::tests::reflow_emits_resize`）。

---

## 6. P2 — 组织与防误操作（F61~F69）

P0-a 已备好数据层，本期主体是 egui 侧。

- **F61** 图标：内置库（发行版 logo 等）+ emoji；后续可加「首次连接读 `/etc/os-release` 自动建议」
- **F62** 语义色板：6~8 个预设（生产/预发/测试/开发/自定义），带深浅色变体；允许自由 hex 但默认走色板
- **F63** 标签 tags + 搜索 + 收藏 + 排序权重
- **F64** 环境等级标记（开发/测试/预发/生产），与 F62 色板绑定（生产=红）
- **F65** 危险命令二次确认（`rm -rf` / `shutdown` / `reboot` 关键词表，可配）
- **F66** 只读模式（禁止输入）
- **F67** 是否允许被多会话广播/同步输入包含
- **F68** 剪贴板策略：多行粘贴前确认
- **F69** 连接前确认弹窗

颜色作用范围复用 F80~F83 的视觉基线：标签页 / 会话列表条目 / pane 标题条 / 状态栏。
色板取值必须与 `docs/` 里已冻结的 UI 视觉规格同源，不得另立一套色值。

**验收**
- 筛选/排序为纯函数（`filter(sessions, query, tags, env) -> Vec<SessionId>`），单测覆盖。
- 危险命令匹配纯函数单测（含误报边界：`rm -rf` 出现在字符串字面量里）。
- 只读模式下键盘事件不写入 channel 的单测。
- 人工验收：深浅色主题下色板对比度可辨。

---

## 7. P3 — 连接韧性 + 终端/外观可配

- **F7** 连接超时、**F8** 自动重连（重试间隔 / 最大次数）、**F9** keepalive 间隔 + TCP keepalive
- 启动时自动连接 / 随工作区恢复
- **F22** TERM 类型、**F23** 字符编码（UTF-8/GBK/GB18030/Big5…）、
  **F24** Backspace/Delete/Home/End 键序列风格、**F25** 回车 CR/CRLF、
  **F26** 光标形状与闪烁、**F27** 铃声、**F28** 鼠标行为（选中即复制/右键）、
  **F29** 空闲自动断开 + 空闲保活字符
- **F86~F89** 会话级字体/配色/初始尺寸覆盖（**F21** 与 **F84** 设置弹窗在本期落地）
- 认证增强：认证方式可多选并排序、Windows 上的 Pageant、agent 转发

**为什么排最后**：F8 重连引入连接状态机，与「一条连接承载多 pane」耦合最重
（重连后 N 个 channel 如何恢复、迟到的 `PaneOpened` 如何处理——adr-009 已记录这类失效模式）。

**验收**
- 重连退避策略为纯函数（`next_delay(attempt) -> Duration`），单测含上限与放弃条件。
- 编码转换对 GBK/Big5 fixture 的 VT 快照测试。
- 键序列风格切换的 `keymap` 单测；**不得破坏 T5/T6**
  （`shift_blocks_mouse_report_so_user_can_copy`、`shift_enter_without_kitty_is_esc_cr`）。

---

## 8. P4 — 边界外储备区（登记，不实现）

**登记动作在 P0 一并完成**（否则这批条目会在四期期间散落无主）。P4 本身没有代码产出，
它是这张表的持续存在：每期结束时复查一次，看有无条目因边界变化而应上移。

在 `spec.md` 新增「已登记未排期」表，逐条写*为什么不做* + *重新评估条件*：

- 清单 §5 隧道转发（-L / -R / -D）与 X11 转发
- 清单 §6 SSH 协议参数（KEX / Ciphers / MAC / 主机密钥算法列表、压缩、客户端 banner 伪装）
- 清单 §9 日志与录制（日志目录模板、时间戳、剥离 ANSI、脱敏规则、asciinema 回放）
- 清单 §10 中的 ZMODEM / XMODEM、传输模式与冲突策略（**SFTP 侧栏本体属 F50~F57，不在储备区**）
- 清单 §11 的 Shell/JS 脚本引擎
- 清单 §13 Serial（波特率等）与 Telnet / Rlogin 协议
- 清单 §3 的 OTP/TOTP 自动填充、expect/send 登录应答、GSSAPI-Kerberos、PKCS#11 智能卡
- 清单 §8 的背景图片与背景模糊

## 9. 与架构不变量的关系

- 所有解析、筛选、匹配、拼装逻辑落在 `mullion-store`（纯同步）或纯函数模块，**可无窗口单测**。
- `mullion-ssh` 仍不认识「pane」「会话记录」概念，只接受 `SshConfig` 字节流参数。
- 依赖方向仍为 `app → {core, term, ssh, store}`，本路线图不引入反向依赖。
- 每期新增字段只往 `SessionRecord` 的既有分节里加，不改顶层形状（D3 的全部价值所在）。

## 10. 未决问题（留给各期的实现计划）

- P1：tmux 会话名在多 pane 下是否附序号（倾向不附，靠 tmux 多客户端 attach）。
- P2：图标内置库的资源形态（SVG 内嵌 / PNG atlas），与 glyphon 渲染的关系。
- P3：编码转换插在 VT 仿真之前还是之后（影响 `mullion-term` 的输入边界）。
