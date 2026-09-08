# 设计:「项目」管理单位(F221~F225)

> 2026-09-08。来源:用户对 v0.1.95 提的新增管理单位「项目」。
> 全部决策经一轮 grilling 逐条拍板,下面的「已拍板边界」不再重开。

## 一句话

**项目 = 一台机器上的一个开发目录 + 到达它的若干条等价路线 + 一个专属 tmux 会话。**
打开项目 = 连到首选节点、attach 进那个 tmux 会话、终端与文件面板都落在那个目录。
项目是这个客户端的**工作单位**,会话是**路线**,分组是**机器分类** —— 三者正交。

## 二、为什么它不是「分组加几个字段」

`GroupRecord` 已经是「一组会话 + 可继承的 automation/terminal/appearance」,
表面上覆盖了 80%。否掉复用的理由:

- 分组的语义是「这批机器天然同类」(生产/测试),一条会话**只能属于一个**分组。
  项目的语义是「我在干的这个活」,一台机器上会同时有多个项目。
- 分组的字段走 `inherit.rs` 的继承链。项目的目录/tmux 名**不进继承链** ——
  它们是打开那一刻的**一次性覆盖**,不写回任何会话记录。把它们塞进继承链要
  回答「项目的 tmux 名和分组的 tmux 名谁赢」,而那是个没人想回答的问题。

## 三、已拍板的边界

| # | 决策 | 否掉的备选与理由 |
|---|---|---|
| P1 | **项目的节点必须指向同一台物理机器**,多节点 = 等价路线(不同凭据/端口/跳板) | 否掉「允许跨机器 + 每台机器各存一个目录」:那时 `(节点, 目录)` 才是真正的记账单位,「项目」退化成一个标签,不如直接用分组的 tags |
| P2 | **同机判据 = `known_hosts` 的 SHA256 指纹**,三态处理 | 否掉「`host` 字符串相等」:那把「IP + 域名指向同一台」直接判死,而那恰恰是要多节点的理由。否掉「未连过就拒绝加入」:会让「离线整理配置」被一次强制连接卡住 |
| P3 | **项目定义进 `sessions.toml` 的 `[[project]]`,schema v9→v10** | 否掉独立 `projects.toml`:引用 `SessionId` 却存在另一个文件里,悬垂引用要自己收拾,且 F189 那套「落盘前先重读」的多实例安全机制要重造一遍 —— 那条路上本项目已经踩过一次 P0 |
| P4 | **打开项目 = 一次带参数的换节点**(走既有 `spawn_rehost_on`) | 否掉「在当前已连的 pane 里发 `cd` + attach」:直接违反 F40~F44 的核心不变量(自动化只在「确定还是干净 shell」的窗口期内发字节)。当前 pane 十有八九正 attach 在 tmux 里跑 Claude Code,发进去的 `cd` 会变成 Claude 输入框里的一行字 |
| P5 | **项目一律强制 tmux**,覆盖会话/分组哪怕显式配了 `Off` | 否掉「只覆盖名字不覆盖开关」:用户在某条会话上关过 tmux(可能是很久以前为了调试),之后所有走这条节点的项目全都不保活,而界面上完全看不出来。没有 tmux 的项目 = 断线就丢工作,那不是可用状态 |
| P6 | **多节点时用「首选节点」,不自动故障切换** | 否掉「连不上自动试下一条」:「连不上」的判据在高延迟代理链路上定不出来(超时多久?认证失败算不算?),且降级意味着**静默换一套凭据**去连机器。T11 的教训正在这类「等多久」的判据上 |
| P7 | **「正在运行」与「访问时间」共用一个判据:pane 上报的 tmux 名 == 项目 tmux 名** | 否掉「我们打算 attach 的名字」:那是**意图**不是**结果**,`attach_guarded` 的 `has-session &&` 短路时我们停在裸 shell 上,而记账已认为进去了 |
| P8 | **tmux 名 + 项目名全局唯一**,保存时校验 | 否掉「按机器唯一」:判据是「同机」而那是三态,待核时给不出答案。全局唯一是**离线可判的纯函数**,符合 store 零 IO 的架构不变量 |
| P9 | **项目目录一个字段管两处**(终端 `work_dir` + 文件面板落脚点) | 否掉「像 `SftpPrefs` 那样分成两个」:那两个分开是因为语义真的不同(工作目录 vs 截图垃圾桶);项目的定义就是「一个 codebase 目录」,终端和文件面板是同一件事的两个视图,拆成两个输入框用户 99% 填成一样再漏改一个 |

