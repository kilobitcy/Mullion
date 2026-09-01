//! App:winit ApplicationHandler<UserEvent>。持有窗口/GPU/文字层/运行时,以及一列
//! 标签(F36:空 = launcher 态 / 非空 = 终端态,§2.2;每个标签一棵布局树,一棵树
//! 可装多个 pane)。每帧(有活动标签时)对每个 pane「排空 rx → feed emu → 回写
//! PtyWrite(T1)」,GPU present 受帧率(T3)与同步块(T2)双闸。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

/// F157:把 `ui_dirty` 置真,并把**调用点的行号**记进归因表。
///
/// **唯一的置脏入口**。绕开它直接给 `self.ui_dirty` 赋值等于在归因表上开一个洞,
/// 而洞的症状是「剖面里少了一行、看起来一切正常」——守护测试
/// `tests::every_ui_dirty_set_site_goes_through_the_attribution_macro` 钉死这一条。
///
/// **是宏而不是 `App` 的方法**:有些置脏点位于 `self.active` 的可变借用作用域里
/// (egui 事件分流那一段),那里调不了任何 `&mut self` 方法(E0499);宏展开成
/// 一句普通的字段赋值,两种上下文都能用。`line!()` 在 `macro_rules!` 体内
/// 展开成**调用点**的行号,正是归因要的东西。
///
/// 开销:一句赋值 + 最坏 8 次 relaxed load + 1 次 CAS,与 `diag::mark` 同量级(T3)。
macro_rules! mark_ui_dirty {
    ($slot:expr) => {{
        $slot = true;
        crate::diag::note_ui_dirty(line!());
    }};
}

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
    /// 为 `true` 时 `pty` 恒 `None`(SFTP 节点不开 PTY,`spawn_connect` 内部
    /// 直接跳过 `open_pty`);为 `false` 时 `pty` 恒 `Some`(`open_pty` 失败会走
    /// `ConnectErr`,不会发一个「两者皆无」的 `ConnectOk`)。
    ///
    /// F205:`dial` 是这次拨号的票号。**「这次连上的是哪条会话」只认它** ——
    /// 从前靠 `App` 上一个单槽记,两条拨号同时在途时后者会把前者盖掉,详见
    /// `shell::dial_ledger` 的模块文档。
    ConnectOk {
        dial: crate::shell::dial_ledger::DialId,
        handle: Arc<SshConnection>,
        wants_sftp: bool,
        pty: Option<(SshSession, Receiver<Vec<u8>>)>,
    },
    /// 异步 connect 失败,已格式化的可操作错误(F6 分类由 `session::connect` 内部给)。
    ///
    /// F205:同样带票号 —— 失败也要把台账上那张票摘掉,否则票里那份
    /// `SshConfig` 永远不释放,而且台账只涨不落。
    ConnectErr(crate::shell::dial_ledger::DialId, String),
    /// 私钥文件对话框结束。`None` = 用户取消/对话框失败——也要回送,否则
    /// `PickerBusy::key` 永远清不掉,以后再点「选择…」就没反应了。
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
    /// F128:一次断线重连拨通了。**跟 `PaneRehosted` 分开**:那条的语义是
    /// 「把 pane 改挂到另一台机器」(要重建 emulator),这条是「同一台机器
    /// 换一条 channel」(必须保留 emulator)。挤在一起只能靠运行时标志判别,
    /// 而走错的后果是把用户断线前那一屏抹掉。
    PaneReconnected {
        generation: u64,
        /// 这条连接原来的 `host_ix`——重连成功后 `ws.hosts` 会 push 一条新的,
        /// 挂在旧 ix 上的**每一块** pane 都要跟着换过去(adr-009:一条连接
        /// 多块 pane)。
        host_ix: usize,
        handle: Arc<SshConnection>,
        /// 每块 pane 一条新 channel,顺序与 `panes` 对齐。
        channels: Vec<(PaneId, SshSession, Receiver<Vec<u8>>)>,
    },
    /// F128:一次重连没拨通。`attempt` 是刚失败的这次的序号,决定下次等多久
    /// (以及要不要放弃),判据在 `crate::reconnect`。
    PaneReconnectErr {
        generation: u64,
        host_ix: usize,
        attempt: u32,
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
        /// F132:这条 channel 开在哪台上。发起时是什么,回来时还是什么 ——
        /// 期间用户可能已经换了焦点分屏,不能在收到时现算。
        host_ix: Option<usize>,
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
    /// F142:一次 `getent` 查完了(属主列要显示的用户名/组名)。
    ///
    /// **失败也要送回来**(`stdout: None`):发出去那一刻这批 id 已经记进了
    /// 负缓存(`OwnerNames::take_missing`),不回送的话它们永远不会被再问一次
    /// —— 一次网络抖动就把这台机器的属主列钉死在数字上。接收方按 `query`
    /// 撤回负缓存。
    ///
    /// 送的是**原始 stdout 而不是解析结果**:解析是纯逻辑,放在窗口线程上
    /// 跑没有代价,而且能跟 `files::owners` 的单测共用同一条代码路径。
    OwnerNames {
        generation: u64,
        query: crate::files::owners::Query,
        stdout: Option<Vec<u8>>,
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
    /// F209:一张截图传完了。`Ok` 是**远端绝对路径**,由接收方打进那块 pane
    /// 的输入行;`Err` 是已经格式化好的原因,只弹提示。
    ///
    /// **必须同时带 `generation` 和 `pane`**,不能到时候取「当前活动标签的
    /// 焦点分屏」:高延迟链路上传一张 3MB 的图要好几秒,用户完全可能已经切
    /// 标签、切分屏了 —— 那时把路径打进别的 shell 里,是一句凭空冒出来的
    /// 乱码(T11 同族:判据要跟着字节走,不是跟着「此刻谁在焦点」走)。
    ShotUploaded {
        generation: u64,
        pane: PaneId,
        result: Result<String, String>,
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

/// F163:发完 attach 之后再宽限这么久才下「没接上」的结论。
///
/// 时长是**猜的**,要人工调:太短会在慢链路上误报,太长则提示来得毫无意义
/// (用户早就自己看出来了)。
const ATTACH_CHECK_GRACE: Duration = Duration::from_secs(4);

/// F163:一条在途的 attach 校验。
struct AttachCheck {
    generation: u64,
    pane: PaneId,
    /// 期望远端报上来的会话名。
    name: String,
    /// 宽限期的起点。**`None` = attach 字节还没发完,不许下任何结论。**
    ///
    /// `automation::run` 第一段是 `tokio::select!` 等首字节,最长能等
    /// `ready_timeout_ms`(默认 15 秒),之后才轮到 `write_scheduled` 睡
    /// `initial_delay_ms` 并真的发出 attach 字节 —— 起算点必须是「发完」
    /// 而不是「打算发」,否则高延迟代理链路上 `tmux attach` 还没发出去,
    /// 校验就已经判「没接上」了。由 `AutomationDone` 上膛,
    /// 见 `App::arm_or_drop_attach_check`。
    deadline: Option<Instant>,
}

/// F163 的判决。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttachVerdict {
    /// 接上了(远端标题报的会话名 = 期望的那个)。
    Ok,
    /// 还没到期,别下结论。
    Waiting,
    /// 到期了还没报上来 / 报的是别的会话。
    Failed,
}

/// F163:这一刻该给这条校验什么判决。纯函数 —— 「几秒算没接上」这条判据
/// 本身要能脱离事件循环单测。
///
/// `measured` = `PaneState::tmux`(F123/F124 远端标题上报的实测值)。
/// attach 成功之后 tmux 必然按 F124 配的 `set-titles-string` 发标题,
/// 所以「接回来了没有」本来就是可观测的 —— 这是实测那条腿的第二个用途。
fn attach_check_verdict(measured: Option<&str>, want: &str, expired: bool) -> AttachVerdict {
    if measured == Some(want) {
        return AttachVerdict::Ok;
    }
    if expired {
        AttachVerdict::Failed
    } else {
        AttachVerdict::Waiting
    }
}

/// F163/D4 的边界:这条校验**依赖 F124 在跑**。
///
/// 用户把 `tmux_bootstrap` 开关关掉时,远端不设标题,attach 成功也不会有会话名
/// 报上来 —— 校验会恒误报「没接上」,而那条误报比不校验糟得多(用户会去查一个
/// 根本不存在的问题)。开关关着就跳过:attach 照发,只是不许下失败结论。
fn should_check_attach(title_reporting_on: bool) -> bool {
    title_reporting_on
}

/// F163:`drive_attach_checks` 的可测核心。拆成收显式 `tabs` 的自由函数
/// (同 `active_ws_of` 一类 `_of` 抽取的理由:`App::new` 要 `EventLoopProxy`,
/// 测试容器里造不出真的 `App`,但真实构造的 `Tabs<TabContent>` 可以),
/// 让「push 进队列 → 驱动 → notice 落到 `PaneState` 上」这条接线能被真的
/// 跑一遍,不只是源码切片。
///
/// **遍历的是 `checks` 不是活动标签**:每条自带世代号,校验途中用户完全
/// 可能切到别的标签去(那条「`drive_*` 每帧驱动函数必须遍历全部标签」的
/// 同源教训)。
///
/// 返回:还没到期的那些(调用方原样放回 `App::attach_checks`)+ 是否有
/// pane 的 `notice` 被真的改动过(调用方据此决定打不打脏,避免空打)。
fn drive_attach_checks_of(
    tabs: &mut Tabs<TabContent>,
    checks: Vec<AttachCheck>,
    now: Instant,
) -> (Vec<AttachCheck>, bool) {
    let mut done: Vec<(u64, PaneId, String, AttachVerdict)> = Vec::new();
    let mut pending = checks;
    pending.retain(|c| {
        let Some(tab) = tabs.by_generation(c.generation) else {
            // 属主标签已经不在了 —— 直接丢弃,不下任何结论,也不打 warn /
            // 挂 notice(标签都没了,没有 pane 可挂)。
            //
            // 上膛的唯一来源是 `AutomationDone`,而 `wind_down`(关整个标签)
            // 对在途的自动化 task 直接 `abort()`:`automation::run` 的
            // future 在下一个 `.await` 点被硬取消,跟在 `.await` 之后的
            // `proxy.send_event(AutomationDone(..))` 永远不会执行 —— 这条
            // 校验的 `deadline` 会恒 `None`、`verdict` 恒 `Waiting`,每帧被
            // `mem::take` 拷贝 + 遍历一次却永远出不去队列。
            //
            // 收口放在这里而不是各个关闭点(`wind_down` 目前有 5 处调用):
            // 移除标签的路径不止「关整个标签」一条,还有 rehost 换世代等,
            // 列举式清理今天对、下次谁再加一条移除路径就又漏,而且漏了
            // 完全静默。
            return false;
        };
        let measured = tab
            .content
            .as_terminal()
            .and_then(|t| t.ws.pane(c.pane))
            .and_then(|p| p.tmux.clone());
        // `deadline` 为 `None` 时 attach 字节还没发完,恒 `Waiting`
        // (见 `AttachCheck::deadline` 的文档)。
        let expired = c.deadline.is_some_and(|d| now >= d);
        let v = attach_check_verdict(measured.as_deref(), &c.name, expired);
        if v == AttachVerdict::Waiting {
            return true;
        }
        done.push((c.generation, c.pane, c.name.clone(), v));
        false
    });
    let mut dirty = false;
    for (generation, pane, name, verdict) in done {
        if finish_attach_check(tabs, generation, pane, &name, verdict) {
            dirty = true;
        }
    }
    (pending, dirty)
}

/// F163:一条校验有结论了。返回这块 pane 的 `notice` 是否被真的改动过。
///
/// **D8:失败之后不补跑配置的登录后命令。** 结论是在「发完等几秒」之后
/// 才有的,那时用户很可能已经在这块 pane 里敲东西了 —— 延迟补发字节是
/// 本项目最危险的一类行为(同 F156-c 只在 pane 刚建立时注入 OSC 7 的理由)。
/// 停在裸 shell,pane 上挂提示,下一步交给用户。
fn finish_attach_check(
    tabs: &mut Tabs<TabContent>,
    generation: u64,
    pane: PaneId,
    name: &str,
    verdict: AttachVerdict,
) -> bool {
    // 修复4:先确认 pane 还在,再打 warn / 挂提示 —— 用户在校验到期前就
    // 关掉了这块 pane(或整个标签),日志不该说「没接回,多半不在远端了」,
    // 那会把排查方向带偏。
    let Some(p) = tabs
        .by_generation_mut(generation)
        .and_then(|t| t.content.as_terminal_mut())
        .and_then(|t| t.ws.pane_mut(pane))
    else {
        return false;
    };
    if verdict == AttachVerdict::Ok {
        // 接上了 —— 上一轮 attach 失败挂的那句「已不存在」已经过期,摘掉,
        // 不然它会永久挂在一块现在完全正常的 pane 的标题条上。
        if p.notice.is_some() {
            p.notice = None;
            return true;
        }
        return false;
    }
    log::warn!(
        target: "mullion",
        "pane {} 没能接回 tmux 会话 {name} —— 它多半已经不在远端了",
        pane.0
    );
    // D4:挂在这块 pane 上,**不弹窗** —— 多块 pane 都失败时会连弹好几次。
    p.notice = Some(format!("当初的会话 {name} 已不存在"));
    true
}

/// F188:这次拨号是「用户点标题条换节点」还是「F162 恢复现场的首次挂载」。
///
/// 两者复用同一条拨号链路(D10:不新写第二条,第二条一定会漏掉防连点的闸),
/// 但**落地语义相反**,而且分不清的后果都是静默的:
///
/// - 首次挂载的那块叶子是 `apply_saved_tree` 刚分配的 id,树上有位、
///   **没有 `PaneState`**。按换节点那套「找不到 `PaneState` 就放弃」处理,
///   已经拨通的连接会被原地丢掉,那一格永远停在「N · 连接中…」。
/// - 焦点:换节点是用户刚亲手指定的,焦点该跟过去(F156-b);恢复现场是
///   后台批量拨号,抢焦点会把 `apply_saved_tree` 刚按 `focus_leaf` 摆好的
///   焦点顶掉 —— 最后落在「碰巧最后一个拨通」的那块 pane 上。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RehostKind {
    /// 用户在 pane 标题条上点的「换节点」。
    UserPicked,
    /// F162 串行恢复队列:这块叶子还没有 `PaneState`,这是它的第一条 channel。
    RestoreFirstMount,
}

/// 一次在途的换节点。真正换挂要等 `PaneRehosted` 抵达,而那条事件带不动
/// 这些东西 —— 标题条要的名字/地址、外观要的 `SessionId`、以及新节点的登录后
/// 命令,全都得在**用户选中那一帧**算好存下来。
///
/// 理由同 `PendingAutomationState::template`:拨号是真实网络往返(高延迟代理链路
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
    /// F128:拨这台**新**机器用的参数,随 `HostConn` 一起存进 `ws.hosts`。
    /// 不带的话,换过节点的那条连接断线时只剩标签级 `last_cfg` 可取,
    /// 而那是**最初**那台机器的(见 `HostConn::cfg` 的文档)。
    cfg: SshConfig,
    /// F188:发起这次拨号的是谁。**在发起那一帧定死**,不能等事件回来再猜
    /// ——「有没有 `PaneState`」不是判据:恢复途中用户完全可能对着同一块
    /// 已经摆成占位态(D6)的 pane 手点换节点。
    kind: RehostKind,
}

/// F141:断线重连时,把**哪块 pane** 接回**哪个 tmux 会话**。
///
/// 在 `ConnectOk`(自动化真的起跑的那一帧)记下,判据与计划生成同源
/// (`mullion_store::tmux_session_name` 就是 `build_plan` tmux 分支的判据本身)
/// —— 记岔了的后果不是「没接回来」,而是「接到了另一个会话上」。
#[derive(Debug, Clone)]
struct TmuxAttach {
    /// 建标签的那块 pane(`PaneId(1)`,见 `Workspace::new`)。分屏出来的、
    /// 换过节点的都不是它。
    pane: PaneId,
    /// 它当时挂在 `ws.hosts` 的第几台上。**判据里必须带上它**:用户把这块
    /// pane「换节点」搬到第二台机器之后,pane id 不变、`host_ix` 变了 ——
    /// 只认 pane id 的话,新机器断线重连时会把**上一台**机器的 tmux 会话名
    /// 发过去,在一台根本没有那个会话的机器上凭空新建一个同名会话。
    host_ix: usize,
    /// 当初真的 attach 上去的那个会话名(已 sanitize)。**不是「现在配置里
    /// 写的那个」**:断线到重连之间用户完全可能去改会话名,按新名字 attach
    /// 会接到一个空会话,而他要的那个还在远端挂着。
    session_name: String,
}

impl TmuxAttach {
    /// 这次重连回来的 `(pane, host_ix)` 是不是当初 attach 了 tmux 的那一块。
    ///
    /// 抽成方法只为了能脱离 `App`/事件循环单测(`App` 在无头环境构造不出来,
    /// 同 `automation::should_cancel_on_status` 的理由)。
    fn matches(&self, pane: PaneId, host_ix: usize) -> bool {
        self.pane == pane && self.host_ix == host_ix
    }
}

/// F141:`ConnectOk` 那一刻该不该记下「重连时接回哪个 tmux 会话」。
/// `None` = 这次连接没走 tmux,重连时全部 pane 都按分屏那套处理。
///
/// 抽成纯函数(而不是在 `ConnectOk` 分支里就地写)只为了能单测 —— 那条分支
/// 要一个真的 `App` + 一条真的 SSH 连接才跑得起来,而这里定死的三件事
/// (哪块 pane、哪台 host、哪个会话名)全都是「错了也看不出来、直到某天
/// 重连接到别的会话上」的那类。
fn tmux_attach_for_connect(
    tpl: Option<&mullion_store::ResolvedAutomation>,
    fallback_name: Option<&str>,
) -> Option<TmuxAttach> {
    Some(TmuxAttach {
        // 建标签的那一块 —— 与下面 `on_pane_ready(.., PaneId(1), ..)`
        // 是同一块 pane(`Workspace::new` 的第一块)。
        pane: PaneId(1),
        // 这个 `ws` 是刚建的,`hosts` 里只有这一条(同 `PaneOpened` 里硬编的
        // `host_ix: 0`)。
        host_ix: 0,
        session_name: mullion_store::tmux_session_name(tpl?, fallback_name?)?,
    })
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
    /// F141:这条连接上「当初 attach 了 tmux 的那块 pane」。`None` = 这个标签
    /// 压根没走 tmux(没配 / 关了 / 会话名为空 / 用户「跳过一次」)。
    ///
    /// F161:断线重连时它**不再是判据,只是回落名**——真值源换成了实测
    /// (`PaneState::tmux`,`reattach_pane` 刻意保留下来的那份)。任何一块
    /// `p.tmux` 非空的 pane 都会被接回,不再局限于「当初 attach 的那一块」;
    /// 这个字段只在某块 pane 实测不到 tmux 名时才顶上去当回落(`matches`
    /// 保证只回落给当初那块、那台机器,不会用错地方)。多块 pane 实测到
    /// 同一个会话名的场景(用户故意开两块镜像同一个 tmux)由
    /// `restore_plan::detach_flags` 去重,不在这里处理。
    tmux_attach: Option<TmuxAttach>,
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
    /// F132:`sftp` 这条 channel 开在 `ws.hosts` 的哪一台上。`None` = 还没开过。
    ///
    /// 用户用「换节点」把某块分屏挪到第二台机器之后,`hosts` 里就有两台,
    /// 而侧栏只有一条 channel。不记归属的话,侧栏连的是第一台、目录却来自
    /// 焦点分屏(第二台)——**路径对了、机器错了**,一次看不出错的误操作。
    sftp_host_ix: Option<usize>,
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
    /// F209:这个标签对应会话配置的「截图上传目录」。`None` = 用
    /// `shot::DEFAULT_DIR`。取值时机同 `sftp_default_remote`(建标签时读一次)。
    sftp_screenshot_dir: Option<String>,
    /// F123:这条 sftp 连接的**真登录目录**(`canonicalize(".")` 的结果)。
    /// `None` = sftp 还没开好。用来把标题里报的 `~/Mullion` 展开成绝对路径
    /// (`sftp-server` 不展开 `~`)。
    ///
    /// **不是「面板当前目录」**:那个在 `files.remote.cwd` 里,会随用户浏览
    /// 移动;这个在同一次 sftp 连接内不变(重连会被新连接的值覆盖)。
    sftp_home: Option<mullion_ssh::sftp::RemotePath>,
    /// F128:这个标签在途的重连任务(`spawn_reconnect` 每发起一次拨号存一个)。
    /// **必须在 `wind_down` 里一并 abort**——理由同 `automation`/`sftp_tasks`:
    /// 那个 task 在退避 `sleep` 或 `establish` 握手里挂着,只 drop `TerminalTab`
    /// 完全收不了口。`establish` 内部没有超时包裹(同 `SftpClient::open`),
    /// 高延迟代理链路黑洞时可能挂很久;用户关标签之后它还会拨完号、做完一整
    /// 套认证(远端多一条登录记录),然后才因为 `by_generation_mut` 查不到
    /// 属主标签而把结果丢掉——白拨一次。
    ///
    /// 每次 `push` 前先 `retain` 掉已经跑完的,稳态下不会无界增长(同
    /// `sftp_tasks`)。
    reconnect_tasks: Vec<tokio::task::JoinHandle<()>>,
    /// F160/F161:恢复出来、**还没连上**的那些叶子分别是什么身份、attach 该不该
    /// 带 `-d`。一块 pane 连上并把 attach 发出去之后,它这一条就被取走
    /// (`on_pane_ready`)—— 此后身份改由运行时实测(设计 5.2②)。
    ///
    /// 落在标签上而不是 `Workspace` 上:它是「上次盘上那份」,而 `Workspace`
    /// 不该知道有磁盘这回事(架构不变量)。
    leaf_wanted: Vec<(PaneId, crate::shell::layout_snapshot::LeafIdentity)>,
    /// F161:这块 pane 下一次「就绪」时,attach 要不要带 `-d`(D5)。
    /// 与 `leaf_wanted` 同进同退,拆成两个表只是因为前者要参与落盘、后者不要。
    leaf_detach: Vec<(PaneId, bool)>,
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
    /// F162:已连上的那块 pane 该落在第几个叶子位上(设计 5.2①)。
    main_leaf: usize,
    /// F160:每个叶子的身份,**按前序**。连上之后照它给每个叶子分派
    /// (同机器 / 另拨一台 / 占位),并在 pane 连上之前替它们保管身份
    /// (设计 5.2②)。
    identities: Vec<crate::shell::layout_snapshot::LeafIdentity>,
}

/// F153:恢复现场之后正在自动串行拨号。`None` = 没在自动拨。
///
/// **一条接一条,不并发**:F37 §1 否掉自动重连的理由是「别让高延迟代理链路上
/// 同时挤一堆握手」——那条理由否的是并发,不是自动。串行既满足「恢复完就能用」,
/// 也不违反它。
#[derive(Debug, Default)]
struct AutoDial {
    /// 已经试过的标签(不管成没成)。**不能省**:失败那条的 `dialing` 会被
    /// `ConnectErr` 复位,「第一个未 dialing 的占位标签」判据会把它反复选中,
    /// 队列在一条连不上的会话上原地打转。
    tried: Vec<shell::tabs::TabId>,
    ok: usize,
    err: usize,
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

    /// F132:焦点分屏挂在 `ws.hosts` 的哪一台上。SFTP 节点标签/占位标签
    /// 没有终端,恒 `None`。
    fn focused_pane_host_ix(&self) -> Option<usize> {
        self.as_terminal()
            .and_then(|t| t.ws.focused())
            .map(|p| p.host_ix)
    }

    /// F132:这条 sftp channel **记录在案**开在哪台机器上(`accept_sftp_opened`
    /// 写入)。`Files` 恒 `None`——它的连接独占,`sftp_connection_for` 不看
    /// 这个值。
    ///
    /// 用于取跟已开 sftp client **同一台**的 `SshConnection`(删除的 exec
    /// 快路径 / 传输 worker 各自开的 channel)——不能改用
    /// `focused_pane_host_ix()`:焦点分屏随时可能已经换到别的机器,那样会让
    /// `client`(host A 的 sftp 会话)和 `conn`(host B 的连接)错配到两台
    /// 不同的机器上。
    fn sftp_host_ix(&self) -> Option<usize> {
        self.as_terminal().and_then(|t| t.sftp_host_ix)
    }

    /// D6/F132:这个标签的 sftp 该蹭哪条连接。
    ///
    /// `host_ix` = 要开在哪台上(`None` 或越界时落回 `hosts[0]`,也就是
    /// 这个标签的主连接)。`Files` 宿主独占自己那条(ADR-010),不看 `host_ix`。
    ///
    /// `hosts[ix]` 在断线重连时是**就地替换** `handle`(见
    /// `UserEvent::PaneReconnected`),所以重连之后这里取到的仍是活的那条。
    fn sftp_connection_for(&self, host_ix: Option<usize>) -> Option<Arc<SshConnection>> {
        match self {
            TabContent::Terminal(t) => {
                let ix = host_ix.unwrap_or(0);
                if host_ix.is_some() && ix >= t.ws.hosts.len() {
                    // 目前到不了:`hosts` 只增不减(换节点 push、重连原地换
                    // `handle`),记下的下标不会腐坏。但真到了这儿,回落 hosts[0]
                    // 就是**原样重现**这条改动要修的 bug(路径对了、机器错了),
                    // 而且悄无声息。留个信号,别让它下次以静默的形式回来。
                    log::warn!(
                        "sftp host_ix {ix} 越界(hosts 只有 {} 台),回落到第一台",
                        t.ws.hosts.len()
                    );
                }
                t.ws.hosts
                    .get(ix)
                    .or_else(|| t.ws.hosts.first())
                    .map(|h| h.handle.clone())
            }
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

/// `App::files_path_editing` 的纯逻辑核心(F131),理由同上 —— 拆出来是为了
/// 能用真实构造的 `Tabs<TabContent>` 单测「path_edit 被置上 → 判定为编辑态」,
/// 不需要一个真的 `App`(`App::new` 要 `EventLoopProxy`,测试容器里造不出来)。
fn files_path_editing_of(tabs: &Tabs<TabContent>, sidebar_open: bool) -> bool {
    files_owner_generation_of(tabs, sidebar_open)
        .and_then(|g| tabs.by_generation(g))
        .and_then(|t| t.content.files_panel())
        .is_some_and(|f| f.remote.path_edit.is_some() || f.local.path_edit.is_some())
}

/// `files_renaming` 的纯逻辑核心,理由同 `files_path_editing_of`。
///
/// **只看远端栏**:本地栏根本进不了改名编辑态(设计 D5)。
fn files_renaming_of(tabs: &Tabs<TabContent>, sidebar_open: bool) -> bool {
    files_owner_generation_of(tabs, sidebar_open)
        .and_then(|g| tabs.by_generation(g))
        .and_then(|t| t.content.files_panel())
        .is_some_and(|f| f.remote.rename_edit.is_some())
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

/// F149:这次 IME 事件该不该落到终端。
///
/// **判据直接复用 `route_focused`,不另起一套。** IME 就是键盘输入的一种,
/// 只是走了 `WindowEvent::Ime` 这条另外的路;两套判据迟早会在维护里分叉,
/// 而分叉的后果是「某些情况下中文又漏进远端 shell」这种间歇性、极难查的故障。
///
/// 抽成不依赖 `App` 的自由函数是为了能单测 —— `App` 在无头环境里造不出来。
fn ime_goes_to_terminal_of(
    focus: shell::input_route::Focus,
    modal_open: bool,
    egui_wants_keyboard: bool,
) -> bool {
    matches!(
        shell::input_route::route_focused(
            focus,
            modal_open,
            egui_wants_keyboard,
            false,
            shell::input_route::InputKind::Keyboard,
        ),
        shell::input_route::Route::Terminal
    )
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
    let mut buf = vec![0u8; crate::profile::XFER_CHUNK as usize];
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

/// F160:一个叶子该往盘上写什么身份(设计 5.2②)。
///
/// 两条来源,优先级不能反:
/// - **已连上**的 pane(`host_pending == false`)→ 现量:会话看
///   `hosts[host_ix].session_id`,tmux 名看 `PaneState::tmux`(F123/F124 远端
///   标题上报的实测值)。D1 的真值源就是它 —— 用户在远端 `tmux switch-client`
///   切过之后,只有实测值是对的。
/// - **还没连上**的叶子(排队 / 占位 / 失败,`host_pending == true`,以及树上
///   有叶子但 `PaneState` 还没挂上的那段空窗期)→ 照抄恢复时从盘上读回来的
///   那份。它的 `host_ix` 指着主叶子那台机器,现量会把身份写成别人的;写空
///   则半路 kill 掉 exe 之后这条身份**永久丢失**。
///
/// 写成自由函数(而不是 `Workspace` 的方法)是因为这条优先级不属于工作区 ——
/// 「盘上那份」住在 `TerminalTab` 上,而 `Workspace` 不该知道有磁盘这回事。
///
/// **不收 `&Workspace`,只收真正需要的两样。** `HostConn` 攥着一条真的 russh
/// `Handle`,无头环境里造不出来 —— 收整个工作区的话这条判据就只能退化成源码
/// 切片断言,而它恰恰是那种「错了也看不出来、直到某天写错身份」的判据。
/// `host_session` 回答 `hosts[ix].session_id`。
fn leaf_identity_of(
    host_session: &dyn Fn(usize) -> Option<SessionId>,
    pane: Option<&crate::shell::workspace::PaneState>,
    wanted: &[(PaneId, crate::shell::layout_snapshot::LeafIdentity)],
    id: PaneId,
) -> crate::shell::layout_snapshot::LeafIdentity {
    use crate::shell::layout_snapshot::LeafIdentity;
    if let Some(p) = pane {
        if !p.host_pending {
            return LeafIdentity {
                session_id: host_session(p.host_ix),
                tmux: p.tmux.clone(),
            };
        }
    }
    wanted
        .iter()
        .find(|(pid, _)| *pid == id)
        .map(|(_, w)| w.clone())
        .unwrap_or_default()
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
                // F160:每个叶子写它**自己**那块 pane 的身份 —— 换过节点之后
                // `ws.hosts` 里有两台机器,只写标签级那一个会让恢复时所有 pane
                // 一起拨向第一台(spec §1.1 症状②)。
                tree: snap::to_entries(t.ws.tree(), &|id| {
                    leaf_identity_of(
                        &|ix| t.ws.hosts.get(ix).and_then(|h| h.session_id),
                        t.ws.pane(id),
                        &t.leaf_wanted,
                        id,
                    )
                }),
            },
            // D1:SFTP 节点标签没有分屏树 —— 恒一个叶子。它没有 tmux,身份就是
            // 标签自己那条会话。
            (TabContent::Files(_), Some(session_id)) => SavedTab {
                kind: SavedTabKind::Files,
                session_id,
                title: tab.title.clone(),
                focus_leaf: 0,
                tree: vec![SavedNodeEntry::leaf_with(Some(session_id), None)],
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

/// 新建一格 pane 的 VT 仿真器。**三个注入点必须全走这里。**
///
/// 网格给 80×24 占位,真实尺寸由下一帧 `apply_geometry` 校准(T4)。
///
/// 两件事过去是分头写在三处的:主题底色(F80 §3.2「三处同源」的第三处)
/// 和 scrollback(F17)。前者已经踩过一次(漏了会让终端底色跟 clear 色失配),
/// 后者更隐蔽 —— 漏掉某一处的表现是「分屏出来的 pane 回溯深度跟主 pane 不
/// 一样」,几乎没人会往配置注入上想。
fn new_pane_emulator(scrollback: usize) -> mullion_term::emulator::Emulator {
    let mut emulator = mullion_term::emulator::Emulator::with_history(80, 24, scrollback);
    let d = theme::term_default_colors(&MULLION_DARK);
    emulator.set_default_colors(d.fg, d.bg);
    emulator
}

/// F17:一条会话解析后的回溯行数。
///
/// `None`(快速连接)/ store 不可用 / 会话已被删,一律落回 store 的内置默认 ——
/// **不另起一套默认值**,否则「未分组会话」在配置页看到的数字和终端里真正
/// 生效的数字会对不上。
///
/// `u32 → usize` 的上界由 `Emulator` 按字节预算兜底,这里不重复夹。
fn resolved_scrollback(
    store: Option<&shell::store::SessionStore>,
    session_id: Option<SessionId>,
) -> usize {
    let v = session_id
        .zip(store)
        .and_then(|(id, s)| s.resolved(id).ok())
        .map_or(mullion_store::DEFAULT_SCROLLBACK, |c| c.scrollback);
    v as usize
}

/// F161/修复2:`take_attach_intent` 判定该按**哪台机器**的自动化设置来。
///
/// `leaf_wanted` 记录的 `session_id` 是这块叶子**自己那条会话**,而不是标签级
/// `automation_template`(那是主叶子那次连接写下的、终身复用的一份快照)。
/// 恢复出来的 Dial 叶子连的是另一台机器 —— 直接用主叶子的模板会让用户对
/// 那台机器明确关掉的自动化被主叶子的设置覆盖掉(反过来也一样:该发的
/// attach 被静默跳过)。
///
/// 边界:
/// - `session_id` 为 `None`(老档案 / 回落到标签会话):没有自己的会话可查,
///   回落到 `fallback`(即 `automation_template`)—— 分屏新开、普通连接那条
///   路径仍然要靠它。
/// - `store` 拿不到 / 那条会话已被删:同样回落到 `fallback`,不 `unwrap`。
///   这类叶子理论上是 Orphan(D3),走不到 `take_attach_intent`,但代码上仍
///   要有确定行为。
fn automation_for_leaf(
    store: Option<&shell::store::SessionStore>,
    session_id: Option<SessionId>,
    fallback: Option<mullion_store::ResolvedAutomation>,
) -> Option<mullion_store::ResolvedAutomation> {
    session_id
        .and_then(|id| store.and_then(|s| s.resolved(id).ok()))
        .map(|c| c.automation)
        .or(fallback)
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

/// F153:自动串行拨号该轮到哪个占位标签。`None` = 没有下一条了。
///
/// 自由函数,理由同 `replace_target`:`App` 要 `EventLoopProxy`,单测里造不出来。
fn next_auto_dial(
    tabs: &shell::tabs::Tabs<TabContent>,
    tried: &[shell::tabs::TabId],
) -> Option<shell::tabs::TabId> {
    tabs.iter().find_map(|t| match &t.content {
        TabContent::Restored(_) if !tried.contains(&t.id) => Some(t.id),
        _ => None,
    })
}

/// F162/D10:串行拨号队列该不该发起下一条。`in_flight` = 上一条还在途中。
///
/// 抽成自由函数,理由同 `next_auto_dial`:`App` 要 `EventLoopProxy`,单测里
/// 造不出来,而「不许并发」这条性质**必须**测得动 —— 破了它的现象是屏幕上
/// 同时叠着三个密码框,而这在无头环境里一个断言都写不出来。
fn take_next_restore_dial(
    queue: &mut std::collections::VecDeque<(u64, PaneId, SessionId)>,
    in_flight: bool,
) -> Option<(u64, PaneId, SessionId)> {
    if in_flight {
        return None;
    }
    queue.pop_front()
}

/// F161:这次连接该不该把自动化模板留给标签(供分屏 / 重连 / 恢复现场复用)。
///
/// 判据只有一条:**用户有没有明确跳过这一次**。刻意不看「这次有没有计划要
/// 跑」—— 用户是在远端手敲 `tt web01` 进 tmux 的,他的会话配置里一条登录后
/// 命令都没有,计划自然为空;而恢复现场按实测名算 attach 全靠这份模板。
/// 两者绑在一起的现象是「重启后一块 pane 都没接回来」,且完全静默 ——
/// 这正是本切片要修的那个症状。
fn tab_keeps_template(_has_plan: bool, user_skipped: bool) -> bool {
    !user_skipped
}

/// F153:自动串行拨号收尾那条 toast。抽成纯函数 —— 文案是这条路径上唯一
/// 有分支的东西,跑一整轮拨号去测它是拿最贵的手段测最便宜的。
fn auto_dial_summary(ok: usize, err: usize) -> String {
    if err == 0 {
        format!("已自动连上 {ok} 个标签")
    } else {
        format!("{ok} 条已连接,{err} 条失败(点「重连」可再试)")
    }
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
            // F128:在途的重连任务同理——见 `TerminalTab::reconnect_tasks` 的文档。
            // 不 abort 的话,用户关标签这一刻若正巧有一次拨号在途,那个任务会把
            // 整套认证做完(远端多一条登录记录)才发现属主标签已经没了。
            for task in t.reconnect_tasks {
                task.abort();
            }
            // F140:**显式**关掉每块 pane 的 channel,然后 `t.ws` 才 drop。
            // 光靠 drop 关不掉 —— russh 0.54.5 的 `ChannelWriteHalf` 没有
            // `Drop` 实现(这一行原本的注释写反了)。不关的话,关掉一个开了
            // 4 块分屏的标签就在远端留下 4 个挂着的 shell。
            t.ws.close_all_panes();
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
    /// F159:上一次**真正提交给 GPU** 的那一帧的整帧指纹。
    ///
    /// `None` = 没有可比对的上一帧(首帧,或 surface 刚被重新 configure ——
    /// 那之后交换链内容未定义,拿旧基准比会让画面停在更早的一帧上)。
    last_frame_fp: Option<crate::frame_fp::FrameFp>,
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
    /// F205:**在途**拨号的台账。发起连接的那一刻还没有标签可放,随行数据
    /// 只能寄存在 `App` 上;从前寄存成几个单槽,于是「一次只许连一个」成了
    /// 一条谁也没写、也没人守的隐含前提 —— 而 `spawn_connect` 从来没有闸。
    /// 见 `shell::dial_ledger` 的模块文档。
    dials: crate::shell::dial_ledger::DialLedger<DialTicket>,
    /// egui UI 侧状态(菜单/状态栏/弹窗/中央区像素),与连接状态解耦(Task 4)。
    ui: crate::ui::UiState,
    /// 会话保险库(Task 6)。`resumed` 末尾打开;keyring/库打开失败时留 `None`,
    /// 会话功能优雅禁用而非 panic/exit(待定 G),错误记 `ui.last_error`。
    store: Option<crate::shell::store::SessionStore>,
    /// 窗口可见性。Windows 最小化会送 `Resized(0,0)`,此时必须整帧跳过 GPU 与
    /// grid 传播,只保留 IO 泵(见 `shell::window_state`)。
    visible: shell::window_state::Visibility,
    /// 三个文件对话框各自的在跑标志,见 `PickerBusy`。
    picker_busy: PickerBusy,
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
    /// F148:本实例在历史目录里的身份。**恢复一条记录时会被换成那条记录的
    /// id**(设计 D12 接管槽位)—— 所以它不是 `const`,也不能是启动时算完就
    /// 不再变的东西。
    instance_id: String,
    /// F148:上一次写心跳的时刻。
    ///
    /// **与 `layout_checked_at` 分开**:布局落盘是「不脏就不写」,心跳必须
    /// **无条件**写 —— 搭它的顺风车的话,一个开着不动的窗口永远不写心跳,
    /// 会被别的实例判成死的,于是它正用着的现场出现在别人的恢复列表里
    /// (设计 D4 的实现约束)。
    heartbeat_at: Instant,
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
    /// F71 + F148:解锁框开着时,那份还没能用上的历史列表。
    ///
    /// 「这条会话还在不在库里」是丢弃规则之一(D16),而解锁框开着的时候库
    /// 还没打开 —— 列表只能先在这儿等着。解锁成功 / 放弃解锁时由
    /// `finish_store_open` 取走。
    pending_history: Option<Vec<mullion_store::HistoryEntry>>,
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
    /// F153:恢复现场之后的自动串行拨号进度。
    auto_dial: Option<AutoDial>,
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
    /// F209:本进程内传过的截图张数,进文件名当序号。
    ///
    /// **不用时间戳兜底撞名**:`/tmp` 是所有用户共用的,同一秒里连贴两张
    /// (完全正常的操作)会静默互相覆盖 —— 用户看到的是「第一张图不见了」。
    shot_seq: u64,
    /// F61/F62:会话外观的解析缓存。**只在会话/分组变更后 rebuild**,
    /// 绝不在渲染里现算(陷阱 T3,见 `ui::badge::AppearanceCache`)。
    appearance: crate::ui::badge::AppearanceCache,
    /// F92 拨测世代号。切会话 / 关编辑器 / 关会话管理器时 +1,
    /// 迟到的结果据此丢弃(见 `accept_probe`)。
    probe_epoch: u64,
    /// 在途拨测任务。退出或取消时 abort —— 20 秒的 timeout 悬着不管,
    /// 关窗后进程还要多活 20 秒。
    probe_task: Option<tokio::task::JoinHandle<()>>,
    /// 换节点在途的那些。`Vec` 而不是 `Option`:两块 pane 可以同时在换
    /// (弹窗一次只开一个,但拨号是异步的,第一次还没回来就能发起第二次),
    /// 用 `Option` 的话后发的会把先发的元信息顶掉 —— 现象是换好之后标题条
    /// 上写着另一台机器的名字。按 `(generation, pane)` 取走。
    pending_rehost: Vec<PendingRehost>,
    /// F162:恢复途中还要拨向**别的机器**的那些叶子。一条接一条,不并发
    /// (D10:并发会同时弹好几个密码框 / 主机指纹确认)。
    /// 三元组 =(标签世代, 那块 pane, 目标会话)。
    restore_dial: std::collections::VecDeque<(u64, PaneId, SessionId)>,
    /// F162:上面那条队列里有没有一条正在途中。`PaneRehosted`/`PaneRehostErr`
    /// 抵达时复位 —— **两条路径都要复位**,漏一条队列就永久停在这里。
    restore_dial_busy: bool,
    /// F128:正在重拨的连接 `(generation, host_ix, 已失败次数)`。
    /// 存在这张表里的 host 这一帧不再发起 —— 帧循环 60fps,不去重就是
    /// 一秒六十条连接(判据在 `reconnect::hosts_to_redial`)。
    reconnecting: Vec<(u64, usize, u32)>,
    /// F111/F114:已启动的隧道。**必须挂在 `App` 上** —— `TunnelHandle` 一
    /// Drop 就停隧道,放进临时变量等于隧道刚起来就被停掉。
    tunnels: crate::tunnels::TunnelRuntime,
    /// F6/设计 D23:用户想把键盘焦点放在哪一侧(终端 / 文件面板)。**只是意愿,
    /// 不是这一帧的真实生效值**——能否兑现取决于面板此刻在不在(见
    /// `effective_focus` 按上下文夹紧的说明)。默认终端,与迁移前行为一致。
    focus: shell::input_route::Focus,
    /// F55:上传/下载队列的全部状态,见 `TransferState`。
    transfer: TransferState,
    /// F53:外部/内置编辑的全部状态,见 `EditState`。
    edit: EditState,
    /// F163:在途的 attach 校验,`drive_attach_checks` 每帧推进。
    attach_checks: Vec<AttachCheck>,
}

/// F55:一条传输 job 从入队到落地牵扯到的全部状态。
///
/// 三项按 job id 一一对应,`queue` 是主表、另两项是它的边料:job 结束时
/// 三处必须一起清。收成一个结构的理由同 `EditState` —— 散在 `App` 上时
/// 「队列空了但 `specs` 还在涨」这类泄漏没有任何静态提示。
struct TransferState {
    /// F55:**跨标签**的传输队列。挂在 `App` 上而不是标签上 —— 设计里它
    /// 是全局的一条队列,切标签不该看见另一份;标签关掉时用
    /// `Queue::cancel_generation` 作废属于它的那些 job。
    queue: crate::files::queue::Queue,
    /// 每条在跑的 job 的取消旗标。worker 每块之后看一眼 —— 取消得能在
    /// 2GB 传到一半时立刻生效,不能等整个文件传完。
    cancels: std::collections::HashMap<u64, Arc<std::sync::atomic::AtomicBool>>,
    /// 每条 job 的完整参数(见 `TransferSpec`)。job 真正走完(不是挂在冲突上)
    /// 之后删掉,不然队列清空了它还在涨。
    specs: std::collections::HashMap<u64, TransferSpec>,
}

/// 三个文件对话框各自的「线程在跑吗」标志。防止连点「选择…」开出多个
/// 对话框(Windows 上主窗被 owner 关系禁用,Linux/XDG 未必)。
///
/// **这三个必须各是各的,绝不能合并成一个 `bool`** —— 收在同一个结构里
/// 是为了少占 `App` 的字段位,不是因为它们可以共用状态。三个按钮分别在
/// 会话编辑器的「连接」Tab、会话编辑器的「外观」Tab、和菜单栏上;共用
/// 一个标志的现象是「刚选完私钥,图标按钮就按不动了」。
#[derive(Default)]
struct PickerBusy {
    /// 私钥文件对话框。
    key: bool,
    /// 图标文件对话框。
    icon: bool,
    /// F2:ssh config 文件对话框。
    import: bool,
}

/// F205:一次拨号的**全部**随行数据。发起时装好、`ConnectOk`/`ConnectErr`
/// 凭票号取回。台账在 `shell::dial_ledger`。
///
/// 这四样从前是 `App` 上的四个独立单槽(`ui.connect_request_last`、
/// `pending_cfg`、`pending_automation` 的三项)。它们的生命周期是**同一条**
/// —— 都从「点连接」那一帧起、到那次拨号有结果为止 —— 却散着放,于是
/// 第二次拨号会把第一次的四样一起盖掉,而两条拨号同时在途在高延迟代理
/// 链路上是常态。收成一个结构之后,「这次拨号的东西」只有一份、只能整份
/// 取走,漏拿一样是编译错误而不是静默串台。
struct DialTicket {
    /// 这次拨的是哪条会话。`None` = CLI 直连(`mullion user@host`),
    /// 没有会话记录可查 —— 书签/默认目录/自动化一律按「没配置」处理。
    session_id: Option<SessionId>,
    /// 这次连接用的配置。`ConnectOk` 抵达时移交给新标签的
    /// `TerminalTab::last_cfg`(F35 分屏 `open_pty` 靠它)。
    cfg: SshConfig,
    /// F40~F44/F141:自动化待决包,见 `PendingAutomationState`。
    automation: PendingAutomationState,
}

/// F40~F44/F141:一次「点连接」在**那一帧**算好、等 `ConnectOk` 抵达时
/// 消费的自动化待决包。
///
/// 四项由同一次点击、同一次 `store.resolved()` 算出,也在同一处消费。
/// **都是在点击帧算而不是 `ConnectOk` 里算**:连接在途期间用户完全可能
/// 改了配置、把会话改名、甚至删掉它 —— 那样分屏发出去的字节就跟他点
/// 「连接」时看到的对不上了。
#[derive(Default)]
struct PendingAutomationState {
    /// 等 `ConnectOk` 抵达时启用的计划。
    plan: Option<crate::automation::PendingAutomation>,
    /// 自动化配置**原件**,`ConnectOk` 抵达时移交给新标签的
    /// `automation_template`,供后来的 pane(分屏 / 换节点)复用。
    template: Option<mullion_store::ResolvedAutomation>,
    /// F141:**会话名**(tmux 会话名的兜底来源)。`ConnectOk` 抵达时跟
    /// `template` 一起用来算 `TerminalTab::tmux_attach`。
    session_name: Option<String>,
    /// F44 右键「连接(跳过自动化)」的一次性标志。`ConnectOk` 消费后立即清零。
    skip: bool,
}

impl TransferState {
    fn new() -> Self {
        Self {
            queue: crate::files::queue::Queue::new(DEFAULT_TRANSFER_CONCURRENCY),
            cancels: std::collections::HashMap::new(),
            specs: std::collections::HashMap::new(),
        }
    }
}

/// F53:一次「把远端文件拉下来编辑再回传」牵扯到的全部状态。
///
/// **收成一个结构而不是散在 `App` 上**:这五项的生命周期是**同一条**——
/// 一条编辑从 `sessions.add` 起算,`close_edit` 时必须同时摘掉它的 watcher、
/// original、conflict。散着放时每加一条清理路径都得记得凑齐五处,漏一处的
/// 表现是「关掉的文件还在后台被 stat」或「第二次编辑不再备份原文」,而且
/// 两者都不报错。聚在一起之后,漏没漏一眼就能看出来。
struct EditState {
    /// F53:所有还挂在监视里的编辑。跨标签一份,理由同 `TransferState::queue`。
    sessions: crate::edit::sessions::EditSessions,
    /// F53:内置编辑器窗口。**同一时刻只开一个** —— 多开的价值远小于
    /// 「哪个窗口对应哪个文件」带来的混乱,而这里每个窗口背后都是一次
    /// 会覆盖远端文件的写。
    editor: Option<crate::ui::editor_window::EditorState>,
    /// F53:每条编辑的看门任务(1 秒看一次本地 mtime,D3-10)。
    ///
    /// **不走事件循环的 `WaitUntil`**:那条路径是 T3/T7 的高压区(三个分支
    /// 都要显式复位 `control_flow`),为一个「一秒一次的文件 stat」去动它
    /// 得不偿失。tokio 任务经 `proxy` 回送事件,天然会唤醒事件循环。
    watchers: std::collections::HashMap<u64, tokio::task::JoinHandle<()>>,
    /// F53/D3-7:打开那一刻读到的原文,用来在**第一次**回传前留一份
    /// `.mullion.bak`。回传成功后即丢 —— 之后远端那份就是我们自己写的,
    /// 再备份一次既没有意义又要多传一遍全量。
    originals: std::collections::HashMap<u64, Vec<u8>>,
    /// F53:撞上冲突时远端**当时**的戳。「保留远端」要拿它刷快照(D3-9),
    /// 「覆盖远端」要拿它当新的比对基准 —— 否则覆盖那一下会再撞一次冲突。
    conflicts: std::collections::HashMap<u64, crate::edit::sessions::RemoteStamp>,
    /// F53:外部编辑用的临时文件根目录。退出时整棵删掉(D3-12)。
    /// 进程启动时算一次 —— `directories` 每次调用都要摸环境变量。
    root: std::path::PathBuf,
}

impl EditState {
    fn new() -> Self {
        Self {
            sessions: crate::edit::sessions::EditSessions::new(),
            editor: None,
            watchers: std::collections::HashMap::new(),
            originals: std::collections::HashMap::new(),
            conflicts: std::collections::HashMap::new(),
            root: crate::edit::tempdir::root(),
        }
    }
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
    /// 换节点弹窗(pane 标题条上的换节点按钮)。里面有搜索框 —— 不算模态的话敲的
    /// 字会同时发给远端 shell(T8)。
    Rehost,
    /// F131:文件面板的路径条正在被编辑。**不算模态的话那个输入框收不到
    /// 任何键** —— 面板持有键盘焦点时键根本不喂 egui(T8 的注入点在
    /// `input_route::egui_should_see_focused`),Backspace 还会被
    /// `handle_panel_key` 解释成「回上级目录」。同 `Editor` 的理由。
    ///
    /// **不进 `touched_store`**:它一行 store 都不写(同 `Rehost` 的姿态)。
    FilesPathEdit,
    /// F200:文件面板里有一行正在**就地改名**。理由与 `FilesPathEdit`
    /// 逐字相同 —— 那个输入框收不到任何键(T8),而 Backspace 还会被
    /// `handle_panel_key` 解释成「回上级目录」,一按就跳走。
    ///
    /// **不进 `touched_store`**:它一行 store 都不写(同 `FilesPathEdit`)。
    FilesRename,
    /// F148:「恢复上次的现场」弹窗。里面没有输入框,但有一颗一按就摆回
    /// 整个标签栏的「恢复」按钮,而空格/回车在 egui 里是按钮的激活键 ——
    /// 同 `Modal::Import` 的理由(T8)。
    History,
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
        Modal::FilesPathEdit,
        Modal::FilesRename,
        Modal::History,
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
            dials: crate::shell::dial_ledger::DialLedger::default(),
            initial,
            cli_direct,
            ui: crate::ui::UiState::default(),
            store: None,
            visible: shell::window_state::Visibility::default(),
            picker_busy: PickerBusy::default(),
            ui_dirty: true, // 首帧必须画出来
            files_sidebar_was_open: false,
            cursor_px: (0.0, 0.0),
            clipboard: crate::clipboard::Clipboard::new(),
            // 与日志文件名同源(logx::instance_id):两边共用一个 id,
            // 日志与现场历史记录才对得上号。
            instance_id: crate::logx::instance_id().to_string(),
            // 减去一整个间隔:第一次 `about_to_wait` 就该写下心跳,而不是
            // 等 15 秒 —— 那 15 秒里别的实例会把本进程判成死的。
            heartbeat_at: Instant::now()
                - Duration::from_secs(mullion_store::HEARTBEAT_INTERVAL_SECS),
            last_saved_layout: None,
            layout_checked_at: Instant::now(),
            // F84:真正的值在 `resumed` 里从 `settings.toml` 读(要先有窗口才
            // 知道 DPI,才能把 pt 换成像素)。这里先放默认。
            settings: mullion_store::Settings::default(),
            settings_backup: None,
            settings_families: Vec::new(),
            pending_history: None,
            settings_not_mono: false,
            pending_restore: None,
            auto_dial: None,
            dragging: false,
            prev_click: None,
            press_anchor: None,
            autoscroll: 0,
            ime: Default::default(),
            ime_cursor_area: None,
            pending_paste: None,
            shot_seq: 0,
            appearance: Default::default(),
            probe_epoch: 0,
            probe_task: None,
            pending_rehost: Vec::new(),
            restore_dial: std::collections::VecDeque::new(),
            restore_dial_busy: false,
            reconnecting: Vec::new(),
            tunnels: Default::default(),
            focus: shell::input_route::Focus::default(),
            // F56:默认 4 条并发。可配 UI 是 D2-c 的欠账,先按设计定的默认值走。
            transfer: TransferState::new(),
            edit: EditState::new(),
            attach_checks: Vec::new(),
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
        history: Vec<mullion_store::HistoryEntry>,
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
        self.finish_store_open(history);
    }

    /// 会话库尘埃落定之后的那串收尾:算外观缓存、决定第一屏。
    ///
    /// 抽出来的唯一理由是 F71:解锁框开着的时候这些都还不能做(库还没打开,
    /// 「这条会话还在不在库里」答不上来),得等解锁成功再跑。
    ///
    /// **F148 起不再自动摆回标签**(D1):启动摆什么由用户在恢复列表里选。
    fn finish_store_open(&mut self, history: Vec<mullion_store::HistoryEntry>) {
        self.migrate_local_bookmarks_into_settings();
        // 启动时先算一次,否则第一次打开会话管理器全是无色。
        self.refresh_appearance();

        // CLI 直连(路径①)→ 立刻发起连接,进终端态。
        if let Some(cfg) = self.initial.take() {
            // CLI 直连恒是终端态——这条路径没有会话记录可查协议字段。
            self.spawn_connect(cfg, false, None, false);
            return;
        }
        // F148 D9:无参启动 → 有历史就先给恢复列表,没有就照旧弹会话管理器。
        // **必须在这里而不是 `resumed` 里**:「这条会话还在不在库里」要查
        // 会话库,而库到这一刻才刚打开。
        let rows = self.history_rows(&history);
        if rows.is_empty() {
            // 首次运行 / 全被清空 —— 弹一个空列表等于让用户点一下才能开始干活。
            self.ui.session_manager_open = true;
        } else {
            self.ui.history = Some(crate::ui::history::HistoryDraft::new(rows));
        }
    }

    /// F187:把老库里挂在各会话下的本地书签并进全局设置,**只做一次**。
    ///
    /// 放在 `finish_store_open` 里而不是 `resumed`:老数据在会话库里,而库到
    /// 那一刻才刚打开(主密码那条路上更晚)。
    ///
    /// **库没打开就一步都不做。** 库打不开(主密码错、文件损坏)时照样置上
    /// 「已迁移」标记的话,用户手上那份老收藏就永久没人再看一眼了 —— 而这
    /// 恰恰是本次要修的那类丢数据。
    fn migrate_local_bookmarks_into_settings(&mut self) {
        if self.settings.local_bookmarks_migrated {
            return;
        }
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let old: Vec<mullion_store::Bookmark> = store
            .list()
            .iter()
            .flat_map(|r| r.sftp.local_bookmarks.iter().cloned())
            .collect();
        let n = self.settings.merge_local_bookmarks(old);
        // 标记本身也要落盘,否则下次启动又来一遍(那样用户取消掉的收藏会
        // 从没清理的会话记录里长回来)。条数为 0 时也存。
        match self.save_settings() {
            Ok(()) => crate::logx::line(&format!("F187:本地收藏夹已并入全局设置,{n} 条")),
            // 不打断启动、也不弹错:这是后台迁移,用户没主动点过什么。下次
            // 启动会再试一次(标记没存下来,内存里的置位随进程一起消失)。
            Err(e) => {
                crate::logx::line(&format!("F187:本地收藏夹迁移没能存下来({e}),下次启动重试"))
            }
        }
    }

    /// 写 `settings.toml`。`apply_settings_action` 与 F187 的书签写入共用 ——
    /// 各写一遍的话,以后往里加一步(比如夹紧某个字段)必然漏改一处。
    fn save_settings(&self) -> Result<(), String> {
        crate::shell::store::config_dir()
            .ok_or_else(|| "定位不到配置目录".to_string())
            .and_then(|d| {
                mullion_store::settings::save(&d, &self.settings).map_err(|e| e.to_string())
            })
    }

    /// F187:把全局本地收藏夹推给**每一个**已开标签的面板副本。
    ///
    /// 收藏夹是全局的,但每个标签的 `PanelFrame` 各持一份画图用的副本 ——
    /// 只更新当前标签的话,在标签 1 收的目录要等标签 2 重开才看得见,而
    /// 用户完全有理由以为「☆ 点了没反应」。遍历全部标签,不挑活动的那个
    /// (同 `drive_*` 那条纪律)。
    fn sync_local_bookmarks_to_tabs(&mut self) {
        let list = self.settings.local_bookmarks.clone();
        for tab in self.tabs.iter_mut() {
            if let Some(files) = tab.content.files_panel_mut() {
                files.local_bookmarks = list.clone();
            }
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
                    let history = self.pending_history.take().unwrap_or_default();
                    self.finish_store_open(history);
                    return false;
                };
                match crate::shell::store::SessionStore::unlock(dir, &password) {
                    Ok(s) => {
                        self.ui.unlock = None;
                        let history = self.pending_history.take().unwrap_or_default();
                        self.open_store_with(Ok(s), history);
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
                        let history = self.pending_history.take().unwrap_or_default();
                        self.finish_store_open(history);
                    }
                }
                mark_ui_dirty!(self.ui_dirty);
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
                mark_ui_dirty!(self.ui_dirty);
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
        mark_ui_dirty!(self.ui_dirty);
        self.refresh_monospace_warning();
    }

    /// F155:把设置里的日志档位施加到 `log` facade 上。
    ///
    /// **环境变量仍然优先**(`logx::resolve_levels`):用户带着
    /// `MULLION_LOG=debug` 启动、又在设置里选了「只记错误」,他要的是前者。
    fn apply_log_level(&self) {
        let env_app = std::env::var("MULLION_LOG").ok();
        let env_deps = std::env::var("MULLION_LOG_DEPS").ok();
        let (app, deps) = crate::logx::resolve_levels(
            self.settings.log_level,
            env_app.as_deref(),
            env_deps.as_deref(),
        );
        crate::logx::set_levels(app, deps);
        crate::logx::line(&format!("日志档位改为 app={app} deps={deps}"));
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
                self.apply_log_level();
                let saved = self.save_settings();
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
            // F155:设置里点了导出。置位交给每帧的 `drain_export_log_request`
            // 统一处理 —— 两个入口(菜单/设置)共用同一条路径,不复制一遍。
            O::ExportLog => {
                self.ui.export_log_request = true;
            }
        }
    }

    /// F155:把**本实例的**日志脱敏后另存一份,并把路径告诉用户。
    ///
    /// 只导本实例(F166 之后一实例一文件)。多开时别的实例的日志各自
    /// 独立,需要的话在那边点。
    ///
    /// **同步读写、在主线程上做**:日志文件上限 8MB(debug 档 64MB),
    /// 一次读+写在本机盘上是几十毫秒,而这是用户点了按钮等着看结果的动作 ——
    /// 为它开一条 task 换来的是「点完什么都没发生,过一会儿状态栏突然变了」。
    /// 落在 `Stage::StoreIo` 里,卡住的话看门狗会说出来。
    fn drain_export_log_request(&mut self) {
        if !std::mem::take(&mut self.ui.export_log_request) {
            return;
        }
        diag::mark(diag::Stage::StoreIo);
        // 缓冲里可能还压着刚刚那几行,先刷下去,否则导出的副本缺最后一段。
        crate::logx::flush_now();
        let done = crate::logx::log_path()
            .ok_or_else(|| "定位不到日志文件".to_string())
            .and_then(|src| {
                let text = std::fs::read_to_string(&src).map_err(|e| format!("读不出日志({e})"))?;
                let mut r = crate::redact::Redactor::new();
                let mut out = crate::redact::header();
                for line in text.lines() {
                    out.push_str(&r.line(line));
                    out.push('\n');
                }
                // 带上 instance id:多开时两个实例都点「导出」的话,固定
                // 文件名会让后点的那个悄悄覆盖前一个。
                let dst = src.with_file_name(format!(
                    "mullion-redacted-{}.log",
                    crate::logx::instance_id()
                ));
                std::fs::write(&dst, out).map_err(|e| format!("写不出副本({e})"))?;
                Ok(dst)
            });
        match done {
            Ok(dst) => {
                let others = crate::logx::log_dir()
                    .and_then(|d| std::fs::read_dir(d).ok())
                    .map_or(0, |rd| {
                        rd.flatten()
                            .filter(|e| {
                                // `file_name()` 返回的是 OsString(自有值),
                                // 必须先绑定再借 —— 链式写 `e.file_name().to_str()`
                                // 会让 `&str` 借在一个当场析构的临时值上,编译失败。
                                let name = e.file_name();
                                name.to_str()
                                    .and_then(crate::logx::parse_log_name)
                                    .is_some_and(|id| id != crate::logx::instance_id())
                            })
                            .count()
                    });
                let msg = if others > 0 {
                    format!(
                        "已导出脱敏日志:{}(本机还有 {others} 份其他实例的日志)",
                        dst.display()
                    )
                } else {
                    format!("已导出脱敏日志:{}", dst.display())
                };
                crate::logx::line(&msg);
                self.ui.set_error(msg);
            }
            Err(e) => self.ui.set_error(format!("导出脱敏日志失败:{e}")),
        }
        mark_ui_dirty!(self.ui_dirty);
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
            self.settings.log_level = d.log_level;
            self.settings.shell_osc7_bootstrap = d.shell_osc7_bootstrap;
        }
    }

    // ------------------------------------------------ F37 布局持久化(E7/E8)

    /// F148 D14:迁移老 `layout.toml` 时给那条记录用的 id。
    ///
    /// **不能直接用 `self.instance_id`**:那是本实例正在写的槽位,迁移过去
    /// 会被本进程 2 秒后的第一次落盘(此刻标签栏是空的)当场覆盖成空现场 ——
    /// 用户升级前那一屏就这么没了。
    fn instance_id_for_legacy(&self) -> String {
        format!("{}-legacy", self.instance_id)
    }

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
            // F148:**这里恒填 0**。时刻由落盘那一侧在确定要写之后才盖上 ——
            // 在这里盖的话,每次现算的快照都带着不同的时刻,`last_saved_layout`
            // 的逐字段比对就永远不相等,于是空闲时每 2 秒也会写一次盘。
            updated_at: 0,
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
    ///
    /// F148:写的是**本实例的槽位** `layouts/<instance_id>.toml`,不再是共享的
    /// `layout.toml` —— 多开时两个进程每 2 秒轮流覆盖同一个文件,最后关的赢
    /// (那正是这一片要修的第一件事)。
    fn save_layout_if_changed(&mut self) {
        let now = self.snapshot_layout();
        if self.last_saved_layout.as_ref() == Some(&now) {
            return;
        }
        let Some(dir) = crate::shell::store::config_dir() else {
            return;
        };
        // 时刻在**确定要写**之后才盖:盖在 `snapshot_layout` 里的话,上面那句
        // 比对永远不相等(见那里的注释)。
        let mut out = now.clone();
        out.updated_at = mullion_store::now_secs();
        match mullion_store::save_record(&dir, &self.instance_id, &out) {
            // 记 `now`(时刻为 0 的那份)而不是 `out` —— 下一次比对拿到的
            // 也是时刻为 0 的新快照,两者才可比。
            Ok(()) => self.last_saved_layout = Some(now),
            Err(e) => log::debug!(target: "mullion", "现场落盘失败: {e}"),
        }
    }

    /// F148:到点就写一次心跳。**无条件**,不看布局脏不脏 —— 见
    /// `heartbeat_at` 字段的说明。
    fn tick_heartbeat(&mut self) {
        if self.heartbeat_at.elapsed().as_secs() < mullion_store::HEARTBEAT_INTERVAL_SECS {
            return;
        }
        self.heartbeat_at = Instant::now();
        let Some(dir) = crate::shell::store::config_dir() else {
            return;
        };
        if let Err(e) =
            mullion_store::touch_alive(&dir, &self.instance_id, mullion_store::now_secs())
        {
            log::debug!(target: "mullion", "心跳写入失败: {e}");
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
    ///
    /// 返回值 = **这一次有没有真的把拨号发出去**。F153 的自动队列靠它区分
    /// 「等 `ConnectOk`/`ConnectErr` 回来推进」和「压根不会有事件回来,当场
    /// 记一笔失败接着试下一条」—— 缺凭据 / 库没打开时这个函数直接 return,
    /// 不返回这个真值的话队列会永远等一个不来的事件。
    fn reconnect_tab(&mut self, tab_id: shell::tabs::TabId) -> bool {
        if self.pending_restore.is_some() {
            return false;
        }
        let Some((saved_session, tree, focus_leaf)) =
            self.tabs.iter().find(|t| t.id == tab_id).and_then(|t| {
                match &t.content {
                    TabContent::Restored(r) => Some((r.session_id, r.tree.clone(), r.focus_leaf)),
                    // 已经连上了(或者本来就不是占位标签)—— 没什么可重连的。
                    TabContent::Terminal(_) | TabContent::Files(_) => None,
                }
            })
        else {
            return false;
        };
        // F162:拨的是**主叶子**那条会话 —— 前序第一个身份还连得上的叶子。
        // 照 `SavedTab::session_id` 拨的话,叶子 0 的会话被用户删掉之后,
        // 会连上一台「树上其实没有任何叶子属于它」的机器。
        let known: Vec<SessionId> = self
            .store
            .as_ref()
            .map_or(Vec::new(), |s| s.list().iter().map(|r| r.id).collect());
        let Some(identities) = crate::shell::layout_snapshot::leaf_identities(&tree, saved_session)
        else {
            log::warn!(target: "mullion", "恢复:标签的树编码坏了,不拨号");
            return false;
        };
        let Some((main_leaf, session_id)) =
            crate::shell::restore_plan::main_leaf(&identities, &|s| known.contains(&s))
        else {
            // 一个能连的叶子都没有(会话全被删了)。保持占位态,别把标签
            // 的 `dialing` 置起来 —— 那会让「重连」按钮永久灰着。
            self.ui
                .set_error("这个标签里的会话都已经不在库里了,无法恢复".to_string());
            mark_ui_dirty!(self.ui_dirty);
            return false;
        };
        let plan = match self.store.as_ref().map(|s| s.dial_plan_for(session_id)) {
            Some(Ok(plan)) => plan,
            Some(Err(e)) => {
                self.ui.set_error(e.to_string());
                mark_ui_dirty!(self.ui_dirty);
                return false;
            }
            None => return false,
        };
        let (cfg, wants_sftp) = plan;
        self.pending_restore = Some(PendingRestore {
            tab_id,
            tree,
            focus_leaf,
            main_leaf,
            identities,
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
        self.cli_direct = false;
        mark_ui_dirty!(self.ui_dirty);
        // F205:会话身份随票走 —— 重连是在**已经有别的连接在途**时最容易被
        // 触发的一条路径,单槽在这里被盖掉的概率最高。
        self.spawn_connect(cfg, wants_sftp, Some(session_id), false);
        true
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
            mark_ui_dirty!(self.ui_dirty);
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
        let _ = self.reconnect_tab(tab_id);
    }

    /// F37:菜单里的「全部重连」。**一个一个来** —— `reconnect_tab` 里那道
    /// `pending_restore` 闸保证同时只有一条在拨号,这里只是把第一个还没连
    /// 的占位标签交给它;剩下的等这条连上之后用户再按一次。
    ///
    /// **这条路径**不排队自动连完:菜单里的「全部重连」是随手点的,连成一串
    /// 长活动会让人以为客户端卡住了。F153 的「恢复上次的现场」那条路径**会**
    /// 排队(`advance_auto_dial`)—— 那是用户明确表达了「我要用这一批」,
    /// 而且照样一条接一条、不并发,设计 §1 否的是并发不是自动。
    fn reconnect_next_restored(&mut self) {
        let Some(id) = self.tabs.iter().find_map(|t| match &t.content {
            TabContent::Restored(r) if !r.dialing => Some(t.id),
            _ => None,
        }) else {
            return;
        };
        let _ = self.reconnect_tab(id);
    }

    /// F153:推进自动串行拨号。`outcome` = 刚结束那一条的结果
    /// (`Some(true)` 成功 / `Some(false)` 失败 / `None` = 起点,还没拨过)。
    ///
    /// 用 `loop` 而不是递归:一条都没发起出去(缺凭据/库没打开)时要接着
    /// 试下一条,递归深度会跟着标签数走。
    fn advance_auto_dial(&mut self, outcome: Option<bool>) {
        let Some(mut auto) = self.auto_dial.take() else {
            return;
        };
        match outcome {
            Some(true) => auto.ok += 1,
            Some(false) => auto.err += 1,
            None => {}
        }
        loop {
            let Some(next) = next_auto_dial(&self.tabs, &auto.tried) else {
                // 一条都没试过就到头了(恢复出来的标签全被筛掉)——不报
                // 「已自动连上 0 个标签」,那只会让人以为出了什么事。
                if !auto.tried.is_empty() {
                    self.ui.set_toast(auto_dial_summary(auto.ok, auto.err));
                }
                mark_ui_dirty!(self.ui_dirty);
                return;
            };
            auto.tried.push(next);
            if self.reconnect_tab(next) {
                self.auto_dial = Some(auto);
                mark_ui_dirty!(self.ui_dirty);
                return;
            }
            // 连拨号都没发起出去 —— 不会有 `ConnectOk`/`ConnectErr` 回来
            // 推进队列,当场记一笔失败,接着试下一条。
            auto.err += 1;
        }
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
        // D13:**追加**在现有标签后面,不清空 —— 清空会断掉正在跑的连接。
        // 所以得先记住追加起点:存进记录里的 `active_tab` 是**那条记录内部**
        // 的下标,不是本窗口标签栏里的下标。
        let base = self.tabs.len();
        let active = usable.active_tab;
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
        // D13:加上追加起点 —— 运行中恢复时前面还有别的标签,用记录内部的
        // 裸下标会跳到一个不相干的标签上。
        self.tabs.switch_to_index(base + active);
        crate::logx::line(&format!("F148:恢复了 {count} 个占位标签"));
        mark_ui_dirty!(self.ui_dirty);
    }

    /// F148:把一批记录做成弹窗要画的行(D10/D16)。
    ///
    /// **会话已删的标签在这里就被滤掉**(沿用 `layout_snapshot::usable` 的
    /// 规则):摘要里列一个已经不存在的会话名,用户点了恢复只会得到一个点了
    /// 必然失败的「重连」。**整条记录一个可用标签都不剩时,这条记录不进列表**
    /// —— 它恢复出来是个空窗口。
    ///
    /// **活着的实例的记录不进列表**(D3):那个现场正被别人用着。
    fn history_rows(
        &self,
        entries: &[mullion_store::HistoryEntry],
    ) -> Vec<crate::ui::history::HistoryRow> {
        let known: Vec<SessionId> = self
            .store
            .as_ref()
            .map_or(Vec::new(), |s| s.list().iter().map(|r| r.id).collect());
        let now = mullion_store::now_secs();
        let mut out = Vec::new();
        for e in entries {
            if e.alive {
                continue;
            }
            let usable =
                crate::shell::layout_snapshot::usable(e.layout.clone(), &|id| known.contains(&id));
            if usable.tabs.is_empty() {
                continue;
            }
            let titles: Vec<String> = usable.tabs.iter().map(|t| t.title.clone()).collect();
            let panes: usize = usable
                .tabs
                .iter()
                .map(|t| crate::shell::layout_snapshot::leaf_count(&t.tree).unwrap_or(1))
                .sum();
            let when = crate::ui::history::when_text(now, e.layout.updated_at);
            out.push(crate::ui::history::HistoryRow {
                id: e.id.clone(),
                head: crate::ui::history::head_text(&when, usable.tabs.len(), panes),
                summary: crate::ui::history::summary_text(&titles),
            });
        }
        out
    }

    /// F148:菜单里点了「恢复上次的现场…」—— 现读一次盘、建草稿。
    ///
    /// **现读而不是用启动时那份**:这中间可能又有别的窗口关掉了,拿旧列表
    /// 会让用户看不到刚关的那个现场。
    fn open_history_dialog(&mut self) {
        let entries = crate::shell::store::config_dir()
            .map(|d| mullion_store::list_records(&d, mullion_store::now_secs()))
            .unwrap_or_default();
        let rows = self.history_rows(&entries);
        if rows.is_empty() {
            self.ui.set_toast("没有可恢复的现场");
            return;
        }
        self.ui.history = Some(crate::ui::history::HistoryDraft::new(rows));
        mark_ui_dirty!(self.ui_dirty);
    }

    /// F148:恢复一条记录(D12 接管槽位 / D13 追加进当前窗口)。
    ///
    /// 三步,顺序不能换:
    /// 1. 读出那条记录并摆回标签(**追加**在现有标签后面,不清空 —— 清空会
    ///    断掉正在跑的连接);
    /// 2. 删掉本实例原来的槽位文件(启动时它通常还不存在,删除是 no-op);
    /// 3. 把本实例的身份换成那条记录的 id —— 此后就往那个文件写。
    ///
    /// 第 3 步是「接管」的全部内容(D12):不接管的话,本实例仍在写自己的新
    /// 槽位,而老记录原样躺着 —— 下次启动列表里就会出现两条内容几乎一样的
    /// 记录,而且越滚越多。
    ///
    /// **窗口几何不套用**(X8/D13):窗口已经建好了,再跳一次位置只会让人
    /// 眼花。
    fn restore_history(&mut self, id: &str) {
        let Some(dir) = crate::shell::store::config_dir() else {
            return;
        };
        let now = mullion_store::now_secs();
        let Some(entry) = mullion_store::list_records(&dir, now)
            .into_iter()
            .find(|e| e.id == id)
        else {
            // 两次启动之间被别的实例裁掉了(D5)。不是错误,说一声就行。
            self.ui.set_toast("那条现场已经不在了");
            return;
        };
        self.restore_tabs(entry.layout);
        // 2 → 3:先删旧槽位再改身份,顺序反了会把**刚接管的那个文件**删掉。
        mullion_store::remove_record(&dir, &self.instance_id);
        self.instance_id = id.to_string();
        // 接管之后立刻打一次心跳:别的实例这一刻起就该把这个槽位看成「有人
        // 在用」,否则第二个新实例会把它也列出来,两个进程往同一个文件写(D12
        // 的残余竞态)。
        let _ = mullion_store::touch_alive(&dir, &self.instance_id, now);
        // 本实例的记录内容变了(标签栏多了一批),下次比对必须重来一遍 ——
        // 不清的话 `save_layout_if_changed` 会拿旧快照比出「没变」,新摆回来
        // 的标签永远不落盘。
        self.last_saved_layout = None;
        // F153:摆完就一条接一条地拨号。用户选「恢复」的意思就是要用它们,
        // 不是要再挨个点一遍「重连」。
        self.auto_dial = Some(AutoDial::default());
        self.advance_auto_dial(None);
        mark_ui_dirty!(self.ui_dirty);
    }

    /// F55:作废属于某个标签的全部传输。扳旗标(让在跑的 worker 立刻停)
    /// **和**改队列状态(让界面上那几条变成「已取消」)缺一不可。
    fn cancel_transfers_of(&mut self, generation: u64) {
        for id in self.transfer.queue.cancel_generation(generation) {
            if let Some(c) = self.transfer.cancels.remove(&id) {
                c.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            self.transfer.specs.remove(&id);
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
    /// **不是**画成空心框:`style_for` 的 `focused` 参数来自
    /// `PaneRender.focused = Some(g.id) == focus`(分屏内哪个 pane 有焦点),
    /// 跟窗口有没有拿到 OS 焦点(这里的 `self.window_focused`)是两个不相干
    /// 的量,这次改动没有把二者接起来。实际效果是:窗口失焦时,焦点 pane
    /// 仍按远端 DECSCUSR 给的形状画(Block/Bar/Underline),只是不再闪烁。
    ///
    /// 薄壳:决策全在 `blink_on_at`(可脱离 `App` 单测),同 `sync_timeout_wake`
    /// 委托给 `sync_timeout_wake_at` 的理由——`App` 在无 GPU/窗口的环境下构造
    /// 不出来,分支逻辑只能靠这条路径测得着。
    fn blink_on(&self, now: Instant) -> bool {
        let elapsed = now
            .saturating_duration_since(self.last_input_at)
            .as_millis() as u64;
        blink_on_at(self.window_focused, elapsed)
    }

    /// F125:下一次光标相位翻转的时刻。`None` = 这一刻不需要为闪烁排唤醒
    /// (窗口失焦 / 没有终端在前台)。薄壳,理由同 `blink_on`。
    fn blink_wake(&self, now: Instant) -> Option<Instant> {
        let elapsed = now
            .saturating_duration_since(self.last_input_at)
            .as_millis() as u64;
        let ms = blink_wake_at(self.window_focused, self.active_ws().is_some(), elapsed)?;
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
            Modal::Editor => self.edit.editor.is_some(),
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
            // F131:见 `Modal::FilesPathEdit` 的说明。
            Modal::FilesPathEdit => self.files_path_editing(),
            // F200:见 `Modal::FilesRename` 的说明。
            Modal::FilesRename => self.files_renaming(),
            // F148:见 `Modal::History` 的说明(T8)。
            Modal::History => self.ui.history.is_some(),
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

    /// F131:这一帧文件面板的某一栏正在编辑路径吗。
    ///
    /// 判据走 `files_owner_generation()`,与面板「这一帧到底画不画得出来」
    /// 同源 —— 面板不可见时恒 `false`,不会因为某个后台标签里留着一个没清
    /// 干净的编辑缓冲就把整个窗口判成模态。纯逻辑核心是 `files_path_editing_of`。
    fn files_path_editing(&self) -> bool {
        files_path_editing_of(&self.tabs, self.ui.files_sidebar_open)
    }

    /// F200:文件面板里有没有一行正在就地改名(`Modal::FilesRename` 的判据)。
    fn files_renaming(&self) -> bool {
        files_renaming_of(&self.tabs, self.ui.files_sidebar_open)
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
    /// 判定核心在 [`sync_plan_of`]:这里只管取数据(世代 / 属主标签 /
    /// sftp client / 这条 channel 记录在案的机器 / 焦点分屏所在的机器 /
    /// 焦点 pane 的 cwd)、调用、按结果派发。
    ///
    /// F132:三选一 —— 同一台机器只 `Goto`;焦点分屏换到了**另一台**,
    /// 原来那条 sftp channel 连的还是旧机器,`Goto` 只会把路径发去错的
    /// 连接上(路径对了、机器错了),必须先摘旧连接、在新机器上 `Reopen`。
    fn sync_files_to_focused_pane(&mut self) {
        let Some(gen) = self.files_owner_generation() else {
            return;
        };
        let tab = self.tabs.by_generation(gen);
        let has_client = tab.is_some_and(|t| t.content.sftp_client().is_some());
        let sftp_host_ix = tab.and_then(|t| t.content.sftp_host_ix());
        let focus_host_ix = tab.and_then(|t| t.content.focused_pane_host_ix());
        let pane_cwd = tab.and_then(|t| t.content.focused_pane_cwd());
        let home = tab.and_then(|t| t.content.sftp_home());
        match sync_plan_of(
            has_client,
            sftp_host_ix,
            focus_host_ix,
            pane_cwd.as_deref(),
            home.as_deref(),
        ) {
            SyncPlan::Nothing => {}
            SyncPlan::Goto(dir) => {
                let target = mullion_ssh::sftp::RemotePath::from_bytes(dir.into_bytes());
                self.apply_remote_file_action(
                    gen,
                    crate::ui::files_panel::FileAction::Goto(target),
                );
            }
            SyncPlan::Reopen => self.reopen_sftp_on_focused_host(gen),
        }
    }

    /// F132:焦点分屏在另一台机器上 —— 把这条 sftp channel 换过去。
    ///
    /// **顺序不可换**:先摘掉旧 client,再 `trigger_sftp_open`。反过来的话它
    /// 开头那句 `sftp_client().is_some() || already_loading` 会直接早退,什么
    /// 都不发 —— 用户看到的是「按了没反应」。同理**不在这里把面板置成加载
    /// 中**:那撞的是同一句的另一半(置加载中是 `trigger_sftp_open` 自己的
    /// 事);作废用 `invalidate`,它把 `load` 退回 `Idle`。
    ///
    /// **不动 `sftp_tasks`**。那个池子是混的:`track_sftp_task` 把列目录、
    /// 写操作、以及 `pump_transfers` 里每条传输的句柄都塞在一起,`drain` 后
    /// 无差别 abort 会连着两样东西一起打断 ——
    /// - 传输 worker 被硬杀就再也发不出 `UserEvent::TransferDone`,队列里那条
    ///   job 永久停在 `Running`,而 `take_runnable` 按 `Running` 数占并发名额,
    ///   撞几次就把全局传输堵死;
    /// - 列目录任务被硬杀则 `PaneState::load` 永远翻不出 `Loading`(只有
    ///   `accept` 翻得动),接着就是上面那条永久早退。
    ///
    /// 而它们本来就不需要被打断:两类任务在发起时各自克隆了一份 `Arc`(传输
    /// 拿 `Arc<SshConnection>` 并**自己开一条新 channel**,写操作拿的是
    /// `Arc<SftpClient>`、复用已开的那条),`*slot = None` 只是把槽位里的那份
    /// 引用拿走,谁都不会因此失效。用户是对着旧那台机器发起的,让它跑完才是
    /// 对的。迟到的列目录结果由 `invalidate` 递增的 `request_seq` 挡掉。
    fn reopen_sftp_on_focused_host(&mut self, generation: u64) {
        if let Some(tab) = self.tabs.by_generation_mut(generation) {
            if let Some(slot) = tab.content.sftp_mut() {
                *slot = None;
            }
            if let Some(t) = tab.content.as_terminal_mut() {
                t.sftp_host_ix = None;
            }
            if let Some(files) = tab.content.files_panel_mut() {
                files.remote.invalidate();
            }
        }
        self.trigger_sftp_open(generation);
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
        // F154:本地目录收藏。**在借出 `files` 之前分流** —— 它们要
        // `&mut self`(store + 存盘),借着 `tab.content.files_panel_mut()`
        // 是调不了的;而且它们不改当前目录,不该走下面那条 `target` 的路。
        match &action {
            FileAction::BookmarkAdd { name, path } => {
                self.add_bookmark(
                    generation,
                    path.clone(),
                    name.clone(),
                    crate::files::PanelColumn::Local,
                );
                return;
            }
            FileAction::BookmarkRemove { path } => {
                self.remove_bookmark(generation, path.clone(), crate::files::PanelColumn::Local);
                return;
            }
            _ => {}
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
                mark_ui_dirty!(self.ui_dirty);
                return;
            }
            // F131:同远端那条,只是 home 来自本机。
            FileAction::GotoInput(input) => {
                let home = crate::files::local::home_dir();
                match crate::files::path_input::resolve_local_input(
                    input,
                    &files.local.cwd,
                    home.as_ref(),
                ) {
                    Some(p) => p,
                    None => return,
                }
            }
            // D5:本地栏不提供写操作,`menu_items_for` 也不会给出这些项 ——
            // 真到了这里说明菜单构造被改坏了,不静默吞掉。
            FileAction::Ask(ask) => {
                log::warn!("本地栏收到了写操作请求 {ask:?},已忽略(D5)");
                return;
            }
            // F200:同上 —— 本地栏根本进不了改名编辑态(`begin_rename` 的
            // 调用点只在远端那条路上)。
            FileAction::Rename { .. } => {
                log::warn!("本地栏收到了改名请求,已忽略(D5)");
                return;
            }
            // 上面已经分流走了(那里不需要借 `files`),走到这儿说明分流被删了。
            FileAction::Transfer
            | FileAction::Drop(_)
            | FileAction::Reconnect
            | FileAction::BookmarkAdd { .. }
            | FileAction::BookmarkRemove { .. } => return,
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
                    mark_ui_dirty!(self.ui_dirty);
                }
                return;
            }
        };
        let seq = files.local.begin_load(target.clone());
        let result = local::list_dir(&local::to_path(&target));
        files.local.accept(seq, result);
        mark_ui_dirty!(self.ui_dirty);
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
            // F139:收藏 / 取消收藏。**不走下面那条 `target` 的路** —— 它们
            // 不改当前目录,只改会话配置;而且要借 `self.store`,夹不进后面
            // 那段借着 `tab` 的代码里。
            FileAction::BookmarkAdd { name, path } => {
                self.add_bookmark(
                    generation,
                    path.clone(),
                    name.clone(),
                    crate::files::PanelColumn::Remote,
                );
                return;
            }
            FileAction::BookmarkRemove { path } => {
                self.remove_bookmark(generation, path.clone(), crate::files::PanelColumn::Remote);
                return;
            }
            // F200:就地改名提交。两条路径已经在面板里拼好、名字也已经过
            // `validate_name`(见 `FileAction::Rename` 的文档),这里只管发。
            // 同 `Ask`,在借出 `files` 之前分流 —— `apply_file_op` 要 `&mut self`。
            FileAction::Rename { from, to } => {
                let op = crate::ui::files_dialog::FileOp::Rename {
                    from: from.clone(),
                    to: to.clone(),
                };
                self.apply_file_op(generation, op);
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
        let home = self
            .tabs
            .by_generation(generation)
            .and_then(|t| t.content.sftp_home());
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
                mark_ui_dirty!(self.ui_dirty);
                return;
            }
            // F131:路径条敲的原文,在这里才解析 —— `~` 要用远端登录目录展开。
            // 解析不出来(空输入 / `~` 但还不知道登录目录)就什么都不做;
            // 真正跳不过去的路径交给远端报错(`spawn_sftp_list_dir` 失败会落
            // `Load::Failed`),不在客户端猜。
            FileAction::GotoInput(input) => {
                match crate::files::path_input::resolve_remote_input(
                    input,
                    &files.remote.cwd,
                    home.as_deref(),
                ) {
                    Some(p) => p,
                    None => return,
                }
            }
            // 这几个在函数开头就分流掉了(那里不需要借 `files`),走不到这儿。
            FileAction::Ask(_)
            | FileAction::OpenInExplorer
            | FileAction::Transfer
            | FileAction::Drop(_)
            | FileAction::EditExternal
            | FileAction::EditInline
            | FileAction::Reconnect
            | FileAction::BookmarkAdd { .. }
            | FileAction::BookmarkRemove { .. }
            | FileAction::Rename { .. } => return,
        };
        let seq = files.remote.begin_load(target.clone());
        let task =
            spawn_sftp_list_dir(&self._runtime, &self.proxy, generation, client, target, seq);
        self.track_sftp_task(generation, task);
        mark_ui_dirty!(self.ui_dirty);
    }

    /// F139:把一条书签写进会话配置,并同步这个标签的内存副本。
    ///
    /// **两处都要写**:`PanelFrame::bookmarks` 是这一帧画 ★/▾ 用的,store
    /// 那份是重启之后还在的。只写一处的话,要么星星不变实心、要么关掉客户端
    /// 收藏就没了。
    ///
    /// 存盘立刻做(`store.save()`),不攒着 —— 收藏是随手动作,用户不会为它
    /// 去点「保存」;攒到退出再写的话,一次崩溃就全没了。
    ///
    /// F154/F187:`column` 决定落在哪一份列表上。两栏的路径空间毫无关系
    /// (`D:\work` 和 `/var/log`),混着存会让路径条那句「当前 cwd 在不在
    /// 列表里」的现算判据在两栏之间串味。
    ///
    /// **两栏的存放位置也不同**(F187):远端书签挂会话(`/srv/app` 是那台
    /// 机器上的东西),本地书签是**全局**的、存 `settings.toml` —— `D:\work`
    /// 在这台 Windows 上跟连的是谁没有关系。于是本地那一支不需要
    /// `SessionId`,快速连接开的标签也能收藏。
    fn add_bookmark(
        &mut self,
        generation: u64,
        path: String,
        name: String,
        column: crate::files::PanelColumn,
    ) {
        let mark = mullion_store::Bookmark {
            name,
            path: path.clone(),
        };
        if column == crate::files::PanelColumn::Local {
            self.settings.add_local_bookmark(mark);
            if let Err(e) = self.save_settings() {
                self.ui.set_error(format!("收藏没能存下来:{e}"));
                return;
            }
            self.sync_local_bookmarks_to_tabs();
            mark_ui_dirty!(self.ui_dirty);
            return;
        }
        let Some(sid) = self
            .tabs
            .by_generation(generation)
            .and_then(|t| t.session_id)
        else {
            // UI 已经按 `BookmarkView::can_edit` 把远端栏的 ☆ 置灰了,走到
            // 这儿说明接线被改坏了 —— 不静默吞。
            log::warn!("收到远端 BookmarkAdd 但标签没有 SessionId,已忽略");
            return;
        };
        if let Some(store) = self.store.as_mut() {
            if let Err(e) = store
                .add_bookmark(sid, mark.clone())
                .and_then(|_| store.save())
            {
                self.ui.set_error(e.to_string());
                return;
            }
        }
        if let Some(files) = self
            .tabs
            .by_generation_mut(generation)
            .and_then(|t| t.content.files_panel_mut())
        {
            // 去重判据与 store 侧同一条(按路径),两边不许分叉。
            if !files.bookmarks.iter().any(|b| b.path == mark.path) {
                files.bookmarks.push(mark);
            }
        }
        mark_ui_dirty!(self.ui_dirty);
    }

    /// F139/F154/F187:取消收藏。按路径相等匹配 —— 书签的身份就是路径。
    /// `column` 的含义同 `add_bookmark`(含两栏存放位置不同那条)。
    fn remove_bookmark(
        &mut self,
        generation: u64,
        path: String,
        column: crate::files::PanelColumn,
    ) {
        if column == crate::files::PanelColumn::Local {
            self.settings.remove_local_bookmark(&path);
            if let Err(e) = self.save_settings() {
                self.ui.set_error(format!("取消收藏没能存下来:{e}"));
                return;
            }
            self.sync_local_bookmarks_to_tabs();
            mark_ui_dirty!(self.ui_dirty);
            return;
        }
        let Some(sid) = self
            .tabs
            .by_generation(generation)
            .and_then(|t| t.session_id)
        else {
            log::warn!("收到远端 BookmarkRemove 但标签没有 SessionId,已忽略");
            return;
        };
        if let Some(store) = self.store.as_mut() {
            if let Err(e) = store.remove_bookmark(sid, &path).and_then(|_| store.save()) {
                self.ui.set_error(e.to_string());
                return;
            }
        }
        if let Some(files) = self
            .tabs
            .by_generation_mut(generation)
            .and_then(|t| t.content.files_panel_mut())
        {
            files.bookmarks.retain(|b| b.path != path);
        }
        mark_ui_dirty!(self.ui_dirty);
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
                    mark_ui_dirty!(self.ui_dirty);
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
                // F202:Shift+Delete 跳过确认框直接删 —— 设计 D17
                // (远端删除不可逆、必须确认)**唯一的明示例外**,用户拿它
                // 清一批临时文件。裸 Delete 那条腿一个字都不能动。
                if matches!(key, WinitKey::Named(NamedKey::Delete)) && mods.shift_key() {
                    let targets = self
                        .tabs
                        .by_generation(generation)
                        .and_then(|t| t.content.files_panel())
                        .map(|f| f.remote.delete_targets())
                        .unwrap_or_default();
                    if targets.is_empty() {
                        return;
                    }
                    // 没有确认框,这句吐司就是用户唯一的回执 —— 它说的是
                    // 「正在」,成败等 `SftpOpDone`。
                    self.ui
                        .set_toast(crate::ui::files_dialog::deleting_toast(&targets));
                    self.apply_file_op(
                        generation,
                        crate::ui::files_dialog::FileOp::Delete { targets },
                    );
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
            // F200:改名**不弹框**,直接让那一行进编辑态。走到这里的两个
            // 入口(F2、右键「重命名」)都归它,不再有第二条路。
            FileAsk::Rename => {
                if let Some(files) = self
                    .tabs
                    .by_generation_mut(generation)
                    .and_then(|t| t.content.files_panel_mut())
                {
                    if files.remote.begin_rename() {
                        self.request_ui_redraw();
                    }
                }
                return;
            }
            FileAsk::Chmod => state.cursor.as_ref().and_then(|cur| {
                let e = state.entries.iter().find(|e| &e.name == cur)?;
                Some(FilesDialog::Chmod {
                    path: state.cwd.join(cur.as_bytes()),
                    mode: e.mode & 0o777,
                })
            }),
            FileAsk::Delete => {
                // F202:与免确认的 Shift+Delete **共用同一个算法**。各算一遍
                // 的话,确认框上列的和实际删掉的可以不是一回事,而免确认那条
                // 路上没有任何东西会让用户发现。
                let targets = state.delete_targets();
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
            self.transfer.queue.resolve_conflict(job, choice, apply_all);
            mark_ui_dirty!(self.ui_dirty);
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
        let conn = tab.content.sftp_connection_for(tab.content.sftp_host_ix());
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
            mark_ui_dirty!(self.ui_dirty);
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
            mark_ui_dirty!(self.ui_dirty);
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
            mark_ui_dirty!(self.ui_dirty);
            return;
        };
        if items.is_empty() {
            // 全被跳过(整批都是目录)。**必须说话** —— 不然用户拖出去
            // 松了手,什么都没发生,也没有任何提示。
            if skipped_dirs > 0 {
                self.ui
                    .set_error(format!("目录还不能拖出({skipped_dirs} 个),请选文件"));
                mark_ui_dirty!(self.ui_dirty);
            }
            return;
        }
        if skipped_dirs > 0 {
            self.ui
                .set_error(format!("跳过了 {skipped_dirs} 个目录,只拖文件"));
            mark_ui_dirty!(self.ui_dirty);
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
            mark_ui_dirty!(self.ui_dirty);
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
            mark_ui_dirty!(self.ui_dirty);
            return;
        }
        let remote = state.cwd.join(cur.as_bytes());
        let Some(client) = tab.content.sftp_client() else {
            self.ui
                .set_error("SFTP 通道还没建立,请先等目录加载完".into());
            mark_ui_dirty!(self.ui_dirty);
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
        mark_ui_dirty!(self.ui_dirty);
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
        mark_ui_dirty!(self.ui_dirty);
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
        let local = crate::edit::tempdir::temp_path(&self.edit.root, &session, &remote);
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
                    .edit
                    .sessions
                    .add(generation, kind, remote, local.clone(), snapshot);
                self.edit.originals.insert(key, bytes);
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
                let key = self
                    .edit
                    .sessions
                    .add(generation, kind, remote, local, snapshot);
                self.edit.originals.insert(key, bytes);
                self.edit.editor = Some(crate::ui::editor_window::EditorState::new(
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
        if let Some(old) = self.edit.watchers.insert(key, task) {
            old.abort();
        }
    }

    /// F53:看门任务报了一次本地状态。
    fn on_edit_tick(&mut self, key: u64, stamp: Option<crate::edit::sessions::LocalStamp>) {
        for k in self.edit.sessions.changed_locally(&[(key, stamp)]) {
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
        let Some(e) = self.edit.sessions.get(key) else {
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
            self.edit.originals.get(&key).cloned()
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
        if let Some(e) = self.edit.sessions.get_mut(key) {
            e.state = EditState::Uploading;
        }
        let proxy = self.proxy.clone();
        let task = self._runtime.spawn(async move {
            let result = write_back(&client, &remote, &bytes, snapshot, backup).await;
            let _ = proxy.send_event(UserEvent::EditSaved { key, result });
        });
        self.track_sftp_task(generation, task);
        mark_ui_dirty!(self.ui_dirty);
    }

    /// 把一条编辑标成失败。**不弹错误框** —— 失败原因就写在「编辑中」那一行上,
    /// 用户正在别的窗口里改字,一个模态框只会打断他。
    fn fail_edit(&mut self, key: u64, why: String) {
        use crate::edit::sessions::EditState;
        let why2 = why.clone();
        if let Some(e) = self.edit.sessions.get_mut(key) {
            e.state = EditState::Failed(why);
        }
        if let Some(ed) = self.edit.editor.as_mut().filter(|ed| ed.key == key) {
            ed.finish_save(Err(why2));
        }
        mark_ui_dirty!(self.ui_dirty);
    }

    /// F53:一次回传收工。
    fn on_edit_saved(&mut self, key: u64, result: Result<EditWriteOutcome, String>) {
        use crate::edit::sessions::EditState;
        mark_ui_dirty!(self.ui_dirty);
        let (generation, local, label) = match self.edit.sessions.get(key) {
            Some(e) => (e.generation, e.local.clone(), e.label.clone()),
            // 条目在这次往返里被「结束编辑」掉了 —— 结果丢掉就是对的。
            None => return,
        };
        match result {
            Ok(EditWriteOutcome::Done(remote_now)) => {
                self.edit
                    .sessions
                    .accept_write_back(key, remote_now, local_stamp(&local));
                self.edit.originals.remove(&key);
                self.edit.conflicts.remove(&key);
                if let Some(ed) = self.edit.editor.as_mut().filter(|ed| ed.key == key) {
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
                self.edit.conflicts.insert(key, remote_now);
                if let Some(e) = self.edit.sessions.get_mut(key) {
                    e.state = EditState::Conflict;
                }
                if let Some(ed) = self.edit.editor.as_mut().filter(|ed| ed.key == key) {
                    ed.finish_save(Err("远端已被改动,见处置框".into()));
                }
                self.open_edit_conflict(key);
            }
            Err(why) => self.fail_edit(key, why),
        }
    }

    /// F53:开(或重开)一条编辑的冲突处置框。
    fn open_edit_conflict(&mut self, key: u64) {
        let Some(e) = self.edit.sessions.get(key) else {
            return;
        };
        self.ui.files_dialog = Some(crate::ui::files_dialog::FilesDialog::EditConflict {
            name: e.remote.display().to_string(),
            key,
        });
        mark_ui_dirty!(self.ui_dirty);
    }

    /// F53:用户在冲突框里选完了。
    fn resolve_edit(&mut self, key: u64, choice: crate::ui::files_dialog::EditResolve) {
        use crate::edit::sessions::EditState;
        use crate::ui::files_dialog::EditResolve;
        mark_ui_dirty!(self.ui_dirty);
        // 没有记到远端当时的戳就没法安全处置(条目已经被收走之类)。
        let Some(remote_now) = self.edit.conflicts.get(&key).copied() else {
            return;
        };
        match choice {
            EditResolve::KeepRemote => {
                // D3-9:**必须刷快照**。不刷的话下一次保存还会撞上同一个
                // 冲突,这个框永远关不掉。
                self.edit.sessions.keep_remote(key, remote_now);
                self.edit.conflicts.remove(&key);
                self.ui.set_toast("已保留远端那一份");
            }
            EditResolve::Overwrite => {
                // 把比对基准换成远端当前值,再走同一条回传 —— 于是这一次
                // `stat` 一定对得上,写下去。
                if let Some(e) = self.edit.sessions.get_mut(key) {
                    e.snapshot = remote_now;
                    e.state = EditState::Watching;
                }
                self.edit.conflicts.remove(&key);
                let bytes = self.editor_bytes_for(key);
                self.push_edit(key, bytes);
            }
            EditResolve::SaveCopy => {
                let Some(e) = self.edit.sessions.get(key) else {
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
                self.edit.sessions.keep_remote(key, remote_now);
                self.edit.conflicts.remove(&key);
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
        self.edit
            .editor
            .as_ref()
            .filter(|e| e.key == key)
            .map(|e| e.bytes())
    }

    /// F53:结束一条编辑 —— 停看门、删临时文件、从列表里去掉。
    fn end_edit(&mut self, key: u64) {
        if let Some(task) = self.edit.watchers.remove(&key) {
            task.abort();
        }
        self.edit.originals.remove(&key);
        self.edit.conflicts.remove(&key);
        if let Some(e) = self.edit.sessions.remove(key) {
            // 删不掉只记一条:临时文件残留不影响正确性,退出时那次
            // `tempdir::purge` 还会再扫一遍。
            if e.kind == crate::edit::sessions::EditKind::External {
                if let Err(err) = std::fs::remove_file(&e.local) {
                    log::debug!("删临时文件失败({}):{err}", e.local.display());
                }
            }
        }
        if self.edit.editor.as_ref().is_some_and(|e| e.key == key) {
            self.edit.editor = None;
        }
        mark_ui_dirty!(self.ui_dirty);
    }

    /// F55/F56:每帧调一次 —— 队列放行几条就起几条 worker。
    ///
    /// **每条 job 自己开一条 sftp channel**(worker 里的 `SftpClient::open`):
    /// 共用一条的话请求在同一个 session 上串行,并发度实际等于 1,设计 D8
    /// 说的吞吐问题原样还在。
    fn pump_transfers(&mut self) {
        for id in self.transfer.queue.take_runnable() {
            let Some(spec) = self.transfer.specs.get(&id).cloned() else {
                self.transfer.queue.finish(id, Err("任务参数丢了".into()));
                continue;
            };
            let Some(tab) = self.tabs.by_generation(spec.generation) else {
                // 属主标签没了 —— 关标签那条路径已经 `cancel_generation` 过,
                // 这里是兜底,不该再当成"失败"报给用户。
                self.transfer.queue.cancel(id);
                continue;
            };
            let Some(conn) = tab.content.sftp_connection_for(tab.content.sftp_host_ix()) else {
                self.transfer.queue.finish(id, Err("连接已断开".into()));
                continue;
            };
            let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
            self.transfer.cancels.insert(id, cancel.clone());
            // 冲突处置结果随 job 存在队列里(重跑时才知道该覆盖还是改名)。
            let resolved = self.transfer.queue.get(id).and_then(|j| j.resolved);
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
        mark_ui_dirty!(self.ui_dirty);
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
    /// 接)。两种宿主取连接的来源不同(`TabContent::sftp_connection_for` 已经
    /// 把差异收掉):
    /// - `Terminal`(侧栏,D1 之前就有):蹭会话已建立的连接
    ///   (`SftpClient::open` 的签名里刻意没有网络参数),不重新握手。取
    ///   `焦点分屏所在那台`(F132,`focused_pane_host_ix`)——用户按「换节点」
    ///   把某块 pane 挪到第二台机器上之后,新开的侧栏该连它此刻正看着的那台,
    ///   不是这个标签最早建立的那台。真实归属要等 channel 打开成功才落
    ///   `sftp_host_ix`(`accept_sftp_opened`),这里只是**发起**用的意图。
    /// - `Files`(D1 标签宿主):独占的连接(`establish` 单独建的那条,
    ///   ADR-010 同款理由),`sftp_connection_for` 不看 `host_ix`,恒 `Some`。
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
        let host_ix = tab.content.focused_pane_host_ix();
        let Some(conn) = tab.content.sftp_connection_for(host_ix) else {
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
            host_ix,
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
        host_ix: Option<usize>,
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
                    // F132:记住这条 channel 的真实归属。**在这里记,不在发起处**
                    // —— 开 channel 是一次网络往返,期间焦点分屏可能已经换了。
                    // 直接匹配 `TabContent::Terminal` 变体(不借那个「只认终端
                    // 标签」的专用访问器,理由见 `file_actions_never_narrow_…`
                    // 那条守护的文档)——`sftp_host_ix` 是 `TerminalTab` 独有
                    // 字段,`FilesTab` 没有也不需要这个概念,不属于要跨变体
                    // 共享的那六条通用行为,不踩那条守护的意图。
                    if let TabContent::Terminal(t) = &mut tab.content {
                        t.sftp_host_ix = host_ix;
                    }
                    // F123:登录目录存下来,给「侧栏关→开跃迁」那条路展开 `~`。
                    tab.content.set_sftp_home(home);
                    let Some(files) = tab.content.files_panel_mut() else {
                        return;
                    };
                    // F142:新连接 = 新的一套 uid/gid 映射。同一个 1000 在
                    // 两台机器上是两个人,不清就是把 A 机的名字画在 B 机的
                    // 文件上(换节点、断线重连都会走到这里)。
                    files.remote.owners.clear();
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
        mark_ui_dirty!(self.ui_dirty);
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
        // F142:这一屏出现了哪些还没问过名字的 uid/gid。在借用块里算、出块
        // 之后再发 —— `spawn` 要 `&self._runtime`/`&self.proxy`,而这里
        // `tab` 攥着 `&mut self.tabs`。
        let mut ask: Option<crate::files::owners::Query> = None;
        if let Some(tab) = self.tabs.by_generation_mut(generation) {
            if let Some(files) = tab.content.files_panel_mut() {
                let pane = &mut files.remote;
                match result {
                    Ok(entries) => {
                        pane.accept(seq, Ok(entries));
                        // **`accept` 之后才问**:它内部会按 `seq` 丢弃后发先至
                        // 的旧结果,拿被丢弃的那批 entries 去问就是白跑一趟
                        // 网络往返(而且会把一批用不上的 id 记进负缓存)。
                        ask = pane.owners.take_missing(&pane.entries);
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
            // F142:连接刚被判定死亡时**不问** —— 那条命令一定失败,徒增一次
            // 无谓往返。负缓存不撤也没关系:重开连接时 `accept_sftp_opened`
            // 会把整份缓存清掉。
            //
            // 取连接的方式与 `trigger_sftp_open` 同源(`sftp_connection_for`
            // 收掉了两种宿主的差异),但这里用**已落定的** `sftp_host_ix` 而不是
            // `focused_pane_host_ix`:名字要查的是「这批 entries 是从哪台机器
            // 列出来的」,不是「用户此刻焦点在哪台」——列目录那一次网络往返
            // 期间用户可能刚换过节点,拿焦点那台去问就是在 B 机上查 A 机的 uid。
            let conn = tab
                .content
                .sftp_connection_for(tab.content.sftp_host_ix())
                .filter(|_| !just_disconnected);
            match (ask.take(), conn) {
                (Some(q), Some(conn)) => {
                    let task = spawn_getent(&self._runtime, &self.proxy, generation, conn, q);
                    self.track_sftp_task(generation, task);
                }
                // 问不成:把负缓存撤回,下次列目录重新问(同 `spawn_getent`
                // 失败那条路)。
                (Some(q), None) => {
                    if let Some(tab) = self.tabs.by_generation_mut(generation) {
                        if let Some(files) = tab.content.files_panel_mut() {
                            files.remote.owners.forget(&q);
                        }
                    }
                }
                (None, _) => {}
            }
        } else {
            log::debug!(target: "mullion", "丢弃过期世代 {generation} 的目录列表(seq={seq})");
        }
        mark_ui_dirty!(self.ui_dirty);
        self.request_ui_redraw();
    }

    /// F142:一次 `getent` 的结果落地。世代对不上就丢(S1,同 `accept_sftp_listed`);
    /// **`stdout: None`(没发出去/远端拒了)要撤回负缓存**,否则这批 id 再也
    /// 不会被问第二次。
    fn accept_owner_names(
        &mut self,
        generation: u64,
        query: crate::files::owners::Query,
        stdout: Option<Vec<u8>>,
    ) {
        let Some(tab) = self.tabs.by_generation_mut(generation) else {
            log::debug!(target: "mullion", "丢弃过期世代 {generation} 的属主名字");
            return;
        };
        let Some(files) = tab.content.files_panel_mut() else {
            return;
        };
        match stdout {
            Some(out) => files.remote.owners.merge(&out),
            None => files.remote.owners.forget(&query),
        }
        mark_ui_dirty!(self.ui_dirty);
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
                mark_ui_dirty!(self.ui_dirty);
                false
            }
            _ => false,
        }
    }

    fn request_ui_redraw(&mut self) {
        mark_ui_dirty!(self.ui_dirty);
        if let Some(a) = &self.active {
            diag::count_request_redraw(diag::RedrawSource::Event);
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

    /// 「点哪块就切到哪块」(F33)。左键划选(F18)与右键粘贴(F185)共用。
    ///
    /// 提成函数**是为了让两个入口不会各写各的**:两边都得在「按键要干的正事」
    /// 之前切焦点(锚点 / 落点都取 `effective_focus()`),而右键那一支此前
    /// 压根没切,分屏下右键会把剪贴板贴进上一块 pane —— 没有报错、没有日志,
    /// 只有内容出现在错的地方。
    ///
    /// 指针不在任何 pane 上(点在分界线 / 内缩留白上)则什么都不做:那种时候
    /// 把焦点清掉或乱指一块都比保持原样更糟。
    fn focus_pane_under_cursor(&mut self) {
        let Some(id) = self.pane_at(self.cursor_px) else {
            return;
        };
        // F199:点中了一块 pane,键盘焦点就该从文件面板回到终端。
        // 在这之前只有 F6 改得动 `self.focus`:用户点一下侧栏再回头点终端接着
        // 打字,每一个字都进了面板的按键处理,远端一个字都收不到,而画面上
        // 光标还在闪。**放在 `pane_at` 之后**:指针落在分界线/内缩留白上时
        // 什么都不改,同这个函数原本的口径。
        self.focus = shell::input_route::Focus::Terminal;
        if let Some(ws) = self.active_ws_mut() {
            if ws.focus() != id {
                ws.set_focus(id);
                mark_ui_dirty!(self.ui_dirty);
            }
        }
    }

    /// 焦点 pane 的几何。鼠标格换算、划选都基于它。
    fn focused_geom(&self) -> Option<PaneGeom> {
        let a = self.active.as_ref()?;
        let f = self.active_ws()?.focus();
        a.geoms.iter().find(|g| g.id == f).copied()
    }

    /// F210:焦点 pane 的 id + 它这一刻的**真**光标格 `(col, row)`。
    /// 只在组字开始时被 [`input::ImeState::on_preedit`] 记成锚点。
    fn focused_cursor_cell(&self) -> Option<(PaneId, (u16, u16))> {
        let ws = self.active_ws()?;
        let id = ws.focus();
        let c = ws.focused()?.emulator.cursor();
        Some((id, (c.col, c.row)))
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
        // F126:组字中候选框要跟拼音串末尾走,不是原始光标列 —— 与
        // `gpu::quads_for_panes`/`text::prepare_panes` 摆字用的是同一个
        // `preedit_cursor_col`,否则候选框和内联拼音的视觉光标位置对不上。
        let dims = self
            .active_ws()
            .and_then(Workspace::focused)
            .map(|p| (p.emulator.cols(), p.emulator.rows()))
            .unwrap_or((cur.col + 1, cur.row + 1));
        // F210:组字期间钉在锚点上,不跟远端重绘时乱跑的真光标 —— 与下面
        // 渲染路径喂给 `PaneRender` 的那份光标**必须**是同一个
        // `anchored_cursor`,不同源的话候选框和内联拼音会分家。
        let (acol, arow) = self.ime.anchored_cursor(g.id, (cur.col, cur.row), dims);
        let col = crate::text::preedit_cursor_col(dims.0, acol, self.ime.preedit());
        let area = ime_cursor_area(g.term_px, (col, arow), cell_w, cell_h);
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
        // F125:粘贴也是输入,重置闪烁相位——否则在暗周期粘贴,光标最长要等
        // 530ms 才转亮,与「刚有动作光标一定亮」的意图相悖。必须在借出
        // `self.active_ws_mut()` 之前做,否则跟下面 `pane` 的可变借用冲突
        // (同 `self.user_took_over()` 挪到 `pane` 借用结束之后的理由一样)。
        self.last_input_at = Instant::now();
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

    /// F209:终端里裸 `Ctrl+V`,剪贴板里是一张位图 —— 编码、传远端、把绝对
    /// 路径打进这块 pane 的输入行。
    ///
    /// 解码 + PNG 编码就在窗口线程上做:一张 4K 图几十毫秒,比开一条 channel
    /// 的一次 RTT 还便宜(本项目的主场景是高延迟代理链路),不值得为它再搬
    /// 一个线程。**上传**才是慢的那一段,那一段在后台 task 上。
    ///
    /// **另开一条 sftp channel,不蹭侧栏那条。** 侧栏那条记在
    /// `sftp_host_ix` 上,而它未必是焦点分屏此刻所在的那台机器(F132);蹭错
    /// 的后果是图传到了另一台机器上,而打进输入行的路径看起来完全正常。
    fn paste_screenshot(&mut self, dib: &[u8]) {
        let png = match crate::shot::dib_to_bitmap(dib).and_then(|bm| crate::shot::encode_png(&bm))
        {
            Ok(png) => png,
            Err(e) => {
                self.ui.set_error(format!("这张截图读不出来:{e}"));
                return;
            }
        };
        if png.len() > crate::shot::MAX_PNG_BYTES {
            self.ui.set_error(format!(
                "截图编码后 {:.1} MB,超过 {} MB 上限,没有上传",
                png.len() as f64 / (1024.0 * 1024.0),
                crate::shot::MAX_PNG_BYTES / (1024 * 1024)
            ));
            return;
        }

        let Some(tab) = self.tabs.active() else {
            return;
        };
        let generation = tab.content.generation();
        // 只有终端标签有输入行可以回填。文件标签/占位标签上按 Ctrl+V 什么都
        // 不做 —— 传上去也没有地方把路径交给用户。
        let Some(pane) = tab.content.as_terminal().map(|t| t.ws.focus()) else {
            return;
        };
        let host_ix = tab.content.focused_pane_host_ix();
        let Some(conn) = tab.content.sftp_connection_for(host_ix) else {
            return;
        };
        let dir = tab
            .content
            .as_terminal()
            .and_then(|t| t.sftp_screenshot_dir.clone())
            .unwrap_or_default();

        self.shot_seq += 1;
        let stamp = crate::shot::stamp(
            time::OffsetDateTime::now_utc().unix_timestamp(),
            crate::localtime::offset(),
        );
        let path = crate::shot::remote_join(&dir, &crate::shot::file_name(&stamp, self.shot_seq));

        let proxy = self.proxy.clone();
        let bytes = png.len();
        let task = self._runtime.spawn(async move {
            let remote = mullion_ssh::sftp::RemotePath::from_bytes(path.clone().into_bytes());
            let result = async {
                let client = mullion_ssh::sftp::SftpClient::open(conn)
                    .await
                    .map_err(|e| format!("开 SFTP 通道失败:{e}"))?;
                client
                    .write_all_truncate(&remote, &png)
                    .await
                    .map_err(|e| format!("截图上传失败:{e}"))?;
                Ok(path)
            }
            .await;
            let _ = proxy.send_event(UserEvent::ShotUploaded {
                generation,
                pane,
                result,
            });
        });
        self.track_sftp_task(generation, task);
        // 用户按了键就是接管(同 `send_paste`)。**在这里掐,不在结果回来时
        // 掐**:那时字节早发出去了,而自动化可能已经往同一个 shell 里灌了
        // 半条命令(T11 同族)。
        self.user_took_over();
        self.ui
            .set_toast(format!("正在上传截图({} KB)…", bytes / 1024));
    }

    /// F209:截图传完了。成功才往终端里打路径,失败只提示 —— **绝不把半截
    /// 路径发下去**,那会在用户的输入行里留下一段他没打过、又必须手动删掉的
    /// 垃圾。
    fn accept_shot_uploaded(
        &mut self,
        generation: u64,
        pane: PaneId,
        result: Result<String, String>,
    ) {
        let path = match result {
            Ok(p) => p,
            Err(msg) => {
                self.ui.set_error(msg);
                self.request_ui_redraw();
                return;
            }
        };
        // F125:光标相位复位——这一下之后输入行会动,光标该是亮的。
        self.last_input_at = Instant::now();
        let landed = self
            .tabs
            .by_generation_mut(generation)
            .and_then(|t| t.content.as_terminal_mut())
            .and_then(|t| t.ws.pane_mut(pane))
            .map(|p| {
                // 绝对路径 + 一个空格,不带回车:用户接着打字就是自然的下一
                // 句话,要不要发是他自己的事。
                let mut out = path.clone().into_bytes();
                out.push(b' ');
                // 与粘贴同理(F17):先回底部,否则「贴了但看不到」。
                p.emulator.scroll_to_bottom();
                let _ = p.pty.write(out);
            })
            .is_some();
        // 这里**不再** `user_took_over()`:那一下在 `paste_screenshot` 按键
        // 当时就做过了(而且这块 pane 未必还是焦点,在这里掐会掐错人)。
        if landed {
            self.ui.set_toast(format!("截图已传到 {path}"));
        } else {
            // 标签或分屏在这几秒里没了。图确实传上去了,路径只能靠提示给他。
            self.ui
                .set_toast(format!("截图已传到 {path}(原来那块分屏不在了)"));
        }
        self.request_ui_redraw();
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
            // F159:`Gpu::resize` 内部也会重新 configure surface,之后交换链内容
            // 未定义,旧的整帧指纹基准必须作废——道理与 render_frame 里
            // Lost/Outdated 分支重新 configure 后作废基准是同一件事。放在这里
            // 而不是 `Gpu::resize` 内部:`Gpu` 拿不到 `Active`,基准字段
            // 是 `Active` 的字段。另外这条路不能省——最小化后还原到**原尺寸**时
            // `frame_fingerprint` 里唯一的几何项(config.width/height)没变,若
            // 基准没作废,下一帧会误判命中而在未定义内容的交换链上提前 return。
            a.last_frame_fp = None;
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
            mark_ui_dirty!(self.ui_dirty);
        }
        self.drive_automation();
        self.drive_attach_checks();
    }

    /// F163:每帧推进在途的 attach 校验。挂在 `drive_*` 那一组里。
    ///
    /// **遍历的是 `attach_checks` 不是活动标签**:每条自带世代号,校验途中
    /// 用户完全可能切到别的标签去(那条「`drive_*` 每帧驱动函数必须遍历
    /// 全部标签」的同源教训)。
    fn drive_attach_checks(&mut self) {
        if self.attach_checks.is_empty() {
            return;
        }
        let checks = std::mem::take(&mut self.attach_checks);
        let (pending, dirty) = drive_attach_checks_of(&mut self.tabs, checks, Instant::now());
        self.attach_checks = pending;
        if dirty {
            mark_ui_dirty!(self.ui_dirty);
        }
    }

    /// 一块 pane 挂上了、写口拿到手了 —— 三条路径(首次连接 / 分屏新开 /
    /// 换节点)共用的落地动作。
    ///
    /// **必须是唯一入口。** 三处各写一遍,正是「列举式门控在加档时必然漏」,
    /// 本项目已经踩中三次;漏一处的症状是那种方式开出来的 pane 永远跟不住
    /// 目录,而且完全静默。守护:`every_pane_ready_path_goes_through_on_pane_ready`。
    ///
    /// 做两件事,**顺序不能反**:
    /// 1. F156-c:注入一次 OSC 7 上报(见 `shell_bootstrap::OSC7_SETUP`)。
    /// 2. 有计划的话,起 F40~F44 的登录后自动化。
    ///
    /// 注入串自带 `clear`,排在自动化之后会把用户登录后命令的输出清掉一半。
    ///
    /// **只在 pane 刚建立、shell 还没跑任何程序时注入。** 这是唯一安全的窗口
    /// —— 换到 `Ctrl+Shift+B` 那一刻现写的话,pane 里可能正跑着 Claude Code
    /// 之类的全屏 TUI,写进去的字节会变成那个 TUI 的按键输入。
    ///
    /// `ByteSink::write` 是**同步**的(`try_send` 语义),不需要起 task。
    /// 写失败(出站队列满 / channel 已死)只记一行日志:这是锦上添花的功能,
    /// 拿不到目录就退回 F120 配置的默认远端目录,不该把连接本身搅黄。
    ///
    /// `may_clear_screen`:断线重连传 `false` —— 那条路径的 shell 是
    /// `reattach_pane` 刚接回来的,本地屏幕内容被刻意保留,注入串自带的
    /// `clear` 会把它抹掉。其余三处调用点传 `true`。
    fn on_pane_ready(
        &mut self,
        generation: u64,
        pane: PaneId,
        sink: Arc<mullion_ssh::session::SshSession>,
        plan: Option<crate::automation::PendingAutomation>,
        may_clear_screen: bool,
    ) {
        // F156-c 回归修复:断线重连(F128)刻意保留断线前的屏幕内容
        // (`reattach_pane`,守护见 `reattach_keeps_the_screen_but_rehost_wipes_it`)。
        // OSC 7 注入串以 `clear` 收尾,对**每一块**重连成功的 pane 无条件发的话
        // 会把刚保住的内容抹掉,跟这块 pane 有没有登录后命令(`plan`)毫无关系。
        // `PaneReconnected` 分支传 `false`,其余三处(首次连接 / 分屏新开 /
        // 换节点)传 `true` —— 那三种情况下这块 pane 从用户视角看本来就是
        // 「全新出现」的,清屏没有代价。
        if may_clear_screen && self.settings.shell_osc7_bootstrap {
            let bytes = crate::shell_bootstrap::osc7_setup_line();
            if let Err(e) = mullion_ssh::schedule::ByteSink::write(sink.as_ref(), bytes) {
                log::warn!(
                    target: "mullion",
                    "pane {} 的 OSC 7 自举没发出去({e:?}),这条 shell 不会报当前目录",
                    pane.0
                );
            }
        }
        // F161/D7:这块 pane 有「当初在哪个 tmux 会话里」的记录 → **只发 attach**,
        // 调用方算好的配置计划整个跳过。两者不能叠加:attach 一旦生效,屏幕就
        // 归那个 TUI 了,之后发任何字节都是打进 TUI。
        //
        // 收口在这里而不是各调用点:三条建立路径(首次连接 / 分屏新开 /
        // 换节点)+ 断线重连都要走同一条规则,分头写迟早走样,而走样的现象是
        // 「某块 pane 接回了别人的会话」。守护:
        // `every_pane_ready_path_goes_through_on_pane_ready`。
        let plan = match self.take_attach_intent(generation, pane) {
            Some(p) => Some(p),
            None => plan,
        };
        if let Some(plan) = plan {
            self.start_automation(generation, pane, plan, sink);
        }
    }

    /// F161:取走这块 pane 的「该接回哪个 tmux 会话」记录并算成计划。
    ///
    /// **取走**语义:恰好生效一次。留着的话,同一块 pane 下次因为别的原因
    /// 再走一遍 `on_pane_ready`(比如断线重连)会拿一个陈旧的会话名去 attach。
    ///
    /// **先判断、产出得了才消费**,顺序不能倒过来:`automation_template` 是
    /// `ConnectOk` 那一帧才写进标签的,倒过来写会让正确性依赖「模板赋值排在
    /// `on_pane_ready` 之前」这条谁都看不见的前提 —— 那一步一旦被挪后,记录
    /// 被吃掉而 attach 永远不发,且完全静默。
    ///
    /// 记录的两个来源共用这一个表(设计要求「恢复与 F128 断线重连共用」):
    /// - 恢复:从 `layout.toml` 的叶子读回来的上次实测名
    /// - 重连:断线前那块 pane 自己量到的名(下一个 task 填)
    fn take_attach_intent(
        &mut self,
        generation: u64,
        pane: PaneId,
    ) -> Option<crate::automation::PendingAutomation> {
        let t = self
            .tabs
            .by_generation_mut(generation)
            .and_then(|tab| tab.content.as_terminal_mut())?;
        let ix = t.leaf_wanted.iter().position(|(id, _)| *id == pane)?;
        let name = t.leaf_wanted[ix].1.tmux.clone()?;
        // F161/修复2:按**这块叶子自己那条会话**的自动化设置来,不是标签级
        // `automation_template`(那是主叶子那次连接写下的 —— 恢复出来的 Dial
        // 叶子连的是另一台机器,见 `automation_for_leaf` 文档)。
        let session_id = t.leaf_wanted[ix].1.session_id;
        let tpl = automation_for_leaf(
            self.store.as_ref(),
            session_id,
            t.automation_template.clone(),
        )?;
        let detach_ix = t.leaf_detach.iter().position(|(id, _)| *id == pane);
        let detach = detach_ix.is_some_and(|i| t.leaf_detach[i].1);
        let plan = crate::automation::pending_for_measured_attach(&tpl, &name, detach)?;
        // 到这里才动表:上面任何一条早退都不该把记录吃掉。
        t.leaf_wanted.remove(ix);
        if let Some(i) = detach_ix {
            t.leaf_detach.remove(i);
        }
        // F163:发完 attach 之后要真的比对它接上没有 —— 但这里只是**打算发**,
        // 字节真正发完是 `AutomationDone` 抵达的那一刻,所以 `deadline` 先按
        // `None` 入队(意味着「还不许下结论」),由 `arm_or_drop_attach_check`
        // 在 automation 收摊时补上,见 `AttachCheck::deadline` 的文档。
        // D4 的边界:远端标题上报关着时不许下失败结论(会恒误报)。
        // 判据是**全局设置**那个开关(`Settings::tmux_bootstrap`,F124),
        // 不是 `HostConn::tmux_bootstrap`(那是个 `BootstrapFlags`,记的是
        // 「这条连接上发过了没有」的进度,不是「用户想不想要」)。
        // 同 `tick_tmux_bootstrap` 里那句 `let enabled = self.settings.tmux_bootstrap;`。
        if should_check_attach(self.settings.tmux_bootstrap) {
            // F163/修复5:同一块 pane 抖动重连时可能在旧校验还没到期时
            // 又发一条 attach —— 先去重,不然同一块 pane 堆着好几条校验。
            self.attach_checks
                .retain(|c| !(c.generation == generation && c.pane == pane));
            self.attach_checks.push(AttachCheck {
                generation,
                pane,
                name: name.clone(),
                deadline: None,
            });
        }
        Some(plan)
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
                // 判据在 `automation::should_cancel_on_status`(纯函数,可单测)。
                // **不要写成 `== Disconnected`**:F128 之后链路死了先落到
                // `Reconnecting`,那样写会让这份自动化一直挂到自己的
                // ready_timeout 才收场(见那个函数的文档)。
                if crate::automation::should_cancel_on_status(pane.status) {
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

    /// F17:把改过的 `scrollback` 推给**已经在跑的** pane,不必重连。
    ///
    /// 用户改完保存,期待的是「立刻按新深度往上翻」,而不是「下次连上才算」——
    /// 后者正是这个项目反复踩过的「配了没反应」。往小调时 alacritty 会真把
    /// 多余的行释放掉(`Emulator::set_history` → `shrink_lines`),所以这条
    /// 路径同时也是**内存能收回来**的那条。
    ///
    /// 按标签的 `session_id` 逐标签解析:一个窗口里可以同时开着好几条会话
    /// 的标签,拿活动标签的配置刷全部就串味了。快速连接的标签(`session_id`
    /// 为 `None`)落回内置默认,与它连接时拿到的值一致,等于不动。
    ///
    /// **已知缺口**:唯一的触发点是「在会话管理器里保存」,而 `scrollback`
    /// 目前**没有编辑控件**(F17 只做完了 store 这一头)。也就是说这条路径
    /// 现在只可能被「改了同一条会话的别的字段」顺带带起来。等 UI 补上,
    /// 这里不需要改。
    fn refresh_scrollback(&mut self) {
        for tab in self.tabs.iter_mut() {
            let n = resolved_scrollback(self.store.as_ref(), tab.session_id);
            let Some(t) = tab.content.as_terminal_mut() else {
                continue;
            };
            for p in t.ws.panes_mut_iter() {
                p.emulator.set_history(n);
            }
        }
    }

    /// 在 `_runtime` 上异步连接;结果经 `proxy` 以 `UserEvent` 回送(§5)。
    /// 不阻塞调用方(winit 事件循环线程)。拆成 `establish` + `open_pty` 两步
    /// (而不是直接调更省事的 `session::connect`):分屏(F35)要在同一条连接上
    /// 另开 channel,必须拿到 `establish` 返回的 `Handle` 本身——`connect` 内部
    /// 会把它吞掉不外露。
    ///
    /// D1/F50:`wants_sftp` 由调用方(点击那一刻)算好传入。为 `true` 时**跳过
    /// `open_pty`**:SFTP 节点没有 PTY 这个概念,`establish` 一成功就直接
    /// 回送(`pty: None`),不做那趟本来就用不上的 shell 握手。
    ///
    /// F205:`session_id`/`skip_automation` 同样由调用方在点击那一帧给定,
    /// 与这次拨号的其余随行数据一起**装进一张票**存进 `self.dials`,票号
    /// 随任务走。从前这些都写在 `App` 的单槽上,第二次拨号会把第一次的
    /// 整体盖掉 —— 见 `shell::dial_ledger` 的模块文档。
    fn spawn_connect(
        &mut self,
        cfg: SshConfig,
        wants_sftp: bool,
        session_id: Option<SessionId>,
        skip_automation: bool,
    ) {
        // F40~F44:此刻才确定「是哪条会话」。连接在途期间用户可能改配置甚至
        // 删会话,所以计划必须在用户点击的这一帧定死。
        // 上一次的结论到此为止:新连接开始了,旧结论就是误导信息。
        // (Task 2 保持替换语义,清的就是马上要被替换掉的那个标签。)
        if let Some(t) = self.active_term_mut() {
            t.automation_status = None;
        }
        // 同一次解析里顺手留一份**原件**:后来的 pane(分屏新开的、换过节点的)
        // 要按它算「跳过 tmux」的计划。绝不等到分屏那一刻再查库——那时用户可能
        // 已经改了配置(见 `PendingAutomationState::template` 的文档)。
        let mut tpl: Option<mullion_store::ResolvedAutomation> = None;
        // F141:同一次解析里把会话名也留一份 —— 重连要按**当初**那个名字把
        // tmux 会话接回来(见 `PendingAutomationState::session_name`)。
        let mut fallback_name: Option<String> = None;
        let plan = crate::automation::pending_for(session_id, |id| {
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
            fallback_name = Some(name.clone());
            Some((resolved.automation, name))
        });
        // F205:这次拨号的随行数据整份装票。`cfg` 也在票里 —— 不装的话
        // 第二次连接后在第一个标签上开分屏,会用上一台主机的 term/尺寸
        // (F35 的 `open_pty` 靠 `TerminalTab::last_cfg`)。
        let dial = self.dials.issue(DialTicket {
            session_id,
            cfg: cfg.clone(),
            automation: PendingAutomationState {
                plan,
                template: tpl,
                session_name: fallback_name,
                skip: skip_automation,
            },
        });
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
                    let _ = proxy.send_event(UserEvent::ConnectErr(dial, e.to_string()));
                    return;
                }
            };
            if wants_sftp {
                let _ = proxy.send_event(UserEvent::ConnectOk {
                    dial,
                    handle,
                    wants_sftp: true,
                    pty: None,
                });
                return;
            }
            match mullion_ssh::session::open_pty(handle.clone(), &cfg, wake).await {
                Ok((ssh, rx)) => {
                    let _ = proxy.send_event(UserEvent::ConnectOk {
                        dial,
                        handle,
                        wants_sftp: false,
                        pty: Some((ssh, rx)),
                    });
                }
                Err(e) => {
                    let _ = proxy.send_event(UserEvent::ConnectErr(dial, e.to_string()));
                }
            }
        });
    }

    /// `spawn_connect` 拨号成功后的落地:开标签、建 `Workspace`、接自动化。
    ///
    /// 从 `user_event` 的 `UserEvent::ConnectOk` 分支原样搬出来(218 行),
    /// **一条语句都没动**。搬出来的理由是那个 `match` 已经长到 836 行,
    /// 而这一支自己就占 224 行 —— 读它得先滚过前面十几个变体。
    ///
    /// 分支里的两处 `return` 语义不变:`user_event` 的 `match` 之后没有任何
    /// 代码,「从分支返回」和「从函数返回」本来就等价。
    fn accept_connect_ok(
        &mut self,
        dial: crate::shell::dial_ledger::DialId,
        handle: Arc<SshConnection>,
        wants_sftp: bool,
        pty: Option<(SshSession, Receiver<Vec<u8>>)>,
    ) {
        // F205:先认票。认不到说明这张票已经被别的结局消费过(或标签早被关掉,
        // 任务被 abort 后事件迟到抵达)—— 宁可整条丢掉,也不能拿别人的身份
        // 往下走,那正是这个 bug 的样子。
        let Some(ticket) = self.dials.claim(dial) else {
            log::warn!(target: "mullion", "ConnectOk 认领不到拨号票 {dial:?},忽略");
            return;
        };
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
        // F205:身份与随行数据一律从**这张票**里取,不再读 `App` 上的单槽 ——
        // 单槽会被在途的第二条拨号整体盖掉(见 `shell::dial_ledger` 的文档)。
        let session_id = ticket.session_id;
        let cfg = Some(ticket.cfg);
        let mut automation = ticket.automation;
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
        // 没有 `session_id`(CLI 直连没有会话记录)或 store 里查不到(会话
        // 已被删)都落回全空默认——
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
                        // F187:本地收藏夹是全局的,来自 settings.toml ——
                        // 不再从会话记录里取(那份老数据启动时已并进来了)。
                        self.settings.local_bookmarks.clone(),
                        // F139:没有会话记录就没地方存书签,☆ 置灰。
                        session_id.is_some(),
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
            mark_ui_dirty!(self.ui_dirty);
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
        let emulator = new_pane_emulator(resolved_scrollback(self.store.as_ref(), session_id));
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
                history_reported: 0,
                host_pending: false,
                notice: None,
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
            // F128:重连要用它。跟下面 `last_cfg` 同一份,但那份是
            // **标签级**的(标题条读 user/host/port),换过节点之后
            // 就不再代表"每一条连接"——见 `HostConn::cfg` 的文档。
            cfg: cfg.clone(),
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
                // F141:下面 `take_pending` 成功时才填(那时才知道这次
                // 到底有没有 attach tmux)。
                tmux_attach: None,
                automation_status: None,
                // F50:每个标签自己的一份侧栏状态(D1:侧栏按会话记住)。
                files: crate::ui::files_panel::PanelFrame::new(
                    sftp_prefs.default_local.as_deref(),
                    sftp_prefs.bookmarks,
                    // F187:同上,全局收藏夹。
                    self.settings.local_bookmarks.clone(),
                    // F139:没有会话记录就没地方存书签,☆ 置灰。
                    session_id.is_some(),
                ),
                sftp: None,
                sftp_host_ix: None,
                sftp_tasks: Vec::new(),
                sftp_default_remote: sftp_prefs.default_remote,
                sftp_screenshot_dir: sftp_prefs.screenshot_dir,
                sftp_home: None,
                reconnect_tasks: Vec::new(),
                leaf_wanted: Vec::new(),
                leaf_detach: Vec::new(),
            })),
        );
        // F37/F160:是重连一个占位标签 → 把上次的分屏形状搭回来,给每个叶子
        // 按它**自己**的身份分派:同一条会话的在已有连接上开 channel(F35 那条路),
        // 别的会话排进串行拨号队列(F162),会话已被删的摆成占位(D3)。
        // 树坏了(`apply_saved_tree` 返回 `None`)就保持单屏,不阻断连接。
        if replaced {
            if let Some(p) = pending.as_ref() {
                let known: Vec<SessionId> = self
                    .store
                    .as_ref()
                    .map_or(Vec::new(), |s| s.list().iter().map(|r| r.id).collect());
                let plans = crate::shell::restore_plan::plan_leaves(
                    &p.identities,
                    // 主叶子那条会话就是这次拨通的这条。
                    p.identities[p.main_leaf]
                        .session_id
                        .expect("main_leaf 选出来的叶子必有 session_id"),
                    &|s| known.contains(&s),
                );
                let detach = crate::shell::restore_plan::detach_flags(&p.identities);
                let laid_out = self
                    .tabs
                    .by_generation_mut(generation)
                    .and_then(|tab| tab.content.as_terminal_mut())
                    .and_then(|t| {
                        let fresh = t.ws.apply_saved_tree(&p.tree, p.focus_leaf, p.main_leaf)?;
                        // 恢复出来的形状一般不对应任何预设按钮;单叶子
                        // 例外(它就是 Single)。
                        t.current_preset = (p.tree.len() == 1).then_some(Preset::Single);
                        // 叶子(前序)→ pane id。`leaves` 与 `to_entries` /
                        // `leaf_identities` 共用同一条前序约定,不许在这里
                        // 另写一遍遍历。
                        let leaves = mullion_core::layout::leaves(t.ws.tree());
                        // 5.2②:身份先由标签替它们保管,连上之后才切回实测。
                        t.leaf_wanted = leaves
                            .iter()
                            .zip(p.identities.iter())
                            .map(|(id, i)| (*id, i.clone()))
                            .collect();
                        t.leaf_detach = leaves
                            .iter()
                            .zip(detach.iter())
                            .map(|(id, d)| (*id, *d))
                            .collect();
                        Some((leaves, fresh))
                    });
                if let Some((leaves, fresh)) = laid_out {
                    self.dispatch_restored_leaves(generation, &leaves, &plans, p.main_leaf, fresh);
                }
            }
        }
        // 连上后关掉会话管理弹窗,别让它盖在新终端上方(复核 #4)。
        self.ui.close_session_manager();
        mark_ui_dirty!(self.ui_dirty);
        // 模板与计划**不再同进同退**(F161,见 `tab_keeps_template`)。
        // 「右键跳过一次」仍然不留模板 —— 用户明确说了这次不跑。
        let tpl = automation.template.take();
        let tmux_name = automation.session_name.take();
        // 跳过标志被 `take_pending` 消费掉,想知道「这次是不是被跳过的」
        // 只能在它之前读一次。
        let user_skipped = automation.skip;
        let plan = crate::automation::take_pending(&mut automation.plan, &mut automation.skip);
        if plan.is_some() {
            // S1:挂回**属主标签**(按世代号查),不用「活动标签」——
            // `open` 刚把新标签设为活动,今天两者等价,但那是巧合:
            // 哪天连接成功不再顺带切换焦点,这里就会把 handle 挂错标签。
            if let Some(t) = self
                .tabs
                .by_generation_mut(generation)
                .and_then(|tab| tab.content.as_terminal_mut())
            {
                // F141:这次全套跑里到底 attach 了哪个 tmux 会话 ——
                // 断线重连要照着它把用户接回去。
                t.tmux_attach = tmux_attach_for_connect(tpl.as_ref(), tmux_name.as_deref());
            }
        }
        if tab_keeps_template(plan.is_some(), user_skipped) {
            if let Some(t) = self
                .tabs
                .by_generation_mut(generation)
                .and_then(|tab| tab.content.as_terminal_mut())
            {
                t.automation_template = tpl;
            }
        }
        // 建标签的这个 pane 照配置**全套**跑,含 tmux
        // (`PaneId(1)` 见 `Workspace::new`)。F156-c:注入也在这里发,
        // 见 `on_pane_ready`。
        self.on_pane_ready(generation, PaneId(1), ssh, plan, true);
        self.request_ui_redraw();
    }

    /// F162:恢复出来的叶子各走各的路。
    ///
    /// - `SameHost` → 在这个标签已有的那条连接上开 channel(F35 那条路)。
    /// - `Dial(s)`  → 排进**串行**拨号队列(D10;并发会同时弹好几个密码框 /
    ///   主机指纹确认)。走「换节点」链路,不新写第二条拨号路径 ——
    ///   第二条一定会漏掉防连点那道闸。
    /// - `Orphan`   → 摆一块占位 pane,挂一句说明,不拨号(D3)。
    ///
    /// `main_leaf` 那个叶子已经是连上的那块 pane 了,跳过。
    fn dispatch_restored_leaves(
        &mut self,
        generation: u64,
        leaves: &[PaneId],
        plans: &[crate::shell::restore_plan::LeafPlan],
        main_leaf: usize,
        fresh: Vec<PaneId>,
    ) {
        use crate::shell::restore_plan::LeafPlan;
        let mut same_host = Vec::new();
        for (ix, (id, plan)) in leaves.iter().zip(plans.iter()).enumerate() {
            if ix == main_leaf {
                continue;
            }
            // `fresh` 是 `apply_saved_tree` 新分配的那些 —— 只有它们需要开
            // channel / 拨号。不在里面的是已经有 `PaneState` 的(理论上只有
            // 主叶子那块)。
            if !fresh.contains(id) {
                continue;
            }
            match plan {
                LeafPlan::SameHost => same_host.push(*id),
                LeafPlan::Dial(s) => self.restore_dial.push_back((generation, *id, *s)),
                LeafPlan::Orphan => self.place_orphan_pane(generation, *id),
            }
        }
        self.spawn_fresh_panes(same_host);
        self.drive_restore_dial();
    }

    /// D3:摆一块**拨不了号**的占位 pane(它那条会话已经被用户删了)。
    ///
    /// 承载机制沿用 F128 的 `Disconnected` pane(emulator + 一条死 channel),
    /// 不发明新的渲染路径 —— 树上有叶子而没有 `PaneState` 的话,那一格
    /// 什么都画不出来(F35 的「空窗期」约定只覆盖短暂空白)。
    fn place_orphan_pane(&mut self, generation: u64, pane: PaneId) {
        self.place_dead_pane(generation, pane, "会话已被删除,无法自动恢复");
    }

    /// D6:某台机器连不上时,**只有那块 pane** 降级成断开态,其余照常用。
    ///
    /// 为什么不是全或无:一台机器关机就让另外两台也连不成,不成比例。
    /// 为什么不接 F128 的指数退避自动重试:认证失败类错误会反复重试到退避
    /// 封顶,远端多出一串登录失败记录。用户点标题条上的「换节点」可以再试。
    fn degrade_restored_pane(&mut self, generation: u64, pane: PaneId, msg: &str) {
        self.place_dead_pane(generation, pane, msg);
    }

    /// D3/D6 共用的承载:一块有 `PaneState`、状态是 `Disconnected`、
    /// 标题条上挂着一句说明的 pane。真正的落地逻辑在 `place_dead_pane_of`
    /// (纯函数,可单测);这里只管现算 `scrollback`、借出 `&mut Workspace`、
    /// 按返回值决定打不打脏。
    fn place_dead_pane(&mut self, generation: u64, pane: PaneId, msg: &str) {
        let scrollback = resolved_scrollback(
            self.store.as_ref(),
            self.tabs
                .by_generation(generation)
                .and_then(|t| t.session_id),
        );
        let Some(ws) = self
            .tabs
            .by_generation_mut(generation)
            .and_then(|t| t.content.as_terminal_mut())
            .map(|t| &mut t.ws)
        else {
            return;
        };
        if place_dead_pane_of(ws, pane, generation, scrollback, msg) {
            mark_ui_dirty!(self.ui_dirty);
        }
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
    ///
    /// **世代号由调用方给**,不再自己从活动标签取:F162 的串行恢复队列拨号
    /// 时用户完全可能已经切到别的标签,拿活动标签会把结果挂错地方。
    ///
    /// 返回值:`true` = 异步拨号任务真的发出去了,`PaneRehosted`/`PaneRehostErr`
    /// 将来必有一个抵达;`false` = 走的是同步早退,那两个事件永远不会来。
    /// `drive_restore_dial` 靠这个判断串行闸该不该在**这里**就复位 ——
    /// 分不清这两种「失败」的话,同步早退命中一次,闸就永久停在 `true`,
    /// 队列里这条之后的叶子(可能跨多个标签)一个都不会再拨,且完全静默。
    ///
    /// `kind` 由调用方给,理由见 `RehostKind`:同一条链路的两个调用点语义相反,
    /// 而事件回来时已经分不出是谁发起的。
    #[must_use]
    fn spawn_rehost_on(
        &mut self,
        generation: u64,
        pane: PaneId,
        session: mullion_store::SessionId,
        kind: RehostKind,
    ) -> bool {
        let Some(store) = self.store.as_ref() else {
            self.ui.set_error("配置库不可用,无法换节点".to_string());
            return false;
        };
        let (cfg, wants_sftp) = match store.dial_plan_for(session) {
            Ok(v) => v,
            Err(e) => {
                self.ui.set_error(e.to_string());
                return false;
            }
        };
        if wants_sftp {
            // 弹窗已经把 SFTP 节点滤掉了(`ui::rehost::visible`),这里是第二道:
            // 真挂过去会得到一块永远不出字的黑屏,而用户只会觉得「换节点坏了」。
            self.ui
                .set_error("这是 SFTP 节点,没有终端,不能换到它上面".to_string());
            return false;
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
            cfg: cfg.clone(),
            kind,
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
        true
    }

    /// 换节点(或 F162 跨机器串行恢复)拨通了:把这块 pane 挂到新连接上。
    ///
    /// 从 `user_event` 的 `PaneRehosted` 分支搬出来(纯搬运,语义不变):
    /// 抽成具名方法是因为 F162 要在 `PaneRehostErr` 那条也复位串行拨号闸,
    /// 而 `arm_of` 的锚点只认单行模式,多行模式的分支体测不了。
    fn on_pane_rehosted(
        &mut self,
        generation: u64,
        pane: PaneId,
        handle: Arc<SshConnection>,
        ssh: SshSession,
        rx: Receiver<Vec<u8>>,
    ) {
        // F162:串行队列的闸。**两条路径都要复位**,漏一条队列就永久停在这里,
        // 后面的叶子一个都不会再拨。
        self.restore_dial_busy = false;
        // 元信息在用户选中那一帧就存下了(`PendingRehost`)。取不到 =
        // 这条事件没有对应的发起记录(理论上到不了),丢掉:硬编一个
        // 占位标题挂上去,标题条会写着一个假名字。
        let Some(ix) = self
            .pending_rehost
            .iter()
            .position(|p| p.generation == generation && p.pane == pane)
        else {
            log::warn!(target: "mullion", "换节点:pane {} 的在途记录已经不在了,丢弃", pane.0);
            self.drive_restore_dial();
            return;
        };
        let pending = self.pending_rehost.swap_remove(ix);
        // 包成 `Arc`:自动化 task 要跟 pane 共享同一条 channel 的写口
        // (同 `PaneOpened`,见 `PtyWriter for Arc<SshSession>`)。
        let ssh = Arc::new(ssh);
        let mut attached: Option<Arc<mullion_ssh::session::SshSession>> = None;
        // F17:换节点之后回溯行数按**新节点**的会话配置来(同上,
        // 得赶在借出 `ws` 之前算);`pending` 里必有 session_id。
        let scrollback = resolved_scrollback(self.store.as_ref(), Some(pending.session_id));
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
                // F128:拨这台新机器的参数跟着它自己走。落回
                // `last_cfg` 的话,这条连接断线时会拨回最初那台机器。
                cfg: Some(pending.cfg),
            });
            let host_ix = ws.hosts.len() - 1;
            if rehost_pane(
                ws,
                pane,
                generation,
                host_ix,
                Box::new(ssh.clone()),
                rx,
                scrollback,
                pending.kind,
            ) {
                attached = Some(ssh);
                // F162:这块 pane 从此有了自己的 `HostConn`,身份改由运行时实测。
                if let Some(p) = ws.pane_mut(pane) {
                    p.host_pending = false;
                    p.notice = None;
                }
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
            // F160/F161:换节点作废这块 pane 原来的 attach 意图 —— 必须在
            // `on_pane_ready` 之前调用,否则 `take_attach_intent` 会拿旧机器
            // 的会话名顶掉这里刚为新机器算好的 `pending.plan`(见
            // `clear_leaf_attach_intent` 文档)。
            if let Some(t) = self
                .tabs
                .by_generation_mut(generation)
                .and_then(|tab| tab.content.as_terminal_mut())
            {
                clear_leaf_attach_intent(t, pane);
            }
            // 用户拍板:换过节点的 pane 要跑**新节点**的登录后命令,
            // 规则同分屏新开的那些 —— 跳过 tmux,其余照跑。
            self.on_pane_ready(generation, pane, sink, pending.plan, true);
            self.ui.set_toast("已换节点");
            mark_ui_dirty!(self.ui_dirty);
            self.request_ui_redraw();
        }
        self.drive_restore_dial();
    }

    /// 换节点(或 F162 跨机器串行恢复)失败:pane 原样留在旧机器 / 摆成占位。
    ///
    /// 从 `user_event` 的 `PaneRehostErr` 分支搬出来,理由同 `on_pane_rehosted`。
    fn on_pane_rehost_err(&mut self, generation: u64, pane: PaneId, msg: String) {
        // F162:串行队列的闸。**两条路径都要复位**,漏一条队列就永久停在这里,
        // 后面的叶子一个都不会再拨。
        self.restore_dial_busy = false;
        log::warn!(target: "mullion", "pane {} 换节点失败: {msg}", pane.0);
        // 在途记录必须清掉,否则同一块 pane 再换一次时,
        // `PaneRehosted` 会取到上一次那条(标题条写着上一台机器的名字)。
        self.pending_rehost
            .retain(|p| !(p.generation == generation && p.pane == pane));
        // F160/F161:换节点失败同样让原 attach 意图作废 —— 这块 pane 接下来
        // 会被降级成占位态,留着旧记录就是给下一次换节点埋雷(见
        // `clear_leaf_attach_intent` 文档)。
        if let Some(t) = self
            .tabs
            .by_generation_mut(generation)
            .and_then(|tab| tab.content.as_terminal_mut())
        {
            clear_leaf_attach_intent(t, pane);
        }
        // F162:这块 pane 恢复途中拨的是**另一台**机器,连不上不该整个标签
        // 退回占位(D6)—— 只把它自己降级成断开态,其余 pane 照常用。
        self.degrade_restored_pane(generation, pane, "这台机器连不上,恢复失败");
        // 失败提示按世代过滤,理由同 `PaneOpenErr`。
        if self.tabs.by_generation(generation).is_some() {
            self.ui.set_error(msg);
            mark_ui_dirty!(self.ui_dirty);
            self.request_ui_redraw();
        }
        self.drive_restore_dial();
    }

    /// F128:一次断线重连拨通了 —— `UserEvent::PaneReconnected` 的处理体
    /// (纯搬运自原来的 match 分支,`user_event` 里改成一句委派)。
    fn on_pane_reconnected(
        &mut self,
        generation: u64,
        host_ix: usize,
        handle: Arc<SshConnection>,
        channels: Vec<(PaneId, SshSession, Receiver<Vec<u8>>)>,
    ) {
        diag::count_reconnect();
        self.reconnecting
            .retain(|(g, h, _)| !(*g == generation && *h == host_ix));
        let mut attached: Vec<(PaneId, Arc<mullion_ssh::session::SshSession>)> = Vec::new();
        let mut template = None;
        if let Some(t) = self
            .tabs
            .by_generation_mut(generation)
            .and_then(|t| t.content.as_terminal_mut())
        {
            // pane 先挂到新 channel 上。`host_ix` **不变** —— 重连换的
            // 是同一台机器的连接,不是换机器,那个下标本来就还是它。
            for (id, ssh, rx) in channels {
                let ssh = Arc::new(ssh);
                if reattach_pane(
                    &mut t.ws,
                    id,
                    generation,
                    host_ix,
                    Box::new(ssh.clone()),
                    rx,
                ) {
                    attached.push((id, ssh));
                }
            }
            if attached.is_empty() {
                // 拨号那几秒里这条连接上的 pane 全被用户关掉了 ——
                // 新连接没人要,**不替换**,`handle` 随这个分支结束而
                // Drop(Drop 即断连)。旧的死 `HostConn` 原样留着:
                // 它至少还带着 label/addr/cfg,下次真有 pane 要重连时
                // 还用得上。
                log::warn!(
                    target: "mullion",
                    "重连:host {host_ix}(世代 {generation})拨号途中所有 pane 都没了,丢弃新连接",
                );
            } else if let Some(h) = t.ws.hosts.get_mut(host_ix) {
                // **就地替换,绝不 push**:`hosts[ix]` 的语义是「第 ix
                // **台机器**的当前连接」,不是「第 ix 次建立的连接」。
                // push 一条新的会让 `hosts[0]` 不再是这个标签的主连接,
                // 而认着这个下标的地方一个都不会跟着走 ——
                // `TabContent::sftp_connection`(文件面板)、
                // `spawn_fresh_panes`(分屏开 channel)、`PaneOpened` 里
                // 硬编的 `host_ix: 0` 全都会指向刚断掉的那条死连接,
                // 症状是重连之后文件面板永久打不开、新开的分屏必然失败,
                // 而终端本身工作正常,用户完全看不出成因。
                //
                // 旧的那条 `HostConn` 在这次赋值里 Drop —— Drop 即断连,
                // 而它本来就已经死了(`rx_closed_action` 的重连判据就是
                // 「传输层没了」),不留孤儿。
                //
                // 同一台机器上那些用户敲过 `exit` 的 `Disconnected` pane
                // **不会被拖着换**:它们的 `host_ix` 仍指这里,但状态机
                // 走的是 `rx_closed_action(link_alive(..))` —— 连接活了
                // 之后返回的是 `UserExited`,状态原样是 `Disconnected`。
                // 它们的 `pty`/`rx` 还是旧的死 channel,写入静默失败,
                // 与替换前完全一致。
                //
                // `label`/`addr`/`session_id`/`cfg` 原样留着:同一台机器,
                // 这四样本来就该一样(旧实现也是从旧的那条复制过来的)。
                h.handle = handle;
                // F124:新连接 = 新的 tmux 服务器状态,自举重来一遍
                // (`tmux set -g` 幂等,重发无副作用)。
                h.tmux_bootstrap = Default::default();
                h.tmux_last_try = None;
            }
            // 这次拨号没赶上的 pane 补一次(判据见
            // `reconnect::strays_after_reconnect`):同一条连接上各 pane
            // 的 `rx` 不保证同一帧关闭,慢一步的那块不会出现在
            // `channels` 里,而它手里攥的是已经死掉的旧 channel ——
            // 不补的话它的输入静默丢失,标题条上却一切正常。
            // 置回 `Reconnecting`,下一帧 `drive_reconnects` 收走。
            if !attached.is_empty() {
                let snapshot: Vec<(PaneId, usize, crate::shell::workspace::PaneStatus)> =
                    t.ws.panes()
                        .iter()
                        .map(|p| (p.id, p.host_ix, p.status))
                        .collect();
                let done: Vec<PaneId> = attached.iter().map(|(id, _)| *id).collect();
                for id in crate::reconnect::strays_after_reconnect(&snapshot, host_ix, &done) {
                    if let Some(p) = t.ws.pane_mut(id) {
                        p.status = crate::shell::workspace::PaneStatus::Reconnecting;
                    }
                    log::info!(
                        target: "mullion",
                        "重连:pane {} 没赶上这次拨号,补一次重连",
                        id.0
                    );
                }
            }
            // 死掉的那条连接上开的 SFTP channel 一起完蛋 —— 留着的话
            // 侧栏每次操作静默失败,用户看到的是「文件面板卡住了」。
            //
            // 这里 abort 是对的(连接真没了,任务再跑也只是等超时),
            // 但光 abort 不够:被硬杀的 worker 发不出 `TransferDone`、
            // 被硬杀的列目录也翻不动 `PaneState::load`。两个收尾各自补
            // 在下面 —— 队列在借用外 `cancel_transfers_of`,面板这里
            // `invalidate`(否则 `load` 永远停在 `Loading`,之后每次
            // `trigger_sftp_open` 都撞 `already_loading` 早退,侧栏
            // **永久**打不开,重连成功了也救不回来)。
            for task in t.sftp_tasks.drain(..) {
                task.abort();
            }
            t.sftp = None;
            t.sftp_home = None;
            t.files.remote.invalidate();
            // F161/D1:重连时该接回哪个会话,**真值源是实测**。
            // `reattach_pane` 刻意保留了 `emulator` 连同它嗅出来的
            // `cwd`/`tmux`,所以此刻 `p.tmux` 还是断线前那个名字。
            //
            // 配置(`tmux_attach`)只在实测为空时回落 —— 用户的 tmux
            // 是在远端手敲 `tt web01` 进去的,配置里根本没有那个名字,
            // 只认配置的话断线之后那个会话永远接不回来。
            let measured: Vec<crate::shell::layout_snapshot::LeafIdentity> = attached
                .iter()
                .map(|(id, _)| crate::shell::layout_snapshot::LeafIdentity {
                    session_id: t.ws.hosts.get(host_ix).and_then(|h| h.session_id),
                    tmux: t.ws.pane(*id).and_then(|p| p.tmux.clone()).or_else(|| {
                        t.tmux_attach
                            .as_ref()
                            .filter(|x| x.matches(*id, host_ix))
                            .map(|x| x.session_name.clone())
                    }),
                })
                .collect();
            // D5:同一台机器上的同一个会话名,只有第一块带 `-d`。
            // 重连场景里「其他 client」几乎必然是我们自己的残骸
            // (SSH 断了但远端 tmux 要等 TCP 超时才知道),第一块必须踢;
            // 第二块再踢就会把第一块踢成 detached(F141 的原始理由)。
            //
            // **已知时序假设,本切片不处理**:`detach_flags` 只按这里的遍历
            // 顺序给「批内第一块」打 `-d`,但真正的 attach 字节是各自
            // `start_automation` spawn 出去、各等各自 ready 才发的 ——
            // 落地顺序不由这个遍历顺序决定。如果用户故意开了两块 pane 实测到
            // **同一个** tmux 名(真实场景,不是构造的),又恰好不带 `-d` 的
            // 那块先 attach 上、带 `-d` 的那块后到,`-d` 会把**我们自己刚接上
            // 的那块**也踢下线(`attach -d` 踢的是所有其他 client,不区分是不是
            // 我们自己的)。症状是两块 pane 里有一块莫名其妙又断开一次,
            // 且只在这种「双 pane 镜像同一会话 + 断线重连」的窄场景下出现。
            let flags = crate::shell::restore_plan::detach_flags(&measured);
            for ((id, _), (want, d)) in attached.iter().zip(measured.into_iter().zip(flags)) {
                t.leaf_wanted.retain(|(x, _)| x != id);
                t.leaf_detach.retain(|(x, _)| x != id);
                t.leaf_wanted.push((*id, want));
                t.leaf_detach.push((*id, d));
                // F163:这块 pane 重连成功了 —— 上一轮挂的「连不上 / 会话已不
                // 存在」的提示已经过期,摘掉,否则会永久误导用户。
                if let Some(p) = t.ws.pane_mut(*id) {
                    p.notice = None;
                }
            }
            template = t.automation_template.clone();
        }
        // 借用外:上面 abort 掉的传输 worker 不会再发 `TransferDone`,
        // 队列里那几条会永久停在 `Running`,而 `take_runnable` 按
        // `Running` 数占并发名额(默认 4)—— 断几次线就把全局传输堵死。
        // 放在 abort **之后**:先杀任务再收口,中间不会有 worker 抢着
        // 把状态从「已取消」改写成「失败」。
        self.cancel_transfers_of(generation);
        // 用户拍板:重连之后重跑登录后命令 —— 否则 tmux 不 attach,
        // 断线前那个 Claude Code 会话回不来(这正是 F128 的初衷)。
        let reconnected = !attached.is_empty();
        // F161:计划由 `on_pane_ready` 按上面写进 `leaf_wanted` 的实测名
        // 决定(D7:有名字就只发 attach)。这里只给「没有任何 tmux 名」
        // 那些 pane 备一份配置计划 —— 分屏出来的、换过节点的照旧
        // 跳过 tmux、只重跑 cd/export/命令。
        if let Some(tpl) = template {
            for (id, sink) in attached {
                let plan = crate::automation::pending_for_extra_pane(&tpl);
                // F156-c:重连出来的 channel 也是一条刚起的干净 shell,
                // 同样要经 `on_pane_ready` 注入 OSC 7;`false` = 不清屏
                // (断线前那一屏是用户想看的东西)。
                self.on_pane_ready(generation, id, sink, plan, false);
            }
        }
        // 零个 pane 真接上却弹「已重新连接」是名不副实——那种情况下
        // 上面已经把刚 push 的 `HostConn` 撤掉了,用户什么都没得到。
        if reconnected {
            self.ui.set_toast("已重新连接");
        }
        mark_ui_dirty!(self.ui_dirty);
        self.request_ui_redraw();
    }

    /// F162:推进跨机器恢复的串行拨号。每帧调(挂在 `drive_*` 那一组里)。
    ///
    /// **遍历的是队列不是活动标签**:队列里的三元组自带世代号,拨号途中用户
    /// 完全可能切到别的标签去(记忆里那条「`drive_*` 每帧驱动函数必须遍历
    /// 全部标签」的同源教训)。
    ///
    /// `while` 而不是尾递归:debug 构建下尾递归不保证 TCO,长队列(标签已关掉
    /// 的那些叶子会被跳过、连续跳好几条)会爆栈。
    fn drive_restore_dial(&mut self) {
        while let Some((generation, pane, session)) =
            take_next_restore_dial(&mut self.restore_dial, self.restore_dial_busy)
        {
            // 世代号即路由键:标签已经被关掉了就跳过这一条,接着试下一条。
            if self.tabs.by_generation(generation).is_none() {
                continue;
            }
            self.restore_dial_busy = true;
            // 复用「换节点」那条链路(D10)。不新写第二条拨号路径 —— 第二条
            // 一定会漏掉 `pending_rehost` 那道防连点的闸。
            // F188:恢复队列拨的这块叶子还**没有** `PaneState`(`apply_saved_tree`
            // 只分配了 id),而且不能抢焦点 —— 详见 `RehostKind`。
            if self.spawn_rehost_on(generation, pane, session, RehostKind::RestoreFirstMount) {
                return;
            }
            // 同步早退(配置库不可用 / dial_plan_for 失败 / SFTP 节点):
            // `PaneRehosted`/`PaneRehostErr` 永远不会抵达,闸必须在**这里**
            // 复位,否则队列里这条之后的叶子一个都不会再拨,而且完全静默。
            self.restore_dial_busy = false;
        }
    }

    /// F128:这一帧该发起哪些重拨。挂在帧循环上而不是在 `pump` 里直接拨:
    /// `Workspace` 不认识 tokio、也不认识 store(架构不变量),拨号是 app 的事。
    ///
    /// **遍历所有标签,不只是活动标签**——理由同 `drive_automation`:用户
    /// 完全可能开着标签 A 连了台机器就切去标签 B,只驱动活动标签的话标签 A
    /// 断线要等用户切回去才会开始重拨,「尽快恢复」这条诉求就落空了。
    fn drive_reconnects(&mut self) {
        let generations: Vec<u64> = self
            .tabs
            .iter()
            .filter_map(|tab| tab.content.as_terminal())
            .map(|t| t.ws.generation())
            .collect();
        let mut plans: Vec<(u64, usize)> = Vec::new();
        for generation in generations {
            let Some(t) = self
                .tabs
                .by_generation(generation)
                .and_then(|t| t.content.as_terminal())
            else {
                continue;
            };
            // 稳态早退(T3):这个标签没有 pane 在 `Reconnecting` 就不用往下
            // collect 两个 `Vec`——多标签下这个函数每帧都对每个标签跑一遍,
            // 早退把稳态成本压回一次 bool 扫描。
            if !t
                .ws
                .panes()
                .iter()
                .any(|p| p.status == crate::shell::workspace::PaneStatus::Reconnecting)
            {
                continue;
            }
            let in_flight: Vec<usize> = self
                .reconnecting
                .iter()
                .filter(|(g, _, _)| *g == generation)
                .map(|(_, h, _)| *h)
                .collect();
            // `Workspace::panes()` 返回 `&[PaneState]`(既有签名),所以走 `.iter()`。
            let panes: Vec<(PaneId, usize, crate::shell::workspace::PaneStatus)> =
                t.ws.panes()
                    .iter()
                    .map(|p| (p.id, p.host_ix, p.status))
                    .collect();
            for host_ix in crate::reconnect::hosts_to_redial(&panes, &in_flight) {
                plans.push((generation, host_ix));
            }
        }
        for (generation, host_ix) in plans {
            // `attempt = 0` 是「首次重拨」:`delay_for(0)` 是 `None`(退避表
            // 1-indexed,`backoff_delay(0)` 视 0 为非法输入),`unwrap_or_default()`
            // 把它变成零延迟——首次立刻拨,失败后才进 1s/2s/4s… 的退避。
            // **别把 0 改成 1**,那会让首次重拨白等 1 秒。
            self.spawn_reconnect(generation, host_ix, 0);
        }
    }

    /// F128:为一条已经死掉的连接发起第 `attempt` 次重拨。
    ///
    /// 凭据取 **`ws.hosts[host_ix].cfg`**(建这条连接那一刻定死的),两条纪律:
    ///
    /// - **绝不回头查库** —— 用户在断线期间完全可能改了会话甚至删了它,那时候
    ///   拨出去的目标就跟他当初点「连接」时看到的不是一回事(理由同 `PendingRehost`)。
    /// - **绝不取标签级的 `last_cfg`** —— 那只记得**最初**连的那台机器。用户
    ///   用「换节点」把 pane 挪到第二台服务器之后,第二台断线时拿 `last_cfg`
    ///   重拨就是静默连到另一台机器上(见 `HostConn::cfg` 的文档)。
    fn spawn_reconnect(&mut self, generation: u64, host_ix: usize, attempt: u32) {
        let Some(t) = self
            .tabs
            .by_generation(generation)
            .and_then(|t| t.content.as_terminal())
        else {
            return;
        };
        let Some(cfg) = t.ws.hosts.get(host_ix).and_then(|h| h.cfg.clone()) else {
            log::warn!(
                target: "mullion",
                "重连:世代 {generation} host {host_ix} 没有拨号参数,放弃"
            );
            return;
        };
        // 这条连接上挂着哪些 pane —— 每块都要一条新 channel(adr-009)。
        let panes: Vec<PaneId> =
            t.ws.panes()
                .iter()
                .filter(|p| {
                    p.host_ix == host_ix
                        && p.status == crate::shell::workspace::PaneStatus::Reconnecting
                })
                .map(|p| p.id)
                .collect();
        if panes.is_empty() {
            return;
        }
        self.reconnecting.push((generation, host_ix, attempt));
        let delay = crate::reconnect::delay_for(attempt).unwrap_or_default();
        // 屏内提示(§7.3):喂进 emulator 当普通输出,不做倒计时。
        let notice = crate::reconnect::notice_bytes(attempt + 1, delay);
        if let Some(ws) = self
            .tabs
            .by_generation_mut(generation)
            .and_then(|t| t.content.as_terminal_mut())
            .map(|t| &mut t.ws)
        {
            for id in &panes {
                if let Some(p) = ws.pane_mut(*id) {
                    p.emulator.feed(&notice);
                }
            }
        }
        let proxy = self.proxy.clone();
        let wake_proxy = self.proxy.clone();
        // 主机密钥照旧走后台策略:指纹变了就当场停(不是「重连时放松校验」——
        // 断线正是中间人最好下手的时机)。
        let policy: Arc<dyn HostKeyPolicy> = Arc::new(crate::host_key::PromptingPolicy::new(
            self.known_hosts.clone(),
            self.proxy.clone(),
            true,
        ));
        let task = self._runtime.spawn(async move {
            tokio::time::sleep(delay).await;
            let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                let _ = wake_proxy.send_event(UserEvent::Wake);
            });
            let handle = match mullion_ssh::session::establish(&cfg, policy).await {
                Ok(h) => Arc::new(h),
                Err(e) => {
                    let _ = proxy.send_event(UserEvent::PaneReconnectErr {
                        generation,
                        host_ix,
                        attempt,
                        msg: e.to_string(),
                    });
                    return;
                }
            };
            let mut channels = Vec::new();
            for id in panes {
                match mullion_ssh::session::open_pty(handle.clone(), &cfg, wake.clone()).await {
                    Ok((ssh, rx)) => channels.push((id, ssh, rx)),
                    Err(e) => {
                        let _ = proxy.send_event(UserEvent::PaneReconnectErr {
                            generation,
                            host_ix,
                            attempt,
                            msg: e.to_string(),
                        });
                        return;
                    }
                }
            }
            let _ = proxy.send_event(UserEvent::PaneReconnected {
                generation,
                host_ix,
                handle,
                channels,
            });
        });
        // 句柄存回属主标签,`wind_down` 才收得了口(见 `reconnect_tasks` 的
        // 文档)。取不到属主标签就当场 abort —— 与 `track_sftp_task` 同一套
        // 「无人收留就别让它跑」的纪律。
        if let Some(t) = self
            .tabs
            .by_generation_mut(generation)
            .and_then(|t| t.content.as_terminal_mut())
        {
            t.reconnect_tasks.retain(|h| !h.is_finished());
            t.reconnect_tasks.push(task);
        } else {
            task.abort();
        }
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
        mark_ui_dirty!(self.ui_dirty);
        self.request_ui_redraw();
        // F163:attach 的字节到这一刻才真的发完 —— 校验的宽限期从这里起算,
        // 不是从 `take_attach_intent` 打算发的那一刻(那时 `automation::run`
        // 还在等首字节,最长能等 `ready_timeout_ms` = 默认 15 秒)。
        // 非 `Completed` 的结局(等首字节超时 / 用户接管 / 断线)意味着 attach
        // 压根没发出去或没发完,这时下「没接上」的结论纯属误报 —— 直接撤掉这条
        // 校验,宁可不报也不错报。
        self.arm_or_drop_attach_check(
            generation,
            pane,
            matches!(outcome, crate::automation::Outcome::Completed(_)),
        );
    }

    /// F163:automation 结束了,把这块 pane 的校验**上膛**或撤掉。
    ///
    /// 拆成具名方法是为了让「非 Completed 就撤掉」这条判据可以被源码切片守护
    /// 扎住 —— 它是这条功能不误报的唯一依据。
    fn arm_or_drop_attach_check(&mut self, generation: u64, pane: PaneId, completed: bool) {
        if !completed {
            self.attach_checks
                .retain(|c| !(c.generation == generation && c.pane == pane));
            return;
        }
        let deadline = Instant::now() + ATTACH_CHECK_GRACE;
        for c in &mut self.attach_checks {
            if c.generation == generation && c.pane == pane && c.deadline.is_none() {
                c.deadline = Some(deadline);
            }
        }
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
        mark_ui_dirty!(self.ui_dirty);
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
        // F148:先把 v1 的 `layout.toml` 迁成一条记录并删掉它(D14),再裁剪
        // 到 10 条(D5/X6:**只在启动时裁一次**,关窗口时也裁的话「读整个
        // 目录」就从一次性动作变成了每个实例退出都要做的共享操作)。
        //
        // 两件事都在建窗口之前做:下面要拿最新一条的几何去填
        // `WindowAttributes`,建完再 `set_outer_position` 会让用户看见窗口
        // 先在默认位置闪一下再跳过去。
        let history = crate::shell::store::config_dir()
            .map(|d| {
                let now = mullion_store::now_secs();
                if let Some(id) =
                    mullion_store::migrate_legacy(&d, &self.instance_id_for_legacy(), now)
                {
                    crate::logx::line(&format!("F148:老的 layout.toml 已迁成记录 {id}"));
                }
                let dropped = mullion_store::prune(&d, now);
                if dropped > 0 {
                    crate::logx::line(&format!("F148:裁掉了 {dropped} 条旧记录"));
                }
                mullion_store::list_records(&d, now)
            })
            .unwrap_or_default();
        // X8:启动**不摆标签**(D1),但窗口总得有个大小和位置 —— 取最新
        // 一条记录的几何(死活不论)。恢复某条记录时**不再改窗口几何**:
        // 窗口已经建好了,再跳一次位置只会让人眼花(D13)。
        let saved_window = history.first().and_then(|e| e.layout.window);
        let mut attrs = Window::default_attributes().with_title("mullion");
        if let Some(w) = saved_window {
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
        // F152:标题栏/Alt-Tab/任务栏的图标。**必须显式挂** —— winit 0.30.13
        // 注册窗口类时写死 `hIcon: 0`(`platform_impl/windows/window.rs:1417`),
        // 不会去读 exe 的资源图标,只嵌资源的话这三处永远是那张空白默认图。
        //
        // 从资源段按序号取,而不是 `include_bytes!` 一份再解码:同一张 ico
        // 就只有一份(370 KB,N6 体积),而且**尺寸由 Windows 自己从 ico 里挑**,
        // 比我们在 CPU 上重采样准 —— 高 DPI 下它会去拿 48/64 那几帧。
        //
        // 取不到就不设(保持默认图标),不 panic:图标缺失不该拦住一个 SSH 客户端启动。
        #[cfg(target_os = "windows")]
        {
            use winit::platform::windows::{IconExtWindows as _, WindowAttributesExtWindows as _};
            let px = |n: u32| Some(winit::dpi::PhysicalSize::new(n, n));
            // 标题栏那张按 16 要(ICON_SMALL);任务栏那张不给尺寸,让
            // `LR_DEFAULTSIZE` 跟着系统 DPI 走(ICON_BIG)。
            let small = winit::window::Icon::from_resource(
                crate::icon_res::RESOURCE_ID,
                px(crate::icon_res::SMALL_PX),
            );
            let big = winit::window::Icon::from_resource(crate::icon_res::RESOURCE_ID, None);
            if let Err(e) = &small {
                crate::logx::line(&format!("F152:窗口图标取不到({e:?}),用系统默认"));
            }
            attrs = attrs
                .with_window_icon(small.ok())
                .with_taskbar_icon(big.ok());
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
            last_frame_fp: None,
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
                    self.pending_history = Some(history);
                    self.ui.unlock = Some(crate::ui::unlock::UnlockDraft::default());
                }
                Ok(false) => {
                    self.open_store_with(
                        crate::shell::store::SessionStore::open(
                            d,
                            &mullion_store::KeyringSource::new(),
                        ),
                        history,
                    );
                }
                Err(e) => {
                    // 探测本身失败(文件头读不懂 / 读不出来)。这里**不能**当成
                    // 「不需要密码」往下走:那会拿钥匙串密钥去解一个主密码文件,
                    // 报出来的是「密文损坏」,把真正的原因盖掉。
                    crate::logx::line(&format!("secrets.enc 探测失败: {e}"));
                    self.ui.set_error(format!("会话库打开失败:{e}"));
                    self.finish_store_open(history);
                }
            },
            None => {
                crate::logx::line("无法定位配置目录,会话功能禁用");
                self.ui.set_error("无法定位配置目录".into());
                self.finish_store_open(history);
            }
        }
        diag::mark(diag::Stage::Idle);
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        diag::mark(diag::Stage::UserEvent);
        // F158:后台事件默认标脏。判据与豁免名单见 `user_event_marks_dirty`。
        if user_event_marks_dirty(&event) {
            mark_ui_dirty!(self.ui_dirty);
        }
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
                    diag::count_request_redraw(diag::RedrawSource::Event);
                    a.window.request_redraw();
                }
            }
            UserEvent::ConnectOk {
                dial,
                handle,
                wants_sftp,
                pty,
            } => {
                diag::count_connect(true);
                self.accept_connect_ok(dial, handle, wants_sftp, pty);
                // F153:**在分派点推进而不是在 `accept_connect_ok` 里面** ——
                // 那个函数有多条早退 return(SFTP 标签、缺 pty 的异常路径),
                // 写在里面会漏掉其中一条,症状是自动拨号连到某个 SFTP 标签
                // 就停住。
                self.advance_auto_dial(Some(true));
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
                // F17:回溯行数取**这个标签所属会话**的配置(分屏出来的 pane
                // 与主 pane 同源)。必须赶在下面借出 `ws` 之前算完 —— 它要读
                // `self.store`,而 `ws` 是从 `self.tabs` 借出来的。
                let scrollback = resolved_scrollback(
                    self.store.as_ref(),
                    self.tabs
                        .by_generation(generation)
                        .and_then(|t| t.session_id),
                );
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
                        let emulator = new_pane_emulator(scrollback);
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
                            history_reported: 0,
                            host_pending: false,
                            notice: None,
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
                    self.on_pane_ready(generation, id, sink, plan, true);
                }
                mark_ui_dirty!(self.ui_dirty);
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
                    mark_ui_dirty!(self.ui_dirty);
                    self.request_ui_redraw();
                }
            }
            UserEvent::PaneRehosted {
                generation,
                pane,
                handle,
                ssh,
                rx,
            } => self.on_pane_rehosted(generation, pane, handle, ssh, rx),
            UserEvent::PaneRehostErr {
                generation,
                pane,
                msg,
            } => self.on_pane_rehost_err(generation, pane, msg),
            UserEvent::PaneReconnected {
                generation,
                host_ix,
                handle,
                channels,
            } => self.on_pane_reconnected(generation, host_ix, handle, channels),
            UserEvent::PaneReconnectErr {
                generation,
                host_ix,
                attempt,
                msg,
            } => {
                self.reconnecting
                    .retain(|(g, h, _)| !(*g == generation && *h == host_ix));
                log::warn!(
                    target: "mullion",
                    "世代 {generation} host {host_ix} 第 {attempt} 次重连失败: {msg}"
                );
                let next = attempt + 1;
                if crate::reconnect::delay_for(next).is_some() {
                    self.spawn_reconnect(generation, host_ix, next);
                } else {
                    // 退避到顶:落回 `Disconnected`,交给用户决定重连还是关掉。
                    if let Some(ws) = self
                        .tabs
                        .by_generation_mut(generation)
                        .and_then(|t| t.content.as_terminal_mut())
                        .map(|t| &mut t.ws)
                    {
                        let ids: Vec<PaneId> = ws
                            .panes()
                            .iter()
                            .filter(|p| p.host_ix == host_ix)
                            .map(|p| p.id)
                            .collect();
                        for id in ids {
                            if let Some(p) = ws.pane_mut(id) {
                                p.status = crate::reconnect::status_after_failure(next);
                                p.emulator.feed(&crate::reconnect::give_up_notice_bytes());
                            }
                        }
                    }
                    self.ui.set_error(format!("重连失败: {msg}"));
                }
                mark_ui_dirty!(self.ui_dirty);
                self.request_ui_redraw();
            }
            UserEvent::KeyPathPicked(picked) => {
                self.picker_busy.key = false;
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
                self.picker_busy.key = false;
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
                self.picker_busy.import = false;
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
                self.picker_busy.icon = false;
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
            UserEvent::ConnectErr(dial, msg) => {
                diag::count_connect(false);
                // F205:失败也要认票 —— 不认的话票留在台账上,里面装着
                // `SshConfig`(含主机/端口/认证方式)一直不释放。
                self.dials.claim(dial);
                // 待定 F:CLI 直连从未成功连过时,保留可脚本化的 exit(1) 语义;
                // launcher 态(或已连过又断开)只记错误,交 UI 展示(ui.last_error)。
                crate::logx::line(&format!("连接失败: {msg}"));
                if self.cli_direct && self.active_ws().is_none() {
                    std::process::exit(1);
                }
                // F37:这次失败的如果是某个占位标签的重连,**必须在这里收口**。
                // 不收的话 `reconnect_tab` 开头那道 `pending_restore` 闸永久
                // 关闭 —— 这个进程里所有占位标签的「重连」从此静默无反应,
                // 而按钮还停在禁用的「连接中…」。没有自愈路径,只能重启 exe。
                if let Some(p) = self.pending_restore.take() {
                    if let Some(TabContent::Restored(r)) = self
                        .tabs
                        .iter_mut()
                        .find(|t| t.id == p.tab_id)
                        .map(|t| &mut t.content)
                    {
                        r.dialing = false;
                    }
                }
                // F153:这一条完了,接着拨下一条。失败不中断队列 —— 一条会话
                // 的凭据不对,不该把其余的一起吊死。
                self.advance_auto_dial(Some(false));
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
            UserEvent::SftpOpened {
                generation,
                host_ix,
                result,
            } => {
                self.accept_sftp_opened(generation, host_ix, result);
            }
            UserEvent::SftpListed {
                generation,
                seq,
                result,
            } => {
                self.accept_sftp_listed(generation, seq, result);
            }
            UserEvent::OwnerNames {
                generation,
                query,
                stdout,
            } => {
                self.accept_owner_names(generation, query, stdout);
            }
            UserEvent::SftpOpDone { generation, result } => {
                diag::count_sftp_op();
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
            UserEvent::ShotUploaded {
                generation,
                pane,
                result,
            } => {
                self.accept_shot_uploaded(generation, pane, result);
            }
            UserEvent::TransferPlanned { generation, result } => {
                match result {
                    Ok(jobs) => {
                        for p in jobs {
                            let id = self.transfer.queue.push(crate::files::queue::NewJob {
                                dir: p.dir,
                                generation,
                                label: p.label,
                                total: p.total,
                            });
                            self.transfer.specs.insert(
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
                self.transfer.queue.progress(job, done);
            }
            UserEvent::TransferDone { job, result } => {
                // F155:一次传输完成 = 一次 SFTP 操作。计在 Done 而不是
                // Progress —— 后者一个大文件几千条(见同 arm 的 T3 守护)。
                diag::count_sftp_op();
                self.transfer.cancels.remove(&job);
                self.transfer.queue.finish(job, result);
                // 传完刷新**目标那一栏** —— 不刷的话新文件不出现,用户以为没成。
                if let Some(spec) = self.transfer.specs.get(&job) {
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
                    .transfer
                    .queue
                    .get(job)
                    .is_none_or(|j| j.state.is_finished())
                {
                    self.transfer.specs.remove(&job);
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
                // T7 变种:`RedrawRequested` 在这里被门控挡掉,**绝不能**喂给
                // egui —— egui-winit 对它恒返回 `repaint: true`,而下面那三句
                // 里的 `request_redraw()` 立刻又生成一个 `RedrawRequested`,
                // 闭环自激。判据与理由见 `egui_should_see_window_event`。
                if shell::input_route::egui_should_see_window_event(&event) {
                    // F175:只包住这一次调用。窗口事件在 v0.1.75 的实机剖面里是
                    // `937x/p95=1.0ms`,但那段含路由判定与标脏,拆不开就没法判断
                    // 该去掉帧还是该去掉这一趟。埋点,不改任何门控。
                    let resp = diag::timed_egui_window_event(|| {
                        active.egui_state.on_window_event(&active.window, &event)
                    });
                    if resp.repaint {
                        // F171:归因埋在 `resp.repaint` **之内** —— 这张表回答的是
                        // 「凭什么出帧」,不是「收到了什么」。挪到 if 外面就把
                        // egui 明确说了「不用重绘」的那几类也算了进来,而
                        // `wev=` 与 `dirty=` 的可比性正来自两者判据相同。
                        diag::note_window_event(crate::wev::kind_of(&event));
                        if let WindowEvent::CursorMoved { position, .. } = &event {
                            diag::note_cursor_pos(position.x, position.y);
                        }
                        // 标脏与请求重绘必须成对:只请求不标脏,那帧会被 frame_is_dirty
                        // 判 Idle 丢掉(终端态尤其明显:远端一安静菜单就点不开)。
                        mark_ui_dirty!(self.ui_dirty);
                        diag::count_request_redraw(diag::RedrawSource::Event);
                        active.window.request_redraw();
                    }
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
                        // `dispatch_panel_action`/`move_panel_selection`)只把
                        // `self.ui_dirty` 标脏,从不请求重绘。事件循环整个跑在
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
                if self.edit.sessions.blocks_exit() && !self.ui.exit_pending {
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
                crate::edit::tempdir::purge(&self.edit.root);
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
                            diag::count_scroll();
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
                    self.focus_pane_under_cursor();
                    self.selection_press();
                }
                (MouseButton::Left, ElementState::Released) => self.selection_release(),
                // 右键直接贴,不弹菜单(Windows 终端习惯,F18 交互口径)。
                (MouseButton::Right, ElementState::Pressed) => {
                    // F185:切焦点必须在 `request_paste` **之前** —— 粘贴的落点是
                    // `effective_focus()`,顺序反了就会把内容贴进上一块 pane,
                    // 而用户看到的是自己刚点过的这一块。分屏下这是静默错投。
                    self.focus_pane_under_cursor();
                    self.request_paste();
                }
                _ => {}
            },
            WindowEvent::Resized(size) => {
                diag::mark(diag::Stage::Resize);
                log::debug!(target: "mullion", "Resized({}x{})", size.width, size.height);
                self.apply_resize(size.width, size.height);
            }
            // 输入法(F21):中文/日文的字是从这条路进来的,不是 `KeyboardInput`。
            // `set_ime_allowed(true)` 在 `resumed` 里开,egui 想关掉它的那次调用
            // 被 F149 的账本按压拦住了(见 `input::ime_ledger_clamp`)。
            WindowEvent::Ime(ime) => {
                // F149:这条事件此前**没有**过输入分流 —— 上面的 `is_kbd` 只匹配
                // `KeyboardInput`,于是 Ime 一路喂给了 egui、又一路写进焦点 pane
                // 的 PTY:在会话名 / 标签改名 / 路径条里打的中文会同时上屏和发到
                // 远端 shell(混在命令行里不显眼,所以一直没人报)。
                //
                // 已知缺口(复核挖出,故意不修):归属是按**每个 Ime 子事件**现算的,
                // 不是按「一次组字」锁定的。用户在 egui 文本框里敲拼音、组字还没
                // 确认就点回终端,随后那条 `Ime::Commit` 会跟着新焦点算成
                // `to_terminal == true`,本该进表单的中文被写进远端 shell;反过来
                // (终端组字中途点进 egui)不泄漏,但会被下面的 `on_disabled()` 清掉
                // 打了一半的拼音,观感是「字凭空消失」。两个方向各错一半——锁定归属
                // 会让「终端组字中途点进 egui 再提交」把字打进终端,同样是错的,而
                // 且现状已经严格优于修复前(修复前是任何时候都双写)。彻底解法要能
                // 在焦点切换时打断 OS 的组字,超出本次范围。
                let to_terminal = ime_goes_to_terminal_of(
                    self.effective_focus(),
                    self.modal_open(),
                    self.active
                        .as_ref()
                        .is_some_and(|a| a.egui_ctx.wants_keyboard_input()),
                );
                if to_terminal {
                    match &ime {
                        winit::event::Ime::Preedit(text, _) => {
                            // F210:锚点只在组字**开始**那一次被记下,之后整段
                            // 组字都钉住(见 `ImeState::anchored_cursor`)。
                            let at = self.focused_cursor_cell();
                            self.ime.on_preedit(text, at);
                            // F125:拼音现在内联上屏,组字中敲字母也算「输入」——
                            // 不重置的话光标可能闪到暗周期,用户敲拼音时观感是丢帧。
                            self.last_input_at = Instant::now();
                        }
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
                    // F126:preedit 串变了,候选框该跟去拼音串末尾——不补这一句,
                    // 候选框位置要等下一次别的事件才更新,组字时肉眼可见地滞后一拍。
                    self.apply_ime_cursor_area();
                } else {
                    // 这串拼音是打给 egui 的。终端侧的组字状态**必须清掉**:
                    // 留着的话 F126 会把它内联画在终端光标处(用户在会话名框里
                    // 打字,终端上跟着显示拼音),而且 `swallows_key()` 恒 true
                    // 会让终端永久吞键 —— 与 `ImeState` 少认一条结束边同一类故障。
                    self.ime.on_disabled();
                    // 候选框位置的记账作废:egui 自己会调 `set_ime_cursor_area`
                    // 把框摆到它的文本框那儿(egui-winit `lib.rs:863`),而我们的
                    // `ime_cursor_area` 没跟着变。不作废的话,回到终端组字时若算出
                    // 的 area 与记账值相同,`apply_ime_cursor_area` 会在第一行早退,
                    // 候选框一直停在那个文本框原来的位置。
                    self.ime_cursor_area = None;
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
                        // F209:裸 `Ctrl+V`。**两样都读**再交给
                        // `shot::clip_paste` 判 —— 把「有文本时就别看图」写成
                        // 提前 return 的话,那个函数的「两样都有」分支在产品
                        // 代码里永远走不到,守住它的测试就成了摆设。
                        // `clipboard_dib` 自己先问 `IsClipboardFormatAvailable`,
                        // 没有图时连剪贴板都不开。
                        if mods.ctrl
                            && !mods.shift
                            && !mods.alt
                            && matches!(key, Key::Char(c) if c.eq_ignore_ascii_case(&'v'))
                        {
                            let text = self.clipboard.get();
                            let dib = crate::shot::clipboard_dib();
                            match crate::shot::clip_paste(text.is_some(), dib.is_some()) {
                                crate::shot::ClipPaste::Text => {
                                    // 走 F18 原路(多行仍然弹确认),它自己会
                                    // 再读一次剪贴板。
                                    self.request_paste();
                                    self.request_ui_redraw();
                                    return;
                                }
                                crate::shot::ClipPaste::Image => {
                                    self.paste_screenshot(&dib.unwrap_or_default());
                                    self.request_ui_redraw();
                                    return;
                                }
                                // 剪贴板里两样都没有:**不吞这一下**,照旧编码
                                // 成 `^V` 发下去 —— readline 的 quoted-insert
                                // 靠它输控制字符。
                                crate::shot::ClipPaste::Neither => {}
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
                                diag::count_scroll();
                            }
                            self.request_ui_redraw();
                            return;
                        }
                        // F129:断开的 pane 上 Ctrl+D 改成「关掉这块分屏」。
                        // 必须在 `encode_key` 之前 —— 它会把 Ctrl+D 编成 0x04,
                        // 漏下去就是往一条死 channel 上写字节(静默失败)。
                        // 修饰键判据走纯函数,不在这儿内联 —— 它要排掉 AltGr
                        // (Windows 合成成 Ctrl+Alt),那条只有单测守得住。
                        if crate::shell::input_route::is_bare_ctrl_d(key, mods) {
                            let st = self
                                .active_ws()
                                .and_then(Workspace::focused)
                                .map(|p| p.status);
                            let is_last = self
                                .active_ws()
                                .map(|ws| ws.pane_count() <= 1)
                                .unwrap_or(true);
                            if let Some(st) = st {
                                match crate::shell::input_route::ctrl_d_action(st, is_last) {
                                    crate::shell::input_route::CtrlD::SendEof => {}
                                    crate::shell::input_route::CtrlD::ClosePane => {
                                        let id = self.active_ws().map(Workspace::focus);
                                        if let (Some(id), Some(ws)) = (id, self.active_ws_mut()) {
                                            ws.close_pane(id);
                                        }
                                        mark_ui_dirty!(self.ui_dirty);
                                        self.request_ui_redraw();
                                        return;
                                    }
                                    crate::shell::input_route::CtrlD::CloseTab => {
                                        // 复用既有的关标签路径(Ctrl+W / 菜单「断开」
                                        // 走的同一条):它负责 abort 自动化、
                                        // 收 sftp task、按顺序 drop workspace。
                                        self.close_active_tab();
                                        mark_ui_dirty!(self.ui_dirty);
                                        self.request_ui_redraw();
                                        return;
                                    }
                                }
                            }
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
                            // F155:记下这次按键的时刻,等下一段入站字节来时
                            // 算回显往返。加在这里而不是键盘事件入口 ——
                            // 被 egui 吞掉、或 launcher 态没有终端可写的那些键
                            // 永远等不到回显,计进去只会把分布拉偏。`bytes` 为空
                            // 时不发往远端(当前 `encode_key` 不会,但防御一下),
                            // 不该记,否则等不到回显还占着上一次按键的起点。
                            if !bytes.is_empty() {
                                diag::note_key();
                            }
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
                // F157:唤醒率读数。必须排在分支第一句,提前 return 的路径
                // 也要计入。
                diag::count_wake();
                let now = self.now_ms();
                // 1+2. 排空每个 pane 的 rx→feed emu→回写各自的 PtyWrite(T1 红线)——
                // 仅终端态有字节可处理;launcher 态(ws=None)没有终端,跳过,但下面的帧率闸 + egui
                // 渲染仍要跑(egui 在 launcher 也要画占位 UI)。
                self.pump_io();
                // F128:这一帧该重拨的连接。挂在 IO 泵之后——`pump_io` 里的
                // `ws.pump` 才会把 rx 关闭翻译成 `PaneStatus::Reconnecting`,
                // 顺序反了就晚一帧才发起重拨。
                self.drive_reconnects();
                // F162:跨机器串行恢复的下一条拨号。队列空时零成本早退。
                self.drive_restore_dial();
                // 1.5 F55/F59:传输队列每帧推进一次(放行 worker + 采样速率)。
                // **放在帧里而不是事件里**是 T3:进度事件每秒几千条,靠它们
                // 驱动重绘就是风扇起飞;这里只把「队列在跑」当成脏,重绘频率
                // 因此由下面那段排期(~5Hz)决定,与事件频率无关。
                self.pump_transfers();
                // F169:存一份 summary 给下面 gauge 段复用,同一帧里不用再问队列
                // 第二遍——队列状态在这之后到 gauge 段之间不会再变。
                let xs = self.transfer.queue.summary();
                if xs.busy {
                    self.transfer.queue.tick(self.start.elapsed().as_secs_f64());
                    mark_ui_dirty!(self.ui_dirty);
                }
                // 1.6 F55:有 job 挂在冲突上就把处置框弹出来。**绝不静默覆盖**;
                // 也不重复弹:已经有别的对话框开着时等它先关掉。
                if self.ui.files_dialog.is_none() {
                    if let Some(j) = self.transfer.queue.first_conflict() {
                        self.ui.files_dialog =
                            Some(crate::ui::files_dialog::FilesDialog::Conflict {
                                name: j.label.clone(),
                                job: j.id,
                                apply_all: false,
                            });
                        mark_ui_dirty!(self.ui_dirty);
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
                // dirty:两态**同一条判据** —— 「远端来了新字节(pacer,含同步块
                // 探测)」与「egui 要重绘」的并集(见 `frame::frame_is_dirty`)。
                //
                // F158:这里原本还有一句 `None => true`(launcher 态无条件判脏),
                // 已摘掉。当时给的理由是「`ControlFlow::Wait` 下 winit 不会凭空
                // 生成 `RedrawRequested`」,而它在同一函数别处会排 `WaitUntil`
                // 的前提下不成立:present 之后那段一旦拿到有限的 `repaint_delay`
                // 就排一次 `WaitUntil`,到点 `about_to_wait` 补一次 `request_redraw`
                // —— 闭环自激。日志坐实:`tabs=0 panes=0` 时照样
                // `frame=300x/present=300`,对着一屏静止的占位 UI 每秒提交 60 帧。
                //
                // 摘掉之后 `ui_dirty` 成为 launcher 态的唯一判据,而它是
                // 75 个置脏点 : 1 个清脏点的结构。兜底改由 F159 的整帧指纹
                // (构造式)提供:漏标脏最多晚一帧,不会永久卡住。
                let terminal_dirty = match self.active_ws() {
                    Some(ws) => crate::render::panes_ready_to_present(
                        ws.panes().iter().map(|p| &p.pacer),
                        now,
                    ),
                    None => false,
                };
                // F155:重绘归因。两个布尔必须与下面 `frame_is_dirty` 收到的是
                // 同一对,否则剖面里「远端来了字节」与「egui 要重绘」全是假的。
                diag::count_redraw(terminal_dirty, self.ui_dirty);
                // F155:此刻的规模。三条 relaxed 原子存,可忽略。
                diag::set_scale(
                    self.tabs.len(),
                    self.active_ws().map_or(0, |ws| ws.pane_count()),
                    self.active_ws().map_or(0, |ws| ws.hosts.len()),
                );
                // F169:内存记账 gauge。遍历全部标签(不是只有活动 ws):
                // 后台标签的 scrollback 也占内存。几十个 pane 的整数乘加,帧预算内。
                let mut scroll_bytes = 0u64;
                let mut scroll_lines = 0u64;
                for tab in self.tabs.iter() {
                    if let Some(t) = tab.content.as_terminal() {
                        for p in t.ws.panes() {
                            scroll_bytes += p.emulator.scrollback_bytes() as u64;
                            scroll_lines += p.emulator.scrollback_lines() as u64;
                        }
                    }
                }
                let text_bytes = self
                    .active
                    .as_ref()
                    .map_or(0, |a| a.text.bytes_estimate() as u64);
                diag::set_mem_gauges(scroll_bytes, scroll_lines, text_bytes);
                diag::set_xfer_gauges(
                    xs.active as u64,
                    (xs.up + xs.down) as u64,
                    xs.bytes_total.saturating_sub(xs.bytes_done),
                );
                let dirty = crate::frame::frame_is_dirty(terminal_dirty, self.ui_dirty);
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
                            let mut snaps: Vec<_> = self
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
                            // F210:组字期间把焦点 pane 的光标钉回锚点。**改在
                            // 快照上**是有意的:内联拼音的位置、让路区间、光标
                            // quad、整帧指纹全都只读 `snap.cursor`,一处改完下游
                            // 天然同源;逐个消费点各判一份迟早漏掉一个,而漏掉的
                            // 那一个就是「拼音和候选框分家」。夹紧到本帧网格是因为
                            // reflow 会让锚点越界,越界的行会把 quad 画到邻居 pane 上。
                            for (g, s) in snaps.iter_mut() {
                                if Some(g.id) != focus {
                                    continue;
                                }
                                let cell = self.ime.anchored_cursor(
                                    g.id,
                                    (s.cursor.col, s.cursor.row),
                                    (s.cols, s.rows),
                                );
                                s.cursor.col = cell.0;
                                s.cursor.row = cell.1;
                            }
                            let renders: Vec<crate::gpu::PaneRender<'_>> = snaps
                                .iter()
                                .map(|(g, s)| {
                                    let focused_here = Some(g.id) == focus;
                                    crate::gpu::PaneRender {
                                        geom: *g,
                                        snap: s,
                                        focused: focused_here,
                                        // F126:输入法一次只对一个 pane 生效,
                                        // 非焦点 pane 不画拼音串。
                                        preedit: if focused_here { self.ime.preedit() } else { "" },
                                    }
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
                                                // D3/D6:`host_pending` 时这块 pane
                                                // 还没连上**它自己**那台机器,
                                                // `host_ix` 指着主叶子(别人那台),
                                                // 借它的名字会让一块占位 pane 显示成
                                                // 一台其实连得好好的机器(见
                                                // `PaneState::host_pending` 的文档)。
                                                host: ws.pane(g.id).and_then(|p| {
                                                    if p.host_pending {
                                                        return None;
                                                    }
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
                                                // 同上:`host_pending` 时 `host_ix`
                                                // 不代表自己,外观也不能借。
                                                appearance: ws
                                                    .pane(g.id)
                                                    .filter(|p| !p.host_pending)
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
                                                // F163/D4:attach 失败 / 会话已删 /
                                                // 连不上的说明,挂在这块 pane 自己
                                                // 的标题条上。
                                                notice: ws
                                                    .pane(g.id)
                                                    .and_then(|p| p.notice.as_deref()),
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
                            // F155:整帧耗时。量在**调用点**而不是 `render_frame`
                            // 里面 —— 那个函数有五条跳帧的提前 return,在里面量
                            // 会把跳帧那几路全漏掉,而跳掉的帧同样消耗了时间,
                            // 漏记会让 p95 偏乐观。
                            // `Instant::now` 在 Windows 上走 QPC,约 20~30ns,
                            // 每帧两次可忽略;**绝不能**在这里做格式化(T3)。
                            let frame_started = Instant::now();
                            let (repaint_delay, mut actions) = render_frame(
                                a,
                                &renders,
                                &mut self.ui,
                                frame,
                                sidebar_arg,
                                content_arg,
                                files_owner_generation.unwrap_or(0),
                                &self.transfer.queue,
                                &mut self.edit,
                                blink_on,
                            );
                            diag::record_frame_us(frame_started.elapsed().as_micros() as u64);
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
                                // F155/T2:各 pane 的同步块计数在 present 之后收口再
                                // 汇总上报——与 `mark_presented` 用同一个 `now`(ms
                                // 时基,见 `self.now_ms()`),否则超时判据会跟 `feed`
                                // 用的时钟基准对不上。
                                let mut sync_blocks = 0u32;
                                let mut sync_timeouts = 0u32;
                                for p in ws.panes_mut_iter() {
                                    p.pacer.mark_presented(now);
                                    let (blocks, timeouts) = p.pacer.take_counts();
                                    sync_blocks = sync_blocks.saturating_add(blocks);
                                    sync_timeouts = sync_timeouts.saturating_add(timeouts);
                                }
                                diag::count_sync(sync_blocks as u64, sync_timeouts as u64);
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
                            // F148:恢复列表的结论。
                            if let Some(out) = actions.history.take() {
                                // 无论恢复还是不恢复,弹窗都收掉 —— 「恢复」之后
                                // 还留着的话,用户会以为可以再选一条(而那时
                                // 本实例已经接管了槽位)。
                                self.ui.history = None;
                                if let crate::ui::history::HistoryOut::Restore(id) = out {
                                    self.restore_history(&id);
                                }
                                mark_ui_dirty!(self.ui_dirty);
                            }
                            if std::mem::take(&mut self.ui.history_request) {
                                self.open_history_dialog();
                            }
                            // F155:导出脱敏日志。两个入口(菜单/设置)共用
                            // 这一条施加路径,见 `drain_export_log_request`。
                            self.drain_export_log_request();
                            // 点了 pane 标题条的「换节点」:开弹窗,真正换在
                            // 用户选完之后。**只开弹窗,不预判节点** —— 这一步
                            // 没有任何默认答案可猜。
                            if let Some(pane) = actions.rehost_pane {
                                self.ui.rehost = Some(crate::ui::rehost::RehostDraft::new(pane));
                                mark_ui_dirty!(self.ui_dirty);
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
                                    mark_ui_dirty!(self.ui_dirty);
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
                                    mark_ui_dirty!(self.ui_dirty);
                                }
                                Some(crate::ui::chrome::TabAction::Close(ix)) => {
                                    if let Some(tab) = self.tabs.close(ix) {
                                        // F55:标签没了,它的传输也就没有落点/
                                        // 连接了 —— 先作废再收口。
                                        self.cancel_transfers_of(tab.content.generation());
                                        wind_down(tab);
                                    }
                                    mark_ui_dirty!(self.ui_dirty);
                                }
                                Some(crate::ui::chrome::TabAction::NewSession) => {
                                    self.ui.session_manager_open = true;
                                    mark_ui_dirty!(self.ui_dirty);
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
                                    mark_ui_dirty!(self.ui_dirty);
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
                                let _ = self.reconnect_tab(id);
                            }
                            if actions.reconnect_all {
                                self.reconnect_next_restored();
                            }
                            // F199:面板上按了一下鼠标 —— 键盘焦点跟过去。
                            // 放在下面 `files_owner_generation` 那个 `if let`
                            // **之外**:焦点是 `App` 级的一份状态,不按标签
                            // 路由;而且面板画得出来就说明属主标签在场,再判
                            // 一遍只会多一条永远走不到的分支。
                            if actions.files_focus_click {
                                self.focus = shell::input_route::Focus::FilesPanel;
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
                                        if let Some(c) = self.transfer.cancels.get(&id) {
                                            c.store(true, std::sync::atomic::Ordering::Relaxed);
                                        }
                                        self.transfer.queue.cancel(id);
                                    }
                                    TransferUiAction::CancelAll => {
                                        for c in self.transfer.cancels.values() {
                                            c.store(true, std::sync::atomic::Ordering::Relaxed);
                                        }
                                        self.transfer.queue.cancel_all();
                                    }
                                    TransferUiAction::ClearFinished => {
                                        self.transfer.queue.clear_finished()
                                    }
                                }
                                mark_ui_dirty!(self.ui_dirty);
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
                                        if let Some(ed) = self.edit.editor.as_mut() {
                                            let (key, bytes, backup) =
                                                (ed.key, ed.bytes(), ed.backup);
                                            ed.busy = true;
                                            // 用户把「留一份 .mullion.bak」关了:
                                            // 把原文丢掉,这条回传就不会带备份。
                                            if !backup {
                                                self.edit.originals.remove(&key);
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
                                        crate::edit::tempdir::purge(&self.edit.root);
                                        event_loop.exit();
                                    }
                                    ExitChoice::Cancel => {
                                        // 回去处理:把「编辑中」展开,否则用户
                                        // 关掉框之后还得自己去找那一行。
                                        self.ui.edit_expanded = true;
                                    }
                                }
                                mark_ui_dirty!(self.ui_dirty);
                            }
                            // F100:导出的 Markdown 送剪贴板。写剪贴板是 IO,`ui/`
                            // 那一层只画不做 IO,所以在这里发起(同 F18 的复制路径)。
                            if let Some(md) = actions.annotate_export {
                                self.clipboard.set(&md);
                                self.ui.set_toast("标注已复制,粘进 Claude Code");
                                mark_ui_dirty!(self.ui_dirty);
                            }
                            // F83 标题条开关:改的是行数,下一帧 compute_geoms
                            // 算出新 grid,再由 apply_geometry 发 window_change。
                            if self.ui.toggle_title_bars {
                                self.ui.toggle_title_bars = false;
                                if let Some(ws) = self.active_ws_mut() {
                                    ws.title_bars = !ws.title_bars;
                                }
                                mark_ui_dirty!(self.ui_dirty);
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
                                mark_ui_dirty!(self.ui_dirty);
                                let at = Instant::now() + repaint_delay;
                                self.next_frame_at = Some(at);
                                event_loop.set_control_flow(ControlFlow::WaitUntil(at));
                            } else if self.transfer.queue.summary().busy {
                                // F59:队列在跑时自己排下一帧。不排的话画面会
                                // 冻在传输开始那一帧 —— 进度事件按 T3 刻意不
                                // 请求重绘,没有别的东西会唤醒事件循环。
                                // T7:这一支同样显式复位 control_flow。
                                mark_ui_dirty!(self.ui_dirty);
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
                        // F155:被帧闸挡下的次数(T3 的直接体感指标)。
                        diag::count_throttled();
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
                        diag::count_scroll();
                    }
                    // 滚动改了 display_offset,选区终点要按新视口重新落点,
                    // 否则拖到边缘后画面在滚、选区却停在原地不长。
                    self.update_selection_endpoint();
                    mark_ui_dirty!(self.ui_dirty);
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
                    mark_ui_dirty!(self.ui_dirty);
                }
                // 换节点的发起点。放在这里(而不是渲染闭包里)的理由同
                // `tab_props_save`:闭包里 `self.ui`/`self.active` 正被借出去。
                if let Some((pane, session)) = self.ui.rehost_request.take() {
                    // 用户在 pane 标题条上亲手指定的 —— 就是当前活动标签。
                    if let Some(g) = self.active_ws().map(|ws| ws.generation()) {
                        // 用户手点「换节点」,同步早退时 `spawn_rehost_on` 已经
                        // `set_error` 给了 toast,这里的返回值只有串行队列才需要看。
                        let _ = self.spawn_rehost_on(g, pane, session, RehostKind::UserPicked);
                    } else {
                        // 活动标签不是终端(文件标签 / launcher)。到不了:换节点的
                        // 入口是 pane 标题条,而标题条只有终端标签才画。
                        log::warn!(target: "mullion", "换节点:活动标签不是终端,忽略");
                    }
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
                    // F17:同一个门控 —— 改了会话/分组就可能改了 `scrollback`
                    // 的继承结果,推给在跑的 pane(见 `refresh_scrollback`)。
                    self.refresh_scrollback();
                }
                // 「选择…」私钥文件:同样是 egui 闭包只记意图、这里才施加。
                if std::mem::take(&mut self.ui.pick_key_request) && !self.picker_busy.key {
                    self.picker_busy.key = true;
                    self.spawn_file_picker("选择私钥文件", None, None, UserEvent::KeyPathPicked);
                }
                // 凭据表单里的「选择…」私钥。与会话侧共用 `PickerBusy::key`
                // (一次只该开一个对话框),但回送的是**另一个事件** ——
                // 正文要写进哪个缓冲,只有事件变体分得清。
                if std::mem::take(&mut self.ui.pick_credential_key_request) && !self.picker_busy.key
                {
                    self.picker_busy.key = true;
                    self.spawn_file_picker(
                        "选择私钥文件",
                        None,
                        None,
                        UserEvent::CredentialKeyPathPicked,
                    );
                }
                // 「导入 .ico…」:同上。加扩展名过滤,免得用户选中 .png 才被
                // 告知不行 —— 归一化只吃 .ico 容器。
                if std::mem::take(&mut self.ui.pick_icon_request) && !self.picker_busy.icon {
                    self.picker_busy.icon = true;
                    self.spawn_file_picker(
                        "选择图标文件",
                        Some(("图标", &["ico"])),
                        None,
                        UserEvent::IconPathPicked,
                    );
                }
                // F2:导入 ssh config。初始目录指向 `~/.ssh`(设计 D7),
                // 省掉用户手动翻目录;不加扩展名过滤 —— config 文件没有后缀。
                if std::mem::take(&mut self.ui.import_pick_request) && !self.picker_busy.import {
                    self.picker_busy.import = true;
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
                    // D1/F50:`dial_plan_for` 多带回 `wants_sftp`——点「连接」
                    // 要靠它决定 `ConnectOk` 抵达时开终端标签还是文件标签。
                    match self.store.as_ref().map(|s| s.dial_plan_for(id)) {
                        Some(Ok((cfg, wants_sftp))) => {
                            // 用户主动发起的连接是交互态,不该继承 CLI 直连的
                            // exit(1) 语义(复核 #1)。
                            self.cli_direct = false;
                            // F205:跳过标志跟这次拨号的其余随行数据一起装进票里
                            // (从前它是 `App` 上的单槽,得靠「只在 `spawn_connect`
                            // 里写」这条约定才不会漂到另一条在途连接上)。失败支
                            // (配置坏了走 Err)压根不发票,自然也带不走它。
                            self.spawn_connect(cfg, wants_sftp, Some(id), skip_automation);
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
                    diag::count_request_redraw(diag::RedrawSource::Scheduled);
                    a.window.request_redraw();
                }
            }
        }
        // F37:到点就比一比布局有没有变、变了就写盘(E7)。放在这里而不是
        // 帧循环里 —— 它跟渲染无关,而 `about_to_wait` 是「已经闲下来了」
        // 这个语义唯一准确的位置。
        self.flush_layout_if_due();
        // F148:心跳。**与落盘分开的一次独立写**,理由见 `heartbeat_at`:
        // 布局没变时不落盘,心跳却必须照写,否则开着不动的窗口会被别的实例
        // 判成死的。
        self.tick_heartbeat();
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

/// D3/D6 共用的落地逻辑:把一块 pane 摆成/改成「拨不了号」的占位态
/// (`PaneStatus::Disconnected` + `notice` 说明),**不许**改动分屏的形状 ——
/// D3 的全部意义就是「树上有几个叶子就还是几个叶子,比例不许变形」,少了
/// 一块叶子等于把用户的分屏布局悄悄砸掉。
///
/// 返回 `true` = 真的改动了画面(调用方该打脏),`false` = 早退(`pane` 已经
/// 不在树上了 —— 恢复途中用户切了预设 / 关了那块 pane)。
///
/// 纯函数(只碰 `&mut Workspace` + 现算好的 `scrollback`),抽成自由函数
/// 是因为 `place_dead_pane` 要 `&mut self`(取 `store`/`ui_dirty`),无头环境
/// 造不出 `App`;这样能拿真实构造的 `Workspace` 直接单测(同 `rehost_pane`)。
fn place_dead_pane_of(
    ws: &mut Workspace,
    pane: PaneId,
    generation: u64,
    scrollback: usize,
    msg: &str,
) -> bool {
    if !pane_still_wanted(ws, pane, generation) {
        return false;
    }
    if let Some(p) = ws.pane_mut(pane) {
        // 已经有 `PaneState` 了(拨号途中降级):只改状态与说明,
        // 别把 emulator 重建掉 —— 里面可能有用户想看的报错。
        p.status = crate::shell::workspace::PaneStatus::Disconnected;
        p.host_pending = true;
        p.notice = Some(msg.to_string());
        return true;
    }
    // `PaneState::rx` 是 `tokio::sync::mpsc::Receiver<Vec<u8>>`。丢掉发送端
    // 之后它恒返回 `None` —— 喂数据那条路会把它当「对端已关」处理,正是
    // 我们要的语义,不必新加分支。
    let (dead_tx, dead_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
    drop(dead_tx);
    ws.attach_pane(crate::shell::workspace::PaneState {
        id: pane,
        host_ix: 0,
        emulator: new_pane_emulator(scrollback),
        pty: Box::new(crate::shell::workspace::DeadPty),
        rx: dead_rx,
        pacer: SyncFramePacer::new(),
        status: crate::shell::workspace::PaneStatus::Disconnected,
        saw_first_byte: false,
        last_grid: (0, 0),
        cwd: None,
        tmux: None,
        history_reported: 0,
        // 它从来没连上过自己那台机器 —— 身份要照抄盘上那份。
        host_pending: true,
        notice: Some(msg.to_string()),
    });
    true
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
/// 照发)。`hosts[0]` 还额外被 sftp 侧栏认着。代价是那条连接闲置到整个标签
/// 关闭为止——换节点是低频操作,拿一条闲置连接换掉一整类静默的错位 bug
/// 是划算的。
///
/// F128 补充:**只有换机器才 push**。断线重连走的是就地替换
/// (`UserEvent::PaneReconnected`),因为那是同一台机器的连接换了一条,
/// `hosts[ix]` 的语义始终是「第 ix **台机器**的当前连接」。理由详见那个分支。
///
/// `host_ix` 由调用方现算(`ws.hosts.len() - 1`,刚 push 进去那条)。**不在这里
/// push `HostConn`**:那个类型里揣着 `Arc<SshConnection>`,而 `SshConnection`
/// 的字段是私有的、只能由真实握手造出来 —— 收它当参数等于把这整段判定推出
/// 单测范围。收一个已经算好的下标,这段就能拿真实构造的 `Workspace` 直接测。
///
/// 纯函数(只碰 `&mut Workspace`),不要 runtime/proxy,可脱离真实事件循环单测。
///
/// 成功时顺带把**分屏焦点**设到这块 pane(F156-b)。失败路径不设 —— 开头的
/// `pane_still_wanted` 早退挡在前面(`set_focus` 自己也有成员校验,但让早退
/// 先挡住,语义更清楚)。
///
/// **不在这里清 `leaf_wanted`/`leaf_detach`**:那两张表挂在 `TerminalTab` 上,
/// 这个函数只碰 `&mut Workspace`(理由见上面「纯函数」那句)。清理放在
/// `clear_leaf_attach_intent`,由 `on_pane_rehosted`/`on_pane_rehost_err` 调用。
// 每个参数都是「必须由调用方现算、这里造不出来」的那类(`host_ix` 见上,
// `scrollback` 要查 store,`kind` 见 `RehostKind`)。打包成结构体只是把同样
// 几个值换个地方写,换不来任何检查。
#[allow(clippy::too_many_arguments)]
fn rehost_pane(
    ws: &mut Workspace,
    id: PaneId,
    generation: u64,
    host_ix: usize,
    pty: Box<dyn crate::shell::workspace::PtyWriter>,
    rx: Receiver<Vec<u8>>,
    scrollback: usize,
    kind: RehostKind,
) -> bool {
    if !pane_still_wanted(ws, id, generation) {
        return false;
    }
    if ws.pane(id).is_none() {
        // F188:`pane_still_wanted` 只保证 id 在**树**上;`PaneState` 是另一
        // 回事。F162 的恢复队列拨的正是这种叶子 —— `apply_saved_tree` 给它
        // 分配了 id、占好了树上的位,但 `PaneState` 要等第一条 channel 开好
        // 才有。这不是「pane 没了」,而是「pane 还没生出来」,拿到手的连接
        // 必须挂上去,丢掉就是那一格永远停在「N · 连接中…」。
        //
        // (原先这里写死了「换节点的入口是标题条,只有画得出来的 pane 才有
        // 标题条,所以到不了这里」—— F162 让恢复队列也走这条链路之后,那个
        // 不变量就不成立了。)
        ws.attach_pane(crate::shell::workspace::PaneState {
            id,
            host_ix,
            emulator: new_pane_emulator(scrollback),
            pty,
            rx,
            pacer: SyncFramePacer::new(),
            status: crate::shell::workspace::PaneStatus::Live,
            saw_first_byte: false,
            last_grid: (0, 0),
            cwd: None,
            tmux: None,
            history_reported: 0,
            // 这条 channel 就是它自己那台机器的,身份不再是「照抄盘上那份」。
            host_pending: false,
            notice: None,
        });
        focus_after_rehost(ws, id, kind);
        return true;
    }
    let p = ws
        .pane_mut(id)
        .expect("上一句刚判过 `ws.pane(id).is_none()`,这里必有");
    // F17:回溯行数跟**新节点**那条会话走,与下面清 `cwd`/`tmux` 同一个道理
    // ——这一格从此属于另一台机器了。
    p.emulator = new_pane_emulator(scrollback);
    // 换了仿真器,夹紧状态要重新报告一次(`history_reported` 的语义是
    // 「上次报告过的是哪个数」,不重置就可能把新仿真器的第一次夹紧吞掉)。
    p.history_reported = 0;
    // ⑥:`cwd`/`tmux` 同 `emulator` 一个道理——旧值是上一台机器嗅出来的
    // (OSC 7 目录 / 窗口标题里的 tmux 会话名),留着会在标题条右区挂一条
    // 「看起来对、其实属于上一台机器」的过期标注,而且 `cwd` 是"只增不清"
    // 语义(见字段注释),不会被新机器的输出自然覆盖掉一个空值,必须在这里
    // 主动清空。
    p.cwd = None;
    p.tmux = None;
    swap_pane_channel(p, host_ix, pty, rx);
    focus_after_rehost(ws, id, kind);
    true
}

/// F156-b/F188:挂完之后焦点该不该跟到这块 pane。
///
/// - `UserPicked`:跟。用户刚在标题条上亲手指定了新节点,下一步必然是往
///   它里面敲东西,不跟他得再点一下。
/// - `RestoreFirstMount`:**不跟**。恢复现场是后台批量拨号,`apply_saved_tree`
///   已经按存盘的 `focus_leaf` 摆好焦点了;这里再抢,焦点最后会落在「碰巧
///   最后一个拨通」的那块 pane 上 —— 拨通顺序取决于网络,同一份现场每次
///   恢复出来的焦点还不一样。
///
/// **放在自由函数里、不放事件分支里**:这里能拿真实构造的 `Workspace` 直接
/// 断言 `ws.focus()`;放事件分支只能写「读 `app.rs` 源码找字符串」式的断言,
/// 那是本项目反复踩到的恒绿模式。
///
/// `reattach_pane`(F128 断线自动重连)**刻意不跟着改**,理由见
/// `rehosting_moves_the_focus_to_that_pane_but_reattaching_never_does`。
///
/// 只动分屏焦点,不动 egui 的输入焦点:此刻输入焦点若在文件侧栏,本片不
/// 把它抢回终端(那是另一类语义,用户没要)。
fn focus_after_rehost(ws: &mut Workspace, id: PaneId, kind: RehostKind) {
    if kind == RehostKind::UserPicked {
        ws.set_focus(id);
    }
}

/// F160/F161:换节点作废这块 pane 原来的「该接回哪个 tmux 会话」记录。
///
/// `leaf_wanted`/`leaf_detach` 是恢复现场时**给全部叶子**一次性写的
/// (`accept_connect_ok`),包括会话已被删的 Orphan(D3)和之后拨号失败被降级
/// 的那些(D6)——这两类 pane 从来没有成功走过 `take_attach_intent`,记录
/// 原封不动带着**旧机器**的会话名留着。用户对着这样一块 pane 点「换节点」
/// 换到一台完全不相关的机器时,`take_attach_intent` 会把这条陈旧记录当成
/// 新机器的意图:轻则用旧会话名顶掉 `on_pane_ready` 为新机器算好的
/// `pending.plan`(新机器自己的登录后命令被静默丢弃),重则新机器上碰巧有
/// 同名 tmux 会话(`main`/`work` 这类名字完全可能撞上)——真的 attach 上去,
/// 且若 `leaf_detach` 是 `true` 还会带 `-d` 把新机器上别人的客户端踢下线。
///
/// **移除而不是刷新**:换节点这一刻还没有新机器的实测名可填,唯一正确的
/// 状态是「这块 pane 暂时没有 attach 意图」,等它自己连上之后由 F128 的实测
/// 流程(`on_pane_reconnected`)重新写入。
///
/// 成功、失败两条路径(`on_pane_rehosted`/`on_pane_rehost_err`)都要调:
/// 失败会把这块 pane 降级成占位态(D6),留着旧记录就是给下一次换节点埋雷。
///
/// 抽成自由函数(只碰 `&mut TerminalTab`,不要 `&mut self`):`rehost_pane` 只
/// 拿得到 `&mut Workspace`(见其文档),够不着这两张表;这样也能拿真实构造
/// 的 `TerminalTab` 直接单测,不必造 `App`。
fn clear_leaf_attach_intent(t: &mut TerminalTab, pane: PaneId) {
    t.leaf_wanted.retain(|(id, _)| *id != pane);
    t.leaf_detach.retain(|(id, _)| *id != pane);
}

/// 换 channel 时两条路径(换机器 / 重连)**共同**要做的事。
///
/// 抽出来是因为漏掉其中任何一条都会产生难查的 bug:`last_grid` 漏了是 T4
/// (远端按 80 列排版),`saw_first_byte` 漏了是自动化在一条还没说话的 channel
/// 上开跑,`pacer` 漏了是上一条 channel 没收口的同步块把新内容一直攒着(T2)。
fn swap_pane_channel(
    p: &mut crate::shell::workspace::PaneState,
    host_ix: usize,
    pty: Box<dyn crate::shell::workspace::PtyWriter>,
    rx: Receiver<Vec<u8>>,
) {
    p.host_ix = host_ix;
    // F140:**先显式关掉旧 channel**。旧的 `pty`/`rx` 会在下面两句赋值里被
    // Drop,但 Drop 关不掉 channel —— russh 0.54.5 的 `ChannelWriteHalf` 没有
    // `Drop` 实现(这行原本的注释写反了)。不关的话,换一次节点 / 重连一次
    // 就在远端留一个挂着的 shell,并占着一个 channel slot。
    p.pty.close();
    p.pty = pty;
    p.rx = rx;
    p.pacer = SyncFramePacer::new();
    p.status = crate::shell::workspace::PaneStatus::Live;
    // 自动化的「就绪」判据要重新攒(新 channel 还一个字节都没说话);`last_grid`
    // 给不可能的初值,逼下一帧 apply_geometry 发一次 window_change(T4)——
    // 新开的 channel 是 80x24,不发的话远端按 80x24 排版。
    p.saw_first_byte = false;
    p.last_grid = (0, 0);
}

/// F128:断线重连之后把 pane 挂到**新开的 channel** 上。
///
/// 与 `rehost_pane` 的唯一差别:**保留 `emulator`**(以及它嗅出来的 `cwd`/`tmux`)。
/// 还是同一台机器、同一个用户,断线前那一屏内容是用户想看的东西
/// (往往正是导致断线的那条报错),重建等于当场抹掉。`host_ix` 仍要传:
/// 重连会往 `ws.hosts` 里 push 一条新的 `HostConn`(旧的那条连接已经死了)。
///
/// F128 拆成 5 个任务:这是第 3 个,只落这个函数本身;调度(Task 14 的退避算法 /
/// Task 15 的接线)接上之后,调用方是 `UserEvent::PaneReconnected` 分支。
fn reattach_pane(
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
        return false;
    };
    swap_pane_channel(p, host_ix, pty, rx);
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

/// F132:「文件侧栏关→开」那一帧该做什么。
#[derive(Debug, Clone, PartialEq, Eq)]
enum SyncPlan {
    /// 什么都不做(还没连上 / pane 没报过目录)。
    Nothing,
    /// 同一台机器,只把远端栏带到这个目录。
    Goto(String),
    /// 焦点分屏在**另一台**机器上:摘掉现在这条 sftp channel,在那台上重开。
    Reopen,
}

/// [`SyncPlan`] 的判定。纯函数 —— `App` 要 `EventLoopProxy`,无头测试里
/// 造不出来,只有把判定摘出来才验得了。把这几个早退埋在 `&mut self` 方法体里
/// 的话,顺序换掉、或者把 `has_client` 判断写反,都不会有任何测试变红。
///
/// `focus_host_ix` 为 `None` = 这个标签没有终端(SFTP 节点标签),
/// 「焦点分屏在哪台」无从谈起,不重开。
///
/// 不接「配置的默认远端目录」参数(`files_start_dir` 第二参这里固定传
/// `None`):面板已经开着了,拿不到 pane 目录时退回配置值会把用户当前的
/// 导航位置拽走,宁可什么都不做 —— 这是它与 `trigger_sftp_open`(会传
/// `default_remote` 兜底)刻意不同的地方。
///
/// `Goto` 带 `String` 而不是 `mullion_ssh::sftp::RemotePath`:后者的构造在
/// `mullion-ssh`,这里保持零依赖更好测;调用方自己转。
fn sync_plan_of(
    has_client: bool,
    sftp_host_ix: Option<usize>,
    focus_host_ix: Option<usize>,
    pane_cwd: Option<&[u8]>,
    home: Option<&[u8]>,
) -> SyncPlan {
    if !has_client {
        return SyncPlan::Nothing;
    }
    if let Some(fix) = focus_host_ix {
        if sftp_host_ix != Some(fix) {
            return SyncPlan::Reopen;
        }
    }
    match files_start_dir(pane_cwd, None, home) {
        Some(dir) => SyncPlan::Goto(dir),
        None => SyncPlan::Nothing,
    }
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
///
/// `host_ix`(F132):`handle` 对应 `ws.hosts` 的哪一台,原样带在
/// `UserEvent::SftpOpened` 里回去,让 `accept_sftp_opened` 记下这条 channel
/// 的真实归属——开 channel 期间用户可能已经换了焦点分屏,不能等回来时现算。
fn spawn_sftp_open(
    runtime: &Runtime,
    proxy: &EventLoopProxy<UserEvent>,
    generation: u64,
    host_ix: Option<usize>,
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
        let _ = proxy.send_event(UserEvent::SftpOpened {
            generation,
            host_ix,
            result,
        });
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

/// F142:异步问一批 uid/gid 的名字。结果经 `UserEvent::OwnerNames` 回送
/// (`App::accept_owner_names` 接)。
///
/// **成败都回送**:失败时送 `stdout: None`,接收方据此撤回负缓存。静默失败
/// 的后果是这批 id 在这条连接的余生里永远显示成数字。
///
/// 同样**返回 `JoinHandle`,调用方必须存进 `sftp_tasks`** —— 理由同
/// `spawn_sftp_list_dir`:关标签/断连时要能立刻收口,而不是等这条 exec
/// 自己的网络超时。
fn spawn_getent(
    runtime: &Runtime,
    proxy: &EventLoopProxy<UserEvent>,
    generation: u64,
    conn: Arc<SshConnection>,
    query: crate::files::owners::Query,
) -> tokio::task::JoinHandle<()> {
    let proxy = proxy.clone();
    runtime.spawn(async move {
        let stdout = match mullion_ssh::exec::exec(&conn, query.command()).await {
            // 退出码**不看**:`getent` 一个 id 都没查到时返回 2,而那是完全
            // 正常的结果(容器里的孤儿 uid)。有多少条解出多少条,`parse`
            // 自己会跳过垃圾行。
            Ok(out) => Some(out.stdout),
            Err(e) => {
                // sftp-only 账号(`ForceCommand internal-sftp`)会走到这儿 ——
                // 属主列退回数字,别的功能照常。不弹 toast:用户没主动要过
                // 这次查询,为它弹一个错误框是噪音。
                log::debug!(target: "mullion", "getent 查属主名字失败:{e}");
                None
            }
        };
        let _ = proxy.send_event(UserEvent::OwnerNames {
            generation,
            query,
            stdout,
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

/// F158:这个后台事件该不该把 egui 侧标脏。
///
/// **穷尽 `match`,不许用 `_`**:摘掉 launcher 的无条件出帧兜底之后,
/// `ui_dirty` 成了 launcher 态的唯一判据,漏标一处的症状是「连上了画面
/// 不动,动一下鼠标才刷出来」。加新变体时这里编译报错,强迫作者表态。
///
/// 方向也刻意不对称:判 `true` 最多多画一帧(而且会被 F159 的整帧指纹
/// 拦掉),判 `false` 是画面卡住。所以默认落在标脏那一侧,只对已知的
/// 高频事件显式豁免。
///
/// **分支不写枚举名前缀**(局部 `use UserEvent::*;`):好几条既有的源码切片
/// 守护测试(如 `a_freshly_opened_pane_starts_its_own_automation`)拿"枚举名
/// 前缀 + 变体名 + 左花括号"拼出的字符串当 `rsplit` 锚点,从 `user_event` 里
/// 抠真正的 match 分支——这里若也写出同一串前缀加变体名,会在源码里制造
/// 第二个匹配,那些测试会切到这段而不是真正的分支,静默切错(已实测复现:
/// 10 个既有测试从「测真分支」变成「测到这段的空隙」)。裸变体名不含那个
/// 前缀,不会命中那些锚点——出于同一原因,这条注释也刻意不把前缀和变体名
/// 拼在一起写出来。
fn user_event_marks_dirty(e: &UserEvent) -> bool {
    use UserEvent::*;
    match e {
        // ——— 豁免之一:每秒几千条,靠帧内排期驱动画面 ———
        //
        // `Wake` 是「远端来了字节」的通知,画面该不该更新由 `terminal_dirty`
        // (pacer,含 T2 的同步块攒帧)判;在这里标脏等于把攒帧闸整个绕过去。
        Wake => false,
        // 传输进度每秒几千条,靠它驱动重绘就是 T3(风扇起飞)。画面由
        // `RedrawRequested` 里那段排期推进。
        TransferProgress { .. } => false,
        // ——— 豁免之二(理由完全不同):自己判脏 ———
        //
        // `EditTick` 的分支把 `self.ui_dirty` **当信号读**(「看门任务只在
        // 文件真的变了时才发这条,但『变了』不一定改动界面」)。在这里预先
        // 置真会让那个 `if self.ui_dirty` 恒成立 —— 编译过、测试过、语义
        // 静默变成「每次 tick 都重绘」。它的分支自成闭环,不需要这里帮忙。
        EditTick { .. } => false,
        // ——— 其余一律标脏 ———
        ConnectOk { .. }
        | ConnectErr(..)
        | KeyPathPicked(_)
        | CredentialKeyPathPicked(_)
        | IconPathPicked(_)
        | SshConfigPicked(_)
        | HostKeyPrompt(_)
        | PaneOpened { .. }
        | PaneOpenErr { .. }
        | PaneRehosted { .. }
        | PaneRehostErr { .. }
        | PaneReconnected { .. }
        | PaneReconnectErr { .. }
        | ProbeOk(_)
        | ProbeErr(_, _)
        | AutomationDone(_, _, _)
        | TunnelState { .. }
        | SftpOpened { .. }
        | SftpListed { .. }
        | OwnerNames { .. }
        | SftpOpDone { .. }
        | ShotUploaded { .. }
        | TransferPlanned { .. }
        | TransferDone { .. }
        | EditOpened { .. }
        | EditSaved { .. } => true,
    }
}

/// F125:`App::blink_on` 的核心判据抽成自由函数——只吃「窗口有没有焦点」和
/// 「距上次输入多少毫秒」,不碰 `&App`,理由同 `sync_timeout_wake_at`(`App`
/// 在无 GPU/窗口的环境下构造不出来,这几条分支只能靠这条路径单测)。
///
/// 隐式耦合:`window_focused == true` 但没有活跃终端(launcher 态,没有 pane
/// 要画)时,这个函数仍按相位交替返回 —— 调用方(`quads_for_panes`)那时压根
/// 没有光标要画,返回值不会被用到。复核已确认这不是 bug:「有没有活跃终端」
/// 故意不是这个函数的判据,那是 `blink_wake_at`(要不要为它排周期唤醒)的事,
/// 这里只管「这一刻该不该画」。
fn blink_on_at(window_focused: bool, elapsed_since_input_ms: u64) -> bool {
    if !window_focused {
        return true;
    }
    crate::frame::blink_visible(elapsed_since_input_ms, 0)
}

/// F125:`App::blink_wake` 的核心判据抽成自由函数,理由同 `blink_on_at`。
/// `has_active_terminal` 对应 `App::active_ws().is_some()`:没有活跃终端就没有
/// 光标要画,不必为闪烁排周期唤醒。返回值是「还要多少毫秒后翻转相位」,
/// `None` = 这一刻不需要排(窗口失焦 / 没有活跃终端)。
fn blink_wake_at(
    window_focused: bool,
    has_active_terminal: bool,
    elapsed_since_input_ms: u64,
) -> Option<u64> {
    if !window_focused || !has_active_terminal {
        return None;
    }
    Some(crate::frame::blink_next_flip_ms(elapsed_since_input_ms, 0))
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
        bookmarks,
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
            // F189:`update` **不碰**书签(它拿到的那份是编辑器打开那一刻的
            // 快照)。表单里那张表真被动过时,才由这一句整份写回去。
            if let Some(marks) = bookmarks {
                store
                    .set_bookmarks(id, marks)
                    .map_err(|e| format!("保存失败:{e}"))?;
            }
            store.save().map_err(|e| format!("保存失败:{e}"))?;
            Ok(id)
        }
        None => {
            // 新建这条路径上 `add` 就是整份写入,书签跟着 draft 进去,
            // 不需要(也没有)第二次写。
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
        || a.files_focus_click
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
        || a.history.is_some()
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
    // F53:在编辑的那些文件 + 内置编辑器窗口,拆开转给 `build_ui`。
    // 整个 `EditState` 收着传而不是摊成两个参数:这函数只能是自由函数
    // (`panes` 借着 `self` 的一部分,拿不到 `&mut self`),参数表因此是
    // `App` 字段的手工投影 —— 每加一项 edit 状态就得再加一个参数。
    edit: &mut EditState,
    // F125:这一帧光标该不该画出来(闪烁相位),调用方 `App::window_event` 算好了
    // 原样转给 `quads_for_panes`——算这个要读 `self.window_focused`/
    // `self.last_input_at`,那两个字段在这里(`Active` 而非 `App`)够不着。
    blink_on: bool,
) -> (std::time::Duration, crate::ui::UiActions) {
    diag::count_frame();
    // --- egui:每帧都跑,launcher 态(panes 为空)也要画菜单/状态栏。---
    diag::mark(diag::Stage::EguiRun);
    let raw_input = a.egui_state.take_egui_input(&a.window);
    // F157:这一帧喂了 egui 几个事件。egui 的 `wants_repaint_after` 只要本趟
    // `events` 非空就返回 `Duration::ZERO` —— 空闲时这个数本该是 0,不是 0
    // 就说明有人在往里灌事件,那正是「凭什么还在出帧」的答案。
    diag::note_egui_events(raw_input.events.len());
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
            &edit.sessions,
            &mut edit.editor,
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
    // F149:把 egui 的 IME 账本按住。egui-winit 的去抖是「账本 ≠ 目标值才发
    // `set_ime_allowed`」——egui 里没有文本框在组字的帧,它会发一次
    // `set_ime_allowed(false)`,关掉的是**整个窗口**的 IME。终端不是 egui
    // 部件,egui 永远不知道它也需要 IME,于是用户点过一次任意输入框再点回
    // 终端,中文输入就永久没了(没有自愈路径,只能重启 exe)。
    //
    // 把账本预先写成它这一帧本来要写的 `false`,去抖短路,那次调用发不出去,
    // 窗口保持 `resumed` 里设的常开。**必须排在 `handle_platform_output`
    // 之前** —— 排在后面等于什么都没做(它读的是调用当时的账本),而且照样
    // 编译、照样静默失灵,守护见 `the_ime_ledger_is_clamped_before_egui_...`。
    if let Some(v) = input::ime_ledger_clamp(full_output.platform_output.ime.is_some()) {
        a.egui_state.set_allow_ime(v);
    }
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
    // F157:egui 到底要不要下一帧。真空闲时本该恒是 `m:`(MAX);日志里
    // 每帧都是 `z:`/`f:` 就坐实了自激回路②。
    diag::note_repaint_delay(repaint_delay);

    // --- F159:整帧指纹。画出来跟上一帧一模一样就不提交 GPU。---
    //
    // **判在结果上,不判在原因上**(与 F12 的行指纹同一条推理,见 ADR-011):
    // 能改变「这一帧长什么样」的来源列举不完,漏一个的症状是屏幕留着陈旧的
    // 一帧,编译/测试/日志全静默。
    //
    // 截断点选在这里(tessellate 之后、终端趟之前):终端侧的全部输入
    // (行指纹来自快照、几何来自 `compute_geoms`、光标相位由调用方算好)
    // 在这个位置**已经全部就绪**,不需要先付 `text_prepare` 那几毫秒才知道
    // 结果没变。egui pass **照跑不跳** —— 它是指纹的真值来源,也是 tooltip /
    // 菜单动画能继续推进的前提(动画在推进 → 顶点变了 → 指纹不同 → 照常出帧)。
    let fp = crate::frame_fp::frame_fingerprint(
        &paint_jobs,
        panes,
        blink_on,
        a.text.style_key(),
        (a.gpu.config.width, a.gpu.config.height),
    );
    // egui 的纹理增量是**每帧 drain 出来、只交付一次**的,非空时一律强制
    // miss(理由见 `frame_fp::can_skip` 的文档)。
    let deltas_empty =
        full_output.textures_delta.set.is_empty() && full_output.textures_delta.free.is_empty();
    if crate::frame_fp::can_skip(a.last_frame_fp.as_ref(), &fp, deltas_empty) {
        diag::count_frame_fp(true);
        // 提前 return 即可,**什么都不用补**:`limiter.record_present` /
        // `ui_dirty = false` / `pacer.mark_presented` / 同步块收口 / 几何施加
        // 全在**调用方**(`App::window_event` 的 `Present` 分支)本函数返回
        // 之后无条件执行,现有的 surface Timeout / AtlasFull 提前 return
        // 也是被同一段兜住的。
        //
        // **这个判断不许挪到调用方侧**:挪出去就得手工重做上面每一笔记账;
        // 漏掉 `pacer.mark_presented` 一笔,`panes_ready_to_present` 恒真 →
        // `terminal_dirty` 恒真 → 每帧醒来算一次指纹,退化回 60fps 空转,
        // 而剖面里 `present` 反而是 0 —— 症状极具迷惑性。
        //
        // 返回**真实的** `repaint_delay`(不是别的提前 return 用的
        // `Duration::MAX`):egui 可能正在推进一段动画,那一路的排期不能被
        // 「这一帧画面没变」吃掉。
        //
        // 跳过整段文字层是安全的:F172 之后 `trim` 只在 `prepare_panes` 内部
        // 且只在全带重建的帧发生,本帧既然不 `prepare`,图集既不增长也不被清。
        return (repaint_delay, actions);
    }
    diag::count_frame_fp(false);

    // F172:trim 挪进了 `TextLayer::prepare_panes`,不再每帧无条件调。
    //
    // 原因是不变量变了:行带差分之后,一帧只重建**脏带**的顶点。`trim` 清空
    // `glyphs_in_use`,而只有本帧真的 prepare 过的带才会把自己用到的字形重新
    // 标回去 —— 干净带的字形就此失去保护,下一次图集淘汰会把它们扔掉,而那些
    // 带的顶点还指着旧坐标:**屏幕上画出别的字,不报错、不 panic**。
    // 判据(以及 `AtlasFull` 的自愈路径)收口在 `bands::may_trim` 与
    // `prepare_panes` 内部,这里不能再有第二个调用点。

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
        if let Err(e) = a.text.prepare_panes(
            &a.gpu.device,
            &a.gpu.queue,
            panes,
            res,
            theme::term_default_colors(&MULLION_DARK).fg,
        ) {
            log::warn!(target: "mullion", "glyphon prepare 失败,跳过本帧: {e:?}");
            diag::count_skipped();
            return (std::time::Duration::MAX, actions);
        }
        // F193:填进常驻实例缓冲,只把实例数带出去 —— 缓冲归 `Gpu` 自己持有。
        Some(a.gpu.upload_quads(&quads))
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
            // F159:重新 configure 之后交换链内容未定义,旧基准作废 ——
            // 留着的话下一帧会误判命中,画面停在更早的一帧上。
            a.last_frame_fp = None;
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

    // F165:GPU 帧耗时抽样。上一次回读还没回来就跳过本帧(传 None,零开销)。
    let ts_writes = a.gpu.gpu_timer.as_ref().and_then(|t| t.writes());
    let sampling = ts_writes.is_some();

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
            timestamp_writes: ts_writes,
            occlusion_query_set: None,
        });
        if let Some(n) = &terminal_draw {
            a.gpu.draw_quads(&mut pass, *n); // 背景趟
                                             // 前景趟:失败(如条目在 prepare 之后被图集淘汰)不 panic,记录并跳过文字层,
                                             // 背景色块这帧仍照常提交。
            if let Err(e) = a.text.render(&mut pass) {
                log::warn!(target: "mullion", "glyphon render 失败,跳过本帧文字层: {e:?}");
            }
        }
        // F170:终端趟/egui 趟的分界时间戳。必须在 forget_lifetime 之前;放在
        // terminal_draw 判空块之外——launcher 态(没有终端可画)那些帧也要打
        // 这个分界点,否则那些帧的 t1 是垃圾值(残留的上一次采样)。
        if sampling {
            if let Some(t) = a.gpu.gpu_timer.as_ref() {
                t.mid_mark(&mut pass);
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

    // resolve 必须在 submit 之前录进同一个 encoder。
    if sampling {
        if let Some(t) = a.gpu.gpu_timer.as_ref() {
            t.resolve(&mut enc);
        }
    }

    a.gpu
        .queue
        .submit(egui_cmds.into_iter().chain(std::iter::once(enc.finish())));

    // 回读在 submit 之后发起:map_async 要等 GPU 跑完这批命令。
    if sampling {
        if let Some(t) = a.gpu.gpu_timer.as_ref() {
            t.read_back();
        }
    }
    // present 在 Fifo 下会等 vsync;它和上面的 acquire 是最可能长阻塞的两步,
    // 分开打点才能区分「等交换链」和「等驱动」。
    diag::mark(diag::Stage::Present);
    frame.present();
    diag::count_present();
    for id in &full_output.textures_delta.free {
        a.egui_renderer.free_texture(id);
    }

    // F159:只有**真正提交过**的帧才成为下一帧的比对基准。任何提前 return
    // (prepare 失败 / acquire 失败 / surface 重配)都不更新它 —— 那些帧
    // 没画出去,拿它们当基准会让下一帧误判命中,屏幕停在更早的一帧上。
    a.last_frame_fp = Some(fp);

    (repaint_delay, actions)
}

#[cfg(test)]
mod tests {
    use super::{
        apply_credential_save, apply_import, apply_layout_actions, apply_save, apply_tab_props,
        attach_check_verdict, auto_dial_summary, automation_for_leaf, autoscroll_for_pane,
        blink_on_at, blink_wake_at, clear_leaf_attach_intent, credential_delete_error,
        download_job, drive_attach_checks_of, effective_focus_of, expand_tilde,
        files_owner_generation_of, files_path_editing_of, files_start_dir, finish_password_change,
        font_px_for, has_real_action, ime_cursor_area, ime_goes_to_terminal_of, leaf_identity_of,
        new_pane_emulator, next_auto_dial, next_panel_selection_index, pane_still_wanted,
        place_dead_pane_of, reattach_pane, rehost_pane, resolved_scrollback, should_check_attach,
        snapshot_tabs_of, sync_plan_of, sync_timeout_wake_at, tab_keeps_template, tab_title,
        take_next_restore_dial, tmux_attach_for_connect, upload_job, user_event_marks_dirty,
        wind_down, AttachCheck, AttachVerdict, Modal, RehostKind, RestoredTab, SyncPlan, Tab,
        TabContent, TerminalTab, TmuxAttach, UserEvent,
    };
    use crate::frame::FrameLimiter;
    use crate::reflow::{reflow, ResizeSink};
    use crate::shell::tabs::{TabId, Tabs};
    use crate::shell::workspace::{Preset, Workspace};
    use mullion_core::layout::{Dir, Node, PaneId, Rect};
    use mullion_store::SessionId;
    use std::sync::Arc;

    // ------------------------------------------------ F155 剖面接线

    /// F155:回显往返靠「按键时刻」与「下一段入站字节」配对,两个点缺一
    /// 不可。只接前者的话回显永远采不到样本(剖面行里恒为 `echo=0x`);
    /// 只接后者的话它永远没有配对的起点。吞吐同理 —— `count_inbound`
    /// 不接的话剖面里 `in=` 恒为 `0B/s`,看着像远端一个字节都没发过来。
    ///
    /// **只搜 `mod tests` 之前的那一段**:needle 本身就写在下面这个数组里,
    /// 整份文件搜的话每一条都恒真,这测试就成了摆设。
    ///
    /// 自证会变红:删掉任意一句接线。
    #[test]
    fn the_input_and_throughput_hooks_are_wired() {
        let app_src = include_str!("app.rs");
        let prod = app_src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .unwrap_or(app_src);
        // `split` 找不到模式时原样返回整份 haystack,`.next()` 永远是 `Some`,
        // 光靠 `expect` 兜不住 —— 切不干净就会搜到这条测试自己的文本。
        assert!(
            prod.len() < app_src.len(),
            "没能切掉测试模块 —— 下面每条断言都会恒真"
        );
        assert!(
            prod.contains("diag::note_key()"),
            "按键那一端没接 —— 回显永远采不到样本,剖面行里恒为 echo=0x"
        );
        let pump_src = include_str!("session_pump.rs");
        assert!(
            pump_src.contains("note_inbound_for_echo()")
                || prod.contains("diag::note_inbound_for_echo()"),
            "入站那一端没接 —— 回显往返永远配不上对"
        );
        // F173:计数点从 `session_pump.rs` 搬到了 `Workspace::pump` —— 它现在
        // 要按 pane 归因,而 `PaneId` 是那个纯件不认识的东西。**搜的文件跟着
        // 搬**:留在原地的话这条断言看不到新调用点,删掉接线也不会红
        // (「源码切片测试与搬运天然冲突」,F153 那次踩过)。
        //
        // 判据带上第一个实参而不是裸 `count_inbound(`:裸前缀连
        // 「per-pane 那一半被摘掉、退回全局单参」都拦不住。
        let ws_src = include_str!("shell/workspace/mod.rs");
        assert!(
            ws_src.contains("diag::count_inbound(p.id.0,"),
            "入站字节没按 pane 计数 —— 剖面里吞吐恒为 0B/s、profile.pane 恒为空,\
             看着像远端一个字节都没发过来"
        );
    }

    /// F155:连接成败、重连、SFTP 要进剖面。高延迟代理链路上「这一小时
    /// 重连了 17 次」是最直接的线索,而它在今天的日志里只能靠人肉数 WARN 行。
    ///
    /// 自证会变红:删掉任意一句接线。
    #[test]
    fn connection_outcomes_are_counted_for_the_profile() {
        let src = include_str!("app.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .unwrap_or(src);
        assert!(prod.len() < src.len(), "没能切掉测试模块 —— 断言会恒真");
        for (needle, what) in [
            ("diag::count_connect(true)", "连接成功"),
            ("diag::count_connect(false)", "连接失败"),
            ("diag::count_reconnect()", "重连"),
        ] {
            assert!(prod.contains(needle), "{what}没计数 —— 剖面里那一列恒为零");
        }
    }

    /// F155:**两类** SFTP 操作都要计数 —— 目录操作(mkdir/rm/rename)与
    /// 传输完成。
    ///
    /// 按分支各扎一次,而不是在整份源码里搜一次 `count_sftp_op()`:后者
    /// 在有两个接线点时,删掉其中任意一个都仍然搜得到,断言恒绿。这不是
    /// 假设 —— 本切片实现时正是先漏了这一点,才把传输那一路的接线又拿掉了。
    ///
    /// `TransferProgress` **绝不能**计数:一个 100MB 的文件几千条,计进去
    /// 剖面里的 SFTP 列就变成了进度条的采样数,毫无意义(同 arm 的 T3 守护)。
    ///
    /// 自证会变红:删掉两条分支里的任意一句 `diag::count_sftp_op();`。
    #[test]
    fn both_kinds_of_sftp_operations_are_counted() {
        let src = include_str!("app.rs");
        let (production, _) = src
            .split_once("\n#[cfg(test)]\nmod tests {")
            .expect("app.rs 的测试模块分界变了,这条测试的锚点失效了");
        for (pattern, what) in [
            ("UserEvent::SftpOpDone { generation, result }", "目录操作"),
            ("UserEvent::TransferDone { job, result }", "传输完成"),
        ] {
            let arm = arm_of(production, pattern);
            assert!(
                arm.contains("diag::count_sftp_op();"),
                "{what}那一路没计数 —— 剖面里的 SFTP 列会少算一半"
            );
        }
        let progress = arm_of(production, "UserEvent::TransferProgress { job, done }");
        assert!(
            !progress.contains("count_sftp_op"),
            "进度事件被计成了 SFTP 操作 —— 一个大文件几千条,这一列就废了"
        );
    }

    // ------------------------------------------------ F18 划选自动滚动

    /// F185:右键粘贴前必须先把焦点切到指针底下那块 pane,**且顺序不能反**。
    ///
    /// 改这条之前右键那一支压根没切焦点:分屏下在 pane 2 上右键,内容贴进
    /// 上一次有焦点的 pane 1。没有报错、没有日志,只有字出现在错的地方。
    /// 顺序反过来同样坏 —— `request_paste` 的落点取 `effective_focus()`,
    /// 先贴再切焦点等于一次都没修。
    ///
    /// 源码切片是因为整条路在 `WindowEvent::MouseInput` 里,要真窗口才发得出。
    /// **必须先剥注释行**:上面这段说明和分支里的注释都写着这两个函数名,
    /// 不剥的话断言拿自己的解释当证据,把两句调用全删掉照样绿。
    ///
    /// 自证会变红:删掉右键分支里的 `self.focus_pane_under_cursor();`,
    /// 或把它挪到 `self.request_paste();` 之后。
    #[test]
    fn a_right_click_takes_focus_before_it_pastes_so_the_text_lands_where_you_clicked() {
        let src = include_str!("app.rs");
        let (production, _) = src
            .split_once("\n#[cfg(test)]\nmod tests {")
            .expect("app.rs 的测试模块分界变了,这条测试的锚点失效了");
        let arm = arm_of(production, "(MouseButton::Right, ElementState::Pressed)");
        let code = arm
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let focus = code
            .find("self.focus_pane_under_cursor();")
            .expect("右键没切焦点 —— 分屏下会把剪贴板贴进上一块 pane");
        let paste = code.find("self.request_paste();").expect("右键不粘贴了?");
        assert!(
            focus < paste,
            "切焦点排在粘贴之后 —— 落点取的是切之前的 effective_focus(),等于没修"
        );
    }

    /// F199:点终端 pane,键盘焦点要从文件面板**抢回来**。
    ///
    /// 在这之前 `self.focus` 只有 F6 改得动。用户点一下侧栏(F199 的另一半会把
    /// 焦点给面板)再回头点终端接着打字,如果这里不抢回来,他敲的每一个字都
    /// 进了面板的按键处理 —— 远端一个字都收不到,而画面上光标还在闪。
    ///
    /// 源码切片:整条路在 `WindowEvent::MouseInput` 里,要真窗口才发得出;
    /// `App` 在无头环境下也造不出来。**先剥注释行**,否则上面这段说明本身
    /// 就能让断言通过(本仓库记过的恒绿模式)。
    ///
    /// 自证会变红:把 `focus_pane_under_cursor` 里那句赋值删掉。
    #[test]
    fn clicking_a_pane_takes_the_keyboard_focus_back_from_the_files_panel() {
        let src = include_str!("app.rs");
        let (production, _) = src
            .split_once("\n#[cfg(test)]\nmod tests {")
            .expect("app.rs 的测试模块分界变了,这条测试的锚点失效了");
        let after = production
            .split("fn focus_pane_under_cursor(&mut self) {")
            .nth(1)
            .expect("找不到 focus_pane_under_cursor");
        let body = &after[..after.find("\n    }\n").expect("找不到函数结尾")];
        let code = body
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            code.contains("self.focus = shell::input_route::Focus::Terminal;"),
            "点 pane 没把焦点抢回终端 —— 点过一次文件面板之后,打字再也到不了远端"
        );
    }

    /// F199:面板上的一次点击必须真的被消费掉,并且不能在 discard 趟被吃掉。
    ///
    /// 三处缺一不可:UI 侧报出来、`app.rs` 收下改 `self.focus`、
    /// `has_real_action` 认得它。少最后一条的症状最刁 —— 「有时候点了没用」。
    ///
    /// 自证会变红:删掉 `if actions.files_focus_click {` 那一段,
    /// 或删掉 `has_real_action` 里对应那一行。
    #[test]
    fn a_click_in_the_files_panel_moves_the_keyboard_focus_onto_it() {
        let src = include_str!("app.rs");
        let (production, _) = src
            .split_once("\n#[cfg(test)]\nmod tests {")
            .expect("app.rs 的测试模块分界变了,这条测试的锚点失效了");
        let code = production
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let at = code
            .find("if actions.files_focus_click {")
            .expect("面板上的点击没人收 —— F5/F2/Del 永远轮不到文件面板");
        assert!(
            code[at..at + 200].contains("self.focus = shell::input_route::Focus::FilesPanel;"),
            "收下了但没改焦点"
        );
        let after = code
            .split("fn has_real_action(")
            .nth(1)
            .expect("找不到 has_real_action");
        let body = &after[..after.find("\n}\n").expect("找不到 has_real_action 的结尾")];
        assert!(
            body.contains("a.files_focus_click"),
            "切焦点会在 egui 的 discard 趟被静默吃掉 —— 表现为「有时候点了没用」"
        );
    }

    /// **接线守护 / F200**:就地改名的输入框必须算模态(T8)。
    ///
    /// 不算的话它**一个字都收不到** —— 面板拿着键盘焦点时键根本不喂给
    /// egui(`input_route::egui_should_see_focused`),而 Backspace 还会被
    /// `handle_panel_key` 解释成「回上级目录」:用户按 F2、看见框亮起来、
    /// 打字没反应、退格直接跳走了。同 `Modal::Editor`/`FilesPathEdit` 的坑。
    ///
    /// 自证会变红:把 `Modal::FilesRename` 从 `Modal::ALL` 里删掉,
    /// 或把它并进别的臂。
    #[test]
    fn the_in_place_rename_box_counts_as_a_modal_so_it_can_receive_keys() {
        assert!(
            Modal::ALL.contains(&Modal::FilesRename),
            "FilesRename 没登记进 Modal::ALL(T8)"
        );
        let src = include_str!("app.rs");
        let after = src
            .split("fn modal_open(&self) -> bool {")
            .nth(1)
            .expect("找不到 modal_open");
        let body = &after[..after.find("\n    }\n").expect("找不到 modal_open 的结尾")];
        assert!(
            body.contains("Modal::FilesRename => self.files_renaming()"),
            "modal_open 里没有 FilesRename 独立的那一臂(T8)"
        );
    }

    /// F200:F2 / 右键「重命名」**不再弹对话框**,而是让那一行进编辑态。
    ///
    /// 「就在 SFTP 里改名」是这条需求的全部内容 —— 走回对话框等于没做。
    ///
    /// 自证会变红:把 `FileAsk::Rename` 那一臂换回构造 `FilesDialog`。
    #[test]
    fn asking_to_rename_starts_an_in_place_edit_instead_of_a_dialog() {
        let src = include_str!("app.rs");
        let (production, _) = src
            .split_once("\n#[cfg(test)]\nmod tests {")
            .expect("app.rs 的测试模块分界变了,这条测试的锚点失效了");
        let after = production
            .split("fn open_files_dialog(")
            .nth(1)
            .expect("找不到 open_files_dialog");
        let body = &after[..after.find("\n    }\n").expect("找不到函数结尾")];
        let code = body
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let at = code.find("FileAsk::Rename =>").expect("重命名那一臂没了");
        assert!(
            code[at..at + 300].contains("begin_rename()"),
            "重命名还在走对话框那条路"
        );
        assert!(
            !code.contains("FilesDialog::Rename"),
            "改名对话框还在 —— 两条入口并存,用户按 F2 会看见弹框"
        );
    }

    /// F202:`Shift+Delete` 跳过确认框直接删,裸 `Delete` **仍然要弹框**。
    ///
    /// 这是设计 D17(远端删除不可逆、必须确认)唯一的例外,所以两条路必须在
    /// 同一个 `match` 臂里显式分叉、看得见:哪天有人把 `mods` 那个判断顺手
    /// 删掉,后果是**裸 Delete 也不弹框了** —— 一个手滑就没了整棵目录,
    /// 而界面上什么都不会变,没有任何东西提示判据丢了。
    ///
    /// 免确认那条路还必须报一句带计数的吐司:用户没看见确认框,这是唯一
    /// 能让他知道刚才打中了什么的东西。
    ///
    /// 自证会变红:把 `mods.shift_key()` 改成 `false`(免确认路死掉),
    /// 或改成 `true`(裸 Delete 也不弹框了),或删掉那句 `set_toast`。
    #[test]
    fn shift_delete_skips_the_confirmation_but_a_bare_delete_still_asks() {
        let src = include_str!("app.rs");
        let (production, _) = src
            .split_once("\n#[cfg(test)]\nmod tests {")
            .expect("app.rs 的测试模块分界变了,这条测试的锚点失效了");
        let after = production
            .split("fn handle_panel_key(")
            .nth(1)
            .expect("找不到 handle_panel_key");
        let body = &after[..after.find("\n    }\n").expect("找不到函数结尾")];
        let code = body
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let at = code
            .find("mods.shift_key()")
            .expect("Delete 没按 Shift 分叉 —— 要么免确认路没接,要么裸 Delete 也不弹框了");
        let arm = &code[at..];
        assert!(
            arm.contains("delete_targets()"),
            "免确认那条路没用与确认框同源的目标算法,两边会删得不一样"
        );
        assert!(
            arm.contains("deleting_toast"),
            "免确认删完一声不吭 —— 用户没看见确认框,不知道刚才打中了什么"
        );
        assert!(
            arm.contains("FileAsk::Delete"),
            "Shift 分叉之后裸 Delete 那条腿丢了 —— 手滑一下就没了整棵目录"
        );
    }

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
    /// 或者把 `mark_ui_dirty!(self.ui_dirty);` 删掉。
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
            body.contains("mark_ui_dirty!(self.ui_dirty);"),
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

    /// 同上,换节点那两份。被 discard 趟吃掉的现象分别是「点换节点按钮不弹窗」和
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
        assert!(has_real_action(&b), "点换节点按钮的那一下被 discard 趟吞了");
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
                shell_osc7_bootstrap: true,
                log_level: mullion_store::LogLevel::Info,
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
    /// 自证会变红:把 `modal_open` 里 `self.edit.editor.is_some()` /
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
            body.contains("self.edit.editor.is_some()"),
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
                Modal::FilesPathEdit => assert!(
                    Modal::ALL.contains(&Modal::FilesPathEdit),
                    "FilesPathEdit 没登记进 Modal::ALL(T8/F131)"
                ),
                Modal::FilesRename => assert!(
                    Modal::ALL.contains(&Modal::FilesRename),
                    "FilesRename 没登记进 Modal::ALL(T8/F200)"
                ),
                Modal::History => assert!(
                    Modal::ALL.contains(&Modal::History),
                    "History 没登记进 Modal::ALL(T8/F148)"
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
            // 补漏:这一项过去缺席 —— `match` 有它那一臂,但循环从不喂它,
            // 那条 `assert!` 是死代码(F148 复核顺带发现)。
            Modal::Rehost,
            Modal::FilesPathEdit,
            Modal::FilesRename,
            Modal::History,
        ] {
            check(m);
        }
        // 去重后仍是同一个数 —— 防「复制粘贴写重了一项来凑数」。
        let mut seen = std::collections::HashSet::new();
        for m in Modal::ALL {
            assert!(seen.insert(format!("{m:?}")), "Modal::ALL 里有重复项:{m:?}");
        }
    }

    /// **接线守护 / F148**:恢复列表弹窗必须算模态(T8)。
    ///
    /// 不算的话,它开着的时候 `Ctrl+W` 仍能关掉背后的标签、方向键仍被判给
    /// 终端 —— 而这个弹窗是启动后用户看到的第一样东西。
    #[test]
    fn the_history_dialog_counts_as_a_modal_so_it_does_not_share_the_keyboard() {
        assert!(
            Modal::ALL.contains(&Modal::History),
            "History 没登记进 Modal::ALL(T8)"
        );
        let src = include_str!("app.rs");
        let after = src
            .split("fn modal_open(&self) -> bool {")
            .nth(1)
            .expect("找不到 modal_open");
        let body = &after[..after.find("\n    }\n").expect("找不到 modal_open 的结尾")];
        // **不能只查 `"Modal::History =>"` 这个子串**:把 History 并进别的臂
        // (如 `Modal::FilesPathEdit | Modal::History => self.files_path_editing(),`)
        // 时,这个子串原样躺在合并后的那一行里,断言会假绿。查完整那一臂,
        // 判据落在「History 真的映射到 `self.ui.history.is_some()`」上。
        assert!(
            body.contains("Modal::History => self.ui.history.is_some()"),
            "modal_open 里没有 History 独立的那一臂(T8)"
        );
    }

    /// **接线守护 / F148**:弹窗的结论必须进 `has_real_action`。
    ///
    /// 漏了的话,「恢复」按下去会在 egui 的 discard 趟被静默吃掉 —— 而这个
    /// 弹窗是启动后唯一能操作的东西,用户只能去杀进程。
    ///
    /// 自证会变红:把 `has_real_action` 里的 `|| a.history.is_some()` 删掉。
    #[test]
    fn the_history_dialog_action_is_not_swallowed_by_the_discard_pass() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn has_real_action(")
            .nth(1)
            .expect("找不到 has_real_action");
        let body = &after[..after.find("\n}\n").expect("找不到 has_real_action 的结尾")];
        assert!(
            body.contains("a.history.is_some()"),
            "恢复列表的结论会在 discard 趟被静默吃掉"
        );
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

    /// F187:老库里各会话名下的本地书签,迁移必须挂在**会话库打开之后**,
    /// 而且**库没打开就一步都不做**。
    ///
    /// 两条各有一种坏法,都静默:
    /// - 挂在 `resumed` 里 → 那时 `self.store` 还是 `None`(主密码那条路上更晚),
    ///   一条老书签都读不到;
    /// - 库打不开(密码错、文件损坏)时照样置上「已迁移」标记并存盘 →
    ///   用户手上那份老收藏从此再没人看一眼,而这正是本次要修的那类丢数据。
    ///
    /// 源码切片:这两条都要一个真的 `App` + 一个真的会话库才跑得起来。
    /// **先剥注释行** —— 上面这段和函数里的说明都写着这几个标识符。
    ///
    /// 自证会变红:把 `finish_store_open` 里那句迁移调用删掉;或把
    /// `migrate_local_bookmarks_into_settings` 里的 `let Some(store)` 早退
    /// 挪到 `merge_local_bookmarks` 之后。
    #[test]
    fn the_local_bookmark_migration_waits_for_the_store_and_gives_up_if_it_never_opened() {
        let src = include_str!("app.rs");
        let (production, _) = src
            .split_once("\n#[cfg(test)]\nmod tests {")
            .expect("app.rs 的测试模块分界变了,这条测试的锚点失效了");
        let strip = |s: &str| {
            s.lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let opened = strip(body_of(production, "fn finish_store_open("));
        assert!(
            opened.contains("self.migrate_local_bookmarks_into_settings();"),
            "迁移没挂在会话库打开之后 —— 老书签一条都读不到,而标记会照置"
        );

        let mig = strip(body_of(
            production,
            "fn migrate_local_bookmarks_into_settings(",
        ));
        let guard = mig
            .find("let Some(store) = self.store.as_ref() else {")
            .expect("库没打开时必须一步都不做");
        let merge = mig
            .find("merge_local_bookmarks(")
            .expect("迁移没调 merge_local_bookmarks —— 那它什么都没干");
        assert!(
            guard < merge,
            "「库没打开」的早退排在合并之后 —— 库打不开时会置上已迁移标记,\
             用户那份老收藏从此没人再看一眼"
        );
    }

    /// F187:全局收藏夹的推送必须遍历**所有**标签,不能只推活动那个。
    ///
    /// 同 `drive_reconnects_walks_every_tab_not_just_the_active_one` 那条纪律。
    /// 只推活动标签的症状:在标签 1 收了个目录,切到标签 2 一看没有 —— 要
    /// 重开标签才出现。收藏夹是全局的,这在用户眼里就是「☆ 时灵时不灵」。
    ///
    /// **补的是一个实测漏掉的口子**:上一条(`bookmarking_writes_…`)只钉了
    /// 「add/remove 里调了 sync」,把 `sync` 的函数体本身换成只改活动标签之后
    /// `cargo test --workspace` 全绿。调用点和实现各扎一次。
    ///
    /// 自证会变红:把 `sync_local_bookmarks_to_tabs` 里的 `self.tabs.iter_mut()`
    /// 换成 `self.tabs.active_mut()`。
    #[test]
    fn the_global_bookmark_list_is_pushed_to_every_tab_not_just_the_active_one() {
        let src = include_str!("app.rs");
        let (production, _) = src
            .split_once("\n#[cfg(test)]\nmod tests {")
            .expect("app.rs 的测试模块分界变了,这条测试的锚点失效了");
        let body = body_of(production, "fn sync_local_bookmarks_to_tabs(");
        assert!(
            body.contains("self.tabs.iter_mut()"),
            "没有遍历全部标签 —— 在标签 1 收的目录,标签 2 要重开才看得见"
        );
        assert!(
            !body.contains("active"),
            "只推了活动标签 —— 收藏夹是全局的,用户会觉得 ☆ 时灵时不灵"
        );
    }

    /// F139/F187:☆ 收藏必须**当场存盘**,而且两栏各存各的地方。
    ///
    /// 漏掉存盘的症状是「收藏了、星星也变实心了,关掉客户端再开就没了」——
    /// 全程零报错,只有重启才发现。F187 之后这件事有**两条**路径:远端书签
    /// 走会话库(`store.save()`),本地书签走全局设置(`save_settings()`)。
    /// 只钉其中一条的话,另一条整个删掉照样绿。
    ///
    /// 跟 `touched_store` 那几条同款扎源码:这两个方法要一个真的 `App` 才调
    /// 得动,而 `App` 在无头环境里造不出来。切片必须先切到函数体内 ——
    /// `include_str!("app.rs")` 读的是同一个文件,本测试自己也含
    /// `store.save()` 这个串,不缩范围就永远绿(第五类恒绿模式)。
    ///
    /// 自证会变红:把 `add_bookmark` 里的 `.and_then(|_| store.save())` 删掉;
    /// 或把本地那一支的 `self.save_settings()` 删掉;或把
    /// `sync_local_bookmarks_to_tabs()` 换成只改当前标签。
    #[test]
    fn bookmarking_writes_through_to_disk_immediately() {
        let src = include_str!("app.rs");
        // **切片键不带 `&mut self`**:F154 给这两个方法加了参数,rustfmt 随即
        // 把签名折成多行,`fn add_bookmark(&mut self` 这个串在真正的定义处
        // 不再出现 —— 只剩本测试自己那份数组字面量,于是 `split` 切到的是
        // 测试自己的源码,整条测试恒绿(第五类恒绿模式,当场实测复现过)。
        for f in ["fn add_bookmark(", "fn remove_bookmark("] {
            let after = src.split(f).nth(1).unwrap_or_else(|| panic!("找不到 {f}"));
            // 到下一个方法定义为止 = 这一个函数的函数体。
            let body = &after[..after.find("\n    fn ").expect("找不到该函数的结尾")];
            assert!(
                body.contains("by_generation(generation)"),
                "{f} 的函数体切歪了 —— 下面那条断言会空过"
            );
            assert!(
                body.contains("store.save()"),
                "{f} 的远端那一支只改了内存没存盘:收藏在重启后消失(F139)"
            );
            // F154/F187:两栏各有一条持久化路径和一份帧内镜像,漏掉任何
            // 一条的症状都是「某一栏的 ☆ 点了没反应 / 重启后没了」,不报错。
            assert!(
                body.contains("PanelColumn::Local"),
                "{f} 没有按栏分流 —— 本地栏的收藏会写进远端那份列表"
            );
            assert!(
                body.contains("self.save_settings()"),
                "{f} 的本地那一支没存 settings.toml —— 本地收藏重启后消失(F187)"
            );
            assert!(
                body.contains("self.sync_local_bookmarks_to_tabs()"),
                "{f} 没把全局收藏夹推给其余标签 —— 在标签 1 收的目录,\
                 标签 2 要重开才看得见(F187)"
            );
        }
    }

    /// F154 接线守护:本地栏收到书签动作要**真处理**,不是记一条 warn 扔掉。
    ///
    /// 自证会变红:把 `apply_local_file_action` 里那两条分支改回
    /// `log::warn!("本地栏收到了书签动作,已忽略(书签只属于远端栏)")`。
    #[test]
    fn the_local_column_actually_stores_its_bookmarks() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn apply_local_file_action")
            .nth(1)
            .expect("找不到 apply_local_file_action");
        let body = &after[..after.find("\n    fn ").expect("找不到该函数的结尾")];
        assert!(
            body.contains("self.add_bookmark(") && body.contains("self.remove_bookmark("),
            "本地栏没接上书签落盘 —— ☆ 点了什么都不会发生"
        );
        assert!(
            !body.contains("书签只属于远端栏"),
            "本地栏还在把书签动作当接线错误扔掉(F154 已经把它接上了)"
        );
    }

    /// F121:拖拽排序落盘走的也是 keyring/TOML 同步 IO,跟删除/保存/移动分组
    /// 同一条门槛——漏算的话看门狗测不出这条路径可能阻塞事件循环。
    ///
    /// **锚点是先找 `if`、再从那之后找 `diag::mark`**(不是反过来):F155 在
    /// `drain_export_log_request` 里加了另一处合法的
    /// `diag::mark(diag::Stage::StoreIo);`,且它在源码里排在这个 `if` 之前 ——
    /// 反过来找(先 `find` mark 再 `rfind` if)会截到那一处不相关的打点。
    ///
    /// 自证会变红:把 `diag::mark(diag::Stage::StoreIo)` 前面那个 `if` 里的
    /// `self.ui.reorder_request.is_some()` 删掉。
    #[test]
    fn dragging_a_session_is_marked_as_store_io_for_the_watchdog() {
        let src = include_str!("app.rs");
        let start = src
            .find("if self.ui.delete_request.is_some()")
            .expect("找不到打点前面那个 if 的开头");
        let after = &src[start..];
        let idx = after
            .find("diag::mark(diag::Stage::StoreIo);")
            .expect("找不到 StoreIo 打点");
        let cond = &after[..idx];
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
        fn close(&self) {}
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
            history_reported: 0,
            host_pending: false,
            notice: None,
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

    /// F125:窗口失焦时,光标判据必须两件事一起做到——恒画(不因暗周期消失)
    /// 且不再为闪烁排周期唤醒(不需要,反正恒画;排了也只是空转)。
    ///
    /// 自证会变红:把 `blink_on_at` 里 `if !window_focused { return true; }`
    /// 这条短路删掉(改回直接按相位算),或者把 `blink_wake_at` 里
    /// `!window_focused` 那半个条件删掉。
    #[test]
    fn unfocused_window_shows_a_steady_cursor_and_never_wakes_for_it() {
        // elapsed 故意落在暗半周期(530..1060ms 那一段)——聚焦状态下这一刻
        // `blink_on_at` 该是 false,拿它来反证「失焦短路」确实生效,而不是
        // 巧合落在了亮半周期。
        let dark_elapsed = 700;
        assert!(
            !crate::frame::blink_visible(dark_elapsed, 0),
            "测试前提:这个 elapsed 在聚焦状态下必须是暗半周期,否则下面的断言测不出短路"
        );
        assert!(
            blink_on_at(false, dark_elapsed),
            "失焦时光标必须恒亮,不能被相位算成暗"
        );
        assert_eq!(
            blink_wake_at(false, true, dark_elapsed),
            None,
            "失焦时不该为闪烁排唤醒——反正恒亮,排了也是空转"
        );
    }

    /// F125:有焦点但没有活跃终端(launcher 态,没有 pane 可画光标)时,不该为
    /// 闪烁排周期唤醒——排了是纯粹的空转,没有任何东西会因为它而改变。
    ///
    /// 自证会变红:把 `blink_wake_at` 里 `|| !has_active_terminal` 那半个条件
    /// 删掉。
    #[test]
    fn focused_without_a_terminal_never_wakes_for_blink() {
        assert_eq!(
            blink_wake_at(true, false, 0),
            None,
            "没有活跃终端就没有光标要画,不该排唤醒"
        );
    }

    /// F125:有焦点且有活跃终端时,`blink_on_at`/`blink_wake_at` 必须原样转发
    /// `crate::frame` 那两个纯函数的判据——相位算法本身在 `frame::tests` 里已经
    /// 单测过,这里只验证「转发没转错参数」。
    ///
    /// 自证会变红:把 `blink_on_at`/`blink_wake_at` 里传给
    /// `crate::frame::blink_visible`/`blink_next_flip_ms` 的参数改错(比如都
    /// 换成常量 `0`)。
    #[test]
    fn focused_with_a_terminal_follows_the_blink_phase() {
        for elapsed in [0, 100, 529, 530, 800, 1059, 1060] {
            assert_eq!(
                blink_on_at(true, elapsed),
                crate::frame::blink_visible(elapsed, 0),
                "elapsed={elapsed}:画不画必须原样转发相位算法"
            );
        }
        let ms = blink_wake_at(true, true, 300).expect("有焦点有终端必须排唤醒");
        assert_eq!(
            ms,
            crate::frame::blink_next_flip_ms(300, 0),
            "唤醒时刻必须原样转发相位算法"
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
            rehost_pane(
                &mut ws,
                PaneId(1),
                0,
                3,
                Box::new(NullPty),
                rx,
                mullion_term::emulator::Emulator::DEFAULT_HISTORY,
                RehostKind::UserPicked,
            ),
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
            !rehost_pane(
                &mut ws,
                PaneId(1),
                0,
                3,
                Box::new(NullPty),
                rx,
                mullion_term::emulator::Emulator::DEFAULT_HISTORY,
                RehostKind::UserPicked,
            ),
            "这是上一个世代发起的换节点,必须拒绝"
        );
        assert_eq!(
            ws.pane(PaneId(1)).map(|p| p.host_ix),
            Some(0),
            "被拒绝时不该动 pane 的任何字段"
        );
    }

    /// F128:重连换的是 channel,**不是机器** —— 屏上内容必须原样留着
    /// (用户拍板:保留旧屏内容)。重建 emulator 的话,断线前那一屏
    /// (往往正是他想看的报错)当场消失。
    ///
    /// 自证会变红:把 `reattach_pane` 改成调 `rehost_pane`。
    #[test]
    fn reattach_keeps_the_screen_but_rehost_wipes_it() {
        use crate::shell::workspace::tests_support::{fresh_pipe, ws_with};
        let (mut ws, _p) = ws_with(1);
        let gen = ws.generation();
        let p = ws.pane_mut(PaneId(1)).unwrap();
        p.emulator.feed(b"before-the-drop");
        // F128:**起始状态必须是 `Reconnecting`** —— 起始就是 `Live` 的话,
        // 下面那条 `status == Live` 的断言把 `swap_pane_channel` 里的
        // `p.status = Live` 删掉也照样绿(终态碰巧没变),等于没守护。
        // 而这一条恰恰是重连最关键的一步:不置回 Live,重连成功了标题条
        // 还一直黄着、Ctrl+D 还按「断开」那套语义走。
        p.status = crate::shell::workspace::PaneStatus::Reconnecting;
        let (pty, rx) = fresh_pipe();
        assert!(reattach_pane(&mut ws, PaneId(1), gen, 0, pty, rx));
        let text: String = ws
            .pane(PaneId(1))
            .unwrap()
            .emulator
            .snapshot()
            .cells
            .iter()
            .map(|c| c.ch)
            .collect();
        assert!(
            text.contains("before-the-drop"),
            "重连不该抹掉断线前的内容,实际:{text:?}"
        );
        assert_eq!(
            ws.pane(PaneId(1)).unwrap().status,
            crate::shell::workspace::PaneStatus::Live
        );

        // 对照组:换机器那条路必须**抹掉**内容(否则上一台机器的输出会
        // 挂在新机器的屏上,用户完全分不清哪些字是谁说的)。
        let (pty, rx) = fresh_pipe();
        assert!(rehost_pane(
            &mut ws,
            PaneId(1),
            gen,
            0,
            pty,
            rx,
            mullion_term::emulator::Emulator::DEFAULT_HISTORY,
            RehostKind::UserPicked,
        ));
        let text: String = ws
            .pane(PaneId(1))
            .unwrap()
            .emulator
            .snapshot()
            .cells
            .iter()
            .map(|c| c.ch)
            .collect();
        assert!(!text.contains("before-the-drop"), "换机器该重建 emulator");
    }

    /// F156-b:换节点成功后,分屏焦点跟到**那块 pane**;断线重连**绝不**跟。
    ///
    /// 两个函数长得很像,但语义相反:
    /// - 换节点是用户**刚刚**在标题条上亲手指定的,下一步必然是往新节点里
    ///   敲东西,焦点不跟过去他得再点一下。
    /// - 断线重连是后台自愈,可能发生在用户正在**另一块** pane 里打字的任意
    ///   时刻。抢焦点等于把他正在打的字发到另一台机器上去。
    ///
    /// 这条差异只写注释拦不住下一次「顺手把这两个函数统一一下」的重构,
    /// 所以拿一条**对照**测试钉住,而不是两条各测各的。
    ///
    /// 自证会变红:
    /// - 删掉 `rehost_pane` 里的 `ws.set_focus(id);` → 第 3 条断言红
    /// - 往 `reattach_pane` 里也加一句 `ws.set_focus(id);` → 第 2 条断言红
    #[test]
    fn rehosting_moves_the_focus_to_that_pane_but_reattaching_never_does() {
        use crate::shell::workspace::tests_support::{fresh_pipe, ws_with};
        let (mut ws, _probes) = ws_with(2);
        let generation = ws.generation();
        ws.set_focus(PaneId(1));
        assert_eq!(
            ws.focus(),
            PaneId(1),
            "脚手架的起始焦点就不在 1 号,下面两条断言分不出对错"
        );

        // 断线重连 2 号:焦点不动。
        let (pty, rx) = fresh_pipe();
        assert!(reattach_pane(&mut ws, PaneId(2), generation, 0, pty, rx));
        assert_eq!(
            ws.focus(),
            PaneId(1),
            "后台自愈把焦点从用户正在用的 pane 抢走了"
        );

        // 换节点 2 号:焦点跟过去。
        let (pty, rx) = fresh_pipe();
        assert!(rehost_pane(
            &mut ws,
            PaneId(2),
            generation,
            0,
            pty,
            rx,
            mullion_term::emulator::Emulator::DEFAULT_HISTORY,
            RehostKind::UserPicked,
        ));
        assert_eq!(
            ws.focus(),
            PaneId(2),
            "换完节点焦点还留在原来那块 pane 上,用户得再点一下才能打字"
        );
    }

    /// 三叶子的现场,只有一块 pane 是「已经连上的那块」,另外两片叶子由
    /// `apply_saved_tree` 分配 id、走 F162 的串行队列各自拨号。**它们此刻
    /// 没有 `PaneState`** —— 用户报的问题 5(三屏必有一屏永远卡在
    /// 「连接中…」)的真根因就在这:换节点那条路径原先看到
    /// `ws.pane_mut(id) == None` 就返回 `false`,`on_pane_rehosted` 于是
    /// `hosts.pop()` 把**已经拨通的连接**原地丢掉,只留一行 warn。
    ///
    /// 拿 `apply_saved_tree` 真跑一遍(而不是手搓一个缺 `PaneState` 的
    /// `Workspace`):缺的那块必须是恢复路径**自己**产出的,手搓等于把
    /// 「恢复路径会不会造出这种叶子」这个前提假设掉,而它正是 bug 的一半。
    ///
    /// 自证会变红:把 `rehost_pane` 里 `ws.pane(id).is_none()` 那个分支
    /// 改回 `return false`。
    #[test]
    fn a_restored_leaf_with_no_pane_state_yet_gets_one_instead_of_being_thrown_away() {
        use crate::shell::workspace::tests_support::{fresh_pipe, ws_with};
        use mullion_store::{SavedDir, SavedNodeEntry};
        let (mut ws, _probes) = ws_with(1);
        let generation = ws.generation();
        // 左边一片叶子 + 右边再对半分:三个叶子,前序 = [已连的那块, 新, 新]。
        let entries = vec![
            SavedNodeEntry::split(SavedDir::Horizontal, 0.5),
            SavedNodeEntry::leaf(),
            SavedNodeEntry::split(SavedDir::Vertical, 0.5),
            SavedNodeEntry::leaf(),
            SavedNodeEntry::leaf(),
        ];
        let fresh = ws.apply_saved_tree(&entries, 0, 0).expect("结构完整");
        assert_eq!(
            fresh.len(),
            2,
            "三叶子里该有两片是新分配、还没有 PaneState 的"
        );
        let leaf = fresh[0];
        assert!(
            ws.pane(leaf).is_none(),
            "脚手架前提就不成立:这块叶子已经有 PaneState 了,测不出那个 bug"
        );

        let (pty, rx) = fresh_pipe();
        assert!(
            rehost_pane(
                &mut ws,
                leaf,
                generation,
                1,
                pty,
                rx,
                mullion_term::emulator::Emulator::DEFAULT_HISTORY,
                RehostKind::RestoreFirstMount,
            ),
            "返回 false 的话调用方会把刚拨通的连接 pop 掉,这一格永远是「连接中…」"
        );
        let p = ws.pane(leaf).expect("首次挂载必须把 PaneState 建出来");
        assert_eq!(p.host_ix, 1, "指向错的机器,键盘输入会写到另一台上去");
        assert_eq!(
            p.status,
            crate::shell::workspace::PaneStatus::Live,
            "刚拨通的 channel 不该是断开态"
        );
        assert!(
            !p.host_pending,
            "身份已经由这条真连接坐实了,还挂着「照抄盘上那份」标题条会一直是灰的"
        );
    }

    /// F188:恢复现场是**后台批量拨号**,焦点必须停在存盘时的那一格。
    ///
    /// `apply_saved_tree` 已经按 `focus_leaf` 摆好焦点了;首次挂载再抢一次的话,
    /// 焦点最后会落在「碰巧最后一个拨通」的那块 pane 上 —— 拨通顺序取决于
    /// 网络,同一份现场每次恢复出来的焦点还不一样。
    ///
    /// **对照**着写(同一个 `Workspace`、两片同样缺 `PaneState` 的叶子、只有
    /// `kind` 不同):两条各测各的话,「`kind` 被整个忽略掉」这类变异总有一条
    /// 还是绿的。
    ///
    /// 自证会变红:
    /// - 把 `focus_after_rehost` 的 `if` 去掉、恒 `set_focus` → 第 1 条断言红
    /// - 把它整个改成空函数 → 第 2 条断言红
    #[test]
    fn a_first_mount_keeps_the_saved_focus_but_a_user_picked_rehost_still_takes_it() {
        use crate::shell::workspace::tests_support::{fresh_pipe, ws_with};
        use mullion_store::{SavedDir, SavedNodeEntry};
        let (mut ws, _probes) = ws_with(1);
        let generation = ws.generation();
        let entries = vec![
            SavedNodeEntry::split(SavedDir::Horizontal, 0.5),
            SavedNodeEntry::leaf(),
            SavedNodeEntry::split(SavedDir::Vertical, 0.5),
            SavedNodeEntry::leaf(),
            SavedNodeEntry::leaf(),
        ];
        // `focus_leaf = 0` = 存盘时焦点在已连上的那块(前序第 0 片叶子)。
        let fresh = ws.apply_saved_tree(&entries, 0, 0).expect("结构完整");
        let saved_focus = ws.focus();
        assert!(
            !fresh.contains(&saved_focus),
            "脚手架前提就不成立:存盘焦点落在了待拨号的叶子上,下面分不出对错"
        );

        // 恢复队列挂上第一片:焦点不动。
        let (pty, rx) = fresh_pipe();
        assert!(rehost_pane(
            &mut ws,
            fresh[0],
            generation,
            1,
            pty,
            rx,
            mullion_term::emulator::Emulator::DEFAULT_HISTORY,
            RehostKind::RestoreFirstMount,
        ));
        assert_eq!(
            ws.focus(),
            saved_focus,
            "后台拨号把焦点从存盘时的那一格抢走了"
        );

        // 对照:用户亲手换节点到第二片,焦点跟过去。
        let (pty, rx) = fresh_pipe();
        assert!(rehost_pane(
            &mut ws,
            fresh[1],
            generation,
            2,
            pty,
            rx,
            mullion_term::emulator::Emulator::DEFAULT_HISTORY,
            RehostKind::UserPicked,
        ));
        assert_eq!(
            ws.focus(),
            fresh[1],
            "用户刚亲手指定的节点,焦点该跟过去(F156-b)"
        );
    }

    /// F128:`swap_pane_channel` 存在的**唯一理由**就是把 pane 挪到新 channel 上,
    /// 而「pty 换没换」「rx 换没换」原先一条断言都没有 —— 两处赋值随便删一个,
    /// 全仓 1207 个测试照样全绿。这两条恰恰是最要命的:
    /// - `pty` 没换:新开的 channel 当场被 Drop,用户敲的键写进已死的旧 channel,
    ///   屏上却显示「已连上」,输入静默全丢、没有任何报错。
    /// - `rx` 没换:下一帧 `pump` 立刻在旧 rx 上读到 `Disconnected`,而这时新连接
    ///   是活的,`rx_closed_action` 判成「远端自己退了」→ pane 被永久错标成
    ///   `Disconnected`,**再也不会自动重试**。
    ///
    /// 自证会变红:删掉 `swap_pane_channel` 里的 `p.pty = pty;`(前半段红)
    /// 或 `p.rx = rx;`(后半段红)。
    #[tokio::test]
    async fn reattach_actually_swaps_both_the_pty_and_the_rx() {
        use crate::shell::workspace::tests_support::{fresh_pipe_probed, ws_with};
        let (mut ws, old) = ws_with(1);
        let gen = ws.generation();
        let (pty, rx, fresh) = fresh_pipe_probed();
        assert!(reattach_pane(&mut ws, PaneId(1), gen, 0, pty, rx));

        // pty:写出去的字节只能落在新管子上。
        let _ = ws.pane(PaneId(1)).unwrap().pty.write(b"ping".to_vec());
        assert_eq!(
            fresh.writes.lock().unwrap().as_slice(),
            [b"ping".to_vec()],
            "写没落到新 channel 上 = pty 没换"
        );
        assert!(
            old[0].writes.lock().unwrap().is_empty(),
            "还在往断掉的旧 channel 里写"
        );

        // rx:只有从新管子灌进来的字节才收得到。
        fresh.tx.try_send(b"pong".to_vec()).unwrap();
        ws.pump(0);
        let text: String = ws
            .pane(PaneId(1))
            .unwrap()
            .emulator
            .snapshot()
            .cells
            .iter()
            .map(|c| c.ch)
            .collect();
        assert!(
            text.contains("pong"),
            "新 channel 的输出没收到 = rx 没换,实际:{text:?}"
        );
    }

    /// F128:换了 channel 之后必须逼出一次 `window_change`(T4)。
    /// 新 channel 是 80x24,不重发的话远端 tmux 里的 TUI 按 80 列排版,
    /// 全屏 TUI 直接错行 —— 这是 T4 的原样复发。
    ///
    /// 自证会变红:把 `reattach_pane` 里的 `p.last_grid = (0, 0);` 删掉。
    #[test]
    fn reattach_forces_a_window_change() {
        use crate::shell::workspace::tests_support::{fresh_pipe, ws_with};
        let (mut ws, _p) = ws_with(1);
        let gen = ws.generation();
        ws.pane_mut(PaneId(1)).unwrap().last_grid = (120, 40);
        let (pty, rx) = fresh_pipe();
        reattach_pane(&mut ws, PaneId(1), gen, 0, pty, rx);
        assert_eq!(
            ws.pane(PaneId(1)).unwrap().last_grid,
            (0, 0),
            "不复位的话下一帧 apply_geometry 认为尺寸没变,不发 window_change(T4)"
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

    /// F131 接线守护:两栏 `FileAction::GotoInput` 分支必须调用**各自那份**
    /// 的解析函数、用**各自那份**的 cwd —— `apply_local_file_action` 和
    /// `apply_remote_file_action` 长得几乎一样(`Up`/`Refresh` 就是
    /// `local`/`remote` 各写一份、内容不同),复制粘贴时最容易把远端那份的
    /// 调用抄进本地分支(或反过来)。抄错的后果是「本地路径条按 POSIX 规则
    /// 解析」或「远端路径条按本机规则解析」,跳转要么跳错要么整个失效,
    /// 且不会有任何报错——两个函数都编译得过、跑得起来。
    ///
    /// **扎的是源码结构而非运行时行为**,理由同上面
    /// `file_actions_never_narrow_to_terminal_tabs_only`:`App` 要
    /// `EventLoopProxy`,单测里造不出来。验证边界:只挡得住「函数体里出现了
    /// 另一栏那份标识符」这一种写法,挡不住把两个解析函数的实现改成互相委托
    /// 之类的等价写法。
    ///
    /// 自证会变红:把 `apply_local_file_action` 里 `GotoInput` 分支的
    /// `resolve_local_input`/`files.local.cwd` 换成
    /// `resolve_remote_input`/`files.remote.cwd`(或者反过来改远端那份)。
    #[test]
    fn goto_input_resolves_against_the_matching_column_not_the_other_one() {
        let src = include_str!("app.rs");
        // 只看生产代码那一半 —— 理由同
        // `a_successful_write_triggers_a_refresh_so_the_list_is_not_stale`:
        // 断言用到的标识符字面量写在本测试自己身上,不切掉的话会命中自己。
        let (production, _) = src
            .split_once("#[cfg(test)]")
            .expect("找不到 #[cfg(test)] 边界");
        for (fn_name, own_resolver, own_cwd, other_resolver, other_cwd) in [
            (
                "fn apply_local_file_action",
                "resolve_local_input(",
                "files.local.cwd",
                "resolve_remote_input(",
                "files.remote.cwd",
            ),
            (
                "fn apply_remote_file_action",
                "resolve_remote_input(",
                "files.remote.cwd",
                "resolve_local_input(",
                "files.local.cwd",
            ),
        ] {
            let after = production
                .split(fn_name)
                .nth(1)
                .unwrap_or_else(|| panic!("找不到 {fn_name} 的定义"));
            // 同 `file_actions_never_narrow_to_terminal_tabs_only`:这两个都是
            // `impl App` 里的方法,函数体止于第一个 4 空格缩进的右花括号。
            let body = &after[..after
                .find("\n    }\n")
                .unwrap_or_else(|| panic!("找不到 {fn_name} 的函数结尾"))];
            assert!(
                body.contains("FileAction::GotoInput"),
                "{fn_name} 切出来的函数体里没有 GotoInput 分支 —— 要么切歪了,\
                 要么这条分支被删掉了"
            );
            assert!(
                body.contains(own_resolver) && body.contains(own_cwd),
                "{fn_name} 的 GotoInput 分支没有调用 {own_resolver} 或没用 \
                 {own_cwd} 当基准目录"
            );
            assert!(
                !body.contains(other_resolver) && !body.contains(other_cwd),
                "{fn_name} 里出现了另一栏那份的调用/字段({other_resolver} / \
                 {other_cwd})—— 大概率是复制粘贴时手滑抄错了栏"
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
        // 结尾锚点 = 紧跟它的下一个 item 的**签名**(F142 起是
        // `accept_owner_names`)。切到签名为止、不进那个函数体 —— 进去了的话
        // 它体内的 `by_generation_mut` 会替 `accept_sftp_listed` 顶包。
        let body = &after[..after
            .find("\n    fn accept_owner_names(")
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
            .find("\n    fn accept_owner_names(")
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
            .find("\n    fn accept_owner_names(")
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

    /// **接线守护**(F142):属主名字这条链的三个接头。`App` 单测里造不出来
    /// (`EventLoopProxy`),只能扎源码结构;验证边界与本文件其余同类守护一样,
    /// 挡得住「整段被删/退回旧写法」,挡不住等价改写。
    ///
    /// 三条各自的失效后果:
    /// 1. 不在 `accept` 之后问 → 拿被 seq 丢弃的旧 entries 去问,白跑一次
    ///    网络往返,还把一批用不上的 id 记进负缓存;
    /// 2. 拿焦点那台机器去问 → 在 B 机上查 A 机的 uid,画出来的名字是错的
    ///    (比不显示更糟:用户没法分辨);
    /// 3. 换连接不清缓存 → 同上,只是触发点换成「换节点/重连」。
    ///
    /// 自证会变红:分别删掉 `accept_sftp_listed` 里的 `take_missing`、把
    /// `sftp_host_ix()` 换成 `focused_pane_host_ix()`、删掉
    /// `accept_sftp_opened` 里的 `owners.clear()`。
    #[test]
    fn owner_names_are_asked_on_the_right_host_at_the_right_time() {
        let src = include_str!("app.rs");
        let listed_after = src
            .split("fn accept_sftp_listed(")
            .nth(1)
            .expect("找不到 accept_sftp_listed 的定义");
        let listed = &listed_after[..listed_after
            .find("\n    fn accept_owner_names(")
            .expect("找不到 accept_sftp_listed 的函数结尾")];

        let accept_at = listed
            .find("pane.accept(seq, Ok(entries));")
            .expect("accept_sftp_listed 里找不到落地 entries 那一句");
        let ask_at = listed
            .find("owners.take_missing(")
            .expect("accept_sftp_listed 没有发起属主名字查询 —— 属主列会永远是数字");
        assert!(
            accept_at < ask_at,
            "属主名字查询发在 `pane.accept` **之前** —— 被 seq 丢弃的那批 entries \
             也会被拿去问一遍"
        );
        assert!(
            listed.contains("sftp_connection_for(tab.content.sftp_host_ix())"),
            "accept_sftp_listed 查属主名字用的不是「列出这批 entries 的那台机器」\
             —— 换节点期间会在 B 机上查 A 机的 uid"
        );

        let opened_after = src
            .split("fn accept_sftp_opened(")
            .nth(1)
            .expect("找不到 accept_sftp_opened 的定义");
        let opened = &opened_after[..opened_after
            .find("\n    fn accept_sftp_listed(")
            .expect("找不到 accept_sftp_opened 的函数结尾")];
        assert!(
            opened.contains("owners.clear()"),
            "换到新连接时没清属主名字缓存 —— 同一个 uid 在两台机器上是两个人"
        );
    }

    /// **接线守护**(F142):`getent` 跑失败也必须回送 `UserEvent::OwnerNames`。
    ///
    /// 发出去那一刻这批 id 已经记进负缓存(`OwnerNames::take_missing` 的语义),
    /// 静默失败 = 它们在这条连接的余生里永远显示成数字,而且没有任何症状可查。
    ///
    /// 自证会变红:把 `spawn_getent` 里 `Err` 分支的 `None` 换成
    /// `return`(即失败就不回送)。
    #[test]
    fn a_getent_that_failed_still_reports_back_so_the_cache_rolls_back() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn spawn_getent(")
            .nth(1)
            .expect("找不到 spawn_getent 的定义");
        let body = &after[..after
            .find("\n/// 采纳一次拨测结果")
            .expect("找不到 spawn_getent 的函数结尾")];
        let err_at = body
            .find("Err(e) =>")
            .expect("spawn_getent 没有处理 exec 失败(sftp-only 账号会走到这条路)");
        assert!(
            !body[err_at..].contains("return"),
            "spawn_getent 的失败分支提前 return 了 —— 负缓存撤不回来,\
             这批 uid 在这条连接上永远显示成数字"
        );
        let send_at = body
            .find("send_event(UserEvent::OwnerNames")
            .expect("spawn_getent 没有回送结果");
        assert!(
            err_at < send_at,
            "回送发生在失败分支之前 —— 失败那条路走不到回送"
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

    /// **接线守护 / F148**:心跳必须挂在 `about_to_wait` 上,而且**不能**
    /// 走 `flush_layout_if_due` 那条「不脏就不写」的路。
    ///
    /// 漏了的症状极其隐蔽:一个开着不动的窗口(布局没变 → 不落盘 → 不写心跳)
    /// 会被别的实例判成死的,于是它**正在用**的现场出现在别人的恢复列表里,
    /// 被恢复出来就是两个窗口抢同一个槽位(设计 D4)。
    ///
    /// **扎的是源码结构**:真正验它要一个完整的 `App` + `EventLoopProxy`,
    /// 容器里造不出来。验证边界:挡得住「整个调用被删/挪走」,挡不住
    /// 「函数体被掏空」。
    ///
    /// 自证会变红:删掉 `about_to_wait` 里那句 `self.tick_heartbeat();`。
    #[test]
    fn about_to_wait_writes_the_heartbeat() {
        let src = include_str!("app.rs");
        let after = src
            .split("\n    fn about_to_wait(")
            .nth(1)
            .expect("找不到 about_to_wait 的定义");
        let body = &after[..after
            .find("\n    }\n")
            .expect("找不到 about_to_wait 的函数结尾")];
        assert!(
            body.contains("self.tick_heartbeat();"),
            "about_to_wait 不写心跳 —— 开着不动的窗口会被别人判死,现场被克隆走"
        );
    }

    /// **接线守护 / F148**:心跳**不许**跟布局落盘共用节流窗口。
    ///
    /// 自证会变红:把 `tick_heartbeat` 的函数体改成
    /// `self.save_layout_if_changed()` 那种「先比对再写」的形状。
    #[test]
    fn the_heartbeat_is_written_unconditionally_not_only_when_the_layout_changed() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn tick_heartbeat(")
            .nth(1)
            .expect("找不到 tick_heartbeat");
        let body = &after[..after
            .find("\n    }\n")
            .expect("找不到 tick_heartbeat 的结尾")];
        assert!(
            !body.contains("last_saved_layout"),
            "心跳搭上了布局落盘的「不脏就不写」—— 开着不动的窗口永远不写心跳"
        );
        assert!(
            body.contains("touch_alive("),
            "tick_heartbeat 没真的写心跳文件"
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

    /// F155:点了「确定」要做两件事 —— 把档位**存进设置**,并**当场施加**
    /// 到 log facade 上。
    ///
    /// 少了前者:重开设置又显示旧档,而且他这时点确定就把选择覆盖回去了。
    /// 少了后者:「选了详细档,日志一行没多」,重启才对 —— 最难自查的那种。
    ///
    /// 这里扎的是**源码结构**:真跑一遍要 `App`(无头环境构造不出来),
    /// 而 `apply_log_level` 是纯副作用、没有可断言的返回值。回写部分与
    /// `tmux_bootstrap` 那条守护切的是同一个函数体(`take_settings_draft`);
    /// 施加部分切的是 `O::Commit` 这个match分支。
    ///
    /// 自证会变红:删掉回写那一行,或删掉 `O::Commit` 里的 `apply_log_level()`。
    #[test]
    fn committing_the_settings_stores_and_applies_the_new_log_level() {
        let src = include_str!("app.rs");

        // 回写:与 tmux_bootstrap 用同一条路径(`take_settings_draft`)。
        let draft_after = src
            .split("\n    fn take_settings_draft(&mut self) {")
            .nth(1)
            .expect("找不到 take_settings_draft 的定义");
        let draft_body = &draft_after[..draft_after
            .find("\n    }\n")
            .expect("找不到 take_settings_draft 的函数结尾")];
        assert!(
            draft_body.contains("self.settings.log_level = d.log_level;"),
            "档位没存进设置 —— 重开设置显示的是旧档,再点确定就把选择覆盖回去了"
        );

        // 施加:O::Commit 分支里要调用 apply_log_level()。
        let commit_after = src
            .split("O::Commit => {")
            .nth(1)
            .expect("apply_settings_action 里没有 O::Commit 分支了？测试的锚点失效了");
        let commit_body = commit_after
            .split("O::Cancel")
            .next()
            .unwrap_or(commit_after);
        assert!(
            commit_body.contains("self.apply_log_level();"),
            "档位没当场施加到 log facade —— 设置存对了、日志却没变,重启才生效"
        );
    }

    /// F156-c:**每一条 pane 建立路径都必须走 `on_pane_ready`。**
    ///
    /// 三处调用点各写一遍注入,正是「列举式门控在加档时必然漏」——本项目
    /// 已经踩中三次。漏一处的症状是:那种方式开出来的 pane 永远跟不住目录,
    /// 而且完全静默(没有报错、没有日志,只是 `Ctrl+Shift+B` 停在 `~`)。
    ///
    /// 判据是「`self.start_automation(` 在整个文件里**只有一个**调用点」——
    /// 加第四种 pane 建立方式的人只要照着现有的写一句 `self.start_automation`,
    /// 这条就红,他会被逼着去看 `on_pane_ready`。
    ///
    /// 顺序也钉:注入串自带 `clear`,排在自动化**之后**会把用户登录后命令的
    /// 输出清掉一半。
    ///
    /// 自证会变红:
    /// - 把任意一处调用点改回直接调 `self.start_automation` → 计数变 2,第 1 条红
    /// - 把 `on_pane_ready` 里的注入删掉 → 第 3 条红
    /// - 把注入挪到 `start_automation` 之后 → 第 4 条红
    #[test]
    fn every_pane_ready_path_goes_through_on_pane_ready() {
        let src = include_str!("app.rs");
        // **只数 `mod tests` 之前的那一段**:本测试自己的文档注释与字符串
        // 字面量里也写着 `self.start_automation(` 这个 needle(用来描述判据
        // 本身),连着数的话永远数不到 1,变成一条永远红不了也永远真不了的
        // 假断言。同一手法见 `the_files_sidebar_syncs_to_the_terminal_only_on_the_closed_to_open_edge`。
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .expect("split 至少给一段");
        assert!(
            prod.len() < src.len(),
            "没能把搜索范围切到 `mod tests` 之前 —— 下面的断言会命中测试自己\
             写的字面量,变成恒绿"
        );
        let calls = prod.matches("self.start_automation(").count();
        assert_eq!(
            calls, 1,
            "`self.start_automation(` 有 {calls} 个调用点。新的 pane 建立方式\
             必须改走 `on_pane_ready`,否则那条路径不会注入 OSC 7(静默失效)"
        );
        // 切片键带上换行和缩进,钉住切的是**方法定义**而不是文档注释里的提及。
        let body = src
            .split("\n    fn on_pane_ready(")
            .nth(1)
            .expect("找不到 on_pane_ready 的定义");
        let body = &body[..body
            .find("\n    }\n")
            .expect("找不到 on_pane_ready 的函数结尾")];
        assert!(
            body.len() > 120,
            "on_pane_ready 的函数体切歪了(切出来 {} 字节)",
            body.len()
        );
        let inject = body
            .find("shell_bootstrap::osc7_setup_line()")
            .expect("on_pane_ready 里没有注入 OSC 7 —— 这个方法存在的理由就是它");
        let automate = body
            .find("self.start_automation(")
            .expect("on_pane_ready 里没起自动化 —— 三处调用点的另一半功能丢了");
        assert!(
            inject < automate,
            "注入排在了自动化之后 —— 注入串自带 clear,会把登录后命令的输出清掉"
        );
        assert!(
            body.contains("self.settings.shell_osc7_bootstrap"),
            "注入没读开关,用户关不掉"
        );
        // 精确锁 `&&`:只分别断言两个变量各自出现过,防不住把 `&&` 改成
        // `||` —— 那样参数依然「被用到」,编译器不会报 unused,但
        // `shell_osc7_bootstrap` 默认开着,断线重连(`may_clear_screen=false`)
        // 会因为 `||` 而照样清屏,精确复现这条测试本来要防的那个回归。
        assert!(
            body.contains("if may_clear_screen && self.settings.shell_osc7_bootstrap"),
            "两个条件不是 `&&` 组合了 —— 断线重连可能又会清屏:{body}"
        );
    }

    /// F156-c 回归修复:断线重连(`PaneReconnected`)**不许**清屏。
    ///
    /// `reattach_pane` 刻意保留断线前的屏幕内容(见
    /// `reattach_keeps_the_screen_but_rehost_wipes_it`)—— 而 F156-c 加的
    /// OSC 7 注入串以 `clear` 收尾,对每一块重连成功的 pane 无条件发,跟
    /// `plan` 是不是 `None` 毫无关系。这条判据是:注入这一步是否被一个「允许
    /// 清屏」的开关挡住,重连路径必须把这个开关传 `false`。
    ///
    /// 同一族设计原则见 F156-b:`rehost_pane`(用户手动操作)跟焦点,
    /// `reattach_pane`(后台自愈)刻意不跟 —— 断线重连不该打扰用户的屏幕。
    ///
    /// 锚点拆开拼(`concat!`)、用 `rsplit(start).next()` 取最后一次出现,
    /// 理由同 `reconnect_reruns_post_login_automation`:直接写完整字面量
    /// 会被这条测试自己的源码(`start` 那个字符串字面量)算成一次出现,
    /// 挤掉真正要切的那个分支。
    ///
    /// 自证会变红:把 `PaneReconnected` 分支里传给 `on_pane_ready` 的那个
    /// bool 改回 `true`(或者把 `on_pane_ready` 里这个开关的判断删掉)。
    #[test]
    fn reconnecting_never_clears_the_screen_even_with_a_pending_plan() {
        let raw = body_of(prod_src(), "fn on_pane_reconnected(");
        let body: String = raw
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            body.contains("self.on_pane_ready(generation, id, sink, plan, false)"),
            "PaneReconnected 分支没有把「允许清屏」传 false —— 重连会抹掉\
             reattach_pane 刚保住的屏幕内容:{body}"
        );
    }

    /// F156-c:设置弹窗「确定」时,新开关要真的搬进 `self.settings` ——
    /// 不搬的话用户改了、点了确定、也落了盘,但**本次运行**仍按旧值走,
    /// 而他不会知道要重启。
    ///
    /// 与既有的 `tmux_bootstrap`/`log_level` 两条切的是同一个函数体
    /// (`take_settings_draft`),写法照抄它们。
    ///
    /// 自证会变红:把 `self.settings.shell_osc7_bootstrap = d.shell_osc7_bootstrap;`
    /// 删掉。
    #[test]
    fn committing_the_settings_carries_the_shell_osc7_switch() {
        let src = include_str!("app.rs");
        let body = src
            .split("\n    fn take_settings_draft(&mut self) {")
            .nth(1)
            .expect("找不到 take_settings_draft 的定义");
        let body = &body[..body
            .find("\n    }\n")
            .expect("找不到 take_settings_draft 的函数结尾")];
        assert!(
            body.contains("self.settings.shell_osc7_bootstrap = d.shell_osc7_bootstrap;"),
            "「确定」没把 F156-c 的开关搬进 settings:{body}"
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

    /// **D13**:运行中恢复要**追加**,不能把现有标签顶掉 —— 顶掉会断连接。
    ///
    /// 自证会变红:把 `restore_tabs` 里的 `base + active` 改回 `active`。
    #[test]
    fn restoring_into_a_non_empty_window_switches_to_the_newly_added_tab() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn restore_tabs(")
            .nth(1)
            .expect("找不到 restore_tabs");
        let body = &after[..after.find("\n    }\n").expect("找不到 restore_tabs 的结尾")];
        assert!(
            body.contains("base + active"),
            "恢复时活动标签用的是记录内部的裸下标 —— 运行中恢复会跳到不相干的标签上"
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

    /// **接线守护 / F148 D1**:启动**不再**自动摆回上次的标签。
    ///
    /// 多开成为常态之后,「最近一条」是哪个窗口关出来的完全不可预测 ——
    /// 自动摆回一个随机窗口的布局比不摆更困惑。摆什么由用户在恢复列表里选。
    ///
    /// 自证会变红:把 `finish_store_open` 里那句 `self.restore_tabs(..)` 加回来。
    #[test]
    fn startup_no_longer_restores_tabs_behind_the_users_back() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn finish_store_open(")
            .nth(1)
            .expect("找不到 finish_store_open");
        let body = &after[..after
            .find("\n    }\n")
            .expect("找不到 finish_store_open 的结尾")];
        assert!(
            !body.contains("self.restore_tabs("),
            "启动仍在自动摆回标签 —— 多开时摆的是哪个窗口的现场完全不可预测(D1)"
        );
    }

    /// **接线守护 / F148 D14**:启动时必须迁移老的 `layout.toml`。
    ///
    /// 漏了的话,升级那一次用户正开着的现场直接消失,而且老文件会永远躺在
    /// 那儿不被任何人读。
    ///
    /// 自证会变红:删掉 `resumed` 里那句 `mullion_store::migrate_legacy(`。
    ///
    /// **必须切到 `resumed` 的函数体末尾再判**:不收尾的话 `after` 一路延伸到
    /// 下一个 `"fn resumed("` 字面量(也就是别的测试里的那个 split 参数),
    /// 而本测试自己的文档注释里就写着这个判据串 —— 一旦那条不相关的测试被
    /// 改名或挪走,这条守护就变成「自己匹配自己」的恒绿(第四类)。
    #[test]
    fn startup_migrates_the_legacy_layout_file() {
        let src = include_str!("app.rs");
        let after = src.split("fn resumed(").nth(1).expect("找不到 resumed");
        let body = &after[..after.find("\n    }\n").expect("找不到 resumed 的结尾")];
        assert!(
            body.contains("mullion_store::migrate_legacy("),
            "启动不迁移老的 layout.toml —— 升级那次的现场会直接消失(D14)"
        );
    }

    /// **接线守护 / F148 D5/X6**:裁剪只在启动时做一次。
    ///
    /// 自证会变红:删掉 `resumed` 里那句 `mullion_store::prune(`。
    ///
    /// 收尾的理由同上一条。
    #[test]
    fn startup_prunes_the_history_once() {
        let src = include_str!("app.rs");
        let after = src.split("fn resumed(").nth(1).expect("找不到 resumed");
        let body = &after[..after.find("\n    }\n").expect("找不到 resumed 的结尾")];
        assert!(
            body.contains("mullion_store::prune("),
            "启动不裁剪历史 —— layouts 目录会无限增长"
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

    /// F153:自动串行拨号选下一条时**必须跳过已经试过的**。
    ///
    /// 不跳的话:失败那条的 `dialing` 刚被 `ConnectErr` 复位(F37 收口),
    /// 「第一个未 dialing 的占位标签」判据会把它反复选中 —— 队列在一条
    /// 连不上的会话上原地打转,永远走不到后面的标签。
    ///
    /// 自证会变红:把 `next_auto_dial` 里的 `!tried.contains(&t.id)` 去掉。
    #[test]
    fn the_auto_dial_queue_skips_tabs_it_already_tried() {
        let mut tabs: Tabs<TabContent> = Tabs::default();
        tabs.open("甲".into(), Some(SessionId(1)), restored_tab(1, 1));
        tabs.open("乙".into(), Some(SessionId(2)), restored_tab(2, 1));
        let first = next_auto_dial(&tabs, &[]).expect("该给出第一条");
        let second = next_auto_dial(&tabs, &[first]).expect("该给出第二条");
        assert_ne!(first, second, "试过的标签又被选了一次 —— 队列会原地打转");
        assert_eq!(
            next_auto_dial(&tabs, &[first, second]),
            None,
            "两条都试过了还给第三条"
        );
    }

    /// F153:已经连上的标签不在自动拨号队列里 —— 它没有什么可拨的。
    #[test]
    fn the_auto_dial_queue_only_looks_at_placeholder_tabs() {
        let tabs = tabs_with_one_terminal_tab();
        assert_eq!(next_auto_dial(&tabs, &[]), None);
    }

    /// F153:收尾那条 toast 的文案。全成功和有失败要分得开 —— 后者得让用户
    /// 知道「有几条要自己点」。
    #[test]
    fn the_auto_dial_summary_tells_failures_apart_from_a_clean_run() {
        assert_eq!(auto_dial_summary(3, 0), "已自动连上 3 个标签");
        assert_eq!(
            auto_dial_summary(2, 1),
            "2 条已连接,1 条失败(点「重连」可再试)"
        );
    }

    /// F161:配置里一条登录后命令都没有时,模板**照样**要留给标签。
    ///
    /// 用户就是在远端手敲 `tt web01` 进 tmux 的那类人 —— 他的会话配置是空的,
    /// 计划自然为空。以前把「留模板」绑在「有计划」上,现象是恢复现场时
    /// `take_attach_intent` 拿不到模板,一块 pane 都没接回来,且完全静默。
    ///
    /// 自证会变红:把 `tab_keeps_template` 的函数体改成 `has_plan && !user_skipped`
    /// (也就是改回旧行为)。
    #[test]
    fn an_empty_automation_config_still_leaves_its_template_on_the_tab() {
        assert!(
            tab_keeps_template(false, false),
            "配置为空也要留模板 —— 恢复现场按实测名算 attach 全靠它"
        );
        assert!(tab_keeps_template(true, false));
        assert!(
            !tab_keeps_template(true, true),
            "用户明确跳过这一次,模板不该留给后来的 pane"
        );
    }

    /// F161:`take_attach_intent` 必须**先判断、产出得了才消费**那两张表。
    ///
    /// 倒过来写(先 `remove` 再 `?`)的话,正确性就依赖「`automation_template`
    /// 的赋值排在 `on_pane_ready` 之前」这条谁都看不见的前提 —— 那一步被挪后,
    /// 记录被永久吃掉、attach 永远不发,而且完全静默,一条测试都不会红。
    ///
    /// 修复2(F161)之后判据从 `t.automation_template.clone()?` 换成了
    /// `automation_for_leaf(..)?`(按叶子自己那条会话取自动化设置,见其
    /// 文档),锚点跟着迁移;不变量本身没变。
    ///
    /// 自证会变红:把 `t.leaf_wanted.remove(ix);` 挪到
    /// `let tpl = automation_for_leaf(..)?;` 之前。
    #[test]
    fn the_attach_intent_is_only_consumed_when_it_really_produces_a_plan() {
        let body = body_of(prod_src(), "fn take_attach_intent(");
        let judged = body
            .find("automation_for_leaf(")
            .expect("找不到取模板那一步");
        let consumed = body
            .find("leaf_wanted.remove(")
            .expect("找不到消费记录那一步");
        assert!(
            judged < consumed,
            "先消费后判断:早退会把记录永久吃掉而 attach 永远不发\n{body}"
        );
    }

    /// 修复2(F161):跨主机叶子必须按**它自己那台机器**的自动化设置发
    /// attach,不能被主叶子的 `automation_template` 覆盖。
    ///
    /// X 是标签级模板(主叶子那次连接写下的,开着自动化);Y 是恢复出来的
    /// Dial 叶子自己那条会话,用户对它明确关掉了自动化。判据必须是「X 开
    /// Y 关时 Y 不发字节」这个方向 —— 这才是「用户意图被主叶子的设置覆盖」
    /// 这个真实症状,反过来测(X 关 Y 开)测不出这条 bug。
    ///
    /// 自证会变红:把 `automation_for_leaf` 的函数体改成裸 `fallback`
    /// (等价于恢复成只看标签模板的旧行为)。
    #[test]
    fn an_attach_uses_the_settings_of_the_machine_it_lands_on() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut store = crate::shell::store::SessionStore::open(
            dir.path().to_path_buf(),
            &mullion_store::InMemoryKey([1u8; 32]),
        )
        .expect("开 store");

        let draft_with_automation = |enabled: bool| mullion_store::SessionDraft {
            identity: mullion_store::Identity {
                name: "leaf".into(),
                note: String::new(),
                group_id: None,
                tags: Vec::new(),
            },
            connection: mullion_store::Connection {
                host: "192.0.2.20".into(),
                port: 22,
                protocol: mullion_store::Protocol::Ssh,
            },
            auth: mullion_store::Auth::inline("user", mullion_store::AuthKind::Password),
            terminal: Default::default(),
            appearance: Default::default(),
            network: Default::default(),
            automation: mullion_store::AutomationPrefs {
                enabled: Some(enabled),
                ..Default::default()
            },
            sftp: Default::default(),
            secret: None,
        };

        let x = store.add(draft_with_automation(true), "2026-08-25T00:00:00Z");
        let y = store.add(draft_with_automation(false), "2026-08-25T00:00:00Z");

        // 标签级模板来自主叶子 X:开着。
        let fallback = store.resolved(x).ok().map(|c| c.automation);
        assert!(
            fallback.as_ref().is_some_and(|a| a.enabled),
            "前提条件不成立:X 应该是开着自动化的"
        );

        // 恢复出来的这块叶子连的是 Y,Y 自己关掉了。
        let tpl = automation_for_leaf(Some(&store), Some(y), fallback)
            .expect("Y 自己有配置,不该落回 fallback");
        assert!(
            !tpl.enabled,
            "取错了机器的设置 —— 用的是 X 的,不是 Y 自己的"
        );

        let plan = crate::automation::pending_for_measured_attach(&tpl, "web01", false);
        assert!(
            plan.is_none(),
            "Y 明确关掉了自动化,attach 字节却仍然发了出去 —— 违背了用户对 Y 的设置"
        );
    }

    /// D3/D6:落地一块占位 pane **不许**把分屏形状砸掉,且要如实说明原因。
    ///
    /// D3 的全部意义就是「树上有几个叶子就还是几个叶子,比例不许变形」——
    /// 之前这条路径(以及它的姊妹函数 `place_orphan_pane`/`degrade_restored_pane`)
    /// 一条测试都没有,包括源码切片。
    ///
    /// 自证会变红:把 `place_dead_pane_of` 里 `p.notice = Some(msg.to_string())`
    /// 那一行删掉(状态照样对,但标题条上不再有说明,用户看到一块断开的 pane
    /// 却不知道为什么)。
    #[test]
    fn a_placeholder_pane_keeps_the_split_intact_and_says_why() {
        let generation = 5;
        let mut ws = Workspace::new(test_pane(1), generation);
        // 切成两屏:多出一个待开的新叶子,特意不 attach —— 模拟它那条会话
        // 已经被用户删了(D3)/ 拨号还没回来(D6)。
        let (fresh, _) = apply_layout_actions(
            &mut ws,
            &crate::ui::UiActions {
                preset: Some(Preset::TwoLeftRight),
                close_pane: None,
                ..Default::default()
            },
        )
        .expect("点了预设,动作不该是 None");
        assert_eq!(fresh.len(), 1, "TwoLeftRight 比原来的 1 屏多 1 个新叶子");
        let placeholder = fresh[0];

        let before = mullion_core::layout::leaves(ws.tree()).len();
        let dirty = place_dead_pane_of(
            &mut ws,
            placeholder,
            generation,
            1000,
            "会话已被删除,无法自动恢复",
        );
        assert!(dirty, "落地一块新占位 pane 应该打脏");

        let after = mullion_core::layout::leaves(ws.tree()).len();
        assert_eq!(
            before, after,
            "落地占位 pane 不许改变叶子数 —— 分屏比例不许变形"
        );

        let p = ws.pane(placeholder).expect("占位 pane 应该有 PaneState 了");
        assert_eq!(p.status, crate::shell::workspace::PaneStatus::Disconnected);
        assert!(p.host_pending, "它从来没连上过自己那台机器");
        assert_eq!(
            p.notice.as_deref(),
            Some("会话已被删除,无法自动恢复"),
            "标题条上没有挂上说明文案"
        );
    }

    /// 设计 5.2②:恢复途中拍的快照**不许**把还没连上的叶子身份冲掉。
    ///
    /// `save_layout_if_changed` 每 2 秒从运行时状态现算快照。串行拨号进行中,
    /// 排队的叶子还没有自己的 `HostConn`,它的 `host_ix` 指着主叶子那台机器
    /// —— 照着量会把它的身份写成**别人的**;写 `None` 则半路 kill 掉 exe 之后
    /// 这条身份**永久丢失**。两种都是数据损坏。
    ///
    /// 自证会变红:把 `leaf_identity_of` 里的 `host_pending` 分支删掉,
    /// 改成一律查 `hosts[host_ix]`。
    #[test]
    fn a_snapshot_taken_mid_restore_keeps_the_pending_leaf_identities() {
        use crate::shell::layout_snapshot::LeafIdentity;
        use mullion_store::SessionId;

        // 排队等拨号的那块 pane:`host_ix` 仍是 0(主叶子那台机器)。
        let mut queued = test_pane(2);
        queued.host_pending = true;
        queued.tmux = Some("主叶子那台机器上的会话".into());

        let wanted = vec![(
            PaneId(2),
            LeafIdentity {
                session_id: Some(SessionId(7)),
                tmux: Some("web01".into()),
            },
        )];

        let got = leaf_identity_of(&|_| Some(SessionId(3)), Some(&queued), &wanted, PaneId(2));
        assert_eq!(
            got.session_id,
            Some(SessionId(7)),
            "排队中的叶子被写成了主叶子那台机器的身份"
        );
        assert_eq!(got.tmux.as_deref(), Some("web01"));
    }

    /// 另一半:**已经连上**的 pane 身份必须现量,不能照抄盘上那份。
    ///
    /// 用户在远端 `tmux detach` 之后 `p.tmux` 变 `None`,这才是事实(D1:
    /// 真值源是实测)。照抄盘上那份会让「已经退出 tmux」的 pane 下次恢复
    /// 又被塞回一个会话里。
    ///
    /// 两条分支各喂一种输入 —— 只喂一种会让两道防御互相掩护。
    #[test]
    fn a_connected_leaf_is_measured_not_copied_from_disk() {
        use crate::shell::layout_snapshot::LeafIdentity;
        use mullion_store::SessionId;

        let mut live = test_pane(1);
        live.host_pending = false;
        live.tmux = None; // 用户刚在远端 detach 出来 —— 这才是事实
        let wanted = vec![(
            PaneId(1),
            LeafIdentity {
                session_id: Some(SessionId(7)),
                tmux: Some("stale".into()),
            },
        )];
        let got = leaf_identity_of(
            &|ix| (ix == 0).then_some(SessionId(3)),
            Some(&live),
            &wanted,
            PaneId(1),
        );
        assert_eq!(got.session_id, Some(SessionId(3)), "该现量的没量");
        assert_eq!(got.tmux, None, "陈旧的 tmux 名被写回盘上了");
    }

    /// 接线守护:`snapshot_tabs_of` 真的把身份传给了 `to_entries`,不是传了个
    /// 空闭包。本项目反复踩过「纯函数写对了没接线」。
    ///
    /// 自证会变红:把 `snapshot_tabs_of` 里的 identity 闭包换成
    /// `&|_| LeafIdentity::default()`。
    #[test]
    fn the_leaf_identity_actually_reaches_the_snapshot() {
        let body = body_of(prod_src(), "fn snapshot_tabs_of(");
        assert!(
            body.contains("leaf_identity_of("),
            "snapshot_tabs_of 没有把真实身份传给 to_entries:\n{body}"
        );
    }

    /// F162 接线:恢复一个标签时,拨号拨的是**主叶子**那条会话,不是
    /// `SavedTab::session_id`。
    ///
    /// 叶子 0 的会话被用户删了时,照 `SavedTab::session_id` 拨会连上一台
    /// 「树上其实没有任何叶子属于它」的机器。
    ///
    /// 自证会变红:把 `reconnect_tab` 里那句 `restore_plan::main_leaf(...)`
    /// 删掉、改回直接用 `r.session_id`。
    #[test]
    fn reconnecting_a_restored_tab_dials_the_main_leaf_session() {
        let body = body_of(prod_src(), "fn reconnect_tab(");
        assert!(
            body.contains("restore_plan::main_leaf("),
            "reconnect_tab 还在照标签级 session_id 拨号:\n{body}"
        );
    }

    /// F162 接线的另一半:算出来的主叶子**真的传给了** `apply_saved_tree`。
    ///
    /// 上一条只钉住「算了主叶子」,这条钉「算完没被丢掉」。第三个参数退回
    /// 常量 `0` 的话,已连上的那块 pane 仍旧落在第 0 个叶子位上 —— 主叶子不是
    /// 第 0 个时,恢复回来的内容整体串一格,而 `apply_saved_tree` 自己的
    /// 单测(传什么就落在哪)照样全绿,扎不到这条接线。
    ///
    /// 自证会变红:把恢复分支里那个参数改回 `0`。
    #[test]
    fn the_main_leaf_actually_reaches_apply_saved_tree() {
        let body = body_of(prod_src(), "fn accept_connect_ok(");
        assert!(
            body.contains("p.main_leaf)"),
            "恢复分支没把主叶子传给 apply_saved_tree —— 已连的 pane 会落回第 0 个叶子位:\n{body}"
        );
    }

    /// D10:跨机器恢复**一条接一条**。并发会同时弹好几个密码框 / 主机指纹
    /// 确认框,用户根本分不清哪个框对应哪台机器。
    ///
    /// 判据放在纯函数上:队列里有在途的那一条时,`take_next_restore_dial`
    /// 必须什么都不给。
    ///
    /// 自证会变红:把 `in_flight` 那道判断去掉。
    #[test]
    fn restoring_a_two_host_tab_dials_serially() {
        let mut q = std::collections::VecDeque::from(vec![
            (1u64, PaneId(2), mullion_store::SessionId(7)),
            (1u64, PaneId(3), mullion_store::SessionId(9)),
        ]);
        let first = take_next_restore_dial(&mut q, false).expect("该发起第一条");
        assert_eq!(first.1, PaneId(2));
        assert_eq!(
            take_next_restore_dial(&mut q, true),
            None,
            "上一条还在途,不许并发拨第二条"
        );
        let second = take_next_restore_dial(&mut q, false).expect("上一条收口后该轮到第二条");
        assert_eq!(second.1, PaneId(3));
        assert_eq!(take_next_restore_dial(&mut q, false), None);
    }

    /// D6:一台机器连不上,**只有那块 pane** 变成断开态,其余照常用。
    ///
    /// 为什么不是全或无:一台机器关机就让另外两台也连不成,不成比例。
    ///
    /// 自证会变红:把 `PaneRehostErr` 分支里的
    /// `self.degrade_restored_pane(` 换成 `self.wind_down(`(整标签退回占位)。
    #[test]
    fn one_unreachable_host_only_disconnects_its_own_pane() {
        let body = body_of(prod_src(), "fn on_pane_rehost_err(");
        assert!(
            body.contains("degrade_restored_pane("),
            "换节点失败没有做 pane 级降级:\n{body}"
        );
        assert!(
            !body.contains("wind_down("),
            "整个标签退回了占位态 —— 一台机器关机不该让另外两台也用不了:\n{body}"
        );
    }

    /// F162:串行闸必须在**同步早退**路径上也复位。`spawn_rehost_on` 有几条
    /// 根本不发异步任务的早退(配置库不可用 / `dial_plan_for` 失败 / SFTP
    /// 节点),那时 `PaneRehosted`/`PaneRehostErr` 永远不会抵达 —— 闸不在这里
    /// 复位,队列里这条之后的叶子一个都不会再拨,而且完全静默。
    ///
    /// 自证会变红:把 `drive_restore_dial` 里那句 `self.restore_dial_busy = false;`
    /// 删掉。
    #[test]
    fn a_dial_that_never_leaves_the_ground_still_reopens_the_gate() {
        let body = body_of(prod_src(), "fn drive_restore_dial(");
        assert!(
            body.contains("self.restore_dial_busy = false;"),
            "同步早退时没复位串行闸,后面的叶子会永久停拨:\n{body}"
        );
    }

    /// D8:attach 失败之后**不补跑**配置的登录后命令。
    ///
    /// 失败检测发生在「发完等几秒看标题」之后,那时用户很可能已经在那块 pane
    /// 里敲东西了。延迟补发字节是本项目最危险的一类行为(同 F156-c 只在 pane
    /// 刚建立时注入 OSC 7 的理由)。停在裸 shell,pane 上挂提示,下一步交给用户。
    ///
    /// 自证会变红:在 `finish_attach_check` 的失败分支里加一句
    /// `self.start_automation(`。
    #[test]
    fn a_failed_attach_does_not_replay_the_configured_plan() {
        let body = body_of(prod_src(), "fn finish_attach_check(");
        assert!(
            !body.contains("start_automation("),
            "attach 失败之后补发了字节 —— 那时用户可能正在这块 pane 里打字:\n{body}"
        );
        assert!(
            !body.contains("pending_for_extra_pane("),
            "attach 失败之后补跑了配置计划:\n{body}"
        );
    }

    /// F163:发完 attach 之后要真的比对 —— `automation::Outcome::Completed`
    /// 的语义只是「字节发出去了」,远端 `tmux attach -t X` 返回什么客户端根本
    /// 不看,默认情况下 attach 失败**完全静默**。
    ///
    /// 接上了(实测名变成期望的那个)→ 收摊,不留提示。
    ///
    /// 自证会变红:把 `attach_check_verdict` 的成功分支删掉、恒返回
    /// `Verdict::Waiting`。
    #[test]
    fn a_successful_attach_clears_the_check() {
        assert_eq!(
            attach_check_verdict(Some("web01"), "web01", false),
            AttachVerdict::Ok
        );
    }

    /// 超时之前不下结论 —— 太早判会在慢链路上误报「没接上」。
    #[test]
    fn the_check_waits_until_the_deadline_before_complaining() {
        assert_eq!(
            attach_check_verdict(None, "web01", false),
            AttachVerdict::Waiting
        );
        assert_eq!(
            attach_check_verdict(None, "web01", true),
            AttachVerdict::Failed
        );
        assert_eq!(
            attach_check_verdict(Some("other"), "web01", true),
            AttachVerdict::Failed
        );
    }

    /// D4 的边界:这条判据**依赖 F124 在跑**。用户把 `tmux_bootstrap` 关掉时,
    /// attach 成功也不会有标题上报,校验会恒误报「没接上」。
    /// 开关关着就跳过校验(attach 照发,只是不许下失败结论)。
    ///
    /// 自证会变红:把 `should_check_attach` 里的 bootstrap 判断去掉。
    #[test]
    fn the_attach_check_is_skipped_when_title_reporting_is_off() {
        assert!(should_check_attach(true));
        assert!(
            !should_check_attach(false),
            "没开远端标题上报时校验会恒误报「没接上」"
        );
    }

    /// F163:宽限期必须从「attach 字节真的发完」起算。
    ///
    /// `automation::run` 的第一段是等首字节,最长能等 `ready_timeout_ms`
    /// (默认 15 秒);从「打算发」起算的话,在高延迟代理链路上 `tmux attach`
    /// 还没发出去,校验就已经判「没接上」了 —— 而这条误报会永久挂在标题条上。
    ///
    /// 自证会变红:把 `take_attach_intent` 里的 `deadline: None` 改回
    /// 一个算好的 `Some(..)`。
    #[test]
    fn the_grace_period_starts_when_the_bytes_are_out_not_when_we_decide_to_send() {
        let body = body_of(prod_src(), "fn take_attach_intent(");
        let code: String = body
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            code.contains("deadline: None"),
            "push 进队列时不许就把宽限期算好:{code}"
        );
    }

    /// F163:等首字节超时 / 用户接管 / 断线 —— attach 压根没发出去,
    /// 这时下「没接上」的结论纯属误报,得把校验撤掉。
    ///
    /// 自证会变红:把 `arm_or_drop_attach_check` 的 `if !completed` 分支删掉。
    #[test]
    fn an_attach_that_never_went_out_is_not_judged() {
        let body = body_of(prod_src(), "fn arm_or_drop_attach_check(");
        let code: String = body
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(code.contains("if !completed"), "{code}");
        assert!(
            code.contains("retain"),
            "非 Completed 要把这条校验撤掉:{code}"
        );
    }

    /// F163:接上了就把上一轮那句「已不存在」摘掉 —— 不摘的话它会永久
    /// 挂在一块完全正常的 pane 的标题条上。
    ///
    /// 自证会变红:把 `finish_attach_check` 的 Ok 分支改回裸 `return`。
    #[test]
    fn a_stale_notice_comes_down_once_the_pane_is_back() {
        let body = body_of(prod_src(), "fn finish_attach_check(");
        let code: String = body
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(code.contains("notice = None"), "{code}");
    }

    /// 造一个装着单块 pane 的 `Tabs<TabContent>`(不需要真的 `App` ——
    /// `App::new` 要 `EventLoopProxy`,测试容器里造不出来,但这里只需要
    /// `drive_attach_checks_of` 摸得到的那部分)。
    fn tabs_with_one_pane(generation: u64) -> Tabs<TabContent> {
        let mut tabs: Tabs<TabContent> = Tabs::default();
        tabs.open(
            "test".into(),
            None,
            TabContent::Terminal(Box::new(TerminalTab {
                ws: Workspace::new(test_pane(1), generation),
                current_preset: None,
                last_cfg: None,
                automation: Vec::new(),
                automation_template: None,
                tmux_attach: None,
                automation_status: None,
                files: Default::default(),
                sftp: None,
                sftp_host_ix: None,
                sftp_tasks: Vec::new(),
                sftp_default_remote: None,
                sftp_screenshot_dir: None,
                sftp_home: None,
                reconnect_tasks: Vec::new(),
                leaf_wanted: Vec::new(),
                leaf_detach: Vec::new(),
            })),
        );
        tabs
    }

    /// **接线守护(F163)**:这是「push 进队列 → 驱动 → notice 落到
    /// `PaneState` 上」这条链唯一真的跑通的测试 —— 纯函数全对、胶水代码错,
    /// 恰恰是这条切片(修复 1/修复 2)已经踩过两次的形状。
    ///
    /// 第一段:到期还没接上 → 摘掉这条校验、把说明挂上 pane。
    /// 第二段:同一块 pane 实测名变成期望的那个(接回来了)→ 再驱动一次,
    /// 上一段挂的说明要被摘掉(同时钉住修复 2)。
    #[test]
    fn a_timed_out_check_lands_its_notice_on_the_pane_and_a_good_one_takes_it_off() {
        let generation = 77;
        let mut tabs = tabs_with_one_pane(generation);
        let past = std::time::Instant::now() - std::time::Duration::from_secs(1);

        let checks = vec![AttachCheck {
            generation,
            pane: PaneId(1),
            name: "web01".into(),
            deadline: Some(past),
        }];
        let (pending, dirty) = drive_attach_checks_of(&mut tabs, checks, std::time::Instant::now());
        assert!(pending.is_empty(), "到期的校验没被摘掉");
        assert!(dirty, "notice 真的改了,该打脏");
        let notice = tabs
            .by_generation(generation)
            .and_then(|t| t.content.as_terminal())
            .and_then(|t| t.ws.pane(PaneId(1)))
            .and_then(|p| p.notice.clone());
        assert!(
            notice.as_deref().is_some_and(|n| n.contains("web01")),
            "没接上却没挂提示:{notice:?}"
        );

        // 这块 pane 实测到了期望的会话名(接回来了)。
        if let Some(p) = tabs
            .by_generation_mut(generation)
            .and_then(|t| t.content.as_terminal_mut())
            .and_then(|t| t.ws.pane_mut(PaneId(1)))
        {
            p.tmux = Some("web01".into());
        }
        let checks = vec![AttachCheck {
            generation,
            pane: PaneId(1),
            name: "web01".into(),
            deadline: Some(past),
        }];
        let (pending, dirty) = drive_attach_checks_of(&mut tabs, checks, std::time::Instant::now());
        assert!(pending.is_empty());
        assert!(dirty, "notice 从有变没有,该打脏");
        let notice = tabs
            .by_generation(generation)
            .and_then(|t| t.content.as_terminal())
            .and_then(|t| t.ws.pane(PaneId(1)))
            .and_then(|p| p.notice.clone());
        assert!(notice.is_none(), "接回来了,上一轮的提示该摘掉:{notice:?}");
    }

    /// F163:属主标签已经没了的校验条目必须被丢掉,而不是永远躺在队列里。
    ///
    /// 上膛的唯一来源是 `AutomationDone`,而 `wind_down`(关整个标签)对在途的
    /// 自动化 task 直接 `abort()` —— 那个事件永远不会抵达。条目的 `deadline`
    /// 会恒 `None`、`verdict` 恒 `Waiting`,每帧被拷贝 + 遍历一次却永远出不去。
    /// 收口放在这里而不是各个关闭点:移除标签的路径不止一条,列举式的清理
    /// 今天对、下次加一条就漏,而且漏了完全静默。
    ///
    /// 自证会变红:把 `drive_attach_checks_of` 里「标签不在就丢掉」那条去掉。
    #[test]
    fn a_check_whose_tab_is_gone_is_dropped_instead_of_piling_up() {
        // 标签根本没开过(等价于「已经被 wind_down 关掉」):`tabs` 里找不到
        // 这个世代号。
        let mut tabs: Tabs<TabContent> = Tabs::default();

        let checks = vec![AttachCheck {
            generation: 77,
            pane: PaneId(1),
            name: "web01".into(),
            // 模拟「字节还没发完标签就被关了」:`deadline` 恒 `None`,
            // 若靠 `expired` 判定就会恒 `Waiting`、永远留在队列里。
            deadline: None,
        }];
        let (pending, dirty) = drive_attach_checks_of(&mut tabs, checks, std::time::Instant::now());
        assert!(
            pending.is_empty(),
            "属主标签已经不在了,这条校验该被丢掉,不该继续留在队列里"
        );
        assert!(!dirty, "标签都没了,没有 pane 可挂 notice,不该打脏");
    }

    /// **接线守护 / F153**:恢复现场之后要自己开始拨号,不能等用户挨个点。
    ///
    /// 自证会变红:删掉 `restore_history` 里那两句 `self.auto_dial = ...` /
    /// `self.advance_auto_dial(None)`。
    #[test]
    fn restoring_a_record_starts_dialing_on_its_own() {
        let src = include_str!("app.rs");
        let after = src
            .split("fn restore_history(")
            .nth(1)
            .expect("找不到 restore_history");
        let body = &after[..after
            .find("\n    }\n")
            .expect("找不到 restore_history 的结尾")];
        assert!(
            body.contains("那条现场已经不在了"),
            "切片切歪了 —— 下面那条断言会空过"
        );
        assert!(
            body.contains("self.advance_auto_dial("),
            "恢复现场之后没有自动开始拨号(F153)"
        );
    }

    /// **接线守护 / F153**:两种结局都要推进队列。只接 `ConnectOk` 的话,
    /// 第一条连不上就把整条队列吊死在那儿。
    ///
    /// 自证会变红:删掉 `ConnectErr` 分支里那句 `advance_auto_dial(Some(false))`。
    #[test]
    fn both_outcomes_advance_the_auto_dial_queue() {
        let src = include_str!("app.rs");
        let err = src
            .split("UserEvent::ConnectErr(dial, msg) => {")
            .nth(1)
            .expect("找不到 ConnectErr 分支");
        let err_body = &err[..err.find("\n            UserEvent::").unwrap_or(err.len())];
        assert!(
            err_body.contains("advance_auto_dial(Some(false))"),
            "拨号失败不推进队列 —— 一条连不上就把后面全吊死"
        );
        // 切片键**带上换行和缩进**:`UserEvent::ConnectOk {` 在本文件里出现
        // 三次(两次是 `spawn_connect` 里往 proxy 发事件),只有事件分派那条
        // match 臂是「行首 + 12 空格」。转义写法让这行代码本身不会自我匹配。
        let ok = src
            .split("\n            UserEvent::ConnectOk {")
            .nth(1)
            .expect("找不到 ConnectOk 分派点");
        let ok_body = &ok[..ok.find("\n            UserEvent::").unwrap_or(ok.len())];
        assert!(
            ok_body.contains("accept_connect_ok("),
            "切片切到的不是 ConnectOk 的分派点 —— 下面那条断言会空过"
        );
        assert!(
            ok_body.contains("advance_auto_dial(Some(true))"),
            "拨号成功不推进队列 —— 只会连上第一条"
        );
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
                tmux_attach: None,
                automation_status: None,
                files: Default::default(),
                sftp: None,
                sftp_host_ix: None,
                sftp_tasks: Vec::new(),
                sftp_default_remote: None,
                sftp_screenshot_dir: None,
                sftp_home: None,
                reconnect_tasks: Vec::new(),
                leaf_wanted: Vec::new(),
                leaf_detach: Vec::new(),
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

    /// F149:IME 事件该不该落到终端,判据与普通键盘完全一致 —— 它就是键盘输入
    /// 的一种,只是走了另一条 winit 事件。四种组合都断言:只测一种的话,一份
    /// 「恒 true」或「恒 false」的实现有一半概率蒙混过去。
    #[test]
    fn ime_reaches_the_terminal_only_when_the_keyboard_would() {
        use crate::shell::input_route::Focus;
        assert!(
            ime_goes_to_terminal_of(Focus::Terminal, false, false),
            "终端聚焦、无模态、egui 不要键盘 → 中文该进终端"
        );
        assert!(
            !ime_goes_to_terminal_of(Focus::Terminal, false, true),
            "egui 文本框正拿着键盘焦点 → 中文是打给它的,不能同时发到远端 shell"
        );
        assert!(
            !ime_goes_to_terminal_of(Focus::FilesPanel, false, false),
            "焦点在文件面板 → 不该往终端写"
        );
        assert!(
            !ime_goes_to_terminal_of(Focus::Terminal, true, false),
            "模态弹窗开着 → 一切归 egui"
        );
    }

    /// F210:组字锚点的**两个**消费点必须都走 `ImeState::anchored_cursor`。
    ///
    /// 一个是系统候选框定位(`apply_ime_cursor_area`),一个是渲染快照
    /// (内联拼音 / 让路区间 / 光标 quad / 整帧指纹全读 `snap.cursor`)。
    /// 只改其中一个的现象是**候选框和内联拼音分家** —— 拼音钉住不动、候选框
    /// 仍跟着远端重绘乱跳,比原来的「一起跳」更难看,而且编译、测试全绿。
    ///
    /// 锚点逻辑本身的行为判据在 `input::tests` 那四条;这里只钉「接线没掉」,
    /// 因为 `App` 在无头环境造不出来。切片写法与
    /// `ime_that_is_not_for_the_terminal_clears_composition_and_invalidates_the_candidate_box`
    /// 同一套:锚点唯一性显式断言,并自检确实切窄了(否则退化成扫全文件、恒绿)。
    #[test]
    fn both_the_candidate_box_and_the_rendered_snapshot_take_the_composition_anchor() {
        let src = include_str!("app.rs");
        // needle 必须**拼**出来:写成整串字面量的话,本测试自己这几行也会被
        // `include_str!` 数进去,`count()` 永远大于 2,断言变成纯噪声。
        let anchored = concat!("self.ime.", "anchored_cursor(");
        assert_eq!(
            src.matches(anchored).count(),
            2,
            "锚点该有且只有两个消费点(候选框定位 + 渲染快照);少一个就是分家,\
             多一个说明又冒出了一条没被本测试钉住的路"
        );

        // ① 候选框定位。
        let head = "\n    fn apply_ime_cursor_area(&mut self) {";
        assert_eq!(src.matches(head).count(), 1, "锚点必须唯一");
        let after = src.split(head).nth(1).expect("找不到候选框定位函数");
        let body = &after[..after.find("\n    }").expect("找不到函数结尾")];
        assert!(
            body.len() < after.len(),
            "没切出函数体,断言会退化成扫全文件"
        );
        assert!(
            body.contains(&format!("let (acol, arow) = {anchored}")),
            "候选框定位必须取锚点,不能直接用远端真光标:{body}"
        );
        assert!(
            body.contains("preedit_cursor_col(dims.0, acol,")
                && body.contains("ime_cursor_area(g.term_px, (col, arow),"),
            "锚点算出来了却没喂进去,等于没改:{body}"
        );

        // ② 渲染快照。
        let head = "\n                            let mut snaps: Vec<_> = self";
        assert_eq!(src.matches(head).count(), 1, "锚点必须唯一");
        let after = src.split(head).nth(1).expect("找不到渲染快照的取用点");
        let region = &after[..after
            .find("\n                            let renders: Vec<crate::gpu::PaneRender<'_>> = snaps")
            .expect("找不到 PaneRender 组装点")];
        assert!(
            region.len() < after.len(),
            "没切出快照组装段,断言会退化成扫全文件"
        );
        assert!(
            region.contains(anchored)
                && region.contains("s.cursor.col = cell.0;")
                && region.contains("s.cursor.row = cell.1;"),
            "渲染快照的光标必须在喂给 PaneRender **之前**被钉回锚点:{region}"
        );
    }

    /// F149:IME 不归终端的那条分支,必须清掉组字状态并作废候选框记账。
    ///
    /// 少了 `on_disabled()`,终端会永久吞键(`swallows_key()` 恒 true)且把别人的
    /// 拼音内联画在自己光标处;少了 `ime_cursor_area = None`,回到终端组字时候选框
    /// 会停在 egui 文本框原来的位置。两条都编译得过、都只有人眼能发现。
    ///
    /// `App` 在无头环境造不出来,只能扎源码结构。按 `} else {` 切出那条分支的体,
    /// 并断言切出来的确实比整段短(否则退化成扫全文件,恒绿)。
    ///
    /// **两个 `split` 锚点都带 `\n` 前缀**:不带的话,`let to_terminal =
    /// ime_goes_to_terminal_of(` 这串字面量在本文件里会命中两次——真调用点一次,
    /// 加上本测试自己那句 `.split("…")` 里的字面量一次。`split` 只取第一处,
    /// 目前恰好是真调用点排在前面所以结果碰巧对,但这是运气,不是保证:一旦
    /// 两处顺序换了,`after` 会从「真调用点之后的代码」膨胀成「测试自己那行
    /// 之后的几乎整个文件」,下面 `else_body.len() < after.len()` 的自检随之
    /// 形同虚设(`after` 本身已经是半个文件,谁都比它短)。带 `\n` 前缀后,
    /// 只有真正顶格缩进 16 空格、前面是换行符的那一处(真调用点)才会命中,
    /// 测试自己那句因为前面是 `.split("` 而不是换行,不会被算进去。
    /// 光加前缀还不够——为了不让「唯一性」退化成又一条靠运气的假设,下面显式
    /// 断言 `matches().count() == 1`,把它钉成保证而不是巧合。
    #[test]
    fn ime_that_is_not_for_the_terminal_clears_composition_and_invalidates_the_candidate_box() {
        let src = include_str!("app.rs");
        let call_site = "\n                let to_terminal = ime_goes_to_terminal_of(";
        assert_eq!(
            src.matches(call_site).count(),
            1,
            "锚点必须在本文件里唯一 —— 出现两次的话 split 取到哪一处就成了运气,\
             切出来的 after 会膨胀到大半个文件,下面那句 len 自检随之失效"
        );
        let after = src
            .split(call_site)
            .nth(1)
            .expect("找不到 F149 的 IME 分流接线");
        let else_body = after
            .split("\n                } else {")
            .nth(1)
            .expect("找不到 IME 不归终端的那条分支");
        let else_body = &else_body[..else_body
            .find("\n                }")
            .expect("找不到 else 分支的结尾")];
        assert!(
            else_body.len() < after.len(),
            "没切出分支体,断言会退化成扫全文件"
        );
        assert!(
            else_body.contains("self.ime.on_disabled();"),
            "IME 不归终端时必须清组字状态,否则终端永久吞键:{else_body}"
        );
        assert!(
            else_body.contains("self.ime_cursor_area = None;"),
            "IME 不归终端时必须作废候选框记账,否则候选框停在 egui 文本框那儿:{else_body}"
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

    /// 取一个**具名函数**的块体。与 `arm_of` 同源(共用 `brace_balanced_arm`),
    /// 只是锚点从 `模式 => {` 换成函数签名的开头。
    ///
    /// `sig` 给到函数名加左括号(`"fn snapshot_tabs_of("`)——光给函数名会命中
    /// 调用处,截出来的是别处的代码,断言全部落空(同 `arm_of` 文档里那条)。
    fn body_of<'a>(production: &'a str, sig: &str) -> &'a str {
        let at = production
            .find(sig)
            .unwrap_or_else(|| panic!("找不到 {sig} —— 这条测试的锚点失效了"));
        let rest = &production[at + production[at..].find('{').expect("函数没有块体")..];
        let body = brace_balanced_arm(rest);
        assert!(
            body.len() < rest.len(),
            "{sig} 没截到闭合大括号,断言会退化成扫全文件"
        );
        body
    }

    /// `app.rs` 去掉测试模块之后的那一半。源码切片断言**必须**先切掉测试模块,
    /// 否则测试自己写的那句字面量就能把断言喂饱,恒绿。
    fn prod_src() -> &'static str {
        let src = include_str!("app.rs");
        let (prod, _) = src
            .split_once("\n#[cfg(test)]\nmod tests {")
            .expect("app.rs 的测试模块分界变了,所有源码切片断言的锚点都失效了");
        prod
    }

    /// F188:两个调用点各自传的 `RehostKind` 是**接线**,`rehost_pane` 的
    /// 那两条行为测试够不着它 —— 把两处的实参对调,行为测试全绿,而现象是
    /// 「恢复现场的叶子照旧永远卡在连接中」加「手点换节点焦点不跟过去」。
    ///
    /// 先剥掉注释行再断言:判据自己的说明文字里就带着这两个变体名,不剥的话
    /// 注释就能把断言喂饱(本项目反复踩到的恒绿模式)。
    ///
    /// 自证会变红:把两处的 `RehostKind::` 实参对调。
    #[test]
    fn the_restore_queue_asks_for_a_first_mount_and_the_title_bar_asks_for_a_user_pick() {
        let strip = |s: &str| {
            s.lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect::<String>()
        };
        let prod = prod_src();
        let queue = strip(body_of(prod, "fn drive_restore_dial(&mut self) {"));
        assert!(
            queue.contains("RehostKind::RestoreFirstMount"),
            "恢复队列拨的叶子还没有 PaneState,按「换节点」处理会被原地丢掉"
        );
        assert!(
            !queue.contains("RehostKind::UserPicked"),
            "恢复队列抢焦点,焦点会落在碰巧最后一个拨通的那块 pane 上"
        );
        let bar = strip(body_of(
            prod,
            "if let Some((pane, session)) = self.ui.rehost_request.take()",
        ));
        assert!(
            bar.contains("RehostKind::UserPicked"),
            "用户手点的换节点,焦点必须跟过去(F156-b)"
        );
        assert!(
            !bar.contains("RehostKind::RestoreFirstMount"),
            "手点换节点被当成首次挂载,焦点不跟过去了"
        );
    }

    /// **T3 守护**:进度事件是高频的(一个 100MB 的文件几千条),那条 arm 里
    /// 一旦出现 `ui_dirty` / `request_redraw`,就变成每秒几千帧、风扇起飞 ——
    /// 正是 T3 点名的那条红线。进度显示该由帧闸驱动,不由事件驱动。
    ///
    /// 结构守护(`user_event` 要 `&mut App`,无头造不出来)。
    /// 自证会变红:在那条 arm 里加一句 `mark_ui_dirty!(self.ui_dirty);`。
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

    /// F131 行为级测试(而不是上面那条源码扫描守护):真的构造一棵
    /// `Tabs<TabContent>`,把某一栏的 `path_edit` 置上,断言
    /// `files_path_editing_of` —— `Modal::FilesPathEdit` 真正读的那个判据 ——
    /// 认出这是编辑态;清空后必须变回 `false`。不需要真的 `App`(它要
    /// `EventLoopProxy`,测试容器里造不出来),纯逻辑核心已经拆成自由函数。
    #[test]
    fn files_path_editing_of_is_true_only_while_a_path_buffer_is_open() {
        let mut tabs = tabs_with_one_terminal_tab();
        assert!(
            !files_path_editing_of(&tabs, true),
            "还没点编辑,不该判成编辑态"
        );

        tabs.active_mut()
            .unwrap()
            .content
            .files_panel_mut()
            .unwrap()
            .remote
            .path_edit = Some("/etc".into());
        assert!(
            files_path_editing_of(&tabs, true),
            "remote 栏的 path_edit 置上后必须判成编辑态,否则 Modal::FilesPathEdit\
             永远不生效,输入框收不到键"
        );

        tabs.active_mut()
            .unwrap()
            .content
            .files_panel_mut()
            .unwrap()
            .remote
            .path_edit = None;
        assert!(
            !files_path_editing_of(&tabs, true),
            "提交/取消编辑后缓冲清空,必须退出模态,否则普通键盘操作会被一直\
             错误地全量喂给 egui"
        );

        // 本地栏那半必须单独钉一次:判据是 `remote || local`,只测 remote 的话
        // 把 `|| local` 那一半删掉照样全绿 —— 本项目在同一个文件里已经栽过
        // 一次同构缺口(见下一条 `files_local_alone_counts_as_a_real_action_…`)。
        tabs.active_mut()
            .unwrap()
            .content
            .files_panel_mut()
            .unwrap()
            .local
            .path_edit = Some("D:\\work".into());
        assert!(
            files_path_editing_of(&tabs, true),
            "本地栏的 path_edit 置上后也必须判成编辑态 —— 两栏各有一条路径条"
        );
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
    /// 扎源码而非行为:`rehost_pane` 之外的这段在 `App::on_pane_rehosted` 里
    /// (F162 把 `UserEvent::PaneRehosted` 分支抽成了具名方法),而 `App` 要
    /// `EventLoopProxy` 才能构造,单测里造不出来。
    ///
    /// 自证会变红:把那个 `else` 分支里的 `ws.hosts.pop();` 删掉。
    #[test]
    fn a_failed_rehost_takes_its_host_back_out_of_the_list() {
        let raw = body_of(prod_src(), "fn on_pane_rehosted(");
        // 注释行剥掉:这段代码里就有一句注释解释了为什么要 pop,不剥的话
        // 删掉 pop 本身测试照绿(踩过一次)。
        let body: String = raw
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            body.contains("rehost_pane("),
            "切错片段了:这段不像 on_pane_rehosted 的函数体"
        );
        assert!(
            body.contains("hosts.pop()"),
            "换节点没挂成时没把刚 push 的 HostConn 撤掉 —— 一条谁也不指向的 SSH \
             连接会一直开到标签关闭,而且完全静默"
        );
    }

    /// F160/F161:换节点 = 这块 pane 换了一台机器,任何关于原会话的意图都作废。
    ///
    /// 不清的话:恢复时拨失败(或会话已被删)的那块 pane,它的 `leaf_wanted`
    /// 记录带着**原来那台机器**的会话名一直留着(`take_attach_intent` 从没
    /// 成功产出过、没消费掉它)。用户换到别的机器 Z 之后,那条陈旧记录会顶掉
    /// 为 Z 现算的计划 —— 轻则 Z 自己的登录后命令被静默丢弃,重则撞上 Z 上的
    /// 同名会话真的 attach 上去,还带 `-d` 把别人踢下线。
    ///
    /// 自证会变红:把 `clear_leaf_attach_intent` 里那两行 `retain` 去掉。
    #[test]
    fn rehosting_a_pane_drops_any_leftover_attach_intent() {
        use crate::shell::layout_snapshot::LeafIdentity;
        use mullion_store::SessionId;

        let generation = 3;
        let mut tab = TerminalTab {
            ws: Workspace::new(test_pane(1), generation),
            current_preset: None,
            last_cfg: None,
            automation: Vec::new(),
            automation_template: None,
            tmux_attach: None,
            automation_status: None,
            files: Default::default(),
            sftp: None,
            sftp_host_ix: None,
            sftp_tasks: Vec::new(),
            sftp_default_remote: None,
            sftp_screenshot_dir: None,
            sftp_home: None,
            reconnect_tasks: Vec::new(),
            leaf_wanted: vec![
                (
                    PaneId(1),
                    LeafIdentity {
                        session_id: Some(SessionId(7)),
                        tmux: Some("旧机器上的会话".into()),
                    },
                ),
                (
                    PaneId(2),
                    LeafIdentity {
                        session_id: Some(SessionId(9)),
                        tmux: Some("别的 pane 的会话".into()),
                    },
                ),
            ],
            leaf_detach: vec![(PaneId(1), true), (PaneId(2), false)],
        };

        clear_leaf_attach_intent(&mut tab, PaneId(1));

        assert!(
            !tab.leaf_wanted.iter().any(|(id, _)| *id == PaneId(1)),
            "换节点之后 pane 1 的 leaf_wanted 记录还留着,会拿旧会话名去打新机器"
        );
        assert!(
            !tab.leaf_detach.iter().any(|(id, _)| *id == PaneId(1)),
            "换节点之后 pane 1 的 leaf_detach 记录还留着"
        );
        assert!(
            tab.leaf_wanted.iter().any(|(id, _)| *id == PaneId(2)),
            "不该把别的 pane 的 leaf_wanted 记录也清掉了"
        );
        assert!(
            tab.leaf_detach.iter().any(|(id, _)| *id == PaneId(2)),
            "不该把别的 pane 的 leaf_detach 记录也清掉了"
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
            calls, 6,
            "用户意图写入点的取消调用应为 6 处(粘贴/滚轮/键盘/输入法提交/\
             合成文本/截图直传,滚轮两个分支共用一次),实际 {calls} 处 —— 少了\
             会让自动化在用户打字时继续发命令,多了说明新增了输入路径但没复核\
             这条不变量。F209 那一处在 `paste_screenshot` 里、按键当时就掐,\
             **不在** `accept_shot_uploaded`:那时字节早发出去了,而且那块 pane\
             未必还是焦点(T11 同族)"
        );
    }

    /// **接线守护 / T7 变种**:`RedrawRequested` 绝不能进
    /// `egui_state.on_window_event`。egui-winit 0.30 把它归进「Things that may
    /// require repaint」,恒返回 `repaint: true`;我们收到 `repaint` 就
    /// `mark_ui_dirty` + `request_redraw()`,后者立刻再生成一个
    /// `RedrawRequested` —— 闭环自激。帧闸只挡出帧,挡不住这一圈空转:v0.1.68
    /// 的实机日志里,完全空闲时 `window_event` = `dirty` = `rr evt` =
    /// 4.8 万次/秒,一整个单核烧掉,而 `ui_dirty` 因此恒真,F158 的
    /// 「空闲不出帧」被彻底架空(`frame=313x` 对着一屏静止画面每秒跑 62 次
    /// egui 布局)。
    ///
    /// 判定本身在 `shell::input_route::egui_should_see_window_event`(有行为
    /// 测试);这里扎的是**调用位置** —— `App` 要 `EventLoopProxy` 才能构造,
    /// 门控有没有真接上只有源码结构能表达。
    ///
    /// **先剥掉 `//` 注释行**:上面这段注释里就写着两个标识符,不剥的话删掉
    /// 真代码它照样绿(恒绿模式⑮)。
    ///
    /// 自证会变红:把 `window_event` 里那句
    /// `shell::input_route::egui_should_see_window_event(&event)` 删掉。
    #[test]
    fn redraw_requested_never_reaches_egui_or_the_event_loop_spins_forever() {
        let src = include_str!("app.rs");
        // 锚点带**行首缩进 + 完整签名**:不带的话会匹配到测试自己写的那行
        // 字面量,函数改名后切到的是一段无关源码,报出方向跑偏的错误(恒绿模式⑩)。
        let at = src
            .find("\n    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {")
            .expect("找不到 window_event 的定义");
        let after = &src[at + 1..];
        let body = &after[..after
            .find("\n    }\n")
            .expect("找不到 window_event 的函数结尾")];
        let code = body
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let gate = code
            .find("shell::input_route::egui_should_see_window_event(&event)")
            .expect(
                "window_event 没用 egui_should_see_window_event 门控 —— RedrawRequested \
                 会被喂给 egui,egui-winit 对它恒返回 repaint:true,于是每一帧都在请求下一帧,\
                 事件循环永远等不到 Wait,空闲时烧满一个核(T3/T7)",
            );
        let feed = code
            .find("egui_state.on_window_event(")
            .expect("找不到 on_window_event 的调用点");
        assert!(
            gate < feed,
            "门控排在了 on_window_event 之后 —— 排在后面等于没门控"
        );
    }

    /// **接线守护 / F175**:喂 egui 的计时闭包里**只许有那一次调用**。
    ///
    /// 这个数字的整个用途是把 `window_event=` 那段拆开(它含路由判定、终端
    /// 分支、标脏),据此判断该去掉帧还是该去掉这一趟处理。往闭包里多包一行,
    /// 量到的就不再是它声称的东西 —— 而日志里长得一模一样,只会把下一轮的
    /// 优化方向带偏。反过来,把 `timed_` 拿掉则 `egui_feed=` 恒报 `n/a`,
    /// 看起来像「这台机器采不到」。
    ///
    /// 用**行下标紧邻**而不是「函数体里出现过 timed_」:后者对「计时闭包挪到
    /// 别处、调用点裸着」这个变异恒绿(F172 的同款判据实测过)。
    ///
    /// 自证会变红:把 `mark_ui_dirty!` 那句挪进闭包,或把 `timed_` 那层去掉。
    #[test]
    fn the_egui_feed_timer_wraps_that_one_call_and_nothing_else() {
        let src = include_str!("app.rs");
        let at = src
            .find("\n    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {")
            .expect("找不到 window_event 的定义");
        let after = &src[at + 1..];
        let body = &after[..after
            .find("\n    }\n")
            .expect("找不到 window_event 的函数结尾")];
        let code: Vec<&str> = body
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect();
        let timer = concat!("timed_egui_", "window_event(");
        let at: Vec<usize> = code
            .iter()
            .enumerate()
            .filter(|(_, l)| l.contains(timer))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            at.len(),
            1,
            "喂 egui 的计时点有 {} 处,只许有一处 —— 多一处就是把同一趟记了两遍",
            at.len()
        );
        let next = code[at[0] + 1].trim();
        assert!(
            next.starts_with(concat!("active.egui_state.on_", "window_event(")),
            "计时闭包的第一行不是那次调用,而是 `{next}` —— 计时范围与它声称的\
             范围不符,而日志里看不出来"
        );
        assert!(
            code[at[0] + 2].trim().starts_with("})"),
            "计时闭包里不止那一次调用 —— 多包进来的开销会被算进 `egui_feed=`,\
             下一轮据此做的取舍就是错的"
        );
    }

    /// **接线守护 / F171**:事件类型归因必须埋在 `resp.repaint` 的**分支体内**。
    ///
    /// 挪到 `if` 外面就变成「收到了什么」而不是「凭什么出帧」:egui 明确说了
    /// `repaint: false` 的那五类(`ActivationTokenDone`/`AxisMotion`/
    /// `DoubleTapGesture`/`RotationGesture`/`PanGesture`)会一起被算进来,
    /// `wev=` 与相邻的 `dirty=` 就不再是同一个判据下的两级归因,加起来对不上
    /// —— 而这两段能相互印证正是它们并排放的全部理由。**日志照写、数字照有,
    /// 只是指向错的地方**,一整趟实机往返白跑。
    ///
    /// 判据用**字节偏移**而不是「包含某串」:后者在埋点被挪到 if 外面时照样绿。
    /// 同款的先剥 `//` 注释行(恒绿模式⑮)——上面这段注释里就写着标识符。
    ///
    /// 自证会变红:把那两句埋点从 `if resp.repaint {` 里挪到它上面一行。
    #[test]
    fn the_event_kind_is_attributed_only_when_egui_actually_asked_for_a_repaint() {
        let src = include_str!("app.rs");
        let at = src
            .find("\n    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {")
            .expect("找不到 window_event 的定义");
        let after = &src[at + 1..];
        let body = &after[..after
            .find("\n    }\n")
            .expect("找不到 window_event 的函数结尾")];
        let code = body
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let branch = code
            .find("if resp.repaint {")
            .expect("找不到 resp.repaint 分支");
        // 分支体的结束:紧跟其后的 `request_redraw()`(标脏/请求重绘那一对)。
        let branch_end = code[branch..]
            .find("request_redraw();")
            .expect("resp.repaint 分支里找不到 request_redraw")
            + branch;
        let note = code
            .find("diag::note_window_event(crate::wev::kind_of(&event));")
            .expect(
                "window_event 里没有 F171 的事件类型埋点 —— `wev=` 会恒为 `-`,\
                 而那与「这窗口一次没触发重绘」在日志里长得一模一样",
            );
        let cursor = code
            .find("diag::note_cursor_pos(position.x, position.y);")
            .expect("window_event 里没有 F171 的指针坐标去重埋点");
        assert!(
            branch < note && note < branch_end,
            "事件类型埋点不在 resp.repaint 分支体内 —— 归因判据与 dirty= 脱钩"
        );
        assert!(
            branch < cursor && cursor < branch_end,
            "指针坐标埋点不在 resp.repaint 分支体内"
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
            fn close(&self) {}
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
                history_reported: 0,
                host_pending: false,
                notice: None,
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
            fn close(&self) {}
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
                history_reported: 0,
                host_pending: false,
                notice: None,
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

    /// **接线守护 / D0**:`ConnectOk` 每开一个标签必须把 `next_ws_generation`
    /// 往前拨一格。
    ///
    /// 世代号是**标签路由键** —— `by_generation_mut`、`drive_*` 那几个每帧
    /// 驱动函数、SFTP/传输/编辑的每一条回程事件,全靠它认领「这条结果是哪个
    /// 标签的」。不自增的话第二个标签跟第一个同号,`by_generation_mut` 命中
    /// 的永远是先找到的那个:症状是在新标签里下载的文件把进度画到旧标签上、
    /// 关掉一个标签把另一个的传输一起取消,而且**两个标签本身都工作正常**,
    /// 归因极难。
    ///
    /// 这条是重构期补的:变异测试(注释掉 `self.next_ws_generation += 1;`)
    /// 实测**全绿放行**,1901 个测试没有一个抓得住 —— 因为它们几乎都只开
    /// 一个标签,单标签下同号与不同号无从区分。
    ///
    /// 自证会变红:注释掉 `ConnectOk` 里的 `self.next_ws_generation += 1;`。
    #[test]
    fn every_new_tab_bumps_the_generation_so_it_is_a_unique_routing_key() {
        let src = include_str!("app.rs");
        // 锚点拆开拼,免得 `split` 撞上这行字面量自身(理由同
        // `reconnect_drops_the_dead_sftp_client`)。换成函数体边界之后比原来的
        // 「两个 UserEvent 变体之间」稳:后者依赖 `ConnectErr` 恰好紧跟在
        // `ConnectOk` 后面这个纯排版事实。
        let after = src
            .split(concat!("fn accept_", "connect_ok("))
            .nth(1)
            .expect("找不到 accept_connect_ok");
        let body = &after[..after
            .find("\n    }\n")
            .expect("找不到 accept_connect_ok 的结尾")];
        assert!(
            body.contains("let generation = self.next_ws_generation;"),
            "ConnectOk 没有取世代号 —— 下面那条断言会空过"
        );
        assert!(
            body.contains("self.next_ws_generation += 1;"),
            "取了世代号却没往前拨 —— 下一个标签会拿到同一个号,\
             by_generation_mut 从此认错标签(进度画到别的标签上、\
             关一个标签取消另一个的传输),而两个标签本身都正常"
        );
    }

    /// **接线守护 / F205**:一次拨号的两种结局都必须**认票**。
    ///
    /// 类型系统只逼到「事件里带上票号」为止 —— 把 `dial` 绑成 `_dial` 照样编过。
    /// 不认的话票永远挂在 `self.dials` 上:里面装着 `SshConfig`(主机/端口/
    /// 认证方式)和整份自动化计划,长时间跑下来全是死票;更要命的是 `claim`
    /// 的「取出即消费」语义失效,同一张票能被认第二次,两个标签共用一份随行
    /// 数据 —— 那正是 F205 要根治的症状,只是换了个入口。
    ///
    /// **扎的是源码结构**:`App` 要 `EventLoopProxy` 才能构造,单测里跑不动
    /// 真实拨号。验证边界:挡得住「分支里压根没有 `claim`」,挡不住「认了却
    /// 把结果丢了」之类更隐蔽的走样(那种由 `accept_connect_ok` 开头的
    /// `let Some(ticket) = .. else { return }` 兜着)。
    ///
    /// 自证会变红:删掉 `ConnectErr` 分支里那句 `self.dials.claim(dial);`。
    #[test]
    fn both_outcomes_claim_the_dial_ticket_so_the_ledger_can_empty() {
        let src = include_str!("app.rs");
        // 锚点拆开拼,免得 `split` 撞上这行字面量自身(理由同上一条守护)。
        let after_ok = src
            .split(concat!("fn accept_", "connect_ok("))
            .nth(1)
            .expect("找不到 accept_connect_ok");
        let ok_body = &after_ok[..after_ok
            .find("\n    }\n")
            .expect("找不到 accept_connect_ok 的结尾")];
        assert!(
            ok_body.contains("let session_id = ticket.session_id;"),
            "ConnectOk 的身份不再来自票 —— 下面那条断言会空过"
        );
        assert!(
            ok_body.contains("self.dials.claim(dial)"),
            "ConnectOk 没认票 —— 票留在台账上,同一张能被认第二次"
        );

        let after_err = src
            .split(concat!("UserEvent::Connect", "Err(dial, msg) => {"))
            .nth(1)
            .expect("找不到 ConnectErr 分支");
        let err_body = &after_err[..after_err
            .find("\n            }\n")
            .expect("找不到 ConnectErr 分支的结尾")];
        assert!(
            err_body.contains("self.ui.set_error(msg);"),
            "切片切到的不是 ConnectErr 的处理分支 —— 下面那条断言会空过"
        );
        assert!(
            err_body.contains("self.dials.claim(dial);"),
            "拨号失败没认票 —— 台账只涨不落,SshConfig 和自动化计划一直不释放"
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
        // 锚点拆开拼,免得 `split` 撞上这行字面量自身(理由同
        // `reconnect_drops_the_dead_sftp_client`)。换成函数体边界之后比原来的
        // 「两个 UserEvent 变体之间」稳:后者依赖 `ConnectErr` 恰好紧跟在
        // `ConnectOk` 后面这个纯排版事实。
        let after = src
            .split(concat!("fn accept_", "connect_ok("))
            .nth(1)
            .expect("找不到 accept_connect_ok");
        let body = &after[..after
            .find("\n    }\n")
            .expect("找不到 accept_connect_ok 的结尾")];
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

    /// **接线守护 / F37 既存 bug**:拨号失败必须把 `pending_restore` 和那个
    /// 标签的 `dialing` 一起收口。
    ///
    /// 少了 `pending_restore.take()`:`reconnect_tab` 开头那道闸永久关闭 ——
    /// 这个进程里**所有**占位标签的「重连」从此静默无反应。
    /// 少了 `dialing = false`:按钮永远停在禁用的「连接中…」。
    /// 两条都是「一次失败换永久坏 + 全程不报错」,所以分开断言 —— 只钉一条
    /// 会让另一条悄悄退化。
    ///
    /// 自证会变红:把 `ConnectErr` 分支里的 `self.pending_restore.take()`
    /// 删掉(第一条红),或把复位 `dialing` 那几行删掉(第二条红)。
    #[test]
    fn a_failed_dial_releases_the_reconnect_latch_and_re_enables_the_button() {
        let src = include_str!("app.rs");
        let after = src
            .split("UserEvent::ConnectErr(dial, msg) => {")
            .nth(1)
            .expect("找不到 ConnectErr 分支");
        let body = &after[..after
            .find("\n            UserEvent::")
            .unwrap_or(after.len())];
        assert!(
            body.contains("连接失败"),
            "切片切歪了 —— 下面两条断言会空过"
        );
        assert!(
            body.contains("self.pending_restore.take()"),
            "拨号失败没释放 pending_restore —— 之后所有占位标签都再也连不上"
        );
        assert!(
            body.contains("dialing = false"),
            "拨号失败没复位 dialing —— 那个标签的「重连」按钮永远禁用"
        );
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
        // 锚点拆开拼,免得 `split` 撞上这行字面量自身(理由同
        // `reconnect_drops_the_dead_sftp_client`)。换成函数体边界之后比原来的
        // 「两个 UserEvent 变体之间」稳:后者依赖 `ConnectErr` 恰好紧跟在
        // `ConnectOk` 后面这个纯排版事实。
        let after = src
            .split(concat!("fn accept_", "connect_ok("))
            .nth(1)
            .expect("找不到 accept_connect_ok");
        let body = &after[..after
            .find("\n    }\n")
            .expect("找不到 accept_connect_ok 的结尾")];
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
            .split("fn spawn_connect(")
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

    /// **接线守护 / D1**:`accept_connect_ok` 收到 `wants_sftp: true` 那条分支,不许
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
        // 锚点拆开拼 + 换成函数体边界,理由同上一条。内层那个结尾锚点跟着
        // 缩进走:分支体从 `match` 里的 16 空格降到方法体的 8 空格。
        let after = src
            .split(concat!("fn accept_", "connect_ok("))
            .nth(1)
            .expect("找不到 accept_connect_ok");
        let full_body = &after[..after
            .find("\n    }\n")
            .expect("找不到 accept_connect_ok 的结尾")];
        let sftp_branch = full_body
            .split("if wants_sftp {")
            .nth(1)
            .expect("找不到 accept_connect_ok 里的 wants_sftp 分支")
            .split("\n        }\n")
            .next()
            .expect("找不到 accept_connect_ok 里 wants_sftp 分支的结尾");
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
                tmux_attach: None,
                automation_status: None,
                files: Default::default(),
                sftp: None,
                sftp_host_ix: None,
                sftp_tasks: vec![task],
                sftp_default_remote: None,
                sftp_screenshot_dir: None,
                sftp_home: None,
                reconnect_tasks: Vec::new(),
                leaf_wanted: Vec::new(),
                leaf_detach: Vec::new(),
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

    /// F140:关标签要把它名下**每一块** pane 的 channel 都关掉。
    ///
    /// `wind_down` 原本只 abort 后台任务、然后让 `Workspace` 自然 drop ——
    /// 而 drop 关不掉 channel(russh 0.54.5 的 `ChannelWriteHalf` 没有 `Drop`)。
    /// 用户关掉一个开了 4 块分屏的标签,远端就多 4 个挂着的 shell,同时占着
    /// 4 个 channel slot。
    ///
    /// 自证会变红:把 `wind_down` 里那句 `t.ws.close_all_panes()` 删掉。
    #[test]
    fn winding_down_a_terminal_tab_closes_every_pane_channel_f140() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CountingPty(Arc<AtomicUsize>);
        impl crate::shell::workspace::PtyWriter for CountingPty {
            fn write(&self, _b: Vec<u8>) -> Result<(), mullion_ssh::session::TrySendErr> {
                Ok(())
            }
            fn resize(&self, _c: u16, _r: u16) -> Result<(), mullion_ssh::session::TrySendErr> {
                Ok(())
            }
            fn close(&self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        let closes = Arc::new(AtomicUsize::new(0));
        let mut p1 = test_pane(1);
        p1.pty = Box::new(CountingPty(closes.clone()));
        let mut ws = Workspace::new(p1, 0);
        let id2 = ws
            .split_focused(mullion_core::layout::Dir::Horizontal)
            .expect("分不出第二块");
        let mut p2 = test_pane(id2.0);
        p2.id = id2;
        p2.pty = Box::new(CountingPty(closes.clone()));
        ws.attach_pane(p2);

        let tab = Tab {
            id: TabId(1),
            title: "test".into(),
            session_id: None,
            title_override: None,
            color_override: None,
            content: TabContent::Terminal(Box::new(TerminalTab {
                ws,
                current_preset: None,
                last_cfg: None,
                automation: Vec::new(),
                automation_template: None,
                tmux_attach: None,
                automation_status: None,
                files: Default::default(),
                sftp: None,
                sftp_host_ix: None,
                sftp_tasks: Vec::new(),
                sftp_default_remote: None,
                sftp_screenshot_dir: None,
                sftp_home: None,
                reconnect_tasks: Vec::new(),
                leaf_wanted: Vec::new(),
                leaf_detach: Vec::new(),
            })),
        };
        wind_down(tab);

        assert_eq!(
            closes.load(Ordering::SeqCst),
            2,
            "关标签没关掉全部 pane 的 channel —— 远端会留下挂着的 shell(F140)"
        );
    }

    /// **接线守护(F128 版,真实行为而非源码文本)**:关标签必须 abort 掉在途的
    /// **重连**任务。
    ///
    /// 与上面 sftp 那条同一类问题的第三次重演,而 F128 落地时漏了这一处:
    /// `spawn_reconnect` 起的 task 挂在退避 `sleep` 或 `establish` 握手上,
    /// 只 drop `TerminalTab` 收不了口。用户关掉标签之后它仍会拨完号、做完一整
    /// 套认证(远端因此多一条登录记录),然后才因为查不到属主标签把结果丢掉。
    /// `establish` 内部没有超时包裹,高延迟代理链路黑洞时这个"白拨"可能挂很久。
    ///
    /// 写法与 `wind_down_aborts_outstanding_sftp_tasks` 完全一致(哑任务 +
    /// `Drop` 观测),理由见那条:比匹配 `.abort()` 字符串更接近真实行为。
    ///
    /// 自证会变红:把 `wind_down` 里 `for task in t.reconnect_tasks` 那一段删掉。
    #[tokio::test]
    async fn wind_down_aborts_outstanding_reconnect_tasks() {
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
            std::future::pending::<()>().await;
        });

        // 先让哑任务真正被 poll 过一次,理由见 sftp 那条测试里的长注释。
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
                tmux_attach: None,
                automation_status: None,
                files: Default::default(),
                sftp: None,
                sftp_host_ix: None,
                sftp_tasks: Vec::new(),
                sftp_default_remote: None,
                sftp_screenshot_dir: None,
                sftp_home: None,
                reconnect_tasks: vec![task],
                leaf_wanted: Vec::new(),
                leaf_detach: Vec::new(),
            })),
        };

        wind_down(tab);

        for _ in 0..200 {
            if dropped.load(Ordering::SeqCst) {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            dropped.load(Ordering::SeqCst),
            "wind_down 没有 abort 掉在途的重连任务 —— 关了标签之后它还会拨完号、\
             做完一整套认证,远端多一条登录记录"
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
                bookmarks: None,
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
                bookmarks: None,
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

    /// F189 端到端红线(用户报的问题 1 的另一半):**路径条 ☆ 收的目录,
    /// 不能被会话编辑器的一次「保存」抹掉。**
    ///
    /// 两条写入口指着同一份数据:`add_bookmark`(随手收藏)与编辑器里那张
    /// 表。编辑器手上那份是打开那一刻的快照,无条件写回去就是拿旧的顶掉新的
    /// —— 而这正是「每次改点配置,收藏夹就少几条」的机制。
    ///
    /// 两段都要走到:`bookmarks: None`(没动过书签表)必须保住盘上那份;
    /// `Some(..)`(真动过)必须写得进去 —— 只测前一半的话,把
    /// `set_bookmarks` 那一支整个删掉也照样绿。
    ///
    /// 自证会变红:
    /// - 把 `Vault::update` 里保住 `bookmarks` 那三句删掉 → 第 1 段红
    /// - 把 `apply_save` 里 `if let Some(marks) = bookmarks` 那一支删掉 → 第 2 段红
    #[test]
    fn saving_the_editor_does_not_wipe_a_bookmark_added_from_the_path_bar() {
        let (_dir, mut store) = tmp_store();
        let buf = crate::ui::session_manager::EditorBuffer {
            name: "dev".into(),
            host: "192.0.2.10".into(),
            user: "user".into(),
            ..Default::default()
        };
        let intent = |id, bookmarks| crate::ui::session_manager::SaveIntent {
            editing_id: id,
            draft: crate::ui::session_manager::build_draft(&buf).expect("build"),
            password: crate::ui::session_manager::SecretField::Clear,
            passphrase: crate::ui::session_manager::SecretField::Clear,
            proxy_password: crate::ui::session_manager::SecretField::Clear,
            private_key: crate::ui::session_manager::SecretField::Keep,
            then_connect: false,
            bookmarks,
        };
        let id = apply_save(&mut store, intent(None, None), "2026-08-28T00:00:00Z").expect("新建");

        // 用户在文件面板路径条上收了一个目录。
        store
            .add_bookmark(
                id,
                mullion_store::Bookmark {
                    name: "日志".into(),
                    path: "/var/log".into(),
                },
            )
            .expect("收藏");
        store.save().expect("落盘");

        // 然后回会话编辑器改了点别的(书签表没动)并保存。
        apply_save(&mut store, intent(Some(id), None), "2026-08-28T00:01:00Z").expect("保存");
        assert_eq!(
            store
                .list()
                .iter()
                .find(|r| r.id == id)
                .map(|r| r.sftp.bookmarks.len()),
            Some(1),
            "编辑器的一次保存把路径条收的目录抹掉了"
        );

        // 对照:真在表单里动过书签表时,那张表必须写得进去。
        apply_save(
            &mut store,
            intent(Some(id), Some(Vec::new())),
            "2026-08-28T00:02:00Z",
        )
        .expect("保存");
        assert_eq!(
            store
                .list()
                .iter()
                .find(|r| r.id == id)
                .map(|r| r.sftp.bookmarks.len()),
            Some(0),
            "在表单里删掉的书签又回来了"
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
                bookmarks: None,
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
                bookmarks: None,
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
                bookmarks: None,
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
                tmux_attach: None,
                automation_status: None,
                files: Default::default(),
                sftp: None,
                sftp_host_ix: None,
                sftp_tasks: Vec::new(),
                sftp_default_remote: configured.map(|s| s.to_string()),
                sftp_screenshot_dir: None,
                sftp_home: None,
                reconnect_tasks: Vec::new(),
                leaf_wanted: Vec::new(),
                leaf_detach: Vec::new(),
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

    /// F132:这条 sftp channel 到底开在哪台机器上,必须在**打开成功回来的
    /// 那一刻**记下(`accept_sftp_opened`),而不是发起时就写。
    ///
    /// 开 channel 是一次真实网络往返,期间用户完全可能又换了焦点分屏;
    /// 发起时写的话,`sftp_host_ix` 记的是「最后一次发起的意图」,而不是
    /// 「手上这条 client 的真实归属」——之后的比对全部错位,症状是换节点后
    /// 侧栏时对时不对,查都没法查。
    ///
    /// 扎的是源码结构:真验它要一条活 sftp 连接,这个测试容器里造不出来
    /// (同本文件其余几条接线守护)。
    ///
    /// 自证会变红:把 `t.sftp_host_ix = host_ix;` 从 `accept_sftp_opened`
    /// 挪进 `trigger_sftp_open`。
    #[test]
    fn the_sftp_host_is_recorded_when_the_channel_opens_not_when_it_is_requested() {
        let src = include_str!("app.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .expect("split 至少给一段");
        assert!(prod.len() < src.len(), "范围没切到 mod tests 之前");
        let at = prod
            .find("    fn accept_sftp_opened(")
            .expect("找不到 accept_sftp_opened");
        let body = &prod[at..];
        let end = body.find("\n    }\n").expect("找不到函数结尾");
        assert!(
            body[..end].contains("sftp_host_ix = host_ix"),
            "accept_sftp_opened 里没记 sftp_host_ix —— 换节点后的比对会永远错位"
        );
    }

    /// F132:「侧栏关→开」那一帧该做什么,三选一。这是这条改动里唯一测得动
    /// 的核心逻辑(`App` 本身要 `EventLoopProxy`,无头容器造不出来)。
    ///
    /// 自证会变红:把 `sync_plan_of` 里 host 比对那一支删掉 —— 第三条断言
    /// 会从 `Reopen` 变成 `Goto`,也就是回到「路径对了、机器错了」那个 bug。
    #[test]
    fn the_open_edge_reopens_sftp_only_when_the_focused_pane_is_on_another_host() {
        // 还没连上:什么都不做(`trigger_sftp_open` 那条路负责起步)。
        assert_eq!(
            sync_plan_of(false, None, Some(0), Some(b"/srv"), Some(b"/home/dev")),
            SyncPlan::Nothing
        );
        // 同一台:只同步目录。
        assert_eq!(
            sync_plan_of(true, Some(0), Some(0), Some(b"/srv"), Some(b"/home/dev")),
            SyncPlan::Goto("/srv".into())
        );
        // 换过节点的分屏:必须重开,否则连的是第一台机器、路径却来自第二台。
        assert_eq!(
            sync_plan_of(true, Some(0), Some(1), Some(b"/srv"), Some(b"/home/dev")),
            SyncPlan::Reopen
        );
        // SFTP 节点标签(没有终端,拿不到 host_ix):照旧只同步目录。
        assert_eq!(
            sync_plan_of(true, Some(0), None, Some(b"/srv"), Some(b"/home/dev")),
            SyncPlan::Goto("/srv".into())
        );
        // 同一台但 pane 没报过目录:什么都不做,不能把用户当前浏览的位置
        // 拽回一个猜出来的目录。
        assert_eq!(
            sync_plan_of(true, Some(0), Some(0), None, Some(b"/home/dev")),
            SyncPlan::Nothing
        );

        // 以下两条从退休的 `sync_target_of` 的测试搬过来 —— 它被本函数取代,
        // 但这两条语义在上面五条里**测不出来**:上面每条的 `pane_cwd` 都是
        // 绝对路径,`home` 传什么都不影响结果,把第五个参数整个吞掉照样全绿。

        // pane 报的是 `~` 而 home 还不知道(sftp 刚连上、还没 canonicalize):
        // 不展开、不同步。openssh 的 sftp-server 不认 `~`,发过去只会让面板
        // 停在「取不到登录目录」。
        assert_eq!(
            sync_plan_of(true, Some(0), Some(0), Some(b"~/Mullion"), None),
            SyncPlan::Nothing
        );
        // home 已知时必须**透传**给 `files_start_dir`,`~` 才展得开。少了这条,
        // 内部写死 `files_start_dir(pane_cwd, None, None)` 也不会有测试变红。
        assert_eq!(
            sync_plan_of(true, Some(0), Some(0), Some(b"~/x"), Some(b"/home/dev")),
            SyncPlan::Goto("/home/dev/x".into())
        );
    }

    /// **接线守护**:`sync_files_to_focused_pane` 要把存下来的登录目录喂给
    /// `sync_plan_of`。传死 `None` 的话这条路永远展不开 `~`。
    ///
    /// 自证会变红:把 `sync_plan_of` 的第五个实参改成字面量 `None`。
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
            .split("sync_plan_of(")
            .nth(1)
            .expect("没调 sync_plan_of");
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
                .expect("找不到 sync_plan_of 调用的结尾")
        };
        let args = &call[..end];
        assert!(
            args.contains("home"),
            "sync_plan_of 收到的不是登录目录:{args}"
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

    /// F131:路径条的编辑态必须算模态。**不算的话那个输入框里一个字都打不
    /// 出来** —— 面板拿着键盘焦点时,键不会喂给 egui
    /// (`input_route::egui_should_see_focused` 是 T8 的注入点),而是被
    /// `handle_panel_key` 吃掉:Backspace 变成「回上级目录」,字母键什么都
    /// 不做。这跟 `Modal::Editor` 当年踩的是同一个坑。
    ///
    /// 自证会变红:把 `Modal::FilesPathEdit` 从 `Modal::ALL` 里删掉
    /// (第二条断言红),或把 `modal_open` 里那一支改成 `=> false`
    /// (第三条断言红)。
    #[test]
    fn the_files_path_editor_is_a_modal_or_it_cannot_receive_a_single_keystroke() {
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
            prod.contains("    FilesPathEdit,"),
            "Modal 枚举里没有 FilesPathEdit"
        );
        assert!(
            prod.contains("        Modal::FilesPathEdit,"),
            "Modal::ALL 里漏了 FilesPathEdit —— modal_open 照 ALL 遍历,\
             漏加等于这一支从来不生效"
        );
        assert!(
            prod.contains("            Modal::FilesPathEdit => self.files_path_editing(),"),
            "modal_open 没有认 FilesPathEdit"
        );
    }

    /// 取某个函数的函数体源码。**`marker` 必须带行首缩进**——不带的话
    /// `include_str!` 出来的源码里会先匹配到测试自己写的那个字符串字面量,
    /// 断言就变成了「测试自我匹配」,永远绿(本项目已实证的第五类恒绿模式)。
    ///
    /// **隐含前提:`cargo fmt --check` 已经过。** `find("\n    }\n")` 靠
    /// rustfmt 保证「方法自己的收尾 `}` 独占一行且缩进恰好 4 空格,内部嵌套块
    /// (`if let`/`match`/`async move` 等)的收尾缩进一律深于 4 空格」——否则
    /// 函数体内部一个巧合缩进到 4 空格的 `}` 会把这里截断在错误的位置。日后
    /// 在别处复用这个辅助函数前,先确认这个前提仍然成立。
    fn fn_body<'a>(src: &'a str, marker: &str) -> &'a str {
        assert!(
            marker.starts_with("    "),
            "锚点必须带行首缩进,否则测试自我匹配恒绿"
        );
        let after = src
            .split(marker)
            .nth(1)
            .unwrap_or_else(|| panic!("找不到 {marker}"));
        &after[..after.find("\n    }\n").expect("找不到函数结尾")]
    }

    /// **接线守护 / F128**:重连的凭据必须是建这条连接那一刻定死的那份,
    /// **不许回头查库**。查库的话,用户在断线期间改了这条会话(换了端口/密钥),
    /// 重连就会拨到一个他没同意过的地方去;会话被删掉时更是直接连不上。
    /// 理由同 `PendingRehost` / `automation_template`。
    ///
    /// 自证会变红:把 `spawn_reconnect` 里取 cfg 那句换成 `store.dial_plan_for(..)`。
    ///
    /// 锚点拆开拼(`concat!`):写成完整字面量的话,`fn_body` 在 0 处真实定义时
    /// 会退化成匹配测试自己这行代码,把自己的函数体(而非生产代码)当成
    /// `body`——已实测踩中过一次(第五类恒绿模式的变体:锚点自我匹配)。
    #[test]
    fn reconnect_uses_the_cfg_frozen_at_connect_time() {
        let src = include_str!("app.rs");
        let body = fn_body(src, concat!("    fn ", "spawn_reconnect("));
        assert!(
            !body.contains("dial_plan_for"),
            "重连回头查库了 —— 见 PendingRehost 的文档"
        );
        assert!(
            !body.contains("self.store"),
            "重连回头查库了 —— 见 PendingRehost 的文档"
        );
    }

    /// **接线守护 / F128**:重连要拨的是**断掉的那台机器**,不是标签最初连的
    /// 那台。
    ///
    /// `TerminalTab::last_cfg` 在 `ConnectOk` 那一刻定死、此后再也不更新,
    /// 而「换节点」(`PaneRehosted`)会往 `ws.hosts` 里加**第二台机器**。
    /// 拿 `last_cfg` 去重拨 `host_ix > 0` 的那条连接,就是静默连到另一台机器
    /// 上——地址、凭据、主机指纹全对不上,而用户看到的只是「已重新连接」,
    /// 接着往一台他没打算登录的服务器上敲命令。
    ///
    /// 结构性守护(`spawn_reconnect` 要 `&mut App` + 真实 runtime,无头环境
    /// 造不出来)。**关键是那条否定断言**:光断言"读了 hosts"挡不住
    /// 「两个来源都留着、`unwrap_or` 落回 `last_cfg`」这种写法,而那正是最
    /// 自然的"稳妥"改法,也正好把 bug 原样保留。
    ///
    /// 锚点拆开拼,理由同 `reconnect_uses_the_cfg_frozen_at_connect_time`。
    #[test]
    fn reconnect_dials_the_host_it_lost_not_the_first_one() {
        let src = include_str!("app.rs");
        let body = fn_body(src, concat!("    fn ", "spawn_reconnect("));
        let code: String = body
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            code.contains("hosts.get(host_ix)"),
            "重连没有按 host_ix 取拨号参数 —— 换过节点之后会拨回最初那台机器"
        );
        assert!(
            !code.contains("last_cfg"),
            "重连仍读得到标签级 last_cfg —— 只要它还在这个函数里,\
             「取不到就落回 last_cfg」这种写法就会把 bug 原样带回来"
        );
    }

    /// **接线守护 / F128**:重连成功要走 `reattach_pane`(保留内容),
    /// 不是 `rehost_pane`(重建 emulator)。走错的现象是「重连之后屏是空的」,
    /// 而用户最想看的恰恰是断线前那一屏。
    ///
    /// 锚点拆开拼(`concat!`),理由同 `a_failed_rehost_takes_its_host_back_out_of_the_list`:
    /// 写成完整字面量的话,这条测试自己的源码里也有一份,会被自己匹配到
    /// (第四类恒绿模式:源码级测试自我匹配)。**这条文档注释本身也不能写出
    /// 完整字面量**——第一版这里写了,反而成了全文件里最后一次出现,
    /// `rsplit(..).next()` 就切到了这段注释而不是真正的 match 分支,已实证。
    /// 用 `rsplit(..).next()` 取**最后一次**出现——`spawn_reconnect` 里发事件那次
    /// 出现在前面,`user_event` 里的 match 分支出现在后面,取最后一次才是
    /// 真正要测的分支。
    ///
    /// 自证会变红:把处理分支里的 `reattach_pane(` 改成 `rehost_pane(`。
    #[test]
    fn reconnect_reattaches_instead_of_rehosting() {
        let raw = body_of(prod_src(), "fn on_pane_reconnected(");
        // 注释行剥掉:分支上方/内部的解释性注释可能提到同样的字样,不剥的话
        // 删掉真正的调用测试也可能照绿。
        let body: String = raw
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(body.contains("reattach_pane("), "重连必须保留屏内容");
        assert!(
            !body.contains("rehost_pane("),
            "走了换机器那条路,内容会被抹掉"
        );
    }

    /// F141:重连的「接回 tmux」只认**当初那块 pane、在当初那台机器上**。
    ///
    /// 第二条断言是这条判据存在的全部理由:用户把主 pane「换节点」搬到第二台
    /// 机器之后,pane id 不变、`host_ix` 变了。只认 pane id 的话,新机器断线
    /// 重连时会把**上一台**机器的 tmux 会话名发过去,在一台根本没有那个会话
    /// 的机器上凭空新建一个同名会话 —— 用户会以为「会话没了」。
    ///
    /// F141:`ConnectOk` 记下的会话名必须**就是**这次真的 attach 上去的那个,
    /// 而且钉在建标签的那块 pane、那台 host 上。
    ///
    /// 三件事错了都看不出来,直到某天断线重连接到别的会话上(或者在一台
    /// 没有那个会话的机器上凭空新建一个)。
    ///
    /// 自证会变红(各自):把 `tmux_attach_for_connect` 里的 `PaneId(1)` 改成
    /// `PaneId(2)`;把 `host_ix: 0` 改成 `1`;把 `tmux_session_name(..)` 换成
    /// `Some(fallback_name?.to_string())`(漏掉 sanitize 与「显式名优先」)。
    #[test]
    fn the_connect_records_exactly_the_tmux_session_the_plan_attaches() {
        let auto = mullion_store::ResolvedAutomation {
            enabled: true,
            tmux: Some(mullion_store::TmuxChoice::Attach { session_name: None }),
            commands: Vec::new(),
            work_dir: None,
            env: Vec::new(),
            initial_delay_ms: 300,
            inter_delay_ms: 200,
            ready_timeout_ms: 15_000,
        };
        let it = tmux_attach_for_connect(Some(&auto), Some("web01.prod:2"))
            .expect("配了 tmux 就该记下来");
        assert_eq!(it.pane, PaneId(1), "记的是建标签的那块 pane");
        assert_eq!(it.host_ix, 0, "记的是这条连接的第一台 host");
        // 计划里真的发出去的名字 —— 拿它当参照,而不是自己再拼一遍
        // (自己拼的话,sanitize 规则改了两边一起错,测试照绿)。
        let line = String::from_utf8(
            mullion_store::build_plan(&auto, "web01.prod:2")[0]
                .bytes
                .clone(),
        )
        .expect("ASCII");
        assert!(
            line.contains(&format!("-t '{}'", it.session_name)),
            "记下的会话名跟计划实际 attach 的那个对不上: 记的是 {} / 计划是 {line}",
            it.session_name
        );

        // 没走 tmux 的三种情形都不许记 —— 记了的话重连会凭空 attach 一个
        // 当初根本没建起来的会话。
        let mut off = auto.clone();
        off.tmux = Some(mullion_store::TmuxChoice::Off);
        assert!(tmux_attach_for_connect(Some(&off), Some("web01")).is_none());
        assert!(
            tmux_attach_for_connect(None, Some("web01")).is_none(),
            "没有模板(比如 CLI 直连)时无从谈起"
        );
        assert!(
            tmux_attach_for_connect(Some(&auto), None).is_none(),
            "没有会话名时同理"
        );
    }

    /// F161:`TmuxAttach::matches` 今天**只用在回落路径上**——真值源换成了
    /// 实测(`p.tmux`)之后,「接回哪个会话」不再由它判定,它只在某块 pane
    /// 实测不到 tmux 名时,负责把回落名钉在「当初那块 pane、那台机器」上,
    /// 不让别的 pane 借用错这份回落。这条测试今天守的是**回落路径**的
    /// 正确性,不是主路径。
    ///
    /// 自证会变红:把 `TmuxAttach::matches` 里的 `&& self.host_ix == host_ix`
    /// 删掉。
    #[test]
    fn only_the_pane_that_attached_tmux_on_that_very_host_gets_reattached() {
        let it = TmuxAttach {
            pane: PaneId(1),
            host_ix: 0,
            session_name: "web01".into(),
        };
        assert!(it.matches(PaneId(1), 0), "就是它,该接回去");
        assert!(
            !it.matches(PaneId(2), 0),
            "分屏出来的 pane 不许 attach 同一个会话(会内容镜像)"
        );
        assert!(
            !it.matches(PaneId(1), 1),
            "换过节点之后它已经在别的机器上了,那台没有这个会话"
        );
    }

    /// spec §1.3 那条**既有 bug** 的回归守护。
    ///
    /// `build_plan_reattach` 的会话名判据是 `tmux_session_name(配置)`,配置没配
    /// tmux 时返回空计划 —— 用户的 tmux 是在远端手敲 `tt web01` 进去的,配置里
    /// 根本没有那个名字,所以**今天断线自动重连也接不回 tmux**(不只是新开 exe)。
    ///
    /// 真值源换成实测(`PaneState::tmux`,`reattach_pane` 刻意保留了它),
    /// 配置只在实测为空时回落(D1)。
    ///
    /// 自证会变红:把 `on_pane_reconnected` 里那句读 `p.tmux` 的代码删掉、
    /// 改回只认 `tmux_attach`。
    #[test]
    fn the_reattach_path_reads_the_measured_name_not_the_configured_one() {
        let raw = body_of(prod_src(), "fn on_pane_reconnected(");
        // 注释行剥掉:上面那句文档注释本身就提到了 `p.tmux` 和 `tmux_attach`
        // 两个词,不剥的话这条测试会去比较注释里两个词的先后顺序,跟真正
        // 该测的「代码读的是哪个字段」毫无关系——已实证:只删代码里的
        // `.and_then(|p| p.tmux.clone())` 而不动注释,这条测试原样照绿。
        let body: String = raw
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let measured = body
            .find("p.tmux")
            .expect("重连分支没有读实测的 tmux 名 —— 手敲进 tmux 的用法接不回来");
        let configured = body.find("tmux_attach").unwrap_or(usize::MAX);
        assert!(
            measured < configured,
            "实测名必须**优先于**配置名(D1),现在顺序反了:\n{body}"
        );
    }

    /// F141 的语义没变,只是真值源换了(F161/D1):断线重连回来的 pane 要真的
    /// **接回原会话**,而不是「在一块新 shell 里把 cd/export 重跑一遍」。
    ///
    /// 判据从「分支里调了 `pending_for_reattach`」换成「分支把实测名写进了
    /// `leaf_wanted`」—— 后者是 `on_pane_ready` 决定发不发 attach 的唯一依据。
    /// 另两条断言(重连要重跑登录后命令、分屏 pane 仍跳过 tmux)沿用旧测试,
    /// 它们守的是没变的那部分性质。
    ///
    /// 自证会变红:把 `on_pane_reconnected` 里那段写 `leaf_wanted` 的代码删掉。
    #[test]
    fn reconnecting_still_reattaches_the_original_tmux_session() {
        let body = body_of(prod_src(), "fn on_pane_reconnected(");
        assert!(
            body.contains("leaf_wanted.push("),
            "重连分支没有登记「该接回哪个会话」,断线前的 tmux 会话回不来:\n{body}"
        );
        assert!(
            body.contains("self.on_pane_ready("),
            "没重跑登录后命令 —— tmux 不 attach,Claude Code 会话回不来"
        );
        assert!(
            body.contains(concat!("pending_for_", "extra_pane(")),
            "其余 pane(分屏出来的)仍必须走跳过 tmux 那条,否则内容会镜像"
        );
    }

    /// **接线守护 / F128**:重连成功要把这个标签的 SFTP 侧栏运行态清掉。
    /// 旧 `SftpClient` 挂在**已经死掉的那条连接**上,留着的话侧栏每次操作都
    /// 静默失败,而用户看到的是「文件面板卡住了」。
    ///
    /// 锚点拆开拼,理由同上一条。
    ///
    /// 自证会变红:把 `PaneReconnected` 分支里 `t.sftp = None;` 那句删掉。
    #[test]
    fn reconnect_drops_the_dead_sftp_client() {
        let raw = body_of(prod_src(), "fn on_pane_reconnected(");
        let body: String = raw
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            body.contains("t.sftp = None;"),
            "死掉的 SftpClient 必须丢掉"
        );
        assert!(body.contains("t.sftp_home = None;"));
    }

    /// **接线守护 / F128**:重连必须**就地替换** `hosts[host_ix]` 的 handle,
    /// 绝不往 `ws.hosts` 里 push 一条新的。
    ///
    /// push 的写法会让 `hosts[0]` 不再是这个标签的主连接,而认着这个下标的
    /// 地方一个都不会跟着走:`TabContent::sftp_connection`(文件面板)、
    /// `spawn_fresh_panes`(分屏开 channel)、`PaneOpened` 里硬编的
    /// `host_ix: 0` 全都会继续指向刚断掉的那条**死**连接。症状是主链路
    /// 自动重连之后文件面板永久打不开(`t.sftp = None` 让它每次重试、每次
    /// 失败)、新开的分屏必然失败,而终端本身工作正常 —— 用户完全看不出成因。
    /// 顺带:push 还会让 `hosts` 随每次断线单调增长,长期挂机攒下一堆死连接。
    ///
    /// 那条否定断言是这条测试的重点:光断言"有 get_mut"挡不住「两种都写、
    /// 某个分支仍 push」。
    ///
    /// 锚点拆开拼,理由同 `reconnect_drops_the_dead_sftp_client`。
    ///
    /// 自证会变红:把就地替换那几句换回 `t.ws.hosts.push(HostConn { .. })`。
    #[test]
    fn reconnect_swaps_the_host_in_place_instead_of_appending_a_new_one() {
        let raw = body_of(prod_src(), "fn on_pane_reconnected(");
        let body: String = raw
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            body.contains("hosts.get_mut(host_ix)") && body.contains("h.handle = handle;"),
            "重连没有就地替换 hosts[host_ix] 的 handle"
        );
        assert!(
            !body.contains("hosts.push("),
            "重连往 hosts 里 push 了新连接 —— hosts[0] 从此指向死连接,\
             文件面板和分屏会静默挂在上面"
        );
        assert!(
            body.contains("h.tmux_bootstrap = Default::default();"),
            "换了连接却没重置 tmux 自举状态 —— 远端 tmux 服务器可能在断线\
             期间重启过,状态上报再也配不上"
        );
    }

    /// **接线守护 / F128**:重连回来必须把这条 host 从 `reconnecting` 里摘掉。
    ///
    /// `reconnecting` 的语义是「这一帧不要再为它发起拨号」(判据在
    /// `reconnect::hosts_to_redial`,60fps 下不去重就是一秒六十条连接)。
    /// 拨号成功后不摘,这条 `(generation, host_ix)` 就**永远**留在表里,
    /// `hosts_to_redial` 从此对它一律跳过 —— 症状是**第一次断线能自动重连,
    /// 之后再断就再也不重连了**,而且没有任何报错:标题条显示 Reconnecting,
    /// 一直转到用户手动点重连为止。用户感知是「自动重连时灵时不灵」。
    ///
    /// 这条是重构期补的:变异测试(删掉 `retain` 那两行)实测**全绿放行**,
    /// 1901 个测试没有一个抓得住。
    ///
    /// 锚点拆开拼,理由同 `reconnect_drops_the_dead_sftp_client`。
    ///
    /// 自证会变红:删掉分支开头的 `self.reconnecting.retain(...)` 那两行。
    #[test]
    fn a_finished_reconnect_is_taken_out_of_the_in_flight_table_so_the_next_drop_redials() {
        let raw = body_of(prod_src(), "fn on_pane_reconnected(");
        let body: String = raw
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            body.contains("self.reconnecting"),
            "重连回来没碰 reconnecting 表 —— 这条 host 永远停在「拨号在途」,\
             以后再断线 hosts_to_redial 一律跳过,自动重连永久失效"
        );
        assert!(
            body.contains(".retain(|(g, h, _)| !(*g == generation && *h == host_ix));"),
            "摘除条件不是「同世代同 host」—— 条件写宽了会把别的标签仍在途的\
             拨号一起摘掉(那条会被重复发起),写窄了等于没摘"
        );
    }

    /// **接线守护 / F128**:拨号那几秒里这条连接上的 pane 全被用户关掉了
    /// (`attached` 为空)时,**不能**把新连接装进 `hosts` —— 一条谁也不指的
    /// `Arc<SshConnection>` 会一直占到标签关闭为止,完全静默。
    /// 同一时刻也不该弹「已重新连接」的 toast —— 用户什么都没得到。
    ///
    /// 锚点拆开拼,理由同 `reconnect_drops_the_dead_sftp_client`。
    ///
    /// 自证会变红:把就地替换从 `else if` 里提出来,改成无条件执行;
    /// 或者把 `set_toast` 挪出 `if reconnected { ... }`。
    #[test]
    fn reconnect_keeps_the_new_connection_out_when_every_pane_vanished_mid_dial() {
        let raw = body_of(prod_src(), "fn on_pane_reconnected(");
        let body: String = raw
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            body.contains("if attached.is_empty() {")
                && body.contains("} else if let Some(h) = t.ws.hosts.get_mut(host_ix) {"),
            "替换没有挂在「真的有 pane 接上」这个前提下 —— 一块 pane 都没接上时\
             也会把新连接装进 hosts,占着不关"
        );
        assert!(
            body.contains("if reconnected {"),
            "toast 没有按「真的有 pane 接上」收敛,零个 pane 接上也会弹「已重新连接」"
        );
    }

    /// **接线守护 / F128**:重连成功之后必须把「没赶上这次拨号」的 pane 捞回
    /// `Reconnecting`。
    ///
    /// 同一条 SSH 连接上每块 pane 有各自的读取任务,传输层死了之后它们的 `rx`
    /// **不保证同一帧关闭**(缓冲里的字节要先排完)。慢一步的那块在
    /// `hosts_to_redial` 拍板时还是 `Live`,不会进这次拨号的名单,于是
    /// `channels` 里没有它 —— 它攥着一条已经死掉的旧 channel,写入静默失败,
    /// 而标题条上一切正常。更糟的是就地替换之后 `hosts[host_ix]` 变活了,
    /// 它下一帧真关闭时 `rx_closed_action(link_alive == true)` 会判成
    /// `UserExited`,直接钉死成 `Disconnected`,**再也不会重连**。
    ///
    /// 锚点拆开拼,理由同 `reconnect_drops_the_dead_sftp_client`。
    ///
    /// 自证会变红:把 `strays_after_reconnect` 那个 for 循环删掉,或者把循环体
    /// 里的 `p.status = ...Reconnecting;` 删掉。
    #[test]
    fn reconnect_picks_up_the_panes_that_missed_this_dial() {
        let raw = body_of(prod_src(), "fn on_pane_reconnected(");
        let body: String = raw
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            body.contains("strays_after_reconnect("),
            "重连之后没有回捞漏网的 pane —— 慢一步关闭的那块会被钉死成 \
             Disconnected,永久不再重连"
        );
        assert!(
            body.contains("p.status = crate::shell::workspace::PaneStatus::Reconnecting;"),
            "捞出来了却没把状态置回 Reconnecting —— drive_reconnects 不会收走它"
        );
    }

    /// **接线守护 / F128**:`drive_reconnects` 必须遍历所有标签,不能只驱动
    /// 活动标签——理由同 `drive_automation`(见其文档注释):用户完全可能
    /// 开着标签 A 连了台机器就切去标签 B,只驱动活动标签的话标签 A 断线要等
    /// 用户切回去才会开始重拨,用户「尽快恢复重连」的诉求就落空了。
    ///
    /// 锚点拆开拼(`concat!`),理由同 `reconnect_uses_the_cfg_frozen_at_connect_time`。
    ///
    /// 自证会变红:把 `self.tabs.iter()` 换回 `self.active_term()`。
    #[test]
    fn drive_reconnects_walks_every_tab_not_just_the_active_one() {
        let src = include_str!("app.rs");
        let body = fn_body(src, concat!("    fn ", "drive_reconnects(&mut self) {"));
        assert!(
            !body.contains("active_term"),
            "drive_reconnects 只驱动了活动标签 —— 后台标签断线要等用户切回去\
             才会开始重拨"
        );
    }

    /// **接线守护 / F209**:裸 `Ctrl+V` 的判定同样要排在 `encode_key`
    /// **之前**。挪到之后就是 `0x16` 先发下去,截图那条路整条作废,而
    /// `shot::clip_paste` 的纯函数单测全是绿的 —— 与 F129 那条同一类静默。
    ///
    /// 锚点拆开拼,理由同上一条(第五类恒绿模式)。
    ///
    /// 自证会变红:把 F209 那段接线整体挪到 `encode_key(...)` 之后。
    #[test]
    fn the_bare_ctrl_v_is_decided_before_the_key_gets_encoded() {
        let src = include_str!("app.rs");
        let start = concat!("WindowEvent::Keyboard", "Input { event, .. } => {");
        let after = src.split(start).nth(1).expect("找不到 KeyboardInput 分支");
        let body = &after[..after
            .find(concat!("WindowEvent::Redraw", "Requested"))
            .expect("找不到 KeyboardInput 分支的结尾")];
        let clip_at = body
            .find(concat!("clip_", "paste(text.is_some()"))
            .expect("找不到 F209 的 Ctrl+V 接线");
        let encode_at = body
            .find(concat!("encode_", "key(key, mods"))
            .expect("找不到 encode_key 调用");
        assert!(
            clip_at < encode_at,
            "裸 Ctrl+V 的判定跑到 encode_key 后面了 —— 它会被编成 0x16 发下去,\
             截图直传永远走不到"
        );
    }

    /// **F209:失败时一个字节都不许进终端。**
    ///
    /// 判据扎在函数体的顺序上:`Err` 那一支必须在任何 `pty.write` 之前
    /// `return`。写成「有没有 `set_error`」是抓不住的 —— 把 `return` 删掉
    /// 之后提示照弹,而用户的输入行里会多出一段他没打过的半截路径,还得
    /// 自己删掉。
    ///
    /// 自证会变红:把 `accept_shot_uploaded` 里 `Err` 分支的 `return;` 删掉。
    #[test]
    fn a_failed_screenshot_upload_never_writes_to_the_terminal() {
        let src = include_str!("app.rs");
        let body = fn_body(src, concat!("    fn ", "accept_shot_uploaded("));
        assert!(body.len() > 400, "函数体切歪了({} 字节)", body.len());
        let err_at = body.find("Err(msg) => {").expect("失败分支不见了");
        let write_at = body.find("p.pty.write(out)").expect("成功分支不写终端了?");
        assert!(err_at < write_at, "锚点顺序反了,这条测试的前提不成立");
        assert!(
            body[err_at..write_at].contains("return;"),
            "失败分支没有 return —— 半截路径会被打进用户的输入行"
        );
    }

    /// **F209:路径回给发起它的那块 pane,不是「此刻的焦点」。**
    ///
    /// 高延迟链路上传一张图要好几秒,用户完全可能已经切标签、切分屏。拿
    /// 当前焦点接的话,那串路径会凭空出现在**另一台机器**的 shell 里 ——
    /// 而且没有任何报错(T11 同族:判据跟着字节走,不跟着「谁在焦点」走)。
    ///
    /// 自证会变红:把 `by_generation_mut(generation)` 换成 `active_term_mut()`
    /// 、把 `ws.pane_mut(pane)` 换成 `ws.focused_mut()`。
    #[test]
    fn the_screenshot_path_goes_back_to_the_pane_that_asked_for_it() {
        let src = include_str!("app.rs");
        let body = fn_body(src, concat!("    fn ", "accept_shot_uploaded("));
        assert!(
            body.contains("by_generation_mut(generation)") && body.contains("ws.pane_mut(pane)"),
            "回填不是按 generation + PaneId 路由的"
        );
        assert!(
            !body.contains("active_term")
                && !body.contains("active_ws")
                && !body.contains("focused_mut"),
            "回填走了「当前活动标签/焦点分屏」—— 用户切走之后路径会打进别的 shell"
        );
    }

    /// **F209:体积闸在发字节之前。**
    ///
    /// 闸放到 task 里(或者干脆没有)的话,一张几百 MB 的图会在没有进度条的
    /// 情况下慢慢传,用户看到的只是程序「卡住了」——这条路是一次按键的副作用,
    /// 不是文件面板的传输队列,没有任何地方能显示它的进度或让人取消。
    ///
    /// 自证会变红:把 `paste_screenshot` 里那段 `MAX_PNG_BYTES` 判断删掉,
    /// 或挪到 `self._runtime.spawn` 之后。
    #[test]
    fn an_oversized_screenshot_is_rejected_before_anything_goes_out() {
        let src = include_str!("app.rs");
        let body = fn_body(src, concat!("    fn ", "paste_screenshot(&mut self"));
        assert!(body.len() > 400, "函数体切歪了({} 字节)", body.len());
        let gate_at = body.find("MAX_PNG_BYTES").expect("体积闸不见了");
        let spawn_at = body
            .find(concat!("_runtime", ".spawn("))
            .expect("找不到上传 task");
        assert!(gate_at < spawn_at, "体积闸排在上传之后 —— 等于没有闸");
    }

    /// **接线守护 / F129**:Ctrl+D 的判定必须排在 `encode_key` **之前**。
    /// 挪到之后的后果是静默的:Ctrl+D 先被编成 `0x04` 写进一条死 channel
    /// (T1 式静默失败),而 `ctrl_d_action` / `is_bare_ctrl_d` 的纯函数单测
    /// 全都还是绿的,没人会发现这个功能已经废了。
    ///
    /// 锚点全部拆开拼:完整字面量会被这条测试自己的源码匹配到,那是本项目
    /// 已实证的第五类恒绿模式。
    ///
    /// 自证会变红:把 F129 那段接线整体挪到 `encode_key(...)` 之后。
    #[test]
    fn ctrl_d_is_decided_before_the_key_gets_encoded() {
        let src = include_str!("app.rs");
        let start = concat!("WindowEvent::Keyboard", "Input { event, .. } => {");
        let after = src.split(start).nth(1).expect("找不到 KeyboardInput 分支");
        let body = &after[..after
            .find(concat!("WindowEvent::Redraw", "Requested"))
            .expect("找不到 KeyboardInput 分支的结尾")];
        let ctrl_d_at = body
            .find(concat!("is_bare_", "ctrl_d("))
            .expect("找不到 Ctrl+D 接线");
        let encode_at = body
            .find(concat!("encode_", "key(key, mods"))
            .expect("找不到 encode_key 调用");
        assert!(
            ctrl_d_at < encode_at,
            "Ctrl+D 的判定跑到 encode_key 后面了 —— 它会被编成 0x04 写进死 channel"
        );
    }

    /// F132:重开之前必须**先摘掉旧 client**,再调 `trigger_sftp_open`。
    ///
    /// 顺序反了是静默失败:`trigger_sftp_open` 开头就有
    /// `if tab.content.sftp_client().is_some() { return; }`,旧 client 还挂着
    /// 的话它直接早退,一个字节都不会发 —— 用户看到的是「按了没反应,侧栏
    /// 还是连着另一台机器」。
    ///
    /// 同理**不能提前把面板置成加载中**:`trigger_sftp_open` 的另一个
    /// 早退条件正是 `already_loading`。
    ///
    /// 扎的是源码结构(要活连接才验得了真行为)。锚点带行首缩进。
    ///
    /// 自证会变红:把 `reopen_sftp_on_focused_host` 里那句
    /// `*slot = None;` 删掉,或在它里面把面板置成加载中。
    #[test]
    fn reopening_sftp_drops_the_old_client_before_asking_for_a_new_one() {
        let src = include_str!("app.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .expect("split 至少给一段");
        assert!(prod.len() < src.len(), "范围没切到 mod tests 之前");
        let at = prod
            .find("    fn reopen_sftp_on_focused_host(")
            .expect("缺 reopen_sftp_on_focused_host —— 换节点后侧栏不会跟过去");
        let body = &prod[at..];
        let end = body.find("\n    }\n").expect("找不到函数结尾");
        let body = &body[..end];
        let drop_at = body.find("*slot = None;").expect("没摘掉旧 client");
        let call_at = body
            .find("self.trigger_sftp_open(generation);")
            .expect("没调 trigger_sftp_open");
        assert!(
            drop_at < call_at,
            "先调了 trigger_sftp_open 才摘旧 client —— 它会在开头早退,静默失败"
        );
        assert!(
            !body.contains("Load::Loading"),
            "提前把面板置成加载中会撞上 trigger_sftp_open 的 already_loading 早退"
        );
        // 复核挖出的两个 Critical,判据钉在这里。
        assert!(
            !body.contains(".abort()"),
            "重开 sftp 不许 abort `sftp_tasks` —— 那个池子里混着传输 worker\
             (被杀就永远发不出 TransferDone,队列里的 job 永久占并发名额)和\
             列目录任务(被杀就永远翻不出 Loading,下一句 trigger_sftp_open 早退)"
        );
        // 判据带 `remote.` 限定:两栏都是 `PaneState`、都有 `invalidate()`,
        // 就在同一个 `if let Some(files)` 块里。手滑写成 `local` 的话远端栏
        // 保持旧机器的 `Loading`,原样复发下面那条早退 —— 而只扫函数名的话
        // 这个手滑一条测试都不会红(复核实测 1262 条全绿)。
        assert!(
            body.contains("files.remote.invalidate()"),
            "没作废远端栏 —— load 留在 Loading 会让紧接着的 trigger_sftp_open\
             撞 already_loading 早退,而那之后 has_client 是 false、判定首行就\
             短路,没有任何自愈路径"
        );
    }

    /// F128:断线重连把死连接上的 sftp 任务 abort 掉之后,**两处状态必须各自
    /// 收口**,否则重连成功了侧栏也回不来。
    ///
    /// - `cancel_transfers_of`:被硬杀的传输 worker 再也发不出 `TransferDone`,
    ///   队列里那几条永久停在 `Running`,而 `take_runnable` 按 `Running` 数占
    ///   并发名额(默认 4)—— 断几次线就把全局传输堵死,只能重启。
    /// - `files.remote.invalidate()`:被硬杀的列目录任务翻不动 `PaneState::load`
    ///   (只有 `accept` 翻得动),留在 `Loading` 的话之后每次 `trigger_sftp_open`
    ///   都撞 `already_loading` 早退,一个字节都不发,侧栏**永久**打不开。
    ///
    /// 与 F132 的 `reopen_sftp_on_focused_host` 是同一类失效、不同处置:那边
    /// 连接还活着(只是换台机器),所以**不** abort、让传输跑完;这边连接真
    /// 没了,abort 是对的,但得把两处状态收干净。
    ///
    /// 扎源码结构(要真断线才验得了)。
    ///
    /// 自证会变红:删掉 `self.cancel_transfers_of(generation);` 或
    /// `t.files.remote.invalidate();` 任一句。
    #[test]
    fn a_reconnect_settles_both_the_transfer_queue_and_the_remote_pane() {
        let body = body_of(prod_src(), "fn on_pane_reconnected(");
        assert!(
            body.contains("sftp_tasks.drain(..)"),
            "锚点失效:这个分支已经不 abort sftp 任务了,下面两条断言随之失去意义,\
             该重新想清楚收口该怎么做"
        );
        assert!(
            body.contains("self.cancel_transfers_of(generation);"),
            "abort 了传输 worker 却没让队列收口 —— job 永久停在 Running,\
             按 Running 数算的并发名额被吃掉,断几次线全局传输就堵死"
        );
        assert!(
            body.contains("files.remote.invalidate()"),
            "abort 了列目录任务却没作废远端栏 —— load 永远停在 Loading,\
             之后每次 trigger_sftp_open 都撞 already_loading 早退,侧栏永久打不开"
        );
    }

    /// F132:判定说要 `Reopen`,`sync_files_to_focused_pane` 就得真的去重开。
    ///
    /// 这一臂写成空操作的话,编译过、全部单测绿,实机行为却原样退回这条改动
    /// 要修的 bug(侧栏连着第一台、路径却来自第二台)—— 两位复核各自独立
    /// 把它改成 `SyncPlan::Reopen => {}` 实测过 1260 条全绿,缺口是真的。
    ///
    /// 判定(`sync_plan_of`)和执行(`reopen_sftp_on_focused_host`)各有测试,
    /// 唯独中间这根线没人守;`App` 造不出来,只能扎源码。
    ///
    /// 自证会变红:把那一臂改成 `SyncPlan::Reopen => {}`。
    #[test]
    fn deciding_to_reopen_actually_calls_the_reopen_path() {
        let src = include_str!("app.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .expect("split 至少给一段");
        assert!(prod.len() < src.len(), "范围没切到 mod tests 之前");
        let at = prod
            .find("    fn sync_files_to_focused_pane(&mut self) {")
            .expect("找不到 sync_files_to_focused_pane 的定义");
        let body = &prod[at..];
        let end = body.find("\n    }\n").expect("找不到函数结尾");
        let arm = body[..end]
            .find("SyncPlan::Reopen =>")
            .expect("没处理 Reopen —— 换过节点的分屏永远等不到侧栏跟过去");
        assert!(
            body[arm..end].contains("self.reopen_sftp_on_focused_host("),
            "Reopen 那一臂没调 reopen_sftp_on_focused_host"
        );
    }

    /// F149:账本必须在 `handle_platform_output` **之前**按住。
    ///
    /// 顺序错了这个修复完全失效 —— 那个函数读的是调用当时的账本,之后再改
    /// 一点用都没有 —— 而且照样编译、测试照样能跑,只有实机打不出中文才暴露。
    ///
    /// 锚点**必须带行首换行 + 缩进**:不带的话会匹配到本测试自己那一行
    /// (`include_str!("app.rs")` 读的就是这个文件),`find` 拿到测试的位置,
    /// 断言变成拿测试自己跟实现比,恒绿。这里的字面量里 `\n` 是转义序列,
    /// 测试自身那一行含的是反斜杠加 n 两个字符,匹配不上真换行,是安全的。
    #[test]
    fn the_ime_ledger_is_clamped_before_egui_gets_a_chance_to_disable_it() {
        let src = include_str!("app.rs");
        let clamp = src
            .find("\n    if let Some(v) = input::ime_ledger_clamp(")
            .expect("找不到 F149 的账本按压接线");
        let hpo = src
            .find("\n        .handle_platform_output(")
            .expect("找不到 handle_platform_output 的调用");
        assert!(
            clamp < hpo,
            "ime_ledger_clamp 必须排在 handle_platform_output 之前,否则账本改了也没人读,\
             中文输入照样会被 egui 关掉"
        );
    }

    // ------------------------------------------------ F17 scrollback 接线

    /// 造一个只有一条会话的 store,那条会话的 `scrollback` 由参数指定。
    fn store_with_scrollback(
        dir: &std::path::Path,
        scrollback: Option<u32>,
    ) -> (crate::shell::store::SessionStore, SessionId) {
        let mut store = crate::shell::store::SessionStore::open(
            dir.to_path_buf(),
            &mullion_store::InMemoryKey([1u8; 32]),
        )
        .expect("开 store");
        let draft = mullion_store::SessionDraft {
            identity: mullion_store::Identity {
                name: "dev".into(),
                note: String::new(),
                group_id: None,
                tags: Vec::new(),
            },
            connection: mullion_store::Connection {
                host: "192.0.2.10".into(),
                port: 22,
                protocol: mullion_store::Protocol::Ssh,
            },
            auth: mullion_store::Auth::inline("user", mullion_store::AuthKind::Password),
            terminal: mullion_store::TerminalPrefs { scrollback },
            appearance: Default::default(),
            network: Default::default(),
            automation: Default::default(),
            sftp: Default::default(),
            secret: None,
        };
        let id = store.add(draft, "2026-08-21T00:00:00Z");
        (store, id)
    }

    /// F17:用户配的回溯行数必须真的抵达 `Emulator`。
    ///
    /// 这条线过去整个是断的:store 那一头(字段 + 继承 + 迁移)和 UI 那一头
    /// 各由一个切片做完,中间从来没接上 —— 三个注入点全写死
    /// `Emulator::new(80, 24)`,恒取内置默认。**恒取默认也能让别的测试全绿**,
    /// 所以必须有一条专门盯着"取到的是配置值"。
    ///
    /// 自证会变红:把 `resolved_scrollback` 里的 `s.resolved(id).ok()` 换成 `None`。
    #[test]
    fn scrollback_comes_from_the_session_config() {
        let dir = tempfile::tempdir().unwrap();
        let (store, id) = store_with_scrollback(dir.path(), Some(777));
        assert_eq!(resolved_scrollback(Some(&store), Some(id)), 777);
    }

    /// 快速连接(没有会话记录)、store 打不开、会话已被删 —— 一律落回 store
    /// 的**内置默认**。
    ///
    /// **不许是 0**:`unwrap_or_default()` 在这里会让 scrollback 整个消失,
    /// 症状只是"往上翻不动",没人会怀疑到默认值头上(`inherit.rs` 的字段
    /// 注释专门为这个陷阱留过一段话)。也不许在这里另写一个字面量 —— 那样
    /// 配置页显示的数字和终端里真正生效的数字会各说各话。
    ///
    /// 自证会变红:把 `map_or(DEFAULT_SCROLLBACK, ..)` 改成 `map_or(0, ..)`。
    #[test]
    fn scrollback_falls_back_to_the_store_default_not_zero() {
        let dir = tempfile::tempdir().unwrap();
        let (store, id) = store_with_scrollback(dir.path(), None);
        let default = mullion_store::DEFAULT_SCROLLBACK as usize;
        assert!(default > 0);
        assert_eq!(resolved_scrollback(None, Some(id)), default, "store 不可用");
        assert_eq!(resolved_scrollback(Some(&store), None), default, "快速连接");
        assert_eq!(
            resolved_scrollback(Some(&store), Some(SessionId(999))),
            default,
            "会话已被删"
        );
        assert_eq!(
            resolved_scrollback(Some(&store), Some(id)),
            default,
            "会话没配 scrollback,该走继承链的默认"
        );
    }

    /// 新建 pane 的仿真器同时带上主题底色(F80 §3.2 三处同源之一)和用户配的
    /// 回溯行数(F17)。两件事分头写在三个注入点上,漏一处都是静默的。
    #[test]
    fn a_new_pane_emulator_carries_both_the_theme_and_the_scrollback() {
        let emu = new_pane_emulator(4321);
        assert_eq!(emu.requested_history(), 4321);
        let snap = emu.snapshot();
        let cell = &snap.row(0)[0];
        assert_eq!(
            cell.bg,
            crate::theme::MULLION_DARK.term_bg,
            "空格背景应是主题底色"
        );
        assert_eq!(
            cell.fg,
            crate::theme::MULLION_DARK.term_fg,
            "空格前景应是主题前景"
        );
    }

    /// **三个注入点必须全走 `new_pane_emulator`。**
    ///
    /// 只能从源码上扎:漏掉其中一处的表现是「某种方式开出来的 pane 回溯深度
    /// 跟别的不一样」,没有任何测试会因此变红,而端到端要往上翻几千行才看得
    /// 出来。三处分别是 `accept_connect_ok`(主 pane)、`PaneOpened`(分屏)、
    /// `rehost_pane`(换节点)。
    ///
    /// 针在运行时拼:写成字面量的话这条测试自己的源码就会匹配上自己,恒绿。
    #[test]
    fn no_production_site_constructs_an_emulator_with_the_default_history() {
        let needle = concat!("Emulator", "::new(");
        let src = include_str!("app.rs");
        let prod = &src[..src
            .find("\n#[cfg(test)]\nmod tests {")
            .expect("找不到 tests mod")];
        let hits: Vec<_> = prod
            .lines()
            .filter(|l| !l.trim_start().starts_with("///"))
            .filter(|l| l.contains(needle))
            .collect();
        assert!(
            hits.is_empty(),
            "生产代码里还有 {} 处直接构造 Emulator:{hits:?} —— 它们拿的是内置\
             默认回溯行数,用户在会话里配的值到不了(F17)。走 `new_pane_emulator`",
            hits.len()
        );
    }

    /// F17「立刻生效」的接线:改完会话配置那一帧要把新值推给在跑的 pane。
    ///
    /// 与 `refresh_appearance` 同一个 `touched_store` 门控 —— 分开写迟早
    /// 漏掉一个;而漏掉的表现是「改了配置得重连才算数」,正是这个项目反复
    /// 踩过的那类静默。
    ///
    /// 自证会变红:把门控里的 `self.refresh_scrollback();` 删掉。
    #[test]
    fn a_store_change_pushes_the_new_scrollback_to_live_panes() {
        let src = include_str!("app.rs");
        let after = src
            .split("\n                if touched_store {")
            .nth(1)
            .expect("找不到 touched_store 的门控块");
        let body = &after[..after.find("\n                }\n").expect("门控块没有结尾")];
        assert!(
            body.contains("self.refresh_appearance();"),
            "门控块切歪了 —— 下面那条断言会空过"
        );
        assert!(
            body.contains("self.refresh_scrollback();"),
            "改了会话配置没把 scrollback 推给在跑的 pane:用户改完得重连才生效(F17)"
        );
    }

    /// F155:四个采集点必须都在事件循环里接上。
    ///
    /// 扎源码结构而不是跑帧:这几处都在 `window_event` 的重绘分支里,要真的
    /// 窗口和 GPU 才跑得起来。漏接一处不会有任何报错,只会让剖面里那一列
    /// **恒为零** —— 而「这个窗口没跳帧」与「这个版本忘了统计」在日志里
    /// 长得一模一样。
    ///
    /// **只搜 `mod tests` 之前的那一段源码**:needle 本身就写在下面这个
    /// 数组里,整份文件搜的话每一条都恒真,这测试就成了摆设。
    ///
    /// 自证会变红:删掉下面任意一句接线。
    #[test]
    fn the_frame_profile_hooks_are_all_wired_into_the_event_loop() {
        let src = include_str!("app.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .expect("app.rs 的测试模块分界变了,这条测试的锚点失效了");
        assert!(
            prod.len() < src.len(),
            "没能切掉测试模块 —— 下面每条断言都会恒真"
        );
        for needle in [
            "diag::record_frame_us(",
            "diag::count_redraw(",
            "diag::count_throttled()",
            "diag::set_scale(",
        ] {
            assert!(
                prod.contains(needle),
                "剖面采集点 `{needle}` 没接进事件循环 —— 剖面里那一列会恒为零"
            );
        }
    }

    /// F167/F169:埋点接线守护。三处滚动调用点每处都要跟 count_scroll,
    /// gauge 计算必须遍历 self.tabs(全部标签)而不是 active_ws,内存/传输
    /// 两组 gauge 各自的接线都要在场。
    ///
    /// 自证会变红:
    /// - 删掉任意一处 `diag::count_scroll();`
    /// - 把遍历改成 `self.active_ws()`
    /// - 删掉 `scroll_lines += p.emulator.scrollback_lines() as u64;`
    ///   (行数 gauge 悄悄退化成恒零,`set_mem_gauges(scroll_bytes` 那条
    ///   锚串本身抓不住这个盲点)
    /// - 整段删掉 `diag::set_xfer_gauges(...)`(传输 gauge 悄悄退化成
    ///   恒零,前面几条锚串都不会发现)
    ///
    /// 同 `the_frame_profile_hooks_are_all_wired_into_the_event_loop`:只搜
    /// `mod tests` 之前的那一段源码 —— 否则这条测试自己断言字符串里的
    /// `diag::count_scroll();` 也会被数进去,计数恒多算一次。
    #[test]
    fn scroll_and_gauge_wiring_is_present_in_source() {
        let src = include_str!("app.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .expect("app.rs 的测试模块分界变了,这条测试的锚点失效了");
        assert!(
            prod.len() < src.len(),
            "没能切掉测试模块 —— 下面每条断言都会恒真"
        );
        assert_eq!(
            prod.matches("diag::count_scroll();").count(),
            3,
            "滚动埋点必须恰好三处(滚轮/翻页/拖拽自滚)"
        );
        assert!(prod.contains("diag::set_mem_gauges(scroll_bytes"));
        assert!(
            prod.contains("for tab in self.tabs.iter() {"),
            "内存记账必须遍历全部标签,而不是只看 active_ws"
        );
        assert!(
            prod.contains("scroll_lines += p.emulator.scrollback_lines() as u64;"),
            "scrollback_lines() 没接线 —— profile.load 的行数 gauge 会恒为零, \
             `set_mem_gauges(scroll_bytes` 这条锚串抓不住这个盲点"
        );
        assert!(
            prod.contains("diag::set_xfer_gauges(\n                    xs.active as u64,"),
            "传输 gauge 没接线 —— profile.load 的传输在途/字节数会恒为零"
        );
    }

    /// F155/T2:每 pane 的同步块计数(`take_counts`)必须被汇总并喂给
    /// `diag::count_sync`,否则剖面里「同步块=/超时=」两列会恒为零 ——
    /// 这是本项目最有价值的一组指标,历史上「打字慢一拍」的真根因就是这里
    /// 的超时收口。
    ///
    /// 同 `the_frame_profile_hooks_are_all_wired_into_the_event_loop`:只搜
    /// `mod tests` 之前的那一段源码,否则 needle 会命中这条测试自己。
    ///
    /// 自证会变红:删掉 `diag::count_sync(...)` 那句接线,或者把
    /// `p.pacer.take_counts()` 那句删掉(汇总永远是 0,`diag::count_sync`
    /// 还在但传的是死值)。
    #[test]
    fn the_sync_block_counts_are_collected_and_forwarded_to_the_profile() {
        let src = include_str!("app.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .expect("app.rs 的测试模块分界变了,这条测试的锚点失效了");
        assert!(
            prod.len() < src.len(),
            "没能切掉测试模块 —— 下面每条断言都会恒真"
        );
        for needle in ["p.pacer.take_counts()", "diag::count_sync("] {
            assert!(
                prod.contains(needle),
                "同步块计数接线 `{needle}` 没接进事件循环 —— 剖面里这一列会恒为零"
            );
        }
    }

    /// F155:`drain_export_log_request` 必须**既被定义、也被调用**。
    ///
    /// 只定义不调用 → 用户点了「导出脱敏日志…」没反应;只置位不消费(反过来,
    /// 万一以后有人把调用点删掉但留着方法)→ `export_log_request` 这个 bool
    /// 永远是 `true`,之后随便哪一帧都会再导一次(而且第一次点击也不会有
    /// 任何反馈,因为消费点根本没跑到)。
    ///
    /// 同 `the_frame_profile_hooks_are_all_wired_into_the_event_loop`:只搜
    /// `mod tests` 之前的那一段源码,否则 needle 会命中这条测试自己。
    ///
    /// 自证会变红:删掉 `fn drain_export_log_request` 的定义(第一条断言红),
    /// 或删掉事件循环里 `self.drain_export_log_request();` 那句调用(第二条红)。
    #[test]
    fn drain_export_log_request_is_both_defined_and_called() {
        let src = include_str!("app.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .expect("app.rs 的测试模块分界变了,这条测试的锚点失效了");
        assert!(
            prod.len() < src.len(),
            "没能切掉测试模块 —— 下面每条断言都会恒真"
        );
        assert!(
            prod.contains("fn drain_export_log_request(&mut self) {"),
            "drain_export_log_request 没有被定义"
        );
        assert!(
            prod.contains("self.drain_export_log_request();"),
            "drain_export_log_request 定义了但事件循环里没调用 —— 用户点了导出没反应"
        );
    }

    /// 重绘归因传的两个布尔,必须与 `frame_is_dirty` 收到的是**同一对**。
    ///
    /// 传错(比如两个都传 `self.ui_dirty`)的话,剖面里「远端来了字节」与
    /// 「egui 要重绘」这两列全是假的,而它们正是用来判断「远端安静时还在
    /// 白烧 GPU」的依据 —— 归因错了比没有更糟,会把人带去改错地方。
    ///
    /// 自证会变红:把接线改成 `diag::count_redraw(self.ui_dirty, self.ui_dirty)`。
    #[test]
    fn the_redraw_attribution_uses_the_same_pair_as_the_dirty_check() {
        let src = include_str!("app.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .unwrap_or(src);
        // `split` 找不到模式时会把整份 haystack 原样返回,`.next()` 永远是
        // `Some` —— 光靠 `expect` 兜不住。切不干净的话下面搜到的会是这条
        // 测试**自己**文档注释里的那句 `count_redraw(self.ui_dirty, ...)`,
        // 断言直接恒绿。
        assert!(
            prod.len() < src.len(),
            "没能切掉测试模块 —— 会搜到测试自己的文本,断言恒绿"
        );
        let call = prod
            .split("diag::count_redraw(")
            .nth(1)
            .expect("没接 count_redraw")
            .split(");")
            .next()
            .unwrap_or_default();
        assert!(
            call.contains("self.ui_dirty"),
            "count_redraw 的 ui 那一路不是 self.ui_dirty:{call}"
        );
        assert!(
            !call.contains("self.ui_dirty, self.ui_dirty"),
            "两路传了同一个值,归因是假的:{call}"
        );
    }

    /// F157:**每一处**置脏都必须走 `mark_ui_dirty!`,不许直接写字段赋值。
    ///
    /// 直接赋值等于在归因表上开一个洞,而洞的症状是「剖面里少了一行、
    /// 看起来一切正常」—— 这类静默失效在本项目已经踩中过多次(F12 的
    /// 埋点、F155 的接线),只能靠机械守护。
    ///
    /// 顺带钉住清脏点还在:一起被替换掉的话 `ui_dirty` 再也不会归零,
    /// 每帧都脏,直接重演 T3。
    ///
    /// 自证会变红:把任意一处 `mark_ui_dirty!(self.ui_dirty);` 改回
    /// 直接字段赋值。
    #[test]
    fn every_ui_dirty_set_site_goes_through_the_attribution_macro() {
        let src = include_str!("app.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .unwrap_or(src);
        // `split` 找不到模式时会把整份 haystack 原样返回,`.next()` 永远是
        // `Some` —— 切不干净的话下面搜到的会是测试自己的文本,断言恒绿。
        assert!(
            prod.len() < src.len(),
            "没能切掉测试模块 —— 会搜到测试自己的文本,断言恒绿"
        );
        assert!(
            prod.contains("macro_rules! mark_ui_dirty"),
            "置脏宏不见了 —— 归因整个失效"
        );
        assert!(
            prod.contains("mark_ui_dirty!(self.ui_dirty);"),
            "一处宏调用都没有 —— 替换没做"
        );
        assert_eq!(
            prod.matches("ui_dirty = true").count(),
            0,
            "有置脏点绕开了 mark_ui_dirty! —— F157 的归因表会漏掉它"
        );
        assert!(
            prod.contains("self.ui_dirty = false;"),
            "清脏点被一起改掉了 —— ui_dirty 再也不会归零,直接重演 T3"
        );
    }

    /// F157:**每一处** `request_redraw()` 都必须同时记一笔来源。
    ///
    /// 漏一处的症状是剖面里 `wake` 与 `rr` 的差值凭空变大,而那个差值正是
    /// 用来判断「唤醒是谁推的」的唯一依据 —— 归因错了会把人带去改错地方。
    ///
    /// 自证会变红:删掉任意一处 `diag::count_request_redraw(...)`,
    /// 或者新加一处 `request_redraw()` 而不配套记账。
    #[test]
    fn every_request_redraw_records_where_it_came_from() {
        let src = include_str!("app.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .unwrap_or(src);
        assert!(
            prod.len() < src.len(),
            "没能切掉测试模块 —— 会搜到测试自己的文本,断言恒绿"
        );
        let calls = prod.matches(".request_redraw();").count();
        let notes = prod.matches("diag::count_request_redraw(").count();
        assert!(calls > 0, "一处 request_redraw 都没有 —— 切片切错了");
        assert_eq!(
            calls, notes,
            "{calls} 处 request_redraw 只有 {notes} 处记了来源"
        );
        assert!(
            prod.contains("diag::count_request_redraw(diag::RedrawSource::Scheduled)"),
            "没有任何一处按 `sched`(about_to_wait 到点补画)记账"
        );
    }

    /// F157:唤醒计数必须记在 `RedrawRequested` 分支的**最开头**。
    ///
    /// 记在后面的话,那些在帧闸之前就 return 掉的路径(最小化 / PumpOnly)
    /// 完全不计数 —— 而「窗口最小化了却还在每秒醒 400 次」恰恰是最该被
    /// 看见的一种。
    ///
    /// 自证会变红:把 `diag::count_wake();` 挪到 `self.pump_io();` 之后。
    #[test]
    fn the_wake_counter_sits_at_the_very_top_of_the_redraw_arm() {
        let src = include_str!("app.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .unwrap_or(src);
        assert!(prod.len() < src.len(), "没能切掉测试模块,断言会恒绿");
        let arm = prod
            .split("WindowEvent::RedrawRequested => {")
            .nth(1)
            .expect("找不到 RedrawRequested 分支");
        // 按**字符**截,不按字节:`arm` 里是中文注释,`&arm[..200]` 一旦让
        // 边界落进某个汉字中间就是 panic 而不是干净的断言失败,排查的人会
        // 先去怀疑代码逻辑。150 字符比原来的 200 字节略宽,仍只够放下分支
        // 开头那几行(计数点现在在第 96 个字符)。
        let head: String = arm.chars().take(150).collect();
        assert!(
            head.contains("diag::count_wake();"),
            "唤醒计数不在分支开头:{head}"
        );
        assert!(
            arm.split("self.pump_io();")
                .next()
                .unwrap_or_default()
                .contains("count_wake"),
            "唤醒计数排在了 pump_io 之后 —— 提前 return 的路径会漏计"
        );
    }

    /// F157:喂给 egui 的事件数与 egui 吐回来的 `repaint_delay` 都要采。
    ///
    /// 这两个数是本切片**唯一**能回答「`wants_repaint_after` 的哪一条判据
    /// 每帧都成立」的东西,少任何一个都得再跑一趟实机。
    ///
    /// 自证会变红:删掉 `diag::note_egui_events(` 或 `diag::note_repaint_delay(`。
    #[test]
    fn the_egui_side_of_the_frame_loop_is_instrumented() {
        let src = include_str!("app.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .unwrap_or(src);
        assert!(prod.len() < src.len(), "没能切掉测试模块,断言会恒绿");
        assert!(
            prod.contains("diag::note_egui_events(raw_input.events.len());"),
            "没采「这一帧喂了 egui 几个事件」"
        );
        assert!(
            prod.contains("diag::note_repaint_delay(repaint_delay);"),
            "没采 egui 吐回来的 repaint_delay"
        );
    }

    /// F158:判脏在 launcher 态与终端态**必须是同一条判据**。
    ///
    /// 摘掉的是原来那句 `None => true`(没连任何东西时无条件判脏)。日志
    /// 坐实了它的后果:`tabs=0 panes=0` 时照样 `frame=300x/present=300`
    /// —— 对着一屏静止的占位 UI 每秒提交 60 帧 GPU。旁边注释给的理由
    /// (「`ControlFlow::Wait` 下 winit 不会凭空生成 `RedrawRequested`」)
    /// 在同一函数别处会排 `WaitUntil` 的前提下不成立。
    ///
    /// 判据本身的真值表由 `frame::tests::egui_repaint_alone_is_dirty_enough`
    /// 里的 `assert!(!frame_is_dirty(false, false))` 守着,这里只钉接线。
    ///
    /// 自证会变红:把绑定式改回
    /// `match self.active_ws() { Some(_) => crate::frame::frame_is_dirty(terminal_dirty, self.ui_dirty), None => true }`。
    #[test]
    fn the_frame_dirty_check_is_the_same_in_both_states() {
        let src = include_str!("app.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .unwrap_or(src);
        assert!(
            prod.len() < src.len(),
            "没能切掉测试模块 —— 会搜到测试自己的文本,断言恒绿"
        );
        assert_eq!(
            prod.matches("let dirty = ").count(),
            1,
            "`let dirty = ` 不止一处,下面的切片会指错地方"
        );
        let binding = prod
            .split("let dirty = ")
            .nth(1)
            .expect("找不到 dirty 的绑定式")
            .split(";\n")
            .next()
            .unwrap_or_default();
        assert_eq!(
            binding.trim(),
            "crate::frame::frame_is_dirty(terminal_dirty, self.ui_dirty)",
            "launcher 态又开了一条兜底判据:{binding}"
        );
    }

    /// F158:后台事件**默认标脏**,只有三种显式豁免。
    ///
    /// 方向不对称是重点:判 `true` 最多多画一帧(而且会被 F159 的整帧指纹
    /// 拦掉),判 `false` 是「连上了画面不动」。所以 `user_event_marks_dirty`
    /// 写成穷尽 `match` 而不是 `_ => false` —— 加新变体时编译报错,强迫作者
    /// 表态,而不是静默落到「不标脏」那一侧。
    ///
    /// 自证会变红:把 `user_event_marks_dirty` 里 `ConnectErr` 那一支挪进
    /// 豁免名单。
    #[test]
    fn a_background_event_marks_the_ui_dirty_unless_it_is_a_known_flood() {
        // 高频、靠帧内排期驱动的两种:标脏就是 T3(风扇起飞)。
        assert!(
            !user_event_marks_dirty(&UserEvent::Wake),
            "Wake 每秒可以来几千条,标脏等于把 T2 的攒帧闸整个绕过去"
        );
        assert!(
            !user_event_marks_dirty(&UserEvent::TransferProgress { job: 1, done: 0 }),
            "传输进度每秒几千条,靠它驱动重绘就是风扇起飞"
        );
        // `EditTick` 豁免的理由**不同**:它自己的分支把 `self.ui_dirty` 当
        // 信号读(「文件变了不一定改动界面」),在这里预先置真会让那个条件
        // 恒成立,静默改掉它的语义。
        assert!(
            !user_event_marks_dirty(&UserEvent::EditTick {
                key: 1,
                stamp: None
            }),
            "EditTick 自己判脏,在这里预置会让它分支里的 `if self.ui_dirty` 恒真"
        );
        // 其余一律标脏 —— 挑三种最容易漏、且漏了症状最难查的。
        assert!(
            user_event_marks_dirty(&UserEvent::ConnectErr(
                crate::shell::dial_ledger::DialId(1),
                "boom".into()
            )),
            "连接失败不标脏 = 用户点了连接,错误提示要等他动鼠标才出现"
        );
        assert!(
            user_event_marks_dirty(&UserEvent::ProbeOk(7)),
            "探测结果不标脏 = 会话管理器里的状态永远停在「探测中」"
        );
        assert!(
            user_event_marks_dirty(&UserEvent::KeyPathPicked(None)),
            "文件对话框取消不标脏 = 按钮永远停在禁用态"
        );
    }

    /// F158:那个判据必须**真的接在** `user_event` 的开头。
    ///
    /// 纯函数写对了但没接线,是本项目反复踩过的静默失效(F12 的埋点、
    /// F155 的接线)——判据全绿,画面照样卡住。
    ///
    /// 自证会变红:把 `fn user_event` 开头那三行删掉。
    #[test]
    fn the_background_dirty_rule_is_actually_wired_into_user_event() {
        let src = include_str!("app.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .unwrap_or(src);
        assert!(prod.len() < src.len(), "没能切掉测试模块,断言会恒绿");
        let body = prod
            .split("fn user_event(&mut self")
            .nth(1)
            .expect("找不到 user_event");
        // 按字符截,不按字节:`app.rs` 里满是中文注释,`&body[..600]` 一旦
        // 让边界落进汉字中间就是 panic 而不是干净的断言失败。
        let head: String = body.chars().take(400).collect();
        assert!(
            head.contains("if user_event_marks_dirty(&event)"),
            "判据没接进 user_event 的开头:{head}"
        );
    }

    /// F159:指纹命中必须在**任何 GPU 工作之前**提前 return,而基准只能由
    /// **真正提交过**的帧更新。
    ///
    /// 三条各自钉一个「编译过、跑起来才发作」的错法:
    ///
    /// - 提前 return 排在 `get_current_texture` 之后 → 每帧照样占一次交换链,
    ///   Fifo 下照样等一个 vsync,收益归零而剖面看起来一切正常。
    /// - 基准在提前 return 的路径上也更新 → prepare 失败 / acquire 失败那几帧
    ///   没画出去却成了基准,下一帧误判命中,屏幕停在更早的一帧上。
    /// - 判断挪到调用方侧 → 得手工重做 `record_present`/`mark_presented` 那几笔
    ///   记账,漏一笔就是 60fps 空转且 `present=0`。
    ///
    /// 自证会变红:把 `a.last_frame_fp = Some(fp);` 挪到 `frame.present();` 之前。
    #[test]
    fn a_fingerprint_hit_returns_before_any_gpu_work_and_only_a_presented_frame_becomes_the_baseline(
    ) {
        let src = include_str!("app.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .unwrap_or(src);
        assert!(
            prod.len() < src.len(),
            "没能切掉测试模块 —— 会搜到测试自己的文本,断言恒绿"
        );
        let at = |needle: &str| {
            assert_eq!(
                prod.matches(needle).count(),
                1,
                "`{needle}` 在生产段里不是恰好一处,下面的先后判断会指错地方"
            );
            prod.find(needle).expect("上面刚断言过存在")
        };
        let fp = at("crate::frame_fp::frame_fingerprint(");
        let skip = at("if crate::frame_fp::can_skip(");
        let acquire = at("a.gpu.surface.get_current_texture()");
        let present = at("frame.present();");
        let baseline = at("a.last_frame_fp = Some(fp);");
        assert!(fp < skip, "先判跳帧后算指纹,顺序反了");
        assert!(
            skip < acquire,
            "指纹命中的提前 return 排在 acquire 之后 —— 每帧照样占一次交换链,收益归零"
        );
        assert!(
            present < baseline,
            "没画出去的帧也成了下一帧的比对基准 —— 屏幕会停在更早的一帧上"
        );
    }

    /// F159:surface 被重新 configure 之后,交换链内容未定义,基准必须作废。
    ///
    /// 不作废的症状:窗口从遮挡/丢失中恢复之后画面全黑或残留旧内容,而
    /// 且只在触发过 `SurfaceError::Lost`/`Outdated` 的机器上出现。
    ///
    /// 自证会变红:把 Lost/Outdated 分支里那句 `a.last_frame_fp = None;` 删掉。
    #[test]
    fn reconfiguring_the_surface_drops_the_fingerprint_baseline() {
        let src = include_str!("app.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .unwrap_or(src);
        assert!(prod.len() < src.len(), "没能切掉测试模块,断言会恒绿");
        let arm = prod
            .split("a.gpu.surface.configure(&a.gpu.device, &a.gpu.config);")
            .nth(1)
            .expect("找不到 surface 重新 configure 的分支");
        // 按**字符**截,不按字节:`app.rs` 里满是中文注释,`&arm[..300]` 一旦让
        // 边界落进汉字中间就是 panic 而不是干净的断言失败。
        let head: String = arm.chars().take(200).collect();
        assert!(
            head.contains("a.last_frame_fp = None;"),
            "重新 configure 之后没作废指纹基准:{head}"
        );
    }

    /// F159:`Gpu::resize` 是 `surface.configure` 的**第二个**调用点(第一个在
    /// `render_frame` 的 Lost/Outdated 分支,见上一条测试),`apply_resize` 里
    /// 调完它也必须紧接着作废整帧指纹基准,否则「最小化后还原到原尺寸」这条
    /// 路——`(config.width, config.height)` 没变、指纹里其余项空闲时也不变——
    /// 会在内容未定义的交换链上误判命中并提前 return,画面停在最小化之前。
    ///
    /// 自证会变红:把 `apply_resize` 里 `a.gpu.resize(width, height);` 之后
    /// 那句作废语句删掉。
    #[test]
    fn resizing_the_surface_also_drops_the_fingerprint_baseline() {
        let src = include_str!("app.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .unwrap_or(src);
        assert!(prod.len() < src.len(), "没能切掉测试模块,断言会恒绿");
        let needle = "a.gpu.resize(width, height);";
        assert_eq!(
            prod.matches(needle).count(),
            1,
            "调用点数量变了,先确认还是不是唯一的 Gpu::resize(width, height) 调用"
        );
        let after = prod.split(needle).nth(1).expect("上面刚断言过存在");
        // 按**字符**截,不按字节:`app.rs` 满是中文注释,`&after[..N]` 一旦让
        // 边界落进汉字中间就是 panic 而不是干净的断言失败。窗口紧贴实际距离:
        // 从调用点到作废语句结尾实测 451 字符,留个位数的余量取 460——太宽会连
        // 下一段无关代码一起框进来(恒绿),太窄会把真实语句切没(误报)。
        let head: String = after.chars().take(460).collect();
        assert!(
            head.contains("a.last_frame_fp = None;"),
            "surface 重新 configure(经 Gpu::resize)之后没作废指纹基准:{head}"
        );
    }

    /// F159:egui 的纹理增量是**每帧 drain、只交付一次**的,`deltas_empty` 必须
    /// 同时看 `.set` 和 `.free` 两侧算出来——只看其中一侧,或干脆恒为
    /// `true`,都会在跳帧时把另一侧的增量静默丢弃(下一次命中时 renderer 拿不到
    /// 本该更新的图集,花屏或 panic)。
    ///
    /// 自证会变红:把 `deltas_empty` 的计算改成 `true`,或者去掉
    /// `&& full_output.textures_delta.free.is_empty()` 只留 `.set` 那半边。
    #[test]
    fn deltas_empty_is_derived_from_both_set_and_free() {
        let src = include_str!("app.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .unwrap_or(src);
        assert!(prod.len() < src.len(), "没能切掉测试模块,断言会恒绿");
        let needle = "let deltas_empty =";
        assert_eq!(
            prod.matches(needle).count(),
            1,
            "deltas_empty 的赋值处数量变了,先确认还是不是唯一一处"
        );
        let after = prod.split(needle).nth(1).expect("上面刚断言过存在");
        // 窗口紧贴实际距离:赋值语句本体在 100 字符内说得完,不需要往后多框。
        let head: String = after.chars().take(100).collect();
        assert!(
            head.contains("full_output.textures_delta.set.is_empty()")
                && head.contains("full_output.textures_delta.free.is_empty()"),
            "deltas_empty 没有同时看 set 和 free 两侧:{head}"
        );
    }

    /// F170:`mid_mark` 必须夹在终端趟之后、`forget_lifetime` 之前 —— 放错
    /// 位置分层就成了「全帧/0」,数字看着还挺合理,只有源码顺序能守。
    ///
    /// 同 `the_frame_profile_hooks_are_all_wired_into_the_event_loop`:只搜
    /// `mod tests` 之前的那一段源码,否则 needle 会命中这条测试自己
    /// (`"t.mid_mark(&mut pass);"` 这个字面串就写在这条测试里)。
    ///
    /// 自证会变红:把 `t.mid_mark(&mut pass);` 那段挪到 `forget_lifetime`
    /// 之后。
    #[test]
    fn the_gpu_mid_mark_sits_between_terminal_and_egui() {
        let src = include_str!("app.rs");
        let prod = src
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .expect("app.rs 的测试模块分界变了,这条测试的锚点失效了");
        assert!(
            prod.len() < src.len(),
            "没能切掉测试模块 —— 下面每条断言都会恒真"
        );
        let mid = prod
            .find("t.mid_mark(&mut pass);")
            .expect("mid_mark 调用点没找到");
        let forget = prod
            .find("let mut static_pass = pass.forget_lifetime();")
            .expect("forget_lifetime 调用点没找到");
        let term_draw = prod
            .find("a.text.render(&mut pass)")
            .expect("终端文字趟调用点没找到");
        assert!(
            term_draw < mid && mid < forget,
            "顺序要求:终端趟 < mid_mark < forget_lifetime,实际 term_draw={term_draw} mid={mid} forget={forget}"
        );

        // 契约二:还得在 `if let Some(terminal_draw)` 判空块**之外**。光比先后
        // 位置管不住这条 —— 挪进块内、仍排在 `a.text.render` 之后,上面三个
        // 位置的相对顺序一个都不变,测试照绿,而 launcher 态(没有终端可画)
        // 那些帧的槽 1 就再没人写,`resolve` 读到上一次采样的残留值。
        let block = prod
            .find("if let Some(n) = &terminal_draw {")
            .expect("终端趟判空块没找到");
        let open = prod[block..].find('{').expect("判空块的左括号") + block;
        let mut depth = 0usize;
        let mut close = None;
        for (i, c) in prod[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(open + i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let close = close.expect("判空块没有配对的右括号");
        // 括号配平扫描的自守:那一段里唯一出现在字符串字面量里的花括号是
        // `{e:?}`(自身配平),所以扫描可信。真配错了,收尾括号就不会落在
        // 与 `if let` 同级的 8 空格缩进上 —— 当场报出来,而不是给下面那条
        // 断言喂一个错的边界。
        assert!(
            prod[..close].ends_with("\n        "),
            "括号配平扫到的收尾括号不在 8 空格缩进上,这条测试的边界不可信"
        );
        assert!(
            mid > close,
            "mid_mark 落进了 terminal_draw 判空块内(块 {block}..{close},mid={mid})"
        );
    }
}
