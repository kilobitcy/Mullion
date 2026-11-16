# F348/F349/F350 手机打开项目(最小可用版)· 设计

> 状态:定案(2026-11-16)。总设计 `2026-11-12-android-client-design.md` §3;F347 门控后补。
> 目标:在 P50 上导入 `.mullionpack` 后,点项目 → 连上 → attach 项目 tmux → 能看能回。

## 分层(总设计 §2:判据一份,编排各写)

- **复用不改**:store 的 `Vault::open`/`resolve_auth`/`resolve_for`/`expand_jump_chain`、
  `candidates`/`rows`/`relative`、`overlay_project` + `build_plan`(自带 T2' sync 登记)、
  `KnownHostsFile`;ssh 的 `connect`/`probe_first_hop`/`write_scheduled`/`host_key_id`。
- **ffi 重写**「会话记录 → `SshConfig`」映射(桌面那份在 `mullion-app/src/shell/`,ffi 不能依赖)。
  手机语义本就不同:**没有 ssh-agent**,目标或跳板缺密码/私钥一律报错点名,不退回 Agent;
  SFTP 节点拒绝。

## F348 项目列表(ffi `list_projects`)

- 入参:vault 目录、32 字节数据密钥、搜索词、本地 UTC 偏移秒(Kotlin 给,Android 上
  `time` 的 local-offset 不可靠)。
- 出参每行:id、名称、目录、「最后打开」相对时间(`relative`;从未打开 = 「从未打开」)、节点数、是否归档。
- 排序/筛选 = `rows(.., Tab::Active, query, ..)`,指示灯传空表:浏览只列在用,搜索穿透归档(F257)。
- 备忘只读放 F348-b。

## F350 TOFU(ffi `PhonePolicy` + `trust_host_key`)

- 查 vault 目录下 `known_hosts.toml`(包里带来的):一致放行;不一致硬拒(`ConnectError::HostKeyChanged`);
  没有记录 → 拒绝并把 `(host_id, algo, SHA256 指纹)` 记在策略里。
- `open_project` 遇未知主机返回 `OpenOutcome::UnknownHostKey`(不是错误),Kotlin 弹窗;
  用户确认 → `trust_host_key` 写手机本地 → 重新 `open_project`。跳板链每跳各确认一次。
- `trust_host_key` **不覆盖**已有的不同指纹(那是「变了」,只能硬拒),配测试。
- 不做「拨号中挂起等确认」:要跨语言异步回调,收益小。

## F349 打开项目(ffi `open_project`)

1. 候选顺序 `candidates(p, is_routed)`,`is_routed` = 解析后代理非 Direct 或跳板非空(桌面 `badge.rs` 同判据)。
2. F304:逐条组拨号参数(组不出来记原因换下一条);非最后一条先 `probe_first_hop` 3 秒,
   不通记原因换下一条;最后一条不探。全失败报错附每条原因。**探通后正式拨号失败不换**,直接报错(桌面同口径)。
3. `connect`,term = `SYNC_TERM`(守护测试)。
4. 自动化:`build_plan(overlay_project(p, resolved.automation), "")`;泵收到首字节后
   `write_scheduled`;首字节等待上限 `ready_timeout_ms`(T11:计时起点在拨号完成之后);
   用户先打字 = 接管,取消计划。**attach 不带 `-d`**,与桌面共挂。
5. 手机不回写访问时间(一期手机上不改数据)。

## 终端界面(够用即可,正式版 F351/F352)

- 纯文本行视图;列/行按可用宽高与等宽字宽算,尺寸变化即 `resize`(T4)。
- 发送框保留;快捷键栏 `Esc Tab ↑ ↓ Enter Ctrl+C`。「断开」回列表。
- 有项目时,未连接页顶部是搜索框 + 项目列表;没项目时显示原手动表单(调试/插桩用)。导入、云端两节照旧在下方。

## 测试

- Rust 单测:映射(密码/私钥+口令/凭据引用/跳板缺凭据报错/SFTP 拒绝/代理口令成对)、
  `PhonePolicy` 三分支、`trust_host_key` 不覆盖、`list_projects` 筛选与时间、term 与 `SYNC_TERM` 同源。
- Rust 集成(进程内 echo sshd):未知主机 → `UnknownHostKey` → 信任后重开成功;
  attach 计划字节回显到屏幕且不含 `attach -d`;两节点项目首条不通换第二条;首字节前用户打字取消计划。
- 插桩:新资产包(节点指向宿主机 `10.0.2.2:2222` echo sshd)导入 → 列表 → 点开 → 确认指纹 → 屏幕出现 `tmux has-session`。
- **未验证,需在 P50 人工确认**:真实代理/跳板链路、Claude Code 画面可读性、与桌面共挂的尺寸拉扯、快捷键在 Claude Code 里是否够用。

## 不做

F347 门控、备忘、彩色/Canvas 渲染、手势、后台保活与重连、访问时间回写。