---

## 四、F221 数据模型与校验(`mullion-store`)

新文件 `crates/mullion-store/src/project.rs`,零 IO、纯数据 + 纯函数。

```rust
pub struct ProjectId(pub u64);          // 取现有 max+1,同 SessionId/GroupId

pub struct ProjectRecord {
    pub id: ProjectId,
    pub name: String,                    // 全局唯一
    #[serde(default)] pub note: String,
    pub nodes: Vec<SessionId>,           // 同机的等价路线
    pub preferred: Option<SessionId>,    // 首选节点;None = 无可用节点
    pub dir: String,                     // 终端 work_dir + 文件面板落脚点
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_name: Option<String>,       // None → sanitize_tmux_name(name)
    pub created_at: String,              // RFC3339,由 app 注入
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_accessed_at: Option<String>,
}
```

**时钟由 app 注入**,store 不持有 —— 同 `SessionRecord.modified_at`。否则 store
的纯单测没法固定时间。

### 最终 tmux 名

```rust
pub fn project_tmux_name(p: &ProjectRecord) -> String {
    sanitize_tmux_name(p.tmux_name.as_deref().unwrap_or(&p.name))
}
```

**留空时回退到项目名,绝不回退到会话名。** 回退到会话名的话,同一台机器上的
两个项目会 attach 进同一个 tmux 会话,**两个项目共用一个 Claude Code** ——
这是本设计里后果最严重、且完全静默的错误。

### 唯一性与结构校验

纯函数,保存前跑,撞了拒绝并指出跟谁撞:

- 项目名全局唯一(列表里两个同名项目用户根本分不清,而这个列表是拿来点着切活的)
- `project_tmux_name` 的结果全局唯一
- 也要与**会话上显式配置的** `TmuxChoice::Attach { session_name: Some(..) }` 比对
- `preferred` 必须 `∈ nodes`(或 `nodes` 为空时为 `None`)
- `nodes` 只接受 `Protocol::Ssh` 的会话 —— SFTP 节点没有 PTY,attach 过去是
  一块永远不出字的黑屏。这是**第一道**闸;rehost 的 `wants_sftp` 检查
  (`app.rs:7736`)仍是第二道

**故意不完备的缺口,明写在代码注释里**:不校验「会话留空时由会话名推导出来的
名字」。那个值会随会话改名而变,追不过来,追了还会在改会话名时反过来把项目卡住。
残余风险:用户把某条会话改名成恰好等于某个项目名时可能撞车。

### 删会话时的处置

从各项目的 `nodes` 里摘掉;`preferred` 命中则置 `None`。
**摘空了也不删项目** —— 目录和 tmux 名是用户手打的东西,不能因一次删会话蒸发
(同 `IconKind` 那几个历史变体不敢删的思路:别让一次操作静默销毁用户输入)。
项目进「无可用节点」态,列表里可见、可编辑、不可打开。

### schema v10

`SessionsFile` 加 `#[serde(default)] pub project: Vec<ProjectRecord>`,
`CURRENT_SCHEMA` 升到 10。**没有一行迁移转换代码**(旧文件没这个键 → 空表),
升版本号的理由同 v8/v9:旧客户端读 v10 会把整个 `[[project]]` 表当未知字段
丢掉再写回,**用户的项目静默消失**。拒绝比装作能用好。

### 落盘

`Vault` 新增的 mutator **必须**第一句就 `sync_from_disk_if_untouched()`,
并被 F189 那条机械完备性测试 `every_mutating_entry_point_reloads_before_it_writes`
覆盖 —— 那条测试是列举式门控在加档时必然漏的第五道保险。

**已实现(2026-09-08)**:`add_project` / `update_project` / `set_project_nodes` /
`delete_project`,外加 `delete`(删会话)里摘节点那一段。完备性测试的下界
随之 18→22。`touch_project_accessed` 留给 F224 —— 它的触发时机(跃迁)
是那一片的判据,不在本片。

`update_project`/`set_project_nodes` **先校验、通过了才写**:半途改一半再报错
的话,用户看到「保存失败」而配置已经变了一部分。

