//! 分屏工作区(F30–F35):多 pane 状态机 + 几何 + 布局预设。
//!
//! 零 winit/wgpu/egui —— 状态机部分可纯单测,这是本切片能在无头容器里验证
//! 布局与 window_change 行为的前提。

pub mod geom;
pub mod preset;

pub use geom::{
    layout_geometry, term_pad_px, title_bar_px, PaneGeom, PxRect, GAP_PX, TERM_PAD_PT, TITLE_BAR_PT,
};
pub use preset::{icon_cells, next_focus, plan_preset, preset_tree, Preset, PresetPlan};

/// pane 的连接状态(§6.3)。断开的 pane 内容保留、可滚可复制,只是不再收发。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneStatus {
    Live,
    /// F128:链路死了,正在自动退避重连。**内容保留**——重连成功后接着往下写,
    /// 用户滚回去还能看到断线前的输出。
    Reconnecting,
    /// 不会自己回来了:远端 shell 自己退了(用户敲了 `exit`),或重试到顶。
    Disconnected,
}

/// F128:`rx` 关闭该怎么处置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RxClosed {
    /// 链路死了,自动重连。
    Reconnect,
    /// 远端 shell 自己退了(`exit`),SSH 连接还活着 —— **绝不重连**,
    /// 否则用户永远退不出登录。
    UserExited,
}

/// F128:判据只有一条 —— SSH **连接**(不是 channel)还在不在。
/// channel 关了而连接还在 = 远端进程退了;连接也没了 = 链路死了。
pub fn rx_closed_action(transport_alive: bool) -> RxClosed {
    if transport_alive {
        RxClosed::UserExited
    } else {
        RxClosed::Reconnect
    }
}

/// F128:`hosts[ix]` 这条连接的传输层还活着没有。
///
/// 抽成 fn 指针字段是为了**能在无头测试里翻转** —— 直接查
/// `handle.is_closed()` 的话,"链路死了"这个状态在测试里根本造不出来
/// (测试用的 `Workspace` 连 `HostConn` 都没有)。
/// 查不到 host 时返回 `true`(当成"连接还在"):那是异常状态,
/// 宁可不重连,也不要对着一条不存在的连接无限重拨。
pub fn default_link_alive(hosts: &[HostConn], ix: usize) -> bool {
    // 写成 match 而不是 `map_or`/`is_none_or`:后两者在不同 clippy 版本里
    // 会互相建议对方(`-D warnings` 下就是编不过),match 两边都不挑刺。
    match hosts.get(ix) {
        None => true,
        Some(h) => !h.handle.is_closed(),
    }
}

use std::sync::Arc;
use std::time::Instant;

use mullion_core::layout::{close_pane, leaves, split_pane, Dir, Node, PaneId};
use mullion_ssh::session::{SshConnection, SshSession, TrySendErr};
use mullion_term::emulator::Emulator;
use tokio::sync::mpsc::{error::TryRecvError, Receiver};

use crate::render::SyncFramePacer;
use crate::session_pump;

/// pane 对下游 PTY 的写口。
///
/// 抽 trait 有两个理由:
/// 1. `SshSession` 的字段和 `SshCmd` 都是 mullion-ssh 私有,跨 crate **无法构造**;
///    没有这层抽象,workspace 的状态机(T1 归属、F34 resize)一条测试都写不出来。
/// 2. 断线的 pane 将来要能换成"丢弃写入"的实现,不必让状态机到处判 Option。
pub trait PtyWriter: Send {
    fn write(&self, bytes: Vec<u8>) -> Result<(), TrySendErr>;
    fn resize(&self, cols: u16, rows: u16) -> Result<(), TrySendErr>;
    /// F140:显式关掉这条 channel(发 `SSH_MSG_CHANNEL_CLOSE`)。
    ///
    /// **没有默认实现**:漏实现就该编译不过。给个空默认的话,将来新加的
    /// 写口会静默地不收口 —— 而那正是 F140 要修的那个 bug 的形状。
    ///
    /// 无返回值:调用点都在"反正要丢弃这个 pane 了"的路径上,失败没有
    /// 补救动作(见 `SshSession::close` 的文档)。
    fn close(&self);
}

impl PtyWriter for SshSession {
    fn write(&self, bytes: Vec<u8>) -> Result<(), TrySendErr> {
        SshSession::write(self, bytes)
    }
    fn resize(&self, cols: u16, rows: u16) -> Result<(), TrySendErr> {
        SshSession::resize(self, cols, rows)
    }
    fn close(&self) {
        SshSession::close(self)
    }
}

/// 同一条 SSH channel 同时被 pane(同步写按键)和自动化 task(异步按时间表写)
/// 持有,所以要有一个可共享的写口。`SshSession` 内部只有一个
/// `mpsc::Sender<SshCmd>`,本身就是 `Send + Sync`,`Arc` 只是共享所有权,
/// 不引入任何锁。
///
/// 转发而不是 `impl<T: PtyWriter> PtyWriter for Arc<T>`:后者会跟
/// `Box<dyn PtyWriter>` 的既有用法产生一堆 trait 求解歧义,而本项目只需要
/// `Arc<SshSession>` 这一种。
impl PtyWriter for Arc<SshSession> {
    fn write(&self, bytes: Vec<u8>) -> Result<(), TrySendErr> {
        SshSession::write(self, bytes)
    }
    fn resize(&self, cols: u16, rows: u16) -> Result<(), TrySendErr> {
        SshSession::resize(self, cols, rows)
    }
    fn close(&self) {
        SshSession::close(self)
    }
}

/// D3/D6:一条**永远写不出去**的 pty。给「摆出来了但没有连接」的占位 pane 用。
///
/// 不用 `Option<Box<dyn PtyWriter>>`:那会让每个写入点都多一层判空,而漏掉
/// 任何一个的现象是 panic。写失败在本项目本来就是**静默丢弃**的既有语义
/// (调用点都是裸的 `let _ = pty.write(...)`,出站队列满时也是这样),
/// 让它走同一条路。
pub struct DeadPty;

impl PtyWriter for DeadPty {
    fn write(&self, _bytes: Vec<u8>) -> Result<(), TrySendErr> {
        Err(TrySendErr::Closed)
    }
    fn resize(&self, _cols: u16, _rows: u16) -> Result<(), TrySendErr> {
        Err(TrySendErr::Closed)
    }
    /// 没有 channel 可关。`close` 没有默认实现是刻意的(F140),这里显式给空体。
    fn close(&self) {}
}

/// 一条**已建立**的 SSH 连接。B2-a 里恒只有一个;数据模型按 N host × M channel
/// 设计,是为了 B2-b 的"换主机"能原地加而不用推翻结构(§4.2)。
pub struct HostConn {
    /// 状态栏/标题条上显示的名字(会话名或 user@host)。
    pub label: String,
    /// `user@host:port`,标题条副标题用。
    pub addr: String,
    /// 这条连接来自哪条会话记录(F61/F62 外观要按它查缓存)。
    /// `None` = 快速连接或 store 不可用。
    pub session_id: Option<mullion_store::SessionId>,
    /// 目标主机连接(含跳板保活)。**必须整条持有**:Drop 即断连。`Arc` 是必须的:
    /// russh 的 `Handle` 没实现 `Clone`,只有 `Drop`(释放即断连)。
    pub handle: Arc<SshConnection>,
    /// F124:这台机器上的 tmux 状态上报配过没有。**按连接存**——B2-b 换主机
    /// 之后一个 `Workspace` 上会有多条 `HostConn`,每台机器的 tmux 服务器
    /// 各自独立,共享一份 `done` 会让第二台永远配不上。
    pub tmux_bootstrap: crate::remote_bootstrap::BootstrapFlags,
    /// F124:上次**发起**自举的时刻。`None` = 还没试过。判据见
    /// `remote_bootstrap::should_attempt`。
    ///
    /// 断线重连换掉这条连接的 `handle` 时,这两个字段一并清零、重新配一遍
    /// (见 `UserEvent::PaneReconnected` 的处理)。这是对的:`tmux set -g` 幂等,
    /// 重发无副作用,而远端 tmux 服务器可能在断线期间重启过。
    pub tmux_last_try: Option<std::time::Instant>,
    /// F128:拨出这条连接用的参数。**按连接存,不是按标签存**。
    ///
    /// 标签级的 `TerminalTab::last_cfg` 只记得**最初**连的那台机器:用户用
    /// 「换节点」把一块 pane 挪到另一台服务器上之后,`hosts` 里就有了两台机器,
    /// 而 `last_cfg` 仍是第一台的。那时候第二台断线,拿 `last_cfg` 去重拨就是
    /// 静默连到**另一台机器**上——地址、凭据、主机指纹全对不上,而用户看到的
    /// 只是"重连好了"。`host_ix` 本来就是"哪台机器"的身份,拨号参数就该跟它走。
    ///
    /// `None` = 这条连接没有可复用的拨号参数(CLI 直连早期路径 / store 不可用),
    /// 此时不重连(`spawn_reconnect` 直接放弃并记日志)——宁可让用户手动重连,
    /// 也不猜一份配置拨出去。
    pub cfg: Option<mullion_ssh::config::SshConfig>,
}

