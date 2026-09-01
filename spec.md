# Mullion 需求规格

> 状态：草案 v0.1 · 最后更新 2026-07-23
> 这份文档是需求的**唯一真源**。代码、测试、提交信息都引用这里的编号。
> 需求变更改这里，不要在别处口头约定。

---

## 0. 一句话

一个原生 GPU 加速的 SSH 客户端，让「在高延迟链路上操作远端 tmux 里的 Claude Code」
这件事，跟在本地一样顺。

---

## 1. 背景

现状是两派工具，各缺一半：

- **终端模拟器**（Windows Terminal / WezTerm / Alacritty）：终端仿真好，
  但没有文件管理，且它们的设计目标不是高延迟远程。
- **一体化 SSH 客户端**（MobaXterm / FinalShell / Xshell / Termius）：文件管理好，
  但终端仿真是短板。全屏 TUI + 鼠标捕获 + 同步输出这套现代终端协议支持得参差不齐。

而 Claude Code 的 TUI 恰好把这两者同时往死里压：`/tui fullscreen` 走 alternate screen
+ 鼠标捕获，外面还套一层 tmux，链路又是跨境代理。

Mullion 的赌注是：**这个交集市场没人做，而它正在快速变大**。

---

## 2. 目标 / 非目标

### 目标

- G1 在 Windows 11 上，Claude Code 全屏 TUI 通过 tmux + 高延迟 SSH 显示**零可见闪烁**
- G2 分屏是一等公民，且分屏后远端排版立刻正确
- G3 终端和文件管理在同一个窗口里，不用切换应用
- G4 单一原生二进制，无 Electron、无 webview、无运行时依赖
- G5 **隧道可脱离终端窗口独立运行**（§4.12）。Mullion 不只是终端客户端 —— 端口转发
  是与会话平级的第二类对象，不开任何 pane 也能把远端内网端口转发到本机

### 非目标（明确不做，有人提就拒绝）

- N-G1 终端复用器。会话保活交给远端 tmux。
- N-G2 内置 AI 助手 / AI 侧栏。Claude Code 已经在远端跑了，再套一层是噪音。
- N-G3 数据库客户端、RDP/VNC、串口。
- N-G4 云同步、账号体系。配置就是本地文件。
- N-G5 移动端。

---

## 3. 用户与场景

**主用户**：在 Windows 上开发、代码跑在 Linux 服务器上、通过代理访问外网的开发者。

**核心场景 S1**：早上打开 Mullion → 一键连上服务器 → 自动 attach 到既有 tmux 会话
→ 左右分屏，左边跑 Claude Code，右边看日志 → 右侧栏浏览远端文件、点开就编辑。
全程不切应用、不闪、不断。

**场景 S2**：Claude Code 生成了一批文件，我想直接看 diff 和内容，
而不是让它 `cat` 给我看。

**场景 S3**：代理抖了一下，连接断了。重连后一切回到断开前的样子（因为 tmux 还活着）。

---

## 4. 功能需求

优先级：**P0** = v0.1 必须有 · **P1** = v0.2–0.3 · **P2** = 之后

### 4.1 连接

| ID | 需求 | 优先级 | 验收标准 |
|---|---|---|---|
| F1 | SSH 密码 / 公钥 / ssh-agent 认证 | P0 | 三种方式各有一个针对真实 sshd 容器的集成测试 |
| F2 | 导入 `~/.ssh/config`（Host / HostName / Port / User / IdentityFile / ProxyJump） | P0 | 给定 fixture config，解析结果与预期 struct 相等。**已实现（v0.1.42）**：解析器在 `mullion-store::ssh_config`（零 IO，按 `man ssh_config` 的 **first-obtains** 语义合并通配块），入口是菜单「会话 → 导入 ssh config…」→ 预览勾选 → 批量落库；`ProxyJump` 两阶段回填成本项目的跳板引用，指向批外的别名会话照导、跳板留空并在预览里标黄。**`IdentityFile` 只把路径记进会话备注、不读私钥正文**（v5 起私钥正文入库，批量代读等于替用户做主）；同名会话不覆盖、默认不勾；`Include` 不展开、`Match` 块与否定模式整块跳过，均在预览里逐条说明。设计见 `docs/superpowers/specs/2026-08-13-ssh-config-import-design.md` |
| F3 | TOFU 主机密钥校验：首次记录指纹，变更时拦截并弹窗（F92「测试连接」触发的指纹确认**仅本次信任、不写 known_hosts**——一次拨测不该在用户还没决定要不要保存这个会话时就改动信任库。） | P0 | 指纹不匹配时连接必须失败；单测断言 `verify()` 返回 false |
| F3-a | `known_hosts` 的 key 采用 OpenSSH 的 `[host]:port` 形式（端口为 22 时省略方括号只写 `host`） | P2 | 单测：同主机名不同端口是两条独立记录；旧的裸 `host` 记录仍能读出（迁移兼容）。**现状（v0.1.7）key 只用主机名，同主机换端口会误报「密钥已变更」** |
| F4 | SOCKS5 / HTTP CONNECT 代理，含带认证的 | P0 | 对本地 mock 代理的集成测试 |
| F5 | 跳板机（ProxyJump），支持跳板机本身在代理后面 | P1 | 两跳链路集成测试 |
| F6 | 连接失败时给出**可操作**的错误（区分 DNS 失败 / 拒绝连接 / 认证失败 / 主机密钥变更） | P0 | 每类错误对应一条独立的错误枚举，不允许统一 "connection failed" |

### 4.2 终端

| ID | 需求 | 优先级 | 验收标准 |
|---|---|---|---|
| F10 | VT100/xterm 仿真：256 色、truecolor、alternate screen、滚动区域 | P0 | `vttest` 主要项目通过；VT 快照测试覆盖 |
| F11 | **同步输出（DEC 2026）**：在 BSU/ESU 之间攒帧，收到 ESU 才提交 | P0 | 单测：喂入含 `CSI ? 2026 h/l` 的序列，断言中间态未触发 present |
| F12 | 差分整形：按行内容指纹跳过未变行的文本整形（不用 `Term::damage()`，理由见 [adr-011](docs/adr-011-row-fingerprint-vs-term-damage.md)） | P0 | 单测：只改一行后，脏行集合只含那一行 |
| F13 | Kitty 键盘协议；不支持时优雅退化 | P0 | 见 F14 |
| F14 | **Shift+Enter 正确编码**（Kitty → CSI-u；否则 → `ESC CR`），Ctrl+J 恒为 `\n` | P0 | `keymap` 单测已覆盖 |
| F15 | SGR 鼠标上报；**按住 Shift 强制走本地划选** | P0 | `keymap` 单测已覆盖 |
| F16 | CJK 宽字符占两格，字体回退不缺字 | P0 | 快照测试断言宽字符列宽；缺字需人工目视确认 |
| F17 | 滚动回溯，默认 10000 行，可配置 | P0 | 单测断言 scrollback 行数 |
| F18 | 划选复制 / 粘贴，粘贴走 bracketed paste | P1 | 单测：开启 bracketed paste 时粘贴内容被 `ESC[200~` 包裹 |
| F19 | 终端内搜索（正则） | P2 | — |
| F20 | 链接识别 + Ctrl 点击打开 | P2 | — |
| F21 | 可配置显示字体（字体族 / 字号 / 字重，随 DPI 缩放，跟随 `ScaleFactorChanged` 动态更新） | P1 | 配置解析单测；字体族缺失时优雅回退。**已实现（v0.1.39）**：字体族 / 字号在设置弹窗（F84）里可配，落 `settings.toml`；字重仍是 Normal 一档 |

### 4.3 分屏（本项目的自研核心）

| ID | 需求 | 优先级 | 验收标准 |
|---|---|---|---|
| F30 | 水平 / 垂直分屏，任意嵌套 | P0 | `layout` 单测已覆盖 |
| F31 | 关闭 pane 时兄弟节点顶替，最后一个 pane 不可关 | P0 | `layout` 单测已覆盖 |
| F32 | 拖分隔条 resize，夹紧最小尺寸 | P0 | `layout` 单测已覆盖 |
| F33 | 方向键切焦点，几何法（不跳斜对角） | P0 | `layout` 单测已覆盖 |
| F34 | **布局变更后立刻对每个受影响 pane 发 `window_change`** | P0 | 集成测试断言 resize 消息被发出，且列数与新矩形一致 |
| F35 | 新 pane 复用同一条 SSH 连接开新 channel，不重新握手 | P0 | 集成测试：开 4 个 pane，断言底层只有 1 次 TCP 连接 |
| F36 | 标签页（一个标签 = 一棵布局树）。**最小版**：新建/关闭/切换（`Ctrl+Tab` / `Ctrl+W` / `Ctrl+1..9`），标签类型两种（终端工作区 / SFTP 文件视图）；**标签栏常驻**——只剩一个标签也显示，不自动隐藏（隐藏/出现是一次高度跳变 = 整幅终端 reflow）；不做拖拽重排、不做重启恢复（那是 F37）、不做分离窗口；标签的名称/颜色支持**本地覆盖**（F122），覆盖不落盘 | P1 | **切标签不断连接**——来回切换后底层 SSH 连接数与 channel 数不变；迟到事件（`PaneOpened`/自动化结论/`ConnectErr`）一律按**世代号**路由到属主标签，**绝不用「活动标签」去接**（否则跨标签串味）；标签栏高度若有任何变化（含将来引入的显隐配置）必须走既有 reflow 链路发 `window_change`（T4），不新开尺寸传播路径 |
| F37 | 布局持久化（标签 + 分屏树 + 焦点叶子 + 窗口几何）。**「启动即自动摆回」那部分已被 F148 取代** —— 现在存的是「本实例的槽位」，摆什么由用户在恢复列表里选 | P1 | 序列化/反序列化 round-trip 单测（`layout::tests`） |
| F38 | 广播输入到所有 pane | P2 | — |

### 4.4 文件