**守护(13 条,全部经变异验证)**:`a_blank_tmux_name_falls_back_to_the_project_name`
(变异:回退改成别的 → 变红)、`two_projects_whose_names_sanitize_to_the_same_tmux_name_clash`、
`a_project_clashes_with_a_tmux_name_a_session_spells_out`、
`a_session_name_that_merely_derives_the_same_tmux_name_is_not_checked`(钉住那个
有意的缺口)、`the_preferred_node_must_be_one_of_the_listed_nodes`、
`an_sftp_session_cannot_be_a_project_node`、`project_toml_round_trips`、
`unset_optional_fields_are_not_written_out`、`a_v9_file_without_projects_reads_as_an_empty_table`、
`deleting_a_session_unlists_it_but_never_deletes_the_project`、
`a_rejected_update_leaves_the_stored_project_untouched`、
`a_new_project_is_readable_and_ids_start_at_one`、
`migrate::tests::current_schema_is_ten`。

---

## 五、F222 同机指纹核对

判据是 `known_hosts.toml` 的 `SHA256:` 指纹(F3-a 起键含端口)。**三态**:

| 状态 | 条件 | 处置 |
|---|---|---|
| 同机 | 两条会话都有条目且指纹相同 | 放行 |
| 异机 | 两条都有条目且指纹不同 | **拒绝加入**,提示「这两条会话指向不同的机器」 |
| 待核 | 至少一条没有条目(没连过) | **放行**,该节点标「待核」 |

**待核的事后核对**:该节点真连上、握手拿到指纹那一刻,与项目里已有节点比对。
不一致 → 弹一次告警 + 该节点在项目编辑界面标红。**不自动踢出** —— 服务器
重装换 key、指纹表被清这类情形下,自动踢会把用户的配置无声改掉。

判定本身是 `mullion-store` 的纯函数,**可纯单测**。取指纹与弹告警在 app 侧。

**签名**(已实现 2026-09-08):`can_join(existing: &[String], candidate: &str,
table: &KnownHostsFile) -> SameMachine`。收的是**已拼好的 `known_hosts` 键**
而不是 host+port —— 拼键的 `host_key_id` 在 `mullion-ssh` 里,store 不能依赖它
(架构不变量)。同一条判定既用于「候选加入」也用于「待核节点事后核对」
(后者把刚握手拿到的指纹先记进表,再拿它当 candidate 跑一遍)。

**守护(8 条,全部经变异验证)**:三态各一条;
`the_same_machine_reached_on_two_ports_is_still_the_same_machine`(与「按 host 串
判」结论相反,是 P2 决策的自证;变异:判据换成「两个键相等」→ 变红);
`an_empty_table_degrades_everything_to_pending_not_different`;
`conflicting_with_any_existing_node_is_enough_to_reject`(变异:对上一个就放行 → 变红);
`an_unverified_existing_node_does_not_mask_a_real_conflict`;
`a_candidate_with_no_verified_peer_to_compare_against_is_still_pending`
—— **最后这条是变异验证挖出来的**:函数尾巴那条分支原先没有任何测试走得到
(空表那条在函数头就早退了),把尾巴改成无条件 `Same` 全绿。那是伪阳性的
安全结论:一个都没比对过却报「已核实同机」,UI 上待核标记也不会出现。

---

## 六、F223 打开项目 = 带参数的换节点

入口三处(见 F225)最终都汇到同一条路:复用 `spawn_rehost_on`(`app.rs:7718`)
的断开/重连骨架连到 `preferred`,对这次连接的 `ResolvedAutomation` 做
**一次性覆盖**。

### ⚠ 不能原样用 rehost 的 plan 来源(复核挖出,写错就白干)

`spawn_rehost_on` 现在拿 `pending_for_extra_pane` 生成计划(`app.rs:7752`),
而那条**故意跳过 tmux**(`automation.rs:104-107`:「只有建标签的那个 pane
全套跑,其余跳过 tmux」—— 防两块 pane attach 同一 session 内容镜像)。
照抄的话:打开项目会 cd 到目录、但**永远不 attach 项目 tmux** —— P5 静默
落空、灯永远不亮、访问时间永远不记,且客户端零报错。

项目打开必须走**含 tmux 的全套 plan**(`pending_for` 那一族)。原「防镜像」
的理由在这里不成立 —— 项目 tmux 是**另一个** session,不是本标签当初 attach
的那个;「同一项目开两块 pane」的镜像风险由 F224 的 attach 前核对兜住。

**守护**:项目打开产出的 steps 里必须含 `tmux ... attach`(变异:换回
`pending_for_extra_pane` → 当场变红)。