/// 一个分屏的全部运行时状态。
pub struct PaneState {
    pub id: PaneId,
    /// 指向 `Workspace::hosts`。B2-a 恒为 0。
    pub host_ix: usize,
    pub emulator: Emulator,
    pub pty: Box<dyn PtyWriter>,
    pub rx: Receiver<Vec<u8>>,
    pub pacer: SyncFramePacer,
    pub status: PaneStatus,
    /// 这个 pane 收到过任何入站字节没有。**粘性**:一旦为真就不再变回假。
    ///
    /// 自动化的「就绪」判据(设计 §2):远端已经在说话,才说明我们拿到的是
    /// 一个活着的 login shell。`App` 每帧查它,不让 `pump` 返回 `Vec<PaneId>`
    /// ——`pump` 每帧必调,返回 Vec 等于每帧一次堆分配(T3)。
    pub saw_first_byte: bool,
    /// 上次发出去的 (cols, rows)。F34 只在这个值变化时才发 window_change ——
    /// 每帧无脑发会把远端 SIGWINCH 刷爆,tmux 里的 TUI 不停重排。
    pub last_grid: (u16, u16),
    /// ⑥:远端报出来的当前目录(字节,见 `RemoteState::cwd`)。
    /// `None` = 远端没报过(裸 shell 不设标题、或 tmux 没开 `set-titles`)。
    /// **只增不清**:拿不到新值时保留上一个已知值,比闪成「未知」有用。
    pub cwd: Option<Vec<u8>>,
    /// ⑥:tmux 会话名。`None` = 不在 tmux 里(或远端没开 `set-titles`)。
    /// 与 `cwd` 不同,**收到新标题就整体重置** —— 用户退出 tmux 之后 bash 会
    /// 发自己的标题,这时必须把会话名清掉,否则标题条上会永久挂着一个已经
    /// 不存在的会话名。
    pub tmux: Option<String>,
    /// F17:上一次**报告过**的实际 scrollback 行数,只用于日志去重。
    ///
    /// 拖窗口会连续产生几十次列数变化,每次都落一行「已夹紧」会把日志刷爆;
    /// 而完全不报告的话,「配了 10000 行、往上翻只有 2700 行」就是一个查无
    /// 可查的静默行为。初值 0 保证第一次一定报告。
    pub history_reported: usize,
    /// F162:`true` = 这块 pane 还没连上**它自己**那台机器 —— 排队等串行拨号的、
    /// 会话被删掉的占位(D3)、拨号失败降级的(D6)三种。
    ///
    /// 这三种 pane 都必须有 `PaneState`(没有的话画不出文字,树上只是一块短暂
    /// 空白,见 F35 的「空窗期」约定),但它们的 `host_ix` 指着**主叶子那台机器**,
    /// **不代表自己的身份**。落盘时据此判断该照运行时量、还是照抄上次从盘上
    /// 读回来的那份(设计 5.2②:不这么做的话,恢复途中被 kill 掉的 exe 会把
    /// 还没连上的叶子身份**永久**丢掉)。
    pub host_pending: bool,
    /// F163/D6:挂在这块 pane 标题条上的一句说明(「当初的会话 web01 已不存在」
    /// / 「会话已被删除,无法自动恢复」/「连接失败」)。`None` = 没话要说。
    ///
    /// **不弹窗**:多块 pane 同时失败时会连弹好几次(D4)。
    pub notice: Option<String>,
}

/// 多 pane 工作区:布局树 + 每 pane 状态 + 主机连接池。
pub struct Workspace {
    tree: Node,
    focus: PaneId,
    panes: Vec<PaneState>,
    pub hosts: Vec<HostConn>,
    next_id: u32,
    /// F83 pane 标题条开关,默认开。
    pub title_bars: bool,
    /// C1:跨「断开→重连」世代的标记。每次重连都会造一个全新的 `Workspace`,
    /// 而 `next_id` 每次都从 2 重新计数(见 `new`)——所以两代之间 `PaneId`
    /// **必然**会撞号。只看 id/树成员判断"这个异步事件还该不该被接受"不够,
    /// 必须连世代一起比。这个值本身只是外部调用方传进来的不透明标记(比大小
    /// 没有意义,只判等),真正保证"新世代的值一定跟旧世代不一样"是调用方
    /// (`App`)的责任——`Workspace` 自己没法保证,见 `new` 的说明。
    generation: u64,
    /// F128:判据见 `default_link_alive`。测试里可替换。
    pub link_alive: fn(&[HostConn], usize) -> bool,
}