| ID | 需求 | 优先级 | 验收标准 |
|---|---|---|---|
| F50 | SFTP 文件面板：浏览远端目录，显示名称/大小/修改时间/权限（+ 属主:组，默认隐藏，uid/gid 显示**数字**——协议拿不到 `/etc/passwd` 映射）。两个宿主：SSH 会话的**右侧栏**（`Ctrl+Shift+B`——**不能用 `Ctrl+Shift+F`，已被 F100 标注模式占用**；蹭会话连接开 sftp subsystem，不重握手）与 SFTP 节点的**标签页**（独占连接，退避重连）。**不进布局树**，`mullion-core` 零改动 | P1 | **进程内假 SFTP 服务端**（`russh_sftp::server` + 内存 FS，跑在既有假 sshd 基建上）；开关侧栏走既有 reflow 发 `window_change`（T4）；真 OpenSSH 差异（`internal-sftp` chroot、exec 被拒、扩展缺失）进人工清单 |
| ~~F51~~ | ~~跟随当前 pane 的 shell cwd 自动切目录（靠 OSC 7）~~ **已于 2026-08 移出范围**，见 §4.11 储备区。替代：F120 书签 + 可配置默认目录；**2026-08-18 起由 F123 以缩小的范围部分承接**（不依赖 tmux 转发 OSC 7，改走 tmux 自己发的窗口标题；且只在侧栏「关→开」跃迁那一次继承，不做持续跟随——F51 被否的三条理由因此都绕开了） | — | — |
| F52 | 从 Windows 资源管理器拖拽上传。**忽略落点**：winit 0.30 的 `DroppedFile` 与 `egui::DroppedFile` 都不带坐标，Windows 在 OLE 拖放期间也不发 `CursorMoved`，落点判定不了 —— 一律上传到**远端栏当前目录**，拖入时远端栏描边 + 顶部明写「松开上传到 /path」，让规则先于动作可见 | P1 | 人工验证清单（无头验不了） |
| F53 | 就地编辑：右键**两个入口**「外部编辑」（`ShellExecute` 系统默认程序）/「内置编辑」；**双击 = 外部编辑**（与资源管理器一致）。回传是**自动 + 冲突检测**：回传前 `stat` 远端，mtime/size 与下载时不一致则**不覆盖**，转冲突提示（保留远端 / 覆盖 / 另存副本）。监视持续到用户显式「结束编辑」，**不靠猜编辑器进程退没退** | P1 | 集成测试：mtime 变化触发回传；**远端先变则走冲突分支而非覆盖**（须自证变红） |
| F54 | 多选批量下载 / 删除 | P1 | — |
| F55 | 传输队列 + 进度 + 取消。**全局、跨标签存活**（底部可折叠面板）；单文件失败不掐队列；同名冲突弹一次询问（覆盖/跳过/重命名/全部应用），不静默覆盖；**默认不做断点续传**（见 §4.11） | P1 | 关标签/切标签不清空队列，有单测 |
| F56 | 并发 SFTP 通道可配（1–8，默认 4）。并发粒度是**不同文件**，不做单文件分块并发 | P2 | 同一条 SSH 连接上开多 channel，不新建连接 |
| F57 | 递归删除：**先试 `exec rm -rf`，被拒或失败回退 SFTP 逐文件递归**。sftp-only 账号（`ForceCommand internal-sftp`）会拒 exec，一律走 exec 则功能残缺；一律逐文件则每文件一个 RTT | P2 | 路径转义（单引号包裹 + `'\''`）覆盖空格/引号/换行/`$`/反引号；exec 被拒时回退有单测 |
| F58 | **双栏文件面板 + 栏间拖拽**：远端栏 + 本地栏，互拖即传输（左拖右上传、右拖左下载、拖到目录行进该目录）。**侧栏模式上下堆叠**（远端 60%/本地 40%，可拖并记住），**标签模式左右并排**。本地栏**只做导航与传输端点**——不提供本地删除/重命名/新建（Windows 删除应进回收站，风险不对称；本地管理外包给资源管理器，右键「在资源管理器中打开」） | P1 | 两种宿主共用同一份列表/拖拽/右键代码，差别只有容器方向；侧栏在典型宽度（320~450px）即可容纳双栏——这是选上下堆叠的全部理由，若实现时发现必须加宽才放得下即为设计跑偏 |
| F59 | **拖出到资源管理器**（Windows-only）：专用 **STA 线程** `DoDragDrop` + `CFSTR_FILEDESCRIPTOR`/`CFSTR_FILECONTENTS` **虚拟文件**（延迟渲染，不预先落盘），`IDataObject`/`IStream` 走 `CoCreateFreeThreadedMarshaler` 让目标进程在自己线程读，UI 全程不冻 | P1 | **绝不能在 winit 回调栈内启动**——`runner.rs:208` 对 `RedrawRequested` 绕过缓冲直调 handler，嵌套模态循环遇 WM_PAINT 必 panic（写进 `gui-render-gotchas.md`）；全流程日志（target `mullion::sftp::drag_out`）是唯一诊断手段；**无头一行都验不了**，进人工清单；**2026-08-20 修正**：交接判据原为「指针出了窗口」，窗口最大化时永远不成立——这个功能从 v0.1.37 起一次都没触发过（用户报「拖了完全没反应、也没有任何提示」，而每条失败路径都会 `set_error`）。改为「指针离开文件面板矩形」，守护 `a_remote_drag_that_left_the_panel_but_not_the_window_is_still_handed_off` |
| F120 | **SFTP 书签与默认目录**（编号接在 F119 之后，§4.4 的 F5x 段已用尽）：`SessionRecord` 新增默认远端目录、默认本地目录、远端书签列表，**schema v7 → v8**。留空时远端用 `.`（home）、本地用 `%USERPROFILE%`。**不记忆上次目录** | P1 | v7 文件加载后新字段缺省、其余逐字节等价；编辑页新「SFTP」分节按 F119 表单规范 |
| F121 | **会话列表手动排序**：左栏拖拽调整会话顺序，支持组内换位与跨组拖动（跨组顺带改 `group_id`，等价于右键「移动到分组」）。顺序真值 = `sessions.toml` 的数组顺序，**不加排序字段、无 schema 改动**。分组自身的顺序不在范围内 | P2 | `Vault::move_session` 的位置算术为纯函数单测（先移除再定位、`before` 悬空落末尾、拖到自己身上 no-op、不动 `modified_at`）；落点判定（上/下半、组内末行、拖到分组头）为纯函数单测；搜索中/`Icons` 档禁用拖拽有单测；`ScrollArea` 关掉 `drag_to_scroll` 有源码级守护（F58 同款坑）；真实指针事件驱动的完整拖拽（按下→移动→松手）写入 `reorder_request` 有行为级单测，`touched_store`/`StoreIo` 两处施加点各自有源码级守护；真实拖放观感人工验收 |
| F122 | **标签本地覆盖**：标签的名称/颜色只作用于该标签自身，**不写回会话记录**（会话列表、pane 标题条、状态栏一律不变），也不进 F37 布局快照——关窗口即丢。同一会话的两个标签可各自改名配色；快速连接开的标签同样可改。颜色画成**整块背景**，标题前景按对比度自动取黑/白 | P2 | 保存不写 store（源码级守护：`touched_store` 表达式不含 `tab_props`）+ 覆盖只落在 `Tab` 的两个字段上、连接时拼的 `title` 不被改写，有单测；覆盖归属标签而非会话（同会话两标签互不影响）有单测；重连（`Tabs::replace`）不丢覆盖有单测；`readable_fg` 在 8 个预设 + 黑/白/中灰上对比度 ≥ 4.5:1 实算单测；`Modal::TabProps` 仍在 `Modal::ALL` 里（T8）；实际观感人工验收 |
| F123 | **远端状态上报**（编号接在 F122 之后；这是 F51 的缩小版承接者，不是它的复活）：从远端字节流里嗅 **OSC 7** 与**窗口标题**（OSC 0/2），解析出当前目录与 tmux 会话名。两个用途：① pane 标题条显示目录名与 tmux 会话名（自 2026-08-18 起并进左边那一整串「序号 · 节点名 · 目录名 · tmux 名」，不再是右侧角标）；② 文件侧栏**「关→开」跃迁那一次**把远端栏开在焦点 pane 的目录（**只接受绝对路径**——openssh 的 `sftp-server` 不展开 `~`，`canonicalize("~/x")` 会失败，面板停在「取不到登录目录」，比不继承更糟）。**不做持续跟随**：侧栏已开着时终端 `cd` 不拽走面板（否则用户在面板里的浏览会被反复打断）。远端不配就静默降级，不是 bug。配置手册见 `docs/remote-state-setup.md` | P2 | 解析是 `mullion-term` 的零依赖纯模块（`remote_state.rs`，14 条单测：`file://` 前缀/percent 解码/BEL 与 ST 两种结束符/跨 `feed` 分片/标题第二段须为纯数字才认会话名/标题里含空格的路径会被截断故 OSC 7 优先）；`take_remote_state` 的 take 语义 + `pump` 是唯一消费者有单测；`cwd` 只增不清、`tmux` 收到新标题即整体重置（含清空）两套策略各有单测；换节点（`rehost_pane`）必须清掉两个字段有单测；`files_start_dir` 的优先级与「只接受绝对路径」有纯函数单测；`sync_target_of` 的四个早退有纯函数单测；跃迁判据存 `App` 字段而非帧内局部变量（热键在另一次事件回调里改标志）有源码级守护。**tmux 默认吞掉内层 OSC 7 不转发**（同 F51 被否的第③条），所以 tmux 场景只能靠 `set-titles-string` 带 `#{pane_current_path}`——这条只能人工验收。**自 2026-08-18 起由 F124 自动配置远端**，「远端不配就静默降级」因此只发生在关掉 F124 开关、或远端没有 tmux 的情况下 |
| F124 | **远端状态自举**（编号接在 F123 之后）：连上之后开一条旁路 exec channel 跑 `tmux set -g set-titles on && tmux set -g set-titles-string '#S:#I:#W #{pane_current_path}'`，让 F123 在**没配过的远端上**也能拿到数据。tmux 的 `set-titles` 是**服务器全局选项**，与 pane 无关，所以旁路 exec 不受 adr-009「分不清是哪块分屏」的限制（那正是 F51 被否的理由之一）。改的是 tmux 服务器**内存里**的选项，不写任何文件，server 退出即失效。**默认开**，设置弹窗可关。tmux 服务器还没起时退出码 1（实测**不会**顺手拉起一个空 server），每 30 秒重试到成功为止，成功后永不再试 | P2 | 命令串（`&&` 串联 + 单引号包住 format string + 带 `#{pane_current_path}`）有纯函数单测；`should_attempt` 的五条分支（开关关 / 已成功 / 在途 / 从没试过 / 未到重试间隔）有纯函数单测；`BootstrapFlags` 只在成功时 latch `done`（失败也 latch 的话「tmux 还没起」会让这条连接永不重试）有单测；`clone` 出来共享同一份状态有单测；tick 挂在 `about_to_wait` 上、走共享判据/共享命令串/写回结论，有源码级守护；**对本机真 tmux 3.7b 跑的 live 测试**（`--ignored`）断言 server 不在时失败且不拉起空 server、有 server 时两个选项确实被改。覆写用户的全局 `set-titles-string` 是刻意的，理由与影响面见 `docs/remote-state-setup.md` |
| F125 | **光标形状跟随远端 DECSCUSR**（`CSI Ps SP q`）：远端不指定时默认**闪烁竖线**（这是 Claude Code / 现代 shell 的观感基线；alacritty 的默认是不闪的方块，不改就跟远端说的不是一回事）。非焦点分屏恒**空心框且不闪**——闪的那个才是键往哪儿去。窗口失焦停闪（同时停掉周期性重绘，守 T3/T7） | P1 | 相位判据是纯函数（530ms 半周期、打字重置相位、粘贴也重置）；`style_for` 的形状×焦点矩阵有单测（含 `HollowBlock` 档与参数顺序）；闪烁并入既有定时唤醒链路而非新起 timer，有源码级守护 |
| F126 | **组字中的拼音内联显示在光标处**（见 `/tmp/1.png` 的参照）：带下划线，光标停在拼音串**末尾**，系统候选框跟随该位置。宽字占两格、到行尾**截断而不折行**（折行会把远端排版顶乱，而 preedit 是纯客户端的临时物）。组字期间正文文字层的原字符字形要**让路**，否则拼音压在旧字上糊成一团 | P1 | 网格布局是纯函数（宽字两格 / 行尾截断 / 光标居末）；`ImeState` 以 preedit 串为唯一真值源（曾有一个与之冗余的 bool，两者能不一致）；三条结束边（提交 / 取消 / 失焦）都清空有单测；隐藏区间的判断抽成纯函数与 preedit 守卫**同源**，避免两处各写一套 |
| F127 | **SFTP 文件图标扩到 8 类**（目录 / 归档 / 图片 / 代码 / 文档 / 可执行 / 链接 / 其他）：形状 + 语义色**双编码**，色板对 `panel_bg` 的对比度均 ≥ 3:1。判类优先级固定：`EntryKind`（目录/链接）→ 扩展名表 → `mode & 0o111` → 其他；**点文件没有扩展名**（`.bashrc` 的 `bashrc` 不该被当成扩展名去查表）。不可操作的行**图标与文字一起变灰**，不出现「文字灰了图标还亮着」 | P2 | 判类纯函数有单测（含扩展名优先于 x 位、点文件无扩展名）；八支形状的顶点序列两两不等有单测——**这只是弱守护**，挡得住「复制粘贴成同一支」，挡不住「新形状跟老形状长得太像」，后者只有人眼能判 |
| F128 | **SSH 断线检测与自动退避重连**：检测靠 keepalive（10s×3，**收不到就算断**，不依赖 TCP 写失败，所以半开链路 30 秒内也检测得出）；重连**保留断线前的屏内容**（走 `reattach_pane` 换 channel，不重建 emulator）、**重跑登录后命令**（tmux 因此会 attach 回原会话，这正是这条需求的初衷）、**按连接分组只拨一次**（adr-009：一条连接承载多块分屏）。退避表复用隧道那张：首拨**零延迟**，其后 1/2/4/8/16/30/30/30 秒，共 9 次后放弃并落回 `Disconnected`。凭据取**连接那一刻定死的 cfg**，绝不回头查库（断线期间用户改了或删了会话，查库就会拨到他没同意过的地方）。主机密钥走后台策略——指纹变了当场停，**不在重连途中弹窗**（断线正是中间人最好下手的时机）。远端敲 `exit` **不触发**重连（判据是链路还活着 = 用户主动退出）。重连成功后丢弃旧 SFTP client（它挂在死连接上，留着的话侧栏每次操作静默失败） | P0 | `hosts_to_redial` / `delay_for` / `status_after_failure` / `notice_bytes` / `give_up_notice_bytes` 全是纯函数带单测；`rx_closed_action` 区分「远端 exit」与「链路死了」有单测；减屏优先级 `Disconnected < Reconnecting < Live` 有单测（穷尽 `match` 只保证加档时编译报错，保证不了三个数字没写反）；`reattach_pane` 保留屏内容而 `rehost_pane` 抹掉、且两者都真的换了 pty 与 rx，各有单测；`drive_reconnects` 遍历**全部**标签而非只驱动活动标签，有源码级守护（只驱动活动标签的话后台标签断线要等用户切回去）；「用 last_cfg 不查库 / 走 reattach 不走 rehost / 重跑自动化 / 丢弃 SFTP / 拨号途中 pane 全没了要撤回 HostConn」五条接线各有源码级守护 |
| F129 | **已断开（含重连中）的分屏上 `Ctrl+D` 关掉该分屏**，是标签里最后一块时关掉整个标签（`close_pane` 本就拒绝关最后一块，不特判的话用户按下去**什么都不会发生**）。**活着的分屏上 `Ctrl+D` 照旧是 EOF**——它是 shell 退出登录、给 `cat` 收尾的标准键，这个语义不能动。修饰键必须排掉 Alt：**Windows 把 AltGr 合成成 Left-Ctrl + Right-Alt** | P2 | `ctrl_d_action` 的三态判据有单测（活着恒 EOF / 死了关分屏 / 最后一块关标签）；`is_bare_ctrl_d` 排除 AltGr 与 Win 键有单测，并配一条反向测试防止判据收太紧让整个功能静默失效；「判定必须排在 `encode_key` 之前」有源码级守护——挪到之后是静默失败：键先被编成 `0x04` 写进一条死 channel |
| F130 | **换节点弹窗的会话顺序与会话管理器左栏一致**（分组桶顺序 + 组内数组顺序）。两边各排一套的话，左栏里挨着的两条在弹窗里可能隔着半屏，而用户是照着左栏的记忆找的 | P2 | `rehost::tests::rows_are_ordered_exactly_like_the_session_manager_list`：顺序取自 `list::visible_order`（左栏渲染与键盘导航的同一个函数），fixture 刻意让数组顺序与分组顺序相反 |
| F131 | **文件面板路径条可编辑**（两栏）。单击进入编辑、回车跳转、Esc/失焦丢弃；`~` 用远端登录目录（本地栏用本机主目录）展开，相对路径拼在当前目录后面，`..` 按纯字符串规整 | P2 | `path_input` 的八条解析单测；`files_panel::tests::clicking_the_path_bar_starts_editing_it`；**编辑态必须进 `Modal`**（面板持焦时键盘不喂 egui，不算模态则一个字都打不出来，T8），有源码级守护 |
| F132 | **`Ctrl+Shift+B` 开侧栏时，SFTP 开在焦点分屏所在的那台机器上**，起始目录 = 该分屏的 cwd。换过节点的分屏此前恒连 `hosts[0]`——路径对了、机器错了，一次看不出错的误操作 | P2 | `sync_plan_of` 的七条分支单测；「host_ix 在打开成功时记、不在发起时记」（`the_sftp_host_is_recorded_when_the_channel_opens_not_when_it_is_requested`）「先摘旧 client 再 `trigger_sftp_open`」（`reopening_sftp_drops_the_old_client_before_asking_for_a_new_one`）「判定说 Reopen 就真的去重开」（`deciding_to_reopen_actually_calls_the_reopen_path`）三条源码级守护 |
| F133 | **office 大类各自的图标**：PDF / Word（doc·docx）/ 表格（xls·xlsx·csv）/ 演示（ppt·pptx），颜色对齐 Windows 心智（红蓝绿橙） | P3 | `file_icon::tests::classify_maps_extensions_to_kinds`；既有的「两两不同」「不越格」两条照 `IconKind::ALL` 遍历；`every_kind_used_by_the_extension_table_is_listed_in_all` 防「加档漏进 ALL」 |
| F134 | **普通文件的默认图标从「其他」里拆出来**：不认识扩展名的普通文件用折角空白页，菱形只留给设备/socket/命名管道 | P3 | `an_unknown_regular_file_is_not_the_same_as_a_device_node` |
| F135 | **文件面板列头分隔线可拖拽调宽**：五列（名称/大小/修改时间/权限/属主）各自定宽，最小宽度名称列 80pt、其余 48pt，最大 800pt（`COL_MAX`）；宽度只存内存 `UiState::files_cols`（两栏与所有标签共用一套），**不落盘、不进 F37 布局快照**——关窗口回默认。拖动只改被拖那一列，右边的列整体平移，**不做此消彼长的「借宽度」**：总宽本就允许超出视口、有 F136 的横向滚动兜着，若做成宽度守恒，「我只想加宽名称列」会变成「顺手把修改时间列挤没了」 | P2 | `dragging_a_column_edge_only_widens_that_column`；`a_column_cannot_be_dragged_below_its_minimum` / `a_column_cannot_be_dragged_past_its_maximum` 夹紧上下界；`the_column_widths_come_from_the_caller_not_from_a_fresh_default` 防「忘了接 `UiState`、每帧用一份新默认值」；**拖拽热区必须先于列体注册**（egui 同点同时命中纯 `click` 与纯 `drag` 部件时后注册的在上，drag 在上且不带 click 会把 click 吃成 `None`，边界那 6pt 上按下去排序和缩放都不响应），守护是 `pressing_exactly_on_a_column_edge_still_sorts` 与 `clicking_the_middle_of_a_header_still_sorts` |
| F136 | **两栏均支持水平滚动**（`ScrollArea::both`）：列不再因宽度不够而自动收起——旧版是「名称列吃掉剩余宽度 + 从右往左逐个收起可选列」，结果窄侧栏下「属主」列**永远看不见**，用户不知道它存在；改成固定列宽 + 横向滚动后，只要愿意滚，五列永远都在。列头绘制在滚动区之后、跟随同一帧的水平偏移 | P2 | `all_five_columns_are_present_no_matter_how_narrow_the_panel_is`；`column_lefts_are_contiguous_and_sum_to_the_content_width`；`the_size_header_and_the_size_value_land_in_the_same_column`；`a_column_scrolled_out_of_view_is_not_registered_with_an_off_screen_rect`；**列头水平偏移必须在调 `show_rows` 之前现读 `egui::scroll_area::State::load()`，不能用 `ScrollAreaOutput.state.offset`**（那是留给下一帧的值，用它会让列头与行体错位，已写进 `docs/gui-render-gotchas.md`），守护是 `horizontal_scroll_moves_the_header_and_the_rows_by_the_same_amount` |
| F137 | **单元格按自己的列宽截断**：名称列**中间省略保留扩展名**——尾部省略会把扩展名整个吃掉，`很长的归档名.tar.gz` 变成 `很长的归…`，看不出这是归档还是文本；中间省略成 `很长…tar.gz` 则类型还在。**已知退化**：名称列压到最小宽度（80pt）时预算放不下两端，会退化成尾部省略，此时同前缀的备份文件确实会长得一模一样——取舍是「极窄时保证还画得出东西」，靠拖宽或横向滚动解决；其余列尾部省略；列头标题本身也截断 | P2 | `eliding_a_name_in_the_middle_keeps_its_extension`；`ext_tail_treats_a_leading_dot_as_part_of_the_name`（点文件 `.bashrc` 不误判扩展名）；`eliding_never_exceeds_the_budget_and_never_panics` / `truncate_to_width_returns_an_empty_prefix_for_a_non_positive_budget` / `eliding_end_mode_actually_truncates_instead_of_degrading_to_empty` 是截断纯函数的边界单测；`a_long_name_is_elided_so_it_cannot_reach_the_size_column`；`a_narrow_column_header_is_elided_so_it_does_not_overflow_the_column` |
| F138 | **文件面板统一内边距**：两个宿主（侧栏 `SidePanel` / 标签 `CentralPanel`）的 `Frame` 加 `inner_margin`，左右 `SP_S`、上下 `SP_XS`——此前路径条的「↑」按钮直接贴在面板外框上，与项目其余表单的留白约定不一致 | P3 | `the_panel_does_not_draw_its_contents_flush_against_its_own_edge`：判据取**矩形**（面板外框 rect 与最外层内容 rect 相比），不拿文字位置——按钮自身的 padding 会把没有 margin 的情形也顶到阈值之上，最初那版就是这么恒绿的 |
| F139 | **路径条收藏与书签下拉**：☆/★ 切换收藏当前目录（名字默认取路径末段），▾ 列出全部书签点击即跳。**去掉原来的横排书签栏**——它只在已配过书签时才出现，用户根本不知道它存在。书签仍存 `SessionRecord.sftp.bookmarks`，与会话编辑页共用一份，**无 schema 改动**；收藏当场 `store.save()`。标签没有 `SessionId`（CLI 直连）时 ☆ 置灰并悬停说明原因；只给远端栏 | P2 | `clicking_the_hollow_star_bookmarks_the_current_directory` / `clicking_the_filled_star_removes_that_bookmark`（★/☆ 由 cwd 与列表现算，不存标志位）/ `the_star_is_disabled_when_the_tab_is_not_bound_to_a_session` / `picking_a_bookmark_from_the_dropdown_emits_goto` / `a_bookmark_with_an_empty_name_falls_back_to_showing_its_path`；`bookmarks_added_from_the_path_bar_survive_save_and_reopen`（store 侧，存盘重开 + 按路径去重）；`bookmarking_writes_through_to_disk_immediately`（源码级，切片限定在函数体内以避开自我匹配） |
| F140 | **SSH channel 显式收口**：关分屏 / 换节点 / 关标签三条路径都发 `SSH_MSG_CHANNEL_CLOSE`（`SshSession::close` → `io_task` 退出前 `write.close()`）。此前只丢弃 `PaneState`，而 **russh 0.54.5 的 `ChannelWriteHalf` 没有 `Drop` 实现**，`io_task` 只发 `eof` —— 远端 shell 挂着不死，channel slot 泄漏到 sshd `MaxSessions`（默认 10）后同一条连接开不出新分屏（adr-009 已列的失效模式）。tmux 语义：client 收 SIGHUP = detach，tmux server/session 不受影响；裸前台命令会被 SIGHUP 杀掉，与关掉 PuTTY 窗口一致 | P1 | `close_sends_a_close_command_down_the_io_queue`（ssh 侧）/ `closing_a_pane_closes_its_channel_f140` / `closing_every_pane_closes_every_channel_f140` / `winding_down_a_terminal_tab_closes_every_pane_channel_f140`；**「真的发出了 CHANNEL_CLOSE 报文」无头验不了**，进人工清单（远端 `ps -ef | grep sshd` 看子进程回收） |
| F141 | **断线重连接回原来的 tmux 会话**：重连后，**当初 attach 过 tmux 的那块 pane**（记在 `TerminalTab::tmux_attach` 里：pane id + host 下标 + 真实会话名）重发一遍连接计划，命令是 `tmux has-session -t X && exec tmux attach -d -t X \|\| exec tmux new-session -s X`。`-d` 踢掉断线后仍挂在远端的僵尸 client（否则 attach 上去两个 client 抢一个终端、按键错乱）；**`-d` 绝不能加到 `new-session` 上**（那是「建好但不 attach」，会话立刻回到后台、屏幕一片空白）。其余分屏 pane 维持原样（跳过 tmux，只跑 cd/export/命令）——一台机器上多块 pane 同时 attach 同一个 tmux 会话，彼此的窗口切换会互相打断 | P1 | `the_reattach_plan_is_the_first_connect_plan_plus_the_detach_flag`（整行做差，不是拿常量断言常量）/ `reattach_never_puts_the_detach_flag_on_new_session` / `the_reattach_plan_is_empty_when_there_is_no_tmux_to_come_back_to` / `the_recorded_session_name_matches_what_the_plan_actually_attaches`；`a_reconnected_main_pane_comes_back_to_its_tmux_session_unlike_an_extra_pane`；`the_connect_records_exactly_the_tmux_session_the_plan_attaches` / `only_the_pane_that_attached_tmux_on_that_very_host_gets_reattached` |
| F142 | **属主列显示用户名/组名**：格式 `用户名:组名`，任一段查不到就**那一段**回退成数字（`deploy:10001`）。SFTP v3 的 attrs 里只有数字 uid/gid，`russh-sftp 2.4.0` 的客户端 `DirEntry` 又把 `longname` 丢了 —— 名字在协议层拿不到（**这一条推翻了 D21 的「不为此去 exec」**）。做法：列完目录后把这一屏出现的 uid/gid 去重，exec 一次 `getent passwd …; echo 分隔行; getent group …`，结果存进**每条连接一份**的缓存（挂远端栏 `PaneState::owners`），换连接时整份清空。**问过就不再问，包括没问出结果的**（负缓存；不然一屏孤儿 uid 会每次刷新都重问一遍）；单批上限 128 个 id，剩下的下次列目录再问；`getent` 不存在或账号是 sftp-only 时整列静默回退数字，不弹错。列宽默认 92→120pt | P2 | `files::owners` 九条纯逻辑单测（`the_query_asks_once_per_distinct_id_not_once_per_file` / `an_id_that_came_back_empty_is_still_never_asked_again` / `a_query_that_never_went_out_can_be_asked_again` / `the_separator_keeps_the_user_half_and_the_group_half_apart` / `each_half_falls_back_to_its_own_number_independently` / `output_without_the_separator_is_refused_wholesale` / `a_malformed_line_is_skipped_without_dropping_the_rest` / `a_huge_directory_is_asked_in_bounded_batches` / `switching_connections_throws_the_whole_cache_away`）；`a_remote_owner_shows_its_name_once_getent_has_answered`（渲染层）；接线守护 `owner_names_are_asked_on_the_right_host_at_the_right_time`（**查名字用的是列出这批 entries 的那台机器（已落定的 `sftp_host_ix`），不是此刻焦点那台**）/ `a_getent_that_failed_still_reports_back_so_the_cache_rolls_back` |
| F143 | **UI 字符串的字形白名单**：egui 的字体链只有两级（内置 Ubuntu-Light/NotoEmoji + `install_cjk_font` 追加的系统 CJK 字体，Windows 首选微软雅黑），两级都没有的字形 epaint 画成豆腐块 `□`，而**编译、测试、日志全静默，只有人眼看得见**。判据是「该字符在 **GBK/CP936** 内」——不是 GB18030（它编码全 Unicode，「能编码」对任何字符都成立，等于没有判据）。放行 ASCII、CJK 汉字、中文标点，其余非 ASCII 一律要在 `ui::glyphs::VERIFIED` 里登记；**登记这一步就是闸门**（逼你先去 Windows 实机看一眼）。不想登记的走 `ui::icon` 自绘。本轮换掉 13 处豆腐（`▾`×3 / `⚠`×3 / `▸`×2 / `⟳` / `↻` / `✕` / `▴` / `•`），`Glyph` 新增 `Refresh`/`TriangleDown`/`TriangleRight` | P1 | `ui::glyphs::tests` 三条（`only_registered_symbols_pass_and_the_known_tofu_does_not` / `every_registered_symbol_is_really_inside_gbk` 拿 `encoding_rs::GBK` 钉住登记闸门本身）；**机械守护** `tests/glyph_whitelist.rs::no_ui_string_contains_a_glyph_the_font_cannot_draw`——用 `proc-macro2` 真词法分析扫 `src/**/*.rs` 的字符串/字符字面量（正则会被注释里的引号带偏），跳过 attribute（**`//!` 展开成 `#![doc=…]`，`#` 与 `[` 之间隔着 `!`，漏掉这个会多出 23 处假红**）与 `#[cfg(test)]` 块 |
| F144 | **文件面板两栏内缩一档**：`content()` 切两栏时 `max_rect` 各 `shrink2(SP_XS)`，`sidebar()` 的两块 `allocate_ui` 同样嵌一层内缩。**裁剪区照旧不动**——两者必须错开：一起缩等于没缩（控件从 `max_rect.min` 起画，边框描边照样压在裁剪边上被削掉半像素）| P2 | `panel_content_does_not_touch_the_clip_edge`；`the_two_columns_get_independent_non_overlapping_scroll_areas` 改成拿**邻栏**边界比对（拿自己那栏的 `max_rect` 比会因为这 4pt 假红，而那 4pt 落在栏间空隙里，永远漏不到隔壁） |
| F145 | **书签下拉显示完整路径**：菜单项画 `bookmark.path`，名字（若与路径不同）退到 hover 提示。原来画名字——一屏全是 `logs`、`conf` 这种末段名，重名的分不出是哪台机器哪个目录 | P2 | `the_bookmark_menu_shows_the_full_path_not_just_the_folder_name` / `a_bookmark_with_an_empty_name_falls_back_to_showing_its_path` |
| F146 | **本地栏不画属主列**：`local.rs` 构造 `Entry` 时 uid/gid 恒填 0，那一列在数据源头上就不存在，画出来是一整列破折号。**判据按栏静态，不按数据**——动态判据（「整列都相同就收起」）会让远端一个全 root 的目录莫名其妙少一列。`col_lefts()` / `content_w()` / `header_at()` 都吃 `PanelColumn`，`row()` 的属主槽改成 `lay.get(4)` | P2 | `the_local_column_has_no_owner_column_but_the_remote_one_does` / `the_local_content_width_drops_the_owner_column_too` |
| F147 | **五列两栏全可排序 + 排序标识画在列尾**：标识从「跟在标题后面拼成一串再截断」改成两次 `painter.text`（标题 `LEFT_CENTER` 在扣掉标识预算后的宽度里截断，标识 `RIGHT_CENTER` 贴列尾留一格 `SP_XS`）。拼接版在窄列必然丢标识——`Elide::End` 从尾部截，标识就在尾部，**而列窄恰恰是最需要看清「按哪列排的」的时候**。排序本身在点击层查下来是好的（矩阵测试全绿），用户报的「大小/修改时间不能排序」多半是标识挤在标题后面看不清 | P2 | `every_column_header_sorts_in_both_panes`（5 列 × 2 栏矩阵，列表取自 `col_lefts()` 不另抄）/ `the_sort_marker_sits_at_the_far_end_of_the_column`（判**边界**贴边，不判中心点——拿中心点要先猜字宽，猜出来的阈值不算判据）/ `a_narrow_column_truncates_its_title_but_keeps_the_sort_marker` |
| F148 | **多实例现场历史**：每个 exe 实例各写自己的 `layouts\<实例id>.toml`（实例 id = 毫秒时间戳 + pid），**从不改别人的文件**；活性靠 `<实例id>.alive` 的心跳（15 秒一写，45 秒宽限），运行中的实例不进别人的恢复列表；已关闭的记录保留最近 10 条（**活着的不删也不占名额**）；启动时迁移 v1 的 `layout.toml`（迁完即删，单向升级）→ 裁剪 → 弹「恢复上次的现场」列表（双行：相对时间 + 标签/分屏数 + 会话名摘要），**启动不再自动摆回标签**；恢复 = 摆回标签 + **接管那条记录的槽位**（此后往它写，并删掉本实例原来的）；菜单「会话」下留常驻入口 | P1 | `is_alive` 纯函数的宽限边界与「未来心跳算活着」/「手改成 `i64::MIN` 不能把客户端崩掉」；`plan_prune` 的「活着的不占名额」；一条坏记录不拖垮整份列表（**两条独立防线各喂一种坏法**：非 UTF-8 死在 `read_to_string`、合法 UTF-8 但非法 TOML 死在 `toml::from_str`）；迁移后老文件必须消失；**心跳不许搭布局落盘的「不脏就不写」**（源码级：`tick_heartbeat` 体内不含 `last_saved_layout`）；`Modal::History` 进 `ALL` 且 `modal_open` 有对应臂（T8）；`has_real_action` 含 `a.history.is_some()`；弹窗的相对时间/摘要/首行三个纯函数；「恢复」回报选中项而非第一项 |
| F149 | **窗口 IME 归宿主所有**：`egui-winit` 在没有文本框组字的帧会 `set_ime_allowed(false)`，关掉的是**整个窗口**的 IME——终端不是 egui 部件，egui 永远不知道它也需要。用户点过一次任意输入框（换节点搜索框/路径条/标签改名/会话管理器字段）再回终端，中文就**永久**打不出来（按 Windows 中英文切换键毫无反应），且**没有自愈路径**，只能重启 exe。修法是在 `handle_platform_output` **之前**把 egui 的账本预写成它这一帧要写的 `false`，让它的去抖短路。同时给 `WindowEvent::Ime` 补上输入分流——此前它绕过了 `is_kbd`（只匹配 `KeyboardInput`），喂给 egui 之后又无条件写进焦点 pane 的 PTY，在会话名/标签改名/路径条里打的中文会**同时**上屏和发到远端 shell。**已知缺口**：归属按每个 Ime 子事件现算而非按一次组字锁定，组字中途切焦点会跟着新焦点走（两个方向各错一半，彻底解法要能打断 OS 的组字） | P1 | `ime_ledger_clamp` 钉住写的是 **false** 不是 true（写 true 反而每帧都触发一次禁用调用，bug 从「用过输入框才触发」恶化成「从第一帧起就没有中文」）；源码级守护钉住它排在 `handle_platform_output` 之前（顺序错了完全失效且静默，锚点带 `\n` 前缀并 `assert_eq!(matches().count(), 1)` 钉唯一性）；`ime_goes_to_terminal_of` 四种组合；不归终端的分支必须清组字状态 + 作废候选框记账 |
| F150 | **多选看得见 + 栏底状态行**：选中行改 `accent` 半透明填充（新 token `sel_alpha`，不新造色相）+ 左侧 2pt 实色色条。原来画 `sunken_bg`（#0e1018），比 `panel_bg`（#14161f）**还暗 6 个亮度单位**，人眼分辨不出来——用户报「按 Ctrl 点，屏幕上完全没变化」，根因是看不见而不是没选上（`click_row` 的 Ctrl/Shift 语义一直都在）。每栏底部加状态行：有选中显示 `已选 N 项 · 体积`（体积只算文件，目录的 `size` 在 SFTP 里是元数据大小；全是目录时**不拼**体积，`· 0 B` 是在撒谎），无选中显示可见行数（`rows()` 而非 `entries.len()`）。**状态行必须画在 `header_at()` 之前**——`scope_builder(max_rect(header_band))` 收尾时 `advance_cursor_after_rect` 对 `TopDown` 是硬赋值，而 `header_at` 从不经 placer 分配部件，`min_rect` 停在列头带顶部的种子点，排在它之后的部件会贴着列头画 | P2 | 拿颜色本身当判据（换回任何比背景暗的 token 都红，宽度区间把整行底色与 2pt 色条分开）；状态行文案三条纯函数测试；**端到端**：平点一行 + Ctrl 点另一行 → 状态行读到「已选 2 项 · 1.0 KB」（这是「点击真的把 ctrl 位带进了 `click_row`」唯一的守护，中间隔着 `ui.input(\|i\| i.modifiers)`——修饰键要写进 `RawInput::modifiers` 而非只写进事件里那份）；`the_status_row_only_renders_when_the_scroll_area_height_is_capped`（没有 `max_height` 兜底时这个部件会从渲染输出里整个消失） |
| F151 | **拖拽跟随预览**：拖动途中在指针旁 `Order::Tooltip` 层画小胶囊，单项显文件名、多项显 `拖动 N 项`。`dnd_set_drag_payload` 只挂载荷不画预览，此前拖起来指针底下空空如也，分不清「拖没拖着」和「拖了几项」。判据取「载荷来自**本栏**」，不区分的话两栏各画一个叠在一起 | P2 | `preview_label` 单项/多项/0 项三条；接线守护 `a_multi_item_drag_paints_a_running_count_next_to_the_pointer`（四条变异全部命中：不画预览/画错栏/多项走单项分支/只画胶囊不画字）。注：`outgoing` 在 `show()` 顶部读载荷、`dnd_set_drag_payload` 在行循环里写，起拖那一帧看不到预览，下一帧才出现 |
| F152 | **程序有自己的图标**：`assets/mullion.ico`（16/32/48/64/128/256 六档）走两条路进 exe——`build.rs` 调 `x86_64-w64-mingw32-windres` 编成 `.rsrc` 段（资源管理器/开始菜单里那个文件图标），`resumed()` 里再用 `Icon::from_resource(RESOURCE_ID, ..)` 显式挂 `with_window_icon` + `with_taskbar_icon`（标题栏/Alt-Tab/任务栏）。**两条都要走**：winit 0.30.13 注册窗口类时写死 `hIcon: 0`（`platform_impl/windows/window.rs:1417`），**不会**去读 exe 的资源图标，只嵌资源的话后三处永远是空白默认图。从资源段按序号取而不是 `include_bytes!` 再解码：同一张 ico 在 exe 里只有一份（370 KB，N6），且尺寸由 Windows 从 ico 里自己挑（高 DPI 下会去拿 48/64 那几帧，比在 CPU 上重采样准）。取不到就不设，不 panic | P2 | `tests/icon_resource.rs` 三条，钉的都是「错了全都不报错」的路径：`the_resource_id_in_the_rc_script_matches_the_one_the_window_asks_for`（序号脱钩时 `from_resource` 只返回 `Err`，构建绿、文件图标还在，**只有窗口和任务栏图标静默消失**）/ `the_icon_file_carries_every_size_windows_will_reach_for`（缺档就在对应场合被最近邻放大成糊的）/ `the_window_takes_the_resource_id_from_the_shared_constant_not_a_literal`（那段代码裹在 `cfg(windows)` 里，本机 `cargo test` 一行都碰不到）。端到端另有 PE 资源目录解析验收：`RT_GROUP_ICON id=1` + 6 个 `RT_ICON`，尺寸与源文件逐条对应 |
| F156-a | **「恢复上次的现场」弹窗加关闭入口**：标题栏右上角 ×（`egui::Window::open()`，`line_segment` 画的，不进 T9 字形白名单）+ Esc，两者都回报**既有的** `HistoryOut::Dismiss`，`app.rs` 侧处置零改动。底部的「不恢复」保留——那是键盘路径的出口，× 是鼠标路径的直觉位置，删掉任一个都会让某一类用户找不到出口 | P2 | `closing_the_window_with_the_title_bar_x_reports_dismiss`（× 是线段画的，找 `Shape::Text` 的老脚手架点不中，改从 accesskit 树按 `"Close window"` 取 rect；取不到就 panic 并打出树里所有 label，egui 换版本改了标签要**当场报出来**而不是静默恒绿）/ `pressing_escape_closes_the_dialog` |
| F156-b | **换节点成功后分屏焦点跟到那块 pane**：`ws.set_focus(id)` 加在**自由函数 `rehost_pane`** 末尾（那里能拿真实 `Workspace` 直接断言 `ws.focus()`；放事件分支只能写「读 `app.rs` 源码找字符串」式的恒绿断言）。`reattach_pane`（F128 断线自动重连）**刻意不跟着改**——换节点是用户刚刚亲手发起的，断线重连是后台自愈，可能发生在用户正在另一块 pane 里打字的任意时刻，抢焦点等于把按键发到另一台机器上。只动分屏焦点，不动 egui 输入焦点 | P2 | `rehosting_moves_the_focus_to_that_pane_but_reattaching_never_does`（**对照**测试，两个方向各有一条变异命中：删 `set_focus` / 往 `reattach_pane` 里也加一句） |
| F156-c | **非 tmux 场景的 shell OSC 7 自举**：pane 的 shell channel 一建立就往 PTY 写一行，让远端 shell 此后每个提示符发一次 OSC 7。这是 F124 的 shell 版——非 tmux 时 `PaneState.cwd` 两条腿同时断（bash 默认不发 OSC 7；窗口标题那条只要 PS1 被 starship/oh-my-bash 接管就断），`Ctrl+Shift+B` 只能停在登录目录。**不写远端任何文件**（只改这条 shell 内存里的 `PROMPT_COMMAND`，断开即消失，这是能默认开启的前提）；保留用户原有的 `PROMPT_COMMAND`；`$PWD` 走 `printf '…%s…' "$PWD"` 的**参数**而非拼进格式串（拼进去时目录名含 `%` 会吐出一条**错的绝对路径**，而错的绝对路径骗得过下游所有校验）；前导空格走 `HISTCONTROL=ignorespace`（尽力而为）；末尾 `clear`（代价是 motd 被清）。开关 `shell_osc7_bootstrap` 默认开，与 F124 那个**分开**（副作用不同）。三处 pane 建立路径收成 `App::on_pane_ready`，注入排在 `start_automation` **之前**（注入串自带 `clear`）。**只在 pane 刚建立时注入**——按 `Ctrl+Shift+B` 那一刻 pane 里可能正跑着全屏 TUI，字节会变成按键 | P1 | `shell_bootstrap` 八条纯逻辑（换行/前导空格/`$PWD` 当参数/主机名段留空/保留用户的 `PROMPT_COMMAND`/bash+zsh 两条分支/末尾 clear/全 ASCII）；**`tests/shell_osc7_live.rs` 拿真 bash 跑一遍再喂给自家 `Osc7Sniffer`**——转义要穿过「Rust 字面量 → shell 单引号 → printf 格式串」三层，漏一个反斜杠远端就安静地什么都不发，测试目录名里放 `%s` 和空格钉住这一条；接线守护 `every_pane_ready_path_goes_through_on_pane_ready`（`self.start_automation(` 全文件**只许有 1 个**调用点 + 注入必须排在它前面）/ `committing_the_settings_carries_the_shell_osc7_switch`；UI 侧 `toggling_the_shell_osc7_checkbox_reports_a_preview` / `the_two_remote_switches_are_independent`（复制粘贴写错开关名会红）/ `the_osc7_draft_starts_from_the_stored_switch`。**验不了**：注入时机是否真落在 pty 缓冲窗口里、`clear` 之后的观感、高延迟代理链路下注入与登录后自动化的先后、非 bash/zsh 远端上那行报错的实际样子——全部进人工验收清单 |
| F157 | **帧循环归因**：剖面行追加 `wake=Nx/rr=sched:N,evt:N`、`dirty=行号:次数,…`、`egui_ev=Nx/f:N`、`rdelay=z:N/f:N/m:N` 四段，一次实机往返就能点名「谁在每帧置脏 / egui 收了几个事件 / `repaint_delay` 到底是什么」。**社区标准答案 `Context::repaint_causes()` 对这个成因无效**——`RepaintCause::new()` 带 `#[track_caller]`，而 egui 自动重绘的调用点是它自己的 `context.rs:2396/2398`，吐出来的只会是 egui 的内部行号；归因必须埋在我们这一侧的边界上。76 处置脏点全部收口到 `mark_ui_dirty!` 宏（是宏不是方法：有些置脏点在 `self.active` 的可变借用作用域里，调不了 `&mut self` 方法），行号由 `line!()` 拿；归因表是固定 8 槽的无锁表（帧路径上不许分配/加锁/格式化，T3），一窗口没响过的槽位归还，否则启动期的一次性置脏点会把槽位永久占死。`repaint_delay` **三分**桶：`Duration::MAX`（egui 不需要重绘）必须与「很大的有限值」（要重绘只是可以等）分开，归成一类的话两种截然不同的状态在日志里长得一样 | P1 | `profile::tests::a_huge_finite_repaint_delay_is_not_the_same_as_never`；`diag::tests` 三条（槽位分辨/槽位归还/drain 清零，用自己的表实例避开并行 runner 的假红）；接线守护 `every_ui_dirty_set_site_goes_through_the_attribution_macro`（生产段里裸置脏必须恰好 0 处）/ `every_request_redraw_records_where_it_came_from`（调用数与记账数必须相等）/ `the_wake_counter_sits_at_the_very_top_of_the_redraw_arm`（排在 `pump_io` 之后的话，最小化路径完全不计数）。**新增四段一律不进 `is_idle`**——它们在空闲时恰恰非零，进了判据就是「空闲的 mullion 每 5 秒写一次盘」 |
| F158 | **launcher 无条件出帧下线**：`let dirty` 那个 `match` 整个删掉，两态统一走 `frame::frame_is_dirty(terminal_dirty, self.ui_dirty)`（`terminal_dirty` 在 launcher 态本来就恒 `false`，天然统一，**不需要**再包一层带 `has_workspace` 的函数——那参数会被忽略）。原来那句 `None => true` 的理由是「`ControlFlow::Wait` 下 winit 不会凭空生成 `RedrawRequested`」，而它在同一函数别处会排 `WaitUntil` 的前提下不成立：present 后拿到有限 `repaint_delay` → 排 `WaitUntil` → `about_to_wait` 到点补 `request_redraw` → 闭环自激。日志坐实 `tabs=0 panes=0` 时照样 `frame=300x/present=300`。摘掉之后 `ui_dirty` 成为 launcher 态唯一判据，缓解②把后台事件的判据**反过来**：`user_event_marks_dirty` 是穷尽 `match`（加变体即编译报错），默认标脏，只豁免三种：`Wake` / `TransferProgress`（每秒几千条，标脏就是 T3）与 `EditTick`（理由不同——它的分支把 `self.ui_dirty` **当信号读**，在这里预置会让 `if self.ui_dirty` 恒真、语义静默改掉）。**必须与 F159 同版**——只做 F158 会让 76 个置脏点 : 1 个清脏点的列举式结构成为唯一判据 | P1 | `the_frame_dirty_check_is_the_same_in_both_states`（源码切片断言 `dirty` 的绑定式**恰好**是那一句；变异：改回 `match … None => true` 当场变红）；`a_background_event_marks_the_ui_dirty_unless_it_is_a_known_flood`；`the_background_dirty_rule_is_actually_wired_into_user_event`（纯函数写对了没接线是本项目反复踩过的静默失效）。**人工验收**：launcher 里点会话/开弹窗/hover/键盘选择全部要有反应；不碰鼠标发起一个连接，画面要自己更新到「已连接」 |
| F159 | **整帧指纹**：`FNV-1a(egui tessellate 产物 ⊕ 各 pane 的 F12 行指纹 ⊕ 光标 ⊕ IME preedit ⊕ 几何 ⊕ 字体样式 ⊕ 交换链尺寸)`，与上一帧相同**且 `textures_delta` 两个方向都为空**就不提交 GPU。截断点在 tessellate 之后、终端趟之前——终端侧输入那时已全部就绪，不必先付 `text_prepare` 才知道没变；egui pass 照跑不跳（它是指纹的真值来源，也是动画能推进的前提）。五条不能省的细节：①`textures_delta` 非空**强制 miss**（那是 egui 每帧 drain、只交付一次的字体图集增量，跳掉就永久丢，之后某帧引用从未上传的纹理 → 花屏或 panic，只在「先命中后未命中」的序列里发作，无头够不到）；②**preedit 必须入指纹**（画在终端文字层不在 egui paint_jobs 里，组字过程中 cells 不变行指纹不变，漏掉就是**打拼音屏幕纹丝不动**，T10 一族）；③光标闪烁吃 `gpu::style_for` 的**结果**而非裸 `blink_on`（吃裸值的话 launcher 也跟着相位每秒白出 2 帧，非焦点 pane 恒画空心光标也会被算成变了）；④`FrameFp` **刻意不 `derive(PartialEq)`**（`Unhashable == Unhashable` 会被 derive 判成 `true`，那正好是「加了 paint callback 之后屏幕永久停住」）；⑤`surface.configure` 的**每一个**调用点之后都要作废基准——项目里有两处（`render_frame` 的 Lost/Outdated 分支、`Gpu::resize`），只堵前者的话「最小化后还原到**原尺寸**」这条路上几何项没变、指纹照旧命中，会在内容未定义的交换链上跳帧，还原后黑屏或停在旧画面且全程静默（复核挖出，非设计原稿）。跳帧判断**必须留在 `render_frame` 内部**：记账全在调用方无条件执行，挪出去漏掉 `mark_presented` 一笔就是 60fps 空转且剖面 `present=0` | P1 | `frame_fp::tests` 15 条，每条配一个自证会变红的变异：内容/划选/组字串/焦点相位/launcher 不churn/egui 顶点/callback 永不命中/字体/尺寸/几何/pane 对调/delta 强制 miss/首帧必画/`GridSnapshot` 长新字段时源码切片变红；接线守护 `a_fingerprint_hit_returns_before_any_gpu_work_and_only_a_presented_frame_becomes_the_baseline`（三个 byte offset 的先后关系）/ `reconfiguring_the_surface_drops_the_fingerprint_baseline` / `resizing_the_surface_also_drops_the_fingerprint_baseline`（第二个 configure 调用点，跨文件够不着，靠 `Gpu::resize` 的文档注释指路）/ `deltas_empty_is_derived_from_both_set_and_free`（纯函数单测喂的是算好的 bool，够不着这行接线——实测硬改成 `true` 时全套测试无一变红）。运行期守护 `fp=hit:N/miss:N`（判据写错导致永远 miss 时画面完全正确、日志一切正常，只有这个比值会掉）。**验不了**：实机 CPU、是否不闪、真实字形/CJK 对齐、输入法候选框 |
| F164 | 周期 profile 行加进程 CPU%（按核数归一）与主线程 CPU%（不归一）；CPU 超阈值强制打破空闲门 | P1 | 两个口径的归一化差异有单测（多核机上「烧满一个核」不得被压成个位数）；采不到时是 `None`，既不冒充 0 也不打破空闲门 |
| F165 | GPU 三口径：PDH `\GPU Engine(*)` 本进程引擎占用率、DXGI `QueryVideoMemoryInfo` 本进程显存、wgpu TIMESTAMP_QUERY 的 GPU 帧耗时 | P1 | PDH 必须用 `PdhAddEnglishCounterW`（本地化名在中文 Windows 上静默失败）；实例名 pid 过滤须防前缀串号（`pid_1234` vs `pid_12345`）；TIMESTAMP_QUERY 条件申请，不支持时降级 `n/a` 而非 `request_device` 失败 |
| F166 | 一实例一日志文件 `mullion-<instance_id>.log`（与 F148 现场历史同源）+ 行内 pid + 运行期轮转 + 按心跳判活的配额清理 | P1 | 清理三道保险（自己按文件名硬排除、活实例不动、60 秒内不删），主文件与 `.log.1` 按 id 分组同进退；文件名解析须严格校验 id 形状，否则 F155 导出的 `mullion-redacted.log` 会被删掉 |
| F167 | **剖面归因分行**：一个 `Snapshot` 渲成一组行（概览 / `profile.load` / `profile.cpu` / `profile.mem` / `profile.gpu`），`grep "profile.mem"` 就只看内存。`load` 行给出场景标签 `scene=` 与用量分母（`tabs/panes/hosts/scroll/xfer/key/in`）——五个总量数字在没有分母时回答不了「是否合理」。**成本分两层**：Info 档常开便宜的，Debug 档才出 `profile.cpu.unmapped`（未映射线程名）和 `profile.mem.delta`（原始字节），档位直接读 `log_enabled!`，不另设开关 | P2 | `scene_of` 九档优先级单测（八条判据 + 涓流阈值）；`render_lines_contract` 断言**恰好五行**（写成 `>= 5` 时对「多出一行垃圾」恒绿，复核变异挖出）、每行无内嵌换行、`is_idle` 时返回空 `Vec`。**两处偏离设计文档，均为有意**：①§7 的 `profile.mem.panes`/`profile.mem.xfer` **逐项**明细延后——数据层只有总量 gauge，per-pane 明细要每帧构 `Vec`，为一个 Debug 档功能往热路径加分配不划算（T3）；②`Scene` 比 §3 多一档 `Unattributed`（空闲门放行了但七条活动判据全没命中）——**这一档本身就是异常信号**，F158 那次「看着空闲实则烧满一个核」如果当时有它，一眼就认得出。另有 `Idle` 档只为让纯函数对任意输入有定义，正常日志里不出现 |
| F168 | **CPU 按线程分组**：`ThreadCpuProbe` 每线程差分采样，按线程名前缀归成 `tokio/watchdog/dialog/dragout/其他`。Windows 走 Toolhelp `Thread32First/Next` + `GetThreadTimes` + `GetThreadDescription`，Linux 走 `/proc/self/task/*/{comm,stat}`。**首次调用返回 `None` 而不是 `Some(空表)`**——没有基线时给一张全零的组表，日志上「各组 0%」与「真的没在跑」长得一模一样，正是本项目明令禁止的「编一个 0 出来」 | P2 | `prefix_matches` 边界（`tokio-runtime-w` 前缀命中、`tokiofoo` 不命中、空名占位）；`thread_group_pct` 不封顶但**饱和不截断**（`12_884_901_888ns / 3ns` 恰好是 `100 × 2^32`，`as u32` 会绕回成 `Some(0)`——一个像模像样的错值，T12 同族）；`sysprobe::tests::this_platform_reports_cpu_time_that_actually_grows_when_we_burn_cpu`。**平台事实**：Linux 的 `/proc/<tid>/comm` 截到 **15 字节**（`TASK_COMM_LEN=16` 含 NUL），前缀匹配须按截断后的名字判，否则长组名在 Linux 上永远归不进去。**固有缺陷不修只记**：两次采样之间生灭的短命线程漏账；tid 复用时新线程累计值从 0 起、`saturating_sub` 把那一窗口静默夹成 0（Windows 回收 tid 比 Linux 积极） |
| F169 | **内存分块记账**：`手工记账已知大块 + 显式余量`（当时拍板不上自定义分配器——为一个诊断指标换全局 allocator，代价与风险都不成比例；**这一条已被 F190 推翻**，原因是实机日志里那 86MB 的棘轮全落在 `其他:` 这个「余量」桶里，而余量桶按定义答不出「是谁」——手工记账只能确认已知的大块没涨，剩下的仍然是一团）。`mem_parts` 渲成 `340MB = scroll:128 xfer:24 text:16 其他:172`，记账超出 RSS 时不吐负数而是 `其他:0(记账超出RSS NMB)` | P2 | `mem_parts` 四条含 `accounted == process_mb` 的相等边界（缺这条时 `<=` 改成 `<` 恒绿，自查挖出）。**单位不对称是有意的**：`process_mb` 是 MB、三个分块参数是字节，各自按 `>> 20` 截断，累计使 `其他` 略偏大（≤3MB），文档注释写明。**回溯口径**：`scrollback_bytes` 按 `grid().history_size()`（**实际已用**行数）计费，不是 `history_lines()`（配置上限）——按上限算的话空 pane 也会报几十 MB。**既有债一并登记**：`diag.rs` 里 `mem_process_mb` 是 `u64` 且采不到时写 0（F155 遗留，`1b04389`），本切片没改数据层类型，改在渲染层兜底成 `profile.mem n/a(RSS 采不到)`；哪天动那块结构体时应改成 `Option<u64>` |
| F170 | **GPU 帧耗时分层**：`TIMESTAMP_QUERY_INSIDE_PASSES` 条件申请，在**同一个** render pass 内插一个分界时间戳把整帧拆成 `term:`/`egui:` 两段（不拆 pass——拆了要多一次 load/store，为诊断指标付真实渲染成本） | P2 | 三条顺序契约全部错了也不报错、只出静默错值，详见 `docs/gui-render-gotchas.md`「GPU 分层的分界时间戳有三条顺序契约」：`mid_mark` 在 `forget_lifetime` **之前**、在 `terminal_draw` 判空块**之外**（launcher 态那些帧的槽 1 否则是残留值）、内部 `if self.split` 不可省（否则同一索引在同一 pass 内写两次，撞 `UsedTwiceInsideRenderpass`）。守护 `app::tests::the_gpu_mid_mark_sits_between_terminal_and_egui`（源码顺序；变异「挪到终端趟之前」保持字面串不变、顺序断言当场变红，「挪到 `forget_lifetime` 之后」编译期就要改成 `&mut static_pass`、红在 needle 找不到那一条上）。不支持分层的机器上 histogram 保持空 → 日志出 `分层:n/a`，与 `分层:0x` 分开。**验不了**：真实驱动上分层数字是否正确（无头容器无 GPU），只能人工拿两种负载对比升降方向 |

