//! App:winit ApplicationHandler<UserEvent>。持有窗口/GPU/文字层/运行时,以及一列
//! 标签(F36:空 = launcher 态 / 非空 = 终端态,§2.2;每个标签一棵布局树,一棵树
//! 可装多个 pane)。每帧(有活动标签时)对每个 pane「排空 rx → feed emu → 回写
//! PtyWrite(T1)」,GPU present 受帧率(T3)与同步块(T2)双闸。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use mullion_core::layout::PaneId;
use mullion_ssh::config::SshConfig;
use mullion_ssh::known_hosts::HostKeyPolicy;
use mullion_ssh::session::{SshConnection, SshSession};
use mullion_store::known_hosts::KnownHostsFile;
use mullion_store::SessionId;
use mullion_term::keymap::{Key, WheelAction};
use mullion_term::Scroll;
use tokio::runtime::Runtime;
use tokio::sync::mpsc::Receiver;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

use crate::frame::{FrameLimiter, RedrawAction};
use crate::gpu::{quads_for_panes, Gpu};
use crate::render::SyncFramePacer;
use crate::shell::tabs::{Tab, TabPayload, Tabs};
use crate::shell::workspace::{PaneGeom, PaneState, Preset, Workspace};
use crate::text::TextLayer;
use crate::theme::{self, MULLION_DARK};
use crate::{diag, input, shell};

/// app 与「连接建立」异步任务之间的事件(ssh io_task / connect 的 wake、结果经此回送)。
/// 携带 `SshSession`/`Receiver` 等非 `Copy` 负载,故不能派生 Copy/Clone;两者也未实现
/// `Debug`,故 `UserEvent` 同样不派生 Debug(winit `ApplicationHandler<T>` 只要求 `T: 'static`)。
pub enum UserEvent {
    Wake,
    /// 异步 connect 成功:**已建立连接**本身的 `Handle`(F35:同一条连接上后续
    /// 分屏另开 channel 要复用它)。`Arc` 是因为 russh 的 `Handle` 没实现
    /// `Clone`,只有 `Drop`(释放即断连)。
    ///
    /// D1/F50:`wants_sftp` 是点击那一刻(`spawn_connect` 调用点)就算好的 ——
    /// 这个事件本身不带 `SessionId`,没法在这里回头再查一次协议字段。为 `true`
    /// 时 `pty` 恒 `None`(SFTP 节点不开 PTY,`spawn_connect` 内部直接跳过
    /// `open_pty`);为 `false` 时 `pty` 恒 `Some`(`open_pty` 失败会走
    /// `ConnectErr`,不会发一个「两者皆无」的 `ConnectOk`)。
    ConnectOk {
        handle: Arc<SshConnection>,
        wants_sftp: bool,
        pty: Option<(SshSession, Receiver<Vec<u8>>)>,
    },
    /// 异步 connect 失败,已格式化的可操作错误(F6 分类由 `session::connect` 内部给)。
    ConnectErr(String),
    /// 私钥文件对话框结束。`None` = 用户取消/对话框失败——也要回送,否则
    /// `key_picker_busy` 永远清不掉,以后再点「选择…」就没反应了。
    KeyPathPicked(Option<PathBuf>),
    /// F74:凭据表单里那次私钥选择的结果。与 `KeyPathPicked` **分开**——
    /// 正文要写进哪个缓冲(会话的还是凭据的),只有事件变体分得清。
    CredentialKeyPathPicked(Option<PathBuf>),
    /// F61:「导入 .ico…」的结果。`None` = 用户取消 / 对话框起不来。
    IconPathPicked(Option<PathBuf>),
    /// F2:「导入 ssh config…」选中的文件。`None` = 用户取消。
    SshConfigPicked(Option<PathBuf>),
    /// 主机密钥需要用户确认(F3)。握手线程正挂在 `reply` 上等回答,
    /// **必须**最终发一个 bool 回去或丢弃 sender(丢弃 = 拒绝,fail-closed)。
    /// `Box` 是因为 `HostKeyPrompt` 比其余变体大得多,不装箱会撑大整个枚举。
    HostKeyPrompt(Box<crate::host_key::HostKeyPrompt>),
    /// 分屏(F82→F30)多出来的 pane 在同一条连接上另开的 channel 开好了(F35)。
    PaneOpened {
        id: PaneId,
        ssh: SshSession,
        rx: Receiver<Vec<u8>>,
        /// C1:发出这个异步任务时,`Workspace::generation()` 是多少。开 channel
        /// 是真实网络往返,可能在用户断开又重连(=新 `Workspace`,`next_id`
        /// 重新从 2 计数)之后才回来——这时 `id` 在新世代的树上完全可能被
        /// **复用**,只查 id/树成员会误判为"还需要",顶掉新世代刚建好的
        /// `PaneState`。世代号是唯一能分辨"这事件到底是哪一代发出的"的信息。
        generation: u64,
    },
    /// 分屏 channel 开失败。树上的叶子位留着,标题条显示错误,用户可以再切布局。
    PaneOpenErr {
        id: PaneId,
        msg: String,
        /// C1:同 `PaneOpened::generation`——旧世代的失败提示落到新世代头上,
        /// 会给用户弹一条跟当前连接毫不相干的错误 toast,必须按世代过滤。
        generation: u64,
    },
    /// 换节点(用户报的问题 2):到新机器的连接建好、PTY 也开好了。
    ///
    /// **跟 `ConnectOk` 分开**:那条事件的语义是「开一个新标签」,这条是
    /// 「把已有的一块 pane 改挂过去」。挤在一起只能靠运行时标志判别,而两者
    /// 走错了的后果并不一样(前者多一个标签,后者顶掉一块正在用的 pane)。
    PaneRehosted {
        /// 发起时属主标签的世代号。拨号是真实网络往返,期间用户可能已经把
        /// 这块 pane 关掉、切了预设、甚至断开重连 —— 判据同 `PaneOpened`。
        generation: u64,
        pane: PaneId,
        /// 新机器那条连接。**必须整条持有**(Drop 即断连),挂进 `ws.hosts`。
        handle: Arc<SshConnection>,
        ssh: SshSession,
        rx: Receiver<Vec<u8>>,
    },
    /// 换节点失败。这块 pane 原样留在旧机器上 —— 用户可以再试一次。
    PaneRehostErr {
        generation: u64,
        pane: PaneId,
        msg: String,
    },
    /// F92:一次拨测成功。`u64` 是发起时的世代号,过期的直接丢。
    ProbeOk(u64),
    /// F92:一次拨测失败(含超时)。
    ProbeErr(u64, String),
    /// F40~F44:一次自动化结束。`u64` 是发起时的 `Workspace` 世代号,
    /// 过期的直接丢(同 `PaneOpenErr::generation`)。
    ///
    /// **必须带 `PaneId`**:一个标签现在可以同时跑多份自动化(每块 pane 一份,
    /// 分屏新开的那些跳过 tmux)。只按世代找的话,先结束的那份会把还在跑的
    /// 别份 handle 一起清掉——状态栏的「进行中」提前消失,`user_took_over`
    /// 也再取消不了它们。
    AutomationDone(u64, PaneId, crate::automation::Outcome),
    /// F111/F114:某条隧道的监管任务报了一次状态。**不带世代号** ——
    /// 隧道不属于 `Workspace`(ADR-010:它有自己的连接),重连一次终端不会
    /// 让 `TunnelId` 被复用;「还在不在运行时表里」就是唯一需要的过期判据
    /// (见 `tunnels::TunnelRuntime::set_state`)。
    TunnelState {
        id: mullion_store::TunnelId,
        state: mullion_ssh::tunnel::TunnelState,
    },
    /// F50/D6:侧栏的 sftp channel 开好了(或者没开成)——蹭会话已建立的连接
    /// (`SftpClient::open` 签名里没有网络参数),登录目录已 `canonicalize(".")`
    /// 过。`generation` 是 S1 路由键:按它找属主标签,不用活动标签接
    /// (用户在标签 A 开侧栏、切到标签 B 的几百毫秒里这条抵达,拿活动标签接
    /// 就会把 A 的 client 挂到 B 上)。`Err` 已经是格式化好的中文原因,
    /// 不是 `SftpError` 的 Debug。
    ///
    /// F123:成功时三样一起回来:`(client, 登录目录, 这次要打开的目录)`。
    ///
    /// **登录目录与起始目录是两回事**:前者恒等于 `canonicalize(".")`,用来
    /// 展开标题里报的 `~/...`(F123);后者是本次要 list 的那个目录,可能来自
    /// pane 的 cwd、F120 的配置值,或者就等于登录目录。合成一个字段的话
    /// 「侧栏关→开跃迁」那条路会拿着用户浏览到的目录去当 home 用。
    SftpOpened {
        generation: u64,
        result: Result<
            (
                Arc<mullion_ssh::sftp::SftpClient>,
                mullion_ssh::sftp::RemotePath,
                mullion_ssh::sftp::RemotePath,
            ),
            String,
        >,
    },
    /// F50:一次列目录的结果。`seq` 与 `PaneState::request_seq` 对齐,对不上
    /// 就丢(用户点得比网络快时的后发先至)。`generation` 同上,S1 路由键。
    SftpListed {
        generation: u64,
        seq: u64,
        result: Result<Vec<mullion_ssh::sftp::Entry>, String>,
    },
    /// D2/F54:一次远端写操作跑完了。`Ok(())` = 成功,`Err` = 已经格式化好的
    /// 可读原因。**按世代路由**(S1):用户在一次网络往返期间切了标签,结果
    /// 也要回到发起它的那个标签,不是当前活动标签。
    ///
    /// 成功之后由接收方发起一次刷新 —— 写操作不带回新的目录内容,不刷新的话
    /// 界面上那个文件「还在」,用户会以为删除没生效然后再删一次。
    SftpOpDone {
        generation: u64,
        result: Result<(), String>,
    },
    /// F52:一批传输展开完了(目录已经递归成一条条文件级 job),可以入队。
    /// **展开在后台做**:远端目录要走网络列目录,本地目录要走磁盘遍历,
    /// 都不能压在窗口线程上。`Err` = 展开阶段就失败(目录读不了 / 落点名在
    /// Windows 上非法),这时**一条 job 都不建** —— 传一半再报错比不传更糟。
    TransferPlanned {
        generation: u64,
        result: Result<Vec<PlannedJob>, String>,
    },
    /// F59:一条传输的进度。**高频**(一个 100MB 的文件几千条)——
    /// 接它的地方绝不能每条都请求重绘(T3),只更队列数据,重绘交给帧闸。
    TransferProgress {
        job: u64,
        done: u64,
    },
    /// F55:一条传输收工。`Err` 里的冲突标记会被队列翻译成
    /// `JobState::Conflict`(见 `crate::files::queue::JobError`),其余
    /// 都是已经格式化好的中文原因。
    TransferDone {
        job: u64,
        result: Result<(), String>,
    },
    /// F53:要编辑的那个远端文件读回来了。`result` 是「内容 + 读到它那一刻的
    /// 远端戳」——戳是回传前判冲突的唯一依据(D3-8),必须跟内容同一次往返
    /// 里取,事后补一次 `stat` 拿到的是**别人可能已经改过**之后的值。
    ///
    /// 按世代路由(S1):读一个几十 MB 的文件够用户切好几次标签。
    EditOpened {
        generation: u64,
        kind: crate::edit::sessions::EditKind,
        remote: mullion_ssh::sftp::RemotePath,
        result: Result<(Vec<u8>, crate::edit::sessions::RemoteStamp), String>,
    },
    /// F53:看门任务报的一次本地文件状态。**只在与上次不同时才发** ——
    /// 每秒一条空事件会把事件循环从 `Wait` 里薅起来,白烧 CPU(T3)。
    /// `None` = 临时文件不见了(用户自己删了 / 编辑器换了 inode)。
    ///
    /// 「第一次看到只登记基线、不算改动」这条规则**不在任务里**,在
    /// `EditSessions::changed_locally` 里 —— 它有单测,任务里再判一遍就成了
    /// 两份判据。
    EditTick {
        key: u64,
        stamp: Option<crate::edit::sessions::LocalStamp>,
    },
    /// F53:一次回传的结果。**不带世代号**:`key` 全局唯一且不回收,
    /// 属主标签关掉时 `drain_generation` 会把条目一并收走,「还在不在表里」
    /// 就是唯一需要的过期判据(同隧道那条)。
    EditSaved {
        key: u64,
        result: Result<EditWriteOutcome, String>,
    },
}

/// F53:一次回传到底发生了什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditWriteOutcome {
    /// 写成了,附回传后远端的新戳(下一次比对的基准)。
    Done(crate::edit::sessions::RemoteStamp),
    /// 远端在我们编辑期间被改过 —— **什么都没写**,附远端当前的戳。
    Conflict(crate::edit::sessions::RemoteStamp),
}

/// 展开后的一条传输:两端完整路径和大小都算好了,入队即可跑。
#[derive(Clone, Debug)]
pub struct PlannedJob {
    dir: crate::files::queue::Direction,
    /// 本地端完整路径(上传时是源,下载时是落点)。
    local: std::path::PathBuf,
    /// 远端完整路径(上传时是落点,下载时是源)。
    remote: mullion_ssh::sftp::RemotePath,
    total: u64,
    label: String,
}

/// 一条传输的全部输入。**冲突处置后重跑读的是同一份** —— 重新算一遍的话
/// 「用户当时选的是哪个文件」和「现在光标在哪」会对不上。
#[derive(Clone)]
struct TransferSpec {
    dir: crate::files::queue::Direction,
    /// S1:属主标签的世代。worker 起跑时按它找连接。
    generation: u64,
    local: std::path::PathBuf,
    remote: mullion_ssh::sftp::RemotePath,
}

/// 一次在途自动化的把手。三条通道都是 `Option`,因为每一条都是**一次性边**:
/// `take()` 天然保证不会重复触发,也省掉一个「是否已触发」的布尔标志。
struct AutomationHandle {
    /// 只认这一个 pane 的首字节。
    ///
    /// 一个标签可以同时挂**多份**(`TerminalTab::automation` 是 `Vec`):每块
    /// pane 各跑各的。原先钉死 `PaneId(1)`(总设计 §7 前提②)的理由是「所有
    /// pane attach 同一个 tmux session 会内容镜像,且 `window-size` 取 `latest`
    /// 会反复 reflow、取 `smallest` 会留白」——那个顾虑现在由
    /// `automation::pending_for_extra_pane`**跳过 tmux 步骤**来化解,而不再靠
    /// 「除第一个 pane 外什么都不跑」。用户配的 `cd` / `export` / 启动命令对每块
    /// 新 shell 都仍然成立,不跑才是错的(用户报的问题:分屏出来的 pane 没执行
    /// 登录后命令)。
    pane: PaneId,
    ready: Option<tokio::sync::oneshot::Sender<()>>,
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
    disconnect: Option<tokio::sync::oneshot::Sender<()>>,
    /// 换新连接时 abort:旧那次的结论对新连接没有意义。
    task: tokio::task::JoinHandle<()>,
}

/// 一次在途的换节点。真正换挂要等 `PaneRehosted` 抵达,而那条事件带不动
/// 这些东西 —— 标题条要的名字/地址、外观要的 `SessionId`、以及新节点的登录后
/// 命令,全都得在**用户选中那一帧**算好存下来。
///
/// 理由同 `pending_automation_template`:拨号是真实网络往返(高延迟代理链路
/// 下几百 ms 到几秒),这期间用户完全可能去会话管理器把这条会话改了甚至删了,
/// 到那时再回头查库,发出去的字节就跟他选中时看到的对不上。
struct PendingRehost {
    generation: u64,
    pane: PaneId,
    /// 标题条上显示的名字(会话名)。
    label: String,
    /// `host:port`,标题条副标题用。
    addr: String,
    session_id: mullion_store::SessionId,
    /// 新节点的登录后命令。**跳过 tmux**——用户拍板的规则,与分屏新开的
    /// pane 一致(见 `automation::pending_for_extra_pane`):多块 pane attach
    /// 同一个 tmux session 会内容镜像。`None` = 这个节点没配自动化。
    plan: Option<crate::automation::PendingAutomation>,
}

/// 一个终端标签的全部 **per-connection** 状态(D0 决策 S2)。
///
/// 这五项过去都平摊在 `App` 上,单标签时看不出问题;多标签时它们会串味 ——
/// 在标签 A 跑着自动化,去标签 B 连一个新会话,`ConnectOk` 里那句
/// 「abort 上一次的自动化」就会把 A 的掐了。判据是「它属于这条连接还是属于
/// 这个进程」:属于连接的进这里,属于进程的(在途连接、隧道、store、外观缓存)
/// 留在 `App`。
struct TerminalTab {
    ws: Workspace,
    /// 当前生效的布局预设(预设按钮组画选中态用)。手动关 pane 之后置 `None`
    /// (布局不再对应任何预设)。
    current_preset: Option<Preset>,
    /// 这个标签是用什么配置连上的。`open_pty`(F35 分屏复用连接)要它的
    /// `term`/`cols`/`rows`,标题条要 `user`/`host`/`port`。
    ///
    /// **按标签存而不是按 App 存**:否则在标签 A 上分屏,会拿标签 B(最近一次
    /// 连接)的 term/尺寸去开 channel。
    last_cfg: Option<SshConfig>,
    /// F40~F44:这个标签上**正在跑的每一份**自动化,每块 pane 至多一份。
    /// 空表 = 没在跑。
    ///
    /// 从 `Option` 改成 `Vec` 是因为分屏新开的 pane 也要跑登录后命令
    /// (见 `AutomationHandle::pane`)。跑完/被取消的那一份由
    /// `accept_automation_done` 按 `PaneId` 摘掉,稳态下长度 ≤ 分屏数。
    automation: Vec<AutomationHandle>,
    /// F40~F44:这条连接的自动化配置**快照**,给后来的 pane 用(分屏新开的、
    /// 换过节点的)。`None` = 这条连接不跑自动化(没配 / 关了 / 用户右键
    /// 「跳过一次」)。
    ///
    /// **在 `ConnectOk` 那一刻定死,之后绝不回头查库**——与
    /// `automation::PendingAutomation` 同一个理由:连上之后用户完全可能改了
    /// 配置甚至删了会话,那时候分屏发出去的字节就跟他当初点「连接」时看到的
    /// 配置对不上了。
    automation_template: Option<mullion_store::ResolvedAutomation>,
    /// 上一次自动化的结论文案。一直显示到这个标签被替换/关闭 —— 不做定时淡出:
    /// 状态栏本来就是常驻信息区,而定时清除要再引一个 deadline 进帧循环,
    /// 正是 spec §1 修订一要避免的东西。
    automation_status: Option<String>,
    /// F50:这个标签自己的侧栏两栏运行态(设计 D1:侧栏「按会话记住」)。
    ///
    /// **不能挂在 `App` 上**——那是 Task 9 的权宜实现:那时远端栏恒
    /// `Load::Idle`,全局一份看不出问题。一旦接上真实数据,不同标签连着
    /// 不同主机,共享一份 `remote` 状态就是标签 B 的侧栏显示标签 A 主机
    /// 目录的 bug,用户看不出异常,直到对着错误的主机操作。
    files: crate::ui::files_panel::PanelFrame,
    /// F50/D6:这个标签的 sftp channel。`None` = 还没开,或者上次没开成
    /// (`accept_sftp_opened` 收到 `Err` 时不写这个字段,留着 `None` 让下次
    /// 用户点击时能重试)。蹭 `ws.hosts[0].handle` 已建立的连接开,不重新握手。
    sftp: Option<Arc<mullion_ssh::sftp::SftpClient>>,
    /// F50/D6:这个标签在途的 sftp 后台任务(`spawn_sftp_open`/
    /// `spawn_sftp_list_dir` 各开一个,句柄经调用方存回这里)。**必须在
    /// `wind_down` 里一并 abort**——理由和上面 `automation` 那条一模一样:
    /// 每个任务经 `Arc<SftpClient>`(内部 `_conn: Arc<SshConnection>`,见其
    /// 文档)持有一份连接保活引用,只 drop `TerminalTab`(=只 drop
    /// `t.sftp`/`t.ws`)收不了口——用户在一次网络往返期间关掉标签,task 手里
    /// 那份 Arc 会撑住底层连接直到这次 RPC 自然结束。`list_dir`/
    /// `canonicalize` 好歹有 russh-sftp 默认 10s 请求超时兜底,但
    /// `SftpClient::open` 内部裸的 `channel_open_session`/`request_subsystem`
    /// 完全没有超时包裹,链路黑洞时可能无限期挂着(见 `spawn_sftp_open` 的
    /// 文档)——这正是本项目高延迟代理链路那个头号场景,`wind_down` 是唯一的
    /// 收口点。
    ///
    /// `Vec` 不是单个 `Option`:一次侧栏交互可能同时有不止一条在途请求(open
    /// 接着首次 list、或者用户连点了几个目录)。每次 `push` 前先 `retain`
    /// 掉已经跑完的,稳态下不会无界增长。**新请求不会中途 abort 旧请求**——
    /// `russh-sftp` 的请求/应答是按请求 id 配对的,没有把握验证「正等着应答
    /// 时被 abort」是否会让它的内部状态错乱,不冒这个险;旧请求靠 `seq` 校验
    /// 在结果层面被丢弃(`PaneState::accept`),不靠中途取消。
    sftp_tasks: Vec<tokio::task::JoinHandle<()>>,
    /// F120:这个标签对应会话在编辑器「SFTP」分节里配置的默认远端目录。
    /// `None` = 没配置,`trigger_sftp_open` 落回登录目录(`.`)。`ConnectOk`
    /// 建标签时从 `store` 读一次存进来,之后不再变——跟 `last_cfg` 同理,
    /// 不随会话记录后续被编辑而变化(那是下一次连接才会生效的东西)。
    sftp_default_remote: Option<String>,
    /// F123:这条 sftp 连接的**真登录目录**(`canonicalize(".")` 的结果)。
    /// `None` = sftp 还没开好。用来把标题里报的 `~/Mullion` 展开成绝对路径
    /// (`sftp-server` 不展开 `~`)。
    ///
    /// **不是「面板当前目录」**:那个在 `files.remote.cwd` 里,会随用户浏览
    /// 移动;这个在同一次 sftp 连接内不变(重连会被新连接的值覆盖)。
    sftp_home: Option<mullion_ssh::sftp::RemotePath>,
}

/// D1/D6:一个「SFTP 节点」标签的全部状态——**独占**自己的连接(跟隧道同一个
/// 理由,ADR-010:establish 一条新的,不蹭会话那条)。没有 `ws`,所以没有
/// PTY、没有自动化、没有分屏——这些概念对这种标签不存在。
struct FilesTab {
    /// 面板运行态(两栏)。字段名故意跟 `TerminalTab::files` 保持一致 ——
    /// `TabContent::files_panel`/`files_panel_mut` 两个变体各取自己那份,
    /// 靠的就是字段名对得上,少一次「这个方法到底该读哪个字段」的心智负担。
    files: crate::ui::files_panel::PanelFrame,
    /// 这个标签独占的连接。`establish` 单独建的一条,不是会话侧栏那种蹭
    /// `ws.hosts[0]` 的连接(D6)。
    conn: Arc<SshConnection>,
    /// S1 路由键。没有 `ws.generation()` 可用——号段来自
    /// `App::next_ws_generation`,与 Terminal 标签共用同一个计数器,保证全局
    /// 唯一(不会跟任何标签的世代号撞)。
    generation: u64,
    /// F50/D6:sftp channel。`None` = 还没开好(标签一开出来就会
    /// `trigger_sftp_open` 起一次;开失败保留 `None` 让用户能重试)。
    sftp: Option<Arc<mullion_ssh::sftp::SftpClient>>,
    /// 同 `TerminalTab::sftp_tasks`——收口纪律一模一样,见该字段文档。
    sftp_tasks: Vec<tokio::task::JoinHandle<()>>,
    /// F120:同 `TerminalTab::sftp_default_remote`,文档见那边。
    sftp_default_remote: Option<String>,
    /// F123:同 `TerminalTab::sftp_home`,文档见那边。
    sftp_home: Option<mullion_ssh::sftp::RemotePath>,
}

/// F37:从 `layout.toml` 恢复出来的**占位标签** —— 一条连接都没建。
///
/// 上次关窗时开着的标签,这次启动只摆回骨架(设计 §1 拍板:恢复骨架 + 手动
/// 重连)。它没有 `Workspace`、没有 sftp channel、没有自动化,所以
/// `wind_down` 对它无事可做 —— 但那条 match **仍必须给它一条具名分支**,
/// 见 `wind_down` 的文档。
///
/// 用户按「重连」后走 `spawn_connect`(与会话管理器双击同一条路径),
/// `ConnectOk` 抵达时**就地**把这个标签替换成真的 `Terminal`/`Files`。
struct RestoredTab {
    /// 上次这个标签连的是哪条会话。重连时拿它去 `ssh_config_for`。
    session_id: SessionId,
    /// 上次的分屏树(扁平前序编码,见 `mullion_store::layout`)。重连成功后
    /// 交给 `Workspace::apply_saved_tree` 摆回形状。
    tree: Vec<mullion_store::SavedNodeEntry>,
    /// 上次的焦点落在第几个叶子(前序序号)。
    focus_leaf: usize,
    /// S1 路由键。号段同 `FilesTab::generation` —— 来自
    /// `App::next_ws_generation`,与所有标签共用一个计数器保证全局唯一。
    ///
    /// **占位标签也要有世代号**:它是 `Tabs<TabContent>` 的成员,
    /// `TabPayload::generation` 是全表遍历比对的,给它一个撞号的值(比如恒 0)
    /// 会让迟到事件路由到错误的标签上。
    generation: u64,
    /// 上次它是个 SFTP 节点标签(`SavedTabKind::Files`)。重连时据此决定
    /// 建 `Files` 标签还是 `Terminal` 标签。
    wants_sftp: bool,
    /// 已经点过重连、正在拨号。按钮据此禁用(见 `ui::restored`)。
    dialing: bool,
}

/// F37:一次「占位标签重连」在途期间要记住的东西(E9)。
struct PendingRestore {
    /// 连上之后**就地替换**的是这个标签,不是「当前活动标签」——
    /// 拨号要几百毫秒,期间用户完全可能切到别的标签去。
    tab_id: shell::tabs::TabId,
    /// 上次的分屏树(扁平前序编码)。
    tree: Vec<mullion_store::SavedNodeEntry>,
    /// 上次焦点落在第几个叶子。
    focus_leaf: usize,
}

/// 标签装的东西。
///
/// `Terminal` 装箱(`Box<TerminalTab>`)是 clippy `large_enum_variant` 逼的:
/// `TerminalTab` 比 `FilesTab` 大出一大截(前者挂着整棵 `Workspace`),两个
/// 变体不装箱会让整个枚举按最大变体算大小,`Files` 那份也要陪绑一份从不用
/// 到的填充。字段访问(`t.ws`/`t.files`/…)不受影响——`.` 运算符自动穿透
/// `Box` 的 `Deref`。
enum TabContent {
    Terminal(Box<TerminalTab>),
    /// D1:SFTP 节点连接后开的独占标签(F50/F120)。装箱理由同上——
    /// `FilesTab` 比裸大小差一截,两个变体都不装箱时枚举按最大的算,
    /// 只装一个又会让另一个变成新的「最大」,clippy 还是会响。
    Files(Box<FilesTab>),
    /// F37:恢复出来的占位标签。**不装箱** —— `RestoredTab` 只有几个标量加
    /// 一个 `Vec`,比另外两个变体小得多,装箱只会多一次堆分配。
    Restored(RestoredTab),
}

impl TabPayload for TabContent {
    fn generation(&self) -> u64 {
        match self {
            TabContent::Terminal(t) => t.ws.generation(),
            TabContent::Files(f) => f.generation,
            TabContent::Restored(r) => r.generation,
        }
    }
}

impl TabContent {
    fn as_terminal(&self) -> Option<&TerminalTab> {
        match self {
            TabContent::Terminal(t) => Some(t.as_ref()),
            TabContent::Files(_) | TabContent::Restored(_) => None,
        }
    }

    fn as_terminal_mut(&mut self) -> Option<&mut TerminalTab> {
        match self {
            TabContent::Terminal(t) => Some(t.as_mut()),
            TabContent::Files(_) | TabContent::Restored(_) => None,
        }
    }

    /// D1:连着的两个变体各有一份面板运行态。
    ///
    /// F37 起要 `Option` —— 占位标签(`Restored`)连都没连,给它一份面板
    /// 运行态就是凭空造一个「远端目录」出来。**不给它一个空 `PanelFrame`
    /// 兜底**:那样的话「对着没连接的标签发起了一次文件操作」会静默变成
    /// 「对着一份空状态操作」,现象是点了没反应,没有任何地方报错。
    fn files_panel(&self) -> Option<&crate::ui::files_panel::PanelFrame> {
        match self {
            TabContent::Terminal(t) => Some(&t.files),
            TabContent::Files(f) => Some(&f.files),
            TabContent::Restored(_) => None,
        }
    }

    fn files_panel_mut(&mut self) -> Option<&mut crate::ui::files_panel::PanelFrame> {
        match self {
            TabContent::Terminal(t) => Some(&mut t.files),
            TabContent::Files(f) => Some(&mut f.files),
            TabContent::Restored(_) => None,
        }
    }

    /// 两个变体都有一份 sftp client 槽位;读时克隆(`Arc`,廉价)。
    fn sftp_client(&self) -> Option<Arc<mullion_ssh::sftp::SftpClient>> {
        match self {
            TabContent::Terminal(t) => t.sftp.clone(),
            TabContent::Files(f) => f.sftp.clone(),
            TabContent::Restored(_) => None,
        }
    }

    /// `None` = 占位标签(F37),它没有连接,也就没有 sftp 槽位可写。
    fn sftp_mut(&mut self) -> Option<&mut Option<Arc<mullion_ssh::sftp::SftpClient>>> {
        match self {
            TabContent::Terminal(t) => Some(&mut t.sftp),
            TabContent::Files(f) => Some(&mut f.sftp),
            TabContent::Restored(_) => None,
        }
    }

    /// `None` = 占位标签(F37):没有连接就没有在途任务,也就没有要收口的东西。
    fn sftp_tasks_mut(&mut self) -> Option<&mut Vec<tokio::task::JoinHandle<()>>> {
        match self {
            TabContent::Terminal(t) => Some(&mut t.sftp_tasks),
            TabContent::Files(f) => Some(&mut f.sftp_tasks),
            TabContent::Restored(_) => None,
        }
    }

    /// F120:这个标签配置的默认远端目录(`None` = 没配置,落回登录目录)。
    /// 读时克隆——两个变体都只是 `Option<String>`,没必要为一次读取拆出借用。
    fn sftp_default_remote(&self) -> Option<String> {
        match self {
            TabContent::Terminal(t) => t.sftp_default_remote.clone(),
            TabContent::Files(f) => f.sftp_default_remote.clone(),
            TabContent::Restored(_) => None,
        }
    }

    /// F123:这个标签 sftp 的登录目录。占位标签恒 `None`。
    fn sftp_home(&self) -> Option<Vec<u8>> {
        match self {
            TabContent::Terminal(t) => t.sftp_home.as_ref().map(|p| p.as_bytes().to_vec()),
            TabContent::Files(f) => f.sftp_home.as_ref().map(|p| p.as_bytes().to_vec()),
            TabContent::Restored(_) => None,
        }
    }

    /// 同上,写入。占位标签没有 sftp,静默忽略。
    fn set_sftp_home(&mut self, home: mullion_ssh::sftp::RemotePath) {
        match self {
            TabContent::Terminal(t) => t.sftp_home = Some(home),
            TabContent::Files(f) => f.sftp_home = Some(home),
            TabContent::Restored(_) => {}
        }
    }

    /// ②:这个标签焦点 pane 报出来的当前目录。SFTP 节点标签/占位标签没有
    /// 终端,恒 `None`。
    fn focused_pane_cwd(&self) -> Option<Vec<u8>> {
        self.as_terminal()
            .and_then(|t| t.ws.focused())
            .and_then(|p| p.cwd.clone())
    }

    /// D6:这个标签的 sftp 该蹭哪条连接。`Terminal` 蹭会话已建立的连接
    /// (`ws.hosts.first()`,ADR-009 下今天恒为其一);`Files` 独占自己的
    /// (`establish` 来的那条,ADR-010 同款理由)。`Terminal` 分支理论上可能是
    /// `None`(连接尚未真正建立完成的极短窗口;测试脚手架也会构造出空
    /// `hosts` 的 `Workspace`)。
    fn sftp_connection(&self) -> Option<Arc<SshConnection>> {
        match self {
            TabContent::Terminal(t) => t.ws.hosts.first().map(|h| h.handle.clone()),
            TabContent::Files(f) => Some(f.conn.clone()),
            TabContent::Restored(_) => None,
        }
    }
}

/// F18 拖拽出界的自动滚动量。判据是**焦点 pane 的终端区**,不是整个窗口。
///
/// 窗口边界不能用:终端区上沿之上还有菜单栏 + 标签栏(几十像素),下沿之下还有
/// 状态栏/传输面板。指针拉到那些地方时窗口坐标仍是正数、仍小于窗口高,
/// `autoscroll_lines` 一行都不滚 —— 用户必须把鼠标拖出**整个窗口**才跨得了屏,
/// 而正常终端是拖到内容区顶端就开始滚。分屏后每块 pane 的上下沿还各不相同,
/// 只有按焦点那块算才对得上。
///
/// 换算与 `cursor_in_grid`/`selection_cursor` 同源(都减 `term_px` 的原点):
/// 两处不同源的话,选区终点落在这一格、却按另一套边界决定滚不滚。
fn autoscroll_for_pane(cursor_px_y: f32, term: shell::workspace::PxRect, cell_h: f32) -> i32 {
    input::autoscroll_lines(cursor_px_y - term.y as f32, term.h as f32, cell_h)
}

/// 输入法候选框该占的物理像素矩形 `(x, y, w, h)`:焦点 pane 里光标那一格。
///
/// 传窗口原点的话候选窗永远飘在窗口左上角 —— 打中文时得低头找候选。夹紧是
/// 因为 resize 的中间态里光标行列可能短暂超出当前几何,不夹会把候选框推到
/// 邻居 pane / 状态栏上。
fn ime_cursor_area(
    term: shell::workspace::PxRect,
    cursor: (u16, u16),
    cell_w: f32,
    cell_h: f32,
) -> (u32, u32, u32, u32) {
    let w = (cell_w.max(1.0)) as u32;
    let h = (cell_h.max(1.0)) as u32;
    // 至少留一格:`w`/`h` 比 pane 还大时 saturating_sub 会得 0,夹出个空矩形。
    let max_x = term.w.saturating_sub(w);
    let max_y = term.h.saturating_sub(h);
    let x = (u32::from(cursor.0) * w).min(max_x);
    let y = (u32::from(cursor.1) * h).min(max_y);
    (term.x + x, term.y + y, w, h)
}

/// 活动标签的 workspace。
///
/// 写成**自由函数而不是 `App` 的方法**是被借用检查器逼的:`App` 的方法借的是
/// 整个 `self`,而事件循环里有好几处要同时拿 `self.active`(GPU)/`self.mods` /
/// `self.ui` 和焦点 pane。原先 `self.ws.as_mut()` 是字段级借用,天然不冲突;
/// 换成方法就会整片飘红。参数收窄到 `&mut Tabs` 就还原了那份粒度。
fn active_term_of(tabs: &Tabs<TabContent>) -> Option<&TerminalTab> {
    tabs.active().and_then(|t| t.content.as_terminal())
}

fn active_ws_of(tabs: &Tabs<TabContent>) -> Option<&Workspace> {
    active_term_of(tabs).map(|t| &t.ws)
}

fn active_ws_mut_of(tabs: &mut Tabs<TabContent>) -> Option<&mut Workspace> {
    tabs.active_mut()
        .and_then(|t| t.content.as_terminal_mut())
        .map(|t| &mut t.ws)
}

/// `App::active_is_files_tab` 的纯逻辑核心。抽成自由函数是为了能在无头测试
/// 容器里拿真实构造的 `Tabs<TabContent>` 单测——理由与上面几个 `_of` 函数
/// 一样:`App` 本身需要一个 `EventLoopProxy`,测试里造不出来。
fn active_is_files_tab_of(tabs: &Tabs<TabContent>) -> bool {
    tabs.active()
        .is_some_and(|t| matches!(t.content, TabContent::Files(_)))
}

/// `App::files_owner_generation` 的纯逻辑核心,理由同上。
fn files_owner_generation_of(tabs: &Tabs<TabContent>, sidebar_open: bool) -> Option<u64> {
    if active_is_files_tab_of(tabs) {
        tabs.active().map(|t| t.content.generation())
    } else if sidebar_open {
        active_ws_of(tabs).map(Workspace::generation)
    } else {
        None
    }
}

/// `App::effective_focus` 的纯逻辑核心,理由同上。三条分支里前两条(活动标签
/// 是 Terminal、侧栏开/关)能用真实构造的 `TerminalTab` 单测;第三条(活动
/// 标签是 Files)测不到——`FilesTab::conn` 是 `Arc<SshConnection>`,
/// `SshConnection::new` 对 `mullion-app` 不可见(`pub(crate)` 到
/// `mullion-ssh`),测试里造不出真的 `FilesTab`(与 `wind_down_has_no_catch_all_arm_`
/// 那组测试面对的限制一样)——那条分支靠结构自检测试钉住
/// (`effective_focus_treats_a_files_tab_as_always_focused_on_the_panel`)。
fn effective_focus_of(
    tabs: &Tabs<TabContent>,
    sidebar_open: bool,
    focus: shell::input_route::Focus,
) -> shell::input_route::Focus {
    use crate::shell::input_route::Focus;
    if active_is_files_tab_of(tabs) {
        Focus::FilesPanel
    } else if sidebar_open {
        focus
    } else {
        Focus::Terminal
    }
}

/// `App::move_panel_selection` 的下标数学核心。抽成不依赖 `App`/`Tabs` 的
/// 自由函数,理由同上面几个 `_of` 函数——这段是这次改动里唯一有算法复杂度
/// 的部分(代码复核 #3),此前只有靠 generation 路由的结构守护测试,边界
/// 情况(空列表/单条/首行再 `↑`/末行再 `↓`/选中项已不在当前行里)一次
/// 都没直接测过。
///
/// `rows`:当前显示的那些条目(已经过滤/排序过,顺序即用户看到的顺序)。
/// `selected`:当前选中项的身份(`PaneState::selected` 存的是身份不是下标,
/// 见该字段文档——过滤/排序一变下标就跟着错位,所以这里也按身份找,不按
/// 下标找;选中项已经不在 `rows` 里时,`position` 找不到,按未选中处理)。
/// `delta`:方向,`< 0` 是 `↑`、`> 0` 是 `↓`。
///
/// 返回下一个选中项在 `rows` 里的下标;`rows` 为空时没有"选第几个"这个
/// 概念,返回 `None`(空列表若走进 `Some(rows.len() - 1)` 那支会直接下溢)。
/// 逐条删。目录走递归删除(F57:先 exec 后回退),文件与链接走 remove。
///
/// **一条失败就停**,并把已经删掉的条数报进错误里:继续删下去的话,用户
/// 看到一条「权限不足」却不知道前面几条到底删没删,而这一步不可逆。
async fn delete_all(
    client: &Arc<mullion_ssh::sftp::SftpClient>,
    conn: Option<&Arc<SshConnection>>,
    targets: &[(mullion_ssh::sftp::RemotePath, bool)],
) -> Result<(), String> {
    for (ix, (path, is_dir)) in targets.iter().enumerate() {
        let r = if *is_dir {
            match conn {
                // 递归删除要 exec 快路径,而 exec 要连接句柄。拿不到就
                // 退化成纯 SFTP 的 rmdir —— 只删得掉空目录,但总比不给删好。
                Some(c) => mullion_ssh::remove_tree::remove_tree(client, c, path)
                    .await
                    .map(|_| ()),
                None => client.remove_dir(path).await,
            }
        } else {
            // **链接走 remove_file** —— SFTP 的 REMOVE 删的是链接本身,
            // 不跟随(设计 D17)。
            client.remove_file(path).await
        };
        if let Err(e) = r {
            return Err(format!(
                "删除 {} 失败:{e}(前面 {ix} 条已删除)",
                path.display()
            ));
        }
    }
    Ok(())
}

/// F52:把「用户点中的一批条目」摊成**文件级** job。目录要递归展开:远端走
/// `list_dir`,本地走 `walk_dir`。两侧都**不跟随符号链接**(设计 D17,理由同
/// `remove_tree`:跟随会跑出这棵树,把用户没看见的东西也传走)。
///
/// 上传时顺手把远端那边需要的子目录建出来 —— 递归上传的第一条 job 落在
/// `a/b/c.txt` 上时 `a/b` 还不存在,`open_write` 会直接失败。放在这里而不是
/// worker 里:worker 是并发跑的,几条同时 mkdir 同一个目录纯属互相踩。
async fn plan_transfer(
    client: &Arc<mullion_ssh::sftp::SftpClient>,
    dir: crate::files::queue::Direction,
    picked: &[(mullion_ssh::sftp::RemotePath, bool, u64)],
    remote_cwd: &mullion_ssh::sftp::RemotePath,
    local_cwd: &mullion_ssh::sftp::RemotePath,
) -> Result<Vec<PlannedJob>, String> {
    use crate::files::queue::Direction;
    let mut out = Vec::new();
    for (name, is_dir, size) in picked {
        match dir {
            Direction::Download => {
                let remote = remote_cwd.join(name.as_bytes());
                if *is_dir {
                    plan_download_dir(client, &remote, &mut out, local_cwd).await?;
                } else {
                    out.push(download_job(&remote, &[], local_cwd, *size)?);
                }
            }
            Direction::Upload => {
                let local = crate::files::local::to_path(&crate::files::local::join_local(
                    local_cwd,
                    name.as_bytes(),
                ));
                if *is_dir {
                    for w in crate::files::local::walk_dir(&local)? {
                        out.push(upload_job(&local, &w.rel, remote_cwd, name, w.size));
                    }
                } else {
                    out.push(upload_job(&local, &[], remote_cwd, name, *size));
                }
            }
        }
    }
    if dir == Direction::Upload {
        // `BTreeSet` 的字节序天然把祖先排在后代前面(`/a` < `/a/b`),
        // 照这个顺序建就不会「先建孙子再建儿子」。已存在的错误吞掉:
        // 目标目录本来就在是最常见的情况,不是失败。
        let dirs: std::collections::BTreeSet<mullion_ssh::sftp::RemotePath> =
            out.iter().map(|j| j.remote.parent()).collect();
        for d in dirs {
            let _ = client.create_dir(&d).await;
        }
    }
    Ok(out)
}

/// 一条下载 job:远端绝对路径(+ 相对段)→ 本地落点。
///
/// D16:落点名在 Windows 上非法就**整条拒掉并给建议名**,不静默改写 ——
/// 用户以为传下来的是 `aux.log`,实际是 `_aux.log`,下次照着原名找不到。
fn download_job(
    remote_root: &mullion_ssh::sftp::RemotePath,
    rel: &[Vec<u8>],
    local_dir: &mullion_ssh::sftp::RemotePath,
    size: u64,
) -> Result<PlannedJob, String> {
    let mut remote = remote_root.clone();
    let mut local = crate::files::local::to_path(local_dir);
    let root_name = last_segment(remote_root);
    check_windows_name(&root_name)?;
    local.push(&root_name);
    for seg in rel {
        remote = remote.join(seg);
        let s = String::from_utf8_lossy(seg).into_owned();
        check_windows_name(&s)?;
        local.push(&s);
    }
    let label = local
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(PlannedJob {
        dir: crate::files::queue::Direction::Download,
        local,
        remote,
        total: size,
        label,
    })
}

fn check_windows_name(name: &str) -> Result<(), String> {
    match crate::files::transfer::illegal_on_windows(name) {
        None => Ok(()),
        Some(sug) => Err(format!(
            "「{name}」在 Windows 上不是合法文件名,传下来会失败。\
             先在远端改成「{sug}」这类名字再传。"
        )),
    }
}

/// 路径的最后一段。空路径 / 根返回整串,不 panic。
fn last_segment(p: &mullion_ssh::sftp::RemotePath) -> String {
    let b = p.as_bytes();
    let seg = b.rsplit(|c| *c == b'/').next().unwrap_or(b);
    String::from_utf8_lossy(seg).into_owned()
}

/// 一条上传 job:本地路径(+ 相对段)→ 远端落点。
///
/// 不像 `download_job` 那样要校验名字 —— 目标是 POSIX 端,本地能存在的名字
/// 在那边基本都合法(反过来才是问题,那条由 `download_job` 挡)。
fn upload_job(
    local_root: &std::path::Path,
    rel: &[String],
    remote_dir: &mullion_ssh::sftp::RemotePath,
    root_name: &mullion_ssh::sftp::RemotePath,
    size: u64,
) -> PlannedJob {
    let mut local = local_root.to_path_buf();
    let mut remote = remote_dir.join(root_name.as_bytes());
    for seg in rel {
        local.push(seg);
        remote = remote.join(seg.as_bytes());
    }
    let label = rel
        .last()
        .cloned()
        .unwrap_or_else(|| root_name.display().into_owned());
    PlannedJob {
        dir: crate::files::queue::Direction::Upload,
        local,
        remote,
        total: size,
        label,
    }
}

/// 远端目录递归展开。**不跟随符号链接**(D17)。
///
/// 手写栈而不是递归 `async fn` —— 后者要 `Box::pin` 兜生命周期,写出来更长
/// 也更容易出错,而这里的递归结构简单到用栈表达就够。
async fn plan_download_dir(
    client: &Arc<mullion_ssh::sftp::SftpClient>,
    root: &mullion_ssh::sftp::RemotePath,
    out: &mut Vec<PlannedJob>,
    local_dir: &mullion_ssh::sftp::RemotePath,
) -> Result<(), String> {
    let mut stack: Vec<Vec<Vec<u8>>> = vec![Vec::new()];
    while let Some(cur) = stack.pop() {
        let mut dir = root.clone();
        for seg in &cur {
            dir = dir.join(seg);
        }
        let entries = client.list_dir(&dir).await.map_err(|e| e.to_string())?;
        for e in entries {
            // 链接和 socket/fifo 之类一律跳过:前者是 D17,后者根本没有
            // 「内容」可传,传下来只会得到一个 0 字节的普通文件。
            if e.kind == mullion_ssh::sftp::EntryKind::Symlink
                || e.kind == mullion_ssh::sftp::EntryKind::Other
                || !e.name.is_operable()
            {
                continue;
            }
            let mut next = cur.clone();
            next.push(e.name.as_bytes().to_vec());
            if e.kind == mullion_ssh::sftp::EntryKind::Dir {
                stack.push(next);
            } else {
                out.push(download_job(root, &next, local_dir, e.size)?);
            }
        }
    }
    Ok(())
}

/// 跑一条传输。**自己开一条 sftp channel**(见 `App::pump_transfers` 的注释)。
///
/// 冲突(目标已存在)且还没处置过时返回 `JobError::Conflict`,由队列翻译成
/// `JobState::Conflict` 去问用户 —— worker 因此是无状态的:不持有任何等用户
/// 回答的通道,被取消/被丢弃都不会留下悬着的东西。
async fn run_transfer(
    conn: Arc<SshConnection>,
    spec: TransferSpec,
    resolved: Option<crate::files::queue::Conflict>,
    job: u64,
    proxy: &EventLoopProxy<UserEvent>,
    cancel: &Arc<std::sync::atomic::AtomicBool>,
) -> Result<(), String> {
    use crate::files::queue::{Conflict, Direction, JobError};
    use crate::files::transfer::{dedup_name, staging_name};

    let client = mullion_ssh::sftp::SftpClient::open(conn)
        .await
        .map_err(|e| e.to_string())?;

    let mut dst_local = spec.local.clone();
    let mut dst_remote = spec.remote.clone();
    let exists = match spec.dir {
        Direction::Download => dst_local.exists(),
        Direction::Upload => client
            .exists(&dst_remote)
            .await
            .map_err(|e| e.to_string())?,
    };
    if exists {
        match resolved {
            // 还没问过用户 —— 交回队列去问。**绝不静默覆盖**(F55)。
            None => return Err(JobError::Conflict.into()),
            Some(Conflict::Skip) => return Ok(()),
            Some(Conflict::Overwrite) => {}
            Some(Conflict::Rename) => match spec.dir {
                Direction::Download => {
                    let parent = dst_local
                        .parent()
                        .unwrap_or(std::path::Path::new("."))
                        .to_path_buf();
                    let base = dst_local
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    dst_local = parent.join(dedup_name(&base, |c| parent.join(c).exists()));
                }
                Direction::Upload => {
                    // 远端查重:**列一次目录**而不是一个候选名一次 `exists`
                    // ——后者在高延迟链路上是 N 个 RTT,而且 `dedup_name` 的
                    // 探测闭包是同步的,压根塞不进 `await`。
                    let parent = dst_remote.parent();
                    let taken: std::collections::BTreeSet<Vec<u8>> = client
                        .list_dir(&parent)
                        .await
                        .map_err(|e| e.to_string())?
                        .into_iter()
                        .map(|e| e.name.as_bytes().to_vec())
                        .collect();
                    let base = last_segment(&dst_remote);
                    let name = dedup_name(&base, |c| taken.contains(c.as_bytes()));
                    dst_remote = parent.join(name.as_bytes());
                }
            },
        }
    }

    // D19:新建走 `.part` 再改名(传到一半断线不会留下一个看着像成品的残件);
    // 覆盖则直接写目标(保住 inode / 权限 / 硬链接 —— 先删再建会全丢)。
    let overwriting = exists && resolved == Some(Conflict::Overwrite);
    let mut done: u64 = 0;
    let mut buf = vec![0u8; 64 * 1024];
    match spec.dir {
        Direction::Download => {
            let final_name = dst_local
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let staging = dst_local.with_file_name(staging_name(&final_name, overwriting));
            if let Some(parent) = staging.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("建不了本地目录:{e}"))?;
            }
            let mut src = client
                .open_read(&spec.remote)
                .await
                .map_err(|e| e.to_string())?;
            {
                use std::io::Write;
                let mut f =
                    std::fs::File::create(&staging).map_err(|e| format!("写不了本地文件:{e}"))?;
                loop {
                    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                        let _ = std::fs::remove_file(&staging);
                        return Err("已取消".into());
                    }
                    let n = src.read_chunk(&mut buf).await.map_err(|e| e.to_string())?;
                    if n == 0 {
                        break;
                    }
                    f.write_all(&buf[..n])
                        .map_err(|e| format!("写不了本地文件:{e}"))?;
                    done += n as u64;
                    let _ = proxy.send_event(UserEvent::TransferProgress { job, done });
                }
                f.flush().map_err(|e| format!("写不了本地文件:{e}"))?;
            }
            if staging != dst_local {
                std::fs::rename(&staging, &dst_local).map_err(|e| format!("改名失败:{e}"))?;
            }
        }
        Direction::Upload => {
            use std::io::Read;
            let final_name = last_segment(&dst_remote);
            let staging = dst_remote
                .parent()
                .join(staging_name(&final_name, overwriting).as_bytes());
            let mut f =
                std::fs::File::open(&spec.local).map_err(|e| format!("读不了本地文件:{e}"))?;
            let mut dst = client
                .open_write(&staging, true)
                .await
                .map_err(|e| e.to_string())?;
            loop {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    // 先把 channel 收干净再删残件 —— 文件还开着时删,某些
                    // sshd 上会报「file still open」。
                    let _ = dst.finish().await;
                    let _ = client.remove_file(&staging).await;
                    return Err("已取消".into());
                }
                let n = f
                    .read(&mut buf)
                    .map_err(|e| format!("读不了本地文件:{e}"))?;
                if n == 0 {
                    break;
                }
                dst.write_chunk(&buf[..n])
                    .await
                    .map_err(|e| e.to_string())?;
                done += n as u64;
                let _ = proxy.send_event(UserEvent::TransferProgress { job, done });
            }
            // `finish()` 不能省:`File` 的 Drop 走的是 `close_nowait`,紧接着
            // 改名会撞上「文件还开着」(见 `RemoteFile::finish` 的文档)。
            dst.finish().await.map_err(|e| e.to_string())?;
            if staging != dst_remote {
                client
                    .rename(&staging, &dst_remote)
                    .await
                    .map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}

fn next_panel_selection_index(
    rows: &[&mullion_ssh::sftp::Entry],
    selected: Option<&mullion_ssh::sftp::RemotePath>,
    delta: i32,
) -> Option<usize> {
    if rows.is_empty() {
        return None;
    }
    let cur = selected.and_then(|name| rows.iter().position(|e| &e.name == name));
    Some(match cur {
        None if delta > 0 => 0,
        None => rows.len() - 1,
        Some(i) => (i as i32 + delta).clamp(0, rows.len() as i32 - 1) as usize,
    })
}

/// `App::snapshot_layout` 的纯逻辑核心:标签栏 → 磁盘格式,外加活动标签的
/// **过滤后**序号。
///
/// 写成自由函数的理由同 `active_ws_of` 那几个:`App` 要一个 `EventLoopProxy`
/// 才能构造,留在方法里的话「哪些标签该跳过」「跳过之后 active 该指哪儿」
/// 这两条真正的判据就只能靠源码结构那种弱断言守着。
fn snapshot_tabs_of(tabs: &Tabs<TabContent>) -> (Vec<mullion_store::SavedTab>, usize) {
    use crate::shell::layout_snapshot as snap;
    use mullion_store::{SavedNodeEntry, SavedTab, SavedTabKind};
    let mut out = Vec::new();
    let mut active_tab = 0usize;
    for (ix, tab) in tabs.iter().enumerate() {
        let saved = match (&tab.content, tab.session_id) {
            (TabContent::Terminal(t), Some(session_id)) => SavedTab {
                kind: SavedTabKind::Terminal,
                session_id,
                title: tab.title.clone(),
                focus_leaf: snap::focus_leaf_index(t.ws.tree(), t.ws.focus()),
                tree: snap::to_entries(t.ws.tree()),
            },
            // D1:SFTP 节点标签没有分屏树 —— 恒一个叶子。
            (TabContent::Files(_), Some(session_id)) => SavedTab {
                kind: SavedTabKind::Files,
                session_id,
                title: tab.title.clone(),
                focus_leaf: 0,
                tree: vec![SavedNodeEntry::leaf()],
            },
            // 占位标签按原样写回去:用户这次没重连它,不代表他想把它丢掉 ——
            // 悄悄丢掉的话,关一次窗口就永久少一个标签。
            (TabContent::Restored(r), _) => SavedTab {
                kind: if r.wants_sftp {
                    SavedTabKind::Files
                } else {
                    SavedTabKind::Terminal
                },
                session_id: r.session_id,
                title: tab.title.clone(),
                focus_leaf: r.focus_leaf,
                tree: r.tree.clone(),
            },
            // 快速连接(命令行 `user@host`):没有会话记录可查,记下来只会
            // 给出一个点了必然失败的「重连」(设计 E2/E6)。
            (TabContent::Terminal(_) | TabContent::Files(_), None) => continue,
        };
        if ix == tabs.active_index() {
            // 前面可能已经跳过了几个快速连接标签,所以取的是**已写进 `out`
            // 的条数**,不是 `ix` —— 用 `ix` 的话,跳过一个之后活动标签就
            // 整体错位一格,恢复时打开的是旁边那个。
            active_tab = out.len();
        }
        out.push(saved);
    }
    (out, active_tab)
}

/// E2:`ConnectOk` 抵达时新标签该叫什么名字。**优先取会话名**,空名字
/// (或压根没有会话记录,如快速连接)退回 `user@host`;两者都没有
/// (理论上不可达,兜底)退回「远端」。
///
/// 抽成纯函数是因为原地写在 `ConnectOk` 分支里没法脱离事件循环单测 ——
/// 这条优先级过去是缺的:标题恒取 `user@host`,完全没看过会话名,
/// 于是标签属性弹窗把名字存进 store 之后,**下一次**重连同一条会话,
/// 新标签仍然叫 `user@host`,用户会以为刚才那次改名根本没生效。
fn tab_title(session_name: Option<&str>, user_host: Option<(&str, &str)>) -> String {
    if let Some(name) = session_name.filter(|n| !n.is_empty()) {
        return name.to_string();
    }
    if let Some((user, host)) = user_host {
        return format!("{user}@{host}");
    }
    "远端".to_string()
}

/// F37:`ConnectOk` 抵达时该**顶替第几个标签**,`None` = 开一个新的。
///
/// 抽成自由函数是因为 `App` 要 `EventLoopProxy`、单测里造不出来,而这里
/// 恰恰是本条路径上唯一有判断的地方:
/// - 不是重连(`pending` 为 `None`)→ 照旧开新标签(F36 的「连接不顶掉已有标签」);
/// - 重连、但那个占位标签在拨号途中被关掉了 → 也开新标签,**不能把连接丢掉**;
/// - 重连且标签还在 → 顶替**它指名的那个**,与「活动标签」无关(活动标签
///   压根不是入参)。
fn replace_target(
    pending: Option<shell::tabs::TabId>,
    ids: &[shell::tabs::TabId],
) -> Option<usize> {
    let want = pending?;
    ids.iter().position(|id| *id == want)
}

/// 关掉一个标签时的收口。**顺序是这条函数存在的全部理由**:
///
/// 自动化 task 也持有一份 `Arc<SshSession>`。只 drop 掉 `Workspace`(即 pane 那
/// 一份)的话,`SshSession` 的 `cmd_tx` 仍有活着的克隆,`io_task` 不会收口 ——
/// 用户关了标签、UI 上它已经消失,预配置的命令却还在往一条没真正断开的 channel
/// 上发,用户既看不到也拦不住。`drive_automation` 补不了这条边:标签一旦从
/// `self.tabs` 里摘掉,它就再也遍历不到了。
fn wind_down(tab: Tab<TabContent>) {
    match tab.content {
        TabContent::Terminal(t) => {
            // 每块 pane 各一份,一个都不能漏 —— 漏掉的那份会继续往一条没真正
            // 断开的 channel 上发命令(见本函数开头的说明)。
            for h in t.automation {
                h.task.abort();
            }
            // F50/D6:sftp 后台任务同理——见 `TerminalTab::sftp_tasks` 的文档。
            // 不 abort 的话,用户关标签这一刻若正巧有一次 open/list 在途,
            // 那个任务手里的 `Arc<SshConnection>` 会继续撑着底层连接,直到
            // 它自己的网络往返结束(`SftpClient::open` 那两步还完全没有
            // 超时包裹,链路黑洞时可能永远不结束)。
            for task in t.sftp_tasks {
                task.abort();
            }
            // `t.ws` 在这里 drop —— 每个 `PaneState` 随之 drop,关掉它那条 SSH channel。
        }
        // D1:文件标签没有自动化、没有 PTY——只有 sftp 后台任务需要收口,
        // 理由与上面 `Terminal` 分支那段一模一样(见 `FilesTab::sftp_tasks`
        // 的文档)。**不能漏这一臂**:落到 `_ => {}` 上会让新变体的连接静默泄漏
        // (这条 match 之所以不写成带通配符的形式,就是为了让编译器在将来再加
        // 变体时强制回来看一眼这里)。
        TabContent::Files(f) => {
            for task in f.sftp_tasks {
                task.abort();
            }
            // `f.sftp`/`f.conn` 在这里 drop —— 独占连接随之释放。
        }
        // F37:占位标签没连接、没 channel、没在途任务 —— 真的无事可收。
        // **仍然写一条具名分支**:落到通配上的话,以后给 `RestoredTab` 加了
        // 需要收口的东西(比如重连在途的那个 task 句柄),这里不会有任何
        // 编译错误提醒,连接就静默泄漏了。
        TabContent::Restored(_) => {}
    }
}

/// 窗口出现后才建的 GPU 相关状态。
struct Active {
    window: Arc<Window>,
    gpu: Gpu,
    text: TextLayer,
    /// 本帧算出的每 pane 几何。渲染、鼠标命中、window_change 三条路径读同一份 ——
    /// 各算各的是这类布局 bug 的经典成因(算出来差一个标题条高度,肉眼看不出来,
    /// 但鼠标点击整体偏 32px)。
    geoms: Vec<PaneGeom>,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
}

pub struct App {
    _runtime: Runtime,
    /// F36:一列标签。**空 = launcher 态**(无终端可画);非空 = 终端态。
    /// 焦点转移/世代路由等纯逻辑在 `shell::tabs`,这里只做接线。
    tabs: Tabs<TabContent>,
    /// C1:下一个 `Workspace` 世代号。只在 `ConnectOk`(唯一新建 `Workspace`
    /// 的地方)读取并自增——不能挂在 `Workspace` 自己身上,它每次都是全新
    /// 对象,内部生成的话新世代又从同一个值起步,等于没有世代区分。挂在
    /// `App` 上而非进程级 atomic:只有 `App` 知道"发生了一次重连",且只在
    /// winit 事件循环这一个线程上递增,不需要跨线程原子操作。
    next_ws_generation: u64,
    start: Instant,
    mods: ModifiersState,
    kitty: bool,
    active: Option<Active>,
    /// 帧率闸(T3,~60fps)。挂在 `App` 而非 `Connection`:egui 在 launcher 态
    /// (`conn=None`)也要画占位 UI,两态必须共用同一个闸,否则切换态时节流状态丢失。
    limiter: FrameLimiter,
    /// 被 `RedrawAction::Throttle` 挡住时记的到点时刻;`about_to_wait` 据此在
    /// deadline 到达后补一次 `request_redraw`,而不是靠陈旧 `WaitUntil` 忙转(T3/N3)。
    next_frame_at: Option<Instant>,
    /// F125:最后一次**键盘输入**的时刻,光标闪烁相位从它起算(打字重置相位)。
    /// 用 `Instant` 而不是 `u64`:与 `next_frame_at`/`sync_timeout_wake` 同一套时钟。
    last_input_at: Instant,
    /// F125:窗口有没有焦点。失焦时不闪(也就不需要周期唤醒),与 Windows 上
    /// 其它终端的惯例一致。
    window_focused: bool,
    /// 唤醒/连接结果回送通道(注入 `session::connect` 的 wake,以及本身发 UserEvent)。
    proxy: EventLoopProxy<UserEvent>,
    /// 已知主机指纹表(F3),对应磁盘 `known_hosts.toml`。SSH 线程只读它做判断;
    /// **写入与落盘只在 GUI 线程的意图施加点做**——store 是同步 IO,不该压在
    /// tokio 线程上,而且失败要能落进 `ui.last_error` 给用户看。
    known_hosts: Arc<Mutex<KnownHostsFile>>,
    /// 正在等用户回答的主机密钥弹窗。`Some` = 弹窗开着、SSH 握手挂起中,
    /// 同时也计入 `window_event` 里的 `modal`(T8:弹窗开着时键盘归 egui)。
    pending_host_key: Option<Box<crate::host_key::HostKeyPrompt>>,
    /// 弹窗弹出的时刻,用于展示 sshd `LoginGraceTime`(默认 120s)倒计时。
    host_key_since: Option<Instant>,
    /// CLI 直连(`mullion user@host`)携带的初始连接参数;`resumed` 里取走后即为 None。
    initial: Option<SshConfig>,
    /// 是否走 CLI 直连路径(路径①)。决定 `ConnectErr` 时是否保留 `exit(1)`(待定 F)。
    /// 仅对「初始 CLI 直连的首次连接」生效:一旦连上(`ConnectOk`)或用户主动发起了
    /// 另一次连接(会话管理器双击/点连接),就清为 `false`,进入交互态语义——
    /// 否则断线后从会话管理器连别的会话失败会把整个 GUI 一并 exit(1)(复核 #1)。
    cli_direct: bool,
    /// **在途**那一次连接用的配置。`ConnectOk` 抵达时移交给新建的标签
    /// (`TerminalTab::last_cfg`)。留在 `App` 上是因为发起连接的那一刻还没有
    /// 标签可放 —— 与 `pending_automation` 同理,一次只连一个。
    pending_cfg: Option<SshConfig>,
    /// egui UI 侧状态(菜单/状态栏/弹窗/中央区像素),与连接状态解耦(Task 4)。
    ui: crate::ui::UiState,
    /// 会话保险库(Task 6)。`resumed` 末尾打开;keyring/库打开失败时留 `None`,
    /// 会话功能优雅禁用而非 panic/exit(待定 G),错误记 `ui.last_error`。
    store: Option<crate::shell::store::SessionStore>,
    /// 窗口可见性。Windows 最小化会送 `Resized(0,0)`,此时必须整帧跳过 GPU 与
    /// grid 传播,只保留 IO 泵(见 `shell::window_state`)。
    visible: shell::window_state::Visibility,
    /// 文件对话框线程是否在跑。防止连点「选择…」开出多个对话框
    /// (Windows 上主窗被 owner 关系禁用,Linux/XDG 未必)。
    key_picker_busy: bool,
    /// 图标文件对话框是否在跑。跟 `key_picker_busy` 分开 —— 两个按钮在不同
    /// Tab 上,共用一个标志会让「刚选完私钥」把图标按钮也按不动。
    icon_picker_busy: bool,
    /// F2:ssh config 文件对话框是否在跑。同样单独一个 —— 它开在菜单栏上,
    /// 跟会话/凭据编辑器里的两个按钮互不相干。
    import_picker_busy: bool,
    /// egui 侧有内容待画(菜单展开/hover/弹窗/错误提示)。与「终端来了新字节」是
    /// 两个独立脏源,`frame::frame_is_dirty` 取并集——只看终端字节的话,远端一安静
    /// egui 的交互就被 `RedrawAction::Idle` 吞掉,菜单点不开。
    ui_dirty: bool,
    /// ②:上一帧结束时侧栏的开合状态,用来判「关→开」跃迁。
    ///
    /// **不能是 `render_frame` 调用前的帧内局部变量**:侧栏开关有两条路——
    /// 菜单(`chrome.rs`)在 `render_frame` **内部**改
    /// `self.ui.files_sidebar_open`,帧内局部变量能测出跃迁;但热键
    /// (`files_hotkey_event`,由 `window_event` 在**另一次事件回调**里调用)
    /// 在 `render_frame` 之外改这个标志——等下一次重绘跑到帧内局部变量赋值
    /// 那一行时,标志早就已经是 `true` 了,`open && !was_open` 恒假,
    /// Ctrl+Shift+B 开侧栏就永远同步不到焦点 pane 的目录。跨帧字段 + 判据
    /// 放在 `render_frame` 调用之后,两条路径才都覆盖得到。
    files_sidebar_was_open: bool,
    /// 指针最近一次的物理像素坐标。`MouseWheel` 事件本身不带坐标,鼠标上报
    /// (F17 alt screen 档)要的 (col,row) 只能靠 `CursorMoved` 记着。
    cursor_px: (f32, f32),
    /// 系统剪贴板(F18)。打不开时内部退化为 no-op(见 `crate::clipboard`)。
    clipboard: crate::clipboard::Clipboard,
    /// F37:**上一次真的写进 `layout.toml` 的那份快照**。`None` = 这个进程
    /// 还没写过。
    ///
    /// **不是一个 `layout_dirty: bool`**,这是刻意偏离设计 E7 的写法:脏标记
    /// 要在每一个改变布局的地方(开/关/切标签、分屏、关 pane、切预设、
    /// 窗口 Resized/Moved…)手工打一次点,漏掉任何一处的后果是「那种改动
    /// 从来不会被保存」,而且**没有任何测试会红** —— 这正是 `has_real_action`
    /// 踩过的那个坑(D4b)。改成「每 2 秒现算一份快照,跟上次存的比一比,
    /// 不一样才写」之后,「哪些操作算改动」这件事不再需要有人记得,
    /// 全部由 `snapshot_layout` 的取值范围决定。
    ///
    /// 代价是每 2 秒一次的快照构造(几个 `String` 克隆),对 60fps 的帧预算
    /// 可以忽略;收益是这一类漏接线的 bug 结构上不存在。
    last_saved_layout: Option<mullion_store::SavedLayout>,
    /// F37:上一次比对快照的时刻。节流窗口见 `layout_snapshot::should_flush`。
    layout_checked_at: Instant,
    /// F84/F21:当前**生效**的外观设置。`resumed` 里从 `settings.toml` 读出来,
    /// 之后由设置弹窗的预览/确定/取消改写。
    settings: mullion_store::Settings,
    /// F84:打开设置弹窗那一刻的 `settings` 副本。「取消」就是把它装回去。
    ///
    /// 不用「草稿没落地过就等于原值」那种写法:预览是**真的改** `settings`
    /// 并当场换字体的(设计 §8),原值不另存一份就找不回来了。
    settings_backup: Option<mullion_store::Settings>,
    /// F84:系统装了哪些字体族。弹窗打开时算一次 —— 枚举 fontdb 的全部 face
    /// 不能进每帧路径(陷阱 T3)。
    settings_families: Vec<crate::font_pick::FontChoice>,
    /// F21:当前字体量出来不是等宽。同 `settings_families`,只在开弹窗和换
    /// 字体之后重算。
    settings_not_mono: bool,
    /// F71:解锁框开着时,那份还没能用上的 `layout.toml` 快照。
    ///
    /// `restore_tabs` 必须在会话库打开之后跑(「这条会话还在不在库里」是丢弃
    /// 规则之一),而解锁框开着的时候库还没打开 —— 快照只能先在这儿等着。
    /// 解锁成功 / 放弃解锁时由 `finish_store_open` 取走。
    pending_layout: Option<mullion_store::SavedLayout>,
    /// F37:正在为哪个占位标签拨号,以及连上之后要摆回什么形状(E9)。
    ///
    /// **`ConnectOk` 事件本身不带 `TabId`**(它是从 tokio task 发回来的,
    /// 发起那一刻还没有标签概念),所以「这次连接是一次重连、要替换的是
    /// 哪个标签」只能在发起时记在这儿。
    ///
    /// **至多一个**:占位标签上的「重连」按钮在拨号期间是禁用的
    /// (`RestoredTab::dialing`),「全部重连」也是一个一个来 —— 高延迟代理
    /// 链路上同时拉 N 条连接正是设计 §1 否掉自动重连的理由之一。
    pending_restore: Option<PendingRestore>,
    /// 左键是否按住(划选进行中)。松开即结束,不跨 focus 保留。
    dragging: bool,
    /// 上一次左键按下的连击状态,喂 `input::click_kind` 判双击/三击。
    prev_click: Option<input::PrevClick>,
    /// 左键按下时的 0-based 锚点格与选区类型(F18)。松开时用来识别
    /// 「只是点了一下、指针从未离开这一格」——那种情况不该复制。
    press_anchor: Option<((u16, u16), mullion_term::selection::SelectionKind)>,
    /// 拖拽出界时每帧要滚的行数;0 = 不自动滚。**只在真正 present 的那一帧施加**
    /// (见 `RedrawRequested` 里的说明),否则重演 T3/T7。
    autoscroll: i32,
    /// 输入法组字状态。组字期间 winit 照样发 `KeyboardInput`(logical_key 是
    /// 拼音字母),不靠它吞掉的话打「你好」会先往远端送一串 `nihao`。
    ime: input::ImeState,
    /// 上次告诉系统输入法的候选框位置(物理像素 `(x, y, w, h)`)。
    ///
    /// 记着是为了**只在变化时**调 `set_ime_cursor_area`:那是一次跨进程的
    /// 系统调用,每帧无脑调与 T3 同一类问题(光标每闪一次就调一遍)。
    ime_cursor_area: Option<(u32, u32, u32, u32)>,
    /// 待用户确认的多行粘贴(F18)。`Some` = 弹窗开着,计入 `modal`(T8)。
    pending_paste: Option<String>,
    /// F61/F62:会话外观的解析缓存。**只在会话/分组变更后 rebuild**,
    /// 绝不在渲染里现算(陷阱 T3,见 `ui::badge::AppearanceCache`)。
    appearance: crate::ui::badge::AppearanceCache,
    /// F92 拨测世代号。切会话 / 关编辑器 / 关会话管理器时 +1,
    /// 迟到的结果据此丢弃(见 `accept_probe`)。
    probe_epoch: u64,
    /// 在途拨测任务。退出或取消时 abort —— 20 秒的 timeout 悬着不管,
    /// 关窗后进程还要多活 20 秒。
    probe_task: Option<tokio::task::JoinHandle<()>>,
    /// `spawn_connect` 算好、等 `ConnectOk` 抵达时启用的计划。
    ///
    /// 在 `spawn_connect`(用户点击那一帧)算而不是 `ConnectOk` 里算:
    /// `ConnectOk` 不携带 `SessionId`,到那时只能读 `ui.connect_request_last`,
    /// 而连接在途期间用户完全可能改了配置甚至删了这条会话。
    pending_automation: Option<crate::automation::PendingAutomation>,
    /// 同一次点击算好的自动化配置**原件**,`ConnectOk` 抵达时移交给新标签的
    /// `automation_template`,供后来的 pane(分屏 / 换节点)复用。
    ///
    /// 跟 `pending_automation` 在同一帧、由同一次 `store.resolved()` 算出,
    /// 理由一模一样:连接在途期间用户改了配置,分屏发出去的字节就跟他点
    /// 「连接」时看到的对不上了。
    pending_automation_template: Option<mullion_store::ResolvedAutomation>,
    /// F44 右键「连接(跳过自动化)」的一次性标志。`ConnectOk` 消费后立即清零。
    pending_skip_automation: bool,
    /// 换节点在途的那些。`Vec` 而不是 `Option`:两块 pane 可以同时在换
    /// (弹窗一次只开一个,但拨号是异步的,第一次还没回来就能发起第二次),
    /// 用 `Option` 的话后发的会把先发的元信息顶掉 —— 现象是换好之后标题条
    /// 上写着另一台机器的名字。按 `(generation, pane)` 取走。
    pending_rehost: Vec<PendingRehost>,
    /// F111/F114:已启动的隧道。**必须挂在 `App` 上** —— `TunnelHandle` 一
    /// Drop 就停隧道,放进临时变量等于隧道刚起来就被停掉。
    tunnels: crate::tunnels::TunnelRuntime,
    /// F6/设计 D23:用户想把键盘焦点放在哪一侧(终端 / 文件面板)。**只是意愿,
    /// 不是这一帧的真实生效值**——能否兑现取决于面板此刻在不在(见
    /// `effective_focus` 按上下文夹紧的说明)。默认终端,与迁移前行为一致。
    focus: shell::input_route::Focus,
    /// F55:**跨标签**的传输队列。挂在 `App` 上而不是标签上 —— 设计里它
    /// 是全局的一条队列,切标签不该看见另一份;标签关掉时用
    /// `Queue::cancel_generation` 作废属于它的那些 job。
    transfer_queue: crate::files::queue::Queue,
    /// 每条在跑的 job 的取消旗标。worker 每块之后看一眼 —— 取消得能在
    /// 2GB 传到一半时立刻生效,不能等整个文件传完。
    transfer_cancels: std::collections::HashMap<u64, Arc<std::sync::atomic::AtomicBool>>,
    /// 每条 job 的完整参数(见 `TransferSpec`)。job 真正走完(不是挂在冲突上)
    /// 之后删掉,不然队列清空了它还在涨。
    transfer_specs: std::collections::HashMap<u64, TransferSpec>,
    /// F53:所有还挂在监视里的编辑。跨标签一份,理由同 `transfer_queue`。
    edits: crate::edit::sessions::EditSessions,
    /// F53:内置编辑器窗口。**同一时刻只开一个** —— 多开的价值远小于
    /// 「哪个窗口对应哪个文件」带来的混乱,而这里每个窗口背后都是一次
    /// 会覆盖远端文件的写。
    editor: Option<crate::ui::editor_window::EditorState>,
    /// F53:每条编辑的看门任务(1 秒看一次本地 mtime,D3-10)。
    ///
    /// **不走事件循环的 `WaitUntil`**:那条路径是 T3/T7 的高压区(三个分支
    /// 都要显式复位 `control_flow`),为一个「一秒一次的文件 stat」去动它
    /// 得不偿失。tokio 任务经 `proxy` 回送事件,天然会唤醒事件循环。
    edit_watchers: std::collections::HashMap<u64, tokio::task::JoinHandle<()>>,
    /// F53/D3-7:打开那一刻读到的原文,用来在**第一次**回传前留一份
    /// `.mullion.bak`。回传成功后即丢 —— 之后远端那份就是我们自己写的,
    /// 再备份一次既没有意义又要多传一遍全量。
    edit_originals: std::collections::HashMap<u64, Vec<u8>>,
    /// F53:撞上冲突时远端**当时**的戳。「保留远端」要拿它刷快照(D3-9),
    /// 「覆盖远端」要拿它当新的比对基准 —— 否则覆盖那一下会再撞一次冲突。
    edit_conflicts: std::collections::HashMap<u64, crate::edit::sessions::RemoteStamp>,
    /// F53:外部编辑用的临时文件根目录。退出时整棵删掉(D3-12)。
    /// 进程启动时算一次 —— `directories` 每次调用都要摸环境变量。
    edit_root: std::path::PathBuf,
}

/// 一种会盖住主界面的模态弹窗。
///
/// **存在的唯一理由是让编译器当守护**:`modal_open` 过去是一串 `||` 列举,
/// 新增弹窗时全靠人记得补一行 —— 已经漏过三次(editor / files_dialog /
/// group_manager),后果是那个弹窗开着时用户敲的字**同时被发给远端 shell**
/// (T8)。改成枚举之后,加一个变体就必须在 `modal_open` 的 `match` 里给出
/// 「它现在开着吗」,不给就编译不过。
///
/// **加变体时要同步两处**:`ALL` 和测试里的 `VARIANT_COUNT`
/// (`every_modal_variant_is_listed_in_all`)。`ALL` 少写一项编译器管不着,
/// 那条测试就是补这个缺口的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Modal {
    SessionManager,
    About,
    Settings,
    Unlock,
    HostKey,
    Paste,
    Import,
    /// F53:内置文件编辑器。**过去漏了** —— 它是个多行输入框,不算模态
    /// 的话里面压根打不出字(键盘全被判给终端)。
    Editor,
    /// D2:远端写操作确认框(新建文件夹 / 重命名 / 删除 / 改权限)。
    /// **过去漏了** —— 新建文件夹时敲的目录名会同时发给远端 shell。
    FilesDialog,
    /// F60:分组管理器。**过去漏了**(`modal_open` 的旧注释已承认)——
    /// 里面有分组名输入框。
    GroupManager,
    /// E2/E3:标签属性弹窗(改名 + 配色)。里面有名字输入框 —— 不算模态的话
    /// 敲的字会同时发给远端 shell(T8)。
    TabProps,
    /// F53/D3-12:退出确认框(「还有改动没传回远端」)。**切片 J 终审才发现
    /// 漏了** —— 它从 D3 引入起就没进过这张表:开着的时候 `Ctrl+W` 仍能关掉
    /// 当前标签、`Ctrl+Shift+B` 仍能开关文件侧栏(两条快捷键的闸门都是
    /// `modal_open()`)。
    ExitConfirm,
    /// 换节点弹窗(pane 标题条上的 `⇆`)。里面有搜索框 —— 不算模态的话敲的
    /// 字会同时发给远端 shell(T8)。
    Rehost,
}

impl Modal {
    const ALL: &'static [Modal] = &[
        Modal::SessionManager,
        Modal::About,
        Modal::Settings,
        Modal::Unlock,
        Modal::HostKey,
        Modal::Paste,
        Modal::Import,
        Modal::Editor,
        Modal::FilesDialog,
        Modal::GroupManager,
        Modal::TabProps,
        Modal::ExitConfirm,
        Modal::Rehost,
    ];
}

/// F59:传输队列在跑时的界面刷新间隔(毫秒)。进度条 5Hz 已经够顺,
/// 再密只是白烧 GPU —— 真实进度数据由 `TransferProgress` 事件实时更新,
/// 这个值只决定「多久把它画出来一次」。
const TRANSFER_UI_INTERVAL_MS: u64 = 200;

/// F56:同时在跑的传输条数。每条一条独立的 sftp channel —— 开太大的话
/// 每条都在抢同一个 TCP 窗口,总吞吐反而掉(设计 D8)。
const DEFAULT_TRANSFER_CONCURRENCY: usize = 4;

impl App {
    pub fn new(
        runtime: Runtime,
        proxy: EventLoopProxy<UserEvent>,
        known_hosts: Arc<Mutex<KnownHostsFile>>,
        initial: Option<SshConfig>,
        cli_direct: bool,
    ) -> Self {
        Self {
            _runtime: runtime,
            tabs: Tabs::default(),
            next_ws_generation: 0,
            start: Instant::now(),
            mods: ModifiersState::empty(),
            kitty: false, // MVP 未协商 Kitty,走优雅退化(T6)
            active: None,
            limiter: FrameLimiter::new(16), // ~60fps(T3)
            next_frame_at: None,
            last_input_at: Instant::now(),
            window_focused: true,
            proxy,
            known_hosts,
            pending_host_key: None,
            host_key_since: None,
            pending_cfg: initial.clone(),
            initial,
            cli_direct,
            ui: crate::ui::UiState::default(),
            store: None,
            visible: shell::window_state::Visibility::default(),
            key_picker_busy: false,
            icon_picker_busy: false,
            import_picker_busy: false,
            ui_dirty: true, // 首帧必须画出来
            files_sidebar_was_open: false,
            cursor_px: (0.0, 0.0),
            clipboard: crate::clipboard::Clipboard::new(),
            last_saved_layout: None,
            layout_checked_at: Instant::now(),
            // F84:真正的值在 `resumed` 里从 `settings.toml` 读(要先有窗口才
            // 知道 DPI,才能把 pt 换成像素)。这里先放默认。
            settings: mullion_store::Settings::default(),
            settings_backup: None,
            settings_families: Vec::new(),
            pending_layout: None,
            settings_not_mono: false,
            pending_restore: None,
            dragging: false,
            prev_click: None,
            press_anchor: None,
            autoscroll: 0,
            ime: Default::default(),
            ime_cursor_area: None,
            pending_paste: None,
            appearance: Default::default(),
            probe_epoch: 0,
            probe_task: None,
            pending_automation: None,
            pending_automation_template: None,
            pending_rehost: Vec::new(),
            pending_skip_automation: false,
            tunnels: Default::default(),
            focus: shell::input_route::Focus::default(),
            // F56:默认 4 条并发。可配 UI 是 D2-c 的欠账,先按设计定的默认值走。
            transfer_queue: crate::files::queue::Queue::new(DEFAULT_TRANSFER_CONCURRENCY),
            transfer_cancels: std::collections::HashMap::new(),
            transfer_specs: std::collections::HashMap::new(),
            edits: crate::edit::sessions::EditSessions::new(),
            editor: None,
            edit_watchers: std::collections::HashMap::new(),
            edit_originals: std::collections::HashMap::new(),
            edit_conflicts: std::collections::HashMap::new(),
            edit_root: crate::edit::tempdir::root(),
        }
    }

    /// 活动标签的终端状态。D1 加了 `Files` 变体后,活动标签是文件视图时返回 `None`。
    fn active_term(&self) -> Option<&TerminalTab> {
        active_term_of(&self.tabs)
    }

    fn active_term_mut(&mut self) -> Option<&mut TerminalTab> {
        self.tabs
            .active_mut()
            .and_then(|t| t.content.as_terminal_mut())
    }

    fn active_ws(&self) -> Option<&Workspace> {
        active_ws_of(&self.tabs)
    }

    fn active_ws_mut(&mut self) -> Option<&mut Workspace> {
        active_ws_mut_of(&mut self.tabs)
    }

    /// 关掉活动标签并收口。关空即回 launcher 态。
    fn close_active_tab(&mut self) {
        if let Some(tab) = self.tabs.close_active() {
            // F55:同 `TabAction::Close` —— 标签的传输随标签一起作废。
            self.cancel_transfers_of(tab.content.generation());
            wind_down(tab);
        }
    }

    // ------------------------------------------------ F71 主密码

    /// 收下 `SessionStore::open` 的结果并做完启动收尾。两条路径(不需要密码 /
    /// 解锁成功)共用 —— 各写一遍的话,以后往收尾里加一步必然漏改一处。
    fn open_store_with(
        &mut self,
        opened: Result<crate::shell::store::SessionStore, mullion_store::StoreError>,
        saved_layout: mullion_store::SavedLayout,
    ) {
        match opened {
            Ok(s) => {
                crate::logx::line(&format!("会话库已打开,{} 个会话", s.list().len()));
                self.store = Some(s);
            }
            Err(e) => {
                crate::logx::line(&format!("会话库打开失败: {e}"));
                self.ui.set_error(format!("会话库打开失败:{e}"));
                self.store = None;
            }
        }
        self.finish_store_open(saved_layout);
    }

    /// 会话库尘埃落定之后的那串收尾:算外观缓存、摆回上次的标签、决定第一屏。
    ///
    /// 抽出来的唯一理由是 F71:解锁框开着的时候这些都还不能做(库还没打开,
    /// 「这条会话还在不在库里」答不上来),得等解锁成功再跑。
    fn finish_store_open(&mut self, saved_layout: mullion_store::SavedLayout) {
        // 启动时先算一次,否则第一次打开会话管理器全是无色。
        self.refresh_appearance();
        // F37:上次那几个标签摆回来(占位,一条连接都不建 —— 设计 §1)。
        // **必须在 store 打开之后**:「这条会话还在不在库里」是丢弃规则之一。
        self.restore_tabs(saved_layout);

        // CLI 直连(路径①)→ 立刻发起连接,进终端态;无参启动(路径②)→ 留在
        // launcher(conn 仍 None)并自动弹出会话管理器,让用户选/建会话(§2/Task7)。
        if let Some(cfg) = self.initial.take() {
            // CLI 直连恒是终端态——这条路径没有会话记录可查协议字段。
            self.spawn_connect(cfg, false);
        } else if self.tabs.is_empty() {
            // F37:恢复出了占位标签就别再弹会话管理器 —— 用户看到的第一屏
            // 该是上次那几个标签,不是一个盖住它们的弹窗。
            self.ui.session_manager_open = true;
        }
    }

    /// 施加解锁框这一帧的结论。返回 `true` = 用户要退出程序。
    fn apply_unlock_action(&mut self, out: crate::ui::unlock::UnlockOut) -> bool {
        use crate::ui::unlock::UnlockOut as O;
        match out {
            O::None => false,
            O::Quit => true,
            O::Submit => {
                let Some(draft) = self.ui.unlock.as_mut() else {
                    return false;
                };
                // 用完就从草稿里搬走:主密码留在一个每帧都被画的结构里没有理由。
                let password = std::mem::take(&mut draft.password);
                let Some(dir) = crate::shell::store::config_dir() else {
                    // 探测那一步已经拿到过目录了,走到这里说明环境在两步之间变了。
                    self.ui.unlock = None;
                    self.ui.set_error("无法定位配置目录".into());
                    let layout = self
                        .pending_layout
                        .take()
                        .unwrap_or_else(mullion_store::SavedLayout::empty);
                    self.finish_store_open(layout);
                    return false;
                };
                match crate::shell::store::SessionStore::unlock(dir, &password) {
                    Ok(s) => {
                        self.ui.unlock = None;
                        let layout = self
                            .pending_layout
                            .take()
                            .unwrap_or_else(mullion_store::SavedLayout::empty);
                        self.open_store_with(Ok(s), layout);
                    }
                    Err(mullion_store::StoreError::WrongPassword) => {
                        // **弹窗留着**:密码打错是最常见的情况,把人踢回一个
                        // 没有会话库的空窗口等于让他重启程序再试。
                        if let Some(d) = self.ui.unlock.as_mut() {
                            d.failed = true;
                        }
                    }
                    Err(e) => {
                        // 密码之外的失败(文件坏了 / 读不出来)不是重试能解决的,
                        // 收掉弹窗、把原因摆出来,程序照常起(会话功能禁用)。
                        crate::logx::line(&format!("解锁失败: {e}"));
                        self.ui.unlock = None;
                        self.ui.set_error(format!("会话库打开失败:{e}"));
                        let layout = self
                            .pending_layout
                            .take()
                            .unwrap_or_else(mullion_store::SavedLayout::empty);
                        self.finish_store_open(layout);
                    }
                }
                self.ui_dirty = true;
                false
            }
        }
    }

    // ------------------------------------------------ F84/F21 外观设置

    /// 弹窗开着就备好草稿与环境,关上就清掉。
    ///
    /// **必须在借出 `self.store`/`self.tabs` 之前调**(同 `Present` 分支里
    /// 那几处触发)——它要 `&mut self`。
    fn sync_settings_dialog(&mut self) {
        if self.ui.settings_open {
            if self.ui.settings_draft.is_none() {
                self.ui.settings_draft = Some(crate::ui::settings::SettingsDraft::from_settings(
                    &self.settings,
                ));
                // 预览是真的改 `self.settings`,原值不另存一份,「取消」就回不去了。
                self.settings_backup = Some(self.settings.clone());
                self.settings_families = self
                    .active
                    .as_ref()
                    .map(|a| a.text.families())
                    .unwrap_or_default();
                self.refresh_monospace_warning();
                self.ui_dirty = true;
            }
        } else if self.ui.settings_draft.is_some() {
            self.ui.settings_draft = None;
            self.settings_backup = None;
            // 清空而不是留着:字体是能在运行期装上的,下次开弹窗要重新枚举。
            self.settings_families = Vec::new();
        }
    }

    /// 量一下当前字体等不等宽,结果给弹窗画那条警告。
    ///
    /// 判据在 `font_pick::is_monospace_advance`(纯函数、有测试),这里只负责
    /// 把两个 advance 量出来 —— 量宽度要 `FontSystem`,那是 GPU 侧的东西。
    fn refresh_monospace_warning(&mut self) {
        self.settings_not_mono = match self.active.as_mut() {
            Some(a) => {
                let m = a.text.advance_of('M');
                let i = a.text.advance_of('i');
                !crate::font_pick::is_monospace_advance(m, i)
            }
            // 还没有窗口就无从量起。报「不等宽」是指错方向,报「等宽」才是
            // 中性的默认(同 `is_monospace_advance` 量不出来时的取舍)。
            None => false,
        };
    }

    /// 把 `self.settings` 的字体族/字号真的换到 `TextLayer` 上。
    ///
    /// **不在这里算 cols/rows**:换字体改的是 `cell_w`/`cell_h`,而
    /// `compute_geoms` 每帧从 `a.text` 现读这两个值,再经 `apply_geometry`
    /// 发 `window_change`(T4)。这条链路已经存在,这里只要标脏让它跑起来;
    /// 在这儿另算一遍尺寸就是 T4 的复发方式。
    fn apply_font(&mut self) {
        let Some(a) = self.active.as_mut() else {
            return;
        };
        let scale = a.window.scale_factor() as f32;
        let px = font_px_for(self.settings.font_pt, scale);
        a.text.set_font(self.settings.font_family.as_deref(), px);
        self.ui_dirty = true;
        self.refresh_monospace_warning();
    }

    /// 施加设置弹窗这一帧的结论。
    fn apply_settings_action(&mut self, out: crate::ui::settings::SettingsOut) {
        use crate::ui::settings::SettingsOut as O;
        match out {
            O::None => {}
            O::Preview => {
                self.take_settings_draft();
                self.apply_font();
            }
            O::Commit => {
                // 也在这里再取一次草稿:用户完全可能什么都没动直接点确定,
                // 那样一次 `Preview` 都没来过。
                self.take_settings_draft();
                self.apply_font();
                let saved = crate::shell::store::config_dir()
                    .ok_or_else(|| "定位不到配置目录".to_string())
                    .and_then(|d| {
                        mullion_store::settings::save(&d, &self.settings).map_err(|e| e.to_string())
                    });
                // 写失败要说 —— 设置是用户刚点了确定的显式动作,静默失败
                // = 这次改了、下次启动又变回去,而他不知道为什么。
                if let Err(e) = saved {
                    self.ui.set_error(format!("设置没能存下来:{e}"));
                }
                self.ui.settings_open = false;
            }
            O::Cancel => {
                if let Some(backup) = self.settings_backup.take() {
                    self.settings = backup;
                    self.apply_font();
                }
                self.ui.settings_open = false;
            }
            // F71:设/改主密码。**弹窗不关** —— 改完主密码还可能接着改字体,
            // 而且用户需要看到那句「已生效」。
            O::SetPassword => {
                let pw = self
                    .ui
                    .settings_draft
                    .as_ref()
                    .map(|d| d.new_password.clone())
                    .unwrap_or_default();
                self.apply_password_change(|s| s.set_master_password(&pw), "主密码已生效");
            }
            O::ClearPassword => {
                self.apply_password_change(
                    crate::shell::store::SessionStore::clear_master_password,
                    "主密码已取消,回到系统钥匙串",
                );
            }
        }
    }

    /// F71:跑一次主密码改动,收尾交给 [`finish_password_change`]。
    fn apply_password_change(
        &mut self,
        f: impl FnOnce(&mut crate::shell::store::SessionStore) -> Result<(), mullion_store::StoreError>,
        ok_msg: &str,
    ) {
        let Some(store) = self.store.as_mut() else {
            self.ui.set_error("会话库没打开,改不了主密码".to_string());
            return;
        };
        let r = f(store);
        let msg = finish_password_change(self.ui.settings_draft.as_mut(), r, ok_msg);
        self.ui.set_error(msg);
    }

    /// 把草稿里的值搬进 `self.settings`(字号顺手夹紧)。
    fn take_settings_draft(&mut self) {
        if let Some(d) = self.ui.settings_draft.as_ref() {
            self.settings.font_family = d.family.clone();
            self.settings.font_pt = mullion_store::settings::clamp_font_pt(d.font_pt);
            self.settings.tmux_bootstrap = d.tmux_bootstrap;
        }
    }

    // ------------------------------------------------ F37 布局持久化(E7/E8)

    /// 把当前的标签栏 + 分屏形状 + 窗口几何拍成一份可落盘的快照。
    ///
    /// **`session_id == None` 的标签跳过**(设计 E2/E6):快速连接(命令行
    /// `user@host`)没有会话记录,恢复出来的占位标签点「重连」时无从查配置,
    /// 只会给出一个必然失败的按钮。
    ///
    /// 占位标签(`Restored`)**按原样写回去**:用户这次没重连它,不代表他
    /// 想把它丢掉 —— 悄悄丢掉的话,关一次窗口就永久少一个标签。
    fn snapshot_layout(&self) -> mullion_store::SavedLayout {
        let (tabs, active_tab) = snapshot_tabs_of(&self.tabs);
        mullion_store::SavedLayout {
            schema_version: mullion_store::CURRENT_LAYOUT_SCHEMA,
            active_tab,
            window: self.window_geometry(),
            tabs,
        }
    }

    /// 当前窗口几何。**物理像素**,与 winit 的 `outer_position` /
    /// `inner_size` / `MonitorHandle::position` 三者同一套单位 —— 混用逻辑点
    /// 会让高 DPI 屏上恢复出来的窗口大小差一个缩放系数。
    ///
    /// 最大化时**仍然记尺寸**(记的是 winit 报的当前尺寸)+ `maximized: true`:
    /// 取消最大化后窗口要有个还原尺寸可用。
    fn window_geometry(&self) -> Option<mullion_store::SavedWindow> {
        let a = self.active.as_ref()?;
        let size = a.window.inner_size();
        let pos = a.window.outer_position().ok();
        Some(mullion_store::SavedWindow {
            width: size.width as f32,
            height: size.height as f32,
            x: pos.map(|p| p.x as f32),
            y: pos.map(|p| p.y as f32),
            maximized: a.window.is_maximized(),
        })
    }

    /// 到点了就比一比、不一样就写盘。`about_to_wait` 每次空闲都会调。
    ///
    /// 节流判据走 `layout_snapshot::should_flush`(有自己的守护测试),
    /// 这里只做接线。
    fn flush_layout_if_due(&mut self) {
        use crate::shell::layout_snapshot as snap;
        let since = self.layout_checked_at.elapsed().as_millis() as u64;
        // 第一个参数恒 `true`:「有没有改动」由下面的快照比对回答,不靠
        // 手工打的脏点(见 `last_saved_layout` 的文档)。
        if !snap::should_flush(true, since) {
            return;
        }
        self.layout_checked_at = Instant::now();
        self.save_layout_if_changed();
    }

    /// 现算一份快照,跟上次写盘的那份不同才写。
    ///
    /// 写盘失败**只记日志**,不弹错误卡片:布局是「上次的场景」,不是用户
    /// 资产(设计 E1),为它打断用户不成比例。
    fn save_layout_if_changed(&mut self) {
        let now = self.snapshot_layout();
        if self.last_saved_layout.as_ref() == Some(&now) {
            return;
        }
        let Some(dir) = crate::shell::store::config_dir() else {
            return;
        };
        match mullion_store::layout::save(&dir, &now) {
            Ok(()) => self.last_saved_layout = Some(now),
            Err(e) => log::debug!(target: "mullion", "布局落盘失败: {e}"),
        }
    }

    /// F124:该配的连接配一遍 tmux 状态上报。
    ///
    /// 每次空闲都跑一遍,但真正的活只有「几个原子读 + 一次减法」——
    /// `should_attempt` 挡在最前面,标签数与主机数都是个位数,不构成 T3
    /// 意义上的每帧重活。
    ///
    /// 失败**只记 debug 日志**:用户没装 tmux、tmux 用的是非默认 socket、
    /// 账号被 `ForceCommand` 限制,都会走到这里,弹错误卡片不成比例。
    ///
    /// `exec()` 本身**没有超时包裹**:挂住的最坏后果是这条连接不再重试
    /// (`busy` 一直置着),且 task 攥着的那份 `Arc<SshConnection>` 会让连接
    /// 晚于 `HostConn` 被 drop 才真断开。不卡 UI、数量有界。刻意的降级。
    fn tick_tmux_bootstrap(&mut self) {
        let enabled = self.settings.tmux_bootstrap;
        let now = Instant::now();
        for tab in self.tabs.iter_mut() {
            let Some(t) = tab.content.as_terminal_mut() else {
                // SFTP 节点标签没有 PTY,也就没有 tmux 客户端在跑;占位标签
                // 连连接都没有。两者都无事可做。
                continue;
            };
            for host in &mut t.ws.hosts {
                let since = host
                    .tmux_last_try
                    .map(|at| now.duration_since(at).as_millis() as u64);
                if !crate::remote_bootstrap::should_attempt(
                    enabled,
                    host.tmux_bootstrap.is_done(),
                    host.tmux_bootstrap.is_busy(),
                    since,
                ) {
                    continue;
                }
                host.tmux_last_try = Some(now);
                host.tmux_bootstrap.mark_busy();
                let conn = host.handle.clone();
                let flags = host.tmux_bootstrap.clone();
                // `tabs` 与 `_runtime` 是 `App` 上两个互不相干的字段,借用检查器
                // 分得开,不必先收集再 spawn。
                self._runtime.spawn(async move {
                    let cmd = crate::remote_bootstrap::bootstrap_command();
                    let ok = match mullion_ssh::exec::exec(&conn, cmd).await {
                        Ok(out) => out.succeeded(),
                        Err(e) => {
                            log::debug!(target: "mullion", "tmux 自举失败:{e}");
                            false
                        }
                    };
                    log::debug!(
                        target: "mullion",
                        "tmux 自举结论:{}",
                        if ok { "已配好" } else { "未配上,稍后重试" }
                    );
                    flags.finish(ok);
                });
            }
        }
    }

    /// F37:把刚连上的内容摆进标签栏。
    ///
    /// `pending` 命中(且那个占位标签还在)→ **就地**替换它,标签在栏里
    /// 的位置不动;否则照旧开一个新标签。返回「是不是就地替换的」——
    /// 终端分支据此决定要不要按存下来的树重建分屏。
    ///
    /// 占位标签在拨号途中被用户关掉了也**不能把连接丢掉**:那是「点了重连,
    /// 什么都没发生」,跟 `ConnectErr`/host_key 那些故意不静默失败的路径
    /// 不一致。此时退回开新标签。
    fn place_tab(
        &mut self,
        pending: Option<&PendingRestore>,
        title: String,
        session_id: Option<SessionId>,
        content: TabContent,
    ) -> bool {
        let ids: Vec<_> = self.tabs.iter().map(|t| t.id).collect();
        let target = replace_target(pending.map(|p| p.tab_id), &ids).map(|ix| (ids[ix], ix));
        let Some((id, ix)) = target else {
            self.tabs.open(title, session_id, content);
            return false;
        };
        if let Some(old) = self.tabs.replace(id, title, content) {
            // 占位标签没有连接、没有 sftp task,`wind_down` 对它是空操作;
            // 照样调是为了「所有被丢弃的标签都过收口」这条不留缺口。
            wind_down(old);
        }
        // 跟 `open` 对齐:连上之后焦点落到这个标签。`spawn_fresh_panes`
        // 取的是**活动标签**的 cfg,不切过去的话恢复分屏会开到别的标签上。
        self.tabs.switch_to_index(ix);
        true
    }

    /// F37:占位标签上按了「重连」(或菜单里「全部重连」轮到它)。
    ///
    /// 走的是**会话管理器双击连接同一条路径**(`dial_plan_for` +
    /// `spawn_connect`),不分叉:分叉出第二条拨号路径意味着代理链/跳板/
    /// 主机密钥/自动化那一整套要维护两份,而它们都已经在那条路上验过。
    ///
    /// 已经有一次重连在途时**直接不理**:同时拉多条连接正是设计 §1 否掉
    /// 自动重连的理由之一。按钮此时本来也是禁用的,这里是第二道闸
    /// (菜单的「全部重连」够得着同一个入口)。
    fn reconnect_tab(&mut self, tab_id: shell::tabs::TabId) {
        if self.pending_restore.is_some() {
            return;
        }
        let Some((session_id, tree, focus_leaf)) =
            self.tabs.iter().find(|t| t.id == tab_id).and_then(|t| {
                match &t.content {
                    TabContent::Restored(r) => Some((r.session_id, r.tree.clone(), r.focus_leaf)),
                    // 已经连上了(或者本来就不是占位标签)—— 没什么可重连的。
                    TabContent::Terminal(_) | TabContent::Files(_) => None,
                }
            })
        else {
            return;
        };
        let plan = match self.store.as_ref().map(|s| s.dial_plan_for(session_id)) {
            Some(Ok(plan)) => plan,
            Some(Err(e)) => {
                self.ui.set_error(e.to_string());
                self.ui_dirty = true;
                return;
            }
            None => return,
        };
        let (cfg, wants_sftp) = plan;
        self.pending_restore = Some(PendingRestore {
            tab_id,
            tree,
            focus_leaf,
        });
        // 按钮禁用靠它(见 `ui::restored`)—— 不置的话用户连点会绕过上面
        // 那道 `pending_restore` 闸之前先弹一堆密码框。
        if let Some(TabContent::Restored(r)) = self
            .tabs
            .iter_mut()
            .find(|t| t.id == tab_id)
            .map(|t| &mut t.content)
        {
            r.dialing = true;
        }
        // `connect_request_last` 是 `ConnectOk` 认「是哪条会话连上了」的唯一
        // 依据(事件本身不带 SessionId),跟双击连接那条路径一样要设。
        self.ui.connect_request_last = Some(session_id);
        self.cli_direct = false;
        self.ui_dirty = true;
        self.spawn_connect(cfg, wants_sftp);
    }

    /// B3:SFTP 节点标签断了之后按「重连」。
    ///
    /// **就地降级成 `RestoredTab` 再走 `reconnect_tab`**,而不是另写一条
    /// 拨号链路 —— F37 已经有一条完整的「拨号 → `ConnectOk` 就地替换标签」
    /// 路径,再写一条就有两处要维护(而且第二条一定会漏掉 `pending_restore`
    /// 那道防连点的闸)。
    ///
    /// 代价:重连后回默认远端目录,不回断线前那个。可接受 —— F120 明确
    /// 「不记忆上次打开的目录」。
    ///
    /// `generation` 而不是「活动标签」:与本文件其余五条 F50 路径同一条
    /// S1 纪律 —— 用户点「重连」那一刻活动标签理论上可能已经切走了。
    fn demote_files_tab_and_reconnect(&mut self, generation: u64) {
        let Some(tab) = self.tabs.by_generation_mut(generation) else {
            return;
        };
        let Some(session_id) = tab.session_id else {
            // 快速连接开出来的 SFTP 标签没有会话记录,无从重连。
            self.ui
                .set_error("这个标签没有对应的会话记录,无法重连".to_string());
            self.ui_dirty = true;
            return;
        };
        let tab_id = tab.id;
        let new_generation = self.next_ws_generation;
        self.next_ws_generation += 1;
        // 旧连接的后台任务必须先收口 —— 每个任务经 Arc 持有一份连接保活
        // 引用,只替换 content 收不了口(同 `wind_down` 那条纪律)。
        let old_content = std::mem::replace(
            &mut tab.content,
            TabContent::Restored(RestoredTab {
                session_id,
                tree: Vec::new(),
                focus_leaf: 0,
                generation: new_generation,
                wants_sftp: true,
                dialing: false,
            }),
        );
        wind_down(Tab {
            id: tab_id,
            title: String::new(),
            session_id: Some(session_id),
            title_override: None,
            color_override: None,
            content: old_content,
        });
        self.reconnect_tab(tab_id);
    }

    /// F37:菜单里的「全部重连」。**一个一个来** —— `reconnect_tab` 里那道
    /// `pending_restore` 闸保证同时只有一条在拨号,这里只是把第一个还没连
    /// 的占位标签交给它;剩下的等这条连上之后用户再按一次。
    ///
    /// 「排队自动连完」是刻意没做的:那等于把设计 §1 否掉的「启动即自动重连
    /// 全部」换个触发时机又做了一遍(N 条握手互相拖慢、缺凭据时连弹 N 个框)。
    fn reconnect_next_restored(&mut self) {
        let Some(id) = self.tabs.iter().find_map(|t| match &t.content {
            TabContent::Restored(r) if !r.dialing => Some(t.id),
            _ => None,
        }) else {
            return;
        };
        self.reconnect_tab(id);
    }

    /// F37:把读出来的布局摆成占位标签。**必须在 `self.store` 打开之后调**
    /// —— 「这条会话还在不在库里」这条丢弃规则要查库(设计 E6)。
    ///
    /// 判据全在 `layout_snapshot::usable` 里(它有自己的守护测试),这里
    /// 只负责把过滤后的结果开成标签。
    fn restore_tabs(&mut self, saved: mullion_store::SavedLayout) {
        let known: Vec<SessionId> = self
            .store
            .as_ref()
            .map_or(Vec::new(), |s| s.list().iter().map(|r| r.id).collect());
        let usable = crate::shell::layout_snapshot::usable(saved, &|id| known.contains(&id));
        if usable.tabs.is_empty() {
            return;
        }
        let count = usable.tabs.len();
        for t in usable.tabs {
            let generation = self.next_ws_generation;
            self.next_ws_generation += 1;
            self.tabs.open(
                t.title,
                Some(t.session_id),
                TabContent::Restored(RestoredTab {
                    session_id: t.session_id,
                    tree: t.tree,
                    focus_leaf: t.focus_leaf,
                    generation,
                    wants_sftp: matches!(t.kind, mullion_store::SavedTabKind::Files),
                    dialing: false,
                }),
            );
        }
        // `open` 每开一个都会把它设成活动标签,所以最后再切回存的那一个。
        self.tabs.switch_to_index(usable.active_tab);
        crate::logx::line(&format!("F37:恢复了 {count} 个占位标签"));
        self.ui_dirty = true;
    }

    /// F55:作废属于某个标签的全部传输。扳旗标(让在跑的 worker 立刻停)
    /// **和**改队列状态(让界面上那几条变成「已取消」)缺一不可。
    fn cancel_transfers_of(&mut self, generation: u64) {
        for id in self.transfer_queue.cancel_generation(generation) {
            if let Some(c) = self.transfer_cancels.remove(&id) {
                c.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            self.transfer_specs.remove(&id);
        }
    }

    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    /// Important #2 / T2:若有 pane 卡在未超时的同步块里,返回该在什么时刻醒来
    /// 重新判定 dirty。`SYNC_TIMEOUT_MS` 只是「过了这个点该出帧」的判定阈值——
    /// 不主动在那个时刻挂一次 `WaitUntil` 的话,冻住的画面只能靠下一个不相关
    /// 事件(鼠标移动/别的 pane 来字节)顺带救回来,T2 点名的「远端发了 BSU、
    /// 链路/TUI 在 ESU 前就死了」场景不保证 ~150ms 自愈。
    ///
    /// 用 `self.start`(与 `now_ms` 同一时钟基准)把绝对的 `deadline_ms` 换算
    /// 成 `Instant`,不是拿 `Instant::now() + (deadline_ms - now_ms)` 这种会
    /// 因两次取时刻之间的间隙而漂移的算法。复用的是 `about_to_wait` 现成的
    /// 「到点重新判定」机制(`RedrawAction::Throttle`/egui repaint_delay 已经
    /// 在用),不是新开一条唤醒路径:一次性、非忙转(T3/T7)——只有严格
    /// `deadline_ms > now_ms` 时才返回 `Some`;到点后该 pane 要么已经不再
    /// holding(正常出帧),要么仍在 holding 但 `sync_since_ms` 不变、
    /// `holding_deadline_ms` 天然不再返回同一个过去的时刻,不会重复排期。
    fn sync_timeout_wake(&self, now_ms: u64) -> Option<Instant> {
        sync_timeout_wake_at(self.start, self.active_ws(), now_ms)
    }

    /// F125:这一帧光标该不该画出来。失焦时恒 `true`(不闪,常显)——
    /// 焦点 pane 在失焦状态下由 `style_for` 收到 `focused=false`,画成空心框。
    fn blink_on(&self, now: Instant) -> bool {
        if !self.window_focused {
            return true;
        }
        let elapsed = now
            .saturating_duration_since(self.last_input_at)
            .as_millis() as u64;
        crate::frame::blink_visible(elapsed, 0)
    }

    /// F125:下一次光标相位翻转的时刻。`None` = 这一刻不需要为闪烁排唤醒
    /// (窗口失焦 / 没有终端在前台)。
    fn blink_wake(&self, now: Instant) -> Option<Instant> {
        if !self.window_focused || self.active_ws().is_none() {
            return None;
        }
        let elapsed = now
            .saturating_duration_since(self.last_input_at)
            .as_millis() as u64;
        let ms = crate::frame::blink_next_flip_ms(elapsed, 0);
        Some(now + std::time::Duration::from_millis(ms))
    }

    /// 所有「定时唤醒源」的汇合点:取最早的那个。**新增定时源一律加在这里**,
    /// 各自排各自的 `WaitUntil` 会互相覆盖(后写的赢),症状是某个定时行为
    /// 时灵时不灵。
    ///
    /// `now` 与两处调用点手里的 `now_ms`(`self.now_ms()`,自 `self.start` 起算
    /// 的毫秒数)同源;`blink_wake` 要的是 `Instant`,这里用 `self.start +
    /// Duration::from_millis(now)` 换算回去——与 `now_ms()` 互为逆运算,不引入漂移。
    fn next_timer_wake(&self, now: u64) -> Option<Instant> {
        let blink_now = self.start + std::time::Duration::from_millis(now);
        [self.sync_timeout_wake(now), self.blink_wake(blink_now)]
            .into_iter()
            .flatten()
            .min()
    }

    /// 有没有模态盖着。分流(§4.5)与标签快捷键共用同一个判据 —— 两处各写一遍
    /// 的话,新增一种弹窗时漏改一处,现象是「弹窗开着按 Ctrl+W 把背后的标签关了」。
    ///
    /// **`match` 必须留在这个函数体里**:既有的两条守护测试
    /// (`the_unlock_dialog_counts_as_a_modal_...` / `the_import_preview_...`)
    /// 扎的是这个函数的源码文本,抽到别的方法里会让它们空过。
    fn modal_open(&self) -> bool {
        Modal::ALL.iter().any(|m| match m {
            Modal::SessionManager => self.ui.session_manager_open,
            Modal::About => self.ui.about_open,
            // F84:设置弹窗里有输入框(手填族名)。不算模态的话,敲进去的字
            // 会同时被发给远端 —— T8 那条「弹窗开着时键盘归 egui」。
            Modal::Settings => self.ui.settings_open,
            // F71:解锁框里输的是主密码。不算模态的话,它会一边被 egui 收进
            // 输入框、一边被原样发给远端 shell —— T8。
            Modal::Unlock => self.ui.unlock.is_some(),
            Modal::HostKey => self.pending_host_key.is_some(),
            Modal::Paste => self.pending_paste.is_some(),
            // F2:导入预览弹窗。里面没有输入框,但有「导入 N 条」这种一按就
            // 落库的按钮,而空格/回车在 egui 里是按钮的激活键 —— T8。
            Modal::Import => self.ui.import.is_some(),
            Modal::Editor => self.editor.is_some(),
            Modal::FilesDialog => self.ui.files_dialog.is_some(),
            Modal::GroupManager => self.ui.group_manager_open,
            // E2/E3:标签属性弹窗里有名字输入框 —— 不算模态的话,敲的字会
            // 同时被发给远端 shell(T8)。
            Modal::TabProps => self.ui.tab_props.is_some(),
            // F53/D3-12:退出确认框。不算模态的话,`Ctrl+W`/`Ctrl+Shift+B`
            // 会在它开着的时候照旧生效(T8)。
            Modal::ExitConfirm => self.ui.exit_pending,
            // 换节点弹窗里有搜索框 —— 同 `TabProps` 的理由(T8)。
            Modal::Rehost => self.ui.rehost.is_some(),
        })
    }

    /// 活动标签本身是不是 `TabContent::Files`(D1 的标签宿主)。`files_owner_generation`
    /// 与 `effective_focus` 共用这一个判据的原子部分——两处各写一遍的话,以后
    /// 加第三种宿主形态时必然有一处漏改。
    fn active_is_files_tab(&self) -> bool {
        active_is_files_tab_of(&self.tabs)
    }

    /// 这一帧文件面板实际要画的标签的世代号(D1:两种宿主互斥)。`None` =
    /// 面板这一刻根本不可见(既不是标签宿主,侧栏也没开)。
    ///
    /// **`Present` 分支与 `effective_focus`/F6 共用这一份判断**(协调者修订
    /// 1)——原先只有 `Present` 分支内联算了一遍,F6 的生效条件如果照抄一份
    /// 判据,两处必然迟早漂移(比如以后侧栏加一种"半开"状态,只改了一处)。
    fn files_owner_generation(&self) -> Option<u64> {
        files_owner_generation_of(&self.tabs, self.ui.files_sidebar_open)
    }

    /// 这一帧真正生效的键盘焦点(协调者修订 2)。裸的 `self.focus` 只是用户
    /// 用 F6 表达的意愿,能不能兑现取决于面板此刻在不在、有没有终端可回:
    ///
    /// - 活动标签是 Files → 恒 `FilesPanel`——那种标签没有终端可回,按
    ///   `Terminal` 路由的话方向键/回车会去找一个不存在的 pane,静默无反应。
    /// - 活动标签是 Terminal 且侧栏关着 → 恒 `Terminal`——面板不可见,焦点
    ///   不能留在看不见的地方,否则键盘表现得像是死了。
    /// - 活动标签是 Terminal 且侧栏开着 → 用 `self.focus`(用户的 F6 意愿生效)。
    fn effective_focus(&self) -> shell::input_route::Focus {
        effective_focus_of(&self.tabs, self.ui.files_sidebar_open, self.focus)
    }

    /// F36/S4:标签快捷键的事件前置处理。返回 `true` = 这个键已被吃掉,
    /// 调用方不要再往下分流(既不喂 egui,也不编码进 PTY)。
    ///
    /// 判定(含模态闸门)全在 `shell::tabs::hotkey` 那个纯函数里,这里只接线。
    fn tab_hotkey_event(&mut self, event: &WindowEvent) -> bool {
        let WindowEvent::KeyboardInput { event: ke, .. } = event else {
            return false;
        };
        if ke.state != ElementState::Pressed {
            return false;
        }
        let Some((key, mods)) = input::translate_key(ke, self.mods) else {
            return false;
        };
        let Some(intent) = shell::tabs::hotkey(key, mods, self.modal_open()) else {
            return false;
        };
        match intent {
            shell::tabs::Intent::Next => self.tabs.switch_next(),
            shell::tabs::Intent::Prev => self.tabs.switch_prev(),
            shell::tabs::Intent::CloseActive => self.close_active_tab(),
            shell::tabs::Intent::Nth(n) => self.tabs.switch_to_nth(n),
        }
        self.request_ui_redraw();
        true
    }

    /// F50 / 设计 D23:`Ctrl+Shift+B` 开关文件侧栏。**独立于** `tab_hotkey_event`
    /// ——一个函数一件事,而且 tab 那个已经有守护测试钉着它的行为,不该把不
    /// 相关的快捷键塞进同一个函数改变它的判定表面。
    ///
    /// 选 `Ctrl+Shift+*` 系是因为它在终端里不产生控制字符,不和远端
    /// tmux / Claude Code 抢键(T5/T6 类冲突)。**不能用 `Ctrl+Shift+F`**
    /// —— 它已被 F100 标注模式占用,先到先得。
    ///
    /// 同 `tab_hotkey_event`:必须在 `window_event` 里输入分流**之前**调用
    /// (T8 纪律)——不然 `Ctrl+Shift+B` 会先被喂给 egui 的焦点系统,`B` 也会
    /// 被编码进 PTY 写给远端。
    fn files_hotkey_event(&mut self, event: &WindowEvent) -> bool {
        let WindowEvent::KeyboardInput { event: ke, .. } = event else {
            return false;
        };
        if ke.state != ElementState::Pressed {
            return false;
        }
        let Some((key, mods)) = input::translate_key(ke, self.mods) else {
            return false;
        };
        if self.modal_open() || !mods.ctrl || !mods.shift || mods.alt || mods.sup {
            return false;
        }
        if !matches!(key, Key::Char('b' | 'B')) {
            return false;
        }
        self.ui.files_sidebar_open = !self.ui.files_sidebar_open;
        self.request_ui_redraw();
        true
    }

    /// ②:把远端栏带到焦点 pane 报出来的目录。
    ///
    /// 两种情形:
    /// - sftp 还没开(`sftp_client()` 是 `None`):什么都不用做 ——
    ///   `trigger_sftp_open` 稍后自己会读焦点 pane 的 cwd 定起始目录。
    /// - 已经开着:走一次普通的 `Goto`,与用户手点目录**同一条路径** ——
    ///   不新开第二条加载/错误处理逻辑。
    ///
    /// 拿不到绝对路径就什么都不做(面板停在原处),不猜 —— 见
    /// [`files_start_dir`]。
    ///
    /// 判定核心在 [`sync_target_of`]:这里只管取数据(世代 / 属主标签 /
    /// sftp client / 焦点 pane 的 cwd)、调用、按结果派发。
    fn sync_files_to_focused_pane(&mut self) {
        let gen = self.files_owner_generation();
        let tab = gen.and_then(|g| self.tabs.by_generation(g));
        let has_client = tab.is_some_and(|t| t.content.sftp_client().is_some());
        let pane_cwd = tab.and_then(|t| t.content.focused_pane_cwd());
        let home = tab.and_then(|t| t.content.sftp_home());
        let Some((gen, dir)) =
            sync_target_of(gen, has_client, pane_cwd.as_deref(), home.as_deref())
        else {
            return;
        };
        let target = mullion_ssh::sftp::RemotePath::from_bytes(dir.into_bytes());
        self.apply_remote_file_action(gen, crate::ui::files_panel::FileAction::Goto(target));
    }

    /// F6/设计 D23:在终端与文件面板之间切换键盘焦点。**独立于**
    /// `files_hotkey_event`——同一个理由,一个函数一件事。
    ///
    /// **不用 `Ctrl+Tab`**——D0 已经把它给了标签切换(`shell::tabs::hotkey`);
    /// `F6` 不是 `mullion_term::keymap::Key` 认识的键(那是发给远端的编码
    /// 词表,F 键不在其中),今天本就到不了终端,截在这里只是让语义显式。
    ///
    /// 同 `tab_hotkey_event`/`files_hotkey_event`:必须在 `window_event` 里
    /// 输入分流**之前**调用(T8 纪律)。
    ///
    /// 协调者修订 1:**不吃**面板不在场时按下的 F6——见函数体内
    /// `files_owner_generation` 那句判断的说明。理由:F6 是纯终端场景里
    /// 也可能被远端 TUI/工具用到的普通功能键,面板不在场时若仍无条件截走,
    /// 会静默偷走这个键(用户按了没反应,还查不出原因)。
    fn focus_hotkey_event(&mut self, event: &WindowEvent) -> bool {
        let WindowEvent::KeyboardInput { event: ke, .. } = event else {
            return false;
        };
        if ke.state != ElementState::Pressed {
            return false;
        }
        if self.modal_open() {
            return false;
        }
        if !matches!(
            ke.logical_key,
            winit::keyboard::Key::Named(winit::keyboard::NamedKey::F6)
        ) {
            return false;
        }
        // 协调者修订 1:F6 的生效条件是"面板此刻在场"——与 `Present` 分支
        // 判断要不要画侧栏共用同一份 `files_owner_generation` 判据(不重复写
        // 一遍逻辑)。面板不在场时**不吃这个键**:直接返回 `false` 让它照旧
        // 走终端编码,否则用户在纯终端场景按 F6(某些远端 TUI/tmux 配置会用
        // 到)会被无声吞掉,表现为"这个键突然没用了"。
        if self.files_owner_generation().is_none() {
            return false;
        }
        self.focus = self.focus.toggled();
        self.request_ui_redraw();
        true
    }

    /// F50/D5:本地栏的一次同步导航。**本地 SSD 上的普通目录**读盘是微秒级,
    /// 不值得像远端那样 spawn 异步任务(远端那条归 Task 10)。四个 `FileAction`
    /// 里只有 `ToggleHidden` 不碰磁盘。
    ///
    /// **已知限制(未根治)**:这个前提在几类目录上不成立 —— 映射的网络盘
    /// (`Z:\`)、断连的 SMB 挂载、未联机的 OneDrive 文件夹、几万项的目录。
    /// `list_dir` 还要对每一项各调一次 `symlink_metadata`,是 N 次 syscall 不是
    /// 一次。而这里跑在 winit 事件循环线程上,一卡就是**整个窗口**没反应
    /// (终端也不刷新),直到系统级超时返回。真要根治得挪去 `spawn_blocking`。
    /// 现在不做:本切片是只读浏览,先把路走通;真机上遇到再说。
    ///
    /// `generation` 是**目标标签**(不是"活动标签")——调用方要么是刚渲染完
    /// 侧栏那一刻的属主标签(见 `Present` 分支里 `files_owner_generation` 的
    /// 说明),要么是首次打开侧栏的触发点,两处都已经知道具体是哪个标签,
    /// 没有必要(也不该)在这里再假设"就是当前活动的那个"。
    fn apply_local_file_action(
        &mut self,
        generation: u64,
        action: crate::ui::files_panel::FileAction,
    ) {
        use crate::files::local;
        use crate::ui::files_panel::FileAction;
        // F52:上传。**在借出 `files` 之前分流** —— `start_transfer` 要
        // `&mut self`,借着 `tab.content.files_panel_mut()` 是调不了的。
        if matches!(action, FileAction::Transfer) {
            self.start_transfer(generation, crate::files::queue::Direction::Upload);
            return;
        }
        // F58:**远端栏**的东西拖到本地栏松手了 = 下载。方向由收到动作的栏
        // 决定,跟上面 `Transfer` 那条正好相反(那是「把本地的送出去」)。
        if let FileAction::Drop(landing) = &action {
            self.start_transfer_into(
                generation,
                crate::files::drag::direction_for_drop(crate::files::PanelColumn::Local),
                landing.clone().sub(),
            );
            return;
        }
        // B3:本地栏是本机文件系统,没有「连接」概念,不会进 `Load::Disconnected`
        // 态、也就画不出「重连」按钮。真收到这个动作说明分派接错了,不静默吞。
        if matches!(action, FileAction::Reconnect) {
            log::warn!("本地栏收到了 Reconnect,已忽略(本地栏没有连接概念)");
            return;
        }
        let Some(tab) = self.tabs.by_generation_mut(generation) else {
            return;
        };
        // F37:占位标签没有面板运行态。走到这儿说明动作的世代号指向了一个
        // 没连接的标签(理论上到不了 —— 占位标签画不出文件面板),丢掉。
        let Some(files) = tab.content.files_panel_mut() else {
            return;
        };
        let target = match &action {
            FileAction::Goto(target) => target.clone(),
            FileAction::Up => local::parent_local(&files.local.cwd),
            FileAction::Refresh => files.local.cwd.clone(),
            FileAction::ToggleHidden => {
                files.local.show_hidden = !files.local.show_hidden;
                self.ui_dirty = true;
                return;
            }
            // D5:本地栏不提供写操作,`menu_items_for` 也不会给出这些项 ——
            // 真到了这里说明菜单构造被改坏了,不静默吞掉。
            FileAction::Ask(ask) => {
                log::warn!("本地栏收到了写操作请求 {ask:?},已忽略(D5)");
                return;
            }
            // 上面已经分流走了(那里不需要借 `files`),走到这儿说明分流被删了。
            FileAction::Transfer | FileAction::Drop(_) | FileAction::Reconnect => return,
            // D5:本地文件在资源管理器里双击就行,`menu_items_for` 也不给
            // 这两项。到这儿同样说明菜单构造被改坏了。
            FileAction::EditExternal | FileAction::EditInline => {
                log::warn!("本地栏收到了编辑请求,已忽略(D5)");
                return;
            }
            FileAction::OpenInExplorer => {
                let dir = files.local.cwd.clone();
                if let Err(e) = local::open_in_file_manager(&dir) {
                    self.ui.set_error(e);
                    self.ui_dirty = true;
                }
                return;
            }
        };
        let seq = files.local.begin_load(target.clone());
        let result = local::list_dir(&local::to_path(&target));
        files.local.accept(seq, result);
        self.ui_dirty = true;
    }

    /// F50/D6:远端栏的一次动作。sftp 还没开好时(`sftp.is_none()`,含"上次
    /// 没开成"的情形),不管点的是哪个具体动作,先把 channel 开起来——打开
    /// 成功后固定用登录目录起步(默认远端目录读配置归 Task 11)。
    ///
    /// **绝不在这里 `block_on`**:开 channel/列目录都是真实网络往返,
    /// 在事件循环线程上等它,会把整个窗口卡在 RTT 上。两步都 spawn 到
    /// `self._runtime`,结果经 `UserEvent` 回送。
    fn apply_remote_file_action(
        &mut self,
        generation: u64,
        action: crate::ui::files_panel::FileAction,
    ) {
        use crate::ui::files_panel::FileAction;
        // D2:这两个不发网络请求,在借出 `files` 之前就分流掉 —— 借着
        // `tab.content.files_panel_mut()` 是没法再调 `&mut self` 方法的。
        match &action {
            // 开对话框。真正的写操作等用户确认之后从 `UiActions::files_op`
            // 回来(见 `apply_file_op`)。
            FileAction::Ask(ask) => {
                self.open_files_dialog(generation, *ask);
                return;
            }
            // 本地栏专属,远端栏收到就是接线接错了 —— 老实记一条,不静默吞。
            FileAction::OpenInExplorer => {
                log::warn!("远端栏收到 OpenInExplorer,已忽略");
                return;
            }
            // F52:下载。同 `Ask`,在借出 `files` 之前分流。
            FileAction::Transfer => {
                self.start_transfer(generation, crate::files::queue::Direction::Download);
                return;
            }
            // F58:**本地栏**的东西拖到远端栏松手了 = 上传。跟上面那条相反。
            FileAction::Drop(landing) => {
                self.start_transfer_into(
                    generation,
                    crate::files::drag::direction_for_drop(crate::files::PanelColumn::Remote),
                    landing.clone().sub(),
                );
                return;
            }
            // F53:编辑。同上,`start_edit` 要 `&mut self`。
            FileAction::EditExternal => {
                self.start_edit(generation, crate::edit::sessions::EditKind::External);
                return;
            }
            FileAction::EditInline => {
                self.start_edit(generation, crate::edit::sessions::EditKind::Inline);
                return;
            }
            // B3:用户在「已断开」态点了「重连」。两种宿主两种语义,**不共用
            // 一条路径**:SFTP 节点标签(`TabContent::Files`)独占自己的连接
            // (ADR-010/D6),重连 = 重建整条连接;终端标签的侧栏只是蹭
            // `ws.hosts[0]` 的连接,sftp channel 单独死掉时重开它即可 ——
            // SSH 本体断了是终端的事,侧栏不越权重建。
            FileAction::Reconnect => {
                let is_files_tab = self
                    .tabs
                    .by_generation(generation)
                    .is_some_and(|t| matches!(t.content, TabContent::Files(_)));
                if is_files_tab {
                    self.demote_files_tab_and_reconnect(generation);
                } else {
                    self.trigger_sftp_open(generation);
                }
                return;
            }
            _ => {}
        }
        let client = {
            let Some(tab) = self.tabs.by_generation_mut(generation) else {
                return;
            };
            tab.content.sftp_client()
        };
        let Some(client) = client else {
            self.trigger_sftp_open(generation);
            return;
        };
        let Some(tab) = self.tabs.by_generation_mut(generation) else {
            return;
        };
        // F37:占位标签没有面板运行态。走到这儿说明动作的世代号指向了一个
        // 没连接的标签(理论上到不了 —— 占位标签画不出文件面板),丢掉。
        let Some(files) = tab.content.files_panel_mut() else {
            return;
        };
        let target = match &action {
            FileAction::Goto(target) => target.clone(),
            // **`RemotePath::parent()`,不是 `local::parent_local`**——那是
            // 本地栏用的,两套路径语义不通用(POSIX vs 本机)。
            FileAction::Up => files.remote.cwd.parent(),
            FileAction::Refresh => files.remote.cwd.clone(),
            FileAction::ToggleHidden => {
                files.remote.show_hidden = !files.remote.show_hidden;
                self.ui_dirty = true;
                return;
            }
            // 这几个在函数开头就分流掉了(那里不需要借 `files`),走不到这儿。
            FileAction::Ask(_)
            | FileAction::OpenInExplorer
            | FileAction::Transfer
            | FileAction::Drop(_)
            | FileAction::EditExternal
            | FileAction::EditInline
            | FileAction::Reconnect => return,
        };
        let seq = files.remote.begin_load(target.clone());
        let task =
            spawn_sftp_list_dir(&self._runtime, &self.proxy, generation, client, target, seq);
        self.track_sftp_task(generation, task);
        self.ui_dirty = true;
    }

    /// F50/设计 D23:文件面板拥有键盘焦点时的按键处理。只有
    /// `shell::input_route::Route::FilesPanel` 到达时才会调用(见
    /// `window_event` 里的输入分流,守 T8)。
    ///
    /// `generation` 是**属主标签**(不是"活动标签")——跟 `apply_local_file_action`/
    /// `apply_remote_file_action` 的既有约定一致(S1 路由纪律),调用方
    /// (`window_event`)已经用 `files_owner_generation()` 算好了传进来,这里
    /// 不再假设"就是当前活动的那个"。
    ///
    /// D2:`Delete`/`F2` 打开删除 / 重命名对话框,**只在远端栏**(设计 D5)。
    fn handle_panel_key(
        &mut self,
        generation: u64,
        key: &winit::keyboard::Key,
        mods: ModifiersState,
    ) {
        use crate::ui::files_panel::FileAction;
        use winit::keyboard::{Key as WinitKey, NamedKey};

        // Ctrl+H:切隐藏文件。得先判——它落进 `WinitKey::Character("h")` 分支,
        // 与下面按具名键的 match 互斥(具名键不受这条影响)。
        if mods.control_key() {
            if let WinitKey::Character(s) = key {
                if s.as_str() == "h" {
                    self.dispatch_panel_action(generation, FileAction::ToggleHidden);
                    return;
                }
            }
        }
        match key {
            WinitKey::Named(NamedKey::Enter) => {
                let Some(tab) = self.tabs.by_generation(generation) else {
                    return;
                };
                // F37:`None` = 占位标签,没有面板可翻 —— 直接不响应。
                let Some((column, state)) = tab.content.files_panel().map(|f| f.active_state())
                else {
                    return;
                };
                // 「进去」是**单目标**动作,认光标行而不是选择集 ——
                // 多选了 5 条时「进哪一个」没有答案。
                let Some(target) = state
                    .cursor
                    .as_ref()
                    .and_then(|name| state.entries.iter().find(|e| &e.name == name))
                    .and_then(|e| state.enter_target(e))
                else {
                    return;
                };
                self.dispatch_panel_action_for(generation, column, FileAction::Goto(target));
            }
            WinitKey::Named(NamedKey::Backspace) => {
                self.dispatch_panel_action(generation, FileAction::Up);
            }
            WinitKey::Named(NamedKey::F5) => {
                self.dispatch_panel_action(generation, FileAction::Refresh);
            }
            WinitKey::Named(NamedKey::Tab) => {
                if let Some(files) = self
                    .tabs
                    .by_generation_mut(generation)
                    .and_then(|t| t.content.files_panel_mut())
                {
                    files.active_column = files.active_column.flipped();
                    self.ui_dirty = true;
                }
            }
            WinitKey::Named(NamedKey::Delete) | WinitKey::Named(NamedKey::F2) => {
                // 设计 D5:本地栏不提供删除 / 重命名。焦点在本地栏时这两个键
                // **静默不动**,不是转投远端栏 —— 用户看着本地栏按 Delete、
                // 结果删了远端文件,是这一片能造成的最坏后果。
                let column = self
                    .tabs
                    .by_generation(generation)
                    .and_then(|t| t.content.files_panel())
                    .map(|f| f.active_column);
                if column != Some(crate::ui::files_panel::PanelColumn::Remote) {
                    return;
                }
                let ask = if matches!(key, WinitKey::Named(NamedKey::Delete)) {
                    crate::ui::files_panel::FileAsk::Delete
                } else {
                    crate::ui::files_panel::FileAsk::Rename
                };
                self.dispatch_panel_action_for(
                    generation,
                    crate::ui::files_panel::PanelColumn::Remote,
                    FileAction::Ask(ask),
                );
            }
            WinitKey::Named(NamedKey::ArrowUp) => self.move_panel_selection(generation, -1),
            WinitKey::Named(NamedKey::ArrowDown) => self.move_panel_selection(generation, 1),
            _ => {}
        }
    }

    /// `handle_panel_key` 的小工具:按当前有焦点的那一栏(`active_column`)
    /// 把 `action` 路由到对应的 `apply_*_file_action`。
    fn dispatch_panel_action(
        &mut self,
        generation: u64,
        action: crate::ui::files_panel::FileAction,
    ) {
        let Some(column) = self
            .tabs
            .by_generation(generation)
            .and_then(|t| t.content.files_panel())
            .map(|f| f.active_column)
        else {
            return;
        };
        self.dispatch_panel_action_for(generation, column, action);
    }

    /// D2:把一个「打开对话框」的意图落成 `UiState::files_dialog`。
    ///
    /// **对话框的内容在这里一次性算好**(要删哪些、原名是什么、当前权限是
    /// 多少),不是等渲染时再回头查面板状态:对话框开着的时候用户可能已经
    /// 切了标签、目录已经刷新过,那时再查就是另一份数据了。
    fn open_files_dialog(&mut self, generation: u64, ask: crate::ui::files_panel::FileAsk) {
        use crate::ui::files_dialog::FilesDialog;
        use crate::ui::files_panel::FileAsk;

        let Some(tab) = self.tabs.by_generation(generation) else {
            return;
        };
        let Some(state) = tab.content.files_panel().map(|f| &f.remote) else {
            return;
        };
        let dialog = match ask {
            FileAsk::NewDir => Some(FilesDialog::NewDir {
                parent: state.cwd.clone(),
                name: String::new(),
            }),
            FileAsk::Rename => state.cursor.as_ref().map(|cur| FilesDialog::Rename {
                from: state.cwd.join(cur.as_bytes()),
                name: cur.display().to_string(),
            }),
            FileAsk::Chmod => state.cursor.as_ref().and_then(|cur| {
                let e = state.entries.iter().find(|e| &e.name == cur)?;
                Some(FilesDialog::Chmod {
                    path: state.cwd.join(cur.as_bytes()),
                    mode: e.mode & 0o777,
                })
            }),
            FileAsk::Delete => {
                // 选中集为空时退化成「删光标那一条」—— 用户按 Delete 时
                // 多半就是想删高亮那条,弹一个「没有选中任何条目」的空框
                // 只会让人以为程序坏了。
                let picked = if state.selected.is_empty() {
                    state.cursor.iter().cloned().collect::<Vec<_>>()
                } else {
                    state.selected_paths()
                };
                let targets: Vec<(mullion_ssh::sftp::RemotePath, bool)> = picked
                    .iter()
                    .filter_map(|name| {
                        let e = state.entries.iter().find(|e| &e.name == name)?;
                        // 发不出去的名字不许进删除列表 —— 请求打不中那个文件,
                        // 而它会在确认框里让用户以为「删了 5 条」。
                        if !name.is_operable() {
                            return None;
                        }
                        Some((
                            state.cwd.join(name.as_bytes()),
                            e.kind == mullion_ssh::sftp::EntryKind::Dir,
                        ))
                    })
                    .collect();
                if targets.is_empty() {
                    None
                } else {
                    Some(FilesDialog::Delete { targets })
                }
            }
        };
        if dialog.is_some() {
            self.ui.files_dialog = dialog;
            // 对话框是新出现的窗口,不请求重绘的话键盘发起的那条路径
            // (Delete / F2)要等鼠标动一下才画得出来(D1 复核挖出的同款 bug)。
            self.request_ui_redraw();
        }
    }

    /// D2/F54:执行一次已确认的远端写操作。
    ///
    /// 全部走后台 task + `UserEvent::SftpOpDone` 回流,**不在 UI 线程上等**:
    /// 一次递归删除在高延迟链路上可能跑几十秒,阻塞窗口线程等于整个程序卡死。
    fn apply_file_op(&mut self, generation: u64, op: crate::ui::files_dialog::FileOp) {
        use crate::ui::files_dialog::FileOp;

        // F55:冲突处置不是「一次远端写操作」——它只改队列状态,由
        // `pump_transfers` 决定要不要重新起 worker。在这里提前分流,
        // 免得为它白开一条 sftp channel。
        if let FileOp::Resolve {
            job,
            choice,
            apply_all,
        } = op
        {
            self.transfer_queue.resolve_conflict(job, choice, apply_all);
            self.ui_dirty = true;
            return;
        }
        // F53:编辑冲突的处置。同上,不走远端写操作那条通用路径 ——
        // 三条出路里有两条根本不发请求。
        if let FileOp::ResolveEdit { key, choice } = op {
            self.resolve_edit(key, choice);
            return;
        }

        let Some(tab) = self.tabs.by_generation(generation) else {
            return;
        };
        let Some(client) = tab.content.sftp_client() else {
            self.ui
                .set_error("SFTP 通道还没建立,请先等目录加载完".into());
            return;
        };
        let conn = tab.content.sftp_connection();
        let proxy = self.proxy.clone();
        let task = self._runtime.spawn(async move {
            let result = match op {
                FileOp::NewDir(p) => client.create_dir(&p).await.map_err(|e| e.to_string()),
                FileOp::Rename { from, to } => {
                    client.rename(&from, &to).await.map_err(|e| e.to_string())
                }
                FileOp::Chmod { path, mode } => client
                    .set_permissions(&path, mode)
                    .await
                    .map_err(|e| e.to_string()),
                FileOp::Delete { targets } => delete_all(&client, conn.as_ref(), &targets).await,
                // 函数开头已经分流走了,走到这里说明分流被删了。
                FileOp::Resolve { .. } | FileOp::ResolveEdit { .. } => {
                    unreachable!("冲突处置不该走远端写操作这条路")
                }
            };
            let _ = proxy.send_event(UserEvent::SftpOpDone { generation, result });
        });
        self.track_sftp_task(generation, task);
    }

    /// F52:发起一批传输。**方向由发起的栏决定**:远端栏 = 下载(源是远端栏
    /// 的选中集),本地栏 = 上传。
    ///
    /// 这里只把「用户点中的那几条」摘出来就交给后台展开(目录要递归):远端
    /// 递归要走网络列目录,本地递归要遍历磁盘,压在窗口线程上就是整个程序
    /// 卡住。展开结果经 `UserEvent::TransferPlanned` 回来一次性入队 ——
    /// 边展开边入队的话,队列会在用户眼前长上半天。
    fn start_transfer(&mut self, generation: u64, dir: crate::files::queue::Direction) {
        self.start_transfer_into(generation, dir, None);
    }

    /// F58:同 [`Self::start_transfer`],但目标目录可以是目标栏当前目录下的
    /// **子目录** —— 拖拽落在目录行上时用。
    ///
    /// `into` 只改**目标**那一侧的目录,源那一侧仍是发起栏的 cwd:拖的是
    /// 源栏选中的那几条,它们的位置不因为落点在哪而改变。
    fn start_transfer_into(
        &mut self,
        generation: u64,
        dir: crate::files::queue::Direction,
        into: Option<Vec<u8>>,
    ) {
        use crate::files::queue::Direction;
        let Some(tab) = self.tabs.by_generation(generation) else {
            return;
        };
        let Some(files) = tab.content.files_panel() else {
            return;
        };
        let src = match dir {
            Direction::Download => &files.remote,
            Direction::Upload => &files.local,
        };
        // 名字发不出去 wire 请求的一律不收(同删除那条路径的判据)——
        // 收进来只会让用户以为「传了 5 个」,其实有一个必然失败。
        let picked: Vec<(mullion_ssh::sftp::RemotePath, bool, u64)> = src
            .picked_entries()
            .into_iter()
            .filter(|e| e.name.is_operable())
            .map(|e| {
                (
                    e.name.clone(),
                    e.kind == mullion_ssh::sftp::EntryKind::Dir,
                    e.size,
                )
            })
            .collect();
        if picked.is_empty() {
            return;
        }
        let mut remote_cwd = files.remote.cwd.clone();
        let mut local_cwd = files.local.cwd.clone();
        // F58:落在目录行上 —— 目标那一侧换成那个子目录。**用各自的 join**:
        // 远端恒用 `/`(SFTP 线上的规矩),本地用平台分隔符,两套路径语义
        // 不通用(同 `Up` 那条已知区分)。
        if let Some(name) = into {
            match dir {
                Direction::Download => {
                    local_cwd = crate::files::local::join_local(&local_cwd, &name)
                }
                Direction::Upload => remote_cwd = remote_cwd.join(&name),
            }
        }
        let Some(client) = tab.content.sftp_client() else {
            self.ui
                .set_error("SFTP 通道还没建立,请先等目录加载完".into());
            self.ui_dirty = true;
            return;
        };
        let proxy = self.proxy.clone();
        let task = self._runtime.spawn(async move {
            let result = plan_transfer(&client, dir, &picked, &remote_cwd, &local_cwd).await;
            let _ = proxy.send_event(UserEvent::TransferPlanned { generation, result });
        });
        self.track_sftp_task(generation, task);
    }

    /// F52:把从资源管理器扔进窗口的一批绝对路径上传到**远端栏当前目录**。
    ///
    /// 落点恒为远端 cwd,不看指针在哪 —— 理由见 `files_panel::show` 里
    /// `drop_in` 那段(winit 的拖放事件不带坐标),界面已在松手前把这条规则
    /// 写在栏顶上了。
    ///
    /// 一批路径可能横跨多个父目录,而既有的 `plan_transfer` 收的是「相对某
    /// 一个本地目录的名字」。按父目录分组后逐组发一次 —— **上传那条通路
    /// 一个字不用改**(改它等于同时动右键上传那条已经验过的路)。
    fn start_drop_in(&mut self, generation: u64, paths: Vec<std::path::PathBuf>) {
        use crate::files::queue::Direction;
        let Some(tab) = self.tabs.by_generation(generation) else {
            return;
        };
        let Some(remote_cwd) = tab.content.files_panel().map(|f| f.remote.cwd.clone()) else {
            return;
        };
        let Some(client) = tab.content.sftp_client() else {
            self.ui
                .set_error("SFTP 通道还没建立,请先等目录加载完".into());
            self.ui_dirty = true;
            return;
        };
        for (parent, names) in crate::files::drag::group_by_parent(&paths) {
            // 目录/大小当场 stat:后台展开要靠 `is_dir` 决定递不递归,而这一
            // 批是**本地**路径,`metadata` 就在手边。stat 不到的(拖过来那一
            // 瞬间被删/权限不足)直接跳过,不塞一条必然失败的 job 进队列。
            let picked: Vec<(mullion_ssh::sftp::RemotePath, bool, u64)> = names
                .into_iter()
                .filter_map(|name| {
                    let p = parent.join(crate::files::local::to_path(
                        &mullion_ssh::sftp::RemotePath::from_bytes(name.clone()),
                    ));
                    let md = std::fs::metadata(&p).ok()?;
                    Some((
                        mullion_ssh::sftp::RemotePath::from_bytes(name),
                        md.is_dir(),
                        md.len(),
                    ))
                })
                .collect();
            if picked.is_empty() {
                continue;
            }
            let local_cwd =
                mullion_ssh::sftp::RemotePath::from_bytes(crate::files::local::path_bytes(&parent));
            let client = client.clone();
            let remote_cwd = remote_cwd.clone();
            let proxy = self.proxy.clone();
            let task = self._runtime.spawn(async move {
                let result =
                    plan_transfer(&client, Direction::Upload, &picked, &remote_cwd, &local_cwd)
                        .await;
                let _ = proxy.send_event(UserEvent::TransferPlanned { generation, result });
            });
            self.track_sftp_task(generation, task);
        }
    }

    /// F59:把远端栏当前的选中集交给操作系统拖出去。
    ///
    /// **立刻返回**,`DoDragDrop` 在 `dragout` 自己的线程上跑(设计 D10:
    /// winit 的回调栈里起嵌套模态消息循环必 panic)。这里做的只有三件事:
    /// 摘出能拖的那几条、把跳过的目录数说出来、把 runtime 句柄和 sftp 连接
    /// 交过去(流是目标程序读的,读的时候我们才真去拉)。
    fn start_drag_out(&mut self, generation: u64) {
        let Some(tab) = self.tabs.by_generation(generation) else {
            return;
        };
        let Some(files) = tab.content.files_panel() else {
            return;
        };
        let (items, skipped_dirs) = crate::dragout::items_for(&files.remote);
        let Some(client) = tab.content.sftp_client() else {
            self.ui
                .set_error("SFTP 通道还没建立,请先等目录加载完".into());
            self.ui_dirty = true;
            return;
        };
        if items.is_empty() {
            // 全被跳过(整批都是目录)。**必须说话** —— 不然用户拖出去
            // 松了手,什么都没发生,也没有任何提示。
            if skipped_dirs > 0 {
                self.ui
                    .set_error(format!("目录还不能拖出({skipped_dirs} 个),请选文件"));
                self.ui_dirty = true;
            }
            return;
        }
        if skipped_dirs > 0 {
            self.ui
                .set_error(format!("跳过了 {skipped_dirs} 个目录,只拖文件"));
            self.ui_dirty = true;
        }
        crate::dragout::start(self._runtime.handle().clone(), client, items);
    }

    /// F53:开始编辑光标行那个远端文件。
    ///
    /// **不进传输队列**(D3-1):临时路径是我们自己造的,冲突/重名/Windows
    /// 非法名那一整套语义都不适用;传输面板是「用户发起的传输」的账本,
    /// 混进「打开一个文件」会让「全部取消」的语义变歧义。
    ///
    /// 目标取**光标行**,与 `FileAsk::Rename`/`Chmod` 同一条约定 ——
    /// 双击那条入口会先把光标挪到被双击的行上(见 `files_panel::show`)。
    fn start_edit(&mut self, generation: u64, kind: crate::edit::sessions::EditKind) {
        use crate::edit::sessions::EditKind;
        let Some(tab) = self.tabs.by_generation(generation) else {
            return;
        };
        let Some(state) = tab.content.files_panel().map(|f| &f.remote) else {
            return;
        };
        let Some(cur) = state.cursor.clone() else {
            return;
        };
        let Some(e) = state.entries.iter().find(|e| e.name == cur) else {
            return;
        };
        // 名字送不上线的行,任何单目标操作都做不了(同删除/传输那条判据)。
        if e.kind != mullion_ssh::sftp::EntryKind::File || !cur.is_operable() {
            self.ui.set_error("只能编辑普通文件".into());
            self.ui_dirty = true;
            return;
        }
        let limit = match kind {
            EditKind::Inline => crate::edit::INLINE_LIMIT,
            EditKind::External => crate::edit::EXTERNAL_LIMIT,
        };
        // 菜单已经按 size 置灰过一遍,这里再判一次是因为**光标行可能已经变了**
        // (双击那条路径就是先挪光标再发动作),而且键盘快捷键那类入口根本
        // 不经过菜单。闸门必须落在真正要发请求的这一处。
        if e.size > limit {
            self.ui.set_error(format!(
                "「{}」有 {},超过了这种打开方式的上限({})",
                cur.display(),
                crate::files::human_size(e.size),
                crate::files::human_size(limit),
            ));
            self.ui_dirty = true;
            return;
        }
        let remote = state.cwd.join(cur.as_bytes());
        let Some(client) = tab.content.sftp_client() else {
            self.ui
                .set_error("SFTP 通道还没建立,请先等目录加载完".into());
            self.ui_dirty = true;
            return;
        };
        let proxy = self.proxy.clone();
        let path = remote.clone();
        let task = self._runtime.spawn(async move {
            let result = read_for_edit(&client, &path, limit).await;
            let _ = proxy.send_event(UserEvent::EditOpened {
                generation,
                kind,
                remote,
                result,
            });
        });
        self.track_sftp_task(generation, task);
        self.ui.set_toast("正在打开…");
        self.ui_dirty = true;
    }

    /// F53:文件读回来了 —— 落临时文件交给外部程序,或者开内置窗口。
    fn finish_edit_open(
        &mut self,
        generation: u64,
        kind: crate::edit::sessions::EditKind,
        remote: mullion_ssh::sftp::RemotePath,
        result: Result<(Vec<u8>, crate::edit::sessions::RemoteStamp), String>,
    ) {
        use crate::edit::sessions::EditKind;
        self.ui_dirty = true;
        let (bytes, snapshot) = match result {
            Ok(v) => v,
            Err(e) => {
                self.ui.set_error(e);
                return;
            }
        };
        // 属主标签在这次往返里被关掉了:临时文件还没落地,直接丢弃就是对的。
        let Some(tab) = self.tabs.by_generation(generation) else {
            return;
        };
        let session = tab.title.clone();
        let label = remote.display().to_string();
        let local = crate::edit::tempdir::temp_path(&self.edit_root, &session, &remote);
        match kind {
            // 外部编辑对内容**完全透明**:字节原样落盘、原样传回。二进制也照开,
            // 用户自己知道 .png 该用什么程序打开(D3-3 那条「二进制拒绝」是
            // 内置编辑器的事 —— 那里内容要变成 `String`)。
            EditKind::External => {
                if let Some(dir) = local.parent() {
                    if let Err(e) = std::fs::create_dir_all(dir) {
                        self.ui.set_error(format!("建不了临时目录:{e}"));
                        return;
                    }
                }
                if let Err(e) = std::fs::write(&local, &bytes) {
                    self.ui.set_error(format!("写不了临时文件:{e}"));
                    return;
                }
                if let Err(e) = crate::edit::launch::open_with_default(&local) {
                    self.ui.set_error(e);
                    return;
                }
                let key = self
                    .edits
                    .add(generation, kind, remote, local.clone(), snapshot);
                self.edit_originals.insert(key, bytes);
                self.watch_edit(key, local);
                self.ui
                    .set_toast(format!("已交给默认程序:{label}。存盘后自动回传"));
            }
            EditKind::Inline => {
                let probe = crate::edit::text::probe(&bytes);
                let text = crate::edit::text::decode(&bytes, &probe);
                // 内置这一条在磁盘上**没有文件**:内容全在窗口里,所以也
                // **不起看门任务**(「变了没有」窗口自己知道)。`local` 仍然
                // 记着,只为「结束编辑」时统一走同一条清理路径(删不存在的
                // 文件本来就不报错)。
                let key = self.edits.add(generation, kind, remote, local, snapshot);
                self.edit_originals.insert(key, bytes);
                self.editor = Some(crate::ui::editor_window::EditorState::new(
                    key,
                    label,
                    text,
                    probe.read_only_reason(),
                    probe.eol,
                    probe.bom,
                ));
            }
        }
    }

    /// F53/D3-10:给一条外部编辑起看门任务。1 秒看一次本地 mtime。
    ///
    /// **不猜编辑器进程退没退**:用户可能开着 vim 存十次,也可能用一个
    /// 常驻的 GUI 编辑器一直开着。「文件变了就传」是唯一不需要猜的判据。
    fn watch_edit(&mut self, key: u64, local: std::path::PathBuf) {
        let proxy = self.proxy.clone();
        let task = self._runtime.spawn(async move {
            let mut last: Option<crate::edit::sessions::LocalStamp> = None;
            let mut first = true;
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                let stamp = local_stamp(&local);
                // 只在**变了**的时候发。每秒一条空事件会把事件循环从 `Wait`
                // 里薅起来,白烧 CPU(T3)。首轮无条件发一次,让
                // `changed_locally` 把基线登记上。
                if first || stamp != last {
                    first = false;
                    last = stamp;
                    if proxy
                        .send_event(UserEvent::EditTick { key, stamp })
                        .is_err()
                    {
                        return; // 事件循环没了
                    }
                }
            }
        });
        if let Some(old) = self.edit_watchers.insert(key, task) {
            old.abort();
        }
    }

    /// F53:看门任务报了一次本地状态。
    fn on_edit_tick(&mut self, key: u64, stamp: Option<crate::edit::sessions::LocalStamp>) {
        for k in self.edits.changed_locally(&[(key, stamp)]) {
            self.push_edit(k, None);
        }
    }

    /// F53:把一条编辑的当前内容传回远端。
    ///
    /// `bytes` 为 `None` 时从本地临时文件读(外部编辑那条路);内置编辑器
    /// 保存时把编码好的字节直接给进来 —— 内置那条在磁盘上根本没有文件。
    ///
    /// `snapshot` 取自条目本身:回传前 `stat` 一次远端跟它比,对不上就是
    /// 冲突(D3-8)。冲突处置里的「覆盖远端」会先把快照刷成远端当前值,
    /// 所以走的还是这同一条函数,不需要一条「强制写」的旁路。
    fn push_edit(&mut self, key: u64, bytes: Option<Vec<u8>>) {
        use crate::edit::sessions::EditState;
        let Some(e) = self.edits.get(key) else {
            return;
        };
        let (generation, remote, local, snapshot, saved_once) = (
            e.generation,
            e.remote.clone(),
            e.local.clone(),
            e.snapshot,
            e.saved_once,
        );
        let bytes = match bytes {
            Some(b) => b,
            None => match std::fs::read(&local) {
                Ok(b) => b,
                Err(err) => {
                    self.fail_edit(key, format!("读不到本地临时文件:{err}"));
                    return;
                }
            },
        };
        // D3-7:只在**第一次**回传前留备份。远端那份在第一次回传之后就是
        // 我们自己写的了,再备份既没意义又要多传一遍全量(用户在 vim 里
        // 存十次就是十遍)。
        let backup = if saved_once {
            None
        } else {
            self.edit_originals.get(&key).cloned()
        };
        let client = match self
            .tabs
            .by_generation(generation)
            .and_then(|t| t.content.sftp_client())
        {
            Some(c) => c,
            None => {
                self.fail_edit(key, "连接已断开,改动还在本地临时文件里".into());
                return;
            }
        };
        if let Some(e) = self.edits.get_mut(key) {
            e.state = EditState::Uploading;
        }
        let proxy = self.proxy.clone();
        let task = self._runtime.spawn(async move {
            let result = write_back(&client, &remote, &bytes, snapshot, backup).await;
            let _ = proxy.send_event(UserEvent::EditSaved { key, result });
        });
        self.track_sftp_task(generation, task);
        self.ui_dirty = true;
    }

    /// 把一条编辑标成失败。**不弹错误框** —— 失败原因就写在「编辑中」那一行上,
    /// 用户正在别的窗口里改字,一个模态框只会打断他。
    fn fail_edit(&mut self, key: u64, why: String) {
        use crate::edit::sessions::EditState;
        let why2 = why.clone();
        if let Some(e) = self.edits.get_mut(key) {
            e.state = EditState::Failed(why);
        }
        if let Some(ed) = self.editor.as_mut().filter(|ed| ed.key == key) {
            ed.finish_save(Err(why2));
        }
        self.ui_dirty = true;
    }

    /// F53:一次回传收工。
    fn on_edit_saved(&mut self, key: u64, result: Result<EditWriteOutcome, String>) {
        use crate::edit::sessions::EditState;
        self.ui_dirty = true;
        let (generation, local, label) = match self.edits.get(key) {
            Some(e) => (e.generation, e.local.clone(), e.label.clone()),
            // 条目在这次往返里被「结束编辑」掉了 —— 结果丢掉就是对的。
            None => return,
        };
        match result {
            Ok(EditWriteOutcome::Done(remote_now)) => {
                self.edits
                    .accept_write_back(key, remote_now, local_stamp(&local));
                self.edit_originals.remove(&key);
                self.edit_conflicts.remove(&key);
                if let Some(ed) = self.editor.as_mut().filter(|ed| ed.key == key) {
                    ed.finish_save(Ok(()));
                }
                self.ui.set_toast(format!("已回传:{label}"));
                // 远端那一栏要刷 —— 大小/时间变了,不刷用户会以为没传上去。
                self.dispatch_panel_action_for(
                    generation,
                    crate::ui::files_panel::PanelColumn::Remote,
                    crate::ui::files_panel::FileAction::Refresh,
                );
            }
            Ok(EditWriteOutcome::Conflict(remote_now)) => {
                self.edit_conflicts.insert(key, remote_now);
                if let Some(e) = self.edits.get_mut(key) {
                    e.state = EditState::Conflict;
                }
                if let Some(ed) = self.editor.as_mut().filter(|ed| ed.key == key) {
                    ed.finish_save(Err("远端已被改动,见处置框".into()));
                }
                self.open_edit_conflict(key);
            }
            Err(why) => self.fail_edit(key, why),
        }
    }

    /// F53:开(或重开)一条编辑的冲突处置框。
    fn open_edit_conflict(&mut self, key: u64) {
        let Some(e) = self.edits.get(key) else {
            return;
        };
        self.ui.files_dialog = Some(crate::ui::files_dialog::FilesDialog::EditConflict {
            name: e.remote.display().to_string(),
            key,
        });
        self.ui_dirty = true;
    }

    /// F53:用户在冲突框里选完了。
    fn resolve_edit(&mut self, key: u64, choice: crate::ui::files_dialog::EditResolve) {
        use crate::edit::sessions::EditState;
        use crate::ui::files_dialog::EditResolve;
        self.ui_dirty = true;
        // 没有记到远端当时的戳就没法安全处置(条目已经被收走之类)。
        let Some(remote_now) = self.edit_conflicts.get(&key).copied() else {
            return;
        };
        match choice {
            EditResolve::KeepRemote => {
                // D3-9:**必须刷快照**。不刷的话下一次保存还会撞上同一个
                // 冲突,这个框永远关不掉。
                self.edits.keep_remote(key, remote_now);
                self.edit_conflicts.remove(&key);
                self.ui.set_toast("已保留远端那一份");
            }
            EditResolve::Overwrite => {
                // 把比对基准换成远端当前值,再走同一条回传 —— 于是这一次
                // `stat` 一定对得上,写下去。
                if let Some(e) = self.edits.get_mut(key) {
                    e.snapshot = remote_now;
                    e.state = EditState::Watching;
                }
                self.edit_conflicts.remove(&key);
                let bytes = self.editor_bytes_for(key);
                self.push_edit(key, bytes);
            }
            EditResolve::SaveCopy => {
                let Some(e) = self.edits.get(key) else {
                    return;
                };
                let (generation, copy) = (e.generation, copy_path(&e.remote));
                let bytes = match self.editor_bytes_for(key) {
                    Some(b) => b,
                    None => match std::fs::read(&e.local) {
                        Ok(b) => b,
                        Err(err) => {
                            self.fail_edit(key, format!("读不到本地临时文件:{err}"));
                            return;
                        }
                    },
                };
                let Some(client) = self
                    .tabs
                    .by_generation(generation)
                    .and_then(|t| t.content.sftp_client())
                else {
                    self.fail_edit(key, "连接已断开,改动还在本地临时文件里".into());
                    return;
                };
                // 副本落地之后,这条编辑就认远端那一份 —— 否则它会一直红着,
                // 而用户已经把自己的改动安全存下来了。
                self.edits.keep_remote(key, remote_now);
                self.edit_conflicts.remove(&key);
                let name = copy.display().to_string();
                let proxy = self.proxy.clone();
                let task = self._runtime.spawn(async move {
                    let result = client
                        .write_all_truncate(&copy, &bytes)
                        .await
                        .map_err(|e| format!("另存副本失败:{e}"));
                    let _ = proxy.send_event(UserEvent::SftpOpDone { generation, result });
                });
                self.track_sftp_task(generation, task);
                self.ui.set_toast(format!("已另存为 {name}"));
            }
        }
    }

    /// 内置编辑器此刻的字节(如果这条正开在内置编辑器里)。外部编辑那条恒
    /// `None` —— 内容在临时文件里,由调用方读盘。
    fn editor_bytes_for(&self, key: u64) -> Option<Vec<u8>> {
        self.editor
            .as_ref()
            .filter(|e| e.key == key)
            .map(|e| e.bytes())
    }

    /// F53:结束一条编辑 —— 停看门、删临时文件、从列表里去掉。
    fn end_edit(&mut self, key: u64) {
        if let Some(task) = self.edit_watchers.remove(&key) {
            task.abort();
        }
        self.edit_originals.remove(&key);
        self.edit_conflicts.remove(&key);
        if let Some(e) = self.edits.remove(key) {
            // 删不掉只记一条:临时文件残留不影响正确性,退出时那次
            // `tempdir::purge` 还会再扫一遍。
            if e.kind == crate::edit::sessions::EditKind::External {
                if let Err(err) = std::fs::remove_file(&e.local) {
                    log::debug!("删临时文件失败({}):{err}", e.local.display());
                }
            }
        }
        if self.editor.as_ref().is_some_and(|e| e.key == key) {
            self.editor = None;
        }
        self.ui_dirty = true;
    }

    /// F55/F56:每帧调一次 —— 队列放行几条就起几条 worker。
    ///
    /// **每条 job 自己开一条 sftp channel**(worker 里的 `SftpClient::open`):
    /// 共用一条的话请求在同一个 session 上串行,并发度实际等于 1,设计 D8
    /// 说的吞吐问题原样还在。
    fn pump_transfers(&mut self) {
        for id in self.transfer_queue.take_runnable() {
            let Some(spec) = self.transfer_specs.get(&id).cloned() else {
                self.transfer_queue.finish(id, Err("任务参数丢了".into()));
                continue;
            };
            let Some(tab) = self.tabs.by_generation(spec.generation) else {
                // 属主标签没了 —— 关标签那条路径已经 `cancel_generation` 过,
                // 这里是兜底,不该再当成"失败"报给用户。
                self.transfer_queue.cancel(id);
                continue;
            };
            let Some(conn) = tab.content.sftp_connection() else {
                self.transfer_queue.finish(id, Err("连接已断开".into()));
                continue;
            };
            let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
            self.transfer_cancels.insert(id, cancel.clone());
            // 冲突处置结果随 job 存在队列里(重跑时才知道该覆盖还是改名)。
            let resolved = self.transfer_queue.get(id).and_then(|j| j.resolved);
            let generation = spec.generation;
            let proxy = self.proxy.clone();
            let task = self._runtime.spawn(async move {
                let result = run_transfer(conn, spec, resolved, id, &proxy, &cancel).await;
                let _ = proxy.send_event(UserEvent::TransferDone { job: id, result });
            });
            self.track_sftp_task(generation, task);
        }
    }

    fn dispatch_panel_action_for(
        &mut self,
        generation: u64,
        column: crate::ui::files_panel::PanelColumn,
        action: crate::ui::files_panel::FileAction,
    ) {
        use crate::ui::files_panel::PanelColumn;
        match column {
            PanelColumn::Remote => self.apply_remote_file_action(generation, action),
            PanelColumn::Local => self.apply_local_file_action(generation, action),
        }
    }

    /// `↑`/`↓`:在有焦点的那一栏里移动 `selected`。纯 UI 状态,不经过
    /// `apply_*_file_action` 那条异步链路(不触发网络请求),直接改
    /// `PaneState` 的光标与选择集。没有选中项时,`↓` 从第一行开始、`↑` 从最后一
    /// 行开始——用户第一下按方向键该落在看得见的那一头,而不是无反应。
    fn move_panel_selection(&mut self, generation: u64, delta: i32) {
        let Some(tab) = self.tabs.by_generation_mut(generation) else {
            return;
        };
        let Some(state) = tab.content.files_panel_mut().map(|f| f.active_state_mut()) else {
            return;
        };
        let rows = state.rows();
        let Some(next) = next_panel_selection_index(&rows, state.cursor.as_ref(), delta) else {
            return;
        };
        let name = rows[next].name.clone();
        drop(rows);
        // 方向键 = 单选移动:光标走到哪,选择集就只剩哪一条(Shift+方向键
        // 扩选不在本切片范围内)。
        state.select_only(&name);
        self.ui_dirty = true;
    }

    /// F50/D6:sftp 任务开出去之后,把它的句柄存回属主标签的 `sftp_tasks`
    /// (`wind_down` 靠这个收口,见该字段的文档)。三处 spawn 点
    /// (`trigger_sftp_open`/`apply_remote_file_action`/`accept_sftp_opened`
    /// 的 `Ok` 分支)都要在拿到 `JoinHandle` 之后调用一次——抽成公共方法是
    /// 因为三处逻辑完全一样:先清掉已经跑完的旧句柄(避免无界增长),再把
    /// 新句柄推进去。
    ///
    /// 找不到属主标签(理论上不会发生:调用链上没有 `.await`,标签不可能在
    /// `spawn_*` 和这一步之间被摘掉)时直接 abort 这一个——没有标签收留它,
    /// 留着不管就是它自己文档里说的那种「无人收口」。D1:`Terminal`/`Files`
    /// 两种标签都走这一条,`TabContent::sftp_tasks_mut` 已经把「该记到哪个
    /// 字段」这件事收掉了。
    fn track_sftp_task(&mut self, generation: u64, task: tokio::task::JoinHandle<()>) {
        // F37:`sftp_tasks_mut()` 对占位标签是 `None` —— 它压根不会发起
        // sftp 请求,真走到这儿说明世代号错了,同样按「无人收留」处理。
        if let Some(tasks) = self
            .tabs
            .by_generation_mut(generation)
            .and_then(|t| t.content.sftp_tasks_mut())
        {
            tasks.retain(|h| !h.is_finished());
            tasks.push(task);
        } else {
            task.abort();
        }
    }

    /// F50/D6:首次要远端数据(或者上一次开失败、用户又点了一下)时,开一条
    /// sftp channel。结果经 `UserEvent::SftpOpened` 回来(`accept_sftp_opened`
    /// 接)。两种宿主取连接的来源不同(`TabContent::sftp_connection` 已经把
    /// 差异收掉):
    /// - `Terminal`(侧栏,D1 之前就有):蹭会话已建立的连接
    ///   (`SftpClient::open` 的签名里刻意没有网络参数),不重新握手。取
    ///   `hosts.first()` 而不是「聚焦 pane 那台」,前提是 ADR-009 下
    ///   `PaneState::host_ix` 目前**恒为 0**(一个 workspace 事实上只挂一条
    ///   连接),二者今天等价。等多主机分屏真落地,这里要跟着改成按聚焦 pane
    ///   取——否则侧栏会连到另一台机器上,而用户看不出来。
    /// - `Files`(D1 标签宿主):独占的连接(`establish` 单独建的那条,
    ///   ADR-010 同款理由),`sftp_connection` 恒 `Some`。
    fn trigger_sftp_open(&mut self, generation: u64) {
        let Some(tab) = self.tabs.by_generation_mut(generation) else {
            return;
        };
        // F37:占位标签没有面板,也就没有「远端目录」要加载 —— 直接不开。
        let Some(files) = tab.content.files_panel() else {
            return;
        };
        let already_loading = matches!(files.remote.load, crate::files::state::Load::Loading);
        if tab.content.sftp_client().is_some() || already_loading {
            // 已经开好了,或者已经在开的路上——别在下一帧/下一次点击重复触发。
            return;
        }
        let Some(conn) = tab.content.sftp_connection() else {
            return;
        };
        let default_remote = tab.content.sftp_default_remote();
        // ②:优先开在这个标签焦点 pane 报出来的目录。起始目录的计算
        // (`files_start_dir`)挪进了 `spawn_sftp_open`——`~` 展开要用远端的
        // 真登录目录,而那个值只有 `canonicalize(".")` 回来之后才知道,这里
        // 算不了(F123)。
        let pane_cwd = tab.content.focused_pane_cwd();
        if let Some(files) = tab.content.files_panel_mut() {
            files.remote.load = crate::files::state::Load::Loading;
        }
        let task = spawn_sftp_open(
            &self._runtime,
            &self.proxy,
            generation,
            conn,
            default_remote,
            pane_cwd,
        );
        self.track_sftp_task(generation, task);
    }

    /// S1:`UserEvent::SftpOpened` 按世代查属主标签,不用活动标签接——用户在
    /// 标签 A 开了侧栏、切到标签 B 的几百毫秒里这条抵达,拿活动标签接就会把
    /// A 的 client 挂到 B 上。
    fn accept_sftp_opened(
        &mut self,
        generation: u64,
        result: Result<
            (
                Arc<mullion_ssh::sftp::SftpClient>,
                mullion_ssh::sftp::RemotePath,
                mullion_ssh::sftp::RemotePath,
            ),
            String,
        >,
    ) {
        match result {
            Ok((client, home, dir)) => {
                let seq = {
                    let Some(tab) = self.tabs.by_generation_mut(generation) else {
                        log::debug!(target: "mullion", "丢弃过期世代 {generation} 的 SFTP 打开结果");
                        return;
                    };
                    let Some(slot) = tab.content.sftp_mut() else {
                        log::debug!(target: "mullion", "世代 {generation} 是占位标签,丢弃 SFTP 打开结果");
                        return;
                    };
                    *slot = Some(client.clone());
                    // F123:登录目录存下来,给「侧栏关→开跃迁」那条路展开 `~`。
                    tab.content.set_sftp_home(home);
                    let Some(files) = tab.content.files_panel_mut() else {
                        return;
                    };
                    files.remote.begin_load(dir.clone())
                };
                let task =
                    spawn_sftp_list_dir(&self._runtime, &self.proxy, generation, client, dir, seq);
                self.track_sftp_task(generation, task);
            }
            Err(msg) => {
                if let Some(tab) = self.tabs.by_generation_mut(generation) {
                    if let Some(files) = tab.content.files_panel_mut() {
                        files.remote.load = crate::files::state::Load::Failed(msg);
                    }
                } else {
                    log::debug!(target: "mullion", "丢弃过期世代 {generation} 的 SFTP 打开结果");
                }
            }
        }
        self.ui_dirty = true;
        self.request_ui_redraw();
    }

    /// S1:`UserEvent::SftpListed` 同样按世代查属主标签。`seq` 对不上
    /// (用户点得比网络快时的后发先至)丢弃 —— `Ok` 分支复用
    /// `PaneState::accept` 内部的判据,`Err` 分支手工复一份同款判据,理由
    /// 见下。
    fn accept_sftp_listed(
        &mut self,
        generation: u64,
        seq: u64,
        result: Result<Vec<mullion_ssh::sftp::Entry>, String>,
    ) {
        if let Some(tab) = self.tabs.by_generation_mut(generation) {
            if let Some(files) = tab.content.files_panel_mut() {
                let pane = &mut files.remote;
                match result {
                    Ok(entries) => {
                        pane.accept(seq, Ok(entries));
                    }
                    Err(msg) => {
                        // `PaneState::accept` 收到 `Err` 时恒落 `Load::Failed`,
                        // 这里要按分类分流到 `Disconnected`,所以不能直接复用
                        // 它——但 seq 判据(丢弃后发先至的旧结果)必须原样照做。
                        if seq == pane.request_seq {
                            pane.entries.clear();
                            // B3:连接级失败要转断开态(给重连入口),路径级
                            // 停在原地报一句。判据在纯函数里,可单测。
                            pane.load = match crate::files::fail::classify(&msg) {
                                crate::files::fail::FailKind::Session => {
                                    crate::files::state::Load::Disconnected
                                }
                                crate::files::fail::FailKind::Path => {
                                    crate::files::state::Load::Failed(msg)
                                }
                            };
                        }
                    }
                }
            }
            // B3 复核修订:转成 `Disconnected` 之后,死掉的 client **不能**
            // 继续留在槽位里。`trigger_sftp_open` 的既有短路守卫是
            // `sftp_client().is_some() || already_loading`——槽位不清空的话
            // 它会看见一个「死了但还在」的 client,直接 return 什么都不做,
            // 「重连」按钮变成死按钮。这正是 B3 要解决的场景(channel 用着
            // 用着死了),跟「从未开成功过」(那种情况下槽位本来就是 `None`,
            // 守卫本来就会放行)不是同一回事,不能假设它已经被别处清过。
            //
            // 顺带 abort 掉绑在同一条死连接上的在途 sftp 任务并清空任务表——
            // 它们攥着的 `Arc<SshConnection>` 不该在连接已经证实死亡之后
            // 还继续悬着等自己的网络往返超时(ADR-009 channel 泄漏那一类
            // 问题;同 `wind_down` 关标签时的收口纪律,只是这里收口的时机
            // 是「确认断连」而不是「标签关闭」)。
            let just_disconnected = tab
                .content
                .files_panel()
                .is_some_and(|f| matches!(f.remote.load, crate::files::state::Load::Disconnected));
            if just_disconnected {
                if let Some(slot) = tab.content.sftp_mut() {
                    *slot = None;
                }
                if let Some(tasks) = tab.content.sftp_tasks_mut() {
                    for t in tasks.drain(..) {
                        t.abort();
                    }
                }
            }
        } else {
            log::debug!(target: "mullion", "丢弃过期世代 {generation} 的目录列表(seq={seq})");
        }
        self.ui_dirty = true;
        self.request_ui_redraw();
    }

    /// UI 侧变了(或 egui 自己要重绘):标脏 + 请求一帧。**两件事必须一起做**——
    /// 只 `request_redraw` 而不标脏,那一帧会在 `frame_is_dirty` 处被判 Idle 丢掉。
    /// F100 标注模式的事件前置处理。返回 `true` = 这个事件已被标注模式吃掉,
    /// 调用方不要再往下分流。
    fn annotate_event(&mut self, event: &WindowEvent) -> bool {
        use crate::ui::annotate::{self, Hotkey};
        // `egui::Context` 是 `Arc` 内部可变,clone 是加一次引用计数 —— 这里 clone
        // 是为了先放掉对 `self.active` 的借用,下面才能改 `self.ui`。
        let Some(ctx) = self.active.as_ref().map(|a| a.egui_ctx.clone()) else {
            return false;
        };
        let on = annotate::is_on(&ctx);
        match event {
            WindowEvent::KeyboardInput { event: ke, .. } => {
                if ke.state != ElementState::Pressed {
                    return false;
                }
                let Some((key, mods)) = input::translate_key(ke, self.mods) else {
                    return false;
                };
                let Some(hk) = annotate::hotkey(key, mods, on) else {
                    return false;
                };
                match hk {
                    Hotkey::Toggle => {
                        let now = annotate::toggle(&ctx);
                        self.ui.set_toast(if now {
                            "标注模式:点选位置,Ctrl+Shift+E 导出"
                        } else {
                            "已退出标注模式"
                        });
                    }
                    Hotkey::Export => annotate::request_export(&ctx),
                    Hotkey::CycleDetail => {
                        annotate::cycle_detail(&ctx);
                    }
                    Hotkey::Exit => {
                        annotate::exit(&ctx);
                        self.ui.set_toast("已退出标注模式");
                    }
                }
                self.request_ui_redraw();
                true
            }
            // 悬停描边是我们自己画在 overlay 上的,不赌 egui 会替它请求重绘 ——
            // 不标脏的话描边会卡在上一帧的位置,看着像「鼠标不跟手」。
            // **不消费**:这个事件还要照旧喂给 egui 维护 hover。
            WindowEvent::CursorMoved { .. } if on => {
                self.ui_dirty = true;
                false
            }
            _ => false,
        }
    }

    fn request_ui_redraw(&mut self) {
        self.ui_dirty = true;
        if let Some(a) = &self.active {
            a.window.request_redraw();
        }
    }

    /// 本帧的 pane 几何。中央区 = egui 布局后剩下的矩形(`central_origin_px` +
    /// `central_px`),布局树按像素切分它。渲染、鼠标命中、window_change 三条
    /// 路径都读这一份结果——各算各的是这类布局 bug 的经典成因。
    fn compute_geoms(&self) -> Vec<PaneGeom> {
        let (Some(a), Some(ws)) = (self.active.as_ref(), self.active_ws()) else {
            return Vec::new();
        };
        let origin = self.ui.central_origin_px;
        let area = crate::shell::workspace::PxRect {
            x: origin.0.max(0.0) as u32,
            y: origin.1.max(0.0) as u32,
            w: self.ui.central_px.0,
            h: self.ui.central_px.1,
        };
        crate::shell::workspace::layout_geometry(
            ws.tree(),
            area,
            (a.text.cell_w, a.text.cell_h),
            ws.title_bars,
            a.window.scale_factor() as f32,
        )
    }

    /// 指针落在哪个 pane 上。命中判定用 `PaneGeom.px`(含标题条),
    /// 与渲染同源 —— 用别的矩形算就会出现"点得到但画不着"的错位。
    fn pane_at(&self, px: (f32, f32)) -> Option<PaneId> {
        let a = self.active.as_ref()?;
        a.geoms
            .iter()
            .find(|g| {
                let r = g.px;
                px.0 >= r.x as f32
                    && px.0 < (r.x + r.w) as f32
                    && px.1 >= r.y as f32
                    && px.1 < (r.y + r.h) as f32
            })
            .map(|g| g.id)
    }

    /// 焦点 pane 的几何。鼠标格换算、划选都基于它。
    fn focused_geom(&self) -> Option<PaneGeom> {
        let a = self.active.as_ref()?;
        let f = self.active_ws()?.focus();
        a.geoms.iter().find(|g| g.id == f).copied()
    }

    /// 指针相对**焦点 pane 终端区**左上角的像素。原点用 `term_px` 而不是中央区:
    /// 分屏后 pane 2 的第 0 列不在窗口左边,用中央区原点算会整体偏一个 pane 宽。
    fn cursor_in_grid(&self) -> (f32, f32) {
        let Some(g) = self.focused_geom() else {
            return (0.0, 0.0);
        };
        (
            self.cursor_px.0 - g.term_px.x as f32,
            self.cursor_px.1 - g.term_px.y as f32,
        )
    }

    /// 指针当前位置对应的 **0-based** viewport 单元格与格内左右半。
    ///
    /// `input::cell_at` 给的是 **1-based**(F17 鼠标上报的口径,SGR 协议要求),
    /// 而选区 API 收 0-based。两套口径并存是既有事实,换算**只在这一个函数里做**,
    /// 别让 0/1 混进事件循环——那是 off-by-one 最容易长出来的地方。
    fn selection_cursor(&self) -> Option<(u16, u16, mullion_term::selection::CellSide)> {
        let g = self.focused_geom()?;
        let a = self.active.as_ref()?;
        let cell_px = (a.text.cell_w, a.text.cell_h);
        let local = self.cursor_in_grid();
        let (col1, row1) = input::cell_at(local, cell_px, g.grid);
        let side = input::cell_side(local.0, cell_px.0, g.grid.0);
        Some((col1.saturating_sub(1), row1.saturating_sub(1), side))
    }

    /// 左键按下:判连击类型 → 开新选区(旧选区被覆盖)。
    fn selection_press(&mut self) {
        // 没有连接就没有终端可选,别让 `dragging` 在 launcher 态被置起来——
        // 那会让后续每次 `CursorMoved` 都白跑一遍划选和重绘。
        if self.active_ws().is_none() {
            return;
        }
        let Some(g) = self.focused_geom() else {
            return;
        };
        let Some(a) = self.active.as_ref() else {
            return;
        };
        let cell_px = (a.text.cell_w, a.text.cell_h);
        let pos1 = input::cell_at(self.cursor_in_grid(), cell_px, g.grid);
        let (kind, prev) = input::click_kind(self.prev_click, Instant::now(), pos1);
        self.prev_click = Some(prev);
        if let Some((col, row, side)) = self.selection_cursor() {
            self.press_anchor = Some(((col, row), kind));
            if let Some(pane) = self.active_ws_mut().and_then(Workspace::focused_mut) {
                pane.emulator.selection_start(col, row, kind, side);
            }
        }
        self.dragging = true;
        self.request_ui_redraw();
    }

    /// 更新选区终点 + 重算出界滚动量。**不请求重绘**:自动滚动那条路径要在
    /// present 之后调它,在那里 `request_redraw` 会与 `RedrawRequested` 互相触发,
    /// 绕开帧闸忙转(T3/T7)。需要重绘的调用方自己调 `request_ui_redraw`。
    fn update_selection_endpoint(&mut self) {
        let Some(a) = self.active.as_ref() else {
            return;
        };
        let cell_h = a.text.cell_h;
        // 判据取焦点 pane 的终端区,不是整窗 —— 见 `autoscroll_for_pane`。
        // 拿不到几何(还没排过版)时不滚,别拿窗口边界凑合。
        self.autoscroll = match self.focused_geom() {
            Some(g) => autoscroll_for_pane(self.cursor_px.1, g.term_px, cell_h),
            None => 0,
        };
        if let Some((col, row, side)) = self.selection_cursor() {
            if let Some(pane) = self.active_ws_mut().and_then(Workspace::focused_mut) {
                pane.emulator.selection_update(col, row, side);
            }
        }
    }

    /// 左键松开:选中即复制(PuTTY / Xshell 习惯,F18 交互口径)。
    ///
    /// 例外是「只是点了一下」:Simple 选区且指针从未离开按下的那一格。
    /// alacritty 的 `is_empty` 只在起止 **side** 也相同时才判空,手在按压瞬间
    /// 抖 1px 跨过半格线就会选出一个字符——点一下终端就把剪贴板覆盖掉,
    /// 是会真实困扰人的。双击选词 / 三击选行不受影响(kind 不是 Simple)。
    fn selection_release(&mut self) {
        // 只有配对过一次本地 `selection_press` 的释放才有资格动剪贴板。
        // 指针事件按 T8 的规则「先喂 egui 再判」,按下与释放是**各自独立**判路由的:
        // 在菜单上按下(判给 egui)、拖到终端区域松开(判给终端),就会走到这里而
        // `press_anchor` 是空的。那种情况下若无条件 `copy_selection`,会把仿真器里
        // 早先残留的选区静默写进剪贴板——用户毫无察觉,原有内容就没了。
        let was_dragging = self.dragging;
        self.dragging = false;
        self.autoscroll = 0;
        let anchor = self.press_anchor.take();
        if !was_dragging {
            return;
        }
        if let (Some((cell, mullion_term::selection::SelectionKind::Simple)), Some((col, row, _))) =
            (anchor, self.selection_cursor())
        {
            if cell == (col, row) {
                // 点一下 = 取消选择,别在屏幕上留一个孤零零的高亮字符。
                if let Some(pane) = self.active_ws_mut().and_then(Workspace::focused_mut) {
                    pane.emulator.selection_clear();
                }
                self.request_ui_redraw();
                return;
            }
        }
        self.copy_selection();
    }

    /// 告诉系统输入法候选框该贴在哪(焦点 pane 的光标格)。
    ///
    /// 只在位置**变了**才调:`set_ime_cursor_area` 是跨进程系统调用,每帧无脑
    /// 调与 T3 同一类问题。拿不到几何/光标不可见时不动 —— 保持上一次的位置
    /// 比把候选框弹回窗口角落好。
    fn apply_ime_cursor_area(&mut self) {
        let Some(g) = self.focused_geom() else { return };
        let Some(a) = self.active.as_ref() else {
            return;
        };
        let (cell_w, cell_h) = (a.text.cell_w, a.text.cell_h);
        let Some(cur) = self
            .active_ws()
            .and_then(Workspace::focused)
            .map(|p| p.emulator.cursor())
            .filter(|c| c.visible)
        else {
            return;
        };
        let area = ime_cursor_area(g.term_px, (cur.col, cur.row), cell_w, cell_h);
        if self.ime_cursor_area == Some(area) {
            return;
        }
        self.ime_cursor_area = Some(area);
        if let Some(a) = self.active.as_ref() {
            a.window.set_ime_cursor_area(
                winit::dpi::PhysicalPosition::new(area.0, area.1),
                winit::dpi::PhysicalSize::new(area.2, area.3),
            );
        }
    }

    /// 把当前选区写进系统剪贴板。无选区 = 什么都不做(`selection_text` 返回
    /// `None`),不能写空串——那会清掉用户剪贴板里原有的内容。
    fn copy_selection(&mut self) {
        let Some(text) = self
            .active_ws()
            .and_then(Workspace::focused)
            .and_then(|p| p.emulator.selection_text())
        else {
            return;
        };
        self.clipboard.set(&text);
    }

    /// 右键 / `Ctrl+Shift+V`:读剪贴板 → 判断要不要先确认 → 发送。
    fn request_paste(&mut self) {
        // 没有连接就没有地方可贴。不早退的话,launcher 态右键会读剪贴板、
        // 多行内容还会弹出一个「确认粘贴」窗——点了「粘贴」却什么都不会发生
        // (`send_paste` 拿不到焦点 pane 直接返回)。与 `selection_press` 同一道门。
        if self.active_ws().is_none() {
            return;
        }
        let Some(text) = self.clipboard.get() else {
            return;
        };
        if text.is_empty() {
            return;
        }
        let bracketed = self
            .active_ws()
            .and_then(Workspace::focused)
            .is_some_and(|p| {
                p.emulator
                    .mode()
                    .contains(mullion_term::TermMode::BRACKETED_PASTE)
            });
        // 判定与预览、与实际发送三者同源(`paste_line_count` 的 doc 说明了为什么):
        // `contains('\n')` 会把带尾随换行的单行命令(浏览器/IDE 复制的常态)
        // 误判成多行,而裸 `\r` 又会被漏掉。> 1 而非 != 0:单行也算「1 行」。
        if !bracketed && mullion_term::keymap::paste_line_count(&text) > 1 {
            self.pending_paste = Some(text);
            self.request_ui_redraw();
            return;
        }
        self.send_paste(&text);
    }

    /// 用户开始输入 ⟹ 自动化让位(设计 §2 的「用户接管优先」)。
    ///
    /// **只能从 `app.rs` 里用户意图的 PTY 写入点调。** `Workspace::pump` 里
    /// 那处 `p.pty.write(out)` 是 T1 的 PtyWrite 应答(DSR 光标查询、同步输出
    /// 探测的回应),不是用户输入——把取消挂在 `PtyWriter::write` 上,远端一发
    /// 同步输出探测自动化就自杀,而且现象是「有时候能跑有时候跑不了」。
    ///
    /// 将来新增用户输入路径(如鼠标按钮上报 F15)也必须一并接上这里。
    /// 当前的几处以 `grep -n "pty.write" crates/mullion-app/src/app.rs` 为准
    /// (行号会漂,别钉死);数量由
    /// `user_intent_write_points_all_yield_to_the_user` 钉住。
    fn user_took_over(&mut self) {
        let Some(t) = self.active_term_mut() else {
            return;
        };
        // **只掐焦点 pane 那一份**。分屏后每块 pane 各跑各的自动化
        // (见 `AutomationHandle::pane`):用户在左边敲字,不该把右边正在
        // 跑的登录后命令拦腰截断——那是两条互不相干的 shell。
        let focus = t.ws.focus();
        if let Some(h) = t.automation.iter_mut().find(|h| h.pane == focus) {
            // drop 发送端即取消(`write_scheduled` 的 doc:收到值**或**发送端
            // 被 drop 都算取消)。
            h.cancel.take();
        }
    }

    /// 真正发送。到这里要么不需要确认,要么用户已经点了「粘贴」。粘贴目标
    /// 是**焦点 pane**——分屏后粘贴永远只进当前正在操作的那一块。
    fn send_paste(&mut self, text: &str) {
        let Some(pane) = self.active_ws_mut().and_then(Workspace::focused_mut) else {
            return;
        };
        let bracketed = pane
            .emulator
            .mode()
            .contains(mullion_term::TermMode::BRACKETED_PASTE);
        let bytes = mullion_term::keymap::encode_paste(text, bracketed);
        // 与按键同理(F17):贴之前先回底部,否则「贴了但看不到」。
        pane.emulator.scroll_to_bottom();
        let _ = pane.pty.write(bytes);
        // `pane` 的借用到此结束,才能再借 `&mut self`。
        self.user_took_over();
    }

    /// 从 Minimized 自愈:凡是「窗口本该看得见」的信号都拿实测尺寸复查一次,
    /// 别指望对方一定会补发非零 `Resized`(理由见 `shell::window_state`)。
    fn recheck_visibility(&mut self) {
        let Some(a) = &self.active else { return };
        let size = a.window.inner_size();
        let Some(vis) =
            shell::window_state::recover_from_minimized(self.visible, size.width, size.height)
        else {
            return;
        };
        crate::logx::line(&format!(
            "窗口可见性自愈 {:?} → {:?}({}x{})",
            self.visible, vis, size.width, size.height
        ));
        // 走与还原同一条路径:最小化期间跳过的 surface configure 都要补上。
        self.apply_resize(size.width, size.height);
    }

    /// 应用一次窗口尺寸变化(`Resized` 事件与 Minimized 自愈共用)。
    ///
    /// 每 pane 的列/行数**不在这里算**:统一由 `RedrawRequested` 里的
    /// `Present` 分支每帧 `compute_geoms()` + `Workspace::apply_geometry`
    /// 施加(F34/T4 唯一出口,三条触发路径——resize/切预设/关 pane/标题条开关
    /// 都收敛到那一个函数)。这里只需要保证 present 分支能跑到最小化态之外的
    /// 尺寸变化都会被下一帧的 compute_geoms 自动捕捉,不需要 resize 事件
    /// 单独再推一次;最小化(0×0)时 `RedrawScope::PumpOnly` 已经在
    /// `RedrawRequested` 里更早的地方整帧跳过,不会走到 compute_geoms。
    fn apply_resize(&mut self, width: u32, height: u32) {
        let vis = shell::window_state::visibility_for(width, height);
        if vis != self.visible {
            crate::logx::line(&format!(
                "窗口可见性 {:?} → {:?}({width}x{height})",
                self.visible, vis
            ));
            self.visible = vis;
        }
        let plan = shell::window_state::plan_resize(vis);
        let Some(a) = &mut self.active else { return };
        // 最小化(0×0)不 configure 0 面积表面。
        if plan.reconfigure_surface {
            a.gpu.resize(width, height);
        }
        if plan.request_redraw {
            self.request_ui_redraw();
        }
    }

    /// 排空每个 pane 的 rx → feed 各自的 emulator → 回写各自的 `PtyWrite`(T1 红线)。
    ///
    /// 从 `RedrawRequested` 和(最小化时)`UserEvent::Wake` 两处调:最小化期间窗口
    /// 未必还会被重绘,不能把这条通路挂在重绘上,否则有界 rx(256)灌满堵住 io_task,
    /// 远端的同步输出探测/光标查询永久等不到应答。
    fn pump_io(&mut self) {
        let now = self.now_ms();
        let mut flushed = false;
        if let Some(ws) = self.active_ws_mut() {
            ws.pump(now);
            // T2:超时的同步块要就地收口 —— 字节还压在 vte 的 `Processor` 里,
            // `Term` 根本没见过,再怎么出帧也只是旧画面。
            // 见 `Emulator::flush_expired_sync`。
            flushed = ws.flush_expired_vt_sync(Instant::now());
        }
        if flushed {
            // 收口出来的字节没经过 `pacer.feed`,`panes_ready_to_present` 判不出
            // 脏;不标脏这一帧会被判 Idle,收口等于白做。
            self.ui_dirty = true;
        }
        self.drive_automation();
    }

    /// 首字节 / 断线两条边。挂在 `pump_io` 上而不是重绘上:最小化期间窗口
    /// 未必还会被重绘,但 `Wake` 仍会驱动 `pump_io`——否则用户最小化着连上,
    /// 自动化会一直等到超时。
    ///
    /// 每帧调,所以**零分配**:只读两个 bool、`take()` 两个 `Option`。
    /// 起一份自动化:建三条一次性通道、spawn 那个 task、把 handle 挂到属主标签。
    ///
    /// 两个调用点(`ConnectOk` 的第一个 pane、`PaneOpened` 的后来者)必须共用
    /// 这一处 —— 分开写的话「结束时回送 `AutomationDone` 要带上 `PaneId`」这类
    /// 约束就得在两个地方各维护一遍,漏一处的现象是那份自动化永远摘不掉,
    /// 状态栏的「进行中」一直挂着。
    ///
    /// `sink` 收 `Arc<SshSession>`:自动化 task 与 pane 同时持有同一条 channel
    /// 的写口(见 `PtyWriter for Arc<SshSession>` 的文档)。
    fn start_automation(
        &mut self,
        generation: u64,
        pane: PaneId,
        plan: crate::automation::PendingAutomation,
        sink: Arc<mullion_ssh::session::SshSession>,
    ) {
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        let (disc_tx, disc_rx) = tokio::sync::oneshot::channel();
        let sink: Arc<dyn mullion_ssh::schedule::ByteSink> = sink;
        let proxy = self.proxy.clone();
        let steps = plan.steps.len();
        let timeout_ms = plan.ready_timeout_ms;
        log::info!(
            target: "mullion",
            "自动化:pane {} 有 {steps} 步待发,就绪超时 {timeout_ms}ms",
            pane.0
        );
        let task = self._runtime.spawn(async move {
            let outcome =
                crate::automation::run(sink, plan.steps, ready_rx, cancel_rx, disc_rx, timeout_ms)
                    .await;
            let _ = proxy.send_event(UserEvent::AutomationDone(generation, pane, outcome));
        });
        if let Some(t) = self
            .tabs
            .by_generation_mut(generation)
            .and_then(|tab| tab.content.as_terminal_mut())
        {
            // 同一个 pane 不该同时挂两份(换节点会走到这里第二次):先摘掉旧的,
            // 否则 `drive_automation` 会把首字节喂给两份,而 `user_took_over`
            // 只取消得掉其中一份。
            if let Some(old) = t.automation.iter().position(|h| h.pane == pane) {
                t.automation.swap_remove(old).task.abort();
            }
            t.automation.push(AutomationHandle {
                pane,
                ready: Some(ready_tx),
                cancel: Some(cancel_tx),
                disconnect: Some(disc_tx),
                task,
            });
        } else {
            // 属主标签在这几行之间没了(理论上到不了:同一帧同步执行)。
            // 不 abort 的话那个 task 会一直等到超时,还攥着一份 channel 写口。
            log::warn!(target: "mullion", "自动化起好时属主标签(世代 {generation})已不在,丢弃");
            task.abort();
        }
    }

    fn drive_automation(&mut self) {
        // **遍历所有标签,不只是活动标签**:用户完全可能连上标签 A(自动化正在
        // 等首字节)就切到标签 B 去,只驱动活动标签的话 A 那次会一直等到超时。
        // 单标签时与原先逐帧等价。
        //
        // 两条边都读**自己 workspace 里的** pane —— `TerminalTab` 把 handle 和
        // workspace 绑在一起,结构上拿不错对。
        for tab in self.tabs.iter_mut() {
            let Some(t) = tab.content.as_terminal_mut() else {
                continue;
            };
            // 每块 pane 各有一份(分屏新开的也跑,见 `AutomationHandle::pane`)。
            // `ws` 与 `automation` 是同一个结构体的两个字段,分别借用合法。
            let (ws, handles) = (&t.ws, &mut t.automation);
            for h in handles.iter_mut() {
                // pane 不在了(被关掉/换世代):让 task 自然结束,别让它挂到超时。
                let Some(pane) = ws.pane(h.pane) else {
                    h.disconnect.take();
                    continue;
                };
                if pane.status == crate::shell::workspace::PaneStatus::Disconnected {
                    // send 的 Err(接收端已走)无所谓:task 已经结束了。
                    if let Some(tx) = h.disconnect.take() {
                        let _ = tx.send(());
                    }
                    continue;
                }
                if pane.saw_first_byte {
                    if let Some(tx) = h.ready.take() {
                        let _ = tx.send(());
                    }
                }
            }
        }
    }

    /// 重算会话外观缓存(F61/F62)。
    ///
    /// **每一处改动了会话或分组的地方都必须调它。** 漏掉一处的症状是:用户改了
    /// 颜色、保存,列表和 pane 标题条却还是旧色,直到重启才更新——一个没有报错、
    /// 只是「看起来没生效」的 bug,最难查。
    ///
    /// 反过来也不能图省事每帧调:`inherit::resolve` 的文档注释点名了陷阱 T3
    /// (喂数据和重绘没解耦),会话列表每帧几十行,逐行解析就是每秒几千次。
    fn refresh_appearance(&mut self) {
        match self.store.as_ref() {
            Some(s) => {
                // `list()` 而不是 `sessions()`——store 的会话访问器叫 `list`。
                self.appearance.rebuild(s.list(), s.groups());
            }
            None => self.appearance.rebuild(&[], &[]),
        }
    }

    /// 在 `_runtime` 上异步连接;结果经 `proxy` 以 `UserEvent` 回送(§5)。
    /// 不阻塞调用方(winit 事件循环线程)。拆成 `establish` + `open_pty` 两步
    /// (而不是直接调更省事的 `session::connect`):分屏(F35)要在同一条连接上
    /// 另开 channel,必须拿到 `establish` 返回的 `Handle` 本身——`connect` 内部
    /// 会把它吞掉不外露。
    ///
    /// D1/F50:`wants_sftp` 由调用方(点击那一刻)算好传入——`ConnectOk` 不带
    /// `SessionId`,没法在收到结果时回头再查协议字段。为 `true` 时**跳过
    /// `open_pty`**:SFTP 节点没有 PTY 这个概念,`establish` 一成功就直接
    /// 回送(`pty: None`),不做那趟本来就用不上的 shell 握手。
    fn spawn_connect(&mut self, cfg: SshConfig, wants_sftp: bool) {
        // F40~F44:此刻才确定「是哪条会话」。连接在途期间用户可能改配置甚至
        // 删会话,所以计划必须在用户点击的这一帧定死。
        // 上一次的结论到此为止:新连接开始了,旧结论就是误导信息。
        // (Task 2 保持替换语义,清的就是马上要被替换掉的那个标签。)
        if let Some(t) = self.active_term_mut() {
            t.automation_status = None;
        }
        // 同一次解析里顺手留一份**原件**:后来的 pane(分屏新开的、换过节点的)
        // 要按它算「跳过 tmux」的计划。绝不等到分屏那一刻再查库——那时用户可能
        // 已经改了配置(见 `pending_automation_template` 的文档)。
        let mut tpl: Option<mullion_store::ResolvedAutomation> = None;
        let plan = crate::automation::pending_for(self.ui.connect_request_last, |id| {
            let store = self.store.as_ref()?;
            let resolved = store.resolved(id).ok()?;
            // `ResolvedConfig` 不含会话名,而 `build_plan` 要它做 tmux 的
            // fallback_name(用户没填 tmux 会话名时按会话名推导)。
            let name = store
                .list()
                .iter()
                .find(|r| r.id == id)?
                .identity
                .name
                .clone();
            tpl = Some(resolved.automation.clone());
            Some((resolved.automation, name))
        });
        self.pending_automation = plan;
        self.pending_automation_template = tpl;
        // 会话管理器发起的连接也要记下,否则第二次连接后开分屏会用上一台
        // 主机的 term/尺寸(F35 的 open_pty 靠它)。`ConnectOk` 抵达时移交给
        // 新建的标签。
        self.pending_cfg = Some(cfg.clone());
        let proxy = self.proxy.clone();
        let wake_proxy = self.proxy.clone();
        // 每次连接现建一个策略:它只持有两个 Arc/Sender 的克隆,构造成本可忽略,
        // 换来 App 不必长期持有一个 dyn 对象。
        let policy: Arc<dyn HostKeyPolicy> = Arc::new(crate::host_key::PromptingPolicy::new(
            self.known_hosts.clone(),
            self.proxy.clone(),
            true,
        ));
        self._runtime.spawn(async move {
            let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                let _ = wake_proxy.send_event(UserEvent::Wake);
            });
            let handle = match mullion_ssh::session::establish(&cfg, policy).await {
                Ok(h) => Arc::new(h),
                Err(e) => {
                    let _ = proxy.send_event(UserEvent::ConnectErr(e.to_string()));
                    return;
                }
            };
            if wants_sftp {
                let _ = proxy.send_event(UserEvent::ConnectOk {
                    handle,
                    wants_sftp: true,
                    pty: None,
                });
                return;
            }
            match mullion_ssh::session::open_pty(handle.clone(), &cfg, wake).await {
                Ok((ssh, rx)) => {
                    let _ = proxy.send_event(UserEvent::ConnectOk {
                        handle,
                        wants_sftp: false,
                        pty: Some((ssh, rx)),
                    });
                }
                Err(e) => {
                    let _ = proxy.send_event(UserEvent::ConnectErr(e.to_string()));
                }
            }
        });
    }

    /// F35 分屏复用连接:给 `fresh`(树上已占好叶子位、还没有 `PaneState`)里的
    /// 每个 id,在同一条 SSH 连接上另开一条 channel。真正决定"该不该开、开
    /// 哪些"的路由逻辑在自由函数 `apply_layout_actions`(可脱离 runtime/proxy
    /// 单测,见其文档注释);这里只管执行,天然依赖 `self._runtime`/`self.proxy`,
    /// 无头环境测不了,只能人工验收(F35 的实际 channel 复用效果)。
    fn spawn_fresh_panes(&mut self, fresh: Vec<PaneId>) {
        if fresh.is_empty() {
            return;
        }
        // **取活动标签自己的 cfg**,不是「最近一次连接的」—— 否则在标签 A 上
        // 分屏会拿标签 B 的 term/尺寸去开 channel(S2 把 last_cfg 搬进标签的理由)。
        let Some(t) = self.active_term() else { return };
        let ws = &t.ws;
        let Some(host) = ws.hosts.first() else { return };
        // C1:开 channel 是异步的,回来时用户可能已经断开重连、换了一个新
        // `Workspace`(`next_id` 重新从 2 计数,`id` 会撞号)。把发起时刻的
        // 世代一起带走,`PaneOpened`/`PaneOpenErr` 抵达时据此判断"这事件还
        // 是不是当前这个 Workspace 发出的"。
        let generation = ws.generation();
        let handle = host.handle.clone();
        let Some(cfg) = t.last_cfg.clone() else {
            return;
        };
        for id in fresh {
            let handle = handle.clone();
            let cfg = cfg.clone();
            let proxy = self.proxy.clone();
            let wake_proxy = self.proxy.clone();
            self._runtime.spawn(async move {
                let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                    let _ = wake_proxy.send_event(UserEvent::Wake);
                });
                match mullion_ssh::session::open_pty(handle, &cfg, wake).await {
                    Ok((ssh, rx)) => {
                        let _ = proxy.send_event(UserEvent::PaneOpened {
                            id,
                            ssh,
                            rx,
                            generation,
                        });
                    }
                    Err(e) => {
                        let _ = proxy.send_event(UserEvent::PaneOpenErr {
                            id,
                            msg: format!("开分屏失败: {e}"),
                            generation,
                        });
                    }
                }
            });
        }
    }

    /// 换节点(用户报的问题 2):把一块已有的 pane 挂到另一条会话上。
    ///
    /// **另起一条自己的 SSH 连接**,不蹭这个标签已有的那条 —— 新节点是另一台
    /// 机器,复用无从谈起(与 F35 分屏「同机器多 channel」正相反)。
    ///
    /// 与 `spawn_connect` 的分工:那条走完会**开一个新标签**,这条只换一块
    /// pane 的挂靠。共用同一个 `establish` + `open_pty` 两步拆分,不共用事件。
    fn spawn_rehost(&mut self, pane: PaneId, session: mullion_store::SessionId) {
        let Some(generation) = self.active_term().map(|t| t.ws.generation()) else {
            // 活动标签不是终端(文件标签 / launcher)。到不了:换节点的入口是
            // pane 标题条,而标题条只有终端标签才画。
            log::warn!(target: "mullion", "换节点:活动标签不是终端,忽略");
            return;
        };
        let Some(store) = self.store.as_ref() else {
            self.ui.set_error("配置库不可用,无法换节点".to_string());
            return;
        };
        let (cfg, wants_sftp) = match store.dial_plan_for(session) {
            Ok(v) => v,
            Err(e) => {
                self.ui.set_error(e.to_string());
                return;
            }
        };
        if wants_sftp {
            // 弹窗已经把 SFTP 节点滤掉了(`ui::rehost::visible`),这里是第二道:
            // 真挂过去会得到一块永远不出字的黑屏,而用户只会觉得「换节点坏了」。
            self.ui
                .set_error("这是 SFTP 节点,没有终端,不能换到它上面".to_string());
            return;
        }
        // 名字/地址/自动化都在**这一帧**定死。理由见 `PendingRehost` 的文档。
        let label = store
            .list()
            .iter()
            .find(|r| r.id == session)
            .map_or_else(|| cfg.host.clone(), |r| r.identity.name.clone());
        let plan = store
            .resolved(session)
            .ok()
            .and_then(|r| crate::automation::pending_for_extra_pane(&r.automation));
        self.pending_rehost.push(PendingRehost {
            generation,
            pane,
            label,
            addr: format!("{}:{}", cfg.host, cfg.port),
            session_id: session,
            plan,
        });
        let proxy = self.proxy.clone();
        let wake_proxy = self.proxy.clone();
        let policy: Arc<dyn HostKeyPolicy> = Arc::new(crate::host_key::PromptingPolicy::new(
            self.known_hosts.clone(),
            self.proxy.clone(),
            true,
        ));
        self._runtime.spawn(async move {
            let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                let _ = wake_proxy.send_event(UserEvent::Wake);
            });
            let handle = match mullion_ssh::session::establish(&cfg, policy).await {
                Ok(h) => Arc::new(h),
                Err(e) => {
                    let _ = proxy.send_event(UserEvent::PaneRehostErr {
                        generation,
                        pane,
                        msg: format!("换节点失败: {e}"),
                    });
                    return;
                }
            };
            match mullion_ssh::session::open_pty(handle.clone(), &cfg, wake).await {
                Ok((ssh, rx)) => {
                    let _ = proxy.send_event(UserEvent::PaneRehosted {
                        generation,
                        pane,
                        handle,
                        ssh,
                        rx,
                    });
                }
                Err(e) => {
                    let _ = proxy.send_event(UserEvent::PaneRehostErr {
                        generation,
                        pane,
                        msg: format!("换节点失败: {e}"),
                    });
                }
            }
        });
    }

    /// F92:拨一次完整认证后立刻断开。**不开 channel、不起 pty** ——
    /// 拨测只回答「这条链路加上这份凭据能不能登上去」。
    fn spawn_probe(&mut self, cfg: SshConfig) {
        /// 拨测超时。比正常连接短:用户是站在弹窗前等结果的,
        /// 超过这个数就该告诉他「不通」,而不是继续转圈。
        const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

        self.probe_epoch = self.probe_epoch.wrapping_add(1);
        let epoch = self.probe_epoch;
        let proxy = self.proxy.clone();
        // persist=false:拨测遇到未知指纹只信任本次,绝不写 known_hosts。
        let policy: Arc<dyn HostKeyPolicy> = Arc::new(crate::host_key::PromptingPolicy::new(
            self.known_hosts.clone(),
            self.proxy.clone(),
            false,
        ));
        let h = self._runtime.spawn(async move {
            let ev = match tokio::time::timeout(
                PROBE_TIMEOUT,
                mullion_ssh::session::establish(&cfg, policy),
            )
            .await
            {
                Err(_) => UserEvent::ProbeErr(epoch, "超时(20s):链路不通或对端无响应".to_owned()),
                Ok(Err(e)) => UserEvent::ProbeErr(epoch, e.to_string()),
                Ok(Ok(c)) => {
                    // russh 的 Drop 不发 disconnect,必须显式断(见 SshConnection::disconnect)。
                    c.disconnect().await;
                    UserEvent::ProbeOk(epoch)
                }
            };
            let _ = proxy.send_event(ev);
        });
        // 覆盖前先 abort 上一个:用户连点两次「测试连接」不该留下孤儿任务。
        if let Some(old) = self.probe_task.replace(h) {
            old.abort();
        }
    }

    /// 按当前表单(含未保存改动)组拨测用的 SshConfig。
    ///
    /// 凭据三态合成必须带上库里的旧值 —— 编辑已有会话时用户多半没重输
    /// 密码,`build_draft` 自己看不到 store,合成出来是 None,拨测会误报
    /// 「缺少凭据」。这一步和 `apply_save` 做的是同一件事。
    fn build_probe_config(&self) -> Result<SshConfig, String> {
        let buf = self.ui.editor.as_ref().ok_or("没有正在编辑的会话")?;
        let mut draft = crate::ui::session_manager::build_draft(buf)?;
        let existing = self
            .ui
            .editor_id
            .and_then(|id| self.store.as_ref().and_then(|s| s.secret(id)));
        let (pw, pp, proxy, key) = crate::ui::session_manager::secret_fields(buf);
        draft.secret = crate::ui::session_manager::merge_secret(existing, &pw, &pp, &proxy, &key);
        // 复用既有的 `sync_has_passphrase`(`apply_save` 用的是同一个),
        // 不要在这里手写第二份 `AuthKind::PublicKey { has_passphrase }` 同步逻辑 ——
        // 两份迟早漂移,而漂移的后果是「测试通过、保存后要不到口令」。
        let merged = draft.secret.clone();
        crate::ui::session_manager::sync_has_passphrase(&mut draft, merged.as_ref());
        let store = self.store.as_ref().ok_or("会话库不可用")?;
        store
            .ssh_config_for_draft(&draft)
            .map_err(|e| e.to_string())
    }

    /// 文件对话框(私钥 / 图标共用):另起线程跑,结果经 `proxy` 回送。
    ///
    /// 两点都是必需的:
    /// 1. **不在事件回调里同步调 `pick_file()`**。egui 闭包跑在 `RedrawRequested`
    ///    中间,一阻塞就是整个事件循环停摆——IO 泵不动(T1)、窗口不重绘、
    ///    看门狗只能报「卡在 window_event」。用户看到的就是「卡死」。
    /// 2. **`set_parent`**。不给 owner 的对话框在 Windows 上可能被排到主窗口
    ///    后面,前台还是那个不响应的主窗(它已被 owner 关系禁用)——同样表现为卡死。
    ///
    /// `rfd::FileDialog` 自身 `unsafe impl Send`(内部只存 raw handle),
    /// 跨线程用 owner 句柄正是 rfd `AsyncFileDialog` 内部的做法。
    fn spawn_file_picker(
        &self,
        title: &str,
        filter: Option<(&str, &[&str])>,
        dir: Option<PathBuf>,
        done: fn(Option<PathBuf>) -> UserEvent,
    ) {
        let mut dialog = rfd::FileDialog::new().set_title(title);
        if let Some((name, exts)) = filter {
            dialog = dialog.add_filter(name, exts);
        }
        // 目录不存在就别设 —— 有的实现会把不存在的路径当成「打开失败」。
        if let Some(d) = dir.filter(|d| d.is_dir()) {
            dialog = dialog.set_directory(d);
        }
        if let Some(a) = &self.active {
            dialog = dialog.set_parent(a.window.as_ref());
        }
        let proxy = self.proxy.clone();
        let spawned = std::thread::Builder::new()
            .name("mullion-file-dialog".into())
            .spawn(move || {
                let _ = proxy.send_event(done(dialog.pick_file()));
            });
        if let Err(e) = spawned {
            // 起不了线程就退回「没选中」,让 busy 标志复位,UI 不会卡在按不动。
            log::warn!(target: "mullion", "文件对话框线程创建失败: {e}");
            let _ = self.proxy.send_event(done(None));
        }
    }

    /// 自动化结束。**必须按世代过滤**:高延迟链路下用户完全可能在自动化还在
    /// 跑的时候断开重连,旧世代的「自动化已中止:连接已断开」落到新连接的
    /// 状态栏上,是一条与当前连接毫不相干的误导信息(判据同 `PaneOpenErr`)。
    fn accept_automation_done(
        &mut self,
        generation: u64,
        pane: PaneId,
        outcome: crate::automation::Outcome,
    ) {
        // S1:结论回给**属主标签**,不是活动标签 —— 后者会让「在 A 上跑的自动化
        // 结论」显示成 B 的状态。
        let Some(t) = self
            .tabs
            .by_generation_mut(generation)
            .and_then(|tab| tab.content.as_terminal_mut())
        else {
            log::debug!(target: "mullion", "丢弃过期世代 {generation} 的自动化结论");
            return;
        };
        log::info!(target: "mullion", "自动化结束(pane {}): {outcome:?}", pane.0);
        t.automation_status = Some(crate::automation::status_text(outcome));
        // **只摘掉报信的那一份**。一个标签可以同时跑好几份(每块 pane 一份),
        // 整个清空会让还在跑的那些永远摘不掉:状态栏的「进行中」提前消失,
        // `user_took_over` 也再取消不了它们。
        if let Some(ix) = t.automation.iter().position(|h| h.pane == pane) {
            t.automation.swap_remove(ix);
        }
        self.ui_dirty = true;
        self.request_ui_redraw();
    }

    /// F111/F112/F113:按一条隧道记录起一条转发。
    ///
    /// `-L`/`-D` 的顺序是**先占本机端口、再建 SSH**(设计 S1):端口被占用
    /// 0ms 就能发现,先烧一次完整建链(高延迟代理链路上好几秒)才告诉用户
    /// 「端口被占了」是纯粹的浪费;而且 listener 归监管任务持有、跨重连不
    /// 释放,重连期间端口不会被别的程序抢走。
    ///
    /// `-R` 占的是**远端**的端口,S1 在那类上无从谈起 —— 冲突只能等服务端
    /// 拒绝(`RemoteForwardDenied`,同样是致命错误,不进退避)。
    fn start_tunnel(&mut self, id: mullion_store::TunnelId) {
        use mullion_store::TunnelKind;
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let Some(rec) = store.tunnels().iter().find(|t| t.id == id).cloned() else {
            return;
        };
        // 隧道**独占**自己的 SSH 连接(ADR-010),所以这里跟点「连接」走的是
        // 同一个 `ssh_config_for` —— 代理、跳板链一并解析,悬垂引用直接报错。
        let cfg = match store.ssh_config_for(rec.session_id) {
            Ok(c) => c,
            Err(e) => {
                self.ui.set_error(e.to_string());
                return;
            }
        };
        // 本机 listener 只有 `-L`/`-D` 要。`block_on` 一次 bind:目标是
        // `SocketAddr`,没有 DNS 往返,微秒级返回,换来的是「端口占用」这条
        // 错误能当场落进 `last_error`,而不是变成一条需要用户回头去状态栏
        // 找的异步失败。
        let bind_local = |expose: bool| {
            self._runtime.block_on(mullion_ssh::tunnel::bind_listener(
                mullion_ssh::tunnel::bind_addr(expose, rec.listen_port),
            ))
        };
        let forward = match rec.kind.clone() {
            TunnelKind::Local {
                target_host,
                target_port,
                expose,
            } => match bind_local(expose) {
                Ok(listener) => mullion_ssh::tunnel::Forward::Local {
                    listener,
                    target: (target_host, target_port),
                },
                Err(e) => {
                    self.ui.set_error(e.to_string());
                    return;
                }
            },
            // `-D` **恒绑回环**(设计 D5):开放的无认证 SOCKS5 代理与
            // 「暴露一个已知目标」是两类风险,`TunnelKind::Dynamic` 在类型上
            // 就没有 `expose` 字段。
            TunnelKind::Dynamic => match bind_local(false) {
                Ok(listener) => mullion_ssh::tunnel::Forward::Dynamic { listener },
                Err(e) => {
                    self.ui.set_error(e.to_string());
                    return;
                }
            },
            TunnelKind::Remote {
                target_host,
                target_port,
                expose,
            } => mullion_ssh::tunnel::Forward::Remote {
                bind: mullion_ssh::tunnel::remote_bind_address(expose),
                port: rec.listen_port,
                target: (target_host, target_port),
            },
        };
        // 首连允许弹 TOFU 窗;重连只信已知主机(设计 S3)。后者用的
        // `TrustedOnlyPolicy` **在类型上**就没有 `EventLoopProxy`,不可能弹窗
        // —— 半夜自动重连时弹一个没人看的模态框,等于隧道静默死掉。
        let first: Arc<dyn mullion_ssh::known_hosts::HostKeyPolicy> =
            Arc::new(crate::host_key::PromptingPolicy::new(
                self.known_hosts.clone(),
                self.proxy.clone(),
                true,
            ));
        let retry: Arc<dyn mullion_ssh::known_hosts::HostKeyPolicy> = Arc::new(
            crate::host_key::TrustedOnlyPolicy::new(self.known_hosts.clone()),
        );
        let proxy = self.proxy.clone();
        let sink: mullion_ssh::tunnel::StateSink = Arc::new(move |state| {
            let _ = proxy.send_event(UserEvent::TunnelState { id, state });
        });
        let handle = {
            // `spawn_tunnel` 内部是 `tokio::spawn`,要 runtime 上下文;
            // GUI 线程不在 runtime 里,得显式进去一趟。
            let _guard = self._runtime.enter();
            mullion_ssh::tunnel::spawn_tunnel(
                forward,
                mullion_ssh::tunnel::TunnelDial {
                    cfg,
                    first_policy: first,
                    retry_policy: retry,
                },
                sink,
            )
        };
        self.tunnels.insert(id, handle);
        self.ui.set_toast("隧道启动中…");
    }

    /// F114/F115:收下一次隧道状态上报。
    ///
    /// toast **只在跃迁到失败时弹一次**,不是「只要当前是失败就弹」——
    /// 后者会在此后每一次状态上报(2 秒一次的健康探测也会走这条路)时
    /// 重新弹一遍,把用户正在看的东西一直盖住。跃迁判据来自
    /// `TunnelRuntime::set_state` 返回的**前一个**状态。
    fn accept_tunnel_state(
        &mut self,
        id: mullion_store::TunnelId,
        state: mullion_ssh::tunnel::TunnelState,
    ) {
        let Some(prev) = self.tunnels.set_state(id, state.clone()) else {
            // 已经被停掉/删掉了,这是一条在途旧消息。
            return;
        };
        // 播不播由纯函数定(它有自己的守护测试),这里只负责组文案。
        if let Some(cause) = crate::tunnels::failure_announcement(&prev, &state) {
            let title = self
                .store
                .as_ref()
                .and_then(|s| s.tunnels().iter().find(|t| t.id == id))
                .map(crate::ui::session_manager::tunnel_list::row_title)
                .unwrap_or_else(|| format!("隧道 {}", id.0));
            self.ui.set_toast(format!("{title} 已停止:{cause}"));
        }
        self.ui_dirty = true;
        self.request_ui_redraw();
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.active.is_some() {
            return;
        }
        // adapter 枚举 / request_device 是阻塞调用,显卡驱动出问题时会卡死在这里——
        // 打上阶段标记,看门狗才说得出「卡在 startup」而不是一片空白。
        diag::mark(diag::Stage::Startup);
        // F37:**建窗口之前**就把上次的布局读出来 —— 几何要写进
        // `WindowAttributes`,建完再 `set_outer_position` 会让用户看见窗口
        // 先在默认位置闪一下再跳过去。
        //
        // `layout::load` 没有 `Result`(设计 E6):布局文件坏了的正确表现是
        // 「这次没恢复」,而不是「打不开客户端」。这里的 `unwrap_or_else`
        // 只对付「连配置目录都定位不到」。
        let saved_layout = crate::shell::store::config_dir()
            .map(|d| mullion_store::layout::load(&d))
            .map(|l| {
                if let Some(note) = l.note {
                    crate::logx::line(&format!("F37:{note}"));
                }
                l.layout
            })
            .unwrap_or_else(mullion_store::SavedLayout::empty);
        let mut attrs = Window::default_attributes().with_title("mullion");
        if let Some(w) = saved_layout.window {
            // 夹紧判据全在 `layout_snapshot::clamp_to_monitors` 里(纯函数,
            // 有守护测试)—— 拔掉一块显示器之后,上次那个位置可能已经在
            // 所有屏幕之外,窗口会恢复到用户根本看不见的地方。
            let monitors: Vec<crate::shell::layout_snapshot::MonitorRect> = event_loop
                .available_monitors()
                .map(|m| {
                    let p = m.position();
                    let s = m.size();
                    crate::shell::layout_snapshot::MonitorRect {
                        x: p.x as f32,
                        y: p.y as f32,
                        width: s.width as f32,
                        height: s.height as f32,
                    }
                })
                .collect();
            let r = crate::shell::layout_snapshot::clamp_to_monitors(w, &monitors);
            attrs = attrs
                .with_inner_size(winit::dpi::PhysicalSize::new(r.width, r.height))
                .with_maximized(r.maximized);
            if let Some((x, y)) = r.pos {
                attrs = attrs.with_position(winit::dpi::PhysicalPosition::new(x, y));
            }
        }
        let window = Arc::new(event_loop.create_window(attrs).expect("create_window"));
        // 输入法:winit **默认不发** `WindowEvent::Ime`,不打开这个开关的话中文/
        // 日文输入法一个字都递不进来(用户报的「ssh 连接后不能输入汉字」)。
        // 候选框位置由 `apply_ime_cursor_area` 每帧跟着终端光标走。
        window.set_ime_allowed(true);
        let init_size = window.inner_size();
        crate::logx::line(&format!(
            "resumed: 窗口创建 {}x{} scale={}",
            init_size.width,
            init_size.height,
            window.scale_factor()
        ));
        let gpu = Gpu::new(window.clone(), self._runtime.handle());
        // F84/F21:外观设置必须在建 `TextLayer` **之前**读出来 —— 字体族与
        // 字号是构造参数,建完再改就得多走一趟 `set_font`,首帧会用默认字体
        // 画一次再跳。读不出来只是回到默认外观,不阻断启动(见
        // `mullion_store::settings::load`)。
        self.settings = crate::shell::store::config_dir()
            .map(|d| {
                let l = mullion_store::settings::load(&d);
                if let Some(note) = l.note {
                    crate::logx::line(&format!("F84:{note}"));
                }
                l.settings
            })
            .unwrap_or_default();
        // 字号按窗口 DPI 缩放成物理像素(inner_size 是物理像素,须一致):
        // px = pt * (96*scale/72)。Windows 常见 125%/150% 缩放下才不会过小。
        let scale = window.scale_factor() as f32;
        let font_px = font_px_for(self.settings.font_pt, scale);
        let text = TextLayer::new(
            &gpu.device,
            &gpu.queue,
            gpu.config.format,
            font_px,
            self.settings.font_family.as_deref(),
            MULLION_DARK.term_fg,
        );
        // egui 0.30 同帧集成(§4.1):本 Task 只画一个占位 `egui::Window` 证明管线通;
        // 菜单/状态栏/session UI 在后续 Task 接线。
        let egui_ctx = egui::Context::default();
        // egui 内嵌字体不含中文;挂系统 CJK 字体,否则菜单/状态栏中文全是 tofu 方框。
        crate::ui::install_cjk_font(&egui_ctx);
        theme::apply_egui(&egui_ctx, &MULLION_DARK);
        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &*window,
            Some(window.scale_factor() as f32),
            None,
            None,
        );
        let egui_renderer =
            egui_wgpu::Renderer::new(&gpu.device, gpu.config.format, None, 1, false);
        self.active = Some(Active {
            window,
            gpu,
            text,
            geoms: Vec::new(),
            egui_ctx,
            egui_state,
            egui_renderer,
        });

        // 打开会话保险库(Task 6)。失败(keyring 不可用/无法定位配置目录等)不
        // panic/exit——记 ui.last_error,会话管理功能优雅禁用,菜单/关于仍能用
        // (待定 G)。
        //
        // F71:先探一下 `secrets.enc` 声明的方案。是主密码方案就**先弹解锁框**,
        // 会话库这一刻不打开 —— 剩下那串收尾动作推迟到解锁成功之后(见
        // `finish_store_open`)。
        let dir = crate::shell::store::config_dir();
        match dir {
            Some(d) => match crate::shell::store::probe_needs_password(&d) {
                Ok(true) => {
                    crate::logx::line("secrets.enc 由主密码加密,等待解锁");
                    self.pending_layout = Some(saved_layout);
                    self.ui.unlock = Some(crate::ui::unlock::UnlockDraft::default());
                }
                Ok(false) => {
                    self.open_store_with(
                        crate::shell::store::SessionStore::open(
                            d,
                            &mullion_store::KeyringSource::new(),
                        ),
                        saved_layout,
                    );
                }
                Err(e) => {
                    // 探测本身失败(文件头读不懂 / 读不出来)。这里**不能**当成
                    // 「不需要密码」往下走:那会拿钥匙串密钥去解一个主密码文件,
                    // 报出来的是「密文损坏」,把真正的原因盖掉。
                    crate::logx::line(&format!("secrets.enc 探测失败: {e}"));
                    self.ui.set_error(format!("会话库打开失败:{e}"));
                    self.finish_store_open(saved_layout);
                }
            },
            None => {
                crate::logx::line("无法定位配置目录,会话功能禁用");
                self.ui.set_error("无法定位配置目录".into());
                self.finish_store_open(saved_layout);
            }
        }
        diag::mark(diag::Stage::Idle);
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        diag::mark(diag::Stage::UserEvent);
        match event {
            UserEvent::Wake => {
                // 最小化时不请求重绘(也不该指望还能收到 RedrawRequested),但 IO 泵
                // 必须继续跑,否则 T1 红线:rx 灌满 + 远端探测无应答。
                if matches!(
                    shell::window_state::redraw_scope(self.visible),
                    shell::window_state::RedrawScope::PumpOnly
                ) {
                    self.pump_io();
                } else if let Some(a) = &self.active {
                    a.window.request_redraw();
                }
            }
            UserEvent::ConnectOk {
                handle,
                wants_sftp,
                pty,
            } => {
                // 一旦连上就进入交互态:后续(哪怕是本次会话断开后)的连接失败
                // 不再是「CLI 直连首次失败」,不该导致整个 GUI exit(1)(复核 #1)。
                self.cli_direct = false;
                // C1:每次连接都是全新世代——`next_ws_generation` 取值后自增,
                // 保证跟上一次(如果有)断开的那个世代号不同,哪怕 `PaneId`
                // 因为 `next_id` 重新计数而撞号,也能靠这个分辨。D1:`Files`
                // 标签同样要一个世代号(S1 路由键),两种标签共用这一个计数器。
                let generation = self.next_ws_generation;
                self.next_ws_generation += 1;
                // F37:这次连接是不是某个占位标签按「重连」发起的。**取出即
                // 消费**:留着的话下一次正常连接会跑去顶替一个早就连上的标签。
                let pending = self.pending_restore.take();
                let cfg = self.pending_cfg.clone();
                let session_id = self.ui.connect_request_last;
                // E2:标签标题优先取会话名,退回 `user@host`(见 `tab_title`
                // 的文档)——过去这里恒取 `user@host`,标签属性弹窗改的名字
                // 要等到**下一次**重连同一条会话才用得上,现在改完立刻生效。
                let title = tab_title(
                    session_id
                        .and_then(|id| {
                            self.store
                                .as_ref()
                                .and_then(|s| s.list().iter().find(|r| r.id == id))
                        })
                        .map(|rec| rec.identity.name.as_str()),
                    cfg.as_ref().map(|c| (c.user.as_str(), c.host.as_str())),
                );
                // F120:这个标签对应会话在编辑器「SFTP」分节配置的默认目录/书签。
                // 没有 `session_id`(理论上不可达,`connect_request_last` 由发起
                // 连接那一刻设好)或 store 里查不到(会话已被删)都落回全空默认——
                // 跟「没配置」等价,不阻断连接本身。
                let sftp_prefs = session_id
                    .and_then(|id| {
                        self.store
                            .as_ref()
                            .and_then(|s| s.list().iter().find(|r| r.id == id))
                    })
                    .map(|rec| rec.sftp.clone())
                    .unwrap_or_default();

                if wants_sftp {
                    // D1/D6:SFTP 节点——独占标签、独占连接(`handle`),不开
                    // PTY。真正开 sftp channel 挪到 `trigger_sftp_open`
                    // (跟侧栏共用同一条路径),这里只管把标签立起来、触发
                    // 首次打开。
                    crate::logx::line("连接成功,进入 SFTP 标签");
                    self.place_tab(
                        pending.as_ref(),
                        title,
                        session_id,
                        TabContent::Files(Box::new(FilesTab {
                            files: crate::ui::files_panel::PanelFrame::new(
                                sftp_prefs.default_local.as_deref(),
                                sftp_prefs.bookmarks,
                            ),
                            conn: handle,
                            generation,
                            sftp: None,
                            sftp_tasks: Vec::new(),
                            sftp_default_remote: sftp_prefs.default_remote,
                            sftp_home: None,
                        })),
                    );
                    self.ui.close_session_manager();
                    self.ui_dirty = true;
                    self.trigger_sftp_open(generation);
                    self.request_ui_redraw();
                    return;
                }

                let Some((ssh, rx)) = pty else {
                    // `spawn_connect` 保证 `wants_sftp=false` 时 `pty` 恒
                    // `Some`(`open_pty` 失败走 `ConnectErr`,不会发一个
                    // 「两者皆无」的 `ConnectOk`)。到这里说明违反了这个前提——
                    // 目前唯一的生产者(`spawn_connect`)维持着这个不变量,
                    // 这条分支实际不可达;但如果哪天真的走到这里,不能只记
                    // 日志静默丢弃——用户会看到「点了连接,什么都没发生」,
                    // 跟 `ConnectErr`/host_key 弹窗那些故意不做静默失败的路径
                    // 不一致。复用已有的错误展示通道,不新开概念。
                    log::error!(target: "mullion", "ConnectOk 缺少 pty 且未标记 wants_sftp,忽略");
                    self.ui
                        .set_error("连接内部状态异常,请重试(缺少终端通道)".to_string());
                    self.request_ui_redraw();
                    return;
                };
                crate::logx::line("连接成功,进入终端态");
                let mut emulator = mullion_term::emulator::Emulator::new(80, 24);
                let d = theme::term_default_colors(&MULLION_DARK);
                emulator.set_default_colors(d.fg, d.bg);
                // pane 和自动化 task 要共享同一条 channel(spec §1 修订二):
                // `PaneState.pty` 本来就是 `Box<dyn PtyWriter>`,`SshSession`
                // 内部只有一个 mpsc Sender、本身 Send+Sync,`Arc` 只是共享
                // 所有权,不引入锁。既有调用点零改动。
                let ssh = Arc::new(ssh);
                let mut ws = crate::shell::workspace::Workspace::new(
                    PaneState {
                        id: PaneId(1),
                        host_ix: 0,
                        emulator,
                        pty: Box::new(ssh.clone()),
                        rx,
                        pacer: SyncFramePacer::new(),
                        status: crate::shell::workspace::PaneStatus::Live,
                        saw_first_byte: false,
                        // 故意给一个不可能的初值:下一帧 apply_geometry 必然发一次
                        // window_change,真实列/行数才知道(T4)。
                        last_grid: (0, 0),
                        cwd: None,
                        tmux: None,
                    },
                    generation,
                );
                ws.hosts.push(crate::shell::workspace::HostConn {
                    label: title.clone(),
                    addr: cfg
                        .as_ref()
                        .map_or_else(String::new, |c| format!("{}:{}", c.host, c.port)),
                    // 取发起这次连接时记下的那条会话
                    // (`ConnectOk` 事件本身不带 SessionId)。
                    session_id,
                    handle,
                    tmux_bootstrap: Default::default(),
                    tmux_last_try: None,
                });
                // F36:每次连接**开一个新标签**,已有的标签原样留着 —— 它们各自
                // 的 SSH 连接一根都不动(spec F36 验收:「切换标签不重连」;守护
                // `switching_tabs_does_not_touch_the_ssh_connections`)。
                // launcher 态(`tabs` 为空)时这就是第一个标签,CLI 直连同理。
                //
                // **同一条会话可以开多个标签,不去重、标题也不加序号**:序号会让
                // 「关掉中间那个」之后编号整体跳变,反而更难认;区分靠节点色和
                // hover 出来的 `user@host`。
                let replaced = self.place_tab(
                    pending.as_ref(),
                    title,
                    session_id,
                    TabContent::Terminal(Box::new(TerminalTab {
                        ws,
                        current_preset: Some(Preset::Single),
                        last_cfg: cfg,
                        automation: Vec::new(),
                        automation_template: None,
                        automation_status: None,
                        // F50:每个标签自己的一份侧栏状态(D1:侧栏按会话记住)。
                        files: crate::ui::files_panel::PanelFrame::new(
                            sftp_prefs.default_local.as_deref(),
                            sftp_prefs.bookmarks,
                        ),
                        sftp: None,
                        sftp_tasks: Vec::new(),
                        sftp_default_remote: sftp_prefs.default_remote,
                        sftp_home: None,
                    })),
                );
                // F37:是重连一个占位标签 → 把上次的分屏形状搭回来,并给新长
                // 出来的叶子在这条连接上另开 channel(与 F35 预设分屏同一条路)。
                // 树坏了(`apply_saved_tree` 返回 `None`)就保持单屏,不阻断连接。
                if replaced {
                    if let Some(p) = pending.as_ref() {
                        let fresh = self
                            .tabs
                            .by_generation_mut(generation)
                            .and_then(|tab| tab.content.as_terminal_mut())
                            .and_then(|t| {
                                let fresh = t.ws.apply_saved_tree(&p.tree, p.focus_leaf)?;
                                // 恢复出来的形状一般不对应任何预设按钮;单叶子
                                // 例外(它就是 Single)。
                                t.current_preset = (p.tree.len() == 1).then_some(Preset::Single);
                                Some(fresh)
                            });
                        if let Some(fresh) = fresh {
                            self.spawn_fresh_panes(fresh);
                        }
                    }
                }
                // 连上后关掉会话管理弹窗,别让它盖在新终端上方(复核 #4)。
                self.ui.close_session_manager();
                self.ui_dirty = true;
                // 模板跟计划**同进同退**:只有这次真的要跑自动化,才把配置留给
                // 后来的 pane。否则「右键跳过一次」在分屏时会失效——用户明确
                // 说了这次不跑,分屏出来的 pane 却照跑不误。
                let tpl = self.pending_automation_template.take();
                if let Some(plan) = crate::automation::take_pending(
                    &mut self.pending_automation,
                    &mut self.pending_skip_automation,
                ) {
                    // S1:挂回**属主标签**(按世代号查),不用「活动标签」——
                    // `open` 刚把新标签设为活动,今天两者等价,但那是巧合:
                    // 哪天连接成功不再顺带切换焦点,这里就会把 handle 挂错标签。
                    if let Some(t) = self
                        .tabs
                        .by_generation_mut(generation)
                        .and_then(|tab| tab.content.as_terminal_mut())
                    {
                        t.automation_template = tpl;
                    }
                    // 建标签的这个 pane 照配置**全套**跑,含 tmux
                    // (`PaneId(1)` 见 `Workspace::new`)。
                    self.start_automation(generation, PaneId(1), plan, ssh);
                }
                self.request_ui_redraw();
            }
            UserEvent::PaneOpened {
                id,
                ssh,
                rx,
                generation,
            } => {
                // 初始网格给 80x24 占位,真实尺寸由下一帧 apply_geometry 校准
                // (last_grid 给 (0,0),保证那一帧必然发一次 window_change)。
                // S1:按**世代号**找属主标签,绝不用「活动标签」去接 —— 用户在
                // 标签 A 发起分屏、切到标签 B 的这几百毫秒里事件抵达,拿活动标签
                // 接就会把 A 的 pane 挂到 B 上。
                //
                // 真的挂上去了才在下面起自动化(拿着写口出来,而不是在 `ws` 的
                // 借用里直接调 `start_automation` —— 那要 `&mut self`)。
                let mut attached: Option<Arc<mullion_ssh::session::SshSession>> = None;
                if let Some(ws) = self
                    .tabs
                    .by_generation_mut(generation)
                    .and_then(|t| t.content.as_terminal_mut())
                    .map(|t| &mut t.ws)
                {
                    // 开 channel 是真实网络往返(高延迟代理链路下可能要几百 ms 到
                    // 几秒),这期间用户完全可能又切了预设,甚至断开重连出了一个
                    // 全新的 Workspace(C1:`next_id` 重新计数,`id` 会跟旧世代
                    // 撞号)——不查树成员 + 世代直接 attach_pane 的后果:轻则是
                    // 孤儿 pane(不出现在 compute_geoms/渲染/标题条里,`pump`
                    // 却仍在每帧驱动它,SSH channel 永远占着不关),重则是顶掉
                    // 新世代刚建好、正常工作的 PaneState(输入从此写进一条已经
                    // 不存在意义的旧连接)。
                    //
                    // 这里的世代比对**在按世代路由之后是恒真的**,刻意留着:它是
                    // 深度防御,挡的正是「将来有人把上面那句改回活动标签」这类回退,
                    // 而代价只是一次整数比较(每开一条 channel 一次)。
                    if pane_still_wanted(ws, id, generation) {
                        let mut emulator = mullion_term::emulator::Emulator::new(80, 24);
                        let d = theme::term_default_colors(&MULLION_DARK);
                        emulator.set_default_colors(d.fg, d.bg);
                        // 包成 `Arc`:自动化 task 要跟 pane 共享同一条 channel
                        // 的写口(见 `PtyWriter for Arc<SshSession>`)。
                        let ssh = Arc::new(ssh);
                        attached = Some(ssh.clone());
                        ws.attach_pane(PaneState {
                            id,
                            host_ix: 0,
                            emulator,
                            pty: Box::new(ssh),
                            rx,
                            pacer: SyncFramePacer::new(),
                            status: crate::shell::workspace::PaneStatus::Live,
                            saw_first_byte: false,
                            last_grid: (0, 0),
                            cwd: None,
                            tmux: None,
                        });
                    } else {
                        // 让 ssh/rx 在这个分支结束时自然 Drop——Drop 会关掉这条
                        // SSH channel,不留孤儿、也不会顶掉新世代的 PaneState。
                        log::warn!(
                            target: "mullion",
                            "pane {} 的 channel 开好时已经不属于当前世代了(世代 {generation},用户已切走/重连),丢弃",
                            id.0
                        );
                    }
                }
                // 用户报的问题:分屏出来的 pane 没执行节点的登录后命令。
                // **跳过 tmux、其余照跑**(用户拍板的规则,见
                // `automation::pending_for_extra_pane`)。模板是连接那一刻存下的,
                // 这里不回头查库 —— 连上之后用户可能已经改了配置。
                if let Some(sink) = attached {
                    let plan = self
                        .tabs
                        .by_generation(generation)
                        .and_then(|tab| tab.content.as_terminal())
                        .and_then(|t| t.automation_template.as_ref())
                        .and_then(crate::automation::pending_for_extra_pane);
                    if let Some(plan) = plan {
                        self.start_automation(generation, id, plan, sink);
                    }
                }
                self.ui_dirty = true;
                self.request_ui_redraw();
            }
            UserEvent::PaneOpenErr {
                id,
                msg,
                generation,
            } => {
                log::warn!(target: "mullion", "pane {} 开启失败: {msg}", id.0);
                // C1:旧世代的失败提示落到新世代头上,会给用户弹一条跟当前连接
                // 毫不相干的错误 toast——按世代过滤,只有当前世代的失败才展示。
                // S1:世代号即路由键,查得到属主标签才说明这条失败还有意义。
                if self.tabs.by_generation(generation).is_some() {
                    self.ui.set_error(msg);
                    self.ui_dirty = true;
                    self.request_ui_redraw();
                }
            }
            UserEvent::PaneRehosted {
                generation,
                pane,
                handle,
                ssh,
                rx,
            } => {
                // 元信息在用户选中那一帧就存下了(`PendingRehost`)。取不到 =
                // 这条事件没有对应的发起记录(理论上到不了),丢掉:硬编一个
                // 占位标题挂上去,标题条会写着一个假名字。
                let Some(ix) = self
                    .pending_rehost
                    .iter()
                    .position(|p| p.generation == generation && p.pane == pane)
                else {
                    log::warn!(target: "mullion", "换节点:pane {} 的在途记录已经不在了,丢弃", pane.0);
                    return;
                };
                let pending = self.pending_rehost.swap_remove(ix);
                // 包成 `Arc`:自动化 task 要跟 pane 共享同一条 channel 的写口
                // (同 `PaneOpened`,见 `PtyWriter for Arc<SshSession>`)。
                let ssh = Arc::new(ssh);
                let mut attached: Option<Arc<mullion_ssh::session::SshSession>> = None;
                if let Some(ws) = self
                    .tabs
                    .by_generation_mut(generation)
                    .and_then(|t| t.content.as_terminal_mut())
                    .map(|t| &mut t.ws)
                {
                    ws.hosts.push(crate::shell::workspace::HostConn {
                        label: pending.label,
                        addr: pending.addr,
                        session_id: Some(pending.session_id),
                        handle,
                        tmux_bootstrap: Default::default(),
                        tmux_last_try: None,
                    });
                    let host_ix = ws.hosts.len() - 1;
                    if rehost_pane(ws, pane, generation, host_ix, Box::new(ssh.clone()), rx) {
                        attached = Some(ssh);
                    } else {
                        // 没挂成(pane 在拨号途中没了)——刚 push 的那条必须撤掉,
                        // 否则 `hosts` 里留一条谁也不指向的连接,占着不关。
                        ws.hosts.pop();
                        log::warn!(
                            target: "mullion",
                            "换节点:pane {} 在拨号途中已经不在了(世代 {generation}),丢弃",
                            pane.0
                        );
                    }
                }
                if let Some(sink) = attached {
                    // 用户拍板:换过节点的 pane 要跑**新节点**的登录后命令,
                    // 规则同分屏新开的那些 —— 跳过 tmux,其余照跑。
                    if let Some(plan) = pending.plan {
                        self.start_automation(generation, pane, plan, sink);
                    }
                    self.ui.set_toast("已换节点");
                    self.ui_dirty = true;
                    self.request_ui_redraw();
                }
            }
            UserEvent::PaneRehostErr {
                generation,
                pane,
                msg,
            } => {
                log::warn!(target: "mullion", "pane {} 换节点失败: {msg}", pane.0);
                // 在途记录必须清掉,否则同一块 pane 再换一次时,
                // `PaneRehosted` 会取到上一次那条(标题条写着上一台机器的名字)。
                self.pending_rehost
                    .retain(|p| !(p.generation == generation && p.pane == pane));
                // 失败提示按世代过滤,理由同 `PaneOpenErr`。
                if self.tabs.by_generation(generation).is_some() {
                    self.ui.set_error(msg);
                    self.ui_dirty = true;
                    self.request_ui_redraw();
                }
            }
            UserEvent::KeyPathPicked(picked) => {
                self.key_picker_busy = false;
                if let Some(p) = picked {
                    if let Some(buf) = self.ui.editor.as_mut() {
                        // v5:选中的文件当场读成正文存进缓冲,路径不留。
                        crate::ui::session_manager::import_key_file(buf, &p, |p| {
                            std::fs::read_to_string(p)
                        });
                        if let Some(note) = buf.key_note.take() {
                            self.ui.key_drop_note = Some(note);
                        }
                    }
                }
                self.request_ui_redraw();
            }
            UserEvent::CredentialKeyPathPicked(picked) => {
                self.key_picker_busy = false;
                if let Some(p) = picked {
                    if let Some(buf) = self.ui.credential_editor.as_mut() {
                        crate::ui::session_manager::import_credential_key_file(buf, &p, |p| {
                            std::fs::read_to_string(p)
                        });
                        if let Some(note) = buf.key_note.take() {
                            self.ui.key_drop_note = Some(note);
                        }
                    }
                }
                self.request_ui_redraw();
            }
            UserEvent::SshConfigPicked(picked) => {
                self.import_picker_busy = false;
                if let Some(p) = picked {
                    match std::fs::read_to_string(&p) {
                        Ok(text) => {
                            let parsed = mullion_store::parse_ssh_config(&text);
                            let existing: &[mullion_store::SessionRecord] =
                                self.store.as_ref().map_or(&[], |s| s.list());
                            self.ui.import = Some(crate::ui::import_dialog::ImportState {
                                path: p.display().to_string(),
                                rows: crate::ui::import_dialog::build_rows(&parsed, existing),
                                skipped: crate::ui::import_dialog::skip_lines(&parsed),
                            });
                        }
                        // 读不出来就直接说,别开一个空弹窗让用户以为文件是空的。
                        Err(e) => self.ui.set_error(format!("读不了 {}:{e}", p.display())),
                    }
                }
                self.request_ui_redraw();
            }
            UserEvent::IconPathPicked(picked) => {
                self.icon_picker_busy = false;
                if let Some(p) = picked {
                    if let Some(buf) = self.ui.editor.as_mut() {
                        self.ui.icon_error =
                            crate::ui::session_manager::import_icon_file(buf, &p, |p| {
                                std::fs::read(p)
                            })
                            .err();
                    }
                }
                self.request_ui_redraw();
            }
            UserEvent::HostKeyPrompt(prompt) => {
                crate::logx::line(&format!(
                    "主机密钥待确认: {} ({}), 变更={}",
                    prompt.host,
                    prompt.algo,
                    prompt.previous.is_some()
                ));
                // 前一个弹窗还没回答就又来一个(用户连点两次连接):丢掉旧 prompt,
                // 它的 sender 随之析构 → 旧那条握手被拒(fail-closed),不会有
                // 两个窗叠在一起、也不会有连接偷偷放行。
                self.host_key_since = Some(Instant::now());
                self.pending_host_key = Some(prompt);
                self.request_ui_redraw();
            }
            UserEvent::ConnectErr(msg) => {
                // 待定 F:CLI 直连从未成功连过时,保留可脚本化的 exit(1) 语义;
                // launcher 态(或已连过又断开)只记错误,交 UI 展示(ui.last_error)。
                crate::logx::line(&format!("连接失败: {msg}"));
                if self.cli_direct && self.active_ws().is_none() {
                    std::process::exit(1);
                }
                self.ui.set_error(msg);
                self.request_ui_redraw();
            }
            UserEvent::ProbeOk(epoch) => {
                crate::app::accept_probe(epoch, self.probe_epoch, &mut self.ui.probe, Ok(()));
                self.request_ui_redraw();
            }
            UserEvent::ProbeErr(epoch, msg) => {
                crate::app::accept_probe(epoch, self.probe_epoch, &mut self.ui.probe, Err(msg));
                self.request_ui_redraw();
            }
            UserEvent::AutomationDone(generation, pane, outcome) => {
                self.accept_automation_done(generation, pane, outcome);
            }
            UserEvent::TunnelState { id, state } => self.accept_tunnel_state(id, state),
            UserEvent::SftpOpened { generation, result } => {
                self.accept_sftp_opened(generation, result);
            }
            UserEvent::SftpListed {
                generation,
                seq,
                result,
            } => {
                self.accept_sftp_listed(generation, seq, result);
            }
            UserEvent::SftpOpDone { generation, result } => {
                match result {
                    Ok(()) => {
                        self.ui.set_toast("已完成");
                        // 写操作不带回新的目录内容 —— 不刷新的话界面上那个
                        // 文件「还在」,用户会以为没生效然后再删一次。
                        self.dispatch_panel_action_for(
                            generation,
                            crate::ui::files_panel::PanelColumn::Remote,
                            crate::ui::files_panel::FileAction::Refresh,
                        );
                    }
                    Err(msg) => self.ui.set_error(msg),
                }
                self.request_ui_redraw();
            }
            UserEvent::TransferPlanned { generation, result } => {
                match result {
                    Ok(jobs) => {
                        for p in jobs {
                            let id = self.transfer_queue.push(crate::files::queue::NewJob {
                                dir: p.dir,
                                generation,
                                label: p.label,
                                total: p.total,
                            });
                            self.transfer_specs.insert(
                                id,
                                TransferSpec {
                                    dir: p.dir,
                                    generation,
                                    local: p.local,
                                    remote: p.remote,
                                },
                            );
                        }
                    }
                    // 展开阶段就失败 —— 一条 job 都没建,老实报出来。
                    Err(e) => self.ui.set_error(e),
                }
                self.request_ui_redraw();
            }
            UserEvent::TransferProgress { job, done } => {
                // T3:**高频事件,只更数据不重绘**。一个 100MB 的文件会发几千条,
                // 每条都请求重绘就是每秒几千帧、风扇起飞。进度显示由
                // `RedrawRequested` 里那段「队列在跑就标脏 + 排下一帧」按帧闸
                // 驱动(~5Hz),与事件频率无关。
                self.transfer_queue.progress(job, done);
            }
            UserEvent::TransferDone { job, result } => {
                self.transfer_cancels.remove(&job);
                self.transfer_queue.finish(job, result);
                // 传完刷新**目标那一栏** —— 不刷的话新文件不出现,用户以为没成。
                if let Some(spec) = self.transfer_specs.get(&job) {
                    let (generation, dir) = (spec.generation, spec.dir);
                    let column = match dir {
                        crate::files::queue::Direction::Download => {
                            crate::ui::files_panel::PanelColumn::Local
                        }
                        crate::files::queue::Direction::Upload => {
                            crate::ui::files_panel::PanelColumn::Remote
                        }
                    };
                    self.dispatch_panel_action_for(
                        generation,
                        column,
                        crate::ui::files_panel::FileAction::Refresh,
                    );
                }
                // 真正走完的才丢 spec:挂在冲突上的那些还要**用同一份**重跑。
                if self
                    .transfer_queue
                    .get(job)
                    .is_none_or(|j| j.state.is_finished())
                {
                    self.transfer_specs.remove(&job);
                }
                self.request_ui_redraw();
            }
            UserEvent::EditOpened {
                generation,
                kind,
                remote,
                result,
            } => {
                self.finish_edit_open(generation, kind, remote, result);
                self.request_ui_redraw();
            }
            UserEvent::EditTick { key, stamp } => {
                self.on_edit_tick(key, stamp);
                // **不无条件重绘**:看门任务只在文件真的变了时才发这条,
                // 但「变了」不一定改动界面(基线登记那一次就不改)。
                if self.ui_dirty {
                    self.request_ui_redraw();
                }
            }
            UserEvent::EditSaved { key, result } => {
                self.on_edit_saved(key, result);
                self.request_ui_redraw();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        diag::mark(diag::Stage::WindowEvent);
        // F100:标注模式的快捷键必须在下面那段输入分流**之前**截。会话管理器
        // 开着时 `modal` 为真,键盘整段判给 egui 并直接 `return`,走不到
        // `KeyboardInput` 分支里 `Ctrl+Shift+C/V` 那个位置 —— 而标注模式最主要的
        // 用处恰好就是标注会话管理器,放在下面等于「在最需要它的界面里按不出来」。
        if self.annotate_event(&event) {
            return;
        }
        // F36/S4:标签快捷键同样必须在分流**之前**截,理由同上面的标注模式:
        // 走到下面的 `KeyboardInput` 分支就已经晚了(那里会把键编码进 PTY,
        // `Ctrl+W` 会在切标签的同时把远端 shell 的前一个词删掉)。
        // 判定在 `shell::tabs::hotkey`(纯函数),这里只做接线。
        if self.tab_hotkey_event(&event) {
            return;
        }
        // F50/T8:文件侧栏快捷键同样必须在分流之前截,理由同标签快捷键 ——
        // `Ctrl+Shift+B` 里的 `B` 走到下面会被编码进 PTY,写给远端一个字母。
        if self.files_hotkey_event(&event) {
            return;
        }
        // F6/T8:换焦点同样必须在分流之前截,理由同上。
        if self.focus_hotkey_event(&event) {
            return;
        }
        // 输入分流(§4.5)。**键盘与指针的顺序是反的,不是笔误**:
        // - 指针:先喂 egui 再判。egui 要靠 `CursorMoved` 维护 hover,不喂就没有
        //   `wants_pointer_input()` 可言。
        // - 键盘:先判再决定喂不喂(T8)。喂给 egui 的键会先经它的焦点系统——Tab 会被
        //   拿去把焦点给菜单栏第一个按钮,此后 `wants_keyboard_input()` 恒 true,
        //   下面的 route 把每个按键都判给 egui,终端永久收不到键。
        // `modal`/`focus` 在借出 `self.active` 之前算好:两者都要 `&self`,
        // 放进下面那个 `&mut self.active` 的作用域里借用检查过不去。
        let modal = self.modal_open();
        let focus = self.effective_focus();
        // Route::FilesPanel 判给面板的键记在这里,借用 `active` 的作用域结束
        // 之后再处理(`handle_panel_key` 要 `&mut self`,不能跟 `&mut self.active`
        // 同时活着)。
        let mut panel_key_pending = false;
        if let Some(active) = &mut self.active {
            let is_kbd = matches!(event, WindowEvent::KeyboardInput { .. });
            let is_ptr = matches!(
                event,
                WindowEvent::MouseInput { .. }
                    | WindowEvent::MouseWheel { .. }
                    | WindowEvent::CursorMoved { .. }
            );
            let wants_kbd = active.egui_ctx.wants_keyboard_input();
            // 键盘归终端/面板时整段跳过 egui;其余事件(含指针与 resize/focus 等)照旧喂。
            if is_kbd
                && !shell::input_route::egui_should_see_focused(
                    focus,
                    shell::input_route::InputKind::Keyboard,
                    modal,
                    wants_kbd,
                )
            {
                // Route::Terminal → 直落下面 UNCHANGED 的 KeyboardInput 分支(守 T5/T6)。
                // Route::FilesPanel → 面板截走(D23):既不喂 egui,也不落终端
                // 写入分支——记一个标记,借用 `active` 结束后再处理。
                panel_key_pending = matches!(
                    shell::input_route::route_focused(
                        focus,
                        modal,
                        wants_kbd,
                        false,
                        shell::input_route::InputKind::Keyboard,
                    ),
                    shell::input_route::Route::FilesPanel
                );
            } else {
                let resp = active.egui_state.on_window_event(&active.window, &event);
                if resp.repaint {
                    // 标脏与请求重绘必须成对:只请求不标脏,那帧会被 frame_is_dirty
                    // 判 Idle 丢掉(终端态尤其明显:远端一安静菜单就点不开)。
                    self.ui_dirty = true;
                    active.window.request_redraw();
                }
                if is_kbd {
                    return; // 上面已判定归 egui(模态/表单聚焦)
                }
                if is_ptr {
                    let wants_ptr = active.egui_ctx.wants_pointer_input();
                    if matches!(
                        shell::input_route::route(
                            modal,
                            false,
                            wants_ptr,
                            shell::input_route::InputKind::Pointer,
                        ),
                        shell::input_route::Route::Egui
                    ) {
                        return; // egui 已收下,不转终端
                    }
                }
            }
        }
        // F50/D23/T8:面板截走的键在这里落地——`self.active` 的可变借用已经
        // 结束,`handle_panel_key` 才能再拿 `&mut self`。不落到下面的 `match
        // event { .. WindowEvent::KeyboardInput .. }`:那段是终端写入路径,
        // Files 标签根本没有 pane 可写,落进去也只是静默无反应,但语义上这个键
        // 已经被面板处理过一次,不该再走一遍分流判断。
        if panel_key_pending {
            if let WindowEvent::KeyboardInput { event: ke, .. } = &event {
                if ke.state == ElementState::Pressed {
                    if let Some(gen) = self.files_owner_generation() {
                        let mods = self.mods;
                        self.handle_panel_key(gen, &ke.logical_key, mods);
                        // 代码复核挖出的真 bug:`handle_panel_key`(经
                        // `dispatch_panel_action`/`move_panel_selection`)只标
                        // `self.ui_dirty = true`,从不请求重绘。事件循环整个跑在
                        // `ControlFlow::Wait`/`WaitUntil` 上(T3/T7),没有别的事件
                        // 兜底重绘的话,键盘单独触发的这一路(Tab 换栏/↑↓选中/
                        // Enter/Backspace/F5/Ctrl+H)画面会一直停在按键前那一帧,
                        // 直到鼠标挪一下之类的无关事件顺带触发重绘。
                        //
                        // 补在这个单一落点,而不是 `apply_local_file_action`/
                        // `apply_remote_file_action` 内部:那两个函数同时也被
                        // 鼠标点击路径调用,鼠标点击是在一次已经在飞的 `Present`
                        // 帧内部触发的,若在函数内部无条件补 `request_redraw`
                        // 会让鼠标路径每次点击都多请求一帧,违反 T3/T7 的
                        // 「标脏与请求重绘必须成对、不无条件每帧重绘」。这里是
                        // 键盘专属分支,补一次不会波及鼠标路径。
                        self.request_ui_redraw();
                    }
                }
            }
            return;
        }
        match event {
            WindowEvent::CloseRequested => {
                // F53/D3-12:有改动没传回远端就先拦一下。**只拦第一次** ——
                // 拦第二次的话,用户在确认框里选了「仍然退出」之后,那一下
                // `event_loop.exit()` 会再走一遍这里又被拦住,窗口永远关不掉。
                if self.edits.blocks_exit() && !self.ui.exit_pending {
                    crate::logx::line("CloseRequested → 有未回传的编辑,先问一句");
                    self.ui.exit_pending = true;
                    self.request_ui_redraw();
                    return;
                }
                crate::logx::line("CloseRequested → 退出");
                // F37:**无条件写一次**,不看节流窗口(E7)。用户最后那几下
                // 操作(切了标签、拖了窗口)大概率落在上一次落盘之后的 2 秒
                // 窗口里,不在这里补一次就永远丢了 —— 而"关窗口前那一刻的
                // 样子"正是这个功能唯一要还原的东西。
                self.save_layout_if_changed();
                // F92:进程要走了,20 秒的 timeout 别悬着。
                if let Some(h) = self.probe_task.take() {
                    h.abort();
                }
                // D3-12:临时目录整棵删掉。放在 `exit()` 之前 —— winit 的
                // `exiting()` 在某些平台上不保证被调到,而残留的临时目录里
                // 是远端文件的明文副本。
                crate::edit::tempdir::purge(&self.edit_root);
                event_loop.exit();
            }
            // 焦点/遮挡:记录以便定位「失焦后无法回到前台/黑屏」;回到前台时补一次
            // 重绘,避免停在陈旧/空白帧(此前这些事件落 `_ => {}`,不重绘也不留痕)。
            WindowEvent::Focused(focused) => {
                crate::logx::line(&format!("Focused({focused})"));
                // F125:失焦不闪,省掉后台窗口的周期唤醒。
                self.window_focused = focused;
                if focused {
                    self.recheck_visibility();
                    self.request_ui_redraw();
                } else {
                    // F18:捕获被别的窗口抢走时(按住左键时 Alt-Tab / 系统弹窗
                    // 跳出来),winit 不会补发 `MouseInput{Released}`,`dragging`
                    // 会永久卡住、自动滚动停不下来。失焦就当拖拽结束。
                    self.dragging = false;
                    self.autoscroll = 0;
                }
            }
            WindowEvent::Occluded(occluded) => {
                crate::logx::line(&format!("Occluded({occluded})"));
                if !occluded {
                    self.recheck_visibility();
                    self.request_ui_redraw();
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                crate::logx::line(&format!("ScaleFactorChanged({scale_factor})"));
                // F21:字号存的是 pt,物理像素随 DPI 变 —— 把窗口从 150% 的屏
                // 拖到 100% 的屏,不重建字体的话字会缩成原来的三分之二。
                // 走 `apply_font` 这一条路(它顺带标脏),不在这里另算一遍 ——
                // 第二条尺寸传播路径就是 T4 的复发方式。
                self.apply_font();
            }
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            // 指针坐标只在这里更新;滚轮上报要用(F17),划选要用(F18)。
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_px = (position.x as f32, position.y as f32);
                if self.dragging {
                    self.update_selection_endpoint();
                    self.request_ui_redraw();
                }
            }
            // F17 滚轮三档分流。决策在 `mullion_term::keymap::wheel_action`(纯函数,
            // 已单测),这里只做 winit 增量→行数、像素→单元格的换算与发送。
            WindowEvent::MouseWheel { delta, .. } => {
                // 滚轮上报是发给远端的字节 = 用户意图。本地回溯
                // (`WheelAction::LocalScroll`)不发字节,不算接管。
                let mut took_over = false;
                // 先算,下面借出焦点 pane 之后就没法再调 `&self` 方法了。
                let local = self.cursor_in_grid();
                let geom = self.focused_geom();
                // 走 `active_ws_mut_of(&mut self.tabs)` 而不是 `self.active_ws_mut()`:
                // 后者借的是整个 `self`,与同一元组里的 `self.active` 和下面的
                // `self.mods` 冲突(见 `active_ws_of` 的说明)。
                if let (Some(a), Some(g), Some(pane)) = (
                    self.active.as_ref(),
                    geom,
                    active_ws_mut_of(&mut self.tabs).and_then(Workspace::focused_mut),
                ) {
                    let cell_px = (a.text.cell_w, a.text.cell_h);
                    let lines = input::wheel_lines(delta, cell_px.1);
                    let cell = input::cell_at(local, cell_px, g.grid);
                    let action = mullion_term::keymap::wheel_action(
                        pane.emulator.mode(),
                        self.mods.shift_key(),
                        lines,
                        cell,
                    );
                    match action {
                        WheelAction::LocalScroll { lines } => {
                            pane.emulator.scroll(Scroll::Delta(lines));
                        }
                        WheelAction::Report {
                            button,
                            col,
                            row,
                            sgr,
                            count,
                        } => {
                            let one =
                                mullion_term::keymap::encode_wheel_report(button, col, row, sgr);
                            let mut bytes = Vec::with_capacity(one.len() * count as usize);
                            for _ in 0..count {
                                bytes.extend_from_slice(&one);
                            }
                            let _ = pane.pty.write(bytes);
                            took_over = true;
                        }
                        WheelAction::ArrowKeys { up, count } => {
                            // SS3(`ESC O A/B`),不是 `encode_key` 的 CSI——见
                            // `keymap::encode_wheel_arrow` 文档注释(对齐上游 alacritty
                            // + xterm terminfo kcuu1,否则 less/man 认不出滚轮退化键)。
                            let one = mullion_term::keymap::encode_wheel_arrow(up);
                            let mut bytes = Vec::with_capacity(one.len() * count as usize);
                            for _ in 0..count {
                                bytes.extend_from_slice(&one);
                            }
                            let _ = pane.pty.write(bytes);
                            took_over = true;
                        }
                        WheelAction::None => {}
                    }
                }
                if took_over {
                    self.user_took_over();
                }
                // 本地回溯不产生新的终端字节,不标脏这一帧会被 frame_is_dirty 判 Idle
                // 丢掉——滚了但画面不动。
                self.request_ui_redraw();
            }
            // F18 划选 / 右键粘贴。
            //
            // 鼠标**按键**上报(F15)本片不做,所以左键无条件走本地划选,不需要
            // T5 的 Shift 逃生门分流;将来加按键上报时,分流点就在这里
            // (与上面 MouseWheel 的 `wheel_action` 同构)。
            WindowEvent::MouseInput { state, button, .. } => match (button, state) {
                (MouseButton::Left, ElementState::Pressed) => {
                    // 点哪块就切到哪块(F33)。必须在 selection_press 之前:
                    // 划选的锚点要落在新焦点 pane 的坐标系里。
                    if let Some(id) = self.pane_at(self.cursor_px) {
                        if let Some(ws) = self.active_ws_mut() {
                            if ws.focus() != id {
                                ws.set_focus(id);
                                self.ui_dirty = true;
                            }
                        }
                    }
                    self.selection_press();
                }
                (MouseButton::Left, ElementState::Released) => self.selection_release(),
                // 右键直接贴,不弹菜单(Windows 终端习惯,F18 交互口径)。
                (MouseButton::Right, ElementState::Pressed) => self.request_paste(),
                _ => {}
            },
            WindowEvent::Resized(size) => {
                diag::mark(diag::Stage::Resize);
                log::debug!(target: "mullion", "Resized({}x{})", size.width, size.height);
                self.apply_resize(size.width, size.height);
            }
            // 输入法(F21):中文/日文的字是从这条路进来的,不是 `KeyboardInput`。
            // `set_ime_allowed(true)` 在 `resumed` 里开,不开这条事件根本不会发。
            WindowEvent::Ime(ime) => {
                match &ime {
                    winit::event::Ime::Preedit(text, _) => self.ime.on_preedit(text),
                    winit::event::Ime::Commit(text) => {
                        self.ime.on_commit();
                        // F125:输入法提交也是输入,重置闪烁相位。
                        self.last_input_at = Instant::now();
                        // 组字结果按用户输入对待:先回底部(否则「打了但看不到」,
                        // 与按键/粘贴同一条口径),再写焦点 pane。
                        if let Some(bytes) = input::ime_commit_bytes(text) {
                            if let Some(pane) =
                                self.active_ws_mut().and_then(Workspace::focused_mut)
                            {
                                pane.emulator.selection_clear();
                                pane.emulator.scroll_to_bottom();
                                let _ = pane.pty.write(bytes);
                                // F40:用户接管,自动化让位(借用已释放)。
                                self.user_took_over();
                            }
                        }
                    }
                    winit::event::Ime::Enabled => {}
                    winit::event::Ime::Disabled => self.ime.on_disabled(),
                }
                self.request_ui_redraw();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                // 组字期间的按键是拼音字母,归输入法。不吞的话打「你好」会先往
                // 远端送一串 `nihao` 再送「你好」(见 `input::ImeState`)。
                if self.ime.swallows_key() {
                    return;
                }
                if event.state == ElementState::Pressed {
                    // F125:打字重置闪烁相位,保证「刚敲完光标一定是亮的」。
                    self.last_input_at = Instant::now();
                    if let Some((key, mods)) = input::translate_key(&event, self.mods) {
                        // F18:`Ctrl+Shift+C/V` 必须在 `encode_key` 之前截住。
                        // Ctrl+C 会被编码成 `0x03`(SIGINT)——漏下去就是「想复制
                        // 结果把远端进程杀了」。Shift 让它与裸 Ctrl+C 明确区分,
                        // 裸 Ctrl+C 照旧转发。
                        if mods.ctrl && mods.shift {
                            if let Key::Char(c) = key {
                                match c.to_ascii_lowercase() {
                                    'c' => {
                                        self.copy_selection();
                                        self.request_ui_redraw();
                                        return;
                                    }
                                    'v' => {
                                        self.request_paste();
                                        self.request_ui_redraw();
                                        return;
                                    }
                                    _ => {}
                                }
                            }
                        }
                        // F17:Shift+PageUp/PageDown 是本地翻页,截住不转发对端
                        // (裸 PageUp/PageDown 照旧转发,tmux/less 自己会翻)。
                        if mods.shift && matches!(key, Key::PageUp | Key::PageDown) {
                            let scroll = if matches!(key, Key::PageUp) {
                                Scroll::PageUp
                            } else {
                                Scroll::PageDown
                            };
                            if let Some(pane) =
                                self.active_ws_mut().and_then(Workspace::focused_mut)
                            {
                                pane.emulator.scroll(scroll);
                            }
                            self.request_ui_redraw();
                            return;
                        }
                        let bytes = mullion_term::keymap::encode_key(key, mods, self.kitty);
                        // `let _` 全文件都这样:写/resize 失败(断线等)没有用户提示、
                        // 无重连。断线感知与重连是 S3,后续 spec,这里不做。
                        // launcher 态(ws=None)没有终端可写,按键静默丢弃。按键永远发给
                        // **焦点** pane(F33)——分屏后不该出现「按了 A 屏的键跑进 B 屏」。
                        if let Some(pane) = self.active_ws_mut().and_then(Workspace::focused_mut) {
                            // F18:一按普通键就清选区。留着的话高亮会挂在屏幕上,
                            // 而底下的内容早被新输出冲掉了——高亮的是别的字。
                            pane.emulator.selection_clear();
                            // F17:一按普通键就贴回底部,否则「打字了但看不到自己输入」。
                            pane.emulator.scroll_to_bottom();
                            let _ = pane.pty.write(bytes);
                            // F40:用户接管,自动化让位(借用已释放)。
                            self.user_took_over();
                        }
                    } else if let Some(text) = input::translate_text(&event.logical_key, self.mods)
                    {
                        // 合成输入(死键 / 部分布局)把结果整段塞进 `Character`,
                        // 映射不成单键。旧实现直接丢 —— 按出来的字凭空消失。
                        if let Some(bytes) = input::ime_commit_bytes(&text) {
                            if let Some(pane) =
                                self.active_ws_mut().and_then(Workspace::focused_mut)
                            {
                                pane.emulator.selection_clear();
                                pane.emulator.scroll_to_bottom();
                                let _ = pane.pty.write(bytes);
                                self.user_took_over();
                            }
                        }
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                let now = self.now_ms();
                // 1+2. 排空每个 pane 的 rx→feed emu→回写各自的 PtyWrite(T1 红线)——
                // 仅终端态有字节可处理;launcher 态(ws=None)没有终端,跳过,但下面的帧率闸 + egui
                // 渲染仍要跑(egui 在 launcher 也要画占位 UI)。
                self.pump_io();
                // 1.5 F55/F59:传输队列每帧推进一次(放行 worker + 采样速率)。
                // **放在帧里而不是事件里**是 T3:进度事件每秒几千条,靠它们
                // 驱动重绘就是风扇起飞;这里只把「队列在跑」当成脏,重绘频率
                // 因此由下面那段排期(~5Hz)决定,与事件频率无关。
                self.pump_transfers();
                if self.transfer_queue.summary().busy {
                    self.transfer_queue.tick(self.start.elapsed().as_secs_f64());
                    self.ui_dirty = true;
                }
                // 1.6 F55:有 job 挂在冲突上就把处置框弹出来。**绝不静默覆盖**;
                // 也不重复弹:已经有别的对话框开着时等它先关掉。
                if self.ui.files_dialog.is_none() {
                    if let Some(j) = self.transfer_queue.first_conflict() {
                        self.ui.files_dialog =
                            Some(crate::ui::files_dialog::FilesDialog::Conflict {
                                name: j.label.clone(),
                                job: j.id,
                                apply_all: false,
                            });
                        self.ui_dirty = true;
                    }
                }
                // 2.2 自愈:能收到重绘请求本身就说明窗口大概率看得见。若还挂在
                // Minimized 且实测尺寸非零,就地恢复(否则一次异常的 Resized(0,0)
                // 之后再没有非零 Resized,窗口永久停在 PumpOnly:字节照收,画面
                // 再不更新——用户看到的就是「键盘没有任何反应」)。
                self.recheck_visibility();
                // 2.5 最小化:泵完就收工。不 present、不重排网格——此时窗口面积为 0,
                // 继续 acquire/present 只是对着 0 面积表面空转,而 egui 也不可能被交互
                // (故下面那段弹窗 intent 施加一并跳过是安全的)。
                if matches!(
                    shell::window_state::redraw_scope(self.visible),
                    shell::window_state::RedrawScope::PumpOnly
                ) {
                    diag::count_skipped();
                    self.next_frame_at = None;
                    event_loop.set_control_flow(ControlFlow::Wait);
                    diag::mark(diag::Stage::Idle);
                    return;
                }
                // 3. present 受帧率(T3)与同步块(T2)双闸。`plan` 是纯决策,三支都
                // 显式复位 control_flow——Throttle 靠 about_to_wait 到点补画,不在
                // 这里 request_redraw,否则陈旧 WaitUntil 过期后每轮零延迟
                // ResumeTimeReached 会忙转空转满 CPU(T3/N3 红线)。
                //
                // dirty:终端态取「远端来了新字节(pacer,含同步块探测)」与「egui 要
                // 重绘」的并集——只看前者的话,远端一安静菜单就点不开(见
                // `frame::frame_is_dirty`)。launcher 态本 Task 没有持续数据源,把
                // 「确实触发了一次 RedrawRequested」当作脏——这不是无条件轮询:
                // ControlFlow::Wait 下 winit 不会凭空生成 RedrawRequested,真正的重绘
                // 频率由触发它的事件(resize/connect/wake/OS 重绘)决定。
                let dirty = match self.active_ws() {
                    Some(ws) => crate::frame::frame_is_dirty(
                        crate::render::panes_ready_to_present(
                            ws.panes().iter().map(|p| &p.pacer),
                            now,
                        ),
                        self.ui_dirty,
                    ),
                    None => true,
                };
                let action = self.limiter.plan(dirty, now);
                // F18 自动滚动只在**真正出帧**的那一轮施加,见 match 之后的说明。
                let presented = matches!(action, RedrawAction::Present);
                match action {
                    RedrawAction::Present => {
                        if self.active.is_some() {
                            // 几何先算:渲染、标题条、鼠标命中、window_change 全用这一份。
                            let geoms = self.compute_geoms();
                            if let Some(a) = self.active.as_mut() {
                                a.geoms = geoms.clone();
                            }
                            // F50:侧栏首次打开(或者还没读过)时,本地栏同步、
                            // 远端栏异步各触发一次数据加载。判据是「开着 且
                            // 还没读过」(`Load::Idle`)而不是「这一帧刚被打开」
                            // ——快捷键和菜单两条打开路径都要覆盖到,而后者是在
                            // `build_ui` 内部直接改的 `ui_state`,这里判不出
                            // "刚刚才开"。之后 `Refresh`/`Goto` 等动作会把
                            // `load` 推离 `Idle`,这段自然只跑一次。**必须在
                            // 下面借出 `self.store`/`self.ui`/`self.tabs`(给
                            // `titles`/`frame.automation`)之前做**——这几处都要
                            // `&mut self` 或 `&self.tabs`,借用检查过不去
                            // (E0502/E0499)。
                            //
                            // 顺带记下这一帧文件面板实际要画的标签的**世代号**
                            // (`files_owner_generation`)——它是本地/远端触发的
                            // 目标,也是下面 `render_frame` 结束后把 `PanelFrame`
                            // 放回原处、以及 `actions.files_local`/`files_remote`
                            // 落回哪个标签的唯一依据。用世代号而不是"现在活动的
                            // 标签":`render_frame` 期间用户切换/关闭标签的话,
                            // 这些动作仍要落回**画出它们的那个标签**(S1 同款
                            // 纪律)。
                            //
                            // D1:两种互斥的宿主(D4)——活动标签本身就是
                            // `TabContent::Files`(标签宿主,占满内容区),或者
                            // 活动标签是 `Terminal` 且侧栏开着(侧栏宿主)。侧栏
                            // 只在终端标签上有意义,不会跟标签宿主同帧成立。
                            //
                            // 判断逻辑抽成了 `active_is_files_tab`/`files_owner_generation`
                            // 两个 `&self` 方法(F6 的 `effective_focus` 与
                            // `handle_panel_key` 也要用同一份判据,不能各写一遍——
                            // 协调者修订 1)。`active_is_files` 这个局部变量仍然
                            // 留着,下面 `sidebar_arg`/`content_arg` 那段要用。
                            let active_is_files = self.active_is_files_tab();
                            let files_owner_generation = self.files_owner_generation();
                            if let Some(gen) = files_owner_generation {
                                if self.tabs.active().is_some_and(|t| {
                                    t.content.files_panel().is_some_and(|f| {
                                        matches!(f.local.load, crate::files::state::Load::Idle)
                                    })
                                }) {
                                    self.apply_local_file_action(
                                        gen,
                                        crate::ui::files_panel::FileAction::Refresh,
                                    );
                                }
                                // F50/D6:远端栏同理,但走异步的 sftp 打开链路
                                // (`trigger_sftp_open` → `UserEvent::SftpOpened` →
                                // 首次 `list_dir` → `UserEvent::SftpListed`),
                                // 不像本地栏那样能同步读盘。D1:标签宿主一开出来
                                // 就已经在 `ConnectOk` 里触发过一次
                                // `trigger_sftp_open`,这里的 `sftp_client().is_none()`
                                // 判据保证不会重复触发(该函数内部本身也有这层
                                // 判重,双保险不冲突)。
                                if self.tabs.active().is_some_and(|t| {
                                    t.content.sftp_client().is_none()
                                        && t.content.files_panel().is_some_and(|f| {
                                            matches!(f.remote.load, crate::files::state::Load::Idle)
                                        })
                                }) {
                                    self.trigger_sftp_open(gen);
                                }
                            }
                            // 把 `PanelFrame` 挪出来变成不再借用 `self.tabs` 的
                            // 本地值:下面 `frame.automation` 是从同一个标签借出
                            // 的 `&str`(`active_term_of(&self.tabs)...`),跟这里
                            // 要的 `&mut PanelFrame` 同源,一个可变一个不可变,
                            // `self.tabs` 借不出这两份。`render_frame` 调用、
                            // `titles`/`snaps` 那几份不可变借用 `drop` 之后,
                            // 按世代号放回去——期间哪怕用户切换/关闭了标签也不会
                            // 错放(S1)。`files_panel_mut` 两个变体都有,不用再
                            // `and_then(as_terminal_mut)` 判一次种类。
                            // F84:备好 / 清掉设置弹窗的草稿与环境。必须在
                            // 下面那些 `self.store`/`self.tabs` 的借用之前 ——
                            // 它要 `&mut self`。
                            self.sync_settings_dialog();
                            let mut taken_files = files_owner_generation.and_then(|gen| {
                                self.tabs
                                    .by_generation_mut(gen)
                                    .and_then(|tab| tab.content.files_panel_mut())
                                    .map(std::mem::take)
                            });
                            let sessions: &[mullion_store::SessionRecord] =
                                self.store.as_ref().map_or(&[], |s| s.list());
                            let store_available = self.store.is_some();
                            // 借 self.pending_host_key / self.host_key_since 与下面
                            // `&mut self.ui` 是不相干字段,可同时借出。
                            let host_key_view = self.pending_host_key.as_deref().map(|p| {
                                crate::ui::host_key::HostKeyView {
                                    host: &p.host,
                                    algo: &p.algo,
                                    fingerprint: &p.fingerprint,
                                    previous: p.previous.as_ref().map(|e| e.fingerprint.as_str()),
                                    elapsed_secs: self
                                        .host_key_since
                                        .map_or(0, |t| t.elapsed().as_secs()),
                                    persist: p.persist,
                                }
                            });
                            // 与 host_key_view 同理:`self.pending_paste` 与
                            // `&mut self.ui` 是不相干字段,可同时借出。
                            let paste_view = self
                                .pending_paste
                                .as_deref()
                                .map(|text| crate::ui::paste::PasteView { text });

                            // 快照要先全部取出来:PaneRender 借着它们,而 render_frame
                            // 同时要 &mut self.ui。
                            let snaps: Vec<_> = self
                                .active_ws()
                                .map(|ws| {
                                    geoms
                                        .iter()
                                        .filter_map(|g| {
                                            ws.pane(g.id).map(|p| (*g, p.emulator.snapshot()))
                                        })
                                        .collect()
                                })
                                .unwrap_or_default();
                            let focus = self.active_ws().map(Workspace::focus);
                            let renders: Vec<crate::gpu::PaneRender<'_>> = snaps
                                .iter()
                                .map(|(g, s)| crate::gpu::PaneRender {
                                    geom: *g,
                                    snap: s,
                                    focused: Some(g.id) == focus,
                                })
                                .collect();
                            // 同 `active_ws_of` 的说明:这里借出去的 `titles` 一直
                            // 活到下面 `render_frame`,走方法会连 `self.active` /
                            // `self.ui` 一起锁住。
                            let titles: Vec<crate::ui::pane_title::TitleView<'_>> =
                                active_ws_of(&self.tabs)
                                    .map(|ws| {
                                        geoms
                                            .iter()
                                            .enumerate()
                                            .map(|(i, g)| crate::ui::pane_title::TitleView {
                                                geom: *g,
                                                index: i + 1,
                                                host: ws.pane(g.id).and_then(|p| {
                                                    ws.hosts
                                                        .get(p.host_ix)
                                                        .map(|h| h.label.as_str())
                                                }),
                                                status: ws.pane(g.id).map_or(
                                                    crate::shell::workspace::PaneStatus::Live,
                                                    |p| p.status,
                                                ),
                                                focused: Some(g.id) == focus,
                                                // 一条连接一个会话(ADR-009:多 pane
                                                // 共用一条 SSH 连接,`host_ix` 目前恒 0)。
                                                appearance: ws
                                                    .pane(g.id)
                                                    .and_then(|p| ws.hosts.get(p.host_ix))
                                                    .and_then(|h| h.session_id)
                                                    .and_then(|sid| self.appearance.get(sid)),
                                                // ⑥:远端报出来的目录 / tmux 名。
                                                // 拿不到就是 `None` —— 不显示,
                                                // 不猜。
                                                cwd_leaf: ws
                                                    .pane(g.id)
                                                    .and_then(|p| p.cwd.as_deref())
                                                    .and_then(crate::ui::pane_title::dir_leaf),
                                                tmux: ws.pane(g.id).and_then(|p| p.tmux.as_deref()),
                                            })
                                            .collect()
                                    })
                                    .unwrap_or_default();
                            // F36:同 `titles`,借用要一直活到 `render_frame`,
                            // 所以走自由函数取 `self.tabs`、不走 `&self` 方法。
                            let active_ix = self.tabs.active_index();
                            let tab_views: Vec<crate::ui::chrome::TabView<'_>> = self
                                .tabs
                                .iter()
                                .enumerate()
                                .map(|(i, tab)| {
                                    let appearance =
                                        tab.session_id.and_then(|sid| self.appearance.get(sid));
                                    crate::ui::chrome::TabView {
                                        title: tab.display_title(),
                                        active: i == active_ix,
                                        session_id: tab.session_id,
                                        appearance,
                                        // F122:覆盖优先,否则会话色(设计 D5:
                                        // 同一条视觉通道,一个标签上不出现两种颜色)。
                                        color: crate::ui::chrome::effective_tab_color(
                                            tab.color_override.map(theme::c32),
                                            appearance.and_then(|a| {
                                                crate::ui::badge::should_paint(
                                                    a,
                                                    mullion_store::ColorTarget::Tab,
                                                )
                                            }),
                                        ),
                                    }
                                })
                                .collect();
                            // F37:活动标签是占位标签时,中央区画的东西。同
                            // `tab_views`,借用要一直活到 `render_frame`,所以
                            // 在这儿现算、不走 `&self` 方法。
                            let restored_view = self.tabs.active().and_then(|tab| {
                                match &tab.content {
                                    TabContent::Restored(r) => {
                                        Some(crate::ui::restored::RestoredView {
                                            tab_id: tab.id,
                                            title: tab.title.as_str(),
                                            // 树坏了就当 1 屏 —— 这里只是一句
                                            // 文案,不值得为它把标签判废。
                                            panes: crate::shell::layout_snapshot::leaf_count(
                                                &r.tree,
                                            )
                                            .unwrap_or(1),
                                            dialing: r.dialing,
                                        })
                                    }
                                    TabContent::Terminal(_) | TabContent::Files(_) => None,
                                }
                            });
                            let restored_count = self
                                .tabs
                                .iter()
                                .filter(|t| matches!(t.content, TabContent::Restored(_)))
                                .count();
                            let groups: &[mullion_store::GroupRecord] =
                                self.store.as_ref().map_or(&[], |s| s.groups());
                            let credentials: &[mullion_store::CredentialRecord] =
                                self.store.as_ref().map_or(&[], |s| s.credentials());
                            let tunnels: &[mullion_store::TunnelRecord] =
                                self.store.as_ref().map_or(&[], |s| s.tunnels());
                            let tunnel_states = self.tunnels.snapshot();
                            let frame = crate::ui::UiFrame {
                                sessions,
                                groups,
                                credentials,
                                tunnels,
                                tunnel_states: &tunnel_states,
                                store_available,
                                // F37:占位标签**不算已连接** —— 它一条连接都
                                // 没建。判据从「有没有标签」收紧成「活动标签
                                // 真的连着」之后,状态栏会照实说「未连接」,
                                // 布局预设按钮组也不会画在一个没有 pane 可切的
                                // 标签上(点了只会静默无反应)。
                                connected: self
                                    .tabs
                                    .active()
                                    .is_some_and(|t| !matches!(t.content, TabContent::Restored(_))),
                                panes: self.active_ws().map_or(1, Workspace::pane_count),
                                preset: self.active_term().and_then(|t| t.current_preset),
                                titles: &titles,
                                tabs: &tab_views,
                                host_key: host_key_view,
                                paste: paste_view,
                                secret_presence: match (self.store.as_ref(), self.ui.editor_id) {
                                    (Some(s), Some(id)) => s.secret_presence(id),
                                    _ => crate::ui::session_manager::SecretPresence::default(),
                                },
                                // F74:凭据档那份。新建凭据(`editor_id == None`)
                                // 落默认值 —— 库里还没有它,谈不上「已设置」。
                                credential_presence: match (
                                    self.store.as_ref(),
                                    self.ui.credential_editor_id,
                                ) {
                                    (Some(s), Some(id)) => s.credential_secret_presence(id),
                                    _ => crate::ui::session_manager::SecretPresence::default(),
                                },
                                // 「跑着的时候盖住上一次的结论」这条规则放在
                                // `automation::status_line` 里,不在这儿手写
                                // if/else —— 写反的现象是新连接的状态栏挂着上
                                // 一条连接的结论,而它有单测钉着。
                                // 同 `active_ws_of`:`automation_status` 借出的 `&str`
                                // 活到 `render_frame`,走 `&self` 方法会锁住整个 self。
                                automation: crate::automation::status_line(
                                    active_term_of(&self.tabs)
                                        .is_some_and(|t| !t.automation.is_empty()),
                                    active_term_of(&self.tabs)
                                        .and_then(|t| t.automation_status.as_deref()),
                                ),
                                appearance: &self.appearance,
                                // 协调者复核 #2:焦点在哪一侧此前完全没有视觉反馈,
                                // F6/Tab 键盘可达性等于形同虚设。`effective_focus`
                                // (已按上下文夹紧,不是裸 `self.focus`)算出的判据
                                // 原样转发,`files_panel::sidebar`/`content` 据此
                                // 决定画不画焦点边框。
                                restored: restored_view,
                                restored_count,
                                settings: self.ui.settings_open.then_some(
                                    crate::ui::settings::SettingsEnv {
                                        families: &self.settings_families,
                                        not_monospace: self.settings_not_mono,
                                        has_master_password: self
                                            .store
                                            .as_ref()
                                            .is_some_and(|s| s.has_master_password()),
                                        store_available: self.store.is_some(),
                                    },
                                ),
                                files_focused: self.effective_focus()
                                    == shell::input_route::Focus::FilesPanel,
                            };
                            // F125:光标该不该画,得在借出 `self.active` 之前算——
                            // `blink_on` 要读 `self.window_focused`/`self.last_input_at`/
                            // `self.active_ws()`,这几个都是 `&self` 方法,借用会跟下面
                            // `self.active.as_mut()` 冲突。
                            let blink_on = self.blink_on(Instant::now());
                            let a = self.active.as_mut().expect("上面刚判过 is_some");
                            // D1:两种宿主互斥(见上面 `files_owner_generation`
                            // 的说明),`taken_files` 只会有其中一份非空数据 ——
                            // 按 `active_is_files` 决定它该走 `render_frame` 的
                            // 哪一个参数槽。
                            let (sidebar_arg, content_arg) = if active_is_files {
                                (None, taken_files.as_mut())
                            } else {
                                (taken_files.as_mut(), None)
                            };
                            let (repaint_delay, mut actions) = render_frame(
                                a,
                                &renders,
                                &mut self.ui,
                                frame,
                                sidebar_arg,
                                content_arg,
                                files_owner_generation.unwrap_or(0),
                                &self.transfer_queue,
                                &self.edits,
                                &mut self.editor,
                                blink_on,
                            );
                            drop(renders);
                            drop(titles);
                            drop(snaps);
                            // 放回去(见上面 `taken_files` 的说明)。找不到属主
                            // 标签——这一帧同时把它关掉的极端情形——数据随
                            // `taken_files` 一起丢弃就是对的:标签都没了,它的
                            // 文件面板状态不需要留着。`files_panel_mut` 两个
                            // 变体都有,不用再判一次种类。
                            if let (Some(gen), Some(pf)) = (files_owner_generation, taken_files) {
                                if let Some(files) = self
                                    .tabs
                                    .by_generation_mut(gen)
                                    .and_then(|tab| tab.content.files_panel_mut())
                                {
                                    *files = pf;
                                }
                            }

                            // ②:侧栏「关→开」跃迁才同步一次。一直跟着焦点
                            // pane 走的话,用户在面板里点开的目录会被反复
                            // 拽回终端所在目录,完全没法浏览。
                            if self.ui.files_sidebar_open && !self.files_sidebar_was_open {
                                self.sync_files_to_focused_pane();
                            }
                            self.files_sidebar_was_open = self.ui.files_sidebar_open;

                            self.limiter.record_present(now);
                            // egui 侧已画出;下面若 egui 又要一帧会重新置脏。
                            self.ui_dirty = false;
                            // 施加几何:F34/T4 的唯一出口。本帧 build_ui 刚写入的
                            // central_px 要下一帧才生效(与 B0 起就是这个语义)。
                            if let Some(ws) = self.active_ws_mut() {
                                for p in ws.panes_mut_iter() {
                                    p.pacer.mark_presented();
                                }
                                ws.apply_geometry(&geoms);
                            }
                            // F84:设置弹窗的结论。放在布局动作之前 —— 换字体
                            // 改的是 `cell_w`/`cell_h`,下一帧的 `compute_geoms`
                            // 才会照着新值重排(T4)。
                            if let Some(out) = actions.settings {
                                self.apply_settings_action(out);
                            }
                            // F71:解锁框的结论。「退出」走既有的 `request_quit`
                            // 收口(本帧靠后那一段),不自己再调一次
                            // `event_loop.exit()` —— 另开一条退出路径就意味着
                            // 标签的收口顺序有两份。
                            if let Some(out) = actions.unlock {
                                if self.apply_unlock_action(out) {
                                    self.ui.request_quit = true;
                                }
                            }
                            // 点了 pane 标题条的「换节点」:开弹窗,真正换在
                            // 用户选完之后。**只开弹窗,不预判节点** —— 这一步
                            // 没有任何默认答案可猜。
                            if let Some(pane) = actions.rehost_pane {
                                self.ui.rehost = Some(crate::ui::rehost::RehostDraft::new(pane));
                                self.ui_dirty = true;
                            }
                            // 换节点弹窗的结论。`Cancel` 什么都不做(弹窗自己
                            // 已经关了);`Pick` 落到 `self.ui` 上中转,真正
                            // 发起在下面借用释放之后(同 `tab_props_save`)。
                            if let Some(crate::ui::rehost::RehostAction::Pick { pane, session }) =
                                actions.rehost
                            {
                                self.ui.rehost_request = Some((pane, session));
                            }
                            // 布局动作:点了预设 / 点了标题条的 ×。路由逻辑在自由函数
                            // `apply_layout_actions`(只碰 &mut Workspace,可脱离
                            // runtime/proxy 单测);真正开新 channel 需要 runtime/proxy,
                            // 落在 `spawn_fresh_panes`。
                            if let Some(t) = self.active_term_mut() {
                                if let Some((fresh, preset_out)) =
                                    apply_layout_actions(&mut t.ws, &actions)
                                {
                                    t.current_preset = preset_out;
                                    self.ui_dirty = true;
                                    self.spawn_fresh_panes(fresh);
                                }
                            }
                            // F36:标签栏动作。切换只动 `active`(不碰任何 SSH
                            // 连接——守护测试
                            // `switching_tabs_does_not_touch_the_ssh_connections`);
                            // 关闭走 `wind_down` 收口;`+` 打开会话管理器。
                            match actions.tab {
                                Some(crate::ui::chrome::TabAction::Switch(ix)) => {
                                    self.tabs.switch_to_index(ix);
                                    self.ui_dirty = true;
                                }
                                Some(crate::ui::chrome::TabAction::Close(ix)) => {
                                    if let Some(tab) = self.tabs.close(ix) {
                                        // F55:标签没了,它的传输也就没有落点/
                                        // 连接了 —— 先作废再收口。
                                        self.cancel_transfers_of(tab.content.generation());
                                        wind_down(tab);
                                    }
                                    self.ui_dirty = true;
                                }
                                Some(crate::ui::chrome::TabAction::NewSession) => {
                                    self.ui.session_manager_open = true;
                                    self.ui_dirty = true;
                                }
                                // F122:双击标签,或右键菜单点了「重命名…」/
                                // 「设置颜色…」。初值取**当前有效值**(覆盖优先,
                                // 否则会话名/会话色)—— 不再去 store 里捞记录,
                                // 改的东西也不再写回去。
                                Some(crate::ui::chrome::TabAction::Props(ix)) => {
                                    if let Some(tab) = self.tabs.get(ix) {
                                        let color = crate::ui::chrome::effective_tab_color(
                                            tab.color_override.map(theme::c32),
                                            tab.session_id
                                                .and_then(|sid| self.appearance.get(sid))
                                                .and_then(|a| {
                                                    crate::ui::badge::should_paint(
                                                        a,
                                                        mullion_store::ColorTarget::Tab,
                                                    )
                                                }),
                                        );
                                        self.ui.tab_props =
                                            Some(crate::ui::tab_props::TabPropsDraft {
                                                tab_id: tab.id,
                                                name: tab.display_title().to_string(),
                                                color,
                                            });
                                    }
                                    self.ui_dirty = true;
                                }
                                None => {}
                            }
                            // F122:标签属性弹窗按了「保存」。先落到 `self.ui`
                            // 上(同 `save_request` 的中转理由:egui 闭包借不到
                            // `&mut self.tabs`),真正施加在下面、`self.active`/
                            // `self.ui` 的借用释放之后。
                            if let Some(a @ crate::ui::tab_props::TabPropsAction::Save { .. }) =
                                actions.tab_props.take()
                            {
                                self.ui.tab_props_save = Some(a);
                            }
                            // F37:占位标签上按了「重连」/菜单里按了「全部
                            // 重连」。两条走同一个 `reconnect_tab`,不分叉。
                            if let Some(id) = actions.reconnect_tab {
                                self.reconnect_tab(id);
                            }
                            if actions.reconnect_all {
                                self.reconnect_next_restored();
                            }
                            // F50:本地栏动作同步施加,远端栏动作走 D6 的 sftp
                            // 打开/加载链路(见 `apply_local_file_action`/
                            // `apply_remote_file_action` 的文档注释)。两者都按
                            // `files_owner_generation` 路由,不是「当前活动
                            // 标签」——理由同上面 `taken_files` 那段。
                            if let Some(gen) = files_owner_generation {
                                if let Some(action) = actions.files_local {
                                    self.apply_local_file_action(gen, action);
                                }
                                if let Some(action) = actions.files_remote {
                                    self.apply_remote_file_action(gen, action);
                                }
                                // F52:从资源管理器扔进来的一批。同一条
                                // `files_owner_generation` 路由(S1)。
                                if !actions.files_drop_in.is_empty() {
                                    self.start_drop_in(
                                        gen,
                                        std::mem::take(&mut actions.files_drop_in),
                                    );
                                }
                                // F59:指针出了窗口,把这一拖交给系统。
                                // 同一条 `files_owner_generation` 路由(S1)。
                                if actions.files_drag_out {
                                    self.start_drag_out(gen);
                                }
                                // D2:对话框里确认了的写操作。按同一条
                                // `files_owner_generation` 判据路由(S1),不
                                // 重新算一遍。
                                if let Some(op) = actions.files_op {
                                    self.apply_file_op(gen, op);
                                }
                            }
                            // F55:传输面板上按的东西。**取消要同时扳旗标和改队列**:
                            // 只改队列的话 worker 还在闷头传完整个文件,只扳旗标
                            // 的话界面上那条永远停在百分比上。
                            if let Some(a) = actions.transfer {
                                use crate::ui::transfer_panel::TransferUiAction;
                                match a {
                                    TransferUiAction::Cancel(id) => {
                                        if let Some(c) = self.transfer_cancels.get(&id) {
                                            c.store(true, std::sync::atomic::Ordering::Relaxed);
                                        }
                                        self.transfer_queue.cancel(id);
                                    }
                                    TransferUiAction::CancelAll => {
                                        for c in self.transfer_cancels.values() {
                                            c.store(true, std::sync::atomic::Ordering::Relaxed);
                                        }
                                        self.transfer_queue.cancel_all();
                                    }
                                    TransferUiAction::ClearFinished => {
                                        self.transfer_queue.clear_finished()
                                    }
                                }
                                self.ui_dirty = true;
                            }
                            // F53:「编辑中」面板上按下的东西。
                            if let Some(a) = actions.edit {
                                use crate::ui::edit_panel::EditUiAction;
                                match a {
                                    EditUiAction::End(key) => self.end_edit(key),
                                    EditUiAction::Resolve(key) => self.open_edit_conflict(key),
                                }
                            }
                            // F53:内置编辑器窗口。保存走**与外部编辑同一条**
                            // 回传路径(含远端变更检查)——分两条的话冲突检查
                            // 迟早只剩一条上有。
                            if let Some(a) = actions.editor {
                                use crate::ui::editor_window::EditorAction;
                                match a {
                                    EditorAction::Save => {
                                        if let Some(ed) = self.editor.as_mut() {
                                            let (key, bytes, backup) =
                                                (ed.key, ed.bytes(), ed.backup);
                                            ed.busy = true;
                                            // 用户把「留一份 .mullion.bak」关了:
                                            // 把原文丢掉,这条回传就不会带备份。
                                            if !backup {
                                                self.edit_originals.remove(&key);
                                            }
                                            self.push_edit(key, Some(bytes));
                                        }
                                    }
                                    // 关窗 = 这条编辑结束(确认丢弃已经在窗口里
                                    // 问过了)。临时文件一并收掉。
                                    EditorAction::Close(key) => self.end_edit(key),
                                }
                            }
                            // F53/D3-12:退出确认框的选择。
                            if let Some(c) = actions.exit {
                                use crate::ui::edit_panel::ExitChoice;
                                self.ui.exit_pending = false;
                                match c {
                                    // `exit_pending` 已经清掉,这一下 CloseRequested
                                    // 就穿过拦截了(见那里的注释)。
                                    ExitChoice::Anyway => {
                                        if let Some(h) = self.probe_task.take() {
                                            h.abort();
                                        }
                                        crate::edit::tempdir::purge(&self.edit_root);
                                        event_loop.exit();
                                    }
                                    ExitChoice::Cancel => {
                                        // 回去处理:把「编辑中」展开,否则用户
                                        // 关掉框之后还得自己去找那一行。
                                        self.ui.edit_expanded = true;
                                    }
                                }
                                self.ui_dirty = true;
                            }
                            // F100:导出的 Markdown 送剪贴板。写剪贴板是 IO,`ui/`
                            // 那一层只画不做 IO,所以在这里发起(同 F18 的复制路径)。
                            if let Some(md) = actions.annotate_export {
                                self.clipboard.set(&md);
                                self.ui.set_toast("标注已复制,粘进 Claude Code");
                                self.ui_dirty = true;
                            }
                            // F83 标题条开关:改的是行数,下一帧 compute_geoms
                            // 算出新 grid,再由 apply_geometry 发 window_change。
                            if self.ui.toggle_title_bars {
                                self.ui.toggle_title_bars = false;
                                if let Some(ws) = self.active_ws_mut() {
                                    ws.title_bars = !ws.title_bars;
                                }
                                self.ui_dirty = true;
                            }
                            // 菜单动作(§4.2):断开 = 关掉活动标签(单标签时即回
                            // launcher 态,与 Task 2 之前逐帧等价);退出整个事件循环。
                            // 收口顺序(先 abort 自动化再 drop workspace)在
                            // `wind_down` 里,理由见它的文档注释。
                            if self.ui.request_disconnect {
                                self.ui.request_disconnect = false;
                                self.close_active_tab();
                            }
                            if self.ui.request_quit {
                                self.ui.request_quit = false;
                                // F36:逐个走同一条收口,不靠进程退出兜底。
                                // `event_loop.exit()` 之后还要跑完本轮事件、
                                // 析构顺序也不由我们定;自动化 task 持着
                                // `Arc<SshSession>`,不显式 abort 就可能在退出
                                // 途中再往一条正在拆的 channel 上发命令。
                                for tab in self.tabs.drain() {
                                    wind_down(tab);
                                }
                                event_loop.exit();
                            }
                            // T3/T7:egui 若自己请求了下次重绘(动画/交互),按 Throttle
                            // 的方式经 next_frame_at/WaitUntil 排期,不在这里无条件
                            // request_redraw——否则一旦某帧 repaint_delay 很小,就会绕开
                            // FrameLimiter 忙转,重演 T3/T7 红线。占位 UI 静态,通常拿到
                            // Duration::MAX(不需要);仍按此路径处理以防将来 UI 变复杂。
                            if repaint_delay < std::time::Duration::MAX {
                                // 排期的那一帧必须同时标脏,否则到点重绘时
                                // `frame_is_dirty` 判 Idle,动画/交互反馈直接丢帧。
                                self.ui_dirty = true;
                                let at = Instant::now() + repaint_delay;
                                self.next_frame_at = Some(at);
                                event_loop.set_control_flow(ControlFlow::WaitUntil(at));
                            } else if self.transfer_queue.summary().busy {
                                // F59:队列在跑时自己排下一帧。不排的话画面会
                                // 冻在传输开始那一帧 —— 进度事件按 T3 刻意不
                                // 请求重绘,没有别的东西会唤醒事件循环。
                                // T7:这一支同样显式复位 control_flow。
                                self.ui_dirty = true;
                                let at = Instant::now()
                                    + std::time::Duration::from_millis(TRANSFER_UI_INTERVAL_MS);
                                self.next_frame_at = Some(at);
                                event_loop.set_control_flow(ControlFlow::WaitUntil(at));
                            } else if let Some(at) = self.next_timer_wake(now) {
                                // Important #2/T2/F125:egui 这帧不需要重绘,但有 pane 卡在
                                // 未超时的同步块里,或者光标该翻转相位了——经
                                // `next_timer_wake` 统一汇合,主动排一次到最早那个
                                // 时刻的唤醒,而不是无条件 Wait 等不相关事件顺带救回。
                                self.next_frame_at = Some(at);
                                event_loop.set_control_flow(ControlFlow::WaitUntil(at));
                            } else {
                                self.next_frame_at = None;
                                event_loop.set_control_flow(ControlFlow::Wait);
                            }
                        } else {
                            self.next_frame_at = None;
                            event_loop.set_control_flow(ControlFlow::Wait);
                        }
                    }
                    RedrawAction::Throttle { wait_ms } => {
                        let at = Instant::now() + std::time::Duration::from_millis(wait_ms);
                        self.next_frame_at = Some(at);
                        event_loop.set_control_flow(ControlFlow::WaitUntil(at));
                    }
                    RedrawAction::Idle => {
                        // Important #2/T2/F125:同上——没有脏帧不代表没有 pane 卡在同步
                        // 块里,也不代表光标不需要翻转相位。
                        if let Some(at) = self.next_timer_wake(now) {
                            self.next_frame_at = Some(at);
                            event_loop.set_control_flow(ControlFlow::WaitUntil(at));
                        } else {
                            self.next_frame_at = None;
                            event_loop.set_control_flow(ControlFlow::Wait);
                        }
                    }
                }

                // F18:拖拽出界时的自动滚动,让选区能跨越多屏 scrollback。
                //
                // 位置很讲究,三个都不能选:
                // - 挂在 `CursorMoved` 上 → 频率是鼠标事件频率,一甩就滚飞;
                // - 挂在 match 之后但不判 `presented` → Throttle 轮也会滚,而下面的
                //   排期又会唤醒下一轮,变成「一轮滚一次」的忙转(T3/T7 红线);
                // - 在这里调 `request_ui_redraw` → 它内含 `request_redraw`,同样会与
                //   `RedrawRequested` 互相触发绕开帧闸。
                //
                // 所以:只在 present 过的那一轮滚一次(频率 = 帧率 ~60fps),只标脏 +
                // 经 next_frame_at/WaitUntil 排期,由 `about_to_wait` 到点补画。
                if presented && self.dragging && self.autoscroll != 0 {
                    let lines = self.autoscroll;
                    if let Some(pane) = self.active_ws_mut().and_then(Workspace::focused_mut) {
                        pane.emulator.scroll(Scroll::Delta(lines));
                    }
                    // 滚动改了 display_offset,选区终点要按新视口重新落点,
                    // 否则拖到边缘后画面在滚、选区却停在原地不长。
                    self.update_selection_endpoint();
                    self.ui_dirty = true;
                    let at = Instant::now() + std::time::Duration::from_millis(16);
                    self.next_frame_at = Some(at);
                    event_loop.set_control_flow(ControlFlow::WaitUntil(at));
                }

                // 输入法候选框跟着终端光标走。**只在 present 过的那一帧算**、
                // 且只在位置真变了才调 —— `set_ime_cursor_area` 是跨进程系统
                // 调用,每帧无脑调是 T3 那一类问题(光标每闪一次调一遍)。
                if presented {
                    self.apply_ime_cursor_area();
                }

                // Task 6:会话管理弹窗的 intent 施加点。放在 `plan` 整块之后——此处
                // self.active/self.ws/self.ui 的借用都已释放,才能拿 `&mut
                // self.store`(egui 闭包里借不到它,只能在这里事后统一施加)。
                // `touched_store` 必须在下面那几个 `take()` 之前算:`take()`
                // 之后就问不出「刚才有没有意图」了。F61/F62 的外观缓存要在
                // 会话/分组变更后重算。
                let touched_store = self.ui.delete_request.is_some()
                    || self.ui.save_request.is_some()
                    || self.ui.group_intent.is_some()
                    || self.ui.move_to_group.is_some()
                    // F121:拖拽排序改的是会话的 group_id 与顺序 —— 跨组会换
                    // 继承来源,外观(图标/颜色)可能跟着变,必须重算全表。
                    || self.ui.reorder_request.is_some()
                    // F2:导入一次能加进几十条会话,外观缓存必须跟着重算 ——
                    // 漏掉它的话新会话在列表里画的是默认色/默认图标。
                    || self.ui.import_request.is_some();
                if self.ui.delete_request.is_some()
                    || self.ui.save_request.is_some()
                    || self.ui.move_to_group.is_some()
                    || self.ui.reorder_request.is_some()
                {
                    // keyring/TOML 是同步 IO,在事件回调里可能阻塞(Windows 凭据管理器
                    // 偶发几百 ms),打点让看门狗能指认。
                    diag::mark(diag::Stage::StoreIo);
                }
                if let Some(id) = self.ui.delete_request.take() {
                    // D3(安全属性,不是体验):先停掉引用这条会话的、**正在跑**
                    // 的隧道。会话一删,那些隧道在界面上就再也找不到了,而它们
                    // 的本机端口还 listen 着 —— 用户以为已经关掉的通路仍然开着,
                    // 且没有任何办法从界面上关掉它。
                    let stop: Vec<_> = self.store.as_ref().map_or_else(Vec::new, |s| {
                        crate::tunnels::tunnels_to_stop_on_session_delete(
                            id,
                            s.tunnels(),
                            &self.tunnels.snapshot(),
                        )
                    });
                    for tid in stop {
                        self.tunnels.stop(tid);
                    }
                    if let Some(store) = self.store.as_mut() {
                        match store.delete(id).and_then(|_| store.save()) {
                            // 走查 13:落盘成功要有一句反馈。删除尤其需要 ——
                            // 那一行从列表里消失了,但「是真删了还是我看花眼」
                            // 只有这句话能回答。
                            Ok(()) => self.ui.set_toast("已删除会话"),
                            Err(e) => self.ui.set_error(format!("删除失败:{e}")),
                        }
                    }
                }
                // 走查 3:右键「移动到分组」。它改的是继承链的上一层,所以下面
                // `touched_store` 触发的 `refresh_appearance` 必须把它算进去。
                if let Some((id, gid)) = self.ui.move_to_group.take() {
                    if let Some(store) = self.store.as_mut() {
                        match store.set_group(id, gid).and_then(|_| store.save()) {
                            Ok(()) => self.ui.set_toast("已移动到分组"),
                            Err(e) => self.ui.set_error(format!("移动分组失败:{e}")),
                        }
                    }
                }
                // F121:左栏拖拽排序。
                if let Some(i) = self.ui.reorder_request.take() {
                    if let Some(store) = self.store.as_mut() {
                        match store
                            .move_session(i.id, i.group, i.before)
                            .and_then(|_| store.save())
                        {
                            Ok(()) => self.ui.set_toast("已调整顺序"),
                            Err(e) => self.ui.set_error(format!("调整顺序失败:{e}")),
                        }
                    }
                }
                if let Some(save) = self.ui.save_request.take() {
                    if let Some(store) = self.store.as_mut() {
                        let now = time::OffsetDateTime::now_utc()
                            .format(&time::format_description::well_known::Rfc3339)
                            .unwrap_or_default();
                        let then_connect = save.then_connect;
                        match apply_save(store, save, &now) {
                            Ok(id) => {
                                // 「保存并连接」下一秒就开始拨号,状态栏自己会说话,
                                // 再飘一条「已保存」是噪音。
                                if then_connect {
                                    self.ui.connect_request = Some(id);
                                } else {
                                    self.ui.set_toast("已保存");
                                }
                            }
                            Err(msg) => self.ui.set_error(msg),
                        }
                    }
                }
                // F122:标签属性的施加点。**不碰 store** —— 只写标签自己的
                // 两个覆盖字段。放在这里(而不是渲染闭包里)的理由不变:
                // 闭包里 `self.tabs` 正被借出去画标签栏。
                if let Some(crate::ui::tab_props::TabPropsAction::Save {
                    tab_id,
                    name,
                    color,
                }) = self.ui.tab_props_save.take()
                {
                    apply_tab_props(&mut self.tabs, tab_id, name, color);
                    self.ui_dirty = true;
                }
                // 换节点的发起点。放在这里(而不是渲染闭包里)的理由同
                // `tab_props_save`:闭包里 `self.ui`/`self.active` 正被借出去。
                if let Some((pane, session)) = self.ui.rehost_request.take() {
                    self.spawn_rehost(pane, session);
                }
                // F110 隧道 CRUD 的施加点。与会话侧同构:UI 只写意图,这里才碰
                // store。**不复用** `save_request`/`delete_request` 那两条通道 ——
                // 它们带的是 `SessionDraft`/`SessionId`,类型不同,挤在一起只能靠
                // 运行时判别,而两类对象删错了的后果并不一样。
                if let Some(id) = self.ui.tunnel_delete_request.take() {
                    if let Some(store) = self.store.as_mut() {
                        match store.delete_tunnel(id).and_then(|_| store.save()) {
                            Ok(()) => {
                                // 删掉的正好是右栏正在编辑的那条 → 清空表单,
                                // 否则表单还留着一条已经不存在的隧道,再点保存
                                // 会以「更新」的语义去改一个不存在的 id。
                                if self.ui.tunnel_editor_id == Some(id) {
                                    self.ui.tunnel_editor_id = None;
                                    self.ui.tunnel_editor = None;
                                    self.ui.tunnel_editor_baseline = None;
                                }
                                self.ui.set_toast("已删除隧道");
                            }
                            Err(e) => self.ui.set_error(format!("删除隧道失败:{e}")),
                        }
                    }
                }
                // F111 启停。**必须在这里、不能在 egui 闭包里**:要 store、要
                // tokio runtime、要 bind 端口,三样在闭包里都够不着。
                if let Some(id) = self.ui.tunnel_stop_request.take() {
                    self.tunnels.stop(id);
                    self.ui.set_toast("已停止隧道");
                }
                if let Some(id) = self.ui.tunnel_start_request.take() {
                    self.start_tunnel(id);
                }
                if let Some(intent) = self.ui.tunnel_save_request.take() {
                    if let Some(store) = self.store.as_mut() {
                        let result = match intent.editing_id {
                            Some(id) => store.update_tunnel(id, intent.draft).map(|_| id),
                            None => Ok(store.add_tunnel(intent.draft)),
                        };
                        match result.and_then(|id| store.save().map(|_| id)) {
                            Ok(id) => {
                                // 新建保存后把编辑器切到刚分配的 id,否则再点一次
                                // 「保存」会**又新建一条**。
                                self.ui.tunnel_editor_id = Some(id);
                                self.ui.tunnel_editor_baseline = self.ui.tunnel_editor.clone();
                                self.ui.set_toast("已保存隧道");
                            }
                            Err(e) => self.ui.set_error(format!("保存隧道失败:{e}")),
                        }
                    }
                }
                // F74 凭据 CRUD 的施加点。同上,UI 只写意图,这里才碰 store。
                if let Some(id) = self.ui.credential_delete_request.take() {
                    if let Some(store) = self.store.as_mut() {
                        match store.delete_credential(id) {
                            Ok(()) => match store.save() {
                                Ok(()) => {
                                    // 删掉的正好是右栏在编辑的那份 → 清表单,
                                    // 否则再点保存会以「更新」的语义去改一个
                                    // 已经不存在的 id(同隧道侧)。
                                    if self.ui.credential_editor_id == Some(id) {
                                        self.ui.credential_editor_id = None;
                                        self.ui.credential_editor = None;
                                        self.ui.credential_editor_baseline = None;
                                    }
                                    self.ui.set_toast("已删除凭据");
                                }
                                Err(e) => self.ui.set_error(format!("删除凭据失败:{e}")),
                            },
                            // 被引用时把引用者的**会话名**报出来(D7)。
                            Err(e) => {
                                let msg = credential_delete_error(&e, store.list());
                                self.ui.set_error(msg);
                            }
                        }
                    }
                }
                if let Some(intent) = self.ui.credential_save_request.take() {
                    if let Some(store) = self.store.as_mut() {
                        match apply_credential_save(store, intent) {
                            Ok(id) => {
                                self.ui.credential_editor_id = Some(id);
                                self.ui.credential_editor_baseline =
                                    self.ui.credential_editor.clone();
                                self.ui.set_toast("已保存凭据");
                            }
                            Err(msg) => self.ui.set_error(msg),
                        }
                    }
                }
                // F2:ssh config 导入的施加点。与会话保存同构 —— 弹窗只交出
                // 勾好的行,落库(含跳板两阶段回填)在 `apply_import`。
                if let Some(rows) = self.ui.import_request.take() {
                    if let Some(store) = self.store.as_mut() {
                        let now = time::OffsetDateTime::now_utc()
                            .format(&time::format_description::well_known::Rfc3339)
                            .unwrap_or_default();
                        match apply_import(store, &rows, &now) {
                            Ok(n) => self.ui.set_toast(format!("已导入 {n} 条会话")),
                            Err(msg) => self.ui.set_error(msg),
                        }
                    }
                }
                // Task 16:分组管理弹窗的 intent 施加点(F60)。`delete_group` 已在
                // store 层把仍引用该分组的会话 group_id 置 None(vault.rs 的
                // `delete_group` 文档:「分组是组织手段,不是会话的所有者」)、不删会话,
                // 所以这里不需要额外处理悬空引用。
                if let Some(intent) = self.ui.group_intent.take() {
                    if let Some(store) = self.store.as_mut() {
                        match intent {
                            crate::ui::group_manager::GroupIntent::Add(name) => {
                                store.add_group(name);
                            }
                            crate::ui::group_manager::GroupIntent::Rename(id, name) => {
                                store.rename_group(id, name);
                            }
                            crate::ui::group_manager::GroupIntent::Delete(id) => {
                                if let Err(e) = store.delete_group(id) {
                                    self.ui.set_error(e.to_string());
                                }
                            }
                        }
                        if let Err(e) = store.save() {
                            self.ui.set_error(e.to_string());
                        }
                    }
                }
                // F61/F62:会话增删改、分组增删改名都可能改变外观继承链
                // (删掉分组 → 会话回落到自己那一层),缓存跟着重算。
                //
                // **必须门控在 `touched_store` 后面**:这段每帧都跑,无条件调
                // `refresh_appearance` 就是每帧对所有会话重跑 `inherit::resolve`
                // —— 正是这个缓存要防的陷阱 T3。
                //
                // 不管成功还是失败都重算:失败路径上 store 可能已经改了一半
                // (比如 `delete` 成功但 `save` 失败),按实际状态重算才是对的。
                if touched_store {
                    self.refresh_appearance();
                }
                // 「选择…」私钥文件:同样是 egui 闭包只记意图、这里才施加。
                if std::mem::take(&mut self.ui.pick_key_request) && !self.key_picker_busy {
                    self.key_picker_busy = true;
                    self.spawn_file_picker("选择私钥文件", None, None, UserEvent::KeyPathPicked);
                }
                // 凭据表单里的「选择…」私钥。与会话侧共用 `key_picker_busy`
                // (一次只该开一个对话框),但回送的是**另一个事件** ——
                // 正文要写进哪个缓冲,只有事件变体分得清。
                if std::mem::take(&mut self.ui.pick_credential_key_request) && !self.key_picker_busy
                {
                    self.key_picker_busy = true;
                    self.spawn_file_picker(
                        "选择私钥文件",
                        None,
                        None,
                        UserEvent::CredentialKeyPathPicked,
                    );
                }
                // 「导入 .ico…」:同上。加扩展名过滤,免得用户选中 .png 才被
                // 告知不行 —— 归一化只吃 .ico 容器。
                if std::mem::take(&mut self.ui.pick_icon_request) && !self.icon_picker_busy {
                    self.icon_picker_busy = true;
                    self.spawn_file_picker(
                        "选择图标文件",
                        Some(("图标", &["ico"])),
                        None,
                        UserEvent::IconPathPicked,
                    );
                }
                // F2:导入 ssh config。初始目录指向 `~/.ssh`(设计 D7),
                // 省掉用户手动翻目录;不加扩展名过滤 —— config 文件没有后缀。
                if std::mem::take(&mut self.ui.import_pick_request) && !self.import_picker_busy {
                    self.import_picker_busy = true;
                    self.spawn_file_picker(
                        "选择 ssh config 文件",
                        None,
                        crate::ui::session_manager::keyscan::default_ssh_dir(),
                        UserEvent::SshConfigPicked,
                    );
                }
                // 连接:双击行 / 点「连接」。必须在 store 的 &mut 借用结束后调
                // (下面 `self.store.as_ref()` 的临时借用在 match 表达式求值完就
                // 释放,故可紧接着调 self.spawn_connect)。
                // F44:**无条件**取走跳过标志 —— 哪怕这一帧没有连接意图(右键
                // 点了又关掉菜单),也不能让它漂到下一次连接上。
                let skip_automation = std::mem::take(&mut self.ui.connect_skip_automation);
                if let Some(id) = self.ui.connect_request.take() {
                    self.ui.connect_request_last = Some(id);
                    // D1/F50:`dial_plan_for` 多带回 `wants_sftp`——点「连接」
                    // 要靠它决定 `ConnectOk` 抵达时开终端标签还是文件标签。
                    match self.store.as_ref().map(|s| s.dial_plan_for(id)) {
                        Some(Ok((cfg, wants_sftp))) => {
                            // 用户主动发起的连接是交互态,不该继承 CLI 直连的
                            // exit(1) 语义(复核 #1)。
                            self.cli_direct = false;
                            // 跳过标志必须跟 `pending_automation` 同进同退 ——
                            // 后者只在 `spawn_connect` 里写。写在 match 外面的话,
                            // 一次**失败**的连接尝试(配置坏了走 Err 支)会把标志
                            // 留给另一条还在途的连接:用户没点过跳过,那条连接的
                            // 自动化却被 `take_pending` 静默丢掉。
                            self.pending_skip_automation = skip_automation;
                            self.spawn_connect(cfg, wants_sftp);
                        }
                        Some(Err(e)) => self.ui.set_error(e.to_string()),
                        None => {}
                    }
                }
                // F3:主机密钥弹窗的回答。record + save 必须在 GUI 线程做——
                // store 是同步 IO,而且失败要能落进 last_error 让用户看见。
                if let Some(accept) = self.ui.host_key_reply.take() {
                    if let Some(prompt) = self.pending_host_key.take() {
                        self.host_key_since = None;
                        // 落盘失败不阻断本次连接:指纹已在内存表里,连接照常;
                        // 代价只是下次启动会再问一遍。last_error 的展示位可能
                        // 随后被别的事件(如 ConnectOk 关掉会话管理器)挪走,
                        // 磁盘日志(ADR-008)是这类静默失败的兜底取证手段。
                        if let Err(e) =
                            crate::host_key::persist_if_allowed(&self.known_hosts, &prompt, accept)
                        {
                            crate::logx::line(&format!("主机指纹落盘失败:{e}"));
                            self.ui
                                .set_error(format!("主机指纹未能保存:{e}(本次连接不受影响)"));
                        }
                        // 送回握手线程。Err = 对端已走(超时/断开),没什么可做的。
                        let _ = prompt.reply.send(accept);
                    }
                }
                // F18:粘贴确认弹窗的回答。放在这里而不是 egui 闭包里——发送要
                // `&mut self.ws`,闭包里借不到(与会话管理器/主机密钥同构)。
                if let Some(accept) = self.ui.paste_reply.take() {
                    if let Some(text) = self.pending_paste.take() {
                        if accept {
                            self.send_paste(&text);
                        }
                    }
                }
                // F92:「测试连接」。取消优先于发起 —— 同一帧里既取消又点测试的
                // 唯一可能是切换会话时手抖,那也该以新表单为准。
                if std::mem::take(&mut self.ui.probe_cancel) {
                    crate::app::cancel_probe(&mut self.probe_epoch, &mut self.ui.probe);
                    if let Some(h) = self.probe_task.take() {
                        h.abort();
                    }
                }
                if std::mem::take(&mut self.ui.probe_click) {
                    match self.build_probe_config() {
                        Ok(cfg) => {
                            self.ui.probe = crate::ui::session_manager::ProbeState::Running;
                            self.spawn_probe(cfg);
                        }
                        Err(msg) => {
                            self.ui.probe = crate::ui::session_manager::ProbeState::Err(msg);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // 只有 Throttle 安排的 deadline 真到点了才补一次 request_redraw,
        // 把被节流的那帧刷出来;不到点就什么也不做——不忙转。
        if let Some(at) = self.next_frame_at {
            if Instant::now() >= at {
                self.next_frame_at = None;
                if let Some(a) = &self.active {
                    a.window.request_redraw();
                }
            }
        }
        // F37:到点就比一比布局有没有变、变了就写盘(E7)。放在这里而不是
        // 帧循环里 —— 它跟渲染无关,而 `about_to_wait` 是「已经闲下来了」
        // 这个语义唯一准确的位置。
        self.flush_layout_if_due();
        // F124:到点就配一遍远端 tmux 的状态上报。跟布局落盘同一个理由放在
        // 这里 —— 它跟渲染无关,而 `about_to_wait` 是「已经闲下来了」这个
        // 语义唯一准确的位置。
        self.tick_tmux_bootstrap();
        // 即将阻塞等事件 = 正常空闲。看门狗据此不误报(等事件本来就可以等很久)。
        diag::mark(diag::Stage::Idle);
    }
}

/// 本帧 UI 产生的布局动作(点了工具栏的预设按钮 / 点了某个 pane 标题条的 ×)
/// 路由到 `Workspace` 上。只碰 `&mut Workspace`,不碰 `App` 的 `_runtime`/
/// `proxy` 字段 —— 这是刻意的:`EventLoopProxy` 在本仓库的无头测试容器里
/// 造不出来(经验证实,见 `host_key.rs:131` 附近同样绕开它的先例),把"点了
/// 哪个预设就该切到哪棵树""关了哪个 pane 就该少哪个 id"这两条真正的路由
/// 逻辑摘出来单独放在这个自由函数里,才能拿一个真实构造的 `Workspace` 直接
/// 单测,而不必伪造一个绕开被测代码的假测试。
///
/// 返回 `None` 表示这一帧没有布局动作,调用方不需要动 `current_preset`/标脏/
/// 开新 channel。返回 `Some((新增待开 channel 的 pane id, 新的 current_preset))`——
/// 后者在只点了预设时是 `Some(preset)`,在点了关闭(不论是否同帧还点了预设)时
/// 是 `None`:手动关掉一个 pane 后,布局不再对应任何预设。真正开 channel 需要
/// `_runtime`/`proxy`,留给调用方的 `App::spawn_fresh_panes`。
fn apply_layout_actions(
    ws: &mut Workspace,
    actions: &crate::ui::UiActions,
) -> Option<(Vec<PaneId>, Option<Preset>)> {
    if actions.preset.is_none() && actions.close_pane.is_none() {
        return None;
    }
    let mut fresh = Vec::new();
    let mut preset_out = None;
    let mut changed = false;
    if let Some(preset) = actions.preset {
        fresh = ws.apply_preset(preset);
        preset_out = Some(preset);
        changed = true;
    }
    if let Some(id) = actions.close_pane {
        // close_pane 在「只剩最后一个 pane」时会拒绝并返回 false、树不变——这种
        // 情况下不该清掉 preset_out,否则工具栏的当前预设高亮会被平白抹掉,
        // 而树其实什么都没变。
        if ws.close_pane(id) {
            preset_out = None;
            changed = true;
        }
    }
    if !changed {
        return None;
    }
    Some((fresh, preset_out))
}

/// 晚到的 `PaneOpened` 是否还该被 attach。两个独立的理由都会让答案是"不该":
///
/// 1. 用户又切了一次预设——`apply_preset` 把等待中的叶子从树上摘掉了,`id`
///    已经不在树上;attach 上去是一个渲染/标题条都看不见、但 `Workspace::pump`
///    仍在每帧驱动的孤儿 pane,SSH channel 永远不关,直到整条连接断开。
/// 2. 用户断开又重连(C1)——`id` 还在树上,甚至已经被新世代自己的
///    `PaneOpened` 正常 attach 过,但这是**旧世代**的事件(`next_id` 每次
///    重连都从 2 重新计数,两代的 `id` 必然会撞号);只看 id/树成员会误判为
///    "还需要",实际 attach 上去会顶掉新世代刚建好、正常工作的 `PaneState`。
///
/// F36 之后理由 2 已经**先**被 `Tabs::by_generation_mut` 挡了一道(查不到属主
/// 标签就根本走不到这里),这里的世代比对成了深度防御:它挡的是「将来有人把
/// 路由改回活动标签」这类回退,代价是每开一条 channel 一次整数比较。
///
/// 纯函数(只读 `&Workspace`),不碰 `EventLoopProxy`,可脱离真实事件循环单测。
fn pane_still_wanted(ws: &Workspace, id: PaneId, generation: u64) -> bool {
    ws.generation() == generation && mullion_core::layout::leaves(ws.tree()).contains(&id)
}

/// 把一块已有的 pane 改挂到刚连上的另一台机器上(用户报的问题 2)。
///
/// 返回 `false` = 这块 pane 已经不在了(拨号途中被关掉 / 用户切了预设 / 断开
/// 重连换了世代),调用方该让新开的 channel 自然 Drop —— 挂上去就是一个渲染
/// 看不见、`pump` 却仍在驱动的孤儿(同 `pane_still_wanted` 的第 1 条理由)。
///
/// **`emulator` 必须换新的,不能沿用**:回滚缓冲里全是上一台机器的输出,留着
/// 等于让用户往上一翻就看到另一台机器的内容,而屏幕上的提示符是新机器的 ——
/// 这种「看起来对、其实错」的画面比黑屏危险得多。
///
/// **旧的 `HostConn` 留在 `hosts` 里不摘**:`host_ix` 是**下标即身份**,摘掉
/// 一条会让排在它后面的所有 pane 的 `host_ix` 集体错位(指向另一台机器,输入
/// 照发)。`hosts[0]` 还额外被 sftp 侧栏和 `last_cfg` 认着。代价是那条连接闲
/// 置到整个标签关闭为止——换节点是低频操作,拿一条闲置连接换掉一整类静默的
/// 错位 bug 是划算的。
///
/// `host_ix` 由调用方现算(`ws.hosts.len() - 1`,刚 push 进去那条)。**不在这里
/// push `HostConn`**:那个类型里揣着 `Arc<SshConnection>`,而 `SshConnection`
/// 的字段是私有的、只能由真实握手造出来 —— 收它当参数等于把这整段判定推出
/// 单测范围。收一个已经算好的下标,这段就能拿真实构造的 `Workspace` 直接测。
///
/// 纯函数(只碰 `&mut Workspace`),不要 runtime/proxy,可脱离真实事件循环单测。
fn rehost_pane(
    ws: &mut Workspace,
    id: PaneId,
    generation: u64,
    host_ix: usize,
    pty: Box<dyn crate::shell::workspace::PtyWriter>,
    rx: Receiver<Vec<u8>>,
) -> bool {
    if !pane_still_wanted(ws, id, generation) {
        return false;
    }
    let Some(p) = ws.pane_mut(id) else {
        // `pane_still_wanted` 只保证 id 在**树**上;`PaneState` 是另一回事
        // (分屏刚切出来、channel 还没开好的叶子就没有)。换节点的入口是
        // pane 标题条,只有画得出来的 pane 才有标题条,所以实际到不了这里。
        return false;
    };
    let mut emulator = mullion_term::emulator::Emulator::new(80, 24);
    let d = theme::term_default_colors(&MULLION_DARK);
    emulator.set_default_colors(d.fg, d.bg);
    p.host_ix = host_ix;
    p.emulator = emulator;
    // 旧的 `pty`/`rx` 在这两句赋值里被 Drop —— Drop 即关掉上一台机器的
    // channel,不留孤儿。
    p.pty = pty;
    p.rx = rx;
    p.pacer = SyncFramePacer::new();
    p.status = crate::shell::workspace::PaneStatus::Live;
    // 自动化的「就绪」判据要重新攒(新机器还一个字节都没说话);`last_grid`
    // 给不可能的初值,逼下一帧 apply_geometry 发一次 window_change(T4)——
    // 新开的 channel 是 80x24,不发的话远端按 80x24 排版。
    p.saw_first_byte = false;
    p.last_grid = (0, 0);
    // ⑥:`cwd`/`tmux` 同 `emulator` 一个道理——旧值是上一台机器嗅出来的
    // (OSC 7 目录 / 窗口标题里的 tmux 会话名),留着会在标题条右区挂一条
    // 「看起来对、其实属于上一台机器」的过期标注,而且 `cwd` 是"只增不清"
    // 语义(见字段注释),不会被新机器的输出自然覆盖掉一个空值,必须在这里
    // 主动清空。
    p.cwd = None;
    p.tmux = None;
    true
}

/// F123 补缺口:把 `~` / `~/x` 拿远端的**真登录目录**展开成绝对路径。
///
/// 为什么需要:Ubuntu 默认 bash 报的标题是 `user@host: ~/Mullion`,而 openssh 的
/// `sftp-server` **不展开 `~`** —— 直接拿去 `canonicalize` 会失败,面板停在
/// 「取不到登录目录」,比不继承更糟。
///
/// 只认恰好是 `~` 和以 `~/` 开头两种。**`~user` 不展开**:那要查远端的 passwd,
/// 我们不知道,猜错会把用户带到别人的家目录去。已经是绝对路径的返回 `None` ——
/// 那一档由调用方更优先地处理。
fn expand_tilde(cwd: &[u8], home: &[u8]) -> Option<Vec<u8>> {
    let rest = match cwd {
        b"~" => return Some(home.to_vec()),
        _ => cwd.strip_prefix(b"~/")?,
    };
    // 接缝两侧的多余斜杠都剥掉再拼,否则 home=`/` + rest=`x` 会拼出 `//x`
    // (POSIX 4.13:**恰好**两个前导斜杠由实现自行解释)。两侧都要剥:home 侧
    // 是 `/` 结尾,cwd 侧是 `~//x` 这种冗余写法,只堵一侧另一侧照样漏。
    let mut out = home.to_vec();
    while out.last() == Some(&b'/') {
        out.pop();
    }
    out.push(b'/');
    out.extend_from_slice(rest.strip_prefix(b"/").unwrap_or(rest));
    Some(out)
}

/// ② 文件面板远端栏该开在哪。
///
/// 优先级:焦点 pane 报出来的当前目录(绝对路径)> 展开 `~` 后的目录
/// (需要已知登录目录,见 [`expand_tilde`])> F120 配置的默认远端目录 >
/// `None`(交给 [`configured_remote_dir`] 落回 `"."`,也就是登录目录)。
///
/// 标题里拿到的常常是 `~/Mullion` 这种缩写,而 openssh 的 `sftp-server`
/// **不展开 `~`** —— 直接拿去 `canonicalize` 会失败,面板会停在
/// 「取不到登录目录」,比不继承更糟。`home` 已知时会先把它展开成绝对路径;
/// `home` 未知(sftp 还没开)时不猜,落回配置值。非 UTF-8 的远端路径
/// (展开前后都算)同样落回配置值(`spawn_sftp_open` 收 `Option<String>`);
/// 标题条那边仍会 lossy 显示,见 `pane_title::dir_leaf`。
fn files_start_dir(
    pane_cwd: Option<&[u8]>,
    default_remote: Option<&str>,
    home: Option<&[u8]>,
) -> Option<String> {
    let absolute = |c: &[u8]| -> Option<String> {
        if !c.starts_with(b"/") {
            return None;
        }
        String::from_utf8(c.to_vec()).ok()
    };
    let from_pane = pane_cwd.and_then(|c| {
        absolute(c).or_else(|| {
            let home = home?;
            absolute(&expand_tilde(c, home)?)
        })
    });
    from_pane.or_else(|| default_remote.map(str::to_string))
}

/// `App::sync_files_to_focused_pane` 的纯逻辑核心。原来那四个早退(有没有
/// 属主世代 / sftp 是否已连 / 焦点 pane 有没有报出绝对路径)全埋在
/// `&mut self` 方法体里,完全靠人读代码——顺序换掉,或者把 `has_client`
/// 判断写反,都不会有任何测试变红。按本文件其余 `_of` 函数的惯例抽出来,
/// 方法体只留取数据 + 调用 + 派发。
///
/// 不接第四个「配置的默认远端目录」参数(`files_start_dir` 第二参在调用点
/// 固定传 `None`):面板已经开着了,拿不到 pane 目录时退回配置值会把用户
/// 当前的导航位置拽走,宁可什么都不做——这是 `sync_files_to_focused_pane`
/// 与 `trigger_sftp_open`(会传 `default_remote` 兜底)刻意不同的地方。
///
/// 返回 `String` 而不是 `mullion_ssh::sftp::RemotePath`:后者的构造在
/// `mullion-ssh`,这里保持零依赖更好测;调用方自己转。返回 `None` = 这一次
/// 不同步(面板停在原处)。
fn sync_target_of(
    gen: Option<u64>,
    has_client: bool,
    pane_cwd: Option<&[u8]>,
    home: Option<&[u8]>,
) -> Option<(u64, String)> {
    let gen = gen?;
    if !has_client {
        // sftp 还没开:`trigger_sftp_open` 稍后自己会读焦点 pane 的 cwd 定
        // 起始目录,这里现在发 Goto 只会打到一条还不存在的连接上。
        return None;
    }
    // 第二参固定 `None`:面板已经开着了,退回配置的默认远端目录会把用户
    // 当前的导航位置拽走——拿不到 pane 目录就宁可什么都不做。
    let dir = files_start_dir(pane_cwd, None, home)?;
    Some((gen, dir))
}

/// F120:`spawn_sftp_open` 该从哪个目录起步——配置了默认远端目录(编辑器
/// 「SFTP」分节)就用它,没配置(`None`)落回登录目录(`.`)。
///
/// 抽成纯函数是为了能脱离 `Runtime`/`EventLoopProxy` 单测这条判定
/// (`tests::configured_remote_dir_falls_back_to_login_directory_when_unset`)——
/// `spawn_sftp_open` 本身两者都要,单测里造不出来。
fn configured_remote_dir(configured: Option<&str>) -> mullion_ssh::sftp::RemotePath {
    let dir = configured.unwrap_or(".");
    mullion_ssh::sftp::RemotePath::from_bytes(dir.as_bytes().to_vec())
}

/// F50/D6:异步开一条 sftp channel + 取登录目录。结果经 `UserEvent::SftpOpened`
/// 回送(`App::accept_sftp_opened` 接,按世代路由,S1)。
///
/// **写成自由函数,只取 `runtime`/`proxy`,不取 `&mut App`**:调用点
/// (`App::trigger_sftp_open`)常常还攥着 `self.tabs.by_generation_mut(..)`
/// 拿到的 `&mut TerminalTab`——若这里是 `&mut self` 的方法,借用检查器会把
/// 两者锁在一起过不了(同 `apply_layout_actions`/`pane_still_wanted` 拆成自由
/// 函数的理由)。错误在这里就转成用户看得懂的中文,不把 `SftpError` 的
/// Debug 输出丢给用户。
///
/// **返回 `JoinHandle`,调用方必须存进 `TerminalTab::sftp_tasks`**——这里面
/// `SftpClient::open` 的 `channel_open_session()`/`request_subsystem()` 两步
/// 是裸的 russh 调用,**没有任何超时包裹**(不像 `list_dir`/`canonicalize`
/// 那样受 russh-sftp 默认 10s 请求超时约束),链路黑洞(本项目高延迟代理链路
/// 的头号场景)时可能无限期挂着。丢弃这个句柄 = 关标签时收不了口,任务会继续
/// 攥着 `Arc<SshConnection>` 撑住本该断开的连接(见 `wind_down`)。
///
/// `default_remote`(F120):会话编辑器「SFTP」分节配置的默认远端目录,
/// `pane_cwd`(F123):焦点 pane 报出来的当前目录。两者都原样递进来,起始
/// 目录的计算(`files_start_dir`)挪到了这里 —— `~` 展开要用远端的真登录
/// 目录,而那个值只有 `canonicalize(".")` 回来之后才知道,调用方那一侧
/// 算不了。
fn spawn_sftp_open(
    runtime: &Runtime,
    proxy: &EventLoopProxy<UserEvent>,
    generation: u64,
    handle: Arc<SshConnection>,
    default_remote: Option<String>,
    pane_cwd: Option<Vec<u8>>,
) -> tokio::task::JoinHandle<()> {
    let proxy = proxy.clone();
    runtime.spawn(async move {
        let result = match mullion_ssh::sftp::SftpClient::open(handle).await {
            Ok(client) => {
                let dot = mullion_ssh::sftp::RemotePath::from_bytes(b".".to_vec());
                match client.canonicalize(&dot).await {
                    Ok(home) => {
                        // F123:`~` 只有在这里才展得开 —— 登录目录要等
                        // `canonicalize(".")` 回来才知道,调用方那一侧算不了。
                        let from_pane = pane_cwd
                            .as_deref()
                            .and_then(|c| files_start_dir(Some(c), None, Some(home.as_bytes())));
                        let configured = configured_remote_dir(default_remote.as_deref());

                        // pane 报的目录**打不开就降级**,不判整体失败:标题由
                        // tmux 异步报上来,那个目录可能刚被删掉/权限变了。继承
                        // 目录是锦上添花,不该让文件面板整个打不开。配置值
                        // (F120)则相反 —— 它打不开必须报出来,静默忽略用户
                        // 配置正是本项目最不想要的失效方式。
                        let mut dir = None;
                        if let Some(p) = from_pane {
                            let p = mullion_ssh::sftp::RemotePath::from_bytes(p.into_bytes());
                            match client.canonicalize(&p).await {
                                Ok(d) => dir = Some(d),
                                Err(e) => log::debug!(
                                    target: "mullion",
                                    "pane 报的目录打不开,退回默认起始目录:{e}"
                                ),
                            }
                        }
                        match dir {
                            Some(dir) => Ok((Arc::new(client), home, dir)),
                            // 没配默认目录时起点就是登录目录,省掉第二次往返
                            // (高延迟链路上一次 RTT 是能看出来的)。
                            None if configured.as_bytes() == b"." => {
                                Ok((Arc::new(client), home.clone(), home))
                            }
                            None => match client.canonicalize(&configured).await {
                                Ok(dir) => Ok((Arc::new(client), home, dir)),
                                Err(e) => Err(format!("SFTP 已连上,但打不开起始目录:{e}")),
                            },
                        }
                    }
                    // 这一步失败时 channel **已经开成功了**,只是取不到登录
                    // 目录。跟上面那条共用「打开 SFTP 失败」会把排查方向带偏
                    // 到连接/认证层,而真实原因通常在权限或远端 `.` 不可 stat。
                    Err(e) => Err(format!("SFTP 已连上,但取不到登录目录:{e}")),
                }
            }
            Err(e) => Err(format!("打开 SFTP 失败:{e}")),
        };
        let _ = proxy.send_event(UserEvent::SftpOpened { generation, result });
    })
}

/// F53:本地临时文件此刻的样子。`None` = 不在了(用户自己删了 / 编辑器
/// 换了 inode 又没落回原名)。
///
/// mtime 取**毫秒**:秒级分不出「同一秒内保存两次」,而脚本化的编辑
/// (`sed -i` 连着跑两遍)就是这个节奏。
fn local_stamp(path: &std::path::Path) -> Option<crate::edit::sessions::LocalStamp> {
    let md = std::fs::metadata(path).ok()?;
    let ms = md
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis() as u64;
    Some((ms, md.len()))
}

/// F53/D3-7:备份落在**同目录**、原名加后缀。不放临时目录 —— 用户找回它
/// 的时候人在远端那台机器上,不在我们的临时目录里。
fn backup_path(remote: &mullion_ssh::sftp::RemotePath) -> mullion_ssh::sftp::RemotePath {
    let mut bytes = remote.as_bytes().to_vec();
    bytes.extend_from_slice(b".mullion.bak");
    mullion_ssh::sftp::RemotePath::from_bytes(bytes)
}

/// F53:「另存为副本」的落点。
fn copy_path(remote: &mullion_ssh::sftp::RemotePath) -> mullion_ssh::sftp::RemotePath {
    let mut bytes = remote.as_bytes().to_vec();
    bytes.extend_from_slice(b".mullion-copy");
    mullion_ssh::sftp::RemotePath::from_bytes(bytes)
}

/// F53/D3-8:回传一次。真正的「先比对再覆盖」在 `SftpClient::write_if_unchanged`
/// 里(那一层才够得着假服务端做端到端守护),这里只负责把 `.mullion.bak`
/// 的落点算出来、把协议错误翻成人话。
async fn write_back(
    client: &mullion_ssh::sftp::SftpClient,
    remote: &mullion_ssh::sftp::RemotePath,
    bytes: &[u8],
    snapshot: crate::edit::sessions::RemoteStamp,
    backup: Option<Vec<u8>>,
) -> Result<EditWriteOutcome, String> {
    let bak = backup.map(|orig| (backup_path(remote), orig));
    let outcome = client
        .write_if_unchanged(
            remote,
            bytes,
            snapshot,
            bak.as_ref().map(|(p, b)| (p, b.as_slice())),
        )
        .await
        .map_err(|e| format!("回传失败:{e}"))?;
    Ok(match outcome {
        mullion_ssh::sftp::WriteOutcome::Written { mtime, size } => {
            EditWriteOutcome::Done((mtime, size))
        }
        mullion_ssh::sftp::WriteOutcome::Conflict { mtime, size } => {
            EditWriteOutcome::Conflict((mtime, size))
        }
    })
}

/// F53:把要编辑的文件整个读回来,并取回**读完那一刻**的远端戳。
///
/// `stat` 放在读之后:戳的用途是回传前判「远端有没有被别人改过」,它必须
/// 描述我们手上这份内容对应的那个版本。读之前 stat 的话,读的过程中(几十
/// MB 走高延迟链路可能好几秒)对方改了文件,我们会拿着旧戳 + 撕裂的内容,
/// 回传时判定「没人动过」直接覆盖 —— 那正是这套机制要防的事。
async fn read_for_edit(
    client: &mullion_ssh::sftp::SftpClient,
    path: &mullion_ssh::sftp::RemotePath,
    limit: u64,
) -> Result<(Vec<u8>, crate::edit::sessions::RemoteStamp), String> {
    let bytes = client
        .read_all(path, limit)
        .await
        .map_err(|e| format!("读取文件失败:{e}"))?;
    let st = client
        .stat(path)
        .await
        .map_err(|e| format!("读取文件属性失败:{e}"))?;
    Ok((bytes, (st.mtime, st.size)))
}

/// F50:异步列一次目录。结果经 `UserEvent::SftpListed` 回送(`App::accept_sftp_listed`
/// 接)。`seq` 原样带上,回来时对着 `PaneState::request_seq` 校验——用户点得
/// 比网络快时,后发先至的旧结果据此被丢弃。自由函数的理由同 `spawn_sftp_open`。
///
/// 同样**返回 `JoinHandle`,调用方必须存进 `TerminalTab::sftp_tasks`**——理由
/// 同 `spawn_sftp_open`(这里的 `list_dir` 本身受 russh-sftp 默认 10s 请求超时
/// 约束,但收口仍然要靠 `wind_down`:用户可能就是想在这 10s 窗口内立刻关掉
/// 标签、立刻断开连接,而不是等超时)。
fn spawn_sftp_list_dir(
    runtime: &Runtime,
    proxy: &EventLoopProxy<UserEvent>,
    generation: u64,
    client: Arc<mullion_ssh::sftp::SftpClient>,
    dir: mullion_ssh::sftp::RemotePath,
    seq: u64,
) -> tokio::task::JoinHandle<()> {
    let proxy = proxy.clone();
    runtime.spawn(async move {
        let result = client
            .list_dir(&dir)
            .await
            .map_err(|e| format!("读取目录失败:{e}"));
        let _ = proxy.send_event(UserEvent::SftpListed {
            generation,
            seq,
            result,
        });
    })
}

/// 采纳一次拨测结果:世代号对得上才写状态。返回是否采纳。
///
/// 抽成自由函数是为了能脱离窗口/运行时单测 —— 事件循环里那一大坨
/// 是本项目最难测的地方,判定逻辑绝不能埋在里面。
pub(crate) fn accept_probe(
    epoch: u64,
    current: u64,
    state: &mut crate::ui::session_manager::ProbeState,
    outcome: Result<(), String>,
) -> bool {
    use crate::ui::session_manager::ProbeState;
    if epoch != current {
        return false;
    }
    *state = match outcome {
        Ok(()) => ProbeState::Ok,
        Err(msg) => ProbeState::Err(msg),
    };
    true
}

/// 取消在途拨测:自增世代号让迟到的结果失效,并清掉已作废的结论。
///
/// 两件事缺一不可。只自增世代号的话,在「切会话」与「施加取消意图」之间
/// 到达的结果仍会被 `accept_probe` 采纳(那一刻世代号还没变),把一个已经
/// 不对应任何表单的「连接成功」永久留在 `ui.probe` 上 —— 用户下次编辑
/// 无关会话时会看到凭空冒出的绿色成功卡片。
pub(crate) fn cancel_probe(epoch: &mut u64, state: &mut crate::ui::session_manager::ProbeState) {
    *epoch = epoch.wrapping_add(1);
    *state = crate::ui::session_manager::ProbeState::Idle;
}

/// Important #2:`App::sync_timeout_wake` 的核心决策抽成自由函数——只吃
/// `start`(`App::now_ms` 的同一时钟基准)/`Option<&Workspace>`/`now_ms`,
/// 不碰 `&App` 本身,因此不需要能构造 `EventLoopProxy`(本容器里构造不出来,
/// 见 `apply_layout_actions` 同样的理由)就能单测「该不该唤醒、唤醒到哪个
/// 时刻」这条决策路径。真正判断"哪个 pane、超时到几点"的逻辑在
/// `render::earliest_sync_timeout_ms`(已单测),这里只做时钟基准换算。
fn sync_timeout_wake_at(start: Instant, ws: Option<&Workspace>, now_ms: u64) -> Option<Instant> {
    let ws = ws?;
    let pacer =
        crate::render::earliest_sync_timeout_ms(ws.panes().iter().map(|p| &p.pacer), now_ms)
            .map(|ms| start + std::time::Duration::from_millis(ms));
    // T2 的另一半:`SyncFramePacer` 管的是「出不出帧」,而 vte 的 `Processor`
    // 在 BSU 之后把字节攒在自己肚子里,`Term` 压根没见过——那个超时点同样得
    // 有人到点来问一次(`Emulator::flush_expired_sync`)。两个取最早的:只排
    // 其中一个,另一个就只能等下一个不相关事件顺带救回来,而「下一个事件」
    // 在用户打字的场景里就是**下一次按键**,画面于是永远慢一拍。
    match (pacer, ws.vt_sync_deadline()) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// 施加一次保存意图。抽成纯函数是为了能在没有窗口的情况下测「编辑已有会话
/// 不会把凭据清掉」(F73)——这条路径以前埋在事件循环里,只能靠上机手点。
///
/// 返回被写入的会话 id:新建时是 store 分配的那个,「保存并连接」要用。
fn apply_save(
    store: &mut crate::shell::store::SessionStore,
    save: crate::ui::session_manager::SaveIntent,
    now: &str,
) -> Result<mullion_store::SessionId, String> {
    use crate::ui::session_manager::{merge_secret, sync_has_passphrase};

    let crate::ui::session_manager::SaveIntent {
        editing_id,
        mut draft,
        password,
        passphrase,
        proxy_password,
        private_key,
        then_connect: _,
    } = save;

    // 先把已存凭据 clone 出来,释放对 store 的不可变借用,下面才能 &mut。
    let existing = editing_id.and_then(|id| store.secret(id)).cloned();
    let merged = merge_secret(
        existing.as_ref(),
        &password,
        &passphrase,
        &proxy_password,
        &private_key,
    );
    sync_has_passphrase(&mut draft, merged.as_ref());
    draft.secret = merged;

    match editing_id {
        Some(id) => {
            store
                .update(id, draft, now)
                .map_err(|e| format!("保存失败:{e}"))?;
            store.save().map_err(|e| format!("保存失败:{e}"))?;
            Ok(id)
        }
        None => {
            let id = store.add(draft, now);
            store.save().map_err(|e| format!("保存失败:{e}"))?;
            Ok(id)
        }
    }
}

/// F122:把弹窗里改的名字/颜色写到那个标签上。
///
/// 空名字(或只有空白)= 清除覆盖,退回连接时拼的 `title` —— 存一个空标签名
/// 会让标签栏上出现一块点得到但看不见的东西。
///
/// 自由函数而不是 `&mut self` 方法:调用点在渲染闭包之后,`self` 的其它字段
/// 此时另有借用(同 `apply_save` 的理由),而且这样能脱离 `App` 单测。
fn apply_tab_props<C>(
    tabs: &mut crate::shell::tabs::Tabs<C>,
    tab_id: crate::shell::tabs::TabId,
    name: String,
    color: Option<mullion_term::snapshot::Rgb>,
) {
    let trimmed = name.trim();
    if let Some(tab) = tabs.iter_mut().find(|t| t.id == tab_id) {
        tab.title_override = (!trimmed.is_empty()).then(|| trimmed.to_string());
        tab.color_override = color;
    }
}

/// 施加一次「保存凭据」意图(F74)。与 `apply_save` 同构、同样抽成自由函数,
/// 理由也一样:「编辑已有凭据点保存把密码清空」这类错误只能靠无窗口单测挡住。
///
/// 凭据**没有代理口令那一格**(设计 D4),合成时那一支恒 `Keep` ——
/// 传 `Clear` 会把会话侧不小心写进来的东西悄悄抹掉,`Keep` 才是「不归我管」。
///
/// 返回被写入的凭据 id:新建时是 store 分配的那个,调用方要拿它把编辑器
/// 切到「正在编辑这一条」,否则再点一次保存会又新建一条。
fn apply_credential_save(
    store: &mut crate::shell::store::SessionStore,
    intent: crate::ui::session_manager::CredentialSaveIntent,
) -> Result<mullion_store::CredentialId, String> {
    use crate::ui::session_manager::{merge_secret, SecretField};

    let crate::ui::session_manager::CredentialSaveIntent {
        editing_id,
        mut draft,
        password,
        passphrase,
        private_key,
    } = intent;

    // 先 clone 出来释放不可变借用,下面才能 &mut store。
    let existing = editing_id
        .and_then(|id| store.credential_secret(id))
        .cloned();
    let merged = merge_secret(
        existing.as_ref(),
        &password,
        &passphrase,
        &SecretField::Keep,
        &private_key,
    );
    // `has_passphrase` 必须跟**合成后**的密文走,不能跟表单走:编辑已有凭据
    // 时口令框恒为空,跟着表单会写成 false,下次连接 russh 拿到加密私钥却
    // 不知道要口令。会话侧是 `sync_has_passphrase`,凭据的 `kind` 在
    // `CredentialDraft` 顶层,直接改。
    if let mullion_store::AuthKind::PublicKey { has_passphrase } = &mut draft.kind {
        *has_passphrase = merged.as_ref().is_some_and(|s| s.passphrase.is_some());
    }
    draft.secret = merged;

    let id = match editing_id {
        Some(id) => {
            store
                .update_credential(id, draft)
                .map_err(|e| format!("保存凭据失败:{e}"))?;
            id
        }
        None => store.add_credential(draft),
    };
    store
        .save()
        .map_err(|e| format!("保存凭据失败:{e}"))
        .map(|_| id)
}

/// 施加一次 `~/.ssh/config` 导入(F2)。返回真正建出来的会话数。
///
/// **两阶段**(设计 D4):`ProxyJump` 在 config 里写的是主机别名,而本项目的
/// 跳板是 `JumpRef(SessionId)` —— id 要等落库才有。所以先按勾选把会话全建
/// 出来、记下 `别名 → id`,再回头把跳板翻译进去。
///
/// 指向本批之外的别名一律跳过(那条会话照常导入,只是跳板留空)——
/// **不凭空造一条跳板会话**,那是往库里塞用户没批准的配置。
///
/// 抽成自由函数是为了能无窗口单测:两阶段回填错了的现象是「导进来了但跳板
/// 是空的」,只有真跑一遍才看得出来。
fn apply_import(
    store: &mut crate::shell::store::SessionStore,
    rows: &[crate::ui::import_dialog::ImportRow],
    now: &str,
) -> Result<usize, String> {
    use std::collections::BTreeMap;

    let picked: Vec<&crate::ui::import_dialog::ImportRow> =
        rows.iter().filter(|r| r.selected).collect();
    let mut ids: BTreeMap<String, mullion_store::SessionId> = BTreeMap::new();
    for row in &picked {
        let id = store.add(crate::ui::import_dialog::draft_of(&row.entry), now);
        ids.insert(row.entry.alias.clone(), id);
    }
    for row in &picked {
        if row.entry.proxy_jump.is_empty() {
            continue;
        }
        let chain: Vec<mullion_store::JumpRef> = row
            .entry
            .proxy_jump
            .iter()
            .filter_map(|alias| ids.get(alias).copied().map(mullion_store::JumpRef))
            .collect();
        if chain.is_empty() {
            continue;
        }
        let id = ids[&row.entry.alias];
        // 读回刚建的那条改 network.jump 再写回去:`SessionStore` 只暴露
        // 「整条 draft 覆盖」这一种更新(与会话编辑器共用同一条路径),
        // 没有单字段 setter,也不该为导入新开一个。
        let mut draft = crate::ui::import_dialog::draft_of(&row.entry);
        draft.network.jump = Some(chain);
        store
            .update(id, draft, now)
            .map_err(|e| format!("回填跳板失败:{e}"))?;
    }
    store.save().map_err(|e| format!("导入失败:{e}"))?;
    Ok(picked.len())
}

/// 删凭据被拒时该说的那句话(F74/D7)。
///
/// store 只报得出 `SessionId`,而用户认的是会话名 —— 只说「还有 3 条会话
/// 引用着」等于让他挨个点开会话去找是哪三条,而「去解绑谁」正是他接下来
/// 唯一要做的事。非「被引用」的错误原样透传。
fn credential_delete_error(
    e: &mullion_store::StoreError,
    sessions: &[mullion_store::SessionRecord],
) -> String {
    match e {
        mullion_store::StoreError::CredentialInUse(ids) => {
            let names: Vec<String> = ids
                .iter()
                .map(|id| {
                    sessions
                        .iter()
                        .find(|s| s.id == *id)
                        .map_or_else(|| format!("{id:?}"), |s| s.identity.name.clone())
                })
                .collect();
            format!(
                "删不了:{} 条会话还在用这份凭据 —— {}",
                names.len(),
                names.join("、")
            )
        }
        other => format!("删除凭据失败:{other}"),
    }
}

/// 一帧渲染:先跑 egui(菜单栏 + 工具栏 + 状态栏 + 标题条,§4.2),再(终端态时)
/// 叠加背景色块 + 文字前景趟。返回 (egui 想要的下次重绘时间, 本帧的布局动作)——
/// 前者 `Duration::MAX` = 不需要,调用方据此走 T3/T7 的 `next_frame_at`/
/// `WaitUntil`,不会无条件 `request_redraw`;后者由调用方在借用释放后统一施加。
/// GPU 胶水,无单测。
/// `this_pass` 里是否有一个"真实动作"——即这一趟 egui 输出 `actions` 要不要
/// 拿去覆盖调用方手里那份(见 `render_frame` 内 `a.egui_ctx.run` 闭包上方的
/// 长注释:discard 趟收到的是空输入,产出的 `UiActions::default()` 绝不能
/// 覆盖掉真实点击那趟)。
///
/// `UiActions` 没有 derive `PartialEq`,这里是逐字段手写的判断——**给
/// `UiActions` 加新字段时必须在这里补上对应的 `.is_some()`(或等价判断),
/// 否则新动作会在 discard 趟里被静默丢弃**。这不是假设性风险:一次代码评审
/// 曾发现删掉 `files_remote` 这一条,662 个既有测试全绿——没有任何测试构造过
/// "`files_remote` 是唯一真实动作"的一帧。守护测试见
/// `files_remote_alone_counts_as_a_real_action_for_the_discard_guard`。
/// F21:字号(pt)按窗口 DPI 换成物理像素。
///
/// **一个函数、两个调用点**(`resumed` 建 `TextLayer` 和 `apply_font`):
/// 两处各写一遍 `pt * scale * 96.0 / 72.0` 的话,改了其中一个的现象是
/// 「启动时字号对,换个屏就不对了」——而两处相隔两千行。
/// F71:主密码改动的收尾 —— **无论成败**清空两个密码框,并给出该报的那句话。
///
/// 抽成自由函数是为了测得着:`App` 里握着窗口和 GPU,单测造不出来,而
/// 「失败时密码留在框里」这类错误只能靠测试挡住。
///
/// 清空是无条件的:失败时留着,用户下一次点「确定」会连着重试一遍他已经
/// 知道会失败的动作;成功时留着更糟 —— 一串明文主密码挂在屏幕上直到弹窗关闭。
///
/// 报告走 `set_error` 那条状态栏通道(成功也走):它是这个程序唯一的通知
/// 面,先例是 F59 的「跳过了 N 个目录」。
fn finish_password_change(
    draft: Option<&mut crate::ui::settings::SettingsDraft>,
    r: Result<(), mullion_store::StoreError>,
    ok_msg: &str,
) -> String {
    if let Some(d) = draft {
        d.new_password.clear();
        d.confirm_password.clear();
    }
    match r {
        Ok(()) => ok_msg.to_string(),
        Err(e) => format!("主密码没能改成:{e}"),
    }
}

fn font_px_for(pt: f32, scale: f32) -> f32 {
    pt * scale * 96.0 / 72.0
}

fn has_real_action(a: &crate::ui::UiActions) -> bool {
    a.preset.is_some()
        || a.close_pane.is_some()
        || a.tab.is_some()
        || a.annotate_export.is_some()
        || a.files_remote.is_some()
        || a.files_local.is_some()
        || !a.files_drop_in.is_empty()
        || a.files_drag_out
        || a.files_op.is_some()
        || a.transfer.is_some()
        || a.edit.is_some()
        || a.editor.is_some()
        || a.exit.is_some()
        || a.reconnect_tab.is_some()
        || a.reconnect_all
        || a.settings.is_some()
        || a.unlock.is_some()
        || a.tab_props.is_some()
        || a.rehost.is_some()
        || a.rehost_pane.is_some()
}

/// 参数多的理由同 `crate::ui::build_ui` —— 这个函数基本上就是它的调用壳。
#[allow(clippy::too_many_arguments)]
fn render_frame(
    a: &mut Active,
    panes: &[crate::gpu::PaneRender<'_>],
    ui_state: &mut crate::ui::UiState,
    frame: crate::ui::UiFrame<'_>,
    // F50:文件侧栏这一帧的两栏状态,`None` = 面板关着。不是 `UiFrame` 的字段——
    // 见 `crate::ui::build_ui` 的同名参数注释(`UiFrame` 必须保持 `Copy`)。
    mut files: Option<&mut crate::ui::files_panel::PanelFrame>,
    // D1:标签宿主(活动标签本身是 `Files`)那一帧的两栏状态。与上面的
    // `files`(侧栏宿主)互斥——调用方 `App::window_event` 的 `Present` 分支
    // 按活动标签是哪种只填其中一个。
    mut files_content: Option<&mut crate::ui::files_panel::PanelFrame>,
    // 代码复核挖出的真 bug 的修法:`files`/`files_content` 所属标签的世代号,
    // 原样转给 `build_ui`(见其同名参数的文档)。`files`/`files_content` 都是
    // `None` 时不会被用到——调用方传的是 `files_owner_generation.unwrap_or(0)`,
    // 那种情况下这个 0 只是个从不会被读取的占位值。
    files_generation: u64,
    // F55:传输队列,只读转给 `build_ui`(见其同名参数的文档)。
    queue: &crate::files::queue::Queue,
    // F53:在编辑的那些文件 + 内置编辑器窗口,转给 `build_ui`。
    edits: &crate::edit::sessions::EditSessions,
    editor: &mut Option<crate::ui::editor_window::EditorState>,
    // F125:这一帧光标该不该画出来(闪烁相位),调用方 `App::window_event` 算好了
    // 原样转给 `quads_for_panes`——算这个要读 `self.window_focused`/
    // `self.last_input_at`,那两个字段在这里(`Active` 而非 `App`)够不着。
    blink_on: bool,
) -> (std::time::Duration, crate::ui::UiActions) {
    diag::count_frame();
    // --- egui:每帧都跑,launcher 态(panes 为空)也要画菜单/状态栏。---
    diag::mark(diag::Stage::EguiRun);
    let raw_input = a.egui_state.take_egui_input(&a.window);
    let mut actions = crate::ui::UiActions::default();
    // egui::Context::run 内部是个 loop(egui 0.30 context.rs:802-841):首趟跑完
    // 若 `platform_output.requested_discard()`(例如某些部件首次展示时调用
    // `request_discard`,如 Grid 首帧)且未超 `max_passes`(默认 2),会整帧重画
    // 一次 —— 而重画前 `RawInput` 已被 `mem::take()`(egui 0.30
    // `data/input.rs:116`,`Context::run` 在两趟之间把上一趟的输入拿走清空),
    // 所以 discard 趟里 `ctx` 收到的是一份空事件的 `RawInput`,`build_ui` 在
    // 那一趟必然拿不到真实点击,只能产出 `UiActions::default()`。`UiFrame`
    // 因此按值收(`derive(Copy)`,见其定义处注释),但 `actions` **不能**每趟
    // 无条件整体覆盖:若 discard 趟排在真实点击那趟之后,无条件覆盖会用这份
    // default 悄悄吃掉第一趟已经拿到的真实点击(例如点了预设按钮的那一帧,
    // 因为某个部件首次展示触发了 discard,预设切换的 `actions.preset` 就会被
    // 静默清空)。改成"仅当本趟产出非默认值时才覆盖":真实点击总能被记住,
    // discard 趟的空结果不会覆盖掉它;若两趟都没有真实点击,`actions` 保持
    // 默认值,语义不变。
    let mut full_output = a.egui_ctx.run(raw_input, |ctx| {
        // `files` 是 `Option<&mut PanelFrame>`:`ctx.run` 的闭包是 `FnMut`、
        // 内部是个多趟 loop(见上面注释),不能把它按值 move 进来(第二趟就没了)。
        // `as_deref_mut()` 每趟从 `&mut Option<&mut PanelFrame>` 里取一个新的
        // reborrow ——`Option<&mut T>: DerefMut<Target = T>` 对任意 `T` 恒成立
        // (标准库给 `&mut T` 实现的 `DerefMut`),不需要 `PanelFrame` 自己是
        // `DerefMut`。
        let this_pass = crate::ui::build_ui(
            ctx,
            &MULLION_DARK,
            ui_state,
            frame,
            files.as_deref_mut(),
            files_content.as_deref_mut(),
            files_generation,
            queue,
            edits,
            editor,
        );
        if has_real_action(&this_pass) {
            actions = this_pass;
        }
    });
    // F100 标注模式的自动候选:本帧的 accesskit 树归约后给**下一帧**当候选
    // (`annotate::overlay` 跑在上面那个 `ctx.run` 里面,树要等 run 返回才有)。
    // 必须在 `handle_platform_output` **之前** take —— 那个函数按值吃掉整个
    // `PlatformOutput`。take 而非 clone:我们不调 `init_accesskit`,egui-winit
    // 那边的 adapter 恒为 None,拿到 None 什么都不做。
    crate::ui::annotate::ingest_accesskit(
        &a.egui_ctx,
        full_output.platform_output.accesskit_update.take(),
    );
    a.egui_state
        .handle_platform_output(&a.window, full_output.platform_output);
    let paint_jobs = a
        .egui_ctx
        .tessellate(full_output.shapes, full_output.pixels_per_point);
    let screen = egui_wgpu::ScreenDescriptor {
        size_in_pixels: [a.gpu.config.width, a.gpu.config.height],
        pixels_per_point: full_output.pixels_per_point,
    };
    // ROOT 是唯一 viewport(未用 egui 多窗口)。取它的 repaint_delay 交回调用方
    // 按 Throttle 语义排期(T3/T7)。
    let repaint_delay = full_output
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .map_or(std::time::Duration::MAX, |v| v.repaint_delay);

    // 每帧先 trim:清掉上一帧的 glyphs_in_use,让本帧 prepare 能按需淘汰旧字形。
    // 必须在 prepare/get_current_texture 的 early-return 之前——挪到函数末尾会导致
    // 一旦 AtlasFull 触发提前 return,trim 永远到不了,图集永远不被清理,
    // 下一帧 prepare 还是 AtlasFull,画面冻在最后一次成功帧且无法自愈。
    // trim 只清 in_use 标记不删纹理,首帧对空图集是 no-op,正常帧语义不变。
    a.text.trim();

    // --- 终端趟:仅 panes 非空(终端态)才生成 quads/prepare 文字;launcher 态
    // (panes 为空)没有终端可画,跳过,只画上面的 egui。每个 pane 自带 term_px
    // (来自调用方 `App::compute_geoms` 的 `layout_geometry`),色块层与文字层
    // 吃同一份 panes——这正是 gpu.rs:44 那条「文字层必须用同一个 origin」
    // 不变量要求的。
    let terminal_draw = if panes.is_empty() {
        None
    } else {
        diag::mark(diag::Stage::TextPrepare);
        let res = glyphon::Resolution {
            width: a.gpu.config.width,
            height: a.gpu.config.height,
        };
        let quads = quads_for_panes(
            panes,
            a.text.cell_w,
            a.text.cell_h,
            theme::term_default_colors(&MULLION_DARK),
            blink_on,
        );
        // 渲染路径不许 panic:prepare 失败(如长会话把图集喂满 AtlasFull)记录并
        // 跳过整帧(含 egui),与 Task 3 之前的行为一致——不拖垮整个 GUI。
        if let Err(e) = a
            .text
            .prepare_panes(&a.gpu.device, &a.gpu.queue, panes, res)
        {
            log::warn!(target: "mullion", "glyphon prepare 失败,跳过本帧: {e:?}");
            diag::count_skipped();
            return (std::time::Duration::MAX, actions);
        }
        let inst = a.gpu.quad_instances(&quads);
        Some((inst, quads.len() as u32))
    };

    diag::mark(diag::Stage::Acquire);
    let frame = match a.gpu.surface.get_current_texture() {
        Ok(f) => f,
        Err(wgpu::SurfaceError::Timeout) => {
            log::warn!(target: "mullion", "wgpu get_current_texture 超时,跳过本帧");
            diag::count_skipped();
            return (std::time::Duration::MAX, actions);
        }
        Err(e @ (wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated)) => {
            log::warn!(target: "mullion", "wgpu surface {e:?},重新 configure 后跳过本帧");
            a.gpu.surface.configure(&a.gpu.device, &a.gpu.config);
            diag::count_skipped();
            return (std::time::Duration::MAX, actions);
        }
        Err(wgpu::SurfaceError::OutOfMemory) => {
            log::error!(target: "mullion", "wgpu get_current_texture OutOfMemory,跳过本帧");
            diag::count_skipped();
            return (std::time::Duration::MAX, actions);
        }
    };
    diag::mark(diag::Stage::Encode);
    let view = frame
        .texture
        .create_view(&wgpu::TextureViewDescriptor::default());
    let mut enc = a
        .gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("frame"),
        });

    // egui 纹理上传/顶点缓冲更新须在 begin_render_pass 之前:update_buffers 要
    // `&mut enc` 记录拷贝命令,而 render pass 开始后 `enc` 会被 pass 借用/锁定。
    for (id, delta) in &full_output.textures_delta.set {
        a.egui_renderer
            .update_texture(&a.gpu.device, &a.gpu.queue, *id, delta);
    }
    let egui_cmds =
        a.egui_renderer
            .update_buffers(&a.gpu.device, &a.gpu.queue, &mut enc, &paint_jobs, &screen);

    {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("main"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(theme::clear_color(&MULLION_DARK)),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        if let Some((inst, n)) = &terminal_draw {
            a.gpu.draw_quads(&mut pass, inst, *n); // 背景趟
                                                   // 前景趟:失败(如条目在 prepare 之后被图集淘汰)不 panic,记录并跳过文字层,
                                                   // 背景色块这帧仍照常提交。
            if let Err(e) = a.text.render(&mut pass) {
                log::warn!(target: "mullion", "glyphon render 失败,跳过本帧文字层: {e:?}");
            }
        }
        // egui 需要 `&mut RenderPass<'static>`。单 pass 方案:终端趟先在 `pass` 上
        // 录完命令,再 `forget_lifetime` 转 'static 给 egui——forget_lifetime 消费
        // `pass` 自身,之后不能再用原 `pass`,故终端趟必须写在它之前(两趟画进
        // 同一个 render pass,而非两个 pass,借用检查器可行,顺利编译)。
        let mut static_pass = pass.forget_lifetime();
        a.egui_renderer
            .render(&mut static_pass, &paint_jobs, &screen);
    }

    a.gpu
        .queue
        .submit(egui_cmds.into_iter().chain(std::iter::once(enc.finish())));
    // present 在 Fifo 下会等 vsync;它和上面的 acquire 是最可能长阻塞的两步,
    // 分开打点才能区分「等交换链」和「等驱动」。
    diag::mark(diag::Stage::Present);
    frame.present();
    diag::count_present();
    for id in &full_output.textures_delta.free {
        a.egui_renderer.free_texture(id);
    }

    (repaint_delay, actions)
}

#[cfg(test)]
mod tests {
    use super::{
        apply_credential_save, apply_import, apply_layout_actions, apply_save, apply_tab_props,
        autoscroll_for_pane, credential_delete_error, download_job, effective_focus_of,
        expand_tilde, files_owner_generation_of, files_start_dir, finish_password_change,
        font_px_for, has_real_action, ime_cursor_area, next_panel_selection_index,
        pane_still_wanted, rehost_pane, snapshot_tabs_of, sync_target_of, sync_timeout_wake_at,
        tab_title, upload_job, wind_down, Modal, RestoredTab, Tab, TabContent, TerminalTab,
    };
    use crate::frame::FrameLimiter;
    use crate::reflow::{reflow, ResizeSink};
    use crate::shell::tabs::{TabId, Tabs};
    use crate::shell::workspace::{Preset, Workspace};
    use mullion_core::layout::{Dir, Node, PaneId, Rect};
    use mullion_store::SessionId;
    use std::sync::Arc;

    // ------------------------------------------------ F18 划选自动滚动

    /// F18:拖拽出界的自动滚动,判据必须是**焦点 pane 的终端区**,不是整个窗口。
    ///
    /// 用窗口边界的后果就是用户报的那条:「左键按住不动往上拉,选不到上一屏」。
    /// 终端区上沿之上还有菜单栏 + 标签栏(几十像素),指针拉到那里时窗口坐标
    /// 仍是正数、仍小于窗口高 —— `autoscroll_lines` 返回 0,一行都不滚。用户
    /// 得把鼠标拖出整个窗口才滚得动,而正常终端是拖到内容区顶端就开始滚。
    ///
    /// 自证会变红:把 `autoscroll_for_pane` 里的 `term.y`/`term.h` 换回窗口
    /// 原点(0)与窗口高 —— 头两条断言立刻红。
    #[test]
    fn autoscroll_triggers_at_the_terminal_edge_not_the_window_edge() {
        // 菜单栏 + 标签栏占掉顶上 100px,终端区 y=100..500,底下还有状态栏。
        let term = crate::shell::workspace::PxRect {
            x: 0,
            y: 100,
            w: 800,
            h: 400,
        };
        assert!(
            autoscroll_for_pane(60.0, term, 16.0) > 0,
            "指针在 y=60:还在窗口里,但已经拉出终端区上沿 —— 必须往历史滚"
        );
        assert!(
            autoscroll_for_pane(560.0, term, 16.0) < 0,
            "指针在 y=560:拉出终端区下沿 —— 必须往新内容滚"
        );
        assert_eq!(
            autoscroll_for_pane(300.0, term, 16.0),
            0,
            "指针在终端区内部不该滚"
        );
    }

    /// 分屏后每块 pane 的上下沿都不一样,滚动判据必须跟着**焦点那块**走。
    /// 上下分屏时,下面那块的上沿在窗口中部:指针停在窗口中上部对上面那块
    /// 是"区内",对下面那块却已经是"拉出上沿"。
    ///
    /// 自证会变红:让 `autoscroll_for_pane` 忽略 `term.y`。
    #[test]
    fn autoscroll_bounds_follow_the_focused_pane_not_the_whole_terminal_area() {
        let top = crate::shell::workspace::PxRect {
            x: 0,
            y: 100,
            w: 800,
            h: 200,
        };
        let bottom = crate::shell::workspace::PxRect {
            x: 0,
            y: 300,
            w: 800,
            h: 200,
        };
        assert_eq!(
            autoscroll_for_pane(250.0, top, 16.0),
            0,
            "y=250 落在上面那块内部,不该滚"
        );
        assert!(
            autoscroll_for_pane(250.0, bottom, 16.0) > 0,
            "同一个 y 对下面那块已经在上沿之外,焦点在它身上时必须滚"
        );
    }

    // ------------------------------------------------ 输入法候选框

    /// 候选框必须贴在**终端光标**那一格上。系统输入法拿这个矩形定位候选窗,
    /// 传窗口原点的话候选框永远飘在窗口左上角 —— 打中文时得低头找候选。
    ///
    /// 自证会变红:把 `ime_cursor_area` 里的 `term.x`/`term.y` 去掉。
    #[test]
    fn ime_candidate_box_sits_on_the_terminal_cursor_not_the_window_corner() {
        let term = crate::shell::workspace::PxRect {
            x: 40,
            y: 100,
            w: 800,
            h: 400,
        };
        // 光标在第 10 列第 5 行,格子 8×16。
        assert_eq!(
            ime_cursor_area(term, (10, 5), 8.0, 16.0),
            (40 + 80, 100 + 80, 8, 16)
        );
    }

    /// 候选框不能跑到 pane 外面去:光标行号在 resize 的中间态里可能短暂
    /// 大于终端区高度,不夹的话候选框会飘到下一个 pane / 状态栏上。
    ///
    /// 自证会变红:去掉 `ime_cursor_area` 里的两处夹紧。
    #[test]
    fn ime_candidate_box_is_clamped_inside_the_pane() {
        let term = crate::shell::workspace::PxRect {
            x: 0,
            y: 0,
            w: 80,
            h: 32,
        };
        let (x, y, _, _) = ime_cursor_area(term, (999, 999), 8.0, 16.0);
        assert!(x < 80, "x={x} 越出了 pane 右缘");
        assert!(y < 32, "y={y} 越出了 pane 下缘");
    }

    // ------------------------------------------------ F84/F21 外观设置

    /// F21:字号存的是 pt,画出来的是像素 —— 换一块 DPI 不同的屏,同一个 pt
    /// 必须换算出不同的像素,否则「跟随 DPI」这条验收点根本不成立。
    ///
    /// 自证会变红:把 `font_px_for` 改成直接返回 `pt`。
    #[test]
    fn the_same_point_size_is_more_pixels_on_a_higher_dpi_screen() {
        let at_100 = font_px_for(10.0, 1.0);
        let at_150 = font_px_for(10.0, 1.5);
        assert!(
            at_150 > at_100 * 1.4,
            "150% 缩放下 {at_150}px vs 100% 的 {at_100}px —— 没在跟 DPI 走"
        );
        // 96/72:pt 是 1/72 英寸,屏幕按 96dpi 算。10pt @100% = 13.33px。
        assert!((at_100 - 13.333_334).abs() < 0.01, "10pt@100% 应是 13.33px");
    }

    /// **T4 的字体版**:换字号改的是 `cell_w`/`cell_h`,而远端 tmux 是按
    /// `cols`/`rows` 排版的 —— 同一块中央区,字变大就必须让远端知道列数变少了,
    /// 否则全屏 TUI 按旧列数排版,当场错行。
    ///
    /// 扎的是 `layout_geometry` 这条**唯一**的换算(`compute_geoms` 现读
    /// `a.text.cell_w/cell_h` 喂给它),纯函数、不需要 GPU。
    ///
    /// 自证会变红:把 `layout_geometry` 里 `grid_size_for` 的除法换成常量。
    #[test]
    fn changing_the_cell_size_changes_what_the_remote_is_told() {
        use crate::shell::workspace::{layout_geometry, PxRect};
        let tree = Node::Leaf(PaneId(0));
        let area = PxRect {
            x: 0,
            y: 0,
            w: 1600,
            h: 900,
        };
        let small = layout_geometry(&tree, area, (10.0, 20.0), false, 1.0)[0].grid;
        let big = layout_geometry(&tree, area, (20.0, 40.0), false, 1.0)[0].grid;
        assert!(
            big.0 < small.0 && big.1 < small.1,
            "字元从 10x20 放大到 20x40,远端却被告知 {big:?}(原 {small:?})——\
             列/行数没跟着变,远端 TUI 会按旧列数排版"
        );
    }

    /// **不许有第二条尺寸传播路径**(T4 的复发方式)。
    ///
    /// `apply_font` 只换字体 + 标脏,`cols`/`rows` 由既有的
    /// `compute_geoms` → `apply_geometry` 那条链路下一帧现算。它自己算一遍的
    /// 话,两处会在「标题条开关」「文件侧栏挤窄」这些场景下漂开,而症状
    /// (远端偶尔错行)极难对上原因。
    ///
    /// 扎源码结构的理由同 `file_actions_never_narrow_to_terminal_tabs_only`
    /// (`App` 单测里造不出 `Active`:要 wgpu 设备和真窗口)。
    ///
    /// 自证会变红:在 `apply_font` 里加一句 `layout_geometry(...)`,
    /// 或者把 `self.ui_dirty = true;` 删掉。
    #[test]
    fn a_font_change_goes_through_the_same_geometry_path_as_a_resize() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn apply_font(&mut self)")
            .nth(1)
            .expect("找不到 apply_font 的定义");
        let body = &after[..after
            .find("\n    }\n")
            .expect("找不到 apply_font 的函数结尾")];
        assert!(
            body.contains("set_font("),
            "apply_font 的函数体切歪了 —— 下面几条断言会空过"
        );
        assert!(
            body.contains("self.ui_dirty = true;"),
            "换了字体不标脏:远端一安静就没有下一帧,新字号要等用户碰一下鼠标\
             才生效(而 `apply_geometry` 只在 present 那一帧跑)"
        );
        for forbidden in ["layout_geometry", "compute_geoms", "apply_geometry"] {
            assert!(
                !body.contains(forbidden),
                "apply_font 里出现了 {forbidden} —— 尺寸传播必须只有 \
                 compute_geoms 那一条路径(T4)"
            );
        }
    }

    /// F21:`ScaleFactorChanged` 必须真的重建字体。
    ///
    /// 这条事件在无头环境里发不出来(要真把窗口拖到另一块 DPI 不同的屏上),
    /// 所以扎源码:那个分支里必须调 `apply_font`。改这条之前它只记一行日志,
    /// 现象是跨屏之后字号纹丝不动。
    ///
    /// 自证会变红:把该分支里的 `self.apply_font();` 删掉。
    #[test]
    fn the_scale_factor_change_actually_rebuilds_the_font() {
        let src = include_str!("app.rs");
        let after = src
            .split("WindowEvent::ScaleFactorChanged")
            .nth(1)
            .expect("找不到 ScaleFactorChanged 分支");
        // 分支体止于下一个 `WindowEvent::` —— 匹配臂是并列的。
        let body = &after[..after
            .find("WindowEvent::")
            .expect("找不到 ScaleFactorChanged 的下一个匹配臂")];
        assert!(
            body.contains("scale_factor"),
            "分支体切歪了 —— 下面那条断言会空过"
        );
        assert!(
            body.contains("self.apply_font()"),
            "DPI 变了却不重建字体:把窗口从 150% 的屏拖到 100% 的屏,字会\
             一直是原来那么大(物理像素没换算)"
        );
    }

    /// D4b 那条老坑的第 N 次复现防线:设置动作**单独**也要算「真动作」,
    /// 否则它会在 egui 的 discard 趟被静默吃掉 —— 现象是拖字号滑块半天没反应。
    ///
    /// 自证会变红:把 `has_real_action` 里的 `a.settings.is_some()` 去掉。
    #[test]
    fn settings_alone_counts_as_a_real_action_for_the_discard_guard() {
        let mut a = crate::ui::UiActions::default();
        assert!(!has_real_action(&a), "空动作不该算数");
        a.settings = Some(crate::ui::settings::SettingsOut::Preview);
        assert!(
            has_real_action(&a),
            "只有设置动作时被判成「什么都没发生」—— 这一帧的结论会被 discard \
             趟的默认值覆盖掉"
        );
    }

    /// 同上,换节点那两份。被 discard 趟吃掉的现象分别是「点 `⇆` 不弹窗」和
    /// 「在弹窗里选了节点毫无反应」——后者尤其难查:弹窗自己关掉了,看起来
    /// 像是生效了。
    ///
    /// 自证会变红:把 `has_real_action` 里 `a.rehost.is_some()` /
    /// `a.rehost_pane.is_some()` 任意一条去掉 —— 写这条测试时后者本来就漏了,
    /// 它当场就红了。
    #[test]
    fn rehost_actions_count_as_real_actions_for_the_discard_guard() {
        let mut a = crate::ui::UiActions::default();
        assert!(!has_real_action(&a), "空动作不该算数");
        a.rehost = Some(crate::ui::rehost::RehostAction::Pick {
            pane: PaneId(2),
            session: mullion_store::SessionId(1),
        });
        assert!(has_real_action(&a), "选中的节点被 discard 趟吞了");

        let b = crate::ui::UiActions {
            rehost_pane: Some(PaneId(2)),
            ..Default::default()
        };
        assert!(has_real_action(&b), "点 `⇆` 的那一下被 discard 趟吞了");
    }

    /// 同上,解锁框那一份。它是整个程序此刻唯一能操作的东西 —— 被 discard
    /// 趟吃掉的现象是「按解锁毫无反应」,而用户没有第二条出路。
    ///
    /// 自证会变红:把 `has_real_action` 里的 `a.unlock.is_some()` 去掉。
    /// F71:主密码改动收尾**无论成败**都清空两个密码框。
    ///
    /// 失败时留着,用户下一次点「确定」会连着重试一遍他已经知道会失败的
    /// 动作;成功时留着更糟 —— 一串明文主密码挂在屏幕上直到弹窗关闭。
    #[test]
    fn a_password_change_always_clears_the_two_boxes() {
        for r in [Ok(()), Err(mullion_store::StoreError::WrongPassword)] {
            let mut d = crate::ui::settings::SettingsDraft {
                family: None,
                font_pt: 10.0,
                typed: String::new(),
                new_password: "hunter2".into(),
                confirm_password: "hunter2".into(),
                tmux_bootstrap: true,
            };
            let _ = finish_password_change(Some(&mut d), r, "已生效");
            assert!(
                d.new_password.is_empty() && d.confirm_password.is_empty(),
                "密码留在框里了"
            );
        }
    }

    /// 失败要说**为什么**失败,不能笼统一句「没能改成」——用户下一步是
    /// 「换个密码重试」还是「先修钥匙串」,全看这句话。
    #[test]
    fn a_failed_password_change_names_the_reason() {
        let msg = finish_password_change(
            None,
            Err(mullion_store::StoreError::Keyring("钥匙串没开".into())),
            "已生效",
        );
        assert!(msg.contains("钥匙串没开"), "把失败原因吞了:{msg}");
        let ok = finish_password_change(None, Ok(()), "主密码已生效");
        assert_eq!(ok, "主密码已生效");
    }

    #[test]
    fn unlock_alone_counts_as_a_real_action_for_the_discard_guard() {
        let mut a = crate::ui::UiActions::default();
        assert!(!has_real_action(&a), "空动作不该算数");
        a.unlock = Some(crate::ui::unlock::UnlockOut::Submit);
        assert!(has_real_action(&a), "解锁动作被 egui 的丢弃趟吞了");
    }

    /// T8:解锁框开着时键盘必须归 egui。它是个密码输入框 —— 不算模态的话,
    /// 主密码会一边被收进输入框、一边被原样发进远端 shell(还会落进 shell
    /// 历史)。
    ///
    /// 扎源码而不是造 `App`:`modal_open` 要一个真的 `App`(带窗口/GPU),
    /// 无头环境造不出来;而这条要防的恰恰是「新增弹窗时漏进这张表」,
    /// 判据本来就是「那张表里有没有这一项」。
    ///
    /// 自证会变红:把 `modal_open` 里的 `self.ui.unlock.is_some()` 删掉。
    #[test]
    fn the_unlock_dialog_counts_as_a_modal_so_the_password_never_reaches_the_shell() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn modal_open(&self) -> bool {")
            .nth(1)
            .expect("找不到 modal_open 的定义");
        let body = &after[..after
            .find("\n    }\n")
            .expect("找不到 modal_open 的函数结尾")];
        assert!(
            body.contains("self.ui.session_manager_open"),
            "modal_open 的函数体切歪了 —— 下面那条断言会空过"
        );
        assert!(
            body.contains("self.ui.unlock.is_some()"),
            "解锁框没算进模态:主密码会一边进输入框、一边被发给远端 shell(T8)"
        );
    }

    /// T8:F2 的导入预览弹窗也必须计进模态。判据与理由同上一条 —— 这张表
    /// 每加一个弹窗就要补一项,而「加了弹窗忘了补」正是它要防的。
    ///
    /// 自证会变红:把 `modal_open` 里的 `self.ui.import.is_some()` 删掉。
    #[test]
    fn the_import_preview_counts_as_a_modal_so_keys_do_not_leak_to_the_shell() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn modal_open(&self) -> bool {")
            .nth(1)
            .expect("找不到 modal_open 的定义");
        let body = &after[..after
            .find("\n    }\n")
            .expect("找不到 modal_open 的函数结尾")];
        assert!(
            body.contains("self.ui.session_manager_open"),
            "modal_open 的函数体切歪了 —— 下面那条断言会空过"
        );
        assert!(
            body.contains("self.ui.import.is_some()"),
            "导入预览没算进模态:弹窗上敲的键会漏给远端 shell(T8)"
        );
    }

    /// T8:四个曾经漏登记的弹窗(内置编辑器 / 文件写操作确认框 / 分组管理器 /
    /// 标签属性弹窗)也必须计进模态。判据与理由同上两条 ——
    /// `every_modal_variant_is_listed_in_all` 只防「变体没塞进 `Modal::ALL`」,
    /// 防不住「变体在 `ALL` 里,但 `modal_open` 的 `match` 分支被悄悄改成
    /// `=> false`」这种接线错误(复核实测:把这四臂都改成 `=> false`,
    /// `cargo test --lib` 之前是全绿的 —— `TabProps` 是切片 J 终审补的,
    /// 之前完全没有守护)。
    ///
    /// 自证会变红:把 `modal_open` 里 `self.editor.is_some()` /
    /// `self.ui.files_dialog.is_some()` / `self.ui.group_manager_open` /
    /// `self.ui.tab_props.is_some()` 任意一处改成字面量 `false`。
    #[test]
    fn the_editor_files_dialog_group_manager_and_tab_props_windows_count_as_modals_so_keys_do_not_leak_to_the_shell(
    ) {
        let src = include_str!("app.rs");
        let after = src
            .split("fn modal_open(&self) -> bool {")
            .nth(1)
            .expect("找不到 modal_open 的定义");
        let body = &after[..after
            .find("\n    }\n")
            .expect("找不到 modal_open 的函数结尾")];
        assert!(
            body.contains("self.ui.session_manager_open"),
            "modal_open 的函数体切歪了 —— 下面那条断言会空过"
        );
        assert!(
            body.contains("self.editor.is_some()"),
            "内置编辑器没算进模态:编辑器开着时键盘仍判给终端,里面根本打不出字(T8)"
        );
        assert!(
            body.contains("self.ui.files_dialog.is_some()"),
            "文件写操作确认框没算进模态:新建文件夹时敲的目录名会同时发给远端 shell(T8)"
        );
        assert!(
            body.contains("self.ui.group_manager_open"),
            "分组管理器没算进模态:分组名输入框里敲的字会同时发给远端 shell(T8)"
        );
        assert!(
            body.contains("self.ui.tab_props.is_some()"),
            "标签属性弹窗没算进模态:改名输入框里敲的字会同时发给远端 shell(T8)"
        );
    }

    /// T8:退出确认框(「还有改动没传回远端」)也必须计进模态。**从 D3 引入
    /// 起就没进过这张表**,是切片 J 终审才发现的漏网之鱼 —— 与前几条不同,
    /// 它没有输入框,后果不是「键盘被同时发给远端」,而是「`Ctrl+W`/
    /// `Ctrl+Shift+B` 两条快捷键的闸门都是 `modal_open()`,不算模态的话它俩
    /// 照样在退出确认框开着期间生效」。
    ///
    /// 自证会变红:把 `modal_open` 里 `self.ui.exit_pending` 改成字面量
    /// `false`。
    #[test]
    fn the_exit_confirm_dialog_counts_as_a_modal_so_ctrl_w_and_ctrl_shift_b_do_not_leak_through() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn modal_open(&self) -> bool {")
            .nth(1)
            .expect("找不到 modal_open 的定义");
        let body = &after[..after
            .find("\n    }\n")
            .expect("找不到 modal_open 的函数结尾")];
        assert!(
            body.contains("self.ui.session_manager_open"),
            "modal_open 的函数体切歪了 —— 下面那条断言会空过"
        );
        assert!(
            body.contains("self.ui.exit_pending"),
            "退出确认框没算进模态:Ctrl+W/Ctrl+Shift+B 会在它开着期间照旧生效(T8)"
        );
    }

    /// T8:模态表的**完备性**守护。`Modal::ALL` 少写一个变体编译器不管 ——
    /// 这条测试补上那个缺口。
    ///
    /// 用穷尽 `match`(`check` 内部)而不是数变体总数 —— 旧版靠人工维护
    /// `VARIANT_COUNT` 防不住「加了变体、也补了 `modal_open` 的 `match`
    /// 分支,却忘了塞进 `ALL`」:那种情况下 `ALL.len()` 没变,`VARIANT_COUNT`
    /// 也没人去改,测试照绿。改成穷尽 `match` 之后,新增一个 `Modal` 变体
    /// 如果不给 `check` 里的 `match` 补一条分支,**这个函数本身就编译
    /// 不过**(`non-exhaustive patterns`)——这个检查在编译期生效,与
    /// 「有没有实际拿这个变体去调用 `check`」无关。
    ///
    /// 与上面几条不同,这条不扎源码:`Modal` 是纯枚举,不需要真 `App` 就能数。
    ///
    /// 自证会变红/编译不过:
    /// - 从 `Modal::ALL` 里删掉任意一个变体 → 对应那一臂的 `assert!` 变红。
    /// - 给 `Modal` 加一个新变体、只在 `modal_open` 里补分支、不回来改这条
    ///   测试 → `check` 里的 `match` 非穷尽,编译不过。
    #[test]
    fn every_modal_variant_is_listed_in_all() {
        fn check(m: Modal) {
            match m {
                Modal::SessionManager => assert!(
                    Modal::ALL.contains(&Modal::SessionManager),
                    "SessionManager 没登记进 Modal::ALL(T8)"
                ),
                Modal::About => assert!(
                    Modal::ALL.contains(&Modal::About),
                    "About 没登记进 Modal::ALL(T8)"
                ),
                Modal::Settings => assert!(
                    Modal::ALL.contains(&Modal::Settings),
                    "Settings 没登记进 Modal::ALL(T8)"
                ),
                Modal::Unlock => assert!(
                    Modal::ALL.contains(&Modal::Unlock),
                    "Unlock 没登记进 Modal::ALL(T8)"
                ),
                Modal::HostKey => assert!(
                    Modal::ALL.contains(&Modal::HostKey),
                    "HostKey 没登记进 Modal::ALL(T8)"
                ),
                Modal::Paste => assert!(
                    Modal::ALL.contains(&Modal::Paste),
                    "Paste 没登记进 Modal::ALL(T8)"
                ),
                Modal::Import => assert!(
                    Modal::ALL.contains(&Modal::Import),
                    "Import 没登记进 Modal::ALL(T8)"
                ),
                Modal::Editor => assert!(
                    Modal::ALL.contains(&Modal::Editor),
                    "Editor 没登记进 Modal::ALL(T8)"
                ),
                Modal::FilesDialog => assert!(
                    Modal::ALL.contains(&Modal::FilesDialog),
                    "FilesDialog 没登记进 Modal::ALL(T8)"
                ),
                Modal::GroupManager => assert!(
                    Modal::ALL.contains(&Modal::GroupManager),
                    "GroupManager 没登记进 Modal::ALL(T8)"
                ),
                Modal::TabProps => assert!(
                    Modal::ALL.contains(&Modal::TabProps),
                    "TabProps 没登记进 Modal::ALL(T8)"
                ),
                Modal::ExitConfirm => assert!(
                    Modal::ALL.contains(&Modal::ExitConfirm),
                    "ExitConfirm 没登记进 Modal::ALL(T8)"
                ),
                Modal::Rehost => assert!(
                    Modal::ALL.contains(&Modal::Rehost),
                    "Rehost 没登记进 Modal::ALL(T8)"
                ),
            }
        }
        for m in [
            Modal::SessionManager,
            Modal::About,
            Modal::Settings,
            Modal::Unlock,
            Modal::HostKey,
            Modal::Paste,
            Modal::Import,
            Modal::Editor,
            Modal::FilesDialog,
            Modal::GroupManager,
            Modal::TabProps,
            Modal::ExitConfirm,
        ] {
            check(m);
        }
        // 去重后仍是同一个数 —— 防「复制粘贴写重了一项来凑数」。
        let mut seen = std::collections::HashSet::new();
        for m in Modal::ALL {
            assert!(seen.insert(format!("{m:?}")), "Modal::ALL 里有重复项:{m:?}");
        }
    }

    /// F61/F62:导入会一次加进几十条会话,外观缓存必须跟着重算 —— 漏掉
    /// 它的话新会话在列表里画的是默认色/默认图标,而用户完全不知道为什么。
    ///
    /// 与 `modal_open` 那两条同样扎源码:`touched_store` 要一个真的 `App`
    /// 才求得出值,而这条要防的正是「新增一条改 store 的通道时忘了计入」。
    ///
    /// 自证会变红:把 `touched_store` 里的 `import_request` 那一项删掉。
    #[test]
    fn importing_sessions_counts_as_touching_the_store_so_the_look_is_recomputed() {
        let src = include_str!("app.rs");
        let after = src
            .split("let touched_store = ")
            .nth(1)
            .expect("找不到 touched_store 的赋值");
        let expr = &after[..after.find(";\n").expect("找不到该赋值的结尾")];
        assert!(
            expr.contains("self.ui.save_request"),
            "touched_store 的表达式切歪了 —— 下面那条断言会空过"
        );
        assert!(
            expr.contains("self.ui.import_request"),
            "导入没算进 touched_store:新导入的会话会画成默认外观(F61/F62)"
        );
    }

    /// F122:弹窗保存不再是「写 store 的意图」——`touched_store` 里不许再有它,
    /// 否则每改一次标签名都白跑一次外观全表重算。
    ///
    /// 自证会变红:把 `self.ui.tab_props_save.is_some()` 加回 `touched_store`。
    #[test]
    fn tab_props_is_no_longer_a_store_write_intent() {
        let src = include_str!("app.rs");
        let after = src
            .split("let touched_store = ")
            .nth(1)
            .expect("找不到 touched_store 的赋值");
        let expr = &after[..after.find(";\n").expect("找不到该赋值的结尾")];
        assert!(
            expr.contains("self.ui.save_request"),
            "切歪了 —— 下面那条会空过"
        );
        assert!(
            !expr.contains("tab_props"),
            "标签属性已改成运行期覆盖(F122),不该再算进 touched_store"
        );
    }

    /// F121:拖拽排序改的是会话的 `group_id` 与顺序,跨组会换继承来源,
    /// 外观(图标/颜色)可能跟着变 —— 漏算的话拖拽跨组之后那一行的图标/
    /// 颜色不会跟着重算,用户看到的还是旧外观。
    ///
    /// 自证会变红:把 `touched_store` 里的 `self.ui.reorder_request` 那一项删掉。
    #[test]
    fn dragging_a_session_counts_as_touching_the_store_so_the_look_is_recomputed() {
        let src = include_str!("app.rs");
        let after = src
            .split("let touched_store = ")
            .nth(1)
            .expect("找不到 touched_store 的赋值");
        let expr = &after[..after.find(";\n").expect("找不到该赋值的结尾")];
        assert!(
            expr.contains("self.ui.save_request"),
            "touched_store 的表达式切歪了 —— 下面那条断言会空过"
        );
        assert!(
            expr.contains("self.ui.reorder_request"),
            "拖拽排序没算进 touched_store:跨组拖拽后外观缓存不会重算(F121)"
        );
    }

    /// F121:拖拽排序落盘走的也是 keyring/TOML 同步 IO,跟删除/保存/移动分组
    /// 同一条门槛——漏算的话看门狗测不出这条路径可能阻塞事件循环。
    ///
    /// 自证会变红:把 `diag::mark(diag::Stage::StoreIo)` 前面那个 `if` 里的
    /// `self.ui.reorder_request.is_some()` 删掉。
    #[test]
    fn dragging_a_session_is_marked_as_store_io_for_the_watchdog() {
        let src = include_str!("app.rs");
        let idx = src
            .find("diag::mark(diag::Stage::StoreIo);")
            .expect("找不到 StoreIo 打点");
        let before = &src[..idx];
        let start = before
            .rfind("if self.ui.delete_request.is_some()")
            .expect("找不到打点前面那个 if 的开头");
        let cond = &src[start..idx];
        assert!(
            cond.contains("self.ui.save_request.is_some()"),
            "切歪了 —— 下面那条断言会空过"
        );
        assert!(
            cond.contains("self.ui.reorder_request.is_some()"),
            "拖拽排序没算进 StoreIo 打点条件(F121)"
        );
    }

    /// F71:探测失败(`secrets.enc` 的文件头读不懂)时**不许**当成「不需要
    /// 密码」往下走 —— 那会拿钥匙串密钥去解一个主密码文件,报出来的是
    /// 「密文损坏」,把真正的原因盖掉。
    ///
    /// 自证会变红:把 `Err(e)` 那一支改成走 `SessionStore::open`。
    #[test]
    fn a_failed_probe_does_not_fall_back_to_opening_with_the_keyring_key() {
        let src = include_str!("app.rs");
        let after = src
            .split("match crate::shell::store::probe_needs_password(&d)")
            .nth(1)
            .expect("找不到探测那一段");
        let arm = &after[after.find("Err(e) =>").expect("找不到探测失败那一支")..];
        let arm = &arm[..arm.find("\n                }").expect("找不到该支的结尾")];
        assert!(
            !arm.contains("SessionStore::open"),
            "探测失败却还是拿钥匙串密钥开库了 —— 真正的原因会被「密文损坏」盖掉"
        );
        assert!(
            arm.contains("set_error"),
            "探测失败必须说出来,静默禁用会话功能等于让人以为会话全没了"
        );
    }

    #[test]
    fn redraw_is_frame_capped() {
        // T3/N3:16ms 窗口内不超发一帧,避免 GPU 空转。
        let mut limiter = FrameLimiter::new(16);
        assert!(limiter.should_present(0), "首帧应允许");
        limiter.record_present(0);
        assert!(!limiter.should_present(8), "同一 16ms 窗口内不应再发");
        assert!(limiter.should_present(16), "满 16ms 后允许下一帧");
        limiter.record_present(16);
        assert!(!limiter.should_present(20));
    }

    #[test]
    fn reflow_emits_resize() {
        // T4/F34:布局变更后每个 pane 收到与新矩形一致的列/行数。
        struct FakeSink {
            calls: Vec<(PaneId, u16, u16)>,
        }
        impl ResizeSink for FakeSink {
            fn resize(&mut self, pane: PaneId, cols: u16, rows: u16) {
                self.calls.push((pane, cols, rows));
            }
        }

        let tree = Node::Split {
            dir: Dir::Horizontal,
            ratio: 0.5,
            a: Box::new(Node::Leaf(PaneId(1))),
            b: Box::new(Node::Leaf(PaneId(2))),
        };
        let area = Rect {
            col: 0,
            row: 0,
            cols: 80,
            rows: 24,
        };
        let mut sink = FakeSink { calls: Vec::new() };
        reflow(&tree, area, &mut sink);

        assert_eq!(
            sink.calls,
            vec![(PaneId(1), 40, 24), (PaneId(2), 40, 24)],
            "resize 列数必须与新矩形一致(F34)"
        );
    }

    /// F34/T4:窗口 resize 的几何必须经 `layout_geometry` 算,再由
    /// `Workspace::apply_geometry` 施加。这里锁住"整窗尺寸 → 每 pane 网格"
    /// 这一段换算 —— 接线写错的典型症状是分屏后远端按整窗列数排版。
    #[test]
    fn window_resize_maps_to_per_pane_grids_f34() {
        use crate::shell::workspace::{layout_geometry, PxRect};
        use mullion_core::layout::{Dir, Node, PaneId};

        let tree = Node::Split {
            dir: Dir::Horizontal,
            ratio: 0.5,
            a: Box::new(Node::Leaf(PaneId(1))),
            b: Box::new(Node::Leaf(PaneId(2))),
        };
        let area = PxRect {
            x: 0,
            y: 100,
            w: 1600,
            h: 900,
        };
        let geoms = layout_geometry(&tree, area, (10.0, 20.0), true, 1.0);
        assert_eq!(geoms.len(), 2);
        for g in &geoms {
            assert!(
                g.grid.0 < 160,
                "每 pane 的列数必须小于整窗列数,否则是没分屏就发了 window_change"
            );
            assert!(g.grid.0 >= 1 && g.grid.1 >= 1);
        }
    }

    /// 状态栏的屏数取自布局树,不是硬编码。B1 遗留技术债 1 的兜底。
    #[test]
    fn status_bar_pane_count_comes_from_the_tree() {
        use mullion_core::layout::{leaves, Dir, Node, PaneId};
        let tree = Node::Split {
            dir: Dir::Vertical,
            ratio: 0.5,
            a: Box::new(Node::Leaf(PaneId(1))),
            b: Box::new(Node::Leaf(PaneId(7))),
        };
        assert_eq!(leaves(&tree).len(), 2);
        let (left, _) = crate::ui::chrome::status_text(leaves(&tree).len(), true);
        assert_eq!(left, "2 屏 · 已连接");
    }

    /// `apply_layout_actions` 之下没有 runtime/proxy,可以拿真实构造的
    /// `Workspace` 直接单测——这几个测试专门堵 handoff 4.6 点名的两个坑:
    /// "点了预设 X,是不是真的切到了 X"、"点了关闭 pane Y,是不是真的关掉了 Y"。
    struct NullPty;
    impl crate::shell::workspace::PtyWriter for NullPty {
        fn write(&self, _bytes: Vec<u8>) -> Result<(), mullion_ssh::session::TrySendErr> {
            Ok(())
        }
        fn resize(&self, _cols: u16, _rows: u16) -> Result<(), mullion_ssh::session::TrySendErr> {
            Ok(())
        }
    }

    fn test_pane(id: u32) -> crate::shell::workspace::PaneState {
        let (_tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
        crate::shell::workspace::PaneState {
            id: PaneId(id),
            host_ix: 0,
            emulator: mullion_term::emulator::Emulator::new(80, 24),
            pty: Box::new(NullPty),
            rx,
            pacer: crate::render::SyncFramePacer::new(),
            status: crate::shell::workspace::PaneStatus::Live,
            saw_first_byte: false,
            last_grid: (80, 24),
            cwd: None,
            tmux: None,
        }
    }

    /// 点工具栏上的预设按钮 X,树必须真的变成 X 对应的形状,不是停在原地、
    /// 也不是无条件切到某个写死的预设。两次切不同的预设,确认路由跟着点击
    /// 的值走(如果实现里硬编码了一个预设,第二个断言必挂)。
    #[test]
    fn preset_click_switches_to_the_specific_preset_clicked() {
        let mut ws = Workspace::new(test_pane(1), 0);
        let (fresh, preset_out) = apply_layout_actions(
            &mut ws,
            &crate::ui::UiActions {
                preset: Some(Preset::ThreeColumns),
                close_pane: None,
                ..Default::default()
            },
        )
        .expect("点了预设,动作不该是 None");
        assert_eq!(
            preset_out,
            Some(Preset::ThreeColumns),
            "必须切到点击的那个预设,不是别的"
        );
        assert_eq!(
            fresh.len(),
            2,
            "ThreeColumns 比原来的 1 屏多 2 个新叶子等着上线"
        );
        assert_eq!(
            mullion_core::layout::leaves(ws.tree()).len(),
            3,
            "树上必须真的变成 3 个叶子,不是停在原来的 Single"
        );
        for id in &fresh {
            ws.attach_pane(test_pane(id.0));
        }

        // 再点一个不同的预设,确认不是写死指向 ThreeColumns。
        let (_, preset_out2) = apply_layout_actions(
            &mut ws,
            &crate::ui::UiActions {
                preset: Some(Preset::TwoTopBottom),
                close_pane: None,
                ..Default::default()
            },
        )
        .expect("点了预设,动作不该是 None");
        assert_eq!(preset_out2, Some(Preset::TwoTopBottom));
        assert_eq!(mullion_core::layout::leaves(ws.tree()).len(), 2);
    }

    /// 点某个 pane 标题条上的 ×,必须真的只关掉**那一个** pane——不是关掉焦点
    /// pane、也不是关掉第一个 pane。特意选一个不等于当前焦点的目标,堵死
    /// "实现里其实关的是 ws.focus() 而不是传入的 id"这类巧合过关的 bug。
    #[test]
    fn close_pane_click_closes_the_specific_pane_clicked() {
        let mut ws = Workspace::new(test_pane(1), 0);
        let (fresh, _) = apply_layout_actions(
            &mut ws,
            &crate::ui::UiActions {
                preset: Some(Preset::ThreeColumns),
                close_pane: None,
                ..Default::default()
            },
        )
        .unwrap();
        for id in &fresh {
            ws.attach_pane(test_pane(id.0));
        }
        let all_ids = mullion_core::layout::leaves(ws.tree());
        assert_eq!(all_ids.len(), 3);
        let focus = ws.focus();
        let target = *all_ids
            .iter()
            .find(|&&id| id != focus)
            .expect("三个 pane 里总有一个不是焦点");
        let others: Vec<PaneId> = all_ids.iter().copied().filter(|&id| id != target).collect();

        let (fresh2, preset_out2) = apply_layout_actions(
            &mut ws,
            &crate::ui::UiActions {
                preset: None,
                close_pane: Some(target),
                ..Default::default()
            },
        )
        .expect("点了关闭,动作不该是 None");
        assert!(fresh2.is_empty(), "关 pane 不该产生待开的新 channel");
        assert_eq!(preset_out2, None, "手动关闭后不再对应任何预设");
        assert!(
            ws.pane(target).is_none(),
            "点击关闭的那个 pane 必须真的没了"
        );
        for id in others {
            assert!(ws.pane(id).is_some(), "没点的 pane 不该被殃及(id={id:?})");
        }
    }

    /// 没有任何动作的一帧:不该无中生有地改 current_preset / 触发重绘。
    #[test]
    fn no_ui_action_means_no_layout_change() {
        let mut ws = Workspace::new(test_pane(1), 0);
        let result = apply_layout_actions(&mut ws, &crate::ui::UiActions::default());
        assert!(result.is_none(), "什么都没点,不该有布局动作");
        assert_eq!(mullion_core::layout::leaves(ws.tree()).len(), 1);
    }

    /// 复核 Important #1:开 channel 是真实网络往返,`PaneOpened` 可能在用户
    /// 又切了一次预设之后才回来——此时 `id` 已经不在树上了。`pane_still_wanted`
    /// 必须能识破这种"晚到的、树上已经没有对应叶子"的 id,不然
    /// `attach_pane` 会攒出一个渲染/标题条都看不见、但仍被 `pump` 每帧驱动的
    /// 孤儿 pane(SSH channel 泄漏,直到整条连接断开才关)。
    #[test]
    fn pane_still_wanted_rejects_a_leaf_dropped_by_a_later_preset_switch() {
        let mut ws = Workspace::new(test_pane(1), 0);
        // 切到 ThreeColumns:多出 2 个待开的新叶子。特意不 attach——模拟它们的
        // SSH channel 还在网络上跑,尚未收到 PaneOpened。
        let (fresh, _) = apply_layout_actions(
            &mut ws,
            &crate::ui::UiActions {
                preset: Some(Preset::ThreeColumns),
                close_pane: None,
                ..Default::default()
            },
        )
        .expect("点了预设,动作不该是 None");
        assert_eq!(fresh.len(), 2, "ThreeColumns 比原来的 1 屏多 2 个新叶子");

        // 在那 2 个 channel 回来之前,用户又切回 Single——把刚才的叶子从树上摘掉。
        let (fresh2, _) = apply_layout_actions(
            &mut ws,
            &crate::ui::UiActions {
                preset: Some(Preset::Single),
                close_pane: None,
                ..Default::default()
            },
        )
        .expect("点了预设,动作不该是 None");
        assert!(fresh2.is_empty(), "切回 Single 不会产生待开的新 channel");

        let remaining = mullion_core::layout::leaves(ws.tree());
        assert_eq!(remaining.len(), 1, "Single 下树上只剩 1 个叶子");

        // fresh 里必有至少一个 id 已经不在树上了——这就是"晚到"的那个 PaneOpened。
        let late_id = *fresh
            .iter()
            .find(|id| !remaining.contains(id))
            .expect("ThreeColumns 的 2 个新叶子里,Single 下必有至少一个被摘掉");
        assert!(
            !pane_still_wanted(&ws, late_id, ws.generation()),
            "id={late_id:?} 已经不在树上了,晚到的 PaneOpened 必须被拒绝,\
             不能 attach 成孤儿 pane"
        );

        // 正例:树上仍然存在的叶子、世代号也对得上,不该被误杀。
        let still_here = remaining[0];
        assert!(
            pane_still_wanted(&ws, still_here, ws.generation()),
            "id={still_here:?} 还在树上、世代也匹配,正常到达的 PaneOpened 不该被拒绝"
        );
    }

    /// 复核 Minor #3:`close_pane` 在"只剩最后一个 pane"时会拒绝并返回
    /// false、树不变。这种情况下 `apply_layout_actions` 不该无脑清掉
    /// `preset_out`,否则工具栏的当前预设高亮被平白抹掉,但树其实什么都
    /// 没变——对用户来说是一次"点了没反应,但高亮还消失了"的诡异体验。
    #[test]
    fn closing_the_last_pane_is_a_noop_and_does_not_clear_current_preset() {
        let mut ws = Workspace::new(test_pane(1), 0);
        let (_, preset_out) = apply_layout_actions(
            &mut ws,
            &crate::ui::UiActions {
                preset: Some(Preset::Single),
                close_pane: None,
                ..Default::default()
            },
        )
        .expect("点了预设,动作不该是 None");
        assert_eq!(preset_out, Some(Preset::Single));

        let only_id = mullion_core::layout::leaves(ws.tree())[0];
        let result = apply_layout_actions(
            &mut ws,
            &crate::ui::UiActions {
                preset: None,
                close_pane: Some(only_id),
                ..Default::default()
            },
        );
        assert!(
            result.is_none(),
            "关最后一个 pane 应该是纯粹的 noop(close_pane 返回 false),\
             不该报告出一个「布局变了」的动作"
        );
        assert!(ws.pane(only_id).is_some(), "最后一个 pane 不该被真的关掉");
    }

    /// 复核 Important #2/T2:没有 pane 卡在同步块里时,不该无中生有排一个
    /// `WaitUntil`(否则就是新开了一条会忙转的唤醒路径,踩 T3/T7)。
    #[test]
    fn sync_timeout_wake_is_none_when_no_pane_is_holding() {
        let start = std::time::Instant::now();
        assert_eq!(
            sync_timeout_wake_at(start, None, 0),
            None,
            "launcher 态(没有 Workspace)不该排任何唤醒"
        );

        let ws = Workspace::new(test_pane(1), 0);
        assert_eq!(
            sync_timeout_wake_at(start, Some(&ws), 0),
            None,
            "刚建好、没进过同步块的 pane 不该产生一个假的唤醒时刻"
        );
    }

    /// 复核 Important #2/T2:有 pane 卡在未超时的同步块里(远端发了 BSU 但还
    /// 没发 ESU)时,必须换算出一个绝对的唤醒 `Instant`——而不是让事件循环
    /// 无条件 `ControlFlow::Wait`,把「~150ms 自愈」的保证退化成「等下一个不
    /// 相关事件顺带救回来」。
    #[test]
    fn sync_timeout_wake_targets_the_holding_panes_deadline() {
        let start = std::time::Instant::now();
        let mut ws = Workspace::new(test_pane(1), 0);
        ws.pane_mut(PaneId(1))
            .unwrap()
            .pacer
            .feed(b"\x1b[?2026h", 0); // BSU 于 t=0,超时于 t=150

        let at = sync_timeout_wake_at(start, Some(&ws), 60)
            .expect("有 pane 卡在未超时的同步块里,必须返回一个唤醒时刻");
        let deadline_ms = at.duration_since(start).as_millis() as u64;
        assert_eq!(
            deadline_ms, 150,
            "唤醒时刻必须对齐 SYNC_TIMEOUT_MS(150ms),不是任意提前/推迟的时刻"
        );

        // 过了超时点:该 pane 不再算「卡住」(T2 的逃生门——过时就照常出帧,
        // 不再为它排唤醒),不该再排一次。
        assert_eq!(
            sync_timeout_wake_at(start, Some(&ws), 150),
            None,
            "超时已过,不该再为同一个 pane 排一次唤醒(否则是忙转)"
        );
    }

    /// T2 的另一半:vte 的 `Processor` 在 BSU 之后把字节攒在自己肚子里,
    /// `Term` 完全看不到——它那个超时点也必须被排进 `WaitUntil`。
    ///
    /// 只排 `SyncFramePacer` 那一个的话,「到点出帧」出的还是旧画面:字节根本
    /// 没进 `Term`。高延迟链路上 ESU 跟内容被拆进两个包时,现象就是画面永远
    /// 慢一拍,而且非得再敲一个键才会动。
    ///
    /// 自证会变红:把 `sync_timeout_wake_at` 里 `ws.vt_sync_deadline()` 那一项
    /// 去掉(只返回 `pacer`)。
    #[test]
    fn sync_timeout_wake_also_covers_the_emulators_own_sync_buffer() {
        let start = std::time::Instant::now();
        let mut ws = Workspace::new(test_pane(1), 0);
        // 只喂 emulator,**不喂 pacer** —— 这样返回值只可能来自 vt 那一路。
        ws.pane_mut(PaneId(1))
            .unwrap()
            .emulator
            .feed(b"\x1b[?2026h");

        let at = sync_timeout_wake_at(start, Some(&ws), 0)
            .expect("emulator 卡在同步块里,必须排一次唤醒去收口");
        assert_eq!(
            Some(at),
            ws.vt_sync_deadline(),
            "唤醒时刻必须就是 vte 记下的那个超时点"
        );

        // 收口之后不该再排:否则每帧都排一次,就是 T3/T7 的忙转。
        assert!(ws.flush_expired_vt_sync(at), "到点必须收得掉");
        assert_eq!(
            sync_timeout_wake_at(start, Some(&ws), 0),
            None,
            "收口后还在排唤醒 = 忙转"
        );
    }

    /// **接线守护 / F125**:闪烁的下一次唤醒必须并进既有的「定时唤醒」判据,
    /// 而不是各自为政 —— 两个定时源各排各的,后写的那个会把先写的覆盖掉,
    /// 症状是「光标闪起来之后同步块超时不再收口」(T2 复发)。
    ///
    /// 自证会变红:把 `next_timer_wake` 的函数体改成只 `self.sync_timeout_wake(now)`。
    #[test]
    fn timer_wakeups_are_merged_in_one_place() {
        let src = include_str!("app.rs");
        let after = src
            .split("    fn next_timer_wake(")
            .nth(1)
            .expect("找不到 next_timer_wake 的定义");
        let body = &after[..after.find("\n    }\n").expect("找不到函数结尾")];
        assert!(
            body.contains("self.sync_timeout_wake(now)"),
            "定时唤醒没并进同步块超时 —— T2 会复发"
        );
        assert!(
            body.contains("self.blink_wake("),
            "定时唤醒没并进光标闪烁 —— 光标只会在别的事件顺带唤醒时才翻转"
        );
    }

    /// **接线守护 / F125**:闪烁只许排 `WaitUntil`,不许 `request_redraw`。
    /// 后者绕开帧闸,是 T3(GPU 空转)/T7(100% CPU 忙转)的直接触发方式。
    ///
    /// 自证会变红:在 `blink_wake` 里加一句 `self.request_ui_redraw();`。
    #[test]
    fn blink_never_forces_a_redraw() {
        let src = include_str!("app.rs");
        let after = src
            .split("    fn blink_wake(")
            .nth(1)
            .expect("找不到 blink_wake 的定义");
        let body = &after[..after.find("\n    }\n").expect("找不到函数结尾")];
        assert!(
            !body.contains("request_redraw"),
            "闪烁不许直接请求重绘,只能排 WaitUntil(T3/T7)"
        );
    }

    /// 复核 C1(终审):跨「断开→重连」世代的 PaneId 碰撞。`Workspace::new` 每
    /// 次都从 `next_id = id.0 + 1` 起步(生产代码里首 pane 恒 `PaneId(1)`),
    /// 所以每建一个新 Workspace,分屏分配出的 id 都从 2 重新计数——旧世代
    /// 飞行中的 `PaneOpened` 抵达时,`id` 在新世代的树上完全可能被复用、甚至
    /// 已经被新世代自己正常 attach 过。只查 id/树成员(上一轮 Important #1
    /// 的 `pane_still_wanted`)会误判为"还需要",实际 attach 会顶掉新世代刚
    /// 建好、正常工作的 `PaneState`——连同它干净的 SSH channel 一起被 Drop,
    /// 该 pane 此后的输入全部写进一条已经没有意义的旧连接。
    #[test]
    fn pane_still_wanted_rejects_a_stale_generations_pane_even_if_the_id_is_reused() {
        // 新世代(重连后的第二个 Workspace,世代号 1)。
        let mut ws = Workspace::new(test_pane(1), 1);
        let (fresh, _) = apply_layout_actions(
            &mut ws,
            &crate::ui::UiActions {
                preset: Some(Preset::TwoLeftRight),
                close_pane: None,
                ..Default::default()
            },
        )
        .expect("点了预设,动作不该是 None");
        assert_eq!(
            fresh,
            vec![PaneId(2)],
            "新世代的 next_id 同样从 2 起步——这就是撞号的根源"
        );

        // 新世代自己的 open_pty 正常回来,attach 上去。last_grid 用一个哨兵值
        // 标记"这是新世代亲手建的 PaneState",后面拿它验证有没有被顶掉。
        let mut new_pane = test_pane(2);
        new_pane.last_grid = (99, 99);
        ws.attach_pane(new_pane);

        // 旧世代(世代号 0,已经被断开)当时也分配过 PaneId(2)、也在飞行中,
        // 现在才迟到抵达。
        assert!(
            !pane_still_wanted(&ws, PaneId(2), 0),
            "id=2 在树上、甚至已经 attach——单看 id/树成员会误判为「还需要」;\
             但这是旧世代(0)的事件,当前 Workspace 已经是世代 1,必须拒绝"
        );
        // 正例:同一世代(1)的事件应该照常被接受。
        assert!(
            pane_still_wanted(&ws, PaneId(2), 1),
            "世代匹配、id 也在树上,不该被误杀"
        );

        // 核心断言:生产代码在 pane_still_wanted 返回 false 时不会调
        // attach_pane,新世代的 PaneState 必须原封不动——没有被旧世代的迟到
        // 事件顶掉。
        assert_eq!(
            ws.pane(PaneId(2)).map(|p| p.last_grid),
            Some((99, 99)),
            "新世代刚建好的 PaneState 不该被旧世代的迟到事件顶掉"
        );
    }

    /// 一块 pane 上现在显示的是哪些字(拉平成一行,只用来断言"有没有留旧内容")。
    fn screen_text(p: &crate::shell::workspace::PaneState) -> String {
        let snap = p.emulator.snapshot();
        (0..snap.rows)
            .flat_map(|r| snap.row(r).iter().map(|c| c.ch).collect::<Vec<_>>())
            .collect()
    }

    /// 用户报的问题 2:分屏出来的 pane 要能换到别的节点。
    ///
    /// 每条断言各挡一类静默错误:
    /// - `host_ix` 没跟着改 → 键盘输入照旧写进**上一台**机器(屏幕上却是新机器
    ///   的提示符,用户完全看不出来)。
    /// - 沿用旧 `emulator` → 往上一翻是上一台机器的输出,同样"看起来对"。
    /// - `last_grid` 不复位 → 下一帧的 `apply_geometry` 认为尺寸没变、不发
    ///   `window_change`,新 channel 就一直按 80x24 排版(T4)。
    /// - `cwd`/`tmux` 不清空 → 标题条右区继续显示上一台机器嗅出来的目录/
    ///   tmux 会话名,换到新机器后仍然挂着,是一条过期的错误标注。
    ///
    /// 自证会变红:把 `rehost_pane` 里 `p.host_ix` / `p.emulator` / `p.last_grid`
    /// / `p.cwd` / `p.tmux` 任意一句赋值删掉。
    #[test]
    fn rehosting_a_pane_repoints_it_and_wipes_the_old_hosts_screen() {
        let mut ws = Workspace::new(test_pane(1), 0);
        {
            let p = ws.pane_mut(PaneId(1)).expect("首 pane 必在");
            p.emulator.feed(b"OLDHOST");
            p.saw_first_byte = true;
            p.last_grid = (120, 40);
            p.cwd = Some(b"/home/dev/A".to_vec());
            p.tmux = Some("work".into());
        }
        let (_tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
        assert!(
            rehost_pane(&mut ws, PaneId(1), 0, 3, Box::new(NullPty), rx),
            "pane 在、世代也对,换节点该成功"
        );
        let p = ws.pane(PaneId(1)).expect("换完 pane 还在");
        assert_eq!(p.host_ix, 3, "输入还会写到上一台机器上");
        assert!(
            !screen_text(p).contains("OLDHOST"),
            "屏幕上还留着上一台机器的输出:{}",
            screen_text(p).trim_end()
        );
        assert!(
            !p.saw_first_byte,
            "新机器还一个字节都没说话,就绪判据必须重攒"
        );
        assert_eq!(
            p.last_grid,
            (0, 0),
            "不复位的话下一帧不会发 window_change(T4)"
        );
        assert_eq!(p.cwd, None, "标题条右区还挂着上一台机器的目录,是过期信息");
        assert_eq!(
            p.tmux, None,
            "标题条右区还挂着上一台机器的 tmux 会话名,是过期信息"
        );
    }

    /// 拨号是真实网络往返,这期间用户完全可能把这块 pane 关掉、切预设、
    /// 或者断开重连(换了世代)。挂上去就是一个渲染看不见、`pump` 却仍在
    /// 驱动的孤儿 —— 同 `pane_still_wanted` 挡的那类。
    ///
    /// 自证会变红:把 `rehost_pane` 开头那个 `pane_still_wanted` 早退去掉。
    #[test]
    fn rehosting_a_pane_that_is_gone_is_refused() {
        let mut ws = Workspace::new(test_pane(1), 1);
        let (_tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
        assert!(
            !rehost_pane(&mut ws, PaneId(1), 0, 3, Box::new(NullPty), rx),
            "这是上一个世代发起的换节点,必须拒绝"
        );
        assert_eq!(
            ws.pane(PaneId(1)).map(|p| p.host_ix),
            Some(0),
            "被拒绝时不该动 pane 的任何字段"
        );
    }

    /// **接线守护**:`PaneOpenErr` 分支必须按世代过滤(不经过
    /// `pane_still_wanted`,因为它不关心 id/树成员,只关心"这条失败提示还是
    /// 不是当前世代的")。旧世代的失败提示如果不过滤,会给用户弹一条跟当前
    /// 连接毫不相干的错误 toast。
    ///
    /// S1(D0):世代号升格为**标签路由键**——过滤判据从"跟活动 ws 的世代比"
    /// 换成"`self.tabs` 里查不查得到这个世代的属主标签"。多标签下前者是错的:
    /// 后台标签开 pane 失败,拿活动标签的世代去比会把它误判成过期而静默吞掉。
    ///
    /// **扎的是源码结构而非运行时行为**,这是刻意的:`App` 要 `EventLoopProxy`
    /// 才能构造,单测里造不出来。验证边界:它只挡得住「分支里没有按标签查世代」
    /// 这一种写法,挡不住有人把查询结果的判断写成永真。
    ///
    /// 自证会变红:把这个分支里的 `if self.tabs.by_generation(...)` 整段删掉。
    #[test]
    fn pane_open_err_is_routed_by_generation_not_by_the_active_tab() {
        let src = include_str!("app.rs");
        let after = src
            .split("\n            UserEvent::PaneOpenErr {")
            .nth(1)
            .expect("找不到 PaneOpenErr 的事件分支");
        let body = &after[..after
            .find("\n            }\n")
            .expect("找不到 PaneOpenErr 分支的结尾")];
        assert!(
            body.contains("self.tabs.by_generation("),
            "PaneOpenErr 分支没按世代查属主标签 —— 要么旧世代的失败提示会弹到\
             当前连接头上,要么后台标签的失败会被活动标签的世代误判成过期吞掉"
        );
    }

    /// **接线守护**:`accept_automation_done` 必须真的按世代过滤。
    ///
    /// 把 `accept_automation_done` 里的过滤整段删掉,全量测试依然全绿 ——
    /// 说明「这个函数有没有用上世代判据」是无人守护的。而这正是它存在的全部
    /// 理由:高延迟链路下用户完全可能在自动化还在跑时断开重连,旧世代的
    /// 「自动化已中止:连接已断开」落到新连接的状态栏上,是一条与当前连接
    /// 毫不相干的误导信息。
    ///
    /// S1(D0):判据同 `PaneOpenErr`,从"跟活动 ws 的世代比"换成"按世代查
    /// 属主标签"——后台标签的自动化结论要落回**它自己那个标签**的状态栏,
    /// 拿活动标签去比会把它丢掉。
    ///
    /// **扎的是源码结构而非运行时行为**,这是刻意的:`App` 要
    /// `EventLoopProxy` 才能构造,单测里造不出来。验证边界:它只挡得住
    /// 「函数体里没按世代查标签」这一种写法,挡不住有人换个同义判据或把过滤
    /// 写成永真。
    ///
    /// 自证会变红:把 `accept_automation_done` 里的世代校验整段删掉。
    #[test]
    fn automation_done_is_filtered_by_generation() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn accept_automation_done")
            .nth(1)
            .expect("找不到 accept_automation_done 的定义");
        let body = &after[..after
            .find("\n    }\n")
            .expect("找不到 accept_automation_done 的函数结尾")];
        assert!(
            body.contains(".by_generation_mut(generation)"),
            "accept_automation_done 里没有世代过滤 —— 旧连接迟到的自动化结论\
             会覆盖新连接的状态栏,给用户看一条与当前连接毫不相干的信息"
        );
    }

    /// **接线守护**:`accept_sftp_opened` 必须按世代查属主标签,不是"当前
    /// 活动标签"。用户在标签 A 开侧栏、切到标签 B 的几百毫秒里 sftp 打开结果
    /// 抵达,拿活动标签接会把 A 的 client 挂到 B 上 —— B 的侧栏此后就在
    /// A 的主机上操作,用户看不出来,直到删错文件。
    ///
    /// **扎的是源码结构而非运行时行为**:`App` 要 `EventLoopProxy` 才能构造,
    /// 单测里造不出来。验证边界:只挡得住「函数体里没按世代查标签」这一种
    /// 写法。
    ///
    /// 自证会变红:把 `accept_sftp_opened` 里的 `.by_generation_mut(generation)`
    /// 全部换成 `self.tabs.active_mut()`(即"接到活动标签"而非"按世代查")。
    #[test]
    fn sftp_opened_is_routed_by_generation_not_by_the_active_tab() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn accept_sftp_opened(")
            .nth(1)
            .expect("找不到 accept_sftp_opened 的定义");
        let body = &after[..after
            .find("\n    fn accept_sftp_listed(")
            .expect("找不到 accept_sftp_opened 的函数结尾")];
        assert!(
            body.contains(".by_generation_mut(generation)"),
            "accept_sftp_opened 没按世代查属主标签 —— 迟到的 sftp client 会\
             挂到当前活动标签而不是真正发起打开请求的那个标签上"
        );
    }

    /// **接线守护**:F50 的六条文件动作路径都必须走 `TabContent` 的**通用**
    /// 访问器(`files_panel_mut`/`sftp_tasks_mut`/`sftp_mut`/`sftp_connection`),
    /// 不许退回 `as_terminal_mut()`。
    ///
    /// 退回去的症状**全都是静默的**,而且一个比一个难查:
    /// - `track_sftp_task`:找不到「终端标签」时它会 `task.abort()` 兜底 ——
    ///   对文件标签就是**刚 spawn 的 open 请求被自己 abort 掉**,标签永远卡在
    ///   「正在读取目录…」,没有任何报错。
    /// - `accept_sftp_opened` / `accept_sftp_listed`:结果被当成「过期世代」丢掉,
    ///   只在 debug 日志里留一行。
    /// - `trigger_sftp_open` / `apply_*_file_action`:文件标签压根找不到入口。
    ///
    /// **扎的是源码结构**(`App` 要 `EventLoopProxy`,单测里造不出来;`FilesTab`
    /// 还要 `Arc<SshConnection>`,而 `SshConnection::new` 是 `pub(crate)`,
    /// `mullion-app` 这边根本构造不出来)。验证边界:只挡得住「函数体里直接写
    /// `as_terminal_mut`」这一种退化,挡不住「换个名字的等价单路径写法」。
    ///
    /// 自证会变红:把其中任意一个函数体里的通用访问器换回
    /// `.and_then(|tab| tab.content.as_terminal_mut())`。
    /// (复核实测:补这条之前,六处**全部**换回去,677 条测试一条都不红。)
    #[test]
    fn file_actions_never_narrow_to_terminal_tabs_only() {
        let src = include_str!("app.rs");
        for name in [
            "fn apply_local_file_action",
            "fn apply_remote_file_action",
            "fn track_sftp_task",
            "fn trigger_sftp_open",
            "fn accept_sftp_opened",
            "fn accept_sftp_listed",
        ] {
            let after = src
                .split(name)
                .nth(1)
                .unwrap_or_else(|| panic!("找不到 {name} 的定义"));
            // 这几个都是 `impl App` 里的方法,函数体止于第一个 4 空格缩进的
            // 右花括号;更深的嵌套块收在 8 空格及以上,不会提前截断。
            let body = &after[..after
                .find("\n    }\n")
                .unwrap_or_else(|| panic!("找不到 {name} 的函数结尾"))];
            // 先证明切出来的确实是个函数体:切歪成空串的话下面那条否定断言
            // 会空过,这条测试就成了摆设(本项目吃过「探针读在被测路径之外」
            // 的亏)。每个函数体都必然提到 generation —— 六条全是按世代路由的。
            assert!(
                body.contains("generation"),
                "{name} 的函数体切歪了(切出来 {} 字节,没提到 generation)——\
                 下面那条断言会空过",
                body.len()
            );
            assert!(
                !body.contains("as_terminal_mut"),
                "{name} 收窄成了只认终端标签 —— SFTP 节点开的文件标签会被静默\
                 跳过(track_sftp_task 那处更狠:在途的 open 请求会被自己 abort,\
                 标签永远停在「正在读取目录…」且不报错)"
            );
        }
    }

    /// 协调者修订 3:`handle_panel_key`(及其内部工具 `dispatch_panel_action`/
    /// `dispatch_panel_action_for`/`move_panel_selection`)必须按**属主标签**
    /// (`generation` 参数)操作,不能落回"活动标签"——跟
    /// `apply_remote_file_action`/`apply_local_file_action` 的既有约定一致
    /// (S1 路由纪律)。理由跟 F50 那六条路径一样:`window_event` 判定阶段
    /// 算出的属主标签,和这次按键真正处理时的活动标签,理论上可能不是同一个
    /// (异步 dispatch 的间隙里活动标签可能已经切走)——落回活动标签会让
    /// 按键作用在错误的标签上。
    ///
    /// **扎的是源码结构**,理由同 `file_actions_never_narrow_to_terminal_tabs_only`
    /// ——`App` 单测里造不出来(`EventLoopProxy`)。验证边界:只挡得住这四个
    /// 函数体里直接出现 `self.tabs.active()`/`self.tabs.active_mut()` 这种
    /// 写法,挡不住换个等价说法的更隐蔽退化。
    ///
    /// 自证会变红:把 `move_panel_selection` 里的
    /// `self.tabs.by_generation_mut(generation)` 换成 `self.tabs.active_mut()`。
    #[test]
    fn panel_key_handling_is_routed_by_generation_not_by_the_active_tab() {
        let src = include_str!("app.rs");
        for name in [
            "fn handle_panel_key(",
            "fn dispatch_panel_action(",
            "fn dispatch_panel_action_for(",
            "fn move_panel_selection(",
        ] {
            let after = src
                .split(name)
                .nth(1)
                .unwrap_or_else(|| panic!("找不到 {name} 的定义"));
            let body = &after[..after
                .find("\n    }\n")
                .unwrap_or_else(|| panic!("找不到 {name} 的函数结尾"))];
            assert!(
                body.contains("generation"),
                "{name} 的函数体切歪了(切出来 {} 字节,没提到 generation)——\
                 下面那条断言会空过",
                body.len()
            );
            assert!(
                !body.contains("self.tabs.active()") && !body.contains("self.tabs.active_mut()"),
                "{name} 落回了「活动标签」,而不是按 generation 路由的属主标签"
            );
        }
    }

    /// 代码复核挖出的真 bug:F6/Tab 切到面板焦点之后,`panel_key_pending`
    /// 这条分支只经 `handle_panel_key` 标脏(`self.ui_dirty = true`),从不
    /// 请求重绘。事件循环整个跑在 `ControlFlow::Wait`/`WaitUntil` 上
    /// (T3/T7),纯键盘交互不产生任何异步 `UserEvent`,画面会一直停在
    /// 按键前那一帧,直到鼠标挪一下之类的无关事件顺带触发重绘才刷新。
    ///
    /// **扎的是源码结构**:`App`/`Window` 在无头单测里都造不出来
    /// (`Window` 需要真实事件循环),没法写行为测试。验证边界:只挡得住
    /// `panel_key_pending` 这个分支里字面上缺 `request_ui_redraw()` 调用
    /// 的退化,挡不住换个等价说法(比如另起一个不生效的重绘函数)的更隐蔽
    /// 走样。
    ///
    /// 自证会变红:把补的那行 `self.request_ui_redraw();` 删掉。
    #[test]
    fn panel_key_pending_requests_a_redraw_so_keyboard_only_input_is_not_stuck_on_a_stale_frame() {
        let src = include_str!("app.rs");
        let after = src
            .split("if panel_key_pending {")
            .nth(1)
            .expect("找不到 panel_key_pending 分支");
        let body = &after[..after
            .find("\n        match event {")
            .expect("找不到 panel_key_pending 分支的结尾(下一个 match event)")];
        assert!(
            body.contains("self.handle_panel_key(gen"),
            "切歪了,没切到 handle_panel_key 调用——下面那条断言会空过"
        );
        let after_call = body
            .split("self.handle_panel_key(gen")
            .nth(1)
            .expect("上面已断言 contains,这里不该找不到");
        assert!(
            after_call.contains("self.request_ui_redraw()"),
            "panel_key_pending 分支处理完键之后没有请求重绘——键盘单独触发\
             的 Tab 换栏/↑↓选中/Enter/Backspace/F5/Ctrl+H 会卡在按键前的\
             那一帧,直到无关事件顺带重绘才刷新"
        );
    }

    /// 空列表没有"选第几个"这个概念——`move_panel_selection` 里原本靠
    /// `rows.is_empty()` 提前 `return` 挡住,抽出来之后这条边界只有纯函数
    /// 自己能直接测(`App`/`Window` 在无头单测里造不出来)。
    #[test]
    fn panel_selection_with_an_empty_row_list_has_no_answer() {
        let rows: [&mullion_ssh::sftp::Entry; 0] = [];
        assert_eq!(next_panel_selection_index(&rows, None, 1), None);
        assert_eq!(next_panel_selection_index(&rows, None, -1), None);
    }

    /// `next_panel_selection_index` 的边界:没有选中项时,`↓`(`delta > 0`)
    /// 落到第一行。
    #[test]
    fn panel_selection_with_nothing_selected_and_arrow_down_lands_on_first_row() {
        let a = panel_selection_test_entry(b"a.txt");
        let b = panel_selection_test_entry(b"b.txt");
        let rows = [&a, &b];
        assert_eq!(next_panel_selection_index(&rows, None, 1), Some(0));
    }

    /// 没有选中项时,`↑`(`delta < 0`)落到最后一行——用户第一下按方向键该
    /// 落在看得见的那一头。
    #[test]
    fn panel_selection_with_nothing_selected_and_arrow_up_lands_on_last_row() {
        let a = panel_selection_test_entry(b"a.txt");
        let b = panel_selection_test_entry(b"b.txt");
        let c = panel_selection_test_entry(b"c.txt");
        let rows = [&a, &b, &c];
        assert_eq!(next_panel_selection_index(&rows, None, -1), Some(2));
    }

    /// 只有一行时,不管选没选、往哪个方向按,都只能停在那一行(下标 0)。
    #[test]
    fn panel_selection_with_a_single_row_stays_on_it_either_direction() {
        let a = panel_selection_test_entry(b"only.txt");
        let rows = [&a];
        assert_eq!(next_panel_selection_index(&rows, None, 1), Some(0));
        assert_eq!(next_panel_selection_index(&rows, None, -1), Some(0));
        assert_eq!(
            next_panel_selection_index(&rows, Some(&a.name), 1),
            Some(0),
            "已经是唯一一行,↓ 不该越界"
        );
        assert_eq!(
            next_panel_selection_index(&rows, Some(&a.name), -1),
            Some(0),
            "已经是唯一一行,↑ 不该越界"
        );
    }

    /// 选中第一行再按 `↑`:必须夹在 0,不能下溢成一个巨大的 `usize`
    /// (`(0i32 - 1).clamp(0, ..)` 算对了才行,这条钉的正是这个夹紧)。
    #[test]
    fn panel_selection_on_the_first_row_pressing_up_clamps_to_zero() {
        let a = panel_selection_test_entry(b"a.txt");
        let b = panel_selection_test_entry(b"b.txt");
        let rows = [&a, &b];
        assert_eq!(
            next_panel_selection_index(&rows, Some(&a.name), -1),
            Some(0)
        );
    }

    /// 选中最后一行再按 `↓`:必须夹在 `rows.len() - 1`,不能越界出去。
    #[test]
    fn panel_selection_on_the_last_row_pressing_down_clamps_to_the_end() {
        let a = panel_selection_test_entry(b"a.txt");
        let b = panel_selection_test_entry(b"b.txt");
        let rows = [&a, &b];
        assert_eq!(next_panel_selection_index(&rows, Some(&b.name), 1), Some(1));
    }

    /// 选中项按**身份**(名字)定位,不是恒定的某个下标——3 行里选中中间那
    /// 行再按 `↓`,必须落到第三行。挑中间行是故意的:两端的边界测试哪怕
    /// `position` 查找被错误地退化成"恒当作第 0 行",算出来的结果也可能
    /// 跟正确答案凑巧撞上(比如选中第一行时两者都是 0)——只有选中中间行
    /// 才能把这类退化跟正确实现的结果分开。
    #[test]
    fn panel_selection_finds_the_selected_row_by_identity_not_a_fixed_index() {
        let a = panel_selection_test_entry(b"a.txt");
        let b = panel_selection_test_entry(b"b.txt");
        let c = panel_selection_test_entry(b"c.txt");
        let rows = [&a, &b, &c];
        assert_eq!(next_panel_selection_index(&rows, Some(&b.name), 1), Some(2));
    }

    /// 选中项已经不在当前 `rows` 里(比如切隐藏文件显示、或换目录之后旧的
    /// `selected` 还没来得及清)——`position` 找不到,必须按"未选中"处理,
    /// 不能 panic 也不能悄悄选中一个不相关的行。
    #[test]
    fn panel_selection_of_a_row_no_longer_present_falls_back_to_the_unselected_case() {
        let a = panel_selection_test_entry(b"a.txt");
        let stale = mullion_ssh::sftp::RemotePath::from_bytes(b"gone.txt".to_vec());
        let rows = [&a];
        assert_eq!(
            next_panel_selection_index(&rows, Some(&stale), 1),
            Some(0),
            "落不到的选中项,↓ 应该退回「未选中」那一支,落到第一行"
        );
    }

    fn panel_selection_test_entry(name: &[u8]) -> mullion_ssh::sftp::Entry {
        mullion_ssh::sftp::Entry {
            name: mullion_ssh::sftp::RemotePath::from_bytes(name.to_vec()),
            kind: mullion_ssh::sftp::EntryKind::File,
            size: 1024,
            mtime: 1_700_000_000,
            mode: 0o644,
            uid: 1000,
            gid: 1000,
            link_target: None,
        }
    }

    /// **接线守护**:`accept_sftp_listed` 同样必须按世代查属主标签。
    ///
    /// 验证边界与自证方式同上一条。
    #[test]
    fn sftp_listed_is_routed_by_generation_not_by_the_active_tab() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn accept_sftp_listed(")
            .nth(1)
            .expect("找不到 accept_sftp_listed 的定义");
        let body = &after[..after
            .find("\n    /// UI 侧变了")
            .expect("找不到 accept_sftp_listed 的函数结尾")];
        assert!(
            body.contains(".by_generation_mut(generation)"),
            "accept_sftp_listed 没按世代查属主标签 —— 迟到的目录列表会落到\
             当前活动标签而不是真正发起这次列目录的那个标签上"
        );
    }

    /// **接线守护**(B3):`accept_sftp_listed` 的 `Err` 分支必须过一遍
    /// `files::fail::classify` 分流,不能像 `Ok` 分支那样直接甩给
    /// `PaneState::accept`(那样只会恒落 `Load::Failed`,连接死了也不会
    /// 转 `Disconnected`、用户就看不到「重连」入口)。
    ///
    /// **扎的是源码结构**:`App` 单测里造不出来(`EventLoopProxy`),这里
    /// 只挡得住「函数体里没提这三个符号」这一种退化。
    ///
    /// 自证会变红:把 `Err` 分支换成直接 `pane.accept(seq, Err(msg))`
    /// (即退回旧写法,丢掉分类)。
    #[test]
    fn sftp_listed_error_is_routed_through_fail_classify() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn accept_sftp_listed(")
            .nth(1)
            .expect("找不到 accept_sftp_listed 的定义");
        let body = &after[..after
            .find("\n    /// UI 侧变了")
            .expect("找不到 accept_sftp_listed 的函数结尾")];
        assert!(
            body.contains("crate::files::fail::classify(&msg)"),
            "accept_sftp_listed 的 Err 分支没有过 files::fail::classify 分类"
        );
        assert!(
            body.contains("crate::files::state::Load::Disconnected"),
            "accept_sftp_listed 的 Err 分支没有落地 Load::Disconnected 分支"
        );
        assert!(
            body.contains("crate::files::state::Load::Failed(msg)"),
            "accept_sftp_listed 的 Err 分支没有保留 Load::Failed 分支(路径级仍要停在原地报错)"
        );
    }

    /// **接线守护**(B3 复核修订):`demote_files_tab_and_reconnect` 必须先
    /// `wind_down` 旧连接的内容,再把 `content` 换成占位标签。
    ///
    /// 漏掉这一步:旧连接的后台任务(含它们手里那份 `Arc<SshConnection>`,
    /// 每个任务靠它保活)不会收口,是 ADR-009 说的 channel 泄漏那一类问题的
    /// 又一个变种——只是触发点从「关标签」换成了「点重连」。
    ///
    /// **复核实测过**:删掉这段 `wind_down` 调用,`cargo test --workspace`
    /// 全绿——没有任何既有测试覆盖到这条路径,必须专门补一条。
    ///
    /// **扎的是源码结构**(`App` 单测里造不出来,理由同本文件其余接线守护:
    /// 需要 `EventLoopProxy`)。验证边界:只挡得住「函数体里没提 wind_down」
    /// 这一种退化,挡不住换个等价说法的更隐蔽退化。
    ///
    /// 自证会变红:删掉函数体里 `wind_down(Tab { ... })` 那几行。
    #[test]
    fn demote_files_tab_and_reconnect_winds_down_the_old_connection() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn demote_files_tab_and_reconnect(")
            .nth(1)
            .expect("找不到 demote_files_tab_and_reconnect 的定义");
        let body = &after[..after
            .find("\n    fn reconnect_next_restored")
            .expect("找不到 demote_files_tab_and_reconnect 的函数结尾")];
        // 先证明切出来的确实是函数体,不是切歪了的空/无关片段 —— 下面那条
        // 否定断言(其实是肯定断言里带否定语气)才有意义。
        assert!(
            body.contains("mem::replace"),
            "demote_files_tab_and_reconnect 的函数体切歪了(切出来 {} 字节,\
             没提到 mem::replace)—— 下面那条断言会空过",
            body.len()
        );
        assert!(
            body.contains("wind_down("),
            "demote_files_tab_and_reconnect 没有调用 wind_down —— 旧连接的\
             后台任务(含它们手里那份 Arc<SshConnection>)不会收口,是\
             ADR-009 说的 channel 泄漏(触发点从「关标签」换成了「点重连」)"
        );
    }

    /// **接线守护**(B3 复核修订):`accept_sftp_listed` 判定为 `Disconnected`
    /// 之后必须清空 sftp 槽位,「重连」才不是死按钮。
    ///
    /// 背景:`trigger_sftp_open` 的短路守卫是
    /// `tab.content.sftp_client().is_some() || already_loading`——全仓唯一
    /// 写 `Some` 的地方是 `accept_sftp_opened` 的 `Ok` 分支,`sftp_client()`
    /// 只克隆 `Arc`、不做存活性检查。B3 要处理的场景**恰好**是「channel
    /// 已经开成功、用着用着死了」,这意味着槽位在失败发生的那一刻必然是
    /// `Some(一个死掉的 client)`——如果没人清掉它,用户点「重连」时
    /// `trigger_sftp_open` 会在第一行直接 `return`,界面上什么反应都没有。
    ///
    /// **扎的是源码结构**(`App`/`EventLoopProxy` 单测里造不出来)。判据钉在
    /// 两处必须同时成立才算数,不是孤立断言槽位赋值本身(那样太容易变成
    /// 重言式):
    /// 1. `accept_sftp_listed` 里,紧跟在「读到 `Load::Disconnected`」这个
    ///    条件之后,确实清了 `sftp_mut()` 槽位;
    /// 2. `trigger_sftp_open` 的短路守卫确实读的还是同一个 `sftp_client()`
    ///    ——防止有人把守卫改成读别的字段,让第 1 条断言变成不痛不痒的死代码。
    ///
    /// 自证会变红:删掉 `accept_sftp_listed` 里 `just_disconnected` 那段清
    /// 槽位的代码(复核实测过:删掉之后 `cargo test --workspace` 全绿,
    /// 因为没有任何测试越过 `App` 的边界验证这条因果链)。
    #[test]
    fn reconnect_after_a_session_failure_actually_reopens_the_channel() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn accept_sftp_listed(")
            .nth(1)
            .expect("找不到 accept_sftp_listed 的定义");
        let body = &after[..after
            .find("\n    /// UI 侧变了")
            .expect("找不到 accept_sftp_listed 的函数结尾")];

        // 「读到 Disconnected 状态」这一句必须存在,且清槽位的代码要跟着
        // 它走——不能是与判定逻辑脱节的孤立赋值。
        let after_disconnect_check =
            body.split("Load::Disconnected)").nth(1).unwrap_or_else(|| {
                panic!(
                    "accept_sftp_listed 里没有找到「读 Disconnected 状态之后」\
                     这一段(形如 `matches!(f.remote.load, ...Load::Disconnected)`)——\
                     清槽位的判据应该跟着这个状态走,不能是孤立赋值"
                )
            });
        assert!(
            after_disconnect_check.contains("sftp_mut()")
                && after_disconnect_check.contains("= None"),
            "accept_sftp_listed 判定连接断开之后没有清空 sftp 槽位 —— \
             trigger_sftp_open 的既有短路守卫会挡住重连请求,「重连」变成死按钮"
        );

        // 守卫读的必须还是同一个 sftp_client() —— 防止上面那条断言被
        // 「守卫改了个说法,清槽位这行成了摆设」这种退化绕过去。
        let trigger_after = src
            .split("fn trigger_sftp_open(")
            .nth(1)
            .expect("找不到 trigger_sftp_open 的定义");
        assert!(
            trigger_after.contains("tab.content.sftp_client().is_some() || already_loading"),
            "trigger_sftp_open 的短路守卫变了 —— 需要重新核对「清槽位能解锁\
             重连」这条因果链是否还成立"
        );
    }

    /// **接线守护**:`PanelFrame`(侧栏两栏运行态)必须挂在 `TerminalTab` 上,
    /// 不能挂回 `App`。
    ///
    /// 背景:Task 9 时远端栏恒 `Load::Idle`,挂在 `App` 上是权宜实现,单标签
    /// 看不出问题。Task 10a 接上真实 sftp 数据之后,若退回挂到 `App` 级
    /// 全局一份,标签 A 连主机甲、标签 B 连主机乙,侧栏会把甲的目录内容显示
    /// 在乙的标签下 —— 用户在不知情的情况下对着错误的主机操作(删除/上传,
    /// 虽然本切片只读,但下一片 D2 就会加写操作,这个地基现在不钉死以后更难改)。
    ///
    /// **扎的是源码结构**:直接断言两个 struct 定义体里 `PanelFrame` 这个
    /// 类型名出现在哪个 struct。验证边界:挡得住"整个字段搬回 App"这种写法,
    /// 挡不住"两边都留一份、只是没人用 App 那份"这类更隐蔽的走样。
    ///
    /// 自证会变红:把 `files: crate::ui::files_panel::PanelFrame` 那行从
    /// `TerminalTab` 挪回 `struct App { ... }` 里。
    #[test]
    fn files_sidebar_state_lives_on_the_tab_not_on_app() {
        let src = include_str!("app.rs");

        let app_after = src
            .split("pub struct App {")
            .nth(1)
            .expect("找不到 struct App 的定义");
        let app_body = &app_after[..app_after.find("\n}\n").expect("找不到 struct App 的结尾")];
        assert!(
            !app_body.contains("PanelFrame"),
            "PanelFrame 不该再挂在 App 上 —— 会导致多标签共享一份侧栏状态,\
             标签 B 显示标签 A 主机的目录内容"
        );

        let tab_after = src
            .split("struct TerminalTab {")
            .nth(1)
            .expect("找不到 struct TerminalTab 的定义");
        let tab_body = &tab_after[..tab_after
            .find("\n}\n")
            .expect("找不到 struct TerminalTab 的结尾")];
        assert!(
            tab_body.contains("PanelFrame"),
            "PanelFrame 没有挂在 TerminalTab 上 —— 侧栏状态就无处安放了"
        );
    }

    /// **前置 A(补测回归)**:`files_remote` 单独一个真实动作时,必须能穿过
    /// `render_frame` 内部的 discard 趟保护(`has_real_action`),不能被静默
    /// 吃掉。
    ///
    /// 背景:一次代码评审发现,`this_pass.preset.is_some() || ... ||
    /// this_pass.files_remote.is_some() || ...` 这条判断链里,`files_remote`
    /// 那一句即使被删掉,`cargo test --workspace`(662 个既有测试)依然全绿
    /// —— 因为没有任何测试构造过"`files_remote` 是这一帧唯一真实动作"的
    /// `UiActions`。这条测试直接测 `has_real_action`(`render_frame` 内部
    /// discard 趟真正调用的那个函数,不是重新抄一遍判断逻辑),把这个缺口钉死。
    #[test]
    fn files_remote_alone_counts_as_a_real_action_for_the_discard_guard() {
        let a = crate::ui::UiActions {
            files_remote: Some(crate::ui::files_panel::FileAction::Refresh),
            ..Default::default()
        };
        assert!(
            has_real_action(&a),
            "files_remote 单独一个真实动作时必须被 has_real_action 认成\
             「有真实动作」,否则 egui 的 discard 趟会把它静默吃掉(见\
             render_frame 内部长注释)"
        );
    }

    /// F52:同上,`files_drop_in` 那一条。**新加字段必须各配一条** ——
    /// 上面那条守的是 `files_remote`,对新字段一点保护也没有;而漏掉的
    /// 后果同样是「拖进来一批文件,程序毫无反应」。
    #[test]
    fn a_drop_in_alone_counts_as_a_real_action_for_the_discard_guard() {
        let a = crate::ui::UiActions {
            files_drop_in: vec![std::path::PathBuf::from("/tmp/a.txt")],
            ..Default::default()
        };
        assert!(
            has_real_action(&a),
            "files_drop_in 单独一个真实动作时必须被 has_real_action 认成\
             「有真实动作」,否则拖进来的文件会在 discard 趟被静默丢掉"
        );
    }

    /// F59:同上,`files_drag_out` 那一条。计划里原本假设「拖出走既有的
    /// `files_remote` 字段所以不用改 `has_real_action`」——实际接线走的是
    /// 独立字段,假设不成立。漏掉的后果是「拖出窗口松了手,什么都没发生」。
    #[test]
    fn a_drag_out_alone_counts_as_a_real_action_for_the_discard_guard() {
        let a = crate::ui::UiActions {
            files_drag_out: true,
            ..Default::default()
        };
        assert!(
            has_real_action(&a),
            "files_drag_out 单独一个真实动作时必须被 has_real_action 认成\
             「有真实动作」,否则这一拖会在 discard 趟被静默丢掉"
        );
    }

    /// F37:同上,`reconnect_tab` 那一条。漏掉的后果是「占位标签上按重连
    /// 毫无反应」——而恢复出来的标签除了重连没有第二条出路,用户只能关掉
    /// 它重新去会话管理器找。
    #[test]
    fn a_reconnect_click_alone_counts_as_a_real_action_for_the_discard_guard() {
        let a = crate::ui::UiActions {
            reconnect_tab: Some(crate::shell::tabs::TabId(3)),
            ..Default::default()
        };
        assert!(
            has_real_action(&a),
            "reconnect_tab 单独一个真实动作时必须被 has_real_action 认成\
             「有真实动作」,否则「重连」按下去会在 discard 趟被静默吃掉"
        );
    }

    /// F37:同上,菜单里那条「全部重连」。
    #[test]
    fn reconnect_all_alone_counts_as_a_real_action_for_the_discard_guard() {
        let a = crate::ui::UiActions {
            reconnect_all: true,
            ..Default::default()
        };
        assert!(
            has_real_action(&a),
            "reconnect_all 单独一个真实动作时必须被 has_real_action 认成\
             「有真实动作」,否则菜单里点「全部重连」毫无反应"
        );
    }

    /// **接线守护 / F37**:关窗口那一刻必须**无条件**补写一次布局,而且要在
    /// `event_loop.exit()` 之前。
    ///
    /// 用户最后那几下操作(切标签、拖窗口)大概率落在上一次落盘之后的 2 秒
    /// 节流窗口里;不在这里补一次,「关窗前那一刻的样子」——这个功能唯一要
    /// 还原的东西——就永远丢了。
    ///
    /// **扎源码结构**:走到这条路要 `EventLoopProxy` + 真窗口,单测造不出来。
    /// 自证会变红:删掉 `CloseRequested` 里那句 `self.save_layout_if_changed();`,
    /// 或把它挪到 `event_loop.exit()` 后面。
    #[test]
    fn closing_the_window_writes_the_layout_even_inside_the_throttle_window() {
        let src = include_str!("app.rs");
        let after = src
            .split("WindowEvent::CloseRequested => {")
            .nth(1)
            .expect("找不到 CloseRequested 分支");
        let body = &after[..after.find("event_loop.exit();").expect("找不到 exit()")];
        assert!(
            body.contains("self.save_layout_if_changed();"),
            "关窗口没补写布局 —— 最后 2 秒内的改动会永久丢失"
        );
    }

    /// **接线守护 / F37**:空闲路径要定期落盘。只靠关窗口那一次的话,进程被
    /// 杀/崩溃时整场布局全丢。
    ///
    /// 自证会变红:删掉 `about_to_wait` 里那句 `self.flush_layout_if_due();`。
    #[test]
    fn the_idle_path_flushes_the_layout_periodically() {
        let src = include_str!("app.rs");
        let after = src
            .split("\n    fn about_to_wait(")
            .nth(1)
            .expect("找不到 about_to_wait 的定义");
        let body = &after[..after
            .find("\n    }\n")
            .expect("找不到 about_to_wait 的函数结尾")];
        assert!(
            body.contains("self.flush_layout_if_due();"),
            "about_to_wait 不再定期落盘布局 —— 进程被杀时整场布局全丢"
        );
    }

    /// **接线守护 / F124**:自举 tick 必须挂在 `about_to_wait` 上。
    ///
    /// 挂在别处的后果是静默的:挂在 `RedrawRequested` 的 `Present` 分支里,
    /// 被节流掉的帧就不跑;不挂,整个功能一次都不会发起。
    ///
    /// **扎的是源码结构**:真正验它要一条活连接 + `EventLoopProxy`,这个
    /// 测试容器里造不出来。验证边界:挡得住「整个调用被删/挪走」,挡不住
    /// 「函数体被掏空」。
    ///
    /// 自证会变红:把 `self.tick_tmux_bootstrap();` 从 `about_to_wait` 里删掉。
    #[test]
    fn about_to_wait_ticks_the_tmux_bootstrap() {
        let src = include_str!("app.rs");
        let after = src
            .split("\n    fn about_to_wait(")
            .nth(1)
            .expect("找不到 about_to_wait 的定义");
        let body = &after[..after
            .find("\n    }\n")
            .expect("找不到 about_to_wait 的函数结尾")];
        assert!(
            body.contains("self.tick_tmux_bootstrap();"),
            "about_to_wait 不再跑自举 tick —— F124 一次都不会发起"
        );
    }

    /// **接线守护 / F124**:草稿里的自举开关要真被搬进 `self.settings`。
    ///
    /// 漏掉这一行的症状很隐蔽:复选框点得动、界面也重画了(`Preview` 回报是
    /// 草稿层的事),但 `self.settings` 一直是老值,于是「关掉」既不生效、
    /// 按确定也不会落盘,下次打开弹窗又显示开着。
    ///
    /// **扎的是源码结构**:`take_settings_draft` 要一个完整的 `App`,这个测试
    /// 容器里造不出来。验证边界:挡得住「整行被删」,挡不住「搬的是常量」。
    ///
    /// 自证会变红:把 `self.settings.tmux_bootstrap = d.tmux_bootstrap;` 删掉。
    #[test]
    fn the_settings_draft_write_back_carries_the_bootstrap_switch() {
        let src = include_str!("app.rs");
        let after = src
            .split("\n    fn take_settings_draft(&mut self) {")
            .nth(1)
            .expect("找不到 take_settings_draft 的定义");
        let body = &after[..after
            .find("\n    }\n")
            .expect("找不到 take_settings_draft 的函数结尾")];
        assert!(
            body.contains("self.settings.font_pt"),
            "函数体切歪了({} 字节)",
            body.len()
        );
        assert!(
            body.contains("self.settings.tmux_bootstrap = d.tmux_bootstrap;"),
            "自举开关没被搬进 settings —— 用户关不掉、也存不住"
        );
    }

    /// **接线守护 / F124**:tick 的三件事都得在——判据走
    /// `remote_bootstrap::should_attempt`、发的是 `bootstrap_command()`、
    /// 结论按退出码写回 `finish(..)`。
    ///
    /// 每一条漏掉都是静默的:
    /// - 不走 `should_attempt` → 要么每帧发一次 exec(高延迟链路上刷爆),
    ///   要么只发第一次(tmux 服务器晚起就永远配不上)。
    /// - 不用 `bootstrap_command()` → live 测试验的命令跟实际发的不是同一条。
    /// - 不写回 `finish` → `busy` 永远置着,第一次之后再也不重试。
    ///
    /// 自证会变红:把 `should_attempt(..)` 换成 `true`。
    #[test]
    fn the_bootstrap_tick_uses_the_shared_predicate_command_and_writes_back() {
        let src = include_str!("app.rs");
        let after = src
            .split("\n    fn tick_tmux_bootstrap(&mut self) {")
            .nth(1)
            .expect("找不到 tick_tmux_bootstrap 的定义");
        let body = &after[..after
            .find("\n    }\n")
            .expect("找不到 tick_tmux_bootstrap 的函数结尾")];
        // 先证明切出来的确实是函数体(切歪成空串的话下面几条会空过)。
        assert!(
            body.contains("hosts"),
            "tick_tmux_bootstrap 的函数体切歪了(切出来 {} 字节)",
            body.len()
        );
        assert!(
            body.contains("remote_bootstrap::should_attempt("),
            "tick 没走共享判据"
        );
        assert!(
            body.contains("self.settings.tmux_bootstrap"),
            "tick 没读设置开关 —— 用户关掉了照样发 exec"
        );
        assert!(
            body.contains("remote_bootstrap::bootstrap_command()"),
            "tick 发的不是共享的命令串"
        );
        assert!(
            body.contains(".mark_busy()"),
            "发起前没置 busy —— 上一次还挂在网络上时会再发一次,\
             高延迟链路上叠成一串"
        );
        assert!(
            body.contains(".finish(ok)"),
            "tick 没把**退出码算出来的结论**写回标志 —— 写死 `finish(true)` 的话\
             第一次尝试就 latch 成功,「tmux 服务器还没起」再也配不上"
        );
    }

    /// **接线守护 / F37**:恢复必须先过 `layout_snapshot::usable` 的筛子
    /// (会话已被删 / 树坏了 / active_tab 越界的条目要丢掉)。
    ///
    /// 判据本身有自己的测试;这里守的是「筛子真的接在路上」——绕过它的话,
    /// 删掉一条会话之后启动会摆出一个点了必然失败的占位标签(设计 E6)。
    ///
    /// 自证会变红:把 `restore_tabs` 里那句 `usable(..)` 换成直接用 `saved`。
    #[test]
    fn restoring_a_layout_goes_through_the_usability_filter() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn restore_tabs(")
            .nth(1)
            .expect("找不到 restore_tabs");
        let body = &after[..after.find("\n    }\n").expect("找不到 restore_tabs 的结尾")];
        assert!(
            body.contains("layout_snapshot::usable("),
            "恢复路径绕过了可用性筛子 —— 会摆出点了必然失败的占位标签"
        );
    }

    /// **接线守护 / F37**:存下来的窗口几何必须过 `clamp_to_monitors` 再交给
    /// winit。不夹的话,拔掉副屏之后窗口会开在一块不存在的屏幕上——用户看到
    /// 的是「程序启动了但没有窗口」,而且**没有任何办法**把它弄回来。
    ///
    /// 自证会变红:把 `resumed` 里 `clamp_to_monitors(..)` 的结果换成原样的
    /// `SavedWindow`。
    #[test]
    fn the_restored_window_geometry_is_clamped_to_the_real_monitors() {
        let src = include_str!("app.rs");
        let after = src.split("fn resumed(").nth(1).expect("找不到 resumed");
        assert!(
            after.contains("layout_snapshot::clamp_to_monitors("),
            "启动时没夹紧窗口几何 —— 副屏拔掉后窗口会开在看不见的地方"
        );
    }

    /// `snapshot_tabs_of` 的脚手架:一个占位标签。`leaves` = 上次的分屏数,
    /// 用**左右均分的扁平前序编码**摆出来(跟真实存盘同一套编码)。
    fn restored_tab(session_id: u64, leaves: usize) -> TabContent {
        use mullion_store::SavedNodeEntry;
        let mut tree = Vec::new();
        for _ in 1..leaves {
            tree.push(SavedNodeEntry::split(
                mullion_store::SavedDir::Horizontal,
                0.5,
            ));
        }
        for _ in 0..leaves {
            tree.push(SavedNodeEntry::leaf());
        }
        TabContent::Restored(RestoredTab {
            session_id: SessionId(session_id),
            tree,
            focus_leaf: leaves.saturating_sub(1),
            generation: 0,
            wants_sftp: false,
            dialing: false,
        })
    }

    /// F37:命令行 `user@host` 起的快速连接**不进 layout.toml**——它没有会话
    /// 记录,存下来只会在下次启动时给出一个点了必然失败的「重连」(设计 E2)。
    ///
    /// 自证会变红:把 `snapshot_tabs_of` 里那条 `=> continue` 改成写一个
    /// `SessionId(0)` 进去。
    #[test]
    fn a_quick_connect_tab_is_not_written_to_the_layout_file() {
        let tabs = tabs_with_one_terminal_tab(); // session_id: None
        let (saved, _) = snapshot_tabs_of(&tabs);
        assert!(saved.is_empty(), "快速连接标签被写进了布局:{saved:?}");
    }

    /// F37:跳过快速连接标签之后,活动标签的下标**不能整体错位**。
    ///
    /// 这条是 `active_tab = out.len()` 而不是 `= ix` 的全部理由:用 `ix` 的话,
    /// 前面每跳过一个快速连接标签,下次启动就打开旁边那个标签(或直接越界)。
    ///
    /// 自证会变红:把 `active_tab = out.len()` 改回 `active_tab = ix`。
    #[test]
    fn the_active_index_does_not_drift_after_skipping_a_quick_connect_tab() {
        let mut tabs = tabs_with_one_terminal_tab(); // 下标 0,不会被存
        tabs.open("留下的".into(), Some(SessionId(9)), restored_tab(9, 1));
        assert_eq!(tabs.active_index(), 1, "脚手架前提:活动的是第二个标签");
        let (saved, active) = snapshot_tabs_of(&tabs);
        assert_eq!(saved.len(), 1);
        assert_eq!(
            active, 0,
            "活动下标没跟着「跳过了一个标签」往前收,下次启动会打开错的标签"
        );
    }

    /// F37:这次没重连的占位标签,**原样写回去**。悄悄丢掉的话,用户关一次
    /// 窗口就永久少一个标签——而他什么都没做。
    ///
    /// 自证会变红:把 `TabContent::Restored(r)` 那条分支也改成 `continue`,
    /// 或把 `tree: r.tree.clone()` 换成 `vec![SavedNodeEntry::leaf()]`。
    #[test]
    fn an_untouched_placeholder_tab_survives_another_round_trip() {
        let mut tabs: Tabs<TabContent> = Tabs::default();
        tabs.open("生产机".into(), Some(SessionId(4)), restored_tab(4, 3));
        let (saved, _) = snapshot_tabs_of(&tabs);
        assert_eq!(saved.len(), 1, "占位标签被丢掉了");
        assert_eq!(saved[0].session_id, SessionId(4));
        assert_eq!(
            saved[0].tree.len(),
            5,
            "3 分屏的树应该是 2 个 split + 3 个叶子,存回去的却是 {:?}",
            saved[0].tree
        );
        assert_eq!(saved[0].focus_leaf, 2, "焦点叶子没原样存回去");
    }

    /// `effective_focus_of`/`files_owner_generation_of` 的测试脚手架:一个
    /// 只有一条 Terminal 标签的 `Tabs<TabContent>`。世代号故意用一个非零的
    /// 哨兵值(7)——用 0 的话,一条「压根没读 `ws.generation()`、随手返回
    /// 默认值」的错误实现也能蒙混过去。
    fn tabs_with_one_terminal_tab() -> Tabs<TabContent> {
        let mut tabs = Tabs::default();
        tabs.open(
            "t".into(),
            None,
            TabContent::Terminal(Box::new(TerminalTab {
                ws: Workspace::new(test_pane(1), 7),
                current_preset: None,
                last_cfg: None,
                automation: Vec::new(),
                automation_template: None,
                automation_status: None,
                files: Default::default(),
                sftp: None,
                sftp_tasks: Vec::new(),
                sftp_default_remote: None,
                sftp_home: None,
            })),
        );
        tabs
    }

    /// `effective_focus` 三条规则(协调者修订 2)里的第一条:活动标签是
    /// Terminal 且侧栏关着 → 恒 `Terminal`。
    ///
    /// 故意把 `focus` 参数传成 `FilesPanel`(装作用户按过 F6、意愿是面板)——
    /// 面板压根不在场,这个意愿必须被夹掉,否则键盘会打进一个不可见的面板,
    /// 表现为「按什么键都没反应」。传 `Terminal` 的话,一份"直接原样返回
    /// focus 参数"的错误实现也能蒙混过去。
    #[test]
    fn effective_focus_of_terminal_tab_without_sidebar_is_always_terminal() {
        use crate::shell::input_route::Focus;
        let tabs = tabs_with_one_terminal_tab();
        assert_eq!(
            effective_focus_of(&tabs, false, Focus::FilesPanel),
            Focus::Terminal
        );
    }

    /// 第二条规则:活动标签是 Terminal 且侧栏开着 → 用 `self.focus`(用户的
    /// F6 意愿生效)。两个方向都断言——只测一个方向的话,一份「恒返回
    /// Terminal」或「恒返回 FilesPanel」的错误实现有一半概率蒙混过去。
    #[test]
    fn effective_focus_of_terminal_tab_with_sidebar_open_follows_user_choice() {
        use crate::shell::input_route::Focus;
        let tabs = tabs_with_one_terminal_tab();
        assert_eq!(
            effective_focus_of(&tabs, true, Focus::Terminal),
            Focus::Terminal
        );
        assert_eq!(
            effective_focus_of(&tabs, true, Focus::FilesPanel),
            Focus::FilesPanel
        );
    }

    /// 第三条规则:活动标签是 Files → 恒 `FilesPanel`。构造不出真实的
    /// `FilesTab`(`FilesTab::conn` 是 `Arc<SshConnection>`,`SshConnection::new`
    /// 对 `mullion-app` 不可见——同 `wind_down_has_no_catch_all_arm_...` 那条
    /// 测试面对的限制),只能扎源码结构。
    ///
    /// 自证会变红:把 `active_is_files_tab_of(tabs)` 为真时返回的
    /// `Focus::FilesPanel` 改成 `Focus::Terminal`。
    #[test]
    fn effective_focus_treats_a_files_tab_as_always_focused_on_the_panel() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn effective_focus_of(")
            .nth(1)
            .expect("找不到 effective_focus_of 的定义");
        let body = &after[..after
            .find("\n}\n")
            .expect("找不到 effective_focus_of 的函数结尾")];
        let files_branch = body
            .split("if active_is_files_tab_of(tabs) {")
            .nth(1)
            .expect("找不到 active_is_files_tab_of 分支体");
        let files_branch_body =
            &files_branch[..files_branch.find("} else").unwrap_or(files_branch.len())];
        assert!(
            files_branch_body.contains("Focus::FilesPanel"),
            "effective_focus_of 在活动标签是 Files 时必须恒返回 \
             Focus::FilesPanel —— 那种标签没有终端可回,按 Terminal 路由的话\
             方向键/回车会去找一个不存在的 pane,静默无反应"
        );
    }

    /// `UiActions` 加了字段却漏改 `has_real_action` 的话,新动作会在 egui 的
    /// discard 趟被静默吃掉 —— 症状是「点了确认,什么也没发生,也不报错」。
    ///
    /// 按大括号配平截出一条 `match` 分支。`rest` 必须**从 `=> {` 起算**
    /// ——从 arm 的模式起算的话,模式自带的那对花括号(`{ job, done }`)
    /// 会让深度在第一步就归零,截出来的「arm」一行代码都不含。
    ///
    /// 截不到闭合大括号时返回整串,调用方据此断言(`arm.len() < rest.len()`)
    /// 自己没有退化成扫全文件。
    fn brace_balanced_arm(rest: &str) -> &str {
        let mut depth = 0usize;
        for (i, c) in rest.char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return &rest[..i + 1];
                    }
                }
                _ => {}
            }
        }
        rest
    }

    /// 取 `match event` 里某条分支的**块体**。`pattern` 只给模式部分,
    /// 这里自己接上 `" => {"` 再找 —— 光按模式找会命中同一个变体在别处的
    /// **构造**处(`send_event(UserEvent::TransferProgress { job, done })`),
    /// 截出来的是那一段代码,断言全部落空。
    fn arm_of<'a>(production: &'a str, pattern: &str) -> &'a str {
        let needle = format!("{pattern} => {{");
        let at = production
            .find(&needle)
            .unwrap_or_else(|| panic!("找不到 {pattern} 的处理分支"));
        let rest = &production[at + production[at..].find("=> {").expect("arm 没有块体")..];
        let arm = brace_balanced_arm(rest);
        assert!(
            arm.len() < rest.len(),
            "{pattern} 没截到闭合大括号,断言会退化成扫全文件"
        );
        arm
    }

    /// **T3 守护**:进度事件是高频的(一个 100MB 的文件几千条),那条 arm 里
    /// 一旦出现 `ui_dirty` / `request_redraw`,就变成每秒几千帧、风扇起飞 ——
    /// 正是 T3 点名的那条红线。进度显示该由帧闸驱动,不由事件驱动。
    ///
    /// 结构守护(`user_event` 要 `&mut App`,无头造不出来)。
    /// 自证会变红:在那条 arm 里加一句 `self.ui_dirty = true;`。
    #[test]
    fn transfer_progress_events_never_request_a_redraw_so_the_fan_stays_quiet() {
        let src = include_str!("app.rs");
        let (production, _) = src
            .split_once("#[cfg(test)]")
            .expect("找不到 #[cfg(test)] 边界");
        let arm = arm_of(production, "UserEvent::TransferProgress { job, done }");
        assert!(
            arm.contains("progress(job, done)"),
            "arm 切歪了(没更新队列进度),下面那条否定断言会空过:{arm}"
        );
        assert!(
            !arm.contains("ui_dirty") && !arm.contains("request_redraw"),
            "进度事件里出现了重绘(T3):{arm}"
        );
    }

    /// 传完必须刷新**目标那一栏**。不刷的症状:文件其实传到了,但列表里
    /// 看不见,用户以为没成、再传一次。
    ///
    /// 自证会变红:删掉那条 arm 里的 `dispatch_panel_action_for(...)`。
    #[test]
    fn a_finished_transfer_refreshes_the_destination_column_so_the_file_shows_up() {
        let src = include_str!("app.rs");
        let (production, _) = src
            .split_once("#[cfg(test)]")
            .expect("找不到 #[cfg(test)] 边界");
        let arm = arm_of(production, "UserEvent::TransferDone { job, result }");
        assert!(
            arm.contains("FileAction::Refresh"),
            "传完不刷新,新文件不会出现在列表里:{arm}"
        );
        assert!(
            arm.contains("PanelColumn::Local") && arm.contains("PanelColumn::Remote"),
            "刷新写死成了一栏 —— 下载该刷本地、上传该刷远端:{arm}"
        );
    }

    /// F56:每条传输**自己开一条 sftp channel**。共用标签那条
    /// (`tab.content.sftp_client()`)的话,请求在同一个 session 上串行,
    /// 并发度实际等于 1,设计 D8 说的吞吐问题原样还在 —— 而且症状是「设了 4
    /// 并发但一点不快」,没人会怀疑到这里。
    ///
    /// 自证会变红:把 `run_transfer` 里的 `SftpClient::open(conn)` 换成从
    /// 外面传进来的 client。
    #[test]
    fn every_transfer_opens_its_own_channel_so_concurrency_is_real() {
        let src = include_str!("app.rs");
        let (production, _) = src
            .split_once("#[cfg(test)]")
            .expect("找不到 #[cfg(test)] 边界");
        let after = production
            .split("async fn run_transfer(")
            .nth(1)
            .expect("找不到 run_transfer 的定义");
        let body = &after[..after.find("\n}\n").expect("找不到 run_transfer 的函数结尾")];
        assert!(
            body.contains("spec.dir"),
            "run_transfer 的函数体切歪了({} 字节),下面那条断言会空过",
            body.len()
        );
        assert!(
            body.contains("SftpClient::open(conn)"),
            "worker 没有自己开 channel —— 并发度会静默退化成 1(F56/设计 D8)"
        );
    }

    /// 传输面板上的按钮**必须算「真动作」**:`has_real_action` 是手写枚举,
    /// 漏一个字段,egui 的丢弃趟就会把这一帧的点击悄悄吃掉(取消传输点了
    /// 没反应,而且时灵时不灵)。
    ///
    /// 自证会变红:把 `has_real_action` 里的 `a.transfer.is_some()` 删掉。
    #[test]
    fn a_transfer_ui_action_counts_as_a_real_action_so_it_is_not_swallowed() {
        use crate::ui::UiActions;
        let a = UiActions {
            transfer: Some(crate::ui::transfer_panel::TransferUiAction::CancelAll),
            ..Default::default()
        };
        assert!(has_real_action(&a), "取消传输被 egui 的丢弃趟吞了");
    }

    /// D16:落点名在 Windows 上非法时**整条拒掉并给建议名**,不静默改写。
    /// 静默改写的后果是用户以为下下来的是 `aux.log`,实际叫别的名字,
    /// 下次照着原名找不到、脚本也对不上。
    #[test]
    fn a_name_that_windows_cannot_store_stops_the_job_with_a_suggestion() {
        let remote = mullion_ssh::sftp::RemotePath::from_bytes(b"/srv/aux".to_vec());
        let local = mullion_ssh::sftp::RemotePath::from_bytes(b"/tmp".to_vec());
        let err = download_job(&remote, &[], &local, 1).expect_err("aux 是保留设备名,该拒掉");
        assert!(err.contains("aux"), "错误里得点名是哪个文件:{err}");
        assert!(err.contains("_aux"), "错误里得给建议名:{err}");
    }

    /// 递归下载要把远端的子树原样落到本地目录下面 —— 拼错的话所有文件会
    /// 挤在同一层(或者跑到目标目录外面去)。
    #[test]
    fn a_download_job_mirrors_the_remote_subtree_under_the_local_directory() {
        let remote = mullion_ssh::sftp::RemotePath::from_bytes(b"/srv/data".to_vec());
        let local = mullion_ssh::sftp::RemotePath::from_bytes(b"/tmp/dl".to_vec());
        let j = download_job(&remote, &[b"sub".to_vec(), b"a.txt".to_vec()], &local, 7)
            .expect("名字合法");
        assert_eq!(j.remote.display(), "/srv/data/sub/a.txt");
        assert_eq!(
            j.local,
            std::path::Path::new("/tmp/dl")
                .join("data")
                .join("sub")
                .join("a.txt")
        );
        assert_eq!(j.total, 7);
        assert_eq!(j.label, "a.txt");
    }

    /// 递归上传的镜像方向。远端一律用 `/`,不能跟着本机分隔符走。
    #[test]
    fn an_upload_job_mirrors_the_local_subtree_under_the_remote_directory() {
        let root = std::path::Path::new("/home/u/data");
        let remote_dir = mullion_ssh::sftp::RemotePath::from_bytes(b"/srv".to_vec());
        let name = mullion_ssh::sftp::RemotePath::from_bytes(b"data".to_vec());
        let j = upload_job(root, &["sub".into(), "a.txt".into()], &remote_dir, &name, 9);
        assert_eq!(j.remote.display(), "/srv/data/sub/a.txt");
        assert_eq!(j.local, root.join("sub").join("a.txt"));
        assert_eq!(j.label, "a.txt");
    }

    /// 破坏性验证:把 `has_real_action` 里的 `a.files_op.is_some()` 删掉,
    /// 这条必须变红。
    #[test]
    fn a_confirmed_file_operation_counts_as_a_real_action() {
        use crate::ui::files_dialog::FileOp;
        use crate::ui::UiActions;

        let mut a = UiActions::default();
        assert!(!has_real_action(&a), "全空时不该算有动作");

        a.files_op = Some(FileOp::NewDir(mullion_ssh::sftp::RemotePath::from_bytes(
            b"/x".to_vec(),
        )));
        assert!(has_real_action(&a), "files_op 没被算进 has_real_action");
    }

    /// 写操作完成后**必须刷新**。不刷新的症状是:删了一个文件,列表里
    /// 那一行还在,用户以为没生效再删一次,收到一条 NoSuchFile。
    ///
    /// 这是结构守护(`user_event` 要 `&mut App`,无头造不出来):断言
    /// `SftpOpDone` 的处理分支里确实发了一次 `Refresh`。
    #[test]
    fn a_successful_write_triggers_a_refresh_so_the_list_is_not_stale() {
        let src = include_str!("app.rs");
        // 只看生产代码那一半 —— 断言字符串写在本测试里,连自己这行都算
        // 命中的话就是一条自证自伪的假测试(同 `files_panel` 里那条)。
        let (production, _) = src
            .split_once("#[cfg(test)]")
            .expect("找不到 #[cfg(test)] 边界");
        // 定位到**处理分支**(`match event` 里那个),不是枚举定义处 ——
        // 后者在前面,取到它的话后面截出来的一段根本不含处理代码。
        let at = production
            .find("UserEvent::SftpOpDone { generation, result } => {")
            .expect("找不到 SftpOpDone 的处理分支");
        // **从 `=> {` 起算**,不是从 `UserEvent::` 起算:模式里那对
        // `{ generation, result }` 花括号会让配平在第一步就归零,截出来的
        // 「arm」只有模式本身、一行代码都不含(断言于是恒红)。
        let rest = &production[at + production[at..].find("=> {").expect("arm 没有块体")..];
        // 按大括号配平截出这一条 arm。**不能拿「下一个 `UserEvent::`」当边界**:
        // 这是 `match` 的最后一条分支,那样截会一路截到文件末尾,把别处
        // (F5 那条)的 `FileAction::Refresh` 也算进来 —— 断言于是恒绿
        // (删掉整段刷新代码它照样过,变异验收当场逮到)。
        let mut depth = 0usize;
        let mut end = rest.len();
        for (i, c) in rest.char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        let arm = &rest[..end];
        assert!(
            arm.len() < rest.len(),
            "没截到 arm 的闭合大括号,下面的断言会退化成扫全文件"
        );
        assert!(
            arm.contains("FileAction::Refresh"),
            "写操作成功后没有刷新目录 —— 界面会一直显示已经删掉的那一行"
        );
    }

    /// 设计 D5 最要命的一条:焦点在**本地栏**时按 `Delete`,绝不能去删远端
    /// 文件。转投远端栏是一个看着「体贴」、后果不可逆的实现。
    ///
    /// 结构守护(`handle_panel_key` 要 `&mut App`,无头造不出来):断言那一段
    /// 里确实有「不是远端栏就 return」这道闸。
    #[test]
    fn delete_and_rename_keys_do_nothing_while_the_local_column_has_focus() {
        let src = include_str!("app.rs");
        let (production, _) = src
            .split_once("#[cfg(test)]")
            .expect("找不到 #[cfg(test)] 边界");
        let at = production
            .find("WinitKey::Named(NamedKey::Delete) | WinitKey::Named(NamedKey::F2) => {")
            .expect("找不到 Delete/F2 键的处理分支");
        // 按大括号配平截出这一条 arm —— 从 `=> {` 起算,理由同
        // `a_successful_write_triggers_a_refresh_so_the_list_is_not_stale`。
        let rest = &production[at + production[at..].find("=> {").expect("arm 没有块体")..];
        let mut depth = 0usize;
        let mut end = rest.len();
        for (i, c) in rest.char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        let arm = &rest[..end];
        assert!(
            arm.len() < rest.len(),
            "没截到 arm 的闭合大括号,下面的断言会退化成扫全文件"
        );
        assert!(
            arm.contains("!= Some(crate::ui::files_panel::PanelColumn::Remote)")
                && arm.contains("return;"),
            "Delete/F2 的处理里没有「焦点不在远端栏就不动」这道闸 —— \
             用户看着本地栏按 Delete 会删掉远端文件"
        );
    }

    /// 修订 1:`files_owner_generation` 侧栏关着时不该有属主——面板这一刻
    /// 根本不可见。
    #[test]
    fn files_owner_generation_of_terminal_tab_without_sidebar_is_none() {
        let tabs = tabs_with_one_terminal_tab();
        assert_eq!(files_owner_generation_of(&tabs, false), None);
    }

    /// 修订 1 的反面:侧栏开着时属主是活动标签自己的世代号,不是随便一个值。
    #[test]
    fn files_owner_generation_of_terminal_tab_with_sidebar_open_is_the_workspace_generation() {
        let tabs = tabs_with_one_terminal_tab();
        assert_eq!(files_owner_generation_of(&tabs, true), Some(7));
    }

    /// 前置 A 的**另一半**:`files_local` 与 `files_remote` 是同构的缺口,
    /// 上一条只钉死了远端那半。复核实测:删掉 `has_real_action` 里
    /// `a.files_local.is_some()` 那一行,补测之前全仓库照样全绿——因为没有
    /// 任何测试构造过「`files_local` 是这一帧唯一真实动作」的 `UiActions`。
    #[test]
    fn files_local_alone_counts_as_a_real_action_for_the_discard_guard() {
        let a = crate::ui::UiActions {
            files_local: Some(crate::ui::files_panel::FileAction::Refresh),
            ..Default::default()
        };
        assert!(
            has_real_action(&a),
            "files_local 单独一个真实动作时必须被 has_real_action 认成\
             「有真实动作」,否则本地栏的点击会在 discard 趟被静默吃掉"
        );
    }

    /// 陪跑:全默认的 `UiActions`(什么都没点)不该被判成「有真实动作」——
    /// 否则上面两条测试就是靠 `has_real_action` 恒真通过的,没有测到东西。
    #[test]
    fn no_actions_is_not_a_real_action() {
        assert!(!has_real_action(&crate::ui::UiActions::default()));
    }

    /// **接线守护**:`pump_io` 必须驱动 `drive_automation`。
    ///
    /// 首字节与断线两条边挂在 `pump_io` 上而不是重绘上,是刻意的:最小化期间
    /// 窗口未必还会被重绘,但 `Wake` 仍会驱动 `pump_io` —— 否则用户最小化着
    /// 连上,自动化会一直等到超时。这条调用一旦在重构 `pump_io` 时被漏掉,
    /// 自动化就永远等不到就绪,而且没有任何报错。
    ///
    /// **扎的是源码结构而非运行时行为**:`App` 要 `EventLoopProxy` 才能构造,
    /// 单测里造不出来。这里只钉「两个函数之间的接线」——`drive_automation`
    /// 函数体内部的逻辑不在这条测试的射程内(那样断言等于把代码抄一遍)。
    ///
    /// 自证会变红:把 `pump_io` 里的 `self.drive_automation();` 删掉。
    #[test]
    fn pump_io_drives_automation() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn pump_io(&mut self) {")
            .nth(1)
            .expect("找不到 pump_io 的定义");
        let body = &after[..after.find("\n    }\n").expect("找不到 pump_io 的函数结尾")];
        assert!(
            body.contains("self.drive_automation()"),
            "pump_io 没有驱动 drive_automation —— 首字节/断线两条边就断了,\
             用户最小化着连上时自动化会一直等到超时,且不会有任何报错"
        );
    }

    /// **接线守护**:每块新开的 pane 都要起自己那一份自动化。
    ///
    /// 用户报的问题就是这条边不存在:菜单栏点分屏,新 pane 是干净 shell,
    /// 节点配的 `cd` / `export` / 启动命令一条都没跑。挂在 `PaneOpened` 上而
    /// 不是发起分屏时,是因为只有 channel 真的开好、`attach_pane` 真的挂上去了,
    /// 才有东西可写(高延迟链路上这中间可能过去好几秒,期间用户还可能切走)。
    ///
    /// **扎的是源码结构而非运行时行为**:`App` 要 `EventLoopProxy` 才能构造。
    /// 这条只钉「分支里调了 `pending_for_extra_pane`」;跳不跳 tmux、总开关认不认
    /// 由 `automation::tests` 那几条真单测把守。
    ///
    /// 自证会变红:把 `PaneOpened` 分支末尾那段 `if let Some(sink) = attached`
    /// 整个删掉。
    #[test]
    fn a_freshly_opened_pane_starts_its_own_automation() {
        let src = include_str!("app.rs");
        // 锚点**拆开拼**:写成完整字面量的话,这条测试自己的源码里也有一份,
        // `rsplit` 会切到测试本身 —— 一条永远绿的测试(本项目踩过的第四类
        // 恒绿模式:源码级字符串测试自我匹配)。
        let start = concat!("UserEvent::Pane", "Opened {");
        let stop = concat!("UserEvent::Pane", "OpenErr {");
        let after = src.rsplit(start).next().expect("找不到 PaneOpened 分支");
        let raw = &after[..after.find(stop).expect("找不到 PaneOpened 分支的结尾")];
        // **注释行必须剥掉**。第一版没剥,而这段代码上方恰好有一句解释性注释
        // 也提到了 `pending_for_extra_pane` —— 把调用整个删掉,测试照样绿。
        // 源码级断言只能扎在代码上:注释想写什么写什么,不该有资格让测试通过。
        let body: String = raw
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        // 前提:切出来的确实是那条 match 分支,不是别处的 send_event。
        // 切错了片段的话下面那条断言测的就不是想测的东西。
        assert!(
            body.contains("pane_still_wanted"),
            "切错片段了:这段不像 PaneOpened 的 match 分支"
        );
        assert!(
            body.contains("pending_for_extra_pane"),
            "PaneOpened 没有为新 pane 起自动化 —— 分屏出来的 pane 会是干净 shell,\
             节点配的 cd/export/启动命令一条都不跑,且没有任何报错"
        );
    }

    /// **接线守护**:换节点挂失败(pane 在拨号途中没了)时,刚 push 进
    /// `ws.hosts` 的那条必须撤掉。
    ///
    /// 漏了的话现象**完全静默**:`hosts` 里留一条谁的 `host_ix` 都不指向的
    /// 连接,`Arc<SshConnection>` 攥着不放 —— 远端那条 SSH 会话一直开着,
    /// 直到整个标签关闭。没有任何报错,也不影响画面。
    ///
    /// 扎源码而非行为:`rehost_pane` 之外的这段在 `App::user_event` 里,
    /// 而 `App` 要 `EventLoopProxy` 才能构造,单测里造不出来。
    ///
    /// 自证会变红:把那个 `else` 分支里的 `ws.hosts.pop();` 删掉。
    #[test]
    fn a_failed_rehost_takes_its_host_back_out_of_the_list() {
        let src = include_str!("app.rs");
        // 锚点拆开拼,理由同上一条(第四类恒绿模式:源码级测试自我匹配)。
        let start = concat!("UserEvent::Pane", "Rehosted {");
        let stop = concat!("UserEvent::Pane", "RehostErr {");
        let after = src.rsplit(start).next().expect("找不到 PaneRehosted 分支");
        let raw = &after[..after.find(stop).expect("找不到 PaneRehosted 分支的结尾")];
        // 注释行剥掉:这段代码里就有一句注释解释了为什么要 pop,不剥的话
        // 删掉 pop 本身测试照绿(踩过一次)。
        let body: String = raw
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            body.contains("rehost_pane("),
            "切错片段了:这段不像 PaneRehosted 的 match 分支"
        );
        assert!(
            body.contains("hosts.pop()"),
            "换节点没挂成时没把刚 push 的 HostConn 撤掉 —— 一条谁也不指向的 SSH \
             连接会一直开到标签关闭,而且完全静默"
        );
    }

    /// **接线守护**:用户意图写入点的数量。
    ///
    /// `user_took_over` 的文档说「当前的几处以 grep 为准」——这条测试把那句话
    /// 变成可执行断言。滚轮的两处(`Report`/`ArrowKeys`)共用分支末尾一次调用;
    /// 输入法(F21)把总数从三处推到五处:`Ime::Commit` 与合成文本(死键 /
    /// 部分布局塞进 `Character` 的多字符)各是一条独立的用户输入路径。
    ///
    /// 挡两个方向:
    /// - **少了**:重构时删掉一处,自动化在用户已经开始打字后仍继续发命令,
    ///   两股输入交织,用户看到的是自己的字被打散;
    /// - **多了**:新增了用户输入路径(如鼠标按钮上报 F15)却没人回头看这条
    ///   不变量。数字对不上时请连同 `user_took_over` 的文档一起更新。
    ///
    /// 注意它**不**保证挂对了地方:`Workspace::pump` 那处 T1 应答不能挂,由
    /// `pty_write_echo_does_not_cancel_automation` 单独把守。
    ///
    /// 自证会变红:删掉产品代码里任意一处取消调用。
    #[test]
    fn user_intent_write_points_all_yield_to_the_user() {
        let src = include_str!("app.rs");
        // 只数产品代码:测试模块里(包括本测试的注释)也会出现同样的字面量。
        let prod = src
            .split("\n#[cfg(test)]")
            .next()
            .expect("split 至少有一段");
        let calls = prod.matches("self.user_took_over();").count();
        assert_eq!(
            calls, 5,
            "用户意图写入点的取消调用应为 5 处(粘贴/滚轮/键盘/输入法提交/\
             合成文本,滚轮两个分支共用一次),实际 {calls} 处 —— 少了会让自动化\
             在用户打字时继续发命令,多了说明新增了输入路径但没复核这条不变量"
        );
    }

    /// **接线守护 / T8**:标签快捷键必须在输入分流**之前**被截走。
    ///
    /// 两条失效模式各挡一半,都得靠「在分流之前」这一个位置:
    /// 1. **别喂 egui**(T8):`Ctrl+Tab` 里带着 Tab,egui 的焦点系统在
    ///    `begin_pass` 里扫原始事件,看到 Tab 就把焦点给菜单栏第一个按钮,此后
    ///    `wants_keyboard_input()` 恒 true,终端**永久**收不到任何键。
    /// 2. **别编码进 PTY**:下面那个 `KeyboardInput` 分支会 `encode_key` 往
    ///    channel 写,`Ctrl+W` 就会在切标签的同时把远端 shell 的前一个词删掉。
    ///
    /// 判定本身(含模态闸门)在 `shell::tabs::hotkey`,有真行为测试;这里扎的是
    /// **调用位置**,只有源码结构能表达 —— `App` 要 `EventLoopProxy` 才能构造。
    /// 验证边界:只挡得住「调用点跑到分流之后 / 整个没调」,挡不住有人在
    /// `tab_hotkey_event` 里返回恒 false。
    ///
    /// 自证会变红:把 `window_event` 开头那句 `if self.tab_hotkey_event(&event)`
    /// 整段删掉,或挪到 `egui_should_see` 那段之后。
    #[test]
    fn tab_shortcuts_are_swallowed_before_the_input_routing() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn window_event(")
            .nth(1)
            .expect("找不到 window_event 的定义");
        let hotkey = after
            .find("self.tab_hotkey_event(&event)")
            .expect("window_event 里没调 tab_hotkey_event —— 标签快捷键会被喂给 egui(Tab 抢焦点,T8),还会被编码进 PTY(Ctrl+W 删远端的词)");
        let routing = after.find("egui_should_see").expect("找不到输入分流那一段");
        assert!(
            hotkey < routing,
            "tab_hotkey_event 排在了输入分流之后 —— 排在后面等于没截:\
             Ctrl+Tab 里的 Tab 已经被喂给 egui 的焦点系统了"
        );
    }

    /// **接线守护 / T8**:文件侧栏快捷键(`Ctrl+Shift+B`)必须同样在输入分流
    /// **之前**被截走。与 `tab_shortcuts_are_swallowed_before_the_input_routing`
    /// 同构,理由同上:不截的话,`Ctrl+Shift+B` 里的 `B` 会先被喂给 egui 的
    /// 焦点系统,也会被 `KeyboardInput` 分支编码进 PTY,写给远端一个字母。
    ///
    /// 验证边界同 `tab_hotkey_event` 那条:只挡得住「调用点跑到分流之后 /
    /// 整个没调」,挡不住有人在 `files_hotkey_event` 里返回恒 false。
    ///
    /// 自证会变红:把 `window_event` 里那句 `if self.files_hotkey_event(&event)`
    /// 整段删掉,或挪到 `egui_should_see` 那段之后。
    #[test]
    fn files_shortcut_is_swallowed_before_the_input_routing() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn window_event(")
            .nth(1)
            .expect("找不到 window_event 的定义");
        let hotkey = after
            .find("self.files_hotkey_event(&event)")
            .expect("window_event 里没调 files_hotkey_event —— Ctrl+Shift+B 会被喂给 egui(T8),还会被编码进 PTY 写给远端");
        let routing = after.find("egui_should_see").expect("找不到输入分流那一段");
        assert!(
            hotkey < routing,
            "files_hotkey_event 排在了输入分流之后 —— 排在后面等于没截:\
             Ctrl+Shift+B 里的 B 已经被喂给 egui 的焦点系统了"
        );
    }

    /// **接线守护 / T8**:F6 换焦点同样必须在输入分流**之前**被截走。与前两条
    /// 同构——不截的话,F6 会先被喂给 egui 的焦点系统(虽然 F6 本身不是 Tab,
    /// 但一旦这套判定被误挪到分流之后,连带一起挪错的风险跟前两条一样大)。
    ///
    /// 验证边界同前两条:只挡得住「调用点跑到分流之后 / 整个没调」,挡不住
    /// 有人在 `focus_hotkey_event` 里返回恒 false。
    ///
    /// 自证会变红:把 `window_event` 里那句 `if self.focus_hotkey_event(&event)`
    /// 整段删掉,或挪到 `egui_should_see` 那段之后。
    #[test]
    fn focus_shortcut_is_swallowed_before_the_input_routing() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn window_event(")
            .nth(1)
            .expect("找不到 window_event 的定义");
        let hotkey = after
            .find("self.focus_hotkey_event(&event)")
            .expect("window_event 里没调 focus_hotkey_event —— F6 会被喂给 egui(T8),还会被编码进 PTY 写给远端");
        let routing = after.find("egui_should_see").expect("找不到输入分流那一段");
        assert!(
            hotkey < routing,
            "focus_hotkey_event 排在了输入分流之后 —— 排在后面等于没截"
        );
    }

    /// 协调者修订 1:F6 的生效条件是「面板此刻在场」——判据必须复用
    /// `files_owner_generation`(与 `Present` 分支判断要不要画侧栏共用同一份
    /// 判据),不能自己另写一份等价逻辑。理由:面板不在场时若仍吞掉 F6,
    /// 用户在纯终端场景按 F6(某些远端 TUI/工具会用到功能键)会被无声吃掉,
    /// 表现为「这个键突然没反应」——恰是本项目最忌的静默失效。
    ///
    /// **扎的是源码结构**——`focus_hotkey_event` 是 `App` 的私有方法,`App`
    /// 单测里造不出来(`EventLoopProxy`)。验证边界:只挡得住「函数体里压根
    /// 没调 `files_owner_generation()`」这一种退化,挡不住换个不共用判据的
    /// 等价写法(比如自己重新拼一遍 `active_is_files_tab() || files_sidebar_open`)。
    ///
    /// 自证会变红:把 `focus_hotkey_event` 里
    /// `if self.files_owner_generation().is_none() { return false; }` 那两行删掉。
    #[test]
    fn f6_is_gated_on_the_panel_actually_being_visible() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn focus_hotkey_event(")
            .nth(1)
            .expect("找不到 focus_hotkey_event 的定义");
        let body = &after[..after
            .find("\n    }\n")
            .expect("找不到 focus_hotkey_event 的函数结尾")];
        assert!(
            body.contains("self.files_owner_generation().is_none()"),
            "focus_hotkey_event 没有拿 files_owner_generation 判断面板在不在场 \
             —— F6 会在没有面板的纯终端场景里被无声吞掉"
        );
    }

    /// 协调者修订 2 的**接入点**守护:`window_event` 里喂给分流的 `focus`
    /// 必须是夹紧过的 `effective_focus()`,不能是裸的 `self.focus`。
    ///
    /// `self.focus` 只记「用户按 F6 表达的意愿」,兑不兑现要看上下文。裸着用
    /// 的症状有两个方向,都是静默的:
    /// - Files 标签(那里没有终端)焦点仍标成 `Terminal` → 方向键/回车全落到
    ///   一个不存在的 pane 上,用户以为键盘死了,只能改用鼠标;
    /// - 侧栏关掉后焦点还留在 `FilesPanel` → 键送给一个看不见的面板。
    ///
    /// `effective_focus_of` 那三条夹紧规则**各自都有行为测试钉着**,但「分流
    /// 处到底调没调它」这一环没人管:实测把这行换成 `let focus = self.focus;`,
    /// 全 709 条测试一条都不红。这条补的就是这一环。
    ///
    /// **扎的是源码结构**(`window_event` 要真 `App` + `EventLoopProxy` 才能跑)。
    /// 验证边界:只挡得住「换回裸字段 / 整个不调」,挡不住有人把夹紧逻辑
    /// 从 `effective_focus_of` 内部掏空——那一侧由上述三条行为测试守。
    ///
    /// 自证会变红:把 `window_event` 里 `let focus = self.effective_focus();`
    /// 换成 `let focus = self.focus;`。
    #[test]
    fn the_input_routing_uses_the_clamped_focus_not_the_raw_field() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn window_event(")
            .nth(1)
            .expect("找不到 window_event 的定义");
        // 只看到分流那一段为止:`self.focus` 在别处(比如 F6 的 toggled 赋值)
        // 出现是合法的,把整个函数体一起扫会误判。
        let routing = after.find("egui_should_see").expect("找不到输入分流那一段");
        let before_routing = &after[..routing];
        assert!(
            before_routing.contains("let focus = self.effective_focus();"),
            "分流之前没有算夹紧后的焦点 —— 见本测试文档注释里那两种静默症状"
        );
        assert!(
            !before_routing.contains("let focus = self.focus;"),
            "分流拿的是裸 self.focus,没经过 effective_focus 的上下文夹紧"
        );
    }

    /// 文件侧栏的快捷键必须**带 Shift**。
    ///
    /// 少了 Shift 就成了 `Ctrl+B`,而那是 tmux 出厂默认的 prefix 键 ——
    /// 本项目的核心场景恰恰是「操作跑在远端 tmux 里的 Claude Code」,
    /// 抢掉 `Ctrl+B` 等于让用户在自己的 tmux 里寸步难行,而且症状极隐蔽:
    /// 按 `Ctrl+B` 弹出个文件面板,用户只会以为 tmux 坏了。
    /// `Ctrl+Shift+*` 系在终端里不产生控制字符,才是安全的取键区间。
    ///
    /// **扎的是源码结构而非运行时行为**(`App` 要 `EventLoopProxy` 才能构造)。
    /// 验证边界:只挡得住「`mods.shift` 那个判断被删掉」这一种写法。
    ///
    /// 自证会变红:把 `files_hotkey_event` 里的 `!mods.shift` 去掉。
    #[test]
    fn the_files_shortcut_requires_shift_so_it_cannot_steal_tmux_prefix() {
        let src = include_str!("app.rs");
        let body = src
            .split("fn files_hotkey_event")
            .nth(1)
            .expect("找不到 files_hotkey_event");
        let body = &body[..body
            .find("\n    }\n")
            .expect("找不到 files_hotkey_event 的函数结尾")];
        assert!(
            body.contains("!mods.shift"),
            "files_hotkey_event 不再要求 Shift —— Ctrl+B 是 tmux 的 prefix 键,\
             抢了它用户在远端 tmux 里寸步难行"
        );
    }

    /// **T4 / S3**:标签栏吃掉的那点高度必须一路走到 `window_change`。
    ///
    /// 标签栏是 F36 引入的第三条常驻横条,中央区因此矮了 `tab_bar_px()` 逻辑点。
    /// 这条链是 `central_px` → `layout_geometry` → `apply_geometry` →
    /// `pty.resize`。**断在任何一环的现象都一样**:tmux 里的 TUI 按旧行数排版,
    /// 全屏应用最后一行被标签栏盖住、或者整屏错位(T4)。
    ///
    /// 这里跑的是真 `Workspace` + 记录型 PTY,不是源码结构守护。
    ///
    /// 自证会变红:把 `tab_bar_px()` 改成返回 0.0(中央区不变矮 → 行数不变 →
    /// 不发 window_change)。
    #[test]
    fn tab_bar_height_reaches_the_remote_as_a_window_change() {
        use crate::shell::workspace::{layout_geometry, PtyWriter, PxRect};
        use std::sync::{Arc, Mutex};

        #[derive(Default)]
        struct RecordingPty(Arc<Mutex<Vec<(u16, u16)>>>);
        impl PtyWriter for RecordingPty {
            fn write(&self, _bytes: Vec<u8>) -> Result<(), mullion_ssh::session::TrySendErr> {
                Ok(())
            }
            fn resize(&self, cols: u16, rows: u16) -> Result<(), mullion_ssh::session::TrySendErr> {
                self.0.lock().unwrap().push((cols, rows));
                Ok(())
            }
        }

        let seen = Arc::new(Mutex::new(Vec::new()));
        let (_tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
        let mut ws = Workspace::new(
            crate::shell::workspace::PaneState {
                id: PaneId(1),
                host_ix: 0,
                emulator: mullion_term::emulator::Emulator::new(80, 24),
                pty: Box::new(RecordingPty(seen.clone())),
                rx,
                pacer: crate::render::SyncFramePacer::new(),
                status: crate::shell::workspace::PaneStatus::Live,
                saw_first_byte: false,
                // 不可能的初值:第一次 apply_geometry 必然发一次,基线由它建立。
                last_grid: (0, 0),
                cwd: None,
                tmux: None,
            },
            0,
        );
        let cell = (10.0f32, 20.0f32);
        // ppp = 1,所以逻辑点即像素。
        let full = PxRect {
            x: 0,
            y: 0,
            w: 1600,
            h: 900,
        };
        let shrunk = PxRect {
            h: full.h - crate::ui::chrome::tab_bar_px() as u32,
            ..full
        };
        let tree = ws.tree().clone();

        ws.apply_geometry(&layout_geometry(&tree, full, cell, false, 1.0));
        let before = *seen.lock().unwrap().last().expect("第一次必然发一次");
        seen.lock().unwrap().clear();

        ws.apply_geometry(&layout_geometry(&tree, shrunk, cell, false, 1.0));
        let after = seen.lock().unwrap().clone();
        assert_eq!(
            after.len(),
            1,
            "标签栏出现后中央区矮了 {} 点,必须恰好发一次 window_change(发 0 次 = \
             远端仍按旧行数排版,最后一行被标签栏盖住;发多次 = 每帧都在发)",
            crate::ui::chrome::tab_bar_px()
        );
        assert!(
            after[0].1 < before.1,
            "标签栏占了高度,新行数 {} 却没比原来的 {} 少",
            after[0].1,
            before.1
        );
    }

    /// **T4 / 设计 D2 的后半段**:中央区被文件侧栏挤窄之后,必须恰好发一次
    /// `window_change`,且列数变少。
    ///
    /// 这条守的是「变窄之后」那一段(`layout_geometry` → `apply_geometry` →
    /// `pty.resize`),跟 `tab_bar_height_reaches_the_remote_as_a_window_change`
    /// 是同一条链路的另一实例,写法直接照抄那条,变窄的维度从「高度」换成
    /// 「宽度」。**这条对「侧栏用 `SidePanel` 还是 `Area`」完全无感**——它
    /// 直接喂两个写死的 `PxRect`,根本不经过 egui,不构成端到端证据。真正
    /// 扎在「侧栏是不是真的让 egui 的中央区变窄」这一步的是
    /// `ui::tests::opening_the_files_sidebar_shrinks_the_central_area`;
    /// 两条各守半段,合起来才是完整的 T4 链路。
    ///
    /// 自证会变红:把下面的 `narrowed`(`w`)算法改成与 `full` 相同的宽度
    /// (中央区不再变窄 → 不发 window_change)。
    #[test]
    fn opening_the_files_sidebar_reaches_the_remote_as_a_window_change() {
        use crate::shell::workspace::{layout_geometry, PtyWriter, PxRect};
        use std::sync::{Arc, Mutex};

        #[derive(Default)]
        struct RecordingPty(Arc<Mutex<Vec<(u16, u16)>>>);
        impl PtyWriter for RecordingPty {
            fn write(&self, _bytes: Vec<u8>) -> Result<(), mullion_ssh::session::TrySendErr> {
                Ok(())
            }
            fn resize(&self, cols: u16, rows: u16) -> Result<(), mullion_ssh::session::TrySendErr> {
                self.0.lock().unwrap().push((cols, rows));
                Ok(())
            }
        }

        let seen = Arc::new(Mutex::new(Vec::new()));
        let (_tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
        let mut ws = Workspace::new(
            crate::shell::workspace::PaneState {
                id: PaneId(1),
                host_ix: 0,
                emulator: mullion_term::emulator::Emulator::new(80, 24),
                pty: Box::new(RecordingPty(seen.clone())),
                rx,
                pacer: crate::render::SyncFramePacer::new(),
                status: crate::shell::workspace::PaneStatus::Live,
                saw_first_byte: false,
                last_grid: (0, 0),
                cwd: None,
                tmux: None,
            },
            0,
        );
        let cell = (10.0f32, 20.0f32);
        let full = PxRect {
            x: 0,
            y: 0,
            w: 1600,
            h: 900,
        };
        // 侧栏 360px(`files_panel::DEFAULT_SIDEBAR_W`):中央区右边被吃掉这么多。
        let narrowed = PxRect {
            w: full.w - 360,
            ..full
        };
        let tree = ws.tree().clone();

        ws.apply_geometry(&layout_geometry(&tree, full, cell, false, 1.0));
        let before = *seen.lock().unwrap().last().expect("第一次必然发一次");
        seen.lock().unwrap().clear();

        ws.apply_geometry(&layout_geometry(&tree, narrowed, cell, false, 1.0));
        let after = seen.lock().unwrap().clone();
        assert_eq!(
            after.len(),
            1,
            "开侧栏必须恰好发一次 window_change(发 0 次 = 远端仍按旧列数排版;\
             发多次 = 每帧都在发)"
        );
        assert!(
            after[0].0 < before.0,
            "列数必须变少:{} → {}",
            before.0,
            after[0].0
        );

        // 反方向同样得走通:**关**侧栏中央区变宽,也必须发一次。只测「变窄」
        // 的话,一个只在变窄时发 resize 的实现照样绿 —— 而它的症状是关掉侧栏
        // 后远端 TUI 仍按窄列数排版,右边空一条,直到下次拖窗口才恢复。
        seen.lock().unwrap().clear();
        ws.apply_geometry(&layout_geometry(&tree, full, cell, false, 1.0));
        let back = seen.lock().unwrap().clone();
        assert_eq!(back.len(), 1, "关侧栏也必须恰好发一次 window_change");
        assert_eq!(back[0].0, before.0, "列数要回到开侧栏之前那个值");
    }

    /// **spec F36 验收标准的另一半**:切换标签的代码路径里不许有任何建连调用。
    ///
    /// 行为那一半在 `shell::tabs::tests::switching_tabs_does_not_touch_the_ssh_connections`
    /// (切换只动下标、`Arc` 计数与指针都不变)。这条守的是**接线**:切换分支
    /// 里但凡混进一句 `spawn_connect`,高延迟代理链路上每切一次标签就要重新握手
    /// 好几秒 —— F36 的产品价值当场归零,而且只有真机才看得出来。
    ///
    /// **扎的是源码结构而非运行时行为**:`App` 要 `EventLoopProxy` 才能构造。
    /// 验证边界:只挡得住「切换分支里出现 `spawn_connect`」这一种写法。
    ///
    /// 自证会变红:在 `TabAction::Switch` 或 `tab_hotkey_event` 的
    /// `Intent::Next` 分支里加一句 `self.spawn_connect(...)`。
    #[test]
    fn tab_switching_never_reconnects() {
        let src = include_str!("app.rs");
        // 快捷键那条路:`tab_hotkey_event` 整个函数体。
        let hot = src
            .split("fn tab_hotkey_event")
            .nth(1)
            .expect("找不到 tab_hotkey_event");
        let hot_body = &hot[..hot
            .find("\n    }\n")
            .expect("找不到 tab_hotkey_event 的函数结尾")];
        assert!(
            !hot_body.contains("spawn_connect"),
            "标签快捷键路径里出现了建连调用 —— 切一下标签就重连一次"
        );
        // 鼠标那条路:`actions.tab` 那个 match。
        let click = src
            .split("match actions.tab {")
            .nth(1)
            .expect("找不到标签栏动作的 match");
        let click_body = &click[..click
            .find("\n                            }\n")
            .expect("找不到标签栏动作 match 的结尾")];
        assert!(
            !click_body.contains("spawn_connect"),
            "标签栏点击路径里出现了建连调用 —— 切一下标签就重连一次"
        );
        assert!(
            click_body.contains("switch_to_index"),
            "标签栏点击路径里没有切换调用,上面那条断言是空跑"
        );
    }

    /// **接线守护 / Task 5**:`ConnectOk` 必须**开新标签**,不许再顶掉活动标签。
    ///
    /// Task 2 的过渡实现是 `close_active()` + `open()`(单标签下与迁移前逐帧
    /// 等价)。留着它的后果是「在 A 上连 B,A 那条连接被静默掐掉」—— 而用户
    /// 的心智模型是「多开一个」。
    ///
    /// 自证会变红:在 `ConnectOk` 的 `self.tabs.open(` 之前加回
    /// `let replaced = self.tabs.close_active();`。
    #[test]
    fn connecting_opens_a_new_tab_instead_of_replacing_the_active_one() {
        let src = include_str!("app.rs");
        let after = src
            .split("UserEvent::ConnectOk {\n                handle,\n                wants_sftp,\n                pty,\n            } => {")
            .nth(1)
            .expect("找不到 ConnectOk 的事件分支");
        let body = &after[..after
            .find("\n            }\n")
            .expect("找不到 ConnectOk 分支的结尾")];
        // D1:`ConnectOk` 两条分支各摆一次标签 —— SFTP 那条(`wants_sftp`)
        // 摆 `TabContent::Files`,终端那条摆 `TabContent::Terminal`。
        // F37:两处都改走 `place_tab`(它在没有 `pending_restore` 时**就是**
        // `self.tabs.open`,见 `replace_target` 的三条测试),断言两处都在。
        assert_eq!(
            body.matches("self.place_tab(").count(),
            2,
            "ConnectOk 里应该有两条 self.place_tab(——SFTP 分支一条,终端分支一条"
        );
        assert!(
            !body.contains("self.tabs.open("),
            "ConnectOk 里还有绕过 place_tab 直接开标签的分支 —— 那条路上重连会开出第二个标签,占位标签留在原地"
        );
        assert!(
            !body.contains("close_active"),
            "ConnectOk 里还在顶掉活动标签 —— 在 A 上连 B 会把 A 那条连接静默掐掉"
        );
    }

    /// F37:重连顶替的是**发起重连的那个标签**,不是活动标签。
    ///
    /// 自证会变红:把 `replace_target` 改成 `Some(0)` 或返回活动下标。
    #[test]
    fn a_reconnect_replaces_the_tab_that_asked_for_it_not_the_active_one() {
        use super::replace_target;
        use crate::shell::tabs::TabId;
        let ids = [TabId(10), TabId(20), TabId(30)];
        assert_eq!(replace_target(Some(TabId(10)), &ids), Some(0));
        assert_eq!(replace_target(Some(TabId(30)), &ids), Some(2));
    }

    /// F36 不许被 F37 破坏:**普通连接**(没有在途的重连)照旧开新标签,
    /// 不顶掉任何已有标签——顶掉的话在标签 A 上连 B 会把 A 那条连接掐掉。
    #[test]
    fn connecting_without_a_pending_restore_still_opens_a_new_tab() {
        use super::replace_target;
        use crate::shell::tabs::TabId;
        assert_eq!(replace_target(None, &[TabId(10), TabId(20)]), None);
    }

    /// 占位标签在拨号途中被用户关掉了:退回开新标签,**不能把连上的东西丢掉**
    /// (那就成了「点了重连,什么都没发生」)。
    #[test]
    fn a_pending_restore_whose_tab_was_closed_falls_back_to_a_new_tab() {
        use super::replace_target;
        use crate::shell::tabs::TabId;
        assert_eq!(replace_target(Some(TabId(99)), &[TabId(10)]), None);
    }

    /// E2:标签标题优先取会话名,空名字退回 `user@host`。
    ///
    /// 自证会变红:把 `.filter(|n| !n.is_empty())` 去掉,空名字会让标题变空白。
    #[test]
    fn the_tab_title_prefers_the_session_name_but_falls_back_to_user_at_host() {
        assert_eq!(
            tab_title(Some("生产库"), Some(("root", "10.0.0.1"))),
            "生产库"
        );
        assert_eq!(
            tab_title(Some(""), Some(("root", "10.0.0.1"))),
            "root@10.0.0.1"
        );
        assert_eq!(tab_title(None, Some(("root", "10.0.0.1"))), "root@10.0.0.1");
        assert_eq!(tab_title(None, None), "远端");
    }

    /// **接线守护 / F120**:`ConnectOk` 建标签时必须用 `PanelFrame::new(..)`
    /// 接配置的默认本地目录/书签,并把 `sftp_default_remote` 填成
    /// `sftp_prefs.default_remote`——不许有任何一条分支退回
    /// `PanelFrame::default()`(那样配置了也白配,新标签永远是空白默认态)。
    ///
    /// **扎的是源码结构**,理由同 `connecting_opens_a_new_tab_instead_of_replacing_the_active_one`:
    /// `ConnectOk` 里两条分支各建一次标签,单测里跑不动真实连接/`EventLoopProxy`,
    /// 只能扫源码确认两处都接上了配置读出来的 `sftp_prefs`。
    ///
    /// 自证会变红:把两处 `files: crate::ui::files_panel::PanelFrame::new(..)`
    /// 中的任意一处改回 `PanelFrame::default()`,或把 `sftp_default_remote:`
    /// 那一行删掉。
    #[test]
    fn connect_ok_wires_configured_sftp_prefs_into_both_new_tabs() {
        let src = include_str!("app.rs");
        let after = src
            .split("UserEvent::ConnectOk {\n                handle,\n                wants_sftp,\n                pty,\n            } => {")
            .nth(1)
            .expect("找不到 ConnectOk 的事件分支");
        let body = &after[..after
            .find("\n            }\n")
            .expect("找不到 ConnectOk 分支的结尾")];
        assert!(
            body.contains("let sftp_prefs ="),
            "ConnectOk 没有从 store 读配置的 SFTP 偏好"
        );
        assert_eq!(
            body.matches("crate::ui::files_panel::PanelFrame::new(")
                .count(),
            2,
            "两条分支(Files/Terminal)都该用 PanelFrame::new 接配置,不是 default()"
        );
        assert!(
            !body.contains("crate::ui::files_panel::PanelFrame::default()"),
            "ConnectOk 里还有分支在用 PanelFrame::default()——配置的默认本地目录/书签会被丢掉"
        );
        assert_eq!(
            body.matches("sftp_default_remote: sftp_prefs.default_remote")
                .count(),
            2,
            "两条分支都该把 sftp_prefs.default_remote 填进 sftp_default_remote 字段"
        );
    }

    /// **接线守护 / D1**:`spawn_connect` 里 `wants_sftp` 为真时,必须在
    /// `open_pty` 之前就 `return`——SFTP 节点没有 PTY 这个概念,握手一趟
    /// shell 既浪费一次网络往返,也会让远端多一条不需要的 channel。
    ///
    /// **扎的是源码结构**:异步任务体是 `self._runtime.spawn(async move { .. })`
    /// 里的自由代码,`App` 要 `EventLoopProxy` 才能构造,单测里跑不动真实
    /// 连接。验证边界:挡得住「`if wants_sftp` 分支里没有 `return`」这一种
    /// 写法(即分支后仍会往下掉到 `open_pty` 调用),挡不住把判断条件写反
    /// 之类更隐蔽的走样。
    ///
    /// 自证会变红:把 `if wants_sftp { .. return; }` 那个 `return;` 删掉。
    #[test]
    fn spawn_connect_skips_open_pty_when_the_target_wants_sftp() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn spawn_connect(&mut self, cfg: SshConfig, wants_sftp: bool) {")
            .nth(1)
            .expect("找不到 spawn_connect 的定义");
        let body = &after[..after
            .find("\n    }\n")
            .expect("找不到 spawn_connect 的函数结尾")];

        let after_if = body
            .split("if wants_sftp {")
            .nth(1)
            .expect("找不到 spawn_connect 里的 wants_sftp 分支");
        let wants_sftp_branch = &after_if[..after_if
            .find("\n            }\n")
            .expect("找不到 wants_sftp 分支的结尾(if 块本身的闭括号)")];
        assert!(
            wants_sftp_branch.contains("return;"),
            "spawn_connect 的 wants_sftp 分支没有 return —— 会继续往下掉到 \
             open_pty,给 SFTP 节点多开一趟根本用不上的 shell 握手"
        );
        assert!(
            !wants_sftp_branch.contains("open_pty"),
            "spawn_connect 的 wants_sftp 分支不该出现 open_pty 调用"
        );
        assert!(
            !body.contains("block_on"),
            "spawn_connect 绝不能 block_on —— 它跑在事件循环唤起的异步任务里, \
             阻塞会连累整个窗口卡在网络往返上(T1/T3 同类问题)"
        );
    }

    /// **接线守护 / D1**:`ConnectOk` 收到 `wants_sftp: true` 那条分支,不许
    /// 走「先建 `Workspace`/`open_pty`」的终端建标签路径——它是靠早 `return`
    /// 跟终端分支分开的(见 `spawn_connect` 那条守护里的 `return;`),这里补
    /// 反过来的一半:SFTP 分支自己也不能碰 `open_pty`/`Workspace::new`。
    ///
    /// 自证会变红:把 `if wants_sftp { .. }` 分支里的 `return;` 删掉,让代码
    /// 掉进下面 `let Some((ssh, rx)) = pty else { .. }`——`pty` 恒 `None` 会
    /// 直接走进那条错误分支,SFTP 标签建不出来。
    #[test]
    fn connect_ok_wants_sftp_branch_never_touches_open_pty_or_workspace() {
        let src = include_str!("app.rs");
        let after = src
            .split("UserEvent::ConnectOk {\n                handle,\n                wants_sftp,\n                pty,\n            } => {")
            .nth(1)
            .expect("找不到 ConnectOk 的事件分支");
        let full_body = &after[..after
            .find("\n            }\n")
            .expect("找不到 ConnectOk 分支的结尾")];
        let sftp_branch = full_body
            .split("if wants_sftp {")
            .nth(1)
            .expect("找不到 ConnectOk 里的 wants_sftp 分支")
            .split("\n                }\n")
            .next()
            .expect("找不到 ConnectOk 里 wants_sftp 分支的结尾");
        assert!(
            sftp_branch.contains("return;"),
            "ConnectOk 的 wants_sftp 分支没有及时 return —— 会继续往下掉进 \
             终端建标签的逻辑"
        );
        assert!(
            !sftp_branch.contains("open_pty") && !sftp_branch.contains("Workspace::new"),
            "ConnectOk 的 wants_sftp 分支不该碰 open_pty/Workspace::new —— \
             SFTP 节点没有 PTY,这些是终端标签专属的建立步骤"
        );
    }

    /// **接线守护 / Task 6**:「退出」必须逐个走收口,不靠进程退出兜底。
    ///
    /// `event_loop.exit()` 之后还要跑完本轮事件,析构顺序也不由我们定;自动化
    /// task 持着 `Arc<SshSession>`,不显式 abort 就可能在退出途中继续往一条
    /// 正在拆的 channel 上发命令。
    ///
    /// 自证会变红:把退出分支里的 `for tab in self.tabs.drain()` 那三行删掉。
    #[test]
    fn quitting_winds_down_every_tab() {
        let src = include_str!("app.rs");
        let after = src
            .split("if self.ui.request_quit {")
            .nth(1)
            .expect("找不到退出的分支");
        let body = &after[..after
            .find("\n                            }\n")
            .expect("找不到退出分支的结尾")];
        assert!(
            body.contains("self.tabs.drain()") && body.contains("wind_down("),
            "退出时没有逐个收口 —— 自动化 task 会在退出途中继续往正在拆的 \
             channel 上发命令"
        );
    }

    /// **接线守护(上半)**:「断开连接」必须走关标签的收口路径。
    ///
    /// D0 之前这条守护直接盯断开分支里的 `abort()`;标签化之后收口挪进了
    /// `wind_down`,所以拆成两条:这条钉「断开分支确实调了收口入口」,下一条
    /// 钉「那个收口入口确实 abort」。两条都在,断开→abort 这条链才算无缺口。
    ///
    /// **扎的是源码结构而非运行时行为**:`App` 要 `EventLoopProxy` 才能构造,
    /// 单测里造不出来。验证边界:只挡得住「断开分支里没调 close_active_tab」
    /// 这一种写法,挡不住有人另写一个不收口的同义函数。
    ///
    /// 自证会变红:把断开分支里的 `self.close_active_tab();` 换成
    /// `self.tabs.close_active();`(丢弃返回值 = 不收口)。
    #[test]
    fn disconnect_goes_through_the_tab_wind_down_path() {
        let src = include_str!("app.rs");
        let after = src
            .split("if self.ui.request_disconnect {")
            .nth(1)
            .expect("找不到断开连接的分支");
        let body = &after[..after
            .find("\n                            }\n")
            .expect("找不到断开连接分支的结尾")];
        assert!(
            body.contains("self.close_active_tab();"),
            "断开连接没走 close_active_tab —— 收口(abort 自动化 + drop pane)\
             全在那条路径上,绕过它等于断开后 io_task 不收口"
        );
    }

    /// **接线守护(下半)**:关标签的收口必须把自动化 task abort 掉。
    ///
    /// 自动化 task 也持有一份 `Arc<SshSession>`。只 drop 掉 `Workspace` 的话,
    /// `SshSession` 的 `cmd_tx` 仍有活着的克隆,`io_task` 不会收口 —— 用户点了
    /// 「断开」、标签已从 UI 上消失,预配置的命令却还在往一条没真正断开的
    /// channel 上发,用户既看不到也拦不住。`drive_automation` 补不了这条边:
    /// 标签一旦从 `self.tabs` 里摘掉,它就再也遍历不到了。
    ///
    /// **扎的是源码结构而非运行时行为**,理由同上半条。验证边界:只挡得住
    /// 「`wind_down` 里没有 abort」这一种写法。
    ///
    /// 自证会变红:把 `wind_down` 里的 `h.task.abort();` 删掉。
    #[test]
    fn closing_a_tab_aborts_its_automation_task() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn wind_down(")
            .nth(1)
            .expect("找不到 wind_down 的定义");
        let body = &after[..after.find("\n}\n").expect("找不到 wind_down 的函数结尾")];
        assert!(
            body.contains("abort()"),
            "wind_down 没 abort 自动化 task —— 它持有的那份 Arc<SshSession> \
             会让底层 io_task 不收口,关掉标签后预配置的命令还在继续发"
        );
    }

    /// **接线守护(SFTP 版,真实行为而非源码文本)**:关标签必须 abort 掉在途
    /// 的 sftp 后台任务(`spawn_sftp_open`/`spawn_sftp_list_dir`)。
    ///
    /// 与上面自动化那条同一类问题的重演:任务经 `Arc<SftpClient>`(内部
    /// `_conn: Arc<SshConnection>`)持有一份连接保活引用,只 drop
    /// `TerminalTab` 收不了口——用户在一次网络往返期间关掉标签,任务会继续
    /// 撑住底层连接,直到它自己的 RPC 结束(而 `SftpClient::open` 内部那两步
    /// 完全没有超时包裹,链路黑洞时可能永远不结束)。
    ///
    /// **这条不落成 `include_str!` 源码守护,而是真实构造 `TerminalTab` 跑
    /// `wind_down`**:`TerminalTab` 的字段全部可以脱离真实 SSH 连接/GPU/
    /// `EventLoopProxy` 直接造出来(`Workspace::new(test_pane(..), ..)` 用的
    /// 是 `NullPty`),不需要退回源码文本匹配。用一个「唯一能被 abort 掉、
    /// 不会自然结束」的哑任务占住 `sftp_tasks`,靠它退出时是否执行了 `Drop`
    /// 来判断有没有被真的 abort——比匹配 `.abort()` 这个字符串更接近「行为
    /// 是否正确」,不会被「换个不生效的写法但字符串上仍含 abort()」这种变体
    /// 蒙混过去。
    ///
    /// 自证会变红:把 `wind_down` 里 `for task in t.sftp_tasks { task.abort(); }`
    /// 那一段删掉。
    #[tokio::test]
    async fn wind_down_aborts_outstanding_sftp_tasks() {
        use std::sync::atomic::{AtomicBool, Ordering};

        struct SetOnDrop(Arc<AtomicBool>);
        impl Drop for SetOnDrop {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let started = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicBool::new(false));
        let started_flag = started.clone();
        let guard_flag = dropped.clone();
        let task = tokio::spawn(async move {
            let _guard = SetOnDrop(guard_flag);
            started_flag.store(true, Ordering::SeqCst);
            // 永不自然结束——唯一能让这个任务收尾的方式是被 abort。
            // `std::future::pending` 是标准库里"永远 Pending"的 future,
            // 不占 CPU,不是忙等。
            std::future::pending::<()>().await;
        });

        // 先让哑任务真正跑起来、挂在 `.await` 上,再 abort——否则 abort 打在
        // 一个**从未被 poll 过**的任务上:它的函数体压根没执行过,`_guard`
        // 也就没被构造过,`Drop` 自然不会触发,测的是"取消一个还没开始跑的
        // 任务"而不是"取消一个卡在网络 RTT 里的任务"(真实 sftp 场景),
        // 两者对 tokio 运行时是不同的路径,不能替代。
        for _ in 0..200 {
            if started.load(Ordering::SeqCst) {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            started.load(Ordering::SeqCst),
            "测试用的哑任务没能跑起来 —— 测试脚手架本身有问题,不是被测代码的锅"
        );

        let tab = Tab {
            id: TabId(1),
            title: "test".into(),
            session_id: None,
            title_override: None,
            color_override: None,
            content: TabContent::Terminal(Box::new(TerminalTab {
                ws: Workspace::new(test_pane(1), 0),
                current_preset: None,
                last_cfg: None,
                automation: Vec::new(),
                automation_template: None,
                automation_status: None,
                files: Default::default(),
                sftp: None,
                sftp_tasks: vec![task],
                sftp_default_remote: None,
                sftp_home: None,
            })),
        };

        wind_down(tab);

        // abort 是协作式的:runtime 要真正调度一次才会把 future drop 掉,
        // 不是调用 `abort()` 那一刻就同步发生。
        for _ in 0..200 {
            if dropped.load(Ordering::SeqCst) {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            dropped.load(Ordering::SeqCst),
            "wind_down 没有 abort 掉在途的 sftp 任务 —— 任务会继续攥着 \
             Arc<SshConnection>,撑住本该随标签一起断开的连接"
        );
    }

    /// **接线守护(D1「最隐蔽」那条的反面)**:`wind_down` 对 `TabContent`
    /// 的 match **不许**落到通配臂 `_ => { .. }` 上。
    ///
    /// 上面 `wind_down_aborts_outstanding_sftp_tasks` 只能真跑 `Terminal`
    /// 分支——`FilesTab::conn` 是 `Arc<SshConnection>`,`SshConnection::new`
    /// 对 `mullion-app` 不可见(`pub(crate)` 到 `mullion-ssh`),测试里造不出
    /// 一个真的 `FilesTab`。这条转而扎源码结构,专门堵一种编译期完全无感的
    /// 走样:如果 `TabContent` 以后加了第三个变体,而 `wind_down` 的 match
    /// 写成了带 `_ => {}` 兜底的形式,编译器不会报任何错——新变体的
    /// `sftp_tasks`/连接会静默泄漏,现象是内存/连接数缓慢增长,没有任何
    /// panic 或测试失败能提示到这里。要求两个变体各有一条具名分支,
    /// 逼着以后加变体的人回来把这条 match 补全。
    ///
    /// 自证会变红:把 `wind_down` 的 match 尾部两条分支合并成
    /// `_ => {}`(或者只删掉 `TabContent::Files` 那一臂换成通配)。
    #[test]
    fn wind_down_has_no_catch_all_arm_so_new_tab_kinds_cannot_leak_silently() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn wind_down(")
            .nth(1)
            .expect("找不到 wind_down 的定义");
        let body = &after[..after.find("\n}\n").expect("找不到 wind_down 的函数结尾")];
        // 按行取 trim 后的实际代码,不匹配文档注释里提到的 `_ => {}` 这串文字
        // (上面就有一行注释专门提醒"不能写成通配"——它本身含这串子串)。
        assert!(
            !body
                .lines()
                .any(|l| !l.trim_start().starts_with("//") && l.trim_start().starts_with("_ =>")),
            "wind_down 的 match 出现了通配臂 —— 以后 TabContent 再加变体,\
             这里会静默漏掉收口,连接/任务缓慢泄漏,且没有任何编译错误提示"
        );
        assert!(
            body.contains("TabContent::Terminal(t) =>")
                && body.contains("TabContent::Files(f) =>")
                && body.contains("TabContent::Restored(_) =>"),
            "wind_down 必须对 Terminal/Files/Restored 各有一条具名分支,不能少写"
        );
        assert!(
            body.matches("task.abort()").count() >= 2,
            "Terminal 和 Files 两条分支都该 abort 各自的 sftp_tasks,\
             实际只找到 {} 处 abort()",
            body.matches("task.abort()").count()
        );
    }

    /// **T1 守护**:取消边绝不能挂在 `PtyWriter::write` 上。
    ///
    /// `Workspace::pump` 内部也调 `p.pty.write(out)` —— 那是 T1 的 PtyWrite
    /// 应答(DSR 光标位置查询、同步输出探测的回应),**不是用户输入**。若把
    /// 「有人往 PTY 写字节」当成用户接管的判据,远端一发同步输出探测,自动化
    /// 立刻自杀,而且现象是「有时候能跑有时候跑不了」,极难定位。
    ///
    /// **这条测试扎的是源码结构而不是运行时行为**,这是刻意的:取消边是
    /// `App` 的私有方法,`Workspace` 根本够不着它,运行时构造不出「pump 触发
    /// 取消」的场景;真正的风险是将来有人为了图省事把取消挪进
    /// `PtyWriter::write` 或 `Workspace::pump`。验证边界:它只挡得住「调用了
    /// `user_took_over`」这一种写法,挡不住有人另起一个同义的新函数。
    ///
    /// 自证会变红:在 `shell/workspace/mod.rs` 里随便加一行注释提到
    /// `user_took_over`。
    #[test]
    fn pty_write_echo_does_not_cancel_automation() {
        let src = include_str!("shell/workspace/mod.rs");
        assert!(
            !src.contains("user_took_over"),
            "workspace 里出现了 user_took_over —— `Workspace::pump` 的 pty.write \
             是 T1 的 PtyWrite 应答(DSR/同步输出探测的回应),不是用户输入。\
             把取消挂上去,远端一发探测自动化就自杀,现象是「有时候能跑有时候不能」。\
             取消边只能挂在 app.rs 里的用户意图写入点上。"
        );
    }

    use mullion_store::MasterKeySource;

    /// 测试用主密钥源:不碰 keyring(CI/无头环境没有钥匙串守护进程)。
    struct FixedKey;
    impl MasterKeySource for FixedKey {
        fn load_or_create(&self) -> Result<[u8; 32], mullion_store::StoreError> {
            Ok([7u8; 32])
        }
    }

    fn tmp_store() -> (tempfile::TempDir, crate::shell::store::SessionStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = crate::shell::store::SessionStore::open(dir.path().to_path_buf(), &FixedKey)
            .expect("open store");
        (dir, store)
    }

    /// F2 两阶段回填的红线:**导入两条互为跳板的会话后,被跳板那条的
    /// `network.jump` 必须真的指向另一条的 id。**
    ///
    /// 一阶段建完就完事的写法编译得过、导入也「成功」了,现象是跳板静静
    /// 地空着,用户要到第一次连接才发现流量根本没过堡垒机。
    ///
    /// 自证会变红:把 `apply_import` 的第二个 `for` 循环整段删掉。
    #[test]
    fn importing_a_pair_wires_the_proxy_jump_to_the_real_session_id() {
        let (_dir, mut store) = tmp_store();
        let parsed = mullion_store::parse_ssh_config(
            "Host target\n  User ops\n  ProxyJump bastion\nHost bastion\n  User ops\n",
        );
        let rows = crate::ui::import_dialog::build_rows(&parsed, &[]);
        let n = apply_import(&mut store, &rows, "2026-08-13T00:00:00Z").expect("导入应成功");
        assert_eq!(n, 2);

        let bastion = store
            .list()
            .iter()
            .find(|s| s.identity.name == "bastion")
            .expect("bastion 应已导入")
            .id;
        let target = store
            .list()
            .iter()
            .find(|s| s.identity.name == "target")
            .expect("target 应已导入");
        assert_eq!(
            target.network.jump,
            Some(vec![mullion_store::JumpRef(bastion)]),
            "ProxyJump 必须翻成对刚建那条会话的引用"
        );
    }

    /// 指向本批之外的别名:那条会话照常导入,跳板留空,**绝不凭空造一条
    /// 跳板会话**(设计 D4)——那是往库里塞用户没批准的配置。
    ///
    /// 自证会变红:让 `apply_import` 对找不到的别名 `store.add` 一条占位会话。
    #[test]
    fn a_jump_outside_the_batch_leaves_the_jump_empty_and_invents_nothing() {
        let (_dir, mut store) = tmp_store();
        let parsed = mullion_store::parse_ssh_config("Host lonely\n  User ops\n  ProxyJump gone\n");
        let rows = crate::ui::import_dialog::build_rows(&parsed, &[]);
        apply_import(&mut store, &rows, "2026-08-13T00:00:00Z").expect("导入应成功");

        assert_eq!(store.list().len(), 1, "不该凭空多造一条「gone」");
        assert_eq!(
            store.list()[0].network.jump,
            Some(Vec::new()),
            "跳板该留空,而不是继承(None)"
        );
    }

    /// 没勾的行一条都不许进库 —— 预览的意义就在这里。
    #[test]
    fn unchecked_rows_are_not_imported() {
        let (_dir, mut store) = tmp_store();
        let parsed = mullion_store::parse_ssh_config("Host a\n  User ops\nHost b\n  User ops\n");
        let mut rows = crate::ui::import_dialog::build_rows(&parsed, &[]);
        rows[1].selected = false;
        let n = apply_import(&mut store, &rows, "2026-08-13T00:00:00Z").expect("导入应成功");
        assert_eq!(n, 1);
        assert_eq!(
            store
                .list()
                .iter()
                .map(|s| s.identity.name.clone())
                .collect::<Vec<_>>(),
            vec!["a".to_string()]
        );
    }

    /// F74:凭据保存一次,`store.credentials()` 里就得有它,且拿得到的是
    /// **store 分配的那个 id** —— 返回值错了的现象是「再点一次保存又新建
    /// 一条」,而用户看到的只是列表里多出一份同名凭据。
    #[test]
    fn a_saved_credential_lands_in_the_store_under_the_returned_id() {
        let (_dir, mut store) = tmp_store();
        let buf = crate::ui::session_manager::CredentialEditorBuffer {
            name: "运维号".into(),
            user: "ops".into(),
            ..Default::default()
        };
        let (password, passphrase, private_key) =
            crate::ui::session_manager::credential_secret_fields(&buf);
        let id = apply_credential_save(
            &mut store,
            crate::ui::session_manager::CredentialSaveIntent {
                editing_id: None,
                draft: crate::ui::session_manager::build_credential_draft(&buf).expect("build"),
                password,
                passphrase,
                private_key,
            },
        )
        .expect("保存应成功");
        let rec = store
            .credentials()
            .iter()
            .find(|c| c.id == id)
            .expect("apply_credential_save 返回的 id 必须是 store 真正分配的那个");
        assert_eq!(rec.name, "运维号");
        assert_eq!(rec.user, "ops");
    }

    /// F74 端到端红线,与会话侧 `editing_a_session_without_touching_password_keeps_it`
    /// 同形:**存一份带私钥+口令的凭据,再原样保存一次(两个框都没碰),
    /// 私钥正文和口令必须都还在,`has_passphrase` 也不能被打回 false。**
    ///
    /// 后半条是独立的一个坑:编辑时口令框恒为空,`build_credential_draft`
    /// 自己算出来的 `has_passphrase` 必然是 false,只有 `apply_credential_save`
    /// 里那句按合成结果的校正能把它救回来;丢了的现象是下次连接时 russh
    /// 拿着加密私钥却不知道要口令。
    ///
    /// 自证会变红(两次):
    /// 1. 把 `apply_credential_save` 里的 `store.credential_secret(id)` 换成
    ///    `None` —— 私钥/口令双双变 None。
    /// 2. 删掉那句 `*has_passphrase = ...` 的校正 —— 读回的 kind 是
    ///    `PublicKey { has_passphrase: false }`。
    #[test]
    fn saving_an_existing_credential_again_keeps_its_key_and_passphrase() {
        let (_dir, mut store) = tmp_store();

        let mut first = crate::ui::session_manager::CredentialEditorBuffer {
            name: "部署号".into(),
            user: "root".into(),
            auth_kind: crate::ui::session_manager::AuthKindUi::PublicKey,
            ..Default::default()
        };
        first.key_data = "-----BEGIN OPENSSH PRIVATE KEY-----\nx\n".into();
        first.key_touched = true;
        first.passphrase = "ph".into();
        first.passphrase_touched = true;
        let (password, passphrase, private_key) =
            crate::ui::session_manager::credential_secret_fields(&first);
        let id = apply_credential_save(
            &mut store,
            crate::ui::session_manager::CredentialSaveIntent {
                editing_id: None,
                draft: crate::ui::session_manager::build_credential_draft(&first).expect("build"),
                password,
                passphrase,
                private_key,
            },
        )
        .expect("首次保存应成功");

        // 重新打开这份凭据:密文框一律为空、没碰过(store 不回吐明文)。
        let second = crate::ui::session_manager::CredentialEditorBuffer::from_record(
            store
                .credentials()
                .iter()
                .find(|c| c.id == id)
                .expect("刚存的凭据应在库里"),
        );
        let (password, passphrase, private_key) =
            crate::ui::session_manager::credential_secret_fields(&second);
        apply_credential_save(
            &mut store,
            crate::ui::session_manager::CredentialSaveIntent {
                editing_id: Some(id),
                draft: crate::ui::session_manager::build_credential_draft(&second).expect("build"),
                password,
                passphrase,
                private_key,
            },
        )
        .expect("二次保存应成功");

        let secret = store.credential_secret(id).expect("密文整条不该塌成 None");
        assert!(
            secret.private_key.is_some(),
            "私钥正文没碰过就该留着,实得 None"
        );
        assert_eq!(
            secret.passphrase.as_deref(),
            Some("ph"),
            "口令没碰过就该留着"
        );
        assert_eq!(
            store
                .credentials()
                .iter()
                .find(|c| c.id == id)
                .map(|c| c.kind.clone()),
            Some(mullion_store::AuthKind::PublicKey {
                has_passphrase: true
            }),
            "has_passphrase 必须跟合成后的密文走,不跟空着的表单走"
        );
    }

    /// F74/D7:删被引用的凭据被拒时,状态栏那句话里要有**引用者的会话名**。
    ///
    /// store 只报得出 `SessionId`(`CredentialInUse` 的 Display 连数量都只
    /// 说个数),照直透传的话用户得挨个点开会话去找是谁在用。
    ///
    /// 自证会变红:把 `credential_delete_error` 的 `CredentialInUse` 分支
    /// 删掉、让它落到 `other` 透传 store 原文。
    #[test]
    fn refusing_to_delete_a_credential_names_the_sessions_using_it() {
        let (_dir, mut store) = tmp_store();
        let cid = store.add_credential(mullion_store::CredentialDraft {
            name: "运维号".into(),
            user: "ops".into(),
            kind: mullion_store::AuthKind::Password,
            secret: None,
        });
        for name in ["web01", "db02"] {
            let buf = crate::ui::session_manager::EditorBuffer {
                name: name.into(),
                host: "192.0.2.10".into(),
                cred_source: crate::ui::session_manager::CredSourceUi::Shared,
                credential_id: Some(cid),
                ..Default::default()
            };
            store.add(
                crate::ui::session_manager::build_draft(&buf).expect("build"),
                "2026-08-13T00:00:00Z",
            );
        }

        let err = store
            .delete_credential(cid)
            .expect_err("被引用的凭据不该删得掉");
        let msg = credential_delete_error(&err, store.list());
        assert!(
            msg.contains("web01") && msg.contains("db02"),
            "拒绝的理由必须点名是谁在用;实得:{msg}"
        );
    }

    /// F73 端到端红线:**先存一个带密码的会话,再原样保存一次(密码框没碰过),
    /// 密码必须还在。** 这是用户实际会走的路径,也是改前会丢密码的那条。
    ///
    /// 自证会变红:把 `apply_save` 里的 `store.secret(id)` 换成 `None`
    /// (即改前「看不到已存凭据」的状态),这条报 None != Some("pw")。
    #[test]
    fn editing_a_session_without_touching_password_keeps_it() {
        let (_dir, mut store) = tmp_store();

        let mut first = crate::ui::session_manager::EditorBuffer {
            name: "dev".into(),
            host: "192.0.2.10".into(),
            user: "user".into(),
            ..Default::default()
        };
        first.password = "pw".into();
        first.password_touched = true;
        let id = apply_save(
            &mut store,
            crate::ui::session_manager::SaveIntent {
                editing_id: None,
                draft: crate::ui::session_manager::build_draft(&first).expect("build"),
                password: crate::ui::session_manager::SecretField::Set("pw".into()),
                passphrase: crate::ui::session_manager::SecretField::Clear,
                proxy_password: crate::ui::session_manager::SecretField::Keep,
                private_key: crate::ui::session_manager::SecretField::Keep,
                then_connect: false,
            },
            "2026-08-03T00:00:00Z",
        )
        .expect("首次保存应成功");
        assert_eq!(
            store.secret(id).and_then(|s| s.password.clone()).as_deref(),
            Some("pw")
        );

        // 第二次:只改备注,密码框一次都没碰过(触碰位全 false)
        let mut again = first.clone();
        again.note = "改了备注".into();
        again.password = String::new();
        again.password_touched = false;
        apply_save(
            &mut store,
            crate::ui::session_manager::SaveIntent {
                editing_id: Some(id),
                draft: crate::ui::session_manager::build_draft(&again).expect("build"),
                password: crate::ui::session_manager::SecretField::Keep,
                passphrase: crate::ui::session_manager::SecretField::Clear,
                proxy_password: crate::ui::session_manager::SecretField::Keep,
                private_key: crate::ui::session_manager::SecretField::Keep,
                then_connect: false,
            },
            "2026-08-03T00:01:00Z",
        )
        .expect("二次保存应成功");

        assert_eq!(
            store.secret(id).and_then(|s| s.password.clone()).as_deref(),
            Some("pw"),
            "没碰密码框就保存,已存密码必须原样留着(F73)"
        );
    }

    /// 新建路径必须把 store 分配的 `SessionId` 交回去 ——「保存并连接」要用它。
    /// 改前 `app.rs` 是 `None => { store.add(draft, &now); store.save() }`,
    /// 返回值直接丢弃。
    /// 自证会变红:把 `apply_save` 新建分支改成 `Ok(SessionId(0))`,
    /// 这条报「新 id 应能在 store 里查到」。
    #[test]
    fn apply_save_new_returns_id_allocated_by_store() {
        let (_dir, mut store) = tmp_store();
        let buf = crate::ui::session_manager::EditorBuffer {
            name: "dev".into(),
            host: "192.0.2.10".into(),
            user: "user".into(),
            ..Default::default()
        };
        let id = apply_save(
            &mut store,
            crate::ui::session_manager::SaveIntent {
                editing_id: None,
                draft: crate::ui::session_manager::build_draft(&buf).expect("build"),
                password: crate::ui::session_manager::SecretField::Clear,
                passphrase: crate::ui::session_manager::SecretField::Clear,
                proxy_password: crate::ui::session_manager::SecretField::Clear,
                private_key: crate::ui::session_manager::SecretField::Keep,
                then_connect: false,
            },
            "2026-08-03T00:00:00Z",
        )
        .expect("保存应成功");
        assert!(
            store.list().iter().any(|r| r.id == id),
            "apply_save 返回的 id 必须是 store 真正分配的那个"
        );
    }

    /// F122 的核心判据(D1):标签属性保存只落在标签自己的两个覆盖字段上,
    /// 连接时拼的 `title` 不被改写。
    ///
    /// 「不写 store」这条**不是**这里验的——`apply_tab_props` 的签名只吃
    /// `&mut Tabs<C>`,结构上就碰不到 store,真去构造一个 store 断言它没变
    /// 反而是分支不可达的恒绿测试。「不写 store」由
    /// `tab_props_is_no_longer_a_store_write_intent`(源码级守护)与这个签名
    /// 本身共同保证。
    ///
    /// 自证会变红:把 `t.title_override`/`color_override` 的赋值改错(比如
    /// 恒设 `None`,或把 `title` 也一起改写)。
    #[test]
    fn applying_tab_props_only_writes_the_two_override_fields() {
        let mut tabs: crate::shell::tabs::Tabs<u64> = Default::default();
        let sid = mullion_store::SessionId(1);
        let tab = tabs.open("u@h".into(), Some(sid), 1);
        apply_tab_props(
            &mut tabs,
            tab,
            "日志".into(),
            Some(mullion_term::snapshot::Rgb::new(0xe0, 0x67, 0x67)),
        );
        let t = tabs.iter().next().unwrap();
        assert_eq!(t.display_title(), "日志");
        assert_eq!(
            t.color_override,
            Some(mullion_term::snapshot::Rgb::new(0xe0, 0x67, 0x67))
        );
        assert_eq!(t.title, "u@h", "连接时拼的 title 不该被改写");
    }

    /// F122:空名字(或只有空白)= 清除覆盖,退回连接时拼的 `title`。
    ///
    /// 存一个空标签名会让标签栏上出现一块点得到但看不见的东西 —— 所以
    /// `apply_tab_props` 把这种输入当成「用户想清掉覆盖」,而不是「把
    /// 标签名存成空串」。先设一个非空覆盖再清,才测得出「清除」这个动作;
    /// 一开始就是 `None`、传空还是 `None` 是恒绿的重言式。
    ///
    /// 自证会变红:把 `(!trimmed.is_empty()).then(...)` 改成
    /// `Some(trimmed.to_string())`。
    #[test]
    fn apply_tab_props_with_a_blank_name_clears_the_override() {
        let mut tabs: crate::shell::tabs::Tabs<u64> = Default::default();
        let sid = mullion_store::SessionId(1);
        let tab = tabs.open("u@h".into(), Some(sid), 1);
        apply_tab_props(&mut tabs, tab, "日志".into(), None);
        assert_eq!(
            tabs.iter().next().unwrap().display_title(),
            "日志",
            "先设一个非空覆盖,后面才测得出清除"
        );

        apply_tab_props(&mut tabs, tab, "".into(), None);
        let t = tabs.iter().next().unwrap();
        assert_eq!(t.title_override, None, "空串应清掉覆盖,而不是存成空串");
        assert_eq!(t.display_title(), "u@h", "清除后应退回连接时拼的 title");

        apply_tab_props(&mut tabs, tab, "日志".into(), None);
        apply_tab_props(&mut tabs, tab, "   ".into(), None);
        let t = tabs.iter().next().unwrap();
        assert_eq!(t.title_override, None, "纯空白同样应清掉覆盖");
        assert_eq!(t.display_title(), "u@h", "清除后应退回连接时拼的 title");
    }

    /// F122/D2:覆盖**不进 F37 布局快照**。`snapshot_tabs_of` 存的必须是连接时拼的
    /// `tab.title`,不是 `display_title()` —— 存了覆盖的话,「关窗口即丢」这条承诺
    /// 就变成了「关窗口还在,但会话改了名又不跟着变」的第三种语义。
    ///
    /// 自证会变红:把 `snapshot_tabs_of` 里的 `tab.title.clone()` 改成
    /// `tab.display_title().to_string()`。
    #[test]
    fn a_tab_override_never_reaches_the_layout_snapshot() {
        let mut tabs: Tabs<TabContent> = Default::default();
        tabs.open("u@h".into(), Some(SessionId(1)), restored_tab(1, 1));
        tabs.iter_mut().next().unwrap().title_override = Some("日志".into());
        let (saved, _) = snapshot_tabs_of(&tabs);
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].title, "u@h", "覆盖被写进了布局快照");
    }

    /// F73 参数错位红线:`merge_secret(existing, &password, &passphrase,
    /// &proxy_password)` 后三个参数同类型(`&SecretField`),顺序写反不会编译
    /// 失败。用公钥认证 + 三个槽位各占三态之一(`Set`/`Keep`/`Clear`)、已存值
    /// 也互不相同,才能让"顺序写反"产生可观察的错误结果——三个槽位结果若趋同,
    /// 参数对调不会被任何断言抓到。同时覆盖 `sync_has_passphrase` 走
    /// `apply_save` 真实路径的效果(F73 附带项,之前只在 `merge_secret`/
    /// `sync_has_passphrase` 单元层测过,没测过它们在 `apply_save` 里接线对不对)。
    ///
    /// 自证会变红(两次,见提交说明):
    /// 1. 把 `apply_save` 里 `merge_secret(..., &passphrase, &proxy_password)`
    ///    两个参数对调——passphrase/proxy_password 的最终值会互相串,断言报错。
    /// 2. 注释掉 `sync_has_passphrase(&mut draft, merged.as_ref())`——
    ///    has_passphrase 不再跟随合成结果,读回的记录里 has_passphrase 与预期不符。
    #[test]
    fn apply_save_merges_three_secret_slots_independently_and_syncs_has_passphrase() {
        let (_dir, mut store) = tmp_store();

        // 先存一条公钥认证会话,三个槽位都有互不相同的已存值。
        let mut first = crate::ui::session_manager::EditorBuffer {
            name: "dev".into(),
            host: "192.0.2.10".into(),
            user: "user".into(),
            auth_kind: crate::ui::session_manager::AuthKindUi::PublicKey,
            ..Default::default()
        };
        first.password = "old-pw".into();
        first.password_touched = true;
        first.passphrase = "old-ph".into();
        first.passphrase_touched = true;
        first.proxy_password = "old-proxy".into();
        first.proxy_password_touched = true;
        let id = apply_save(
            &mut store,
            crate::ui::session_manager::SaveIntent {
                editing_id: None,
                draft: crate::ui::session_manager::build_draft(&first).expect("build"),
                password: crate::ui::session_manager::SecretField::Set("old-pw".into()),
                passphrase: crate::ui::session_manager::SecretField::Set("old-ph".into()),
                proxy_password: crate::ui::session_manager::SecretField::Set("old-proxy".into()),
                private_key: crate::ui::session_manager::SecretField::Keep,
                then_connect: false,
            },
            "2026-08-03T00:00:00Z",
        )
        .expect("首次保存应成功");

        // 第二次保存:password=Set(覆盖新值)、passphrase=Keep(原样留着)、
        // proxy_password=Clear(真的清除)——三态各占一个槽位,已存值互不相同,
        // 足以抓住任意两个参数写反。passphrase 框显式清成"没碰过"(触碰位
        // false、内容清空),模拟真实场景(store 不回吐明文,编辑已有会话时
        // 口令框恒为空)——这样 `build_draft(&second)` 自己算出来的
        // `has_passphrase` 是 false(它看不到 store 的已存值),必须靠
        // `apply_save` 里真正的 `sync_has_passphrase` 用合成结果校正回 true;
        // 若沿用 first.passphrase_touched=true,`build_draft` 会恰好蒙对答案,
        // 抓不住 `sync_has_passphrase` 被删掉的情况(已通过红队注入实测确认)。
        let mut second = first.clone();
        second.password = "new-pw".into();
        second.passphrase = String::new();
        second.passphrase_touched = false;
        apply_save(
            &mut store,
            crate::ui::session_manager::SaveIntent {
                editing_id: Some(id),
                draft: crate::ui::session_manager::build_draft(&second).expect("build"),
                password: crate::ui::session_manager::SecretField::Set("new-pw".into()),
                passphrase: crate::ui::session_manager::SecretField::Keep,
                proxy_password: crate::ui::session_manager::SecretField::Clear,
                private_key: crate::ui::session_manager::SecretField::Keep,
                then_connect: false,
            },
            "2026-08-03T00:01:00Z",
        )
        .expect("二次保存应成功");

        let secret = store
            .secret(id)
            .expect("凭据应还在(passphrase 被 Keep 住,整条不该塌成 None)");
        assert_eq!(
            secret.password.as_deref(),
            Some("new-pw"),
            "password=Set 应覆盖为新值"
        );
        assert_eq!(
            secret.passphrase.as_deref(),
            Some("old-ph"),
            "passphrase=Keep 应原样留着已存值"
        );
        assert_eq!(
            secret.proxy_password, None,
            "proxy_password=Clear 应真的清除已存值"
        );

        // sync_has_passphrase 走 apply_save 真实路径:passphrase 被 Keep 住、
        // 合成后仍有值,has_passphrase 必须是 true——靠 `store.list()` 读回
        // 记录验证(不为测试新增生产 API)。
        let rec = store
            .list()
            .iter()
            .find(|r| r.id == id)
            .expect("刚保存的会话应能在 store 里查到");
        match &rec.auth.as_inline().expect("测试里存的是自带认证").kind {
            mullion_store::AuthKind::PublicKey { has_passphrase, .. } => assert!(
                *has_passphrase,
                "passphrase 合成后仍有值,has_passphrase 必须是 true"
            ),
            other => panic!("应为 PublicKey,实际: {other:?}"),
        }
    }

    /// F92:世代号变了就必须丢弃迟到的拨测结果。
    ///
    /// 场景:对 A 点了「测试连接」,20 秒超时未到就切到了 B。若不校验世代,
    /// A 的结果会写到 B 的表单上 —— 用户看到「连接成功」,其实测的是别人。
    ///
    /// 自证变红的方式:把 `accept_probe` 里 `epoch == current` 的守卫去掉,
    /// 而不是改测试里传的 epoch 值。
    #[test]
    fn stale_probe_result_is_discarded_after_epoch_bump() {
        use crate::ui::session_manager::ProbeState;

        let mut state = ProbeState::Running;
        // 当前世代 7,回来的是 6 —— 期间切过一次会话。
        assert!(!crate::app::accept_probe(6, 7, &mut state, Ok(())));
        assert_eq!(state, ProbeState::Running, "过期结果不许改动状态");

        // 同世代的结果照常采纳。
        assert!(crate::app::accept_probe(7, 7, &mut state, Ok(())));
        assert_eq!(state, ProbeState::Ok);

        let mut state = ProbeState::Running;
        assert!(crate::app::accept_probe(
            3,
            3,
            &mut state,
            Err("超时".to_owned())
        ));
        assert_eq!(state, ProbeState::Err("超时".to_owned()));
    }

    /// F92:取消拨测必须**同时**自增世代号和清掉旧结论。
    ///
    /// 只做前者会漏掉一个窗口期:`close_session_manager` 设 `probe_cancel`
    /// 是在 UI/事件回调里,真正施加要等到下一次重绘。这中间到达的结果
    /// 世代号还是旧的、会被采纳,于是一个作废的「连接成功」留在状态里,
    /// 下次编辑无关会话时凭空出现。
    ///
    /// 自证变红的方式:把 `cancel_probe` 里的 `*state = ProbeState::Idle;`
    /// 删掉(第二条断言变红),或把 `wrapping_add(1)` 改成不自增
    /// (第一、三条变红)。都要改**产品代码**,不是改测试里的初值。
    #[test]
    fn cancelling_a_probe_bumps_the_epoch_and_clears_the_stale_verdict() {
        use crate::ui::session_manager::ProbeState;

        let mut epoch = 7u64;
        // 窗口期内被采纳的、已经作废的结论。
        let mut state = ProbeState::Ok;

        crate::app::cancel_probe(&mut epoch, &mut state);

        assert_eq!(epoch, 8, "取消必须自增世代,否则在途结果还会被采纳");
        assert_eq!(state, ProbeState::Idle, "作废的结论不许留在表单上");

        // 自增之后,旧世代的结果确实进不来了。
        let mut later = ProbeState::Running;
        assert!(!crate::app::accept_probe(7, epoch, &mut later, Ok(())));
        assert_eq!(later, ProbeState::Running);
    }

    /// F120:`spawn_sftp_open` 该用哪个目录做起点——配置了默认远端目录就用它,
    /// 没配置(`None`)落回登录目录(`.`)。
    ///
    /// 抽成纯函数单测,理由同 `sync_timeout_wake_at`:`spawn_sftp_open` 本身
    /// 要 `Runtime`/`EventLoopProxy`,单测里构造不出来,判定逻辑不能只靠它
    /// 兜底。
    ///
    /// 自证变红的方式:把 `configured_remote_dir` 改成恒返回登录目录
    /// (第一条断言变红),或者恒返回传入的 `configured`(`None` 分支编译
    /// 不过,或者第二条断言变红)。
    #[test]
    fn configured_remote_dir_falls_back_to_login_directory_when_unset() {
        let configured = crate::app::configured_remote_dir(Some("/srv/app"));
        assert_eq!(
            configured,
            mullion_ssh::sftp::RemotePath::from_bytes(b"/srv/app".to_vec())
        );

        let default = crate::app::configured_remote_dir(None);
        assert_eq!(
            default,
            mullion_ssh::sftp::RemotePath::from_bytes(b".".to_vec())
        );
    }

    /// **F120 补链路覆盖(读取半程)**:`trigger_sftp_open` 传给
    /// `spawn_sftp_open` 的 `default_remote` 靠 `TabContent::sftp_default_remote()`
    /// 从标签字段里取。复核实测:把 `trigger_sftp_open` 里这个取值硬改成
    /// `None`,689 条测试零变红——`connect_ok_wires_configured_sftp_prefs_into_both_new_tabs`
    /// 只守「配置值写进字段」,`configured_remote_dir_falls_back_to_login_directory_when_unset`
    /// 只守「拿到值之后怎么判定回退」,中间「字段真的被读出来」没人守。
    ///
    /// 这条钉死取值器本身:配了就原样吐出来,没配就是 `None`——不能是硬编码
    /// 的常量(无论配没配都返回同一个值,这条测试也会挡住)。
    ///
    /// `TabContent::Files` 变体因为 `FilesTab::conn` 是 `Arc<SshConnection>`
    /// (`SshConnection::new` 对 `mullion-app` 不可见)在这个测试容器里造不出来,
    /// 这条只覆盖 `Terminal` 变体——`Files` 分支(`f.sftp_default_remote.clone()`)
    /// 是与 `Terminal` 分支同构的一行代码,下面
    /// `trigger_sftp_open_passes_the_tabs_default_remote_into_spawn_sftp_open`
    /// 补两个变体共用的调用点覆盖。
    ///
    /// 自证会变红:把 `TabContent::sftp_default_remote` 里 `Terminal` 分支的
    /// `t.sftp_default_remote.clone()` 改成 `None`(第一条断言红);改成恒
    /// `Some("...".into())`(第二条断言红)。
    #[test]
    fn tab_content_sftp_default_remote_reflects_the_configured_value_not_hardcoded() {
        fn make_tab(configured: Option<&str>) -> TabContent {
            TabContent::Terminal(Box::new(TerminalTab {
                ws: Workspace::new(test_pane(1), 0),
                current_preset: None,
                last_cfg: None,
                automation: Vec::new(),
                automation_template: None,
                automation_status: None,
                files: Default::default(),
                sftp: None,
                sftp_tasks: Vec::new(),
                sftp_default_remote: configured.map(|s| s.to_string()),
                sftp_home: None,
            }))
        }

        let configured = make_tab(Some("/srv/configured-app"));
        assert_eq!(
            configured.sftp_default_remote(),
            Some("/srv/configured-app".to_string()),
            "配了默认远端目录时,取值器必须原样吐出来"
        );

        let unset = make_tab(None);
        assert_eq!(
            unset.sftp_default_remote(),
            None,
            "没配置时必须是 None——区别于恒返回某个写死值的实现"
        );
    }

    /// **接线守护**:`trigger_sftp_open` 把「配置的默认远端目录」和「焦点 pane
    /// 报出来的 cwd」**两样都原样**递给 `spawn_sftp_open`。
    ///
    /// 起始目录的计算从这里挪进了 `spawn_sftp_open` —— `~` 展开要用远端的真
    /// 登录目录,而那个值只有 `canonicalize(".")` 回来之后才知道,在这里算
    /// 就必然拿不到。这里只负责把两个原料递下去,少递一个都是静默失效:
    /// 少了 `default_remote`,F120 配置的默认目录被丢;少了 `pane_cwd`,
    /// F123 的目录继承被丢。
    ///
    /// 自证会变红:把 `pane_cwd` 那个实参换成 `None`。
    #[test]
    fn trigger_sftp_open_hands_both_ingredients_to_spawn_sftp_open() {
        let src = include_str!("app.rs");
        // 锚点带**行首缩进**:`"\n    fn …"` 在源码里只有真正的方法定义处成立。
        // 不带的话,测试自己 `.split("fn trigger_sftp_open(…")` 这行字面量也会
        // 被 `include_str!` 匹配上,函数一旦改名/被删,切分不会走到 `expect`,
        // 而是切到测试自己身上,报出一条方向完全跑偏的错误。
        let at = src
            .find("\n    fn trigger_sftp_open(&mut self, generation: u64) {")
            .expect("找不到 trigger_sftp_open 的定义");
        let after = &src[at + 1..];
        let body = &after[..after
            .find("\n    }\n")
            .expect("找不到 trigger_sftp_open 的函数结尾")];

        assert!(
            body.contains("let default_remote = tab.content.sftp_default_remote();"),
            "trigger_sftp_open 没从 tab 读 default_remote"
        );
        assert!(
            body.contains("focused_pane_cwd()"),
            "trigger_sftp_open 没读焦点 pane 的当前目录 —— 目录继承会静默失效"
        );

        let call_after = body
            .split("spawn_sftp_open(")
            .nth(1)
            .expect("找不到 spawn_sftp_open 调用");
        let call_args = &call_after[..call_after
            .find(");")
            .expect("找不到 spawn_sftp_open 调用的结尾")];
        assert!(
            call_args.contains("default_remote"),
            "spawn_sftp_open 没收到 default_remote —— F120 的默认目录被静默丢弃"
        );
        assert!(
            call_args.contains("pane_cwd"),
            "spawn_sftp_open 没收到 pane_cwd —— F123 的目录继承被静默丢弃"
        );
    }

    /// ②:文件面板远端栏该开在哪。优先级:焦点 pane 报出来的当前目录 >
    /// F120 配置的默认远端目录 > `None`(交给 `spawn_sftp_open` 里的
    /// `canonicalize(".")` 落回登录目录)。
    ///
    /// **只接受绝对路径**:标题里拿到的可能是 `~/Mullion`,而 openssh 的
    /// `sftp-server` **不展开 `~`** —— 直接拿去 `canonicalize` 会失败,
    /// 面板会停在「取不到登录目录」,比不继承更糟。`~` 那种只用来在标题条上
    /// 显示目录名。
    ///
    /// 自证会变红:把 `files_start_dir` 里 `starts_with('/')` 那个判断删掉
    /// (`~` 用例红);把 `pane_cwd` 那条优先级去掉(第一条红)。
    #[test]
    fn files_start_dir_prefers_the_panes_cwd_but_only_if_absolute() {
        assert_eq!(
            files_start_dir(Some(b"/home/dev/Mullion"), Some("/srv"), None).as_deref(),
            Some("/home/dev/Mullion"),
            "pane 报的目录该压过配置的默认目录"
        );
        // ~ 展开需要已知 home;这里 home 未知(sftp 还没开),该落回配置值。
        assert_eq!(
            files_start_dir(Some(b"~/Mullion"), Some("/srv"), None).as_deref(),
            Some("/srv"),
            "home 未知时 ~ 不展开,sftp-server 不展开它,该落回配置值"
        );
        assert_eq!(
            files_start_dir(None, Some("/srv"), None).as_deref(),
            Some("/srv"),
            "没有 pane 目录时用配置值"
        );
        assert_eq!(files_start_dir(None, None, None), None);
        // 非 UTF-8 的远端路径落回配置值:`spawn_sftp_open` 收 `Option<String>`,
        // 到不了这条路。标题条那边仍会 lossy 显示出来(`dir_leaf`)。
        assert_eq!(
            files_start_dir(Some(b"/tmp/\xff"), Some("/srv"), None).as_deref(),
            Some("/srv")
        );
        // 空切片:`starts_with(b"/")` 为假,同 `~` 一样落回配置值——不是
        // panic 也不是「当成根目录」。
        assert_eq!(
            files_start_dir(Some(b""), Some("/srv"), None).as_deref(),
            Some("/srv")
        );
    }

    /// F123 补缺口:标题里拿到的常常是 `~/Mullion` 这种缩写,而 openssh 的
    /// `sftp-server` **不展开 `~`**。拿 SFTP 的真登录目录(`canonicalize(".")`)
    /// 把它拼成绝对路径,裸 shell 场景就不用配任何东西也能继承目录了。
    ///
    /// 自证会变红:让 `expand_tilde` 无条件返回 `None`(前两条红);
    /// 把 `~user` 那条也当成 `~` 展开(第四条红)。
    #[test]
    fn expand_tilde_uses_the_sftp_login_directory() {
        assert_eq!(
            expand_tilde(b"~", b"/home/dev").as_deref(),
            Some(&b"/home/dev"[..])
        );
        assert_eq!(
            expand_tilde(b"~/Mullion", b"/home/dev").as_deref(),
            Some(&b"/home/dev/Mullion"[..])
        );
        // 已经是绝对路径:不归它管(调用方那一档更优先)。
        assert_eq!(expand_tilde(b"/srv/app", b"/home/dev"), None);
        // `~user` 的语义要查远端的 passwd,我们不知道 —— **不猜**。
        assert_eq!(expand_tilde(b"~foo/x", b"/home/dev"), None);
        assert_eq!(expand_tilde(b"", b"/home/dev"), None);
        // home 自己是根目录时不能拼出 `//x`。
        assert_eq!(expand_tilde(b"~/x", b"/").as_deref(), Some(&b"/x"[..]));
        // home 带尾斜杠同理。
        assert_eq!(
            expand_tilde(b"~/x", b"/home/dev/").as_deref(),
            Some(&b"/home/dev/x"[..])
        );
        // 冗余斜杠出在 **cwd 侧**时也一样 —— 只剥 home 那一侧的话,
        // `~//x` + home=`/` 会拼出正是这里想避开的 `//x`。
        assert_eq!(expand_tilde(b"~//x", b"/").as_deref(), Some(&b"/x"[..]));
        assert_eq!(
            expand_tilde(b"~//x", b"/home/dev").as_deref(),
            Some(&b"/home/dev/x"[..])
        );
    }

    /// 四档优先级:pane 报的绝对路径 > 展开后的 `~` > 配置的默认远端目录 >
    /// `None`(交给调用方落回登录目录)。
    ///
    /// 自证会变红:把 `home` 那一档删掉(第二条落到 `/srv`,红)。
    #[test]
    fn files_start_dir_expands_a_tilde_before_falling_back_to_the_configured_dir() {
        // 绝对路径最优先,home 在不在都一样。
        assert_eq!(
            files_start_dir(Some(b"/home/dev/Mullion"), Some("/srv"), Some(b"/home/dev"))
                .as_deref(),
            Some("/home/dev/Mullion")
        );
        // `~` + 已知 home → 展开。
        assert_eq!(
            files_start_dir(Some(b"~/Mullion"), Some("/srv"), Some(b"/home/dev")).as_deref(),
            Some("/home/dev/Mullion")
        );
        // `~` 但 home 未知(sftp 还没开):不展开、不猜 `/home/<user>`,
        // 落回配置值。
        assert_eq!(
            files_start_dir(Some(b"~/Mullion"), Some("/srv"), None).as_deref(),
            Some("/srv")
        );
        // pane 什么都没报:配置值。
        assert_eq!(
            files_start_dir(None, Some("/srv"), Some(b"/home/dev")).as_deref(),
            Some("/srv")
        );
        // 都没有:None。
        assert_eq!(files_start_dir(None, None, Some(b"/home/dev")), None);
        // 非 UTF-8 展开结果同样进不了 `Option<String>`,落回配置值。
        assert_eq!(
            files_start_dir(Some(b"~/\xff"), Some("/srv"), Some(b"/home/dev")).as_deref(),
            Some("/srv")
        );
        // home 自己不是绝对路径(远端 `canonicalize(".")` 返回了怪东西)时,
        // 展开结果照样不绝对 —— 展开后必须**再**过一遍绝对路径检查。少了那道
        // 检查这两条会把 `relative/x`、`` 当成起始目录发给 sftp-server。
        assert_eq!(
            files_start_dir(Some(b"~/x"), Some("/srv"), Some(b"relative")).as_deref(),
            Some("/srv")
        );
        assert_eq!(
            files_start_dir(Some(b"~"), Some("/srv"), Some(b"")).as_deref(),
            Some("/srv")
        );
    }

    /// `App::sync_files_to_focused_pane` 的判定核心 `sync_target_of`:
    /// 四个早退(有没有属主世代 / sftp 是否已连 / 焦点 pane 有没有报出
    /// 绝对路径)+ 一条正常路径。此前这条链完全埋在 `&mut self` 方法体里
    /// 靠人读代码——把 `has_client` 判断写反、或者把 client 检查和 dir
    /// 计算的顺序换掉,都不会有任何测试变红。
    ///
    /// 自证会变红:
    /// - 把 `has_client` 那个判断取反(`if has_client { return None; }`)——
    ///   `has_client_false_blocks_it_even_with_a_good_cwd` 和
    ///   `happy_path_returns_the_generation_and_the_directory` 都该红。
    /// - 把 `files_start_dir(pane_cwd, None)` 换成无条件接受(丢掉「只接受
    ///   绝对路径」)——`relative_pane_cwd_blocks_it_the_same_way_as_a_missing_one`
    ///   该红。
    /// - 让 `sync_target_of` 无脑返回 `None`——
    ///   `happy_path_returns_the_generation_and_the_directory` 该红。
    #[test]
    fn sync_target_of_covers_all_four_early_returns_and_the_happy_path() {
        // 早退①:没有属主世代——即使 sftp 已连、pane 目录是合法绝对路径,
        // 用户可感知的后果是「侧栏开了却什么都没同步」,不该发生同步。
        assert_eq!(
            sync_target_of(None, true, Some(b"/home/dev/x"), None),
            None,
            "没有属主世代时不该同步——这个分支理论上到不了(`sync_files_to_focused_pane`\
             早就该拿不到 has_client/pane_cwd),但纯函数自己也不能在这种输入下瞎猜一个世代出来"
        );

        // 早退②:sftp 还没连上。用户可感知的后果:面板已经在等 `trigger_sftp_open`
        // 把 sftp 连起来,这时候发 Goto 会打到一条还不存在的连接上。
        assert_eq!(
            sync_target_of(Some(7), false, Some(b"/home/dev/x"), None),
            None,
            "sftp 还没开时不该发 Goto——那条连接还不存在"
        );

        // 早退③:焦点 pane 没报过目录。用户可感知的后果:面板停在原处,
        // 不该凭空跳到某个猜测目录。
        assert_eq!(
            sync_target_of(Some(7), true, None, None),
            None,
            "拿不到 pane 目录时不该同步——面板该停在原处"
        );

        // 早退④:pane 报的是相对路径/`~`,home 未知时不展开。同上,openssh
        // 的 sftp-server 不展开 `~`,发过去只会让面板停在「取不到登录目录」。
        assert_eq!(
            sync_target_of(Some(7), true, Some(b"~/Mullion"), None),
            None,
            "home 未知时 pane 目录不是绝对路径就不该同步——发过去 sftp-server 展不开 `~`"
        );

        // 正常路径:世代 + 已连 + 绝对路径,三者都齐了才同步。
        assert_eq!(
            sync_target_of(Some(7), true, Some(b"/home/dev/x"), None),
            Some((7, "/home/dev/x".to_string())),
            "三个条件都满足时该把 (世代, 目录) 原样吐出来"
        );

        // `home` 真的透传下去了。上面每一条的 `home` 都是 `None`,而 `None` 下
        // 「透传」和「吞掉」行为一模一样 —— 只有这条(`~` + 已知 home)能分辨。
        // 少了它,`files_start_dir(pane_cwd, None, None)` 这种漏传照样全绿,
        // 而登录目录接真值之后 `~` 就永远展不开了。
        assert_eq!(
            sync_target_of(Some(7), true, Some(b"~/x"), Some(b"/home/dev")),
            Some((7, "/home/dev/x".to_string())),
            "home 已知时该透传给 files_start_dir,让 `~` 展开后再同步"
        );
    }

    /// **接线守护**:两个起始目录来源的**失败处置不一样**,不能被抹平。
    ///
    /// pane 报的目录会过期(标题由 tmux 异步报来,目录可能刚被删),打不开就
    /// 退回默认起始目录、面板照常打开;配置的默认远端目录(F120)打不开则必须
    /// 报错 —— 静默忽略用户填的配置是本项目最不想要的失效方式。抹平成「都报错」
    /// 会让 pane 目录一过期文件面板就整个打不开;抹平成「都降级」会让写错的
    /// F120 配置永远不吭声。
    ///
    /// 自证会变红:把 pane 那条的 `Err(e) => log::debug!(…)` 改成 `Err(e) =>
    /// return Err(…)` 之类的直接失败(第二条红);或把配置那条的 `Err` 分支
    /// 也改成退回 home(第三条红)。
    #[test]
    fn only_the_stale_pane_directory_degrades_a_bad_configured_dir_must_surface() {
        let src = include_str!("app.rs");
        let at = src
            .find("\nfn spawn_sftp_open(")
            .expect("找不到 spawn_sftp_open 的定义");
        let after = &src[at + 1..];
        let body = &after[..after
            .find("\n}\n")
            .expect("找不到 spawn_sftp_open 的函数结尾")];
        assert!(
            body.contains("canonicalize(&dot)"),
            "函数体切歪了({} 字节)",
            body.len()
        );
        assert!(
            body.contains("\"pane 报的目录打不开,退回默认起始目录:{e}\""),
            "pane 目录打不开时没降级 —— 目录一过期文件面板就整个打不开"
        );
        assert!(
            body.contains("Err(e) => Err(format!(\"SFTP 已连上,但打不开起始目录:{e}\"))"),
            "配置的默认远端目录打不开时没报错 —— F120 的配置会被静默忽略"
        );
    }

    /// **接线守护**:`accept_sftp_opened` 必须把真登录目录存进标签。
    ///
    /// 不存的话「侧栏已开着、关→开跃迁」那条路(`sync_files_to_focused_pane`)
    /// 拿不到 home,`~/Mullion` 展不开,面板停在原处 —— 而首次打开那条路
    /// 却是好的,现象成了「第一次开对、之后再开都不对」,极难对上原因。
    ///
    /// 自证会变红:把 `set_sftp_home` 那一行删掉。
    #[test]
    fn accept_sftp_opened_remembers_the_login_directory() {
        let src = include_str!("app.rs");
        // 锚点带行首缩进,切到自身闭合括号(不是「下一个 `fn`」)—— 后者会把
        // 下一个函数的文档注释也带进来,注释里出现 `set_sftp_home` 字样就够让
        // 这条守护在实现真丢了那行时照样绿。
        let at = src
            .find("\n    fn accept_sftp_opened(")
            .expect("找不到 accept_sftp_opened 的定义");
        let after = &src[at + 1..];
        let body = &after[..after
            .find("\n    }\n")
            .expect("找不到 accept_sftp_opened 的函数结尾")];
        assert!(
            body.contains("generation"),
            "函数体切歪了({} 字节)",
            body.len()
        );
        assert!(
            body.contains("set_sftp_home("),
            "登录目录没被存下来 —— 侧栏「关→开」跃迁那条路展不开 ~"
        );
    }

    /// **接线守护**:`sync_files_to_focused_pane` 要把存下来的登录目录喂给
    /// `sync_target_of`。传死 `None` 的话这条路永远展不开 `~`。
    ///
    /// 自证会变红:把 `sync_target_of` 的第四个实参改成字面量 `None`。
    #[test]
    fn the_sidebar_sync_feeds_the_login_directory_into_the_predicate() {
        let src = include_str!("app.rs");
        // 锚点带行首缩进,理由同 `trigger_sftp_open_hands_both_ingredients_…`。
        let at = src
            .find("\n    fn sync_files_to_focused_pane(&mut self) {")
            .expect("找不到 sync_files_to_focused_pane 的定义");
        let after = &src[at + 1..];
        let body = &after[..after
            .find("\n    }\n")
            .expect("找不到 sync_files_to_focused_pane 的函数结尾")];
        assert!(body.contains("sftp_home()"), "没读标签存下的登录目录");
        let call = body
            .split("sync_target_of(")
            .nth(1)
            .expect("没调 sync_target_of");
        // 切到**配对**的右括号,不是第一个 —— 实参里带 `.as_deref()` 这种
        // 空括号是惯用写法,按第一个 `)` 切会把实参串截在 `pane_cwd.as_deref(`
        // 处,于是这条守护恒红,逼得生产代码为迁就测试写成怪样子。
        let end = {
            let mut depth = 1usize;
            call.char_indices()
                .find_map(|(i, c)| {
                    match c {
                        '(' => depth += 1,
                        ')' => depth -= 1,
                        _ => {}
                    }
                    (depth == 0).then_some(i)
                })
                .expect("找不到 sync_target_of 调用的结尾")
        };
        let args = &call[..end];
        assert!(
            args.contains("home"),
            "sync_target_of 收到的不是登录目录:{args}"
        );
    }

    /// ②:侧栏从「关」变「开」那一帧,把远端栏带到焦点 pane 报出来的目录。
    ///
    /// 判据必须是「关→开」跃迁,而不是「侧栏当前是开的」这个每帧恒真的条件
    /// ——否则用户在面板里手动点开的目录,会在下一帧就被拽回终端所在目录,
    /// 面板变得没法浏览。
    ///
    /// 判据存在 `App::files_sidebar_was_open` 这个跨帧字段里,而不是
    /// `render_frame` 调用前的帧内局部变量:侧栏开关有两条路——菜单
    /// (`chrome.rs`)在 `render_frame` **内部**改 `self.ui.files_sidebar_open`,
    /// 帧内局部变量能测出跃迁;但热键(`files_hotkey_event`,由
    /// `window_event` 在另一次事件回调里调用)在 `render_frame` **之外**改
    /// 这个标志——等下一帧的重绘块跑到帧内局部变量赋值那一行时,标志早就
    /// 已经是 `true` 了,`open && !was_open` 恒假,Ctrl+Shift+B 开侧栏永远
    /// 同步不到。用跨帧字段、并把判据放在 `render_frame` 调用**之后**,
    /// 两条路径才都能覆盖到。
    ///
    /// **扎的是源码结构**:真正验它要一条活 sftp 连接和真实一帧渲染,这个
    /// 测试容器里造不出来(同上面几条接线守护的限制)。
    ///
    /// 自证会变红:
    /// - 把 `sync_files_to_focused_pane` 整个函数删掉/改名 —— 第一条断言红。
    /// - 把判据改成 `if self.ui.files_sidebar_open {`(去掉跃迁判断)——
    ///   第二条断言红。
    /// - 删掉帧尾 `self.files_sidebar_was_open = self.ui.files_sidebar_open;`
    ///   —— 第三条断言红。
    #[test]
    fn the_files_sidebar_syncs_to_the_terminal_only_on_the_closed_to_open_edge() {
        let src = include_str!("app.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .expect("split 至少给一段");
        assert!(
            prod.len() < src.len(),
            "没能把搜索范围切到 `mod tests` 之前 —— 下面的断言会命中测试自己\
             写的字面量,变成恒绿"
        );

        assert!(
            prod.contains("fn sync_files_to_focused_pane(&mut self)"),
            "缺 sync_files_to_focused_pane —— ② 在侧栏已开着时会静默不生效"
        );
        assert!(
            prod.contains("if self.ui.files_sidebar_open && !self.files_sidebar_was_open {"),
            "同步的判据不是「关→开」跃迁 —— 每帧都同步会把用户在面板里点开的\
             目录反复拽回终端所在目录"
        );
        assert!(
            prod.contains("self.files_sidebar_was_open = self.ui.files_sidebar_open;"),
            "没有在帧尾记下这一帧的开合状态 —— 下一帧判不出跃迁,\
             而且热键那条路(在另一次事件回调里改标志)会永远同步不到"
        );
    }
}