impl Workspace {
    /// 用第一个 pane 起一个工作区(单屏)。`hosts` 由调用方随后 push。
    ///
    /// `generation` 必须由调用方保证跨「本进程存活期间创建过的所有
    /// `Workspace`」单调递增(不能在这里内部生成——`Workspace` 每次都是全新
    /// 对象,若在这里赋常量,新世代又从同一个值起步,等于没有世代区分)。
    pub fn new(first: PaneState, generation: u64) -> Self {
        let id = first.id;
        Self {
            tree: Node::Leaf(id),
            focus: id,
            next_id: id.0 + 1,
            panes: vec![first],
            hosts: Vec::new(),
            title_bars: true,
            generation,
            link_alive: default_link_alive,
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn tree(&self) -> &Node {
        &self.tree
    }
    pub fn focus(&self) -> PaneId {
        self.focus
    }
    pub fn set_focus(&mut self, id: PaneId) {
        if leaves(&self.tree).contains(&id) {
            self.focus = id;
        }
    }
    pub fn panes(&self) -> &[PaneState] {
        &self.panes
    }
    /// 逐个 pane 的可变引用。`mark_presented` 之类的逐帧动作用。
    pub fn panes_mut_iter(&mut self) -> impl Iterator<Item = &mut PaneState> {
        self.panes.iter_mut()
    }
    pub fn pane(&self, id: PaneId) -> Option<&PaneState> {
        self.panes.iter().find(|p| p.id == id)
    }
    pub fn pane_mut(&mut self, id: PaneId) -> Option<&mut PaneState> {
        self.panes.iter_mut().find(|p| p.id == id)
    }
    pub fn focused(&self) -> Option<&PaneState> {
        self.pane(self.focus)
    }
    pub fn focused_mut(&mut self) -> Option<&mut PaneState> {
        let f = self.focus;
        self.pane_mut(f)
    }
    /// 树上的叶子数。状态栏的"N 屏"用这个,不是 `panes.len()` ——
    /// 正在连接中的 pane 已经占了叶子位但还没有 `PaneState`。
    pub fn pane_count(&self) -> usize {
        leaves(&self.tree).len()
    }

    fn alloc_id(&mut self) -> PaneId {
        let id = PaneId(self.next_id);
        self.next_id += 1;
        id
    }

    /// 每 pane 的状态。树上有叶子但还没挂 `PaneState`(正在开 channel)的,
    /// 按 `Live` 算 —— 减屏时不该优先把"正在连"的当成死的先杀掉。
    fn statuses(&self) -> Vec<(PaneId, PaneStatus)> {
        leaves(&self.tree)
            .into_iter()
            .map(|id| {
                let st = self.pane(id).map_or(PaneStatus::Live, |p| p.status);
                (id, st)
            })
            .collect()
    }

    /// 套用布局预设(§5.2):就地重排树、关掉多余 pane,返回**待新建**的 pane id。
    ///
    /// 返回的 id 已经占好了树上的叶子位,但还没有 `PaneState`。调用方(app)为每个
    /// id 发起 `open_pty`,完成后调 [`Workspace::attach_pane`]。这段空窗期里
    /// 渲染层照常按几何画一块空 pane + "连接中"标题 —— 布局不会先塌一下再撑开。
    pub fn apply_preset(&mut self, preset: Preset) -> Vec<PaneId> {
        let plan = plan_preset(preset, &self.statuses());
        for id in &plan.close {
            self.panes.retain(|p| p.id != *id);
        }
        let mut ids = plan.keep;
        let mut fresh = Vec::new();
        for _ in 0..plan.spawn {
            let id = self.alloc_id();
            ids.push(id);
            fresh.push(id);
        }
        self.tree = preset_tree(preset, &ids);
        self.focus = next_focus(self.focus, &ids);
        fresh
    }

    /// 按 `layout.toml` 里存的树形状恢复分屏(F37,设计 E10)。语义与
    /// [`Workspace::apply_preset`] 完全一致:返回**待新建**的 pane id,
    /// 调用方为每个 id 发起 `open_pty`,完成后调 [`Workspace::attach_pane`]。
    ///
    /// 恢复的是**形状与比例**,pane 里的内容一律是新的 —— 终端 scrollback
    /// 从不落盘(设计 E2)。
    ///
    /// `main_leaf`(F160,设计 5.2①)= 已经连上的那块 pane 该落在**第几个**
    /// 叶子位上。恢复时主叶子是「前序第一个身份还连得上的叶子」,不一定是
    /// 第 0 个 —— 叶子 0 是个拨不了号的占位时,恒填 0 会让已连的 pane 占掉
    /// 本该空着的那格,主叶子反而空着(现象:恢复回来内容全串了一格)。
    /// 旧调用点传 `0` 行为不变。越界(文件被手改过)夹回 0,不 panic。
    ///
    /// `None` = 树编码损坏,**什么都不动**。校验放在任何 mutation 之前:
    /// 中途失败会留下一个树与 `panes` 对不上的工作区,那比不恢复糟得多。
    pub fn apply_saved_tree(
        &mut self,
        entries: &[mullion_store::SavedNodeEntry],
        focus_leaf: usize,
        main_leaf: usize,
    ) -> Option<Vec<PaneId>> {
        use crate::shell::layout_snapshot as snap;
        // 先验后改。`leaf_count` 通过就意味着下面的 `from_entries` 一定能拼出来
        // (结构完整 + id 数与叶子数相等,是它仅有的两个失败条件)。
        let want = snap::leaf_count(entries)?;
        let plan = preset::plan_for_count(want, &self.statuses());
        for id in &plan.close {
            self.panes.retain(|p| p.id != *id);
        }
        let keep = plan.keep.len();
        let mut ids = plan.keep;
        let mut fresh = Vec::new();
        for _ in 0..plan.spawn {
            let id = self.alloc_id();
            ids.push(id);
            fresh.push(id);
        }
        // 5.2①:把那块已连上的 pane 从 0 号叶子位挪到主叶子位。
        // `rotate_left(1)` 把 `[a, f1, f2]` 变成 `[f1, f2, a]` —— 其余叶子
        // 依次前移,前序顺序不乱。**只在恰好保留一块 pane 时挪位**:保留多块
        // 时「哪块算已连的那块」本身就没有定义,乱挪只会把内容互相换位。
        // 恢复路径上 `ws` 是刚建的,恒只有一块(`Workspace::new`)。
        if keep == 1 {
            let main = main_leaf.min(ids.len().saturating_sub(1));
            ids[0..=main].rotate_left(1);
        }
        self.tree = snap::from_entries(entries, &ids)
            .expect("leaf_count 已经验过结构,这里拼不出来说明两者的判据走样了");
        self.focus = ids[snap::sane_focus_leaf(focus_leaf, ids.len())];
        Some(fresh)
    }

    /// 异步 `open_pty` 完成后把 pane 挂进来(id 由 [`Workspace::apply_preset`] 预分配)。
    pub fn attach_pane(&mut self, pane: PaneState) {
        self.next_id = self.next_id.max(pane.id.0 + 1);
        self.panes.retain(|p| p.id != pane.id);
        self.panes.push(pane);
    }

    /// 关闭一个 pane(F31):树上兄弟顶替,`PaneState` 一并丢弃。
    /// 最后一个 pane 不可关,返回 `false` 且什么都不动。
    ///
    /// F140:**丢弃 `PaneState` 不等于关掉 channel**。russh 0.54.5 的
    /// `ChannelWriteHalf` 没有 `Drop` 实现,不显式 `close()` 的话远端 shell
    /// 会一直挂着,channel slot 泄漏到 sshd 的 `MaxSessions` 上限(默认 10)
    /// 之后,同一条连接再也开不出新分屏(adr-009 已列的失效模式)。
    pub fn close_pane(&mut self, id: PaneId) -> bool {
        if !close_pane(&mut self.tree, id) {
            return false;
        }
        if let Some(p) = self.panes.iter().find(|p| p.id == id) {
            p.pty.close();
        }
        self.panes.retain(|p| p.id != id);
        self.focus = next_focus(self.focus, &leaves(&self.tree));
        true
    }

    /// F140:关掉**所有** pane 的 channel。关标签时用(见 `app.rs::wind_down`)
    /// —— 那条路径不经过 `close_pane`,整个 `Workspace` 被直接 drop。
    ///
    /// 不改任何状态:调用方紧接着就要丢弃整个 `Workspace`,清空 `panes`
    /// 只是多一步没人观察得到的写。
    pub fn close_all_panes(&self) {
        for p in &self.panes {
            p.pty.close();
        }
    }

    /// 把焦点 pane 一分为二(F30)。返回新 pane 的 id(还没有 `PaneState`,
    /// 同 `apply_preset` 的空窗期约定)。B2-a 的 UI 不暴露这个入口,只留给
    /// 快捷键切片和测试 —— core 的 `split_pane` 需要一个真实调用方才不算死代码。
    pub fn split_focused(&mut self, dir: Dir) -> Option<PaneId> {
        let new_id = self.alloc_id();
        if split_pane(&mut self.tree, self.focus, new_id, dir, 0.5) {
            Some(new_id)
        } else {
            self.next_id -= 1;
            None
        }
    }

    /// 排空每个 pane 的 inbound,喂各自的仿真器,把 `Event::PtyWrite` 回写到
    /// **该 pane 自己的** channel(T1)。
    ///
    /// 写错 channel 的后果不是"输出乱一点":发起同步输出探测 / 光标位置查询的
    /// TUI 会永远等不到应答,表现为整块 pane 冻死。
    pub fn pump(&mut self, now_ms: u64) {
        let link_alive = self.link_alive;
        let hosts = &self.hosts;
        for p in &mut self.panes {
            let mut inbound: Vec<Vec<u8>> = Vec::new();
            loop {
                match p.rx.try_recv() {
                    Ok(chunk) => {
                        p.pacer.feed(&chunk, now_ms);
                        inbound.push(chunk);
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        // §6.3:内容保留(可滚可复制),只是不再收发。
                        // F128:关掉的成因决定要不要重连,判据见 `rx_closed_action`。
                        //
                        // **只有 `Live` 才做这次判定** —— 状态转移只能从 `Live`
                        // 出发。已经是 `Reconnecting` 的 pane 手里那条 `rx` 属于
                        // **旧**连接,而 `link_alive` 查的是 `hosts[host_ix]` 的
                        // **当前**连接:重连成功就地替换过之后那条是活的,判据会
                        // 返回 `UserExited`,把一块正等着新 channel 的 pane 打成
                        // `Disconnected`——`hosts_to_redial` 从此不再管它,那块
                        // 分屏永久停在"已断开",而同一条连接上的其它分屏都回来了。
                        // (同一条 host 上各 pane 的 `rx` 不保证同一帧关闭,所以
                        // 这个窗口是真实存在的,不是理论假设。)
                        //
                        // `Disconnected` 同理不再重判:它是终态,只有 `reattach_pane`
                        // /`rehost_pane` 换 channel 才能离开。
                        if p.status == PaneStatus::Live {
                            p.status = match rx_closed_action(link_alive(hosts, p.host_ix)) {
                                RxClosed::Reconnect => PaneStatus::Reconnecting,
                                RxClosed::UserExited => PaneStatus::Disconnected,
                            };
                        }
                        break;
                    }
                }
            }
            // ⑥:远端状态。**必须在 `inbound.is_empty()` 的 `continue` 之前**——
            // OSC 字节喂进 `emulator` 的路径不止「这一帧从 rx 收到新字节」这一条
            // (比如测试直接调 `emulator.feed`),放在 `continue` 之后会让这些
            // 情形永远收集不到。两个字段的重置策略不同,见 `PaneState::cwd` /
            // `::tmux` 的文档。
            if let Some(st) = p.emulator.take_remote_state() {
                if let Some(cwd) = st.cwd {
                    p.cwd = Some(cwd);
                }
                if st.title_seen {
                    p.tmux = st.tmux;
                }
            }
            if inbound.is_empty() {
                continue;
            }
            // 粘性置位。必须在 `continue` **之后** —— 放前面的话每个 pane 每帧
            // 都会被置位,自动化会在一条还没说过话的 channel 上开跑。
            p.saw_first_byte = true;
            let out = session_pump::pump(&mut p.emulator, &inbound);
            if !out.is_empty() {
                let _ = p.pty.write(out);
            }
        }
    }

    /// 所有 pane 里最早的那个同步块超时点(T2)。app 拿它排一次 `WaitUntil`。
    pub fn vt_sync_deadline(&self) -> Option<Instant> {
        self.panes
            .iter()
            .filter_map(|p| p.emulator.vt_sync_deadline())
            .min()
    }

    /// 到点的同步块就地收口,返回有没有任何一个 pane 真的收了口(收了就得出帧,
    /// 否则字节进了 `Term` 却没人画)。
    ///
    /// **必须独立于 `pump`**:超时这条路径的前提就是「没有新字节进来」,挂在
    /// 排空 rx 的循环里(那里 `inbound.is_empty()` 就 `continue`)等于没接。
    /// 详见 `Emulator::flush_expired_sync` 的文档。
    pub fn flush_expired_vt_sync(&mut self, now: Instant) -> bool {
        let mut flushed = false;
        for p in &mut self.panes {
            flushed |= p.emulator.flush_expired_sync(now);
        }
        flushed
    }

    /// 施加几何(F34/T4)。**四条触发路径**(套预设 / 关 pane / 窗口 resize /
    /// 标题条开关)全走这一个函数,只有这一份代码 —— 分散写迟早漏掉一条,
    /// 而漏掉的那条会表现成"某种操作之后远端排版就错了",极难定位。
    pub fn apply_geometry(&mut self, geoms: &[PaneGeom]) {
        for g in geoms {
            let Some(p) = self.panes.iter_mut().find(|p| p.id == g.id) else {
                continue; // 还在开 channel 的 pane,attach 时会补一次
            };
            if p.last_grid == g.grid {
                continue;
            }
            // 本地仿真器始终跟着新几何走:标题条/分隔线扣减后的像素区域已经
            // 真的变了,这是本地渲染要用的尺寸,和远端有没有收到 window_change
            // 是两回事 —— 不跟着改,本地渲染立刻就会错(不用等远端确认)。
            p.emulator.resize(g.grid.0, g.grid.1);
            Self::report_history_clamp(p);
            // 只有远端确认收到(Ok)才推进 last_grid;`TrySendErr::Full`(出站队列
            // 满,高延迟链路 + 大段粘贴时是有据可查的真实场景,见
            // mullion-ssh/src/session.rs 的 TrySendErr 注释)时保留旧值,让下一次
            // 同一份几何的 apply_geometry 重试发送 —— 否则这次 window_change 会
            // 静默丢失,远端永远收不到正确尺寸(T4)。`Closed`(真断线)时同样不推进:
            // 虽然基本无害(pump() 很快会把该 pane 标 Disconnected,不会再有人为
            // 它调 apply_geometry),但 TrySendErr 只有 Full/Closed 两个变体,
            // 没必要为无害的一支单开分支,统一按失败处理更简单。
            if p.pty.resize(g.grid.0, g.grid.1).is_ok() {
                p.last_grid = g.grid;
            }
        }
    }

    /// F17:实际生效的 scrollback 行数变了就落一行日志(只在被预算夹到时)。
    ///
    /// **为什么必须说话**:`Emulator` 会按 `HISTORY_BUDGET_BYTES` 把宽 pane 的
    /// 回溯行数往下夹(N5 内存红线的兜底)。夹紧本身是对的,但用户看到的是
    /// 「我配了 10000 行,往上翻到 2700 行就没了」——不落日志的话这条线索
    /// 根本无处可查,又是一次「配了没反应还不说为什么」。
    ///
    /// 去重靠 `history_reported`:`apply_geometry` 已经按 `last_grid` 挡掉了
    /// 没变化的几何,但拖窗口本身会连续产生几十个不同的列数。
    fn report_history_clamp(p: &mut PaneState) {
        let actual = p.emulator.history_lines();
        if actual == p.history_reported {
            return;
        }
        p.history_reported = actual;
        let requested = p.emulator.requested_history();
        if actual < requested {
            log::info!(
                target: "mullion",
                "pane {}:回溯 {requested} 行超出单 pane 内存预算({} 列),实际生效 {actual} 行",
                p.id.0,
                p.emulator.cols(),
            );
        }
    }

    /// 有没有任何 pane 还活着。状态栏的连接指示灯用。
    pub fn any_live(&self) -> bool {
        self.panes.iter().any(|p| p.status == PaneStatus::Live)
    }

    /// 测试脚手架:把树扩到 n 个叶子,让 `ws_with(n)` 能造出多 pane 场景。
    #[cfg(test)]
    fn tree_mut_for_test(&mut self, new_id: u32) {
        let target = self.focus;
        split_pane(&mut self.tree, target, PaneId(new_id), Dir::Horizontal, 0.5);
        self.next_id = self.next_id.max(new_id + 1);
    }
}

/// 供 `app.rs` 等跨模块测试复用的脚手架(F128:`reattach_pane`/`rehost_pane`
/// 的测试要造 `Workspace`/`fake_pane`,原先这些都锁在 `tests` 模块里出不来)。
/// **只挪不改**——内容与原先 `mod tests` 里的逐字节一致,只是换了个模块名
/// 并把要跨模块用的项标了 `pub`。
#[cfg(test)]
pub mod tests_support {
    use super::*;
    use crate::theme::{term_default_colors, MULLION_DARK};
    use std::sync::Mutex;

    /// 测试替身。`SshSession` 的字段与 `SshCmd` 都是 mullion-ssh 私有,跨 crate
    /// 造不出来 —— 这就是 `PtyWriter` trait 存在的理由(实现决策 1)。
    #[derive(Default)]
    struct FakePty {
        writes: Arc<Mutex<Vec<Vec<u8>>>>,
        resizes: Arc<Mutex<Vec<(u16, u16)>>>,
        /// 开关:置真后 `resize` 一律返回 `Err(TrySendErr::Full)`,模拟出站队列满
        /// (高延迟链路 + 大段粘贴时的真实场景,见 mullion-ssh/src/session.rs 的
        /// `TrySendErr` 注释),用来测 apply_geometry 的重试语义。
        resize_fails: Arc<Mutex<bool>>,
        /// F140:`close()` 被调了几次。**计数而不是布尔** —— 重复关是无害的
        /// (io_task 已退出时 try_send 失败即忽略),但一次都没有就是泄漏。
        closes: Arc<Mutex<usize>>,
    }

    impl PtyWriter for FakePty {
        fn write(&self, bytes: Vec<u8>) -> Result<(), TrySendErr> {
            self.writes.lock().unwrap().push(bytes);
            Ok(())
        }
        fn resize(&self, cols: u16, rows: u16) -> Result<(), TrySendErr> {
            if *self.resize_fails.lock().unwrap() {
                return Err(TrySendErr::Full);
            }
            self.resizes.lock().unwrap().push((cols, rows));
            Ok(())
        }
        fn close(&self) {
            *self.closes.lock().unwrap() += 1;
        }
    }

    pub struct Probe {
        pub writes: Arc<Mutex<Vec<Vec<u8>>>>,
        pub resizes: Arc<Mutex<Vec<(u16, u16)>>>,
        pub resize_fails: Arc<Mutex<bool>>,
        pub closes: Arc<Mutex<usize>>,
        pub tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    }

    /// 造一个挂着 FakePty 的 pane,并把它的输入端 / 观测端一起返回。
    pub fn fake_pane(id: u32) -> (PaneState, Probe) {
        let pty = FakePty::default();
        let probe_writes = pty.writes.clone();
        let probe_resizes = pty.resizes.clone();
        let probe_resize_fails = pty.resize_fails.clone();
        let probe_closes = pty.closes.clone();
        let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(16);
        let mut emulator = mullion_term::emulator::Emulator::new(80, 24);
        let d = term_default_colors(&MULLION_DARK);
        emulator.set_default_colors(d.fg, d.bg);
        (
            PaneState {
                id: PaneId(id),
                host_ix: 0,
                emulator,
                pty: Box::new(pty),
                rx,
                pacer: crate::render::SyncFramePacer::new(),
                status: PaneStatus::Live,
                saw_first_byte: false,
                last_grid: (80, 24),
                cwd: None,
                tmux: None,
                history_reported: 0,
                host_pending: false,
                notice: None,
            },
            Probe {
                writes: probe_writes,
                resizes: probe_resizes,
                resize_fails: probe_resize_fails,
                closes: probe_closes,
                tx,
            },
        )
    }

    pub fn ws_with(n: u32) -> (Workspace, Vec<Probe>) {
        let (first, p0) = fake_pane(1);
        let mut ws = Workspace::new(first, 0);
        let mut probes = vec![p0];
        for id in 2..=n {
            let (p, probe) = fake_pane(id);
            ws.attach_pane(p);
            probes.push(probe);
            ws.tree_mut_for_test(id);
        }
        (ws, probes)
    }

    /// 一条崭新的、没接任何东西的 PTY 管道 —— 换 channel 那两条路径要拿它当
    /// 「新开好的 channel」。**连观测端一起给**:要验证「pty/rx 真的换过去了」,
    /// 就得能分辨字节到底进了哪一条管子。
    pub fn fresh_pipe_probed() -> (
        Box<dyn super::PtyWriter>,
        tokio::sync::mpsc::Receiver<Vec<u8>>,
        Probe,
    ) {
        let (p, probe) = fake_pane(999);
        (p.pty, p.rx, probe)
    }

    /// 不关心观测端时的简写。
    pub fn fresh_pipe() -> (
        Box<dyn super::PtyWriter>,
        tokio::sync::mpsc::Receiver<Vec<u8>>,
    ) {
        let (pty, rx, probe) = fresh_pipe_probed();
        std::mem::forget(probe); // 发送端留着,免得 rx 立刻变成 Disconnected
        (pty, rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tests_support::{fake_pane, ws_with};

    /// T1:光标位置查询的应答必须回写到**产生它的那个 pane 自己的** channel。
    /// 串到别的 pane 上去,发起查询的 TUI 就永远等不到应答 → 全屏 TUI 冻死。
    #[tokio::test]
    async fn pty_write_goes_to_its_own_pane_channel_t1() {
        let (mut ws, probes) = ws_with(2);
        // 只给 2 号 pane 喂 DSR 6 查询。
        probes[1].tx.send(b"\x1b[6n".to_vec()).await.unwrap();
        // send 之后 try_recv 才看得到,给 runtime 一次让步。
        tokio::task::yield_now().await;
        ws.pump(0);
        assert!(
            probes[0].writes.lock().unwrap().is_empty(),
            "1 号 pane 没被查询,不该有任何回写(T1 串台)"
        );
        assert_eq!(
            probes[1].writes.lock().unwrap().as_slice(),
            &[b"\x1b[1;1R".to_vec()],
            "2 号 pane 的 CPR 应答没回到自己的 channel(T1)"
        );
    }

    /// 首字节检测:只有真的收到过入站字节的 pane 才置位。
    ///
    /// 不让 `pump` 返回 `Vec<PaneId>`:`pump` 每帧必调,返回 Vec 等于每帧一次
    /// 堆分配,本项目对帧路径上的无谓开销一向敏感(T3)。挂个 bool 在
    /// `PaneState` 上零分配,而且「这个 pane 收到过字节没有」本来就是
    /// pane 自己的状态。
    ///
    /// 自证会变红:把 `pump` 里 `p.saw_first_byte = true;` 挪到
    /// `if inbound.is_empty() { continue; }` **之前**(1 号 pane 会被误置位)。
    #[tokio::test]
    async fn pump_marks_saw_first_byte_only_for_panes_with_inbound() {
        let (mut ws, probes) = ws_with(2);
        assert!(
            !ws.pane(PaneId(1)).unwrap().saw_first_byte,
            "初值必须是 false"
        );
        assert!(!ws.pane(PaneId(2)).unwrap().saw_first_byte);

        // 只给 2 号 pane 喂字节。
        probes[1].tx.send(b"hello".to_vec()).await.unwrap();
        tokio::task::yield_now().await;
        ws.pump(0);

        assert!(
            !ws.pane(PaneId(1)).unwrap().saw_first_byte,
            "1 号 pane 什么都没收到,不该置位 —— 置位了就会让自动化在一条还没
             说话的 channel 上开跑"
        );
        assert!(
            ws.pane(PaneId(2)).unwrap().saw_first_byte,
            "2 号 pane 收到了字节却没置位 —— 自动化会一直等到超时"
        );
    }

    /// 置位之后不会被后续的空帧清掉:它是「历史上收到过」而不是「这一帧收到了」。
    ///
    /// 自证会变红:把 `p.saw_first_byte = true;` 改成
    /// `p.saw_first_byte = !inbound.is_empty();` 并挪到 `continue` 之前。
    #[tokio::test]
    async fn saw_first_byte_is_sticky_across_idle_frames() {
        let (mut ws, probes) = ws_with(1);
        probes[0].tx.send(b"hello".to_vec()).await.unwrap();
        tokio::task::yield_now().await;
        ws.pump(0);
        assert!(ws.pane(PaneId(1)).unwrap().saw_first_byte);

        ws.pump(16); // 空帧
        assert!(
            ws.pane(PaneId(1)).unwrap().saw_first_byte,
            "首字节标志必须是粘的,否则空帧会把它清掉、自动化永远等不到就绪"
        );
    }

    /// T4/F34:布局一变,**每个**尺寸真的变了的 pane 都要收到 window_change。
    /// 漏发 → 远端 TUI 按旧列数排版 → tmux 里全屏应用直接错行。
    ///
    /// 设计文档 §9 把它叫 `preset_change_emits_resize_for_every_pane_f34`;
    /// 这里叫 geometry_ 是因为四条触发路径(切预设 / 关 pane / 窗口 resize /
    /// 开关标题条)统一收敛到 `apply_geometry` 一个出口,测的是那个出口而不是
    /// 「切预设」这一条路径。名字对齐实现,免得后来者以为还有三条没测。
    #[test]
    fn geometry_change_emits_resize_for_every_pane_f34() {
        let (mut ws, probes) = ws_with(2);
        let geoms = vec![
            PaneGeom {
                id: PaneId(1),
                px: PxRect {
                    x: 0,
                    y: 0,
                    w: 400,
                    h: 600,
                },
                title_px: PxRect {
                    x: 0,
                    y: 0,
                    w: 400,
                    h: 0,
                },
                term_px: PxRect {
                    x: 0,
                    y: 0,
                    w: 399,
                    h: 600,
                },
                grid: (39, 30),
            },
            PaneGeom {
                id: PaneId(2),
                px: PxRect {
                    x: 400,
                    y: 0,
                    w: 400,
                    h: 600,
                },
                title_px: PxRect {
                    x: 400,
                    y: 0,
                    w: 400,
                    h: 0,
                },
                term_px: PxRect {
                    x: 400,
                    y: 0,
                    w: 400,
                    h: 600,
                },
                grid: (40, 30),
            },
        ];
        ws.apply_geometry(&geoms);
        assert_eq!(probes[0].resizes.lock().unwrap().as_slice(), &[(39, 30)]);
        assert_eq!(probes[1].resizes.lock().unwrap().as_slice(), &[(40, 30)]);
        // 仿真器也得跟着改,否则本地渲染按旧尺寸、远端按新尺寸。
        assert_eq!(ws.pane(PaneId(1)).unwrap().emulator.snapshot().cols, 39);
    }

    /// 尺寸没变就别发:每帧无脑 window_change 会把远端 SIGWINCH 刷爆,
    /// tmux 里的 TUI 会不停重排。
    #[test]
    fn unchanged_geometry_emits_no_resize() {
        let (mut ws, probes) = ws_with(1);
        let geoms = vec![PaneGeom {
            id: PaneId(1),
            px: PxRect {
                x: 0,
                y: 0,
                w: 800,
                h: 480,
            },
            title_px: PxRect {
                x: 0,
                y: 0,
                w: 800,
                h: 0,
            },
            term_px: PxRect {
                x: 0,
                y: 0,
                w: 800,
                h: 480,
            },
            grid: (80, 24), // 与 fake_pane 的 last_grid 相同
        }];
        ws.apply_geometry(&geoms);
        assert!(
            probes[0].resizes.lock().unwrap().is_empty(),
            "尺寸未变却发了 window_change"
        );
    }

    /// F17:pane 一拖宽,回溯行数就得按新列数重夹一次 —— 而这只可能发生在
    /// `apply_geometry` 里(pane 全是按 80×24 占位建出来的,真实列数只从
    /// 这里来)。
    ///
    /// 这条守的是**记账那一半**:`history_reported` 不跟着走的话,拖窗口
    /// 途中每一个中间列数都会重复落一行「已夹紧」,把日志刷爆。
    ///
    /// **它守不住「grid 上的行真被砍了」**:`history_lines()` 是按当前列数
    /// 重算一遍的纯函数,跟 grid 的实际状态无关 —— 拿掉
    /// `Emulator::resize` 里的 `apply_history_budget()` 这条测试照绿
    /// (实测过)。那一半由 `emulator::tests::resize_reclamps_history_to_the_byte_budget`
    /// 用真实行内容守,不要把职责搬到这里来。
    ///
    /// 自证会变红:把 `apply_geometry` 里的 `Self::report_history_clamp(p);` 删掉。
    #[test]
    fn a_wider_pane_reclamps_its_scrollback_and_reports_once() {
        let (mut ws, _probes) = ws_with(1);
        let wide = |cols: u16| {
            vec![PaneGeom {
                id: PaneId(1),
                px: PxRect {
                    x: 0,
                    y: 0,
                    w: 8000,
                    h: 480,
                },
                title_px: PxRect {
                    x: 0,
                    y: 0,
                    w: 8000,
                    h: 0,
                },
                term_px: PxRect {
                    x: 0,
                    y: 0,
                    w: 8000,
                    h: 480,
                },
                grid: (cols, 24),
            }]
        };
        let requested = ws.panes()[0].emulator.requested_history();
        ws.apply_geometry(&wide(4000));
        let clamped = ws.panes()[0].emulator.history_lines();
        assert!(
            clamped < requested,
            "4000 列下 {requested} 行远超单 pane 内存预算,却一行没夹(N5)"
        );
        assert_eq!(
            ws.panes()[0].history_reported,
            clamped,
            "夹紧了却没记账 —— 拖窗口时每个中间列数都会重复落一次日志"
        );
    }

    /// T4:`pty.resize` 失败(出站队列满)不能让这次 window_change 静默消失 ——
    /// `last_grid` 必须保留旧值,好让**下一次同一份几何**的 apply_geometry 重试。
    /// 若像修复前那样无条件写 `last_grid`,失败之后 `last_grid == g.grid` 恒成立,
    /// 同一份几何永远不会再重试,远端就此卡在错误尺寸上。
    #[test]
    fn failed_pty_resize_keeps_last_grid_stale_so_next_apply_retries_t4() {
        let (mut ws, probes) = ws_with(1);
        let geoms = vec![PaneGeom {
            id: PaneId(1),
            px: PxRect {
                x: 0,
                y: 0,
                w: 800,
                h: 480,
            },
            title_px: PxRect {
                x: 0,
                y: 0,
                w: 800,
                h: 0,
            },
            term_px: PxRect {
                x: 0,
                y: 0,
                w: 800,
                h: 480,
            },
            grid: (100, 40),
        }];

        *probes[0].resize_fails.lock().unwrap() = true;
        ws.apply_geometry(&geoms);
        assert!(
            probes[0].resizes.lock().unwrap().is_empty(),
            "resize 失败不该被记成功"
        );
        assert_eq!(
            ws.pane(PaneId(1)).unwrap().last_grid,
            (80, 24),
            "pty.resize 失败时 last_grid 必须保留旧值,否则下次同一份几何不会重试"
        );
        // 本地仿真器仍然跟着渲染区域走:该区域的像素尺寸已经真的变了,
        // 这与远端有没有收到 window_change 是两回事。
        assert_eq!(ws.pane(PaneId(1)).unwrap().emulator.snapshot().cols, 100);

        // 队列不满了,同一份几何再来一次应该重试成功。
        *probes[0].resize_fails.lock().unwrap() = false;
        ws.apply_geometry(&geoms);
        assert_eq!(probes[0].resizes.lock().unwrap().as_slice(), &[(100, 40)]);
        assert_eq!(ws.pane(PaneId(1)).unwrap().last_grid, (100, 40));
    }

    /// §6.3:对端断开(channel 关闭)→ pane 标记 Disconnected,内容留着可滚可复制。
    #[tokio::test]
    async fn closed_channel_marks_pane_disconnected() {
        let (mut ws, probes) = ws_with(1);
        assert_eq!(ws.pane(PaneId(1)).unwrap().status, PaneStatus::Live);
        drop(probes);
        tokio::task::yield_now().await;
        ws.pump(0);
        assert_eq!(ws.pane(PaneId(1)).unwrap().status, PaneStatus::Disconnected);
    }

    #[test]
    fn apply_preset_reuses_panes_and_reports_the_ids_to_open() {
        let (mut ws, _p) = ws_with(1);
        let fresh = ws.apply_preset(Preset::FourGrid);
        assert_eq!(fresh.len(), 3, "1 屏 → 4 屏要新开 3 条 channel");
        assert_eq!(
            mullion_core::layout::leaves(ws.tree()).len(),
            4,
            "树上必须先有 4 个叶子,新 pane 还在连的时候画占位"
        );
        assert!(!fresh.contains(&PaneId(1)), "已有 pane 不该被重开");
    }

    /// F37/E10:刚连上的工作区只有 1 个 pane,按存下来的树恢复出 3 屏。
    ///
    /// 断言的是「形状与比例逐字段回来」,不是「叶子数对」—— 叶子数对而
    /// 方向/比例错,用户看到的是一个他从没摆过的布局,却又说不上哪里不对。
    #[test]
    fn apply_saved_tree_rebuilds_the_exact_shape_and_reports_the_ids_to_open() {
        use mullion_store::{SavedDir, SavedNodeEntry};
        let (mut ws, _p) = ws_with(1);
        // H0.25(L, V0.75(L, L))
        let entries = [
            SavedNodeEntry::split(SavedDir::Horizontal, 0.25),
            SavedNodeEntry::leaf(),
            SavedNodeEntry::split(SavedDir::Vertical, 0.75),
            SavedNodeEntry::leaf(),
            SavedNodeEntry::leaf(),
        ];
        let fresh = ws.apply_saved_tree(&entries, 2, 0).expect("编码是好的");
        assert_eq!(fresh.len(), 2, "1 屏 → 3 屏要新开 2 条 channel");
        assert!(!fresh.contains(&PaneId(1)), "已有 pane 不该被重开");

        let leaves = mullion_core::layout::leaves(ws.tree());
        assert_eq!(leaves.len(), 3);
        assert_eq!(leaves[0], PaneId(1), "已有 pane 填第一个叶子位");
        let Node::Split { dir, ratio, b, .. } = ws.tree() else {
            panic!("根该是一个分割节点");
        };
        assert_eq!(*dir, Dir::Horizontal);
        assert_eq!(*ratio, 0.25, "比例是用户拖出来的,不能回落成 0.5");
        let Node::Split {
            dir: inner_dir,
            ratio: inner_ratio,
            ..
        } = b.as_ref()
        else {
            panic!("右子树该是一个分割节点");
        };
        assert_eq!(*inner_dir, Dir::Vertical, "嵌套那层的方向也得对");
        assert_eq!(*inner_ratio, 0.75);

        assert_eq!(ws.focus(), leaves[2], "焦点该落在存下来的那个叶子上");
    }

    /// 树编码坏了 → **什么都不动**。中途失败会留下一个树与 `panes` 对不上的
    /// 工作区,那比不恢复糟得多(渲染层按树画,画到一个没有 `PaneState` 的
    /// 叶子上就是一块永远空着的 pane)。
    #[test]
    fn apply_saved_tree_leaves_the_workspace_untouched_when_the_encoding_is_corrupt() {
        use mullion_store::{SavedDir, SavedNodeEntry};
        let (mut ws, _p) = ws_with(2);
        ws.set_focus(PaneId(2));
        let before = ws.tree().clone();
        // 分割节点少了一棵子树。
        let broken = [
            SavedNodeEntry::split(SavedDir::Horizontal, 0.5),
            SavedNodeEntry::leaf(),
        ];
        assert!(ws.apply_saved_tree(&broken, 0, 0).is_none());
        assert_eq!(ws.tree(), &before, "树被动过了");
        assert_eq!(ws.pane_count(), 2, "pane 被动过了");
        assert_eq!(ws.focus(), PaneId(2), "焦点被动过了");
    }

    /// 焦点叶子序号越界(文件被手改、或树被改小了)→ 落回第一个叶子,
    /// 不 panic、不留在一个不存在的 pane 上。
    #[test]
    fn apply_saved_tree_clamps_an_out_of_range_focus_leaf() {
        use mullion_store::SavedNodeEntry;
        let (mut ws, _p) = ws_with(1);
        ws.apply_saved_tree(&[SavedNodeEntry::leaf()], 7, 0)
            .unwrap();
        assert_eq!(ws.focus(), PaneId(1));
    }

    /// 设计 5.2①:已连上的那块 pane 必须落在**主叶子**位上。
    ///
    /// `apply_saved_tree` 原本把已有 pane 恒填第一个叶子位(`ids = keep + fresh`),
    /// 而主叶子是「前序第一个**身份还连得上**的叶子」—— 叶子 0 是个占位
    /// (会话被删了)时,已连的 pane 会占掉本该空着的那格,主叶子反而空着,
    /// 现象是「恢复回来内容全串了一格」。
    ///
    /// 自证会变红:把 `main_leaf` 参数忽略掉、恒按 0 处理。
    #[test]
    fn the_connected_pane_lands_on_the_main_leaf_not_the_first_leaf() {
        use mullion_store::{SavedDir, SavedNodeEntry};
        let (mut ws, _probes) = ws_with(1);
        let entries = vec![
            SavedNodeEntry::split(SavedDir::Horizontal, 0.5),
            SavedNodeEntry::leaf(),
            SavedNodeEntry::leaf(),
        ];
        ws.apply_saved_tree(&entries, 0, 1).expect("结构完整");
        let leaves = mullion_core::layout::leaves(ws.tree());
        assert_eq!(leaves.len(), 2);
        assert_eq!(
            leaves[1],
            PaneId(1),
            "已连上的那块 pane 该落在主叶子(第 1 个)位上,实际落在了 {leaves:?}"
        );
    }

    /// 旧调用点传 0 时行为与改动前逐字节一致。
    #[test]
    fn passing_zero_keeps_the_old_placement() {
        use mullion_store::{SavedDir, SavedNodeEntry};
        let (mut ws, _probes) = ws_with(1);
        let entries = vec![
            SavedNodeEntry::split(SavedDir::Horizontal, 0.5),
            SavedNodeEntry::leaf(),
            SavedNodeEntry::leaf(),
        ];
        ws.apply_saved_tree(&entries, 0, 0).expect("结构完整");
        assert_eq!(mullion_core::layout::leaves(ws.tree())[0], PaneId(1));
    }

    /// 越界的 `main_leaf`(文件被手改过)夹回 0,不 panic —— 恢复是启动路径,
    /// 为一份坏文件崩掉整个进程不成比例。
    #[test]
    fn an_out_of_range_main_leaf_is_clamped() {
        use mullion_store::SavedNodeEntry;
        let (mut ws, _probes) = ws_with(1);
        ws.apply_saved_tree(&[SavedNodeEntry::leaf()], 0, 9)
            .expect("单叶子");
        assert_eq!(mullion_core::layout::leaves(ws.tree()), vec![PaneId(1)]);
    }

    #[test]
    fn apply_preset_drops_panes_and_moves_focus_off_the_closed_one() {
        let (mut ws, _p) = ws_with(2);
        ws.set_focus(PaneId(2));
        let fresh = ws.apply_preset(Preset::Single);
        assert!(fresh.is_empty());
        assert_eq!(mullion_core::layout::leaves(ws.tree()), vec![PaneId(1)]);
        assert!(ws.pane(PaneId(2)).is_none(), "被关的 pane 状态该一起清掉");
        assert_eq!(ws.focus(), PaneId(1), "焦点必须落到存活 pane 上");
    }

    #[test]
    fn close_pane_refuses_to_kill_the_last_one_f31() {
        let (mut ws, _p) = ws_with(1);
        assert!(!ws.close_pane(PaneId(1)), "最后一个 pane 不可关");
        assert!(ws.pane(PaneId(1)).is_some());
    }

    #[test]
    fn close_pane_promotes_sibling_f31() {
        let (mut ws, _p) = ws_with(2);
        ws.set_focus(PaneId(2));
        assert!(ws.close_pane(PaneId(2)));
        assert_eq!(mullion_core::layout::leaves(ws.tree()), vec![PaneId(1)]);
        assert_eq!(ws.focus(), PaneId(1));
    }

    /// F140:关分屏必须**显式**关掉那条 SSH channel。
    ///
    /// 光丢弃 `PaneState` 不够:那条路径最终走到 `io_task` 的 `None` 分支,
    /// 而 russh 0.54.5 的 `ChannelWriteHalf` 没有 `Drop` —— 远端 shell 一直
    /// 挂着,channel slot 泄漏到 sshd 的 `MaxSessions` 之后同一条连接再也
    /// 开不出新分屏。
    ///
    /// 自证会变红:把 `close_pane` 里那句 `p.pty.close()` 删掉。
    #[test]
    fn closing_a_pane_closes_its_channel_f140() {
        let (mut ws, p) = ws_with(2);
        assert_eq!(*p[1].closes.lock().unwrap(), 0, "还没关就已经调过 close");
        assert!(ws.close_pane(PaneId(2)));
        assert_eq!(
            *p[1].closes.lock().unwrap(),
            1,
            "关分屏没有关掉它的 channel —— 远端 shell 会挂着不死(F140)"
        );
        assert_eq!(
            *p[0].closes.lock().unwrap(),
            0,
            "只该关被关的那块,不能顺手把留下的那块也关了"
        );
    }

    /// F140:关标签要关掉**每一块** pane 的 channel,不是只关焦点那块。
    ///
    /// 自证会变红:把 `close_all_panes` 的循环改成只关 `self.focus` 那块。
    #[test]
    fn closing_every_pane_closes_every_channel_f140() {
        let (ws, p) = ws_with(3);
        ws.close_all_panes();
        for (i, probe) in p.iter().enumerate() {
            assert_eq!(
                *probe.closes.lock().unwrap(),
                1,
                "第 {} 块 pane 的 channel 没关 —— 关标签会在远端留下挂着的 shell(F140)",
                i + 1
            );
        }
    }

    /// 新分配的 id 不能撞已有 pane —— 撞了就是两个 PaneState 抢一个叶子位,
    /// 渲染和输入会随机挑一个,表现成"输入偶尔跑到另一块去"。
    #[test]
    fn ids_are_never_reused() {
        let (mut ws, _p) = ws_with(2);
        ws.apply_preset(Preset::Single);
        let fresh = ws.apply_preset(Preset::TwoLeftRight);
        assert_eq!(fresh, vec![PaneId(3)], "不能回收 2 号");
    }

    /// F30 成功路径:焦点 pane 一分为二,新 id 落在树的叶子位;`split_focused`
    /// 本身不挪焦点(留给调用方决定要不要切焦点到新 pane)。
    #[test]
    fn split_focused_splits_the_tree_and_returns_the_new_id_f30() {
        let (mut ws, _p) = ws_with(1);
        let new_id = ws.split_focused(Dir::Horizontal);
        assert_eq!(new_id, Some(PaneId(2)), "1 号 pane 之后该分到 2 号");
        assert_eq!(
            mullion_core::layout::leaves(ws.tree()),
            vec![PaneId(1), PaneId(2)],
            "原 pane 在前(a 支),新 pane 在后(b 支),DFS 顺序"
        );
        assert_eq!(
            ws.focus(),
            PaneId(1),
            "split_focused 不该顺带把焦点挪到新 pane"
        );
    }

    /// core 的 `split_pane` 在 target 不在树里时返回 `false`;这在正常使用下不该
    /// 发生(焦点恒指向存活叶子),但要锁住失败分支的状态回滚 —— 分配失败不能
    /// 留下"分配过、没用上"的 id 缺口,也不能把树改坏。
    #[test]
    fn split_focused_rolls_back_next_id_when_target_is_missing() {
        let (mut ws, _p) = ws_with(1);
        let next_id_before = ws.next_id;
        ws.focus = PaneId(999); // 人为构造 focus 指向树上不存在的叶子
        let result = ws.split_focused(Dir::Horizontal);
        assert_eq!(result, None, "target 不在树里,split 必须失败");
        assert_eq!(
            ws.next_id, next_id_before,
            "分配失败要把预支的 id 吐回去,否则 id 白白跳号"
        );
        assert_eq!(
            mullion_core::layout::leaves(ws.tree()),
            vec![PaneId(1)],
            "失败时树不该被动"
        );
    }

    /// ⑥:远端报出来的 cwd / tmux 名要落到 `PaneState` 上。
    ///
    /// **两个字段的更新策略不同**,一条测试同时钉住:
    /// - `cwd` 只增不清:拿不到新值时保留上一个已知值(比闪成「未知」有用)。
    /// - `tmux` 收到新标题就整体重置,**包括重置成 `None`** —— 用户退出 tmux
    ///   之后 bash 会发自己的标题,这时必须把会话名清掉,否则标题条上会永久
    ///   挂着一个已经不存在的会话名。
    ///
    /// 自证会变红:把 `pump` 里 `if st.title_seen { p.tmux = st.tmux }` 改成
    /// `if let Some(t) = st.tmux { p.tmux = Some(t) }`(第三段红);把
    /// `if let Some(cwd) = st.cwd` 改成无条件 `p.cwd = st.cwd`(第四段红 ——
    /// 第二、三段里 `st.cwd` 恒 `Some`:段 2 走 OSC 7,段 3 的标题
    /// `dev@h: /tmp` 本身带路径,`parse_title` 会兜底解出来,所以这两段验
    /// 不出这个变异;第四段的标题 `plain:0:bash` 不含任何路径 token,
    /// `st.cwd` 才真的是 `None`,专门冲着「只增不清」去)。
    #[test]
    fn remote_state_lands_on_the_pane_with_the_right_reset_policy() {
        let (first, _probe) = fake_pane(1);
        let mut ws = Workspace::new(first, 0);
        let id = PaneId(1);

        // 1. tmux 里,报了目录
        ws.pane_mut(id)
            .unwrap()
            .emulator
            .feed(b"\x1b]2;work:0:bash\x07");
        ws.pane_mut(id)
            .unwrap()
            .emulator
            .feed(b"\x1b]7;file://h/home/dev/Mullion\x07");
        ws.pump(0);
        assert_eq!(
            ws.pane(id).unwrap().cwd.as_deref(),
            Some(&b"/home/dev/Mullion"[..])
        );
        assert_eq!(ws.pane(id).unwrap().tmux.as_deref(), Some("work"));

        // 2. 只报了新目录 —— tmux 名不该被这一批清掉
        ws.pane_mut(id)
            .unwrap()
            .emulator
            .feed(b"\x1b]7;file://h/tmp\x07");
        ws.pump(0);
        assert_eq!(ws.pane(id).unwrap().cwd.as_deref(), Some(&b"/tmp"[..]));
        assert_eq!(
            ws.pane(id).unwrap().tmux.as_deref(),
            Some("work"),
            "只报目录不该把 tmux 名清掉"
        );

        // 3. 退出 tmux,bash 发自己的标题 —— 会话名必须清掉,cwd 留着
        ws.pane_mut(id)
            .unwrap()
            .emulator
            .feed(b"\x1b]2;dev@h: /tmp\x07");
        ws.pump(0);
        assert_eq!(
            ws.pane(id).unwrap().tmux,
            None,
            "退出 tmux 之后会话名还挂着"
        );
        assert_eq!(
            ws.pane(id).unwrap().cwd.as_deref(),
            Some(&b"/tmp"[..]),
            "cwd 不该被标题批次清掉"
        );

        // 4. 换了个新 tmux 会话,但这条标题里没有任何路径 token(`parse_title`
        // 给出 cwd: None)—— 这一段专门钉「cwd 只增不清」:st.cwd 是 None,
        // p.cwd 必须原样保留上一个已知值,不能被这一批清掉。
        ws.pane_mut(id)
            .unwrap()
            .emulator
            .feed(b"\x1b]2;plain:0:bash\x07");
        ws.pump(0);
        assert_eq!(
            ws.pane(id).unwrap().cwd.as_deref(),
            Some(&b"/tmp"[..]),
            "st.cwd 是 None 时 p.cwd 必须保留上一个已知值(只增不清)"
        );
        assert_eq!(
            ws.pane(id).unwrap().tmux.as_deref(),
            Some("plain"),
            "标题批次仍要整体重置 tmux"
        );
    }

    /// F124:两次 `BootstrapFlags::default()` 造出来的是**互不相干**的两份状态。
    /// 一个 `Workspace` 上会有多条 `HostConn`(B2-b 换主机),每台机器的 tmux
    /// 服务器各自独立,串在一起的话第一台成功 latch 了 `done`,第二台就永远
    /// 配不上。
    ///
    /// **这条不碰 `HostConn`**:它里面有 `Arc<SshConnection>`,而
    /// `SshConnection::new` 是 `mullion-ssh` 内部的 `pub(crate)`,这边造不出实例。
    /// 所以它只挡得住「`default()` 退化成共享同一个静态实例」这一种坏法;
    /// 「字段真的挂在 `HostConn` 上」由下一条源码级守护兜。
    #[test]
    fn each_host_connection_carries_its_own_bootstrap_state() {
        let a = crate::remote_bootstrap::BootstrapFlags::default();
        let b = crate::remote_bootstrap::BootstrapFlags::default();
        a.finish(true);
        assert!(a.is_done());
        assert!(!b.is_done(), "两条连接的自举状态串了");
    }

    /// **接线守护**:自举状态必须挂在 `HostConn` 上。
    ///
    /// **扎的是源码结构**(`HostConn` 造不出实例,见上一条的说明)。验证边界:
    /// 挡得住「字段整个搬走」,挡不住「两边都留一份、只是没人用这份」。
    ///
    /// 自证会变红:把 `pub tmux_bootstrap: ...` 那行从 `HostConn` 删掉。
    #[test]
    fn bootstrap_state_lives_on_the_host_connection() {
        let src = include_str!("mod.rs");
        let after = src
            .split("\npub struct HostConn {")
            .nth(1)
            .expect("找不到 HostConn 的定义");
        let body = &after[..after.find("\n}\n").expect("找不到 HostConn 的结尾")];
        assert!(
            body.contains("tmux_bootstrap"),
            "HostConn 上没有自举状态 —— 多主机时会共用一份 done"
        );
        assert!(
            body.contains("tmux_last_try"),
            "HostConn 上没有「上次发起时刻」—— 重试判据拿不到 since_last_ms,\
             要么永不重试要么每帧重试"
        );
    }

    /// **接线守护 / F128**:拨号参数必须挂在 `HostConn` 上,不能只有标签级的
    /// `TerminalTab::last_cfg`。
    ///
    /// 换节点(B2-b)之后一个 `Workspace` 上有两台机器,而 `last_cfg` 只记得
    /// **最初**那台。第二台断线时拿 `last_cfg` 去重拨,拨的是另一台机器 ——
    /// 地址、凭据、指纹全对不上,用户看到的却只是「已重新连接」。
    ///
    /// 扎源码结构的理由同上一条(`HostConn` 里有 `Arc<SshConnection>`,
    /// 字段私有、只能由真实握手造出来,测试里造不出实例)。验证边界:挡得住
    /// 「字段被搬回标签级」,挡不住「两边都留一份、重连仍读错那份」——后者由
    /// `app.rs` 里的 `reconnect_dials_the_host_it_lost_not_the_first_one` 兜。
    ///
    /// 自证会变红:把 `pub cfg: Option<..>` 那行从 `HostConn` 删掉。
    #[test]
    fn dial_config_lives_on_the_host_connection() {
        let src = include_str!("mod.rs");
        let after = src
            .split("\npub struct HostConn {")
            .nth(1)
            .expect("找不到 HostConn 的定义");
        let body = &after[..after.find("\n}\n").expect("找不到 HostConn 的结尾")];
        assert!(
            body.contains("pub cfg:"),
            "HostConn 上没有拨号参数 —— 换过节点之后重连会拨回最初那台机器"
        );
    }

    /// F128:`rx` 关了有两个成因,**判反了两边都是灾难**:
    /// - 用户敲 `exit`(连接还活着)却去重连 → 用户永远退不出登录,
    ///   每次 exit 都被自动拉回来。
    /// - 链路死了却当成正常退出 → 就是今天的现状,永远不重连。
    ///
    /// 判据是 SSH **连接**(不是 channel)还在不在。
    ///
    /// 自证会变红:把 `rx_closed_action` 的函数体改成恒返回
    /// `RxClosed::Reconnect`。
    #[test]
    fn a_closed_rx_means_reconnect_only_if_the_transport_died() {
        // 入参是「连接是否还活着」(见 `rx_closed_action` 的 `transport_alive`):
        // 活着(true)= 用户 exit,死了(false)= 链路断,重连。
        assert_eq!(rx_closed_action(false), RxClosed::Reconnect, "链路死了");
        assert_eq!(
            rx_closed_action(true),
            RxClosed::UserExited,
            "连接还活着 = 远端 shell 自己退了,不许重连"
        );
    }

    /// F128:`pump` 必须按成因分别置位。`Reconnecting` 与 `Disconnected`
    /// 的差别是「等一下会自己回来」vs「不会再回来了」,标题条的点、
    /// Ctrl+D 的语义、减屏时先关谁,三处都看它。
    #[tokio::test]
    async fn pump_marks_reconnecting_when_the_transport_died() {
        let (mut ws, probes) = ws_with(1);
        // 造「链路死了」:丢掉发送端(rx 关闭)+ 让判据报 false。
        drop(probes);
        ws.link_alive = |_, _| false;
        ws.pump(0);
        assert_eq!(ws.pane(PaneId(1)).unwrap().status, PaneStatus::Reconnecting);
    }

    /// 反面:同样是 rx 关了,链路还活着就只是「远端 shell 退了」。
    /// 这一条与上一条**必须成对**——只有一条的话,把 `rx_closed_action` 写成
    /// 恒返回某一个值也能过。
    #[tokio::test]
    async fn pump_marks_disconnected_when_the_remote_shell_exited() {
        let (mut ws, probes) = ws_with(1);
        drop(probes);
        ws.link_alive = |_, _| true;
        ws.pump(0);
        assert_eq!(ws.pane(PaneId(1)).unwrap().status, PaneStatus::Disconnected);
    }

    /// F128 竞态:一块**已经在等新 channel**(`Reconnecting`)的 pane,绝不能
    /// 因为手里那条旧 `rx` 关闭而被重新判定成 `Disconnected`。
    ///
    /// 现场:adr-009 下一条 SSH 连接承载多块分屏,链路一死时各 pane 的 `rx`
    /// **不保证同一帧关闭**(各自的 reader task 独立,而且缓冲里积压的字节要先
    /// 排空)。于是会出现:A 先翻 `Reconnecting` → 为这条 host 发起重连 → 拨号
    /// 成功、`hosts[host_ix]` 就地换成新的活连接 → 这之后 B 的旧 `rx` 才关闭。
    /// 此时 `link_alive` 查到的是**新**连接(活的),`rx_closed_action` 返回
    /// `UserExited`,B 被打成 `Disconnected` —— `hosts_to_redial` 从此不再管它,
    /// 那块分屏永久停在"已断开",而同一条连接上的其它分屏都回来了,用户完全
    /// 看不出成因。
    ///
    /// 这条与上面两条**必须成组**:上面两条只覆盖 `Live` 出发的两个方向,
    /// 把「谁能触发这次判定」整个删掉它们照样绿。
    ///
    /// 自证会变红:把 `pump` 里 `if p.status == PaneStatus::Live` 那个门去掉。
    #[tokio::test]
    async fn a_pane_waiting_for_its_new_channel_is_never_downgraded_to_disconnected() {
        let (mut ws, probes) = ws_with(1);
        drop(probes);
        ws.pane_mut(PaneId(1)).unwrap().status = PaneStatus::Reconnecting;
        // 重连已经成功、`hosts[host_ix]` 换成了新的活连接,但这块 pane 还没拿到
        // 自己的新 channel —— 手里的 `rx` 仍是旧的那条(已关)。
        ws.link_alive = |_, _| true;
        ws.pump(0);
        assert_eq!(
            ws.pane(PaneId(1)).unwrap().status,
            PaneStatus::Reconnecting,
            "等着新 channel 的 pane 被打成了 Disconnected —— 它再也不会被重拨"
        );
    }

    /// 同一条门的另一半:`Disconnected` 是终态,`rx` 关闭不该把它拉回
    /// `Reconnecting`。用户敲了 `exit` 之后链路才断(比如他接着关窗口),
    /// 不挡的话那块 pane 会自己"复活"成重连中,用户永远退不出登录 ——
    /// 正是 `rx_closed_action` 那条红线要防的现象,只是换了个入口。
    ///
    /// 自证会变红:同上(去掉那个门之后,`link_alive` 报 false 就会把
    /// `Disconnected` 改成 `Reconnecting`)。
    #[tokio::test]
    async fn a_pane_the_user_exited_is_never_pulled_back_into_reconnecting() {
        let (mut ws, probes) = ws_with(1);
        drop(probes);
        ws.pane_mut(PaneId(1)).unwrap().status = PaneStatus::Disconnected;
        ws.link_alive = |_, _| false;
        ws.pump(0);
        assert_eq!(
            ws.pane(PaneId(1)).unwrap().status,
            PaneStatus::Disconnected,
            "已经断开的 pane 被拉回重连中 —— 用户退不出登录"
        );
    }
}