### 覆盖内容

```
tmux      := TmuxChoice::Attach { session_name: Some(project_tmux_name(p)) }   // 强制
work_dir  := p.dir
```

**一次性,不写回会话记录**(同 F122 标签覆盖不落盘的姿态)。

被覆盖的痕迹:该会话原本是 `Some(TmuxChoice::Off)` 时,pane 标题条/状态栏
留一条可见提示。否则用户会以为自己的「不用 tmux」设置坏了。

### 文件面板

覆盖 `SftpPrefs.default_remote`,面板落到 `p.dir`。同样是运行期覆盖,不污染配置。
**项目是更具体的上下文,覆盖更泛的默认值**。

### 目录只在 tmux 会话新建时生效 —— 这条必须让用户看见

`automation.rs:277` 的 `-c` 只挂在 `new-session` 上(注释:「附着已有会话时
改它的工作目录是越权」)。后果:项目第一次在 `/srv/app` 建了 tmux 会话,之后
把目录改成 `/srv/app2`,**下次打开毫无变化** —— 走的是 `has-session` 命中 →
`attach`,`-c` 那一支根本没执行。日志、测试、界面全都正常。

处置两条,**都要**:

1. **attach 补 `-c <dir>`**(首次 attach 与断线重连 `build_plan_reattach`
   **两条路径都补** —— 只补前者的话,重连回来之后新开的 window 又落回旧目录)。
   核实过 tmux 3.7b man:
   > `-c` will set the session working directory (used for new windows) to working-directory.

   即改的是 session 的默认工作目录,**只影响此后新开的 window/pane**,不动
   已经在跑的那个 shell。语义正确、成本近零。
2. **项目编辑界面写明**「已存在的 tmux 会话不会移动,需先在远端结束它」;
   改目录保存时提示一次。

单靠 1 会造成更隐蔽的困惑(新 window 在新目录、原来那个在旧目录,用户看到
「有时生效有时不生效」)。

### 断开确认:有条件

按 P5,项目一律走 tmux,所以断开 pane **不丢远端工作** —— 那正是 tmux 的意义。
真正会丢东西的只有三种,**只有这三种才拦**:

1. 内置编辑器里有未保存的改动
2. 有 SFTP 传输在途
3. 当前 pane **确定不在 tmux 里**(裸 shell,断开是真丢)

第 3 条的判据是 `RemoteState { title_seen: true, tmux: None }`。
**`title_seen == false` 不算** —— 那是「还没收到上报」,不是「没有 tmux」。
拿 `tmux.is_none()` 单独当判据的话,链路慢时确认框会乱弹。

否掉「总是确认」:这是个日常高频动作(切活),高频路径上的无谓确认,用户
三天就学会闭眼点「确定」,那时它对真正危险的那几种也失效了。

**守护**:三条拦截条件各一条 + 「`title_seen == false` 时不拦」(变异:
判据改成裸 `tmux.is_none()` → 变红);覆盖注入后 `build_plan` 产出的
tmux 名/工作目录是项目的那份;「会话配了 `Off` 也照样走 attach 分支」。

---

## 七、F224 运行指示灯与访问时间

### 统一判据(P7)

`RemoteState` 现成就是三态,不用自己造:

| `title_seen` | `tmux` | 含义 |
|---|---|---|
| `false` | — | **未知**(上报还没到) |
| `true` | `Some(n)` | 在 tmux 里,名字是 `n`,**权威** |
| `true` | `None` | **确定**不在 tmux 里 |

「项目 X 正在运行」= 存在某个 pane,其上报 `tmux == Some(project_tmux_name(X))`。

这条判据自动覆盖了用户**不通过项目入口**、直接连会话 attach 进那个 tmux 会话
的情况 —— 不需要两套记账。

### 灯是三态,UI 不许用「灭」冒充「未知」

- **亮**:有 pane 上报命中
- **灭**:本地所有实例的 pane **都已上报**(`title_seen`)且无一命中
- **未知**:存在还没上报的 pane(它可能正 attach 在这个项目里)

「灭」**故意不要求远端核对过** —— 核对只在 attach 前那一刻发生,要求它的话
launcher 列表里「灭」永远不可达,三态实际退化成两态。「灭」的已知盲区
(非 Mullion 的 client,如 PowerShell 直接 ssh 上去 attach)由 attach 前的
远端核对兜底 —— 那才是产生后果的时刻。