| F171 | **窗口事件类型归因**：概览行紧跟 `dirty=` 后加 `wev=cursor:158,kbd:2 curdup=158`——`dirty=` 只能答到「`app.rs` 哪一行置的脏」，`wev=` 接着往上答「那一行凭什么响」，`curdup=` 再答「是不是同一个坐标反复来」。实机日志里那一行每 5 秒响 158 次而用户根本没碰键鼠（`in=0B/s key=0x present=1`），三级一起才定得了性：坐标恒定 → 幽灵事件（可按位置掐），坐标在变 → 真有东西在动指针，两者修法相反。**本条只加埋点不改行为**，判据靠实机日志回来再定 | P2 | `wev::kind_of` 是**不带 `_` 的穷尽 match**（winit 0.30.13 的 `WindowEvent` 不是 `#[non_exhaustive]`，加档是编译错误而非静默归进兜底桶——「列举式门控在加档时必然漏」的根治手段），守护 `wev::tests::the_match_has_no_catch_all_so_a_new_winit_variant_breaks_the_build`；故 `wev=` 里的 `other:N` **只有一个含义**：活跃类型超过 8 个槽位。埋点必须在 `resp.repaint` **分支体内**（挪到外面就把 egui 说了 `repaint:false` 的五类算进来、与 `dirty=` 脱钩，而日志照写、数字照有、只是指向错地方），守护 `app::tests::the_event_kind_is_attributed_only_when_egui_actually_asked_for_a_repaint`（字节偏移判据，「包含某串」对这个变异恒绿）。dup 判据用 **bit 相等**而非数值相等（保守侧：只漏报、不误报；误报会让人以为「按坐标去重是安全的」）；`CursorTracker` 的 `last`/`seen` **取数时不重置**（跨窗口的静止仍是 dup，重置会让 dup 率随采样频率变化）；码 0 是 `KeyTable` 的空槽标记，不许被任何事件占用。三段**一律不进 `is_idle`**（同 F157 约定：空闲时恰恰非零）。**实机结论**（2026-08-28，v0.1.78，34 分钟 / 286 个窗口）：`curdup=0` **无一例外**——**幽灵事件假设证伪**，3866 次 cursor 事件的坐标全都在真的变化，「按位置掐」那条修法作废；真凶是**真实事件被 egui 的 `repaint_delay = ZERO` 放大**（`rr=sched:` 恒大于 `evt:`）。这条埋点达到了设计目的——一次实机往返就把两种相反的修法分了开——修法本身转 F178 |
| F172 | **顶点层行带差分**：每 pane 把行切成 16 行一带（`bands::BAND_ROWS`），一带一个 `TextRenderer`、共用同一个 `TextAtlas`，**只把脏带交给 `prepare`**，全部带照常 `render`。补的是 F12 与 F159 之间那一层：实机 `reshape=hit:329/miss:1`（330 行只变 1 行、整形几乎全命中）而 `text_prepare` 仍要 8~16ms——glyphon 0.7.0 的 `prepare` 开头就是 `glyph_vertices.clear()`，把传进去的**每个字形**重走一遍 LRU 查找 + 顶点 push，省整形省不到这一笔。概览行同版加 `bands=脏/总 seg=连通段数` | P2 | **脏带判据 = 带级指纹**（`bands::BandInput` 穷尽解构，加字段是编译错误），**不是「带内有行走了 `RowPlan::Reshape`」**——后者是原因侧（ADR-011「判在结果上不判在原因上」），漏三类：pane 移动/改大小（`left`/`top` 每帧现算、整形全命中，但顶点里烤着绝对坐标）、换主题（`default_color` 变而行内容没动）、组字（preedit 走临时 buffer、不进整形缓存）。三类症状都是「某一块留着陈旧的字」，编译/测试/日志全静默。**`trim()` ⟺ 本帧所有带都重建了**（`bands::may_trim`）——glyphon 的图集淘汰只保护 `glyphs_in_use`，`trim()` 清空它、靠随后的 `prepare` 填回；干净带的字形失去保护被踢掉、槽位让给新字形，那一带的旧顶点就指向别的字形，**画出别的字且不报错**。撞 `AtlasFull` 时全带指纹作废 + `force_full` 自愈。守护：`bands::tests::plan::*` 七条（顺序/单行只脏一带/pane 移动全脏/换主题全脏/组字只脏光标带/静止零重建/强制全量）、`text::tests::the_atlas_is_trimmed_only_behind_the_full_rebuild_gate`（行下标邻近判据，「文件里包含 may_trim」对「把 trim 挪出 if」恒绿）、`a_clean_band_never_reaches_prepare`、`an_atlas_full_invalidates_every_band_not_just_the_one_that_hit_it`、`profile::tests::the_band_diff_counts_reach_the_line`。**debug 档影子校验**：从**结果**侧（实际 `TextArea` + 已 shape 字形）再推一遍摘要，与从**输入**侧算的指纹互为独立证据，判成干净的带摘要变了就 `error!`；默认关闭（它要走一遍每个字形，正是本条要省的开销）。**固有边界，不是待办**：滚动态零收益（顶点烤着绝对 y，滚一行则全带脏）。**实机回报**（2026-08-28，v0.1.78）：主场景（远端输出）`bands=` 脏 50 / 总 990，**跳过 95% 的带**——「滚动态零收益」那句仍然成立，但它不是全貌，主场景的收益很大，别据此以为这条投资没回本。**验不了**：屏幕上有没有留着陈旧字形（只有人眼能判），交付人工验收清单（拖分屏分界线/切标签/换字号字体/换主题/打中文/滚回历史 + 打开影子校验档跑一趟）。**2026-08-28 人工验收通过**（v0.1.78 数日日常使用未见陈旧字形；F174 空行合并的画面正确性同批确认） |
| F173 | **per-pane 归因行**：新增 `profile.pane p1 in=6.5KB@b11,b23/24 frames=5 \| p2 …`，一个 pane 一段：这一窗口它收了多少远端字节、重建了哪几条带（F172 的带号，取**并集**）、共几带、参与了几帧。补的是 F155~F172 那一串埋点共同的盲区——`in=`/`bands=脏/总`/`frame=` 全是**跨 pane 拍平的标量**。实机三节点静置日志里每 60 秒来一次 `in=3.9KB/s bands=4/72 seg=3`，看得见有东西在动、指不出是谁在动、动在屏幕哪一块，而「三个 pane 各自的 tmux status-line 在跳」与「某一个 pane 在刷屏」修法完全相反。**本条只加埋点不改行为** | P2 | **归因表与 `KeyTable` 刻意不共享实现**：一个 pane 挂四个字段且合并语义各不相同（字节 `+=`、帧数 `+1`、脏带位图 `\|=`、总带数取最新），硬塞进去要给它三套 merge、反而把那条微妙的「槽位归还」时序搞浑。抢槽判据里**没有 `id == 0` 这一项**——`PaneId(0)` 是真实 pane（`Node::Leaf(PaneId(0))`），把它的占用槽当空槽的话后来者会连它攒的字节一起抢走，静默丢账且丢的总是 pane 0；空槽本来就满足「零字节零帧」，不需要哨兵。安静判据必须是**零字节且零帧**：只看字节的话，没有远端字节但顶点在重建的那三类帧（pane 移动/换主题/组字，正是 F172 最易漏的场景）会被反复抢槽、位图永远残缺。`drain` 必须**先读 `id` 再 `swap`**（同 `KeyTable::drain` 的承重顺序）。字节计数点从 `session_pump.rs` 搬到 `Workspace::pump`——纯件不认识也不该认识 `PaneId`；**全局 `in=` 与 per-pane 走同一个 `count_inbound` 写入点**，拆两个函数的话漏调一个就让「`in=` 有数但 `profile.pane` 是空的」在日志里长成「这个 pane 没说话」，而那正是本条要答的问题。`count_bands` 改收 `&[BandPlan]`（`key` 本就是 `(PaneId.0, 带号)`，per-pane 是现成数据，以前被 `plans.len()` 拍平扔了），折叠依赖 `plan_bands` 的「同 pane 的带连续」；带号 ≥64 折进最高位（`1u64 << 70` 在 release 下绕回去会把第 70 带报成第 6 带）。渲染侧三条：脏带为空印 `-` 而不是省略（省略则「差分全命中」与「埋点漏了」同形）、截断留 `+N`（不留痕则「脏 4 带」与「脏 40 带」一样长，而后者意味着差分失效）、**一个 pane 都没动就整行不印**（静置日志的价值一半在于安静）。守护：`diag::tests` 六条（按 pane 归字节/安静归还槽位/无流量不入行/多帧脏带取并集/零字节但重画过仍出账/带号折叠）+ `a_frame_of_band_plans_is_split_across_the_panes_that_own_them` + 接线自证 `pumping_a_pane_charges_the_bytes_to_both_the_global_and_that_pane`；`profile::tests` 五条（格式与排序/无读数不印行/零脏带印分母/截断报剩余/溢出字节报 `其他`）。走 `take_snapshot` 的用例一律走 `SNAPSHOT_LOCK` 串行（它把进程级 static 取空，并行 runner 下互偷计数 = 概率性假红）。**验不了**：这行在实机静置日志里指出的凶手是谁——本轮交付到「问得出来」为止 |
| F176 | **内存口径收口**：`profile.mem` 改报 `commit=428MB(ws 289) = scroll:0 xfer:0 text:5 其他:423`。原先只报 `PrivateUsage`（提交量），与任务管理器进程页「内存」列（**专用**工作集）同一时刻差着一百多 MB，对不上所以没人信，N5 那轮排查因此绕开日志直接上 VMMap——**这才是这条要修的东西** | P2 | ws 取 `PROCESS_MEMORY_COUNTERS_EX2.PrivateWorkingSetSize`，**不是 `EX.WorkingSetSize`**（后者是含共享页的总工作集，对照照样对不上、只是差到另一个方向）；同一次 `K32GetProcessMemoryInfo`，零额外系统调用；`EX2` 要 Win11/Server2022，老系统回落 `EX` 并报 `(ws n/a)`。`PrivateWorkingSetSize == 0` 当采不到——结构体是 `zeroed()` 出来的，0 是「没被填写」的唯一痕迹，而活进程的专用工作集不可能真为 0。**ws 刻意不参与减法**：工作集会被系统裁剪（最小化窗口尤甚）而记账块是堆上的 `Vec`、不随换出变小，拿它做被减数会把正常系统行为报成「记账超出」刷屏；减法算在 commit 上。平台差异下沉成 `diag::MemKind`，**渲染层不带 `#[cfg]`**，Windows 形态能在 Linux 开发机上单测。守护：`profile::tests` 五条（Windows 双数／Linux 单数不印 `(ws …)`／回落印 `n/a`／`the_remainder_is_computed_against_commit_not_the_working_set`／既有的余量四态）+ `the_mem_line_carries_the_working_set_from_the_snapshot`。**验不了**：ws 与任务管理器是否真的一致、老 Windows 的回落路径——交人工验收 |
| F177 | **N5 预算闸**：ws 越过 `N5_BUDGET_MB`（300）时单独写一行 `WARN profile.mem.over ws=428MB > N5 300MB (commit 512, 其他 507)`，跌回后写一行 `回落`。补的是「`其他:423` 在日志里躺了几百次、和一切正常时长得一模一样」——数据一直都在，缺的是它自己会叫 | P2 | **判据必须是绝对阈值，百分比那条路是死的**：只记 `scroll/xfer/text` 三种内容缓冲，基线的 GPU／字体／代码从不在账上，空载时 `其他` 天然接近 100%、健康版本也一样，按占比报警等于常亮。越界判 `>`（恰好 300 不算超）；越界后只在比上次报告值又高 ≥64MB 时再报（**不用 `diag::should_report` 那套翻倍**——300→600 才吭第二声，中间几百 MB 的慢泄漏全程静默）；**回落判 `ws <= 300-16`，滞回带不可省**：没有它，ws 在 299↔301 抖动（Windows 主动裁剪工作集时常见）会让每次穿越产出一对 `Cross`/`Recover`，空闲进程每几个窗口写两行、硬盘永不休眠，正是「这条可以穿透空闲门」所依据的前提被推翻。**调用点在 `log_enabled!(Info)` 门之外**：关进门里则 warn 档听不见警报，且会连带被 `render_lines` 的 `is_idle()` 挡掉——而空载恰恰是空闲门拦掉的那一类，也正是要查的场景；挪进去之后编译过、纯函数测试全绿，只有实机空载才发现它不响，故守护用**行序**判据（`the_budget_gate_is_asked_before_the_info_log_gate`），「文件里包含 `budget_verdict`」对这个变异恒绿。`ws_mb == None` 恒 `Quiet`（同 `cpu_is_busy` 的 `is_some_and`：读不到的机器不许凭空报警）。守护：`profile::tests` 五条（状态机／抖动不写盘／严格大于／采不到不报警／两种告警正文）。**实机回报**（2026-08-28，v0.1.78，34 分钟）：`ws=226MB < 300MB`，0 次 `profile.mem.over`，与「没超就不该响」一致；`commit=323MB` 同时超 300 而没误报，证实「判据取 ws 不取 commit」这个选择在实机上是对的 |
| F174 | **整形缓存改内容寻址**（补登记，已随 v0.1.76 交付）：键从 `(PaneId, row)` 改成 `ShapeKey { hash, term_w }`。F12 原来**用位置当键、用内容当有效性判据**（hash + term_w 存在条目里比对），滚一行则每行行号都变、整块 pane 全作废，而那些行的文字一个像素没动。**这类 bug 的通用判据：缓存的键和「什么时候该失效」如果不是同一个维度，就一定有一整类输入让它白白全 miss，而画面完全正确。** 改后滚一行只 miss 新露出那一行，附带全窗口空行共用一条、分屏跨 pane 共享 | P1 | **两处「删掉才对」**：`recycle_row`（未命中时摘同键旧载荷）在内容寻址下是**空操作**——miss 只在「这份内容没见过」时发生，不存在同键旧条目，删掉后 `end_frame` 成为唯一回收点；`CachedRow` 里的 `hash`/`term_w` 副本删掉——键里已有，存两份会漂移而漂移的症状是**画面陈旧且完全静默**。**内容寻址会静默毁掉建在它上面的诊断**：`seg=`（连通段数，用来调 `bands::BAND_ROWS`）原先从 `RowPlan::Reshape` 分支收 `changed_rows`，内容寻址之后这个集合的含义从「哪些行内容变了」退化成「哪些内容没缓存过」——滚动时它只有 1 个而实际整屏在动，编译/测试/画面全静默、**只有这个数字在骗人**；处置是另建 `row_fp.rs`（位置寻址的 `(PaneId,row) → hash` 台账，纯诊断），职责切开。接线层三条变异只发生在 `text.rs::prepare_panes`（需真实 wgpu Device/Queue，无头跑不起来），单测一条都杀不掉：`term_w` 写死／`hash` 写死／行指纹改用当帧下标；补源码切片守护（判据：唯一构造点 + 必须用字段简写，`term_w: 0` 这类显式赋值一律不认）。`term_w` 写死的症状是**分屏拖宽后文字永久按旧列宽排且不自愈**。**实机回报**（2026-08-28，v0.1.78）：`reshape=hit:14469/miss:51`，命中率 **99.6%**（改前 82.6%，miss 从 1254 降到 51）；`text_prepare` max 从 65.5ms 降到 16.4ms、典型窗口 p95=2.0ms |
| F175 | **`egui_feed=` 与阶段 p50**（补登记，已随 v0.1.76 交付）：概览行加 `egui_feed=Nx/p50=/p95=/max=`——喂给 egui-winit `on_window_event` 那一趟单独的账，它被 `window_event=` 段**包含**，两者相减才是「路由判定 + 终端分支 + 标脏」的开销；各阶段段同时报 p50 与 p95 | P2 | **为什么另开直方图而不是插一个 `Stage`**：`StageClock::mark()` 记的是**离开上一阶段**的时长，在 `window_event` 中间插一次 `mark` 会让 `window_event=` 的样本数凭空翻倍，把一个已有归因数字的含义改掉。**为什么必须补 p50**：桶是 log2 的，只有 p95 时「937x/p95=1.0ms」的总账落在 0~19% 之间任何位置都说得通——那个区间宽到没法据此决定优化方向，这也正是当时只补埋点、不动重绘门控的理由 |
| F178 | **白跑帧收口(已量化,判为不值得做——保留证据,不实现)**：整帧指纹命中的帧仍然跑完了 CPU 侧的 tessellate + `text_prepare` + 指纹计算，只省下 GPU 提交（`present=0`）。起因是一个离群窗口：`frame=170 present=27 wake=431 rr=sched:270,evt:165 wev=cursor:153`，浪费率 84%、主线程 4%。**把同一份日志按 286 个窗口聚合之后，这个判断反了**：白跑帧的 CPU 上限是 **0.27%~0.95% 单核**（全部按 `frame` 的 p50 / 全部按 p95 两端夹，分母是 23.9 分钟写盘窗口），而这一块的收口点正是 `ControlFlow::WaitUntil` 那段 —— T3、T7、F158 三次事故的同一处代码。**风险与回报倒挂，本条不做**，把量化结论留在这里，免得下次又从某个离群窗口重新立项 | P3 | **分场景聚合（v0.1.78 实机，34 分钟 / 286 个窗口）**：<br>`ui-only` 52 窗 / 801 帧 / 白跑 **86%**，但帧 CPU 合计只有 **0.4 秒**、`main` 均值 **0.4%**；<br>`remote-output` 203 窗 / 13559 帧 / 白跑 14%，帧 CPU **25.1 秒**、`main` 均值 4.4%；<br>`typing` 28 窗 / 白跑 28%、帧 CPU 3.5 秒。<br>**白跑帧占比最高的场景恰好是 CPU 最少的场景** —— 那个 `frame=170` 的窗口是用户连续挥了 5 秒鼠标，不是常态。**近空闲态本来就已经很好**：46 个窗口 `frame≤12`，其中一批是 `frame=1 present=1 wake=1 in=20B panes=3 main=0%` —— 3 个 pane 连着、每 5 秒只醒一次画一帧，没有任何自激。**F158 那个自激在 v0.1.70 已经修掉了**，残留的放大只在用户真的在动鼠标时出现。<br>**方法论教训（比结论值钱）**：这条最初是从**单个最坏窗口**立的项，写进 spec 时带着 84% 浪费率和「省了 GPU 没省 CPU」的推理，读起来完全成立；聚合一算才发现那个场景总共只花 0.4 秒 CPU。**离群窗口能证明「这条路径存在」，不能证明「这条路径值得修」——立项前先按场景聚合一遍，代价是十行 Python。**<br>**若将来真要做**（判据：某个场景的白跑帧 CPU 单独超过 1% 单核）：约束仍然是原来那几条 —— 不许只掐调度路径（帧闸正在吸收自激，掐一半会让帧数不降反升）、判在整帧指纹连续命中上而不是事件类型上（鼠标移动会改 hover/光标/tooltip，那些是真实视觉变化）、`textures_delta`/`terminal_dirty`/`surface.configure`/`AtlasFull` 强制复位、T7 三分支显式复位 control_flow。<br>**另一条独立线索仍然挂着**：IME 事件放大 1:4.1（`ime:15 → wake=62 → frame=33`），出现在 `key=0x` 的窗口（用户没打字却在收 IME 事件），全局 798 次。它落在 T10/F149 那片，单独一轮处理 |
| F179 | **CPU 口径的分辨率**：`profile.cpu` 的 `total=` 是 `Δns×100/(窗口×核数)` 截断取整（`sysprobe::cpu_pct`），16 核机器上 **1 个显示百分点 = 0.16 个核，单核 16% 以下全部显示 0%**——而 N1 要求「空闲 CPU < 1%（单核）」折成归一口径是 0.0625%，比最小刻度小 16 倍。**达标与超标十五倍长得一模一样**：实机 34 分钟 286 个窗口，`total` 的 p50/p95/max 全是 0%。N1 目前**没有观测能力**，不是「已达标」 | P1 | 两个口径都要提精度（`main=` 不归一，但 1% 单核的阈值同样落在整数分辨率的边界上，实机 `ui-only` 场景 p50=0%/p95=2%/max=4% 读不出结论）。**判据：N1 的验收必须能从一行日志直接读出来**，否则这条指标永远无法收口——报一位小数、报千分比、或直接报窗口内的原始 CPU 时间增量（ns），三选一皆可，但不许再出现「0% 同时兼容两种事实」。**连带发现，一并处理**：`IDLE_CPU_PCT = 5` 是**归一后**的阈值，16 核机器上等于 0.8 个核——F164 那条「CPU 超阈值强制打破空闲门」的保险在多核机上事实上从未触发（实机 `main` 最高 12%，`IDLE_MAIN_CPU_PCT = 20` 也没触发过）。因此**「窗口没写盘」不能推出「那一窗口达标 N1」**，空闲门只保证 total<5% 归一；任何拿「未写盘窗口数」当 N1 证据的推理都是错的，注释里要写死这一条 **已实现**（v0.1.79）：`cpu_pct(Δ, 窗口, 核数) -> u8` 换成 `sysprobe::cpu_bp(Δ, 窗口) -> u32`（**单核万分比**，不归一不封顶、饱和而非截断），`profile.cpu` 报 `total=0.63% main=96.00% cores=16`；`cores=` 是为了让单核口径能换算回归一口径（不印的话「180% 单核」在 4 核机和 32 核机上是两件事，而日志里看不出是哪台）。**线程分组表一并升到同口径同精度**——归因行的分辨率不能比被归因的那个数还粗，否则 `total=` 超标时每组都印 `0%`，「谁在烧」当场答不了、又得多一轮实机往返；升完 `thread_group_pct` 与 `cpu_bp` 完全同义，删掉。空闲门改成与核数无关的绝对值 `IDLE_CPU_BP = 5_000`（半个核），并把「这道门比 N1 松五十倍、『未写盘』不是达标证据」写成一条**负面守护测试**（`a_window_ten_times_over_n1_still_counts_as_idle_so_silence_is_not_evidence`）——它在有人为了「让 N1 可验」去收紧空闲门时变红。八处变异逐条自证全红 |
| F180 | **脱敏器的 token 收录判据**：`profile.pane` 的段格式 `in=<字节量>@<脏带表>/<总带数>`（F173）与 `user@host` 启发式**同形**——`replace_user_at_host` 把 `in=0B@-/6` 里的 `0B` 收成用户假名、把空脏带占位 `-` 收成主机假名（`is_host('-')` 为 true）；`-` 一旦进了 map，`replace_known_bare_tokens` 就把**整份日志里所有孤立的 `-`** 换成同一个假名。实测 34 分钟日志：184 处 `wev=-`、184 处 `dirty=-`、439 处 `@-/6` 全变成 `host#2`——**凭空多出一台主机**，而 `-` 恰恰是 F157/F171/F173 三处埋点明定的空表占位符（`profile.rs:507`：「空表报 `-` 而不是空串——后面什么都没有 = 埋点根本没接上」）。脱敏把那个占位符变成假主机名，**正好摧毁它存在的理由**。字节量每窗口不同 → 每窗口造一个新用户假名（`user#265`→`#266`→`#267` 逐窗口递增）——**假名编号单调跑飞本身就是模式匹配抓错东西的信号** | P2 | 修法两层，缺一层都堵不住：①收录判据——候选须含 ASCII 字母数字、长度 ≥2（单字符 token 一律不收：即便真是单字母主机名，泄露量为零，而它与占位符/分隔符不可区分），且 `@` 前的「用户名」**不得匹配字节量模式**（`^\d+(\.\d+)?(B|KB|MB|GB)$`）——只堵 `-` 的话 `0B`/`6.5KB` 照样每窗口造新假名、编号照样跑飞；②回归判据——喂真实的 `profile.pane` 行（`in=0B@-/6` 与 `in=6.5KB@b11,b23/24` 两种形态都要）进 `Redactor`，输出与输入**逐字节相等**；另加「一份典型日志脱敏后假名总数有上界」的守护，编号跑飞当场红。**fixture 必须用真实日志行，不许手编**——手编的行不会踩中「自家格式与启发式同形碰撞」，那正是这个 bug 没被既有测试抓住的原因 |
| F182 | **线程枚举改缓存句柄**：`ThreadCpuProbe` 原先每 5 秒调一次 `CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD)`，**那是全系统快照**（`th32OwnerProcessID` 过滤是在拿到几千个线程之后才做的），然后对本进程的每个线程 `OpenThread` + `GetThreadDescription` + `CloseHandle`。实机 v0.1.79 日志（79 分钟 / 332 个窗口）里，看门狗线程吃掉了**全部被测 CPU 的 32%**（20.7 / 65.7 核·秒，p95 4.32%、max 8.65% 单核），而近空闲窗口的 `watchdog/total` 中位数是 **100%**——测量本身就是空闲期唯一的负载，N1 量的是尺子 | P2 | 改成**缓存句柄**：`OpenThread` 的结果连同 tid、线程名一起留在 `Vec<CachedThread>` 里，每窗口只做 `GetThreadTimes`；`RESCAN_EVERY_WINDOWS = 12`（一分钟）或上一窗口有任何一次读失败时才重扫。**重扫判据与新线程记账判据都提成不带 `#[cfg]` 的纯函数**（`needs_rescan` / `NewThread`）——gate 进 `#[cfg(windows)]` 的话开发机上一行都跑不到，只能靠「交叉编译过不过」来「验证」，而那验的是语法。新线程进表的第一个窗口**只建基线不记账**（`NewThread::BaselineOnly`）：它的 `GetThreadTimes` 是自出生起的累计，最多可能是一整分钟，照 Linux 分支那样 `ChargeFull` 会印出「某线程 1200% 单核」。**缓存为空一律返回 `None`**——空表只可能是枚举失败，让它走到差分层会得到 `Some(空表)`、到分组层就是「各组 0.00%」，比 `n/a` 难发现得多；这行在 `#[cfg(windows)]` 里，行为测试够不着，守护是**位置判据**的源码切片（早退必须在 `diff_and_rebase` 之前，「文件里出现过 `is_empty`」对「挪到差分之后」恒绿）。`Drop` 关句柄；线程退出后旧句柄上的 `GetThreadTimes` 仍成功（返回终值），所以陈旧条目表现为「该线程的 delta 恒为 0」而不是报错——这是接受的：最长一分钟后重扫会清掉它 |
| F183 | **采样顺序**：`cpu.sample()`（进程/主线程 CPU）必须排在 `gpu.sample()` 与 `threads.sample()` **之后**。原先排在最前，于是本窗口的探针开销落到**下一个**窗口的 `total=` 里，而线程表当场就含着它——同一行出现「某个线程比整个进程还忙」。实机 v0.1.79：**331 个窗口里 63 个（19%）`main + 各组 > total`，最坏一处 `watchdog:8.65%` 对 `total=2.47%`**。归因表在尖峰窗口上不可读，而尖峰窗口正是要读它的时候 | P2 | 守护用**行序**判据（`the_process_cpu_is_read_after_the_probes_it_should_account_for`），「文件里包含 `cpu.sample(`」对这个变异恒绿。判据须先剥掉注释行再切片：第一版写完当场红，因为 `src.find("cpu.sample(")` 命中的是**说明这条规则的注释**、位置在真调用点之前几十行——反过来更坏，注释里随手写一句 `gpu.sample()` 就能让顺序判据永远绿 |
| F181 | **探针自陈**：`profile.cpu` 行加 `probe=cpu:12us/gpu:1.3ms/thr:53ms`——三段各自那一趟的墙钟。F182 立项时源码读不出「Toolhelp 快照与 PDH 的 `\GPU Engine(*)` 通配枚举哪个才是大头」，两个都是全系统枚举；与其猜，不如让下一份日志自己回答 | P2 | 三段用 `Some` 而不是留 `None`——这三趟都跑过了，「跑了但快到量不出来」是 `0us`，与「没跑」是两回事（本文件其余埋点是「采不到 ≠ 0」，这里恰好反过来）。三段**不进 `is_idle`**（同 F157 约定）。**唯一「写错了照样全绿、日志照样有数、只是数全错」的地方**是三个 `saturating_sub` 的被减数：抄错一个就读成「GPU 探针 53ms」而真凶是线程枚举，**而这三个数存在的全部理由就是指认真凶**；守护 `each_probe_is_timed_against_its_own_start_not_a_shared_one` 逐字段比对被减数，源码切片是唯一测得到它的手段（运行期三个数都合法） |
| F184 | **时间列混排目录与文件**：按修改时间排序时不再「目录恒在前」。按 mtime 排是在问「最近动过什么」，在这个问题下目录和文件同等重要；分组之后刚建的目录永远沉在整段文件下面，与「最新的排最前」直接冲突。**其余四列（名称/大小/权限/属主）保持目录恒在前不变**（设计 D21）——那几列问的是「找哪个东西」，分组是导航的一部分 | P2 | 两条对照守护：`the_mtime_column_interleaves_directories_with_files` / `every_column_other_than_mtime_still_groups_directories_first`（后者是**完备性**方向：把分组条件写成只豁免某一列之外的写法在它下面变红） |
| F185 | **右键先夺焦点再粘贴**：分屏下在 pane 2 上右键，内容会贴进上一次有焦点的 pane 1——右键那一支此前没切焦点，落点取的是 `effective_focus()`，**没有报错也没有日志**。切焦点提成 `focus_pane_under_cursor`，左键划选与右键粘贴共用，两边都在「正事」之前调 | P1 | `a_right_click_takes_focus_before_it_pastes_so_the_text_lands_where_you_clicked`（**位置判据**：夺焦点必须排在 `request_paste()` 之前；「右键分支里出现过 `focus_pane_under_cursor`」对「顺序写反」恒绿） |
| F186 | **修改时间按本机时区显示**：`mtime_text` 原先直接用 `from_unix_timestamp`，恒 UTC——东八区用户看到的每个文件都早 8 小时，且没有任何报错。时区偏移在 `main` **最前面**取一次（`localtime::init`），换算与格式化提成纯函数 `localtime::format_unix(secs, offset)` | P1 | **`init` 必须排在起 tokio 运行时和看门狗之前**：Unix 上 `current_local_offset()` 只在单线程进程里成功（`localtime_r` 的 soundness 闸门），晚了就静默退回 UTC；而 Windows 那边**没有**这个闸门——顺序写错只坏 Linux/macOS，在唯一的一等公民平台上反而看不出来。守护 `the_offset_is_captured_before_the_process_grows_a_second_thread` + `the_same_instant_renders_as_different_wall_clocks_in_different_zones`。**两层各扎一次**：只钉纯函数时，把 `mtime_text` 改成 `format_unix(secs, UtcOffset::UTC)` 全绿——而那正好等于修复没做。全局 `OFFSET` 是 `OnceLock`，只许一条用例设（多一处会在并行 runner 下随机红）。代价已认下：运行中改系统时区/跨夏令时要重启才更新 |
| F187 | **本地收藏夹改成全局一份**：搬到 `Settings::local_bookmarks`（`settings.toml`），`sessions.toml` 的 schema 不动。`D:\work` 是这台 Windows 上的目录，跟连的是哪台远端毫无关系；F154 当初照着远端书签的样子把它挂在 `SessionRecord` 下，代价是同一个本地目录要在每条会话里各收一次。与 `font_family` 同一条理由：本机偏好，导出会话给同事不该把 `D:\我的项目` 带走 | P2 | 首次启动把各会话名下的老列表并进来，**带 `local_bookmarks_migrated` 标记只做一次**——每次都合的话，用户取消掉的收藏会从没清理的会话记录里长回来，成为一个删不掉、看不出原因的收藏。迁移挂在 `finish_store_open`，**库没打开就一步都不做**（否则密码错一次就把那份老收藏永久判死）。本地栏的 ☆ 不再看 `session_bound`（快速连接开的标签也能收），远端栏照旧置灰。收藏变动后推给**全部**标签的 `PanelFrame` 副本——`sync_local_bookmarks_to_tabs` 的**调用点和实现各扎一次**：只钉「add/remove 里调了 sync」时，把实现换成只改活动标签仍然全绿，而那就是「在标签 1 收的目录，标签 2 要重开才看得见」 |
| F188 | **恢复现场的叶子首次挂载不是「换节点」**：三屏恢复时 100% 有一屏永久停在「N · 连接中…」。F162 的串行拨号队列复用了换节点那条路径，而 `rehost_pane` 开头 `let Some(p) = ws.pane_mut(id) else { return false }`——`apply_saved_tree` 只把叶子 id 建进**树**里，**从不建 `PaneState`**，于是刚拨通的连接被原地丢掉。判据不能是「有没有 `PaneState`」（那是被这个 bug 改变的量），要在**发起那一帧**就定死：`PendingRehost` 带 `RehostKind::{UserPicked, RestoreFirstMount}` | P0 | 首次挂载分支现场 `attach_pane` 一个 `PaneState`；**焦点语义相反**——`UserPicked` 是用户刚刚亲手发起的，夺焦点；`RestoreFirstMount` 必须保住 `apply_saved_tree` 摆好的那个焦点（否则最后拨通的那一格抢走焦点，恢复出来的焦点永远是队列末尾那块，与 F156-b/F128 同一条纪律）。守护三条：`a_restored_leaf_with_no_pane_state_yet_gets_one_instead_of_being_thrown_away` / `a_first_mount_keeps_the_saved_focus_but_a_user_picked_rehost_still_takes_it`（**对照**）/ 源码切片 `the_restore_queue_asks_for_a_first_mount_and_the_title_bar_asks_for_a_user_pick`（两个调用点各自传对了 kind；纯行为测试够不到接线） |
| F189 | **收藏夹不再被另一实例/编辑器旧快照整份覆盖**：升级后收藏「全没了」是**两个**独立成因叠加。①`Vault` 开机把 `sessions.toml` 整份读进内存，此后**只写不读**——多实例并存时，A 的下一次 `save` 拿开机快照整份盖掉 B 期间写的一切；②会话编辑器的 `update()` 用表单 draft 整份替换 `rec.sftp`，而路径条上收的书签从来没进过那份 draft | P0 | ①**每个 mutator 落盘前先重读**：`sync_from_disk_if_untouched()` 排在 18 个 mutator 的第一句，配**机械完备性**测试 `every_mutating_entry_point_reloads_before_it_writes`（列举式门控在加档时必然漏，本项目第四次踩）。「手上有没有还没落盘的改动」用**结构式判据**——现在序列化一遍跟 `synced_toml` 比，不是每个 mutator 各自举手的脏标记；有未落盘改动就不重读，护的是 F2 导入那条「连着 add 十几条最后统一 save」的路径。`save` 结尾必须把 `disk_toml`/`synced_toml` 双双对齐到刚写出去那份，否则「有未落盘改动」恒为真、重读**从此静默停摆而不是报错**（这条变异的守护里，A 存盘前那一步必须是**真的内容改动**，拿一句 `set_group(id, None)`（值本来就是 `None`）当那一步的话序列化结果没变、基准对不对齐都一样，变异逃得掉——已实测）。重读时发生了什么走 `take_reload_notes()` 交给 app 记日志（store 零 UI、连 `log` 都不依赖），排水口选在 `save` 而不是各 mutator 边上：mutator 有十几个，漏一个就少一类痕迹。②`update()` 把书签摘出来再放回去，另开 `set_bookmarks` 专写；`SaveIntent` 带 `bookmarks: Option<Vec<_>>`（**`None` 不是「一条也没有」，是「这次保存不管书签」**），脏判据 `bookmark_table_touched` **只比书签这一格、不复用 `is_dirty`**——复用的话改个端口号就把路径条上收的书签一起抹掉。接线守护用**窄窗口**源码切片（只看构造点前 600 字符），扫全文件时 `use` 那行就能把断言喂饱 |
| F190 | **`profile.mem` 报 Rust 堆用量**：全局分配器换成带记账的 `CountingAlloc`，`mem_parts` 渲成 `commit=428MB(ws 289) 堆=96MB = scroll:0 xfer:0 text:5 其他:423`。补的是 F169 留下的洞——实机 52 分钟日志里 `其他:` 单调涨了 86MB（棘轮式，台阶落在 `WindowEvent::Resized` 上），而 `其他:` 是**减出来的余量**，涨了只说明「不是那三个已知桶」，指不出是谁。加一个堆内/堆外的切分，才能把「Rust 侧漏」和「wgpu/驱动/字体等堆外」分开——这是两拨完全不同的修法 | P2 | **`堆=` 印在 `=` 左边、与三个分块并列不得**：它与 scroll/xfer/text 是**包含**关系不是并列关系（那三桶本身就在堆上），印到右边会诱人拿它去减 `其他:`，把 scroll/text 重复扣两遍。守护 `mem_parts_renders_*` 五条 + `the_mem_line_carries_the_working_set_and_the_heap_from_the_snapshot`。**记两个单调计数器而不是一个带符号净值**：跨线程时 `alloc`/`dealloc` 交错，净值一旦短暂走负，`AtomicU64` 会绕回成天文数字（F168 那一族的又一例）；读出来时才 `saturating_sub`。`realloc` **先销旧账再记新账**，返回 null 时一格都不动（那时旧块还活着）。`Counters` 做成可多实例**专为测试**：1600+ 条测试并行跑，进程级全局计数上的绝对增量测不准，四条数值守护各自用 `Box::leak` 的私有计数器断言**精确相等**而非容差。`#[global_allocator]` 挂在 `lib.rs` 而不是 `main.rs`——测试链接的是 lib，挂错地方计数器在测试进程里恒 0、数值守护全部退化成「0 约等于 0」的恒绿，而生产里它照常工作、**这个错误没有任何症状**；守护 `the_process_actually_runs_on_this_allocator` 读 `include_str!("lib.rs")` 验挂载点（把它改回 `std::alloc::System` 当场红）。`s.mem_heap_bytes` **不走 `set_mem_gauges`**：那条路要有人每帧抄一遍，漏抄的症状是 `堆=0MB`，和「一个字节都没分配」长得一模一样。10 条变异全红（四个分配器方法各自不记账／纯转发、`live` 去掉 saturating、渲染层写死 0、`heap` 参与减法、换回 System） |
| F191 | **resize 分段埋点（已量化，判为不值得做——保留证据，不实现）**：F190 定位到内存棘轮的台阶落在 `WindowEvent::Resized` 上，顺势想给 `Gpu::resize`（`surface.configure` + `write_buffer`）拆段计时。**先数了一遍就否了**：v0.1.80 那份 52 分钟日志里 `Stage::Resize` 一共 5 次、每次 131–262ms，合计 **1.3 秒 / 3100 秒**。而且**拖分屏分界线根本不产生 `WindowEvent::Resized`**（只有 OS 级窗口尺寸变化才产生），埋在这里量不到用户实际最常触发的那条路径 | P3 | 同 F178 的处置：量化结论留在 spec 里，免得下次又从「台阶落在 Resized 上」重新立项。**若将来要做**，判据是「单次 `Stage::Resize` 驻留超过 500ms，或 resize 合计超过窗口时长 1%」；且要先解决「分屏拖拽走的是哪条事件」——那条路径上没有 `Resized`，得另找埋点 |
| F192 | **文字层 Buffer 记账修正 + 单价标定**：`TextLayer::bytes_estimate` 数的是 `cache.len()`（**行数**），而每个 `CachedRow` 装的是 `runs: Vec<CachedRun<Buffer>>`（**一行 N 个 Buffer**）——`row_to_runs` 把每个非 ASCII 字符单独切成一个 run，满屏框线的 Claude Code TUI 下一行 120 列 = 120 个 Buffer 却只记 1。改成 runs 求和（新增 `ShapedCache::payload_count()`），并用 `heapgauge::GLOBAL` 实测标定 `BUFFER_EST_BYTES` | P2 | **计数与单价拆不开**：4096 这个单价当初按「一行 ~200 格」拍，修完计数后被计数的单位从「一行」变成「一个 run」（大多只装一个汉字），同一个常数不可能同时对两者成立。**只改计数不会触发既有的「记账超出commit」兜底**（那条的被减数是 commit 346MB，而 text 修完约 82MB），真实症状是 `text:` 逼近甚至反超 `堆=` 这个**物理上不可能**的读数、而没有任何分支会指出来——所以配 §2.4 的第三条渲染分支（`其他:N(!记账 X > 堆 Y)`，**打标记不 assert**：`堆=` 是瞬时读数、与三桶差一帧有几 MB 噪声）。**标定不能照抄 F190 的私有计数器手法**（那对分配器不可见，只能读 `GLOBAL`），改用「信号做大到噪声之上」：整形并持有 10000 个单字 run（信号 20–40MB）× 3 轮取中位数 × 只断言 `[EST/4, EST×4]` 量级。**落地后单常数模型被自己的标定数据否掉**：实测 1 字形 2371B、20 字形 7580B、60 字形 16900B、200 字形 55920B，**跨度 24 倍**，标成哪一端另一端都错一个数量级——那等于把低报从「计数错」搬到「单价错」。改成两项 `1770 + 269 × 字形数`。代价是 `CachedRun` 多存一个 `u32 glyphs`，且它**必须跟着载荷一起进回收池**（`Vec<(T, u32)>`）——池里躺的多半是长行退下来的 buffer，按固定价计就是同一个病换个地方复发。字形数只能在整形完那一刻算（`Buffer::layout_runs`），gauge 采样时再遍历上千个 Buffer 就跑到帧路径上去了（T3），所以是**存**不是**算** |
| F193 | **quad 实例缓冲常驻**：`Gpu::quad_instances` 每帧 `create_buffer_init` 一个用完即弃的 buffer，且先 `collect()` 出一个 `Vec<QuadInstance>`——两笔都在帧路径上分配（T3）。改成常驻 `quad_buf`（`VERTEX \| COPY_DST`，容量不足按 2 倍重建）+ 常驻 staging `Vec`，走 `write_buffer` | P2 | **验收判据不是 `vram=`，是帧路径分配量/`堆=` 的空闲增长**。原分析把这条列为「250MB 里最可能立刻见效的一刀」，实测量级否掉了：`quads_for` 只在 `cell.selected` 或 `bg != 默认 bg` 时产 quad，典型帧约 200 个 × 32B = **6.4KB**（病态满彩也才 0.96MB），撑不出 250MB free-list，差四到五个数量级。**commit body 必须明写「不预期 `vram=` 有变化」**——否则改完看 vram 不动，会得出「这一刀没用」的错误结论。`quads_for` 内部那个 `Vec::new()` **不动**（改它要把纯函数改成吃 `&mut Vec`，`gpu.rs` 一批直接构造 `PaneRender` 的测试全要跟着改，不值） |
| F194 | **GPU 次分配器块大小做成旋钮，默认不动**：`MULLION_GPU_BLOCK=4,16`（单位 MB）→ `MemoryHints::Manual`；未设/解析失败 → 仍是 `MemoryUsage`。选中档位打进启动日志 | P2 | **`MemoryHints::Manual` 的基底是 `Performance` 不是 `MemoryUsage`**（`wgpu-hal-23.0.1/src/vulkan/adapter.rs:1948` 的 `..perf_cfg`）：写 `Manual{4..16MB}` 在压小 chunk（8→4MB / 64→16MB）的同时，**静默**把 `dedicated_threshold` 8→32MB、`transient_dedicated_threshold` 16→128MB 各放宽 4 倍与 8 倍。于是 20MB 的资源不再拿释放即归还的独占块，而是去挤上限只有 16MB 的 free-list、装不下再退独占，白留一块永不归还。**方向未知，所以不进默认值**（wgpu 自己的注释都写着 "not set in stone nor where they picked with strong confidence"）。守护测试要拆成两条：`memory_hints:` 那行必须引用具名函数（守「有没有显式写」，原不变量「不许退回 `Performance`」不能丢）+ 该函数的默认分支必须是 `MemoryUsage`（守「默认档是哪一档」） |
| F195 | **run 合并判据从「是不是 ASCII」改成实测 advance**：判据改为「这个字符在当前字体链下量出来的 advance 是否恰好 `width × cell_w`」，按 `char` memo 化。ASCII 成为新判据的**真子集**；框线符 `─│┌└` 在编程字体里 advance 就是 `cell_w`、会被放行（Claude Code TUI 满屏都是它，**这是本条的主要奖金**） | P1 | **不能按「同宽度 + 同回退字体就合并」**（原分析的判据）：cosmic-text 在一个 run 内部按 advance **累加**摆字，而底色/光标/选区按 `col × cell_w` 画，合并 N 个 CJK 后第 k 个字漂 `k × (a − 2·cell_w)`——每字差 0.2px、一行 60 字就漂满一格，正是用户当年实报的「粘贴的内容和光标之间有空白」被 O(N) 放大后请回来。「advance 彼此一致」≠「advance 等于 `width × cell_w`」。**CJK 多半会被新判据拒绝，这是正确行为不是遗憾**（原分析承诺的「中文行 Buffer 降一到两个数量级」多半拿不到）。**谓词必须注入**（`row_to_runs(cells, hidden, &mut impl FnMut)`）：改成 TextLayer 的方法会让现有 12 处纯数据单测全部被迫挂上 `FontSystem`。硬护栏 `width >= 1`（组合附加符号 `unicode-width` 判 0，会满足 `advance == 0 × cell_w` 白送进合并）。**memo 按 `char` 单键的前提要登记**：整形路径的 `Attrs` 目前一律只有 `.family()`、无 `.weight()/.style()`（bold 只走调色板加亮，F128），哪天加了真粗体这个 memo 就静默失效——配源码切片守护，要加就得先把键扩成 `(char, weight, style)`。**收益实测是 7.2× 不是一到两个数量级**：一行 200 格 ASCII 从 `200×1770 + 200×269`（407.8KB）降到 `2×1770 + 200×269`（57.3KB，`MAX_MERGED_CELLS=128` ⇒ 2 个 run）——省掉的只有 1770 的固定头，269 的字形边际价省不掉。**新判据带一个真实的性能悬崖**：它量的是**当前字体链**下的advance，开发机没装 Google Sans Code 时回退到比例字体（`cell_w=13.8047` 而 `i=4.4453`、`x=9.4688`），**连 ASCII 都不再合并**，退化成每格一个 Buffer、比改之前还差。判据没写错（那字体确实排不进格子，强行合并就是错位），但这意味着**字体族名写错／字体没装 = 静默的内存与整形量暴涨**。诊断指纹写进 `advance_fits_its_cells` 文档：实机若见 `text:` 与 `reshape=miss:` 同时异常高，先查字体族名 |
| F196 | **整形 Buffer 回收池加 cap**：`TextLayer::pool` 无上限、从不收缩，只进不出地卡在历史峰值——按 v0.1.81 日志反推里面躺着约 **6500 个**空闲 `Buffer`，每个还攥着上一次整形留下的 `Vec` 容量。cap = `2 × cache 内 Buffer 数`（下界给常数），在 `ShapedCache::end_frame` 回收完之后 `truncate` | P2 | **cap 的单位是 Buffer 数不是行数**——用的就是 F192 新加的 `payload_count()`，**一处实现两处用**，免得日后改了一个忘另一个（原分析写「2 × cache 行数」，正是 F192 要修的那个单位错配）。2 倍余量是为了不 thrash（稳态下每帧回池数 ≈ 每帧取用数）。丢弃安全：`glyphon::Buffer` 就是 `cosmic_text::Buffer`，不持有 GPU 资源。**同轮否掉「`bands` 改 N 帧未见才回收」**：①量级不成立（`BAND_ROWS=16`，3 pane × 50 行 ⇒ 全窗口约 12 个 `TextRenderer`）；②`slots.retain(last_seen == frame)` **不是回收策略而是安全不变量**——`trim` 清空 `glyphs_in_use`，只有本帧真 prepare 过的带才会把字形标回去，休眠带跨 `trim` 复活后 `fp` 比对相同、跳过 prepare、顶点指着旧图集坐标 ⇒ **屏幕上画出别的字，不报错不 panic**；③方向相反（内存切片却多留 300 帧顶点缓冲）。这条「不做」配的是**一条会变红的源码切片断言**而不是一句注释——它被正经提议过，注释挡不住下一个人。**落地值**：`POOL_CAP_FLOOR=256`、`pool_cap_for(n)=max(2n,256)`，`truncate` 落在 `retain` **之后**（放前面截的是上一帧的旧池，本帧刚回收的一批照样无上限——这个变异已配守护）。池里条目彼此可替换（取出来都要重新整形），丢哪一头都一样，所以用 O(1) 的 `truncate` |
| F197 | **接 DECTCEM（`CSI ?25 l/h`）**：远端要求隐藏光标时不画。实报症状是「pane 里随机位置冒出光标，位置不固定」——全屏 TUI（Claude Code / tmux）自绘期间常驻 `?25l`，把真光标停在最后写字的那一格（表格线中间、行尾……），我们照画。`Emulator::cursor_shape()` 判 `TermMode::SHOW_CURSOR`，不满足就吐 `CursorShape::Hidden`；`gpu::style_for` 把 `Hidden` 提到 `!focused` **之前**判 | P1 | **`Term::cursor_style()` 帮不上忙**：alacritty 0.26.0 那个函数只吐 DECSCUSR/vi-mode 的形状，压根不看 `SHOW_CURSOR`（它自家 UI 层是在 `renderable_content` 里另判一次的）——所以 `CursorShape::Hidden` 此前注释为「不可达」。**隐藏必须走 `shape` 而不是 `Cursor::visible`**：`visible` 的语义是「光标在可视区里」（F17 回溯），而下游拿它当**组字串画不画**的判据（`text::hidden_span_for_row`、`gpu::quads_for_panes`）——并进 `visible` 的话，在常驻 `?25l` 自绘光标的 Claude Code 里打中文，拼音串整个不上屏，而这正是本项目的主场景。**`Hidden` 与「非焦点恒空心框」（F125）的优先级顺序是判据本身**：`!focused` 若先 return，非焦点 pane 会在远端明说「别画」时画出空心框，症状**只在非焦点 pane 上出现**（焦点那支会落到 `match` 的 `S::Hidden`），分屏时格外明显——用户实报的那张图正是这一支。既有测试 `unfocused_pane_is_always_hollow_regardless_of_remote_shape` 当初把 `Hidden` 也列进「恒空心框」的形状表（因为它当时不可达），本条把它移出并另配一条 `a_hidden_cursor_stays_hidden_even_on_an_unfocused_pane`。隐藏**不抹形状**：`?25h` 回来要还原到 DECSCUSR 选的那个，不是默认 Beam。`cursor()` 与 `snapshot().cursor` 走同一个 `cursor_shape()`，两处各判一份的话 IME 定位那条路径看到的是另一套状态 |
| F198 | **接 SGR 7（反显）**：`Emulator::snapshot` 遇 `Flags::INVERSE` 就把解析后的前景/背景对调。实报症状是 F197 发版后「Claude Code 的输入框里闪烁的光标不见了，不知道下一个字打在哪」——**Claude Code 的输入光标本来就不是真光标**，是 `❯ ` 后面那一格 `\e[7m \e[27m` 反显块（Ink / readline 系 TUI 的标准做法：`?25l` 隐掉真光标 + 自绘一格反显）。这一格从 MVP 起就被画成普通空格，只是 F197 之前恰好被「我们错画的那个真光标」盖住了——实录字节流里 Claude Code 把真光标也停在反显块那一格（`cursor col=4`、`inverse cells [25,4]`），所以那个错画的光标看着是对的 | P1 | **两条 bug 叠在一起互相掩盖，单独看任何一条都会得出错误结论**：F197 修好「不该画的别画」，缺的那条「该画的没画」当场暴露成「一个光标都没有」。**对调发生在颜色解析之后**（与 alacritty `RenderableCell::new` 同序）——bold 提亮说的是「程序选的那个前景」，先提亮再整体对调；顺序反了会让提亮落在原背景色上，TUI 高亮条画出来的底色和真终端不是一个。**只接 SGR 7**：DECSCNM（`?5h` 整屏反显）是 `TermMode` 上的另一件事，真机未见需求，注释里写明未接免得下个人以为接了。下游无需改：`gpu::quads_for` 的 `cell.bg == defaults.bg` 短路对反显格不成立（对调后底色是默认前景色）、`text::row_to_spans` 画字用的就是 `cell.fg`（已对调成背景色）。**落地本项目第一个 VT fixture**（`claude-code-input-cursor.bin/.snap`，录自 `tmux new-session -x 100 -y 30 claude` 的 pipe-pane）：`?25h` 在整段流里只出现一次（启动时）、随后 `?25l` 再没撤过，这是 F197 判据的真机佐证；快照头两行是「光标状态 + 反显格坐标」而不是文本网格，失配时先看到的应该是语义位。**录制流必须等长脱敏**（`chenjp`→`tester` 这样按字节数一比一换）——流里全是 `CSI row;colH` 绝对定位，改长度就和内容错位，渲出来的不再是真机那一帧 |