灯的图形走 `ui::icon` 自绘,**不用字符**(T9:非 ASCII 符号要么进
`ui::glyphs::VERIFIED` 白名单、要么自绘,且判据是 GBK 内)。

### 本地层:实例心跳

新文件 `<config_dir>/projects/<实例id>.alive`,**另开一份**,不塞进 F148 的
`layouts/<实例id>.toml` —— 那个文件的写入时机围绕「现场恢复」设计,塞一个
每 15 秒刷新的高频字段进去会让两套东西的生命周期纠缠。

复用 F148 的全部纯函数与常量:`new_instance_id`、`HEARTBEAT_INTERVAL_SECS`(15)、
`ALIVE_GRACE_SECS`(45)、`is_alive`。文件内容是**这个实例各 pane 当前上报的
tmux 名的集合**(不是「我打开了哪些项目」)。

沿用 F148 那条结构性保证:**每个进程只写自己那个文件,从不改别人的**。
陈旧文件也照抄 F148 的处置:**启动时删心跳已过期的**(误判后果有界 ——
多删 = 灯短暂少亮一处,少删 = 目录里多几个文件,下次启动再删)。漏掉这条
的话文件永久堆积,且判活要遍历的文件越来越多。

### 远端层:attach 前二次核对

真要 attach 那一刻(而不是列表里)跑 `tmux list-clients -t <name>`。有人在 →
弹确认:「项目 X 已在别处打开(N 个客户端),继续会把对方踢下线」,
**默认按钮是取消**。

**`-d` 由核对结果驱动**(复核挖出:确认框承诺「踢下线」,而
`tmux_command(a, name, false)` 的首次 attach 根本不踢 —— 用户点了继续,
两个 client 照样同挂,`window-size latest` 互相 resize 打架,恰是这盏灯
要防的事):

- 核对无人 → attach **不带** `-d`(与现状一致)
- 核对有人、用户确认 → attach **带** `-d`,承诺与行为一致
- 用户取消 → 整次打开撤销,一个字节都没发(此时也**不记**访问时间)

**与「恰好一个 Step」不变量的关系**:核对走**独立的 exec channel**,不进 PTY。
时序是「PTY 就绪(干净 shell 停着)→ exec 跑 list-clients → (必要时)等用户
确认 → 才 `write_scheduled` 发那一步」。PTY 在等待期间没人往里写,窗口期
仍然干净,不变量不破。等待用户确认可以任意长 —— 那只是一个 bash 提示符
在远端闲着。

**冲突时以远端为准** —— 本地心跳看不见「你从 PowerShell 直接 ssh 上去 attach」
和「另一台 Windows 上的 Mullion」。

### 访问时间

**跃迁触发,不是电平触发**(复核挖出):判据从「不命中」变为「命中」的那一刻
记一笔。RemoteState 的上报是持续的,「命中就更新」照字面做等于**每几秒写一次
`sessions.toml`** —— T-b 的教训原话:「播报判据是跃迁不是当前状态」。
pane 断开再接回、或从项目 A 的 tmux 切到项目 B,都是新的跃迁,各记各的。

「非 `Completed` 不记」(等首字节超时 / 用户接管 / 断线,T11)被这条判据
**自动蕴含** —— 那些结局下 attach 压根没发生,命中上报永远到不了。不必
单独接线,但守护测试要有一条钉住这个推论。

否掉「点选项目那一刻就记」:列表会被失败的尝试污染(网络不通、凭据过期、
按错了马上取消全都算「最近访问过」),而用户查这个列表是想找**上次真正在干的活**。

**守护**:三态判据各一条(变异:`title_seen` 从判据里拿掉 → 变红);
「从普通会话 attach 进项目 tmux 也更新时间」(这条是 P7 的自证,
只钉项目入口那条路的话它恒绿);「连续两批命中上报只记一笔」(变异:
跃迁判据改成电平 → 变红 —— 这条是防「每几秒写一次盘」的唯一闸);
「超时/接管的结局不产生命中上报」(钉住上面那条蕴含关系);
「核对有人且确认后的 attach 带 `-d`,无人则不带」;
心跳判活复用 F148 纯函数(不重新实现一份阈值)。

---

## 八、F225 三处入口

### ① launcher 页项目列表(主入口)

**事实**:launcher 态的中央区**现在完全没有 egui 内容** —— 终端是 GPU 自绘,
egui 只画菜单栏/标签栏/状态栏/侧栏,全项目没有一处业务 `CentralPanel` 铺在
中央区。所以这是**往空白区新建**,不是重排既有布局。

内容:项目列表,按 `last_accessed_at` **倒序**(无访问记录的排最后),每行带
运行指示灯 + 项目名 + 目录 + 首选节点名。点一行直接打开。

为什么它是主入口:开机后想干的第一件事是**回到昨天那个活**,不是「连服务器」。
项目列表 + 灯恰好回答开机第一个问题:「哪个活还在跑?」

表单/间距一律照 `docs/ui-form-guidelines.md`,机械守护在
`crates/mullion-app/tests/form_guidelines.rs`。

### ② 菜单「会话 → 项目管理器」(编辑入口)

与「会话管理器」「分组管理器」并列 —— 三者是同一类东西(配置库里的持久实体)。
**不放「配置」菜单**:那里装的是本机偏好(标题条开关、文件面板、日志、设置),
混进去会把两类东西的边界糊掉。

**必须同时登记进 `app.rs::modal_open` 的 `Modal` 枚举和 `touched_store`**:

- 进 `Modal`:里面有文本输入框(项目名/目录/tmux 名),不登记的话敲的字会
  同时漏给远端 shell(T8)
- 进 `touched_store`:它写 `sessions.toml`

这条本项目踩过(切片 I:「弹窗要同时进两张表」)。`Modal` 完备性表要改两处
(F148 的教训)。

管理器里双击项目也能打开(A 作为 B 的补充)。

### ③ pane 标题条第三个按钮

`TitleAction` 加 `pick_project: Option<PaneId>`,弹窗复用 `ui/rehost.rs` 的形态
(按 pane 定位、搜索框 + 列表 + 取消,每行带灯)。语义是「把手上这块 pane 切到
另一个活」。

**不合并进换节点弹窗**:底层机制虽然共享,但用户心智是两件事 ——「换一条路线到
同一台机器」vs「切到另一个活」。合并成 tab 会让每次普通换节点都先撞见一个
不相干的 tab,把一个已实机验收过的功能搅浑。

**不做「点标题文字弹出」**:没有可发现性,且标题文字现在是 F123 的远端状态
展示区,点击语义和展示语义会打架。

### 标题条文字

「这块 pane 属于项目 X」**不是存储的状态,是推导的**:判据与 F224 同一条
(上报 tmux 名 == `project_tmux_name(X)`)。刚打开、上报还没到时显示 F123
现状,到了再切成项目样式 —— 别为标题条单独记一份「pane→项目」映射,
那份映射在用户手动 detach / 换 tmux 之后没人清(F160~F163 的「意图表
换节点没人清」同形)。

有项目的 pane 优先显示 `项目名 · 目录名`,顶掉现在的 tmux 名 —— 按 P5,
项目的 tmux 名就是从项目名推导的,两个一起显示是同一个信息说两遍。
无项目的 pane 保持 F123/F124 现状不变。

`·`(U+00B7)在 GBK 内(A1A4),但**仍要登记进 `ui::glyphs::VERIFIED`**,
否则 `tests/glyph_whitelist.rs` 会拦下(T9)。

---

## 九、实施顺序

1. **F221** 数据模型 + 校验 + schema v10 + Vault mutator(纯 store,可纯单测)
2. **F222** 同机指纹三态(纯函数在 store,接线在 app)
3. **F225②** 项目管理器弹窗(要有地方建项目,后面才验得动)
4. **F223** 打开项目 = 带参换节点 + 条件确认
5. **F224** 灯与访问时间(判据 + 心跳 + 远端核对)
6. **F225①③** launcher 列表 + 标题条按钮

3 排在 4 前面是因为没有项目就没法验打开;1、2 全在 store,不碰 GPU,最快。

---

## 十、验不了的部分

按项目约定提前声明,**不会自己编结论**:

- launcher 项目列表的观感与信息密度
- 标题条第三个按钮在**窄分屏**下挤不挤(只有人眼能判;不预先设计一套自适应
  隐藏 —— 那会引入「按钮会消失」这个自己的坑)
- 灯的三态在真实高延迟链路上的延迟体感(上报要多久才到、「未知」态停留多长)
- `tmux list-clients` 在高延迟代理链路上的往返成本
- 「已在别处打开」确认框在真实多开场景下是否真的拦住了误踢

这些全部进人工验收清单,随 Windows Release 一起交付实测。