| F199 | **点文件面板即夺键盘焦点**：面板/侧栏里任何一次鼠标按下（点行、点空白、点栏头、右键）都把 `App::focus` 切到 `Focus::FilesPanel`。面板把这件事以 `UiActions::files_focus_click` 报给 `app.rs`，活动栏（远端/本地）仍由面板自己就地改 | P1 | **这是 F200/F201/F202 与既有 F5/F2/Del 的共同前提**：那几个键的代码从 D1 起就在 `handle_panel_key` 里，但键只在 `focus == FilesPanel` 时才路由过去，而在这之前**只有 F6 改得动 `focus`** —— 用户开着侧栏、点了个远端文件、按 F5，那个 F5 一路发给了远端的 Claude Code（v0.1.84 实报）。**加字段要同步 `has_real_action`**：漏了的话这次切焦点会在 egui 的 discard 趟被静默吃掉，表现为「有时候点了没用」 |
| F200 | **就地改名（F2）**：选中行按 F2 或右键「重命名」，**不弹框**，那一行的文件名列原地变成输入框；回车提交、Esc/点别处放弃。名字沿用 `files_dialog::validate_name`（挡 `/`、`.`、`..`、空），非法时红框 + hover 说明、既不提交也不丢弃已输入的内容。预选中**主干名、保留扩展名**。远端栏专有（设计 D5） | P1 | **编辑态存的是原名不是行下标**：点一次列头 `entries` 就重排，下标会让输入框跳到另一个文件上（与 `PaneState::selected` 存身份同理）。**生命周期三处清**：`begin_load`／`invalidate`／`accept` 后那一行消失了 —— 否则一个陈旧的编辑框会打到别的文件头上。**提交发的是两条绝对路径**，在面板里用同一个 `cwd` 拼好：从开始编辑到敲回车中间用户完全可能换目录，app 侧再拿「当前 cwd」拼就是改另一个目录里的同名文件。**焦点用一次性 `focus_pending` 而不是每帧 `request_focus()`**：F131 实测过后者会让两栏互抢、先进编辑态那栏永远 `lost_focus()` 不了。**输入框高度必须正好 `ROW_H`**：`Ui::put` 按子 ui 的 `min_rect` 推进布局光标（`centered_and_justified` 让 `TextEdit` 填满给它的矩形），矮 1pt 下面每行就整体上移 1pt，而 `show_rows` 的虚拟滚动只管起始偏移、不检查行距，**编译/测试/日志全不吭声**；早先靠 `advance_cursor_after_rect` 补偿的写法过不了变异验证（漂移量小于测试分辨率），改成由几何构造保证。**它必须登记成 `Modal`（`Modal::FilesRename`，五处一处不落）**，否则 T8：一个键都收不到、退格还被 `handle_panel_key` 抢走 |
| F201 | **路径编辑框默认全选**：点路径条进编辑态时整条路径选中，敲第一个字直接换掉 | P2 | 走 `egui::TextEdit` 的 `CCursorRange` 状态，**只在进编辑态那一次设**——每帧调的话用户自己拖出来的选区会被反复覆盖。与 F200 的预选共用 `select_all` 一个出口 |
| F202 | **`Shift+Delete` 免确认删除**：裸 `Delete` 仍弹确认框，`Shift+Delete` 直接发，文件和目录一视同仁，并报一句带计数的吐司 | P1 | **设计 D17（远端删除不可逆、必须确认）唯一的明示例外**，用户拿它清一批临时文件——一条条确认反而会养成「闭眼点确定」的习惯，那比没有框更危险；代价是手滑按到 Shift 就真的没了，所以那条路必须有吐司。**吐司措辞是「正在删除 N 个文件、M 个目录」而不是「已删除」**：字节这会儿才刚发出去，成败要等 `UserEvent::SftpOpDone`（成了另有「已完成」、败了是错误卡片），在发出那一刻宣布「已删除」，链路一断就是假消息。**删除清单与确认框共用 `PaneState::delete_targets()` 一个出口**、文案共用 `counted()` 一个出口，两条路各写一遍必然漂移（同 `cancel_op` 的理由）。守护 `shift_delete_skips_the_confirmation_but_a_bare_delete_still_asks` 必须**同时**钉住两条腿 |
| F203 | **弹窗底色 + 标题栏 ✕**：新增 token `modal_bg` = `rgb(63,63,63)`（接 `Visuals::window_fill`），六个 SFTP 弹窗统一走一个 `modal()` 出口，标题栏带 `egui::Window::open()` 的 ✕，**✕ 与「取消」是同一件事**（共用 `cancel_op`） | P1 | 用户实报「确认框弹在 pane 的黑背景上找不到」：原先弹窗蹭 `bar_status`（#181b26），对 `term_bg`（#14161f）只抬 7/255。**不复用 `bar_status` 并把它调亮**——状态栏与会话管理器内框也用它，那两处贴在 `panel_bg` 上本来就分得清，跟着变灰纯属误伤。**底色抬亮的连带账是对比度往下走**：`fg_dim` 在 `#3f3f3f` 上掉到约 4.1:1、不过 AA，弹窗里一律提一档到 `fg_muted`（约 5.0:1），并配一条「哪天把底色调暗回去就红」的反向断言。**✕ 走 `Window::open()` 而不是自己在正文里画**：egui 画在标题栏右上角、位置与系统窗口一致，自己画会掉进标题栏内边距里。判据是亮度差而不是具体色值——后者只是把常量抄两遍 |
| F204 | **编辑器窗口件**：`Ctrl+S` = 「保存到远端」那颗按钮（**包括它按不动的时候**，此时说出为什么）；开窗摆屏幕正中；标题栏右上角自绘 □（最大化/还原）与 ✕（关闭），底部那一行不再重复关闭按钮 | P1 | **居中只在开窗头两帧生效**（`centre_frames`，每帧减一），归零后位置归用户拖——`egui::Window` 的尺寸要先量一帧才知道，第一帧只能用估值。摆哪儿的唯一出口是纯函数 `centred_pos`，用具体数字锁死。**□ 和 ✕ 一律自绘**（T9：直接写进 UI 字符串会在 Windows 的两级字体链外画成豆腐块，且编译/测试/日志全静默）；它们不画文字，测试只能靠 accesskit 的 `widget_info` 定位，故 `icon_button` 补了这一项 |
| F205 | **一次拨号一张票**：`spawn_connect` 发起时把这次连接的全部随行数据（`session_id` / `cfg` / 自动化计划与模板 / tmux 会话名 / 跳过标志）装进一张票存进 `App::dials`（`shell::dial_ledger`），票号随 `UserEvent::ConnectOk`/`ConnectErr` 原样带回，**认领即消费**。`ui.connect_request_last`、`pending_cfg`、`pending_automation` 三个单槽整条下线 | P0 | 用户实报「SFTP 远端书签升级后消失」，**反复修反复复发三轮**。用户导出的 `sessions.toml` 证明数据在盘上完好——错的不是写盘，是**读回时的身份归属**：`ConnectOk` 从 tokio task 发回来，事件本身不带 `SessionId`，「这次连上的是哪条会话」记在一个单槽里，而 `spawn_connect` 从来没有「在途只许一条」的闸，本项目主场景恰恰是**高延迟代理链路**（一次连接好几秒）。第二条拨号发起时四个单槽被整体盖掉，先连上的那条拿到别人的身份：标签 A 的面板填的是会话 B 的书签（A 的看起来「消失了」）、在 A 上点 ☆ 按 `tab.session_id` 落盘**写进了 B 的记录**、A 上分屏按 B 的 cfg 开 pty、**给 A 配的登录后命令在 B 的终端里跑**。全程零报错。前两轮修复（F187/F189）都修在 store 那一层，而错的是身份归属这一层——与 F188/T11 同族：**判据放错了层**。已经被写歪的历史书签不自动纠偏（分不清哪条是用户本意），发版说明里点名让用户核对 |
| F206 | **焦点描边单一来源**:`theme::focus_ring()` + `FOCUS_RING_W`(1.0）+ `FOCUS_RING_ROUNDING`（直角），pane 边框 / 文件面板 / 内置编辑器窗口三处共用 | P2 | 用户实报「编辑器的焦点边框要跟 pane 一样细、要直角」。走查发现同一个语义（「键盘现在归我」）在屏幕上已经漂成三套：pane 1.0 直角、文件面板 2.0 圆角 4、egui 窗口默认白 6% 圆角 6。**文件面板那两处「拖放落点」描边故意不改**——语义不同（松手会落这儿），且能与焦点框同时出现，长得一样用户就分不出来。**编辑器不做条件判断**：它是 `Modal::Editor`，永远持有键盘，`if focused` 恒真只会让人以为它会灭 |
| F207 | **编辑器正文底色 = `term_bg`**：窗口壳仍是 `modal_bg`（#3f3f3f），正文区走 `TextEdit::background_color(term_bg)` | P2 | 用户看的是远端文件，底色跟终端一致才连得上「这就是那台机器上的东西」；两层色差本身就是「哪块能打字」的边界。**不改 `Visuals::extreme_bg_color`**：那是全局量，一改会话表单/路径条/改名框的输入框全跟着变，而那些贴在 `panel_bg` 上本来就配好了 |
| F208 | **编辑器固定尺寸 + 屏幕夹紧**：默认 1100×760（原 720×480），`max_size` 夹到主窗口客户区的 85%，开窗那两帧连尺寸一起钉；拖拽/最大化照旧 | P1 | 用户实报「编辑器底部被 Windows 11 任务栏遮挡」。**根因是 egui 的正反馈棘轮**：`Resize` 每帧 `desired_size = desired_size.max(last_content_size)`（0.30 `containers/resize.rs:258`），而本窗口的正文高度又是从窗口可用高度反推的——两者互为因果，一帧涨一点。**`default_size` 治不了**：它只在这个窗口 id 第一次出现那一帧生效，之后 `Resize` 从 `Memory` 读老尺寸；所以「每次打开都回到默认」得靠开窗那两帧 `fixed_size` 把老值冲掉。**`max_size` 管的是内容区而挡住任务栏的是外框**，差的那一截（标题栏 + 四圈边距，默认样式约 50 点、随字号/DPI 变）按上一帧实测的 `chrome` 扣除——不照抄 egui 私有的 `title_bar_height + margins` 算法，那玩意版本一变就静默算错 |
| F209 | **终端里 `Ctrl+V` 直传截图**：剪贴板里是位图时编码成 PNG，经一条**新开的** sftp channel 传到远端目录（默认 `/tmp`，会话「SFTP」分节可改），成功后把**绝对路径 + 一个空格**打进那块 pane 的输入行，不带回车。剪贴板里有文本则照旧走 F18 文本粘贴（多行仍弹确认）；两样都没有时不吞按键，照旧编码成 `^V` | P2 | 用户诉求不是「看图/编辑图」，是**在 SSH 上跟 Claude Code 说话时能方便地引用一张截屏**。**不开 `arboard/image-data`**：它在 Windows 上会拉进 `image` 0.25，与「刻意不用 `image` crate」（N6 exe 体积）冲突——`CF_DIB` 自己解不到两百行，编码用的 `png` 已因 `ico` 躺在依赖树里。**alpha 一律丢掉**：截图工具给的 32 位 DIB 里 alpha 常常整片是 0，照单全收会得到一张「编码成功、传输成功、打开全透明」的图，全程零报错。**文本优先**：复制图片时很多程序会同时放一份文本，图优先会把一次普通粘贴变成莫名其妙的上传；Win+Shift+S 只放位图，不受影响。结果按 `generation` + `PaneId` 路由回**发起的那块 pane**，不取「此刻的焦点」（T11 同族：高延迟链路上传几秒，用户早切走了）；失败只弹提示，**绝不把半截路径写进输入行**。20 MiB 上限——这条路没有进度条，传大文件用户只会看到程序「卡住了」。代价（已认下）：裸 `Ctrl+V` 不再原样发 `^V`，readline 的 quoted-insert 改用 `Ctrl+Shift+V` 之外的手段
| F210 | **组字锚点**：一次组字开始那一刻的 `(PaneId, col, row)` 记进 `ImeState`，整段组字期间内联拼音（F126）与系统候选框（`set_ime_cursor_area`）都钉在它上面，**不再每帧跟着终端真光标走**。夹紧到当前网格（reflow 会让锚点越界）；`PaneId` 不匹配就退回真光标；三条结束边（commit / 空 preedit / disabled）各撤锚点 | P1 | 用户实报「tmux + Claude Code 里跑 `/compact` 时打中文，画面闪烁」。**根因不是渲染抖动，是锚点跟错了东西**：远端 TUI 重绘时会把光标挪到正在重画的那一段、画完再挪回输入行，这些中间态本该被 DEC 2026 同步块（T2/F11）挡住——但**经 tmux 转发时同步块被 tmux 吃掉**：tmux 只在外层终端登记了 `sync` 特性时才往外发 BSU/ESU，而我们报的 TERM 是 `xterm-256color`（tmux 内置特性表里没有 sync）。于是 T2 的攒帧在本项目**主场景下根本不生效**，我们实打实地画出重绘中间帧。`/compact` 持续刷进度条 ⇒ 拼音串和候选框每次重绘瞬移一次再弹回来，连续闪烁；候选框还额外滞后一帧（只在 present 过的那一轮才调那次跨进程 API），一次瞬移闪两下。钉住是安全的：**组字期间用户敲的字母一个都没发给远端**，远端光标没有任何理由因为用户而移动。**改在快照的 `cursor` 上而不是逐个消费点**：内联拼音位置、正文让路区间、光标 quad、整帧指纹全只读 `snap.cursor`，一处改完下游天然同源；各判一份迟早漏一个，漏掉的那个就是「拼音钉住、候选框还在跳」的分家。已认下的取舍：裸 shell 下背景输出把整屏顶上去时锚点停在原地、拼音画错位置（一次提交即自愈），相对「每次重绘必闪」划算。**欠账已由 F211 收口**：让 tmux 真的把同步块转发过来是 T2 在主场景生效的前提。收口后本条仍必须留着——攒帧只挡住「远端在同步块里重绘」那一类，内层不发同步块或攒帧超时兜底放行时中间态照样上屏
| F211 | **让 tmux 真的转发 DEC 2026 同步块**：`tmux attach` 之前先在远端登记 `terminal-features[99] = xterm-256color:sync`，作为**独立的一条 shell 语句**、自吞 stderr；新建会话改成 `new-session -d` 建好、登记、再单独 attach | P1 | F210 挖出来的欠账：tmux 只在外层终端登记了 `sync` 特性时才把内层程序的 BSU/ESU 往外发，否则**整个吞掉**。实测确认 —— 内层发 20 个同步块、外层收到 0 个；登记之后 20 个原样到达。⇒ **T2/F11 的攒帧在本项目主场景（tmux）下从来没生效过**，「不闪」这条项目存在理由一直是半残的。Claude Code 自己就在发同步块（静置 6 秒抓到 9 个），所以链路一通，每一轮远端重绘就原子到达。三条实测出来的硬约束，少一条都静默失效或反过来搞坏连接：**① 必须在 attach 之前** —— 运行期改 `terminal-features` 对已 attach 的 client 不生效（BSU 恒 0），tmux 只在 client 建立时匹配一次特性表；这也是它不能挂在 F124 `remote_bootstrap`（attach 之后才跑）上的原因。**② 必须是独立的 shell 语句** —— 写成 `tmux set … \; attach` 串进同一次 tmux 调用的话，老 tmux（< 3.2 无此选项）上 set 失败会**中止后面全部命令**，连会话都建不起来；拆开 + 吞 stderr，老 tmux 上只是白跑一句。**③ 必须写死数组下标** —— tmux 对 `set -a` 不去重（设 4 次留 4 条），每次重连都会让特性表长一条。新建路径因此从前台 `new-session` 改为 `-d` + 单独 attach：前台 new-session 把「起 server」和「client attach」并成一步，登记没有插入点，**首连（最常走的那条）永远拿不到同步块**。代价（已认下）：新会话先以 tmux 默认尺寸建立、attach 时再 resize，TUI 多收一次 SIGWINCH。**不走冒充终端名那条路**（应答 XTVERSION 报 iTerm2/kitty 之类）：那会连带启用一堆我们没实现的能力。TERM 字面量在 store 与 ssh 两个 crate 里各一份（依赖方向不许互引），靠 `mullion-app` 的跨 crate 断言锁住 —— 对不上是**静默不生效** |
| F212 | **按住左键期间远端擦行不再抹掉选区**：`Emulator` 新增 `hold_selection(bool)`，开着时 `feed` 在 `parser.advance` 前后给 `term.selection` 留底，**只在被丢成 `None` 时**补回；app 侧按下即进入该态，`selection_release` 与失焦兜底两条出口各自归还，`selection_clear` 也撤（丢 `Released` 事件时的唯一解） | P1 | 用户实报「`/compact` 的过程中，pane 里按着左键选不了文字，高亮出现又被冲掉」。根因在上游：alacritty 的 `clear_line`/`clear_screen`（EL/ED）判据是 `take().filter(|s| !s.intersects_range(擦掉那几格))` —— **沾边就整段丢**，连选区里没被碰过的行一起。全屏 TUI 每轮重绘都要擦几行（`/compact` 的转圈提示行每秒擦好几次），于是拖拽中的选区被反复清空；而 `selection_update` 在 `None` 上是**静默 no-op**，拖到天涯海角也回不来，用户只能松手重按 —— 等于整个划选功能在重绘期间不可用。判据是「划选是纯本地意图，远端输出无权取消它」。**只补 `None` 这一种**：滚动路径上 alacritty 走的是 `selection.rotate(..)`、结果仍是 `Some`，那是正确的跟随行为，无条件盖回旧坐标会让选区钉死在屏幕位置上、滚一行就选中别的文本（F18 头号坑的另一种走法）。已认下的取舍：同一次 `feed` 里既滚屏又擦行时补回来的是没跟着滚的旧坐标、错位一行 —— 这类 TUI 用绝对定位重绘、不滚屏，而漏掉补偿的代价大得多。hold 必须每条出口都归还：挂住之后这个 pane 的选区**再也擦不掉**，用户看到一段永远赖着不走的高亮且无任何报错。守护：`vt_fixtures` 的 `claude-code-compact-repaint`（真机 `/compact` 字节流）两条 + `emulator::tests` 两条 + `app::tests::every_path_that_ends_the_drag_also_hands_the_selection_hold_back` |
| F213 | **弹窗/toast 配色收口**：toast 底色从 `sunken_bg` 改为与弹窗同源的 `modal_bg`，边框按语义分 `Busy`/`Ok`/`Warn` 三档（`set_toast` 的档位是**必填参数**，由编译器当闸门）；弹窗正文灰阶收成 `fg`/`fg_muted` 两档；新增 `danger_text` 承载危险语义的**文字**（`danger` 只留给填充/描边）；F203 的列举式闸门换成从源码现算弹窗清单的穷尽式守护 | P2 | 用户实报「弹窗以及 toast 的配色对比度不够、层次乱」。实算下来是三笔各自独立的欠账：**① toast 从来没被 F203 捞出来** —— `sunken_bg`(#0e1018) 对终端底 (#14161f) 只有 **1.05:1**，一块飘在正文之上却几乎看不出边界的浮层；边框还无条件画 `ok` 绿，「正在上传…」（没落地）和「隧道已停止：连接被拒」（降级了）都镶一圈成功色，颜色在说谎。**② F203 的闸门是列举式的**（写死 `files_dialog.rs` / `editor_window.rs` 两个文件名），另外五个弹窗文件共 13 处 `fg_dim`/`fg_dimmer` 一直漏在外面——「列举式门控在加档时必然漏」本仓库第四次踩中，这次把清单改成扫 `src/ui/**` 现算：凡是开了 `egui::Window::new`/`egui::Modal::new` 且没自己 `.frame(..)` 覆盖底色的文件自动入闸，新加弹窗不用回来改数组；同文件里画在别的底色上的行（`edit_panel` 的底部列表在 `panel_bg`、会话管理器主窗自定 `bar_status`）走**行级**逃生门，要写理由。**③ `danger` 是 Windows 系统红 #e81123，亮度太低**：在 `modal_bg` 上 2.27:1、在 `bar_status` 上 3.71:1，两处都读不到 4.5——而它承载的恰是全 app 后果最重的几句（「删除 3 个目录（连同其中全部内容）」「有未保存的修改，关掉就没了」）。不动 `danger` 本身（它还要当填充/描边，判据 3:1；把纯红提到 4.5 只能提到接近粉，填充语义就散了），另立 `danger_text` #ff9090（modal 4.84 / panel 8.28）。`host_key.rs` 里那个写死的 `#c82828`（**1.9:1**，「⚠ 主机密钥已变更」）一并并回色板 |
| F214 | **编辑打开减往返 + 分段埋点**：`SftpClient::read_all` 的读缓冲 64 KiB → 256 KiB（对齐 russh-sftp 的单包上限），并接受一个「列目录时已经知道的大小」提示，命中就不再多发一次只为问 EOF 的空往返；返回值带上 `ReadTiming`（open/read/stat 各段耗时 + **READ 次数**），`start_edit` 打一行 `编辑打开:open=…ms read=…ms×N stat=…ms total=…ms` | P2 | 用户实报「在 Mullion 里编辑，几 KB~几十 KB 的文件也要等好几秒」。**这条链路的成本单位是往返数，不是字节数**：russh-sftp 的 READ 在客户端是**串行**的（只有 WRITE 有 `max_concurrent_writes`），一个 1 MiB 的文件按 64 KiB 缓冲要走 16 次 RTT——本项目主场景是高延迟代理链路，16×RTT 就是那「好几秒」。缓冲提到 256 KiB 后同一个文件降到 5 次（服务端还会按自己通告的 `read_len` 再切，255 KiB 是 OpenSSH 的值）。大小提示省的是最后那次「读到 0 字节才知道到头了」的空往返，判据必须是 `==` 不能是 `>=`：列目录的大小是**旧的**，文件在这期间被写长了而我们拿 `>=` 收尾，就会静默截断一半内容再回写覆盖——**宁可多一次往返也不能截**。`stat` 仍排在读**之后**：F53 的冲突检测要求那个时间戳描述的是我们**手上这一份**，提前取会在并发写下配出「旧戳 + 撕裂内容」并被冲突检查放行。埋点是因为「慢在哪」在无头容器里**根本量不到**（本机 RTT 为零，16 次和 1 次一样快），只能让程序自陈往返数交人工从实机日志读回来 |
| F215 | **内置编辑器语法高亮**:syntect(纯 Rust 的 `fancy-regex` 后端)按行增量上色，主题拿 `theme.rs` 的色板现拼六档（注释/字符串/数值/关键字/函数名/类型），扩展名与整文件名映射到「形状够近」的语法，超过 256 KiB 只排版不上色；窗口底部报出认到的语法名 | P2 | 与 F214 同一条实报的另一半。三条约束决定了实现形状：**① egui 的 `layouter` 每帧都跑**（`TextEdit` 只缓存 galley，不缓存拼 galley 的过程），照直写就是「三千行 × 60fps = 每秒几千次全量重算」，正是 T3/N3 那条红线；所以挡两层——文本没变直接还上一帧的 `Arc<Galley>`，文本变了只重算受影响的行。**② 增量的判据必须是「进入这一行时的解析状态」，不是「这一行变没变」**：在第 1 行补一个 `/*`，底下几百行内容一个字都没动却全该变成注释色——只比行内容的实现会**静默**留住旧颜色，编译/测试/日志全不报，只有人眼看得见。**③ 行的身份是「内容 + 位置」，而回车把整片位置挪掉**：纯按下标比对的话，在文件开头按一次回车 = 全文重算，而这恰恰是编辑时最常做的动作；先求公共前缀/后缀、后缀按位移查旧表，就退回「只重算改动附近」。**特性砍到只剩解析**：默认的 `default-onig` 会拉进 `onig_sys`（C 库，交叉编译要 mingw 那边也有一份），换 `regex-fancy`；`default-themes`/`plist-load`/`yaml-load`/`html` 一律不开（N6 exe 体积）。**主题不另配一套颜色**：编辑器里的绿必须就是文件面板里的那个绿，各配一份没人会记得同步改；六档在 `term_bg` 上全过 4.5:1（注释那档最暗，6.53:1——它该退到后面，但退到读不清就是另一个 bug）。**认不出来的一律 Plain Text**，不随便挑一个画满屏假颜色；而 ini/conf/dockerfile/ts 这些默认包里没有的，映射到形状最接近的那一个（`key = value` 一族归 Java Properties，`#!` 一族归 bash）。语法名报在窗口底部：猜错时用户看到的只是「颜色不太对」，这一行让他分得清是猜错了语法还是我们画错了色 |

**§4.4 的三条纪律**（违反即为设计错误，各配守护测试）：

1. **远端路径的真源是 `Vec<u8>`。** SFTP v3 的文件名是字节串、无编码约定，Linux 上可以是任意
   非 UTF-8 字节。客户端内部一律以字节为真源，UI 显示串只是 `from_utf8_lossy` 的**投影**。
   **协议层限制（2026-08-12 核实）**：`russh-sftp 2.4.0` 的 wire 字符串一律走
   `from_utf8_lossy`，**收发两个方向都过**，字节通道只对文件**内容**开放。故远端非
   UTF-8 名到手时已是合法 UTF-8、只带 `U+FFFD`，判据必须是「发不发得出去」
   （`RemotePath::is_operable()`）而非「是不是合法 UTF-8」；**含 `U+FFFD` 的路径与
   非 UTF-8 字节一样不发请求**，对应条目在列表里照常显示、所有操作禁用并说明原因
   ——绝不静默打到别的文件上。
   一旦某处拿显示串反推路径，非 ASCII 文件名就会静默失败——在中文目录下必然发生。
   下载到 Windows 遇非法文件名（`\ / : * ? " < > |`、空格/点结尾、`CON`/`NUL`/`COM1`~`LPT9`）
   **打断并给建议名**，不静默改写（静默改写会让 F53 的回传对不上原文件）。
2. **删除符号链接绝不跟随。** 列举用 `readdir`（不解引用），显示目标靠 `readlink`；删除删的是
   链接本身。搞错就是把远端整个目标目录删了。
3. **内置编辑器边界**：>1MB 拒绝（egui `TextEdit` 全量重排，撞 T3）；读到 NUL 即判二进制拒绝；
   **非 UTF-8 只读打开并标注**（猜错编码保存回去 = 静默毁文件）；换行符原样保留；写回**直接
   覆盖**不做 rename 替换（rename 换 inode，丢属主/权限/ACL/硬链接——对 `/etc/*.conf` 是实打实
   的破坏）。同理传输用 `.mullion-part` 临时名 + 完成后 rename，但**覆盖已存在文件时退化为
   直接写**。

### 4.5 凭据

| ID | 需求 | 优先级 | 验收标准 |
|---|---|---|---|
| F70 | 主机配置本地存储，密码/私钥口令加密（Argon2id + ChaCha20-Poly1305） | P0 | 单测：磁盘上的字节里搜不到明文口令。**现状（v0.1.20）只兑现了后半**：`crypto.rs` 是 XChaCha20-Poly1305，但主密钥由 `OsRng` 随机生成后直接存 OS 钥匙串，**没有任何口令派生步骤**，argon2 依赖都没引。验收标准本身满足，所以一直没暴露；**前半已由 F71 于 v0.1.40 补上**（设了主密码即走 Argon2id；不设时仍是钥匙串方案） |
| F71 | 主密码：设定后主密钥改由主密码经 **Argon2id 派生**（而非随机生成 + 钥匙串），使同一份 `secrets.enc` 在任意机器上都能解开。默认仍不设（此时维持现有钥匙串方案，行为不变）。**盐随密文存放**（`secrets.enc` 文件头）——随机器存等于白做 | P1 | 已实现（v0.1.40：`kdf.rs` Argon2id 派生 + `secrets_file.rs` 文件头带盐与参数 + `Vault::open_with` / `probe_scheme`；启动解锁框与设置弹窗「安全」分节）。派生纯函数单测、未设主密码时与今日行为逐字节等价的回归测试均已覆盖。**是 F48 的硬前置** |
| F72 | **不上报任何遥测** | P0 | 代码审查 + 无出网依赖 |
| F73 | 编辑会话时凭据三态：未触碰保持原值（UI 显示 6 位黑点）/ 触碰后留空清除 / 输入新值覆盖。三个密码字段各自独立 | P1 | `merge_secret` 纯函数单测；红队注入「Keep 走 None 分支」必须变红 |
| F74 | **凭据实体**：一份凭据（用户名 + 认证方式 + 私钥路径 + 口令/passphrase）可被多条会话引用，换密钥改一处。`Auth` 改为**严格二选一** `Inline \| Ref(CredentialId)`，任何时刻只有一个真值——不做「引用 + 局部覆盖」。引用完整性**对齐跳板而非分组**：被引用的凭据**不可删**（UI 列出引用者，要求先解绑），悬空引用**连接时报错、绝不回落到别的身份** | P1 | ①动手前先做 toml round-trip 探测——`AuthKind` 是内部标签枚举且被 `flatten` 进 `[session.auth]`，路线图 §4.3「风险 1」点名过 toml 对嵌套内部标签枚举的已知限制；撞上就退到扁平编码（`source = "inline"\|"ref"`），语义不变。②schema v5→v6 **机械映射迁移**单测（v4 被先落地的 F40~F44 拿走、v5 被私钥正文入库拿走了，规则是「谁先落地谁拿号」）：每条 v3 会话逐字段等价映射成 `Inline`，凭据表初始为空，`.bak` 存在。③删除被引用凭据必须报错的单测。④悬空引用必须报错、不降级的单测（比照 `jump.rs` 的 `dangling_reference_is_rejected_never_degraded_to_direct`）。⑤`secrets.enc` 的键新增凭据命名空间后，`vault.rs` 那行 `secrets.retain(\|k, _\| live.contains(k))` 的 GC 集合必须同步扩展——守护测试：存入凭据口令后重开 vault，断言口令还在（不同步就是**静默数据丢失**） 。**已实现（v0.1.41）**：schema **v8→v9**（v5/v6/v7 分别被私钥正文入库、SFTP 分档、隧道拿走，仍按「谁先落地谁拿号」）；密文键空间 `cred:<id>`；`Auth::Inline \| Ref` 严格二选一；会话编辑器「身份」节多一档「凭据来源」，会话管理器新增「凭据」档（列表 + 表单 + 删除）；被引用时删除按钮当场置灰并列出引用者会话名，store 侧 `CredentialInUse` 仍是最后防线；悬空引用连接时硬失败（`DanglingCredential`）。**代理口令归会话不归凭据**——它回答「连到哪」，凭据只回答「以谁的身份」 |
| F75 | 显式去重提取：扫描出重复的 `(用户名, 私钥路径)` 组合，提示「提取为共享凭据」。**只在用户点头后执行；格式迁移中不得静默合并** | P2 | 重复检测为纯函数单测；迁移单测须断言迁移后凭据表为空（即未擅自合并）。**仍未做**（v0.1.41 只落了 F74 的实体与手工引用；v8→v9 迁移未合并任何东西，凭据表初始为空，已有守护测试） |

**分组不持有默认凭据**（F74 的边界，与路线图 §4.2 一致）：凭据可继承意味着「改一次分组凭据 = 整组会话的登录身份静默切换」，与 F74 「不允许静默改变登录身份」是同一类风险。便利性交给 UI——新建会话时**预填**同组最常用的凭据，预填是一次性动作，写进记录后固定。

**既有欠账（同源，登记待修）**：`vault::delete()` 删除会话时**不检查跳板引用**，删掉一条被别人当跳板用的会话会直接产生悬空引用。悬空跳板引用在解析时会报错（有守护测试），所以不是安全问题，但用户拿到的是「连接时才炸」而非「删除时被拦」。修法与 F74 的删除前置检查同构，宜一并做。

### 4.6 外观与外壳

视觉规格（精确尺寸/色值/控件语义）冻结在
`docs/superpowers/specs/2026-07-29-ui-visual-baseline-design.md`，此处只列需求与验收。

| ID | 需求 | 优先级 | 验收标准 |
|---|---|---|---|
| F80 | 深色视觉 token 统一：egui 外壳与 glyphon 终端共用同一套色板。另含终端网格区**左右各内缩 8 逻辑点**（随 DPI 缩放，纵向不缩），不再顶着分屏边界，缩进量与标题条文字的左边距同值 | P1 | 单测断言 token 表取值；内缩量、坏 `scale_factor` 兜底、以及「内缩与分隔线让位叠加」有几何单测；观感需人工确认 |
| F81 | 状态栏信息架构：左（布局 · 连接态）/ 右（编码） | P2 | 单测：格式化函数在各状态下的输出字符串 |
| F82 | 布局预设按钮组：画在**菜单栏同一行、水平居中**（不占独立工具栏） | P1 | 预设须**套用成一棵 F30 布局树**，不得另立固定枚举模型；**平铺 7 个纯图标按钮**（第一个是 1 屏，无文字，说明走 tooltip）；按钮图标的格子几何必须**由布局树推导**，不得手写第二份几何表；三屏「左/右满高」**左右等宽**，「大」只指高度 |
| F83 | pane 标题条：主机名 + IP + 状态徽标，**可关**（默认开） | P1 | 关闭后该 32px 归还终端网格，`grid_dims` 行数随之变化（T4/F34） |
| F84 | 设置弹窗：终端主题 / 字号滑块 / 快捷键一览（承载 F21） | P1 | 配置项 round-trip 到 store 的单测 |
| ~~F85~~ | ~~自绘标题栏 + Windows 三键~~ | — | **已否决**，见下 |
| F90 | 会话管理器单窗双栏：左栏搜索+分组列表，右栏三 Tab 编辑器，删除确认内联，不再弹独立窗口 | P1 | 单测断言一帧内 `Order::Middle` 层只有一个 Area；搜索匹配与脏检查为纯函数、可无窗口单测 |
| F91 | 会话编辑器必填校验：会话名称 / 主机 / 用户名任一为空时，「保存」「保存并连接」禁用，缺项所在 Tab 打红点，字段标红星，按钮 hover 说明缺什么。端口有默认 22，不算必填。 | P1 | 判定逻辑为纯函数单测（空白字符不算填、缺项映射到首个所在 Tab、hint 文案）；按钮禁用态与 tooltip 文案有单测覆盖；Tab 红点与字段红星的实际绘制效果为人工验收 |
| F92 | 「测试连接」：按当前表单（含未保存改动）走完 代理 → 跳板 → TCP → 握手 → 指纹 → 认证，成功后**立即断开**，不开 channel、不起 pty。20 秒超时。结果以卡片展示。切换会话或关闭窗口即作废在途结果。 | P1 | 世代号防止「切会话/关窗后旧拨测结果误盖新表单」有单测覆盖；表单被编辑后作废在途结论有单测覆盖；`establish()`（含代理/跳板/握手/指纹/认证全链路）由 mullion-ssh 的 live 集成测试覆盖（需真机，`MULLION_LIVE=1`）；结果卡片的视觉展示为人工验收 |
| F93 | 私钥选择辅助：扫描 `~/.ssh` 列出候选（**只看文件名，不读内容**），支持把私钥文件拖入窗口填路径。 | P2 | `~/.ssh` 扫描启发式（仅按文件名、不读内容；跟随符号链接但排除目录链接）有单测覆盖；候选下拉为空时的禁用态有单测覆盖；拖放决策（取第一个 / 拒绝目录 / 多文件提示 / 无路径跳过）抽成纯函数 `decide_key_drop`，有单测覆盖；真实拖放行为与悬停高亮观感需人工验收（依赖真实窗口与文件管理器，无头环境测不了） |
| F94 | 会话管理器横向拖 resize：窗口外框（边框 / 标题栏 / 关闭按钮 / resize 手柄）必须跟着变宽，内容不许画到外框之外 | P1 | 用真实指针事件拖右边缘，断言外框右边缘逐步跟手 + 右栏内容右边缘不超出外框（`dragging_the_resize_handle_widens_the_window_frame_not_just_its_contents`）；实际观感人工验收 |

**F82 修订**（2026-07-30，v0.1.12 实机验收后）：原定一条 48px 独立工具栏，承载
「新建连接 / 分屏操作 / 布局预设 / SFTP 开关 / 设置」。取消那一栏——它为「更好找」
永久吃掉终端一行多，而五项里只有布局预设是高频操作（新建连接已在「会话」菜单，
设置是 F84，SFTP 开关是 F50，各归本编号）。布局按钮组移进菜单栏空着的中段并居中。
「分屏」菜单同时撤掉（按钮组就在同一行，菜单里再放一份是重复入口），其下唯一的真
功能「显示/隐藏 pane 标题条」(F83) 移到「配置」菜单。

**F85 否决理由**（2026-07-29 评估，记录以免重复提案）：winit `with_decorations(false)`
之后，Windows 上必须自接 `WM_NCHITTEST` 才能保住 resize 边框命中区、双击标题栏最大化、
Aero Snap 与 Win11 Snap Layouts（悬停最大化按钮弹出的布局菜单）。用这些系统集成风险
换 32px 的视觉统一，在功能欠账（F30 分屏、F50 SFTP）未清前不划算。若将来重提，需先
给出 `WM_NCHITTEST` 的可测方案。

### 4.7 会话组织与集合操作

| ID | 需求 | 优先级 | 验收标准 |
|---|---|---|---|
| F60 | 单层分组（不做任意深度树）。分组只持有**可继承**字段（标签 / 终端 / 外观 / 网络路径），连接目标与凭据永不进分组；删除分组**不级联删会话**，只把归属置空 | P1 | 已实现（v0.1.14 起：`GroupRecord` + `resolve_override` / `resolve_merge_list` 纯函数）；GUI 的增 / 改名 / 删除三个入口已接线（`GroupIntent`） |
| F61 | 会话图标：内置形状库 + emoji。**不做自定义图片**——要引 image 解码器，顶爆 N6 的体积线 | P2 | 形状名 → 形状的映射为纯函数单测，未知名**降级为不画**（旧配置手改坏 / 未来新增形状名在旧版本上，共用这一条路径）；emoji 呈现为**黑白剪影**（epaint 不支持 COLR/CPAL 彩色字形），观感人工验收；三档密度（Full/Compact/Icons）统一用 32px 帧，行高与阈值有 `every_step_uses_the_32px_frame_and_the_row_fits_it` 钉着 |
| F62 | 会话语义色：8 个预设色板（按**颜色**命名，用途只做 tooltip 建议）+ 自由 hex。落点由 `ColorSpec.apply_to` 决定：会话列表 / pane 标题条 / 状态栏 / 标签页（标签页落点自 F122 起颜色画法改成**整块背景**；原有的活动标签底部横杠**仍保留**，但语义收窄为「标识哪个是活动标签」，不再表达节点色；标签自己的覆盖色优先于会话色） | P2 | 8 个预设 vs 面板底色的对比度实算 ≥ 3:1（WCAG 1.4.11 非文本阈值）单测；hex 解析坏值**降级为无色**不报错；`apply_to` 过滤为纯函数单测。**色板按颜色命名而非按环境命名**，避免与 F64 撞语义；选中/悬停行背景 = 节点色低透明度混色（选中 28% / 悬停 14%），8 个预设铺底后 `fg` 对比度 ≥ 4.5:1 单测；图标底色同源于该闸门（列表用 `ListItem`、pane 标题条用 `PaneTitle`，各有守护测试） |
| F63 | 标签 tags（Merge 继承）+ 搜索 + 收藏 + 排序权重 | P2 | tags 的 Merge 继承已实现（v0.1.14）；**欠账**：标签编辑 UI、收藏与排序权重字段（后者需 schema 升版）。排序为纯函数单测 |
| F64 | 环境等级标记（开发 / 测试 / 预发 / 生产），作为外观解析 fallback 序列中的一层**隐含默认色** | P2 | 需 schema 升版新增字段；`resolve()` 已支持任意层数，插入本层**不改签名**（`PrefsLayer` 序列末尾追加）；隐含色被会话显式色覆盖的单测 |
| F45 | 克隆 / 另存为会话 | P2 | 克隆结果除 `id` 与名称外逐字段等价的单测；凭据引用随之复制，**不复制口令明文** |
| F46 | 会话导入导出与备份：导出全部 / 仅选中为可再导入的文件，**默认不含凭据**（含凭据需显式勾选且走主密码）；自动备份保留 N 份 | P2 | 导出 → 导入 round-trip 单测；不勾选凭据时，导出文件字节里搜不到任何口令（比照 `tests/f70_no_plaintext.rs`） |
| F47 | 快速连接：不保存的一次性会话——输入 `user@host[:port]` 或粘贴连接串直接连；只进「最近」，不进会话库 | P2 | 连接串解析纯函数单测（缺省端口 / 带端口 / IPv6 字面量）；断言快速连接**不产生** `SessionRecord` |
| F48 | 配置跨机可用（含凭据）：设定主密码后，把配置目录整体搬到另一台机器即可直接用。**Mullion 自己不做同步传输**——文件搬运交给用户或外部工具（Syncthing / Git / 网盘），与 N-G4「不做云同步、账号体系」一致。Mullion 的责任是保证目录**可被外部接管**：原子写（已有 `write_atomic`）、路径不含机器绑定信息、外部改动后重读不损坏、冲突时**报错而非静默合并** | P2 | **硬前置 F71**。未设主密码时必须明确拒绝并引导设置，不得搬出一份注定解不开的 `secrets.enc`；跨机解密单测（同一主密码 + 随密文走的盐，在「另一台机器」语境下解开同一 blob） |
| F49 | 使用统计与最近使用：最后连接时间、连接次数，支持按最近使用排序 | P2 | 排序纯函数单测；断言统计字段**不参与继承解析**（它是会话自身的事实，不是可继承偏好） |

### 4.8 运行时状态展示

> 本节是**界面上显示的运行时信息，不进配置文件**。它与 4.6 共用同一块界面
> （会话列表条目、标签页、状态栏），但数据来源是活的连接，不是磁盘上的记录。

| ID | 需求 | 优先级 | 验收标准 |
|---|---|---|---|
| F95 | 连接状态呈现：未连接 / 连接中 / 已连接 / 重连中 / 失败，在会话列表条目、标签页、图标角标三处的表现 | P2 | 状态 → 呈现的映射为纯函数单测；观感人工验收 |
| F96 | 链路延迟 RTT：走 **SSH 协议层往返**（keepalive 请求的应答时延），**不用 ICMP**——代理链路下 ICMP 常被整段阻断，且测的不是 SSH 实际走的路径 | P2 | 采样与平滑为纯函数单测；真实高延迟链路下数值的合理性人工验收 |
| F97 | 远端系统概况（负载 / 内存）。下面四条硬边界写进验收，因为它们是**拒绝理由**——将来有人提「加个 CPU 曲线图」直接引用本行 | P2 | ①走**独立 exec channel，绝不进 pty**（否则命令会打进用户正在看的 tmux / Claude Code 界面）；②只读 `/proc/loadavg` + `/proc/meminfo`，不调 `top`/`ps`、不做进程列表；③**默认关闭**，轮询间隔可配（默认 5s）；④**只显示当前值，不画图表、不存历史**——一旦存时间序列，它就长成监控产品了。两个 proc 文件的解析为纯函数单测；轮询周期的额外流量有上限断言 |

### 4.9 登录后自动化

> 设计冻结在 `docs/superpowers/specs/2026-08-05-slice-p1-login-automation-design.md`。
> 该设计**推翻了路线图 §5 的两条前提**（分屏语义、SSH env 请求），理由见其 §7。

核心约束（下面每条验收都从它推出）：每次 SSH 连接都会拿到 sshd fork 的**新 login
shell**，所以第一个字节是安全的；危险的是**第二个**——我们自己发出的 `tmux attach`
一旦生效，屏幕就从 shell 变成了那个还在跑的 Claude Code TUI，此后再发命令行，
字符会进它的输入框并可能被当成提问发出去。而客户端无法判断 attach 有没有生效。
所以：**自动化只在「确定还是干净 shell」的那一个窗口期内发字节，一旦发出可能改变
屏幕归属的东西就不再发第二个**——整套自动化编码成一行 shell 条件表达式，由远端
自己原子判断走 attach 还是 new-session，客户端不解析任何远端输出，也不发第二步。

| ID | 需求 | 优先级 | 验收标准 |
|---|---|---|---|
| F40 | tmux 自动 attach：已存在则附着、不存在则新建。会话名默认由会话名称 sanitize 生成（tmux 会话名不能含 `.` 与 `:`），可显式覆盖 | P1 | 生成的字节序列为纯函数单测：有 tmux 时**恰好一步**，且形如 `tmux has-session -t X 2>/dev/null && exec tmux attach -t X \|\| exec tmux new-session -s X …`。**不许用 `new-session -A`**——它无法区分「新建」与「附着」，命令列表和工作目录就没有安全落点。会话名 sanitize 与 shell 转义（含空格 / 单引号）各有单测 |
| F41 | 登录后命令列表，逐条可设延时。**继承取 Override 而非拼接**。**默认合并成一行发送**（`;` 连接），只有用户显式给某条配了延时才拆多步 | P1 | 单测：无 tmux 且无逐条延时时**也只有一步**——`.bashrc` 里自动 attach tmux 是极常见配置，无条件拆多步会让第二条起打进 TUI，与核心约束同坑；配了延时才拆，此时 UI 须写明「后续命令会发给当时屏幕上的任何东西」。命令文本**原样拼接不再 quote**（`echo 'hi'` 要能原样跑），只有会话名/目录/env 值各 quote 一次，最外层由生成函数整串统一转义——各有单测。分组与会话都配了命令时，断言只跑会话那份 |
| F42 | 初始工作目录 | P1 | 有 tmux 时经 `new-session -c`（只在新建分支生效，附着已有会话时不得改其目录）；无 tmux 时为 `cd` 步。空目录时省略 `-c`；启动命令串为空时整个 `'…'` 参数省略——`new-session -s X ''` 会让 tmux 跑一个空命令，行为随版本而异 |
| F43 | 环境变量注入。**只做 `export` 行，不用 SSH env 请求**——sshd 的 `AcceptEnv` 默认只放行 `LANG`/`LC_*`，且 tmux attach 后面对的 shell 是 tmux server 早先 fork 的，不继承本次 channel 环境，env 请求结构性无效 | P1 | 值**存明文进 `sessions.toml`，不进 `secrets.enc`**，UI 须写明「环境变量不是存密码的地方」——值终归要以 `export` 行发进远端，会落进 shell 历史与 `/proc/<pid>/environ`，加密只会给用户错误的安全承诺。日志**只记步数与字节长度，不记命令原文与 env 值** |
| F44 | 自动化总开关（可继承），外加会话列表右键「连接（跳过自动化）」的一次性入口 | P1 | 关闭后行为与未引入本特性时逐字一致的回归测试；「显式关闭」与「继承」在配置里可区分（同 `ProxyChoice::Direct` 的坑）；schema v3→v4 迁移单测（v3 记录读出来自动化字段全空 = 不做自动化，无损）+ `.bak` |

**三条跨切片的硬规则**（不单独发编号，违反即设计错误）：

1. **「登录完成」判定用「首次收到 PTY 输出 + 可配延时」**，不匹配 shell 提示符
   （形态千差万别）。超时（默认可配，**从 `open_pty` 返回时起算**，不含代理/跳板/
   认证耗时）未收到输出则**跳过、绝不补发**。
2. **用户接管优先**：一旦收到任何用户输入（键盘 / 粘贴 / 鼠标上报），立即中止剩余
   全部自动化，并在状态栏说明原因。
3. **只有连接建立时的第一个 pane 跑自动化**。分屏新开的 pane（adr-009，同连接开新
   channel）是干净 shell——所有 pane attach 同一 tmux session 会导致内容镜像，且
   `window-size` 取 `latest` 会反复 reflow、取 `smallest` 会留白，两种取值都踩排版坑。

**调度归属**：定时写入是 `mullion-ssh` 的通用 async 函数，只认 `Vec<(Duration, Vec<u8>)>`，
不认识 tmux / 自动化语义；生成时间表的纯函数留在 `mullion-store`。绝不能进 store
（破坏零 async 红线），也不堆进 app 事件循环（会与 T3/T7 帧率节流打架）。
超时检测要为 deadline 设 `ControlFlow::WaitUntil`，且**三个分支都要复位 control_flow**（T7）。

**行终止符**一律 `\r`，复用 `keymap.rs` 里 Enter 键的既有约定；待 F25 落地后两条路径
统一改走同一份配置——不允许「人手敲回车」与「自动化发送」用不同换行约定。

### 4.10 开发自用工具

不面向终端用户，是为了把「UI 打磨」这类工作的回路缩短。**允许出现在正式 exe 里**，
但必须是零开销、不可误触的（默认关、只有组合键能进）。

| ID | 需求 | 优先级 | 验收标准 |
|---|---|---|---|
| F100 | 标注模式：`Ctrl+Shift+F` 进/出，鼠标悬停给容器与控件描边并显示语义路径，点击选中并打上 ①②③ 编号，`Ctrl+Shift+E` 把一段 Markdown 写进剪贴板（每处含语义路径 + 屏幕矩形 + 插桩点的 `文件:行号`），`Ctrl+Shift+D` 在紧凑/标准/详细三档间循环（**默认详细**）。候选有两个来源：手工 `annotate::mark()` 的**容器**（带真实行号，含分屏整块/标题条/终端区这些非 egui 的自绘区）与 **accesskit 自动树**（模式开着时才构，覆盖全部 egui 控件，挂在包住它的最小手工容器下） | P1 | 模式关着时零登记开销（`enable_accesskit` 只在模式开着时调）；开着时点会话行不会真的切会话；导出的文本粘进 Claude Code 后，能不经追问直接定位到对应代码 |

**为什么要它**：UI 打磨的瓶颈是「用户看到『那个东西不对』」到「Claude 知道那是哪段
代码」之间的带宽。图片这条路是死的——Windows 剪贴板里的图片粘不进 SSH 里跑的
Claude Code，所以产出**必须是纯文本**。思路参考 `kilobitcy/snapmark`（web 端 UI 注释
工具），它的真实价值不是截图标注，是**把视觉位置映射回代码标识**。

**四条边界**（都是刻意的，不是没做完）：

1. **只盖 egui 外壳，不碰 glyphon 终端网格**——网格没有 widget，抓不到身份。
   字形/CJK 对齐问题仍然走 VT 快照或人眼。
2. **注释不在应用内敲**，只给编号，用户在 Claude Code 里口述「第 2 个太挤」。
   中文注释输入要过 winit + 第三方 IME（`CLAUDE.md`「你无法验证的东西」之一），
   而在应用内做文本框会撞陷阱 T8 的键盘路由。
3. **导出不带逐 widget 的样式值**，只带全局上下文（窗口尺寸/缩放/主题/当前页）。
   样式值 Claude 自己读代码更准，写进导出只会让人误以为它是权威。
4. **身份靠 `#[track_caller]` 插桩**，不是 egui 的 callstack —— 后者在 egui 0.30 的
   API 边界外（`mod callstack` / `mod pass_state` 非 pub，`register_widget_info` 整个
   被 `#[cfg(debug_assertions)]` 包住，`debug_painting` 私有）。取舍全表见
   `docs/superpowers/plans/2026-08-09-f100-annotate-mode.md`。

配套的离屏出图 harness（`examples/ui_shot.rs`，见 `docs/ui-shot.md`）不占 F 编号：
它是 example target，不进 exe。

### 4.11 已登记未排期

来源：`docs/session-mng.md`（通用 SSH 客户端字段清单）中**服务于别的产品形态**的条目。
**登记不等于承诺**，也不等于永久拒绝——每期结束复查一次，边界变化时从这里取货。
新增需求若落在本表已覆盖的范围内，先在这里加一行，不要直接开 F 编号。

| 来源 | 条目 | 为什么现在不做 | 重新评估条件 |
|---|---|---|---|
| §1 | 快捷别名（命令行快速连接用） | 没有命令行快速连接入口，没有载体 | F47 快速连接落地后 |
| §2 | IP 协议族偏好（自动 / IPv4 / IPv6） | 走系统默认解析，至今未遇到问题 | 出现真实的双栈解析故障 |
| §2 | 协议：本地 Shell / Telnet / Rlogin | 主线是远程 SSH；本地 Shell 有 Windows Terminal，Telnet/Rlogin 明文协议 | 无（除非产品定位改变） |
| §3 | 主机密钥策略三态（严格 / 首次接受 / 忽略）+ `known_hosts` 路径可配 | 当前固定 TOFU，够用；「忽略」是给用户开安全后门 | 出现必须连接密钥频繁轮换主机的场景。注：F3-a（key 未含端口）是既有欠账，按 spec 自身排期走，与本条无关 |
| §3 | OTP/TOTP 自动填充、expect/send 登录应答、GSSAPI-Kerberos、PKCS#11 智能卡 | 企业认证形态，单人自用场景用不上 | 出现必须用这些认证的目标主机 |
| §4 | 自定义 `ProxyCommand` | F4/F5 已覆盖网络代理与 ProxyJump 两条主路径 | 出现需要任意外部命令拨号的链路 |
| §5 | ~~隧道转发（-L / -R / -D）~~ 与 X11 转发 | 原否掉理由：与「操作远端 TUI」主线无关；隧道有 `ssh -L` 顶着 | **隧道部分已于 2026-08 取货 → §4.12 / F110~F117**（重评条件「出现高频、需图形化管理的隧道需求」被正面触发）。X11 转发仍不做，条件不变 |
| §6 | SSH 协议参数（KEX / Ciphers / MAC / 主机密钥算法列表、压缩、客户端 banner 伪装） | russh 的默认协商已能连通目标主机；暴露这些参数等于把协商失败的排查责任交给用户 | 遇到默认协商连不上的老旧 sshd |
| §8 | 背景图片、背景模糊 | 与 G1「零可见闪烁」直接冲突——每帧多合成一层 | 无 |
| §8 | 深 / 浅色模式跟随系统主题 | 现有视觉基线是单一深色方案，引入浅色要重做整套 token 与终端配色，成本远超收益 | 出现真实浅色场景（白天户外 / 投屏演示） |
| §9 | 日志与录制（目录模板、时间戳、剥离 ANSI、脱敏规则、asciinema 回放） | 远端 tmux 本身可录；客户端侧录制会与 F72「不上报遥测」的信任叙事混淆 | 出现合规留痕要求 |
| §10 | ZMODEM / XMODEM、传输模式（ASCII/二进制）与同名冲突策略 | 有 SFTP（F50~F57）即可；ZMODEM 是串口时代遗留。**SFTP 侧栏本体不在储备区** | 无 |
| 4.4 | ~~F51 跟随当前 pane 的 shell cwd 自动切目录（OSC 7）~~ **原 P1，2026-08 移出** | 三条叠加导致在本项目主场景恒失效：① `vte 0.15` 的 `osc_dispatch` 只认 OSC 0/2、4、8、10~19、52、104，**没有 OSC 7**；② Ubuntu 的 bash 默认 `PROMPT_COMMAND` 根本不发 OSC 7（那是 `vte.sh` 干的）；③ 就算发了，**tmux 会自己吃掉它**去更新 `pane_current_path`，默认不向外层透传——而「远端 tmux 里的 Claude Code」正是本项目的核心场景。替代方案是 F120 书签 + 可配置默认目录 | 做 tmux 深度集成（能直接问 `#{pane_current_path}`）时可重提 |
| 4.4 | 传输断点续传 | 续传要保证「本地已下载前缀」与远端当前内容一致，而没有廉价校验手段（远端不一定有 `sha256sum`，SFTP 也没有部分校验请求）。为省几十秒去赌一个**静默损坏**的文件不划算；断线后整文件重传 | 出现常态化的大文件传输需求时，做成「>50MB 断点续传」的知情开关，**默认仍关** |
| §11 | 登录脚本引擎（Shell / JS） | F40~F44 的命令列表已覆盖 90% 场景；脚本引擎是另一个安全面 | 命令列表被证明不够用 |
| 4.9 | tmux grouped session（`tmux new-session -t <src>`：多 pane 共享窗口集合、各自独立当前窗口与尺寸） | 这是「多 pane 都想连同一台机器的 tmux」的真解，但 F40 只做单 pane 自动化就够用，且 grouped session 的生命周期（源会话销毁时组员的去向）要单独设计 | 多 pane 内容镜像被证明是真实痛点 |
| §13 | Serial（波特率 / 数据位 / 停止位 / 校验位 / 流控 / 设备号） | N-G3 明确非目标 | 无 |
| §1 附录 | 首次连接读 `/etc/os-release` 自动推断发行版图标 | 识别源与建议策略未定，非刚需；且**推断绝不能进颜色/图标的解析链**，只能出建议等用户点头，否则会出现「连一次机器颜色自己变了」 | F61 内置图标库落地后 |
| §A | 会话列表的树 / 网格视图、`tag:prod` 式搜索前缀语法 | 几十台机器规模下，扁平列表 + 分组 + 模糊搜索够用 | 会话数破百 |
| §A | 左栏协议筛选 chips（全部 / SSH / SFTP） | **理由已于 2026-08 更新**：不再是「只有一个有效值」（F118 之后库里真的有两类了），而是**模式条已经承担了协议这条轴**（F118），再加一层筛选是同一件事做两遍 | 出现第三类协议、或用户需要「跨协议一起看」的明确场景 |
| §A | 从 PuTTY / Xshell / MobaXterm / Termius 导入 | F2 已覆盖 `~/.ssh/config` 这条主路径；其余格式各有私有编码与加密 | 出现真实迁移需求 |
| §B | 传输态展示（速率 / 队列长度） | 依赖 F50~F55 先落地 | F55 传输队列完成后 |
| §C | 凭据的「引用 + 局部覆盖」二义模型 | F74 明确选了严格二选一；二义状态会让「这条会话到底用哪个用户名」永远需要现场推理 | 无 |
| §C | 分组持有默认凭据（凭据可继承） | 见 4.5 表下的说明：会造成登录身份被静默切换 | 若新建会话频次高到 UI 预填也嫌烦 |

**一条勘误**：清单 §2 的「禁用 Nagle 算法」**已经实现**——`mullion-ssh` 的 `session.rs`
里 `set_nodelay(true)` 是现有行为，不需要排期，也不属于储备区。

### 4.12 隧道转发

从 §4.11 的 §5 行取货（2026-08）。设计全文见
`docs/superpowers/specs/2026-08-11-tunnels-design.md`。

**隧道是一等对象**：它引用一条会话（取其地址/凭据/代理/跳板链），但**另开一条专用
SSH 连接**，因此不开任何 pane 也能独立运行（G5）。这有意偏离 PuTTY 的「隧道属于
会话」模型，也有意偏离 ADR-009 的「一条连接承载多单元」——后者的推理针对 pane
（数量随分屏增长、频繁开关），不适用于手动启停、长期驻留、个位数的隧道。

| ID | 需求 | 优先级 | 验收标准 |
|---|---|---|---|
| F110 | 隧道数据模型：`sessions.toml` 的 `[[tunnel]]`，schema v6 → **v7**。引用会话且**纯引用不覆盖**（要换身份就建新会话，理由同 §C 拒绝二义模型），因而**无独立 `secrets.enc` 条目** | P1 | v6 文件加载后 `tunnel` 为空数组且既有字段一个不丢；隧道 id 独立于 `SessionId` 分配；删除隧道不触碰 secrets |
| F111 | `-L` 本地转发：本机 `TcpListener` → `channel_open_direct_tcpip` | P1 | 起真 `TcpListener` + 假 channel 端，断言双向字节搬运与关闭传播（无需 GPU/远端） |
| F112 | `-R` 远程转发：`tcpip_forward` + `server_channel_open_forwarded_tcpip` 回调。远端绑定地址**可请求但不可验证**（sshd 默认 `GatewayPorts no` 会静默降级为 `127.0.0.1`，协议响应不回绑定地址），UI 须明写这一点 | P1 | 回调的**每条**提前返回路径都 `channel.close()`（channel 泄漏，同 ADR-009 不变量 3）；「无法验证」声明有渲染断言 |
| F113 | `-D` 动态转发：本机 SOCKS5 **服务端**（RFC 1928）→ `direct-tcpip`。复用 `proxy.rs`（F4 的 SOCKS5 客户端）的常量与 `duplex` 测试手法。**不做 UDP ASSOCIATE**（SSH 无承载，回 `0x07`）、**只提供 `NO AUTH`**（F117 已锁死本机，加认证挡不住任何东西） | P1 | 三种 ATYP、非 5 版本字节拒绝、`UDP ASSOCIATE → 0x07` 均有单测 |
| F114 | 有界指数退避重连（1s→2s→…封顶 30s，8 次后停）。**仅在无需交互时自动**（缺密码/口令则停下等人）；**指纹变更立即停止且绝不重试** | P1 | 退避序列是纯函数单测；指纹变更不进重试队列的断言须能自证变红 |
| F115 | 状态栏隧道指示器，按**最坏**状态三态上色（全运行/重连中/失败）+ 跃迁到失败态弹一次 toast | P1 | 三态取最坏而非取第一条，有单测；状态栏常驻不受弹窗开关影响（同 F3 `last_error` 的兜底理由） |
| F116 | 会话管理器顶层模式切换：**会话 / SFTP / 隧道**（第三档见 F118）。左栏一级组织轴**不变**（仍按分组归桶）；隧道列表不做分组、不做三档密度 | P1 | 切模式不污染另一侧编辑器的脏标记，有单测；模式条须过 F100 登记；**键盘动作按模式分流**——隧道档的 ↑↓/Enter/Ctrl+N 一律 no-op（原先无模式判断，会操作看不见的会话列表） |
| F117 | 侦听绑定安全：默认 `127.0.0.1`。`-L` 可勾选放开到 `0.0.0.0`（红字警告点名具体目标），**`-D` 硬禁止**——放开等于把本机变成无认证的开放 SOCKS5 代理，性质与 `-L` 暴露单一目标不同 | P1 | `Dynamic` 变体在**类型上**就没有暴露字段（非法状态不可表示）；缺键默认值必须是安全值 |
| F118 | 会话管理器 SFTP 节点档：模式条第三档「会话 \| SFTP \| 隧道」，SFTP 节点 = `protocol == Sftp` 的 `SessionRecord`（**schema 不动**，仍 v7）。协议字段改**只读**（要换协议就新建）；SFTP 档隐掉「登录后」页；连接入口置灰（F50 未实现）；隧道「经由会话」只列 SSH。SFTP 档复用会话列表的分组与三档密度，不像隧道档那样特化 | P1 | 分流真源是纯函数 `protocol_of`，`list::show` 与 `visible_order` **共用**同一判据（只过滤渲染侧会让方向键跳到看不见的行，有守护测试）；Tab 下标走 `visible_tabs` 映射（隐藏中间页后用 `enumerate` 序号会让点「图标」打开「登录后」）；连接意图有一道兜底闸门 + 各入口置灰两层 |
| F119 | 表单布局规范：骨架构件抽 `session_manager/form.rs`（分节/两列 Grid/必填星号/内联红字），规范文档 `docs/ui-form-guidelines.md`，两条扫源码的机械守护 | P1 | 守护判据是「参数不得是数字字面量」而非白名单式（后者会误伤 `let w = field_w(..)` 再 `desired_width(w)` 的合规写法）；必须实测自证变红；文档须写明守护挡不住的三类 |

**边界**（刻意不做）：X11 转发（仍在 §4.11）；隧道自动启动（`autostart` 字段已落盘，
UI 不出——每条隧道启动都是一次完整建链，可能要 TOFU/密码/口令，开机并发拨 N 条
等于糊一屏模态框）；隧道分组与三档密度；自由填 bind 地址。

---

## 5. 非功能需求

全部要**可测**。不可测的指标不写进这份文档。

| ID | 指标 | 阈值 | 测法 |
|---|---|---|---|
| N1 | 空闲 CPU 占用 | < 1%（单核） | 连接 4 个 pane 静置 60s，采样。**v0.1.79 起可从一行日志直读**（F179）：`profile.cpu` 的 `total=` 是单核口径两位小数，与 1% 比大小即可。v0.1.78 及更早的日志里它是归一后的整数百分点，`total=0%` 不是达标证据；任何版本都不许拿「窗口没写盘」当证据——空闲门比这条指标松五十倍。**2026-08-28 实机首测（v0.1.79，79 分钟 / 332 个窗口）**：近空闲窗口（54 个，`frame≤12` 且 `in=0B` 且 `key=0x`）`total` p50 **0.93%** / p95 4.76%，`main` p50 **0.00%** —— 按 p50 达标，但**整个预算花在诊断自己身上**：`watchdog/total` 中位数 100%，应用本体低于测量分辨率。F182 之后需要复测。**另发现一层分辨率天花板**：`total=` 的取值全部是 **0.31% 的整数倍** —— Windows 调度器时间片 15.625ms 落在 5 秒窗口上就是 0.31%，`GetProcessTimes` 的粒度到此为止。N1 的阈值等于 **3.2 个时间片**，够用但没有余量；再想提精度只能拉长窗口，换单位没用（F179 已经把刻度从 16% 降到 0.31%，那条路走完了） |
| N2 | 流式输出时 CPU | < 15%（单核） | 回放 fixture 字节流，采样 |
| N3 | 重绘频率 | ≤ 显示器刷新率，不超过 | 埋点计数，单测断言帧率上限 |
| N4 | 冷启动到可输入 | < 500ms | 埋点计时 |
| N5 | 常驻内存（8 pane，10000 行回溯） | < 300MB | 采样。**2026-08-28 实机达标**：`ws=226MB`（专用工作集，与任务管理器同口径）。注意 `commit=323MB` 同时超 300——两个口径差着 100MB，判据取 ws（F176/F177） |
| N6 | 安装包体积 | < 25MB | CI 断言 |
| N7 | 按键到字符上屏 | ≤ 1 帧 + RTT | 埋点，不含网络部分需 < 16ms |
| N8 | 空闲连接存活 | ≥ 30 分钟无流量不断 | keepalive 30s；真实链路人工验证 |
| N9 | 大输出吞吐 | ≥ 5MB/s @ 200ms RTT | 集成测试 `cat` 大文件计时 |

---

## 6. 里程碑

| 版本 | 范围 | 完成的判定 |
|---|---|---|
| **v0.1** | F1–F3, F6, F10–F17, F70, F72 | **单 pane 能跑真实 Claude Code 全屏 TUI，我目视确认不闪** |
| **v0.2** | F30–F35 | 四宫格分屏，每格独立跑 agent，排版全对 |
| **v0.3** | F11/F12 深化 + N1–N3 达标 | 性能指标全绿 |
| **v0.4** | ~~F50–F55~~ **已并入 D 系列**（2026-08-12，范围扩为 F50/F52~F59/F120） | SFTP 侧栏可用 |
| **v0.5** | F4, F5, ~~F36~~（已提前到 D0）, F37, F148, F71 | 代理、跳板机、持久化 |
| **插队** | F110–F117 | 隧道可用（§4.12，2026-08 取货）。**排在 F50–F55 之前**，SFTP 相应推后 |
| **插队** | F118–F119 | SFTP 节点管理 + 表单规范（2026-08）。**F50–F57 SFTP 传输本体仍在其后**（含 P2 的 F56/F57，比 v0.4 里程碑的 F50–F55 范围更大），本档只管节点配置 |
| **插队** | F74 | 凭据实体（2026-08-13，v0.1.41）。一份凭据被多条会话引用，换密钥只改一处；schema v8→v9。**F75 去重提取仍未做** |
| **插队** | F2 | 导入 `~/.ssh/config`（2026-08-13，v0.1.42）。`spec.md` 里最后一个 P0 落地 |
| **插队** | F181–F183 | 探针自陈 + 线程枚举缓存句柄 + 采样顺序（v0.1.80，2026-08-28）。**由 v0.1.79 的实机日志驱动**（79 分钟 / 332 个窗口）：N1 首次可读、而读出来的结论是「近空闲 p50 0.93%，其中 100% 是看门狗自己」——**尺子占满了预算**；同一份日志里 19% 的窗口 `main + 各组 > total`（采样顺序）。**F181 是「不猜、让下一份日志自己答」的产物**：源码读不出 Toolhelp 与 PDH 通配枚举谁是大头，与其二选一赌一把，不如先花一行日志把它测出来。同批把 N1/N5 的实机判决写回本表 |
| **插队** | F178–F180 | CPU 口径分辨率 + 脱敏器 token 判据（v0.1.79，2026-08-28）。**全部由一份 v0.1.78 的实机日志驱动**（34 分钟 / 286 个窗口）：F171 的幽灵事件假设被证伪、N1 被发现根本没有观测能力、空表占位符 `-` 被脱敏成假主机名。**F178 在动手前被同一份日志否掉**——按场景聚合后白跑帧的 CPU 上限只有 0.27%~0.95% 单核，而收口点是 T7 那段；证据留在条目里，不实现。同批把 F174/F175 补登记进本表——它们随 v0.1.76 已交付且有守护测试，spec 里一直缺行 |
| **插队** | F190–F191 | 内存棘轮定位：装计数分配器 + resize 埋点量化后否掉（2026-08-28，**未发版**）。由 v0.1.80 的实机日志驱动（52 分钟 / 437 个窗口）：F182/F183 在实机确认修好（看门狗 32% → 2.02%，437/437 窗口线程记账自洽），但 `其他:` 那 86MB 的单调棘轮在 F169 的口径下**无法定位**——余量桶答不出「是谁」。**为此推翻了 F169 当初「不上自定义分配器」的拍板**：代价评估当时是对的（诊断指标 vs 全局 allocator），变的是回报——现在有一个具体的、跑了 52 分钟仍在涨的东西要定位，不再是「万一以后有用」。F191 是同一轮里**被自己的数据否掉的第二条**（第一条是 F178），处置一致：留量化证据、不实现 |
| **插队** | F184–F189 | 实机报障六修（2026-08-28，**未发版**）。全部由用户实机反馈驱动，六条互相独立：时间列混排（F184）／右键先夺焦点（F185）／mtime 按本机时区（F186）／本地收藏夹改全局（F187）／恢复现场首屏卡「连接中…」（F188，P0）／收藏夹被覆盖（F189，P0）。**两条 P0 的共同形状是「判据放错了层」**——F188 拿「有没有 `PaneState`」当拨号类型的判据，而那正是被 bug 改变的量；F189 拿「整份 draft」当书签的真源，而书签根本不从表单来。这与 F160–F163 三个 Critical 是同一族 |
| **插队** | F192–F196 | 文字层内存归因与 run 合并（2026-08-29，设计见 `docs/superpowers/specs/2026-08-29-f192-f196-text-buffer-accounting-and-run-merging-design.md`）。由 v0.1.81 实机日志驱动（PID 22508，38 分钟 / 97 条 `profile.mem`）：`堆=98MB` 里已记账只有 27MB，**71MB 未归因**，反推出 `text:` 的计数口径低报一个数量级、且 `pool` 里躺着约 6500 个空闲 Buffer。**同轮否掉三条**：⑴ 原分析「大头在 GPU 侧、两个独立口径对上了」——`ws=244 < vram=258` 的不等式自相矛盾（驱动分配是锁页常驻的，不可能超过工作集），且 commit 不含 mmap 而字体走 `fontdb/memmap`，两把尺子量的不是一个东西；用户拍板不做前置验证、按原排序动手，证据留在设计文档 §1.2，若 F193/F194 落地后体感无改善从那里接着查。⑵ `bands` 留 N 帧（见 F196）。⑶ **N5 从 300 收到 150**——`MEM_REPORT_STEP_MB=64` 意味着 ws=244 会立刻越界并**永久停在越界态**，下次出声要等 308，**244–308 这个最可能发生回归的区间从此失明**；且这条线**从没按定义载荷量过**（spec 原文是 8 pane / 10000 行，而实测是 3 pane / 436 行就 244MB）——**N5=300 八成早就不达标，问题在量法不在阈值**。处置：先补一次 8 pane 的实机采样，本轮落地后再按「实测 + 余量」棘轮式收线 |
| **插队** | F197–F198 | 光标可见性两修（2026-08-30/31，v0.1.83 / v0.1.84）。**同一个现象的两面，且互相掩盖**：F197 修「远端说别画我们照画」（随机位置冒出光标），修完 F198 那条「远端画了我们没认」（Claude Code 的反显块输入光标）立刻从被掩盖变成实报。**两条都是靠实录字节流定的性**，不是读源码猜的——顺手补上了本项目第一个 VT fixture 与它的脱敏纪律 |
| **插队** | F199–F204 | SFTP/编辑器交互六修（2026-08-31，v0.1.85，设计见 `docs/superpowers/specs/2026-08-31-f199-f204-sftp-editor-interaction-design.md`）。全部由用户实机反馈驱动，**其中三条的真根因是同一个**：F5/F2/Del 从 D1 起就写好了，只是键根本路由不到面板上（`focus` 只有 F6 改得动）——F199 那一次「点面板即夺焦点」是 F200/F202 与既有 F5 的共同前提。另三条互相独立：路径全选（F201）／弹窗底色与标题栏 ✕（F203）／编辑器窗口件（F204） |
| **插队** | F205 | 连接身份归属（2026-08-31，v0.1.86）。SFTP 远端书签「升级后消失」的真根因，三轮误修之后才找对层：不是 store 丢数据，是在途第二条拨号把「这次连上的是谁」那个单槽盖掉了。同一个根因还解释了另外三桩没人报过的错乱（分屏用错主机的 cfg、登录后命令跑错机器、tmux 接错会话） |
| **插队** | F206–F208 | 内置编辑器三修（2026-08-31，v0.1.87）。全部由用户实机反馈驱动：焦点描边同源（F206）／正文底色（F207）／固定尺寸与屏幕夹紧（F208）。**F208 的根因是 egui `Resize` 的正反馈棘轮**，不是「忘了设默认尺寸」 |
| **插队** | F209 | 截屏直传（2026-08-31，v0.1.88）。用户诉求是「跟 Claude Code 说话时能方便地引用截屏」，落成终端里一次 `Ctrl+V`：剪贴板位图 → PNG → sftp → 绝对路径打进输入行。自解 `CF_DIB` 而不是开 `arboard/image-data`（N6 exe 体积那条决定）
| **插队** | F210 | 组字锚点（2026-09-01，v0.1.89）。用户实报「`/compact` 时打中文闪烁」。真根因不在渲染层：**T2 的同步块攒帧经 tmux 转发时压根不生效**（TERM 报的是 `xterm-256color`，tmux 不往外发 BSU/ESU），我们会画出远端重绘的中间帧，拼音和候选框跟着光标乱跳。先把锚点钉住治掉症状；让 tmux 真的转发同步块见 F211 |
| **插队** | F211 | 让 tmux 真的转发同步块（2026-09-01，v0.1.90）。F210 的欠账收口：attach 前在远端登记 `terminal-features[99]=xterm-256color:sync`。实测「内层 20 个块 → 外层 0 个」变成「20 个原样到达」，**T2 的攒帧第一次在主场景里生效** |
| **插队** | F212 | 划选不再被远端重绘冲掉（2026-09-01，v0.1.91）。用户实报「`/compact` 时按着左键选不中文字，高亮出现又被冲掉」。根因在上游 alacritty：擦一行就把**整段**跨行选区丢成 `None`，而 `selection_update` 在 `None` 上静默 no-op。按住左键期间给选区留底、只补被丢空的那一种 |
| **插队** | F213 | 弹窗/toast 配色收口（2026-09-01，v0.1.92）。用户实报「对比度不够、层次乱」。toast 底色与弹窗同源 + 三档语义边框；弹窗灰阶收成两档；新增 `danger_text`；F203 的列举式闸门换成穷尽式（当场揪出 5 个文件 13 处漏网）
| **插队** | F214 | 编辑打开减往返 + 分段埋点（2026-09-01，v0.1.92）。用户实报「几十 KB 的文件也要等好几秒」。成本单位是**串行 READ 的往返数**：缓冲 64→256 KiB（1 MiB 文件 16 次 RTT → 5 次），再用列目录已知的大小省掉「只为问 EOF」那一次。慢在哪本机量不到（RTT 为零），所以让程序自陈 open/read×N/stat 各段
| **插队** | F215 | 内置编辑器语法高亮（2026-09-01，v0.1.92）。按行增量 + 每帧不重拼 `LayoutJob`（egui 的 `layouter` 每帧都跑，照直写就撞 T3/N3）。增量判据是**进入这一行的解析状态**，不是行内容——否则改一个 `/*` 之后底下整片**静默**停在旧颜色
| **D 系列** | F36 → F50/F120 → F54–F57 → F53 → F52/F58/F59 | SFTP 文件浏览器（2026-08-12 取货，设计见 `docs/superpowers/specs/2026-08-12-sftp-browser-design.md`）。分五片各自发版：**D0** 标签页最小版（F36 提前，因为 SFTP 节点连上后需要地方安放）→ **D1** 只读浏览 + 书签/默认目录（F50/F120，schema v8）→ **D2** 写操作与传输（F54–F57）→ **D3** 编辑（F53）→ **D4** 拖拽三类（F52/F58/F59）。F37 持久化**不在内**，仍归 v0.5 |

**v0.1 的判定标准是人工目视，不是测试通过。** 终端仿真的坑只在真实 TUI 下暴露，
自己写 demo 测不出来。别等功能齐了再去验证这一条。

---

## 7. 风险与未决

| 风险 | 影响 | 当前对策 |
|---|---|---|
| R1 | 依赖 crate（russh/winit/wgpu/glyphon）API 漂移 | 锁死版本；升级单独开 PR，不混在功能里 |
| R2 | 第三方中文输入法在 winit 下行为异常（WezTerm 有过闪退先例） | v0.1 就要在真机上试搜狗/微信/小狼毫，**这是可能推翻技术选型的风险，优先验证** |
| R3 | 自研渲染器在多显示器 / 混合 DPI / 独显切换下出问题 | 早期就在双屏不同缩放下测 |
| R4 | 高延迟下 TCP 上的 SSH 天然抖 | v0.5 之后评估 UDP 方案（参考 tsshd） |
| R5 | 一个人维护，范围失控 | 非目标清单从严执行 |

**已决问题**（原为未决，决策与理由见 `docs/adr-*`）：

- Q1 字形渲染：glyphon 通用文本路径 vs 等宽网格专用 wgpu pipeline → **v0.1 用 glyphon**，
  N2/N3 埋点显示瓶颈落在文本布局时再换专用 pipeline。见 ADR-001。
- Q2 配置文件格式：TOML vs SQLite → **TOML**；敏感字段单独加密，与格式正交。见 ADR-002。
- Q3 是否支持 tmux `-CC` control mode → **不做**（至少 v0.5 前），与自研分屏定位及 N-G1 冲突。见 ADR-003。

---

## 8. 词汇表

| 词 | 含义 |
|---|---|
| pane | 分屏后的一格，一格对应一条 SSH channel 和一个 VT 仿真器实例 |
| BSU / ESU | 同步输出的开始 / 结束（`CSI ? 2026 h` / `l`） |
| damage | alacritty_terminal 提供的逐行脏区信息 |
| fixture | 录制下来的真实终端字节流，用于快照测试 |
| TOFU | Trust On First Use，首次连接记录主机密钥指纹 |
